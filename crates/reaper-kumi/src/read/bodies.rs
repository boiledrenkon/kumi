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
        let (from, to) =
            window_param(params)?.or_else(|| reaper.arrange_view(proj)).unwrap_or((0.0, reaper.project_length(proj).max(30.0)));
        let first_page = cursor.is_none();
        let start = self.position_number(cursor)?;
        let limit = limit_param(params, ARRANGE_TRACKS, ARRANGE_TRACKS)?;
        let budget = if first_page { ANSWER_BYTES - ARRANGE_EXTRAS_BYTES } else { ANSWER_BYTES };
        let mut pager = Pager::new(limit, budget);
        let total = reaper.track_count(proj) as usize;
        let mut position = start;
        let mut next = None;
        while position < total {
            if !pager.open() {
                next = Some(position);
                break;
            }
            if let Some(track) = reaper.track_at(proj, position as i32) {
                if !pager.push(arrange_track(reaper, track, from, to)) {
                    next = Some(position);
                    break;
                }
            }
            position += 1;
        }
        let mut body = json!({"window": {"from": rows::seconds(from), "to": rows::seconds(to)}, "tracks": pager.rows, "trackCount": total});
        if let Some(next) = next {
            body["nextCursor"] = self.cursor_text(&next.to_string()).into();
        }
        if first_page {
            self.arrange_extras(reaper, proj, from, to, &mut body);
        }
        Ok(body)
    }

    /// The parts of the window that aren't tracks: the playhead, the loop, the selection, markers and the tempo map.
    fn arrange_extras(&self, reaper: &Reaper, proj: Project, from: f64, to: f64, body: &mut Value) {
        let state = reaper.play_state(proj);
        let cursor = reaper.cursor(proj);
        let (loop_start, loop_end) = reaper.loop_points(proj);
        body["playhead"] = rows::seconds(if state.playing || state.recording { reaper.play_position(proj) } else { cursor }).into();
        body["cursor"] = rows::seconds(cursor).into();
        body["loop"] = json!({"on": reaper.repeat(proj), "start": rows::seconds(loop_start), "end": rows::seconds(loop_end)});
        body["timeSelection"] = rows::time_selection(reaper, proj);
        let (mut markers, mut regions) = (Vec::new(), Vec::new());
        for index in 0..reaper.marker_count(proj) {
            let Some(marker) = reaper.marker(proj, index) else { break };
            let (start, end) = if marker.is_region { (marker.start, marker.end) } else { (marker.start, marker.start) };
            if end < from || start > to {
                continue;
            }
            let list = if marker.is_region { &mut regions } else { &mut markers };
            if list.len() < WINDOW_MARKERS {
                list.push(rows::marker_row(reaper, &marker));
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
            .map(|(_, m)| rows::tempo_marker_row(m))
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
        let chain: Vec<Value> = (0..reaper.fx_count(owner))
            .map(|index| Fx { owner, index })
            .filter_map(|fx| self.fx_node(reaper, proj, fx, &mut budget))
            .collect();
        let input: Vec<Value> = match owner {
            FxOwner::Track(track) => (0..reaper.input_fx_count(track))
                .map(|index| Fx { owner, index: INPUT_FX | index })
                .filter_map(|fx| self.fx_node(reaper, proj, fx, &mut budget))
                .collect(),
            FxOwner::Take(_) => Vec::new(),
        };
        let mut body = json!({"ref": reference, "owner": kind, "ownerName": name, "chain": chain, "inputChain": input});
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

    /// One node of the tree; a container carries its children. `budget` counts the nodes left to list.
    fn fx_node(&self, reaper: &Reaper, proj: Project, fx: Fx, budget: &mut usize) -> Option<Value> {
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
            "parallel": reaper.fx_config(fx, "parallel").and_then(|p| p.trim().parse::<i32>().ok()).unwrap_or(0),
            "container": is_container,
        });
        if is_container {
            node["children"] =
                reaper.fx_children(fx).into_iter().filter_map(|child| self.fx_node(reaper, proj, child, budget)).collect::<Vec<_>>().into();
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
        let list = |category: i32| {
            let total = reaper.send_count(track, category);
            let rows: Vec<Value> = (0..total.min(MAX_SENDS as i32)).map(|index| rows::send_row(reaper, track, category, index)).collect();
            (rows, total)
        };
        let ((sends, send_total), (receives, receive_total), (hardware, hardware_total)) = (list(0), list(-1), list(1));
        let mut body = json!({
            "ref": refs::track_ref(&id),
            "name": rows::clip(&reaper.track_name(track)),
            "sends": sends,
            "receives": receives,
            "hardwareOuts": hardware,
        });
        // The master has no parent to send to.
        body["parentSend"] = if id == TrackId::Master {
            Value::Null
        } else {
            json!({
                "on": reaper.track_value(track, "B_MAINSEND") != 0.0,
                "offset": reaper.track_value(track, "C_MAINSEND_OFFS") as i64,
                "channels": reaper.track_value(track, "C_MAINSEND_NCH") as i64,
            })
        };
        if send_total as usize > MAX_SENDS || receive_total as usize > MAX_SENDS || hardware_total as usize > MAX_SENDS {
            body["truncated"] = json!({"sends": send_total, "receives": receive_total, "hardwareOuts": hardware_total});
        }
        Ok(body)
    }
}

/// A track in the arrange window: its place in the folders and the items that touch the window.
fn arrange_track(reaper: &Reaper, track: Track, from: f64, to: f64) -> Value {
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
            items.push(arrange_item(reaper, item, position, length));
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

fn arrange_item(reaper: &Reaper, item: Item, position: f64, length: f64) -> Value {
    let take = reaper.active_take(item);
    json!({
        "ref": refs::item_ref(&reaper.item_guid(item)),
        "position": rows::seconds(position),
        "length": rows::seconds(length),
        "name": take.map(|t| rows::clip(&reaper.take_name(t))).unwrap_or_default(),
        "source": rows::source_word(reaper, take),
        "color": reaper.color_hex(reaper.item_value(item, "I_CUSTOMCOLOR") as i32).filter(|_| reaper.item_value(item, "I_CUSTOMCOLOR") as i32 & 0x100_0000 != 0),
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
    fn the_arrange_window_has_the_tracks_in_their_folders_and_the_items_that_touch_it() {
        let reads = lab();
        fake::with(|s| {
            s.markers.push(fake::FakeMarker { is_region: false, start: 40.0, end: 40.0, name: "Far".into(), number: 2, color: 0 });
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
        let body = body(&reads, json!({"kind": "arrange", "window": {"from": 3.0, "to": 10.0}})).unwrap();
        assert_eq!(body["kind"], "arrange");
        assert_eq!(body["epoch"], reads.epoch());
        assert_eq!(body["window"], json!({"from": 3.0, "to": 10.0}));
        let tracks = body["tracks"].as_array().unwrap();
        assert_eq!(refs_of(&body["tracks"]), ["r:track:{AAAA}", "r:track:{BBBB}", "r:track:{CCCC}", "r:track:{DDDD}"]);
        assert_eq!((&tracks[0]["depth"], &tracks[0]["folder"], &tracks[0]["color"]), (&json!(0), &json!(1), &json!("#ff0000")));
        assert_eq!((&tracks[1]["depth"], &tracks[2]["depth"], &tracks[2]["folder"]), (&json!(1), &json!(1), &json!(-1)));
        // Kick's first item ends at 2 s, before the window; the second (2 s to 4 s) touches it.
        assert_eq!(refs_of(&tracks[1]["items"]), ["r:item:{ITEM-K2}"]);
        assert_eq!(
            (&tracks[1]["items"][0]["selected"], &tracks[1]["items"][0]["source"], &tracks[1]["items"][0]["name"]),
            (&json!(true), &json!("wav"), &json!("kick.wav"))
        );
        // Snare's MIDI item (8 s to 12 s) starts inside it.
        assert_eq!(tracks[2]["items"][0]["source"], "mid");
        assert_eq!(
            (&body["playhead"], &body["cursor"], &body["loop"]),
            (&json!(3.0), &json!(3.0), &json!({"on": true, "start": 4.0, "end": 8.0}))
        );
        assert_eq!(body["timeSelection"], Value::Null);
        // The marker at 4 s and the region from 8 s; the marker at 40 s is off the window.
        assert_eq!((refs_of(&body["markers"]), refs_of(&body["regions"])), (vec!["r:marker:1"], vec!["r:region:1"]));
        // The tempo in force when the window opens, and the change inside it; not the one far past it.
        let tempo: Vec<f64> = body["tempoMarkers"].as_array().unwrap().iter().map(|m| m["bpm"].as_f64().unwrap()).collect();
        assert_eq!(tempo, [120.0, 90.0]);
        assert_eq!(body["tempoMarkers"][1], json!({"position": 6.0, "bpm": 90.0, "num": 3, "den": 4, "linear": false}));
    }

    #[test]
    fn without_a_window_the_arrange_view_is_the_window() {
        let reads = lab();
        fake::with(|s| s.arrange_view = (2.0, 9.0));
        assert_eq!(body(&reads, json!({"kind": "arrange"})).unwrap()["window"], json!({"from": 2.0, "to": 9.0}));
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
        let first = body(&reads, json!({"kind": "arrange", "window": {"from": 0.0, "to": 10.0}})).unwrap();
        assert_eq!(first["tracks"].as_array().unwrap().len(), ARRANGE_TRACKS);
        assert!(first["markers"].is_array());
        let second = body(&reads, json!({"kind": "arrange", "window": {"from": 0.0, "to": 10.0}, "cursor": first["nextCursor"]})).unwrap();
        assert_eq!(second["tracks"].as_array().unwrap().len(), 120);
        assert_eq!(second["tracks"][0]["ref"], "r:track:{T500}");
        assert_eq!(second.get("nextCursor"), None);
        assert_eq!(second.get("markers"), None);
    }

    #[test]
    fn a_track_with_too_many_items_lists_some_and_counts_the_rest() {
        let reads = lab();
        fake::with(|s| {
            s.tracks[0].items = (0..450)
                .map(|i| FakeItem::new(&format!("{{I{i}}}"), i as f64 * 0.01, 1.0, vec![FakeTake::wave(&format!("{{K{i}}}"), "x")]))
                .collect();
        });
        let body = body(&reads, json!({"kind": "arrange", "window": {"from": 0.0, "to": 10.0}})).unwrap();
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
        let params = json!({"kind": "arrange", "window": {"from": 0.0, "to": 100.0}});
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
        assert_eq!((&body["window"], &body["length"], &body["noteCount"]), (&json!({"from": 0.0, "to": 8.0}), &json!(8.0), &json!(2)));
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
        let chain = &body["chain"];
        assert_eq!(refs_of(chain), ["r:fx:{FX-EQ}", "r:fx:{FX-C}"]);
        assert_eq!((&chain[0]["enabled"], &chain[0]["preset"], &chain[0]["container"]), (&json!(false), &json!("Warm"), &json!(false)));
        assert_eq!(chain[0].get("children"), None);
        let outer = &chain[1];
        assert_eq!(refs_of(&outer["children"]), ["r:fx:{FX-COMP}", "r:fx:{FX-C2}"]);
        assert_eq!(outer["children"][0]["parallel"], 1);
        assert_eq!(refs_of(&outer["children"][1]["children"]), ["r:fx:{FX-DLY}"]);
        assert_eq!(refs_of(&body["inputChain"]), ["r:fx:{FX-IN}"]);
    }

    #[test]
    fn the_fx_body_covers_takes_items_the_master_and_the_selected_track() {
        let reads = lab();
        fake::with(|s| {
            s.tracks[1].items[1].takes[0].fx.push(FakeFx::new("{FX-TAKE}", "JS: Volume"));
            s.master.fx.push(FakeFx::new("{FX-LIM}", "VST: ReaLimit"));
        });
        let take = body(&reads, json!({"kind": "fx", "ref": "r:take:{TAKE-K2}"})).unwrap();
        assert_eq!((&take["owner"], refs_of(&take["chain"])), (&json!("take"), vec!["r:fx:{FX-TAKE}"]));
        assert_eq!(body(&reads, json!({"kind": "fx", "ref": "r:item:{ITEM-K2}"})).unwrap()["ref"], "r:take:{TAKE-K2}");
        let master = body(&reads, json!({"kind": "fx", "ref": "r:track:master"})).unwrap();
        assert_eq!((&master["owner"], refs_of(&master["chain"])), (&json!("master"), vec!["r:fx:{FX-LIM}"]));
        assert!(body(&reads, json!({"kind": "fx"})).unwrap_err().1.contains("no track is selected"));
        fake::select_tracks(&[3]);
        assert_eq!(body(&reads, json!({"kind": "fx"})).unwrap()["ref"], "r:track:{DDDD}");
    }

    #[test]
    fn a_huge_chain_is_cut_and_says_so() {
        let reads = lab();
        fake::with(|s| s.tracks[3].fx = (0..600).map(|i| FakeFx::new(&format!("{{F{i}}}"), "fx")).collect());
        let body = body(&reads, json!({"kind": "fx", "ref": "r:track:{DDDD}"})).unwrap();
        assert_eq!(body["chain"].as_array().unwrap().len(), MAX_FX_NODES);
        assert_eq!(body["truncated"], true);
    }

    #[test]
    fn routing_lists_sends_receives_hardware_and_the_parent_send() {
        let reads = lab();
        fake::with(|s| s.tracks[3].main_send = false);
        let reverb = body(&reads, json!({"kind": "routing", "ref": "r:track:{DDDD}"})).unwrap();
        assert_eq!((&reverb["ref"], &reverb["name"]), (&json!("r:track:{DDDD}"), &json!("Reverb")));
        assert_eq!(refs_of(&reverb["receives"]), ["r:send:{DDDD}:-1:0"]);
        assert_eq!(reverb["receives"][0]["destRef"], "r:track:{AAAA}");
        assert_eq!(refs_of(&reverb["hardwareOuts"]), ["r:send:{DDDD}:1:0"]);
        assert_eq!(reverb["sends"], json!([]));
        assert_eq!(reverb["parentSend"]["on"], false);
        let drums = body(&reads, json!({"kind": "routing", "ref": "r:track:{AAAA}"})).unwrap();
        assert_eq!(refs_of(&drums["sends"]), ["r:send:{AAAA}:0:0"]);
        assert_eq!(drums["parentSend"]["on"], true);
        // The master has nowhere to send; with no ref the selected track is the one.
        assert_eq!(body(&reads, json!({"kind": "routing", "ref": "r:track:master"})).unwrap()["parentSend"], Value::Null);
        fake::select_tracks(&[0]);
        assert_eq!(body(&reads, json!({"kind": "routing"})).unwrap()["ref"], "r:track:{AAAA}");
        assert_eq!(body(&reads, json!({"kind": "routing", "ref": "r:fx:{FX-EQ}"})).unwrap_err().0, "bad-params");
    }

    #[test]
    fn a_track_with_hundreds_of_sends_lists_the_first_and_counts_them() {
        let reads = lab();
        fake::with(|s| s.tracks[1].sends = (0..260).map(|_| FakeSend::to(3)).collect());
        let body = body(&reads, json!({"kind": "routing", "ref": "r:track:{BBBB}"})).unwrap();
        assert_eq!(body["sends"].as_array().unwrap().len(), MAX_SENDS);
        assert_eq!(body["truncated"]["sends"], 260);
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
