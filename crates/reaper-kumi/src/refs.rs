//! Refs and the lookup REAPER lacks. A ref is `r:<kind>:<id>` with the object's GUID, so it holds across reloads; REAPER
//! has no object lookup by GUID (only for takes), so the registry keeps GUID to handle maps, checks each hit with
//! ValidatePtr2 and the object's own GUID, and rebuilds a map when a hit goes stale or a GUID isn't found.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use crate::reaper::{Fx, FxOwner, Item, Project, Reaper, Take, Track, CONTAINER_FX, INPUT_FX};

pub const PREFIX: &str = "r";
pub const MASTER: &str = "master";

/// A track: the master, or by GUID.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrackId {
    Master,
    Guid(String),
}

/// Which envelope of a track.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnvelopeId {
    /// A track's own: the chunk name (`VOLENV2`).
    Chunk(String),
    /// An FX parameter's.
    Param { fx: String, index: i32 },
    /// By position in the track's envelope list, for an envelope neither of the others names.
    Position(i32),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ref {
    Track(TrackId),
    Item(String),
    Take(String),
    Fx(String),
    Param { fx: String, index: i32 },
    Send { track: TrackId, category: i32, index: i32 },
    Marker(i32),
    Region(i32),
    Envelope { track: TrackId, which: EnvelopeId },
}

impl TrackId {
    fn parse(text: &str) -> Result<TrackId, String> {
        match text {
            MASTER => Ok(TrackId::Master),
            guid if looks_like_guid(guid) => Ok(TrackId::Guid(guid.to_string())),
            other => Err(format!("{other:?} isn't a track id: expected a GUID in braces, or master")),
        }
    }

    pub fn text(&self) -> &str {
        match self {
            TrackId::Master => MASTER,
            TrackId::Guid(guid) => guid,
        }
    }
}

/// REAPER writes a GUID as `{8-4-4-4-12 hex}`; the bridge only checks the braces, which is all a ref's grammar needs.
fn looks_like_guid(text: &str) -> bool {
    text.len() >= 3 && text.len() <= 64 && text.starts_with('{') && text.ends_with('}') && !text.contains(':')
}

fn number(text: &str, what: &str) -> Result<i32, String> {
    text.parse::<i32>().map_err(|_| format!("{text:?} isn't {what}"))
}

fn guid_of(text: &str, what: &str) -> Result<String, String> {
    if looks_like_guid(text) {
        Ok(text.to_string())
    } else {
        Err(format!("{text:?} isn't {what}: expected a GUID in braces"))
    }
}

impl Ref {
    pub fn parse(text: &str) -> Result<Ref, String> {
        let mut parts = text.split(':');
        if parts.next() != Some(PREFIX) {
            return Err(format!("{text:?} isn't a REAPER ref: they look like r:track:{{GUID}}"));
        }
        let kind = parts.next().ok_or_else(|| format!("{text:?} names no kind"))?;
        let rest: Vec<&str> = parts.collect();
        let bad = || format!("{text:?} isn't a well-formed r:{kind} ref");
        Ok(match (kind, rest.as_slice()) {
            ("track", [id]) => Ref::Track(TrackId::parse(id)?),
            ("item", [guid]) => Ref::Item(guid_of(guid, "an item id")?),
            ("take", [guid]) => Ref::Take(guid_of(guid, "a take id")?),
            ("fx", [guid]) => Ref::Fx(guid_of(guid, "an FX id")?),
            ("param", [guid, index]) => Ref::Param { fx: guid_of(guid, "an FX id")?, index: number(index, "a parameter index")? },
            ("send", [track, category, index]) => Ref::Send {
                track: TrackId::parse(track)?,
                category: number(category, "a send category")?,
                index: number(index, "a send index")?,
            },
            ("marker", [n]) => Ref::Marker(number(n, "a marker number")?),
            ("region", [n]) => Ref::Region(number(n, "a region number")?),
            ("env", [track, name]) => {
                let which = match name.strip_prefix('i').and_then(|n| n.parse::<i32>().ok()) {
                    Some(position) => EnvelopeId::Position(position),
                    None if !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') => {
                        EnvelopeId::Chunk(name.to_string())
                    }
                    None => return Err(bad()),
                };
                Ref::Envelope { track: TrackId::parse(track)?, which }
            }
            ("env", [track, fx, index]) => Ref::Envelope {
                track: TrackId::parse(track)?,
                which: EnvelopeId::Param { fx: guid_of(fx, "an FX id")?, index: number(index, "a parameter index")? },
            },
            ("track" | "item" | "take" | "fx" | "param" | "send" | "marker" | "region" | "env", _) => return Err(bad()),
            _ => return Err(format!("{text:?}: REAPER has no ref kind {kind:?}")),
        })
    }

    pub fn kind(&self) -> &'static str {
        match self {
            Ref::Track(_) => "track",
            Ref::Item(_) => "item",
            Ref::Take(_) => "take",
            Ref::Fx(_) => "fx",
            Ref::Param { .. } => "param",
            Ref::Send { .. } => "send",
            Ref::Marker(_) => "marker",
            Ref::Region(_) => "region",
            Ref::Envelope { .. } => "env",
        }
    }

    pub fn text(&self) -> String {
        match self {
            Ref::Track(id) => track_ref(id),
            Ref::Item(guid) => item_ref(guid),
            Ref::Take(guid) => take_ref(guid),
            Ref::Fx(guid) => fx_ref(guid),
            Ref::Param { fx, index } => param_ref(fx, *index),
            Ref::Send { track, category, index } => send_ref(track, *category, *index),
            Ref::Marker(n) => marker_ref(*n),
            Ref::Region(n) => region_ref(*n),
            Ref::Envelope { track, which } => envelope_ref(track, which),
        }
    }
}

pub fn track_ref(id: &TrackId) -> String {
    format!("{PREFIX}:track:{}", id.text())
}
pub fn item_ref(guid: &str) -> String {
    format!("{PREFIX}:item:{guid}")
}
pub fn take_ref(guid: &str) -> String {
    format!("{PREFIX}:take:{guid}")
}
pub fn fx_ref(guid: &str) -> String {
    format!("{PREFIX}:fx:{guid}")
}
pub fn param_ref(fx_guid: &str, index: i32) -> String {
    format!("{PREFIX}:param:{fx_guid}:{index}")
}
pub fn send_ref(track: &TrackId, category: i32, index: i32) -> String {
    format!("{PREFIX}:send:{}:{category}:{index}", track.text())
}
pub fn marker_ref(number: i32) -> String {
    format!("{PREFIX}:marker:{number}")
}
pub fn region_ref(number: i32) -> String {
    format!("{PREFIX}:region:{number}")
}
pub fn envelope_ref(track: &TrackId, which: &EnvelopeId) -> String {
    match which {
        EnvelopeId::Chunk(name) => format!("{PREFIX}:env:{}:{name}", track.text()),
        EnvelopeId::Param { fx, index } => format!("{PREFIX}:env:{}:{fx}:{index}", track.text()),
        EnvelopeId::Position(position) => format!("{PREFIX}:env:{}:i{position}", track.text()),
    }
}

/// How long a map that was just rebuilt without finding a GUID is believed: a ref that isn't there stays missing until
/// the project's state changes or this passes, so a caller polling a dead ref doesn't walk the whole project each time.
const MISS_MEMORY: Duration = Duration::from_millis(500);

/// GUID to handle, per project tab.
#[derive(Default)]
pub struct Registry {
    project: usize,
    tracks: HashMap<String, Track>,
    items: HashMap<String, Item>,
    fx: HashMap<String, Fx>,
    /// When the item map and the FX map were last rebuilt, and the project's state change count then.
    items_built: Option<(i32, Instant)>,
    fx_built: Option<(i32, Instant)>,
}

impl Registry {
    /// Forgets everything: a tab switch, or a new project in the tab.
    pub fn clear(&mut self) {
        *self = Registry::default();
    }

    fn follow(&mut self, proj: Project) {
        if self.project != proj.addr() {
            self.clear();
            self.project = proj.addr();
        }
    }

    fn recently_missed(built: Option<(i32, Instant)>, count: i32) -> bool {
        built.is_some_and(|(at, when)| at == count && when.elapsed() < MISS_MEMORY)
    }

    /// The track for an id; the master by name, any other by GUID.
    pub fn track(&mut self, reaper: &Reaper, proj: Project, id: &TrackId) -> Option<Track> {
        let guid = match id {
            TrackId::Master => return reaper.master_track(proj),
            TrackId::Guid(guid) => guid,
        };
        self.follow(proj);
        if let Some(&track) = self.tracks.get(guid) {
            if reaper.track_valid(proj, track) && reaper.track_guid(track) == *guid {
                return Some(track);
            }
        }
        // A project's tracks are few: walk them all on a miss rather than trust a count that may not have moved.
        self.tracks.clear();
        for index in 0..reaper.track_count(proj) {
            if let Some(track) = reaper.track_at(proj, index) {
                self.tracks.insert(reaper.track_guid(track), track);
            }
        }
        self.tracks.get(guid).copied()
    }

    /// A track's id: the master by name, any other by GUID. Remembers the track.
    pub fn track_id(&mut self, reaper: &Reaper, proj: Project, track: Track) -> TrackId {
        if reaper.track_value(track, "IP_TRACKNUMBER") < 0.0 {
            return TrackId::Master;
        }
        self.follow(proj);
        let guid = reaper.track_guid(track);
        self.tracks.insert(guid.clone(), track);
        TrackId::Guid(guid)
    }

    /// The item for a GUID.
    pub fn item(&mut self, reaper: &Reaper, proj: Project, guid: &str) -> Option<Item> {
        self.follow(proj);
        if let Some(&item) = self.items.get(guid) {
            if reaper.item_valid(proj, item) && reaper.item_guid(item) == guid {
                return Some(item);
            }
        }
        let count = reaper.state_change_count(proj).unwrap_or(0);
        if Self::recently_missed(self.items_built, count) {
            return None;
        }
        self.items.clear();
        for track in (0..reaper.track_count(proj)).filter_map(|i| reaper.track_at(proj, i)).chain(reaper.master_track(proj)) {
            for index in 0..reaper.track_item_count(track) {
                if let Some(item) = reaper.track_item(track, index) {
                    self.items.insert(reaper.item_guid(item), item);
                }
            }
        }
        self.items_built = Some((count, Instant::now()));
        self.items.get(guid).copied()
    }

    /// Remembers an item REAPER just handed out, so a ref to it resolves without a walk.
    pub fn note_item(&mut self, proj: Project, guid: &str, item: Item) {
        self.follow(proj);
        self.items.insert(guid.to_string(), item);
    }

    /// The take for a GUID: REAPER looks these up itself.
    pub fn take(&mut self, reaper: &Reaper, proj: Project, guid: &str) -> Option<Take> {
        reaper.take_by_guid(proj, guid).filter(|&take| reaper.take_valid(proj, take))
    }

    /// The FX for a GUID, wherever it is: a track's chain, its input chain, a container, or a take's chain.
    pub fn fx(&mut self, reaper: &Reaper, proj: Project, guid: &str) -> Option<Fx> {
        self.follow(proj);
        if let Some(&fx) = self.fx.get(guid) {
            if self.fx_matches(reaper, proj, fx, guid) {
                return Some(fx);
            }
            // The chain was reordered: the same owner's chain is the first place it can have gone.
            self.index_chain(reaper, fx.owner);
            if let Some(&fx) = self.fx.get(guid) {
                if self.fx_matches(reaper, proj, fx, guid) {
                    return Some(fx);
                }
            }
        }
        let count = reaper.state_change_count(proj).unwrap_or(0);
        if Self::recently_missed(self.fx_built, count) {
            return None;
        }
        self.fx.clear();
        // Tracks first: nearly every FX is on one. Takes only when the track chains didn't have it.
        let tracks: Vec<Track> =
            (0..reaper.track_count(proj)).filter_map(|i| reaper.track_at(proj, i)).chain(reaper.master_track(proj)).collect();
        for &track in &tracks {
            self.index_chain(reaper, FxOwner::Track(track));
        }
        if !self.fx.contains_key(guid) {
            for &track in &tracks {
                for index in 0..reaper.track_item_count(track) {
                    let Some(item) = reaper.track_item(track, index) else { continue };
                    for n in 0..reaper.take_count(item) {
                        if let Some(take) = reaper.take_at(item, n) {
                            self.index_chain(reaper, FxOwner::Take(take));
                        }
                    }
                }
            }
        }
        self.fx_built = Some((count, Instant::now()));
        self.fx.get(guid).copied()
    }

    fn fx_matches(&self, reaper: &Reaper, proj: Project, fx: Fx, guid: &str) -> bool {
        let owner_valid = match fx.owner {
            FxOwner::Track(track) => reaper.track_valid(proj, track),
            FxOwner::Take(take) => reaper.take_valid(proj, take),
        };
        owner_valid && reaper.fx_guid(fx) == guid
    }

    /// Remembers every FX in an owner's chain, containers' children and a track's input chain included.
    pub fn index_chain(&mut self, reaper: &Reaper, owner: FxOwner) {
        for index in 0..reaper.fx_count(owner) {
            self.index_fx(reaper, Fx { owner, index });
        }
        if let FxOwner::Track(track) = owner {
            for index in 0..reaper.input_fx_count(track) {
                self.index_fx(reaper, Fx { owner, index: INPUT_FX | index });
            }
        }
    }

    fn index_fx(&mut self, reaper: &Reaper, fx: Fx) {
        let guid = reaper.fx_guid(fx);
        if guid.is_empty() {
            return;
        }
        self.fx.insert(guid, fx);
        for child in reaper.fx_children(fx) {
            debug_assert!(child.index & CONTAINER_FX != 0);
            self.index_fx(reaper, child);
        }
    }

    /// Remembers an FX REAPER just handed out.
    pub fn note_fx(&mut self, proj: Project, guid: &str, fx: Fx) {
        self.follow(proj);
        self.fx.insert(guid.to_string(), fx);
    }
}

/// The epoch: changes when refs stop meaning what they meant (a tab switch, a new project in the tab). Distinct for each
/// run of REAPER, so a cursor from before a restart is refused.
pub struct Epoch {
    session: u64,
    serial: u64,
}

impl Epoch {
    pub fn new() -> Epoch {
        let session = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0);
        Epoch { session, serial: 1 }
    }

    pub fn text(&self) -> String {
        format!("{:x}-{}", self.session, self.serial)
    }

    pub fn bump(&mut self) {
        self.serial += 1;
    }
}

impl Default for Epoch {
    fn default() -> Self {
        Epoch::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refs_round_trip_through_their_text() {
        let guid = "{1A2B3C4D-0000-1111-2222-333344445555}";
        let fx = "{AAAAAAAA-0000-1111-2222-333344445555}";
        let track = TrackId::Guid(guid.into());
        for reference in [
            Ref::Track(TrackId::Master),
            Ref::Track(track.clone()),
            Ref::Item(guid.into()),
            Ref::Take(guid.into()),
            Ref::Fx(fx.into()),
            Ref::Param { fx: fx.into(), index: 7 },
            Ref::Send { track: track.clone(), category: -1, index: 2 },
            Ref::Marker(3),
            Ref::Region(12),
            Ref::Envelope { track: track.clone(), which: EnvelopeId::Chunk("VOLENV2".into()) },
            Ref::Envelope { track: track.clone(), which: EnvelopeId::Param { fx: fx.into(), index: 4 } },
            Ref::Envelope { track, which: EnvelopeId::Position(5) },
        ] {
            let text = reference.text();
            assert_eq!(Ref::parse(&text).as_ref(), Ok(&reference), "{text}");
            // The second part of the ref is its kind, as Kumi's book reads it.
            assert_eq!(text.split(':').nth(1), Some(reference.kind()));
        }
    }

    #[test]
    fn malformed_refs_say_what_is_wrong() {
        for (text, needle) in [
            ("track:{A}", "isn't a REAPER ref"),
            ("r:", "kind"),
            ("r:track", "well-formed"),
            ("r:track:nope", "isn't a track id"),
            ("r:item:42", "GUID"),
            ("r:param:{A}:x", "parameter index"),
            ("r:send:{A}:0", "well-formed"),
            ("r:pattern:3", "no ref kind"),
            ("r:env:{A}:", "well-formed"),
            ("r:env:{A}:VOL ENV", "well-formed"),
        ] {
            let error = Ref::parse(text).unwrap_err();
            assert!(error.contains(needle), "{text}: {error}");
        }
    }

    #[test]
    fn the_epoch_changes_on_a_bump_and_differs_between_runs() {
        let mut epoch = Epoch::new();
        let first = epoch.text();
        epoch.bump();
        assert_ne!(epoch.text(), first);
        assert!(epoch.text().starts_with(first.split('-').next().unwrap()));
        let mut other = Epoch { session: epoch.session + 1, serial: 1 };
        other.bump();
        assert_ne!(other.text(), epoch.text());
    }
}
