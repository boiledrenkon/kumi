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
}

impl DawProfile {
    /// No DAW known: plain words, and nothing it can do.
    pub const UNKNOWN: DawProfile = DawProfile { id: None, words: DawWords { app: "the DAW", place: "project" }, caps: Capabilities::NONE };

    /// Ableton Live, as Kumi drives it today: everything above.
    pub const ABLETON: DawProfile = DawProfile {
        id: Some(DawId::Ableton),
        words: DawWords { app: "Live", place: "Set" },
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

    #[test]
    fn daw_ids_keep_their_names() {
        assert_eq!(serde_json::to_string(&[DawId::Ableton, DawId::Reaper, DawId::Renoise]).unwrap(), r#"["ableton","reaper","renoise"]"#);
    }
}
