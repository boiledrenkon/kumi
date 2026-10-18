//! FOCUS's bodies from Kumi's host's `focus_body` answers (the M2 contract, §5), the same for every DAW whose bridge is
//! Kumi's own: a device tree from nodes, a take's notes, the timeline with its lanes, a tracker grid, the sequence, an
//! instrument's keyzones, a track's routing.
use crate::core::{
    contracts::{ClipNote, JsonObject},
    view::*,
};
use serde_json::Value;

fn text(value: &Value, key: &str) -> String {
    value.get(key).and_then(Value::as_str).unwrap_or_default().to_owned()
}
fn number(value: &Value, key: &str) -> Option<f64> {
    value.get(key).and_then(Value::as_f64).filter(|n| n.is_finite())
}
fn flag(value: &Value, key: &str) -> bool {
    value.get(key) == Some(&Value::Bool(true))
}
fn list<'a>(value: &'a Value, key: &str) -> impl Iterator<Item = &'a Value> {
    value.get(key).and_then(Value::as_array).into_iter().flatten()
}
fn color(value: &Value) -> Option<String> {
    value.get("color").and_then(Value::as_str).filter(|c| c.starts_with('#')).map(str::to_owned)
}

/// A device tree from `nodes` (REAPER's FX chain, Renoise's DSP chain): a container's children are one chain each
/// branch runs (a parallel node starts a new one).
pub fn tree(track_ref: &str, body: &Value) -> DeviceTree {
    fn node(value: &Value) -> DeviceNode {
        let container = flag(value, "container");
        let mut chains: Vec<ChainNode> = vec![];
        for child in list(value, "childNodes") {
            if chains.is_empty() || flag(child, "parallel") {
                chains.push(ChainNode {
                    r#ref: format!("{}#{}", text(value, "ref"), chains.len() + 1),
                    name: String::new(),
                    devices: Some(vec![]),
                });
            }
            if let Some(devices) = chains.last_mut().and_then(|chain| chain.devices.as_mut()) {
                devices.push(node(child));
            }
        }
        let form = if container {
            DeviceForm::Container
        } else if flag(value, "plugin") {
            DeviceForm::Plugin
        } else {
            DeviceForm::Native
        };
        DeviceNode {
            r#ref: text(value, "ref"),
            name: text(value, "name"),
            class_name: None,
            can_have_chains: container.then_some(true),
            can_have_drum_pads: None,
            device_type: None,
            chains: container.then_some(chains),
            role: Some(DeviceRole { signal: DeviceSignal::Unknown, form }),
        }
    }
    DeviceTree { track_ref: track_ref.into(), devices: list(body, "nodes").map(node).collect() }
}

/// A take's notes (REAPER's MIDI editor), in beats from the item's start.
pub fn notes(item_ref: &str, body: &Value) -> NoteView {
    let notes = list(body, "notes")
        .take(512)
        .filter_map(|note| {
            Some(ClipViewNote {
                note: ClipNote {
                    pitch: number(note, "pitch")?,
                    start: number(note, "start")?,
                    duration: number(note, "length")?,
                    velocity: number(note, "velocity").unwrap_or(100.),
                },
                selected: flag(note, "selected").then_some(true),
            })
        })
        .collect();
    ClipView {
        slot_ref: item_ref.into(),
        name: text(body, "name"),
        length: number(body, "length").unwrap_or(0.),
        notes,
        beats_per_bar: number(body, "beatsPerBar"),
    }
}

/// The timeline with each track's items as bars (REAPER's arrange view), in beats.
pub fn timeline(body: &Value) -> TimelineStrip {
    let lanes = list(body, "tracks")
        .map(|track| TimelineLane {
            name: text(track, "name"),
            color: color(track),
            depth: number(track, "depth").unwrap_or(0.).max(0.) as u32,
            items: list(track, "items")
                .filter_map(|item| {
                    Some(TimelineItem {
                        start: number(item, "position")?,
                        length: number(item, "length")?,
                        name: text(item, "name"),
                        muted: flag(item, "muted"),
                        selected: flag(item, "selected"),
                    })
                })
                .collect(),
        })
        .collect::<Vec<_>>();
    let mut locators: Vec<Locator> = list(body, "markers")
        .chain(list(body, "regions"))
        .filter_map(|mark| Some(Locator { name: text(mark, "name"), position: number(mark, "position")? }))
        .collect();
    locators.sort_by(|a, b| a.position.total_cmp(&b.position));
    let r#loop = body.get("loop").and_then(|span| {
        let (start, end) = (number(span, "start")?, number(span, "end")?);
        (end > start).then(|| ArrangementLoop { start, length: end - start, enabled: flag(span, "on") })
    });
    TimelineStrip {
        length: number(body, "length").unwrap_or(0.),
        position: number(body, "playhead").unwrap_or(0.),
        playing: flag(body, "playing"),
        r#loop,
        locators,
        beats_per_bar: number(body, "beatsPerBar"),
        lanes: Some(lanes),
        track_count: number(body, "trackCount").map(|n| n.max(0.) as usize),
    }
}

/// A tracker track's columns: its note columns (with a sample effect column when any note has one) and its effect
/// columns.
struct Columns {
    notes: usize,
    fx: bool,
    effects: usize,
}

/// The column a note or effect sits in (from 1), or its place in the list when it doesn't say.
fn column(value: &Value, at: usize) -> usize {
    number(value, "col").filter(|n| *n >= 1.).map_or(at + 1, |n| n as usize)
}

/// One line of a tracker track as the DAW writes it: each note column ("C-4 01 80 .. .."), then each effect column
/// ("0A10"), in the DAW's own text; an empty line or column is drawn as the DAW draws it ("--- .. .. .. ..", "....").
fn tracker_cell(line: Option<&Value>, columns: &Columns) -> String {
    let field = |value: Option<&Value>, key: &str, empty: &str| {
        value.and_then(|value| value.get(key)).and_then(Value::as_str).filter(|s| !s.is_empty()).unwrap_or(empty).to_owned()
    };
    let at = |key: &str, col: usize| {
        line.and_then(|line| line.get(key))
            .and_then(Value::as_array)
            .and_then(|items| items.iter().enumerate().find(|(index, item)| column(item, *index) == col).map(|(_, item)| item))
    };
    let mut cells: Vec<String> = (1..=columns.notes)
        .map(|col| {
            let note = at("notes", col);
            let mut fields = vec![
                field(note, "note", "---"),
                field(note, "instrument", ".."),
                field(note, "volume", ".."),
                field(note, "panning", ".."),
                field(note, "delay", ".."),
            ];
            if columns.fx {
                fields.push(field(note, "fx", "...."));
            }
            fields.join(" ")
        })
        .collect();
    cells.extend((1..=columns.effects).map(|col| {
        let effect = at("effects", col);
        format!("{}{}", field(effect, "number", ".."), field(effect, "amount", ".."))
    }));
    cells.join(" ")
}

/// A tracker's pattern or phrase around its cursor (Renoise), lines counted from 1 as the DAW shows them. The DAW
/// sends only the lines with something in them; every line of the window is drawn.
pub fn tracker(body: &Value) -> TrackerGrid {
    let from = number(body, "from").unwrap_or(1.).max(1.) as usize;
    let lines = number(body, "lines").unwrap_or(0.).max(0.) as usize;
    let line = |key: &str| number(body, key).filter(|n| *n >= 1.).map(|n| n as usize - 1);
    let listed = |track: &Value| list(track, "lines").filter_map(|row| number(row, "line")).map(|n| n as usize).max().unwrap_or(0);
    let last = body
        .get("window")
        .and_then(|window| number(window, "to"))
        .map(|to| to as usize)
        .unwrap_or_else(|| list(body, "tracks").map(listed).max().unwrap_or(0));
    let last = if lines > 0 { last.min(lines) } else { last };
    TrackerGrid {
        item_ref: text(body, "ref"),
        name: text(body, "name"),
        lines,
        from: from - 1,
        cursor: line("cursor"),
        playing: line("playing"),
        lpb: number(body, "lpb"),
        tracks: list(body, "tracks")
            .map(|track| {
                let rows: Vec<&Value> = list(track, "lines").collect();
                let widest = |key: &str| {
                    rows.iter().flat_map(|row| list(row, key).enumerate().map(|(index, item)| column(item, index))).max().unwrap_or(0)
                };
                let columns = Columns {
                    notes: (number(track, "noteColumns").unwrap_or(1.).max(0.) as usize).max(widest("notes")),
                    fx: rows
                        .iter()
                        .any(|row| list(row, "notes").any(|note| note.get("fx").and_then(Value::as_str).is_some_and(|fx| !fx.is_empty()))),
                    effects: (number(track, "effectColumns").unwrap_or(0.).max(0.) as usize).max(widest("effects")),
                };
                let at = |n: usize| rows.iter().copied().find(|row| number(row, "line") == Some(n as f64));
                TrackerTrack {
                    name: text(track, "name"),
                    color: color(track),
                    cells: (from..=last).map(|n| tracker_cell(at(n), &columns)).collect(),
                }
            })
            .collect(),
    }
}

/// A tracker's sequence (Renoise's pattern matrix).
pub fn matrix(body: &Value) -> SequenceMatrix {
    SequenceMatrix {
        tracks: list(body, "tracks").filter_map(Value::as_str).map(str::to_owned).collect(),
        slots: list(body, "slots")
            .map(|slot| SequenceSlot {
                pattern: number(slot, "pattern").unwrap_or(0.).max(0.) as usize,
                section: slot.get("section").and_then(Value::as_str).filter(|s| !s.is_empty()).map(str::to_owned),
                muted: list(slot, "mutedTracks").filter_map(Value::as_f64).filter(|n| *n >= 1.).map(|n| n as usize - 1).collect(),
            })
            .collect(),
        current: number(body, "current").filter(|n| *n >= 1.).map(|n| n as usize - 1),
    }
}

/// An instrument's samples on the keyboard (Renoise's keyzones).
pub fn keyzones(item_ref: &str, body: &Value) -> KeyzoneMap {
    let pair = |value: &Value, key: &str, (lo, hi): (u8, u8)| -> (u8, u8) {
        let range: Vec<u8> = list(value, key).filter_map(Value::as_f64).map(|n| n.clamp(0., 127.) as u8).collect();
        match range.as_slice() {
            [a, b] => (*a.min(b), *a.max(b)),
            _ => (lo, hi),
        }
    };
    KeyzoneMap {
        item_ref: item_ref.into(),
        name: text(body, "name"),
        zones: list(body, "keyzones")
            .map(|zone| Keyzone {
                sample: text(zone, "sample"),
                notes: pair(zone, "noteRange", (0, 119)),
                velocities: pair(zone, "velocityRange", (0, 127)),
                base: number(zone, "baseNote").unwrap_or(48.).clamp(0., 127.) as u8,
            })
            .collect(),
        macros: list(body, "macros").filter_map(|m| Some((text(m, "name"), number(m, "value")?))).collect(),
        phrases: list(body, "phrases").filter_map(Value::as_str).map(str::to_owned).collect(),
    }
}

/// A track's sends and receives (REAPER's routing).
pub fn routing(track_ref: &str, body: &Value) -> RoutingView {
    let routes = list(body, "routes")
        .map(|route| {
            let category = number(route, "category").unwrap_or(0.);
            let kind = if category < 0. {
                RouteKind::Receive
            } else if category > 0. {
                RouteKind::Hardware
            } else {
                RouteKind::Send
            };
            let tap = number(route, "mode").and_then(|mode| match mode as i64 {
                0 => Some("post-fader".to_owned()),
                1 => Some("pre-FX".to_owned()),
                3 => Some("pre-fader".to_owned()),
                _ => None,
            });
            Route { kind, other: text(route, "otherName"), volume_db: number(route, "volumeDb"), muted: flag(route, "mute"), tap }
        })
        .collect();
    RoutingView { track_ref: track_ref.into(), routes, parent_send: body.get("parentSend").and_then(Value::as_bool) }
}

/// The track kind FOCUS's chip shows, from a focus's `track.type`.
pub fn track_kind(kind: &str) -> Option<TrackKind> {
    Some(match kind {
        "midi" => TrackKind::Midi,
        "audio" => TrackKind::Audio,
        "folder" | "group" => TrackKind::Group,
        "master" => TrackKind::Main,
        "send" => TrackKind::Return,
        "sequencer" => TrackKind::Sequencer,
        _ => return None,
    })
}

/// The track a focus names, with its chip.
pub fn focus_track(selection: &Value) -> Option<FocusTrack> {
    let track = selection.get("track")?;
    Some(FocusTrack {
        name: text(track, "name"),
        color: color(track),
        kind: track.get("type").and_then(Value::as_str).and_then(track_kind),
    })
}

/// A view by its kind and the DAW's word for it.
pub fn view(kind: ViewKind, label: &str) -> FocusView {
    FocusView { kind, label: label.into() }
}

/// The selection's refs, by their contract keys.
pub fn selected(selection: &Value, key: &str) -> Option<String> {
    selection.get(key).and_then(Value::as_str).filter(|s| !s.is_empty()).map(str::to_owned)
}

/// The transport a focus carries, for FOCUS's beat light: `at` is when it was read (performance time).
pub fn transport(focus: &Value, at: f64) -> Option<Transport> {
    let transport = focus.get("transport")?;
    Some(Transport {
        playing: flag(transport, "playing"),
        tempo: number(transport, "tempo"),
        beat: number(transport, "beat"),
        at,
        beats_per_bar: number(transport, "beatsPerBar"),
    })
}

/// A body's request as the host's `focus_body` arguments.
pub fn body_args(kind: &str, reference: Option<&str>) -> JsonObject {
    let mut args = JsonObject::new();
    args.insert("kind".into(), kind.into());
    if let Some(reference) = reference {
        args.insert("ref".into(), reference.into());
    }
    args
}
