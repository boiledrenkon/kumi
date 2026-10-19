//! The host's tools that read the DAW: `read`, `snapshot`, `focus` and `focus_body`. One implementation for every DAW:
//! each checks its arguments, asks the chosen bridge the method of the same name and hands the answer back, keeping
//! it inside the frame bound. What the rows and bodies hold is the DAW's own (see the M2 contract).
use crate::host::{CallError, Host, Pick, MAX_ANSWER_BYTES};
use daw_bridge::Daw;
use serde_json::{json, Map, Value};
use std::time::Duration;

/// A read, a snapshot or a body waits this long: the bridge answers a busy DAW at once, so this is for a slow project.
const READ_TIMEOUT: Duration = Duration::from_secs(15);
/// `focus` is polled; it never waits for the DAW.
const FOCUS_TIMEOUT: Duration = Duration::from_secs(3);
/// Rows in a page by default, and at most: the contract's.
pub const DEFAULT_LIMIT: u64 = 50;
pub const MAX_LIMIT: u64 = 200;
/// How many times a page the bridge made too big is asked for again with half the rows.
const SMALLER_PAGES: usize = 4;

/// What a DAW's `read` takes as a kind, and its `focus_body` as a body; and the change kinds the DAW's own undo never takes
/// back (they make no step in its history), so that its undo of a step never counts them, answered or not.
pub struct Kinds {
    pub read: &'static [&'static str],
    pub bodies: &'static [&'static str],
    pub outside_undo: &'static [&'static str],
}

pub fn kinds(daw: Daw) -> Kinds {
    match daw {
        Daw::Reaper => Kinds {
            read: &["project", "track", "item", "take", "fx", "param", "send", "marker", "region", "note", "envelope", "point", "autoitem"],
            bodies: &["arrange", "midi", "fx", "routing"],
            outside_undo: &["loop-repeat", "automation-override"],
        },
        Daw::Renoise => Kinds {
            read: &["song", "track", "sequence", "pattern", "line", "instrument", "sample", "phrase", "device", "param"],
            bodies: &["pattern", "matrix", "phrase", "instrument", "dsp"],
            outside_undo: &["track-mute", "track-solo", "loop-range", "loop-pattern"],
        },
    }
}

fn text_arg(arguments: &Value, name: &str) -> Result<Option<String>, String> {
    match arguments.get(name) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) if !text.is_empty() => Ok(Some(text.clone())),
        Some(_) => Err(format!("{name} must be a non-empty string")),
    }
}

fn limit_arg(arguments: &Value) -> Result<Option<u64>, String> {
    match arguments.get("limit") {
        None | Some(Value::Null) => Ok(None),
        Some(value) => match value.as_u64() {
            // Over the most a page holds is refused, as the bridges refuse it, not quietly cut.
            Some(n) if (1..=MAX_LIMIT).contains(&n) => Ok(Some(n)),
            _ => Err(format!("limit must be a whole number from 1 to {MAX_LIMIT}")),
        },
    }
}

fn window_arg(arguments: &Value, daw: Daw) -> Result<Option<Value>, String> {
    let Some(window) = arguments.get("window").filter(|w| !w.is_null()) else { return Ok(None) };
    let number = |name: &str| window.get(name).and_then(Value::as_f64).filter(|n| n.is_finite());
    match (number("from"), number("to")) {
        (Some(from), Some(to)) if to > from || (daw == Daw::Renoise && to == from) => Ok(Some(json!({"from": from, "to": to}))),
        // (a pattern's one line is a window in Renoise, whose lines are whole numbers; REAPER's are seconds)
        _ => Err(format!("window must be {{from, to}}: two numbers, to {} from", if daw == Daw::Renoise { "not before" } else { "after" })),
    }
}

/// The arguments a `read` forwards: only the ones it knows, each checked.
fn read_params(arguments: &Value) -> Result<Value, String> {
    let mut params = Map::new();
    for name in ["kind", "ref", "parent", "cursor", "epoch"] {
        if let Some(text) = text_arg(arguments, name)? {
            params.insert(name.into(), text.into());
        }
    }
    // The track whose lines of a pattern are read (Renoise's `line`): a number or a ref.
    match arguments.get("track") {
        None | Some(Value::Null) => {}
        Some(Value::Number(n)) if n.as_u64().is_some_and(|n| n >= 1) => {
            params.insert("track".into(), n.clone().into());
        }
        Some(Value::String(text)) if !text.is_empty() => {
            params.insert("track".into(), text.clone().into());
        }
        Some(_) => return Err("track must be a track number from 1, or a track ref".into()),
    }
    if !params.contains_key("kind") && !params.contains_key("ref") {
        return Err("kind: what to read (or ref: one object)".into());
    }
    layout_arg(arguments, &mut params)?;
    hidden_arg(arguments, &mut params)?;
    if let Some(limit) = limit_arg(arguments)? {
        params.insert("limit".into(), limit.into());
    }
    Ok(Value::Object(params))
}

/// The arguments a `focus_body` forwards.
fn body_params(arguments: &Value, daw: Daw) -> Result<Value, String> {
    let mut params = Map::new();
    let Some(kind) = text_arg(arguments, "kind")? else { return Err("kind: which body".into()) };
    params.insert("kind".into(), kind.into());
    for name in ["ref", "cursor", "around", "epoch"] {
        if let Some(text) = text_arg(arguments, name)? {
            params.insert(name.into(), text.into());
        }
    }
    if let Some(window) = window_arg(arguments, daw)? {
        params.insert("window".into(), window);
    }
    if let Some(limit) = limit_arg(arguments)? {
        params.insert("limit".into(), limit.into());
    }
    layout_arg(arguments, &mut params)?;
    hidden_arg(arguments, &mut params)?;
    Ok(Value::Object(params))
}

/// Whether to look through the columns a track doesn't show (Renoise), which costs about six times as much: off unless asked.
fn hidden_arg(arguments: &Value, params: &mut Map<String, Value>) -> Result<(), String> {
    match arguments.get("hidden") {
        None | Some(Value::Null) | Some(Value::Bool(false)) => Ok(()),
        Some(Value::Bool(true)) => {
            params.insert("hidden".into(), true.into());
            Ok(())
        }
        Some(_) => Err("hidden must be true or false".into()),
    }
}

/// The layout the request's refs were made in (Renoise's track shifts since): the bridge carries older refs through them.
fn layout_arg(arguments: &Value, params: &mut Map<String, Value>) -> Result<(), String> {
    match arguments.get("layout") {
        None | Some(Value::Null) => Ok(()),
        Some(Value::Number(n)) if n.as_u64().is_some() => {
            params.insert("layout".into(), n.clone().into());
            Ok(())
        }
        Some(_) => Err("layout must be a whole number from 0".into()),
    }
}

fn size_of(value: &Value) -> usize {
    serde_json::to_string(value).map(|text| text.len()).unwrap_or(usize::MAX)
}

impl Host {
    /// The answer of a call as a tool gives it: the JSON, and whether it is an error.
    async fn answer_of(&self, method: &str, params: Value, pick: &Pick, timeout: Duration) -> (Value, bool) {
        match self.call(method, params, pick, timeout).await {
            Ok(answer) => (answer, false),
            Err(error) => (self.call_error(&error), true),
        }
    }

    /// A call whose answer is a page: if the bridge sends one past the frame bound, the same page is asked for again
    /// with half the rows, down to one.
    async fn paged(&self, method: &str, mut params: Value, pick: &Pick, timeout: Duration) -> (Value, bool) {
        let mut before = usize::MAX;
        for attempt in 0..=SMALLER_PAGES {
            match self.call(method, params.clone(), pick, timeout).await {
                Ok(answer) if size_of(&answer) <= MAX_ANSWER_BYTES => return (answer, false),
                Ok(answer) => {
                    // (An answer that doesn't page, or a bridge that ignores the limit, is no smaller for asking again.)
                    let size = size_of(&answer);
                    let limit = params.get("limit").and_then(Value::as_u64).unwrap_or(DEFAULT_LIMIT);
                    if limit <= 1 || attempt == SMALLER_PAGES || size >= before {
                        return (self.call_error(&CallError::TooBig), true);
                    }
                    before = size;
                    params["limit"] = (limit / 2).into();
                }
                Err(error) => return (self.call_error(&error), true),
            }
        }
        (self.call_error(&CallError::TooBig), true)
    }

    fn pick_of(arguments: &Value) -> Result<Pick, (Value, bool)> {
        Pick::from_arguments(arguments).map_err(|why| (json!({"error": why}), true))
    }

    /// `read`: rows of one kind, a page, or one object by ref.
    pub async fn read(&self, arguments: &Value) -> (Value, bool) {
        let pick = match Self::pick_of(arguments) {
            Ok(pick) => pick,
            Err(answer) => return answer,
        };
        match read_params(arguments) {
            Ok(params) => self.paged("read", params, &pick, READ_TIMEOUT).await,
            Err(why) => (json!({"error": why}), true),
        }
    }

    /// `snapshot`: the project on one screen.
    pub async fn snapshot(&self, arguments: &Value) -> (Value, bool) {
        match Self::pick_of(arguments) {
            Ok(pick) => self.answer_of_bounded("snapshot", json!({}), &pick, READ_TIMEOUT).await,
            Err(answer) => answer,
        }
    }

    /// `focus`: what the producer is looking at, cheap enough to poll.
    pub async fn focus(&self, arguments: &Value) -> (Value, bool) {
        match Self::pick_of(arguments) {
            Ok(pick) => self.answer_of("focus", json!({}), &pick, FOCUS_TIMEOUT).await,
            Err(answer) => answer,
        }
    }

    /// `focus_body`: one FOCUS body.
    pub async fn focus_body(&self, arguments: &Value) -> (Value, bool) {
        let pick = match Self::pick_of(arguments) {
            Ok(pick) => pick,
            Err(answer) => return answer,
        };
        match body_params(arguments, self.daw()) {
            Ok(params) => self.paged("focus_body", params, &pick, READ_TIMEOUT).await,
            Err(why) => (json!({"error": why}), true),
        }
    }

    /// An answer that has no page to shrink: past the frame bound it is refused.
    async fn answer_of_bounded(&self, method: &str, params: Value, pick: &Pick, timeout: Duration) -> (Value, bool) {
        let (answer, is_error) = self.answer_of(method, params, pick, timeout).await;
        if !is_error && size_of(&answer) > MAX_ANSWER_BYTES {
            return (self.call_error(&CallError::TooBig), true);
        }
        (answer, is_error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hidden_is_forwarded_only_when_true() {
        assert_eq!(
            read_params(&json!({"kind": "line", "parent": "1:pattern:2", "track": 3, "hidden": true})).unwrap(),
            json!({"kind": "line", "parent": "1:pattern:2", "track": 3, "hidden": true})
        );
        assert_eq!(
            body_params(&json!({"kind": "pattern", "hidden": true}), Daw::Renoise).unwrap(),
            json!({"kind": "pattern", "hidden": true})
        );
        assert_eq!(read_params(&json!({"kind": "line", "hidden": false})).unwrap(), json!({"kind": "line"}));
        assert!(read_params(&json!({"kind": "line", "hidden": "yes"})).is_err());
        assert!(body_params(&json!({"kind": "pattern", "hidden": 1}), Daw::Renoise).is_err());
    }

    #[test]
    fn a_read_forwards_only_what_it_knows_checked() {
        let params =
            read_params(&json!({"kind": "item", "parent": "r:track:master", "cursor": "c", "limit": 200, "pid": 4, "extra": true}))
                .unwrap();
        assert_eq!(params, json!({"kind": "item", "parent": "r:track:master", "cursor": "c", "limit": 200}));
        assert_eq!(read_params(&json!({"ref": "r:track:master"})).unwrap(), json!({"ref": "r:track:master"}));
        for bad in [
            json!({}),
            json!({"kind": 3}),
            json!({"kind": ""}),
            json!({"kind": "track", "limit": 0}),
            json!({"kind": "track", "limit": 201}),
            json!({"kind": "track", "limit": "9"}),
            json!({"kind": "track", "cursor": []}),
            json!({"kind": "line", "track": 0}),
            json!({"kind": "line", "track": 1.5}),
            json!({"kind": "line", "track": ""}),
            json!({"kind": "line", "track": ["1"]}),
        ] {
            assert!(read_params(&bad).is_err(), "{bad}");
        }
        // Renoise's lines are read for a track, given by number or by ref.
        assert_eq!(
            read_params(&json!({"kind": "line", "parent": "1:pattern:2", "track": 3})).unwrap(),
            json!({"kind": "line", "parent": "1:pattern:2", "track": 3})
        );
        assert_eq!(
            read_params(&json!({"kind": "line", "parent": "1:pattern:2", "track": "1:track:3"})).unwrap(),
            json!({"kind": "line", "parent": "1:pattern:2", "track": "1:track:3"})
        );
    }

    #[test]
    fn a_body_forwards_its_window_when_it_is_a_range() {
        let params =
            body_params(&json!({"kind": "arrange", "window": {"from": 0, "to": 32}, "limit": 10, "resource": "/x"}), Daw::Reaper).unwrap();
        assert_eq!(params, json!({"kind": "arrange", "window": {"from": 0.0, "to": 32.0}, "limit": 10}));
        let params = body_params(&json!({"kind": "arrange", "limit": 24, "around": "r:track:{A}"}), Daw::Reaper).unwrap();
        assert_eq!(params, json!({"kind": "arrange", "around": "r:track:{A}", "limit": 24}));
        assert!(body_params(&json!({"kind": "arrange", "around": 5}), Daw::Reaper).is_err());
        // (one line of a pattern is a window; a bridge that wants more says so)
        assert_eq!(
            body_params(&json!({"kind": "pattern", "window": {"from": 5, "to": 5}}), Daw::Renoise).unwrap(),
            json!({"kind": "pattern", "window": {"from": 5.0, "to": 5.0}})
        );
        for bad in [
            json!({}),
            json!({"kind": "arrange", "window": {"from": 6, "to": 5}}),
            json!({"kind": "arrange", "window": {"from": 5, "to": 5}}),
            json!({"kind": "arrange", "window": "all"}),
            json!({"kind": "fx", "ref": 2}),
        ] {
            assert!(body_params(&bad, Daw::Reaper).is_err(), "{bad}");
        }
    }

    #[test]
    fn each_daw_has_its_own_kinds() {
        assert!(kinds(Daw::Reaper).read.contains(&"envelope") && !kinds(Daw::Reaper).read.contains(&"pattern"));
        assert!(kinds(Daw::Renoise).bodies.contains(&"matrix") && !kinds(Daw::Renoise).bodies.contains(&"arrange"));
    }
}
