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
        ReaperScript { state: Arc::new(Mutex::new(State { epoch: 1, tracks: rows, changed: 7, keeps_bound: true, asked: vec![] })) }
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
        SimulatorConfig::reaper(secret, resource).with_methods(&["read", "snapshot", "focus", "focus_body"], self.respond())
    }

    fn epoch_text(state: &State) -> String {
        format!("sim-{}", state.epoch)
    }

    fn answer(&self, method: &str, params: &Value) -> Option<Result<Value, (String, String)>> {
        let mut state = self.state.lock().unwrap();
        state.asked.push((method.to_string(), params.clone()));
        let epoch = Self::epoch_text(&state);
        Some(match method {
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
