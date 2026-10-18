use std::cell::RefCell;

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
        if params.get("epoch").is_none() && !matches!(method, "transport_stop" | "undo_step_begin" | "undo_step_end") {
            params["epoch"] = self.reads.epoch().into();
        }
        match method {
            "change_preview" => self.changes.preview(&reaper, &self.reads, &params),
            "change_apply" => self.changes.apply(&reaper, &self.reads, &params),
            "change_restore" => self.changes.restore(&reaper, &self.reads, &params),
            "transport_stop" => self.changes.transport_stop(&reaper),
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
        ("track-rename", json!({"ref": KICK, "name": "Kick 1"}), "Renamed track Kick to Kick 1"),
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
    assert_eq!(preview["display"]["title"], "Deleted track Reverb");
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
    let (preview, applied) = lab.make("track-add", json!({"name": "Pad", "color": "#102030", "index": 1}), "Kumi: Added track Pad (kadd1)");
    assert_eq!(preview["display"]["title"], "Added track Pad");
    assert_eq!(preview["target"], json!({"ref": null, "kind": "track", "index": 1}));
    assert_eq!(names(), ["Drums", "Pad", "Kick", "Snare", "Reverb"]);
    assert_eq!(state(|s| s.tracks[1].color), 0x10 | 0x20 << 8 | 0x30 << 16 | 0x100_0000);
    let created = applied["created"]["ref"].as_str().unwrap().to_string();
    assert!(created.starts_with("r:track:{NEW-"), "{created}");
    assert_eq!((applied["applied"]["index"].clone(), applied["applied"]["name"].clone()), (json!(1), json!("Pad")));
    // The sends that named a track by place still reach it: Drums sends to Reverb, now one further on.
    assert_eq!(state(|s| s.tracks[0].sends[0].dest), 4);
    let restored = lab.undo("track-add", &preview, &applied, "Kumi: undo Added track Pad (kund1)");
    // (undo of an add is told what was created)
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
    assert_eq!(preview["display"]["title"], "Added folder Subgroup");
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
    let preview = lab.call("change_preview", json!({"kind": "track-add", "input": {"name": "Pad", "index": 1}})).unwrap();
    state(|s| s.tracks.insert(1, crate::reaper::fake::FakeTrack::new("{OTHER}", "Other")));
    let (code, _) = lab
        .call(
            "change_apply",
            json!({"kind": "track-add", "input": {"name": "Pad", "index": 1}, "fence": preview["fence"], "desc": "Kumi: x (k1111)"}),
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
    let (preview, applied) = lab.make("track-move", json!({"ref": KICK, "index": 3}), "Kumi: Moved track Kick to position 4 (kmov1)");
    assert_eq!(preview["display"]["title"], "Moved track Kick to position 4");
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
    let preview = lab.call("change_preview", json!({"kind": "track-move", "input": {"ref": KICK, "index": 2}})).unwrap();
    state(|s| s.tracks.swap(2, 3));
    let (code, _) = lab
        .call(
            "change_apply",
            json!({"kind": "track-move", "input": {"ref": KICK, "index": 2}, "fence": preview["fence"], "desc": "Kumi: x (k1111)"}),
        )
        .unwrap_err();
    assert_eq!(code, "changed");
    state(|s| s.tracks.swap(2, 3));
    let (preview, applied) = lab.make("track-move", json!({"ref": KICK, "index": 2}), "Kumi: Moved (kmov1)");
    assert_eq!(names(), ["Drums", "Snare", "Kick", "Reverb"]);
    // The producer moved another track next to it since.
    state(|s| s.tracks.swap(0, 1));
    let (code, message) = lab.undo("track-move", &preview, &applied, "Kumi: undo (kund1)").unwrap_err();
    assert_eq!(code, "changed");
    assert!(message.contains("moved or changed in REAPER since"), "{message}");
    assert_eq!(names(), ["Snare", "Drums", "Kick", "Reverb"]);
    // Index out of range, the master, and no ref are refused up front.
    for input in [json!({"ref": KICK, "index": 4}), json!({"ref": KICK}), json!({"ref": "r:track:master", "index": 0}), json!({"index": 0})]
    {
        assert_eq!(lab.call("change_preview", json!({"kind": "track-move", "input": input})).unwrap_err().0, "bad-params", "{input}");
    }
}

#[test]
fn moving_a_track_to_where_it_is_changes_nothing() {
    let lab = Lab::new();
    let (preview, applied) = lab.make("track-move", json!({"ref": KICK, "index": 1}), "Kumi: Moved (kmov1)");
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
        ("send-volume", json!({"ref": DRUMS_TO_REVERB, "volumeDb": -12.0}), "Drums → Reverb −12 dB"),
        ("send-pan", json!({"ref": DRUMS_TO_REVERB, "pan": 0.5}), "Drums → Reverb pan 50% right"),
        ("send-mute", json!({"ref": DRUMS_TO_REVERB, "mute": true}), "Drums → Reverb send muted"),
        ("send-mode", json!({"ref": DRUMS_TO_REVERB, "mode": "pre-fx"}), "Drums → Reverb send pre-fx"),
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
    assert_eq!(preview["display"]["title"], "Added send Kick → Reverb");
    assert_eq!(applied["created"], json!({"ref": "r:send:{BBBB}:0:{DDDD}", "kind": "send"}));
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
    assert_eq!(applied["created"]["ref"], "r:send:{AAAA}:0:{DDDD}:2of2");
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
    assert_eq!(preview["display"]["title"], "Removed send Drums → Reverb");
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
    assert_eq!(preview["display"]["title"], "Marker Intro at bar 3");
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
    assert_eq!(preview["display"]["title"], "Region Chorus 2 from bar 5 to bar 9");
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
    assert_eq!(preview["display"]["title"], "Moved marker Drop to bar 11");
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
    assert_eq!(preview["display"]["title"], "Renamed marker Drop to Breakdown");
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
    assert_eq!(preview["display"]["title"], "Deleted region Verse");
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
