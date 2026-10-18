//! What the listening tools ask of a DAW: its facts (tempo, meter, tracks), hearing the project, Kumi's changes in its
//! history and the level a judged run keeps. The tools themselves (a judged run, a tune, a groove, a form, a sound) are
//! the same whichever DAW is there; a DAW answers these.
use crate::{
    core::{
        contracts::{AuditionCandidate, Render},
        daw::DawProfile,
        errors::RuntimeError,
    },
    listening::{
        checklist::MainState,
        fit::Band,
        judging::Placed,
        knobs::Scale,
        notes::{Kit, Note},
    },
    plugins::roles::Seen,
};
use async_trait::async_trait;
use indexmap::IndexMap;
use kumi_common::abort::Signal;
use std::{any::Any, cell::Cell, path::PathBuf};

/// A stretch of the timeline, in beats.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Window {
    pub from: f64,
    pub beats: f64,
}

/// The bars `window` covers, as said while the DAW plays them: "bars 17–24", or "bar 17".
pub fn bars_of(beats_per_bar: f64, window: Window) -> String {
    let meter = beats_per_bar.max(1.);
    let bar = |beat: f64| (beat / meter).floor() as i64 + 1;
    let (first, last) = (bar(window.from), bar(window.from + window.beats - 1e-6));
    if first >= last {
        format!("bar {first}")
    } else {
        format!("bars {first}–{last}")
    }
}

/// Whether Kumi is playing the project to listen. Main stays as the producer has it, so they hear what Kumi hears (and
/// steer from it).
#[derive(Default)]
pub struct ListenFlag {
    on: Cell<bool>,
}

impl ListenFlag {
    /// Kumi starts playing the project to listen (see `ListenNow`).
    pub fn begin(&self) -> ListenNow<'_> {
        let began = !self.on.replace(true);
        ListenNow { flag: self, began }
    }
    /// Whether Kumi is playing the project to listen now.
    pub fn is_on(&self) -> bool {
        self.on.get()
    }
}

/// Kumi playing the project to listen. However the listen ends, a panic included, dropping it ends it. One taken while
/// another listen runs changes nothing.
pub struct ListenNow<'a> {
    flag: &'a ListenFlag,
    began: bool,
}

impl Drop for ListenNow<'_> {
    fn drop(&mut self) {
        if self.began {
            self.flag.on.set(false);
        }
    }
}

/// What a listen captured: each source's capture by its name, the sources in order with whether each is the mix, and the
/// mix as it comes into the master chain when the DAW heard it there too.
pub struct Captured {
    pub files: IndexMap<String, Render>,
    pub sources: Vec<(String, bool)>,
    pub before_master: Option<Render>,
}

/// One listen through the DAW: what it captured, what the DAW noted, and why the mix wasn't heard as it comes into the
/// master chain when it was tried.
pub struct Pass {
    /// What the listen captured; the error when it failed (the notes are still the DAW's).
    pub rendered: Result<Captured, RuntimeError>,
    /// What went wrong around the listen, or was done to make it work (a source the DAW hears on its own).
    pub notes: Vec<String>,
    /// Why the mix wasn't heard as it comes into the master chain, when the DAW tried (and that it came through empty).
    pub tried: Option<String>,
    /// The most a capture runs before the part asked for, in seconds: a pass that records keeps a short lead for a hit
    /// to rise out of; one that taps the signal has none.
    pub lead: f64,
}

/// A device's knob: where it is, its range, the values it lists (a switch or a menu), and its scale.
#[derive(Debug, Clone)]
pub struct DeviceKnob {
    pub reference: String,
    pub name: String,
    pub raw: f64,
    pub min: f64,
    pub max: f64,
    pub items: Vec<String>,
    pub scale: Option<Scale>,
}

impl DeviceKnob {
    /// The raw value of one of the values it lists.
    pub fn item(&self, wanted: &str) -> Option<f64> {
        let index = self.items.iter().position(|item| item.eq_ignore_ascii_case(wanted))?;
        let steps = (self.items.len().max(2) - 1) as f64;
        Some(self.min + index as f64 * (self.max - self.min) / steps)
    }
}

/// What the DAW sets on an equaliser to draw fitted bands: knobs (by reference) to raw values, in the order to set them,
/// and what else it set to make them land as calculated, in words.
pub struct EqFit {
    pub values: Vec<(String, f64)>,
    pub settings: Vec<&'static str>,
}

/// A clip's notes as the DAW plays them: each with its id (-1 when the DAW gave none), and where its time starts.
pub struct ClipNotes {
    pub name: String,
    pub notes: Vec<(i64, Note)>,
    /// The song beat of the clip's own time zero, for a clip on the timeline.
    pub song_zero: Option<f64>,
    /// Where what the DAW plays of the clip ends, in its own time (its loop's end, say).
    pub to: f64,
    /// Where the clip's notes have to end, in its own time: the DAW refuses a note past it.
    pub end: f64,
}

/// A note's change: where it starts now, how hard, and a shorter length when it would end past the clip.
#[derive(Debug, Clone, PartialEq)]
pub struct NotePatch {
    pub id: i64,
    pub start: f64,
    pub velocity: f64,
    pub duration: Option<f64>,
}

/// Scratch copies of a device's track, made for one search or probe and taken away after it.
pub struct Copies {
    /// The copies' track names, in order.
    pub names: Vec<String>,
    /// What the DAW keeps to set and remove them.
    pub held: Box<dyn Any>,
}

/// The DAW as the listening tools meet it.
#[async_trait(?Send)]
pub trait Renderer {
    /// The DAW's words.
    fn profile(&self) -> &DawProfile;
    /// Why nothing can be heard now (the DAW isn't connected), when it can't.
    fn refusal(&self) -> Option<String>;
    /// The project's tempo, once known.
    fn tempo(&self) -> Option<f64>;
    /// Beats in a bar.
    fn beats_per_bar(&self) -> f64;
    /// Ends when the DAW goes away.
    fn lifetime(&self) -> Signal;
    /// Ends a clean-up after a stopped answer, in its own time.
    fn cleanup(&self) -> Signal;
    /// Says what Kumi is doing, as the app shows it; `playing` when the DAW plays or stops for it.
    fn tell(&self, title: &str, playing: Option<bool>);
    /// Whether Kumi is playing the project to listen.
    fn flag(&self) -> &ListenFlag;
    /// Where captures worth keeping go, out of the listening folder's pruning.
    fn kept_folder(&self) -> PathBuf;

    /// Where the song ends, in beats.
    async fn song_end(&self, signal: Signal) -> Option<f64>;
    /// A track's name, from its reference or its name.
    async fn track_name(&self, named: &str, signal: Signal) -> Option<String>;
    /// A track's long reference, as the history keeps references (positions): from its reference or its name.
    async fn track_ref(&self, named: &str, signal: Signal) -> Option<String>;
    /// The audio file a clip or a file name stands for; None when it names a file itself.
    async fn clip_file(&self, named: &str, signal: Signal) -> Result<Option<String>, RuntimeError>;

    /// Hears `window` of these sources in one pass (the mix is `MIX_CANDIDATE`): with `before_main`, the mix as it comes
    /// into the master chain too, when the DAW can.
    async fn hear_pass(&self, candidates: &[AuditionCandidate], window: Window, before_main: bool, signal: Signal) -> Pass;
    /// The master chain as masking needs it, read as it is now: what moves the mix but not an element heard before it,
    /// and the level its gain devices add. A chain the DAW can't read counts as one masking can't be read through.
    async fn master_chain(&self, signal: Signal) -> MainState;
    /// A track's fader, dB (−inf reads as −120; None when it can't be read).
    async fn fader_of(&self, track: &str, signal: Signal) -> Option<f64>;

    /// Kumi's changes in the history now (applied ones), oldest first.
    fn applied(&self) -> Vec<String>;
    /// Those not in `mark`: ids and titles.
    fn applied_since(&self, mark: &[String]) -> Vec<(String, String)>;
    /// Whether a change can change what's heard; None when the history has no such change.
    fn audible(&self, id: &str) -> Option<bool>;
    /// How much has changed the project outside the history (a count, to compare later).
    fn outside(&self) -> usize;
    /// What changed it outside the history since `mark`, in words.
    fn outside_since(&self, mark: usize) -> Vec<&'static str>;
    /// Kumi's undo of one change; why not, when the DAW wouldn't.
    async fn undo(&self, id: &str) -> Result<(), String>;
    /// The DAW's identities for the devices these changes made, as the history recorded them.
    fn made(&self, changes: &[(String, String)]) -> Vec<String>;
    /// How many devices these changes made (loaded or duplicated).
    fn devices_made(&self, changes: &[(String, String)]) -> usize;
    /// Kumi's applied track renames not among `before`, oldest first: the renamed track's reference, the name it had
    /// and its new name.
    fn renames(&self, before: &[String]) -> Vec<(String, String, String)>;

    /// Turns the level by `gain` dB where rebalancing goes on the run's track (the master without one); says what it
    /// turned, and whether a limiter holds the peaks after it.
    async fn turn_gain(&self, gain: f64, track: Option<&str>, signal: Signal) -> Result<(String, bool), String>;
    /// Whether a limiter after the gain rebalancing turns holds the peaks.
    async fn limited(&self, track: Option<&str>, signal: Signal) -> bool;
    /// The devices on the run's track (the master without one), in order.
    async fn chain(&self, track: Option<&str>) -> Result<Vec<Placed>, String>;
    /// Deletes a device; whether it went.
    async fn delete_device(&self, reference: &str) -> bool;

    /// A device's knobs with the text the DAW shows across each one's range; why not, when it can't be read.
    async fn knobs(&self, device: &str, signal: Signal) -> Result<Result<Vec<DeviceKnob>, String>, RuntimeError>;
    /// Sets knobs (by reference) to raw values in one step.
    async fn set_knobs(&self, device: &str, values: &[(&str, f64)], signal: Signal) -> Result<(), RuntimeError>;
    /// What kind of device it is, as its probes are saved: the DAW's class for its own devices, the name for a plug-in.
    async fn device_kind(&self, device: &str, signal: Signal) -> Option<String>;
    /// The knobs to set so an equaliser's bands are these (the ones it isn't using take them); why not, when it can't.
    async fn fit_eq(
        &self,
        device: &str,
        knobs: &[DeviceKnob],
        bands: &[Band],
        signal: Signal,
    ) -> Result<Result<EqFit, String>, RuntimeError>;
    /// Whether the device is on the track (its reference, as the history keeps them).
    async fn on_track(&self, device: &str, track: &str, signal: Signal) -> bool;
    /// Makes `count` copies of a device's track, for work heard in `passes` passes over `window`.
    async fn open_copies(
        &self,
        device: &str,
        count: usize,
        label: &str,
        passes: u32,
        window: Window,
        signal: Signal,
    ) -> Result<Result<Copies, String>, RuntimeError>;
    /// Sets each copy's knobs (by copy name, knob name and raw value).
    async fn set_copies(
        &self,
        copies: &Copies,
        values: serde_json::Map<String, serde_json::Value>,
        signal: Signal,
    ) -> Result<Result<(), String>, RuntimeError>;
    /// Takes the copies away, whatever happened.
    async fn close_copies(&self, copies: Copies);

    /// The devices on a device's track, as tune sees them, and where the device is among them.
    async fn track_devices(&self, device: &str, signal: Signal) -> Result<(Vec<Seen>, usize), RuntimeError>;
    /// Every parameter name the DAW lists for a plug-in; None when it can't list them.
    async fn listed_names(&self, device: &str, signal: Signal) -> Result<Option<Vec<String>>, RuntimeError>;
    /// A device's turnable parameters, the switches that are on, an equaliser's bands, and for a mapped plug-in what
    /// the DAW lists; left empty when it can't say.
    async fn read_seen(&self, device: &mut Seen, signal: Signal);
    /// An equaliser's bands from its knobs: each band's number, frequency (Hz), whether it's on, and whether its gain
    /// shapes. None for any other device.
    fn eq_bands(&self, knobs: &[DeviceKnob]) -> Vec<(u8, f64, bool, bool)>;
    /// The devices on the run's track (the master without one) as tune sees them, before their parameters are read.
    async fn devices_seen(&self, track: Option<&str>, signal: Signal) -> Option<Vec<Seen>>;

    /// A clip's notes the DAW plays, each with its id; why not, when it can't be read or holds none.
    async fn clip_notes(&self, clip: &str, signal: Signal) -> Result<ClipNotes, String>;
    /// How a clip's notes fall into lanes: by its track's drum rack, when it has one.
    async fn lanes_of(&self, clip: &str, signal: Signal) -> Kit;
    /// The clip read again, so this answer may change it.
    async fn reread_clip(&self, clip: &str, signal: Signal) -> Result<(), RuntimeError>;
    /// Moves notes in one change, so the DAW takes all of it or none and the history takes it back as one.
    async fn change_notes(&self, clip: &str, patches: &[NotePatch], signal: Signal) -> Result<(), RuntimeError>;
    /// The clip's long reference, as the history keeps it.
    fn clip_id(&self, clip: &str) -> Option<String>;
    /// The changes applied since a checkpoint, split into the note edits on a clip (by its long reference) and the rest.
    fn note_changes(&self, checkpoint: &[String], clip: Option<&str>) -> (Vec<(String, String)>, Vec<(String, String)>);
    /// The track a drum stem's clip is on, by name; why not, when it isn't one on the timeline.
    async fn stem_track(&self, audio: &str, signal: Signal) -> Result<String, String>;

    /// Each track's clips on the timeline, by track name (the first 64 tracks): where each that plays starts and ends, in
    /// beats. A muted clip doesn't play; a looped clip ends where it stops playing, not where its loop does.
    async fn clips_by_track(&self, signal: Signal) -> Vec<(String, Vec<(f64, f64)>)>;
}
