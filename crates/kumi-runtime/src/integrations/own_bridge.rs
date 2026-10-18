//! The DAWs whose bridges are Kumi's own (REAPER's extension, Renoise's tool), through Kumi's host, `daw-mcp-server`:
//! one integration over the host's tools (`status`, `snapshot`, `read`, `focus`, `focus_body`), each DAW giving its
//! words, its refs and its instructions.
use crate::{
    core::{
        context::IntegrationContext,
        contracts::*,
        daw::DawProfile,
        errors::RuntimeError,
        fold::fold_tracks_with,
        place::{is_project_id, unsaved, Place},
        refs::{Book, RefScheme, Shift},
        view::{BodyKind, BodyRequest, DawFocus, FocusBody},
    },
    integrations::none::no_access,
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
const CALL_MS: u64 = 30_000;
/// How often FOCUS's focus is read while the app shows it, unless the app says.
const FOCUS_MS: u64 = 150;
/// How often the focus is polled when nothing draws it (plain mode).
const UNDRAWN_FOCUS_MS: u64 = 1_000;
/// The most a page of the model's read asks for.
const MOST_ROWS: u64 = 200;

/// What one DAW gives the shared integration: its profile, how its host is started, its instructions, and the kinds
/// its read tool takes.
pub struct OwnDaw {
    pub profile: &'static DawProfile,
    pub descriptor: BridgeDescriptor,
    /// What `daw-mcp-server --daw` takes: "reaper", "renoise".
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
}

/// Starts Kumi's host for a DAW: the production one runs `daw-mcp-server`; a test's stand-in answers in its place.
pub type Connect = Rc<dyn Fn(Signal) -> LocalBoxFuture<'static, Result<Rc<dyn McpEndpoint>, RuntimeError>>>;

/// What the app gives a DAW's integration beside the neutral context.
#[derive(Clone)]
pub struct OwnOptions {
    pub context: IntegrationContext,
    /// Kumi's folder, where the host finds the bridge's secret and discovery files.
    pub kumi_home: PathBuf,
    pub connect: Option<Connect>,
}

/// One DAW's integration through Kumi's host.
pub struct OwnIntegration<S: RefScheme> {
    daw: &'static OwnDaw,
    options: OwnOptions,
    endpoint: Rc<RefCell<Option<Rc<dyn McpEndpoint>>>>,
    book: Rc<RefCell<Book<S>>>,
    epoch: RefCell<Option<String>>,
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
    /// The epoch each long ref was last read in: a read names its ref's, so a DAW whose refs hold across projects
    /// (REAPER's GUIDs, which a Save As copy in another tab shares) refuses one read in another project.
    seen_in: Rc<RefCell<HashMap<String, String>>>,
    /// Stops listening to the host's events.
    unlisten: RefCell<Option<Box<dyn Fn()>>>,
    /// The last look that read the project: its key and project, kept by a look that can't (as Live's are), so the
    /// conversation stays with the project while the DAW is busy or away.
    previous: RefCell<Option<(String, Option<ProjectRef>)>>,
}

pub fn create_own_integration<S: RefScheme + 'static>(daw: &'static OwnDaw, options: OwnOptions) -> Rc<dyn Integration> {
    Rc::new(OwnIntegration::<S> {
        daw,
        options,
        endpoint: Rc::new(RefCell::new(None)),
        book: Rc::new(RefCell::new(Book::default())),
        epoch: RefCell::new(None),
        connected: Rc::default(),
        closed: Rc::new(Cell::new(false)),
        watching: Cell::new(false),
        selected: Rc::default(),
        identity: Rc::default(),
        layout: Rc::default(),
        seen_in: Rc::default(),
        unlisten: RefCell::default(),
        previous: RefCell::default(),
    })
}

/// The markers a look shows; more are read.
const MARKERS_SHOWN: usize = 30;
/// The most a read's page can hold for the model: the kernel cuts a tool's result at 64 KB.
const PAGE_TEXT_BYTES: usize = 60 * 1024;
/// A turn's short ref (`track:3`): the shape of a name this turn gave.
static SHORT_REF: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[a-z]+:[0-9]+$").unwrap());

/// Why a host tool answered an error: its words, and whether the DAW is there at all (it quit, or its bridge did).
struct Refused {
    why: String,
    there: bool,
    /// Busy for now (a dialog, a load, a render): worth waiting for.
    busy: bool,
    /// The host's code for it (`stale`, `changed`, `failed`…).
    code: Option<String>,
}

/// The JSON a host tool answered with, or why it couldn't.
fn answer_of(result: &CallToolResult) -> Result<Value, Refused> {
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
            busy: value.get("code").and_then(Value::as_str) == Some("busy"),
            code: value.get("code").and_then(Value::as_str).map(str::to_owned),
        });
    }
    Ok(value)
}

/// Why the DAW may answer late, in words: REAPER's `busy` code.
fn busy_words(code: &str, profile: &DawProfile) -> String {
    match code {
        "modal" => "a dialog is open".into(),
        "loading" => format!("a {} is loading", profile.words.place),
        "rendering" => "it's rendering".into(),
        other => other.into(),
    }
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
        // Tracks inserted, removed or moved where refs are positions: the book's refs follow what they named.
        let (book, layout) = (self.book.clone(), self.layout.clone());
        let unlisten = endpoint.on_live_event(Rc::new(move |event| {
            if event.get("name").and_then(Value::as_str) != Some("tracks") {
                return;
            }
            let data = event.get("data").cloned().unwrap_or(Value::Null);
            let places = |key: &str| -> Vec<usize> {
                data.get(key).and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_u64).map(|n| n as usize).collect()
            };
            book.borrow_mut().shift(&Shift { tracks_made: places("made"), tracks_gone: places("gone"), ..Shift::default() });
            if let Some(now) = data.get("layout").and_then(Value::as_u64) {
                layout.set(Some(now));
            }
        }));
        if let Some(before) = self.unlisten.replace(Some(unlisten)) {
            before();
        }
        Ok(endpoint)
    }

    /// A host tool's answer: Ok with its JSON, Err with why (the host's words) when it answered an error.
    async fn call(&self, tool: &str, args: Value, signal: Signal) -> Result<Result<Value, Refused>, RuntimeError> {
        let endpoint = self.endpoint(signal.clone()).await?;
        let args = args.as_object().cloned().unwrap_or_default();
        let bounded = abort::any([signal, abort::timeout(CALL_MS)]);
        match endpoint.call(tool, args, bounded).await {
            Ok(result) => Ok(answer_of(&result)),
            Err(error) => {
                // The host went away: the next call starts it again.
                self.endpoint.borrow_mut().take();
                Err(error)
            }
        }
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
                        last_epoch = None;
                        lose_focus(&mut last);
                        continue;
                    }
                    _ => {
                        lose_focus(&mut last);
                        continue;
                    }
                };
                // Back after it went away: the session looks again.
                if !connected.replace(true) {
                    on_connection(ConnectionState::Connected, None);
                }
                // Another project in the same tab, or another song: the epoch moved, and the project's identity with it.
                let epoch = focus.get("epoch").and_then(Value::as_str).map(str::to_owned);
                if last_epoch.is_some() && epoch != last_epoch {
                    let mut args = JsonObject::new();
                    args.insert("kind".into(), daw.project_kind.into());
                    let row = host.call("read", args, abort::timeout(2_000)).await.ok().and_then(|result| answer_of(&result).ok());
                    let now = row.as_ref().and_then(|row| row.pointer("/items/0/identity")).and_then(Value::as_str).map(str::to_owned);
                    let before = identity.borrow().clone();
                    if now.is_some() && before.is_some() && now != before {
                        *identity.borrow_mut() = now;
                        on_connection(ConnectionState::Disconnected, Some(DisconnectCause::Set));
                        on_connection(ConnectionState::Connected, None);
                    }
                }
                last_epoch = epoch;
                *selected.borrow_mut() = focus.pointer("/selection/trackRef").and_then(Value::as_str).map(str::to_owned);
                let seen = json!({"view": focus.get("view"), "selection": focus.get("selection"), "epoch": focus.get("epoch"), "busy": focus.get("busy")});
                if last.as_ref() != Some(&seen) {
                    if let Some(on_focus) = &on_focus {
                        let mut filled = (daw.fill_focus)(&focus);
                        filled.busy = focus.get("busy").and_then(Value::as_str).map(|code| busy_words(code, daw.profile));
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
        self.see_epoch(snapshot.get("epoch").and_then(Value::as_str));
        note_epoch(&self.seen_in, &self.book.borrow().scheme, snapshot);
        if self.layout.get().is_none() {
            self.layout.set(snapshot.get("layout").and_then(Value::as_u64));
        }
        let profile = self.daw.profile;
        let project = snapshot.get("project").cloned().unwrap_or(Value::Null);
        let name = project.get("name").and_then(Value::as_str).filter(|name| !name.is_empty()).unwrap_or("untitled").to_owned();
        let saved = project.get("id").and_then(Value::as_str).filter(|id| is_project_id(id));
        let place = match saved {
            Some(id) => Place::Saved { daw: self.id(), id: id.into() }.text(),
            None => unsaved(self.id()).into(),
        };
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
        // What the producer pinned in FOCUS (an FX, a DSP device), in the turn's short refs.
        if let Some(pin) = pinned {
            context["pinned"] = self.book.borrow_mut().shorten(&serde_json::to_value(pin).unwrap_or(Value::Null));
        }
        let identity = project.get("identity").and_then(Value::as_str).map(str::to_owned);
        *self.identity.borrow_mut() = identity.clone();
        // A saved project is its id, through a restart of the DAW; an unsaved one is this run's identity for it.
        let key = saved
            .map(|id| format!("{}:{id}", self.daw.host_flag))
            .or_else(|| identity.clone().map(|identity| format!("{}:{identity}", self.daw.host_flag)))
            .unwrap_or_else(|| place.clone());
        *self.previous.borrow_mut() = Some((key.clone(), saved.map(|_| ProjectRef { id: place.clone(), name: name.clone() })));
        let tool: Rc<dyn KernelTool> = Rc::new(ReadTool::<S> {
            daw: self.daw,
            endpoint: self.endpoint.borrow().clone(),
            book: self.book.clone(),
            layout: self.layout.clone(),
            seen_in: self.seen_in.clone(),
            description: read_description(self.daw, &self.book.borrow().scheme),
        });
        Observation {
            key: key.clone(),
            revision: None,
            label: format!("{} — {name}", profile.words.app),
            context: stringify(&context),
            instructions: self.daw.instructions.into(),
            tools: vec![tool],
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
        self.set_connected(status.is_ok_and(|status| status.get("connected") != Some(&Value::Bool(false))));
        self.watch_focus();
        Ok(())
    }
    fn can_show(&self, kind: BodyKind) -> bool {
        self.daw.bodies.contains(&kind)
    }
    async fn focus_body(&self, request: &BodyRequest, signal: Signal) -> Result<Option<FocusBody>, RuntimeError> {
        let Some(mut args) = (self.daw.body_request)(request).filter(|_| self.daw.bodies.contains(&request.kind())) else {
            return Ok(None);
        };
        // The timeline's tracks around the one the producer works on, not the first of a big project's.
        if let (BodyRequest::Timeline, Some(track)) = (request, self.selected.borrow().clone()) {
            args.insert("around".into(), track.into());
        }
        if let Some(layout) = self.layout.get() {
            args.insert("layout".into(), layout.into());
        }
        let args = Value::Object(args);
        Ok(match self.call("focus_body", args, signal).await? {
            Ok(body) => (self.daw.to_body)(request, &body),
            Err(_) => None,
        })
    }
    async fn observe(&self, signal: Signal, hints: Option<ObserveHints>) -> Result<Observation, RuntimeError> {
        let pinned = hints.as_ref().and_then(|hints| hints.pinned.as_ref());
        signal.check()?;
        if self.closed.get() {
            return Err(RuntimeError::plain("Integration is closed"));
        }
        match self.call("snapshot", json!({}), signal.clone()).await {
            Ok(Ok(snapshot)) => {
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
                match self.call("snapshot", json!({}), signal).await {
                    Ok(Ok(snapshot)) => Ok(self.observation(&snapshot, pinned)),
                    Ok(Err(refused)) => Ok(self.no_access(Some(&refused.why))),
                    Err(error) if error.is_aborted() => Err(error),
                    Err(_) => Ok(self.no_access(None)),
                }
            }
            Ok(Err(refused)) if refused.there => Ok(self.no_access(Some(&refused.why))),
            Ok(Err(refused)) => {
                if self.connected.replace(false) {
                    (self.options.context.on_connection)(ConnectionState::Disconnected, Some(DisconnectCause::Live));
                }
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
        self.closed.set(true);
        if let Some(unlisten) = self.unlisten.borrow_mut().take() {
            unlisten();
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
    seen_in: Rc<RefCell<HashMap<String, String>>>,
    /// The description, with which kinds are read under which parent.
    description: String,
}

/// Keeps the epoch an answer was read in for each long ref it names.
fn note_epoch(seen_in: &RefCell<HashMap<String, String>>, scheme: &impl RefScheme, answer: &Value) {
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
    let line =
        if daw.read_kinds.contains(&"line") { " A pattern's lines need the pattern as parent and the track as trackRef." } else { "" };
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
        // A tracker's lines are read one track at a time.
        if self.daw.read_kinds.contains(&"line") {
            schema["properties"]["trackRef"] = json!({"type": "string", "description": "For kind line: the track (its ref, from this turn) whose lines of the parent pattern to read"});
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
        if let Some(layout) = self.layout.get() {
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
                    note_epoch(&self.seen_in, &self.book.borrow().scheme, &page);
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
