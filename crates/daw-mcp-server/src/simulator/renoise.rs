//! A scripted Renoise song for the host's tests: the tool's `read`, `snapshot`, `focus` and `focus_body` answered the
//! way the tool answers them (the M2 contract).
//!
//! What the tool answers to a fixed set of requests is kept in tests/vectors/renoise-reads.json, which the tool's own
//! tests write from the real tool over the mock Renoise (KUMI_BLESS=1) and fail on when it changes; this replays those
//! answers verbatim, so the simulator cannot drift from the tool. A song of one very full pattern can be added, to
//! have pages as big as the tool makes them (pages by limit and cursor, cut at the frame bound).
use super::{Respond, SimulatorConfig};
use daw_bridge::auth::Secret;
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};

const VECTORS: &str = include_str!("../../tests/vectors/renoise-reads.json");
/// The epoch the vectors were made in.
const EPOCH: &str = "1";
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
        RenoiseScript { state: Arc::new(Mutex::new(State { cases: load_cases(), bulk: None, keeps_bound: true, asked: vec![] })) }
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
        if let Some(lines) = state.bulk {
            if method == "read" && params["kind"] == "line" && params["parent"] == format!("{EPOCH}:pattern:1") && params["track"] == 1 {
                return Some(full_page(lines, params, state.keeps_bound));
            }
        }
        let wanted = normal(params);
        let found = state.cases.iter().find(|case| case.method == method && case.params == wanted);
        Some(match found {
            Some(case) => case.response.clone(),
            None => Err(("bad-params".into(), format!("the simulator has no answer scripted for {method} {params}"))),
        })
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
