//! Renoise, through Kumi's tool and host: its refs (positions, as Renoise numbers them, under the song's epoch), its
//! read tool and its instructions. The integration itself is the one every bridge of Kumi's own shares (`own_bridge`).
use super::own_bridge::{create_own_integration, OwnDaw, OwnOptions};
use super::own_views::{self, body_args, focus_track, selected, view};
use crate::core::{
    contracts::{Integration, JsonObject},
    daw::DawProfile,
    presentation::{ToolActivity, ToolPresentation},
    refs::{moved_index, Moved, RefScheme, Shift},
    view::{BodyKind, BodyRequest, DawFocus, FocusBody, FocusItem, FocusSection, ItemKind, SectionKind, ViewKind, ViewPresentation},
};
use crate::mcp::client::BridgeDescriptor;
use regex::Regex;
use serde_json::Value;
use std::{rc::Rc, sync::LazyLock};

/// The instructions Renoise's turns carry (M7 brings the full instruction pack).
const INSTRUCTIONS: &str = "You work in Renoise through Kumi's tool. Read the song with renoise_read before you describe it: its tracks (sequencer, group, send and master), the pattern sequence and its patterns, a pattern's lines in Renoise's own notation, instruments with their samples, keyzones and phrases, and each track's devices and their parameters. A ref such as track:3 is this turn's: pass it back as parent (a pattern for its lines, with track) to read what's inside.";

pub const DAW: OwnDaw = OwnDaw {
    profile: &DawProfile::RENOISE,
    descriptor: BridgeDescriptor::RENOISE,
    host_flag: "renoise",
    instructions: INSTRUCTIONS,
    read_tool: "renoise_read",
    read_description: "Read Renoise: the song, its tracks, and what's in them (the sequence, patterns and their lines, instruments, samples, phrases, devices and parameters; an instrument's modulation sets and sample FX chains, and the parameters of their devices and of a plug-in instrument), a page at a time. Rows carry refs to pass back as parent; a page with nextCursor has more. A track's groupRef is the group it's in; pan runs -1 to 1. Lines list only lines with something on them, numbered from 1. A note column: note (C-4, OFF), instrument (hex, 00 the first), volume (hex 00-80, 80 full, or a column effect), panning (hex 00-80, 40 centre), delay (hex 00-FF, 256ths of a line), fx (the column's own effect: two characters, then a hex amount). effects are the line's effect columns: number and amount (0A and 20 is 0A20). A pattern is lines long, at the song's lines per beat. A pattern's automation (parent the pattern, with track) lists each automated parameter's points: time in lines from 1, value 0 to 1 of the parameter's range.",
    read_kinds: &["song", "track", "sequence", "pattern", "line", "automation", "instrument", "sample", "phrase", "device", "param", "modset", "chain"],
    project_kind: "song",
    fill_focus,
    bodies: &[BodyKind::Tree, BodyKind::Tracker, BodyKind::Matrix, BodyKind::Keyzones],
    body_request,
    to_body,
    changes: &super::renoise_changes::CHANGES,
};

/// What the producer looks at in Renoise: the middle frame (the pattern or phrase editor, an instrument, the mixer),
/// with the track DSPs below it and the pattern matrix beside it when they're open; the selected track, pattern (or
/// phrase, or instrument), sequence slot and device. Renoise can't say which pane has the keyboard, so FOCUS picks by
/// what changes.
fn fill_focus(focus: &Value) -> DawFocus {
    let selection = focus.get("selection").cloned().unwrap_or(Value::Null);
    let shown = focus.get("view").and_then(Value::as_str).unwrap_or("pattern");
    let open = |key: &str| focus.get(key) == Some(&Value::Bool(true));
    let transport = focus.get("transport").cloned().unwrap_or(Value::Null);
    let main = match shown {
        "phrase" => view(ViewKind::PatternEditor, "Phrase Editor"),
        "instrument" => view(ViewKind::Instrument, "Instrument"),
        "sample" => view(ViewKind::Instrument, "Sample Editor"),
        "mixer" => view(ViewKind::Mixer, "Mixer"),
        _ => view(ViewKind::PatternEditor, "Pattern Editor"),
    };
    let name = |key: &str| selection.get(key).and_then(Value::as_str).filter(|name| !name.is_empty()).map(str::to_owned);
    let item = match shown {
        "phrase" => {
            selected(&selection, "phraseRef").map(|reference| FocusItem { kind: ItemKind::Phrase, item_ref: Some(reference), name: None })
        }
        "instrument" | "sample" => selected(&selection, "instrumentRef").map(|reference| FocusItem {
            kind: ItemKind::Sample,
            item_ref: Some(reference),
            name: name("instrumentName"),
        }),
        _ => selected(&selection, "patternRef").map(|reference| FocusItem {
            kind: ItemKind::PatternTrack,
            item_ref: Some(reference),
            name: name("patternName"),
        }),
    };
    let section = selection.get("sequence").and_then(Value::as_f64).filter(|n| *n >= 1.).map(|n| FocusSection {
        kind: SectionKind::SequenceSlot,
        index: Some(n - 1.),
        name: name("section"),
    });
    DawFocus {
        track: focus_track(&selection),
        track_ref: selected(&selection, "trackRef"),
        device_ref: selected(&selection, "deviceRef"),
        device: name("deviceName"),
        main_view: Some(main),
        editor_view: open("dspVisible").then(|| view(ViewKind::DeviceChain, "Track DSPs")),
        item,
        section,
        panes: open("matrixVisible").then(|| vec![view(ViewKind::Sequencer, "Pattern Matrix")]),
        section_follows: (transport.get("playing") == Some(&Value::Bool(true))
            && transport.get("followPlayer") == Some(&Value::Bool(true)))
        .then_some(true),
        ..Default::default()
    }
}

fn body_request(request: &BodyRequest) -> Option<JsonObject> {
    Some(match request {
        BodyRequest::Tree { track_ref } => body_args("dsp", Some(track_ref)),
        BodyRequest::Tracker { item_ref } => {
            let phrase = item_ref.as_deref().and_then(|r| RenoiseRefs.kind_of(r)) == Some("phrase");
            body_args(if phrase { "phrase" } else { "pattern" }, item_ref.as_deref())
        }
        BodyRequest::Matrix => body_args("matrix", None),
        BodyRequest::Keyzones { item_ref } => body_args("instrument", Some(item_ref)),
        _ => return None,
    })
}

fn to_body(request: &BodyRequest, body: &Value) -> Option<FocusBody> {
    Some(match request {
        BodyRequest::Tree { track_ref } => FocusBody::Tree(own_views::tree(track_ref, body)),
        BodyRequest::Tracker { item_ref } => {
            let mut grid = own_views::tracker(body);
            // A phrase: the phrase editor's own words, and how it plays.
            if item_ref.as_deref().and_then(|r| RenoiseRefs.kind_of(r)) == Some("phrase") || body.get("instrumentRef").is_some() {
                grid.heading = Some("Phrase".into());
                if let Some(lpb) = body.get("lpb").and_then(Value::as_f64) {
                    grid.facts.push(format!("LPB {lpb}"));
                }
                match body.get("keyTracking").and_then(Value::as_str) {
                    Some("none") => grid.facts.push("no key tracking".into()),
                    Some(tracking) => grid.facts.push(format!("key tracking: {tracking}")),
                    None => {}
                }
                if body.get("looping") == Some(&Value::Bool(true)) {
                    grid.facts.push("loops".into());
                }
            }
            FocusBody::Tracker(grid)
        }
        BodyRequest::Matrix => FocusBody::Matrix(own_views::matrix(body)),
        BodyRequest::Keyzones { item_ref } => FocusBody::Keyzones(own_views::keyzones(item_ref, body)),
        _ => return None,
    })
}

/// Renoise's refs: `<epoch>:<kind>:<numbers>`, numbered as Renoise shows them (from 1). A track inserted or deleted
/// moves the tracks after it, and the devices and parameters on them.
#[derive(Default)]
pub struct RenoiseRefs;

/// A ref's parts: its epoch, kind and the numbers after it.
fn parts(reference: &str) -> Option<(&str, &str, Vec<usize>)> {
    let mut parts = reference.splitn(3, ':');
    let (epoch, kind, numbers) = (parts.next()?, parts.next()?, parts.next()?);
    if epoch.is_empty() || kind.is_empty() {
        return None;
    }
    let numbers: Option<Vec<usize>> = numbers.split('.').map(|n| n.parse().ok()).collect();
    Some((epoch, kind, numbers?))
}

impl RefScheme for RenoiseRefs {
    fn positional(&self) -> bool {
        true
    }
    fn kind_of<'a>(&self, reference: &'a str) -> Option<&'a str> {
        parts(reference).map(|(_, kind, _)| kind)
    }
    fn moved(&self, shift: &Shift, reference: &str) -> Moved {
        let Some((epoch, kind, mut numbers)) = parts(reference) else { return Moved::Same };
        // Which of its numbers is a track's: the first, but a pattern track's second (pattern.track).
        let at = match kind {
            "track" | "device" | "param" | "slot" => 0,
            "ptrack" => 1,
            _ => return Moved::Same,
        };
        if numbers.len() <= at {
            return Moved::Same;
        }
        match moved_index(numbers[at], &shift.tracks_gone, &shift.tracks_made) {
            Some(now) if now == numbers[at] => Moved::Same,
            Some(now) => {
                numbers[at] = now;
                let numbers: Vec<String> = numbers.iter().map(usize::to_string).collect();
                Moved::To(format!("{epoch}:{kind}:{}", numbers.join(".")))
            }
            None => Moved::Gone,
        }
    }
    fn place_of(&self, reference: &str) -> String {
        reference.into()
    }
    fn refs_in<'a>(&self, text: &'a str) -> Vec<&'a str> {
        static REF: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[0-9A-Za-z]+:[a-z]+:[0-9]+(?:\.[0-9]+)*").unwrap());
        REF.find_iter(text).map(|found| found.as_str()).collect()
    }
    fn reference_fields(&self) -> &[String] {
        &[]
    }
    fn parents(&self, kind: &str) -> Option<Vec<String>> {
        let parents: &[&str] = match kind {
            "line" => &["pattern"],
            "sample" | "phrase" | "modset" | "chain" => &["instrument"],
            "device" => &["track"],
            // (a track's device; a sample FX chain's device, or a plug-in instrument: n-instruments)
            "param" => &["device", "chaindevice", "instrument"],
            _ => return None,
        };
        Some(parents.iter().map(|parent| parent.to_string()).collect())
    }
    fn parent_hint(&self, kind: &str, _parent: &str) -> &'static str {
        match kind {
            "line" => "read a pattern's lines with its ref as parent and the track as track",
            _ => "",
        }
    }
    fn row_refs(&self, _kind: &str, _row: &JsonObject) -> Vec<(String, String, bool)> {
        vec![]
    }
    fn track_of(&self, reference: &str) -> Option<String> {
        let (epoch, kind, numbers) = parts(reference)?;
        match kind {
            "track" | "device" | "param" => Some(format!("{epoch}:track:{}", numbers.first()?)),
            _ => None,
        }
    }
}

/// Renoise's tools as the app shows them.
pub const PRESENTATION: ToolPresentation = ToolPresentation {
    label: |tool| match tool {
        "renoise_read" => Some("looked at your song"),
        _ => None,
    },
    doing: |tool| match tool {
        "renoise_read" => Some("looking at your song"),
        _ => None,
    },
    activity: |tool| (tool == "renoise_read").then_some(ToolActivity::Read),
    plan: |tool| tool == "make_changes",
    acts: |_| false,
    read_prefix: Some("renoise_"),
};

/// How the app reads Renoise's focus and devices: the focus comes neutral.
pub const VIEW: ViewPresentation = ViewPresentation {
    heading: |kind| match kind {
        BodyKind::Tree => "DSP",
        BodyKind::Tracker => "Pattern",
        BodyKind::Matrix => "Pattern Matrix",
        BodyKind::Keyzones => "Keyzones",
        other => (ViewPresentation::NONE.heading)(other),
    },
    open_containers: true,
    ..ViewPresentation::NONE
};

/// Renoise's integration.
pub fn create_renoise_integration(options: OwnOptions) -> Rc<dyn Integration> {
    create_own_integration::<RenoiseRefs>(&DAW, options)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_track_inserted_moves_the_tracks_after_it_with_their_devices_and_parameters() {
        let refs = RenoiseRefs;
        let shift = Shift { tracks_made: vec![2], ..Default::default() };
        assert_eq!(refs.moved(&shift, "7:track:1"), Moved::Same);
        assert_eq!(refs.moved(&shift, "7:track:2"), Moved::To("7:track:3".into()));
        assert_eq!(refs.moved(&shift, "7:device:4.2"), Moved::To("7:device:5.2".into()));
        assert_eq!(refs.moved(&shift, "7:param:2.1.9"), Moved::To("7:param:3.1.9".into()));
        assert_eq!(refs.moved(&shift, "7:pattern:2"), Moved::Same);
        let gone = Shift { tracks_gone: vec![2], ..Default::default() };
        assert_eq!(refs.moved(&gone, "7:device:2.1"), Moved::Gone);
        assert_eq!(refs.track_of("7:param:3.1.9"), Some("7:track:3".into()));
        assert_eq!(refs.kind_of("7:sample:2.4"), Some("sample"));
        assert_eq!(refs.kind_of("track:3"), None);
    }
}
