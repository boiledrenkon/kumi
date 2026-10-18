//! `--daw <id>` and KUMI_DAW choose the DAW a session works with, among the DAWs this Kumi integrates (Live alone so
//! far). Choosing Live is what Kumi does anyway: the configuration reads, and is written, exactly as without it.
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
    for asked in [vec!["--daw", "reaper"], vec!["--daw=renoise"], vec!["--daw"], vec!["--daw", "Live"], vec!["--daw", "ABLETON"]] {
        assert_eq!(config(&asked, &env), Err("Use: --daw <daw>, with daw one of ableton.".into()), "{asked:?}");
    }
    let mut chosen = env.clone();
    chosen.insert("KUMI_DAW".into(), "reaper".into());
    assert_eq!(config(&[], &chosen), Err("KUMI_DAW must be one of: ableton.".into()));
    // --daw beats KUMI_DAW.
    assert_eq!(config(&["--daw", "ableton"], &chosen), config(&[], &env));
}

/// With no DAW's bridge installed and none asked for, Kumi chats without a DAW in Live's words: Live is the only DAW
/// registered. A second DAW has to decide this on purpose (the DAW used last, or asking), so this pins it.
#[test]
fn without_any_bridge_kumi_chats_in_lives_words_while_live_is_the_only_daw() {
    use kumi::daw::{chosen, providers};
    use kumi_runtime::core::daw::{DawId, DawProfile};
    let env = env();
    assert_eq!(providers().map(|provider| provider.id()), [DawId::Ableton], "a second DAW decides the fallback on purpose");
    let (provider, found) = chosen(None, &env);
    assert_eq!((provider.id(), found), (DawId::Ableton, None));
    let without = provider.without_daw(std::rc::Rc::new(|_| {}));
    // Live's words and vocabulary, with none of Live's capabilities while there's no Live to use them on.
    assert_eq!((without.profile().id, without.profile().words), (Some(DawId::Ableton), DawProfile::ABLETON.words));
    assert!(*without.profile() == DawProfile::ABLETON.without_caps());
    let config = serde_json::to_value(load_config(&[], &env).unwrap()).unwrap();
    assert_eq!((config["mode"].as_str(), config["bridgeMissing"].as_bool(), config.get("daw")), (Some("inference-only"), Some(true), None));
}
