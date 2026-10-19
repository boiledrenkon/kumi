//! An FX's parameters that move by themselves (a meter the plug-in shows as a parameter): what they hold says nothing of
//! what the producer did, so the signature that tells an FX from how Kumi left it leaves them out. Found without holding
//! REAPER's main thread (NIT-7): by their names (read once per plug-in, not once per change), by their moving between two
//! reads in one call (nothing the producer does runs in between: REAPER's interface is on the same thread), and by the
//! extension's ticks, which look again at an FX Kumi just looked at for about a second after: a parameter that moved in two
//! of those intervals moves by itself (the producer's one tweak moves it once, and is never taken for a meter).
//!
//! A parameter found to move only after a signature was taken is left out when that signature is compared again: the look
//! the signature was taken from is kept, so both sides are hashed again without it.

use std::cell::RefCell;
use std::collections::{BTreeSet, HashMap, VecDeque};
use std::time::{Duration, Instant};

use serde_json::Value;

use super::Ctx;
use crate::read::Reads;
use crate::reaper::{Fx, Reaper};

/// The most parameters whose values make an FX's signature.
pub(super) const SIGNATURE_PARAMS: i32 = 4096;
/// How often a tick looks again at an FX Kumi looked at, and how many times.
const RELOOK_EVERY: Duration = Duration::from_millis(100);
const RELOOKS: u8 = 12;
/// The most FX looked at again at once, plug-ins whose names were read, looks kept, and FX with parameters found moving.
const WATCHED: usize = 8;
const NAMES_KEPT: usize = 256;
const LOOKS_KEPT: usize = 64;
const LEARNED_KEPT: usize = 256;

/// One read of an FX's parameter values, as a signature hashes them.
struct Look {
    guid: String,
    hash: String,
    values: Vec<f32>,
}

/// An FX the ticks look at again: its values as last read, when, and how many more times.
struct Watched {
    guid: String,
    values: Vec<f32>,
    at: Instant,
    left: u8,
}

#[derive(Default)]
struct State {
    /// The parameters a plug-in's names mark as meters, by its name and parameter count.
    names: HashMap<(String, i32), Vec<i32>>,
    names_order: VecDeque<(String, i32)>,
    /// The looks signatures were taken from, newest last.
    looks: VecDeque<Look>,
    /// The parameters found to move by themselves, by FX GUID.
    learned: HashMap<String, BTreeSet<i32>>,
    learned_order: VecDeque<String>,
    /// How many of the ticks' intervals each parameter of a watched FX moved in, by FX GUID.
    moves: HashMap<String, HashMap<i32, u8>>,
    watched: Vec<Watched>,
}

thread_local! {
    static STATE: RefCell<State> = RefCell::new(State::default());
}

/// The values a signature hashes: each parameter's normalized value as a 32-bit float.
pub(super) fn values(reaper: &Reaper, fx: Fx) -> Vec<f32> {
    (0..reaper.fx_param_count(fx).min(SIGNATURE_PARAMS)).map(|param| reaper.param_normalized(fx, param) as f32).collect()
}

/// The hash of those values, leaving out the parameters in `skip`.
pub(super) fn hash(values: &[f32], skip: &[i32]) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for (_, value) in values.iter().enumerate().filter(|(param, _)| !skip.contains(&(*param as i32))) {
        for byte in value.to_bits().to_le_bytes() {
            hash = (hash ^ byte as u64).wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    format!("{hash:016x}")
}

fn moved<'a>(before: &'a [f32], now: &'a [f32]) -> impl Iterator<Item = i32> + 'a {
    before.iter().zip(now).enumerate().filter(|(_, (a, b))| a.to_bits() != b.to_bits()).map(|(param, _)| param as i32)
}

/// The parameters a plug-in's names mark as meters: read once per plug-in and parameter count.
fn named(reaper: &Reaper, fx: Fx, count: i32) -> Vec<i32> {
    let key = (reaper.fx_name(fx), count);
    if let Some(found) = STATE.with(|state| state.borrow().names.get(&key).cloned()) {
        return found;
    }
    let found: Vec<i32> = (0..count)
        .filter(|&param| {
            let name = reaper.param_name(fx, param).to_lowercase();
            ["meter", "gain reduction", "readout"].iter().any(|word| name.contains(word))
        })
        .collect();
    STATE.with(|state| {
        let mut state = state.borrow_mut();
        if state.names.insert(key.clone(), found.clone()).is_none() {
            state.names_order.push_back(key);
            while state.names_order.len() > NAMES_KEPT {
                if let Some(oldest) = state.names_order.pop_front() {
                    state.names.remove(&oldest);
                }
            }
        }
    });
    found
}

fn learn(state: &mut State, guid: &str, params: impl IntoIterator<Item = i32>) {
    let params: Vec<i32> = params.into_iter().collect();
    if params.is_empty() {
        return;
    }
    if !state.learned.contains_key(guid) {
        state.learned_order.push_back(guid.to_string());
        while state.learned_order.len() > LEARNED_KEPT {
            if let Some(oldest) = state.learned_order.pop_front() {
                state.learned.remove(&oldest);
            }
        }
    }
    state.learned.entry(guid.to_string()).or_default().extend(params);
}

/// The parameters of this FX that move by themselves, as far as is known now: by name, by moving between two reads in this
/// call, or found by the ticks. `now` is a read of its values in this call.
fn moving_with(ctx: &Ctx, fx: Fx, guid: &str, now: &[f32]) -> Vec<i32> {
    let names = named(ctx.reaper, fx, now.len() as i32);
    // (read again after the names: the time between the two reads is what a meter has to move in)
    let again = values(ctx.reaper, fx);
    STATE.with(|state| {
        let mut state = state.borrow_mut();
        learn(&mut state, guid, moved(now, &again).collect::<Vec<_>>());
        let mut all: BTreeSet<i32> = names.into_iter().collect();
        all.extend(state.learned.get(guid).into_iter().flatten().copied());
        all.into_iter().collect()
    })
}

/// The parameters to leave out of a new signature of this FX (see the module's words).
pub(super) fn moving(ctx: &Ctx, fx: Fx) -> Vec<i32> {
    let guid = ctx.reaper.fx_guid(fx);
    let now = values(ctx.reaper, fx);
    moving_with(ctx, fx, &guid, &now)
}

/// A signature was taken of this FX: the look it hashed is kept, and the ticks look at the FX again for a while.
pub(super) fn remember(ctx: &Ctx, fx: Fx, signature: &Value) {
    let guid = ctx.reaper.fx_guid(fx);
    let values = values(ctx.reaper, fx);
    let hash = signature["hash"].as_str().unwrap_or_default().to_string();
    STATE.with(|state| {
        let mut state = state.borrow_mut();
        state.looks.push_back(Look { guid: guid.clone(), hash, values: values.clone() });
        while state.looks.len() > LOOKS_KEPT {
            state.looks.pop_front();
        }
        state.watched.retain(|watched| watched.guid != guid);
        state.watched.push(Watched { guid, values, at: Instant::now(), left: RELOOKS });
        if state.watched.len() > WATCHED {
            state.watched.remove(0);
        }
    });
}

/// Whether the FX is as the recorded signature says but for parameters found to move by themselves since it was taken: the
/// look it was taken from and the FX now are hashed again without them.
pub(super) fn same_but_for_moving(ctx: &Ctx, fx: Fx, recorded: &Value) -> bool {
    let guid = ctx.reaper.fx_guid(fx);
    let Some(recorded_hash) = recorded["hash"].as_str() else { return false };
    let then = STATE.with(|state| {
        state.borrow().looks.iter().rev().find(|look| look.guid == guid && look.hash == recorded_hash).map(|look| look.values.clone())
    });
    let Some(then) = then else { return false };
    let now = values(ctx.reaper, fx);
    if now.len() != then.len() {
        return false;
    }
    let mut skip: BTreeSet<i32> =
        recorded["volatile"].as_array().into_iter().flatten().filter_map(Value::as_i64).map(|n| n as i32).collect();
    skip.extend(moving_with(ctx, fx, &guid, &now));
    let skip: Vec<i32> = skip.into_iter().collect();
    hash(&then, &skip) == hash(&now, &skip)
}

/// Once a tick: the FX Kumi looked at lately are looked at again, a parameter that moved meanwhile is one that moves by
/// itself. At most one look per FX every `RELOOK_EVERY`, and `RELOOKS` in all.
pub fn tick(reaper: &Reaper, reads: &Reads) {
    let due: Vec<(String, Vec<f32>)> = STATE.with(|state| {
        let state = state.borrow();
        state.watched.iter().filter(|w| w.at.elapsed() >= RELOOK_EVERY).map(|w| (w.guid.clone(), w.values.clone())).collect()
    });
    if due.is_empty() {
        return;
    }
    let Some(info) = reaper.current_project() else { return };
    for (guid, before) in due {
        let now = reads.fx_by_guid(reaper, info.ptr, &guid).ok().map(|fx| values(reaper, fx));
        STATE.with(|state| {
            let mut state = state.borrow_mut();
            match now {
                Some(now) if now.len() == before.len() => {
                    // A parameter that moved in two intervals moves by itself; one that moved once may be the producer's.
                    let counts = state.moves.entry(guid.clone()).or_default();
                    let mut twice = Vec::new();
                    for param in moved(&before, &now) {
                        let count = counts.entry(param).or_insert(0);
                        *count = count.saturating_add(1);
                        if *count >= 2 {
                            twice.push(param);
                        }
                    }
                    learn(&mut state, &guid, twice);
                    if let Some(watched) = state.watched.iter_mut().find(|w| w.guid == guid) {
                        watched.values = now;
                        watched.at = Instant::now();
                        watched.left = watched.left.saturating_sub(1);
                    }
                }
                // Gone, or another plug-in at that id: nothing more to learn of it.
                _ => {
                    if let Some(watched) = state.watched.iter_mut().find(|w| w.guid == guid) {
                        watched.left = 0;
                    }
                }
            }
            state.watched.retain(|w| w.left > 0);
            let watched: Vec<String> = state.watched.iter().map(|w| w.guid.clone()).collect();
            state.moves.retain(|guid, _| watched.contains(guid));
        });
    }
}
