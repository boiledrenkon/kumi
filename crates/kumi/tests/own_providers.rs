//! REAPER's and Renoise's providers: Kumi's bridge goes into the DAW's own folder through the shared lifecycle, is
//! found again (and chosen when it's the only bridge installed), and comes out; the doctor says nothing of a DAW this
//! user has never used.
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

#[tokio::test(flavor = "current_thread")]
async fn renoises_tool_goes_in_is_found_and_chosen_and_comes_out() {
    let (dir, env) = renoise_home();
    let renoise: &dyn DawProvider = &own::RENOISE;
    assert!(!renoise.bridge_installed(&env));
    assert_eq!(chosen(None, &env).0.id(), DawId::Ableton, "nothing installed: Live's words");

    let out = Rc::new(Out::default());
    let code = renoise.setup_bridge(out.clone(), &env, Default::default()).await.unwrap();
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
