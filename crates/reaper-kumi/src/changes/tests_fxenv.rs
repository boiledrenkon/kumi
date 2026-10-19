//! m4-r-fxenv: the FX and envelope kinds of M4, against the fake REAPER. Each kind is applied, undone by Kumi, refused when
//! the producer changed what it touched (and put back anyway with discard), refused `gone` when the object went, a retry
//! of its undo writes nothing, and an apply is refused when what the preview fenced moved in between.

use std::cell::RefCell;

use serde_json::{json, Value};

use super::*;
use crate::reaper::fake::{self, Display, FakeAutoItem, FakeEnvelope, FakeFx, FakeItem, FakeParam, FakePoint, FakePreset, FakeTake};

#[derive(Default)]
struct Events(RefCell<Vec<(String, Value)>>);

impl Outbox for Events {
    fn event(&self, name: &str, data: Value) {
        self.0.borrow_mut().push((name.to_string(), data));
    }
    fn heartbeat(&self) {}
}

struct Lab {
    changes: Changes,
    reads: Reads,
    events: Events,
    serial: std::cell::Cell<u32>,
}

impl Lab {
    fn new() -> Lab {
        fake::lab_project();
        let lab = Lab { changes: Changes::default(), reads: Reads::default(), events: Events::default(), serial: Default::default() };
        lab.tick();
        lab
    }

    fn call(&self, method: &str, mut params: Value) -> Answer {
        let api = fake::api();
        let reaper = Reaper::new(&api);
        if params.get("epoch").is_none() {
            params["epoch"] = self.reads.epoch().into();
        }
        match method {
            "change_preview" => self.changes.preview(&reaper, &self.reads, &params),
            "change_apply" => self.changes.apply(&reaper, &self.reads, &params),
            "change_restore" => self.changes.restore(&reaper, &self.reads, &params),
            "read" => self.reads.read(&reaper, &params),
            other => panic!("{other}"),
        }
    }

    fn tick(&self) {
        let api = fake::api();
        self.changes.tick(&Reaper::new(&api), &self.reads, &self.events);
    }

    fn desc(&self) -> String {
        self.serial.set(self.serial.get() + 1);
        format!("Kumi: change (k{:04})", self.serial.get())
    }

    fn preview(&self, kind: &str, input: Value) -> Answer {
        self.call("change_preview", json!({"kind": kind, "input": input}))
    }

    fn apply(&self, kind: &str, input: &Value, fence: &Value) -> Answer {
        let desc = self.desc();
        self.call("change_apply", json!({"kind": kind, "input": input, "fence": fence, "desc": desc}))
    }

    /// A change as Kumi's host makes it: preview, then apply with the preview's fence and a description.
    fn make(&self, kind: &str, input: Value) -> (Value, Value) {
        let preview = self.preview(kind, input.clone()).unwrap_or_else(|e| panic!("{kind} preview: {e:?}"));
        let applied = self.apply(kind, &input, &preview["fence"]).unwrap_or_else(|e| panic!("{kind} apply: {e:?}"));
        (preview, applied)
    }

    fn restore_params(&self, kind: &str, preview: &Value, applied: &Value, discard: bool) -> Value {
        let mut params = json!({"kind": kind, "target": preview["target"], "prior": preview["prior"], "applied": applied["applied"], "desc": self.desc()});
        for key in ["material", "created"] {
            if let Some(value) = applied.get(key) {
                params[key] = value.clone();
            }
        }
        if discard {
            params["discard"] = true.into();
        }
        params
    }

    fn undo(&self, kind: &str, preview: &Value, applied: &Value) -> Answer {
        let params = self.restore_params(kind, preview, applied, false);
        self.call("change_restore", params)
    }

    fn discard(&self, kind: &str, preview: &Value, applied: &Value) -> Answer {
        let params = self.restore_params(kind, preview, applied, true);
        self.call("change_restore", params)
    }

    fn read(&self, params: Value) -> Value {
        self.call("read", params).unwrap_or_else(|e| panic!("read: {e:?}"))
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

const DRUMS: &str = "r:track:{AAAA}";
const KICK: &str = "r:track:{BBBB}";

fn fx_ref(guid: &str) -> String {
    format!("r:fx:{guid}")
}

/// The three controls REAPER gives every FX after its own: bypass, wet, delta.
fn wrapper() -> Vec<FakeParam> {
    vec![
        FakeParam::steps("Bypass", 0.0, &["normal", "bypassed"]),
        FakeParam::linear("Wet", 1.0, 0.0, 100.0, 0, "%"),
        FakeParam::steps("Delta", 0.0, &["normal", "enabled"]),
    ]
}

/// A plug-in with a gain in dB, a mode and REAPER's wrapper controls.
fn plugin(guid: &str, name: &str) -> FakeFx {
    let mut fx = FakeFx::new(guid, name);
    fx.params = vec![FakeParam::linear("Gain", 0.5, -12.0, 12.0, 1, "dB"), FakeParam::steps("Mode", 0.0, &["Low", "Mid", "High"])];
    fx.params.extend(wrapper());
    fx
}

/// ReaEQ: two bands (frequency in Hz, gain in dB, bandwidth in octaves), its band configuration and a state chunk.
fn reaeq(guid: &str) -> FakeFx {
    let mut fx = FakeFx::new(guid, "VST: ReaEQ (Cockos)");
    let band = |n: usize, t: i32, i: i32| {
        let mut freq = FakeParam::linear(&format!("Freq-Band {n}"), 0.25, 20.0, 20020.0, 1, "Hz");
        freq.eq = Some((t, i, 0));
        let mut gain = FakeParam::linear(&format!("Gain-Band {n}"), 0.5, -24.0, 24.0, 1, "dB");
        gain.eq = Some((t, i, 1));
        let mut bw = FakeParam::linear(&format!("BW-Band {n}"), 0.5, 0.0, 4.0, 2, "");
        bw.eq = Some((t, i, 2));
        [freq, gain, bw]
    };
    fx.params = band(1, 1, 0).into_iter().chain(band(2, 2, 0)).collect();
    fx.params.extend(wrapper());
    for (key, value) in [("BANDTYPE0", "0"), ("BANDENABLED0", "1"), ("BANDTYPE1", "8"), ("BANDENABLED1", "1")] {
        fx.config.insert(key.into(), value.into());
    }
    fx.chunk = Some(String::new());
    fx
}

/// JS: utility/volume, whose slider runs from -150 to 150 dB.
fn js_volume(guid: &str) -> FakeFx {
    let mut fx = FakeFx::new(guid, "JS: Volume Adjustment");
    let mut adjust = FakeParam::linear("Adjustment (dB)", 0.5, -150.0, 150.0, 1, "");
    adjust.min = -150.0;
    adjust.max = 150.0;
    fx.params = vec![adjust];
    fx.params.extend(wrapper());
    fx
}

/// Kick with an FX in its main chain, one in its input chain and a take with an FX.
fn chains_lab() -> Lab {
    let lab = Lab::new();
    state(|s| {
        s.tracks[1].fx = vec![plugin("{K-MAIN}", "VST: ReaComp (Cockos)")];
        s.tracks[1].input_fx = vec![plugin("{K-IN}", "VST: ReaGate (Cockos)")];
        s.tracks[1].items[0].takes[0].fx = vec![plugin("{T-FX}", "VST: ReaDelay (Cockos)")];
        s.state_count += 1;
    });
    lab
}

const TAKE: &str = "r:take:{TAKE-K1}";

fn guids(chain: &[FakeFx]) -> Vec<String> {
    chain.iter().map(|f| f.guid.clone()).collect()
}

// ----- FX in every chain -----

#[test]
fn an_fx_is_added_to_a_tracks_input_chain_a_take_and_a_container_and_each_undo_takes_that_one_off() {
    let lab = chains_lab();
    let (preview, applied) = lab.make("fx-add", json!({"ref": KICK, "name": "ReaEQ", "input": true}));
    assert_eq!(preview["display"]["title"], "Added “ReaEQ” to “Kick”'s input FX");
    let made = applied["created"]["ref"].as_str().unwrap().trim_start_matches("r:fx:").to_string();
    assert_eq!(state(|s| guids(&s.tracks[1].input_fx)), ["{K-IN}".to_string(), made.clone()]);
    assert_eq!(applied["undo"]["entry"], true);
    lab.undo("fx-add", &preview, &applied).unwrap();
    assert_eq!(state(|s| guids(&s.tracks[1].input_fx)), ["{K-IN}"]);
    // A take, at the front; an item stands for its active take.
    let (preview, applied) = lab.make("fx-add", json!({"ref": "r:item:{ITEM-K1}", "name": "ReaEQ", "index": 0}));
    assert_eq!(preview["display"]["title"], "Added “ReaEQ” to take “kick.wav”");
    assert_eq!(state(|s| s.tracks[1].items[0].takes[0].fx.len()), 2);
    assert_eq!(state(|s| s.tracks[1].items[0].takes[0].fx[1].guid.clone()), "{T-FX}");
    lab.undo("fx-add", &preview, &applied).unwrap();
    assert_eq!(state(|s| guids(&s.tracks[1].items[0].takes[0].fx)), ["{T-FX}"]);
    // A container on Drums (ReaComp and a container with ReaDelay inside), at its second place, and in the nested one.
    let (preview, applied) = lab.make("fx-add", json!({"ref": fx_ref("{FX-C}"), "name": "ReaEQ", "index": 1}));
    assert_eq!(preview["display"]["title"], "Added “ReaEQ” to “Container” on “Drums”");
    let inside = state(|s| guids(s.tracks[0].fx[1].children.as_ref().unwrap()));
    assert_eq!(inside.len(), 3);
    assert_eq!((inside[0].as_str(), inside[2].as_str()), ("{FX-COMP}", "{FX-C2}"));
    lab.undo("fx-add", &preview, &applied).unwrap();
    assert_eq!(state(|s| guids(s.tracks[0].fx[1].children.as_ref().unwrap())), ["{FX-COMP}", "{FX-C2}"]);
    let (preview, applied) = lab.make("fx-add", json!({"ref": fx_ref("{FX-C2}"), "name": "ReaEQ"}));
    let nested = state(|s| guids(s.tracks[0].fx[1].children.as_ref().unwrap()[1].children.as_ref().unwrap()));
    assert_eq!(nested.len(), 2);
    assert_eq!(nested[0], "{FX-DLY}");
    lab.undo("fx-add", &preview, &applied).unwrap();
    // A take has no input chain; a plain FX is no container.
    assert_eq!(lab.preview("fx-add", json!({"ref": TAKE, "name": "ReaEQ", "input": true})).unwrap_err().0, "bad-params");
    assert_eq!(lab.preview("fx-add", json!({"ref": fx_ref("{FX-EQ}"), "name": "ReaEQ"})).unwrap_err().0, "bad-params");
    assert_eq!(state(|s| s.copies_into_containers), 0);
}

#[test]
fn fx_in_a_take_an_input_chain_and_a_container_are_bypassed_set_moved_and_deleted_and_put_back_where_they_were() {
    let lab = chains_lab();
    // Bypass and parameter of a take's FX and an input FX.
    let (p, a) = lab.make("fx-bypass", json!({"ref": fx_ref("{T-FX}"), "bypass": true}));
    assert_eq!(p["display"]["title"], "Bypassed “ReaDelay” on take “kick.wav”");
    assert!(!state(|s| s.tracks[1].items[0].takes[0].fx[0].enabled));
    lab.undo("fx-bypass", &p, &a).unwrap();
    assert!(state(|s| s.tracks[1].items[0].takes[0].fx[0].enabled));
    let (p, a) = lab.make("fx-param", json!({"ref": fx_ref("{K-IN}"), "param": "Gain", "value": "-6 dB"}));
    assert_eq!(p["display"]["title"], "ReaGate Gain -6.0 dB on “Kick”'s input FX");
    assert!((state(|s| s.tracks[1].input_fx[0].params[0].value) - 0.25).abs() < 0.01);
    lab.undo("fx-param", &p, &a).unwrap();
    assert_eq!(state(|s| s.tracks[1].input_fx[0].params[0].value), 0.5);
    // A container's child is moved within its container and put back.
    let (p, a) = lab.make("fx-move", json!({"ref": fx_ref("{FX-COMP}"), "index": 1}));
    assert_eq!(p["display"]["title"], "Moved “ReaComp” to place 2 in “Container” on “Drums”");
    assert_eq!(state(|s| guids(s.tracks[0].fx[1].children.as_ref().unwrap())), ["{FX-C2}", "{FX-COMP}"]);
    lab.undo("fx-move", &p, &a).unwrap();
    assert_eq!(state(|s| guids(s.tracks[0].fx[1].children.as_ref().unwrap())), ["{FX-COMP}", "{FX-C2}"]);
    // A take's FX and an input FX moved.
    lab.make("fx-add", json!({"ref": TAKE, "name": "ReaEQ"}));
    let (p, a) = lab.make("fx-move", json!({"ref": fx_ref("{T-FX}"), "index": 1}));
    assert_eq!(state(|s| s.tracks[1].items[0].takes[0].fx[1].guid.clone()), "{T-FX}");
    lab.undo("fx-move", &p, &a).unwrap();
    assert_eq!(state(|s| s.tracks[1].items[0].takes[0].fx[0].guid.clone()), "{T-FX}");
    // Deleted from each chain, and put back with the same id where it was.
    let dir = resource();
    for (guid, chain) in [("{K-IN}", "input"), ("{T-FX}", "take"), ("{FX-COMP}", "container"), ("{FX-DLY}", "nested")] {
        let (p, a) = lab.make("fx-delete", json!({"ref": fx_ref(guid)}));
        assert!(!all_guids().contains(&guid.to_string()), "{chain}: deleted");
        let restored = lab.undo("fx-delete", &p, &a).unwrap();
        assert_eq!(restored["restored"]["ref"], fx_ref(guid), "{chain}");
        assert!(all_guids().contains(&guid.to_string()), "{chain}: back");
    }
    assert_eq!(state(|s| guids(&s.tracks[1].input_fx)), ["{K-IN}"]);
    assert_eq!(state(|s| guids(s.tracks[0].fx[1].children.as_ref().unwrap())), ["{FX-COMP}", "{FX-C2}"]);
    assert_eq!(state(|s| s.tracks[0].fx[1].children.as_ref().unwrap()[1].children.as_ref().unwrap()[0].guid.clone()), "{FX-DLY}");
    assert_eq!(chain_files(&dir), 0, "no FX chain file is left behind");
    assert_eq!(state(|s| s.copies_into_containers), 0);
}

fn all_guids() -> Vec<String> {
    fn walk(chain: &[FakeFx], out: &mut Vec<String>) {
        for fx in chain {
            out.push(fx.guid.clone());
            walk(fx.children.as_deref().unwrap_or_default(), out);
        }
    }
    state(|s| {
        let mut out = Vec::new();
        for t in s.tracks.iter() {
            walk(&t.fx, &mut out);
            walk(&t.input_fx, &mut out);
            for item in &t.items {
                for take in &item.takes {
                    walk(&take.fx, &mut out);
                }
            }
        }
        out
    })
}

/// A resource folder the fake keeps FX chain files in.
fn resource() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    state(|s| s.resource = dir.path().to_string_lossy().into_owned());
    dir
}

fn chain_files(dir: &tempfile::TempDir) -> usize {
    std::fs::read_dir(dir.path().join("FXChains"))
        .map_or(0, |d| d.filter_map(Result::ok).filter(|e| e.file_name().to_string_lossy().starts_with("kumi-")).count())
}

#[test]
fn a_deleted_container_comes_back_with_everything_inside_and_its_automation() {
    let lab = Lab::new();
    let _dir = resource();
    state(|s| s.tracks[0].envelopes.push(FakeEnvelope::for_param("Gain / ReaComp", "{FX-COMP}", 0, 3)));
    let (preview, applied) = lab.make("fx-delete", json!({"ref": fx_ref("{FX-C}")}));
    assert!(preview["display"]["note"].as_str().unwrap().contains("2 FX inside the container"), "{}", preview["display"]);
    assert!(preview["display"]["note"].as_str().unwrap().contains("as new envelopes"), "{}", preview["display"]);
    assert_eq!(state(|s| guids(&s.tracks[0].fx)), ["{FX-EQ}"]);
    assert!(state(|s| s.tracks[0].envelopes.iter().all(|e| e.fx_param.as_ref().is_none_or(|(g, _)| g != "{FX-COMP}"))));
    let restored = lab.undo("fx-delete", &preview, &applied).unwrap();
    assert!(restored["display"]["note"].as_str().unwrap().contains("new envelopes"));
    assert_eq!(state(|s| guids(&s.tracks[0].fx)), ["{FX-EQ}", "{FX-C}"]);
    assert_eq!(state(|s| guids(s.tracks[0].fx[1].children.as_ref().unwrap())), ["{FX-COMP}", "{FX-C2}"]);
    // Its child's automation came back, points and all, as a new envelope.
    let back =
        state(|s| s.tracks[0].envelopes.iter().find(|e| e.fx_param.as_ref().is_some_and(|(g, _)| g == "{FX-COMP}")).cloned()).unwrap();
    assert_eq!(fake::points_of(&back).len(), 3);
}

#[test]
fn a_deleted_fx_goes_back_after_the_fx_it_followed_wherever_that_one_is_now() {
    let lab = chains_lab();
    let _dir = resource();
    lab.make("fx-add", json!({"ref": KICK, "name": "ReaEQ"}));
    lab.make("fx-add", json!({"ref": KICK, "name": "ReaDelay"}));
    let chain = state(|s| guids(&s.tracks[1].fx));
    let (preview, applied) = lab.make("fx-delete", json!({"ref": fx_ref(&chain[1])}));
    // The producer puts two FX in front: the deleted FX's place by number would now be the wrong one.
    state(|s| {
        s.tracks[1].fx.insert(0, plugin("{P-1}", "VST: ReaVerb (Cockos)"));
        s.tracks[1].fx.insert(0, plugin("{P-2}", "VST: ReaVerb (Cockos)"));
        s.state_count += 1;
    });
    lab.undo("fx-delete", &preview, &applied).unwrap();
    assert_eq!(state(|s| guids(&s.tracks[1].fx)), ["{P-2}", "{P-1}", &chain[0], &chain[1], &chain[2]].map(String::from));
}

#[test]
fn a_take_fxs_pins_are_mapped_and_put_back() {
    let lab = chains_lab();
    let (preview, applied) = lab.make("fx-pins", json!({"ref": fx_ref("{T-FX}"), "pins": [{"pin": 1, "channels": [3]}]}));
    assert_eq!(preview["display"]["title"], "Pins of “ReaDelay” on take “kick.wav”: in 1 ← 3");
    assert_eq!(state(|s| s.tracks[1].items[0].takes[0].fx[0].pins.get(&(false, 0)).copied()), Some(4));
    lab.undo("fx-pins", &preview, &applied).unwrap();
    assert_eq!(state(|s| s.tracks[1].items[0].takes[0].fx[0].pins.get(&(false, 0)).copied()), Some(1));
}

// ----- offline, wet and delta -----

#[test]
fn an_fx_taken_offline_is_put_back_online_by_undo_and_refused_when_the_producer_switched_it() {
    let lab = chains_lab();
    let input = json!({"ref": fx_ref("{K-MAIN}"), "offline": true});
    let (preview, applied) = lab.make("fx-offline", input.clone());
    assert_eq!(preview["display"]["title"], "Took “ReaComp” on “Kick” offline");
    assert!(state(|s| s.tracks[1].fx[0].offline));
    assert_eq!(applied["undo"]["entry"], true);
    // The producer put it online and offline again: the same state, so the undo goes ahead.
    lab.undo("fx-offline", &preview, &applied).unwrap();
    assert!(!state(|s| s.tracks[1].fx[0].offline));
    // A retry writes nothing.
    let again = lab.undo("fx-offline", &preview, &applied).unwrap();
    assert_eq!(again["undo"]["entry"], false);
    // Producer put it back online between the apply and the undo: back already; offline but changed by them elsewhere
    // can't be told apart, so the fence is the offline state itself.
    let (preview, applied) = lab.make("fx-offline", input.clone());
    state(|s| s.tracks[1].fx[0].offline = false);
    let undone = lab.undo("fx-offline", &preview, &applied).unwrap();
    assert!(undone["display"]["note"].as_str().unwrap_or_default().contains("back already"), "{undone}");
    // Fence: switched between the preview and the apply.
    let preview = lab.preview("fx-offline", input.clone()).unwrap();
    state(|s| s.tracks[1].fx[0].offline = true);
    assert_eq!(lab.apply("fx-offline", &input, &preview["fence"]).unwrap_err().0, "changed");
    // Gone.
    state(|s| s.tracks[1].fx[0].offline = false);
    let (preview, applied) = lab.make("fx-offline", input);
    state(|s| s.tracks[1].fx.clear());
    assert_eq!(lab.undo("fx-offline", &preview, &applied).unwrap_err().0, "gone");
}

#[test]
fn wet_and_delta_are_set_together_or_alone_and_undo_puts_back_only_what_it_touched() {
    let lab = chains_lab();
    let input = json!({"ref": fx_ref("{K-MAIN}"), "wet": 0.25, "delta": true});
    let (preview, applied) = lab.make("fx-wet", input.clone());
    assert_eq!(preview["display"]["title"], "“ReaComp” on “Kick” wet 25%, delta on");
    let (wet, delta) = state(|s| (s.tracks[1].fx[0].params[3].value, s.tracks[1].fx[0].params[4].value));
    assert_eq!((wet, delta), (0.25, 1.0));
    assert_eq!(applied["applied"]["wetDisplay"], "25 %");
    // The producer moves the wet: the undo is refused, and discard puts both back.
    state(|s| s.tracks[1].fx[0].params[3].value = 0.6);
    let (code, message) = lab.undo("fx-wet", &preview, &applied).unwrap_err();
    assert_eq!(code, "changed", "{message}");
    lab.discard("fx-wet", &preview, &applied).unwrap();
    assert_eq!(state(|s| (s.tracks[1].fx[0].params[3].value, s.tracks[1].fx[0].params[4].value)), (1.0, 0.0));
    // Delta alone leaves the wet alone.
    let (preview, applied) = lab.make("fx-wet", json!({"ref": fx_ref("{K-MAIN}"), "delta": true}));
    assert!(preview["prior"].get("wet").is_none());
    state(|s| s.tracks[1].fx[0].params[3].value = 0.7);
    lab.undo("fx-wet", &preview, &applied).unwrap();
    assert_eq!(state(|s| (s.tracks[1].fx[0].params[3].value, s.tracks[1].fx[0].params[4].value)), (0.7, 0.0));
    // Fence and bad requests.
    let preview = lab.preview("fx-wet", input.clone()).unwrap();
    state(|s| s.tracks[1].fx[0].params[4].value = 1.0);
    assert_eq!(lab.apply("fx-wet", &input, &preview["fence"]).unwrap_err().0, "changed");
    for bad in [
        json!({"ref": fx_ref("{K-MAIN}")}),
        json!({"ref": fx_ref("{K-MAIN}"), "wet": 1.5}),
        json!({"ref": fx_ref("{K-MAIN}"), "delta": "on"}),
    ] {
        assert_eq!(lab.preview("fx-wet", bad.clone()).unwrap_err().0, "bad-params", "{bad}");
    }
    // An FX REAPER gives no wet control (the fake's ReaVerb from the lab project has no parameters at all).
    assert_eq!(lab.preview("fx-wet", json!({"ref": fx_ref("{FX-VERB}"), "wet": 0.5})).unwrap_err().0, "failed");
}

// ----- parameters in units -----

#[test]
fn reaeq_takes_hz_db_and_octaves_and_a_jsfx_slider_its_own_value_and_other_plug_ins_are_told_to_use_text() {
    let lab = Lab::new();
    state(|s| {
        s.tracks[1].fx = vec![reaeq("{EQ}"), js_volume("{JS}"), plugin("{PLUG}", "VST: Pro-Q 3 (FabFilter)")];
        s.state_count += 1;
    });
    let (preview, applied) = lab.make("fx-param", json!({"ref": "r:param:{EQ}:3", "units": 1000}));
    let freq = state(|s| s.tracks[1].fx[0].params[3].value);
    assert!(((20.0 + freq * 20000.0) - 1000.0).abs() < 0.5, "{freq}");
    assert_eq!(applied["applied"]["display"], "1000.0 Hz");
    assert_eq!(preview["display"]["title"], "ReaEQ Freq-Band 2 1000.0 Hz on “Kick”");
    lab.undo("fx-param", &preview, &applied).unwrap();
    assert_eq!(state(|s| s.tracks[1].fx[0].params[3].value), 0.25);
    lab.make("fx-param", json!({"ref": fx_ref("{EQ}"), "param": "Gain-Band 1", "units": -4.5}));
    let gain = state(|s| s.tracks[1].fx[0].params[1].value);
    assert!((gain - (19.5 / 48.0)).abs() < 1e-3, "{gain}");
    // A JSFX slider in its own units: exact.
    let (preview, applied) = lab.make("fx-param", json!({"ref": "r:param:{JS}:0", "units": -6}));
    let raw = state(|s| s.tracks[1].fx[1].params[0].value * 300.0 - 150.0);
    assert!((raw + 6.0).abs() < 1e-9, "{raw}");
    lab.undo("fx-param", &preview, &applied).unwrap();
    assert_eq!(state(|s| s.tracks[1].fx[1].params[0].value), 0.5);
    for (input, code) in [
        (json!({"ref": "r:param:{JS}:0", "units": 200}), "failed"),
        (json!({"ref": "r:param:{EQ}:3", "units": 99999}), "failed"),
        (json!({"ref": "r:param:{PLUG}:0", "units": 3}), "bad-params"),
        (json!({"ref": "r:param:{JS}:0", "units": "-6"}), "bad-params"),
        (json!({"ref": "r:param:{JS}:0", "units": 1, "normalized": 0.5}), "bad-params"),
    ] {
        assert_eq!(lab.preview("fx-param", input.clone()).unwrap_err().0, code, "{input}");
    }
}

#[test]
fn a_reaeq_bands_type_and_switch_change_and_undo_puts_them_back() {
    let lab = Lab::new();
    state(|s| {
        s.tracks[1].fx = vec![reaeq("{EQ}"), plugin("{PLUG}", "VST: ReaComp (Cockos)")];
        s.state_count += 1;
    });
    let input = json!({"ref": fx_ref("{EQ}"), "band": 1, "type": "notch", "enabled": false});
    let (preview, applied) = lab.make("fx-eq-band", input.clone());
    assert_eq!(preview["display"]["title"], "“ReaEQ” on “Kick” band 2 a notch, off");
    assert_eq!(
        state(|s| (s.tracks[1].fx[0].config["BANDTYPE1"].clone(), s.tracks[1].fx[0].config["BANDENABLED1"].clone())),
        ("6".into(), "0".into())
    );
    assert_eq!(applied["undo"]["entry"], true, "ReaEQ's band settings make no point by themselves: the nudge makes one");
    state(|s| s.tracks[1].fx[0].config.insert("BANDTYPE1".into(), "3".into()));
    assert_eq!(lab.undo("fx-eq-band", &preview, &applied).unwrap_err().0, "changed");
    lab.discard("fx-eq-band", &preview, &applied).unwrap();
    assert_eq!(
        state(|s| (s.tracks[1].fx[0].config["BANDTYPE1"].clone(), s.tracks[1].fx[0].config["BANDENABLED1"].clone())),
        ("8".into(), "1".into())
    );
    let preview = lab.preview("fx-eq-band", input.clone()).unwrap();
    state(|s| s.tracks[1].fx[0].config.insert("BANDENABLED1".into(), "0".into()));
    assert_eq!(lab.apply("fx-eq-band", &input, &preview["fence"]).unwrap_err().0, "changed");
    for (input, code) in [
        (json!({"ref": fx_ref("{PLUG}"), "band": 0, "type": "notch"}), "bad-params"),
        (json!({"ref": fx_ref("{EQ}"), "band": 2, "type": "notch"}), "bad-params"),
        (json!({"ref": fx_ref("{EQ}"), "band": 0, "type": "wobble"}), "bad-params"),
        (json!({"ref": fx_ref("{EQ}"), "band": 0}), "bad-params"),
    ] {
        assert_eq!(lab.preview("fx-eq-band", input.clone()).unwrap_err().0, code, "{input}");
    }
}

// ----- presets and state -----

fn with_presets(mut fx: FakeFx, chunk: bool) -> FakeFx {
    fx.presets = vec![
        FakePreset { name: "Soft".into(), values: vec![0.3, 0.0, 0.0, 1.0, 0.0] },
        FakePreset { name: "Hard".into(), values: vec![0.9, 1.0, 0.0, 1.0, 0.0] },
        FakePreset { name: "Wide".into(), values: vec![0.6, 0.5, 0.0, 1.0, 0.0] },
    ];
    if chunk {
        fx.chunk = Some(String::new());
    }
    fx
}

fn values(guid: &str) -> Vec<f64> {
    state(|s| s.tracks[1].fx.iter().find(|f| f.guid == guid).unwrap().params.iter().map(|p| p.value).collect())
}

fn preset_name(guid: &str) -> String {
    state(|s| s.tracks[1].fx.iter().find(|f| f.guid == guid).unwrap().preset.clone())
}

#[test]
fn a_preset_is_loaded_by_name_place_or_step_and_undo_puts_back_the_preset_and_every_value() {
    let lab = Lab::new();
    state(|s| {
        s.tracks[1].fx =
            vec![with_presets(plugin("{P}", "VST: ReaComp (Cockos)"), false), with_presets(plugin("{C}", "VST: ReaEQ (Cockos)"), true)];
        s.tracks[1].fx[0].preset = "Soft".into();
        for (p, v) in s.tracks[1].fx[0].params.iter_mut().zip([0.3, 0.0, 0.0, 1.0, 0.0]) {
            p.value = v;
        }
        s.state_count += 1;
    });
    for guid in ["{P}", "{C}"] {
        let before = (preset_name(guid), values(guid));
        let (preview, applied) = lab.make("fx-preset", json!({"ref": fx_ref(guid), "preset": "Hard"}));
        assert_eq!((preset_name(guid), values(guid)[0]), ("Hard".into(), 0.9), "{guid}");
        assert!(preview["display"]["title"].as_str().unwrap().ends_with("preset “Hard”"), "{}", preview["display"]["title"]);
        lab.undo("fx-preset", &preview, &applied).unwrap();
        assert_eq!((preset_name(guid), values(guid)), before, "{guid}: the preset and every value back");
        // By place and by step.
        let (preview, applied) = lab.make("fx-preset", json!({"ref": fx_ref(guid), "index": 2}));
        assert_eq!(preset_name(guid), "Wide");
        assert_eq!(applied["display"]["title"].as_str().unwrap().ends_with("preset “Wide”"), true);
        lab.undo("fx-preset", &preview, &applied).unwrap();
        assert_eq!((preset_name(guid), values(guid)), before);
        let (preview, applied) = lab.make("fx-preset", json!({"ref": fx_ref(guid), "step": 1}));
        lab.undo("fx-preset", &preview, &applied).unwrap();
        assert_eq!((preset_name(guid), values(guid)), before);
    }
    // A preset the producer had tweaked: its values come back exactly; a plug-in with a state chunk loses the name.
    state(|s| {
        s.tracks[1].fx[1].preset = "Soft".into();
        for (p, v) in s.tracks[1].fx[1].params.iter_mut().zip([0.35, 0.0, 0.0, 1.0, 0.0]) {
            p.value = v;
        }
    });
    let tweaked = values("{C}");
    let (preview, applied) = lab.make("fx-preset", json!({"ref": fx_ref("{C}"), "preset": "Hard"}));
    let undone = lab.undo("fx-preset", &preview, &applied).unwrap();
    assert_eq!(values("{C}"), tweaked);
    assert!(undone["display"]["note"].as_str().unwrap().contains("no preset name"), "{}", undone["display"]);
    // Changed since: refused; discard puts it back.
    let (preview, applied) = lab.make("fx-preset", json!({"ref": fx_ref("{P}"), "preset": "Hard"}));
    state(|s| s.tracks[1].fx[0].params[0].value = 0.1);
    assert_eq!(lab.undo("fx-preset", &preview, &applied).unwrap_err().0, "changed");
    lab.discard("fx-preset", &preview, &applied).unwrap();
    assert_eq!(preset_name("{P}"), "Soft");
    // No such preset: nothing changes. The fence: tweaked between the preview and the apply.
    let before = values("{P}");
    let input = json!({"ref": fx_ref("{P}"), "preset": "Nope"});
    let preview = lab.preview("fx-preset", input.clone()).unwrap();
    assert_eq!(lab.apply("fx-preset", &input, &preview["fence"]).unwrap_err().0, "failed");
    assert_eq!(values("{P}"), before);
    let input = json!({"ref": fx_ref("{P}"), "preset": "Wide"});
    let preview = lab.preview("fx-preset", input.clone()).unwrap();
    state(|s| s.tracks[1].fx[0].params[1].value = 0.9);
    assert_eq!(lab.apply("fx-preset", &input, &preview["fence"]).unwrap_err().0, "changed");
    for bad in [json!({"ref": fx_ref("{P}")}), json!({"ref": fx_ref("{P}"), "index": 9}), json!({"ref": fx_ref("{P}"), "step": 2})] {
        assert_eq!(lab.preview("fx-preset", bad.clone()).unwrap_err().0, "bad-params", "{bad}");
    }
}

#[test]
fn a_plug_ins_state_chunk_is_written_and_put_back_and_one_without_a_chunk_is_told_so() {
    let lab = Lab::new();
    state(|s| {
        s.tracks[1].fx = vec![reaeq("{EQ}"), plugin("{PLUG}", "VST: ReaComp (Cockos)")];
        s.state_count += 1;
    });
    let read = lab.read(json!({"ref": fx_ref("{EQ}")}));
    let state_text = read["items"][0]["state"]["chunk"].as_str().unwrap().to_string();
    let wanted = fake::chunk_of(&[0.1, 0.2, 0.3, 0.4, 0.5, 0.6, 1.0, 1.0, 0.0]);
    let input = json!({"ref": fx_ref("{EQ}"), "state": wanted});
    assert!(lab.read(json!({"ref": fx_ref("{PLUG}")}))["items"][0].get("state").is_none());
    let (preview, applied) = lab.make("fx-state", input.clone());
    assert_eq!(values("{EQ}")[..3], [0.1, 0.2, 0.3]);
    assert_eq!(applied["undo"]["entry"], true, "a state chunk makes no point by itself: the nudge makes one");
    lab.undo("fx-state", &preview, &applied).unwrap();
    assert_eq!(lab.read(json!({"ref": fx_ref("{EQ}")}))["items"][0]["state"]["chunk"], state_text);
    // Changed since; the fence; a plug-in with no chunk; not base64.
    let (preview, applied) = lab.make("fx-state", input.clone());
    state(|s| s.tracks[1].fx[0].params[0].value = 0.9);
    assert_eq!(lab.undo("fx-state", &preview, &applied).unwrap_err().0, "changed");
    lab.discard("fx-state", &preview, &applied).unwrap();
    let preview = lab.preview("fx-state", input.clone()).unwrap();
    state(|s| s.tracks[1].fx[0].params[2].value = 0.9);
    assert_eq!(lab.apply("fx-state", &input, &preview["fence"]).unwrap_err().0, "changed");
    assert_eq!(lab.preview("fx-state", json!({"ref": fx_ref("{PLUG}"), "state": "AAAA"})).unwrap_err().0, "bad-params");
    assert_eq!(lab.preview("fx-state", json!({"ref": fx_ref("{EQ}"), "state": "not base64!"})).unwrap_err().0, "bad-params");
}

// ----- FX chain files and copies -----

fn write_chain(dir: &tempfile::TempDir, name: &str, guids: &[&str]) {
    // Entries as the fake writes them (REAPER's lines: BYPASS … FXID … WAK).
    let mut text = String::new();
    for guid in guids {
        text += &format!(
            "BYPASS 0 0 0\n<VST \"VST: ReaEQ (Cockos)\" fake.vst 0 \"\" 0\nPARAM 0.75 Gain\n>\nFLOATPOS 0 0 0 0\nFXID {guid}\nWAK 0 0\n"
        );
    }
    let folder = dir.path().join("FXChains");
    std::fs::create_dir_all(folder.join("Mine")).unwrap();
    std::fs::write(folder.join(name), text).unwrap();
}

#[test]
fn an_fx_chain_file_is_loaded_into_a_chain_with_ids_of_its_own_and_undo_takes_its_fx_off() {
    let lab = chains_lab();
    let dir = resource();
    write_chain(&dir, "Mine/Vocal.RfxChain", &["{FX-EQ}", "{UNSEEN}"]);
    let (preview, applied) = lab.make("fx-chain-load", json!({"ref": KICK, "file": "Mine/Vocal", "index": 0}));
    assert_eq!(preview["display"]["title"], "Loaded the FX chain “Vocal” into “Kick”");
    let chain = state(|s| guids(&s.tracks[1].fx));
    assert_eq!(chain.len(), 3);
    assert_eq!(chain[2], "{K-MAIN}");
    // The file's ids were left out: REAPER gave both FX ids of their own (one of them was Drums' ReaEQ's).
    assert!(!chain.contains(&"{FX-EQ}".to_string()) && !chain.contains(&"{UNSEEN}".to_string()), "{chain:?}");
    assert_eq!(applied["created"]["fxRefs"].as_array().unwrap().len(), 2);
    assert_eq!(state(|s| s.tracks[1].fx[0].params[0].value), 0.75);
    lab.undo("fx-chain-load", &preview, &applied).unwrap();
    assert_eq!(state(|s| guids(&s.tracks[1].fx)), ["{K-MAIN}"]);
    assert_eq!(lab.undo("fx-chain-load", &preview, &applied).unwrap_err().0, "gone");
    // Into a take and into a container; a producer's change to one of the FX keeps them (discard takes them off).
    let (preview, applied) = lab.make("fx-chain-load", json!({"ref": TAKE, "file": "Mine/Vocal.RfxChain"}));
    assert_eq!(state(|s| s.tracks[1].items[0].takes[0].fx.len()), 3);
    state(|s| s.tracks[1].items[0].takes[0].fx[1].params[0].value = 0.1);
    assert_eq!(lab.undo("fx-chain-load", &preview, &applied).unwrap_err().0, "changed");
    lab.discard("fx-chain-load", &preview, &applied).unwrap();
    assert_eq!(state(|s| guids(&s.tracks[1].items[0].takes[0].fx)), ["{T-FX}"]);
    let (preview, applied) = lab.make("fx-chain-load", json!({"ref": fx_ref("{FX-C}"), "file": "Mine/Vocal"}));
    assert_eq!(state(|s| s.tracks[0].fx[1].children.as_ref().unwrap().len()), 4);
    lab.undo("fx-chain-load", &preview, &applied).unwrap();
    // The chain moved between the preview and the apply; a file that isn't there; a name climbing out of the folder.
    let input = json!({"ref": KICK, "file": "Mine/Vocal"});
    let preview = lab.preview("fx-chain-load", input.clone()).unwrap();
    state(|s| s.tracks[1].fx.push(plugin("{P}", "VST: ReaVerb (Cockos)")));
    assert_eq!(lab.apply("fx-chain-load", &input, &preview["fence"]).unwrap_err().0, "changed");
    assert_eq!(lab.preview("fx-chain-load", json!({"ref": KICK, "file": "Nope"})).unwrap_err().0, "failed");
    assert_eq!(lab.preview("fx-chain-load", json!({"ref": KICK, "file": "../Vocal"})).unwrap_err().0, "bad-params");
    assert_eq!(chain_files(&dir), 0);
    assert_eq!(state(|s| s.copies_into_containers), 0);
}

#[test]
fn an_fx_is_copied_to_another_track_a_take_and_a_container_and_undo_takes_the_copy_off() {
    let lab = chains_lab();
    let _dir = resource();
    state(|s| s.tracks[1].fx[0].params[0].value = 0.8);
    let (preview, applied) = lab.make("fx-copy", json!({"ref": fx_ref("{K-MAIN}"), "toRef": "r:track:{DDDD}", "index": 0}));
    assert_eq!(preview["display"]["title"], "Copied “ReaComp” on “Kick” to “Reverb”");
    let copy = state(|s| s.tracks[3].fx[0].clone());
    assert!(copy.guid != "{K-MAIN}" && copy.params[0].value == 0.8, "a copy of its own, with the state");
    lab.undo("fx-copy", &preview, &applied).unwrap();
    assert_eq!(state(|s| guids(&s.tracks[3].fx)), ["{FX-VERB}"]);
    let (preview, applied) = lab.make("fx-copy", json!({"ref": fx_ref("{K-MAIN}"), "toRef": TAKE}));
    assert_eq!(state(|s| s.tracks[1].items[0].takes[0].fx.len()), 2);
    lab.undo("fx-copy", &preview, &applied).unwrap();
    // Into a container: never by REAPER's copy, which hangs it.
    let (preview, applied) = lab.make("fx-copy", json!({"ref": fx_ref("{K-MAIN}"), "toRef": fx_ref("{FX-C}"), "index": 0}));
    let inside = state(|s| s.tracks[0].fx[1].children.as_ref().unwrap()[0].clone());
    assert!(inside.guid != "{K-MAIN}" && inside.params[0].value == 0.8);
    assert_eq!(state(|s| s.copies_into_containers), 0);
    lab.undo("fx-copy", &preview, &applied).unwrap();
    assert_eq!(state(|s| guids(s.tracks[0].fx[1].children.as_ref().unwrap())), ["{FX-COMP}", "{FX-C2}"]);
    // Refused: into itself; the source changed between the preview and the apply.
    assert_eq!(lab.preview("fx-copy", json!({"ref": fx_ref("{FX-C}"), "toRef": fx_ref("{FX-C2}")})).unwrap_err().0, "bad-params");
    let input = json!({"ref": fx_ref("{K-MAIN}"), "toRef": "r:track:{DDDD}"});
    let preview = lab.preview("fx-copy", input.clone()).unwrap();
    state(|s| s.tracks[1].fx[0].params[0].value = 0.1);
    assert_eq!(lab.apply("fx-copy", &input, &preview["fence"]).unwrap_err().0, "changed");
}

// ----- the guard against recording in touch, latch and write modes -----

#[test]
fn a_parameter_written_on_a_track_in_touch_mode_records_nothing_and_the_override_is_put_back() {
    let lab = chains_lab();
    state(|s| s.tracks[1].extra.insert("I_AUTOMODE".into(), 2.0));
    let envelopes = || state(|s| s.tracks[1].envelopes.len());
    let before = envelopes();
    let (preview, applied) = lab.make("fx-param", json!({"ref": fx_ref("{K-MAIN}"), "param": "Gain", "normalized": 0.9}));
    assert_eq!(envelopes(), before, "REAPER would record Kumi's write into an envelope");
    assert_eq!(state(|s| s.automation_override), -1, "the override is put back");
    lab.undo("fx-param", &preview, &applied).unwrap();
    lab.make("fx-bypass", json!({"ref": fx_ref("{K-MAIN}"), "bypass": true}));
    lab.make("fx-wet", json!({"ref": fx_ref("{K-MAIN}"), "wet": 0.5}));
    assert_eq!(envelopes(), before);
    // A global override in latch mode is the producer's: it is put back as it was.
    state(|s| s.automation_override = 4);
    lab.make("fx-param", json!({"ref": fx_ref("{K-MAIN}"), "param": "Gain", "normalized": 0.2}));
    assert_eq!((envelopes(), state(|s| s.automation_override)), (before, 4));
    // The producer's own write still records, as REAPER does: the fake's recording is real.
    let api = fake::api();
    let reaper = Reaper::new(&api);
    let fx = crate::reaper::Fx {
        owner: crate::reaper::FxOwner::Track(reaper.track_at(reaper.current_project().unwrap().ptr, 1).unwrap()),
        index: 0,
    };
    reaper.fx_set_param(fx, 0, 0.4);
    assert_eq!(envelopes(), before + 1);
}

#[test]
fn every_fx_kind_is_refused_for_another_epoch_and_while_reaper_is_busy() {
    let lab = chains_lab();
    let _dir = resource();
    write_chain(&_dir, "Busy.RfxChain", &["{B1}"]);
    for (kind, input) in [
        ("fx-offline", json!({"ref": fx_ref("{K-MAIN}"), "offline": true})),
        ("fx-wet", json!({"ref": fx_ref("{K-MAIN}"), "wet": 0.5})),
        ("fx-copy", json!({"ref": fx_ref("{K-MAIN}"), "toRef": TAKE})),
        ("fx-chain-load", json!({"ref": KICK, "file": "Busy"})),
        ("fx-container-add", json!({"ref": KICK})),
        ("fx-parallel", json!({"ref": fx_ref("{K-MAIN}"), "parallel": "parallel"})),
        ("fx-param-lfo", json!({"ref": "r:param:{K-MAIN}:0", "active": true})),
        ("envelope-show", json!({"trackRef": KICK, "name": "pan"})),
        ("track-automation-mode", json!({"ref": KICK, "mode": "latch"})),
    ] {
        let (code, _) = lab.call("change_preview", json!({"kind": kind, "input": input, "epoch": "old-1"})).unwrap_err();
        assert_eq!(code, "stale", "{kind}");
        let preview = lab.preview(kind, input.clone()).unwrap_or_else(|e| panic!("{kind}: {e:?}"));
        state(|s| s.loading = true);
        assert_eq!(lab.apply(kind, &input, &preview["fence"]).unwrap_err().0, "busy", "{kind}");
        state(|s| s.loading = false);
    }
}

// (read by the state test)
#[allow(dead_code)]
fn unused(_: Display, _: FakeItem, _: FakeTake, _: FakePoint, _: FakeAutoItem) {}
