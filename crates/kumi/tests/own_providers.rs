//! REAPER's, Renoise's and Logic's providers: Kumi's bridge goes into the DAW's own folder (Logic's into Kumi's own)
//! through the shared lifecycle, is found again (and chosen when it's the only bridge installed), and comes out; the
//! doctor says nothing of a DAW this user has never used.
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
        DawCommand::Say(
            "Kumi works with REAPER. /daw live, /daw renoise or /daw logic opens Kumi again with Live, Renoise or Logic Pro.".into()
        )
    );
    for typed in [" Live", "live", "ableton"] {
        assert_eq!(daw_command(typed), DawCommand::Switch(DawId::Ableton, "Kumi closes and opens again with Live.".into()), "{typed}");
    }
    assert_eq!(daw_command("reaper"), DawCommand::Say("Kumi works with REAPER already.".into()));
    assert_eq!(
        daw_command("bitwig"),
        DawCommand::Say("Kumi doesn't know bitwig: /daw live, /daw reaper, /daw renoise, /daw logic.".into())
    );
    // Logic Pro goes by its id, not its two-word name.
    for typed in ["logic", "Logic Pro"] {
        assert_eq!(daw_command(typed), DawCommand::Switch(DawId::Logic, "Kumi closes and opens again with Logic Pro.".into()), "{typed}");
    }
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

/// Copies a folder, as a release's bundle holds one.
fn copy_folder(from: &std::path::Path, to: &std::path::Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap().flatten() {
        let path = entry.path();
        if path.is_dir() {
            copy_folder(&path, &to.join(entry.file_name()));
        } else {
            std::fs::copy(&path, to.join(entry.file_name())).unwrap();
        }
    }
}

/// A Kumi's app folder carrying REAPER's extension (these bytes, this version), Renoise's tool (this checkout's, saying
/// `tool`) and Logic's bridge (a stand-in bridge and helper, versioned `logic`), laid out as a release lays them out.
fn app_carrying(root: &std::path::Path, name: &str, extension: &[u8], version: &str, tool: &str, logic: &str) -> std::path::PathBuf {
    let app = root.join(name);
    let reaper = app.join("daws").join("reaper");
    std::fs::create_dir_all(&reaper).unwrap();
    let file = daw_bridge::install::reaper_extension_name(daw_bridge::detect::Os::current());
    std::fs::write(reaper.join(file), extension).unwrap();
    std::fs::write(reaper.join("reaper_kumi.version"), format!("{version}\n")).unwrap();
    let folder = app.join("daws").join("renoise").join("com.kumi.Bridge.xrnx");
    copy_folder(&std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../daws/renoise/com.kumi.Bridge.xrnx"), &folder);
    let lua = folder.join("payload").join("version.lua");
    let text = std::fs::read_to_string(&lua).unwrap();
    std::fs::write(&lua, text.replace("\"1.0.0\"", &format!("\"{tool}\""))).unwrap();
    let bridge = app.join("daws").join("logic").join("logic-kumi");
    std::fs::create_dir_all(&bridge).unwrap();
    std::fs::write(bridge.join("logic-kumi"), format!("#!/bin/sh\necho logic-kumi {logic}\n")).unwrap();
    std::fs::write(bridge.join("kumi-logic-hands-test"), b"#!/bin/sh\nexit 0\n").unwrap();
    std::fs::write(bridge.join("version"), format!("{logic}\n")).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for name in ["logic-kumi", "kumi-logic-hands-test"] {
            std::fs::set_permissions(bridge.join(name), std::fs::Permissions::from_mode(0o755)).unwrap();
        }
    }
    app
}

fn receipt_of(env: &Env, daw: daw_bridge::daw::Daw) -> daw_bridge::lifecycle::Receipt {
    let paths = daw_bridge::paths::BridgePaths::new(std::path::Path::new(&env["KUMI_HOME"]), daw);
    serde_json::from_str(&std::fs::read_to_string(paths.receipt()).unwrap()).unwrap()
}

/// An update that brings the bridges in place changes nothing (so the one to roll back to stays); Kumi's rollback takes
/// each to the copy the Kumi it goes back to carries, and the next rollback back again, from the generations kept; an
/// earlier Kumi that knows none of these DAWs leaves each where it is. An update finds REAPER's extension where Kumi put
/// it. Logic's bridge goes the same ways in Kumi's own folder, in its own words: nothing of Logic's restarts.
#[tokio::test(flavor = "current_thread")]
async fn an_update_keeps_the_bridges_in_place_and_a_rollback_takes_each_to_the_copy_its_kumi_carries() {
    use daw_bridge::daw::Daw;
    let (dir, mut env) = renoise_home();
    let resource = dir.path().join("REAPER");
    std::fs::create_dir_all(resource.join("UserPlugins")).unwrap();
    std::fs::write(resource.join("reaper.ini"), "[REAPER]\n").unwrap();
    env.insert("KUMI_REAPER_RESOURCE_DIR".into(), resource.to_string_lossy().into_owned());
    let now = app_carrying(dir.path(), "app", b"extension 0.1.0", "0.1.0", "1.0.0", "0.1.0");
    let earlier = app_carrying(dir.path(), "app.previous", b"extension 0.0.9", "0.0.9", "0.9.0", "0.0.9");
    let unknowing = dir.path().join("app.older");
    std::fs::create_dir_all(&unknowing).unwrap();
    let home = env["KUMI_HOME"].clone();
    let out = Rc::new(Out::default());
    let io = kumi::install::InstalledIo::new(out.clone(), env.clone());
    let providers = [&own::REAPER as &dyn DawProvider, &own::RENOISE, &own::LOGIC];
    let logic_placed = std::path::Path::new(&home).join("bridges/logic/bridge/logic-kumi");

    // All three in, as `kumi --daw renoise bridge`, `kumi --daw logic bridge` and `kumi --daw reaper bridge` put them
    // (REAPER's and Logic's from `now`).
    assert_eq!(own::RENOISE.setup_bridge(out.clone(), &env, yes()).await.unwrap(), 0, "{}", out.0.borrow());
    let mut from_now = env.clone();
    from_now.insert(own::LOGIC_BRIDGE_SOURCE_VAR.into(), now.join("daws/logic/logic-kumi").to_string_lossy().into_owned());
    assert_eq!(own::LOGIC.setup_bridge(out.clone(), &from_now, yes()).await.unwrap(), 0, "{}", out.0.borrow());
    let extension = now.join("daws/reaper").join(daw_bridge::install::reaper_extension_name(daw_bridge::detect::Os::current()));
    let target = daw_bridge::install::reaper_target(
        &daw_bridge::detect::ReaperInstall {
            app: None,
            version: None,
            archs: vec![],
            resource: resource.clone(),
            resource_from: daw_bridge::detect::ResourceFrom::Override,
            opened: true,
        },
        daw_bridge::detect::Os::current(),
    )
    .unwrap();
    daw_bridge::lifecycle::Lifecycle::new(std::path::Path::new(&home), target).install(&extension, "0.1.0").unwrap();
    let installed = [receipt_of(&env, Daw::Reaper), receipt_of(&env, Daw::Renoise), receipt_of(&env, Daw::Logic)];
    assert_eq!(installed[2].version.as_deref(), Some("0.1.0"));

    // `kumi update` to a Kumi bringing the same bridges, twice: nothing changes, nothing is said.
    out.0.borrow_mut().clear();
    for _ in 0..2 {
        for provider in providers {
            assert_eq!(provider.after_update(&io, &home, &now.to_string_lossy(), None).await, 0);
        }
    }
    assert_eq!([receipt_of(&env, Daw::Reaper), receipt_of(&env, Daw::Renoise), receipt_of(&env, Daw::Logic)], installed);
    assert_eq!(out.0.borrow().as_str(), "");

    // Rolled back to a Kumi carrying 0.0.9, 0.9.0 and 0.0.9, then forward again.
    for (app, kumi_version, reaper, renoise, logic) in
        [(&earlier, "1.19.2", "0.0.9", "0.9.0", "0.0.9"), (&now, "1.19.3", "0.1.0", "1.0.0", "0.1.0")]
    {
        out.0.borrow_mut().clear();
        for provider in providers {
            let step = provider.prepare_rollback(&io, &home, &app.to_string_lossy(), kumi_version, false).await.unwrap();
            assert_eq!(step.after_swap(&io, &home, &app.to_string_lossy(), kumi_version, true).await, 0);
        }
        let said = out.0.borrow().clone();
        assert!(
            said.contains(&format!(
                "Kumi's extension {reaper} is back in REAPER, as Kumi {kumi_version} had it: restart REAPER if it's open."
            )),
            "{said}"
        );
        assert!(said.contains(&format!("Kumi's tool {renoise} is back in Renoise, as Kumi {kumi_version} had it")), "{said}");
        assert!(
            said.contains(&format!(
                "Kumi's Logic Pro bridge {logic} is back in Kumi's own folder, as Kumi {kumi_version} had it: Kumi starts it beside Logic Pro when a session begins.\n"
            )),
            "{said}"
        );
        assert!(!said.contains("restart Logic Pro") && !said.contains("in Logic Pro"), "{said}");
        let (reaper_receipt, renoise_receipt) = (receipt_of(&env, Daw::Reaper), receipt_of(&env, Daw::Renoise));
        assert_eq!((reaper_receipt.version.as_deref(), renoise_receipt.version.as_deref()), (Some(reaper), Some(renoise)));
        assert_eq!((reaper_receipt.history[0].action.as_str(), renoise_receipt.history[0].action.as_str()), ("rollback", "rollback"));
        let placed = std::fs::read(resource.join("UserPlugins").join(extension.file_name().unwrap())).unwrap();
        assert_eq!(placed, format!("extension {reaper}").into_bytes());
        let logic_receipt = receipt_of(&env, Daw::Logic);
        assert_eq!((logic_receipt.version.as_deref(), logic_receipt.history[0].action.as_str()), (Some(logic), "rollback"));
        assert_eq!(std::fs::read_to_string(logic_placed.join("version")).unwrap(), format!("{logic}\n"));
        assert!(logic_placed.join("bridge-reference.json").is_file(), "the reference stays inside the folder");
    }
    // Forward again is the generation kept from the first install, not a third copy.
    assert_eq!(receipt_of(&env, Daw::Reaper).generation, installed[0].generation);
    assert_eq!(receipt_of(&env, Daw::Renoise).generation, installed[1].generation);
    assert_eq!(receipt_of(&env, Daw::Logic).generation, installed[2].generation);

    // An earlier Kumi that knows none of these DAWs: each stays, said; and as KUMI_DAW names Renoise, which that Kumi
    // refuses to start with, that's said too.
    out.0.borrow_mut().clear();
    let mut asking = env.clone();
    asking.insert("KUMI_DAW".into(), "renoise".into());
    let asking = kumi::install::InstalledIo::new(out.clone(), asking);
    for provider in providers {
        let step = provider.prepare_rollback(&asking, &home, &unknowing.to_string_lossy(), "1.19.1", false).await.unwrap();
        assert_eq!(step.after_swap(&asking, &home, &unknowing.to_string_lossy(), "1.19.1", true).await, 0);
    }
    let said = out.0.borrow().clone();
    assert!(said.contains("Kumi's extension stays in REAPER (") && said.contains("Kumi 1.19.1 doesn't know REAPER"), "{said}");
    assert!(said.contains("Kumi's tool stays in Renoise (") && said.contains("Kumi 1.19.1 doesn't know Renoise"), "{said}");
    assert!(
        said.contains(&format!(
            "Kumi's Logic Pro bridge stays in Kumi's own folder ({}): Kumi 1.19.1 doesn't know Logic Pro, so it won't use it or remove it. Delete that folder to take it out.\n",
            installed[2].target
        )),
        "{said}"
    );
    assert_eq!(receipt_of(&env, Daw::Logic).version.as_deref(), Some("0.1.0"));
    assert!(
        said.contains(
            "KUMI_DAW is set to renoise where you run Kumi, and Kumi 1.19.1 doesn't know Renoise: it won't start while it's set."
        ),
        "{said}"
    );
    assert!(!said.contains("KUMI_DAW is set to reaper"), "{said}");
    assert_eq!(receipt_of(&env, Daw::Reaper).version.as_deref(), Some("0.1.0"));

    // An update with KUMI_REAPER_RESOURCE_DIR no longer set, and a REAPER with its default resource folder too:
    // REAPER's extension is updated where Kumi put it, not moved into the other REAPER.
    for default in ["Library/Application Support/REAPER", "AppData/Roaming/REAPER", ".config/REAPER"] {
        std::fs::create_dir_all(dir.path().join(default)).unwrap();
        std::fs::write(dir.path().join(default).join("reaper.ini"), "[REAPER]\n").unwrap();
    }
    let mut unset = env.clone();
    unset.remove("KUMI_REAPER_RESOURCE_DIR");
    let io = kumi::install::InstalledIo::new(out.clone(), unset);
    out.0.borrow_mut().clear();
    assert_eq!(own::REAPER.after_update(&io, &home, &earlier.to_string_lossy(), None).await, 0);
    assert!(out.0.borrow().contains("Kumi's extension 0.0.9 is in REAPER now: restart REAPER if it's open."), "{}", out.0.borrow());
    assert_eq!(receipt_of(&env, Daw::Reaper).target, installed[0].target);
    for default in ["Library/Application Support/REAPER", "AppData/Roaming/REAPER", ".config/REAPER"] {
        assert!(!dir.path().join(default).join("UserPlugins").exists(), "{default}");
    }

    // An update bringing another Logic bridge puts it in Kumi's own folder, where it was.
    out.0.borrow_mut().clear();
    assert_eq!(own::LOGIC.after_update(&io, &home, &earlier.to_string_lossy(), None).await, 0);
    assert_eq!(
        out.0.borrow().as_str(),
        "Kumi's Logic Pro bridge 0.0.9 is in Kumi's own folder now: Kumi starts it beside Logic Pro when a session begins.\n"
    );
    let logic_receipt = receipt_of(&env, Daw::Logic);
    assert_eq!((logic_receipt.version.as_deref(), logic_receipt.target.as_str()), (Some("0.0.9"), installed[2].target.as_str()));
}

/// A Kumi with no bridge for Logic Pro to set up (Linux's, Windows', the Intel Mac's) says Logic Pro is supported on
/// Apple Silicon Macs only. A release (installed: its launcher sets KUMI_INSTALLED) sets up only the bridge it carries,
/// never one staged from the checkout it was built in; and nothing but a Mac runs Logic Pro, so a checkout's build
/// elsewhere has none either. A session and the doctor say why, rather than the command that would only say it again.
#[tokio::test(flavor = "current_thread")]
async fn a_kumi_without_logics_bridge_says_logic_pro_is_supported_on_apple_silicon_macs_only() {
    let (dir, mut env) = logic_home();
    env.remove(own::LOGIC_BRIDGE_SOURCE_VAR);
    env.insert("KUMI_INSTALLED".into(), "1".into());
    env.insert("KUMI_LOGIC_APP".into(), fake_logic_app(dir.path(), "12.3.1").to_string_lossy().into_owned());
    let why = if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        // The Apple Silicon Mac's release carries it: one that doesn't wants installing again.
        "this Kumi doesn't carry it: run Kumi's installer again".to_string()
    } else {
        let system = if cfg!(target_os = "macos") {
            "Intel Macs"
        } else if cfg!(windows) {
            "Windows"
        } else {
            "Linux"
        };
        format!("Logic Pro is supported on Apple Silicon Macs only, and this Kumi is for {system}")
    };
    let logic: &dyn DawProvider = &own::LOGIC;
    let placed = dir.path().join(".kumi/bridges/logic/bridge/logic-kumi");
    let mut envs = vec![env.clone()];
    if !cfg!(target_os = "macos") {
        let mut checkout = env.clone();
        checkout.remove("KUMI_INSTALLED");
        envs.push(checkout);
    }
    for env in &envs {
        // With --yes or without (then nothing is asked first): refused in words, nothing set up or staged.
        for command in [yes(), Default::default()] {
            let out = Rc::new(Out::default());
            assert_eq!(logic.setup_bridge(out.clone(), env, command).await.unwrap(), 1, "{}", out.0.borrow());
            assert_eq!(out.0.borrow().as_str(), format!("Kumi's Logic Pro bridge couldn't be set up: {why}.\n"));
            assert!(!placed.exists() && !logic.bridge_installed(env));
        }
        let state = logic.bridge_state(env, false, true, None, "kumi");
        assert_eq!(
            state.notice,
            Some(format!(
                "Kumi's Logic Pro bridge isn't set up yet, so Kumi can't see your project; chatting without it. {}{}.",
                why[..1].to_uppercase(),
                &why[1..]
            ))
        );
        let doctor = kumi::doctor::DoctorIo::new(Rc::new(Out::default()), env.clone());
        let checks = logic.doctor_checks(&doctor, &kumi::doctor::Check::ok("Kumi")).await;
        assert_eq!(checks.len(), 1, "{checks:?}");
        assert_eq!(
            (checks[0].status.clone(), checks[0].text.clone(), checks[0].next.clone()),
            (kumi::doctor::CheckStatus::Note, format!("Kumi's Logic Pro bridge isn't set up: {why}"), None)
        );
    }
}

/// A folder laid out as a release lays Logic's bridge out: a stand-in bridge binary, one helper and the version file.
fn logic_bridge_source(dir: &std::path::Path) -> std::path::PathBuf {
    let source = dir.join("release").join("logic-kumi");
    std::fs::create_dir_all(&source).unwrap();
    std::fs::write(source.join("logic-kumi"), b"#!/bin/sh\necho logic-kumi 0.1.0\n").unwrap();
    std::fs::write(source.join("kumi-logic-hands-test"), b"#!/bin/sh\nexit 0\n").unwrap();
    std::fs::write(source.join("version"), "0.1.0\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for name in ["logic-kumi", "kumi-logic-hands-test"] {
            std::fs::set_permissions(source.join(name), std::fs::Permissions::from_mode(0o755)).unwrap();
        }
    }
    source
}

/// A Logic Pro app of `version`, as detection reads it (its plist and its arm64 executable).
fn fake_logic_app(dir: &std::path::Path, version: &str) -> std::path::PathBuf {
    let app = dir.join("Logic Pro.app");
    std::fs::create_dir_all(app.join("Contents/MacOS")).unwrap();
    std::fs::write(
        app.join("Contents/Info.plist"),
        format!("<plist><dict><key>CFBundleIdentifier</key><string>com.apple.logic10</string><key>CFBundleShortVersionString</key><string>{version}</string></dict></plist>"),
    )
    .unwrap();
    std::fs::write(app.join("Contents/MacOS/Logic Pro"), [0xcf, 0xfa, 0xed, 0xfe, 0x0c, 0, 0, 0x01]).unwrap();
    app
}

/// A home with no DAW's folders, Logic's bridge to set up from a release-like folder, and Kumi's folder in it.
fn logic_home() -> (tempfile::TempDir, Env) {
    let dir = tempfile::tempdir().unwrap();
    let source = logic_bridge_source(dir.path());
    let env = Env::from([
        ("HOME".into(), dir.path().to_string_lossy().into_owned()),
        ("KUMI_HOME".into(), dir.path().join(".kumi").to_string_lossy().into_owned()),
        ("KUMI_SETTINGS_FILE".into(), dir.path().join("settings.json").to_string_lossy().into_owned()),
        ("KUMI_REMOTE_SCRIPTS_DIR".into(), dir.path().join("Remote Scripts").to_string_lossy().into_owned()),
        ("KUMI_RENOISE_PREFS_DIR".into(), dir.path().join("Renoise").to_string_lossy().into_owned()),
        ("KUMI_REAPER_RESOURCE_DIR".into(), dir.path().join("REAPER").to_string_lossy().into_owned()),
        // No Logic here: the bridge needs none to go in, since it goes into Kumi's own folder.
        ("KUMI_LOGIC_APP".into(), dir.path().join("nowhere").join("Logic Pro.app").to_string_lossy().into_owned()),
        (kumi::daw::own::LOGIC_BRIDGE_SOURCE_VAR.into(), source.to_string_lossy().into_owned()),
    ]);
    (dir, env)
}

/// Logic loads nothing, so its bridge goes into Kumi's own folder (with the reference inside it, beside the binary
/// and its helper), while Logic stays as it is: nothing says to quit or restart it.
#[tokio::test(flavor = "current_thread")]
async fn logics_bridge_goes_in_kumis_own_folder_is_found_and_chosen_and_comes_out() {
    let (dir, env) = logic_home();
    let logic: &dyn DawProvider = &own::LOGIC;
    assert!(!logic.bridge_installed(&env));
    assert_eq!(chosen(None, &env).0.id(), DawId::Ableton, "nothing installed: Live's words");

    // Asked first: with nobody to answer (no --yes, no terminal), nothing is set up.
    let out = Rc::new(Out::default());
    assert_eq!(logic.setup_bridge(out.clone(), &env, Default::default()).await.unwrap(), 1);
    let said = out.0.borrow().clone();
    assert!(said.contains("Set up Kumi's Logic Pro bridge in Kumi's own folder (") && said.contains("add --yes"), "{said}");
    assert!(said.contains("Nothing was changed. To set it up later: ") && said.contains(" --daw logic bridge\n"), "{said}");
    let placed = dir.path().join(".kumi/bridges/logic/bridge/logic-kumi");
    assert!(!placed.exists() && !logic.bridge_installed(&env));

    let out = Rc::new(Out::default());
    let code = logic.setup_bridge(out.clone(), &env, yes()).await.unwrap();
    assert_eq!(code, 0, "{}", out.0.borrow());
    let said = out.0.borrow().clone();
    assert!(said.contains("Kumi's Logic Pro bridge 0.1.0 is in Kumi's own folder."), "{said}");
    assert!(said.contains("open Logic Pro, then run") && said.contains("--daw logic") && said.contains("Accessibility"), "{said}");
    for word in ["restart", "quit"] {
        assert!(!said.to_lowercase().contains(word), "{word:?} in:\n{said}");
    }
    for name in ["logic-kumi", "kumi-logic-hands-test", "version", "bridge-reference.json"] {
        assert!(placed.join(name).is_file(), "{name} in {}", placed.display());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(placed.join("logic-kumi")).unwrap().permissions().mode();
        assert_eq!(mode & 0o111, 0o111, "the host runs it: {mode:o}");
    }
    let reference = daw_bridge::files::Reference::read(&placed.join("bridge-reference.json")).unwrap();
    assert_eq!((reference.daw.as_str(), reference.port), ("logic", Some(9790)));
    assert!(reference.discovery_dir.ends_with("bridges/logic/discovery"), "{reference:?}");
    assert!(
        reference.secret_file.ends_with("bridges/logic/bridge.secret") && dir.path().join(".kumi/bridges/logic/bridge.secret").is_file()
    );
    assert!(logic.bridge_installed(&env));
    let config = logic.bridge_config(&env).expect("the reference, once installed");
    assert!(config.ends_with("logic-kumi/bridge-reference.json"), "{config}");
    let (provider, found) = chosen(None, &env);
    assert_eq!((provider.id(), found.as_deref()), (DawId::Logic, Some(config.as_str())), "the only bridge installed is chosen");

    // Its words never say Live's, and say how it's set up.
    let state = logic.bridge_state(&env, false, true, None, "kumi");
    let watch = logic.watch_words();
    let said = [
        state.notice.clone().unwrap_or_default(),
        state.why.clone().unwrap_or_default(),
        watch.starting.into(),
        watch.unreachable.into(),
        watch.answered.into(),
        logic.removal_question().into(),
        logic.files_stay("~/folder"),
    ]
    .join("\n");
    let leaks = live_words::leaks(&said);
    assert!(leaks.is_empty(), "Logic says Live's words:\n{}\n\nin:\n{said}", leaks.join("\n"));
    assert_eq!(
        state.notice.as_deref(),
        Some("Kumi's Logic Pro bridge isn't set up yet, so Kumi can't see your project; chatting without it. Run kumi --daw logic bridge to set it up.")
    );
    assert_eq!(
        logic.enable_step().map(|step| (step.label, step.waiting)),
        Some(("Accessibility", "waiting for Accessibility")),
        "setup's step after the bridge"
    );

    // Out again: the folder goes, and nobody is asked to quit Logic.
    let io = kumi::install::InstalledIo::new(out.clone(), env.clone());
    let run: kumi::process::Run = Rc::new(|_, _, _| panic!("removing the bridge runs no program"));
    assert_eq!(logic.remove_bridge(&io, run).await.unwrap(), RemovedBridge::Removed);
    assert!(!placed.exists());
    assert!(!logic.bridge_installed(&env));
}

/// The doctor speaks of Logic once Logic Pro is here: a note that the bridge isn't set up (with the command), never a
/// fix; and nothing at all while there's no Logic Pro and no bridge.
#[tokio::test(flavor = "current_thread")]
async fn the_doctor_notes_logics_bridge_once_logic_pro_is_here() {
    let (dir, mut env) = logic_home();
    let logic: &dyn DawProvider = &own::LOGIC;
    let io = kumi::doctor::InstallationIo::default();
    assert!(logic.installation_checks(&env, &io, "darwin").await.is_empty(), "no Logic Pro, no bridge: nothing to say");
    let doctor = kumi::doctor::DoctorIo::new(Rc::new(Out::default()), env.clone());
    assert!(logic.doctor_checks(&doctor, &kumi::doctor::Check::ok("Kumi")).await.is_empty());
    let app = fake_logic_app(dir.path(), "12.3.1");
    env.insert("KUMI_LOGIC_APP".into(), app.to_string_lossy().into_owned());
    let checks = logic.installation_checks(&env, &io, "darwin").await;
    assert_eq!(checks.len(), 1, "{checks:?}");
    assert_eq!(checks[0].text, "Logic Pro is installed here.");
    let doctor = kumi::doctor::DoctorIo::new(Rc::new(Out::default()), env.clone());
    let checks = logic.doctor_checks(&doctor, &kumi::doctor::Check::ok("Kumi")).await;
    assert_eq!(checks.len(), 1, "{checks:?}");
    assert_eq!(
        (checks[0].status.clone(), checks[0].text.as_str()),
        (kumi::doctor::CheckStatus::Note, "Kumi's Logic Pro bridge isn't set up")
    );
    assert!(
        checks[0].next.as_deref().is_some_and(|next| next.ends_with(" --daw logic bridge sets it up, if you want Kumi in Logic Pro")),
        "{:?}",
        checks[0].next
    );
}
