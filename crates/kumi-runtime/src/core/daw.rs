//! Which DAW an integration drives, and what Kumi says and offers for it: what the producer calls the DAW and its
//! parts, and what the DAW can ever do through Kumi. The bridge and its version still decide what's there right now.
use serde::{Deserialize, Serialize};

/// The DAWs Kumi knows. Serde names are what files and settings keep.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DawId {
    Ableton,
    Reaper,
    Renoise,
    Logic,
}

impl DawId {
    pub const ALL: [DawId; 4] = [DawId::Ableton, DawId::Reaper, DawId::Renoise, DawId::Logic];

    /// Its name as files, settings and `--daw` say it.
    pub fn as_str(self) -> &'static str {
        match self {
            DawId::Ableton => "ableton",
            DawId::Reaper => "reaper",
            DawId::Renoise => "renoise",
            DawId::Logic => "logic",
        }
    }

    pub fn parse(text: &str) -> Option<DawId> {
        DawId::ALL.into_iter().find(|daw| daw.as_str() == text)
    }
}

/// What the producer calls the DAW and a saved place in it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DawWords {
    /// The DAW by the name the producer uses: "Live".
    pub app: &'static str,
    /// A saved place in it: "Set".
    pub place: &'static str,
    /// More than one: "Sets".
    pub places: &'static str,
    /// The track everything ends in: "Main".
    pub master: &'static str,
    /// The timeline a song is laid out on: "Arrangement".
    pub timeline: &'static str,
}

/// The DAW's own things that Kumi's texts name: its devices, its ways and its limits. Live's are what the texts said
/// before other DAWs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PromptVocab {
    /// The tool that makes several changes as one plan: "make_changes".
    pub plan_tool: &'static str,
    /// A reference to a track as a turn gives it, for examples: "track:3".
    pub ref_example: &'static str,
    /// A plan step setting a device a recipe loads by parameter name, for the recipe tool's example.
    pub recipe_step_example: &'static str,
    /// A capability scripts can't reach, for note_gap's example: "setting Operator's voice count".
    pub missing_example: &'static str,
    /// What a request may need beyond, as the gap guidance says it: "Kumi's tools or Live's scripting" (Kumi's tools
    /// alone, for a DAW Kumi reaches only through them).
    pub gap_offer: &'static str,
    /// What may lack a capability, as the gap tool says it, with its example: "Kumi's tools or Live's API lack it (a
    /// device setting scripts can't reach, an operation no tool offers)".
    pub gap_lack: &'static str,
    /// Base instruments a sound-matching search tries apart: "Operator, Wavetable, Drift, Collision or a Simpler sample".
    pub goal_instruments: &'static str,
    /// Topologies to try apart: "serial chains against parallel racks".
    pub serial_parallel: &'static str,
    /// Building in parallel inside a device container: "add rack chains, load devices into them".
    pub build_chains: &'static str,
    /// Why a match run mutes its other candidates instead of undoing them: Live undoes tracks only from the last one back.
    pub keep_candidates: &'static str,
    /// A layered sound inside one device container: "a rack of layers".
    pub layers: &'static str,
    /// What Kumi says when the DAW stays out of reach: how to let Kumi in.
    pub cant_reach: &'static str,
    /// What a crash may have cost, and how the DAW gives it back.
    pub crash_note: &'static str,
    /// What a sound-match goal needs from the DAW when it isn't there.
    pub goal_needs: &'static str,
    /// What to clean up by hand when Kumi couldn't remove what it rendered with.
    pub render_leftovers: &'static str,
    /// The device that ends a kept sound's chain for safety: "a Limiter".
    pub safety_limiter: &'static str,
    /// How Kumi hears the project: in Live, after each track's devices, as it plays or by playing the loop.
    pub listen_method: &'static str,
    /// The tool that writes notes into the project: "write_midi_clip".
    pub write_notes_tool: &'static str,
    /// The DAW's own EQ, by name: "EQ Eight".
    pub eq: &'static str,
    /// The octave the DAW names middle C (MIDI 60) in: Live calls it C3.
    pub middle_c_octave: i8,
    /// For each gap no knob closes, the structural change that closes it, with the DAW's own devices.
    pub structure_moves: &'static [(crate::audio::matching::StructuralKind, &'static str)],
    /// The DAW's own places for sounds, as find_sounds names them: "Live's User Library and Places".
    pub library_places: &'static str,
    /// What presets the producer has, as find_presets names them.
    pub preset_sources: &'static str,
    /// Devices to find presets for, as find_presets' examples.
    pub preset_examples: &'static str,
    /// find_presets' device field, with examples.
    pub preset_device_hint: &'static str,
    /// What the producer works in, for the speech recogniser's hint: "Ableton Live Set".
    pub voice_project: &'static str,
    /// Devices a producer names aloud, for the speech recogniser's hint.
    pub voice_devices: &'static str,
    /// What a tutorial video talks about, for the speech recogniser's hint.
    pub tutorial_words: &'static str,
    /// The DAW's own undo, which takes back what Kumi's can't: "Live's own undo (Cmd-Z in Live)".
    pub own_undo: &'static str,
    /// Said to the model after a restructure's undo the DAW didn't confirm: Kumi's refs are retired.
    pub refs_retired: &'static str,
    /// What a restructure adds, deletes or moves: "tracks and scenes".
    pub structure: &'static str,
    /// What brings a judged run's level back to where it belongs: "the last Live Limiter on the track, else a Utility it adds".
    pub gain_stage: &'static str,
    /// The device the judge's fix for a steady imbalance names: "EQ Eight" (none: the fix names no device).
    pub eq_name: &'static str,
    /// The device its fix for an imbalance that comes and goes names: "Multiband Dynamics".
    pub multiband: &'static str,
    /// The device its fix for wide lows names: "Utility's Bass Mono".
    pub bass_mono: &'static str,
    /// The device a drum part is read in lanes by: "a Drum Rack".
    pub drum_kit: &'static str,
    /// How a reference's drums become a MIDI clip and a stem: "separate its stems (live_command separate_stems), convert
    /// the drum stem to MIDI (live_command convert_drums_to_midi)".
    pub drum_stem_steps: &'static str,
    /// A clip in the DAW's launcher, which Kumi plays from the timeline to render: "a Session clip".
    pub launcher_clip: &'static str,
}

/// The structural changes for a DAW Kumi doesn't know: no device names.
const UNKNOWN_STRUCTURE_MOVES: &[(crate::audio::matching::StructuralKind, &str)] = {
    use crate::audio::matching::StructuralKind::*;
    &[
        (MissingLow, "add a sub layer (a sine one octave down, in a parallel chain), a low-shelf EQ, or change the base instrument"),
        (ExcessLow, "high-pass it (an EQ), take the low layer out, or change the base"),
        (MissingTop, "add saturation, an exciter or a brighter base (a saw or noise layer)"),
        (ExcessTop, "low-pass it (a filter), use a darker base (sine, triangle), or take a layer out"),
        (MissingMids, "add a layer for the body (a second oscillator or a parallel chain), or change the base"),
        (ExcessMids, "cut the mids (an EQ), change the base, or thin the layers"),
        (Envelope, "change the instrument family (a plucked or struck model against a sustained one), or add a transient shaper"),
        (Register, "transpose it (the MIDI, or the instrument's octave), or change the base"),
        (Pitched, "change the kind of source: a tonal oscillator against noise or a sample"),
        (Width, "widen or narrow it: a stereo width control, a chorus, or parallel chains panned apart"),
        (Density, "change the MIDI: more or fewer notes, another rhythm"),
    ]
};

/// Live's word for each field of a profile's words and vocabulary: the one place they're spelled, so a text
/// `daw_text!` makes in Live's words stays byte for byte what it was.
#[doc(hidden)]
#[macro_export]
macro_rules! live_word {
    (app) => {
        "Live"
    };
    (place) => {
        "Set"
    };
    (places) => {
        "Sets"
    };
    (master) => {
        "Main"
    };
    (timeline) => {
        "Arrangement"
    };
    (plan_tool) => {
        "make_changes"
    };
    (ref_example) => {
        "track:3"
    };
    (recipe_step_example) => {
        "set_device_parameter with deviceRef \"@sat\" and parameter \"Drive\""
    };
    (missing_example) => {
        "setting Operator's voice count"
    };
    (gap_offer) => {
        "Kumi's tools or Live's scripting"
    };
    (gap_lack) => {
        "Kumi's tools or Live's API lack it (a device setting scripts can't reach, an operation no tool offers)"
    };
    (goal_instruments) => {
        "Operator, Wavetable, Drift, Collision or a Simpler sample"
    };
    (serial_parallel) => {
        "serial chains against parallel racks"
    };
    (build_chains) => {
        "add rack chains, load devices into them"
    };
    (keep_candidates) => {
        "don't undo them: Live removes a track only from the last one back, and not one changed since"
    };
    (layers) => {
        "a rack of layers"
    };
    (cant_reach) => {
        "Kumi can't reach Live. Is it open, with AbletonMcpBridge chosen as a Control Surface (Settings → Link, Tempo & MIDI)?"
    };
    (crash_note) => {
        "If it crashed, your unsaved changes may be lost: Live offers to recover them when it opens."
    };
    (goal_needs) => {
        "Goals need Live connected, with the Ableton bridge 1.0.49 or later."
    };
    (render_leftovers) => {
        "Kumi couldn't remove its render tracks; delete the “Kumi · render” tracks by hand."
    };
    (safety_limiter) => {
        "a Limiter"
    };
    (listen_method) => {
        "Kumi listens in Live directly, after each track's devices: while Live plays, to what's playing now (seconds, 8 by default); while it's stopped, by playing the loop or from the playhead (or from_beat and beats), which the producer hears too. No recording or bouncing first."
    };
    (write_notes_tool) => {
        "write_midi_clip"
    };
    (eq) => {
        "EQ Eight"
    };
    (library_places) => {
        "Live's User Library and Places"
    };
    (preset_sources) => {
        "Live's presets and racks (in the User Library, Places and packs, and Live's own), Max for Live devices, and plug-ins' own presets"
    };
    (preset_examples) => {
        "(\"Wavetable\", \"Drum Rack\", \"Serum\")"
    };
    (preset_device_hint) => {
        "The device it's for, or a rack holding it: \"Operator\", \"Drum Rack\", \"Pro-Q 3\""
    };
    (voice_project) => {
        "Ableton Live Set"
    };
    (voice_devices) => {
        "Operator, Wavetable, Drift, Simpler, Drum Rack, Saturator, Roar, EQ Eight, Glue Compressor and Auto Filter"
    };
    (tutorial_words) => {
        "Ableton Live tutorial: Operator, Wavetable, Drift, Simpler, Sampler, Drum Rack, Instrument Rack, Saturator, Dynamic Tube, EQ Eight, EQ Three, Glue Compressor, OTT, Roar, Auto Filter, Utility, reverb, delay, LFO, envelope, oscillator, detune, sidechain, dry/wet, Serum, Vital."
    };
    (own_undo) => {
        "Live's own undo (Cmd-Z in Live)"
    };
    (refs_retired) => {
        "Live may have taken the tracks or scenes back, so Kumi's references are retired: discover again before using any."
    };
    (structure) => {
        "tracks and scenes"
    };
    (gain_stage) => {
        "the last Live Limiter on the track, else a Utility it adds"
    };
    (eq_name) => {
        "EQ Eight"
    };
    (multiband) => {
        "Multiband Dynamics"
    };
    (bass_mono) => {
        "Utility's Bass Mono"
    };
    (drum_kit) => {
        "a Drum Rack"
    };
    (drum_stem_steps) => {
        "separate its stems (live_command separate_stems), convert the drum stem to MIDI (live_command convert_drums_to_midi)"
    };
    (launcher_clip) => {
        "a Session clip"
    };
}

/// A field of a profile's words or vocabulary, by its name in `live_word!`.
#[doc(hidden)]
#[macro_export]
macro_rules! daw_word {
    ($profile:expr, app) => {
        $profile.words.app
    };
    ($profile:expr, place) => {
        $profile.words.place
    };
    ($profile:expr, places) => {
        $profile.words.places
    };
    ($profile:expr, master) => {
        $profile.words.master
    };
    ($profile:expr, timeline) => {
        $profile.words.timeline
    };
    ($profile:expr, plan_tool) => {
        $profile.vocab.plan_tool
    };
    ($profile:expr, ref_example) => {
        $profile.vocab.ref_example
    };
    ($profile:expr, recipe_step_example) => {
        $profile.vocab.recipe_step_example
    };
    ($profile:expr, missing_example) => {
        $profile.vocab.missing_example
    };
    ($profile:expr, gap_offer) => {
        $profile.vocab.gap_offer
    };
    ($profile:expr, gap_lack) => {
        $profile.vocab.gap_lack
    };
    ($profile:expr, goal_instruments) => {
        $profile.vocab.goal_instruments
    };
    ($profile:expr, serial_parallel) => {
        $profile.vocab.serial_parallel
    };
    ($profile:expr, build_chains) => {
        $profile.vocab.build_chains
    };
    ($profile:expr, keep_candidates) => {
        $profile.vocab.keep_candidates
    };
    ($profile:expr, layers) => {
        $profile.vocab.layers
    };
    ($profile:expr, cant_reach) => {
        $profile.vocab.cant_reach
    };
    ($profile:expr, crash_note) => {
        $profile.vocab.crash_note
    };
    ($profile:expr, goal_needs) => {
        $profile.vocab.goal_needs
    };
    ($profile:expr, render_leftovers) => {
        $profile.vocab.render_leftovers
    };
    ($profile:expr, safety_limiter) => {
        $profile.vocab.safety_limiter
    };
    ($profile:expr, listen_method) => {
        $profile.vocab.listen_method
    };
    ($profile:expr, write_notes_tool) => {
        $profile.vocab.write_notes_tool
    };
    ($profile:expr, eq) => {
        $profile.vocab.eq
    };
    ($profile:expr, library_places) => {
        $profile.vocab.library_places
    };
    ($profile:expr, preset_sources) => {
        $profile.vocab.preset_sources
    };
    ($profile:expr, preset_examples) => {
        $profile.vocab.preset_examples
    };
    ($profile:expr, preset_device_hint) => {
        $profile.vocab.preset_device_hint
    };
    ($profile:expr, voice_project) => {
        $profile.vocab.voice_project
    };
    ($profile:expr, voice_devices) => {
        $profile.vocab.voice_devices
    };
    ($profile:expr, tutorial_words) => {
        $profile.vocab.tutorial_words
    };
    ($profile:expr, own_undo) => {
        $profile.vocab.own_undo
    };
    ($profile:expr, refs_retired) => {
        $profile.vocab.refs_retired
    };
    ($profile:expr, structure) => {
        $profile.vocab.structure
    };
    ($profile:expr, gain_stage) => {
        $profile.vocab.gain_stage
    };
    ($profile:expr, eq_name) => {
        $profile.vocab.eq_name
    };
    ($profile:expr, multiband) => {
        $profile.vocab.multiband
    };
    ($profile:expr, bass_mono) => {
        $profile.vocab.bass_mono
    };
    ($profile:expr, drum_kit) => {
        $profile.vocab.drum_kit
    };
    ($profile:expr, drum_stem_steps) => {
        $profile.vocab.drum_stem_steps
    };
    ($profile:expr, launcher_clip) => {
        $profile.vocab.launcher_clip
    };
}

/// (M5) What Kumi says it can't do in a DAW it is connected to, which doesn't do that through Kumi yet: "Kumi can't
/// pursue a goal in Logic Pro yet." (never that it isn't connected).
pub fn cant_yet(profile: &DawProfile, what: &str) -> String {
    format!("Kumi can't {what} in {} yet.", profile.words.app)
}

/// A device's name as a parenthetical after what it does: " (EQ Eight)", or nothing for a DAW whose fix names no device.
pub fn parenthetical(name: &str) -> String {
    if name.is_empty() {
        String::new()
    } else {
        format!(" ({name})")
    }
}

/// "an EQ Eight", "a ReaEQ": a name with its article, unless it has one.
pub fn with_article(name: &str) -> String {
    if name.starts_with("a ") || name.starts_with("an ") {
        return name.into();
    }
    let vowel = name.chars().next().is_some_and(|first| "AEIOUaeiou".contains(first));
    format!("{} {name}", if vowel { "an" } else { "a" })
}

/// A text in a DAW's words, made from pieces: string literals, a word's name (`place`, `app`…) or `(if cap: "…")` for
/// a clause only a DAW with that capability gets. `daw_text!(pub NAME, name: "…", place, "…")` makes the constant
/// `NAME` in Live's words, as callers had it before other DAWs, and `name(&DawProfile)` for any DAW, from the same
/// pieces: Live's text can't drift from what it was. (Live has every capability, so its text has every clause.)
#[macro_export]
macro_rules! daw_text {
    ($(#[$meta:meta])* $vis:vis $konst:ident, $func:ident: $($piece:tt),+ $(,)?) => {
        $(#[$meta])*
        $vis const $konst: &str = concat!($($crate::daw_text!(@live $piece)),+);
        $(#[$meta])*
        $vis fn $func(profile: &$crate::core::daw::DawProfile) -> String {
            [$($crate::daw_text!(@of profile, $piece)),+].concat()
        }
    };
    (@live $text:literal) => { $text };
    (@live $word:ident) => { $crate::live_word!($word) };
    (@live (if $cap:ident: $text:literal)) => { $text };
    (@of $profile:ident, $text:literal) => { $text };
    (@of $profile:ident, $word:ident) => { $crate::daw_word!($profile, $word) };
    (@of $profile:ident, (if $cap:ident: $text:literal)) => { if $profile.caps.$cap { $text } else { "" } };
}

/// The language a DAW runs code from Kumi in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ScriptKind {
    Python,
    Lua,
}

/// What a DAW can ever do through Kumi. Each flag says whether the feature exists for this DAW at all; whether it's
/// offered now still depends on the bridge (its tools and version) and on Live or the DAW being there.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Capabilities {
    /// Kumi's own undo of a change it made.
    pub undo: bool,
    /// A fixed phrase made as the DAW's change with no model call.
    pub instant: bool,
    /// Code from the model run inside the DAW.
    pub scripting: Option<ScriptKind>,
    /// The audio file behind something in the project.
    pub audio_file: bool,
    /// Hearing tracks or the mix, as they play or over a stretch of the timeline.
    pub hear: bool,
    /// Rendering candidates and setting them against a reference.
    pub audition: bool,
    /// A goal's render rig, kept open across generations.
    pub goal_rig: bool,
    /// Splitting a track into stems inside the DAW (Live 12's own splitter).
    pub stems: bool,
    /// Turning audio into MIDI inside the DAW (Live's conversions).
    pub audio_to_midi: bool,
    /// Stop clips, the transport and recording at once.
    pub stop_transport: bool,
    /// A short line in the DAW's own window, never a dialog.
    pub status_line: bool,
    /// Whether the DAW is recording now.
    pub recording_state: bool,
    /// Opening a saved project in the DAW.
    pub open_project: bool,
    /// FOCUS: a track's devices and containers.
    pub device_tree: bool,
    /// FOCUS: the notes of the item in the note editor.
    pub note_view: bool,
    /// FOCUS: a launcher grid (Live's Session slots).
    pub launcher_grid: bool,
    /// FOCUS: the timeline at a glance.
    pub timeline_strip: bool,
    /// What the producer looks at, as it changes.
    pub focus_stream: bool,
    /// The transport, as it moves.
    pub transport_stream: bool,
    /// The producer pointing at something in the DAW ("Ask Kumi about this").
    pub pointing: bool,
    /// An id kept inside the saved project, so its conversations follow it.
    pub durable_project_id: bool,
    /// What changed in a saved project while Kumi wasn't running.
    pub catch_up: bool,
    /// Devices Kumi builds as Max for Live files.
    pub max_for_live_devices: bool,
    /// Kumi Ears, the Max for Live device Kumi hears through.
    pub ears: bool,
    /// Kumi's hands on the DAW's own window, where its scripting can't reach.
    pub ui_hands: bool,
    /// Live's Extensions SDK channel.
    pub extension_sdk: bool,
    /// Willington's bindings.
    pub willington: bool,
    /// The DAW's manual, searched by a tool.
    pub manual: bool,
    /// Browser items loaded by id.
    pub browser_items: bool,
    /// Plug-in parameters the DAW exposes only once configured.
    pub configured_plugin_params: bool,
}

/// One DAW as Kumi knows it: its id (None when no DAW is known), its words and what it can do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DawProfile {
    pub id: Option<DawId>,
    pub words: DawWords,
    pub caps: Capabilities,
    pub vocab: PromptVocab,
}

impl DawProfile {
    /// No DAW known: plain words, and nothing it can do.
    pub const UNKNOWN: DawProfile = DawProfile {
        id: None,
        words: DawWords { app: "the DAW", place: "project", places: "projects", master: "the master track", timeline: "timeline" },
        caps: Capabilities::NONE,
        vocab: PromptVocab {
            plan_tool: "make_changes",
            ref_example: "track:3",
            recipe_step_example: "a parameter set by its name on a device an earlier step loaded",
            missing_example: "setting a synth's voice count",
            gap_offer: "Kumi's tools or the DAW's scripting",
            gap_lack: "Kumi's tools or the DAW's API lack it (a device setting scripts can't reach, an operation no tool offers)",
            goal_instruments: "different synths or a sampler",
            serial_parallel: "serial chains against parallel ones",
            build_chains: "add parallel chains, load devices into them",
            keep_candidates: "don't undo them: the producer may want to A/B them",
            layers: "layered parallel chains",
            cant_reach: "Kumi can't reach the DAW. Is it open, with Kumi's bridge in it?",
            crash_note: "If it crashed, your unsaved changes may be lost.",
            goal_needs: "Goals need the DAW connected, with Kumi's bridge.",
            render_leftovers: "Kumi couldn't clean up after rendering; remove what it left in the project by hand.",
            safety_limiter: "a limiter",
            listen_method: "Kumi hears the DAW directly, after each track's devices (seconds, 8 by default, or from_beat and beats). No recording or bouncing first.",
            write_notes_tool: "the tool that writes notes",
            eq: "an EQ",
            middle_c_octave: 4,
            structure_moves: UNKNOWN_STRUCTURE_MOVES,
            library_places: "the DAW's own library",
            preset_sources: "the DAW's own presets and device chains, and plug-ins' own presets",
            preset_examples: "(\"Serum\", \"Pro-Q 3\")",
            preset_device_hint: "The device it's for: \"Serum\", \"Pro-Q 3\"",
            voice_project: "project",
            voice_devices: "synths, samplers, EQs, compressors and filters",
            tutorial_words: "Music production tutorial: synth, sampler, EQ, compressor, saturation, reverb, delay, LFO, envelope, oscillator, detune, sidechain, dry/wet, Serum, Vital.",
            own_undo: "the DAW's own undo",
            refs_retired: "The DAW may have taken the tracks back, so Kumi's references are retired: discover again before using any.",
            structure: "tracks",
            gain_stage: "the track's last limiter, else a gain device it adds",
            eq_name: "",
            multiband: "",
            bass_mono: "",
            drum_kit: "a drum-kit device",
            drum_stem_steps: "separate its stems, convert the drum stem to MIDI",
            launcher_clip: "a clip",
        },
    };

    /// Ableton Live, as Kumi drives it today: everything above.
    pub const ABLETON: DawProfile = DawProfile {
        id: Some(DawId::Ableton),
        words: DawWords {
            app: crate::live_word!(app),
            place: crate::live_word!(place),
            places: crate::live_word!(places),
            master: crate::live_word!(master),
            timeline: crate::live_word!(timeline),
        },
        caps: Capabilities {
            undo: true,
            instant: true,
            scripting: Some(ScriptKind::Python),
            audio_file: true,
            hear: true,
            audition: true,
            goal_rig: true,
            stems: true,
            audio_to_midi: true,
            stop_transport: true,
            status_line: true,
            recording_state: true,
            open_project: true,
            device_tree: true,
            note_view: true,
            launcher_grid: true,
            timeline_strip: true,
            focus_stream: true,
            transport_stream: true,
            pointing: true,
            durable_project_id: true,
            catch_up: true,
            max_for_live_devices: true,
            ears: true,
            ui_hands: true,
            extension_sdk: true,
            willington: true,
            manual: true,
            browser_items: true,
            configured_plugin_params: true,
        },
        vocab: PromptVocab {
            plan_tool: crate::live_word!(plan_tool),
            ref_example: crate::live_word!(ref_example),
            recipe_step_example: crate::live_word!(recipe_step_example),
            missing_example: crate::live_word!(missing_example),
            gap_offer: crate::live_word!(gap_offer),
            gap_lack: crate::live_word!(gap_lack),
            goal_instruments: crate::live_word!(goal_instruments),
            serial_parallel: crate::live_word!(serial_parallel),
            build_chains: crate::live_word!(build_chains),
            keep_candidates: crate::live_word!(keep_candidates),
            layers: crate::live_word!(layers),
            cant_reach: crate::live_word!(cant_reach),
            crash_note: crate::live_word!(crash_note),
            goal_needs: crate::live_word!(goal_needs),
            render_leftovers: crate::live_word!(render_leftovers),
            safety_limiter: crate::live_word!(safety_limiter),
            listen_method: crate::live_word!(listen_method),
            write_notes_tool: crate::live_word!(write_notes_tool),
            eq: crate::live_word!(eq),
            middle_c_octave: 3,
            structure_moves: crate::audio::matching::STRUCTURE_MOVES,
            library_places: crate::live_word!(library_places),
            preset_sources: crate::live_word!(preset_sources),
            preset_examples: crate::live_word!(preset_examples),
            preset_device_hint: crate::live_word!(preset_device_hint),
            voice_project: crate::live_word!(voice_project),
            voice_devices: crate::live_word!(voice_devices),
            tutorial_words: crate::live_word!(tutorial_words),
            own_undo: crate::live_word!(own_undo),
            refs_retired: crate::live_word!(refs_retired),
            structure: crate::live_word!(structure),
            gain_stage: crate::live_word!(gain_stage),
            eq_name: crate::live_word!(eq_name),
            multiband: crate::live_word!(multiband),
            bass_mono: crate::live_word!(bass_mono),
            drum_kit: crate::live_word!(drum_kit),
            drum_stem_steps: crate::live_word!(drum_stem_steps),
            launcher_clip: crate::live_word!(launcher_clip),
        },
    };

    /// REAPER, through Kumi's extension: its words and vocabulary. What it can do grows with each milestone.
    pub const REAPER: DawProfile = DawProfile {
        id: Some(DawId::Reaper),
        words: DawWords { app: "REAPER", place: "project", places: "projects", master: "the master track", timeline: "arrange view" },
        // FOCUS follows REAPER (M2): its FX chains, a take's notes, the arrange view, the focus and transport as read.
        caps: Capabilities {
            device_tree: true,
            note_view: true,
            timeline_strip: true,
            focus_stream: true,
            transport_stream: true,
            ..Capabilities::NONE
        },
        vocab: PromptVocab {
            plan_tool: "make_changes",
            ref_example: "track:3",
            recipe_step_example: "a parameter set by its name on an FX an earlier step added",
            missing_example: "setting a plug-in's oversampling",
            gap_offer: "Kumi's tools or REAPER's scripting",
            gap_lack: "Kumi's tools or REAPER's API lack it (a device setting scripts can't reach, an operation no tool offers)",
            goal_instruments: "ReaSynth, a ReaSamplOmatic5000 sample or a plug-in synth",
            serial_parallel: "serial FX chains against parallel containers",
            build_chains: "add an FX container with parallel chains, load FX into them",
            keep_candidates: "don't undo them: the producer may want to A/B them",
            layers: "an FX container of parallel layers",
            cant_reach: "Kumi can't reach REAPER. Is it open, with Kumi's extension installed (kumi --daw reaper bridge)?",
            crash_note: "If it crashed, your unsaved changes may be lost.",
            goal_needs: "Goals need REAPER connected, with Kumi's extension.",
            render_leftovers: "Kumi couldn't clean up after rendering; remove what it left in the project by hand.",
            safety_limiter: "a limiter",
            listen_method: "Kumi renders the part offline to hear it (seconds, 8 by default, or from_beat and beats), then plays what it chose out loud.",
            write_notes_tool: "the tool that writes MIDI notes",
            eq: "ReaEQ",
            middle_c_octave: 4,
            structure_moves: UNKNOWN_STRUCTURE_MOVES,
            library_places: "REAPER's resource folder (FX chains, track and project templates)",
            preset_sources: "REAPER's FX presets and chains, and plug-ins' own presets",
            preset_examples: "(\"ReaEQ\", \"Serum\", \"Pro-Q 3\")",
            preset_device_hint: "The FX it's for: \"ReaComp\", \"Serum\"",
            voice_project: "REAPER project",
            voice_devices: "ReaEQ, ReaComp, ReaXcomp, ReaDelay, ReaVerbate, ReaSynth and ReaSamplOmatic5000",
            tutorial_words: "REAPER tutorial: ReaEQ, ReaComp, ReaXcomp, ReaDelay, ReaVerbate, ReaSynth, ReaSamplOmatic5000, JSFX, FX chain, FX container, routing, sidechain, automation item, take, lane, region, marker, Serum, Vital.",
            own_undo: "REAPER's own undo (Cmd-Z in REAPER)",
            refs_retired: "REAPER may have changed the tracks, so Kumi's references are retired: read them again before using any.",
            structure: "tracks",
            gain_stage: "the track's last limiter, else a gain FX it adds",
            eq_name: "ReaEQ",
            multiband: "ReaXcomp",
            bass_mono: "",
            drum_kit: "a sampler kit (one ReaSamplOmatic5000 per pad)",
            drum_stem_steps: "separate its stems, convert the drum stem to MIDI",
            launcher_clip: "an item",
        },
    };

    /// Renoise, through Kumi's tool: its words and vocabulary. What it can do grows with each milestone.
    pub const RENOISE: DawProfile = DawProfile {
        id: Some(DawId::Renoise),
        words: DawWords { app: "Renoise", place: "song", places: "songs", master: "the Master track", timeline: "pattern sequence" },
        // FOCUS follows Renoise (M2): its DSP chains, the focus and transport as read.
        caps: Capabilities { device_tree: true, focus_stream: true, transport_stream: true, ..Capabilities::NONE },
        vocab: PromptVocab {
            plan_tool: "make_changes",
            ref_example: "track:3",
            recipe_step_example: "a parameter set by its name on a device an earlier step loaded",
            missing_example: "setting a plug-in's oversampling",
            gap_offer: "Kumi's tools or Renoise's scripting",
            gap_lack: "Kumi's tools or Renoise's API lack it (a device setting scripts can't reach, an operation no tool offers)",
            goal_instruments: "a sample instrument with keyzones, a plug-in synth or a Doofer of layers",
            serial_parallel: "serial device chains against a Splitter's parallel branches",
            build_chains: "add a Splitter, load devices into its branches",
            keep_candidates: "don't undo them: the producer may want to A/B them",
            layers: "a Doofer of layers",
            cant_reach: "Kumi can't reach Renoise. Is it open, with Kumi's tool installed (kumi --daw renoise bridge)?",
            crash_note: "If it crashed, your unsaved changes may be lost.",
            goal_needs: "Goals need Renoise connected, with Kumi's tool.",
            render_leftovers: "Kumi couldn't clean up after rendering; remove what it left in the song by hand.",
            safety_limiter: "a Maximizer",
            listen_method: "Kumi renders the part offline to hear it (seconds, 8 by default, or from_beat and beats), then plays what it chose out loud.",
            write_notes_tool: "the tool that writes pattern lines",
            eq: "EQ 10",
            middle_c_octave: 5,
            structure_moves: UNKNOWN_STRUCTURE_MOVES,
            library_places: "Renoise's Library folders (instruments, multi-samples, chains, presets, phrases)",
            preset_sources: "Renoise's device presets, chains and instruments, and plug-ins' own presets",
            preset_examples: "(\"Compressor\", \"Serum\", \"Pro-Q 3\")",
            preset_device_hint: "The device it's for: \"Compressor\", \"Serum\"",
            voice_project: "Renoise song",
            voice_devices: "Compressor, EQ 10, Maximizer, Delay, Reverb, Distortion, Doofer, Splitter and sample instruments",
            tutorial_words: "Renoise tutorial: pattern editor, pattern matrix, phrase editor, keyzones, effect commands, Doofer, Splitter, instrument macros, modulation, Compressor, EQ 10, Maximizer, Reverb, Delay, sidechain, LFO, envelope, Serum, Vital.",
            own_undo: "Renoise's own undo (Cmd-Z in Renoise)",
            refs_retired: "Renoise may have moved the tracks, so Kumi's references are retired: read them again before using any.",
            structure: "tracks",
            gain_stage: "the track's last Maximizer, else a Gainer it adds",
            eq_name: "EQ 10",
            multiband: "",
            bass_mono: "",
            drum_kit: "a drum-kit instrument (a sample per keyzone)",
            drum_stem_steps: "separate its stems, convert the drum stem to MIDI",
            launcher_clip: "a pattern",
        },
    };

    /// Logic Pro, through Kumi's own bridge beside it (it reads Logic through macOS Accessibility): its words and
    /// vocabulary. What it can do grows with each milestone.
    pub const LOGIC: DawProfile = DawProfile {
        id: Some(DawId::Logic),
        words: DawWords { app: "Logic Pro", place: "project", places: "projects", master: "Stereo Out", timeline: "Tracks area" },
        // FOCUS follows Logic (M2): a channel strip's plug-ins, a region's notes, the Tracks area, the focus and transport
        // as read; the bridge sees whether Logic records, can open a project, and works Logic's own window. Kumi's
        // changes in Logic are undone through its host's journal (M3), each as a new step of Logic's own undo. (M5) Kumi
        // hears Logic: it plays a part and captures what Logic outputs, and auditions candidates a pass each; a region's
        // audio file waits for the bridge's rows to name it, and a goal's render rig for a later slice. A saved project is
        // caught up on as it opens (M6), from the snapshot Kumi kept of it; its id is its path's, not kept inside it. Its
        // fixed phrases run with no model call (M6), each as one of its own changes.
        caps: Capabilities {
            undo: true,
            instant: true,
            hear: true,
            audition: true,
            device_tree: true,
            note_view: true,
            timeline_strip: true,
            focus_stream: true,
            transport_stream: true,
            recording_state: true,
            open_project: true,
            ui_hands: true,
            catch_up: true,
            ..Capabilities::NONE
        },
        vocab: PromptVocab {
            plan_tool: "make_changes",
            ref_example: "track:3",
            recipe_step_example: "fx_param with ref \"@eq\" and param \"Peak 1 Gain\", on the plug-in an earlier step inserted with fx_add as \"eq\"",
            missing_example: "setting a plug-in's oversampling",
            // Logic has no scripting of its own that Kumi reaches: its bridge works Logic's windows for Kumi's tools.
            gap_offer: "Kumi's tools",
            gap_lack: "Kumi's tools lack it (a plug-in setting Kumi's bridge can't reach, an operation no tool offers)",
            goal_instruments: "Alchemy, Retro Synth, Sampler, Quick Sampler or an Audio Unit synth",
            serial_parallel: "serial insert chains against parallel auxes fed by sends",
            build_chains: "add aux tracks fed by sends, insert plug-ins on them",
            keep_candidates: "don't undo them: the producer may want to A/B them",
            layers: "a summing Track Stack of layers",
            cant_reach: "Kumi can't reach Logic Pro. Is it open, with Kumi's Logic Pro bridge set up (kumi --daw logic bridge) and Accessibility allowed?",
            crash_note: "If it crashed, your unsaved changes may be lost.",
            goal_needs: "Goals need Logic Pro connected, with Kumi's Logic Pro bridge.",
            render_leftovers: "Kumi couldn't clean up after bouncing; remove what it left in the project by hand.",
            safety_limiter: "a Limiter",
            listen_method: "Kumi plays the part in Logic Pro and captures what it outputs after the Stereo Out, which the producer hears too: from the playhead (seconds, 8 by default) or from_beat and beats; each track is heard alone, soloed for a pass of its own. No bouncing first.",
            write_notes_tool: "the tool that writes MIDI notes into a region",
            eq: "Channel EQ",
            middle_c_octave: 3,
            structure_moves: UNKNOWN_STRUCTURE_MOVES,
            library_places: "Logic Pro's Library (patches, channel strip settings and plug-in settings) and its Loop Browser",
            preset_sources: "Logic Pro's patches, channel strip settings and plug-in settings, and Audio Units' own presets",
            preset_examples: "(\"Channel EQ\", \"Alchemy\", \"Serum\")",
            preset_device_hint: "The plug-in it's for: \"Compressor\", \"Alchemy\", \"Serum\"",
            voice_project: "Logic Pro project",
            voice_devices: "Channel EQ, Compressor, Alchemy, Retro Synth, Sampler, Quick Sampler, Drum Kit Designer, Space Designer, ChromaVerb and Tape Delay",
            tutorial_words: "Logic Pro tutorial: Channel EQ, Compressor, Alchemy, Retro Synth, Sampler, Quick Sampler, Drum Kit Designer, Drum Machine Designer, Drummer, Space Designer, ChromaVerb, Tape Delay, Smart Controls, Track Stacks, busses, auxes, sends, Piano Roll, Step Sequencer, Flex Time, sidechain, LFO, envelope, oscillator, Serum, Vital.",
            own_undo: "Logic Pro's own undo (Cmd-Z in Logic Pro)",
            refs_retired: "Logic Pro may have moved the tracks, so Kumi's references are retired: read them again before using any.",
            structure: "tracks",
            gain_stage: "the strip's last Adaptive Limiter or Limiter, else its last Gain plug-in, else a Gain it adds at the end (the Stereo Out's, for the whole mix)",
            eq_name: "Channel EQ",
            multiband: "Multipressor",
            bass_mono: "",
            drum_kit: "a Drum Kit Designer or Drum Machine Designer kit (a pad per sound)",
            drum_stem_steps: "separate its stems (Stem Splitter), convert the drum stem to MIDI",
            launcher_clip: "a grid cell",
        },
    };

    /// The same DAW with nothing it can do now: its words stay (the producer still calls it by its name).
    pub const fn without_caps(self) -> DawProfile {
        DawProfile { caps: Capabilities::NONE, ..self }
    }

    /// The profile a text that said Live before other DAWs speaks in: Live's when no DAW is known (an integration
    /// that doesn't say), this one's otherwise.
    pub fn or_live(&self) -> &DawProfile {
        if *self == Self::UNKNOWN {
            &Self::ABLETON
        } else {
            self
        }
    }

    /// The same DAW naming middle C (MIDI 60) in `octave` when the open project says it does (Logic Pro's "Display
    /// Middle C as", 3 or 4); as its profile has it otherwise.
    pub fn with_middle_c(mut self, octave: Option<i8>) -> DawProfile {
        if let Some(octave) = octave {
            self.vocab.middle_c_octave = octave;
        }
        self
    }

    /// A MIDI note by the name the DAW gives it: "C3" for 60 in Live.
    pub fn note_name(&self, midi: u8) -> String {
        if self.vocab.middle_c_octave == 3 {
            return crate::notation::pitch_name(midi);
        }
        const NAMES: [&str; 12] = ["C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"];
        format!("{}{}", NAMES[midi as usize % 12], midi as i32 / 12 - 5 + self.vocab.middle_c_octave as i32)
    }

    /// `(app, place)`, as `Integration::words` answered before profiles.
    pub const fn word_pair(&self) -> (&'static str, &'static str) {
        (self.words.app, self.words.place)
    }
}

impl Capabilities {
    pub const NONE: Capabilities = Capabilities {
        undo: false,
        instant: false,
        scripting: None,
        audio_file: false,
        hear: false,
        audition: false,
        goal_rig: false,
        stems: false,
        audio_to_midi: false,
        stop_transport: false,
        status_line: false,
        recording_state: false,
        open_project: false,
        device_tree: false,
        note_view: false,
        launcher_grid: false,
        timeline_strip: false,
        focus_stream: false,
        transport_stream: false,
        pointing: false,
        durable_project_id: false,
        catch_up: false,
        max_for_live_devices: false,
        ears: false,
        ui_hands: false,
        extension_sdk: false,
        willington: false,
        manual: false,
        browser_items: false,
        configured_plugin_params: false,
    };
}

/// A phrase one of Kumi's frozen assets (a tool's JSON) says in Live's words, how many times the asset's descriptions
/// hold it, and how a DAW says it. The asset stays byte for byte as it is; a DAW's words go in as it's loaded.
pub struct AssetPhrase {
    pub live: &'static str,
    pub count: usize,
    pub said: fn(&DawProfile) -> String,
}

/// Every description in an asset: a string under a "description" key, at any depth. Keys, names, enum values and
/// defaults are never descriptions, so the words never change a schema.
fn descriptions(value: &mut serde_json::Value, each: &mut dyn FnMut(&mut String)) {
    match value {
        serde_json::Value::Object(map) => {
            for (key, item) in map.iter_mut() {
                match item {
                    serde_json::Value::String(text) if key == "description" => each(text),
                    _ => descriptions(item, each),
                }
            }
        }
        serde_json::Value::Array(items) => items.iter_mut().for_each(|item| descriptions(item, each)),
        _ => {}
    }
}

/// Whether each phrase appears in the asset's descriptions exactly as many times as it says.
pub fn check_asset_phrases(value: &serde_json::Value, phrases: &[AssetPhrase]) -> Result<(), String> {
    let mut value = value.clone();
    let mut found = vec![0; phrases.len()];
    descriptions(&mut value, &mut |text| {
        for (count, phrase) in found.iter_mut().zip(phrases) {
            *count += text.matches(phrase.live).count();
        }
    });
    match phrases.iter().zip(found).find(|(phrase, found)| phrase.count != *found) {
        Some((phrase, found)) => Err(format!("“{}” is in the asset {found} times, not {}", phrase.live, phrase.count)),
        None => Ok(()),
    }
}

/// An asset in a DAW's words: its descriptions with each phrase said the DAW's way, in one pass from left to right,
/// so what one phrase becomes is never read again for another. Live's stay as they are.
pub fn asset_in_words(mut value: serde_json::Value, profile: &DawProfile, phrases: &[AssetPhrase]) -> serde_json::Value {
    debug_assert_eq!(check_asset_phrases(&value, phrases), Ok(()));
    let said: Vec<(&str, String)> = phrases.iter().map(|phrase| (phrase.live, (phrase.said)(profile))).collect();
    if said.iter().all(|(live, said)| live == said) {
        return value;
    }
    descriptions(&mut value, &mut |text| {
        if !said.iter().any(|(live, _)| text.contains(live)) {
            return;
        }
        let mut out = String::with_capacity(text.len());
        let mut rest = text.as_str();
        while !rest.is_empty() {
            match said.iter().find(|(live, _)| rest.starts_with(live)) {
                Some((live, said)) => {
                    out.push_str(said);
                    rest = &rest[live.len()..];
                }
                None => {
                    let next = rest.chars().next().map_or(1, char::len_utf8);
                    out.push_str(&rest[..next]);
                    rest = &rest[next..];
                }
            }
        }
        *text = out;
    });
    value
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_words_are_what_kumi_said_before_profiles() {
        assert_eq!(DawProfile::UNKNOWN.word_pair(), ("the DAW", "project"));
        assert_eq!(DawProfile::ABLETON.word_pair(), ("Live", "Set"));
        assert_eq!(DawProfile::ABLETON.without_caps().word_pair(), ("Live", "Set"));
        assert_eq!(DawProfile::ABLETON.without_caps().caps, Capabilities::NONE);
        assert_eq!(Capabilities::default(), Capabilities::NONE);
    }

    crate::daw_text!(
        /// A text for the test below.
        SAID, said: "Save the ", place, " in ", app, (if max_for_live_devices: ", or make a device"), "."
    );

    #[test]
    fn a_text_says_lives_words_as_its_constant_and_any_dawss_from_the_same_pieces() {
        assert_eq!(SAID, "Save the Set in Live, or make a device.");
        assert_eq!(said(&DawProfile::ABLETON), SAID);
        assert_eq!(said(&DawProfile::UNKNOWN), "Save the project in the DAW.");
    }

    #[test]
    fn reaper_renoise_and_logic_speak_their_own_words_never_lives() {
        let live = regex::Regex::new(
            r"\b(Live|Set|Sets|Ableton|Session|Arrangement|Remote Script|Control Surface|Main|Drum Rack|Operator|Simpler|EQ Eight|Utility|[Rr]acks?|[Cc]lip slots?|[Ss]cenes?)\b",
        )
        .unwrap();
        for profile in [DawProfile::REAPER, DawProfile::RENOISE, DawProfile::LOGIC] {
            let said = format!("{:?}", profile);
            let found: Vec<&str> = live.find_iter(&said).map(|m| m.as_str()).collect();
            assert!(found.is_empty(), "{:?}: {found:?}", profile.id);
        }
        assert_eq!(DawProfile::REAPER.word_pair(), ("REAPER", "project"));
        assert_eq!(DawProfile::RENOISE.word_pair(), ("Renoise", "song"));
        assert_eq!(DawProfile::LOGIC.word_pair(), ("Logic Pro", "project"));
        assert_eq!((DawProfile::LOGIC.words.master, DawProfile::LOGIC.words.timeline), ("Stereo Out", "Tracks area"));
        // Logic names middle C as Live does; its channel strips, regions and plug-ins are its own words.
        assert_eq!(DawProfile::LOGIC.note_name(60), "C3");
        // (M6) Logic's "Display Middle C As", as the open project says it: C4 when the producer chose it.
        assert_eq!(DawProfile::LOGIC.with_middle_c(Some(4)).note_name(60), "C4");
        assert_eq!(DawProfile::LOGIC.with_middle_c(Some(4)).note_name(61), "C#4");
        assert_eq!(DawProfile::LOGIC.with_middle_c(None).note_name(60), "C3");
        assert_eq!(DawProfile::LOGIC.with_middle_c(Some(3)), DawProfile::LOGIC);
        assert_eq!(DawProfile::LOGIC.vocab.eq, "Channel EQ");
        assert!(DawProfile::LOGIC.caps.device_tree && DawProfile::LOGIC.caps.note_view && DawProfile::LOGIC.caps.timeline_strip);
        assert!(DawProfile::LOGIC.caps.ui_hands && DawProfile::LOGIC.caps.scripting.is_none() && DawProfile::LOGIC.caps.undo);
        // (M5) Kumi hears Logic and auditions in it; a region's file and a goal's rig wait (REAPER and Renoise hear
        // nothing yet). Its words say how it hears and where a judged run turns the level.
        let caps = DawProfile::LOGIC.caps;
        assert_eq!((caps.hear, caps.audition, caps.audio_file, caps.goal_rig), (true, true, false, false));
        assert!(!DawProfile::REAPER.caps.hear && !DawProfile::RENOISE.caps.hear && !DawProfile::REAPER.caps.audition);
        assert!(DawProfile::LOGIC.vocab.listen_method.contains("captures what it outputs after the Stereo Out"));
        assert!(DawProfile::LOGIC.vocab.gain_stage.contains("Adaptive Limiter or Limiter, else its last Gain plug-in"));
        assert_eq!(cant_yet(&DawProfile::LOGIC, "pursue a goal"), "Kumi can't pursue a goal in Logic Pro yet.");
    }

    #[test]
    fn every_profile_has_a_structural_change_for_every_gap() {
        for profile in [DawProfile::ABLETON, DawProfile::UNKNOWN, DawProfile::REAPER, DawProfile::RENOISE, DawProfile::LOGIC] {
            for (kind, _) in crate::audio::matching::STRUCTURE_MOVES {
                assert!(profile.vocab.structure_moves.iter().any(|(k, _)| k == kind), "{kind:?}");
            }
        }
    }

    #[test]
    fn an_assets_phrases_are_said_once_each_never_again_inside_what_another_became() {
        const PHRASES: &[AssetPhrase] = &[
            AssetPhrase { live: "the Set", count: 1, said: |_| "the project (not the Arrangement)".into() },
            AssetPhrase { live: "the Arrangement", count: 1, said: |_| "the timeline".into() },
        ];
        let asset = serde_json::json!({"description": "Hear the Set, or the Arrangement.", "properties": {"x": {"description": "From the Arrangement."}}});
        assert_eq!(check_asset_phrases(&asset, PHRASES), Err("“the Arrangement” is in the asset 2 times, not 1".into()));
        let asset = serde_json::json!({"description": "Hear the Set, then the Arrangement.", "enum": ["the Set"]});
        let said = asset_in_words(asset, &DawProfile::UNKNOWN, PHRASES);
        assert_eq!(said["description"], "Hear the project (not the Arrangement), then the timeline.");
        assert_eq!(said["enum"][0], "the Set", "only descriptions change");
    }

    #[test]
    fn a_name_comes_with_its_article_and_a_device_in_parentheses_when_there_is_one() {
        assert_eq!(with_article("EQ Eight"), "an EQ Eight");
        assert_eq!(with_article("an EQ"), "an EQ");
        assert_eq!(with_article("timeline"), "a timeline");
        assert_eq!(with_article("Arrangement"), "an Arrangement");
        assert_eq!(parenthetical("EQ Eight"), " (EQ Eight)");
        assert_eq!(parenthetical(""), "");
    }

    #[test]
    fn daw_ids_keep_their_names() {
        assert_eq!(serde_json::to_string(&DawId::ALL).unwrap(), r#"["ableton","reaper","renoise","logic"]"#);
        assert_eq!(DawId::parse("logic"), Some(DawId::Logic));
        assert_eq!(DawId::parse("Logic"), None);
    }
}
