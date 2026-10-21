//! FOCUS draws REAPER's, Renoise's and Logic's views as the producer moves between them: REAPER's MIDI editor shows
//! the take's notes, its arrange view each track's items, a track's routing its sends; Renoise's pattern editor shows
//! the lines around the cursor, its matrix the sequence, an instrument its keyzones; Logic's Piano Roll shows the
//! region's notes, its Tracks area each track's regions, a channel strip its plug-ins and its routing.
#[path = "support/models.rs"]
mod models;
#[path = "support/tui_app.rs"]
mod support;
use kumi::daw::work_with;
use kumi_runtime::core::{contracts::*, daw::DawId};
use serde_json::json;
use std::rc::Rc;
use support::*;
/// Waits (a little) for FOCUS to draw what a body read brings.
async fn shows(h: &Harness, text: &str) {
    for _ in 0..200 {
        if has(&h.screen(), text) {
            return;
        }
        delay(5).await;
    }
    h.has(text);
}
macro_rules! case {
    ($name:ident,$body:expr) => {
        #[tokio::test(flavor = "current_thread")]
        async fn $name() {
            tokio::task::LocalSet::new().run_until($body).await;
        }
    };
}

case!(reapers_midi_editor_arrange_view_and_routing_draw_in_focus, async {
    work_with(DawId::Reaper);
    let c = Rc::new(Control::default());
    *c.clip.borrow_mut() = Some(
        serde_json::from_value(json!({"slotRef":"r:take:{T1}","name":"Bass take","length":16.0,"notes":[
            {"pitch":36,"start":0.0,"duration":1.0,"velocity":100},{"pitch":43,"start":2.0,"duration":0.5,"velocity":90,"selected":true}
        ]}))
        .unwrap(),
    );
    let h = Harness::with(120, 40, c.clone(), |_| {});
    h.start().await;
    h.connect();
    let track = json!({"name":"Bass","color":"#3c7cf5","kind":"midi"});
    let midi = json!({"track":track,"trackRef":"r:track:{B}","mainView":{"kind":"timeline","label":"Arrange"},"editorView":{"kind":"note-editor","label":"MIDI editor"},"item":{"kind":"take","itemRef":"r:take:{T1}","name":"Bass take"}});
    h.emit(json!({"type":"focus","focus":midi}));
    shows(&h, "FOCUS · MIDI").await;
    h.has("Bass take");
    h.has("4 bars · 2 notes · 1 selected");
    assert!(h.calls().contains(&"clip:r:take:{T1}".to_string()), "{:?}", h.calls());

    // The routing window: the track's sends, with their levels and taps.
    *c.own.borrow_mut() = Some(FocusBody::Routing(RoutingView {
        track_ref: "r:track:{B}".into(),
        routes: vec![
            Route {
                kind: RouteKind::Send,
                other: "Reverb Bus".into(),
                volume_db: Some(-12.04),
                muted: false,
                tap: Some("post-fader".into()),
            },
            Route { kind: RouteKind::Receive, other: "Kick".into(), volume_db: None, muted: false, tap: Some("pre-fader".into()) },
        ],
        parent_send: Some(true),
    }));
    let routing = json!({"track":track,"trackRef":"r:track:{B}","mainView":{"kind":"timeline","label":"Arrange"},"editorView":{"kind":"routing","label":"Routing"}});
    h.emit(json!({"type":"focus","focus":routing}));
    shows(&h, "FOCUS · Routing").await;
    h.has("→ Reverb Bus · -12 dB · post-fader");
    h.has("← Kick · pre-fader");
    h.has("feeds its parent");

    // The arrange view: each track's items as bars on the timeline.
    *c.arrangement.borrow_mut() = Some(
        serde_json::from_value(
            json!({"length":64.0,"position":8.0,"playing":true,"locators":[{"name":"Verse","position":0.0}],"beatsPerBar":4.0,"track_count":200,"lanes":[
                {"name":"Drums","depth":0,"items":[{"start":0.0,"length":32.0,"name":"Beat"}]},
                {"name":"Bass","depth":1,"items":[{"start":16.0,"length":16.0,"name":"Bass take","selected":true}]}
            ]}),
        )
        .unwrap(),
    );
    let arrange = json!({"track":track,"trackRef":"r:track:{B}","mainView":{"kind":"timeline","label":"Arrange"}});
    h.emit(json!({"type":"focus","focus":arrange}));
    shows(&h, "FOCUS · Arrange").await;
    h.has("Drums");
    h.has("  Bass");
    h.has("▬▬");
    h.has("198 more tracks below");

    // A page of lanes from track 101 of 200, two tracks named Bass: the selected one (by its ref) is centred, and the
    // rest are counted above and below.
    let lane = |i: usize| {
        let name = if i == 1 || i == 8 { "Bass".to_string() } else { format!("T{i}") };
        json!({"name": name, "track_ref": format!("r:track:{{{i}}}"), "depth": 0, "items": []})
    };
    *c.arrangement.borrow_mut() = Some(
        serde_json::from_value(json!({"length":64.0,"position":8.0,"playing":false,"locators":[],"track_count":200,"lanes_from":100,
            "lanes":(0..10).map(lane).collect::<Vec<_>>()}))
        .unwrap(),
    );
    let arrange = json!({"track":{"name":"Bass"},"trackRef":"r:track:{8}","mainView":{"kind":"timeline","label":"Arrange"}});
    h.emit(json!({"type":"focus","focus":arrange}));
    shows(&h, "102 more tracks above · 90 below").await;

    // What REAPER doesn't draw, FOCUS doesn't either: a hidden track, and the tracks of a folder collapsed all the way
    // (marked ▸); they're counted, not drawn.
    *c.arrangement.borrow_mut() = Some(
        serde_json::from_value(json!({"length":64.0,"position":0.0,"playing":false,"locators":[],"track_count":5,"lanes":[
            {"name":"Drums","depth":0,"collapsed":true,"items":[]},
            {"name":"Kick","depth":1,"hidden":true,"items":[]},
            {"name":"Guide","depth":0,"hidden":true,"items":[]},
            {"name":"Keys","depth":0,"items":[]},
            {"name":"Vox","depth":0,"items":[]}
        ]}))
        .unwrap(),
    );
    let arrange = json!({"track":{"name":"Vox"},"trackRef":"r:track:{V}","mainView":{"kind":"timeline","label":"Arrange"}});
    h.emit(json!({"type":"focus","focus":arrange}));
    shows(&h, "2 hidden").await;
    h.has("▸ Drums");
    h.has("Keys");
    let screen = h.screen().join("\n");
    assert!(!screen.contains("Kick") && !screen.contains("Guide"), "{screen}");
    h.close().await;
});

case!(renoises_pattern_editor_matrix_and_keyzones_draw_in_focus, async {
    work_with(DawId::Renoise);
    let c = Rc::new(Control::default());
    let grid = TrackerGrid {
        item_ref: "3:pattern:2".into(),
        name: "Drop".into(),
        lines: 64,
        from: 0,
        cursor: Some(1),
        playing: Some(0),
        lpb: Some(4.),
        tracks: vec![
            TrackerTrack {
                name: "Kick".into(),
                color: None,
                cells: vec!["C-4 01 .. .. ..".into(), "--- .. .. .. ..".into(), "C-4 01 40 .. .. 0A10".into()],
            },
            TrackerTrack {
                name: "Bass".into(),
                color: None,
                cells: vec!["E-2 02 .. .. ..".into(), "OFF .. .. .. ..".into(), "--- .. .. .. ..".into()],
            },
        ],
    };
    *c.own.borrow_mut() = Some(FocusBody::Tracker(grid));
    let h = Harness::with(120, 40, c.clone(), |_| {});
    h.start().await;
    h.connect();
    let track = json!({"name":"Bass","kind":"sequencer"});
    let pattern = json!({"track":track,"trackRef":"3:track:2","mainView":{"kind":"pattern-editor","label":"Pattern Editor"},"item":{"kind":"pattern-track","itemRef":"3:pattern:2","name":"Drop"}});
    h.emit(json!({"type":"focus","focus":pattern}));
    shows(&h, "FOCUS · Pattern").await;
    h.has("Drop · 64 lines");
    h.has("E-2 02 .. .. ..");
    h.has("OFF .. .. .. ..");
    // The focused track comes first: in a narrow pane, it's the one that fits.
    let lines = h.screen();
    let first = lines.iter().position(|line| line.contains("E-2 02")).expect("the focused track's lines");
    assert!(lines.iter().take(first).any(|line| line.trim_end().ends_with("Bass") || line.contains(" Bass ")), "{}", lines.join("\n"));

    // The pattern matrix: the sequence under each track, muted slots dimmed, sections between.
    *c.own.borrow_mut() = Some(FocusBody::Matrix(SequenceMatrix {
        tracks: vec!["Kick".into(), "ベース".into(), "Bass".into()],
        slots: vec![
            SequenceSlot { pattern: 1, section: Some("Intro".into()), muted: vec![] },
            SequenceSlot { pattern: 2, section: None, muted: vec![1] },
        ],
        current: Some(1),
        // A long sequence comes as the slots around the current one: they keep their numbers.
        first: 40,
    }));
    let matrix = json!({"track":track,"trackRef":"3:track:2","mainView":{"kind":"sequencer","label":"Pattern Matrix"},"section":{"kind":"sequence-slot","index":1.0}});
    h.emit(json!({"type":"focus","focus":matrix}));
    shows(&h, "FOCUS · Pattern Matrix").await;
    h.has("— Intro");
    h.has(" 41 ");
    h.has(" 42 ");
    // Columns by width: a wide name takes the cells it needs, and the next column stays in place.
    h.has("Kic ベ  Bas");

    // An instrument's keyzones: a map of them, keys across and velocities up, then each sample's notes, in Renoise's names
    // (MIDI 60 is C-5's octave); its phrases with their keys.
    *c.own.borrow_mut() = Some(FocusBody::Keyzones(KeyzoneMap {
        item_ref: "3:instrument:1".into(),
        name: "Drumkit".into(),
        zones: vec![
            Keyzone { sample: "Kick".into(), notes: (60, 60), velocities: (0, 127), base: 60, ..Default::default() },
            Keyzone { sample: "Snare".into(), notes: (62, 63), velocities: (64, 127), base: 62, ..Default::default() },
            Keyzone { sample: "Ghost".into(), notes: (63, 63), velocities: (0, 127), base: 63, note_off: true },
        ],
        macros: vec![("Drive".into(), 0.4)],
        phrases: vec!["Fill".into(), "Roll".into()],
        overlap: Some("cycle".into()),
        phrase_playback: Some("keymap".into()),
        phrase_keys: vec![Some((48, 59)), None],
        ..Default::default()
    }));
    let instrument = json!({"track":track,"trackRef":"3:track:2","mainView":{"kind":"instrument","label":"Instrument"},"item":{"kind":"sample","itemRef":"3:instrument:1","name":"Drumkit"}});
    h.emit(json!({"type":"focus","focus":instrument}));
    shows(&h, "FOCUS · Keyzones").await;
    h.has("Drumkit · overlapping zones: cycle");
    // (an octave of keys from G#4: the Kick at C5 and the Ghost at D#5 at every velocity; loud, the Snare's D5, and D#5 where
    // the Snare and the Ghost overlap)
    h.has("····A·B▒····");
    h.has("····A··C····");
    h.has("G#4       G5");
    h.has("A C5        Kick");
    h.has("B D5–D#5    Snare · vel 64–127");
    h.has("C D#5       Ghost · note-off");
    h.has("3 zones · 1 macros · 2 phrases");
    h.has("phrases by keymap: Fill C4–B4 · Roll");
    h.close().await;
});

// (M6) FOCUS's keyzones name notes as the session's profile does (the octave the open project names middle C in, as
// Logic's project row says it), not as the DAW's static profile.
case!(keyzones_name_notes_in_the_sessions_octave, async {
    work_with(DawId::Renoise);
    let c = Rc::new(Control::default());
    *c.profile.borrow_mut() = Some(kumi_runtime::core::daw::DawProfile::RENOISE.with_middle_c(Some(4)));
    *c.own.borrow_mut() = Some(FocusBody::Keyzones(KeyzoneMap {
        item_ref: "3:instrument:1".into(),
        name: "Drumkit".into(),
        zones: vec![Keyzone { sample: "Kick".into(), notes: (60, 60), velocities: (0, 127), base: 60, ..Default::default() }],
        ..Default::default()
    }));
    let h = Harness::with(120, 40, c.clone(), |_| {});
    h.start().await;
    h.connect();
    let track = json!({"name":"Bass","kind":"sequencer"});
    let instrument = json!({"track":track,"trackRef":"3:track:2","mainView":{"kind":"instrument","label":"Instrument"},"item":{"kind":"sample","itemRef":"3:instrument:1","name":"Drumkit"}});
    h.emit(json!({"type":"focus","focus":instrument}));
    shows(&h, "FOCUS · Keyzones").await;
    h.has("A C4        Kick");
    h.close().await;
});

/// Renoise can't say which pane has the keyboard, so FOCUS follows what changes: the pattern under the DSP frame, the
/// DSP chain for another device on the same track, the matrix when the producer moves through the sequence (never
/// when playback does), and the pattern again on another track.
#[test]
fn renoises_open_panes_show_when_the_producer_works_in_them() {
    use kumi::tui::app::{touched_next, Touched};
    use kumi_runtime::core::view::DawFocus;
    work_with(DawId::Renoise);
    let focus = |track: u32, device: &str, slot: f64, follows: bool| -> DawFocus {
        serde_json::from_value(json!({
            "track": {"name": format!("Track {track}")}, "trackRef": format!("3:track:{track}"), "device": device,
            "mainView": {"kind": "pattern-editor", "label": "Pattern Editor"},
            "editorView": {"kind": "device-chain", "label": "Track DSPs"},
            "panes": [{"kind": "sequencer", "label": "Pattern Matrix"}],
            "item": {"kind": "pattern-track", "itemRef": format!("3:pattern:{}", slot as u32 + 1)},
            "section": {"kind": "sequence-slot", "index": slot},
            "sectionFollows": follows,
        }))
        .unwrap()
    };
    let start = focus(1, "Mixer", 0., false);
    assert_eq!(touched_next(None, Some(&start), None), Some(Touched::Tracker), "the pattern, though the DSP frame is open");
    let device = focus(1, "Delay", 0., false);
    assert_eq!(touched_next(Some(&start), Some(&device), Some(Touched::Tracker)), Some(Touched::Tree), "another device, same track");
    let track = focus(2, "Mixer", 0., false);
    assert_eq!(touched_next(Some(&device), Some(&track), Some(Touched::Tree)), Some(Touched::Tracker), "another track is the tracker's");
    let slot = focus(2, "Mixer", 1., false);
    assert_eq!(touched_next(Some(&track), Some(&slot), Some(Touched::Tracker)), Some(Touched::Matrix), "moving through the sequence");
    assert_eq!(touched_next(Some(&slot), Some(&slot), Some(Touched::Matrix)), Some(Touched::Matrix), "nothing changed, nothing moves");
    let played = focus(2, "Mixer", 2., true);
    assert_eq!(
        touched_next(Some(&track), Some(&played), Some(Touched::Tracker)),
        Some(Touched::Tracker),
        "playback moved it, not the producer"
    );
    let closed = DawFocus { panes: None, ..focus(2, "Mixer", 1., false) };
    assert_eq!(touched_next(Some(&track), Some(&closed), Some(Touched::Tracker)), Some(Touched::Tracker), "no matrix open, the pattern");
}

// Renoise's lower frame shows the track DSPs most of the time: FOCUS still draws the pattern, asked for by its ref.
case!(renoises_pattern_draws_under_an_open_dsp_frame, async {
    work_with(DawId::Renoise);
    let c = Rc::new(Control::default());
    *c.own.borrow_mut() = Some(FocusBody::Tracker(TrackerGrid {
        item_ref: "55cef1:pattern:1".into(),
        name: "".into(),
        lines: 64,
        from: 0,
        cursor: Some(0),
        playing: None,
        lpb: Some(4.),
        tracks: vec![TrackerTrack { name: "Track 01".into(), color: None, cells: vec!["--- .. .. .. .. ....".into()] }],
    }));
    let h = Harness::with(120, 40, c.clone(), |_| {});
    h.start().await;
    h.connect();
    let focus = json!({"track":{"name":"Track 01","kind":"sequencer"},"trackRef":"55cef1:track:1","mainView":{"kind":"pattern-editor","label":"Pattern Editor"},"editorView":{"kind":"device-chain","label":"Track DSPs"},"item":{"kind":"pattern-track","itemRef":"55cef1:pattern:1"},"section":{"kind":"sequence-slot","index":0.0}});
    h.emit(json!({"type":"focus","focus":focus}));
    shows(&h, "FOCUS · Pattern").await;
    h.has("Pattern · 64 lines");
    assert!(h.calls().contains(&"own:Tracker { item_ref: Some(\"55cef1:pattern:1\") }".to_string()), "{:?}", h.calls());
    h.close().await;
});

// The DAW sends lines well around the cursor (16 above it): FOCUS draws the rows around the cursor, so line 40 of 64
// is on screen, not cut off below the first 16.
case!(renoises_cursor_line_stays_in_view_deep_in_a_pattern, async {
    work_with(DawId::Renoise);
    let c = Rc::new(Control::default());
    let mut cells: Vec<String> = (0..33).map(|_| "--- .. .. .. ..".to_string()).collect();
    cells[16] = "OFF 01 .. .. ..".into();
    *c.own.borrow_mut() = Some(FocusBody::Tracker(TrackerGrid {
        item_ref: "3:pattern:2".into(),
        name: "Drop".into(),
        lines: 64,
        from: 23,
        cursor: Some(39),
        playing: None,
        lpb: Some(4.),
        tracks: vec![TrackerTrack { name: "Bass".into(), color: None, cells }],
    }));
    let h = Harness::with(120, 40, c.clone(), |_| {});
    h.start().await;
    h.connect();
    let focus = json!({"track":{"name":"Bass","kind":"sequencer"},"trackRef":"3:track:2","mainView":{"kind":"pattern-editor","label":"Pattern Editor"},"item":{"kind":"pattern-track","itemRef":"3:pattern:2","name":"Drop"}});
    h.emit(json!({"type":"focus","focus":focus}));
    shows(&h, "FOCUS · Pattern").await;
    let lines = h.screen();
    let cursor = lines.iter().find(|line| line.contains(" 40 ")).unwrap_or_else(|| panic!("line 40 on screen:\n{}", lines.join("\n")));
    assert!(cursor.contains("OFF 01"), "{cursor}");
    h.close().await;
});

case!(logics_piano_roll_tracks_area_channel_strip_and_routing_draw_in_focus, async {
    work_with(DawId::Logic);
    let c = Rc::new(Control::default());
    *c.clip.borrow_mut() = Some(
        serde_json::from_value(json!({"slotRef":"1:region:1","name":"Keys","length":16.0,"notes":[
            {"pitch":60,"start":0.0,"duration":0.25,"velocity":100},{"pitch":64,"start":1.0,"duration":0.25,"velocity":96,"selected":true}
        ]}))
        .unwrap(),
    );
    let h = Harness::with(120, 40, c.clone(), |_| {});
    h.start().await;
    h.connect();
    let track = json!({"name":"Keys","color":"#5b8def","kind":"midi"});
    // The Piano Roll over the Tracks area: the region's notes.
    let piano_roll = json!({"track":track,"trackRef":"1:track:2","mainView":{"kind":"timeline","label":"Tracks"},"editorView":{"kind":"note-editor","label":"Piano Roll"},"item":{"kind":"item","itemRef":"1:region:1","name":"Keys"}});
    h.emit(json!({"type":"focus","focus":piano_roll}));
    shows(&h, "FOCUS · Piano Roll").await;
    h.has("Keys");
    h.has("4 bars · 2 notes · 1 selected");
    assert!(h.calls().contains(&"clip:1:region:1".to_string()), "{:?}", h.calls());

    // A plug-in window open: the channel strip's inserts, as the tree.
    *c.tree.borrow_mut() = Some(
        serde_json::from_value(
            json!({"trackRef":"1:track:2","devices":[{"ref":"1:plugin:2.1","name":"Alchemy"},{"ref":"1:plugin:2.2","name":"Channel EQ"}]}),
        )
        .unwrap(),
    );
    let strip = json!({"track":track,"trackRef":"1:track:2","device":"Alchemy","deviceRef":"1:plugin:2.1","mainView":{"kind":"timeline","label":"Tracks"},"editorView":{"kind":"device-chain","label":"Plug-in"}});
    h.emit(json!({"type":"focus","focus":strip}));
    shows(&h, "FOCUS · Channel Strip").await;
    h.has("Alchemy");
    h.has("Channel EQ");

    // The strip's routing: its input, its send and its output.
    *c.own.borrow_mut() = Some(FocusBody::Routing(RoutingView {
        track_ref: "1:track:2".into(),
        routes: vec![
            Route { kind: RouteKind::Receive, other: "Input 1".into(), volume_db: None, muted: false, tap: None },
            Route { kind: RouteKind::Send, other: "Bus 1".into(), volume_db: Some(-6.0), muted: false, tap: None },
            Route { kind: RouteKind::Hardware, other: "Stereo Out".into(), volume_db: None, muted: false, tap: None },
        ],
        parent_send: None,
    }));
    let routing = json!({"track":track,"trackRef":"1:track:2","mainView":{"kind":"timeline","label":"Tracks"},"editorView":{"kind":"routing","label":"Routing"}});
    h.emit(json!({"type":"focus","focus":routing}));
    shows(&h, "FOCUS · Routing").await;
    h.has("← Input 1");
    h.has("→ Bus 1 · -6 dB");
    h.has("⇥ Stereo Out");

    // The Tracks area: each track's regions as bars.
    *c.arrangement.borrow_mut() = Some(
        serde_json::from_value(
            json!({"length":32.0,"position":0.0,"playing":false,"locators":[{"name":"Chorus","position":32.0}],"beatsPerBar":4.0,"track_count":2,"lanes":[
                {"name":"Vocal","depth":0,"items":[]},
                {"name":"Keys","depth":0,"items":[{"start":0.0,"length":16.0,"name":"Keys","selected":true}]}
            ]}),
        )
        .unwrap(),
    );
    let tracks = json!({"track":track,"trackRef":"1:track:2","mainView":{"kind":"timeline","label":"Tracks"}});
    h.emit(json!({"type":"focus","focus":tracks}));
    shows(&h, "FOCUS · Tracks").await;
    h.has("Vocal");
    h.has("Keys");
    h.has("▬▬");
    h.close().await;
});
