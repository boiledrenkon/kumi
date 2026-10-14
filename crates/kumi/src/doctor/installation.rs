//! `kumi doctor`'s Installation section: this Kumi and where it is, what `update --rollback` goes back to, how
//! the shell finds `kumi`, whether a newer Kumi is out, free space, and what Live needs from this computer.
//! Files and the system are read; nothing goes over the network: whether a newer Kumi is out comes from
//! the daily check's note, and the bridge's ports are tried on this computer (127.0.0.1) only.
use super::{tilde, Check};
use crate::{
    bridge_setup::{default_run, is_live_running, repository_dir, AsyncBool},
    config::{find_bridge_config, kumi_dir, live_user_library, load_settings_file, read_settings, remote_scripts_dir},
    install::{app_version, chown_line, has_app, not_yours, startup_files, PATH_MARKER, UPDATE_SPACE},
    live_app::{installed_lives, installed_with, runs_on, InstalledLive, LiveInstalled},
    live_extension::{extension_data_dir, installed_extension, live_extensions_dir, running_extension},
    update::newer,
};
use futures::{
    future::{join_all, LocalBoxFuture},
    FutureExt,
};
use kumi_common::time::{now_ms, since};
use kumi_runtime::{
    core::disk::{free_bytes, size},
    library::sources::{dirname, join},
    system::{self, Env},
    KUMI, KUMI_VERSION,
};
use serde_json::Value;
use std::{path::Path, rc::Rc, time::Duration};

/// Who listens on one of this computer's ports: nobody (None), or a program, named when the system says.
pub type PortOwner = Option<Option<String>>;

/// What the Installation section reads from the system; tests answer it.
#[derive(Clone, Default)]
pub struct InstallationIo {
    pub platform: Option<String>,
    pub lives: Option<Rc<dyn Fn() -> Vec<InstalledLive>>>,
    pub free: Option<Rc<dyn Fn(String) -> LocalBoxFuture<'static, Option<f64>>>>,
    pub port: Option<Rc<dyn Fn(u16) -> LocalBoxFuture<'static, PortOwner>>>,
    pub live_running: Option<AsyncBool>,
    /// Whether Live runs its own Extension Host, which it does with Developer Mode off.
    pub live_extension_host: Option<AsyncBool>,
    pub now: Option<f64>,
}

/// The `kumi` programs on `path`, in the order a shell finds them.
pub fn kumi_on_path(path: &str, windows: bool, extensions: &str) -> Vec<String> {
    let mut found = vec![];
    for folder in path.split(if windows { ';' } else { ':' }).filter(|folder| !folder.is_empty()) {
        if windows {
            for extension in extensions.split(';').filter(|e| !e.is_empty()) {
                let file = join(folder, &format!("kumi{}", extension.to_ascii_lowercase()));
                if Path::new(&file).is_file() {
                    found.push(file);
                    break;
                }
            }
            continue;
        }
        let file = join(folder, "kumi");
        #[cfg(unix)]
        let runs = {
            use std::os::unix::fs::PermissionsExt;
            std::fs::metadata(&file).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        };
        #[cfg(not(unix))]
        let runs = Path::new(&file).is_file();
        if runs {
            found.push(file);
        }
    }
    found
}

/// Whether `file` is Kumi's own launcher, or anything else in Kumi's folder (a link to it counts).
fn kumi_s_own(file: &str, home: &str) -> bool {
    let real = |path: &str| std::fs::canonicalize(path).unwrap_or_else(|_| path.into());
    real(file).starts_with(real(home))
}

fn path_check(env: &Env, home: &str, windows: bool) -> Check {
    let bin = join(home, "bin");
    let found = kumi_on_path(
        env.get("PATH").or_else(|| env.get("Path")).map(String::as_str).unwrap_or(""),
        windows,
        env.get("PATHEXT").map(String::as_str).unwrap_or(".COM;.EXE;.BAT;.CMD"),
    );
    match found.first() {
        Some(first) if kumi_s_own(first, home) => Check::ok(format!("kumi on PATH: {}", tilde(first))),
        Some(first) => Check::fix(
            format!("Another kumi comes first on PATH: {}", tilde(first)),
            Some(format!("Remove it, or put {} before {} in PATH", tilde(&bin), tilde(&dirname(first)))),
        ),
        None if windows => Check::note(
            "kumi isn't on PATH in this window",
            Some(format!("Open a new terminal window; the installer added {} to PATH for new ones", tilde(&bin))),
        ),
        None => {
            let launcher = join(&bin, "kumi");
            let added =
                startup_files(env).into_iter().find(|file| std::fs::read_to_string(file).is_ok_and(|text| text.contains(PATH_MARKER)));
            Check::note(
                "kumi isn't on PATH in this window",
                Some(match added {
                    Some(file) => format!("Open a new terminal window ({} adds it there); until then: {}", tilde(&file), tilde(&launcher)),
                    None => format!("Add {} to PATH in your shell's startup file, or run {}", tilde(&bin), tilde(&launcher)),
                }),
            )
        }
    }
}

fn rollback_check(home: &str) -> Check {
    let previous = join(home, "app.previous");
    match app_version(&previous) {
        Some(version) if version == KUMI_VERSION => {
            Check::ok(format!("{} update --rollback has only this same {version} to go back to (kept by a reinstall)", *KUMI))
        }
        Some(version) => Check::ok(format!("{} update --rollback goes back to {version}", *KUMI)),
        None if has_app(&previous) => Check::ok(format!("{} update --rollback goes back to the Kumi before", *KUMI)),
        None => Check::ok(format!("Nothing kept to go back to yet: the next update keeps {KUMI_VERSION} for {} update --rollback", *KUMI)),
    }
}

fn update_check(env: &Env, now: f64) -> Check {
    let settings = load_settings_file(env).ok();
    let off = if env.get("KUMI_NO_UPDATE_CHECK").is_some_and(|s| !s.is_empty()) {
        Some("KUMI_NO_UPDATE_CHECK")
    } else if settings.as_deref().is_some_and(|file| read_settings(file).update_check == Some(false)) {
        Some("updateCheck in settings.json")
    } else {
        None
    };
    let checked = settings
        .map(|file| join(&dirname(&file), "update-check.json"))
        .and_then(|file| serde_json::from_slice::<Value>(&std::fs::read(file).ok()?).ok())
        .and_then(|cached| Some((cached.get("latest")?.as_str()?.to_string(), cached.get("checkedAt")?.as_f64()?)));
    match (off, checked) {
        (Some(why), _) => Check::ok(format!("Kumi doesn't look for updates by itself ({why}); {} update --check asks now", *KUMI)),
        (None, Some((latest, _))) if newer(&latest, KUMI_VERSION) => {
            Check::note(format!("Kumi {latest} is out (this is {KUMI_VERSION})"), Some(format!("{} update", *KUMI)))
        }
        (None, Some((_, at))) => Check::ok(format!("Up to date when Kumi last looked ({})", since(at, now))),
        (None, None) => Check::ok(format!("Kumi looks for updates once a day while it runs; {} update --check asks now", *KUMI)),
    }
}

fn space_check(free: Option<f64>, home: &str) -> Option<Check> {
    let free = free?;
    Some(if free < UPDATE_SPACE {
        Check::fix(
            format!("Only {} free on the disk with {}", size(free), tilde(home)),
            Some("Free some space there: an update needs about 250 MB, and Kumi's conversations and notes need room too".into()),
        )
    } else {
        Check::ok(format!("{} free on the disk with {}", size(free), tilde(home)))
    })
}

/// The Lives installed here and, once one Kumi works with can take the bridge (setup's own verdict,
/// `live_app::installed`), its User Library. No such Live (none, an older one, one never opened) is said with its
/// next step by the bridge's check below, so here it's information, never a fix: Kumi chats without Live, and a
/// computer without it (a build machine, say) isn't broken.
fn live_checks(env: &Env, platform: &str, lives: &[InstalledLive], running: bool) -> Vec<Check> {
    if !runs_on(platform) {
        return vec![];
    }
    let ready = installed_with(env, platform, lives) == LiveInstalled::Ready || running;
    let mut checks = vec![if !lives.is_empty() {
        let named: Vec<_> =
            lives.iter().map(|live| format!("{} ({})", live.version.as_deref().unwrap_or("version unknown"), live.name)).collect();
        Check::ok(format!("Live installed: {}", named.join(", ")))
    } else if ready {
        Check::ok("Live has been opened on this account (its app isn't in the usual folder)")
    } else {
        Check::note("Live isn't installed here", None)
    }];
    if !ready {
        return checks;
    }
    if let Some(folder) = env.get("KUMI_REMOTE_SCRIPTS_DIR").filter(|s| !s.is_empty()) {
        checks.push(if Path::new(folder).is_dir() {
            Check::ok(format!("Remote Scripts folder: {} (KUMI_REMOTE_SCRIPTS_DIR)", tilde(folder)))
        } else {
            Check::fix(
                format!("KUMI_REMOTE_SCRIPTS_DIR names {}, which isn't there", tilde(folder)),
                Some("Make that folder, or unset KUMI_REMOTE_SCRIPTS_DIR".into()),
            )
        });
        return checks;
    }
    let library = live_user_library(env);
    let found = library.clone().unwrap_or_else(|| dirname(&remote_scripts_dir(env)));
    if !Path::new(&found).is_dir() {
        checks.push(match library {
            Some(library) => Check::fix(
                format!("Live's User Library is set to {}, which isn't there", tilde(&library)),
                Some("Connect the drive it's on, or choose it again in Live's Settings → Library".into()),
            ),
            None => Check::note(
                "Kumi can't find Live's User Library",
                Some("Live's Settings → Library shows where it is: set KUMI_REMOTE_SCRIPTS_DIR to the Remote Scripts folder in it".into()),
            ),
        });
        return checks;
    }
    checks.push(Check::ok(format!("User Library: {}", tilde(&found))));
    if platform == "win32" && found.to_ascii_lowercase().contains("onedrive") {
        checks.push(Check::note(
            "Live's User Library is in OneDrive, which can keep files online only",
            Some("In File Explorer, right-click Documents\\Ableton and choose Always keep on this device".into()),
        ));
    }
    checks
}

/// The bridge's ports: its configuration's, or the ones it installs with.
fn bridge_ports(env: &Env) -> Vec<u16> {
    let config = find_bridge_config(env).and_then(|file| serde_json::from_slice::<Value>(&std::fs::read(file).ok()?).ok());
    let port = |key: &str, default: u16| {
        config.as_ref().and_then(|c| c["bridge"][key].as_u64()).and_then(|p| u16::try_from(p).ok()).unwrap_or(default)
    };
    let mut ports = vec![port("port", 9765), port("realtimePort", 9766)];
    ports.dedup();
    ports
}

/// Whether something listens on `port` of this computer, by connecting to it (never by listening), and if so,
/// which program, when the system says.
pub async fn port_owner(port: u16, platform: &str) -> PortOwner {
    let connected = tokio::time::timeout(Duration::from_millis(400), tokio::net::TcpStream::connect(("127.0.0.1", port))).await;
    if !connected.is_ok_and(|c| c.is_ok()) {
        return None;
    }
    let run = default_run();
    // A lookup that hangs (a stuck lsof, a slow PowerShell) leaves the owner unnamed rather than the doctor waiting.
    let run = move |program: String, args: Vec<String>| {
        let run = run.clone();
        async move { tokio::time::timeout(Duration::from_secs(2), run(program, args, None)).await.unwrap_or_default() }
    };
    let named = match platform {
        "darwin" | "linux" => {
            let lsof = ["/usr/sbin/lsof", "/usr/bin/lsof"].into_iter().find(|p| Path::new(p).exists()).unwrap_or("lsof");
            let args = ["+c", "0", "-nP", &format!("-iTCP:{port}"), "-sTCP:LISTEN", "-Fc"];
            let ran = run(lsof.into(), args.map(str::to_string).into()).await;
            ran.stdout.lines().find_map(|line| line.strip_prefix('c')).map(str::to_string)
        }
        "win32" => {
            let script = format!(
                "Get-NetTCPConnection -State Listen -LocalPort {port} -ErrorAction SilentlyContinue | Select-Object -First 1 -ExpandProperty OwningProcess | ForEach-Object {{ (Get-Process -Id $_ -ErrorAction SilentlyContinue).ProcessName }}"
            );
            let powershell = system::system_program_default(system::SystemProgram::Powershell);
            let ran = run(powershell, vec!["-NoProfile".into(), "-NonInteractive".into(), "-Command".into(), script]).await;
            Some(ran.stdout.trim().to_string()).filter(|s| !s.is_empty())
        }
        _ => None,
    };
    Some(named)
}

fn is_live(program: &str) -> bool {
    program == "Live" || program.starts_with("Ableton Live")
}

/// The bridge's ports: free, or held by Live's bridge while Live is open, are both fine; another program
/// holding one keeps the bridge from starting.
fn ports_check(held: &[(u16, PortOwner)], live_running: bool) -> Check {
    let ports = |list: &[u16]| match list {
        [one] => format!("port {one}"),
        many => format!("ports {}", many.iter().map(u16::to_string).collect::<Vec<_>>().join(" and ")),
    };
    let taken: Vec<_> = held.iter().filter_map(|(port, owner)| owner.as_ref().map(|name| (*port, name.clone()))).collect();
    if taken.is_empty() {
        let all: Vec<_> = held.iter().map(|(port, _)| *port).collect();
        return Check::ok(format!("{} free for Live's bridge", capitalized(&ports(&all))));
    }
    let other = taken.iter().find(|(_, name)| name.as_deref().is_some_and(|name| !is_live(name)) || (name.is_none() && !live_running));
    match other {
        Some((port, Some(name))) => {
            Check::fix(format!("{name} holds port {port}, which Live's bridge needs"), Some(format!("Quit {name}, then restart Live")))
        }
        Some((port, None)) => Check::fix(
            format!("Another program holds port {port}, which Live's bridge needs"),
            Some(format!(
                "Quit it, then restart Live ({} names it)",
                if cfg!(windows) { format!("netstat -ano | findstr :{port}") } else { format!("lsof -nP -iTCP:{port} -sTCP:LISTEN") }
            )),
        ),
        None => Check::ok(format!("Live's bridge has {} (Live is open)", ports(&taken.iter().map(|(port, _)| *port).collect::<Vec<_>>()))),
    }
}

fn capitalized(text: &str) -> String {
    let mut chars = text.chars();
    chars.next().map(|first| first.to_uppercase().chain(chars).collect()).unwrap_or_default()
}

/// Live's Developer Mode, when it shows: who started Kumi's extension, or, with Live open, whether Live runs
/// its own Extension Host (it doesn't with Developer Mode on). None while it can't be told.
async fn developer_mode(env: &Env, platform: &str, io: &InstallationIo, live_running: bool) -> Option<Check> {
    let extensions = live_extensions_dir(env, platform)?;
    let off = || Check::ok("Live's Developer Mode is off: Live starts Kumi's extension when it opens");
    let on = || Check::ok("Live's Developer Mode is on: Kumi's bridge starts Kumi's extension while Kumi runs");
    if running_extension(&extension_data_dir(&extensions)).is_some() {
        return Some(off());
    }
    let config = find_bridge_config(env);
    if config.as_ref().is_some_and(|config| running_extension(&join(&dirname(config), "live-extension")).is_some()) {
        return Some(on());
    }
    if !live_running || installed_extension(&extensions).is_none() {
        return None;
    }
    let own_host = match &io.live_extension_host {
        Some(host) => host().await,
        None => ableton_mcp_server::bridge::extension_launcher::running_extension_hosts().await.live,
    };
    Some(if own_host { off() } else { on() })
}

pub async fn installation_checks(env: &Env, io: &InstallationIo) -> Vec<Check> {
    let platform = io.platform.clone().unwrap_or_else(|| system::platform().into());
    let home = kumi_dir(env);
    let installed = env.get("KUMI_INSTALLED").is_some_and(|s| s == "1");
    let mut checks = vec![];
    if installed {
        checks.push(Check::ok(format!("Kumi {KUMI_VERSION} in {}", tilde(&home))));
        checks.push(rollback_check(&home));
        // PATH is this system's, whatever platform the Live checks are told.
        checks.push(path_check(env, &home, cfg!(windows)));
    } else {
        // A checkout's build, or Kumi's own executable started without its launcher.
        let repository = repository_dir();
        checks.push(Check::ok(if Path::new(&join(&repository, "Cargo.toml")).is_file() {
            format!("Kumi {KUMI_VERSION}, built in the checkout at {}", tilde(&repository))
        } else {
            let exe = std::env::current_exe().map(|exe| exe.display().to_string()).unwrap_or_default();
            format!("Kumi {KUMI_VERSION} at {}", tilde(&exe))
        }));
    }
    checks.push(update_check(env, io.now.unwrap_or_else(|| now_ms() as f64)));
    if let Some(folder) = not_yours(&home) {
        checks.push(Check::fix(format!("{} belongs to another account (installed with sudo?)", tilde(&folder)), Some(chown_line(&home))));
    }
    let free = match &io.free {
        Some(free) => free(home.clone()).await,
        None => free_bytes(Path::new(&home)).await,
    };
    checks.extend(space_check(free, &home));
    // Where Live doesn't run (Linux), nothing below applies; the bridge's check says Kumi chats without Live.
    if !runs_on(&platform) {
        return checks;
    }
    let lives = match &io.lives {
        Some(lives) => lives(),
        None => installed_lives(env, &platform),
    };
    let ports = bridge_ports(env);
    let (held, live_running) = futures::join!(
        join_all(ports.iter().map(|port| {
            let port = *port;
            let platform = platform.clone();
            match &io.port {
                Some(owner) => owner(port),
                None => async move { port_owner(port, &platform).await }.boxed_local(),
            }
            .map(move |owner| (port, owner))
        })),
        async {
            match &io.live_running {
                Some(running) => running().await,
                None => is_live_running(default_run()).await,
            }
        }
    );
    checks.extend(live_checks(env, &platform, &lives, live_running));
    checks.push(ports_check(&held, live_running));
    checks.extend(developer_mode(env, &platform, io, live_running).await);
    checks
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ports_held_by_live_are_fine_and_by_another_program_need_fixing() {
        assert_eq!(ports_check(&[(9765, None), (9766, None)], false).text, "Ports 9765 and 9766 free for Live's bridge");
        assert_eq!(ports_check(&[(9765, None), (9766, None)], true).status, crate::doctor::CheckStatus::Ok);
        let live = ports_check(&[(9765, Some(Some("Live".into()))), (9766, None)], true);
        assert_eq!(live, Check::ok("Live's bridge has port 9765 (Live is open)"));
        let windows =
            ports_check(&[(9765, Some(Some("Ableton Live 12 Suite".into()))), (9766, Some(Some("Ableton Live 12 Suite".into())))], true);
        assert_eq!(windows.text, "Live's bridge has ports 9765 and 9766 (Live is open)");
        // Unnamed while Live runs: Live's. Unnamed with Live closed: another program's.
        assert_eq!(ports_check(&[(9765, Some(None))], true).status, crate::doctor::CheckStatus::Ok);
        let unnamed = ports_check(&[(9765, Some(None))], false);
        assert_eq!(unnamed.status, crate::doctor::CheckStatus::Fix);
        assert!(unnamed.next.unwrap().starts_with("Quit it, then restart Live"));
        let other = ports_check(&[(9765, Some(Some("Live".into()))), (9766, Some(Some("node".into())))], true);
        assert_eq!(other, Check::fix("node holds port 9766, which Live's bridge needs", Some("Quit node, then restart Live".into())));
    }
}
