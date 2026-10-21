//! REAPER, Renoise and Logic Pro as the app works with them: Kumi's own bridge for each (REAPER's extension, Renoise's
//! tool, the bridge beside Logic Pro), found, installed, checked and taken out through the shared lifecycle
//! (daw-bridge), and the session's integration through Kumi's host. One provider serves the three; each DAW gives its
//! words and its integration. REAPER and Renoise load their bridge from their own folder when they start; Logic loads
//! nothing, so its bridge lives in Kumi's own folder and the host starts it beside Logic when a session begins.
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
    install::{
        self, reaper_extension_name, reaper_target, renoise_target, TargetError, LOGIC_BINARY, LOGIC_BRIDGE_FOLDER, LOGIC_HELPER_PREFIX,
        LOGIC_VERSION_FILE, RENOISE_TOOL_FOLDER,
    },
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
    /// The bridge, as the producer knows it: "Kumi's extension", "Kumi's tool", "Kumi's Logic Pro bridge".
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

pub static LOGIC: Own = Own {
    id: DawId::Logic,
    daw: Daw::Logic,
    bridge: "Kumi's Logic Pro bridge",
    min_major: 12,
    create: kumi_runtime::integrations::logic::create_logic_integration,
    instructions: "You are Kumi, a concise, practical producer collaborator. Logic Pro isn't connected, so Kumi can't see your project: answer from what you know and from Apple's Logic Pro User Guide (logic_manual), and say so.",
};

/// Names a folder laid out as a release lays Logic's bridge out (`logic-kumi`, a `kumi-logic-hands-*` helper and
/// `version`) to set the bridge up from, instead of the one beside Kumi or this checkout's build.
pub const LOGIC_BRIDGE_SOURCE_VAR: &str = "KUMI_LOGIC_BRIDGE_SOURCE";

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

/// The newest Logic Pro on this Mac, by version (the usual one when none says).
fn best_logic(env: &Env) -> Option<detect::LogicInstall> {
    let installs = detected(env, detect::detect_logic);
    installs.iter().max_by_key(|install| install.version).cloned().or_else(|| installs.into_iter().next())
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
            // Logic loads nothing: its bridge goes in Kumi's own folder, which waits on nothing.
            Daw::Logic => Ok(install::logic_target(&BridgePaths::new(&kumi_home(env), self.daw))),
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
            folder: self.is_folder(),
            reference: PathBuf::from(&receipt.reference),
        }
    }

    /// Where an update or a rollback brings the bridge: REAPER's where Kumi put it, while that folder's there (a
    /// portable REAPER, or a resource folder named once with KUMI_REAPER_RESOURCE_DIR); Renoise's in the version folder
    /// detection finds, as a newer Renoise makes its own, else where Kumi put it.
    fn update_target(&self, env: &Env) -> Result<Target, String> {
        let placed = self.receipt(env).map(|receipt| self.receipt_target(&receipt)).filter(|target| target.dir.is_dir());
        match (self.daw, placed) {
            (Daw::Reaper, Some(placed)) => Ok(placed),
            (_, placed) => self.target(env).or_else(|why| placed.ok_or(why)),
        }
    }

    /// The bridge the Kumi in `app` (its app folder) brings, laid out as a release lays it out (`daws/<daw>/`).
    fn carried_by(&self, app: &Path) -> Option<PathBuf> {
        let path = match self.daw {
            Daw::Reaper => app.join("daws").join("reaper").join(reaper_extension_name(Os::current())),
            Daw::Renoise => app.join("daws").join("renoise").join(RENOISE_TOOL_FOLDER),
            Daw::Logic => app.join("daws").join("logic").join(LOGIC_BRIDGE_FOLDER),
        };
        path.exists().then_some(path)
    }

    /// The bridge this Kumi brings: beside Kumi in a release, or this checkout's own build (Logic's, once staged by
    /// `logic_source`).
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
            Daw::Logic => logic_staging(&checkout),
        };
        self.carried_by(&beside).or_else(|| built.exists().then_some(built))
    }

    /// The bundled bridge's version, where it says one (Renoise's tool does; REAPER's build gets a version file with
    /// the release pipeline; Logic's folder carries one).
    fn bundled_version(&self) -> Option<String> {
        self.bundled_source().and_then(|source| self.version_of(&source))
    }

    fn version_of(&self, source: &Path) -> Option<String> {
        let text = match self.daw {
            Daw::Renoise => std::fs::read_to_string(source.join("payload").join("version.lua")).ok()?,
            Daw::Reaper => std::fs::read_to_string(source.with_extension("version")).ok()?,
            Daw::Logic => std::fs::read_to_string(source.join(LOGIC_VERSION_FILE)).ok()?,
        };
        let quoted = text.split('"').nth(1).unwrap_or(text.trim());
        detect::Version::parse(quoted).map(|_| quoted.to_string())
    }

    /// The folder Logic's bridge is set up from: the one beside Kumi in a release, the one `KUMI_LOGIC_BRIDGE_SOURCE`
    /// names, else this checkout's own build, staged as a release lays it out (its helper built when it isn't yet).
    fn logic_source(&self, env: &Env) -> Result<PathBuf, String> {
        let beside = std::env::current_exe().ok().and_then(|exe| exe.parent().map(Path::to_path_buf));
        if let Some(carried) = beside.as_deref().and_then(|beside| self.carried_by(beside)) {
            return Ok(carried);
        }
        if let Some(named) = env.get(LOGIC_BRIDGE_SOURCE_VAR).filter(|named| !named.is_empty()) {
            let source = PathBuf::from(named);
            return if source.join(LOGIC_BINARY).is_file() {
                Ok(source)
            } else {
                Err(format!("{LOGIC_BRIDGE_SOURCE_VAR} names {named}, which holds no {LOGIC_BINARY}"))
            };
        }
        stage_logic_bridge(beside.as_deref())
    }

    /// Puts the bridge this Kumi brings in the DAW (Logic's in Kumi's own folder), where detection finds it, or what
    /// stopped it, in words.
    fn install(&self, env: &Env) -> Result<Put, String> {
        let source = match self.daw {
            Daw::Logic => self.logic_source(env)?,
            _ => self
                .bundled_source()
                .ok_or_else(|| format!("this Kumi doesn't carry {} for {}: build it, or install a release", self.bridge, self.name()))?,
        };
        self.put(self.target(env)?, env, &source, false)
    }

    /// Puts `source` at `target` through the lifecycle (`rollback`: recorded as one): a copy it keeps already goes back
    /// rather than in again, and nothing changes when it's the one in place.
    fn put(&self, target: Target, env: &Env, source: &Path, rollback: bool) -> Result<Put, String> {
        let version = self.version_of(source).unwrap_or_else(|| "dev".into());
        let life = Lifecycle::new(&kumi_home(env), target);
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
            .map(|kept| format!("The copy that was in {} had changed since Kumi put it there: it's kept in {kept}.\n", self.place_words()))
            .unwrap_or_default()
    }

    /// Whether the bridge is a folder (Renoise's tool, Logic's bridge with its helper) rather than one file.
    fn is_folder(&self) -> bool {
        matches!(self.daw, Daw::Renoise | Daw::Logic)
    }

    /// Where the bridge sits, as the producer knows it: in the DAW, or (Logic's) in Kumi's own folder.
    fn place_words(&self) -> String {
        match self.daw {
            Daw::Logic => "Kumi's own folder".into(),
            _ => self.name().into(),
        }
    }

    /// What to tell the producer when the Kumi in `app` (`version`) doesn't bring this DAW's bridge: it stays where it
    /// is, and that Kumi neither uses nor removes it.
    fn left_behind(&self, env: &Env, app: &Path, version: &str) -> Option<String> {
        let receipt = self.receipt(env).filter(|_| self.carried_by(app).is_none())?;
        Some(format!(
            "{} stays in {} ({}): Kumi {version} doesn't know {}, so it won't use it or remove it. Delete that {} to take it out.\n",
            self.bridge,
            self.place_words(),
            receipt.target,
            self.name(),
            if self.is_folder() { "folder" } else { "file" },
        ))
    }

    /// Whether this user has the DAW (it has made its own folder: REAPER's resource folder, Renoise's preferences;
    /// Logic Pro is installed) or Kumi's bridge for it is installed: else the doctor has nothing to say of it.
    fn present(&self, env: &Env) -> bool {
        if self.receipt(env).is_some() {
            return true;
        }
        match self.daw {
            Daw::Reaper => detected(env, detect::detect_reaper).iter().any(|install| install.opened),
            Daw::Renoise => !detected(env, detect::detect_renoise).prefs.is_empty(),
            Daw::Logic => !detected(env, detect::detect_logic).is_empty(),
        }
    }

    /// The bridge isn't there: in the DAW, or (Logic's) set up at all.
    fn not_in_words(&self) -> String {
        match self.daw {
            Daw::Logic => format!("{} isn't set up", self.bridge),
            _ => format!("{} isn't in {}", self.bridge, self.name()),
        }
    }

    fn missing_words(&self) -> String {
        format!("{} yet, so Kumi can't see your {}", self.not_in_words(), self.profile().words.place)
    }

    /// What `kumi --daw <daw> bridge` does, as a verb: puts the bridge in the DAW, or sets Logic's up.
    fn setup_verb(&self) -> &'static str {
        match self.daw {
            Daw::Logic => "set it up",
            _ => "put it in",
        }
    }

    /// The same, as the doctor says it of the command: "puts it in", "sets it up".
    fn setup_verb_does(&self) -> &'static str {
        match self.daw {
            Daw::Logic => "sets it up",
            _ => "puts it in",
        }
    }
}

/// Where this checkout stages Logic's bridge as a release lays it out.
fn logic_staging(checkout: &Path) -> PathBuf {
    checkout.join("target").join("logic-bridge").join(LOGIC_BRIDGE_FOLDER)
}

/// The Accessibility helper in `dir`: the newest `kumi-logic-hands-*` file there.
fn helper_in(dir: &Path) -> Option<PathBuf> {
    let mut helpers: Vec<PathBuf> = std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_file() && path.file_name().is_some_and(|name| name.to_string_lossy().starts_with(LOGIC_HELPER_PREFIX)))
        .collect();
    helpers.sort_by_key(|path| std::fs::metadata(path).and_then(|meta| meta.modified()).ok());
    helpers.pop()
}

/// The bridge's version, as its binary says it (`logic-kumi 0.1.0`).
fn bridge_version(binary: &Path) -> Option<String> {
    let output = std::process::Command::new(binary).arg("--version").output().ok()?;
    let text = String::from_utf8_lossy(&output.stdout);
    text.split_whitespace().rev().find_map(|word| detect::Version::parse(word).map(|_| word.to_string()))
}

/// This checkout's build of Logic's bridge, staged as a release lays it out, in `target/logic-bridge/logic-kumi/`: the
/// bridge binary (`cargo build -p logic-kumi`, beside this Kumi or in the checkout's target), the Accessibility helper
/// (`scripts/build-logic-hands.py` builds it when it isn't there yet) and the version file. Staged afresh each time, so
/// what's installed is the build of now.
fn stage_logic_bridge(beside: Option<&Path>) -> Result<PathBuf, String> {
    let checkout = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let target = checkout.join("target");
    let binary = beside
        .map(|beside| beside.join(LOGIC_BINARY))
        .into_iter()
        .chain([target.join("release").join(LOGIC_BINARY), target.join("debug").join(LOGIC_BINARY)])
        .find(|path| path.is_file())
        .ok_or_else(|| "this checkout hasn't built Kumi's Logic Pro bridge: run cargo build -p logic-kumi first".to_string())?;
    let hands = target.join("logic-hands");
    let helper = match helper_in(&hands) {
        Some(helper) => helper,
        None => {
            build_logic_helper(&checkout)?;
            helper_in(&hands)
                .ok_or_else(|| format!("scripts/build-logic-hands.py left no {LOGIC_HELPER_PREFIX}* in {}", hands.display()))?
        }
    };
    let version = bridge_version(&binary).unwrap_or_else(|| "dev".into());
    let staging = logic_staging(&checkout);
    let failed = |what: &str, error: std::io::Error| format!("couldn't stage Logic Pro's bridge ({what}): {error}");
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging).map_err(|error| failed("the staging folder", error))?;
    std::fs::copy(&binary, staging.join(LOGIC_BINARY)).map_err(|error| failed("the bridge", error))?;
    let helper_name = helper.file_name().map(|name| name.to_os_string()).unwrap_or_default();
    std::fs::copy(&helper, staging.join(helper_name)).map_err(|error| failed("the helper", error))?;
    std::fs::write(staging.join(LOGIC_VERSION_FILE), format!("{version}\n")).map_err(|error| failed("the version", error))?;
    Ok(staging)
}

/// Builds the Accessibility helper with the checkout's script (a universal, ad-hoc signed Swift binary).
fn build_logic_helper(checkout: &Path) -> Result<(), String> {
    let script = checkout.join("scripts").join("build-logic-hands.py");
    let status = std::process::Command::new("python3")
        .arg(&script)
        .current_dir(checkout)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::inherit())
        .status()
        .map_err(|error| format!("couldn't run python3 {} to build Kumi's helper for Logic Pro: {error}", script.display()))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("scripts/build-logic-hands.py failed ({status}); build Kumi's helper for Logic Pro by hand, then try again"))
    }
}

/// Whether Logic's bridge has Accessibility, as the running bridge's status says (`permission` null): None when no
/// bridge answers (it isn't started yet, or the secret can't be read). A bridge that answers busy is taken as granted.
fn logic_accessibility(home: &Path) -> Option<bool> {
    let paths = BridgePaths::new(home, Daw::Logic);
    let secret = files::read_secret(&paths.secret()).ok()?;
    files::scan_discovery(&paths.discovery(), Daw::Logic).into_iter().filter(|found| found.alive).find_map(|found| {
        let discovery = found.discovery.ok()?;
        let address = std::net::SocketAddr::from(([127, 0, 0, 1], discovery.port));
        let probe = daw_bridge::probe::probe(address, &secret, "kumi", Duration::from_secs(2)).ok()?;
        Some(probe.status.as_ref().is_none_or(|status| status.get("permission").is_none_or(serde_json::Value::is_null)))
    })
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
                // Logic's bridge is the host's to start: a running Logic is what answers.
                if daw == Daw::Logic {
                    return !detect::running(daw).is_empty();
                }
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
            Daw::Logic => WatchWords {
                starting: "Logic Pro is open; Kumi connects once its bridge attaches.",
                unreachable: "Kumi couldn't reach Logic Pro, so this is chat without it. Open Logic Pro with Kumi's bridge installed (kumi --daw logic bridge); Kumi connects by itself once it answers.",
                answered: "Logic Pro answered, so Kumi is connected to it now.",
            },
        }
    }
    fn enable_step(&self) -> Option<EnableStep> {
        match self.daw {
            // Logic is read through Accessibility, which the producer grants once, in System Settings.
            Daw::Logic => Some(EnableStep {
                label: "Accessibility",
                say: "Allow Kumi's helper under System Settings > Privacy & Security > Accessibility: macOS asks the first time Kumi starts it, and lists the app Kumi runs in (your terminal).",
                waiting: "waiting for Accessibility",
                later: "when Accessibility is allowed",
                put_off: "Kumi reads Logic Pro by itself once Accessibility is allowed: in System Settings, open Privacy & Security > Accessibility and turn on the app Kumi runs in.",
            }),
            _ => None,
        }
    }
    fn min_major(&self) -> u32 {
        self.min_major
    }
    fn integration(&self, mut context: IntegrationContext, session: &DawSession) -> Rc<dyn Integration> {
        let connect = session.factory.own_host.as_ref().map(|host| host(self.id));
        // The session's project store: a DAW Kumi catches up on keeps its projects' baselines there, beside their
        // conversations.
        context.project_store = Some(session.project_store.clone());
        (self.create)(OwnOptions { context, kumi_home: kumi_home(&session.env), connect, open_file: None })
    }
    fn probe(&self, _bridge_config: String, _factory: &DawFactory, env: &Env) -> LocalBoxFuture<'static, LiveProbe> {
        let create = self.create;
        let home = kumi_home(env);
        async move {
            let integration = create(OwnOptions {
                context: IntegrationContext::new(Rc::new(|_, _| {})),
                kumi_home: home,
                connect: None,
                open_file: None,
            });
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
            notice: Some(format!(
                "{missing}; chatting without it. Run {command} --daw {} bridge to {}.",
                self.id.as_str(),
                self.setup_verb()
            )),
            why: Some(format!("{missing}.")),
        }
    }
    fn setup(
        &self,
        env: &Env,
        bridge: Rc<RefCell<Option<String>>>,
        given: bool,
        controller: Rc<RefCell<Option<Weak<dyn SessionController>>>>,
    ) -> Rc<dyn DawSetup> {
        Rc::new(OwnSetup { own: self.id, env: env.clone(), bridge, given, controller })
    }
    fn doctor_checks<'a>(&'a self, io: &'a DoctorIo, _node: &'a Check) -> LocalBoxFuture<'a, Vec<Check>> {
        if !self.present(&io.env) {
            return async { vec![] }.boxed_local();
        }
        // The DAW is here, but Kumi's bridge was never put in: a note, nothing to fix.
        if self.receipt(&io.env).is_none() {
            let note = Check::note(
                self.not_in_words(),
                Some(format!(
                    "{} --daw {} bridge {}, if you want Kumi in {}",
                    *kumi_runtime::KUMI,
                    self.id.as_str(),
                    self.setup_verb_does(),
                    self.name()
                )),
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
            // The bridge the new Kumi brings, from its app folder: the running Kumi may already be the old one's. (Logic's
            // bridge is a process the host starts for each session, so nothing of Logic's needs closing for this.)
            let Some(source) = self.carried_by(Path::new(app)) else {
                if let Some(said) = back_to.and_then(|version| self.left_behind(&io.env, Path::new(app), version)) {
                    io.out.write(&said);
                }
                return 0;
            };
            match self.update_target(&io.env).and_then(|target| self.put(target, &io.env, &source, false)) {
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
                let _ = self.update_target(&io.env).and_then(|target| self.put(target, &io.env, &source, false));
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
                let question = match own.daw {
                    Daw::Logic => format!("Set up {} in Kumi's own folder ({})?", own.bridge, target.path().display()),
                    _ => format!("Put {} in {} ({})?", own.bridge, own.name(), target.path().display()),
                };
                if !crate::process::ask_yes_no(command.input.clone(), out.clone(), &question, Some("add --yes")).await {
                    out.write(&format!(
                        "Nothing was changed. To {} later: {} --daw {} bridge\n",
                        own.setup_verb(),
                        *kumi_runtime::KUMI,
                        own.id.as_str()
                    ));
                    return Ok(1);
                }
            }
            let (code, said) = match own.install(&env) {
                Ok(put) if put.changed => (
                    0,
                    match own.daw {
                        // Logic loads nothing: Kumi starts the bridge beside it, so Logic stays as it is.
                        Daw::Logic => format!(
                            "{} {} is in Kumi's own folder. Kumi starts it beside {} when a session begins: open {}, then run {} --daw logic. The first time, macOS asks to allow Kumi's helper under Accessibility.\n{}",
                            own.bridge,
                            put.version,
                            own.name(),
                            own.name(),
                            *kumi_runtime::KUMI,
                            own.kept_words(&put)
                        ),
                        _ => format!(
                            "{} {} is in {}. {} loads it when it starts: restart {} if it's open.\n{}",
                            own.bridge,
                            put.version,
                            own.name(),
                            own.name(),
                            own.name(),
                            own.kept_words(&put)
                        ),
                    },
                ),
                Ok(put) => (0, format!("{} {} is in {} already.\n", own.bridge, put.version, own.place_words())),
                Err(why) => (
                    1,
                    match own.daw {
                        Daw::Logic => format!("{} couldn't be set up: {why}.\n", own.bridge),
                        _ => format!("{} couldn't go in {}: {why}.\n", own.bridge, own.name()),
                    },
                ),
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
            Daw::Logic => "bridges/logic",
        }
    }
    fn bridge_files_in_use(&self, _folder: &str, _env: &Env) -> bool {
        // REAPER and Renoise load their copy from their own folder, not Kumi's; Logic's bridge runs only while a
        // session of Kumi's does.
        false
    }
    fn remove_bridge<'a>(&'a self, io: &'a InstalledIo, _run: Run) -> LocalBoxFuture<'a, Result<RemovedBridge, RuntimeError>> {
        async move {
            let Some(receipt) = self.receipt(&io.env) else {
                return Ok(RemovedBridge::None);
            };
            // Where the receipt says it went: the DAW's folder may be gone (Renoise's preferences, a portable REAPER).
            let target = self.receipt_target(&receipt);
            match Lifecycle::new(&kumi_home(&io.env), target).uninstall(true) {
                Ok(_) => Ok(RemovedBridge::Removed),
                Err(why) => {
                    io.out.write(&format!("{} stays in {}: {why}.\n", self.bridge, self.place_words()));
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
            Daw::Logic => "take Kumi's bridge for Logic Pro out too",
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
        match own.update_target(&io.env).and_then(|target| own.put(target, &io.env, &source, true)) {
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
        DawId::Logic => &LOGIC,
        _ => &RENOISE,
    }
}

/// Setup's DAW side for REAPER, Renoise and Logic Pro: REAPER's and Renoise's bridge goes in with the DAW closed, and
/// the DAW loads it when it opens; Logic's goes into Kumi's own folder while Logic stays open, and the session starts
/// it beside Logic when it connects.
struct OwnSetup {
    own: DawId,
    env: Env,
    /// The bridge configuration the session's integration starts from, and whether it was given (`--bridge-config`),
    /// so the one put in place doesn't replace it.
    bridge: Rc<RefCell<Option<String>>>,
    given: bool,
    controller: Rc<RefCell<Option<Weak<dyn SessionController>>>>,
}

impl OwnSetup {
    fn own(&self) -> &'static Own {
        own_of(self.own)
    }
    fn app(&self) -> Option<PathBuf> {
        match self.own().daw {
            Daw::Reaper => detected(&self.env, detect::detect_reaper).into_iter().find_map(|install| install.app),
            Daw::Renoise => detected(&self.env, detect::detect_renoise).apps.into_iter().map(|app| app.path).next(),
            Daw::Logic => best_logic(&self.env).map(|install| install.app),
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
    fn needs_restart(&self) -> bool {
        // Logic loads nothing: its bridge is a process the session starts beside it.
        self.own().daw != Daw::Logic
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
        // Off the app's thread: Logic's bridge may be staged (its helper built) on the way.
        let (own, env) = (self.own(), self.env.clone());
        tokio::task::spawn_blocking(move || own.install(&env).map(|put| put.version)).await.unwrap_or_else(|error| Err(error.to_string()))
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
    async fn connect(&self) {
        // The session's integration starts from the bridge just put in place (unless one was given): for Logic, the
        // host starts the bridge beside Logic as the session connects; for the others, it answers once the DAW has
        // loaded it.
        if !self.given {
            if let Some(found) = self.own().bridge_config(&self.env) {
                *self.bridge.borrow_mut() = Some(found);
            }
        }
        // The session may still be starting: a busy one is asked again shortly.
        for _ in 0..40 {
            let session = self.controller.borrow().as_ref().and_then(Weak::upgrade);
            let Some(session) = session else { return };
            if session.reconnect().await.is_ok() {
                return;
            }
            drop(session);
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }
    async fn version(&self, _app: Option<String>) -> Option<String> {
        match self.own().daw {
            Daw::Reaper => detected(&self.env, detect::detect_reaper).into_iter().find_map(|install| install.version),
            Daw::Renoise => detected(&self.env, detect::detect_renoise).apps.into_iter().find_map(|app| app.version),
            Daw::Logic => best_logic(&self.env).and_then(|install| install.version).map(|version| version.to_string()),
        }
    }
    async fn installed(&self) -> Installed {
        if self.own().daw == Daw::Logic {
            // Logic needs no first run: the bridge goes in Kumi's folder. Only Logic Pro 12 is served.
            return match best_logic(&self.env) {
                None => Installed::Missing,
                Some(install) => match install.version {
                    Some(version) if version < detect::LOGIC_MIN => Installed::Older(version.to_string()),
                    _ => Installed::Ready,
                },
            };
        }
        match self.own().placed(&self.env) {
            Ok(_) => Installed::Ready,
            Err(Some(TargetError::NeverOpened(_))) => Installed::NeverOpened(String::new()),
            Err(None) => Installed::Missing,
        }
    }
    async fn enabled(&self) -> Option<bool> {
        if self.own().daw != Daw::Logic {
            return None;
        }
        // What the running bridge says of Accessibility; none answering yet counts as not granted, so the step waits.
        let home = kumi_home(&self.env);
        Some(tokio::task::spawn_blocking(move || logic_accessibility(&home)).await.ok().flatten().unwrap_or(false))
    }
    fn settings_menu(&self) -> Option<String> {
        (self.own().daw == Daw::Logic).then(|| {
            "Accessibility is under System Settings > Privacy & Security: turn on the app Kumi runs in, and Kumi reads Logic Pro from then on.".into()
        })
    }
    fn surface_wait(&self) -> Duration {
        Duration::from_secs(10)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The staged folder takes the newest helper beside the bridge; its place is the checkout's target.
    #[test]
    fn the_newest_helper_in_a_folder_is_the_one_staged() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(helper_in(dir.path()), None);
        std::fs::write(dir.path().join("kumi-logic-hands-aaaa"), b"old").unwrap();
        std::fs::write(dir.path().join("not-a-helper"), b"x").unwrap();
        let newer = dir.path().join("kumi-logic-hands-bbbb");
        std::fs::write(&newer, b"new").unwrap();
        let later = std::time::SystemTime::now() + Duration::from_secs(5);
        std::fs::File::options().write(true).open(&newer).unwrap().set_modified(later).unwrap();
        assert_eq!(helper_in(dir.path()), Some(newer));
        assert_eq!(logic_staging(Path::new("/checkout")), PathBuf::from("/checkout/target/logic-bridge/logic-kumi"));
    }

    /// (M7) Without Logic Pro, its conversation keeps Kumi's persona and Apple's guide, in a line of Logic's words;
    /// REAPER's and Renoise's say what they said.
    #[test]
    fn without_logic_pro_kumi_answers_from_what_it_knows_and_apples_guide_in_one_line() {
        let said = LOGIC.instructions;
        assert!(!said.contains('\n'), "{said}");
        assert!(said.starts_with("You are Kumi, a concise, practical producer collaborator. Logic Pro isn't connected"), "{said}");
        assert!(said.contains("from Apple's Logic Pro User Guide (logic_manual)"), "{said}");
        let live = regex::Regex::new(r"\b(Live|Set|Sets|Ableton|Session|Arrangement|Main|[Rr]acks?|[Cc]lips?|[Ss]cenes?)\b").unwrap();
        assert!(!live.is_match(said), "{said}");
        assert_eq!(REAPER.instructions, "REAPER isn't connected, so Kumi can't see your project: answer from what you know, and say so.");
        assert_eq!(RENOISE.instructions, "Renoise isn't connected, so Kumi can't see your song: answer from what you know, and say so.");
    }
}
