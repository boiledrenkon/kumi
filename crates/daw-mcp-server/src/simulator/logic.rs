//! A scripted Logic Pro project for the host's tests: what Kumi's own bridge beside Logic (`logic-kumi`) answers to
//! `read`, `snapshot`, `focus` and `focus_body` (the M2 contract, docs/en/LOGIC_CONTRACT.md), replayed verbatim; and
//! what it answers to Kumi's changes (M3): `change_preview`, `change_apply` and `change_restore`, the transport and the
//! playhead, and a plan's steps, counted, since Logic has no undo block.
//!
//! What the bridge answers to the fixed request set of M2's brief over the Scratch project is kept in
//! tests/vectors/logic-reads.json, which the bridge's own tests write from the real Logic over its AX fixtures
//! (KUMI_BLESS=1 in crates/logic-kumi/tests/reads.rs; until they do, the file holds what the hand-written rows of the
//! same project answered). This replays those answers, so the simulator cannot drift from the bridge. The project can be
//! changed, as a track or region added, removed or moved changes it in Logic: the epoch moves, refs and cursors from
//! before are refused as `stale`, and the answers carry the new epoch. Another project can be opened: the epoch moves
//! and the project's id, identity, name and path are another's. A page of the Tracks area nobody recorded (FOCUS asks
//! for a screenful of lanes around the selected track) is cut from the recorded one, as the bridge pages it.
//!
//! Kumi's changes are made on the project as changed over the recorded rows, the way the contract has every kind
//! made: a preview that writes nothing and fences what it saw, an apply that checks the fence again, writes and reads
//! back, a restore by value fenced by what the apply wrote. Each is one step of Logic's own undo, named by the Edit
//! menu's title and by the `desc` the host sent; a track added or deleted moves the epoch, as the M2 replay moves it,
//! and a restore's ref from before is carried to where its track is now. A changed value shows in every read after
//! the change. A dialog (`busy_once`) refuses the next preview before anything is written; a write Logic doesn't show
//! back (`uncertain_once`) is made and answered `uncertain`. The simulator makes the kinds of the first slice whose
//! state is a value: the strip's fader, pan and buttons and its name, tracks added and deleted, markers, the tempo and
//! the cycle area.
use super::replay::{epoch_of, find, load_cases, normal, retag, Case, RECORDED_EPOCH};
use super::{Respond, SimulatorConfig};
use daw_bridge::auth::Secret;
use daw_bridge::wire::Project;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

const VECTORS: &str = include_str!("../../tests/vectors/logic-reads.json");
const VECTORS_NAME: &str = "tests/vectors/logic-reads.json";
/// The timeline body's page, in tracks.
const MAX_LANES: usize = 500;
/// A read's page: the contract's.
const DEFAULT_LIMIT: usize = 50;
const MAX_LIMIT: usize = 200;
const METHODS: [&str; 4] = ["read", "snapshot", "focus", "focus_body"];
/// What the project answers besides: Kumi's changes, the transport and the playhead, and a plan's steps.
const CHANGE_METHODS: [&str; 8] = [
    "change_preview",
    "change_apply",
    "change_restore",
    "transport_stop",
    "transport_play",
    "cursor_set",
    "undo_step_begin",
    "undo_step_end",
];
/// The recorded project's signature: four beats to the bar.
const BEATS_PER_BAR: f64 = 4.0;
/// Logic's resolution: 960 ticks to a beat, a division being a sixteenth of 240 ticks.
const TICKS_PER_BEAT: f64 = 960.0;
const DIVISIONS_PER_BEAT: f64 = 4.0;

/// One of the project's tracks, as Kumi's changes left it: the recorded row (or one made for a track Kumi added) and
/// the values the changes reach.
struct Track {
    /// Tells a track apart through the places it has over the epochs.
    id: u64,
    row: Value,
    name: String,
    kind: String,
    volume_db: f64,
    /// -1 (left) to 1 (right), as the tool has it; the rows show Logic's -64 to 63.
    pan: f64,
    mute: bool,
    solo: bool,
    armed: bool,
}

impl Track {
    /// A recorded row.
    fn recorded(id: u64, row: &Value) -> Track {
        Track {
            id,
            row: row.clone(),
            name: row["name"].as_str().unwrap_or_default().to_string(),
            kind: row["kind"].as_str().unwrap_or("instrument").to_string(),
            volume_db: row["volumeDb"].as_f64().unwrap_or(0.0),
            pan: row["pan"].as_f64().unwrap_or(0.0) / 64.0,
            mute: row["mute"] == true,
            solo: row["solo"] == true,
            armed: row["armed"] == true,
        }
    }

    /// A track Kumi added: the New Tracks dialog's default strip, named.
    fn made(id: u64, name: &str, kind: &str) -> Track {
        let row = json!({
            "armed": false, "color": null, "index": 0, "inputs": if kind == "audio" { json!(["Input 1"]) } else { json!([]) },
            "kind": kind, "monitoring": false, "mute": false, "name": name, "output": "Stereo Output", "pan": 0,
            "ref": "", "selected": false, "solo": false, "volumeDb": 0.0,
        });
        Track { id, row, name: name.into(), kind: kind.into(), volume_db: 0.0, pan: 0.0, mute: false, solo: false, armed: false }
    }

    /// The row as a read shows it at `place` (from 1) in the recorded epoch (the answer is retagged to the one now).
    fn row(&self, place: usize) -> Value {
        let mut row = self.row.clone();
        row["ref"] = format!("{RECORDED_EPOCH}:track:{place}").into();
        row["index"] = place.into();
        row["name"] = self.name.clone().into();
        row["kind"] = self.kind.clone().into();
        row["volumeDb"] = self.volume_db.into();
        row["pan"] = logic_pan(self.pan).into();
        row["mute"] = self.mute.into();
        row["solo"] = self.solo.into();
        row["armed"] = self.armed.into();
        row
    }

    /// What the changes read and write: the field as the tool names it.
    fn get(&self, field: &str) -> Value {
        match field {
            "volumeDb" => json!(self.volume_db),
            "pan" => json!(self.pan),
            "mute" => json!(self.mute),
            "solo" => json!(self.solo),
            "arm" => json!(self.armed),
            _ => json!(self.name),
        }
    }

    fn set(&mut self, field: &str, value: &Value) {
        match field {
            "volumeDb" => self.volume_db = value.as_f64().unwrap_or(self.volume_db),
            "pan" => self.pan = value.as_f64().unwrap_or(self.pan),
            "mute" => self.mute = value == &Value::Bool(true),
            "solo" => self.solo = value == &Value::Bool(true),
            "arm" => self.armed = value == &Value::Bool(true),
            _ => self.name = value.as_str().unwrap_or(&self.name).to_string(),
        }
    }
}

/// Logic's pan, -64 to 63, from the tool's -1 to 1.
fn logic_pan(pan: f64) -> i64 {
    ((pan * 64.0).round() as i64).clamp(-64, 63)
}

#[derive(Clone, PartialEq)]
struct Marker {
    name: String,
    position_beats: f64,
    length: f64,
}

impl Marker {
    fn state(&self) -> Value {
        json!({"positionBeats": self.position_beats, "name": self.name, "length": self.length})
    }
    /// The row as the Marker List reads it, at `place` (from 1).
    fn row(&self, place: usize) -> Value {
        json!({"length": self.length, "name": self.name, "position": position_of(self.position_beats), "ref": format!("{RECORDED_EPOCH}:marker:{place}"), "text": ""})
    }
}

#[derive(Clone, Copy, PartialEq)]
struct Cycle {
    on: bool,
    start: f64,
    end: f64,
}

impl Cycle {
    fn state(self) -> Value {
        json!({"on": self.on, "start": self.start, "end": self.end})
    }
}

/// A track attribute Kumi changes: the input's field, its noun for the words, and the Edit menu's title for its step.
struct Prop {
    kind: &'static str,
    field: &'static str,
    noun: &'static str,
    logic: &'static str,
}

/// The titles Logic's Edit menu is expected to give the steps (the bridge records the real ones).
static TRACK_PROPS: [Prop; 6] = [
    Prop { kind: "track-volume", field: "volumeDb", noun: "volume", logic: "Change Volume" },
    Prop { kind: "track-pan", field: "pan", noun: "pan", logic: "Change Pan" },
    Prop { kind: "track-mute", field: "mute", noun: "mute", logic: "Mute Track" },
    Prop { kind: "track-solo", field: "solo", noun: "solo", logic: "Solo Track" },
    Prop { kind: "track-arm", field: "arm", noun: "record enable", logic: "Record Enable Track" },
    Prop { kind: "track-rename", field: "name", noun: "name", logic: "Rename Track" },
];

struct State {
    /// The epoch the project is in now (the vectors were recorded in [`RECORDED_EPOCH`]).
    epoch: u32,
    /// Which project is open: 0 the recorded one, else another copy of it, with an id and a name of its own.
    project: u32,
    cases: Vec<Case>,
    /// What the simulator was asked, in order: method and params.
    asked: Vec<(String, Value)>,
    /// The project's tracks as Kumi's changes left them, in the Tracks area's order.
    tracks: Vec<Track>,
    /// Which track stood at each place in every epoch before this one: a restore names a track by the place it had
    /// when the change was previewed, and the bridge carries it to where the track is now.
    places: HashMap<u32, Vec<u64>>,
    next_track: u64,
    /// The markers, in the Marker List's order (by position).
    markers: Vec<Marker>,
    tempo: f64,
    cycle: Cycle,
    /// The playhead, in beats from the project's start.
    playhead: f64,
    playing: bool,
    /// Logic's undo history, newest last: the Edit menu's title for each step, and the `desc` the host sent with the
    /// change that made it (none for a write Logic didn't show back: the bridge couldn't name that step).
    undo: Vec<(String, Option<String>)>,
    /// A plan's step, counted (Logic has no undo block): its id and label, and the undo steps when it began.
    open_step: Option<(String, String, usize)>,
    steps: u32,
    /// A dialog shows for the next change preview only (it closes right after).
    busy_once: bool,
    /// The next write (an apply or a restore) is made, and Logic doesn't show the value back.
    uncertain_once: bool,
}

/// A project that answers like the bridge. Clones share the project.
#[derive(Clone)]
pub struct LogicScript {
    state: Arc<Mutex<State>>,
}

type Fault = (String, String);
type Answer = Result<Value, Fault>;

fn bad(message: impl Into<String>) -> Fault {
    ("bad-params".into(), message.into())
}

fn changed(message: impl Into<String>) -> Fault {
    ("changed".into(), message.into())
}

/// A name inside a title's sentence, in curly quotes.
fn quoted(name: &str) -> String {
    format!("\u{201c}{name}\u{201d}")
}

/// Two of a field's values are the same: numbers by their value (-3 and -3.0), anything else exactly.
fn same(a: &Value, b: &Value) -> bool {
    match (a.as_f64(), b.as_f64()) {
        (Some(a), Some(b)) => (a - b).abs() < 1e-9,
        _ => a == b,
    }
}

/// A position as Logic shows it (bar, beat, division and tick, each from 1), from beats.
fn position_of(beats: f64) -> Value {
    let bar = (beats / BEATS_PER_BAR).floor();
    let in_bar = beats - bar * BEATS_PER_BAR;
    let beat = in_bar.floor();
    let in_beat = in_bar - beat;
    let division = (in_beat * DIVISIONS_PER_BEAT).floor();
    let tick = ((in_beat * DIVISIONS_PER_BEAT - division) * TICKS_PER_BEAT / DIVISIONS_PER_BEAT).round();
    json!({"bar": bar as i64 + 1, "beat": beat as i64 + 1, "division": division as i64 + 1, "tick": tick as i64 + 1})
}

/// Beats from the project's start, from a position as Logic shows it.
fn beats_of(position: &Value) -> f64 {
    let number = |key: &str| position.get(key).and_then(Value::as_f64).unwrap_or(1.0);
    (number("bar") - 1.0) * BEATS_PER_BAR
        + (number("beat") - 1.0)
        + (number("division") - 1.0) / DIVISIONS_PER_BEAT
        + (number("tick") - 1.0) / TICKS_PER_BEAT
}

/// Where a time is, as a producer says it: `bar 5`, or `bar 5 beat 2.5` off the bar line.
fn bar_text(beats: f64) -> String {
    let bar = (beats / BEATS_PER_BAR).floor();
    let beat = beats - bar * BEATS_PER_BAR + 1.0;
    if (beat - 1.0).abs() < 0.005 {
        format!("bar {}", bar as i64 + 1)
    } else {
        let text = format!("{beat:.2}");
        format!("bar {} beat {}", bar as i64 + 1, text.trim_end_matches('0').trim_end_matches('.'))
    }
}

fn pan_text(pan: f64) -> String {
    match (pan.abs() * 100.0).round() as i64 {
        0 => "centre".into(),
        n if pan < 0.0 => format!("{n}% left"),
        n => format!("{n}% right"),
    }
}

fn number(value: &Value) -> String {
    let text = format!("{:.2}", value.as_f64().unwrap_or_default());
    text.trim_end_matches('0').trim_end_matches('.').to_string()
}

/// The project the vectors were recorded over, as the bridge's `status` named it: what the welcome and the discovery
/// file carry.
pub fn project() -> Project {
    let cases = load_cases(VECTORS, VECTORS_NAME);
    find(&cases, "status", &json!({}))
        .and_then(|case| case.response.as_ref().ok())
        .and_then(|status| serde_json::from_value(status["project"].clone()).ok())
        .unwrap_or_else(|| Project { name: Some("Scratch".into()), ..Project::default() })
}

/// The project row of `value`, if it holds one (an `id` of 32 hex digits beside a path or an identity), made the
/// `n`th copy of the recorded project: another id, identity, name and path.
fn another_project(value: &Value, n: u32) -> Value {
    if n == 0 {
        return value.clone();
    }
    match value {
        Value::Object(map) => {
            let is_project = map.get("id").and_then(Value::as_str).is_some_and(|id| id.len() == 32)
                && (map.contains_key("path") || map.contains_key("identity"));
            Value::Object(
                map.iter()
                    .map(|(key, field)| {
                        let field = match (is_project, key.as_str(), field.as_str()) {
                            (true, "id", Some(id)) => Value::String(format!("{}{n:02x}", &id[..30])),
                            (true, "identity" | "name", Some(text)) if !text.is_empty() => Value::String(format!("{text} {n}")),
                            (true, "path", Some(path)) if !path.is_empty() => {
                                let (stem, extension) = path.rsplit_once('.').unwrap_or((path, ""));
                                Value::String(if extension.is_empty() { format!("{stem} {n}") } else { format!("{stem} {n}.{extension}") })
                            }
                            _ => another_project(field, n),
                        };
                        (key.clone(), field)
                    })
                    .collect(),
            )
        }
        Value::Array(items) => Value::Array(items.iter().map(|item| another_project(item, n)).collect()),
        other => other.clone(),
    }
}

/// A ref or cursor in `params` from another epoch than `epoch`: the bridge refuses it, whatever it was for.
fn stale_in(params: &Value, epoch: u32) -> Option<(String, String)> {
    for key in ["ref", "parent", "around", "cursor"] {
        let Some(from) = params.get(key).and_then(Value::as_str).and_then(epoch_of).filter(|from| *from != epoch) else { continue };
        let (what, then) = if key == "cursor" { ("cursor", "read again from the start") } else { ("ref", "read again") };
        return Some(("stale".into(), format!("that {what} is from an earlier epoch ({from}; now {epoch}): {then}")));
    }
    None
}

/// Where a page starts: the position a cursor (`<epoch>.<n>`) names, or 0.
fn start_of(params: &Value) -> Result<usize, (String, String)> {
    match params.get("cursor").and_then(Value::as_str) {
        None => Ok(0),
        Some(cursor) => cursor
            .split_once('.')
            .and_then(|(_, n)| n.parse::<usize>().ok())
            .ok_or_else(|| ("bad-params".to_string(), "cursor: pass the nextCursor an earlier read gave, unchanged".to_string())),
    }
}

/// A page of the Tracks area, as the bridge pages it: `limit` lanes from a cursor, or centred on the `around` track
/// (half a page above it), each lane's regions kept to the `window` asked for (the recorded one otherwise).
fn timeline_page(whole: &Value, params: &Value) -> Answer {
    let lanes = whole["lanes"].as_array().cloned().unwrap_or_default();
    let limit = params.get("limit").and_then(Value::as_u64).map_or(MAX_LANES, |n| (n as usize).clamp(1, MAX_LANES));
    let start = match params.get("around").and_then(Value::as_str) {
        Some(around) => {
            let place = lanes
                .iter()
                .position(|lane| lane["trackRef"] == around)
                .ok_or_else(|| ("failed".to_string(), format!("{around} isn't in the project (deleted, or another project)")))?;
            place.saturating_sub(limit / 2).min(lanes.len().saturating_sub(limit))
        }
        None => start_of(params)?,
    };
    let window = match params.get("window") {
        Some(window) => json!({"from": window["from"].as_f64().unwrap_or(0.0), "to": window["to"].as_f64().unwrap_or(f64::MAX)}),
        None => whole["window"].clone(),
    };
    let (from, to) = (window["from"].as_f64().unwrap_or(0.0), window["to"].as_f64().unwrap_or(f64::MAX));
    let page: Vec<Value> = lanes
        .iter()
        .skip(start)
        .take(limit)
        .map(|lane| {
            let mut lane = lane.clone();
            if let Some(regions) = lane["regions"].as_array_mut() {
                regions.retain(|region| {
                    region["startBeats"].as_f64().unwrap_or(0.0) < to && region["endBeats"].as_f64().unwrap_or(0.0) > from
                });
            }
            lane
        })
        .collect();
    let mut body = whole.clone();
    body["window"] = window;
    body["lanes"] = Value::Array(page);
    body["from"] = start.into();
    if let Some(map) = body.as_object_mut() {
        map.remove("nextCursor");
    }
    if start + limit < lanes.len() {
        body["nextCursor"] = format!("{RECORDED_EPOCH}.{}", start + limit).into();
    }
    Ok(body)
}

/// The page of rows `rows` (every row, in order) a read asks for, by limit and cursor; `total` with it.
fn page_of(rows: Vec<Value>, params: &Value) -> Answer {
    let limit = params.get("limit").and_then(Value::as_u64).map_or(DEFAULT_LIMIT, |n| (n as usize).clamp(1, MAX_LIMIT));
    let start = start_of(params)?;
    let items: Vec<Value> = rows.iter().skip(start).take(limit).cloned().collect();
    // (the keys in the bridge's order, as the recorded pages have them)
    let mut page = json!({"epoch": RECORDED_EPOCH.to_string(), "items": items});
    if start + limit < rows.len() {
        page["nextCursor"] = format!("{RECORDED_EPOCH}.{}", start + limit).into();
    }
    page["total"] = rows.len().into();
    Ok(page)
}

impl State {
    /// The project of the vectors: its tracks, markers, tempo and transport as recorded.
    fn recorded() -> State {
        let cases = load_cases(VECTORS, VECTORS_NAME);
        let rows = |kind: &str| -> Vec<Value> {
            find(&cases, "read", &json!({"kind": kind}))
                .and_then(|case| case.response.as_ref().ok())
                .and_then(|page| page["items"].as_array().cloned())
                .unwrap_or_default()
        };
        let tracks: Vec<Track> = rows("track").iter().enumerate().map(|(n, row)| Track::recorded(n as u64 + 1, row)).collect();
        let markers = rows("marker")
            .iter()
            .map(|row| Marker {
                name: row["name"].as_str().unwrap_or_default().to_string(),
                position_beats: beats_of(&row["position"]),
                length: row["length"].as_f64().unwrap_or(0.0),
            })
            .collect();
        let tempo = rows("tempo").first().and_then(|row| row["bpm"].as_f64()).unwrap_or(120.0);
        let focus = find(&cases, "focus", &json!({})).and_then(|case| case.response.as_ref().ok()).cloned().unwrap_or(Value::Null);
        let next_track = tracks.len() as u64 + 1;
        State {
            epoch: RECORDED_EPOCH,
            project: 0,
            cases,
            asked: vec![],
            tracks,
            places: HashMap::new(),
            next_track,
            markers,
            tempo,
            cycle: Cycle { on: focus["transport"]["cycle"] == true, start: 0.0, end: 0.0 },
            playhead: focus["transport"]["beat"].as_f64().unwrap_or(0.0),
            playing: focus["transport"]["playing"] == true,
            undo: vec![],
            open_step: None,
            steps: 0,
            busy_once: false,
            uncertain_once: false,
        }
    }

    fn epoch_text(&self) -> String {
        self.epoch.to_string()
    }

    /// The epoch moves (a track added, deleted or moved): the places of this one are kept, for a restore's ref.
    fn bump_epoch(&mut self) {
        self.places.insert(self.epoch, self.tracks.iter().map(|track| track.id).collect());
        self.epoch += 1;
    }

    /// The track a ref names, by its place now: a ref of the epoch now, or (a restore's) one of an earlier epoch,
    /// carried to where that track is now.
    fn track_at(&self, reference: Option<&str>, carry: bool) -> Result<usize, Fault> {
        let text = reference.ok_or_else(|| bad("ref must be a track ref"))?;
        let mut parts = text.splitn(3, ':');
        let (epoch, kind, place) = (parts.next(), parts.next(), parts.next().and_then(|n| n.parse::<usize>().ok()));
        let (Some(epoch), Some("track"), Some(place)) = (epoch.and_then(|e| e.parse::<u32>().ok()), kind, place) else {
            return Err(bad(format!("{text}: not a track ref")));
        };
        if epoch == self.epoch {
            return (place >= 1 && place <= self.tracks.len())
                .then_some(place - 1)
                .ok_or_else(|| ("failed".to_string(), format!("{text} isn't in the project (deleted, or another project)")));
        }
        if !carry || epoch > self.epoch {
            return Err(("stale".into(), format!("that ref is from an earlier epoch ({epoch}; now {}): read again", self.epoch)));
        }
        let id = self
            .places
            .get(&epoch)
            .and_then(|places| places.get(place.wrapping_sub(1)))
            .ok_or_else(|| ("gone".to_string(), format!("{text} names no track of that epoch")))?;
        self.tracks
            .iter()
            .position(|track| track.id == *id)
            .ok_or_else(|| ("gone".to_string(), format!("the track {text} named isn't in the project any more")))
    }

    /// The marker a ref names, by its place in the list.
    fn marker_at(&self, reference: Option<&str>) -> Result<usize, Fault> {
        let text = reference.ok_or_else(|| bad("ref must be a marker ref"))?;
        let mut parts = text.splitn(3, ':');
        let (_, kind, place) = (parts.next(), parts.next(), parts.next().and_then(|n| n.parse::<usize>().ok()));
        match (kind, place) {
            (Some("marker"), Some(place)) if place >= 1 && place <= self.markers.len() => Ok(place - 1),
            (Some("marker"), Some(_)) => Err(("gone".into(), format!("{text} isn't in the Marker List any more"))),
            _ => Err(bad(format!("{text}: not a marker ref"))),
        }
    }

    /// The marker a restore names: the one at the ref's place, if it is as the apply left it (`applied`), else the one
    /// the apply's values find (a marker's ref is its place in the list, which a move, or a marker added since, may
    /// have changed); None when no marker is as Kumi left it.
    fn marker_of(&self, reference: Option<&str>, applied: &Value) -> Option<usize> {
        let fits =
            |marker: &Marker| applied.as_object().is_some_and(|fields| fields.iter().all(|(key, value)| same(&marker.state()[key], value)));
        if let Ok(at) = self.marker_at(reference) {
            if fits(&self.markers[at]) {
                return Some(at);
            }
        }
        self.markers.iter().position(fits)
    }

    /// The marker's ref at its place in the list now.
    fn marker_ref(&self, marker: &Marker) -> String {
        let place = self.markers.iter().position(|m| m == marker).map_or(0, |at| at + 1);
        format!("{}:marker:{place}", self.epoch)
    }

    fn sort_markers(&mut self) {
        self.markers.sort_by(|a, b| a.position_beats.total_cmp(&b.position_beats));
    }

    /// A write made: its step in Logic's undo (the Edit menu's title, and the host's `desc`), as the apply's `undo`;
    /// or, when Logic doesn't show the value back, the bridge's `uncertain` (the step is Logic's, unnamed to Kumi).
    fn step(&mut self, logic: &str, desc: &str) -> Result<Value, Fault> {
        let title = format!("Undo {logic}");
        if std::mem::take(&mut self.uncertain_once) {
            self.undo.push((title, None));
            return Err(("uncertain".into(), "Logic Pro didn't show the value after the write: it may or may not have taken it".into()));
        }
        self.undo.push((title.clone(), Some(desc.to_string())));
        Ok(json!({"desc": desc, "entry": true, "title": title}))
    }

    /// A write that is no step of Logic's undo (the cycle area): the host's `desc`, and that Logic's undo won't have it.
    fn no_step(&mut self, desc: &str) -> Result<Value, Fault> {
        if std::mem::take(&mut self.uncertain_once) {
            return Err(("uncertain".into(), "Logic Pro didn't show the value after the write: it may or may not have taken it".into()));
        }
        Ok(json!({"desc": desc, "entry": false}))
    }

    // ----- the changes -----

    fn change(&mut self, method: &str, params: &Value) -> Answer {
        if method == "change_preview" && std::mem::take(&mut self.busy_once) {
            return Err(("busy".into(), "a dialog is up in Logic Pro; nothing changed".into()));
        }
        // Refs from before the project changed are refused for a preview or an apply, as the bridge refuses them
        // whatever they were for; a restore names the change's own place, carried to where its track is now.
        if method != "change_restore" {
            if let Some(sent) = params.get("epoch").and_then(Value::as_str).filter(|sent| *sent != self.epoch_text()) {
                return Err((
                    "stale".into(),
                    format!("those refs are from an earlier epoch ({sent}; now {}): the project changed, read again", self.epoch),
                ));
            }
        }
        let kind = params["kind"].as_str().unwrap_or_default().to_string();
        match kind.as_str() {
            "track-add" => self.track_add(method, params),
            "track-delete" => self.track_delete(method, params),
            "marker-add" => self.marker_add(method, params),
            "marker-move" | "marker-rename" => self.marker_prop(&kind, method, params),
            "marker-delete" => self.marker_delete(method, params),
            "tempo-set" => self.tempo_set(method, params),
            "loop-range" => self.loop_range(method, params),
            _ => match TRACK_PROPS.iter().find(|prop| prop.kind == kind) {
                Some(prop) => self.track_prop(prop, method, params),
                None => Err(bad(format!(
                    "the simulated Logic Pro makes only {} (not {kind:?})",
                    "track-volume, track-pan, track-mute, track-solo, track-arm, track-rename, track-add, track-delete, marker-add, marker-move, marker-rename, marker-delete, tempo-set and loop-range"
                ))),
            },
        }
    }

    fn desc(params: &Value) -> Result<String, Fault> {
        params["desc"].as_str().map(str::to_owned).ok_or_else(|| bad("desc must be a string"))
    }

    /// A track's attribute: the title of the change, from the track's name and the value it goes to.
    fn prop_title(prop: &Prop, name: &str, from: &Value, to: &Value) -> String {
        match prop.field {
            "volumeDb" => format!("{name} volume {} dB", number(to)),
            "pan" => format!("{name} pan {}", pan_text(to.as_f64().unwrap_or(0.0))),
            "mute" => format!("{name} {}", if to == true { "muted" } else { "unmuted" }),
            "solo" => format!("{name} {}", if to == true { "soloed" } else { "unsoloed" }),
            "arm" => format!("{name} {}", if to == true { "armed for recording" } else { "disarmed" }),
            _ => format!("Renamed track {} → {}", quoted(from.as_str().unwrap_or_default()), quoted(to.as_str().unwrap_or_default())),
        }
    }

    fn prop_wanted(prop: &Prop, input: &Value) -> Result<Value, Fault> {
        let value = input.get(prop.field).filter(|value| !value.is_null()).ok_or_else(|| bad(format!("{} is missing", prop.field)))?;
        let fits = match prop.field {
            "volumeDb" => value.as_f64().is_some_and(|db| (-96.0..=6.0).contains(&db)),
            "pan" => value.as_f64().is_some_and(|pan| (-1.0..=1.0).contains(&pan)),
            "mute" | "solo" | "arm" => value.is_boolean(),
            _ => value.as_str().is_some_and(|name| !name.is_empty() && name.chars().count() <= 256),
        };
        if !fits {
            return Err(bad(format!("{} isn't a value Logic Pro's strip takes", prop.field)));
        }
        Ok(value.clone())
    }

    fn track_prop(&mut self, prop: &Prop, method: &str, params: &Value) -> Answer {
        let (reference, input) = match method {
            "change_restore" => (params["target"]["ref"].as_str(), &params["prior"]),
            _ => (params["input"]["ref"].as_str(), &params["input"]),
        };
        let at = self.track_at(reference, method == "change_restore")?;
        let (now, name) = (self.tracks[at].get(prop.field), self.tracks[at].name.clone());
        let wanted = Self::prop_wanted(prop, input)?;
        let title = Self::prop_title(prop, &name, &now, &wanted);
        let (epoch, field) = (self.epoch_text(), prop.field);
        match method {
            "change_preview" => {
                let mut display = json!({"title": title, "before": {field: now}, "after": {field: wanted}});
                if same(&now, &wanted) {
                    display["unchanged"] = true.into();
                }
                Ok(
                    json!({"epoch": epoch, "target": {"ref": reference, "kind": "track"}, "prior": {field: now}, "fence": {field: now}, "display": display}),
                )
            }
            "change_apply" => {
                if !same(&params["fence"][field], &now) {
                    return Err(changed(format!("{name}'s {} changed since the preview", prop.noun)));
                }
                let desc = Self::desc(params)?;
                self.tracks[at].set(field, &wanted);
                let after = self.tracks[at].get(field);
                let undo = self.step(prop.logic, &desc)?;
                Ok(
                    json!({"epoch": epoch, "applied": {field: after}, "display": {"title": title, "before": {field: now}, "after": {field: after}}, "undo": undo}),
                )
            }
            _ => {
                if same(&now, &wanted) {
                    return Ok(json!({"epoch": epoch, "restored": {field: wanted}, "display": {"title": format!("{title} (put back)")}}));
                }
                if !same(&params["applied"][field], &now) && params["discard"] != true {
                    return Err(changed(format!("{name}'s {} changed in Logic Pro since Kumi set it", prop.noun)));
                }
                let desc = Self::desc(params)?;
                self.tracks[at].set(field, &wanted);
                let after = self.tracks[at].get(field);
                let undo = self.step(prop.logic, &desc)?;
                Ok(json!({"epoch": epoch, "restored": {field: after}, "display": {"title": format!("{title} (put back)")}, "undo": undo}))
            }
        }
    }

    /// `track-add {name?, kind?}`: a track at the end of the Tracks area (the New Tracks dialog), which moves the epoch;
    /// its ref in `created`. Its restore deletes it, unless it's gone.
    fn track_add(&mut self, method: &str, params: &Value) -> Answer {
        let input = &params["input"];
        let kind = match input.get("kind").filter(|kind| !kind.is_null()) {
            None => "instrument".to_string(),
            Some(Value::String(kind)) if kind == "audio" || kind == "instrument" => kind.clone(),
            Some(_) => return Err(bad("kind must be audio or instrument")),
        };
        let count = self.tracks.len();
        let name = match input.get("name").filter(|name| !name.is_null()) {
            None => None,
            Some(Value::String(name)) if name.chars().count() <= 256 => Some(name.clone()),
            Some(_) => return Err(bad("name must be text of at most 256 characters")),
        };
        let title = |name: &Option<String>| match name {
            Some(name) => format!("Added track {}", quoted(name)),
            None => "Added a track".to_string(),
        };
        match method {
            "change_preview" => Ok(json!({
                "epoch": self.epoch_text(),
                "target": {"ref": null, "kind": "track"},
                "prior": {"tracks": count},
                "fence": {"tracks": count},
                "display": {"title": title(&name), "index": count + 1},
            })),
            "change_apply" => {
                if params["fence"]["tracks"].as_u64() != Some(count as u64) {
                    return Err(changed("the tracks changed since the preview"));
                }
                let desc = Self::desc(params)?;
                let named = name.clone().unwrap_or_else(|| format!("{} {}", if kind == "audio" { "Audio" } else { "Inst" }, count + 1));
                let made = Track::made(self.next_track, &named, &kind);
                self.next_track += 1;
                self.tracks.push(made);
                self.bump_epoch();
                let reference = format!("{}:track:{}", self.epoch, count + 1);
                let undo = self.step("Create Track", &desc)?;
                Ok(json!({
                    "epoch": self.epoch_text(),
                    "applied": {"index": count + 1, "name": named, "kind": kind},
                    "created": {"ref": reference, "kind": "track"},
                    "display": {"title": format!("Added track {}", quoted(&named))},
                    "undo": undo,
                }))
            }
            _ => {
                let at = self.track_at(params["created"]["ref"].as_str(), true)?;
                let desc = Self::desc(params)?;
                let gone = self.tracks.remove(at);
                self.bump_epoch();
                let undo = self.step("Delete Track", &desc)?;
                Ok(json!({
                    "epoch": self.epoch_text(),
                    "restored": {"deleted": params["created"]["ref"]},
                    "display": {"title": format!("{} taken away again", quoted(&gone.name))},
                    "undo": undo,
                }))
            }
        }
    }

    /// `track-delete {ref}`: the track goes (the key command, its confirmation answered), which moves the epoch; what it
    /// was is kept in `material`. Its restore is Logic's own undo (Cmd-Z), which the bridge sends only while the Edit
    /// menu's Undo title is this very step: a track can't be made again by value with what it held.
    fn track_delete(&mut self, method: &str, params: &Value) -> Answer {
        if method == "change_restore" {
            let material = &params["material"];
            let desc = Self::desc(params)?;
            let top = self.undo.last().and_then(|(_, desc)| desc.clone());
            if top.is_none() || top != material["step"].as_str().map(str::to_owned) {
                return Err((
                    "failed".into(),
                    "Logic Pro's undo has other steps on top of this delete, so Kumi can't put the track back; Cmd-Z in Logic Pro can, step by step".into(),
                ));
            }
            if std::mem::take(&mut self.uncertain_once) {
                self.undo.pop();
                return Err((
                    "uncertain".into(),
                    "Logic Pro's Undo was pressed and the track doesn't show yet: it may or may not be back".into(),
                ));
            }
            self.undo.pop();
            let index = (material["index"].as_u64().unwrap_or(1) as usize).clamp(1, self.tracks.len() + 1);
            let mut track = Track::made(
                self.next_track,
                material["name"].as_str().unwrap_or_default(),
                material["kind"].as_str().unwrap_or("instrument"),
            );
            self.next_track += 1;
            for field in ["volumeDb", "pan", "mute", "solo", "arm"] {
                track.set(field, &material[field]);
            }
            let name = track.name.clone();
            self.tracks.insert(index - 1, track);
            self.bump_epoch();
            return Ok(json!({
                "epoch": self.epoch_text(),
                "restored": {"ref": format!("{}:track:{index}", self.epoch), "was": params["target"]["ref"]},
                "display": {"title": format!("{} put back", quoted(&name))},
                "undo": {"desc": desc, "entry": false, "how": "undo"},
            }));
        }
        let at = self.track_at(params["input"]["ref"].as_str(), false)?;
        let track = &self.tracks[at];
        let (name, fence) = (track.name.clone(), json!({"name": track.name, "index": at + 1}));
        let title = format!("Deleted track {}", quoted(&name));
        match method {
            "change_preview" => Ok(json!({
                "epoch": self.epoch_text(),
                "target": {"ref": params["input"]["ref"], "kind": "track"},
                "prior": {"name": name, "kind": track.kind, "index": at + 1},
                "fence": fence,
                "display": {"title": title},
            })),
            _ => {
                if params["fence"] != fence {
                    return Err(changed(format!("{name} changed since the preview")));
                }
                let desc = Self::desc(params)?;
                let material = json!({
                    "name": track.name, "kind": track.kind, "index": at + 1, "volumeDb": track.volume_db, "pan": track.pan,
                    "mute": track.mute, "solo": track.solo, "arm": track.armed, "step": desc,
                });
                self.tracks.remove(at);
                self.bump_epoch();
                let undo = self.step("Delete Track", &desc)?;
                Ok(
                    json!({"epoch": self.epoch_text(), "applied": {"deleted": true}, "material": material, "display": {"title": title}, "undo": undo}),
                )
            }
        }
    }

    fn marker_add(&mut self, method: &str, params: &Value) -> Answer {
        let input = &params["input"];
        let position = match input.get("positionBeats").filter(|p| !p.is_null()) {
            None => self.playhead,
            Some(value) => value.as_f64().filter(|p| *p >= 0.0).ok_or_else(|| bad("positionBeats must be a number from 0"))?,
        };
        let name = match input.get("name").filter(|name| !name.is_null()) {
            None => None,
            Some(Value::String(name)) if !name.is_empty() && name.chars().count() <= 256 => Some(name.clone()),
            Some(_) => return Err(bad("name must be text of at most 256 characters")),
        };
        let title = |name: &str| format!("Marker {} at {}", quoted(name), bar_text(position));
        match method {
            "change_preview" => Ok(json!({
                "epoch": self.epoch_text(),
                "target": {"ref": null, "kind": "marker"},
                "prior": {"markers": self.markers.len()},
                "fence": {},
                "display": {"title": name.as_deref().map(title).unwrap_or_else(|| format!("Marker at {}", bar_text(position)))},
            })),
            "change_apply" => {
                let desc = Self::desc(params)?;
                let named = name.unwrap_or_else(|| format!("Marker {}", self.markers.len() + 1));
                let marker = Marker { name: named.clone(), position_beats: position, length: 4.0 };
                self.markers.push(marker.clone());
                self.sort_markers();
                let reference = self.marker_ref(&marker);
                let undo = self.step("Create Marker", &desc)?;
                Ok(json!({
                    "epoch": self.epoch_text(),
                    "applied": marker.state(),
                    "created": {"ref": reference, "kind": "marker"},
                    "display": {"title": title(&named)},
                    "undo": undo,
                }))
            }
            _ => {
                // (deleting whatever is at the place now would be no put-back: even told to discard, the marker must
                // be the one Kumi added, as Kumi left it)
                let Some(at) = self.marker_of(params["created"]["ref"].as_str(), &params["applied"]) else {
                    return Err(changed("no marker is as Kumi added it (moved, renamed or deleted in Logic Pro since)"));
                };
                let desc = Self::desc(params)?;
                let gone = self.markers.remove(at);
                let undo = self.step("Delete Marker", &desc)?;
                Ok(json!({
                    "epoch": self.epoch_text(),
                    "restored": {"deleted": params["created"]["ref"]},
                    "display": {"title": format!("Marker {} taken away again", quoted(&gone.name))},
                    "undo": undo,
                }))
            }
        }
    }

    /// `marker-move {ref, positionBeats}` and `marker-rename {ref, name}`: one field of a marker.
    fn marker_prop(&mut self, kind: &str, method: &str, params: &Value) -> Answer {
        let (field, noun, logic) =
            if kind == "marker-move" { ("positionBeats", "place", "Move Marker") } else { ("name", "name", "Rename Marker") };
        let (reference, input) = match method {
            "change_restore" => (params["target"]["ref"].as_str(), &params["prior"]),
            _ => (params["input"]["ref"].as_str(), &params["input"]),
        };
        let at = if method == "change_restore" {
            match self.marker_of(reference, &params["applied"]) {
                Some(at) => at,
                None if params["discard"] == true => self.marker_at(reference)?,
                None => return Err(changed(format!("no marker's {noun} is as Kumi set it (changed in Logic Pro since)"))),
            }
        } else {
            self.marker_at(reference)?
        };
        let marker = self.markers[at].clone();
        let now = if field == "name" { json!(marker.name) } else { json!(marker.position_beats) };
        let wanted = input.get(field).filter(|v| !v.is_null()).cloned().ok_or_else(|| bad(format!("{field} is missing")))?;
        let fits = if field == "name" { wanted.as_str().is_some_and(|n| !n.is_empty()) } else { wanted.as_f64().is_some_and(|p| p >= 0.0) };
        if !fits {
            return Err(bad(format!("{field} isn't a value the Marker List takes")));
        }
        let title = if field == "name" {
            format!("Renamed marker {} → {}", quoted(&marker.name), quoted(wanted.as_str().unwrap_or_default()))
        } else {
            format!("Moved marker {} to {}", quoted(&marker.name), bar_text(wanted.as_f64().unwrap_or(0.0)))
        };
        let epoch = self.epoch_text();
        // (a marker moved may change its place in the list: the one written is read back where it is now)
        let (name, position) = (marker.name.clone(), marker.position_beats);
        let write = |state: &mut State, value: &Value| {
            if field == "name" {
                state.markers[at].name = value.as_str().unwrap_or_default().to_string();
            } else {
                state.markers[at].position_beats = value.as_f64().unwrap_or(0.0);
                state.sort_markers();
            }
        };
        let read_back = move |state: &State| -> Value {
            let written = if field == "name" {
                state
                    .markers
                    .iter()
                    .find(|m| m.position_beats == position && m.name != name)
                    .or_else(|| state.markers.iter().find(|m| m.position_beats == position))
            } else {
                state.markers.iter().find(|m| m.name == name)
            };
            match written {
                Some(m) if field == "name" => json!(m.name),
                Some(m) => json!(m.position_beats),
                None => Value::Null,
            }
        };
        match method {
            "change_preview" => {
                let mut display = json!({"title": title, "before": {field: now}, "after": {field: wanted}});
                if same(&now, &wanted) {
                    display["unchanged"] = true.into();
                }
                Ok(
                    json!({"epoch": epoch, "target": {"ref": reference, "kind": "marker"}, "prior": {field: now}, "fence": {field: now}, "display": display}),
                )
            }
            "change_apply" => {
                if !same(&params["fence"][field], &now) {
                    return Err(changed(format!("Marker {}'s {noun} changed since the preview", quoted(&marker.name))));
                }
                let desc = Self::desc(params)?;
                write(self, &wanted);
                let after = read_back(self);
                let undo = self.step(logic, &desc)?;
                Ok(
                    json!({"epoch": epoch, "applied": {field: after}, "display": {"title": title, "before": {field: now}, "after": {field: after}}, "undo": undo}),
                )
            }
            _ => {
                if same(&now, &wanted) {
                    return Ok(json!({"epoch": epoch, "restored": {field: wanted}, "display": {"title": format!("{title} (put back)")}}));
                }
                if !same(&params["applied"][field], &now) && params["discard"] != true {
                    return Err(changed(format!("Marker {}'s {noun} changed in Logic Pro since Kumi set it", quoted(&marker.name))));
                }
                let desc = Self::desc(params)?;
                write(self, &wanted);
                let after = read_back(self);
                let undo = self.step(logic, &desc)?;
                Ok(json!({"epoch": epoch, "restored": {field: after}, "display": {"title": format!("{title} (put back)")}, "undo": undo}))
            }
        }
    }

    fn marker_delete(&mut self, method: &str, params: &Value) -> Answer {
        if method == "change_restore" {
            let material = &params["material"];
            let marker = Marker {
                name: material["state"]["name"].as_str().unwrap_or_default().to_string(),
                position_beats: material["state"]["positionBeats"].as_f64().unwrap_or(0.0),
                length: material["state"]["length"].as_f64().unwrap_or(0.0),
            };
            let desc = Self::desc(params)?;
            self.markers.push(marker.clone());
            self.sort_markers();
            let reference = self.marker_ref(&marker);
            let undo = self.step("Create Marker", &desc)?;
            return Ok(json!({
                "epoch": self.epoch_text(),
                "restored": {"ref": reference, "was": material["was"]},
                "display": {"title": format!("Marker {} put back", quoted(&marker.name)), "note": "Logic Pro numbers a marker made again by its place in the list."},
                "undo": undo,
            }));
        }
        let at = self.marker_at(params["input"]["ref"].as_str())?;
        let marker = self.markers[at].clone();
        let title = format!("Deleted marker {}", quoted(&marker.name));
        match method {
            "change_preview" => Ok(json!({
                "epoch": self.epoch_text(),
                "target": {"ref": params["input"]["ref"], "kind": "marker"},
                "prior": marker.state(),
                "fence": marker.state(),
                "display": {"title": title},
            })),
            _ => {
                if params["fence"] != marker.state() {
                    return Err(changed(format!("Marker {} changed since the preview", quoted(&marker.name))));
                }
                let desc = Self::desc(params)?;
                self.markers.remove(at);
                let undo = self.step("Delete Marker", &desc)?;
                Ok(json!({
                    "epoch": self.epoch_text(),
                    "applied": {"deleted": true},
                    "material": {"state": marker.state(), "was": params["input"]["ref"]},
                    "display": {"title": title},
                    "undo": undo,
                }))
            }
        }
    }

    fn tempo_set(&mut self, method: &str, params: &Value) -> Answer {
        let input = if method == "change_restore" { &params["prior"] } else { &params["input"] };
        let wanted = input
            .get("bpm")
            .and_then(Value::as_f64)
            .filter(|bpm| (5.0..=990.0).contains(bpm))
            .ok_or_else(|| bad("bpm must be a number from 5 to 990"))?;
        let now = self.tempo;
        let (epoch, title) = (self.epoch_text(), format!("Tempo {} BPM", number(&json!(wanted))));
        match method {
            "change_preview" => {
                let mut display = json!({"title": title, "before": {"bpm": now}, "after": {"bpm": wanted}});
                if same(&json!(now), &json!(wanted)) {
                    display["unchanged"] = true.into();
                }
                Ok(
                    json!({"epoch": epoch, "target": {"ref": null, "kind": "project"}, "prior": {"bpm": now}, "fence": {"bpm": now}, "display": display}),
                )
            }
            "change_apply" => {
                if !same(&params["fence"]["bpm"], &json!(now)) {
                    return Err(changed("the tempo changed since the preview"));
                }
                let desc = Self::desc(params)?;
                self.tempo = wanted;
                let undo = self.step("Change Tempo", &desc)?;
                Ok(json!({"epoch": epoch, "applied": {"bpm": wanted}, "display": {"title": title}, "undo": undo}))
            }
            _ => {
                let title = format!("Tempo back to {} BPM", number(&json!(wanted)));
                if same(&json!(now), &json!(wanted)) {
                    return Ok(json!({"epoch": epoch, "restored": {"bpm": wanted}, "display": {"title": title}}));
                }
                if !same(&params["applied"]["bpm"], &json!(now)) && params["discard"] != true {
                    return Err(changed("the tempo has been changed in Logic Pro since Kumi set it"));
                }
                let desc = Self::desc(params)?;
                self.tempo = wanted;
                let undo = self.step("Change Tempo", &desc)?;
                Ok(json!({"epoch": epoch, "restored": {"bpm": wanted}, "display": {"title": title}, "undo": undo}))
            }
        }
    }

    /// `loop-range {startBeats, endBeats}`: the cycle area and Cycle on; 0 and 0 (or neither) turn Cycle off. No step of
    /// Logic's undo: Kumi's undo takes it back, Logic's doesn't.
    fn loop_range(&mut self, method: &str, params: &Value) -> Answer {
        let wanted = if method == "change_restore" {
            let prior = &params["prior"];
            Cycle { on: prior["on"] == true, start: prior["start"].as_f64().unwrap_or(0.0), end: prior["end"].as_f64().unwrap_or(0.0) }
        } else {
            let input = &params["input"];
            let beats = |key: &str| -> Result<f64, Fault> {
                match input.get(key).filter(|v| !v.is_null()) {
                    None => Ok(0.0),
                    Some(value) => value.as_f64().filter(|b| *b >= 0.0).ok_or_else(|| bad(format!("{key} must be a number from 0"))),
                }
            };
            let (start, end) = (beats("startBeats")?, beats("endBeats")?);
            if end > 0.0 && end <= start {
                return Err(bad("the cycle area must end after it starts"));
            }
            Cycle { on: end > 0.0, start, end }
        };
        let now = self.cycle;
        let title = |cycle: Cycle| {
            if cycle.on {
                format!("Cycle from {} to {}", bar_text(cycle.start), bar_text(cycle.end))
            } else {
                "Cycle off".to_string()
            }
        };
        let epoch = self.epoch_text();
        let note = "Logic Pro's own undo won't take this back; Kumi can.";
        match method {
            "change_preview" => {
                let mut display = json!({"title": title(wanted), "before": now.state(), "after": wanted.state(), "note": note});
                if now == wanted {
                    display["unchanged"] = true.into();
                }
                Ok(
                    json!({"epoch": epoch, "target": {"ref": null, "kind": "project"}, "prior": now.state(), "fence": now.state(), "display": display}),
                )
            }
            "change_apply" => {
                if params["fence"] != now.state() {
                    return Err(changed("the cycle area changed since the preview"));
                }
                let desc = Self::desc(params)?;
                self.cycle = wanted;
                let undo = self.no_step(&desc)?;
                Ok(json!({"epoch": epoch, "applied": wanted.state(), "display": {"title": title(wanted), "note": note}, "undo": undo}))
            }
            _ => {
                if now == wanted {
                    return Ok(
                        json!({"epoch": epoch, "restored": wanted.state(), "display": {"title": format!("{} (put back)", title(wanted))}}),
                    );
                }
                if params["applied"] != now.state() && params["discard"] != true {
                    return Err(changed("the cycle area has been changed in Logic Pro since Kumi set it"));
                }
                let desc = Self::desc(params)?;
                self.cycle = wanted;
                let undo = self.no_step(&desc)?;
                Ok(
                    json!({"epoch": epoch, "restored": wanted.state(), "display": {"title": format!("{} (put back)", title(wanted))}, "undo": undo}),
                )
            }
        }
    }

    // ----- the transport and the plan's steps -----

    fn transport(&mut self, method: &str, params: &Value) -> Answer {
        match method {
            "transport_stop" => {
                let was = std::mem::replace(&mut self.playing, false);
                Ok(json!({"stopped": was, "wasRecording": false}))
            }
            "transport_play" => {
                let was = std::mem::replace(&mut self.playing, true);
                Ok(json!({"started": !was, "playing": true}))
            }
            _ => {
                if let Some(sent) = params.get("epoch").and_then(Value::as_str).filter(|sent| *sent != self.epoch_text()) {
                    return Err(("stale".into(), format!("those refs are from an earlier epoch ({sent}; now {}): read again", self.epoch)));
                }
                let seconds_per_beat = 60.0 / self.tempo;
                let beats = match (params.get("positionBeats").and_then(Value::as_f64), params.get("position").and_then(Value::as_f64)) {
                    (Some(_), Some(_)) => return Err(bad("pass positionBeats or position, not both")),
                    (Some(beats), None) if beats >= 0.0 => beats,
                    (None, Some(seconds)) if seconds >= 0.0 => seconds / seconds_per_beat,
                    _ => return Err(bad("pass positionBeats (beats from the project's start) or position (seconds)")),
                };
                self.playhead = beats;
                Ok(
                    json!({"epoch": self.epoch_text(), "positionBeats": beats, "position": beats * seconds_per_beat, "playing": self.playing}),
                )
            }
        }
    }

    /// `undo_step_begin {label}` / `undo_step_end {stepId, label?}`: a plan's step, counted. Logic has no undo block:
    /// the changes made meanwhile are each a step of its undo, and the end says how many.
    fn plan_step(&mut self, method: &str, params: &Value) -> Answer {
        let label = params.get("label").and_then(Value::as_str).map(str::to_owned);
        if method == "undo_step_begin" {
            let label = label.ok_or_else(|| bad("label must be a string"))?;
            if self.open_step.is_some() {
                return Err(("failed".into(), "a step is open already: end it before beginning another".into()));
            }
            self.steps += 1;
            let id = format!("s{}", self.steps);
            self.open_step = Some((id.clone(), label, self.undo.len()));
            return Ok(json!({"stepId": id}));
        }
        let id = params.get("stepId").and_then(Value::as_str).ok_or_else(|| bad("stepId must be a string"))?;
        let (_, begun, at) = match self.open_step.as_ref() {
            Some((open, _, _)) if open == id => self.open_step.take().expect("the step"),
            _ => return Err(("failed".into(), "that step isn't open (it ended already, or the project changed under it)".into())),
        };
        let desc = label.unwrap_or(begun);
        Ok(json!({"desc": desc, "entry": false, "changes": self.undo.len() - at}))
    }

    // ----- the reads, over the project as changed -----

    /// A read's rows that Kumi's changes reach, made from the project as changed over the recorded rows: a page of the
    /// tracks or of the markers, one track by its ref, and the project's, the transport's and the bodies' fields; None
    /// for a read the changes don't touch, which the recorded case answers. `asked` is in the recorded epoch.
    fn changed_rows(&self, method: &str, asked: &Value, params: &Value) -> Option<Answer> {
        let recorded = |method: &str, params: Value| {
            find(&self.cases, method, &params)
                .and_then(|case| case.response.as_ref().ok())
                .cloned()
                .ok_or_else(|| ("bad-params".to_string(), format!("the simulator has no answer scripted for {method} {params}")))
        };
        let kind = asked.get("kind").and_then(Value::as_str);
        let track_rows = || -> Vec<Value> { self.tracks.iter().enumerate().map(|(at, track)| track.row(at + 1)).collect() };
        let marker_rows = || -> Vec<Value> { self.markers.iter().enumerate().map(|(at, marker)| marker.row(at + 1)).collect() };
        let patch_transport = |transport: &mut Value| {
            transport["playing"] = self.playing.into();
            transport["cycle"] = self.cycle.on.into();
            transport["tempo"] = self.tempo.into();
        };
        let playhead = || {
            let position = position_of(self.playhead);
            json!({"bar": position["bar"], "beat": position["beat"]})
        };
        Some(match method {
            "read" if asked.get("ref").is_some() => {
                let reference = asked["ref"].as_str().unwrap_or_default();
                let (_, rest) = reference.split_once(':')?;
                if !rest.starts_with("track:") || epoch_of(reference) != Some(RECORDED_EPOCH) {
                    return None;
                }
                match rest["track:".len()..].parse::<usize>().ok().filter(|place| *place >= 1 && *place <= self.tracks.len()) {
                    Some(place) => Ok(json!({"epoch": RECORDED_EPOCH.to_string(), "items": [self.tracks[place - 1].row(place)]})),
                    None => Err((
                        "failed".to_string(),
                        format!("{} isn't in the project (deleted, or another project)", params["ref"].as_str().unwrap_or_default()),
                    )),
                }
            }
            "read" if kind == Some("track") && asked.get("parent").is_none() => page_of(track_rows(), asked),
            "read" if kind == Some("marker") && asked.get("parent").is_none() => page_of(marker_rows(), asked),
            "read" if kind == Some("project") => recorded("read", asked.clone()).map(|mut page| {
                page["items"][0]["tempo"] = self.tempo.into();
                page["items"][0]["trackCount"] = self.tracks.len().into();
                page
            }),
            "read" if kind == Some("tempo") => recorded("read", asked.clone()).map(|mut page| {
                page["items"][0]["bpm"] = self.tempo.into();
                page
            }),
            "snapshot" => recorded("snapshot", asked.clone()).map(|mut snapshot| {
                snapshot["project"]["tempo"] = self.tempo.into();
                snapshot["project"]["trackCount"] = self.tracks.len().into();
                snapshot["tracks"] = Value::Array(track_rows());
                snapshot["markers"] = Value::Array(marker_rows());
                patch_transport(&mut snapshot["transport"]);
                snapshot["transport"]["playhead"] = playhead();
                snapshot
            }),
            "focus" => recorded("focus", asked.clone()).map(|mut focus| {
                patch_transport(&mut focus["transport"]);
                focus["transport"]["beat"] = self.playhead.into();
                focus["playhead"] = playhead();
                focus
            }),
            "focus_body" if kind == Some("timeline") => recorded("focus_body", json!({"kind": "timeline"})).and_then(|mut whole| {
                let recorded_lanes = whole["lanes"].as_array().cloned().unwrap_or_default();
                let lanes: Vec<Value> = self
                    .tracks
                    .iter()
                    .enumerate()
                    .map(|(at, track)| {
                        // A recorded track keeps its lane (its regions); a track Kumi added has none.
                        let mut lane = recorded_lanes
                            .get(track.id as usize - 1)
                            .filter(|_| track.id <= recorded_lanes.len() as u64)
                            .cloned()
                            .unwrap_or_else(|| json!({"kind": track.kind, "regions": []}));
                        lane["name"] = track.name.clone().into();
                        lane["trackRef"] = format!("{RECORDED_EPOCH}:track:{}", at + 1).into();
                        lane
                    })
                    .collect();
                whole["lanes"] = Value::Array(lanes);
                whole["trackCount"] = self.tracks.len().into();
                whole["playing"] = self.playing.into();
                whole["playheadBeats"] = self.playhead.into();
                whole["markers"] = self.markers.iter().map(|m| json!({"name": m.name, "positionBeats": m.position_beats})).collect();
                timeline_page(&whole, asked)
            }),
            "focus_body" if kind == Some("tree") => {
                let mut body = recorded("focus_body", asked.clone()).ok()?;
                let reference = body["trackRef"].as_str().unwrap_or_default();
                let place =
                    reference.rsplit(':').next().and_then(|n| n.parse::<usize>().ok()).filter(|p| *p >= 1 && *p <= self.tracks.len())?;
                let track = &self.tracks[place - 1];
                let strip = &mut body["strip"];
                strip["name"] = track.name.clone().into();
                strip["volumeDb"] = track.volume_db.into();
                strip["pan"] = logic_pan(track.pan).into();
                strip["mute"] = track.mute.into();
                strip["solo"] = track.solo.into();
                strip["armed"] = track.armed.into();
                Ok(body)
            }
            _ => return None,
        })
    }
}

impl LogicScript {
    /// The project of the vectors: what the bridge answers for Scratch.
    pub fn new() -> Self {
        LogicScript { state: Arc::new(Mutex::new(State::recorded())) }
    }

    /// A track or region was added, removed or moved: the epoch moves, so refs and cursors from before are stale.
    pub fn change_tracks(&self) {
        self.state.lock().unwrap().bump_epoch();
    }

    /// Another project is open: the `n`th copy of the recorded one (0 is the recorded one itself), with an id, an
    /// identity, a name and a path of its own and the same rows, as a copy saved under another name has. The epoch
    /// moves with it, and no ref of the project before names anything in it.
    pub fn open_project(&self, n: u32) {
        let mut state = self.state.lock().unwrap();
        state.epoch += 1;
        state.project = n;
        state.places.clear();
    }

    /// The project open now, as the bridge's `status` would name it: the recorded one, or the copy `open_project` opened.
    pub fn project(&self) -> Project {
        let n = self.state.lock().unwrap().project;
        serde_json::from_value(another_project(&serde_json::to_value(project()).unwrap(), n)).unwrap()
    }

    /// Every request the simulator answered, in order.
    pub fn asked(&self) -> Vec<(String, Value)> {
        self.state.lock().unwrap().asked.clone()
    }

    /// The epoch the project is in now.
    pub fn epoch(&self) -> u32 {
        self.state.lock().unwrap().epoch
    }

    /// A track's fader, in dB (`index` is its place in the Tracks area, from 1).
    pub fn volume(&self, index: usize) -> f64 {
        self.state.lock().unwrap().tracks[index - 1].volume_db
    }

    /// A track's row as a read shows it now (`index` from 1).
    pub fn track(&self, index: usize) -> Value {
        let state = self.state.lock().unwrap();
        retag(&state.tracks[index - 1].row(index), RECORDED_EPOCH, state.epoch)
    }

    pub fn track_count(&self) -> usize {
        self.state.lock().unwrap().tracks.len()
    }

    /// The producer moves a track's fader in Logic (`index` from 1).
    pub fn set_volume(&self, index: usize, db: f64) {
        self.state.lock().unwrap().tracks[index - 1].volume_db = db;
    }

    /// The producer changes a track's `field` (as the tool names it: `pan`, `mute`, `solo`, `arm`, `name`) in Logic.
    pub fn set_track(&self, index: usize, field: &str, value: Value) {
        self.state.lock().unwrap().tracks[index - 1].set(field, &value);
    }

    pub fn tempo(&self) -> f64 {
        self.state.lock().unwrap().tempo
    }

    /// The markers, in the Marker List's order: each name and its position in beats.
    pub fn markers(&self) -> Vec<(String, f64)> {
        self.state.lock().unwrap().markers.iter().map(|m| (m.name.clone(), m.position_beats)).collect()
    }

    /// The cycle area: on, and from and to (beats).
    pub fn cycle(&self) -> (bool, f64, f64) {
        let cycle = self.state.lock().unwrap().cycle;
        (cycle.on, cycle.start, cycle.end)
    }

    pub fn playing(&self) -> bool {
        self.state.lock().unwrap().playing
    }

    /// The playhead, in beats from the project's start.
    pub fn playhead(&self) -> f64 {
        self.state.lock().unwrap().playhead
    }

    /// Logic's undo history as the host named its steps (`desc`), oldest first; a step of a write Logic didn't show
    /// back is empty, the bridge not knowing its name.
    pub fn undo_history(&self) -> Vec<String> {
        self.state.lock().unwrap().undo.iter().map(|(_, desc)| desc.clone().unwrap_or_default()).collect()
    }

    /// The same, as Logic's Edit menu titles them.
    pub fn undo_titles(&self) -> Vec<String> {
        self.state.lock().unwrap().undo.iter().map(|(title, _)| title.clone()).collect()
    }

    /// A dialog shows for the next change's preview, and closes right after.
    pub fn busy_once(&self) {
        self.state.lock().unwrap().busy_once = true;
    }

    /// The next write (an apply or a restore) is made, and Logic doesn't show the value back: the bridge answers
    /// `uncertain`.
    pub fn uncertain_once(&self) {
        self.state.lock().unwrap().uncertain_once = true;
    }

    /// The producer presses Cmd-Z in Logic: its last step comes off (its values back as the preview saw them is the
    /// test's business), and the bridge names it to the host by the `desc` it kept (`undone_in_daw`): Logic's title
    /// for the step, and that desc, if the bridge had one.
    pub fn undo_last(&self) -> Option<(String, Option<String>)> {
        self.state.lock().unwrap().undo.pop()
    }

    pub fn respond(&self) -> Respond {
        let script = self.clone();
        Arc::new(move |method, params| script.answer(method, params))
    }

    /// The simulator's config for Logic with this project behind it: the reads, and Kumi's changes.
    pub fn config(&self, secret: Secret, resource: &str) -> SimulatorConfig {
        SimulatorConfig::logic(secret, resource).with_methods(&METHODS, self.respond()).with_methods(&CHANGE_METHODS, self.respond())
    }

    fn answer(&self, method: &str, params: &Value) -> Option<Answer> {
        if !METHODS.contains(&method) && !CHANGE_METHODS.contains(&method) {
            return None;
        }
        let mut state = self.state.lock().unwrap();
        state.asked.push((method.to_string(), params.clone()));
        match method {
            "change_preview" | "change_apply" | "change_restore" => return Some(state.change(method, params)),
            "transport_stop" | "transport_play" | "cursor_set" => return Some(state.transport(method, params)),
            "undo_step_begin" | "undo_step_end" => return Some(state.plan_step(method, params)),
            _ => {}
        }
        let (epoch, project) = (state.epoch, state.project);
        // Past the recorded epoch, a ref or cursor from an earlier one is refused before anything is looked up, as the
        // bridge refuses it whatever it was for; in the recorded epoch the refusals recorded answer.
        if epoch != RECORDED_EPOCH {
            if let Some(stale) = stale_in(params, epoch) {
                return Some(Err(stale));
            }
        }
        let answered = |result: &Value| another_project(&retag(result, RECORDED_EPOCH, epoch), project);
        // (whole numbers as integers, so a page asked with limit 1.0 is the page of limit 1)
        let asked = normal(&retag(params, epoch, RECORDED_EPOCH));
        // The rows Kumi's changes reach are made from the project as changed, over the recorded ones.
        if let Some(made) = state.changed_rows(method, &asked, params) {
            return Some(made.map(|result| answered(&result)));
        }
        if let Some(case) = find(&state.cases, method, &asked) {
            return Some(case.response.clone().map(|result| answered(&result)));
        }
        if let Some(stale) = stale_in(params, epoch) {
            return Some(Err(stale));
        }
        Some(Err(("bad-params".into(), format!("the simulator has no answer scripted for {method} {params}"))))
    }
}

impl Default for LogicScript {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_recorded_set_answers_verbatim_and_follows_the_epoch() {
        let script = LogicScript::new();
        let answer = |method: &str, params: Value| script.answer(method, &params).expect("a method the simulator has");
        let tracks = answer("read", json!({"kind": "track"})).unwrap();
        assert_eq!((tracks["epoch"].as_str(), tracks["items"][0]["ref"].as_str()), (Some("1"), Some("1:track:1")));
        assert_eq!(answer("read", json!({"kind": "note"})).unwrap_err().0, "bad-params", "the recorded refusal");
        assert_eq!(answer("read", json!({"kind": "region", "parent": "0:track:2"})).unwrap_err().0, "stale");
        assert_eq!(answer("read", json!({"kind": "track", "limit": 1.0})).unwrap()["nextCursor"], "1.1", "whole numbers alike");
        assert!(answer("read", json!({"kind": "note", "parent": "1:region:1", "limit": 3})).unwrap_err().1.contains("no answer scripted"));
        // The epoch moves: refs from before are stale, and the same questions answer in the new epoch.
        script.change_tracks();
        assert_eq!(answer("read", json!({"ref": "1:track:2"})).unwrap_err().0, "stale");
        assert_eq!(
            answer("read", json!({"kind": "track", "cursor": "1.1"})).unwrap_err().1,
            "that cursor is from an earlier epoch (1; now 2): read again from the start"
        );
        let again = answer("read", json!({"kind": "track", "limit": 1})).unwrap();
        assert_eq!(
            (again["epoch"].as_str(), again["items"][0]["ref"].as_str(), again["nextCursor"].as_str()),
            (Some("2"), Some("2:track:1"), Some("2.1"))
        );
        let project = answer("read", json!({"kind": "project"})).unwrap();
        assert_eq!(project["items"][0]["id"], json!(super::project().id), "the same project: its id stays");
        assert_eq!(answer("focus", json!({})).unwrap()["selection"]["trackRef"], "2:track:1");
        // Another project: another id, identity and path, the same rows.
        script.open_project(1);
        let other = answer("snapshot", json!({})).unwrap();
        assert_eq!((other["epoch"].as_str(), other["tracks"][0]["ref"].as_str()), (Some("3"), Some("3:track:1")));
        assert_ne!(other["project"]["id"], json!(super::project().id));
        let copy = super::project().path.map(|path| path.replace(".logicx", " 1.logicx")).expect("the recorded project has a path");
        assert_eq!(other["project"]["path"], json!(copy));
        assert_eq!(script.project().path.as_deref(), Some(copy.as_str()));
        script.open_project(0);
        assert_eq!(answer("snapshot", json!({})).unwrap()["project"]["id"], json!(super::project().id));
    }

    #[test]
    fn a_page_of_the_tracks_area_is_cut_from_the_recorded_one() {
        let script = LogicScript::new();
        let answer = |params: Value| script.answer("focus_body", &params).unwrap();
        let page = answer(json!({"kind": "timeline", "limit": 24, "around": "1:track:1"})).unwrap();
        assert_eq!((page["from"].as_u64(), page["lanes"].as_array().map(Vec::len), page.get("nextCursor")), (Some(0), Some(2), None));
        let one = answer(json!({"kind": "timeline", "limit": 1})).unwrap();
        assert_eq!((one["lanes"][0]["name"].as_str(), one["nextCursor"].as_str()), (Some("Vocal"), Some("1.1")));
        let rest = answer(json!({"kind": "timeline", "limit": 1, "cursor": "1.1"})).unwrap();
        assert_eq!((rest["lanes"][0]["name"].as_str(), rest["from"].as_u64()), (Some("Keys"), Some(1)));
        let later = answer(json!({"kind": "timeline", "window": {"from": 32.0, "to": 48.0}})).unwrap();
        assert!(later["lanes"][1]["regions"].as_array().unwrap().is_empty(), "the region ends before the window: {later}");
        assert_eq!(answer(json!({"kind": "timeline", "around": "1:track:7"})).unwrap_err().0, "failed");
        // The recorded page itself, when the request is the recorded one.
        let recorded = answer(json!({"kind": "timeline", "around": "1:track:2", "window": {"from": 0.0, "to": 16.0}, "limit": 1})).unwrap();
        assert_eq!((recorded["from"].as_u64(), recorded["lanes"][0]["name"].as_str()), (Some(1), Some("Keys")));
    }

    /// A change's value shows in every read after it, over the recorded rows; a track added moves the epoch and takes
    /// its place at the end, in the pages, the snapshot and the Tracks area; a restore's ref from before is carried.
    #[test]
    fn a_changed_value_shows_in_the_reads_and_a_track_added_takes_its_place_in_them() {
        let script = LogicScript::new();
        let answer = |method: &str, params: Value| script.answer(method, &params).expect("a method the simulator has");
        let preview =
            answer("change_preview", json!({"kind": "track-volume", "input": {"ref": "1:track:1", "volumeDb": -6}, "epoch": "1"})).unwrap();
        assert_eq!(
            (preview["prior"].clone(), preview["display"]["title"].as_str()),
            (json!({"volumeDb": 0.0}), Some("Vocal volume -6 dB"))
        );
        assert_eq!(script.volume(1), 0.0, "a preview changes nothing");
        let applied = answer(
            "change_apply",
            json!({"kind": "track-volume", "input": {"ref": "1:track:1", "volumeDb": -6}, "fence": {"volumeDb": 0.0}, "desc": "Kumi: Vocal volume -6 dB (k0001)", "epoch": "1"}),
        )
        .unwrap();
        assert_eq!(
            (applied["applied"].clone(), applied["undo"]["entry"].clone(), applied["undo"]["title"].as_str()),
            (json!({"volumeDb": -6.0}), json!(true), Some("Undo Change Volume")),
            "the value read back after the write"
        );
        assert_eq!(answer("read", json!({"ref": "1:track:1"})).unwrap()["items"][0]["volumeDb"], -6.0);
        assert_eq!(answer("read", json!({"kind": "track", "limit": 1})).unwrap()["items"][0]["volumeDb"], -6.0);
        assert_eq!(answer("snapshot", json!({})).unwrap()["tracks"][0]["volumeDb"], -6.0);
        assert_eq!(answer("focus_body", json!({"kind": "tree"})).unwrap()["strip"]["volumeDb"], -6.0);
        assert_eq!(script.undo_history(), ["Kumi: Vocal volume -6 dB (k0001)"]);

        // A track added: the epoch moves, the new track is third everywhere, and the fader's restore still finds track 1.
        let added = answer(
            "change_apply",
            json!({"kind": "track-add", "input": {"name": "Pad"}, "fence": {"tracks": 2}, "desc": "Kumi: Added track “Pad” (k0002)", "epoch": "1"}),
        )
        .unwrap();
        assert_eq!((added["epoch"].as_str(), added["created"]["ref"].as_str()), (Some("2"), Some("2:track:3")));
        let page = answer("read", json!({"kind": "track"})).unwrap();
        assert_eq!(
            (page["total"].as_u64(), page["items"][2]["name"].as_str(), page["items"][2]["ref"].as_str()),
            (Some(3), Some("Pad"), Some("2:track:3"))
        );
        assert_eq!(answer("read", json!({"kind": "project"})).unwrap()["items"][0]["trackCount"], 3);
        let timeline = answer("focus_body", json!({"kind": "timeline"})).unwrap();
        assert_eq!(
            (
                timeline["trackCount"].as_u64(),
                timeline["lanes"][2]["name"].as_str(),
                timeline["lanes"][1]["regions"].as_array().map(Vec::len)
            ),
            (Some(3), Some("Pad"), Some(1))
        );
        assert_eq!(answer("read", json!({"ref": "1:track:1"})).unwrap_err().0, "stale", "a read from before is stale");
        let restored = answer(
            "change_restore",
            json!({"kind": "track-volume", "target": {"ref": "1:track:1", "kind": "track"}, "prior": {"volumeDb": 0.0}, "applied": {"volumeDb": -6.0}, "desc": "Kumi: undo Vocal volume -6 dB (k0003)", "epoch": "1"}),
        )
        .unwrap();
        assert_eq!((restored["epoch"].as_str(), restored["restored"].clone()), (Some("2"), json!({"volumeDb": 0.0})));
        assert_eq!(script.volume(1), 0.0);
        // The added track taken away again: the epoch moves once more, and the pages are as recorded.
        let gone = answer(
            "change_restore",
            json!({"kind": "track-add", "target": {"ref": null, "kind": "track"}, "prior": {"tracks": 2}, "applied": added["applied"], "created": added["created"], "desc": "Kumi: undo Added track “Pad” (k0004)", "epoch": "2"}),
        )
        .unwrap();
        assert_eq!((gone["epoch"].as_str(), script.track_count()), (Some("3"), 2));
        assert_eq!(answer("read", json!({"kind": "track"})).unwrap()["items"][1]["ref"], "3:track:2");
        assert_eq!(script.undo_titles(), ["Undo Change Volume", "Undo Create Track", "Undo Change Volume", "Undo Delete Track"]);
    }

    #[test]
    fn positions_read_as_logic_shows_them() {
        assert_eq!(position_of(32.0), json!({"bar": 9, "beat": 1, "division": 1, "tick": 1}));
        assert_eq!(position_of(5.5), json!({"bar": 2, "beat": 2, "division": 3, "tick": 1}));
        assert_eq!(beats_of(&json!({"bar": 9, "beat": 1, "division": 1, "tick": 1})), 32.0);
        assert_eq!(beats_of(&position_of(5.5)), 5.5);
        assert_eq!((bar_text(16.0), bar_text(17.5)), ("bar 5".to_string(), "bar 5 beat 2.5".to_string()));
        assert_eq!((logic_pan(0.25), logic_pan(1.0), logic_pan(-1.0)), (16, 63, -64));
        assert_eq!(
            (pan_text(0.0), pan_text(-0.5), pan_text(0.25)),
            ("centre".to_string(), "50% left".to_string(), "25% right".to_string())
        );
    }
}
