//! FX in a track's main chain: inserted by name, deleted, moved, bypassed and set. An FX is found by its GUID, which REAPER
//! keeps when the FX moves or comes back; input FX, take FX and FX inside containers are not changed here.
//!
//! A deleted FX is kept as its entry in the track's chunk (its state, its place in the chain, its id) and put back as an FX
//! chain file that holds just that entry: REAPER adds it with the same id and state, and neither the track's other FX nor
//! its items are touched.

use std::fs;

use serde_json::{json, Value};

use super::{Applied, Ctx, Done, Fault, Preview, Restore, Restored};
use crate::read::{bad, failed};
use crate::reaper::{Fx, FxOwner, Track, CONTAINER_FX, INPUT_FX};
use crate::refs::{self, Ref, TrackId};
use crate::rows;

/// The most parameters whose values tell an FX from what Kumi left.
const SIGNATURE_PARAMS: i32 = 4096;
/// The longest a plug-in name may be.
const NAME_LIMIT: usize = 256;

fn changed(message: impl Into<String>) -> Fault {
    ("changed".into(), message.into())
}

fn gone(message: impl Into<String>) -> Fault {
    ("gone".into(), message.into())
}

#[derive(Clone, Copy, PartialEq)]
pub enum Which {
    Add,
    Delete,
    Move,
    Bypass,
    Param,
}

/// An FX in a track's main chain.
struct Located {
    track: Track,
    index: i32,
    guid: String,
}

fn locate_guid(ctx: &Ctx, guid: &str) -> Done<Located> {
    let fx = ctx.reads.fx_by_guid(ctx.reaper, ctx.proj, guid)?;
    let FxOwner::Track(track) = fx.owner else {
        return Err(bad("FX on a take are not changed yet: only the FX in a track's chain are"));
    };
    if fx.index & (INPUT_FX | CONTAINER_FX) != 0 {
        return Err(bad("only the FX in a track's main chain are changed: not its input FX, nor FX inside a container"));
    }
    Ok(Located { track, index: fx.index, guid: guid.to_string() })
}

fn locate(ctx: &Ctx, reference: &Value) -> Done<Located> {
    let text = reference.as_str().ok_or_else(|| bad("ref must be an FX ref"))?;
    match Ref::parse(text).map_err(bad)? {
        Ref::Fx(guid) => locate_guid(ctx, &guid),
        other => Err(bad(format!("ref must be an FX ref, not r:{}", other.kind()))),
    }
}

fn fx_of(located: &Located) -> Fx {
    Fx { owner: FxOwner::Track(located.track), index: located.index }
}

fn track_label(ctx: &Ctx, track: Track) -> String {
    if rows::track_id(ctx.reaper, track) == TrackId::Master {
        return "Master".into();
    }
    let name = rows::clip(&ctx.reaper.track_name(track));
    if name.is_empty() {
        format!("Track {}", ctx.reaper.track_value(track, "IP_TRACKNUMBER") as i64)
    } else {
        name
    }
}

/// A plug-in's name as a producer says it: without the format in front and the maker behind.
fn short_name(name: &str) -> String {
    let rest = match name.split_once(": ") {
        Some((format, rest)) if format.len() <= 6 && !format.contains(' ') => rest,
        _ => name,
    };
    let rest = match rest.rfind(" (") {
        Some(at) if rest.ends_with(')') && at > 0 => &rest[..at],
        _ => rest,
    };
    rest.to_string()
}

fn fx_label(ctx: &Ctx, fx: Fx) -> String {
    let renamed = ctx.reaper.fx_config(fx, "renamed_name").unwrap_or_default();
    rows::clip(&if renamed.is_empty() { short_name(&ctx.reaper.fx_name(fx)) } else { renamed })
}

fn chain(ctx: &Ctx, track: Track) -> Vec<String> {
    let owner = FxOwner::Track(track);
    (0..ctx.reaper.fx_count(owner)).map(|index| ctx.reaper.fx_guid(Fx { owner, index })).collect()
}

fn chain_fence(chain: &[String]) -> Value {
    json!({"chain": chain})
}

fn position_of(chain: &[String], guid: &str) -> Option<usize> {
    chain.iter().position(|g| g.eq_ignore_ascii_case(guid))
}

/// What tells an FX apart from how Kumi left it: its name, whether it runs, its preset and its parameters' values.
fn signature(ctx: &Ctx, fx: Fx) -> Value {
    let reaper = ctx.reaper;
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for param in 0..reaper.fx_param_count(fx).min(SIGNATURE_PARAMS) {
        for byte in (reaper.param_normalized(fx, param) as f32).to_bits().to_le_bytes() {
            hash = (hash ^ byte as u64).wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    json!({
        "name": reaper.fx_name(fx),
        "enabled": reaper.fx_enabled(fx),
        "offline": reaper.fx_offline(fx),
        "preset": reaper.fx_preset(fx),
        "params": reaper.fx_param_count(fx),
        "hash": format!("{hash:016x}"),
    })
}

fn fx_target(ctx: &Ctx, located: &Located) -> Value {
    json!({"ref": refs::fx_ref(&located.guid), "kind": "fx", "track": refs::track_ref(&rows::track_id(ctx.reaper, located.track))})
}

fn track_target(ctx: &Ctx, track: Track) -> Value {
    json!({"ref": refs::track_ref(&rows::track_id(ctx.reaper, track)), "kind": "track"})
}

pub fn preview(ctx: &Ctx, input: &Value, which: Which) -> Done<Preview> {
    match which {
        Which::Add => add_preview(ctx, input),
        Which::Delete => delete_preview(ctx, input),
        Which::Move => move_preview(ctx, input),
        Which::Bypass => bypass_preview(ctx, input),
        Which::Param => param_preview(ctx, input),
    }
}

pub fn apply(ctx: &Ctx, input: &Value, fence: &Value, which: Which) -> Done<Applied> {
    match which {
        Which::Add => add_apply(ctx, input, fence),
        Which::Delete => delete_apply(ctx, input, fence),
        Which::Move => move_apply(ctx, input, fence),
        Which::Bypass => bypass_apply(ctx, input, fence),
        Which::Param => param_apply(ctx, input, fence),
    }
}

pub fn restore(ctx: &Ctx, restore: &Restore, which: Which) -> Done<Restored> {
    match which {
        Which::Add => add_restore(ctx, restore),
        Which::Delete => delete_restore(ctx, restore),
        Which::Move => move_restore(ctx, restore),
        Which::Bypass => bypass_restore(ctx, restore),
        Which::Param => param_restore(ctx, restore),
    }
}

// ----- insert -----

fn plugin_name(input: &Value) -> Done<String> {
    let name = input
        .get("name")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|n| !n.is_empty())
        .ok_or_else(|| bad("name must be the plug-in's name"))?;
    if name.chars().count() > NAME_LIMIT || name.chars().any(char::is_control) {
        return Err(bad(format!("name must be one line of at most {NAME_LIMIT} characters")));
    }
    if name.to_ascii_lowercase().ends_with(".rfxchain") {
        return Err(bad("name must be a plug-in's name, not an FX chain file"));
    }
    Ok(name.to_string())
}

fn add_track(ctx: &Ctx, input: &Value) -> Done<Track> {
    let id = super::track_ref_of(input, "fx-add")?;
    ctx.reads.track_by_id(ctx.reaper, ctx.proj, &id)
}

fn add_position(input: &Value, count: usize) -> Done<Option<i32>> {
    match input.get("index") {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_u64()
            .filter(|i| *i as usize <= count)
            .map(|i| Some(i as i32))
            .ok_or_else(|| bad(format!("index must be a place in the chain, from 0 to {count}"))),
    }
}

fn add_preview(ctx: &Ctx, input: &Value) -> Done<Preview> {
    let track = add_track(ctx, input)?;
    let name = plugin_name(input)?;
    let before = chain(ctx, track);
    let position = add_position(input, before.len())?;
    let place = position.map_or(before.len(), |p| p as usize);
    Ok(Preview {
        target: track_target(ctx, track),
        prior: chain_fence(&before),
        fence: chain_fence(&before),
        display: json!({"title": format!("Added {} to {}", short_name(&name), track_label(ctx, track)), "position": place}),
    })
}

fn add_apply(ctx: &Ctx, input: &Value, fence: &Value) -> Done<Applied> {
    let track = add_track(ctx, input)?;
    let name = plugin_name(input)?;
    let before = chain(ctx, track);
    if chain_fence(&before) != *fence {
        return Err(changed("the FX on that track changed since the preview"));
    }
    let position = add_position(input, before.len())?;
    let index = ctx.reaper.add_fx_by_name(track, &name, position);
    if index < 0 {
        return Err(failed(format!(
            "REAPER has no plug-in named \"{name}\": use the name the FX browser shows, such as \"ReaEQ (Cockos)\" or \"VST3: Pro-Q 3 (FabFilter)\""
        )));
    }
    let after = chain(ctx, track);
    let expected = position.map_or(before.len(), |p| p as usize);
    let guid = after.get(index as usize).cloned().unwrap_or_default();
    if after.len() != before.len() + 1 || index as usize != expected || guid.is_empty() || position_of(&before, &guid).is_some() {
        return Err(failed("the FX chain isn't what it should be after adding the plug-in"));
    }
    let fx = Fx { owner: FxOwner::Track(track), index };
    let label = fx_label(ctx, fx);
    Ok(Applied {
        applied: json!({"guid": guid, "index": index, "signature": signature(ctx, fx)}),
        created: Some(json!({"ref": refs::fx_ref(&guid), "kind": "fx"})),
        material: None,
        display: json!({"title": format!("Added {label} to {}", track_label(ctx, track)), "ref": refs::fx_ref(&guid), "index": index}),
    })
}

fn add_restore(ctx: &Ctx, restore: &Restore) -> Done<Restored> {
    let guid = restore.applied["guid"].as_str().ok_or_else(|| bad("applied.guid: the FX that was added"))?;
    let located = locate_guid(ctx, guid).map_err(|fault| if fault.0 == "failed" { gone(fault.1) } else { fault })?;
    let fx = fx_of(&located);
    if !restore.discard && signature(ctx, fx) != restore.applied["signature"] {
        return Err(changed("the FX Kumi added has been changed in REAPER since"));
    }
    let label = fx_label(ctx, fx);
    let before = chain(ctx, located.track);
    if !ctx.reaper.delete_fx(located.track, located.index) || chain(ctx, located.track).len() + 1 != before.len() {
        return Err(failed("REAPER wouldn't delete the FX"));
    }
    Ok(Restored {
        restored: json!({"deleted": guid}),
        display: json!({"title": format!("{label} taken off {}", track_label(ctx, located.track))}),
    })
}

// ----- delete -----

/// An FX's entry in a track's chunk: from its `BYPASS` line through its `WAK` line, whatever is between.
fn fx_entry(chunk: &str, guid: &str) -> Option<String> {
    let lines: Vec<&str> = chunk.lines().collect();
    let first = lines.iter().position(|line| *line == "<FXCHAIN")?;
    let mut depth = 1;
    let mut starts = Vec::new();
    let mut end = None;
    for (at, line) in lines.iter().enumerate().skip(first + 1) {
        if depth == 1 && line.starts_with("BYPASS ") {
            starts.push(at);
        }
        if line.starts_with('<') {
            depth += 1;
        } else if *line == ">" {
            depth -= 1;
            if depth == 0 {
                end = Some(at);
                break;
            }
        }
    }
    let end = end?;
    let id = format!("FXID {guid}");
    starts.iter().enumerate().find_map(|(n, &start)| {
        let stop = starts.get(n + 1).copied().unwrap_or(end);
        let entry = &lines[start..stop];
        entry.iter().any(|line| line.trim().eq_ignore_ascii_case(&id)).then(|| entry.join("\n"))
    })
}

/// The FX's parameter envelopes: they are in the track's chunk apart from the FX, and don't come back with it.
fn envelopes_of(ctx: &Ctx, located: &Located) -> usize {
    let reaper = ctx.reaper;
    (0..reaper.track_envelope_count(located.track))
        .filter_map(|i| reaper.track_envelope(located.track, i))
        .filter(|&envelope| reaper.envelope_owner(envelope).is_some_and(|(_, fx, _)| fx == located.index))
        .count()
}

fn delete_preview(ctx: &Ctx, input: &Value) -> Done<Preview> {
    let located = locate(ctx, &input["ref"])?;
    let fx = fx_of(&located);
    if ctx.reaper.fx_config(fx, "container_count").is_some() {
        return Err(bad("containers aren't changed yet"));
    }
    let now = json!({"guid": located.guid, "index": located.index, "signature": signature(ctx, fx)});
    let mut display = json!({"title": format!("Deleted {} from {}", fx_label(ctx, fx), track_label(ctx, located.track))});
    let lost = envelopes_of(ctx, &located);
    if lost > 0 {
        display["note"] = format!("Its {lost} automation envelope(s) go with it and don't come back with an undo.").into();
    }
    Ok(Preview { target: fx_target(ctx, &located), prior: now.clone(), fence: now, display })
}

fn delete_apply(ctx: &Ctx, input: &Value, fence: &Value) -> Done<Applied> {
    let located = locate(ctx, &input["ref"]).map_err(|fault| {
        if fault.0 == "failed" {
            changed(format!("{}: it went since the preview", fault.1))
        } else {
            fault
        }
    })?;
    let fx = fx_of(&located);
    let now = json!({"guid": located.guid, "index": located.index, "signature": signature(ctx, fx)});
    if now != *fence {
        return Err(changed("the FX changed since the preview"));
    }
    let reaper = ctx.reaper;
    let chunk = reaper.track_chunk(located.track).ok_or_else(|| failed("REAPER wouldn't give the track's state"))?;
    let entry =
        fx_entry(&chunk, &located.guid).ok_or_else(|| failed("the FX isn't in the track's state (a plug-in REAPER can't describe)"))?;
    let (label, track_name) = (fx_label(ctx, fx), track_label(ctx, located.track));
    let name = reaper.fx_name(fx);
    let lost = envelopes_of(ctx, &located);
    let before = chain(ctx, located.track);
    if !reaper.delete_fx(located.track, located.index) {
        return Err(failed("REAPER wouldn't delete the FX"));
    }
    let after = chain(ctx, located.track);
    if after.len() + 1 != before.len() || position_of(&after, &located.guid).is_some() {
        return Err(failed("the FX is still in the chain after it was deleted"));
    }
    Ok(Applied {
        applied: json!({"deleted": located.guid}),
        created: None,
        material: Some(json!({"entry": entry, "index": located.index, "guid": located.guid, "name": name, "lostEnvelopes": lost})),
        display: json!({"title": format!("Deleted {label} from {track_name}")}),
    })
}

/// An FX chain file in REAPER's FXChains folder, removed again when it goes out of scope.
struct ChainFile {
    path: std::path::PathBuf,
    name: String,
}

impl ChainFile {
    fn write(ctx: &Ctx, text: &str, id: &str) -> Done<ChainFile> {
        let folder = std::path::Path::new(&ctx.reaper.resource_path()).join("FXChains");
        let _ = fs::create_dir_all(&folder);
        let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis());
        let clean: String = id.chars().filter(|c| c.is_ascii_hexdigit()).take(8).collect();
        let name = format!("kumi-restore-{clean}-{stamp}.RfxChain");
        let path = folder.join(&name);
        fs::write(&path, format!("{text}\n"))
            .map_err(|e| failed(format!("couldn't write the FX into REAPER's FXChains folder to put it back: {e}")))?;
        Ok(ChainFile { path, name })
    }
}

impl Drop for ChainFile {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

fn delete_restore(ctx: &Ctx, restore: &Restore) -> Done<Restored> {
    let material = restore.material.ok_or_else(|| bad("material: what the delete removed"))?;
    let guid = material["guid"].as_str().ok_or_else(|| bad("material.guid"))?;
    let entry = material["entry"].as_str().ok_or_else(|| bad("material.entry"))?;
    if let Ok(fx) = ctx.reads.fx_by_guid(ctx.reaper, ctx.proj, guid) {
        // Back already (a retry, or the producer undid it): nothing to write.
        return Ok(Restored {
            restored: json!({"ref": refs::fx_ref(guid), "was": refs::fx_ref(guid)}),
            display: json!({"title": format!("{} is back already", fx_label(ctx, fx))}),
        });
    }
    let track = track_of(ctx, restore.target)?;
    let before = chain(ctx, track);
    let index = (material["index"].as_i64().unwrap_or(0).max(0) as usize).min(before.len());
    let file = ChainFile::write(ctx, entry, guid)?;
    let made = ctx.reaper.add_fx_by_name(track, &file.name, Some(index as i32));
    drop(file);
    let after = chain(ctx, track);
    if made < 0 || after.len() != before.len() + 1 {
        return Err(failed("REAPER wouldn't add the FX back"));
    }
    let now = after.get(made as usize).cloned().unwrap_or_default();
    let fx = Fx { owner: FxOwner::Track(track), index: made };
    Ok(Restored {
        restored: json!({"ref": refs::fx_ref(&now), "was": refs::fx_ref(guid), "index": made}),
        display: json!({"title": format!("{} put back on {}", fx_label(ctx, fx), track_label(ctx, track))}),
    })
}

/// The track a target names: an FX target says which track the FX was on.
fn track_of(ctx: &Ctx, target: &Value) -> Done<Track> {
    let text = target["track"].as_str().ok_or_else(|| bad("target.track: the track the FX was on"))?;
    match Ref::parse(text).map_err(bad)? {
        Ref::Track(id) => {
            ctx.reads.track_by_id(ctx.reaper, ctx.proj, &id).map_err(|fault| if fault.0 == "failed" { gone(fault.1) } else { fault })
        }
        other => Err(bad(format!("target.track: a track ref, not r:{}", other.kind()))),
    }
}

// ----- move -----

fn move_index(input: &Value, count: usize) -> Done<usize> {
    input
        .get("index")
        .and_then(Value::as_u64)
        .map(|i| i as usize)
        .filter(|i| *i < count)
        .ok_or_else(|| bad(format!("index must be the FX's new place in the chain, from 0 to {}", count.saturating_sub(1))))
}

fn moved(chain: &[String], guid: &str, to: usize) -> Vec<String> {
    let mut out: Vec<String> = chain.iter().filter(|g| !g.eq_ignore_ascii_case(guid)).cloned().collect();
    out.insert(to.min(out.len()), guid.to_string());
    out
}

fn move_preview(ctx: &Ctx, input: &Value) -> Done<Preview> {
    let located = locate(ctx, &input["ref"])?;
    let before = chain(ctx, located.track);
    let to = move_index(input, before.len())?;
    let fx = fx_of(&located);
    let mut display = json!({"title": format!("Moved {} to place {} on {}", fx_label(ctx, fx), to + 1, track_label(ctx, located.track)), "from": located.index, "to": to});
    if to as i32 == located.index {
        display["unchanged"] = true.into();
    }
    Ok(Preview { target: fx_target(ctx, &located), prior: chain_fence(&before), fence: chain_fence(&before), display })
}

fn move_apply(ctx: &Ctx, input: &Value, fence: &Value) -> Done<Applied> {
    let located = locate(ctx, &input["ref"]).map_err(|fault| {
        if fault.0 == "failed" {
            changed(format!("{}: it went since the preview", fault.1))
        } else {
            fault
        }
    })?;
    let before = chain(ctx, located.track);
    if chain_fence(&before) != *fence {
        return Err(changed("the FX on that track changed since the preview"));
    }
    let to = move_index(input, before.len())?;
    let expected = moved(&before, &located.guid, to);
    if to as i32 != located.index && !ctx.reaper.move_fx(located.track, located.index, to as i32) {
        return Err(failed("REAPER wouldn't move the FX"));
    }
    let after = chain(ctx, located.track);
    if after != expected {
        return Err(failed("the FX chain isn't in the order it should be after the move"));
    }
    let fx = Fx { owner: FxOwner::Track(located.track), index: to as i32 };
    Ok(Applied {
        applied: chain_fence(&after),
        created: None,
        material: None,
        display: json!({"title": format!("Moved {} to place {} on {}", fx_label(ctx, fx), to + 1, track_label(ctx, located.track))}),
    })
}

fn chain_of_value(value: &Value) -> Done<Vec<String>> {
    value["chain"]
        .as_array()
        .ok_or_else(|| bad("chain: the FX ids in order"))?
        .iter()
        .map(|g| g.as_str().map(str::to_string).ok_or_else(|| bad("chain: FX ids")))
        .collect()
}

fn move_restore(ctx: &Ctx, restore: &Restore) -> Done<Restored> {
    let guid = restore.target["ref"].as_str().and_then(|r| match Ref::parse(r) {
        Ok(Ref::Fx(guid)) => Some(guid),
        _ => None,
    });
    let guid = guid.ok_or_else(|| bad("target.ref: the FX that was moved"))?;
    let located = locate_guid(ctx, &guid).map_err(|fault| if fault.0 == "failed" { gone(fault.1) } else { fault })?;
    let (prior, applied) = (chain_of_value(restore.prior)?, chain_of_value(restore.applied)?);
    let now = chain(ctx, located.track);
    let label = fx_label(ctx, fx_of(&located));
    if now == prior {
        return Ok(Restored { restored: chain_fence(&now), display: json!({"title": format!("{label} is where it was")}) });
    }
    if !restore.discard && now != applied {
        return Err(changed("the FX on that track have been rearranged in REAPER since Kumi moved one"));
    }
    let to = position_of(&prior, &guid).unwrap_or(0).min(now.len().saturating_sub(1));
    let expected = moved(&now, &guid, to);
    if !ctx.reaper.move_fx(located.track, located.index, to as i32) || chain(ctx, located.track) != expected {
        return Err(failed("REAPER wouldn't put the FX back"));
    }
    Ok(Restored {
        restored: chain_fence(&expected),
        display: json!({"title": format!("{label} back to place {} on {}", to + 1, track_label(ctx, located.track))}),
    })
}

// ----- bypass -----

fn bypass_input(input: &Value) -> Done<bool> {
    input.get("bypass").and_then(Value::as_bool).ok_or_else(|| bad("bypass must be true or false"))
}

fn bypass_title(ctx: &Ctx, located: &Located, bypass: bool) -> String {
    format!("{} {} on {}", if bypass { "Bypassed" } else { "Enabled" }, fx_label(ctx, fx_of(located)), track_label(ctx, located.track))
}

fn bypass_preview(ctx: &Ctx, input: &Value) -> Done<Preview> {
    let located = locate(ctx, &input["ref"])?;
    let bypass = bypass_input(input)?;
    let now = json!({"enabled": ctx.reaper.fx_enabled(fx_of(&located))});
    let mut display = json!({"title": bypass_title(ctx, &located, bypass)});
    if now["enabled"] == !bypass {
        display["unchanged"] = true.into();
    }
    Ok(Preview { target: fx_target(ctx, &located), prior: now.clone(), fence: now, display })
}

fn bypass_apply(ctx: &Ctx, input: &Value, fence: &Value) -> Done<Applied> {
    let located = locate(ctx, &input["ref"]).map_err(|fault| {
        if fault.0 == "failed" {
            changed(format!("{}: it went since the preview", fault.1))
        } else {
            fault
        }
    })?;
    let bypass = bypass_input(input)?;
    let fx = fx_of(&located);
    if json!({"enabled": ctx.reaper.fx_enabled(fx)}) != *fence {
        return Err(changed("the FX was switched in REAPER since the preview"));
    }
    if ctx.reaper.fx_enabled(fx) == bypass && !ctx.reaper.set_fx_enabled(located.track, located.index, !bypass) {
        return Err(failed("REAPER wouldn't switch the FX"));
    }
    if ctx.reaper.fx_enabled(fx) == bypass {
        return Err(failed("REAPER kept the FX as it was"));
    }
    Ok(Applied {
        applied: json!({"enabled": !bypass}),
        created: None,
        material: None,
        display: json!({"title": bypass_title(ctx, &located, bypass)}),
    })
}

fn bypass_restore(ctx: &Ctx, restore: &Restore) -> Done<Restored> {
    let located = target_fx(ctx, restore.target)?;
    let fx = fx_of(&located);
    let (prior, applied) = (restore.prior["enabled"] == true, restore.applied["enabled"] == true);
    let now = ctx.reaper.fx_enabled(fx);
    if now != prior {
        if !restore.discard && now != applied {
            return Err(changed("the FX has been switched in REAPER since Kumi did"));
        }
        if !ctx.reaper.set_fx_enabled(located.track, located.index, prior) || ctx.reaper.fx_enabled(fx) != prior {
            return Err(failed("REAPER wouldn't switch the FX back"));
        }
    }
    Ok(Restored { restored: json!({"enabled": prior}), display: json!({"title": bypass_title(ctx, &located, !prior)}) })
}

/// The FX a restore's target names; one that is gone is `gone`.
fn target_fx(ctx: &Ctx, target: &Value) -> Done<Located> {
    let text = target["ref"].as_str().ok_or_else(|| bad("target.ref: the FX"))?;
    let guid = match Ref::parse(text).map_err(bad)? {
        Ref::Fx(guid) => guid,
        Ref::Param { fx, .. } => fx,
        other => return Err(bad(format!("target.ref: an FX ref, not r:{}", other.kind()))),
    };
    locate_guid(ctx, &guid).map_err(|fault| if fault.0 == "failed" { gone(fault.1) } else { fault })
}

// ----- a parameter -----

struct ParamAt {
    located: Located,
    param: i32,
}

fn param_index(ctx: &Ctx, located: &Located, which: Option<&Value>) -> Done<i32> {
    let fx = fx_of(located);
    let count = ctx.reaper.fx_param_count(fx);
    match which {
        Some(Value::Number(n)) => n
            .as_i64()
            .filter(|i| (0..count as i64).contains(i))
            .map(|i| i as i32)
            .ok_or_else(|| bad(format!("param must be a parameter number from 0 to {}", count - 1))),
        Some(Value::String(name)) => {
            let found: Vec<i32> = (0..count).filter(|&i| ctx.reaper.param_name(fx, i).eq_ignore_ascii_case(name.trim())).collect();
            match found.as_slice() {
                [one] => Ok(*one),
                [] => Err(bad(format!("that FX has no parameter named \"{name}\": read its parameters"))),
                _ => Err(bad(format!("several parameters are named \"{name}\": use the param ref"))),
            }
        }
        _ => Err(bad("pass the param ref as ref, or the FX ref as ref and param (its number or name)")),
    }
}

fn locate_param(ctx: &Ctx, input: &Value) -> Done<ParamAt> {
    let text = input.get("ref").and_then(Value::as_str).ok_or_else(|| bad("ref must be a param ref"))?;
    match Ref::parse(text).map_err(bad)? {
        Ref::Param { fx, index } => {
            let located = locate_guid(ctx, &fx)?;
            let count = ctx.reaper.fx_param_count(fx_of(&located));
            if index < 0 || index >= count {
                return Err(bad(format!("that FX has {count} parameters")));
            }
            Ok(ParamAt { located, param: index })
        }
        Ref::Fx(guid) => {
            let located = locate_guid(ctx, &guid)?;
            let param = param_index(ctx, &located, input.get("param"))?;
            Ok(ParamAt { located, param })
        }
        other => Err(bad(format!("ref must be a param ref, not r:{}", other.kind()))),
    }
}

/// What the plug-in prints for a value, with its unit when it prints one: the formatted value of a parameter leaves the unit
/// off in some plug-ins (ReaEQ's "2000.0"), where asking the plug-in to print the value keeps it.
fn shown(ctx: &Ctx, at: &ParamAt, normalized: f64) -> String {
    let printed =
        ctx.reaper.format_fx_param(at.located.track, at.located.index, at.param, normalized).filter(|text| !text.trim().is_empty());
    printed.unwrap_or_else(|| ctx.reaper.param_formatted(fx_of(&at.located), at.param))
}

fn param_state(ctx: &Ctx, at: &ParamAt) -> Value {
    let normalized = ctx.reaper.param_normalized(fx_of(&at.located), at.param);
    json!({"normalized": normalized, "display": shown(ctx, at, normalized)})
}

fn near(a: f64, b: f64) -> bool {
    (a - b).abs() <= 1e-6
}

fn same_param(a: &Value, b: &Value) -> bool {
    near(a["normalized"].as_f64().unwrap_or(f64::NAN), b["normalized"].as_f64().unwrap_or(f64::NAN))
}

/// The first number in what a plug-in prints for a value: `-3.0 dB`, `250 Hz`, `100 %`.
fn reading(text: &str) -> Option<f64> {
    let text = text.replace('\u{2212}', "-");
    let chars: Vec<char> = text.chars().collect();
    let start = (0..chars.len()).find(|&i| {
        chars[i].is_ascii_digit()
            || (matches!(chars[i], '-' | '+' | '.') && chars.get(i + 1).is_some_and(|c| c.is_ascii_digit() || *c == '.'))
    })?;
    let mut end = start + 1;
    let mut seen_dot = chars[start] == '.';
    while end < chars.len() && (chars[end].is_ascii_digit() || (chars[end] == '.' && !seen_dot)) {
        seen_dot |= chars[end] == '.';
        end += 1;
    }
    chars[start..end].iter().collect::<String>().parse().ok()
}

/// How many decimals the producer typed: the precision a reading must match to.
fn decimals(text: &str) -> usize {
    text.split_once('.').map_or(0, |(_, rest)| rest.chars().take_while(char::is_ascii_digit).count())
}

/// The normalized value a plug-in prints as `wanted`: found by asking it to print values, never by setting them.
fn find_value(ctx: &Ctx, at: &ParamAt, wanted: &str) -> Done<f64> {
    let reaper = ctx.reaper;
    let ask = |value: f64| reaper.format_fx_param(at.located.track, at.located.index, at.param, value);
    let wanted = wanted.trim();
    if wanted.is_empty() {
        return Err(bad("value must say what the plug-in should show"));
    }
    if let Some(target) =
        reading(wanted).filter(|_| wanted.chars().next().is_some_and(|c| c.is_ascii_digit() || matches!(c, '-' | '+' | '.' | '\u{2212}')))
    {
        let at_ends = ask(0.0).as_deref().and_then(reading).zip(ask(1.0).as_deref().and_then(reading));
        if let Some((low, high)) = at_ends {
            let rising = high >= low;
            let tolerance = 0.5 * 10f64.powi(-(decimals(wanted) as i32));
            if target < low.min(high) - tolerance || target > low.max(high) + tolerance {
                let (a, b) = (ask(0.0).unwrap_or_default(), ask(1.0).unwrap_or_default());
                return Err(failed(format!("that parameter runs from {a} to {b}: {wanted} is outside it")));
            }
            let (mut lo, mut hi) = (0.0f64, 1.0f64);
            for _ in 0..60 {
                let mid = (lo + hi) / 2.0;
                let Some(read) = ask(mid).as_deref().and_then(reading) else { break };
                if (read < target) == rising {
                    lo = mid;
                } else {
                    hi = mid;
                }
            }
            let best = [lo, hi, (lo + hi) / 2.0]
                .into_iter()
                .filter_map(|v| ask(v).as_deref().and_then(reading).map(|r| (v, r)))
                .min_by(|a, b| (a.1 - target).abs().total_cmp(&(b.1 - target).abs()));
            if let Some((value, read)) = best {
                if (read - target).abs() <= tolerance {
                    return Ok(value);
                }
                return Err(failed(format!(
                    "no value of that parameter reads {wanted}; the closest reads {}",
                    ask(value).unwrap_or_default()
                )));
            }
        }
    }
    // Words and steps: the middle of the first run of values that print exactly this.
    let matches: Vec<usize> =
        (0..=1000).filter(|&n| ask(n as f64 / 1000.0).is_some_and(|text| text.trim().eq_ignore_ascii_case(wanted))).collect();
    let Some(&first) = matches.first() else {
        return Err(failed(format!("no value of that parameter reads \"{wanted}\": read the parameter to see what it shows")));
    };
    let run = matches.iter().zip(first..).take_while(|(n, expected)| **n == *expected).count();
    Ok((first + run / 2) as f64 / 1000.0)
}

fn wanted_value(ctx: &Ctx, at: &ParamAt, input: &Value) -> Done<f64> {
    let normalized = input.get("normalized").filter(|v| !v.is_null());
    let value = input.get("value").filter(|v| !v.is_null());
    match (normalized, value) {
        (Some(n), None) => n.as_f64().filter(|n| (0.0..=1.0).contains(n)).ok_or_else(|| bad("normalized must be a number from 0 to 1")),
        (None, Some(Value::String(text))) => find_value(ctx, at, text),
        (None, Some(_)) => Err(bad("value must be text: what the plug-in shows, such as \"-3 dB\"")),
        _ => Err(bad("pass normalized (0 to 1) or value (what the plug-in shows), not both")),
    }
}

fn param_title(ctx: &Ctx, at: &ParamAt, display: &str) -> String {
    let fx = fx_of(&at.located);
    format!(
        "{} {} {} on {}",
        fx_label(ctx, fx),
        rows::clip(&ctx.reaper.param_name(fx, at.param)),
        display,
        track_label(ctx, at.located.track)
    )
}

fn param_target(ctx: &Ctx, at: &ParamAt) -> Value {
    let mut target = fx_target(ctx, &at.located);
    target["ref"] = refs::param_ref(&at.located.guid, at.param).into();
    target["kind"] = "param".into();
    target["fx"] = refs::fx_ref(&at.located.guid).into();
    target["param"] = at.param.into();
    target
}

fn param_preview(ctx: &Ctx, input: &Value) -> Done<Preview> {
    let at = locate_param(ctx, input)?;
    let wanted = wanted_value(ctx, &at, input)?;
    let now = param_state(ctx, &at);
    let after = shown(ctx, &at, wanted);
    let mut display = json!({"title": param_title(ctx, &at, &after), "before": now["display"], "after": after, "normalized": wanted});
    if near(wanted, now["normalized"].as_f64().unwrap_or(f64::NAN)) {
        display["unchanged"] = true.into();
    }
    Ok(Preview { target: param_target(ctx, &at), prior: now.clone(), fence: now, display })
}

fn param_apply(ctx: &Ctx, input: &Value, fence: &Value) -> Done<Applied> {
    let at = locate_param(ctx, input).map_err(|fault| {
        if fault.0 == "failed" {
            changed(format!("{}: it went since the preview", fault.1))
        } else {
            fault
        }
    })?;
    let wanted = wanted_value(ctx, &at, input)?;
    let now = param_state(ctx, &at);
    if !same_param(&now, fence) {
        return Err(changed("the parameter changed since the preview"));
    }
    if !near(wanted, now["normalized"].as_f64().unwrap_or(f64::NAN))
        && !ctx.reaper.set_fx_param_normalized(at.located.track, at.located.index, at.param, wanted)
    {
        return Err(failed("REAPER wouldn't set the parameter"));
    }
    let after = param_state(ctx, &at);
    let read = after["normalized"].as_f64().unwrap_or(f64::NAN);
    if (read - wanted).abs() > 1e-3 && near(read, now["normalized"].as_f64().unwrap_or(f64::NAN)) {
        return Err(failed("the plug-in kept the parameter as it was"));
    }
    let title = param_title(ctx, &at, after["display"].as_str().unwrap_or_default());
    Ok(Applied { applied: after, created: None, material: None, display: json!({"title": title}) })
}

fn param_restore(ctx: &Ctx, restore: &Restore) -> Done<Restored> {
    let located = target_fx(ctx, restore.target)?;
    let param = restore.target["param"].as_i64().ok_or_else(|| bad("target.param"))? as i32;
    let at = ParamAt { located, param };
    let now = param_state(ctx, &at);
    if !same_param(&now, restore.prior) {
        if !restore.discard && !same_param(&now, restore.applied) {
            return Err(changed("the parameter has been changed in REAPER since Kumi set it"));
        }
        let prior = restore.prior["normalized"].as_f64().ok_or_else(|| bad("prior.normalized"))?;
        if !ctx.reaper.set_fx_param_normalized(at.located.track, at.located.index, at.param, prior) {
            return Err(failed("REAPER wouldn't put the parameter back"));
        }
    }
    let after = param_state(ctx, &at);
    if (after["normalized"].as_f64().unwrap_or(f64::NAN) - restore.prior["normalized"].as_f64().unwrap_or(f64::NAN)).abs() > 1e-3 {
        return Err(failed("the plug-in kept the parameter as it was"));
    }
    let title = param_title(ctx, &at, after["display"].as_str().unwrap_or_default());
    Ok(Restored { restored: after, display: json!({"title": format!("{title} (put back)")}) })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plug_in_is_named_without_its_format_and_maker() {
        assert_eq!(short_name("VST3: ReaEQ (Cockos)"), "ReaEQ");
        assert_eq!(short_name("JS: utility/volume"), "utility/volume");
        assert_eq!(short_name("CLAP: Vital (Vital Audio)"), "Vital");
        assert_eq!(short_name("Pro-Q 3"), "Pro-Q 3");
        assert_eq!(short_name("AU: Kontakt 7 (Native Instruments)"), "Kontakt 7");
    }

    #[test]
    fn what_a_plug_in_prints_is_read_as_a_number() {
        assert_eq!(reading("-3.0 dB"), Some(-3.0));
        assert_eq!(reading("\u{2212}6.5 dB"), Some(-6.5));
        assert_eq!(reading("250 Hz"), Some(250.0));
        assert_eq!(reading("100 %"), Some(100.0));
        assert_eq!(reading("+12.00 st"), Some(12.0));
        assert_eq!(reading("Mid"), None);
        assert_eq!(reading("1.5 s"), Some(1.5));
        assert_eq!((decimals("-3.25 dB"), decimals("250 Hz"), decimals("1.5")), (2, 0, 1));
    }

    #[test]
    fn an_fx_is_found_in_a_chunk_by_its_id_whatever_nests_in_its_entry() {
        let chunk = "<TRACK\nNAME x\n<FXCHAIN\nSHOW 0\nBYPASS 0 0 0\n<JS a \"\"\n0.1 - -\n>\nFLOATPOS 0 0 0 0\nFXID {AAA}\nWAK 0 0\nBYPASS 1 0 0\n<CONTAINER Container 0 \"\" 0\nBYPASS 0 0 0\n<VST \"in\" x.vst 0 \"\" 1\nQUJD\n>\nFXID {CHILD}\nWAK 0 0\n>\nFXID {BBB}\nWAK 0 0\nBYPASS 0 0 0\n<VST \"c\" c.vst 0 \"\" 2\nREVG\n>\nPRESETNAME \"p\"\nFXID {CCC}\nWAK 0 0\n>\n<ITEM\nFXID {NOTME}\n>\n>";
        let a = fx_entry(chunk, "{AAA}").unwrap();
        assert!(a.starts_with("BYPASS 0 0 0\n<JS a") && a.ends_with("WAK 0 0"), "{a}");
        let b = fx_entry(chunk, "{BBB}").unwrap();
        assert!(b.starts_with("BYPASS 1 0 0\n<CONTAINER") && b.contains("{CHILD}") && b.ends_with("FXID {BBB}\nWAK 0 0"), "{b}");
        let c = fx_entry(chunk, "{CCC}").unwrap();
        assert!(c.contains("PRESETNAME \"p\"") && !c.contains("{BBB}"), "{c}");
        assert_eq!(fx_entry(chunk, "{NOTME}"), None, "only the FX chain's own entries");
        assert_eq!(fx_entry("<TRACK\n>", "{AAA}"), None);
    }
}
