//! FOCUS draws REAPER's and Renoise's views as the producer moves between them: REAPER's MIDI editor shows the take's
//! notes, its arrange view each track's items, a track's routing its sends; Renoise's pattern editor shows the lines
//! around the cursor, its matrix the sequence, an instrument its keyzones.
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
            json!({"length":64.0,"position":8.0,"playing":true,"locators":[{"name":"Verse","position":0.0}],"beatsPerBar":4.0,"lanes":[
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
        tracks: vec!["Kick".into(), "Bass".into()],
        slots: vec![
            SequenceSlot { pattern: 1, section: Some("Intro".into()), muted: vec![] },
            SequenceSlot { pattern: 2, section: None, muted: vec![1] },
        ],
        current: Some(1),
    }));
    let matrix = json!({"track":track,"trackRef":"3:track:2","mainView":{"kind":"sequencer","label":"Pattern Matrix"},"section":{"kind":"sequence-slot","index":1.0}});
    h.emit(json!({"type":"focus","focus":matrix}));
    shows(&h, "FOCUS · Pattern Matrix").await;
    h.has("— Intro");
    h.has("Kic Bas");

    // An instrument's keyzones: each sample's notes, in Renoise's names (MIDI 60 is C-5's octave).
    *c.own.borrow_mut() = Some(FocusBody::Keyzones(KeyzoneMap {
        item_ref: "3:instrument:1".into(),
        name: "Drumkit".into(),
        zones: vec![
            Keyzone { sample: "Kick".into(), notes: (60, 60), velocities: (0, 127), base: 60 },
            Keyzone { sample: "Snare".into(), notes: (62, 63), velocities: (64, 127), base: 62 },
        ],
        macros: vec![("Drive".into(), 0.4)],
        phrases: vec![],
    }));
    let instrument = json!({"track":track,"trackRef":"3:track:2","mainView":{"kind":"instrument","label":"Instrument"},"item":{"kind":"sample","itemRef":"3:instrument:1","name":"Drumkit"}});
    h.emit(json!({"type":"focus","focus":instrument}));
    shows(&h, "FOCUS · Keyzones").await;
    h.has("C5        Kick");
    h.has("D5–D#5    Snare · vel 64–127");
    h.has("2 zones · 1 macros");
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
