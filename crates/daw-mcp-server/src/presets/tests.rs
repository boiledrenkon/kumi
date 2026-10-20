use super::*;

/// A Doofer holding an EQ 5 and a Gainer, a Splitter (Frequency type, a Gainer in lane 1 and an EQ 5 in lane 2, macro 1
/// mapped into lane 2), a fresh Doofer, a Notepad: as Renoise 3.5.4 wrote them in the lab.
const DOOFER: &str = include_str!("../../tests/fixtures/renoise/doofer_eq5_gainer.xml");
const SPLITTER: &str = include_str!("../../tests/fixtures/renoise/splitter_gainer_eq5.xml");
const FRESH: &str = include_str!("../../tests/fixtures/renoise/doofer_default.xml");
const NOTEPAD: &str = include_str!("../../tests/fixtures/renoise/notepad.xml");

const DOOFER_TYPE: &str = "DooferDevice";

fn device(label: &str) -> Device {
    Device { label: label.into(), track: "Bass".into(), path: format!("Audio/Effects/Native/{label}"), available: Map::new() }
}

fn ok(kind: &str, input: Value, text: &str, label: &str) -> Edited {
    edit(kind, &input, text, &device(label)).unwrap_or_else(|e| panic!("{kind}: {e:?}"))
}

fn refused(kind: &str, input: Value, text: &str, label: &str) -> Refusal {
    edit(kind, &input, text, &device(label)).expect_err(kind)
}

/// The lines that differ between two texts of as many lines, place by place: (before, after).
fn diff(before: &str, after: &str) -> (Vec<String>, Vec<String>) {
    let a: Vec<&str> = before.lines().collect();
    let b: Vec<&str> = after.lines().collect();
    assert_eq!(a.len(), b.len(), "an edit in place keeps the number of lines");
    let changed: Vec<(&str, &str)> = a.iter().zip(&b).filter(|(x, y)| x != y).map(|(x, y)| (*x, *y)).collect();
    (changed.iter().map(|(x, _)| x.trim().to_string()).collect(), changed.iter().map(|(_, y)| y.trim().to_string()).collect())
}

/// The edit taken back, as Kumi's undo does it (the shape after the edit is the one it left).
fn undone(kind: &str, edited: &Edited, now: &str, label: &str, discard: bool) -> Result<Undo, Refusal> {
    let shape = shape_of(&edited.document.to_xml());
    let kept = Kept { kind, prior: &edited.prior, applied: &edited.applied, shape: &shape, discard };
    undo(&kept, now, &shape_of(now), &device(label))
}

fn written(result: Result<Undo, Refusal>) -> String {
    match result {
        Ok(Undo::Write { document, .. }) => document.to_xml(),
        other => panic!("expected a write, got {other:?}"),
    }
}

#[test]
fn prints_and_shapes_are_the_tools() {
    // The same numbers the tool's changes_presets.lua gives (LuaJIT, on the same texts).
    assert_eq!(print_of("abc"), "0b885c8b3025f862:3");
    assert_eq!(print_of(""), "0000150500000000:0");
    assert_eq!(print_of(SPLITTER), "f340bd8e46a1f501:5889");
    assert_eq!(shape_of(SPLITTER), "1d1f047e3dbda60f:5362");
    assert_eq!(
        shape_of("<a><Value>1.0</Value><ParameterChunk><![CDATA[xx]]></ParameterChunk><Value><b/></Value></a>"),
        "f1e1d47b13de377c:51"
    );
    // Values and plug-in chunks move by themselves: the shape stays.
    let moved = DOOFER.replacen("<Value>50</Value>", "<Value>73.5</Value>", 1);
    assert_eq!(shape_of(&moved), shape_of(DOOFER));
    // So do the inner devices' SelectedPresetIsModified (Renoise turns it true when it writes the container again) and
    // IsSelected (the producer's clicks): lab, 2026-10-20.
    let flipped = SPLITTER
        .replace("<SelectedPresetIsModified>false</SelectedPresetIsModified>", "<SelectedPresetIsModified>true</SelectedPresetIsModified>");
    let clicked = flipped.replacen("<IsSelected>false</IsSelected>", "<IsSelected>true</IsSelected>", 1);
    assert_ne!(clicked, SPLITTER, "the fixture has both flags");
    assert_eq!(shape_of(&clicked), shape_of(SPLITTER));
    assert_ne!(shape_of(&DOOFER.replacen("<Name>Macro 1</Name>", "<Name>Drive</Name>", 1)), shape_of(DOOFER));
}

#[test]
fn a_macro_is_named_set_and_mapped_and_only_its_lines_change() {
    let edited = ok("doofer-macro", json!({"macro": 1, "name": "Drive", "value": 75}), DOOFER, "Doofer");
    let text = edited.document.to_xml();
    let (gone, new) = diff(DOOFER, &text);
    assert_eq!(gone, ["<Value>50</Value>", "<Name>Macro 1</Name>"]);
    assert_eq!(new, ["<Value>75</Value>", "<Name>Drive</Name>"]);
    assert_eq!(edited.title, "Doofer on Bass: macro 1 named \u{201c}Drive\u{201d}, at 75 %");
    assert_eq!(edited.prior, json!({"macro": 1, "name": "Macro 1", "value": 50.0}));
    // Mapped to the Gainer's gain (place 2, its parameter 1, by name too) over half its range, logarithmically.
    let mapped = ok(
        "doofer-macro",
        json!({"macro": 2, "mappings": [{"device": 2, "param": "gain", "min": 0.25, "max": 0.75, "scaling": "log-slow"}]}),
        DOOFER,
        "Doofer",
    );
    let text = mapped.document.to_xml();
    assert!(text.contains("<DestChainIndex>0</DestChainIndex>"), "a Doofer's chain is 0 (-1 crashes Renoise): {text}");
    assert!(text.contains("<DestDeviceIndex>1</DestDeviceIndex>") && text.contains("<DestParameterIndex>1</DestParameterIndex>"));
    assert!(text.contains("<Min>0.25</Min>") && text.contains("<Scaling>Log Slow</Scaling>"));
    assert_eq!(mapped.title, "Doofer on Bass: macro 2 mapped to \u{201c}Gain\u{201d} of \u{201c}Gainer\u{201d}");
    assert!(xml::parse(&text).is_ok());
    // How many it shows.
    let shown = ok("doofer-macro", json!({"macro": 1, "activeMacros": 2}), DOOFER, "Doofer");
    assert!(shown.document.to_xml().contains("<NumActiveMacros>2</NumActiveMacros>"));
    assert_eq!(shown.title, "Doofer on Bass: shows 2 macros");
}

#[test]
fn a_mapping_that_could_crash_renoise_is_refused_before_anything_is_written() {
    for (input, why) in [
        (json!({"macro": 9, "name": "x"}), "from 1 to 8"),
        (json!({"macro": 1, "mappings": [{"device": 3, "param": 1}]}), "from 1 to 2"),
        (json!({"macro": 1, "mappings": [{"device": 2, "param": 3}]}), "from 0 to 2"),
        (json!({"macro": 1, "mappings": [{"lane": 2, "device": 1, "param": 1}]}), "one chain"),
        (json!({"macro": 1, "mappings": [{"device": 1, "param": "Nope"}]}), "no parameter"),
        (json!({"macro": 1, "mappings": [{"device": 1, "param": 1, "max": 1.5}]}), "from 0 to 1"),
        (json!({"macro": 1, "mappings": [{"device": 1, "param": 1, "scaling": "s-curve"}]}), "scaling"),
        (json!({"macro": 1, "value": 101}), "0 to 100"),
        (json!({"macro": 1}), "give name"),
    ] {
        let refusal = refused("doofer-macro", input.clone(), DOOFER, "Doofer");
        assert!(matches!(&refusal, Refusal::Bad(m) if m.contains(why)), "{input}: {refusal:?}");
    }
    // A Splitter's mappings name their lane; its 16 macros.
    let mapped = ok("doofer-macro", json!({"macro": 16, "mappings": [{"lane": 2, "device": 1, "param": "Gain 1"}]}), SPLITTER, "Splitter");
    assert!(mapped.document.to_xml().contains("<DestChainIndex>1</DestChainIndex>"));
    assert!(
        matches!(refused("doofer-macro", json!({"macro": 1, "name": "x"}), NOTEPAD, "Notepad"), Refusal::Bad(m) if m.contains("isn't a Doofer"))
    );
    // Plug-ins inside: Kumi can't tell their parameter count, so it maps nothing to them.
    let with_plugin = DOOFER.replacen(
        "<GainerDevice type=\"GainerDevice\">",
        "<AudioPluginDevice type=\"AudioPluginDevice\"><PluginIdentifier>aufx:dely:appl</PluginIdentifier></AudioPluginDevice>\n        <GainerDevice type=\"GainerDevice\">",
        1,
    );
    let refusal = refused("doofer-macro", json!({"macro": 1, "mappings": [{"device": 2, "param": 1}]}), &with_plugin, "Doofer");
    assert!(matches!(&refusal, Refusal::Bad(m) if m.contains("can't tell how many parameters")), "{refusal:?}");
}

#[test]
fn a_device_put_in_moves_the_mappings_and_meta_devices_after_it_and_its_undo_takes_it_out_exactly() {
    // Macro 1 drives the Gainer (place 2); an LFO aimed at the Gainer sits after it.
    let mapped = ok("doofer-macro", json!({"macro": 1, "mappings": [{"device": 2, "param": 1}]}), DOOFER, "Doofer").document.to_xml();
    let with_lfo = mapped.replacen(
        "      </Devices>",
        "        <LfoDevice type=\"LfoDevice\">\n          <DestEffect>\n            <Value>1</Value>\n          </DestEffect>\n          <DestParameter>\n            <Value>1</Value>\n          </DestParameter>\n        </LfoDevice>\n      </Devices>",
        1,
    );
    let added =
        ok("doofer-device-add", json!({"path": "Audio/Effects/Native/Analog Filter", "at": [1], "name": "Low cut"}), &with_lfo, "Doofer");
    let text = added.document.to_xml();
    let document = xml::parse(&text).unwrap();
    let slot_ = slot(&document).unwrap();
    let list = devices_in(slot_, DOOFER_TYPE, 0);
    assert_eq!(list.iter().map(|d| d.name()).collect::<Vec<_>>(), ["AnalogFilterDevice", "Eq5Device", "GainerDevice", "LfoDevice"]);
    assert_eq!(list[0].child_text("CustomDeviceName").as_deref(), Some("Low cut"));
    assert!(text.contains("<DestDeviceIndex>2</DestDeviceIndex>"), "the mapping follows the Gainer to place 3");
    assert_eq!(value_of(list[3], "DestEffect"), Some(2.0), "the LFO follows it too");
    assert_eq!(added.title, "Added \u{201c}Low cut\u{201d} to \u{201c}Doofer on Bass\u{201d} (place 1)");
    // Kumi's undo takes it out again: the text it started from, byte for byte.
    assert_eq!(written(undone("doofer-device-add", &added, &text, "Doofer", false)), with_lfo);
    // Something structural changed since (the producer's mapping): refused, nothing to write.
    let theirs = text.replacen("<NumActiveMacros>4</NumActiveMacros>", "<NumActiveMacros>4</NumActiveMacros>\n    <Bogus/>", 1);
    assert!(
        matches!(undone("doofer-device-add", &added, &theirs, "Doofer", true), Err(Refusal::Changed(_))),
        "discard doesn't pass a changed structure"
    );
    // A value moved since (a knob, automation) doesn't stop it.
    let knob = text.replacen("<Value>0.0</Value>", "<Value>3.5</Value>", 1);
    assert!(matches!(undone("doofer-device-add", &added, &knob, "Doofer", false), Ok(Undo::Write { .. })));
}

#[test]
fn routing_devices_and_unknown_ones_never_go_inside_and_places_are_checked() {
    for (path, why) in [
        ("#Send", "routing devices"),
        ("Audio/Effects/Native/#Sidechain", "routing devices"),
        ("#Multiband Send", "routing devices"),
        ("#Line Input", "routing devices"),
        ("Fuzzbox", "no native device"),
        ("Audio/Effects/VST3/Something", "only native devices and Audio Units"),
        ("Audio/Effects/AU/aufx:nope:none", "no Audio Unit"),
    ] {
        let refusal = refused("doofer-device-add", json!({"path": path}), DOOFER, "Doofer");
        assert!(matches!(&refusal, Refusal::Bad(m) if m.contains(why)), "{path}: {refusal:?}");
    }
    for at in [json!([0]), json!([4]), json!([1, 1])] {
        assert!(matches!(refused("doofer-device-add", json!({"path": "Gainer", "at": at}), DOOFER, "Doofer"), Refusal::Bad(_)), "{at}");
    }
    // An Audio Unit the track can take: its type and identifier.
    let mut with_au = device("Doofer");
    with_au.available.insert("Audio/Effects/AU/aufx:dely:appl".into(), true.into());
    let added = edit("doofer-device-add", &json!({"path": "Audio/Effects/AU/aufx:dely:appl"}), DOOFER, &with_au).unwrap();
    assert!(added.document.to_xml().contains("<PluginIdentifier>aufx:dely:appl</PluginIdentifier>"));
    // The end by default, and into a Splitter's lane.
    let last = ok("doofer-device-add", json!({"path": "DC Offset"}), DOOFER, "Doofer");
    assert_eq!(last.applied["at"], json!([3]));
    let lane = ok("doofer-device-add", json!({"path": "Gainer", "at": [2, 1]}), SPLITTER, "Splitter");
    let text = lane.document.to_xml();
    assert!(text.contains("<DestDeviceIndex>1</DestDeviceIndex>"), "the lane-2 mapping follows the EQ to place 2: {text}");
    assert_eq!(lane.title, "Added \u{201c}Gainer\u{201d} to \u{201c}Splitter on Bass\u{201d} (lane 2, place 1)");
}

#[test]
fn a_device_taken_out_takes_its_mappings_with_it_and_its_undo_puts_back_everything() {
    let mapped =
        ok("doofer-macro", json!({"macro": 1, "mappings": [{"device": 1, "param": 1}, {"device": 2, "param": 1}]}), DOOFER, "Doofer")
            .document
            .to_xml();
    let deleted = ok("doofer-device-delete", json!({"at": [1]}), &mapped, "Doofer");
    let text = deleted.document.to_xml();
    let document = xml::parse(&text).unwrap();
    assert_eq!(devices_in(slot(&document).unwrap(), DOOFER_TYPE, 0).iter().map(|d| d.name()).collect::<Vec<_>>(), ["GainerDevice"]);
    assert_eq!(text.matches("<Mapping>").count(), 1, "the EQ's mapping went");
    assert!(text.contains("<DestDeviceIndex>0</DestDeviceIndex>"), "the Gainer's follows it to place 1");
    assert_eq!(deleted.title, "Took \u{201c}EQ 5\u{201d} out of \u{201c}Doofer on Bass\u{201d} (place 1)");
    assert!(deleted.note.as_deref().unwrap().contains("1 macro mapping to it went with it"));
    // Kumi's undo: the EQ, its mapping (first in the macro's list again), the Gainer's index: the text before.
    assert_eq!(written(undone("doofer-device-delete", &deleted, &text, "Doofer", false)), mapped);
    // A Doofer inside: deleted with what it holds.
    let nested = ok("doofer-device-add", json!({"path": "Doofer", "at": [3]}), DOOFER, "Doofer").document.to_xml();
    let inner = ok("doofer-device-add", json!({"path": "Gainer", "at": [3, 1]}), &nested, "Doofer").document.to_xml();
    assert!(inner.contains("<DooferDevice type=\"DooferDevice\">"));
    let gone = ok("doofer-device-delete", json!({"at": [3]}), &inner, "Doofer");
    assert_eq!(written(undone("doofer-device-delete", &gone, &gone.document.to_xml(), "Doofer", false)), inner);
}

#[test]
fn a_device_moves_with_its_mappings_in_its_chain_and_across_a_splitters_lanes() {
    let mapped = ok("doofer-macro", json!({"macro": 1, "mappings": [{"device": 1, "param": 1}]}), DOOFER, "Doofer").document.to_xml();
    let moved = ok("doofer-device-move", json!({"at": [1], "to": [2]}), &mapped, "Doofer");
    let text = moved.document.to_xml();
    let document = xml::parse(&text).unwrap();
    assert_eq!(
        devices_in(slot(&document).unwrap(), DOOFER_TYPE, 0).iter().map(|d| d.name()).collect::<Vec<_>>(),
        ["GainerDevice", "Eq5Device"]
    );
    assert!(text.contains("<DestDeviceIndex>1</DestDeviceIndex>"), "macro 1 still drives the EQ, now place 2");
    assert_eq!(written(undone("doofer-device-move", &moved, &text, "Doofer", false)), mapped);
    // The Splitter's EQ (lane 2, driven by macro 1) to lane 1: its mapping changes lane.
    let across = ok("doofer-device-move", json!({"at": [2, 1], "to": [1, 1]}), SPLITTER, "Splitter");
    let text = across.document.to_xml();
    assert!(text.contains("<DestChainIndex>0</DestChainIndex>") && text.contains("<DestDeviceIndex>0</DestDeviceIndex>"), "{text}");
    assert_eq!(written(undone("doofer-device-move", &across, &text, "Splitter", false)), SPLITTER);
    // Into a Doofer inside, while a macro drives it: refused.
    let nested = ok("doofer-device-add", json!({"path": "Doofer", "at": [3]}), &mapped, "Doofer").document.to_xml();
    // (`to` is where it is once moved: the Doofer at place 3 is at place 2 with the EQ out of its way)
    assert!(
        matches!(refused("doofer-device-move", json!({"at": [1], "to": [2, 1]}), &nested, "Doofer"), Refusal::Bad(m) if m.contains("only inside its own"))
    );
    // Nothing drives the Gainer: it may go in.
    let inside = ok("doofer-device-move", json!({"at": [2], "to": [2, 1]}), &nested, "Doofer");
    let document = xml::parse(&inside.document.to_xml()).unwrap();
    let list = devices_in(slot(&document).unwrap(), DOOFER_TYPE, 0);
    assert_eq!(list.iter().map(|d| d.name()).collect::<Vec<_>>(), ["Eq5Device", "DooferDevice"]);
    assert_eq!(devices_in(list[1], DOOFER_TYPE, 0).iter().map(|d| d.name()).collect::<Vec<_>>(), ["GainerDevice"]);
}

#[test]
fn a_device_inside_is_set_by_value_or_text_in_range_and_its_undo_puts_back_only_what_it_changed() {
    let set = ok(
        "doofer-device-set",
        json!({"at": [2], "param": "Gain", "valueString": "-6 dB", "bypass": true, "name": "Trim"}),
        DOOFER,
        "Doofer",
    );
    let text = set.document.to_xml();
    let document = xml::parse(&text).unwrap();
    let gainer = devices_in(slot(&document).unwrap(), DOOFER_TYPE, 0)[1];
    assert!((value_of(gainer, "Volume").unwrap() - 0.501_187).abs() < 1e-5);
    assert_eq!(value_of(gainer, "IsActive"), Some(0.0));
    assert_eq!(gainer.child_text("CustomDeviceName").as_deref(), Some("Trim"));
    assert_eq!(set.title, "Gainer in Doofer on Bass: Gain 0.501, bypassed, renamed \u{201c}Trim\u{201d}");
    // The producer moved another knob since: Kumi's undo puts back only its three.
    let knob = text.replacen("<Value>100</Value>", "<Value>250</Value>", 1);
    let back = written(undone("doofer-device-set", &set, &knob, "Doofer", false));
    assert_eq!(back, DOOFER.replacen("<Value>100</Value>", "<Value>250</Value>", 1));
    // The producer moved the Gain since: refused, unless discard.
    let theirs = text.replacen(&format!("<Value>{}</Value>", xml::number_text(0.501_187_233_627_272_6)), "<Value>2</Value>", 1);
    assert_ne!(theirs, text);
    assert!(matches!(undone("doofer-device-set", &set, &theirs, "Doofer", false), Err(Refusal::Changed(_))));
    assert!(matches!(undone("doofer-device-set", &set, &theirs, "Doofer", true), Ok(Undo::Write { .. })));
    // Back already: nothing to write.
    assert!(matches!(undone("doofer-device-set", &set, DOOFER, "Doofer", false), Ok(Undo::Back(_))));
    // A device added before it since: the one Kumi changed can't be told.
    let shifted = ok("doofer-device-add", json!({"path": "Gainer", "at": [1]}), &text, "Doofer").document.to_xml();
    assert!(matches!(undone("doofer-device-set", &set, &shifted, "Doofer", true), Err(Refusal::Changed(_))));
}

#[test]
fn a_value_out_of_range_or_a_trigger_renoise_doesnt_save_is_refused() {
    for (input, why) in [
        (json!({"at": [2], "param": "Gain", "value": 9}), "from 0 to 4"),
        (json!({"at": [1], "param": "Gain 1", "valueString": "loud"}), "doesn't read"),
        (json!({"at": [1], "param": 99, "value": 1}), "from 1 to 15"),
        (json!({"at": [2]}), "give param"),
    ] {
        let refusal = refused("doofer-device-set", input.clone(), DOOFER, "Doofer");
        assert!(matches!(&refusal, Refusal::Bad(m) if m.contains(why)), "{input}: {refusal:?}");
    }
    let lfo = ok("doofer-device-add", json!({"path": "*LFO"}), DOOFER, "Doofer").document.to_xml();
    let refusal = refused("doofer-device-set", json!({"at": [3], "param": "Reset", "value": 1}), &lfo, "Doofer");
    assert!(matches!(&refusal, Refusal::Bad(m) if m.contains("doesn't save")), "{refusal:?}");
    // A meta device's destination takes -1, none.
    assert!(edit("doofer-device-set", &json!({"at": [3], "param": "Dest. Effect", "value": -1}), &lfo, &device("Doofer")).is_ok());
    // A parameter a macro drives: said.
    let mapped = ok("doofer-macro", json!({"macro": 1, "name": "Drive", "mappings": [{"device": 2, "param": 1}]}), DOOFER, "Doofer")
        .document
        .to_xml();
    let driven = ok("doofer-device-set", json!({"at": [2], "param": 1, "value": 2}), &mapped, "Doofer");
    assert!(driven.note.as_deref().unwrap().contains("the macro \u{201c}Drive\u{201d} drives it"), "{:?}", driven.note);
}

#[test]
fn a_splitter_splits_another_way_and_a_notepad_says_something_else() {
    let split = ok("splitter-set", json!({"type": "mid-side"}), SPLITTER, "Splitter");
    assert!(split.document.to_xml().contains("<Type>Mid/Side</Type>"));
    assert_eq!(split.title, "Splitter on Bass splits into mid and side");
    assert_eq!(split.prior, json!({"type": "frequency"}));
    assert_eq!(written(undone("splitter-set", &split, &split.document.to_xml(), "Splitter", false)), SPLITTER);
    assert!(
        matches!(refused("splitter-set", json!({"type": "mid-side"}), DOOFER, "Doofer"), Refusal::Bad(m) if m.contains("isn't a Splitter"))
    );
    let note = ok("notepad-write", json!({"text": "Bass: mono below 120 Hz\nkeep the sub & kick apart"}), NOTEPAD, "Notepad");
    let text = note.document.to_xml();
    assert!(text.contains("<Paragraph>keep the sub &amp; kick apart</Paragraph>"), "{text}");
    assert_eq!(note.title, "Wrote the Notepad on Bass");
    assert_eq!(written(undone("notepad-write", &note, &text, "Notepad", false)), NOTEPAD);
    // One inside a Doofer.
    let with_pad = ok("doofer-device-add", json!({"path": "Notepad", "at": [1]}), FRESH, "Doofer").document.to_xml();
    let inner = ok("notepad-write", json!({"at": [1], "text": "macro 1: drive"}), &with_pad, "Doofer");
    assert!(inner.document.to_xml().contains("<Paragraph>macro 1: drive</Paragraph>"));
    assert!(matches!(refused("notepad-write", json!({"text": "x"}), DOOFER, "Doofer"), Refusal::Bad(m) if m.contains("give at")));
}

#[test]
fn a_macros_undo_puts_back_each_field_it_changed_and_refuses_one_changed_since() {
    let edited = ok("doofer-macro", json!({"macro": 1, "name": "Drive", "mappings": [{"device": 2, "param": 1}]}), DOOFER, "Doofer");
    let text = edited.document.to_xml();
    assert_eq!(written(undone("doofer-macro", &edited, &text, "Doofer", false)), DOOFER);
    // The producer renamed it since.
    let renamed = text.replacen("<Name>Drive</Name>", "<Name>Theirs</Name>", 1);
    assert!(matches!(undone("doofer-macro", &edited, &renamed, "Doofer", false), Err(Refusal::Changed(_))));
    assert!(matches!(undone("doofer-macro", &edited, &renamed, "Doofer", true), Ok(Undo::Write { .. })));
    assert!(matches!(undone("doofer-macro", &edited, DOOFER, "Doofer", false), Ok(Undo::Back(_))));
}

#[test]
fn a_read_says_what_a_container_holds_by_place_with_renoises_texts_and_a_notepads_words() {
    let mapped =
        ok("doofer-macro", json!({"macro": 1, "name": "Drive", "mappings": [{"device": 2, "param": 1, "max": 0.5}]}), DOOFER, "Doofer")
            .document
            .to_xml();
    let (key, inside) = super::inside(&mapped, true).unwrap();
    assert_eq!(key, "inside");
    assert_eq!(inside["type"], "doofer");
    assert_eq!(inside["macros"][0]["name"], "Drive");
    assert_eq!(inside["macros"][0]["mappings"][0]["drives"], "Gain of Gainer");
    assert!(inside["macros"][0]["mappings"][0].get("lane").is_none(), "a Doofer's mappings name no lane");
    let devices = inside["devices"].as_array().unwrap();
    assert_eq!(devices.len(), 2);
    assert_eq!((&devices[1]["at"], &devices[1]["name"], &devices[1]["device"]), (&json!([2]), &json!("Gainer"), &json!("Gainer")));
    let gain = devices[1]["params"].as_array().unwrap().iter().find(|p| p["name"] == "Gain").unwrap();
    assert_eq!((&gain["value"], &gain["text"]), (&json!(1.0), &json!("0 dB")));
    let eq_gain = &devices[0]["params"][0];
    assert_eq!((&eq_gain["param"], &eq_gain["text"]), (&json!(1), &json!("0 dB")));
    // A Splitter: its split and its lanes, places with their lane.
    let (_, split) = super::inside(SPLITTER, false).unwrap();
    assert_eq!((&split["type"], &split["split"]), (&json!("splitter"), &json!("frequency")));
    assert_eq!(split["lanes"][1][0]["at"], json!([2, 1]));
    assert!(split["lanes"][1][0].get("params").is_none(), "no parameters when not asked");
    assert_eq!(split["macros"][0]["mappings"][0]["lane"], 2);
    // A Notepad's text; any other device, nothing.
    assert_eq!(super::inside(NOTEPAD, true), Some(("text", json!("Bass: keep it mono & dry\nLine two \u{2014} \u{e9}t\u{e9}"))));
    let eq5 =
        "<?xml version=\"1.0\"?>\n<FilterDevicePreset doc_version=\"14\">\n  <DeviceSlot type=\"Eq5Device\"/>\n</FilterDevicePreset>\n";
    assert_eq!(super::inside(eq5, true), None);
}

#[test]
fn a_dsp_body_gets_each_containers_devices_as_branches_a_splitters_second_lane_starting_a_new_one() {
    let nested = ok("doofer-device-add", json!({"path": "Doofer", "at": [3]}), DOOFER, "Doofer").document.to_xml();
    let nested = ok("doofer-device-add", json!({"path": "Gainer", "at": [3, 1], "name": "Inner"}), &nested, "Doofer").document.to_xml();
    let nodes = child_nodes(&nested);
    assert_eq!(nodes.iter().map(|n| n["name"].as_str().unwrap()).collect::<Vec<_>>(), ["EQ 5", "Gainer", "Doofer"]);
    assert_eq!(nodes[2]["container"], true);
    assert_eq!(nodes[2]["childNodes"][0]["name"], "Inner");
    assert_eq!(nodes[2]["childNodes"][0]["at"], json!([3, 1]));
    let lanes = child_nodes(SPLITTER);
    assert_eq!(lanes.len(), 2);
    assert!(lanes[0].get("parallel").is_none() && lanes[1]["parallel"] == true, "{lanes:?}");
    assert!(child_nodes(NOTEPAD).is_empty());
}

fn make_read(reference: &str, label: &str, name: &str, xml: &str, parameters: Value) -> (String, Value) {
    (
        reference.into(),
        json!({"label": label, "name": name, "active": true, "presetData": xml, "parameters": parameters, "trackName": "Bass"}),
    )
}

#[test]
fn a_doofer_is_made_of_devices_with_their_settings_names_bypass_aims_and_the_macros_asked() {
    let eq5 = "<?xml version=\"1.0\"?>\n<FilterDevicePreset doc_version=\"14\">\n  <DeviceSlot type=\"Eq5Device\">\n    <Gain0>\n      <Value>-5</Value>\n    </Gain0>\n  </DeviceSlot>\n</FilterDevicePreset>\n";
    let lfo = "<?xml version=\"1.0\"?>\n<FilterDevicePreset doc_version=\"14\">\n  <DeviceSlot type=\"LfoDevice\">\n    <Amplitude>\n      <Value>0.5</Value>\n    </Amplitude>\n  </DeviceSlot>\n</FilterDevicePreset>\n";
    // The LFO (Bass's device 3) drives the EQ (Bass's device 2) Gain 1.
    let aims = json!([{"name": "Dest. Track", "value": 1}, {"name": "Dest. Effect", "value": 1}, {"name": "Dest. Parameter", "value": 1}, {"name": "Amplitude", "value": 0.5}]);
    let lanes =
        vec![vec![make_read("e1:device:2.2", "Low cut", "EQ 5", eq5, json!([])), make_read("e1:device:2.3", "*LFO", "*LFO", lfo, aims)]];
    let input = json!({"macros": [{"name": "Wobble", "mappings": [{"device": 2, "param": "Amplitude"}]}]});
    let made = make(&input, &lanes, "Bass").unwrap();
    let text = made.document.to_xml();
    let document = xml::parse(&text).unwrap();
    let slot_ = slot(&document).unwrap();
    let list = devices_in(slot_, DOOFER_TYPE, 0);
    assert_eq!(list.iter().map(|d| d.name()).collect::<Vec<_>>(), ["Eq5Device", "LfoDevice"]);
    assert_eq!(list[0].child_text("CustomDeviceName").as_deref(), Some("Low cut"));
    assert_eq!(value_of(list[0], "Gain0"), Some(-5.0), "its settings");
    assert_eq!(value_of(list[0], "IsActive"), Some(1.0));
    assert_eq!(
        (value_of(list[1], "DestTrack"), value_of(list[1], "DestEffect"), value_of(list[1], "DestParameter")),
        (Some(-1.0), Some(0.0), Some(1.0))
    );
    assert_eq!(slot_.child("Macro0").and_then(|m| m.child_text("Name")).as_deref(), Some("Wobble"));
    assert!(text.contains("<DestDeviceIndex>1</DestDeviceIndex>") && text.contains("<DestParameterIndex>4</DestParameterIndex>"), "{text}");
    assert_eq!(made.title, "Made a Doofer of \u{201c}Low cut\u{201d} and \u{201c}*LFO\u{201d} on \u{201c}Bass\u{201d}");
    // A meta device aimed at a device staying outside: refused.
    let outside =
        json!([{"name": "Dest. Track", "value": 1}, {"name": "Dest. Effect", "value": 4}, {"name": "Dest. Parameter", "value": 1}]);
    let lanes = vec![vec![make_read("e1:device:2.3", "*LFO", "*LFO", lfo, outside)]];
    assert!(matches!(make(&json!({}), &lanes, "Bass"), Err(Refusal::Bad(m)) if m.contains("stays outside")));
    // A routing device: refused. A Splitter: its lanes and split.
    let send =
        "<?xml version=\"1.0\"?>\n<FilterDevicePreset doc_version=\"14\">\n  <DeviceSlot type=\"SendDevice\"/>\n</FilterDevicePreset>\n";
    let lanes = vec![vec![make_read("e1:device:2.2", "#Send", "#Send", send, json!([]))]];
    assert!(matches!(make(&json!({}), &lanes, "Bass"), Err(Refusal::Bad(m)) if m.contains("routing devices")));
    let lanes = vec![
        vec![make_read("e1:device:2.2", "EQ 5", "EQ 5", eq5, json!([]))],
        vec![make_read("e1:device:2.4", "EQ 5", "EQ 5", eq5, json!([]))],
    ];
    let split = make(&json!({"type": "splitter", "split": "frequency"}), &lanes, "Bass").unwrap();
    let text = split.document.to_xml();
    assert!(text.contains("<Type>Frequency</Type>"));
    let document = xml::parse(&text).unwrap();
    assert_eq!(devices_in(slot(&document).unwrap(), "SplitterDevice", 1).len(), 1);
    assert!(matches!(make(&json!({"type": "doofer"}), &lanes, "Bass"), Err(Refusal::Bad(m)) if m.contains("second lane")));
}

#[test]
fn a_plug_in_inside_a_doofer_reads_by_the_name_renoise_gives_it() {
    // Renoise 3.5.4 writes a plug-in inside a Doofer with its type and identifier only (lab): the tool's preset_read names
    // it (`plugins`).
    let with_au = DOOFER.replacen(
        "</Devices>",
        "  <AudioPluginDevice type=\"AudioPluginDevice\">\n            <PluginType>AU</PluginType>\n            <PluginIdentifier>aufx:dely:appl</PluginIdentifier>\n          </AudioPluginDevice>\n        </Devices>",
        1,
    );
    let names = json!({"aufx:dely:appl": "AUDelay"});
    let (_, inside) = with_plugin_names(Some(&names), || super::inside(&with_au, false)).unwrap();
    let devices = inside["devices"].as_array().unwrap();
    assert_eq!(devices.last().unwrap()["name"], "AUDelay", "{inside}");
    assert_eq!(with_plugin_names(Some(&names), || child_nodes(&with_au)).last().unwrap()["name"], "AUDelay");
    // Without the names (and after them), its type.
    let (_, unnamed) = super::inside(&with_au, false).unwrap();
    assert_eq!(unnamed["devices"].as_array().unwrap().last().unwrap()["name"], "AudioPlugin");
}
