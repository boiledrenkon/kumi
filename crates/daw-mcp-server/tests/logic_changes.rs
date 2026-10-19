//! The host's change tools (M3 contract §3a) against the simulated Logic Pro: preview, apply and undo through the
//! journal in Logic's words, each change one step of Logic's own undo (it has no undo block: a plan's step is counted),
//! a track added moving the epoch with a restore's ref carried past it, a write Logic doesn't show back left uncertain
//! and never sent twice (an apply's and an undo's), the transport and the playhead, and what the bridge receives for
//! every method, as a golden of the wire (tests/golden/logic-changes-bridge.ndjson).
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
        let epoch = self.script.epoch().to_string();
        let (preview, is_error) = self.tool("change_preview", json!({"kind": kind, "input": input, "epoch": epoch})).await;
        assert!(!is_error, "{kind}: {preview}");
        preview
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

#[tokio::test]
async fn a_change_is_previewed_applied_and_undone_each_as_one_step_of_logics_undo() {
    let mut lab = Lab::start().await;
    let preview = lab.preview("track-volume", json!({"ref": "1:track:1", "volumeDb": -6})).await;
    assert_eq!(
        (&preview["prior"], &preview["target"]["ref"], &preview["epoch"]),
        (&json!({"volumeDb": 0.0}), &json!("1:track:1"), &json!("1"))
    );
    assert_eq!(preview["display"]["title"], "Vocal volume -6 dB");
    assert_eq!(lab.script.volume(1), 0.0, "a preview changes nothing");

    let (wrong, is_error) =
        lab.tool("change_apply", json!({"transactionId": preview["transactionId"], "confirmation": "c0000", "idempotencyKey": "a1"})).await;
    assert!(is_error && wrong["code"] == "confirmation", "{wrong}");

    let (applied, is_error) = lab.apply(&preview, "a1").await;
    assert!(!is_error && applied["state"] == "applied", "{applied}");
    assert_eq!((applied["applied"].clone(), applied["undo"]["entry"].clone()), (json!({"volumeDb": -6.0}), json!(true)));
    assert_eq!(applied["undo"]["title"], "Undo Change Volume", "the Edit menu's title at apply time");
    assert_eq!(lab.script.volume(1), -6.0);
    let desc = applied["undo"]["desc"].as_str().unwrap().to_string();
    assert!(desc.starts_with("Kumi: Vocal volume -6 dB (k") && desc.ends_with(')'), "{desc}");
    // The same key again: the same answer, and Logic changed once.
    assert_eq!(lab.apply(&preview, "a1").await, (applied.clone(), false));
    assert_eq!(lab.script.undo_history(), [desc.clone()]);
    // The value shows in a read after the change.
    let (row, _) = lab.tool("read", json!({"ref": "1:track:1"})).await;
    assert_eq!(row["items"][0]["volumeDb"], -6.0, "{row}");

    let (undone, is_error) = lab.undo(&preview, "u1", false).await;
    assert!(!is_error && undone["state"] == "undone", "{undone}");
    assert_eq!(undone["restored"], json!({"volumeDb": 0.0}));
    assert_eq!(lab.script.volume(1), 0.0);
    let history = lab.script.undo_history();
    assert_eq!(history.len(), 2, "the undo is a new step, never Logic's own undo");
    assert!(history[1].starts_with("Kumi: undo Vocal volume -6 dB (k") && history[1] != history[0], "{history:?}");
    assert_eq!(lab.script.undo_titles(), ["Undo Change Volume", "Undo Change Volume"]);
    assert_eq!(lab.undo(&preview, "u1", false).await, (undone, false));
    let (again, is_error) = lab.undo(&preview, "u2", false).await;
    assert!(is_error && again["code"] == "undone", "{again}");
    lab.simulator.stop();
}

/// Logic has no undo block: a plan's step is counted, each change in it is a step of Logic's own undo, and the
/// producer's Cmd-Z takes the last one back, which the bridge names to the host by the desc it kept.
#[tokio::test]
async fn a_plan_is_as_many_steps_of_logics_undo_as_it_has_changes_and_cmd_z_takes_the_last_back() {
    let mut lab = Lab::start().await;
    let (begun, is_error) = lab.tool("undo_step_begin", json!({"label": "Tighten the keys"})).await;
    assert!(!is_error && begun["stepId"] == "s1", "{begun}");
    let (_, muted) = lab.make("track-mute", json!({"ref": "1:track:2", "mute": true}), "a1").await;
    let (_, renamed) = lab.make("track-rename", json!({"ref": "1:track:2", "name": "Lead"}), "a2").await;
    assert_eq!(renamed["display"]["title"], "Renamed track “Keys” → “Lead”");
    assert_eq!((lab.script.track(2)["mute"].clone(), lab.script.track(2)["name"].clone()), (json!(true), json!("Lead")));
    let history = lab.script.undo_history();
    assert_eq!(history.len(), 2, "each change its own step: {history:?}");
    assert!(
        history[0].starts_with("Kumi: Keys muted (k") && history[1].starts_with("Kumi: Renamed track “Keys” → “Lead” (k"),
        "{history:?}"
    );
    let (ended, is_error) = lab.tool("undo_step_end", json!({"stepId": "s1", "label": "Keys muted and 1 more"})).await;
    assert!(!is_error, "{ended}");
    assert!(ended["desc"].as_str().unwrap().starts_with("Kumi: Keys muted and 1 more (k"), "{ended}");
    assert_eq!((ended["entry"].clone(), ended["changes"].clone()), (json!(false), json!(2)), "no step of Logic's undo for the plan");
    assert_eq!(lab.script.undo_history().len(), 2);

    // The producer presses Cmd-Z: the rename comes off, and the bridge names it by its desc; the host names the change.
    let (title, desc) = lab.script.undo_last().expect("a step");
    assert_eq!(title, "Undo Rename Track");
    lab.simulator.emit("undone_in_daw", json!({"descs": [desc.clone().unwrap()], "how": "undo", "epoch": "1"}));
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

/// A track added or deleted moves Logic's epoch (a header appeared or vanished): a change previewed before is refused
/// as stale, the reads carry the new epoch, and a change made before is still undone, its ref carried to where its
/// track is now. A deleted track comes back by Logic's own undo only while that step is on top.
#[tokio::test]
async fn a_track_added_moves_the_epoch_and_changes_before_it_are_still_undone_by_their_place() {
    let mut lab = Lab::start().await;
    let (volume, _) = lab.make("track-volume", json!({"ref": "1:track:1", "volumeDb": -6}), "a1").await;
    let (rename, _) = lab.make("track-rename", json!({"ref": "1:track:2", "name": "Lead"}), "a2").await;
    let stale = lab.preview("track-mute", json!({"ref": "1:track:2", "mute": true})).await;

    let (add, added) = lab.make("track-add", json!({"name": "Pad"}), "a3").await;
    assert_eq!((add["prior"].clone(), add["epoch"].clone()), (json!({"tracks": 2}), json!("1")));
    assert_eq!((added["epoch"].clone(), added["created"].clone()), (json!("2"), json!({"ref": "2:track:3", "kind": "track"})));
    assert_eq!(added["undo"]["title"], "Undo Create Track");
    assert_eq!(lab.script.track_count(), 3);
    let (page, _) = lab.tool("read", json!({"kind": "track"})).await;
    assert_eq!((page["epoch"].clone(), page["total"].clone(), page["items"][2]["name"].clone()), (json!("2"), json!(3), json!("Pad")));
    // Previewed before the add: refused as stale, nothing written.
    let (refused, is_error) = lab.apply(&stale, "a4").await;
    assert!(is_error && refused["code"] == "stale", "{refused}");
    assert_eq!(lab.script.track(2)["mute"], json!(false));

    // Each undone as a new step, the ones from before the add by the place their track had then.
    let (undone, is_error) = lab.undo(&rename, "u1", false).await;
    assert!(!is_error && undone["state"] == "undone" && undone["epoch"] == "2", "{undone}");
    assert_eq!(lab.script.track(2)["name"], json!("Keys"));
    let (undone, is_error) = lab.undo(&add, "u2", false).await;
    assert!(!is_error && undone["restored"] == json!({"deleted": "2:track:3"}) && undone["epoch"] == "3", "{undone}");
    assert_eq!(lab.script.track_count(), 2);
    let (undone, is_error) = lab.undo(&volume, "u3", false).await;
    assert!(!is_error && undone["state"] == "undone", "{undone}");
    assert_eq!(lab.script.volume(1), 0.0);
    assert_eq!(lab.script.undo_history().len(), 6, "{:?}", lab.script.undo_history());

    // A track deleted: its restore is Cmd-Z, while Logic's undo has it on top; with another step on top, Kumi can't.
    let (delete, deleted) = lab.make("track-delete", json!({"ref": "3:track:2"}), "a5").await;
    assert_eq!((deleted["epoch"].clone(), deleted["applied"].clone(), lab.script.track_count()), (json!("4"), json!({"deleted": true}), 1));
    assert_eq!(deleted["display"]["title"], "Deleted track “Keys”");
    let (back, is_error) = lab.undo(&delete, "u4", false).await;
    assert!(!is_error && back["restored"]["ref"] == "5:track:2" && back["epoch"] == "5", "{back}");
    assert_eq!((lab.script.track_count(), lab.script.track(2)["name"].clone()), (2, json!("Keys")));
    let (delete, _) = lab.make("track-delete", json!({"ref": "5:track:2"}), "a6").await;
    lab.make("track-volume", json!({"ref": "6:track:1", "volumeDb": -3}), "a7").await;
    let (kept, is_error) = lab.undo(&delete, "u5", false).await;
    assert!(is_error && kept["code"] == "failed" && kept["error"].as_str().unwrap().contains("other steps on top"), "{kept}");
    assert_eq!(lab.script.track_count(), 1, "nothing changed");
    lab.simulator.stop();
}

/// A write Logic doesn't show back: the bridge answers uncertain, the journal keeps the change uncertain (never
/// applied again, never undone blindly), and an undo that went the same way is never sent twice.
#[tokio::test]
async fn a_write_logic_doesnt_show_back_is_uncertain_and_is_never_sent_twice() {
    let mut lab = Lab::start().await;
    lab.script.uncertain_once();
    let preview = lab.preview("track-volume", json!({"ref": "1:track:1", "volumeDb": -6})).await;
    let (unsure, is_error) = lab.apply(&preview, "a1").await;
    assert!(is_error && unsure["state"] == "uncertain" && unsure["code"] == "uncertain", "{unsure}");
    assert_eq!(unsure["transactionId"], preview["transactionId"]);
    assert!(unsure["error"].as_str().unwrap().contains("didn't show the value"), "{unsure}");
    assert_eq!(lab.script.volume(1), -6.0, "Logic took the write");
    assert_eq!(lab.script.undo_history(), [""], "a step of Logic's undo the bridge couldn't name");
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

    // An undo Logic doesn't show back: uncertain, kept under its key, never sent again.
    let (pan, applied) = lab.make("track-pan", json!({"ref": "1:track:1", "pan": 0.25}), "a3").await;
    assert_eq!(applied["display"]["title"], "Vocal pan 25% right");
    lab.script.uncertain_once();
    let (unsure, is_error) = lab.undo(&pan, "u2", false).await;
    assert!(is_error && unsure["state"] == "uncertain" && unsure["code"] == "uncertain", "{unsure}");
    assert_eq!(lab.script.track(1)["pan"], json!(0), "Logic put the pan back");
    let asked = lab.script.asked().len();
    assert_eq!(lab.undo(&pan, "u2", false).await, (unsure, true), "the same key answers the same");
    let (refused, is_error) = lab.undo(&pan, "u3", false).await;
    assert!(is_error && refused["code"] == "uncertain" && refused["error"].as_str().unwrap().contains("won't send it again"), "{refused}");
    assert_eq!(lab.script.asked().len(), asked, "nothing reached the bridge");
    lab.simulator.stop();
}

#[tokio::test]
async fn markers_the_tempo_and_the_cycle_change_and_come_back_in_logics_words() {
    let mut lab = Lab::start().await;
    let (tempo, applied) = lab.make("tempo-set", json!({"bpm": 128}), "a1").await;
    assert_eq!((tempo["prior"].clone(), tempo["target"].clone()), (json!({"bpm": 160.0}), json!({"ref": null, "kind": "project"})));
    assert_eq!(
        (applied["display"]["title"].clone(), applied["undo"]["title"].clone()),
        (json!("Tempo 128 BPM"), json!("Undo Change Tempo"))
    );
    let (read, _) = lab.tool("read", json!({"kind": "tempo"})).await;
    let (focus, _) = lab.tool("focus", json!({})).await;
    assert_eq!((read["items"][0]["bpm"].clone(), focus["transport"]["tempo"].clone()), (json!(128.0), json!(128.0)));
    let (undone, _) = lab.undo(&tempo, "u1", false).await;
    assert_eq!((undone["display"]["title"].clone(), lab.script.tempo()), (json!("Tempo back to 160 BPM"), 160.0));

    // The cycle area is no step of Logic's undo: Kumi's undo takes it back, and the answer says so.
    let (cycle, applied) = lab.make("loop-range", json!({"startBeats": 16, "endBeats": 32}), "a2").await;
    assert_eq!(
        applied["display"],
        json!({"title": "Cycle from bar 5 to bar 9", "note": "Logic Pro's own undo won't take this back; Kumi can."})
    );
    assert_eq!(applied["undo"]["entry"], false);
    assert_eq!(lab.script.cycle(), (true, 16.0, 32.0));
    let (focus, _) = lab.tool("focus", json!({})).await;
    assert_eq!(focus["transport"]["cycle"], true);
    let (undone, _) = lab.undo(&cycle, "u2", false).await;
    assert_eq!((undone["display"]["title"].clone(), lab.script.cycle()), (json!("Cycle off (put back)"), (false, 0.0, 0.0)));

    // Markers by their place in the Marker List, sorted by position: one added before the recorded one is first.
    let (add, added) = lab.make("marker-add", json!({"positionBeats": 16, "name": "Verse"}), "a3").await;
    assert_eq!((add["prior"].clone(), added["created"].clone()), (json!({"markers": 1}), json!({"ref": "1:marker:1", "kind": "marker"})));
    assert_eq!(added["display"]["title"], "Marker “Verse” at bar 5");
    let (page, _) = lab.tool("read", json!({"kind": "marker"})).await;
    assert_eq!(page["items"].as_array().unwrap().iter().map(|m| m["name"].as_str().unwrap()).collect::<Vec<_>>(), ["Verse", "Marker 1"]);
    assert_eq!(page["items"][0]["position"], json!({"bar": 5, "beat": 1, "division": 1, "tick": 1}));
    let (moved, applied) = lab.make("marker-move", json!({"ref": "1:marker:1", "positionBeats": 40}), "a4").await;
    assert_eq!(
        (moved["prior"].clone(), applied["display"]["title"].clone()),
        (json!({"positionBeats": 16.0}), json!("Moved marker “Verse” to bar 11"))
    );
    assert_eq!(lab.script.markers(), [("Marker 1".to_string(), 32.0), ("Verse".to_string(), 40.0)]);
    let (rename, renamed) = lab.make("marker-rename", json!({"ref": "1:marker:2", "name": "Chorus"}), "a5").await;
    assert_eq!(renamed["display"]["title"], "Renamed marker “Verse” → “Chorus”");
    let (delete, deleted) = lab.make("marker-delete", json!({"ref": "1:marker:2"}), "a6").await;
    assert_eq!(
        (deleted["applied"].clone(), deleted["display"]["title"].clone()),
        (json!({"deleted": true}), json!("Deleted marker “Chorus”"))
    );
    assert_eq!(lab.script.markers(), [("Marker 1".to_string(), 32.0)], "what it was is the journal's to keep (material)");
    let (back, _) = lab.undo(&delete, "u3", false).await;
    assert_eq!(
        (back["restored"]["ref"].clone(), back["display"]["title"].clone()),
        (json!("1:marker:2"), json!("Marker “Chorus” put back"))
    );
    // The marker Kumi added was moved and renamed since: no marker is as Kumi added it, so its undo leaves it even told
    // to discard (a marker's ref is its place in the list, and deleting whatever is there now is no put-back). The
    // later changes undone first, each finding the marker by what it set, it goes.
    let (kept, is_error) = lab.undo(&add, "u4", true).await;
    assert!(is_error && kept["code"] == "changed", "{kept}");
    let (undone, is_error) = lab.undo(&rename, "u5", false).await;
    assert!(!is_error && undone["restored"] == json!({"name": "Verse"}), "{undone}");
    let (undone, is_error) = lab.undo(&moved, "u6", false).await;
    assert!(!is_error && undone["restored"] == json!({"positionBeats": 16.0}), "{undone}");
    assert_eq!(lab.script.markers(), [("Verse".to_string(), 16.0), ("Marker 1".to_string(), 32.0)]);
    let (gone, is_error) = lab.undo(&add, "u7", false).await;
    assert!(!is_error && gone["restored"] == json!({"deleted": "1:marker:1"}), "{gone}");
    assert_eq!(lab.script.markers(), [("Marker 1".to_string(), 32.0)]);
    assert_eq!(lab.script.epoch(), 1, "markers, the tempo and the cycle never move the epoch");
    lab.simulator.stop();
}

#[tokio::test]
async fn a_dialog_stops_a_change_before_anything_is_written_and_the_transport_and_playhead_act() {
    let mut lab = Lab::start().await;
    lab.script.busy_once();
    let epoch = lab.script.epoch().to_string();
    let (busy, is_error) =
        lab.tool("change_preview", json!({"kind": "track-mute", "input": {"ref": "1:track:1", "mute": true}, "epoch": epoch})).await;
    assert!(
        is_error && busy["code"] == "busy" && busy["busy"] == true && busy["error"] == "a dialog is up in Logic Pro; nothing changed",
        "{busy}"
    );
    assert_eq!(lab.script.track(1)["mute"], json!(false));

    let (playing, is_error) = lab.tool("transport_play", json!({})).await;
    assert!(!is_error && playing == json!({"started": true, "playing": true}), "{playing}");
    let (focus, _) = lab.tool("focus", json!({})).await;
    assert_eq!(focus["transport"]["playing"], true);
    // The playhead, in beats or in seconds (160 BPM: a beat is 0.375 s).
    let (moved, is_error) = lab.tool("cursor_set", json!({"positionBeats": 8})).await;
    assert!(!is_error && moved == json!({"epoch": "1", "positionBeats": 8.0, "position": 3.0, "playing": true}), "{moved}");
    let (moved, _) = lab.tool("cursor_set", json!({"position": 6})).await;
    assert_eq!((moved["positionBeats"].clone(), lab.script.playhead()), (json!(16.0), 16.0));
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
    let epoch = lab.script.epoch().to_string();
    lab.tool("change_preview", json!({"kind": "track-pan", "input": {"ref": "1:track:1", "pan": -0.5}, "epoch": epoch})).await;
    lab.script.uncertain_once();
    let preview = lab.preview("track-arm", json!({"ref": "1:track:1", "arm": true})).await;
    lab.apply(&preview, "a2").await;
    lab.tool("undo_step_begin", json!({"label": "A plan"})).await;
    lab.make("track-mute", json!({"ref": "1:track:2", "mute": true}), "a3").await;
    lab.make("track-solo", json!({"ref": "1:track:2", "solo": true}), "a4").await;
    lab.tool("undo_step_end", json!({"stepId": "s1", "label": "Keys muted and 1 more"})).await;
    let (add, _) = lab.make("track-add", json!({"name": "Pad", "kind": "audio"}), "a5").await;
    lab.make("track-rename", json!({"ref": "2:track:3", "name": "Pads"}), "a6").await;
    lab.undo(&add, "u2", false).await;
    let (marker, _) = lab.make("marker-add", json!({"positionBeats": 16, "name": "Verse"}), "a7").await;
    let (moved, _) = lab.make("marker-move", json!({"ref": "3:marker:1", "positionBeats": 20}), "a8").await;
    lab.undo(&moved, "u3", false).await;
    lab.undo(&marker, "u4", false).await;
    let (tempo, _) = lab.make("tempo-set", json!({"bpm": 128}), "a9").await;
    lab.undo(&tempo, "u5", false).await;
    lab.make("loop-range", json!({"startBeats": 16, "endBeats": 32}), "a10").await;
    lab.tool("transport_play", json!({})).await;
    lab.tool("cursor_set", json!({"positionBeats": 8})).await;
    lab.tool("transport_stop", json!({})).await;
    let (delete, _) = lab.make("track-delete", json!({"ref": "3:track:2"}), "a11").await;
    lab.undo(&delete, "u6", false).await;
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
    ] {
        assert!(asked.iter().any(|m| m == method), "{method} never reached the bridge");
    }
    golden("logic-changes-bridge.ndjson", &without_nonces(&normalized(&lab.simulator.transcript())));
    lab.simulator.stop();
}
