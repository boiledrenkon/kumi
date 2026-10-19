//! `--daw <id>` and KUMI_DAW choose the DAW a session works with, among the DAWs this Kumi integrates (Live, REAPER,
//! Renoise and Logic Pro). Choosing Live is what Kumi does anyway: the configuration reads, and is written, exactly as
//! without it.
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
        assert_eq!(config(&asked, &env), Err("Use: --daw <daw>, with daw one of ableton, reaper, renoise, logic.".into()), "{asked:?}");
    }
    let mut chosen = env.clone();
    chosen.insert("KUMI_DAW".into(), "bitwig".into());
    assert_eq!(config(&[], &chosen), Err("KUMI_DAW must be one of: ableton, reaper, renoise, logic.".into()));
    // --daw beats KUMI_DAW.
    assert_eq!(config(&["--daw", "ableton"], &chosen), config(&[], &env));
}

#[test]
fn reaper_renoise_or_logic_chosen_is_written_and_read_back() {
    let env = env();
    for (asked, id) in [(vec!["--daw", "reaper"], "reaper"), (vec!["--daw=renoise"], "renoise"), (vec!["--daw", "logic"], "logic")] {
        let config: serde_json::Value = serde_json::from_str(&config(&asked, &env).unwrap()).unwrap();
        assert_eq!(config["daw"], id, "{asked:?}");
    }
    let mut chosen = env.clone();
    chosen.insert("KUMI_DAW".into(), "renoise".into());
    let config: serde_json::Value = serde_json::from_str(&config(&[], &chosen).unwrap()).unwrap();
    assert_eq!(config["daw"], "renoise");
}

/// With no DAW's bridge installed and none asked for, Kumi chats without a DAW in Live's words: Live stays the DAW Kumi
/// starts with. REAPER, Renoise and Logic Pro are chosen with --daw or KUMI_DAW, or by their bridge being installed.
#[test]
fn without_any_bridge_kumi_chats_in_lives_words() {
    use kumi::daw::{chosen, providers};
    use kumi_runtime::core::daw::{DawId, DawProfile};
    let env = env();
    assert_eq!(providers().map(|provider| provider.id()), [DawId::Ableton, DawId::Reaper, DawId::Renoise, DawId::Logic], "Live first");
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
    let mut chosen = env.clone();
    chosen.insert("KUMI_DAW".into(), "renoise".into());
    assert!(config(&["bridge"], &chosen).unwrap().contains(r#""daw":"renoise""#));
    assert_eq!(
        config(&["--daw", "logic", "bridge", "--yes"], &env).unwrap(),
        r#"{"mode":"bridge","yes":true,"allowDirty":false,"daw":"logic"}"#,
        "Logic's bridge is set up the same way"
    );
    assert!(config(&["--daw", "bitwig", "bridge"], &env).unwrap_err().starts_with("Use: --daw <daw>"));
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
    assert_eq!(config(&["--daw", "logic", "doctor"], &env).unwrap(), r#"{"mode":"doctor","json":false,"daw":"logic"}"#);
    assert!(config(&["--daw", "bitwig", "doctor"], &env).unwrap_err().starts_with("Use: --daw <daw>"));
    // A KUMI_DAW Kumi doesn't know never stops the doctor or the report (the doctor says so).
    let mut wrong = env.clone();
    wrong.insert("KUMI_DAW".into(), "bitwig".into());
    assert_eq!(config(&["doctor"], &wrong).unwrap(), r#"{"mode":"doctor","json":false}"#);
    assert_eq!(config(&["report"], &wrong).unwrap(), r#"{"mode":"report"}"#);
    assert!(config(&["doctor", "--verbose"], &env).is_err());
}
