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
}

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
        words: DawWords { app: "the DAW", place: "project", places: "projects" },
        caps: Capabilities::NONE,
        vocab: PromptVocab {
            plan_tool: "make_changes",
            ref_example: "track:3",
            recipe_step_example: "a parameter set by its name on a device an earlier step loaded",
            missing_example: "setting a synth's voice count",
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
        },
    };

    /// Ableton Live, as Kumi drives it today: everything above.
    pub const ABLETON: DawProfile = DawProfile {
        id: Some(DawId::Ableton),
        words: DawWords { app: crate::live_word!(app), place: crate::live_word!(place), places: crate::live_word!(places) },
        caps: Capabilities {
            undo: true,
            instant: true,
            scripting: Some(ScriptKind::Python),
            audio_file: true,
            hear: true,
            audition: true,
            goal_rig: true,
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
        },
    };

    /// The same DAW with nothing it can do now: its words stay (the producer still calls it by its name).
    pub const fn without_caps(self) -> DawProfile {
        DawProfile { caps: Capabilities::NONE, ..self }
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
    fn daw_ids_keep_their_names() {
        assert_eq!(serde_json::to_string(&[DawId::Ableton, DawId::Reaper, DawId::Renoise]).unwrap(), r#"["ableton","reaper","renoise"]"#);
    }
}
