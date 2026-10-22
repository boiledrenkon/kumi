//! The wire of the read methods (`read`, `snapshot`, `focus`, `focus_body`) between the host's client and a bridge that
//! answers like Kumi's own bridge for Logic, as a golden transcript (tests/golden/logic-reads-bridge.ndjson): every
//! kind and body of the contract, paging, a moved epoch, and what the bridge answers without Accessibility.
mod common;

use common::{golden, normalized};
use daw_bridge::auth::Secret;
use daw_mcp_server::client::{BridgeClient, RequestError};
use daw_mcp_server::simulator::logic::LogicScript;
use daw_mcp_server::simulator::Simulator;
use serde_json::json;
use std::time::Duration;

const WAIT: Duration = Duration::from_secs(5);

fn refused_as(result: Result<serde_json::Value, RequestError>, wanted: &str) -> bool {
    matches!(result, Err(RequestError::Bridge { code, .. }) if code == wanted)
}

#[tokio::test]
async fn the_read_methods_on_the_wire() {
    let secret = Secret::new("simulated-secret-0123456789-abcdefghijklmnop").unwrap();
    let script = LogicScript::new();
    let simulator = Simulator::start(script.config(secret.clone(), "com.apple.logic10")).await.unwrap();
    let client = BridgeClient::connect(simulator.address(), secret, "daw-mcp-server/test").await.unwrap();
    assert_eq!(client.welcome().methods, ["ping", "status", "read", "snapshot", "focus", "focus_body"]);
    assert_eq!(client.welcome().identity.daw_version, "12.3.1");

    // Every kind of the contract: the project, the tracks, the children under their parent, one object by its ref.
    let project = client.request("read", json!({"kind": "project"}), WAIT).await.unwrap();
    assert_eq!((project["items"][0]["tempo"].clone(), project["items"][0]["trackCount"].clone()), (json!(160.0), json!(2)));
    let tracks = client.request("read", json!({"kind": "track"}), WAIT).await.unwrap();
    assert_eq!(tracks["items"].as_array().unwrap().len(), 2);
    assert_eq!((tracks["items"][0]["ref"].clone(), tracks["epoch"].clone()), (json!("1:track:1"), json!("1")));
    let regions = client.request("read", json!({"kind": "region", "parent": "1:track:2"}), WAIT).await.unwrap();
    assert_eq!((regions["items"][0]["kind"].clone(), regions["total"].clone()), (json!("midi"), json!(1)));
    let notes = client.request("read", json!({"kind": "note", "parent": "1:region:1", "limit": 3}), WAIT).await.unwrap();
    assert_eq!(notes["items"].as_array().unwrap().len(), 3);
    assert_eq!(notes["nextCursor"], "1.3");
    let rest = client
        .request("read", json!({"kind": "note", "parent": "1:region:1", "limit": 3, "cursor": notes["nextCursor"]}), WAIT)
        .await
        .unwrap();
    assert_eq!((rest["items"][0]["pitch"].clone(), rest.get("nextCursor").cloned()), (json!(72), None));
    let plugins = client.request("read", json!({"kind": "plugin", "parent": "1:track:1"}), WAIT).await.unwrap();
    assert_eq!(plugins["items"][0]["name"], "Channel EQ");
    assert_eq!(client.request("read", json!({"kind": "send"}), WAIT).await.unwrap()["items"][0]["destination"], "Bus 1");
    assert_eq!(client.request("read", json!({"kind": "marker"}), WAIT).await.unwrap()["items"][0]["name"], "Chorus");
    assert_eq!(client.request("read", json!({"kind": "tempo"}), WAIT).await.unwrap()["items"][0]["bpm"], 160.0);
    let one = client.request("read", json!({"ref": "1:plugin:1.1"}), WAIT).await.unwrap();
    assert_eq!(one["items"][0]["trackRef"], "1:track:1");
    let snapshot = client.request("snapshot", json!({}), WAIT).await.unwrap();
    assert_eq!((snapshot["tracks"].as_array().unwrap().len(), snapshot["markers"].as_array().unwrap().len()), (2, 1));
    let focus = client.request("focus", json!({}), WAIT).await.unwrap();
    assert_eq!((focus["selection"]["regionRef"].clone(), focus["editor"].clone()), (json!("1:region:1"), json!("pianoRoll")));
    let tree = client.request("focus_body", json!({"kind": "tree"}), WAIT).await.unwrap();
    assert_eq!((tree["trackRef"].clone(), tree["strip"]["inserts"][0]["pluginRef"].clone()), (json!("1:track:1"), json!("1:plugin:1.1")));
    let notes_body = client.request("focus_body", json!({"kind": "notes", "ref": "1:region:1", "limit": 2}), WAIT).await.unwrap();
    assert_eq!((notes_body["notes"].as_array().unwrap().len(), notes_body["nextCursor"].clone()), (2, json!("1.2")));
    let timeline = client.request("focus_body", json!({"kind": "timeline", "window": {"from": 0, "to": 8}}), WAIT).await.unwrap();
    assert_eq!(timeline["lanes"][1]["regions"][0]["ref"], "1:region:1");
    let routing = client.request("focus_body", json!({"kind": "routing", "ref": "1:track:1"}), WAIT).await.unwrap();
    assert_eq!(routing["sends"][0]["sendRef"], "1:send:1.1");

    // What the bridge refuses: a kind it doesn't have, notes without their region, a body it doesn't have.
    assert!(refused_as(client.request("read", json!({"kind": "pattern"}), WAIT).await, "bad-params"));
    assert!(refused_as(client.request("read", json!({"kind": "note"}), WAIT).await, "bad-params"));
    assert!(refused_as(client.request("focus_body", json!({"kind": "mixer"}), WAIT).await, "bad-params"));
    assert!(refused_as(client.request("read", json!({"ref": "1:track:9"}), WAIT).await, "failed"));

    // A track changed: the epoch moves, refs and cursors from before are stale, and the new ones read the same.
    script.change_tracks();
    let stale = client.request("read", json!({"ref": "1:track:1"}), WAIT).await.unwrap_err();
    assert!(matches!(&stale, RequestError::Bridge { code, message } if code == "stale" && message.contains("earlier epoch")), "{stale:?}");
    assert!(refused_as(client.request("read", json!({"kind": "note", "parent": "1:region:1", "cursor": "1.3"}), WAIT).await, "stale"));
    assert!(refused_as(client.request("focus_body", json!({"kind": "routing", "ref": "1:track:1"}), WAIT).await, "stale"));
    let again = client.request("read", json!({"kind": "track", "limit": 1}), WAIT).await.unwrap();
    assert_eq!(
        (again["epoch"].clone(), again["items"][0]["ref"].clone(), again["nextCursor"].clone()),
        (json!("2"), json!("2:track:1"), json!("2.1"))
    );
    assert_eq!(client.request("focus", json!({}), WAIT).await.unwrap()["selection"]["trackRef"], "2:track:1");

    // Without Accessibility the status says what is missing, and every read fails in words.
    simulator.set_permission(Some("accessibility"));
    assert_eq!(client.request("status", json!({}), WAIT).await.unwrap()["permission"], "accessibility");
    let refused = client.request("focus", json!({}), WAIT).await.unwrap_err();
    assert!(
        matches!(&refused, RequestError::Bridge { code, message } if code == "failed" && message.contains("Accessibility")),
        "{refused:?}"
    );
    simulator.set_permission(None);
    assert_eq!(script.asked().len(), 25, "{:?}", script.asked());

    golden("logic-reads-bridge.ndjson", &normalized(&simulator.transcript()));
    simulator.stop();
}
