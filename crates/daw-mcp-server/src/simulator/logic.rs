//! A scripted Logic Pro project for the host's tests: what Kumi's own bridge beside Logic (`logic-kumi`) answers to
//! `read`, `snapshot`, `focus` and `focus_body` (the M2 contract, docs/en/LOGIC_CONTRACT.md), replayed verbatim; and
//! what it answers to Kumi's changes (M3): `change_preview`, `change_apply` and `change_restore`, the transport and the
//! playhead, and a plan's steps, counted, since Logic has no undo block.
//!
//! What the bridge answers to the fixed request set of M2's brief over the Scratch project is kept in
//! tests/vectors/logic-reads.json, which the bridge's own tests write from the real Logic over its AX fixtures
//! (KUMI_BLESS=1 in crates/logic-kumi/tests/reads.rs). This replays those answers, so the simulator cannot drift from
//! the bridge. The project can be changed, as a track or region added, removed or moved changes it in Logic: the epoch
//! moves, refs and cursors from before are refused as `stale`, and the answers carry the new epoch. Another project can
//! be opened: the epoch moves and the project's id, identity, name and path are another's. A page of the Tracks area
//! nobody recorded (FOCUS asks for a screenful of lanes around the selected track) is cut from the recorded one, as the
//! bridge pages it.
//!
//! Kumi's changes are made on the project as changed over the recorded rows, the way the bridge makes them (the
//! contract's Changes section): a preview that writes nothing and fences what it saw, an apply that checks the fence
//! again, writes and answers the value read back, a restore by value fenced by what the apply wrote. Logic keeps no
//! undo step for a header's control (volume, pan, mute, solo, arm) or the tempo, so those answer `undo.entry: false`
//! with no title; a rename ("Renaming"), a track made ("Create Track") or deleted ("Delete Tracks") and a marker made
//! ("Create Marker"), moved ("Value Change") or deleted ("Delete") are steps of Logic's undo, named by the Edit menu's
//! title and by the `desc` the host sent. A track added goes below the selected one (or the one `index` names) and
//! moves the epoch, as the M2 replay moves it; a restore's ref from before is carried to where its track is now. A
//! deleted track comes back only through Logic's own undo (Cmd-Z), while that step is still on top; a deleted marker
//! the same way first, else made again by value, numbered by Logic. The reads replay the vectors with the changed
//! values in them. A dialog (`busy_once`) refuses the next preview before anything is written; a write Logic doesn't
//! answer after (`uncertain_once`) is made and answered `uncertain`. The kinds the bridge can't make are refused saying
//! why, as it refuses them; of the kinds it makes, the simulator makes those whose state is a value here (the strip's
//! controls and its name, tracks added and deleted, markers and the tempo), and says so for the rest (notes, bypass).
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
/// The reads, in the order the bridge's welcome lists them after ping and status.
const METHODS: [&str; 4] = ["focus", "read", "snapshot", "focus_body"];
/// What the project answers besides: Kumi's changes, the transport and the playhead, a plan's steps, and (M4) the
/// project saved, which is no change.
const CHANGE_METHODS: [&str; 9] = [
    "change_preview",
    "change_apply",
    "change_restore",
    "transport_stop",
    "transport_play",
    "cursor_set",
    "undo_step_begin",
    "undo_step_end",
    "project_save",
];
/// The kinds the bridge can't make yet, and why, as it refuses them.
const NOT_YET: [(&str, &str); 7] = [
    ("marker-rename", "a marker's name cell in the Marker List isn't a text field"),
    ("fx-add", "the insert slot's pop-up menu can't be driven from outside"),
    ("fx-delete", "the insert slot's pop-up menu can't be driven from outside"),
    ("fx-param", "a plug-in's controls aren't reached yet"),
    ("fx-move", "the insert slots can't be rearranged from outside"),
    ("send-add", "the send slot's pop-up menu can't be driven from outside"),
    ("send-volume", "the send slot's pop-up menu can't be driven from outside"),
];
/// The kinds the bridge makes whose state the simulator doesn't hold.
const NOT_SIMULATED: [&str; 5] = ["notes-add", "notes-change", "notes-delete", "notes-quantize", "fx-bypass"];
/// The kinds the simulator makes, for the refusal that names them.
const SIMULATED: &str = "track-volume, track-pan, track-mute, track-solo, track-arm, track-rename, track-color, track-add, track-delete, track-move, track-stack, track-stack-flatten, marker-add, marker-move, marker-delete, tempo-set, tempo-marker-set, tempo-marker-delete, signature-set, loop-range, region-move, region-resize, region-split, region-loop, region-rename, region-delete, midi-import and key-command";
/// (M4) What a key command's preview says of taking it back, as the bridge says it (REAPER's note, in Logic's words).
const KEY_NOTE: &str = "Only Logic Pro's own undo can take this back (Cmd-Z in Logic Pro); it acts on what is selected in Logic Pro now.";
/// What a key command Logic kept no step of answers: no row in HISTORY.
const KEY_NO_STEP: &str = "Logic Pro made no undo step of it: it changed nothing the project keeps (a view, selection or transport command), so it has no row in HISTORY.";
/// The key commands the simulated Logic knows by their English names, with the step Logic's history takes for each
/// (none for a selection); the rest of the Key Commands window isn't modelled.
const KEY_COMMANDS: [(&str, Option<&str>); 4] = [
    ("Select All", None),
    ("Deselect All", None),
    ("Create Marker", Some("Create Marker")),
    ("Split Regions/Events at Playhead", Some("Split")),
];
/// The key commands the bridge refuses: each opens a dialog or leaves the project.
const KEY_REFUSED: [&str; 11] = [
    "Quit Logic Pro",
    "Open…",
    "Close Project",
    "Close",
    "Save",
    "Save As…",
    "Bounce Project or Section…",
    "New…",
    "New Project",
    "Import MIDI File…",
    "Export Selection as MIDI File…",
];
/// The name a region imported from Kumi's MIDI file gets (the file's name, which the bridge chooses).
const IMPORT_NAME: &str = "Kumi Import";
/// The colour every recorded track shows in the palette (the header read shows none: M2's `color: null`).
const PALETTE_COLOR: &str = "#8a8a8a";
/// The recorded project's signature: four beats to the bar.
const BEATS_PER_BAR: f64 = 4.0;
/// Logic's resolution: 960 ticks to a beat, a division being a sixteenth of 240 ticks.
const TICKS_PER_BEAT: f64 = 960.0;
const DIVISIONS_PER_BEAT: f64 = 4.0;
/// The fader's floor and ceiling in dB, and how far a reading may be from what was asked and still count.
const MIN_DB: f64 = -96.0;
const MAX_DB: f64 = 6.0;
const CLOSE_DB: f64 = 0.051;
/// Logic's pan runs 64 steps each way from centre.
const PAN_CENTRE: i64 = 64;
/// Logic's tempo slider runs 5 to 990 BPM in whole steps.
const TEMPO_MIN: f64 = 5.0;
const TEMPO_MAX: f64 = 990.0;
/// How long a marker Logic makes is (its length to the next one isn't modelled).
const MARKER_LENGTH: f64 = 16.0;
/// What the Edit menu says with nothing to undo.
const CANT_UNDO: &str = "Can't Undo";
/// What the bridge answers when Logic doesn't answer after a write was sent.
const UNANSWERED: &str = "Logic Pro didn't answer after the change was sent: the helper didn't answer in time";

/// One of the project's tracks, as Kumi's changes left it: the recorded row (or one made for a track Kumi added) and
/// the values the changes reach.
#[derive(Clone)]
struct Track {
    /// Tells a track apart through the places it has over the epochs.
    id: u64,
    row: Value,
    name: String,
    kind: String,
    volume_db: f64,
    /// Logic's pan, -64 to 63 (the tool's -1 to 1, as steps).
    pan_steps: i64,
    mute: bool,
    solo: bool,
    armed: bool,
    /// (M4) The colour Kumi set through the palette, shown in the row once set (the header read shows none).
    color: Option<String>,
    /// (M4) The Track Stack this track is in: its own track's id.
    in_stack: Option<u64>,
}

impl Track {
    /// A recorded row.
    fn recorded(id: u64, row: &Value) -> Track {
        Track {
            id,
            row: row.clone(),
            name: row["name"].as_str().unwrap_or_default().to_string(),
            kind: row["kind"].as_str().unwrap_or("audio").to_string(),
            volume_db: row["volumeDb"].as_f64().unwrap_or(0.0),
            pan_steps: row["pan"].as_i64().unwrap_or(0),
            mute: row["mute"] == true,
            solo: row["solo"] == true,
            armed: row["armed"] == true,
            color: row["color"].as_str().map(str::to_owned),
            in_stack: None,
        }
    }

    /// A track Logic's Track menu makes: an audio, software instrument or external MIDI track, named; or (M4) a
    /// Track Stack's own track (`folder`, `summing`).
    fn made(id: u64, name: &str, kind: &str) -> Track {
        let row = json!({
            "armed": false, "color": null, "index": 0, "inputs": if kind == "audio" { json!(["Input 1"]) } else { json!([]) },
            "kind": kind, "monitoring": false, "mute": false, "name": name, "output": "Stereo Output", "pan": 0,
            "ref": "", "selected": false, "solo": false, "volumeDb": 0.0,
        });
        Track {
            id,
            row,
            name: name.into(),
            kind: kind.into(),
            volume_db: 0.0,
            pan_steps: 0,
            mute: false,
            solo: false,
            armed: false,
            color: None,
            in_stack: None,
        }
    }

    /// The row as a read shows it at `place` (from 1) in the recorded epoch (the answer is retagged to the one now).
    fn row(&self, place: usize) -> Value {
        let mut row = self.row.clone();
        row["ref"] = format!("{RECORDED_EPOCH}:track:{place}").into();
        row["index"] = place.into();
        row["name"] = self.name.clone().into();
        row["kind"] = self.kind.clone().into();
        row["volumeDb"] = self.volume_db.into();
        row["pan"] = self.pan_steps.into();
        row["mute"] = self.mute.into();
        row["solo"] = self.solo.into();
        row["armed"] = self.armed.into();
        row["color"] = self.color.clone().map_or(Value::Null, Value::String);
        row
    }

    /// Whether it is a Track Stack's own track.
    fn is_stack(&self) -> bool {
        self.kind == "folder" || self.kind == "summing"
    }

    /// A prop's state as the bridge reads it from the header.
    fn state(&self, field: &str) -> Value {
        match field {
            "volumeDb" => volume_state(self.volume_db),
            "pan" => pan_state(self.pan_steps),
            "mute" => json!({"mute": self.mute}),
            "solo" => json!({"solo": self.solo}),
            "arm" => json!({"armed": self.armed}),
            _ => json!({"name": self.name}),
        }
    }

    /// A prop's state written.
    fn write(&mut self, field: &str, state: &Value) {
        match field {
            "volumeDb" => self.volume_db = state["volumeDb"].as_f64().unwrap_or(self.volume_db),
            "pan" => self.pan_steps = state["steps"].as_i64().unwrap_or(self.pan_steps),
            "mute" => self.mute = state["mute"] == true,
            "solo" => self.solo = state["solo"] == true,
            "arm" => self.armed = state["armed"] == true,
            _ => self.name = state["name"].as_str().unwrap_or(&self.name).to_string(),
        }
    }
}

fn volume_state(db: f64) -> Value {
    json!({"volumeDb": (db * 10.0).round() / 10.0})
}

fn pan_state(steps: i64) -> Value {
    json!({"pan": (steps as f64 / PAN_CENTRE as f64 * 1000.0).round() / 1000.0, "steps": steps})
}

/// dB with the true minus sign, as the bridge's titles show it: `−6`, `+3`, `0`, `−6.5`, `−∞`.
fn db_text(db: f64) -> String {
    if db <= MIN_DB + 0.5 {
        return "−∞".into();
    }
    let rounded = (db * 10.0).round() / 10.0;
    let text = format!("{:.1}", rounded.abs());
    let text = text.strip_suffix(".0").map(str::to_string).unwrap_or(text);
    if rounded == 0.0 {
        "0".into()
    } else if rounded < 0.0 {
        format!("−{text}")
    } else {
        format!("+{text}")
    }
}

/// A pan as a producer says it: `centre`, `50% left`, `98% right`.
fn pan_text(steps: i64) -> String {
    let percent = ((steps.abs() as f64 / PAN_CENTRE as f64) * 100.0).round() as i64;
    match steps {
        0 => "centre".into(),
        n if n < 0 => format!("{percent}% left"),
        _ => format!("{percent}% right"),
    }
}

#[derive(Clone, PartialEq)]
struct Marker {
    name: String,
    position_beats: f64,
    length: f64,
}

impl Marker {
    /// What a marker is, as the Marker List shows it (the bridge's state).
    fn state(&self) -> Value {
        json!({"position": position_of(self.position_beats), "name": self.name, "length": self.length, "text": ""})
    }
    /// The row as the Marker List reads it, at `place` (from 1).
    fn row(&self, place: usize) -> Value {
        json!({"length": self.length, "name": self.name, "position": position_of(self.position_beats), "ref": format!("{RECORDED_EPOCH}:marker:{place}"), "text": ""})
    }
    /// The marker as a title says it: `marker “Marker 1”`.
    fn label(&self) -> String {
        if self.name.is_empty() {
            "marker".into()
        } else {
            format!("marker {}", quoted(&self.name))
        }
    }
}

/// (M4) One of the project's regions, as Kumi's changes left it: the recorded row's fields, the track it sits on (by
/// the track's id, through the places it has over the epochs), and its notes as the Event List lists them.
#[derive(Clone)]
struct Region {
    /// Tells a region apart through the places it has over the epochs.
    id: u64,
    track: u64,
    name: String,
    kind: String,
    start: f64,
    end: f64,
    looped: bool,
    selected: bool,
    /// Each note as its Event List row less its refs and position (`channel`, `lengthTicks`, `pitch`, `velocity`,
    /// `selected` when it is), with `beat`: its start from the region's start, in beats.
    notes: Vec<Value>,
}

impl Region {
    /// A recorded row, with the notes recorded for it (their positions are the project's: made the region's).
    fn recorded(id: u64, row: &Value, track: u64, notes: &[Value]) -> Region {
        let start = row["startBeats"].as_f64().unwrap_or(0.0);
        let notes = notes
            .iter()
            .map(|note| {
                let mut kept = note.clone();
                let beat = beats_of(&note["position"]) - start;
                if let Some(map) = kept.as_object_mut() {
                    map.remove("position");
                    map.remove("ref");
                    map.remove("regionRef");
                }
                kept["beat"] = beat.into();
                kept
            })
            .collect();
        Region {
            id,
            track,
            name: row["name"].as_str().unwrap_or_default().to_string(),
            kind: row["kind"].as_str().unwrap_or("midi").to_string(),
            start,
            end: row["endBeats"].as_f64().unwrap_or(start),
            looped: row["looped"] == true,
            selected: row["selected"] == true,
            notes,
        }
    }

    /// Its edges, as the bridge reads them from the Tracks area.
    fn bounds(&self) -> Value {
        json!({"startBeats": self.start, "endBeats": self.end})
    }

    /// The row as a read shows it at `place` (from 1), its track at `track_place`, in the recorded epoch.
    fn row(&self, place: usize, track_place: usize) -> Value {
        let mut row = json!({
            "endBeats": self.end, "kind": self.kind, "looped": self.looped, "name": self.name,
            "ref": format!("{RECORDED_EPOCH}:region:{place}"), "startBeats": self.start,
            "trackRef": format!("{RECORDED_EPOCH}:track:{track_place}"),
        });
        if self.selected {
            row["selected"] = true.into();
        }
        alphabetical(row)
    }

    /// The region as the Tracks area's lane lists it.
    fn lane_row(&self, place: usize) -> Value {
        let mut row = json!({"endBeats": self.end, "kind": self.kind, "name": self.name, "ref": format!("{RECORDED_EPOCH}:region:{place}"), "startBeats": self.start});
        if self.selected {
            row["selected"] = true.into();
        }
        alphabetical(row)
    }

    /// Its notes as the Event List's rows, each at its place in the region (from 1), in the recorded epoch.
    fn note_rows(&self, place: usize) -> Vec<Value> {
        self.notes
            .iter()
            .enumerate()
            .map(|(n, note)| {
                let mut row = note.clone();
                let beat = row.as_object_mut().and_then(|map| map.remove("beat")).and_then(|beat| beat.as_f64()).unwrap_or(0.0);
                row["position"] = position_of(self.start + beat);
                row["ref"] = format!("{RECORDED_EPOCH}:note:{place}.{}", n + 1).into();
                row["regionRef"] = format!("{RECORDED_EPOCH}:region:{place}").into();
                alphabetical(row)
            })
            .collect()
    }

    /// The region as a title says it: `region “Keys”`.
    fn label(&self) -> String {
        format!("region {}", quoted(&self.name))
    }

    /// The same, opening a title: `Region “Keys”`.
    fn heading(&self) -> String {
        format!("Region {}", quoted(&self.name))
    }
}

/// A number in a state, NaN when it isn't there.
fn number(value: &Value, key: &str) -> f64 {
    value[key].as_f64().unwrap_or(f64::NAN)
}

/// Two edges the same, exactly.
fn same_bounds(a: &Value, b: &Value) -> bool {
    number(a, "startBeats") == number(b, "startBeats") && number(a, "endBeats") == number(b, "endBeats")
}

/// A row with its keys in alphabetical order, as the bridge writes its rows (the goldens are the wire's bytes).
fn alphabetical(value: Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut fields: Vec<(String, Value)> = map.into_iter().map(|(key, field)| (key, alphabetical(field))).collect();
            fields.sort_by(|a, b| a.0.cmp(&b.0));
            Value::Object(fields.into_iter().collect())
        }
        Value::Array(items) => Value::Array(items.into_iter().map(alphabetical).collect()),
        other => other,
    }
}

/// (M4) A note as `midi-import` takes it, made an Event List row less its position: `beat` from the region's start.
fn note_row(note: &Value) -> Result<Value, Fault> {
    let pitch = note["pitch"].as_u64().filter(|p| *p <= 127).ok_or_else(|| bad("each note's pitch must be 0 to 127"))?;
    let start =
        note["start"].as_f64().filter(|s| s.is_finite() && *s >= 0.0).ok_or_else(|| bad("each note's start must be a number from 0"))?;
    let length =
        note["length"].as_f64().filter(|l| l.is_finite() && *l > 0.0).ok_or_else(|| bad("each note's length must be more than 0"))?;
    let velocity = match note.get("velocity").filter(|v| !v.is_null()) {
        None => 100,
        Some(v) => v.as_u64().filter(|v| (1..=127).contains(v)).ok_or_else(|| bad("each note's velocity must be 1 to 127"))?,
    };
    let channel = match note.get("channel").filter(|c| !c.is_null()) {
        None => 1,
        Some(c) => c.as_u64().filter(|c| (1..=16).contains(c)).ok_or_else(|| bad("each note's channel must be 1 to 16"))?,
    };
    Ok(
        json!({"beat": start, "channel": channel, "lengthTicks": (length * TICKS_PER_BEAT).round() as i64, "pitch": pitch, "velocity": velocity}),
    )
}

/// A track attribute Kumi changes through its header or the Inspector: the input's field, its noun for the words, and
/// the name Logic's history gives its step, when it takes one (a header's control makes none).
struct Prop {
    kind: &'static str,
    field: &'static str,
    noun: &'static str,
    step: Option<&'static str>,
}

static TRACK_PROPS: [Prop; 6] = [
    Prop { kind: "track-volume", field: "volumeDb", noun: "volume", step: None },
    Prop { kind: "track-pan", field: "pan", noun: "pan", step: None },
    Prop { kind: "track-mute", field: "mute", noun: "mute", step: None },
    Prop { kind: "track-solo", field: "solo", noun: "solo", step: None },
    Prop { kind: "track-arm", field: "arm", noun: "record arm", step: None },
    Prop { kind: "track-rename", field: "name", noun: "name", step: Some("Renaming") },
];

impl Prop {
    /// The state the input asks for.
    fn wanted(&self, input: &Value) -> Result<Value, Fault> {
        Ok(match self.field {
            "volumeDb" => {
                let db = input
                    .get("volumeDb")
                    .and_then(Value::as_f64)
                    .filter(|db| db.is_finite())
                    .ok_or_else(|| bad("volumeDb must be a number"))?;
                if db > MAX_DB {
                    return Err(bad(format!("volumeDb must be at most {MAX_DB} (Logic's fader goes no higher)")));
                }
                volume_state(db.max(MIN_DB))
            }
            "pan" => {
                let pan = input
                    .get("pan")
                    .and_then(Value::as_f64)
                    .filter(|pan| (-1.0..=1.0).contains(pan))
                    .ok_or_else(|| bad("pan must be a number from -1 (left) to 1 (right)"))?;
                pan_state(((pan * PAN_CENTRE as f64).round() as i64).clamp(-PAN_CENTRE, PAN_CENTRE - 1))
            }
            "mute" | "solo" => {
                let on =
                    input.get(self.field).and_then(Value::as_bool).ok_or_else(|| bad(format!("{} must be true or false", self.field)))?;
                json!({self.field: on})
            }
            "arm" => {
                let armed = input
                    .get("armed")
                    .or_else(|| input.get("arm"))
                    .and_then(Value::as_bool)
                    .ok_or_else(|| bad("armed must be true or false"))?;
                json!({"armed": armed})
            }
            _ => {
                let name = input.get("name").and_then(Value::as_str).ok_or_else(|| bad("name must be text"))?;
                if name.trim().is_empty() {
                    return Err(bad("name must not be empty: Logic Pro names a track itself when its name is cleared"));
                }
                if name.chars().count() > 256 || name.chars().any(char::is_control) {
                    return Err(bad("name must be one line of at most 256 characters"));
                }
                json!({"name": name})
            }
        })
    }

    /// Two states the same: a fader within a step, a pan by its steps, the rest exactly.
    fn same(&self, a: &Value, b: &Value) -> bool {
        match self.field {
            "volumeDb" => (a["volumeDb"].as_f64().unwrap_or(f64::NAN) - b["volumeDb"].as_f64().unwrap_or(f64::NAN)).abs() <= CLOSE_DB,
            "pan" => a["steps"] == b["steps"],
            "arm" => a["armed"] == b["armed"],
            field => a[field] == b[field],
        }
    }

    /// The title of the change, with the track's name and the state it goes to.
    fn title(&self, name: &str, from: &Value, to: &Value) -> String {
        match self.field {
            "volumeDb" => format!("{name} volume {} dB", db_text(to["volumeDb"].as_f64().unwrap_or(0.0))),
            "pan" => format!("{name} pan {}", pan_text(to["steps"].as_i64().unwrap_or(0))),
            "mute" => format!("{name} {}", if to["mute"] == true { "muted" } else { "unmuted" }),
            "solo" => format!("{name} {}", if to["solo"] == true { "soloed" } else { "unsoloed" }),
            "arm" => format!("{name} {}", if to["armed"] == true { "armed" } else { "disarmed" }),
            _ => format!(
                "Renamed track {} → {}",
                quoted(from["name"].as_str().unwrap_or_default()),
                quoted(to["name"].as_str().unwrap_or_default())
            ),
        }
    }
}

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
    /// The tracks Kumi deleted, by the apply's `desc`: Logic's own undo brings one back whole.
    deleted: HashMap<String, Track>,
    /// The markers, in the Marker List's order (by position).
    markers: Vec<Marker>,
    /// The number Logic gives the next marker it makes ("Marker 2").
    next_marker: usize,
    tempo: f64,
    /// (M4) The project's regions, in the Tracks area's order (by track, then by start); which region stood at each
    /// place in every epoch before this one, for a restore's ref; and the regions Kumi deleted, by the apply's `desc`,
    /// which Logic's own undo brings back whole.
    regions: Vec<Region>,
    region_places: HashMap<u32, Vec<u64>>,
    next_region: u64,
    deleted_regions: HashMap<String, Region>,
    /// (M4) The Tempo List past the project's tempo at the start: each change's beat and BPM, by beat; and the
    /// Signature List: the project's signature at the start, and each change's beat and signature.
    tempo_changes: Vec<(f64, f64)>,
    signature: (i64, i64),
    signature_changes: Vec<(f64, (i64, i64))>,
    /// (M4) The cycle, once Kumi set it: on, and the locators in beats.
    cycle: Option<(bool, f64, f64)>,
    /// (M4) The package the project was saved as, if Kumi saved it as another; every path saved to.
    saved_as: Option<String>,
    saved_paths: Vec<String>,
    /// The playhead, in beats from the project's start.
    playhead: f64,
    playing: bool,
    /// Logic's undo history, newest last: the Edit menu's title for each step, and the `desc` the host sent with the
    /// change that made it (none for a restore's step, or a write Logic didn't answer after: the bridge names neither).
    undo: Vec<(String, Option<String>)>,
    /// A plan's step, counted (Logic has no undo block): its id and label, and the changes applied in it so far.
    open_step: Option<(String, String, usize)>,
    steps: u32,
    /// A dialog shows for the next change preview only (it closes right after).
    busy_once: bool,
    /// The next write (an apply or a restore) is made, and Logic doesn't answer after it.
    uncertain_once: bool,
    /// The next restore alone is made, and Logic doesn't answer after it (REAPER's simulator's switch).
    uncertain_restore_once: bool,
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

fn failed(message: impl Into<String>) -> Fault {
    ("failed".into(), message.into())
}

fn gone(message: impl Into<String>) -> Fault {
    ("gone".into(), message.into())
}

fn uncertain(message: impl Into<String>) -> Fault {
    ("uncertain".into(), message.into())
}

/// A name inside a title's sentence, in curly quotes.
fn quoted(name: &str) -> String {
    format!("\u{201c}{name}\u{201d}")
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

/// Where a position is, as the bridge's titles say it: `bar 17`, `bar 2 beat 3`, or `bar 2 beat 3.3.1` off the beat.
fn bar_text(beats: f64) -> String {
    let position = position_of(beats);
    let at = |key: &str| position[key].as_i64().unwrap_or(1);
    let mut text = format!("bar {}", at("bar"));
    if at("beat") > 1 || at("division") > 1 || at("tick") > 1 {
        text.push_str(&format!(" beat {}", at("beat")));
    }
    if at("division") > 1 || at("tick") > 1 {
        text.push_str(&format!(".{}.{}", at("division"), at("tick")));
    }
    text
}

/// A marker's or the playhead's beat: Logic's displays place them on the bar and beat.
fn on_the_beat(beats: f64) -> f64 {
    beats.floor()
}

/// A time signature from its text (`4/4`), as the Signature List shows it.
fn signature_of(text: &str) -> Option<(i64, i64)> {
    let (num, den) = text.split_once('/')?;
    Some((num.trim().parse().ok()?, den.trim().parse().ok()?))
}

/// The rows a recorded read of `kind` under `parent` answered, if one was recorded.
fn rows_of(cases: &[Case], kind: &str, parent: &str) -> Vec<Value> {
    find(cases, "read", &json!({"kind": kind, "parent": parent}))
        .and_then(|case| case.response.as_ref().ok())
        .and_then(|page| page["items"].as_array().cloned())
        .unwrap_or_default()
}

/// A colour as the tool gives it (`#rrggbb`), lowercased.
fn color_input(input: &Value) -> Result<String, Fault> {
    let text = input.get("color").and_then(Value::as_str).ok_or_else(|| bad("color must be \"#rrggbb\""))?;
    let hex = text.strip_prefix('#').filter(|hex| hex.len() == 6 && hex.bytes().all(|b| b.is_ascii_hexdigit()));
    hex.map(|hex| format!("#{}", hex.to_ascii_lowercase())).ok_or_else(|| bad("color must be \"#rrggbb\""))
}

/// Thirty-two hex digits from a path, as the project's id is a hash of its path (the bridge's).
fn id_of_path(path: &str) -> String {
    let fnv = |seed: u64| path.bytes().fold(seed, |hash, byte| (hash ^ byte as u64).wrapping_mul(0x0000_0100_0000_01b3));
    format!("{:016x}{:016x}", fnv(0xcbf2_9ce4_8422_2325), fnv(0x84222325_cbf29ce4))
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

/// (M4) The project row of `value`, if it holds one, as the package Kumi saved the project as: that path, its stem as
/// the name and identity, and an id hashed from the path, as the bridge's ids are.
fn saved_project(value: &Value, path: &str) -> Value {
    match value {
        Value::Object(map) => {
            let is_project = map.get("id").and_then(Value::as_str).is_some_and(|id| id.len() == 32)
                && (map.contains_key("path") || map.contains_key("identity"));
            let stem = path.rsplit('/').next().unwrap_or(path).trim_end_matches(".logicx");
            Value::Object(
                map.iter()
                    .map(|(key, field)| {
                        let field = match (is_project, key.as_str()) {
                            (true, "id") => Value::String(id_of_path(path)),
                            (true, "identity" | "name") => Value::String(stem.to_string()),
                            (true, "path") => Value::String(path.to_string()),
                            _ => saved_project(field, path),
                        };
                        (key.clone(), field)
                    })
                    .collect(),
            )
        }
        Value::Array(items) => Value::Array(items.iter().map(|item| saved_project(item, path)).collect()),
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
        let markers: Vec<Marker> = rows("marker")
            .iter()
            .map(|row| Marker {
                name: row["name"].as_str().unwrap_or_default().to_string(),
                position_beats: beats_of(&row["position"]),
                length: row["length"].as_f64().unwrap_or(0.0),
            })
            .collect();
        let tempo_row = rows("tempo").into_iter().next().unwrap_or(Value::Null);
        let tempo = tempo_row["bpm"].as_f64().unwrap_or(120.0);
        let signature = tempo_row["signatures"][0]["time"].as_str().and_then(signature_of).unwrap_or((4, 4));
        // The regions as read through the project, each with the notes recorded for it, on the track its ref names.
        let regions: Vec<Region> = rows("region")
            .iter()
            .enumerate()
            .map(|(n, row)| {
                let track = row["trackRef"].as_str().and_then(|r| r.rsplit(':').next()).and_then(|p| p.parse::<u64>().ok()).unwrap_or(1);
                let notes = rows_of(&cases, "note", &format!("{RECORDED_EPOCH}:region:{}", n + 1));
                Region::recorded(n as u64 + 1, row, track, &notes)
            })
            .collect();
        let focus = find(&cases, "focus", &json!({})).and_then(|case| case.response.as_ref().ok()).cloned().unwrap_or(Value::Null);
        let (next_track, next_marker, next_region) = (tracks.len() as u64 + 1, markers.len() + 1, regions.len() as u64 + 1);
        let mut state = State {
            epoch: RECORDED_EPOCH,
            project: 0,
            cases,
            asked: vec![],
            tracks,
            places: HashMap::new(),
            next_track,
            deleted: HashMap::new(),
            markers,
            next_marker,
            tempo,
            regions,
            region_places: HashMap::new(),
            next_region,
            deleted_regions: HashMap::new(),
            tempo_changes: vec![],
            signature,
            signature_changes: vec![],
            cycle: None,
            saved_as: None,
            saved_paths: vec![],
            playhead: focus["transport"]["beat"].as_f64().unwrap_or(0.0),
            playing: focus["transport"]["playing"] == true,
            undo: vec![],
            open_step: None,
            steps: 0,
            busy_once: false,
            uncertain_once: false,
            uncertain_restore_once: false,
        };
        state.keep_places();
        state
    }

    fn epoch_text(&self) -> String {
        self.epoch.to_string()
    }

    /// The epoch moves (a track added, deleted or renamed; a region added, removed or moved): the places the tracks
    /// and regions have in the new epoch are kept under it, for a restore's ref (a layout holds for a whole epoch,
    /// since every change to it moves the epoch; the changes bump it once they have made the new layout).
    fn bump_epoch(&mut self) {
        self.epoch += 1;
        self.keep_places();
    }

    /// The places the tracks and regions have in the epoch now.
    fn keep_places(&mut self) {
        self.places.insert(self.epoch, self.tracks.iter().map(|track| track.id).collect());
        self.region_places.insert(self.epoch, self.regions.iter().map(|region| region.id).collect());
    }

    /// The regions a track holds now.
    fn regions_of(&self, track: &Track) -> usize {
        self.regions.iter().filter(|region| region.track == track.id).count()
    }

    /// The place (from 0) of the track with `id`, if it is in the project.
    fn track_index(&self, id: u64) -> Option<usize> {
        self.tracks.iter().position(|track| track.id == id)
    }

    /// The regions in the Tracks area's order: by their track's place, then by their start.
    fn sort_regions(&mut self) {
        let order: HashMap<u64, usize> = self.tracks.iter().enumerate().map(|(at, track)| (track.id, at)).collect();
        self.regions.sort_by(|a, b| {
            let (a_track, b_track) =
                (order.get(&a.track).copied().unwrap_or(usize::MAX), order.get(&b.track).copied().unwrap_or(usize::MAX));
            a_track.cmp(&b_track).then(a.start.total_cmp(&b.start)).then(a.id.cmp(&b.id))
        });
    }

    /// The region a ref names, by its place now (from 0): a ref of the epoch now, or (a restore's) one of an earlier
    /// epoch, carried to where that region is now.
    fn region_at(&self, reference: Option<&str>, carry: bool) -> Result<usize, Fault> {
        let text = reference.ok_or_else(|| bad("ref must be a region ref"))?;
        let mut parts = text.splitn(3, ':');
        let (epoch, kind, place) = (parts.next(), parts.next(), parts.next().and_then(|n| n.parse::<usize>().ok()));
        let (Some(epoch), Some("region"), Some(place)) = (epoch.and_then(|e| e.parse::<u32>().ok()), kind, place) else {
            return Err(bad(format!("ref must be a region ref ({}:region:1), not {text}", self.epoch)));
        };
        if epoch == self.epoch {
            return (place >= 1 && place <= self.regions.len())
                .then_some(place - 1)
                .ok_or_else(|| failed(format!("{text} isn't in the project (deleted, or another project)")));
        }
        if !carry || epoch > self.epoch {
            return Err(("stale".into(), format!("that ref is from an earlier epoch ({epoch}; now {}): read again", self.epoch)));
        }
        let id = self
            .region_places
            .get(&epoch)
            .and_then(|places| places.get(place.wrapping_sub(1)))
            .ok_or_else(|| gone(format!("the region {text} named isn't in the project any more (or that epoch is too far back)")))?;
        self.regions
            .iter()
            .position(|region| region.id == *id)
            .ok_or_else(|| gone(format!("the region {text} named isn't in the project any more (or that epoch is too far back)")))
    }

    fn region_ref(&self, place: usize) -> String {
        format!("{}:region:{place}", self.epoch)
    }

    /// A track's ref in the epoch now, by its id (empty when the track is gone).
    fn track_ref_of(&self, id: u64) -> String {
        self.track_index(id).map(|at| format!("{}:track:{}", self.epoch, at + 1)).unwrap_or_default()
    }

    /// The region rows as a read shows them (in the recorded epoch, retagged later).
    fn region_rows(&self) -> Vec<Value> {
        self.regions
            .iter()
            .enumerate()
            .map(|(at, region)| region.row(at + 1, self.track_index(region.track).map_or(0, |t| t + 1)))
            .collect()
    }

    /// The track a ref names, by its place now: a ref of the epoch now, or (a restore's) one of an earlier epoch,
    /// carried to where that track is now.
    fn track_at(&self, reference: Option<&str>, carry: bool) -> Result<usize, Fault> {
        let text = reference.ok_or_else(|| bad("ref must be a track ref"))?;
        let mut parts = text.splitn(3, ':');
        let (epoch, kind, place) = (parts.next(), parts.next(), parts.next().and_then(|n| n.parse::<usize>().ok()));
        let (Some(epoch), Some("track"), Some(place)) = (epoch.and_then(|e| e.parse::<u32>().ok()), kind, place) else {
            return Err(bad(format!("ref must be a track ref ({}:track:1), not {text}", self.epoch)));
        };
        if epoch == self.epoch {
            return (place >= 1 && place <= self.tracks.len())
                .then_some(place - 1)
                .ok_or_else(|| failed(format!("{text} isn't in the project (deleted, or another project)")));
        }
        if !carry || epoch > self.epoch {
            return Err(("stale".into(), format!("that ref is from an earlier epoch ({epoch}; now {}): read again", self.epoch)));
        }
        let id = self
            .places
            .get(&epoch)
            .and_then(|places| places.get(place.wrapping_sub(1)))
            .ok_or_else(|| gone(format!("the track {text} named isn't in the project any more (or that epoch is too far back)")))?;
        self.tracks
            .iter()
            .position(|track| track.id == *id)
            .ok_or_else(|| gone(format!("the track {text} named isn't in the project any more (or that epoch is too far back)")))
    }

    /// The place a marker ref names (from 1): markers are by their place in the list, whatever the epoch.
    fn marker_place(&self, reference: Option<&str>) -> Result<usize, Fault> {
        let text = reference.ok_or_else(|| bad("ref must be a marker ref"))?;
        let mut parts = text.splitn(3, ':');
        let (_, kind, place) = (parts.next(), parts.next(), parts.next().and_then(|n| n.parse::<usize>().ok()));
        match (kind, place) {
            (Some("marker"), Some(place)) if place >= 1 => Ok(place),
            _ => Err(bad(format!("ref must be a marker ref ({}:marker:1), not {text}", self.epoch))),
        }
    }

    /// The marker at a position, if one is.
    fn marker_at(&self, beats: f64) -> Option<usize> {
        self.markers.iter().position(|m| m.position_beats == beats)
    }

    fn marker_ref(&self, place: usize) -> String {
        format!("{}:marker:{place}", self.epoch)
    }

    fn sort_markers(&mut self) {
        self.markers.sort_by(|a, b| a.position_beats.total_cmp(&b.position_beats));
    }

    /// What the Edit menu's Undo title says now.
    fn undo_title(&self) -> String {
        self.undo.last().map_or(CANT_UNDO.to_string(), |(title, _)| title.clone())
    }

    /// A write made: the apply's `undo`. Logic's history takes a step for `step`, named by the Edit menu's title and
    /// by the host's `desc` (none for a header's control or the tempo); a write Logic doesn't answer after is
    /// `uncertain`, its step (if any) unnamed to Kumi.
    fn applied(&mut self, step: Option<&str>, desc: &str) -> Result<Value, Fault> {
        let title = step.map(|step| format!("Undo {step}"));
        if std::mem::take(&mut self.uncertain_once) {
            if let Some(title) = title {
                self.undo.push((title, None));
            }
            return Err(uncertain(UNANSWERED));
        }
        match title {
            Some(title) => {
                self.undo.push((title.clone(), Some(desc.to_string())));
                Ok(json!({"desc": desc, "entry": true, "title": title}))
            }
            None => Ok(json!({"desc": desc, "entry": false})),
        }
    }

    /// Whether Logic doesn't answer after the restore being made now (either switch).
    fn restore_unanswered(&mut self) -> bool {
        let once = std::mem::take(&mut self.uncertain_once);
        std::mem::take(&mut self.uncertain_restore_once) || once
    }

    /// A restore made by value: the restore's `undo`. Logic's history takes a step for `step` (a rename, a track
    /// deleted, a marker made, moved or deleted again), which the bridge doesn't name.
    fn restored(&mut self, step: Option<&str>, desc: &str) -> Result<Value, Fault> {
        if self.restore_unanswered() {
            if let Some(step) = step {
                self.undo.push((format!("Undo {step}"), None));
            }
            return Err(uncertain(UNANSWERED));
        }
        if let Some(step) = step {
            self.undo.push((format!("Undo {step}"), None));
        }
        Ok(json!({"desc": desc, "entry": false}))
    }

    /// Logic's own undo of Kumi's step, through the Edit menu: only while its Undo title still names `step` and the top
    /// step is Kumi's (`ours` says whether the top step's desc is the change's). The title pressed.
    fn undo_in_logic(&mut self, step: &str, ours: impl Fn(&str) -> bool) -> Result<String, Fault> {
        let title = self.undo_title();
        let top = self.undo.last().and_then(|(_, desc)| desc.as_deref()).is_some_and(ours);
        if title != format!("Undo {step}") || !top {
            return Err(failed(format!(
                "Logic Pro's undo has other steps on top of this one (its Edit menu says {title:?}), so Kumi can't put it back; Cmd-Z in Logic Pro can, step by step"
            )));
        }
        self.undo.pop();
        Ok(title)
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
                    format!("those refs are from an earlier epoch ({sent}; now {}): the tracks or regions changed, read again", self.epoch),
                ));
            }
        }
        let kind = params["kind"].as_str().unwrap_or_default().to_string();
        if let Some((_, why)) = NOT_YET.iter().find(|(not_yet, _)| *not_yet == kind) {
            return Err(bad(format!("Logic Pro's bridge can't make a {kind} change yet: {why}")));
        }
        match kind.as_str() {
            "track-add" => self.track_add(method, params),
            "track-delete" => self.track_delete(method, params),
            "track-move" => self.track_move(method, params),
            "track-color" => self.track_color(method, params),
            "track-stack" => self.track_stack(method, params),
            "track-stack-flatten" => self.track_stack_flatten(method, params),
            "marker-add" => self.marker_add(method, params),
            "marker-move" => self.marker_move(method, params),
            "marker-delete" => self.marker_delete(method, params),
            "tempo-set" => self.tempo_set(method, params),
            "tempo-marker-set" => self.tempo_marker_set(method, params),
            "tempo-marker-delete" => self.tempo_marker_delete(method, params),
            "signature-set" => self.signature_set(method, params),
            "loop-range" => self.loop_range(method, params),
            "region-move" => self.region_move(method, params),
            "region-resize" => self.region_resize(method, params),
            "region-split" => self.region_split(method, params),
            "region-loop" => self.region_loop(method, params),
            "region-rename" => self.region_rename(method, params),
            "region-delete" => self.region_delete(method, params),
            "midi-import" => self.midi_import(method, params),
            "key-command" => self.key_command(method, params),
            _ => match TRACK_PROPS.iter().find(|prop| prop.kind == kind) {
                Some(prop) => self.track_prop(prop, method, params),
                None if NOT_SIMULATED.contains(&kind.as_str()) => Err(bad(format!(
                    "the simulated Logic Pro makes only {SIMULATED} (not {kind:?}); the bridge makes {} too",
                    NOT_SIMULATED.join(", ")
                ))),
                None => Err(bad(format!("Logic Pro has no change kind {kind:?}"))),
            },
        }
    }

    // ----- (M4) saving, and the escape hatch -----

    /// `project_save {epoch, path?, version?}`: the package written where it is (Cmd-S), or as `path` (File > Save
    /// As…, a `.logicx` that isn't there yet; the project is that file from then on). No change: nothing in the
    /// project moves, and Logic's history takes no step. `version` is REAPER's: Logic keeps no versions.
    fn project_save(&mut self, params: &Value) -> Answer {
        let Some(sent) = params.get("epoch").and_then(Value::as_str) else {
            return Err(bad("epoch: the epoch the refs were read in"));
        };
        if sent != self.epoch_text() {
            return Err((
                "stale".into(),
                format!("those refs are from an earlier epoch ({sent}; now {}): the tracks or regions changed, read again", self.epoch),
            ));
        }
        if params.get("version").is_some_and(|version| !version.is_null() && *version != false) {
            return Err(bad("Logic Pro keeps no versions: give path"));
        }
        let now = self.project_path();
        match params.get("path").filter(|path| !path.is_null()) {
            None => Ok(json!({"epoch": self.epoch_text(), "saved": now, "dirty": false})),
            Some(path) => {
                let path = path.as_str().filter(|p| !p.is_empty()).ok_or_else(|| bad("path must be a whole path to a .logicx package"))?;
                if !path.starts_with('/') || path.split('/').any(|part| part == "..") {
                    return Err(bad("path must be a whole path, with no \"..\" in it"));
                }
                if !path.ends_with(".logicx") {
                    return Err(bad("path must name a .logicx package"));
                }
                if path == now || self.saved_paths.iter().any(|saved| saved == path) {
                    return Err(bad(format!("{path} is there already: Kumi never saves over a package")));
                }
                self.saved_as = Some(path.to_string());
                self.saved_paths.push(path.to_string());
                Ok(json!({"epoch": self.epoch_text(), "saved": path, "dirty": false}))
            }
        }
    }

    /// The package the project is saved as now: the recorded one's, or the one Kumi saved it as.
    fn project_path(&self) -> String {
        self.saved_as.clone().unwrap_or_else(|| {
            another_project(&serde_json::to_value(project()).unwrap_or(Value::Null), self.project)["path"]
                .as_str()
                .unwrap_or_default()
                .to_string()
        })
    }

    /// `key-command {command}`: one of Logic's key commands by its English name (or id), posted with Logic in front;
    /// Logic's history takes a step for an edit (Create Marker, a split) and none for a selection, which the apply says
    /// (`applied.changed: false`, `display.unchanged`, `undo.entry: false`: no row). A restore is Logic's own undo's
    /// business (`daw-only`). The commands that open a dialog or leave the project are refused.
    fn key_command(&mut self, method: &str, params: &Value) -> Answer {
        if method == "change_restore" {
            return Err((
                "daw-only".into(),
                "Kumi can't take back what a key command did: Logic Pro's own undo can take it back (Cmd-Z in Logic Pro)".into(),
            ));
        }
        let input = &params["input"];
        let (name, step) = match input.get("command") {
            Some(Value::Number(id)) => {
                return Err(bad(format!("Logic Pro has no key command with id {id} (the simulated Logic Pro knows commands by name)")))
            }
            Some(Value::String(text)) => {
                let text = text.trim();
                if text.is_empty() {
                    return Err(bad("command: a key command's English name as the Key Commands window lists it, or its id"));
                }
                if text == "Undo" || text == "Redo" {
                    return Err(bad(format!("{} moves Logic Pro's undo history under Kumi's HISTORY: Kumi's own changes are taken back by Kumi's undo, anything else is the producer's to undo or redo in Logic Pro", quoted(text))));
                }
                if KEY_REFUSED.iter().any(|refused| refused.eq_ignore_ascii_case(text)) || text.ends_with('…') || text.ends_with("...") {
                    return Err(bad(format!(
                        "{} opens a dialog or leaves the project: Kumi won't run it as a key command (save_project saves)",
                        quoted(text)
                    )));
                }
                match KEY_COMMANDS.iter().find(|(known, _)| known.eq_ignore_ascii_case(text)) {
                    Some((known, step)) => (known.to_string(), *step),
                    None => {
                        return Err(bad(format!(
                            "Logic Pro has no key command named {} (as the Key Commands window lists it)",
                            quoted(text)
                        )))
                    }
                }
            }
            _ => return Err(bad("command: a key command's English name as the Key Commands window lists it, or its id")),
        };
        let title = format!("Ran key command {}", quoted(&name));
        if method == "change_preview" {
            return Ok(json!({
                "epoch": self.epoch_text(),
                "target": {"kind": "action", "command": name},
                "prior": {},
                "fence": {"command": name},
                "display": {"title": title, "note": KEY_NOTE},
            }));
        }
        if params["fence"]["command"] != name {
            return Err(bad("the key command isn't the one previewed"));
        }
        let desc = Self::desc(params)?;
        // What the command does to the project: a marker at the playhead, the selected region split at the playhead
        // when it is inside one; a selection changes nothing the project keeps.
        let edited = match name.as_str() {
            "Create Marker" => {
                let beats = on_the_beat(self.playhead);
                let marker = Marker { name: format!("Marker {}", self.next_marker), position_beats: beats, length: MARKER_LENGTH };
                self.next_marker += 1;
                self.markers.push(marker);
                self.sort_markers();
                true
            }
            "Split Regions/Events at Playhead" => {
                let playhead = self.playhead;
                match self.regions.iter().position(|region| region.selected && region.start < playhead && playhead < region.end) {
                    Some(at) => {
                        self.split_region(at, playhead);
                        true
                    }
                    None => false,
                }
            }
            _ => false,
        };
        let step = step.filter(|_| edited);
        if step.is_none() {
            return Ok(json!({
                "epoch": self.epoch_text(),
                "applied": {"command": name, "changed": false},
                "display": {"title": title, "note": KEY_NO_STEP, "unchanged": true},
                "undo": {"desc": desc, "entry": false},
            }));
        }
        let undo = self.applied(step, &desc)?;
        Ok(json!({
            "epoch": self.epoch_text(),
            "applied": {"command": name, "changed": true},
            "display": {"title": title, "note": KEY_NOTE},
            "undo": undo,
        }))
    }

    fn desc(params: &Value) -> Result<String, Fault> {
        params["desc"].as_str().map(str::to_owned).ok_or_else(|| bad("desc must be a string"))
    }

    fn track_prop(&mut self, prop: &Prop, method: &str, params: &Value) -> Answer {
        let (reference, input) = match method {
            "change_restore" => (params["target"]["ref"].as_str(), &params["prior"]),
            _ => (params["input"]["ref"].as_str(), &params["input"]),
        };
        let at = self.track_at(reference, method == "change_restore")?;
        let (now, name) = (self.tracks[at].state(prop.field), self.tracks[at].name.clone());
        let epoch = self.epoch_text();
        match method {
            "change_preview" => {
                let wanted = prop.wanted(input)?;
                let mut display = json!({"title": prop.title(&name, &now, &wanted), "before": now, "after": wanted});
                if prop.same(&now, &wanted) {
                    display["unchanged"] = true.into();
                }
                Ok(json!({"epoch": epoch, "target": {"ref": reference, "kind": "track"}, "prior": now, "fence": now, "display": display}))
            }
            "change_apply" => {
                if !prop.same(&now, &params["fence"]) {
                    return Err(changed(format!("{name}'s {} changed since the preview", prop.noun)));
                }
                let wanted = prop.wanted(input)?;
                let desc = Self::desc(params)?;
                self.tracks[at].write(prop.field, &wanted);
                let after = self.tracks[at].state(prop.field);
                let undo = self.applied(prop.step, &desc)?;
                // (a renamed header moves the epoch, as the observers count it)
                if prop.step.is_some() && !prop.same(&now, &after) {
                    self.bump_epoch();
                }
                Ok(
                    json!({"epoch": self.epoch_text(), "applied": after, "display": {"title": prop.title(&name, &now, &after), "before": now, "after": after}, "undo": undo}),
                )
            }
            _ => {
                let prior = &params["prior"];
                let desc = Self::desc(params)?;
                if !prop.same(&now, prior) {
                    if !prop.same(&now, &params["applied"]) && params["discard"] != true {
                        return Err(changed(format!("{name}'s {} changed in Logic Pro since Kumi set it", prop.noun)));
                    }
                    self.tracks[at].write(prop.field, prior);
                }
                let after = self.tracks[at].state(prop.field);
                let undo = self.restored(prop.step.filter(|_| !prop.same(&now, prior)), &desc)?;
                if prop.step.is_some() && !prop.same(&now, &after) {
                    self.bump_epoch();
                }
                let title = format!("{} (put back)", prop.title(&name, &now, &after));
                Ok(
                    json!({"epoch": self.epoch_text(), "restored": after, "display": {"title": title, "before": now, "after": after}, "undo": undo}),
                )
            }
        }
    }

    /// `track-add {kind?, index?, name?}`: a track below the one `index` names (the last by default), as Logic's Track
    /// menu makes one, which moves the epoch; its ref in `created`. Its restore deletes it by value, unless it's gone
    /// or holds regions since.
    fn track_add(&mut self, method: &str, params: &Value) -> Answer {
        if method == "change_restore" {
            let created = params["created"]["ref"].as_str();
            let at = self.track_at(created, true)?;
            let name = params["applied"]["name"].as_str().unwrap_or_default().to_string();
            if self.tracks[at].name != name {
                return Err(gone(format!("the track Kumi added ({}) isn't at place {} any more", quoted(&name), at + 1)));
            }
            if params["discard"] != true {
                let held = self.regions_of(&self.tracks[at]);
                if held > 0 {
                    return Err(changed(format!("the track Kumi added ({}) holds {held} region(s) now", quoted(&name))));
                }
            }
            let desc = Self::desc(params)?;
            self.tracks.remove(at);
            self.bump_epoch();
            let undo = self.restored(Some("Delete Tracks"), &desc)?;
            return Ok(json!({
                "epoch": self.epoch_text(),
                "restored": {"deleted": created},
                "display": {"title": format!("{} taken away again", quoted(&name))},
                "undo": undo,
            }));
        }
        let input = &params["input"];
        let count = self.tracks.len();
        let kind = match input.get("kind").filter(|kind| !kind.is_null()) {
            None => "audio".to_string(),
            Some(Value::String(kind)) if ["audio", "instrument", "external"].contains(&kind.as_str()) => kind.clone(),
            Some(_) => return Err(bad("kind must be audio, instrument or external")),
        };
        let after = match input.get("index").filter(|index| !index.is_null()) {
            None => count,
            Some(index) => {
                let index = index
                    .as_u64()
                    .map(|i| i as usize)
                    .filter(|i| *i <= count)
                    .ok_or_else(|| bad(format!("index must be a whole number from 1 to {count}")))?;
                if index == 0 {
                    return Err(bad(
                        "Logic Pro adds a track below the selected one, so index 0 (above every track) isn't reachable: use 1 or more",
                    ));
                }
                index
            }
        };
        if after == 0 {
            return Err(failed("Logic Pro has no track to add one after: the project has none"));
        }
        let name = match input.get("name") {
            None | Some(Value::Null) => None,
            Some(Value::String(name)) if !name.trim().is_empty() && name.chars().count() <= 256 && !name.chars().any(char::is_control) => {
                Some(name.clone())
            }
            Some(_) => return Err(bad("name must be one line of at most 256 characters")),
        };
        let names: Vec<String> = self.tracks.iter().map(|track| track.name.clone()).collect();
        let fence = json!({"tracks": count, "names": names});
        match method {
            "change_preview" => {
                let title = match &name {
                    Some(name) => format!("Added {kind} track {}", quoted(name)),
                    None => format!(
                        "Added {} track",
                        match kind.as_str() {
                            "external" => "an external MIDI",
                            "audio" => "an audio",
                            _ => "an instrument",
                        }
                    ),
                };
                Ok(json!({
                    "epoch": self.epoch_text(),
                    "target": {"ref": null, "kind": "track", "index": after},
                    "prior": {"tracks": count},
                    "fence": fence,
                    "display": {"title": title, "index": after},
                }))
            }
            _ => {
                if params["fence"] != fence {
                    return Err(changed("the tracks changed since the preview"));
                }
                let desc = Self::desc(params)?;
                let named = name.unwrap_or_else(|| {
                    format!(
                        "{} {}",
                        match kind.as_str() {
                            "audio" => "Audio",
                            "instrument" => "Inst",
                            _ => "MIDI",
                        },
                        count + 1
                    )
                });
                let made = Track::made(self.next_track, &named, &kind);
                self.next_track += 1;
                self.tracks.insert(after, made);
                self.bump_epoch();
                let place = after + 1;
                let undo = self.applied(Some("Create Track"), &desc)?;
                Ok(json!({
                    "epoch": self.epoch_text(),
                    "applied": {"index": place, "name": named, "kind": kind},
                    "created": {"ref": format!("{}:track:{place}", self.epoch), "kind": "track"},
                    "display": {"title": format!("Added {kind} track {}", quoted(&named))},
                    "undo": undo,
                }))
            }
        }
    }

    /// `track-delete {ref}`: the track goes (Delete Track, its sheet answered), which moves the epoch; what it was is
    /// kept in `material`. Its restore is Logic's own undo (Cmd-Z), sent only while the Edit menu's Undo title is this
    /// very step: a track can't be made again by value with what it held.
    fn track_delete(&mut self, method: &str, params: &Value) -> Answer {
        if method == "change_restore" {
            let material = &params["material"];
            let desc = Self::desc(params)?;
            let name = material["name"].as_str().unwrap_or_default().to_string();
            let place = material["index"].as_u64().unwrap_or(0) as usize;
            if self.tracks.get(place.wrapping_sub(1)).is_some_and(|track| track.name == name) {
                return Ok(json!({
                    "epoch": self.epoch_text(),
                    "restored": {"ref": format!("{}:track:{place}", self.epoch)},
                    "display": {"title": format!("{} is back already", quoted(&name))},
                    "undo": {"desc": desc, "entry": false},
                }));
            }
            let step = material["step"].as_str().unwrap_or_default().to_string();
            let pressed = self.undo_in_logic("Delete Tracks", |top| top == step)?;
            if self.restore_unanswered() {
                return Err(uncertain(format!("Logic Pro's {pressed:?} was pressed, but {} isn't back at place {place}", quoted(&name))));
            }
            let track = self.deleted.remove(&step).unwrap_or_else(|| {
                let mut track = Track::made(0, &name, material["kind"].as_str().unwrap_or("audio"));
                track.volume_db = material["volumeDb"].as_f64().unwrap_or(0.0);
                track.pan_steps = (material["pan"].as_f64().unwrap_or(0.0) * PAN_CENTRE as f64).round() as i64;
                track.mute = material["mute"] == true;
                track.solo = material["solo"] == true;
                track.armed = material["arm"] == true;
                track
            });
            let place = place.clamp(1, self.tracks.len() + 1);
            self.tracks.insert(place - 1, track);
            self.bump_epoch();
            return Ok(json!({
                "epoch": self.epoch_text(),
                "restored": {"ref": format!("{}:track:{place}", self.epoch), "was": params["target"]["ref"], "pressed": pressed},
                "display": {"title": format!("{} put back", quoted(&name)), "note": "Put back with Logic Pro's own undo."},
                "undo": {"desc": desc, "entry": false, "how": "undo"},
            }));
        }
        let at =
            self.track_at(params["input"]["ref"].as_str(), false)
                .map_err(|fault| if fault.0 == "failed" { changed(fault.1) } else { fault })?;
        let track = self.tracks[at].clone();
        let regions = self.regions_of(&track);
        let fence = json!({"name": track.name, "index": at + 1, "regions": regions});
        let title = format!("Deleted track {}", quoted(&track.name));
        match method {
            "change_preview" => {
                let mut display = json!({"title": title});
                if regions > 0 {
                    display["note"] =
                        format!("It holds {regions} region(s); only Logic Pro's own undo brings a deleted track back.").into();
                }
                Ok(json!({
                    "epoch": self.epoch_text(),
                    "target": {"ref": params["input"]["ref"], "kind": "track"},
                    "prior": fence,
                    "fence": fence,
                    "display": display,
                }))
            }
            _ => {
                if params["fence"] != fence {
                    return Err(changed(format!("{} changed since the preview", track.name)));
                }
                let desc = Self::desc(params)?;
                let material = json!({
                    "name": track.name, "kind": track.kind, "index": at + 1, "regions": regions,
                    "volumeDb": track.volume_db, "pan": pan_state(track.pan_steps)["pan"],
                    "mute": track.mute, "solo": track.solo, "arm": track.armed, "step": desc,
                });
                let gone = self.tracks.remove(at);
                self.deleted.insert(desc.clone(), gone);
                self.bump_epoch();
                let undo = self.applied(Some("Delete Tracks"), &desc)?;
                Ok(
                    json!({"epoch": self.epoch_text(), "applied": {"deleted": true}, "material": material, "display": {"title": title}, "undo": undo}),
                )
            }
        }
    }

    /// A marker's position from an input: `positionBeats`, beats from the project's start (seconds are refused).
    fn position_input(input: &Value) -> Result<f64, Fault> {
        if input.get("position").is_some_and(|v| !v.is_null()) && input.get("positionBeats").is_none_or(Value::is_null) {
            return Err(bad("Logic Pro's bridge places markers by positionBeats (beats from the project's start), not seconds"));
        }
        input
            .get("positionBeats")
            .and_then(Value::as_f64)
            .filter(|n| n.is_finite() && (0.0..=1.0e6).contains(n))
            .ok_or_else(|| bad("positionBeats must be a number from 0: beats from the project's start"))
    }

    /// `marker-add {positionBeats}`: a marker at the playhead moved there (on the beat), from the Marker List's Create
    /// new Marker; Logic names it. Its restore deletes the row at its place, if the marker there is still at that
    /// position.
    fn marker_add(&mut self, method: &str, params: &Value) -> Answer {
        if method == "change_restore" {
            let created = params["created"]["ref"].as_str();
            let place = self.marker_place(created)?;
            let desc = Self::desc(params)?;
            let Some(marker) = self.markers.get(place - 1).cloned() else {
                return Err(gone("the marker Kumi added isn't in the Marker List any more"));
            };
            if params["discard"] != true && marker.state()["position"] != params["applied"]["position"] {
                return Err(changed(format!("{} has been moved in Logic Pro since Kumi added it", marker.label())));
            }
            self.markers.remove(place - 1);
            let undo = self.restored(Some("Delete"), &desc)?;
            return Ok(json!({
                "epoch": self.epoch_text(),
                "restored": {"deleted": created},
                "display": {"title": format!("{} taken away again", marker.label())},
                "undo": undo,
            }));
        }
        let input = &params["input"];
        if input.get("name").is_some_and(|v| !v.is_null()) {
            return Err(bad(
                "Logic Pro names markers itself (Marker 1, Marker 2, in order): a marker can't be named from outside, so leave name out",
            ));
        }
        let asked = Self::position_input(input)?;
        let beats = on_the_beat(asked);
        let count = self.markers.len();
        match method {
            "change_preview" => {
                let mut display = json!({"title": format!("Marker at {}", bar_text(beats))});
                if asked != beats {
                    display["note"] = "Logic Pro's playhead display places a marker on a beat: the division and tick are dropped.".into();
                }
                Ok(json!({
                    "epoch": self.epoch_text(),
                    "target": {"ref": null, "kind": "marker"},
                    "prior": {"markers": count},
                    "fence": {"markers": count},
                    "display": display,
                }))
            }
            _ => {
                if params["fence"] != json!({"markers": count}) {
                    return Err(changed("the markers changed since the preview"));
                }
                let desc = Self::desc(params)?;
                let marker = Marker { name: format!("Marker {}", self.next_marker), position_beats: beats, length: MARKER_LENGTH };
                self.next_marker += 1;
                self.markers.push(marker.clone());
                self.sort_markers();
                let place = self.marker_at(beats).map_or(0, |at| at + 1);
                let undo = self.applied(Some("Create Marker"), &desc)?;
                Ok(json!({
                    "epoch": self.epoch_text(),
                    "applied": marker.state(),
                    "created": {"ref": self.marker_ref(place), "kind": "marker"},
                    "display": {"title": format!("Marker {} at {}", quoted(&marker.name), bar_text(beats))},
                    "undo": undo,
                }))
            }
        }
    }

    /// `marker-move {ref, positionBeats}`: the row's position scrubber, a segment at a time; the list keeps its rows in
    /// order of position. Its restore moves the marker back from where Kumi left it.
    fn marker_move(&mut self, method: &str, params: &Value) -> Answer {
        if method == "change_restore" {
            let (prior, applied) = (beats_of(&params["prior"]["position"]), beats_of(&params["applied"]["position"]));
            let desc = Self::desc(params)?;
            if self.marker_at(prior).is_some() && self.marker_at(applied).is_none() {
                return Ok(json!({
                    "epoch": self.epoch_text(),
                    "restored": {"position": position_of(prior)},
                    "display": {"title": "The marker is where it was"},
                    "undo": {"desc": desc, "entry": false},
                }));
            }
            let Some(at) = self.marker_at(applied) else {
                return Err(if params["discard"] == true {
                    gone("the marker Kumi moved isn't where it left it, nor where it was")
                } else {
                    changed("the marker has been moved in Logic Pro since Kumi moved it")
                });
            };
            let label = self.markers[at].label();
            self.markers[at].position_beats = prior;
            self.sort_markers();
            let undo = self.restored(Some("Value Change"), &desc)?;
            return Ok(json!({
                "epoch": self.epoch_text(),
                "restored": {"position": position_of(prior)},
                "display": {"title": format!("{label} back at {}", bar_text(prior))},
                "undo": undo,
            }));
        }
        let input = &params["input"];
        let place = self.marker_place(input["ref"].as_str())?;
        let to = on_the_beat(Self::position_input(input)?);
        let Some(marker) = self.markers.get(place - 1).cloned() else {
            return Err(match method {
                "change_preview" => {
                    failed(format!("{} isn't in the project (deleted, or another project)", input["ref"].as_str().unwrap_or_default()))
                }
                _ => changed("the marker went since the preview"),
            });
        };
        let now = json!({"position": position_of(marker.position_beats)});
        let title = format!("Moved {} to {}", marker.label(), bar_text(to));
        match method {
            "change_preview" => {
                let mut display = json!({"title": title, "before": now, "after": {"position": position_of(to)}});
                if marker.position_beats == to {
                    display["unchanged"] = true.into();
                }
                Ok(
                    json!({"epoch": self.epoch_text(), "target": {"ref": input["ref"], "kind": "marker"}, "prior": now, "fence": now, "display": display}),
                )
            }
            _ => {
                if params["fence"] != now {
                    return Err(changed(format!("{} moved since the preview", marker.label())));
                }
                let desc = Self::desc(params)?;
                self.markers[place - 1].position_beats = to;
                self.sort_markers();
                let undo = self.applied(Some("Value Change"), &desc)?;
                Ok(json!({
                    "epoch": self.epoch_text(),
                    "applied": {"position": position_of(to)},
                    "display": {"title": title, "before": now, "after": {"position": position_of(to)}},
                    "undo": undo,
                }))
            }
        }
    }

    /// `marker-delete {ref}`: the row's Delete action. Its restore is Logic's own undo while that step is on top, else
    /// the marker made again by value at its place, numbered by Logic.
    fn marker_delete(&mut self, method: &str, params: &Value) -> Answer {
        if method == "change_restore" {
            let material = &params["material"];
            let (name, beats, length) = (
                material["name"].as_str().unwrap_or_default().to_string(),
                beats_of(&material["position"]),
                material["length"].as_f64().unwrap_or(MARKER_LENGTH),
            );
            let desc = Self::desc(params)?;
            // Logic's own undo brings it back whole, name and all, while the Edit menu still names Kumi's deletion first.
            if let Ok(pressed) = self.undo_in_logic("Delete", |top| top.starts_with("Kumi: Deleted marker")) {
                if self.restore_unanswered() {
                    return Err(uncertain(format!("Logic Pro's {pressed:?} was pressed, but no marker is back at {}", bar_text(beats))));
                }
                self.markers.push(Marker { name: name.clone(), position_beats: beats, length });
                self.sort_markers();
                let place = self.marker_at(beats).map_or(0, |at| at + 1);
                return Ok(json!({
                    "epoch": self.epoch_text(),
                    "restored": {"ref": self.marker_ref(place), "was": params["target"]["ref"], "pressed": pressed},
                    "display": {"title": format!("Marker {} put back", quoted(&name)), "note": "Put back with Logic Pro's own undo."},
                    "undo": {"desc": desc, "entry": false, "how": "undo"},
                }));
            }
            // Else by value: a marker made again at its place; Logic numbers it itself.
            if let Some(at) = self.marker_at(beats) {
                if params["discard"] == true || self.markers[at].name == name {
                    return Ok(json!({
                        "epoch": self.epoch_text(),
                        "restored": {"ref": self.marker_ref(at + 1)},
                        "display": {"title": format!("Marker {} is back already", quoted(&name))},
                        "undo": {"desc": desc, "entry": false},
                    }));
                }
                return Err(changed(format!("another marker ({}) is at {} now", quoted(&self.markers[at].name), bar_text(beats))));
            }
            let made = Marker { name: format!("Marker {}", self.next_marker), position_beats: beats, length };
            self.next_marker += 1;
            self.markers.push(made.clone());
            self.sort_markers();
            let place = self.marker_at(beats).map_or(0, |at| at + 1);
            let undo = self.restored(Some("Create Marker"), &desc)?;
            return Ok(json!({
                "epoch": self.epoch_text(),
                "restored": {"ref": self.marker_ref(place), "name": made.name},
                "display": {"title": format!("Marker {} put back as {}", quoted(&name), quoted(&made.name)), "note": "Logic Pro numbers a marker made again itself."},
                "undo": undo,
            }));
        }
        let input = &params["input"];
        let place = self.marker_place(input["ref"].as_str())?;
        let Some(marker) = self.markers.get(place - 1).cloned() else {
            return Err(match method {
                "change_preview" => {
                    failed(format!("{} isn't in the project (deleted, or another project)", input["ref"].as_str().unwrap_or_default()))
                }
                _ => changed("the marker went since the preview"),
            });
        };
        let title = format!("Deleted {}", marker.label());
        match method {
            "change_preview" => Ok(json!({
                "epoch": self.epoch_text(),
                "target": {"ref": input["ref"], "kind": "marker"},
                "prior": marker.state(),
                "fence": marker.state(),
                "display": {"title": title},
            })),
            _ => {
                if params["fence"] != marker.state() {
                    return Err(changed(format!("{} changed since the preview", marker.label())));
                }
                let desc = Self::desc(params)?;
                self.markers.remove(place - 1);
                let undo = self.applied(Some("Delete"), &desc)?;
                Ok(json!({
                    "epoch": self.epoch_text(),
                    "applied": {"deleted": true},
                    "material": marker.state(),
                    "display": {"title": title},
                    "undo": undo,
                }))
            }
        }
    }

    /// `tempo-set {bpm}`: the LCD's tempo slider, whole BPM; no step of Logic's undo. Its restore is by value.
    fn tempo_set(&mut self, method: &str, params: &Value) -> Answer {
        let now = json!({"bpm": self.tempo});
        let same = |a: &Value, b: &Value| (a["bpm"].as_f64().unwrap_or(f64::NAN) - b["bpm"].as_f64().unwrap_or(f64::NAN)).abs() < 0.01;
        let epoch = self.epoch_text();
        if method == "change_restore" {
            let prior = params["prior"]["bpm"].as_f64().ok_or_else(|| bad("prior.bpm"))?;
            let desc = Self::desc(params)?;
            if !same(&now, &params["prior"]) {
                if !same(&now, &params["applied"]) && params["discard"] != true {
                    return Err(changed("the tempo has been changed in Logic Pro since Kumi set it"));
                }
                self.tempo = prior;
            }
            let undo = self.restored(None, &desc)?;
            return Ok(
                json!({"epoch": epoch, "restored": {"bpm": prior}, "display": {"title": format!("Tempo back to {prior} BPM"), "before": now, "after": {"bpm": prior}}, "undo": undo}),
            );
        }
        let asked = params["input"]["bpm"].as_f64().filter(|bpm| bpm.is_finite()).ok_or_else(|| bad("bpm must be a number"))?;
        if !(TEMPO_MIN..=TEMPO_MAX).contains(&asked) {
            return Err(bad(format!("bpm must be from {TEMPO_MIN} to {TEMPO_MAX}")));
        }
        let wanted = asked.round();
        match method {
            "change_preview" => {
                let mut display = json!({"title": format!("Tempo {wanted} BPM"), "before": now, "after": {"bpm": wanted}});
                if asked != wanted {
                    display["note"] = format!(
                        "Logic Pro's tempo display takes whole BPM from outside: {} was rounded to {wanted}.",
                        params["input"]["bpm"]
                    )
                    .into();
                }
                if same(&now, &json!({"bpm": wanted})) {
                    display["unchanged"] = true.into();
                }
                Ok(json!({"epoch": epoch, "target": {"ref": null, "kind": "project"}, "prior": now, "fence": now, "display": display}))
            }
            _ => {
                if !same(&now, &params["fence"]) {
                    return Err(changed("the tempo changed since the preview"));
                }
                let desc = Self::desc(params)?;
                self.tempo = wanted;
                let undo = self.applied(None, &desc)?;
                Ok(
                    json!({"epoch": epoch, "applied": {"bpm": wanted}, "display": {"title": format!("Tempo {wanted} BPM"), "before": now, "after": {"bpm": wanted}}, "undo": undo}),
                )
            }
        }
    }

    // ----- (M4) regions -----

    /// The region a preview or an apply names (a ref of the epoch now; one gone since the preview is `changed`).
    fn region_for(&self, method: &str, input: &Value) -> Result<usize, Fault> {
        self.region_at(input["ref"].as_str(), false).map_err(|fault| match (method, fault.0.as_str()) {
            ("change_apply", "failed") => changed("the region went since the preview"),
            _ => fault,
        })
    }

    /// A region's place as the bridge reads it from the Tracks area: its edges and the track it sits on.
    fn region_place_state(&self, region: &Region) -> Value {
        json!({"startBeats": region.start, "endBeats": region.end, "trackRef": self.track_ref_of(region.track)})
    }

    /// Two places the same: the edges exactly, and the tracks by where they stand now (a restore's trackRef is from the
    /// change's own epoch, carried).
    fn same_place(&self, a: &Value, b: &Value) -> bool {
        let track = |state: &Value| self.track_at(state["trackRef"].as_str(), true).ok();
        same_bounds(a, b) && track(a).is_some() && track(a) == track(b)
    }

    /// The region at `at` put on a track at an edge, the Tracks area's order kept; its place now (from 0).
    fn place_region(&mut self, at: usize, track: u64, start: f64, end: f64) -> usize {
        let id = self.regions[at].id;
        let region = &mut self.regions[at];
        region.track = track;
        region.start = start;
        region.end = end;
        self.sort_regions();
        self.regions.iter().position(|region| region.id == id).unwrap_or(at)
    }

    /// `region-move {ref, positionBeats?, toRef?}`: the region moved to a beat (on the beat), onto another track, or
    /// both, through the Event List's region row; it keeps its length. The epoch moves, as M2's observers count a
    /// region moved. Its restore moves it back, by value.
    fn region_move(&mut self, method: &str, params: &Value) -> Answer {
        if method == "change_restore" {
            let at = self.region_at(params["target"]["ref"].as_str(), true)?;
            let desc = Self::desc(params)?;
            let (now, label) = (self.region_place_state(&self.regions[at]), self.regions[at].label());
            if self.same_place(&now, &params["prior"]) {
                return Ok(
                    json!({"epoch": self.epoch_text(), "restored": now, "display": {"title": format!("{label} is where it was")}, "undo": {"desc": desc, "entry": false}}),
                );
            }
            if !self.same_place(&now, &params["applied"]) && params["discard"] != true {
                return Err(changed(format!("{label} has been moved in Logic Pro since Kumi moved it")));
            }
            let track = self
                .track_at(params["prior"]["trackRef"].as_str(), true)
                .map_err(|_| gone("the track the region was on isn't in the project any more"))?;
            let (start, end) = (number(&params["prior"], "startBeats"), number(&params["prior"], "endBeats"));
            let at = self.place_region(at, self.tracks[track].id, start, end);
            self.bump_epoch();
            let undo = self.restored(Some("Value Change"), &desc)?;
            let back = self.region_place_state(&self.regions[at]);
            return Ok(
                json!({"epoch": self.epoch_text(), "restored": back, "display": {"title": format!("{label} back at {}", bar_text(start))}, "undo": undo}),
            );
        }
        let input = &params["input"];
        let at = self.region_for(method, input)?;
        let region = self.regions[at].clone();
        let now = self.region_place_state(&region);
        let to_beat = match input.get("positionBeats").filter(|v| !v.is_null()) {
            None => None,
            Some(_) => Some(on_the_beat(Self::position_input(input)?)),
        };
        let to_track = match input.get("toRef").filter(|v| !v.is_null()) {
            None => None,
            Some(to) => Some(self.track_at(to.as_str(), false).map_err(|fault| {
                if fault.0 == "failed" {
                    bad(format!("toRef: {}", fault.1))
                } else {
                    fault
                }
            })?),
        };
        if to_beat.is_none() && to_track.is_none() {
            return Err(bad("pass positionBeats (beats from the project's start), toRef (another track) or both"));
        }
        if to_track.is_some_and(|t| self.tracks[t].is_stack()) {
            return Err(bad("a Track Stack's own track holds no regions: move it onto one of the stack's tracks"));
        }
        let start = to_beat.unwrap_or(region.start);
        let end = start + (region.end - region.start);
        let track = to_track.map_or(region.track, |t| self.tracks[t].id);
        let wanted = json!({"startBeats": start, "endBeats": end, "trackRef": self.track_ref_of(track)});
        let mut title = format!("Moved {} to {}", region.label(), bar_text(start));
        if let Some(t) = to_track {
            title.push_str(&format!(" on {}", quoted(&self.tracks[t].name)));
        }
        match method {
            "change_preview" => {
                let mut display = json!({"title": title, "before": now, "after": wanted});
                if self.same_place(&now, &wanted) {
                    display["unchanged"] = true.into();
                }
                Ok(
                    json!({"epoch": self.epoch_text(), "target": {"ref": input["ref"], "kind": "region"}, "prior": now, "fence": now, "display": display}),
                )
            }
            _ => {
                if !self.same_place(&now, &params["fence"]) {
                    return Err(changed(format!("{} moved since the preview", region.label())));
                }
                let desc = Self::desc(params)?;
                let at = self.place_region(at, track, start, end);
                self.bump_epoch();
                let undo = self.applied(Some("Value Change"), &desc)?;
                let after = self.region_place_state(&self.regions[at]);
                Ok(
                    json!({"epoch": self.epoch_text(), "applied": after, "display": {"title": title, "before": now, "after": after}, "undo": undo}),
                )
            }
        }
    }

    /// The regions sorted again after an edge moved: whether their order (their places) changed, which moves the epoch.
    fn resort_regions(&mut self) -> bool {
        let before: Vec<u64> = self.regions.iter().map(|region| region.id).collect();
        self.sort_regions();
        let moved = self.regions.iter().map(|region| region.id).ne(before.into_iter());
        if moved {
            self.bump_epoch();
        }
        moved
    }

    /// `region-resize {ref, startBeats?, endBeats?}`: the region's edges, through the Region inspector's Position and
    /// Length; its notes stay where they are in time. Its restore puts the edges back, by value.
    fn region_resize(&mut self, method: &str, params: &Value) -> Answer {
        if method == "change_restore" {
            let at = self.region_at(params["target"]["ref"].as_str(), true)?;
            let desc = Self::desc(params)?;
            let (now, label, id) = (self.regions[at].bounds(), self.regions[at].label(), self.regions[at].id);
            if same_bounds(&now, &params["prior"]) {
                return Ok(
                    json!({"epoch": self.epoch_text(), "restored": now, "display": {"title": format!("{label} is as it was")}, "undo": {"desc": desc, "entry": false}}),
                );
            }
            if !same_bounds(&now, &params["applied"]) && params["discard"] != true {
                return Err(changed(format!("{label} has been resized in Logic Pro since Kumi resized it")));
            }
            let (start, end) = (number(&params["prior"], "startBeats"), number(&params["prior"], "endBeats"));
            self.regions[at].start = start;
            self.regions[at].end = end;
            self.resort_regions();
            let undo = self.restored(Some("Value Change"), &desc)?;
            let back = self.regions.iter().find(|region| region.id == id).map(Region::bounds).unwrap_or(Value::Null);
            return Ok(
                json!({"epoch": self.epoch_text(), "restored": back, "display": {"title": format!("{label} back to {} to {}", bar_text(start), bar_text(end))}, "undo": undo}),
            );
        }
        let input = &params["input"];
        let at = self.region_for(method, input)?;
        let region = self.regions[at].clone();
        let edge = |key: &str| -> Result<Option<f64>, Fault> {
            match input.get(key) {
                None | Some(Value::Null) => Ok(None),
                Some(value) => value
                    .as_f64()
                    .filter(|n| n.is_finite() && (0.0..=1.0e6).contains(n))
                    .map(Some)
                    .ok_or_else(|| bad(format!("{key} must be a number from 0: beats from the project's start"))),
            }
        };
        let (start, end) = (edge("startBeats")?, edge("endBeats")?);
        if start.is_none() && end.is_none() {
            return Err(bad("pass startBeats, endBeats or both (beats from the project's start)"));
        }
        let (start, end) = (start.unwrap_or(region.start), end.unwrap_or(region.end));
        if end <= start {
            return Err(bad("the region must end after it starts"));
        }
        let now = region.bounds();
        let wanted = json!({"startBeats": start, "endBeats": end});
        let title = format!("Resized {}: {} to {}", region.label(), bar_text(start), bar_text(end));
        match method {
            "change_preview" => {
                let mut display = json!({"title": title, "before": now, "after": wanted});
                if same_bounds(&now, &wanted) {
                    display["unchanged"] = true.into();
                }
                Ok(
                    json!({"epoch": self.epoch_text(), "target": {"ref": input["ref"], "kind": "region"}, "prior": now, "fence": now, "display": display}),
                )
            }
            _ => {
                if !same_bounds(&now, &params["fence"]) {
                    return Err(changed(format!("{} changed since the preview", region.label())));
                }
                let desc = Self::desc(params)?;
                self.regions[at].start = start;
                self.regions[at].end = end;
                self.resort_regions();
                let undo = self.applied(Some("Value Change"), &desc)?;
                Ok(
                    json!({"epoch": self.epoch_text(), "applied": wanted, "display": {"title": title, "before": now, "after": wanted}, "undo": undo}),
                )
            }
        }
    }

    /// The region at `at` split at `beats` (inside it): the left part keeps the region's id, the right part is a new
    /// region after it with the notes from the split on; the epoch moves. The right part's id.
    fn split_region(&mut self, at: usize, beats: f64) -> u64 {
        let id = self.next_region;
        self.next_region += 1;
        let left = &mut self.regions[at];
        let cut = beats - left.start;
        let (kept, moved): (Vec<Value>, Vec<Value>) = left.notes.drain(..).partition(|note| note["beat"].as_f64().unwrap_or(0.0) < cut);
        left.notes = kept;
        let right = Region {
            id,
            track: left.track,
            name: left.name.clone(),
            kind: left.kind.clone(),
            start: beats,
            end: left.end,
            looped: left.looped,
            selected: false,
            notes: moved
                .into_iter()
                .map(|mut note| {
                    note["beat"] = (note["beat"].as_f64().unwrap_or(0.0) - cut).into();
                    note
                })
                .collect(),
        };
        left.end = beats;
        self.regions.push(right);
        self.sort_regions();
        self.bump_epoch();
        id
    }

    /// The parts of a split joined again: the right part's notes back in the left, the right part gone, the left's end
    /// back at `end`; the epoch moves. The left's place now (from 0).
    fn join_split(&mut self, left_at: usize, right_at: Option<usize>, end: f64) -> usize {
        let left_id = self.regions[left_at].id;
        if let Some(right_at) = right_at {
            let right = self.regions.remove(right_at);
            if let Some(left_at) = self.regions.iter().position(|region| region.id == left_id) {
                let cut = right.start - self.regions[left_at].start;
                for mut note in right.notes {
                    note["beat"] = (note["beat"].as_f64().unwrap_or(0.0) + cut).into();
                    self.regions[left_at].notes.push(note);
                }
            }
        }
        if let Some(left) = self.regions.iter_mut().find(|region| region.id == left_id) {
            left.end = end;
        }
        self.sort_regions();
        self.bump_epoch();
        self.regions.iter().position(|region| region.id == left_id).unwrap_or(0)
    }

    /// `region-split {ref, positionBeats}`: the region selected, the playhead put there, Split Regions/Events at
    /// Playhead: the left part keeps the ref's place, the right part is a new region (its ref in `created`); the epoch
    /// moves. Its restore is Logic's own undo while that step is on top, else the right part taken away and the left
    /// stretched back, while both are as the split left them.
    fn region_split(&mut self, method: &str, params: &Value) -> Answer {
        if method == "change_restore" {
            return self.region_split_restore(params);
        }
        let input = &params["input"];
        let at = self.region_for(method, input)?;
        let region = self.regions[at].clone();
        let beats = Self::position_input(input)?;
        if !(region.start < beats && beats < region.end) {
            return Err(bad(format!("positionBeats must be inside the region ({} to {})", bar_text(region.start), bar_text(region.end))));
        }
        let fence = json!({"startBeats": region.start, "endBeats": region.end, "name": region.name, "notes": region.notes.len()});
        let title = format!("Split {} at {}", region.label(), bar_text(beats));
        match method {
            "change_preview" => Ok(json!({
                "epoch": self.epoch_text(),
                "target": {"ref": input["ref"], "kind": "region"},
                "prior": fence,
                "fence": fence,
                "display": {"title": title},
            })),
            _ => {
                if params["fence"] != fence {
                    return Err(changed(format!("{} changed since the preview", region.label())));
                }
                let desc = Self::desc(params)?;
                let right = self.split_region(at, beats);
                let left_at = self.regions.iter().position(|r| r.id == region.id).unwrap_or(at);
                let right_at = self.regions.iter().position(|r| r.id == right).unwrap_or(left_at + 1);
                let undo = self.applied(Some("Split"), &desc)?;
                Ok(json!({
                    "epoch": self.epoch_text(),
                    "applied": {"left": self.regions[left_at].bounds(), "right": self.regions[right_at].bounds()},
                    "created": {"ref": self.region_ref(right_at + 1), "kind": "region"},
                    "display": {"title": title},
                    "undo": undo,
                }))
            }
        }
    }

    fn region_split_restore(&mut self, params: &Value) -> Answer {
        let desc = Self::desc(params)?;
        let left_at = self.region_at(params["target"]["ref"].as_str(), true)?;
        let right_at = self.region_at(params["created"]["ref"].as_str(), true).ok();
        let label = self.regions[left_at].label();
        let end = number(&params["prior"], "endBeats");
        if right_at.is_none() && self.regions[left_at].end == end {
            return Ok(
                json!({"epoch": self.epoch_text(), "restored": {"ref": self.region_ref(left_at + 1)}, "display": {"title": format!("{label} is whole already")}, "undo": {"desc": desc, "entry": false}}),
            );
        }
        // Logic's own undo while the split is on top: the parts join again.
        if let Ok(pressed) = self.undo_in_logic("Split", |top| top.starts_with("Kumi: Split region")) {
            if self.restore_unanswered() {
                return Err(uncertain(format!("Logic Pro's {pressed:?} was pressed, but {label} isn't whole")));
            }
            let place = self.join_split(left_at, right_at, end);
            return Ok(json!({
                "epoch": self.epoch_text(),
                "restored": {"ref": self.region_ref(place + 1), "was": params["target"]["ref"], "pressed": pressed},
                "display": {"title": format!("{label} whole again"), "note": "Put back with Logic Pro's own undo."},
                "undo": {"desc": desc, "entry": false, "how": "undo"},
            }));
        }
        if params["discard"] != true {
            let Some(right_at) = right_at else {
                return Err(changed(
                    "the right part of the split has been deleted in Logic Pro since: putting the region back whole would bring it back",
                ));
            };
            if !same_bounds(&self.regions[left_at].bounds(), &params["applied"]["left"]) {
                return Err(changed(format!("{label} has been changed in Logic Pro since Kumi split it")));
            }
            if !same_bounds(&self.regions[right_at].bounds(), &params["applied"]["right"]) {
                return Err(changed("the right part of the split has been changed in Logic Pro since Kumi split it"));
            }
        }
        let place = self.join_split(left_at, right_at, end);
        let undo = self.restored(Some("Delete"), &desc)?;
        Ok(
            json!({"epoch": self.epoch_text(), "restored": {"ref": self.region_ref(place + 1)}, "display": {"title": format!("{label} whole again")}, "undo": undo}),
        )
    }

    /// `region-loop {ref, loop}`: the Region inspector's Loop checkbox. Its restore is by value.
    fn region_loop(&mut self, method: &str, params: &Value) -> Answer {
        let title_of = |region: &Region, looped: bool| format!("{} {}", region.heading(), if looped { "looped" } else { "unlooped" });
        if method == "change_restore" {
            let at = self.region_at(params["target"]["ref"].as_str(), true)?;
            let desc = Self::desc(params)?;
            let (now, prior) = (json!({"looped": self.regions[at].looped}), params["prior"]["looped"] == true);
            if now["looped"] != prior {
                if now != params["applied"] && params["discard"] != true {
                    return Err(changed(format!("{}'s loop has been changed in Logic Pro since Kumi set it", self.regions[at].label())));
                }
                self.regions[at].looped = prior;
            }
            let undo = self.restored(Some("Value Change").filter(|_| now["looped"] != prior), &desc)?;
            let title = format!("{} (put back)", title_of(&self.regions[at], prior));
            return Ok(json!({"epoch": self.epoch_text(), "restored": {"looped": prior}, "display": {"title": title}, "undo": undo}));
        }
        let input = &params["input"];
        let at = self.region_for(method, input)?;
        let wanted = input.get("loop").and_then(Value::as_bool).ok_or_else(|| bad("loop must be true or false"))?;
        let region = self.regions[at].clone();
        let (now, after) = (json!({"looped": region.looped}), json!({"looped": wanted}));
        let title = title_of(&region, wanted);
        match method {
            "change_preview" => {
                let mut display = json!({"title": title, "before": now, "after": after});
                if region.looped == wanted {
                    display["unchanged"] = true.into();
                }
                Ok(
                    json!({"epoch": self.epoch_text(), "target": {"ref": input["ref"], "kind": "region"}, "prior": now, "fence": now, "display": display}),
                )
            }
            _ => {
                if params["fence"] != now {
                    return Err(changed(format!("{}'s loop changed since the preview", region.label())));
                }
                let desc = Self::desc(params)?;
                self.regions[at].looped = wanted;
                let undo = self.applied(Some("Value Change"), &desc)?;
                Ok(
                    json!({"epoch": self.epoch_text(), "applied": after, "display": {"title": title, "before": now, "after": after}, "undo": undo}),
                )
            }
        }
    }

    /// `region-rename {ref, name}`: the Event List's region row's name field, confirmed. Its restore is by value.
    fn region_rename(&mut self, method: &str, params: &Value) -> Answer {
        if method == "change_restore" {
            let at = self.region_at(params["target"]["ref"].as_str(), true)?;
            let desc = Self::desc(params)?;
            let prior = params["prior"]["name"].as_str().unwrap_or_default().to_string();
            let now = self.regions[at].name.clone();
            if now != prior {
                if Some(now.as_str()) != params["applied"]["name"].as_str() && params["discard"] != true {
                    return Err(changed(format!("{}'s name has been changed in Logic Pro since Kumi set it", self.regions[at].label())));
                }
                self.regions[at].name = prior.clone();
            }
            let undo = self.restored(Some("Renaming").filter(|_| now != prior), &desc)?;
            let title = format!("Renamed region {} → {} (put back)", quoted(&now), quoted(&prior));
            return Ok(json!({"epoch": self.epoch_text(), "restored": {"name": prior}, "display": {"title": title}, "undo": undo}));
        }
        let input = &params["input"];
        let at = self.region_for(method, input)?;
        let name = input.get("name").and_then(Value::as_str).ok_or_else(|| bad("name must be text"))?;
        if name.trim().is_empty() || name.chars().count() > 256 || name.chars().any(char::is_control) {
            return Err(bad("name must be one line of at most 256 characters"));
        }
        let now = json!({"name": self.regions[at].name});
        let title = format!("Renamed region {} → {}", quoted(&self.regions[at].name), quoted(name));
        match method {
            "change_preview" => {
                let mut display = json!({"title": title, "before": now, "after": {"name": name}});
                if self.regions[at].name == name {
                    display["unchanged"] = true.into();
                }
                Ok(
                    json!({"epoch": self.epoch_text(), "target": {"ref": input["ref"], "kind": "region"}, "prior": now, "fence": now, "display": display}),
                )
            }
            _ => {
                if params["fence"] != now {
                    return Err(changed(format!("{}'s name changed since the preview", self.regions[at].label())));
                }
                let desc = Self::desc(params)?;
                self.regions[at].name = name.to_string();
                let undo = self.applied(Some("Renaming"), &desc)?;
                Ok(
                    json!({"epoch": self.epoch_text(), "applied": {"name": name}, "display": {"title": title, "before": now, "after": {"name": name}}, "undo": undo}),
                )
            }
        }
    }

    /// `region-delete {ref}`: the region selected, Delete; the epoch moves, and what it was is kept in `material`.
    /// Its restore is Logic's own undo (Cmd-Z) while that step is on top: a region with its notes can't be made
    /// again by value.
    fn region_delete(&mut self, method: &str, params: &Value) -> Answer {
        if method == "change_restore" {
            let material = &params["material"];
            let desc = Self::desc(params)?;
            let name = material["name"].as_str().unwrap_or_default().to_string();
            let (start, end) = (number(material, "startBeats"), number(material, "endBeats"));
            let track = self.track_at(material["trackRef"].as_str(), true).ok().map(|at| self.tracks[at].id);
            if let Some(at) = self.regions.iter().position(|r| r.name == name && r.start == start && r.end == end && Some(r.track) == track)
            {
                return Ok(json!({
                    "epoch": self.epoch_text(),
                    "restored": {"ref": self.region_ref(at + 1)},
                    "display": {"title": format!("Region {} is back already", quoted(&name))},
                    "undo": {"desc": desc, "entry": false},
                }));
            }
            let step = material["step"].as_str().unwrap_or_default().to_string();
            let pressed = self.undo_in_logic("Delete", |top| top == step)?;
            if self.restore_unanswered() {
                return Err(uncertain(format!("Logic Pro's {pressed:?} was pressed, but region {} isn't back", quoted(&name))));
            }
            let Some(mut region) = self.deleted_regions.remove(&step) else {
                return Err(gone(format!(
                    "region {} isn't in the project any more, and Logic Pro's undo didn't bring it back",
                    quoted(&name)
                )));
            };
            if let Some(track) = track {
                region.track = track;
            }
            let id = region.id;
            self.regions.push(region);
            self.sort_regions();
            self.bump_epoch();
            let place = self.regions.iter().position(|r| r.id == id).map_or(0, |at| at + 1);
            return Ok(json!({
                "epoch": self.epoch_text(),
                "restored": {"ref": self.region_ref(place), "was": params["target"]["ref"], "pressed": pressed},
                "display": {"title": format!("Region {} put back", quoted(&name)), "note": "Put back with Logic Pro's own undo."},
                "undo": {"desc": desc, "entry": false, "how": "undo"},
            }));
        }
        let input = &params["input"];
        let at = self.region_for(method, input)?;
        let region = self.regions[at].clone();
        let fence = json!({"name": region.name, "startBeats": region.start, "endBeats": region.end, "notes": region.notes.len()});
        let title = format!("Deleted {}", region.label());
        match method {
            "change_preview" => {
                let mut display = json!({"title": title});
                if !region.notes.is_empty() {
                    display["note"] =
                        format!("It holds {} note(s); only Logic Pro's own undo brings a deleted region back.", region.notes.len()).into();
                }
                Ok(
                    json!({"epoch": self.epoch_text(), "target": {"ref": input["ref"], "kind": "region"}, "prior": fence, "fence": fence, "display": display}),
                )
            }
            _ => {
                if params["fence"] != fence {
                    return Err(changed(format!("{} changed since the preview", region.label())));
                }
                let desc = Self::desc(params)?;
                let material = json!({
                    "name": region.name, "kind": region.kind, "startBeats": region.start, "endBeats": region.end,
                    "trackRef": self.track_ref_of(region.track), "looped": region.looped, "notes": region.notes.len(), "step": desc,
                });
                let gone = self.regions.remove(at);
                self.deleted_regions.insert(desc.clone(), gone);
                self.bump_epoch();
                let undo = self.applied(Some("Delete"), &desc)?;
                Ok(
                    json!({"epoch": self.epoch_text(), "applied": {"deleted": true}, "material": material, "display": {"title": title}, "undo": undo}),
                )
            }
        }
    }

    // ----- (M4) the cycle, a track's colour, the Tempo and Signature Lists -----

    /// The cycle as the bridge reads it: the control bar's Cycle button and the LCD's locators.
    fn cycle_state(&self) -> Value {
        let (on, start, end) = self.cycle.unwrap_or((false, 0.0, 0.0));
        json!({"cycle": on, "startBeats": start, "endBeats": end})
    }

    /// `loop-range {startBeats, endBeats}`: the locators through the LCD and the Cycle button (on the beat); 0 and 0
    /// turns the cycle off and leaves the locators. No step of Logic's undo; its restore is by value.
    fn loop_range(&mut self, method: &str, params: &Value) -> Answer {
        let same = |a: &Value, b: &Value| a["cycle"] == b["cycle"] && same_bounds(a, b);
        let title_of = |state: &Value| {
            if state["cycle"] == true {
                format!("Cycle {} to {}", bar_text(number(state, "startBeats")), bar_text(number(state, "endBeats")))
            } else {
                "Cycle off".to_string()
            }
        };
        let now = self.cycle_state();
        if method == "change_restore" {
            let desc = Self::desc(params)?;
            let prior = &params["prior"];
            if !same(&now, prior) {
                if !same(&now, &params["applied"]) && params["discard"] != true {
                    return Err(changed("the cycle has been changed in Logic Pro since Kumi set it"));
                }
                self.cycle = Some((prior["cycle"] == true, number(prior, "startBeats"), number(prior, "endBeats")));
            }
            let undo = self.restored(None, &desc)?;
            let back = self.cycle_state();
            let title = format!("{} (put back)", title_of(&back));
            return Ok(
                json!({"epoch": self.epoch_text(), "restored": back, "display": {"title": title, "before": now, "after": back}, "undo": undo}),
            );
        }
        let input = &params["input"];
        let beat = |key: &str| {
            input
                .get(key)
                .and_then(Value::as_f64)
                .filter(|n| n.is_finite() && (0.0..=1.0e6).contains(n))
                .ok_or_else(|| bad(format!("{key} must be a number from 0: beats from the project's start")))
        };
        let (start, end) = (on_the_beat(beat("startBeats")?), on_the_beat(beat("endBeats")?));
        let off = start == 0.0 && end == 0.0;
        if end <= start && !off {
            return Err(bad("the cycle must end after it starts (0 and 0 turns it off)"));
        }
        let wanted = if off {
            json!({"cycle": false, "startBeats": now["startBeats"], "endBeats": now["endBeats"]})
        } else {
            json!({"cycle": true, "startBeats": start, "endBeats": end})
        };
        let title = title_of(&wanted);
        match method {
            "change_preview" => {
                let mut display = json!({"title": title, "before": now, "after": wanted});
                if same(&now, &wanted) {
                    display["unchanged"] = true.into();
                }
                Ok(
                    json!({"epoch": self.epoch_text(), "target": {"ref": null, "kind": "project"}, "prior": now, "fence": now, "display": display}),
                )
            }
            _ => {
                if !same(&now, &params["fence"]) {
                    return Err(changed("the cycle changed since the preview"));
                }
                let desc = Self::desc(params)?;
                self.cycle = Some((wanted["cycle"] == true, number(&wanted, "startBeats"), number(&wanted, "endBeats")));
                let undo = self.applied(None, &desc)?;
                Ok(
                    json!({"epoch": self.epoch_text(), "applied": wanted, "display": {"title": title, "before": now, "after": wanted}, "undo": undo}),
                )
            }
        }
    }

    /// `track-color {ref, color}`: View > Show Colors, a swatch for the selected track, read back from the palette (the
    /// header read shows none: a recorded track's colour is the palette's until Kumi sets one). A step of Logic's undo;
    /// its restore is by value.
    fn track_color(&mut self, method: &str, params: &Value) -> Answer {
        let state_of = |track: &Track| json!({"color": track.color.clone().unwrap_or_else(|| PALETTE_COLOR.into())});
        let title_of = |name: &str, color: &str| format!("{name} coloured {color}");
        if method == "change_restore" {
            let at = self.track_at(params["target"]["ref"].as_str(), true)?;
            let desc = Self::desc(params)?;
            let now = state_of(&self.tracks[at]);
            let prior = params["prior"]["color"].as_str().unwrap_or(PALETTE_COLOR).to_string();
            let name = self.tracks[at].name.clone();
            let moved = now["color"] != prior;
            if moved {
                if now != params["applied"] && params["discard"] != true {
                    return Err(changed(format!("{name}'s colour has been changed in Logic Pro since Kumi set it")));
                }
                self.tracks[at].color = Some(prior.clone());
            }
            let undo = self.restored(Some("Change Color").filter(|_| moved), &desc)?;
            let title = format!("{} (put back)", title_of(&name, &prior));
            return Ok(json!({"epoch": self.epoch_text(), "restored": {"color": prior}, "display": {"title": title}, "undo": undo}));
        }
        let input = &params["input"];
        let at = self.track_at(input["ref"].as_str(), false).map_err(|fault| {
            if method == "change_apply" && fault.0 == "failed" {
                changed(fault.1)
            } else {
                fault
            }
        })?;
        let wanted = color_input(input)?;
        let (now, name) = (state_of(&self.tracks[at]), self.tracks[at].name.clone());
        let after = json!({"color": wanted});
        let title = title_of(&name, &wanted);
        match method {
            "change_preview" => {
                let mut display = json!({"title": title, "before": now, "after": after});
                if now == after {
                    display["unchanged"] = true.into();
                }
                Ok(
                    json!({"epoch": self.epoch_text(), "target": {"ref": input["ref"], "kind": "track"}, "prior": now, "fence": now, "display": display}),
                )
            }
            _ => {
                if params["fence"] != now {
                    return Err(changed(format!("{name}'s colour changed since the preview")));
                }
                let desc = Self::desc(params)?;
                self.tracks[at].color = Some(wanted);
                let undo = self.applied(Some("Change Color"), &desc)?;
                Ok(
                    json!({"epoch": self.epoch_text(), "applied": after, "display": {"title": title, "before": now, "after": after}, "undo": undo}),
                )
            }
        }
    }

    /// The Tempo List's row at a beat, if one is (0 is the project's tempo, always there).
    fn tempo_at(&self, beats: f64) -> Value {
        if beats == 0.0 {
            return json!({"exists": true, "bpm": self.tempo});
        }
        match self.tempo_changes.iter().find(|(at, _)| *at == beats) {
            Some((_, bpm)) => json!({"exists": true, "bpm": bpm}),
            None => json!({"exists": false}),
        }
    }

    fn put_tempo(&mut self, beats: f64, bpm: f64) {
        if beats == 0.0 {
            self.tempo = bpm;
            return;
        }
        match self.tempo_changes.iter_mut().find(|(at, _)| *at == beats) {
            Some(change) => change.1 = bpm,
            None => {
                self.tempo_changes.push((beats, bpm));
                self.tempo_changes.sort_by(|a, b| a.0.total_cmp(&b.0));
            }
        }
    }

    /// Two Tempo List rows the same: both there with the same BPM, or neither there.
    fn same_tempo(a: &Value, b: &Value) -> bool {
        a["exists"] == b["exists"] && (a["exists"] != true || (number(a, "bpm") - number(b, "bpm")).abs() < 0.01)
    }

    /// `tempo-marker-set {positionBeats, bpm}`: the Tempo List's row at the beat (made at the playhead when none is),
    /// its BPM cell stepped to whole BPM; at 0 it is the project's tempo. Its restore is by value: the row put back,
    /// or taken away.
    fn tempo_marker_set(&mut self, method: &str, params: &Value) -> Answer {
        if method == "change_restore" {
            let beats = number(&params["target"], "positionBeats");
            if !beats.is_finite() {
                return Err(bad("target: the tempo change's positionBeats"));
            }
            let desc = Self::desc(params)?;
            let (now, prior) = (self.tempo_at(beats), &params["prior"]);
            let title = format!("Tempo at {} put back", bar_text(beats));
            if Self::same_tempo(&now, prior) {
                return Ok(
                    json!({"epoch": self.epoch_text(), "restored": now, "display": {"title": title}, "undo": {"desc": desc, "entry": false}}),
                );
            }
            if !Self::same_tempo(&now, &params["applied"]) && params["discard"] != true {
                return Err(changed(format!("the tempo at {} has been changed in Logic Pro since Kumi set it", bar_text(beats))));
            }
            let step = if prior["exists"] == true {
                self.put_tempo(beats, number(prior, "bpm"));
                "Change Tempo"
            } else {
                self.tempo_changes.retain(|(at, _)| *at != beats);
                "Delete"
            };
            let undo = self.restored(Some(step), &desc)?;
            return Ok(json!({"epoch": self.epoch_text(), "restored": self.tempo_at(beats), "display": {"title": title}, "undo": undo}));
        }
        let input = &params["input"];
        let beats = on_the_beat(Self::position_input(input)?);
        let asked = input["bpm"].as_f64().filter(|bpm| bpm.is_finite()).ok_or_else(|| bad("bpm must be a number"))?;
        if !(TEMPO_MIN..=TEMPO_MAX).contains(&asked) {
            return Err(bad(format!("bpm must be from {TEMPO_MIN} to {TEMPO_MAX}")));
        }
        let bpm = asked.round();
        let now = self.tempo_at(beats);
        let wanted = json!({"exists": true, "bpm": bpm});
        let title = format!("Tempo {bpm} BPM at {}", bar_text(beats));
        match method {
            "change_preview" => {
                let mut display = json!({"title": title, "before": now, "after": wanted});
                if asked != bpm {
                    display["note"] =
                        format!("Logic Pro's Tempo List takes whole BPM from outside: {} was rounded to {bpm}.", input["bpm"]).into();
                }
                if Self::same_tempo(&now, &wanted) {
                    display["unchanged"] = true.into();
                }
                Ok(
                    json!({"epoch": self.epoch_text(), "target": {"ref": null, "kind": "tempo", "positionBeats": beats}, "prior": now, "fence": now, "display": display}),
                )
            }
            _ => {
                if !Self::same_tempo(&now, &params["fence"]) {
                    return Err(changed(format!("the tempo at {} changed since the preview", bar_text(beats))));
                }
                let desc = Self::desc(params)?;
                let step = if now["exists"] == true { "Change Tempo" } else { "Create Tempo" };
                self.put_tempo(beats, bpm);
                let undo = self.applied(Some(step), &desc)?;
                Ok(
                    json!({"epoch": self.epoch_text(), "applied": wanted, "display": {"title": title, "before": now, "after": wanted}, "undo": undo}),
                )
            }
        }
    }

    /// `tempo-marker-delete {positionBeats}`: the Tempo List row's Delete (never the project's tempo at the start).
    /// Its restore makes the row again by value, unless another tempo is there now.
    fn tempo_marker_delete(&mut self, method: &str, params: &Value) -> Answer {
        if method == "change_restore" {
            let material = &params["material"];
            let (beats, bpm) = (number(material, "positionBeats"), number(material, "bpm"));
            if !beats.is_finite() || !bpm.is_finite() {
                return Err(bad("material: the tempo change's positionBeats and bpm"));
            }
            let desc = Self::desc(params)?;
            let title = format!("Tempo {bpm} BPM at {} put back", bar_text(beats));
            let now = self.tempo_at(beats);
            if now["exists"] == true {
                if (number(&now, "bpm") - bpm).abs() < 0.01 {
                    return Ok(
                        json!({"epoch": self.epoch_text(), "restored": now, "display": {"title": title}, "undo": {"desc": desc, "entry": false}}),
                    );
                }
                return Err(changed(format!(
                    "a different tempo ({} BPM) is at {} now (not the one Kumi deleted): nothing was written",
                    number(&now, "bpm"),
                    bar_text(beats)
                )));
            }
            self.put_tempo(beats, bpm);
            let undo = self.restored(Some("Create Tempo"), &desc)?;
            return Ok(
                json!({"epoch": self.epoch_text(), "restored": {"exists": true, "bpm": bpm}, "display": {"title": title}, "undo": undo}),
            );
        }
        let input = &params["input"];
        let beats = on_the_beat(Self::position_input(input)?);
        if beats == 0.0 {
            return Err(bad("the tempo at the project's start is the project's: change it (tempo_marker_set at 0), never delete it"));
        }
        let now = self.tempo_at(beats);
        if now["exists"] != true {
            return Err(match method {
                "change_preview" => failed(format!("there is no tempo change at {}", bar_text(beats))),
                _ => changed("the tempo change went since the preview"),
            });
        }
        let title = format!("Deleted the tempo change at {}", bar_text(beats));
        match method {
            "change_preview" => Ok(json!({
                "epoch": self.epoch_text(),
                "target": {"ref": null, "kind": "tempo", "positionBeats": beats},
                "prior": now,
                "fence": now,
                "display": {"title": title},
            })),
            _ => {
                if !Self::same_tempo(&now, &params["fence"]) {
                    return Err(changed(format!("the tempo at {} changed since the preview", bar_text(beats))));
                }
                let desc = Self::desc(params)?;
                self.tempo_changes.retain(|(at, _)| *at != beats);
                let undo = self.applied(Some("Delete"), &desc)?;
                Ok(json!({
                    "epoch": self.epoch_text(),
                    "applied": {"deleted": true},
                    "material": {"positionBeats": beats, "bpm": now["bpm"]},
                    "display": {"title": title},
                    "undo": undo,
                }))
            }
        }
    }

    /// The Signature List's row at a beat, if one is (0 is the project's, always there).
    fn signature_at(&self, beats: f64) -> Value {
        if beats == 0.0 {
            return json!({"exists": true, "timeSignature": [self.signature.0, self.signature.1]});
        }
        match self.signature_changes.iter().find(|(at, _)| *at == beats) {
            Some((_, (num, den))) => json!({"exists": true, "timeSignature": [num, den]}),
            None => json!({"exists": false}),
        }
    }

    fn put_signature(&mut self, beats: f64, signature: (i64, i64)) {
        if beats == 0.0 {
            self.signature = signature;
            return;
        }
        match self.signature_changes.iter_mut().find(|(at, _)| *at == beats) {
            Some(change) => change.1 = signature,
            None => {
                self.signature_changes.push((beats, signature));
                self.signature_changes.sort_by(|a, b| a.0.total_cmp(&b.0));
            }
        }
    }

    fn signature_input(input: &Value) -> Result<(i64, i64), Fault> {
        let pair = input["timeSignature"]
            .as_array()
            .filter(|pair| pair.len() == 2)
            .ok_or_else(|| bad("timeSignature must be [numerator, denominator]"))?;
        match (pair[0].as_i64(), pair[1].as_i64()) {
            (Some(num), Some(den)) if (1..=64).contains(&num) && [1, 2, 4, 8, 16, 32, 64].contains(&den) => Ok((num, den)),
            _ => Err(bad("timeSignature must be [numerator, denominator]: 1 to 64 over 1, 2, 4, 8, 16, 32 or 64")),
        }
    }

    /// `signature-set {positionBeats, timeSignature}`: the Signature List's row at the beat (made at the playhead when
    /// none is), its cells stepped; at 0 it is the project's. Its restore is by value. (The simulator counts bars by
    /// four beats whatever the signature: its positions don't follow a change at the start.)
    fn signature_set(&mut self, method: &str, params: &Value) -> Answer {
        let text = |state: &Value| format!("{}/{}", state["timeSignature"][0], state["timeSignature"][1]);
        if method == "change_restore" {
            let beats = number(&params["target"], "positionBeats");
            if !beats.is_finite() {
                return Err(bad("target: the signature's positionBeats"));
            }
            let desc = Self::desc(params)?;
            let (now, prior) = (self.signature_at(beats), &params["prior"]);
            let title = format!("Time signature at {} put back", bar_text(beats));
            if now == *prior {
                return Ok(
                    json!({"epoch": self.epoch_text(), "restored": now, "display": {"title": title}, "undo": {"desc": desc, "entry": false}}),
                );
            }
            if now != params["applied"] && params["discard"] != true {
                return Err(changed(format!("the time signature at {} has been changed in Logic Pro since Kumi set it", bar_text(beats))));
            }
            let step = if prior["exists"] == true {
                let signature = Self::signature_input(prior)?;
                self.put_signature(beats, signature);
                "Change Signature"
            } else {
                self.signature_changes.retain(|(at, _)| *at != beats);
                "Delete"
            };
            let undo = self.restored(Some(step), &desc)?;
            return Ok(
                json!({"epoch": self.epoch_text(), "restored": self.signature_at(beats), "display": {"title": title}, "undo": undo}),
            );
        }
        let input = &params["input"];
        let beats = on_the_beat(Self::position_input(input)?);
        let signature = Self::signature_input(input)?;
        let now = self.signature_at(beats);
        let wanted = json!({"exists": true, "timeSignature": [signature.0, signature.1]});
        let title = format!("Time signature {} at {}", text(&wanted), bar_text(beats));
        match method {
            "change_preview" => {
                let mut display = json!({"title": title, "before": now, "after": wanted});
                if now == wanted {
                    display["unchanged"] = true.into();
                }
                Ok(
                    json!({"epoch": self.epoch_text(), "target": {"ref": null, "kind": "signature", "positionBeats": beats}, "prior": now, "fence": now, "display": display}),
                )
            }
            _ => {
                if now != params["fence"] {
                    return Err(changed(format!("the time signature at {} changed since the preview", bar_text(beats))));
                }
                let desc = Self::desc(params)?;
                let step = if now["exists"] == true { "Change Signature" } else { "Create Signature" };
                self.put_signature(beats, signature);
                let undo = self.applied(Some(step), &desc)?;
                Ok(
                    json!({"epoch": self.epoch_text(), "applied": wanted, "display": {"title": title, "before": now, "after": wanted}, "undo": undo}),
                )
            }
        }
    }

    // ----- (M4) a MIDI file imported, Track Stacks, a track moved -----

    /// `midi-import {ref, positionBeats, notes}`: the track selected, the playhead put at the beat, File > Import >
    /// MIDI File… through the panel service with the file Kumi wrote: a region named after the file, holding the
    /// notes, on the track (a software instrument or external MIDI one); the epoch moves. Its restore is Logic's own
    /// undo ("Import") while on top, else the region selected and deleted, while it is as the import left it.
    fn midi_import(&mut self, method: &str, params: &Value) -> Answer {
        if method == "change_restore" {
            let desc = Self::desc(params)?;
            let created = params["created"]["ref"].as_str();
            let Ok(at) = self.region_at(created, true) else {
                return Ok(
                    json!({"epoch": self.epoch_text(), "restored": {"deleted": created}, "display": {"title": "The imported region is gone already"}, "undo": {"desc": desc, "entry": false}}),
                );
            };
            let label = self.regions[at].label();
            if let Ok(pressed) = self.undo_in_logic("Import", |top| top.starts_with("Kumi: Imported")) {
                if self.restore_unanswered() {
                    return Err(uncertain(format!("Logic Pro's {pressed:?} was pressed, but {label} is still there")));
                }
                self.regions.remove(at);
                self.bump_epoch();
                return Ok(json!({
                    "epoch": self.epoch_text(),
                    "restored": {"deleted": created, "pressed": pressed},
                    "display": {"title": format!("{label} taken away again"), "note": "Put back with Logic Pro's own undo."},
                    "undo": {"desc": desc, "entry": false, "how": "undo"},
                }));
            }
            if params["discard"] != true {
                let region = &self.regions[at];
                let as_imported = region.notes.len() as u64 == params["applied"]["notes"].as_u64().unwrap_or(0)
                    && same_bounds(&region.bounds(), &params["applied"]);
                if !as_imported {
                    return Err(changed(format!("{label} has been changed in Logic Pro since Kumi imported it (edited, moved or filled)")));
                }
            }
            self.regions.remove(at);
            self.bump_epoch();
            let undo = self.restored(Some("Delete"), &desc)?;
            return Ok(
                json!({"epoch": self.epoch_text(), "restored": {"deleted": created}, "display": {"title": format!("{label} taken away again")}, "undo": undo}),
            );
        }
        let input = &params["input"];
        let at = self.track_at(input["ref"].as_str(), false).map_err(|fault| {
            if method == "change_apply" && fault.0 == "failed" {
                changed(fault.1)
            } else {
                fault
            }
        })?;
        let track = self.tracks[at].clone();
        if track.kind != "instrument" && track.kind != "external" {
            let article = if track.kind.starts_with(['a', 'e', 'i', 'o', 'u']) { "an" } else { "a" };
            return Err(bad(format!(
                "{} is {article} {} track: Logic Pro puts a MIDI file on a software instrument or external MIDI track (track_add kind instrument first)",
                quoted(&track.name),
                track.kind
            )));
        }
        let beats = on_the_beat(Self::position_input(input)?);
        let notes = input["notes"]
            .as_array()
            .filter(|notes| !notes.is_empty() && notes.len() <= 10_000)
            .ok_or_else(|| bad("notes must list 1 to 10000 notes"))?;
        let rows = notes.iter().map(note_row).collect::<Result<Vec<_>, _>>()?;
        let last = rows
            .iter()
            .map(|note| note["beat"].as_f64().unwrap_or(0.0) + note["lengthTicks"].as_f64().unwrap_or(0.0) / TICKS_PER_BEAT)
            .fold(0.0, f64::max);
        let end = beats + (last / BEATS_PER_BAR).ceil().max(1.0) * BEATS_PER_BAR;
        let fence = json!({"regions": self.regions_of(&track)});
        let title = format!("Imported {} notes on {} at {}", rows.len(), quoted(&track.name), bar_text(beats));
        match method {
            "change_preview" => Ok(json!({
                "epoch": self.epoch_text(),
                "target": {"ref": input["ref"], "kind": "track"},
                "prior": fence,
                "fence": fence,
                "display": {"title": title},
            })),
            _ => {
                if params["fence"] != fence {
                    return Err(changed(format!("{}'s regions changed since the preview", track.name)));
                }
                let desc = Self::desc(params)?;
                let count = rows.len();
                let region = Region {
                    id: self.next_region,
                    track: track.id,
                    name: IMPORT_NAME.into(),
                    kind: "midi".into(),
                    start: beats,
                    end,
                    looped: false,
                    selected: false,
                    notes: rows,
                };
                let id = region.id;
                self.next_region += 1;
                self.regions.push(region);
                self.sort_regions();
                self.bump_epoch();
                let undo = self.applied(Some("Import"), &desc)?;
                let place = self.regions.iter().position(|region| region.id == id).map_or(0, |at| at + 1);
                Ok(json!({
                    "epoch": self.epoch_text(),
                    "applied": {"notes": count, "startBeats": beats, "endBeats": end, "trackRef": self.track_ref_of(track.id)},
                    "created": {"ref": self.region_ref(place), "kind": "region"},
                    "display": {"title": title},
                    "undo": undo,
                }))
            }
        }
    }

    /// The stack whose own track is at `at` flattened: its track gone, its tracks on their own; the epoch moves.
    fn flatten(&mut self, at: usize) -> Track {
        let main = self.tracks.remove(at);
        for track in &mut self.tracks {
            if track.in_stack == Some(main.id) {
                track.in_stack = None;
            }
        }
        self.bump_epoch();
        main
    }

    /// `track-stack {refs, kind, name?}`: the tracks selected, Track > Create Track Stack…, its dialog's kind, Create:
    /// the stack's own track above the first of them, read back from the headers; the epoch moves. Its restore is
    /// Logic's own undo while that step is on top, else Track > Flatten Track Stack.
    fn track_stack(&mut self, method: &str, params: &Value) -> Answer {
        if method == "change_restore" {
            let desc = Self::desc(params)?;
            let created = params["created"]["ref"].as_str();
            let at = self.track_at(created, true)?;
            if !self.tracks[at].is_stack() {
                return Err(gone("the track Kumi's Track Stack had isn't a stack's own track any more"));
            }
            let name = self.tracks[at].name.clone();
            let pressed = self.undo_in_logic("Create Track Stack", |top| top.starts_with("Kumi: Track Stack")).ok();
            if pressed.is_some() && self.restore_unanswered() {
                return Err(uncertain(format!(
                    "Logic Pro's \"Undo Create Track Stack\" was pressed, but Track Stack {} is still there",
                    quoted(&name)
                )));
            }
            self.flatten(at);
            let title = format!("Track Stack {} taken away again", quoted(&name));
            return match pressed {
                Some(pressed) => Ok(json!({
                    "epoch": self.epoch_text(),
                    "restored": {"flattened": created, "pressed": pressed},
                    "display": {"title": title, "note": "Put back with Logic Pro's own undo."},
                    "undo": {"desc": desc, "entry": false, "how": "undo"},
                })),
                None => {
                    let undo = self.restored(Some("Flatten Track Stack"), &desc)?;
                    Ok(json!({"epoch": self.epoch_text(), "restored": {"flattened": created}, "display": {"title": title}, "undo": undo}))
                }
            };
        }
        let input = &params["input"];
        let refs = input["refs"]
            .as_array()
            .filter(|refs| !refs.is_empty() && refs.len() <= 500)
            .ok_or_else(|| bad("refs must list the tracks, next to each other, in their order"))?;
        let places: Vec<usize> = refs
            .iter()
            .map(|reference| self.track_at(reference.as_str(), false))
            .collect::<Result<_, _>>()
            .map_err(|fault| if method == "change_apply" && fault.0 == "failed" { changed(fault.1) } else { fault })?;
        if places.windows(2).any(|pair| pair[1] != pair[0] + 1) {
            return Err(bad("refs must be tracks next to each other in the Tracks area, in their order"));
        }
        if places.iter().any(|&place| self.tracks[place].is_stack() || self.tracks[place].in_stack.is_some()) {
            return Err(bad("a track in a Track Stack (or a stack's own track) can't go in another stack: flatten it first"));
        }
        let kind = match input["kind"].as_str() {
            Some("folder") => "folder",
            Some("summing") => "summing",
            _ => return Err(bad("kind must be folder or summing")),
        };
        let name = match input.get("name") {
            None | Some(Value::Null) => format!("Stack {}", self.tracks.iter().filter(|track| track.is_stack()).count() + 1),
            Some(Value::String(name)) if !name.trim().is_empty() && name.chars().count() <= 256 && !name.chars().any(char::is_control) => {
                name.clone()
            }
            Some(_) => return Err(bad("name must be one line of at most 256 characters")),
        };
        let names: Vec<String> = places.iter().map(|&place| self.tracks[place].name.clone()).collect();
        let fence = json!({"tracks": self.tracks.len(), "names": names});
        let title = format!("Track Stack {} ({kind}) of {}", quoted(&name), names.iter().map(|n| quoted(n)).collect::<Vec<_>>().join(", "));
        match method {
            "change_preview" => Ok(json!({
                "epoch": self.epoch_text(),
                "target": {"ref": null, "kind": "track", "index": places[0] + 1},
                "prior": {"tracks": self.tracks.len()},
                "fence": fence,
                "display": {"title": title},
            })),
            _ => {
                if params["fence"] != fence {
                    return Err(changed("the tracks changed since the preview"));
                }
                let desc = Self::desc(params)?;
                let main = Track::made(self.next_track, &name, kind);
                self.next_track += 1;
                let id = main.id;
                for &place in &places {
                    self.tracks[place].in_stack = Some(id);
                }
                self.tracks.insert(places[0], main);
                self.bump_epoch();
                let place = places[0] + 1;
                let undo = self.applied(Some("Create Track Stack"), &desc)?;
                Ok(json!({
                    "epoch": self.epoch_text(),
                    "applied": {"index": place, "name": name, "kind": kind, "tracks": names},
                    "created": {"ref": format!("{}:track:{place}", self.epoch), "kind": "track"},
                    "display": {"title": title},
                    "undo": undo,
                }))
            }
        }
    }

    /// `track-stack-flatten {ref}`: Track > Flatten Track Stack on the stack's own track; the epoch moves, and what
    /// the stack was is kept in `material`. Its restore is Logic's own undo while that step is on top.
    fn track_stack_flatten(&mut self, method: &str, params: &Value) -> Answer {
        if method == "change_restore" {
            let material = &params["material"];
            let desc = Self::desc(params)?;
            let name = material["name"].as_str().unwrap_or_default().to_string();
            let place = material["index"].as_u64().unwrap_or(0) as usize;
            if self.tracks.get(place.wrapping_sub(1)).is_some_and(|track| track.name == name && track.is_stack()) {
                return Ok(json!({
                    "epoch": self.epoch_text(),
                    "restored": {"ref": format!("{}:track:{place}", self.epoch)},
                    "display": {"title": format!("Track Stack {} is back already", quoted(&name))},
                    "undo": {"desc": desc, "entry": false},
                }));
            }
            let step = material["step"].as_str().unwrap_or_default().to_string();
            let pressed = self.undo_in_logic("Flatten Track Stack", |top| top == step)?;
            if self.restore_unanswered() {
                return Err(uncertain(format!("Logic Pro's {pressed:?} was pressed, but Track Stack {} isn't back", quoted(&name))));
            }
            let count = material["tracks"].as_array().map_or(0, Vec::len);
            // (Logic's own undo brings the stack's own track back as it was: its place in every epoch holds)
            let main = self.deleted.remove(&step).unwrap_or_else(|| {
                let made = Track::made(self.next_track, &name, material["kind"].as_str().unwrap_or("folder"));
                self.next_track += 1;
                made
            });
            let id = main.id;
            let place = place.clamp(1, self.tracks.len() + 1);
            self.tracks.insert(place - 1, main);
            for track in self.tracks.iter_mut().skip(place).take(count) {
                track.in_stack = Some(id);
            }
            self.bump_epoch();
            return Ok(json!({
                "epoch": self.epoch_text(),
                "restored": {"ref": format!("{}:track:{place}", self.epoch), "was": params["target"]["ref"], "pressed": pressed},
                "display": {"title": format!("Track Stack {} put back", quoted(&name)), "note": "Put back with Logic Pro's own undo."},
                "undo": {"desc": desc, "entry": false, "how": "undo"},
            }));
        }
        let at = self.track_at(params["input"]["ref"].as_str(), false).map_err(|fault| {
            if method == "change_apply" && fault.0 == "failed" {
                changed(fault.1)
            } else {
                fault
            }
        })?;
        let track = self.tracks[at].clone();
        if !track.is_stack() {
            return Err(bad(format!("{} isn't a Track Stack's own track", quoted(&track.name))));
        }
        let members: Vec<String> = self.tracks.iter().filter(|t| t.in_stack == Some(track.id)).map(|t| t.name.clone()).collect();
        let fence = json!({"name": track.name, "index": at + 1, "kind": track.kind, "tracks": members});
        let title = format!("Flattened Track Stack {}", quoted(&track.name));
        match method {
            "change_preview" => Ok(json!({
                "epoch": self.epoch_text(),
                "target": {"ref": params["input"]["ref"], "kind": "track"},
                "prior": fence,
                "fence": fence,
                "display": {"title": title},
            })),
            _ => {
                if params["fence"] != fence {
                    return Err(changed(format!("Track Stack {} changed since the preview", quoted(&track.name))));
                }
                let desc = Self::desc(params)?;
                let material = json!({"name": track.name, "kind": track.kind, "index": at + 1, "tracks": members, "step": desc});
                let main = self.flatten(at);
                self.deleted.insert(desc.clone(), main);
                let undo = self.applied(Some("Flatten Track Stack"), &desc)?;
                Ok(
                    json!({"epoch": self.epoch_text(), "applied": {"flattened": true}, "material": material, "display": {"title": title}, "undo": undo}),
                )
            }
        }
    }

    /// `track-move {ref, index}`: the track at another place in the Tracks area (the bridge's channel, if it finds
    /// one); the epoch moves. Its restore moves it back, by value. A track in a Track Stack moves with its stack.
    fn track_move(&mut self, method: &str, params: &Value) -> Answer {
        if method == "change_restore" {
            let at = self.track_at(params["target"]["ref"].as_str(), true)?;
            let desc = Self::desc(params)?;
            let prior = params["prior"]["index"].as_u64().unwrap_or(0) as usize;
            let name = self.tracks[at].name.clone();
            if at + 1 == prior {
                return Ok(
                    json!({"epoch": self.epoch_text(), "restored": {"index": prior}, "display": {"title": format!("{} is where it was", quoted(&name))}, "undo": {"desc": desc, "entry": false}}),
                );
            }
            if at + 1 != params["applied"]["index"].as_u64().unwrap_or(0) as usize && params["discard"] != true {
                return Err(changed(format!("{} has been moved in Logic Pro since Kumi moved it", quoted(&name))));
            }
            let to = prior.clamp(1, self.tracks.len());
            let track = self.tracks.remove(at);
            self.tracks.insert(to - 1, track);
            self.bump_epoch();
            let undo = self.restored(Some("Move Tracks"), &desc)?;
            return Ok(
                json!({"epoch": self.epoch_text(), "restored": {"index": to}, "display": {"title": format!("{} moved back to place {to}", quoted(&name))}, "undo": undo}),
            );
        }
        let input = &params["input"];
        let at = self.track_at(input["ref"].as_str(), false).map_err(|fault| {
            if method == "change_apply" && fault.0 == "failed" {
                changed(fault.1)
            } else {
                fault
            }
        })?;
        let count = self.tracks.len();
        let index = input["index"]
            .as_u64()
            .map(|index| index as usize)
            .filter(|index| (1..=count).contains(index))
            .ok_or_else(|| bad(format!("index must be a whole number from 1 to {count}")))?;
        let track = self.tracks[at].clone();
        if track.is_stack() || track.in_stack.is_some() {
            return Err(bad(format!("{} is in a Track Stack: flatten the stack first", quoted(&track.name))));
        }
        let now = json!({"index": at + 1});
        let title = format!("Moved track {} to place {index}", quoted(&track.name));
        match method {
            "change_preview" => {
                let mut display = json!({"title": title, "before": now, "after": {"index": index}});
                if at + 1 == index {
                    display["unchanged"] = true.into();
                }
                Ok(
                    json!({"epoch": self.epoch_text(), "target": {"ref": input["ref"], "kind": "track"}, "prior": now, "fence": now, "display": display}),
                )
            }
            _ => {
                if params["fence"] != now {
                    return Err(changed(format!("{} moved since the preview", quoted(&track.name))));
                }
                let desc = Self::desc(params)?;
                let moved = self.tracks.remove(at);
                self.tracks.insert(index - 1, moved);
                self.bump_epoch();
                let undo = self.applied(Some("Move Tracks"), &desc)?;
                Ok(
                    json!({"epoch": self.epoch_text(), "applied": {"index": index}, "display": {"title": title, "before": now, "after": {"index": index}}, "undo": undo}),
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
                let number = |key: &str| -> Result<Option<f64>, Fault> {
                    match params.get(key) {
                        None | Some(Value::Null) => Ok(None),
                        Some(value) => value
                            .as_f64()
                            .filter(|n| n.is_finite() && (0.0..=1.0e6).contains(n))
                            .map(Some)
                            .ok_or_else(|| bad(format!("{key} must be a number from 0"))),
                    }
                };
                let beats = match (number("positionBeats")?, number("position")?) {
                    (Some(_), Some(_)) => return Err(bad("pass positionBeats or position, not both")),
                    (Some(beats), None) => beats,
                    (None, Some(seconds)) => seconds * self.tempo.max(1.0) / 60.0,
                    (None, None) => return Err(bad("pass positionBeats (beats from the project's start) or position (seconds)")),
                };
                // The LCD's bar and beat: no finer.
                self.playhead = on_the_beat(beats);
                let position = position_of(self.playhead);
                Ok(json!({
                    "epoch": self.epoch_text(),
                    "positionBeats": self.playhead,
                    "position": self.playhead * 60.0 / self.tempo,
                    "playhead": {"bar": position["bar"], "beat": position["beat"]},
                    "playing": self.playing,
                }))
            }
        }
    }

    /// `undo_step_begin {label}` / `undo_step_end {stepId, label?}`: a plan's step, counted. Logic has no undo block:
    /// the changes made meanwhile are each what they are to Logic's undo, and the end says how many there were.
    fn plan_step(&mut self, method: &str, params: &Value) -> Answer {
        let label = params.get("label").and_then(Value::as_str).map(str::to_owned);
        if method == "undo_step_begin" {
            let label = label.ok_or_else(|| bad("label must be a string"))?;
            if self.open_step.is_some() {
                return Err(failed("a step is open already: end it before beginning another"));
            }
            self.steps += 1;
            let id = format!("s{}", self.steps);
            self.open_step = Some((id.clone(), label, 0));
            return Ok(json!({"stepId": id}));
        }
        let id = params.get("stepId").and_then(Value::as_str).ok_or_else(|| bad("stepId must be a string"))?;
        let (_, begun, changes) = match self.open_step.as_ref() {
            Some((open, _, _)) if open == id => self.open_step.take().expect("the step"),
            _ => return Err(failed("that step isn't open (it ended already)")),
        };
        Ok(json!({"desc": label.unwrap_or(begun), "entry": false, "changes": changes}))
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
        let signature_text = format!("{}/{}", self.signature.0, self.signature.1);
        let patch_transport = |transport: &mut Value| {
            transport["playing"] = self.playing.into();
            transport["tempo"] = self.tempo.into();
            if let Some((on, _, _)) = self.cycle {
                transport["cycle"] = on.into();
            }
            if transport.get("timeSignature").is_some() {
                transport["timeSignature"] = signature_text.clone().into();
            }
            if transport.get("beatsPerBar").is_some() {
                transport["beatsPerBar"] = self.signature.0.into();
            }
        };
        let patch_project = |project: &mut Value| {
            project["tempo"] = self.tempo.into();
            project["trackCount"] = self.tracks.len().into();
            project["timeSignature"] = signature_text.clone().into();
        };
        let playhead = || {
            let position = position_of(self.playhead);
            json!({"bar": position["bar"], "beat": position["beat"]})
        };
        // The place a region ref of the recorded epoch names (from 0), if the region is there.
        let region_of = |reference: &str| -> Option<usize> {
            let rest = reference.strip_prefix(&format!("{RECORDED_EPOCH}:region:"))?;
            rest.parse::<usize>().ok().filter(|place| *place >= 1 && *place <= self.regions.len()).map(|place| place - 1)
        };
        let gone = |reference: &str| ("failed".to_string(), format!("{reference} isn't in the project (deleted, or another project)"));
        // A region's notes as a FOCUS body (the recorded shape).
        let notes_body = |at: usize| {
            let region = &self.regions[at];
            let rows = region.note_rows(at + 1);
            alphabetical(json!({
                "endBeats": region.end, "epoch": RECORDED_EPOCH.to_string(), "kind": "notes", "name": region.name,
                "notes": rows, "regionRef": format!("{RECORDED_EPOCH}:region:{}", at + 1), "startBeats": region.start,
                "timeSignature": signature_text, "total": region.notes.len(),
            }))
        };
        Some(match method {
            "read" if asked.get("ref").is_some() => {
                let reference = asked["ref"].as_str().unwrap_or_default();
                let (_, rest) = reference.split_once(':')?;
                if epoch_of(reference) != Some(RECORDED_EPOCH) {
                    return None;
                }
                let asked_for = params["ref"].as_str().unwrap_or_default();
                if rest.starts_with("region:") {
                    return Some(match region_of(reference) {
                        Some(at) => Ok(json!({"epoch": RECORDED_EPOCH.to_string(), "items": [self.region_rows()[at].clone()]})),
                        None => Err(gone(asked_for)),
                    });
                }
                if !rest.starts_with("track:") {
                    return None;
                }
                match rest["track:".len()..].parse::<usize>().ok().filter(|place| *place >= 1 && *place <= self.tracks.len()) {
                    Some(place) => Ok(json!({"epoch": RECORDED_EPOCH.to_string(), "items": [self.tracks[place - 1].row(place)]})),
                    None => Err(gone(asked_for)),
                }
            }
            "read" if kind == Some("track") && asked.get("parent").is_none() => page_of(track_rows(), asked),
            "read" if kind == Some("marker") && asked.get("parent").is_none() => page_of(marker_rows(), asked),
            // (M4) The regions, through the project or under a track; a region's notes as the Event List lists them.
            "read" if kind == Some("region") => match asked.get("parent").and_then(Value::as_str) {
                None => page_of(self.region_rows(), asked),
                // (the request is in the recorded epoch here; a parent from another epoch is left to the recorded
                // refusals and the stale check)
                Some(parent) => {
                    let place = parent.strip_prefix(&format!("{RECORDED_EPOCH}:track:")).and_then(|n| n.parse::<usize>().ok())?;
                    match place >= 1 && place <= self.tracks.len() {
                        true => page_of(self.region_rows().into_iter().filter(|row| row["trackRef"] == parent).collect(), asked),
                        false => Err(gone(params["parent"].as_str().unwrap_or_default())),
                    }
                }
            },
            "read" if kind == Some("note") && asked.get("parent").is_some() => match region_of(asked["parent"].as_str().unwrap_or_default()) {
                Some(at) => page_of(self.regions[at].note_rows(at + 1), asked),
                None => Err(gone(params["parent"].as_str().unwrap_or_default())),
            },
            "read" if kind == Some("project") => recorded("read", asked.clone()).map(|mut page| {
                patch_project(&mut page["items"][0]);
                page
            }),
            "read" if kind == Some("tempo") => recorded("read", asked.clone()).map(|mut page| {
                let row = &mut page["items"][0];
                row["bpm"] = self.tempo.into();
                // (M4) The Tempo List beyond the project's tempo, and the Signature List as changed.
                if !self.tempo_changes.is_empty() {
                    let mut tempos = vec![json!({"positionBeats": 0.0, "bpm": self.tempo})];
                    tempos.extend(self.tempo_changes.iter().map(|(at, bpm)| json!({"positionBeats": at, "bpm": bpm})));
                    row["tempos"] = Value::Array(tempos);
                }
                if self.signature != (4, 4) || !self.signature_changes.is_empty() {
                    let key = row["signatures"][0]["key"].clone();
                    let mut signatures = vec![json!({"key": key, "position": position_of(0.0), "time": signature_text})];
                    signatures.extend(self.signature_changes.iter().map(|(at, (num, den))| {
                        json!({"key": key, "position": position_of(*at), "time": format!("{num}/{den}")})
                    }));
                    row["signatures"] = Value::Array(signatures);
                }
                page
            }),
            "snapshot" => recorded("snapshot", asked.clone()).map(|mut snapshot| {
                patch_project(&mut snapshot["project"]);
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
                let lanes: Vec<Value> = self
                    .tracks
                    .iter()
                    .enumerate()
                    .map(|(at, track)| {
                        // Each track's regions, as the Tracks area shows them now (a track Kumi added holds none).
                        let regions: Vec<Value> = self
                            .regions
                            .iter()
                            .enumerate()
                            .filter(|(_, region)| region.track == track.id)
                            .map(|(place, region)| region.lane_row(place + 1))
                            .collect();
                        json!({"kind": track.kind, "name": track.name, "regions": regions, "trackRef": format!("{RECORDED_EPOCH}:track:{}", at + 1)})
                    })
                    .collect();
                whole["lanes"] = Value::Array(lanes);
                whole["trackCount"] = self.tracks.len().into();
                whole["playing"] = self.playing.into();
                whole["playheadBeats"] = self.playhead.into();
                whole["beatsPerBar"] = self.signature.0.into();
                whole["markers"] = self.markers.iter().map(|m| json!({"name": m.name, "positionBeats": m.position_beats})).collect();
                if let Some((on, start, end)) = self.cycle {
                    whole["cycle"] = json!({"on": on, "start": start, "end": end});
                }
                timeline_page(&whole, asked)
            }),
            "focus_body" if kind == Some("notes") => match asked.get("ref").and_then(Value::as_str) {
                Some(reference) => match region_of(reference) {
                    Some(at) => Ok(notes_body(at)),
                    None => Err(gone(params["ref"].as_str().unwrap_or_default())),
                },
                // (the selected region, else the first)
                None => match self.regions.iter().position(|region| region.selected).or((!self.regions.is_empty()).then_some(0)) {
                    Some(at) => Ok(notes_body(at)),
                    None => Err(("failed".to_string(), "no region is selected to show the notes of".to_string())),
                },
            },
            "focus_body" if kind == Some("tree") => {
                let mut body = recorded("focus_body", asked.clone()).ok()?;
                let reference = body["trackRef"].as_str().unwrap_or_default();
                let place =
                    reference.rsplit(':').next().and_then(|n| n.parse::<usize>().ok()).filter(|p| *p >= 1 && *p <= self.tracks.len())?;
                let track = &self.tracks[place - 1];
                let strip = &mut body["strip"];
                strip["name"] = track.name.clone().into();
                strip["volumeDb"] = track.volume_db.into();
                strip["pan"] = track.pan_steps.into();
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
        state.region_places.clear();
        state.keep_places();
    }

    /// The project open now, as the bridge's `status` would name it: the recorded one, the copy `open_project` opened,
    /// or (M4) the package Kumi saved it as.
    pub fn project(&self) -> Project {
        let state = self.state.lock().unwrap();
        let project = another_project(&serde_json::to_value(project()).unwrap(), state.project);
        let project = match &state.saved_as {
            Some(path) => saved_project(&project, path),
            None => project,
        };
        serde_json::from_value(project).unwrap()
    }

    /// (M4) The package the project was last saved to by Kumi (a save as), if it was.
    pub fn saved_as(&self) -> Option<String> {
        self.state.lock().unwrap().saved_as.clone()
    }

    /// (M4) The regions in the Tracks area's order: each name, its track's place (from 1), its start and its end.
    pub fn regions(&self) -> Vec<(String, usize, f64, f64)> {
        let state = self.state.lock().unwrap();
        state
            .regions
            .iter()
            .map(|region| (region.name.clone(), state.track_index(region.track).map_or(0, |at| at + 1), region.start, region.end))
            .collect()
    }

    /// (M4) How many notes the region at `place` (from 1) holds.
    pub fn notes_of(&self, place: usize) -> usize {
        self.state.lock().unwrap().regions.get(place - 1).map_or(0, |region| region.notes.len())
    }

    /// (M4) The cycle: whether it is on, and the locators in beats (0 and 0 until set).
    pub fn cycle(&self) -> (bool, f64, f64) {
        self.state.lock().unwrap().cycle.unwrap_or((false, 0.0, 0.0))
    }

    /// (M4) The Tempo List: each change's beat and BPM, the project's at 0 first.
    pub fn tempos(&self) -> Vec<(f64, f64)> {
        let state = self.state.lock().unwrap();
        std::iter::once((0.0, state.tempo)).chain(state.tempo_changes.iter().copied()).collect()
    }

    /// (M4) The Signature List: each change's beat and signature, the project's at 0 first.
    pub fn signatures(&self) -> Vec<(f64, (i64, i64))> {
        let state = self.state.lock().unwrap();
        std::iter::once((0.0, state.signature)).chain(state.signature_changes.iter().copied()).collect()
    }

    /// (M4) A track's colour as Kumi set it (`index` from 1); None until set.
    pub fn track_color(&self, index: usize) -> Option<String> {
        self.state.lock().unwrap().tracks[index - 1].color.clone()
    }

    /// (M4) The next restore alone is made, and Logic doesn't answer after it (REAPER's simulator's switch).
    pub fn uncertain_restore_once(&self) {
        self.state.lock().unwrap().uncertain_restore_once = true;
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

    /// The producer changes a track's `field` in Logic, as the tool names it: `pan` (-1 to 1), `mute`, `solo`, `arm`,
    /// `name`.
    pub fn set_track(&self, index: usize, field: &str, value: Value) {
        let mut state = self.state.lock().unwrap();
        let track = &mut state.tracks[index - 1];
        match field {
            "pan" => {
                track.pan_steps = ((value.as_f64().unwrap_or(0.0) * PAN_CENTRE as f64).round() as i64).clamp(-PAN_CENTRE, PAN_CENTRE - 1)
            }
            "mute" => track.mute = value == true,
            "solo" => track.solo = value == true,
            "arm" => track.armed = value == true,
            "name" => track.name = value.as_str().unwrap_or_default().to_string(),
            _ => track.volume_db = value.as_f64().unwrap_or(track.volume_db),
        }
    }

    pub fn tempo(&self) -> f64 {
        self.state.lock().unwrap().tempo
    }

    /// The markers, in the Marker List's order: each name and its position in beats.
    pub fn markers(&self) -> Vec<(String, f64)> {
        self.state.lock().unwrap().markers.iter().map(|m| (m.name.clone(), m.position_beats)).collect()
    }

    pub fn playing(&self) -> bool {
        self.state.lock().unwrap().playing
    }

    /// The playhead, in beats from the project's start.
    pub fn playhead(&self) -> f64 {
        self.state.lock().unwrap().playhead
    }

    /// Logic's undo history as the host named its steps (`desc`), oldest first; a step the bridge couldn't name (a
    /// restore's, or a write Logic didn't answer after) is empty.
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

    /// The next write (an apply or a restore) is made, and Logic doesn't answer after it: the bridge answers
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

    /// The simulator's config for Logic with this project behind it: the reads, and Kumi's changes, in the bridge's order.
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
            "change_preview" | "change_apply" | "change_restore" => {
                let answer = state.change(method, params);
                if method == "change_apply" && answer.is_ok() {
                    if let Some((_, _, changes)) = state.open_step.as_mut() {
                        *changes += 1;
                    }
                }
                return Some(answer);
            }
            "transport_stop" | "transport_play" | "cursor_set" => return Some(state.transport(method, params)),
            "undo_step_begin" | "undo_step_end" => return Some(state.plan_step(method, params)),
            // (M4) Saving: the package written, nothing in the project changed.
            "project_save" => return Some(state.project_save(params)),
            _ => {}
        }
        let (epoch, project, saved_as) = (state.epoch, state.project, state.saved_as.clone());
        // Past the recorded epoch, a ref or cursor from an earlier one is refused before anything is looked up, as the
        // bridge refuses it whatever it was for; in the recorded epoch the refusals recorded answer.
        if epoch != RECORDED_EPOCH {
            if let Some(stale) = stale_in(params, epoch) {
                return Some(Err(stale));
            }
        }
        let answered = |result: &Value| {
            let answer = another_project(&retag(result, RECORDED_EPOCH, epoch), project);
            match &saved_as {
                Some(path) => saved_project(&answer, path),
                None => answer,
            }
        };
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
        // (M4) The notes are the region's, paged as a read pages them, as recorded when the request was.
        let notes = answer("read", json!({"kind": "note", "parent": "1:region:1", "limit": 3})).unwrap();
        assert_eq!(
            (notes["items"].as_array().map(Vec::len), notes["nextCursor"].as_str(), notes["total"].as_u64()),
            (Some(3), Some("1.3"), Some(4))
        );
        assert!(answer("read", json!({"kind": "note", "parent": "1:region:9"})).unwrap_err().1.contains("isn't in the project"));
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

    /// A change's value shows in every read after it, over the recorded rows; a header's control is no step of Logic's
    /// undo; a track added goes below the selected one, moves the epoch and takes its place in the pages, the snapshot
    /// and the Tracks area; a restore's ref from before is carried.
    #[test]
    fn a_changed_value_shows_in_the_reads_and_a_track_added_takes_its_place_in_them() {
        let script = LogicScript::new();
        let answer = |method: &str, params: Value| script.answer(method, &params).expect("a method the simulator has");
        let preview =
            answer("change_preview", json!({"kind": "track-volume", "input": {"ref": "1:track:1", "volumeDb": -6}, "epoch": "1"})).unwrap();
        assert_eq!(
            (preview["prior"].clone(), preview["display"]["title"].as_str()),
            (json!({"volumeDb": 0.0}), Some("Vocal volume −6 dB"))
        );
        assert_eq!(script.volume(1), 0.0, "a preview changes nothing");
        let applied = answer(
            "change_apply",
            json!({"kind": "track-volume", "input": {"ref": "1:track:1", "volumeDb": -6}, "fence": {"volumeDb": 0.0}, "desc": "Kumi: Vocal volume −6 dB (k0001)", "epoch": "1"}),
        )
        .unwrap();
        assert_eq!(
            (applied["applied"].clone(), applied["undo"].clone()),
            (json!({"volumeDb": -6.0}), json!({"desc": "Kumi: Vocal volume −6 dB (k0001)", "entry": false})),
            "the value read back after the write; no step of Logic's undo"
        );
        assert_eq!(answer("read", json!({"ref": "1:track:1"})).unwrap()["items"][0]["volumeDb"], -6.0);
        assert_eq!(answer("read", json!({"kind": "track", "limit": 1})).unwrap()["items"][0]["volumeDb"], -6.0);
        assert_eq!(answer("snapshot", json!({})).unwrap()["tracks"][0]["volumeDb"], -6.0);
        assert_eq!(answer("focus_body", json!({"kind": "tree"})).unwrap()["strip"]["volumeDb"], -6.0);
        assert!(script.undo_titles().is_empty());

        // A track added below track 1: the epoch moves, the new track is second everywhere, and the fader's restore
        // still finds track 1.
        let added = answer(
            "change_apply",
            json!({"kind": "track-add", "input": {"name": "Pad", "index": 1}, "fence": {"tracks": 2, "names": ["Vocal", "Keys"]}, "desc": "Kumi: Added audio track “Pad” (k0002)", "epoch": "1"}),
        )
        .unwrap();
        assert_eq!(
            (added["epoch"].as_str(), added["created"]["ref"].as_str(), added["applied"]["kind"].as_str()),
            (Some("2"), Some("2:track:2"), Some("audio"))
        );
        assert_eq!(added["undo"], json!({"desc": "Kumi: Added audio track “Pad” (k0002)", "entry": true, "title": "Undo Create Track"}));
        let page = answer("read", json!({"kind": "track"})).unwrap();
        assert_eq!(
            (page["total"].as_u64(), page["items"][1]["name"].as_str(), page["items"][2]["ref"].as_str()),
            (Some(3), Some("Pad"), Some("2:track:3"))
        );
        assert_eq!(answer("read", json!({"kind": "project"})).unwrap()["items"][0]["trackCount"], 3);
        let timeline = answer("focus_body", json!({"kind": "timeline"})).unwrap();
        assert_eq!(
            (
                timeline["trackCount"].as_u64(),
                timeline["lanes"][1]["name"].as_str(),
                timeline["lanes"][2]["regions"].as_array().map(Vec::len)
            ),
            (Some(3), Some("Pad"), Some(1))
        );
        assert_eq!(answer("read", json!({"ref": "1:track:1"})).unwrap_err().0, "stale", "a read from before is stale");
        let restored = answer(
            "change_restore",
            json!({"kind": "track-volume", "target": {"ref": "1:track:1", "kind": "track"}, "prior": {"volumeDb": 0.0}, "applied": {"volumeDb": -6.0}, "desc": "Kumi: undo Vocal volume −6 dB (k0003)", "epoch": "1"}),
        )
        .unwrap();
        assert_eq!(
            (restored["epoch"].as_str(), restored["restored"].clone(), restored["undo"]["entry"].clone()),
            (Some("2"), json!({"volumeDb": 0.0}), json!(false))
        );
        assert_eq!(script.volume(1), 0.0);
        // The added track taken away again: the epoch moves once more, and the pages are as recorded.
        let gone = answer(
            "change_restore",
            json!({"kind": "track-add", "target": {"ref": null, "kind": "track", "index": 1}, "prior": {"tracks": 2}, "applied": added["applied"], "created": added["created"], "desc": "Kumi: undo Added audio track “Pad” (k0004)", "epoch": "2"}),
        )
        .unwrap();
        assert_eq!((gone["epoch"].as_str(), script.track_count()), (Some("3"), 2));
        assert_eq!(answer("read", json!({"kind": "track"})).unwrap()["items"][1]["name"], "Keys");
        assert_eq!(script.undo_titles(), ["Undo Create Track", "Undo Delete Tracks"]);
        assert_eq!(script.undo_history(), ["Kumi: Added audio track “Pad” (k0002)", ""], "a restore's step the bridge doesn't name");
    }

    #[test]
    fn positions_and_levels_read_as_logic_shows_them() {
        assert_eq!(position_of(32.0), json!({"bar": 9, "beat": 1, "division": 1, "tick": 1}));
        assert_eq!(position_of(5.5), json!({"bar": 2, "beat": 2, "division": 3, "tick": 1}));
        assert_eq!(beats_of(&json!({"bar": 9, "beat": 1, "division": 1, "tick": 1})), 32.0);
        assert_eq!(beats_of(&position_of(5.5)), 5.5);
        assert_eq!(
            (bar_text(16.0), bar_text(18.0), bar_text(18.5)),
            ("bar 5".to_string(), "bar 5 beat 3".to_string(), "bar 5 beat 3.3.1".to_string())
        );
        assert_eq!(
            (db_text(-6.0), db_text(3.5), db_text(0.0), db_text(-96.0)),
            ("−6".to_string(), "+3.5".to_string(), "0".to_string(), "−∞".to_string())
        );
        assert_eq!((pan_text(0), pan_text(-32), pan_text(63)), ("centre".to_string(), "50% left".to_string(), "98% right".to_string()));
        let pan = &TRACK_PROPS[1];
        assert_eq!(pan.wanted(&json!({"pan": -0.5})).unwrap(), json!({"pan": -0.5, "steps": -32}));
        assert_eq!(pan.wanted(&json!({"pan": 1.0})).unwrap()["steps"], 63);
        let arm = &TRACK_PROPS[4];
        assert_eq!(arm.wanted(&json!({"arm": true})).unwrap(), json!({"armed": true}));
        assert_eq!(arm.title("Vocal", &json!({}), &json!({"armed": false})), "Vocal disarmed");
    }
}
