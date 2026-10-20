//! A goal's render rig in a DAW whose bridge is Kumi's own (`/goal`, a match run's search, `/loop` with a reference):
//! each candidate track is searched on a scratch copy of it that never reaches the master, every generation's copies
//! rendered offline in one job and scored against the reference, so the producer's tracks stay as they are until the
//! search settles. The best is kept on a track of its own ("Kumi · Goal best") and played out loud: the winner is heard.
use super::own_listening::{DeviceRow, OwnRenderer};
use crate::{
    audio::{
        self,
        analyze::Analysis,
        matching::{closeness, Focus},
    },
    core::{contracts::*, errors::RuntimeError, evolve::Knob, refs::RefScheme},
    listening::{
        engine::{heard_options, silent_render, Listening},
        renderer::{Renderer, Window},
    },
};
use async_trait::async_trait;
use indexmap::IndexMap;
use kumi_common::{
    abort::{self, Signal, SignalExt},
    js::number::to_fixed,
};
use serde_json::{json, Value};
use std::{cell::RefCell, collections::HashSet, rc::Rc};

/// The most knobs a slot offers the search: a device's first ones, and in all.
const KNOBS_PER_DEVICE: usize = 128;
const KNOBS_PER_SLOT: usize = 512;
/// The name the best goes by.
const BEST: &str = "Kumi · Goal best";

/// One searched candidate: its track, and the scratch copy its trials are set on and heard from.
struct Slot {
    info: GoalSlotInfo,
    /// The candidate track's long ref.
    track: String,
    scratch: String,
    copy_name: String,
    /// Each knob's place (its device's in the chain, its own in the device), by its key.
    places: IndexMap<String, (u64, u64)>,
    /// The steps setting its copy left in the DAW's undo (Renoise's: one a set), told when the copy goes.
    steps: u64,
}

/// What a trial scored, kept so the same values aren't heard twice.
#[derive(Clone)]
struct Cached {
    score: f64,
    gaps: Vec<String>,
    structural: Option<StructuralMove>,
    /// The render it was heard from, to play the winner out loud.
    render: Render,
}

/// A short stretch characteristic of the reference, heard while a search screens.
struct Screen {
    window: Window,
    reference: Analysis,
}

pub(super) struct OwnGoalRig<S: RefScheme + 'static> {
    renderer: Rc<OwnRenderer<S>>,
    slots: RefCell<Vec<Slot>>,
    /// Each slot's values as last set on its copy, by knob key.
    current: RefCell<IndexMap<String, IndexMap<String, f64>>>,
    window: Window,
    reference: Analysis,
    focus: Option<Focus>,
    screen: Option<Screen>,
    heard: RefCell<IndexMap<String, Cached>>,
    /// The change that kept the last best (its track), taken back when a better one replaces it.
    best: RefCell<Option<String>>,
}

fn key(knob: &Knob) -> String {
    format!("{}|{}", knob.device, knob.name)
}

fn text(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::to_owned)
}

impl<S: RefScheme + 'static> OwnGoalRig<S> {
    /// A goal's rig over these candidates, toward the reference the request names; why not, when it can't be.
    pub(super) async fn open(
        renderer: Rc<OwnRenderer<S>>,
        listening: &Listening,
        request: &AuditionRequest,
        original: Signal,
    ) -> Result<Result<Rc<dyn GoalRig>, String>, RuntimeError> {
        if request.candidates.iter().any(|candidate| candidate.mix == Some(true) || candidate.track == MIX_CANDIDATE) {
            return Ok(Err("A goal searches a track's own devices. For the whole mix, audition it against the reference (candidates [{\"mix\": true}]) and change EQ, compression and levels between rounds.".into()));
        }
        if let Some(why) = renderer.why_not() {
            return Ok(Err(why));
        }
        if !renderer.flags().scratch {
            return Ok(Err(format!("{} can't make the scratch copies a goal searches on just now.", renderer.app())));
        }
        let Some(named) = request.reference.as_deref().filter(|named| !named.is_empty()) else {
            return Ok(Err("A goal needs a reference to reach.".into()));
        };
        let Some(tempo) = renderer.tempo().filter(|tempo| *tempo > 0.) else {
            return Ok(Err(format!("Kumi doesn't know the {}'s tempo yet; try again.", renderer.profile().words.place)));
        };
        let signal = abort::any([original, renderer.lifetime()]);
        let reference = listening.heard_reference(named, request, signal.clone()).await?;
        let window = Window { from: request.from_beat.unwrap_or(0.), beats: request.beats.unwrap_or(8.) };
        let mut rig = OwnGoalRig {
            renderer: renderer.clone(),
            slots: RefCell::new(vec![]),
            current: RefCell::new(IndexMap::new()),
            window,
            reference,
            focus: request.focus,
            screen: None,
            heard: RefCell::new(IndexMap::new()),
            best: RefCell::new(None),
        };
        renderer.tell_app(&format!(
            "Kumi tries the goal's candidates on copies only it hears, and puts its best on “{BEST}” when the search pauses or ends"
        ));
        for candidate in &request.candidates {
            let label = candidate.label.clone().unwrap_or_else(|| candidate.track.clone());
            match rig.adopt(&candidate.track, &label, &signal).await {
                Ok(Ok(_)) => {}
                Ok(Err(why)) => {
                    rig.drop_copies().await;
                    return Ok(Err(why));
                }
                Err(error) => {
                    rig.drop_copies().await;
                    return Err(error);
                }
            }
        }
        // A long part is screened on its most characteristic three seconds while the search is young.
        if window.beats * 60. / tempo >= 6. {
            let beats = (3. * tempo / 60.).round().max(2.);
            let lufs = &rig.reference.over_time.lufs;
            let slice = rig.reference.seconds / lufs.len().max(1) as f64;
            let across = (beats * 60. / tempo / slice).round().max(1.) as usize;
            let mut best = (0, f64::NEG_INFINITY);
            if across <= lufs.len() {
                for at in 0..=lufs.len() - across {
                    let part: Vec<f64> = lufs[at..at + across].iter().map(|v| v.unwrap_or(-70.)).collect();
                    let mean = part.iter().sum::<f64>() / part.len() as f64;
                    let change: f64 = part.windows(2).map(|v| (v[1] - v[0]).abs()).sum();
                    if mean + change > best.1 {
                        best = (at, mean + change);
                    }
                }
            }
            let offset = (best.0 as f64 * slice * tempo / 60.).round().min(window.beats - beats).max(0.);
            let mut snippet = request.clone();
            snippet.reference_from = Some(request.reference_from.unwrap_or(0.) + offset * 60. / tempo);
            snippet.reference_seconds = Some(beats * 60. / tempo);
            match listening.heard_reference(named, &snippet, signal).await {
                Ok(reference) => rig.screen = Some(Screen { window: Window { from: window.from + offset, beats }, reference }),
                Err(error) => {
                    rig.drop_copies().await;
                    return Err(error);
                }
            }
        }
        Ok(Ok(Rc::new(rig)))
    }

    /// A candidate joins: a safety limiter last in its chain (kept quietly, as it's part of the candidate), its knobs
    /// read, and a scratch copy of it made to search on.
    async fn adopt(&self, named: &str, label: &str, signal: &Signal) -> Result<Result<GoalSlotInfo, String>, RuntimeError> {
        let r = &self.renderer;
        let (track, name) = match r.resolve(named, signal).await? {
            Ok(found) => found,
            Err(why) => return Ok(Err(why)),
        };
        if self.slots.borrow().iter().any(|slot| slot.info.name == name) {
            return Ok(Err(format!("“{name}” is already in the search.")));
        }
        let mut devices = match r.devices(&track, signal).await? {
            Ok(devices) => devices,
            Err(why) => return Ok(Err(why)),
        };
        let (add, limiter) = r.table.safety_limiter;
        if !devices.last().is_some_and(|device| device.name.contains(limiter)) {
            let mut input = json!({"ref": track});
            input[r.table.add_field] = add.into();
            let added = r.plan.history.quietly(None, r.change(r.table.add_tool, input, signal.clone())).await;
            if let Err(why) = added {
                return Ok(Err(format!("Kumi couldn't put a safety {limiter} on “{name}”: {why}")));
            }
            devices = match r.devices(&track, signal).await? {
                Ok(devices) => devices,
                Err(why) => return Ok(Err(why)),
            };
        }
        let (knobs, places) = match self.knobs_of(&devices, signal).await? {
            Ok(read) => read,
            Err(why) => return Ok(Err(why)),
        };
        let args = json!({"action": "make", "ref": track, "count": 1, "label": "Kumi: goal search", "range": {"fromBeat": self.window.from, "beats": self.window.beats}});
        let made = match r.ask("scratch", args, signal.clone()).await? {
            Ok(made) => made,
            Err(refused) => return Ok(Err(refused.why)),
        };
        let (Some(scratch), Some(copy)) = (text(&made, "scratchId"), made.pointer("/copies/0").cloned()) else {
            return Ok(Err(format!("{} made no copy of “{name}”", r.app())));
        };
        let (Some(copy_ref), Some(copy_name)) = (text(&copy, "ref"), text(&copy, "name")) else {
            return Ok(Err(format!("{} made no copy of “{name}”", r.app())));
        };
        r.keep_copy(copy_name.clone(), &copy_ref);
        r.tell_scratch_steps(made.pointer("/undo/steps").and_then(Value::as_u64).unwrap_or(0));
        for note in super::own_listening::notes_of(&made) {
            r.tell_app(&note);
        }
        let chain = devices.iter().map(|device| device.name.clone()).collect::<Vec<_>>().join(" → ");
        let info =
            GoalSlotInfo { name: name.clone(), label: label.into(), chain: if chain.is_empty() { "empty".into() } else { chain }, knobs };
        self.current.borrow_mut().insert(name.clone(), info.knobs.iter().map(|knob| (key(knob), knob.value)).collect());
        self.slots.borrow_mut().push(Slot { info: info.clone(), track, scratch, copy_name, places, steps: 0 });
        Ok(Ok(info))
    }

    /// A chain's knobs as the search moves them, each with its place (device, parameter) to set it by on a copy.
    async fn knobs_of(
        &self,
        devices: &[DeviceRow],
        signal: &Signal,
    ) -> Result<Result<(Vec<Knob>, IndexMap<String, (u64, u64)>), String>, RuntimeError> {
        let r = &self.renderer;
        let mut knobs = vec![];
        let mut places = IndexMap::new();
        for (at, device) in devices.iter().enumerate() {
            // (a grid of two points: a stepped parameter answers its steps' texts)
            let rows = match r.rows(json!({"kind": "param", "parent": device.reference, "grid": 2}), 16, signal).await? {
                Ok(rows) => rows,
                Err(why) => return Ok(Err(why)),
            };
            for row in rows.iter().take(KNOBS_PER_DEVICE) {
                let (Some(reference), Some(name)) = (text(row, "ref"), text(row, "name")) else { continue };
                let number = |key: &str| row.get(key).and_then(Value::as_f64);
                let (min, max) = if r.table.normalized { (0., 1.) } else { (number("min").unwrap_or(0.), number("max").unwrap_or(1.)) };
                let index =
                    row.get("index").and_then(Value::as_u64).or_else(|| reference.rsplit([':', '.']).next()?.parse().ok()).unwrap_or(0);
                let knob = Knob {
                    r#ref: reference,
                    device: format!("{at}:{}", device.name),
                    name,
                    min,
                    max,
                    step: super::own_listening::steps_of(row).map(|steps| (max - min) / (steps - 1) as f64),
                    value: number("value").unwrap_or(min),
                };
                places.insert(key(&knob), (device.index, index));
                knobs.push(knob);
                if knobs.len() >= KNOBS_PER_SLOT {
                    return Ok(Ok((knobs, places)));
                }
            }
        }
        Ok(Ok((knobs, places)))
    }

    /// Sets a slot's trial values on its copy; the knobs the DAW refused, by key.
    async fn set_copy(&self, slot: &str, moved: &[(Knob, f64)], signal: &Signal) -> Result<Result<(), String>, RuntimeError> {
        let r = &self.renderer;
        let (scratch, copy, places) = {
            let slots = self.slots.borrow();
            let Some(found) = slots.iter().find(|found| found.info.name == slot) else {
                return Ok(Err(format!("“{slot}” isn't in the search")));
            };
            let Some(copy) = r.copy_ref(&found.copy_name) else { return Ok(Err(format!("“{slot}”'s copy is gone"))) };
            (found.scratch.clone(), copy, found.places.clone())
        };
        let values: Vec<Value> = moved
            .iter()
            .filter_map(|(knob, value)| {
                let (device, param) = places.get(&key(knob))?;
                let mut set = json!({"ref": copy, "device": device, "param": param});
                set[r.table.scratch_field] = json!(value);
                Some(set)
            })
            .collect();
        if values.is_empty() {
            return Ok(Ok(()));
        }
        for chunk in values.chunks(256) {
            match r.ask("scratch", json!({"action": "set", "scratchId": scratch, "values": chunk}), signal.clone()).await? {
                Ok(done) => {
                    let steps = done.pointer("/undo/steps").and_then(Value::as_u64).unwrap_or(0);
                    if let Some(found) = self.slots.borrow_mut().iter_mut().find(|found| found.info.name == slot) {
                        found.steps += steps;
                    }
                }
                Err(refused) => return Ok(Err(refused.why)),
            }
        }
        Ok(Ok(()))
    }

    /// The scratch copies go, whatever happened.
    async fn drop_copies(&self) {
        let r = &self.renderer;
        let slots: Vec<(String, String, u64)> =
            self.slots.borrow().iter().map(|slot| (slot.scratch.clone(), slot.copy_name.clone(), slot.steps)).collect();
        for (scratch, name, steps) in slots {
            r.drop_make(&scratch, steps).await;
            r.copies.borrow_mut().shift_remove(&name);
        }
    }

    /// The slot's knobs set on the candidate track itself, as one change (several in HISTORY's one row).
    async fn set_track(&self, slot: &str, knobs: &[Knob], values: &[f64], signal: Signal) -> Result<(), String> {
        let pairs: Vec<(&str, f64)> = knobs.iter().zip(values).map(|(knob, value)| (knob.r#ref.as_str(), *value)).collect();
        // (by device: one change for each, as a device's knobs are set)
        let mut by_device: IndexMap<String, Vec<(&str, f64)>> = IndexMap::new();
        for (knob, (reference, value)) in knobs.iter().zip(pairs) {
            by_device.entry(knob.device.clone()).or_default().push((reference, value));
        }
        let devices = {
            let slots = self.slots.borrow();
            let found = slots.iter().find(|found| found.info.name == slot).ok_or_else(|| format!("“{slot}” isn't in the search"))?;
            found.track.clone()
        };
        let chain = match self.renderer.devices(&devices, &signal).await {
            Ok(Ok(chain)) => chain,
            Ok(Err(why)) => return Err(why),
            Err(error) => return Err(error.to_string()),
        };
        for (device, values) in by_device {
            let at: usize = device.split(':').next().and_then(|at| at.parse().ok()).unwrap_or(usize::MAX);
            let Some(reference) = chain.get(at).map(|row| row.reference.clone()) else { continue };
            self.renderer.set_knobs(&reference, &values, signal.clone()).await.map_err(|error| error.to_string())?;
        }
        Ok(())
    }
}

#[async_trait(?Send)]
impl<S: RefScheme + 'static> GoalRig for OwnGoalRig<S> {
    fn slots(&self) -> Vec<GoalSlotInfo> {
        self.slots.borrow().iter().map(|slot| slot.info.clone()).collect()
    }
    fn screens(&self) -> bool {
        self.screen.is_some()
    }
    async fn add(&self, candidate: &AuditionCandidate, given: Signal) -> Result<Result<GoalSlotInfo, String>, RuntimeError> {
        let signal = abort::any([given, self.renderer.lifetime()]);
        let label = candidate.label.clone().unwrap_or_else(|| candidate.track.clone());
        match self.adopt(&candidate.track, &label, &signal).await {
            Err(error) if !error.is_aborted() => Ok(Err(error.to_string())),
            other => other,
        }
    }
    async fn generation(
        &self,
        trials: &[GenerationTrial],
        given: Signal,
        options: Option<GenerationOptions>,
    ) -> Result<Generation, RuntimeError> {
        let r = &self.renderer;
        let signal = abort::any([given, r.lifetime()]);
        let screened = options.and_then(|options| options.screen) == Some(true) && self.screen.is_some();
        let cache_key = |trial: &GenerationTrial| {
            format!(
                "{}|{}|{}",
                trial.slot,
                if screened { "screen" } else { "full" },
                trial.values.iter().map(|value| to_fixed(*value, 4)).collect::<Vec<_>>().join(",")
            )
        };
        if r.flag().is_on() {
            return Err(RuntimeError::Observation("Another render is running.".into()));
        }
        let _rendering = r.flag().begin();
        let mut generation = Generation { screened, ..Default::default() };
        // Each trial's values on its slot's copy (only what moved).
        for trial in trials {
            let moved: Vec<(Knob, f64)> = {
                let current = self.current.borrow();
                let last = current.get(&trial.slot);
                trial
                    .knobs
                    .iter()
                    .enumerate()
                    .map(|(at, knob)| (knob.clone(), trial.values.get(at).copied().unwrap_or(f64::NAN)))
                    .filter(|(knob, value)| {
                        value.is_finite() && last.and_then(|last| last.get(&key(knob))).is_none_or(|last| (last - value).abs() > 1e-6)
                    })
                    .collect()
            };
            if moved.is_empty() {
                continue;
            }
            match self.set_copy(&trial.slot, &moved, &signal).await? {
                Ok(()) => {
                    let mut current = self.current.borrow_mut();
                    let last = current.entry(trial.slot.clone()).or_default();
                    for (knob, value) in &moved {
                        last.insert(key(knob), *value);
                    }
                }
                // (the DAW refused them: the search leaves them be)
                Err(_) => {
                    generation.frozen.insert(trial.slot.clone(), moved.iter().map(|(knob, _)| key(knob)).collect::<HashSet<_>>());
                }
            }
        }
        // Rendered unless every trial was heard before at these values.
        let fresh: Vec<&GenerationTrial> =
            trials.iter().filter(|trial| trial.fresh == Some(true) || !self.heard.borrow().contains_key(&cache_key(trial))).collect();
        let window = if screened { self.screen.as_ref().map(|screen| screen.window).unwrap_or(self.window) } else { self.window };
        let mut files: IndexMap<String, Render> = IndexMap::new();
        if !fresh.is_empty() {
            let sources: Vec<Value> = {
                let slots = self.slots.borrow();
                fresh
                    .iter()
                    .filter_map(|trial| slots.iter().find(|slot| slot.info.name == trial.slot))
                    .enumerate()
                                    .map(|(at, slot)| json!({"id": format!("g{at}"), "what": "track", "ref": r.copy_ref(&slot.copy_name), "stage": "pre-fader"}))
                    .collect()
            };
            let ids: Vec<String> = fresh.iter().map(|trial| trial.slot.clone()).collect();
            match r.render_sources(sources, window, &signal).await? {
                Ok((rendered, _)) => {
                    for (at, slot) in ids.iter().enumerate() {
                        if let Some((render, _)) = rendered.get(&format!("g{at}")) {
                            files.insert(slot.clone(), render.clone());
                        }
                    }
                }
                Err(why) => return Err(RuntimeError::Observation(why)),
            }
        }
        let tempo = r.tempo().unwrap_or(120.);
        let against = if screened { &self.screen.as_ref().unwrap().reference } else { &self.reference };
        for trial in trials {
            let cache_key = cache_key(trial);
            let known = if trial.fresh != Some(true) { self.heard.borrow().get(&cache_key).cloned() } else { None };
            if let Some(known) = known {
                generation.cached += 1;
                generation.scores.insert(trial.slot.clone(), known.score);
                generation.gaps.insert(trial.slot.clone(), known.gaps);
                if let Some(structural) = known.structural {
                    generation.structural.insert(trial.slot.clone(), structural);
                }
                continue;
            }
            let Some(rendered) = files.get(&trial.slot) else {
                generation.silent.push(trial.slot.clone());
                continue;
            };
            let seconds = rendered.seconds.unwrap_or(window.beats * 60. / tempo);
            let heard = audio::hear(&rendered.file, heard_options(rendered.start, seconds, self.focus, signal.clone()))
                .await
                .map_err(|error| RuntimeError::plain(error.to_string()))?;
            if silent_render(&heard) {
                generation.silent.push(trial.slot.clone());
                continue;
            }
            let close = closeness(&heard, against, self.focus);
            let structural = close.structural.map(|found| StructuralMove { gap: found.gap, r#move: found.r#move });
            generation.scores.insert(trial.slot.clone(), close.score);
            generation.gaps.insert(trial.slot.clone(), close.gaps.clone());
            if let Some(structural) = &structural {
                generation.structural.insert(trial.slot.clone(), structural.clone());
            }
            let mut cache = self.heard.borrow_mut();
            cache.insert(cache_key, Cached { score: close.score, gaps: close.gaps, structural, render: rendered.clone() });
            if cache.len() > 2000 {
                cache.shift_remove_index(0);
            }
        }
        Ok(generation)
    }
    async fn keep_best(&self, slot: &str, knobs: &[Knob], values: &[f64], given: Signal) -> Result<String, RuntimeError> {
        let r = &self.renderer;
        let signal = abort::any([given, r.lifetime()]);
        let result: Result<Result<String, String>, RuntimeError> = async {
            if !r.link.daw.changes.kinds.iter().any(|kind| kind.tool == "scratch_keep") {
                return Ok(Err(format!("this {} bridge can't keep a copy as a track yet: update Kumi's bridge", r.app())));
            }
            // The last best goes (Kumi's undo of keeping it), then a fresh copy of the slot at these values is kept.
            if let Some(previous) = self.best.borrow_mut().take() {
                let _ = r.undo(&previous).await;
            }
            let track = {
                let slots = self.slots.borrow();
                slots.iter().find(|found| found.info.name == slot).map(|found| found.track.clone())
            };
            let Some(track) = track else { return Ok(Err(format!("“{slot}” isn't in the search"))) };
            let args = json!({"action": "make", "ref": track, "count": 1, "label": "Kumi: goal best"});
            let made = match r.ask("scratch", args, signal.clone()).await? {
                Ok(made) => made,
                Err(refused) => return Ok(Err(refused.why)),
            };
            let (Some(scratch), Some(copy)) =
                (text(&made, "scratchId"), made.pointer("/copies/0/ref").and_then(Value::as_str).map(str::to_owned))
            else {
                return Ok(Err(format!("{} made no copy of “{slot}”", r.app())));
            };
            let places =
                self.slots.borrow().iter().find(|found| found.info.name == slot).map(|found| found.places.clone()).unwrap_or_default();
            let set: Vec<Value> = knobs
                .iter()
                .zip(values)
                .filter_map(|(knob, value)| {
                    let (device, param) = places.get(&key(knob))?;
                    let mut entry = json!({"ref": copy, "device": device, "param": param});
                    entry[r.table.scratch_field] = json!(value);
                    Some(entry)
                })
                .collect();
            let mut steps = made.pointer("/undo/steps").and_then(Value::as_u64).unwrap_or(0);
            if !set.is_empty() {
                match r.ask("scratch", json!({"action": "set", "scratchId": scratch, "values": set}), signal.clone()).await? {
                    Ok(done) => steps += done.pointer("/undo/steps").and_then(Value::as_u64).unwrap_or(0),
                    Err(refused) => {
                        r.drop_make(&scratch, steps).await;
                        return Ok(Err(refused.why));
                    }
                }
            }
            let kept = r.change("scratch_keep", json!({"ref": copy, "name": BEST}), signal.clone()).await;
            // The make goes either way: a copy kept as a track stays (the bridge leaves it), and the make no longer counts
            // as standing.
            r.drop_make(&scratch, steps).await;
            match kept {
                Ok((id, _)) => *self.best.borrow_mut() = Some(id),
                Err(why) => return Ok(Err(why)),
            }
            // The winner is heard: what it scored from, out loud.
            let heard = self
                .heard
                .borrow()
                .iter()
                .rev()
                .find(|(cached, _)| cached.starts_with(&format!("{slot}|")))
                .map(|(_, found)| found.render.clone());
            if let Some(render) = heard {
                let (played, said) = r.play(&render).await;
                if !played {
                    let why = said.map(|why| format!(" ({})", why.trim_end_matches('.'))).unwrap_or_default();
                    r.tell_app(&format!("Kumi couldn't play {BEST} out loud{why}: play it to hear it"));
                }
            }
            Ok(Ok(BEST.to_owned()))
        }
        .await;
        match result {
            Ok(Ok(name)) => Ok(name),
            Ok(Err(why)) => Ok(why),
            Err(error) => {
                signal.check()?;
                Ok(error.to_string())
            }
        }
    }
    async fn settle(&self, slot: &str, knobs: &[Knob], values: &[f64], given: Signal) -> Result<Option<String>, RuntimeError> {
        let signal = abort::any([given, self.renderer.lifetime()]);
        match self.set_track(slot, knobs, values, signal.clone()).await {
            Ok(()) => Ok(None),
            Err(why) => {
                signal.check()?;
                Ok(Some(why))
            }
        }
    }
    async fn tidy(&self, top: &[String], given: Signal) -> Result<Vec<String>, RuntimeError> {
        let r = &self.renderer;
        let signal = abort::any([given, r.lifetime()]);
        let mut said = vec![];
        let slots: Vec<(String, String)> = self.slots.borrow().iter().map(|slot| (slot.info.name.clone(), slot.track.clone())).collect();
        for (name, track) in slots.into_iter().rev() {
            if top.contains(&name) {
                let _ = r.change("track_mute", json!({"ref": track, "mute": true}), signal.clone()).await;
                continue;
            }
            // A candidate the model made goes again (its add undone); one of the producer's stays, muted.
            let made = r.plan.history.entries.borrow().values().rev().find_map(|entry| {
                let entry = entry.borrow();
                (entry.tool.as_deref() == Some("track_add")
                    && entry.record.state == ChangeState::Applied
                    && entry.record.title.contains(&format!("“{name}”")))
                .then(|| entry.record.id.clone())
            });
            let undone = match made {
                Some(id) => r.plan.history.undo(&id, signal.clone(), true).await.ok(),
                None => None,
            };
            if undone.is_none_or(|undone| undone.is_error) {
                let _ = r.change("track_mute", json!({"ref": track, "mute": true}), signal.clone()).await;
                said.push(format!("“{name}” stays, muted."));
            }
        }
        Ok(said)
    }
    async fn close(&self) -> Result<Vec<String>, RuntimeError> {
        self.drop_copies().await;
        Ok(vec![])
    }
}
