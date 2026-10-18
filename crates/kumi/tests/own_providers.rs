//! REAPER's and Renoise's providers: Kumi's bridge goes into the DAW's own folder through the shared lifecycle, is
//! found again (and chosen when it's the only bridge installed), and comes out; the doctor says nothing of a DAW this
//! user has never used.
#[path = "support/live_words.rs"]
mod live_words;
use kumi::daw::{chosen, own, DawProvider, RemovedBridge};
use kumi_runtime::{core::daw::DawId, system::Env};
use std::rc::Rc;

#[derive(Default)]
struct Out(std::cell::RefCell<String>);
impl kumi::tui::tty::TtyOutput for Out {
    fn is_tty(&self) -> bool {
        false
    }
    fn columns(&self) -> Option<i32> {
        Some(80)
    }
    fn rows(&self) -> Option<i32> {
        Some(24)
    }
    fn write(&self, s: &str) {
        self.0.borrow_mut().push_str(s);
    }
}

/// A home with Renoise 3.5.4's preferences folder (as Renoise makes on its first run), and Kumi's folder in it.
fn renoise_home() -> (tempfile::TempDir, Env) {
    let dir = tempfile::tempdir().unwrap();
    let prefs = dir.path().join("Renoise");
    std::fs::create_dir_all(prefs.join("V3.5.4").join("Scripts").join("Tools")).unwrap();
    let env = Env::from([
        ("HOME".into(), dir.path().to_string_lossy().into_owned()),
        ("KUMI_HOME".into(), dir.path().join(".kumi").to_string_lossy().into_owned()),
        ("KUMI_RENOISE_PREFS_DIR".into(), prefs.to_string_lossy().into_owned()),
        ("KUMI_SETTINGS_FILE".into(), dir.path().join("settings.json").to_string_lossy().into_owned()),
        ("KUMI_REMOTE_SCRIPTS_DIR".into(), dir.path().join("Remote Scripts").to_string_lossy().into_owned()),
    ]);
    (dir, env)
}

/// `kumi --daw <daw> bridge --yes`.
fn yes() -> kumi::daw::BridgeCommand {
    kumi::daw::BridgeCommand { yes: true, ..Default::default() }
}

#[tokio::test(flavor = "current_thread")]
async fn renoises_tool_goes_in_is_found_and_chosen_and_comes_out() {
    let (dir, env) = renoise_home();
    let renoise: &dyn DawProvider = &own::RENOISE;
    assert!(!renoise.bridge_installed(&env));
    assert_eq!(chosen(None, &env).0.id(), DawId::Ableton, "nothing installed: Live's words");

    // Asked first: with nobody to answer (no --yes, no terminal), nothing goes in Renoise's folder.
    let out = Rc::new(Out::default());
    assert_eq!(renoise.setup_bridge(out.clone(), &env, Default::default()).await.unwrap(), 1);
    let said = out.0.borrow().clone();
    assert!(said.contains("Put Kumi's tool in Renoise (") && said.contains("add --yes"), "{said}");
    assert!(said.contains("Nothing was changed. To put it in later: ") && said.contains(" --daw renoise bridge\n"), "{said}");
    assert!(!renoise.bridge_installed(&env) && !dir.path().join("Renoise/V3.5.4/Scripts/Tools/com.kumi.Bridge.xrnx").exists());

    let out = Rc::new(Out::default());
    let code = renoise.setup_bridge(out.clone(), &env, yes()).await.unwrap();
    assert_eq!(code, 0, "{}", out.0.borrow());
    assert!(out.0.borrow().contains("Kumi's tool 1.0.0 is in Renoise"), "{}", out.0.borrow());
    let tool = dir.path().join("Renoise/V3.5.4/Scripts/Tools/com.kumi.Bridge.xrnx");
    assert!(tool.join("main.lua").exists() && tool.join("bridge-reference.json").exists());
    assert!(renoise.bridge_installed(&env));
    let config = renoise.bridge_config(&env).expect("the reference, once installed");
    assert!(config.ends_with("bridge-reference.json"), "{config}");
    let (provider, found) = chosen(None, &env);
    assert_eq!((provider.id(), found.as_deref()), (DawId::Renoise, Some(config.as_str())), "the only bridge installed is chosen");

    let io = kumi::install::InstalledIo::new(out.clone(), env.clone());
    let run: kumi::process::Run = Rc::new(|_, _, _| panic!("removing the tool runs no program"));
    assert_eq!(renoise.remove_bridge(&io, run).await.unwrap(), RemovedBridge::Removed);
    assert!(!tool.exists());
    assert!(!renoise.bridge_installed(&env));
}

#[tokio::test(flavor = "current_thread")]
async fn the_doctor_says_nothing_of_a_daw_this_user_never_used() {
    let dir = tempfile::tempdir().unwrap();
    let env = Env::from([
        ("HOME".into(), dir.path().to_string_lossy().into_owned()),
        ("KUMI_HOME".into(), dir.path().join(".kumi").to_string_lossy().into_owned()),
        ("KUMI_RENOISE_PREFS_DIR".into(), dir.path().join("Renoise").to_string_lossy().into_owned()),
        ("KUMI_REAPER_RESOURCE_DIR".into(), dir.path().join("REAPER").to_string_lossy().into_owned()),
    ]);
    let io = kumi::doctor::InstallationIo::default();
    for provider in [&own::REAPER as &dyn DawProvider, &own::RENOISE] {
        assert!(provider.installation_checks(&env, &io, "darwin").await.is_empty(), "{:?}", provider.id());
    }
    let (_renoise_dir, renoise_env) = renoise_home();
    let checks = own::RENOISE.installation_checks(&renoise_env, &io, "darwin").await;
    assert_eq!(checks.len(), 1);
}

/// With several DAWs' bridges installed and none asked for: the one running now, else the one used last, else Live,
/// else the first. A producer with Live's bridge alone starts in Live, whatever runs.
#[tokio::test(flavor = "current_thread")]
async fn among_several_bridges_the_running_daw_then_the_last_used_is_chosen() {
    use kumi::daw::{chosen_with, remember_used};
    let (dir, mut env) = renoise_home();
    let out = Rc::new(Out::default());
    assert_eq!(own::RENOISE.setup_bridge(out.clone(), &env, yes()).await.unwrap(), 0, "{}", out.0.borrow());
    // Kumi's extension in REAPER too, as its receipt says.
    let resource = dir.path().join("REAPER");
    env.insert("KUMI_REAPER_RESOURCE_DIR".into(), resource.to_string_lossy().into_owned());
    let paths = daw_bridge::paths::BridgePaths::new(&dir.path().join(".kumi"), daw_bridge::daw::Daw::Reaper);
    std::fs::create_dir_all(&paths.root).unwrap();
    let receipt = daw_bridge::lifecycle::Receipt {
        schema: daw_bridge::lifecycle::RECEIPT_SCHEMA.into(),
        daw: "reaper".into(),
        status: "installed".into(),
        target: resource.join("UserPlugins/reaper_kumi.dylib").to_string_lossy().into_owned(),
        reference: resource.join("UserPlugins/kumi-bridge-reference.json").to_string_lossy().into_owned(),
        generation: Some("g1".into()),
        version: Some("0.1.0".into()),
        sha256: None,
        previous: vec![],
        updated_ms: 0,
        history: vec![],
    };
    std::fs::write(paths.receipt(), serde_json::to_string(&receipt).unwrap()).unwrap();
    assert!(own::REAPER.bridge_installed(&env));
    let only = |id: DawId| move |provider: &dyn DawProvider| provider.id() == id;
    assert_eq!(chosen_with(None, &env, &only(DawId::Renoise)).0.id(), DawId::Renoise, "the one running");
    assert_eq!(chosen_with(None, &env, &only(DawId::Reaper)).0.id(), DawId::Reaper, "the one running");
    let none = |_: &dyn DawProvider| false;
    assert_eq!(chosen_with(None, &env, &none).0.id(), DawId::Reaper, "none running, none used: the first installed");
    remember_used(DawId::Renoise, &env);
    assert_eq!(chosen_with(None, &env, &none).0.id(), DawId::Renoise, "none running: the one used last");
    let both = |provider: &dyn DawProvider| provider.id() != DawId::Ableton;
    assert_eq!(chosen_with(None, &env, &both).0.id(), DawId::Renoise, "both running: the one used last");
    assert_eq!(chosen_with(Some(DawId::Reaper), &env, &none).0.id(), DawId::Reaper, "asked for, always");
    // Both running and none asked for: the session says which one it works with, and how to switch.
    use kumi::daw::{also_running, also_running_words};
    let others = also_running(DawId::Renoise, &env, &both);
    assert_eq!(others.iter().map(|provider| provider.id()).collect::<Vec<_>>(), vec![DawId::Reaper]);
    assert_eq!(
        also_running_words(DawId::Renoise, &others, &env).as_deref(),
        Some("REAPER is open too: Kumi works with Renoise, the one you used last. /daw reaper switches to REAPER.")
    );
    assert!(also_running(DawId::Renoise, &env, &only(DawId::Renoise)).is_empty(), "alone: nothing to say");
}

/// `/daw` says which DAW Kumi works with, and takes the DAW by the producer's name for it ("live") or Kumi's id.
#[test]
fn daw_says_which_daw_kumi_works_with_and_switches_by_name() {
    use kumi::daw::{daw_command, work_with, DawCommand};
    work_with(DawId::Reaper);
    assert_eq!(
        daw_command(""),
        DawCommand::Say("Kumi works with REAPER. /daw live or /daw renoise opens Kumi again with Live or Renoise.".into())
    );
    for typed in [" Live", "live", "ableton"] {
        assert_eq!(daw_command(typed), DawCommand::Switch(DawId::Ableton, "Kumi closes and opens again with Live.".into()), "{typed}");
    }
    assert_eq!(daw_command("reaper"), DawCommand::Say("Kumi works with REAPER already.".into()));
    assert_eq!(daw_command("bitwig"), DawCommand::Say("Kumi doesn't know bitwig: /daw live, /daw reaper, /daw renoise.".into()));
    work_with(DawId::Ableton);
}

/// What `kumi --daw reaper|renoise bridge`, uninstall and update say for REAPER and Renoise: their own words, never
/// Live's, and the command that sets up their bridge (`--daw`), not Live's.
#[tokio::test(flavor = "current_thread")]
async fn reapers_and_renoises_bridge_messages_never_speak_lives_words() {
    let (dir, mut env) = renoise_home();
    env.insert("KUMI_REAPER_RESOURCE_DIR".into(), dir.path().join("REAPER").to_string_lossy().into_owned());
    std::fs::create_dir_all(dir.path().join("REAPER/UserPlugins")).unwrap();
    for provider in [&own::REAPER as &dyn DawProvider, &own::RENOISE] {
        let out = Rc::new(Out::default());
        provider.setup_bridge(out.clone(), &env, yes()).await.unwrap();
        let io = kumi::install::InstalledIo::new(out.clone(), env.clone());
        let run: kumi::process::Run = Rc::new(|_, _, _| panic!("removing the bridge runs no program"));
        let _ = provider.remove_bridge(&io, run).await;
        let state = provider.bridge_state(&env, false, true, None, "kumi");
        let watch = provider.watch_words();
        let said = [
            out.0.borrow().clone(),
            provider.removal_question().to_string(),
            provider.files_stay("~/folder"),
            state.notice.clone().unwrap_or_default(),
            state.why.unwrap_or_default(),
            watch.starting.into(),
            watch.unreachable.into(),
            watch.answered.into(),
        ]
        .join("\n");
        let leaks = live_words::leaks(&said);
        assert!(leaks.is_empty(), "{:?} says Live's words:\n{}\n\nin:\n{said}", provider.id(), leaks.join("\n"));
        let command = format!("kumi --daw {} bridge", provider.id().as_str());
        assert!(state.notice.unwrap().contains(&command) && watch.unreachable.contains(&command), "{said}");
    }
}

/// Asking for Live (`--daw ableton`, `KUMI_DAW=ableton`) starts Live, though REAPER's bridge is in too and was used
/// last; asking for nothing goes by the last used. The configuration still never writes Live.
#[tokio::test(flavor = "current_thread")]
async fn asking_for_live_starts_live_whatever_was_used_last() {
    let (dir, mut env) = renoise_home();
    // Live's bridge, as its reference in the Remote Scripts says.
    let config = dir.path().join("bridge-config.json");
    std::fs::write(&config, "{}").unwrap();
    let scripts = dir.path().join("Remote Scripts/AbletonMcpBridge");
    std::fs::create_dir_all(&scripts).unwrap();
    std::fs::write(scripts.join("bridge-reference.json"), serde_json::json!({"config": config}).to_string()).unwrap();
    // REAPER's, as its receipt says, and used last.
    let resource = dir.path().join("REAPER");
    env.insert("KUMI_REAPER_RESOURCE_DIR".into(), resource.to_string_lossy().into_owned());
    let paths = daw_bridge::paths::BridgePaths::new(&dir.path().join(".kumi"), daw_bridge::daw::Daw::Reaper);
    std::fs::create_dir_all(&paths.root).unwrap();
    let receipt = daw_bridge::lifecycle::Receipt {
        schema: daw_bridge::lifecycle::RECEIPT_SCHEMA.into(),
        daw: "reaper".into(),
        status: "installed".into(),
        target: resource.join("UserPlugins/reaper_kumi.dylib").to_string_lossy().into_owned(),
        reference: resource.join("UserPlugins/kumi-bridge-reference.json").to_string_lossy().into_owned(),
        generation: Some("g1".into()),
        version: Some("0.1.0".into()),
        sha256: None,
        previous: vec![],
        updated_ms: 0,
        history: vec![],
    };
    std::fs::write(paths.receipt(), serde_json::to_string(&receipt).unwrap()).unwrap();
    kumi::daw::remember_used(DawId::Reaper, &env);
    let config_of = |args: &[&str], env: &Env| {
        serde_json::to_value(kumi::config::load_config(&args.iter().map(|s| s.to_string()).collect::<Vec<_>>(), env).unwrap()).unwrap()
    };
    let asked = config_of(&["--daw", "ableton"], &env);
    assert_eq!(asked["bridgeConfig"], serde_json::json!(config.to_string_lossy()), "Live's bridge: {asked}");
    assert!(asked.get("daw").is_none(), "Live is never written: {asked}");
    let mut by_env = env.clone();
    by_env.insert("KUMI_DAW".into(), "ableton".into());
    assert_eq!(config_of(&[], &by_env), asked);
    // Nothing asked and nothing running: the one used last (what runs on this computer is left out).
    let none = |_: &dyn DawProvider| false;
    assert_eq!(kumi::daw::chosen_with(None, &env, &none).0.id(), DawId::Reaper, "nothing asked: the one used last");
    assert_eq!(kumi::daw::chosen_with(Some(DawId::Ableton), &env, &none).0.id(), DawId::Ableton, "Live asked: Live");
}
