//! Kumi's changes in a DAW whose bridge is its own (REAPER's extension, Renoise's tool): each one previewed and applied
//! through the host's journal (`change_preview`, `change_apply`), kept in HISTORY, and undone as a new step of the
//! DAW's (`change_undo`), never by the DAW's own undo. Plans run through core's plan engine over the DAW's kinds.
use super::own_bridge::{answer_of, follow_epoch, names_tracks, note_epoch, OwnDaw, Refused, CALL_MS, SHORT_REF};
use crate::{
    core::{
        contracts::*,
        daw::DawProfile,
        errors::RuntimeError,
        history::{cant_reach_to_undo, cant_take_back, Applied, ChangeHost, Entry, History, UndoResult},
        plan::{make_changes, stream_changes, ActionOutcome, ActionSummary, Batch, ChangeOutcome, PlanHost, StepKind},
        refs::{Book, RefScheme},
    },
    integrations::shared::{
        changes::{new_record, ChangeSummary},
        steps::{plan_tool, step_line},
        undo::undo_change,
    },
    mcp::client::McpEndpoint,
};
use async_trait::async_trait;
use chrono::Utc;
use indexmap::IndexMap;
use kumi_common::{
    abort::{self, Signal, SignalExt},
    js::json::stringify,
};
use serde_json::{json, Value};
use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    path::Path,
    rc::Rc,
};

/// The most changes one release names (the host takes 64 at a time).
const RELEASE_CHUNK: usize = 64;
/// How many of Kumi's latest changes the observation lists.
const CHANGES_SHOWN: usize = 12;

/// One of a DAW's change kinds: the step (and tool) that names it, the host's kind, and what HISTORY and the model are
/// told of it.
pub struct OwnKind {
    /// The step's name in make_changes, and the tool's when it has one: "track_volume".
    pub tool: &'static str,
    /// The host's (and the bridge's) name for it: "track-volume".
    pub kind: &'static str,
    pub family: ChangeFamily,
    pub description: &'static str,
    /// Its input, as JSON Schema.
    pub schema: &'static str,
    /// A tool of its own on every request; the others are make_changes steps.
    pub core: bool,
    /// Adds, deletes or moves tracks: later steps on tracks wait for it.
    pub structural: bool,
    /// Deletes what the producer made: the project's saved file is copied first.
    pub deletes: bool,
    /// Input fields the bridge names otherwise (the model's name, the bridge's): every ref field the model sees is named
    /// as refs are (`ref`, `…Ref`, `…Refs`), so Kumi shortens and lengthens it.
    pub wire: &'static [(&'static str, &'static str)],
}

impl OwnKind {
    pub const fn new(
        tool: &'static str,
        kind: &'static str,
        family: ChangeFamily,
        description: &'static str,
        schema: &'static str,
    ) -> Self {
        Self { tool, kind, family, description, schema, core: false, structural: false, deletes: false, wire: &[] }
    }
    pub const fn wire(mut self, renamed: &'static [(&'static str, &'static str)]) -> Self {
        self.wire = renamed;
        self
    }
    pub const fn core(mut self) -> Self {
        self.core = true;
        self
    }
    pub const fn structural(mut self) -> Self {
        self.structural = true;
        self
    }
    pub const fn deletes(mut self) -> Self {
        self.deletes = true;
        self
    }
    fn input_schema(&self) -> JsonObject {
        serde_json::from_str(self.schema).expect("a change kind's schema is JSON")
    }
}

impl StepKind for OwnKind {
    fn since(&self) -> Option<&str> {
        None
    }
}

/// What a DAW of Kumi's own can change, and how its plans and undo are described.
pub struct OwnChangeSet {
    pub kinds: &'static [OwnKind],
    /// make_changes' description before the lines of the steps that aren't tools of their own.
    pub plan_description: &'static str,
    pub undo_description: &'static str,
    /// What the model is told of changing the DAW, after the DAW's instructions.
    pub instructions: &'static str,
    /// Whether the DAW holds one undo step open across calls, so one press of its undo takes a whole plan back.
    pub plan_steps: bool,
}

/// make_changes' input: the same shape for every DAW (its steps' tool enum is filled in for the DAW).
const PLAN_SCHEMA: &str = r#"{"type":"object","additionalProperties":false,"required":["steps"],"properties":{"steps":{"type":"array","minItems":1,"maxItems":5000,"items":{"type":"object","additionalProperties":false,"required":["tool","input"],"properties":{"tool":{"type":"string"},"input":{"type":"object","description":"What that step takes; \"@name\" for what an earlier step made; wait takes {\"beats\": 8} or {\"seconds\": 4}"},"as":{"type":"string","pattern":"^[a-zA-Z][a-zA-Z0-9_]{0,31}$","description":"Name what this step makes (a new track, an added effect) for later steps"},"each":{"type":"object","description":"Repeat this step: each field's list gives that input field its value run by run, e.g. {\"volumeDb\": [-6, -3]}, or several lists of one length","additionalProperties":{"type":"array","maxItems":5000}}}}},"final":{"type":"boolean","description":"These changes complete the request: Kumi says what changed and you aren't called again. Leave it out to see the results and carry on."}}}"#;
const UNDO_SCHEMA: &str = r#"{"type":"object","properties":{"change":{"type":"string","minLength":1,"maxLength":32,"description":"A change id such as c3, or \"last\""},"discard":{"type":"boolean","description":"Put the change's values back even though the producer changed them since (only when they asked for that)"},"final":{"type":"boolean","description":"true when the undo completes the request: Kumi tells the producer, and the answer ends there"}},"required":["change"],"additionalProperties":false}"#;

/// Kumi's link to the DAW's host, shared by the integration and its changes.
pub struct Link<S: RefScheme> {
    pub daw: &'static OwnDaw,
    pub endpoint: Rc<RefCell<Option<Rc<dyn McpEndpoint>>>>,
    pub book: Rc<RefCell<Book<S>>>,
    pub epoch: Rc<RefCell<Option<String>>>,
    pub layout: Rc<Cell<Option<u64>>>,
    pub seen_in: Rc<RefCell<HashMap<String, String>>>,
    pub connected: Rc<Cell<bool>>,
    /// Cancelled when the integration closes.
    pub lifetime: Signal,
    /// The tempo as last read, for a wait in beats.
    pub tempo: Rc<Cell<Option<f64>>>,
    /// The project's saved file as last read, for the safety copy.
    pub file: Rc<RefCell<Option<String>>>,
}

impl<S: RefScheme> Link<S> {
    /// A host tool's answer, Ok with its JSON or Err with why; Err(RuntimeError) when the host isn't there.
    async fn call(&self, tool: &str, args: Value, signal: Signal) -> Result<Result<Value, Refused>, RuntimeError> {
        let Some(endpoint) = self.endpoint.borrow().clone() else {
            return Err(RuntimeError::plain(self.daw.profile.vocab.cant_reach));
        };
        let args = args.as_object().cloned().unwrap_or_default();
        let result = endpoint.call(tool, args, abort::any([signal, self.lifetime.clone(), abort::timeout(CALL_MS)])).await?;
        Ok(answer_of(&result))
    }

    fn words(&self) -> &'static DawProfile {
        self.daw.profile
    }

    /// The DAW's words for a refusal, with its long refs in this turn's short ones.
    fn in_short_refs(&self, why: &str) -> String {
        let mut why = why.to_owned();
        let mut book = self.book.borrow_mut();
        for reference in book.scheme.refs_in(&why.clone()) {
            let short = book.short_ref(reference);
            why = why.replace(reference, &short);
        }
        why
    }
}

/// What HISTORY asks of a DAW of Kumi's own: its refs, its clock, and its way back for one change (the host's undo).
pub struct OwnChanges<S: RefScheme> {
    pub link: Rc<Link<S>>,
}

#[async_trait(?Send)]
impl<S: RefScheme + 'static> ChangeHost for OwnChanges<S> {
    type Refs = S;
    /// Nothing: the host keeps what a delete removed, with the change's record in its journal.
    type Material = Value;
    fn profile(&self) -> &DawProfile {
        self.link.daw.profile
    }
    fn book(&self) -> &RefCell<Book<S>> {
        &self.link.book
    }
    fn lifetime(&self) -> Signal {
        self.link.lifetime.clone()
    }
    fn now_ms(&self) -> i64 {
        Utc::now().timestamp_millis()
    }
    fn keep_soon(&self) {}
    fn release(&self, transactions: Vec<String>) {
        let link = self.link.clone();
        tokio::task::spawn_local(async move {
            for chunk in transactions.chunks(RELEASE_CHUNK) {
                let signal = abort::timeout(10_000);
                if !matches!(link.call("change_release", json!({"transactionIds": chunk}), signal).await, Ok(Ok(_))) {
                    break;
                }
            }
        });
    }
    async fn ready(&self, _signal: Signal) -> bool {
        self.link.endpoint.borrow().is_some()
    }
    fn reachable(&self) -> bool {
        self.link.connected.get() && self.link.endpoint.borrow().is_some()
    }
    fn plan_steps(&self) -> bool {
        self.link.daw.changes.plan_steps
    }
    async fn step_begin(&self) -> Option<String> {
        let opened = self.link.call("undo_step_begin", json!({"label": "Kumi"}), abort::timeout(10_000)).await.ok()?.ok()?;
        opened.get("stepId").and_then(Value::as_str).map(str::to_owned)
    }
    async fn step_end(&self, id: String) {
        let _ = self.link.call("undo_step_end", json!({"stepId": id}), abort::timeout(10_000)).await;
    }
    async fn stop_transport(&self, signal: Signal) -> bool {
        matches!(self.link.call("transport_stop", json!({}), signal).await, Ok(Ok(answer)) if answer.get("stopped") != Some(&Value::Bool(false)))
    }
    async fn revert(
        &self,
        history: &History<Self>,
        entry: &Entry<Value>,
        _revert: Vec<Value>,
        _signal: Signal,
    ) -> Result<UndoResult, RuntimeError> {
        // Own DAWs keep no revert scripts: every change is undone through the host.
        Ok(UndoResult::with(entry.borrow().record.clone(), cant_take_back(history.host.profile()), true))
    }
    async fn undo_material(
        &self,
        history: &History<Self>,
        entry: &Entry<Value>,
        snapshot: &Applied<Value>,
        _material: Value,
        undo_key: &str,
        discard: bool,
        _signal: Signal,
    ) -> Result<UndoResult, RuntimeError> {
        match self.undo_transaction(history, entry, snapshot, undo_key, discard).await? {
            Some(stopped) => Ok(stopped),
            None => Ok(history.undone(entry)),
        }
    }
    async fn undo_transaction(
        &self,
        history: &History<Self>,
        entry: &Entry<Value>,
        snapshot: &Applied<Value>,
        undo_key: &str,
        discard: bool,
    ) -> Result<Option<UndoResult>, RuntimeError> {
        let words = self.link.words().words;
        let mut args = json!({"transactionId": snapshot.transaction_id, "confirmation": "undo", "idempotencyKey": undo_key});
        if discard {
            args["discard"] = true.into();
        }
        let answer = match self.link.call("change_undo", args, history.change_signal()).await {
            Ok(answer) => answer,
            Err(error) if error.is_aborted() && !self.link.lifetime.is_cancelled() => {
                let note = format!("{} didn't answer the undo; try again.", words.app);
                return Ok(Some(UndoResult::with(
                    history.update(entry, ChangeState::Unsure, Some(note)),
                    format!("{} didn't answer the undo; it can be retried.", words.app),
                    true,
                )));
            }
            Err(_) => return Ok(Some(UndoResult::error(cant_reach_to_undo(self.link.words())))),
        };
        let refused = match answer {
            Ok(done) if done.get("state").and_then(Value::as_str) == Some("undone") => {
                // A restore that made devices again moves Renoise's epoch: the book follows.
                follow_epoch(&self.link.book, &self.link.epoch, &self.link.layout, &done);
                note_epoch(&self.link.seen_in, &self.link.book.borrow().scheme, &done);
                return Ok(None);
            }
            Ok(other) => {
                let note = format!("{} didn't confirm the undo; try again.", words.app);
                return Ok(Some(UndoResult::with(history.update(entry, ChangeState::Unsure, Some(note)), stringify(&other), true)));
            }
            Err(refused) => refused,
        };
        let why = self.link.in_short_refs(&refused.why);
        let (state, note) = match refused.code.as_deref() {
            Some("changed") => (ChangeState::Kept, format!("It changed in {} since, so Kumi left it as it is.", words.app)),
            Some("gone") => (ChangeState::Kept, format!("What it changed is gone from the {}, so there's nothing to undo.", words.place)),
            Some("stale") => (
                ChangeState::Kept,
                format!("The {} changed since (another one is open, or it was reloaded), so Kumi left it.", words.place),
            ),
            Some("undone") => return Ok(Some(history.undone(entry))),
            Some("unknown") => (ChangeState::Expired, format!("Kumi can't undo this anymore; {} can.", self.link.words().vocab.own_undo)),
            Some("uncertain") => {
                (ChangeState::Unsure, format!("Whether this change reached {} isn't known, so Kumi won't undo it blindly.", words.app))
            }
            // Busy (a dialog, a render) or another refusal with nothing written: the change stays as it was, undoable.
            Some("busy") => {
                return Ok(Some(UndoResult::with(
                    snapshot.record.clone(),
                    format!("{} is busy ({why}): nothing was undone; try again in a moment.", words.app),
                    true,
                )))
            }
            _ => return Ok(Some(UndoResult::with(snapshot.record.clone(), why, true))),
        };
        Ok(Some(UndoResult::with(history.update(entry, state, Some(note)), why, true)))
    }
}

/// The DAW's change engine: what a plan, a change tool and an undo ask of it.
pub struct OwnPlan<S: RefScheme + 'static> {
    pub history: History<OwnChanges<S>>,
    names: RefCell<IndexMap<String, String>>,
    on_action: Option<Rc<dyn Fn(ActionEvent)>>,
    /// Saved files already copied before a delete (by path, size and time): one copy per version.
    copied: RefCell<Vec<String>>,
}

impl<S: RefScheme + 'static> OwnPlan<S> {
    pub fn new(
        link: Rc<Link<S>>,
        timeout_ms: Option<u64>,
        on_change: Option<Rc<dyn Fn(ChangeRecord)>>,
        on_action: Option<Rc<dyn Fn(ActionEvent)>>,
    ) -> Rc<Self> {
        Rc::new(Self {
            history: History::over(OwnChanges { link }, timeout_ms, on_change),
            names: RefCell::default(),
            on_action,
            copied: RefCell::default(),
        })
    }

    fn link(&self) -> &Link<S> {
        &self.history.host.link
    }

    /// The tools for this DAW's changes: each core kind's, make_changes with every kind as a step, and undo_change.
    pub fn tools(self: &Rc<Self>) -> Vec<Rc<dyn KernelTool>> {
        let set = self.link().daw.changes;
        let mut tools: Vec<Rc<dyn KernelTool>> = Vec::new();
        let mut lines = Vec::new();
        for kind in set.kinds {
            if kind.core {
                tools.push(Rc::new(ChangeTool { engine: self.clone(), kind }));
            } else {
                lines.push(step_line(kind.tool, kind.description, &kind.input_schema()));
            }
        }
        let steps = set.kinds.iter().map(|kind| kind.tool.to_owned()).collect();
        let (description, schema) = plan_tool(set.plan_description, &serde_json::from_str(PLAN_SCHEMA).expect("JSON"), steps, &lines);
        tools.push(Rc::new(PlanTool { engine: self.clone(), description, schema }));
        tools.push(Rc::new(UndoTool { engine: self.clone() }));
        tools
    }

    /// The observation's list of Kumi's latest changes and where each stands.
    pub fn latest(&self) -> Option<Value> {
        let entries = self.history.entries.borrow();
        let rows: Vec<Value> = entries
            .values()
            .rev()
            .take(CHANGES_SHOWN)
            .map(|entry| {
                let record = &entry.borrow().record;
                let mut row = json!({"change": record.id, "title": record.title, "state": record.state});
                if let Some(note) = &record.note {
                    row["note"] = json!(note);
                }
                row
            })
            .collect();
        (!rows.is_empty()).then(|| Value::Array(rows.into_iter().rev().collect()))
    }

    /// The producer took Kumi's changes back (or put them back) with the DAW's own undo: HISTORY follows.
    pub fn undone_in_daw(&self, data: &Value) {
        let ids: Vec<&str> = data.get("transactionIds").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str).collect();
        if ids.is_empty() {
            return;
        }
        let redo = data.get("how").and_then(Value::as_str) == Some("redo");
        let profile = self.link().words();
        let entries: Vec<_> =
            self.history.entries.borrow().values().filter(|entry| ids.contains(&entry.borrow().transaction_id.as_str())).cloned().collect();
        for entry in entries {
            let state = entry.borrow().record.state;
            match (redo, state) {
                (false, ChangeState::Applied | ChangeState::Unsure) => {
                    self.history.update(&entry, ChangeState::Undone, Some(format!("Undone with {}.", profile.vocab.own_undo)));
                }
                (true, ChangeState::Undone) => {
                    self.history.update(&entry, ChangeState::Applied, Some(format!("Put back with {}'s redo.", profile.words.app)));
                }
                _ => {}
            }
        }
        self.history.outside.borrow_mut().push(profile.vocab.own_undo);
    }

    /// The input with this turn's short refs made long, or what to say of a short ref the turn doesn't know.
    fn lengthened(&self, input: &JsonObject) -> Result<JsonObject, String> {
        let book = self.link().book.borrow();
        let long = book.lengthen(&Value::Object(input.clone()));
        let mut unknown = None;
        find_unknown(&long, "", &mut unknown, 0);
        if let Some(asked) = unknown {
            let place = self.link().words().words.place;
            return Err(format!(
                "{asked} isn't one of this turn's refs (it may be from before the {place} changed): read again for a current one"
            ));
        }
        Ok(long.as_object().cloned().unwrap_or_default())
    }

    /// Where a change is asked from: the epoch its refs were read in, and (refs that are positions) their layout.
    fn place(&self, args: &mut Value, input: &JsonObject) {
        let link = self.link();
        let book = link.book.borrow();
        let positional = book.scheme.positional();
        let primary = ["ref", "parent", "trackRef", "track"].iter().find_map(|key| input.get(*key).and_then(Value::as_str));
        let epoch = match primary {
            Some(reference) if !positional => link.seen_in.borrow().get(reference).cloned().or_else(|| link.epoch.borrow().clone()),
            _ => link.epoch.borrow().clone(),
        };
        if let Some(epoch) = epoch {
            args["epoch"] = epoch.into();
        }
        if let Some(layout) = link.layout.get().filter(|_| names_tracks(&book.scheme, input)) {
            args["layout"] = layout.into();
        }
    }

    /// A refusal before anything was written, in Kumi's words.
    fn refusal(&self, refused: &Refused, input: &JsonObject) -> String {
        let profile = self.link().words();
        let words = profile.words;
        let why = self.link().in_short_refs(&refused.why);
        let what = input.get("ref").and_then(Value::as_str).unwrap_or("That ref");
        match refused.code.as_deref() {
            Some("stale") if self.link().book.borrow().scheme.positional() => {
                format!("{what} is from before the {} changed (something was added, removed or opened): read again for current refs; nothing changed", words.place)
            }
            Some("stale") => format!(
                "{what} was read in another {} than the one {} has open now: read again for current refs; nothing changed",
                words.place, words.app
            ),
            Some("changed") => format!(
                "It changed in {} between Kumi's look and the change ({why}), so nothing was written: read it again and retry",
                words.app
            ),
            Some("gone") => format!("{what} is gone from the {}: nothing changed", words.place),
            Some("unknown-method") => format!(
                "{}'s {} is older than this Kumi and can't make changes yet: run kumi --daw {} bridge, then restart {}",
                words.app,
                if self.link().daw.host_flag == "renoise" { "tool" } else { "extension" },
                self.link().daw.host_flag,
                words.app
            ),
            _ => why,
        }
    }

    /// One change: previewed, applied in one named step of the DAW's undo, and kept in HISTORY.
    async fn one_change(&self, kind: &'static OwnKind, input: JsonObject, signal: Signal) -> ChangeOutcome {
        let words = self.link().words().words;
        if self.history.changes_this_turn.get() >= 5_000 {
            return ChangeOutcome::stop("That's 5000 changes in one answer; carry on in the next one");
        }
        let mut input = match self.lengthened(&input) {
            Ok(input) => input,
            Err(why) => return ChangeOutcome::error(why),
        };
        for (ours, theirs) in kind.wire {
            if let Some(value) = input.remove(*ours) {
                input.insert((*theirs).into(), value);
            }
        }
        let mut args = json!({"kind": kind.kind, "input": input});
        self.place(&mut args, &input);
        let preview = match self.link().call("change_preview", args, signal.clone()).await {
            Ok(Ok(preview)) => preview,
            Ok(Err(refused)) if refused.busy => {
                return ChangeOutcome::error(format!("{} is busy ({}): nothing changed; try again in a moment", words.app, refused.why))
            }
            Ok(Err(refused)) => return ChangeOutcome::error(self.refusal(&refused, &input)),
            Err(error) if error.is_aborted() => {
                return ChangeOutcome::error(format!("Stopped before {} was asked: nothing changed", words.app))
            }
            Err(_) => return ChangeOutcome::error(self.link().words().vocab.cant_reach),
        };
        follow_epoch(&self.link().book, &self.link().epoch, &self.link().layout, &preview);
        let title = preview.pointer("/display/title").and_then(Value::as_str).unwrap_or(kind.kind).to_owned();
        // Nothing to change (already so): no step in the DAW's undo, no row in HISTORY.
        if preview.pointer("/display/unchanged") == Some(&Value::Bool(true)) {
            let id = preview.get("transactionId").and_then(Value::as_str).map(str::to_owned);
            self.history.release(&id.into_iter().collect::<Vec<_>>());
            return ChangeOutcome {
                text: stringify(&json!({"changed": title, "unchanged": true})),
                is_error: false,
                missed: None,
                stops: false,
            };
        }
        let (Some(transaction), Some(confirmation)) =
            (preview.get("transactionId").and_then(Value::as_str), preview.get("confirmation").and_then(Value::as_str))
        else {
            return ChangeOutcome::error(format!("{} answered the preview without a transaction: nothing changed", words.app));
        };
        let transaction = transaction.to_owned();
        let apply = json!({"transactionId": transaction, "confirmation": confirmation, "idempotencyKey": uuid::Uuid::new_v4().to_string()});
        // Once asked, the apply runs to its answer: an Esc mid-way would leave the change's outcome unknown.
        let applied = match self.link().call("change_apply", apply, self.history.change_signal()).await {
            Ok(Ok(applied)) => applied,
            Ok(Err(refused)) if refused.code.as_deref() == Some("uncertain") || refused.why.contains("uncertain") => {
                self.remember(kind, &title, &transaction, ChangeState::Unsure, Some(format!("{} didn't confirm this change.", words.app)));
                return ChangeOutcome::stop(format!("{} didn't confirm this change ({title}), so it may or may not have happened: read it before going on", words.app));
            }
            Ok(Err(refused)) if refused.busy => {
                return ChangeOutcome::stop(format!(
                    "{} went busy ({}) before this step, so it wasn't made and nothing of it was written; the steps before it stand, each undoable",
                    words.app, refused.why
                ))
            }
            Ok(Err(refused)) => return ChangeOutcome::error(self.refusal(&refused, &input)),
            Err(_) => {
                self.remember(kind, &title, &transaction, ChangeState::Unsure, Some(format!("{} didn't answer this change.", words.app)));
                return ChangeOutcome::stop(format!("{} didn't answer this change ({title}), so it may or may not have happened: read it before going on", words.app));
            }
        };
        follow_epoch(&self.link().book, &self.link().epoch, &self.link().layout, &applied);
        note_epoch(&self.link().seen_in, &self.link().book.borrow().scheme, &applied);
        let title = applied.pointer("/display/title").and_then(Value::as_str).map(str::to_owned).unwrap_or(title);
        let note = applied.pointer("/display/note").and_then(Value::as_str).map(str::to_owned);
        let id = self.remember(kind, &title, &transaction, ChangeState::Applied, None);
        let mut reply = json!({"changed": title, "change": id});
        if let Some(made) = applied.pointer("/created/ref").and_then(Value::as_str) {
            reply["ref"] = json!(self.link().book.borrow_mut().short_ref(made));
        }
        if let Some(restored) = applied.pointer("/applied/base") {
            reply["base"] = self.link().book.borrow_mut().shorten(restored);
        }
        if let Some(note) = note {
            reply["note"] = json!(note);
        }
        if applied.pointer("/undo/entry") == Some(&Value::Bool(false)) && reply.get("note").is_none() {
            reply["note"] = json!(format!("{} can't take this back; Kumi can.", self.link().words().vocab.own_undo));
        }
        ChangeOutcome { text: stringify(&reply), is_error: false, missed: None, stops: false }
    }

    /// A change kept in HISTORY under its transaction: its id.
    fn remember(&self, kind: &OwnKind, title: &str, transaction: &str, state: ChangeState, note: Option<String>) -> String {
        let mut record = new_record(kind.family, ChangeSummary::title(title), state, self.history.host.now_ms());
        record.note = note;
        let id = record.id.clone();
        self.history.remember(record, transaction.into(), None);
        self.history.made_by(&id, kind.tool);
        self.history.changes_this_turn.set(self.history.changes_this_turn.get() + 1);
        id
    }

    /// A copy of the project's saved file beside it, once per saved version: its path.
    fn copy_saved(&self) -> Option<String> {
        let path = self.link().file.borrow().clone()?;
        let stats = std::fs::metadata(&path).ok()?;
        let modified = stats.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?.as_millis();
        let version = format!("{path}:{}:{modified}", stats.len());
        if self.copied.borrow().contains(&version) {
            return None;
        }
        let source = Path::new(&path);
        let folder = source.parent()?.join("Kumi copies");
        let stem = source.file_stem()?.to_string_lossy();
        let extension = source.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
        std::fs::create_dir_all(&folder).ok()?;
        let copy = folder.join(format!("{stem} {}{extension}", Utc::now().format("%Y-%m-%d %H.%M.%S")));
        std::fs::copy(source, &copy).ok()?;
        self.copied.borrow_mut().push(version);
        Some(copy.to_string_lossy().into_owned())
    }
}

/// The first short ref a request names that lengthening left short: one this turn doesn't know.
fn find_unknown(value: &Value, key: &str, out: &mut Option<String>, depth: usize) {
    if out.is_some() || depth > 16 {
        return;
    }
    match value {
        Value::String(text)
            if (key == "ref" || key == "parent" || key.ends_with("Ref") || key.ends_with("Refs")) && SHORT_REF.is_match(text) =>
        {
            *out = Some(text.clone())
        }
        Value::Array(items) => items.iter().for_each(|item| find_unknown(item, key, out, depth + 1)),
        Value::Object(row) => row.iter().for_each(|(key, item)| find_unknown(item, key, out, depth + 1)),
        _ => {}
    }
}

#[async_trait(?Send)]
impl<S: RefScheme + 'static> PlanHost for OwnPlan<S> {
    type Changes = OwnChanges<S>;
    type Change = OwnKind;
    type Action = OwnKind;
    fn history(&self) -> &History<OwnChanges<S>> {
        &self.history
    }
    fn names(&self) -> &RefCell<IndexMap<String, String>> {
        &self.names
    }
    fn change_kind(&self, tool: &str) -> Option<&'static OwnKind> {
        self.link().daw.changes.kinds.iter().find(|kind| kind.tool == tool)
    }
    fn action_kind(&self, _tool: &str) -> Option<&'static OwnKind> {
        None
    }
    fn structural(&self, tool: &str) -> bool {
        self.change_kind(tool).is_some_and(|kind| kind.structural)
    }
    fn moves_transport(&self, _tool: &str) -> bool {
        false
    }
    fn may_lose_material(&self, tool: &str) -> bool {
        self.change_kind(tool).is_some_and(|kind| kind.deletes)
    }
    fn supported(&self, _since: Option<&str>) -> bool {
        true
    }
    fn too_old(&self, _since: Option<&str>) -> String {
        String::new()
    }
    fn tempo(&self) -> Option<f64> {
        self.link().tempo.get()
    }
    fn batches(&self) -> &[Batch] {
        &[]
    }
    fn batch_kind(&self, _batch: &Batch) -> &'static OwnKind {
        unreachable!("own DAWs offer no batches")
    }
    fn batch_offered(&self, _batch: &Batch) -> bool {
        false
    }
    fn batch_input(&self, _batch: &Batch, _group: &[JsonObject]) -> JsonObject {
        JsonObject::new()
    }
    async fn change(&self, kind: &'static OwnKind, input: JsonObject, signal: Signal, _confirmed: bool) -> ChangeOutcome {
        self.one_change(kind, input, signal).await
    }
    async fn act(&self, _action: &'static OwnKind, _input: JsonObject, _signal: Signal) -> ActionOutcome {
        ActionOutcome::error("no actions yet")
    }
    async fn safety_copy(&self, _signal: Signal) -> Result<Option<String>, RuntimeError> {
        Ok(self.copy_saved())
    }
    fn emit_action(&self, done: &ActionSummary) {
        if let Some(on_action) = &self.on_action {
            on_action(ActionEvent { title: done.title.clone(), playing: done.playing, recording: done.recording });
        }
    }
}

/// One change kind's own tool.
struct ChangeTool<S: RefScheme + 'static> {
    engine: Rc<OwnPlan<S>>,
    kind: &'static OwnKind,
}

#[async_trait(?Send)]
impl<S: RefScheme + 'static> KernelTool for ChangeTool<S> {
    fn name(&self) -> &str {
        self.kind.tool
    }
    fn description(&self) -> &str {
        self.kind.description
    }
    fn input_schema(&self) -> JsonObject {
        self.kind.input_schema()
    }
    async fn execute(&self, input: JsonObject, signal: Signal) -> Result<ToolResult, RuntimeError> {
        signal.check()?;
        let outcome = self.engine.one_change(self.kind, input, signal).await;
        Ok(ToolResult { text: outcome.text, is_error: outcome.is_error, ..Default::default() })
    }
    fn listed_by(&self) -> Option<&str> {
        Some("make_changes")
    }
}

/// make_changes: the DAW's changes as steps of one plan.
struct PlanTool<S: RefScheme + 'static> {
    engine: Rc<OwnPlan<S>>,
    description: String,
    schema: JsonObject,
}

#[async_trait(?Send)]
impl<S: RefScheme + 'static> KernelTool for PlanTool<S> {
    fn name(&self) -> &str {
        "make_changes"
    }
    fn description(&self) -> &str {
        &self.description
    }
    fn input_schema(&self) -> JsonObject {
        self.schema.clone()
    }
    async fn execute(&self, input: JsonObject, signal: Signal) -> Result<ToolResult, RuntimeError> {
        make_changes(&self.engine, input, signal).await
    }
    fn stream(&self, signal: Signal, on_start: Rc<dyn Fn()>) -> Option<Box<dyn StreamingCall>> {
        Some(stream_changes(&self.engine, signal, on_start))
    }
}

/// undo_change: one of Kumi's changes taken back as a new step of the DAW's undo.
struct UndoTool<S: RefScheme + 'static> {
    engine: Rc<OwnPlan<S>>,
}

#[async_trait(?Send)]
impl<S: RefScheme + 'static> KernelTool for UndoTool<S> {
    fn name(&self) -> &str {
        "undo_change"
    }
    fn description(&self) -> &str {
        self.engine.link().daw.changes.undo_description
    }
    fn input_schema(&self) -> JsonObject {
        serde_json::from_str(UNDO_SCHEMA).expect("JSON")
    }
    async fn execute(&self, input: JsonObject, signal: Signal) -> Result<ToolResult, RuntimeError> {
        undo_change(&self.engine.history, &input, signal).await
    }
}
