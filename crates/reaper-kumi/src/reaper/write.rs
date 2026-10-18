//! What the bridge writes to REAPER, and the undo history it names its writes in. Each call is REAPER's own, one for one;
//! a function this REAPER lacks answers false or None, and the change that needed it is refused before it begins.

use std::ffi::{c_char, c_int, CString};
use std::ptr;

use super::{Project, Reaper, Track};
use crate::api::{self, ProjectMarker};

/// The most a track's chunk may grow to when it is read: the plug-in states in it are what take the room.
const MAX_CHUNK: usize = 256 << 20;

fn cstring(text: &str) -> CString {
    CString::new(text.replace('\0', "")).unwrap_or_default()
}

impl Reaper<'_> {
    // ----- tracks -----

    pub fn set_track_value(&self, track: Track, parm: &str, value: f64) -> bool {
        let parm = cstring(parm);
        self.api.SetMediaTrackInfo_Value().is_some_and(|f| unsafe { f(track.0, parm.as_ptr(), value) })
    }

    /// A track's text attribute (`P_NAME`).
    pub fn set_track_string(&self, track: Track, parm: &str, value: &str) -> bool {
        let (parm, value) = (cstring(parm), cstring(value));
        let Some(f) = self.api.GetSetMediaTrackInfo_String() else { return false };
        // The buffer is read, not written, when setting; REAPER wants it writable all the same.
        let mut buf = value.into_bytes_with_nul();
        unsafe { f(track.0, parm.as_ptr(), buf.as_mut_ptr().cast::<c_char>(), true) }
    }

    /// A new track at `index` (0-based; at the end when past the last), without the project's default FX and sends.
    pub fn insert_track(&self, proj: Project, index: i32) -> Option<Track> {
        let at = index.clamp(0, self.track_count(proj));
        unsafe { self.api.InsertTrackAtIndex()?(at, false) };
        self.track_at(proj, at)
    }

    pub fn delete_track(&self, track: Track) -> bool {
        match self.api.DeleteTrack() {
            Some(f) => {
                unsafe { f(track.0) };
                true
            }
            None => false,
        }
    }

    /// The track's whole state as REAPER writes it into a project file, plug-in states and GUIDs included.
    pub fn track_chunk(&self, track: Track) -> Option<String> {
        let f = self.api.GetTrackStateChunk()?;
        let mut size = 1usize << 16;
        loop {
            let mut buf = vec![0u8; size];
            let ok = unsafe { f(track.0, buf.as_mut_ptr().cast::<c_char>(), size as c_int, false) };
            let len = buf.iter().position(|&b| b == 0).unwrap_or(size);
            if !ok {
                return None;
            }
            if len + 1 < size {
                buf.truncate(len);
                return Some(String::from_utf8_lossy(&buf).into_owned());
            }
            if size >= MAX_CHUNK {
                return None;
            }
            size *= 4;
        }
    }

    pub fn set_track_chunk(&self, track: Track, chunk: &str) -> bool {
        let chunk = cstring(chunk);
        self.api.SetTrackStateChunk().is_some_and(|f| unsafe { f(track.0, chunk.as_ptr(), false) })
    }

    /// A colour in the order REAPER stores it (without the flag that says it is set).
    pub fn color_to_native(&self, r: i32, g: i32, b: i32) -> Option<i32> {
        self.api.ColorToNative().map(|f| unsafe { f(r, g, b) })
    }

    /// Selects exactly this track (the master included).
    pub fn select_only(&self, track: Track) {
        if let Some(f) = self.api.SetOnlyTrackSelected() {
            unsafe { f(track.0) }
        }
    }

    pub fn set_track_selected(&self, track: Track, selected: bool) {
        if let Some(f) = self.api.SetTrackSelected() {
            unsafe { f(track.0, selected) }
        }
    }

    /// Moves the selected tracks to just above the track at `before` (counted before the move). `folder` 0 leaves their
    /// folder depths as they are.
    pub fn reorder_selected(&self, before: i32, folder: i32) -> bool {
        self.api.ReorderSelectedTracks().is_some_and(|f| unsafe { f(before, folder) })
    }

    // ----- sends -----

    /// A send from `track` to `dest`, or a hardware output when there is none: its index among the sends of its kind.
    pub fn create_send(&self, track: Track, dest: Option<Track>) -> Option<i32> {
        let index = unsafe { self.api.CreateTrackSend()?(track.0, dest.map_or(ptr::null_mut(), |d| d.0)) };
        (index >= 0).then_some(index)
    }

    pub fn remove_send(&self, track: Track, category: i32, index: i32) -> bool {
        self.api.RemoveTrackSend().is_some_and(|f| unsafe { f(track.0, category, index) })
    }

    pub fn set_send_value(&self, track: Track, category: i32, index: i32, parm: &str, value: f64) -> bool {
        let parm = cstring(parm);
        self.api.SetTrackSendInfo_Value().is_some_and(|f| unsafe { f(track.0, category, index, parm.as_ptr(), value) })
    }

    // ----- the screen -----

    /// Redraws the arrange view and the track list after a change made by script.
    pub fn refresh_after_change(&self) {
        if let Some(f) = self.api.TrackList_AdjustWindows() {
            unsafe { f(false) }
        }
        if let Some(f) = self.api.UpdateArrange() {
            unsafe { f() }
        }
    }

    // ----- the undo history -----

    pub fn undo_begin(&self, proj: Project) -> bool {
        match self.api.Undo_BeginBlock2() {
            Some(f) => {
                unsafe { f(proj.0) };
                true
            }
            None => false,
        }
    }

    /// Ends the block as one undo point named `desc`. `flags` -1 covers every kind of change; 0 makes a point only if
    /// something REAPER itself marked changed.
    pub fn undo_end(&self, proj: Project, desc: &str, flags: i32) -> bool {
        let desc = cstring(desc);
        match self.api.Undo_EndBlock2() {
            Some(f) => {
                unsafe { f(proj.0, desc.as_ptr(), flags) };
                true
            }
            None => false,
        }
    }

    /// The description of the point Undo would take back.
    pub fn undo_can_undo(&self, proj: Project) -> Option<String> {
        let text = unsafe { api::string(self.api.Undo_CanUndo2()?(proj.0)) };
        (!text.is_empty()).then_some(text)
    }

    /// The description of the point Redo would bring back.
    pub fn undo_can_redo(&self, proj: Project) -> Option<String> {
        let text = unsafe { api::string(self.api.Undo_CanRedo2()?(proj.0)) };
        (!text.is_empty()).then_some(text)
    }

    /// The history's entries, and the one the project is at; entry 0 is "--- Initial state ---".
    pub fn undo_position(&self, proj: Project) -> Option<(i32, i32)> {
        let entries = unsafe { self.api.Undo_GetNumEntries()?(proj.0) };
        let at = unsafe { self.api.Undo_GetCurEntry()?(proj.0) };
        Some((entries, at))
    }

    pub fn undo_entry(&self, proj: Project, index: i32) -> Option<String> {
        let text = unsafe { api::string(self.api.Undo_GetEntryDesc()?(proj.0, index)) };
        (!text.is_empty()).then_some(text)
    }
}

/// Markers and regions, which REAPER hands out by pointer and names by GUID.
impl Reaper<'_> {
    fn marker_pointer(&self, proj: Project, guid: &str) -> Option<*mut ProjectMarker> {
        let guid = cstring(guid);
        let marker = unsafe { self.api.GetRegionOrMarker()?(proj.0, -1, guid.as_ptr()) };
        (!marker.is_null()).then_some(marker)
    }

    /// A marker or region made at `start` (seconds), and its GUID. `end` is the region's.
    pub fn add_marker(&self, proj: Project, region: bool, start: f64, end: f64, name: &str, color: i32) -> Option<String> {
        let name = cstring(name);
        let marker = unsafe { self.api.AddRegionOrMarker()?(proj.0, region, start, end, name.as_ptr(), -1, color) };
        if marker.is_null() {
            return None;
        }
        let f = self.api.GetSetRegionOrMarkerInfo_String()?;
        let mut buf = vec![0u8; 256];
        let ok = unsafe { f(proj.0, marker, c"GUID".as_ptr(), buf.as_mut_ptr().cast(), false) };
        let len = buf.iter().position(|&b| b == 0).unwrap_or(0);
        (ok && len > 0).then(|| String::from_utf8_lossy(&buf[..len]).into_owned())
    }

    /// A marker's number attribute: `D_STARTPOS`, `D_ENDPOS`, `I_NUMBER`, `I_CUSTOMCOLOR`.
    pub fn set_marker_value(&self, proj: Project, guid: &str, parm: &str, value: f64) -> bool {
        let (Some(marker), Some(f)) = (self.marker_pointer(proj, guid), self.api.SetRegionOrMarkerInfo_Value()) else { return false };
        let parm = cstring(parm);
        // What it returns is not documented as a success flag: the change reads the marker again.
        unsafe { f(proj.0, marker, parm.as_ptr(), value) };
        true
    }

    pub fn set_marker_name(&self, proj: Project, guid: &str, name: &str) -> bool {
        let (Some(marker), Some(f)) = (self.marker_pointer(proj, guid), self.api.GetSetRegionOrMarkerInfo_String()) else { return false };
        let mut buf = cstring(name).into_bytes_with_nul();
        unsafe { f(proj.0, marker, c"P_NAME".as_ptr(), buf.as_mut_ptr().cast::<c_char>(), true) }
    }

    pub fn delete_marker(&self, proj: Project, guid: &str) -> bool {
        let (Some(marker), Some(value), Some(delete)) =
            (self.marker_pointer(proj, guid), self.api.GetRegionOrMarkerInfo_Value(), self.api.DeleteProjectMarkerByIndex())
        else {
            return false;
        };
        let index = unsafe { value(proj.0, marker, c"I_INDEX".as_ptr()) } as i32;
        unsafe { delete(proj.0, index) }
    }
}
