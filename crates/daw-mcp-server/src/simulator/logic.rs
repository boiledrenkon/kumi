//! A scripted Logic Pro project for the host's tests: what Kumi's own bridge beside Logic (`logic-kumi`) answers to
//! `read`, `snapshot`, `focus` and `focus_body`, from a table of the contract's rows (docs/en/LOGIC_CONTRACT.md): one
//! project, two tracks, one MIDI region of four notes, a Channel EQ and a send on track 1, one marker. Pages by limit
//! and cursor; refs are positions with an epoch (`1:track:2`; children under their parent's position: `1:note:1.3`),
//! and the epoch moves when a track or region changes, after which refs and cursors from before are refused as stale.
//! Small on purpose: the host's tests are about the plumbing; what Logic holds is the bridge's own tests' concern.
use super::{Respond, SimulatorConfig};
use daw_bridge::auth::Secret;
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};

const DEFAULT_LIMIT: usize = 50;
const MAX_LIMIT: usize = 200;
/// The timeline body's page, in tracks.
const MAX_LANES: usize = 500;
const KINDS: [&str; 8] = ["project", "track", "region", "note", "plugin", "send", "marker", "tempo"];
const BODIES: [&str; 4] = ["tree", "notes", "timeline", "routing"];
const METHODS: [&str; 4] = ["read", "snapshot", "focus", "focus_body"];
/// The project's id: the SHA-256 prefix of its package path, as the contract has it.
pub const PROJECT_ID: &str = "3f9c2a7b1d4e5f60718293a4b5c6d7e8";

struct State {
    epoch: u32,
    changed: u64,
    /// What the simulator was asked, in order: method and params.
    asked: Vec<(String, Value)>,
}

/// A project that answers like the bridge. Clones share the project.
#[derive(Clone)]
pub struct LogicScript {
    state: Arc<Mutex<State>>,
}

/// The epoch a ref (`<epoch>:<kind>:<n>`) or a cursor (`<epoch>.<n>`) names, if the text is one.
fn epoch_of(text: &str) -> Option<u32> {
    let end = text.find([':', '.'])?;
    let digits = &text[..end];
    (!digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit())).then(|| digits.parse().ok()).flatten()
}

fn position(bar: u32, beat: u32, division: u32, tick: u32) -> Value {
    json!({"bar": bar, "beat": beat, "division": division, "tick": tick})
}

/// The project's rows of one kind, with this epoch's refs.
fn rows(epoch: u32, kind: &str) -> Vec<Value> {
    let r = |kind: &str, n: &str| format!("{epoch}:{kind}:{n}");
    match kind {
        "project" => vec![json!({
            "id": PROJECT_ID,
            "identity": "Scratch",
            "path": "/lab/Scratch.logicx",
            "tempo": 160.0,
            "timeSignature": "4/4",
            "keySignature": "C major",
            "sampleRate": 48000,
            "trackCount": 2,
        })],
        "track" => vec![
            json!({"ref": r("track", "1"), "index": 1, "name": "Vocal", "kind": "audio", "mute": false, "solo": false, "armed": false, "monitoring": false, "volumeDb": 0.0, "pan": 0, "color": "#5b8def", "output": "Stereo Out", "inputs": ["Input 1"], "selected": true}),
            json!({"ref": r("track", "2"), "index": 2, "name": "Keys", "kind": "instrument", "mute": false, "solo": false, "armed": false, "monitoring": false, "volumeDb": -3.0, "pan": 0, "color": null, "output": "Stereo Out", "inputs": [], "selected": false}),
        ],
        "region" => vec![json!({
            "ref": r("region", "1"),
            "trackRef": r("track", "2"),
            "name": "Keys",
            "startBeats": 0.0,
            "endBeats": 16.0,
            "kind": "midi",
            "selected": true,
            "looped": false,
        })],
        "note" => [(1, 60, 100, 240), (2, 64, 96, 240), (3, 67, 92, 240), (4, 72, 110, 480)]
            .iter()
            .map(|(beat, pitch, velocity, length)| {
                json!({
                    "ref": r("note", &format!("1.{beat}")),
                    "regionRef": r("region", "1"),
                    "position": position(1, *beat, 1, 1),
                    "pitch": pitch,
                    "velocity": velocity,
                    "lengthTicks": length,
                    "channel": 1,
                })
            })
            .collect(),
        "plugin" => vec![json!({
            "ref": r("plugin", "1.1"),
            "trackRef": r("track", "1"),
            "slot": 1,
            "name": "Channel EQ",
            "bypassed": false,
            "kind": "effect",
        })],
        "send" => vec![json!({"ref": r("send", "1.1"), "trackRef": r("track", "1"), "slot": 1, "destination": "Bus 1", "level": -6.0})],
        "marker" => vec![json!({"ref": r("marker", "1"), "position": position(9, 1, 1, 1), "name": "Chorus", "length": 8.0, "text": ""})],
        "tempo" => vec![json!({"bpm": 160.0, "signatures": [{"position": position(1, 1, 1, 1), "time": "4/4", "key": "C major"}]})],
        _ => vec![],
    }
}

/// The key a kind's rows name their parent by.
fn parent_key(kind: &str) -> Option<&'static str> {
    match kind {
        "region" | "plugin" | "send" => Some("trackRef"),
        "note" => Some("regionRef"),
        _ => None,
    }
}

fn selection(epoch: u32) -> Value {
    json!({"trackRef": format!("{epoch}:track:1"), "regionRef": format!("{epoch}:region:1")})
}

fn transport() -> Value {
    json!({"playing": false, "recording": false, "cycle": false, "playhead": {"bar": 1, "beat": 1}, "tempo": 160.0, "timeSignature": "4/4"})
}

type Answer = Result<Value, (String, String)>;

fn bad(message: impl Into<String>) -> (String, String) {
    ("bad-params".to_string(), message.into())
}

fn limit_of(params: &Value, most: usize) -> usize {
    params.get("limit").and_then(Value::as_u64).map_or(DEFAULT_LIMIT.min(most), |n| (n as usize).clamp(1, most))
}

/// Where a page starts: the position a cursor (`<epoch>.<n>`, this epoch's) names, or 0.
fn start_of(params: &Value) -> Result<usize, (String, String)> {
    match params.get("cursor").and_then(Value::as_str) {
        None => Ok(0),
        Some(cursor) => cursor
            .split_once('.')
            .and_then(|(_, n)| n.parse::<usize>().ok())
            .ok_or_else(|| bad("cursor: pass the nextCursor an earlier read gave, unchanged")),
    }
}

impl LogicScript {
    pub fn new() -> Self {
        LogicScript { state: Arc::new(Mutex::new(State { epoch: 1, changed: 7, asked: vec![] })) }
    }

    /// A track or region was added, removed or moved: the epoch moves, so refs and cursors from before are stale.
    pub fn change_tracks(&self) {
        let mut state = self.state.lock().unwrap();
        state.epoch += 1;
        state.changed += 1;
    }

    /// Every request the simulator answered, in order.
    pub fn asked(&self) -> Vec<(String, Value)> {
        self.state.lock().unwrap().asked.clone()
    }

    pub fn respond(&self) -> Respond {
        let script = self.clone();
        Arc::new(move |method, params| script.answer(method, params))
    }

    /// The simulator's config for Logic with this project behind it.
    pub fn config(&self, secret: Secret, resource: &str) -> SimulatorConfig {
        SimulatorConfig::logic(secret, resource).with_methods(&METHODS, self.respond())
    }

    fn answer(&self, method: &str, params: &Value) -> Option<Answer> {
        if !METHODS.contains(&method) {
            return None;
        }
        let mut state = self.state.lock().unwrap();
        state.asked.push((method.to_string(), params.clone()));
        let epoch = state.epoch;
        // A ref or cursor from before the epoch moved is refused, whatever it was for.
        for key in ["ref", "parent", "around", "cursor"] {
            let Some(from) = params.get(key).and_then(Value::as_str).and_then(epoch_of).filter(|from| *from != epoch) else { continue };
            let (what, then) = if key == "cursor" { ("cursor", "read again from the start") } else { ("ref", "read again") };
            return Some(Err(("stale".into(), format!("that {what} is from an earlier epoch ({from}; now {epoch}): {then}"))));
        }
        Some(match method {
            "read" => Self::read(epoch, params),
            "snapshot" => Ok(json!({
                "epoch": epoch.to_string(),
                "project": rows(epoch, "project").remove(0),
                "transport": transport(),
                "tracks": rows(epoch, "track"),
                "selection": selection(epoch),
                "markers": rows(epoch, "marker"),
            })),
            "focus" => Ok(json!({
                "epoch": epoch.to_string(),
                "view": "tracks",
                "editor": "pianoRoll",
                "selection": selection(epoch),
                "playhead": {"bar": 1, "beat": 1},
                "transport": {"playing": false, "recording": false, "cycle": false},
                "changed": state.changed,
                "ageMs": 12,
            })),
            "focus_body" => Self::body(epoch, params),
            _ => return None,
        })
    }

    fn read(epoch: u32, params: &Value) -> Answer {
        if let Some(wanted) = params.get("ref").and_then(Value::as_str) {
            let row = KINDS.iter().flat_map(|kind| rows(epoch, kind)).find(|row| row["ref"] == wanted);
            return match row {
                Some(row) => Ok(json!({"items": [row], "epoch": epoch.to_string()})),
                None => Err(("failed".into(), format!("{wanted} isn't in the project (deleted, or another project)"))),
            };
        }
        let kind = params.get("kind").and_then(Value::as_str).ok_or_else(|| bad("kind: one of the kinds"))?;
        if !KINDS.contains(&kind) {
            return Err(bad(format!("Logic Pro has no kind {kind:?} to read")));
        }
        let all: Vec<Value> = match (parent_key(kind), params.get("parent").and_then(Value::as_str)) {
            (Some("regionRef"), None) => return Err(bad(format!("{kind}: pass the parent region"))),
            (Some(key), Some(parent)) => rows(epoch, kind).into_iter().filter(|row| row[key] == parent).collect(),
            _ => rows(epoch, kind),
        };
        let (limit, start) = (limit_of(params, MAX_LIMIT), start_of(params)?);
        let items: Vec<Value> = all.iter().skip(start).take(limit).cloned().collect();
        let mut page = json!({"items": items, "total": all.len(), "epoch": epoch.to_string()});
        if start + limit < all.len() {
            page["nextCursor"] = format!("{epoch}.{}", start + limit).into();
        }
        Ok(page)
    }

    /// One row by its ref, or the selected one of its kind (track 1, region 1) when none is named.
    fn one(epoch: u32, kind: &str, wanted: Option<&str>) -> Answer {
        let wanted = wanted.map(str::to_string).unwrap_or_else(|| format!("{epoch}:{kind}:1"));
        rows(epoch, kind)
            .into_iter()
            .find(|row| row["ref"] == wanted)
            .ok_or_else(|| ("failed".to_string(), format!("{wanted} isn't in the project (deleted, or another project)")))
    }

    fn sends_of(epoch: u32, track_ref: &Value) -> Vec<Value> {
        rows(epoch, "send")
            .into_iter()
            .filter(|send| send["trackRef"] == *track_ref)
            .map(|send| json!({"slot": send["slot"], "destination": send["destination"], "level": send["level"], "sendRef": send["ref"]}))
            .collect()
    }

    fn body(epoch: u32, params: &Value) -> Answer {
        let e = epoch.to_string();
        let wanted = params.get("ref").and_then(Value::as_str);
        match params.get("kind").and_then(Value::as_str).unwrap_or("") {
            "tree" => {
                let track = Self::one(epoch, "track", wanted)?;
                let inserts: Vec<Value> = rows(epoch, "plugin")
                    .into_iter()
                    .filter(|plugin| plugin["trackRef"] == track["ref"])
                    .map(|plugin| json!({"slot": plugin["slot"], "name": plugin["name"], "bypassed": plugin["bypassed"], "pluginRef": plugin["ref"]}))
                    .collect();
                Ok(json!({
                    "kind": "tree",
                    "epoch": e,
                    "trackRef": track["ref"],
                    "strip": {
                        "name": track["name"], "kind": track["kind"], "volumeDb": track["volumeDb"], "pan": track["pan"],
                        "mute": track["mute"], "solo": track["solo"], "armed": track["armed"],
                        "inputs": track["inputs"], "output": track["output"],
                        "inserts": inserts, "sends": Self::sends_of(epoch, &track["ref"]),
                    },
                }))
            }
            "notes" => {
                let region = Self::one(epoch, "region", wanted)?;
                let notes: Vec<Value> = rows(epoch, "note").into_iter().filter(|note| note["regionRef"] == region["ref"]).collect();
                let (limit, start) = (limit_of(params, MAX_LIMIT), start_of(params)?);
                let page: Vec<Value> = notes.iter().skip(start).take(limit).cloned().collect();
                let mut body = json!({
                    "kind": "notes",
                    "epoch": e,
                    "regionRef": region["ref"],
                    "startBeats": region["startBeats"],
                    "endBeats": region["endBeats"],
                    "notes": page,
                    "total": notes.len(),
                });
                if start + limit < notes.len() {
                    body["nextCursor"] = format!("{epoch}.{}", start + limit).into();
                }
                Ok(body)
            }
            "timeline" => {
                let window = params.get("window").cloned().unwrap_or(json!({"from": 0.0, "to": 16.0}));
                let (from, to) = (window["from"].as_f64().unwrap_or(0.0), window["to"].as_f64().unwrap_or(16.0));
                let tracks = rows(epoch, "track");
                let limit = params.get("limit").and_then(Value::as_u64).map_or(MAX_LANES, |n| (n as usize).clamp(1, MAX_LANES));
                let start = match params.get("around").and_then(Value::as_str) {
                    Some(around) => {
                        let place = tracks.iter().position(|track| track["ref"] == around).ok_or_else(|| {
                            ("failed".to_string(), format!("{around} isn't in the project (deleted, or another project)"))
                        })?;
                        place.saturating_sub(limit / 2).min(tracks.len().saturating_sub(limit))
                    }
                    None => start_of(params)?,
                };
                let lanes: Vec<Value> = tracks
                    .iter()
                    .skip(start)
                    .take(limit)
                    .map(|track| {
                        let regions: Vec<Value> = rows(epoch, "region")
                            .into_iter()
                            .filter(|region| {
                                region["trackRef"] == track["ref"]
                                    && region["startBeats"].as_f64().unwrap_or(0.0) < to
                                    && region["endBeats"].as_f64().unwrap_or(0.0) > from
                            })
                            .map(|region| json!({"ref": region["ref"], "name": region["name"], "startBeats": region["startBeats"], "endBeats": region["endBeats"], "kind": region["kind"]}))
                            .collect();
                        json!({"trackRef": track["ref"], "name": track["name"], "kind": track["kind"], "regions": regions})
                    })
                    .collect();
                let mut body = json!({
                    "kind": "timeline",
                    "epoch": e,
                    "window": {"from": from, "to": to},
                    "lanes": lanes,
                    "trackCount": tracks.len(),
                    "from": start,
                });
                if start + limit < tracks.len() {
                    body["nextCursor"] = format!("{epoch}.{}", start + limit).into();
                }
                Ok(body)
            }
            "routing" => {
                let track = Self::one(epoch, "track", wanted)?;
                Ok(json!({
                    "kind": "routing",
                    "epoch": e,
                    "trackRef": track["ref"],
                    "inputs": track["inputs"],
                    "output": track["output"],
                    "sends": Self::sends_of(epoch, &track["ref"]),
                }))
            }
            _ => Err(bad(format!("Logic Pro has no body of that kind: use one of {}", BODIES.join(", ")))),
        }
    }
}

impl Default for LogicScript {
    fn default() -> Self {
        Self::new()
    }
}
