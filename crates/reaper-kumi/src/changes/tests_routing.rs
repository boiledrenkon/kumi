//! Routing (r-tracks, M4): receives changed through their own refs and made from the receiving side, a send's polarity,
//! mono, pre-receive mode and MIDI buses, hardware outputs, and a deleted track's routes made again in their places (N10).

use serde_json::{json, Value};

use super::tests_lab::*;
use crate::reaper::fake::{self, FakeSend};

const DRUMS_TO_REVERB: &str = "r:send:{AAAA}:0:{DDDD}";
const REVERB_FROM_DRUMS: &str = "r:send:{DDDD}:-1:{AAAA}";

fn drums_send() -> FakeSend {
    state(|s| s.tracks[0].sends[0].clone())
}

#[test]
fn a_receive_is_changed_through_its_own_ref_as_the_send_it_is() {
    let lab = Lab::new();
    let (preview, made) = lab.make("send-volume", json!({"ref": REVERB_FROM_DRUMS, "volumeDb": -6.0}), "Kumi: x (kr001)");
    assert_eq!(preview["target"]["ref"], DRUMS_TO_REVERB, "the send it is, from the sending side");
    assert!((drums_send().volume - 10f64.powf(-6.0 / 20.0)).abs() < 1e-12);
    assert_eq!(made["display"]["title"], "Send “Drums” → “Reverb” −6 dB");
    assert!(drums_send().ext.contains_key("P_EXT:kumi_id"), "Kumi's mark, on the one object");
    lab.undo("send-volume", &preview, &made, "Kumi: undo (kr002)").unwrap();
    assert_eq!(drums_send().volume, 1.0);
    // And removed through it.
    let (preview, made) = lab.make("send-remove", json!({"ref": REVERB_FROM_DRUMS}), "Kumi: x (kr003)");
    assert!(state(|s| s.tracks[0].sends.is_empty()));
    lab.undo("send-remove", &preview, &made, "Kumi: undo (kr004)").unwrap();
    assert_eq!(state(|s| s.tracks[0].sends.len()), 1);
}

#[test]
fn a_receive_ref_into_a_pair_of_routes_carries_their_state_and_a_producer_change_refuses_it() {
    let lab = Lab::new();
    state(|s| s.tracks[0].sends.push(FakeSend { volume: 0.5, ..FakeSend::to(3) }));
    let rows = lab.call("read", json!({"kind": "send", "parent": REVERB})).unwrap();
    let receives: Vec<String> =
        rows["items"].as_array().unwrap().iter().filter(|r| r["category"] == -1).map(|r| r["ref"].as_str().unwrap().to_string()).collect();
    assert_eq!(receives.len(), 2);
    assert!(receives[1].starts_with("r:send:{DDDD}:-1:{AAAA}:2of2."), "{}", receives[1]);
    // As read, it names the second route: a change through it goes to that one.
    let preview = lab.preview("send-mute", json!({"ref": receives[1], "mute": true})).unwrap();
    assert_eq!(preview["prior"], json!({"mute": false}));
    assert!(preview["target"]["ref"].as_str().unwrap().starts_with("r:send:{AAAA}:0:{DDDD}:2of2."));
    // The producer changes the pair after the read: the ref may name another route now.
    state(|s| s.tracks[0].sends[0].pan = 0.3);
    let (code, _) = lab.preview("send-mute", json!({"ref": receives[1], "mute": true})).unwrap_err();
    assert_eq!(code, "changed");
    assert!(!state(|s| s.tracks[0].sends[1].mute));
}

#[test]
fn a_receive_made_from_the_receiving_side_is_the_send_from_the_other_track() {
    let lab = Lab::new();
    let (preview, made) =
        lab.make("receive-add", json!({"ref": REVERB, "fromRef": KICK, "volumeDb": -12.0, "mode": "pre-fader"}), "Kumi: x (kv001)");
    assert_eq!(made["display"]["title"], "Added send “Kick” → “Reverb”");
    let send = state(|s| s.tracks[1].sends[0].clone());
    assert_eq!((send.dest, send.mode), (3, 3));
    assert!(made["created"]["stamp"].is_string());
    lab.undo("receive-add", &preview, &made, "Kumi: undo (kv002)").unwrap();
    assert!(state(|s| s.tracks[1].sends.is_empty()));
    assert_eq!(lab.preview("receive-add", json!({"ref": REVERB})).unwrap_err().0, "bad-params");
}

#[test]
fn a_sends_polarity_mono_and_pre_receive_mode_are_applied_and_put_back() {
    let lab = Lab::new();
    let cases = [
        ("send-phase", json!({"ref": DRUMS_TO_REVERB, "inverted": true}), "B_PHASE", "Send “Drums” → “Reverb” polarity inverted"),
        ("send-mono", json!({"ref": DRUMS_TO_REVERB, "mono": true}), "B_MONO", "Send “Drums” → “Reverb” summed to mono"),
    ];
    for (n, (kind, input, parm, title)) in cases.into_iter().enumerate() {
        let (preview, made) = lab.make(kind, input, &format!("Kumi: x (kp00{n})"));
        assert_eq!(made["display"]["title"], title);
        assert_eq!(drums_send().extra.get(parm), Some(&1.0), "{kind}");
        lab.undo(kind, &preview, &made, &format!("Kumi: undo (kq00{n})")).unwrap();
        assert_eq!(drums_send().extra.get(parm), Some(&0.0), "{kind}");
    }
    let (preview, made) = lab.make("send-mode", json!({"ref": DRUMS_TO_REVERB, "mode": "pre-receive"}), "Kumi: x (kp010)");
    assert_eq!((drums_send().mode, made["display"]["title"].as_str()), (8, Some("Send “Drums” → “Reverb” pre-receive")));
    lab.undo("send-mode", &preview, &made, "Kumi: undo (kp011)").unwrap();
    assert_eq!(drums_send().mode, 0);
}

#[test]
fn a_sends_midi_buses_and_faders_are_written_and_a_midi_only_send_takes_no_audio() {
    let lab = Lab::new();
    let midi = json!({"from": 2, "to": 5, "fromBus": 3, "toBus": 4, "faders": true});
    let (preview, made) = lab.make("send-channels", json!({"ref": DRUMS_TO_REVERB, "midi": midi}), "Kumi: x (kb001)");
    let flags = 2 | (5 << 5) | 1024 | (3 << 14) | (4 << 22);
    assert_eq!(drums_send().extra.get("I_MIDIFLAGS"), Some(&(flags as f64)));
    let title = made["display"]["title"].as_str().unwrap();
    assert!(
        title.contains("MIDI 2 → 5") && title.contains("bus 3 → bus 4") && title.contains("faders send MIDI volume and pan"),
        "{title}"
    );
    assert_eq!(made["applied"]["midi"], json!({"from": 2, "to": 5, "fromBus": 3, "toBus": 4, "faders": true}));
    lab.undo("send-channels", &preview, &made, "Kumi: undo (kb002)").unwrap();
    assert_eq!(drums_send().extra.get("I_MIDIFLAGS"), Some(&0.0));
    // A send of MIDI alone: no audio, all channels.
    let (_, made) =
        lab.make("send-add", json!({"ref": KICK, "toRef": REVERB, "srcChannels": "none", "midi": {"from": "all"}}), "Kumi: x (kb003)");
    assert_eq!(state(|s| s.tracks[1].sends[0].src_chan), -1);
    assert!(made["display"]["title"].as_str().unwrap().contains("no audio"));
    assert_eq!(lab.preview("send-channels", json!({"ref": DRUMS_TO_REVERB, "midi": {"fromBus": 200}})).unwrap_err().0, "bad-params");
}

// ----- hardware outputs -----

fn kick_outputs() -> Vec<FakeSend> {
    state(|s| s.tracks[1].hw_outs.clone())
}

fn kick_output_ref(lab: &Lab, n: usize) -> String {
    let rows = lab.call("read", json!({"kind": "send", "parent": KICK})).unwrap();
    rows["items"].as_array().unwrap().iter().filter(|r| r["category"] == 1).nth(n).unwrap()["ref"].as_str().unwrap().to_string()
}

#[test]
fn a_hardware_output_is_added_to_an_output_the_device_has_and_undone_by_its_mark() {
    let lab = Lab::new();
    let (preview, made) =
        lab.make("hw-out-add", json!({"ref": KICK, "output": {"first": 1}, "volumeDb": -6.0}), "Kumi: Added output (kh001)");
    assert_eq!(made["display"]["title"], "Added hardware output “Kick” → output 1/2");
    let outputs = kick_outputs();
    assert_eq!((outputs.len(), outputs[0].dst_chan), (1, 0));
    assert!((outputs[0].volume - 10f64.powf(-6.0 / 20.0)).abs() < 1e-12);
    assert!(made["created"]["stamp"].is_string() && made["created"]["ref"] == "r:send:{BBBB}:1:hw0");
    // The producer adds one of their own before it in REAPER's list's terms (it's made first): the mark still finds Kumi's.
    state(|s| s.tracks[1].hw_outs.insert(0, FakeSend { dst_chan: 1, ..FakeSend::to(0) }));
    lab.undo("hw-out-add", &preview, &made, "Kumi: undo (kh002)").unwrap();
    assert_eq!(kick_outputs().iter().map(|o| o.dst_chan).collect::<Vec<_>>(), [1], "the producer's stays");
    // An output the device lacks, with the ones it has.
    let (code, message) = lab.preview("hw-out-add", json!({"ref": KICK, "output": {"first": 2}})).unwrap_err();
    assert_eq!(code, "bad-params");
    assert!(message.contains("2 output(s): 1 “Output 1”, 2 “Output 2”") && message.contains("no output 3"), "{message}");
    fake::extra(|e| e.audio_outputs.clear());
    let (_, message) = lab.preview("hw-out-add", json!({"ref": KICK, "output": {"first": 1, "mono": true}})).unwrap_err();
    assert!(message.contains("no outputs"), "{message}");
}

#[test]
fn a_hardware_outputs_settings_are_changed_and_put_back_and_a_producer_change_after_the_preview_refuses() {
    let lab = Lab::new();
    state(|s| s.tracks[1].hw_outs = vec![FakeSend::to(0)]);
    let output = kick_output_ref(&lab, 0);
    assert_eq!(output, "r:send:{BBBB}:1:hw0");
    let cases = [
        ("hw-out-volume", json!({"ref": output, "volumeDb": -3.0}), "Hardware output “Kick” → output 1/2 −3 dB"),
        ("hw-out-pan", json!({"ref": output, "pan": -0.5}), "Hardware output “Kick” → output 1/2 pan 50% left"),
        ("hw-out-mute", json!({"ref": output, "mute": true}), "Hardware output “Kick” → output 1/2 muted"),
        ("hw-out-mode", json!({"ref": output, "mode": "pre-fx"}), "Hardware output “Kick” → output 1/2 pre-fx"),
        ("hw-out-phase", json!({"ref": output, "inverted": true}), "Hardware output “Kick” → output 1/2 polarity inverted"),
        ("hw-out-mono", json!({"ref": output, "mono": true}), "Hardware output “Kick” → output 1/2 summed to mono"),
    ];
    let before = kick_outputs()[0].clone();
    for (n, (kind, input, title)) in cases.into_iter().enumerate() {
        let (preview, made) = lab.make(kind, input, &format!("Kumi: x (ks00{n})"));
        assert_eq!(made["display"]["title"], title, "{kind}");
        lab.undo(kind, &preview, &made, &format!("Kumi: undo (kt00{n})")).unwrap();
        let now = kick_outputs()[0].clone();
        assert_eq!((now.volume, now.pan, now.mute, now.mode), (before.volume, before.pan, before.mute, before.mode), "{kind}");
    }
    assert_eq!(lab.preview("hw-out-mode", json!({"ref": output, "mode": "pre-receive"})).unwrap_err().0, "bad-params");
    // Kumi's output (marked now): the producer changes what the change touches after the preview: refused, nothing written.
    let input = json!({"ref": output, "volumeDb": -9.0});
    let preview = lab.preview("hw-out-volume", input.clone()).unwrap();
    state(|s| s.tracks[1].hw_outs[0].volume = 0.25);
    let (code, _) = lab.apply("hw-out-volume", &input, &preview, "Kumi: x (ku001)").unwrap_err();
    assert_eq!((code.as_str(), kick_outputs()[0].volume), ("changed", 0.25));
    // An output Kumi never marked is the one at its ref's place only while nothing of it changed: a pan the producer moved
    // after the preview refuses a volume change, since it may be another output now.
    state(|s| s.tracks[1].hw_outs = vec![FakeSend::to(0)]);
    let preview = lab.preview("hw-out-volume", input.clone()).unwrap();
    state(|s| s.tracks[1].hw_outs[0].pan = 0.25);
    let (code, _) = lab.apply("hw-out-volume", &input, &preview, "Kumi: x (ku002)").unwrap_err();
    assert_eq!((code.as_str(), kick_outputs()[0].volume), ("changed", 1.0));
}

#[test]
fn an_output_moved_to_another_output_is_found_by_kumis_mark_to_undo() {
    let lab = Lab::new();
    state(|s| s.tracks[1].hw_outs = vec![FakeSend::to(0)]);
    let output = kick_output_ref(&lab, 0);
    let (preview, made) = lab.make("hw-out-channels", json!({"ref": output, "output": {"first": 2, "mono": true}}), "Kumi: x (kc001)");
    assert_eq!(kick_outputs()[0].dst_chan, 1 | 1024);
    assert_eq!(made["display"]["title"], "Hardware output “Kick” → output 1/2 to output 2 (mono)");
    // Its ref named output 1; it goes to output 2 now: its mark finds it.
    lab.undo("hw-out-channels", &preview, &made, "Kumi: undo (kc002)").unwrap();
    assert_eq!(kick_outputs()[0].dst_chan, 0);
    // Past the device's outputs, or from channels the track lacks: refused.
    assert_eq!(lab.preview("hw-out-channels", json!({"ref": output, "output": {"first": 2}})).unwrap_err().0, "bad-params");
    let (_, message) = lab.preview("hw-out-channels", json!({"ref": output, "srcChannels": {"offset": 3}})).unwrap_err();
    assert!(message.contains("has 2 channels"), "{message}");
}

#[test]
fn a_removed_output_is_put_back_with_its_mark_and_last_in_the_list_said_so() {
    let lab = Lab::new();
    state(|s| s.tracks[1].hw_outs = vec![FakeSend { volume: 0.5, ..FakeSend::to(0) }, FakeSend { dst_chan: 1024 | 1, ..FakeSend::to(0) }]);
    let first = kick_output_ref(&lab, 0);
    let (preview, made) = lab.make("hw-out-remove", json!({"ref": first}), "Kumi: x (kx001)");
    assert_eq!(kick_outputs().len(), 1);
    let undone = lab.undo("hw-out-remove", &preview, &made, "Kumi: undo (kx002)").unwrap();
    let outputs = kick_outputs();
    assert_eq!((outputs.len(), outputs[1].dst_chan, outputs[1].volume), (2, 0, 0.5));
    assert_eq!(undone["display"]["note"], "REAPER lists it after the track's other hardware outputs now.");
    let again = lab.undo("hw-out-remove", &preview, &made, "Kumi: undo (kx003)").unwrap();
    assert_eq!(again["display"]["title"], "The hardware output is back already");
    assert_eq!(kick_outputs().len(), 2);
}

#[test]
fn two_outputs_of_a_track_to_one_output_are_never_guessed_between() {
    let lab = Lab::new();
    state(|s| s.tracks[1].hw_outs = vec![FakeSend::to(0), FakeSend { volume: 0.5, ..FakeSend::to(0) }]);
    let (code, message) = lab.preview("hw-out-mute", json!({"ref": "r:send:{BBBB}:1:hw0", "mute": true})).unwrap_err();
    assert_eq!(code, "changed");
    assert!(message.contains("can't tell which"), "{message}");
}

// ----- N10: a deleted track's routes come back in their places -----

/// Each track's sends (destination, volume) and receives' volumes, in REAPER's order.
fn routes() -> Vec<Vec<(usize, f64, f64, f64)>> {
    let or_default = |send: &FakeSend, key: &str| send.extra.get(key).copied().unwrap_or(-1.0);
    state(|s| {
        s.tracks
            .iter()
            .map(|t| {
                t.sends.iter().map(|send| (send.dest, send.volume, or_default(send, "I_SLOT_HINT"), or_default(send, "D_PANLAW"))).collect()
            })
            .collect()
    })
}

#[test]
fn a_deleted_tracks_routes_come_back_where_they_were_with_their_slot_and_pan_law() {
    let lab = Lab::new();
    state(|s| {
        // Kick sends twice to Reverb (one with a slot of the producer's and a pan law), once to Snare; Drums and Snare send to
        // Kick, and Drums to Reverb after it.
        let mut hinted = FakeSend { volume: 0.4, ..FakeSend::to(3) };
        hinted.extra.insert("I_SLOT_HINT".into(), 2.0);
        hinted.extra.insert("D_PANLAW".into(), 0.5);
        s.tracks[1].sends = vec![FakeSend { volume: 0.2, ..FakeSend::to(2) }, FakeSend { volume: 0.3, ..FakeSend::to(3) }, hinted];
        s.tracks[0].sends = vec![FakeSend { volume: 0.7, ..FakeSend::to(1) }, FakeSend { volume: 0.8, ..FakeSend::to(3) }];
        s.tracks[2].sends = vec![FakeSend { volume: 0.9, ..FakeSend::to(1) }];
    });
    let before = routes();
    let (preview, made) = lab.make("track-delete", json!({"ref": KICK}), "Kumi: Deleted track (kn001)");
    lab.undo("track-delete", &preview, &made, "Kumi: undo (kn002)").unwrap();
    assert_eq!(routes(), before, "every route in its place, the producer's slot and pan law kept");
}
