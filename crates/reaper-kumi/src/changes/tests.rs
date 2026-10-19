use std::cell::RefCell;
use std::time::Instant;

use serde_json::{json, Value};

use super::*;
use crate::reaper::fake::{self, FakeSend};

#[derive(Default)]
struct Events(RefCell<Vec<(String, Value)>>);

impl Outbox for Events {
    fn event(&self, name: &str, data: Value) {
        self.0.borrow_mut().push((name.to_string(), data));
    }
    fn heartbeat(&self) {}
}

impl Events {
    fn take(&self) -> Vec<(String, Value)> {
        std::mem::take(&mut *self.0.borrow_mut())
    }
}

struct Lab {
    changes: Changes,
    reads: Reads,
    events: Events,
}

impl Lab {
    fn new() -> Lab {
        fake::lab_project();
        let lab = Lab { changes: Changes::default(), reads: Reads::default(), events: Events::default() };
        // The first look at the history: nothing to report.
        lab.tick();
        lab
    }

    fn call(&self, method: &str, mut params: Value) -> Answer {
        let api = fake::api();
        let reaper = Reaper::new(&api);
        if params.get("epoch").is_none() && !matches!(method, "transport_stop" | "transport_play" | "undo_step_begin" | "undo_step_end") {
            params["epoch"] = self.reads.epoch().into();
        }
        match method {
            "change_preview" => self.changes.preview(&reaper, &self.reads, &params),
            "change_apply" => self.changes.apply(&reaper, &self.reads, &params),
            "change_restore" => self.changes.restore(&reaper, &self.reads, &params),
            "transport_stop" => self.changes.transport_stop(&reaper),
            "transport_play" => self.changes.transport_play(&reaper),
            "cursor_set" => self.changes.cursor_set(&reaper, &self.reads, &params),
            "undo_step_begin" => self.changes.step_begin(&reaper, &self.reads, &params),
            "undo_step_end" => self.changes.step_end(&reaper, &self.reads, &params),
            other => panic!("{other}"),
        }
    }

    fn tick(&self) {
        let api = fake::api();
        self.changes.tick(&Reaper::new(&api), &self.reads, &self.events);
    }

    /// A change as Kumi's host makes it: preview, then apply with the preview's fence and a description.
    fn make(&self, kind: &str, input: Value, desc: &str) -> (Value, Value) {
        let preview = self.call("change_preview", json!({"kind": kind, "input": input})).unwrap();
        let applied = self
            .call("change_apply", json!({"kind": kind, "input": input, "fence": preview["fence"], "desc": desc}))
            .unwrap_or_else(|e| panic!("{e:?}"));
        (preview, applied)
    }

    fn undo(&self, kind: &str, preview: &Value, applied: &Value, desc: &str) -> Answer {
        let mut params =
            json!({"kind": kind, "target": preview["target"], "prior": preview["prior"], "applied": applied["applied"], "desc": desc});
        if let Some(material) = applied.get("material") {
            params["material"] = material.clone();
        }
        if let Some(created) = applied.get("created") {
            params["created"] = created.clone();
        }
        self.call("change_restore", params)
    }
}

fn state<T>(f: impl FnOnce(&mut fake::State) -> T) -> T {
    fake::with(f)
}

fn top() -> Option<String> {
    let api = fake::api();
    let reaper = Reaper::new(&api);
    reaper.undo_can_undo(reaper.current_project().unwrap().ptr)
}

const KICK: &str = "r:track:{BBBB}";

#[test]
fn a_preview_writes_nothing_and_says_what_it_would_do() {
    let lab = Lab::new();
    let before = state(|s| (s.tracks[1].volume, s.undo_entries, s.state_count));
    let preview = lab.call("change_preview", json!({"kind": "track-volume", "input": {"ref": KICK, "volumeDb": -6.0}})).unwrap();
    assert_eq!(preview["epoch"], lab.reads.epoch());
    assert_eq!(preview["target"], json!({"ref": KICK, "kind": "track"}));
    assert_eq!(preview["prior"], json!({"volume": 1.0, "volumeDb": 0.0}));
    assert_eq!(preview["fence"], preview["prior"]);
    assert_eq!(preview["display"]["title"], "Kick volume −6 dB");
    assert_eq!(state(|s| (s.tracks[1].volume, s.undo_entries, s.state_count)), before);
}

#[test]
fn an_apply_is_one_named_undo_block_and_a_restore_puts_the_value_back_as_another() {
    let lab = Lab::new();
    let (preview, applied) = lab.make("track-volume", json!({"ref": KICK, "volumeDb": -6.0}), "Kumi: Kick volume −6 dB (k1a2b)");
    assert!((state(|s| s.tracks[1].volume) - 10f64.powf(-6.0 / 20.0)).abs() < 1e-12);
    assert_eq!(applied["applied"]["volumeDb"], -6.0);
    assert_eq!(applied["undo"]["desc"], "Kumi: Kick volume −6 dB (k1a2b)");
    assert_eq!(applied["undo"]["entry"], true);
    assert_eq!(applied["undo"]["stateCount"], state(|s| s.state_count));
    assert_eq!(top().as_deref(), Some("Kumi: Kick volume −6 dB (k1a2b)"));
    // The producer's fader hasn't moved since: Kumi's undo puts the volume back, as a point of its own.
    let restored = lab.undo("track-volume", &preview, &applied, "Kumi: undo Kick volume −6 dB (k9z9z)").unwrap();
    assert_eq!(restored["restored"], preview["prior"]);
    assert_eq!(state(|s| s.tracks[1].volume), 1.0);
    assert_eq!(top().as_deref(), Some("Kumi: undo Kick volume −6 dB (k9z9z)"));
    assert_eq!(restored["undo"]["entry"], true);
    // A retry of the restore finds it put back and writes nothing more.
    let again = lab.undo("track-volume", &preview, &applied, "Kumi: undo Kick volume −6 dB (k9z9z)").unwrap();
    assert_eq!(again["restored"], preview["prior"]);
}

#[test]
fn a_change_is_refused_when_what_it_fenced_has_moved_and_nothing_is_written() {
    let lab = Lab::new();
    let input = json!({"ref": KICK, "volumeDb": -6.0});
    let preview = lab.call("change_preview", json!({"kind": "track-volume", "input": input})).unwrap();
    // The producer moved the fader between the preview and the apply.
    state(|s| s.tracks[1].volume = 0.5);
    let entries = state(|s| s.undo_entries);
    let (code, message) = lab
        .call("change_apply", json!({"kind": "track-volume", "input": input, "fence": preview["fence"], "desc": "Kumi: x (k1111)"}))
        .unwrap_err();
    assert_eq!(code, "changed");
    assert!(message.contains("Kick") && message.contains("volume"), "{message}");
    assert_eq!((state(|s| s.tracks[1].volume), state(|s| s.undo_entries)), (0.5, entries));
    // A track that went between the preview and the apply has changed too.
    state(|s| s.tracks.remove(1));
    let (code, _) = lab
        .call("change_apply", json!({"kind": "track-volume", "input": input, "fence": preview["fence"], "desc": "Kumi: x (k1111)"}))
        .unwrap_err();
    assert_eq!(code, "changed");
}

#[test]
fn a_restore_is_refused_when_the_producer_changed_the_value_since_unless_discarded() {
    let lab = Lab::new();
    let (preview, applied) = lab.make("track-pan", json!({"ref": KICK, "pan": -0.3}), "Kumi: Kick pan 30% left (k1111)");
    assert_eq!(state(|s| s.tracks[1].pan), -0.3);
    assert_eq!(preview["display"]["title"], "Kick pan 30% left");
    state(|s| s.tracks[1].pan = 0.8);
    let (code, message) = lab.undo("track-pan", &preview, &applied, "Kumi: undo (k2222)").unwrap_err();
    assert_eq!(code, "changed");
    assert!(message.contains("changed in REAPER since Kumi set it"), "{message}");
    assert_eq!(state(|s| s.tracks[1].pan), 0.8);
    let mut params = json!({"kind": "track-pan", "target": preview["target"], "prior": preview["prior"], "applied": applied["applied"], "desc": "Kumi: undo (k2222)", "discard": true});
    params["epoch"] = lab.reads.epoch().into();
    lab.call("change_restore", params).unwrap();
    assert_eq!(state(|s| s.tracks[1].pan), 0.0);
}

#[test]
fn every_attribute_is_applied_and_put_back_exactly() {
    let lab = Lab::new();
    state(|s| s.tracks[1].solo = 2);
    let cases = [
        ("track-mute", json!({"ref": KICK, "mute": true}), "Kick muted"),
        ("track-solo", json!({"ref": KICK, "solo": false}), "Kick unsoloed"),
        ("track-rename", json!({"ref": KICK, "name": "Kick 1"}), "Renamed track “Kick” → “Kick 1”"),
        ("track-color", json!({"ref": KICK, "color": "#ff0000"}), "Kick colour changed"),
        ("track-color", json!({"ref": KICK, "color": null}), "Kick colour cleared"),
    ];
    for (n, (kind, input, title)) in cases.into_iter().enumerate() {
        // Clearing a colour needs one to clear.
        if title == "Kick colour cleared" {
            state(|s| s.tracks[1].color = 0x0000ff | 0x100_0000);
        }
        let before = state(|s| (s.tracks[1].mute, s.tracks[1].solo, s.tracks[1].name.clone(), s.tracks[1].color));
        let (preview, applied) = lab.make(kind, input, &format!("Kumi: {title} (k{n}000)"));
        assert_eq!(preview["display"]["title"], title);
        let changed = state(|s| (s.tracks[1].mute, s.tracks[1].solo, s.tracks[1].name.clone(), s.tracks[1].color));
        assert_ne!(changed, before, "{kind} changed nothing");
        lab.undo(kind, &preview, &applied, &format!("Kumi: undo (k{n}999)")).unwrap();
        assert_eq!(state(|s| (s.tracks[1].mute, s.tracks[1].solo, s.tracks[1].name.clone(), s.tracks[1].color)), before, "{kind}");
    }
    // A colour is REAPER's own order with the flag that says it is set.
    lab.make("track-color", json!({"ref": KICK, "color": "#102030"}), "Kumi: c (k0001)");
    assert_eq!(state(|s| s.tracks[1].color), 0x10 | 0x20 << 8 | 0x30 << 16 | 0x100_0000);
}

#[test]
fn a_bad_request_is_refused_before_anything_is_written() {
    let lab = Lab::new();
    for (kind, input, code) in [
        ("track-volume", json!({"ref": KICK, "volumeDb": "loud"}), "bad-params"),
        ("track-volume", json!({"ref": KICK, "volumeDb": 40.0}), "bad-params"),
        ("track-pan", json!({"ref": KICK, "pan": 2.0}), "bad-params"),
        ("track-mute", json!({"ref": KICK}), "bad-params"),
        ("track-rename", json!({"ref": KICK, "name": "a\nb"}), "bad-params"),
        ("track-color", json!({"ref": KICK, "color": "red"}), "bad-params"),
        ("track-volume", json!({"ref": "r:item:{ITEM-K1}", "volumeDb": 0.0}), "bad-params"),
        ("track-volume", json!({"ref": "r:track:{NOPE}", "volumeDb": 0.0}), "failed"),
        ("track-rename", json!({"ref": "r:track:master", "name": "x"}), "bad-params"),
        ("track-solo", json!({"ref": "r:track:master", "solo": true}), "bad-params"),
        ("track-delete", json!({"ref": "r:track:master"}), "bad-params"),
        ("track-warp", json!({"ref": KICK}), "bad-params"),
    ] {
        let (got, message) = lab.call("change_preview", json!({"kind": kind, "input": input})).unwrap_err();
        assert_eq!(got, code, "{kind} {input}: {message}");
    }
    assert_eq!(lab.call("change_preview", json!({"kind": "track-volume"})).unwrap_err().0, "bad-params");
    // The master can be given a volume, a pan, a mute and a colour.
    for (kind, input) in
        [("track-volume", json!({"volumeDb": -3.0})), ("track-pan", json!({"pan": 0.5})), ("track-mute", json!({"mute": true}))]
    {
        let mut input = input;
        input["ref"] = "r:track:master".into();
        lab.make(kind, input, "Kumi: master (k1111)");
    }
    assert_eq!(state(|s| s.master.pan), 0.5);
}

#[test]
fn a_request_from_another_epoch_or_while_reaper_is_busy_is_refused() {
    let lab = Lab::new();
    let input = json!({"ref": KICK, "volumeDb": -6.0});
    let (code, message) = lab.call("change_preview", json!({"kind": "track-volume", "input": input, "epoch": "old-1"})).unwrap_err();
    assert_eq!(code, "stale");
    assert!(message.contains("old-1"), "{message}");
    assert_eq!(lab.call("change_preview", json!({"kind": "track-volume", "input": input, "epoch": null})).unwrap_err().0, "bad-params");
    let preview = lab.call("change_preview", json!({"kind": "track-volume", "input": input})).unwrap();
    // The tab changed between the preview and the apply.
    lab.reads.bump_epoch();
    let apply =
        json!({"kind": "track-volume", "input": input, "fence": preview["fence"], "desc": "Kumi: x (k1111)", "epoch": preview["epoch"]});
    assert_eq!(lab.call("change_apply", apply.clone()).unwrap_err().0, "stale");
    assert_eq!(state(|s| s.tracks[1].volume), 1.0);
    // And while REAPER loads, renders or has a dialog, nothing is written.
    for set in [|s: &mut fake::State| s.loading = true, |s: &mut fake::State| s.rendering = true] {
        state(set);
        let mut apply = apply.clone();
        apply["epoch"] = lab.reads.epoch().into();
        assert_eq!(lab.call("change_apply", apply).unwrap_err().0, "busy");
        assert_eq!(state(|s| s.tracks[1].volume), 1.0);
        state(|s| {
            s.loading = false;
            s.rendering = false;
        });
    }
}

#[test]
fn a_change_that_refuses_leaves_no_undo_point() {
    let lab = Lab::new();
    let input = json!({"ref": KICK, "volumeDb": -6.0});
    let preview = lab.call("change_preview", json!({"kind": "track-volume", "input": input})).unwrap();
    state(|s| s.tracks[1].volume = 0.5);
    let entries = state(|s| s.undo_entries);
    let _ = lab.call("change_apply", json!({"kind": "track-volume", "input": input, "fence": preview["fence"], "desc": "Kumi: x (k1111)"}));
    assert_eq!(state(|s| s.undo_entries), entries);
    assert_eq!(state(|s| s.undo_depth), 0, "the block was closed");
}

// ----- deleting a track -----

fn sends_of(lab: &Lab, track: &str) -> Vec<Value> {
    let api = fake::api();
    let reads = &lab.reads;
    let answer = reads.read(&Reaper::new(&api), &json!({"kind": "send", "parent": track})).unwrap();
    answer["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| json!({"ref": row["ref"], "volumeDb": row["volumeDb"], "category": row["category"]}))
        .collect()
}

#[test]
fn deleting_a_track_keeps_what_undo_needs_to_make_it_again_with_its_sends_and_state() {
    let lab = Lab::new();
    let reverb = "r:track:{DDDD}";
    state(|s| {
        s.tracks[0].sends[0].volume = 0.25;
        s.tracks[0].sends[0].extra.insert("B_PHASE".into(), 1.0);
        s.tracks[3].selected = true;
    });
    let drums_before = sends_of(&lab, "r:track:{AAAA}");
    let reverb_before = sends_of(&lab, reverb);
    let fx_before = state(|s| s.tracks[3].fx.len());
    let (preview, applied) = lab.make("track-delete", json!({"ref": reverb}), "Kumi: Deleted track Reverb (kdel1)");
    assert_eq!(preview["display"]["title"], "Deleted track “Reverb”");
    assert_eq!(preview["prior"]["index"], 4);
    assert_eq!(state(|s| s.tracks.len()), 3);
    // The send that went to it went with it.
    assert!(state(|s| s.tracks[0].sends.is_empty()));
    let material = &applied["material"];
    assert_eq!((material["guid"].as_str(), material["index"].as_i64()), (Some("{DDDD}"), Some(3)));
    assert!(material["chunk"].as_str().unwrap().starts_with("<TRACK {DDDD}"), "{}", material["chunk"]);
    assert_eq!(material["routes"].as_array().unwrap().len(), 1);
    assert_eq!(material["routes"][0]["other"], "{AAAA}");
    assert_eq!(applied["applied"], json!({"deleted": true}));
    // Undo makes it again: the same id, the same place, its FX, its send with its values.
    let restored = lab.undo("track-delete", &preview, &applied, "Kumi: undo Deleted track Reverb (kund1)").unwrap();
    assert_eq!(restored["restored"]["ref"], reverb);
    assert_eq!(state(|s| s.tracks.len()), 4);
    assert_eq!(state(|s| s.tracks[3].guid.clone()), "{DDDD}");
    assert_eq!(state(|s| s.tracks[3].fx.len()), fx_before);
    assert_eq!(sends_of(&lab, "r:track:{AAAA}"), drums_before);
    assert_eq!(sends_of(&lab, reverb), reverb_before);
    assert_eq!(state(|s| s.tracks[0].sends[0].extra.get("B_PHASE").copied()), Some(1.0));
    assert!(state(|s| s.tracks[3].selected));
    assert_eq!(top().as_deref(), Some("Kumi: undo Deleted track Reverb (kund1)"));
    // The track can be read by the ref it had.
    let api = fake::api();
    let row = lab.reads.read(&Reaper::new(&api), &json!({"ref": reverb})).unwrap();
    assert_eq!(row["items"][0]["name"], "Reverb");
}

#[test]
fn undo_of_a_delete_is_refused_if_the_track_is_back_or_its_state_cant_be_put_back() {
    let lab = Lab::new();
    let (preview, applied) = lab.make("track-delete", json!({"ref": KICK}), "Kumi: Deleted track Kick (kdel1)");
    // Put back once: a track with that id exists again, and nothing is made twice.
    lab.undo("track-delete", &preview, &applied, "Kumi: undo (kund1)").unwrap();
    let (code, _) = lab.undo("track-delete", &preview, &applied, "Kumi: undo (kund2)").unwrap_err();
    assert_eq!(code, "changed");
    assert_eq!(state(|s| s.tracks.len()), 4);
    // A chunk REAPER won't take: what was made is taken away again.
    state(|s| s.tracks.remove(1));
    let mut broken = applied.clone();
    broken["material"]["chunk"] = "<TRACK {BBBB}\nnot a state\n>".into();
    let (code, _) = lab.undo("track-delete", &preview, &broken, "Kumi: undo (kund3)").unwrap_err();
    assert_eq!(code, "failed");
    assert_eq!(state(|s| s.tracks.len()), 3, "the track made for it was removed again");
}

#[test]
fn a_delete_is_refused_when_the_track_changed_since_the_preview() {
    let lab = Lab::new();
    let preview = lab.call("change_preview", json!({"kind": "track-delete", "input": {"ref": KICK}})).unwrap();
    state(|s| s.tracks[1].items.pop());
    let (code, _) = lab
        .call("change_apply", json!({"kind": "track-delete", "input": {"ref": KICK}, "fence": preview["fence"], "desc": "Kumi: x (k1111)"}))
        .unwrap_err();
    assert_eq!(code, "changed");
    assert_eq!(state(|s| s.tracks.len()), 4);
}

#[test]
fn the_sends_a_track_receives_are_taken_out_of_its_chunk_and_nothing_else() {
    let chunk = "<TRACK {AAAA}\n  NAME \"Reverb\"\n  VOLPAN 1 0 -1 -1 1\n  AUXRECV 0 0 1 0 0 0 0 0 0 -1:U\n  <AUXVOLENV\n    EGUID {1111}\n    PT 0 1 0\n  >\n  <FXCHAIN\n    <VST \"ReaVerb\"\n      AUXRECV not a send\n    >\n  >\n  HWOUT 0 0 1 0 0 0 0 -1:U\n>\n";
    let (stripped, dropped) = tracks::without_receives(chunk);
    assert_eq!(dropped, 1);
    assert!(!stripped.contains("AUXRECV 0") && !stripped.contains("AUXVOLENV") && !stripped.contains("EGUID"), "{stripped}");
    assert!(stripped.contains("VOLPAN") && stripped.contains("<FXCHAIN") && stripped.contains("HWOUT"), "{stripped}");
    // A line that only looks like one, deeper in, stays.
    assert!(stripped.contains("AUXRECV not a send"));
    assert_eq!(tracks::without_receives("no chunk at all"), ("no chunk at all".to_string(), 0));
}

// ----- adding and moving tracks -----

fn names() -> Vec<String> {
    state(|s| s.tracks.iter().map(|t| t.name.clone()).collect())
}

fn depths() -> Vec<i32> {
    state(|s| s.tracks.iter().map(|t| t.folder).collect())
}

#[test]
fn adding_a_track_makes_it_where_asked_and_undo_takes_it_away_by_its_id() {
    let lab = Lab::new();
    let (preview, applied) = lab.make("track-add", json!({"name": "Pad", "color": "#102030", "index": 2}), "Kumi: Added track Pad (kadd1)");
    assert_eq!(preview["display"]["title"], "Added track “Pad”");
    assert_eq!(preview["target"], json!({"ref": null, "kind": "track", "index": 1}));
    assert_eq!(names(), ["Drums", "Pad", "Kick", "Snare", "Reverb"]);
    assert_eq!(state(|s| s.tracks[1].color), 0x10 | 0x20 << 8 | 0x30 << 16 | 0x100_0000);
    let created = applied["created"]["ref"].as_str().unwrap().to_string();
    assert!(created.starts_with("r:track:{NEW-"), "{created}");
    assert_eq!((applied["applied"]["index"].clone(), applied["applied"]["name"].clone()), (json!(1), json!("Pad")));
    // The sends that named a track by place still reach it: Drums sends to Reverb, now one further on.
    assert_eq!(state(|s| s.tracks[0].sends[0].dest), 4);
    // (undo of an add is told what was created)
    let without = json!({"kind": "track-add", "target": preview["target"], "prior": preview["prior"], "applied": applied["applied"], "desc": "Kumi: undo (kund0)", "epoch": lab.reads.epoch()});
    let restored = lab.call("change_restore", without);
    assert!(restored.is_err(), "created is needed: {restored:?}");
    let mut params = json!({"kind": "track-add", "target": preview["target"], "prior": preview["prior"], "applied": applied["applied"], "created": applied["created"], "desc": "Kumi: undo Added track Pad (kund1)"});
    params["epoch"] = lab.reads.epoch().into();
    let restored = lab.call("change_restore", params.clone()).unwrap();
    assert_eq!(restored["restored"]["deleted"], created);
    assert_eq!(names(), ["Drums", "Kick", "Snare", "Reverb"]);
    assert_eq!(state(|s| s.tracks[0].sends[0].dest), 3);
    // Taken away already: gone.
    assert_eq!(lab.call("change_restore", params).unwrap_err().0, "gone");
}

#[test]
fn a_track_the_producer_filled_since_is_not_deleted_by_undo_unless_discarded() {
    let lab = Lab::new();
    let (preview, applied) = lab.make("track-add", json!({"name": "Pad"}), "Kumi: Added track Pad (kadd1)");
    state(|s| s.tracks[4].fx.push(crate::reaper::fake::FakeFx::new("{FX-NEW}", "ReaEQ")));
    let mut params = json!({"kind": "track-add", "target": preview["target"], "prior": preview["prior"], "applied": applied["applied"], "created": applied["created"], "desc": "Kumi: undo (kund1)"});
    params["epoch"] = lab.reads.epoch().into();
    let (code, message) = lab.call("change_restore", params.clone()).unwrap_err();
    assert_eq!(code, "changed");
    assert!(message.contains("has items, FX or sends"), "{message}");
    assert_eq!(state(|s| s.tracks.len()), 5);
    params["discard"] = true.into();
    lab.call("change_restore", params).unwrap();
    assert_eq!(state(|s| s.tracks.len()), 4);
}

#[test]
fn a_folder_is_a_track_added_before_the_run_it_holds_and_undo_closes_the_run_as_it_was() {
    let lab = Lab::new();
    assert_eq!(depths(), [1, 0, -1, 0]);
    // Kick and Snare (Snare closes Drums' folder) become the children of a new folder.
    let (preview, applied) =
        lab.make("track-add", json!({"name": "Subgroup", "asFolderOf": [KICK, "r:track:{CCCC}"]}), "Kumi: Added folder Subgroup (kfol1)");
    assert_eq!(preview["display"]["title"], "Added folder “Subgroup”");
    assert_eq!(names(), ["Drums", "Subgroup", "Kick", "Snare", "Reverb"]);
    assert_eq!(depths(), [1, 1, 0, -2, 0], "the new folder opens, and the run closes it and Drums");
    assert_eq!(applied["applied"]["closedAt"], json!({"guid": "{CCCC}", "before": -1, "after": -2}));
    let mut params = json!({"kind": "track-add", "target": preview["target"], "prior": preview["prior"], "applied": applied["applied"], "created": applied["created"], "desc": "Kumi: undo (kund1)"});
    params["epoch"] = lab.reads.epoch().into();
    lab.call("change_restore", params).unwrap();
    assert_eq!(depths(), [1, 0, -1, 0]);
    assert_eq!(names(), ["Drums", "Kick", "Snare", "Reverb"]);
}

#[test]
fn a_bad_add_is_refused_and_one_whose_place_moved_is_changed() {
    let lab = Lab::new();
    for (input, why) in [
        (json!({"index": 9}), "index"),
        (json!({"index": 0}), "from 1"),
        (json!({"index": 6}), "from 1"),
        (json!({"index": -1}), "index"),
        (json!({"name": "a\nb"}), "name"),
        (json!({"color": "red"}), "color"),
        (json!({"asFolderOf": []}), "asFolderOf"),
        (json!({"asFolderOf": ["r:track:master"]}), "master"),
        (json!({"asFolderOf": [KICK, "r:track:{DDDD}"]}), "next to each other"),
        (json!({"asFolderOf": ["r:track:{CCCC}", KICK]}), "next to each other"),
        (json!({"asFolderOf": [KICK], "index": 1}), "not both"),
    ] {
        let (code, message) = lab.call("change_preview", json!({"kind": "track-add", "input": input})).unwrap_err();
        assert_eq!(code, "bad-params", "{input}");
        assert!(message.contains(why), "{input}: {message}");
    }
    let preview = lab.call("change_preview", json!({"kind": "track-add", "input": {"name": "Pad", "index": 2}})).unwrap();
    state(|s| s.tracks.insert(1, crate::reaper::fake::FakeTrack::new("{OTHER}", "Other")));
    let (code, _) = lab
        .call(
            "change_apply",
            json!({"kind": "track-add", "input": {"name": "Pad", "index": 2}, "fence": preview["fence"], "desc": "Kumi: x (k1111)"}),
        )
        .unwrap_err();
    assert_eq!(code, "changed");
    assert_eq!(state(|s| s.tracks.len()), 5);
}

#[test]
fn moving_a_track_puts_it_there_keeps_the_selection_and_undo_puts_it_and_the_depths_back() {
    let lab = Lab::new();
    state(|s| {
        s.tracks[3].selected = true;
    });
    let (preview, applied) = lab.make("track-move", json!({"ref": KICK, "index": 4}), "Kumi: Moved track Kick to position 4 (kmov1)");
    assert_eq!(preview["display"]["title"], "Moved track “Kick” to position 4");
    assert_eq!(names(), ["Drums", "Snare", "Reverb", "Kick"]);
    assert_eq!(applied["applied"]["index"], 3);
    // The producer's selection (Reverb) is as it was, and Kick isn't selected.
    assert_eq!(state(|s| s.tracks.iter().map(|t| t.selected).collect::<Vec<_>>()), [false, false, true, false]);
    // Drums' send to Reverb follows Reverb.
    assert_eq!(state(|s| s.tracks[0].sends[0].dest), 2);
    lab.undo("track-move", &preview, &applied, "Kumi: undo (kund1)").unwrap();
    assert_eq!(names(), ["Drums", "Kick", "Snare", "Reverb"]);
    assert_eq!(depths(), [1, 0, -1, 0]);
    assert_eq!(state(|s| s.tracks[0].sends[0].dest), 3);
    assert_eq!(state(|s| s.tracks.iter().map(|t| t.selected).collect::<Vec<_>>()), [false, false, false, true]);
}

#[test]
fn a_move_is_refused_when_the_tracks_around_it_changed_before_or_after() {
    let lab = Lab::new();
    let preview = lab.call("change_preview", json!({"kind": "track-move", "input": {"ref": KICK, "index": 3}})).unwrap();
    state(|s| s.tracks.swap(2, 3));
    let (code, _) = lab
        .call(
            "change_apply",
            json!({"kind": "track-move", "input": {"ref": KICK, "index": 3}, "fence": preview["fence"], "desc": "Kumi: x (k1111)"}),
        )
        .unwrap_err();
    assert_eq!(code, "changed");
    state(|s| s.tracks.swap(2, 3));
    let (preview, applied) = lab.make("track-move", json!({"ref": KICK, "index": 3}), "Kumi: Moved (kmov1)");
    assert_eq!(names(), ["Drums", "Snare", "Kick", "Reverb"]);
    // The producer moved another track next to it since.
    state(|s| s.tracks.swap(0, 1));
    let (code, message) = lab.undo("track-move", &preview, &applied, "Kumi: undo (kund1)").unwrap_err();
    assert_eq!(code, "changed");
    assert!(message.contains("moved or changed in REAPER since"), "{message}");
    assert_eq!(names(), ["Snare", "Drums", "Kick", "Reverb"]);
    // Index out of range, the master, and no ref are refused up front.
    for input in [
        json!({"ref": KICK, "index": 5}),
        json!({"ref": KICK, "index": 0}),
        json!({"ref": KICK}),
        json!({"ref": "r:track:master", "index": 1}),
        json!({"index": 1}),
    ] {
        assert_eq!(lab.call("change_preview", json!({"kind": "track-move", "input": input})).unwrap_err().0, "bad-params", "{input}");
    }
}

#[test]
fn moving_a_track_to_where_it_is_changes_nothing() {
    let lab = Lab::new();
    let (preview, applied) = lab.make("track-move", json!({"ref": KICK, "index": 2}), "Kumi: Moved (kmov1)");
    assert_eq!(preview["display"]["unchanged"], true);
    assert_eq!(applied["undo"]["entry"], false, "nothing was written, so REAPER made no point");
    assert_eq!(names(), ["Drums", "Kick", "Snare", "Reverb"]);
}

// ----- sends -----

const DRUMS_TO_REVERB: &str = "r:send:{AAAA}:0:{DDDD}";

#[test]
fn a_sends_volume_pan_mute_and_mode_are_changed_and_put_back_exactly() {
    let lab = Lab::new();
    state(|s| s.tracks[0].sends[0].mode = 3);
    let cases = [
        ("send-volume", json!({"ref": DRUMS_TO_REVERB, "volumeDb": -12.0}), "Send “Drums” → “Reverb” −12 dB"),
        ("send-pan", json!({"ref": DRUMS_TO_REVERB, "pan": 0.5}), "Send “Drums” → “Reverb” pan 50% right"),
        ("send-mute", json!({"ref": DRUMS_TO_REVERB, "mute": true}), "Send “Drums” → “Reverb” muted"),
        ("send-mode", json!({"ref": DRUMS_TO_REVERB, "mode": "pre-fx"}), "Send “Drums” → “Reverb” pre-fx"),
    ];
    for (n, (kind, input, title)) in cases.into_iter().enumerate() {
        let before = state(|s| {
            let send = &s.tracks[0].sends[0];
            (send.volume, send.pan, send.mute, send.mode)
        });
        let (preview, applied) = lab.make(kind, input, &format!("Kumi: {title} (ks{n}00)"));
        assert_eq!(preview["target"], json!({"ref": DRUMS_TO_REVERB, "kind": "send"}));
        assert_eq!(preview["display"]["title"], title);
        assert_eq!(applied["undo"]["entry"], true);
        let changed = state(|s| {
            let send = &s.tracks[0].sends[0];
            (send.volume, send.pan, send.mute, send.mode)
        });
        assert_ne!(changed, before, "{kind}");
        lab.undo(kind, &preview, &applied, &format!("Kumi: undo (ks{n}99)")).unwrap();
        assert_eq!(
            state(|s| {
                let send = &s.tracks[0].sends[0];
                (send.volume, send.pan, send.mute, send.mode)
            }),
            before,
            "{kind}"
        );
    }
}

#[test]
fn a_send_change_is_refused_when_the_send_or_its_neighbours_moved() {
    let lab = Lab::new();
    let input = json!({"ref": DRUMS_TO_REVERB, "volumeDb": -6.0});
    let preview = lab.call("change_preview", json!({"kind": "send-volume", "input": input})).unwrap();
    // The producer added a second send to the same track: the ref names neither now.
    state(|s| s.tracks[0].sends.push(FakeSend::to(3)));
    let (code, message) = lab
        .call("change_apply", json!({"kind": "send-volume", "input": input, "fence": preview["fence"], "desc": "Kumi: x (k1111)"}))
        .unwrap_err();
    assert_eq!(code, "changed");
    assert!(message.contains("2 such send"), "{message}");
    assert!(state(|s| s.tracks[0].sends[0].volume == 1.0));
    // The value moved between the preview and the apply.
    state(|s| {
        s.tracks[0].sends.pop();
        s.tracks[0].sends[0].volume = 0.5;
    });
    assert_eq!(
        lab.call("change_apply", json!({"kind": "send-volume", "input": input, "fence": preview["fence"], "desc": "Kumi: x (k1111)"}))
            .unwrap_err()
            .0,
        "changed"
    );
    // The ways to ask wrongly.
    for (kind, input) in [
        ("send-volume", json!({"ref": "r:send:{DDDD}:-1:{AAAA}", "volumeDb": 0.0})),
        ("send-volume", json!({"ref": "r:send:{DDDD}:1:hw2", "volumeDb": 0.0})),
        ("send-volume", json!({"ref": "r:track:{AAAA}", "volumeDb": 0.0})),
        ("send-volume", json!({"ref": DRUMS_TO_REVERB, "volumeDb": 99.0})),
        ("send-mode", json!({"ref": DRUMS_TO_REVERB, "mode": "sideways"})),
        ("send-add", json!({"ref": "r:track:{AAAA}", "toRef": "r:track:{AAAA}"})),
        ("send-add", json!({"ref": "r:track:master", "toRef": "r:track:{AAAA}"})),
        ("send-add", json!({"ref": "r:track:{AAAA}"})),
    ] {
        assert_eq!(lab.call("change_preview", json!({"kind": kind, "input": input})).unwrap_err().0, "bad-params", "{kind} {input}");
    }
    assert_eq!(
        lab.call("change_preview", json!({"kind": "send-volume", "input": {"ref": "r:send:{AAAA}:0:{NOPE}", "volumeDb": 0.0}}))
            .unwrap_err()
            .0,
        "failed"
    );
}

#[test]
fn adding_a_send_makes_it_with_its_settings_and_undo_removes_exactly_that_one() {
    let lab = Lab::new();
    let input = json!({"ref": KICK, "toRef": "r:track:{DDDD}", "volumeDb": -9.0, "mode": "pre-fader"});
    let (preview, applied) = lab.make("send-add", input, "Kumi: Added send Kick → Reverb (ksa01)");
    assert_eq!(preview["display"]["title"], "Added send “Kick” → “Reverb”");
    assert_eq!(applied["created"]["ref"], "r:send:{BBBB}:0:{DDDD}");
    assert_eq!(applied["created"]["kind"], "send");
    assert!(applied["created"]["stamp"].as_str().is_some_and(|stamp| !stamp.is_empty()), "Kumi's own send carries its mark");
    assert_eq!(state(|s| s.tracks[1].sends.len()), 1);
    assert!((state(|s| s.tracks[1].sends[0].volume) - 10f64.powf(-9.0 / 20.0)).abs() < 1e-12);
    assert_eq!(state(|s| s.tracks[1].sends[0].mode), 3);
    let mut params = json!({"kind": "send-add", "target": preview["target"], "prior": preview["prior"], "applied": applied["applied"], "created": applied["created"], "desc": "Kumi: undo (kund1)"});
    params["epoch"] = lab.reads.epoch().into();
    lab.call("change_restore", params.clone()).unwrap();
    assert!(state(|s| s.tracks[1].sends.is_empty()));
    assert_eq!(lab.call("change_restore", params).unwrap_err().0, "gone");
}

#[test]
fn a_second_send_to_the_same_track_is_told_apart_and_undone_alone() {
    let lab = Lab::new();
    let (preview, applied) = lab.make(
        "send-add",
        json!({"ref": "r:track:{AAAA}", "toRef": "r:track:{DDDD}", "volumeDb": -6.0}),
        "Kumi: Added send Drums → Reverb (ksa01)",
    );
    // One of two sends to Reverb: its ref carries the state of both, as a read would give it.
    assert_eq!(applied["created"]["ref"].as_str(), Some(send_refs(0)[1].as_str()));
    assert!(send_refs(0)[1].starts_with("r:send:{AAAA}:0:{DDDD}:2of2."), "{}", send_refs(0)[1]);
    assert_eq!(preview["display"]["note"], "There are 1 send(s) between them already.");
    // The old send's ref is now 1of2, so a ref made before reads nothing; the new one is told.
    let mut params = json!({"kind": "send-add", "target": preview["target"], "prior": preview["prior"], "applied": applied["applied"], "created": applied["created"], "desc": "Kumi: undo (kund1)"});
    params["epoch"] = lab.reads.epoch().into();
    // The producer changed the new send since.
    state(|s| s.tracks[0].sends[1].volume = 0.9);
    let (code, _) = lab.call("change_restore", params.clone()).unwrap_err();
    assert_eq!(code, "changed");
    state(|s| s.tracks[0].sends[1].volume = 10f64.powf(-6.0 / 20.0));
    lab.call("change_restore", params).unwrap();
    assert_eq!(state(|s| s.tracks[0].sends.len()), 1);
    assert_eq!(state(|s| s.tracks[0].sends[0].volume), 1.0);
}

#[test]
fn removing_a_send_keeps_its_settings_and_undo_makes_it_again() {
    let lab = Lab::new();
    state(|s| {
        let send = &mut s.tracks[0].sends[0];
        send.volume = 0.3;
        send.pan = -0.2;
        send.mode = 1;
        send.extra.insert("B_PHASE".into(), 1.0);
    });
    let (preview, applied) = lab.make("send-remove", json!({"ref": DRUMS_TO_REVERB}), "Kumi: Removed send Drums → Reverb (ksr01)");
    assert_eq!(preview["display"]["title"], "Removed send “Drums” → “Reverb”");
    assert!(state(|s| s.tracks[0].sends.is_empty()));
    assert_eq!(applied["material"]["receiver"], "{DDDD}");
    lab.undo("send-remove", &preview, &applied, "Kumi: undo (kund1)").unwrap();
    let (volume, pan, mode, phase) = state(|s| {
        let send = &s.tracks[0].sends[0];
        (send.volume, send.pan, send.mode, send.extra.get("B_PHASE").copied())
    });
    assert_eq!((volume, pan, mode, phase), (0.3, -0.2, 1, Some(1.0)));
    // Undone twice: it is back, and nothing is made again.
    lab.undo("send-remove", &preview, &applied, "Kumi: undo (kund2)").unwrap();
    assert_eq!(state(|s| s.tracks[0].sends.len()), 1);
    // The receiver went: the send can't be made again.
    let (preview, applied) = lab.make("send-remove", json!({"ref": DRUMS_TO_REVERB}), "Kumi: Removed send Drums → Reverb (ksr02)");
    state(|s| {
        s.tracks.remove(3);
    });
    assert_eq!(lab.undo("send-remove", &preview, &applied, "Kumi: undo (kund3)").unwrap_err().0, "gone");
}

// ----- markers and regions -----

fn markers() -> Vec<(String, f64, f64, String, i32)> {
    state(|s| s.markers.iter().map(|m| (m.guid.clone(), m.start, m.end, m.name.clone(), m.number)).collect())
}

#[test]
fn a_marker_is_added_where_asked_in_beats_or_seconds_and_undo_takes_that_one_away() {
    let lab = Lab::new();
    let before = markers();
    let (preview, applied) =
        lab.make("marker-add", json!({"positionBeats": 8.0, "name": "Intro", "color": "#102030"}), "Kumi: Marker Intro at bar 3 (kma01)");
    assert_eq!(preview["display"]["title"], "Marker “Intro” at bar 3");
    assert_eq!(applied["created"]["kind"], "marker");
    let now = markers();
    assert_eq!(now.len(), before.len() + 1);
    let made = now.last().unwrap();
    assert_eq!((made.1, made.3.as_str()), (4.0, "Intro"), "8 quarter notes at 120 bpm");
    assert_eq!(state(|s| s.markers.last().unwrap().color), 0x10 | 0x20 << 8 | 0x30 << 16 | 0x100_0000);
    let mut params = json!({"kind": "marker-add", "target": preview["target"], "prior": preview["prior"], "applied": applied["applied"], "created": applied["created"], "desc": "Kumi: undo (kund1)"});
    params["epoch"] = lab.reads.epoch().into();
    // The producer renamed it since: undo is refused, unless discarded.
    state(|s| s.markers.last_mut().unwrap().name = "Renamed".into());
    assert_eq!(lab.call("change_restore", params.clone()).unwrap_err().0, "changed");
    state(|s| s.markers.last_mut().unwrap().name = "Intro".into());
    lab.call("change_restore", params.clone()).unwrap();
    assert_eq!(markers(), before);
    assert_eq!(lab.call("change_restore", params).unwrap_err().0, "gone");
    // In seconds.
    let (_, applied) = lab.make("marker-add", json!({"position": 12.5}), "Kumi: Marker at bar 7 beat 2 (kma02)");
    assert_eq!(state(|s| s.markers.last().unwrap().start), 12.5);
    assert!(applied["display"]["title"].as_str().unwrap().starts_with("Marker at bar"));
}

#[test]
fn a_region_needs_an_end_after_its_start_and_a_marker_has_none() {
    let lab = Lab::new();
    let (preview, applied) = lab.make(
        "region-add",
        json!({"positionBeats": 16.0, "endBeats": 32.0, "name": "Chorus 2"}),
        "Kumi: Region Chorus 2 from bar 5 to bar 9 (kra01)",
    );
    assert_eq!(preview["display"]["title"], "Region “Chorus 2” from bar 5 to bar 9");
    assert_eq!(applied["created"]["kind"], "region");
    assert_eq!(
        state(|s| (s.markers.last().unwrap().start, s.markers.last().unwrap().end, s.markers.last().unwrap().is_region)),
        (8.0, 16.0, true)
    );
    for (kind, input) in [
        ("region-add", json!({"positionBeats": 16.0})),
        ("region-add", json!({"positionBeats": 16.0, "endBeats": 16.0})),
        ("region-add", json!({"positionBeats": 16.0, "end": 5.0})),
        ("marker-add", json!({})),
        ("marker-add", json!({"positionBeats": 1.0, "position": 1.0})),
        ("marker-add", json!({"positionBeats": -1.0})),
        ("marker-add", json!({"position": 1.0, "name": "a\nb"})),
        ("marker-add", json!({"position": 1.0, "color": "pink"})),
        ("marker-move", json!({"ref": "r:marker:{MARK-1}", "position": 1.0, "end": 3.0})),
        ("marker-move", json!({"ref": "r:track:{AAAA}", "position": 1.0})),
        ("marker-rename", json!({"ref": "r:marker:{MARK-1}"})),
    ] {
        assert_eq!(lab.call("change_preview", json!({"kind": kind, "input": input})).unwrap_err().0, "bad-params", "{kind} {input}");
    }
    assert_eq!(
        lab.call("change_preview", json!({"kind": "marker-move", "input": {"ref": "r:marker:{NOPE}", "position": 1.0}})).unwrap_err().0,
        "failed"
    );
    // A marker ref is not a region's.
    assert_eq!(
        lab.call("change_preview", json!({"kind": "marker-rename", "input": {"ref": "r:region:{MARK-1}", "name": "x"}})).unwrap_err().0,
        "failed"
    );
}

#[test]
fn a_marker_is_moved_renamed_and_coloured_and_each_goes_back() {
    let lab = Lab::new();
    // Drop is at 4 s; Verse is the region from 8 s to 16 s.
    let (preview, applied) =
        lab.make("marker-move", json!({"ref": "r:marker:{MARK-1}", "positionBeats": 40.0}), "Kumi: Moved marker Drop to bar 11 (kmm01)");
    assert_eq!(preview["display"]["title"], "Moved marker “Drop” to bar 11");
    assert_eq!(state(|s| s.markers[0].start), 20.0);
    lab.undo("marker-move", &preview, &applied, "Kumi: undo (kund1)").unwrap();
    assert_eq!(state(|s| s.markers[0].start), 4.0);
    // A region moved by its start keeps its length; one moved with an end is resized.
    let (preview, applied) =
        lab.make("marker-move", json!({"ref": "r:region:{REG-1}", "position": 20.0}), "Kumi: Moved region Verse (kmm02)");
    assert_eq!(state(|s| (s.markers[1].start, s.markers[1].end)), (20.0, 28.0));
    lab.undo("marker-move", &preview, &applied, "Kumi: undo (kund2)").unwrap();
    assert_eq!(state(|s| (s.markers[1].start, s.markers[1].end)), (8.0, 16.0));
    let (preview, applied) =
        lab.make("marker-move", json!({"ref": "r:region:{REG-1}", "position": 6.0, "end": 12.0}), "Kumi: Moved region Verse (kmm03)");
    assert_eq!(state(|s| (s.markers[1].start, s.markers[1].end)), (6.0, 12.0));
    lab.undo("marker-move", &preview, &applied, "Kumi: undo (kund3)").unwrap();
    assert_eq!(state(|s| (s.markers[1].start, s.markers[1].end)), (8.0, 16.0));
    // Name and colour.
    let (preview, applied) = lab.make(
        "marker-rename",
        json!({"ref": "r:marker:{MARK-1}", "name": "Breakdown"}),
        "Kumi: Renamed marker Drop to Breakdown (kmr01)",
    );
    assert_eq!(preview["display"]["title"], "Renamed marker “Drop” → “Breakdown”");
    assert_eq!(state(|s| s.markers[0].name.clone()), "Breakdown");
    lab.undo("marker-rename", &preview, &applied, "Kumi: undo (kund4)").unwrap();
    assert_eq!(state(|s| s.markers[0].name.clone()), "Drop");
    let green = state(|s| s.markers[1].color);
    let (preview, applied) =
        lab.make("marker-color", json!({"ref": "r:region:{REG-1}", "color": "#ff0000"}), "Kumi: Region Verse colour changed (kmc01)");
    assert_eq!(state(|s| s.markers[1].color), 0xff | 0x100_0000);
    lab.undo("marker-color", &preview, &applied, "Kumi: undo (kund5)").unwrap();
    assert_eq!(state(|s| s.markers[1].color), green);
    // Moved since: refused.
    let (preview, applied) = lab.make("marker-move", json!({"ref": "r:marker:{MARK-1}", "position": 9.0}), "Kumi: Moved (kmm04)");
    state(|s| s.markers[0].start = 11.0);
    assert_eq!(lab.undo("marker-move", &preview, &applied, "Kumi: undo (kund6)").unwrap_err().0, "changed");
    // A change fenced by where it was is refused when it moved before the apply.
    let preview = lab.call("change_preview", json!({"kind": "marker-rename", "input": {"ref": "r:marker:{MARK-1}", "name": "X"}})).unwrap();
    state(|s| s.markers[0].name = "Other".into());
    assert_eq!(lab.call("change_apply", json!({"kind": "marker-rename", "input": {"ref": "r:marker:{MARK-1}", "name": "X"}, "fence": preview["fence"], "desc": "Kumi: x (k1111)"})).unwrap_err().0, "changed");
}

#[test]
fn deleting_a_marker_keeps_it_to_make_again_with_a_new_id_and_its_number() {
    let lab = Lab::new();
    state(|s| s.markers[1].number = 7);
    let (preview, applied) = lab.make("marker-delete", json!({"ref": "r:region:{REG-1}"}), "Kumi: Deleted region Verse (kmd01)");
    assert_eq!(preview["display"]["title"], "Deleted region “Verse”");
    assert_eq!(state(|s| s.markers.len()), 1);
    let restored = lab.undo("marker-delete", &preview, &applied, "Kumi: undo (kund1)").unwrap();
    assert_eq!(restored["restored"]["was"], "r:region:{REG-1}");
    assert!(restored["restored"]["ref"].as_str().unwrap().starts_with("r:region:{MK-NEW-"), "{restored}");
    let (start, end, name, number, region) = state(|s| {
        let m = s.markers.last().unwrap();
        (m.start, m.end, m.name.clone(), m.number, m.is_region)
    });
    assert_eq!((start, end, name.as_str(), number, region), (8.0, 16.0, "Verse", 7, true));
    // A second undo finds a marker like it and makes nothing again... by the old id it finds none, so it makes another:
    // the journal only undoes once, so this is the producer's own doing.
    assert_eq!(state(|s| s.markers.len()), 2);
}

// ----- a plan -----

#[test]
fn a_plan_is_one_undo_block_and_its_changes_open_none_of_their_own() {
    let lab = Lab::new();
    let step = lab.call("undo_step_begin", json!({"label": "Kumi: balance the drums (kplan)"})).unwrap();
    let id = step["stepId"].as_str().unwrap().to_string();
    assert!(lab.call("undo_step_begin", json!({"label": "again"})).unwrap_err().1.contains("open already"));
    let before = state(|s| s.undo_entries);
    let (_, first) = lab.make("track-volume", json!({"ref": KICK, "volumeDb": -3.0}), "Kumi: Kick volume −3 dB (kp001)");
    let (_, second) = lab.make("track-pan", json!({"ref": KICK, "pan": 0.2}), "Kumi: Kick pan 20% right (kp002)");
    // Inside the plan nothing says yet whether REAPER made a point.
    assert_eq!((first["undo"]["entry"].clone(), second["undo"]["inPlan"].clone()), (Value::Null, json!(true)));
    assert_eq!(state(|s| s.undo_entries), before, "no point until the plan ends");
    assert_eq!(lab.call("undo_step_end", json!({"stepId": "nope"})).unwrap_err().0, "failed");
    let ended = lab.call("undo_step_end", json!({"stepId": id})).unwrap();
    assert_eq!(ended["desc"], "Kumi: balance the drums (kplan)");
    assert_eq!(ended["entry"], true);
    assert_eq!(state(|s| s.undo_entries), before + 1);
    assert_eq!(top().as_deref(), Some("Kumi: balance the drums (kplan)"));
    // Over: a second end finds nothing open.
    assert_eq!(lab.call("undo_step_end", json!({"stepId": id})).unwrap_err().0, "failed");
}

#[test]
fn a_step_in_a_plan_says_whether_the_plans_point_takes_it_back() {
    let lab = Lab::new();
    let step = lab.call("undo_step_begin", json!({"label": "Kumi: a plan (kcov1)"})).unwrap();
    let covered = |answer: &Value| answer["undo"]["covered"].clone();
    // A change that is written is in the plan's point.
    let (_, volume) = lab.make("track-volume", json!({"ref": KICK, "volumeDb": -3.0}), "Kumi: Kick volume −3 dB (kcv01)");
    assert_eq!(covered(&volume), json!(true));
    // The repeat button leaves REAPER no history to take it back from.
    let (_, repeat) = lab.make("loop-repeat", json!({"on": true}), "Kumi: Repeat on (kcv02)");
    assert_eq!(covered(&repeat), json!(false));
    assert!(state(|s| s.repeat));
    // And a change that found everything as asked wrote nothing.
    let (_, again) = lab.make("track-volume", json!({"ref": KICK, "volumeDb": -3.0}), "Kumi: Kick volume −3 dB (kcv03)");
    assert_eq!(covered(&again), json!(false));
    // Outside a plan the answer says what REAPER's history shows, as before.
    lab.call("undo_step_end", json!({"stepId": step["stepId"]})).unwrap();
    let (_, alone) = lab.make("track-pan", json!({"ref": KICK, "pan": 0.2}), "Kumi: Kick pan 20% right (kcv04)");
    assert_eq!((alone["undo"]["entry"].clone(), alone["undo"].get("covered").cloned()), (json!(true), None));
}

#[test]
fn a_plan_left_open_is_closed_when_the_project_changes_so_no_block_stays_open() {
    let lab = Lab::new();
    lab.call("undo_step_begin", json!({"label": "Kumi: plan (kplan)"})).unwrap();
    lab.make("track-mute", json!({"ref": KICK, "mute": true}), "Kumi: Kick muted (kp001)");
    assert_eq!(state(|s| s.undo_depth), 1);
    lab.reads.bump_epoch();
    lab.tick();
    assert_eq!(state(|s| s.undo_depth), 0);
    // Or when it was never ended.
    lab.call("undo_step_begin", json!({"label": "Kumi: plan (kplan)"})).unwrap();
    lab.changes.stop(&Reaper::new(&fake::api()), &lab.reads);
    assert_eq!(state(|s| s.undo_depth), 0);
}

// ----- the producer's undo -----

#[test]
fn the_producer_undoing_and_redoing_kumis_change_is_an_event_naming_its_description() {
    let lab = Lab::new();
    let desc = "Kumi: Kick volume −6 dB (k7f3a)";
    lab.make("track-volume", json!({"ref": KICK, "volumeDb": -6.0}), desc);
    lab.tick();
    assert!(lab.events.take().is_empty(), "Kumi's own point is not the producer's undo");
    fake::producer_undo();
    lab.tick();
    assert_eq!(lab.events.take(), [("undone_in_daw".to_string(), json!({"descs": [desc], "how": "undo", "epoch": lab.reads.epoch()}))]);
    fake::producer_redo();
    lab.tick();
    assert_eq!(lab.events.take(), [("undone_in_daw".to_string(), json!({"descs": [desc], "how": "redo", "epoch": lab.reads.epoch()}))]);
    // Nothing moved: nothing said.
    lab.tick();
    assert!(lab.events.take().is_empty());
}

#[test]
fn several_points_undone_at_once_are_all_named_and_the_producers_own_are_not() {
    let lab = Lab::new();
    lab.make("track-volume", json!({"ref": KICK, "volumeDb": -6.0}), "Kumi: one (kaaaa)");
    fake::producer_edit("Adjust track pan");
    lab.make("track-mute", json!({"ref": KICK, "mute": true}), "Kumi: two (kbbbb)");
    lab.tick();
    lab.events.take();
    fake::producer_undo();
    fake::producer_undo();
    fake::producer_undo();
    lab.tick();
    let events = lab.events.take();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].1["descs"], json!(["Kumi: one (kaaaa)", "Kumi: two (kbbbb)"]));
    assert_eq!(events[0].1["how"], "undo");
    // Redone, two of the three: only the producer's own edit and Kumi's first.
    fake::producer_redo();
    fake::producer_redo();
    lab.tick();
    let events = lab.events.take();
    assert_eq!((events.len(), events[0].1["descs"].clone(), events[0].1["how"].clone()), (1, json!(["Kumi: one (kaaaa)"]), json!("redo")));
}

#[test]
fn a_new_edit_that_drops_what_could_have_been_redone_is_no_redo() {
    let lab = Lab::new();
    lab.make("track-volume", json!({"ref": KICK, "volumeDb": -6.0}), "Kumi: one (kaaaa)");
    fake::producer_undo();
    lab.tick();
    lab.events.take();
    // The producer edits instead of redoing: the change can't come back, and was not redone.
    fake::producer_edit("Adjust track pan");
    lab.tick();
    assert!(lab.events.take().is_empty());
    // Undoing something that isn't Kumi's says nothing.
    fake::producer_undo();
    lab.tick();
    assert!(lab.events.take().is_empty());
}

#[test]
fn a_producer_undo_noticed_after_a_hook_nudge_even_when_the_count_didnt_move() {
    let lab = Lab::new();
    lab.make("track-volume", json!({"ref": KICK, "volumeDb": -6.0}), "Kumi: one (kaaaa)");
    lab.tick();
    lab.events.take();
    state(|s| s.undo_cur = Some(s.undo_entries - 2));
    lab.changes.nudge();
    lab.tick();
    assert_eq!(lab.events.take().len(), 1);
}

#[test]
fn a_new_project_in_the_tab_starts_the_history_afresh() {
    let lab = Lab::new();
    lab.make("track-volume", json!({"ref": KICK, "volumeDb": -6.0}), "Kumi: one (kaaaa)");
    lab.tick();
    lab.events.take();
    lab.reads.bump_epoch();
    state(|s| {
        s.undo_entries = 1;
        s.undo_descs.clear();
        s.undo_cur = None;
    });
    lab.tick();
    assert!(lab.events.take().is_empty(), "another project's history is not the producer undoing Kumi's change");
}

// ----- transport -----

#[test]
fn transport_stop_stops_what_plays_and_says_whether_it_did() {
    let lab = Lab::new();
    assert_eq!(lab.call("transport_stop", json!({})).unwrap(), json!({"stopped": false, "wasRecording": false}));
    assert!(state(|s| s.actions.is_empty()));
    state(|s| s.play_bits = 1 | 4);
    assert_eq!(lab.call("transport_stop", json!({})).unwrap(), json!({"stopped": true, "wasRecording": true}));
    assert_eq!(state(|s| s.actions.clone()), [1016]);
}

#[test]
fn a_send_that_is_not_in_the_project_any_more_is_skipped_when_a_deleted_track_is_made_again() {
    let lab = Lab::new();
    let (preview, applied) = lab.make("track-delete", json!({"ref": "r:track:{DDDD}"}), "Kumi: Deleted track Reverb (kdel1)");
    // The track that sent to it went too.
    state(|s| {
        s.tracks.remove(0);
    });
    let restored = lab.undo("track-delete", &preview, &applied, "Kumi: undo (kund1)").unwrap();
    assert_eq!(restored["restored"]["routes"], 0);
}

// ----- the timeline -----

fn restore_params(preview: &Value, applied: &Value, desc: &str) -> Value {
    json!({"kind": "", "target": preview["target"], "prior": preview["prior"], "applied": applied["applied"], "desc": desc})
}

fn tempo_markers() -> Vec<(f64, f64, i32, i32, bool)> {
    state(|s| s.tempo_markers.iter().map(|m| (m.time, m.bpm, m.num, m.denom, m.linear)).collect())
}

#[test]
fn the_tempo_is_set_in_one_named_block_and_put_back_by_value() {
    let lab = Lab::new();
    let (preview, applied) = lab.make("tempo-set", json!({"bpm": 90.0}), "Kumi: Tempo 90 BPM (ktm01)");
    assert_eq!((&preview["display"]["title"], &preview["prior"]), (&json!("Tempo 90 BPM"), &json!({"bpm": 120.0})));
    assert_eq!((state(|s| s.bpm), top().as_deref()), (90.0, Some("Kumi: Tempo 90 BPM (ktm01)")));
    assert_eq!(applied["applied"], json!({"bpm": 90.0}));
    // The producer moved it since: undo is refused, unless discarded.
    state(|s| s.bpm = 100.0);
    let (code, message) = lab.undo("tempo-set", &preview, &applied, "Kumi: undo (kund1)").unwrap_err();
    assert_eq!(code, "changed");
    assert!(message.contains("tempo"), "{message}");
    assert_eq!(state(|s| s.bpm), 100.0);
    state(|s| s.bpm = 90.0);
    lab.undo("tempo-set", &preview, &applied, "Kumi: undo (kund1)").unwrap();
    assert_eq!((state(|s| s.bpm), top().as_deref()), (120.0, Some("Kumi: undo (kund1)")));
    // Undone already: nothing to write, and no complaint.
    lab.undo("tempo-set", &preview, &applied, "Kumi: undo (kund2)").unwrap();
    let mut params = restore_params(&preview, &applied, "Kumi: undo (kund3)");
    params["kind"] = "tempo-set".into();
    params["discard"] = true.into();
    state(|s| s.bpm = 77.0);
    lab.call("change_restore", params).unwrap();
    assert_eq!(state(|s| s.bpm), 120.0);
}

#[test]
fn a_tempo_change_is_refused_when_the_tempo_moved_or_the_project_has_a_tempo_map() {
    let lab = Lab::new();
    let preview = lab.call("change_preview", json!({"kind": "tempo-set", "input": {"bpm": 90.0}})).unwrap();
    state(|s| s.bpm = 110.0);
    let entries = state(|s| s.undo_entries);
    let apply = json!({"kind": "tempo-set", "input": {"bpm": 90.0}, "fence": preview["fence"], "desc": "Kumi: x (k1111)"});
    assert_eq!(lab.call("change_apply", apply).unwrap_err().0, "changed");
    assert_eq!((state(|s| s.bpm), state(|s| s.undo_entries)), (110.0, entries));
    for bpm in [json!(0.5), json!(2000.0), json!("fast"), Value::Null] {
        assert_eq!(lab.call("change_preview", json!({"kind": "tempo-set", "input": {"bpm": bpm}})).unwrap_err().0, "bad-params");
    }
    // Setting what it already is says so.
    let same = lab.call("change_preview", json!({"kind": "tempo-set", "input": {"bpm": 110.0}})).unwrap();
    assert_eq!(same["display"]["unchanged"], true);
    // With a tempo map the project has no one tempo.
    state(|s| s.tempo_markers.push(fake::FakeTempo { time: 0.0, bpm: 110.0, num: -1, denom: -1, linear: false }));
    let (code, message) = lab.call("change_preview", json!({"kind": "tempo-set", "input": {"bpm": 90.0}})).unwrap_err();
    assert_eq!(code, "bad-params");
    assert!(message.contains("tempo-marker-set"), "{message}");
}

#[test]
fn a_tempo_marker_is_made_where_asked_with_its_time_signature_and_undo_takes_that_one_away() {
    let lab = Lab::new();
    state(|s| s.tempo_markers.push(fake::FakeTempo { time: 0.0, bpm: 120.0, num: 4, denom: 4, linear: false }));
    let before = tempo_markers();
    let input = json!({"positionBeats": 32.0, "bpm": 140.0, "timeSignature": [3, 4]});
    let (preview, applied) = lab.make("tempo-marker-set", input, "Kumi: Tempo 140 BPM and time signature 3/4 at bar 9 (ktm02)");
    assert_eq!(preview["prior"], json!({"exists": false}));
    assert_eq!(preview["target"]["kind"], "tempo-marker");
    assert_eq!(tempo_markers(), [before[0], (16.0, 140.0, 3, 4, false)]);
    assert_eq!(applied["applied"], json!({"exists": true, "bpm": 140.0, "num": 3, "den": 4, "linear": false}));
    // The producer changed it since: undo is refused, unless discarded.
    state(|s| s.tempo_markers[1].bpm = 150.0);
    let mut params = restore_params(&preview, &applied, "Kumi: undo (kund1)");
    params["kind"] = "tempo-marker-set".into();
    assert_eq!(lab.call("change_restore", params.clone()).unwrap_err().0, "changed");
    assert_eq!(tempo_markers().len(), 2);
    state(|s| s.tempo_markers[1].bpm = 140.0);
    lab.call("change_restore", params.clone()).unwrap();
    assert_eq!(tempo_markers(), before);
    // Undone already.
    lab.call("change_restore", params).unwrap();
    assert_eq!(tempo_markers(), before);
}

#[test]
fn a_marker_that_is_there_is_changed_by_the_part_asked_and_put_back_whole() {
    let lab = Lab::new();
    state(|s| s.tempo_markers.push(fake::FakeTempo { time: 8.0, bpm: 100.0, num: 3, denom: 4, linear: true }));
    // Only the tempo: the time signature and the shape stay.
    let (preview, applied) = lab.make("tempo-marker-set", json!({"position": 8.0, "bpm": 128.0}), "Kumi: Tempo 128 BPM at bar 5 (ktm03)");
    assert_eq!(preview["prior"], json!({"exists": true, "bpm": 100.0, "num": 3, "den": 4, "linear": true}));
    assert_eq!(tempo_markers(), [(8.0, 128.0, 3, 4, true)]);
    // Only the time signature, and the shape back to a jump.
    let (preview2, applied2) = lab.make(
        "tempo-marker-set",
        json!({"position": 8.0, "timeSignature": {"num": 7, "den": 8}, "linear": false}),
        "Kumi: Time signature 7/8 at bar 5 (ktm04)",
    );
    assert_eq!(tempo_markers(), [(8.0, 128.0, 7, 8, false)]);
    lab.undo("tempo-marker-set", &preview2, &applied2, "Kumi: undo (kund2)").unwrap();
    assert_eq!(tempo_markers(), [(8.0, 128.0, 3, 4, true)]);
    lab.undo("tempo-marker-set", &preview, &applied, "Kumi: undo (kund1)").unwrap();
    assert_eq!(tempo_markers(), [(8.0, 100.0, 3, 4, true)]);
}

#[test]
fn a_tempo_marker_request_names_a_place_and_something_to_change() {
    let lab = Lab::new();
    for input in [
        json!({"bpm": 120.0}),
        json!({"position": 4.0}),
        json!({"position": 4.0, "positionBeats": 8.0, "bpm": 120.0}),
        json!({"position": 4.0, "bpm": 0.0}),
        json!({"position": 4.0, "timeSignature": [4, 3]}),
        json!({"position": 4.0, "timeSignature": [0, 4]}),
        json!({"position": 4.0, "timeSignature": "3/4"}),
        json!({"position": 4.0, "bpm": 100.0, "linear": "yes"}),
        json!({"position": -1.0, "bpm": 100.0}),
    ] {
        let (code, message) = lab.call("change_preview", json!({"kind": "tempo-marker-set", "input": input})).unwrap_err();
        assert_eq!(code, "bad-params", "{input}: {message}");
    }
    assert!(tempo_markers().is_empty());
}

#[test]
fn a_tempo_marker_change_is_refused_when_the_markers_there_moved() {
    let lab = Lab::new();
    state(|s| s.tempo_markers.push(fake::FakeTempo { time: 8.0, bpm: 100.0, num: -1, denom: -1, linear: false }));
    let input = json!({"position": 8.0, "bpm": 128.0});
    let preview = lab.call("change_preview", json!({"kind": "tempo-marker-set", "input": input})).unwrap();
    state(|s| s.tempo_markers[0].bpm = 90.0);
    let entries = state(|s| s.undo_entries);
    let apply = json!({"kind": "tempo-marker-set", "input": input, "fence": preview["fence"], "desc": "Kumi: x (k1111)"});
    assert_eq!(lab.call("change_apply", apply.clone()).unwrap_err().0, "changed");
    // The marker went altogether.
    state(|s| s.tempo_markers.clear());
    assert_eq!(lab.call("change_apply", apply).unwrap_err().0, "changed");
    assert_eq!((tempo_markers().len(), state(|s| s.undo_entries)), (0, entries));
}

#[test]
fn deleting_a_tempo_marker_keeps_it_to_make_again_exactly() {
    let lab = Lab::new();
    state(|s| {
        s.tempo_markers.push(fake::FakeTempo { time: 0.0, bpm: 120.0, num: 4, denom: 4, linear: false });
        s.tempo_markers.push(fake::FakeTempo { time: 8.0, bpm: 90.0, num: 3, denom: 4, linear: true });
    });
    let before = tempo_markers();
    let (preview, applied) = lab.make("tempo-marker-delete", json!({"position": 8.0}), "Kumi: Deleted tempo marker at bar 5 (ktm05)");
    assert_eq!(preview["display"]["title"], "Deleted tempo marker at bar 5");
    assert_eq!(tempo_markers(), [before[0]]);
    assert_eq!(applied["material"]["state"], json!({"exists": true, "bpm": 90.0, "num": 3, "den": 4, "linear": true}));
    let restored = lab.undo("tempo-marker-delete", &preview, &applied, "Kumi: undo (kund1)").unwrap();
    assert_eq!(restored["restored"]["bpm"], 90.0);
    assert_eq!(tempo_markers(), before);
    // Made again already: nothing more.
    lab.undo("tempo-marker-delete", &preview, &applied, "Kumi: undo (kund2)").unwrap();
    assert_eq!(tempo_markers(), before);
    // A different marker took its place: refused, unless discarded.
    lab.make("tempo-marker-delete", json!({"position": 8.0}), "Kumi: Deleted tempo marker at bar 5 (ktm06)");
    state(|s| s.tempo_markers.push(fake::FakeTempo { time: 8.0, bpm: 70.0, num: -1, denom: -1, linear: false }));
    let (code, _) = lab.undo("tempo-marker-delete", &preview, &applied, "Kumi: undo (kund3)").unwrap_err();
    assert_eq!(code, "changed");
    assert_eq!(lab.call("change_preview", json!({"kind": "tempo-marker-delete", "input": {"position": 40.0}})).unwrap_err().0, "failed");
}

#[test]
fn the_loop_is_set_cleared_and_put_back_and_end_must_follow_start() {
    let lab = Lab::new();
    let (preview, applied) = lab.make("loop-range", json!({"startBeats": 8.0, "endBeats": 16.0}), "Kumi: Loop bar 3 to bar 5 (klp01)");
    assert_eq!(preview["display"]["title"], "Loop bar 3 to bar 5");
    assert_eq!(preview["prior"], json!({"start": 0.0, "end": 0.0}));
    assert_eq!(state(|s| s.loop_range), (4.0, 8.0));
    // Moved by the producer: undo is refused, unless discarded.
    state(|s| s.loop_range = (4.0, 12.0));
    assert_eq!(lab.undo("loop-range", &preview, &applied, "Kumi: undo (kund1)").unwrap_err().0, "changed");
    state(|s| s.loop_range = (4.0, 8.0));
    lab.undo("loop-range", &preview, &applied, "Kumi: undo (kund1)").unwrap();
    assert_eq!(state(|s| s.loop_range), (0.0, 0.0));
    // Cleared, in seconds.
    state(|s| s.loop_range = (2.0, 6.0));
    let (preview, applied) = lab.make("loop-range", json!({"start": 0.0, "end": 0.0}), "Kumi: Loop cleared (klp02)");
    assert_eq!(preview["display"]["title"], "Loop cleared");
    assert_eq!(state(|s| s.loop_range), (0.0, 0.0));
    lab.undo("loop-range", &preview, &applied, "Kumi: undo (kund2)").unwrap();
    assert_eq!(state(|s| s.loop_range), (2.0, 6.0));
    for input in [
        json!({"start": 4.0, "end": 4.0}),
        json!({"start": 6.0, "end": 2.0}),
        json!({"start": 2.0}),
        json!({"startBeats": 4.0, "start": 2.0, "end": 6.0}),
        json!({"start": "a", "end": 6.0}),
    ] {
        assert_eq!(lab.call("change_preview", json!({"kind": "loop-range", "input": input})).unwrap_err().0, "bad-params", "{input}");
    }
}

#[test]
fn a_loop_change_is_refused_when_the_loop_moved_since_the_preview() {
    let lab = Lab::new();
    let input = json!({"start": 2.0, "end": 6.0});
    let preview = lab.call("change_preview", json!({"kind": "loop-range", "input": input})).unwrap();
    state(|s| s.loop_range = (1.0, 3.0));
    let entries = state(|s| s.undo_entries);
    let apply = json!({"kind": "loop-range", "input": input, "fence": preview["fence"], "desc": "Kumi: x (k1111)"});
    assert_eq!(lab.call("change_apply", apply).unwrap_err().0, "changed");
    assert_eq!((state(|s| s.loop_range), state(|s| s.undo_entries)), ((1.0, 3.0), entries));
}

#[test]
fn repeat_is_switched_and_put_back_unless_the_producer_switched_it_since() {
    let lab = Lab::new();
    let (preview, applied) = lab.make("loop-repeat", json!({"on": true}), "Kumi: Repeat on (krp01)");
    assert_eq!((preview["prior"].clone(), state(|s| s.repeat)), (json!({"on": false}), true));
    state(|s| s.repeat = false);
    assert_eq!(
        lab.undo("loop-repeat", &preview, &applied, "Kumi: undo (kund1)").unwrap().get("restored").map(|r| r["on"].clone()),
        Some(json!(false))
    );
    state(|s| s.repeat = true);
    lab.undo("loop-repeat", &preview, &applied, "Kumi: undo (kund2)").unwrap();
    assert!(!state(|s| s.repeat));
    for input in [json!({}), json!({"on": "yes"})] {
        assert_eq!(lab.call("change_preview", json!({"kind": "loop-repeat", "input": input})).unwrap_err().0, "bad-params");
    }
    // The producer switched it since the preview.
    let preview = lab.call("change_preview", json!({"kind": "loop-repeat", "input": {"on": true}})).unwrap();
    state(|s| s.repeat = true);
    let apply = json!({"kind": "loop-repeat", "input": {"on": true}, "fence": preview["fence"], "desc": "Kumi: x (k1111)"});
    assert_eq!(lab.call("change_apply", apply).unwrap_err().0, "changed");
    // And after the apply: a changed restore is refused.
    state(|s| s.repeat = false);
    let (preview, applied) = lab.make("loop-repeat", json!({"on": true}), "Kumi: Repeat on (krp02)");
    state(|s| s.repeat = false);
    // Already at its prior: nothing to write, and no complaint.
    assert!(lab.undo("loop-repeat", &preview, &applied, "Kumi: undo (kund3)").unwrap().get("restored").is_some());
}

#[test]
fn the_timeline_kinds_are_refused_for_another_epoch_and_while_reaper_is_busy() {
    let lab = Lab::new();
    for (kind, input) in [
        ("tempo-set", json!({"bpm": 90.0})),
        ("tempo-marker-set", json!({"position": 4.0, "bpm": 90.0})),
        ("loop-range", json!({"start": 2.0, "end": 6.0})),
        ("loop-repeat", json!({"on": true})),
    ] {
        let (code, _) = lab.call("change_preview", json!({"kind": kind, "input": input, "epoch": "old-1"})).unwrap_err();
        assert_eq!(code, "stale", "{kind}");
        let preview = lab.call("change_preview", json!({"kind": kind, "input": input})).unwrap();
        state(|s| s.loading = true);
        let apply = json!({"kind": kind, "input": input, "fence": preview["fence"], "desc": "Kumi: x (k1111)"});
        assert_eq!(lab.call("change_apply", apply).unwrap_err().0, "busy", "{kind}");
        state(|s| s.loading = false);
    }
    assert_eq!((state(|s| s.bpm), state(|s| s.loop_range), state(|s| s.repeat), tempo_markers().len()), (120.0, (0.0, 0.0), false, 0));
}

// ----- notes -----

const SNARE_TAKE: &str = "r:take:{TAKE-S1}";

/// (pitch, start, end, velocity, channel, selected) of the snare take's notes, in order.
fn snare_notes() -> Vec<(i32, f64, f64, i32, i32, bool)> {
    state(|s| {
        s.tracks[2].items[0].takes[0].notes.iter().map(|n| (n.pitch, n.start_ppq, n.end_ppq, n.velocity, n.channel, n.selected)).collect()
    })
}

#[test]
fn notes_are_written_where_asked_in_beats_and_undo_takes_exactly_those_away() {
    let lab = Lab::new();
    let before = snare_notes();
    let input = json!({"ref": SNARE_TAKE, "notes": [
        {"pitch": 60, "start": 1.0, "length": 0.5, "velocity": 90, "channel": 2},
        {"pitch": 64, "start": 2.0, "length": 1.0},
        {"pitch": 67, "start": 0.5, "length": 0.25, "muted": true},
    ]});
    let (preview, applied) = lab.make("notes-add", input, "Kumi: Wrote 3 notes in snare.mid take (kna01)");
    assert_eq!(preview["display"]["title"], "Wrote 3 notes in take “snare.mid”");
    assert_eq!(preview["target"], json!({"ref": SNARE_TAKE, "kind": "take"}));
    assert_eq!(top().as_deref(), Some("Kumi: Wrote 3 notes in snare.mid take (kna01)"));
    let now = snare_notes();
    assert_eq!(now.len(), 5);
    assert!(now.windows(2).all(|pair| pair[0].1 <= pair[1].1), "in order of start: {now:?}");
    assert!(now.contains(&(60, 960.0, 1440.0, 90, 1, false)) && now.contains(&(64, 1920.0, 2880.0, 100, 0, false)));
    assert_eq!(applied["applied"]["added"].as_array().unwrap().len(), 3);
    assert_eq!(applied["applied"]["removed"], json!([]));
    // The notes that were there are untouched, the selection too.
    assert!(before.iter().all(|n| now.contains(n)));
    // The producer changed one since: undo is refused and nothing is touched, unless discarded.
    state(|s| s.tracks[2].items[0].takes[0].notes.iter_mut().find(|n| n.pitch == 64).unwrap().velocity = 20);
    let (code, message) = lab.undo("notes-add", &preview, &applied, "Kumi: undo (kund1)").unwrap_err();
    assert_eq!(code, "changed");
    assert!(message.contains("changed in REAPER"), "{message}");
    assert_eq!(snare_notes().len(), 5);
    let mut params = restore_params(&preview, &applied, "Kumi: undo (kund1)");
    params["kind"] = "notes-add".into();
    params["discard"] = true.into();
    lab.call("change_restore", params).unwrap();
    // The note the producer changed is theirs now and stays; the two Kumi wrote and nobody touched are gone.
    let mut expected = [before, vec![(64, 1920.0, 2880.0, 20, 0, false)]].concat();
    expected.sort_by(|a, b| a.1.total_cmp(&b.1));
    assert_eq!(snare_notes(), expected);
}

#[test]
fn notes_are_changed_by_index_and_put_back_by_their_values() {
    let lab = Lab::new();
    let before = snare_notes();
    let input = json!({"ref": SNARE_TAKE, "changes": [
        {"index": 0, "pitch": 36, "start": 0.5},
        {"index": 1, "velocity": 127, "length": 2.0, "muted": true},
    ]});
    let (preview, applied) = lab.make("notes-change", input, "Kumi: Changed 2 notes in snare.mid take (knc01)");
    assert_eq!(preview["display"]["before"].as_array().unwrap().len(), 2);
    assert_eq!(preview["display"]["after"][0]["pitch"], 36);
    let now = snare_notes();
    // The first: moved half a beat later, the same length. The second: longer and louder.
    assert_eq!(now, [(36, 480.0, 960.0, 100, 0, false), (40, 960.0, 2880.0, 127, 9, true)]);
    assert!(state(|s| s.tracks[2].items[0].takes[0].notes[1].muted));
    lab.undo("notes-change", &preview, &applied, "Kumi: undo (kund1)").unwrap();
    assert_eq!(snare_notes().len(), 2);
    let after: Vec<_> = snare_notes().into_iter().map(|n| (n.0, n.1, n.2, n.3, n.4)).collect();
    assert_eq!(after, before.iter().map(|n| (n.0, n.1, n.2, n.3, n.4)).collect::<Vec<_>>());
    assert!(!state(|s| s.tracks[2].items[0].takes[0].notes[1].muted));
    // Undone already: nothing more happens.
    lab.undo("notes-change", &preview, &applied, "Kumi: undo (kund2)").unwrap();
    assert_eq!(snare_notes().len(), 2);
    // Changing a note to what it is says so.
    let same = lab
        .call("change_preview", json!({"kind": "notes-change", "input": {"ref": SNARE_TAKE, "changes": [{"index": 0, "pitch": 38}]}}))
        .unwrap();
    assert_eq!(same["display"]["unchanged"], true);
}

#[test]
fn deleted_notes_are_written_again_with_their_selection_and_a_retry_changes_nothing() {
    let lab = Lab::new();
    let before = snare_notes();
    let (preview, applied) =
        lab.make("notes-delete", json!({"ref": SNARE_TAKE, "indexes": [1]}), "Kumi: Deleted 1 note in snare.mid take (knd01)");
    assert_eq!(preview["display"]["title"], "Deleted 1 note in take “snare.mid”");
    assert_eq!(snare_notes().len(), 1);
    lab.undo("notes-delete", &preview, &applied, "Kumi: undo (kund1)").unwrap();
    assert_eq!(snare_notes(), before, "the second note is selected, and is again");
    lab.undo("notes-delete", &preview, &applied, "Kumi: undo (kund2)").unwrap();
    assert_eq!(snare_notes(), before);
    // The producer wrote the same note by hand since: nothing is written twice.
    let (preview, applied) =
        lab.make("notes-delete", json!({"ref": SNARE_TAKE, "indexes": [0, 1]}), "Kumi: Deleted 2 notes in snare.mid take (knd02)");
    assert!(snare_notes().is_empty());
    state(|s| {
        s.tracks[2].items[0].takes[0].notes.push(fake::FakeNote {
            start_ppq: 0.0,
            end_ppq: 480.0,
            channel: 0,
            pitch: 38,
            velocity: 100,
            selected: false,
            muted: false,
        })
    });
    lab.undo("notes-delete", &preview, &applied, "Kumi: undo (kund3)").unwrap();
    assert_eq!(snare_notes().len(), 2);
}

#[test]
fn a_notes_change_is_refused_when_the_take_changed_since_the_preview_and_nothing_is_written() {
    let lab = Lab::new();
    for (kind, input) in [
        ("notes-add", json!({"ref": SNARE_TAKE, "notes": [{"pitch": 60, "start": 1.0, "length": 1.0}]})),
        ("notes-change", json!({"ref": SNARE_TAKE, "changes": [{"index": 0, "pitch": 50}]})),
        ("notes-delete", json!({"ref": SNARE_TAKE, "indexes": [0]})),
    ] {
        let preview = lab.call("change_preview", json!({"kind": kind, "input": input})).unwrap();
        // The producer moved a note, without changing how many there are.
        state(|s| s.tracks[2].items[0].takes[0].notes[1].pitch += 1);
        let entries = state(|s| s.undo_entries);
        let before = snare_notes();
        let apply = json!({"kind": kind, "input": input, "fence": preview["fence"], "desc": "Kumi: x (k1111)"});
        assert_eq!(lab.call("change_apply", apply).unwrap_err().0, "changed", "{kind}");
        assert_eq!((snare_notes(), state(|s| s.undo_entries)), (before, entries));
        state(|s| s.tracks[2].items[0].takes[0].notes[1].pitch -= 1);
    }
}

#[test]
fn a_bad_notes_request_is_refused_before_anything_is_written() {
    let lab = Lab::new();
    let note = |extra: Value| {
        let mut note = json!({"pitch": 60, "start": 1.0, "length": 1.0});
        for (key, value) in extra.as_object().unwrap() {
            note[key] = value.clone();
        }
        note
    };
    let cases = [
        ("notes-add", json!({"ref": SNARE_TAKE, "notes": []})),
        ("notes-add", json!({"ref": SNARE_TAKE})),
        ("notes-add", json!({"ref": SNARE_TAKE, "notes": [note(json!({"pitch": 128}))]})),
        ("notes-add", json!({"ref": SNARE_TAKE, "notes": [note(json!({"velocity": 0}))]})),
        ("notes-add", json!({"ref": SNARE_TAKE, "notes": [note(json!({"channel": 17}))]})),
        ("notes-add", json!({"ref": SNARE_TAKE, "notes": [note(json!({"start": -1.0}))]})),
        ("notes-add", json!({"ref": SNARE_TAKE, "notes": [note(json!({"length": 0.0}))]})),
        ("notes-add", json!({"ref": SNARE_TAKE, "notes": [note(json!({"muted": "yes"}))]})),
        ("notes-add", json!({"ref": SNARE_TAKE, "notes": [{"start": 1.0, "length": 1.0}]})),
        ("notes-add", json!({"ref": "r:track:{CCCC}", "notes": [note(json!({}))]})),
        ("notes-add", json!({"notes": [note(json!({}))]})),
        ("notes-delete", json!({"ref": SNARE_TAKE, "indexes": [5]})),
        ("notes-delete", json!({"ref": SNARE_TAKE, "indexes": [0, 0]})),
        ("notes-delete", json!({"ref": SNARE_TAKE, "indexes": ["a"]})),
        ("notes-change", json!({"ref": SNARE_TAKE, "changes": [{"pitch": 50}]})),
        ("notes-change", json!({"ref": SNARE_TAKE, "changes": [{"index": 9, "pitch": 50}]})),
        ("notes-change", json!({"ref": SNARE_TAKE, "changes": [{"index": 0, "length": -1.0}]})),
    ];
    for (kind, input) in cases {
        let (code, message) = lab.call("change_preview", json!({"kind": kind, "input": input})).unwrap_err();
        assert_eq!(code, "bad-params", "{kind} {input}: {message}");
    }
    // A take that isn't MIDI.
    let audio = state(|s| s.tracks[1].items[0].takes[0].guid.clone());
    let (code, _) = lab
        .call("change_preview", json!({"kind": "notes-add", "input": {"ref": format!("r:take:{audio}"), "notes": [note(json!({}))]}}))
        .unwrap_err();
    assert_eq!(code, "bad-params");
    assert_eq!(snare_notes().len(), 2);
}

#[test]
fn a_thousand_notes_are_one_change_and_one_undo() {
    let lab = Lab::new();
    let notes: Vec<Value> =
        (0..1000).map(|i| json!({"pitch": 36 + i % 24, "start": i as f64 * 0.25, "length": 0.25, "velocity": 1 + i % 127})).collect();
    let (preview, applied) =
        lab.make("notes-add", json!({"ref": SNARE_TAKE, "notes": notes}), "Kumi: Wrote 1000 notes in snare.mid take (knb01)");
    assert_eq!(preview["display"]["count"], 1000);
    assert_eq!(snare_notes().len(), 1002);
    lab.undo("notes-add", &preview, &applied, "Kumi: undo (kund1)").unwrap();
    assert_eq!(snare_notes().len(), 2);
    let too_many: Vec<Value> = (0..10_001).map(|i| json!({"pitch": 60, "start": i as f64, "length": 1.0})).collect();
    assert_eq!(
        lab.call("change_preview", json!({"kind": "notes-add", "input": {"ref": SNARE_TAKE, "notes": too_many}})).unwrap_err().0,
        "bad-params"
    );
}

// ----- FX -----

fn kick_fx() -> Vec<(String, String, bool)> {
    state(|s| s.tracks[1].fx.iter().map(|f| (f.guid.clone(), f.name.clone(), f.enabled)).collect())
}

fn fx_ref_of(guid: &str) -> String {
    format!("r:fx:{guid}")
}

/// A resource folder the fake can keep FX chain files in.
fn fx_folder() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    state(|s| s.resource = dir.path().to_string_lossy().into_owned());
    dir
}

fn chain_files(dir: &tempfile::TempDir) -> usize {
    std::fs::read_dir(dir.path().join("FXChains")).map_or(0, |d| d.count())
}

#[test]
fn a_plug_in_is_added_where_asked_and_undo_deletes_that_one_by_its_id() {
    let lab = Lab::new();
    let (preview, applied) = lab.make("fx-add", json!({"ref": KICK, "name": "ReaEQ"}), "Kumi: Added ReaEQ to Kick (kfa01)");
    assert_eq!(preview["display"]["title"], "Added “ReaEQ” to “Kick”");
    assert_eq!(top().as_deref(), Some("Kumi: Added ReaEQ to Kick (kfa01)"));
    assert_eq!(kick_fx().len(), 1);
    let eq = kick_fx()[0].0.clone();
    assert_eq!(applied["created"], json!({"ref": fx_ref_of(&eq), "kind": "fx"}));
    // At a place, in front.
    let (preview2, applied2) =
        lab.make("fx-add", json!({"ref": KICK, "name": "ReaComp", "index": 0}), "Kumi: Added ReaComp to Kick (kfa02)");
    assert_eq!(kick_fx().iter().map(|f| f.1.as_str()).collect::<Vec<_>>(), ["VST: ReaComp (Cockos)", "VST: ReaEQ (Cockos)"]);
    // The producer turned the compressor's parameter: undo is refused, unless discarded.
    state(|s| s.tracks[1].fx[0].params[0].value = 0.9);
    let (code, message) = lab.undo("fx-add", &preview2, &applied2, "Kumi: undo (kund1)").unwrap_err();
    assert_eq!(code, "changed");
    assert!(message.contains("changed in REAPER"), "{message}");
    assert_eq!(kick_fx().len(), 2);
    let mut params = restore_params(&preview2, &applied2, "Kumi: undo (kund1)");
    params["kind"] = "fx-add".into();
    params["discard"] = true.into();
    lab.call("change_restore", params).unwrap();
    assert_eq!(kick_fx().iter().map(|f| f.0.as_str()).collect::<Vec<_>>(), [eq.as_str()]);
    // Undone already: it is gone.
    assert_eq!(lab.undo("fx-add", &preview2, &applied2, "Kumi: undo (kund2)").unwrap_err().0, "gone");
    lab.undo("fx-add", &preview, &applied, "Kumi: undo (kund3)").unwrap();
    assert!(kick_fx().is_empty());
}

#[test]
fn a_plug_in_that_isnt_there_or_a_bad_request_writes_nothing() {
    let lab = Lab::new();
    let entries = state(|s| s.undo_entries);
    for (input, code) in [
        (json!({"ref": KICK, "name": "NoSuchPlugin"}), "failed"),
        (json!({"ref": KICK, "name": ""}), "bad-params"),
        (json!({"ref": KICK}), "bad-params"),
        (json!({"ref": KICK, "name": "a\nb"}), "bad-params"),
        (json!({"ref": KICK, "name": "mine.RfxChain"}), "bad-params"),
        (json!({"ref": KICK, "name": "ReaEQ", "index": 3}), "bad-params"),
        (json!({"ref": "r:fx:{FX-EQ}", "name": "ReaEQ"}), "bad-params"),
    ] {
        // Unknown plug-ins are found out when it is added, not at the preview.
        let preview = lab.call("change_preview", json!({"kind": "fx-add", "input": input}));
        if code == "failed" {
            let preview = preview.unwrap();
            let apply = json!({"kind": "fx-add", "input": input, "fence": preview["fence"], "desc": "Kumi: x (k1111)"});
            let (got, message) = lab.call("change_apply", apply).unwrap_err();
            assert_eq!(got, code, "{input}");
            assert!(message.contains("NoSuchPlugin"), "{message}");
        } else {
            assert_eq!(preview.unwrap_err().0, code, "{input}");
        }
    }
    assert_eq!((kick_fx().len(), state(|s| s.undo_entries)), (0, entries));
    // The chain moved between the preview and the apply.
    let input = json!({"ref": KICK, "name": "ReaEQ"});
    let preview = lab.call("change_preview", json!({"kind": "fx-add", "input": input})).unwrap();
    state(|s| s.tracks[1].fx.push(fake::FakeFx::new("{FX-X}", "VST: ReaEQ (Cockos)")));
    let apply = json!({"kind": "fx-add", "input": input, "fence": preview["fence"], "desc": "Kumi: x (k1111)"});
    assert_eq!(lab.call("change_apply", apply).unwrap_err().0, "changed");
    assert_eq!(kick_fx().len(), 1);
}

fn three_fx(lab: &Lab) -> Vec<String> {
    for (n, name) in ["ReaEQ", "ReaComp", "ReaDelay"].iter().enumerate() {
        lab.make("fx-add", json!({"ref": KICK, "name": name}), &format!("Kumi: Added {name} to Kick (kfx0{n})"));
    }
    kick_fx().into_iter().map(|f| f.0).collect()
}

#[test]
fn an_fx_is_moved_to_a_place_in_the_chain_and_undo_puts_it_back_unless_the_chain_was_rearranged() {
    let lab = Lab::new();
    let guids = three_fx(&lab);
    let (preview, applied) =
        lab.make("fx-move", json!({"ref": fx_ref_of(&guids[2]), "index": 0}), "Kumi: Moved ReaDelay to place 1 on Kick (kfm01)");
    assert_eq!(preview["display"]["title"], "Moved “ReaDelay” to place 1 on “Kick”");
    assert_eq!(kick_fx().iter().map(|f| f.0.clone()).collect::<Vec<_>>(), [guids[2].clone(), guids[0].clone(), guids[1].clone()]);
    // The producer moved another since.
    state(|s| s.tracks[1].fx.swap(1, 2));
    assert_eq!(lab.undo("fx-move", &preview, &applied, "Kumi: undo (kund1)").unwrap_err().0, "changed");
    state(|s| s.tracks[1].fx.swap(1, 2));
    lab.undo("fx-move", &preview, &applied, "Kumi: undo (kund1)").unwrap();
    assert_eq!(kick_fx().iter().map(|f| f.0.clone()).collect::<Vec<_>>(), guids);
    // Undone already.
    lab.undo("fx-move", &preview, &applied, "Kumi: undo (kund2)").unwrap();
    assert_eq!(kick_fx().iter().map(|f| f.0.clone()).collect::<Vec<_>>(), guids);
    // To where it is: nothing to do. Past the end: refused.
    let same = lab.call("change_preview", json!({"kind": "fx-move", "input": {"ref": fx_ref_of(&guids[1]), "index": 1}})).unwrap();
    assert_eq!(same["display"]["unchanged"], true);
    for index in [json!(3), json!(-1), json!("a"), Value::Null] {
        let input = json!({"ref": fx_ref_of(&guids[1]), "index": index});
        assert_eq!(lab.call("change_preview", json!({"kind": "fx-move", "input": input})).unwrap_err().0, "bad-params", "{index}");
    }
    // The producer rearranged between the preview and the apply.
    let input = json!({"ref": fx_ref_of(&guids[0]), "index": 2});
    let preview = lab.call("change_preview", json!({"kind": "fx-move", "input": input})).unwrap();
    state(|s| s.tracks[1].fx.swap(0, 1));
    let apply = json!({"kind": "fx-move", "input": input, "fence": preview["fence"], "desc": "Kumi: x (k1111)"});
    assert_eq!(lab.call("change_apply", apply).unwrap_err().0, "changed");
}

#[test]
fn an_fx_is_bypassed_and_enabled_and_undo_refuses_when_the_producer_switched_it() {
    let lab = Lab::new();
    let guids = three_fx(&lab);
    let reference = fx_ref_of(&guids[1]);
    let (preview, applied) = lab.make("fx-bypass", json!({"ref": reference, "bypass": true}), "Kumi: Bypassed ReaComp on Kick (kfb01)");
    assert_eq!(preview["display"]["title"], "Bypassed “ReaComp” on “Kick”");
    assert_eq!(kick_fx().iter().map(|f| f.2).collect::<Vec<_>>(), [true, false, true]);
    // The producer switched it back since: it is where undo would put it, and nothing is written.
    state(|s| s.tracks[1].fx[1].enabled = true);
    lab.undo("fx-bypass", &preview, &applied, "Kumi: undo (kund1)").unwrap();
    assert!(kick_fx()[1].2);
    state(|s| s.tracks[1].fx[1].enabled = false);
    lab.undo("fx-bypass", &preview, &applied, "Kumi: undo (kund2)").unwrap();
    assert!(kick_fx()[1].2);
    let (preview, _) = lab.make("fx-bypass", json!({"ref": reference, "bypass": false}), "Kumi: Enabled ReaComp on Kick (kfb02)");
    assert_eq!(preview["display"]["unchanged"], true);
    assert_eq!(lab.call("change_preview", json!({"kind": "fx-bypass", "input": {"ref": reference}})).unwrap_err().0, "bad-params");
    // Changed between the preview and the apply.
    let input = json!({"ref": reference, "bypass": true});
    let preview = lab.call("change_preview", json!({"kind": "fx-bypass", "input": input})).unwrap();
    state(|s| s.tracks[1].fx[1].enabled = false);
    let apply = json!({"kind": "fx-bypass", "input": input, "fence": preview["fence"], "desc": "Kumi: x (k1111)"});
    assert_eq!(lab.call("change_apply", apply).unwrap_err().0, "changed");
}

#[test]
fn a_parameter_is_set_by_its_normalized_value_or_by_what_the_plug_in_shows() {
    let lab = Lab::new();
    let guids = three_fx(&lab);
    let fx = fx_ref_of(&guids[0]);
    let param = |n: usize| -> f64 { state(|s| s.tracks[1].fx[0].params[n].value) };
    // Gain is -12 to 12 dB, centre at 0.5.
    let (preview, applied) =
        lab.make("fx-param", json!({"ref": fx, "param": "Gain", "normalized": 0.75}), "Kumi: ReaEQ Gain 6.0 dB on Kick (kfp01)");
    assert_eq!(preview["display"]["title"], "ReaEQ Gain 6.0 dB on “Kick”");
    assert_eq!((preview["prior"]["display"].as_str(), applied["applied"]["display"].as_str()), (Some("0.0 dB"), Some("6.0 dB")));
    assert_eq!(param(0), 0.75);
    lab.undo("fx-param", &preview, &applied, "Kumi: undo (kund1)").unwrap();
    assert_eq!(param(0), 0.5);
    // By what it shows.
    let by_ref = format!("r:param:{}:0", guids[0]);
    let (preview, applied) = lab.make("fx-param", json!({"ref": by_ref, "value": "-3 dB"}), "Kumi: ReaEQ Gain -3.0 dB on Kick (kfp02)");
    // The first value the plug-in prints as -3.0 dB.
    assert!((param(0) - 0.375).abs() < 0.005, "{}", param(0));
    assert_eq!(applied["applied"]["display"], "-3.0 dB");
    assert_eq!(preview["display"]["after"], "-3.0 dB");
    // Words.
    lab.make("fx-param", json!({"ref": fx, "param": 1, "value": "high"}), "Kumi: ReaEQ Mode High on Kick (kfp03)");
    assert!(param(1) > 0.75 && param(1) < 1.0, "the middle of the values that print High: {}", param(1));
    // The producer turned it since: undo is refused, unless discarded.
    state(|s| s.tracks[1].fx[0].params[0].value = 0.9);
    assert_eq!(lab.undo("fx-param", &preview, &applied, "Kumi: undo (kund2)").unwrap_err().0, "changed");
    let mut params = restore_params(&preview, &applied, "Kumi: undo (kund2)");
    params["kind"] = "fx-param".into();
    params["discard"] = true.into();
    lab.call("change_restore", params).unwrap();
    assert_eq!(param(0), 0.5);
}

#[test]
fn a_parameter_request_that_cant_be_met_writes_nothing() {
    let lab = Lab::new();
    let guids = three_fx(&lab);
    let fx = fx_ref_of(&guids[0]);
    let entries = state(|s| s.undo_entries);
    for (input, code) in [
        (json!({"ref": fx, "param": "Gain"}), "bad-params"),
        (json!({"ref": fx, "param": "Gain", "normalized": 1.5}), "bad-params"),
        (json!({"ref": fx, "param": "Gain", "normalized": 0.5, "value": "0 dB"}), "bad-params"),
        (json!({"ref": fx, "normalized": 0.5}), "bad-params"),
        (json!({"ref": fx, "param": "Nope", "normalized": 0.5}), "bad-params"),
        (json!({"ref": fx, "param": 9, "normalized": 0.5}), "bad-params"),
        (json!({"ref": format!("r:param:{}:9", guids[0]), "normalized": 0.5}), "bad-params"),
        (json!({"ref": fx, "param": "Gain", "value": "40 dB"}), "failed"),
        (json!({"ref": fx, "param": "Gain", "value": "-3.25 dB"}), "failed"),
        (json!({"ref": fx, "param": "Mode", "value": "Loud"}), "failed"),
        (json!({"ref": fx, "param": "Gain", "value": 3}), "bad-params"),
        (json!({"ref": "r:fx:{FX-COMP}", "param": 0, "normalized": 0.1}), "bad-params"),
        (json!({"ref": "r:fx:{FX-C2}", "param": 0, "normalized": 0.1}), "bad-params"),
    ] {
        let (got, message) = lab.call("change_preview", json!({"kind": "fx-param", "input": input})).unwrap_err();
        assert_eq!(got, code, "{input}: {message}");
    }
    assert_eq!(state(|s| s.undo_entries), entries);
    // Moved between the preview and the apply.
    let input = json!({"ref": fx, "param": "Gain", "normalized": 0.7});
    let preview = lab.call("change_preview", json!({"kind": "fx-param", "input": input})).unwrap();
    state(|s| s.tracks[1].fx[0].params[0].value = 0.1);
    let apply = json!({"kind": "fx-param", "input": input, "fence": preview["fence"], "desc": "Kumi: x (k1111)"});
    assert_eq!(lab.call("change_apply", apply).unwrap_err().0, "changed");
}

#[test]
fn deleting_an_fx_keeps_its_entry_and_undo_adds_it_back_with_its_id_and_state_and_no_file_is_left() {
    let lab = Lab::new();
    let dir = fx_folder();
    let guids = three_fx(&lab);
    state(|s| {
        s.tracks[1].fx[1].params[0].value = 0.8;
        s.tracks[1].fx[1].enabled = false;
    });
    let (preview, applied) = lab.make("fx-delete", json!({"ref": fx_ref_of(&guids[1])}), "Kumi: Deleted ReaComp from Kick (kfd01)");
    assert_eq!(preview["display"]["title"], "Deleted “ReaComp” from “Kick”");
    assert_eq!(kick_fx().iter().map(|f| f.0.clone()).collect::<Vec<_>>(), [guids[0].clone(), guids[2].clone()]);
    let entry = applied["material"]["entry"].as_str().unwrap();
    assert!(entry.starts_with("BYPASS 1 0 0\n<VST \"VST: ReaComp (Cockos)\"") && entry.contains(&format!("FXID {}", guids[1])), "{entry}");
    let restored = lab.undo("fx-delete", &preview, &applied, "Kumi: undo (kund1)").unwrap();
    assert_eq!(restored["restored"]["ref"], fx_ref_of(&guids[1]));
    assert_eq!(kick_fx().iter().map(|f| f.0.clone()).collect::<Vec<_>>(), guids, "in its place, with its id");
    let (enabled, gain) = state(|s| (s.tracks[1].fx[1].enabled, s.tracks[1].fx[1].params[0].value));
    assert_eq!((enabled, gain), (false, 0.8), "its state came back");
    assert_eq!(chain_files(&dir), 0, "the chain file is removed");
    // Back already: nothing is added twice.
    lab.undo("fx-delete", &preview, &applied, "Kumi: undo (kund2)").unwrap();
    assert_eq!(kick_fx().len(), 3);
    // The chain got shorter since: it goes where it can.
    let (preview, applied) = lab.make("fx-delete", json!({"ref": fx_ref_of(&guids[2])}), "Kumi: Deleted ReaDelay from Kick (kfd02)");
    state(|s| s.tracks[1].fx.clear());
    lab.undo("fx-delete", &preview, &applied, "Kumi: undo (kund3)").unwrap();
    assert_eq!(kick_fx().iter().map(|f| f.0.clone()).collect::<Vec<_>>(), [guids[2].clone()]);
    assert_eq!(chain_files(&dir), 0);
}

#[test]
fn deleting_an_fx_is_refused_when_it_changed_or_went_and_containers_and_other_chains_are_left_alone() {
    let lab = Lab::new();
    let guids = three_fx(&lab);
    let input = json!({"ref": fx_ref_of(&guids[1])});
    let preview = lab.call("change_preview", json!({"kind": "fx-delete", "input": input})).unwrap();
    state(|s| s.tracks[1].fx[1].params[0].value = 0.9);
    let entries = state(|s| s.undo_entries);
    let apply = json!({"kind": "fx-delete", "input": input, "fence": preview["fence"], "desc": "Kumi: x (k1111)"});
    assert_eq!(lab.call("change_apply", apply.clone()).unwrap_err().0, "changed");
    state(|s| s.tracks[1].fx.remove(1));
    assert_eq!(lab.call("change_apply", apply).unwrap_err().0, "changed");
    assert_eq!(state(|s| s.undo_entries), entries);
    assert_eq!(lab.call("change_preview", json!({"kind": "fx-delete", "input": input})).unwrap_err().0, "failed");
    for reference in ["r:fx:{FX-C}", "r:fx:{FX-DLY}", "r:track:{BBBB}", "r:fx:{NOPE}"] {
        let (code, _) = lab.call("change_preview", json!({"kind": "fx-delete", "input": {"ref": reference}})).unwrap_err();
        assert!(code == "bad-params" || code == "failed", "{reference}: {code}");
    }
    // An FX the chunk doesn't hold can't be kept, so it isn't deleted.
    state(|s| {
        s.tracks[1].fx.push(fake::FakeFx::container("{FX-K}", vec![]));
        s.state_count += 1;
    });
    assert_eq!(lab.call("change_preview", json!({"kind": "fx-delete", "input": {"ref": "r:fx:{FX-K}"}})).unwrap_err().0, "bad-params");
}

#[test]
fn the_fx_kinds_are_refused_for_another_epoch_and_while_reaper_is_busy() {
    let lab = Lab::new();
    let guids = three_fx(&lab);
    let fx = fx_ref_of(&guids[0]);
    for (kind, input) in [
        ("fx-add", json!({"ref": KICK, "name": "ReaEQ"})),
        ("fx-delete", json!({"ref": fx})),
        ("fx-move", json!({"ref": fx, "index": 1})),
        ("fx-bypass", json!({"ref": fx, "bypass": true})),
        ("fx-param", json!({"ref": fx, "param": 0, "normalized": 0.2})),
        ("notes-add", json!({"ref": SNARE_TAKE, "notes": [{"pitch": 60, "start": 1.0, "length": 1.0}]})),
        ("notes-delete", json!({"ref": SNARE_TAKE, "indexes": [0]})),
    ] {
        let (code, _) = lab.call("change_preview", json!({"kind": kind, "input": input, "epoch": "old-1"})).unwrap_err();
        assert_eq!(code, "stale", "{kind}");
        let preview = lab.call("change_preview", json!({"kind": kind, "input": input})).unwrap();
        state(|s| s.loading = true);
        let apply = json!({"kind": kind, "input": input, "fence": preview["fence"], "desc": "Kumi: x (k1111)"});
        assert_eq!(lab.call("change_apply", apply).unwrap_err().0, "busy", "{kind}");
        state(|s| s.loading = false);
    }
    assert_eq!((kick_fx().len(), snare_notes().len()), (3, 2));
}

#[test]
fn transport_play_starts_what_isnt_playing_and_the_cursor_moves_without_an_undo_point() {
    let lab = Lab::new();
    assert_eq!(
        lab.call("transport_play", json!({})).unwrap(),
        json!({"started": false, "playing": false}),
        "the fake's action doesn't start it"
    );
    assert_eq!(state(|s| s.actions.clone()), [1007]);
    state(|s| {
        s.actions.clear();
        s.play_bits = 1;
    });
    assert_eq!(lab.call("transport_play", json!({})).unwrap(), json!({"started": false, "playing": true}));
    assert!(state(|s| s.actions.is_empty()), "already playing: nothing to start");
    state(|s| s.loading = true);
    assert_eq!(lab.call("transport_play", json!({})).unwrap_err().0, "busy");
    state(|s| s.loading = false);

    let entries = state(|s| s.undo_entries);
    let moved = lab.call("cursor_set", json!({"positionBeats": 8.0})).unwrap();
    assert_eq!((moved["position"].clone(), moved["positionBeats"].clone()), (json!(4.0), json!(8.0)));
    assert_eq!((state(|s| s.cursor), state(|s| s.undo_entries)), (4.0, entries));
    assert_eq!(lab.call("cursor_set", json!({"position": 1.5})).unwrap()["position"], 1.5);
    for params in [json!({}), json!({"position": 1.0, "positionBeats": 2.0}), json!({"position": -1.0}), json!({"position": "a"})] {
        assert_eq!(lab.call("cursor_set", params).unwrap_err().0, "bad-params");
    }
    assert_eq!(lab.call("cursor_set", json!({"position": 1.0, "epoch": "old-1"})).unwrap_err().0, "stale");
    state(|s| s.loading = true);
    assert_eq!(lab.call("cursor_set", json!({"position": 1.0})).unwrap_err().0, "busy");
}

#[test]
fn the_marker_reaper_makes_at_the_start_with_the_first_one_goes_with_it_unless_the_producer_changed_it() {
    let lab = Lab::new();
    assert!(tempo_markers().is_empty());
    let (preview, applied) =
        lab.make("tempo-marker-set", json!({"positionBeats": 16.0, "bpm": 140.0}), "Kumi: Tempo 140 BPM at bar 5 (ktb01)");
    assert_eq!(tempo_markers(), [(0.0, 120.0, 4, 4, false), (8.0, 140.0, -1, -1, false)]);
    assert_eq!(applied["applied"]["base"], json!({"exists": true, "bpm": 120.0, "num": 4, "den": 4, "linear": false}));
    lab.undo("tempo-marker-set", &preview, &applied, "Kumi: undo (kund1)").unwrap();
    assert!(tempo_markers().is_empty(), "back to a project with the one tempo");
    // The producer changed the one at the start: it stays.
    let (preview, applied) =
        lab.make("tempo-marker-set", json!({"positionBeats": 16.0, "bpm": 140.0}), "Kumi: Tempo 140 BPM at bar 5 (ktb02)");
    state(|s| s.tempo_markers[0].bpm = 100.0);
    lab.undo("tempo-marker-set", &preview, &applied, "Kumi: undo (kund2)").unwrap();
    assert_eq!(tempo_markers(), [(0.0, 100.0, 4, 4, false)]);
    // A marker at the start itself brings nothing.
    state(|s| s.tempo_markers.clear());
    let (_, applied) = lab.make("tempo-marker-set", json!({"position": 0.0, "bpm": 90.0}), "Kumi: Tempo 90 BPM at bar 1 (ktb03)");
    assert!(applied["applied"].get("base").is_none());
}

#[test]
fn the_controller_events_in_a_take_are_untouched_by_a_notes_change() {
    let lab = Lab::new();
    state(|s| {
        s.tracks[2].items[0].takes[0].ccs =
            (0..5).map(|i| fake::FakeCc { ppq: i as f64 * 480.0, kind: 0xB0, channel: 0, data1: 1, data2: 10 * i }).collect();
    });
    let ccs = || state(|s| s.tracks[2].items[0].takes[0].ccs.iter().map(|c| (c.ppq as i64, c.data1, c.data2)).collect::<Vec<_>>());
    let before = ccs();
    let (preview, applied) = lab.make(
        "notes-add",
        json!({"ref": SNARE_TAKE, "notes": [{"pitch": 50, "start": 0.0, "length": 1.0}, {"pitch": 52, "start": 2.0, "length": 1.0}]}),
        "Kumi: Wrote 2 notes in snare.mid take (kcc01)",
    );
    assert_eq!((ccs(), snare_notes().len()), (before.clone(), 4));
    lab.undo("notes-add", &preview, &applied, "Kumi: undo (kund1)").unwrap();
    assert_eq!((ccs(), snare_notes().len()), (before, 2));
}

#[test]
fn a_note_that_would_overlap_one_of_the_same_pitch_is_refused_so_no_note_is_shortened() {
    let lab = Lab::new();
    // The snare take has pitch 38 on channel 1 from tick 0 to 480 (beat 0 to 0.5).
    for (input, ok) in [
        (json!({"pitch": 38, "start": 0.25, "length": 1.0}), false),
        (json!({"pitch": 38, "start": 0.5, "length": 1.0}), true),
        (json!({"pitch": 38, "start": 0.25, "length": 1.0, "channel": 2}), true),
        (json!({"pitch": 39, "start": 0.25, "length": 1.0}), true),
    ] {
        let result = lab.call("change_preview", json!({"kind": "notes-add", "input": {"ref": SNARE_TAKE, "notes": [input]}}));
        assert_eq!(result.is_ok(), ok, "{input}: {result:?}");
        if let Err((code, message)) = result {
            assert_eq!(code, "bad-params");
            assert!(message.contains("overlap"), "{message}");
        }
    }
    // Two new notes that overlap each other.
    let both = json!([{"pitch": 50, "start": 4.0, "length": 1.0}, {"pitch": 50, "start": 4.5, "length": 1.0}]);
    assert_eq!(
        lab.call("change_preview", json!({"kind": "notes-add", "input": {"ref": SNARE_TAKE, "notes": both}})).unwrap_err().0,
        "bad-params"
    );
    // A change that moves a note onto another.
    let onto = json!([{"index": 1, "pitch": 38, "channel": 1, "start": 0.25}]);
    assert_eq!(
        lab.call("change_preview", json!({"kind": "notes-change", "input": {"ref": SNARE_TAKE, "changes": onto}})).unwrap_err().0,
        "bad-params"
    );
    // Taking a note away makes room for one in its place.
    let swap = json!([{"index": 0, "start": 0.0, "length": 1.0}]);
    lab.make("notes-change", json!({"ref": SNARE_TAKE, "changes": swap}), "Kumi: Changed 1 note in snare.mid take (kov01)");
    // A take that already overlaps (the producer's): adding elsewhere is no matter.
    state(|s| {
        s.tracks[2].items[0].takes[0].notes.push(fake::FakeNote {
            start_ppq: 100.0,
            end_ppq: 500.0,
            channel: 0,
            pitch: 38,
            velocity: 90,
            selected: false,
            muted: false,
        })
    });
    lab.make(
        "notes-add",
        json!({"ref": SNARE_TAKE, "notes": [{"pitch": 60, "start": 8.0, "length": 1.0}]}),
        "Kumi: Wrote 1 note in snare.mid take (kov02)",
    );
}

#[test]
fn a_plan_is_named_by_what_it_did_when_it_ends() {
    let lab = Lab::new();
    let id = lab.call("undo_step_begin", json!({"label": "Kumi: Kumi (kplan)"})).unwrap()["stepId"].as_str().unwrap().to_string();
    lab.make("track-mute", json!({"ref": KICK, "mute": true}), "Kumi: Kick muted (kp001)");
    lab.make("track-pan", json!({"ref": KICK, "pan": 0.2}), "Kumi: Kick pan 20% right (kp002)");
    let ended = lab.call("undo_step_end", json!({"stepId": id, "label": "Kumi: Kick muted and 1 more (kplan)"})).unwrap();
    assert_eq!((ended["desc"].clone(), ended["entry"].clone()), (json!("Kumi: Kick muted and 1 more (kplan)"), json!(true)));
    assert_eq!(top().as_deref(), Some("Kumi: Kick muted and 1 more (kplan)"), "REAPER's undo menu says what the plan did");
    // A label that isn't text is refused, and the step stays open to be ended.
    let id = lab.call("undo_step_begin", json!({"label": "Kumi: Kumi (kplan)"})).unwrap()["stepId"].as_str().unwrap().to_string();
    assert_eq!(lab.call("undo_step_end", json!({"stepId": id, "label": 7})).unwrap_err().0, "bad-params");
    assert!(lab.call("undo_step_end", json!({"stepId": id})).is_ok());
}

#[test]
fn a_parameters_title_keeps_the_unit_the_plug_in_prints_even_when_its_formatted_value_leaves_it_off() {
    let lab = Lab::new();
    let guids = three_fx(&lab);
    let fx = fx_ref_of(&guids[0]);
    // The formatted value of Gain is a bare number, as ReaEQ's is; asking the plug-in to print the value keeps the unit.
    assert!(state(|s| s.tracks[1].fx[0].params[0].bare));
    let (preview, applied) =
        lab.make("fx-param", json!({"ref": fx, "param": "Gain", "normalized": 0.75}), "Kumi: ReaEQ Gain 6.0 dB on Kick (ktu01)");
    assert_eq!(preview["display"]["title"], "ReaEQ Gain 6.0 dB on “Kick”");
    assert_eq!(applied["display"]["title"], "ReaEQ Gain 6.0 dB on “Kick”");
    assert_eq!((preview["prior"]["display"].as_str(), applied["applied"]["display"].as_str()), (Some("0.0 dB"), Some("6.0 dB")));
    let restored = lab.undo("fx-param", &preview, &applied, "Kumi: undo (kund1)").unwrap();
    assert_eq!(restored["display"]["title"], "ReaEQ Gain 0.0 dB on “Kick” (put back)");
    // A parameter that prints no unit has its number.
    let (preview, applied) =
        lab.make("fx-param", json!({"ref": fx, "param": "Offset", "normalized": 1.0}), "Kumi: ReaEQ Offset 1.00 on Kick (ktu02)");
    assert_eq!(preview["display"]["title"], "ReaEQ Offset 1.00 on “Kick”");
    assert_eq!(applied["display"]["title"], "ReaEQ Offset 1.00 on “Kick”");
}

#[test]
fn a_track_called_track_005_reads_without_stuttering_in_every_title_and_its_restore() {
    let lab = Lab::new();
    state(|s| {
        s.tracks.push(fake::FakeTrack::new("{EEEE}", "Track 005"));
        s.state_count += 1;
    });
    let five = "r:track:{EEEE}";
    let title = |preview: &Value| preview["display"]["title"].as_str().unwrap().to_string();
    let no_stutter = |text: &str| assert!(!text.contains("Track Track") && !text.contains("track Track"), "{text}");

    // A rename: the names in quotes, the arrow between them; undone, it goes the other way.
    let (preview, applied) =
        lab.make("track-rename", json!({"ref": five, "name": "Lead Vox"}), "Kumi: Renamed track “Track 005” → “Lead Vox” (kst01)");
    assert_eq!(title(&preview), "Renamed track “Track 005” → “Lead Vox”");
    assert_eq!(applied["display"]["title"], "Renamed track “Track 005” → “Lead Vox”");
    let restored = lab.undo("track-rename", &preview, &applied, "Kumi: undo (kund1)").unwrap();
    assert_eq!(restored["display"]["title"], "Renamed track “Lead Vox” → “Track 005” (put back)");

    // A title that begins with the track's name leaves it bare, and its restore does not stutter.
    let (preview, applied) = lab.make("track-volume", json!({"ref": five, "volumeDb": -3.0}), "Kumi: Track 005 volume −3 dB (kst02)");
    assert_eq!(title(&preview), "Track 005 volume −3 dB");
    let restored = lab.undo("track-volume", &preview, &applied, "Kumi: undo (kund2)").unwrap();
    assert_eq!(restored["display"]["title"], "Track 005 volume 0 dB (put back)");

    // A delete and its restore.
    let (preview, applied) = lab.make("track-delete", json!({"ref": five}), "Kumi: Deleted track “Track 005” (kst03)");
    assert_eq!(title(&preview), "Deleted track “Track 005”");
    assert_eq!(applied["display"]["title"], "Deleted track “Track 005”");
    let restored = lab.undo("track-delete", &preview, &applied, "Kumi: undo (kund3)").unwrap();
    assert_eq!(restored["display"]["title"], "“Track 005” put back");

    // A move, a send, an FX and a track added by that name.
    let (preview, applied) =
        lab.make("track-move", json!({"ref": five, "index": 1}), "Kumi: Moved track “Track 005” to position 1 (kst04)");
    assert_eq!(title(&preview), "Moved track “Track 005” to position 1");
    let restored = lab.undo("track-move", &preview, &applied, "Kumi: undo (kund4)").unwrap();
    assert_eq!(restored["display"]["title"], "“Track 005” back at position 5");
    let (preview, _) = lab.make(
        "send-add",
        json!({"ref": five, "toRef": "r:track:{DDDD}", "volumeDb": -6.0}),
        "Kumi: Added send “Track 005” → “Reverb” (kst05)",
    );
    assert_eq!(title(&preview), "Added send “Track 005” → “Reverb”");
    let (preview, applied) = lab.make("fx-add", json!({"ref": five, "name": "ReaEQ"}), "Kumi: Added “ReaEQ” to “Track 005” (kst06)");
    assert_eq!(title(&preview), "Added “ReaEQ” to “Track 005”");
    let restored = lab.undo("fx-add", &preview, &applied, "Kumi: undo (kund6)").unwrap();
    assert_eq!(restored["display"]["title"], "“ReaEQ” taken off “Track 005”");
    let (preview, applied) = lab.make("track-add", json!({"name": "Track 006"}), "Kumi: Added track “Track 006” (kst07)");
    assert_eq!(title(&preview), "Added track “Track 006”");
    assert_eq!(applied["display"]["title"], "Added track “Track 006”");

    // A track with no name is the number REAPER shows.
    state(|s| {
        s.tracks[4].name = String::new();
        s.state_count += 1;
    });
    let (preview, applied) = lab.make("track-delete", json!({"ref": five}), "Kumi: Deleted track “Track 5” (kst08)");
    assert_eq!(title(&preview), "Deleted track “Track 5”");
    let restored = lab.undo("track-delete", &preview, &applied, "Kumi: undo (kund8)").unwrap();
    assert_eq!(restored["display"]["title"], "“Track 5” put back");
    for text in [title(&preview), restored["display"]["title"].as_str().unwrap().to_string()] {
        no_stutter(&text);
    }
}

// ----- sends by identity (M1) -----

/// The refs of a track's sends, as a read gives them.
fn send_refs(track: i32) -> Vec<String> {
    let api = fake::api();
    let reaper = Reaper::new(&api);
    let owner = reaper.track_at(reaper.current_project().unwrap().ptr, track).unwrap();
    crate::rows::send_keys(&reaper, owner, 0)
        .iter()
        .enumerate()
        .map(|(index, key)| crate::rows::send_row(&reaper, owner, 0, index as i32, key)["ref"].as_str().unwrap().to_string())
        .collect()
}

/// Kick sends twice to Reverb: s1 at -12 dB and s2 at -6 dB (the producer's own sends, with no mark). Their refs, read.
fn two_sends() -> (String, String) {
    state(|s| {
        let mut first = FakeSend::to(3);
        first.volume = 0.25;
        let mut second = FakeSend::to(3);
        second.volume = 0.5;
        s.tracks[1].sends = vec![first, second];
        s.state_count += 1;
    });
    let refs = send_refs(1);
    (refs[0].clone(), refs[1].clone())
}

fn send_volumes() -> Vec<(f64, bool)> {
    state(|s| s.tracks[1].sends.iter().map(|send| ((send.volume * 100.0).round() / 100.0, send.mute)).collect())
}

/// The producer removes the first send and makes a new one (a fresh send is at 0 dB).
fn producer_swaps_the_first_send() {
    state(|s| {
        s.tracks[1].sends.remove(0);
        s.tracks[1].sends.push(FakeSend::to(3));
        s.state_count += 1;
    });
}

#[test]
fn a_send_ref_that_names_another_send_after_the_producer_removed_one_and_added_one_is_refused_at_the_apply() {
    let lab = Lab::new();
    let (first, _) = two_sends();
    let input = json!({"ref": first, "mute": true});
    let preview = lab.call("change_preview", json!({"kind": "send-mute", "input": input})).unwrap();
    producer_swaps_the_first_send();
    let before = send_volumes();
    let entries = state(|s| s.undo_entries);
    let apply = json!({"kind": "send-mute", "input": input, "fence": preview["fence"], "desc": "Kumi: x (k1111)"});
    let (code, message) = lab.call("change_apply", apply).unwrap_err();
    assert_eq!(code, "changed");
    assert!(message.contains("changed") || message.contains("another send"), "{message}");
    assert_eq!((send_volumes(), state(|s| s.undo_entries)), (before.clone(), entries), "no send was muted");
    assert!(before.iter().all(|(_, mute)| !mute));
}

/// A preview and an apply through a ref, as Kumi makes them; the error, if one refused.
fn mute_through(lab: &Lab, reference: &str) -> Option<(String, String)> {
    let input = json!({"ref": reference, "mute": true});
    let preview = match lab.call("change_preview", json!({"kind": "send-mute", "input": input})) {
        Ok(preview) => preview,
        Err(fault) => return Some(fault),
    };
    lab.call("change_apply", json!({"kind": "send-mute", "input": input, "fence": preview["fence"], "desc": "Kumi: x (k2222)"})).err()
}

#[test]
fn a_send_ref_read_before_the_producer_removed_one_and_added_one_is_refused_at_the_preview_whatever_was_read_since() {
    for poll_between in [false, true] {
        let lab = Lab::new();
        let (first, _) = two_sends();
        producer_swaps_the_first_send();
        if poll_between {
            // FOCUS's routing body reads the sends again while the producer looks at them.
            send_refs(1);
        }
        let before = send_volumes();
        let (code, message) = mute_through(&lab, &first).expect("refused");
        assert_eq!(code, "changed");
        assert!(message.contains("read them again"), "{message}");
        assert_eq!(send_volumes(), before, "no send was muted (poll between: {poll_between})");
    }
}

#[test]
fn a_send_ref_read_before_the_producer_changed_the_other_send_to_that_track_is_refused() {
    let lab = Lab::new();
    let (first, _) = two_sends();
    state(|s| s.tracks[1].sends[1].volume = 0.9);
    let (code, message) = mute_through(&lab, &first).expect("refused");
    assert_eq!(code, "changed");
    assert!(message.contains("read them again"), "{message}");
    assert!(send_volumes().iter().all(|(_, mute)| !mute));
}

#[test]
fn a_plan_over_two_sends_to_one_track_from_one_read_goes_through_until_the_producer_changes_them() {
    let lab = Lab::new();
    let (first, second) = two_sends();
    lab.make("send-mute", json!({"ref": first, "mute": true}), "Kumi: Send muted (ksi10)");
    lab.make("send-volume", json!({"ref": second, "volumeDb": -3.0}), "Kumi: Send -3 dB (ksi11)");
    lab.make("send-volume", json!({"ref": first, "volumeDb": -20.0}), "Kumi: Send -20 dB (ksi12)");
    assert_eq!(send_volumes(), [(0.1, true), (0.71, false)]);
    // The producer pans one: Kumi's writes no longer lead to what is there, and the refs read before name nothing.
    state(|s| s.tracks[1].sends[0].pan = 0.5);
    let (code, _) = mute_through(&lab, &second).expect("refused");
    assert_eq!(code, "changed");
    // Read again: the refs name them again.
    let refs = send_refs(1);
    assert_eq!(mute_through(&lab, &refs[1]), None);
    assert_eq!(send_volumes(), [(0.1, true), (0.71, true)]);
}

#[test]
fn a_send_alone_to_its_track_is_the_send_to_that_track_even_after_the_producer_made_it_again() {
    let lab = Lab::new();
    state(|s| {
        s.tracks[1].sends = vec![FakeSend::to(3)];
        s.state_count += 1;
    });
    let reference = send_refs(1).remove(0);
    assert_eq!(reference, "r:send:{BBBB}:0:{DDDD}", "a send alone to its track carries nothing of its state");
    state(|s| {
        s.tracks[1].sends = vec![FakeSend { volume: 0.5, ..FakeSend::to(3) }];
        s.state_count += 1;
    });
    assert_eq!(mute_through(&lab, &reference), None);
    assert_eq!(send_volumes(), [(0.5, true)], "the new send to Reverb is muted");
}

#[test]
fn a_ref_to_one_of_two_sends_without_their_state_is_refused() {
    let lab = Lab::new();
    two_sends();
    let (code, message) = mute_through(&lab, "r:send:{BBBB}:0:{DDDD}:1of2").expect("refused");
    assert_eq!(code, "changed");
    assert!(message.contains("read them again"), "{message}");
}

#[test]
fn kumis_undo_of_a_send_change_leaves_the_producers_sends_when_the_pair_changed() {
    let lab = Lab::new();
    let (first, _) = two_sends();
    let (preview, applied) = lab.make("send-mute", json!({"ref": first, "mute": true}), "Kumi: Send “Kick” → “Reverb” muted (ksi01)");
    // The producer mutes the second send too, removes the first and adds a new one.
    state(|s| s.tracks[1].sends[1].mute = true);
    producer_swaps_the_first_send();
    let before = send_volumes();
    let (code, _) = lab.undo("send-mute", &preview, &applied, "Kumi: undo (kund1)").unwrap_err();
    assert_eq!(code, "changed");
    assert_eq!(send_volumes(), before, "the producer's mute of the other send stays");
    // The sends as Kumi left them: the same undo goes through.
    state(|s| {
        s.tracks[1].sends.pop();
        s.tracks[1].sends.insert(0, {
            let mut send = FakeSend::to(3);
            send.volume = 0.25;
            send.mute = true;
            send
        });
        s.tracks[1].sends[1].mute = false;
    });
    lab.undo("send-mute", &preview, &applied, "Kumi: undo (kund2)").unwrap();
    assert_eq!(send_volumes(), [(0.25, false), (0.5, false)]);
}

#[test]
fn kumis_undo_of_a_send_it_added_finds_that_send_by_its_mark_and_never_the_producers() {
    let lab = Lab::new();
    state(|s| s.tracks[1].sends.clear());
    let input = json!({"ref": KICK, "toRef": "r:track:{DDDD}"});
    let (preview, applied) = lab.make("send-add", input, "Kumi: Added send “Kick” → “Reverb” (ksi02)");
    let kumis_stamp = applied["created"]["stamp"].as_str().unwrap().to_string();
    // The producer deletes it and makes their own send to the same track.
    state(|s| {
        s.tracks[1].sends.clear();
        s.tracks[1].sends.push(FakeSend::to(3));
        s.state_count += 1;
    });
    let (code, message) = lab.undo("send-add", &preview, &applied, "Kumi: undo (kund1)").unwrap_err();
    assert_eq!(code, "gone", "{message}: Kumi's send is not there, and the producer's is not Kumi's to take away");
    assert_eq!(state(|s| s.tracks[1].sends.len()), 1);
    // The producer puts another send in front of Kumi's: the mark finds Kumi's, whatever its place.
    state(|s| {
        s.tracks[1].sends.clear();
        let mut mine = FakeSend::to(3);
        mine.ext.insert("P_EXT:kumi_id".into(), kumis_stamp.clone());
        s.tracks[1].sends = vec![FakeSend::to(3), mine];
        s.state_count += 1;
    });
    lab.undo("send-add", &preview, &applied, "Kumi: undo (kund2)").unwrap();
    assert_eq!(state(|s| s.tracks[1].sends.len()), 1);
    assert!(state(|s| s.tracks[1].sends[0].ext.is_empty()), "the one left is the producer's");
}

#[test]
fn kumis_mark_is_taken_only_on_a_send_to_the_track_the_ref_names_and_two_sends_with_it_are_refused() {
    let lab = Lab::new();
    state(|s| s.tracks[1].sends.clear());
    let input = json!({"ref": KICK, "toRef": "r:track:{DDDD}"});
    let (preview, applied) = lab.make("send-add", input, "Kumi: Added send “Kick” → “Reverb” (ksi06)");
    let kumis_stamp = applied["created"]["stamp"].as_str().unwrap().to_string();
    // The producer duplicates Reverb: the copy receives from Kick with the same mark, and its send comes first.
    state(|s| {
        s.tracks.push(fake::FakeTrack::new("{EEEE}", "Reverb copy"));
        let mut copy = FakeSend::to(4);
        copy.ext.insert("P_EXT:kumi_id".into(), kumis_stamp.clone());
        s.tracks[1].sends.insert(0, copy);
        s.state_count += 1;
    });
    let restored = lab.undo("send-add", &preview, &applied, "Kumi: undo (kund1)").unwrap();
    assert_eq!(restored["display"]["title"], "Send “Kick” → “Reverb” taken away again");
    assert_eq!(state(|s| s.tracks[1].sends.iter().map(|send| send.dest).collect::<Vec<_>>()), [4], "the copy's send stays");

    // Two sends to Reverb with Kumi's mark: which is Kumi's can't be told, and neither is taken away.
    let lab = Lab::new();
    state(|s| s.tracks[1].sends.clear());
    let input = json!({"ref": KICK, "toRef": "r:track:{DDDD}"});
    let (preview, applied) = lab.make("send-add", input, "Kumi: Added send “Kick” → “Reverb” (ksi07)");
    state(|s| {
        let twin = FakeSend { ext: s.tracks[1].sends[0].ext.clone(), ..FakeSend::to(3) };
        s.tracks[1].sends.push(twin);
        s.state_count += 1;
    });
    let (code, message) = lab.undo("send-add", &preview, &applied, "Kumi: undo (kund2)").unwrap_err();
    assert_eq!(code, "changed");
    assert!(message.contains("can't tell which"), "{message}");
    assert_eq!(state(|s| s.tracks[1].sends.len()), 2, "nothing was taken away");
}

#[test]
fn a_send_kumi_removed_is_made_again_even_when_an_identical_one_is_left_and_keeps_its_mark() {
    let lab = Lab::new();
    state(|s| {
        s.tracks[1].sends = vec![FakeSend::to(3), FakeSend::to(3)];
        s.state_count += 1;
    });
    let (preview, applied) = lab.make("send-remove", json!({"ref": send_refs(1)[0]}), "Kumi: Removed send “Kick” → “Reverb” (ksi03)");
    assert_eq!(state(|s| s.tracks[1].sends.len()), 1);
    lab.undo("send-remove", &preview, &applied, "Kumi: undo (kund1)").unwrap();
    assert_eq!(state(|s| s.tracks[1].sends.len()), 2, "the one that was left is not taken for the one removed");
    // A retry makes no third.
    lab.undo("send-remove", &preview, &applied, "Kumi: undo (kund2)").unwrap();
    assert_eq!(state(|s| s.tracks[1].sends.len()), 2);
    // A send with a mark comes back with it.
    state(|s| {
        s.tracks[1].sends = vec![{
            let mut send = FakeSend::to(3);
            send.ext.insert("P_EXT:kumi_id".into(), "abc".into());
            send
        }];
        s.state_count += 1;
    });
    let (preview, applied) =
        lab.make("send-remove", json!({"ref": "r:send:{BBBB}:0:{DDDD}"}), "Kumi: Removed send “Kick” → “Reverb” (ksi04)");
    assert!(state(|s| s.tracks[1].sends.is_empty()));
    lab.undo("send-remove", &preview, &applied, "Kumi: undo (kund3)").unwrap();
    assert_eq!(state(|s| s.tracks[1].sends[0].ext.get("P_EXT:kumi_id").cloned()), Some("abc".to_string()));
}

#[test]
fn a_deleted_tracks_sends_come_back_with_their_marks() {
    let lab = Lab::new();
    state(|s| {
        s.tracks[0].sends[0].ext.insert("P_EXT:kumi_id".into(), "def".into());
    });
    let (preview, applied) = lab.make("track-delete", json!({"ref": "r:track:{AAAA}"}), "Kumi: Deleted track “Drums” (ksi05)");
    lab.undo("track-delete", &preview, &applied, "Kumi: undo (kund1)").unwrap();
    let marks: Vec<_> =
        state(|s| s.tracks.iter().flat_map(|t| t.sends.iter().filter_map(|send| send.ext.get("P_EXT:kumi_id").cloned())).collect());
    assert_eq!(marks, ["def"]);
}

// ----- the undo of an add leaves what the producer put on it (M3) -----

#[test]
fn undoing_an_added_track_is_refused_when_the_producer_drew_automation_on_it_or_made_it_a_folder() {
    let lab = Lab::new();
    // Automation: a volume envelope with points.
    let (preview, applied) = lab.make("track-add", json!({"name": "Pad", "index": 2}), "Kumi: Added track “Pad” (kad01)");
    state(|s| {
        s.tracks[1].envelopes.push(fake::FakeEnvelope::own("Volume", "<VOLENV2", 24));
        s.state_count += 1;
    });
    let entries = state(|s| s.tracks.len());
    let (code, message) = lab.undo("track-add", &preview, &applied, "Kumi: undo (kund1)").unwrap_err();
    assert_eq!(code, "changed");
    assert!(message.contains("automation"), "{message}");
    assert_eq!(state(|s| s.tracks.len()), entries);
    state(|s| s.tracks[1].envelopes.clear());
    // A folder: the track opens one that holds the next track.
    state(|s| {
        s.tracks[1].folder = 1;
        s.tracks[2].folder = -1;
        s.state_count += 1;
    });
    let (code, message) = lab.undo("track-add", &preview, &applied, "Kumi: undo (kund2)").unwrap_err();
    assert_eq!(code, "changed");
    assert!(message.contains("folder"), "{message}");
    assert_eq!(state(|s| s.tracks.len()), entries);
    // Told to discard, it goes.
    let mut params = json!({"kind": "track-add", "target": preview["target"], "prior": preview["prior"], "applied": applied["applied"], "created": applied["created"], "desc": "Kumi: undo (kund3)", "discard": true});
    params["epoch"] = lab.reads.epoch().into();
    lab.call("change_restore", params).unwrap();
    assert_eq!(state(|s| s.tracks.len()), entries - 1);
}

#[test]
fn undoing_an_added_fx_is_refused_when_the_producer_automated_one_of_its_parameters() {
    let lab = Lab::new();
    let (preview, applied) = lab.make("fx-add", json!({"ref": KICK, "name": "ReaEQ"}), "Kumi: Added “ReaEQ” to “Kick” (kad02)");
    let guid = applied["applied"]["guid"].as_str().unwrap().to_string();
    state(|s| {
        s.tracks[1].envelopes.push(fake::FakeEnvelope::for_param("ReaEQ: Gain", &guid, 0, 16));
        s.state_count += 1;
    });
    let (code, message) = lab.undo("fx-add", &preview, &applied, "Kumi: undo (kund1)").unwrap_err();
    assert_eq!(code, "changed");
    assert!(message.contains("automation"), "{message}");
    assert_eq!(kick_fx().len(), 1);
    let mut params = restore_params(&preview, &applied, "Kumi: undo (kund2)");
    params["kind"] = "fx-add".into();
    params["discard"] = true.into();
    lab.call("change_restore", params).unwrap();
    assert!(kick_fx().is_empty());
}

// ----- folders around a deleted track, big tracks, a folder moved (M5, S2, S7) -----

/// Replaces the lab's tracks with these (name, folder depth), ids {T-name}, and no sends.
fn folder_tracks(spec: &[(&str, i32)]) {
    state(|s| {
        s.tracks = spec
            .iter()
            .map(|(name, depth)| {
                let mut track = fake::FakeTrack::new(&format!("{{T-{name}}}"), name);
                track.folder = *depth;
                track
            })
            .collect();
        s.state_count += 1;
    });
}

fn track_depths() -> Vec<(String, i32)> {
    state(|s| s.tracks.iter().map(|t| (t.name.clone(), t.folder)).collect())
}

fn depths_are(expected: &[(&str, i32)]) -> bool {
    track_depths() == expected.iter().map(|(n, d)| (n.to_string(), *d)).collect::<Vec<_>>()
}

#[test]
fn undoing_a_delete_writes_no_folder_depth_the_deletion_didnt_change() {
    let lab = Lab::new();
    folder_tracks(&[("Group", 1), ("A", 0), ("B", 0), ("C", -1), ("D", 0)]);
    let (preview, applied) = lab.make("track-delete", json!({"ref": "r:track:{T-B}"}), "Kumi: Deleted track “B” (kdp01)");
    assert_eq!(applied["material"]["depths"], json!([]), "deleting a middle track changes no depth");
    // The producer drags D into the folder: C no longer closes it, D does.
    state(|s| {
        s.tracks[2].folder = 0;
        s.tracks[3].folder = -1;
    });
    lab.undo("track-delete", &preview, &applied, "Kumi: undo (kund1)").unwrap();
    assert!(depths_are(&[("Group", 1), ("A", 0), ("B", 0), ("C", 0), ("D", -1)]), "{:?}", track_depths());
}

#[test]
fn a_folder_that_was_mended_by_a_delete_is_mended_back_unless_the_producer_rearranged_it() {
    let lab = Lab::new();
    folder_tracks(&[("G", 1), ("H", 1), ("A", 0), ("B", -2), ("D", 0)]);
    let (preview, applied) = lab.make("track-delete", json!({"ref": "r:track:{T-H}"}), "Kumi: Deleted track “H” (kdp02)");
    // REAPER made B close one folder fewer, and Kumi kept what it made of it.
    assert_eq!(applied["material"]["depths"], json!([{"guid": "{T-B}", "before": -2, "after": -1}]));
    assert!(depths_are(&[("G", 1), ("A", 0), ("B", -1), ("D", 0)]), "{:?}", track_depths());
    // The producer made B close nothing and D close the folder: B is not as the deletion left it.
    state(|s| {
        s.tracks[2].folder = 0;
        s.tracks[3].folder = -1;
    });
    let (code, message) = lab.undo("track-delete", &preview, &applied, "Kumi: undo (kund1)").unwrap_err();
    assert_eq!(code, "changed");
    assert!(message.contains("rearranged"), "{message}");
    assert_eq!(state(|s| s.tracks.len()), 4, "nothing was made");
    // As the deletion left it: B is put back to -2 and H is a folder again.
    state(|s| {
        s.tracks[2].folder = -1;
        s.tracks[3].folder = 0;
    });
    lab.undo("track-delete", &preview, &applied, "Kumi: undo (kund2)").unwrap();
    assert!(depths_are(&[("G", 1), ("H", 1), ("A", 0), ("B", -2), ("D", 0)]), "{:?}", track_depths());
}

#[test]
fn a_delete_whose_folders_the_producer_rearranged_is_put_back_when_told_to_discard() {
    let lab = Lab::new();
    folder_tracks(&[("G", 1), ("H", 1), ("A", 0), ("B", -2), ("D", 0)]);
    let (preview, applied) = lab.make("track-delete", json!({"ref": "r:track:{T-H}"}), "Kumi: Deleted track “H” (kdp03)");
    state(|s| s.tracks[2].folder = 0);
    let mut params = restore_params(&preview, &applied, "Kumi: undo (kund1)");
    params["kind"] = "track-delete".into();
    params["material"] = applied["material"].clone();
    params["discard"] = true.into();
    lab.call("change_restore", params).unwrap();
    assert!(depths_are(&[("G", 1), ("H", 1), ("A", 0), ("B", -2), ("D", 0)]), "{:?}", track_depths());
}

/// The producer drags track `from` to place `to` (places counted once it is taken out).
fn drag(from: usize, to: usize) {
    state(|s| {
        let track = s.tracks.remove(from);
        s.tracks.insert(to, track);
        s.state_count += 1;
    });
}

#[test]
fn a_deleted_folders_last_track_goes_back_after_the_track_above_it_when_the_producer_dragged_another_in_before() {
    let lab = Lab::new();
    folder_tracks(&[("Group", 1), ("A", 0), ("B", -1), ("C", 0)]);
    let (preview, applied) = lab.make("track-delete", json!({"ref": "r:track:{T-B}"}), "Kumi: Deleted track “B” (kdp20)");
    assert!(depths_are(&[("Group", 1), ("A", -1), ("C", 0)]), "{:?}", track_depths());
    // The producer drags C into the folder, above A.
    drag(2, 1);
    state(|s| {
        s.tracks[1].folder = 0;
        s.tracks[2].folder = -1;
    });
    assert!(depths_are(&[("Group", 1), ("C", 0), ("A", -1)]), "{:?}", track_depths());
    lab.undo("track-delete", &preview, &applied, "Kumi: undo (kund1)").unwrap();
    assert!(depths_are(&[("Group", 1), ("C", 0), ("A", 0), ("B", -1)]), "A stays in the folder, B closes it: {:?}", track_depths());
}

#[test]
fn a_deleted_track_goes_back_beside_its_old_neighbours_wherever_the_producer_moved_them() {
    // After the track that was above it.
    let lab = Lab::new();
    folder_tracks(&[("X", 0), ("A", 0), ("B", 0), ("C", 0)]);
    let (preview, applied) = lab.make("track-delete", json!({"ref": "r:track:{T-B}"}), "Kumi: Deleted track “B” (kdp21)");
    drag(1, 2);
    assert_eq!(names(), ["X", "C", "A"]);
    lab.undo("track-delete", &preview, &applied, "Kumi: undo (kund1)").unwrap();
    assert_eq!(names(), ["X", "C", "A", "B"]);

    // Before the track that was below it, when the one above is gone.
    let lab = Lab::new();
    folder_tracks(&[("X", 0), ("A", 0), ("B", 0), ("C", 0)]);
    let (preview, applied) = lab.make("track-delete", json!({"ref": "r:track:{T-B}"}), "Kumi: Deleted track “B” (kdp22)");
    state(|s| {
        s.tracks.remove(1);
        s.state_count += 1;
    });
    drag(1, 0);
    assert_eq!(names(), ["C", "X"]);
    lab.undo("track-delete", &preview, &applied, "Kumi: undo (kund1)").unwrap();
    assert_eq!(names(), ["B", "C", "X"]);

    // The first track goes back before the one that was below it.
    let lab = Lab::new();
    folder_tracks(&[("A", 0), ("B", 0), ("C", 0)]);
    let (preview, applied) = lab.make("track-delete", json!({"ref": "r:track:{T-A}"}), "Kumi: Deleted track “A” (kdp23)");
    drag(0, 1);
    assert_eq!(names(), ["C", "B"]);
    lab.undo("track-delete", &preview, &applied, "Kumi: undo (kund1)").unwrap();
    assert_eq!(names(), ["C", "A", "B"]);

    // Its old place, when both are gone.
    let lab = Lab::new();
    folder_tracks(&[("X", 0), ("A", 0), ("B", 0), ("C", 0), ("D", 0)]);
    let (preview, applied) = lab.make("track-delete", json!({"ref": "r:track:{T-B}"}), "Kumi: Deleted track “B” (kdp24)");
    state(|s| {
        s.tracks.retain(|t| t.name != "A" && t.name != "C");
        s.state_count += 1;
    });
    lab.undo("track-delete", &preview, &applied, "Kumi: undo (kund1)").unwrap();
    assert_eq!(names(), ["X", "D", "B"], "its old place (third) is past the end now: it goes last");
}

// ----- folders over random layouts (N-1, N-2) -----

/// A small random number generator, so the layouts of the tests are the same each time.
struct Dice(u64);

impl Dice {
    fn new(seed: u64) -> Dice {
        Dice(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }

    fn below(&mut self, n: usize) -> usize {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        ((self.0 >> 11) % n as u64) as usize
    }
}

/// A layout of 2 to 10 tracks, "t0", "t1"…: each opens a folder, closes one or two, or is plain.
fn random_layout(dice: &mut Dice) -> Vec<(String, i32)> {
    (0..2 + dice.below(9))
        .map(|at| {
            let depth = match dice.below(10) {
                0..=2 => 1,
                3..=6 => 0,
                7..=8 => -1,
                _ => -2,
            };
            (format!("t{at}"), depth)
        })
        .collect()
}

/// Each track's folder as the depths say, by name: the innermost one open when it comes.
fn folders_of(layout: &[(String, i32)]) -> std::collections::BTreeMap<String, Option<String>> {
    let mut open: Vec<&str> = Vec::new();
    let mut parents = std::collections::BTreeMap::new();
    for (name, depth) in layout {
        parents.insert(name.clone(), open.last().map(|parent| parent.to_string()));
        if *depth > 0 {
            open.push(name);
        }
        for _ in 0..(-*depth).max(0) {
            open.pop();
        }
    }
    parents
}

fn spec_of(layout: &[(String, i32)]) -> Vec<(&str, i32)> {
    layout.iter().map(|(name, depth)| (name.as_str(), *depth)).collect()
}

fn lab_layout() -> Vec<(String, i32)> {
    track_depths()
}

#[test]
fn a_delete_leaves_every_other_track_in_its_folder_but_for_the_tracks_of_a_folder_that_goes() {
    for seed in 0..400 {
        let mut dice = Dice::new(seed);
        let layout = random_layout(&mut dice);
        let gone = dice.below(layout.len());
        let lab = Lab::new();
        folder_tracks(&spec_of(&layout));
        let before = folders_of(&layout);
        lab.make("track-delete", json!({"ref": format!("r:track:{{T-t{gone}}}")}), "Kumi: Deleted track (kdp10)");
        let after = folders_of(&lab_layout());
        let name = format!("t{gone}");
        for (track, parent) in &after {
            let was = match &before[track] {
                Some(parent) if *parent == name => before[&name].clone(),
                other => other.clone(),
            };
            assert_eq!(*parent, was, "seed {seed}: {layout:?} without {name}: {track} is in {parent:?}, it was in {:?}", before[track]);
        }
        assert_eq!(after.len(), layout.len() - 1);
    }
}

#[test]
fn a_delete_and_its_undo_with_nothing_between_give_every_depth_back() {
    for seed in 0..400 {
        let mut dice = Dice::new(seed + 1000);
        let layout = random_layout(&mut dice);
        let gone = dice.below(layout.len());
        let lab = Lab::new();
        folder_tracks(&spec_of(&layout));
        let (preview, applied) = lab.make("track-delete", json!({"ref": format!("r:track:{{T-t{gone}}}")}), "Kumi: Deleted track (kdp11)");
        lab.undo("track-delete", &preview, &applied, "Kumi: undo (kund1)")
            .unwrap_or_else(|e| panic!("seed {seed}: {layout:?} without t{gone}: {e:?}"));
        assert_eq!(lab_layout(), layout, "seed {seed}: t{gone} was deleted and put back");
    }
}

#[test]
fn a_delete_whose_folders_the_producer_refolded_is_refused_or_changes_no_other_tracks_folder() {
    let (mut put_back, mut refused) = (0, 0);
    for seed in 0..600 {
        let mut dice = Dice::new(seed + 5000);
        let layout = random_layout(&mut dice);
        let gone = dice.below(layout.len());
        let name = format!("t{gone}");
        let lab = Lab::new();
        folder_tracks(&spec_of(&layout));
        let children: Vec<String> = folders_of(&layout)
            .into_iter()
            .filter(|(_, parent)| parent.as_deref() == Some(name.as_str()))
            .map(|(track, _)| track)
            .collect();
        let (preview, applied) = lab.make("track-delete", json!({"ref": format!("r:track:{{T-t{gone}}}")}), "Kumi: Deleted track (kdp12)");
        // The producer folds again: a few of the tracks left take another depth.
        state(|s| {
            for _ in 0..1 + dice.below(3) {
                let at = dice.below(s.tracks.len());
                s.tracks[at].folder = [-2, -1, 0, 0, 1][dice.below(5)];
            }
        });
        let now = lab_layout();
        let result = lab.undo("track-delete", &preview, &applied, "Kumi: undo (kund1)");
        match result {
            Err((code, _)) => {
                assert_eq!(code, "changed", "seed {seed}");
                assert_eq!(lab_layout(), now, "seed {seed}: nothing was written");
                refused += 1;
            }
            Ok(_) => {
                put_back += 1;
                let (before, after) = (folders_of(&now), folders_of(&lab_layout()));
                for (track, parent) in &before {
                    let after = &after[track];
                    let adopted = children.contains(track) && after.as_deref() == Some(name.as_str());
                    assert!(
                        after == parent || adopted,
                        "seed {seed}: {layout:?} without {name}, refolded to {now:?}: {track} was in {parent:?} and is in {after:?}"
                    );
                }
            }
        }
    }
    assert!(put_back > 20 && refused > 20, "put back {put_back}, refused {refused}");
}

#[test]
fn a_track_too_big_to_keep_is_not_deleted() {
    let lab = Lab::new();
    // A name of 25 MiB: the track's chunk is as big.
    state(|s| s.tracks[1].name = "x".repeat(25 << 20));
    let entries = state(|s| s.undo_entries);
    let preview = lab.call("change_preview", json!({"kind": "track-delete", "input": {"ref": KICK}})).unwrap();
    let apply = json!({"kind": "track-delete", "input": {"ref": KICK}, "fence": preview["fence"], "desc": "Kumi: x (k1111)"});
    let (code, message) = lab.call("change_apply", apply).unwrap_err();
    assert_eq!(code, "failed");
    assert!(message.contains("won't delete what it can't put back"), "{}", &message[..message.len().min(300)]);
    assert_eq!((state(|s| s.tracks.len()), state(|s| s.undo_entries)), (4, entries));
}

#[test]
fn a_folder_is_not_moved_alone() {
    let lab = Lab::new();
    // Drums is a folder of Kick and Snare.
    let (code, message) =
        lab.call("change_preview", json!({"kind": "track-move", "input": {"ref": "r:track:{AAAA}", "index": 4}})).unwrap_err();
    assert_eq!(code, "bad-params");
    assert!(message.contains("folder"), "{message}");
    assert_eq!(names(), ["Drums", "Kick", "Snare", "Reverb"]);
}

#[test]
fn the_index_a_read_gives_is_the_index_a_change_takes() {
    let lab = Lab::new();
    let api = fake::api();
    let read = |kind: &str, extra: Value| {
        let mut params = json!({"kind": kind, "epoch": lab.reads.epoch()});
        for (key, value) in extra.as_object().unwrap() {
            params[key] = value.clone();
        }
        lab.reads.read(&Reaper::new(&api), &params).unwrap()
    };
    let guids = three_fx(&lab);
    let fx = read("fx", json!({"parent": KICK}));
    assert_eq!(fx["items"][2]["ref"], fx_ref_of(&guids[2]));
    lab.make("fx-move", json!({"ref": fx_ref_of(&guids[2]), "index": 0}), "Kumi: Moved “ReaDelay” to place 1 on “Kick” (kix03)");
    assert_eq!(kick_fx()[0].0, guids[2], "FX index 0 is the first place");
    // Tracks are counted from 1 in a read, and a change takes the place it shows.
    let tracks = read("track", json!({}));
    assert_eq!(tracks["indexFrom"], 1);
    let rows: Vec<(String, i64)> = tracks["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| (r["name"].as_str().unwrap().to_string(), r["index"].as_i64().unwrap()))
        .collect();
    // (The master comes first, as 0; the tracks are 1, 2, 3.)
    let named: Vec<(&str, i64)> = rows.iter().map(|(n, i)| (n.as_str(), *i)).collect();
    assert_eq!(named, [("MASTER", 0), ("Drums", 1), ("Kick", 2), ("Snare", 3), ("Reverb", 4)]);
    lab.make("track-move", json!({"ref": KICK, "index": 3}), "Kumi: Moved track “Kick” to position 3 (kix01)");
    assert_eq!(names(), ["Drums", "Snare", "Kick", "Reverb"], "Kick took the place its neighbour's row said");
    lab.make("track-add", json!({"name": "Pad", "index": 1}), "Kumi: Added track “Pad” (kix02)");
    assert_eq!(names()[0], "Pad", "index 1 is the first place");
    // FX, parameters and notes are counted from 0 in a read, and in a change.
    for kind in ["fx", "param", "note"] {
        let parent = match kind {
            "fx" => json!({"parent": "r:track:{AAAA}"}),
            "param" => json!({"parent": "r:fx:{FX-EQ}"}),
            _ => json!({"parent": SNARE_TAKE}),
        };
        let page = read(kind, parent);
        assert_eq!(page["indexFrom"], 0, "{kind}");
        assert_eq!(page["items"][0]["index"], 0, "{kind}");
    }
}

// ----- an undo after a look at another tab and back (S3) -----

#[test]
fn an_undo_asked_in_an_earlier_epoch_of_the_same_project_goes_through_and_another_projects_does_not() {
    let lab = Lab::new();
    let api = fake::api();
    let proj = Reaper::new(&api).current_project().unwrap().ptr;
    lab.reads.note_project(proj);
    let first = lab.reads.epoch();
    let (preview, applied) = lab.make("track-volume", json!({"ref": KICK, "volumeDb": -6.0}), "Kumi: Kick volume −6 dB (ktb01)");
    // The producer looks at another tab and comes back: the epoch moved twice, to the same project.
    for _ in 0..2 {
        lab.reads.bump_epoch();
        lab.reads.note_project(proj);
    }
    assert_ne!(lab.reads.epoch(), first);
    let mut params = json!({"kind": "track-volume", "target": preview["target"], "prior": preview["prior"], "applied": applied["applied"], "desc": "Kumi: undo (kund1)"});
    params["epoch"] = first.clone().into();
    // A change is not asked in a past epoch.
    let apply = json!({"kind": "track-volume", "input": {"ref": KICK, "volumeDb": -3.0}, "fence": preview["fence"], "desc": "Kumi: x (k1111)", "epoch": first});
    assert_eq!(lab.call("change_apply", apply).unwrap_err().0, "stale");
    // An undo of it is: the id it names is the same.
    let restored = lab.call("change_restore", params.clone()).unwrap();
    assert_eq!(restored["epoch"], lab.reads.epoch().as_str(), "the answer carries the epoch now");
    assert_eq!(state(|s| s.tracks[1].volume), 1.0);
    // A different project took the tab: what was done in the project before is another project's.
    let (preview, applied) = lab.make("track-volume", json!({"ref": KICK, "volumeDb": -9.0}), "Kumi: Kick volume −9 dB (ktb02)");
    let before = lab.reads.epoch();
    lab.reads.project_replaced(proj);
    lab.reads.bump_epoch();
    lab.reads.note_project(proj);
    let mut params = json!({"kind": "track-volume", "target": preview["target"], "prior": preview["prior"], "applied": applied["applied"], "desc": "Kumi: undo (kund2)"});
    params["epoch"] = before.into();
    assert_eq!(lab.call("change_restore", params).unwrap_err().0, "stale");
    // An epoch that was never seen is not any project's.
    let mut unknown = json!({"kind": "track-volume", "target": preview["target"], "prior": preview["prior"], "applied": applied["applied"], "desc": "Kumi: undo (kund3)"});
    unknown["epoch"] = "never-1".into();
    assert_eq!(lab.call("change_restore", unknown).unwrap_err().0, "stale");
}

// ----- a write that goes wrong is taken back, or said to be unknown (S4); notes cost and undo overlaps (S5, S6) -----

#[test]
fn a_name_that_stands_for_several_plug_ins_is_refused_before_anything_is_added() {
    let lab = Lab::new();
    for name in ["FXADD:ReaEQ", "fxadd:", "  FXADD:x"] {
        let (code, message) = lab.call("change_preview", json!({"kind": "fx-add", "input": {"ref": KICK, "name": name}})).unwrap_err();
        assert_eq!(code, "bad-params", "{name}");
        assert!(message.contains("one plug-in"), "{message}");
    }
    assert!(kick_fx().is_empty());
}

#[test]
fn an_add_that_leaves_more_than_it_should_is_taken_back_inside_the_same_call() {
    let lab = Lab::new();
    // A name that adds two plug-ins where one was asked for.
    let input = json!({"ref": KICK, "name": "ReaEQ twice"});
    let preview = lab.call("change_preview", json!({"kind": "fx-add", "input": input})).unwrap();
    let apply = json!({"kind": "fx-add", "input": input, "fence": preview["fence"], "desc": "Kumi: x (k1111)"});
    let (code, message) = lab.call("change_apply", apply.clone()).unwrap_err();
    assert_eq!(code, "failed");
    assert!(message.contains("taken back"), "{message}");
    assert!(kick_fx().is_empty(), "nothing of it is left");
    // Taken back inside a block that wrote nothing in the end: no entry the producer would find.
    assert_eq!(state(|s| s.undo_depth), 0);
    // If the plug-ins can't be taken off again, nobody knows what the track holds.
    state(|s| s.fail_fx_delete = true);
    let (code, message) = lab.call("change_apply", apply).unwrap_err();
    assert_eq!(code, "uncertain");
    assert!(message.contains("could not take back"), "{message}");
    assert_eq!(kick_fx().len(), 2);
}

#[test]
fn a_notes_write_that_goes_wrong_is_taken_back_or_called_uncertain() {
    let lab = Lab::new();
    let input =
        json!({"ref": SNARE_TAKE, "notes": [{"pitch": 60, "start": 4.0, "length": 1.0}, {"pitch": 61, "start": 5.0, "length": 1.0}]});
    let before = snare_notes();
    let preview = lab.call("change_preview", json!({"kind": "notes-add", "input": input})).unwrap();
    let apply = json!({"kind": "notes-add", "input": input, "fence": preview["fence"], "desc": "Kumi: x (k1111)"});
    // The write drops a note; putting the take back works.
    state(|s| s.glitch_events = 1);
    let (code, message) = lab.call("change_apply", apply.clone()).unwrap_err();
    assert_eq!(code, "failed");
    assert!(message.contains("taken back"), "{message}");
    assert_eq!(snare_notes(), before);
    // The write drops one and so does putting back.
    state(|s| s.glitch_events = 2);
    let (code, _) = lab.call("change_apply", apply.clone()).unwrap_err();
    assert_eq!(code, "uncertain");
    // And an undo that goes wrong the same way.
    let (preview, applied) = lab.make("notes-add", input, "Kumi: Wrote 2 notes (kno01)");
    let written = snare_notes();
    state(|s| s.glitch_events = 1);
    let (code, _) = lab.undo("notes-add", &preview, &applied, "Kumi: undo (kund1)").unwrap_err();
    assert_eq!(code, "failed");
    assert_eq!(snare_notes(), written);
}

/// The notes of a take as `read` gives them.
fn read_notes_of(lab: &Lab, parent: &str) -> Vec<Value> {
    let api = fake::api();
    let answer = lab.reads.read(&Reaper::new(&api), &json!({"kind": "note", "parent": parent})).unwrap();
    answer["items"].as_array().unwrap().clone()
}

#[test]
fn a_note_is_read_with_the_length_a_change_writes_back_when_two_notes_of_a_pitch_overlap() {
    let lab = Lab::new();
    let note = |start_ppq, end_ppq, pitch, velocity| fake::FakeNote {
        start_ppq,
        end_ppq,
        channel: 0,
        pitch,
        velocity,
        selected: false,
        muted: false,
    };
    // The producer's take: two notes of one pitch and channel that overlap (REAPER does not correct a take it loaded), and
    // one note beside them. REAPER's own `MIDI_GetNote` ends both overlapping notes at the first note-off.
    state(|s| {
        s.tracks[2].items[0].takes[0].notes = vec![note(0.0, 480.0, 72, 84), note(0.0, 1440.0, 72, 85), note(1000.0, 1480.0, 60, 90)]
    });
    let rows = read_notes_of(&lab, SNARE_TAKE);
    assert_eq!(rows.iter().map(|r| r["length"].as_f64().unwrap()).collect::<Vec<_>>(), [0.5, 1.5, 0.5]);
    // What a change would write back for each note is what was read.
    for (i, row) in rows.iter().enumerate() {
        let preview = lab.call("change_preview", json!({"kind": "notes-delete", "input": {"ref": SNARE_TAKE, "indexes": [i]}})).unwrap();
        assert_eq!(preview["display"]["before"][0], *row, "note {i}");
    }
    // A change beside them writes the whole take back; the two keep the lengths that were read.
    let (preview, applied) = lab.make(
        "notes-change",
        json!({"ref": SNARE_TAKE, "changes": [{"index": 2, "velocity": 33}]}),
        "Kumi: Changed 1 note in snare.mid take (kov10)",
    );
    let after = read_notes_of(&lab, SNARE_TAKE);
    assert_eq!((&after[0], &after[1]), (&rows[0], &rows[1]));
    assert_eq!(after[2]["velocity"], 33);
    assert_eq!(state(|s| s.tracks[2].items[0].takes[0].notes.iter().map(|n| n.end_ppq).collect::<Vec<_>>()), [480.0, 1440.0, 1480.0]);
    lab.undo("notes-change", &preview, &applied, "Kumi: undo (kund1)").unwrap();
    assert_eq!(read_notes_of(&lab, SNARE_TAKE), rows);
}

#[test]
fn undoing_a_notes_delete_is_refused_when_the_producer_wrote_a_note_over_one_it_would_put_back() {
    let lab = Lab::new();
    // The snare take has pitch 38 on channel 1 from tick 0 to 480.
    let (preview, applied) = lab.make("notes-delete", json!({"ref": SNARE_TAKE, "indexes": [0]}), "Kumi: Deleted 1 note (kno02)");
    state(|s| {
        s.tracks[2].items[0].takes[0].notes.insert(
            0,
            fake::FakeNote { start_ppq: 100.0, end_ppq: 600.0, channel: 0, pitch: 38, velocity: 70, selected: false, muted: false },
        );
    });
    let notes = snare_notes();
    let (code, message) = lab.undo("notes-delete", &preview, &applied, "Kumi: undo (kund1)").unwrap_err();
    assert_eq!(code, "changed");
    assert!(message.contains("overlap"), "{message}");
    assert_eq!(snare_notes(), notes, "nothing was written");
    // Told to discard, it is put back.
    let mut params = restore_params(&preview, &applied, "Kumi: undo (kund2)");
    params["kind"] = "notes-delete".into();
    params["discard"] = true.into();
    lab.call("change_restore", params).unwrap();
    assert_eq!(snare_notes().len(), 3);
}

#[test]
fn a_notes_change_in_a_big_take_costs_no_more_calls_than_in_a_small_one() {
    let lab = Lab::new();
    let fill = |count: i32| {
        state(|s| {
            s.tracks[2].items[0].takes[0].notes = (0..count)
                .map(|n| fake::FakeNote {
                    start_ppq: n as f64 * 960.0,
                    end_ppq: n as f64 * 960.0 + 480.0,
                    channel: 0,
                    pitch: 40 + n % 40,
                    velocity: 90,
                    selected: false,
                    muted: false,
                })
                .collect();
        })
    };
    let notes: Vec<Value> = (0..100).map(|i| json!({"pitch": 100 + i % 20, "start": 200_000.0 + i as f64, "length": 0.5})).collect();
    let mut costs = vec![];
    for (n, count) in [100, 20_000, 100_000].into_iter().enumerate() {
        fill(count);
        let started = Instant::now();
        let calls = state(|s| s.calls);
        let (preview, applied) =
            lab.make("notes-add", json!({"ref": SNARE_TAKE, "notes": notes}), &format!("Kumi: Wrote 100 notes (kno0{n})"));
        let apply_calls = state(|s| s.calls) - calls;
        let calls = state(|s| s.calls);
        lab.undo("notes-add", &preview, &applied, "Kumi: undo (kund1)").unwrap();
        let undo_calls = state(|s| s.calls) - calls;
        assert_eq!(snare_notes().len(), count as usize);
        assert!(started.elapsed() < std::time::Duration::from_secs(30), "{count} notes: {:?}", started.elapsed());
        costs.push((apply_calls, undo_calls));
    }
    // (The calls a note costs are the conversions of the 100 notes it writes; the take's size adds none.)
    assert_eq!(costs[0], costs[1], "{costs:?}");
    assert_eq!(costs[1], costs[2], "{costs:?}");
}

// ----- a restore that writes nothing makes no point (N1) -----

#[test]
fn a_restore_or_apply_that_writes_nothing_closes_without_a_point_so_it_commits_nothing_of_the_producers() {
    let lab = Lab::new();
    let (preview, applied) = lab.make("track-volume", json!({"ref": KICK, "volumeDb": -6.0}), "Kumi: Kick volume −6 dB (kn101)");
    assert_eq!(applied["undo"]["entry"], true);
    let restored = lab.undo("track-volume", &preview, &applied, "Kumi: undo (kund1)").unwrap();
    assert_eq!(restored["undo"]["entry"], true, "an undo that wrote is a point");
    // The producer has bare edits that REAPER hasn't made a point of yet; the same undo is asked again (a retry).
    state(|s| s.undo_dirty = true);
    let entries = state(|s| s.undo_entries);
    let again = lab.undo("track-volume", &preview, &applied, "Kumi: undo (kund2)").unwrap();
    assert_eq!(again["undo"]["entry"], false, "nothing was written, so there is no point");
    assert_eq!(state(|s| s.undo_entries), entries);
    assert!(state(|s| s.undo_dirty), "the producer's pending edits are not committed under Kumi's name");
    // The same for an apply that finds it so already.
    let (_, applied) = lab.make("track-volume", json!({"ref": KICK, "volumeDb": 0.0}), "Kumi: Kick volume 0 dB (kn102)");
    assert_eq!(applied["undo"]["entry"], false);
    assert_eq!(state(|s| s.undo_entries), entries);
    assert!(state(|s| s.undo_dirty));
    // And the other kinds that can find it so: a send, an FX parameter, a note retry, a tempo, the loop.
    let (preview, applied) = lab.make("fx-add", json!({"ref": KICK, "name": "ReaEQ"}), "Kumi: Added “ReaEQ” to “Kick” (kn103)");
    lab.undo("fx-add", &preview, &applied, "Kumi: undo (kund3)").unwrap();
    let entries = state(|s| s.undo_entries);
    let (preview, applied) = lab.make("notes-delete", json!({"ref": SNARE_TAKE, "indexes": [1]}), "Kumi: Deleted 1 note (kn104)");
    lab.undo("notes-delete", &preview, &applied, "Kumi: undo (kund4)").unwrap();
    let entries_after = state(|s| s.undo_entries);
    assert_eq!(entries_after, entries + 2);
    let retry = lab.undo("notes-delete", &preview, &applied, "Kumi: undo (kund5)").unwrap();
    assert_eq!(retry["undo"]["entry"], false);
    assert_eq!(state(|s| s.undo_entries), entries_after);
}

// ----- the undo watch with a long history, and an undo with an edit in one look (N2) -----

#[test]
fn an_undo_is_still_seen_when_hundreds_of_points_are_redoable() {
    let lab = Lab::new();
    let desc = "Kumi: Kick volume −6 dB (kn201)";
    lab.make("track-volume", json!({"ref": KICK, "volumeDb": -6.0}), desc);
    for n in 0..300 {
        fake::producer_edit(&format!("Edit {n}"));
    }
    lab.tick();
    lab.events.take();
    // The producer steps back over 300 of their own edits: none is Kumi's.
    for _ in 0..300 {
        fake::producer_undo();
    }
    lab.tick();
    assert!(lab.events.take().is_empty());
    // And then over Kumi's: more than 256 points can be redone now, which a look at the first 256 of them can't tell apart.
    fake::producer_undo();
    lab.tick();
    assert_eq!(lab.events.take(), [("undone_in_daw".to_string(), json!({"descs": [desc], "how": "undo", "epoch": lab.reads.epoch()}))]);
    fake::producer_redo();
    lab.tick();
    assert_eq!(lab.events.take(), [("undone_in_daw".to_string(), json!({"descs": [desc], "how": "redo", "epoch": lab.reads.epoch()}))]);
}

#[test]
fn reaper_dropping_its_oldest_points_when_its_history_is_full_is_no_undo_of_kumis_change() {
    let lab = Lab::new();
    for n in 0..600 {
        fake::producer_edit(&format!("Edit {n}"));
    }
    let desc = "Kumi: Kick volume −6 dB (kn203)";
    lab.make("track-volume", json!({"ref": KICK, "volumeDb": -6.0}), desc);
    for n in 0..60 {
        fake::producer_edit(&format!("Edit after {n}"));
    }
    lab.tick();
    lab.events.take();
    // The producer's next edit fills the history: REAPER drops its 246 oldest points at once.
    fake::producer_edit("Adjust track pan");
    fake::trim_history(246);
    lab.tick();
    assert!(lab.events.take().is_empty(), "Kumi's change is still applied, 61 points down");
    // Undone after all: told.
    for _ in 0..62 {
        fake::producer_undo();
    }
    lab.tick();
    assert_eq!(lab.events.take(), [("undone_in_daw".to_string(), json!({"descs": [desc], "how": "undo", "epoch": lab.reads.epoch()}))]);
}

#[test]
fn an_undo_and_a_new_edit_between_two_looks_still_tell_that_kumis_point_was_undone() {
    let lab = Lab::new();
    let desc = "Kumi: Kick volume −6 dB (kn202)";
    lab.make("track-volume", json!({"ref": KICK, "volumeDb": -6.0}), desc);
    lab.tick();
    lab.events.take();
    // The same number of points, with the project at the same place, and Kumi's gone.
    fake::producer_undo();
    fake::producer_edit("Adjust track pan");
    lab.tick();
    assert_eq!(lab.events.take(), [("undone_in_daw".to_string(), json!({"descs": [desc], "how": "undo", "epoch": lab.reads.epoch()}))]);
    // Bare edits that make no point, and the producer's own, say nothing.
    state(|s| s.state_count += 1);
    lab.tick();
    fake::producer_edit("Adjust track mute");
    lab.tick();
    assert!(lab.events.take().is_empty());
}

#[test]
fn a_track_put_back_does_not_join_the_selection_the_producer_has_made_since() {
    let lab = Lab::new();
    state(|s| s.tracks[3].selected = true);
    let (preview, applied) = lab.make("track-delete", json!({"ref": "r:track:{DDDD}"}), "Kumi: Deleted track “Reverb” (kn301)");
    // The producer selects Kick; the track comes back without being selected beside it.
    state(|s| s.tracks[1].selected = true);
    lab.undo("track-delete", &preview, &applied, "Kumi: undo (kund1)").unwrap();
    assert_eq!(state(|s| s.tracks.iter().map(|t| t.selected).collect::<Vec<_>>()), [false, true, false, false]);
    // With nothing selected, it is the selection again, as it was.
    state(|s| s.tracks[3].selected = true);
    let (preview, applied) = lab.make("track-delete", json!({"ref": "r:track:{DDDD}"}), "Kumi: Deleted track “Reverb” (kn302)");
    state(|s| s.tracks.iter_mut().for_each(|t| t.selected = false));
    lab.undo("track-delete", &preview, &applied, "Kumi: undo (kund2)").unwrap();
    assert_eq!(state(|s| s.tracks.iter().map(|t| t.selected).collect::<Vec<_>>()), [false, false, false, true]);
}

// ----- a marker made again (N3) -----

#[test]
fn a_marker_made_again_is_made_once_keeps_its_lane_and_a_region_its_render_matrix() {
    let lab = Lab::new();
    state(|s| {
        s.markers[1].number = 7;
        s.marker_lanes.insert("{REG-1}".into(), 2);
        s.render_matrix.insert(7, vec!["{BBBB}".into(), "master".into()]);
    });
    let (preview, applied) = lab.make("marker-delete", json!({"ref": "r:region:{REG-1}"}), "Kumi: Deleted region “Verse” (kmk01)");
    assert_eq!(applied["material"]["matrix"], json!(["{BBBB}", "master"]));
    assert_eq!(applied["material"]["lane"], 2);
    assert_eq!(state(|s| s.markers.len()), 1);
    let restored = lab.undo("marker-delete", &preview, &applied, "Kumi: undo (kund1)").unwrap();
    assert_eq!(state(|s| s.markers.len()), 2);
    let made = restored["restored"]["ref"].as_str().unwrap().trim_start_matches("r:region:").to_string();
    assert_eq!(state(|s| s.marker_lanes.get(&made).copied()), Some(2), "its ruler lane");
    assert_eq!(state(|s| s.render_matrix.get(&7).cloned()), Some(vec!["{BBBB}".to_string(), "master".to_string()]), "its render matrix");
    // The first answer was lost and the host asks again: the marker made is the one that comes back, and no second is made.
    let entries = state(|s| s.undo_entries);
    let retry = lab.undo("marker-delete", &preview, &applied, "Kumi: undo (kund2)").unwrap();
    assert_eq!(state(|s| s.markers.len()), 2, "no second marker");
    assert_eq!(retry["restored"]["ref"], restored["restored"]["ref"]);
    assert_eq!(retry["undo"]["entry"], false);
    assert_eq!(state(|s| s.undo_entries), entries);
    // A marker (not a region) the same way.
    let (preview, applied) = lab.make("marker-delete", json!({"ref": "r:marker:{MARK-1}"}), "Kumi: Deleted marker “Drop” (kmk02)");
    lab.undo("marker-delete", &preview, &applied, "Kumi: undo (kund3)").unwrap();
    lab.undo("marker-delete", &preview, &applied, "Kumi: undo (kund4)").unwrap();
    assert_eq!(state(|s| s.markers.len()), 2);
}

// ----- a plug-in with a meter among its parameters (N5) -----

#[test]
fn a_meter_shown_as_a_parameter_does_not_make_every_undo_of_an_fx_changed_but_a_tweak_still_does() {
    let lab = Lab::new();
    // A plug-in made now has two meters among its parameters: one called a meter, one that only moves by itself.
    state(|s| s.meter_params = true);
    let (preview, applied) = lab.make("fx-add", json!({"ref": KICK, "name": "ReaEQ"}), "Kumi: Added “ReaEQ” to “Kick” (kn501)");
    assert_eq!(applied["applied"]["signature"]["volatile"], json!([4, 5]), "{}", applied["applied"]["signature"]);
    // A real tweak is seen with the meters about.
    state(|s| s.tracks[1].fx[0].params[0].value = 0.9);
    let (code, _) = lab.undo("fx-add", &preview, &applied, "Kumi: undo (kund1)").unwrap_err();
    assert_eq!(code, "changed");
    state(|s| s.tracks[1].fx[0].params[0].value = 0.5);
    lab.undo("fx-add", &preview, &applied, "Kumi: undo (kund2)").unwrap();
    assert!(kick_fx().is_empty(), "the meters didn't refuse it");
}

#[test]
fn deleting_an_fx_with_a_meter_in_it_is_not_refused_for_the_meter() {
    let lab = Lab::new();
    state(|s| s.meter_params = true);
    lab.make("fx-add", json!({"ref": KICK, "name": "ReaEQ"}), "Kumi: Added “ReaEQ” to “Kick” (kn502)");
    let fx = fx_ref_of(&kick_fx()[0].0);
    let (preview, applied) = lab.make("fx-delete", json!({"ref": fx}), "Kumi: Deleted “ReaEQ” from “Kick” (kn503)");
    assert_eq!(preview["fence"]["signature"]["volatile"], json!([4, 5]));
    assert!(kick_fx().is_empty());
    let dir = fx_folder();
    lab.undo("fx-delete", &preview, &applied, "Kumi: undo (kund1)").unwrap();
    assert_eq!(kick_fx().len(), 1);
    drop(dir);
}

// ----- the escape hatches: a REAPER API function by name, and a Lua chunk (M4) -----

fn call_read(lab: &Lab, params: Value) -> Answer {
    let api = fake::api();
    let reaper = Reaper::new(&api);
    lab.reads.read(&reaper, &params)
}

#[test]
fn a_function_that_only_reads_is_called_by_name_with_refs_in_and_out() {
    let lab = Lab::new();
    let name = call_read(&lab, json!({"kind": "call", "fn": "GetTrackName", "args": [KICK]})).unwrap();
    assert_eq!((&name["result"], &name["out"]["bufOut"]), (&json!(true), &json!("Kick")));
    let named = call_read(&lab, json!({"kind": "call", "fn": "GetTrackName", "args": {"track": KICK}})).unwrap();
    assert_eq!(named["out"]["bufOut"], "Kick", "by name as well as in order");
    let volume = call_read(&lab, json!({"kind": "call", "fn": "GetMediaTrackInfo_Value", "args": [KICK, "D_VOL"]})).unwrap();
    assert!(volume["result"].is_number());
    let count = call_read(&lab, json!({"kind": "call", "fn": "CountTracks"})).unwrap();
    assert_eq!(count["result"], 4, "the project, optional, is the open one");
    let second = call_read(&lab, json!({"kind": "call", "fn": "GetTrack", "args": [0, 1]})).unwrap();
    assert_eq!(second["result"], KICK, "a track REAPER gives back is its ref");
    // A GetSet… function asked to get only reads.
    let text = call_read(&lab, json!({"kind": "call", "fn": "GetSetMediaTrackInfo_String", "args": [KICK, "P_NAME", "", false]})).unwrap();
    assert_eq!(text["out"]["stringNeedBig"], "Kick");
}

#[test]
fn a_call_by_name_that_changes_denied_or_unknown_is_refused_in_a_read() {
    let lab = Lab::new();
    let refused = |params: Value| call_read(&lab, params).unwrap_err();
    let (code, message) = refused(json!({"kind": "call", "fn": "SetMediaTrackInfo_Value", "args": [KICK, "D_VOL", 0.5]}));
    assert_eq!(code, "bad-params");
    assert!(message.contains("api-call"), "{message}");
    let (_, message) = refused(json!({"kind": "call", "fn": "GetSetMediaTrackInfo_String", "args": [KICK, "P_NAME", "x", true]}));
    assert!(message.contains("api-call"), "a GetSet… asked to set changes the project: {message}");
    assert!(refused(json!({"kind": "call", "fn": "Main_OnCommand", "args": [40001, 0]})).1.contains("action method"));
    assert!(refused(json!({"kind": "call", "fn": "GetUserInputs", "args": []})).1.contains("dialog"));
    assert!(refused(json!({"kind": "call", "fn": "GetTrackNam"})).1.contains("perhaps GetTrackName"));
    assert!(refused(json!({"kind": "call", "fn": "GetTrackName", "args": ["r:track:{NOPE}"]})).1.contains("track"));
    assert!(refused(json!({"kind": "call", "fn": "GetTrackName", "args": []})).1.contains("track must be a ref"));
    // A function with a parameter the call can't make: run-lua can call it.
    let uncallable = crate::reaper::generic::function("LICE_Clear").unwrap();
    assert!(crate::reaper::generic::uncallable(uncallable).is_some());
}

#[test]
fn every_function_of_the_header_is_callable_by_name_or_says_why_not() {
    let callable = (0..crate::api::FUNCTION_COUNT).filter(|&i| crate::reaper::generic::uncallable(i).is_none()).count();
    assert!(callable > 600, "only {callable} of {} functions can be called by name", crate::api::FUNCTION_COUNT);
    for index in 0..crate::api::FUNCTION_COUNT {
        if let Some(why) = crate::reaper::generic::uncallable(index) {
            assert!(!why.is_empty(), "{}", crate::reaper::generic::name_of(index));
        }
    }
}

#[test]
fn a_function_that_changes_the_project_is_a_change_reapers_undo_takes_back_and_kumis_refuses() {
    let lab = Lab::new();
    let input = json!({"fn": "SetMediaTrackInfo_Value", "args": [KICK, "D_VOL", 0.5]});
    let (preview, applied) = lab.make("api-call", input, "Kumi: Called SetMediaTrackInfo_Value (kc001)");
    assert_eq!(preview["display"]["title"], "Called SetMediaTrackInfo_Value");
    assert!(preview["display"]["note"].as_str().unwrap().contains("REAPER's own undo"));
    assert_eq!(state(|s| s.tracks[1].volume), 0.5);
    assert_eq!(applied["applied"]["result"], true);
    assert_eq!(applied["undo"]["entry"], true, "one point, named by the host");
    assert_eq!(top().as_deref(), Some("Kumi: Called SetMediaTrackInfo_Value (kc001)"));
    let (code, message) = lab.undo("api-call", &preview, &applied, "Kumi: undo (kc002)").unwrap_err();
    assert_eq!(code, "daw-only");
    assert!(message.contains("REAPER's own undo"), "{message}");
    assert_eq!(state(|s| s.tracks[1].volume), 0.5, "nothing written");
    // Refused at the preview, nothing written: one that only reads, a bad ref, a missing argument, a denied one.
    let preview_of = |input: Value| lab.call("change_preview", json!({"kind": "api-call", "input": input})).unwrap_err();
    assert!(preview_of(json!({"fn": "GetTrackName", "args": [KICK]})).1.contains("only reads"));
    assert!(preview_of(json!({"fn": "SetMediaTrackInfo_Value", "args": ["r:track:{NOPE}", "D_VOL", 0.1]})).1.contains("track"));
    assert!(preview_of(json!({"fn": "SetMediaTrackInfo_Value", "args": [KICK, "D_VOL"]})).1.contains("newvalue"));
    assert!(preview_of(json!({"fn": "Main_OnCommandEx", "args": [40001, 0, 0]})).1.contains("action method"));
    assert_eq!(state(|s| s.tracks[1].volume), 0.5);
}

// A stand-in for REAPER's Lua: what the wrapper would do, from the wrapper's text.
fn token_of(script: &str) -> String {
    let start = script.find("local TOKEN, LIMIT_MS, CHECK = \"").unwrap() + "local TOKEN, LIMIT_MS, CHECK = \"".len();
    script[start..].split('"').next().unwrap().to_string()
}

fn checking(script: &str) -> bool {
    script.lines().find(|l| l.starts_with("local TOKEN, LIMIT_MS, CHECK")).unwrap().ends_with("true")
}

fn answer(script: &str, text: &str) {
    let token = token_of(script);
    state(|s| s.ext_state.insert(("kumi_run".into(), token), text.into()));
}

fn lua_ok(script: &str) {
    if checking(script) {
        return answer(script, r#"{"compiled":true}"#);
    }
    // (what `reaper.SetMediaTrackInfo_Value(track, "D_VOL", 0.25); print("hi"); return 42, track` would do)
    state(|s| {
        s.tracks[1].volume = 0.25;
        s.undo_dirty = true;
    });
    answer(script, r#"{"compiled":true,"ok":true,"result":[42,"r:track:{BBBB}"],"printed":["hi"]}"#);
}

fn lua_syntax(script: &str) {
    answer(script, r#"{"compiled":false,"error":{"message":"unexpected symbol near 'x'","line":2}}"#);
}

fn lua_fails(script: &str) {
    if checking(script) {
        return answer(script, r#"{"compiled":true}"#);
    }
    state(|s| {
        s.tracks[1].volume = 0.125;
        s.undo_dirty = true;
    });
    answer(script, r#"{"compiled":true,"ok":false,"error":{"message":"attempt to call a nil value","line":3},"printed":[]}"#);
}

fn lua_silent(script: &str) {
    if checking(script) {
        answer(script, r#"{"compiled":true}"#);
    }
}

#[test]
fn a_lua_chunk_runs_once_in_one_undo_point_and_only_reapers_undo_takes_it_back() {
    let lab = Lab::new();
    state(|s| s.on_script = Some(lua_ok));
    let code = "-- louder\nlocal track = reaper.GetTrack(0, 1)\nreaper.SetMediaTrackInfo_Value(track, \"D_VOL\", 0.25)\nprint(\"hi\")\nreturn 42, track";
    let (preview, applied) = lab.make("run-lua", json!({"code": code}), "Kumi: Ran Lua (kl001)");
    assert_eq!(preview["display"]["title"], "Ran Lua: local track = reaper.GetTrack(0, 1)");
    assert_eq!(state(|s| s.tracks[1].volume), 0.25, "run once: the preview only compiled it");
    assert_eq!(applied["applied"]["result"], json!([42, KICK]));
    assert_eq!(applied["applied"]["printed"], json!(["hi"]));
    assert_eq!((&applied["undo"]["entry"], top().as_deref()), (&json!(true), Some("Kumi: Ran Lua (kl001)")));
    assert!(state(|s| s.scripts.is_empty() && s.ext_state.is_empty()), "the script is taken away, and its answer");
    let (code, message) = lab.undo("run-lua", &preview, &applied, "Kumi: undo (kl002)").unwrap_err();
    assert_eq!(code, "daw-only");
    assert!(message.contains("Lua chunk"), "{message}");
}

#[test]
fn a_lua_chunk_that_doesnt_compile_is_refused_at_the_preview_and_one_that_fails_says_where_and_what_stays() {
    let lab = Lab::new();
    state(|s| s.on_script = Some(lua_syntax));
    let (code, message) = lab.call("change_preview", json!({"kind": "run-lua", "input": {"code": "local x =\nx x"}})).unwrap_err();
    assert_eq!(code, "bad-params");
    assert!(message.contains("line 2") && message.contains("unexpected symbol"), "{message}");
    state(|s| s.on_script = Some(lua_fails));
    let (_, applied) = lab.make(
        "run-lua",
        json!({"code": "reaper.SetMediaTrackInfo_Value(reaper.GetTrack(0, 1), \"D_VOL\", 0.125)\nlocal f\nf()"}),
        "Kumi: Ran Lua (kl003)",
    );
    assert_eq!(applied["applied"]["failed"]["line"], 3);
    let note = applied["display"]["note"].as_str().unwrap();
    assert!(note.contains("line 3") && note.contains("stays"), "{note}");
    assert_eq!(state(|s| s.tracks[1].volume), 0.125);
    assert_eq!(top().as_deref(), Some("Kumi: Ran Lua (kl003)"), "what it did before the error is one point REAPER's undo takes back");
}

#[test]
fn a_lua_chunk_that_gives_no_answer_is_uncertain_and_limits_are_checked() {
    let lab = Lab::new();
    state(|s| s.on_script = Some(lua_silent));
    let preview = lab.call("change_preview", json!({"kind": "run-lua", "input": {"code": "return 1"}})).unwrap();
    let (code, _) = lab
        .call(
            "change_apply",
            json!({"kind": "run-lua", "input": {"code": "return 1"}, "fence": preview["fence"], "desc": "Kumi: Ran Lua (kl004)"}),
        )
        .unwrap_err();
    assert_eq!(code, "uncertain");
    let refused = |input: Value| lab.call("change_preview", json!({"kind": "run-lua", "input": input})).unwrap_err().1;
    assert!(refused(json!({"code": "  "})).contains("code"));
    assert!(refused(json!({"code": "return 1", "timeoutMs": 60_000})).contains("timeoutMs"));
}

/// The wrapper itself, run by LuaJIT where it is installed, against a stand-in `reaper` table.
#[test]
fn the_lua_wrapper_answers_results_prints_errors_and_the_time_limit() {
    if std::process::Command::new("luajit").arg("-v").output().is_err() {
        eprintln!("luajit isn't installed: the wrapper's own test is skipped");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let run = |code: &str, limit: u64, check: bool| -> Value {
        // (LuaJIT calls no hook inside code it compiled: REAPER's Lua compiles nothing, so neither does this)
        let stub = "if jit then jit.off() end\nreaper = { SetExtState = function(_, _, v) io.write(v) end, time_precise = os.clock, \
                    ValidatePtr2 = function() return false end, GetMasterTrack = function() end, ShowConsoleMsg = function() end }\n";
        let path = dir.path().join("w.lua");
        std::fs::write(&path, format!("{stub}{}", super::lua::wrapper(code, "t1", limit, check))).unwrap();
        let out = std::process::Command::new("luajit").arg(&path).output().unwrap();
        serde_json::from_slice(&out.stdout).unwrap_or_else(|e| panic!("{e}: {}", String::from_utf8_lossy(&out.stdout)))
    };
    assert_eq!(run("return 1", 1000, true), json!({"compiled": true}));
    let ran = run("print('a', 2)\nreaper.ShowConsoleMsg('b')\nreturn {x = 1.5, y = {1, 2, 'q\"'}}, nil, true", 1000, false);
    assert_eq!(ran["ok"], true);
    assert_eq!(ran["printed"], json!(["a\t2", "b"]));
    assert_eq!(ran["result"], json!([{"x": 1.5, "y": [1, 2, "q\""]}, null, true]));
    let bad = run("local x =\nx x", 1000, false);
    assert_eq!((&bad["compiled"], &bad["error"]["line"]), (&json!(false), &json!(2)));
    let failed = run("local a = 1\nerror('boom')", 1000, false);
    assert_eq!((&failed["ok"], &failed["error"]["line"], &failed["error"]["message"]), (&json!(false), &json!(2), &json!("boom")));
    let looped = run("while true do end", 50, false);
    assert!(looped["error"]["message"].as_str().unwrap().contains("stopped after 50 ms"), "{looped}");
    // A chunk that closes the wrapper's own long bracket is still one string.
    assert_eq!(run("return ']]'", 1000, false)["result"], "]]");
}
