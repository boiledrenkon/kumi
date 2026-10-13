//! Live, the app: which one is open, asking it to quit the way its own menu does (unsaved work makes
//! it ask to save first), opening it again, and which Live is on this computer at all.

use crate::bridge_setup::{is_live_running_on, Run};
use kumi_common::abort::Signal;
use kumi_runtime::{
    library::sources::join,
    system::{self, Env, SystemProgram},
};
use regex::Regex;
use std::{path::Path, sync::LazyLock, time::Duration};

/// The Live that's open: its app, a `.app` on macOS and an `.exe` on Windows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenLive {
    pub app: String,
}

/// "/Applications/Ableton Live 12 Suite.app/Contents/MacOS/Live", as `ps` names it, is that app.
pub fn live_app_of(line: &str) -> Option<OpenLive> {
    let app = line.trim().strip_suffix("/Contents/MacOS/Live")?;
    app.ends_with(".app").then(|| OpenLive { app: app.into() })
}

fn powershell(env: &Env, platform: &str) -> String {
    system::system_program(SystemProgram::Powershell, env, platform)
}
fn args(words: &[&str]) -> Vec<String> {
    words.iter().map(|word| word.to_string()).collect()
}
/// A PowerShell single-quoted string.
fn quoted(text: &str) -> String {
    format!("'{}'", text.replace('\'', "''"))
}

/// Which Live is open, if one is.
pub async fn open_live(run: &Run, platform: &str, env: &Env) -> Option<OpenLive> {
    match platform {
        "darwin" => run("ps".into(), args(&["-axo", "comm="]), None).await.stdout.lines().find_map(live_app_of),
        "win32" => {
            let path = "Get-Process -Name 'Ableton Live*' -ErrorAction SilentlyContinue | Select-Object -First 1 -ExpandProperty Path";
            let ran = run(powershell(env, platform), args(&["-NoProfile", "-NonInteractive", "-Command", path]), None).await;
            let app = ran.stdout.trim();
            (!app.is_empty()).then(|| OpenLive { app: app.into() })
        }
        _ => None,
    }
}

/// Ask Live to quit, as its own menu does: with unsaved work it asks the producer to save first. This
/// returns once asked, not when Live has answered: the producer may still be deciding about saving. False
/// when the system didn't let Kumi ask: macOS's Automation is off for this terminal, so the producer quits
/// Live instead (asking again can't help).
pub async fn ask_to_quit(run: &Run, platform: &str, env: &Env) -> bool {
    match platform {
        "darwin" => {
            // Without `ignoring application responses`, osascript waits for Live's answer, which comes
            // only once the save dialog is answered (or after AppleScript's two-minute timeout).
            let quit = ["ignoring application responses", "tell application id \"com.ableton.live\" to quit", "end ignoring"];
            let ran = run("osascript".into(), args(&["-e", quit[0], "-e", quit[1], "-e", quit[2]]), None).await;
            ran.code == 0 || !not_allowed(&ran.stderr)
        }
        "win32" => {
            let close = "Get-Process -Name 'Ableton Live*' -ErrorAction SilentlyContinue | ForEach-Object { [void]$_.CloseMainWindow() }";
            run(powershell(env, platform), args(&["-NoProfile", "-NonInteractive", "-Command", close]), None).await;
            true
        }
        _ => true,
    }
}

/// What osascript says when macOS doesn't let it send Live an Apple Event: turned down (-1743), or it would have
/// to ask and can't (-1744).
pub fn not_allowed(said: &str) -> bool {
    said.contains("-1743") || said.contains("-1744") || said.contains("Not authorized to send Apple events")
}

/// Wait until Live has closed: true once it has, false when `stop` comes first.
pub async fn closed(run: &Run, platform: &str, env: &Env, stop: &Signal) -> bool {
    loop {
        if !is_live_running_on(run.clone(), platform, env).await {
            return true;
        }
        if stop.is_cancelled() {
            return false;
        }
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_millis(500)) => {}
            _ = stop.cancelled() => return false,
        }
    }
}

/// Open Live: the app that was open before, or else the newest one installed; with `set`, that Set in it.
pub async fn start(run: &Run, platform: &str, env: &Env, app: Option<&str>, set: Option<&str>) -> bool {
    match platform {
        "darwin" => {
            let mut target = match (app, set) {
                (Some(app), Some(_)) => args(&["-a", app]),
                (Some(app), None) => args(&[app]),
                (None, _) => args(&["-b", "com.ableton.live"]),
            };
            target.extend(set.map(str::to_string));
            run("open".into(), target, None).await.code == 0
        }
        "win32" => {
            let found = "Get-ChildItem -Path (Join-Path $env:ProgramData 'Ableton\\Live*\\Program\\Ableton Live*.exe') -ErrorAction SilentlyContinue | Sort-Object LastWriteTime -Descending | Select-Object -First 1 -ExpandProperty FullName";
            let app = app.map(quoted).unwrap_or_else(|| format!("({found})"));
            // One argument, quoted for the command line Live reads, spaces and all.
            let set = set.map(|set| format!(" -ArgumentList {}", quoted(&format!("\"{set}\"")))).unwrap_or_default();
            let script = format!("$app = {app}; if ($app) {{ Start-Process -FilePath $app{set}; 'started' }}");
            run(powershell(env, platform), args(&["-NoProfile", "-NonInteractive", "-Command", &script]), None)
                .await
                .stdout
                .contains("started")
        }
        _ => false,
    }
}

/// What of Live is on this computer, read from its files without opening it: whether it can take the bridge yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LiveInstalled {
    /// Live 12 or later has been opened here, so it has a User Library the bridge goes in.
    Ready,
    /// No Live at all.
    Missing,
    /// Only an older Live: the newest one's version ("11.3.13").
    Older(String),
    /// Live 12 is installed, but hasn't been opened yet, so it has no User Library.
    NeverOpened,
}

/// Which Live is here: from the folders Live is installed in, the settings folder each Live makes as it first
/// starts (named for its version), and the User Library the bridge goes in.
pub fn installed(env: &Env, platform: &str) -> LiveInstalled {
    // The producer said where Live's Remote Scripts are: that's Live's to have made.
    if env.get("KUMI_REMOTE_SCRIPTS_DIR").is_some_and(|dir| !dir.is_empty()) {
        return LiveInstalled::Ready;
    }
    let var = |name: &str| env.get(name).filter(|value| !value.is_empty()).cloned().unwrap_or_default();
    let places = match platform {
        "darwin" => Places {
            apps: vec!["/Applications".into(), join(&var("HOME"), "Applications")],
            settings: join(&var("HOME"), "Library/Preferences/Ableton"),
            libraries: vec![join(&var("HOME"), "Music/Ableton/User Library")],
        },
        "win32" => Places {
            apps: vec![join(&var("ProgramData"), "Ableton")],
            settings: join(&var("APPDATA"), "Ableton"),
            libraries: ["Documents", "OneDrive/Documents"]
                .map(|documents| join(&var("USERPROFILE"), &format!("{documents}/Ableton/User Library")))
                .into(),
        },
        _ => return LiveInstalled::Missing,
    };
    places.installed()
}

/// Where Live is installed, keeps its settings, and makes its User Library.
struct Places {
    apps: Vec<String>,
    settings: String,
    libraries: Vec<String>,
}
impl Places {
    fn installed(&self) -> LiveInstalled {
        static APP: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^(?:Ableton )?Live (\d+)\b").unwrap());
        static OPENED: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^Live ((\d+)[0-9A-Za-z.]*)$").unwrap());
        let names = |folder: &str| -> Vec<String> {
            std::fs::read_dir(folder)
                .into_iter()
                .flatten()
                .flatten()
                .map(|entry| entry.file_name().to_string_lossy().into_owned())
                .collect()
        };
        let installed: Vec<u32> =
            self.apps.iter().flat_map(|folder| names(folder)).filter_map(|name| APP.captures(&name)?[1].parse().ok()).collect();
        let mut opened: Vec<(u32, String)> = names(&self.settings)
            .into_iter()
            .filter_map(|name| {
                let found = OPENED.captures(&name)?;
                Some((found[2].parse().ok()?, found[1].to_string()))
            })
            .collect();
        opened.sort_by(|a, b| b.0.cmp(&a.0));
        if opened.iter().any(|(major, _)| *major >= 12) {
            return LiveInstalled::Ready;
        }
        if installed.iter().any(|major| *major >= 12) {
            return LiveInstalled::NeverOpened;
        }
        if let Some((_, version)) = opened.first() {
            return LiveInstalled::Older(version.clone());
        }
        if let Some(major) = installed.iter().max() {
            return LiveInstalled::Older(major.to_string());
        }
        // Live somewhere Kumi doesn't look, that has made its User Library where Live puts it: the bridge can go in.
        if self.libraries.iter().any(|library| Path::new(library).is_dir()) {
            return LiveInstalled::Ready;
        }
        LiveInstalled::Missing
    }
}

/// Where Live keeps its Settings, for the Control Surface step's second look.
pub fn settings_menu(platform: &str) -> Option<String> {
    match platform {
        "darwin" => Some("Settings is in Live's Live menu (⌘,).".into()),
        "win32" => Some("Settings is in Live's Options menu (Ctrl+,).".into()),
        _ => None,
    }
}

/// "12.4" from a version such as "12.4.15b5" or "12.4b15": its first two numbers.
pub fn major_minor(version: &str) -> Option<String> {
    let mut parts = version.trim().split(|c: char| !c.is_ascii_digit());
    let major = parts.next().filter(|p| !p.is_empty())?;
    let minor = parts.next().filter(|p| !p.is_empty());
    Some(minor.map(|minor| format!("{major}.{minor}")).unwrap_or_else(|| major.into()))
}

/// Live's version ("12.4"), read from its app: `app`, or else the Live that's open.
pub async fn version(run: &Run, platform: &str, env: &Env, app: Option<&str>) -> Option<String> {
    let app = match app {
        Some(app) => app.to_string(),
        None => open_live(run, platform, env).await?.app,
    };
    let read = match platform {
        "darwin" => run("defaults".into(), args(&["read", &format!("{app}/Contents/Info"), "CFBundleShortVersionString"]), None).await,
        "win32" => {
            let script = format!("(Get-Item -LiteralPath {}).VersionInfo.ProductVersion", quoted(&app));
            run(powershell(env, platform), args(&["-NoProfile", "-NonInteractive", "-Command", &script]), None).await
        }
        _ => return None,
    };
    (read.code == 0).then(|| major_minor(&read.stdout)).flatten()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bridge_setup::Ran;
    use futures::FutureExt;
    use std::{cell::RefCell, rc::Rc};

    #[test]
    fn the_open_live_is_named_by_its_app() {
        assert_eq!(
            live_app_of("/Applications/Ableton Live 12 Suite.app/Contents/MacOS/Live"),
            Some(OpenLive { app: "/Applications/Ableton Live 12 Suite.app".into() })
        );
        for other in ["/Applications/Ableton Live 12 Suite.app/Contents/Helpers/ExtensionHost/node", "/usr/bin/Live", ""] {
            assert_eq!(live_app_of(other), None, "{other}");
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn live_is_found_quit_waited_for_and_opened_again() {
        let calls = Rc::new(RefCell::new(Vec::<String>::new()));
        let running = Rc::new(RefCell::new(2u32));
        let run: Run = {
            let calls = calls.clone();
            let running = running.clone();
            Rc::new(move |command, args, _| {
                calls.borrow_mut().push(format!("{command} {}", args.join(" ")));
                let ran = match command.as_str() {
                    "ps" => Ran {
                        code: 0,
                        stdout: "/sbin/launchd\n/Applications/Ableton Live 12 Beta.app/Contents/MacOS/Live\n".into(),
                        stderr: String::new(),
                    },
                    // pgrep: running for two more looks, then closed.
                    "pgrep" => {
                        let left = *running.borrow();
                        *running.borrow_mut() = left.saturating_sub(1);
                        Ran { code: if left > 0 { 0 } else { 1 }, stdout: String::new(), stderr: String::new() }
                    }
                    _ => Ran { code: 0, stdout: String::new(), stderr: String::new() },
                };
                async move { ran }.boxed_local()
            })
        };
        let env = Env::new();
        let live = open_live(&run, "darwin", &env).await.unwrap();
        assert_eq!(live.app, "/Applications/Ableton Live 12 Beta.app");
        assert!(ask_to_quit(&run, "darwin", &env).await);
        assert!(closed(&run, "darwin", &env, &Signal::new()).await);
        assert!(start(&run, "darwin", &env, Some(&live.app), None).await);
        assert!(start(&run, "darwin", &env, None, None).await);
        {
            let calls = calls.borrow();
            assert_eq!(
                calls[1],
                "osascript -e ignoring application responses -e tell application id \"com.ableton.live\" to quit -e end ignoring"
            );
            assert_eq!(calls.iter().filter(|c| c.starts_with("pgrep")).count(), 3);
            assert_eq!(&calls[calls.len() - 2..], ["open /Applications/Ableton Live 12 Beta.app", "open -b com.ableton.live"]);
        }
        // Waiting stops when asked to.
        *running.borrow_mut() = 100;
        let stop = Signal::new();
        stop.cancel();
        assert!(!closed(&run, "darwin", &env, &stop).await);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn on_windows_live_is_found_closed_opened_and_read_through_powershell() {
        let calls = Rc::new(RefCell::new(Vec::<(String, Vec<String>)>::new()));
        let run: Run = {
            let calls = calls.clone();
            Rc::new(move |command, args, _| {
                let script = args.last().cloned().unwrap_or_default();
                calls.borrow_mut().push((command, args));
                let stdout = if script.contains("Select-Object -First 1 -ExpandProperty Path") {
                    "C:\\ProgramData\\Ableton\\Live 12 Suite\\Program\\Ableton Live 12 Suite.exe\r\n"
                } else if script.contains("Start-Process") {
                    "started\r\n"
                } else if script.contains("ProductVersion") {
                    "12.4.15.0\r\n"
                } else {
                    ""
                };
                let ran = Ran { code: 0, stdout: stdout.into(), stderr: String::new() };
                async move { ran }.boxed_local()
            })
        };
        let env = Env::from([("SystemRoot".to_string(), "C:\\Windows".to_string())]);
        let powershell = "C:\\Windows\\System32\\WindowsPowerShell\\v1.0\\powershell.exe";
        let live = open_live(&run, "win32", &env).await.unwrap();
        assert_eq!(live.app, "C:\\ProgramData\\Ableton\\Live 12 Suite\\Program\\Ableton Live 12 Suite.exe");
        assert!(ask_to_quit(&run, "win32", &env).await);
        // The same Live again, by its path; a path with a quote in it stays one string.
        assert!(start(&run, "win32", &env, Some(&live.app), None).await);
        assert!(start(&run, "win32", &env, Some("C:\\Live's\\Ableton Live 12.exe"), None).await);
        // Without one, the newest Live under ProgramData.
        assert!(start(&run, "win32", &env, None, None).await);
        assert_eq!(version(&run, "win32", &env, Some(&live.app)).await.as_deref(), Some("12.4"));
        let calls = calls.borrow();
        for (command, args) in calls.iter() {
            assert_eq!(command, powershell);
            assert_eq!(&args[..3], ["-NoProfile", "-NonInteractive", "-Command"]);
        }
        let scripts: Vec<&str> = calls.iter().map(|(_, args)| args[3].as_str()).collect();
        assert_eq!(
            scripts[0],
            "Get-Process -Name 'Ableton Live*' -ErrorAction SilentlyContinue | Select-Object -First 1 -ExpandProperty Path"
        );
        assert_eq!(
            scripts[1],
            "Get-Process -Name 'Ableton Live*' -ErrorAction SilentlyContinue | ForEach-Object { [void]$_.CloseMainWindow() }"
        );
        assert_eq!(
            scripts[2],
            "$app = 'C:\\ProgramData\\Ableton\\Live 12 Suite\\Program\\Ableton Live 12 Suite.exe'; if ($app) { Start-Process -FilePath $app; 'started' }"
        );
        assert_eq!(scripts[3], "$app = 'C:\\Live''s\\Ableton Live 12.exe'; if ($app) { Start-Process -FilePath $app; 'started' }");
        assert_eq!(
            scripts[4],
            "$app = (Get-ChildItem -Path (Join-Path $env:ProgramData 'Ableton\\Live*\\Program\\Ableton Live*.exe') -ErrorAction SilentlyContinue | Sort-Object LastWriteTime -Descending | Select-Object -First 1 -ExpandProperty FullName); if ($app) { Start-Process -FilePath $app; 'started' }"
        );
        assert_eq!(scripts[5], "(Get-Item -LiteralPath 'C:\\ProgramData\\Ableton\\Live 12 Suite\\Program\\Ableton Live 12 Suite.exe').VersionInfo.ProductVersion");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn on_windows_live_not_found_or_not_started_says_so() {
        let run: Run = Rc::new(|_, _, _| async { Ran { code: 0, stdout: String::new(), stderr: String::new() } }.boxed_local());
        let env = Env::new();
        assert_eq!(open_live(&run, "win32", &env).await, None);
        assert!(!start(&run, "win32", &env, None, None).await, "no Live under ProgramData: nothing started");
        assert_eq!(version(&run, "win32", &env, Some("C:\\x.exe")).await, None);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn lives_version_is_read_from_its_app() {
        for (raw, want) in [("12.4.15b5", Some("12.4")), ("12.4b15", Some("12.4")), ("12", Some("12")), ("", None), ("beta", None)] {
            assert_eq!(major_minor(raw).as_deref(), want, "{raw}");
        }
        let calls = Rc::new(RefCell::new(Vec::<String>::new()));
        let run: Run = {
            let calls = calls.clone();
            Rc::new(move |command, args, _| {
                calls.borrow_mut().push(format!("{command} {}", args.join(" ")));
                let stdout = match command.as_str() {
                    "ps" => "/Applications/Ableton Live 12 Beta.app/Contents/MacOS/Live\n",
                    "defaults" => "12.4b15\n",
                    _ => "",
                };
                let ran = Ran { code: 0, stdout: stdout.into(), stderr: String::new() };
                async move { ran }.boxed_local()
            })
        };
        let env = Env::new();
        assert_eq!(version(&run, "darwin", &env, None).await.as_deref(), Some("12.4"));
        assert_eq!(calls.borrow()[1], "defaults read /Applications/Ableton Live 12 Beta.app/Contents/Info CFBundleShortVersionString");
        assert_eq!(version(&run, "linux", &env, Some("/x")).await, None);
    }

    /// What osascript and `open` did, and what osascript answers.
    fn darwin(said: Ran) -> (Run, Rc<RefCell<Vec<String>>>) {
        let calls = Rc::new(RefCell::new(Vec::<String>::new()));
        let run: Run = {
            let calls = calls.clone();
            Rc::new(move |command, args, _| {
                calls.borrow_mut().push(format!("{command} {}", args.join(" ")));
                let ran = if command == "osascript" { said.clone() } else { Ran::default() };
                async move { ran }.boxed_local()
            })
        };
        (run, calls)
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_mac_that_doesnt_let_kumi_ask_live_to_quit_is_told_apart() {
        let env = Env::new();
        let refused = |code: &str| Ran {
            code: 1,
            stdout: String::new(),
            stderr: format!("29:63: execution error: Not authorized to send Apple events to Ableton Live 12 Suite. ({code})\n"),
        };
        for code in ["-1743", "-1744"] {
            let (run, calls) = darwin(refused(code));
            assert!(!ask_to_quit(&run, "darwin", &env).await, "{code}");
            assert_eq!(calls.borrow().len(), 1, "asked once, never again by itself");
        }
        // Asked: osascript is done once the request is sent. Any other trouble (Live already gone, say) isn't the
        // system's refusal, so Kumi watches for Live to close as it always has.
        let (run, _) = darwin(Ran::default());
        assert!(ask_to_quit(&run, "darwin", &env).await);
        let (run, _) = darwin(Ran { code: 1, stdout: String::new(), stderr: "execution error: Application isn't running. (-600)".into() });
        assert!(ask_to_quit(&run, "darwin", &env).await);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn live_opens_again_with_the_set_it_had_open() {
        let env = Env::new();
        let (run, calls) = darwin(Ran::default());
        let set = "/Users/me/Music/Night Drive Project/Night Drive.als";
        assert!(start(&run, "darwin", &env, Some("/Applications/Ableton Live 12 Suite.app"), Some(set)).await);
        assert!(start(&run, "darwin", &env, None, Some(set)).await);
        assert_eq!(
            *calls.borrow(),
            [format!("open -a /Applications/Ableton Live 12 Suite.app {set}"), format!("open -b com.ableton.live {set}"),]
        );
        let scripts = Rc::new(RefCell::new(Vec::<String>::new()));
        let run: Run = {
            let scripts = scripts.clone();
            Rc::new(move |_, args, _| {
                scripts.borrow_mut().push(args.last().cloned().unwrap_or_default());
                async { Ran { code: 0, stdout: "started\r\n".into(), stderr: String::new() } }.boxed_local()
            })
        };
        let app = "C:\\ProgramData\\Ableton\\Live 12 Suite\\Program\\Ableton Live 12 Suite.exe";
        assert!(start(&run, "win32", &env, Some(app), Some("C:\\Users\\me\\Rock 'n' Roll.als")).await);
        assert_eq!(
            scripts.borrow()[0],
            format!("$app = '{app}'; if ($app) {{ Start-Process -FilePath $app -ArgumentList '\"C:\\Users\\me\\Rock ''n'' Roll.als\"'; 'started' }}")
        );
    }

    #[test]
    fn which_live_is_here_is_read_from_its_folders() {
        let root = tempfile::tempdir().unwrap();
        let path = |name: &str| root.path().join(name).to_string_lossy().into_owned();
        let places = Places { apps: vec![path("Applications")], settings: path("Preferences"), libraries: vec![path("User Library")] };
        let make = |name: &str| std::fs::create_dir_all(root.path().join(name)).unwrap();
        assert_eq!(places.installed(), LiveInstalled::Missing, "nothing here");
        make("Applications/Ableton Live 11 Suite.app");
        assert_eq!(places.installed(), LiveInstalled::Older("11".into()), "installed, never opened");
        make("Preferences/Live 11.3.13");
        assert_eq!(places.installed(), LiveInstalled::Older("11.3.13".into()), "opened: its whole version");
        make("Applications/Ableton Live 12 Beta.app");
        assert_eq!(places.installed(), LiveInstalled::NeverOpened, "Live 12 is here, and hasn't made its User Library");
        make("Preferences/Live 12.4.15b6");
        assert_eq!(places.installed(), LiveInstalled::Ready);
        // Live installed where Kumi doesn't look, that has made its User Library.
        let elsewhere = Places { apps: vec![path("none")], settings: path("none"), libraries: vec![path("Library")] };
        make("Library");
        assert_eq!(elsewhere.installed(), LiveInstalled::Ready);
        // On Windows: ProgramData's "Live 12 Suite", AppData's "Live 12.1.5".
        let windows = Places { apps: vec![path("ProgramData")], settings: path("AppData"), libraries: vec![] };
        make("ProgramData/Live 12 Suite/Program");
        assert_eq!(windows.installed(), LiveInstalled::NeverOpened);
        make("AppData/Live 12.1.5");
        make("AppData/Live Reports");
        assert_eq!(windows.installed(), LiveInstalled::Ready);
        // Remote Scripts named by the producer, and Linux, where Live doesn't run.
        let named = Env::from([("KUMI_REMOTE_SCRIPTS_DIR".to_string(), path("Remote Scripts"))]);
        assert_eq!(installed(&named, "darwin"), LiveInstalled::Ready);
        assert_eq!(installed(&Env::new(), "linux"), LiveInstalled::Missing);
        assert_eq!(settings_menu("darwin").as_deref(), Some("Settings is in Live's Live menu (⌘,)."));
        assert_eq!(settings_menu("linux"), None);
    }
}
