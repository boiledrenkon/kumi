//! `focus_body`: what FOCUS draws for the view the producer is in. The arrange window with its tracks and items, a MIDI
//! take as a piano roll, an FX chain as a tree, a track's routing. Each stays under the frame bound: the arrange window and
//! the piano roll page by a cursor, the others are capped.

use std::collections::BTreeMap;

use serde_json::{json, Value};

use super::{bad, failed, take_start_qn, Pager, Reads, ANSWER_BYTES};
use crate::queue::Answer;
use crate::reaper::{Fx, FxOwner, Item, Project, Reaper, Take, Track, INPUT_FX};
use crate::refs::{self, Ref, TrackId};
use crate::rows;

pub const BODY_KINDS: [&str; 4] = ["arrange", "midi", "fx", "routing"];
/// Tracks in one arrange page, and items one track lists in the window.
const ARRANGE_TRACKS: usize = 500;
const ITEMS_PER_TRACK: usize = 400;
/// What the first arrange page keeps for the markers, regions and tempo map.
const ARRANGE_EXTRAS_BYTES: usize = 48 * 1024;
const WINDOW_MARKERS: usize = 200;
const WINDOW_TEMPO_MARKERS: usize = 200;
/// The longest arrange window asked for, in seconds.
const MAX_WINDOW: f64 = 100.0 * 3600.0;
/// Notes in a MIDI page by default and at most.
const DEFAULT_NOTES: usize = 500;
const MAX_NOTES: usize = 2000;
/// A CC lane keeps this many points; the most CC events read for the lanes.
const LANE_POINTS: usize = 64;
const MAX_CC_EVENTS: i32 = 20_000;
/// FX nodes in a tree and sends in a list.
const MAX_FX_NODES: usize = 500;
const MAX_SENDS: usize = 200;

/// The track ref `around` names, if any.
fn around_param(params: &Value) -> Result<Option<&str>, (String, String)> {
    match params.get("around") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) if !text.is_empty() => Ok(Some(text)),
        Some(_) => Err(bad("around: a track ref")),
    }
}

fn window_param(params: &Value) -> Result<Option<(f64, f64)>, (String, String)> {
    let Some(window) = params.get("window").filter(|w| !w.is_null()) else { return Ok(None) };
    let number = |name: &str| window.get(name).and_then(Value::as_f64).filter(|n| n.is_finite());
    match (number("from"), number("to")) {
        (Some(from), Some(to)) if to > from && to - from <= MAX_WINDOW => Ok(Some((from, to))),
        (Some(_), Some(_)) => Err(bad(format!("window: to must be after from, and no more than {MAX_WINDOW} apart"))),
        _ => Err(bad("window: {from, to} as numbers")),
    }
}

fn limit_param(params: &Value, default: usize, max: usize) -> Result<usize, (String, String)> {
    match params.get("limit").filter(|l| !l.is_null()) {
        None => Ok(default),
        Some(limit) => match limit.as_u64() {
            Some(n) if n >= 1 => Ok((n as usize).min(max)),
            _ => Err(bad(format!("limit must be a whole number from 1 to {max}"))),
        },
    }
}

impl Reads {
    pub fn focus_body(&self, reaper: &Reaper, params: &Value) -> Answer {
        let kind = params.get("kind").and_then(Value::as_str).ok_or_else(|| bad(format!("kind: one of {}", BODY_KINDS.join(", "))))?;
        let info = reaper.current_project().ok_or_else(|| failed("REAPER has no project open"))?;
        let proj = info.ptr;
        let reference = match params.get("ref") {
            None | Some(Value::Null) => None,
            Some(Value::String(text)) if !text.is_empty() => Some(text.as_str()),
            Some(_) => return Err(bad("ref must be a non-empty string")),
        };
        let cursor = params.get("cursor").and_then(Value::as_str);
        let mut body = match kind {
            "arrange" => self.arrange_body(reaper, proj, params, cursor)?,
            "midi" => self.midi_body(reaper, proj, params, reference, cursor)?,
            "fx" => self.fx_body(reaper, proj, reference)?,
            "routing" => self.routing_body(reaper, proj, reference)?,
            other => return Err(bad(format!("REAPER has no {other:?} body: use one of {}", BODY_KINDS.join(", ")))),
        };
        body["epoch"] = self.epoch().into();
        body["kind"] = kind.into();
        Ok(body)
    }

    // ----- the arrange window -----

    fn arrange_body(&self, reaper: &Reaper, proj: Project, params: &Value, cursor: Option<&str>) -> Answer {
        let beats = |time: f64| reaper.quarter_notes_at(proj, time).unwrap_or(0.0);
        let length = beats(reaper.project_length(proj));
        // In beats: the window asked for, else the whole project (a few bars of room when it is empty).
        let (from, to) = window_param(params)?.unwrap_or((0.0, length.max(16.0)));
        let (from_seconds, to_seconds) = (seconds_at(reaper, proj, from), seconds_at(reaper, proj, to));
        let signature = reaper.time_signature_at(proj, from_seconds);
        let first_page = cursor.is_none();
        let limit = limit_param(params, ARRANGE_TRACKS, ARRANGE_TRACKS)?;
        let total = reaper.track_count(proj) as usize;
        // Where the page starts: a cursor goes on from where the last page stopped; `around` centres the first page on a
        // track; else the top.
        let start = match (cursor, around_param(params)?) {
            (None, Some(around)) => self.around_start(reaper, proj, around, limit, total)?,
            _ => self.position_number(cursor)?,
        };
        let budget = if first_page { ANSWER_BYTES - ARRANGE_EXTRAS_BYTES } else { ANSWER_BYTES };
        let mut pager = Pager::new(limit, budget);
        let mut position = start;
        let mut next = None;
        while position < total {
            if !pager.open() {
                next = Some(position);
                break;
            }
            if let Some(track) = reaper.track_at(proj, position as i32) {
                if !pager.push(arrange_track(reaper, proj, track, from_seconds, to_seconds)) {
                    next = Some(position);
                    break;
                }
            }
            position += 1;
        }
        let mut body = json!({
            "window": {"from": rows::seconds(from), "to": rows::seconds(to)},
            "windowSeconds": {"from": rows::seconds(from_seconds), "to": rows::seconds(to_seconds)},
            "length": rows::seconds(length),
            "lengthSeconds": rows::seconds(reaper.project_length(proj)),
            "beatsPerBar": signature.map(|s| bar_beats(&s)),
            "tracks": pager.rows,
            "trackCount": total,
            // The place of the first track listed among all of them (0-based; its row's `index` is one more).
            "from": start,
        });
        if let Some(next) = next {
            body["nextCursor"] = self.cursor_text(&next.to_string()).into();
        }
        if first_page {
            self.arrange_extras(reaper, proj, from_seconds, to_seconds, &mut body);
        }
        Ok(body)
    }

    /// The place a page of `limit` tracks starts to be centred on the track `around` names: half the page above it, kept
    /// within the first track and the last page.
    fn around_start(&self, reaper: &Reaper, proj: Project, around: &str, limit: usize, total: usize) -> Result<usize, (String, String)> {
        let track = self.parent_track_named(reaper, proj, around, "around")?;
        // IP_TRACKNUMBER is 1-based, and -1 for the master, which sits above them all.
        let place = (reaper.track_value(track, "IP_TRACKNUMBER") as i64 - 1).max(0) as usize;
        Ok(place.saturating_sub(limit / 2).min(total.saturating_sub(limit)))
    }

    /// The parts of the window that aren't tracks: the playhead, the loop, the selection, markers and the tempo map. All
    /// positions are beats; `…Seconds` keys carry the same in seconds.
    fn arrange_extras(&self, reaper: &Reaper, proj: Project, from: f64, to: f64, body: &mut Value) {
        let beats = |time: f64| rows::seconds(reaper.quarter_notes_at(proj, time).unwrap_or(0.0));
        let state = reaper.play_state(proj);
        let cursor = reaper.cursor(proj);
        let playhead = if state.playing || state.recording { reaper.play_position(proj) } else { cursor };
        let (loop_start, loop_end) = reaper.loop_points(proj);
        let (selection_start, selection_end) = reaper.time_selection(proj);
        body["playing"] = (state.playing || state.recording).into();
        body["playhead"] = beats(playhead).into();
        body["playheadSeconds"] = rows::seconds(playhead).into();
        body["cursor"] = beats(cursor).into();
        body["loop"] = json!({"on": reaper.repeat(proj), "start": beats(loop_start), "end": beats(loop_end)});
        body["loopSeconds"] = json!({"start": rows::seconds(loop_start), "end": rows::seconds(loop_end)});
        if selection_end > selection_start {
            body["timeSelection"] = json!({"start": beats(selection_start), "end": beats(selection_end)});
            body["timeSelectionSeconds"] = json!({"start": rows::seconds(selection_start), "end": rows::seconds(selection_end)});
        } else {
            body["timeSelection"] = Value::Null;
        }
        if let Some((view_from, view_to)) = reaper.arrange_view(proj) {
            body["view"] = json!({"from": beats(view_from), "to": beats(view_to)});
        }
        let (mut markers, mut regions) = (Vec::new(), Vec::new());
        for index in 0..reaper.marker_count(proj) {
            let Some(marker) = reaper.marker(proj, index) else { break };
            let end = if marker.is_region { marker.end } else { marker.start };
            if end < from || marker.start > to {
                continue;
            }
            let list = if marker.is_region { &mut regions } else { &mut markers };
            if list.len() < WINDOW_MARKERS {
                let mut row = json!({"ref": if marker.is_region { refs::region_ref(&marker.guid) } else { refs::marker_ref(&marker.guid) }, "number": marker.number, "name": rows::clip(&marker.name), "position": beats(marker.start)});
                if marker.is_region {
                    row["end"] = beats(marker.end).into();
                }
                list.push(row);
            }
        }
        body["markers"] = markers.into();
        body["regions"] = regions.into();
        // The tempo markers in the window, and the one in force when it opens.
        let tempo: Vec<_> = (0..reaper.tempo_marker_count(proj)).filter_map(|i| reaper.tempo_marker(proj, i)).collect();
        let before = tempo.iter().rposition(|m| m.time <= from);
        let shown: Vec<Value> = tempo
            .iter()
            .enumerate()
            .filter(|(i, m)| Some(*i) == before || (m.time > from && m.time <= to))
            .take(WINDOW_TEMPO_MARKERS)
            .map(|(_, m)| json!({"position": beats(m.time), "bpm": rows::seconds(m.bpm), "num": m.num, "den": m.denom, "linear": m.linear}))
            .collect();
        body["tempoMarkers"] = shown.into();
    }

    // ----- a MIDI take as a piano roll -----

    /// The MIDI take a body is about: the ref's, else the MIDI editor's, else the selected item's.
    fn midi_take(&self, reaper: &Reaper, proj: Project, reference: Option<&str>) -> Result<Take, (String, String)> {
        let take = match reference {
            Some(reference) => match Ref::parse(reference).map_err(bad)? {
                Ref::Take(guid) => self.take_by_guid(reaper, proj, &guid)?,
                Ref::Item(guid) => {
                    let item = self.item_by_guid(reaper, proj, &guid)?;
                    reaper.active_take(item).ok_or_else(|| failed("that item has no take"))?
                }
                other => return Err(bad(format!("ref: a take or item ref, not r:{}", other.kind()))),
            },
            None => reaper
                .focused_midi_take()
                .or_else(|| reaper.selected_items_up_to(proj, 1).1.first().and_then(|&item| reaper.active_take(item)))
                .ok_or_else(|| bad("midi: pass the take as ref (no MIDI editor is open and no item is selected)"))?,
        };
        if !reaper.take_is_midi(take) {
            return Err(bad("that take isn't MIDI"));
        }
        Ok(take)
    }

    fn midi_body(&self, reaper: &Reaper, proj: Project, params: &Value, reference: Option<&str>, cursor: Option<&str>) -> Answer {
        let take = self.midi_take(reaper, proj, reference)?;
        let item = reaper.take_item(take).ok_or_else(|| failed("that take's item is gone"))?;
        let item_qn = take_start_qn(reaper, proj, take);
        let position = reaper.item_value(item, "D_POSITION");
        let length_qn = reaper.quarter_notes_at(proj, position + reaper.item_value(item, "D_LENGTH")).unwrap_or(item_qn) - item_qn;
        let (from, to) = window_param(params)?.unwrap_or((0.0, length_qn.max(0.0)));
        let from_ppq = reaper.quarter_notes_to_ppq(take, item_qn + from).unwrap_or(f64::MIN);
        let to_ppq = reaper.quarter_notes_to_ppq(take, item_qn + to).unwrap_or(f64::MAX);
        let (note_count, cc_count, _) = reaper.midi_counts(take).unwrap_or((0, 0, 0));
        let first_page = cursor.is_none();
        let mut pager =
            Pager::new(limit_param(params, DEFAULT_NOTES, MAX_NOTES)?, if first_page { ANSWER_BYTES - 40 * 1024 } else { ANSWER_BYTES });
        let mut index = self.position_number(cursor)?;
        let mut next = None;
        while (index as i32) < note_count {
            if !pager.open() {
                next = Some(index);
                break;
            }
            if let Some(note) = reaper.midi_note(take, index as i32) {
                if note.end_ppq > from_ppq
                    && note.start_ppq < to_ppq
                    && !pager.push(rows::note_row(reaper, take, item_qn, index as i32, &note))
                {
                    next = Some(index);
                    break;
                }
            }
            index += 1;
        }
        let mut body = json!({
            "ref": refs::take_ref(&reaper.take_guid(take)),
            "itemRef": refs::item_ref(&reaper.item_guid(item)),
            "trackRef": reaper.item_track(item).map(|t| refs::track_ref(&rows::track_id(reaper, t))),
            "name": rows::clip(&reaper.take_name(take)),
            "window": {"from": rows::seconds(from), "to": rows::seconds(to)},
            "length": rows::seconds(length_qn),
            "beatsPerBar": reaper.time_signature_at(proj, position).map(|s| bar_beats(&s)),
            "noteCount": note_count,
            "notes": pager.rows,
        });
        if let Some(next) = next {
            body["nextCursor"] = self.cursor_text(&next.to_string()).into();
        }
        if first_page {
            body["ccLanes"] = cc_lanes(reaper, take, item_qn, from_ppq, to_ppq, cc_count.min(MAX_CC_EVENTS));
        }
        Ok(body)
    }

    // ----- an FX chain as a tree -----

    fn fx_body(&self, reaper: &Reaper, proj: Project, reference: Option<&str>) -> Answer {
        let (owner, reference, name, kind) = self.chain_owner(reaper, proj, reference)?;
        let mut budget = MAX_FX_NODES;
        let nodes: Vec<Value> = (0..reaper.fx_count(owner))
            .map(|index| Fx { owner, index })
            .filter_map(|fx| self.fx_node(reaper, proj, fx, &mut budget, false))
            .collect();
        let input: Vec<Value> = match owner {
            FxOwner::Track(track) => (0..reaper.input_fx_count(track))
                .map(|index| Fx { owner, index: INPUT_FX | index })
                .filter_map(|fx| self.fx_node(reaper, proj, fx, &mut budget, false))
                .collect(),
            FxOwner::Take(_) => Vec::new(),
        };
        let mut body = json!({"ref": reference, "owner": kind, "ownerName": name, "nodes": nodes, "inputNodes": input});
        if budget == 0 {
            body["truncated"] = true.into();
        }
        Ok(body)
    }

    /// The owner an FX body is about: a track, the master, a take or an item (its active take); with no ref, the first
    /// selected track.
    fn chain_owner(
        &self,
        reaper: &Reaper,
        proj: Project,
        reference: Option<&str>,
    ) -> Result<(FxOwner, String, String, &'static str), (String, String)> {
        let parsed = match reference {
            Some(reference) => Ref::parse(reference).map_err(bad)?,
            None => match reaper.selected_tracks_up_to(proj, 1).1.first() {
                Some(&track) => Ref::Track(rows::track_id(reaper, track)),
                None => return Err(bad("fx: pass the track or take as ref (no track is selected)")),
            },
        };
        match parsed {
            Ref::Track(id) => {
                let track = self.track_by_id(reaper, proj, &id)?;
                let kind = if id == TrackId::Master { "master" } else { "track" };
                Ok((FxOwner::Track(track), refs::track_ref(&id), rows::clip(&reaper.track_name(track)), kind))
            }
            Ref::Take(guid) => {
                let take = self.take_by_guid(reaper, proj, &guid)?;
                Ok((FxOwner::Take(take), refs::take_ref(&guid), rows::clip(&reaper.take_name(take)), "take"))
            }
            Ref::Item(guid) => {
                let item = self.item_by_guid(reaper, proj, &guid)?;
                let take = reaper.active_take(item).ok_or_else(|| failed("that item has no take"))?;
                Ok((FxOwner::Take(take), refs::take_ref(&reaper.take_guid(take)), rows::clip(&reaper.take_name(take)), "take"))
            }
            other => Err(bad(format!("ref: a track, take or item ref, not r:{}", other.kind()))),
        }
    }

    /// One node of the tree; a container carries its child nodes. `budget` counts the nodes left to list. A node is
    /// `parallel` when it runs beside the one before it.
    fn fx_node(&self, reaper: &Reaper, proj: Project, fx: Fx, budget: &mut usize, _nested: bool) -> Option<Value> {
        if *budget == 0 {
            return None;
        }
        *budget -= 1;
        let guid = reaper.fx_guid(fx);
        self.registry().note_fx(proj, &guid, fx);
        let renamed = reaper.fx_config(fx, "renamed_name").unwrap_or_default();
        let is_container = reaper.fx_config(fx, "container_count").is_some();
        let mut node = json!({
            "ref": refs::fx_ref(&guid),
            "name": rows::clip(&if renamed.is_empty() { reaper.fx_name(fx) } else { renamed }),
            "enabled": reaper.fx_enabled(fx),
            "offline": reaper.fx_offline(fx),
            "preset": rows::clip(&reaper.fx_preset(fx)),
            "plugin": !is_container,
            "container": is_container,
            "parallel": reaper.fx_config(fx, "parallel").and_then(|p| p.trim().parse::<i32>().ok()).unwrap_or(0) != 0,
        });
        if is_container {
            node["childNodes"] = reaper
                .fx_children(fx)
                .into_iter()
                .filter_map(|child| self.fx_node(reaper, proj, child, budget, true))
                .collect::<Vec<_>>()
                .into();
        }
        Some(node)
    }

    // ----- a track's routing -----

    fn routing_body(&self, reaper: &Reaper, proj: Project, reference: Option<&str>) -> Answer {
        let track = match reference {
            Some(reference) => match Ref::parse(reference).map_err(bad)? {
                Ref::Track(id) => self.track_by_id(reaper, proj, &id)?,
                other => return Err(bad(format!("ref: a track ref, not r:{}", other.kind()))),
            },
            None => *reaper
                .selected_tracks_up_to(proj, 1)
                .1
                .first()
                .ok_or_else(|| bad("routing: pass the track as ref (no track is selected)"))?,
        };
        let id = rows::track_id(reaper, track);
        // Sends, then receives, then hardware outputs, each as its `send` row with the other end's name.
        let all: Vec<(i32, i32, rows::SendKey)> = [0, -1, 1]
            .into_iter()
            .flat_map(|category| {
                rows::send_keys(reaper, track, category).into_iter().enumerate().map(move |(index, key)| (category, index as i32, key))
            })
            .collect();
        let total = all.len();
        let routes: Vec<Value> = all
            .iter()
            .take(MAX_SENDS)
            .map(|(category, index, key)| {
                let mut route = rows::send_row(reaper, track, *category, *index, key);
                route["otherName"] = other_name(reaper, track, *category, *index).into();
                route
            })
            .collect();
        let mut body = json!({
            "ref": refs::track_ref(&id),
            "name": rows::clip(&reaper.track_name(track)),
            "routes": routes,
            // The master has nowhere to send.
            "parentSend": id != TrackId::Master && reaper.track_value(track, "B_MAINSEND") != 0.0,
        });
        if id != TrackId::Master {
            body["parentSendOffset"] = (reaper.track_value(track, "C_MAINSEND_OFFS") as i64).into();
            body["parentSendChannels"] = (reaper.track_value(track, "C_MAINSEND_NCH") as i64).into();
        }
        if total > MAX_SENDS {
            body["routeCount"] = total.into();
            body["truncated"] = true.into();
        }
        Ok(body)
    }
}

/// The name of the other end of a route: the track's, or the hardware output's channels.
fn other_name(reaper: &Reaper, track: Track, category: i32, index: i32) -> String {
    match reaper.send_peer(track, category, index) {
        Some(other) => rows::clip(&reaper.track_name(other)),
        None => {
            let destination = reaper.send_value(track, category, index, "I_DSTCHAN") as i64;
            let first = (destination & 1023) + 1;
            if destination & 1024 != 0 {
                format!("Out {first} (mono)")
            } else {
                format!("Out {first}/{}", first + 1)
            }
        }
    }
}

/// Quarter notes in a bar of this time signature.
fn bar_beats(signature: &crate::reaper::TimeSignature) -> f64 {
    rows::seconds(signature.num as f64 * 4.0 / signature.denom.max(1) as f64)
}

/// The time of a position in quarter notes.
fn seconds_at(reaper: &Reaper, proj: Project, beats: f64) -> f64 {
    reaper.time_at_quarter_notes(proj, beats).unwrap_or(0.0)
}

/// A track in the arrange window: its place in the folders and the items that touch it. `from` and `to` are seconds; the
/// items' places are beats.
fn arrange_track(reaper: &Reaper, proj: Project, track: Track, from: f64, to: f64) -> Value {
    let id = rows::track_id(reaper, track);
    let mut items = Vec::new();
    let mut omitted = 0usize;
    for index in 0..reaper.track_item_count(track) {
        let Some(item) = reaper.track_item(track, index) else { continue };
        let position = reaper.item_value(item, "D_POSITION");
        let length = reaper.item_value(item, "D_LENGTH");
        if position >= to || position + length <= from {
            continue;
        }
        if items.len() >= ITEMS_PER_TRACK {
            omitted += 1;
        } else {
            items.push(arrange_item(reaper, proj, item, position, length));
        }
    }
    let mut row = json!({
        "ref": refs::track_ref(&id),
        "index": reaper.track_value(track, "IP_TRACKNUMBER") as i64,
        "name": rows::clip(&reaper.track_name(track)),
        "depth": reaper.track_depth(track),
        "folder": reaper.track_value(track, "I_FOLDERDEPTH") as i64,
        "color": reaper.track_color(track).and_then(|c| reaper.color_hex(c)),
        "mute": reaper.track_value(track, "B_MUTE") != 0.0,
        "solo": reaper.track_value(track, "I_SOLO") != 0.0,
        "arm": reaper.track_value(track, "I_RECARM") != 0.0,
        "selected": reaper.track_value(track, "I_SELECTED") != 0.0,
        "items": items,
    });
    if omitted > 0 {
        row["itemsOmitted"] = omitted.into();
    }
    row
}

/// An item as a bar: where it starts and how long it is, in beats.
fn arrange_item(reaper: &Reaper, proj: Project, item: Item, position: f64, length: f64) -> Value {
    let take = reaper.active_take(item);
    let start = reaper.quarter_notes_at(proj, position).unwrap_or(0.0);
    let end = reaper.quarter_notes_at(proj, position + length).unwrap_or(start);
    let custom = reaper.item_value(item, "I_CUSTOMCOLOR") as i32;
    json!({
        "ref": refs::item_ref(&reaper.item_guid(item)),
        "position": rows::seconds(start),
        "length": rows::seconds(end - start),
        "name": take.map(|t| rows::clip(&reaper.take_name(t))).unwrap_or_default(),
        "source": rows::source_word(reaper, take),
        "color": (custom & 0x100_0000 != 0).then(|| reaper.color_hex(custom)).flatten(),
        "muted": reaper.item_value(item, "B_MUTE") != 0.0,
        "lane": reaper.item_value(item, "I_FIXEDLANE") as i64,
        "selected": reaper.item_value(item, "B_UISEL") != 0.0,
    })
}

/// A lane's name: the controller number, or what the other kinds of event are called.
fn lane_key(kind: i32, data1: i32) -> Value {
    match kind & 0xF0 {
        0xB0 => json!(data1),
        0xE0 => json!("pitchbend"),
        0xC0 => json!("program"),
        0xD0 => json!("pressure"),
        0xA0 => json!("polypressure"),
        other => json!(format!("0x{other:02x}")),
    }
}

/// A lane's value: the controller's, the 14 bits of a pitch bend, or the program or pressure byte.
fn lane_value(kind: i32, data1: i32, data2: i32) -> i32 {
    match kind & 0xF0 {
        0xE0 => data1 | (data2 << 7),
        0xB0 | 0xA0 => data2,
        _ => data1,
    }
}

/// The CC, pitch bend, program and pressure events in the window as lanes of at most `LANE_POINTS` points, `[beat, value]`
/// with the beat in quarter notes from the start of the item.
fn cc_lanes(reaper: &Reaper, take: Take, item_qn: f64, from_ppq: f64, to_ppq: f64, count: i32) -> Value {
    let mut lanes: BTreeMap<String, (Value, Vec<(f64, i32)>)> = BTreeMap::new();
    for index in 0..count {
        let Some(cc) = reaper.midi_cc(take, index) else { continue };
        if cc.ppq < from_ppq || cc.ppq > to_ppq {
            continue;
        }
        let key = lane_key(cc.kind, cc.data1);
        let beat = reaper.ppq_to_quarter_notes(take, cc.ppq).unwrap_or(0.0) - item_qn;
        lanes.entry(key.to_string()).or_insert_with(|| (key, Vec::new())).1.push((beat, lane_value(cc.kind, cc.data1, cc.data2)));
    }
    lanes
        .into_values()
        .map(|(key, points)| {
            let total = points.len();
            let shown: Vec<Value> =
                thin(&points).into_iter().map(|(beat, value)| json!([(beat * 10_000.0).round() / 10_000.0, value])).collect();
            json!({"cc": key, "total": total, "points": shown})
        })
        .collect::<Vec<_>>()
        .into()
}

/// At most `LANE_POINTS` of the points, spread evenly, the first and last among them.
fn thin(points: &[(f64, i32)]) -> Vec<(f64, i32)> {
    if points.len() <= LANE_POINTS {
        return points.to_vec();
    }
    (0..LANE_POINTS).map(|i| points[i * (points.len() - 1) / (LANE_POINTS - 1)]).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reaper::fake::{self, FakeCc, FakeFx, FakeItem, FakeNote, FakeSend, FakeTake, FakeTempo, FakeTrack};

    fn body(reads: &Reads, params: Value) -> Answer {
        let api = fake::api();
        reads.focus_body(&Reaper::new(&api), &params)
    }

    fn lab() -> Reads {
        fake::lab_project();
        Reads::default()
    }

    fn refs_of(rows: &Value) -> Vec<&str> {
        rows.as_array().unwrap().iter().map(|r| r["ref"].as_str().unwrap()).collect()
    }

    #[test]
    fn thinning_keeps_the_ends_and_spreads_the_rest() {
        let points: Vec<(f64, i32)> = (0..1000).map(|i| (i as f64, i)).collect();
        let thinned = thin(&points);
        assert_eq!(thinned.len(), LANE_POINTS);
        assert_eq!((thinned[0], thinned[LANE_POINTS - 1]), ((0.0, 0), (999.0, 999)));
        assert!(thinned.windows(2).all(|w| w[0].0 < w[1].0));
        assert_eq!(thin(&points[..10]).len(), 10);
    }

    #[test]
    fn lane_values_follow_the_kind_of_event() {
        assert_eq!(lane_value(0xB0, 7, 100), 100);
        assert_eq!(lane_value(0xE0, 0, 64), 8192);
        assert_eq!(lane_value(0xC0, 5, 0), 5);
        assert_eq!(lane_key(0xB0, 74), json!(74));
        assert_eq!(lane_key(0xE0, 0), json!("pitchbend"));
    }

    #[test]
    fn the_arrange_window_has_the_tracks_in_their_folders_and_the_items_that_touch_it_in_beats() {
        let reads = lab();
        // 120 bpm: a second is two beats.
        fake::with(|s| {
            s.markers.push(fake::FakeMarker {
                guid: "{MARK-FAR}".into(),
                is_region: false,
                start: 40.0,
                end: 40.0,
                name: "Far".into(),
                number: 2,
                color: 0,
            });
            s.tempo_markers = vec![
                FakeTempo { time: 0.0, bpm: 120.0, num: 4, denom: 4 },
                FakeTempo { time: 6.0, bpm: 90.0, num: 3, denom: 4 },
                FakeTempo { time: 50.0, bpm: 70.0, num: 4, denom: 4 },
            ];
            s.tracks[1].items[1].selected = true;
            s.cursor = 3.0;
            s.loop_range = (4.0, 8.0);
            s.repeat = true;
        });
        let body = body(&reads, json!({"kind": "arrange", "window": {"from": 6.0, "to": 20.0}})).unwrap();
        assert_eq!(body["kind"], "arrange");
        assert_eq!(body["epoch"], reads.epoch());
        assert_eq!((&body["window"], &body["windowSeconds"]), (&json!({"from": 6.0, "to": 20.0}), &json!({"from": 3.0, "to": 10.0})));
        assert_eq!((&body["length"], &body["lengthSeconds"], &body["beatsPerBar"]), (&json!(128.0), &json!(64.0), &json!(4.0)));
        let tracks = body["tracks"].as_array().unwrap();
        assert_eq!(refs_of(&body["tracks"]), ["r:track:{AAAA}", "r:track:{BBBB}", "r:track:{CCCC}", "r:track:{DDDD}"]);
        assert_eq!((&tracks[0]["depth"], &tracks[0]["folder"], &tracks[0]["color"]), (&json!(0), &json!(1), &json!("#ff0000")));
        assert_eq!((&tracks[1]["depth"], &tracks[2]["depth"], &tracks[2]["folder"]), (&json!(1), &json!(1), &json!(-1)));
        // Kick's first item ends at 2 s, before the window; the second (2 s to 4 s) touches it: beats 4 to 8.
        assert_eq!(refs_of(&tracks[1]["items"]), ["r:item:{ITEM-K2}"]);
        let kick = &tracks[1]["items"][0];
        assert_eq!(
            (&kick["position"], &kick["length"], &kick["selected"], &kick["source"], &kick["name"]),
            (&json!(4.0), &json!(4.0), &json!(true), &json!("wav"), &json!("kick.wav"))
        );
        // Snare's MIDI item (8 s to 12 s) starts inside the window: beats 16 to 24.
        assert_eq!(
            (&tracks[2]["items"][0]["source"], &tracks[2]["items"][0]["position"], &tracks[2]["items"][0]["length"]),
            (&json!("mid"), &json!(16.0), &json!(8.0))
        );
        assert_eq!(
            (&body["playing"], &body["playhead"], &body["playheadSeconds"], &body["cursor"]),
            (&json!(false), &json!(6.0), &json!(3.0), &json!(6.0))
        );
        assert_eq!(
            (&body["loop"], &body["loopSeconds"]),
            (&json!({"on": true, "start": 8.0, "end": 16.0}), &json!({"start": 4.0, "end": 8.0}))
        );
        assert_eq!(body["timeSelection"], Value::Null);
        assert_eq!(body["view"], json!({"from": 0.0, "to": 60.0}));
        // The marker at 4 s and the region from 8 s, in beats; the marker at 40 s is off the window.
        assert_eq!((refs_of(&body["markers"]), refs_of(&body["regions"])), (vec!["r:marker:{MARK-1}"], vec!["r:region:{REG-1}"]));
        assert_eq!(body["markers"][0]["position"], 8.0);
        assert_eq!((&body["regions"][0]["position"], &body["regions"][0]["end"]), (&json!(16.0), &json!(32.0)));
        // The tempo in force when the window opens, and the change inside it; not the one far past it.
        let tempo: Vec<f64> = body["tempoMarkers"].as_array().unwrap().iter().map(|m| m["bpm"].as_f64().unwrap()).collect();
        assert_eq!(tempo, [120.0, 90.0]);
        assert_eq!(
            (&body["tempoMarkers"][1]["position"], &body["tempoMarkers"][1]["num"], &body["tempoMarkers"][1]["den"]),
            (&json!(12.0), &json!(3), &json!(4))
        );
    }

    #[test]
    fn a_selected_time_range_is_in_beats_with_the_seconds_beside_it() {
        let reads = lab();
        fake::with(|s| s.time_selection = (2.0, 5.0));
        let body = body(&reads, json!({"kind": "arrange"})).unwrap();
        assert_eq!(
            (&body["timeSelection"], &body["timeSelectionSeconds"]),
            (&json!({"start": 4.0, "end": 10.0}), &json!({"start": 2.0, "end": 5.0}))
        );
    }

    #[test]
    fn without_a_window_the_whole_project_is_shown() {
        let reads = lab();
        assert_eq!(body(&reads, json!({"kind": "arrange"})).unwrap()["window"], json!({"from": 0.0, "to": 128.0}));
        // An empty project still gets a few bars to draw.
        fake::with(|s| {
            s.length = 0.0;
            s.tracks.clear();
        });
        assert_eq!(body(&reads, json!({"kind": "arrange"})).unwrap()["window"], json!({"from": 0.0, "to": 16.0}));
    }

    #[test]
    fn a_bad_window_is_refused() {
        let reads = lab();
        for window in [
            json!({"from": 5.0, "to": 5.0}),
            json!({"from": 9.0, "to": 2.0}),
            json!({"from": 0}),
            json!("now"),
            json!({"from": 0.0, "to": 1e9}),
        ] {
            assert_eq!(body(&reads, json!({"kind": "arrange", "window": window})).unwrap_err().0, "bad-params");
        }
    }

    #[test]
    fn a_long_arrange_pages_by_track_and_keeps_the_markers_for_the_first_page() {
        let reads = lab();
        fake::with(|s| s.tracks = (0..620).map(|i| FakeTrack::new(&format!("{{T{i}}}"), "t")).collect());
        let first = body(&reads, json!({"kind": "arrange", "window": {"from": 0.0, "to": 20.0}})).unwrap();
        assert_eq!(first["tracks"].as_array().unwrap().len(), ARRANGE_TRACKS);
        assert!(first["markers"].is_array());
        let second = body(&reads, json!({"kind": "arrange", "window": {"from": 0.0, "to": 20.0}, "cursor": first["nextCursor"]})).unwrap();
        assert_eq!(second["tracks"].as_array().unwrap().len(), 120);
        assert_eq!(second["tracks"][0]["ref"], "r:track:{T500}");
        assert_eq!(second.get("nextCursor"), None);
        assert_eq!(second.get("markers"), None);
    }

    fn tracks(n: usize) {
        fake::with(|s| s.tracks = (0..n).map(|i| FakeTrack::new(&format!("{{T{i}}}"), &format!("Track {i}"))).collect());
    }

    fn first_and_last(body: &Value) -> (String, String) {
        let tracks = body["tracks"].as_array().unwrap();
        (tracks[0]["ref"].as_str().unwrap().to_string(), tracks[tracks.len() - 1]["ref"].as_str().unwrap().to_string())
    }

    #[test]
    fn arrange_centres_its_page_on_a_track_and_says_where_the_page_starts() {
        let reads = lab();
        tracks(200);
        let page = |reads: &Reads, params: Value| {
            let mut all = json!({"kind": "arrange", "window": {"from": 0.0, "to": 8.0}});
            all.as_object_mut().unwrap().extend(params.as_object().unwrap().clone());
            body(reads, all)
        };
        // Track 150 (position 149) with 24 to a page: 12 above it, 11 below.
        let middle = page(&reads, json!({"limit": 24, "around": "r:track:{T149}"})).unwrap();
        assert_eq!(
            (middle["from"].clone(), middle["trackCount"].clone(), middle["tracks"].as_array().unwrap().len()),
            (json!(137), json!(200), 24)
        );
        assert_eq!(first_and_last(&middle), ("r:track:{T137}".to_string(), "r:track:{T160}".to_string()));
        assert_eq!(middle["tracks"][0]["index"], 138);
        assert!(middle["nextCursor"].is_string());
        // The page goes on from there with its cursor, and says where that one starts.
        let next = page(&reads, json!({"limit": 24, "cursor": middle["nextCursor"]})).unwrap();
        assert_eq!((next["from"].clone(), first_and_last(&next).0), (json!(161), "r:track:{T161}".to_string()));
        // Near the top the page starts at the first track; near the end it is the last page.
        assert_eq!(page(&reads, json!({"limit": 24, "around": "r:track:{T2}"})).unwrap()["from"], 0);
        let end = page(&reads, json!({"limit": 24, "around": "r:track:{T198}"})).unwrap();
        assert_eq!((end["from"].clone(), first_and_last(&end).1), (json!(176), "r:track:{T199}".to_string()));
        assert_eq!(end.get("nextCursor"), None);
        // An odd page centres as well as it can; the master sits above them all; more room than tracks starts at the top.
        assert_eq!(page(&reads, json!({"limit": 5, "around": "r:track:{T50}"})).unwrap()["from"], 48);
        assert_eq!(page(&reads, json!({"limit": 24, "around": "r:track:master"})).unwrap()["from"], 0);
        assert_eq!(page(&reads, json!({"around": "r:track:{T149}"})).unwrap()["from"], 0);
        assert_eq!(page(&reads, json!({"limit": 24})).unwrap()["from"], 0);
        // A cursor goes on from where it was: around is for the first page.
        assert_eq!(page(&reads, json!({"limit": 24, "around": "r:track:{T10}", "cursor": middle["nextCursor"]})).unwrap()["from"], 161);
        // The first page still carries the markers and the rest of the window.
        assert!(middle["markers"].is_array() && middle["playhead"].is_number());
    }

    #[test]
    fn arrange_around_a_track_that_is_gone_or_not_a_track_is_refused() {
        let reads = lab();
        let ask = |around: Value| body(&reads, json!({"kind": "arrange", "limit": 2, "around": around}));
        assert_eq!(ask(json!("r:track:{GONE}")).unwrap_err().0, "failed");
        assert_eq!(ask(json!("r:fx:{FX-EQ}")).unwrap_err().0, "bad-params");
        assert_eq!(ask(json!("track:3")).unwrap_err().0, "bad-params");
        assert_eq!(ask(json!(7)).unwrap_err().0, "bad-params");
        assert_eq!(ask(json!("")).unwrap_err().0, "bad-params");
        assert!(ask(json!("r:track:{BBBB}")).is_ok());
    }

    #[test]
    fn a_track_with_too_many_items_lists_some_and_counts_the_rest() {
        let reads = lab();
        fake::with(|s| {
            s.tracks[0].items = (0..450)
                .map(|i| FakeItem::new(&format!("{{I{i}}}"), i as f64 * 0.01, 1.0, vec![FakeTake::wave(&format!("{{K{i}}}"), "x")]))
                .collect();
        });
        let body = body(&reads, json!({"kind": "arrange", "window": {"from": 0.0, "to": 20.0}})).unwrap();
        assert_eq!(body["tracks"][0]["items"].as_array().unwrap().len(), ITEMS_PER_TRACK);
        assert_eq!(body["tracks"][0]["itemsOmitted"], 50);
    }

    #[test]
    fn an_arrange_page_never_passes_the_frame_bound() {
        let reads = lab();
        let long = "n".repeat(900);
        fake::with(|s| {
            s.tracks = (0..400)
                .map(|t| {
                    let mut track = FakeTrack::new(&format!("{{T{t}}}"), &long);
                    track.items = (0..30)
                        .map(|i| {
                            FakeItem::new(&format!("{{I{t}-{i}}}"), i as f64, 1.0, vec![FakeTake::wave(&format!("{{K{t}-{i}}}"), &long)])
                        })
                        .collect();
                    track
                })
                .collect();
        });
        let params = json!({"kind": "arrange", "window": {"from": 0.0, "to": 200.0}});
        let mut page = body(&reads, params.clone()).unwrap();
        let mut seen = 0;
        loop {
            assert!(serde_json::to_string(&page).unwrap().len() <= 256 * 1024);
            seen += page["tracks"].as_array().unwrap().len();
            let Some(cursor) = page.get("nextCursor").cloned() else { break };
            let mut again = params.clone();
            again["cursor"] = cursor;
            page = body(&reads, again).unwrap();
        }
        assert_eq!(seen, 400);
    }

    #[test]
    fn a_midi_take_is_notes_in_the_window_and_cc_lanes() {
        let reads = lab();
        // The snare take is 4 s long: 8 quarter notes at 120 bpm.
        let body = body(&reads, json!({"kind": "midi", "ref": "r:take:{TAKE-S1}"})).unwrap();
        assert_eq!(body["kind"], "midi");
        assert_eq!(
            (&body["ref"], &body["itemRef"], &body["trackRef"], &body["name"]),
            (&json!("r:take:{TAKE-S1}"), &json!("r:item:{ITEM-S1}"), &json!("r:track:{CCCC}"), &json!("snare.mid"))
        );
        assert_eq!(
            (&body["window"], &body["length"], &body["noteCount"], &body["beatsPerBar"]),
            (&json!({"from": 0.0, "to": 8.0}), &json!(8.0), &json!(2), &json!(4.0))
        );
        assert_eq!(body["notes"].as_array().unwrap().len(), 2);
        assert_eq!(body["notes"][1]["pitch"], 40);
        assert_eq!(body["ccLanes"], json!([{"cc": 7, "total": 2, "points": [[0.0, 100], [0.5, 64]]}]));
        // A window cuts the notes; a note touching it counts.
        let cut =
            super::tests::body(&reads, json!({"kind": "midi", "ref": "r:take:{TAKE-S1}", "window": {"from": 0.9, "to": 3.0}})).unwrap();
        assert_eq!(cut["notes"].as_array().unwrap().len(), 1);
        assert_eq!(cut["notes"][0]["pitch"], 40);
        assert_eq!(cut["ccLanes"], json!([]));
    }

    #[test]
    fn the_midi_body_defaults_to_the_editor_then_the_selected_item() {
        let reads = lab();
        assert!(body(&reads, json!({"kind": "midi"})).unwrap_err().1.contains("pass the take as ref"));
        fake::with(|s| s.tracks[2].items[0].selected = true);
        assert_eq!(body(&reads, json!({"kind": "midi"})).unwrap()["ref"], "r:take:{TAKE-S1}");
        // The MIDI editor's take wins over the selection.
        fake::with(|s| {
            s.tracks[2].items.push(FakeItem::new("{ITEM-S2}", 16.0, 2.0, vec![FakeTake::midi("{TAKE-S2}", "fill.mid")]));
            s.midi_editor = Some((2, 1, 0));
        });
        assert_eq!(body(&reads, json!({"kind": "midi"})).unwrap()["ref"], "r:take:{TAKE-S2}");
        // An item ref works for its active take.
        assert_eq!(body(&reads, json!({"kind": "midi", "ref": "r:item:{ITEM-S1}"})).unwrap()["ref"], "r:take:{TAKE-S1}");
        assert!(body(&reads, json!({"kind": "midi", "ref": "r:take:{TAKE-K1}"})).unwrap_err().1.contains("isn't MIDI"));
        assert_eq!(body(&reads, json!({"kind": "midi", "ref": "r:track:{AAAA}"})).unwrap_err().0, "bad-params");
    }

    #[test]
    fn a_big_take_pages_its_notes_and_thins_its_lanes() {
        let reads = lab();
        fake::with(|s| {
            let take = &mut s.tracks[2].items[0].takes[0];
            take.notes = (0..1200)
                .map(|i| FakeNote {
                    start_ppq: i as f64 * 3.0,
                    end_ppq: i as f64 * 3.0 + 2.0,
                    channel: 0,
                    pitch: 36 + (i % 12),
                    velocity: 90,
                    selected: false,
                })
                .collect();
            take.ccs = (0..1000).map(|i| FakeCc { ppq: i as f64 * 3.0, kind: 0xB0, channel: 0, data1: 1, data2: i % 128 }).collect();
        });
        let first = body(&reads, json!({"kind": "midi", "ref": "r:take:{TAKE-S1}", "limit": 500})).unwrap();
        assert_eq!(first["notes"].as_array().unwrap().len(), 500);
        assert_eq!(first["noteCount"], 1200);
        assert_eq!((&first["ccLanes"][0]["total"], first["ccLanes"][0]["points"].as_array().unwrap().len()), (&json!(1000), LANE_POINTS));
        let second = body(&reads, json!({"kind": "midi", "ref": "r:take:{TAKE-S1}", "limit": 500, "cursor": first["nextCursor"]})).unwrap();
        assert_eq!(second["notes"][0]["index"], 500);
        assert_eq!(second.get("ccLanes"), None);
        let third = body(&reads, json!({"kind": "midi", "ref": "r:take:{TAKE-S1}", "limit": 500, "cursor": second["nextCursor"]})).unwrap();
        assert_eq!(third["notes"].as_array().unwrap().len(), 200);
        assert_eq!(third.get("nextCursor"), None);
    }

    #[test]
    fn the_cc_lanes_cover_every_kind_of_event() {
        let reads = lab();
        fake::with(|s| {
            s.tracks[2].items[0].takes[0].ccs = vec![
                FakeCc { ppq: 0.0, kind: 0xE0, channel: 0, data1: 0, data2: 64 },
                FakeCc { ppq: 0.0, kind: 0xC0, channel: 0, data1: 12, data2: 0 },
                FakeCc { ppq: 960.0, kind: 0xD0, channel: 0, data1: 90, data2: 0 },
                FakeCc { ppq: 960.0, kind: 0xB0, channel: 0, data1: 74, data2: 33 },
            ];
        });
        let lanes = body(&reads, json!({"kind": "midi", "ref": "r:take:{TAKE-S1}"})).unwrap()["ccLanes"].clone();
        let named: Vec<String> = lanes.as_array().unwrap().iter().map(|l| l["cc"].to_string()).collect();
        assert_eq!(named.len(), 4);
        for key in ["74", "\"pitchbend\"", "\"program\"", "\"pressure\""] {
            assert!(named.iter().any(|n| n == key), "{key} in {named:?}");
        }
        let bend = lanes.as_array().unwrap().iter().find(|l| l["cc"] == "pitchbend").unwrap();
        assert_eq!(bend["points"], json!([[0.0, 8192]]));
    }

    #[test]
    fn an_fx_chain_is_a_tree_with_containers_and_parallel_flags() {
        let reads = lab();
        fake::with(|s| {
            s.tracks[0].fx[1].children.as_mut().unwrap()[0].parallel = 1;
            s.tracks[0].input_fx.push(FakeFx::new("{FX-IN}", "JS: Monitor"));
            s.tracks[0].fx[0].enabled = false;
        });
        let body = body(&reads, json!({"kind": "fx", "ref": "r:track:{AAAA}"})).unwrap();
        assert_eq!((&body["ref"], &body["owner"], &body["ownerName"]), (&json!("r:track:{AAAA}"), &json!("track"), &json!("Drums")));
        let nodes = &body["nodes"];
        assert_eq!(refs_of(nodes), ["r:fx:{FX-EQ}", "r:fx:{FX-C}"]);
        assert_eq!(
            (&nodes[0]["enabled"], &nodes[0]["preset"], &nodes[0]["plugin"], &nodes[0]["container"], &nodes[0]["parallel"]),
            (&json!(false), &json!("Warm"), &json!(true), &json!(false), &json!(false))
        );
        assert_eq!(nodes[0].get("childNodes"), None);
        let outer = &nodes[1];
        assert_eq!((&outer["container"], &outer["plugin"]), (&json!(true), &json!(false)));
        assert_eq!(refs_of(&outer["childNodes"]), ["r:fx:{FX-COMP}", "r:fx:{FX-C2}"]);
        assert_eq!(outer["childNodes"][0]["parallel"], true);
        assert_eq!(outer["childNodes"][1]["parallel"], false);
        assert_eq!(refs_of(&outer["childNodes"][1]["childNodes"]), ["r:fx:{FX-DLY}"]);
        assert_eq!(refs_of(&body["inputNodes"]), ["r:fx:{FX-IN}"]);
    }

    #[test]
    fn the_fx_body_covers_takes_items_the_master_and_the_selected_track() {
        let reads = lab();
        fake::with(|s| {
            s.tracks[1].items[1].takes[0].fx.push(FakeFx::new("{FX-TAKE}", "JS: Volume"));
            s.master.fx.push(FakeFx::new("{FX-LIM}", "VST: ReaLimit"));
        });
        let take = body(&reads, json!({"kind": "fx", "ref": "r:take:{TAKE-K2}"})).unwrap();
        assert_eq!((&take["owner"], refs_of(&take["nodes"])), (&json!("take"), vec!["r:fx:{FX-TAKE}"]));
        assert_eq!(body(&reads, json!({"kind": "fx", "ref": "r:item:{ITEM-K2}"})).unwrap()["ref"], "r:take:{TAKE-K2}");
        let master = body(&reads, json!({"kind": "fx", "ref": "r:track:master"})).unwrap();
        assert_eq!((&master["owner"], refs_of(&master["nodes"])), (&json!("master"), vec!["r:fx:{FX-LIM}"]));
        assert!(body(&reads, json!({"kind": "fx"})).unwrap_err().1.contains("no track is selected"));
        fake::select_tracks(&[3]);
        assert_eq!(body(&reads, json!({"kind": "fx"})).unwrap()["ref"], "r:track:{DDDD}");
    }

    #[test]
    fn a_huge_chain_is_cut_and_says_so() {
        let reads = lab();
        fake::with(|s| s.tracks[3].fx = (0..600).map(|i| FakeFx::new(&format!("{{F{i}}}"), "fx")).collect());
        let body = body(&reads, json!({"kind": "fx", "ref": "r:track:{DDDD}"})).unwrap();
        assert_eq!(body["nodes"].as_array().unwrap().len(), MAX_FX_NODES);
        assert_eq!(body["truncated"], true);
    }

    #[test]
    fn routing_lists_sends_receives_hardware_and_the_parent_send_as_routes_with_the_other_end_named() {
        let reads = lab();
        fake::with(|s| s.tracks[3].main_send = false);
        let reverb = body(&reads, json!({"kind": "routing", "ref": "r:track:{DDDD}"})).unwrap();
        assert_eq!((&reverb["ref"], &reverb["name"], &reverb["parentSend"]), (&json!("r:track:{DDDD}"), &json!("Reverb"), &json!(false)));
        assert_eq!(refs_of(&reverb["routes"]), ["r:send:{DDDD}:-1:{AAAA}", "r:send:{DDDD}:1:hw0"]);
        let routes = reverb["routes"].as_array().unwrap();
        assert_eq!(
            (&routes[0]["direction"], &routes[0]["destRef"], &routes[0]["otherName"]),
            (&json!("receive"), &json!("r:track:{AAAA}"), &json!("Drums"))
        );
        assert_eq!(
            (&routes[1]["direction"], &routes[1]["destRef"], &routes[1]["otherName"]),
            (&json!("hardware"), &Value::Null, &json!("Out 3/4"))
        );
        let drums = body(&reads, json!({"kind": "routing", "ref": "r:track:{AAAA}"})).unwrap();
        assert_eq!(refs_of(&drums["routes"]), ["r:send:{AAAA}:0:{DDDD}"]);
        assert_eq!((&drums["routes"][0]["otherName"], &drums["parentSend"]), (&json!("Reverb"), &json!(true)));
        // The master has nowhere to send; with no ref the selected track is the one.
        assert_eq!(body(&reads, json!({"kind": "routing", "ref": "r:track:master"})).unwrap()["parentSend"], false);
        fake::select_tracks(&[0]);
        assert_eq!(body(&reads, json!({"kind": "routing"})).unwrap()["ref"], "r:track:{AAAA}");
        assert_eq!(body(&reads, json!({"kind": "routing", "ref": "r:fx:{FX-EQ}"})).unwrap_err().0, "bad-params");
    }

    #[test]
    fn a_mono_hardware_output_is_named_by_its_one_channel() {
        let reads = lab();
        fake::with(|s| s.tracks[3].hw_outs = vec![FakeSend { dst_chan: 1 | 1024, ..FakeSend::to(0) }]);
        let body = body(&reads, json!({"kind": "routing", "ref": "r:track:{DDDD}"})).unwrap();
        assert_eq!(body["routes"][1]["otherName"], "Out 2 (mono)");
    }

    #[test]
    fn a_track_with_hundreds_of_sends_lists_the_first_and_counts_them() {
        let reads = lab();
        fake::with(|s| s.tracks[1].sends = (0..260).map(|_| FakeSend::to(3)).collect());
        let body = body(&reads, json!({"kind": "routing", "ref": "r:track:{BBBB}"})).unwrap();
        assert_eq!(body["routes"].as_array().unwrap().len(), MAX_SENDS);
        assert_eq!((&body["routeCount"], &body["truncated"]), (&json!(260), &json!(true)));
        assert!(serde_json::to_string(&body).unwrap().len() < 256 * 1024);
    }

    #[test]
    fn an_unknown_kind_or_no_project_is_refused() {
        let reads = lab();
        assert!(body(&reads, json!({})).unwrap_err().1.contains("kind"));
        assert!(body(&reads, json!({"kind": "mixer"})).unwrap_err().1.contains("no \"mixer\" body"));
        assert_eq!(body(&reads, json!({"kind": "fx", "ref": 7})).unwrap_err().0, "bad-params");
        fake::with(|s| s.project = None);
        assert_eq!(body(&reads, json!({"kind": "arrange"})).unwrap_err().0, "failed");
    }
}
