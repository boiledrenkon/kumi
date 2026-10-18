//! `read` and `snapshot`: the project as rows, paged. Every answer stays under the frame bound (256 KiB): a page ends at
//! the limit, at the byte budget or at the time budget, and says where to go on with a cursor.

use std::cell::{RefCell, RefMut};
use std::collections::HashMap;
use std::io::Write;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::focus;
use crate::queue::Answer;
use crate::reaper::{Envelope, Fx, FxOwner, Item, Project, Reaper, Take, Track, INPUT_FX};
use crate::refs::{self, EnvelopeId, Epoch, Ref, Registry, TrackId};
use crate::rows;

pub const DEFAULT_LIMIT: usize = 50;
pub const MAX_LIMIT: usize = 200;
/// The most one answer carries. The bridge's frame bound is 256 KiB; the rest is the envelope around the rows.
pub const ANSWER_BYTES: usize = 224 * 1024;
/// A page stops growing after this long on the main thread: the rest comes with the cursor.
const PAGE_TIME: Duration = Duration::from_millis(40);
/// The most tracks and markers a snapshot carries; `read` has the rest.
const SNAPSHOT_TRACKS: usize = 200;
const SNAPSHOT_MARKERS: usize = 100;
/// The snapshot's own share of the answer, so the tracks leave room for the markers and the selection.
const SNAPSHOT_TRACK_BYTES: usize = 150 * 1024;
/// A track's own envelopes, by the chunk name `GetTrackEnvelopeByChunkName` takes.
const OWN_ENVELOPES: [&str; 8] = ["VOLENV", "VOLENV2", "VOLENV3", "PANENV", "PANENV2", "WIDTHENV", "WIDTHENV2", "MUTEENV"];
/// The parameters probed for an envelope in one call, so a plug-in with thousands doesn't hold the main thread.
const PARAM_PROBES: usize = 6000;
const KINDS: [&str; 11] = ["project", "track", "item", "take", "fx", "param", "send", "marker", "region", "note", "envelope"];

pub fn bad(message: impl Into<String>) -> (String, String) {
    ("bad-params".into(), message.into())
}

pub fn failed(message: impl Into<String>) -> (String, String) {
    ("failed".into(), message.into())
}

/// The refs' registry and the epoch they live in.
#[derive(Default)]
pub struct Reads {
    registry: RefCell<Registry>,
    epoch: RefCell<Epoch>,
    /// How many times a new project took the place of the one in a tab, by tab: part of the project's identity.
    replaced: RefCell<HashMap<usize, u32>>,
}

/// The size of a value as JSON, without building the text.
pub fn json_len(value: &Value) -> usize {
    struct Count(usize);
    impl Write for Count {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0 += bytes.len();
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut count = Count(0);
    let _ = serde_json::to_writer(&mut count, value);
    count.0
}

/// Rows gathered for one answer.
pub struct Pager {
    limit: usize,
    budget: usize,
    bytes: usize,
    started: Instant,
    pub rows: Vec<Value>,
}

impl Pager {
    pub fn new(limit: usize, budget: usize) -> Pager {
        Pager { limit, budget, bytes: 0, started: Instant::now(), rows: Vec::new() }
    }

    /// Whether the page has room for another row.
    pub fn open(&self) -> bool {
        self.rows.len() < self.limit && self.bytes < self.budget && (self.rows.is_empty() || self.started.elapsed() < PAGE_TIME)
    }

    /// Adds a row; false when it would overflow the byte budget (a first row always goes in).
    pub fn push(&mut self, row: Value) -> bool {
        let size = json_len(&row) + 1;
        if !self.rows.is_empty() && self.bytes + size > self.budget {
            return false;
        }
        self.bytes += size;
        self.rows.push(row);
        true
    }
}

/// What a `read` was asked.
#[derive(Debug, Default)]
struct Request {
    kind: Option<String>,
    reference: Option<String>,
    parent: Option<String>,
    cursor: Option<String>,
    limit: usize,
}

fn text_param(params: &Value, name: &str) -> Result<Option<String>, (String, String)> {
    match params.get(name) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) if !text.is_empty() => Ok(Some(text.clone())),
        Some(_) => Err(bad(format!("{name} must be a non-empty string"))),
    }
}

fn limit_param(params: &Value) -> Result<usize, (String, String)> {
    match params.get("limit") {
        None | Some(Value::Null) => Ok(DEFAULT_LIMIT),
        Some(value) => match value.as_u64() {
            Some(n) if n >= 1 => Ok((n as usize).min(MAX_LIMIT)),
            _ => Err(bad(format!("limit must be a whole number from 1 to {MAX_LIMIT}"))),
        },
    }
}

impl Request {
    fn parse(params: &Value) -> Result<Request, (String, String)> {
        let request = Request {
            kind: text_param(params, "kind")?,
            reference: text_param(params, "ref")?,
            parent: text_param(params, "parent")?,
            cursor: text_param(params, "cursor")?,
            limit: limit_param(params)?,
        };
        match (&request.kind, &request.reference) {
            (None, None) => return Err(bad(format!("kind: one of {}", KINDS.join(", ")))),
            (Some(kind), _) if !KINDS.contains(&kind.as_str()) => {
                return Err(bad(format!("REAPER has no kind {kind:?} to read: use one of {}", KINDS.join(", "))))
            }
            _ => {}
        }
        Ok(request)
    }
}

/// The project a read is about: the active tab.
fn active(reaper: &Reaper) -> Result<(Project, crate::reaper::ProjectInfo), (String, String)> {
    let info = reaper.current_project().ok_or_else(|| failed("REAPER has no project open"))?;
    Ok((info.ptr, info))
}

impl Reads {
    pub fn epoch(&self) -> String {
        self.epoch.borrow().text()
    }

    /// Refs stop meaning what they meant: a tab switch, or a new project in the tab.
    pub fn bump_epoch(&self) {
        self.epoch.borrow_mut().bump();
        self.registry.borrow_mut().clear();
    }

    /// What names the project open in this tab while it stays open in this REAPER: this process, the tab, and how many
    /// projects have taken the tab's place.
    pub fn identity(&self, proj: Project) -> String {
        let replaced = self.replaced.borrow().get(&proj.addr()).copied().unwrap_or(0);
        let tab = format!("{}:{:x}", std::process::id(), proj.addr());
        if replaced == 0 {
            tab
        } else {
            format!("{tab}.{replaced}")
        }
    }

    /// A new project took the place of the one in this tab.
    pub fn project_replaced(&self, proj: Project) {
        *self.replaced.borrow_mut().entry(proj.addr()).or_insert(0) += 1;
    }

    pub fn registry(&self) -> RefMut<'_, Registry> {
        self.registry.borrow_mut()
    }

    fn cursor_text(&self, position: &str) -> String {
        format!("c1.{}.{position}", self.epoch())
    }

    /// The position a cursor names, or an error when it isn't this project's.
    fn cursor_position(&self, cursor: Option<&str>) -> Result<Option<String>, (String, String)> {
        let Some(cursor) = cursor else { return Ok(None) };
        let mut parts = cursor.splitn(3, '.');
        match (parts.next(), parts.next(), parts.next()) {
            (Some("c1"), Some(epoch), Some(position)) if !position.is_empty() => {
                if epoch == self.epoch() {
                    Ok(Some(position.to_string()))
                } else {
                    Err(bad("that cursor is from before the project tab changed: read again from the start"))
                }
            }
            _ => Err(bad("cursor: pass the nextCursor an earlier read gave, unchanged")),
        }
    }

    fn position_number(&self, cursor: Option<&str>) -> Result<usize, (String, String)> {
        match self.cursor_position(cursor)? {
            None => Ok(0),
            Some(position) => position.parse::<usize>().map_err(|_| bad("cursor: not one this kind of read gave")),
        }
    }

    fn page(&self, pager: Pager, next: Option<usize>, total: Option<usize>) -> Answer {
        self.page_at(pager, next.map(|n| n.to_string()), total)
    }

    fn page_at(&self, pager: Pager, next: Option<String>, total: Option<usize>) -> Answer {
        let mut answer = json!({"items": pager.rows, "epoch": self.epoch()});
        if let Some(total) = total {
            answer["total"] = total.into();
        }
        if let Some(next) = next {
            answer["nextCursor"] = self.cursor_text(&next).into();
        }
        Ok(answer)
    }

    // ----- resolving refs -----

    fn track_by_id(&self, reaper: &Reaper, proj: Project, id: &TrackId) -> Result<Track, (String, String)> {
        self.registry
            .borrow_mut()
            .track(reaper, proj, id)
            .ok_or_else(|| failed(format!("{} isn't in the project (deleted, or another tab)", refs::track_ref(id))))
    }

    fn item_by_guid(&self, reaper: &Reaper, proj: Project, guid: &str) -> Result<Item, (String, String)> {
        self.registry
            .borrow_mut()
            .item(reaper, proj, guid)
            .ok_or_else(|| failed(format!("{} isn't in the project (deleted, or another tab)", refs::item_ref(guid))))
    }

    fn take_by_guid(&self, reaper: &Reaper, proj: Project, guid: &str) -> Result<Take, (String, String)> {
        self.registry
            .borrow_mut()
            .take(reaper, proj, guid)
            .ok_or_else(|| failed(format!("{} isn't in the project (deleted, or another tab)", refs::take_ref(guid))))
    }

    fn fx_by_guid(&self, reaper: &Reaper, proj: Project, guid: &str) -> Result<Fx, (String, String)> {
        self.registry
            .borrow_mut()
            .fx(reaper, proj, guid)
            .ok_or_else(|| failed(format!("{} isn't in the project (deleted, or another tab)", refs::fx_ref(guid))))
    }

    /// A parent ref that must be a track.
    fn parent_track(&self, reaper: &Reaper, proj: Project, parent: &str) -> Result<Track, (String, String)> {
        match Ref::parse(parent).map_err(bad)? {
            Ref::Track(id) => self.track_by_id(reaper, proj, &id),
            other => Err(bad(format!("parent: a track ref, not r:{}", other.kind()))),
        }
    }

    fn parent_fx(&self, reaper: &Reaper, proj: Project, parent: &str) -> Result<Fx, (String, String)> {
        match Ref::parse(parent).map_err(bad)? {
            Ref::Fx(guid) => self.fx_by_guid(reaper, proj, &guid),
            other => Err(bad(format!("parent: an FX ref, not r:{}", other.kind()))),
        }
    }

    // ----- read -----

    pub fn read(&self, reaper: &Reaper, params: &Value) -> Answer {
        let request = Request::parse(params)?;
        let (proj, info) = active(reaper)?;
        if let Some(reference) = &request.reference {
            return self.read_one(reaper, proj, reference);
        }
        let kind = request.kind.as_deref().unwrap_or_default();
        let pager = Pager::new(request.limit, ANSWER_BYTES);
        let cursor = request.cursor.as_deref();
        let parent = request.parent.as_deref();
        match kind {
            "project" => {
                let mut pager = pager;
                pager.push(rows::project_row(reaper, proj, &info, &self.identity(proj)));
                self.page(pager, None, Some(1))
            }
            "track" => self.read_tracks(reaper, proj, pager, cursor),
            "item" => self.read_items(reaper, proj, pager, parent, cursor),
            "take" => self.read_takes(reaper, proj, pager, parent, cursor),
            "fx" => self.read_fx(reaper, proj, pager, parent, cursor),
            "param" => self.read_params(reaper, proj, pager, parent, cursor),
            "send" => self.read_sends(reaper, proj, pager, parent, cursor),
            "marker" => self.read_markers(reaper, proj, pager, false, cursor),
            "region" => self.read_markers(reaper, proj, pager, true, cursor),
            "note" => self.read_notes(reaper, proj, pager, parent, cursor),
            "envelope" => self.read_envelopes(reaper, proj, pager, parent, cursor),
            other => Err(bad(format!("REAPER has no kind {other:?} to read"))),
        }
    }

    /// `read {ref}`: the one object a ref names.
    fn read_one(&self, reaper: &Reaper, proj: Project, reference: &str) -> Answer {
        let row = match Ref::parse(reference).map_err(bad)? {
            Ref::Track(id) => rows::track_row(reaper, self.track_by_id(reaper, proj, &id)?),
            Ref::Item(guid) => rows::item_row(reaper, self.item_by_guid(reaper, proj, &guid)?),
            Ref::Take(guid) => take_row(reaper, self.take_by_guid(reaper, proj, &guid)?, None),
            Ref::Fx(guid) => {
                let fx = self.fx_by_guid(reaper, proj, &guid)?;
                rows::fx_row(reaper, fx, chain_position(reaper, fx))
            }
            Ref::Param { fx: guid, index } => {
                let fx = self.fx_by_guid(reaper, proj, &guid)?;
                if index < 0 || index >= reaper.fx_param_count(fx) {
                    return Err(failed(format!("{reference} isn't there: the FX has {} parameters", reaper.fx_param_count(fx))));
                }
                rows::param_row(reaper, fx, &guid, index)
            }
            Ref::Send { track, category, index } => {
                let owner = self.track_by_id(reaper, proj, &track)?;
                if index < 0 || index >= reaper.send_count(owner, category) {
                    return Err(failed(format!("{reference} isn't there: the track has {} of them", reaper.send_count(owner, category))));
                }
                rows::send_row(reaper, owner, category, index)
            }
            Ref::Marker(number) => self.marker_by_number(reaper, proj, number, false)?,
            Ref::Region(number) => self.marker_by_number(reaper, proj, number, true)?,
            Ref::Envelope { track, which } => {
                let owner = self.track_by_id(reaper, proj, &track)?;
                let envelope = self.envelope_of(reaper, proj, owner, &which)?;
                rows::envelope_row(reaper, &track, &which, envelope)
            }
        };
        Ok(json!({"items": [row], "epoch": self.epoch()}))
    }

    fn marker_by_number(&self, reaper: &Reaper, proj: Project, number: i32, region: bool) -> Result<Value, (String, String)> {
        (0..reaper.marker_count(proj))
            .filter_map(|i| reaper.marker(proj, i))
            .find(|m| m.is_region == region && m.number == number)
            .map(|m| rows::marker_row(reaper, &m))
            .ok_or_else(|| failed(format!("there is no {} {number}", if region { "region" } else { "marker" })))
    }

    fn envelope_of(&self, reaper: &Reaper, proj: Project, track: Track, which: &EnvelopeId) -> Result<Envelope, (String, String)> {
        let found = match which {
            EnvelopeId::Chunk(name) => reaper.track_envelope_by_chunk_name(track, &format!("<{name}")),
            EnvelopeId::Param { fx, index } => {
                let fx = self.fx_by_guid(reaper, proj, fx)?;
                match fx.owner {
                    FxOwner::Track(owner) if owner == track => reaper.fx_param_envelope(track, fx.index, *index),
                    _ => None,
                }
            }
            EnvelopeId::Position(position) => reaper.track_envelope(track, *position),
        };
        found.ok_or_else(|| failed("that track has no such envelope"))
    }

    // ----- tracks -----

    /// Rows of the master (position 0) and the tracks (1 and on).
    pub fn track_page(&self, reaper: &Reaper, proj: Project, pager: &mut Pager, start: usize) -> Option<usize> {
        let count = reaper.track_count(proj) as usize;
        let mut position = start;
        while position <= count {
            if !pager.open() {
                return Some(position);
            }
            let track = if position == 0 { reaper.master_track(proj) } else { reaper.track_at(proj, position as i32 - 1) };
            if let Some(track) = track {
                if !pager.push(rows::track_row(reaper, track)) {
                    return Some(position);
                }
            }
            position += 1;
        }
        None
    }

    fn read_tracks(&self, reaper: &Reaper, proj: Project, mut pager: Pager, cursor: Option<&str>) -> Answer {
        let start = self.position_number(cursor)?;
        let next = self.track_page(reaper, proj, &mut pager, start);
        self.page(pager, next, Some(reaper.track_count(proj) as usize + 1))
    }

    // ----- items and takes -----

    fn read_items(&self, reaper: &Reaper, proj: Project, mut pager: Pager, parent: Option<&str>, cursor: Option<&str>) -> Answer {
        if let Some(parent) = parent {
            let track = self.parent_track(reaper, proj, parent)?;
            let total = reaper.track_item_count(track) as usize;
            let mut position = self.position_number(cursor)?;
            let mut next = None;
            while position < total {
                if !pager.open() {
                    next = Some(position);
                    break;
                }
                if let Some(item) = reaper.track_item(track, position as i32) {
                    if !pager.push(rows::item_row(reaper, item)) {
                        next = Some(position);
                        break;
                    }
                }
                position += 1;
            }
            return self.page(pager, next, Some(total));
        }
        // Every item in the project: the cursor is `track.item`.
        let (mut track_at, mut item_at) = match self.cursor_position(cursor)? {
            None => (0usize, 0usize),
            Some(position) => {
                let (t, i) = position.split_once('.').ok_or_else(|| bad("cursor: not one this kind of read gave"))?;
                (
                    t.parse().map_err(|_| bad("cursor: not one this kind of read gave"))?,
                    i.parse().map_err(|_| bad("cursor: not one this kind of read gave"))?,
                )
            }
        };
        let tracks = reaper.track_count(proj) as usize;
        let mut next = None;
        'tracks: while track_at < tracks {
            if let Some(track) = reaper.track_at(proj, track_at as i32) {
                let count = reaper.track_item_count(track) as usize;
                while item_at < count {
                    if !pager.open() {
                        next = Some(format!("{track_at}.{item_at}"));
                        break 'tracks;
                    }
                    if let Some(item) = reaper.track_item(track, item_at as i32) {
                        if !pager.push(rows::item_row(reaper, item)) {
                            next = Some(format!("{track_at}.{item_at}"));
                            break 'tracks;
                        }
                    }
                    item_at += 1;
                }
            }
            track_at += 1;
            item_at = 0;
        }
        self.page_at(pager, next, None)
    }

    fn read_takes(&self, reaper: &Reaper, proj: Project, mut pager: Pager, parent: Option<&str>, cursor: Option<&str>) -> Answer {
        let parent = parent.ok_or_else(|| bad("take: pass the item as parent"))?;
        let item = match Ref::parse(parent).map_err(bad)? {
            Ref::Item(guid) => self.item_by_guid(reaper, proj, &guid)?,
            other => return Err(bad(format!("parent: an item ref, not r:{}", other.kind()))),
        };
        let total = reaper.take_count(item) as usize;
        let active = reaper.active_take(item);
        let mut position = self.position_number(cursor)?;
        let mut next = None;
        while position < total {
            if !pager.open() {
                next = Some(position);
                break;
            }
            if let Some(take) = reaper.take_at(item, position as i32) {
                if !pager.push(take_row(reaper, take, Some((position, active == Some(take))))) {
                    next = Some(position);
                    break;
                }
            }
            position += 1;
        }
        self.page(pager, next, Some(total))
    }

    // ----- FX and parameters -----

    fn read_fx(&self, reaper: &Reaper, proj: Project, mut pager: Pager, parent: Option<&str>, cursor: Option<&str>) -> Answer {
        let parent = parent.ok_or_else(|| bad("fx: pass the track, take or container as parent"))?;
        // The chain to list: top-level FX plus, on a track, its input chain; or one container's children.
        let chain: Vec<Fx> = match Ref::parse(parent).map_err(bad)? {
            Ref::Track(id) => {
                let track = self.track_by_id(reaper, proj, &id)?;
                let owner = FxOwner::Track(track);
                (0..reaper.fx_count(owner))
                    .map(|index| Fx { owner, index })
                    .chain((0..reaper.input_fx_count(track)).map(|index| Fx { owner, index: INPUT_FX | index }))
                    .collect()
            }
            Ref::Take(guid) => {
                let owner = FxOwner::Take(self.take_by_guid(reaper, proj, &guid)?);
                (0..reaper.fx_count(owner)).map(|index| Fx { owner, index }).collect()
            }
            Ref::Fx(guid) => reaper.fx_children(self.fx_by_guid(reaper, proj, &guid)?),
            other => return Err(bad(format!("parent: a track, take or FX ref, not r:{}", other.kind()))),
        };
        let total = chain.len();
        let mut position = self.position_number(cursor)?;
        let mut next = None;
        while position < total {
            if !pager.open() {
                next = Some(position);
                break;
            }
            let fx = chain[position];
            self.registry.borrow_mut().note_fx(proj, &reaper.fx_guid(fx), fx);
            if !pager.push(rows::fx_row(reaper, fx, position)) {
                next = Some(position);
                break;
            }
            position += 1;
        }
        self.page(pager, next, Some(total))
    }

    fn read_params(&self, reaper: &Reaper, proj: Project, mut pager: Pager, parent: Option<&str>, cursor: Option<&str>) -> Answer {
        let parent = parent.ok_or_else(|| bad("param: pass the FX as parent"))?;
        let fx = self.parent_fx(reaper, proj, parent)?;
        let guid = reaper.fx_guid(fx);
        let total = reaper.fx_param_count(fx).max(0) as usize;
        let mut position = self.position_number(cursor)?;
        let mut next = None;
        while position < total {
            if !pager.open() {
                next = Some(position);
                break;
            }
            if !pager.push(rows::param_row(reaper, fx, &guid, position as i32)) {
                next = Some(position);
                break;
            }
            position += 1;
        }
        self.page(pager, next, Some(total))
    }

    // ----- sends -----

    fn read_sends(&self, reaper: &Reaper, proj: Project, mut pager: Pager, parent: Option<&str>, cursor: Option<&str>) -> Answer {
        let parent = parent.ok_or_else(|| bad("send: pass the track as parent"))?;
        let track = self.parent_track(reaper, proj, parent)?;
        // Sends, then receives, then hardware outputs.
        let counts = [(0, reaper.send_count(track, 0)), (-1, reaper.send_count(track, -1)), (1, reaper.send_count(track, 1))];
        let all: Vec<(i32, i32)> = counts.iter().flat_map(|&(category, n)| (0..n).map(move |index| (category, index))).collect();
        let mut position = self.position_number(cursor)?;
        let mut next = None;
        while position < all.len() {
            if !pager.open() {
                next = Some(position);
                break;
            }
            let (category, index) = all[position];
            if !pager.push(rows::send_row(reaper, track, category, index)) {
                next = Some(position);
                break;
            }
            position += 1;
        }
        self.page(pager, next, Some(all.len()))
    }

    // ----- markers and regions -----

    fn read_markers(&self, reaper: &Reaper, proj: Project, mut pager: Pager, regions: bool, cursor: Option<&str>) -> Answer {
        let total = reaper.marker_count(proj);
        let mut position = self.position_number(cursor)? as i32;
        let mut next = None;
        while position < total {
            let Some(marker) = reaper.marker(proj, position) else { break };
            if marker.is_region == regions {
                if !pager.open() {
                    next = Some(position as usize);
                    break;
                }
                if !pager.push(rows::marker_row(reaper, &marker)) {
                    next = Some(position as usize);
                    break;
                }
            }
            position += 1;
        }
        self.page(pager, next, None)
    }

    // ----- MIDI notes -----

    fn read_notes(&self, reaper: &Reaper, proj: Project, mut pager: Pager, parent: Option<&str>, cursor: Option<&str>) -> Answer {
        let parent = parent.ok_or_else(|| bad("note: pass the MIDI take as parent"))?;
        let take = match Ref::parse(parent).map_err(bad)? {
            Ref::Take(guid) => self.take_by_guid(reaper, proj, &guid)?,
            Ref::Item(guid) => {
                let item = self.item_by_guid(reaper, proj, &guid)?;
                reaper.active_take(item).ok_or_else(|| failed("that item has no take"))?
            }
            other => return Err(bad(format!("parent: a take ref, not r:{}", other.kind()))),
        };
        let (notes, _, _) = reaper.midi_counts(take).ok_or_else(|| bad("that take isn't MIDI"))?;
        let item_qn = take_start_qn(reaper, proj, take);
        let mut position = self.position_number(cursor)?;
        let mut next = None;
        while position < notes.max(0) as usize {
            if !pager.open() {
                next = Some(position);
                break;
            }
            if let Some(note) = reaper.midi_note(take, position as i32) {
                if !pager.push(rows::note_row(reaper, take, item_qn, position as i32, &note)) {
                    next = Some(position);
                    break;
                }
            }
            position += 1;
        }
        self.page(pager, next, Some(notes.max(0) as usize))
    }

    // ----- envelopes -----

    /// A track's envelopes with the id their refs use: own ones by chunk name, FX ones by FX and parameter.
    fn track_envelopes(&self, reaper: &Reaper, proj: Project, track: Track) -> Vec<(EnvelopeId, Envelope)> {
        let own: Vec<(&str, Envelope)> = OWN_ENVELOPES
            .iter()
            .filter_map(|name| reaper.track_envelope_by_chunk_name(track, &format!("<{name}")).map(|e| (*name, e)))
            .collect();
        let mut fx_params: Option<HashMap<Envelope, (String, i32)>> = None;
        let mut found = Vec::new();
        for position in 0..reaper.track_envelope_count(track) {
            let Some(envelope) = reaper.track_envelope(track, position) else { continue };
            let which = if let Some((name, _)) = own.iter().find(|(_, e)| *e == envelope) {
                EnvelopeId::Chunk((*name).to_string())
            } else {
                let map = fx_params.get_or_insert_with(|| self.fx_param_envelopes(reaper, proj, track));
                match map.get(&envelope) {
                    Some((fx, index)) => EnvelopeId::Param { fx: fx.clone(), index: *index },
                    None => EnvelopeId::Position(position),
                }
            };
            found.push((which, envelope));
        }
        found
    }

    /// Every parameter envelope on a track's FX, found by asking each parameter (REAPER can't list them by FX).
    fn fx_param_envelopes(&self, reaper: &Reaper, proj: Project, track: Track) -> HashMap<Envelope, (String, i32)> {
        let mut map = HashMap::new();
        let mut probes = 0usize;
        let mut pending: Vec<Fx> = {
            let owner = FxOwner::Track(track);
            (0..reaper.fx_count(owner)).map(|index| Fx { owner, index }).collect()
        };
        while let Some(fx) = pending.pop() {
            let guid = reaper.fx_guid(fx);
            self.registry.borrow_mut().note_fx(proj, &guid, fx);
            pending.extend(reaper.fx_children(fx));
            for param in 0..reaper.fx_param_count(fx) {
                probes += 1;
                if probes > PARAM_PROBES {
                    return map;
                }
                if let Some(envelope) = reaper.fx_param_envelope(track, fx.index, param) {
                    map.insert(envelope, (guid.clone(), param));
                }
            }
        }
        map
    }

    fn read_envelopes(&self, reaper: &Reaper, proj: Project, mut pager: Pager, parent: Option<&str>, cursor: Option<&str>) -> Answer {
        let parent = parent.ok_or_else(|| bad("envelope: pass the track or FX as parent"))?;
        let found: Vec<(TrackId, EnvelopeId, Envelope)> = match Ref::parse(parent).map_err(bad)? {
            Ref::Track(id) => {
                let track = self.track_by_id(reaper, proj, &id)?;
                self.track_envelopes(reaper, proj, track).into_iter().map(|(which, e)| (id.clone(), which, e)).collect()
            }
            Ref::Fx(guid) => {
                let fx = self.fx_by_guid(reaper, proj, &guid)?;
                let FxOwner::Track(track) = fx.owner else { return Err(bad("take FX envelopes aren't read yet")) };
                let id = rows::track_id(reaper, track);
                (0..reaper.fx_param_count(fx))
                    .filter_map(|param| reaper.fx_param_envelope(track, fx.index, param).map(|e| (param, e)))
                    .map(|(param, e)| (id.clone(), EnvelopeId::Param { fx: guid.clone(), index: param }, e))
                    .collect()
            }
            other => return Err(bad(format!("parent: a track or FX ref, not r:{}", other.kind()))),
        };
        let mut position = self.position_number(cursor)?;
        let mut next = None;
        while position < found.len() {
            if !pager.open() {
                next = Some(position);
                break;
            }
            let (track, which, envelope) = &found[position];
            if !pager.push(rows::envelope_row(reaper, track, which, *envelope)) {
                next = Some(position);
                break;
            }
            position += 1;
        }
        self.page(pager, next, Some(found.len()))
    }

    // ----- snapshot -----

    /// The project on one screen: the project, the transport, the tracks (the first 200), the selection and the markers.
    /// What doesn't fit is named in `truncated`, with the cursor for `read`.
    pub fn snapshot(&self, reaper: &Reaper) -> Answer {
        let (proj, info) = active(reaper)?;
        let mut tracks = Pager::new(SNAPSHOT_TRACKS + 1, SNAPSHOT_TRACK_BYTES);
        let next_track = self.track_page(reaper, proj, &mut tracks, 0);
        let mut markers = Pager::new(SNAPSHOT_MARKERS, ANSWER_BYTES - SNAPSHOT_TRACK_BYTES - 16 * 1024);
        let marker_total = reaper.marker_count(proj);
        let mut next_marker = None;
        for index in 0..marker_total {
            let Some(marker) = reaper.marker(proj, index) else { break };
            if !markers.open() || !markers.push(rows::marker_row(reaper, &marker)) {
                next_marker = Some(index as usize);
                break;
            }
        }
        let mut answer = json!({
            "epoch": self.epoch(),
            "project": rows::project_row(reaper, proj, &info, &self.identity(proj)),
            "transport": rows::transport(reaper, proj),
            "tracks": tracks.rows,
            "selection": focus::selection(self, reaper, proj),
            "markers": markers.rows,
        });
        let mut truncated = serde_json::Map::new();
        if let Some(next) = next_track {
            truncated
                .insert("tracks".into(), json!({"total": reaper.track_count(proj) + 1, "nextCursor": self.cursor_text(&next.to_string())}));
        }
        if let Some(next) = next_marker {
            truncated.insert("markers".into(), json!({"total": marker_total, "nextCursor": self.cursor_text(&next.to_string())}));
        }
        if !truncated.is_empty() {
            answer["truncated"] = Value::Object(truncated);
        }
        Ok(answer)
    }
}

/// The position of the item a take is on, in quarter notes.
pub fn take_start_qn(reaper: &Reaper, proj: Project, take: Take) -> f64 {
    reaper.take_item(take).and_then(|item| reaper.quarter_notes_at(proj, reaper.item_value(item, "D_POSITION"))).unwrap_or(0.0)
}

/// Where an FX sits in its parent's chain (an input FX continues after the main chain).
fn chain_position(reaper: &Reaper, fx: Fx) -> usize {
    let siblings = match reaper.fx_config(fx, "parent_container").and_then(|p| p.trim().parse::<i32>().ok()) {
        Some(parent) => reaper.fx_children(Fx { owner: fx.owner, index: parent }),
        None => {
            let main = reaper.fx_count(fx.owner);
            let base = fx.index & !INPUT_FX;
            return if fx.index & INPUT_FX != 0 { main as usize + base as usize } else { base as usize };
        }
    };
    siblings.iter().position(|s| *s == fx).unwrap_or(0)
}

fn take_row(reaper: &Reaper, take: Take, place: Option<(usize, bool)>) -> Value {
    let mut row = json!({
        "ref": refs::take_ref(&reaper.take_guid(take)),
        "name": rows::clip(&reaper.take_name(take)),
        "source": rows::source_word(reaper, Some(take)),
        "fxCount": reaper.fx_count(FxOwner::Take(take)),
    });
    if let Some((index, active)) = place {
        row["index"] = index.into();
        row["active"] = active.into();
    }
    if let Some((notes, ccs, _)) = reaper.midi_counts(take) {
        row["noteCount"] = notes.into();
        row["ccCount"] = ccs.into();
    }
    row
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reaper::fake::{self, FakeTrack};

    /// Reads against the lab project through the fake table.
    struct Lab {
        reads: Reads,
    }

    impl Lab {
        fn new() -> Lab {
            fake::lab_project();
            Lab { reads: Reads::default() }
        }

        fn read(&self, params: Value) -> Answer {
            let api = fake::api();
            self.reads.read(&Reaper::new(&api), &params)
        }

        fn rows(&self, params: Value) -> Vec<Value> {
            self.read(params).unwrap()["items"].as_array().unwrap().clone()
        }
    }

    #[test]
    fn the_project_is_one_row() {
        let lab = Lab::new();
        let answer = lab.read(json!({"kind": "project"})).unwrap();
        assert_eq!(answer["epoch"], lab.reads.epoch());
        let project = &answer["items"][0];
        assert_eq!(project["name"], "Song.rpp");
        assert_eq!(project["path"], "/lab/Projects/Song.rpp");
        assert_eq!(project["saved"], true);
        assert_eq!(project["tempo"], 120.0);
        assert_eq!(project["timeSignature"], json!([4, 4]));
        assert_eq!(project["length"], 64.0);
        assert_eq!(project["playState"], "stopped");
        assert_eq!(project["timeSelection"], Value::Null);
        assert_eq!(project["tabCount"], 1);
        // The Kumi project id the project carries, once it has one; and what names it while it stays open.
        assert_eq!(project["id"], Value::Null);
        assert_eq!(project["identity"], lab.reads.identity(Project::first()));
        fake::with(|s| {
            s.project_ext.insert(("kumi".into(), "project".into()), "0123456789abcdef0123456789abcdef".into());
        });
        assert_eq!(lab.rows(json!({"kind": "project"}))[0]["id"], "0123456789abcdef0123456789abcdef");
    }

    #[test]
    fn a_project_is_named_by_its_tab_until_another_takes_the_tabs_place() {
        let lab = Lab::new();
        let before = lab.rows(json!({"kind": "project"}))[0]["identity"].as_str().unwrap().to_string();
        assert!(before.starts_with(&format!("{}:", std::process::id())), "{before}");
        assert_eq!(lab.rows(json!({"kind": "project"}))[0]["identity"], before.as_str());
        lab.reads.project_replaced(Project::first());
        let after = lab.rows(json!({"kind": "project"}))[0]["identity"].as_str().unwrap().to_string();
        assert_ne!(after, before);
        assert!(after.starts_with(&before));
        // Another tab is another project.
        fake::with(|s| s.project_id += 1);
        assert!(!lab.rows(json!({"kind": "project"}))[0]["identity"].as_str().unwrap().starts_with(&before));
    }

    #[test]
    fn tracks_come_master_first_with_folders_colours_and_counts() {
        let lab = Lab::new();
        fake::with(|s| {
            s.tracks[0].solo = 1;
            s.tracks[1].arm = true;
            s.tracks[2].mute = true;
        });
        fake::select_tracks(&[2]);
        let rows = lab.rows(json!({"kind": "track"}));
        assert_eq!(
            rows.iter().map(|r| r["ref"].as_str().unwrap()).collect::<Vec<_>>(),
            ["r:track:master", "r:track:{AAAA}", "r:track:{BBBB}", "r:track:{CCCC}", "r:track:{DDDD}"]
        );
        assert_eq!(rows[0]["master"], true);
        assert_eq!(rows[0]["index"], 0);
        let drums = &rows[1];
        assert_eq!(drums["index"], 1);
        assert_eq!(drums["name"], "Drums");
        assert_eq!((&drums["depth"], &drums["folder"]), (&json!(0), &json!(1)));
        assert_eq!(drums["color"], "#ff0000");
        assert_eq!(drums["volumeDb"], -6.02);
        assert_eq!(drums["pan"], -0.25);
        assert_eq!(
            (&drums["fxCount"], &drums["sends"], &drums["receives"], &drums["itemCount"]),
            (&json!(2), &json!(1), &json!(0), &json!(0))
        );
        assert_eq!(drums["solo"], true);
        // The children of the folder sit one deeper, and the track that closes it says so.
        assert_eq!(
            (&rows[2]["depth"], &rows[2]["folder"], &rows[2]["arm"], &rows[2]["color"]),
            (&json!(1), &json!(0), &json!(true), &Value::Null)
        );
        assert_eq!(
            (&rows[3]["depth"], &rows[3]["folder"], &rows[3]["mute"], &rows[3]["selected"]),
            (&json!(1), &json!(-1), &json!(true), &json!(true))
        );
        assert_eq!((&rows[4]["depth"], &rows[4]["receives"]), (&json!(0), &json!(1)));
    }

    #[test]
    fn a_page_ends_at_the_limit_and_the_cursor_goes_on_from_there() {
        let lab = Lab::new();
        let first = lab.read(json!({"kind": "track", "limit": 2})).unwrap();
        assert_eq!(first["items"].as_array().unwrap().len(), 2);
        assert_eq!(first["total"], 5);
        let second = lab.read(json!({"kind": "track", "limit": 2, "cursor": first["nextCursor"]})).unwrap();
        assert_eq!(second["items"][0]["ref"], "r:track:{BBBB}");
        let third = lab.read(json!({"kind": "track", "limit": 2, "cursor": second["nextCursor"]})).unwrap();
        assert_eq!(third["items"].as_array().unwrap().len(), 1);
        assert_eq!(third.get("nextCursor"), None);
    }

    #[test]
    fn a_cursor_from_another_epoch_or_made_up_is_refused() {
        let lab = Lab::new();
        let first = lab.read(json!({"kind": "track", "limit": 1})).unwrap();
        let cursor = first["nextCursor"].clone();
        lab.reads.bump_epoch();
        let (code, message) = lab.read(json!({"kind": "track", "cursor": cursor})).unwrap_err();
        assert_eq!(code, "bad-params");
        assert!(message.contains("from before the project tab changed"), "{message}");
        assert!(lab.read(json!({"kind": "track", "cursor": "7"})).unwrap_err().1.contains("nextCursor"));
        assert!(lab.read(json!({"kind": "track", "cursor": format!("c1.{}.x", lab.reads.epoch())})).unwrap_err().1.contains("cursor"));
    }

    #[test]
    fn limits_default_to_fifty_and_stop_at_two_hundred() {
        let lab = Lab::new();
        fake::with(|s| s.tracks = (0..260).map(|i| FakeTrack::new(&format!("{{T{i}}}"), "t")).collect());
        assert_eq!(lab.rows(json!({"kind": "track"})).len(), DEFAULT_LIMIT);
        assert_eq!(lab.rows(json!({"kind": "track", "limit": 500})).len(), MAX_LIMIT);
        for bad_limit in [json!(0), json!(-1), json!("ten"), json!(1.5)] {
            assert_eq!(lab.read(json!({"kind": "track", "limit": bad_limit})).unwrap_err().0, "bad-params");
        }
    }

    #[test]
    fn items_of_a_track_and_of_the_whole_project() {
        let lab = Lab::new();
        let kick = lab.rows(json!({"kind": "item", "parent": "r:track:{BBBB}"}));
        assert_eq!(kick.len(), 2);
        assert_eq!(kick[0]["ref"], "r:item:{ITEM-K1}");
        assert_eq!((&kick[0]["position"], &kick[0]["length"], &kick[0]["name"]), (&json!(0.0), &json!(2.0), &json!("kick.wav")));
        assert_eq!(
            (&kick[0]["source"], &kick[0]["takeRef"], &kick[0]["trackRef"]),
            (&json!("wav"), &json!("r:take:{TAKE-K1}"), &json!("r:track:{BBBB}"))
        );
        assert_eq!(
            (&kick[0]["muted"], &kick[0]["lane"], &kick[0]["color"], &kick[0]["selected"]),
            (&json!(false), &json!(0), &Value::Null, &json!(false))
        );
        // Without a parent: every item, in track order, paged by a cursor that names track and item.
        let first = lab.read(json!({"kind": "item", "limit": 2})).unwrap();
        assert_eq!(
            first["items"].as_array().unwrap().iter().map(|r| r["ref"].as_str().unwrap()).collect::<Vec<_>>(),
            ["r:item:{ITEM-K1}", "r:item:{ITEM-K2}"]
        );
        let rest = lab.read(json!({"kind": "item", "limit": 2, "cursor": first["nextCursor"]})).unwrap();
        assert_eq!(rest["items"][0]["ref"], "r:item:{ITEM-S1}");
        assert_eq!(rest["items"][0]["source"], "mid");
        assert_eq!(rest.get("nextCursor"), None);
    }

    #[test]
    fn an_item_without_a_take_is_empty() {
        let lab = Lab::new();
        fake::with(|s| s.tracks[1].items[0].takes.clear());
        let row = &lab.rows(json!({"kind": "item", "parent": "r:track:{BBBB}"}))[0];
        assert_eq!((&row["source"], &row["name"], &row["takeRef"]), (&json!("empty"), &json!(""), &Value::Null));
    }

    #[test]
    fn takes_of_an_item() {
        let lab = Lab::new();
        let takes = lab.rows(json!({"kind": "take", "parent": "r:item:{ITEM-S1}"}));
        assert_eq!(takes.len(), 1);
        assert_eq!(
            (&takes[0]["ref"], &takes[0]["source"], &takes[0]["active"], &takes[0]["noteCount"]),
            (&json!("r:take:{TAKE-S1}"), &json!("mid"), &json!(true), &json!(2))
        );
    }

    #[test]
    fn fx_chains_list_containers_with_their_children_and_containers_list_their_own() {
        let lab = Lab::new();
        let chain = lab.rows(json!({"kind": "fx", "parent": "r:track:{AAAA}"}));
        assert_eq!(chain.len(), 2);
        assert_eq!(chain[0]["ref"], "r:fx:{FX-EQ}");
        assert_eq!(
            (&chain[0]["name"], &chain[0]["enabled"], &chain[0]["preset"], &chain[0]["paramCount"], &chain[0]["container"]),
            (&json!("VST3: ReaEQ (Cockos)"), &json!(true), &json!("Warm"), &json!(3), &json!(false))
        );
        assert_eq!(chain[0].get("childRefs"), None);
        assert_eq!((&chain[1]["container"], &chain[1]["childRefs"]), (&json!(true), &json!(["r:fx:{FX-COMP}", "r:fx:{FX-C2}"])));
        let children = lab.rows(json!({"kind": "fx", "parent": "r:fx:{FX-C}"}));
        assert_eq!(children.iter().map(|r| r["ref"].as_str().unwrap()).collect::<Vec<_>>(), ["r:fx:{FX-COMP}", "r:fx:{FX-C2}"]);
        let nested = lab.rows(json!({"kind": "fx", "parent": "r:fx:{FX-C2}"}));
        assert_eq!(nested[0]["ref"], "r:fx:{FX-DLY}");
        assert_eq!(nested[0]["container"], false);
    }

    #[test]
    fn the_fx_lookup_survives_a_reorder_and_says_so_when_the_fx_is_gone() {
        let lab = Lab::new();
        assert_eq!(lab.rows(json!({"kind": "param", "parent": "r:fx:{FX-EQ}"})).len(), 3);
        // The chain is reordered: the cached address no longer holds that FX.
        fake::with(|s| s.tracks[0].fx.reverse());
        assert_eq!(lab.rows(json!({"kind": "param", "parent": "r:fx:{FX-EQ}"}))[0]["ref"], "r:param:{FX-EQ}:0");
        fake::with(|s| s.tracks[0].fx.clear());
        let (code, message) = lab.read(json!({"kind": "param", "parent": "r:fx:{FX-EQ}"})).unwrap_err();
        assert_eq!(code, "failed");
        assert!(message.contains("isn't in the project"), "{message}");
    }

    #[test]
    fn take_fx_are_found_by_guid_too() {
        let lab = Lab::new();
        fake::with(|s| s.tracks[1].items[1].takes[0].fx.push(crate::reaper::fake::FakeFx::new("{FX-TAKE}", "JS: Volume")));
        let rows = lab.rows(json!({"kind": "fx", "parent": "r:take:{TAKE-K2}"}));
        assert_eq!(rows[0]["ref"], "r:fx:{FX-TAKE}");
        assert_eq!(lab.rows(json!({"kind": "param", "parent": "r:fx:{FX-TAKE}"})).len(), 0);
    }

    #[test]
    fn parameters_have_their_value_text_and_range() {
        let lab = Lab::new();
        let params = lab.rows(json!({"kind": "param", "parent": "r:fx:{FX-EQ}"}));
        assert_eq!(
            params[1],
            json!({"ref": "r:param:{FX-EQ}:1", "index": 1, "name": "Band 1 Gain", "value": 0.5, "formatted": "0.0 dB", "min": 0.0, "max": 1.0})
        );
        let page = lab.read(json!({"kind": "param", "parent": "r:fx:{FX-EQ}", "limit": 2})).unwrap();
        assert_eq!((&page["total"], page["nextCursor"].is_string()), (&json!(3), true));
    }

    #[test]
    fn sends_receives_and_hardware_outputs_are_rows_of_the_track() {
        let lab = Lab::new();
        let out = lab.rows(json!({"kind": "send", "parent": "r:track:{AAAA}"}));
        assert_eq!(out.len(), 1);
        assert_eq!(out[0]["ref"], "r:send:{AAAA}:0:0");
        assert_eq!(
            (&out[0]["destRef"], &out[0]["trackRef"], &out[0]["direction"]),
            (&json!("r:track:{DDDD}"), &json!("r:track:{AAAA}"), &json!("send"))
        );
        assert_eq!(
            (&out[0]["volumeDb"], &out[0]["mode"], &out[0]["modeName"], &out[0]["mute"]),
            (&json!(0.0), &json!(0), &json!("post-fader"), &json!(false))
        );
        assert_eq!(out[0]["channels"], json!({"src": {"first": 0, "count": 2}, "dst": {"first": 0, "mono": false}}));
        let reverb = lab.rows(json!({"kind": "send", "parent": "r:track:{DDDD}"}));
        assert_eq!(reverb.iter().map(|r| r["ref"].as_str().unwrap()).collect::<Vec<_>>(), ["r:send:{DDDD}:-1:0", "r:send:{DDDD}:1:0"]);
        // A receive's other track is where it comes from.
        assert_eq!(
            (&reverb[0]["destRef"], &reverb[0]["trackRef"], &reverb[0]["direction"]),
            (&json!("r:track:{AAAA}"), &json!("r:track:{DDDD}"), &json!("receive"))
        );
        assert_eq!((&reverb[1]["destRef"], &reverb[1]["category"], &reverb[1]["direction"]), (&Value::Null, &json!(1), &json!("hardware")));
        assert_eq!(reverb[1]["channels"]["dst"]["first"], 2);
    }

    #[test]
    fn send_channels_and_modes_are_decoded() {
        let lab = Lab::new();
        fake::with(|s| {
            s.tracks[0].sends[0].mode = 3;
            s.tracks[0].sends[0].src_chan = 2 << 10 | 2;
            s.tracks[0].sends[0].dst_chan = 2 | 1024;
            s.tracks[0].sends[0].volume = 0.25;
        });
        let row = &lab.rows(json!({"kind": "send", "parent": "r:track:{AAAA}"}))[0];
        assert_eq!((&row["mode"], &row["modeName"], &row["volumeDb"]), (&json!(3), &json!("pre-fader"), &json!(-12.04)));
        assert_eq!(row["channels"], json!({"src": {"first": 2, "count": 4}, "dst": {"first": 2, "mono": true}}));
        fake::with(|s| s.tracks[0].sends[0].src_chan = -1);
        assert_eq!(lab.rows(json!({"kind": "send", "parent": "r:track:{AAAA}"}))[0]["channels"]["src"], Value::Null);
    }

    #[test]
    fn markers_and_regions_are_read_apart_with_the_numbers_reaper_shows() {
        let lab = Lab::new();
        let markers = lab.rows(json!({"kind": "marker"}));
        assert_eq!(markers, [json!({"ref": "r:marker:1", "number": 1, "name": "Drop", "position": 4.0, "color": Value::Null})]);
        let regions = lab.rows(json!({"kind": "region"}));
        assert_eq!(regions, [json!({"ref": "r:region:1", "number": 1, "name": "Verse", "position": 8.0, "end": 16.0, "color": "#00ff00"})]);
    }

    #[test]
    fn notes_are_in_quarter_notes_from_the_start_of_the_item() {
        let lab = Lab::new();
        // The item starts at 8 s; at 120 bpm that is quarter note 16.
        let notes = lab.rows(json!({"kind": "note", "parent": "r:take:{TAKE-S1}"}));
        assert_eq!(
            notes[0],
            json!({"index": 0, "pitch": 38, "start": 0.0, "length": 0.5, "velocity": 100, "channel": 1, "selected": false, "muted": false})
        );
        assert_eq!(
            (&notes[1]["start"], &notes[1]["length"], &notes[1]["channel"], &notes[1]["selected"]),
            (&json!(1.0), &json!(0.5), &json!(10), &json!(true))
        );
        // The item works as a parent too (its active take).
        assert_eq!(lab.rows(json!({"kind": "note", "parent": "r:item:{ITEM-S1}"})).len(), 2);
        let (code, message) = lab.read(json!({"kind": "note", "parent": "r:take:{TAKE-K1}"})).unwrap_err();
        assert_eq!(code, "bad-params");
        assert!(message.contains("isn't MIDI"), "{message}");
    }

    #[test]
    fn envelopes_are_named_by_chunk_or_by_fx_and_parameter() {
        let lab = Lab::new();
        let envelopes = lab.rows(json!({"kind": "envelope", "parent": "r:track:{AAAA}"}));
        assert_eq!(
            envelopes.iter().map(|r| r["ref"].as_str().unwrap()).collect::<Vec<_>>(),
            ["r:env:{AAAA}:VOLENV2", "r:env:{AAAA}:{FX-EQ}:1"]
        );
        assert_eq!(
            (&envelopes[0]["name"], &envelopes[0]["pointCount"], &envelopes[0]["armed"], &envelopes[0]["visible"]),
            (&json!("Volume"), &json!(4), &json!(false), &json!(true))
        );
        // An FX lists its own.
        let of_fx = lab.rows(json!({"kind": "envelope", "parent": "r:fx:{FX-EQ}"}));
        assert_eq!(of_fx.len(), 1);
        assert_eq!(of_fx[0]["ref"], "r:env:{AAAA}:{FX-EQ}:1");
        // And a ref reads back to the same row.
        assert_eq!(lab.rows(json!({"ref": "r:env:{AAAA}:VOLENV2"}))[0], envelopes[0]);
        assert_eq!(lab.rows(json!({"ref": "r:env:{AAAA}:{FX-EQ}:1"}))[0], envelopes[1]);
    }

    #[test]
    fn an_envelope_nothing_names_falls_back_to_its_position() {
        let lab = Lab::new();
        fake::with(|s| {
            s.tracks[1].envelopes.push(crate::reaper::fake::FakeEnvelope {
                chunk: None,
                fx_param: None,
                ..crate::reaper::fake::FakeEnvelope::own("Mystery", "", 1)
            })
        });
        let envelopes = lab.rows(json!({"kind": "envelope", "parent": "r:track:{BBBB}"}));
        assert_eq!(envelopes[0]["ref"], "r:env:{BBBB}:i0");
        assert_eq!(lab.rows(json!({"ref": "r:env:{BBBB}:i0"}))[0]["name"], "Mystery");
    }

    #[test]
    fn one_object_by_its_ref() {
        let lab = Lab::new();
        for (reference, field, expected) in [
            ("r:track:{CCCC}", "name", json!("Snare")),
            ("r:track:master", "master", json!(true)),
            ("r:item:{ITEM-K2}", "position", json!(2.0)),
            ("r:take:{TAKE-K1}", "name", json!("kick.wav")),
            ("r:fx:{FX-DLY}", "name", json!("VST: ReaDelay (Cockos)")),
            ("r:param:{FX-EQ}:2", "name", json!("Wet")),
            ("r:send:{DDDD}:-1:0", "category", json!(-1)),
            ("r:marker:1", "name", json!("Drop")),
            ("r:region:1", "end", json!(16.0)),
        ] {
            let rows = lab.rows(json!({"ref": reference}));
            assert_eq!(rows.len(), 1, "{reference}");
            assert_eq!(rows[0][field], expected, "{reference}");
        }
    }

    #[test]
    fn a_ref_to_something_gone_or_never_there_fails_and_a_malformed_one_is_bad_params() {
        let lab = Lab::new();
        for reference in [
            "r:track:{NOPE}",
            "r:item:{NOPE}",
            "r:take:{NOPE}",
            "r:fx:{NOPE}",
            "r:param:{FX-EQ}:99",
            "r:send:{AAAA}:0:5",
            "r:marker:77",
            "r:region:77",
            "r:env:{AAAA}:PANENV",
        ] {
            let (code, message) = lab.read(json!({"ref": reference})).unwrap_err();
            assert_eq!(code, "failed", "{reference}: {message}");
        }
        for reference in ["track:{AAAA}", "r:pattern:3", "r:track:", "r:param:{FX-EQ}:x"] {
            assert_eq!(lab.read(json!({"ref": reference})).unwrap_err().0, "bad-params", "{reference}");
        }
    }

    #[test]
    fn requests_that_name_no_kind_or_the_wrong_parent_say_what_to_pass() {
        let lab = Lab::new();
        assert!(lab.read(json!({})).unwrap_err().1.contains("kind"));
        assert!(lab.read(json!({"kind": "pattern"})).unwrap_err().1.contains("no kind"));
        assert!(lab.read(json!({"kind": "param"})).unwrap_err().1.contains("pass the FX as parent"));
        assert!(lab.read(json!({"kind": "fx"})).unwrap_err().1.contains("parent"));
        assert!(lab.read(json!({"kind": "send", "parent": "r:fx:{FX-EQ}"})).unwrap_err().1.contains("track ref"));
        assert!(lab.read(json!({"kind": "item", "parent": "r:take:{TAKE-K1}"})).unwrap_err().1.contains("track ref"));
        assert!(lab.read(json!({"kind": 3})).unwrap_err().1.contains("kind"));
        assert!(lab.read(json!({"kind": "track", "parent": ""})).unwrap_err().1.contains("parent"));
    }

    #[test]
    fn a_track_that_was_deleted_is_not_confused_with_the_one_that_took_its_place() {
        let lab = Lab::new();
        assert_eq!(lab.rows(json!({"ref": "r:track:{CCCC}"}))[0]["name"], "Snare");
        // Deleting Kick moves Snare to index 1: the old handle points at index 2, which is now Reverb.
        fake::with(|s| {
            s.tracks.remove(1);
        });
        assert_eq!(lab.rows(json!({"ref": "r:track:{CCCC}"}))[0]["index"], 2);
        assert_eq!(lab.read(json!({"ref": "r:track:{BBBB}"})).unwrap_err().0, "failed");
        // An item whose track is gone is gone.
        assert_eq!(lab.read(json!({"ref": "r:item:{ITEM-K1}"})).unwrap_err().0, "failed");
    }

    #[test]
    fn a_ref_that_stays_missing_does_not_walk_the_whole_project_each_time() {
        let lab = Lab::new();
        fake::with(|s| s.calls = 0);
        for _ in 0..2 {
            assert!(lab.read(json!({"ref": "r:item:{NOPE}"})).is_err());
        }
        let after_two = fake::with(|s| s.calls);
        for _ in 0..5 {
            assert!(lab.read(json!({"ref": "r:item:{NOPE}"})).is_err());
        }
        let after_seven = fake::with(|s| s.calls);
        // The first miss walked the items; the rest answered from that.
        assert!(after_seven - after_two < 20 * 5, "{after_two} then {after_seven}");
    }

    #[test]
    fn a_page_never_passes_the_frame_bound() {
        let lab = Lab::new();
        // Names at the clip: 200 rows of ~1 KiB, ~220 KiB of rows alone.
        let long = "x".repeat(2000);
        fake::with(|s| s.tracks = (0..300).map(|i| FakeTrack::new(&format!("{{T{i}}}"), &long)).collect());
        let answer = lab.read(json!({"kind": "track", "limit": 200})).unwrap();
        let size = serde_json::to_string(&answer).unwrap().len();
        assert!(size <= 256 * 1024, "{size}");
        let rows = answer["items"].as_array().unwrap().len();
        assert!(rows < 200 && rows > 100, "{rows}");
        // Every track is still reachable by following the cursors.
        let mut seen = rows;
        let mut cursor = answer["nextCursor"].clone();
        while cursor.is_string() {
            let page = lab.read(json!({"kind": "track", "limit": 200, "cursor": cursor})).unwrap();
            assert!(serde_json::to_string(&page).unwrap().len() <= 256 * 1024);
            seen += page["items"].as_array().unwrap().len();
            cursor = page["nextCursor"].clone();
        }
        assert_eq!(seen, 301);
        // A name past the clip is cut with an ellipsis.
        assert!(answer["items"][1]["name"].as_str().unwrap().ends_with('…'));
    }

    #[test]
    fn the_lookups_stay_cheap_in_a_big_project() {
        fake::lab_project();
        fake::with(|s| {
            s.tracks = (0..200)
                .map(|t| {
                    let mut track = FakeTrack::new(&format!("{{T{t}}}"), &format!("Track {t}"));
                    track.items = (0..10)
                        .map(|i| {
                            fake::FakeItem::new(
                                &format!("{{I{t}-{i}}}"),
                                i as f64,
                                1.0,
                                vec![fake::FakeTake::wave(&format!("{{K{t}-{i}}}"), "x")],
                            )
                        })
                        .collect();
                    track
                })
                .collect();
        });
        let reads = Reads::default();
        let api = fake::api();
        let reaper = Reaper::new(&api);
        let ask = |reference: &str| reads.read(&reaper, &json!({"ref": reference})).unwrap();
        // The first item lookup walks all 2,000 items; the ones after it are one validation and one GUID read.
        assert_eq!(ask("r:item:{I199-9}")["items"][0]["position"], 9.0);
        fake::with(|s| s.calls = 0);
        for t in [0, 57, 199] {
            ask(&format!("r:item:{{I{t}-3}}"));
        }
        let calls = fake::with(|s| s.calls);
        assert!(calls < 3 * 40, "{calls} calls for three cached items");
    }

    fn snapshot(lab: &Lab) -> Answer {
        let api = fake::api();
        lab.reads.snapshot(&Reaper::new(&api))
    }

    #[test]
    fn the_snapshot_is_the_project_on_one_screen() {
        let lab = Lab::new();
        fake::select_tracks(&[1]);
        fake::with(|s| {
            s.cursor = 2.0;
            s.play_bits = 1;
            s.play_position = 6.0;
            s.tracks[1].items[0].selected = true;
        });
        let snapshot = snapshot(&lab).unwrap();
        assert_eq!(snapshot["epoch"], lab.reads.epoch());
        assert_eq!(snapshot["project"]["name"], "Song.rpp");
        let transport = &snapshot["transport"];
        assert_eq!((&transport["playing"], &transport["seconds"], &transport["beat"]), (&json!(true), &json!(6.0), &json!(12.0)));
        assert_eq!((&transport["bar"], &transport["beatInBar"], &transport["beatsPerBar"]), (&json!(4), &json!(1.0), &json!(4.0)));
        assert_eq!((&transport["tempo"], &transport["timeSignature"]), (&json!(120.0), &json!([4, 4])));
        assert_eq!(transport["loop"], json!({"on": false, "start": 0.0, "end": 0.0}));
        let tracks = snapshot["tracks"].as_array().unwrap();
        assert_eq!(tracks.len(), 5);
        assert_eq!(tracks[0]["ref"], "r:track:master");
        let selection = &snapshot["selection"];
        assert_eq!(
            (&selection["trackRef"], &selection["itemRef"], &selection["cursor"]),
            (&json!("r:track:{BBBB}"), &json!("r:item:{ITEM-K1}"), &json!(2.0))
        );
        assert_eq!(selection["track"], json!({"name": "Kick", "color": Value::Null, "type": "audio"}));
        assert_eq!(selection["itemName"], "kick.wav");
        let markers: Vec<&str> = snapshot["markers"].as_array().unwrap().iter().map(|m| m["ref"].as_str().unwrap()).collect();
        assert_eq!(markers, ["r:marker:1", "r:region:1"]);
        assert_eq!(snapshot.get("truncated"), None);
    }

    #[test]
    fn a_snapshot_of_a_big_project_names_what_it_leaves_for_read() {
        let lab = Lab::new();
        fake::with(|s| {
            s.tracks = (0..320).map(|i| FakeTrack::new(&format!("{{T{i}}}"), "t")).collect();
            s.markers = (0..150)
                .map(|i| fake::FakeMarker { is_region: false, start: i as f64, end: i as f64, name: "m".into(), number: i + 1, color: 0 })
                .collect();
        });
        let snapshot = snapshot(&lab).unwrap();
        assert_eq!(snapshot["tracks"].as_array().unwrap().len(), SNAPSHOT_TRACKS + 1);
        assert_eq!(snapshot["markers"].as_array().unwrap().len(), SNAPSHOT_MARKERS);
        assert_eq!(snapshot["truncated"]["tracks"]["total"], 321);
        assert_eq!(snapshot["truncated"]["markers"]["total"], 150);
        // The cursors go to `read`: the rest of the tracks, and the rest of the markers.
        let rest = lab.read(json!({"kind": "track", "limit": 200, "cursor": snapshot["truncated"]["tracks"]["nextCursor"]})).unwrap();
        assert_eq!(rest["items"].as_array().unwrap().len(), 120);
        assert_eq!(rest["items"][0]["ref"], "r:track:{T200}");
        let more = lab.read(json!({"kind": "marker", "cursor": snapshot["truncated"]["markers"]["nextCursor"]})).unwrap();
        assert_eq!(more["items"].as_array().unwrap().len(), 50);
    }

    #[test]
    fn a_snapshot_stays_inside_the_frame_bound_whatever_the_names() {
        let lab = Lab::new();
        let long = "n".repeat(5000);
        fake::with(|s| {
            s.tracks = (0..260).map(|i| FakeTrack::new(&format!("{{T{i}}}"), &long)).collect();
            s.markers = (0..150)
                .map(|i| fake::FakeMarker {
                    is_region: true,
                    start: i as f64,
                    end: i as f64 + 1.0,
                    name: long.clone(),
                    number: i + 1,
                    color: 0,
                })
                .collect();
        });
        let snapshot = snapshot(&lab).unwrap();
        let size = serde_json::to_string(&snapshot).unwrap().len();
        assert!(size <= 256 * 1024, "{size}");
        assert!(snapshot["truncated"]["tracks"].is_object() && snapshot["truncated"]["markers"].is_object());
    }

    #[test]
    fn no_project_open_is_a_failure_not_a_panic() {
        let lab = Lab::new();
        fake::with(|s| s.project = None);
        assert_eq!(snapshot(&lab).unwrap_err().0, "failed");
        assert_eq!(lab.read(json!({"kind": "track"})).unwrap_err().0, "failed");
    }
}
