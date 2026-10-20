//! Keeping a scratch copy (M5 listening, `scratch-keep {ref: copy, name}`): a goal's best copy becomes a track of the
//! producer's, named as Kumi says and sending to the master as a track does, and leaves the scratch copies (its mark is
//! taken off, so no drop or sweep removes it). Undone by value: the track is deleted while it is as Kumi left it.

use serde_json::{json, Value};

use super::{Applied, Ctx, Done, Fault, Preview, Restore, Restored};
use crate::listening::scratch::MARK;
use crate::read::{bad, failed};
use crate::reaper::{FxOwner, Reaper, Track};
use crate::refs::{self, Ref, TrackId};

fn changed(message: impl Into<String>) -> Fault {
    ("changed".into(), message.into())
}

fn gone(message: impl Into<String>) -> Fault {
    ("gone".into(), message.into())
}

fn guid_of(input: &Value, field: &str) -> Done<String> {
    let text = input.get(field).and_then(Value::as_str).ok_or_else(|| bad(format!("{field}: the copy's track ref")))?;
    match Ref::parse(text).map_err(bad)? {
        Ref::Track(TrackId::Guid(guid)) => Ok(guid),
        Ref::Track(TrackId::Master) => Err(bad("the master is no scratch copy")),
        other => Err(bad(format!("{field} must be a track ref, not r:{}", other.kind()))),
    }
}

fn find(ctx: &Ctx, guid: &str) -> Option<Track> {
    ctx.reads.track_by_id(ctx.reaper, ctx.proj, &TrackId::Guid(guid.to_string())).ok()
}

fn name_of(input: &Value) -> Done<String> {
    match input.get("name") {
        Some(Value::String(name)) if !name.trim().is_empty() && name.chars().count() <= 256 && !name.chars().any(char::is_control) => {
            Ok(name.clone())
        }
        _ => Err(bad("name: what the kept track is called, one line of at most 256 characters")),
    }
}

/// A hash of the track's items: their GUIDs, places and lengths.
fn items_hash(reaper: &Reaper, track: Track) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    let mut feed = |bytes: &[u8]| {
        for byte in bytes {
            hash = (hash ^ *byte as u64).wrapping_mul(0x0000_0100_0000_01b3);
        }
    };
    for index in 0..reaper.track_item_count(track) {
        let Some(item) = reaper.track_item(track, index) else { continue };
        feed(reaper.item_guid(item).as_bytes());
        feed(&reaper.item_value(item, "D_POSITION").to_bits().to_le_bytes());
        feed(&reaper.item_value(item, "D_LENGTH").to_bits().to_le_bytes());
    }
    format!("{hash:016x}")
}

/// What the copy is, besides its name and where it sends: its items, its FX, its routing, its fader, its folder. A
/// producer's edit of any of it is theirs, and Kumi's undo then leaves the track be.
fn signature(reaper: &Reaper, track: Track) -> Value {
    let owner = FxOwner::Track(track);
    let fx: Vec<Value> = (0..reaper.fx_count(owner))
        .map(|index| {
            let fx = crate::reaper::Fx { owner, index };
            json!([reaper.fx_guid(fx), reaper.fx_enabled(fx), reaper.fx_offline(fx)])
        })
        .collect();
    let points: i32 = (0..reaper.track_envelope_count(track))
        .filter_map(|i| reaper.track_envelope(track, i))
        .map(|envelope| reaper.envelope_point_count(envelope))
        .sum();
    json!({
        "items": {"count": reaper.track_item_count(track), "hash": items_hash(reaper, track)},
        "fx": fx,
        "sends": reaper.send_count(track, 0),
        "receives": reaper.send_count(track, -1),
        "hwOuts": reaper.send_count(track, 1),
        "volume": reaper.track_value(track, "D_VOL"),
        "pan": reaper.track_value(track, "D_PAN"),
        "mute": reaper.track_value(track, "B_MUTE") != 0.0,
        "parent": reaper.track_parent(track).map(|parent| reaper.track_guid(parent)),
        "depth": reaper.track_value(track, "I_FOLDERDEPTH") as i64,
        "envelopePoints": points,
    })
}

fn mark_of(reaper: &Reaper, track: Track) -> String {
    reaper.track_string(track, MARK).unwrap_or_default()
}

/// What the preview fences and the apply checks again: the copy by its GUID, still a scratch copy, as it was.
fn fence(reaper: &Reaper, track: Track, guid: &str) -> Value {
    json!({
        "guid": guid,
        "mark": mark_of(reaper, track),
        "name": reaper.track_name(track),
        "mainSend": reaper.track_value(track, "B_MAINSEND") != 0.0,
        "signature": signature(reaper, track),
    })
}

pub fn preview(ctx: &Ctx, input: &Value) -> Done<Preview> {
    let guid = guid_of(input, "ref")?;
    let name = name_of(input)?;
    let track = find(ctx, &guid).ok_or_else(|| failed("that copy isn't in the project any more: read again"))?;
    let mark = mark_of(ctx.reaper, track);
    if mark.is_empty() {
        return Err(bad("that track isn't one of Kumi's scratch copies (scratch make makes them)"));
    }
    let now = ctx.reaper.track_name(track);
    Ok(Preview {
        target: json!({"ref": refs::track_ref(&TrackId::Guid(guid.clone())), "kind": "track"}),
        prior: json!({"name": now, "mainSend": ctx.reaper.track_value(track, "B_MAINSEND") != 0.0, "mark": mark}),
        fence: fence(ctx.reaper, track, &guid),
        display: json!({"title": format!("Kept {}", super::quoted(&name)), "before": now, "after": name}),
    })
}

pub fn apply(ctx: &Ctx, input: &Value, fenced: &Value) -> Done<Applied> {
    let guid = guid_of(input, "ref")?;
    let name = name_of(input)?;
    let reaper = ctx.reaper;
    let track = find(ctx, &guid).ok_or_else(|| changed("that copy went since the preview"))?;
    if fence(reaper, track, &guid) != *fenced {
        let was = fenced["name"].as_str().unwrap_or_default();
        return Err(changed(format!("{} changed since the preview", super::quoted(was))));
    }
    if !reaper.set_track_string(track, "P_NAME", &name) {
        return Err(failed("REAPER wouldn't name the track"));
    }
    reaper.set_track_value(track, "B_MAINSEND", 1.0);
    // Out of the scratch copies: no drop or sweep takes a track without Kumi's mark.
    reaper.set_track_string(track, MARK, "");
    let reference = refs::track_ref(&TrackId::Guid(guid.clone()));
    Ok(Applied {
        applied: json!({"name": reaper.track_name(track), "mainSend": true, "signature": signature(reaper, track)}),
        created: Some(json!({"ref": reference, "kind": "track"})),
        material: None,
        display: json!({"title": format!("Kept {}", super::quoted(&name))}),
        ..Default::default()
    })
}

pub fn restore(ctx: &Ctx, restore: &Restore) -> Done<Restored> {
    let created = restore.created.ok_or_else(|| bad("created: the track that was kept"))?;
    let guid = guid_of(created, "ref")?;
    let reaper = ctx.reaper;
    let Some(track) = find(ctx, &guid) else {
        return Err(gone("the track Kumi kept isn't in the project any more"));
    };
    let name = reaper.track_name(track);
    let applied = restore.applied;
    let as_left = applied["name"].as_str() == Some(name.as_str())
        && reaper.track_value(track, "B_MAINSEND") != 0.0
        && applied.get("signature") == Some(&signature(reaper, track));
    if !as_left && !restore.discard {
        return Err(changed(format!("{} changed in REAPER since Kumi kept it", super::quoted(&name))));
    }
    if !reaper.delete_track(track) {
        return Err(failed("REAPER wouldn't delete the track"));
    }
    Ok(Restored {
        restored: json!({"deletedRef": refs::track_ref(&TrackId::Guid(guid))}),
        display: json!({"title": format!("{} taken away again", super::quoted(&name))}),
        ..Default::default()
    })
}
