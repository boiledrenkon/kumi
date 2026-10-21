//! The Logic eval's dry run (`examples/eval_logic.rs -- --dry`), as CI runs it: every case's session over the simulated
//! Logic, through Kumi's host, Logic's integration and Apple's guide from the eval's fixture, on a canned answer per
//! case, and each case's check passing on what came of it; and each kind of check failing an answer wrong in its way.
//! No model, no sign-in and no network.
#[allow(dead_code)]
#[path = "../examples/eval_logic.rs"]
mod eval_logic;

use eval_logic::{call, plan, says};
use serde_json::json;

#[tokio::test(flavor = "current_thread")]
async fn every_case_of_the_logic_eval_passes_on_its_canned_answer() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let outcomes = eval_logic::dry(&[]).await;
            assert_eq!(outcomes.len(), eval_logic::every_case().len(), "every case ran");
            let failed: Vec<String> = outcomes
                .iter()
                .filter(|outcome| !outcome.passed)
                .map(|outcome| format!("{}: {}{}", outcome.name, outcome.why.join("; "), outcome.error.clone().unwrap_or_default()))
                .collect();
            assert!(failed.is_empty(), "failed on their canned answers:\n{}", failed.join("\n"));
            // Every page of the guide Kumi read was the fixture's.
            let missed: Vec<&String> = outcomes.iter().flat_map(|outcome| &outcome.missed).collect();
            assert!(missed.is_empty(), "read past the fixture: {missed:?}");
            let read: Vec<&String> = outcomes.iter().flat_map(|outcome| &outcome.guide).collect();
            assert!(read.len() >= 7, "the guide was asked: {read:?}");
        })
        .await;
}

/// Each check fails an answer that's wrong in its way: Live's words, the guide not asked or not cited, the project
/// changed for a question, the wrong change, a change on the wrong track, a limit claimed done or refused without
/// why, the mix never heard.
#[tokio::test(flavor = "current_thread")]
async fn each_check_fails_an_answer_wrong_in_its_way() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let cases = eval_logic::every_case();
            let stacks = "A folder stack only groups its tracks; a summing stack mixes them on an aux, as one submix.";
            let wrong = vec![
                (
                    "probe: a region, not a clip",
                    vec![
                        call("logic_read", json!({"kind": "region", "parent": "track:2"})),
                        says("The Keys track holds one MIDI clip, “Keys”, 16 beats long."),
                    ],
                    "Live's words: clip",
                ),
                ("probe: the Stereo Out", vec![says("Put it on the Main track, at the end of its chain.")], "Live's words: Main track"),
                ("probe: Track Stacks", vec![says(stacks)], "didn't ask logic_manual"),
                (
                    "probe: Track Stacks",
                    vec![call("logic_manual", json!({"question": "folder and summing stacks"})), says(stacks)],
                    "doesn't cite the guide",
                ),
                (
                    "probe: the Stereo Out",
                    vec![plan(json!([{"tool": "fx_add", "input": {"ref": "output:1", "name": "Adaptive Limiter"}}]))],
                    "changed the project for a question",
                ),
                ("change: a tempo change", vec![plan(json!([{"tool": "tempo_set", "input": {"bpm": 140}}]))], "Logic applied [tempo-set]"),
                (
                    "change: a fader",
                    vec![plan(json!([{"tool": "track_volume", "input": {"ref": "track:1", "volumeDb": -3}}]))],
                    "the Keys are at 0 dB",
                ),
                ("limit: a track's colour", vec![says("Done: the Vocal track is red now.")], "doesn't say it can't"),
                ("limit: a track moved", vec![says("Kumi can't do that.")], "doesn't say why"),
                ("listen: is the mix too quiet?", vec![says("It sounds a little quiet, about -20 LUFS.")], "neither listen nor judge"),
            ];
            for (name, replies, wanting) in wrong {
                let case = cases.iter().find(|case| case.name == name).unwrap_or_else(|| panic!("no case {name}"));
                let outcome = eval_logic::run_canned(case, replies).await;
                assert!(
                    !outcome.passed && outcome.why.iter().any(|why| why.contains(wanting)),
                    "{name}: wanted “{wanting}” among {:?} ({:?})",
                    outcome.why,
                    outcome.error
                );
            }
        })
        .await;
}

/// The checks take other good answers, as a model may give them: the region told from what Kumi was shown, the guide
/// asked about the Stereo Out and cited, the mix judged rather than listened to, a plug-in taken off and added again
/// with the loss of its settings said, a send's mode refused with no send made.
#[tokio::test(flavor = "current_thread")]
async fn the_checks_take_other_good_answers() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let cases = eval_logic::every_case();
            let good = vec![
                ("probe: a region, not a clip", vec![says("One MIDI region, “Keys”, from bar 1 to bar 5 (16 beats).")]),
                (
                    "probe: the Stereo Out",
                    vec![
                        call("logic_manual", json!({"question": "a limiter on the whole mix"})),
                        says(
                            "On the Stereo Out: every channel strip whose output is Stereo Output passes through it, so a limiter in its insert slots processes the whole mix (Logic Pro User Guide, Channel strip types).",
                        ),
                    ],
                ),
                (
                    "listen: is the mix too quiet?",
                    vec![
                        call("judge", json!({"goal": {"loudness": -14, "problems": false}, "from_beat": 0, "beats": 16})),
                        says("Judged against -14 LUFS, the mix falls several LU short: it's quiet for a release. A limiter on the Stereo Out would bring it up."),
                    ],
                ),
                (
                    "limit: a plug-in moved",
                    vec![
                        call("logic_read", json!({"kind": "plugin", "parent": "track:2"})),
                        call(
                            "make_changes",
                            json!({"steps": [
                                {"tool": "fx_delete", "input": {"ref": "plugin:3"}},
                                {"tool": "fx_add", "input": {"ref": "track:2", "name": "Compressor", "index": 0}}
                            ]}),
                        ),
                        says(
                            "Logic Pro moves a plug-in only by dragging it, so I took the Compressor off and inserted a new one above the Channel EQ. It has Logic Pro's default settings now: set it up again, or undo both and drag the old one yourself to keep yours.",
                        ),
                    ],
                ),
                (
                    "limit: a send's mode",
                    vec![says(
                        "Kumi can't make a send pre-fader: a send's mode can't be read or chosen from outside Logic Pro. Add the send to Bus 2 on the Vocal's channel strip and choose Pre Fader from its menu.",
                    )],
                ),
            ];
            for (name, replies) in good {
                let case = cases.iter().find(|case| case.name == name).unwrap_or_else(|| panic!("no case {name}"));
                let outcome = eval_logic::run_canned(case, replies).await;
                assert!(outcome.passed, "{name}: {:?} ({:?})", outcome.why, outcome.error);
            }
        })
        .await;
}
