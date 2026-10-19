//! The project beyond its tracks (r-tracks, M4): the time selection, notes, title and author, grid, timebase, ruler lanes,
//! a marker's lane and a region's render matrix.

use serde_json::json;

use super::tests_lab::*;
use crate::reaper::fake;

fn time_selection() -> (f64, f64) {
    state(|s| s.time_selection)
}

#[test]
fn the_time_selection_is_made_cleared_and_put_back_and_refused_when_it_moved() {
    let lab = Lab::new();
    let (preview, made) = lab.make("time-selection", json!({"start": 2.0, "end": 6.0}), "Kumi: x (kt001)");
    assert_eq!(time_selection(), (2.0, 6.0));
    assert_eq!(made["undo"]["entry"], true);
    assert!(made["display"]["title"].as_str().unwrap().starts_with("Time selection bar"), "{}", made["display"]["title"]);
    lab.undo("time-selection", &preview, &made, "Kumi: undo (kt002)").unwrap();
    assert_eq!(time_selection(), (0.0, 0.0));
    // Moved by the producer after the preview: refused. After the apply: the undo is refused, discard puts it back.
    let input = json!({"start": 1.0, "end": 3.0});
    let preview = lab.preview("time-selection", input.clone()).unwrap();
    state(|s| s.time_selection = (4.0, 5.0));
    assert_eq!(lab.apply("time-selection", &input, &preview, "Kumi: x (kt003)").unwrap_err().0, "changed");
    assert_eq!(time_selection(), (4.0, 5.0));
    let (preview, made) = lab.make("time-selection", json!({"start": 0.0, "end": 0.0}), "Kumi: x (kt004)");
    assert_eq!(made["display"]["title"], "Time selection cleared");
    state(|s| s.time_selection = (7.0, 8.0));
    assert_eq!(lab.undo("time-selection", &preview, &made, "Kumi: undo (kt005)").unwrap_err().0, "changed");
    lab.undo_anyway("time-selection", &preview, &made, "Kumi: undo (kt006)").unwrap();
    assert_eq!(time_selection(), (4.0, 5.0));
    assert_eq!(lab.preview("time-selection", json!({"start": 3.0, "end": 1.0})).unwrap_err().0, "bad-params");
}

#[test]
fn the_notes_title_and_author_are_written_and_put_back_and_the_notes_say_reaper_wont() {
    let lab = Lab::new();
    fake::extra(|e| e.notes = "Mix notes".into());
    let (preview, made) = lab.make("project-notes", json!({"notes": "Bridge needs a riser\nVocal comp: take 3"}), "Kumi: x (kn001)");
    assert_eq!(fake::extra(|e| e.notes.clone()), "Bridge needs a riser\nVocal comp: take 3");
    assert_eq!(made["display"]["title"], "Project notes changed");
    assert_eq!(made["display"]["note"], "REAPER's own undo won't take back the project's notes; Kumi can.");
    lab.undo("project-notes", &preview, &made, "Kumi: undo (kn002)").unwrap();
    assert_eq!(fake::extra(|e| e.notes.clone()), "Mix notes");
    // The producer edits them after the apply: refused.
    let (preview, made) = lab.make("project-notes", json!({"notes": ""}), "Kumi: x (kn003)");
    assert_eq!(made["display"]["title"], "Project notes cleared");
    fake::extra(|e| e.notes = "Mine".into());
    assert_eq!(lab.undo("project-notes", &preview, &made, "Kumi: undo (kn004)").unwrap_err().0, "changed");
    // Title and author.
    let (preview, made) = lab.make("project-info", json!({"title": "Night Drive", "author": "K."}), "Kumi: x (kn005)");
    assert_eq!(made["display"]["title"], "Project title “Night Drive”, author “K.”");
    assert_eq!(fake::extra(|e| e.project_text.get("PROJECT_TITLE").cloned()).as_deref(), Some("Night Drive"));
    lab.undo("project-info", &preview, &made, "Kumi: undo (kn006)").unwrap();
    assert_eq!(fake::extra(|e| e.project_text.get("PROJECT_TITLE").cloned()).as_deref(), Some(""));
    assert_eq!(lab.preview("project-info", json!({"title": "two\nlines"})).unwrap_err().0, "bad-params");
}

#[test]
fn the_grid_takes_note_values_swing_and_the_measure_grid() {
    let lab = Lab::new();
    let grid = || fake::extra(|e| e.grid);
    let (preview, made) = lab.make("project-grid", json!({"division": "1/16", "swing": 0.3}), "Kumi: x (kg001)");
    assert_eq!(grid(), (0.0625, 1, 0.3));
    assert_eq!(made["display"]["title"], "Grid 1/16, swing 30%");
    assert_eq!(made["display"]["note"], "REAPER's own undo won't take back the grid; Kumi can.");
    lab.undo("project-grid", &preview, &made, "Kumi: undo (kg002)").unwrap();
    assert_eq!(grid(), (0.25, 0, 0.0));
    let (_, made) = lab.make("project-grid", json!({"division": "1/8T"}), "Kumi: x (kg003)");
    assert!((grid().0 - 0.125 * 2.0 / 3.0).abs() < 1e-12);
    assert_eq!(made["display"]["title"], "Grid 1/8 triplet");
    let (_, made) = lab.make("project-grid", json!({"division": "1/4.", "measure": true}), "Kumi: x (kg004)");
    assert_eq!((grid().0, grid().1), (0.375, 3));
    assert_eq!(made["display"]["title"], "Grid 1/4 dotted, the measure grid");
    for wrong in [json!({"division": "fast"}), json!({"division": 0}), json!({"swing": 2.0}), json!({})] {
        assert_eq!(lab.preview("project-grid", wrong.clone()).unwrap_err().0, "bad-params", "{wrong}");
    }
}

#[test]
fn the_projects_timebase_is_a_project_setting_put_back_by_kumi() {
    let lab = Lab::new();
    let config = |name: &str| fake::extra(|e| e.config.get(name).cloned()).unwrap();
    let (preview, made) = lab.make("project-timebase", json!({"items": "time", "tempoEnvelopes": "time"}), "Kumi: x (kb001)");
    assert_eq!((config("itemtimelock"), config("tempoenvtimelock")), ("0".into(), "0".into()));
    assert_eq!(made["display"]["title"], "Project timebase time, tempo envelope timebase time");
    lab.undo("project-timebase", &preview, &made, "Kumi: undo (kb002)").unwrap();
    assert_eq!((config("itemtimelock"), config("tempoenvtimelock")), ("1".into(), "1".into()));
    assert_eq!(lab.preview("project-timebase", json!({"tempoEnvelopes": "beats-position"})).unwrap_err().0, "bad-params");
}

#[test]
fn a_ruler_lane_is_named_hidden_and_put_back_by_its_id() {
    let lab = Lab::new();
    let (preview, made) = lab.make("ruler-lane", json!({"lane": 1, "name": "Sections", "hidden": true}), "Kumi: x (kl001)");
    assert_eq!(preview["target"]["guid"], "{LANE-1}");
    assert_eq!(made["display"]["title"], "Ruler lane 1 named “Sections”, hidden");
    assert_eq!(fake::extra(|e| e.project_text.get("RULER_LANE_NAME:1").cloned()).as_deref(), Some("Sections"));
    assert_eq!(fake::extra(|e| e.project_info.get("RULER_LANE_HIDDEN:1").copied()), Some(1.0));
    lab.undo("ruler-lane", &preview, &made, "Kumi: undo (kl002)").unwrap();
    assert_eq!(fake::extra(|e| e.project_text.get("RULER_LANE_NAME:1").cloned()).as_deref(), Some(""));
    // A lane that isn't there any more: gone.
    let (preview, made) = lab.make("ruler-lane", json!({"lane": 1, "locked": true}), "Kumi: x (kl003)");
    fake::extra(|e| e.project_info.insert("RULER_LANE_COUNT".into(), 1.0));
    assert_eq!(lab.undo("ruler-lane", &preview, &made, "Kumi: undo (kl004)").unwrap_err().0, "gone");
    assert_eq!(lab.preview("ruler-lane", json!({"lane": 4, "name": "x"})).unwrap_err().0, "bad-params");
}

#[test]
fn a_ruler_lane_is_added_and_taken_away_again_only_while_nothing_is_on_it() {
    let lab = Lab::new();
    let lanes = || fake::extra(|e| e.project_info.get("RULER_LANE_COUNT").copied()).unwrap();
    let (preview, made) = lab.make("ruler-lane-add", json!({"name": "Hits"}), "Kumi: x (ka001)");
    assert_eq!((lanes(), made["created"]["guid"].as_str()), (3.0, Some("{LANE-2}")));
    assert_eq!(made["display"]["title"], "Added ruler lane 2 named “Hits”");
    lab.undo("ruler-lane-add", &preview, &made, "Kumi: undo (ka002)").unwrap();
    assert_eq!(lanes(), 2.0);
    // A marker the producer put on it keeps it.
    let (preview, made) = lab.make("ruler-lane-add", json!({}), "Kumi: x (ka003)");
    state(|s| s.marker_lanes.insert("{MARK-1}".into(), 2));
    let (code, message) = lab.undo("ruler-lane-add", &preview, &made, "Kumi: undo (ka004)").unwrap_err();
    assert_eq!(code, "changed");
    assert!(message.contains("markers or regions on it"), "{message}");
    assert_eq!(lanes(), 3.0);
}

#[test]
fn a_marker_moves_to_another_ruler_lane_and_back() {
    let lab = Lab::new();
    let lane = || state(|s| s.marker_lanes.get("{MARK-1}").copied().unwrap_or(0));
    fake::extra(|e| e.project_text.insert("RULER_LANE_NAME:1".into(), "Cues".into()));
    let (preview, made) = lab.make("marker-lane", json!({"ref": "r:marker:{MARK-1}", "lane": 1}), "Kumi: x (km001)");
    assert_eq!(lane(), 1);
    assert_eq!(made["display"]["title"], "Moved marker “Drop” to ruler lane 1 (“Cues”)");
    lab.undo("marker-lane", &preview, &made, "Kumi: undo (km002)").unwrap();
    assert_eq!(lane(), 0);
    let (code, message) = lab.preview("marker-lane", json!({"ref": "r:marker:{MARK-1}", "lane": 5})).unwrap_err();
    assert_eq!(code, "bad-params");
    assert!(message.contains("2 ruler lanes"), "{message}");
}

#[test]
fn a_regions_render_matrix_gains_and_loses_tracks_and_only_those_are_put_back() {
    let lab = Lab::new();
    let matrix = || state(|s| s.render_matrix.get(&1).cloned().unwrap_or_default());
    state(|s| s.render_matrix.insert(1, vec!["{CCCC}".into()]));
    let input = json!({"ref": "r:region:{REG-1}", "addRefs": [KICK, "r:track:master"], "removeRefs": [SNARE]});
    let (preview, made) = lab.make("region-render", input, "Kumi: x (kx001)");
    assert_eq!(matrix(), ["{BBBB}", "master"]);
    assert_eq!(made["display"]["title"], "Region “Verse” renders “Kick”, the master; no longer renders “Snare”");
    // The producer adds Reverb meanwhile: not Kumi's, and it stays.
    state(|s| s.render_matrix.get_mut(&1).unwrap().push("{DDDD}".into()));
    lab.undo("region-render", &preview, &made, "Kumi: undo (kx002)").unwrap();
    assert_eq!(matrix(), ["{DDDD}", "{CCCC}"]);
    // The producer took out the track Kumi added: it is back as it was, and nothing is written.
    let (preview, made) = lab.make("region-render", json!({"ref": "r:region:{REG-1}", "addRefs": [KICK]}), "Kumi: x (kx003)");
    state(|s| s.render_matrix.get_mut(&1).unwrap().retain(|id| id != "{BBBB}"));
    let again = lab.undo("region-render", &preview, &made, "Kumi: undo (kx004)").unwrap();
    assert_eq!(again["display"]["note"], "It was back already: nothing was written.");
    // A track Kumi added that was deleted since: gone (discard puts back the rest).
    let (preview, made) =
        lab.make("region-render", json!({"ref": "r:region:{REG-1}", "addRefs": [KICK, "r:track:master"]}), "Kumi: x (kx005)");
    state(|s| s.tracks.remove(1));
    assert_eq!(lab.undo("region-render", &preview, &made, "Kumi: undo (kx006)").unwrap_err().0, "gone");
    lab.undo_anyway("region-render", &preview, &made, "Kumi: undo (kx007)").unwrap();
    assert!(!matrix().contains(&"master".to_string()));
    for wrong in [
        json!({"ref": "r:marker:{MARK-1}", "addRefs": [KICK]}),
        json!({"ref": "r:region:{REG-1}"}),
        json!({"ref": "r:region:{REG-1}", "addRefs": [SNARE], "removeRefs": [SNARE]}),
    ] {
        assert_eq!(lab.preview("region-render", wrong.clone()).unwrap_err().0, "bad-params", "{wrong}");
    }
}

#[test]
fn the_project_row_says_its_lanes_named_groups_grid_timebase_title_and_notes() {
    let lab = Lab::new();
    let project = |lab: &Lab| lab.call("read", json!({"kind": "project"})).unwrap()["items"][0].clone();
    let row = project(&lab);
    assert_eq!(
        row["rulerLanes"],
        json!([{"lane": 0, "name": "", "defaultFor": "regions"}, {"lane": 1, "name": "", "defaultFor": "markers"}])
    );
    assert_eq!(row["grid"], json!({"division": 0.25, "swing": 0.0, "measure": false}));
    assert_eq!(row["timebase"], json!({"items": "beats", "tempoEnvelopes": "beats"}));
    for absent in ["trackGroups", "title", "author", "notes"] {
        assert!(row.get(absent).is_none(), "{absent} only when set");
    }
    lab.make("ruler-lane", json!({"lane": 1, "name": "Cues", "hidden": true}), "Kumi: x (kr001)");
    lab.make("track-group-name", json!({"groupNumber": 2, "name": "Drums"}), "Kumi: x (kr002)");
    lab.make("project-info", json!({"title": "Night Drive"}), "Kumi: x (kr003)");
    lab.make("project-notes", json!({"notes": "n".repeat(2500)}), "Kumi: x (kr004)");
    lab.make("project-grid", json!({"division": "1/16", "swing": 0.25}), "Kumi: x (kr005)");
    let row = project(&lab);
    assert_eq!(row["rulerLanes"][1], json!({"lane": 1, "name": "Cues", "defaultFor": "markers", "hidden": true}));
    assert_eq!(row["trackGroups"], json!([{"group": 2, "name": "Drums"}]));
    assert_eq!((row["title"].as_str(), row["notesClipped"].as_bool()), (Some("Night Drive"), Some(true)));
    assert_eq!(row["notes"].as_str().unwrap().len(), 2000);
    assert_eq!(row["grid"], json!({"division": 0.0625, "swing": 0.25, "measure": false}));
}

#[test]
fn a_ruler_lane_added_and_then_taken_away_by_the_producer_is_gone_to_undo() {
    let lab = Lab::new();
    let (preview, made) = lab.make("ruler-lane-add", json!({}), "Kumi: x (kg001)");
    fake::extra(|e| e.project_info.insert("RULER_LANE_COUNT".into(), 2.0));
    assert_eq!(lab.undo("ruler-lane-add", &preview, &made, "Kumi: undo (kg002)").unwrap_err().0, "gone");
    // A grid set back by hand: back already, nothing written.
    let (preview, made) = lab.make("project-grid", json!({"division": "1/8"}), "Kumi: x (kg003)");
    fake::extra(|e| e.grid = (0.25, 0, 0.0));
    let again = lab.undo("project-grid", &preview, &made, "Kumi: undo (kg004)").unwrap();
    assert_eq!(again["display"]["note"], "It was back already: nothing was written.");
}
