//! A scripted REAPER project for the host's tests: the bridge's `read`, `snapshot`, `focus` and `focus_body` answered
//! from a table of rows, the way the extension answers them (the M2 contract): pages by limit and cursor, a page that
//! stops at the frame bound, an epoch that refuses a cursor from before it changed. Small on purpose: what the host
//! tests are about is the plumbing, not what REAPER holds.
use super::{Respond, SimulatorConfig};
use daw_bridge::auth::Secret;
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};

/// The most an answer carries (the contract's frame bound, less room for the envelope).
const PAGE_BYTES: usize = 224 * 1024;
const DEFAULT_LIMIT: usize = 50;
const MAX_LIMIT: usize = 200;
/// The arrange body's page, in tracks.
const MAX_ARRANGE: usize = 500;
const KINDS: [&str; 11] = ["project", "track", "item", "take", "fx", "param", "send", "marker", "region", "note", "envelope"];
const BODIES: [&str; 4] = ["arrange", "midi", "fx", "routing"];

struct State {
    epoch: u32,
    tracks: Vec<Value>,
    changed: u64,
    /// The sim keeps its pages inside the frame bound, as the extension does; a test can make it ignore the bound.
    keeps_bound: bool,
    /// What the sim was asked, in order: method and params.
    asked: Vec<(String, Value)>,
    /// The undo history's descriptions, oldest first (the changes applied and restored).
    undo: Vec<String>,
    /// A plan's undo step, open: its label (the changes made meanwhile make no step of their own).
    open_step: Option<String>,
    /// A dialog shows for the next change preview only (it closes right after).
    busy_once: bool,
    /// The next apply is made and then answered `uncertain` (a job the extension took and didn't finish in time).
    uncertain_once: bool,
    /// The next restore is made and then answered `uncertain`.
    uncertain_restore_once: bool,
    /// The next restore is made in part: its answer says what it left out.
    left_out_once: Option<String>,
}

/// A project that answers like the extension. Clones share the project.
#[derive(Clone)]
pub struct ReaperScript {
    state: Arc<Mutex<State>>,
}

fn track_row(index: usize, name: &str) -> Value {
    json!({
        "ref": format!("r:track:{{T{index:04}}}"),
        "index": index,
        "name": name,
        "depth": 0,
        "folder": 0,
        "color": null,
        "mute": false,
        "solo": false,
        "arm": false,
        "volumeDb": 0.0,
        "pan": 0.0,
        "fxCount": 0,
        "itemCount": 1,
        "sends": 0,
        "receives": 0,
        "selected": index == 1,
    })
}

/// Two of a field's values are the same: numbers by their value (-3 and -3.0), anything else exactly.
fn same(a: &Value, b: &Value) -> bool {
    match (a.as_f64(), b.as_f64()) {
        (Some(a), Some(b)) => a == b,
        _ => a == b,
    }
}

fn item_row(track: &str) -> Value {
    json!({"ref": format!("r:item:{{I-{}}}", &track[9..track.len() - 1]), "trackRef": track, "position": 0.0, "length": 2.0, "name": "loop.wav", "source": "wav"})
}

impl ReaperScript {
    /// A project of `tracks` tracks (the master first) named `Track 1`…, each `name_len` characters longer than its number.
    pub fn new(tracks: usize, name_len: usize) -> Self {
        let rows = (0..=tracks)
            .map(|index| {
                let name = if index == 0 { "MASTER".to_string() } else { format!("Track {index}{}", "x".repeat(name_len)) };
                track_row(index, &name)
            })
            .collect();
        ReaperScript {
            state: Arc::new(Mutex::new(State {
                epoch: 1,
                tracks: rows,
                changed: 7,
                keeps_bound: true,
                asked: vec![],
                undo: vec![],
                open_step: None,
                busy_once: false,
                uncertain_once: false,
                uncertain_restore_once: false,
                left_out_once: None,
            })),
        }
    }

    /// The project switched tabs: refs and cursors from before are stale.
    pub fn switch_tab(&self) {
        let mut state = self.state.lock().unwrap();
        state.epoch += 1;
        state.changed += 1;
    }

    /// The sim stops keeping its pages inside the frame bound, as a bridge that doesn't know it would.
    pub fn ignore_the_frame_bound(&self) {
        self.state.lock().unwrap().keeps_bound = false;
    }

    /// The producer moves a track's fader (`index` counts the master as 0).
    pub fn set_volume(&self, index: usize, db: f64) {
        let mut state = self.state.lock().unwrap();
        state.tracks[index]["volumeDb"] = db.into();
        state.changed += 1;
    }

    pub fn volume(&self, index: usize) -> f64 {
        self.state.lock().unwrap().tracks[index]["volumeDb"].as_f64().unwrap()
    }

    /// REAPER's undo history, as the changes named their steps.
    pub fn undo_history(&self) -> Vec<String> {
        self.state.lock().unwrap().undo.clone()
    }

    /// Every request the sim answered, in order.
    pub fn asked(&self) -> Vec<(String, Value)> {
        self.state.lock().unwrap().asked.clone()
    }

    pub fn respond(&self) -> Respond {
        let script = self.clone();
        Arc::new(move |method, params| script.answer(method, params))
    }

    /// The simulator's config for REAPER with this project behind it.
    pub fn config(&self, secret: Secret, resource: &str) -> SimulatorConfig {
        SimulatorConfig::reaper(secret, resource).with_methods(
            &[
                "read",
                "snapshot",
                "focus",
                "focus_body",
                "change_preview",
                "change_apply",
                "change_restore",
                "undo_step_begin",
                "undo_step_end",
            ],
            self.respond(),
        )
    }

    /// The kinds the sim makes, the way the contract has every kind made: a preview that writes nothing, an apply
    /// fenced by what the preview saw, a restore fenced by what the apply wrote. A track's volume, mute or name (one
    /// field each), and a track added at the end (its restore deletes it).
    fn change(state: &mut State, epoch: &str, method: &str, params: &Value) -> Result<Value, (String, String)> {
        let bad = |message: &str| ("bad-params".to_string(), message.to_string());
        if method == "change_preview" && std::mem::take(&mut state.busy_once) {
            return Err(("busy".into(), "a dialog is open; nothing changed".into()));
        }
        let kind = params["kind"].as_str().unwrap_or_default();
        if kind == "track-add" {
            return Self::add_track(state, epoch, method, params);
        }
        if kind == "run-action" {
            return Self::run_action(state, epoch, method, params);
        }
        let field = match kind {
            "track-volume" => "volumeDb",
            "track-mute" => "mute",
            "track-rename" => "name",
            _ => return Err(bad("the simulator makes only track-volume, track-mute, track-rename and track-add")),
        };
        let (reference, input) = match method {
            "change_restore" => (params["target"]["ref"].as_str(), &params["prior"]),
            _ => (params["input"]["ref"].as_str(), &params["input"]),
        };
        let index = state
            .tracks
            .iter()
            .position(|track| Some(track["ref"].as_str().unwrap_or_default()) == reference)
            .ok_or(("failed".to_string(), "that track isn't in the project".to_string()))?;
        let now = state.tracks[index][field].clone();
        let name = state.tracks[index]["name"].as_str().unwrap_or_default().to_string();
        let wanted = input.get(field).filter(|value| !value.is_null()).cloned().ok_or(bad(&format!("{field} is missing")))?;
        let title = match field {
            "volumeDb" => format!("{name} volume {} dB", wanted.as_f64().unwrap_or_default()),
            "mute" if wanted == true => format!("{name} muted"),
            "mute" => format!("{name} unmuted"),
            _ => format!("Renamed track “{name}” → “{}”", wanted.as_str().unwrap_or_default()),
        };
        match method {
            "change_preview" => Ok(json!({
                "epoch": epoch,
                "target": {"ref": reference, "kind": "track"},
                "prior": {field: now},
                "fence": {field: now},
                "display": {"title": title, "unchanged": same(&now, &wanted)},
            })),
            "change_apply" => {
                if !same(&params["fence"][field], &now) {
                    return Err(("changed".into(), format!("{name} changed since the preview")));
                }
                let desc = params["desc"].as_str().ok_or(bad("desc must be a string"))?.to_string();
                state.tracks[index][field] = wanted.clone();
                state.changed += 1;
                Self::step(state, &desc);
                if std::mem::take(&mut state.uncertain_once) {
                    return Err(("uncertain".into(), "REAPER didn't finish the change in time; it may have been made".into()));
                }
                Ok(
                    json!({"epoch": epoch, "applied": {field: wanted}, "display": {"title": title}, "undo": {"desc": desc, "entry": true, "stateCount": state.changed}}),
                )
            }
            _ => {
                if same(&now, &wanted) {
                    return Ok(json!({"epoch": epoch, "restored": {field: wanted}, "display": {"title": title}}));
                }
                if !same(&params["applied"][field], &now) && params["discard"] != true {
                    return Err(("changed".into(), format!("{name} changed since Kumi set it")));
                }
                let desc = params["desc"].as_str().ok_or(bad("desc must be a string"))?.to_string();
                state.tracks[index][field] = wanted.clone();
                state.changed += 1;
                state.undo.push(desc);
                if std::mem::take(&mut state.uncertain_restore_once) {
                    return Err(("uncertain".into(), "REAPER didn't finish putting it back in time; it may have been".into()));
                }
                let mut display = json!({"title": format!("{title} (put back)")});
                if let Some(note) = state.left_out_once.take() {
                    display["note"] = note.into();
                }
                Ok(json!({"epoch": epoch, "restored": {field: wanted}, "display": display}))
            }
        }
    }

    /// `track-add {name?}`: a track at the end, its ref in `created`; its restore deletes it, unless it's gone.
    fn add_track(state: &mut State, epoch: &str, method: &str, params: &Value) -> Result<Value, (String, String)> {
        let name = params["input"]["name"].as_str().unwrap_or("").to_string();
        match method {
            "change_preview" => Ok(json!({
                "epoch": epoch,
                "target": {"ref": null, "kind": "track"},
                "prior": {"tracks": state.tracks.len() - 1},
                "fence": {"tracks": state.tracks.len() - 1},
                "display": {"title": format!("Added track “{name}”")},
            })),
            "change_apply" => {
                let desc = params["desc"].as_str().unwrap_or_default().to_string();
                let index = state.tracks.len();
                let row = track_row(index + 900, &name);
                let made = row["ref"].clone();
                state.tracks.push(row);
                state.changed += 1;
                Self::step(state, &desc);
                Ok(
                    json!({"epoch": epoch, "applied": {"added": true}, "created": {"ref": made, "kind": "track"}, "display": {"title": format!("Added track “{name}”")}, "undo": {"desc": desc, "entry": true}}),
                )
            }
            _ => {
                let made = params["created"]["ref"].as_str().unwrap_or_default();
                let Some(index) = state.tracks.iter().position(|track| track["ref"] == made) else {
                    return Err(("gone".into(), "the track Kumi added is gone".into()));
                };
                state.tracks.remove(index);
                state.changed += 1;
                state.undo.push(params["desc"].as_str().unwrap_or_default().to_string());
                Ok(json!({"epoch": epoch, "restored": {"added": false}, "display": {"title": format!("“{name}” taken away again")}}))
            }
        }
    }

    /// (r-tracks) `run-action {action}`: 40001 inserts a track, a change REAPER makes a point of; 40297 unselects every track,
    /// which it makes none of (the answer says so, and Kumi makes no row). Only REAPER's undo takes either back.
    fn run_action(state: &mut State, epoch: &str, method: &str, params: &Value) -> Result<Value, (String, String)> {
        let action = params["input"]["action"].as_i64().unwrap_or_default();
        let name = match action {
            40001 => "Track: Insert new track",
            40297 => "Track: Unselect (clear selection of) all tracks",
            _ => return Err(("bad-params".into(), format!("REAPER has no action {action}"))),
        };
        let title = format!("Ran action “{name}”");
        let note = "Only REAPER's own undo can take this back (Edit > Undo).";
        match method {
            "change_preview" => Ok(
                json!({"epoch": epoch, "target": {"kind": "action", "command": action}, "prior": {}, "fence": {"command": action}, "display": {"title": title, "note": note}}),
            ),
            "change_apply" => {
                let desc = params["desc"].as_str().unwrap_or_default().to_string();
                if action == 40297 {
                    let note = "REAPER made no undo point of it: it changed nothing the project keeps (a view, selection or transport action), so it has no row in HISTORY.";
                    return Ok(
                        json!({"epoch": epoch, "applied": {"command": action, "changed": false}, "display": {"title": title, "note": note, "unchanged": true}, "undo": {"desc": desc, "entry": false}}),
                    );
                }
                let index = state.tracks.len();
                state.tracks.push(track_row(index + 950, ""));
                state.changed += 1;
                Self::step(state, &desc);
                Ok(
                    json!({"epoch": epoch, "applied": {"command": action}, "display": {"title": title, "note": note}, "undo": {"desc": desc, "entry": true}}),
                )
            }
            _ => Err(("daw-only".into(), "Kumi can't take back what an action did: REAPER's own undo can (Edit > Undo)".into())),
        }
    }

    /// A change's step in REAPER's undo: its own, or none inside a plan's (whose label names them all when it closes).
    fn step(state: &mut State, desc: &str) {
        if state.open_step.is_none() {
            state.undo.push(desc.to_string());
        }
    }

    /// `undo_step_begin {label}` / `undo_step_end {stepId, label?}`: one step around a plan's changes, named by the label
    /// it ends with (or began with).
    fn plan_step(state: &mut State, method: &str, params: &Value) -> Result<Value, (String, String)> {
        let label = params.get("label").and_then(Value::as_str).map(str::to_owned);
        if method == "undo_step_begin" {
            state.open_step = Some(label.unwrap_or_default());
            return Ok(json!({"stepId": "s1"}));
        }
        let begun = state.open_step.take().ok_or(("failed".to_string(), "no step is open".to_string()))?;
        let desc = label.unwrap_or(begun);
        state.undo.push(desc.clone());
        Ok(json!({"desc": desc, "entry": true}))
    }

    /// The next apply is made, then answered `uncertain`, as the extension answers a job it took and didn't finish in time.
    pub fn uncertain_once(&self) {
        self.state.lock().unwrap().uncertain_once = true;
    }

    /// The next restore is made, then answered `uncertain`.
    pub fn uncertain_restore_once(&self) {
        self.state.lock().unwrap().uncertain_restore_once = true;
    }

    /// The next restore is made in part, and its answer says what it left out (`display.note`).
    pub fn left_out_once(&self, note: &str) {
        self.state.lock().unwrap().left_out_once = Some(note.into());
    }

    /// A dialog shows for the next change's preview, and closes right after.
    pub fn busy_once(&self) {
        self.state.lock().unwrap().busy_once = true;
    }

    /// The producer presses REAPER's undo: Kumi's last step comes off (its values back as the preview saw them is the
    /// test's business); the description of what came off, for the bridge's `undone_in_daw`.
    pub fn undo_last(&self) -> Option<String> {
        self.state.lock().unwrap().undo.pop()
    }

    pub fn track(&self, index: usize) -> Value {
        self.state.lock().unwrap().tracks[index].clone()
    }

    pub fn set_track(&self, index: usize, field: &str, value: Value) {
        let mut state = self.state.lock().unwrap();
        state.tracks[index][field] = value;
        state.changed += 1;
    }

    fn epoch_text(state: &State) -> String {
        format!("sim-{}", state.epoch)
    }

    fn answer(&self, method: &str, params: &Value) -> Option<Result<Value, (String, String)>> {
        let mut state = self.state.lock().unwrap();
        state.asked.push((method.to_string(), params.clone()));
        let epoch = Self::epoch_text(&state);
        // Refs that say which epoch they came from are refused once the project in the tab is another.
        if matches!(method, "read" | "focus_body" | "change_preview" | "change_apply" | "change_restore") {
            if let Some(sent) = params.get("epoch").and_then(Value::as_str).filter(|sent| *sent != epoch) {
                let message =
                    format!("those refs are from an earlier epoch ({sent}; now {epoch}): the project in the tab changed, read again");
                return Some(Err(("stale".into(), message)));
            }
        }
        Some(match method {
            "change_preview" | "change_apply" | "change_restore" => Self::change(&mut state, &epoch, method, params),
            "undo_step_begin" | "undo_step_end" => Self::plan_step(&mut state, method, params),
            // (r-tracks) Saving: a file written, nothing in the project changed.
            // (as the extension: a save names the epoch its refs and project were read in)
            "project_save" | "track_template_save" if params.get("epoch").and_then(Value::as_str).is_none() => {
                Err(("bad-params".to_string(), "epoch: the epoch the refs were read in".to_string()))
            }
            "project_save" => Ok(json!({"epoch": epoch, "saved": "/lab/Projects/Sim.rpp", "dirty": false})),
            "track_template_save" => Ok(
                json!({"epoch": epoch, "saved": format!("/lab/REAPER/TrackTemplates/{}.RTrackTemplate", params["name"].as_str().unwrap_or("Template"))}),
            ),
            "read" => Self::read(&state, &epoch, params),
            "snapshot" => {
                let tracks: Vec<Value> = state.tracks.iter().take(MAX_LIMIT).cloned().collect();
                let mut snapshot = json!({
                    "epoch": epoch,
                    "project": Self::project(&state),
                    "transport": {"playing": false, "recording": false, "beat": 0.0, "tempo": 120.0, "beatsPerBar": 4.0, "seconds": 0.0, "loop": {"on": false, "start": 0.0, "end": 0.0}},
                    "tracks": tracks,
                    "selection": {"trackRef": "r:track:{T0001}", "itemRef": null, "takeRef": null, "fxRef": null, "cursor": 0.0},
                    "markers": [],
                });
                if state.tracks.len() > MAX_LIMIT {
                    snapshot["truncated"] =
                        json!({"tracks": {"total": state.tracks.len(), "nextCursor": format!("c1.{epoch}.{MAX_LIMIT}")}});
                }
                Ok(snapshot)
            }
            "focus" => Ok(json!({
                "epoch": epoch,
                "view": "arrange",
                "selection": {"trackRef": "r:track:{T0001}", "cursor": 0.0},
                "transport": {"playing": false, "recording": false, "beat": 0.0, "tempo": 120.0, "beatsPerBar": 4.0, "seconds": 0.0},
                "changed": state.changed,
                "ageMs": 12,
            })),
            "focus_body" => match params.get("kind").and_then(Value::as_str) {
                Some("arrange") => Self::arrange(&state, &epoch, params),
                Some(kind) if BODIES.contains(&kind) => {
                    Ok(json!({"kind": kind, "epoch": epoch, "ref": params.get("ref"), "window": params.get("window")}))
                }
                _ => Err(("bad-params".into(), format!("REAPER has no body of that kind: use one of {}", BODIES.join(", ")))),
            },
            _ => return None,
        })
    }

    /// The arrange body's tracks, paged like the extension's: from the top, from a cursor, or centred on the `around` track.
    fn arrange(state: &State, epoch: &str, params: &Value) -> Result<Value, (String, String)> {
        let bad = |message: &str| ("bad-params".to_string(), message.to_string());
        // The tracks without the master, which is the first row.
        let tracks = &state.tracks[1..];
        let limit = params.get("limit").and_then(Value::as_u64).map_or(MAX_ARRANGE, |n| (n as usize).clamp(1, MAX_ARRANGE));
        let start = match (params.get("cursor").and_then(Value::as_str), params.get("around")) {
            (Some(cursor), _) => match cursor.splitn(3, '.').collect::<Vec<_>>().as_slice() {
                ["c1", from, position] if *from == epoch => position.parse::<usize>().map_err(|_| bad("cursor: not one this read gave"))?,
                ["c1", _, _] => return Err(bad("that cursor is from before the project tab changed: read again from the start")),
                _ => return Err(bad("cursor: pass the nextCursor an earlier read gave, unchanged")),
            },
            (None, Some(around)) => {
                let around = around.as_str().filter(|a| !a.is_empty()).ok_or_else(|| bad("around: a track ref"))?;
                if !around.starts_with("r:track:") {
                    return Err(bad("around: a track ref"));
                }
                let place = if around == "r:track:master" {
                    0
                } else {
                    tracks
                        .iter()
                        .position(|row| row["ref"] == around)
                        .ok_or_else(|| ("failed".to_string(), format!("{around} isn't in the project (deleted, or another tab)")))?
                };
                place.saturating_sub(limit / 2).min(tracks.len().saturating_sub(limit))
            }
            (None, None) => 0,
        };
        let shown: Vec<Value> = tracks
            .iter()
            .skip(start)
            .take(limit)
            .cloned()
            .map(|mut row| {
                row["items"] = json!([]);
                row
            })
            .collect();
        let mut body = json!({
            "kind": "arrange",
            "epoch": epoch,
            "window": params.get("window").cloned().unwrap_or(json!({"from": 0.0, "to": 16.0})),
            "tracks": shown,
            "trackCount": tracks.len(),
            "from": start,
        });
        if start + limit < tracks.len() {
            body["nextCursor"] = format!("c1.{epoch}.{}", start + limit).into();
        }
        Ok(body)
    }

    /// The project row: another project in the tab (`switch_tab`) is another identity.
    fn project(state: &State) -> Value {
        json!({"id": null, "identity": format!("sim:{}", state.epoch), "name": "Song.rpp", "path": "/lab/Song.rpp", "tempo": 120.0})
    }

    fn read(state: &State, epoch: &str, params: &Value) -> Result<Value, (String, String)> {
        let bad = |message: &str| ("bad-params".to_string(), message.to_string());
        if let Some(reference) = params.get("ref").and_then(Value::as_str) {
            return match state.tracks.iter().find(|row| row["ref"] == reference) {
                Some(row) => Ok(json!({"items": [row], "epoch": epoch})),
                None => Err(("failed".into(), format!("{reference} isn't in the project (deleted, or another tab)"))),
            };
        }
        let kind = params.get("kind").and_then(Value::as_str).ok_or_else(|| bad("kind: one of the kinds"))?;
        if !KINDS.contains(&kind) {
            return Err(bad(&format!("REAPER has no kind {kind:?} to read")));
        }
        let limit = params.get("limit").and_then(Value::as_u64).map_or(DEFAULT_LIMIT, |n| (n as usize).clamp(1, MAX_LIMIT));
        let start = match params.get("cursor").and_then(Value::as_str) {
            None => 0,
            Some(cursor) => {
                let mut parts = cursor.splitn(3, '.');
                match (parts.next(), parts.next(), parts.next().and_then(|n| n.parse::<usize>().ok())) {
                    (Some("c1"), Some(from), Some(position)) if from == epoch => position,
                    (Some("c1"), Some(_), Some(_)) => {
                        return Err(bad("that cursor is from before the project tab changed: read again from the start"))
                    }
                    _ => return Err(bad("cursor: pass the nextCursor an earlier read gave, unchanged")),
                }
            }
        };
        let all: Vec<Value> = match (kind, params.get("parent").and_then(Value::as_str)) {
            ("project", _) => vec![Self::project(state)],
            ("track", _) => state.tracks.clone(),
            ("item", Some(parent)) => {
                state.tracks.iter().filter(|row| row["ref"] == parent).map(|row| item_row(row["ref"].as_str().unwrap())).collect()
            }
            ("item", None) | ("fx", None) | ("param", None) | ("send", None) | ("note", None) | ("envelope", None) | ("take", None) => {
                return Err(bad(&format!("{kind}: pass the parent")))
            }
            _ => vec![],
        };
        let mut rows = Vec::new();
        let mut bytes = 0;
        let mut next = None;
        for (position, row) in all.iter().enumerate().skip(start) {
            let size = row.to_string().len() + 1;
            let full = rows.len() >= limit || (state.keeps_bound && !rows.is_empty() && bytes + size > PAGE_BYTES);
            if full {
                next = Some(position);
                break;
            }
            bytes += size;
            rows.push(row.clone());
        }
        let mut page = json!({"items": rows, "total": all.len(), "epoch": epoch});
        if let Some(position) = next {
            page["nextCursor"] = format!("c1.{epoch}.{position}").into();
        }
        Ok(page)
    }
}
