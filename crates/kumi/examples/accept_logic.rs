//! Opt-in acceptance run on the real Logic Pro: Kumi's host started beside this binary, as Kumi starts it, the bridge
//! it puts beside Logic, and the M1, M2 and M3 checks of Logic's plan against the project Logic has open. M1: the
//! status and discovery. M2: the focus's latency (median and worst over 50 calls, under 150 ms), the first look and
//! every kind read through logic_read, the four bodies, and, with an operator at Logic, the selection following, a
//! dialog counting as busy, a project switched and refs held across a track moved. M3: a plan of several changes (a
//! fader, a rename, a track added, the new track muted) through Kumi's plan tool, each in HISTORY, then each undone
//! through Kumi (the fader and the mute put back by value, the rename as a fresh step of Logic's undo, the added track
//! by its deletion); and, with the operator, a rename made through Kumi and a Cmd-Z in Logic named back by the bridge.
//! M4 (the first slice): a save as to a copy under the folder `--files` names (the project is that copy from then on,
//! so the recorded one stays as it was), a key command Logic keeps no step of (no row) and one it does (a marker, whose
//! row only Logic's own undo takes back: taken away through Kumi by its deletion), a region split and its undo, a MIDI
//! import of 64 notes read back and undone, the cycle set and cleared, a tempo change set and deleted in the Tempo
//! List, each read back, and everything it made undone through Kumi at the end, with the times.
//! It leaves the project as it found it: every change it makes is undone before it ends (the operator's own steps of
//! M2 aside). It prints a Markdown summary for docs/evidence/ at the end (and writes it to --report, when given). The
//! bridge must be set up in the home (`kumi --daw logic bridge`) and Accessibility allowed; build the host first:
//!   cargo build --release -p daw-mcp-server --bins
//!   cargo run --release -p kumi --example accept_logic -- [--kumi-home <dir>] [--files <dir>] [--report <file>] [--no-operator]
use futures::FutureExt;
use kumi_common::{
    abort::{self, Signal},
    time::perf_now,
};
use kumi_runtime::{
    core::{
        context::IntegrationContext,
        contracts::{ChangeRecord, ChangeState},
        view::BodyRequest,
    },
    integrations::{
        logic::create_logic_integration,
        own_bridge::{Connect, OwnOptions},
    },
    mcp::{
        client::{connect_mcp, BridgeDescriptor, McpEndpoint, Options},
        types::{CallToolResult, ContentBlock},
    },
    system::Env,
    Integration, JsonObject, Observation, RuntimeError,
};
use serde_json::{json, Value};
use std::{
    cell::{Cell, RefCell},
    future::Future,
    path::{Path, PathBuf},
    rc::Rc,
    time::Duration,
};

/// The focus answers from a cache: this is the most a call may take, median and worst alike.
const FOCUS_LIMIT_MS: f64 = 150.0;
const FOCUS_CALLS: usize = 50;
/// How long each step with the operator waits for them.
const OPERATOR_MS: u64 = 90_000;

fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let after = |flag: &str| argv.iter().position(|arg| arg == flag).and_then(|at| argv.get(at + 1)).cloned();
    if argv.iter().any(|arg| arg == "--help" || arg == "-h") {
        eprintln!(
            "cargo run --release -p kumi --example accept_logic -- [--kumi-home <dir>] [--files <dir>] [--report <file>] [--no-operator]"
        );
        std::process::exit(2);
    }
    let env: Env = std::env::vars().collect();
    let kumi_home = after("--kumi-home").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(kumi::config::kumi_dir(&env)));
    let report = after("--report").map(PathBuf::from);
    // The folder the save as goes under (M4): never the recorded project's own folder.
    let files = after("--files").map(PathBuf::from);
    let operator = !argv.iter().any(|arg| arg == "--no-operator");
    let (host, build) = host_program();
    if !host.is_file() {
        eprintln!("Kumi's host isn't built ({}); build it first: {build}", host.display());
        std::process::exit(2);
    }
    let runtime = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("accept-logic: {error}");
            std::process::exit(1);
        }
    };
    let local = tokio::task::LocalSet::new();
    let code = local.block_on(&runtime, accept_logic(kumi_home, host, operator, report, files));
    std::process::exit(code);
}

/// The host this run starts, and the command that builds it. Kumi starts `daw-mcp-server` beside its own binary;
/// Cargo puts an example one folder further down, in `examples`.
fn host_program() -> (PathBuf, String) {
    let name = if cfg!(windows) { "daw-mcp-server.exe" } else { "daw-mcp-server" };
    let mut folder = std::env::current_exe().ok().and_then(|exe| exe.parent().map(Path::to_path_buf)).unwrap_or_default();
    if folder.file_name().is_some_and(|name| name == "examples") {
        folder.pop();
    }
    let profile = match folder.file_name().and_then(|name| name.to_str()) {
        Some("release") => " --release".to_owned(),
        Some(name) if name != "debug" => format!(" --profile {name}"),
        _ => String::new(),
    };
    (folder.join(name), format!("cargo build{profile} -p daw-mcp-server --bins"))
}

/// One check's row in the summary.
struct Row {
    ok: bool,
    ms: Option<f64>,
    what: String,
}

/// A host tool's answer: its JSON (null when it isn't JSON), whether it was an error, and the error's code.
struct Answer {
    ok: bool,
    ms: f64,
    body: Value,
}

impl Answer {
    fn code(&self) -> Option<&str> {
        self.body.get("code").and_then(Value::as_str)
    }
    fn error(&self) -> String {
        self.body.get("error").and_then(Value::as_str).map(str::to_owned).unwrap_or_else(|| short(&self.body.to_string(), 200))
    }
}

/// The bridge's events as the host forwards them, in order: their names and data.
type Events = Rc<RefCell<Vec<(String, Value)>>>;

/// One run: the host's endpoint (Kumi's integration shares it), the integration, the events heard, and the rows.
struct Accept {
    endpoint: Rc<dyn McpEndpoint>,
    integration: Rc<dyn Integration>,
    events: Events,
    rows: RefCell<Vec<Row>>,
    observation: RefCell<Option<Observation>>,
    stopping: Rc<Cell<bool>>,
    operator: bool,
    /// What the status said, for the summary's header.
    status: RefCell<Value>,
    /// Kumi's changes as HISTORY hears them: each record as it is first told, and again as its state moves.
    changes: Rc<RefCell<Vec<ChangeRecord>>>,
    /// Where the save as of M4 writes its copy, when the run was given a folder.
    files: Option<PathBuf>,
}

fn signal() -> Signal {
    abort::timeout(60_000)
}

fn short(text: &str, most: usize) -> String {
    let text: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if text.chars().count() <= most {
        text
    } else {
        format!("{}…", text.chars().take(most).collect::<String>())
    }
}

fn seconds(ms: f64) -> String {
    if ms < 1000.0 {
        format!("{} ms", ms.round())
    } else {
        format!("{:.1} s", ms / 1000.0)
    }
}

/// One of an observation's tools, by name.
fn tool_named(observation: &Observation, name: &str) -> Option<Rc<dyn kumi_runtime::core::contracts::KernelTool>> {
    observation.tools.iter().find(|tool| tool.name() == name).cloned()
}

/// A change's state as HISTORY words it.
fn state_word(state: ChangeState) -> &'static str {
    match state {
        ChangeState::Applied => "applied",
        ChangeState::Undone => "undone",
        ChangeState::Kept => "kept",
        ChangeState::Unsure => "unsure",
        ChangeState::Expired => "expired",
        ChangeState::Heard => "heard",
    }
}

/// The JSON a tool answered with, from its text blocks.
fn body_of(result: &CallToolResult) -> Value {
    let text = result
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    serde_json::from_str(&text).unwrap_or(Value::String(text))
}

async fn accept_logic(kumi_home: PathBuf, host: PathBuf, operator: bool, report: Option<PathBuf>, files: Option<PathBuf>) -> i32 {
    let stopping = Rc::new(Cell::new(false));
    tokio::task::spawn_local({
        let stopping = stopping.clone();
        async move {
            if tokio::signal::ctrl_c().await.is_ok() {
                stopping.set(true);
                println!("\nStopping after this check…");
                if tokio::signal::ctrl_c().await.is_ok() {
                    std::process::exit(130);
                }
            }
        }
    });
    println!("Kumi's host for Logic Pro, from {} with the home {}", host.display(), kumi_home.display());
    let t0 = perf_now();
    let endpoint = match connect_mcp(Options {
        signal: signal(),
        entry: Some(host.clone()),
        args: vec!["--daw".into(), "logic".into(), "--kumi-home".into(), kumi_home.to_string_lossy().into_owned()],
        timeout_ms: Some(30_000),
        cwd: host.parent().map(Path::to_path_buf),
        bridge: Some(BridgeDescriptor::LOGIC),
        ..Default::default()
    })
    .await
    {
        Ok(endpoint) => endpoint,
        Err(error) => {
            eprintln!("accept-logic: the host didn't start: {}", short(&error.message(), 300));
            return 1;
        }
    };
    println!("  ok   {:>8}  the host started", seconds(perf_now() - t0));
    let events: Events = Rc::default();
    let _listening = endpoint.on_live_event(Rc::new({
        let events = events.clone();
        move |event| {
            let name = event.get("name").and_then(Value::as_str).unwrap_or_default().to_owned();
            let data = event.get("data").cloned().unwrap_or(Value::Null);
            events.borrow_mut().push((name, data));
        }
    }));
    // Kumi's integration over the same host, as the app makes it: its looks, read tool and bodies are the real ones.
    // Nothing draws the focus here, so the integration doesn't poll it: the latency below is this run's alone.
    let shared = endpoint.clone();
    let connect: Connect = Rc::new(move |_| {
        let endpoint = shared.clone();
        async move { Ok(endpoint) }.boxed_local()
    });
    let mut context = IntegrationContext::new(Rc::new(|_, _| {}));
    let changes: Rc<RefCell<Vec<ChangeRecord>>> = Rc::default();
    let into = changes.clone();
    context.on_change = Some(Rc::new(move |record| into.borrow_mut().push(record)));
    let integration = create_logic_integration(OwnOptions { context, kumi_home, connect: Some(connect), open_file: None });
    let accept = Accept {
        endpoint,
        integration,
        events,
        rows: RefCell::default(),
        observation: RefCell::default(),
        stopping,
        operator,
        status: RefCell::new(Value::Null),
        changes,
        files,
    };
    let code = accept.accept().await.unwrap_or_else(|error| {
        eprintln!("accept-logic: {}", short(&error.message(), 300));
        1
    });
    let summary = accept.summary();
    println!("\n{summary}");
    if let Some(report) = report {
        match std::fs::write(&report, &summary) {
            Ok(()) => println!("Written to {}", report.display()),
            Err(error) => eprintln!("accept-logic: couldn't write {}: {error}", report.display()),
        }
    }
    let _ = accept.integration.close().await;
    code
}

impl Accept {
    fn say(&self, ok: bool, ms: Option<f64>, what: &str) {
        println!("  {} {:>8}  {what}", if ok { "ok  " } else { "FAIL" }, ms.map(seconds).unwrap_or_default());
        self.rows.borrow_mut().push(Row { ok, ms, what: what.into() });
    }

    /// A host tool, asked directly: what Kumi's integration asks of the host, without the turn's short refs.
    async fn tool(&self, name: &str, arguments: Value) -> Answer {
        let args: JsonObject = arguments.as_object().cloned().unwrap_or_default();
        let t0 = perf_now();
        match self.endpoint.call(name, args, signal()).await {
            Ok(result) => Answer { ok: result.is_error != Some(true), ms: perf_now() - t0, body: body_of(&result) },
            Err(error) => Answer { ok: false, ms: perf_now() - t0, body: json!({"error": error.message()}) },
        }
    }

    /// The tempo at `beats` in Logic's Tempo List, read back through the bridge: a delete previewed there (never
    /// applied) finds the event and its BPM, or says there is none; `Err` when the preview failed otherwise.
    async fn tempo_listed(&self, beats: f64) -> Result<Option<f64>, String> {
        let epoch = self.tool("read", json!({"kind": "project"})).await.body["epoch"].clone();
        let preview =
            self.tool("change_preview", json!({"kind": "tempo-marker-delete", "input": {"positionBeats": beats}, "epoch": epoch})).await;
        if preview.ok {
            return Ok(preview.body["prior"]["bpm"].as_f64());
        }
        let error = preview.error();
        if error.contains("there is no tempo event") {
            Ok(None)
        } else {
            Err(short(&error, 160))
        }
    }

    async fn observe(&self) -> Result<Observation, RuntimeError> {
        let observation = self.integration.observe(signal(), None).await?;
        *self.observation.borrow_mut() = Some(observation.clone());
        Ok(observation)
    }

    /// Waits for the operator (or the bridge) until `done` answers, or the time is up: what `done` answered.
    async fn wait_for<T, F, Fut>(&self, mut done: F) -> Option<T>
    where
        F: FnMut() -> Fut,
        Fut: Future<Output = Option<T>>,
    {
        let deadline = tokio::time::Instant::now() + Duration::from_millis(OPERATOR_MS);
        while tokio::time::Instant::now() < deadline && !self.stopping.get() {
            if let Some(found) = done().await {
                return Some(found);
            }
            tokio::time::sleep(Duration::from_millis(150)).await;
        }
        done().await
    }

    /// The events heard since `from`, by name.
    fn events_since(&self, from: usize, name: &str) -> Vec<Value> {
        self.events.borrow().iter().skip(from).filter(|(heard, _)| heard == name).map(|(_, data)| data.clone()).collect()
    }

    async fn accept(&self) -> Result<i32, RuntimeError> {
        println!("\nM1: the status and discovery");
        let status = self.tool("status", json!({})).await;
        *self.status.borrow_mut() = status.body.clone();
        let connected = status.ok && status.body["connected"] == true;
        let project = status.body["project"]["name"].as_str().unwrap_or("none");
        let what = if connected {
            format!(
                "status: Logic Pro {} (pid {}, {}), bridge {}, project “{project}”, busy {}, permission {}",
                status.body["dawVersion"].as_str().unwrap_or("?"),
                status.body["pid"],
                status.body["resource"].as_str().unwrap_or("?"),
                status.body["bridge"].as_str().unwrap_or("?"),
                status.body["busy"],
                status.body["permission"]
            )
        } else {
            format!("status: not connected: {}", status.error())
        };
        self.say(connected, Some(status.ms), &what);
        if !connected {
            return Ok(1);
        }
        if status.body["permission"].as_str().is_some() {
            self.say(false, None, "Accessibility isn't allowed to Kumi's helper: the reads below will fail in words");
        }
        let discover = self.tool("discover", json!({})).await;
        let instances = discover.body["instances"].as_array().map_or(0, Vec::len);
        self.say(discover.ok && instances >= 1, Some(discover.ms), &format!("discover: {instances} Logic Pro instance(s)"));

        println!("\nM2: the focus's latency");
        // The bridge fills its focus cache on its first pass over the project after attaching: the latency is measured
        // from the first answer on, not against that pass.
        let warm = perf_now();
        loop {
            let focus = self.tool("focus", json!({})).await;
            if focus.ok || perf_now() - warm > 10_000.0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        let mut times = Vec::with_capacity(FOCUS_CALLS);
        let mut last = Value::Null;
        for _ in 0..FOCUS_CALLS {
            let focus = self.tool("focus", json!({})).await;
            if !focus.ok {
                self.say(false, Some(focus.ms), &format!("focus: {}", focus.error()));
                break;
            }
            times.push(focus.ms);
            last = focus.body;
        }
        if times.len() == FOCUS_CALLS {
            times.sort_by(|a, b| a.total_cmp(b));
            let (median, worst) = (times[FOCUS_CALLS / 2], times[FOCUS_CALLS - 1]);
            let what = format!(
                "focus over {FOCUS_CALLS} calls: median {}, worst {} (under {} ms); selection {}, editor {}, ageMs {}",
                seconds(median),
                seconds(worst),
                FOCUS_LIMIT_MS,
                last["selection"]["trackRef"].as_str().unwrap_or("none"),
                last["editor"].as_str().unwrap_or("none"),
                last["ageMs"]
            );
            self.say(median < FOCUS_LIMIT_MS && worst < FOCUS_LIMIT_MS, Some(median), &what);
        }

        println!("\nM2: the first look, every kind, the bodies");
        let t0 = perf_now();
        let observation = self.observe().await?;
        let context: Value = serde_json::from_str(&observation.context).unwrap_or(Value::Null);
        let tracks = context["tracks"].as_array().map_or(0, Vec::len);
        let markers = context["markers"].as_array().map_or(0, Vec::len);
        let looked = observation.tools.iter().any(|tool| tool.name() == "logic_read");
        self.say(
            looked && tracks > 0,
            Some(perf_now() - t0),
            &format!(
                "first look: “{}”, {tracks} tracks, {markers} markers, selection {}, place {}",
                observation.label,
                context["selection"]["trackRef"].as_str().unwrap_or("none"),
                observation.project.as_ref().map(|project| project.id.as_str()).unwrap_or("unsaved")
            ),
        );
        let open = self.integration.open_set();
        self.say(open.is_some(), None, &format!("open_set: {}", open.as_deref().unwrap_or("none (the project isn't saved)")));
        let recording = self.integration.recording(signal()).await?;
        self.say(recording.is_some(), None, &format!("recording: {recording:?} (from the focus's transport)"));
        let selected_track = context["selection"]["trackRef"].as_str().map(str::to_owned);
        let selected_region = context["selection"]["regionRef"].as_str().map(str::to_owned);
        // The look's refs are the model's short ones; FOCUS's bodies and the selection check take the focus's long ones.
        let focus_track = last["selection"]["trackRef"].as_str().map(str::to_owned);
        let focus_region = last["selection"]["regionRef"].as_str().map(str::to_owned);
        let read = observation.tools.iter().find(|tool| tool.name() == "logic_read").cloned();
        if let Some(read) = read {
            let kinds: Vec<(&str, Option<&str>)> = vec![
                ("project", None),
                ("track", None),
                ("region", None),
                ("note", selected_region.as_deref()),
                ("plugin", selected_track.as_deref()),
                ("send", selected_track.as_deref()),
                ("marker", None),
                ("tempo", None),
            ];
            for (kind, parent) in kinds {
                let mut input = json!({"kind": kind});
                match parent {
                    Some(parent) => input["parent"] = json!(parent),
                    None if matches!(kind, "note") => {
                        self.say(false, None, "read note: no region is selected to read the notes of");
                        continue;
                    }
                    None => {}
                }
                let t0 = perf_now();
                let result = read.execute(input.as_object().cloned().unwrap_or_default(), signal()).await?;
                let page: Value = serde_json::from_str(&result.text).unwrap_or(Value::Null);
                let rows = page["items"].as_array().map_or(0, Vec::len);
                let what = if result.is_error {
                    format!("read {kind}: {}", short(&result.text, 200))
                } else {
                    format!("read {kind}: {rows} rows (total {}, epoch {})", page["total"], page["epoch"])
                };
                self.say(!result.is_error, Some(perf_now() - t0), &what);
            }
        }
        let bodies: Vec<(&str, Option<BodyRequest>)> = vec![
            ("tree", focus_track.clone().map(|track_ref| BodyRequest::Tree { track_ref })),
            ("notes", focus_region.clone().map(|item_ref| BodyRequest::Notes { item_ref })),
            ("timeline", Some(BodyRequest::Timeline)),
            ("routing", focus_track.clone().map(|track_ref| BodyRequest::Routing { track_ref })),
        ];
        for (name, request) in bodies {
            let Some(request) = request else {
                self.say(false, None, &format!("body {name}: nothing is selected to show it for"));
                continue;
            };
            let t0 = perf_now();
            let body = self.integration.focus_body(&request, signal()).await?;
            let what = match &body {
                Some(body) => format!("body {name}: {}", short(&format!("{body:?}"), 160)),
                None => format!("body {name}: nothing came back"),
            };
            self.say(body.is_some(), Some(perf_now() - t0), &what);
        }
        if self.stopping.get() {
            return Ok(self.exit_code());
        }

        println!("\nM3: a plan of changes, each undone through Kumi");
        self.plan_and_undo().await?;
        if self.stopping.get() {
            return Ok(self.exit_code());
        }
        println!("\nM4: a save as, key commands, a region split, a MIDI import, the cycle and the Tempo List, each undone");
        self.m4_kinds().await?;
        if !self.operator || self.stopping.get() {
            println!("\n(the steps with an operator at Logic Pro were skipped)");
            return Ok(self.exit_code());
        }

        println!("\nM2: with you at Logic Pro");
        self.select_follows(focus_track.as_deref()).await;
        if self.stopping.get() {
            return Ok(self.exit_code());
        }
        self.modal_busy().await;
        if self.stopping.get() {
            return Ok(self.exit_code());
        }
        println!("\nM3: with you at Logic Pro");
        self.cmd_z_named().await?;
        if self.stopping.get() {
            return Ok(self.exit_code());
        }
        println!("\nM2: with you at Logic Pro, the project changed");
        self.refs_hold_across_a_track_move().await;
        if self.stopping.get() {
            return Ok(self.exit_code());
        }
        self.project_switch(observation.project.as_ref().map(|project| project.id.clone())).await?;
        Ok(self.exit_code())
    }

    /// The operator selects another track: the focus follows, and a `selection` event comes.
    async fn select_follows(&self, before: Option<&str>) {
        println!("  → In Logic Pro, select another track (within {} s)…", OPERATOR_MS / 1000);
        // The selection as it stands now, by its place: refs carry the epoch, which the plan before this moved.
        let place = |track_ref: &str| track_ref.splitn(2, ':').nth(1).unwrap_or(track_ref).to_owned();
        let fresh = self.tool("focus", json!({})).await;
        let before: Option<String> = fresh.body["selection"]["trackRef"].as_str().map(str::to_owned).or_else(|| before.map(str::to_owned));
        let before_place = before.as_deref().map(place);
        let before = before.as_deref();
        let heard = self.events.borrow().len();
        let t0 = perf_now();
        let seen = self
            .wait_for(|| {
                let before_place = before_place.clone();
                async move {
                    let focus = self.tool("focus", json!({})).await;
                    let now = focus.body["selection"]["trackRef"].as_str().map(str::to_owned);
                    (focus.ok && now.is_some() && now.as_deref().map(place) != before_place).then_some(now.unwrap_or_default())
                }
            })
            .await;
        let events = self.events_since(heard, "selection");
        match seen {
            Some(now) => self.say(true, Some(perf_now() - t0), &format!("the focus follows: {} → {now}", before.unwrap_or("none"))),
            None => self.say(false, Some(perf_now() - t0), "the focus didn't follow a selection (none made, or not seen)"),
        }
        self.say(!events.is_empty(), None, &format!("selection events: {} ({})", events.len(), short(&json!(events).to_string(), 120)));
    }

    /// The operator opens a dialog (Cmd-Shift-S) and closes it (Escape): the status says modal, a look is refused as
    /// busy, and the next one succeeds.
    async fn modal_busy(&self) {
        println!("  → In Logic Pro, press Cmd-Shift-S (Save As…), wait for the ok below, then press Escape…");
        let t0 = perf_now();
        let modal = self
            .wait_for(|| async move {
                let status = self.tool("status", json!({})).await;
                (status.body["busy"] == "modal").then_some(())
            })
            .await;
        self.say(modal.is_some(), Some(perf_now() - t0), "status says busy: modal while the dialog is up");
        if modal.is_none() {
            return;
        }
        let refused = self.tool("snapshot", json!({})).await;
        self.say(
            !refused.ok && refused.code() == Some("busy"),
            Some(refused.ms),
            &format!("a look is refused as busy: {}", refused.error()),
        );
        println!("  → now press Escape in Logic Pro…");
        let t0 = perf_now();
        let free = self
            .wait_for(|| async move {
                let status = self.tool("status", json!({})).await;
                (status.body["busy"] == false).then_some(())
            })
            .await;
        self.say(free.is_some(), Some(perf_now() - t0), "status says free again once the dialog is closed");
        let look = self.tool("snapshot", json!({})).await;
        self.say(look.ok, Some(look.ms), &format!("the next look succeeds: {} tracks", look.body["tracks"].as_array().map_or(0, Vec::len)));
    }

    /// The operator adds or moves a track: the epoch moves, a ref from before is refused as stale, and a fresh read
    /// carries the new epoch.
    async fn refs_hold_across_a_track_move(&self) {
        let before = self.tool("read", json!({"kind": "track"})).await;
        let Some(reference) = before.body["items"][0]["ref"].as_str().map(str::to_owned) else {
            self.say(false, Some(before.ms), &format!("read track before the move: {}", before.error()));
            return;
        };
        let epoch = before.body["epoch"].as_str().unwrap_or_default().to_owned();
        println!("  → In Logic Pro, add a track (Option-Cmd-N) or drag one to another place…");
        let heard = self.events.borrow().len();
        let t0 = perf_now();
        let epoch = &epoch;
        let moved = self
            .wait_for(|| async move {
                let page = self.tool("read", json!({"kind": "track"})).await;
                let now = page.body["epoch"].as_str().unwrap_or_default().to_owned();
                (page.ok && now != *epoch).then_some(now)
            })
            .await;
        let tracks_events = self.events_since(heard, "tracks").len();
        match moved {
            Some(now) => {
                self.say(true, Some(perf_now() - t0), &format!("the epoch moved: {epoch} → {now} ({tracks_events} tracks events)"))
            }
            None => {
                self.say(false, Some(perf_now() - t0), "the epoch didn't move (no track added or moved, or not seen)");
                return;
            }
        }
        let stale = self.tool("read", json!({"ref": reference})).await;
        self.say(
            !stale.ok && stale.code() == Some("stale"),
            Some(stale.ms),
            &format!("a ref from before ({reference}) is refused as stale: {}", stale.error()),
        );
        let fresh = self.tool("read", json!({"kind": "track"})).await;
        let first = fresh.body["items"][0]["ref"].as_str().unwrap_or("none");
        self.say(
            fresh.ok && first.starts_with(&format!("{}:", fresh.body["epoch"].as_str().unwrap_or("?"))),
            Some(fresh.ms),
            &format!("a fresh read carries the new epoch: {first}"),
        );
    }

    /// The operator opens another project: a `project` event comes, and the next look is of another place.
    async fn project_switch(&self, place: Option<String>) -> Result<(), RuntimeError> {
        println!("  → In Logic Pro, open another project (File > Open Recent…)…");
        let heard = self.events.borrow().len();
        let t0 = perf_now();
        let event = self.wait_for(|| async move { self.events_since(heard, "project").into_iter().next() }).await;
        match &event {
            Some(data) => self.say(true, Some(perf_now() - t0), &format!("project event: {}", short(&data.to_string(), 160))),
            None => {
                self.say(false, Some(perf_now() - t0), "no project event (no project opened, or not seen)");
                return Ok(());
            }
        }
        // The bridge may still be loading: the look waits for it as Kumi's does.
        tokio::time::sleep(Duration::from_millis(1_000)).await;
        let t0 = perf_now();
        let observation = self.observe().await?;
        let now = observation.project.as_ref().map(|project| project.id.clone());
        self.say(
            now != place && observation.tools.iter().any(|tool| tool.name() == "logic_read"),
            Some(perf_now() - t0),
            &format!(
                "the look after is of another place: {} → {}; “{}”",
                place.as_deref().unwrap_or("unsaved"),
                now.as_deref().unwrap_or("unsaved"),
                observation.label
            ),
        );
        let open = self.integration.open_set();
        self.say(true, None, &format!("open_set now: {}", open.as_deref().unwrap_or("none")));
        Ok(())
    }

    /// A change as HISTORY last told of it.
    fn change(&self, id: &str) -> Option<ChangeRecord> {
        self.changes.borrow().iter().rev().find(|record| record.id == id).cloned()
    }

    /// M3: a plan of several changes through Kumi's own plan tool (the first track's fader, the second track renamed,
    /// a track added below it, that track muted), each in HISTORY with undo; then each undone through Kumi, last first:
    /// the mute and the fader put back by value, the added track by its deletion, the rename by value as a fresh step
    /// of Logic's undo. The project reads as before when it's done.
    async fn plan_and_undo(&self) -> Result<(), RuntimeError> {
        let observation = self.observe().await?;
        let context: Value = serde_json::from_str(&observation.context).unwrap_or(Value::Null);
        let tracks = context["tracks"].as_array().cloned().unwrap_or_default();
        let (Some(first), Some(second)) = (tracks.first().cloned(), tracks.get(1).or(tracks.first()).cloned()) else {
            self.say(false, None, "plan: the project has no track to change");
            return Ok(());
        };
        let (Some(plan), Some(undo)) = (tool_named(&observation, "make_changes"), tool_named(&observation, "undo_change")) else {
            self.say(false, None, "plan: no make_changes and undo_change tools (Logic Pro's change set is empty)");
            return Ok(());
        };
        let volume = first["volumeDb"].as_f64().unwrap_or(0.0);
        // The fader moved down 6 dB (up 3 when it is low already), the second track's name marked, a track added below
        // the last one and muted.
        let target = if volume > -60.0 { volume - 6.0 } else { volume + 3.0 };
        let (first_name, second_name) =
            (first["name"].as_str().unwrap_or("?").to_owned(), second["name"].as_str().unwrap_or("?").to_owned());
        let steps = json!({"steps": [
            {"tool": "track_volume", "input": {"ref": first["ref"], "volumeDb": target}},
            {"tool": "track_rename", "input": {"ref": second["ref"], "name": format!("{second_name} (Kumi)")}},
            {"tool": "track_add", "as": "pad", "input": {"name": "Kumi Pad", "kind": "instrument", "index": tracks.len().max(1)}},
            {"tool": "track_mute", "input": {"ref": "@pad", "mute": true}},
        ]});
        let heard = self.changes.borrow().len();
        let t0 = perf_now();
        let done = plan.execute(steps.as_object().cloned().unwrap_or_default(), signal()).await?;
        let ms = perf_now() - t0;
        // The changes HISTORY was told of by this plan, in order, each as it stands now.
        let mut ids: Vec<String> = Vec::new();
        for record in self.changes.borrow()[heard..].iter() {
            if !ids.contains(&record.id) {
                ids.push(record.id.clone());
            }
        }
        let rows: Vec<ChangeRecord> = ids.iter().filter_map(|id| self.change(id)).collect();
        let applied = rows.iter().filter(|row| row.state == ChangeState::Applied).count();
        let listed: Vec<String> = rows.iter().map(|row| format!("“{}” {}", row.title, state_word(row.state))).collect();
        let stopped = if done.is_error { format!("; the plan stopped: {}", short(&done.text, 1500)) } else { String::new() };
        self.say(
            !done.is_error && applied == 4,
            Some(ms),
            &format!("a plan of 4 changes: {} in HISTORY ({}){stopped}", rows.len(), listed.join("; ")),
        );
        // Each undone through Kumi, last first: the mute and the fader put back by value, the added track by its deletion,
        // the rename by value as a fresh step of Logic's undo.
        for row in rows.iter().rev().filter(|row| matches!(row.state, ChangeState::Applied | ChangeState::Unsure)) {
            let t0 = perf_now();
            let result = undo.execute(json!({"change": row.id}).as_object().cloned().unwrap_or_default(), signal()).await?;
            let undone = !result.is_error && self.change(&row.id).is_some_and(|now| now.state == ChangeState::Undone);
            let what = if undone { "undone".to_string() } else { short(&result.text, 200) };
            self.say(undone, Some(perf_now() - t0), &format!("undo “{}”: {what}", row.title));
        }
        // The project as it was, read again. Right after a structural change the bridge may still be settling its epoch
        // and refuse the look as busy, which Kumi's session answers by looking again once it's free: so does this.
        let t0 = perf_now();
        let mut context = Value::Null;
        for _ in 0..20 {
            let after = self.observe().await?;
            context = serde_json::from_str(&after.context).unwrap_or(Value::Null);
            if context["tracks"].as_array().is_some_and(|tracks| !tracks.is_empty()) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        let waited = perf_now() - t0;
        let now = context["tracks"].as_array().cloned().unwrap_or_default();
        let volume_now = now.first().and_then(|track| track["volumeDb"].as_f64());
        let second_now = now.get(1).and_then(|track| track["name"].as_str()).unwrap_or("?").to_owned();
        let back = now.len() == tracks.len() && volume_now.is_some_and(|db| (db - volume).abs() < 0.05) && second_now == second_name;
        self.say(
            back,
            Some(waited),
            &format!(
                "the project reads as before the plan: {} tracks, “{first_name}” at {} dB, “{second_now}”",
                now.len(),
                volume_now.map(|db| db.to_string()).unwrap_or_else(|| "?".into())
            ),
        );
        Ok(())
    }

    /// A change tool of the observation run with `input`, by its name, or, for a kind that is a make_changes step and no
    /// tool of its own (the key command, the cycle), a plan of that one step: whether it answered an error, and its text.
    async fn run_tool(&self, observation: &Observation, name: &str, input: Value) -> Result<(bool, String), RuntimeError> {
        let (tool, arguments) = match tool_named(observation, name) {
            Some(tool) => (tool, input),
            None => match tool_named(observation, "make_changes") {
                Some(plan) => (plan, json!({"steps": [{"tool": name, "input": input}]})),
                None => return Ok((true, format!("no {name} tool, and no make_changes to run it as a step"))),
            },
        };
        let result = tool.execute(arguments.as_object().cloned().unwrap_or_default(), signal()).await?;
        Ok((result.is_error, result.text))
    }

    /// How many rows a kind has, read through the host (0 when the read fails).
    async fn count(&self, kind: &str) -> usize {
        self.tool("read", json!({"kind": kind, "limit": 1})).await.body["total"].as_u64().unwrap_or(0) as usize
    }

    /// The latest change HISTORY heard of since `from`, by its title's start.
    fn change_since(&self, from: usize, title_start: &str) -> Option<ChangeRecord> {
        let heard = self.changes.borrow();
        heard[from.min(heard.len())..].iter().rev().find(|record| record.title.starts_with(title_start)).cloned()
    }

    /// M4, the first slice: a save as to a copy under the folder the run was given (the project is that copy from then
    /// on, so the recorded one stays as it was), a key command Logic keeps no step of (no row) and one it does (a marker
    /// at the playhead, whose row only Logic's own undo takes back: Kumi's undo is refused in Logic's words, and the
    /// marker is taken away through Kumi by its deletion), a region split and its undo, a MIDI import of 64 notes read
    /// back and undone, the cycle set and cleared, a tempo change set and deleted in the Tempo List, each read back;
    /// then everything it made undone through Kumi, last first, and the project read as before.
    async fn m4_kinds(&self) -> Result<(), RuntimeError> {
        let observation = self.observe().await?;
        let (Some(plan), Some(undo)) = (tool_named(&observation, "make_changes"), tool_named(&observation, "undo_change")) else {
            self.say(false, None, "M4: no make_changes and undo_change tools");
            return Ok(());
        };
        let context: Value = serde_json::from_str(&observation.context).unwrap_or(Value::Null);
        let tracks = context["tracks"].as_array().cloned().unwrap_or_default();
        let before = (tracks.len(), self.count("region").await, self.count("marker").await);
        let heard = self.changes.borrow().len();
        // The changes that only tidy up after a key command: not undone at the end.
        let mut tidied: Vec<String> = Vec::new();
        let run_plan = |steps: Value| plan.execute(json!({"steps": steps}).as_object().cloned().unwrap_or_default(), signal());

        // A save as under the folder given: the project is the copy from then on, read back from the host.
        match &self.files {
            None => self.say(true, None, "save as: skipped (no --files folder was given)"),
            Some(folder) => {
                let _ = std::fs::create_dir_all(folder);
                let name = self.status.borrow()["project"]["name"].as_str().unwrap_or("Project").to_owned();
                let copy = folder.join(format!("{name} Kumi M4 {}.logicx", chrono::Utc::now().format("%Y%m%d-%H%M%S")));
                let copy = copy.to_string_lossy().into_owned();
                let t0 = perf_now();
                let done = run_plan(json!([{"tool": "save_project", "input": {"path": copy}}])).await?;
                let ms = perf_now() - t0;
                let status = self.tool("status", json!({})).await;
                let project = self.tool("read", json!({"kind": "project"})).await;
                let paths = [status.body["project"]["path"].as_str(), project.body["items"][0]["path"].as_str()];
                let named = paths.into_iter().flatten().any(|path| path == copy);
                let what = if done.is_error {
                    format!("save as {copy}: {}", short(&done.text, 200))
                } else if named {
                    format!("save as {copy}: the project is the copy now (status and read)")
                } else {
                    format!(
                        "save as {copy}: saved, but the project reads as {} (status), {} (read)",
                        paths[0].unwrap_or("?"),
                        paths[1].unwrap_or("?")
                    )
                };
                self.say(!done.is_error && named, Some(ms), &what);
            }
        }
        if self.stopping.get() {
            return Ok(());
        }

        // A key command Logic keeps no step of: no row.
        let rows = self.changes.borrow().len();
        let t0 = perf_now();
        let (is_error, text) = self.run_tool(&observation, "key_command", json!({"command": "Select All"})).await?;
        let no_row = self.changes.borrow().len() == rows;
        let what = if is_error {
            format!("key command Select All: {}", short(&text, 200))
        } else if no_row {
            "key command Select All: no row (Logic Pro kept no step of it)".into()
        } else {
            format!("key command Select All: a row was made: {}", short(&text, 200))
        };
        self.say(!is_error && no_row, Some(perf_now() - t0), &what);

        // One it does: a marker at the playhead, a bar past every region; its row is Logic's own undo's, so Kumi's undo
        // of it is refused and the marker is taken away through Kumi by its deletion.
        let regions = self.tool("read", json!({"kind": "region", "limit": 200})).await;
        let last_end =
            regions.body["items"].as_array().into_iter().flatten().filter_map(|region| region["endBeats"].as_f64()).fold(0.0, f64::max);
        let at = ((last_end / 4.0).floor() + 2.0) * 4.0;
        let moved = run_plan(json!([{"tool": "set_cursor", "input": {"positionBeats": at}}])).await?;
        let markers = self.count("marker").await;
        let rows = self.changes.borrow().len();
        let t0 = perf_now();
        let (is_error, text) = self.run_tool(&observation, "key_command", json!({"command": "Create Marker"})).await?;
        let ms = perf_now() - t0;
        let markers_now = self.count("marker").await;
        let made = self.change_since(rows, "Ran key command");
        let what = if is_error || moved.is_error {
            format!("key command Create Marker at beat {at}: {}", short(if is_error { &text } else { &moved.text }, 200))
        } else {
            format!(
                "key command Create Marker at beat {at}: {markers} → {markers_now} markers, row {}",
                made.as_ref().map(|row| format!("“{}” {}", row.title, state_word(row.state))).unwrap_or_else(|| "missing".into())
            )
        };
        self.say(!is_error && !moved.is_error && markers_now == markers + 1 && made.is_some(), Some(ms), &what);
        if let Some(made) = made {
            let t0 = perf_now();
            let refused = undo.execute(json!({"change": made.id}).as_object().cloned().unwrap_or_default(), signal()).await?;
            let kept = self.change(&made.id).is_some_and(|row| row.state == ChangeState::Kept);
            self.say(
                refused.is_error && refused.text.contains("Logic Pro's own undo") && kept,
                Some(perf_now() - t0),
                &format!("Kumi's undo of the key command is refused, the row kept: {}", short(&refused.text, 200)),
            );
            // The marker it made, found by its place, taken away through Kumi.
            let page = self.tool("read", json!({"kind": "marker", "limit": 200})).await;
            let marker = page.body["items"].as_array().into_iter().flatten().find(|row| {
                let bar = row["position"]["bar"].as_f64().unwrap_or(0.0);
                let beat = row["position"]["beat"].as_f64().unwrap_or(0.0);
                ((bar - 1.0) * 4.0 + (beat - 1.0) - at).abs() < 0.5
            });
            match marker.and_then(|row| row["ref"].as_str().map(str::to_owned)) {
                Some(marker_ref) => {
                    let look = self.observe().await?;
                    let marker_ref = serde_json::from_str::<Value>(&look.context).ok().and_then(|context| {
                        context["markers"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .find(|row| {
                                row["ref"]
                                    .as_str()
                                    .is_some_and(|short| marker_ref.ends_with(&short.replace("marker:", ":marker:")) || short == marker_ref)
                            })
                            .and_then(|row| row["ref"].as_str().map(str::to_owned))
                    });
                    let rows = self.changes.borrow().len();
                    let t0 = perf_now();
                    let done = match marker_ref {
                        Some(short) => Some(
                            tool_named(&look, "make_changes")
                                .unwrap_or(plan.clone())
                                .execute(
                                    json!({"steps": [{"tool": "marker_delete", "input": {"ref": short}}]})
                                        .as_object()
                                        .cloned()
                                        .unwrap_or_default(),
                                    signal(),
                                )
                                .await?,
                        ),
                        None => None,
                    };
                    let deleted = done.as_ref().is_some_and(|done| !done.is_error) && self.count("marker").await == markers;
                    for record in self.changes.borrow()[rows..].iter() {
                        if !tidied.contains(&record.id) {
                            tidied.push(record.id.clone());
                        }
                    }
                    let what = match &done {
                        Some(done) if !done.is_error => {
                            format!("the marker taken away through Kumi (marker_delete, not undone at the end): {markers} markers again")
                        }
                        Some(done) => format!("the marker's deletion through Kumi: {}", short(&done.text, 200)),
                        None => "the marker isn't in the look's markers to delete it through Kumi: left for Cmd-Z in Logic Pro".into(),
                    };
                    self.say(deleted, Some(perf_now() - t0), &what);
                }
                None => self.say(false, None, &format!("no marker at beat {at} to take away: left for Cmd-Z in Logic Pro")),
            }
        }
        if self.stopping.get() {
            return Ok(());
        }

        // A region split at its middle, the right part's ref named; whole again through Kumi's undo.
        let look = self.observe().await?;
        let page = match tool_named(&look, "logic_read") {
            Some(read) => serde_json::from_str::<Value>(
                &read.execute(json!({"kind": "region", "limit": 200}).as_object().cloned().unwrap_or_default(), signal()).await?.text,
            )
            .unwrap_or(Value::Null),
            None => Value::Null,
        };
        let region = page["items"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|row| row["endBeats"].as_f64().unwrap_or(0.0) - row["startBeats"].as_f64().unwrap_or(0.0) >= 2.0)
            .cloned();
        match region {
            None => self.say(false, None, "region split: no region of two beats or more to split"),
            Some(region) => {
                let (start, end) = (region["startBeats"].as_f64().unwrap_or(0.0), region["endBeats"].as_f64().unwrap_or(0.0));
                let cut = ((start + end) / 2.0).floor().clamp(start + 1.0, end - 1.0);
                let name = region["name"].as_str().unwrap_or("?").to_owned();
                let regions = self.count("region").await;
                let rows = self.changes.borrow().len();
                let t0 = perf_now();
                let (is_error, text) = self.run_tool(&look, "region_split", json!({"ref": region["ref"], "positionBeats": cut})).await?;
                let ms = perf_now() - t0;
                let created = serde_json::from_str::<Value>(&text).ok().and_then(|answer| answer["ref"].as_str().map(str::to_owned));
                let regions_now = self.count("region").await;
                let what = if is_error {
                    format!("region split “{name}” at beat {cut}: {}", short(&text, 200))
                } else {
                    format!(
                        "region split “{name}” at beat {cut}: {regions} → {regions_now} regions, the right part {}",
                        created.as_deref().unwrap_or("unnamed")
                    )
                };
                self.say(!is_error && regions_now == regions + 1 && created.is_some(), Some(ms), &what);
                if let Some(row) = self.change_since(rows, "Split region") {
                    let t0 = perf_now();
                    let result = undo.execute(json!({"change": row.id}).as_object().cloned().unwrap_or_default(), signal()).await?;
                    let whole = !result.is_error && self.count("region").await == regions;
                    let note = self.change(&row.id).and_then(|row| row.note).map(|note| format!(" ({note})")).unwrap_or_default();
                    self.say(
                        whole,
                        Some(perf_now() - t0),
                        &format!(
                            "undo the split: {}{note}",
                            if whole { format!("{regions} regions again") } else { short(&result.text, 200) }
                        ),
                    );
                }
            }
        }
        if self.stopping.get() {
            return Ok(());
        }

        // A MIDI import of 64 sixteenths onto a software instrument track, a bar past every region, read back; undone.
        // Logic lands the file on a new track below the one named. The bridge's note read lists only the Event List rows
        // Logic shows (33 of 64 at the M4 gate), and its apply says so in the change's note ("lists its first N rows"), so
        // the import counts when its region spans the 64 sixteenths (16 beats) and either the read holds the 64 notes or
        // the answer says the Event List listed fewer rows.
        let look = self.observe().await?;
        let context: Value = serde_json::from_str(&look.context).unwrap_or(Value::Null);
        let target = context["tracks"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|track| track["kind"] == "instrument")
            .or_else(|| context["tracks"].as_array().into_iter().flatten().find(|track| track["kind"] == "external"))
            .cloned();
        match target {
            None => self.say(false, None, "MIDI import: no software instrument or external MIDI track to import onto"),
            Some(track) => {
                let notes: Vec<Value> =
                    (0..64).map(|n| json!({"pitch": 48 + n % 12, "start": n as f64 * 0.25, "length": 0.25, "velocity": 96})).collect();
                let name = track["name"].as_str().unwrap_or("?").to_owned();
                let regions = self.count("region").await;
                let rows = self.changes.borrow().len();
                let t0 = perf_now();
                let (is_error, text) =
                    self.run_tool(&look, "midi_import", json!({"ref": track["ref"], "positionBeats": at, "notes": notes})).await?;
                let ms = perf_now() - t0;
                // The change tool's text: {"changed": "<title> (<the apply's note>)", "change": <its id>, "ref": <the region,
                // short>}; the note names the Event List's rows when it listed fewer than the file's notes.
                let created = serde_json::from_str::<Value>(&text).ok().and_then(|answer| answer["ref"].as_str().map(str::to_owned));
                let listed = text.split("lists its first ").nth(1).and_then(|rest| rest.split(' ').next()?.parse::<u64>().ok());
                let t1 = perf_now();
                let (held, span) = match (&created, tool_named(&look, "logic_read")) {
                    (Some(created), Some(read)) => {
                        let page = |input: Value| read.execute(input.as_object().cloned().unwrap_or_default(), signal());
                        let notes = page(json!({"kind": "note", "parent": created, "limit": 200})).await?;
                        let held = serde_json::from_str::<Value>(&notes.text).ok().and_then(|page| page["total"].as_u64()).unwrap_or(0);
                        let made = page(json!({"kind": "region", "limit": 200})).await?;
                        let span = serde_json::from_str::<Value>(&made.text)
                            .ok()
                            .and_then(|page| {
                                let row =
                                    page["items"].as_array()?.iter().find(|row| row["ref"].as_str() == Some(created.as_str()))?.clone();
                                Some(row["endBeats"].as_f64()? - row["startBeats"].as_f64()?)
                            })
                            .unwrap_or(0.0);
                        (held, span)
                    }
                    _ => (0, 0.0),
                };
                let read_ms = perf_now() - t1;
                let spans = span + 1e-6 >= 16.0;
                let shown = match listed {
                    _ if held == 64 => " holds 64 notes".to_string(),
                    Some(shown_rows) => format!(": the Event List listed {shown_rows} of 64 rows; the region spans {span} beats"),
                    None => format!(" holds {held} notes; the region spans {span} beats"),
                };
                let what = if is_error {
                    format!("MIDI import of 64 notes on “{name}” at beat {at}: {}", short(&text, 200))
                } else {
                    format!(
                        "MIDI import of 64 notes on “{name}” at beat {at}: region {}{shown} (read back in {}), {regions} → {} regions",
                        created.as_deref().unwrap_or("unnamed"),
                        seconds(read_ms),
                        self.count("region").await
                    )
                };
                self.say(!is_error && spans && (held == 64 || listed.is_some_and(|shown_rows| shown_rows < 64)), Some(ms), &what);
                if let Some(row) = self.change_since(rows, "Imported") {
                    let t0 = perf_now();
                    let result = undo.execute(json!({"change": row.id}).as_object().cloned().unwrap_or_default(), signal()).await?;
                    let gone = !result.is_error && self.count("region").await == regions;
                    let note = self.change(&row.id).and_then(|row| row.note).map(|note| format!(" ({note})")).unwrap_or_default();
                    self.say(
                        gone,
                        Some(perf_now() - t0),
                        &format!(
                            "undo the import: {}{note}",
                            if gone { format!("{regions} regions again") } else { short(&result.text, 200) }
                        ),
                    );
                }
            }
        }
        if self.stopping.get() {
            return Ok(());
        }

        // The cycle set, then cleared, each read back from the focus's transport.
        let look = self.observe().await?;
        let t0 = perf_now();
        let (is_error, text) = self.run_tool(&look, "loop_range", json!({"startBeats": 0, "endBeats": 16})).await?;
        let cycle = self.tool("focus", json!({})).await.body["transport"]["cycle"].clone();
        self.say(
            !is_error && cycle == true,
            Some(perf_now() - t0),
            &format!(
                "cycle set, bar 1 to bar 5: the focus's transport says cycle {cycle}{}",
                if is_error { format!(": {}", short(&text, 200)) } else { String::new() }
            ),
        );
        let t0 = perf_now();
        let (is_error, text) = self.run_tool(&look, "loop_range", json!({"startBeats": 0, "endBeats": 0})).await?;
        let cycle = self.tool("focus", json!({})).await.body["transport"]["cycle"].clone();
        self.say(
            !is_error && cycle == false,
            Some(perf_now() - t0),
            &format!(
                "cycle cleared: the focus's transport says cycle {cycle}{}",
                if is_error { format!(": {}", short(&text, 200)) } else { String::new() }
            ),
        );

        // A tempo change in the Tempo List, set and deleted, each read back from the list itself (the tempo row holds
        // the project's tempo and signatures only).
        let bpm = (context["project"]["tempo"].as_f64().unwrap_or(120.0).round() + 8.0).min(990.0);
        let t0 = perf_now();
        let done = run_plan(json!([{"tool": "tempo_marker_set", "input": {"positionBeats": at, "bpm": bpm}}])).await?;
        let listed = self.tempo_listed(at).await;
        self.say(
            !done.is_error && listed.as_ref().is_ok_and(|shown| *shown == Some(bpm)),
            Some(perf_now() - t0),
            &format!(
                "tempo {bpm} BPM at beat {at} in the Tempo List: {}; the list {}",
                if done.is_error { short(&done.text, 200) } else { "set".into() },
                match &listed {
                    Ok(Some(shown)) => format!("shows {shown} BPM there"),
                    Ok(None) => "has no tempo event there".into(),
                    Err(why) => format!("couldn't be read ({why})"),
                }
            ),
        );
        let t0 = perf_now();
        let done = run_plan(json!([{"tool": "tempo_marker_delete", "input": {"positionBeats": at}}])).await?;
        let listed = self.tempo_listed(at).await;
        self.say(
            !done.is_error && matches!(listed, Ok(None)),
            Some(perf_now() - t0),
            &format!(
                "the tempo change at beat {at} deleted: {}; the list {}",
                if done.is_error { short(&done.text, 200) } else { "deleted".into() },
                match &listed {
                    Ok(Some(shown)) => format!("still shows {shown} BPM there"),
                    Ok(None) => "has no tempo event there".into(),
                    Err(why) => format!("couldn't be read ({why})"),
                }
            ),
        );

        // Everything M4 made, undone through Kumi, last first: the cycle's two changes and the Tempo List's two (the
        // split and the import are undone already, the key command's row is Logic's, the tidy-ups stay).
        let mut ids: Vec<String> = Vec::new();
        for record in self.changes.borrow()[heard..].iter() {
            if !ids.contains(&record.id) && !tidied.contains(&record.id) {
                ids.push(record.id.clone());
            }
        }
        for id in ids.iter().rev() {
            let Some(row) = self.change(id) else { continue };
            if !matches!(row.state, ChangeState::Applied | ChangeState::Unsure) {
                continue;
            }
            let t0 = perf_now();
            let result = undo.execute(json!({"change": id}).as_object().cloned().unwrap_or_default(), signal()).await?;
            let undone = !result.is_error && self.change(id).is_some_and(|now| now.state == ChangeState::Undone);
            let what = if undone { "undone".to_string() } else { short(&result.text, 200) };
            self.say(undone, Some(perf_now() - t0), &format!("undo “{}”: {what}", row.title));
        }
        let after = (self.observe().await?.context, self.count("region").await, self.count("marker").await);
        let tracks_now =
            serde_json::from_str::<Value>(&after.0).ok().and_then(|context| context["tracks"].as_array().map(Vec::len)).unwrap_or(0);
        let same = (tracks_now, after.1, after.2) == before;
        self.say(
            same,
            None,
            &format!(
                "the project reads as before M4: {tracks_now} tracks, {} regions, {} markers (before: {} tracks, {} regions, {} markers)",
                after.1, after.2, before.0, before.1, before.2
            ),
        );
        Ok(())
    }

    /// M3, with the operator: a rename made through Kumi (a step of Logic's undo, where a fader move is none), then
    /// Cmd-Z in Logic: the bridge names it back (`undone_in_daw`), and HISTORY marks the change undone with Logic's
    /// own undo. Without the Cmd-Z, Kumi undoes it, so nothing of the run is left in the project.
    async fn cmd_z_named(&self) -> Result<(), RuntimeError> {
        let observation = self.observe().await?;
        let context: Value = serde_json::from_str(&observation.context).unwrap_or(Value::Null);
        let Some(first) = context["tracks"].as_array().and_then(|tracks| tracks.first().cloned()) else {
            self.say(false, None, "Cmd-Z: the project has no track to change");
            return Ok(());
        };
        let (Some(plan), Some(undo)) = (tool_named(&observation, "make_changes"), tool_named(&observation, "undo_change")) else {
            self.say(false, None, "Cmd-Z: no make_changes and undo_change tools");
            return Ok(());
        };
        let name = first["name"].as_str().unwrap_or("Track 1").to_owned();
        let heard = self.changes.borrow().len();
        let steps = json!({"steps": [{"tool": "track_rename", "input": {"ref": first["ref"], "name": format!("{name} (Kumi)")}}]});
        let made = plan.execute(steps.as_object().cloned().unwrap_or_default(), signal()).await?;
        let Some(record) = self.changes.borrow()[heard..].first().cloned() else {
            self.say(false, None, &format!("Cmd-Z: the change wasn't made: {}", short(&made.text, 200)));
            return Ok(());
        };
        self.say(!made.is_error, None, &format!("a change to take back in Logic Pro: “{}”", record.title));
        println!("  → In Logic Pro, press Cmd-Z once (within {} s)…", OPERATOR_MS / 1000);
        let events_before = self.events.borrow().len();
        let t0 = perf_now();
        let named = self
            .wait_for(|| async move {
                self.events_since(events_before, "undone_in_daw")
                    .into_iter()
                    .find(|data| data["transactionIds"].as_array().is_some_and(|ids| !ids.is_empty()))
            })
            .await;
        match &named {
            Some(data) => {
                self.say(true, Some(perf_now() - t0), &format!("Cmd-Z named back: undone_in_daw {}", short(&data.to_string(), 160)))
            }
            None => self.say(false, Some(perf_now() - t0), "no undone_in_daw named the change (no Cmd-Z, or the bridge didn't see it)"),
        }
        // HISTORY follows a moment after the event.
        let mut now = self.change(&record.id);
        for _ in 0..20 {
            if now.as_ref().is_some_and(|record| record.state == ChangeState::Undone) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
            now = self.change(&record.id);
        }
        let undone = now.as_ref().is_some_and(|record| record.state == ChangeState::Undone);
        let note = now.as_ref().and_then(|record| record.note.clone()).map(|note| format!(" ({note})")).unwrap_or_default();
        self.say(
            undone,
            None,
            &format!("HISTORY: “{}” {}{note}", record.title, now.as_ref().map(|r| state_word(r.state)).unwrap_or("gone")),
        );
        if !undone {
            let result = undo.execute(json!({"change": record.id}).as_object().cloned().unwrap_or_default(), signal()).await?;
            let back = !result.is_error && self.change(&record.id).is_some_and(|now| now.state == ChangeState::Undone);
            let what = if back { "undone".to_string() } else { short(&result.text, 200) };
            self.say(back, None, &format!("undone through Kumi instead, so nothing of the run is left: {what}"));
        }
        Ok(())
    }

    fn exit_code(&self) -> i32 {
        let rows = self.rows.borrow();
        let passed = rows.iter().filter(|row| row.ok).count();
        let stopping = self.stopping.get();
        println!("\n{passed} of {} passed{}.", rows.len(), if stopping { " (stopped early)" } else { "" });
        if passed == rows.len() && !stopping {
            0
        } else {
            1
        }
    }

    /// The run as a Markdown record for docs/evidence/.
    fn summary(&self) -> String {
        let status = self.status.borrow();
        let rows = self.rows.borrow();
        let passed = rows.iter().filter(|row| row.ok).count();
        let today = chrono::Utc::now().format("%Y-%m-%d");
        let mut out = format!("# Logic Pro M2, M3 and M4 acceptance, {today}\n\nStatus: {} of {} checks passed.\n\n", passed, rows.len());
        out.push_str("| | |\n| --- | --- |\n");
        out.push_str(&format!(
            "| Logic Pro | {} (pid {}, `{}`) |\n| Bridge | {} |\n| Project | {} |\n| Host | `daw-mcp-server --daw logic` from this build |\n\n",
            status["dawVersion"].as_str().unwrap_or("?"),
            status["pid"],
            status["resource"].as_str().unwrap_or("?"),
            status["bridge"].as_str().unwrap_or("?"),
            status["project"]["path"].as_str().unwrap_or("none")
        ));
        out.push_str("| Check | Result | Time |\n| --- | --- | --- |\n");
        for row in rows.iter() {
            let what = row.what.replace('|', "\\|");
            out.push_str(&format!("| {what} | {} | {} |\n", if row.ok { "ok" } else { "failed" }, row.ms.map(seconds).unwrap_or_default()));
        }
        out
    }
}
