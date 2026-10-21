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
    install::{reaper_extension_name, reaper_target, renoise_target, renoise_targets, unloadable, TargetError, RENOISE_TOOL_FOLDER},
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

/// What putting the bridge in did: its version, whether the DAW's copy changed (a DAW that's open loads the new one when
/// it restarts), and where Kumi kept the copy that was there, when it had changed since Kumi put it in.
struct Put {
    version: String,
    changed: bool,
    kept: Option<String>,
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
        self.placed(env).map_err(|why| why.map(|why| why.to_string()).unwrap_or_else(|| format!("{} isn't installed", self.name())))
    }

    /// `target`, with why it can't go yet as detection says it; None when the DAW isn't installed.
    fn placed(&self, env: &Env) -> Result<Target, Option<TargetError>> {
        match self.daw {
            Daw::Reaper => {
                let installs = detected(env, detect::detect_reaper);
                let install = installs.iter().find(|install| install.opened).or(installs.first()).ok_or(None)?;
                reaper_target(install, Os::current()).map_err(Some)
            }
            Daw::Renoise => renoise_target(&detected(env, detect::detect_renoise)).map_err(Some),
        }
    }

    fn receipt(&self, env: &Env) -> Option<Receipt> {
        let paths = BridgePaths::new(&kumi_home(env), self.daw);
        let text = std::fs::read_to_string(paths.receipt()).ok()?;
        serde_json::from_str::<Receipt>(&text).ok().filter(|receipt| receipt.status == "installed")
    }

    /// Where the receipt says the bridge went.
    fn receipt_target(&self, receipt: &Receipt) -> Target {
        let target = Path::new(&receipt.target);
        Target {
            daw: self.daw,
            dir: target.parent().map(Path::to_path_buf).unwrap_or_default(),
            name: target.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default(),
            folder: self.daw == Daw::Renoise,
            reference: PathBuf::from(&receipt.reference),
        }
    }

    /// Renoise's version folders from 3.5, where a newer Renoise may have copied Kumi's tool: places for the lifecycle
    /// to follow a copy of Kumi's to, and to take it out of. REAPER has none.
    fn elsewhere(&self, env: &Env) -> Vec<Target> {
        match self.daw {
            Daw::Renoise => renoise_targets(&detected(env, detect::detect_renoise)),
            Daw::Reaper => vec![],
        }
    }

    /// Where an update or a rollback brings the bridge: where Kumi put it, as its receipt says (a portable REAPER, a
    /// resource folder named once with KUMI_REAPER_RESOURCE_DIR, the Renoise version folder chosen at setup), with each
    /// copy of Kumi's in Renoise's other version folders brought along; never into a DAW nobody chose. Err, in words,
    /// when nothing of Kumi's is left where it was (the producer took it out, or the folder's gone): an update doesn't
    /// put it back.
    fn follow(&self, env: &Env) -> Result<Lifecycle, String> {
        let home = kumi_home(env);
        let receipt = self.receipt(env).ok_or_else(|| self.missing_words())?;
        let at = self.receipt_target(&receipt);
        let path = at.path();
        let found = Lifecycle::new(&home, at).with_elsewhere(self.elsewhere(env)).found().map_err(|why| why.to_string())?;
        let found = found.ok_or_else(|| {
            format!(
                "{} isn't in {} any more ({}), so it was left as it is: {} --daw {} bridge puts it back",
                self.bridge,
                self.name(),
                path.display(),
                *kumi_runtime::KUMI,
                self.id.as_str()
            )
        })?;
        Ok(Lifecycle::new(&home, found).with_elsewhere(self.elsewhere(env)))
    }

    /// The bridge the Kumi in `app` (its app folder) brings, laid out as a release lays it out (`daws/<daw>/`).
    fn carried_by(&self, app: &Path) -> Option<PathBuf> {
        let path = match self.daw {
            Daw::Reaper => app.join("daws").join("reaper").join(reaper_extension_name(Os::current())),
            Daw::Renoise => app.join("daws").join("renoise").join(RENOISE_TOOL_FOLDER),
        };
        path.exists().then_some(path)
    }

    /// The bridge this Kumi brings: beside Kumi in a release, or this checkout's own build.
    fn bundled_source(&self) -> Option<PathBuf> {
        let exe = std::env::current_exe().ok()?;
        let beside = exe.parent()?.to_path_buf();
        let checkout = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let built = match self.daw {
            Daw::Reaper => {
                let name = reaper_extension_name(Os::current());
                beside.join(if cfg!(windows) { name.to_string() } else { format!("lib{name}") })
            }
            Daw::Renoise => checkout.join("daws").join("renoise").join(RENOISE_TOOL_FOLDER),
        };
        self.carried_by(&beside).or_else(|| built.exists().then_some(built))
    }

    /// The bundled bridge's version, where it says one (Renoise's tool does; REAPER's build gets a version file with
    /// the release pipeline).
    fn bundled_version(&self) -> Option<String> {
        self.bundled_source().and_then(|source| self.version_of(&source))
    }

    fn version_of(&self, source: &Path) -> Option<String> {
        let text = match self.daw {
            Daw::Renoise => std::fs::read_to_string(source.join("payload").join("version.lua")).ok()?,
            Daw::Reaper => std::fs::read_to_string(source.with_extension("version")).ok()?,
        };
        let quoted = text.split('"').nth(1).unwrap_or(text.trim());
        detect::Version::parse(quoted).map(|_| quoted.to_string())
    }

    /// Puts the bridge this Kumi brings in the DAW, where detection finds it, or what stopped it, in words.
    fn install(&self, env: &Env) -> Result<Put, String> {
        let source = self
            .bundled_source()
            .ok_or_else(|| format!("this Kumi doesn't carry {} for {}: build it, or install a release", self.bridge, self.name()))?;
        let target = self.target(env)?;
        // A REAPER that can't load it (a 32-bit one, say) is said, and nothing goes in.
        if self.daw == Daw::Reaper {
            let installs = detected(env, detect::detect_reaper);
            if let Some(why) =
                installs.iter().find(|install| install.opened).or(installs.first()).and_then(|install| unloadable(&source, install))
            {
                return Err(why);
            }
        }
        self.put(Lifecycle::new(&kumi_home(env), target).with_elsewhere(self.elsewhere(env)), &source, false)
    }

    /// Puts `source` at the lifecycle's target, and in each copy of Kumi's it follows (`rollback`: recorded as one): a
    /// copy it keeps already goes back rather than in again, and nothing changes when it's the one in place.
    fn put(&self, life: Lifecycle, source: &Path, rollback: bool) -> Result<Put, String> {
        let version = self.version_of(source).unwrap_or_else(|| "dev".into());
        let before = life.receipt().ok().flatten();
        let receipt = if rollback { life.rollback_to(source, &version) } else { life.install(source, &version) };
        let receipt = receipt.map_err(|why| why.to_string())?;
        let changed = before.as_ref() != Some(&receipt);
        let kept = receipt.history.first().filter(|_| changed).and_then(|row| row.kept.clone());
        Ok(Put { version, changed, kept })
    }

    /// The words for a copy Kumi kept before replacing it, else nothing.
    fn kept_words(&self, put: &Put) -> String {
        put.kept
            .as_ref()
            .map(|kept| format!("The copy that was in {} had changed since Kumi put it there: it's kept in {kept}.\n", self.name()))
            .unwrap_or_default()
    }

    /// What to tell the producer when the Kumi in `app` (`version`) doesn't bring this DAW's bridge: it stays in the DAW,
    /// and that Kumi neither uses nor removes it.
    fn left_behind(&self, env: &Env, app: &Path, version: &str) -> Option<String> {
        let receipt = self.receipt(env).filter(|_| self.carried_by(app).is_none())?;
        Some(format!(
            "{} stays in {} ({}): Kumi {version} doesn't know {}, so it won't use it or remove it. Delete that {} to take it out.\n",
            self.bridge,
            self.name(),
            receipt.target,
            self.name(),
            if self.daw == Daw::Renoise { "folder" } else { "file" },
        ))
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
    fn installed_bridge_version(&self, bridge_config: &str, env: &Env) -> Option<String> {
        let _ = bridge_config;
        self.receipt(env).and_then(|receipt| receipt.version)
    }
    fn bridge_answers(&self, bridge_config: &str, env: &Env) -> LocalBoxFuture<'static, bool> {
        let _ = bridge_config;
        let daw = self.daw;
        let home = kumi_home(env);
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
                unreachable: "Kumi couldn't reach REAPER, so this is chat without it. Open REAPER with Kumi's extension installed (kumi --daw reaper bridge); Kumi connects by itself once it answers.",
                answered: "REAPER answered, so Kumi is connected to it now.",
            },
            Daw::Renoise => WatchWords {
                starting: "Renoise is open; Kumi connects once its tool answers.",
                unreachable: "Kumi couldn't reach Renoise, so this is chat without it. Open Renoise with Kumi's tool installed (kumi --daw renoise bridge); Kumi connects by itself once it answers.",
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
        let connect = session.factory.own_host.as_ref().map(|host| host(self.id));
        (self.create)(OwnOptions { context, kumi_home: kumi_home(&session.env), connect })
    }
    fn probe(&self, _bridge_config: String, _factory: &DawFactory, env: &Env) -> LocalBoxFuture<'static, LiveProbe> {
        let create = self.create;
        let home = kumi_home(env);
        async move {
            let integration = create(OwnOptions { context: IntegrationContext::new(Rc::new(|_, _| {})), kumi_home: home, connect: None });
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
    fn bridge_state(&self, _env: &Env, _found: bool, missing: bool, _bundled: Option<&str>, command: &str) -> BridgeState {
        if !missing {
            return BridgeState::default();
        }
        let missing = self.missing_words();
        BridgeState {
            notice: Some(format!("{missing}; chatting without it. Run {command} --daw {} bridge to put it in.", self.id.as_str())),
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
        // The DAW is here, but Kumi's bridge was never put in: a note, nothing to fix.
        if self.receipt(&io.env).is_none() {
            let note = Check::note(
                format!("{} isn't in {}", self.bridge, self.name()),
                Some(format!("{} --daw {} bridge puts it in, if you want Kumi in {}", *kumi_runtime::KUMI, self.id.as_str(), self.name())),
            );
            return async move { vec![note] }.boxed_local();
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
    fn after_update<'a>(&'a self, io: &'a InstalledIo, _home: &'a str, app: &'a str, back_to: Option<&'a str>) -> LocalBoxFuture<'a, i32> {
        async move {
            if self.receipt(&io.env).is_none() {
                return 0;
            }
            // The bridge the new Kumi brings, from its app folder: the running Kumi may already be the old one's.
            let Some(source) = self.carried_by(Path::new(app)) else {
                if let Some(said) = back_to.and_then(|version| self.left_behind(&io.env, Path::new(app), version)) {
                    io.out.write(&said);
                }
                return 0;
            };
            // Taken out by the producer: said, and left as it is.
            let life = match self.follow(&io.env) {
                Ok(life) => life,
                Err(why) => {
                    io.out.write(&format!("{why}.\n"));
                    return 0;
                }
            };
            match self.put(life, &source, false) {
                Ok(put) if put.changed => io.out.write(&format!(
                    "{} {} is in {} now: restart {} if it's open.\n{}",
                    self.bridge,
                    put.version,
                    self.name(),
                    self.name(),
                    self.kept_words(&put)
                )),
                // The bridge in place is the one this Kumi brings.
                Ok(_) => {}
                Err(why) => io.out.write(&format!(
                    "{} wasn't brought up to date: {why}. Run {} --daw {} bridge to try again.\n",
                    self.bridge,
                    *kumi_runtime::KUMI,
                    self.id.as_str()
                )),
            }
            0
        }
        .boxed_local()
    }
    fn after_checkout_update<'a>(&'a self, io: &'a UpdateIo, _run: Run, _repo: &'a str) -> LocalBoxFuture<'a, i32> {
        async move {
            if let (Some(_), Some(source)) = (self.receipt(&io.env), self.bundled_source()) {
                let _ = self.follow(&io.env).and_then(|life| self.put(life, &source, false));
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
        // Nothing moves before the swap: the bridge follows Kumi once it's back (see OwnRollback).
        async move { Ok(Box::new(OwnRollback(self.id)) as Box<dyn BridgeRollbackStep>) }.boxed_local()
    }
    fn finish_transition<'a>(&'a self, _io: &'a InstalledIo) -> LocalBoxFuture<'a, ()> {
        async {}.boxed_local()
    }
    fn setup_bridge(
        &self,
        out: Rc<dyn TtyOutput>,
        env: &Env,
        command: BridgeCommand,
    ) -> LocalBoxFuture<'static, Result<i32, RuntimeError>> {
        let (own, env) = (own_of(self.id), env.clone());
        async move {
            // Asked before anything is written into the DAW's folder (--yes answers beforehand). Without a place yet,
            // the install below says why.
            if let (false, Ok(target)) = (command.yes, own.target(&env)) {
                let question = format!("Put {} in {} ({})?", own.bridge, own.name(), target.path().display());
                if !crate::process::ask_yes_no(command.input.clone(), out.clone(), &question, Some("add --yes")).await {
                    out.write(&format!(
                        "Nothing was changed. To put it in later: {} --daw {} bridge\n",
                        *kumi_runtime::KUMI,
                        own.id.as_str()
                    ));
                    return Ok(1);
                }
            }
            let (code, said) = match own.install(&env) {
                Ok(put) if put.changed => (
                    0,
                    format!(
                        "{} {} is in {}. {} loads it when it starts: restart {} if it's open.\n{}",
                        own.bridge,
                        put.version,
                        own.name(),
                        own.name(),
                        own.name(),
                        own.kept_words(&put)
                    ),
                ),
                Ok(put) => (0, format!("{} {} is in {} already.\n", own.bridge, put.version, own.name())),
                Err(why) => (1, format!("{} couldn't go in {}: {why}.\n", own.bridge, own.name())),
            };
            out.write(&said);
            Ok(code)
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
            let Some(receipt) = self.receipt(&io.env) else {
                return Ok(RemovedBridge::None);
            };
            // Where the receipt says it went: the DAW's folder may be gone (Renoise's preferences, a portable REAPER).
            let target = self.receipt_target(&receipt);
            let path = target.path().display().to_string();
            match Lifecycle::new(&kumi_home(&io.env), target).uninstall(true) {
                Ok(receipt) => {
                    let row = receipt.history.first();
                    let mut said = format!("{} is out of {} ({path}).\n", self.bridge, self.name());
                    if let Some(kept) = row.and_then(|row| row.kept.as_ref()) {
                        said.push_str(&format!(
                            "The copy that was in {} had changed since Kumi put it there: it's kept in {kept}.\n",
                            self.name()
                        ));
                    }
                    if let Some(left) = row.and_then(|row| row.left.as_ref()) {
                        said.push_str(&format!(
                            "{} still holds an old copy, {left}: close {}, then delete it.\n",
                            self.name(),
                            self.name()
                        ));
                    }
                    io.out.write(&said);
                    Ok(RemovedBridge::Removed)
                }
                Err(why) => {
                    io.out.write(&format!("{} couldn't be taken out of {} ({why}): delete {path} by hand.\n", self.bridge, self.name()));
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

/// A rollback's step for a DAW whose bridge is Kumi's own: once Kumi is back, the bridge goes to the copy that Kumi
/// carries (the generation kept for it, as a rollback in the receipt); an earlier Kumi that doesn't know this DAW
/// leaves the bridge where it is, and says so.
struct OwnRollback(DawId);

#[async_trait(?Send)]
impl BridgeRollbackStep for OwnRollback {
    async fn before_swap(&self) -> Result<(), RuntimeError> {
        Ok(())
    }
    async fn swap_failed(&self) -> Option<String> {
        None
    }
    async fn after_swap(&self, io: &InstalledIo, _home: &str, app: &str, version: &str, _native: bool) -> i32 {
        let own = own_of(self.0);
        // An earlier Kumi that doesn't know this DAW refuses to start while KUMI_DAW names it (1.19 and before).
        let asked = io.env.get("KUMI_DAW").is_some_and(|daw| daw.eq_ignore_ascii_case(own.id.as_str()));
        if asked && own.carried_by(Path::new(app)).is_none() {
            io.out.write(&format!(
                "KUMI_DAW is set to {} where you run Kumi, and Kumi {version} doesn't know {}: it won't start while it's set. Unset KUMI_DAW to use Kumi {version}.\n",
                own.id.as_str(),
                own.name()
            ));
        }
        if own.receipt(&io.env).is_none() {
            return 0;
        }
        let Some(source) = own.carried_by(Path::new(app)) else {
            if let Some(said) = own.left_behind(&io.env, Path::new(app), version) {
                io.out.write(&said);
            }
            return 0;
        };
        let life = match own.follow(&io.env) {
            Ok(life) => life,
            Err(why) => {
                io.out.write(&format!("{why}.\n"));
                return 0;
            }
        };
        match own.put(life, &source, true) {
            Ok(put) if put.changed => io.out.write(&format!(
                "{} {} is back in {}, as Kumi {version} had it: restart {} if it's open.\n{}",
                own.bridge,
                put.version,
                own.name(),
                own.name(),
                own.kept_words(&put)
            )),
            Ok(_) => {}
            Err(why) => io.out.write(&format!("{} stays as it was in {}: {why}.\n", own.bridge, own.name())),
        }
        0
    }
}

fn own_of(id: DawId) -> &'static Own {
    match id {
        DawId::Reaper => &REAPER,
        _ => &RENOISE,
    }
}

/// Setup's DAW side for REAPER and Renoise: the bridge goes in with the DAW closed, and the DAW loads it when it opens.
struct OwnSetup {
    own: DawId,
    env: Env,
}

impl OwnSetup {
    fn own(&self) -> &'static Own {
        own_of(self.own)
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
        self.own().install(&self.env).map(|put| put.version)
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
        match self.own().placed(&self.env) {
            Ok(_) => Installed::Ready,
            Err(Some(TargetError::NeverOpened(_))) => Installed::NeverOpened(String::new()),
            Err(None) => Installed::Missing,
        }
    }
}
