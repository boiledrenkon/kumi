//! Actions by id or name, track templates, and saving (r-tracks, M4).

use serde_json::json;

use super::tests_lab::*;
use crate::reaper::fake::{self, FakeTrack};

/// The fake REAPER's actions for these tests: insert track and mute mark a change of REAPER's own (an undo point); unselect
/// all doesn't. Each records the tracks selected as it runs.
fn actions(command: i32) {
    let selected: Vec<String> = fake::with(|s| s.tracks.iter().filter(|t| t.selected).map(|t| t.name.clone()).collect());
    fake::with(|s| s.ext_state.insert(("test".into(), format!("selected {command}")), selected.join(",")));
    match command {
        40001 => {
            fake::with(|s| s.tracks.push(FakeTrack::new("{ACTION-NEW}", "")));
            fake::mark_changed();
        }
        40280 => {
            fake::with(|s| {
                for t in s.tracks.iter_mut().filter(|t| t.selected) {
                    t.mute = !t.mute;
                }
            });
            fake::mark_changed();
        }
        40297 => fake::with(|s| s.tracks.iter_mut().for_each(|t| t.selected = false)),
        _ => {}
    }
}

fn selected_when(command: i32) -> String {
    state(|s| s.ext_state.get(&("test".to_string(), format!("selected {command}"))).cloned().unwrap_or_default())
}

#[test]
fn an_action_by_id_or_name_runs_in_one_block_and_only_reapers_undo_takes_it_back() {
    let lab = Lab::new();
    state(|s| s.on_action = Some(actions));
    for (n, action) in [json!(40001), json!("Track: Insert new track"), json!("track: insert NEW track")].into_iter().enumerate() {
        let tracks = state(|s| s.tracks.len());
        let (preview, made) = lab.make("run-action", json!({"action": action}), &format!("Kumi: Ran action (ka00{n})"));
        assert_eq!(preview["display"]["title"], "Ran action “Track: Insert new track”");
        assert_eq!(state(|s| s.tracks.len()), tracks + 1);
        assert_eq!((made["undo"]["entry"].as_bool(), made["display"].get("unchanged")), (Some(true), None));
        assert_eq!(made["display"]["note"], "Only REAPER's own undo can take this back (Edit > Undo).");
        assert_eq!(top().as_deref(), Some(format!("Kumi: Ran action (ka00{n})").as_str()));
        let (code, _) = lab.undo("run-action", &preview, &made, "Kumi: undo (kb000)").unwrap_err();
        assert_eq!(code, "daw-only");
    }
    // A named command.
    let preview = lab.preview("run-action", json!({"action": "_RS_kumi"})).unwrap();
    assert_eq!(preview["display"]["title"], "Ran action “Script: kumi.lua”");
}

#[test]
fn an_action_that_changes_nothing_reaper_keeps_makes_no_point_and_says_it_has_no_row() {
    let lab = Lab::new();
    state(|s| s.on_action = Some(actions));
    fake::select_tracks(&[1]);
    let entries_before = entries();
    let (_, made) = lab.make("run-action", json!({"action": 40297}), "Kumi: Ran action (kn001)");
    assert_eq!((made["undo"]["entry"].as_bool(), made["display"]["unchanged"].as_bool()), (Some(false), Some(true)));
    assert_eq!(made["applied"]["changed"], false);
    assert!(made["display"]["note"].as_str().unwrap().contains("no row in HISTORY"));
    assert_eq!(entries(), entries_before, "no point of REAPER's, and the producer's pending edits not committed under Kumi's name");
}

#[test]
fn an_action_runs_on_the_tracks_and_items_kumi_selects_and_the_producers_selection_comes_back() {
    let lab = Lab::new();
    state(|s| s.on_action = Some(actions));
    fake::select_tracks(&[2, 3]);
    state(|s| s.tracks[2].items[0].selected = true);
    lab.make(
        "run-action",
        json!({"action": "Track: Mute/unmute tracks", "trackRefs": [KICK], "itemRefs": ["r:item:{ITEM-K1}"]}),
        "Kumi: x (ks001)",
    );
    assert_eq!(selected_when(40280), "Kick");
    assert_eq!(state(|s| s.tracks.iter().map(|t| t.mute).collect::<Vec<_>>()), [false, true, false, false]);
    assert_eq!(state(|s| s.tracks.iter().map(|t| t.selected).collect::<Vec<_>>()), [false, false, true, true], "the producer's tracks");
    assert_eq!(state(|s| (s.tracks[1].items[0].selected, s.tracks[2].items[0].selected)), (false, true), "and items");
    assert_eq!(lab.preview("run-action", json!({"action": 40280, "trackRefs": ["r:item:{ITEM-K1}"]})).unwrap_err().0, "bad-params");
}

#[test]
fn dialogs_quitting_closing_saving_and_rendering_are_never_run_as_a_change() {
    let lab = Lab::new();
    for action in
        [json!(40392), json!(40004), json!(40860), json!(40886), json!(41824), json!(40026), json!(41895), json!("Options: Preferences...")]
    {
        let (code, message) = lab.preview("run-action", json!({"action": action})).unwrap_err();
        assert_eq!(code, "bad-params", "{action}");
        assert!(message.contains("Kumi won't run it"), "{action}: {message}");
    }
    let (code, message) = lab.preview("run-action", json!({"action": "Track: Insert new trak"})).unwrap_err();
    assert_eq!(code, "bad-params");
    assert!(message.contains("no action named") && message.contains("“Track: Insert new track”"), "{message}");
    assert_eq!(lab.preview("run-action", json!({"action": 99999})).unwrap_err().0, "bad-params");
    assert_eq!(lab.preview("run-action", json!({"action": "_NOPE"})).unwrap_err().0, "bad-params");
}

#[test]
fn an_action_in_a_plan_is_covered_by_the_plans_point() {
    let lab = Lab::new();
    state(|s| s.on_action = Some(actions));
    let step = lab.call("undo_step_begin", json!({"label": "Kumi: plan (kp001)"})).unwrap();
    let (_, made) = lab.make("run-action", json!({"action": 40001}), "Kumi: Ran action (kp002)");
    assert_eq!((made["undo"]["inPlan"].as_bool(), made["display"].get("unchanged")), (Some(true), None));
    lab.call("undo_step_end", json!({"stepId": step["stepId"]})).unwrap();
    assert_eq!(top().as_deref(), Some("Kumi: plan (kp001)"));
}

// ----- track templates -----

/// A resource folder with a template of two tracks (a folder and its child) and one that opens a folder it doesn't close.
fn templates() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("TrackTemplates/Drums")).unwrap();
    std::fs::write(
        dir.path().join("TrackTemplates/Drums/Kit.RTrackTemplate"),
        "<TRACK\n  NAME \"Kit Bus\"\n  ISBUS 1 1\n>\n<TRACK\n  NAME \"Kit Kick\"\n  ISBUS 2 -1\n>\n",
    )
    .unwrap();
    std::fs::write(dir.path().join("TrackTemplates/Open.RTrackTemplate"), "<TRACK\n  NAME \"Open\"\n  ISBUS 1 1\n>\n").unwrap();
    state(|s| s.resource = dir.path().to_string_lossy().into_owned());
    dir
}

fn names() -> Vec<String> {
    state(|s| s.tracks.iter().map(|t| t.name.clone()).collect())
}

#[test]
fn a_track_template_goes_in_at_its_place_and_its_undo_takes_those_tracks_away() {
    let lab = Lab::new();
    let _dir = templates();
    fake::select_tracks(&[3]);
    let (preview, made) = lab.make("track-template-insert", json!({"name": "Drums/Kit", "index": 2}), "Kumi: template (kt001)");
    assert_eq!(made["display"]["title"], "Inserted track template “Kit” (2 tracks) at position 2");
    assert_eq!(preview["display"]["note"], "Its tracks: Kit Bus, Kit Kick.");
    assert_eq!(names(), ["Drums", "Kit Bus", "Kit Kick", "Kick", "Snare", "Reverb"]);
    assert_eq!(made["created"]["trackRefs"], json!(["r:track:{TPL-1}", "r:track:{TPL-2}"]));
    assert_eq!(made["created"]["ref"], "r:track:{TPL-1}");
    assert_eq!(
        state(|s| s.tracks.iter().map(|t| t.selected).collect::<Vec<_>>()),
        [false, false, false, false, false, true],
        "the producer's selection"
    );
    lab.undo("track-template-insert", &preview, &made, "Kumi: undo (kt002)").unwrap();
    assert_eq!(names(), ["Drums", "Kick", "Snare", "Reverb"]);
    // At the end by default.
    lab.make("track-template-insert", json!({"name": "Drums/Kit.RTrackTemplate"}), "Kumi: template (kt003)");
    assert_eq!(names()[4..], ["Kit Bus", "Kit Kick"]);
}

#[test]
fn a_template_that_isnt_there_or_opens_a_folder_it_doesnt_close_is_refused_with_nothing_left() {
    let lab = Lab::new();
    let dir = templates();
    assert_eq!(lab.preview("track-template-insert", json!({"name": "Nope"})).unwrap_err().0, "failed");
    assert_eq!(lab.preview("track-template-insert", json!({"name": "../Kit"})).unwrap_err().0, "bad-params");
    assert_eq!(lab.preview("track-template-insert", json!({"path": "relative/Kit.RTrackTemplate"})).unwrap_err().0, "bad-params");
    let path = dir.path().join("TrackTemplates/Open.RTrackTemplate").to_string_lossy().into_owned();
    let input = json!({"path": path, "index": 1});
    let preview = lab.preview("track-template-insert", input.clone()).unwrap();
    let (code, message) = lab.apply("track-template-insert", &input, &preview, "Kumi: x (ko001)").unwrap_err();
    assert_eq!(code, "bad-params");
    assert!(message.contains("open a folder they don't close"), "{message}");
    assert_eq!(names(), ["Drums", "Kick", "Snare", "Reverb"], "what REAPER made is taken away again");
    // The template file changed after the preview.
    let input = json!({"name": "Drums/Kit"});
    let preview = lab.preview("track-template-insert", input.clone()).unwrap();
    std::fs::write(dir.path().join("TrackTemplates/Drums/Kit.RTrackTemplate"), "<TRACK\n  NAME \"Other\"\n>\n").unwrap();
    assert_eq!(lab.apply("track-template-insert", &input, &preview, "Kumi: x (ko002)").unwrap_err().0, "changed");
}

#[test]
fn a_template_insert_isnt_undone_once_the_producer_changed_its_tracks_or_moved_one_into_its_folder() {
    let lab = Lab::new();
    let _dir = templates();
    let (preview, made) = lab.make("track-template-insert", json!({"name": "Drums/Kit"}), "Kumi: template (kp001)");
    state(|s| s.tracks[5].name = "My Kick".into());
    let (code, message) = lab.undo("track-template-insert", &preview, &made, "Kumi: undo (kp002)").unwrap_err();
    assert_eq!(code, "changed");
    assert!(message.contains("“My Kick”"), "{message}");
    state(|s| s.tracks[5].name = "Kit Kick".into());
    // The producer drags Reverb in under the template's folder.
    state(|s| {
        let reverb = s.tracks.remove(3);
        s.tracks.insert(4, reverb);
        s.tracks[5].folder = 0;
        s.tracks[4].folder = 0;
        s.tracks[3].folder = 1;
        s.tracks[5].folder = -1;
    });
    let (code, _) = lab.undo("track-template-insert", &preview, &made, "Kumi: undo (kp003)").unwrap_err();
    assert_eq!(code, "changed");
}

// ----- saving -----

#[test]
fn a_saved_project_is_saved_in_place_or_as_its_next_version() {
    let lab = Lab::new();
    state(|s| s.dirty = true);
    let saved = lab.call("project_save", json!({})).unwrap();
    assert_eq!((saved["saved"].as_str(), state(|s| s.saved)), (Some("/lab/Projects/Song.rpp"), 1));
    lab.call("project_save", json!({"version": true})).unwrap();
    assert_eq!(state(|s| s.actions.clone()), [41895]);
    state(|s| s.save_fails = true);
    assert_eq!(lab.call("project_save", json!({})).unwrap_err().0, "failed");
    assert_eq!(lab.call("project_save", json!({"path": "/lab/Projects/Other.rpp"})).unwrap_err().0, "bad-params");
}

#[test]
fn a_project_never_saved_is_saved_only_in_the_producers_project_folder_and_never_over_a_file() {
    let lab = Lab::new();
    let dir = tempfile::tempdir().unwrap();
    let folder = dir.path().join("Projects");
    fake::extra(|e| e.config.insert("defsavepath".into(), folder.to_string_lossy().into_owned()));
    state(|s| s.project = Some((String::new(), String::new())));
    let (code, message) = lab.call("project_save", json!({})).unwrap_err();
    assert_eq!(code, "failed");
    assert!(message.contains("never saved") && message.contains("never opens Save As"), "{message}");
    let outside = dir.path().join("Elsewhere/Song.rpp").to_string_lossy().into_owned();
    assert_eq!(lab.call("project_save", json!({"path": outside})).unwrap_err().0, "bad-params");
    let inside = folder.join("Night Drive/Night Drive.rpp");
    let saved = lab.call("project_save", json!({"path": inside.to_string_lossy()})).unwrap();
    assert_eq!(saved["saved"].as_str(), Some(inside.to_string_lossy().as_ref()));
    assert!(inside.exists());
    state(|s| s.project = Some((String::new(), String::new())));
    let (code, message) = lab.call("project_save", json!({"path": inside.to_string_lossy()})).unwrap_err();
    assert_eq!(code, "bad-params");
    assert!(message.contains("never saves over a file"), "{message}");
}

#[test]
fn tracks_are_saved_as_a_template_never_over_a_file_unless_told_and_the_selection_comes_back() {
    let lab = Lab::new();
    let dir = templates();
    fake::select_tracks(&[3]);
    let saved = lab.call("track_template_save", json!({"trackRefs": [KICK, SNARE], "name": "Mine/Two"})).unwrap();
    let file = dir.path().join("TrackTemplates/Mine/Two.RTrackTemplate");
    assert_eq!(saved["saved"].as_str(), Some(file.to_string_lossy().as_ref()));
    assert_eq!(saved["tracks"], json!(["Kick", "Snare"]));
    let text = std::fs::read_to_string(&file).unwrap();
    assert!(text.contains("\"Kick\"") && text.contains("\"Snare\"") && !text.contains("Reverb"), "{text}");
    assert_eq!(state(|s| s.tracks.iter().map(|t| t.selected).collect::<Vec<_>>()), [false, false, false, true]);
    let (code, message) = lab.call("track_template_save", json!({"trackRefs": [KICK], "name": "Mine/Two"})).unwrap_err();
    assert_eq!(code, "bad-params");
    assert!(message.contains("replace: true"), "{message}");
    lab.call("track_template_save", json!({"trackRefs": [KICK], "name": "Mine/Two", "replace": true})).unwrap();
    assert!(!std::fs::read_to_string(&file).unwrap().contains("Snare"));
    assert_eq!(fake::extra(|e| e.saved_as.last().map(|(_, options)| *options)), Some(1 | 4));
    for wrong in [
        json!({"trackRefs": [KICK], "name": "../x"}),
        json!({"trackRefs": ["r:track:master"], "name": "x"}),
        json!({"trackRefs": [], "name": "x"}),
    ] {
        assert_eq!(lab.call("track_template_save", wrong.clone()).unwrap_err().0, "bad-params", "{wrong}");
    }
}
