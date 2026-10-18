//! The loopback server, on its own threads: the handshake and framing come from daw-bridge; requests other than
//! `ping` and `status` go to the main thread's queue; events and heartbeats come back from it through the [`Hub`].

use std::io::{self, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Sender, SyncSender};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use daw_bridge::auth::Secret;
use daw_bridge::files::{self, Discovery, Reference, DISCOVERY_SCHEMA};
use daw_bridge::handshake::{Offer, ServerAction, ServerConnection};
use daw_bridge::wire::{self, code, Frame, Identity, Project, Request, MAX_AUTHENTICATED, MAX_UNAUTHENTICATED, PROTOCOL};
use daw_bridge::{loopback, Daw};
use serde_json::{json, Value};

use crate::extension::{Outbox, ProjectSummary, Snapshot, BRIDGE_VERSION};
use crate::log;
use crate::queue::{self, Job, Outcome};
use crate::reaper::Busy;

/// The reference file Kumi's lifecycle writes beside the extension.
pub const REFERENCE_FILE: &str = "kumi-bridge-reference.json";
/// How long a request may wait for the main thread when it names no deadline.
const DEFAULT_WAIT: Duration = Duration::from_secs(10);
/// No tick for this long: the main thread is held (a modal dialog, a long load or render).
const STALL: Duration = Duration::from_millis(1500);
/// Frames queued for one connection before it counts as stuck and is closed.
const OUTBOX_FRAMES: usize = 4096;
const PREAUTH_POLL: Duration = Duration::from_millis(200);

/// Methods served off the main thread: they answer even while REAPER shows a dialog.
const SOCKET_METHODS: [&str; 2] = ["ping", "status"];
const MAIN_METHODS: [&str; 2] = ["action", "quit"];
const EVENTS: [&str; 4] = [wire::event::PROJECT, wire::event::TRANSPORT, wire::event::TRACKS, wire::event::SELECTION];

/// Where the reference file lives for a resource folder.
pub fn reference_path(resource: &Path) -> PathBuf {
    resource.join("UserPlugins").join(REFERENCE_FILE)
}

/// The connections, shared by the socket threads and the main thread.
#[derive(Clone)]
pub struct Hub {
    inner: Arc<HubInner>,
}

struct HubInner {
    conns: Mutex<Vec<Arc<Conn>>>,
    started: Instant,
}

struct Conn {
    id: u64,
    state: Mutex<ServerConnection>,
    out: SyncSender<Vec<u8>>,
    stream: TcpStream,
    authenticated: AtomicBool,
}

impl Default for Hub {
    fn default() -> Self {
        Hub { inner: Arc::new(HubInner { conns: Mutex::new(Vec::new()), started: Instant::now() }) }
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

impl Hub {
    fn conns(&self) -> Vec<Arc<Conn>> {
        lock(&self.inner.conns).clone()
    }

    fn add(&self, conn: Arc<Conn>) {
        lock(&self.inner.conns).push(conn);
    }

    fn remove(&self, id: u64) {
        lock(&self.inner.conns).retain(|c| c.id != id);
    }

    fn counts(&self) -> (usize, usize) {
        let conns = lock(&self.inner.conns);
        let authenticated = conns.iter().filter(|c| c.authenticated.load(Ordering::Acquire)).count();
        (conns.len() - authenticated, authenticated)
    }

    fn send(&self, conn: &Conn, bytes: Vec<u8>) {
        if conn.out.try_send(bytes).is_err() {
            log::line(&format!("connection {} isn't reading; closing it", conn.id));
            self.remove(conn.id);
            let _ = conn.stream.shutdown(Shutdown::Both);
        }
    }

    pub fn connections(&self) -> usize {
        lock(&self.inner.conns).len()
    }
}

impl Outbox for Hub {
    fn event(&self, name: &str, data: Value) {
        for conn in self.conns() {
            let bytes = lock(&conn.state).event(name, data.clone());
            if let Some(bytes) = bytes {
                self.send(&conn, bytes);
            }
        }
    }

    fn heartbeat(&self) {
        let ms = self.inner.started.elapsed().as_millis() as u64;
        for conn in self.conns().iter().filter(|c| c.authenticated.load(Ordering::Acquire)) {
            self.send(conn, Frame::Heartbeat { ms }.encode());
        }
    }
}

/// What the server needs from the reference file.
#[derive(Debug, Clone)]
pub struct Config {
    pub secret_file: PathBuf,
    pub discovery_dir: PathBuf,
    pub port: Option<u16>,
    pub log_dir: Option<PathBuf>,
    /// Makes each accepted connection's handshake: random nonces, or the conformance vectors' fixed ones.
    pub connection: fn(Secret, u64) -> ServerConnection,
}

impl Config {
    pub fn from_reference(reference: &Reference) -> Config {
        Config {
            secret_file: reference.secret_file.clone().into(),
            discovery_dir: reference.discovery_dir.clone().into(),
            port: reference.port,
            log_dir: reference.log_dir.clone().map(PathBuf::from),
            connection: ServerConnection::new,
        }
    }

    /// Binds 127.0.0.1 on the first free port of REAPER's range, from the reference's port.
    pub fn bind(&self) -> io::Result<TcpListener> {
        loopback::bind(Daw::Reaper, self.port)
    }
}

pub struct Server {
    config: Config,
    port: u16,
    hub: Hub,
    stopping: Arc<AtomicBool>,
    accept: Option<JoinHandle<()>>,
    threads: Arc<Mutex<Vec<JoinHandle<()>>>>,
    started_ms: u64,
    discovery: Option<(PathBuf, Option<ProjectSummary>)>,
}

impl Server {
    pub fn start(listener: TcpListener, config: Config, hub: Hub, jobs: Sender<Job>, snapshot: Arc<Mutex<Snapshot>>) -> io::Result<Server> {
        let port = listener.local_addr()?.port();
        let stopping = Arc::new(AtomicBool::new(false));
        let threads = Arc::new(Mutex::new(Vec::new()));
        let shared = Shared { config: config.clone(), hub: hub.clone(), jobs: Mutex::new(jobs), snapshot, stopping: stopping.clone() };
        let shared = Arc::new(shared);
        let accept = {
            let threads = threads.clone();
            thread::Builder::new()
                .name("kumi-accept".into())
                .spawn(move || crate::guard("accept", (), || accept_loop(listener, shared, threads)))?
        };
        Ok(Server { config, port, hub, stopping, accept: Some(accept), threads, started_ms: unix_ms(), discovery: None })
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    /// Writes the discovery file when it's missing or the project changed. Main thread, each tick.
    pub fn publish(&mut self, snapshot: &Snapshot) {
        if snapshot.daw_version.is_empty() {
            return;
        }
        if self.discovery.as_ref().is_some_and(|(_, project)| *project == snapshot.project) {
            return;
        }
        let discovery = Discovery {
            schema: DISCOVERY_SCHEMA.into(),
            daw: Daw::Reaper.id().into(),
            pid: std::process::id(),
            host: loopback::BIND_ADDRESS.to_string(),
            port: self.port,
            protocol: PROTOCOL.into(),
            version: BRIDGE_VERSION.into(),
            daw_version: snapshot.daw_version.clone(),
            resource: snapshot.resource.clone(),
            project: snapshot.project.as_ref().map(project),
            started_ms: self.started_ms,
        };
        match files::write_discovery(&self.config.discovery_dir, &discovery) {
            Ok(path) => self.discovery = Some((path, snapshot.project.clone())),
            Err(error) => log::line(&format!("couldn't write the discovery file: {error}")),
        }
    }

    /// Says bye (`quit`, `reload` or `shutdown`), closes every connection, removes the discovery file and waits for
    /// the threads.
    pub fn stop(mut self, reason: &str) {
        self.stopping.store(true, Ordering::Release);
        // Wakes the blocking accept.
        let _ = TcpStream::connect_timeout(&(loopback::BIND_ADDRESS, self.port).into(), Duration::from_millis(200));
        if let Some(accept) = self.accept.take() {
            let _ = accept.join();
        }
        let conns = std::mem::take(&mut *lock(&self.hub.inner.conns));
        for conn in &conns {
            if conn.authenticated.load(Ordering::Acquire) {
                let _ = conn.out.try_send(Frame::Bye { reason: reason.into() }.encode());
            }
            let _ = conn.stream.shutdown(Shutdown::Read);
        }
        drop(conns);
        for handle in std::mem::take(&mut *lock(&self.threads)) {
            let _ = handle.join();
        }
        if let Some((path, _)) = self.discovery.take() {
            let _ = std::fs::remove_file(path);
        }
    }
}

fn project(summary: &ProjectSummary) -> Project {
    Project {
        name: (!summary.name.is_empty()).then(|| summary.name.clone()),
        path: (!summary.path.is_empty()).then(|| summary.path.clone()),
        ..Project::default()
    }
}

struct Shared {
    config: Config,
    hub: Hub,
    jobs: Mutex<Sender<Job>>,
    snapshot: Arc<Mutex<Snapshot>>,
    stopping: Arc<AtomicBool>,
}

impl Shared {
    fn identity(&self) -> (Identity, Offer) {
        let snapshot = lock(&self.snapshot).clone();
        let identity = Identity {
            daw: Daw::Reaper.id().into(),
            bridge: BRIDGE_VERSION.into(),
            daw_version: snapshot.daw_version,
            pid: std::process::id(),
            resource: snapshot.resource,
            project: snapshot.project.as_ref().map(project),
        };
        let methods = SOCKET_METHODS.iter().chain(MAIN_METHODS.iter()).map(|m| m.to_string()).collect();
        let events = EVENTS.iter().map(|e| e.to_string()).collect();
        (identity, Offer { methods, events, ..Offer::default() })
    }

    /// Why the main thread can't take work now, if it can't.
    fn busy(&self) -> Option<Busy> {
        let snapshot = lock(&self.snapshot);
        match snapshot.last_tick {
            Some(tick) if tick.elapsed() > STALL => Some(snapshot.busy.unwrap_or(Busy::Modal)),
            None => Some(Busy::Starting),
            _ => snapshot.busy,
        }
    }

    fn status(&self) -> Value {
        let (identity, _) = self.identity();
        let snapshot = lock(&self.snapshot).clone();
        let mut status = serde_json::to_value(identity).unwrap_or_else(|_| json!({}));
        let busy = self.busy();
        status["busy"] = busy.map_or(Value::Bool(false), |b| b.code().into());
        if let Some(busy) = busy {
            status["busyReason"] = busy.reason().into();
        }
        status["lastTickMs"] = snapshot.last_tick.map(|t| t.elapsed().as_millis() as u64).into();
        status["connections"] = self.hub.connections().into();
        if let Value::Object(extra) = snapshot.status {
            for (key, value) in extra {
                status[key] = value;
            }
        }
        status
    }

    fn answer(&self, request: Request) -> Frame {
        let id = request.id;
        match request.method.as_str() {
            "ping" => return Frame::ok(id, json!({})),
            "status" => return Frame::ok(id, self.status()),
            _ => {}
        }
        let now = unix_ms();
        let wait = match request.deadline_ms {
            Some(deadline) if deadline <= now => {
                return Frame::error(id, code::DEADLINE, "the deadline passed before REAPER got to it; nothing changed")
            }
            Some(deadline) => Duration::from_millis(deadline - now),
            None => DEFAULT_WAIT,
        };
        let (job, waiter) = queue::job(&request.method, request.params);
        if lock(&self.jobs).send(job).is_err() {
            return Frame::error(id, code::FAILED, "REAPER is closing");
        }
        match waiter.wait(wait) {
            Outcome::Answered(Ok(result)) => Frame::ok(id, result),
            Outcome::Answered(Err((code, message))) => Frame::error(id, &code, message),
            Outcome::Cancelled if request.deadline_ms.is_some() => {
                Frame::error(id, code::DEADLINE, "the deadline passed before REAPER got to it; nothing changed")
            }
            Outcome::Cancelled => {
                let why = self.busy().map_or("REAPER didn't get to it in time", Busy::reason);
                Frame::error(id, code::BUSY, format!("{why}; nothing changed"))
            }
            Outcome::Uncertain => {
                Frame::error(id, code::UNCERTAIN, "REAPER started this but didn't finish in time; it may or may not have happened")
            }
            Outcome::Closed => Frame::error(id, code::FAILED, "REAPER is closing"),
        }
    }
}

fn accept_loop(listener: TcpListener, shared: Arc<Shared>, threads: Arc<Mutex<Vec<JoinHandle<()>>>>) {
    static NEXT_ID: AtomicU64 = AtomicU64::new(1);
    let started = Instant::now();
    for stream in listener.incoming() {
        if shared.stopping.load(Ordering::Acquire) {
            break;
        }
        let Ok(stream) = stream else { continue };
        let Ok(peer) = stream.peer_addr() else { continue };
        if !loopback::peer_allowed(&peer) {
            continue;
        }
        let (waiting, authenticated) = shared.hub.counts();
        if waiting >= MAX_UNAUTHENTICATED || authenticated >= MAX_AUTHENTICATED {
            log::line("a connection was turned away: too many open");
            continue;
        }
        let secret = match files::read_secret(&shared.config.secret_file) {
            Ok(secret) => secret,
            Err(error) => {
                log::line(&format!("a connection was closed: the secret can't be read ({error})"));
                continue;
            }
        };
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let now_ms = started.elapsed().as_millis() as u64;
        match spawn_connection(id, stream, (shared.config.connection)(secret, now_ms), started, &shared) {
            Ok(handles) => {
                let mut threads = lock(&threads);
                threads.retain(|h| !h.is_finished());
                threads.extend(handles);
            }
            Err(error) => log::line(&format!("couldn't start a connection: {error}")),
        }
    }
}

fn spawn_connection(
    id: u64,
    stream: TcpStream,
    state: ServerConnection,
    started: Instant,
    shared: &Arc<Shared>,
) -> io::Result<[JoinHandle<()>; 2]> {
    stream.set_nodelay(true)?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;
    let (out, rx) = mpsc::sync_channel::<Vec<u8>>(OUTBOX_FRAMES);
    let conn = Arc::new(Conn { id, state: Mutex::new(state), out, stream: stream.try_clone()?, authenticated: AtomicBool::new(false) });
    let mut writer = stream.try_clone()?;
    let write = thread::Builder::new().name("kumi-write".into()).spawn(move || {
        for bytes in rx {
            if writer.write_all(&bytes).is_err() {
                break;
            }
        }
        let _ = writer.shutdown(Shutdown::Both);
    })?;
    shared.hub.add(conn.clone());
    let reader_shared = shared.clone();
    let read = thread::Builder::new().name("kumi-read".into()).spawn(move || {
        let shared = reader_shared;
        crate::guard("connection", (), || serve(&conn, stream, started, &shared));
        shared.hub.remove(conn.id);
    })?;
    Ok([read, write])
}

fn serve(conn: &Arc<Conn>, mut stream: TcpStream, started: Instant, shared: &Shared) {
    let mut buf = vec![0u8; 64 * 1024];
    let _ = stream.set_read_timeout(Some(PREAUTH_POLL));
    loop {
        let actions = match stream.read(&mut buf) {
            Ok(0) => return,
            Ok(n) => lock(&conn.state).on_bytes(&buf[..n], &mut || shared.identity()),
            Err(e) if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut) => {
                lock(&conn.state).on_tick(started.elapsed().as_millis() as u64)
            }
            Err(_) => return,
        };
        for action in actions {
            match action {
                ServerAction::Send(bytes) => {
                    if conn.out.send(bytes).is_err() {
                        return;
                    }
                }
                ServerAction::Close(reason) => {
                    log::line(&format!("connection {} closed: {reason:?}", conn.id));
                    return;
                }
                ServerAction::Authenticated => {
                    conn.authenticated.store(true, Ordering::Release);
                    let _ = stream.set_read_timeout(None);
                }
                ServerAction::Request(request) => {
                    let response = shared.answer(request);
                    if conn.out.send(response.encode()).is_err() {
                        return;
                    }
                }
            }
        }
    }
}

fn unix_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use daw_bridge::handshake::{ClientHandshake, ClientStep};
    use daw_bridge::owner;
    use std::io::{BufRead, BufReader};

    const VECTOR_SECRET: &str = "kumi-test-secret-0123456789abcdefghijklmnopqrstuvwxyz";

    fn vectors() -> Value {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../daw-bridge/tests/vectors/server.json");
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    }

    fn vector_connection(secret: Secret, now_ms: u64) -> ServerConnection {
        let v = vectors();
        ServerConnection::with_nonces(secret, v["serverNonce"].as_str().unwrap().into(), v["session"].as_str().unwrap().into(), now_ms)
    }

    struct Lab {
        dir: tempfile::TempDir,
        server: Option<Server>,
        hub: Hub,
        snapshot: Arc<Mutex<Snapshot>>,
        jobs: mpsc::Receiver<Job>,
    }

    impl Lab {
        fn start(secret: &str, connection: fn(Secret, u64) -> ServerConnection) -> Lab {
            let dir = tempfile::tempdir().unwrap();
            let bridge = dir.path().join("bridges").join("reaper");
            owner::create_owner_only_dir(&bridge).unwrap();
            owner::write_owner_only(&bridge.join("bridge.secret"), format!("{secret}\n").as_bytes()).unwrap();
            let config = Config {
                secret_file: bridge.join("bridge.secret"),
                discovery_dir: bridge.join("discovery"),
                port: None,
                log_dir: None,
                connection,
            };
            let snapshot = Arc::new(Mutex::new(Snapshot {
                daw_version: "7.82/macOS-arm64".into(),
                resource: dir.path().join("REAPER").to_string_lossy().into_owned(),
                project: Some(ProjectSummary { name: "Song.rpp".into(), path: "/m/Song.rpp".into() }),
                last_tick: Some(Instant::now()),
                busy: None,
                status: json!({"tracks": 3}),
            }));
            let hub = Hub::default();
            let (tx, jobs) = queue::channel();
            let listener = TcpListener::bind((loopback::BIND_ADDRESS, 0)).unwrap();
            let server = Server::start(listener, config, hub.clone(), tx, snapshot.clone()).unwrap();
            Lab { dir, server: Some(server), hub, snapshot, jobs }
        }

        fn server(&mut self) -> &mut Server {
            self.server.as_mut().unwrap()
        }

        fn connect(&mut self) -> (TcpStream, BufReader<TcpStream>) {
            let stream = TcpStream::connect((loopback::BIND_ADDRESS, self.server().port())).unwrap();
            stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            let reader = BufReader::new(stream.try_clone().unwrap());
            (stream, reader)
        }

        fn authenticated(&mut self) -> (TcpStream, BufReader<TcpStream>, Value) {
            let (mut stream, mut reader) = self.connect();
            let (mut handshake, hello) = ClientHandshake::start(Secret::new(VECTOR_SECRET).unwrap(), "test");
            stream.write_all(&hello).unwrap();
            loop {
                match handshake.on_line(&read_line(&mut reader).unwrap()).unwrap() {
                    ClientStep::Send(bytes) => stream.write_all(&bytes).unwrap(),
                    ClientStep::Welcome(welcome) => return (stream, reader, serde_json::to_value(*welcome).unwrap()),
                }
            }
        }
    }

    impl Drop for Lab {
        fn drop(&mut self) {
            if let Some(server) = self.server.take() {
                server.stop("test over");
            }
        }
    }

    fn read_line(reader: &mut BufReader<TcpStream>) -> Option<String> {
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) | Err(_) => None,
            Ok(_) => Some(line.trim_end().to_string()),
        }
    }

    fn read_frame(reader: &mut BufReader<TcpStream>) -> Value {
        loop {
            let frame: Value = serde_json::from_str(&read_line(reader).expect("a frame")).unwrap();
            if frame["type"] != "heartbeat" {
                return frame;
            }
        }
    }

    fn request(stream: &mut TcpStream, reader: &mut BufReader<TcpStream>, id: u64, method: &str) -> Value {
        stream
            .write_all(Frame::Request(Request { id, method: method.into(), params: json!({}), deadline_ms: None }).encode().as_slice())
            .unwrap();
        let response = read_frame(reader);
        assert_eq!(response["id"], id);
        response
    }

    // Replays daw-bridge's server vectors against the real sockets: what a host sees from this extension.
    #[test]
    fn the_wire_matches_daw_bridges_server_vectors() {
        let v = vectors();
        assert_eq!(v["secret"], VECTOR_SECRET);
        let mut lab = Lab::start(VECTOR_SECRET, vector_connection);
        for case in v["cases"].as_array().unwrap() {
            let name = case["name"].as_str().unwrap();
            let (mut stream, mut reader) = lab.connect();
            let accepted = Instant::now();
            let mut closed = false;
            for step in case["steps"].as_array().unwrap() {
                if let Some(hex) = step["inHex"].as_str() {
                    // A failed write is fine once the bridge closed; the expectations say what it sent before.
                    let _ = stream.write_all(&hex::decode(hex).unwrap());
                    thread::sleep(Duration::from_millis(20));
                } else {
                    let tick = Duration::from_millis(step["tickMs"].as_u64().unwrap());
                    // Waiting a minute isn't worth it: past the handshake's 3 s, being still open proves the point.
                    let wait = tick.min(Duration::from_millis(3600));
                    thread::sleep(wait.saturating_sub(accepted.elapsed()));
                }
                for expect in step["expect"].as_array().unwrap() {
                    if let Some(want) = expect.get("send") {
                        let got: Value =
                            serde_json::from_str(&read_line(&mut reader).unwrap_or_else(|| panic!("{name}: no frame"))).unwrap();
                        if want["type"] == "welcome" {
                            for key in ["type", "session", "maxFrameBytes", "heartbeatMs"] {
                                assert_eq!(got[key], want[key], "{name}: {key}");
                            }
                            assert_eq!(got["daw"], "reaper");
                            assert!(got["methods"].as_array().unwrap().iter().any(|m| m == "quit"), "{name}");
                        } else {
                            assert_eq!(&got, want, "{name}");
                        }
                    } else if let Some(want) = expect.get("request") {
                        // The extension answers each request in order.
                        let got = read_frame(&mut reader);
                        assert_eq!((got["type"].as_str(), &got["id"], &got["ok"]), (Some("response"), &want["id"], &json!(true)), "{name}");
                    } else if expect.get("close").is_some() {
                        assert_eq!(read_line(&mut reader), None, "{name}: expected the bridge to close");
                        closed = true;
                    }
                }
            }
            if !closed {
                // Still open: a ping answers (after auth) or nothing more arrives (before it).
                if case["steps"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|s| s["expect"].as_array().unwrap().iter().any(|e| e.get("authenticated").is_some()))
                {
                    assert_eq!(request(&mut stream, &mut reader, 99, "ping")["ok"], true, "{name}");
                }
            }
        }
    }

    #[test]
    fn requests_reach_the_main_thread_and_events_follow_in_order() {
        let mut lab = Lab::start(VECTOR_SECRET, ServerConnection::new);
        let (mut stream, mut reader, welcome) = lab.authenticated();
        assert_eq!(welcome["daw"], "reaper");
        assert_eq!(welcome["project"], json!({"name": "Song.rpp", "path": "/m/Song.rpp"}));
        assert_eq!(welcome["events"], json!(["project", "transport", "tracks", "selection"]));

        let jobs = std::mem::replace(&mut lab.jobs, mpsc::channel().1);
        let main = thread::spawn(move || {
            let job = jobs.recv_timeout(Duration::from_secs(5)).unwrap();
            assert!(job.claim());
            assert_eq!(job.method, "quit");
            job.answer(Ok(json!({"quitting": true})));
        });
        assert_eq!(request(&mut stream, &mut reader, 1, "quit")["result"], json!({"quitting": true}));
        main.join().unwrap();

        lab.hub.event("tracks", json!({"count": 4}));
        lab.hub.event("selection", json!({"tracks": []}));
        let first = read_frame(&mut reader);
        let second = read_frame(&mut reader);
        assert_eq!((first["type"].as_str(), first["seq"].as_u64(), first["name"].as_str()), (Some("event"), Some(1), Some("tracks")));
        assert_eq!(first["data"], json!({"count": 4}));
        assert_eq!((second["seq"].as_u64(), second["name"].as_str()), (Some(2), Some("selection")));

        lab.hub.heartbeat();
        let beat: Value = serde_json::from_str(&read_line(&mut reader).unwrap()).unwrap();
        assert_eq!(beat["type"], "heartbeat");
    }

    #[test]
    fn status_answers_off_the_main_thread_and_says_when_it_is_held() {
        let mut lab = Lab::start(VECTOR_SECRET, ServerConnection::new);
        let (mut stream, mut reader, _) = lab.authenticated();
        let status = request(&mut stream, &mut reader, 1, "status")["result"].clone();
        assert_eq!(status["busy"], false);
        assert_eq!(status["tracks"], 3);
        assert_eq!(status["dawVersion"], "7.82/macOS-arm64");
        assert_eq!(status["connections"], 1);

        // No tick for a while: a dialog holds the main thread. Queued work is refused as busy, nothing changed.
        lab.snapshot.lock().unwrap().last_tick = Some(Instant::now() - Duration::from_secs(5));
        let status = request(&mut stream, &mut reader, 2, "status")["result"].clone();
        assert_eq!(status["busy"], "modal");
        assert!(status["busyReason"].as_str().unwrap().contains("dialog"));
        let deadline = unix_ms() + 50;
        stream
            .write_all(&Frame::Request(Request { id: 3, method: "quit".into(), params: json!({}), deadline_ms: Some(deadline) }).encode())
            .unwrap();
        let response = read_frame(&mut reader);
        assert_eq!(response["error"]["code"], code::DEADLINE);
    }

    #[test]
    fn the_discovery_file_follows_the_project_and_goes_with_the_server() {
        let mut lab = Lab::start(VECTOR_SECRET, ServerConnection::new);
        let snapshot = lab.snapshot.lock().unwrap().clone();
        lab.server().publish(&snapshot);
        let dir = lab.dir.path().join("bridges/reaper/discovery");
        let found = files::scan_discovery(&dir, Daw::Reaper);
        assert_eq!(found.len(), 1);
        let discovery = found[0].discovery.as_ref().unwrap();
        assert_eq!(discovery.port, lab.server().port());
        assert_eq!(discovery.project.as_ref().unwrap().name.as_deref(), Some("Song.rpp"));

        let mut moved = snapshot.clone();
        moved.project = Some(ProjectSummary::default());
        lab.server().publish(&moved);
        let found = files::scan_discovery(&dir, Daw::Reaper);
        assert_eq!(found[0].discovery.as_ref().unwrap().project, Some(Project::default()));

        let (_stream, mut reader, _) = lab.authenticated();
        lab.server.take().unwrap().stop("quit");
        assert_eq!(read_frame(&mut reader), json!({"type": "bye", "reason": "quit"}));
        assert_eq!(read_line(&mut reader), None);
        assert!(files::scan_discovery(&dir, Daw::Reaper).is_empty());
    }

    #[test]
    fn a_wrong_secret_never_gets_a_welcome() {
        let mut lab = Lab::start(&"o".repeat(40), ServerConnection::new);
        let (mut stream, mut reader) = lab.connect();
        let (mut handshake, hello) = ClientHandshake::start(Secret::new(VECTOR_SECRET).unwrap(), "test");
        stream.write_all(&hello).unwrap();
        let challenge = read_line(&mut reader).unwrap();
        assert!(handshake.on_line(&challenge).is_err(), "the host refuses a bridge that can't prove the secret");
    }
}
