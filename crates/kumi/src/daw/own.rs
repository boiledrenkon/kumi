//! REAPER and Renoise as the app works with them: Kumi's own bridge in each (REAPER's extension, Renoise's tool),
//! found, installed, checked and taken out through the shared lifecycle (daw-bridge), and the session's integration
//! through Kumi's host. One provider serves both; each DAW gives its words and its integration.
use crate::daw::provider::{
    BridgeCommand, BridgeRollbackStep, BridgeState, DawFactory, DawProvider, DawSession, EnableStep, Installed, RemovedBridge, Toggle,
    ToggleWords, WatchWords,
};
use crate::doctor::{Check, DoctorIo, InstallationIo, LiveProbe};
use crate::report::LogFiles;
use crate::tui::app::DawSetup;
use crate::{install::InstalledIo, process::Run, tui::tty::TtyOutput, update::UpdateIo};
use async_trait::async_trait;
use daw_bridge::{
    daw::Daw,
    detect::{self, Os},
    doctor::{self, Bundled, Level},
    files,
    install::{reaper_extension_name, reaper_target, renoise_target, RENOISE_TOOL_FOLDER},
    lifecycle::{Lifecycle, Receipt, Target},
    paths::BridgePaths,
};
use futures::{future::LocalBoxFuture, FutureExt};
use kumi_common::abort::{self, Signal};
use kumi_runtime::{
    core::{
        context::IntegrationContext,
        contracts::{ConnectionState, Integration, SessionController},
        daw::DawId,
    },
    integrations::own_bridge::OwnOptions,
    system::Env,
    RuntimeError,
};
use std::{
    cell::RefCell,
    path::{Path, PathBuf},
    rc::{Rc, Weak},
    time::Duration,
};

/// One DAW whose bridge is Kumi's own.
pub struct Own {
    pub id: DawId,
    pub daw: Daw,
    /// The bridge, as the producer knows it: "Kumi's extension", "Kumi's tool".
    pub bridge: &'static str,
    pub min_major: u32,
    pub create: fn(OwnOptions) -> Rc<dyn Integration>,
    /// What chatting without the DAW says it is.
    pub instructions: &'static str,
}

pub static REAPER: Own = Own {
    id: DawId::Reaper,
    daw: Daw::Reaper,
    bridge: "Kumi's extension",
    min_major: 7,
    create: kumi_runtime::integrations::reaper::create_reaper_integration,
    instructions: "REAPER isn't connected, so Kumi can't see your project: answer from what you know, and say so.",
};

pub static RENOISE: Own = Own {
    id: DawId::Renoise,
    daw: Daw::Renoise,
    bridge: "Kumi's tool",
    min_major: 3,
    create: kumi_runtime::integrations::renoise::create_renoise_integration,
    instructions: "Renoise isn't connected, so Kumi can't see your song: answer from what you know, and say so.",
};

fn kumi_home(env: &Env) -> PathBuf {
    PathBuf::from(crate::config::kumi_dir(env))
}

fn home(env: &Env) -> PathBuf {
    env.get("HOME")
        .or_else(|| env.get("USERPROFILE"))
        .filter(|home| !home.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| home::home_dir().unwrap_or_default())
}

/// What daw-bridge's detection reads, from the app's environment.
fn detected<T>(env: &Env, read: impl FnOnce(&detect::Env) -> T) -> T {
    let var = |name: &str| env.get(name).cloned();
    read(&detect::Env { os: Os::current(), home: home(env), var: &var, root: None })
}

impl Own {
    fn name(&self) -> &'static str {
        self.profile().words.app
    }

    /// Where the bridge goes in the DAW, or why it can't go yet.
    fn target(&self, env: &Env) -> Result<Target, String> {
        match self.daw {
            Daw::Reaper => {
                let installs = detected(env, detect::detect_reaper);
                let install = installs.iter().find(|install| install.opened).or(installs.first()).ok_or("REAPER isn't installed")?;
                reaper_target(install, Os::current()).map_err(|why| why.to_string())
            }
            Daw::Renoise => renoise_target(&detected(env, detect::detect_renoise)).map_err(|why| why.to_string()),
        }
    }

    fn receipt(&self, env: &Env) -> Option<Receipt> {
        let paths = BridgePaths::new(&kumi_home(env), self.daw);
        let text = std::fs::read_to_string(paths.receipt()).ok()?;
        serde_json::from_str::<Receipt>(&text).ok().filter(|receipt| receipt.status == "installed")
    }

    /// The bridge this Kumi brings: beside Kumi in a release (`daws/<daw>/`), or this checkout's own build.
    fn bundled_source(&self) -> Option<PathBuf> {
        let exe = std::env::current_exe().ok()?;
        let beside = exe.parent()?.to_path_buf();
        let checkout = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let candidates: Vec<PathBuf> = match self.daw {
            Daw::Reaper => {
                let name = reaper_extension_name(Os::current());
                let built = if cfg!(windows) { name.to_string() } else { format!("lib{name}") };
                vec![beside.join("daws").join("reaper").join(name), beside.join(built)]
            }
            Daw::Renoise => {
                vec![
                    beside.join("daws").join("renoise").join(RENOISE_TOOL_FOLDER),
                    checkout.join("daws").join("renoise").join(RENOISE_TOOL_FOLDER),
                ]
            }
        };
        candidates.into_iter().find(|path| path.exists())
    }

    /// The bundled bridge's version, where it says one (Renoise's tool does; REAPER's build gets a version file with
    /// the release pipeline).
    fn bundled_version(&self) -> Option<String> {
        let source = self.bundled_source()?;
        let text = match self.daw {
            Daw::Renoise => std::fs::read_to_string(source.join("payload").join("version.lua")).ok()?,
            Daw::Reaper => std::fs::read_to_string(source.with_extension("version")).ok()?,
        };
        let quoted = text.split('"').nth(1).unwrap_or(text.trim());
        detect::Version::parse(quoted).map(|_| quoted.to_string())
    }

    /// Puts the bridge in the DAW, or brings it up to date: its version, or what stopped it, in words.
    fn install(&self, env: &Env) -> Result<String, String> {
        let target = self.target(env)?;
        let source = self
            .bundled_source()
            .ok_or_else(|| format!("this Kumi doesn't carry {} for {}: build it, or install a release", self.bridge, self.name()))?;
        let version = self.bundled_version().unwrap_or_else(|| "dev".into());
        Lifecycle::new(&kumi_home(env), target).install(&source, &version).map_err(|why| why.to_string())?;
        Ok(version)
    }

    /// Whether this user has used the DAW (it has made its own folder: REAPER's resource folder, Renoise's preferences)
    /// or Kumi's bridge for it is installed: else the doctor has nothing to say of it.
    fn present(&self, env: &Env) -> bool {
        if self.receipt(env).is_some() {
            return true;
        }
        match self.daw {
            Daw::Reaper => detected(env, detect::detect_reaper).iter().any(|install| install.opened),
            Daw::Renoise => !detected(env, detect::detect_renoise).prefs.is_empty(),
        }
    }

    fn missing_words(&self) -> String {
        format!("{} isn't in {} yet, so Kumi can't see your {}", self.bridge, self.name(), self.profile().words.place)
    }
}

impl DawProvider for Own {
    fn id(&self) -> DawId {
        self.id
    }
    fn bridge_config(&self, env: &Env) -> Option<String> {
        self.receipt(env).map(|receipt| receipt.reference)
    }
    fn bundled_bridge_version(&self) -> Option<String> {
        self.bundled_version()
    }
    fn installed_bridge_version(&self, bridge_config: &str) -> Option<String> {
        let _ = bridge_config;
        let env: Env = std::env::vars().collect();
        self.receipt(&env).and_then(|receipt| receipt.version)
    }
    fn bridge_answers(&self, bridge_config: &str) -> LocalBoxFuture<'static, bool> {
        let _ = bridge_config;
        let daw = self.daw;
        let env: Env = std::env::vars().collect();
        let home = kumi_home(&env);
        async move {
            tokio::task::spawn_blocking(move || {
                let paths = BridgePaths::new(&home, daw);
                let Ok(secret) = files::read_secret(&paths.secret()) else { return false };
                files::scan_discovery(&paths.discovery(), daw).into_iter().filter(|found| found.alive).any(|found| {
                    let Ok(discovery) = found.discovery else { return false };
                    let address = std::net::SocketAddr::from(([127, 0, 0, 1], discovery.port));
                    daw_bridge::probe::probe(address, &secret, "kumi", Duration::from_secs(2)).is_ok()
                })
            })
            .await
            .unwrap_or(false)
        }
        .boxed_local()
    }
    fn running_now(&self) -> bool {
        !detect::running(self.daw).is_empty()
    }
    fn app_open(&self, _env: &Env) -> LocalBoxFuture<'static, bool> {
        let daw = self.daw;
        async move { tokio::task::spawn_blocking(move || !detect::running(daw).is_empty()).await.unwrap_or(false) }.boxed_local()
    }
    fn watch_words(&self) -> WatchWords {
        match self.daw {
            Daw::Reaper => WatchWords {
                starting: "REAPER is open; Kumi connects once its extension answers.",
                unreachable: "Kumi couldn't reach REAPER, so this is chat without it. Open REAPER with Kumi's extension installed (kumi bridge); Kumi connects by itself once it answers.",
                answered: "REAPER answered, so Kumi is connected to it now.",
            },
            Daw::Renoise => WatchWords {
                starting: "Renoise is open; Kumi connects once its tool answers.",
                unreachable: "Kumi couldn't reach Renoise, so this is chat without it. Open Renoise with Kumi's tool installed (kumi bridge); Kumi connects by itself once it answers.",
                answered: "Renoise answered, so Kumi is connected to it now.",
            },
        }
    }
    fn enable_step(&self) -> Option<EnableStep> {
        None
    }
    fn min_major(&self) -> u32 {
        self.min_major
    }
    fn integration(&self, context: IntegrationContext, session: &DawSession) -> Rc<dyn Integration> {
        (self.create)(OwnOptions { context, kumi_home: kumi_home(&session.env), connect: None })
    }
    fn probe(&self, _bridge_config: String, _factory: &DawFactory) -> LocalBoxFuture<'static, LiveProbe> {
        let create = self.create;
        let env: Env = std::env::vars().collect();
        async move {
            let integration =
                create(OwnOptions { context: IntegrationContext::new(Rc::new(|_, _| {})), kumi_home: kumi_home(&env), connect: None });
            if let Err(error) = integration.start(abort::timeout(20_000)).await {
                let _ = integration.close().await;
                return LiveProbe { started: false, said: Some(error.to_string()), ..Default::default() };
            }
            let probe = match integration.observe(abort::timeout(20_000), None).await {
                Ok(observation) if !observation.tools.is_empty() => LiveProbe {
                    started: true,
                    connected: Some(true),
                    set: observation.label.split(" — ").nth(1).map(str::to_owned),
                    ..Default::default()
                },
                _ => LiveProbe { started: true, connected: Some(false), ..Default::default() },
            };
            let _ = integration.close().await;
            probe
        }
        .boxed_local()
    }
    fn bridge_state(&self, _env: &Env, _found: bool, missing: bool, _bundled: Option<&str>, _command: &str) -> BridgeState {
        if !missing {
            return BridgeState::default();
        }
        let missing = self.missing_words();
        BridgeState {
            notice: Some(format!("{missing}; chatting without it. Run kumi bridge to put it in.")),
            why: Some(format!("{missing}.")),
        }
    }
    fn setup(
        &self,
        env: &Env,
        _bridge: Rc<RefCell<Option<String>>>,
        _given: bool,
        _controller: Rc<RefCell<Option<Weak<dyn SessionController>>>>,
    ) -> Rc<dyn DawSetup> {
        Rc::new(OwnSetup { own: self.id, env: env.clone() })
    }
    fn doctor_checks<'a>(&'a self, io: &'a DoctorIo, _node: &'a Check) -> LocalBoxFuture<'a, Vec<Check>> {
        if !self.present(&io.env) {
            return async { vec![] }.boxed_local();
        }
        let daw = self.daw;
        let env = io.env.clone();
        let bundled = self.bundled_source().and_then(|source| {
            let sha256 = daw_bridge::lifecycle::tree_sha256(&source).ok()?;
            Some(Bundled { version: self.bundled_version().unwrap_or_else(|| "dev".into()), sha256 })
        });
        async move {
            tokio::task::spawn_blocking(move || {
                let var = |name: &str| env.get(name).cloned();
                let home_dir = kumi_home(&env);
                let inputs = doctor::Inputs {
                    env: detect::Env { os: Os::current(), home: home(&env), var: &var, root: None },
                    kumi_home: &home_dir,
                    bundled,
                    running: &detect::running,
                    probe_timeout: Duration::from_secs(2),
                };
                doctor::doctor(daw, &inputs)
                    .into_iter()
                    .map(|check| match check.level {
                        Level::Ok => Check::ok(check.message),
                        Level::Note | Level::Warn => Check::note(check.message, None),
                        Level::Fail => Check::fix(check.message, None),
                    })
                    .collect()
            })
            .await
            .unwrap_or_default()
        }
        .boxed_local()
    }
    fn installation_checks<'a>(&'a self, env: &'a Env, _io: &'a InstallationIo, _platform: &'a str) -> LocalBoxFuture<'a, Vec<Check>> {
        if !self.present(env) {
            return async { vec![] }.boxed_local();
        }
        let check = match self.target(env) {
            Ok(_) => Check::ok(format!("{} is installed here.", self.name())),
            Err(why) => Check::note(format!("{}: {why}.", self.name()), None),
        };
        async move { vec![check] }.boxed_local()
    }
    fn extra_checks<'a>(&'a self, _io: &'a DoctorIo) -> LocalBoxFuture<'a, Result<Vec<Check>, RuntimeError>> {
        async { Ok(vec![]) }.boxed_local()
    }
    fn report_log<'a>(&'a self, env: &'a Env, _home: &'a str, logs: Option<&'a LogFiles>) -> LocalBoxFuture<'a, (String, String)> {
        let heading = format!("{}'s bridge log", self.name());
        let folder = BridgePaths::new(&kumi_home(env), self.daw).logs();
        async move {
            let lines = match logs {
                Some(logs) => logs().await.unwrap_or_default(),
                None => {
                    let mut lines = vec![];
                    for entry in std::fs::read_dir(&folder).into_iter().flatten().flatten() {
                        if let Ok(text) = std::fs::read_to_string(entry.path()) {
                            lines.extend(text.lines().rev().take(200).map(str::to_owned).collect::<Vec<_>>().into_iter().rev());
                        }
                    }
                    lines
                }
            };
            (heading, lines.join("\n"))
        }
        .boxed_local()
    }
    fn after_update<'a>(
        &'a self,
        io: &'a InstalledIo,
        _home: &'a str,
        _app: &'a str,
        _back_to: Option<&'a str>,
    ) -> LocalBoxFuture<'a, i32> {
        async move {
            if self.receipt(&io.env).is_some() {
                if let Err(why) = self.install(&io.env) {
                    io.out.write(&format!("{} wasn't brought up to date: {why}. Run kumi bridge to try again.\n", self.bridge));
                }
            }
            0
        }
        .boxed_local()
    }
    fn after_checkout_update<'a>(&'a self, io: &'a UpdateIo, _run: Run, _repo: &'a str) -> LocalBoxFuture<'a, i32> {
        async move {
            if self.receipt(&io.env).is_some() {
                let _ = self.install(&io.env);
            }
            0
        }
        .boxed_local()
    }
    fn prepare_rollback<'a>(
        &'a self,
        _io: &'a InstalledIo,
        _home: &'a str,
        _previous: &'a str,
        _version: &'a str,
        _legacy: bool,
    ) -> LocalBoxFuture<'a, Result<Box<dyn BridgeRollbackStep>, RuntimeError>> {
        // An earlier Kumi leaves this DAW's bridge where it is: it never touches what it doesn't know.
        async { Ok(Box::new(KeepBridge) as Box<dyn BridgeRollbackStep>) }.boxed_local()
    }
    fn finish_transition<'a>(&'a self, _io: &'a InstalledIo) -> LocalBoxFuture<'a, ()> {
        async {}.boxed_local()
    }
    fn setup_bridge(
        &self,
        out: Rc<dyn TtyOutput>,
        env: &Env,
        _command: BridgeCommand,
    ) -> LocalBoxFuture<'static, Result<i32, RuntimeError>> {
        let said = match self.install(env) {
            Ok(version) => (
                0,
                format!(
                    "{} {version} is in {}. {} loads it when it starts: restart {} if it's open.\n",
                    self.bridge,
                    self.name(),
                    self.name(),
                    self.name()
                ),
            ),
            Err(why) => (1, format!("{} couldn't go in {}: {why}.\n", self.bridge, self.name())),
        };
        async move {
            out.write(&said.1);
            Ok(said.0)
        }
        .boxed_local()
    }
    fn bridge_installed(&self, env: &Env) -> bool {
        self.receipt(env).is_some()
    }
    fn bridge_folder(&self) -> &'static str {
        match self.daw {
            Daw::Reaper => "bridges/reaper",
            Daw::Renoise => "bridges/renoise",
        }
    }
    fn bridge_files_in_use(&self, _folder: &str, _env: &Env) -> bool {
        // The DAW loads its copy from its own folder, not Kumi's.
        false
    }
    fn remove_bridge<'a>(&'a self, io: &'a InstalledIo, _run: Run) -> LocalBoxFuture<'a, Result<RemovedBridge, RuntimeError>> {
        async move {
            if self.receipt(&io.env).is_none() {
                return Ok(RemovedBridge::None);
            }
            let target = self.target(&io.env).map_err(RuntimeError::plain)?;
            match Lifecycle::new(&kumi_home(&io.env), target).uninstall(true) {
                Ok(_) => Ok(RemovedBridge::Removed),
                Err(why) => {
                    io.out.write(&format!("{} stays in {}: {why}.\n", self.bridge, self.name()));
                    Ok(RemovedBridge::Kept)
                }
            }
        }
        .boxed_local()
    }
    fn removal_question(&self) -> &'static str {
        match self.daw {
            Daw::Reaper => "take Kumi's extension out of REAPER too",
            Daw::Renoise => "take Kumi's tool out of Renoise too",
        }
    }
    fn files_stay(&self, folder: &str) -> String {
        format!("{}'s files stay in {folder}.", self.bridge)
    }
    fn toggle(&self, _env: &Env) -> Option<Toggle> {
        None
    }
    fn toggle_words(&self) -> Option<ToggleWords> {
        None
    }
    fn without_daw(&self, on_connection: Rc<dyn Fn(ConnectionState)>) -> Rc<dyn Integration> {
        kumi_runtime::integrations::none::create_no_daw_integration(*self.profile(), self.instructions, on_connection)
    }
}

/// An earlier Kumi doesn't know this DAW's bridge, so a rollback leaves it as it is.
struct KeepBridge;

#[async_trait(?Send)]
impl BridgeRollbackStep for KeepBridge {
    async fn before_swap(&self) -> Result<(), RuntimeError> {
        Ok(())
    }
    async fn swap_failed(&self) -> Option<String> {
        None
    }
    async fn after_swap(&self, _io: &InstalledIo, _home: &str, _app: &str, _version: &str, _native: bool) -> i32 {
        0
    }
}

/// Setup's DAW side for REAPER and Renoise: the bridge goes in with the DAW closed, and the DAW loads it when it opens.
struct OwnSetup {
    own: DawId,
    env: Env,
}

impl OwnSetup {
    fn own(&self) -> &'static Own {
        match self.own {
            DawId::Reaper => &REAPER,
            _ => &RENOISE,
        }
    }
    fn app(&self) -> Option<PathBuf> {
        match self.own().daw {
            Daw::Reaper => detected(&self.env, detect::detect_reaper).into_iter().find_map(|install| install.app),
            Daw::Renoise => detected(&self.env, detect::detect_renoise).apps.into_iter().map(|app| app.path).next(),
        }
    }
}

#[async_trait(?Send)]
impl DawSetup for OwnSetup {
    async fn open_live(&self) -> Option<String> {
        let daw = self.own().daw;
        let open = tokio::task::spawn_blocking(move || !detect::running(daw).is_empty()).await.unwrap_or(false);
        open.then(|| self.app()).flatten().map(|app| app.to_string_lossy().into_owned())
    }
    async fn ask_to_quit(&self) -> bool {
        // Kumi never scripts the DAW's menus: the producer quits it, and saves first if it asks.
        false
    }
    fn can_ask_to_quit(&self) -> bool {
        false
    }
    async fn closed(&self, stop: &Signal) -> bool {
        let daw = self.own().daw;
        loop {
            if tokio::task::spawn_blocking(move || detect::running(daw).is_empty()).await.unwrap_or(false) {
                return true;
            }
            if stop.is_cancelled() {
                return false;
            }
            tokio::select! { _ = tokio::time::sleep(Duration::from_millis(500)) => {}, _ = stop.cancelled() => return false }
        }
    }
    async fn install(&self) -> Result<String, String> {
        self.own().install(&self.env)
    }
    async fn start(&self, app: Option<String>, _set: Option<String>) -> bool {
        let Some(app) = app.map(PathBuf::from).or_else(|| self.app()) else { return false };
        let mut command = if cfg!(target_os = "macos") {
            let mut open = std::process::Command::new("open");
            open.arg("-a").arg(&app);
            open
        } else {
            std::process::Command::new(&app)
        };
        command.spawn().is_ok()
    }
    async fn connect(&self) {}
    async fn version(&self, _app: Option<String>) -> Option<String> {
        match self.own().daw {
            Daw::Reaper => detected(&self.env, detect::detect_reaper).into_iter().find_map(|install| install.version),
            Daw::Renoise => detected(&self.env, detect::detect_renoise).apps.into_iter().find_map(|app| app.version),
        }
    }
    async fn installed(&self) -> Installed {
        match self.own().target(&self.env) {
            Ok(_) => Installed::Ready,
            Err(why) if why.contains("open") => Installed::NeverOpened(String::new()),
            Err(_) => Installed::Missing,
        }
    }
}
