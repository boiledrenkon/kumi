//! What the listening tools ask of a DAW: its facts (tempo, meter, tracks), hearing the project, Kumi's changes in its
//! history and the level a judged run keeps. The tools themselves (a judged run, a tune, a groove, a form, a sound) are
//! the same whichever DAW is there; a DAW answers these.
use crate::{
    core::{
        contracts::{AuditionCandidate, Render},
        daw::DawProfile,
        errors::RuntimeError,
    },
    listening::{checklist::MainState, judging::Placed},
};
use async_trait::async_trait;
use indexmap::IndexMap;
use kumi_common::abort::Signal;
use std::{cell::Cell, path::PathBuf};

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
}
