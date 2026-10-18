//! REAPER's and Renoise's sessions made the way the app makes them (`kumi::cli::run`, as main() does, with
//! `--daw reaper|renoise`), Kumi's host in process in front of a simulated bridge: the session speaks the DAW's words
//! (its profile is the DAW's), and says so when the DAW is busy, opens another project in the same tab, or goes away;
//! none of it in Live's words.
#[path = "support/live_words.rs"]
mod live_words;
#[path = "support/vt.rs"]
mod vt;
use async_trait::async_trait;
use daw_bridge::{files::ensure_secret, paths::BridgePaths, Daw};
use daw_mcp_server::{
    host::Host,
    simulator::{reaper::ReaperScript, renoise::RenoiseScript, Simulator, SimulatorConfig},
};
use futures::FutureExt;
use kumi::{
    cli::{run, CliIo},
    daw::{DawFactory, OwnHostFactory},
    input::{ByteListener, TerminalInput},
    tui::tty::TtyOutput,
};
use kumi_common::abort::Signal;
use kumi_runtime::core::{contracts::*, daw::DawId, errors::RuntimeError};
use kumi_runtime::mcp::client::{McpEndpoint, StderrStatus};
use kumi_runtime::mcp::types::{CallToolResult, Implementation, ListToolsResult};
use kumi_runtime::system::Env;
use serde_json::json;
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    sync::Arc,
};

#[derive(Default)]
struct Input {
    tty: bool,
    raw: Cell<bool>,
    data: RefCell<Option<ByteListener>>,
    end: RefCell<Option<Rc<dyn Fn()>>>,
}
impl TerminalInput for Input {
    fn is_tty(&self) -> bool {
        self.tty
    }
    fn is_raw(&self) -> bool {
        self.raw.get()
    }
    fn set_raw_mode(&self, raw: bool) -> std::io::Result<()> {
        self.raw.set(raw);
        Ok(())
    }
    fn resume(&self, f: ByteListener) {
        *self.data.borrow_mut() = Some(f);
    }
    fn pause(&self) {}
    fn on_end(&self, f: Rc<dyn Fn()>) {
        *self.end.borrow_mut() = Some(f);
    }
}
impl Input {
    fn write(&self, s: &str) {
        let f = self.data.borrow().clone();
        if let Some(f) = f {
            f(s.as_bytes())
        }
    }
    fn end(&self) {
        let f = self.end.borrow().clone();
        if let Some(f) = f {
            f()
        }
    }
}
#[derive(Default)]
struct Output(RefCell<String>, bool);
impl TtyOutput for Output {
    fn is_tty(&self) -> bool {
        self.1
    }
    fn columns(&self) -> Option<i32> {
        self.1.then_some(120)
    }
    fn rows(&self) -> Option<i32> {
        self.1.then_some(40)
    }
    fn write(&self, s: &str) {
        self.0.borrow_mut().push_str(s);
    }
}

/// Kumi's host, called in process.
struct InProcess {
    host: Arc<Host>,
    id: Cell<u64>,
}
#[async_trait(?Send)]
impl McpEndpoint for InProcess {
    fn pid(&self) -> Option<u32> {
        None
    }
    fn server_info(&self) -> Option<Implementation> {
        None
    }
    async fn list(&self, _: Option<&str>, _: Signal) -> Result<ListToolsResult, RuntimeError> {
        Ok(serde_json::from_value(json!({"tools": daw_mcp_server::mcp::tools(&self.host)})).unwrap())
    }
    async fn call(&self, name: &str, args: JsonObject, _: Signal) -> Result<CallToolResult, RuntimeError> {
        self.id.set(self.id.get() + 1);
        let message = json!({"jsonrpc": "2.0", "id": self.id.get(), "method": "tools/call", "params": {"name": name, "arguments": args}});
        let answer = daw_mcp_server::mcp::handle(&self.host, &message).await.expect("an answer");
        Ok(serde_json::from_value(answer["result"].clone()).unwrap())
    }
    fn on_catalog_changed(&self, _: Rc<dyn Fn()>) -> Box<dyn Fn()> {
        Box::new(|| {})
    }
    fn on_disconnect(&self, _: Rc<dyn Fn()>) -> Box<dyn Fn()> {
        Box::new(|| {})
    }
    fn stderr_status(&self) -> StderrStatus {
        StderrStatus { bytes: 0, truncated: false }
    }
    async fn close(&self) -> Result<(), RuntimeError> {
        self.host.close().await;
        Ok(())
    }
}

async fn wait_for(what: &str, out: &Output, done: impl Fn(&str) -> bool) {
    for _ in 0..2500 {
        if done(&out.0.borrow()) {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(4)).await;
    }
    panic!("timed out waiting for {what}:\n{}", out.0.borrow());
}

/// What the plain terminal shows a command has answered: commands answer in order, so an unknown one's answer marks it.
const MARK: &str = "Unknown command. Use /help.";
async fn command(input: &Input, out: &Output, line: &str) -> String {
    let before = out.0.borrow().len();
    input.write(&format!("{line}\n/mark\n"));
    wait_for(line, out, |text| text[before..].contains(MARK)).await;
    let text = out.0.borrow()[before..].to_string();
    text
}

/// A home where Kumi's bridge is in `bridge`'s DAW (as its receipt says), `simulator` answering as that DAW behind
/// Kumi's host, called in process: main()'s own path, `kumi::cli::run` with `--daw`, makes the session.
struct AppLab {
    _home: tempfile::TempDir,
    simulator: Rc<Simulator>,
    env: Env,
    factory: DawFactory,
    daw: DawId,
}
impl AppLab {
    async fn new(daw: DawId, bridge: Daw, config: impl FnOnce(daw_bridge::auth::Secret) -> SimulatorConfig) -> AppLab {
        let home = tempfile::tempdir().unwrap();
        let kumi_home = home.path().join(".kumi");
        let paths = BridgePaths::new(&kumi_home, bridge);
        let secret = ensure_secret(&paths.secret()).unwrap();
        let mut config = config(secret);
        config.discovery_dir = Some(paths.discovery());
        let simulator = Rc::new(Simulator::start(config).await.unwrap());
        let receipt = daw_bridge::lifecycle::Receipt {
            schema: daw_bridge::lifecycle::RECEIPT_SCHEMA.into(),
            daw: bridge.id().into(),
            status: "installed".into(),
            target: home.path().join("bridge").display().to_string(),
            reference: home.path().join("bridge-reference.json").display().to_string(),
            generation: Some("g1".into()),
            version: Some("1.0.0".into()),
            sha256: None,
            previous: vec![],
            updated_ms: 0,
            history: vec![],
        };
        std::fs::write(paths.receipt(), serde_json::to_string(&receipt).unwrap()).unwrap();
        std::fs::write(
            kumi_home.join("settings.json"),
            json!({"model": "anthropic/claude-sonnet-5-5", "updateCheck": false, "libraryFolders": [], "keepAwake": false, "notifications": "off"})
                .to_string(),
        )
        .unwrap();
        let env = Env::from([
            ("HOME".into(), home.path().display().to_string()),
            ("KUMI_HOME".into(), kumi_home.display().to_string()),
            ("KUMI_NO_UPDATE_CHECK".into(), "1".into()),
            ("KUMI_REMOTE_SCRIPTS_DIR".into(), home.path().join("Remote Scripts").display().to_string()),
            ("KUMI_REAPER_RESOURCE_DIR".into(), home.path().join("REAPER").display().to_string()),
            ("KUMI_RENOISE_PREFS_DIR".into(), home.path().join("Renoise").display().to_string()),
            ("TERM".into(), "xterm-256color".into()),
        ]);
        let host_home = kumi_home.clone();
        let own_host: OwnHostFactory = Rc::new(move |_| {
            let endpoint = Rc::new(InProcess { host: Arc::new(Host::new(bridge, &host_home)), id: Cell::new(0) });
            Rc::new(move |_| {
                let endpoint = endpoint.clone();
                async move { Ok(endpoint as Rc<dyn McpEndpoint>) }.boxed_local()
            })
        });
        let factory = DawFactory {
            ableton: Rc::new(|_| panic!("a REAPER or Renoise session makes no Ableton integration")),
            own_host: Some(own_host),
        };
        AppLab { _home: home, simulator, env, factory, daw }
    }
    /// Kumi started as main() starts it: the full screen in a terminal (`tty`), else plain lines.
    fn start(&self, tty: bool) -> (Rc<Input>, Rc<Output>, tokio::task::JoinHandle<i32>) {
        let input = Rc::new(Input { tty, ..Input::default() });
        let (out, err) = (Rc::new(Output(RefCell::default(), tty)), Rc::new(Output::default()));
        let mut env = self.env.clone();
        if !tty {
            env.insert("KUMI_UI".into(), "plain".into());
        }
        let mut io = CliIo::new(input.clone(), out.clone(), err, env);
        io.args = vec!["--daw".into(), self.daw.as_str().into()];
        (input, out, tokio::task::spawn_local(run(io, self.factory.clone())))
    }
}

/// A plain session of `daw`: what it says as the DAW gets busy, takes another project in its tab (`another`), and goes
/// away.
async fn session(
    daw: DawId,
    bridge: Daw,
    config: impl FnOnce(daw_bridge::auth::Secret) -> SimulatorConfig,
    another: impl FnOnce(),
) -> String {
    let lab = AppLab::new(daw, bridge, config).await;
    let simulator = lab.simulator.clone();
    let (input, out, task) = lab.start(false);
    wait_for("the first look", &out, |text| text.lines().any(|l| l.starts_with("[observation] "))).await;
    // Busy: a dialog is open.
    simulator.set_busy(Some("modal"));
    wait_for("busy", &out, |text| text.contains(": busy (a dialog is open)")).await;
    let status = command(&input, &out, "/status").await;
    assert!(status.contains("busy (a dialog is open)"), "{status}");
    simulator.set_busy(None);
    wait_for("not busy", &out, |text| text.contains(": answering again")).await;
    // Another project in the same tab (or another song).
    another();
    wait_for("the other project", &out, |text| text.matches("[observation] ").count() >= 2).await;
    command(&input, &out, "/status").await;
    // The DAW goes away.
    simulator.stop();
    let before = out.0.borrow().len();
    wait_for("the DAW gone", &out, |text| text[before..].contains(": disconnected")).await;
    input.write("/quit\n");
    input.end();
    assert_eq!(tokio::time::timeout(std::time::Duration::from_secs(10), task).await.unwrap().unwrap(), 0);
    // One bridge alone: the DAW used last decides nothing, so it isn't written (uninstall and rollbacks stay clean).
    assert!(!lab._home.path().join(".kumi/last-daw").exists(), "last-daw written with one bridge");
    let said = out.0.borrow().clone();
    said
}

/// The full screen of a `daw` session, as the producer sees it at each step: started, busy, another project, gone,
/// and the goodbye.
async fn screens(
    daw: DawId,
    bridge: Daw,
    config: impl FnOnce(daw_bridge::auth::Secret) -> SimulatorConfig,
    another: impl FnOnce(),
) -> Vec<(String, String)> {
    let lab = AppLab::new(daw, bridge, config).await;
    let simulator = lab.simulator.clone();
    let (input, out, task) = lab.start(true);
    let term = RefCell::new(vt::VirtualTerminal::new(120, 40));
    let consumed = Cell::new(0);
    let screen = || {
        let written = out.0.borrow();
        term.borrow_mut().write(&written[consumed.get()..]);
        consumed.set(written.len());
        term.borrow().lines().join("\n")
    };
    let app = kumi_runtime::integrations::profile(daw).unwrap().words.app;
    let mut seen = vec![];
    until("the session", &screen, |s| s.contains(&format!("● {app}"))).await;
    seen.push(("connected".to_string(), screen()));
    simulator.set_busy(Some("modal"));
    until("busy", &screen, |s| s.contains(&format!("● {app} busy")) && s.contains(&format!("FOCUS · {app} is busy"))).await;
    seen.push(("busy".to_string(), screen()));
    input.write("/status\r");
    until("/status", &screen, |s| s.contains(&format!("{app} is busy: a dialog is open ·"))).await;
    seen.push(("busy /status".to_string(), screen()));
    simulator.set_busy(None);
    until("not busy", &screen, |s| s.contains(&format!("● {app} ")) && !s.contains(&format!("● {app} busy"))).await;
    another();
    until("the other project", &screen, |s| s.contains("has the other")).await;
    seen.push(("another project".to_string(), screen()));
    simulator.stop();
    until("gone", &screen, |s| s.contains(&format!("{app} not connected"))).await;
    seen.push(("gone".to_string(), screen()));
    input.write("\x03\x03");
    let _ = tokio::time::timeout(std::time::Duration::from_secs(10), task).await;
    let written = out.0.borrow().clone();
    let plain = regex::Regex::new(r"\x1b\[[0-9;?<>]*[ -/]*[@-~]").unwrap().replace_all(&written, "").to_string();
    seen.push(("goodbye".to_string(), plain.rfind("Kumi closed").map(|at| plain[at..].to_string()).unwrap_or_default()));
    seen
}

/// Waits (a while) until the screen shows what `done` looks for.
async fn until(what: &str, screen: &dyn Fn() -> String, done: impl Fn(&str) -> bool) {
    for _ in 0..2500 {
        if done(&screen()) {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(4)).await;
    }
    panic!("timed out waiting for {what}:\n{}", screen());
}

fn assert_no_live_words(daw: &str, seen: &[(String, String)]) {
    let leaks: Vec<String> =
        seen.iter().flat_map(|(at, screen)| live_words::leaks(screen).into_iter().map(move |line| format!("[{at}] {line}"))).collect();
    assert!(leaks.is_empty(), "{daw} shows Live's words:\n{}", leaks.join("\n"));
}

macro_rules! case {
    ($name:ident,$body:expr) => {
        #[tokio::test(flavor = "current_thread")]
        async fn $name() {
            tokio::task::LocalSet::new().run_until($body).await;
        }
    };
}

case!(a_reaper_session_made_as_the_app_makes_it_speaks_reapers_words, async {
    let script = ReaperScript::new(3, 0);
    let tab = script.clone();
    let said = session(DawId::Reaper, Daw::Reaper, move |secret| script.config(secret, "/lab/REAPER"), move || tab.switch_tab()).await;
    let leaks = live_words::leaks(&said);
    assert!(leaks.is_empty(), "Live's words:\n{}\n\nin:\n{said}", leaks.join("\n"));
    for want in ["MCP/REAPER", "REAPER is opening another project", "REAPER closed", "Each project's conversation continues next time"] {
        assert!(said.contains(want), "no {want:?} in:\n{said}");
    }
});

case!(a_renoise_session_made_as_the_app_makes_it_speaks_renoises_words, async {
    let script = RenoiseScript::new();
    let song = script.clone();
    let said = session(DawId::Renoise, Daw::Renoise, move |secret| script.config(secret, "/lab/Renoise"), move || song.switch_song()).await;
    let leaks = live_words::leaks(&said);
    assert!(leaks.is_empty(), "Live's words:\n{}\n\nin:\n{said}", leaks.join("\n"));
    for want in ["MCP/Renoise", "Renoise is opening another song", "Renoise closed", "Each song's conversation continues next time"] {
        assert!(said.contains(want), "no {want:?} in:\n{said}");
    }
});

case!(reapers_full_screen_made_as_the_app_makes_it_shows_busy_another_project_and_gone_in_its_words, async {
    let script = ReaperScript::new(3, 0);
    let tab = script.clone();
    let seen = screens(DawId::Reaper, Daw::Reaper, move |secret| script.config(secret, "/lab/REAPER"), move || tab.switch_tab()).await;
    assert_no_live_words("REAPER", &seen);
    let busy = &seen.iter().find(|(at, _)| at == "busy").unwrap().1;
    assert!(busy.contains("● REAPER busy") && !busy.contains("not connected") && !busy.contains("isn't connected"), "{busy}");
    let gone = &seen.iter().find(|(at, _)| at == "gone").unwrap().1;
    assert!(gone.contains("REAPER isn't connected"), "the status bar and FOCUS agree: {gone}");
    assert!(seen.last().unwrap().1.contains("Each project's conversation continues next time"), "{:?}", seen.last());
});

case!(renoises_full_screen_made_as_the_app_makes_it_shows_busy_another_song_and_gone_in_its_words, async {
    let script = RenoiseScript::new();
    let song = script.clone();
    let seen = screens(DawId::Renoise, Daw::Renoise, move |secret| script.config(secret, "/lab/Renoise"), move || song.switch_song()).await;
    assert_no_live_words("Renoise", &seen);
    let gone = &seen.iter().find(|(at, _)| at == "gone").unwrap().1;
    assert!(gone.contains("Renoise isn't connected"), "the status bar and FOCUS agree: {gone}");
    assert!(seen.last().unwrap().1.contains("Each song's conversation continues next time"), "{:?}", seen.last());
});
