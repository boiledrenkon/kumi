//! The wire of the read methods (`read`, `snapshot`, `focus`, `focus_body`) between the host's client and a bridge
//! that answers like REAPER's, as a golden transcript (tests/golden/reaper-reads-bridge.ndjson).
mod common;

use common::{golden, normalized};
use daw_bridge::auth::Secret;
use daw_mcp_server::client::{BridgeClient, RequestError};
use daw_mcp_server::simulator::reaper::ReaperScript;
use daw_mcp_server::simulator::Simulator;
use serde_json::json;
use std::time::Duration;

const WAIT: Duration = Duration::from_secs(5);

#[tokio::test]
async fn the_read_methods_on_the_wire() {
    let secret = Secret::new("simulated-secret-0123456789-abcdefghijklmnop").unwrap();
    let script = ReaperScript::new(3, 0);
    let simulator = Simulator::start(script.config(secret.clone(), "/lab/REAPER")).await.unwrap();
    let client = BridgeClient::connect(simulator.address(), secret, "daw-mcp-server/test").await.unwrap();
    assert_eq!(client.welcome().methods, ["ping", "status", "read", "snapshot", "focus", "focus_body"]);

    let page = client.request("read", json!({"kind": "track", "limit": 2}), WAIT).await.unwrap();
    assert_eq!(page["items"].as_array().unwrap().len(), 2);
    let rest = client.request("read", json!({"kind": "track", "limit": 2, "cursor": page["nextCursor"]}), WAIT).await.unwrap();
    assert_eq!(rest["items"].as_array().unwrap().len(), 2);
    client.request("read", json!({"ref": "r:track:{T0001}"}), WAIT).await.unwrap();
    let snapshot = client.request("snapshot", json!({}), WAIT).await.unwrap();
    assert_eq!(snapshot["tracks"].as_array().unwrap().len(), 4);
    client.request("focus", json!({}), WAIT).await.unwrap();
    client.request("focus_body", json!({"kind": "routing", "ref": "r:track:{T0001}"}), WAIT).await.unwrap();
    assert_eq!(
        client.request("read", json!({"kind": "pattern"}), WAIT).await,
        Err(RequestError::Bridge { code: "bad-params".into(), message: "REAPER has no kind \"pattern\" to read".into() })
    );
    script.switch_tab();
    let stale = client.request("read", json!({"kind": "track", "cursor": page["nextCursor"]}), WAIT).await.unwrap_err();
    assert!(matches!(stale, RequestError::Bridge { code, .. } if code == "bad-params"));

    golden("reaper-reads-bridge.ndjson", &normalized(&simulator.transcript()));
    simulator.stop();
}
