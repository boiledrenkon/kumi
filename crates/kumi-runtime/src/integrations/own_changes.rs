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
        undo::undo_change_with,
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
    /// What HISTORY draws of it, from the values it found and left.
    pub picture: Picture,
}

/// What HISTORY draws of a change: a value moved within its span (a fader, a pan), or a colour's swatches.
#[derive(Clone, Copy)]
pub enum Picture {
    None,
    Meter { field: &'static str, min: f64, max: f64 },
    Colors { field: &'static str },
}

impl OwnKind {
    pub const fn new(
        tool: &'static str,
        kind: &'static str,
        family: ChangeFamily,
        description: &'static str,
        schema: &'static str,
    ) -> Self {
        Self { tool, kind, family, description, schema, core: false, structural: false, deletes: false, wire: &[], picture: Picture::None }
    }
    pub const fn meter(mut self, field: &'static str, min: f64, max: f64) -> Self {
        self.picture = Picture::Meter { field, min, max };
        self
    }
    pub const fn colors(mut self, field: &'static str) -> Self {
        self.picture = Picture::Colors { field };
        self
    }
    /// HISTORY's summary of the change: its title, and its picture from the values before (`prior`) and after.
    fn summary(&self, title: &str, prior: Option<&Value>, after: Option<&Value>) -> ChangeSummary {
        let mut summary = ChangeSummary::title(title);
        let (Some(prior), Some(after)) = (prior.filter(|v| v.is_object()), after.filter(|v| v.is_object())) else { return summary };
        match self.picture {
            Picture::Meter { field, min, max } => {
                // A value the DAW leaves out (Renoise's volume at silence) is the bottom of its span.
                let value = |values: &Value| values.get(field).and_then(Value::as_f64).unwrap_or(min).clamp(min, max);
                if after.get(field).is_some() {
                    (summary.from, summary.to, summary.range) = (Some(value(prior)), Some(value(after)), Some([min, max]));
                }
            }
            Picture::Colors { field } => {
                if let Some(to) = after.get(field).and_then(Value::as_str) {
                    let from = prior.get(field).and_then(Value::as_str).map(str::to_owned);
                    summary.colors = Some(ColorChange { from, to: to.to_owned() });
                }
            }
            Picture::None => {}
        }
        summary
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

/// One of a DAW's actions: what a plan's step or a tool of its own asks the DAW to do that no HISTORY row keeps (play,
/// stop, move the cursor), through one of the host's tools.
pub struct OwnAction {
    pub tool: &'static str,
    /// The host's tool for it.
    pub method: &'static str,
    pub description: &'static str,
    /// Its input, as JSON Schema.
    pub schema: &'static str,
    /// A tool of its own on every request; the others are make_changes steps.
    pub core: bool,
    /// What it leaves the transport doing: playing, stopped, or as it was.
    pub playing: Option<bool>,
    /// What the producer is told it did.
    pub title: &'static str,
}

impl OwnAction {
    fn input_schema(&self) -> JsonObject {
        serde_json::from_str(self.schema).expect("an action's schema is JSON")
    }
}

impl StepKind for OwnAction {
    fn since(&self) -> Option<&str> {
        None
    }
}

/// What a DAW of Kumi's own can change, and how its plans and undo are described.
pub struct OwnChangeSet {
    pub kinds: &'static [OwnKind],
    pub actions: &'static [OwnAction],
    /// make_changes' description before the lines of the steps that aren't tools of their own.
    pub plan_description: &'static str,
    pub undo_description: &'static str,
    /// What the model is told of changing the DAW, after the DAW's instructions.
    pub instructions: &'static str,
    /// Whether the DAW holds one undo step open across calls, so one press of its undo takes a whole plan back.
    pub plan_steps: bool,
    /// The most tracks one answer may add, where each costs the DAW a lot in a big project (Renoise).
    pub most_track_adds: Option<usize>,
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
    /// While a plan's undo step is open: the titles of the changes made in it, which name the step when it closes.
    pub step_titles: RefCell<Option<Vec<String>>>,
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
        let id = opened.get("stepId").and_then(Value::as_str).map(str::to_owned)?;
        *self.link.step_titles.borrow_mut() = Some(Vec::new());
        Some(id)
    }
    async fn step_end(&self, id: String) {
        // The DAW's undo menu names the plan by what it did: its first change, and how many more.
        let titles = self.link.step_titles.borrow_mut().take().unwrap_or_default();
        let mut args = json!({"stepId": id});
        if let Some(first) = titles.first() {
            args["label"] = match titles.len() {
                1 => first.clone(),
                n => format!("{first} and {} more", n - 1),
            }
            .into();
        }
        let _ = self.link.call("undo_step_end", args, abort::timeout(10_000)).await;
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
            // Sent, and not confirmed (no answer came, or the DAW didn't show the values back): the host keeps the undo
            // from going again, and HISTORY says why.
            Err(refused) if refused.uncertain => {
                let note =
                    format!("{} didn't confirm the undo, so whether it went through isn't known; Kumi won't send it again.", words.app);
                let why = self.link.in_short_refs(&refused.why);
                return Ok(Some(UndoResult::with(history.update(entry, ChangeState::Unsure, Some(note)), why, true)));
            }
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
            // The journal's refusal: the apply, or an undo sent before, that the DAW never confirmed.
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
    /// Tracks added this answer (`OwnChangeSet::most_track_adds`).
    added: Cell<usize>,
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
            added: Cell::new(0),
        })
    }

    fn link(&self) -> &Link<S> {
        &self.history.host.link
    }

    /// The tools for this DAW's changes: each core kind's and action's, make_changes with every kind and action as a step,
    /// and undo_change.
    pub fn tools(self: &Rc<Self>) -> Vec<Rc<dyn KernelTool>> {
        let set = self.link().daw.changes;
        if set.kinds.is_empty() && set.actions.is_empty() {
            // A DAW Kumi only reads so far (Logic, until M3): no change tools at all, not an empty plan.
            return Vec::new();
        }
        let mut tools: Vec<Rc<dyn KernelTool>> = Vec::new();
        let mut lines = Vec::new();
        for kind in set.kinds {
            if kind.core {
                tools.push(Rc::new(ChangeTool { engine: self.clone(), kind }));
            } else {
                lines.push(step_line(kind.tool, kind.description, &kind.input_schema()));
            }
        }
        for action in set.actions {
            if action.core {
                tools.push(Rc::new(ActionTool { engine: self.clone(), action }));
            } else {
                lines.push(step_line(action.tool, action.description, &action.input_schema()));
            }
        }
        let steps = set.kinds.iter().map(|kind| kind.tool).chain(set.actions.iter().map(|action| action.tool)).map(str::to_owned).collect();
        let (description, schema) = plan_tool(set.plan_description, &serde_json::from_str(PLAN_SCHEMA).expect("JSON"), steps, &lines);
        tools.push(Rc::new(PlanTool { engine: self.clone(), description, schema }));
        tools.push(Rc::new(UndoTool { engine: self.clone() }));
        tools
    }

    /// A new answer: its counts of changes and added tracks start again.
    pub fn new_turn(&self) {
        self.history.changes_this_turn.set(0);
        self.added.set(0);
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

    /// A refusal before anything was written, in Kumi's words (`asked`: the input as the model gave it, in short refs).
    fn refusal(&self, refused: &Refused, asked: &JsonObject) -> String {
        let profile = self.link().words();
        let words = profile.words;
        let why = self.link().in_short_refs(&refused.why);
        let what = asked.get("ref").and_then(Value::as_str).unwrap_or("That ref");
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
    async fn one_change(&self, kind: &'static OwnKind, asked: JsonObject, signal: Signal) -> ChangeOutcome {
        let words = self.link().words().words;
        if self.history.changes_this_turn.get() >= 5_000 {
            return ChangeOutcome::stop("That's 5000 changes in one answer; carry on in the next one");
        }
        // Each track a DAW like Renoise adds costs it more in a big song (and it stalls near 600): an answer adds a few.
        let most = self.link().daw.changes.most_track_adds.filter(|_| kind.kind == "track-add");
        if let Some(most) = most.filter(|most| self.added.get() >= *most) {
            return ChangeOutcome::stop(format!(
                "That's {most} tracks added in one answer, the most {} takes at once; carry on in the next one",
                self.link().words().words.app
            ));
        }
        let mut input = match self.lengthened(&asked) {
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
            // Busy (a dialog): the plan stops here, so nothing after it is made out of order once the dialog closes.
            Ok(Err(refused)) if refused.busy => {
                return ChangeOutcome::stop(format!(
                    "{} is busy ({}), so this wasn't made and nothing of it was written; in a plan, the steps after it weren't run and the ones before it stand, each undoable. Try again once it's free",
                    words.app, refused.why
                ))
            }
            Ok(Err(refused)) => return ChangeOutcome::error(self.refusal(&refused, &asked)),
            Err(error) if error.is_aborted() => {
                return ChangeOutcome::error(format!("Stopped before {} was asked: nothing changed", words.app))
            }
            Err(_) => return ChangeOutcome::error(self.link().words().vocab.cant_reach),
        };
        follow_epoch(&self.link().book, &self.link().epoch, &self.link().layout, &preview);
        let title = preview.pointer("/display/title").and_then(Value::as_str).unwrap_or(kind.kind).to_owned();
        let prior = preview.get("prior").cloned();
        // Nothing to change (already so): no step in the DAW's undo, no row in HISTORY.
        if preview.pointer("/display/unchanged") == Some(&Value::Bool(true)) {
            let id = preview.get("transactionId").and_then(Value::as_str).map(str::to_owned);
            self.history.release(&id.into_iter().collect::<Vec<_>>());
            // (A plan's answer keeps only a step's `changed`: the reason goes there.)
            return ChangeOutcome {
                text: stringify(&json!({"changed": format!("{title}: it was so already, so nothing changed"), "unchanged": true})),
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
            Ok(Err(refused)) if refused.uncertain || refused.code.as_deref() == Some("uncertain") || refused.why.contains("uncertain") => {
                self.remember(kind, kind.summary(&title, None, None), &transaction, ChangeState::Unsure, Some(format!("{} didn't confirm this change.", words.app)));
                return ChangeOutcome::stop(format!("{} didn't confirm this change ({title}), so it may or may not have happened: read it before going on", words.app));
            }
            Ok(Err(refused)) if refused.busy => {
                return ChangeOutcome::stop(format!(
                    "{} went busy ({}) before this was written, so it wasn't made and nothing of it was written; in a plan, the steps after it weren't run and the ones before it stand, each undoable. Try again once it's free",
                    words.app, refused.why
                ))
            }
            Ok(Err(refused)) => return ChangeOutcome::error(self.refusal(&refused, &asked)),
            Err(_) => {
                self.remember(kind, kind.summary(&title, None, None), &transaction, ChangeState::Unsure, Some(format!("{} didn't answer this change.", words.app)));
                return ChangeOutcome::stop(format!("{} didn't answer this change ({title}), so it may or may not have happened: read it before going on", words.app));
            }
        };
        follow_epoch(&self.link().book, &self.link().epoch, &self.link().layout, &applied);
        note_epoch(&self.link().seen_in, &self.link().book.borrow().scheme, &applied);
        let title = applied.pointer("/display/title").and_then(Value::as_str).map(str::to_owned).unwrap_or(title);
        // What the producer should know of it (a step the DAW's own undo skips), on its row and in its answer.
        let own_undo = self.link().words().vocab.own_undo;
        let note = applied.pointer("/display/note").and_then(Value::as_str).map(str::to_owned).or_else(|| {
            (applied.pointer("/undo/entry") == Some(&Value::Bool(false))).then(|| format!("{own_undo} can't take this back; Kumi can."))
        });
        if kind.kind == "track-add" {
            self.added.set(self.added.get() + 1);
        }
        let summary = kind.summary(&title, prior.as_ref(), applied.get("applied"));
        let id = self.remember(kind, summary, &transaction, ChangeState::Applied, note.clone());
        // A plan's answer keeps a step's `changed` and not its other fields, so the note goes in it.
        let changed = match &note {
            Some(note) => format!("{title} ({})", note.trim_end_matches('.')),
            None => title,
        };
        let mut reply = json!({"changed": changed, "change": id});
        if let Some(made) = applied.pointer("/created/ref").and_then(Value::as_str) {
            reply["ref"] = json!(self.link().book.borrow_mut().short_ref(made));
        }
        if let Some(restored) = applied.pointer("/applied/base") {
            reply["base"] = self.link().book.borrow_mut().shorten(restored);
        }
        ChangeOutcome { text: stringify(&reply), is_error: false, missed: None, stops: false }
    }

    /// One action through the host's tool: what the producer is told it did, or why it didn't.
    async fn one_action(&self, action: &'static OwnAction, input: JsonObject, signal: Signal) -> ActionOutcome {
        let words = self.link().words().words;
        let mut args = Value::Object(input.clone());
        self.place(&mut args, &input);
        match self.link().call(action.method, args, signal).await {
            Ok(Ok(answer)) => {
                follow_epoch(&self.link().book, &self.link().epoch, &self.link().layout, &answer);
                let playing = action.playing.or_else(|| answer.get("playing").and_then(Value::as_bool));
                let recording = (action.playing == Some(false)).then_some(false);
                let mut text = json!({"done": action.title});
                for field in ["playing", "positionBeats", "position", "wasRecording"] {
                    if let Some(value) = answer.get(field) {
                        text[field] = value.clone();
                    }
                }
                let done = ActionSummary { title: action.title.into(), playing, recording };
                ActionOutcome { text: stringify(&text), is_error: false, maybe: None, done: Some(done) }
            }
            Ok(Err(refused)) if refused.busy => {
                ActionOutcome::error(format!("{} is busy ({}): try again in a moment", words.app, refused.why))
            }
            Ok(Err(refused)) => ActionOutcome::error(self.refusal(&refused, &input)),
            Err(_) => ActionOutcome {
                text: format!("{} didn't answer, so it may or may not have done it", words.app),
                is_error: true,
                maybe: Some(true),
                done: None,
            },
        }
    }

    /// A change kept in HISTORY under its transaction: its id.
    fn remember(&self, kind: &OwnKind, summary: ChangeSummary, transaction: &str, state: ChangeState, note: Option<String>) -> String {
        if let Some(titles) = self.link().step_titles.borrow_mut().as_mut() {
            titles.push(summary.title.clone());
        }
        let mut record = new_record(kind.family, summary, state, self.history.host.now_ms());
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
    type Action = OwnAction;
    fn history(&self) -> &History<OwnChanges<S>> {
        &self.history
    }
    fn names(&self) -> &RefCell<IndexMap<String, String>> {
        &self.names
    }
    fn change_kind(&self, tool: &str) -> Option<&'static OwnKind> {
        self.link().daw.changes.kinds.iter().find(|kind| kind.tool == tool)
    }
    fn action_kind(&self, tool: &str) -> Option<&'static OwnAction> {
        self.link().daw.changes.actions.iter().find(|action| action.tool == tool)
    }
    fn structural(&self, tool: &str) -> bool {
        self.change_kind(tool).is_some_and(|kind| kind.structural)
    }
    fn moves_transport(&self, tool: &str) -> bool {
        self.action_kind(tool).is_some()
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
    async fn act(&self, action: &'static OwnAction, input: JsonObject, signal: Signal) -> ActionOutcome {
        self.one_action(action, input, signal).await
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

/// One action's own tool.
struct ActionTool<S: RefScheme + 'static> {
    engine: Rc<OwnPlan<S>>,
    action: &'static OwnAction,
}

#[async_trait(?Send)]
impl<S: RefScheme + 'static> KernelTool for ActionTool<S> {
    fn name(&self) -> &str {
        self.action.tool
    }
    fn description(&self) -> &str {
        self.action.description
    }
    fn input_schema(&self) -> JsonObject {
        self.action.input_schema()
    }
    async fn execute(&self, input: JsonObject, signal: Signal) -> Result<ToolResult, RuntimeError> {
        signal.check()?;
        let outcome = self.engine.one_action(self.action, input, signal).await;
        if let Some(done) = outcome.done.as_ref().filter(|_| !outcome.is_error) {
            self.engine.emit_action(done);
        }
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
        // Discard is the own DAWs' (their schema offers it); Live's undo_change never reads it.
        let discard = input.get("discard") == Some(&Value::Bool(true));
        undo_change_with(&self.engine.history, &input, signal, discard).await
    }
}

#[cfg(test)]
mod tests {
    use super::super::{logic_changes, reaper_changes, renoise_changes};
    use super::*;
    use regex::Regex;
    use std::collections::BTreeSet;

    /// Live's words, whole: none may reach the model from another DAW's changes.
    const LIVE_WORDS: &str = r"\b(Live|Set|Sets|Ableton|Session|Arrangement|Remote Script|Control Surface|Main|Drum Rack|Operator|[Rr]acks?|[Cc]lips?|[Cc]lip slots?|[Ss]cenes?)\b";

    fn sets() -> [(&'static str, &'static OwnChangeSet); 3] {
        [("REAPER", &reaper_changes::CHANGES), ("Renoise", &renoise_changes::CHANGES), ("Logic Pro", &logic_changes::CHANGES)]
    }

    /// The kinds a bridge's code registers, found with `pattern` in the files under `folder` whose names start with
    /// `prefix`, less the kinds that only name a change's target.
    fn registered(folder: &str, prefix: &str, pattern: &str) -> BTreeSet<String> {
        let folder = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").join(folder);
        let found = Regex::new(pattern).unwrap();
        let mut kinds = BTreeSet::new();
        for entry in std::fs::read_dir(&folder).unwrap() {
            let path = entry.unwrap().path();
            if path.is_file() && path.file_name().unwrap().to_string_lossy().starts_with(prefix) {
                let text = std::fs::read_to_string(&path).unwrap();
                kinds.extend(found.captures_iter(&text).filter_map(|c| c.iter().skip(1).flatten().next().map(|m| m.as_str().to_string())));
            }
        }
        kinds.remove("pattern-track");
        kinds
    }

    #[test]
    fn each_daws_kinds_are_exactly_what_its_bridge_makes() {
        const KIND: &str = r#"kind: "([a-z]+(?:-[a-z]+)+)"|"([a-z]+(?:-[a-z]+)+)" => Some\(Kind"#;
        let reaper = registered("crates/reaper-kumi/src/changes", "", KIND);
        let renoise = registered("daws/renoise/com.kumi.Bridge.xrnx/payload", "changes", r#"(?:kind = |register\()"([a-z]+(?:-[a-z]+)+)""#);
        // Logic's bridge takes its kinds on in the shared brief's order (M3's bridge lane: a changes module in
        // crates/logic-kumi/src, as a file or a folder): once it registers any, Kumi's declaration is pruned to exactly
        // those; until then the declaration is the brief's, and there is nothing to compare it with.
        let logic = [("crates/logic-kumi/src/changes", ""), ("crates/logic-kumi/src", "changes")]
            .into_iter()
            .filter(|(folder, _)| Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").join(folder).is_dir())
            .map(|(folder, prefix)| registered(folder, prefix, KIND))
            .find(|kinds| !kinds.is_empty());
        for ((app, set), bridge) in sets().into_iter().zip([Some(reaper), Some(renoise), logic]) {
            let Some(bridge) = bridge else { continue };
            let ours: BTreeSet<String> = set.kinds.iter().map(|kind| kind.kind.to_string()).collect();
            assert_eq!(ours, bridge, "{app}: Kumi's kinds against its bridge's");
        }
    }

    fn engine<S: RefScheme + Default + 'static>(daw: &'static OwnDaw, file: Option<String>) -> Rc<OwnPlan<S>> {
        let link = Rc::new(Link {
            daw,
            endpoint: Rc::new(RefCell::new(None)),
            book: Rc::new(RefCell::new(Book::default())),
            epoch: Rc::default(),
            layout: Rc::default(),
            seen_in: Rc::default(),
            connected: Rc::default(),
            lifetime: Signal::new(),
            tempo: Rc::default(),
            file: Rc::new(RefCell::new(file)),
            step_titles: RefCell::default(),
        });
        OwnPlan::new(link, None, None, None)
    }

    #[test]
    fn a_delete_copies_the_saved_project_beside_it_once_per_saved_version() {
        let folder = tempfile::tempdir().unwrap();
        let project = folder.path().join("Night Drive.rpp");
        std::fs::write(&project, "<REAPER_PROJECT").unwrap();
        let plan = engine::<super::super::reaper::ReaperRefs>(&super::super::reaper::DAW, Some(project.to_string_lossy().into()));
        let copy = plan.copy_saved().expect("a copy");
        assert!(copy.contains("Kumi copies") && copy.ends_with(".rpp") && copy.contains("Night Drive "), "{copy}");
        assert_eq!(std::fs::read_to_string(&copy).unwrap(), "<REAPER_PROJECT");
        assert_eq!(plan.copy_saved(), None, "the same saved version is copied once");
        std::fs::write(&project, "<REAPER_PROJECT saved again>").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        assert!(plan.copy_saved().is_some(), "a newer save is copied again");
        // Never saved: nothing to copy.
        assert_eq!(engine::<super::super::reaper::ReaperRefs>(&super::super::reaper::DAW, None).copy_saved(), None);
    }

    #[tokio::test]
    async fn renoise_adds_a_few_tracks_an_answer_and_says_so() {
        let plan = engine::<super::super::renoise::RenoiseRefs>(&super::super::renoise::DAW, None);
        let add = plan.change_kind("track_add").unwrap();
        plan.added.set(32);
        let stopped = plan.one_change(add, JsonObject::new(), Signal::new()).await;
        assert!(stopped.stops && stopped.text.contains("32 tracks added in one answer"), "{}", stopped.text);
        plan.new_turn();
        let next = plan.one_change(add, JsonObject::new(), Signal::new()).await;
        assert!(!next.text.contains("tracks added in one answer"), "a new answer starts again: {}", next.text);
    }

    #[test]
    fn an_epoch_renoise_moved_only_for_devices_keeps_the_names_of_what_stayed() {
        use super::super::{own_bridge::follow_epoch, renoise::RenoiseRefs};
        let fresh = || {
            let mut book = Book::<RenoiseRefs>::default();
            for reference in ["1:track:3", "1:device:3.2", "1:seq:1"] {
                book.short_ref(reference);
            }
            (RefCell::new(book), RefCell::new(Some("1".to_string())), Cell::new(Some(4)))
        };
        let named = |book: &RefCell<Book<RenoiseRefs>>| {
            ["track:1", "device:1", "seq:1"].map(|short| book.borrow().lengthen(&json!({"ref": short}))["ref"] != short)
        };
        // Tracks kept: the track's name stays, the device's and the slot's go; the layout goes on as it was.
        let (book, epoch, layout) = fresh();
        follow_epoch(&book, &epoch, &layout, &json!({"epoch": "2", "layout": 9, "kept": {"from": "1", "tracks": true}}));
        assert_eq!((named(&book), epoch.borrow().clone(), layout.get()), ([true, false, false], Some("2".into()), Some(4)));
        // The kinds the tool names as kept stay too.
        let (book, epoch, layout) = fresh();
        follow_epoch(&book, &epoch, &layout, &json!({"epoch": "2", "kept": {"from": "1", "tracks": true, "kinds": ["track", "seq"]}}));
        assert_eq!(named(&book), [true, false, true]);
        // Kept from another epoch than the book's, or not kept: every name goes, and the answer's layout is taken.
        let (book, epoch, layout) = fresh();
        follow_epoch(&book, &epoch, &layout, &json!({"epoch": "3", "layout": 9, "kept": {"from": "2", "tracks": true}}));
        assert_eq!((named(&book), layout.get()), ([false, false, false], Some(9)));
    }

    #[test]
    fn every_kind_has_a_schema_the_model_can_follow_in_the_daws_own_words() {
        let live = Regex::new(LIVE_WORDS).unwrap();
        for (app, set) in sets() {
            for text in [set.plan_description, set.undo_description, set.instructions] {
                assert!(!live.is_match(text), "{app}: {text}");
            }
            let mut tools = BTreeSet::new();
            for kind in set.kinds {
                assert!(tools.insert(kind.tool), "{app}: {} twice", kind.tool);
                assert_eq!(kind.tool.replace('_', "-"), kind.kind, "{app}: the tool is its kind's name");
                assert!(!live.is_match(kind.description), "{app} {}: {}", kind.tool, kind.description);
                let schema = kind.input_schema();
                let properties = schema["properties"].as_object().unwrap_or_else(|| panic!("{app} {}", kind.tool));
                for required in schema.get("required").and_then(Value::as_array).into_iter().flatten() {
                    assert!(properties.contains_key(required.as_str().unwrap()), "{app} {}: {required}", kind.tool);
                }
                // A field that holds a ref is named as refs are, so Kumi shortens and lengthens it; the bridge's other
                // names for them are what `wire` sends.
                for (ours, theirs) in kind.wire {
                    assert!(properties.contains_key(*ours), "{app} {}: {ours}", kind.tool);
                    assert!(ours.ends_with("Ref") || ours.ends_with("Refs"), "{app} {}: {ours}", kind.tool);
                    assert!(!properties.contains_key(*theirs), "{app} {}: {theirs}", kind.tool);
                }
                for unnamed in ["track", "pattern", "group", "asFolderOf"] {
                    assert!(!properties.contains_key(unnamed), "{app} {}: {unnamed} holds a ref", kind.tool);
                }
            }
            assert!(set.kinds.iter().filter(|kind| kind.core).count() <= 8, "{app}: few tools on every request");
        }
    }
}
