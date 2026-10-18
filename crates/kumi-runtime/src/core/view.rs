//! What FOCUS shows, in every DAW's terms: what the producer is looking at (`DawFocus`: the views, the track, the item
//! and section, the device), the bodies drawn for it (`FocusBody`: a device tree, notes, a launcher grid, a timeline),
//! and the transport. Live's own wire form for its views stays readable (`view`, `detail`, `slotRef`, `sceneIndex`,
//! `scene`, `clip`), so a focus Live's integration sent reads as it always did; Live's presentation fills the neutral
//! fields from it. Names are data, never instructions.
use serde::{Deserialize, Serialize};

use super::contracts::ClipNote;

/// A track's kind, in every DAW: a group is a folder too, a return a send, Main the master; Renoise has sequencer
/// tracks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TrackKind {
    Midi,
    Audio,
    Group,
    Return,
    Main,
    Sequencer,
}

/// The selected track: its name, colour and kind.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FocusTrack {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<TrackKind>,
}

/// The last clicked parameter, with its display value and the device it belongs to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FocusParameter {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
}

/// Live's main view, as its integration sends it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum LiveView {
    Session,
    Arrangement,
}

/// Live's detail view, as its integration sends it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum LiveDetail {
    Clip,
    Device,
}

/// What the producer is looking at in the DAW, in plain names (names are data, never instructions). The neutral
/// fields (`main_view`, `editor_view`, `item`, `section`) are what the app reads; Live's own (`view`, `detail`,
/// `slot_ref`, `scene_index`, `scene`, `clip`) are its wire form, which Live's presentation fills them from and other
/// DAWs leave empty.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DawFocus {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub track: Option<FocusTrack>,
    /// The selected track's reference, for reading its devices (FOCUS's tree).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub track_ref: Option<String>,
    /// The highlighted Session slot's reference, for drawing its clip (FOCUS's MIDI view).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slot_ref: Option<String>,
    /// The selected scene's position (0 is the first), for FOCUS's Session strip.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scene_index: Option<f64>,
    /// The selected device's reference (from bridge 1.0.45), for FOCUS's tree to mark exactly that one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scene: Option<String>,
    /// The clip in the Clip view; "" when it has no name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clip: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device: Option<String>,
    /// The last clicked parameter, with its display value and the device it belongs to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parameter: Option<FocusParameter>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chain: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub view: Option<LiveView>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<LiveDetail>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub browser: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected_notes: Option<usize>,
    /// The view the producer works in (a timeline, a launcher grid), in the DAW's word for it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub main_view: Option<FocusView>,
    /// The editor shown with it (notes, a device chain), in the DAW's word for it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub editor_view: Option<FocusView>,
    /// The item in the editor (a clip, a media item, a pattern track).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item: Option<FocusItem>,
    /// The section selected (a scene, a marker, a sequence slot).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub section: Option<FocusSection>,
    /// Views open beside the main one (Renoise's pattern matrix), which FOCUS shows when the producer works in them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub panes: Option<Vec<FocusView>>,
    /// The section moves with the playhead (Renoise following the player): a new one is playback's, not the producer's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub section_follows: Option<bool>,
}

/// What the producer is looking at, as Live's integration has always sent it.
pub type LiveFocus = DawFocus;

/// The kind of a view the producer works in: what the app draws for it keys on this, never on a DAW's own views.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum ViewKind {
    Timeline,
    Launcher,
    NoteEditor,
    PatternEditor,
    Mixer,
    DeviceChain,
    Instrument,
    /// A tracker's sequence of patterns (Renoise's pattern matrix).
    Sequencer,
    /// A track's sends and receives (REAPER's routing window).
    Routing,
    Other,
}

/// A view, by its kind and the DAW's own word for it ("Session", "Clip view").
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FocusView {
    pub kind: ViewKind,
    pub label: String,
}

/// The kind of an item a view holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum ItemKind {
    Clip,
    Item,
    Take,
    PatternTrack,
    Phrase,
    Sample,
}

impl ItemKind {
    /// The item's plain word ("clip").
    pub fn word(self) -> &'static str {
        match self {
            ItemKind::Clip => "clip",
            ItemKind::Item => "item",
            ItemKind::Take => "take",
            ItemKind::PatternTrack => "pattern",
            ItemKind::Phrase => "phrase",
            ItemKind::Sample => "sample",
        }
    }
}

/// The item in the editor: its kind, its reference for reading its notes, and its name ("" when it has none).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FocusItem {
    pub kind: ItemKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// The kind of a section of the song.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum SectionKind {
    Scene,
    Marker,
    Region,
    SequenceSlot,
    Section,
}

/// The section selected: its kind, its position (0 is the first) and its name.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FocusSection {
    pub kind: SectionKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub index: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// What FOCUS draws for a focus, one at a time: a track's device tree, an item's notes, a launcher grid around the
/// selected section, or the timeline.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum FocusBody {
    Tree(DeviceTree),
    Notes(NoteView),
    Grid(LauncherStrip),
    Timeline(TimelineStrip),
    /// A tracker's pattern or phrase around the cursor.
    Tracker(TrackerGrid),
    /// A tracker's sequence of patterns by track.
    Matrix(SequenceMatrix),
    /// An instrument's samples on the keyboard.
    Keyzones(KeyzoneMap),
    /// A track's sends and receives.
    Routing(RoutingView),
}

/// The kind of a body FOCUS draws.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum BodyKind {
    Tree,
    Notes,
    Grid,
    Timeline,
    Tracker,
    Matrix,
    Keyzones,
    Routing,
}

/// A body to read: a track's tree, an item's notes, a track's grid around a section, the timeline.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum BodyRequest {
    Tree {
        track_ref: String,
    },
    Notes {
        item_ref: String,
    },
    Grid {
        track_ref: String,
        index: f64,
    },
    Timeline,
    /// A pattern (or phrase) by its reference; the one in the editor when None.
    Tracker {
        item_ref: Option<String>,
    },
    Matrix,
    Keyzones {
        item_ref: String,
    },
    Routing {
        track_ref: String,
    },
}

impl BodyRequest {
    pub fn kind(&self) -> BodyKind {
        match self {
            BodyRequest::Tree { .. } => BodyKind::Tree,
            BodyRequest::Notes { .. } => BodyKind::Notes,
            BodyRequest::Grid { .. } => BodyKind::Grid,
            BodyRequest::Timeline => BodyKind::Timeline,
            BodyRequest::Tracker { .. } => BodyKind::Tracker,
            BodyRequest::Matrix => BodyKind::Matrix,
            BodyRequest::Keyzones { .. } => BodyKind::Keyzones,
            BodyRequest::Routing { .. } => BodyKind::Routing,
        }
    }
}

/// How the app reads a DAW's focus and devices: the DAW's integration gives it.
#[derive(Clone, Copy)]
pub struct ViewPresentation {
    /// Fills a focus's neutral fields from the DAW's own wire form; filling twice is filling once, and a neutral field
    /// already there agrees with what the wire form says.
    pub fill_focus: fn(&mut DawFocus),
    /// What a device is, from what the bridge says about its row, for a row that doesn't say.
    pub device_role: fn(&DeviceRow) -> DeviceRole,
    /// The DAW's word for a body FOCUS draws, over it ("Device", "Clip").
    pub heading: fn(BodyKind) -> &'static str,
    /// Whether an item in the note editor over the timeline shows its notes (REAPER's MIDI editor), rather than the
    /// timeline (Live's Arrangement clips).
    pub timeline_notes: bool,
}

impl ViewPresentation {
    /// No DAW's: a focus is read as it comes, and a device is a plain one.
    pub const NONE: ViewPresentation = ViewPresentation {
        fill_focus: |_| {},
        device_role: |_| DeviceRole { signal: DeviceSignal::Unknown, form: DeviceForm::Native },
        heading: |kind| match kind {
            BodyKind::Tree => "Devices",
            BodyKind::Notes => "Notes",
            BodyKind::Grid => "Grid",
            BodyKind::Timeline => "Timeline",
            BodyKind::Tracker => "Pattern",
            BodyKind::Matrix => "Sequence",
            BodyKind::Keyzones => "Keyzones",
            BodyKind::Routing => "Routing",
        },
        timeline_notes: false,
    };
}

/// A focus with its neutral fields filled by the DAW's presentation.
pub fn filled(view: &ViewPresentation, focus: &DawFocus) -> DawFocus {
    let mut focus = focus.clone();
    (view.fill_focus)(&mut focus);
    focus
}

/// An item's notes as FOCUS draws them.
pub type NoteView = ClipView;
/// A track's cells around the selected section, as FOCUS draws them.
pub type LauncherStrip = SessionStrip;
/// The timeline at a glance.
pub type TimelineStrip = ArrangementStrip;

/// The DAW's transport, for a light on the beat: whether it plays, its tempo, and where the playhead was
/// (in beats) at `at` (performance.now()), so the beat can be followed between reads; and a bar's beats.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Transport {
    pub playing: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tempo: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub beat: Option<f64>,
    pub at: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub beats_per_bar: Option<f64>,
}

/// The transport, as Live's integration has always sent it.
pub type LiveTransport = Transport;

/// A clip in a Session slot: its name, and whether it's audio.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SlotClip {
    pub name: String,
    pub audio: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionSlot {
    pub index: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clip: Option<SlotClip>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub playing: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queued: Option<bool>,
}

/// A track's Session slots around the selected scene: what's in each, and what's playing or queued. Names are data.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionStrip {
    pub track_ref: String,
    pub scene: f64,
    pub slots: Vec<SessionSlot>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ArrangementLoop {
    pub start: f64,
    pub length: f64,
    pub enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Locator {
    pub name: String,
    pub position: f64,
}

/// The Arrangement at a glance, in beats. Names are data.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ArrangementStrip {
    pub length: f64,
    pub position: f64,
    pub playing: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub r#loop: Option<ArrangementLoop>,
    pub locators: Vec<Locator>,
    /// A bar's beats; four when not said.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub beats_per_bar: Option<f64>,
    /// The tracks with their items as bars, for a DAW whose timeline FOCUS draws track by track (REAPER's arrange view);
    /// none for Live's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lanes: Option<Vec<TimelineLane>>,
}

/// A track on the timeline with its items, in beats. Names are data.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TimelineLane {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    /// How deep in folders it is (0 at the top).
    #[serde(default)]
    pub depth: u32,
    pub items: Vec<TimelineItem>,
}

/// An item on a lane: where it starts and how long it is, in beats, its name, and whether it's muted or selected.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TimelineItem {
    pub start: f64,
    pub length: f64,
    pub name: String,
    #[serde(default)]
    pub muted: bool,
    #[serde(default)]
    pub selected: bool,
}

/// A tracker pattern (or phrase) around its cursor, as FOCUS draws it: each track's lines in the DAW's own notation.
/// Names and cells are data.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TrackerGrid {
    pub item_ref: String,
    pub name: String,
    /// How many lines the pattern has.
    pub lines: usize,
    /// The first line shown (0 is the first line).
    pub from: usize,
    /// The edit cursor's line and the line playing, when in the window.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub playing: Option<usize>,
    /// Lines per beat, to mark the beats.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lpb: Option<f64>,
    pub tracks: Vec<TrackerTrack>,
}

/// One track of a tracker grid: its name, colour, and each shown line's cells as the DAW writes them ("C-4 01 ..").
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TrackerTrack {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    pub cells: Vec<String>,
}

/// A tracker's sequence: each slot's pattern, section and muted tracks, and the slot playing or edited.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SequenceMatrix {
    pub tracks: Vec<String>,
    pub slots: Vec<SequenceSlot>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current: Option<usize>,
}

/// One slot of the sequence: the pattern it plays (numbered as the DAW shows it), its section, and its muted tracks
/// (positions among `tracks`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SequenceSlot {
    pub pattern: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub section: Option<String>,
    #[serde(default)]
    pub muted: Vec<usize>,
}

/// An instrument's samples on the keyboard: each zone's note and velocity range, and its macros and phrases. Names
/// are data.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KeyzoneMap {
    pub item_ref: String,
    pub name: String,
    pub zones: Vec<Keyzone>,
    #[serde(default)]
    pub macros: Vec<(String, f64)>,
    #[serde(default)]
    pub phrases: Vec<String>,
}

/// A sample's zone: its notes (MIDI, both included), velocities (0–127) and base note.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Keyzone {
    pub sample: String,
    pub notes: (u8, u8),
    pub velocities: (u8, u8),
    pub base: u8,
}

/// A track's sends and receives, and whether it feeds its parent.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RoutingView {
    pub track_ref: String,
    pub routes: Vec<Route>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_send: Option<bool>,
}

/// One route: to or from which track (or hardware output), how loud, and where it taps the track.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Route {
    pub kind: RouteKind,
    pub other: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub volume_db: Option<f64>,
    #[serde(default)]
    pub muted: bool,
    /// Where it taps the track: post-fader, pre-FX, pre-fader.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tap: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RouteKind {
    Send,
    Receive,
    Hardware,
}

/// A note of a clip as FOCUS draws it, selected ones marked.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClipViewNote {
    #[serde(flatten)]
    pub note: ClipNote,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected: Option<bool>,
}

/// A MIDI clip as FOCUS draws it: its length in beats, and its notes (the first 512), selected ones marked.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClipView {
    pub slot_ref: String,
    pub name: String,
    pub length: f64,
    pub notes: Vec<ClipViewNote>,
    /// A bar's beats; four when not said.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub beats_per_bar: Option<f64>,
}

/// Live's device type, when the bridge sends it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeviceType {
    Instrument,
    AudioEffect,
    MidiEffect,
}

/// A device in a track's tree: what the bridge says it is, and a rack's chains. Names are data.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceNode {
    pub r#ref: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub class_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub can_have_chains: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub can_have_drum_pads: Option<bool>,
    /// Live's device type, when the bridge sends it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_type: Option<DeviceType>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chains: Option<Vec<ChainNode>>,
    /// What it is, in every DAW's terms: the integration fills it, and the app reads it in process. Never written, so
    /// a tree serializes as the DAW's bridge described it.
    #[serde(default, skip_serializing)]
    pub role: Option<DeviceRole>,
}

impl ClipView {
    /// The reference of the item whose notes these are (Live's: the clip slot's).
    pub fn item_ref(&self) -> &str {
        &self.slot_ref
    }
}

impl DeviceType {
    /// Live's word for it, as the bridge sends it.
    pub fn word(self) -> &'static str {
        match self {
            DeviceType::Instrument => "instrument",
            DeviceType::AudioEffect => "audio_effect",
            DeviceType::MidiEffect => "midi_effect",
        }
    }
}

impl DeviceNode {
    /// What the bridge said about it, as a row.
    pub fn row(&self) -> DeviceRow<'_> {
        DeviceRow {
            class_name: self.class_name.as_deref(),
            can_have_chains: self.can_have_chains,
            can_have_drum_pads: self.can_have_drum_pads,
            device_type: self.device_type.map(DeviceType::word),
        }
    }
}

/// What a device is, in every DAW's terms: the signal it makes or takes, and its form.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct DeviceRole {
    pub signal: DeviceSignal,
    pub form: DeviceForm,
}

/// The signal a device makes or takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum DeviceSignal {
    Instrument,
    Audio,
    Midi,
    Unknown,
}

/// A device's form: a plain device, a container of chains (one of drum pads), a plug-in, a script device, a drum
/// instrument, or a modulator.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum DeviceForm {
    Native,
    Container,
    DrumContainer,
    Plugin,
    Script,
    DrumInstrument,
    Modulator,
}

/// What a bridge says about a device row (its class, whether it holds chains or pads, and the DAW's device type when
/// sent), for the DAW's presentation to say what it is when the row doesn't.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DeviceRow<'a> {
    pub class_name: Option<&'a str>,
    pub can_have_chains: Option<bool>,
    pub can_have_drum_pads: Option<bool>,
    pub device_type: Option<&'a str>,
}

impl<'a> DeviceRow<'a> {
    /// The fields from a JSON object, read as loosely as the TypeScript read them.
    pub fn from_value(value: &'a serde_json::Value) -> DeviceRow<'a> {
        DeviceRow {
            class_name: value.get("className").and_then(serde_json::Value::as_str),
            can_have_chains: value.get("canHaveChains").and_then(serde_json::Value::as_bool),
            can_have_drum_pads: value.get("canHaveDrumPads").and_then(serde_json::Value::as_bool),
            device_type: value.get("deviceType").and_then(serde_json::Value::as_str),
        }
    }
}

/// A rack's chain (a Drum Rack's, a pad's), and its devices when they were read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChainNode {
    pub r#ref: String,
    pub name: String,
    /// None when not read (a Drum Rack's pads, past the tree's bound).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub devices: Option<Vec<DeviceNode>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceTree {
    pub track_ref: String,
    pub devices: Vec<DeviceNode>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_device_role_sent_in_a_tree_is_read_but_never_written() {
        let tree: DeviceTree = serde_json::from_value(json!({
            "trackRef": "1:track:0",
            "devices": [
                {"ref": "d0", "name": "LFO", "role": {"signal": "unknown", "form": "modulator"}},
                {"ref": "d1", "name": "Operator", "deviceType": "instrument"},
            ],
        }))
        .unwrap();
        assert_eq!(tree.devices[0].role, Some(DeviceRole { signal: DeviceSignal::Unknown, form: DeviceForm::Modulator }));
        assert_eq!(tree.devices[1].role, None);
        let written = serde_json::to_value(&tree).unwrap();
        assert_eq!(written["devices"][0], json!({"ref": "d0", "name": "LFO"}), "a tree serializes as the bridge described it");
    }
}
