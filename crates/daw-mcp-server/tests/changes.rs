//! The host's change tools (M3 contract §3a) against the simulated REAPER: preview, apply and undo through the journal,
//! each step fenced, each key answered once.
mod common;

use common::Session;
use daw_bridge::files::ensure_secret;
use daw_bridge::paths::BridgePaths;
use daw_bridge::Daw;
use daw_mcp_server::simulator::reaper::ReaperScript;
use daw_mcp_server::simulator::renoise::RenoiseScript;
use daw_mcp_server::simulator::Simulator;
use serde_json::{json, Value};

struct Lab {
    _home: tempfile::TempDir,
    session: Session,
    script: ReaperScript,
    _simulator: Simulator,
    next_id: u64,
}

impl Lab {
    async fn start() -> Lab {
        let home = tempfile::tempdir().unwrap();
        let paths = BridgePaths::new(home.path(), Daw::Reaper);
        let secret = ensure_secret(&paths.secret()).unwrap();
        let script = ReaperScript::new(3, 0);
        let mut config = script.config(secret, "/lab/REAPER");
        config.discovery_dir = Some(paths.discovery());
        let simulator = Simulator::start(config).await.unwrap();
        let session = Session::start(Daw::Reaper, home.path());
        Lab { _home: home, session, script, _simulator: simulator, next_id: 100 }
    }

    async fn tool(&mut self, name: &str, arguments: Value) -> (Value, bool) {
        self.next_id += 1;
        let id = self.next_id;
        self.session.tool_full(id, name, arguments, false).await
    }

    /// Track 1's volume to `db`, previewed: the preview's answer.
    async fn preview(&mut self, db: f64) -> Value {
        let (preview, is_error) =
            self.tool("change_preview", json!({"kind": "track-volume", "input": {"ref": "r:track:{T0001}", "volumeDb": db}})).await;
        assert!(!is_error, "{preview}");
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
}

#[tokio::test]
async fn a_change_is_previewed_applied_and_undone_each_as_one_named_step_of_reapers_undo() {
    let mut lab = Lab::start().await;
    let preview = lab.preview(-6.0).await;
    assert_eq!((&preview["prior"], &preview["target"]["ref"]), (&json!({"volumeDb": 0.0}), &json!("r:track:{T0001}")));
    assert_eq!(preview["epoch"], "sim-1");
    assert_eq!(lab.script.volume(1), 0.0, "a preview changes nothing");

    let (wrong, is_error) =
        lab.tool("change_apply", json!({"transactionId": preview["transactionId"], "confirmation": "c0000", "idempotencyKey": "a1"})).await;
    assert!(is_error && wrong["code"] == "confirmation", "{wrong}");

    let (applied, is_error) = lab.apply(&preview, "a1").await;
    assert!(!is_error && applied["state"] == "applied", "{applied}");
    assert_eq!(lab.script.volume(1), -6.0);
    let desc = applied["undo"]["desc"].as_str().unwrap().to_string();
    assert!(desc.starts_with("Kumi: Track 1 volume -6 dB (k") && desc.ends_with(')'), "{desc}");
    // The same key again: the same answer, and REAPER changed once.
    assert_eq!(lab.apply(&preview, "a1").await, (applied.clone(), false));
    assert_eq!(lab.script.undo_history().len(), 1);

    let (undone, is_error) = lab.undo(&preview, "u1", false).await;
    assert!(!is_error && undone["state"] == "undone", "{undone}");
    assert_eq!(lab.script.volume(1), 0.0);
    let history = lab.script.undo_history();
    assert_eq!(history.len(), 2, "the undo is a new step, never REAPER's own undo");
    assert!(history[1].starts_with("Kumi: undo Track 1 volume -6 dB (k") && history[1] != history[0], "{history:?}");
    assert_eq!(lab.undo(&preview, "u1", false).await, (undone, false));
    let (again, is_error) = lab.undo(&preview, "u2", false).await;
    assert!(is_error && again["code"] == "undone", "{again}");
}

#[tokio::test]
async fn what_the_producer_changed_since_is_never_overwritten_unless_kumi_is_told_to() {
    let mut lab = Lab::start().await;
    // Moved between the preview and the apply: nothing is written.
    let preview = lab.preview(-6.0).await;
    lab.script.set_volume(1, -3.0);
    let (refused, is_error) = lab.apply(&preview, "a1").await;
    assert!(is_error && refused["code"] == "changed", "{refused}");
    assert_eq!(lab.script.volume(1), -3.0);

    // Moved after the apply: the undo is refused and the change stays undoable.
    let preview = lab.preview(-6.0).await;
    lab.apply(&preview, "a1").await;
    lab.script.set_volume(1, -9.0);
    let (refused, is_error) = lab.undo(&preview, "u1", false).await;
    assert!(is_error && refused["code"] == "changed", "{refused}");
    assert_eq!(lab.script.volume(1), -9.0);
    // Put back by hand, the same undo goes through.
    lab.script.set_volume(1, -6.0);
    let (undone, is_error) = lab.undo(&preview, "u1", false).await;
    assert!(!is_error && undone["state"] == "undone", "{undone}");
    assert_eq!(lab.script.volume(1), -3.0, "back to what the preview saw");

    // Told to discard: the values go back whatever happened since.
    let preview = lab.preview(-12.0).await;
    lab.apply(&preview, "a1").await;
    lab.script.set_volume(1, 1.0);
    let (undone, is_error) = lab.undo(&preview, "u1", true).await;
    assert!(!is_error && undone["state"] == "undone", "{undone}");
    assert_eq!(lab.script.volume(1), -3.0);
}

#[tokio::test]
async fn a_change_previewed_in_another_project_is_refused_and_released_changes_are_forgotten() {
    let mut lab = Lab::start().await;
    let preview = lab.preview(-6.0).await;
    lab.script.switch_tab();
    let (stale, is_error) = lab.apply(&preview, "a1").await;
    assert!(is_error && stale["code"] == "stale", "{stale}");
    assert_eq!(lab.script.volume(1), 0.0);

    let preview = lab.preview(-6.0).await;
    lab.apply(&preview, "a1").await;
    let (released, is_error) = lab.tool("change_release", json!({"transactionIds": [preview["transactionId"], "t-unknown"]})).await;
    assert_eq!((released, is_error), (json!({"released": 1}), false));
    let (gone, is_error) = lab.undo(&preview, "u1", false).await;
    assert!(is_error && gone["code"] == "unknown", "{gone}");
    let (bad, is_error) = lab.tool("change_release", json!({"transactionIds": []})).await;
    assert!(is_error && bad["code"] == "bad-params", "{bad}");
}

#[tokio::test]
async fn an_undo_the_daw_couldnt_say_it_finished_is_never_sent_again() {
    let mut lab = Lab::start().await;
    let preview = lab.preview(-6.0).await;
    lab.apply(&preview, "a1").await;
    lab.script.uncertain_restore_once();
    let (unsure, is_error) = lab.undo(&preview, "u1", false).await;
    assert!(is_error && unsure["code"] == "undo-uncertain", "{unsure}");
    let restores = lab.script.undo_history().len();
    // Asked again (a new key): refused, nothing sent, so the restore isn't made twice.
    let (again, is_error) = lab.undo(&preview, "u2", false).await;
    assert!(is_error && again["code"] == "undo-uncertain", "{again}");
    assert_eq!(lab.script.undo_history().len(), restores);
}

// (r-tracks) REAPER's saves are no change: the host hands them to the bridge by name, its answer back, no journal.
#[tokio::test]
async fn reapers_saves_go_to_the_bridge_by_name_and_are_no_change() {
    let mut lab = Lab::start().await;
    let (answer, is_error) = lab.tool("project_save", json!({"version": true, "epoch": "e1"})).await;
    assert!(!is_error, "{answer}");
    assert_eq!(answer["saved"], "/lab/Projects/Sim.rpp");
    let (answer, is_error) = lab.tool("track_template_save", json!({"trackRefs": ["r:track:{T0001}"], "name": "Kit"})).await;
    assert!(is_error, "the epoch is needed: {answer}");
    let (answer, is_error) = lab.tool("track_template_save", json!({"trackRefs": ["r:track:{T0001}"], "name": "Kit", "epoch": "e1"})).await;
    assert!(!is_error, "{answer}");
    assert_eq!(answer["saved"], "/lab/REAPER/TrackTemplates/Kit.RTrackTemplate");
    let asked = lab.script.asked();
    let (_, sent) = asked.iter().find(|(method, _)| method == "project_save").expect("asked by name");
    assert_eq!(sent, &json!({"version": true, "epoch": "e1"}), "the arguments as they came");
    // Nothing is kept for undo.
    let (released, _) = lab.tool("change_release", json!({"transactionIds": ["t1"]})).await;
    assert_eq!(released["released"], 0, "{released}");
}

#[tokio::test]
async fn renoises_song_is_saved_through_the_same_tool_and_a_version_is_refused() {
    let home = tempfile::tempdir().unwrap();
    let paths = BridgePaths::new(home.path(), Daw::Renoise);
    let secret = ensure_secret(&paths.secret()).unwrap();
    let script = RenoiseScript::new();
    let mut config = script.config_with_changes(secret, "/lab/Renoise");
    config.discovery_dir = Some(paths.discovery());
    let _simulator = Simulator::start(config).await.unwrap();
    let mut session = Session::start(Daw::Renoise, home.path());
    let (snapshot, is_error) = session.tool_full(1, "snapshot", json!({}), false).await;
    assert!(!is_error, "{snapshot}");
    let epoch = snapshot["epoch"].clone();
    let (answer, is_error) = session.tool_full(2, "project_save", json!({"epoch": epoch}), false).await;
    assert!(!is_error, "{answer}");
    assert_eq!(answer["saved"], "/lab/Renoise/Sim.xrns");
    let (answer, is_error) = session.tool_full(3, "project_save", json!({"epoch": epoch, "version": true}), false).await;
    assert!(is_error, "{answer}");
    assert_eq!(answer["code"], "bad-params");
    // REAPER's track templates are no tool of Renoise's.
    let message = json!({"jsonrpc": "2.0", "id": 4, "method": "tools/call", "params": {"name": "track_template_save", "arguments": {"trackRefs": ["track:1"], "name": "Kit", "epoch": epoch}}});
    let answer = session.call_as(message, false).await;
    assert_eq!(answer["error"]["code"], -32602, "{answer}");
}
