//! The host's change tools (M3 contract §3a) against the simulated Logic Pro, which answers as the bridge does: a
//! header's control and the tempo put back by value with no step of Logic's undo, a rename, a track added or deleted
//! and a marker made, moved or deleted each a step of it (it has no undo block: a plan's step is counted), a track
//! added below the selected one moving the epoch with a restore's ref carried past it, a deleted track and a deleted
//! marker back through Logic's own undo while that step is on top, a write Logic doesn't answer after left uncertain
//! and never sent twice (an apply's and an undo's), the kinds the bridge can't make refused saying why, the transport
//! and the playhead, and what the bridge receives for every method, as a golden of the wire
//! (tests/golden/logic-changes-bridge.ndjson).
mod common;

use common::{golden, normalized, Session};
use daw_bridge::files::ensure_secret;
use daw_bridge::paths::BridgePaths;
use daw_bridge::Daw;
use daw_mcp_server::host::Host;
use daw_mcp_server::simulator::logic::LogicScript;
use daw_mcp_server::simulator::Simulator;
use serde_json::{json, Value};
use std::sync::Arc;

const BUNDLE: &str = "com.apple.logic10";

struct Lab {
    _home: tempfile::TempDir,
    session: Session,
    script: LogicScript,
    simulator: Simulator,
    /// The bridge's events as the host passes them on to its client, the journal's transaction ids filled in.
    events: tokio::sync::broadcast::Receiver<Value>,
    next_id: u64,
}

impl Lab {
    async fn start() -> Lab {
        let home = tempfile::tempdir().unwrap();
        let paths = BridgePaths::new(home.path(), Daw::Logic);
        let secret = ensure_secret(&paths.secret()).unwrap();
        let script = LogicScript::new();
        let mut config = script.config(secret, BUNDLE);
        config.discovery_dir = Some(paths.discovery());
        let simulator = Simulator::start(config).await.unwrap();
        let host = Arc::new(Host::new(Daw::Logic, home.path()));
        let events = host.events();
        let session = Session::start_with(host, home.path());
        Lab { _home: home, session, script, simulator, events, next_id: 100 }
    }

    async fn tool(&mut self, name: &str, arguments: Value) -> (Value, bool) {
        self.next_id += 1;
        let id = self.next_id;
        self.session.tool_full(id, name, arguments, false).await
    }

    /// A change previewed (in the epoch the project is in): the preview's answer.
    async fn preview(&mut self, kind: &str, input: Value) -> Value {
        let (preview, is_error) = self.try_preview(kind, input).await;
        assert!(!is_error, "{kind}: {preview}");
        preview
    }

    async fn try_preview(&mut self, kind: &str, input: Value) -> (Value, bool) {
        let epoch = self.script.epoch().to_string();
        self.tool("change_preview", json!({"kind": kind, "input": input, "epoch": epoch})).await
    }

    async fn apply(&mut self, preview: &Value, key: &str) -> (Value, bool) {
        let (id, confirmation) = (preview["transactionId"].clone(), preview["confirmation"].clone());
        self.tool("change_apply", json!({"transactionId": id, "confirmation": confirmation, "idempotencyKey": key})).await
    }

    async fn undo(&mut self, preview: &Value, key: &str, discard: bool) -> (Value, bool) {
        let id = preview["transactionId"].clone();
        self.tool("change_undo", json!({"transactionId": id, "confirmation": "undo", "idempotencyKey": key, "discard": discard})).await
    }

    /// A change made: its preview and its apply's answer (which must have gone through).
    async fn make(&mut self, kind: &str, input: Value, key: &str) -> (Value, Value) {
        let preview = self.preview(kind, input).await;
        let (applied, is_error) = self.apply(&preview, key).await;
        assert!(!is_error && applied["state"] == "applied", "{kind}: {applied}");
        (preview, applied)
    }

    /// The next `undone_in_daw` the host passes on.
    async fn undone_in_daw(&mut self) -> Value {
        loop {
            let event = tokio::time::timeout(std::time::Duration::from_secs(5), self.events.recv()).await.unwrap().unwrap();
            if event["name"] == "undone_in_daw" {
                return event["data"].clone();
            }
        }
    }
}

/// A desc's nonce (`k` and four base-36 characters, in parentheses) made the same in every run.
fn without_nonces(transcript: &str) -> String {
    let mut out = String::with_capacity(transcript.len());
    let mut rest = transcript;
    while let Some(at) = rest.find("(k") {
        let after = &rest[at + 2..];
        let nonce = after.get(..4).filter(|n| n.bytes().all(|b| b.is_ascii_digit() || b.is_ascii_lowercase()));
        match nonce.filter(|_| after.as_bytes().get(4) == Some(&b')')) {
            Some(_) => {
                out.push_str(&rest[..at]);
                out.push_str("(k<nonce>)");
                rest = &after[5..];
            }
            None => {
                out.push_str(&rest[..at + 2]);
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

fn position(bar: i64, beat: i64) -> Value {
    json!({"bar": bar, "beat": beat, "division": 1, "tick": 1})
}

/// A header's control: no step of Logic's undo for the change nor for its undo (the Edit menu stays "Can't Undo");
/// both are writes, read back, and each key is answered once.
#[tokio::test]
async fn a_change_is_previewed_applied_and_undone_by_value_with_no_step_of_logics_undo() {
    let mut lab = Lab::start().await;
    let preview = lab.preview("track-volume", json!({"ref": "1:track:1", "volumeDb": -6})).await;
    assert_eq!(
        (&preview["prior"], &preview["target"]["ref"], &preview["epoch"]),
        (&json!({"volumeDb": 0.0}), &json!("1:track:1"), &json!("1"))
    );
    assert_eq!(preview["display"]["title"], "Vocal volume −6 dB");
    assert_eq!(lab.script.volume(1), 0.0, "a preview changes nothing");

    let (wrong, is_error) =
        lab.tool("change_apply", json!({"transactionId": preview["transactionId"], "confirmation": "c0000", "idempotencyKey": "a1"})).await;
    assert!(is_error && wrong["code"] == "confirmation", "{wrong}");

    let (applied, is_error) = lab.apply(&preview, "a1").await;
    assert!(!is_error && applied["state"] == "applied", "{applied}");
    assert_eq!(applied["applied"], json!({"volumeDb": -6.0}), "the value read back");
    let desc = applied["undo"]["desc"].as_str().unwrap().to_string();
    assert!(desc.starts_with("Kumi: Vocal volume −6 dB (k") && desc.ends_with(')'), "{desc}");
    assert_eq!(applied["undo"], json!({"desc": desc, "entry": false}), "no step of Logic's undo, so no title");
    assert_eq!(lab.script.volume(1), -6.0);
    // The same key again: the same answer, and Logic changed once.
    assert_eq!(lab.apply(&preview, "a1").await, (applied.clone(), false));
    assert!(lab.script.undo_titles().is_empty(), "{:?}", lab.script.undo_titles());
    // The value shows in a read after the change.
    let (row, _) = lab.tool("read", json!({"ref": "1:track:1"})).await;
    assert_eq!(row["items"][0]["volumeDb"], -6.0, "{row}");

    let (undone, is_error) = lab.undo(&preview, "u1", false).await;
    assert!(!is_error && undone["state"] == "undone", "{undone}");
    assert_eq!(
        (undone["restored"].clone(), undone["display"]["title"].clone()),
        (json!({"volumeDb": 0.0}), json!("Vocal volume 0 dB (put back)"))
    );
    assert_eq!(lab.script.volume(1), 0.0);
    assert!(lab.script.undo_titles().is_empty(), "put back by value: Logic's Edit menu still says Can't Undo");
    assert_eq!(lab.undo(&preview, "u1", false).await, (undone, false));
    let (again, is_error) = lab.undo(&preview, "u2", false).await;
    assert!(is_error && again["code"] == "undone", "{again}");
    lab.simulator.stop();
}

/// Logic has no undo block: a plan's step is counted, a mute in it is no step of Logic's undo and a rename is one
/// ("Renaming"), which the producer's Cmd-Z takes back and the bridge names to the host by the desc it kept.
#[tokio::test]
async fn a_plan_is_counted_and_only_its_rename_is_a_step_of_logics_undo_which_cmd_z_takes_back() {
    let mut lab = Lab::start().await;
    let (begun, is_error) = lab.tool("undo_step_begin", json!({"label": "Tighten the keys"})).await;
    assert!(!is_error && begun["stepId"] == "s1", "{begun}");
    let (_, muted) = lab.make("track-mute", json!({"ref": "1:track:2", "mute": true}), "a1").await;
    assert_eq!(muted["undo"]["entry"], false);
    let (_, renamed) = lab.make("track-rename", json!({"ref": "1:track:2", "name": "Lead"}), "a2").await;
    assert_eq!(renamed["display"]["title"], "Renamed track “Keys” → “Lead”");
    assert_eq!((renamed["undo"]["entry"].clone(), renamed["undo"]["title"].clone()), (json!(true), json!("Undo Renaming")));
    assert_eq!(renamed["epoch"], "2", "a renamed header moves the epoch, as the observers count it");
    assert_eq!((lab.script.track(2)["mute"].clone(), lab.script.track(2)["name"].clone()), (json!(true), json!("Lead")));
    let history = lab.script.undo_history();
    assert_eq!(history.len(), 1, "the rename alone is a step: {history:?}");
    assert!(history[0].starts_with("Kumi: Renamed track “Keys” → “Lead” (k"), "{history:?}");
    assert_eq!(lab.script.undo_titles(), ["Undo Renaming"]);
    let (ended, is_error) = lab.tool("undo_step_end", json!({"stepId": "s1", "label": "Keys muted and 1 more"})).await;
    assert!(!is_error, "{ended}");
    assert!(ended["desc"].as_str().unwrap().starts_with("Kumi: Keys muted and 1 more (k"), "{ended}");
    assert_eq!(
        (ended["entry"].clone(), ended["changes"].clone()),
        (json!(false), json!(2)),
        "counted, no step of Logic's undo for the plan"
    );

    // The producer presses Cmd-Z: the rename comes off, and the bridge names it by its desc; the host names the change.
    let (title, desc) = lab.script.undo_last().expect("a step");
    assert_eq!(title, "Undo Renaming");
    lab.simulator.emit("undone_in_daw", json!({"descs": [desc.clone().unwrap()], "how": "undo", "epoch": "2"}));
    let named = lab.undone_in_daw().await;
    assert_eq!((named["transactionIds"].clone(), named["how"].clone()), (json!([renamed["transactionId"]]), json!("undo")));
    assert_ne!(renamed["transactionId"], muted["transactionId"]);
    lab.simulator.stop();
}

#[tokio::test]
async fn what_the_producer_changed_since_is_never_overwritten_unless_kumi_is_told_to() {
    let mut lab = Lab::start().await;
    // Moved between the preview and the apply: nothing is written.
    let preview = lab.preview("track-volume", json!({"ref": "1:track:1", "volumeDb": -6})).await;
    lab.script.set_volume(1, -3.0);
    let (refused, is_error) = lab.apply(&preview, "a1").await;
    assert!(is_error && refused["code"] == "changed" && refused["error"] == "Vocal's volume changed since the preview", "{refused}");
    assert_eq!(lab.script.volume(1), -3.0);

    // Moved after the apply: the undo is refused and the change stays undoable.
    let preview = lab.preview("track-volume", json!({"ref": "1:track:1", "volumeDb": -6})).await;
    lab.apply(&preview, "a1").await;
    lab.script.set_volume(1, -9.0);
    let (refused, is_error) = lab.undo(&preview, "u1", false).await;
    assert!(
        is_error && refused["code"] == "changed" && refused["error"] == "Vocal's volume changed in Logic Pro since Kumi set it",
        "{refused}"
    );
    assert_eq!(lab.script.volume(1), -9.0);
    // Put back by hand, the same undo goes through.
    lab.script.set_volume(1, -6.0);
    let (undone, is_error) = lab.undo(&preview, "u1", false).await;
    assert!(!is_error && undone["state"] == "undone", "{undone}");
    assert_eq!(lab.script.volume(1), -3.0, "back to what the preview saw");

    // Told to discard: the values go back whatever happened since.
    let preview = lab.preview("track-volume", json!({"ref": "1:track:1", "volumeDb": -12})).await;
    lab.apply(&preview, "a1").await;
    lab.script.set_volume(1, 1.0);
    let (undone, is_error) = lab.undo(&preview, "u1", true).await;
    assert!(!is_error && undone["state"] == "undone", "{undone}");
    assert_eq!(lab.script.volume(1), -3.0);
    lab.simulator.stop();
}

/// A track added or deleted (and a header renamed) moves Logic's epoch: a change previewed before is refused as
/// stale, the reads carry the new epoch, and a change made before is still undone, its ref carried to where its track
/// is now. A new track goes below the selected one, so `index` counts from 1. A deleted track comes back by Logic's
/// own undo only while that step is on top.
#[tokio::test]
async fn a_track_added_below_a_track_moves_the_epoch_and_changes_before_it_are_still_undone_by_their_place() {
    let mut lab = Lab::start().await;
    let (rename, renamed) = lab.make("track-rename", json!({"ref": "1:track:2", "name": "Lead"}), "a1").await;
    assert_eq!(renamed["epoch"], "2");
    let (volume, _) = lab.make("track-volume", json!({"ref": "2:track:1", "volumeDb": -6}), "a2").await;
    let stale = lab.preview("track-mute", json!({"ref": "2:track:2", "mute": true})).await;

    let (add, added) = lab.make("track-add", json!({"name": "Pad", "kind": "instrument", "index": 1}), "a3").await;
    assert_eq!((add["prior"].clone(), add["target"].clone()), (json!({"tracks": 2}), json!({"ref": null, "kind": "track", "index": 1})));
    assert_eq!(add["display"], json!({"title": "Added instrument track “Pad”", "index": 1}));
    assert_eq!((added["epoch"].clone(), added["created"].clone()), (json!("3"), json!({"ref": "3:track:2", "kind": "track"})));
    assert_eq!(
        (added["applied"].clone(), added["undo"]["title"].clone()),
        (json!({"index": 2, "name": "Pad", "kind": "instrument"}), json!("Undo Create Track"))
    );
    assert_eq!(lab.script.track_count(), 3);
    let (page, _) = lab.tool("read", json!({"kind": "track"})).await;
    assert_eq!(
        (page["epoch"].clone(), page["total"].clone(), page["items"][1]["name"].clone(), page["items"][2]["name"].clone()),
        (json!("3"), json!(3), json!("Pad"), json!("Lead"))
    );
    // Previewed before the add: refused as stale, nothing written.
    let (refused, is_error) = lab.apply(&stale, "a4").await;
    assert!(is_error && refused["code"] == "stale", "{refused}");
    assert_eq!(lab.script.track(3)["mute"], json!(false));
    // Index 0 (above every track) isn't reachable; a kind Logic's menu hasn't is refused.
    let (refused, is_error) = lab.try_preview("track-add", json!({"index": 0})).await;
    assert!(
        is_error && refused["code"] == "bad-params" && refused["error"].as_str().unwrap().contains("below the selected one"),
        "{refused}"
    );
    let (refused, is_error) = lab.try_preview("track-add", json!({"kind": "drummer"})).await;
    assert!(is_error && refused["error"] == "kind must be audio, instrument or external", "{refused}");

    // Each undone, the ones from before the add by the place their track had then: the rename by value (a fresh
    // step of Logic's undo), the added track by its deletion, the fader by value.
    let (undone, is_error) = lab.undo(&rename, "u1", false).await;
    assert!(!is_error && undone["state"] == "undone" && undone["epoch"] == "4", "{undone}");
    assert_eq!(lab.script.track(3)["name"], json!("Keys"));
    let (undone, is_error) = lab.undo(&add, "u2", false).await;
    assert!(!is_error && undone["restored"] == json!({"deleted": "3:track:2"}) && undone["epoch"] == "5", "{undone}");
    assert_eq!(lab.script.track_count(), 2);
    let (undone, is_error) = lab.undo(&volume, "u3", false).await;
    assert!(!is_error && undone["state"] == "undone", "{undone}");
    assert_eq!(lab.script.volume(1), 0.0);
    assert_eq!(lab.script.undo_titles(), ["Undo Renaming", "Undo Create Track", "Undo Renaming", "Undo Delete Tracks"]);

    // A track deleted: its restore is Cmd-Z, while Logic's undo has it on top; with another step on top, Kumi can't.
    let (delete, deleted) = lab.make("track-delete", json!({"ref": "5:track:2"}), "a5").await;
    assert_eq!(delete["display"]["note"], "It holds 1 region(s); only Logic Pro's own undo brings a deleted track back.");
    assert_eq!(
        (deleted["epoch"].clone(), deleted["applied"].clone(), deleted["undo"]["title"].clone()),
        (json!("6"), json!({"deleted": true}), json!("Undo Delete Tracks"))
    );
    assert_eq!(lab.script.track_count(), 1);
    let (back, is_error) = lab.undo(&delete, "u4", false).await;
    assert!(!is_error && back["restored"]["ref"] == "7:track:2" && back["restored"]["pressed"] == "Undo Delete Tracks", "{back}");
    assert_eq!(back["display"], json!({"title": "“Keys” put back", "note": "Put back with Logic Pro's own undo."}));
    assert_eq!((lab.script.track_count(), lab.script.track(2)["name"].clone()), (2, json!("Keys")));
    let (delete, _) = lab.make("track-delete", json!({"ref": "7:track:2"}), "a6").await;
    lab.make("track-rename", json!({"ref": "8:track:1", "name": "Vox"}), "a7").await;
    let (kept, is_error) = lab.undo(&delete, "u5", false).await;
    assert!(
        is_error && kept["code"] == "failed" && kept["error"].as_str().unwrap().contains("(its Edit menu says \"Undo Renaming\")"),
        "{kept}"
    );
    assert_eq!(lab.script.track_count(), 1, "nothing changed");
    lab.simulator.stop();
}

/// A write Logic doesn't answer after: the bridge answers uncertain, the journal keeps the change uncertain (never
/// applied again, never undone blindly), and an undo that went the same way is never sent twice.
#[tokio::test]
async fn a_write_logic_doesnt_answer_after_is_uncertain_and_is_never_sent_twice() {
    let mut lab = Lab::start().await;
    lab.script.uncertain_once();
    let preview = lab.preview("track-volume", json!({"ref": "1:track:1", "volumeDb": -6})).await;
    let (unsure, is_error) = lab.apply(&preview, "a1").await;
    assert!(is_error && unsure["state"] == "uncertain" && unsure["code"] == "uncertain", "{unsure}");
    assert_eq!(unsure["transactionId"], preview["transactionId"]);
    assert!(unsure["error"].as_str().unwrap().contains("didn't answer after the change was sent"), "{unsure}");
    assert_eq!(lab.script.volume(1), -6.0, "Logic took the write");
    assert!(lab.script.undo_titles().is_empty(), "a header's control makes no step");
    // The same key: the same answer; another: the change was uncertain already; its undo: refused, not sent.
    assert_eq!(lab.apply(&preview, "a1").await, (unsure.clone(), true));
    let (again, is_error) = lab.apply(&preview, "a2").await;
    assert!(is_error && again["code"] == "applied" && again["error"].as_str().unwrap().contains("uncertain"), "{again}");
    let asked = lab.script.asked().len();
    let (refused, is_error) = lab.undo(&preview, "u1", false).await;
    assert!(
        is_error && refused["code"] == "uncertain" && refused["error"].as_str().unwrap().contains("won't undo it blindly"),
        "{refused}"
    );
    assert_eq!(lab.script.asked().len(), asked, "nothing reached the bridge");

    // An undo Logic doesn't answer after: the restore may have run, so the change is uncertain and the undo is never
    // sent again, under any key.
    let (pan, applied) = lab.make("track-pan", json!({"ref": "1:track:1", "pan": 0.25}), "a3").await;
    assert_eq!(
        (applied["display"]["title"].clone(), applied["applied"].clone()),
        (json!("Vocal pan 25% right"), json!({"pan": 0.25, "steps": 16}))
    );
    lab.script.uncertain_once();
    let (unsure, is_error) = lab.undo(&pan, "u2", false).await;
    assert!(is_error && unsure["state"] == "uncertain" && unsure["code"] == "undo-uncertain" && unsure["answered"] == true, "{unsure}");
    assert!(unsure["error"].as_str().unwrap().contains("didn't answer after the change was sent"), "{unsure}");
    assert_eq!(lab.script.track(1)["pan"], json!(0), "Logic put the pan back");
    let asked = lab.script.asked().len();
    for key in ["u2", "u3"] {
        let (refused, is_error) = lab.undo(&pan, key, false).await;
        assert!(
            is_error && refused["code"] == "undo-uncertain" && refused["error"].as_str().unwrap().contains("won't undo it again"),
            "{key}: {refused}"
        );
    }
    assert_eq!(lab.script.asked().len(), asked, "nothing reached the bridge");
    lab.simulator.stop();
}

/// Markers by their place in the Marker List, numbered by Logic, each a step of its undo; a deleted one back through
/// Logic's own undo while on top, else made again; the tempo in whole BPM, no step; and the kinds the bridge can't
/// make, refused saying why.
#[tokio::test]
async fn markers_and_the_tempo_change_and_come_back_in_logics_words() {
    let mut lab = Lab::start().await;
    let (tempo, applied) = lab.make("tempo-set", json!({"bpm": 128}), "a1").await;
    assert_eq!((tempo["prior"].clone(), tempo["target"].clone()), (json!({"bpm": 160.0}), json!({"ref": null, "kind": "project"})));
    assert_eq!((applied["display"]["title"].clone(), applied["undo"]["entry"].clone()), (json!("Tempo 128 BPM"), json!(false)));
    let (read, _) = lab.tool("read", json!({"kind": "tempo"})).await;
    let (focus, _) = lab.tool("focus", json!({})).await;
    assert_eq!((read["items"][0]["bpm"].clone(), focus["transport"]["tempo"].clone()), (json!(128.0), json!(128.0)));
    let (undone, _) = lab.undo(&tempo, "u1", false).await;
    assert_eq!((undone["display"]["title"].clone(), lab.script.tempo()), (json!("Tempo back to 160 BPM"), 160.0));
    let rounded = lab.preview("tempo-set", json!({"bpm": 120.4})).await;
    assert_eq!(rounded["display"]["title"], "Tempo 120 BPM");
    assert!(rounded["display"]["note"].as_str().unwrap().contains("was rounded to 120"), "{rounded}");

    // A marker can't be named (Logic numbers it) nor placed in seconds.
    let (refused, is_error) = lab.try_preview("marker-add", json!({"positionBeats": 16, "name": "Verse"})).await;
    assert!(
        is_error && refused["code"] == "bad-params" && refused["error"].as_str().unwrap().contains("names markers itself"),
        "{refused}"
    );
    let (refused, is_error) = lab.try_preview("marker-add", json!({"position": 6})).await;
    assert!(is_error && refused["error"].as_str().unwrap().contains("not seconds"), "{refused}");
    // One added before the recorded one is first in the list; a move re-sorts it; a delete and the undos, each a step.
    let (add, added) = lab.make("marker-add", json!({"positionBeats": 16}), "a2").await;
    assert_eq!((add["prior"].clone(), add["display"]["title"].clone()), (json!({"markers": 1}), json!("Marker at bar 5")));
    assert_eq!(
        (added["created"].clone(), added["applied"]["position"].clone()),
        (json!({"ref": "1:marker:1", "kind": "marker"}), position(5, 1))
    );
    assert_eq!(
        (added["display"]["title"].clone(), added["undo"]["title"].clone()),
        (json!("Marker “Marker 2” at bar 5"), json!("Undo Create Marker"))
    );
    let (page, _) = lab.tool("read", json!({"kind": "marker"})).await;
    assert_eq!(page["items"].as_array().unwrap().iter().map(|m| m["name"].as_str().unwrap()).collect::<Vec<_>>(), ["Marker 2", "Marker 1"]);
    assert_eq!(page["items"][0]["position"], position(5, 1));
    let (moved, applied) = lab.make("marker-move", json!({"ref": "1:marker:1", "positionBeats": 40}), "a3").await;
    assert_eq!(
        (moved["prior"].clone(), applied["display"]["title"].clone()),
        (json!({"position": position(5, 1)}), json!("Moved marker “Marker 2” to bar 11"))
    );
    assert_eq!(applied["undo"]["title"], "Undo Value Change");
    assert_eq!(lab.script.markers(), [("Marker 1".to_string(), 32.0), ("Marker 2".to_string(), 40.0)]);
    let (delete, deleted) = lab.make("marker-delete", json!({"ref": "1:marker:2"}), "a4").await;
    assert_eq!(
        (deleted["applied"].clone(), deleted["display"]["title"].clone(), deleted["undo"]["title"].clone()),
        (json!({"deleted": true}), json!("Deleted marker “Marker 2”"), json!("Undo Delete"))
    );
    assert_eq!(lab.script.markers().len(), 1);
    let (back, is_error) = lab.undo(&delete, "u2", false).await;
    assert!(!is_error && back["restored"]["ref"] == "1:marker:2" && back["restored"]["pressed"] == "Undo Delete", "{back}");
    assert_eq!(back["display"], json!({"title": "Marker “Marker 2” put back", "note": "Put back with Logic Pro's own undo."}));
    let (undone, is_error) = lab.undo(&moved, "u3", false).await;
    assert!(!is_error && undone["display"]["title"] == "marker “Marker 2” back at bar 5", "{undone}");
    assert_eq!(lab.script.markers(), [("Marker 2".to_string(), 16.0), ("Marker 1".to_string(), 32.0)]);
    let (gone, is_error) = lab.undo(&add, "u4", false).await;
    assert!(
        !is_error
            && gone["restored"] == json!({"deleted": "1:marker:1"})
            && gone["display"]["title"] == "marker “Marker 2” taken away again",
        "{gone}"
    );
    assert_eq!(lab.script.markers(), [("Marker 1".to_string(), 32.0)]);
    assert_eq!(lab.script.undo_titles(), ["Undo Create Marker", "Undo Value Change", "Undo Value Change", "Undo Delete"]);
    assert_eq!(lab.script.epoch(), 1, "markers and the tempo never move the epoch");

    // What the bridge can't make yet is refused saying why (a track moved or coloured among them: the headers can't be
    // dragged, and a colour could never be read back); what it makes beyond the simulator says so too.
    for (kind, why) in [
        ("marker-rename", "a marker's name cell in the Marker List isn't a text field"),
        ("fx-add", "the insert slot's pop-up menu can't be driven from outside"),
        ("fx-param", "a plug-in's controls aren't reached yet"),
        ("send-volume", "the send slot's pop-up menu can't be driven from outside"),
        ("track-move", "the track headers can't be dragged from outside, no key command reorders tracks, and the headers offer no action but their menu"),
        ("track-color", "Logic Pro's Color palette shows its swatches to Accessibility as bare positions, and the track headers show no colour: nothing to read back"),
    ] {
        let (refused, is_error) = lab.try_preview(kind, json!({"ref": "1:marker:1"})).await;
        assert!(is_error && refused["code"] == "bad-params", "{kind}: {refused}");
        assert_eq!(refused["error"], format!("Logic Pro's bridge can't make a {kind} change yet: {why}"));
    }
    let (refused, _) = lab.try_preview("fx-bypass", json!({"ref": "1:plugin:1.1", "bypass": true})).await;
    assert!(refused["error"].as_str().unwrap().starts_with("the simulated Logic Pro makes only"), "{refused}");
    let (refused, _) = lab.try_preview("bogus", json!({})).await;
    assert_eq!(refused["error"], "Logic Pro has no change kind \"bogus\"");
    lab.simulator.stop();
}

#[tokio::test]
async fn a_dialog_stops_a_change_before_anything_is_written_and_the_transport_and_playhead_act() {
    let mut lab = Lab::start().await;
    lab.script.busy_once();
    let (busy, is_error) = lab.try_preview("track-mute", json!({"ref": "1:track:1", "mute": true})).await;
    assert!(
        is_error && busy["code"] == "busy" && busy["busy"] == true && busy["error"] == "a dialog is up in Logic Pro; nothing changed",
        "{busy}"
    );
    assert_eq!(lab.script.track(1)["mute"], json!(false));

    let (playing, is_error) = lab.tool("transport_play", json!({})).await;
    assert!(!is_error && playing == json!({"started": true, "playing": true}), "{playing}");
    let (focus, _) = lab.tool("focus", json!({})).await;
    assert_eq!(focus["transport"]["playing"], true);
    // The playhead, in beats or in seconds (160 BPM: a beat is 0.375 s), to the bar and beat.
    let (moved, is_error) = lab.tool("cursor_set", json!({"positionBeats": 8})).await;
    assert!(
        !is_error
            && moved == json!({"epoch": "1", "positionBeats": 8.0, "position": 3.0, "playhead": {"bar": 3, "beat": 1}, "playing": true}),
        "{moved}"
    );
    let (moved, _) = lab.tool("cursor_set", json!({"position": 6})).await;
    assert_eq!(
        (moved["positionBeats"].clone(), moved["playhead"].clone(), lab.script.playhead()),
        (json!(16.0), json!({"bar": 5, "beat": 1}), 16.0)
    );
    let (focus, _) = lab.tool("focus", json!({})).await;
    assert_eq!((focus["playhead"].clone(), focus["transport"]["beat"].clone()), (json!({"bar": 5, "beat": 1}), json!(16.0)));
    let (stopped, _) = lab.tool("transport_stop", json!({})).await;
    assert_eq!(stopped, json!({"stopped": true, "wasRecording": false}));
    let (stopped, _) = lab.tool("transport_stop", json!({})).await;
    assert_eq!(stopped["stopped"], false, "stopped already");

    // Released changes are forgotten.
    let (preview, _) = lab.make("track-solo", json!({"ref": "1:track:2", "solo": true}), "a1").await;
    let (released, is_error) = lab.tool("change_release", json!({"transactionIds": [preview["transactionId"], "t-unknown"]})).await;
    assert_eq!((released, is_error), (json!({"released": 1}), false));
    let (gone, is_error) = lab.undo(&preview, "u1", false).await;
    assert!(is_error && gone["code"] == "unknown", "{gone}");
    lab.simulator.stop();
}

/// Every change method once, over the wire between the host and the bridge: what Logic's bridge receives for a preview,
/// an apply, a restore, a plan's counted step, the transport and the playhead, and what it answers, as the golden
/// tests/golden/logic-changes-bridge.ndjson (the descs' nonces made the same in every run).
#[tokio::test]
async fn the_change_methods_on_the_wire() {
    let mut lab = Lab::start().await;
    let (volume, _) = lab.make("track-volume", json!({"ref": "1:track:1", "volumeDb": -6}), "a1").await;
    lab.undo(&volume, "u1", false).await;
    lab.script.busy_once();
    lab.try_preview("track-pan", json!({"ref": "1:track:1", "pan": -0.5})).await;
    lab.script.uncertain_once();
    let preview = lab.preview("track-arm", json!({"ref": "1:track:1", "arm": true})).await;
    lab.apply(&preview, "a2").await;
    lab.tool("undo_step_begin", json!({"label": "A plan"})).await;
    lab.make("track-mute", json!({"ref": "1:track:2", "mute": true}), "a3").await;
    lab.make("track-rename", json!({"ref": "1:track:2", "name": "Lead"}), "a4").await;
    lab.tool("undo_step_end", json!({"stepId": "s1", "label": "Keys muted and 1 more"})).await;
    let (add, _) = lab.make("track-add", json!({"name": "Pad", "kind": "instrument"}), "a5").await;
    // (the added track's rename is put back before the track is taken away: its undo wants it as the apply left it)
    let (pads, _) = lab.make("track-rename", json!({"ref": "3:track:3", "name": "Pads"}), "a6").await;
    lab.undo(&pads, "u2", false).await;
    lab.undo(&add, "u3", false).await;
    let (marker, _) = lab.make("marker-add", json!({"positionBeats": 16}), "a7").await;
    let (moved, _) = lab.make("marker-move", json!({"ref": "6:marker:1", "positionBeats": 20}), "a8").await;
    lab.undo(&moved, "u4", false).await;
    lab.undo(&marker, "u5", false).await;
    let (tempo, _) = lab.make("tempo-set", json!({"bpm": 128}), "a9").await;
    lab.undo(&tempo, "u6", false).await;
    lab.tool("transport_play", json!({})).await;
    lab.tool("cursor_set", json!({"positionBeats": 8})).await;
    lab.tool("transport_stop", json!({})).await;
    let (delete, _) = lab.make("track-delete", json!({"ref": "6:track:2"}), "a10").await;
    lab.undo(&delete, "u7", false).await;
    assert_eq!((lab.script.epoch(), lab.script.track_count()), (8, 2), "every undo went through");
    // (M4) The project saved where it is and as a new package; a key command with and without a step; a region split,
    // moved, resized, looped, renamed and deleted, the cycle, a tempo event made and one deleted, a signature, each
    // undone; a MIDI file imported and undone; a Track Stack made and flattened, each undone.
    lab.tool("project_save", json!({"epoch": "8"})).await;
    lab.tool("project_save", json!({"epoch": "8", "path": "/Users/me/Music/Logic/Scratch Kumi.logicx"})).await;
    lab.make("key-command", json!({"command": "Select All"}), "a11").await;
    let (created, _) = lab.make("key-command", json!({"command": "Create Marker"}), "a12").await;
    lab.undo(&created, "u8", false).await;
    let (split, _) = lab.make("region-split", json!({"ref": "8:region:1", "positionBeats": 8}), "a13").await;
    lab.undo(&split, "u9", false).await;
    let (moved, _) = lab.make("region-move", json!({"ref": "10:region:1", "positionBeats": 16}), "a14").await;
    lab.undo(&moved, "u10", false).await;
    let (resized, _) = lab.make("region-resize", json!({"ref": "12:region:1", "endBeats": 8}), "a15").await;
    lab.undo(&resized, "u11", false).await;
    let (looped, _) = lab.make("region-loop", json!({"ref": "14:region:1", "loop": true}), "a16").await;
    lab.undo(&looped, "u12", false).await;
    let (renamed, _) = lab.make("region-rename", json!({"ref": "14:region:1", "name": "Chords"}), "a17").await;
    lab.undo(&renamed, "u13", false).await;
    let (deleted, _) = lab.make("region-delete", json!({"ref": "16:region:1"}), "a18").await;
    lab.undo(&deleted, "u14", false).await;
    let (cycle, _) = lab.make("loop-range", json!({"startBeats": 16, "endBeats": 32}), "a19").await;
    lab.undo(&cycle, "u15", false).await;
    let (tempo, _) = lab.make("tempo-marker-set", json!({"positionBeats": 32, "bpm": 128}), "a20").await;
    let (gone, _) = lab.make("tempo-marker-delete", json!({"positionBeats": 32}), "a21").await;
    lab.undo(&gone, "u16", false).await;
    lab.undo(&tempo, "u17", false).await;
    let (signature, _) = lab.make("signature-set", json!({"positionBeats": 16, "timeSignature": [3, 4]}), "a22").await;
    lab.undo(&signature, "u18", false).await;
    let notes: Vec<Value> = (0..4).map(|n| json!({"pitch": 60 + n, "start": n as f64 * 0.25, "length": 0.25})).collect();
    let (imported, _) = lab.make("midi-import", json!({"ref": "18:track:2", "positionBeats": 32, "notes": notes}), "a23").await;
    lab.undo(&imported, "u19", false).await;
    let (stack, _) = lab.make("track-stack", json!({"refs": ["20:track:1", "20:track:2"], "kind": "summing"}), "a24").await;
    let (flatten, _) = lab.make("track-stack-flatten", json!({"ref": "21:track:1"}), "a25").await;
    lab.undo(&flatten, "u20", false).await;
    lab.undo(&stack, "u21", false).await;
    assert_eq!((lab.script.epoch(), lab.script.track_count(), lab.script.regions().len()), (24, 2, 1), "every M4 undo went through");
    let asked: Vec<String> = lab.script.asked().into_iter().map(|(method, _)| method).collect();
    for method in [
        "change_preview",
        "change_apply",
        "change_restore",
        "undo_step_begin",
        "undo_step_end",
        "transport_play",
        "cursor_set",
        "transport_stop",
        "project_save",
    ] {
        assert!(asked.iter().any(|m| m == method), "{method} never reached the bridge");
    }
    golden("logic-changes-bridge.ndjson", &without_nonces(&normalized(&lab.simulator.transcript())));
    lab.simulator.stop();
}

// ----- M4, the first slice -----

/// (M4) Saving is no change: the package written where it is (Cmd-S), or as a new package (File > Save As… through the
/// panel service), which the project is from then on; never as the project's own file, never over a file, and never
/// a version (REAPER's), each refused in the bridge's words; the epoch is checked as every call's is. Nothing is
/// journaled.
#[tokio::test]
async fn the_project_is_saved_where_it_is_or_as_a_new_package_and_never_versioned() {
    let mut lab = Lab::start().await;
    let (saved, is_error) = lab.tool("project_save", json!({"epoch": "1"})).await;
    assert!(!is_error, "{saved}");
    assert_eq!(saved, json!({"epoch": "1", "saved": "/Users/me/Music/Logic/Scratch.logicx", "dirty": false}));
    let (released, _) = lab.tool("change_release", json!({"transactionIds": ["t1"]})).await;
    assert_eq!(released["released"], 0, "nothing is kept for undo");
    for (arguments, words) in [
        (
            json!({"epoch": "1", "version": true}),
            "Logic Pro keeps no versions: give path (a .logicx that isn't there yet) to save a copy, or nothing to save in place",
        ),
        (
            json!({"epoch": "1", "path": "/Users/me/Music/Logic/Scratch.logicx"}),
            "that is the project's own file: save in place (no path), or give another path",
        ),
        (json!({"epoch": "1", "path": "/Users/me/Music/Logic/Scratch Kumi.txt"}), "path must name a .logicx package"),
        (json!({"epoch": "1", "path": "Scratch Kumi.logicx"}), "whole path"),
        (json!({"epoch": "1", "path": "/Users/me/Music/Logic/A:B.logicx"}), "path must end in a file name Logic Pro can save"),
        (json!({}), "epoch: the epoch the refs were read in"),
    ] {
        let (refused, is_error) = lab.tool("project_save", arguments.clone()).await;
        assert!(
            is_error && refused["code"] == "bad-params" && refused["error"].as_str().unwrap().contains(words),
            "{arguments}: {refused}"
        );
    }
    let (stale, is_error) = lab.tool("project_save", json!({"epoch": "0"})).await;
    assert!(is_error && stale["code"] == "stale", "{stale}");
    // Saved as: the project is that package from then on, its name and id with it.
    let copy = "/Users/me/Music/Logic/Scratch Kumi.logicx";
    let (saved, is_error) = lab.tool("project_save", json!({"epoch": "1", "path": copy})).await;
    assert!(!is_error && saved["saved"] == copy && saved["dirty"] == false, "{saved}");
    assert_eq!(lab.script.saved_as().as_deref(), Some(copy));
    let (project, _) = lab.tool("read", json!({"kind": "project"})).await;
    assert_eq!((project["items"][0]["path"].as_str(), project["items"][0]["identity"].as_str()), (Some(copy), Some("Scratch Kumi")));
    assert_ne!(project["items"][0]["id"], "e805ab87b3ca9d66f0f62d47b97caa29", "another package is another id");
    assert_eq!(lab.script.project().path.as_deref(), Some(copy));
    let (again, is_error) = lab.tool("project_save", json!({"epoch": "1", "path": copy})).await;
    assert!(is_error && again["error"].as_str().unwrap().contains("that is the project's own file"), "{again}");
    // Saved as another: the first copy is a file there now, never saved over.
    let other = "/Users/me/Music/Logic/Scratch Kumi 2.logicx";
    lab.tool("project_save", json!({"epoch": "1", "path": other})).await;
    let (again, is_error) = lab.tool("project_save", json!({"epoch": "1", "path": copy})).await;
    assert!(is_error && again["error"] == format!("{copy} is there already: Kumi never saves over a file"), "{again}");
    let (saved, _) = lab.tool("project_save", json!({"epoch": "1"})).await;
    assert_eq!(saved["saved"], other, "saved where it is now");
    lab.simulator.stop();
}

/// (M4) A key command runs as a change, as the bridge resolves it (by its English name or its id, from Logic's own
/// names and the factory U.S. bindings): one Logic keeps a step of (Create Marker; a split at the playhead) is a row
/// only Logic's own undo takes back; a selection command keeps none and makes no row (the apply says so); a command
/// that saves, moves Logic's own undo history, opens a dialog or has no key is refused, and a name Logic hasn't is
/// refused with the closest it has.
#[tokio::test]
async fn a_key_command_is_a_row_only_when_logic_keeps_a_step_of_it_and_only_logic_takes_it_back() {
    let mut lab = Lab::start().await;
    let preview = lab.preview("key-command", json!({"command": "Select All"})).await;
    assert_eq!(
        (preview["target"].clone(), preview["prior"].clone()),
        (json!({"kind": "action", "command": 766, "name": "Select All"}), json!({}))
    );
    assert_eq!(
        preview["display"],
        json!({"title": "Ran key command “Select All” (⌘A)", "note": "Only Logic Pro's own undo can take this back (Cmd-Z in Logic Pro). It acts on what the producer has selected now."})
    );
    let (applied, is_error) = lab.apply(&preview, "a1").await;
    assert!(!is_error && applied["state"] == "applied", "{applied}");
    assert_eq!(applied["applied"], json!({"command": 766, "name": "Select All", "key": "⌘A", "changed": false}));
    assert_eq!((applied["display"]["unchanged"].clone(), applied["undo"]["entry"].clone()), (json!(true), json!(false)));
    assert!(applied["display"]["note"].as_str().unwrap().contains("no row in HISTORY"), "{applied}");
    assert!(lab.script.undo_titles().is_empty(), "no step of Logic's undo");

    // Create Marker at the playhead, by its id: a step, and a marker Logic numbered; Kumi's undo of it is Logic's.
    lab.tool("cursor_set", json!({"positionBeats": 16})).await;
    let (created, applied) = lab.make("key-command", json!({"command": 513}), "a2").await;
    assert_eq!(created["display"]["title"], "Ran key command “Create Marker” (⌥')");
    assert_eq!(
        (applied["applied"].clone(), applied["undo"]["title"].clone()),
        (json!({"command": 513, "name": "Create Marker", "key": "⌥'", "step": "Create Marker"}), json!("Undo Create Marker"))
    );
    assert_eq!(lab.script.markers(), [("Marker 2".to_string(), 16.0), ("Marker 1".to_string(), 32.0)]);
    let (refused, is_error) = lab.undo(&created, "u1", false).await;
    assert!(is_error && refused["code"] == "daw-only", "{refused}");
    assert_eq!(refused["error"], "Kumi can't take back what a key command did: Logic Pro's own undo can (Cmd-Z in Logic Pro)");
    assert_eq!(lab.script.markers().len(), 2, "nothing changed");

    // A split at the playhead: every selected region (Select All selected them) the playhead is inside, split there (a
    // step, and the epoch moves); nothing when it is inside none. A name is matched whatever its case or an ellipsis,
    // an id as a number or as text.
    lab.tool("cursor_set", json!({"positionBeats": 8})).await;
    let (_, split) = lab.make("key-command", json!({"command": "split regions/events at playhead position…"}), "a3").await;
    assert_eq!(
        (split["applied"]["step"].clone(), split["undo"]["title"].clone(), split["epoch"].clone()),
        (json!("Split Regions"), json!("Undo Split Regions"), json!("2"))
    );
    assert_eq!(lab.script.regions().len(), 2);
    lab.tool("cursor_set", json!({"positionBeats": 40})).await;
    let (_, nothing) = lab.make("key-command", json!({"command": "574"}), "a4").await;
    assert_eq!((nothing["applied"]["changed"].clone(), nothing["undo"]["entry"].clone()), (json!(false), json!(false)));
    assert_eq!(lab.script.regions().len(), 2);

    // Refused before anything is sent to Logic, in the bridge's words: a save, Logic's own undo, a bounce through a
    // dialog, a command with no key, a name Logic hasn't (with the closest it has), an id out of range, an empty name;
    // and an id the simulated Logic doesn't model says so.
    for (command, words) in [
        (json!(754), "key command 754 saves the project: use project_save: Kumi won't run it as a change"),
        (json!("Undo"), "“Undo” moves Logic Pro's undo history under Kumi's HISTORY"),
        (
            json!("Bounce Tracks in Place"),
            "“Bounce Tracks in Place” imports, exports or bounces through a dialog, which Kumi never runs unattended",
        ),
        (json!("Set Left Locator by Playhead"), "isn't bound to any key in Logic Pro's key command set"),
        (json!("Save As…"), "Logic Pro has no key command named “Save As…”"),
        (
            json!("Split at Playhead"),
            "Logic Pro has no key command named “Split at Playhead”; close: “Split Regions/Events at Playhead Position”",
        ),
        (json!(9999), "command: an id is a whole number from 1 to 4999"),
        (json!(""), "command: a key command's name"),
        (json!(42), "the simulated Logic Pro knows only the key commands"),
    ] {
        let (refused, is_error) = lab.try_preview("key-command", json!({"command": command})).await;
        assert!(is_error && refused["code"] == "bad-params" && refused["error"].as_str().unwrap().contains(words), "{command}: {refused}");
    }
    lab.simulator.stop();
}

/// (M4) Regions by their place in the Tracks area, as the bridge makes them: moved in time (the epoch moves; a region's
/// state is its edges, name, track and the Tracks area's `looped`), resized to whole beats by Logic's own Set Start and
/// Set End to Playhead Position (the notes stay where they are in time), split on a beat (the right part's ref in
/// `created`; whole again only through Logic's own undo while that step is on top), looped in the Region inspector
/// (the rows never show it), renamed (the epoch moves) and deleted (back only through Logic's own undo while on top),
/// each undone, the refs carried by place.
#[tokio::test]
async fn regions_move_resize_split_loop_rename_and_delete_and_come_back_by_their_place() {
    let mut lab = Lab::start().await;
    let keys = |start: f64, end: f64| json!({"startBeats": start, "endBeats": end, "name": "Keys", "track": 2, "looped": false});
    let (page, _) = lab.tool("read", json!({"kind": "region"})).await;
    assert_eq!((page["total"].clone(), page["items"][0]["trackRef"].clone()), (json!(1), json!("1:track:2")));
    let (moved, applied) = lab.make("region-move", json!({"ref": "1:region:1", "positionBeats": 8}), "a1").await;
    assert_eq!((moved["prior"].clone(), moved["target"].clone()), (keys(0.0, 16.0), json!({"ref": "1:region:1", "kind": "region"})));
    assert_eq!((applied["epoch"].clone(), applied["applied"].clone()), (json!("2"), keys(8.0, 24.0)));
    assert_eq!(
        (applied["display"]["title"].clone(), applied["undo"]["title"].clone()),
        (json!("Moved region “Keys” to bar 3"), json!("Undo Value Change"))
    );
    let (notes, _) = lab.tool("read", json!({"kind": "note", "parent": "2:region:1"})).await;
    assert_eq!(notes["items"][0]["position"], position(3, 1), "the notes moved with it: {notes}");
    for (input, words) in [
        (
            json!({"ref": "2:region:1", "toRef": "2:track:1"}),
            "Logic Pro's Event List moves a region in time, not to another track: leave toRef out",
        ),
        (
            json!({"ref": "2:region:1", "position": 4.0}),
            "Logic Pro's bridge places regions by positionBeats (beats from the project's start), not seconds",
        ),
        (json!({"ref": "2:region:1"}), "positionBeats must be a number from 0: beats from the project's start"),
    ] {
        let (refused, _) = lab.try_preview("region-move", input).await;
        assert_eq!(refused["error"], words);
    }
    let (back, is_error) = lab.undo(&moved, "u1", false).await;
    assert!(!is_error && back["restored"] == keys(0.0, 16.0) && back["epoch"] == "3", "{back}");
    assert_eq!(back["display"]["title"], "region “Keys” moved back to bar 1");

    // Resized: the end at the playhead (Logic's step names it), put back by value; an edge off the beat is refused at
    // the apply, before anything is written; both edges, the start first, the notes staying where they are in time.
    let (resized, applied) = lab.make("region-resize", json!({"ref": "3:region:1", "endBeats": 8}), "a2").await;
    assert_eq!(
        (applied["epoch"].clone(), applied["applied"].clone(), applied["display"]["title"].clone(), applied["undo"]["title"].clone()),
        (json!("4"), keys(0.0, 8.0), json!("Trimmed region “Keys” to bar 1–bar 3"), json!("Undo Set End to Playhead Position"))
    );
    let (refused, _) = lab.try_preview("region-resize", json!({"ref": "4:region:1", "startBeats": 9})).await;
    assert_eq!(refused["error"], "the region must end after it starts");
    let (back, _) = lab.undo(&resized, "u2", false).await;
    assert_eq!((back["restored"].clone(), back["display"]["title"].clone()), (keys(0.0, 16.0), json!("region “Keys” back to its length")));
    let preview = lab.preview("region-resize", json!({"ref": "5:region:1", "endBeats": 12.5})).await;
    let (refused, is_error) = lab.apply(&preview, "a3").await;
    assert!(is_error && refused["error"] == "Logic Pro's playhead display places an edge on a beat: give whole beats", "{refused}");
    assert_eq!((lab.script.regions()[0].3, lab.script.epoch()), (16.0, 5), "nothing written");
    let (resized, applied) = lab.make("region-resize", json!({"ref": "5:region:1", "startBeats": 1, "endBeats": 12}), "a4").await;
    assert_eq!(
        (applied["applied"].clone(), applied["undo"]["title"].clone()),
        (keys(1.0, 12.0), json!("Undo Set End to Playhead Position"))
    );
    let titles = lab.script.undo_titles();
    assert_eq!(titles[titles.len() - 2..], ["Undo Set Start to Playhead Position", "Undo Set End to Playhead Position"]);
    let (notes, _) = lab.tool("read", json!({"kind": "note", "parent": "6:region:1"})).await;
    assert_eq!(notes["items"][1]["position"], position(1, 2), "the notes stay where they were: {notes}");
    lab.undo(&resized, "u3", false).await;

    // Split on a beat, at bar 1 beat 3: the right part is a new region with the notes from there on, which Logic leaves
    // selected; whole again through Logic's own undo while the split is on top.
    let (refused, _) = lab.try_preview("region-split", json!({"ref": "7:region:1", "positionBeats": 2.5})).await;
    assert_eq!(refused["error"], "Logic Pro's playhead display splits on a beat: give whole beats");
    let (split, applied) = lab.make("region-split", json!({"ref": "7:region:1", "positionBeats": 2}), "a5").await;
    assert_eq!(
        split["display"],
        json!({"title": "Split region “Keys” at bar 1 beat 3", "note": "Logic Pro's own undo puts the region back whole while the split is its latest step."})
    );
    assert_eq!(
        (applied["epoch"].clone(), applied["created"].clone(), applied["undo"]["title"].clone()),
        (json!("8"), json!({"ref": "8:region:2", "kind": "region"}), json!("Undo Split Regions"))
    );
    assert_eq!(applied["applied"], json!({"left": keys(0.0, 2.0), "right": keys(2.0, 16.0)}));
    assert_eq!((lab.script.notes_of(1), lab.script.notes_of(2)), (2, 2));
    let (page, _) = lab.tool("read", json!({"kind": "region"})).await;
    assert_eq!((page["items"][0]["selected"].clone(), page["items"][1]["selected"].clone()), (json!(false), json!(true)), "{page}");
    let (notes, _) = lab.tool("read", json!({"kind": "note", "parent": "8:region:2"})).await;
    assert_eq!((notes["total"].clone(), notes["items"][0]["position"].clone()), (json!(2), position(1, 3)));
    let (timeline, _) = lab.tool("focus_body", json!({"kind": "timeline"})).await;
    assert_eq!(timeline["lanes"][1]["regions"].as_array().map(Vec::len), Some(2), "{timeline}");
    let (refused, _) = lab.try_preview("region-split", json!({"ref": "8:region:1", "positionBeats": 2})).await;
    assert_eq!(refused["error"], "that's not inside the region: it runs from bar 1 to bar 1 beat 3");
    let (whole, is_error) = lab.undo(&split, "u4", false).await;
    assert!(!is_error && whole["restored"]["pressed"] == "Undo Split Regions", "{whole}");
    assert_eq!(whole["display"], json!({"title": "region “Keys” whole again", "note": "Put back with Logic Pro's own undo."}));
    assert_eq!((lab.script.regions().len(), lab.script.notes_of(1), lab.script.epoch()), (1, 4, 9));

    // Split again: with a marker made since, the split isn't Logic's latest step, and nothing else makes it whole;
    // with a part renamed, it isn't as the split left it.
    let (split, _) = lab.make("region-split", json!({"ref": "9:region:1", "positionBeats": 2}), "a6").await;
    lab.make("marker-add", json!({"positionBeats": 40}), "a7").await;
    let (kept, is_error) = lab.undo(&split, "u5", false).await;
    assert!(is_error && kept["error"].as_str().unwrap().contains("(its Edit menu says \"Undo Create Marker\")"), "{kept}");
    let (renamed, applied) = lab.make("region-rename", json!({"ref": "10:region:1", "name": "Chords"}), "a8").await;
    assert_eq!(
        (applied["display"]["title"].clone(), applied["undo"]["title"].clone(), applied["epoch"].clone()),
        (json!("Renamed region “Keys” → “Chords”"), json!("Undo Renaming"), json!("11"))
    );
    let (kept, is_error) = lab.undo(&split, "u6", false).await;
    assert!(is_error && kept["error"] == "one of the split's parts has been changed in Logic Pro since Kumi split it", "{kept}");
    assert_eq!(lab.script.regions().len(), 2, "still split");

    // Looped through the Region inspector (a step of Logic's undo), which the Tracks area's rows never show; unlooped,
    // and the name put back (the epoch moves).
    let (looped, applied) = lab.make("region-loop", json!({"ref": "11:region:1", "loop": true}), "a9").await;
    assert_eq!(
        (applied["applied"].clone(), applied["display"]["title"].clone(), applied["undo"]["title"].clone()),
        (json!({"loop": true}), json!("region “Chords” looped"), json!("Undo Loop Parameter Change"))
    );
    let (region, _) = lab.tool("read", json!({"ref": "11:region:1"})).await;
    assert_eq!(region["items"][0]["looped"], false, "{region}");
    let (back, _) = lab.undo(&looped, "u7", false).await;
    assert_eq!(
        (back["restored"].clone(), back["display"]["title"].clone()),
        (json!({"loop": false}), json!("region “Chords” not looped (put back)"))
    );
    let (back, _) = lab.undo(&renamed, "u8", false).await;
    assert_eq!(
        (back["restored"].clone(), back["display"]["title"].clone(), back["epoch"].clone()),
        (json!({"name": "Keys"}), json!("Region named “Keys” again"), json!("12"))
    );

    // Deleted: back through Logic's own undo while on top, its notes with it; with another step on top, Kumi can't.
    let (delete, applied) = lab.make("region-delete", json!({"ref": "12:region:1"}), "a10").await;
    assert_eq!(delete["display"]["note"], "Only Logic Pro's own undo brings a deleted region back, while the deletion is its latest step.");
    assert_eq!(
        (applied["epoch"].clone(), applied["applied"].clone(), applied["undo"]["title"].clone()),
        (json!("13"), json!({"deleted": true}), json!("Undo Delete"))
    );
    let (page, _) = lab.tool("read", json!({"kind": "region"})).await;
    assert_eq!(page["total"], 1);
    let (back, is_error) = lab.undo(&delete, "u9", false).await;
    assert!(!is_error && back["restored"] == json!({"ref": "14:region:1", "was": "12:region:1", "pressed": "Undo Delete"}), "{back}");
    assert_eq!((lab.script.regions().len(), lab.script.notes_of(1)), (2, 2));
    let (delete, _) = lab.make("region-delete", json!({"ref": "14:region:1"}), "a11").await;
    lab.make("track-rename", json!({"ref": "15:track:1", "name": "Vox"}), "a12").await;
    let (kept, is_error) = lab.undo(&delete, "u10", false).await;
    assert!(
        is_error && kept["code"] == "failed" && kept["error"].as_str().unwrap().contains("(its Edit menu says \"Undo Renaming\")"),
        "{kept}"
    );
    lab.simulator.stop();
}

/// (M4) Many notes at once: a MIDI file the bridge writes (named by its bytes, as the bridge names it), which Logic
/// imports onto a new software instrument track below the one named, as a region it names itself; the epoch moves,
/// the region's ref and its track's in `created`. Taken away again, track and all, only through Logic's own undo while
/// that step is on top.
#[tokio::test]
async fn many_notes_go_in_as_a_midi_file_and_come_out_again() {
    let mut lab = Lab::start().await;
    for (note, words) in [
        (json!({"pitch": 200, "start": 0, "length": 1}), "pitch must be a whole number from 0 to 127"),
        (json!({"start": 0, "length": 1}), "every note needs a pitch (0 to 127)"),
        (json!({"pitch": 60, "start": 0, "length": 0}), "every note needs a length above 0 (beats)"),
    ] {
        let (refused, _) = lab.try_preview("midi-import", json!({"ref": "1:track:2", "positionBeats": 32, "notes": [note]})).await;
        assert_eq!(refused["error"], words);
    }
    let notes: Vec<Value> =
        (0..64).map(|n| json!({"pitch": 48 + n % 12, "start": n as f64 * 0.25, "length": 0.25, "velocity": 96})).collect();
    let (import, applied) = lab.make("midi-import", json!({"ref": "1:track:2", "positionBeats": 32, "notes": notes}), "a1").await;
    assert_eq!(
        (import["prior"].clone(), import["target"].clone()),
        (json!({"tracks": 2, "regions": 1}), json!({"ref": "1:track:2", "kind": "track"}))
    );
    assert_eq!(
        import["display"],
        json!({"title": "Imported 64 notes on “Keys” at bar 9", "notes": 64, "note": "Logic Pro's own undo takes an import back (\"Import\"), while it is its latest step."})
    );
    assert_eq!(
        (applied["epoch"].clone(), applied["created"].clone(), applied["undo"]["title"].clone()),
        (json!("2"), json!({"ref": "2:region:2", "kind": "region", "track": "2:track:3", "trackAdded": true}), json!("Undo Import"))
    );
    let file = applied["applied"]["file"].as_str().unwrap().to_string();
    assert!(file.starts_with("/tmp/kumi-logic/kumi-") && file.ends_with(".mid"), "{file}");
    assert_eq!(
        applied["applied"],
        json!({"notes": 64, "startBeats": 32.0, "endBeats": 48.0, "track": 3, "trackName": "Studio Grand", "file": file, "answered": []})
    );
    assert_eq!(applied["display"], json!({"title": "Imported 64 notes on “Studio Grand” at bar 9", "notes": 64}));
    let (tracks, _) = lab.tool("read", json!({"kind": "track"})).await;
    assert_eq!((tracks["items"][2]["name"].clone(), tracks["items"][2]["kind"].clone()), (json!("Studio Grand"), json!("instrument")));
    let (regions, _) = lab.tool("read", json!({"kind": "region"})).await;
    let new = &regions["items"][1];
    assert_eq!(
        (new["name"].clone(), new["trackRef"].clone(), new["selected"].clone(), regions["items"][0]["selected"].clone()),
        (json!("MIDI Region"), json!("2:track:3"), json!(true), json!(false))
    );
    let (notes_page, _) = lab.tool("read", json!({"kind": "note", "parent": "2:region:2", "limit": 200})).await;
    assert_eq!(
        (notes_page["total"].clone(), notes_page["items"][1]["position"].clone()),
        (json!(64), json!({"bar": 9, "beat": 1, "division": 2, "tick": 1}))
    );
    let (gone, is_error) = lab.undo(&import, "u1", false).await;
    assert!(!is_error && gone["restored"] == json!({"deleted": "2:region:2", "pressed": "Undo Import"}), "{gone}");
    assert_eq!(gone["display"], json!({"title": "Import taken back", "note": "Put back with Logic Pro's own undo."}));
    assert_eq!((lab.script.regions().len(), lab.script.track_count(), lab.script.epoch()), (1, 2, 3), "the track went with the region");

    // Four notes as the M4 gate imported them, onto the audio track: the file the bridge recorded (its name is its
    // bytes'), the region on the beat (the preview says the division and tick go), the new track below the one named.
    let gate = json!([
        {"pitch": 60, "start": 0, "length": 1}, {"pitch": 64, "start": 1, "length": 1},
        {"pitch": 67, "start": 2, "length": 1}, {"pitch": 72, "start": 3, "length": 1, "velocity": 110}
    ]);
    let (import, applied) = lab.make("midi-import", json!({"ref": "3:track:1", "positionBeats": 32.5, "notes": gate}), "a2").await;
    assert_eq!(import["display"]["note"], "Logic Pro's playhead display places the region on a beat: the division and tick are dropped.");
    assert_eq!(
        (
            applied["applied"]["file"].clone(),
            applied["applied"]["startBeats"].clone(),
            applied["applied"]["endBeats"].clone(),
            applied["applied"]["track"].clone()
        ),
        (json!("/tmp/kumi-logic/kumi-05514ac43b39.mid"), json!(32.0), json!(36.0), json!(2))
    );
    // With a marker made since (a step on top), Kumi can't take the import back: Logic's own undo can, step by step.
    lab.make("marker-add", json!({"positionBeats": 16}), "a3").await;
    let (kept, is_error) = lab.undo(&import, "u2", false).await;
    assert!(is_error && kept["error"].as_str().unwrap().contains("(its Edit menu says \"Undo Create Marker\")"), "{kept}");
    assert_eq!((lab.script.regions().len(), lab.script.track_count()), (2, 3));
    lab.simulator.stop();
}

/// (M4) The cycle (no step of Logic's undo; an off cycle keeps its locators), the Tempo and Signature Lists (a tempo
/// event made is two steps of Logic's undo), and Track Stacks (Logic names a stack and gathers its tracks; its own undo
/// takes a stack apart while on top, else it is flattened by value; a flattened one comes back only through Logic's own
/// undo): each read back and put back.
#[tokio::test]
async fn the_cycle_the_tempo_and_signature_lists_and_track_stacks_change_and_come_back() {
    let mut lab = Lab::start().await;
    let off = |start: f64, end: f64| json!({"on": false, "startBeats": start, "endBeats": end});
    let (cycle, applied) = lab.make("loop-range", json!({"startBeats": 16, "endBeats": 32}), "a1").await;
    assert_eq!((cycle["prior"].clone(), cycle["target"].clone()), (off(0.0, 16.0), json!({"ref": null, "kind": "project"})));
    assert_eq!(
        (applied["applied"].clone(), applied["display"]["title"].clone(), applied["undo"]["entry"].clone()),
        (json!({"on": true, "startBeats": 16.0, "endBeats": 32.0}), json!("Cycle bar 5 to bar 9"), json!(false))
    );
    let (focus, _) = lab.tool("focus", json!({})).await;
    let (timeline, _) = lab.tool("focus_body", json!({"kind": "timeline"})).await;
    assert_eq!((focus["transport"]["cycle"].clone(), timeline.get("cycle")), (json!(true), None), "the timeline says nothing of the cycle");
    let (cleared, applied) = lab.make("loop-range", json!({"startBeats": 0, "endBeats": 0}), "a2").await;
    assert_eq!((applied["applied"].clone(), applied["display"]["title"].clone()), (off(16.0, 32.0), json!("Cycle off")));
    for (input, words) in [
        (json!({"startBeats": 8, "endBeats": 4}), "the cycle must end after it starts (0 and 0 turn it off)"),
        (
            json!({"start": 1.0, "end": 2.0}),
            "Logic Pro's bridge places the cycle by startBeats and endBeats (beats from the project's start), not seconds",
        ),
        (json!({"endBeats": 4}), "pass startBeats (beats from the project's start) and endBeats; 0 and 0 turn the cycle off"),
    ] {
        let (refused, _) = lab.try_preview("loop-range", input).await;
        assert_eq!(refused["error"], words);
    }
    let preview = lab.preview("loop-range", json!({"startBeats": 16.0001, "endBeats": 32})).await;
    assert_eq!(preview["display"]["note"], "The locators sit on Logic Pro's ticks (960 to a beat): the range is rounded to them.");
    lab.undo(&cleared, "u1", false).await;
    assert_eq!(lab.script.cycle(), (true, 16.0, 32.0));
    let (back, _) = lab.undo(&cycle, "u2", false).await;
    assert_eq!((back["restored"].clone(), back["display"]["title"].clone()), (off(0.0, 16.0), json!("Cycle off (put back)")));
    assert_eq!(lab.script.cycle(), (false, 0.0, 16.0));
    assert!(lab.script.undo_titles().is_empty(), "the cycle makes no step");

    // The Tempo List: an event made (Insert Tempo, then its Tempo cell's Value Change), changed to a fraction of a BPM
    // and deleted, each put back; the project's first tempo changed, never deleted. The tempo row lists no events.
    let (tempo, applied) = lab.make("tempo-marker-set", json!({"positionBeats": 32, "bpm": 128}), "a3").await;
    assert_eq!(
        (tempo["prior"].clone(), tempo["target"].clone()),
        (json!({"exists": false}), json!({"ref": null, "kind": "tempo-marker", "positionBeats": 32.0}))
    );
    assert_eq!(
        (applied["applied"].clone(), applied["display"]["title"].clone(), applied["undo"]["title"].clone()),
        (json!({"exists": true, "bpm": 128.0, "made": true}), json!("Tempo 128 BPM at bar 9"), json!("Undo Value Change"))
    );
    assert_eq!(lab.script.undo_titles(), ["Undo Insert Tempo", "Undo Value Change"]);
    let (read, _) = lab.tool("read", json!({"kind": "tempo"})).await;
    assert!(read["items"][0].get("tempos").is_none(), "{read}");
    let (changed, applied) = lab.make("tempo-marker-set", json!({"positionBeats": 32, "bpm": 100.5}), "a4").await;
    assert_eq!(
        (applied["applied"].clone(), applied["display"]["title"].clone(), applied["undo"]["title"].clone()),
        (json!({"exists": true, "bpm": 100.5, "made": false}), json!("Tempo 100.5 BPM at bar 9"), json!("Undo Value Change"))
    );
    let (delete, applied) = lab.make("tempo-marker-delete", json!({"positionBeats": 32}), "a5").await;
    assert_eq!(
        (delete["display"]["title"].clone(), applied["applied"].clone(), applied["undo"]["title"].clone()),
        (json!("Deleted tempo event at bar 9"), json!({"deleted": true}), json!("Undo Delete"))
    );
    for (position, code, words) in [
        (0, "bad-params", "the project's first tempo event stays: change it with tempo_marker_set"),
        (48, "failed", "there is no tempo event at bar 13"),
    ] {
        let (refused, _) = lab.try_preview("tempo-marker-delete", json!({"positionBeats": position})).await;
        assert_eq!((refused["code"].clone(), refused["error"].clone()), (json!(code), json!(words)));
    }
    let (back, _) = lab.undo(&delete, "u3", false).await;
    assert_eq!(
        (back["restored"].clone(), back["display"].clone()),
        (
            json!({"exists": true, "bpm": 100.5}),
            json!({"title": "Tempo event at bar 9 put back", "note": "Put back with Logic Pro's own undo."})
        )
    );
    let (back, _) = lab.undo(&changed, "u4", false).await;
    assert_eq!(
        (back["restored"].clone(), back["display"]["title"].clone()),
        (json!({"exists": true, "bpm": 128.0}), json!("Tempo at bar 9 put back"))
    );
    let (back, _) = lab.undo(&tempo, "u5", false).await;
    assert_eq!(
        (back["restored"].clone(), back["display"]["title"].clone()),
        (json!({"exists": false}), json!("Tempo event at bar 9 taken away again"))
    );
    assert_eq!(lab.script.tempos(), [(0.0, 160.0)]);
    let (_, applied) = lab.make("tempo-marker-set", json!({"positionBeats": 0, "bpm": 150}), "a6").await;
    assert_eq!(
        (applied["applied"]["made"].clone(), applied["undo"]["title"].clone(), lab.script.tempo()),
        (json!(false), json!("Undo Value Change"), 150.0)
    );

    // The Signature List: a signature made at bar 5, and the project's own at the start (as {num, den}), each a step its
    // Edit menu names "Value Change"; a signature changes on a bar.
    let (signature, applied) = lab.make("signature-set", json!({"positionBeats": 16, "timeSignature": [3, 4]}), "a7").await;
    assert_eq!(
        (signature["prior"].clone(), signature["target"].clone()),
        (json!({"exists": false}), json!({"ref": null, "kind": "signature", "positionBeats": 16.0}))
    );
    assert_eq!(
        (applied["applied"].clone(), applied["display"]["title"].clone(), applied["undo"]["title"].clone()),
        (json!({"exists": true, "timeSignature": [3, 4], "made": true}), json!("Time signature 3/4 at bar 5"), json!("Undo Value Change"))
    );
    let (read, _) = lab.tool("read", json!({"kind": "tempo"})).await;
    assert_eq!(
        (read["items"][0]["signatures"].as_array().map(Vec::len), read["items"][0]["signatures"][1]["time"].clone()),
        (Some(2), json!("3/4"))
    );
    let (start, applied) = lab.make("signature-set", json!({"positionBeats": 0, "timeSignature": {"num": 6, "den": 8}}), "a8").await;
    assert_eq!((applied["applied"]["made"].clone(), applied["undo"]["title"].clone()), (json!(false), json!("Undo Value Change")));
    let (project, _) = lab.tool("read", json!({"kind": "project"})).await;
    let (focus, _) = lab.tool("focus", json!({})).await;
    assert_eq!((project["items"][0]["timeSignature"].clone(), focus["transport"]["beatsPerBar"].clone()), (json!("6/8"), json!(6)));
    for (input, words) in [
        (
            json!({"positionBeats": 16, "timeSignature": [3, 5]}),
            "timeSignature must be [numerator, denominator]: 1 to 99 over 1, 2, 4, 8, 16 or 32",
        ),
        (json!({"positionBeats": 18, "timeSignature": [3, 4]}), "a time signature changes at a bar: give the bar's first beat"),
    ] {
        let (refused, _) = lab.try_preview("signature-set", input).await;
        assert_eq!(refused["error"], words);
    }
    let (back, _) = lab.undo(&start, "u6", false).await;
    assert_eq!(back["display"]["title"], "Time signature at bar 1 put back");
    let (back, _) = lab.undo(&signature, "u7", false).await;
    assert_eq!(
        (back["restored"].clone(), back["display"]["title"].clone()),
        (json!({"exists": false}), json!("Time signature at bar 5 taken away again"))
    );
    assert_eq!(lab.script.signatures(), [(0.0, (4, 4))]);

    // A summing stack of both tracks: Logic names it Sum 1, its own track an aux fed by Bus 1 where the first track was,
    // their outputs on that bus (the epoch moves); taken apart through Logic's own undo while that step is on top.
    let (stack, applied) = lab.make("track-stack", json!({"refs": ["1:track:1", "1:track:2"], "kind": "summing"}), "a9").await;
    assert_eq!(
        (stack["prior"].clone(), stack["target"].clone(), stack["display"].clone()),
        (
            json!({"tracks": 2}),
            json!({"ref": null, "kind": "track"}),
            json!({"title": "Summing stack of “Vocal” and “Keys”", "kind": "summing"})
        )
    );
    assert_eq!(
        (applied["epoch"].clone(), applied["created"].clone(), applied["undo"]["title"].clone()),
        (json!("2"), json!({"ref": "2:track:1", "kind": "track", "stack": "summing"}), json!("Undo Create Track Stack"))
    );
    assert_eq!(applied["applied"], json!({"index": 1, "name": "Sum 1", "kind": "summing", "tracks": ["2:track:2", "2:track:3"]}));
    assert_eq!(applied["display"]["title"], "Summing stack “Sum 1” of “Vocal” and “Keys”");
    let (page, _) = lab.tool("read", json!({"kind": "track"})).await;
    let rows = page["items"].as_array().unwrap().clone();
    assert_eq!(
        (rows[0]["kind"].clone(), rows[0]["stack"].clone(), rows[0]["inputs"].clone(), rows[0]["output"].clone()),
        (json!("aux"), json!(true), json!(["Bus 1"]), json!("Stereo Output"))
    );
    assert_eq!((rows[1].get("stack"), rows[1]["output"].clone(), rows[2]["output"].clone()), (None, json!("Bus 1"), json!("Bus 1")));
    let (refused, _) = lab.try_preview("track-stack", json!({"refs": ["2:track:1"], "kind": "folder"})).await;
    assert_eq!(refused["error"], "track 1 is a Track Stack already; Logic Pro doesn't stack stacks from outside");
    let (gone, is_error) = lab.undo(&stack, "u8", false).await;
    assert!(!is_error && gone["restored"] == json!({"tracks": 2, "pressed": "Undo Create Track Stack"}), "{gone}");
    assert_eq!(gone["display"], json!({"title": "Stack “Sum 1” taken apart again", "note": "Put back with Logic Pro's own undo."}));

    // A folder stack of one track, named: renamed once made, so "Renaming" is on top of its creation, and its undo
    // flattens it by value.
    let (folder, applied) = lab.make("track-stack", json!({"refs": ["3:track:2"], "kind": "folder", "name": "Keys Folder"}), "a10").await;
    assert_eq!(folder["display"], json!({"title": "Folder stack of “Keys”", "kind": "folder", "name": "Keys Folder"}));
    assert_eq!(
        (applied["applied"].clone(), applied["undo"]["title"].clone()),
        (json!({"index": 2, "name": "Keys Folder", "kind": "folder", "tracks": ["4:track:3"]}), json!("Undo Renaming"))
    );
    let titles = lab.script.undo_titles();
    assert_eq!(titles[titles.len() - 2..], ["Undo Create Track Stack", "Undo Renaming"]);
    let row = lab.script.track(2);
    assert_eq!(
        (row["name"].clone(), row["kind"].clone(), row["stack"].clone(), row["output"].clone()),
        (json!("Keys Folder"), json!("external"), json!(true), json!(""))
    );
    let (flat, is_error) = lab.undo(&folder, "u9", false).await;
    assert!(
        !is_error && flat["restored"] == json!({"tracks": 2}) && flat["display"]["title"] == "Stack “Keys Folder” flattened again",
        "{flat}"
    );
    assert_eq!(lab.script.undo_titles().last().map(String::as_str), Some("Undo Flatten Stack"));

    // Tracks not next to each other: Logic gathers them under the stack, where the first of them was; its own undo puts
    // them back where they were.
    let names =
        |lab: &Lab| (1..=lab.script.track_count()).map(|at| lab.script.track(at)["name"].as_str().unwrap().to_string()).collect::<Vec<_>>();
    lab.make("track-add", json!({"name": "Pad"}), "a11").await;
    let (gathered, applied) = lab.make("track-stack", json!({"refs": ["6:track:3", "6:track:1"], "kind": "summing"}), "a12").await;
    assert_eq!(applied["applied"], json!({"index": 1, "name": "Sum 2", "kind": "summing", "tracks": ["7:track:2", "7:track:3"]}));
    assert_eq!(names(&lab), ["Sum 2", "Vocal", "Pad", "Keys"]);
    lab.undo(&gathered, "u10", false).await;
    assert_eq!(names(&lab), ["Vocal", "Keys", "Pad"]);

    // A stack flattened (its tracks stay where they are) comes back through Logic's own undo while that step is on top,
    // which leaves the stack's creation on top again: Logic's own undo takes the stack apart too.
    let (stack, _) = lab.make("track-stack", json!({"refs": ["8:track:1", "8:track:2"], "kind": "summing"}), "a13").await;
    let (refused, _) = lab.try_preview("track-stack-flatten", json!({"ref": "9:track:2"})).await;
    assert_eq!(refused["error"], "track 2 “Vocal” isn't a Track Stack");
    let (flatten, applied) = lab.make("track-stack-flatten", json!({"ref": "9:track:1"}), "a14").await;
    assert_eq!(
        (flatten["prior"].clone(), flatten["display"]["title"].clone()),
        (json!({"name": "Sum 3", "index": 1, "tracks": 4}), json!("Stack “Sum 3” flattened"))
    );
    assert_eq!(
        (applied["applied"].clone(), applied["undo"]["title"].clone(), lab.script.track_count()),
        (json!({"name": "Sum 3", "tracks": 3}), json!("Undo Flatten Stack"), 3)
    );
    let (back, is_error) = lab.undo(&flatten, "u11", false).await;
    assert!(!is_error && back["restored"] == json!({"tracks": 4, "pressed": "Undo Flatten Stack", "ref": "11:track:1"}), "{back}");
    assert_eq!(back["display"], json!({"title": "Stack “Sum 3” back", "note": "Put back with Logic Pro's own undo."}));
    let (gone, is_error) = lab.undo(&stack, "u12", false).await;
    assert!(!is_error && gone["restored"] == json!({"tracks": 3, "pressed": "Undo Create Track Stack"}), "{gone}");
    assert_eq!(names(&lab), ["Vocal", "Keys", "Pad"]);

    // With a step on top: a stack Kumi made is flattened by value instead, and a flattened one stays flat.
    let (stack, _) = lab.make("track-stack", json!({"refs": ["12:track:1", "12:track:2"], "kind": "folder"}), "a15").await;
    lab.make("marker-add", json!({"positionBeats": 16}), "a16").await;
    let (flat, is_error) = lab.undo(&stack, "u13", false).await;
    assert!(
        !is_error && flat["restored"] == json!({"tracks": 3}) && flat["display"]["title"] == "Stack “Folder 2” flattened again",
        "{flat}"
    );
    lab.make("track-stack", json!({"refs": ["14:track:1", "14:track:2"], "kind": "summing"}), "a17").await;
    let (flatten, _) = lab.make("track-stack-flatten", json!({"ref": "15:track:1"}), "a18").await;
    lab.make("marker-add", json!({"positionBeats": 20}), "a19").await;
    let (kept, is_error) = lab.undo(&flatten, "u14", false).await;
    assert!(is_error && kept["error"].as_str().unwrap().contains("(its Edit menu says \"Undo Create Marker\")"), "{kept}");
    assert_eq!(lab.script.track_count(), 3);
    lab.simulator.stop();
}

/// (M4) REAPER's simulator's switch in Logic's: the next restore alone is made and never answered, so the host keeps
/// the change uncertain and never sends the undo again.
#[tokio::test]
async fn an_undo_logic_doesnt_answer_after_with_the_restore_switch_is_never_sent_twice() {
    let mut lab = Lab::start().await;
    let (preview, _) = lab.make("track-volume", json!({"ref": "1:track:1", "volumeDb": -6}), "a1").await;
    lab.script.uncertain_restore_once();
    let (unsure, is_error) = lab.undo(&preview, "u1", false).await;
    assert!(is_error && unsure["code"] == "undo-uncertain" && unsure["answered"] == true, "{unsure}");
    assert_eq!(lab.script.volume(1), 0.0, "Logic put the fader back");
    let restores = lab.script.asked().iter().filter(|(method, _)| method == "change_restore").count();
    let (again, is_error) = lab.undo(&preview, "u2", false).await;
    assert!(is_error && again["code"] == "undo-uncertain", "{again}");
    assert_eq!(lab.script.asked().iter().filter(|(method, _)| method == "change_restore").count(), restores, "nothing sent twice");
    // The switch is spent: the next change and its undo go through.
    let (pan, _) = lab.make("track-pan", json!({"ref": "1:track:1", "pan": 0.5}), "a2").await;
    let (undone, is_error) = lab.undo(&pan, "u3", false).await;
    assert!(!is_error && undone["state"] == "undone", "{undone}");
    lab.simulator.stop();
}

/// (M4) The producer's Cmd-Z on a step one of the new kinds made (a split) is named back by the bridge through the
/// desc it kept, as a rename's is: the host names the change, and Kumi's HISTORY follows.
#[tokio::test]
async fn a_cmd_z_on_a_split_is_named_back_to_the_host() {
    let mut lab = Lab::start().await;
    let (_, split) = lab.make("region-split", json!({"ref": "1:region:1", "positionBeats": 8}), "a1").await;
    let (_, moved) = lab.make("marker-move", json!({"ref": "2:marker:1", "positionBeats": 40}), "a2").await;
    assert_eq!(lab.script.undo_titles(), ["Undo Split Regions", "Undo Value Change"]);
    // Two presses of Cmd-Z: the marker's move, then the split, each named by its desc.
    for (expected_title, change) in [("Undo Value Change", &moved), ("Undo Split Regions", &split)] {
        let (title, desc) = lab.script.undo_last().expect("a step");
        assert_eq!(title, expected_title);
        lab.simulator
            .emit("undone_in_daw", json!({"descs": [desc.clone().unwrap()], "how": "undo", "epoch": lab.script.epoch().to_string()}));
        let named = lab.undone_in_daw().await;
        assert_eq!(
            (named["transactionIds"].clone(), named["undone"].clone()),
            (json!([change["transactionId"]]), json!([change["transactionId"]]))
        );
    }
    // (no tool is asked after an event: the session's line reader would take the event's notification for the answer)
    lab.simulator.stop();
}
