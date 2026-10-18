//! The listening tools' engines, apart from any DAW: a judged run (the judge, and the tune that picks its numbers) over
//! what the DAW can do (`Renderer`). The DAW hears, changes and undoes; the engines decide.
mod judge;
mod probe;
mod roles;
mod tune;
#[cfg(test)]
pub(crate) use judge::prune_unstored;
pub(crate) use judge::{masking_items, JudgeHeard};
pub use judge::{JudgeRun, Named};

use super::{
    listener::Listener,
    renderer::{bars_of, Renderer, Window},
    round::Round,
};
use crate::{
    audio,
    core::{context::ListenerSource, errors::RuntimeError},
    references::store::ReferenceStore,
};
use kumi_common::{abort::Signal, js::number::round};
use std::{
    cell::{Cell, RefCell},
    path::PathBuf,
    rc::Rc,
};

/// The tools' state across calls (a judged run), and the DAW they listen through.
pub struct Listening {
    pub(crate) host: Rc<dyn Renderer>,
    on_judge: Option<Rc<dyn Fn(Round)>>,
    /// The judged run under way (or the last one).
    pub(crate) judge: RefCell<Option<JudgeRun>>,
    listener_source: Option<ListenerSource>,
    /// Measured references, read by name for the judge.
    references: Option<Rc<ReferenceStore>>,
    /// Whether the run's listens are heard by the learned models too: the style model, the effects model.
    pub(crate) embedding_wanted: Cell<(bool, bool)>,
    /// The style model a judge run hears with, fixed when it starts: the embeddings slot's file then (None: Kumi's
    /// own) and its identity. A slot swapped during a run doesn't change it.
    style_model: RefCell<Option<(Option<PathBuf>, String)>>,
    /// The listening model as last looked up (found, a definite none, or why the lookup failed), and when.
    listener: RefCell<Option<(Result<Option<Rc<dyn Listener>>, String>, i64)>>,
}

impl Listening {
    pub fn new(
        host: Rc<dyn Renderer>,
        on_judge: Option<Rc<dyn Fn(Round)>>,
        listener_source: Option<ListenerSource>,
        references: Option<Rc<ReferenceStore>>,
    ) -> Rc<Self> {
        Rc::new(Self {
            host,
            on_judge,
            judge: RefCell::new(None),
            listener_source,
            references,
            embedding_wanted: Cell::new((false, false)),
            style_model: RefCell::new(None),
            listener: RefCell::new(None),
        })
    }

    /// A new answer, not the match run's continuing one: what the producer asked for since isn't a judged run's change to
    /// take back, so its changes count from `ids` (the changes applied now). If that changed the sound, the run's
    /// numbers are out of date: its next judge hears its bars again first.
    pub fn new_answer(&self, ids: Vec<String>) {
        let mut guard = self.judge.borrow_mut();
        if let Some(run) = guard.as_mut() {
            let audible = |id: &String| self.host.audible(id) == Some(true);
            // An undo in between counts too: a change the run's numbers took in that isn't in the project any more.
            let made = self.applied_since(&run.checkpoint).iter().any(|(id, _)| audible(id));
            let undone = run.checkpoint.iter().any(|id| !ids.contains(id) && audible(id));
            // Python run in the DAW, a command or its own undo may have changed it too, outside the history.
            let outside = !self.host.outside_since(run.outside).is_empty();
            if made || undone || outside {
                run.stale = true;
            }
            run.checkpoint = ids;
            run.outside = self.host.outside();
            // The steer was this answer's: the new answer's changes are its own again.
            run.steered = None;
        }
    }

    /// The producer's words came into the answer under way: the judged run's changes from here may be theirs, so they
    /// aren't a round's to take back. The first steer marks where (`applied`, the changes applied now); the run's next
    /// round takes back only what came before it, then starts again from what's in the project.
    pub fn steered(&self, applied: Vec<String>) {
        if let Some(run) = self.judge.borrow_mut().as_mut() {
            run.steered.get_or_insert_with(|| applied.clone());
        }
    }

    /// What Kumi says when the project's tempo isn't known yet.
    pub(crate) fn no_tempo(&self) -> String {
        format!("Kumi doesn't know the {}'s tempo yet; try again.", self.host.profile().words.place)
    }

    /// Says what Kumi is doing.
    pub(crate) fn tell(&self, title: impl AsRef<str>, playing: Option<bool>) {
        self.host.tell(title.as_ref(), playing);
    }

    /// The bars `window` covers, as said while the DAW plays them: "bars 17–24", or "bar 17".
    pub(crate) fn bars_of(&self, window: Window) -> String {
        bars_of(self.host.beats_per_bar(), window)
    }

    /// Kumi's changes in the history now (applied ones), to tell a round's own from what came before.
    pub(crate) fn applied_ids(&self) -> Vec<String> {
        self.host.applied()
    }

    /// The applied changes since a checkpoint, oldest first: their ids and titles.
    pub(crate) fn applied_since(&self, checkpoint: &[String]) -> Vec<(String, String)> {
        self.host.applied_since(checkpoint)
    }

    /// The audio file a reference names: a clip's, else the name itself as a file. A clipRef that's stale, gone or a
    /// MIDI clip's says so, rather than reading as a file that isn't there.
    pub(crate) async fn reference_file(&self, named: &str, signal: Signal) -> Result<Result<String, String>, RuntimeError> {
        match self.host.clip_file(named, signal).await {
            Ok(file) => Ok(Ok(file.unwrap_or_else(|| audio::audio_path(named)))),
            Err(RuntimeError::Aborted) => Err(RuntimeError::Aborted),
            Err(error) => Ok(Err(error.to_string())),
        }
    }
}

/// The longest stretch one listen hears, in seconds (a judged run's span, a form's, a sound's and an audition's too).
pub(crate) const LONGEST_LISTEN: f64 = 3600.;

/// Why a stretch of `beats` is too long to hear at once, when it is, and what to do instead.
pub(crate) fn too_long(beats: f64, tempo: f64, instead: &str) -> Option<String> {
    let seconds = beats * 60. / tempo;
    (seconds > LONGEST_LISTEN || !seconds.is_finite())
        .then(|| format!("That's {} of music; Kumi listens to at most an hour at once, so {instead}.", clock(seconds)))
}

fn clock(seconds: f64) -> String {
    let whole = round(seconds) as i64;
    format!("{}:{:02}", whole / 60, whole % 60)
}
