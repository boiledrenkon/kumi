//! A whole session over a DAW that isn't Live: Kumi's real agent kernel on a scripted model, every option on, and an
//! integration in another DAW's words. Nothing the model is sent (the system text, every tool and its schema, the
//! messages with their observations, notes and tool results) and nothing the producer is told (notices, errors) is in
//! Live's words: not as Live comes and goes, a loop, a job, memory and recipes, a stopped answer, results cleared to
//! save room, or running without the DAW.
use async_trait::async_trait;
use futures::stream::{self, StreamExt};
use futures::FutureExt;
use kumi_common::abort::{Signal, SignalExt};
use kumi_runtime::ai::error::LanguageModelError;
use kumi_runtime::ai::types::{
    CallOptions, FinishReason, FinishReasonUnified, InputTokens, OutputTokens, StreamPart, StreamParts, ToolCall, Usage as ModelUsage,
};
use kumi_runtime::core::contracts::*;
use kumi_runtime::core::daw::{Capabilities, PromptVocab, ScriptKind};
use kumi_runtime::core::errors::{FailureKind, KumiError, RuntimeError};
use kumi_runtime::core::memory::{create_memory_store, MemoryStoreOptions};
use kumi_runtime::core::session::{JobsOptions, VideoDirectories};
use kumi_runtime::core::techniques::create_technique_store;
use kumi_runtime::integrations::none::create_no_daw_integration;
use kumi_runtime::kernel::agent::{create_agent_kernel, AgentKernel, AgentKernelOptions, LanguageModel, ModelBinding, ModelRequest};
use kumi_runtime::kernel::budget::ContextBudget;
use kumi_runtime::library::{create_library, sources::SourceOptions, LibraryOptions};
use kumi_runtime::{
    create_conversation_store, create_goal_store, create_objective_store, create_playbook_store, create_recipe_store, create_session,
    with_fallback, Session, SessionOptions,
};
use regex::Regex;
use serde_json::{json, Value};
#[path = "support/in_process.rs"]
mod in_process;
use std::{
    cell::{Cell, RefCell},
    collections::VecDeque,
    rc::Rc,
    sync::LazyLock,
    time::Duration,
};

/// Live's words, whole: none may reach the producer or the model over another DAW.
static LIVE_WORDS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\b(Live|Set|Sets|Ableton|Session|Arrangement|Remote Script|Control Surface|Main|Drum Rack|Operator|[Rr]acks?|[Cc]lip slots?|[Ss]cenes?)\b")
        .unwrap()
});

/// What core may still say over another DAW that names Live's things, by where it's said, with why.
const ALLOWED: &[(&str, &str, &str)] = &[(
    "web/tool-data.json",
    "a Max patch or Max for Live device",
    "a file format the producer may hand Kumi in any DAW: Kumi reads it without Live",
)];

/// Another DAW, with what it can do (none of Live's own: no Max for Live, Ears, Hands, Willington or launcher grid).
const FAKE: DawProfile = DawProfile {
    id: None,
    words: DawWords { app: "Fakeloop", place: "song", places: "songs", master: "the master track", timeline: "timeline" },
    caps: Capabilities {
        undo: true,
        instant: true,
        scripting: Some(ScriptKind::Lua),
        audio_file: true,
        hear: true,
        audition: true,
        stop_transport: true,
        recording_state: true,
        open_project: true,
        device_tree: true,
        note_view: true,
        timeline_strip: true,
        catch_up: true,
        ..Capabilities::NONE
    },
    vocab: PromptVocab {
        own_undo: "Fakeloop's own undo (Ctrl-Z)",
        refs_retired: "Fakeloop may have taken the tracks back, so Kumi's references are retired: discover again before using any.",
        structure: "tracks",
        cant_reach: "Kumi can't reach Fakeloop. Is it open, with Kumi's extension in it?",
        crash_note: "If it crashed, your unsaved changes may be lost.",
        goal_needs: "Goals need Fakeloop connected, with Kumi's extension.",
        ..DawProfile::UNKNOWN.vocab
    },
};
const INSTRUCTIONS: &str = "You work in Fakeloop: read the song with fake_read before changing it.";

fn finish(unified: FinishReasonUnified) -> StreamPart {
    let raw = serde_json::to_value(unified).unwrap().as_str().unwrap().to_string();
    let usage = ModelUsage {
        input_tokens: InputTokens { total: Some(3.0), no_cache: Some(3.0), cache_read: Some(0.0), cache_write: Some(0.0) },
        output_tokens: OutputTokens { total: Some(2.0), text: Some(2.0), reasoning: Some(0.0) },
        raw: None,
    };
    StreamPart::Finish { usage, finish_reason: FinishReason { unified, raw: Some(raw) }, provider_metadata: None }
}
fn answer(words: &str) -> Vec<StreamPart> {
    vec![
        StreamPart::TextStart { id: "t".into(), provider_metadata: None },
        StreamPart::TextDelta { id: "t".into(), delta: words.into(), provider_metadata: None },
        StreamPart::TextEnd { id: "t".into(), provider_metadata: None },
        finish(FinishReasonUnified::Stop),
    ]
}
fn call(name: &str, input: Value) -> Vec<StreamPart> {
    vec![
        StreamPart::ToolCall(ToolCall {
            tool_call_id: format!("call_{name}"),
            tool_name: name.into(),
            input: input.to_string(),
            provider_executed: None,
            dynamic: None,
            provider_metadata: None,
        }),
        finish(FinishReasonUnified::ToolCalls),
    ]
}

/// What the model says next: each step in turn, then "Done."
type Step = Box<dyn Fn(&Recorded) -> Vec<StreamPart>>;
#[derive(Default)]
struct Recorded {
    tools: RefCell<Vec<Rc<dyn KernelTool>>>,
    /// Each request: its system text, messages and tools, as JSON text.
    requests: RefCell<Vec<String>>,
    script: RefCell<VecDeque<Step>>,
}
struct Scripted {
    recorded: Rc<Recorded>,
}
#[async_trait(?Send)]
impl LanguageModel for Scripted {
    async fn do_stream(&self, _: CallOptions) -> Result<StreamParts, LanguageModelError> {
        let step = self.recorded.script.borrow_mut().pop_front();
        let parts = step.map(|step| step(&self.recorded)).unwrap_or_else(|| answer("Done."));
        Ok(stream::iter(parts).boxed_local())
    }
}

/// Kumi's agent kernel on the scripted model, recording each request; a small budget, so earlier results are cleared.
fn recording(into: Rc<Recorded>, max_steps: Option<usize>) -> KernelFactory {
    Rc::new(move |options: KernelOptions| {
        let into = into.clone();
        async move {
            *into.tools.borrow_mut() = options.tools.clone();
            let requests = into.clone();
            let binding = ModelBinding {
                id: "test/fixture".into(),
                model: Rc::new(Scripted { recorded: into.clone() }),
                prepare: Box::new(move |request: ModelRequest| {
                    requests
                        .requests
                        .borrow_mut()
                        .push(json!({"system":request.instructions,"messages":request.messages,"tools":request.tools}).to_string());
                    CallOptions {
                        prompt: request.messages,
                        tools: (!request.tools.is_empty()).then_some(request.tools),
                        ..CallOptions::default()
                    }
                }),
                budget: Some(Box::new(|_| ContextBudget { clear_at: 1_500.0, limit: 1e9 })),
            };
            let kernel: Rc<AgentKernel> = Rc::new(create_agent_kernel(AgentKernelOptions {
                conversation: options.conversation,
                binding,
                instructions: options.instructions,
                tools: options.tools,
                signal: options.signal,
                checkpoint: options.checkpoint,
                max_steps,
                budget: None,
            })?);
            Ok(kernel as Rc<dyn Kernel>)
        }
        .boxed_local()
    })
}

/// The DAW's own tools: a read, and one that waits until it's stopped.
struct FakeTool {
    name: &'static str,
    started: Rc<Cell<bool>>,
}
#[async_trait(?Send)]
impl KernelTool for FakeTool {
    fn name(&self) -> &str {
        self.name
    }
    fn description(&self) -> &str {
        if self.name == "fake_read" {
            "Reads the song's tracks and patterns."
        } else {
            "Waits for Fakeloop to finish what it's playing."
        }
    }
    fn input_schema(&self) -> JsonObject {
        json!({"type":"object","properties":{}}).as_object().unwrap().clone()
    }
    async fn execute(&self, _input: JsonObject, signal: Signal) -> Result<ToolResult, RuntimeError> {
        if self.name == "fake_read" {
            return Ok(ToolResult::text(json!({"tracks":[{"ref":"track:1","name":"Kick"},{"ref":"track:2","name":"Bass"}]}).to_string()));
        }
        self.started.set(true);
        signal.cancelled().await;
        Err(RuntimeError::Aborted)
    }
    fn reach(&self) -> Reach {
        if self.name == "fake_read" {
            Reach::DawRead
        } else {
            Reach::DawWrite
        }
    }
}

/// The fake DAW as the session sees it.
struct FakeIntegration {
    listener: ConnectionListener,
    listeners: Rc<RefCell<Vec<ConnectionListener>>>,
    started: Rc<Cell<bool>>,
    /// The DAW won't start: the session carries on without it.
    away: bool,
}
#[async_trait(?Send)]
impl Integration for FakeIntegration {
    fn profile(&self) -> &DawProfile {
        &FAKE
    }
    async fn start(&self, signal: Signal) -> Result<(), RuntimeError> {
        signal.check()?;
        if self.away {
            return Err(KumiError::new(FailureKind::Daw, FAKE.vocab.cant_reach).into());
        }
        self.listeners.borrow_mut().push(self.listener.clone());
        (self.listener)(ConnectionState::Connecting, None);
        (self.listener)(ConnectionState::Connected, None);
        Ok(())
    }
    async fn observe(&self, signal: Signal, _: Option<ObserveHints>) -> Result<Observation, RuntimeError> {
        signal.check()?;
        let tools: Vec<Rc<dyn KernelTool>> = vec![
            Rc::new(FakeTool { name: "fake_read", started: self.started.clone() }),
            Rc::new(FakeTool { name: "fake_wait", started: self.started.clone() }),
        ];
        Ok(Observation {
            key: "song:beat".into(),
            revision: None,
            label: "Fakeloop — Beat".into(),
            context: json!({"song":"Beat","tempo":120,"tracks":[{"ref":"track:1","name":"Kick"},{"ref":"track:2","name":"Bass"}]})
                .to_string(),
            instructions: INSTRUCTIONS.into(),
            tools,
            project: Some(ProjectRef { id: "0123456789abcdef0123456789abcdef".into(), name: "Beat".into() }),
            set: Some("identity:beat".into()),
            tracks: Some(vec!["Kick".into(), "Bass".into()]),
            saved_at: None,
            identity: None,
        })
    }
    async fn close(&self) -> Result<(), RuntimeError> {
        Ok(())
    }
    fn can_show(&self, kind: kumi_runtime::core::view::BodyKind) -> bool {
        kind == kumi_runtime::core::view::BodyKind::Matrix
    }
    async fn focus_body(
        &self,
        request: &kumi_runtime::core::view::BodyRequest,
        _: Signal,
    ) -> Result<Option<kumi_runtime::core::view::FocusBody>, RuntimeError> {
        use kumi_runtime::core::view::*;
        Ok((request == &BodyRequest::Matrix)
            .then(|| FocusBody::Matrix(SequenceMatrix { tracks: vec!["Kick".into()], slots: vec![], current: None, first: 0 })))
    }
}

/// A session with every option on, over the fake DAW (or, `away`, without it).
fn open(
    main: &Rc<Recorded>,
    worker: &Rc<Recorded>,
    dir: &std::path::Path,
    away: bool,
) -> (Session, Rc<RefCell<Vec<SessionEvent>>>, Rc<RefCell<Vec<ConnectionListener>>>, Rc<Cell<bool>>) {
    let events = Rc::new(RefCell::new(vec![]));
    let out = events.clone();
    let listeners = Rc::new(RefCell::new(vec![]));
    let started = Rc::new(Cell::new(false));
    let (held, waiting) = (listeners.clone(), started.clone());
    let integration: IntegrationFactory = Box::new(move |listener: ConnectionListener| {
        let fallback = listener.clone();
        let primary = Rc::new(FakeIntegration { listener, listeners: held.clone(), started: waiting.clone(), away });
        with_fallback(
            primary,
            Rc::new(move || {
                let on_connection = fallback.clone();
                create_no_daw_integration(FAKE, INSTRUCTIONS, Rc::new(move |state| on_connection(state, None)))
            }),
            Rc::new(|_| {}),
        )
    });
    let mut options = SessionOptions::new(recording(main.clone(), None), integration, Rc::new(move |event| out.borrow_mut().push(event)));
    options.profile = FAKE;
    options.timeout_ms = Some(20_000);
    options.close_timeout_ms = Some(5_000);
    options.cancel_grace_ms = Some(10);
    options.missing_after_ms = Some(30);
    let text = |name: &str| dir.join(name).to_string_lossy().into_owned();
    options.memory =
        Some(create_memory_store(MemoryStoreOptions { projects_dir: dir.join("projects"), producer_file: dir.join("producer.json") }));
    options.listen = true;
    options.web = true;
    options.recipes = Some(create_recipe_store(dir.join("recipes")));
    options.watch = Some(VideoDirectories { videos_dir: text("videos"), tools_dir: text("tools") });
    options.references = Some(Rc::new(kumi_runtime::references::store::ReferenceStore::new(text("references"))));
    options.techniques = Some(create_technique_store(dir.join("techniques.json")));
    options.gaps = Some(text("gaps.jsonl"));
    options.conversations = Some(create_conversation_store(dir));
    options.library = Some(create_library(LibraryOptions {
        dir: text("library"),
        find_sets: Some(false),
        fork: Some(false),
        sources: Some(SourceOptions { home: Some(text("home")), ..Default::default() }),
        ..Default::default()
    }));
    options.playbook = Some(create_playbook_store(text("playbook.json")));
    options.goals = Some(create_goal_store(text("goals")));
    options.objectives = Some(create_objective_store(text("objectives")));
    options.timings = Some(text("timings.jsonl"));
    options.jobs = Some(JobsOptions { worker_factory: recording(worker.clone(), Some(40)), max: 3, store: None });
    (create_session(options).unwrap(), events, listeners, started)
}

async fn delay(ms: u64) {
    tokio::time::sleep(Duration::from_millis(ms)).await;
}
fn notices(events: &RefCell<Vec<SessionEvent>>) -> Vec<String> {
    events
        .borrow()
        .iter()
        .filter_map(|event| match event {
            SessionEvent::Notice { message } => Some(message.clone()),
            _ => None,
        })
        .collect()
}
fn step(make: impl Fn(&Recorded) -> Vec<StreamPart> + 'static) -> Step {
    Box::new(make)
}

/// Everything core said over the fake DAW, in one session and in one without it.
async fn everything_said() -> Vec<String> {
    let dir = tempfile::tempdir().unwrap();
    let (main, worker) = (Rc::new(Recorded::default()), Rc::new(Recorded::default()));
    let (session, events, listeners, started) = open(&main, &worker, dir.path(), false);
    session.start().await.unwrap();

    // A turn that remembers, saves a recipe, starts a job, finds every deferred tool, and reads the song.
    main.script.borrow_mut().extend([
        step(|_| call("remember", json!({"note":"Likes a dry kick","about":"producer"}))),
        step(|_| {
            call("save_recipe", json!({"name":"drum bus","about":"Glues the drums","steps":[{"tool":"set_tempo","input":{"bpm":124}}]}))
        }),
        step(|_| call("start_job", json!({"title":"Kick references","task":"Find three reference tracks with a punchy kick."}))),
        step(|recorded| {
            let names: Vec<String> =
                recorded.tools.borrow().iter().filter(|tool| tool.deferred()).map(|tool| tool.name().to_owned()).take(24).collect();
            call("find_tools", json!({"names":names}))
        }),
        step(|_| call("find_tools", json!({"words":"listen to the mix and compare it with a reference"}))),
        step(|_| call("fake_read", json!({}))),
        step(|_| answer("Done.")),
    ]);
    session.submit("Make the kick punchier.", None).await.unwrap();

    // A loop.
    session.submit("/loop make the mix less muddy", None).await.unwrap();

    // The DAW goes, for each cause, is still missing a moment later, then comes back.
    for cause in [
        Some(DisconnectCause::Live),
        Some(DisconnectCause::Bridge),
        Some(DisconnectCause::Set),
        Some(DisconnectCause::AskedSet),
        Some(DisconnectCause::Busy),
        None,
    ] {
        let listener = listeners.borrow().last().unwrap().clone();
        listener(ConnectionState::Disconnected, cause);
        for _ in 0..1000 {
            if notices(&events).iter().any(|notice| notice == FAKE.vocab.cant_reach) {
                break;
            }
            delay(2).await;
        }
        listener(ConnectionState::Connected, None);
        for _ in 0..1000 {
            if session.status().state == TurnState::Idle {
                break;
            }
            delay(2).await;
        }
        session.submit("Is it back?", None).await.unwrap();
    }
    session.reconnect().await.unwrap();

    // An answer stopped while the DAW's tool ran, after a step it finished, then the next turn, which carries its note.
    main.script.borrow_mut().extend([step(|_| call("fake_read", json!({}))), step(|_| call("fake_wait", json!({})))]);
    let running = {
        let session = session.clone();
        tokio::task::spawn_local(async move { session.submit("Wait for the loop to end.", None).await })
    };
    for _ in 0..2000 {
        if started.get() {
            break;
        }
        delay(2).await;
    }
    assert!(started.get(), "the DAW's tool ran");
    session.cancel().await.unwrap();
    let _ = running.await;
    session.submit("Carry on.", None).await.unwrap();
    let _ = session.undo(None).await;
    // The worker has asked its model by now, or soon after.
    for _ in 0..500 {
        if !worker.requests.borrow().is_empty() {
            break;
        }
        delay(10).await;
    }
    session.close().await.unwrap();

    let mut said: Vec<String> = main.requests.borrow().clone();
    said.extend(worker.requests.borrow().iter().cloned());
    assert!(said.iter().any(|request| request.contains("[Kumi cleared")), "earlier results were cleared");
    assert!(said.iter().any(|request| request.contains("Stopped before finishing")), "the stopped answer's note went to the model");
    said.extend(
        main.tools
            .borrow()
            .iter()
            .map(|tool| json!({"name":tool.name(),"description":tool.description(),"inputSchema":tool.input_schema()}).to_string()),
    );
    said.extend(events.borrow().iter().map(|event| serde_json::to_string(event).unwrap()));

    // Without the DAW: the fallback's own words.
    let dir = tempfile::tempdir().unwrap();
    let (main, worker) = (Rc::new(Recorded::default()), Rc::new(Recorded::default()));
    let (session, events, _, _) = open(&main, &worker, dir.path(), true);
    session.start().await.unwrap();
    session.submit("Make the kick punchier.", None).await.unwrap();
    let _ = session.undo(None).await;
    session.close().await.unwrap();
    said.extend(main.requests.borrow().iter().cloned());
    said.extend(events.borrow().iter().map(|event| serde_json::to_string(event).unwrap()));
    said
}

#[tokio::test(flavor = "current_thread")]
async fn a_whole_session_over_another_daw_says_nothing_in_lives_words() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let said = everything_said().await;
            for (at, phrase, why) in ALLOWED {
                assert!(said.iter().any(|text| text.contains(phrase)), "{at} no longer says {phrase} ({why}): take it off the list");
            }
            assert_no_live_words(&said);
        })
        .await;
}

/// Nothing said is in Live's words, but for what ALLOWED lets through.
fn assert_no_live_words(said: &[String]) {
    let found: Vec<String> = said
        .iter()
        .map(|text| ALLOWED.iter().fold(text.clone(), |text, (_, phrase, _)| text.replace(phrase, "")))
        .flat_map(|text| {
            LIVE_WORDS
                .find_iter(&text)
                .map(|word| {
                    let (start, end) = (word.start().saturating_sub(80), (word.end() + 80).min(text.len()));
                    let (start, end) = (floor(&text, start), ceil(&text, end));
                    format!("{:?}: …{}…", word.as_str(), &text[start..end])
                })
                .collect::<Vec<_>>()
        })
        .collect();
    assert!(found.is_empty(), "{} in Live's words:\n{}", found.len(), found.join("\n"));
}

/// A session in REAPER through Kumi's host (in process, in front of the simulated REAPER): the model changes the
/// project with a plan, undoes a change, and the producer undoes another with /undo. What the model is sent (its tools,
/// the plan's and undo's answers, the observations with kumiChanges) and what the producer is told (the changes in
/// HISTORY, notices) is in REAPER's words, never Live's.
#[tokio::test(flavor = "current_thread")]
async fn a_reaper_session_changes_and_undoes_in_reapers_words() {
    use daw_bridge::{files::ensure_secret, paths::BridgePaths, Daw};
    use daw_mcp_server::{host::Host, simulator::reaper::ReaperScript, simulator::Simulator};
    use kumi_runtime::core::context::IntegrationContext;
    use kumi_runtime::integrations::{
        own_bridge::{Connect, OwnOptions},
        reaper::create_reaper_integration,
    };
    tokio::task::LocalSet::new()
        .run_until(async {
            let dir = tempfile::tempdir().unwrap();
            let paths = BridgePaths::new(dir.path(), Daw::Reaper);
            let secret = ensure_secret(&paths.secret()).unwrap();
            let script = ReaperScript::new(3, 0);
            let mut config = script.config(secret, "/lab/REAPER");
            config.discovery_dir = Some(paths.discovery());
            let simulator = Simulator::start(config).await.unwrap();
            let host = Rc::new(in_process::InProcess { host: std::sync::Arc::new(Host::new(Daw::Reaper, dir.path())), id: Cell::new(0) });
            let home = dir.path().to_path_buf();
            let changes: Rc<RefCell<Vec<ChangeRecord>>> = Rc::default();
            let into = changes.clone();
            let integration: IntegrationFactory = Box::new(move |listener: ConnectionListener| {
                let host = host.clone();
                let connect: Connect = Rc::new(move |_| {
                    let host = host.clone();
                    async move { Ok(host as Rc<dyn kumi_runtime::mcp::client::McpEndpoint>) }.boxed_local()
                });
                let mut context = IntegrationContext::new(listener);
                let into = into.clone();
                context.on_change = Some(Rc::new(move |record| into.borrow_mut().push(record)));
                create_reaper_integration(OwnOptions { context, kumi_home: home.clone(), connect: Some(connect) })
            });
            let main = Rc::new(Recorded::default());
            let events = Rc::new(RefCell::new(vec![]));
            let out = events.clone();
            let mut options =
                SessionOptions::new(recording(main.clone(), None), integration, Rc::new(move |event| out.borrow_mut().push(event)));
            options.profile = DawProfile::REAPER;
            options.timeout_ms = Some(20_000);
            options.close_timeout_ms = Some(5_000);
            options.conversations = Some(create_conversation_store(dir.path()));
            let session = create_session(options).unwrap();
            session.start().await.unwrap();

            main.script.borrow_mut().extend([
                step(|_| {
                    call(
                        "make_changes",
                        json!({"steps": [
                            {"tool": "track_volume", "input": {"ref": "track:2", "volumeDb": -6}},
                            {"tool": "track_add", "as": "pad", "input": {"name": "Pad"}},
                            {"tool": "track_rename", "input": {"ref": "@pad", "name": "Pads"}},
                        ]}),
                    )
                }),
                step(|_| call("undo_change", json!({"change": "last"}))),
                step(|_| call("reaper_read", json!({"kind": "track"}))),
                step(|_| answer("Done.")),
            ]);
            session.submit("Pull the first track down and add a pad track.", None).await.unwrap();
            assert_eq!(script.volume(1), -6.0, "the plan ran");
            assert_eq!(script.track(4)["name"], json!("Pad"), "the rename was undone");
            // The producer's /undo takes back the latest change still standing: the track Kumi added.
            let undone = session.undo(None).await.unwrap();
            assert_eq!(undone.map(|record| record.title).as_deref(), Some("Added track Pad"));
            session.submit("What changed?", None).await.unwrap();
            session.close().await.unwrap();
            simulator.stop();

            let titles: Vec<String> = changes.borrow().iter().map(|record| record.title.clone()).collect();
            assert!(titles.iter().any(|title| title == "Track 1 volume -6 dB"), "{titles:?}");
            let mut said: Vec<String> = main.requests.borrow().clone();
            assert!(said.iter().any(|request| request.contains("kumiChanges")), "the model sees its changes");
            said.extend(
                main.tools
                    .borrow()
                    .iter()
                    .map(|tool| json!({"name":tool.name(),"description":tool.description(),"inputSchema":tool.input_schema()}).to_string()),
            );
            said.extend(events.borrow().iter().map(|event| serde_json::to_string(event).unwrap()));
            said.extend(changes.borrow().iter().map(|record| serde_json::to_string(record).unwrap()));
            assert_no_live_words(&said);
        })
        .await;
}

fn floor(text: &str, mut at: usize) -> usize {
    while !text.is_char_boundary(at) {
        at -= 1;
    }
    at
}
fn ceil(text: &str, mut at: usize) -> usize {
    while !text.is_char_boundary(at) {
        at += 1;
    }
    at
}

/// FOCUS's bodies of another DAW's own (here its sequence) reach the app through the session, as Live's four always
/// did.
#[tokio::test(flavor = "current_thread")]
async fn the_session_offers_another_daws_own_focus_bodies() {
    tokio::task::LocalSet::new()
        .run_until(async {
            use kumi_runtime::core::view::{BodyKind, BodyRequest, FocusBody};
            let dir = tempfile::tempdir().unwrap();
            let (main, worker) = (Rc::new(Recorded::default()), Rc::new(Recorded::default()));
            let (session, _, _, _) = open(&main, &worker, dir.path(), false);
            session.start().await.unwrap();
            assert!(session.can_show(BodyKind::Matrix), "the DAW's own body");
            assert!(!session.can_show(BodyKind::Tracker), "one the DAW doesn't draw");
            assert!(session.can_show(BodyKind::Tree) && session.can_show(BodyKind::Timeline), "Live's four, as always");
            assert!(matches!(session.focus_body(&BodyRequest::Matrix).await.unwrap(), Some(FocusBody::Matrix(_))));
            session.close().await.unwrap();
        })
        .await;
}
