//! The host's bridge client against simulated REAPER and Renoise bridges: the handshake, requests, events, busy and
//! quitting, with golden NDJSON transcripts of the wire (tests/golden/*-bridge.ndjson; KUMI_BLESS=1 rewrites them).
use daw_bridge::auth::Secret;
use daw_bridge::wire::Project;
use daw_mcp_server::client::{BridgeClient, ConnectError, RequestError};
use daw_mcp_server::simulator::{Line, Simulator, SimulatorConfig};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::time::Duration;

const CLIENT: &str = "daw-mcp-server/test";
const WAIT: Duration = Duration::from_secs(5);

fn secret() -> Secret {
    Secret::new("simulated-secret-0123456789-abcdefghijklmnop").unwrap()
}

/// The transcript with what changes from run to run (nonces, proofs, sessions, the pid) replaced by names.
pub fn normalized(lines: &[Line]) -> String {
    let mut out = String::new();
    for line in lines {
        let mut value: Value = serde_json::from_str(&line.text).unwrap_or_else(|_| Value::String(line.text.clone()));
        if let Some(object) = value.as_object_mut() {
            for key in ["nonce", "proof", "session"] {
                if object.contains_key(key) {
                    object.insert(key.into(), format!("<{key}>").into());
                }
            }
            if object.contains_key("pid") {
                object.insert("pid".into(), "<pid>".into());
            }
            if let Some(result) = object.get_mut("result").and_then(Value::as_object_mut) {
                if result.contains_key("pid") {
                    result.insert("pid".into(), "<pid>".into());
                }
            }
        }
        out.push_str(&format!("{} {} {}\n", line.connection, if line.from_host { ">" } else { "<" }, value));
    }
    out
}

pub fn golden(name: &str, made: &str) {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests").join("golden").join(name);
    if std::env::var("KUMI_BLESS").is_ok_and(|value| value == "1") {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, made).unwrap();
        return;
    }
    let kept = std::fs::read_to_string(&path).unwrap_or_else(|_| panic!("{} is missing; run with KUMI_BLESS=1", path.display()));
    assert_eq!(made, kept, "{name} changed; bless it if that's intended");
}

async fn session(simulator: &Simulator) {
    let client = BridgeClient::connect(simulator.address(), secret(), CLIENT).await.unwrap();
    let mut events = client.events();
    assert_eq!(client.request("ping", json!({}), WAIT).await.unwrap(), json!({}));
    let status = client.request("status", json!({}), WAIT).await.unwrap();
    assert_eq!(status["busy"], false);
    assert_eq!(
        client.request("render", json!({}), WAIT).await,
        Err(RequestError::Bridge { code: "unknown-method".into(), message: "no method render".into() })
    );
    simulator.open_project(Some(Project { name: Some("Next".into()), ..Project::default() })).unwrap();
    let event = tokio::time::timeout(WAIT, events.recv()).await.unwrap().unwrap();
    assert_eq!((event.seq, event.name.as_str(), &event.data), (1, "project", &json!({"name": "Next"})));
    assert_eq!(client.request("status", json!({}), WAIT).await.unwrap()["project"], json!({"name": "Next"}));
    simulator.set_busy(Some("rendering"));
    assert!(matches!(client.request("status", json!({}), WAIT).await, Err(RequestError::Bridge { code, .. }) if code == "busy"));
    simulator.set_busy(None);
    simulator.stop();
    for _ in 0..100 {
        if client.is_closed() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(client.is_closed());
    assert_eq!(client.request("ping", json!({}), WAIT).await, Err(RequestError::Closed));
}

#[tokio::test]
async fn reaper_session_transcript() {
    let simulator = Simulator::start(SimulatorConfig::reaper(secret(), "/lab/REAPER")).await.unwrap();
    session(&simulator).await;
    golden("reaper-bridge.ndjson", &normalized(&simulator.transcript()));
}

#[tokio::test]
async fn renoise_session_transcript() {
    let simulator = Simulator::start(SimulatorConfig::renoise(secret(), "/lab/Renoise/V3.5.4")).await.unwrap();
    session(&simulator).await;
    let transcript = normalized(&simulator.transcript());
    assert!(transcript.contains("\"demo\":false"));
    golden("renoise-bridge.ndjson", &transcript);
}

#[tokio::test]
async fn a_wrong_secret_or_a_non_loopback_address_never_connects() {
    let simulator = Simulator::start(SimulatorConfig::reaper(secret(), "/lab/REAPER")).await.unwrap();
    let other = Secret::new("another-secret-0123456789-abcdefghijklmnopq").unwrap();
    let refused = BridgeClient::connect(simulator.address(), other, CLIENT).await;
    assert!(matches!(refused, Err(ConnectError::Handshake(daw_bridge::handshake::HandshakeError::BadProof))));
    // The host never sent its own proof.
    assert!(simulator.transcript().iter().all(|line| !line.text.contains("\"auth\"")));
    let remote = "192.0.2.1:9770".parse().unwrap();
    assert!(matches!(BridgeClient::connect(remote, secret(), CLIENT).await, Err(ConnectError::NotLoopback(_))));
}

#[tokio::test]
async fn silence_reads_as_busy_and_heartbeats_clear_it() {
    let mut config = SimulatorConfig::renoise(secret(), "/lab/Renoise/V3.5.4");
    config.offer.heartbeat_ms = 40;
    let simulator = Simulator::start(config).await.unwrap();
    let client = BridgeClient::connect(simulator.address(), secret(), CLIENT).await.unwrap();
    assert_eq!(client.welcome().heartbeat_ms, 40);
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(!client.busy());
    simulator.set_busy(Some("showing a dialog"));
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(client.busy());
    simulator.set_busy(None);
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(!client.busy());
}

#[tokio::test]
async fn several_connections_each_number_their_own_events() {
    let simulator = Simulator::start(SimulatorConfig::reaper(secret(), "/lab/REAPER")).await.unwrap();
    let first = BridgeClient::connect(simulator.address(), secret(), CLIENT).await.unwrap();
    let mut first_events = first.events();
    simulator.emit("tracks", json!({"count": 3}));
    assert_eq!(tokio::time::timeout(WAIT, first_events.recv()).await.unwrap().unwrap().seq, 1);
    let second = BridgeClient::connect(simulator.address(), secret(), CLIENT).await.unwrap();
    let mut second_events = second.events();
    assert_ne!(first.welcome().session, second.welcome().session);
    simulator.emit("tracks", json!({"count": 4}));
    assert_eq!(tokio::time::timeout(WAIT, first_events.recv()).await.unwrap().unwrap().seq, 2);
    let event = tokio::time::timeout(WAIT, second_events.recv()).await.unwrap().unwrap();
    assert_eq!((event.seq, event.data), (1, json!({"count": 4})));
}

/// A listener that answers the hello with `reply` and closes.
async fn one_reply(reply: &'static str) -> std::net::SocketAddr {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut chunk = [0u8; 1024];
        let _ = stream.read(&mut chunk).await;
        let _ = stream.write_all(reply.as_bytes()).await;
    });
    address
}

#[tokio::test]
async fn refusals_early_closes_and_garbage_during_the_handshake() {
    let refused = BridgeClient::connect(
        one_reply("{\"type\":\"refused\",\"reason\":\"protocol\",\"protocol\":\"kumi-daw/2\"}\n").await,
        secret(),
        CLIENT,
    )
    .await;
    assert!(matches!(
        refused,
        Err(ConnectError::Handshake(daw_bridge::handshake::HandshakeError::Refused { ref reason, protocol: Some(ref protocol) })) if reason == "protocol" && protocol == "kumi-daw/2"
    ));
    assert!(matches!(BridgeClient::connect(one_reply("").await, secret(), CLIENT).await, Err(ConnectError::Closed)));
    assert!(matches!(
        BridgeClient::connect(one_reply("HTTP/1.1 400 Bad Request\r\n\r\n").await, secret(), CLIENT).await,
        Err(ConnectError::Handshake(_))
    ));
    let welcome_first = "{\"type\":\"welcome\",\"session\":\"00000000000000000000000000000000\",\"daw\":\"reaper\",\"bridge\":\"0\",\"dawVersion\":\"0\",\"pid\":1,\"resource\":\"/\",\"project\":null,\"maxFrameBytes\":1,\"heartbeatMs\":1}\n";
    assert!(matches!(BridgeClient::connect(one_reply(welcome_first).await, secret(), CLIENT).await, Err(ConnectError::Handshake(_))));
    // Nothing listening.
    let closed = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await.unwrap().local_addr().unwrap();
    assert!(matches!(BridgeClient::connect(closed, secret(), CLIENT).await, Err(ConnectError::Io(_))));
}

#[tokio::test]
async fn a_bridge_that_never_speaks_is_a_handshake_timeout() {
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let address = listener.local_addr().unwrap();
    let silent = tokio::spawn(async move {
        let (_stream, _) = listener.accept().await.unwrap();
        tokio::time::sleep(Duration::from_secs(5)).await;
    });
    assert!(matches!(
        BridgeClient::connect_within(address, secret(), CLIENT, Duration::from_millis(150)).await,
        Err(ConnectError::Timeout)
    ));
    silent.abort();
}

#[tokio::test]
async fn params_nested_past_the_limit_are_refused_before_sending() {
    let simulator = Simulator::start(SimulatorConfig::reaper(secret(), "/lab/REAPER")).await.unwrap();
    let client = BridgeClient::connect(simulator.address(), secret(), CLIENT).await.unwrap();
    let nest = |levels: usize| (1..levels).fold(json!({}), |inner, _| json!({"a": inner}));
    assert_eq!(client.request("ping", nest(daw_bridge::wire::MAX_PAYLOAD_DEPTH), WAIT).await.unwrap(), json!({}));
    let refused = client.request("ping", nest(daw_bridge::wire::MAX_PAYLOAD_DEPTH + 1), WAIT).await;
    assert!(matches!(refused, Err(RequestError::Bridge { ref code, .. }) if code == "bad-params"), "{refused:?}");
    assert!(!client.is_closed());
    assert_eq!(client.request("ping", json!({}), WAIT).await.unwrap(), json!({}));
    // Two pings reached the bridge, not three.
    assert_eq!(simulator.transcript().iter().filter(|line| line.from_host && line.text.contains("\"ping\"")).count(), 2);
}

#[tokio::test]
async fn an_answer_nested_past_the_limit_costs_that_answer_not_the_connection() {
    use daw_bridge::handshake::{ServerAction, ServerConnection};
    use daw_bridge::wire::Frame;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let address = listener.local_addr().unwrap();
    // A bridge that answers the first request with a line nested 70 levels deep, and the rest properly.
    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let config = SimulatorConfig::reaper(secret(), "/lab/REAPER");
        let mut connection = ServerConnection::new(secret(), 0);
        let mut chunk = vec![0u8; 4096];
        loop {
            let read = stream.read(&mut chunk).await.unwrap_or(0);
            if read == 0 {
                return;
            }
            for action in connection.on_bytes(&chunk[..read], 0, &mut || (config.identity.clone(), config.offer.clone())) {
                match action {
                    ServerAction::Send(bytes) => stream.write_all(&bytes).await.unwrap(),
                    ServerAction::Request(request) if request.id == 1 => {
                        let line =
                            format!("{{\"type\":\"response\",\"id\":1,\"ok\":true,\"result\":{}{}}}\n", "[".repeat(70), "]".repeat(70));
                        stream.write_all(line.as_bytes()).await.unwrap();
                    }
                    ServerAction::Request(request) => {
                        stream.write_all(&Frame::ok(request.id, json!({"n": request.id})).encode()).await.unwrap()
                    }
                    _ => {}
                }
            }
        }
    });
    let client = BridgeClient::connect(address, secret(), CLIENT).await.unwrap();
    // The too-deep answer is dropped alone: that request gets no answer.
    assert!(matches!(client.request("ping", json!({}), Duration::from_millis(300)).await, Err(RequestError::Timeout)));
    assert!(!client.is_closed());
    assert_eq!(client.request("ping", json!({}), WAIT).await.unwrap(), json!({"n": 2}));
}

/// A bridge that welcomes the host and then never reads another byte (a Lua script that never returns, a dialog).
async fn a_bridge_that_stops_reading() -> std::net::SocketAddr {
    use daw_bridge::handshake::{ServerAction, ServerConnection};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let config = SimulatorConfig::reaper(secret(), "/lab/REAPER");
        let mut connection = ServerConnection::new(secret(), 0);
        let mut chunk = vec![0u8; 4096];
        loop {
            let read = stream.read(&mut chunk).await.unwrap_or(0);
            if read == 0 {
                return;
            }
            for action in connection.on_bytes(&chunk[..read], 0, &mut || (config.identity.clone(), config.offer.clone())) {
                match action {
                    ServerAction::Send(bytes) => stream.write_all(&bytes).await.unwrap(),
                    ServerAction::Authenticated => {
                        // Held open, unread.
                        tokio::time::sleep(Duration::from_secs(60)).await;
                        return;
                    }
                    _ => {}
                }
            }
        }
    });
    address
}

#[tokio::test]
async fn a_bridge_that_stopped_reading_cant_hold_a_request_past_its_timeout() {
    let client = BridgeClient::connect(a_bridge_that_stops_reading().await, secret(), CLIENT).await.unwrap();
    let started = std::time::Instant::now();
    // Far more than the socket buffers take: the write blocks for room that never comes.
    let big = client.clone();
    let writing = tokio::spawn(async move { big.request("big", json!({"pad": "x".repeat(32 << 20)}), Duration::from_secs(1)).await });
    tokio::time::sleep(Duration::from_millis(200)).await;
    // Behind it, a small request has its own, shorter timeout.
    let behind = client.request("status", json!({}), Duration::from_millis(300)).await;
    assert!(matches!(behind, Err(RequestError::Timeout)), "{behind:?}");
    // (Building the frame is a few hundred ms of CPU on this single thread, before the write starts.)
    assert!(started.elapsed() < Duration::from_millis(2500), "{:?}", started.elapsed());
    let first = writing.await.unwrap();
    assert!(matches!(first, Err(RequestError::Timeout)), "{first:?}");
    assert!(started.elapsed() < Duration::from_secs(5), "{:?}", started.elapsed());
    // Part of a frame went out, so the connection is gone rather than carrying a corrupt stream.
    assert!(client.is_closed());
    assert!(matches!(client.request("ping", json!({}), WAIT).await, Err(RequestError::Closed)));
}

#[tokio::test]
async fn a_host_that_only_listens_tells_the_bridge_it_is_reading() {
    let simulator = Simulator::start(SimulatorConfig::renoise(secret(), "/lab/Renoise/V3.5.4")).await.unwrap();
    let client = BridgeClient::connect(simulator.address(), secret(), CLIENT).await.unwrap();
    let mut events = client.events();
    let heartbeats = || simulator.transcript().iter().filter(|line| line.from_host && line.text.contains("\"heartbeat\"")).count();
    assert_eq!(heartbeats(), 0);
    // Not enough yet for the host to say anything.
    simulator.emit("tracks", json!({"pad": "x".repeat(4000)}));
    events.recv().await.unwrap();
    assert_eq!(heartbeats(), 0);
    for _ in 0..17 {
        simulator.emit("tracks", json!({"pad": "x".repeat(4000)}));
        events.recv().await.unwrap();
    }
    tokio::time::sleep(Duration::from_millis(100)).await;
    // About 72 KB read: one heartbeat each time 16 KiB came in without the host writing.
    assert!((3..=4).contains(&heartbeats()), "{}", heartbeats());
    // Once the host has written, the count starts over.
    assert_eq!(client.request("ping", json!({}), WAIT).await.unwrap(), json!({}));
    assert!(!client.is_closed());
}
