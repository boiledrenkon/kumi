//! Kumi's changes to a REAPER project (M3): a preview that writes nothing, an apply fenced by what the preview saw, and a
//! restore fenced by what the apply wrote. Every write is one undo block named by the host (`desc`, with its nonce), so
//! the producer's own Ctrl-Z takes a change back whole. Kumi's undo never calls REAPER's: REAPER's undo also reverts the
//! producer's bare edits since the last undo point, so a change is put back by values (see `restore`).
//!
//! Each kind knows three things: what it would do (`preview`), how to do it (`apply`, which re-reads what the preview fenced
//! and refuses `changed` before it writes anything), and how to put it back (`restore`, which refuses `changed` if the
//! fields it touched no longer hold what the apply wrote). A kind never leaves half a change: it checks first.

mod call;
mod folders;
mod fx;
mod lua;
mod marks;
mod notes;
mod pins;
mod project;
mod sends;
pub(crate) use sends::seen_holds as send_seen_holds;
mod structure;
mod tracks;
mod undo;
// (r-tracks) M4: track properties, routing, project items, actions; NIT-7.
mod volatile;

use std::cell::{Cell, RefCell};
use std::time::{Duration, Instant};

use serde_json::{json, Map, Value};

use crate::extension::Outbox;
use crate::queue::Answer;
use crate::read::{bad, failed, Reads};
use crate::reaper::{Project, Reaper};
use crate::refs::Ref;

/// An error as the wire gives it: the code, and words for the model.
pub type Fault = (String, String);
pub type Done<T> = Result<T, Fault>;

/// The step a plan holds open at most: a host that never ended it must not leave REAPER's undo history in a block.
const PLAN_MAX: Duration = Duration::from_secs(120);

/// What a kind needs of the world it changes.
pub struct Ctx<'a, 'r> {
    pub reaper: &'a Reaper<'r>,
    pub reads: &'a Reads,
    pub proj: Project,
}

/// What a change would do. `prior` is what undo puts back; `fence` what apply checks again before it writes.
pub struct Preview {
    pub target: Value,
    pub prior: Value,
    pub fence: Value,
    pub display: Value,
}

#[derive(Default)]
pub struct Applied {
    pub applied: Value,
    pub created: Option<Value>,
    pub material: Option<Value>,
    pub display: Value,
    /// Nothing was written (it was so already): the block closes without a point, so it commits nothing of the producer's.
    pub unwritten: bool,
}

/// What a restore is told: the change as the journal kept it.
pub struct Restore<'a> {
    pub target: &'a Value,
    pub prior: &'a Value,
    pub applied: &'a Value,
    pub created: Option<&'a Value>,
    pub material: Option<&'a Value>,
    pub discard: bool,
}

#[derive(Default)]
pub struct Restored {
    pub restored: Value,
    pub display: Value,
    /// Nothing was written (it was as it was already, or a retry): see `Applied`.
    pub unwritten: bool,
}

/// What an apply or a restore says of itself that the block it ran in needs to know.
trait Written {
    fn unwritten(&self) -> bool;
}

impl Written for Applied {
    fn unwritten(&self) -> bool {
        self.unwritten
    }
}

impl Written for Restored {
    fn unwritten(&self) -> bool {
        self.unwritten
    }
}

/// A kind of change.
#[derive(Clone, Copy)]
enum Kind {
    TrackProp(&'static tracks::Prop),
    TrackDelete,
    TrackAdd,
    TrackMove,
    Fx(fx::Which),
    Notes(notes::Mode),
    Tempo,
    TempoMarker,
    TempoMarkerDelete,
    LoopRange,
    LoopRepeat,
    SendProp(&'static sends::Prop),
    MarkProp(&'static marks::Prop),
    MarkerAdd,
    RegionAdd,
    MarkerDelete,
    SendAdd,
    SendRemove,
    /// An FX's pins: the channels each takes from or gives to.
    FxPins,
    /// A Lua chunk, run as a ReaScript (only REAPER's undo takes it back).
    RunLua,
    /// A REAPER API function that changes the project, called by name (only REAPER's undo takes it back).
    ApiCall,
}

fn kind(name: &str) -> Option<Kind> {
    if let Some(prop) = tracks::PROPS.iter().find(|prop| prop.kind == name) {
        return Some(Kind::TrackProp(prop));
    }
    if let Some(prop) = sends::PROPS.iter().find(|prop| prop.kind == name) {
        return Some(Kind::SendProp(prop));
    }
    if let Some(prop) = marks::PROPS.iter().find(|prop| prop.kind == name) {
        return Some(Kind::MarkProp(prop));
    }
    match name {
        "fx-add" => Some(Kind::Fx(fx::Which::Add)),
        "fx-delete" => Some(Kind::Fx(fx::Which::Delete)),
        "fx-move" => Some(Kind::Fx(fx::Which::Move)),
        "fx-bypass" => Some(Kind::Fx(fx::Which::Bypass)),
        "fx-param" => Some(Kind::Fx(fx::Which::Param)),
        "notes-add" => Some(Kind::Notes(notes::Mode::Add)),
        "notes-change" => Some(Kind::Notes(notes::Mode::Change)),
        "notes-delete" => Some(Kind::Notes(notes::Mode::Delete)),
        "tempo-set" => Some(Kind::Tempo),
        "tempo-marker-set" => Some(Kind::TempoMarker),
        "tempo-marker-delete" => Some(Kind::TempoMarkerDelete),
        "loop-range" => Some(Kind::LoopRange),
        "loop-repeat" => Some(Kind::LoopRepeat),
        "marker-add" => Some(Kind::MarkerAdd),
        "region-add" => Some(Kind::RegionAdd),
        "marker-delete" => Some(Kind::MarkerDelete),
        "send-add" => Some(Kind::SendAdd),
        "send-remove" => Some(Kind::SendRemove),
        "track-delete" => Some(Kind::TrackDelete),
        "track-add" => Some(Kind::TrackAdd),
        "track-move" => Some(Kind::TrackMove),
        "fx-pins" => Some(Kind::FxPins),
        "run-lua" => Some(Kind::RunLua),
        "api-call" => Some(Kind::ApiCall),
        _ => None,
    }
}

impl Kind {
    /// Whether a change of this kind leaves a point in REAPER's undo history: the repeat button does not.
    fn leaves_point(self) -> bool {
        !matches!(self, Kind::LoopRepeat)
    }

    fn preview(self, ctx: &Ctx, input: &Value) -> Done<Preview> {
        match self {
            Kind::TrackProp(prop) => tracks::prop_preview(ctx, prop, input),
            Kind::TrackDelete => tracks::delete_preview(ctx, input),
            Kind::TrackAdd => structure::add_preview(ctx, input),
            Kind::TrackMove => structure::move_preview(ctx, input),
            Kind::SendProp(prop) => sends::prop_preview(ctx, prop, input),
            Kind::SendAdd => sends::add_preview(ctx, input),
            Kind::SendRemove => sends::remove_preview(ctx, input),
            Kind::MarkProp(prop) => marks::prop_preview(ctx, prop, input),
            Kind::MarkerAdd => marks::add_preview(ctx, input, false),
            Kind::RegionAdd => marks::add_preview(ctx, input, true),
            Kind::MarkerDelete => marks::delete_preview(ctx, input),
            Kind::Fx(which) => fx::preview(ctx, input, which),
            Kind::Notes(mode) => notes::preview(ctx, input, mode),
            Kind::Tempo => project::tempo_preview(ctx, input),
            Kind::TempoMarker => project::marker_preview(ctx, input),
            Kind::TempoMarkerDelete => project::marker_delete_preview(ctx, input),
            Kind::LoopRange => project::loop_preview(ctx, input),
            Kind::LoopRepeat => project::repeat_preview(ctx, input),
            Kind::FxPins => pins::preview(ctx, input),
            Kind::RunLua => lua::preview(ctx, input),
            Kind::ApiCall => call::preview(ctx, input),
        }
    }

    fn apply(self, ctx: &Ctx, input: &Value, fence: &Value) -> Done<Applied> {
        match self {
            Kind::TrackProp(prop) => tracks::prop_apply(ctx, prop, input, fence),
            Kind::TrackDelete => tracks::delete_apply(ctx, input, fence),
            Kind::TrackAdd => structure::add_apply(ctx, input, fence),
            Kind::TrackMove => structure::move_apply(ctx, input, fence),
            Kind::SendProp(prop) => sends::prop_apply(ctx, prop, input, fence),
            Kind::SendAdd => sends::add_apply(ctx, input, fence),
            Kind::SendRemove => sends::remove_apply(ctx, input, fence),
            Kind::MarkProp(prop) => marks::prop_apply(ctx, prop, input, fence),
            Kind::MarkerAdd => marks::add_apply(ctx, input, false),
            Kind::RegionAdd => marks::add_apply(ctx, input, true),
            Kind::MarkerDelete => marks::delete_apply(ctx, input, fence),
            Kind::Fx(which) => fx::apply(ctx, input, fence, which),
            Kind::Notes(mode) => notes::apply(ctx, input, fence, mode),
            Kind::Tempo => project::tempo_apply(ctx, input, fence),
            Kind::TempoMarker => project::marker_apply(ctx, input, fence),
            Kind::TempoMarkerDelete => project::marker_delete_apply(ctx, input, fence),
            Kind::LoopRange => project::loop_apply(ctx, input, fence),
            Kind::LoopRepeat => project::repeat_apply(ctx, input, fence),
            Kind::FxPins => pins::apply(ctx, input, fence),
            Kind::RunLua => lua::apply(ctx, input, fence),
            Kind::ApiCall => call::apply(ctx, input, fence),
        }
    }

    fn restore(self, ctx: &Ctx, restore: &Restore) -> Done<Restored> {
        match self {
            Kind::TrackProp(prop) => tracks::prop_restore(ctx, prop, restore),
            Kind::TrackDelete => tracks::delete_restore(ctx, restore),
            Kind::TrackAdd => structure::add_restore(ctx, restore),
            Kind::TrackMove => structure::move_restore(ctx, restore),
            Kind::SendProp(prop) => sends::prop_restore(ctx, prop, restore),
            Kind::SendAdd => sends::add_restore(ctx, restore),
            Kind::SendRemove => sends::remove_restore(ctx, restore),
            Kind::MarkProp(prop) => marks::prop_restore(ctx, prop, restore),
            Kind::MarkerAdd | Kind::RegionAdd => marks::add_restore(ctx, restore),
            Kind::MarkerDelete => marks::delete_restore(ctx, restore),
            Kind::Fx(which) => fx::restore(ctx, restore, which),
            Kind::Notes(_) => notes::restore(ctx, restore),
            Kind::Tempo => project::tempo_restore(ctx, restore),
            Kind::TempoMarker => project::marker_restore(ctx, restore),
            Kind::TempoMarkerDelete => project::marker_delete_restore(ctx, restore),
            Kind::LoopRange => project::loop_restore(ctx, restore),
            Kind::LoopRepeat => project::repeat_restore(ctx, restore),
            Kind::FxPins => pins::restore(ctx, restore),
            Kind::RunLua => lua::restore(ctx, restore),
            Kind::ApiCall => call::restore(ctx, restore),
        }
    }
}

/// A plan's one undo block, held open across the changes in it.
struct Plan {
    id: String,
    label: String,
    proj: Project,
    epoch: String,
    started: Instant,
}

#[derive(Default)]
pub struct Changes {
    plan: RefCell<Option<Plan>>,
    steps: Cell<u64>,
    watch: RefCell<undo::Watch>,
}

fn text<'a>(params: &'a Value, name: &str) -> Done<&'a str> {
    params.get(name).and_then(Value::as_str).filter(|text| !text.is_empty()).ok_or_else(|| bad(format!("{name} must be a string")))
}

fn object<'a>(params: &'a Value, name: &str) -> Done<&'a Value> {
    params.get(name).filter(|value| value.is_object()).ok_or_else(|| bad(format!("{name} must be an object")))
}

/// The kind a request names.
fn kind_of(params: &Value) -> Done<Kind> {
    let name = text(params, "kind")?;
    kind(name).ok_or_else(|| bad(format!("REAPER has no change kind {name:?}")))
}

impl Changes {
    /// The project the request is about, if the epoch it names is this project's: a request made in another tab, or before
    /// the project in this one was replaced, names refs that mean something else now.
    fn context<'a, 'r>(&self, reaper: &'a Reaper<'r>, reads: &'a Reads, params: &Value) -> Done<Ctx<'a, 'r>> {
        self.context_from(reaper, reads, params, false)
    }

    /// As `context`; with `past` (an undo, whose refs are ids that hold across a look at another tab and back) an earlier
    /// epoch of the project that is open now is accepted too.
    fn context_from<'a, 'r>(&self, reaper: &'a Reaper<'r>, reads: &'a Reads, params: &Value, past: bool) -> Done<Ctx<'a, 'r>> {
        let epoch = text(params, "epoch").map_err(|_| bad("epoch: the epoch the refs were read in"))?;
        let of_this_project = past && reaper.current_project().is_some_and(|info| reads.epoch_is_of(epoch, info.ptr));
        if epoch != reads.epoch() && !of_this_project {
            return Err((
                daw_bridge::wire::code::STALE.into(),
                format!(
                    "those refs are from an earlier epoch ({epoch}; now {}): the project in the tab changed, read again",
                    reads.epoch()
                ),
            ));
        }
        if let Some(busy) = reaper.busy() {
            return Err((daw_bridge::wire::code::BUSY.into(), format!("{}; nothing changed", busy.reason())));
        }
        let info = reaper.current_project().ok_or_else(|| failed("REAPER has no project open"))?;
        Ok(Ctx { reaper, reads, proj: info.ptr })
    }

    pub fn preview(&self, reaper: &Reaper, reads: &Reads, params: &Value) -> Answer {
        let ctx = self.context(reaper, reads, params)?;
        let kind = kind_of(params)?;
        let preview = kind.preview(&ctx, object(params, "input")?)?;
        Ok(json!({
            "epoch": reads.epoch(),
            "target": preview.target,
            "prior": preview.prior,
            "fence": preview.fence,
            "display": preview.display,
        }))
    }

    pub fn apply(&self, reaper: &Reaper, reads: &Reads, params: &Value) -> Answer {
        let ctx = self.context(reaper, reads, params)?;
        let kind = kind_of(params)?;
        let (input, fence, desc) = (object(params, "input")?, object(params, "fence")?, text(params, "desc")?);
        let applied = self.in_block(&ctx, desc, kind.leaves_point(), |ctx| kind.apply(ctx, input, fence))?;
        let (applied, undo) = applied;
        let mut answer = Map::new();
        answer.insert("epoch".into(), reads.epoch().into());
        answer.insert("applied".into(), applied.applied);
        if let Some(created) = applied.created {
            answer.insert("created".into(), created);
        }
        if let Some(material) = applied.material {
            answer.insert("material".into(), material);
        }
        answer.insert("display".into(), applied.display);
        answer.insert("undo".into(), undo);
        Ok(Value::Object(answer))
    }

    pub fn restore(&self, reaper: &Reaper, reads: &Reads, params: &Value) -> Answer {
        let ctx = self.context_from(reaper, reads, params, true)?;
        let kind = kind_of(params)?;
        let desc = text(params, "desc")?;
        let restore = Restore {
            target: object(params, "target")?,
            prior: params.get("prior").unwrap_or(&Value::Null),
            applied: params.get("applied").unwrap_or(&Value::Null),
            created: params.get("created").filter(|v| !v.is_null()),
            material: params.get("material").filter(|v| !v.is_null()),
            discard: params.get("discard") == Some(&Value::Bool(true)),
        };
        let (restored, undo) = self.in_block(&ctx, desc, kind.leaves_point(), |ctx| kind.restore(ctx, &restore))?;
        // (a restore that found it back already wrote nothing: said, so Kumi doesn't tell it as an undo it made)
        let mut display = restored.display;
        if restored.unwritten && display.get("note").is_none_or(Value::is_null) {
            display["note"] = "It was back already: nothing was written.".into();
        }
        Ok(json!({"epoch": reads.epoch(), "restored": restored.restored, "display": display, "undo": undo}))
    }

    /// Runs a change as one undo block named `desc` (or inside the plan's block, when one is open) and says what REAPER's
    /// history shows after it: whether its top point is this one, and the state count then.
    ///
    /// `point` is whether a change of this kind leaves a point in REAPER's history at all. Inside a plan the answer says
    /// whether the plan's point takes the change back (`covered`): not a change that wrote nothing, nor one REAPER keeps no
    /// history of (the repeat button).
    fn in_block<T: Written>(&self, ctx: &Ctx, desc: &str, point: bool, write: impl FnOnce(&Ctx) -> Done<T>) -> Done<(T, Value)> {
        let in_plan = self.plan.borrow().is_some();
        if !in_plan && !ctx.reaper.undo_begin(ctx.proj) {
            return Err(failed("this REAPER has no undo blocks, so Kumi won't change anything in it"));
        }
        let result = write(ctx);
        if in_plan {
            return result.map(|done| {
                let covered = point && !done.unwritten();
                (done, json!({"desc": desc, "entry": null, "stateCount": null, "inPlan": true, "covered": covered}))
            });
        }
        // A change that refused, or found everything as it should be, wrote nothing: its block makes no point (flags 0),
        // and pending bare edits of the producer's stay where they were.
        let wrote = result.as_ref().is_ok_and(|done| !done.unwritten());
        ctx.reaper.undo_end(ctx.proj, desc, if wrote { -1 } else { 0 });
        let undo = self.after_block(ctx, desc, wrote);
        result.map(|done| (done, undo))
    }

    fn after_block(&self, ctx: &Ctx, desc: &str, wrote: bool) -> Value {
        if wrote {
            ctx.reaper.refresh_after_change();
        }
        let entry = ctx.reaper.undo_can_undo(ctx.proj).as_deref() == Some(desc);
        let count = ctx.reaper.state_change_count(ctx.proj);
        // The point this block made is Kumi's own: not something the producer undid.
        self.watch.borrow_mut().resync(ctx.reaper, ctx.proj);
        json!({"desc": desc, "entry": entry, "stateCount": count})
    }

    /// `transport_stop`: the DAW stops playing and recording (the emergency stop).
    pub fn transport_stop(&self, reaper: &Reaper) -> Answer {
        let info = reaper.current_project().ok_or_else(|| failed("REAPER has no project open"))?;
        let state = reaper.play_state(info.ptr);
        let was_running = state.playing || state.paused || state.recording;
        if was_running {
            reaper.run_action(TRANSPORT_STOP);
        }
        Ok(json!({"stopped": was_running, "wasRecording": state.recording}))
    }

    /// `transport_play`: the DAW plays from the edit cursor (an action: no undo point, nothing in the project changes).
    pub fn transport_play(&self, reaper: &Reaper) -> Answer {
        if let Some(busy) = reaper.busy() {
            return Err((daw_bridge::wire::code::BUSY.into(), format!("{}; nothing started", busy.reason())));
        }
        let info = reaper.current_project().ok_or_else(|| failed("REAPER has no project open"))?;
        let before = reaper.play_state(info.ptr);
        let was_playing = before.playing && !before.paused;
        if !was_playing && !reaper.run_action(TRANSPORT_PLAY) {
            return Err(failed("REAPER wouldn't start playing"));
        }
        let now = reaper.play_state(info.ptr);
        Ok(json!({"started": !was_playing && now.playing, "playing": now.playing && !now.paused}))
    }

    /// `cursor_set {positionBeats | position}`: the edit cursor moves (an action: no undo point). Playing follows it.
    pub fn cursor_set(&self, reaper: &Reaper, reads: &Reads, params: &Value) -> Answer {
        let ctx = self.context(reaper, reads, params)?;
        let time = marks::moment(&ctx, params, "positionBeats", "position")?
            .ok_or_else(|| bad("pass positionBeats (quarter notes from the start) or position (seconds)"))?;
        let playing = reaper.play_state(ctx.proj).playing;
        if !reaper.set_cursor(ctx.proj, time, playing) {
            return Err(failed("REAPER wouldn't move the cursor"));
        }
        let at = reaper.cursor(ctx.proj);
        Ok(json!({"epoch": reads.epoch(), "position": at, "positionBeats": reaper.quarter_notes_at(ctx.proj, at), "playing": playing}))
    }

    /// `undo_step_begin {label}`: one undo block around a plan's changes, which then open none of their own.
    pub fn step_begin(&self, reaper: &Reaper, reads: &Reads, params: &Value) -> Answer {
        let label = text(params, "label")?;
        if self.plan.borrow().is_some() {
            return Err(failed("a step is open already: end it before beginning another"));
        }
        let info = reaper.current_project().ok_or_else(|| failed("REAPER has no project open"))?;
        if let Some(busy) = reaper.busy() {
            return Err((daw_bridge::wire::code::BUSY.into(), format!("{}; nothing changed", busy.reason())));
        }
        if !reaper.undo_begin(info.ptr) {
            return Err(failed("this REAPER has no undo blocks"));
        }
        self.steps.set(self.steps.get() + 1);
        let id = format!("s{}", self.steps.get());
        *self.plan.borrow_mut() =
            Some(Plan { id: id.clone(), label: label.to_string(), proj: info.ptr, epoch: reads.epoch(), started: Instant::now() });
        Ok(json!({"stepId": id}))
    }

    /// `undo_step_end {stepId, label?}`: closes the plan's block as one point named by its label, or by `label` when given
    /// (what the plan did, known only once it's done).
    pub fn step_end(&self, reaper: &Reaper, reads: &Reads, params: &Value) -> Answer {
        let id = text(params, "stepId")?;
        let label = match params.get("label") {
            None | Some(Value::Null) => None,
            Some(_) => Some(text(params, "label")?.to_string()),
        };
        let mut plan = {
            let mut open = self.plan.borrow_mut();
            match open.as_ref() {
                Some(plan) if plan.id == id => open.take().expect("the plan"),
                _ => return Err(failed("that step isn't open (it ended already, or the project changed under it)")),
            }
        };
        if let Some(label) = label {
            plan.label = label;
        }
        Ok(self.close(reaper, reads, plan))
    }

    fn close(&self, reaper: &Reaper, reads: &Reads, plan: Plan) -> Value {
        // A project that went away takes its blocks with it.
        let alive = reaper.project_valid(plan.proj);
        if alive {
            reaper.undo_end(plan.proj, &plan.label, -1);
            reaper.refresh_after_change();
        }
        let entry = alive && reaper.undo_can_undo(plan.proj).as_deref() == Some(plan.label.as_str());
        let count = if alive { reaper.state_change_count(plan.proj) } else { None };
        if alive {
            self.watch.borrow_mut().resync(reaper, plan.proj);
        }
        let _ = reads;
        json!({"desc": plan.label, "entry": entry, "stateCount": count})
    }

    /// Once a tick: a plan left open too long, or across a change of project, is closed (a block left open would swallow
    /// every edit the producer makes after it); and the producer's own undo and redo of Kumi's points are noticed.
    pub fn tick(&self, reaper: &Reaper, reads: &Reads, out: &dyn Outbox) {
        let stale = self.plan.borrow().as_ref().is_some_and(|plan| plan.started.elapsed() > PLAN_MAX || plan.epoch != reads.epoch());
        if stale {
            if let Some(plan) = self.plan.borrow_mut().take() {
                self.close(reaper, reads, plan);
            }
        }
        // (r-tracks, NIT-7) The FX Kumi looked at lately are looked at again for parameters that move by themselves.
        volatile::tick(reaper, reads);
        let Some(info) = reaper.current_project() else { return };
        // A plan's block is open: its points are not the producer's undo either.
        if self.plan.borrow().is_some() {
            return;
        }
        self.watch.borrow_mut().poll(reaper, info.ptr, &reads.epoch(), out);
    }

    /// The hooks said REAPER ran an action: look at the history at once.
    pub fn nudge(&self) {
        self.watch.borrow_mut().nudge();
    }

    /// The extension is leaving: a plan's open block is closed.
    pub fn stop(&self, reaper: &Reaper, reads: &Reads) {
        if let Some(plan) = self.plan.borrow_mut().take() {
            self.close(reaper, reads, plan);
        }
    }
}

/// Transport: Stop.
const TRANSPORT_STOP: i32 = 1016;
const TRANSPORT_PLAY: i32 = 1007;

/// A name inside a title's sentence, in curly quotes as Live's titles quote theirs: a track called "Track 5" reads
/// "Deleted track “Track 5”", not "Deleted track Track 5". A title that begins with the name (a fader's, a mute's) leaves it bare.
pub(crate) fn quoted(name: &str) -> String {
    format!("\u{201c}{name}\u{201d}")
}

/// A track as the producer calls it: its name, or the number REAPER shows for one with none ("Master" for the master).
pub(crate) fn track_name(reaper: &Reaper, track: crate::reaper::Track) -> String {
    if reaper.track_value(track, "IP_TRACKNUMBER") < 0.0 {
        return "Master".into();
    }
    let name = crate::rows::clip(&reaper.track_name(track));
    if name.is_empty() {
        format!("Track {}", reaper.track_value(track, "IP_TRACKNUMBER") as i64)
    } else {
        name
    }
}

/// The track a request's `ref` names.
pub(crate) fn track_ref_of(input: &Value, what: &str) -> Done<crate::refs::TrackId> {
    let text = input.get("ref").and_then(Value::as_str).ok_or_else(|| bad(format!("{what}: ref must be a track ref")))?;
    match Ref::parse(text).map_err(bad)? {
        Ref::Track(id) => Ok(id),
        other => Err(bad(format!("{what}: ref must be a track ref, not r:{}", other.kind()))),
    }
}

#[cfg(test)]
mod tests;
// (r-tracks) M4's tests of its kinds and of M3's carried fixes.
#[cfg(test)]
mod tests_carried;
#[cfg(test)]
mod tests_lab;
