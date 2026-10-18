//! Sends between tracks: adding and removing one, and changing its volume, pan, mute and mode. REAPER gives a send no
//! identity of its own, so a send is found by its ref, which holds only while the same number of sends go to the same
//! track (see `refs`); a restore finds it the same way and refuses `changed` if that number moved.

use serde_json::{json, Value};

use super::{Applied, Ctx, Done, Fault, Preview, Restore, Restored};
use crate::read::{bad, failed};
use crate::reaper::{Reaper, Track};
use crate::refs::{self, Ref, SendEnd, TrackId};
use crate::rows::{self, SendKey};

fn changed(message: impl Into<String>) -> Fault {
    ("changed".into(), message.into())
}

/// A send of the track a ref names (the sending side; REAPER's category 0).
struct Found {
    track: Track,
    index: i32,
    from: String,
    to: String,
}

fn name_of(reaper: &Reaper, track: Track) -> String {
    let name = rows::clip(&reaper.track_name(track));
    if name.is_empty() {
        format!("Track {}", reaper.track_value(track, "IP_TRACKNUMBER") as i64)
    } else {
        name
    }
}

fn track_by_id(ctx: &Ctx, id: &TrackId) -> Option<Track> {
    ctx.reads.track_by_id(ctx.reaper, ctx.proj, id).ok()
}

/// Finds the send a ref names: its track, and where it is in that track's sends now.
fn find(ctx: &Ctx, reference: &str) -> Done<Found> {
    let Ref::Send { track, category, other, repeat, of } = Ref::parse(reference).map_err(bad)? else {
        return Err(bad("ref must be a send ref (r:send:…)"));
    };
    if category != 0 {
        return Err(bad("pass the send from the sending track's side (r:send:{sender}:0:{receiver}), not a receive or a hardware output"));
    }
    let SendEnd::Track(other) = other else { return Err(bad("ref must be a send to a track")) };
    let reaper = ctx.reaper;
    let owner = ctx.reads.track_by_id(reaper, ctx.proj, &track)?;
    let key = SendKey { other: SendEnd::Track(other.clone()), repeat, of };
    let keys = rows::send_keys(reaper, owner, 0);
    let Some(index) = keys.iter().position(|k| *k == key) else {
        let now = keys.iter().filter(|k| k.other == key.other).count();
        return Err(failed(match now {
            0 => "that send isn't there (deleted, or it went elsewhere)".to_string(),
            n => format!("the track has {n} such send(s) now, and this ref was made when there were {of}: read them again"),
        }));
    };
    let receiver = reaper.send_peer(owner, 0, index as i32).ok_or_else(|| failed("that send has no receiving track"))?;
    Ok(Found { track: owner, index: index as i32, from: name_of(reaper, owner), to: name_of(reaper, receiver) })
}

fn find_for_apply(ctx: &Ctx, reference: &str) -> Done<Found> {
    find(ctx, reference)
        .map_err(|fault| if fault.0 == "failed" { changed(format!("{}: it changed since the preview", fault.1)) } else { fault })
}

fn find_for_restore(ctx: &Ctx, reference: &str) -> Done<Found> {
    find(ctx, reference).map_err(|fault| if fault.0 == "failed" { ("gone".to_string(), fault.1) } else { fault })
}

fn target(reference: &str) -> Value {
    json!({"ref": reference, "kind": "send"})
}

// ----- a send's attributes -----

pub struct Prop {
    pub kind: &'static str,
    read: fn(&Reaper, Track, i32) -> Value,
    wanted: fn(&Value) -> Done<Value>,
    write: fn(&Reaper, Track, i32, &Value) -> bool,
    same: fn(&Value, &Value) -> bool,
    title: fn(&str, &Value) -> String,
    noun: &'static str,
}

fn number(value: &Value, key: &str) -> f64 {
    value.get(key).and_then(Value::as_f64).unwrap_or(f64::NAN)
}

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() <= 1e-9 * a.abs().max(b.abs()).max(1.0)
}

const MODES: [(&str, i64); 3] = [("post-fader", 0), ("pre-fader", 3), ("pre-fx", 1)];

fn mode_state(mode: i64) -> Value {
    json!({"mode": mode, "modeName": rows::send_mode_name(mode)})
}

pub static PROPS: [Prop; 4] = [
    Prop {
        kind: "send-volume",
        read: |reaper, track, index| {
            let gain = reaper.send_value(track, 0, index, "D_VOL");
            json!({"volume": gain, "volumeDb": rows::db(gain)})
        },
        wanted: |input| {
            let db = input
                .get("volumeDb")
                .and_then(Value::as_f64)
                .filter(|db| db.is_finite())
                .ok_or_else(|| bad("volumeDb must be a number"))?;
            if !(-150.0..=24.0).contains(&db) {
                return Err(bad("volumeDb must be from -150 (silent) to 24"));
            }
            let gain = if db <= -150.0 { 0.0 } else { 10f64.powf(db / 20.0) };
            Ok(json!({"volume": gain, "volumeDb": rows::db(gain)}))
        },
        write: |reaper, track, index, state| reaper.set_send_value(track, 0, index, "D_VOL", number(state, "volume")),
        same: |a, b| close(number(a, "volume"), number(b, "volume")),
        title: |route, to| format!("{route} {} dB", super::tracks::db_text(number(to, "volumeDb"))),
        noun: "volume",
    },
    Prop {
        kind: "send-pan",
        read: |reaper, track, index| json!({"pan": reaper.send_value(track, 0, index, "D_PAN")}),
        wanted: |input| {
            let pan = input
                .get("pan")
                .and_then(Value::as_f64)
                .filter(|pan| (-1.0..=1.0).contains(pan))
                .ok_or_else(|| bad("pan must be a number from -1 (left) to 1 (right)"))?;
            Ok(json!({"pan": pan}))
        },
        write: |reaper, track, index, state| reaper.set_send_value(track, 0, index, "D_PAN", number(state, "pan")),
        same: |a, b| close(number(a, "pan"), number(b, "pan")),
        title: |route, to| format!("{route} pan {}", super::tracks::pan_text(number(to, "pan"))),
        noun: "pan",
    },
    Prop {
        kind: "send-mute",
        read: |reaper, track, index| json!({"mute": reaper.send_value(track, 0, index, "B_MUTE") != 0.0}),
        wanted: |input| {
            let mute = input.get("mute").and_then(Value::as_bool).ok_or_else(|| bad("mute must be true or false"))?;
            Ok(json!({"mute": mute}))
        },
        write: |reaper, track, index, state| {
            reaper.set_send_value(track, 0, index, "B_MUTE", if state["mute"] == true { 1.0 } else { 0.0 })
        },
        same: |a, b| a["mute"] == b["mute"],
        title: |route, to| format!("{route} send {}", if to["mute"] == true { "muted" } else { "unmuted" }),
        noun: "mute",
    },
    Prop {
        kind: "send-mode",
        read: |reaper, track, index| mode_state(reaper.send_value(track, 0, index, "I_SENDMODE") as i64),
        wanted: |input| {
            let word = input.get("mode").and_then(Value::as_str).ok_or_else(|| bad("mode must be post-fader, pre-fader or pre-fx"))?;
            let (_, mode) =
                MODES.iter().find(|(name, _)| *name == word).ok_or_else(|| bad("mode must be post-fader, pre-fader or pre-fx"))?;
            Ok(mode_state(*mode))
        },
        write: |reaper, track, index, state| reaper.set_send_value(track, 0, index, "I_SENDMODE", number(state, "mode")),
        // 2 is the deprecated spelling of 3.
        same: |a, b| {
            (a["mode"].as_i64().unwrap_or(0) == 2 && b["mode"] == 3)
                || (b["mode"].as_i64().unwrap_or(0) == 2 && a["mode"] == 3)
                || a["mode"] == b["mode"]
        },
        title: |route, to| format!("{route} send {}", to["modeName"].as_str().unwrap_or("")),
        noun: "mode",
    },
];

pub fn prop_preview(ctx: &Ctx, prop: &Prop, input: &Value) -> Done<Preview> {
    let reference = input["ref"].as_str().unwrap_or_default();
    let found = find(ctx, reference)?;
    let now = (prop.read)(ctx.reaper, found.track, found.index);
    let wanted = (prop.wanted)(input)?;
    let route = format!("{} → {}", found.from, found.to);
    let mut display = json!({"title": (prop.title)(&route, &wanted), "before": now, "after": wanted});
    if (prop.same)(&now, &wanted) {
        display["unchanged"] = true.into();
    }
    Ok(Preview { target: target(reference), prior: now.clone(), fence: now, display })
}

pub fn prop_apply(ctx: &Ctx, prop: &Prop, input: &Value, fence: &Value) -> Done<Applied> {
    let found = find_for_apply(ctx, input["ref"].as_str().unwrap_or_default())?;
    let route = format!("{} → {}", found.from, found.to);
    let now = (prop.read)(ctx.reaper, found.track, found.index);
    if !(prop.same)(&now, fence) {
        return Err(changed(format!("{route}'s {} changed since the preview", prop.noun)));
    }
    let wanted = (prop.wanted)(input)?;
    if !(prop.same)(&now, &wanted) && !(prop.write)(ctx.reaper, found.track, found.index, &wanted) {
        return Err(failed(format!("REAPER wouldn't take the send's {}", prop.noun)));
    }
    let after = (prop.read)(ctx.reaper, found.track, found.index);
    if !(prop.same)(&after, &wanted) {
        return Err(failed(format!("REAPER kept the send's {} as it was", prop.noun)));
    }
    Ok(Applied {
        applied: after.clone(),
        created: None,
        material: None,
        display: json!({"title": (prop.title)(&route, &after), "before": now, "after": after}),
    })
}

pub fn prop_restore(ctx: &Ctx, prop: &Prop, restore: &Restore) -> Done<Restored> {
    let found = find_for_restore(ctx, restore.target["ref"].as_str().unwrap_or_default())?;
    let route = format!("{} → {}", found.from, found.to);
    let now = (prop.read)(ctx.reaper, found.track, found.index);
    if !(prop.same)(&now, restore.prior) {
        if !restore.discard && !(prop.same)(&now, restore.applied) {
            return Err(changed(format!("{route}'s {} changed in REAPER since Kumi set it", prop.noun)));
        }
        if !(prop.write)(ctx.reaper, found.track, found.index, restore.prior) {
            return Err(failed(format!("REAPER wouldn't put back the send's {}", prop.noun)));
        }
    }
    let after = (prop.read)(ctx.reaper, found.track, found.index);
    if !(prop.same)(&after, restore.prior) {
        return Err(failed(format!("REAPER kept the send's {} as it was", prop.noun)));
    }
    Ok(Restored {
        restored: after.clone(),
        display: json!({"title": format!("{} (put back)", (prop.title)(&route, &after)), "before": now, "after": after}),
    })
}

// ----- adding a send -----

/// All of a send's settings, as a state to compare and to make again.
fn send_state(reaper: &Reaper, track: Track, index: i32) -> Value {
    let value = |parm: &str| reaper.send_value(track, 0, index, parm);
    json!({
        "volume": value("D_VOL"), "pan": value("D_PAN"), "mute": value("B_MUTE"), "mode": value("I_SENDMODE"),
        "srcChan": value("I_SRCCHAN"), "dstChan": value("I_DSTCHAN"), "midiFlags": value("I_MIDIFLAGS"),
        "phase": value("B_PHASE"), "mono": value("B_MONO"), "autoMode": value("I_AUTOMODE"),
    })
}

const STATE_KEYS: [(&str, &str); 10] = [
    ("D_VOL", "volume"),
    ("D_PAN", "pan"),
    ("B_MUTE", "mute"),
    ("I_SENDMODE", "mode"),
    ("I_SRCCHAN", "srcChan"),
    ("I_DSTCHAN", "dstChan"),
    ("I_MIDIFLAGS", "midiFlags"),
    ("B_PHASE", "phase"),
    ("B_MONO", "mono"),
    ("I_AUTOMODE", "autoMode"),
];

fn write_state(reaper: &Reaper, track: Track, index: i32, state: &Value) {
    for (parm, key) in STATE_KEYS {
        if let Some(value) = state[key].as_f64() {
            reaper.set_send_value(track, 0, index, parm, value);
        }
    }
}

fn same_state(a: &Value, b: &Value) -> bool {
    STATE_KEYS.iter().all(|(_, key)| close(number(a, key), number(b, key)))
}

struct Add {
    from: Track,
    from_id: TrackId,
    to: Track,
    to_id: TrackId,
    changes: Vec<(&'static str, f64)>,
}

fn add_plan(ctx: &Ctx, input: &Value) -> Done<Add> {
    let id = |what: &str| -> Done<TrackId> {
        match Ref::parse(input[what].as_str().unwrap_or_default()).map_err(bad)? {
            Ref::Track(id) => Ok(id),
            other => Err(bad(format!("{what}: a track ref, not r:{}", other.kind()))),
        }
    };
    let (from_id, to_id) = (id("ref")?, id("toRef")?);
    if from_id == TrackId::Master || to_id == TrackId::Master {
        return Err(bad("the master track has no sends of this kind: a send goes from one track to another"));
    }
    if from_id == to_id {
        return Err(bad("a track can't send to itself"));
    }
    let from = ctx.reads.track_by_id(ctx.reaper, ctx.proj, &from_id)?;
    let to = ctx.reads.track_by_id(ctx.reaper, ctx.proj, &to_id)?;
    let mut changes = Vec::new();
    if input.get("volumeDb").is_some_and(|v| !v.is_null()) {
        changes.push(("D_VOL", number(&(PROPS[0].wanted)(input)?, "volume")));
    }
    if input.get("pan").is_some_and(|v| !v.is_null()) {
        changes.push(("D_PAN", number(&(PROPS[1].wanted)(input)?, "pan")));
    }
    if input.get("mode").is_some_and(|v| !v.is_null()) {
        changes.push(("I_SENDMODE", number(&(PROPS[3].wanted)(input)?, "mode")));
    }
    Ok(Add { from, from_id, to, to_id, changes })
}

fn add_fence(ctx: &Ctx, plan: &Add) -> Value {
    json!({"sends": ctx.reaper.send_count(plan.from, 0), "toGuid": ctx.reaper.track_guid(plan.to)})
}

pub fn add_preview(ctx: &Ctx, input: &Value) -> Done<Preview> {
    let plan = add_plan(ctx, input)?;
    let route = format!("{} → {}", name_of(ctx.reaper, plan.from), name_of(ctx.reaper, plan.to));
    let existing = rows::send_keys(ctx.reaper, plan.from, 0).iter().filter(|key| key.other == SendEnd::Track(plan.to_id.clone())).count();
    let mut display = json!({"title": format!("Added send {route}")});
    if existing > 0 {
        display["note"] = format!("There are {existing} send(s) between them already.").into();
    }
    Ok(Preview {
        target: json!({"ref": refs::track_ref(&plan.from_id), "kind": "track"}),
        prior: json!({"sends": ctx.reaper.send_count(plan.from, 0)}),
        fence: add_fence(ctx, &plan),
        display,
    })
}

pub fn add_apply(ctx: &Ctx, input: &Value, fence: &Value) -> Done<Applied> {
    let plan = add_plan(ctx, input).map_err(|fault| {
        if fault.0 == "failed" {
            changed(format!("{}: it went since the preview", fault.1))
        } else {
            fault
        }
    })?;
    if &add_fence(ctx, &plan) != fence {
        return Err(changed("the sends of that track changed since the preview"));
    }
    let reaper = ctx.reaper;
    let index = reaper.create_send(plan.from, Some(plan.to)).ok_or_else(|| failed("REAPER wouldn't make the send"))?;
    for (parm, value) in &plan.changes {
        reaper.set_send_value(plan.from, 0, index, parm, *value);
    }
    let keys = rows::send_keys(reaper, plan.from, 0);
    let key = keys.get(index as usize).ok_or_else(|| failed("the new send isn't in the track's sends"))?;
    let reference = refs::send_ref(&plan.from_id, 0, &key.other, key.repeat, key.of);
    let route = format!("{} → {}", name_of(reaper, plan.from), name_of(reaper, plan.to));
    Ok(Applied {
        applied: send_state(reaper, plan.from, index),
        created: Some(target(&reference)),
        material: None,
        display: json!({"title": format!("Added send {route}")}),
    })
}

pub fn add_restore(ctx: &Ctx, restore: &Restore) -> Done<Restored> {
    let created = restore.created.ok_or_else(|| bad("created: the send that was added"))?;
    let reference = created["ref"].as_str().ok_or_else(|| bad("created: the send's ref"))?;
    let found = find_for_restore(ctx, reference)?;
    let now = send_state(ctx.reaper, found.track, found.index);
    if !restore.discard && !same_state(&now, restore.applied) {
        return Err(changed(format!("the send {} → {} has been changed in REAPER since Kumi added it", found.from, found.to)));
    }
    if !ctx.reaper.remove_send(found.track, 0, found.index) {
        return Err(failed("REAPER wouldn't remove the send"));
    }
    Ok(Restored {
        restored: json!({"removed": reference}),
        display: json!({"title": format!("Send {} → {} taken away again", found.from, found.to)}),
    })
}

// ----- removing a send -----

pub fn remove_preview(ctx: &Ctx, input: &Value) -> Done<Preview> {
    let reference = input["ref"].as_str().unwrap_or_default();
    let found = find(ctx, reference)?;
    let now = send_state(ctx.reaper, found.track, found.index);
    let mut display = json!({"title": format!("Removed send {} → {}", found.from, found.to), "before": now});
    let envelopes = active_envelopes(ctx.reaper, found.track, found.index);
    if envelopes > 0 {
        display["note"] = format!("Its {envelopes} envelope(s) can't be put back by undo.").into();
    }
    Ok(Preview { target: target(reference), prior: now.clone(), fence: now, display })
}

fn active_envelopes(reaper: &Reaper, track: Track, index: i32) -> usize {
    ["<VOLENV", "<PANENV", "<MUTEENV"]
        .into_iter()
        .filter_map(|chunk| reaper.send_envelope(track, index, chunk))
        .filter(|envelope| reaper.envelope_flag(*envelope, "ACTIVE"))
        .count()
}

pub fn remove_apply(ctx: &Ctx, input: &Value, fence: &Value) -> Done<Applied> {
    let reference = input["ref"].as_str().unwrap_or_default();
    let found = find_for_apply(ctx, reference)?;
    let reaper = ctx.reaper;
    let now = send_state(reaper, found.track, found.index);
    if !same_state(&now, fence) {
        return Err(changed(format!("the send {} → {} changed since the preview", found.from, found.to)));
    }
    let receiver = reaper
        .send_peer(found.track, 0, found.index)
        .map(|t| reaper.track_guid(t))
        .ok_or_else(|| failed("that send has no receiving track"))?;
    let lost = active_envelopes(reaper, found.track, found.index);
    if !reaper.remove_send(found.track, 0, found.index) {
        return Err(failed("REAPER wouldn't remove the send"));
    }
    let material = json!({"state": now, "receiver": receiver, "lostEnvelopes": lost});
    Ok(Applied {
        applied: json!({"removed": true}),
        created: None,
        material: Some(material),
        display: json!({"title": format!("Removed send {} → {}", found.from, found.to)}),
    })
}

pub fn remove_restore(ctx: &Ctx, restore: &Restore) -> Done<Restored> {
    let material = restore.material.ok_or_else(|| bad("material: what the removal took away"))?;
    let reference = restore.target["ref"].as_str().unwrap_or_default();
    let Ref::Send { track, .. } = Ref::parse(reference).map_err(bad)? else { return Err(bad("target: a send ref")) };
    let reaper = ctx.reaper;
    let from = ctx.reads.track_by_id(reaper, ctx.proj, &track).map_err(|(_, message)| ("gone".to_string(), message))?;
    let receiver = TrackId::Guid(material["receiver"].as_str().unwrap_or_default().to_string());
    let to =
        track_by_id(ctx, &receiver).ok_or_else(|| ("gone".to_string(), "the receiving track isn't in the project any more".to_string()))?;
    // The send is back already (a retry, or the producer made it): nothing is made twice.
    if let Some(index) = rows::send_keys(reaper, from, 0).iter().position(|key| key.other == SendEnd::Track(receiver.clone())) {
        let existing = send_state(reaper, from, index as i32);
        if same_state(&existing, &material["state"]) {
            return Ok(Restored { restored: existing, display: json!({"title": "The send is back already"}) });
        }
    }
    let index = reaper.create_send(from, Some(to)).ok_or_else(|| failed("REAPER wouldn't make the send again"))?;
    write_state(reaper, from, index, &material["state"]);
    let keys = rows::send_keys(reaper, from, 0);
    let key = keys.get(index as usize).ok_or_else(|| failed("the send made again isn't in the track's sends"))?;
    let mut display = json!({"title": format!("Send {} → {} put back", name_of(reaper, from), name_of(reaper, to))});
    if let Some(lost) = material["lostEnvelopes"].as_u64().filter(|lost| *lost > 0) {
        display["note"] = format!("Its {lost} envelope(s) were not put back.").into();
    }
    Ok(Restored {
        restored: json!({"ref": refs::send_ref(&track, 0, &key.other, key.repeat, key.of), "state": send_state(reaper, from, index)}),
        display,
    })
}
