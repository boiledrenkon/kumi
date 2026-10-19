//! The Doofer, Splitter and Notepad changes the host makes from a device's preset data (M4 n-devices), through its tools
//! against a simulated Renoise tool that does what the real one does with them: reads the preset data (`preset_read`),
//! writes what the host composed while the device's shape is the one the host edited from, and puts the host's restore
//! back the same way.
mod common;

use common::Session;
use daw_bridge::files::ensure_secret;
use daw_bridge::paths::BridgePaths;
use daw_bridge::Daw;
use daw_mcp_server::presets::shape_of;
use daw_mcp_server::simulator::{Simulator, SimulatorConfig};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};

const DOOFER: &str = include_str!("fixtures/renoise/doofer_eq5_gainer.xml");

/// The Doofer on track 3 of a simulated song: its preset data, and every write the host asked for.
#[derive(Default)]
struct Song {
    xml: String,
    writes: Vec<(String, String)>,
}

struct Lab {
    _home: tempfile::TempDir,
    session: Session,
    song: Arc<Mutex<Song>>,
    _simulator: Simulator,
    next: u64,
}

fn refusal(code: &str, message: &str) -> Option<Result<Value, (String, String)>> {
    Some(Err((code.into(), message.into())))
}

impl Lab {
    async fn start() -> Lab {
        let home = tempfile::tempdir().unwrap();
        let paths = BridgePaths::new(home.path(), Daw::Renoise);
        let secret = ensure_secret(&paths.secret()).unwrap();
        let song = Arc::new(Mutex::new(Song { xml: DOOFER.into(), ..Song::default() }));
        let state = song.clone();
        let respond: daw_mcp_server::simulator::Respond = Arc::new(move |method, params| {
            let mut song = state.lock().unwrap();
            let at = json!({"track": 3, "dev": {"n": 2, "mark": "d1:4", "print": {"path": "Audio/Effects/Native/Doofer", "name": "Doofer"}}, "label": "Doofer"});
            match method {
                "preset_read" => Some(Ok(json!({
                    "epoch": "e1", "layout": 0, "ref": "e1:device:3.2", "target": {"kind": "device", "track": 3, "device": 2}, "at": at,
                    "id": "track:Bass|Audio/Effects/Native/Doofer", "path": "Audio/Effects/Native/Doofer", "name": "Doofer", "label": "Doofer",
                    "trackName": "Bass", "presetData": song.xml, "shape": shape_of(&song.xml),
                }))),
                "change_apply" => {
                    let input = &params["input"];
                    if input["shape"].as_str() != Some(&shape_of(&song.xml)) {
                        return refusal("changed", "“Doofer on Bass” changed in Renoise since Kumi read it: read it again");
                    }
                    let xml = input["presetData"].as_str().unwrap().to_string();
                    song.writes.push((params["desc"].as_str().unwrap().into(), xml.clone()));
                    song.xml = xml;
                    Some(Ok(json!({
                        "epoch": "e1", "layout": 0, "applied": {"shape": shape_of(&song.xml), "edit": input["edit"]}, "material": at,
                        "target": {"kind": "device", "track": 3, "device": 2}, "display": input["display"],
                        "undo": {"desc": params["desc"], "entry": true},
                    })))
                }
                "change_restore" => {
                    let write = &params["material"]["write"];
                    if write["shape"].as_str() != Some(&shape_of(&song.xml)) {
                        return refusal("changed", "“Doofer on Bass” changed in Renoise since Kumi read it");
                    }
                    let xml = write["presetData"].as_str().unwrap().to_string();
                    song.writes.push((params["desc"].as_str().unwrap().into(), xml.clone()));
                    song.xml = xml;
                    Some(Ok(
                        json!({"epoch": "e1", "layout": 0, "restored": {"shape": shape_of(&song.xml)}, "display": write["display"], "undo": {"desc": params["desc"], "entry": true}}),
                    ))
                }
                _ => None,
            }
        });
        let mut config =
            SimulatorConfig::renoise(secret, "/lab/Renoise").with_methods(&["preset_read", "change_apply", "change_restore"], respond);
        config.discovery_dir = Some(paths.discovery());
        let simulator = Simulator::start(config).await.unwrap();
        let session = Session::start(Daw::Renoise, home.path());
        Lab { _home: home, session, song, _simulator: simulator, next: 100 }
    }

    async fn tool(&mut self, name: &str, arguments: Value) -> (Value, bool) {
        self.next += 1;
        self.session.tool_full(self.next, name, arguments, false).await
    }

    async fn preview(&mut self, kind: &str, input: Value) -> Value {
        let (preview, is_error) = self.tool("change_preview", json!({"kind": kind, "input": input, "epoch": "e1", "layout": 0})).await;
        assert!(!is_error, "{preview}");
        preview
    }

    async fn apply(&mut self, preview: &Value, key: &str) -> (Value, bool) {
        self.tool(
            "change_apply",
            json!({"transactionId": preview["transactionId"], "confirmation": preview["confirmation"], "idempotencyKey": key}),
        )
        .await
    }

    async fn undo(&mut self, preview: &Value, key: &str, discard: bool) -> (Value, bool) {
        self.tool(
            "change_undo",
            json!({"transactionId": preview["transactionId"], "confirmation": "undo", "idempotencyKey": key, "discard": discard}),
        )
        .await
    }

    fn xml(&self) -> String {
        self.song.lock().unwrap().xml.clone()
    }

    fn writes(&self) -> usize {
        self.song.lock().unwrap().writes.len()
    }

    fn edit_xml(&self, from: &str, to: &str) {
        let mut song = self.song.lock().unwrap();
        song.xml = song.xml.replacen(from, to, 1);
    }
}

#[tokio::test]
async fn a_macro_is_named_and_mapped_in_one_write_and_kumis_undo_writes_the_preset_data_back_exactly() {
    let mut lab = Lab::start().await;
    let preview = lab
        .preview("doofer-macro", json!({"ref": "e1:device:3.2", "macro": 1, "name": "Drive", "mappings": [{"device": 2, "param": "Gain"}]}))
        .await;
    assert_eq!(
        preview["display"]["title"],
        "Doofer on Bass: macro 1 named \u{201c}Drive\u{201d}, mapped to \u{201c}Gain\u{201d} of \u{201c}Gainer\u{201d}"
    );
    assert_eq!(lab.writes(), 0, "a preview writes nothing");
    let (applied, is_error) = lab.apply(&preview, "a1").await;
    assert!(!is_error && applied["state"] == "applied", "{applied}");
    assert!(lab.xml().contains("<Name>Drive</Name>") && lab.xml().contains("<DestChainIndex>0</DestChainIndex>"));
    let desc = &lab.song.lock().unwrap().writes[0].0.clone();
    assert!(desc.starts_with("Kumi: Doofer on Bass: macro 1 named") && desc.ends_with(')'), "{desc}");
    // The same key: once.
    assert_eq!(lab.apply(&preview, "a1").await, (applied, false));
    assert_eq!(lab.writes(), 1);
    // A knob moved by the producer (a value) since: the undo still puts back only what Kumi changed.
    lab.edit_xml("<Value>1000</Value>", "<Value>1500</Value>");
    let (undone, is_error) = lab.undo(&preview, "u1", false).await;
    assert!(!is_error && undone["state"] == "undone", "{undone}");
    assert_eq!(lab.xml(), DOOFER.replacen("<Value>1000</Value>", "<Value>1500</Value>", 1));
    // Asked again: the same answer, nothing written.
    assert_eq!(lab.undo(&preview, "u1", false).await.0, undone);
    assert_eq!(lab.writes(), 2);
}

#[tokio::test]
async fn a_producers_edit_between_preview_and_apply_refuses_the_apply_and_nothing_is_written() {
    let mut lab = Lab::start().await;
    let preview = lab.preview("doofer-macro", json!({"ref": "e1:device:3.2", "macro": 1, "name": "Drive"})).await;
    lab.edit_xml("<Name>Macro 1</Name>", "<Name>Theirs</Name>");
    let (refused, is_error) = lab.apply(&preview, "a1").await;
    assert!(is_error && refused["code"] == "changed", "{refused}");
    assert_eq!(lab.writes(), 0);
    assert!(lab.xml().contains("<Name>Theirs</Name>"));
    // Previewed again, it goes through.
    let again = lab.preview("doofer-macro", json!({"ref": "e1:device:3.2", "macro": 1, "name": "Drive"})).await;
    assert!(!lab.apply(&again, "a2").await.1);
}

#[tokio::test]
async fn a_device_added_inside_is_taken_out_by_kumis_undo_unless_the_devices_or_mappings_changed_since() {
    let mut lab = Lab::start().await;
    let preview = lab.preview("doofer-device-add", json!({"ref": "e1:device:3.2", "path": "*LFO", "at": [1]})).await;
    assert_eq!(preview["display"]["title"], "Added \u{201c}*LFO\u{201d} to \u{201c}Doofer on Bass\u{201d} (place 1)");
    assert!(!lab.apply(&preview, "a1").await.1);
    assert!(lab.xml().contains("<LfoDevice type=\"LfoDevice\""));
    // The producer adds a mapping in Renoise since: structural, so Kumi can't tell what to take back.
    let after = lab.xml();
    lab.edit_xml("<Name>Macro 2</Name>", "<Name>Macro 2</Name>\n      <Mappings>\n        <Mapping>\n          <DestDeviceIndex>0</DestDeviceIndex>\n        </Mapping>\n      </Mappings>");
    let (refused, is_error) = lab.undo(&preview, "u1", true).await;
    assert!(is_error && refused["code"] == "changed", "{refused}");
    assert_eq!(lab.writes(), 1, "nothing written, discard or not");
    // Taken back by the producer: Kumi's undo goes through, and gives the Doofer as it was.
    lab.song.lock().unwrap().xml = after;
    let (undone, is_error) = lab.undo(&preview, "u2", false).await;
    assert!(!is_error, "{undone}");
    assert_eq!(lab.xml(), DOOFER);
}

#[tokio::test]
async fn an_undo_that_finds_it_back_already_writes_nothing_and_a_preview_never_sends_kept_xml_to_kumi() {
    let mut lab = Lab::start().await;
    let preview = lab.preview("doofer-device-delete", json!({"ref": "e1:device:3.2", "at": [2]})).await;
    assert!(preview["prior"].get("xml").is_none(), "Kumi isn't sent the kept device: {}", preview["prior"]);
    assert!(!lab.apply(&preview, "a1").await.1);
    assert!(!lab.xml().contains("GainerDevice"));
    // The producer's Cmd-Z brought it back: Kumi's undo writes nothing and says so.
    lab.song.lock().unwrap().xml = DOOFER.into();
    let set = lab.preview("doofer-device-set", json!({"ref": "e1:device:3.2", "at": [2], "bypass": true})).await;
    assert!(!lab.apply(&set, "a2").await.1);
    lab.edit_xml(
        "<Value>0.0</Value>\n            <Visualization>Device only</Visualization>\n          </IsActive>\n          <Volume>",
        "<Value>1.0</Value>\n            <Visualization>Device only</Visualization>\n          </IsActive>\n          <Volume>",
    );
    let writes = lab.writes();
    let (back, is_error) = lab.undo(&set, "u1", false).await;
    assert!(!is_error && back["state"] == "undone", "{back}");
    assert!(back["display"]["note"].as_str().unwrap().contains("back already"), "{back}");
    assert_eq!(lab.writes(), writes, "nothing written");
}
