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

use daw_bridge::files::{self, Discovery, Reference, DISCOVERY_SCHEMA};
use daw_bridge::handshake::{Offer, ServerAction, ServerConnection};
use daw_bridge::wire::{self, code, Frame, Identity, Project, Request, MAX_AUTHENTICATED, MAX_UNAUTHENTICATED, PROTOCOL};
use daw_bridge::{loopback, Daw};
use serde_json::{json, Value};

use crate::extension::{Outbox, ProjectSummary, Snapshot, BRIDGE_VERSION};
use crate::log;
use crate::queue::{self, Job, Outcome};

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
const MAIN_METHODS: [&str; 1] = ["quit"];
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
}

impl Config {
    pub fn from_reference(reference: &Reference) -> Config {
        Config {
            secret_file: reference.secret_file.clone().into(),
            discovery_dir: reference.discovery_dir.clone().into(),
            port: reference.port,
            log_dir: reference.log_dir.clone().map(PathBuf::from),
        }
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
    pub fn start(config: Config, hub: Hub, jobs: Sender<Job>, snapshot: Arc<Mutex<Snapshot>>) -> io::Result<Server> {
        let listener = loopback::bind(Daw::Reaper, config.port)?;
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

    /// Says bye, closes every connection, removes the discovery file and waits for the threads.
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
    fn busy(&self) -> Option<String> {
        let snapshot = lock(&self.snapshot);
        match snapshot.last_tick {
            Some(tick) if tick.elapsed() > STALL => {
                Some("REAPER isn't answering: a dialog may be open, or it is loading or rendering".into())
            }
            None => Some("REAPER is starting".into()),
            _ => snapshot.busy.clone(),
        }
    }

    fn status(&self) -> Value {
        let (identity, _) = self.identity();
        let snapshot = lock(&self.snapshot).clone();
        let mut status = serde_json::to_value(identity).unwrap_or_else(|_| json!({}));
        let busy = self.busy();
        status["busy"] = busy.is_some().into();
        if let Some(why) = busy {
            status["busyReason"] = why.into();
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
                let why = self.busy().unwrap_or_else(|| "REAPER didn't get to it in time".into());
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
        match spawn_connection(id, stream, ServerConnection::new(secret, now_ms), started, &shared) {
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
