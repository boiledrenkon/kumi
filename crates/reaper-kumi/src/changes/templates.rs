//! A track template (`.RTrackTemplate`) inserted as tracks (r-tracks, M4): by its name in REAPER's TrackTemplates folder
//! or its path, at a place. REAPER inserts a template's tracks at the end of the track list (whatever is selected),
//! selected, with new GUIDs and their folders (7.82 lab); Kumi then moves them where they were asked to go and puts the
//! producer's selection back. The tracks made are the change's (`created`, by GUID); its undo deletes them, only while
//! each is as the insert left it and no track of the producer's has gone into their folders.
//!
//! A template file that isn't there would make REAPER show an error dialog: it is checked first.

use std::path::{Component, Path, PathBuf};

use serde_json::{json, Value};

use super::{Applied, Ctx, Done, Fault, Preview, Restore, Restored};
use crate::read::{bad, failed};
use crate::reaper::{FxOwner, Reaper, Track};
use crate::refs::{self, TrackId};

fn changed(message: impl Into<String>) -> Fault {
    ("changed".into(), message.into())
}

/// The most of a template Kumi reads to say what it holds.
const MOST_TEMPLATE: u64 = 64 << 20;

fn template_path(ctx: &Ctx, input: &Value) -> Done<PathBuf> {
    let path = match (input.get("name").and_then(Value::as_str), input.get("path").and_then(Value::as_str)) {
        (Some(name), None) => {
            let name = name.trim().trim_end_matches(".RTrackTemplate");
            let relative = Path::new(name);
            if name.is_empty() || relative.is_absolute() || relative.components().any(|c| !matches!(c, Component::Normal(_))) {
                return Err(bad(
                    "name must be a template's name in REAPER's TrackTemplates folder (folders inside allowed, \"Drums/Kit\")",
                ));
            }
            Path::new(&ctx.reaper.resource_path()).join("TrackTemplates").join(format!("{name}.RTrackTemplate"))
        }
        (None, Some(path)) => {
            let path = PathBuf::from(path);
            if !path.is_absolute() || path.components().any(|c| matches!(c, Component::ParentDir)) {
                return Err(bad("path must be a whole path, with no \"..\" in it"));
            }
            path
        }
        _ => return Err(bad("give name (a template in REAPER's TrackTemplates folder) or path, not both")),
    };
    if path.extension().is_none_or(|e| !e.eq_ignore_ascii_case("RTrackTemplate")) {
        return Err(bad("a track template is a .RTrackTemplate file"));
    }
    let meta = std::fs::metadata(&path).map_err(|_| failed(format!("there is no track template at {}", path.display())))?;
    if !meta.is_file() || meta.len() > MOST_TEMPLATE {
        return Err(failed(format!("{} isn't a track template Kumi can insert", path.display())));
    }
    Ok(path)
}

/// What a template holds: its tracks' names, and the hash of its text (what the preview saw).
struct Template {
    names: Vec<String>,
    hash: String,
}

fn read_template(path: &Path) -> Done<Template> {
    let text = std::fs::read_to_string(path).map_err(|e| failed(format!("Kumi couldn't read {}: {e}", path.display())))?;
    let mut names = Vec::new();
    let mut depth = 0i32;
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('<') {
            depth += 1;
            if depth == 1 && trimmed.starts_with("<TRACK") {
                names.push(String::new());
            }
            continue;
        }
        if trimmed == ">" {
            depth -= 1;
            continue;
        }
        if depth == 1 {
            if let (Some(rest), Some(last)) = (trimmed.strip_prefix("NAME "), names.last_mut()) {
                *last = rest.trim().trim_matches('"').to_string();
            }
        }
    }
    if names.is_empty() {
        return Err(failed(format!("{} holds no tracks", path.display())));
    }
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in text.bytes() {
        hash = (hash ^ byte as u64).wrapping_mul(0x0000_0100_0000_01b3);
    }
    Ok(Template { names, hash: format!("{hash:016x}") })
}

/// Where the tracks go (0-based), from `index` counted from 1 as the track rows count; the end by default.
fn place_of(ctx: &Ctx, input: &Value) -> Done<i64> {
    let count = ctx.reaper.track_count(ctx.proj) as i64;
    match input.get("index") {
        None | Some(Value::Null) => Ok(count),
        Some(v) => v.as_i64().filter(|i| (1..=count + 1).contains(i)).map(|i| i - 1).ok_or_else(|| {
            bad(format!("index must be the first track's place, from 1 to {} (counted from 1, as a read shows)", count + 1))
        }),
    }
}

/// What stands where the tracks go: the track count, and the track at that place now.
fn fence_of(ctx: &Ctx, at: i64, template: &Template) -> Value {
    let reaper = ctx.reaper;
    json!({
        "tracks": reaper.track_count(ctx.proj),
        "at": reaper.track_at(ctx.proj, at as i32).map(|t| reaper.track_guid(t)),
        "template": template.hash,
    })
}

fn label(path: &Path) -> String {
    path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()
}

fn title(path: &Path, count: usize, at: i64) -> String {
    format!(
        "Inserted track template {} ({count} track{}) at position {}",
        super::quoted(&label(path)),
        if count == 1 { "" } else { "s" },
        at + 1
    )
}

pub fn preview(ctx: &Ctx, input: &Value) -> Done<Preview> {
    let path = template_path(ctx, input)?;
    let template = read_template(&path)?;
    let at = place_of(ctx, input)?;
    let names: Vec<String> = template.names.iter().map(|n| if n.is_empty() { "(unnamed)".to_string() } else { n.clone() }).collect();
    Ok(Preview {
        target: json!({"ref": null, "kind": "track", "index": at, "template": path.to_string_lossy()}),
        prior: json!({"tracks": ctx.reaper.track_count(ctx.proj)}),
        fence: fence_of(ctx, at, &template),
        display: json!({"title": title(&path, template.names.len(), at), "note": format!("Its tracks: {}.", names.join(", "))}),
    })
}

/// What tells a track made from the template from how it was left: its place in the block, name, items, FX, routes,
/// envelopes with points, folder depth.
fn signature(reaper: &Reaper, track: Track) -> Value {
    let envelopes = (0..reaper.track_envelope_count(track))
        .filter_map(|i| reaper.track_envelope(track, i))
        .map(|e| reaper.envelope_point_count(e))
        .sum::<i32>();
    let fx = FxOwner::Track(track);
    json!({
        "name": reaper.track_name(track),
        "items": reaper.track_item_count(track),
        "fx": (0..reaper.fx_count(fx)).map(|index| reaper.fx_guid(crate::reaper::Fx { owner: fx, index })).collect::<Vec<_>>(),
        "sends": reaper.send_count(track, 0),
        "receives": reaper.send_count(track, -1),
        "outputs": reaper.send_count(track, 1),
        "points": envelopes,
        "folderDepth": reaper.track_value(track, "I_FOLDERDEPTH") as i64,
    })
}

fn selection(ctx: &Ctx) -> (Vec<String>, bool) {
    let reaper = ctx.reaper;
    let tracks = (0..reaper.track_count(ctx.proj))
        .filter_map(|at| reaper.track_at(ctx.proj, at))
        .filter(|t| reaper.track_value(*t, "I_SELECTED") != 0.0)
        .map(|t| reaper.track_guid(t))
        .collect();
    (tracks, reaper.master_track(ctx.proj).is_some_and(|m| reaper.track_value(m, "I_SELECTED") != 0.0))
}

fn put_selection(ctx: &Ctx, (tracks, master): &(Vec<String>, bool)) {
    let reaper = ctx.reaper;
    for at in 0..reaper.track_count(ctx.proj) {
        if let Some(t) = reaper.track_at(ctx.proj, at) {
            reaper.set_track_selected(t, tracks.contains(&reaper.track_guid(t)));
        }
    }
    if let Some(m) = reaper.master_track(ctx.proj) {
        reaper.set_track_selected(m, *master);
    }
}

fn guids(ctx: &Ctx) -> Vec<String> {
    (0..ctx.reaper.track_count(ctx.proj)).filter_map(|at| ctx.reaper.track_at(ctx.proj, at)).map(|t| ctx.reaper.track_guid(t)).collect()
}

fn by_guid(ctx: &Ctx, guid: &str) -> Option<Track> {
    ctx.reads.track_by_id(ctx.reaper, ctx.proj, &TrackId::Guid(guid.to_string())).ok()
}

/// Deletes these tracks, last first (what was made, taken away again inside the call that made it).
fn delete_all(ctx: &Ctx, made: &[String]) {
    for guid in made.iter().rev() {
        if let Some(track) = by_guid(ctx, guid) {
            ctx.reaper.delete_track(track);
        }
    }
}

pub fn apply(ctx: &Ctx, input: &Value, fence: &Value) -> Done<Applied> {
    let path = template_path(ctx, input).map_err(|fault| {
        if fault.0 == "failed" {
            changed(format!("{}: it went since the preview", fault.1))
        } else {
            fault
        }
    })?;
    let template = read_template(&path)?;
    let at = place_of(ctx, input)?;
    if &fence_of(ctx, at, &template) != fence {
        return Err(changed("the tracks, or the template file, changed since the preview"));
    }
    let reaper = ctx.reaper;
    let kept = selection(ctx);
    let before = guids(ctx);
    if !reaper.open_project(&path.to_string_lossy()) {
        return Err(failed("this REAPER can't insert track templates"));
    }
    // The tracks REAPER made: those it didn't have, in its order (at the end).
    let made: Vec<String> = guids(ctx).into_iter().filter(|guid| !before.contains(guid)).collect();
    if made.is_empty() {
        put_selection(ctx, &kept);
        return Err(failed("REAPER made no tracks from the template"));
    }
    // A template whose tracks open a folder they don't close would pull the tracks after them into it: those go at the end.
    let net: i64 = made.iter().filter_map(|guid| by_guid(ctx, guid)).map(|t| reaper.track_value(t, "I_FOLDERDEPTH") as i64).sum();
    let count = reaper.track_count(ctx.proj) as i64;
    let first_at = count - made.len() as i64;
    if at < first_at {
        if net != 0 {
            delete_all(ctx, &made);
            put_selection(ctx, &kept);
            return Err(bad("the template's tracks open a folder they don't close: insert it at the end (no index)"));
        }
        // Moved as one block, just above the track at the asked place.
        for t in 0..reaper.track_count(ctx.proj) {
            if let Some(track) = reaper.track_at(ctx.proj, t) {
                reaper.set_track_selected(track, made.contains(&reaper.track_guid(track)));
            }
        }
        if !reaper.reorder_selected(at as i32, 0) {
            delete_all(ctx, &made);
            put_selection(ctx, &kept);
            return Err(failed("REAPER wouldn't move the template's tracks to their place: they are taken away again"));
        }
    }
    put_selection(ctx, &kept);
    let placed: Vec<i64> =
        made.iter().filter_map(|guid| by_guid(ctx, guid)).map(|t| reaper.track_value(t, "IP_TRACKNUMBER") as i64 - 1).collect();
    let in_place = placed.len() == made.len() && placed.iter().enumerate().all(|(n, p)| *p == at + n as i64);
    if !in_place {
        delete_all(ctx, &made);
        return Err(failed("REAPER put the template's tracks elsewhere than asked: they are taken away again"));
    }
    let tracks: Vec<Value> =
        made.iter().filter_map(|guid| by_guid(ctx, guid).map(|t| json!({"guid": guid, "signature": signature(reaper, t)}))).collect();
    let track_refs: Vec<String> = made.iter().map(|guid| refs::track_ref(&TrackId::Guid(guid.clone()))).collect();
    Ok(Applied {
        applied: json!({"index": at, "tracks": tracks}),
        created: Some(json!({"ref": track_refs[0], "kind": "track", "trackRefs": track_refs})),
        material: None,
        display: json!({"title": title(&path, made.len(), at)}),
        ..Default::default()
    })
}

pub fn restore(ctx: &Ctx, restore: &Restore) -> Done<Restored> {
    let reaper = ctx.reaper;
    let tracks = restore.applied["tracks"].as_array().ok_or_else(|| bad("applied: the tracks made"))?;
    let made: Vec<String> = tracks.iter().filter_map(|t| t["guid"].as_str().map(str::to_string)).collect();
    let present: Vec<(String, Track)> = made.iter().filter_map(|guid| by_guid(ctx, guid).map(|t| (guid.clone(), t))).collect();
    if present.is_empty() {
        return Err(("gone".into(), "the tracks the template made aren't in the project any more".into()));
    }
    if !restore.discard {
        if present.len() != made.len() {
            return Err(changed("some of the tracks the template made were deleted in REAPER since: the others stay"));
        }
        for (entry, (_, track)) in tracks.iter().zip(&present) {
            if signature(reaper, *track) != entry["signature"] {
                return Err(changed(format!(
                    "{} has been changed in REAPER since the template made it",
                    super::quoted(&super::track_name(reaper, *track))
                )));
            }
        }
        // Still one block, and no track of the producer's inside its folders.
        let places: Vec<i64> = present.iter().map(|(_, t)| reaper.track_value(*t, "IP_TRACKNUMBER") as i64 - 1).collect();
        if places.windows(2).any(|w| w[1] != w[0] + 1) {
            return Err(changed("the template's tracks aren't together any more: other tracks were moved among them"));
        }
        let inside = (0..reaper.track_count(ctx.proj)).filter_map(|at| reaper.track_at(ctx.proj, at)).any(|other| {
            !present.iter().any(|(_, t)| *t == other)
                && reaper.track_parent(other).is_some_and(|parent| present.iter().any(|(_, t)| *t == parent))
        });
        if inside {
            return Err(changed("a track of the producer's is inside the template's folder now"));
        }
    }
    let names: Vec<String> = present.iter().map(|(_, t)| super::track_name(reaper, *t)).collect();
    for (_, track) in present.iter().rev() {
        if !reaper.delete_track(*track) {
            return Err(("uncertain".into(), "REAPER wouldn't delete one of the template's tracks: read the tracks".into()));
        }
    }
    Ok(Restored {
        restored: json!({"deleted": made.len()}),
        display: json!({"title": format!("Template tracks {} taken away again", names.iter().map(|n| super::quoted(n)).collect::<Vec<_>>().join(", "))}),
        ..Default::default()
    })
}
