//! The rows `read`, `snapshot` and the FOCUS bodies answer with: one JSON object per REAPER object, named in camelCase
//! (the shapes are in the M2 contract). Times are seconds, positions in MIDI and sends in quarter notes, gains in dB.

use std::collections::HashMap;

use serde_json::{json, Value};

use crate::reaper::{Envelope, Fx, FxOwner, Item, Marker, MidiNote, PlayState, Project, ProjectInfo, Reaper, Take, Track};
use crate::refs::{self, EnvelopeId, SendEnd, TrackId};

/// Longest text a row carries from REAPER (a name, a preset): a pasted essay of a track name mustn't fill an answer.
pub const TEXT_LIMIT: usize = 1024;
/// REAPER's fader floor, which stands for minus infinity.
pub const MIN_DB: f64 = -150.0;

pub fn clip(text: &str) -> String {
    if text.chars().count() <= TEXT_LIMIT {
        return text.to_string();
    }
    let mut clipped: String = text.chars().take(TEXT_LIMIT - 1).collect();
    clipped.push('…');
    clipped
}

fn round(value: f64, places: i32) -> f64 {
    if !value.is_finite() {
        return 0.0;
    }
    let scale = 10f64.powi(places);
    (value * scale).round() / scale
}

/// A time in seconds, to the microsecond.
pub fn seconds(value: f64) -> f64 {
    round(value, 6)
}

/// A gain factor in dB, floored at REAPER's fader bottom.
pub fn db(gain: f64) -> f64 {
    if gain <= 1e-7 {
        MIN_DB
    } else {
        round((20.0 * gain.log10()).max(MIN_DB), 2)
    }
}

pub fn track_id(reaper: &Reaper, track: Track) -> TrackId {
    if reaper.track_value(track, "IP_TRACKNUMBER") < 0.0 {
        TrackId::Master
    } else {
        TrackId::Guid(reaper.track_guid(track))
    }
}

fn color(reaper: &Reaper, native: Option<i32>) -> Value {
    native.and_then(|c| reaper.color_hex(c)).map_or(Value::Null, Value::from)
}

/// A colour field stored as an int in `I_CUSTOMCOLOR`: set only when the 0x1000000 flag is.
fn custom_color(reaper: &Reaper, value: f64) -> Value {
    let native = value as i32;
    color(reaper, (native & 0x100_0000 != 0).then_some(native))
}

pub fn play_state_name(state: PlayState) -> &'static str {
    match (state.recording, state.playing, state.paused) {
        (true, _, true) => "recording-paused",
        (true, _, false) => "recording",
        (false, true, _) => "playing",
        (false, false, true) => "paused",
        _ => "stopped",
    }
}

/// What the transport shows. `beat` counts quarter notes from the project's start, and the loop is in the same beats.
pub fn transport(reaper: &Reaper, proj: Project) -> Value {
    let state = reaper.play_state(proj);
    let now = if state.playing || state.recording { reaper.play_position(proj) } else { reaper.cursor(proj) };
    let (loop_start, loop_end) = reaper.loop_points(proj);
    let signature = reaper.time_signature_at(proj, now);
    let beats = |time: f64| reaper.quarter_notes_at(proj, time).map(seconds);
    let mut transport = json!({
        "playing": state.playing,
        "paused": state.paused,
        "recording": state.recording,
        "state": play_state_name(state),
        "seconds": seconds(now),
        "beat": beats(now),
        "tempo": signature.map(|s| round(s.bpm, 3)).or_else(|| reaper.tempo()),
        "timeSignature": signature.map(|s| json!([s.num, s.denom])),
        "beatsPerBar": signature.map(|s| round(s.num as f64 * 4.0 / s.denom.max(1) as f64, 4)),
        "loop": {"on": reaper.repeat(proj), "start": beats(loop_start), "end": beats(loop_end)},
        "loopSeconds": {"start": seconds(loop_start), "end": seconds(loop_end)},
    });
    if let Some((bar, beat)) = reaper.bar_and_beat_at(proj, now) {
        transport["bar"] = (bar + 1).into();
        transport["beatInBar"] = round(beat + 1.0, 3).into();
    }
    transport
}

/// The time selection, or null when there is none.
pub fn time_selection(reaper: &Reaper, proj: Project) -> Value {
    let (start, end) = reaper.time_selection(proj);
    if end > start {
        json!({"start": seconds(start), "end": seconds(end)})
    } else {
        Value::Null
    }
}

/// The project's row. `identity` is what names this open project while it stays open in this REAPER; `id` is the Kumi
/// project id the project itself carries (see `Reads::project_id`), null before it has one of its own.
pub fn project_row(reaper: &Reaper, proj: Project, info: &ProjectInfo, identity: &str, id: Option<&str>) -> Value {
    let cursor = reaper.cursor(proj);
    let state = reaper.play_state(proj);
    let (loop_start, loop_end) = reaper.loop_points(proj);
    let signature = reaper.time_signature_at(proj, cursor);
    json!({
        "id": id,
        "identity": identity,
        "name": clip(&info.name),
        "path": clip(&info.path),
        "saved": !info.path.is_empty(),
        "dirty": reaper.is_dirty(proj),
        "tempo": signature.map(|s| round(s.bpm, 3)).or_else(|| reaper.tempo()),
        "timeSignature": signature.map(|s| json!([s.num, s.denom])),
        "length": seconds(reaper.project_length(proj)),
        "playState": play_state_name(state),
        "cursor": seconds(cursor),
        "loop": {"on": reaper.repeat(proj), "start": seconds(loop_start), "end": seconds(loop_end)},
        "timeSelection": time_selection(reaper, proj),
        "tabCount": reaper.tab_count(),
    })
}

pub fn track_row(reaper: &Reaper, track: Track) -> Value {
    let id = track_id(reaper, track);
    let master = id == TrackId::Master;
    let number = reaper.track_value(track, "IP_TRACKNUMBER");
    let mut row = json!({
        "ref": refs::track_ref(&id),
        "index": if master { 0 } else { number as i64 },
        "name": clip(&reaper.track_name(track)),
        "depth": if master { 0 } else { reaper.track_depth(track) },
        "folder": reaper.track_value(track, "I_FOLDERDEPTH") as i64,
        "color": color(reaper, reaper.track_color(track)),
        "mute": reaper.track_value(track, "B_MUTE") != 0.0,
        "solo": reaper.track_value(track, "I_SOLO") != 0.0,
        "arm": reaper.track_value(track, "I_RECARM") != 0.0,
        "volumeDb": db(reaper.track_value(track, "D_VOL")),
        "pan": round(reaper.track_value(track, "D_PAN"), 4),
        "fxCount": reaper.fx_count(FxOwner::Track(track)),
        "itemCount": reaper.track_item_count(track),
        "sends": reaper.send_count(track, 0),
        "receives": reaper.send_count(track, -1),
        "selected": reaper.track_value(track, "I_SELECTED") != 0.0,
    });
    if master {
        row["master"] = true.into();
    }
    row
}

/// What a take's source is called in a row: "wav", "mid", other types in lower case, "empty" for no take or source.
pub fn source_word(reaper: &Reaper, take: Option<Take>) -> String {
    match take.and_then(|t| reaper.take_source_type(t)) {
        None => "empty".into(),
        Some(kind) => match kind.to_ascii_lowercase().as_str() {
            "wave" => "wav".into(),
            "midi" => "mid".into(),
            other => other.to_string(),
        },
    }
}

pub fn item_row(reaper: &Reaper, proj: Project, item: Item) -> Value {
    let guid = reaper.item_guid(item);
    let take = reaper.active_take(item);
    let (position, length) = (reaper.item_value(item, "D_POSITION"), reaper.item_value(item, "D_LENGTH"));
    let start = reaper.quarter_notes_at(proj, position);
    let end = reaper.quarter_notes_at(proj, position + length);
    json!({
        "ref": refs::item_ref(&guid),
        "trackRef": reaper.item_track(item).map(|t| refs::track_ref(&track_id(reaper, t))),
        "position": seconds(position),
        "length": seconds(length),
        "positionBeats": start.map(seconds),
        "lengthBeats": start.zip(end).map(|(start, end)| seconds(end - start)),
        "name": take.map(|t| clip(&reaper.take_name(t))).unwrap_or_default(),
        "takeRef": take.map(|t| refs::take_ref(&reaper.take_guid(t))),
        "takes": reaper.take_count(item),
        "source": source_word(reaper, take),
        "muted": reaper.item_value(item, "B_MUTE") != 0.0,
        "lane": reaper.item_value(item, "I_FIXEDLANE") as i64,
        "color": custom_color(reaper, reaper.item_value(item, "I_CUSTOMCOLOR")),
        "selected": reaper.item_value(item, "B_UISEL") != 0.0,
    })
}

/// An FX in a chain: `position` is its place there; a container lists its children's refs.
pub fn fx_row(reaper: &Reaper, fx: Fx, position: usize) -> Value {
    let guid = reaper.fx_guid(fx);
    let renamed = reaper.fx_config(fx, "renamed_name").unwrap_or_default();
    let name = if renamed.is_empty() { reaper.fx_name(fx) } else { renamed };
    let children = reaper.fx_config(fx, "container_count").is_some().then(|| reaper.fx_children(fx));
    let mut row = json!({
        "ref": refs::fx_ref(&guid),
        "index": position,
        "name": clip(&name),
        "enabled": reaper.fx_enabled(fx),
        "offline": reaper.fx_offline(fx),
        "preset": clip(&reaper.fx_preset(fx)),
        "paramCount": reaper.fx_param_count(fx),
        "container": children.is_some(),
    });
    if let Some(children) = children {
        row["childRefs"] = children.into_iter().map(|child| refs::fx_ref(&reaper.fx_guid(child))).collect::<Vec<_>>().into();
    }
    if let Some(parallel) = reaper.fx_config(fx, "parallel").and_then(|p| p.trim().parse::<i32>().ok()).filter(|&p| p != 0) {
        row["parallel"] = parallel.into();
    }
    if fx.index & crate::reaper::INPUT_FX != 0 {
        row["input"] = true.into();
    }
    row
}

pub fn param_row(reaper: &Reaper, fx: Fx, fx_guid: &str, index: i32) -> Value {
    let (min, max) = reaper.param_range(fx, index).unwrap_or((0.0, 1.0));
    json!({
        "ref": refs::param_ref(fx_guid, index),
        "index": index,
        "name": clip(&reaper.param_name(fx, index)),
        "value": round(reaper.param_normalized(fx, index), 6),
        "formatted": clip(&reaper.param_formatted(fx, index)),
        "min": min,
        "max": max,
    })
}

pub fn send_mode_name(mode: i64) -> &'static str {
    match mode {
        0 => "post-fader",
        1 => "pre-fx",
        3 => "pre-fader",
        8 => "pre-receive",
        _ => "other",
    }
}

/// Channels of a send as `{first, count}`: the source's count comes from the width REAPER packs above the low 10 bits.
fn channels(reaper: &Reaper, track: Track, category: i32, index: i32) -> Value {
    let source = reaper.send_value(track, category, index, "I_SRCCHAN") as i64;
    let destination = reaper.send_value(track, category, index, "I_DSTCHAN") as i64;
    let source = if source < 0 {
        Value::Null
    } else {
        let width = source >> 10;
        json!({"first": source & 1023, "count": match width { 0 => 2, 1 => 1, n => 2 * n }})
    };
    json!({"src": source, "dst": {"first": destination & 1023, "mono": destination & 1024 != 0}})
}

/// What names a send in its ref: the other end, and which of several to the same one (from 1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SendKey {
    pub other: SendEnd,
    pub repeat: u32,
}

/// The keys of all of a track's sends (`category` 0), receives (-1) or hardware outputs (1), by position.
pub fn send_keys(reaper: &Reaper, track: Track, category: i32) -> Vec<SendKey> {
    let mut seen: HashMap<String, u32> = HashMap::new();
    (0..reaper.send_count(track, category))
        .map(|index| {
            let other = match reaper.send_peer(track, category, index) {
                Some(peer) => SendEnd::Track(track_id(reaper, peer)),
                None => SendEnd::Hardware(index),
            };
            let count = seen.entry(other.text()).or_insert(0);
            *count += 1;
            SendKey { other, repeat: *count }
        })
        .collect()
}

/// A send (`category` 0), receive (-1) or hardware output (1) of `track`, with the key `send_keys` gave it.
pub fn send_row(reaper: &Reaper, track: Track, category: i32, index: i32, key: &SendKey) -> Value {
    let this = track_id(reaper, track);
    // The other track: where a send goes, where a receive comes from; none for a hardware output.
    let other = match &key.other {
        SendEnd::Track(id) => Some(refs::track_ref(id)),
        SendEnd::Hardware(_) => None,
    };
    let mode = reaper.send_value(track, category, index, "I_SENDMODE") as i64;
    json!({
        "ref": refs::send_ref(&this, category, &key.other, key.repeat),
        "category": category,
        "direction": match category { c if c < 0 => "receive", 0 => "send", _ => "hardware" },
        "index": index,
        "trackRef": refs::track_ref(&this),
        "destRef": other,
        "volumeDb": db(reaper.send_value(track, category, index, "D_VOL")),
        "pan": round(reaper.send_value(track, category, index, "D_PAN"), 4),
        "mute": reaper.send_value(track, category, index, "B_MUTE") != 0.0,
        "mode": mode,
        "modeName": send_mode_name(mode),
        "channels": channels(reaper, track, category, index),
    })
}

pub fn marker_row(reaper: &Reaper, proj: Project, marker: &Marker) -> Value {
    let beats = |time: f64| reaper.quarter_notes_at(proj, time).map(seconds);
    let mut row = json!({
        "ref": if marker.is_region { refs::region_ref(&marker.guid) } else { refs::marker_ref(&marker.guid) },
        "number": marker.number,
        "name": clip(&marker.name),
        "position": seconds(marker.start),
        "positionBeats": beats(marker.start),
        "color": custom_color(reaper, marker.color as f64),
    });
    if marker.is_region {
        row["end"] = seconds(marker.end).into();
        row["endBeats"] = beats(marker.end).into();
    }
    row
}

/// A note in quarter notes from the start of its item.
pub fn note_row(reaper: &Reaper, take: Take, item_start_qn: f64, index: i32, note: &MidiNote) -> Value {
    let start = reaper.ppq_to_quarter_notes(take, note.start_ppq).unwrap_or(0.0);
    let end = reaper.ppq_to_quarter_notes(take, note.end_ppq).unwrap_or(start);
    json!({
        "index": index,
        "pitch": note.pitch,
        "start": round(start - item_start_qn, 4),
        "length": round(end - start, 4),
        "velocity": note.velocity,
        "channel": note.channel + 1,
        "selected": note.selected,
        "muted": note.muted,
    })
}

/// The envelope's row. `which` is how its ref names it.
pub fn envelope_row(reaper: &Reaper, track: &TrackId, which: &EnvelopeId, envelope: Envelope) -> Value {
    json!({
        "ref": refs::envelope_ref(track, which),
        "name": clip(&reaper.envelope_name(envelope)),
        "pointCount": reaper.envelope_point_count(envelope),
        "armed": reaper.envelope_flag(envelope, "ARM"),
        "visible": reaper.envelope_flag(envelope, "VISIBLE"),
        "active": reaper.envelope_flag(envelope, "ACTIVE"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gains_are_decibels_with_a_floor() {
        assert_eq!(db(1.0), 0.0);
        assert_eq!(db(0.5), -6.02);
        assert_eq!(db(2.0), 6.02);
        assert_eq!(db(0.0), MIN_DB);
        assert_eq!(db(1e-12), MIN_DB);
    }

    #[test]
    fn long_text_is_clipped_with_an_ellipsis() {
        assert_eq!(clip("short"), "short");
        let clipped = clip(&"é".repeat(TEXT_LIMIT + 50));
        assert_eq!(clipped.chars().count(), TEXT_LIMIT);
        assert!(clipped.ends_with('…'));
    }
}
