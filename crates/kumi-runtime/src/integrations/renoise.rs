//! Renoise, through Kumi's tool and host: its refs (positions, as Renoise numbers them, under the song's epoch), its
//! read tool and its instructions. The integration itself is the one every bridge of Kumi's own shares (`own_bridge`).
use super::own_bridge::{create_own_integration, OwnDaw, OwnOptions};
use super::own_catchup::{as_read, CatchUpWords, Mixer, RecipeKinds};
use super::own_phrases::{bar_span, slots_spanning, track, OwnPhrases, PhraseLook, Phrased};
use super::own_views::{self, body_args, focus_track, selected, view};
use crate::core::{
    contracts::{Integration, JsonObject},
    daw::DawProfile,
    describe::ProjectWords,
    phrases::{Intent, Switch},
    presentation::{ToolActivity, ToolPresentation},
    refs::{moved_index, Moved, RefScheme, Shift},
    view::{BodyKind, BodyRequest, DawFocus, FocusBody, FocusItem, FocusSection, ItemKind, SectionKind, ViewKind, ViewPresentation},
};
use crate::mcp::client::BridgeDescriptor;
use regex::Regex;
use serde_json::{json, Value};
use std::{rc::Rc, sync::LazyLock};

/// The instructions Renoise's turns carry (M7 brings the full instruction pack).
const INSTRUCTIONS: &str = "You work in Renoise through Kumi's tool. Read the song with renoise_read before you describe it: its tracks (sequencer, group, send and master), the pattern sequence and its patterns, a pattern's lines in Renoise's own notation, instruments with their samples, keyzones and phrases, and each track's devices and their parameters. A ref such as track:3 is this turn's name for something, not Renoise's number for it: pass it back as parent (a pattern for its lines, with track) to read what's inside. Numbers are Renoise's own: lines, patterns, instruments and samples from 0 (00 the first), tracks, phrases and sequence slots from 1.";

pub const DAW: OwnDaw = OwnDaw {
    profile: &DawProfile::RENOISE,
    descriptor: BridgeDescriptor::RENOISE,
    host_flag: "renoise",
    instructions: INSTRUCTIONS,
    read_tool: "renoise_read",
    read_description: "Read Renoise: the song, its tracks, and what's in them (the sequence, patterns and their lines, instruments, samples, phrases, devices and parameters; an instrument's modulation sets and sample FX chains, and the parameters of their devices and of a plug-in instrument), a page at a time. Rows carry refs to pass back as parent; a page with nextCursor has more. A track's groupRef is the group it's in; pan runs -1 to 1. A pattern's number, an instrument's and a sample's index and a line's number count from 0, as Renoise shows them (instruments and samples in hex: index 10 is 0A); tracks, phrases and sequence slots from 1. Lines list only lines with something on them. A note column: note (C-4, OFF), instrument (hex, 00 the first), volume (hex 00-80, 80 full, or a column effect), panning (hex 00-80, 40 centre), delay (hex 00-FF, 256ths of a line), fx (the column's own effect: two characters, then a hex amount). effects are the line's effect columns: number and amount (0A and 20 is 0A20). A pattern is lines long, at the song's lines per beat. A pattern's automation (parent the pattern, with track) lists each automated parameter's points: time in lines from 0, value 0 to 1 of the parameter's range.",
    read_kinds: &["song", "track", "sequence", "pattern", "line", "automation", "instrument", "sample", "phrase", "device", "param", "modset", "chain"],
    project_kind: "song",
    fill_focus,
    bodies: &[BodyKind::Tree, BodyKind::Tracker, BodyKind::Matrix, BodyKind::Keyzones, BodyKind::Mixer],
    body_request,
    to_body,
    changes: &super::renoise_changes::CHANGES,
    catch_up: &CATCH_UP,
    phrases: &PHRASES,
};

/// What changed in a Renoise song, in Renoise's words: catch-up's lines, watch_me's changes and its draft recipe.
pub const CATCH_UP: CatchUpWords = CatchUpWords {
    words: ProjectWords {
        kinds: &[("track", "tracks"), ("device", "devices"), ("pattern", "patterns"), ("instrument", "instruments")],
        project: "song",
        rank: &["song", "track", "device", "pattern", "instrument"],
        opaque: &["device", "instrument"],
    },
    facts: song_facts,
    recipe: RecipeKinds {
        track_add: "track_add",
        effect: "device",
        effect_add: |made, _| {
            let path = made.get("path").and_then(Value::as_str).filter(|path| !path.is_empty())?;
            Some(("device_add", serde_json::json!({"path": path}).as_object().cloned().unwrap_or_default()))
        },
        effect_values: |values| {
            let values: Vec<Value> = values.iter().map(|(name, value)| serde_json::json!({"param": name, "value": value})).collect();
            ("device_params", serde_json::json!({"values": values}).as_object().cloned().unwrap_or_default())
        },
        mixer: &[
            Mixer { what: "mixer.volume", tool: "track_volume", field: "volumeDb", value: as_read },
            Mixer { what: "mixer.pan", tool: "track_pan", field: "pan", value: as_read },
            Mixer { what: "mixer.width", tool: "track_width", field: "width", value: as_read },
            // (Renoise's mute states: active plays, muted and off don't)
            Mixer { what: "mixer.mute", tool: "track_mute", field: "mute", value: |state| state.as_str().map(|state| Value::Bool(state != "active")) },
            Mixer { what: "mixer.solo", tool: "track_solo", field: "solo", value: as_read },
        ],
    },
    watch_description: "Learn a routine the producer does by hand in Renoise: action start just before they do it (Kumi notes the song as it is; nothing in Renoise changes), then action stop when they say they're done. Stop gives what changed: tracks added (with their mixer and routing), devices added (with the parameters they moved after adding them), patterns' lines, instruments and settings changed, each with its track; and recipeSteps, a draft of the steps that would redo it in your change kinds, with $blanks for the tracks they worked on (the one they selected is $track). Save it at once with save_recipe from those steps, fixing the draft where it's off and leaving out anything clearly unrelated. Then say in a few words what the recipe does. The producer can ask you to change it.",
};

/// The song's own values that changed, as catch-up lines.
fn song_facts(details: &[Value]) -> Vec<String> {
    let text = |value: &Value| kumi_common::js::json::stringify(value);
    let quoted = |value: &Value| format!("“{}”", value.as_str().unwrap_or(""));
    let mut lines = vec![];
    for detail in details {
        let (from, to) = (&detail["from"], &detail["to"]);
        match detail["what"].as_str().unwrap_or("") {
            "bpm" => lines.push(format!("Tempo {} → {} BPM", text(from), text(to))),
            "lpb" => lines.push(format!("Lines per beat {} → {}", text(from), text(to))),
            "tpl" => lines.push(format!("Ticks per line {} → {}", text(from), text(to))),
            "name" => lines.push(format!("Renamed the song {} → {}", quoted(from), quoted(to))),
            "sequence.slots" => lines.push(format!("The pattern sequence has {} → {} slots", text(from), text(to))),
            what if what.starts_with("sequence.") => lines.push("Changed the pattern sequence".into()),
            _ => {}
        }
    }
    // (more slots also changes the sequence's fingerprint: told once)
    if lines.iter().any(|line| line.starts_with("The pattern sequence has")) {
        lines.retain(|line| line != "Changed the pattern sequence");
    }
    lines.dedup();
    lines
}

/// Renoise's fixed phrases: the song's tempo, a track's mute and solo, the loop over whole slots of the sequence (with
/// the pattern loop off) and the metronome. Renoise arms no tracks.
const PHRASES: OwnPhrases = OwnPhrases { plan: phrase, loops_by_slot: true };

fn phrase(intent: &Intent, look: &PhraseLook) -> Phrased {
    let step = |tool: &str, input: Value| json!({"tool": tool, "input": input});
    let pattern_loop = look.transport.pointer("/loop/pattern") == Some(&Value::Bool(true));
    match intent {
        Intent::Tempo { bpm } if *bpm < 32.0 => Phrased::Refused("Renoise's tempo goes from 32 to 999 BPM.".into()),
        Intent::Tempo { bpm } => Phrased::Steps(vec![step("song_bpm", json!({"bpm": bpm}))]),
        Intent::Track { switch: Switch::Arm, .. } => Phrased::Refused(
            "Renoise doesn't arm tracks: it records notes into the selected track's pattern while edit mode is on (Esc).".into(),
        ),
        Intent::Track { track: pick, switch, on } => match track(pick, look) {
            Some(reference) if *switch == Switch::Mute => Phrased::Steps(vec![step("track_mute", json!({"ref": reference, "mute": on}))]),
            Some(reference) => Phrased::Steps(vec![step("track_solo", json!({"ref": reference, "solo": on}))]),
            None => Phrased::Model,
        },
        // Renoise loops whole slots of the sequence: bars that start or end inside a pattern are the model's to work out.
        Intent::Loop { from_bar, to_bar } => {
            let per_bar = look.transport.get("beatsPerBar").and_then(Value::as_f64).unwrap_or(4.0);
            let (start, end) = bar_span(*from_bar, *to_bar, per_bar);
            let Some((first, last)) = look.slots.and_then(|slots| slots_spanning(slots, start, end)) else { return Phrased::Model };
            let mut steps = Vec::new();
            if pattern_loop {
                steps.push(step("loop_pattern", json!({"on": false})));
            }
            steps.push(step("loop_range", json!({"first": first, "last": last})));
            Phrased::Steps(steps)
        }
        Intent::LoopOff => {
            let mut steps = Vec::new();
            if pattern_loop {
                steps.push(step("loop_pattern", json!({"on": false})));
            }
            steps.push(step("loop_range", json!({"first": 0, "last": 0})));
            Phrased::Steps(steps)
        }
        Intent::Metronome { on } => Phrased::Steps(vec![step("metronome_switch", json!({"on": on}))]),
    }
}

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
        // (Renoise's mixer pages around the selected track)
        BodyRequest::Mixer { .. } => body_args("mixer", None),
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
        BodyRequest::Mixer { .. } => FocusBody::Mixer(own_views::mixer(body)),
        _ => return None,
    })
}

/// Renoise's refs: `<epoch>:<kind>:<numbers>`, each number a Lua index (from 1; what the model sees is a short ref, and Renoise's
/// own numbers are in the rows). A track inserted or deleted moves the tracks after it, and the devices and parameters on them.
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
        BodyKind::Mixer => "Mixer",
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

    /// m6-places: each phrase's steps in a song as the look found it.
    #[test]
    fn a_phrase_is_renoises_own_change_its_loop_is_whole_slots_and_it_arms_no_tracks() {
        use crate::core::phrases::{PhraseTrack, TrackPick};
        use serde_json::json;
        let tracks = [
            PhraseTrack { reference: "track:1", name: "Kick", own: true, master: false },
            PhraseTrack { reference: "track:2", name: "Master", own: false, master: true },
            PhraseTrack { reference: "track:3", name: "Send 1", own: false, master: false },
        ];
        // Slots of 32, 64 and 32 lines at 4 lines a beat: 8, 16 and 8 beats.
        let slots = [(0.0, 8.0), (8.0, 16.0), (24.0, 8.0)];
        let plan = |intent: Intent, transport: Value| {
            phrase(&intent, &PhraseLook { tracks: &tracks, project: &json!({}), transport: &transport, slots: Some(&slots) })
        };
        let steps = |intent: Intent, transport: Value| match plan(intent, transport) {
            Phrased::Steps(steps) => Value::Array(steps),
            other => panic!("{other:?}"),
        };
        let switch = |track: TrackPick, switch: Switch| Intent::Track { track, switch, on: false };
        assert_eq!(steps(Intent::Tempo { bpm: 140.0 }, json!({})), json!([{"tool": "song_bpm", "input": {"bpm": 140.0}}]));
        assert_eq!(
            steps(switch(TrackPick::Name("send 1".into()), Switch::Mute), json!({})),
            json!([{"tool": "track_mute", "input": {"ref": "track:3", "mute": false}}]),
            "a send track by its name"
        );
        assert_eq!(
            plan(switch(TrackPick::Position(2), Switch::Solo), json!({})),
            Phrased::Model,
            "sends and the master don't count by place"
        );
        assert!(
            matches!(plan(switch(TrackPick::Name("kick".into()), Switch::Arm), json!({})), Phrased::Refused(why) if why.contains("edit mode"))
        );
        // Bars 3 to 6 at 4 beats a bar: beats 8 to 24, the second slot. The pattern loop on is turned off first.
        assert_eq!(
            steps(Intent::Loop { from_bar: 3, to_bar: 6 }, json!({"beatsPerBar": 4, "loop": {"pattern": true}})),
            json!([{"tool": "loop_pattern", "input": {"on": false}}, {"tool": "loop_range", "input": {"first": 2, "last": 2}}])
        );
        // At 2 beats a bar, bars 1 to 16 are the whole sequence.
        assert_eq!(
            steps(Intent::Loop { from_bar: 1, to_bar: 16 }, json!({"beatsPerBar": 2})),
            json!([{"tool": "loop_range", "input": {"first": 1, "last": 3}}])
        );
        assert_eq!(plan(Intent::Loop { from_bar: 2, to_bar: 3 }, json!({"beatsPerBar": 4})), Phrased::Model, "inside a pattern");
        assert_eq!(steps(Intent::LoopOff, json!({})), json!([{"tool": "loop_range", "input": {"first": 0, "last": 0}}]));
        assert_eq!(
            steps(Intent::LoopOff, json!({"loop": {"pattern": true}})),
            json!([{"tool": "loop_pattern", "input": {"on": false}}, {"tool": "loop_range", "input": {"first": 0, "last": 0}}])
        );
        assert_eq!(steps(Intent::Metronome { on: true }, json!({})), json!([{"tool": "metronome_switch", "input": {"on": true}}]));
        assert!(matches!(plan(Intent::Tempo { bpm: 30.0 }, json!({})), Phrased::Refused(why) if why.contains("32")));
    }

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
