//! The host's journal of changes (M3 contract §3a): one record for each change previewed, applied and maybe undone,
//! so Kumi's undo sends the DAW what it needs (the values before, what a delete removed) and a call sent twice changes
//! the DAW once. The bridge applies and restores; the journal decides what may be asked of it.
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::sync::Mutex;

/// The most records kept, and the most JSON they hold: past either, previewed records go first, oldest first.
pub const MAX_RECORDS: usize = 512;
pub const MAX_BYTES: usize = 64 * 1024 * 1024;
/// The most transactions one `change_release` names.
pub const MAX_RELEASE: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Previewed,
    Applying,
    Applied,
    /// The apply was sent but its answer never came: the DAW may or may not have changed.
    Uncertain,
    Undoing,
    Undone,
}

impl State {
    pub fn word(self) -> &'static str {
        match self {
            State::Previewed => "previewed",
            State::Applying => "applying",
            State::Applied => "applied",
            State::Uncertain => "uncertain",
            State::Undoing => "undoing",
            State::Undone => "undone",
        }
    }
}

/// One change, as the journal keeps it.
#[derive(Debug, Clone)]
pub struct Record {
    pub id: String,
    pub kind: String,
    pub input: Value,
    pub epoch: Option<String>,
    pub layout: Option<u64>,
    pub target: Value,
    pub prior: Value,
    pub fence: Value,
    pub display: Value,
    pub confirmation: String,
    /// `k` and four base-36 characters, in the change's DAW step description, so the DAW's own undo of it is known.
    pub nonce: String,
    pub desc: Option<String>,
    /// The plan's DAW step the change was made in (its label, with the plan's nonce): the DAW's undo names that, not
    /// the change's own description.
    pub step_desc: Option<String>,
    pub applied: Option<Value>,
    pub created: Option<Value>,
    pub material: Option<Value>,
    pub state: State,
    pub created_ms: u64,
    /// The answers given, by idempotency key: the same key gets the same answer and changes nothing again.
    pub apply_answers: HashMap<String, (Value, bool)>,
    pub undo_answers: HashMap<String, (Value, bool)>,
}

impl Record {
    fn bytes(&self) -> usize {
        [&self.input, &self.target, &self.prior, &self.fence, &self.display]
            .into_iter()
            .chain(self.applied.iter())
            .chain(self.created.iter())
            .chain(self.material.iter())
            .map(|value| value.to_string().len())
            .sum::<usize>()
            + self.apply_answers.values().chain(self.undo_answers.values()).map(|(answer, _)| answer.to_string().len()).sum::<usize>()
    }
}

/// An answer a key already got, and whether it was an error.
pub type Answered = (Value, bool);
/// What a call may go on with: the record (and the restore's description), or the answer a key already got.
pub type Begun<T> = Result<Result<T, Answered>, Refusal>;

/// Why the journal refuses a call, with the code the tool answers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    pub code: &'static str,
    pub message: String,
}

fn refuse(code: &'static str, message: impl Into<String>) -> Refusal {
    Refusal { code, message: message.into() }
}

#[derive(Default)]
struct Inner {
    /// Oldest first.
    order: Vec<String>,
    records: HashMap<String, Record>,
    next: u64,
    /// Each record's JSON size, and their sum.
    sizes: HashMap<String, usize>,
    total: usize,
    /// The plan's DAW step open now (its label, with its nonce), if one is.
    open_step: Option<String>,
}

impl Inner {
    /// Counts a record's size again after it changed.
    fn resize(&mut self, id: &str) {
        let now = self.records.get(id).map_or(0, Record::bytes);
        let before = self.sizes.insert(id.to_string(), now).unwrap_or(0);
        self.total = self.total + now - before;
    }

    fn remove(&mut self, id: &str) -> bool {
        self.order.retain(|kept| kept != id);
        self.total -= self.sizes.remove(id).unwrap_or(0);
        self.records.remove(id).is_some()
    }
}

/// The journal. Shared by every call the host serves at once.
#[derive(Default)]
pub struct Journal {
    inner: Mutex<Inner>,
}

/// Four base-36 characters from a counter and the clock, unique among the records kept.
fn token(seed: u64) -> String {
    const DIGITS: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    let mut n = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ (seed >> 7);
    (0..4)
        .map(|_| {
            let digit = DIGITS[(n % 36) as usize] as char;
            n /= 36;
            digit
        })
        .collect()
}

fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

impl Journal {
    /// Keeps a previewed change (the bridge's `change_preview` answer): its id and confirmation.
    pub fn previewed(&self, kind: &str, input: &Value, answer: &Value, layout: Option<u64>) -> Result<Record, Refusal> {
        let mut inner = self.inner.lock().expect("journal lock");
        inner.next += 1;
        let seed = inner.next ^ now_ms();
        let mut nonce = format!("k{}", token(seed));
        while inner.records.values().any(|record| record.nonce == nonce) {
            inner.next += 1;
            nonce = format!("k{}", token(inner.next ^ now_ms()));
        }
        let record = Record {
            id: format!("t{}{}", inner.next, token(seed ^ 0x5bd1)),
            kind: kind.to_string(),
            input: input.clone(),
            epoch: answer.get("epoch").and_then(Value::as_str).map(str::to_owned),
            layout: answer.get("layout").and_then(Value::as_u64).or(layout),
            target: answer.get("target").cloned().unwrap_or(Value::Null),
            prior: answer.get("prior").cloned().unwrap_or(Value::Null),
            fence: answer.get("fence").cloned().unwrap_or(Value::Null),
            display: answer.get("display").cloned().unwrap_or(Value::Null),
            confirmation: format!("c{}", token(seed ^ 0xa11ce)),
            nonce,
            desc: None,
            step_desc: None,
            applied: None,
            created: None,
            material: None,
            state: State::Previewed,
            created_ms: now_ms(),
            apply_answers: HashMap::new(),
            undo_answers: HashMap::new(),
        };
        Self::make_room(&mut inner, record.bytes())?;
        inner.order.push(record.id.clone());
        inner.records.insert(record.id.clone(), record.clone());
        inner.resize(&record.id);
        Ok(record)
    }

    /// Room for one more record of `bytes`: previewed records go, oldest first; a change Kumi may still undo never does.
    fn make_room(inner: &mut Inner, bytes: usize) -> Result<(), Refusal> {
        while inner.records.len() >= MAX_RECORDS || inner.total + bytes > MAX_BYTES {
            let Some(oldest) = inner.order.iter().find(|id| inner.records[*id].state == State::Previewed).cloned() else {
                return Err(refuse(
                    "full",
                    format!("Kumi keeps at most {MAX_RECORDS} changes it can undo in this host: release some (change_release) first"),
                ));
            };
            inner.remove(&oldest);
        }
        Ok(())
    }

    pub fn get(&self, id: &str) -> Option<Record> {
        self.inner.lock().expect("journal lock").records.get(id).cloned()
    }

    /// Before an apply is sent: the answer a key already got, or the record (now applying) and its step description.
    pub fn begin_apply(&self, id: &str, confirmation: &str, key: &str, title: Option<&str>) -> Begun<Record> {
        let mut inner = self.inner.lock().expect("journal lock");
        let record = inner.records.get_mut(id).ok_or_else(|| refuse("unknown", format!("no change {id} here: preview it again")))?;
        if let Some(answer) = record.apply_answers.get(key) {
            return Ok(Err(answer.clone()));
        }
        if record.confirmation != confirmation {
            return Err(refuse("confirmation", "that confirmation isn't this change's: apply with the one its preview gave"));
        }
        match record.state {
            State::Previewed => {}
            State::Applying => return Err(refuse("in-flight", "this change is being applied now")),
            other => return Err(refuse("applied", format!("this change was {} already", other.word()))),
        }
        let title = title
            .map(str::to_owned)
            .or_else(|| record.display.get("title").and_then(Value::as_str).map(str::to_owned))
            .unwrap_or_else(|| record.kind.clone());
        record.desc = Some(format!("Kumi: {title} ({})", record.nonce));
        record.state = State::Applying;
        let record = record.clone();
        let step = inner.open_step.clone();
        let record = match inner.records.get_mut(id) {
            Some(kept) => {
                kept.step_desc = step;
                kept.clone()
            }
            None => record,
        };
        Ok(Ok(record))
    }

    /// The bridge's answer to the apply (or None when it never came): the record's state, and the answer kept for its key.
    pub fn end_apply(&self, id: &str, key: &str, answer: Result<&Value, (&Value, bool)>) -> (Value, bool) {
        let mut inner = self.inner.lock().expect("journal lock");
        let Some(record) = inner.records.get_mut(id) else { return (json!({"error": "the change was released meanwhile"}), true) };
        let (reply, is_error) = match answer {
            Ok(applied) => {
                record.state = State::Applied;
                // Where the DAW's refs are now (Renoise: a deleted track moved the layout): an undo is asked from there.
                if let Some(epoch) = applied.get("epoch").and_then(Value::as_str) {
                    record.epoch = Some(epoch.to_string());
                }
                if let Some(layout) = applied.get("layout").and_then(Value::as_u64) {
                    record.layout = Some(layout);
                }
                record.applied = applied.get("applied").cloned();
                record.created = applied.get("created").cloned().filter(|created| !created.is_null());
                record.material = applied.get("material").cloned().filter(|material| !material.is_null());
                if let Some(display) = applied.get("display").filter(|display| !display.is_null()) {
                    record.display = display.clone();
                }
                let mut reply = Map::new();
                reply.insert("transactionId".into(), record.id.clone().into());
                reply.insert("state".into(), "applied".into());
                // `kept` (Renoise): the epoch moved only for devices, patterns or the sequence, and the tracks' refs hold.
                for field in ["applied", "created", "display", "undo", "epoch", "layout", "kept"] {
                    if let Some(value) = applied.get(field).filter(|value| !value.is_null()) {
                        reply.insert(field.into(), value.clone());
                    }
                }
                (Value::Object(reply), false)
            }
            // Sent, and no answer: the DAW may have changed.
            Err((error, true)) => {
                record.state = State::Uncertain;
                let mut reply = error.clone();
                reply["state"] = "uncertain".into();
                reply["transactionId"] = record.id.clone().into();
                (reply, true)
            }
            // Refused before anything was written: the change can be applied again with another preview.
            Err((error, false)) => {
                record.state = State::Previewed;
                (error.clone(), true)
            }
        };
        record.apply_answers.insert(key.to_string(), (reply.clone(), is_error));
        inner.resize(id);
        (reply, is_error)
    }

    /// Before an undo is sent: the answer a key already got, or the record (now undoing) and the restore's description.
    pub fn begin_undo(&self, id: &str, key: &str) -> Begun<(Record, String)> {
        let mut inner = self.inner.lock().expect("journal lock");
        inner.next += 1;
        let seed = inner.next ^ now_ms();
        let record = inner.records.get_mut(id).ok_or_else(|| refuse("unknown", format!("no change {id} here")))?;
        if let Some(answer) = record.undo_answers.get(key) {
            return Ok(Err(answer.clone()));
        }
        match record.state {
            State::Applied => {}
            State::Undone => return Err(refuse("undone", "this change was undone already")),
            State::Undoing => return Err(refuse("in-flight", "this change is being undone now")),
            State::Uncertain => {
                return Err(refuse("uncertain", "whether this change reached the DAW isn't known, so Kumi won't undo it blindly"))
            }
            State::Previewed | State::Applying => return Err(refuse("not-applied", "this change wasn't applied")),
        }
        record.state = State::Undoing;
        let title = record.display.get("title").and_then(Value::as_str).map(str::to_owned).unwrap_or_else(|| record.kind.clone());
        Ok(Ok((record.clone(), format!("Kumi: undo {title} (k{})", token(seed)))))
    }

    /// The bridge's answer to the restore: undone, or still applied (a refusal leaves the change as it was).
    pub fn end_undo(&self, id: &str, key: &str, answer: Result<&Value, &Value>) -> (Value, bool) {
        let mut inner = self.inner.lock().expect("journal lock");
        let Some(record) = inner.records.get_mut(id) else { return (json!({"error": "the change was released meanwhile"}), true) };
        let (reply, is_error) = match answer {
            Ok(restored) => {
                record.state = State::Undone;
                // Where the DAW's refs are now (Renoise: a restore that made devices again moved the epoch).
                if let Some(epoch) = restored.get("epoch").and_then(Value::as_str) {
                    record.epoch = Some(epoch.to_string());
                }
                if let Some(layout) = restored.get("layout").and_then(Value::as_u64) {
                    record.layout = Some(layout);
                }
                let mut reply = json!({"transactionId": record.id, "state": "undone"});
                for field in ["display", "restored", "epoch", "layout", "kept"] {
                    if let Some(value) = restored.get(field).filter(|value| !value.is_null()) {
                        reply[field] = value.clone();
                    }
                }
                (reply, false)
            }
            Err(error) => {
                record.state = State::Applied;
                (error.clone(), true)
            }
        };
        // A refusal isn't kept: the producer may put things back, and the same undo can then go through.
        if !is_error {
            record.undo_answers.insert(key.to_string(), (reply.clone(), false));
        }
        inner.resize(id);
        (reply, is_error)
    }

    /// Forgets these changes (unknown ids are skipped): how many went.
    pub fn release(&self, ids: &[String]) -> usize {
        let mut inner = self.inner.lock().expect("journal lock");
        ids.iter().filter(|id| inner.remove(id)).count()
    }

    /// The changes whose DAW step description is one of `descs` (the DAW's own undo or redo of them): their own, or
    /// the plan's step they were made in.
    pub fn by_descs(&self, descs: &[String]) -> Vec<String> {
        let inner = self.inner.lock().expect("journal lock");
        let named = |desc: &Option<String>| desc.as_ref().is_some_and(|desc| descs.contains(desc));
        inner.order.iter().filter(|id| named(&inner.records[*id].desc) || named(&inner.records[*id].step_desc)).cloned().collect()
    }

    /// A plan's DAW step label, with a nonce of its own, so the DAW's undo of the plan names its changes.
    pub fn step_label(&self, label: &str) -> String {
        let mut inner = self.inner.lock().expect("journal lock");
        inner.next += 1;
        format!("Kumi: {label} (k{})", token(inner.next ^ now_ms() ^ 0x57e9))
    }

    /// The plan's step opened (Some, its label) or closed (None): the changes made meanwhile are in it.
    pub fn step(&self, open: Option<String>) {
        self.inner.lock().expect("journal lock").open_step = open;
    }

    /// The open step named, as it closes, by what it did: its description keeps its nonce, and the changes made in it
    /// are told by the new one. None when no step is open.
    pub fn name_step(&self, label: &str) -> Option<String> {
        let mut inner = self.inner.lock().expect("journal lock");
        let before = inner.open_step.clone()?;
        let nonce = before.rfind(" (k").map_or("", |at| &before[at..]).to_string();
        let named = format!("Kumi: {label}{nonce}");
        for record in inner.records.values_mut() {
            if record.step_desc.as_deref() == Some(before.as_str()) {
                record.step_desc = Some(named.clone());
            }
        }
        inner.open_step = Some(named.clone());
        Some(named)
    }

    pub fn len(&self) -> usize {
        self.inner.lock().expect("journal lock").records.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// The producer undid (or redid) Kumi's changes with the DAW's own undo: the bridge names their step descriptions, and
/// Kumi is told the changes' ids. Any other event goes on as it came.
pub fn undone_in_daw(journal: &Journal, name: &str, mut data: Value) -> Value {
    if name != "undone_in_daw" {
        return data;
    }
    let descs: Vec<String> =
        data.get("descs").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str).map(str::to_owned).collect();
    data["transactionIds"] = json!(journal.by_descs(&descs));
    data
}

/// How long a change may take the DAW (it bounds each of its own steps; a big project takes a while).
const CHANGE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

fn refusal(refusal: Refusal) -> (Value, bool) {
    (json!({"error": refusal.message, "code": refusal.code}), true)
}

fn text<'a>(arguments: &'a Value, name: &str) -> Result<&'a str, (Value, bool)> {
    arguments
        .get(name)
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty() && text.len() <= 256)
        .ok_or_else(|| (json!({"error": format!("{name} must be a string"), "code": "bad-params"}), true))
}

/// What the bridge is told of a change's place: the epoch it was read in, and the layout (Renoise) its refs are at.
fn place(params: &mut Value, epoch: Option<&str>, layout: Option<u64>) {
    if let Some(epoch) = epoch {
        params["epoch"] = epoch.into();
    }
    if let Some(layout) = layout {
        params["layout"] = layout.into();
    }
}

impl crate::host::Host {
    /// `change_preview`: what a change would do, and the id and confirmation to apply it by. Nothing changes.
    pub async fn change_preview(&self, arguments: &Value) -> (Value, bool) {
        let pick = match crate::host::Pick::from_arguments(arguments) {
            Ok(pick) => pick,
            Err(why) => return (json!({"error": why, "code": "bad-params"}), true),
        };
        let kind = match text(arguments, "kind") {
            Ok(kind) => kind.to_string(),
            Err(answer) => return answer,
        };
        let Some(input) = arguments.get("input").filter(|input| input.is_object()) else {
            return (json!({"error": "input must be an object", "code": "bad-params"}), true);
        };
        let mut params = json!({"kind": kind, "input": input});
        let layout = arguments.get("layout").and_then(Value::as_u64);
        place(&mut params, arguments.get("epoch").and_then(Value::as_str), layout);
        let answer = match self.call("change_preview", params, &pick, CHANGE_TIMEOUT).await {
            Ok(answer) => answer,
            Err(error) => return (self.call_error(&error), true),
        };
        match self.journal().previewed(&kind, input, &answer, layout) {
            Ok(record) => {
                let mut reply = json!({"transactionId": record.id, "confirmation": record.confirmation, "prior": record.prior, "display": record.display, "target": record.target});
                place(&mut reply, record.epoch.as_deref(), record.layout);
                (reply, false)
            }
            Err(why) => refusal(why),
        }
    }

    /// `change_apply`: the previewed change, made in one named DAW step. A key sent again gets the same answer.
    pub async fn change_apply(&self, arguments: &Value) -> (Value, bool) {
        let (id, confirmation, key) =
            match (text(arguments, "transactionId"), text(arguments, "confirmation"), text(arguments, "idempotencyKey")) {
                (Ok(id), Ok(confirmation), Ok(key)) => (id.to_string(), confirmation.to_string(), key.to_string()),
                (Err(answer), _, _) | (_, Err(answer), _) | (_, _, Err(answer)) => return answer,
            };
        let title = arguments.get("title").and_then(Value::as_str);
        let record = match self.journal().begin_apply(&id, &confirmation, &key, title) {
            Ok(Ok(record)) => record,
            Ok(Err(answered)) => return answered,
            Err(why) => return refusal(why),
        };
        let mut params = json!({"kind": record.kind, "input": record.input, "fence": record.fence, "desc": record.desc});
        place(&mut params, record.epoch.as_deref(), record.layout);
        match self.call("change_apply", params, &crate::host::Pick::default(), CHANGE_TIMEOUT).await {
            Ok(answer) => self.journal().end_apply(&id, &key, Ok(&answer)),
            // Answered, or never sent: nothing was written, and the change can be previewed again.
            Err(error @ (crate::host::CallError::Bridge { .. } | crate::host::CallError::NotConnected(_))) => {
                self.journal().end_apply(&id, &key, Err((&self.call_error(&error), false)))
            }
            // Sent, and no answer came: the DAW may have changed.
            Err(error) => self.journal().end_apply(&id, &key, Err((&self.call_error(&error), true))),
        }
    }

    /// `change_undo`: the change taken back as a new named step, its values put back exactly; refused if they changed since.
    pub async fn change_undo(&self, arguments: &Value) -> (Value, bool) {
        let (id, key) = match (text(arguments, "transactionId"), text(arguments, "idempotencyKey")) {
            (Ok(id), Ok(key)) => (id.to_string(), key.to_string()),
            (Err(answer), _) | (_, Err(answer)) => return answer,
        };
        if arguments.get("confirmation").and_then(Value::as_str) != Some("undo") {
            return (json!({"error": "confirmation must be \"undo\"", "code": "confirmation"}), true);
        }
        let (record, desc) = match self.journal().begin_undo(&id, &key) {
            Ok(Ok(found)) => found,
            Ok(Err(answered)) => return answered,
            Err(why) => return refusal(why),
        };
        let mut params =
            json!({"kind": record.kind, "target": record.target, "prior": record.prior, "applied": record.applied, "desc": desc});
        if let Some(created) = &record.created {
            params["created"] = created.clone();
        }
        if let Some(material) = &record.material {
            params["material"] = material.clone();
        }
        if arguments.get("discard") == Some(&Value::Bool(true)) {
            params["discard"] = true.into();
        }
        place(&mut params, record.epoch.as_deref(), record.layout);
        match self.call("change_restore", params, &crate::host::Pick::default(), CHANGE_TIMEOUT).await {
            Ok(answer) => self.journal().end_undo(&id, &key, Ok(&answer)),
            Err(error) => self.journal().end_undo(&id, &key, Err(&self.call_error(&error))),
        }
    }

    /// `transport_stop`: the DAW stops playing and recording (the emergency stop).
    pub async fn transport_stop(&self, arguments: &Value) -> (Value, bool) {
        let pick = match crate::host::Pick::from_arguments(arguments) {
            Ok(pick) => pick,
            Err(why) => return (json!({"error": why, "code": "bad-params"}), true),
        };
        match self.call("transport_stop", json!({}), &pick, CHANGE_TIMEOUT).await {
            Ok(answer) => (answer, false),
            Err(error) => (self.call_error(&error), true),
        }
    }

    /// `transport_play` / `cursor_set`: the DAW starts playing (Renoise: from a slot and line, if given), or its edit cursor
    /// moves (the play position follows while it plays). Neither is a step of the DAW's undo.
    pub async fn transport(&self, method: &str, arguments: &Value) -> (Value, bool) {
        let pick = match crate::host::Pick::from_arguments(arguments) {
            Ok(pick) => pick,
            Err(why) => return (json!({"error": why, "code": "bad-params"}), true),
        };
        let mut params = json!({});
        let fields: &[&str] = if method == "cursor_set" { &["positionBeats", "position", "epoch"] } else { &["sequence", "line", "epoch"] };
        for field in fields {
            if let Some(value) = arguments.get(*field) {
                params[*field] = value.clone();
            }
        }
        match self.call(method, params, &pick, CHANGE_TIMEOUT).await {
            Ok(answer) => (answer, false),
            Err(error) => (self.call_error(&error), true),
        }
    }

    /// `undo_step_begin` / `undo_step_end`: one DAW undo step around a plan's changes, where the DAW gives one. Its
    /// label carries a nonce, so the DAW's own undo of the plan names the plan's changes.
    pub async fn undo_step(&self, method: &str, arguments: &Value) -> (Value, bool) {
        let (params, label) = match method {
            "undo_step_begin" => match text(arguments, "label") {
                Ok(label) => {
                    let label = self.journal().step_label(label);
                    (json!({"label": label}), Some(label))
                }
                Err(answer) => return answer,
            },
            _ => match text(arguments, "stepId") {
                Ok(step) => {
                    let mut params = json!({"stepId": step});
                    // Named by what the plan did, as it closes (REAPER names a block when it ends it).
                    if let Some(named) = arguments.get("label").and_then(Value::as_str).and_then(|label| self.journal().name_step(label)) {
                        params["label"] = named.into();
                    }
                    (params, None)
                }
                Err(answer) => return answer,
            },
        };
        match self.call(method, params, &crate::host::Pick::default(), CHANGE_TIMEOUT).await {
            Ok(answer) => {
                // Closed (or refused to close) or opened: the changes made from now on are in it, or aren't.
                self.journal().step(label);
                (answer, false)
            }
            Err(error) => {
                if method == "undo_step_end" {
                    self.journal().step(None);
                }
                (self.call_error(&error), true)
            }
        }
    }

    /// `change_release`: changes Kumi won't undo any more, forgotten.
    pub fn change_release(&self, arguments: &Value) -> (Value, bool) {
        let Some(ids) = arguments.get("transactionIds").and_then(Value::as_array) else {
            return (json!({"error": "transactionIds must be a list", "code": "bad-params"}), true);
        };
        if ids.is_empty() || ids.len() > MAX_RELEASE || ids.iter().any(|id| !id.is_string()) {
            return (json!({"error": format!("transactionIds must be 1 to {MAX_RELEASE} ids"), "code": "bad-params"}), true);
        }
        let ids: Vec<String> = ids.iter().filter_map(Value::as_str).map(str::to_owned).collect();
        (json!({"released": self.journal().release(&ids)}), false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn preview(journal: &Journal) -> Record {
        let answer = json!({"epoch": "e1", "target": {"ref": "r:track:{A}", "kind": "track"}, "prior": {"volumeDb": 0.0}, "fence": {"volumeDb": 0.0}, "display": {"title": "Bass volume −6 dB"}});
        journal.previewed("track-volume", &json!({"ref": "r:track:{A}", "volumeDb": -6.0}), &answer, None).unwrap()
    }

    #[test]
    fn a_change_goes_previewed_applying_applied_undoing_undone_and_a_key_sent_twice_answers_once() {
        let journal = Journal::default();
        let record = preview(&journal);
        assert_eq!((record.state, record.epoch.as_deref()), (State::Previewed, Some("e1")));
        assert!(record.nonce.starts_with('k') && record.nonce.len() == 5, "{}", record.nonce);
        assert_eq!(journal.begin_apply(&record.id, "wrong", "a1", None).unwrap_err().code, "confirmation");
        let Ok(Ok(applying)) = journal.begin_apply(&record.id, &record.confirmation, "a1", None) else { panic!("applying") };
        assert_eq!(applying.desc.as_deref(), Some(format!("Kumi: Bass volume −6 dB ({})", record.nonce).as_str()));
        assert_eq!(journal.begin_apply(&record.id, &record.confirmation, "a2", None).unwrap_err().code, "in-flight");
        let (reply, is_error) =
            journal.end_apply(&record.id, "a1", Ok(&json!({"applied": {"volumeDb": -6.0}, "undo": {"desc": "x", "entry": true}})));
        assert!(!is_error && reply["state"] == "applied", "{reply}");
        // The same key: the same answer, nothing sent.
        assert_eq!(journal.begin_apply(&record.id, &record.confirmation, "a1", None).unwrap().unwrap_err(), (reply, false));
        assert_eq!(journal.begin_apply(&record.id, &record.confirmation, "a3", None).unwrap_err().code, "applied");
        let Ok(Ok((undoing, desc))) = journal.begin_undo(&record.id, "u1") else { panic!("undoing") };
        assert!(desc.starts_with("Kumi: undo Bass volume −6 dB (k"), "{desc}");
        assert_eq!(undoing.applied, Some(json!({"volumeDb": -6.0})));
        // Refused by the DAW (it changed since): still applied, and the same undo can go again.
        let (refused, _) = journal.end_undo(&record.id, "u1", Err(&json!({"code": "changed", "error": "changed since"})));
        assert_eq!(refused["code"], "changed");
        assert_eq!(journal.get(&record.id).unwrap().state, State::Applied);
        assert!(journal.begin_undo(&record.id, "u1").unwrap().is_ok());
        let kept = json!({"from": "e1", "tracks": true});
        let (undone, is_error) =
            journal.end_undo(&record.id, "u1", Ok(&json!({"restored": {"volumeDb": 0.0}, "epoch": "e2", "kept": kept})));
        assert!(!is_error && undone["state"] == "undone");
        // What the restore put back, and where the DAW's refs are now (and that its tracks' refs hold), for Kumi to follow.
        assert_eq!((&undone["restored"], &undone["epoch"], &undone["kept"]), (&json!({"volumeDb": 0.0}), &json!("e2"), &kept));
        assert_eq!(journal.get(&record.id).unwrap().epoch.as_deref(), Some("e2"));
        assert_eq!(journal.begin_undo(&record.id, "u1").unwrap().unwrap_err(), (undone, false));
        assert_eq!(journal.begin_undo(&record.id, "u2").unwrap_err().code, "undone");
    }

    #[test]
    fn an_undo_is_asked_from_the_epoch_and_layout_the_apply_answered() {
        let journal = Journal::default();
        let answer = json!({"epoch": "e1", "layout": 4, "target": {"ref": "e1:track:3", "kind": "track"}, "prior": {"deleted": false}, "fence": {}, "display": {"title": "Deleted track Bass"}});
        let record = journal.previewed("track-delete", &json!({"ref": "e1:track:3"}), &answer, Some(4)).unwrap();
        let Ok(Ok(_)) = journal.begin_apply(&record.id, &record.confirmation, "a1", None) else { panic!("applying") };
        // Deleting the track moved Renoise's layout from 4 to 5 (and a device list, in another change, would move the epoch).
        journal.end_apply(
            &record.id,
            "a1",
            Ok(&json!({"epoch": "e1", "layout": 5, "applied": {"deleted": true}, "undo": {"desc": "x", "entry": true}})),
        );
        let Ok(Ok((undoing, _))) = journal.begin_undo(&record.id, "u1") else { panic!("undoing") };
        assert_eq!((undoing.epoch.as_deref(), undoing.layout), (Some("e1"), Some(5)));
    }

    #[test]
    fn an_apply_without_an_answer_is_uncertain_and_kumi_won_t_undo_it_blindly() {
        let journal = Journal::default();
        let record = preview(&journal);
        journal.begin_apply(&record.id, &record.confirmation, "a1", None).unwrap().unwrap();
        let (reply, is_error) =
            journal.end_apply(&record.id, "a1", Err((&json!({"error": "REAPER didn't answer in time", "busy": true}), true)));
        assert!(is_error && reply["state"] == "uncertain");
        assert_eq!(journal.begin_undo(&record.id, "u1").unwrap_err().code, "uncertain");
        // Refused before writing: back to previewed, applicable again.
        let other = preview(&journal);
        journal.begin_apply(&other.id, &other.confirmation, "b1", None).unwrap().unwrap();
        journal.end_apply(&other.id, "b1", Err((&json!({"error": "changed", "code": "changed"}), false)));
        assert_eq!(journal.get(&other.id).unwrap().state, State::Previewed);
    }

    #[test]
    fn previewed_changes_make_room_and_changes_kumi_may_undo_never_do() {
        let journal = Journal::default();
        let first = preview(&journal);
        for _ in 1..MAX_RECORDS {
            preview(&journal);
        }
        assert_eq!(journal.len(), MAX_RECORDS);
        preview(&journal);
        assert!(journal.get(&first.id).is_none(), "the oldest previewed one went");
        // Every record applied: a new preview is refused, nothing dropped.
        let ids: Vec<String> = journal.inner.lock().unwrap().order.clone();
        for id in &ids {
            let record = journal.get(id).unwrap();
            journal.begin_apply(id, &record.confirmation, "k", None).unwrap().unwrap();
            journal.end_apply(id, "k", Ok(&json!({"applied": {}})));
        }
        let answer = json!({"prior": {}, "fence": {}});
        assert_eq!(journal.previewed("track-volume", &json!({}), &answer, None).unwrap_err().code, "full");
        assert_eq!(journal.release(&ids[..MAX_RELEASE]), MAX_RELEASE);
        assert!(journal.previewed("track-volume", &json!({}), &answer, None).is_ok());
    }

    #[test]
    fn the_daws_undo_event_names_kumis_changes_and_other_events_go_on_as_they_came() {
        let journal = Journal::default();
        let record = preview(&journal);
        let Ok(Ok(applying)) = journal.begin_apply(&record.id, &record.confirmation, "a", None) else { panic!() };
        journal.end_apply(&record.id, "a", Ok(&json!({"applied": {}})));
        let data =
            undone_in_daw(&journal, "undone_in_daw", json!({"descs": [applying.desc.unwrap(), "Adjust track volume"], "how": "undo"}));
        assert_eq!((&data["transactionIds"], &data["how"]), (&json!([record.id]), &json!("undo")));
        assert_eq!(undone_in_daw(&journal, "tracks", json!({"count": 3})), json!({"count": 3}));
    }

    #[test]
    fn the_daws_undo_of_a_plan_names_the_changes_made_in_its_step() {
        let journal = Journal::default();
        let label = journal.step_label("Tighten the drums");
        assert!(label.starts_with("Kumi: Tighten the drums (k") && label.ends_with(')'), "{label}");
        journal.step(Some(label.clone()));
        let (first, second) = (preview(&journal), preview(&journal));
        for record in [&first, &second] {
            journal.begin_apply(&record.id, &record.confirmation, "a", None).unwrap().unwrap();
            journal.end_apply(&record.id, "a", Ok(&json!({"applied": {}})));
        }
        // Named, as it closes, by what it did: the nonce stays, and the plan's changes are found by the new name.
        let named = journal.name_step("Bass volume −6 dB and 1 more").unwrap();
        assert_eq!(named, format!("Kumi: Bass volume −6 dB and 1 more{}", &label[label.rfind(" (k").unwrap()..]));
        journal.step(None);
        assert_eq!(journal.name_step("nothing open"), None);
        let after = preview(&journal);
        journal.begin_apply(&after.id, &after.confirmation, "a", None).unwrap().unwrap();
        journal.end_apply(&after.id, "a", Ok(&json!({"applied": {}})));
        assert_eq!(journal.by_descs(std::slice::from_ref(&named)), vec![first.id, second.id], "the plan's changes, not the one after");
        assert!(journal.by_descs(std::slice::from_ref(&label)).is_empty(), "the name it began with is gone");
    }

    #[test]
    fn the_daws_own_undo_finds_its_changes_by_their_step_description() {
        let journal = Journal::default();
        let record = preview(&journal);
        let Ok(Ok(applying)) = journal.begin_apply(&record.id, &record.confirmation, "a", Some("Bass −6 dB")) else { panic!() };
        journal.end_apply(&record.id, "a", Ok(&json!({"applied": {}})));
        let found = journal.by_descs(std::slice::from_ref(applying.desc.as_ref().unwrap()));
        assert_eq!(found, vec![record.id.clone()]);
        assert!(journal.by_descs(&["Kumi: something else (kzzzz)".into()]).is_empty());
    }
}
