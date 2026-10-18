//! The host's connection to a bridge: connect on loopback, prove the secret both ways, then requests, events and
//! heartbeats.
use daw_bridge::auth::Secret;
use daw_bridge::frame::{Framer, MAX_FRAME_BYTES};
use daw_bridge::handshake::{ClientHandshake, ClientStep, HandshakeError};
use daw_bridge::loopback::peer_allowed;
use daw_bridge::wire::{Event, Frame, Request, Welcome, CLIENT_HANDSHAKE_TIMEOUT_MS, HEARTBEATS_BEFORE_BUSY, MAX_SAFE_INTEGER};
use serde_json::Value;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::TcpStream;
use tokio::sync::{broadcast, oneshot};

/// Lines the bridge sends before its welcome: larger than the bridge's own 1024-byte limit, since a welcome carries
/// paths and a project name.
const HANDSHAKE_LINE_BYTES: usize = 64 * 1024;
const READ_CHUNK: usize = 64 * 1024;

/// Why a connection could not be made.
#[derive(Debug)]
pub enum ConnectError {
    NotLoopback(SocketAddr),
    /// Nothing listens there, or the connection broke.
    Io(std::io::Error),
    Timeout,
    Handshake(HandshakeError),
    /// The bridge closed the connection before its welcome.
    Closed,
}

impl std::fmt::Display for ConnectError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConnectError::NotLoopback(address) => write!(f, "{address} is not a loopback address"),
            ConnectError::Io(error) => error.fmt(f),
            ConnectError::Timeout => f.write_str("the bridge didn't finish the handshake in time"),
            ConnectError::Handshake(error) => error.fmt(f),
            ConnectError::Closed => f.write_str("the bridge closed the connection during the handshake"),
        }
    }
}

impl std::error::Error for ConnectError {}

/// Why a request has no result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RequestError {
    /// The bridge answered with an error (`code` from [`daw_bridge::wire::code`]).
    Bridge { code: String, message: String },
    /// No answer in time: the request may or may not have run.
    Timeout,
    /// The connection is gone.
    Closed,
}

impl std::fmt::Display for RequestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RequestError::Bridge { code, message } => write!(f, "{message} ({code})"),
            RequestError::Timeout => f.write_str("the bridge didn't answer in time"),
            RequestError::Closed => f.write_str("the bridge closed the connection"),
        }
    }
}

impl std::error::Error for RequestError {}

type Pending = HashMap<u64, oneshot::Sender<Result<Value, RequestError>>>;

struct Inner {
    writer: tokio::sync::Mutex<OwnedWriteHalf>,
    pending: Mutex<Pending>,
    next_id: AtomicU64,
    last_frame: Mutex<Instant>,
    closed: AtomicBool,
    events: broadcast::Sender<Event>,
    welcome: Welcome,
}

/// One authenticated connection. Cloning shares it; dropping the last clone doesn't close it (call [`close`]).
///
/// [`close`]: BridgeClient::close
#[derive(Clone)]
pub struct BridgeClient {
    inner: Arc<Inner>,
}

impl BridgeClient {
    /// Connects and authenticates; `client` names this host in the hello (e.g. `daw-mcp-server/0.1.0`).
    pub async fn connect(address: SocketAddr, secret: Secret, client: &str) -> Result<Self, ConnectError> {
        Self::connect_within(address, secret, client, Duration::from_millis(CLIENT_HANDSHAKE_TIMEOUT_MS)).await
    }

    /// `timeout` covers the connect and the handshake together. A connect nobody accepts in time is an `Io` error of kind
    /// `TimedOut` (Windows keeps retrying the SYN to a closed loopback port for about 2 s before it refuses), a handshake
    /// that doesn't finish is `Timeout`.
    pub async fn connect_within(address: SocketAddr, secret: Secret, client: &str, timeout: Duration) -> Result<Self, ConnectError> {
        if !peer_allowed(&address) {
            return Err(ConnectError::NotLoopback(address));
        }
        let deadline = tokio::time::Instant::now() + timeout;
        let stream = match tokio::time::timeout_at(deadline, TcpStream::connect(address)).await {
            Ok(stream) => stream.map_err(ConnectError::Io)?,
            Err(_) => {
                return Err(ConnectError::Io(std::io::Error::new(std::io::ErrorKind::TimedOut, "nothing accepted the connection in time")))
            }
        };
        match tokio::time::timeout_at(deadline, Self::handshake(stream, secret, client)).await {
            Ok(result) => result,
            Err(_) => Err(ConnectError::Timeout),
        }
    }

    async fn handshake(stream: TcpStream, secret: Secret, client: &str) -> Result<Self, ConnectError> {
        stream.set_nodelay(true).map_err(ConnectError::Io)?;
        let (mut reader, mut writer) = stream.into_split();
        let (mut handshake, hello) = ClientHandshake::start(secret, client);
        writer.write_all(&hello).await.map_err(ConnectError::Io)?;
        let mut framer = Framer::new(HANDSHAKE_LINE_BYTES);
        let mut chunk = vec![0u8; READ_CHUNK];
        let welcome = 'handshake: loop {
            while let Some(line) = framer.next_line() {
                let line = line.map_err(|error| ConnectError::Handshake(HandshakeError::Unexpected(error.code().into())))?;
                match handshake.on_line(&line).map_err(ConnectError::Handshake)? {
                    ClientStep::Send(bytes) => writer.write_all(&bytes).await.map_err(ConnectError::Io)?,
                    ClientStep::Welcome(welcome) => break 'handshake welcome,
                }
            }
            let read = reader.read(&mut chunk).await.map_err(ConnectError::Io)?;
            if read == 0 {
                return Err(ConnectError::Closed);
            }
            framer.feed(&chunk[..read]);
        };
        framer.set_max_bytes(MAX_FRAME_BYTES);
        let (events, _) = broadcast::channel(1024);
        let inner = Arc::new(Inner {
            writer: tokio::sync::Mutex::new(writer),
            pending: Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(1),
            last_frame: Mutex::new(Instant::now()),
            closed: AtomicBool::new(false),
            events,
            welcome: *welcome,
        });
        tokio::spawn(read_loop(Arc::downgrade(&inner), reader, framer));
        Ok(Self { inner })
    }

    pub fn welcome(&self) -> &Welcome {
        &self.inner.welcome
    }

    pub fn is_closed(&self) -> bool {
        self.inner.closed.load(Ordering::SeqCst)
    }

    /// Whether the bridge has been silent for three heartbeats: the DAW is in a dialog, rendering or stuck. A closed
    /// connection isn't busy, it's gone.
    pub fn busy(&self) -> bool {
        let silent = self.inner.last_frame.lock().unwrap().elapsed();
        !self.is_closed() && silent >= Duration::from_millis(self.inner.welcome.heartbeat_ms.max(1) * HEARTBEATS_BEFORE_BUSY)
    }

    /// The bridge's events from now on.
    pub fn events(&self) -> broadcast::Receiver<Event> {
        self.inner.events.subscribe()
    }

    pub async fn request(&self, method: &str, params: Value, timeout: Duration) -> Result<Value, RequestError> {
        if self.is_closed() {
            return Err(RequestError::Closed);
        }
        let id = self.inner.next_id.fetch_add(1, Ordering::SeqCst);
        if id > MAX_SAFE_INTEGER {
            return Err(RequestError::Closed);
        }
        let (answer, answered) = oneshot::channel();
        self.inner.pending.lock().unwrap().insert(id, answer);
        let frame = Frame::Request(Request { id, method: method.into(), params, deadline_ms: None }).encode();
        if self.inner.writer.lock().await.write_all(&frame).await.is_err() {
            self.inner.pending.lock().unwrap().remove(&id);
            fail_all(&self.inner);
            return Err(RequestError::Closed);
        }
        match tokio::time::timeout(timeout, answered).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err(RequestError::Closed),
            Err(_) => {
                self.inner.pending.lock().unwrap().remove(&id);
                Err(RequestError::Timeout)
            }
        }
    }

    pub async fn close(&self) {
        let _ = self.inner.writer.lock().await.shutdown().await;
        fail_all(&self.inner);
    }
}

fn fail_all(inner: &Inner) {
    inner.closed.store(true, Ordering::SeqCst);
    for (_, answer) in inner.pending.lock().unwrap().drain() {
        let _ = answer.send(Err(RequestError::Closed));
    }
}

async fn read_loop(inner: std::sync::Weak<Inner>, mut reader: OwnedReadHalf, mut framer: Framer) {
    let mut chunk = vec![0u8; READ_CHUNK];
    loop {
        let Some(live) = inner.upgrade() else { return };
        // Lines already fed (the welcome's read may have carried more).
        while let Some(line) = framer.next_line() {
            let Ok(line) = line else {
                fail_all(&live);
                return;
            };
            *live.last_frame.lock().unwrap() = Instant::now();
            match Frame::parse(&line) {
                Ok(Frame::Response(response)) => {
                    let answer = live.pending.lock().unwrap().remove(&response.id);
                    if let Some(answer) = answer {
                        let result = if response.ok {
                            Ok(response.result.unwrap_or(Value::Null))
                        } else {
                            let error = response.error.unwrap_or_else(|| unreachable!("checked when parsed"));
                            Err(RequestError::Bridge { code: error.code, message: error.message })
                        };
                        let _ = answer.send(result);
                    }
                }
                Ok(Frame::Event(event)) => {
                    let _ = live.events.send(event);
                }
                Ok(Frame::Bye { .. }) => {
                    fail_all(&live);
                    return;
                }
                Ok(_) => {}
                Err(_) => {
                    fail_all(&live);
                    return;
                }
            }
        }
        drop(live);
        let read = reader.read(&mut chunk).await;
        let Some(live) = inner.upgrade() else { return };
        match read {
            Ok(0) | Err(_) => {
                fail_all(&live);
                return;
            }
            Ok(count) => framer.feed(&chunk[..count]),
        }
    }
}
