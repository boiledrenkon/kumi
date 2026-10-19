//! Markers and regions: adding, moving, renaming, colouring and deleting them. A marker is found by its GUID. REAPER makes a
//! new GUID for one made again, so undoing a delete names the marker it makes by a new ref.

use serde_json::{json, Value};

use super::tracks::colour_of;
use super::{Applied, Ctx, Done, Fault, Preview, Restore, Restored};
use crate::read::{bad, failed};
use crate::reaper::{Marker, Reaper};
use crate::refs::{self, Ref, TrackId};
use crate::rows;

fn changed(message: impl Into<String>) -> Fault {
    ("changed".into(), message.into())
}

fn reference(marker: &Marker) -> String {
    if marker.is_region {
        refs::region_ref(&marker.guid)
    } else {
        refs::marker_ref(&marker.guid)
    }
}

fn kind_word(marker_is_region: bool) -> &'static str {
    if marker_is_region {
        "region"
    } else {
        "marker"
    }
}

fn target(marker: &Marker) -> Value {
    json!({"ref": reference(marker), "kind": kind_word(marker.is_region)})
}

/// The marker or region a ref names.
fn find(ctx: &Ctx, text: &Value) -> Done<Marker> {
    let text = text.as_str().ok_or_else(|| bad("ref must be a marker or region ref"))?;
    let (guid, region) = match Ref::parse(text).map_err(bad)? {
        Ref::Marker(guid) => (guid, false),
        Ref::Region(guid) => (guid, true),
        other => return Err(bad(format!("ref must be a marker or region ref, not r:{}", other.kind()))),
    };
    ctx.reaper
        .marker_by_guid(ctx.proj, &guid)
        .filter(|marker| marker.is_region == region)
        .ok_or_else(|| failed(format!("that {} isn't in the project (deleted, or another tab)", kind_word(region))))
}

fn find_for_apply(ctx: &Ctx, text: &Value) -> Done<Marker> {
    find(ctx, text).map_err(|fault| if fault.0 == "failed" { changed(format!("{}: it went since the preview", fault.1)) } else { fault })
}

fn find_for_restore(ctx: &Ctx, text: &Value) -> Done<Marker> {
    find(ctx, text).map_err(|fault| if fault.0 == "failed" { ("gone".to_string(), fault.1) } else { fault })
}

fn label(marker: &Marker) -> String {
    let word = if marker.is_region { "Region" } else { "Marker" };
    if marker.name.is_empty() {
        format!("{word} {}", marker.number)
    } else {
        format!("{word} {}", super::quoted(&rows::clip(&marker.name)))
    }
}

/// Where a time is, as a producer says it: `bar 33`, or `bar 33 beat 2.5` off the bar line.
pub fn bar_text(reaper: &Reaper, proj: crate::reaper::Project, time: f64) -> String {
    match reaper.bar_and_beat_at(proj, time) {
        Some((measure, beat)) if beat.abs() < 0.005 => format!("bar {}", measure + 1),
        Some((measure, beat)) => format!("bar {} beat {}", measure + 1, trimmed(beat + 1.0)),
        None => format!("{time:.2} s"),
    }
}

pub(super) fn trimmed(value: f64) -> String {
    let text = format!("{:.2}", value);
    text.trim_end_matches('0').trim_end_matches('.').to_string()
}

/// A time in the input, in seconds: given as `…Beats` (quarter notes from the start) or in seconds, not both.
pub(super) fn moment(ctx: &Ctx, input: &Value, beats: &str, seconds: &str) -> Done<Option<f64>> {
    let number = |key: &str| -> Done<Option<f64>> {
        match input.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(value) => value
                .as_f64()
                .filter(|n| n.is_finite() && (0.0..=1.0e6).contains(n))
                .map(Some)
                .ok_or_else(|| bad(format!("{key} must be a number from 0"))),
        }
    };
    match (number(beats)?, number(seconds)?) {
        (Some(_), Some(_)) => Err(bad(format!("pass {beats} or {seconds}, not both"))),
        (Some(qn), None) => {
            ctx.reaper.time_at_quarter_notes(ctx.proj, qn).map(Some).ok_or_else(|| failed("this REAPER can't turn beats into time"))
        }
        (None, seconds) => Ok(seconds),
    }
}

fn text_input(input: &Value, key: &str) -> Done<Option<String>> {
    match input.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) if text.chars().count() <= 256 && !text.chars().any(char::is_control) => Ok(Some(text.clone())),
        Some(_) => Err(bad(format!("{key} must be one line of at most 256 characters"))),
    }
}

fn colour_input(ctx: &Ctx, input: &Value) -> Done<Option<i32>> {
    match input.get("color") {
        None => Ok(None),
        Some(Value::Null) => Ok(Some(0)),
        Some(Value::String(hex)) => {
            let digits = hex
                .strip_prefix('#')
                .filter(|d| d.len() == 6 && d.bytes().all(|b| b.is_ascii_hexdigit()))
                .ok_or_else(|| bad("color must be #rrggbb, or null for the default"))?;
            let channel = |at: usize| i32::from_str_radix(&digits[at..at + 2], 16).unwrap_or(0);
            ctx.reaper
                .color_to_native(channel(0), channel(2), channel(4))
                .map(|native| Some(native | 0x100_0000))
                .ok_or_else(|| failed("this REAPER can't make a colour"))
        }
        Some(_) => Err(bad("color must be #rrggbb, or null for the default")),
    }
}

// ----- adding -----

struct Add {
    region: bool,
    start: f64,
    end: f64,
    name: String,
    color: i32,
}

fn add_plan(ctx: &Ctx, input: &Value, region: bool) -> Done<Add> {
    let start = moment(ctx, input, "positionBeats", "position")?
        .ok_or_else(|| bad("pass positionBeats (quarter notes from the start) or position (seconds)"))?;
    let end = if region {
        let end = moment(ctx, input, "endBeats", "end")?.ok_or_else(|| bad("a region needs endBeats or end"))?;
        if end <= start {
            return Err(bad("a region must end after it starts"));
        }
        end
    } else {
        start
    };
    Ok(Add { region, start, end, name: text_input(input, "name")?.unwrap_or_default(), color: colour_input(ctx, input)?.unwrap_or(0) })
}

pub fn add_preview(ctx: &Ctx, input: &Value, region: bool) -> Done<Preview> {
    let plan = add_plan(ctx, input, region)?;
    let word = if region { "Region" } else { "Marker" };
    let name = if plan.name.is_empty() { String::new() } else { format!(" {}", super::quoted(&plan.name)) };
    let title = if region {
        format!("{word}{name} from {} to {}", bar_text(ctx.reaper, ctx.proj, plan.start), bar_text(ctx.reaper, ctx.proj, plan.end))
    } else {
        format!("{word}{name} at {}", bar_text(ctx.reaper, ctx.proj, plan.start))
    };
    Ok(Preview {
        target: json!({"ref": null, "kind": kind_word(region)}),
        prior: json!({"markers": ctx.reaper.marker_count(ctx.proj)}),
        fence: json!({}),
        display: json!({"title": title}),
    })
}

pub fn add_apply(ctx: &Ctx, input: &Value, region: bool) -> Done<Applied> {
    let plan = add_plan(ctx, input, region)?;
    let guid = ctx
        .reaper
        .add_marker(ctx.proj, plan.region, plan.start, plan.end, &plan.name, plan.color)
        .ok_or_else(|| failed("REAPER wouldn't add the marker"))?;
    let marker = ctx.reaper.marker_by_guid(ctx.proj, &guid).ok_or_else(|| failed("the marker made isn't in the project"))?;
    let word = if region { "Region" } else { "Marker" };
    let name = if marker.name.is_empty() { String::new() } else { format!(" {}", super::quoted(&rows::clip(&marker.name))) };
    let title = if region {
        format!("{word}{name} from {} to {}", bar_text(ctx.reaper, ctx.proj, marker.start), bar_text(ctx.reaper, ctx.proj, marker.end))
    } else {
        format!("{word}{name} at {}", bar_text(ctx.reaper, ctx.proj, marker.start))
    };
    Ok(Applied {
        applied: state(&marker),
        created: Some(target(&marker)),
        material: None,
        display: json!({"title": title}),
        ..Default::default()
    })
}

/// What a marker is, exactly: where, what it is called, its colour and number.
fn state(marker: &Marker) -> Value {
    json!({"start": marker.start, "end": marker.end, "name": marker.name, "color": marker.color, "number": marker.number})
}

fn same_state(a: &Value, b: &Value) -> bool {
    let close = |key: &str| (a[key].as_f64().unwrap_or(f64::NAN) - b[key].as_f64().unwrap_or(f64::NAN)).abs() < 1e-9;
    close("start") && close("end") && a["name"] == b["name"] && colour_value(a) == colour_value(b)
}

fn colour_value(state: &Value) -> i64 {
    colour_of(&json!({"color": state["color"]}))
}

pub fn add_restore(ctx: &Ctx, restore: &Restore) -> Done<Restored> {
    let created = restore.created.ok_or_else(|| bad("created: the marker that was added"))?;
    let marker = find_for_restore(ctx, &created["ref"])?;
    if !restore.discard && !same_state(&state(&marker), restore.applied) {
        return Err(changed(format!("{} has been changed in REAPER since Kumi added it", label(&marker))));
    }
    if !ctx.reaper.delete_marker(ctx.proj, &marker.guid) {
        return Err(failed("REAPER wouldn't delete the marker"));
    }
    Ok(Restored {
        restored: json!({"deleted": reference(&marker)}),
        display: json!({"title": format!("{} taken away again", label(&marker))}),
        ..Default::default()
    })
}

// ----- deleting -----

pub fn delete_preview(ctx: &Ctx, input: &Value) -> Done<Preview> {
    let marker = find(ctx, &input["ref"])?;
    let now = state(&marker);
    Ok(Preview {
        target: target(&marker),
        prior: now.clone(),
        fence: now,
        display: json!({"title": format!("Deleted {}", label(&marker).to_lowercase_first())}),
    })
}

trait LowerFirst {
    fn to_lowercase_first(&self) -> String;
}

impl LowerFirst for String {
    fn to_lowercase_first(&self) -> String {
        let mut chars = self.chars();
        chars.next().map(|c| c.to_lowercase().chain(chars).collect()).unwrap_or_default()
    }
}

pub fn delete_apply(ctx: &Ctx, input: &Value, fence: &Value) -> Done<Applied> {
    let marker = find_for_apply(ctx, &input["ref"])?;
    if !same_state(&state(&marker), fence) {
        return Err(changed(format!("{} changed since the preview", label(&marker))));
    }
    // What a marker is besides its state: the ruler lane it sits in, and for a region the tracks it renders in the region
    // render matrix (by id, "master" for the master).
    let matrix: Vec<String> = if marker.is_region {
        ctx.reaper
            .region_render_tracks(ctx.proj, marker.number)
            .into_iter()
            .map(|track| match super::tracks::track_id_of(ctx, track) {
                TrackId::Master => "master".to_string(),
                TrackId::Guid(guid) => guid,
            })
            .collect()
    } else {
        vec![]
    };
    let material =
        json!({"region": marker.is_region, "state": state(&marker), "was": reference(&marker), "lane": marker.lane, "matrix": matrix});
    if !ctx.reaper.delete_marker(ctx.proj, &marker.guid) {
        return Err(failed("REAPER wouldn't delete the marker"));
    }
    Ok(Applied {
        applied: json!({"deleted": true}),
        created: None,
        material: Some(material),
        display: json!({"title": format!("Deleted {}", label(&marker).to_lowercase_first())}),
        ..Default::default()
    })
}

pub fn delete_restore(ctx: &Ctx, restore: &Restore) -> Done<Restored> {
    let material = restore.material.ok_or_else(|| bad("material: what the delete removed"))?;
    let saved = &material["state"];
    let region = material["region"] == true;
    // A marker with its id is there: nothing is made twice.
    if let Ok(existing) = find(ctx, &restore.target["ref"]) {
        if restore.discard || same_state(&state(&existing), saved) {
            return Ok(Restored {
                restored: json!({"ref": reference(&existing)}),
                display: json!({"title": format!("{} is back already", label(&existing))}),
                unwritten: true,
            });
        }
        return Err(changed("a marker with that id is in the project again"));
    }
    // One made again is a new id, so a retry (the first answer was lost) must know it by what it is: the same kind of
    // marker in the same place, with the same name, colour and number, is the one made already.
    let reaper = ctx.reaper;
    let same = (0..reaper.marker_count(ctx.proj))
        .filter_map(|index| reaper.marker(ctx.proj, index))
        .find(|candidate| candidate.is_region == region && same_state(&state(candidate), saved));
    if let Some(made) = same {
        return Ok(Restored {
            restored: json!({"ref": reference(&made), "was": material["was"]}),
            display: json!({"title": format!("{} is back already", label(&made))}),
            unwritten: true,
        });
    }
    let start = saved["start"].as_f64().unwrap_or(0.0);
    let end = saved["end"].as_f64().unwrap_or(start);
    let guid = ctx
        .reaper
        .add_marker(ctx.proj, region, start, end, saved["name"].as_str().unwrap_or_default(), saved["color"].as_i64().unwrap_or(0) as i32)
        .ok_or_else(|| failed("REAPER wouldn't make the marker again"))?;
    // Its number as it was, when it can be (REAPER hands out another if that one is taken).
    if let Some(number) = saved["number"].as_i64() {
        ctx.reaper.set_marker_value(ctx.proj, &guid, "I_NUMBER", number as f64);
    }
    // The ruler lane it sat in, and the render matrix a region had (the tracks that no longer exist are left out).
    if let Some(lane) = material["lane"].as_i64().filter(|lane| *lane != 0) {
        ctx.reaper.set_marker_value(ctx.proj, &guid, "I_LANENUMBER", lane as f64);
    }
    let marker = ctx.reaper.marker_by_guid(ctx.proj, &guid).ok_or_else(|| failed("the marker made again isn't in the project"))?;
    for id in material["matrix"].as_array().into_iter().flatten().filter_map(Value::as_str) {
        let track = match id {
            "master" => ctx.reaper.master_track(ctx.proj),
            guid => ctx.reads.track_by_id(ctx.reaper, ctx.proj, &TrackId::Guid(guid.to_string())).ok(),
        };
        if let Some(track) = track {
            ctx.reaper.add_to_region_render(ctx.proj, marker.number, track);
        }
    }
    Ok(Restored {
        restored: json!({"ref": reference(&marker), "was": material["was"]}),
        display: json!({"title": format!("{} put back", label(&marker)), "note": "REAPER gives a marker made again a new id."}),
        ..Default::default()
    })
}

// ----- changing one -----

/// An attribute of a marker or region.
pub struct Prop {
    pub kind: &'static str,
    read: fn(&Marker) -> Value,
    wanted: fn(&Ctx, &Value, &Marker) -> Done<Value>,
    write: fn(&Ctx, &Marker, &Value) -> bool,
    same: fn(&Value, &Value) -> bool,
    title: fn(&Ctx, &Marker, &Value) -> String,
    noun: &'static str,
}

pub static PROPS: [Prop; 4] = [
    Prop {
        kind: "marker-move",
        read: |marker| json!({"start": marker.start, "end": marker.end}),
        wanted: |ctx, input, marker| {
            let start = moment(ctx, input, "positionBeats", "position")?
                .ok_or_else(|| bad("pass positionBeats (quarter notes from the start) or position (seconds)"))?;
            let end = moment(ctx, input, "endBeats", "end")?;
            if !marker.is_region {
                if end.is_some() {
                    return Err(bad("a marker has no end: end and endBeats are for regions"));
                }
                return Ok(json!({"start": start, "end": start}));
            }
            // A region moved by its start keeps its length.
            let end = end.unwrap_or(start + (marker.end - marker.start));
            if end <= start {
                return Err(bad("a region must end after it starts"));
            }
            Ok(json!({"start": start, "end": end}))
        },
        write: |ctx, marker, state| {
            let reaper = ctx.reaper;
            // The end first when it moves later, the start first when it moves earlier: a region never ends before it starts.
            let (start, end) = (state["start"].as_f64().unwrap_or(0.0), state["end"].as_f64().unwrap_or(0.0));
            if marker.is_region && end > marker.end {
                reaper.set_marker_value(ctx.proj, &marker.guid, "D_ENDPOS", end)
                    && reaper.set_marker_value(ctx.proj, &marker.guid, "D_STARTPOS", start)
            } else {
                reaper.set_marker_value(ctx.proj, &marker.guid, "D_STARTPOS", start)
                    && (!marker.is_region || reaper.set_marker_value(ctx.proj, &marker.guid, "D_ENDPOS", end))
            }
        },
        same: |a, b| {
            (a["start"].as_f64().unwrap_or(f64::NAN) - b["start"].as_f64().unwrap_or(f64::NAN)).abs() < 1e-9
                && (a["end"].as_f64().unwrap_or(f64::NAN) - b["end"].as_f64().unwrap_or(f64::NAN)).abs() < 1e-9
        },
        title: |ctx, marker, to| {
            let at = bar_text(ctx.reaper, ctx.proj, to["start"].as_f64().unwrap_or(0.0));
            format!("Moved {} to {at}", label(marker).to_lowercase_first())
        },
        noun: "place",
    },
    Prop {
        kind: "marker-rename",
        read: |marker| json!({"name": marker.name}),
        wanted: |_, input, _| {
            let name = text_input(input, "name")?.ok_or_else(|| bad("name must be text"))?;
            Ok(json!({"name": name}))
        },
        write: |ctx, marker, state| ctx.reaper.set_marker_name(ctx.proj, &marker.guid, state["name"].as_str().unwrap_or_default()),
        same: |a, b| a["name"] == b["name"],
        title: |_, marker, to| {
            let name = to["name"].as_str().unwrap_or_default();
            let shown = if name.is_empty() { "no name".to_string() } else { super::quoted(name) };
            format!("Renamed {} → {shown}", label(marker).to_lowercase_first())
        },
        noun: "name",
    },
    Prop {
        kind: "marker-color",
        read: |marker| json!({"color": marker.color}),
        wanted: |ctx, input, _| {
            let color = colour_input(ctx, input)?.ok_or_else(|| bad("color must be #rrggbb, or null for the default"))?;
            Ok(json!({"color": color}))
        },
        write: |ctx, marker, state| {
            ctx.reaper.set_marker_value(ctx.proj, &marker.guid, "I_CUSTOMCOLOR", state["color"].as_f64().unwrap_or(0.0))
        },
        same: |a, b| colour_value(a) == colour_value(b),
        title: |_, marker, to| {
            if colour_value(to) == 0 {
                format!("{} colour cleared", label(marker))
            } else {
                format!("{} colour changed", label(marker))
            }
        },
        noun: "colour",
    },
    // (r-tracks, M4) The ruler lane it sits in (REAPER 7.62+: lanes counted from 0, as the rows' lane says).
    Prop {
        kind: "marker-lane",
        read: |marker| json!({"lane": marker.lane}),
        wanted: |ctx, input, _| {
            let count = ctx.reaper.project_info(ctx.proj, "RULER_LANE_COUNT").unwrap_or(1.0) as i64;
            let lane = input.get("lane").and_then(Value::as_i64).ok_or_else(|| bad("lane must be a ruler lane's number, from 0"))?;
            if !(0..count).contains(&lane) {
                return Err(bad(format!("the project has {count} ruler lanes, 0 to {}: add one first (ruler_lane_add)", count - 1)));
            }
            Ok(json!({"lane": lane}))
        },
        write: |ctx, marker, state| {
            ctx.reaper.set_marker_value(ctx.proj, &marker.guid, "I_LANENUMBER", state["lane"].as_f64().unwrap_or(0.0))
        },
        same: |a, b| a["lane"].as_i64() == b["lane"].as_i64(),
        title: |ctx, marker, to| {
            let lane = to["lane"].as_i64().unwrap_or(0);
            let name = ctx.reaper.project_text(ctx.proj, &format!("RULER_LANE_NAME:{lane}")).unwrap_or_default();
            let name = if name.is_empty() { String::new() } else { format!(" ({})", super::quoted(&name)) };
            format!("Moved {} to ruler lane {lane}{name}", label(marker).to_lowercase_first())
        },
        noun: "ruler lane",
    },
];

pub fn prop_preview(ctx: &Ctx, prop: &Prop, input: &Value) -> Done<Preview> {
    let marker = find(ctx, &input["ref"])?;
    let now = (prop.read)(&marker);
    let wanted = (prop.wanted)(ctx, input, &marker)?;
    let mut display = json!({"title": (prop.title)(ctx, &marker, &wanted), "before": now, "after": wanted});
    if (prop.same)(&now, &wanted) {
        display["unchanged"] = true.into();
    }
    Ok(Preview { target: target(&marker), prior: now.clone(), fence: now, display })
}

pub fn prop_apply(ctx: &Ctx, prop: &Prop, input: &Value, fence: &Value) -> Done<Applied> {
    let marker = find_for_apply(ctx, &input["ref"])?;
    let now = (prop.read)(&marker);
    if !(prop.same)(&now, fence) {
        return Err(changed(format!("{}'s {} changed since the preview", label(&marker), prop.noun)));
    }
    let wanted = (prop.wanted)(ctx, input, &marker)?;
    if !(prop.same)(&now, &wanted) && !(prop.write)(ctx, &marker, &wanted) {
        return Err(failed(format!("REAPER wouldn't take the {}", prop.noun)));
    }
    let after_marker = ctx.reaper.marker_by_guid(ctx.proj, &marker.guid).ok_or_else(|| failed("the marker went while it was changed"))?;
    let after = (prop.read)(&after_marker);
    if !(prop.same)(&after, &wanted) {
        return Err(failed(format!("REAPER kept {}'s {} as it was", label(&marker), prop.noun)));
    }
    Ok(Applied {
        applied: after.clone(),
        created: None,
        material: None,
        display: json!({"title": (prop.title)(ctx, &marker, &after), "before": now, "after": after}),
        unwritten: (prop.same)(&now, &wanted),
    })
}

pub fn prop_restore(ctx: &Ctx, prop: &Prop, restore: &Restore) -> Done<Restored> {
    let marker = find_for_restore(ctx, &restore.target["ref"])?;
    let now = (prop.read)(&marker);
    let already = (prop.same)(&now, restore.prior);
    if !already {
        if !restore.discard && !(prop.same)(&now, restore.applied) {
            return Err(changed(format!("{}'s {} changed in REAPER since Kumi set it", label(&marker), prop.noun)));
        }
        if !(prop.write)(ctx, &marker, restore.prior) {
            return Err(failed(format!("REAPER wouldn't put back the {}", prop.noun)));
        }
    }
    let after_marker = ctx.reaper.marker_by_guid(ctx.proj, &marker.guid).ok_or_else(|| failed("the marker went while it was changed"))?;
    let after = (prop.read)(&after_marker);
    if !(prop.same)(&after, restore.prior) {
        return Err(failed(format!("REAPER kept {}'s {} as it was", label(&marker), prop.noun)));
    }
    Ok(Restored {
        restored: after.clone(),
        display: json!({"title": format!("{} (put back)", (prop.title)(ctx, &marker, &after)), "before": now, "after": after}),
        unwritten: already,
    })
}
