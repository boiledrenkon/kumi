//! A track's settings beyond its fader (r-tracks, M4): each applied, undone by Kumi, refused when the producer changed it
//! between the preview and the apply or since, gone with its track, and found back already; and each one's own rules.

use serde_json::{json, Value};

use super::tests_lab::*;
use crate::reaper::fake;

fn kick_value(parm: &str) -> f64 {
    let api = fake::api();
    let reaper = crate::reaper::Reaper::new(&api);
    let proj = reaper.current_project().unwrap().ptr;
    reaper.track_value(reaper.track_at(proj, 1).unwrap(), parm)
}

fn set_kick(parm: &str, value: f64) {
    let api = fake::api();
    let reaper = crate::reaper::Reaper::new(&api);
    let proj = reaper.current_project().unwrap().ptr;
    reaper.set_track_value(reaper.track_at(proj, 1).unwrap(), parm, value);
}

/// Every setting on Kick: its input, the REAPER value it writes, what that becomes, a value the producer might set after
/// the preview (not the one there before), one they might set after the apply (neither Kumi's nor the one before: none for a
/// switch, which has only two), and its title.
type Case = (&'static str, Value, &'static str, f64, f64, Option<f64>, &'static str);

fn cases() -> Vec<Case> {
    vec![
        ("track-phase", json!({"ref": KICK, "inverted": true}), "B_PHASE", 1.0, 1.0, None, "Kick polarity inverted"),
        ("track-arm", json!({"ref": KICK, "arm": true}), "I_RECARM", 1.0, 1.0, None, "Kick armed for recording"),
        (
            "track-record-input",
            json!({"ref": KICK, "input": {"kind": "midi", "device": "all", "channel": 3}}),
            "I_RECINPUT",
            (4096 | (63 << 5) | 3) as f64,
            1024.0,
            Some(1024.0),
            "Kick records from all MIDI devices, channel 3",
        ),
        (
            "track-monitoring",
            json!({"ref": KICK, "monitoring": "auto"}),
            "I_RECMON",
            2.0,
            1.0,
            Some(1.0),
            "Kick monitoring on when not playing (tape style)",
        ),
        (
            "track-record-mode",
            json!({"ref": KICK, "mode": "midi-overdub"}),
            "I_RECMODE",
            7.0,
            1.0,
            Some(1.0),
            "Kick records MIDI overdubbing",
        ),
        ("track-main-send", json!({"ref": KICK, "on": false}), "B_MAINSEND", 0.0, 0.0, None, "Kick no longer sends to its parent"),
        ("track-fx-enabled", json!({"ref": KICK, "enabled": false}), "I_FXEN", 0.0, 0.0, None, "Kick FX chain bypassed"),
        ("track-pan-mode", json!({"ref": KICK, "mode": "dual"}), "I_PANMODE", 6.0, 0.0, Some(0.0), "Kick pans in dual pan mode"),
        ("track-pan-law", json!({"ref": KICK, "law": -6.0}), "D_PANLAW", 0.5011872336272722, 1.0, Some(1.0), "Kick pan law −6 dB"),
        ("track-solo-defeat", json!({"ref": KICK, "defeat": true}), "B_SOLO_DEFEAT", 1.0, 1.0, None, "Kick solo defeat on"),
        ("track-visible", json!({"ref": KICK, "mixer": false}), "B_SHOWINMIXER", 0.0, 0.0, None, "Kick hidden in the mixer"),
        (
            "track-midi-out",
            json!({"ref": KICK, "device": 0, "channel": 10}),
            "I_MIDIHWOUT",
            10.0,
            33.0,
            Some(33.0),
            "Kick sends MIDI to “Fake Synth” on channel 10",
        ),
    ]
}

#[test]
fn every_track_setting_is_applied_undone_by_kumi_and_found_back_already() {
    for (n, (kind, input, parm, applied, _, _, title)) in cases().into_iter().enumerate() {
        let lab = Lab::new();
        let before = kick_value(parm);
        let (preview, made) = lab.make(kind, input.clone(), &format!("Kumi: {title} (ka{n:03})"));
        assert_eq!(preview["display"]["title"], title, "{kind}");
        assert_eq!(made["display"]["title"], title, "{kind}");
        assert!((kick_value(parm) - applied).abs() < 1e-9, "{kind}: {parm} is {}", kick_value(parm));
        assert_eq!(made["undo"]["entry"], true, "{kind}: one point of REAPER's");
        assert_eq!(top().as_deref(), Some(format!("Kumi: {title} (ka{n:03})").as_str()));
        let undone = lab.undo(kind, &preview, &made, &format!("Kumi: undo {title} (ku{n:03})")).unwrap();
        assert!((kick_value(parm) - before).abs() < 1e-9, "{kind}: put back");
        assert!(undone["display"]["title"].as_str().unwrap().ends_with("(put back)"), "{kind}");
        // Asked again (a lost answer): it is back already, and nothing is written.
        let entries_before = entries();
        let again = lab.undo(kind, &preview, &made, &format!("Kumi: undo {title} (kv{n:03})")).unwrap();
        assert_eq!(again["display"]["note"], "It was back already: nothing was written.", "{kind}");
        assert_eq!(entries(), entries_before, "{kind}: no point for a restore that wrote nothing");
    }
}

#[test]
fn a_setting_the_producer_changed_after_the_preview_is_refused_and_nothing_is_written() {
    for (kind, input, parm, _, theirs, _, _) in cases() {
        let lab = Lab::new();
        let preview = lab.preview(kind, input.clone()).unwrap();
        set_kick(parm, theirs);
        let entries_before = entries();
        let (code, message) = lab.apply(kind, &input, &preview, "Kumi: x (kf001)").unwrap_err();
        assert_eq!(code, "changed", "{kind}: {message}");
        assert!(message.contains("changed since the preview"), "{kind}: {message}");
        assert!((kick_value(parm) - theirs).abs() < 1e-9, "{kind}: the producer's value stays");
        assert_eq!(entries(), entries_before, "{kind}: no point");
    }
}

#[test]
fn undo_refuses_a_setting_the_producer_changed_since_and_discard_puts_it_back_anyway() {
    for (n, (kind, input, parm, _, _, theirs, _)) in cases().into_iter().enumerate() {
        let Some(theirs) = theirs else { continue };
        let lab = Lab::new();
        let before = kick_value(parm);
        let (preview, made) = lab.make(kind, input, &format!("Kumi: x (kc{n:03})"));
        set_kick(parm, theirs);
        let (code, message) = lab.undo(kind, &preview, &made, "Kumi: undo (kc999)").unwrap_err();
        assert_eq!(code, "changed", "{kind}");
        assert!(message.contains("changed in REAPER since Kumi set it"), "{kind}: {message}");
        assert!((kick_value(parm) - theirs).abs() < 1e-9, "{kind}: nothing written");
        lab.undo_anyway(kind, &preview, &made, "Kumi: undo (kc998)").unwrap();
        assert!((kick_value(parm) - before).abs() < 1e-9, "{kind}: discard puts it back");
    }
}

#[test]
fn undo_of_a_setting_on_a_track_that_went_is_gone() {
    let lab = Lab::new();
    let (preview, made) = lab.make("track-phase", json!({"ref": KICK, "inverted": true}), "Kumi: x (kg001)");
    state(|s| s.tracks.remove(1));
    let (code, _) = lab.undo("track-phase", &preview, &made, "Kumi: undo (kg002)").unwrap_err();
    assert_eq!(code, "gone");
}

#[test]
fn undo_puts_back_only_the_fields_the_change_touched() {
    let lab = Lab::new();
    let (preview, made) = lab.make("track-monitoring", json!({"ref": KICK, "monitoring": "on"}), "Kumi: x (kt001)");
    assert_eq!(preview["prior"], json!({"monitoring": 0}));
    // The producer turns on monitoring items beside it: not Kumi's to put back, nor a reason to refuse.
    set_kick("I_RECMONITEMS", 1.0);
    lab.undo("track-monitoring", &preview, &made, "Kumi: undo (kt002)").unwrap();
    assert_eq!((kick_value("I_RECMON"), kick_value("I_RECMONITEMS")), (0.0, 1.0));
    // The record mode's tap is two bits of a flags word: the other bits are left.
    set_kick("I_RECMODE_FLAGS", 4.0 + 1.0);
    let (preview, made) = lab.make("track-record-mode", json!({"ref": KICK, "tap": "post-fx"}), "Kumi: x (kt003)");
    assert_eq!(kick_value("I_RECMODE_FLAGS"), 4.0 + 2.0);
    assert_eq!(made["display"]["title"], "Kick records outputs post-fx");
    lab.undo("track-record-mode", &preview, &made, "Kumi: undo (kt004)").unwrap();
    assert_eq!(kick_value("I_RECMODE_FLAGS"), 4.0 + 1.0);
}

#[test]
fn a_record_input_reaper_doesnt_have_is_refused_with_what_it_has() {
    let lab = Lab::new();
    // Two audio inputs: a stereo pair from 1 is there, from 2 it isn't.
    lab.make("track-record-input", json!({"ref": KICK, "input": {"kind": "audio", "first": 1, "layout": "stereo"}}), "Kumi: x (kr001)");
    assert_eq!(kick_value("I_RECINPUT"), 1024.0);
    let (code, message) =
        lab.preview("track-record-input", json!({"ref": KICK, "input": {"kind": "audio", "first": 2, "layout": "stereo"}})).unwrap_err();
    assert_eq!(code, "bad-params");
    assert!(message.contains("2 input(s): 1 “Input 1”, 2 “Input 2”") && message.contains("no audio input 3"), "{message}");
    // No audio device with inputs (the lab's): said so.
    fake::extra(|e| e.audio_inputs.clear());
    let (_, message) = lab.preview("track-record-input", json!({"ref": KICK, "input": {"kind": "audio", "first": 1}})).unwrap_err();
    assert!(message.contains("no inputs"), "{message}");
    // A MIDI device that isn't there, with the ones that are.
    let (_, message) = lab.preview("track-record-input", json!({"ref": KICK, "input": {"kind": "midi", "device": 5}})).unwrap_err();
    assert!(message.contains("no MIDI input 5") && message.contains("0 “Fake Keys”"), "{message}");
    let made =
        lab.make("track-record-input", json!({"ref": KICK, "input": {"kind": "midi", "device": "virtual-keyboard"}}), "Kumi: x (kr002)");
    assert_eq!(made.1["display"]["title"], "Kick records from the virtual MIDI keyboard, all channels");
    let made =
        lab.make("track-record-input", json!({"ref": KICK, "input": {"kind": "midi", "device": 0, "channel": 1}}), "Kumi: x (kr003)");
    assert_eq!(made.1["display"]["title"], "Kick records from “Fake Keys”, channel 1");
    let made = lab.make("track-record-input", json!({"ref": KICK, "input": {"kind": "none"}}), "Kumi: x (kr004)");
    assert_eq!((kick_value("I_RECINPUT"), made.1["display"]["title"].as_str()), (-1.0, Some("Kick records from no input")));
}

#[test]
fn width_needs_stereo_pan_and_a_switch_to_it_is_said_and_undone_with_it() {
    let lab = Lab::new();
    // Kick pans in balance mode, which has no width.
    let (code, message) = lab.preview("track-width", json!({"ref": KICK, "width": 0.5})).unwrap_err();
    assert_eq!(code, "bad-params");
    assert!(message.contains("balance mode, which has no width") && message.contains("panMode \"stereo\""), "{message}");
    let (preview, made) = lab.make("track-width", json!({"ref": KICK, "width": 0.5, "panMode": "stereo"}), "Kumi: x (kw001)");
    assert_eq!((kick_value("I_PANMODE"), kick_value("D_WIDTH")), (5.0, 0.5));
    assert_eq!(made["display"]["title"], "Kick width 50%");
    assert_eq!(made["display"]["note"], "It now pans in stereo pan mode (pan and width); its pan law is unchanged.");
    assert_eq!(kick_value("D_PANLAW"), -1.0, "the pan law is left as it was");
    lab.undo("track-width", &preview, &made, "Kumi: undo (kw002)").unwrap();
    assert_eq!((kick_value("I_PANMODE"), kick_value("D_WIDTH")), (3.0, 1.0));
    // A track that follows the project's default, when that is stereo pan, needs no switch.
    set_kick("I_PANMODE", -1.0);
    fake::extra(|e| e.config.insert("panmode".into(), "5".into()));
    let (_, made) = lab.make("track-width", json!({"ref": KICK, "width": -0.25}), "Kumi: x (kw003)");
    assert_eq!((kick_value("I_PANMODE"), kick_value("D_WIDTH")), (-1.0, -0.25));
    assert!(made["display"].get("note").is_none());
}

#[test]
fn dual_pan_needs_dual_pan_mode_and_each_side_is_put_back() {
    let lab = Lab::new();
    assert_eq!(lab.preview("track-dual-pan", json!({"ref": KICK, "left": -1.0})).unwrap_err().0, "bad-params");
    let (preview, made) =
        lab.make("track-dual-pan", json!({"ref": KICK, "left": -1.0, "right": 0.5, "panMode": "dual"}), "Kumi: x (kd001)");
    assert_eq!((kick_value("I_PANMODE"), kick_value("D_DUALPANL"), kick_value("D_DUALPANR")), (6.0, -1.0, 0.5));
    assert_eq!(made["display"]["title"], "Kick dual pan left 100% left, right 50% right");
    lab.undo("track-dual-pan", &preview, &made, "Kumi: undo (kd002)").unwrap();
    assert_eq!(kick_value("I_PANMODE"), 3.0);
}

#[test]
fn a_parent_send_must_reach_only_channels_its_parent_has() {
    let lab = Lab::new();
    // Kick is in the Drums folder, which has 2 channels.
    let (code, message) = lab.preview("track-main-send", json!({"ref": KICK, "offset": 3, "channels": 2})).unwrap_err();
    assert_eq!(code, "bad-params");
    assert!(message.contains("“Drums” has 2 channels") && message.contains("reach channel 4"), "{message}");
    state(|s| s.tracks[0].extra.insert("I_NCHAN".into(), 4.0));
    let (preview, made) = lab.make("track-main-send", json!({"ref": KICK, "offset": 3, "channels": 2}), "Kumi: x (km001)");
    assert_eq!((kick_value("C_MAINSEND_OFFS"), kick_value("C_MAINSEND_NCH")), (2.0, 2.0));
    assert_eq!(made["display"]["title"], "Kick sends 2 channel(s) to the parent's channels 3/4");
    lab.undo("track-main-send", &preview, &made, "Kumi: undo (km002)").unwrap();
    assert_eq!((kick_value("C_MAINSEND_OFFS"), kick_value("C_MAINSEND_NCH")), (0.0, 0.0));
    // A track at the top level sends to the master.
    let (_, message) = lab.preview("track-main-send", json!({"ref": REVERB, "offset": 2})).unwrap_err();
    assert!(message.contains("“Master” has 2 channels"), "{message}");
}

/// The fake REAPER's lock actions: they lock or unlock the selected tracks, a change REAPER makes a point of.
fn lock_actions(command: i32) {
    if command == 41312 || command == 41313 {
        fake::with(|s| {
            for t in s.tracks.iter_mut().filter(|t| t.selected) {
                t.extra.insert("LOCK".into(), if command == 41312 { 1.0 } else { 0.0 });
            }
        });
        fake::mark_changed();
    }
}

#[test]
fn locking_controls_runs_reapers_lock_on_that_track_alone_and_leaves_the_selection_as_it_was() {
    let lab = Lab::new();
    state(|s| s.on_action = Some(lock_actions));
    fake::select_tracks(&[2, 3]);
    let (preview, made) = lab.make("track-lock", json!({"ref": KICK, "locked": true}), "Kumi: Kick controls locked (kl001)");
    assert_eq!(made["display"]["title"], "Kick controls locked");
    let locks: Vec<bool> = state(|s| s.tracks.iter().map(|t| t.extra.get("LOCK") == Some(&1.0)).collect());
    assert_eq!(locks, [false, true, false, false], "only Kick");
    let selected: Vec<bool> = state(|s| s.tracks.iter().map(|t| t.selected).collect());
    assert_eq!(selected, [false, false, true, true], "the producer's selection");
    assert_eq!(state(|s| s.actions.clone()), [41312]);
    lab.undo("track-lock", &preview, &made, "Kumi: undo (kl002)").unwrap();
    assert_eq!(state(|s| s.tracks[1].extra.get("LOCK").copied()), Some(0.0));
    assert_eq!(state(|s| s.tracks.iter().map(|t| t.selected).collect::<Vec<_>>()), [false, false, true, true]);
}

#[test]
fn a_folder_opens_and_collapses_and_a_track_that_is_no_folder_is_refused() {
    let lab = Lab::new();
    let (code, message) = lab.preview("track-folder-state", json!({"ref": KICK, "state": "collapsed"})).unwrap_err();
    assert_eq!(code, "bad-params");
    assert!(message.contains("isn't a folder"), "{message}");
    let (preview, made) = lab.make("track-folder-state", json!({"ref": DRUMS, "state": "fully-collapsed"}), "Kumi: x (ko001)");
    assert_eq!(made["display"]["title"], "Folder “Drums” fully collapsed");
    assert_eq!(state(|s| s.tracks[0].extra.get("I_FOLDERCOMPACT").copied()), Some(2.0));
    lab.undo("track-folder-state", &preview, &made, "Kumi: undo (ko002)").unwrap();
    assert_eq!(state(|s| s.tracks[0].extra.get("I_FOLDERCOMPACT").copied()), Some(0.0));
}

#[test]
fn the_midi_hardware_output_must_be_a_device_reaper_has() {
    let lab = Lab::new();
    let (code, message) = lab.preview("track-midi-out", json!({"ref": KICK, "device": 3})).unwrap_err();
    assert_eq!(code, "bad-params");
    assert!(message.contains("no MIDI output 3") && message.contains("0 “Fake Synth”"), "{message}");
    let (_, made) = lab.make("track-midi-out", json!({"ref": KICK, "device": 0}), "Kumi: x (kn001)");
    assert_eq!(made["display"]["title"], "Kick sends MIDI to “Fake Synth” as played");
    lab.make("track-midi-out", json!({"ref": KICK, "device": "none"}), "Kumi: x (kn002)");
    assert_eq!(kick_value("I_MIDIHWOUT"), -1.0);
}

#[test]
fn the_master_has_polarity_pan_mode_and_its_fx_switch_but_no_record_arm_or_parent_send() {
    let lab = Lab::new();
    lab.make("track-phase", json!({"ref": "r:track:master", "inverted": true}), "Kumi: x (kq001)");
    assert_eq!(state(|s| s.master.extra.get("B_PHASE").copied()), Some(1.0));
    for (kind, input) in
        [("track-arm", json!({"arm": true})), ("track-main-send", json!({"on": false})), ("track-lock", json!({"locked": true}))]
    {
        let mut input = input;
        input["ref"] = "r:track:master".into();
        let (code, message) = lab.preview(kind, input).unwrap_err();
        assert_eq!(code, "bad-params", "{kind}");
        assert!(message.starts_with("the master track has no"), "{kind}: {message}");
    }
}

#[test]
fn a_pan_law_is_its_level_at_the_centre_with_or_without_compensation() {
    let lab = Lab::new();
    let (_, made) = lab.make("track-pan-law", json!({"ref": KICK, "law": -3.0, "taper": "linear"}), "Kumi: x (kp001)");
    assert!((kick_value("D_PANLAW") - 10f64.powf(-3.0 / 20.0)).abs() < 1e-12);
    assert_eq!(kick_value("I_PANLAW_FLAGS"), 2.0);
    assert_eq!(made["display"]["title"], "Kick pan law −3 dB, linear taper");
    let (_, made) = lab.make("track-pan-law", json!({"ref": KICK, "law": -3.0, "compensated": true}), "Kumi: x (kp002)");
    assert!((kick_value("D_PANLAW") - 10f64.powf(3.0 / 20.0)).abs() < 1e-12);
    assert_eq!(made["display"]["title"], "Kick pan law −3 dB with gain compensation");
    let (_, made) = lab.make("track-pan-law", json!({"ref": KICK, "law": "default"}), "Kumi: x (kp003)");
    assert_eq!((kick_value("D_PANLAW"), made["display"]["title"].as_str()), (-1.0, Some("Kick pan law the project's default")));
}

#[test]
fn a_setting_already_so_writes_nothing_and_makes_no_point() {
    let lab = Lab::new();
    let preview = lab.preview("track-fx-enabled", json!({"ref": KICK, "enabled": true})).unwrap();
    assert_eq!(preview["display"]["unchanged"], true);
    let entries_before = entries();
    let made = lab.apply("track-fx-enabled", &json!({"ref": KICK, "enabled": true}), &preview, "Kumi: x (ky001)").unwrap();
    assert_eq!(made["undo"]["entry"], false);
    assert_eq!(entries(), entries_before);
}

#[test]
fn a_track_row_says_where_it_sends_to_its_parent_and_what_a_power_user_set() {
    let lab = Lab::new();
    let kick = |lab: &Lab| {
        let rows = lab.call("read", json!({"kind": "track"})).unwrap();
        rows["items"].as_array().unwrap().iter().find(|row| row["name"] == "Kick").cloned().unwrap()
    };
    let row = kick(&lab);
    assert_eq!(row["mainSend"], json!({"on": true, "offset": 1, "channels": "all"}));
    for absent in ["recordTap", "monitorItems", "soloDefeat", "midiOut"] {
        assert!(row.get(absent).is_none(), "{absent} only when set");
    }
    state(|s| s.tracks[0].extra.insert("I_NCHAN".into(), 4.0));
    lab.make("track-main-send", json!({"ref": KICK, "offset": 3, "channels": 2}), "Kumi: x (kz001)");
    lab.make("track-record-mode", json!({"ref": KICK, "mode": "stereo-out", "tap": "pre-fx"}), "Kumi: x (kz002)");
    lab.make("track-monitoring", json!({"ref": KICK, "items": true}), "Kumi: x (kz003)");
    lab.make("track-solo-defeat", json!({"ref": KICK, "defeat": true}), "Kumi: x (kz004)");
    lab.make("track-midi-out", json!({"ref": KICK, "device": 0, "channel": 2}), "Kumi: x (kz005)");
    let row = kick(&lab);
    assert_eq!(row["mainSend"], json!({"on": true, "offset": 3, "channels": 2}));
    assert_eq!((&row["recordMode"], &row["recordTap"]), (&json!("stereo-out"), &json!("pre-fx")));
    assert_eq!((&row["monitorItems"], &row["soloDefeat"]), (&json!(true), &json!(true)));
    assert_eq!(row["midiOut"], json!({"device": 0, "channel": 2}));
}
