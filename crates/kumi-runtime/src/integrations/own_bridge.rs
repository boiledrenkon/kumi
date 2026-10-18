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
        refs::{Book, RefScheme},
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
};
use serde_json::{json, Value};
use std::{
    cell::{Cell, RefCell},
    path::PathBuf,
    rc::Rc,
};

/// How long a call to Kumi's host waits.
const CALL_MS: u64 = 30_000;
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
    endpoint: RefCell<Option<Rc<dyn McpEndpoint>>>,
    book: Rc<RefCell<Book<S>>>,
    epoch: RefCell<Option<String>>,
    connected: Cell<bool>,
    closed: Cell<bool>,
}

pub fn create_own_integration<S: RefScheme + 'static>(daw: &'static OwnDaw, options: OwnOptions) -> Rc<dyn Integration> {
    Rc::new(OwnIntegration::<S> {
        daw,
        options,
        endpoint: RefCell::new(None),
        book: Rc::new(RefCell::new(Book::default())),
        epoch: RefCell::new(None),
        connected: Cell::new(false),
        closed: Cell::new(false),
    })
}

/// The JSON a host tool answered with, or why it couldn't.
fn answer_of(result: &CallToolResult) -> Result<Value, String> {
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
        return Err(value.get("error").and_then(Value::as_str).map(str::to_owned).unwrap_or(text));
    }
    Ok(value)
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
        Ok(endpoint)
    }

    /// A host tool's answer: Ok with its JSON, Err with why (the host's words) when it answered an error.
    async fn call(&self, tool: &str, args: Value, signal: Signal) -> Result<Result<Value, String>, RuntimeError> {
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
            let mut book = self.book.borrow_mut();
            book.refs.clear();
            book.known.clear();
            book.cursors.clear();
            book.clear_names();
        }
        *self.epoch.borrow_mut() = Some(epoch.to_owned());
    }

    fn no_access(&self, why: Option<&str>) -> Observation {
        let mut observation = no_access(self.daw.profile, self.daw.instructions, unsaved(self.id()), Utc::now(), None);
        if let Some(why) = why {
            observation.label = format!("{} — {why}", observation.label);
        }
        observation
    }

    fn id(&self) -> crate::core::daw::DawId {
        self.daw.profile.id.expect("a DAW with a bridge of Kumi's own is known")
    }

    /// The observation from the host's snapshot.
    fn observation(&self, snapshot: &Value) -> Observation {
        self.see_epoch(snapshot.get("epoch").and_then(Value::as_str));
        let profile = self.daw.profile;
        let project = snapshot.get("project").cloned().unwrap_or(Value::Null);
        let name = project.get("name").and_then(Value::as_str).filter(|name| !name.is_empty()).unwrap_or("untitled").to_owned();
        let saved = project.get("id").and_then(Value::as_str).filter(|id| is_project_id(id));
        let place = match saved {
            Some(id) => Place::Saved { daw: self.id(), id: id.into() }.text(),
            None => unsaved(self.id()).into(),
        };
        let mut book = self.book.borrow_mut();
        let tracks: Vec<JsonObject> = snapshot
            .get("tracks")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|row| book.shorten(row).as_object().cloned())
            .collect();
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
        context[profile.words.place] = project.clone();
        context["transport"] = snapshot.get("transport").cloned().unwrap_or(Value::Null);
        context["tracks"] = json!(folded.tracks);
        context["selection"] = selection;
        if let Some(folded_note) = folded.folded {
            context["folded"] = json!(folded_note);
        }
        if let Some(more) = folded.more_tracks {
            context["moreTracks"] = json!(more);
        }
        if !markers.is_null() {
            context["markers"] = markers;
        }
        let identity = project.get("identity").and_then(Value::as_str).map(str::to_owned);
        let tool: Rc<dyn KernelTool> =
            Rc::new(ReadTool::<S> { daw: self.daw, endpoint: self.endpoint.borrow().clone(), book: self.book.clone() });
        Observation {
            key: identity.clone().map(|identity| format!("{}:{identity}", self.daw.host_flag)).unwrap_or_else(|| place.clone()),
            revision: None,
            label: format!("{} — {name}", profile.words.app),
            context: stringify(&context),
            instructions: self.daw.instructions.into(),
            tools: vec![tool],
            project: saved.map(|_| ProjectRef { id: place, name: name.clone() }),
            set: identity,
            tracks: Some(tracks.iter().filter_map(|track| track.get("name").and_then(Value::as_str).map(str::to_owned)).collect()),
            saved_at: None,
            identity: None,
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
        Ok(())
    }
    async fn observe(&self, signal: Signal, _hints: Option<ObserveHints>) -> Result<Observation, RuntimeError> {
        signal.check()?;
        if self.closed.get() {
            return Err(RuntimeError::plain("Integration is closed"));
        }
        match self.call("snapshot", json!({}), signal).await {
            Ok(Ok(snapshot)) => {
                self.set_connected(true);
                Ok(self.observation(&snapshot))
            }
            Ok(Err(why)) => {
                self.set_connected(false);
                Ok(self.no_access(Some(&why)))
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
}

#[async_trait(?Send)]
impl<S: RefScheme + 'static> KernelTool for ReadTool<S> {
    fn name(&self) -> &str {
        self.daw.read_tool
    }
    fn description(&self) -> &str {
        self.daw.read_description
    }
    fn input_schema(&self) -> JsonObject {
        let app = self.daw.profile.words.app;
        json!({
            "type": "object",
            "additionalProperties": false,
            "required": ["kind"],
            "properties": {
                "kind": {"type": "string", "enum": self.daw.read_kinds, "description": format!("What to read in {app}")},
                "parent": {"type": "string", "description": "The ref of what it's in (a track's items, an FX's parameters…), from this turn"},
                "cursor": {"type": "string", "description": "The cursor an earlier page gave, for the next page"},
                "limit": {"type": "integer", "minimum": 1, "maximum": MOST_ROWS, "description": "How many rows (50 by default)"},
            },
        })
        .as_object()
        .cloned()
        .unwrap_or_default()
    }
    async fn execute(&self, input: JsonObject, signal: Signal) -> Result<ToolResult, RuntimeError> {
        signal.check()?;
        let Some(endpoint) = self.endpoint.clone() else {
            return Ok(ToolResult::error(self.daw.profile.vocab.cant_reach));
        };
        let args = self.book.borrow().lengthen(&Value::Object(input.clone()));
        let args = args.as_object().cloned().unwrap_or_default();
        let result = endpoint.call("read", args, abort::any([signal, abort::timeout(CALL_MS)])).await?;
        match answer_of(&result) {
            Ok(page) => {
                let shortened = self.book.borrow_mut().shorten(&page);
                Ok(ToolResult::text(stringify(&shortened)))
            }
            Err(why) => Ok(ToolResult::error(why)),
        }
    }
    fn reach(&self) -> Reach {
        Reach::DawRead
    }
}
