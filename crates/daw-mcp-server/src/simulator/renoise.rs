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
const METHODS: [&str; 4] = ["read", "snapshot", "focus", "focus_body"];

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
}

/// A song that answers like the tool. Clones share the song.
#[derive(Clone)]
pub struct RenoiseScript {
    state: Arc<Mutex<State>>,
}

/// The epoch a ref (`<epoch>:<kind>:…`) or a cursor (`<epoch>.<n>`) names, if the text is one.
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
            state: Arc::new(Mutex::new(State { epoch: VECTORS_EPOCH, cases: load_cases(), bulk: None, keeps_bound: true, asked: vec![] })),
        }
    }

    /// Another song is opened (or something refs index into is restructured): the epoch moves, so refs and cursors
    /// from before are refused as stale and the answers carry the new one.
    pub fn switch_song(&self) {
        self.state.lock().unwrap().epoch += 1;
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

    fn answer(&self, method: &str, params: &Value) -> Option<Result<Value, (String, String)>> {
        if !METHODS.contains(&method) {
            return None;
        }
        let mut state = self.state.lock().unwrap();
        state.asked.push((method.to_string(), params.clone()));
        let epoch = state.epoch;
        let stale = |from: u32| ("stale".to_string(), format!("that ref is from an earlier epoch ({from}; now {epoch}): read again"));
        // The vectors were made in epoch 1. In a later one the same questions have that epoch's refs, and anything from
        // before is refused.
        if epoch != VECTORS_EPOCH {
            if let Some(from) = from_another_epoch(params, epoch) {
                return Some(Err(stale(from)));
            }
        }
        let asked = retag(params, epoch, VECTORS_EPOCH, None);
        if let Some(lines) = state.bulk {
            let parent = format!("{VECTORS_EPOCH}:pattern:1");
            if method == "read" && asked["kind"] == "line" && asked["parent"] == parent && asked["track"] == 1 {
                return Some(full_page(lines, &asked, state.keeps_bound).map(|page| retag(&page, VECTORS_EPOCH, epoch, None)));
            }
        }
        let wanted = normal(&asked);
        if let Some(case) = state.cases.iter().find(|case| case.method == method && case.params == wanted) {
            return Some(case.response.clone().map(|result| retag(&result, VECTORS_EPOCH, epoch, None)));
        }
        if let Some(from) = from_another_epoch(params, epoch) {
            return Some(Err(stale(from)));
        }
        Some(Err(("bad-params".into(), format!("the simulator has no answer scripted for {method} {params}"))))
    }
}

impl Default for RenoiseScript {
    fn default() -> Self {
        Self::new()
    }
}

/// One page of the very full pattern's lines, as the tool pages them: by limit and cursor (`<epoch>.<line>`), cut where
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
        Some(cursor) => match cursor.split_once('.') {
            Some((epoch, position)) if epoch == EPOCH => position.parse::<usize>().map_err(|_| bad("not a cursor"))?,
            Some(_) => return Err(("stale".into(), "that cursor is from an earlier epoch: read again from the start".into())),
            None => return Err(bad("not a cursor")),
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
        page["nextCursor"] = format!("{EPOCH}.{number}").into();
    }
    Ok(page)
}
