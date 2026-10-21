//! What REAPER's and Renoise's turns tell the model (each DAW's instructions, with its pack of power users' rules, and its
//! tools' descriptions): held to the budget Live's instructions keep, in the DAW's own words, and claiming nothing the
//! DAW lacks.
use super::own_bridge::{instructions_of, OwnDaw};
use super::{reaper, renoise};
use regex::Regex;

const DAWS: [&OwnDaw; 2] = [&reaper::DAW, &renoise::DAW];

/// Live's words, whole (as the app's own leak tests have them).
fn live_words() -> Regex {
    Regex::new(r"\b(Live|Ableton|Sets?|Session|Arrangement|Main|[Ss]cenes?|[Rr]acks?|[Cc]lip slots?|Control Surfaces?|Remote Scripts?|Max for Live|M4L|Operator)\b").unwrap()
}

/// Everything a DAW's turns tell the model about it.
fn texts(daw: &OwnDaw) -> Vec<(String, String)> {
    let changes = daw.changes;
    let mut texts = vec![
        ("instructions".to_string(), instructions_of(daw)),
        (daw.read_tool.to_string(), daw.read_description.to_string()),
        ("make_changes".to_string(), changes.plan_description.to_string()),
        ("undo_change".to_string(), changes.undo_description.to_string()),
    ];
    texts.extend(changes.kinds.iter().map(|kind| (kind.tool.to_string(), kind.description.to_string())));
    texts.extend(changes.actions.iter().map(|action| (action.tool.to_string(), action.description.to_string())));
    texts
}

#[test]
fn each_daws_instructions_keep_within_lives() {
    let live = super::ableton::context::INSTRUCTIONS.len();
    for daw in DAWS {
        let text = instructions_of(daw);
        assert!(text.len() <= live, "{}'s instructions are {} bytes, over Live's {live}", daw.profile.words.app, text.len());
    }
}

#[test]
fn reaper_and_renoise_are_told_of_in_their_own_words() {
    let live = live_words();
    for daw in DAWS {
        for (place, text) in texts(daw) {
            let found: Vec<&str> = live.find_iter(&text).map(|m| m.as_str()).collect();
            assert!(found.is_empty(), "{}'s {place} speaks Live's words: {found:?}", daw.profile.words.app);
        }
    }
}

/// What each DAW doesn't have: its instructions may name one only to say it isn't there.
const LACKS: [(&str, &[&str]); 2] = [
    ("REAPER", &["session view", "clip launcher", "pattern editor", "phrase editor", "drum rack", "simpler"]),
    ("Renoise", &["piano roll", "audio track", "clap", "rewire", "lv2", "clip launcher", "session view"]),
];

#[test]
fn the_instructions_name_what_a_daw_lacks_only_as_lacking() {
    let negation = Regex::new(r"\b(no|never|not|without|lacks|can't|isn't|doesn't|has no)\b").unwrap();
    for daw in DAWS {
        let app = daw.profile.words.app;
        let lacks = LACKS.iter().find(|(name, _)| *name == app).expect("each DAW's list").1;
        let text = instructions_of(daw).to_lowercase();
        for word in lacks {
            for (at, _) in text.match_indices(word) {
                let before: Vec<&str> = text[..at].split_whitespace().rev().take(6).collect();
                assert!(
                    before.iter().any(|w| negation.is_match(w)),
                    "{app}'s instructions name “{word}” as if {app} had it: …{}",
                    &text[at.saturating_sub(80)..(at + word.len() + 20).min(text.len())]
                );
            }
        }
    }
}

/// What the lab found wrong in the research the packs came from: never told to the model.
#[test]
fn what_the_lab_found_wrong_stays_out() {
    let reaper = instructions_of(&reaper::DAW);
    // REAPER 7.82 renders $tracknameornumber as "Master" and the letters "ornumber" (KP3).
    assert!(!reaper.contains("$tracknameornumber"));
    // REAPER does ship instruments (ReaSynth, ReaSynDr, ReaSamplOmatic5000).
    assert!(!reaper.contains("no instruments") && !reaper.contains("no stock instrument"));
    let renoise = instructions_of(&renoise::DAW);
    // Renoise 3.5.4 takes a #Send aimed at its own track without an error (KN7): it isn't refused.
    assert!(!Regex::new(r"(?i)send[^.]*\brefuse").unwrap().is_match(&renoise));
}

#[test]
fn the_packs_carry_what_the_lab_checked() {
    for (daw, words) in [
        (&reaper::DAW, &["razor area", "$parenttrack", "$timelineorder", "-001", "ReaSynDr", "ACX", "dynamic split"][..]),
        (&renoise::DAW, &["0S40", "Autoseek", "8 macros", "0Zxx", "no CLAP", "no ReWire", "Bus Compressor"][..]),
    ] {
        let text = instructions_of(daw);
        for word in words {
            assert!(text.contains(word), "{}'s instructions say {word}", daw.profile.words.app);
        }
    }
    let lua = |daw: &OwnDaw| daw.changes.kinds.iter().find(|kind| kind.tool == "run_lua").unwrap().description;
    assert!(lua(&reaper::DAW).contains("APIExists") && lua(&reaper::DAW).contains("NamedCommandLookup"));
    assert!(lua(&renoise::DAW).contains("postfx_volume") && lua(&renoise::DAW).contains("load_plugin"));
}
