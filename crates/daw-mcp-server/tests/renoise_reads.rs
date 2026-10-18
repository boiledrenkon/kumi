//! The wire of the read methods (`read`, `snapshot`, `focus`, `focus_body`) between the host's client and a bridge that
//! answers like the Renoise tool's, as a golden transcript (tests/golden/renoise-reads-bridge.ndjson), and what the host's
//! client does as it reads pages bigger than the bridge's credit.
mod common;

use common::{golden, normalized};
use daw_bridge::auth::Secret;
use daw_mcp_server::client::{BridgeClient, RequestError};
use daw_mcp_server::simulator::renoise::RenoiseScript;
use daw_mcp_server::simulator::Simulator;
use serde_json::{json, Value};
use std::time::Duration;

const WAIT: Duration = Duration::from_secs(5);
const FRAME_BOUND: usize = 256 * 1024;

fn secret() -> Secret {
    Secret::new("simulated-secret-0123456789-abcdefghijklmnop").unwrap()
}

#[tokio::test]
async fn the_read_methods_on_the_wire() {
    let secret = secret();
    let script = RenoiseScript::new();
    let simulator = Simulator::start(script.config(secret.clone(), "/lab/Renoise")).await.unwrap();
    let client = BridgeClient::connect(simulator.address(), secret, "daw-mcp-server/test").await.unwrap();
    assert_eq!(client.welcome().methods, ["ping", "status", "read", "snapshot", "focus", "focus_body"]);
    assert_eq!(client.welcome().credit_bytes, Some(512 * 1024), "the tool sends within credit");

    let song = client.request("read", json!({"kind": "song"}), WAIT).await.unwrap();
    assert_eq!(song["items"][0]["name"], "Demo");
    let page = client.request("read", json!({"kind": "track", "limit": 2}), WAIT).await.unwrap();
    assert_eq!(page["items"].as_array().unwrap().len(), 2);
    let rest = client.request("read", json!({"kind": "track", "limit": 2, "cursor": page["nextCursor"]}), WAIT).await.unwrap();
    assert_eq!(rest["items"].as_array().unwrap().len(), 2);
    let one = client.request("read", json!({"ref": "1:track:3"}), WAIT).await.unwrap();
    assert_eq!(one["items"][0]["name"], "Drums");
    let lines = client.request("read", json!({"kind": "line", "parent": "1:pattern:1", "track": 1}), WAIT).await.unwrap();
    assert_eq!(lines["items"][1]["notes"][0]["note"], "D#4");
    let snapshot = client.request("snapshot", json!({}), WAIT).await.unwrap();
    assert_eq!(snapshot["tracks"].as_array().unwrap().len(), 7);
    let focus = client.request("focus", json!({}), WAIT).await.unwrap();
    assert_eq!(focus["selection"]["trackRef"], "1:track:1");
    for (kind, reference) in
        [("matrix", None), ("phrase", Some("1:phrase:1.1")), ("instrument", Some("1:instrument:1")), ("dsp", Some("1:track:1"))]
    {
        let mut params = json!({"kind": kind});
        if let Some(reference) = reference {
            params["ref"] = reference.into();
        }
        client.request("focus_body", params, WAIT).await.unwrap();
    }
    client.request("focus_body", json!({"kind": "pattern", "window": {"from": 3, "to": 6}}), WAIT).await.unwrap();

    // What the tool refuses, in its words and codes.
    for (params, code) in [
        (json!({"kind": "nonsense"}), "bad-params"),
        (json!({"ref": "0:track:1"}), "stale"),
        (json!({"kind": "track", "cursor": "0.2"}), "stale"),
    ] {
        let error = client.request("read", params, WAIT).await.unwrap_err();
        assert!(matches!(&error, RequestError::Bridge { code: got, .. } if got == code), "{error:?}");
    }
    assert_eq!(script.asked().len(), 15);

    golden("renoise-reads-bridge.ndjson", &normalized(&simulator.transcript()));
    simulator.stop();
}

/// A pattern of 300 full lines is far more than the credit window and each page is most of a frame: the host's client
/// gives credit back as it reads, so a bridge that holds back at zero credit goes on.
#[tokio::test]
async fn pages_bigger_than_the_credit_window_arrive_as_the_host_reads() {
    let secret = secret();
    let script = RenoiseScript::with_full_pattern(300);
    let simulator = Simulator::start(script.config(secret.clone(), "/lab/Renoise")).await.unwrap();
    let client = BridgeClient::connect(simulator.address(), secret, "daw-mcp-server/test").await.unwrap();

    let mut seen = Vec::new();
    let mut cursor: Option<Value> = None;
    let mut pages = 0;
    loop {
        let mut params = json!({"kind": "line", "parent": "1:pattern:1", "track": 1, "limit": 200});
        if let Some(cursor) = &cursor {
            params["cursor"] = cursor.clone();
        }
        let page = client.request("read", params, WAIT).await.unwrap();
        assert!(page.to_string().len() <= FRAME_BOUND, "page {pages} is {} bytes", page.to_string().len());
        seen.extend(page["items"].as_array().unwrap().iter().map(|row| row["line"].as_u64().unwrap()));
        pages += 1;
        match page.get("nextCursor") {
            Some(next) => cursor = Some(next.clone()),
            None => break,
        }
    }
    assert!(pages >= 2, "{pages} pages");
    assert_eq!(seen, (1..=300).collect::<Vec<u64>>(), "every line once, in order");
    // 300 lines of about 1.6 KB: over the 512 KiB window, so the bridge could only have finished by being given credit.
    let credits = simulator.transcript().iter().filter(|line| line.from_host && line.text.contains("\"type\":\"credit\"")).count();
    assert!(credits >= 1, "the host gave credit back: {credits} grants");
    simulator.stop();
}
