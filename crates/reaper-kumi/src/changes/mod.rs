//! Kumi's changes to a REAPER project (M3): a preview that writes nothing, an apply fenced by what the preview saw, and a
//! restore fenced by what the apply wrote. Every write is one undo block named by the host (`desc`, with its nonce), so
//! the producer's own Ctrl-Z takes a change back whole. Kumi's undo never calls REAPER's: REAPER's undo also reverts the
//! producer's bare edits since the last undo point, so a change is put back by values (see `restore`).
//!
//! Each kind knows three things: what it would do (`preview`), how to do it (`apply`, which re-reads what the preview fenced
//! and refuses `changed` before it writes anything), and how to put it back (`restore`, which refuses `changed` if the
//! fields it touched no longer hold what the apply wrote). A kind never leaves half a change: it checks first.

mod marks;
mod sends;
mod structure;
mod tracks;
mod undo;

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

pub struct Applied {
    pub applied: Value,
    pub created: Option<Value>,
    pub material: Option<Value>,
    pub display: Value,
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

pub struct Restored {
    pub restored: Value,
    pub display: Value,
}

/// A kind of change.
#[derive(Clone, Copy)]
enum Kind {
    TrackProp(&'static tracks::Prop),
    TrackDelete,
    TrackAdd,
    TrackMove,
    SendProp(&'static sends::Prop),
    MarkProp(&'static marks::Prop),
    MarkerAdd,
    RegionAdd,
    MarkerDelete,
    SendAdd,
    SendRemove,
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
        "marker-add" => Some(Kind::MarkerAdd),
        "region-add" => Some(Kind::RegionAdd),
        "marker-delete" => Some(Kind::MarkerDelete),
        "send-add" => Some(Kind::SendAdd),
        "send-remove" => Some(Kind::SendRemove),
        "track-delete" => Some(Kind::TrackDelete),
        "track-add" => Some(Kind::TrackAdd),
        "track-move" => Some(Kind::TrackMove),
        _ => None,
    }
}

impl Kind {
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
        let epoch = text(params, "epoch").map_err(|_| bad("epoch: the epoch the refs were read in"))?;
        if epoch != reads.epoch() {
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
        let applied = self.in_block(&ctx, desc, |ctx| kind.apply(ctx, input, fence))?;
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
        let ctx = self.context(reaper, reads, params)?;
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
        let (restored, undo) = self.in_block(&ctx, desc, |ctx| kind.restore(ctx, &restore))?;
        Ok(json!({"epoch": reads.epoch(), "restored": restored.restored, "display": restored.display, "undo": undo}))
    }

    /// Runs a change as one undo block named `desc` (or inside the plan's block, when one is open) and says what REAPER's
    /// history shows after it: whether its top point is this one, and the state count then.
    fn in_block<T>(&self, ctx: &Ctx, desc: &str, write: impl FnOnce(&Ctx) -> Done<T>) -> Done<(T, Value)> {
        let in_plan = self.plan.borrow().is_some();
        if !in_plan && !ctx.reaper.undo_begin(ctx.proj) {
            return Err(failed("this REAPER has no undo blocks, so Kumi won't change anything in it"));
        }
        let result = write(ctx);
        if in_plan {
            return result.map(|done| (done, json!({"desc": desc, "entry": null, "stateCount": null, "inPlan": true})));
        }
        // A change that refused wrote nothing: its block makes no point (flags 0), and pending bare edits of the
        // producer's stay where they were.
        ctx.reaper.undo_end(ctx.proj, desc, if result.is_ok() { -1 } else { 0 });
        let undo = self.after_block(ctx, desc, result.is_ok());
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

    /// `undo_step_end {stepId}`: closes the plan's block as one point named by its label.
    pub fn step_end(&self, reaper: &Reaper, reads: &Reads, params: &Value) -> Answer {
        let id = text(params, "stepId")?;
        let plan = {
            let mut open = self.plan.borrow_mut();
            match open.as_ref() {
                Some(plan) if plan.id == id => open.take().expect("the plan"),
                _ => return Err(failed("that step isn't open (it ended already, or the project changed under it)")),
            }
        };
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
