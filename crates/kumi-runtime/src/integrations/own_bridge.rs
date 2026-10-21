//! The DAWs whose bridges are Kumi's own (REAPER's extension, Renoise's tool, the bridge beside Logic Pro), through
//! Kumi's host, `daw-mcp-server`: one integration over the host's tools (`status`, `snapshot`, `read`, `focus`,
//! `focus_body`), each DAW giving its words, its refs and its instructions.
use crate::{
    core::{
        context::IntegrationContext,
        contracts::*,
        daw::DawProfile,
        errors::{FailureKind, KumiError, RuntimeError},
        fold::fold_tracks_with,
        place::{is_project_id, unsaved, Place},
        refs::{Book, RefScheme, Shift},
        view::{BodyKind, BodyRequest, DawFocus, FocusBody},
    },
    integrations::{
        none::no_access,
        own_changes::{Ending, Link, OwnPlan},
        own_listening::{ListenHook, OwnListenLink, OwnListening},
        own_project::{OwnProject, Remember, Seen},
    },
    mcp::client::{connect_mcp, BridgeDescriptor, McpEndpoint, Options},
    mcp::types::{CallToolResult, ContentBlock},
};
use async_trait::async_trait;
use chrono::{SecondsFormat, Utc};
use futures::future::LocalBoxFuture;
use kumi_common::{
    abort::{self, Signal, SignalExt},
    js::json::stringify,
    time::perf_now,
};
use regex::Regex;
use serde_json::{json, Value};
use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    path::PathBuf,
    rc::Rc,
    sync::LazyLock,
};

/// How long a call to Kumi's host waits.
pub(super) const CALL_MS: u64 = 30_000;
/// How often FOCUS's focus is read while the app shows it, unless the app says.
const FOCUS_MS: u64 = 150;
/// How often the focus is polled when nothing draws it (plain mode).
const UNDRAWN_FOCUS_MS: u64 = 1_000;
/// The most a page of the model's read asks for.
const MOST_ROWS: u64 = 200;
/// How long after Kumi asked the DAW for another project losing the open one still reads as that switch.
const PROJECT_OPENING_MS: u64 = 120_000;
/// How long `open_place` waits for the bridge to say the project changed before leaving the switch expected.
const PROJECT_EVENT_MS: u64 = 10_000;

/// What one DAW gives the shared integration: its profile, how its host is started, its instructions, and the kinds
/// its read tool takes.
pub struct OwnDaw {
    pub profile: &'static DawProfile,
    pub descriptor: BridgeDescriptor,
    /// What `daw-mcp-server --daw` takes: "reaper", "renoise", "logic".
    pub host_flag: &'static str,
    pub instructions: &'static str,
    /// The model's read tool: its name, what it says and the kinds it takes.
    pub read_tool: &'static str,
    pub read_description: &'static str,
    pub read_kinds: &'static [&'static str],
    /// The project row's word in the read tool ("project", "song").
    pub project_kind: &'static str,
    /// What the producer is looking at, from the host's `focus` (contract §4).
    pub fill_focus: fn(&Value) -> DawFocus,
    /// The bodies FOCUS can draw for this DAW, each as the host's `focus_body` kind with its ref, and read back.
    pub bodies: &'static [BodyKind],
    pub body_request: fn(&BodyRequest) -> Option<JsonObject>,
    pub to_body: fn(&BodyRequest, &Value) -> Option<FocusBody>,
    /// Why the DAW answers late, from its bridge's `busy` code (`modal`, `loading`…), in the DAW's words.
    pub busy_words: fn(&str) -> String,
    /// What the bridge lacks, from its status's `permission` (Logic's `accessibility`), in words that say how to grant
    /// it: a look fails for it, and never counts as busy.
    pub permission_words: fn(&str) -> String,
    /// What Kumi can change in it, and how.
    pub changes: &'static super::own_changes::OwnChangeSet,
    /// Opens a saved project's file with the DAW that's running, for a DAW whose bridge has no method for it (Logic's
    /// `.logicx` package, opened with Logic's own app as a double-click would); None for a DAW Kumi can't open
    /// projects in from here.
    pub open_project: Option<OpenProject>,
    /// (M5) Hears the DAW: builds the listening tools' `Renderer`, and what answers `hear` and `audio_file`, over Kumi's
    /// link to its host (Logic's); None for a DAW Kumi can't hear yet, whose integration offers no listening.
    pub listening: Option<ListenHook>,
    /// How Kumi catches up on the DAW's saved projects and watches the producer work (watch_me): its snapshot of a
    /// project and how two differ, in its words. None for a DAW that has neither yet.
    pub project: Option<&'static OwnProject>,
}

/// Opens a project's file with the DAW that's running (`resource`: the running DAW as the host's status names it,
/// Logic's bundle id), or says why not, in the DAW's words.
pub type OpenProject = fn(Option<String>, String) -> LocalBoxFuture<'static, Result<(), String>>;

/// Starts Kumi's host for a DAW: the production one runs `daw-mcp-server`; a test's stand-in answers in its place.
pub type Connect = Rc<dyn Fn(Signal) -> LocalBoxFuture<'static, Result<Rc<dyn McpEndpoint>, RuntimeError>>>;

/// Opens a project's file with the DAW in the place of the DAW's own opener (a test's stand-in), or says why not.
pub type OpenFile = Rc<dyn Fn(String) -> LocalBoxFuture<'static, Result<(), String>>>;

/// What the app gives a DAW's integration beside the neutral context.
#[derive(Clone)]
pub struct OwnOptions {
    pub context: IntegrationContext,
    /// Kumi's folder, where the host finds the bridge's secret and discovery files.
    pub kumi_home: PathBuf,
    pub connect: Option<Connect>,
    /// What opens a project's file in the DAW, in the place of the DAW's own opener; None for that opener.
    pub open_file: Option<OpenFile>,
}

/// One DAW's integration through Kumi's host.
pub struct OwnIntegration<S: RefScheme + 'static> {
    daw: &'static OwnDaw,
    options: OwnOptions,
    endpoint: Rc<RefCell<Option<Rc<dyn McpEndpoint>>>>,
    book: Rc<RefCell<Book<S>>>,
    /// The epoch the book's refs are from (the latest any answer showed).
    epoch: Rc<RefCell<Option<String>>>,
    connected: Rc<Cell<bool>>,
    closed: Rc<Cell<bool>>,
    watching: Cell<bool>,
    /// The track the producer has selected, as the last focus said: the timeline's page is centred on it.
    selected: Rc<RefCell<Option<String>>>,
    /// The project's identity as last observed: another one in the same tab (or another song) is another project.
    identity: Rc<RefCell<Option<String>>>,
    /// The layout the book's refs are at (Renoise's track shifts, each raising it): every request names it, so the
    /// bridge carries an older ref through the shifts it hasn't seen.
    layout: Rc<Cell<Option<u64>>>,
    /// Looks asked for again since the last one that read the project (a failed look asks for another, a few times).
    looks_again: Rc<Cell<u32>>,
    /// A look failed because the DAW was busy (a dialog): the next is asked for once the focus says it's free.
    look_when_free: Rc<Cell<bool>>,
    /// When the DAW last came back (or first answered): a DAW starting on its empty song and then loading the file
    /// isn't taken for a switch while it settles.
    back_at: Rc<Cell<Option<tokio::time::Instant>>>,
    /// The epoch each long ref was last read in: a read names its ref's, so a DAW whose refs hold across projects
    /// (REAPER's GUIDs, which a Save As copy in another tab shares) refuses one read in another project.
    seen_in: Rc<RefCell<HashMap<String, String>>>,
    /// Stops listening to the host's events.
    unlisten: RefCell<Option<Box<dyn Fn()>>>,
    /// The last look that read the project: its key and project, kept by a look that can't (as Live's are), so the
    /// conversation stays with the project while the DAW is busy or away.
    previous: RefCell<Option<(String, Option<ProjectRef>)>>,
    /// Kumi's changes in the DAW: HISTORY, plans and undo through the host's journal.
    changes: Rc<OwnPlan<S>>,
    /// Cancelled when the integration closes: a change under way is let go.
    lifetime: Signal,
    /// The tempo and saved file as last read (a wait in beats; the copy before a delete).
    tempo: Rc<Cell<Option<f64>>>,
    file: Rc<RefCell<Option<String>>>,
    /// Hosts started: one started again has lost its journal, so Kumi's changes before can't be undone through it.
    hosts: Cell<u32>,
    /// The running DAW as the host's status names it (Logic's bundle id): what a project is opened with.
    resource: RefCell<Option<String>>,
    /// Where each saved project seen this run is, by its place: what opens to go back to its conversation.
    paths: RefCell<HashMap<String, String>>,
    /// Kumi asked the DAW for another project, until this instant: the one open going meanwhile is that switch, not
    /// the producer's.
    project_wanted: Rc<Cell<Option<tokio::time::Instant>>>,
    /// The bridge's latest `project` event, for whoever waits on the project changing.
    projects: Rc<tokio::sync::watch::Sender<Option<Value>>>,
    /// A `project` event came since the focus was last read: the next read checks whose project it is, whatever the
    /// epoch says.
    project_changed: Rc<Cell<bool>>,
    /// (M5) The DAW's listening, when its hook gives one: the listening tools and what hears for `hear`.
    listening: Option<OwnListening>,
    /// What Kumi keeps of the saved project open, for catching up and watch_me, for a DAW that has them.
    remember: Option<Rc<Remember>>,
}

pub fn create_own_integration<S: RefScheme + 'static>(daw: &'static OwnDaw, options: OwnOptions) -> Rc<dyn Integration> {
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
        file: Rc::default(),
        step_titles: RefCell::default(),
        outside_daw_undo: RefCell::default(),
        undo_unanswered: RefCell::default(),
        in_plan: RefCell::default(),
        maybe_redone: RefCell::default(),
    });
    let context = &options.context;
    // (the session's project store, from the neutral context: none keeps no baseline)
    let remember = daw.project.map(|project| {
        let store = context.project_store.clone();
        Remember::new(project, daw.profile, store, link.endpoint.clone(), link.connected.clone(), link.lifetime.clone(), context)
    });
    // Kumi's own changes are kept in the baseline soon after, so the next catch-up doesn't tell them as the producer's.
    let on_change = match &remember {
        Some(remember) => {
            let (remember, told) = (Rc::downgrade(remember), context.on_change.clone());
            Some(Rc::new(move |record: ChangeRecord| {
                if let Some(remember) = remember.upgrade() {
                    remember.keep_soon();
                }
                if let Some(told) = &told {
                    told(record);
                }
            }) as Rc<dyn Fn(ChangeRecord)>)
        }
        None => context.on_change.clone(),
    };
    let changes = OwnPlan::new(link.clone(), context.change_timeout_ms, on_change, context.on_action.clone());
    let listening = daw.listening.map(|hook| {
        let reach = Rc::new(OwnListenLink { link: link.clone(), plan: changes.clone(), on_action: context.on_action.clone() });
        OwnListening::new(hook, reach, context)
    });
    Rc::new(OwnIntegration::<S> {
        daw,
        options,
        endpoint: link.endpoint.clone(),
        book: link.book.clone(),
        epoch: link.epoch.clone(),
        connected: link.connected.clone(),
        closed: Rc::new(Cell::new(false)),
        watching: Cell::new(false),
        selected: Rc::default(),
        identity: Rc::default(),
        layout: link.layout.clone(),
        seen_in: link.seen_in.clone(),
        back_at: Rc::default(),
        looks_again: Rc::default(),
        look_when_free: Rc::default(),
        unlisten: RefCell::default(),
        previous: RefCell::default(),
        changes,
        lifetime: link.lifetime.clone(),
        tempo: link.tempo.clone(),
        file: link.file.clone(),
        hosts: Cell::new(0),
        resource: RefCell::default(),
        paths: RefCell::default(),
        project_wanted: Rc::default(),
        projects: Rc::new(tokio::sync::watch::channel(None).0),
        project_changed: Rc::default(),
        listening,
        remember,
    })
}

/// The markers a look shows; more are read.
const MARKERS_SHOWN: usize = 30;
/// How long a DAW that just came back may take to load the file it was opened with (it starts on its empty song).
const SETTLE_MS: u64 = 3_000;
/// A look that ran into the DAW's own change (a file loading, a song switched mid-read) is taken again this often,
/// this far apart.
const LOOK_TRIES: u32 = 20;
const LOOK_GAP_MS: u64 = 250;
/// A look that failed while the DAW was there asks the session for another this often, a second apart.
const LOOKS_AGAIN: u32 = 20;
/// The most a read's page can hold for the model: the kernel cuts a tool's result at 64 KB.
const PAGE_TEXT_BYTES: usize = 60 * 1024;
/// A turn's short ref (`track:3`): the shape of a name this turn gave.
pub(super) static SHORT_REF: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[a-z]+:[0-9]+$").unwrap());

/// A call's arguments (`args`, in order or by name) with this turn's short refs anywhere in them made long: they sit under no
/// ref-named field, so the book's own walk passes them by.
pub(super) fn lengthen_anywhere<S: RefScheme>(book: &Book<S>, value: &Value) -> Value {
    match value {
        Value::String(text) if SHORT_REF.is_match(text) => book.lengthen(&json!({"ref": text}))["ref"].clone(),
        Value::Array(items) => Value::Array(items.iter().map(|item| lengthen_anywhere(book, item)).collect()),
        Value::Object(fields) => Value::Object(fields.iter().map(|(key, item)| (key.clone(), lengthen_anywhere(book, item))).collect()),
        other => other.clone(),
    }
}

/// What a call answered (`result`, `out`) with the DAW's refs anywhere in it made this turn's short ones.
pub(super) fn shorten_anywhere<S: RefScheme>(book: &mut Book<S>, value: &Value) -> Value {
    match value {
        Value::String(text) if book.scheme.kind_of(text).is_some() => json!(book.short_ref(text)),
        Value::Array(items) => Value::Array(items.iter().map(|item| shorten_anywhere(book, item)).collect()),
        Value::Object(fields) => Value::Object(fields.iter().map(|(key, item)| (key.clone(), shorten_anywhere(book, item))).collect()),
        other => other.clone(),
    }
}

/// Why a host tool answered an error: its words, and whether the DAW is there at all (it quit, or its bridge did).
pub(super) struct Refused {
    pub(super) why: String,
    pub(super) there: bool,
    /// Busy for now (a dialog, a load, a render): worth waiting for.
    pub(super) busy: bool,
    /// The host's code for it (`stale`, `changed`, `failed`…).
    pub(super) code: Option<String>,
    /// The DAW answered, and couldn't say how it ended (an undo it answered `uncertain`), rather than not answering.
    pub(super) answered: bool,
}

/// A host call's outcome, with a failure that came of the call's own signal (the caller stopped, or its time ran out) as
/// Aborted: the MCP client reports both as plain errors, and neither means the host is gone.
pub(super) fn stopped_or<T>(outcome: Result<T, RuntimeError>, signal: &Signal) -> Result<T, RuntimeError> {
    match outcome {
        Err(_) if signal.is_cancelled() => Err(RuntimeError::Aborted),
        other => other,
    }
}

/// The JSON a host tool answered with, or why it couldn't.
pub(super) fn answer_of(result: &CallToolResult) -> Result<Value, Refused> {
    let text = result
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    let value: Value = serde_json::from_str(&text).unwrap_or(Value::String(text.clone()));
    if result.is_error == Some(true) {
        return Err(Refused {
            why: value.get("error").and_then(Value::as_str).map(str::to_owned).unwrap_or(text),
            there: value.get("connected") != Some(&Value::Bool(false)),
            // The bridge's busy, or the host's own word that the DAW didn't answer in time (a dialog may be open).
            busy: value.get("code").and_then(Value::as_str) == Some("busy") || value.get("busy") == Some(&Value::Bool(true)),
            code: value.get("code").and_then(Value::as_str).map(str::to_owned),
            answered: value.get("answered") == Some(&Value::Bool(true)),
        });
    }
    Ok(value)
}

/// Why the DAW may answer late, in words, for a DAW whose `busy` codes are REAPER's (`modal`, `loading`, `rendering`).
pub fn busy_words(code: &str, profile: &DawProfile) -> String {
    match code {
        "modal" => "a dialog is open".into(),
        "loading" => format!("a {} is loading", profile.words.place),
        "rendering" => "it's rendering".into(),
        "working" => "it's still running a long request (a plug-in loading, or a dialog that request opened)".into(),
        other => other.into(),
    }
}

/// What a permission the bridge lacks means, for a DAW whose bridge runs inside it and lacks none.
pub fn permission_words(permission: &str, profile: &DawProfile) -> String {
    format!("Kumi's bridge in {} lacks the {permission} permission", profile.words.app)
}

impl<S: RefScheme + 'static> OwnIntegration<S> {
    fn set_connected(&self, connected: bool) {
        if self.connected.replace(connected) != connected || !connected {
            let state = if connected { ConnectionState::Connected } else { ConnectionState::Disconnected };
            (self.options.context.on_connection)(state, None);
        }
    }

    async fn endpoint(&self, signal: Signal) -> Result<Rc<dyn McpEndpoint>, RuntimeError> {
        if let Some(endpoint) = self.endpoint.borrow().clone() {
            return Ok(endpoint);
        }
        let endpoint = match &self.options.connect {
            Some(connect) => connect(signal).await?,
            None => {
                let args = vec![
                    "--daw".into(),
                    self.daw.host_flag.into(),
                    "--kumi-home".into(),
                    self.options.kumi_home.to_string_lossy().into_owned(),
                ];
                connect_mcp(Options { signal, args, timeout_ms: Some(CALL_MS), bridge: Some(self.daw.descriptor), ..Default::default() })
                    .await?
            }
        };
        *self.endpoint.borrow_mut() = Some(endpoint.clone());
        // A host started again has a new journal: Kumi's changes before it can't be undone through it.
        if self.hosts.replace(self.hosts.get() + 1) > 0 {
            self.changes.retire_all(Ending::Restart);
        }
        // Tracks inserted, removed or moved where refs are positions: the book's refs follow what they named.
        let (book, layout, epoch) = (self.book.clone(), self.layout.clone(), self.epoch.clone());
        let changes = Rc::downgrade(&self.changes);
        let (file, projects, project_changed) = (self.file.clone(), self.projects.clone(), self.project_changed.clone());
        let unlisten = endpoint.on_live_event(Rc::new(move |event| {
            // The producer undid (or redid) Kumi's changes with the DAW's own undo: HISTORY follows.
            if event.get("name").and_then(Value::as_str) == Some("undone_in_daw") {
                if let (Some(changes), Some(data)) = (changes.upgrade(), event.get("data")) {
                    changes.undone_in_daw(data);
                }
                return;
            }
            // The project changed (another one opened, the open one saved under a name): whoever waits on it is told,
            // and the next focus read checks whose project it is now.
            if event.get("name").and_then(Value::as_str) == Some("project") {
                let data = event.get("data").cloned().unwrap_or(Value::Null);
                *file.borrow_mut() = data.get("path").and_then(Value::as_str).filter(|path| !path.is_empty()).map(str::to_owned);
                project_changed.set(true);
                projects.send_replace(Some(data));
                return;
            }
            // Events were dropped on the way: a track shift among them is unknown, so positional refs are read again.
            if event.get("name").and_then(Value::as_str) == Some("events_lost") {
                if book.borrow().scheme.positional() {
                    let mut book = book.borrow_mut();
                    book.clear_names();
                    book.cursors.clear();
                    layout.set(None);
                }
                return;
            }
            if event.get("name").and_then(Value::as_str) != Some("tracks") {
                return;
            }
            let data = event.get("data").cloned().unwrap_or(Value::Null);
            let places = |key: &str| -> Vec<usize> {
                data.get(key).and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_u64).map(|n| n as usize).collect()
            };
            // Another epoch (a swap, notifications that don't add up): the names go and the event's layout is taken.
            if data.get("epoch").and_then(Value::as_str).is_some_and(|now| epoch.borrow().as_deref().is_some_and(|before| before != now)) {
                follow_epoch(&book, &epoch, &layout, &data);
                return;
            }
            let Some(now) = data.get("layout").and_then(Value::as_u64) else { return };
            match layout.get() {
                // The next shift: the names follow it.
                Some(before) if now == before + 1 => {
                    book.borrow_mut().shift(&Shift { tracks_made: places("made"), tracks_gone: places("gone"), ..Shift::default() });
                    layout.set(Some(now));
                }
                // One the book is already past (taken from an answer, or after events were lost): nothing to move.
                Some(before) if now <= before => {}
                // A gap: shifts in between are unknown, so the names go.
                Some(_) => {
                    if book.borrow().scheme.positional() {
                        let mut book = book.borrow_mut();
                        book.clear_names();
                        book.cursors.clear();
                    }
                    layout.set(Some(now));
                }
                // No layout yet (names retired): this one is where the book is.
                None => layout.set(Some(now)),
            }
        }));
        // The host went away (it exited, or its pipe closed): the next call starts another.
        let (held, gone) = (self.endpoint.clone(), Rc::downgrade(&endpoint));
        let undisconnect = endpoint.on_disconnect(Rc::new(move || {
            let same = held.borrow().as_ref().zip(gone.upgrade()).is_some_and(|(now, it)| Rc::ptr_eq(now, &it));
            if same {
                held.borrow_mut().take();
            }
        }));
        let unlisten: Box<dyn Fn()> = Box::new(move || {
            unlisten();
            undisconnect();
        });
        if let Some(before) = self.unlisten.replace(Some(unlisten)) {
            before();
        }
        Ok(endpoint)
    }

    /// A host tool's answer: Ok with its JSON, Err with why (the host's words) when it answered an error.
    /// The host stays through a failed call (the caller's stop, a time limit): only its going away (`on_disconnect`) lets
    /// the next call start another, whose journal is empty.
    async fn call(&self, tool: &str, args: Value, signal: Signal) -> Result<Result<Value, Refused>, RuntimeError> {
        let endpoint = self.endpoint(signal.clone()).await?;
        let args = args.as_object().cloned().unwrap_or_default();
        let bounded = abort::any([signal, abort::timeout(CALL_MS)]);
        stopped_or(endpoint.call(tool, args, bounded.clone()).await.map(|result| answer_of(&result)), &bounded)
    }

    /// Refs a new epoch made stale go: a positional DAW's refs carry their epoch; a stable DAW's never change.
    fn see_epoch(&self, epoch: Option<&str>) {
        let Some(epoch) = epoch else { return };
        let changed = self.epoch.borrow().as_deref().is_some_and(|before| before != epoch);
        if changed {
            self.layout.set(None);
            let mut book = self.book.borrow_mut();
            // Refs that hold through anything (REAPER's GUIDs) keep their names; positions retire with their epoch.
            if !book.scheme.positional() {
                book.cursors.clear();
                drop(book);
                *self.epoch.borrow_mut() = Some(epoch.to_owned());
                return;
            }
            book.refs.clear();
            book.known.clear();
            book.cursors.clear();
            book.clear_names();
        }
        *self.epoch.borrow_mut() = Some(epoch.to_owned());
        // What every project has, which no read lists (Logic's Stereo Out), is named in this epoch.
        self.book.borrow_mut().stand(epoch);
    }

    /// Reads the host's focus while the app shows FOCUS, telling the app what changed: what the producer looks at and
    /// the transport (for the beat light, every read while it plays).
    fn watch_focus(&self) {
        let (on_focus, on_transport) = (self.options.context.on_focus.clone(), self.options.context.on_transport.clone());
        if self.watching.replace(true) || (on_focus.is_none() && on_transport.is_none()) {
            return;
        }
        let (endpoint, closed, daw, selected) = (self.endpoint.clone(), self.closed.clone(), self.daw, self.selected.clone());
        let (connected, identity, on_connection) =
            (self.connected.clone(), self.identity.clone(), self.options.context.on_connection.clone());
        let back_at = self.back_at.clone();
        let (book, layout, book_epoch) = (self.book.clone(), self.layout.clone(), self.epoch.clone());
        let (look_when_free, look_again) = (self.look_when_free.clone(), self.options.context.on_look_again.clone());
        let (project_changed, project_wanted) = (self.project_changed.clone(), self.project_wanted.clone());
        let drawn = if self.options.context.focus_drawn { FOCUS_MS } else { UNDRAWN_FOCUS_MS };
        let period = self.options.context.focus_interval_ms.unwrap_or(drawn).max(50);
        tokio::task::spawn_local(async move {
            let (mut last, mut last_transport, mut last_epoch): (Option<Value>, Option<Value>, Option<String>) = (None, None, None);
            let lose_focus = |last: &mut Option<Value>| {
                if last.take().is_some() {
                    if let Some(on_focus) = &on_focus {
                        on_focus(None);
                    }
                }
            };
            while !closed.get() {
                tokio::time::sleep(std::time::Duration::from_millis(period)).await;
                let Some(host) = endpoint.borrow().clone() else { continue };
                let read = host.call("focus", JsonObject::new(), abort::timeout(2_000)).await.ok().map(|result| answer_of(&result));
                if closed.get() {
                    break;
                }
                let focus = match read {
                    Some(Ok(focus)) if focus.is_object() => focus,
                    // The DAW went away (it quit, or its bridge did): the session says so, and FOCUS empties with it.
                    Some(Err(Refused { there: false, .. })) => {
                        if connected.replace(false) {
                            on_connection(ConnectionState::Disconnected, Some(DisconnectCause::Live));
                        }
                        // A DAW started again is another process: its project's identity is found afresh.
                        identity.borrow_mut().take();
                        last_epoch = None;
                        lose_focus(&mut last);
                        continue;
                    }
                    _ => {
                        lose_focus(&mut last);
                        continue;
                    }
                };
                // Back after it went away: the session looks again, once the DAW has loaded what it opened.
                if !connected.replace(true) {
                    back_at.set(Some(tokio::time::Instant::now()));
                    on_connection(ConnectionState::Connected, None);
                }
                // Another project in the same tab, or another song: the epoch moved, and the project's identity with it.
                // An epoch no event told of (a device or pattern edit in Renoise): the book follows it.
                follow_epoch(&book, &book_epoch, &layout, &focus);
                // Free again after a look failed because it was busy: the session looks again now.
                if focus.get("busy").is_none_or(Value::is_null) && look_when_free.replace(false) {
                    if let Some(look_again) = &look_again {
                        look_again();
                    }
                }
                let epoch = focus.get("epoch").and_then(Value::as_str).map(str::to_owned);
                // (a project event before the first read is kept for the read that can compare)
                let announced = last_epoch.is_some() && project_changed.replace(false);
                if last_epoch.is_some() && (epoch != last_epoch || announced) {
                    let mut args = JsonObject::new();
                    args.insert("kind".into(), daw.project_kind.into());
                    let row = host.call("read", args, abort::timeout(2_000)).await.ok().and_then(|result| answer_of(&result).ok());
                    let now = row.as_ref().and_then(|row| row.pointer("/items/0/identity")).and_then(Value::as_str).map(str::to_owned);
                    let before = identity.borrow().clone();
                    if now.is_some() && before.is_some() && now != before {
                        *identity.borrow_mut() = now;
                        // The switch Kumi asked for (a kept conversation's project, opened again), or the producer's.
                        let asked = project_wanted.take().is_some_and(|until| tokio::time::Instant::now() <= until);
                        let cause = if asked { DisconnectCause::AskedSet } else { DisconnectCause::Set };
                        on_connection(ConnectionState::Disconnected, Some(cause));
                        on_connection(ConnectionState::Connected, None);
                    }
                }
                last_epoch = epoch;
                *selected.borrow_mut() = focus.pointer("/selection/trackRef").and_then(Value::as_str).map(str::to_owned);
                let seen = json!({"view": focus.get("view"), "selection": focus.get("selection"), "epoch": focus.get("epoch"), "busy": focus.get("busy")});
                if last.as_ref() != Some(&seen) {
                    if let Some(on_focus) = &on_focus {
                        let mut filled = (daw.fill_focus)(&focus);
                        filled.busy = focus.get("busy").and_then(Value::as_str).map(|code| (daw.busy_words)(code));
                        on_focus(Some(filled));
                    }
                    last = Some(seen);
                }
                let transport = focus.get("transport").cloned();
                let playing = transport.as_ref().and_then(|t| t.get("playing")) == Some(&Value::Bool(true));
                if playing || transport != last_transport {
                    if let Some(on_transport) = &on_transport {
                        on_transport(crate::integrations::own_views::transport(&focus, perf_now()));
                    }
                    last_transport = transport;
                }
            }
        });
    }

    /// The project's snapshot: a read the DAW's own change ran into (a file loading, a song switched mid-read) is taken
    /// again, a moment later.
    async fn look(&self, signal: &Signal) -> Result<Result<Value, Refused>, RuntimeError> {
        for _ in 0..LOOK_TRIES {
            match self.call("snapshot", json!({}), signal.clone()).await? {
                Err(refused) if refused.there && matches!(refused.code.as_deref(), Some("epoch-changed" | "changed")) => {
                    signal.check()?;
                    tokio::time::sleep(std::time::Duration::from_millis(LOOK_GAP_MS)).await;
                }
                other => return Ok(other),
            }
        }
        self.call("snapshot", json!({}), signal.clone()).await
    }

    /// A look that failed while the DAW is there (busy, loading, changing): the project and the DAW's tools stay, so the
    /// conversation does; and Kumi looks again by itself, since no turn may come to.
    fn unread(&self, why: &str, busy: bool) -> Observation {
        let mut observation = self.no_access(None);
        let app = self.daw.profile.words.app;
        observation.revision = None;
        observation.label = format!("{app} — couldn't look just now: {why}");
        observation.tools = self.tools();
        observation.instructions = self.instructions();
        observation.context = stringify(&json!({
            "observedAt": Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true),
            "daw": app,
            "look": format!("{app} couldn't be read just now ({why}); Kumi looks again, and {} reads it meanwhile.", self.daw.read_tool),
        }));
        // Busy (a dialog): asked for again once the focus says it's free, not every second meanwhile.
        if busy && self.watching.get() {
            self.look_when_free.set(true);
        } else {
            self.look_again_later();
        }
        observation
    }

    /// A look that failed while the DAW is there: for what its bridge lacks (Logic's Accessibility), said in words that
    /// say how to grant it, else for the reason the bridge gave.
    async fn unread_for(&self, refused: Refused, signal: &Signal) -> Observation {
        let lacking = if refused.busy { None } else { self.lacking(signal).await };
        self.unread(lacking.as_deref().unwrap_or(&refused.why), refused.busy)
    }

    /// What the bridge lacks, as its status says (`permission`: Logic's `accessibility`), in the DAW's words; None when
    /// it lacks nothing, or the status can't be read.
    async fn lacking(&self, signal: &Signal) -> Option<String> {
        let status = self.call("status", json!({}), signal.clone()).await.ok()?.ok()?;
        let permission = status.get("permission").and_then(Value::as_str).filter(|permission| !permission.is_empty())?;
        Some((self.daw.permission_words)(permission))
    }

    /// Asks the session for another look a second from now (a few times at most since the last good one).
    fn look_again_later(&self) {
        let Some(look_again) = self.options.context.on_look_again.clone() else { return };
        if self.looks_again.get() >= LOOKS_AGAIN {
            return;
        }
        self.looks_again.set(self.looks_again.get() + 1);
        let (closed, connected) = (self.closed.clone(), self.connected.clone());
        tokio::task::spawn_local(async move {
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            if !closed.get() && connected.get() {
                look_again();
            }
        });
    }

    /// The model's tools: the read, and the DAW's changes with their plan and undo; (M5) the listening tools, for a DAW
    /// Kumi hears.
    fn tools(&self) -> Vec<Rc<dyn KernelTool>> {
        let mut tools = vec![self.read_tool()];
        tools.extend(self.changes.tools());
        if let Some(listening) = &self.listening {
            tools.extend(listening.tools());
        }
        if let Some(remember) = &self.remember {
            tools.push(remember.watch_tool());
        }
        tools
    }

    fn instructions(&self) -> String {
        match self.daw.changes.instructions {
            "" => self.daw.instructions.to_string(),
            changes => format!("{} {}", self.daw.instructions, changes),
        }
    }

    fn read_tool(&self) -> Rc<dyn KernelTool> {
        Rc::new(ReadTool::<S> {
            daw: self.daw,
            endpoint: self.endpoint.borrow().clone(),
            book: self.book.clone(),
            layout: self.layout.clone(),
            epoch: self.epoch.clone(),
            seen_in: self.seen_in.clone(),
            description: read_description(self.daw, &self.book.borrow().scheme),
        })
    }

    /// The snapshot once a DAW that just came back has loaded what it opened: it starts on an empty unsaved song and
    /// loads the file a moment later, and that first song is no switch. A song still unsaved after a while is one.
    async fn settled(&self, mut snapshot: Value, signal: &Signal) -> Result<Value, RuntimeError> {
        let unsaved = |snapshot: &Value| snapshot.pointer("/project/path").and_then(Value::as_str).is_none_or(str::is_empty);
        let Some(back) = self.back_at.get() else { return Ok(snapshot) };
        while unsaved(&snapshot) && back.elapsed() < std::time::Duration::from_millis(SETTLE_MS) {
            signal.check()?;
            tokio::time::sleep(std::time::Duration::from_millis(LOOK_GAP_MS)).await;
            match self.look(signal).await {
                Ok(Ok(now)) => snapshot = now,
                Err(error) if error.is_aborted() => return Err(error),
                _ => break,
            }
        }
        self.back_at.set(None);
        Ok(snapshot)
    }

    fn no_access(&self, why: Option<&str>) -> Observation {
        // The last project read keeps its key: a busy or quit DAW isn't another project.
        let (key, project) = self.previous.borrow().clone().unwrap_or_else(|| (unsaved(self.id()).into(), None));
        let mut observation = no_access(self.daw.profile, self.daw.instructions, &key, Utc::now(), project);
        if let Some(why) = why {
            observation.label = format!("{} — {why}", observation.label);
        }
        observation
    }

    fn id(&self) -> crate::core::daw::DawId {
        self.daw.profile.id.expect("a DAW with a bridge of Kumi's own is known")
    }

    /// The observation from the host's snapshot.
    fn observation(&self, snapshot: &Value, pinned: Option<&PinnedNode>) -> Observation {
        // (the DAW went away since the look before, and is back: its project's identity went with it)
        let came_back = self.identity.borrow().is_none();
        self.see_epoch(snapshot.get("epoch").and_then(Value::as_str));
        note_epoch(&self.seen_in, &self.book.borrow().scheme, snapshot);
        if self.layout.get().is_none() {
            self.layout.set(snapshot.get("layout").and_then(Value::as_u64));
        }
        let profile = self.daw.profile;
        let project = snapshot.get("project").cloned().unwrap_or(Value::Null);
        // The project's name: as the bridge says it, else its file's (Logic's row names the package's path), else the
        // identity an unsaved one carries (Logic's window title).
        let name = project
            .get("name")
            .and_then(Value::as_str)
            .filter(|name| !name.is_empty())
            .map(str::to_owned)
            .or_else(|| {
                let path = project.get("path").and_then(Value::as_str).filter(|path| !path.is_empty())?;
                let file = path.rsplit(['/', '\\']).next()?;
                Some(file.rsplit_once('.').map_or(file, |(stem, _)| stem).to_owned()).filter(|stem| !stem.is_empty())
            })
            .or_else(|| project.get("identity").and_then(Value::as_str).filter(|identity| !identity.is_empty()).map(str::to_owned))
            .unwrap_or_else(|| "untitled".to_owned());
        let saved = project.get("id").and_then(Value::as_str).filter(|id| is_project_id(id));
        let place = match saved {
            Some(id) => Place::Saved { daw: self.id(), id: id.into() }.text(),
            None => unsaved(self.id()).into(),
        };
        // Where the project is, for a kept conversation of its place to open it again.
        if let (Some(_), Some(file)) = (saved, project.get("path").and_then(Value::as_str).filter(|path| !path.is_empty())) {
            self.paths.borrow_mut().insert(place.clone(), file.to_owned());
        }
        let mut book = self.book.borrow_mut();
        let rows = snapshot.get("tracks").and_then(Value::as_array).into_iter().flatten();
        // Each track is named in the project's order before any ref in a row (a group's, a send's) names one out of it.
        for reference in rows.clone().filter_map(|row| row.get("ref").and_then(Value::as_str)) {
            book.short_ref(reference);
        }
        let tracks: Vec<JsonObject> = rows.filter_map(|row| book.shorten(row).as_object().cloned()).collect();
        let selected: Vec<String> =
            snapshot.pointer("/selection/trackRef").and_then(Value::as_str).map(|track| book.short_ref(track)).into_iter().collect();
        let selection = book.shorten(snapshot.get("selection").unwrap_or(&Value::Null));
        let markers = book.shorten(snapshot.get("markers").unwrap_or(&Value::Null));
        // The strips every project has that no read lists (Logic's Stereo Out), with their refs.
        let outputs: Vec<Value> = snapshot
            .get("epoch")
            .and_then(Value::as_str)
            .map(|epoch| book.stand(epoch))
            .unwrap_or_default()
            .into_iter()
            .map(|(reference, name)| json!({"ref": reference, "name": name}))
            .collect();
        drop(book);
        let note = format!("Some tracks are folded to fit; {} reads them whole.", self.daw.read_tool);
        let read_tool = self.daw.read_tool;
        let folded = fold_tracks_with(
            &tracks,
            |track| track.get("ref").and_then(Value::as_str).is_some_and(|reference| selected.iter().any(|s| s == reference)),
            None,
            &note,
            |left| format!("{left} more tracks aren't listed; {read_tool} kind track (with cursor) reads them"),
        );
        let mut context = json!({
            "observedAt": Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true),
            "daw": profile.words.app,
        });
        context[profile.words.place] = shown_project(&project, snapshot.get("transport"));
        context["transport"] = snapshot.get("transport").cloned().unwrap_or(Value::Null);
        context["tracks"] = json!(folded.tracks);
        if !outputs.is_empty() {
            context["outputs"] = json!(outputs);
        }
        context["selection"] = selection;
        if let Some(folded_note) = folded.folded {
            context["folded"] = json!(folded_note);
        }
        if let Some(more) = folded.more_tracks {
            context["moreTracks"] = json!(more);
        }
        // A project bigger than the snapshot (REAPER names the total; Renoise says only that there are more): the model
        // is told, and how to read the rest.
        let listed = context["tracks"].as_array().map_or(0, Vec::len);
        let total = snapshot.pointer("/truncated/tracks/total").and_then(Value::as_u64);
        if let Some(total) = total.filter(|total| *total as usize > listed) {
            context["moreTracks"] = json!(format!(
                "{} tracks in all, {listed} listed here; {read_tool} kind track pages through every one",
                crate::notation::thousands(total as usize)
            ));
        } else if snapshot.get("moreTracks") == Some(&Value::Bool(true)) {
            context["moreTracks"] = json!(format!("Not every track is listed here; {read_tool} kind track pages through every one"));
        }
        // Markers within the look's budget: the rest are read.
        if let Some(all) = markers.as_array() {
            if all.len() > MARKERS_SHOWN {
                context["markers"] = json!(all[..MARKERS_SHOWN]);
                context["moreMarkers"] =
                    json!(format!("{} more markers; {read_tool} kind marker pages through every one", all.len() - MARKERS_SHOWN));
            } else {
                context["markers"] = markers;
            }
        } else if !markers.is_null() {
            context["markers"] = markers;
        }
        // A saved project opened or switched to is caught up on, and what changed since Kumi last saw it is told here.
        if let Some(remember) = &self.remember {
            let playing =
                ["playing", "recording"].iter().any(|key| snapshot.pointer(&format!("/transport/{key}")) == Some(&Value::Bool(true)));
            let path = project.get("path").and_then(Value::as_str);
            remember.seen(Seen { id: saved, path, name: &name, came_back, playing });
            if let Some(since) = remember.context() {
                context["sinceLastTime"] = Value::Object(since);
            }
        }
        if let Some(latest) = self.changes.latest() {
            context["kumiChanges"] = latest;
        }
        // What the producer pinned in FOCUS (an FX, a DSP device), in the turn's short refs.
        if let Some(pin) = pinned {
            context["pinned"] = self.book.borrow_mut().shorten(&serde_json::to_value(pin).unwrap_or(Value::Null));
        }
        let identity = project.get("identity").and_then(Value::as_str).map(str::to_owned);
        *self.identity.borrow_mut() = identity.clone();
        let tempo = ["/transport/tempo", "/transport/bpm", "/project/tempo", "/project/bpm"]
            .iter()
            .find_map(|at| snapshot.pointer(at).and_then(Value::as_f64));
        self.tempo.set(tempo);
        // What the listening keeps between looks (the meter with the tempo).
        if let Some(listening) = &self.listening {
            listening.hearing.seen(snapshot);
        }
        *self.file.borrow_mut() = project.get("path").and_then(Value::as_str).filter(|path| !path.is_empty()).map(str::to_owned);
        // A saved project is its id, through a restart of the DAW; an unsaved one is this run's identity for it.
        let key = saved
            .map(|id| format!("{}:{id}", self.daw.host_flag))
            .or_else(|| identity.clone().map(|identity| format!("{}:{identity}", self.daw.host_flag)))
            .unwrap_or_else(|| place.clone());
        *self.previous.borrow_mut() = Some((key.clone(), saved.map(|_| ProjectRef { id: place.clone(), name: name.clone() })));
        // KUMI_TRACE=1: which project each look took the DAW to be, so a restart's key can be checked without a turn.
        if let Some(trace) = &self.options.context.on_dispatch {
            trace(format!("look: {key}"));
        }
        self.looks_again.set(0);
        Observation {
            key: key.clone(),
            revision: None,
            label: format!("{} — {name}", profile.words.app),
            context: stringify(&context),
            instructions: self.instructions(),
            tools: self.tools(),
            project: saved.map(|_| ProjectRef { id: place, name: name.clone() }),
            set: identity.clone(),
            tracks: Some(tracks.iter().filter_map(|track| track.get("name").and_then(Value::as_str).map(str::to_owned)).collect()),
            saved_at: None,
            // The project as the DAW names it, for the app to show (saved or not).
            identity: Some(ProjectIdentity {
                daw: self.id(),
                id: saved.map(str::to_owned),
                name,
                file: project.get("path").and_then(Value::as_str).filter(|path| !path.is_empty()).map(str::to_owned),
                saved_at: None,
                open_identity: identity,
            }),
        }
    }
}

#[async_trait(?Send)]
impl<S: RefScheme + 'static> Integration for OwnIntegration<S> {
    fn profile(&self) -> &DawProfile {
        self.daw.profile
    }
    async fn start(&self, signal: Signal) -> Result<(), RuntimeError> {
        signal.check()?;
        if self.closed.get() {
            return Err(RuntimeError::plain("Integration is closed"));
        }
        (self.options.context.on_connection)(ConnectionState::Connecting, None);
        let status = self.call("status", json!({}), signal).await?;
        if let Ok(status) = &status {
            // The running DAW, and the project it has open, as the host names them.
            *self.resource.borrow_mut() = status.get("resource").and_then(Value::as_str).filter(|r| !r.is_empty()).map(str::to_owned);
            if let Some(path) = status.pointer("/project/path").and_then(Value::as_str).filter(|path| !path.is_empty()) {
                *self.file.borrow_mut() = Some(path.to_owned());
            }
        }
        self.set_connected(status.is_ok_and(|status| status.get("connected") != Some(&Value::Bool(false))));
        self.watch_focus();
        Ok(())
    }
    /// The saved project the DAW has open, by its file: from the last look, the status that started the session, or
    /// the bridge's `project` event since. None for one not saved yet.
    fn open_set(&self) -> Option<String> {
        self.file.borrow().clone()
    }
    /// Opens the saved project a kept conversation belongs to, where this run last saw it, with the DAW's own opener
    /// (Logic's `open`): the DAW asks about the project open itself. Losing that project meanwhile is the switch Kumi
    /// asked for (`AskedSet`), and the bridge's `project` event says when the other one is open: waited for a while,
    /// since the conversation goes on only then; past it (a dialog about the project open, say) the switch stays
    /// expected. `Err(why)` inside when it can't: a project this run never saw, or one the DAW didn't take.
    async fn open_place(&self, place: &str, signal: Signal) -> Result<Result<(), String>, RuntimeError> {
        let words = self.daw.profile.words;
        if self.closed.get() || !self.connected.get() {
            return Ok(Err(format!("Kumi isn't connected to {}.", words.app)));
        }
        let Some(path) = self.paths.borrow().get(place).cloned() else {
            return Ok(Err(format!("Kumi doesn't know where that {} is.", words.place)));
        };
        if self.options.open_file.is_none() && self.daw.open_project.is_none() {
            return Ok(Err(format!("Kumi can't open {} in {} from here.", words.places, words.app)));
        }
        self.project_wanted.set(Some(tokio::time::Instant::now() + std::time::Duration::from_millis(PROJECT_OPENING_MS)));
        // (subscribed before the file is opened: an event during the opening isn't missed)
        let mut events = self.projects.subscribe();
        let opening = match (&self.options.open_file, self.daw.open_project) {
            (Some(open), _) => open(path.clone()),
            (None, Some(open)) => open(self.resource.borrow().clone(), path.clone()),
            (None, None) => unreachable!("checked above"),
        };
        if let Err(why) = opening.await {
            self.project_wanted.set(None);
            return Ok(Err(why));
        }
        let waiting = abort::any([signal, abort::timeout(PROJECT_EVENT_MS)]);
        loop {
            tokio::select! {
                changed = events.changed() => {
                    if changed.is_err() {
                        break;
                    }
                    let opened = events.borrow().as_ref().and_then(|project| project.get("path")).and_then(Value::as_str) == Some(path.as_str());
                    if opened {
                        break;
                    }
                }
                _ = waiting.cancelled() => break,
            }
        }
        Ok(Ok(()))
    }
    /// Whether the DAW records now, as its focus's transport says (Logic's and REAPER's carry `recording`); None when
    /// it isn't there, its transport doesn't say (Renoise's), or it can't be asked (a bridge lacking Accessibility
    /// answers in words, never busy).
    async fn recording(&self, signal: Signal) -> Result<Option<bool>, RuntimeError> {
        if self.closed.get() || !self.connected.get() {
            return Ok(None);
        }
        Ok(match self.call("focus", json!({}), signal).await? {
            Ok(focus) => focus.pointer("/transport/recording").and_then(Value::as_bool),
            Err(_) => None,
        })
    }
    fn can_show(&self, kind: BodyKind) -> bool {
        self.daw.bodies.contains(&kind)
    }
    fn has_undo(&self) -> bool {
        true
    }
    async fn undo(&self, id: Option<&str>, signal: Signal) -> Result<ChangeRecord, RuntimeError> {
        if self.closed.get() || !self.connected.get() {
            let app = self.daw.profile.words.app;
            return Err(KumiError::new(FailureKind::Request, format!("Kumi isn't connected to {app}, so it can't undo.")).into());
        }
        let outcome = self.changes.history.undo(id.unwrap_or("last"), signal, false).await?;
        outcome.record.ok_or_else(|| KumiError::new(FailureKind::Request, outcome.text).into())
    }
    fn has_stop_live(&self) -> bool {
        true
    }
    async fn stop_live(&self, signal: Signal) -> Result<bool, RuntimeError> {
        Ok(self.changes.history.stop_everything(signal).await)
    }
    /// (M5) The producer's words came into the answer under way: a judged run's changes from here may be theirs.
    fn steered(&self) {
        if let Some(listening) = &self.listening {
            listening.steered();
        }
    }
    /// (M5) Hearing the project, auditioning and a region's audio file: through the DAW's listening, for a DAW Kumi hears
    /// (and whose profile says it can); none for the others.
    fn has_hear(&self) -> bool {
        self.listening.is_some() && self.daw.profile.caps.hear
    }
    async fn hear(&self, request: &HearRequest, signal: Signal) -> Result<Result<Vec<HeardTake>, String>, RuntimeError> {
        match self.listening.as_ref().filter(|_| self.has_hear()) {
            Some(listening) => listening.hearing.hear(request, signal).await,
            None => Err(RuntimeError::plain("Not available")),
        }
    }
    fn has_audition(&self) -> bool {
        self.listening.is_some() && self.daw.profile.caps.audition
    }
    async fn audition(&self, request: &AuditionRequest, signal: Signal) -> Result<Result<AuditionResult, String>, RuntimeError> {
        match self.listening.as_ref().filter(|_| self.has_audition()) {
            Some(listening) => listening.engine.audition(request, signal).await,
            None => Err(RuntimeError::plain("Not available")),
        }
    }
    fn has_audio_file(&self) -> bool {
        self.listening.is_some() && self.daw.profile.caps.audio_file
    }
    async fn audio_file(&self, named: &str, signal: Signal) -> Result<Option<String>, RuntimeError> {
        match self.listening.as_ref().filter(|_| self.has_audio_file()) {
            Some(listening) => listening.hearing.audio_file(named, signal).await,
            None => Ok(None),
        }
    }
    async fn focus_body(&self, request: &BodyRequest, signal: Signal) -> Result<Option<FocusBody>, RuntimeError> {
        let Some(mut args) = (self.daw.body_request)(request).filter(|_| self.daw.bodies.contains(&request.kind())) else {
            return Ok(None);
        };
        // The timeline's tracks around the one the producer works on, not the first of a big project's.
        if let (BodyRequest::Timeline, Some(track)) = (request, self.selected.borrow().clone()) {
            args.insert("around".into(), track.into());
        }
        if let Some(layout) = self.layout.get().filter(|_| names_tracks(&self.book.borrow().scheme, &args)) {
            args.insert("layout".into(), layout.into());
        }
        let args = Value::Object(args);
        Ok(match self.call("focus_body", args, signal).await? {
            Ok(body) => {
                follow_epoch(&self.book, &self.epoch, &self.layout, &body);
                (self.daw.to_body)(request, &body)
            }
            Err(_) => None,
        })
    }
    async fn observe(&self, signal: Signal, hints: Option<ObserveHints>) -> Result<Observation, RuntimeError> {
        let pinned = hints.as_ref().and_then(|hints| hints.pinned.as_ref());
        signal.check()?;
        self.changes.new_turn();
        // A new answer: a judged run's changes count from here (a match run's next round goes on).
        if let Some(listening) = &self.listening {
            listening.new_turn(hints.as_ref().is_some_and(|hints| hints.continuing == Some(true)));
        }
        if self.closed.get() {
            return Err(RuntimeError::plain("Integration is closed"));
        }
        match self.look(&signal).await {
            Ok(Ok(mut snapshot)) => {
                // A snapshot older than a shift the book has followed is taken again, carried to now.
                for _ in 0..3 {
                    if layout_of(&self.book, &self.layout, snapshot.get("layout").and_then(Value::as_u64)).await != Layout::Older {
                        break;
                    }
                    match self.look(&signal).await? {
                        Ok(now) => snapshot = now,
                        Err(_) => break,
                    }
                }
                if !self.connected.get() {
                    self.back_at.set(Some(tokio::time::Instant::now()));
                }
                let snapshot = self.settled(snapshot, &signal).await?;
                self.set_connected(true);
                Ok(self.observation(&snapshot, pinned))
            }
            // Busy (a dialog, a load): still there, and still connected. Kumi waits a little for it and looks once more
            // (as Live's #256); still busy, the look keeps the project, and the focus says why it waits.
            Ok(Err(refused)) if refused.there && refused.busy => {
                for _ in 0..20 {
                    signal.check()?;
                    tokio::time::sleep(std::time::Duration::from_millis(250)).await;
                    let busy = matches!(self.call("status", json!({}), signal.clone()).await, Ok(Ok(status)) if status.get("busy").is_some_and(|b| b != &Value::Bool(false)));
                    if !busy {
                        break;
                    }
                }
                match self.look(&signal).await {
                    Ok(Ok(snapshot)) => Ok(self.observation(&snapshot, pinned)),
                    Ok(Err(refused)) if refused.there => Ok(self.unread_for(refused, &signal).await),
                    Ok(Err(refused)) => Ok(self.no_access(Some(&refused.why))),
                    Err(error) if error.is_aborted() => Err(error),
                    Err(_) => Ok(self.no_access(None)),
                }
            }
            // Still there, but the look failed: for what the bridge lacks (a permission), or the DAW's own change.
            Ok(Err(refused)) if refused.there => Ok(self.unread_for(refused, &signal).await),
            Ok(Err(refused)) => {
                if self.connected.replace(false) {
                    (self.options.context.on_connection)(ConnectionState::Disconnected, Some(DisconnectCause::Live));
                }
                self.identity.borrow_mut().take();
                Ok(self.no_access(Some(&refused.why)))
            }
            Err(error) if error.is_aborted() => Err(error),
            Err(_) => {
                self.set_connected(false);
                Ok(self.no_access(None))
            }
        }
    }
    async fn close(&self) -> Result<(), RuntimeError> {
        // The saved project's baseline, kept as Kumi leaves it if the DAW answers at once.
        if let Some(remember) = self.remember.as_ref().filter(|_| !self.closed.get()) {
            remember.close().await;
        }
        self.closed.set(true);
        self.lifetime.cancel();
        if let Some(unlisten) = self.unlisten.borrow_mut().take() {
            unlisten();
        }
        if let Some(listening) = &self.listening {
            listening.hearing.close().await;
        }
        let endpoint = self.endpoint.borrow_mut().take();
        if let Some(endpoint) = endpoint {
            endpoint.close().await?;
        }
        Ok(())
    }
}

/// The model's read of the DAW: a kind of thing, under a parent, a page at a time, in the turn's short refs.
struct ReadTool<S: RefScheme> {
    daw: &'static OwnDaw,
    endpoint: Option<Rc<dyn McpEndpoint>>,
    book: Rc<RefCell<Book<S>>>,
    layout: Rc<Cell<Option<u64>>>,
    epoch: Rc<RefCell<Option<String>>>,
    seen_in: Rc<RefCell<HashMap<String, String>>>,
    /// The description, with which kinds are read under which parent.
    description: String,
}

/// An answer from another epoch than the book's: Renoise moves it for a device, sample, phrase, pattern, instrument or
/// sequence edit, and no event says so. Positional names are retired, and the book takes the answer's epoch and layout.
/// (A DAW whose refs hold across epochs keeps its names; `see_epoch` looks after it.)
pub(super) fn follow_epoch<S: RefScheme>(
    book: &RefCell<Book<S>>,
    epoch: &RefCell<Option<String>>,
    layout: &Cell<Option<u64>>,
    answer: &Value,
) {
    let Some(now) = answer.get("epoch").and_then(Value::as_str) else { return };
    if !book.borrow().scheme.positional() || epoch.borrow().as_deref() == Some(now) {
        return;
    }
    // Moved only for devices, patterns or the sequence from the epoch the book is in (`kept`): the tracks (and the kinds
    // `kept.kinds` names, when it does) are where they were, and the DAW takes their refs from before, so their names
    // stay and the layout goes on as the tracks events say; every other name goes.
    let kept = answer.get("kept").filter(|kept| {
        (kept.get("tracks") == Some(&Value::Bool(true)) || kept.get("kinds").is_some_and(Value::is_array))
            && kept.get("from").and_then(Value::as_str).is_some_and(|from| epoch.borrow().as_deref() == Some(from))
    });
    if let Some(kept) = kept {
        let kinds: Vec<&str> = match kept.get("kinds").and_then(Value::as_array) {
            Some(kinds) => kinds.iter().filter_map(Value::as_str).collect(),
            None => vec!["track"],
        };
        let survives = |scheme: &S, reference: &str| scheme.kind_of(reference).is_some_and(|kind| kinds.contains(&kind));
        let mut book = book.borrow_mut();
        let gone: Vec<String> =
            book.refs.keys().chain(book.known.keys()).filter(|reference| !survives(&book.scheme, reference)).cloned().collect();
        for reference in gone {
            book.retire(&reference);
        }
        book.cursors.clear();
        book.keep_names(survives);
        book.stand(now);
        drop(book);
        *epoch.borrow_mut() = Some(now.to_owned());
        return;
    }
    if epoch.borrow().is_some() {
        let mut book = book.borrow_mut();
        book.refs.clear();
        book.known.clear();
        book.cursors.clear();
        book.clear_names();
    }
    // What every project has, which no read lists (Logic's Stereo Out), keeps its name in the new epoch.
    book.borrow_mut().stand(now);
    *epoch.borrow_mut() = Some(now.to_owned());
    layout.set(answer.get("layout").and_then(Value::as_u64));
}

/// Whether a request names something on a track (a track, a device or a parameter), the refs a track shift moves: only
/// those carry the layout they were made in.
pub(super) fn names_tracks<S: RefScheme>(scheme: &S, args: &JsonObject) -> bool {
    // Any string anywhere in it (a change's `group`, a list of refs), as the tool looks; a bare track number too.
    fn walk<S: RefScheme>(scheme: &S, value: &Value, depth: usize) -> bool {
        match value {
            Value::String(text) => matches!(scheme.kind_of(text), Some("track" | "device" | "param" | "ptrack" | "slot")),
            Value::Array(items) if depth < 8 => items.iter().any(|item| walk(scheme, item, depth + 1)),
            Value::Object(row) if depth < 8 => row.values().any(|item| walk(scheme, item, depth + 1)),
            _ => false,
        }
    }
    args.get("track").is_some_and(Value::is_number) || args.values().any(|value| walk(scheme, value, 0))
}

/// An answer's layout against the book's.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Layout {
    /// The same (or the DAW's refs aren't positions): its refs name what the book's names do.
    Same,
    /// Newer, and no tracks event brought the book there (one was lost): the book's names went, and it takes the answer's.
    Adopted,
    /// Older than the book (a shift landed while it was on its way): read it again, carried to the layout of now.
    Older,
}

/// Where an answer's layout stands against the book's. A tracks event on its way is waited for a moment first.
pub(super) async fn layout_of<S: RefScheme>(book: &RefCell<Book<S>>, layout: &Cell<Option<u64>>, answered: Option<u64>) -> Layout {
    let Some(answered) = answered.filter(|_| book.borrow().scheme.positional()) else { return Layout::Same };
    for _ in 0..4 {
        match layout.get() {
            None => {
                layout.set(Some(answered));
                return Layout::Same;
            }
            Some(now) if now == answered => return Layout::Same,
            Some(now) if now > answered => return Layout::Older,
            Some(_) => tokio::time::sleep(std::time::Duration::from_millis(25)).await,
        }
    }
    let mut book = book.borrow_mut();
    book.clear_names();
    book.cursors.clear();
    layout.set(Some(answered));
    Layout::Adopted
}

/// Keeps the epoch an answer was read in for each long ref it names.
pub(super) fn note_epoch(seen_in: &RefCell<HashMap<String, String>>, scheme: &impl RefScheme, answer: &Value) {
    let Some(epoch) = answer.get("epoch").and_then(Value::as_str) else { return };
    let text = answer.to_string();
    let mut seen_in = seen_in.borrow_mut();
    for reference in scheme.refs_in(&text) {
        seen_in.insert(reference.to_owned(), epoch.to_owned());
    }
}

/// The project as the model sees it: its name and state, without Kumi's identity for it or the folders it's in (its file
/// name stays), and without what the transport tells once already.
fn shown_project(project: &Value, transport: Option<&Value>) -> Value {
    let mut shown = project.clone();
    if let Some(row) = shown.as_object_mut() {
        row.remove("identity");
        if let Some(path) = row.remove("path").and_then(|path| path.as_str().map(str::to_owned)).filter(|path| !path.is_empty()) {
            let file = path.rsplit(['/', '\\']).next().unwrap_or(&path).to_owned();
            row.insert("file".into(), json!(file));
        }
        if transport.is_some_and(|transport| transport.get("playing").is_some()) {
            row.remove("playState");
        }
        if transport.is_some_and(|transport| transport.get("loop").is_some()) {
            row.remove("loop");
        }
    }
    shown
}

/// The read tool's description: what it reads, and for each kind read inside something, its parent.
fn read_description(daw: &OwnDaw, scheme: &impl RefScheme) -> String {
    let parents: Vec<String> = daw
        .read_kinds
        .iter()
        .filter_map(|kind| scheme.parents(kind).map(|parents| format!("{kind} under a {}", parents.join(" or "))))
        .collect();
    let line = if daw.read_kinds.contains(&"line") {
        " A pattern's lines need the pattern as parent and the track as trackRef; hidden: true reads the columns the track hides too."
    } else {
        ""
    };
    format!(
        "{} Pass ref alone to read one object again. Read inside something with its ref as parent: {}.{line}",
        daw.read_description,
        parents.join("; ")
    )
}

#[async_trait(?Send)]
impl<S: RefScheme + 'static> KernelTool for ReadTool<S> {
    fn name(&self) -> &str {
        self.daw.read_tool
    }
    fn description(&self) -> &str {
        &self.description
    }
    fn input_schema(&self) -> JsonObject {
        let app = self.daw.profile.words.app;
        let mut schema = json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "kind": {"type": "string", "enum": self.daw.read_kinds, "description": format!("What to read in {app} (or pass ref alone)")},
                "ref": {"type": "string", "description": "One object's ref, from this turn: reads it alone, as it is now"},
                "parent": {"type": "string", "description": "The ref of what it's in (a track's items, an FX's parameters…), from this turn"},
                "cursor": {"type": "string", "description": "The cursor an earlier page gave, for the next page"},
                "limit": {"type": "integer", "minimum": 1, "maximum": MOST_ROWS, "description": "How many rows (50 by default)"},
            },
        });
        // A REAPER API function that only reads, by name, with its arguments.
        if self.daw.read_kinds.contains(&"call") {
            schema["properties"]["fn"] = json!({"type": "string", "description": "For kind call: a REAPER API function that only reads (GetMediaTrackInfo_Value, TrackFX_GetParamName…)"});
            schema["properties"]["args"] = json!({"type": ["array", "object"], "description": "For kind call: its arguments, in order or by the SDK's names; tracks, items, takes and envelopes as refs"});
        }
        // A tracker's lines are read one track at a time.
        if self.daw.read_kinds.contains(&"line") {
            schema["properties"]["trackRef"] = json!({"type": "string", "description": "For kind line: the track (its ref, from this turn) whose lines of the parent pattern to read"});
            schema["properties"]["hidden"] = json!({"type": "boolean", "description": "For kind line: also read the note and effect columns the track hides (a row says hidden when they hold something)"});
        }
        schema.as_object().cloned().unwrap_or_default()
    }
    async fn execute(&self, input: JsonObject, signal: Signal) -> Result<ToolResult, RuntimeError> {
        signal.check()?;
        let Some(endpoint) = self.endpoint.clone() else {
            return Ok(ToolResult::error(self.daw.profile.vocab.cant_reach));
        };
        let args = self.book.borrow().lengthen(&Value::Object(input.clone()));
        let mut args = args.as_object().cloned().unwrap_or_default();
        if let Some(call_args) = args.get("args").cloned() {
            args.insert("args".into(), lengthen_anywhere(&self.book.borrow(), &call_args));
        }
        // A short ref this turn doesn't know (an earlier song's, or made up) is refused in Kumi's words, not the DAW's.
        for key in ["ref", "parent", "trackRef"] {
            let (Some(asked), Some(sent)) = (input.get(key).and_then(Value::as_str), args.get(key).and_then(Value::as_str)) else {
                continue;
            };
            if asked == sent && SHORT_REF.is_match(asked) {
                let place = self.daw.profile.words.place;
                return Ok(ToolResult::error(format!(
                    "{asked} isn't one of this turn's refs (it may be from before the {place} changed): read again for a current one"
                )));
            }
        }
        // The host's name for the track whose lines are read.
        if let Some(track) = args.remove("trackRef") {
            args.insert("track".into(), track);
        }
        // Only refs on a track are carried through track shifts: a request without one names no layout.
        if let Some(layout) = self.layout.get().filter(|_| names_tracks(&self.book.borrow().scheme, &args)) {
            args.insert("layout".into(), layout.into());
        }
        // Refs that hold across projects are read only in the project they came from: the DAW refuses another (`stale`).
        let primary = args.get("ref").or_else(|| args.get("parent")).and_then(Value::as_str).map(str::to_owned);
        let epoch = primary.and_then(|reference| self.seen_in.borrow().get(&reference).cloned());
        if let Some(epoch) = epoch.filter(|_| !self.book.borrow().scheme.positional()) {
            args.insert("epoch".into(), epoch.into());
        }
        // A page the model can take whole: past the kernel's cut its nextCursor would be lost, so fewer rows are asked for
        // (a few times at most, should the bridge not take fewer).
        let mut tries = 0;
        loop {
            let result = endpoint.call("read", args.clone(), abort::any([signal.clone(), abort::timeout(CALL_MS)])).await?;
            match answer_of(&result) {
                Ok(page) => {
                    follow_epoch(&self.book, &self.epoch, &self.layout, &page);
                    if layout_of(&self.book, &self.layout, page.get("layout").and_then(Value::as_u64)).await == Layout::Older {
                        if tries < 4 {
                            tries += 1;
                            continue;
                        }
                        // Still from before a shift the book follows: its refs would get the wrong names.
                        let app = self.daw.profile.words.app;
                        return Ok(ToolResult::error(format!("{app} changed while reading (tracks were added or removed): read it again")));
                    }
                    note_epoch(&self.seen_in, &self.book.borrow().scheme, &page);
                    let mut page = page;
                    // (a call's answer holds refs under no ref-named field)
                    for key in ["result", "out"] {
                        if let Some(value) = page.get(key).cloned() {
                            page[key] = shorten_anywhere(&mut self.book.borrow_mut(), &value);
                        }
                    }
                    let text = stringify(&self.book.borrow_mut().shorten(&page));
                    let rows = page.get("items").and_then(Value::as_array).map_or(0, Vec::len);
                    let asked = args.get("limit").and_then(Value::as_u64).map_or(rows, |limit| (limit as usize).min(rows));
                    tries += 1;
                    if text.len() > PAGE_TEXT_BYTES && asked > 1 && tries < 5 {
                        args.insert("limit".into(), (asked / 2).into());
                        continue;
                    }
                    return Ok(ToolResult::text(text));
                }
                // A ref from another project, or a list that moved between pages: what to do, in Kumi's words.
                Err(Refused { code: Some(code), .. }) if code == "stale" || code == "changed" => {
                    let words = self.daw.profile.words;
                    let what = input.get("ref").or_else(|| input.get("parent")).and_then(Value::as_str).unwrap_or("That ref");
                    let positional = self.book.borrow().scheme.positional();
                    return Ok(ToolResult::error(match code.as_str() {
                        "stale" if positional => {
                            format!("{what} is from before the {} changed (something was added, removed or opened): read again for current refs", words.place)
                        }
                        "stale" => format!(
                            "{what} was read in another {} than the one {} has open now: read again for current refs",
                            words.place, words.app
                        ),
                        _ => format!(
                            "{} changed while reading (something was added or removed): read it again from the start, without the cursor",
                            words.app
                        ),
                    }));
                }
                // The DAW's words, with its long refs in this turn's short ones.
                Err(refused) => {
                    let mut why = refused.why;
                    let mut book = self.book.borrow_mut();
                    for reference in book.scheme.refs_in(&why.clone()) {
                        let short = book.short_ref(reference);
                        why = why.replace(reference, &short);
                    }
                    return Ok(ToolResult::error(why));
                }
            }
        }
    }
    fn reach(&self) -> Reach {
        Reach::DawRead
    }
}
