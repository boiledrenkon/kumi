//! A scripted Renoise song for the host's tests: the tool's `read`, `snapshot`, `focus` and `focus_body` answered the
//! way the tool answers them (the M2 contract).
//!
//! What the tool answers to a fixed set of requests is kept in tests/vectors/renoise-reads.json, which the tool's own
//! tests write from the real tool over the mock Renoise (KUMI_BLESS=1) and fail on when it changes; this replays those
//! answers verbatim, so the simulator cannot drift from the tool. A song of one very full pattern can be added, to
//! have pages as big as the tool makes them (pages by limit and cursor, cut at the frame bound), and the song can be
//! switched, which moves the epoch as Renoise's tool does: refs and cursors from before are refused as `stale`, and
//! the answers carry the new epoch.
use super::{Respond, SimulatorConfig};
use daw_bridge::auth::Secret;
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};

const VECTORS: &str = include_str!("../../tests/vectors/renoise-reads.json");
/// The epoch the vectors were made in.
const EPOCH: &str = "1";
const VECTORS_EPOCH: u32 = 1; // (the same, as a number)
/// The most a page carries: the contract's 256 KiB less room for the envelope, as in the tool.
const PAGE_BYTES: usize = 256 * 1024 - 4096;
const DEFAULT_LIMIT: usize = 50;
const MAX_LIMIT: usize = 200;
const METHODS: [&str; 5] = ["read", "snapshot", "focus", "focus_body", "digest"];
/// The ref kinds a device change leaves naming what they named, as the tool lists them (`kept.kinds`).
const KEPT_BY_DEVICES: [&str; 9] = ["song", "track", "ptrack", "slot", "seq", "pattern", "instrument", "sample", "phrase"];
/// What a song made to be changed answers besides (`config_with_changes`).
const CHANGE_METHODS: [&str; 4] = ["change_preview", "change_apply", "change_restore", "project_save"];
/// (m6-places) A line in the status bar, which a song made to be changed shows too.
const STATUS_MESSAGE: &str = "status_message";

struct Case {
    method: String,
    params: Value,
    response: Result<Value, (String, String)>,
}

struct State {
    /// The epoch the song is in now (the vectors were made in [`EPOCH`]).
    epoch: u32,
    cases: Vec<Case>,
    /// Lines of the very full pattern (track 1 of pattern 1), if there is one.
    bulk: Option<usize>,
    /// The simulator keeps its pages inside the frame bound, as the tool does; a test can make it ignore the bound.
    keeps_bound: bool,
    /// What the simulator was asked, in order: method and params.
    asked: Vec<(String, Value)>,
    /// Renoise started again and is still on its empty song: the file it was opened with isn't loaded yet.
    empty: bool,
    /// How often Renoise was started again: each start is another process, so the song's identity is another.
    restarts: u32,
    /// The file loads while the next snapshot of the empty song is being read: that read ends `epoch-changed`.
    load_during_look: bool,
    /// What Kumi's changes did to the song: each track's volume and mute, and the devices added to it.
    volume: std::collections::HashMap<u64, f64>,
    mute: std::collections::HashMap<u64, bool>,
    devices: std::collections::HashMap<u64, u64>,
    /// Renoise's undo history, as the changes named their steps.
    undo: Vec<String>,
    /// The first epoch of the run the song is in: epochs moved only for devices keep the tracks, and the tool takes a
    /// track ref from any epoch of the run (`kept`).
    run_start: u32,
    /// M6: what the producer changed by hand, for the digest: tracks renamed (the tempo is the song's `bpm`).
    names: std::collections::HashMap<u64, String>,
    /// (m6-places) What the fixed phrases change: the song's `bpm`, the sequence's `loop` {first, last}, the `patternLoop`,
    /// the `metronome`, and each track's solo.
    song: Value,
    solo: std::collections::HashMap<u64, bool>,
    /// The lines shown in the status bar, oldest first.
    statuses: Vec<String>,
}

/// A song that answers like the tool. Clones share the song.
#[derive(Clone)]
pub struct RenoiseScript {
    state: Arc<Mutex<State>>,
}

/// The epoch a ref (`<epoch>:<kind>:…`) or a cursor (`<epoch>.<layout>.<n>`) names, if the text is one.
fn epoch_of(text: &str) -> Option<u32> {
    let end = text.find([':', '.'])?;
    let (digits, rest) = text.split_at(end);
    (!digits.is_empty() && rest.len() > 1 && digits.bytes().all(|b| b.is_ascii_digit())).then(|| digits.parse().ok()).flatten()
}

/// `value` with the epoch `from` made `to`: in every ref and cursor that names it, and in a field called `epoch`.
fn retag(value: &Value, from: u32, to: u32, key: Option<&str>) -> Value {
    match value {
        Value::String(text) if key == Some("epoch") && *text == from.to_string() => Value::String(to.to_string()),
        // Another song is another identity, and another id.
        Value::String(text) if key == Some("identity") && from != to => Value::String(format!("{text}.{to}")),
        Value::String(text) if key == Some("id") && from != to && text.len() == 32 => Value::String(format!("{}{to:02x}", &text[..30])),
        Value::String(text) if epoch_of(text) == Some(from) => {
            Value::String(format!("{to}{}", &text[text.find([':', '.']).expect("a ref or cursor")..]))
        }
        Value::Array(items) => Value::Array(items.iter().map(|item| retag(item, from, to, key)).collect()),
        Value::Object(map) => Value::Object(map.iter().map(|(k, v)| (k.clone(), retag(v, from, to, Some(k)))).collect()),
        other => other.clone(),
    }
}

/// The first ref or cursor in `params` that is from another epoch than `epoch`.
fn from_another_epoch(params: &Value, epoch: u32) -> Option<u32> {
    match params {
        Value::String(text) => epoch_of(text).filter(|from| *from != epoch),
        Value::Array(items) => items.iter().find_map(|item| from_another_epoch(item, epoch)),
        Value::Object(map) => map.values().find_map(|value| from_another_epoch(value, epoch)),
        _ => None,
    }
}

/// Whole numbers as integers, so 3 and 3.0 are the same parameter.
fn normal(value: &Value) -> Value {
    match value {
        Value::Number(n) => match n.as_f64() {
            Some(f) if f.fract() == 0.0 && f.abs() < 1e15 => json!(f as i64),
            _ => value.clone(),
        },
        Value::Array(items) => Value::Array(items.iter().map(normal).collect()),
        Value::Object(map) => Value::Object(map.iter().map(|(k, v)| (k.clone(), normal(v))).collect()),
        other => other.clone(),
    }
}

fn load_cases() -> Vec<Case> {
    let vectors: Value = serde_json::from_str(VECTORS).expect("tests/vectors/renoise-reads.json is JSON");
    vectors["cases"]
        .as_array()
        .expect("cases")
        .iter()
        .map(|case| {
            let response = &case["response"];
            Case {
                method: case["request"]["method"].as_str().expect("method").to_string(),
                params: normal(&case["request"]["params"]),
                response: if response["ok"] == true {
                    Ok(response["result"].clone())
                } else {
                    Err((
                        response["error"]["code"].as_str().expect("code").to_string(),
                        response["error"]["message"].as_str().expect("message").to_string(),
                    ))
                },
            }
        })
        .collect()
}

/// A line of the very full pattern: twelve note columns and eight effect columns, all with something in them.
fn full_line(number: usize) -> Value {
    let notes: Vec<Value> = (1..=12)
        .map(|col| json!({"col": col, "note": "C-5", "instrument": "01", "volume": "40", "panning": "20", "delay": "10", "fx": "0A20"}))
        .collect();
    let effects: Vec<Value> = (1..=8).map(|col| json!({"col": col, "number": "0A", "amount": "40"})).collect();
    json!({"line": number, "notes": notes, "effects": effects})
}

impl RenoiseScript {
    /// The song of the vectors: what the tool answers for the demo song.
    pub fn new() -> Self {
        RenoiseScript {
            state: Arc::new(Mutex::new(State {
                epoch: VECTORS_EPOCH,
                cases: load_cases(),
                bulk: None,
                keeps_bound: true,
                asked: vec![],
                empty: false,
                restarts: 0,
                load_during_look: false,
                volume: Default::default(),
                mute: Default::default(),
                devices: Default::default(),
                undo: vec![],
                run_start: VECTORS_EPOCH,
                names: Default::default(),
                song: json!({"bpm": 128, "loop": {"first": 0, "last": 0}, "patternLoop": false, "metronome": false}),
                solo: Default::default(),
                statuses: vec![],
            })),
        }
    }

    /// Another song is opened (or something refs index into is restructured): the epoch moves, so refs and cursors
    /// from before are refused as stale and the answers carry the new one.
    pub fn switch_song(&self) {
        let mut state = self.state.lock().unwrap();
        state.epoch += 1;
        state.run_start = state.epoch;
    }

    /// The same, and track 1 of pattern 1 (`1:pattern:1`) has `lines` lines with every column full: its lines are paged
    /// at the frame bound.
    pub fn with_full_pattern(lines: usize) -> Self {
        let script = Self::new();
        script.state.lock().unwrap().bulk = Some(lines);
        script
    }

    /// The simulator stops keeping its pages inside the frame bound, as a bridge that doesn't know it would.
    pub fn ignore_the_frame_bound(&self) {
        self.state.lock().unwrap().keeps_bound = false;
    }

    /// Renoise started again (another process, so another identity for the song), on its empty unsaved song first, as
    /// it starts before it loads the file it was opened with.
    pub fn start_again_on_the_empty_song(&self) {
        let mut state = self.state.lock().unwrap();
        state.restarts += 1;
        state.empty = true;
    }

    /// The file it was opened with is loaded: the same song as before (its id is its path's), in the new process.
    pub fn load_the_file(&self) {
        self.state.lock().unwrap().empty = false;
    }

    /// The file loads while the next look at the empty song is being read, as on real Renoise: that read ends
    /// `epoch-changed`, and the next one finds the file.
    pub fn load_the_file_during_the_next_look(&self) {
        self.state.lock().unwrap().load_during_look = true;
    }

    /// The empty song Renoise starts on: unsaved, an id of its own, nothing in it.
    fn empty_song(state: &State, method: &str, params: &Value) -> Option<Result<Value, (String, String)>> {
        let epoch = (state.epoch + 100).to_string();
        let project = json!({"id": format!("{:032x}", 0xe0e0 + state.restarts), "identity": format!("4243:{}", state.restarts), "name": "", "path": null, "bpm": 120, "lpb": 4});
        Some(Ok(match method {
            "snapshot" => {
                json!({"epoch": epoch, "layout": 0, "project": project, "tracks": [], "selection": {}, "transport": {"playing": false}})
            }
            "focus" => json!({"epoch": epoch, "layout": 0, "view": "pattern", "selection": {}, "transport": {"playing": false}}),
            "read" if params["kind"] == "song" => json!({"epoch": epoch, "layout": 0, "items": [project]}),
            _ => return Some(Err(("stale".into(), "the song changed: read again".into()))),
        }))
    }

    /// A track's volume (dB), mute and devices Kumi added, as Kumi's changes left them.
    pub fn track_state(&self, track: u64) -> (f64, bool, u64) {
        let state = self.state.lock().unwrap();
        (
            state.volume.get(&track).copied().unwrap_or(0.0),
            state.mute.get(&track).copied().unwrap_or(false),
            state.devices.get(&track).copied().unwrap_or(0),
        )
    }

    /// The producer moves a track's volume in Renoise.
    pub fn set_volume(&self, track: u64, db: f64) {
        self.state.lock().unwrap().volume.insert(track, db);
    }

    /// M6: the producer renames a track in Renoise.
    pub fn rename_track(&self, track: u64, name: &str) {
        self.state.lock().unwrap().names.insert(track, name.into());
    }

    /// M6: the producer adds an EQ 10 at the end of a track's devices by hand (the epoch moves, as for any device).
    pub fn add_device_by_hand(&self, track: u64) {
        let mut state = self.state.lock().unwrap();
        *state.devices.entry(track).or_default() += 1;
        state.epoch += 1;
    }

    /// M6: the producer sets the tempo in Renoise.
    pub fn set_bpm(&self, bpm: f64) {
        self.state.lock().unwrap().song["bpm"] = bpm.into();
    }

    /// M6: the digest the tool answered for the demo song, with what was changed since (each track's volume, mute, name and
    /// the devices added to it, the tempo).
    fn digest_now(state: &State, mut page: Value) -> Value {
        let records = page["records"].as_array().cloned().unwrap_or_default();
        let mut out = vec![];
        for mut record in records {
            let track = record["o"].as_u64().unwrap_or(0);
            if record["k"] == "track" {
                if let Some(db) = state.volume.get(&track) {
                    record["f"]["mixer"]["volume"] = json!(db);
                }
                if let Some(muted) = state.mute.get(&track) {
                    record["f"]["mixer"]["mute"] = json!(if *muted { "muted" } else { "active" });
                }
                if let Some(name) = state.names.get(&track) {
                    record["n"] = json!(name);
                    record.as_object_mut().unwrap().remove("dn");
                }
                let added = state.devices.get(&track).copied().unwrap_or(0);
                let before = record["f"]["devices"]["count"].as_u64().unwrap_or(0);
                record["f"]["devices"]["count"] = json!(before + added);
                out.push(record);
                for k in 0..added {
                    out.push(json!({
                        "k": "device", "n": "EQ 10", "o": before + 2 + k, "p": format!("track#/{track}"),
                        "f": {"path": "Audio/Effects/Native/EQ 10", "active": true, "parameters": {"count": 0, "parametersHash": "811c9dc5"}},
                    }));
                }
                continue;
            }
            out.push(record);
        }
        page["records"] = Value::Array(out);
        page["facts"]["bpm"] = state.song["bpm"].clone();
        page
    }

    /// Renoise's undo history, as Kumi's changes named their steps.
    pub fn undo_history(&self) -> Vec<String> {
        self.state.lock().unwrap().undo.clone()
    }

    /// The kinds the simulator makes, as the tool makes them (contract §3b): a track's volume and mute (mute is no
    /// step of Renoise's undo), and a device added at the end of a track, which moves the epoch (Renoise's refs into
    /// device lists are positions) and is deleted again by its restore, moving it once more. A track is found by its
    /// place in any epoch: the tool carries its journal's places through epochs that kept the tracks.
    fn change(state: &mut State, method: &str, params: &Value) -> Result<Value, (String, String)> {
        let bad = |message: &str| ("bad-params".to_string(), message.to_string());
        let place = |text: Option<&str>, kind: &str| -> Option<(u32, Vec<u64>)> {
            let text = text?;
            let mut parts = text.splitn(3, ':');
            let (epoch, found, rest) = (parts.next()?.parse().ok()?, parts.next()?, parts.next()?);
            (found == kind).then(|| (epoch, rest.split('.').filter_map(|n| n.parse().ok()).collect()))
        };
        let kind = params["kind"].as_str().unwrap_or_default();
        let epoch = state.epoch;
        if method != "change_restore" && params.get("epoch").and_then(Value::as_str) != Some(&epoch.to_string()) {
            return Err(("stale".into(), format!("that change is from an earlier epoch (now {epoch}): read again")));
        }
        // Lua run in the song: one step of Renoise's undo, which only Renoise's own undo takes back.
        if kind == "run-lua" {
            let title = "Ran Lua: song.transport.bpm = 140";
            let mut value = match method {
                "change_preview" => {
                    json!({"target": {"ref": format!("{epoch}:song:1"), "kind": "song"}, "prior": {}, "fence": {}, "display": {"title": title}})
                }
                "change_apply" => {
                    let desc = params["desc"].as_str().unwrap_or_default().to_string();
                    state.undo.push(desc.clone());
                    json!({"applied": {"changed": true, "printed": ["set"], "result": 140}, "display": {"title": title}, "undo": {"desc": desc, "entry": true}})
                }
                _ => {
                    return Err((
                        "daw-only".into(),
                        "Kumi can't take back what a Lua chunk did: Renoise's own undo can (Edit > Undo)".into(),
                    ))
                }
            };
            value["epoch"] = epoch.to_string().into();
            value["layout"] = 0.into();
            return Ok(value);
        }
        if matches!(kind, "song-bpm" | "loop-range" | "loop-pattern" | "metronome-switch" | "track-solo") {
            return Self::phrase_change(state, method, params);
        }
        let (reference, input) = match method {
            "change_restore" => (params["target"]["ref"].as_str(), &params["prior"]),
            _ => (params["input"]["ref"].as_str(), &params["input"]),
        };
        let Some((from, at)) = place(reference, "track") else { return Err(bad("ref must be a track's ref")) };
        if method != "change_restore" && !(state.run_start..=epoch).contains(&from) {
            return Err(("stale".into(), format!("that ref is from an earlier epoch ({from}; now {epoch}): read again")));
        }
        let track = at[0];
        let desc = params["desc"].as_str().unwrap_or_default().to_string();
        let answer = |state: &State, value: Value| {
            let mut value = value;
            value["epoch"] = state.epoch.to_string().into();
            value["layout"] = 0.into();
            value
        };
        match kind {
            "track-volume" | "track-mute" => {
                let field = if kind == "track-volume" { "volumeDb" } else { "mute" };
                let now = if kind == "track-volume" {
                    json!(state.volume.get(&track).copied().unwrap_or(0.0))
                } else {
                    json!(state.mute.get(&track).copied().unwrap_or(false))
                };
                let wanted = input.get(field).cloned().ok_or(bad(&format!("{field} is missing")))?;
                let title = if kind == "track-volume" {
                    format!("Track {track} volume {} dB", wanted.as_f64().unwrap_or_default())
                } else {
                    format!("Track {track} muted")
                };
                let write = |state: &mut State, value: &Value| {
                    if kind == "track-volume" {
                        state.volume.insert(track, value.as_f64().unwrap_or_default());
                    } else {
                        state.mute.insert(track, value == &Value::Bool(true));
                    }
                };
                match method {
                    "change_preview" => Ok(answer(
                        state,
                        json!({"target": {"ref": reference, "kind": "track"}, "prior": {field: now}, "fence": {field: now}, "display": {"title": title}}),
                    )),
                    "change_apply" => {
                        if params["fence"][field].as_f64() != now.as_f64() || params["fence"][field].as_bool() != now.as_bool() {
                            return Err(("changed".into(), format!("track {track} changed since the preview")));
                        }
                        write(state, &wanted);
                        // Mute is no step of Renoise's undo: Kumi's undo takes it back, Renoise's doesn't.
                        let entry = kind != "track-mute";
                        if entry {
                            state.undo.push(desc.clone());
                        }
                        let mut display = json!({"title": title});
                        if !entry {
                            display["note"] = "Renoise's own undo won't take this back; Kumi can.".into();
                        }
                        Ok(answer(state, json!({"applied": {field: wanted}, "display": display, "undo": {"desc": desc, "entry": entry}})))
                    }
                    _ => {
                        let applied = &params["applied"][field];
                        if (applied.as_f64() != now.as_f64() || applied.as_bool() != now.as_bool()) && params["discard"] != true {
                            return Err(("changed".into(), format!("track {track} changed in Renoise since Kumi set it")));
                        }
                        write(state, &wanted);
                        state.undo.push(desc);
                        Ok(answer(state, json!({"restored": {field: wanted}, "display": {"title": format!("{title} (put back)")}})))
                    }
                }
            }
            "device-add" => {
                let count = state.devices.get(&track).copied().unwrap_or(0);
                let path = params["input"]["path"].as_str().unwrap_or("EQ 10").to_string();
                match method {
                    "change_preview" => Ok(answer(
                        state,
                        json!({"target": {"ref": reference, "kind": "track"}, "prior": {"devices": count + 1}, "fence": {"devices": count + 1}, "display": {"title": format!("Added “{path}” to “Track {track}”")}}),
                    )),
                    "change_apply" => {
                        state.devices.insert(track, count + 1);
                        state.epoch += 1;
                        state.undo.push(desc.clone());
                        let made = format!("{}:device:{track}.{}", state.epoch, count + 2);
                        Ok(answer(
                            state,
                            // Only the devices moved: the tracks are where they were, and their refs from before still name them.
                            json!({"applied": {"devices": count + 2}, "created": {"ref": made, "kind": "device"}, "display": {"title": format!("Added “{path}” to “Track {track}”")}, "undo": {"desc": desc, "entry": true}, "kept": {"from": epoch.to_string(), "tracks": true, "kinds": KEPT_BY_DEVICES}}),
                        ))
                    }
                    _ => {
                        let Some((made_in, _)) = place(params["created"]["ref"].as_str(), "device") else {
                            return Err(bad("created must be a device's ref"));
                        };
                        if made_in != epoch || count == 0 {
                            return Err(("changed".into(), "the devices changed since Kumi added it".into()));
                        }
                        state.devices.insert(track, count - 1);
                        state.epoch += 1;
                        state.undo.push(desc);
                        Ok(answer(
                            state,
                            json!({"restored": {"devices": count}, "display": {"title": format!("Removed “{path}” from “Track {track}” (put back)")}, "kept": {"from": epoch.to_string(), "tracks": true, "kinds": KEPT_BY_DEVICES}}),
                        ))
                    }
                }
            }
            _ => Err(bad("the simulator makes only track-volume, track-mute and device-add")),
        }
    }

    /// (m6-places) What the fixed phrases change, as the tool makes it (contract §3b), each fenced by its value as the
    /// preview saw it: the song's tempo (`song-bpm {bpm}`, a step of Renoise's undo); the sequence's loop (`loop-range
    /// {first, last}`), the pattern loop (`loop-pattern {on}`), the metronome (`metronome-switch {on}`) and a track's solo
    /// (`track-solo {ref, solo}`), none of them steps of Renoise's undo.
    fn phrase_change(state: &mut State, method: &str, params: &Value) -> Result<Value, (String, String)> {
        let bad = |message: &str| ("bad-params".to_string(), message.to_string());
        let kind = params["kind"].as_str().unwrap_or_default();
        let track = match kind {
            "track-solo" => {
                let reference = if method == "change_restore" { &params["target"]["ref"] } else { &params["input"]["ref"] };
                let at = reference.as_str().and_then(|text| text.rsplit(':').next()).and_then(|n| n.parse::<u64>().ok());
                Some(at.ok_or(bad("ref must be a track's ref"))?)
            }
            _ => None,
        };
        let field = match kind {
            "song-bpm" => "bpm",
            "loop-range" => "loop",
            "loop-pattern" => "patternLoop",
            "metronome-switch" => "metronome",
            _ => "solo",
        };
        let wanted = match (method, kind) {
            ("change_restore", _) => params["prior"][field].clone(),
            (_, "song-bpm") => params["input"]["bpm"].clone(),
            (_, "loop-range") => {
                json!({"first": params["input"]["first"].as_u64().unwrap_or(0), "last": params["input"]["last"].as_u64().unwrap_or(0)})
            }
            (_, "track-solo") => params["input"]["solo"].clone(),
            _ => params["input"]["on"].clone(),
        };
        if wanted.is_null() {
            return Err(bad(&format!("{kind}: its input is missing")));
        }
        let now = match track {
            Some(track) => json!(state.solo.get(&track).copied().unwrap_or(false)),
            None => state.song[field].clone(),
        };
        let on_off = |value: &Value| if value == &Value::Bool(true) { "on" } else { "off" };
        let title = match kind {
            "song-bpm" => format!("Tempo {} BPM", wanted.as_f64().unwrap_or_default()),
            "loop-range" if wanted["first"] == 0 => "Loop cleared".to_string(),
            "loop-range" => format!("Loop slots {} to {}", wanted["first"], wanted["last"]),
            "loop-pattern" => format!("Pattern loop {}", on_off(&wanted)),
            "metronome-switch" => format!("Metronome {}", on_off(&wanted)),
            _ if wanted == true => format!("Track {} soloed", track.unwrap_or(0)),
            _ => format!("Track {} unsoloed", track.unwrap_or(0)),
        };
        let same = |a: &Value, b: &Value| a == b || (a.as_f64().is_some() && a.as_f64() == b.as_f64());
        let write = |state: &mut State, value: &Value| match track {
            Some(track) => {
                state.solo.insert(track, value == &Value::Bool(true));
            }
            None => state.song[field] = value.clone(),
        };
        // Only the tempo is a step of Renoise's undo.
        let entry = kind == "song-bpm";
        let target = match track {
            Some(_) => json!({"ref": params["input"]["ref"], "kind": "track"}),
            None => json!({"ref": format!("{}:song:1", state.epoch), "kind": "song"}),
        };
        let desc = params["desc"].as_str().unwrap_or_default().to_string();
        let mut value = match method {
            "change_preview" => {
                json!({"target": target, "prior": {field: now}, "fence": {field: now}, "display": {"title": title, "unchanged": same(&now, &wanted)}})
            }
            "change_apply" => {
                if !same(&params["fence"][field], &now) {
                    return Err(("changed".into(), format!("the song's {field} changed since the preview")));
                }
                write(state, &wanted);
                if entry {
                    state.undo.push(desc.clone());
                }
                let mut display = json!({"title": title});
                if !entry {
                    display["note"] = "Renoise's own undo won't take this back; Kumi can.".into();
                }
                json!({"applied": {field: wanted}, "display": display, "undo": {"desc": desc, "entry": entry}})
            }
            _ => {
                if same(&now, &wanted) {
                    json!({"restored": {field: wanted}, "display": {"title": format!("{title} (put back)"), "note": "It was back already: nothing was written."}})
                } else if !same(&params["applied"][field], &now) && params["discard"] != true {
                    return Err(("changed".into(), format!("the song's {field} changed in Renoise since Kumi set it")));
                } else {
                    write(state, &wanted);
                    if entry {
                        state.undo.push(desc);
                    }
                    json!({"restored": {field: wanted}, "display": {"title": format!("{title} (put back)")}})
                }
            }
        };
        value["epoch"] = state.epoch.to_string().into();
        value["layout"] = 0.into();
        Ok(value)
    }

    /// (m6-places) What the fixed phrases left: the song's `bpm`, `loop`, `patternLoop` and `metronome`.
    pub fn song(&self) -> Value {
        self.state.lock().unwrap().song.clone()
    }

    /// A track's solo, as Kumi's changes left it.
    pub fn solo(&self, track: u64) -> bool {
        self.state.lock().unwrap().solo.get(&track).copied().unwrap_or(false)
    }

    /// The producer changes one of the song's settings in Renoise.
    pub fn set_song(&self, field: &str, value: Value) {
        self.state.lock().unwrap().song[field] = value;
    }

    /// Every request the simulator answered, in order.
    pub fn asked(&self) -> Vec<(String, Value)> {
        self.state.lock().unwrap().asked.clone()
    }

    pub fn respond(&self) -> Respond {
        let script = self.clone();
        Arc::new(move |method, params| script.answer(method, params))
    }

    /// The simulator's config for Renoise with this song behind it.
    pub fn config(&self, secret: Secret, resource: &str) -> SimulatorConfig {
        SimulatorConfig::renoise(secret, resource).with_methods(&METHODS, self.respond())
    }

    /// The same, and Kumi's changes are made in the song (`change`), and its lines shown in the status bar.
    pub fn config_with_changes(&self, secret: Secret, resource: &str) -> SimulatorConfig {
        self.config(secret, resource).with_methods(&CHANGE_METHODS, self.respond()).with_methods(&[STATUS_MESSAGE], self.respond())
    }

    /// The lines shown in Renoise's status bar, oldest first.
    pub fn statuses(&self) -> Vec<String> {
        self.state.lock().unwrap().statuses.clone()
    }

    fn answer(&self, method: &str, params: &Value) -> Option<Result<Value, (String, String)>> {
        if !METHODS.contains(&method) && !CHANGE_METHODS.contains(&method) && method != STATUS_MESSAGE {
            return None;
        }
        let mut state = self.state.lock().unwrap();
        state.asked.push((method.to_string(), params.clone()));
        if method == STATUS_MESSAGE {
            return Some(match params.get("text").and_then(Value::as_str).filter(|text| !text.trim().is_empty()) {
                Some(text) => {
                    state.statuses.push(text.to_string());
                    Ok(json!({"shown": true}))
                }
                None => Err(("bad-params".into(), "text must be a line of words".into())),
            });
        }
        if state.empty && state.load_during_look && method == "snapshot" {
            state.empty = false;
            state.load_during_look = false;
            return Some(Err(("epoch-changed".into(), "the song changed while this was being read: read again".into())));
        }
        if state.empty {
            return Self::empty_song(&state, method, params);
        }
        if method.starts_with("change_") {
            return Some(Self::change(&mut state, method, params));
        }
        // (r-tracks) Saving the song: the tool's `project_save`, refused from another epoch; Renoise has no versions.
        if method == "project_save" {
            let epoch = state.epoch.to_string();
            return Some(if params.get("epoch").and_then(Value::as_str) != Some(epoch.as_str()) {
                Err(("stale".into(), format!("that save is from another epoch (now {epoch}): read again")))
            } else if params.get("version").is_some_and(|v| !v.is_null()) {
                Err(("bad-params".into(), "Renoise keeps no versions of a song: save it, or under path".into()))
            } else {
                Ok(json!({"epoch": epoch, "saved": "/lab/Renoise/Sim.xrns", "dirty": false}))
            });
        }
        let restarts = state.restarts;
        let epoch = state.epoch;
        let stale = |from: u32| ("stale".to_string(), format!("that ref is from an earlier epoch ({from}; now {epoch}): read again"));
        // The vectors were made in epoch 1. In a later one the same questions have that epoch's refs, and anything from
        // before is refused.
        if epoch != VECTORS_EPOCH {
            if let Some(from) = from_another_epoch(params, epoch) {
                return Some(Err(stale(from)));
            }
        }
        // (the simulator's song never has tracks inserted or removed: its layout is always 0, which is what a request that
        // names one gets answered as; the vectors hold no request with it)
        let mut without_layout = params.clone();
        if let Some(map) = without_layout.as_object_mut() {
            map.remove("layout");
        }
        let asked = retag(&without_layout, epoch, VECTORS_EPOCH, None);
        if let Some(lines) = state.bulk {
            let parent = format!("{VECTORS_EPOCH}:pattern:1");
            if method == "read" && asked["kind"] == "line" && asked["parent"] == parent && asked["track"] == 1 {
                return Some(full_page(lines, &asked, state.keeps_bound).map(|page| retag(&page, VECTORS_EPOCH, epoch, None)));
            }
        }
        // (a digest is asked as the vectors hold it, with values or without: the demo song fits one page, its lines always read)
        let asked = match method {
            "digest" if asked.get("values") == Some(&Value::Bool(true)) => json!({"values": true}),
            "digest" => json!({}),
            _ => asked,
        };
        let wanted = normal(&asked);
        if let Some(case) = state.cases.iter().find(|case| case.method == method && case.params == wanted) {
            let response = case.response.clone().map(|result| restarted(retag(&result, VECTORS_EPOCH, epoch, None), restarts));
            if method == "digest" {
                // (a device edit moves the epoch, not the song: its id and identity are the run's)
                let project = case
                    .response
                    .as_ref()
                    .ok()
                    .map(|page| restarted(retag(&page["project"], VECTORS_EPOCH, state.run_start, None), restarts));
                return Some(response.map(|mut page| {
                    if let Some(project) = project.filter(|p| !p.is_null()) {
                        page["project"] = project;
                    }
                    Self::digest_now(&state, page)
                }));
            }
            return Some(response);
        }
        if let Some(from) = from_another_epoch(params, epoch) {
            return Some(Err(stale(from)));
        }
        Some(Err(("bad-params".into(), format!("the simulator has no answer scripted for {method} {params}"))))
    }
}

/// An answer from a Renoise started `restarts` times since: the song's identity is another process's; its id stays.
fn restarted(value: Value, restarts: u32) -> Value {
    if restarts == 0 {
        return value;
    }
    match value {
        Value::Object(map) => Value::Object(
            map.into_iter()
                .map(|(key, value)| match (key.as_str(), value) {
                    ("identity", Value::String(identity)) => (key, Value::String(format!("{identity}.r{restarts}"))),
                    (_, value) => (key, restarted(value, restarts)),
                })
                .collect(),
        ),
        Value::Array(items) => Value::Array(items.into_iter().map(|item| restarted(item, restarts)).collect()),
        other => other,
    }
}

impl Default for RenoiseScript {
    fn default() -> Self {
        Self::new()
    }
}

/// One page of the very full pattern's lines, as the tool pages them: by limit and cursor (`<epoch>.<layout>.<line>`), cut where
/// the next row would pass the page bound.
fn full_page(lines: usize, params: &Value, keeps_bound: bool) -> Result<Value, (String, String)> {
    let bad = |message: &str| ("bad-params".to_string(), message.to_string());
    let limit = match params.get("limit") {
        None | Some(Value::Null) => DEFAULT_LIMIT,
        Some(value) => match value.as_u64() {
            Some(n) if (1..=MAX_LIMIT as u64).contains(&n) => n as usize,
            _ => return Err(bad("limit must be a whole number from 1 to 200")),
        },
    };
    let start = match params.get("cursor").and_then(Value::as_str) {
        None => 1,
        Some(cursor) => match cursor.split('.').collect::<Vec<_>>()[..] {
            [epoch, "0", position] if epoch == EPOCH => position.parse::<usize>().map_err(|_| bad("not a cursor"))?,
            [_, _, _] => return Err(("stale".into(), "that cursor is from an earlier epoch: read again from the start".into())),
            _ => return Err(bad("not a cursor")),
        },
    };
    let mut items = Vec::new();
    let mut bytes = 0;
    let mut next = None;
    for number in start..=lines {
        let row = full_line(number);
        let size = row.to_string().len() + 1;
        if items.len() >= limit || (keeps_bound && !items.is_empty() && bytes + size > PAGE_BYTES) {
            next = Some(number);
            break;
        }
        bytes += size;
        items.push(row);
    }
    let mut page = json!({"items": items, "epoch": EPOCH});
    if let Some(number) = next {
        page["nextCursor"] = format!("{EPOCH}.0.{number}").into();
    }
    Ok(page)
}
