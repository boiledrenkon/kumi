//! `--daw <id>` and KUMI_DAW choose the DAW a session works with, among the DAWs this Kumi integrates (Live, REAPER and
//! Renoise). Choosing Live is what Kumi does anyway: the configuration reads, and is written, exactly as without it.
use kumi::config::*;
use kumi_runtime::system::Env;

fn env() -> Env {
    Env::from([
        ("KUMI_SETTINGS_FILE".into(), "/nonexistent-kumi-test/settings.json".into()),
        ("KUMI_REMOTE_SCRIPTS_DIR".into(), "/nonexistent-kumi-test/Remote Scripts".into()),
    ])
}
fn args(args: &[&str]) -> Vec<String> {
    args.iter().map(|s| s.to_string()).collect()
}
fn config(args_: &[&str], env: &Env) -> Result<String, String> {
    load_config(&args(args_), env).map(|config| serde_json::to_string(&config).unwrap()).map_err(|error| error.message())
}

#[test]
fn choosing_live_reads_as_choosing_nothing() {
    let env = env();
    for (asked, plain) in [
        (vec!["--daw", "ableton"], vec![]),
        (vec!["--daw=ableton"], vec![]),
        (vec!["--daw", "ableton", "--inference-only"], vec!["--inference-only"]),
        (vec!["--inference-only", "--daw", "ableton"], vec!["--inference-only"]),
        (vec!["-c", "--daw", "ableton"], vec!["-c"]),
    ] {
        assert_eq!(config(&asked, &env), config(&plain, &env), "{asked:?}");
    }
    let mut chosen = env.clone();
    chosen.insert("KUMI_DAW".into(), "ableton".into());
    assert_eq!(config(&[], &chosen), config(&[], &env));
    chosen.insert("KUMI_DAW".into(), String::new());
    assert_eq!(config(&[], &chosen), config(&[], &env), "an empty KUMI_DAW is no choice");
    assert!(!config(&["--daw", "ableton"], &env).unwrap().contains("daw"), "never written for Live");
}

#[test]
fn only_a_daw_this_kumi_integrates_can_be_chosen() {
    let env = env();
    for asked in [vec!["--daw"], vec!["--daw", "Live"], vec!["--daw", "ABLETON"], vec!["--daw", "bitwig"]] {
        assert_eq!(config(&asked, &env), Err("Use: --daw <daw>, with daw one of ableton, reaper, renoise.".into()), "{asked:?}");
    }
    let mut chosen = env.clone();
    chosen.insert("KUMI_DAW".into(), "bitwig".into());
    assert_eq!(config(&[], &chosen), Err("KUMI_DAW must be one of: ableton, reaper, renoise.".into()));
    // --daw beats KUMI_DAW.
    assert_eq!(config(&["--daw", "ableton"], &chosen), config(&[], &env));
}

#[test]
fn reaper_or_renoise_chosen_is_written_and_read_back() {
    let env = env();
    for (asked, id) in [(vec!["--daw", "reaper"], "reaper"), (vec!["--daw=renoise"], "renoise")] {
        let config: serde_json::Value = serde_json::from_str(&config(&asked, &env).unwrap()).unwrap();
        assert_eq!(config["daw"], id, "{asked:?}");
    }
    let mut chosen = env.clone();
    chosen.insert("KUMI_DAW".into(), "renoise".into());
    let config: serde_json::Value = serde_json::from_str(&config(&[], &chosen).unwrap()).unwrap();
    assert_eq!(config["daw"], "renoise");
}

/// With no DAW's bridge installed and none asked for, Kumi chats without a DAW in Live's words: Live stays the DAW Kumi
/// starts with. REAPER and Renoise are chosen with --daw or KUMI_DAW, or by their bridge being installed.
#[test]
fn without_any_bridge_kumi_chats_in_lives_words() {
    use kumi::daw::{chosen, providers};
    use kumi_runtime::core::daw::{DawId, DawProfile};
    let env = env();
    assert_eq!(providers().map(|provider| provider.id()), [DawId::Ableton, DawId::Reaper, DawId::Renoise], "Live first");
    let (provider, found) = chosen(None, &env);
    assert_eq!((provider.id(), found), (DawId::Ableton, None));
    let without = provider.without_daw(std::rc::Rc::new(|_| {}));
    // Live's words and vocabulary, with none of Live's capabilities while there's no Live to use them on.
    assert_eq!((without.profile().id, without.profile().words), (Some(DawId::Ableton), DawProfile::ABLETON.words));
    assert!(*without.profile() == DawProfile::ABLETON.without_caps());
    let config = serde_json::to_value(load_config(&[], &env).unwrap()).unwrap();
    assert_eq!((config["mode"].as_str(), config["bridgeMissing"].as_bool(), config.get("daw")), (Some("inference-only"), Some(true), None));
}

#[test]
fn bridge_sets_up_the_bridge_of_the_daw_asked_for_and_lives_otherwise() {
    let env = env();
    assert_eq!(config(&["bridge"], &env).unwrap(), r#"{"mode":"bridge","yes":false,"allowDirty":false}"#, "Live's, as always");
    assert_eq!(config(&["--daw", "ableton", "bridge"], &env), config(&["bridge"], &env));
    assert_eq!(
        config(&["--daw", "reaper", "bridge", "--yes"], &env).unwrap(),
        r#"{"mode":"bridge","yes":true,"allowDirty":false,"daw":"reaper"}"#
    );
    assert_eq!(config(&["bridge", "--daw=renoise"], &env), config(&["--daw", "renoise", "bridge"], &env));
    // KUMI_DAW chooses the DAW sessions work with, never whose bridge `kumi bridge` sets up: Live's update ends with
    // `kumi bridge`, and every fix Kumi gives for Live says it (review L1). Nor does one Kumi doesn't know stop it (L10).
    for chosen in ["renoise", "reaper", "logic"] {
        let mut chosen_env = env.clone();
        chosen_env.insert("KUMI_DAW".into(), chosen.into());
        assert_eq!(config(&["bridge", "--yes"], &chosen_env), config(&["bridge", "--yes"], &env), "KUMI_DAW={chosen}");
        assert_eq!(config(&["--daw", "reaper", "bridge"], &chosen_env), config(&["--daw", "reaper", "bridge"], &env));
    }
    assert!(config(&["--daw", "logic", "bridge"], &env).unwrap_err().starts_with("Use: --daw <daw>"));
    assert_eq!(config(&["--daw", "reaper", "bridge", "--force"], &env).unwrap_err(), "Use: bridge [--yes] [--allow-dirty].");
}

/// `kumi --daw reaper doctor` (or KUMI_DAW) asks the doctor and the report to speak of that DAW; without it they read as
/// they always did.
#[test]
fn doctor_and_report_take_the_daw_asked_for() {
    let env = env();
    assert_eq!(config(&["doctor"], &env).unwrap(), r#"{"mode":"doctor","json":false}"#);
    assert_eq!(config(&["doctor", "--json"], &env).unwrap(), r#"{"mode":"doctor","json":true}"#);
    assert_eq!(config(&["report"], &env).unwrap(), r#"{"mode":"report"}"#);
    assert_eq!(config(&["--daw", "reaper", "doctor"], &env).unwrap(), r#"{"mode":"doctor","json":false,"daw":"reaper"}"#);
    assert_eq!(config(&["doctor", "--json", "--daw=renoise"], &env).unwrap(), r#"{"mode":"doctor","json":true,"daw":"renoise"}"#);
    assert_eq!(config(&["--daw", "ableton", "report"], &env).unwrap(), r#"{"mode":"report","daw":"ableton"}"#, "Live asked for decides");
    let mut chosen = env.clone();
    chosen.insert("KUMI_DAW".into(), "renoise".into());
    assert_eq!(config(&["report"], &chosen).unwrap(), r#"{"mode":"report","daw":"renoise"}"#);
    assert!(config(&["--daw", "logic", "doctor"], &env).unwrap_err().starts_with("Use: --daw <daw>"));
    // A KUMI_DAW Kumi doesn't know never stops the doctor or the report (the doctor says so).
    let mut wrong = env.clone();
    wrong.insert("KUMI_DAW".into(), "bitwig".into());
    assert_eq!(config(&["doctor"], &wrong).unwrap(), r#"{"mode":"doctor","json":false}"#);
    assert_eq!(config(&["report"], &wrong).unwrap(), r#"{"mode":"report"}"#);
    assert!(config(&["doctor", "--verbose"], &env).is_err());
}

/// "live" is another name for Live's DAW, as /daw live takes it: `--daw live` and KUMI_DAW=live choose Live, and nothing
/// writes it (review L10).
#[test]
fn live_is_another_name_for_ableton() {
    let env = env();
    for asked in [vec!["--daw", "live"], vec!["--daw=live"], vec!["--daw", "ableton"]] {
        assert_eq!(config(&asked, &env), config(&[], &env), "{asked:?}");
    }
    let mut live = env.clone();
    live.insert("KUMI_DAW".into(), "live".into());
    assert_eq!(config(&[], &live), config(&[], &env));
    assert_eq!(config(&["bridge"], &live), config(&["bridge"], &env));
    assert_eq!(config(&["--daw", "live", "doctor"], &env).unwrap(), r#"{"mode":"doctor","json":false,"daw":"ableton"}"#);
}

/// A bridge configuration given (`--bridge-config <Live's file>`) starts Live whatever KUMI_DAW says; --daw still says
/// whose it is (review L10).
#[test]
fn a_bridge_configuration_given_is_lives_unless_daw_says_whose() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("bridge.json");
    std::fs::write(&file, "{}").unwrap();
    let path = file.to_string_lossy().into_owned();
    let mut chosen = env();
    chosen.insert("KUMI_DAW".into(), "reaper".into());
    let started = config(&["--bridge-config", &path], &chosen).unwrap();
    assert!(started.contains(r#""mode":"live""#) && !started.contains(r#""daw""#), "{started}");
    let asked = config(&["--daw", "reaper", "--bridge-config", &path], &env()).unwrap();
    assert!(asked.contains(r#""daw":"reaper""#), "{asked}");
}

/// A start with several bridges asks every DAW whether it runs (choosing, then saying which others are open) from one
/// process listing, not one per DAW (review L11).
#[test]
fn every_daw_is_asked_whether_it_runs_from_one_process_listing() {
    let before = kumi::daw::process_listings();
    for provider in kumi::daw::providers() {
        provider.running_now();
    }
    let env = env();
    let chosen = kumi::daw::chosen_with(None, &env, &|provider| provider.running_now()).0.id();
    kumi::daw::also_running(chosen, &env, &|provider| provider.running_now());
    for provider in kumi::daw::providers() {
        provider.running_now();
    }
    assert_eq!(kumi::daw::process_listings() - before, 1, "one listing for every DAW");
}
