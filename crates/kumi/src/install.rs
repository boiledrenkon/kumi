//! Installed Kumi and its native release bundles.
mod migration;
pub use crate::config::kumi_dir as kumi_home;
pub use crate::update::newer as newer_version;
use crate::{
    bridge_setup::{ask_yes_no, bridge_version, default_run, executable_dir, executable_name, is_live_running, AsyncBool, Confirm, Run},
    config::{find_bridge_config, json_files, load_db_file, remote_scripts_dir},
    doctor::read_bridge_server,
    input::TerminalInput,
    live_extension::{extension_data_dir, live_extensions_dir, remove_extension, remove_former_extension, KUMI_EXTENSION_ID},
    spinner::step,
    tui::tty::TtyOutput,
};
use futures::{future::LocalBoxFuture, StreamExt};
use kumi_common::{
    abort,
    js::{json::stringify, string::trim},
    time::now_ms,
};
use kumi_runtime::{
    ai::{
        error::LanguageModelError,
        http::{default_fetch, Fetch, FetchInit, Headers},
    },
    core::{
        disk::{free_bytes, size, MB},
        errors::RuntimeError,
        store_import::write_back,
    },
    ears::device::EARS_NAME,
    library::sources::{basename, dirname, join, resolve},
    net::proxy,
    system::{self, Env, SystemProgram},
    web::cause::{unreachable, unreachable_call, NetCause},
    CONNECT_LIVE, KUMI, KUMI_VERSION, REINSTALL,
};
pub use migration::{ensure_native_launcher, finish_legacy_transition, has_app, launcher, only_the_host_differs, write_launcher};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{fs, path::Path, process::Stdio, rc::Rc, time::Duration};
/// What an update needs free: the download and the new Kumi unpacked beside it, until the download goes. Measured on
/// 1.16.5's bundles: Windows x64, the biggest, downloads 48 MB and unpacks to 125 MB (173 MB side by side); macOS arm64
/// 41 + 90 MB. 200 MB covers the biggest with room for a release or two of growth. The swap itself only renames.
pub const UPDATE_SPACE: f64 = 200. * MB;
pub fn release_base(env: &Env) -> String {
    env.get("KUMI_RELEASES")
        .filter(|s| !s.is_empty())
        .map(String::as_str)
        .unwrap_or("https://github.com/user1303836/kumi/releases/latest/download")
        .trim_end_matches('/')
        .into()
}
/// Where `update --version` finds that release: its own downloads on GitHub, or KUMI_RELEASES when that's set.
pub fn release_base_for(env: &Env, version: Option<&str>) -> String {
    match version {
        Some(version) if env.get("KUMI_RELEASES").is_none_or(|s| s.is_empty()) => {
            format!("https://github.com/user1303836/kumi/releases/download/v{version}")
        }
        _ => release_base(env),
    }
}
/// The place releases come from, as Kumi names it: GitHub, or KUMI_RELEASES's host.
fn place(base: &str) -> String {
    let host = url::Url::parse(base).ok().and_then(|url| {
        url.host_str().map(|host| match url.port() {
            Some(port) => format!("{host}:{port}"),
            None => host.to_string(),
        })
    });
    match host {
        Some(host) if host == "github.com" || host.ends_with(".github.com") => "GitHub".into(),
        Some(host) => host,
        None => base.into(),
    }
}
pub fn native_target() -> String {
    let arch = std::env::consts::ARCH;
    match system::platform() {
        "darwin" => format!("{arch}-apple-darwin"),
        "win32" => format!("{arch}-pc-windows-{}", if cfg!(target_env = "gnu") { "gnu" } else { "msvc" }),
        "linux" => format!("{arch}-unknown-linux-{}", if cfg!(target_env = "musl") { "musl" } else { "gnu" }),
        other => format!("{arch}-unknown-{other}"),
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReleaseManifest {
    pub kumi: String,
    pub bundle: String,
    pub sha256: String,
    pub runtime: String,
    pub target: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bridge: Option<String>,
}
#[derive(Debug, Clone, PartialEq)]
pub enum AskedRelease {
    Manifest(ReleaseManifest),
    None,
    /// Not reached: why, when that's known, and the call that failed, whose words say more (a proxy that isn't
    /// answering, the proxy settings for that address). No call for Kumi's own wait running out or a cut-off answer.
    Offline(Option<NetCause>, Option<(String, LanguageModelError)>),
    /// Reached, but it answered with this status in place of the release's description.
    Answered(u16),
    Invalid,
    /// A web page came back instead of the release's description: a Wi-Fi sign-in page, or a proxy's.
    Page,
}
pub async fn ask_release(env: &Env, fetch: Option<Rc<dyn Fetch>>) -> AskedRelease {
    ask_release_at(&release_base(env), fetch).await
}
pub async fn ask_release_at(base: &str, fetch: Option<Rc<dyn Fetch>>) -> AskedRelease {
    let fetch = fetch.unwrap_or_else(default_fetch);
    let url = format!("{base}/kumi-release.json");
    let waited = abort::timeout(20_000);
    let response = match fetch.fetch(&url, FetchInit { signal: Some(waited.clone()), ..Default::default() }).await {
        Ok(response) => response,
        // Kumi's own wait running out is a timeout, whatever the call says of it.
        Err(_) if waited.is_cancelled() => return AskedRelease::Offline(Some(NetCause::Timeout), None),
        Err(error) => return AskedRelease::Offline(NetCause::of_call(&error), Some((url, error))),
    };
    if response.status == 404 {
        return AskedRelease::None;
    }
    if !response.ok() {
        return match NetCause::of_status(response.status) {
            Some(cause) => AskedRelease::Offline(Some(cause), None),
            None => AskedRelease::Answered(response.status),
        };
    }
    let Ok(text) = response.text().await else { return AskedRelease::Offline(Some(NetCause::Offline), None) };
    if text.trim_start().starts_with('<') {
        return AskedRelease::Page;
    }
    let Ok(value) = serde_json::from_str::<Value>(&text) else { return AskedRelease::Invalid };
    let value = match value.get("targets") {
        Some(targets) => match targets.get(native_target()) {
            Some(target) => target.clone(),
            None => return AskedRelease::Invalid,
        },
        None => value,
    };
    let Ok(manifest) = serde_json::from_value::<ReleaseManifest>(value) else { return AskedRelease::Invalid };
    let matches = |value: &str, pattern: &str| regex::Regex::new(pattern).unwrap().is_match(value);
    if !matches(&manifest.kumi, r"^[0-9]+\.[0-9]+\.[0-9]+(?:-[A-Za-z0-9_.]+)?$")
        || !matches(&manifest.bundle, r"^[A-Za-z0-9_.-]+\.tar\.gz$")
        || !matches(&manifest.sha256, r"^[0-9a-f]{64}$")
        || manifest.runtime != "rust-native"
        || !matches(&manifest.target, r"^[A-Za-z0-9_]+-[A-Za-z0-9_.-]+$")
    {
        AskedRelease::Invalid
    } else {
        AskedRelease::Manifest(manifest)
    }
}
/// The newest release's own downloads, as GitHub's API names it. For a few minutes after a release,
/// `releases/latest/download` can still lead to the one before, while the API already names the new one. The answer
/// only chooses which release's own description (and checksum) to read. None when the API doesn't say (its rate
/// limit, a network that blocks it). It's asked without a token.
async fn newest_release_base(fetch: &Rc<dyn Fetch>) -> Option<String> {
    let headers =
        Headers::from([("accept".into(), "application/vnd.github+json".into()), ("user-agent".into(), format!("kumi/{KUMI_VERSION}"))]);
    let init = FetchInit { headers, signal: Some(abort::timeout(3_000)), ..Default::default() };
    let response = fetch.fetch("https://api.github.com/repos/user1303836/kumi/releases/latest", init).await.ok()?;
    if !response.ok() {
        return None;
    }
    let tag = response.json().await.ok()?.get("tag_name")?.as_str()?.to_string();
    let version = tag.strip_prefix('v')?;
    (crate::config::release_version(version).as_deref() == Some(version))
        .then(|| format!("https://github.com/user1303836/kumi/releases/download/{tag}"))
}
/// The newest release for `kumi update` and its checks: the one GitHub's API names, read from that release's own
/// description, else the usual way. With KUMI_RELEASES, only there. The daily check doesn't come here, so it
/// asks no more than once a day.
async fn ask_newest(env: &Env, fetch: Rc<dyn Fetch>) -> (String, AskedRelease) {
    let usual = release_base(env);
    if env.get("KUMI_RELEASES").is_none_or(|s| s.is_empty()) {
        if let Some(base) = newest_release_base(&fetch).await {
            match ask_release_at(&base, Some(fetch.clone())).await {
                // The usual way goes over the same network: no second wait for the same failure.
                asked @ (AskedRelease::Manifest(_) | AskedRelease::Offline(..)) => return (base, asked),
                _ => {}
            }
        }
    }
    let asked = ask_release_at(&usual, Some(fetch)).await;
    (usual, asked)
}
pub async fn fetch_manifest(env: &Env, fetch: Option<Rc<dyn Fetch>>) -> Option<ReleaseManifest> {
    match ask_release(env, fetch).await {
        AskedRelease::Manifest(manifest) => Some(manifest),
        _ => None,
    }
}
/// Why there's no release to go on, without the last full stop. `pinned`: `update --version`'s.
fn unasked(asked: &AskedRelease, base: &str, pinned: Option<&str>, env: &Env) -> String {
    match (asked, pinned) {
        (AskedRelease::None, Some(version)) => {
            format!("There's no Kumi {version} to get (github.com/user1303836/kumi/releases lists every release)")
        }
        (AskedRelease::None, None) => {
            format!("There's no Kumi release to get at {} yet; try again later", base.strip_prefix("https://").unwrap_or(base))
        }
        (AskedRelease::Offline(cause, call), _) => {
            let host = place(base);
            call.as_ref()
                .and_then(|(url, error)| unreachable_call(&host, url, error, env, &proxy::current()))
                .unwrap_or_else(|| unreachable(&host, *cause, env))
                .trim_end_matches('.')
                .into()
        }
        (AskedRelease::Answered(status), _) => {
            format!("{} answered {status} in place of the release's description; try again in a moment", place(base))
        }
        (AskedRelease::Page, _) => {
            format!("A Wi-Fi sign-in page or a proxy answered instead of {}: sign in to the network, then try again", place(base))
        }
        (_, Some(version)) => format!("Kumi {version}'s description didn't make sense; try again later"),
        _ => "The latest release's description didn't make sense; try again later".into(),
    }
}
fn error(e: impl std::fmt::Display) -> RuntimeError {
    RuntimeError::plain(e.to_string())
}
#[derive(Clone)]
pub struct InstalledIo {
    pub out: Rc<dyn TtyOutput>,
    pub env: Env,
    pub input: Option<Rc<dyn TerminalInput>>,
    pub run: Option<Run>,
    pub fetcher: Option<Rc<dyn Fetch>>,
    pub live_running: Option<AsyncBool>,
    pub confirm: Option<Confirm>,
    pub update_bridge: Option<Rc<dyn Fn(String) -> LocalBoxFuture<'static, i32>>>,
    /// The producer's Ctrl-C while an update downloads: it stops the download, and nothing changes.
    pub cancel: Option<kumi_common::abort::Signal>,
    /// `update --version`: that release, newer or older, in place of the newest.
    pub version: Option<String>,
    /// Free space on the disk holding a folder (tests say how much).
    pub free: Option<Rc<dyn Fn(String) -> LocalBoxFuture<'static, Option<f64>>>>,
    /// How many other Kumi processes run this Kumi (tests say how many).
    pub others: Option<Rc<dyn Fn() -> LocalBoxFuture<'static, usize>>>,
}
impl InstalledIo {
    pub fn new(out: Rc<dyn TtyOutput>, env: Env) -> Self {
        Self {
            out,
            env,
            input: None,
            run: None,
            fetcher: None,
            live_running: None,
            confirm: None,
            update_bridge: None,
            cancel: None,
            version: None,
            free: None,
            others: None,
        }
    }
}
/// The version of the Kumi in `folder`, from its package.json.
pub fn app_version(folder: &str) -> Option<String> {
    let value: Value = serde_json::from_slice(&fs::read(join(folder, "package.json")).ok()?).ok()?;
    value.get("version").and_then(Value::as_str).filter(|s| !s.is_empty()).map(str::to_string)
}
/// `path` as the producer reads it: their home folder as ~.
fn tilde(path: &str) -> String {
    tilde_in(path, &home::home_dir().unwrap_or_default().display().to_string())
}
/// The part of Kumi's folder that belongs to another account (an install run with sudo), so Kumi can't update
/// it: the folder itself, or one of what an update moves. None for root, who can change anything.
pub fn not_yours(home: &str) -> Option<String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        // SAFETY: geteuid has no preconditions.
        let me = unsafe { libc::geteuid() };
        if me == 0 {
            return None;
        }
        return std::iter::once(home.to_string())
            .chain(["app", "app.previous", "app.previous.old", "app.new", "app.rollback", "bin", "downloads"].map(|part| join(home, part)))
            .find(|path| fs::symlink_metadata(path).is_ok_and(|m| m.uid() != me));
    }
    #[cfg(not(unix))]
    {
        let _ = home;
        None
    }
}
/// Kumi's folder as the producer reads it, and as they'd type it in a shell.
fn shown_and_typed(home: &str) -> (String, String) {
    let shown = tilde(home);
    let typed = if shown.chars().all(|c| c.is_ascii_alphanumeric() || "~/._-".contains(c)) {
        shown.clone()
    } else {
        format!("'{}'", home.replace('\'', "'\\''"))
    };
    (shown, typed)
}
/// The command that gives Kumi's folder back to this account.
pub fn chown_line(home: &str) -> String {
    format!("sudo chown -R $USER {}", shown_and_typed(home).1)
}
/// How to give Kumi's folder back to this account.
pub fn owner_advice(home: &str) -> String {
    format!("{} isn't yours (installed with sudo?): {}", shown_and_typed(home).0, chown_line(home))
}
/// What to say when the system refused to change something in Kumi's folder (on macOS and Linux): whose it is
/// when that's the reason, or the system's words and the usual way back.
fn refused(home: &str, error: &std::io::Error) -> String {
    if not_yours(home).is_some() {
        return owner_advice(home);
    }
    let (shown, typed) = shown_and_typed(home);
    format!("The system refused to change {shown} ({error}); if something in it was installed with sudo: sudo chown -R $USER {typed}")
}
/// Whether the system refused for want of permission, on macOS and Linux (EACCES, EPERM). On Windows the same
/// errors mean a folder held for a moment, which `rename_in` waits out.
fn denied(error: &std::io::Error) -> bool {
    !cfg!(windows) && (error.kind() == std::io::ErrorKind::PermissionDenied || error.raw_os_error() == Some(1))
}
/// A yes/no question; `ahead` says how to answer it beforehand, where a flag does ("add --yes").
async fn ask(io: &InstalledIo, question: &str, ahead: Option<&str>) -> bool {
    if let Some(ask) = &io.confirm {
        ask(question.into()).await
    } else {
        ask_yes_no(io.input.clone(), io.out.clone(), question, ahead).await
    }
}
async fn live_open(io: &InstalledIo, run: Run) -> bool {
    step(
        io.out.clone(),
        &io.env,
        "Checking whether Live is open…",
        async {
            if let Some(live) = &io.live_running {
                live().await
            } else {
                is_live_running(run).await
            }
        },
        false,
    )
    .await
}
async fn bridge_after(io: &InstalledIo, home: &str, app: &str) -> i32 {
    let say = |text: String| io.out.write(&format!("{text}\n"));
    let Some(config) = find_bridge_config(&io.env) else {
        if let Some(words) = no_bridge_yet(system::platform()) {
            say(words);
        }
        return 0;
    };
    let server = read_bridge_server(&config).ok();
    let runtime_migration = server.as_ref().is_some_and(|s| !s.native());
    let installed = server.and_then(|s| s.version);
    let bundled = bridge_version(app);
    let Some((installed, bundled)) = installed
        .zip(bundled)
        .filter(|(installed, bundled)| newer_version(bundled, installed) || (runtime_migration && bundled == installed))
    else {
        say("The bridge in Live is up to date.".into());
        return 0;
    };
    say(if runtime_migration {
        format!("The bridge in Live uses JavaScript ({installed}); this Kumi includes the native bridge ({bundled}).")
    } else {
        format!("The bridge in Live is {installed}; this Kumi's is {bundled}.")
    });
    if live_open(io, io.run.clone().unwrap_or_else(default_run)).await {
        say(format!("{}.", *CONNECT_LIVE));
        return 0;
    }
    if let Some(update) = &io.update_bridge {
        return update(app.into()).await;
    }
    bridge_command(app, home).status().await.ok().and_then(|s| s.code()).unwrap_or(1)
}
/// With no bridge in Live yet, an update ends by saying how to connect it, where Live runs: on Linux there's no Live,
/// so nothing is said.
pub fn no_bridge_yet(platform: &str) -> Option<String> {
    crate::live_app::runs_on(platform).then(|| format!("The bridge isn't in Live yet. {}.", *CONNECT_LIVE))
}
/// `kumi bridge` as an update's last step, Live just found closed: it asks nothing more (its lifecycle looks for
/// Live again before changing anything) and doesn't wait for a Live nobody has opened.
pub fn bridge_command(app: &str, home: &str) -> tokio::process::Command {
    let mut command = tokio::process::Command::new(join(app, &executable_name("kumi")));
    command
        .args(["bridge", "--yes"])
        .current_dir(home)
        .env("KUMI_INSTALLED", "1")
        .env("KUMI_BRIDGE_AFTER", "1")
        .env("KUMI_BRIDGE_WAIT_SECONDS", "0")
        .env("KUMI_HOME", home)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    command
}
/// What a swap does to Kumi's folders. The tests stand in for Windows holding one.
trait Folders {
    fn rename(&self, from: &str, to: &str) -> std::io::Result<()>;
    fn remove(&self, path: &str) -> std::io::Result<()>;
    /// A move waits for a busy folder.
    fn waiting(&self) {}
}
/// Folders that say, once, when a move first has to wait: holding a folder can keep an update waiting a minute.
struct Saying<'a, F: Folders, S: Fn()> {
    folders: &'a F,
    say: S,
    said: std::cell::Cell<bool>,
}
impl<F: Folders, S: Fn()> Folders for Saying<'_, F, S> {
    fn rename(&self, from: &str, to: &str) -> std::io::Result<()> {
        self.folders.rename(from, to)
    }
    fn remove(&self, path: &str) -> std::io::Result<()> {
        self.folders.remove(path)
    }
    fn waiting(&self) {
        if !self.said.replace(true) {
            (self.say)()
        }
    }
}
struct Disk;
impl Folders for Disk {
    fn rename(&self, from: &str, to: &str) -> std::io::Result<()> {
        fs::rename(from, to)
    }
    fn remove(&self, path: &str) -> std::io::Result<()> {
        remove(path)
    }
}
async fn rename(from: &str, to: &str) -> std::io::Result<()> {
    rename_in(&Disk, from, to).await
}
/// Renames, waiting out a folder held for a moment (a Kumi window, an antivirus scan) for 5 s. On macOS and
/// Linux only a busy mount waits: a folder that isn't this account's (EACCES, EPERM) never frees up by itself.
async fn rename_in(folders: &impl Folders, from: &str, to: &str) -> std::io::Result<()> {
    for attempt in 0..=20 {
        match folders.rename(from, to) {
            Ok(()) => return Ok(()),
            Err(e)
                if attempt < 20
                    && (if cfg!(windows) {
                        e.kind() == std::io::ErrorKind::PermissionDenied || matches!(e.raw_os_error(), Some(5 | 32))
                    } else {
                        e.raw_os_error() == Some(16)
                    }) =>
            {
                folders.waiting();
                tokio::time::sleep(Duration::from_millis(250)).await
            }
            Err(e) => return Err(e),
        }
    }
    unreachable!()
}
pub fn remove(path: &str) -> std::io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(m) if m.is_dir() => fs::remove_dir_all(path),
        Ok(_) => fs::remove_file(path),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}
/// Puts the Kumi that was there (in `previous`) back in `app`, unless `app` already holds a whole one: `app` can
/// be missing, or hold part of a move, which is cleared first. Waits out a busy folder a few times over; says
/// whether `app` holds a whole Kumi.
async fn put_back(folders: &impl Folders, app: &str, previous: &str) -> bool {
    for attempt in 0..3 {
        if migration::has_app(app) {
            return true;
        }
        if !migration::has_app(previous) {
            return false;
        }
        if (folders.remove(app).is_err() || rename_in(folders, previous, app).await.is_err()) && attempt < 2 {
            tokio::time::sleep(Duration::from_secs(1)).await
        }
    }
    migration::has_app(app)
}
/// Puts the new Kumi (`fresh`) in `app`, and the one that was there in `previous`, where a rollback finds it.
/// Whenever the new one can't go in, `app` gets the one that was there back, and `previous` the one before it.
/// An update that gave up earlier can leave the only whole Kumi in `previous` (or the one before it in
/// `previous.old`): it goes back first, and neither is cleared while it holds the only whole Kumi.
pub async fn swap_in(fresh: &str, app: &str, previous: &str) -> std::io::Result<()> {
    swap_in_with(&Disk, fresh, app, previous).await
}
async fn swap_in_with(folders: &impl Folders, fresh: &str, app: &str, previous: &str) -> std::io::Result<()> {
    let older = format!("{previous}.old");
    settle(folders, app, previous, &older).await;
    if migration::has_app(app) || migration::has_app(previous) || !migration::has_app(&older) {
        folders.remove(&older)?;
    }
    let swapped = async {
        if migration::has_app(app) {
            // The one before waits aside until the new one is in: a failed update still leaves it to roll back to.
            if Path::new(previous).exists() {
                rename_in(folders, previous, &older).await?
            }
            rename_in(folders, app, previous).await?
        } else {
            // No whole Kumi in app (none yet, or one couldn't go back): the new one replaces what's there, and
            // app.previous stays as it is.
            folders.remove(app)?
        }
        match rename_in(folders, fresh, app).await {
            // A move that went through before its error counts.
            Err(_) if !Path::new(fresh).exists() && migration::has_app(app) => Ok(()),
            moved => moved,
        }
    }
    .await;
    if let Err(error) = swapped {
        put_back(folders, app, previous).await;
        if !Path::new(previous).exists() && migration::has_app(&older) {
            let _ = rename_in(folders, &older, previous).await;
        }
        return Err(error);
    }
    // What was in app.previous before is no longer needed; a later swap clears it if this can't.
    let _ = folders.remove(&older);
    Ok(())
}
/// After an update that gave up: `app` gets a whole Kumi back from `previous`, or from the one before it in
/// `older`, and `previous` gets the one before back if it's gone.
async fn settle(folders: &impl Folders, app: &str, previous: &str, older: &str) {
    if !put_back(folders, app, previous).await {
        put_back(folders, app, older).await;
    }
    if !Path::new(previous).exists() && migration::has_app(older) {
        let _ = rename_in(folders, older, previous).await;
    }
}
/// Puts things back as they were after an update that gave up, before anything else: a later run starts
/// from a whole Kumi in app wherever one is left.
async fn settle_installed(home: &str) {
    let previous = join(home, "app.previous");
    settle(&Disk, &join(home, "app"), &previous, &format!("{previous}.old")).await
}
/// What to say when the new Kumi couldn't go in; `kept` says whether the one that was there still works, and
/// `why` is the system's reason (macOS and Linux; Windows names the usual one).
fn swap_failed(windows: bool, kept: bool, why: &str) -> String {
    let what = "Windows kept Kumi's folder busy: a Kumi window, or an antivirus scan of the new files.";
    let installer = format!("run the installer again ({REINSTALL})");
    match (windows, kept) {
        (true, true) => format!("{what} The Kumi you had still works: close every Kumi window, give the scan a minute, then run update again."),
        (true, false) => format!(
            "{what} The Kumi you had couldn't go back in place yet, so kumi won't start: close every Kumi window, give the scan a minute, then {installer}."
        ),
        (false, true) => format!("Couldn't put the new Kumi in place ({why}), so this one stays."),
        (false, false) => format!("Couldn't put the new Kumi in place ({why}), and the one you had couldn't go back, so kumi won't start: {installer}."),
    }
}
/// What an update leaves behind, removed when it ends, best effort: a file a scan still holds can't make a good
/// update fail.
struct Cleanup(Vec<String>);
impl Drop for Cleanup {
    fn drop(&mut self) {
        for path in &self.0 {
            let _ = remove(path);
        }
    }
}
/// Why a download didn't finish.
enum Stopped {
    /// The producer's Ctrl-C.
    Cancelled,
    /// The connection, and why when the error says.
    Network(Option<NetCause>),
    /// What the server answered instead of the bundle.
    Status(u16),
    /// Writing it on this computer.
    Disk(std::io::Error),
}
impl Stopped {
    /// What to say, ending with what that means: nothing was changed.
    fn said(&self, home: &str) -> String {
        match self {
            Self::Cancelled => "The download was stopped, so nothing was changed.".into(),
            Self::Network(Some(cause)) => {
                format!("The download stopped partway: {}, so nothing was changed. {}", cause.words(), cause.fix())
            }
            Self::Network(None) => "The download stopped partway, so nothing was changed. Check the connection, then try again.".into(),
            Self::Status(status) => match NetCause::of_status(*status) {
                Some(cause) => format!("The download stopped: {}, so nothing was changed. {}", cause.words(), cause.fix()),
                None => format!("The download failed (it answered {status}), so nothing was changed. Try again in a moment."),
            },
            Self::Disk(error) if error.kind() == std::io::ErrorKind::StorageFull => {
                "The disk filled up during the download, so nothing was changed. Free some space, then try again.".into()
            }
            Self::Disk(error) if denied(error) => format!("{}. Nothing was changed.", refused(home, error).trim_end_matches('.')),
            Self::Disk(error) => format!("Kumi couldn't save the download ({error}), so nothing was changed."),
        }
    }
}
/// Downloads `url` into `file` as it arrives, and gives what it got's SHA-256: the bundle is never held whole, nor
/// read back to check it.
async fn download(url: &str, file: &str, fetch: Rc<dyn Fetch>, cancel: Option<abort::Signal>) -> Result<String, Stopped> {
    use tokio::io::AsyncWriteExt;
    let signal = abort::any(std::iter::once(abort::timeout(600000)).chain(cancel.clone()));
    let stopped = |error: &LanguageModelError| {
        if cancel.as_ref().is_some_and(abort::Signal::is_cancelled) {
            Stopped::Cancelled
        } else {
            Stopped::Network(NetCause::of_call(error))
        }
    };
    let response = fetch.fetch(url, FetchInit { signal: Some(signal.clone()), ..Default::default() }).await.map_err(|e| stopped(&e))?;
    if !response.ok() {
        return Err(Stopped::Status(response.status));
    }
    let mut out = tokio::fs::File::create(file).await.map_err(Stopped::Disk)?;
    let mut digest = Sha256::new();
    if let Some(mut body) = response.body {
        loop {
            // The body's chunks race the signal too: a stalled connection stops when it fires.
            let chunk = tokio::select! {
                chunk = body.next() => chunk,
                _ = signal.cancelled() => return Err(if cancel.as_ref().is_some_and(abort::Signal::is_cancelled) {
                    Stopped::Cancelled
                } else {
                    Stopped::Network(Some(NetCause::Timeout))
                }),
            };
            let Some(chunk) = chunk else { break };
            let chunk = chunk.map_err(|e| stopped(&e))?;
            digest.update(&chunk);
            out.write_all(&chunk).await.map_err(Stopped::Disk)?;
        }
    }
    // A tokio file finishes its last write in the background: wait for it, so a full disk is the download's error.
    out.flush().await.map_err(Stopped::Disk)?;
    Ok(hex::encode(digest.finalize()))
}
/// What a step that moves Kumi's folders says when the system refuses: whose folder it is, or why.
fn not_in_place(home: &str, error: &std::io::Error) -> String {
    if denied(error) {
        format!("{}. The Kumi you had stays.", refused(home, error).trim_end_matches('.'))
    } else {
        format!("Couldn't put the new Kumi in place ({error}), so this one stays.")
    }
}
/// The older Kumi reads notes, techniques and lessons from files: what the database keeps is written back for it
/// first. A file that already says it is left as it is; if this fails, going back still goes ahead.
async fn write_back_for_older(io: &InstalledIo) {
    let say = |s: String| io.out.write(&format!("{s}\n"));
    let written = match (load_db_file(&io.env), json_files(&io.env)) {
        (Ok(db), Ok(files)) => write_back(db.into(), files, kumi_common::time::now_ms()).await,
        (Err(why), _) | (_, Err(why)) => Err(why),
    };
    match written {
        Ok(written) => {
            for (file, why) in written.left {
                say(format!("Left {} as it was: {why}.", file.display()));
            }
        }
        Err(why) => say(format!(
            "The older Kumi won't see the notes, techniques or lessons kept since the update ({}); they stay in Kumi's database for when you update again.",
            why.message()
        )),
    }
}
pub async fn update_installed(io: InstalledIo) -> Result<i32, RuntimeError> {
    let say = |s: String| io.out.write(&format!("{s}\n"));
    let home = kumi_home(&io.env);
    let app = join(&home, "app");
    // Another account's folder (an install run with sudo) can't take a new Kumi: said before anything moves.
    if not_yours(&home).is_some() {
        say(format!("{} Nothing was changed.", owner_advice(&home)));
        return Ok(1);
    }
    settle_installed(&home).await;
    let run = io.run.clone().unwrap_or_else(default_run);
    let fetch = io.fetcher.clone().unwrap_or_else(default_fetch);
    let pinned = io.version.as_deref();
    if let Some(version) = pinned.filter(|version| !newer_version(version, "1.7.5")) {
        say(format!("Kumi {version} came before Kumi's native releases, so kumi update can't install it; 1.7.6 is the earliest it can."));
        return Ok(1);
    }
    if pinned == Some(KUMI_VERSION) {
        say(format!("Kumi is {KUMI_VERSION} already."));
        return Ok(bridge_after(&io, &home, &app).await);
    }
    let looking = pinned.map(|version| format!("Looking for Kumi {version}…")).unwrap_or_else(|| "Looking for a newer Kumi…".into());
    let asking = async {
        match pinned {
            Some(_) => {
                let base = release_base_for(&io.env, pinned);
                let asked = ask_release_at(&base, Some(fetch.clone())).await;
                (base, asked)
            }
            None => ask_newest(&io.env, fetch.clone()).await,
        }
    };
    let (base, asked) = step(io.out.clone(), &io.env, &looking, asking, true).await;
    let AskedRelease::Manifest(manifest) = asked else {
        say(format!("{}. Nothing was changed.", unasked(&asked, &base, pinned, &io.env)));
        return Ok(1);
    };
    match pinned {
        Some(version) if manifest.kumi != version => {
            say(format!("There's no Kumi {version} at {}; it has {}. Nothing was changed.", place(&base), manifest.kumi));
            return Ok(1);
        }
        None if !newer_version(&manifest.kumi, KUMI_VERSION) => {
            say(format!("Kumi is up to date ({KUMI_VERSION})."));
            return Ok(bridge_after(&io, &home, &app).await);
        }
        _ => {}
    }
    if manifest.target != native_target() {
        say(format!(
            "Kumi {} is for {}, but this Kumi is for {}. Run the installer again for this computer ({REINSTALL}).",
            manifest.kumi,
            manifest.target,
            native_target()
        ));
        return Ok(1);
    }
    let free = match &io.free {
        Some(free) => free(home.clone()).await,
        None => free_bytes(Path::new(&home)).await,
    };
    if let Some(free) = free.filter(|free| *free < UPDATE_SPACE) {
        say(format!(
            "Kumi needs about 200 MB free to update, and the disk with {} has {}. Free some space (empty the Trash or Recycle Bin, or move old bounces and videos off that disk), then try again. Nothing was changed.",
            tilde(&home),
            size(free)
        ));
        return Ok(1);
    }
    let older = newer_version(KUMI_VERSION, &manifest.kumi);
    let downloads = join(&home, "downloads");
    // Gone again once emptied, but only when this update made it: never a folder that was there (in a home folder
    // used as Kumi's, on a disk that ignores case, `downloads` is the producer's Downloads).
    let made = fs::symlink_metadata(&downloads).is_err();
    if let Err(error) = tokio::fs::create_dir_all(&downloads).await {
        say(Stopped::Disk(error).said(&home));
        return Ok(1);
    }
    let bundle = join(&downloads, &manifest.bundle);
    let fresh = join(&home, "app.new");
    let cleanup = Cleanup(vec![fresh.clone(), bundle.clone()]);
    let result: Result<i32, RuntimeError> = async {
        let downloaded = step(
            io.out.clone(),
            &io.env,
            &format!("Downloading Kumi {}…", manifest.kumi),
            download(&format!("{base}/{}", manifest.bundle), &bundle, fetch, io.cancel.clone()),
            true,
        )
        .await;
        let downloaded = match downloaded {
            Ok(sha256) => sha256,
            Err(stopped) => {
                say(stopped.said(&home));
                return Ok(1);
            }
        };
        if downloaded != manifest.sha256 {
            say("The download didn't match its checksum, so nothing was changed. Try again in a moment.".into());
            return Ok(1);
        }
        let unpacked = step(
            io.out.clone(),
            &io.env,
            &format!("Unpacking Kumi {}…", manifest.kumi),
            async {
                if let Err(error) = remove(&fresh).and_then(|()| fs::create_dir_all(&fresh)) {
                    return not_in_place(&home, &error).into();
                }
                let unpacked = run(
                    system::system_program_default(SystemProgram::Tar),
                    vec!["-xzf".into(), bundle.clone(), "-C".into(), fresh.clone()],
                    None,
                )
                .await;
                if unpacked.code != 0 {
                    let text = if unpacked.stderr.is_empty() { &unpacked.stdout } else { &unpacked.stderr };
                    return Some(format!("Unpacking it failed: {}", trim(text).split('\n').next_back().unwrap_or("tar failed")));
                }
                let probe = run(join(&fresh, &executable_name("kumi")), vec!["--version".into()], None).await;
                if probe.code != 0 || !probe.stdout.contains(&manifest.kumi) {
                    return Some(format!(
                        "The new Kumi didn't start, so this one stays. Try again, or run the installer again ({REINSTALL})."
                    ));
                }
                None
            },
            false,
        )
        .await;
        if let Some(failed) = unpacked {
            say(failed);
            return Ok(1);
        }
        // Going back: what the database keeps is written for the older Kumi just before it goes in, once it's
        // unpacked and has started.
        if older {
            write_back_for_older(&io).await;
        }
        let failed = step(
            io.out.clone(),
            &io.env,
            &format!("Putting Kumi {} in place…", manifest.kumi),
            async {
                if let Err(error) = write_launcher(&home) {
                    return not_in_place(&home, &error).into();
                }
                // Printed above the spinner, which draws its line again below.
                let spinning = crate::spinner::spins(io.out.as_ref(), &io.env);
                let say = || {
                    let line = if cfg!(windows) {
                        "Windows is holding Kumi's folder (a Kumi window, or an antivirus scan); waiting…"
                    } else {
                        "Kumi's folder is busy; waiting…"
                    };
                    io.out.write(&if spinning { format!("\r\u{1b}[2K{line}\n") } else { format!("{line}\n") })
                };
                let folders = Saying { folders: &Disk, say, said: Default::default() };
                if let Err(error) = swap_in_with(&folders, &fresh, &app, &join(&home, "app.previous")).await {
                    let kept = migration::has_app(&app);
                    return Some(if denied(&error) && kept {
                        not_in_place(&home, &error)
                    } else {
                        swap_failed(cfg!(windows), kept, &error.to_string())
                    });
                }
                None
            },
            false,
        )
        .await;
        if let Some(failed) = failed {
            say(failed);
            return Ok(1);
        }
        Ok(0)
    }
    .await;
    drop(cleanup);
    // Only when it's empty: a file a scan still holds stays for the next update to clear.
    if made {
        let _ = fs::remove_dir(&downloads);
    }
    let code = result?;
    if code != 0 {
        return Ok(code);
    }
    say(format!(
        "Kumi {KUMI_VERSION} → {}{}. {} update --rollback goes back to {KUMI_VERSION}.",
        manifest.kumi,
        if older { ", an earlier one" } else { "" },
        *KUMI
    ));
    if !older {
        let changelog = fs::read_to_string(join(&app, "CHANGELOG.md")).unwrap_or_default();
        for line in crate::whats_new::update_lines(&changelog, KUMI_VERSION, &manifest.kumi) {
            say(line);
        }
    }
    if let Some(open) = still_open(&io, &join(&app, &executable_name("kumi"))).await {
        say(open);
    }
    // Going back, a bridge in Live as new as the earlier Kumi's stays: a newer one serves it too, and Live may have it
    // loaded. An older one is brought up to date as after any update.
    let installed = find_bridge_config(&io.env).and_then(|config| read_bridge_server(&config).ok()?.version);
    if let Some(installed) =
        installed.filter(|installed| older && bridge_version(&app).is_none_or(|bundled| !newer_version(&bundled, installed)))
    {
        say(format!("The bridge in Live stays {installed}, which works with {}.", manifest.kumi));
        return Ok(0);
    }
    Ok(bridge_after(&io, &home, &app).await)
}
/// How many other processes run `exe` on this account: Kumi windows, or a `kumi` command in another terminal.
/// None on Windows, where Kumi can't update or roll back while one runs at all.
pub async fn others_running(exe: &str) -> usize {
    if cfg!(windows) {
        return 0;
    }
    let ps = if Path::new("/bin/ps").exists() { "/bin/ps" } else { "ps" };
    let listing = crate::bridge_setup::run_program(ps, &["-axo".into(), "uid=,pid=,args=".into()], None).await.stdout;
    #[cfg(unix)]
    // SAFETY: getuid has no preconditions.
    let uid = unsafe { libc::getuid() }.to_string();
    #[cfg(not(unix))]
    let uid = String::new();
    let me = std::process::id().to_string();
    let names: Vec<String> =
        [Some(exe.to_string()), fs::canonicalize(exe).ok().map(|p| p.display().to_string())].into_iter().flatten().collect();
    listing
        .lines()
        .filter_map(|line| {
            let (owner, rest) = line.trim_start().split_once(char::is_whitespace)?;
            let (pid, args) = rest.trim_start().split_once(char::is_whitespace).unwrap_or((rest.trim_start(), ""));
            (owner == uid && pid != me).then_some(args.trim_start())
        })
        .filter(|args| names.iter().any(|name| *args == name.as_str() || args.starts_with(&format!("{name} "))))
        .count()
}
/// After an update: Kumi windows already open still run this Kumi (Windows can't update while one is open). What
/// they keep reaches the new Kumi at its next turn; what it keeps reaches them once restarted. Said only when one is.
pub fn open_windows(others: usize) -> Option<String> {
    match others {
        0 => None,
        1 => Some(format!("A Kumi window opened before the update keeps running {KUMI_VERSION} until you restart it.")),
        n => Some(format!("{n} Kumi windows opened before the update keep running {KUMI_VERSION} until you restart them.")),
    }
}
async fn still_open(io: &InstalledIo, exe: &str) -> Option<String> {
    open_windows(match &io.others {
        Some(others) => others().await,
        None => others_running(exe).await,
    })
}
/// What the farewell adds when the installer started Kumi in a window where `kumi` doesn't work yet, as the installer
/// says in KUMI_FROM_INSTALLER: new terminal windows have it on PATH (`new-window`), or nothing put it there (`no-path`).
pub fn next_time(env: &Env) -> Option<String> {
    let home =
        env.get("HOME").filter(|s| !s.is_empty()).cloned().unwrap_or_else(|| home::home_dir().unwrap_or_default().display().to_string());
    let shown = |path: String| match path.strip_prefix(&home) {
        Some(rest) if !home.is_empty() && rest.starts_with('/') => format!("~{rest}"),
        _ => path,
    };
    let bin = join(&kumi_home(env), "bin");
    match env.get("KUMI_FROM_INSTALLER").map(String::as_str) {
        Some("new-window") => Some("Next time: open a new terminal window and type kumi".into()),
        Some("no-path") => {
            let kumi = shown(join(&bin, "kumi"));
            let typed = if kumi.contains(' ') { format!("\"{}\"", join(&bin, "kumi")) } else { kumi };
            Some(format!("Next time: run {typed}, or add {} to PATH", shown(bin)))
        }
        _ => None,
    }
}

pub async fn rollback_installed(io: InstalledIo) -> Result<i32, RuntimeError> {
    let say = |s: String| io.out.write(&format!("{s}\n"));
    let home = kumi_home(&io.env);
    let app = join(&home, "app");
    let previous = join(&home, "app.previous");
    let hold = join(&home, "app.rollback");
    settle_installed(&home).await;
    if !migration::has_app(&previous) {
        say("There's no earlier Kumi to go back to.".into());
        return Ok(1);
    }
    // Windows can't roll back while a Kumi window is open; elsewhere one keeps running this Kumi.
    let others = match &io.others {
        Some(others) => others().await,
        None => others_running(&join(&app, &executable_name("kumi"))).await,
    };
    match others {
        0 => {}
        1 => say(format!(
            "A Kumi window still open keeps running {KUMI_VERSION} until you close it; what it keeps from now on stays in Kumi's database for your next update."
        )),
        n => say(format!(
            "{n} Kumi windows still open keep running {KUMI_VERSION} until you close them; what they keep from now on stays in Kumi's database for your next update."
        )),
    }
    let legacy = !Path::new(&join(&previous, &executable_name("kumi"))).is_file();
    let version = fs::read(join(&previous, "package.json"))
        .ok()
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
        .and_then(|v| v.get("version").and_then(Value::as_str).map(str::to_string))
        .unwrap_or("the one before".into());
    // A Node Kumi can't run without its own bridge: that goes back first, and forward again if the swap fails. A
    // native one runs with a newer bridge too, so its own goes back after the swap, when it can.
    let (bridge_rollback, earlier) = if legacy {
        (migration::prepare_legacy_rollback(&io, &home).await?, None)
    } else {
        (None, Some(migration::earlier_bridge(&io, &home, &previous, &version).await))
    };
    write_back_for_older(&io).await;
    write_launcher(&home).map_err(error)?;
    if let Some(rollback) = &bridge_rollback {
        rollback.apply().await?;
    }
    let switched = async {
        remove(&hold)?;
        rename(&app, &hold).await?;
        rename(&previous, &app).await?;
        rename(&hold, &previous).await
    }
    .await;
    if switched.is_err() {
        if Path::new(&hold).exists() {
            if Path::new(&app).exists() && !Path::new(&previous).exists() {
                rename(&app, &previous).await.map_err(error)?;
            }
            if !Path::new(&app).exists() {
                rename(&hold, &app).await.map_err(error)?;
            }
        }
        if let Some(rollback) = &bridge_rollback {
            if let Err(error) = rollback.apply().await {
                say(format!(
                    "The application swap failed, and restoring its bridge also failed: {}. Keep Live closed and run: kumi bridge",
                    error.message()
                ));
            }
        }
        say("Couldn't switch back; close every Kumi window and try again.".into());
        return Ok(1);
    }
    say(format!("Kumi is back to {version}. {} update --rollback again returns to {KUMI_VERSION}.", *KUMI));
    if Path::new(&join(&app, &executable_name("kumi"))).exists() {
        let Some(migration::EarlierBridge { rollback, said: Some(said), later }) = earlier else {
            return Ok(bridge_after(&io, &home, &app).await);
        };
        let Some(rollback) = rollback else {
            say(said);
            return Ok(0);
        };
        // This process is still the newer Kumi: the swap only renamed its folder, which nothing from here on reads
        // (on Windows too, where the running executable stays in use). The bridge's own rollback and the kept
        // extension come from the bridge's folders, which the swap doesn't move.
        // Live may have opened while the folders moved (a rename can wait on a held folder): the switch runs with
        // it closed, or not at all.
        if live_open(&io, io.run.clone().unwrap_or_else(default_run)).await {
            say(later.unwrap_or_default());
            return Ok(0);
        }
        match step(io.out.clone(), &io.env, "Putting back the bridge…", rollback.apply(), true).await {
            Ok(()) => {
                say(said);
                if let Some(placed) = rollback.place_extension(&io.env) {
                    say(placed);
                }
                Ok(0)
            }
            // The lifecycle couldn't put its own changes back: the bridge is neither one nor the other.
            Err(reason) if reason.message().contains("compensation was incomplete") => {
                say(format!("The bridge's rollback stopped partway ({}). {} doctor says how to repair it.", reason.message(), *KUMI));
                Ok(1)
            }
            Err(reason) => {
                say(format!(
                    "The bridge couldn't go back ({}): the one in Live stays, and works with {version}. To try again, quit Live, then run {} update --rollback twice.",
                    reason.message(),
                    *KUMI
                ));
                Ok(0)
            }
        }
    } else {
        // The receipt-bound bridge rollback above restored the legacy command/config as well; Live's extension goes
        // back with it.
        if let Some(said) = bridge_rollback.as_ref().and_then(|rollback| rollback.place_extension(&io.env)) {
            say(said);
        }
        Ok(0)
    }
}
pub const PATH_MARKER: &str = "# Added by the Kumi installer";
pub fn startup_files(env: &Env) -> Vec<String> {
    let actual = home::home_dir().unwrap_or_default().display().to_string();
    let home = env.get("HOME").filter(|s| !s.is_empty()).map(String::as_str).unwrap_or(&actual);
    let zsh = env.get("ZDOTDIR").filter(|s| !s.is_empty()).map(String::as_str).unwrap_or(home);
    let mut files = vec![];
    for file in [join(zsh, ".zshrc"), join(zsh, ".zprofile")].into_iter().chain(
        [".zshrc", ".bashrc", ".bash_profile", ".bash_login", ".profile", ".config/fish/conf.d/kumi.fish"].map(|name| join(home, name)),
    ) {
        if !files.contains(&file) {
            files.push(file)
        }
    }
    files
}
/// Kumi's PATH lines in one startup file: what an uninstall takes out, and what it leaves alone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathLines {
    pub file: String,
    /// What the file holds once Kumi's lines are out; None when it goes (fish's kumi.fish with nothing else in it).
    pub after: Option<String>,
    /// Kumi's lines it takes out, with their line numbers (from 1), as they are in the file.
    pub removed: Vec<(usize, String)>,
    /// Kumi's lines it leaves alone because they sit inside an if, for or function there: taking them out could leave
    /// an empty body, which the shell refuses.
    pub left: Vec<(usize, String)>,
}
/// A line that opens a body Kumi's lines could sit in: one ending in `then`, `do`, `{`, `else` or a case's `in`; in
/// fish, one starting with `if`, `for`, `while`, `function`, `begin`, `switch`, `case` or `else` that doesn't end it.
fn opens_a_body(line: &str, fish: bool) -> bool {
    let words: Vec<&str> = line.split_whitespace().collect();
    let last = words.last().copied().unwrap_or("");
    if fish {
        return words.first().is_some_and(|first| ["if", "for", "while", "function", "begin", "switch", "case", "else"].contains(first))
            && last != "end";
    }
    ["then", "do", "{", "else", "in"].contains(&last) || last.ends_with(";then") || last.ends_with(";do")
}
/// A line that closes such a body: `fi`, `done`, `}`, `esac`, `else`, `elif`, `;;`, or fish's `end`.
fn closes_a_body(line: &str) -> bool {
    line.split_whitespace().next().is_some_and(|first| {
        ["fi", "done", "}", "esac", "else", "elif", ";;", "end"].contains(&first.trim_end_matches(';'))
            || first.starts_with('}')
            || first == ";;"
    })
}
/// A line with code on it: not blank, not a comment.
fn code(line: &&str) -> bool {
    let line = line.trim();
    !line.is_empty() && !line.starts_with('#')
}
/// The startup files with Kumi's PATH lines. A marker is found as install.sh finds it, CRLF endings and trailing
/// spaces included. Kumi's line is the one under it naming this Kumi's bin, as written or spelled from $HOME,
/// ${HOME} or ~; the installer's blank line above the marker goes with them, and so does a marker with no PATH line
/// under it. A marker over another Kumi's PATH line, every line under no marker, and a block inside an if, for or
/// function body stay.
pub fn path_lines(env: &Env, home: &str) -> Vec<PathLines> {
    if cfg!(windows) {
        return vec![];
    }
    let bin = join(home, "bin");
    let actual = home::home_dir().unwrap_or_default().display().to_string();
    let user = env.get("HOME").filter(|s| !s.is_empty()).map(String::as_str).unwrap_or(&actual);
    let relative = bin.strip_prefix(&format!("{}/", user.trim_end_matches('/'))).filter(|rest| !rest.is_empty()).map(str::to_string);
    let ours = |line: &str| {
        line.contains(&bin)
            || relative.as_ref().is_some_and(|rest| ["$HOME/", "${HOME}/", "~/"].iter().any(|from| line.contains(&format!("{from}{rest}"))))
    };
    let mut found = vec![];
    for file in startup_files(env) {
        let Ok(text) = fs::read_to_string(&file) else { continue };
        let lines: Vec<&str> = text.split('\n').collect();
        let mut kept: Vec<&str> = vec![];
        let (mut removed, mut left) = (vec![], vec![]);
        let mut index = 0;
        while index < lines.len() {
            let line = lines[index];
            let next = lines.get(index + 1).copied();
            let other_kumis = next.is_some_and(|next| !ours(next) && next.contains("PATH"));
            if line.trim() != PATH_MARKER || other_kumis {
                kept.push(line);
                index += 1;
                continue;
            }
            let block = if next.is_some_and(ours) { 2 } else { 1 };
            // The code around the block, past blank lines and comments: an if, for or function it may sit in.
            let above = kept.iter().rev().find(|line| code(line)).copied();
            let below = lines[index + block..].iter().find(|line| code(line)).copied();
            let indented = line.starts_with([' ', '\t']) || (block == 2 && next.is_some_and(|next| next.starts_with([' ', '\t'])));
            if indented || above.is_some_and(|above| opens_a_body(above, file.ends_with(".fish"))) || below.is_some_and(closes_a_body) {
                left.extend((index..index + block).map(|at| (at + 1, lines[at].trim_end_matches('\r').to_string())));
                kept.extend(&lines[index..index + block]);
            } else {
                if kept.last().is_some_and(|last| last.trim().is_empty()) {
                    kept.pop();
                }
                removed.extend((index..index + block).map(|at| (at + 1, lines[at].trim_end_matches('\r').to_string())));
            }
            index += block;
        }
        if removed.is_empty() && left.is_empty() {
            continue;
        }
        let after = kept.join("\n");
        let emptied = file.ends_with("kumi.fish") && after.trim().is_empty();
        found.push(PathLines { file, after: (!emptied).then_some(after), removed, left });
    }
    found
}
/// Whether this account can make the change `lines` needs: write the file, or remove it from its folder.
fn can_change(lines: &PathLines) -> bool {
    #[cfg(unix)]
    {
        let target = if lines.after.is_some() { lines.file.clone() } else { dirname(&lines.file) };
        let Ok(path) = std::ffi::CString::new(target) else { return false };
        // SAFETY: `path` is a valid C string.
        unsafe { libc::access(path.as_ptr(), libc::W_OK) == 0 }
    }
    #[cfg(not(unix))]
    {
        let _ = lines;
        true
    }
}
/// Kumi's lines as the producer would take them out by hand.
fn by_hand(lines: &[(usize, String)]) -> String {
    let first = lines.first().map_or(0, |(at, _)| *at);
    let last = lines.last().map_or(0, |(at, _)| *at);
    let span = if first == last { format!("line {first}") } else { format!("lines {first}–{last}") };
    format!("{span}: {}", lines.iter().map(|(_, line)| line.as_str()).collect::<Vec<_>>().join(" / "))
}
/// Takes Kumi's PATH lines out of the startup files, and says what it did and what it couldn't do (with the lines to
/// take out by hand). False when a file couldn't be changed.
pub fn remove_path_lines(io: &InstalledIo, home: &str) -> bool {
    let user = user_home(&io.env);
    let mut changed = true;
    for lines in path_lines(&io.env, home) {
        let shown = tilde_in(&lines.file, &user);
        if !lines.left.is_empty() {
            io.out.write(&format!(
                "Left Kumi's PATH line in {shown} alone ({}): it sits inside an if, for or function there; take it out by hand if you like.\n",
                by_hand(&lines.left)
            ));
        }
        if lines.removed.is_empty() {
            continue;
        }
        let result = match &lines.after {
            Some(text) => fs::write(&lines.file, text).map(|()| format!("Took Kumi out of {shown}.")),
            None => remove(&lines.file).map(|()| format!("Took Kumi out of fish ({shown} is gone).")),
        };
        io.out.write(&match result {
            Ok(said) => format!("{said}\n"),
            Err(error) => {
                changed = false;
                format!("Couldn't take Kumi out of {shown} ({error}): take out {} by hand.\n", by_hand(&lines.removed))
            }
        });
    }
    changed
}
async fn remove_windows_path(run: Run, home: &str) {
    if !cfg!(windows) {
        return;
    }
    let bin = join(home, "bin").replace('\'', "''");
    let script = format!(
        r#"$key = (Get-Item -LiteralPath 'HKCU:\').OpenSubKey('Environment', $true)
$path = $key.GetValue('Path', '', 'DoNotExpandEnvironmentNames')
if ($path) {{
  $kept = ($path -split ';' | Where-Object {{ $_ -and ($_.TrimEnd('\') -ne '{bin}') }}) -join ';'
  if ($kept -ne $path) {{
    $key.SetValue('Path', $kept, $key.GetValueKind('Path'))
    Add-Type -Namespace Kumi -Name Env -MemberDefinition '[DllImport("user32.dll", CharSet = CharSet.Auto)] public static extern System.IntPtr SendMessageTimeout(System.IntPtr hWnd, uint msg, System.UIntPtr wParam, string lParam, uint flags, uint timeout, out System.UIntPtr result);'
    $result = [UIntPtr]::Zero; [void][Kumi.Env]::SendMessageTimeout([IntPtr]0xffff, 0x1a, [UIntPtr]::Zero, 'Environment', 2, 5000, [ref]$result)
  }}
}}"#
    );
    let _ = run(
        system::system_program_default(SystemProgram::Powershell),
        vec!["-NoProfile".into(), "-NonInteractive".into(), "-Command".into(), script],
        None,
    )
    .await;
}
#[derive(PartialEq, Eq)]
enum RemovedBridge {
    Removed,
    Kept,
    None,
}
async fn remove_bridge(io: &InstalledIo, run: Run) -> Result<RemovedBridge, RuntimeError> {
    let say = |s: String| io.out.write(&format!("{s}\n"));
    let Some(config) = find_bridge_config(&io.env) else { return Ok(RemovedBridge::None) };
    let extensions = live_extensions_dir(&io.env, system::platform());
    let by_hand = format!(
        "AbletonMcpBridge from Live's Remote Scripts folder{}",
        extensions
            .as_ref()
            .map(|ext| format!(", and {KUMI_EXTENSION_ID} from {ext} and from {}", dirname(&extension_data_dir(ext))))
            .unwrap_or_default()
    );
    if !ask(io, "Remove the Ableton bridge from Live too?", None).await {
        say(format!("The bridge stays in Live, and so do its files. To take it out later, remove {by_hand}."));
        return Ok(RemovedBridge::Kept);
    }
    if live_open(io, run.clone()).await {
        say(format!("Live is open, so the bridge stays, and so do its files. Quit Live, then remove {by_hand}."));
        return Ok(RemovedBridge::Kept);
    }
    let root = read_bridge_server(&config).ok().and_then(|server| server.package_root());
    let lifecycle =
        root.as_ref().map(|root| join(root, &executable_name("ableton-mcp-server"))).filter(|file| Path::new(file).is_file()).or_else(
            || {
                let file = join(&executable_dir(), &executable_name("ableton-mcp-server"));
                Path::new(&file).is_file().then_some(file)
            },
        );
    let Some((root, lifecycle)) = root.zip(lifecycle) else {
        say(format!("Kumi couldn't find the bridge's own uninstaller; remove {by_hand} by hand."));
        return Ok(RemovedBridge::Kept);
    };
    let ran = step(
        io.out.clone(),
        &io.env,
        "Taking the bridge out of Live…",
        run(
            lifecycle,
            vec![
                "lifecycle".into(),
                "uninstall".into(),
                "--remote-scripts-dir".into(),
                remote_scripts_dir(&io.env),
                "--state-dir".into(),
                dirname(&config),
                "--package-root".into(),
                root,
                "--apply".into(),
                "--confirm-live-stopped".into(),
            ],
            None,
        ),
        false,
    )
    .await;
    if ran.code != 0 {
        say(format!("The bridge's uninstaller refused; remove {by_hand} by hand."));
        return Ok(RemovedBridge::Kept);
    }
    let removed_extension = extensions.as_ref().map(|ext| remove_extension(ext)).transpose().map_err(error)?.unwrap_or(false);
    let removed_former = remove_former_extension(&io.env, system::platform()).map_err(error)?;
    say(if removed_extension || removed_former {
        "The bridge and Kumi's extension are out of Live."
    } else {
        "The bridge is out of Live."
    }
    .into());
    let scripts = remote_scripts_dir(&io.env);
    if basename(&scripts) == "Remote Scripts" {
        remove(&join(&join(&dirname(&scripts), "Kumi"), &format!("{EARS_NAME}.amxd"))).map_err(error)?;
    }
    Ok(RemovedBridge::Removed)
}
fn bridge_uses(folder: &str, env: &Env) -> bool {
    let Some(config) = find_bridge_config(env) else { return false };
    let inside = |path: &str| {
        let mut folder = resolve(folder);
        let mut path = resolve(path);
        if cfg!(windows) {
            folder = folder.to_lowercase();
            path = path.to_lowercase();
        }
        Path::new(&path)
            .strip_prefix(&folder)
            .is_ok_and(|relative| !relative.as_os_str().is_empty() && !relative.to_string_lossy().starts_with(".."))
    };
    inside(&config) || read_bridge_server(&config).ok().and_then(|s| s.entry).is_some_and(|entry| inside(&entry))
}
#[derive(Clone, Copy, Default)]
pub struct UninstallOptions {
    pub all: bool,
    pub yes: bool,
    /// Say what would go, and remove nothing.
    pub dry_run: bool,
}
/// What `kumi uninstall` takes out of Kumi's folder, by these exact names: Kumi and the one kept for a rollback, what
/// updates and repairs leave behind (`app.replaced` is the Windows installer's), and what Kumi fetched and fetches
/// again when needed (programs, models, videos; each only while it holds only Kumi's own). Never conversations, notes,
/// recipes, sign-ins, the library or references; the bridge's files stay while Live uses them.
pub const KUMI_S_OWN: [&str; 13] = [
    "app",
    "app.previous",
    "app.previous.old",
    "app.new",
    "app.replaced",
    "app.rollback",
    "node",
    "bin",
    "downloads",
    "tools",
    "models",
    "videos",
    "bridge",
];
/// What the producer keeps unless they say --all.
const KEPT: &str = "conversations, notes, recipes, sign-ins, library and references";
/// Why a folder isn't one of Kumi's own to uninstall from. In the home folder or at a disk's root, the names Kumi
/// uses (bin, downloads, tools…) could be the producer's own; on macOS and Windows `downloads` is also their Downloads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotKumis {
    /// A relative path: where it points depends on the folder Kumi runs in.
    Relative,
    /// There's no such folder.
    Missing,
    /// The home folder itself.
    Home,
    /// A disk's root: /, C:\, or where a disk is mounted.
    Root,
    /// It holds no Kumi launcher (bin/kumi) and app.
    Unmarked,
}
/// Whether `home` is a folder of Kumi's own, which an uninstall may take Kumi's parts out of; why not, when it isn't.
pub fn not_kumis_own(home: &str, env: &Env) -> Option<NotKumis> {
    let folder = Path::new(home);
    if !folder.is_absolute() {
        return Some(NotKumis::Relative);
    }
    let Ok(real) = fs::canonicalize(folder) else { return Some(NotKumis::Missing) };
    let homes = [env.get("HOME").cloned(), env.get("USERPROFILE").cloned(), home::home_dir().map(|home| home.display().to_string())];
    if homes.into_iter().flatten().filter(|home| !home.is_empty()).any(|home| same_folder(&real, Path::new(&home))) {
        return Some(NotKumis::Home);
    }
    if real.parent().is_none() || mount_point(&real) {
        return Some(NotKumis::Root);
    }
    let launcher = real.join("bin").join(if cfg!(windows) { "kumi.cmd" } else { "kumi" });
    let names_kumi = |app: &str| {
        let package = fs::read(real.join(app).join("package.json")).ok().and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok());
        package.is_some_and(|package| package["name"] == "kumi")
    };
    if !launcher.is_file() || !(names_kumi("app") || names_kumi("app.previous")) {
        return Some(NotKumis::Unmarked);
    }
    None
}
/// Whether two paths name one folder, however each is spelled (case, links, a trailing separator).
fn same_folder(a: &Path, b: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if let (Ok(a), Ok(b)) = (fs::metadata(a), fs::metadata(b)) {
            return a.dev() == b.dev() && a.ino() == b.ino();
        }
    }
    fs::canonicalize(a).ok().zip(fs::canonicalize(b).ok()).is_some_and(|(a, b)| a == b)
}
/// Whether a disk is mounted at `path`.
fn mount_point(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if let (Ok(here), Some(Ok(above))) = (fs::metadata(path), path.parent().map(fs::metadata)) {
            return here.dev() != above.dev();
        }
    }
    let _ = path;
    false
}
/// `path` as the producer reads it, with `home` (their home folder) as ~.
fn tilde_in(path: &str, home: &str) -> String {
    let home = home.trim_end_matches(['/', '\\']);
    match path.strip_prefix(home) {
        Some(rest) if !home.is_empty() && (rest.is_empty() || rest.starts_with(['/', '\\'])) => format!("~{rest}"),
        _ => path.into(),
    }
}
/// The producer's home folder, as `env` has it.
fn user_home(env: &Env) -> String {
    let actual = home::home_dir().unwrap_or_default().display().to_string();
    env.get("HOME").or_else(|| env.get("USERPROFILE")).filter(|s| !s.is_empty()).cloned().unwrap_or(actual)
}
/// Why an uninstall won't take anything out of `home`, and Kumi's own parts there to take out by hand.
fn not_kumis_said(home: &str, why: NotKumis, display: &dyn Fn(&str) -> String) -> String {
    let shown = display(home);
    let names = "so the names Kumi uses there (bin, downloads, tools…) could be your own";
    let mut said = match why {
        NotKumis::Relative => {
            return format!(
                "Kumi won't uninstall from {shown}: KUMI_HOME is a relative path, so where it points depends on the folder you're in. Set it to the whole path of Kumi's folder and run kumi uninstall again. Nothing was removed."
            )
        }
        NotKumis::Missing => return format!("There's no {shown} (Kumi's folder) to uninstall Kumi from. Nothing was removed."),
        NotKumis::Home => format!("Kumi won't uninstall from {shown}: it's your home folder, {names}."),
        NotKumis::Root => format!("Kumi won't uninstall from {shown}: it's the root of a disk, {names}."),
        NotKumis::Unmarked => format!("Kumi won't uninstall from {shown}: it holds no Kumi launcher (bin/kumi) and app, {names}."),
    };
    said.push_str(" Nothing was removed.");
    let mut parts: Vec<String> = ["app", "app.previous", "app.previous.old", "app.new", "app.replaced", "app.rollback"]
        .into_iter()
        .map(|part| join(home, part))
        .filter(|part| has_app(part))
        .collect();
    let launcher = join(&join(home, "bin"), if cfg!(windows) { "kumi.cmd" } else { "kumi" });
    if fs::read_to_string(&launcher).is_ok_and(|text| text.contains("KUMI_INSTALLED")) {
        parts.push(launcher);
    }
    if !parts.is_empty() {
        let shown: Vec<String> = parts.iter().map(|part| display(part)).collect();
        let path = if cfg!(windows) {
            format!("{} from your PATH", display(&join(home, "bin")))
        } else {
            format!("the two lines under \"{PATH_MARKER}\" in your shell's startup file")
        };
        said.push_str(&format!(" Kumi's own there, to take out by hand: {}, and {path}.", listed(&shown)));
    }
    said
}
/// Whether a folder Kumi fetches into (tools, models, videos) holds only what Kumi puts there.
fn holds_kumis_own(part: &str, folder: &Path) -> bool {
    use kumi_runtime::models::pinned::{AFX_REP, BASIC_PITCH, CLAP};
    let Ok(entries) = fs::read_dir(folder) else { return false };
    let names = |folder: &Path| -> Vec<String> {
        fs::read_dir(folder)
            .into_iter()
            .flatten()
            .filter_map(Result::ok)
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect()
    };
    let litter = |name: &str| [".DS_Store", "Thumbs.db", "desktop.ini"].contains(&name);
    entries.filter_map(Result::ok).all(|entry| {
        let name = entry.file_name().to_string_lossy().into_owned();
        litter(&name)
            || match part {
                "tools" => {
                    ["ffmpeg", "whisper", "whisper-models", "yt-dlp", "hands"].contains(&name.as_str())
                        || [".ffmpeg-", ".whisper-", ".yt-dlp"].iter().any(|start| name.starts_with(start))
                }
                "models" => {
                    [CLAP.name, AFX_REP.name, BASIC_PITCH.name, "slots"].contains(&name.as_str())
                        || name.starts_with("onnxruntime")
                        || name.contains(".partial-")
                        || name.contains(".unpacking-")
                }
                // A video's folder, named as Kumi names them (its site and id, in lowercase), with Kumi's files in it.
                "videos" => {
                    entry.path().is_dir()
                        && name.contains('-')
                        && name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
                        && names(&entry.path()).iter().all(|inside| {
                            litter(inside)
                                || ["meta.json", "cues.json", "captions", "frames", "sound"].contains(&inside.as_str())
                                || inside.starts_with("speech-")
                        })
                }
                _ => false,
            }
    })
}
/// What an uninstall takes out of `home`, a folder of Kumi's own: with --all everything in it (the bridge's files
/// excepted while Live uses them); otherwise Kumi's parts, by their exact names as the folder lists them, and what a
/// stopped installer left (`.install.*`). In KUMI_S_OWN's order. The second list: fetched folders that stay because
/// they hold more than Kumi put there.
fn own_parts(home: &str, all: bool, bridge_stays: bool) -> Result<(Vec<String>, Vec<String>), RuntimeError> {
    let mut names: Vec<String> = match fs::read_dir(home) {
        Ok(entries) => entries
            .map(|entry| entry.map(|entry| entry.file_name().to_string_lossy().into_owned()))
            .collect::<Result<_, _>>()
            .map_err(error)?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => vec![],
        Err(e) => return Err(error(e)),
    };
    names.sort_by_key(|name| (KUMI_S_OWN.iter().position(|own| own == name).unwrap_or(KUMI_S_OWN.len()), name.clone()));
    let (mut parts, mut left) = (vec![], vec![]);
    for name in names {
        let path = join(home, &name);
        if name == "bridge" && bridge_stays {
            continue;
        }
        if all {
            parts.push(path);
        } else if name.starts_with(".install.") || KUMI_S_OWN.contains(&name.as_str()) {
            if ["tools", "models", "videos"].contains(&name.as_str()) && !holds_kumis_own(&name, Path::new(&path)) {
                left.push(path);
            } else {
                parts.push(path);
            }
        }
    }
    Ok((parts, left))
}
/// The bytes in `path`, its folders walked without following links.
fn bytes_in(path: &Path) -> u64 {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.is_dir() => {
            fs::read_dir(path).into_iter().flatten().filter_map(Result::ok).map(|entry| bytes_in(&entry.path())).sum()
        }
        Ok(meta) => meta.len(),
        Err(_) => 0,
    }
}
fn listed(words: &[String]) -> String {
    match words {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}
/// What's said of the fetched folders that stay because they hold more than Kumi put there.
fn left_said(left: &[String], display: &dyn Fn(&str) -> String) -> Option<String> {
    let shown: Vec<String> = left.iter().map(|path| display(path)).collect();
    match shown.len() {
        0 => None,
        1 => Some(format!("{} stays: it holds more than Kumi put there.", shown[0])),
        _ => Some(format!("{} stay: they hold more than Kumi put there.", listed(&shown))),
    }
}
/// `kumi uninstall --dry-run`: what would go, and what would stay.
fn dry_run(io: &InstalledIo, home: &str, options: UninstallOptions, display: &dyn Fn(&str) -> String) -> Result<i32, RuntimeError> {
    let say = |s: String| io.out.write(&format!("{s}\n"));
    if let Some(folder) = not_yours(home) {
        say(format!(
            "It would stop before removing anything: {} belongs to another account (installed with sudo?). Run this first: {}",
            display(&folder),
            chown_line(home)
        ));
    }
    let bridge = find_bridge_config(&io.env).is_some();
    let (parts, left) = own_parts(home, options.all, bridge && bridge_uses(&join(home, "bridge"), &io.env))?;
    let bytes: u64 = parts.iter().map(|part| bytes_in(Path::new(part))).sum();
    if options.all {
        say(format!("kumi uninstall --all would remove {} ({}), with your {KEPT}.", display(home), size(bytes as f64)));
    } else if parts.is_empty() {
        say(format!("kumi uninstall would find nothing of Kumi's own left in {}.", display(home)));
    } else {
        let names: Vec<String> = parts.iter().map(|part| basename(part)).collect();
        say(format!("kumi uninstall would remove from {} ({}): {}.", display(home), size(bytes as f64), names.join(", ")));
    }
    if let Some(said) = left_said(&left, display) {
        say(said);
    }
    let lines = path_lines(&io.env, home);
    let files: Vec<String> =
        lines.iter().filter(|lines| !lines.removed.is_empty() && can_change(lines)).map(|lines| display(&lines.file)).collect();
    let mut also = vec![];
    if cfg!(windows) {
        also.push("take Kumi out of your PATH".to_string());
    } else if !files.is_empty() {
        also.push(format!("take Kumi out of {}", listed(&files)));
    }
    if bridge {
        also.push("ask whether to take the bridge out of Live too (its files stay while Live uses it)".into());
    }
    if !also.is_empty() {
        say(format!("It would also {}.", also.join(", and ")));
    }
    for lines in &lines {
        if !lines.removed.is_empty() && !can_change(lines) {
            say(format!("It couldn't change {} (it isn't writable): take out {} by hand.", display(&lines.file), by_hand(&lines.removed)));
        }
        if !lines.left.is_empty() {
            say(format!(
                "It would leave Kumi's PATH line in {} alone ({}): it sits inside an if, for or function there.",
                display(&lines.file),
                by_hand(&lines.left)
            ));
        }
    }
    if !options.all {
        say(format!("Your {KEPT} would stay in {}.", display(home)));
    }
    say("Nothing was removed (--dry-run).".into());
    Ok(0)
}
/// Removes each of `parts`; the ones that stayed, with why.
#[cfg(not(windows))]
fn removing(parts: &[String]) -> Vec<(String, std::io::Error)> {
    parts.iter().filter_map(|path| remove(path).err().map(|error| (path.clone(), error))).collect()
}
pub async fn uninstall_installed(io: InstalledIo, options: UninstallOptions) -> Result<i32, RuntimeError> {
    let say = |s: String| io.out.write(&format!("{s}\n"));
    let home = kumi_home(&io.env);
    let user = user_home(&io.env);
    let display = |path: &str| tilde_in(path, &user);
    // Before any question, any change, or any look inside: a folder that isn't Kumi's own keeps everything in it.
    if let Some(why) = not_kumis_own(&home, &io.env) {
        say(not_kumis_said(&home, why, &display));
        return Ok(1);
    }
    if options.dry_run {
        return dry_run(&io, &home, options, &display);
    }
    // A folder another account owns can't lose its parts: said before anything goes, so Kumi isn't left half removed.
    if not_yours(&home).is_some() {
        say(format!("{} Nothing was removed.", owner_advice(&home)));
        return Ok(1);
    }
    let run = io.run.clone().unwrap_or_else(default_run);
    let keeps = if options.all {
        format!("Everything in it goes too: your {KEPT}.")
    } else {
        format!("Your {KEPT} stay (add --all to remove them too).")
    };
    if !options.yes && !ask(&io, &format!("Remove Kumi from {}? {keeps}", display(&home)), Some("add --yes")).await {
        say("Nothing was removed.".into());
        return Ok(1);
    }
    let bridge = remove_bridge(&io, run.clone()).await?;
    let bridge_stays = bridge != RemovedBridge::Removed && bridge_uses(&join(&home, "bridge"), &io.env);
    let (parts, left) = own_parts(&home, options.all, bridge_stays)?;
    let bytes: u64 = parts.iter().map(|part| bytes_in(Path::new(part))).sum();
    // Every part is tried, Kumi itself (app and bin) last: when another part stays, Kumi does too, so kumi uninstall
    // can run again once that's sorted.
    #[cfg(not(windows))]
    let (stayed, kumi_stays) = {
        let (kumi, rest): (Vec<String>, Vec<String>) =
            parts.iter().cloned().partition(|path| ["app", "bin"].contains(&basename(path).as_str()));
        let mut stayed = removing(&rest);
        let kumi_stays = !stayed.is_empty();
        if !kumi_stays {
            stayed = removing(&kumi);
            if stayed.is_empty() && options.all && !bridge_stays {
                stayed.extend(remove(&home).err().map(|error| (home.clone(), error)));
            }
        }
        (stayed, kumi_stays)
    };
    #[cfg(windows)]
    let (stayed, kumi_stays): (Vec<(String, std::io::Error)>, bool) = {
        // Windows won't remove a running program: a detached cmd does, once this Kumi has exited.
        use std::os::windows::process::CommandExt;
        let targets = if options.all && !bridge_stays { vec![home.clone()] } else { parts.clone() };
        let list =
            targets.iter().map(|path| format!("rmdir /s /q \"{path}\" 2>nul & del /f /q \"{path}\" 2>nul")).collect::<Vec<_>>().join(" & ");
        std::process::Command::new(std::env::var("ComSpec").unwrap_or_else(|_| "cmd.exe".into()))
            .raw_arg(format!("/d /s /c \"ping -n 4 127.0.0.1 >nul & {list}\""))
            .creation_flags(0x00000008 | 0x00000200 | 0x08000000)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(error)?;
        (vec![], false)
    };
    // The startup files last, and only once Kumi's bin has gone: until then its PATH line still finds Kumi.
    let rc_changed = if Path::new(&join(&home, "bin")).exists() && !cfg!(windows) { true } else { remove_path_lines(&io, &home) };
    if cfg!(windows) {
        step(io.out.clone(), &io.env, "Taking Kumi out of your PATH…", remove_windows_path(run, &home), false).await;
    }
    if bridge_stays {
        say(format!("The bridge's files stay in {} while Live uses it.", display(&join(&home, "bridge"))));
    }
    if let Some(said) = left_said(&left, &display) {
        say(said);
    }
    if let Some((_, why)) = stayed.first() {
        let names: Vec<String> = stayed.iter().map(|(path, _)| display(path)).collect();
        let them = if names.len() == 1 { "it" } else { "them" };
        let owner = if stayed.iter().any(|(_, error)| error.kind() == std::io::ErrorKind::PermissionDenied) {
            format!(" (if another account owns {them}: {})", chown_line(&home))
        } else {
            String::new()
        };
        say(format!(
            "Kumi is partly removed: {} couldn't go ({why}). {}",
            listed(&names),
            if kumi_stays {
                format!("Kumi itself stays, so you can run kumi uninstall again once that's sorted{owner}, or take {them} out by hand.")
            } else {
                format!("Take {them} out by hand{owner}.")
            }
        ));
        return Ok(1);
    }
    let freed = if bytes > 0 { format!(", freeing {}", size(bytes as f64)) } else { String::new() };
    say(if options.all {
        format!("Kumi is removed, with everything it kept{}{freed}.", if bridge_stays { " but the bridge's files" } else { "" })
    } else {
        format!("Kumi is removed{freed}. Your {KEPT} are still in {}; delete that folder to remove them too.", display(&home))
    });
    say("Open a new terminal window so the `kumi` command is gone there too.".into());
    Ok(i32::from(!rc_changed))
}
pub async fn check_release(env: &Env, fetcher: Option<Rc<dyn Fetch>>) -> Result<Option<String>, RuntimeError> {
    match ask_newest(env, fetcher.unwrap_or_else(default_fetch)).await {
        (_, AskedRelease::Manifest(manifest)) => Ok(newer_version(&manifest.kumi, KUMI_VERSION).then_some(manifest.kumi)),
        (base, asked) => Err(RuntimeError::plain(unasked(&asked, &base, None, env))),
    }
}
pub async fn newer_release(cache_file: &str, env: &Env, now: Option<f64>, fetcher: Option<Rc<dyn Fetch>>) -> Option<String> {
    let now = now.unwrap_or_else(|| now_ms() as f64);
    if let Some(cached) = fs::read(cache_file).ok().and_then(|b| serde_json::from_slice::<Value>(&b).ok()) {
        if cached.get("checkedAt").and_then(Value::as_f64).is_some_and(|at| now - at < 86400000. && now >= at) {
            return cached.get("latest").and_then(Value::as_str).filter(|latest| newer_version(latest, KUMI_VERSION)).map(str::to_string);
        }
    }
    let manifest = fetch_manifest(env, fetcher).await?;
    let mut options = fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    if let Ok(mut file) = options.open(cache_file) {
        use std::io::Write;
        let _ = file.write_all(stringify(&json!({"checkedAt":now,"latest":manifest.kumi})).as_bytes());
    }
    newer_version(&manifest.kumi, KUMI_VERSION).then_some(manifest.kumi)
}

#[cfg(test)]
mod swap_tests {
    use super::*;
    use std::cell::RefCell;
    use std::path::PathBuf;

    /// What a move does once it's let through.
    #[derive(Clone, Copy)]
    enum Then {
        Moves,
        /// Leaves part of the folder behind and fails, as a move between disks can.
        LeavesPart,
        /// Goes through, then reports an error anyway.
        MovesButFails,
    }
    /// The disk, failing moves the ways Windows can: busy for some tries (or every one) while a Kumi window or an
    /// antivirus scan holds a folder, then what the move does once it's let through.
    #[derive(Default)]
    struct Windows(RefCell<Vec<(&'static str, &'static str, usize, Then)>>);
    impl Windows {
        fn holds(self, from: &'static str, to: &'static str, tries: usize, then: Then) -> Self {
            self.0.borrow_mut().push((from, to, tries, then));
            self
        }
    }
    fn name(path: &str) -> String {
        Path::new(path).file_name().unwrap().to_string_lossy().into()
    }
    impl Folders for Windows {
        fn rename(&self, from: &str, to: &str) -> std::io::Result<()> {
            let mut rules = self.0.borrow_mut();
            let Some(rule) = rules.iter_mut().find(|rule| rule.0 == name(from) && rule.1 == name(to)) else {
                return fs::rename(from, to);
            };
            if rule.2 > 0 {
                rule.2 -= 1;
                // A held folder: Windows' sharing violation, or a busy mount elsewhere.
                return Err(std::io::Error::from_raw_os_error(if cfg!(windows) { 32 } else { 16 }));
            }
            match rule.3 {
                Then::Moves => fs::rename(from, to),
                Then::LeavesPart => {
                    fs::create_dir_all(to)?;
                    fs::write(Path::new(to).join("part"), "")?;
                    Err(std::io::Error::other("the move stopped part of the way"))
                }
                Then::MovesButFails => {
                    fs::rename(from, to)?;
                    Err(std::io::Error::other("reported after the move"))
                }
            }
        }
        fn remove(&self, path: &str) -> std::io::Result<()> {
            remove(path)
        }
    }
    const ALWAYS: usize = usize::MAX;

    struct Home(tempfile::TempDir);
    impl Home {
        fn new() -> Self {
            Home(tempfile::tempdir().unwrap())
        }
        fn path(&self, folder: &str) -> String {
            self.0.path().join(folder).to_string_lossy().into()
        }
        /// A whole native Kumi of `version` in `folder`.
        fn kumi(&self, folder: &str, version: &str) -> &Self {
            let folder = PathBuf::from(self.path(folder));
            fs::create_dir_all(&folder).unwrap();
            fs::write(folder.join(executable_name("kumi")), version).unwrap();
            self
        }
        /// The version of the whole Kumi in `folder`: a native one's, or "node" for an earlier Node Kumi.
        fn version(&self, folder: &str) -> Option<String> {
            let folder = self.path(folder);
            if !migration::has_app(&folder) {
                return None;
            }
            Some(fs::read_to_string(join(&folder, &executable_name("kumi"))).unwrap_or_else(|_| "node".into()))
        }
        async fn swap(&self, windows: &Windows) -> std::io::Result<()> {
            self.swap_with(windows).await
        }
        async fn swap_with(&self, folders: &impl Folders) -> std::io::Result<()> {
            swap_in_with(folders, &self.path("app.new"), &self.path("app"), &self.path("app.previous")).await
        }
    }
    /// app holds 1.2, app.previous 1.1 to roll back to, and 1.3 waits in app.new.
    fn updating() -> Home {
        let home = Home::new();
        home.kumi("app", "1.2").kumi("app.previous", "1.1").kumi("app.new", "1.3");
        home
    }

    #[tokio::test(start_paused = true)]
    async fn a_folder_busy_for_a_moment_is_waited_out() {
        let home = updating();
        home.swap(&Windows::default().holds("app.new", "app", 5, Then::Moves)).await.unwrap();
        assert_eq!(home.version("app").as_deref(), Some("1.3"));
        assert_eq!(home.version("app.previous").as_deref(), Some("1.2"));
        assert!(!Path::new(&home.path("app.previous.old")).exists());
    }

    #[tokio::test(start_paused = true)]
    async fn a_new_kumi_that_cant_go_in_leaves_the_kumi_that_was_there_and_the_one_before() {
        let home = updating();
        assert!(home.swap(&Windows::default().holds("app.new", "app", ALWAYS, Then::Moves)).await.is_err());
        assert_eq!(home.version("app").as_deref(), Some("1.2"));
        assert_eq!(home.version("app.previous").as_deref(), Some("1.1"));
        assert!(!Path::new(&home.path("app.previous.old")).exists());
        // The caller clears app.new; the next update goes through.
        home.swap(&Windows::default()).await.unwrap();
        assert_eq!(home.version("app").as_deref(), Some("1.3"));
    }

    #[tokio::test(start_paused = true)]
    async fn part_of_a_move_is_cleared_and_the_kumi_that_was_there_goes_back() {
        let home = updating();
        assert!(home.swap(&Windows::default().holds("app.new", "app", 0, Then::LeavesPart)).await.is_err());
        assert_eq!(home.version("app").as_deref(), Some("1.2"));
        assert!(!Path::new(&home.path("app")).join("part").exists());
        assert_eq!(home.version("app.previous").as_deref(), Some("1.1"));
    }

    #[tokio::test(start_paused = true)]
    async fn a_put_back_busy_past_its_first_wait_is_tried_again() {
        let home = updating();
        // Each rename waits out 21 tries; the put-back's first goes by, its second goes through.
        let windows = Windows::default().holds("app.new", "app", ALWAYS, Then::Moves).holds("app.previous", "app", 25, Then::Moves);
        assert!(home.swap(&windows).await.is_err());
        assert_eq!(home.version("app").as_deref(), Some("1.2"));
        assert_eq!(home.version("app.previous").as_deref(), Some("1.1"));
    }

    #[tokio::test(start_paused = true)]
    async fn an_earlier_node_kumi_is_a_whole_kumi_too() {
        let home = Home::new();
        home.kumi("app.new", "1.3");
        put_file(&home.path("app/apps/kumi/bin/kumi.mjs"));
        assert!(home.swap(&Windows::default().holds("app.new", "app", ALWAYS, Then::Moves)).await.is_err());
        assert_eq!(home.version("app").as_deref(), Some("node"));
        home.swap(&Windows::default()).await.unwrap();
        assert_eq!(home.version("app").as_deref(), Some("1.3"));
        assert_eq!(home.version("app.previous").as_deref(), Some("node"));
    }

    #[tokio::test(start_paused = true)]
    async fn a_first_install_has_nothing_to_keep() {
        let home = Home::new();
        home.kumi("app.new", "1.3");
        home.swap(&Windows::default()).await.unwrap();
        assert_eq!(home.version("app").as_deref(), Some("1.3"));
        assert!(!Path::new(&home.path("app.previous")).exists());
    }

    #[tokio::test(start_paused = true)]
    async fn a_run_after_one_that_gave_up_puts_the_kumi_back_first() {
        // An update that gave up with app emptied: 1.2 in app.previous, and 1.1 aside in app.previous.old.
        let home = Home::new();
        home.kumi("app.previous", "1.2").kumi("app.previous.old", "1.1");
        settle_installed(home.0.path().to_str().unwrap()).await;
        assert_eq!(home.version("app").as_deref(), Some("1.2"));
        assert_eq!(home.version("app.previous").as_deref(), Some("1.1"));
        home.kumi("app.new", "1.3");
        home.swap(&Windows::default()).await.unwrap();
        assert_eq!(home.version("app").as_deref(), Some("1.3"));
        assert_eq!(home.version("app.previous").as_deref(), Some("1.2"));
    }

    #[tokio::test(start_paused = true)]
    async fn a_run_after_one_that_gave_up_keeps_app_previous_while_its_put_back_stays_busy() {
        let home = Home::new();
        home.kumi("app.previous", "1.2").kumi("app.new", "1.3");
        home.swap(&Windows::default().holds("app.previous", "app", ALWAYS, Then::Moves)).await.unwrap();
        assert_eq!(home.version("app").as_deref(), Some("1.3"));
        assert_eq!(home.version("app.previous").as_deref(), Some("1.2"));
    }

    #[tokio::test(start_paused = true)]
    async fn a_move_that_went_through_before_its_error_counts() {
        let home = updating();
        home.swap(&Windows::default().holds("app.new", "app", 0, Then::MovesButFails)).await.unwrap();
        assert_eq!(home.version("app").as_deref(), Some("1.3"));
        assert_eq!(home.version("app.previous").as_deref(), Some("1.2"));
    }

    #[tokio::test(start_paused = true)]
    async fn when_nothing_can_go_back_app_previous_keeps_the_kumi_that_was_there() {
        let home = updating();
        let windows = Windows::default().holds("app.new", "app", ALWAYS, Then::Moves).holds("app.previous", "app", ALWAYS, Then::Moves);
        assert!(home.swap(&windows).await.is_err());
        assert_eq!(home.version("app"), None);
        assert_eq!(home.version("app.previous").as_deref(), Some("1.2"));
        assert_eq!(home.version("app.previous.old").as_deref(), Some("1.1"));
        // Once Windows lets go, the next run puts 1.2 back before anything else, and 1.1 beside it.
        settle_installed(home.0.path().to_str().unwrap()).await;
        assert_eq!(home.version("app").as_deref(), Some("1.2"));
        assert_eq!(home.version("app.previous").as_deref(), Some("1.1"));
    }

    #[tokio::test(start_paused = true)]
    async fn a_wait_for_a_busy_folder_is_said_once() {
        let home = updating();
        let said = std::cell::Cell::new(0);
        let windows = Windows::default().holds("app.new", "app", ALWAYS, Then::Moves);
        let folders = Saying { folders: &windows, say: || said.set(said.get() + 1), said: Default::default() };
        assert!(swap_in_with(&folders, &home.path("app.new"), &home.path("app"), &home.path("app.previous")).await.is_err());
        assert_eq!(said.get(), 1);
        let quiet = updating();
        let folders = Saying { folders: &Windows::default(), say: || said.set(said.get() + 1), said: Default::default() };
        swap_in_with(&folders, &quiet.path("app.new"), &quiet.path("app"), &quiet.path("app.previous")).await.unwrap();
        assert_eq!(said.get(), 1, "a swap that never waits says nothing");
    }

    #[test]
    fn a_failed_update_says_what_holds_the_folder_and_whether_the_kumi_you_had_works() {
        let kept = swap_failed(true, true, "");
        assert!(kept.contains("a Kumi window, or an antivirus scan") && kept.contains("still works") && kept.contains("run update again"));
        let gone = swap_failed(true, false, "");
        assert!(gone.contains("a Kumi window, or an antivirus scan") && !gone.contains("still works") && gone.contains("installer again"));
        assert_eq!(
            swap_failed(false, true, "Directory not empty"),
            "Couldn't put the new Kumi in place (Directory not empty), so this one stays."
        );
        assert!(swap_failed(false, false, "Directory not empty").contains("installer again"));
    }

    /// A folder another account owns (an install run with sudo) never frees up: no waiting, and the update says whose.
    #[cfg(unix)]
    #[tokio::test(start_paused = true)]
    async fn a_folder_this_account_cant_change_isnt_waited_for() {
        struct Theirs;
        impl Folders for Theirs {
            fn rename(&self, _: &str, _: &str) -> std::io::Result<()> {
                Err(std::io::Error::from_raw_os_error(libc::EACCES))
            }
            fn remove(&self, path: &str) -> std::io::Result<()> {
                remove(path)
            }
            fn waiting(&self) {
                panic!("a folder another account owns isn't waited for")
            }
        }
        let home = updating();
        let error = home.swap_with(&Theirs).await.unwrap_err();
        assert!(denied(&error));
        assert_eq!(home.version("app").as_deref(), Some("1.2"));
        let said = not_in_place(&home.path(""), &error);
        assert!(
            said.starts_with("The system refused to change ")
                && said.contains("if something in it was installed with sudo: sudo chown -R $USER"),
            "{said}"
        );
        assert!(said.ends_with(". The Kumi you had stays."), "{said}");
    }

    fn put_file(path: &str) {
        fs::create_dir_all(Path::new(path).parent().unwrap()).unwrap();
        fs::write(path, "").unwrap();
    }
}
