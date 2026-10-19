//! A fake REAPER for tests: an API table whose functions read and write a thread-local project (tracks, items, takes,
//! FX with containers, sends, envelopes, MIDI, markers). Handles are numbers that say what they point at, and the fake
//! never dereferences one; a handle for something deleted answers like REAPER's `ValidatePtr2`: not valid.

use super::*;
use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::{c_char, c_int, c_void, CStr};

#[derive(Clone)]
pub struct FakeParam {
    pub name: String,
    pub value: f64,
    pub min: f64,
    pub max: f64,
    /// What the plug-in prints for the value.
    pub shown: String,
    /// How it prints other values (`TrackFX_FormatParamValueNormalized`); `shown` for any, if not set.
    pub display: Display,
    /// The formatted value (`TrackFX_GetFormattedParamValue`) leaves the unit off, as ReaEQ's does.
    pub bare: bool,
}

/// How a fake plug-in prints a normalized value.
#[derive(Clone)]
pub enum Display {
    Fixed,
    /// `min` to `max` over the range, with `decimals` and a `unit`.
    Linear {
        min: f64,
        max: f64,
        decimals: usize,
        unit: String,
    },
    /// One of these, evenly over the range.
    Steps(Vec<String>),
}

impl FakeParam {
    pub fn new(name: &str, value: f64, shown: &str) -> Self {
        FakeParam { name: name.into(), value, min: 0.0, max: 1.0, shown: shown.into(), display: Display::Fixed, bare: false }
    }

    pub fn linear(name: &str, value: f64, min: f64, max: f64, decimals: usize, unit: &str) -> Self {
        let mut param = FakeParam::new(name, value, "");
        param.display = Display::Linear { min, max, decimals, unit: unit.into() };
        param.shown = param.text_for(value);
        param
    }

    pub fn steps(name: &str, value: f64, steps: &[&str]) -> Self {
        let mut param = FakeParam::new(name, value, "");
        param.display = Display::Steps(steps.iter().map(|s| s.to_string()).collect());
        param.shown = param.text_for(value);
        param
    }

    /// What the plug-in prints for a value.
    pub fn text_for(&self, value: f64) -> String {
        match &self.display {
            Display::Fixed => self.shown.clone(),
            Display::Linear { min, max, decimals, unit } => {
                format!("{:.*} {unit}", decimals, min + (max - min) * value).trim_end().to_string()
            }
            Display::Steps(steps) => steps[((value * (steps.len() - 1) as f64).round() as usize).min(steps.len() - 1)].clone(),
        }
    }
}

#[derive(Clone)]
pub struct FakeFx {
    pub guid: String,
    pub name: String,
    pub renamed: String,
    pub enabled: bool,
    pub offline: bool,
    pub preset: String,
    pub parallel: i32,
    pub params: Vec<FakeParam>,
    /// Some for a container, even an empty one.
    pub children: Option<Vec<FakeFx>>,
    pub instrument: bool,
}

impl FakeFx {
    pub fn new(guid: &str, name: &str) -> Self {
        FakeFx {
            guid: guid.into(),
            name: name.into(),
            renamed: String::new(),
            enabled: true,
            offline: false,
            preset: String::new(),
            parallel: 0,
            params: vec![],
            children: None,
            instrument: false,
        }
    }

    pub fn container(guid: &str, children: Vec<FakeFx>) -> Self {
        FakeFx { children: Some(children), ..FakeFx::new(guid, "Container") }
    }
}

#[derive(Clone)]
pub struct FakeNote {
    pub start_ppq: f64,
    pub end_ppq: f64,
    pub channel: i32,
    pub pitch: i32,
    pub velocity: i32,
    pub selected: bool,
    pub muted: bool,
}

#[derive(Clone)]
pub struct FakeCc {
    pub ppq: f64,
    pub kind: i32,
    pub channel: i32,
    pub data1: i32,
    pub data2: i32,
}

#[derive(Clone)]
pub struct FakeEnvelope {
    pub name: String,
    /// The envelope's own GUID, as REAPER keeps one for every envelope.
    pub guid: String,
    /// The chunk name (`<VOLENV2`) of a track's own envelope.
    pub chunk: Option<String>,
    /// The FX (by GUID) and parameter an FX envelope belongs to.
    pub fx_param: Option<(String, i32)>,
    /// The send (by index) and chunk name of a send's envelope.
    pub send: Option<(usize, String)>,
    pub points: i32,
    pub armed: bool,
    pub visible: bool,
    /// An inactive envelope is one REAPER hands out for a send that has none yet.
    pub active: bool,
}

impl FakeEnvelope {
    /// A send's own envelope (`<VOLENV`…): the index of the send it belongs to.
    pub fn for_send(name: &str, send: usize, chunk: &str, points: i32) -> Self {
        let mut envelope = FakeEnvelope::own(name, "", points);
        envelope.chunk = None;
        envelope.guid = format!("{{ENV-SEND{send}-{}}}", chunk.trim_start_matches('<'));
        envelope.send = Some((send, chunk.into()));
        envelope
    }

    pub fn own(name: &str, chunk: &str, points: i32) -> Self {
        let guid = format!("{{ENV-{}}}", chunk.trim_start_matches('<'));
        FakeEnvelope {
            name: name.into(),
            guid,
            chunk: Some(chunk.into()),
            fx_param: None,
            send: None,
            points,
            armed: false,
            visible: true,
            active: true,
        }
    }

    pub fn for_param(name: &str, fx: &str, param: i32, points: i32) -> Self {
        let guid = format!("{{ENV-{}-{param}}}", fx.trim_matches(|c| c == '{' || c == '}'));
        FakeEnvelope {
            name: name.into(),
            guid,
            chunk: None,
            fx_param: Some((fx.into(), param)),
            send: None,
            points,
            armed: false,
            visible: true,
            active: true,
        }
    }
}

#[derive(Clone)]
pub struct FakeTake {
    pub guid: String,
    pub name: String,
    /// "WAVE", "MIDI"…; None for a take without a source.
    pub source: Option<String>,
    pub fx: Vec<FakeFx>,
    pub notes: Vec<FakeNote>,
    pub ccs: Vec<FakeCc>,
    pub envelopes: Vec<FakeEnvelope>,
    /// Other `GetMediaItemTakeInfo_Value` attributes (`I_CUSTOMCOLOR`…).
    pub extra: HashMap<String, f64>,
}

impl FakeTake {
    pub fn wave(guid: &str, name: &str) -> Self {
        FakeTake {
            guid: guid.into(),
            name: name.into(),
            source: Some("WAVE".into()),
            fx: vec![],
            notes: vec![],
            ccs: vec![],
            envelopes: vec![],
            extra: HashMap::new(),
        }
    }

    pub fn midi(guid: &str, name: &str) -> Self {
        FakeTake { source: Some("MIDI".into()), ..FakeTake::wave(guid, name) }
    }
}

#[derive(Clone)]
pub struct FakeItem {
    pub guid: String,
    pub position: f64,
    pub length: f64,
    pub mute: bool,
    pub lane: i32,
    pub color: i32,
    pub selected: bool,
    pub takes: Vec<FakeTake>,
    pub active: usize,
    /// Other `GetMediaItemInfo_Value` attributes (`C_LANEPLAYS`…).
    pub extra: HashMap<String, f64>,
}

impl FakeItem {
    pub fn new(guid: &str, position: f64, length: f64, takes: Vec<FakeTake>) -> Self {
        FakeItem {
            guid: guid.into(),
            position,
            length,
            mute: false,
            lane: 0,
            color: 0,
            selected: false,
            takes,
            active: 0,
            extra: HashMap::new(),
        }
    }
}

#[derive(Clone)]
pub struct FakeSend {
    /// The destination track's index (a send), or ignored (a hardware output).
    pub dest: usize,
    pub mode: i32,
    pub volume: f64,
    pub pan: f64,
    pub mute: bool,
    pub src_chan: i32,
    pub dst_chan: i32,
    /// Other `GetTrackSendInfo_Value` attributes (`B_PHASE`, `I_MIDIFLAGS`…); a send's default for those not here.
    pub extra: HashMap<String, f64>,
    /// `P_EXT:…` text a send carries.
    pub ext: HashMap<String, String>,
}

impl FakeSend {
    pub fn to(dest: usize) -> Self {
        FakeSend { dest, mode: 0, volume: 1.0, pan: 0.0, mute: false, src_chan: 0, dst_chan: 0, extra: HashMap::new(), ext: HashMap::new() }
    }
}

#[derive(Clone)]
pub struct FakeTrack {
    pub guid: String,
    pub name: String,
    pub volume: f64,
    pub pan: f64,
    pub mute: bool,
    pub solo: i32,
    pub arm: bool,
    pub selected: bool,
    /// As REAPER stores it: the OS's order with 0x1000000 set; 0 for none.
    pub color: i32,
    pub folder: i32,
    pub main_send: bool,
    pub fx: Vec<FakeFx>,
    pub input_fx: Vec<FakeFx>,
    pub items: Vec<FakeItem>,
    pub sends: Vec<FakeSend>,
    pub hw_outs: Vec<FakeSend>,
    pub envelopes: Vec<FakeEnvelope>,
    /// Other `GetMediaTrackInfo_Value` attributes (`I_PANMODE`, `B_SHOWINTCP`, `C_LANEPLAYS:0`…); a new track's default for
    /// those not here.
    pub extra: HashMap<String, f64>,
    /// `P_LANENAME:n` of a track with fixed lanes.
    pub lane_names: Vec<String>,
}

impl FakeTrack {
    pub fn new(guid: &str, name: &str) -> Self {
        FakeTrack {
            guid: guid.into(),
            name: name.into(),
            volume: 1.0,
            pan: 0.0,
            mute: false,
            solo: 0,
            arm: false,
            selected: false,
            color: 0,
            folder: 0,
            main_send: true,
            fx: vec![],
            input_fx: vec![],
            items: vec![],
            sends: vec![],
            hw_outs: vec![],
            envelopes: vec![],
            extra: HashMap::new(),
            lane_names: vec![],
        }
    }
}

#[derive(Clone)]
pub struct FakeMarker {
    pub guid: String,
    pub is_region: bool,
    pub start: f64,
    pub end: f64,
    pub name: String,
    pub number: i32,
    pub color: i32,
}

#[derive(Clone)]
pub struct FakeTempo {
    pub time: f64,
    pub bpm: f64,
    pub num: i32,
    pub denom: i32,
    pub linear: bool,
}

#[derive(Clone)]
pub struct State {
    pub version: String,
    pub resource: String,
    /// None: no project (REAPER never has none, but the bridge must not care).
    pub project: Option<(String, String)>,
    /// Which project object the active tab holds: a tab switch changes it, a new project in the tab doesn't.
    pub project_id: u64,
    /// The project objects of the other open tabs (by `project_id`).
    pub other_tabs: Vec<u64>,
    pub play_bits: i32,
    pub bpm: f64,
    pub time_signature: (i32, i32),
    pub state_count: i32,
    /// The undo history's entries: one for a project just loaded.
    pub undo_entries: i32,
    /// The descriptions of entries 1 and on (a test's own entries may be missing: they read "Edit n"), and the one the
    /// project is at (None: the last).
    pub undo_descs: Vec<String>,
    pub undo_cur: Option<i32>,
    /// Blocks begun and not ended, and whether anything was written since the last point: a bare edit makes no point
    /// of its own and joins the next block that is ended with flags.
    pub undo_depth: i32,
    pub undo_dirty: bool,
    /// Track states handed out by `GetTrackStateChunk`, by the number in the chunk text.
    pub chunks: Vec<FakeTrack>,
    /// A GUID counter for tracks made by `InsertTrackAtIndex`.
    pub made: u32,
    pub fx_made: u32,
    /// How often the screen was told to redraw.
    pub redraws: u32,
    pub master: FakeTrack,
    pub tracks: Vec<FakeTrack>,
    pub markers: Vec<FakeMarker>,
    pub tempo_markers: Vec<FakeTempo>,
    pub length: f64,
    pub cursor: f64,
    pub play_position: f64,
    pub loop_range: (f64, f64),
    pub time_selection: (f64, f64),
    pub repeat: bool,
    pub arrange_view: (f64, f64),
    /// The take of the MIDI editor with the focus: (track, item, take).
    pub midi_editor: Option<(usize, usize, usize)>,
    /// (track, item, take, fx, param) as GetTouchedOrFocusedFX gives them; track -1 is the master.
    pub focused_fx: Option<(i32, i32, i32, i32, i32)>,
    pub touched_fx: Option<(i32, i32, i32, i32, i32)>,
    pub selected_envelope: Option<(usize, usize)>,
    pub cursor_context: i32,
    /// False while a modal dialog has the main window disabled.
    pub main_enabled: bool,
    /// The title of the window with the keyboard focus (None: the platform can't say).
    pub front_title: Option<String>,
    pub loading: bool,
    pub dirty: bool,
    /// Saving leaves the project changed (a read-only folder, a plug-in refusing).
    pub save_fails: bool,
    pub saved: u32,
    pub actions: Vec<i32>,
    pub ui_refresh: i32,
    /// A project is rendering (what EnumProjects(0x40000000) answers).
    pub rendering: bool,
    /// Called, with no borrow of the state held, as REAPER runs each action: a test's REAPER that quits by running
    /// its atexit hooks inside the quit action.
    pub on_action: Option<fn(i32)>,
    /// What the project carries for extensions: (section, key) to value.
    pub project_ext: std::collections::HashMap<(String, String), String>,
    /// How many REAPER calls the bridge has made (a read's cost).
    pub calls: u64,
}

impl Default for State {
    fn default() -> Self {
        State {
            version: String::new(),
            resource: String::new(),
            project: None,
            project_id: 0,
            other_tabs: vec![],
            play_bits: 0,
            bpm: 0.0,
            time_signature: (4, 4),
            state_count: 0,
            undo_entries: 1,
            undo_descs: vec![],
            undo_cur: None,
            undo_depth: 0,
            undo_dirty: false,
            chunks: vec![],
            made: 0,
            fx_made: 0,
            redraws: 0,
            master: FakeTrack::new("{MASTER}", "MASTER"),
            tracks: vec![],
            markers: vec![],
            tempo_markers: vec![],
            length: 0.0,
            cursor: 0.0,
            play_position: 0.0,
            loop_range: (0.0, 0.0),
            time_selection: (0.0, 0.0),
            repeat: false,
            arrange_view: (0.0, 30.0),
            midi_editor: None,
            focused_fx: None,
            touched_fx: None,
            selected_envelope: None,
            cursor_context: 0,
            main_enabled: true,
            front_title: None,
            loading: false,
            dirty: false,
            save_fails: false,
            saved: 0,
            actions: vec![],
            ui_refresh: 0,
            rendering: false,
            on_action: None,
            project_ext: Default::default(),
            calls: 0,
        }
    }
}

thread_local! {
    pub static STATE: RefCell<State> = RefCell::new(State::default());
}

pub fn with<T>(f: impl FnOnce(&mut State) -> T) -> T {
    STATE.with(|s| f(&mut s.borrow_mut()))
}

/// Selects exactly these tracks (by index).
pub fn select_tracks(indexes: &[usize]) {
    with(|s| {
        for (i, track) in s.tracks.iter_mut().enumerate() {
            track.selected = indexes.contains(&i);
        }
    });
}

// ----- handles -----

const PROJECT_BASE: u64 = 0x10;
const MASTER: u64 = 0xFFFF;
const TRACK: u64 = 1;
const ITEM: u64 = 2;
const TAKE: u64 = 3;
const ENVELOPE: u64 = 4;
const TAKE_ENVELOPE: u64 = 5;
const SOURCE: u64 = 6;
const MIDI_EDITOR: u64 = 0x77;
const MAIN_WINDOW: u64 = 0x99;

fn pack(kind: u64, a: u64, b: u64, c: u64) -> u64 {
    kind << 56 | a << 40 | b << 20 | c
}

fn unpack(handle: u64) -> (u64, u64, u64, u64) {
    (handle >> 56, (handle >> 40) & 0xFFFF, (handle >> 20) & 0xF_FFFF, handle & 0xF_FFFF)
}

pub fn track_ptr(index: usize) -> *mut MediaTrack {
    pack(TRACK, index as u64, 0, 0) as *mut MediaTrack
}

pub fn master_ptr() -> *mut MediaTrack {
    pack(TRACK, MASTER, 0, 0) as *mut MediaTrack
}

fn item_ptr(track: usize, item: usize) -> *mut MediaItem {
    pack(ITEM, track as u64, item as u64, 0) as *mut MediaItem
}

fn take_ptr(track: usize, item: usize, take: usize) -> *mut MediaItem_Take {
    pack(TAKE, track as u64, item as u64, take as u64) as *mut MediaItem_Take
}

fn project_ptr(s: &State) -> *mut ReaProject {
    (PROJECT_BASE + s.project_id * 16) as *mut ReaProject
}

fn track_of(s: &State, handle: u64) -> Option<&FakeTrack> {
    let (kind, a, _, _) = unpack(handle);
    match (kind, a) {
        (TRACK, MASTER) => Some(&s.master),
        (TRACK, a) => s.tracks.get(a as usize),
        _ => None,
    }
}

fn item_of(s: &State, handle: u64) -> Option<&FakeItem> {
    let (kind, a, b, _) = unpack(handle);
    if kind != ITEM {
        return None;
    }
    s.tracks.get(a as usize)?.items.get(b as usize)
}

fn take_of(s: &State, handle: u64) -> Option<&FakeTake> {
    let (kind, a, b, c) = unpack(handle);
    if kind != TAKE && kind != SOURCE {
        return None;
    }
    s.tracks.get(a as usize)?.items.get(b as usize)?.takes.get(c as usize)
}

fn envelope_of(s: &State, handle: u64) -> Option<&FakeEnvelope> {
    let (kind, a, b, c) = unpack(handle);
    match kind {
        ENVELOPE => track_of(s, pack(TRACK, a, 0, 0))?.envelopes.get(b as usize),
        TAKE_ENVELOPE => {
            let take = s.tracks.get(a as usize)?.items.get(b as usize)?.takes.get((c >> 10) as usize)?;
            take.envelopes.get((c & 1023) as usize)
        }
        _ => None,
    }
}

/// An FX chain's owner: a track's chain, its input chain, or a take's.
fn chain_of(s: &State, owner: u64, index: i32) -> Option<&Vec<FakeFx>> {
    if let Some(track) = track_of(s, owner) {
        return Some(if index & INPUT_FX != 0 { &track.input_fx } else { &track.fx });
    }
    take_of(s, owner).map(|take| &take.fx)
}

/// The levels of a container-addressed FX: its path of child positions, the top-level one first.
fn fx_path(index: i32) -> Vec<usize> {
    let bits = index & 0xFF_FFFF & !INPUT_FX;
    if index & CONTAINER_FX == 0 {
        return vec![bits as usize];
    }
    (0..4).map(|level| ((bits >> (6 * level)) & 63) as usize).take_while(|&n| n != 0).map(|n| n - 1).collect()
}

fn fx_address(path: &[usize], input: bool) -> i32 {
    if path.len() == 1 && !input {
        return path[0] as i32;
    }
    let levels = path.iter().enumerate().map(|(level, &n)| (n as i32 + 1) << (6 * level)).sum::<i32>();
    CONTAINER_FX | levels | if input { INPUT_FX } else { 0 }
}

fn fx_of(s: &State, owner: u64, index: i32) -> Option<&FakeFx> {
    let chain = chain_of(s, owner, index)?;
    let path = fx_path(index);
    let mut node = chain.get(*path.first()?)?;
    for &child in &path[1..] {
        node = node.children.as_ref()?.get(child)?;
    }
    Some(node)
}

// ----- strings -----

fn write(buf: *mut c_char, size: c_int, text: &str) {
    if buf.is_null() || size <= 0 {
        return;
    }
    let n = text.len().min(size as usize - 1);
    unsafe {
        ptr::copy_nonoverlapping(text.as_ptr(), buf.cast(), n);
        *buf.add(n) = 0;
    }
}

thread_local! {
    static STRINGS: RefCell<Vec<CString>> = const { RefCell::new(Vec::new()) };
}

fn keep(text: String) -> *const c_char {
    STRINGS.with(|s| {
        let mut s = s.borrow_mut();
        s.push(CString::new(text).unwrap());
        s.last().unwrap().as_ptr()
    })
}

fn counted<T>(f: impl FnOnce(&mut State) -> T) -> T {
    with(|s| {
        s.calls += 1;
        f(s)
    })
}

fn name_of(name: *const c_char) -> String {
    unsafe { CStr::from_ptr(name) }.to_string_lossy().into_owned()
}

// ----- the application and the project -----

unsafe extern "C" fn get_app_version() -> *const c_char {
    keep(with(|s| s.version.clone()))
}
unsafe extern "C" fn get_resource_path() -> *const c_char {
    keep(with(|s| s.resource.clone()))
}
unsafe extern "C" fn enum_projects(idx: c_int, buf: *mut c_char, size: c_int) -> *mut ReaProject {
    counted(|s| {
        let active = project_ptr(s);
        match (&s.project, idx) {
            (_, 0x4000_0000) if s.rendering => active,
            (Some((path, _)), -1 | 0) => {
                write(buf, size, path);
                active
            }
            (Some((path, _)), n) if n > 0 && (n as usize) <= s.other_tabs.len() => {
                write(buf, size, path);
                (PROJECT_BASE + s.other_tabs[n as usize - 1] * 16) as *mut ReaProject
            }
            _ => ptr::null_mut(),
        }
    })
}
unsafe extern "C" fn get_project_name(_proj: *mut ReaProject, buf: *mut c_char, size: c_int) {
    with(|s| write(buf, size, s.project.as_ref().map(|p| p.1.as_str()).unwrap_or("")))
}
unsafe extern "C" fn get_current_project_in_load_save() -> *mut ReaProject {
    if with(|s| s.loading) {
        with(|s| project_ptr(s))
    } else {
        ptr::null_mut()
    }
}
unsafe extern "C" fn master_get_tempo() -> f64 {
    with(|s| s.bpm)
}
unsafe extern "C" fn get_play_state_ex(_proj: *mut ReaProject) -> c_int {
    with(|s| s.play_bits)
}
unsafe extern "C" fn get_project_state_change_count(_proj: *mut ReaProject) -> c_int {
    counted(|s| s.state_count)
}
unsafe extern "C" fn undo_get_num_entries(_proj: *mut ReaProject) -> c_int {
    counted(|s| s.undo_entries)
}
unsafe extern "C" fn get_project_length(_proj: *mut ReaProject) -> f64 {
    counted(|s| s.length)
}
unsafe extern "C" fn get_cursor_position_ex(_proj: *mut ReaProject) -> f64 {
    counted(|s| s.cursor)
}
unsafe extern "C" fn get_play_position2_ex(_proj: *mut ReaProject) -> f64 {
    counted(|s| if s.play_bits & 1 != 0 { s.play_position } else { s.cursor })
}
unsafe extern "C" fn get_set_loop_time_range2(
    _proj: *mut ReaProject,
    set: bool,
    is_loop: bool,
    start: *mut f64,
    end: *mut f64,
    _seek: bool,
) {
    counted(|s| {
        if set {
            let (from, to) = unsafe { (*start, *end) };
            if is_loop {
                s.loop_range = (from, to);
            } else {
                s.time_selection = (from, to);
            }
            s.undo_dirty = true;
            return;
        }
        let (from, to) = if is_loop { s.loop_range } else { s.time_selection };
        unsafe {
            *start = from;
            *end = to;
        }
    })
}
unsafe extern "C" fn get_set_repeat_ex(_proj: *mut ReaProject, value: c_int) -> c_int {
    counted(|s| {
        if value >= 0 {
            s.repeat = value != 0;
            s.undo_dirty = true;
        }
        s.repeat as c_int
    })
}

/// With tempo markers the project's tempo is the first one's; with none it is the project's own.
unsafe extern "C" fn set_current_bpm(_proj: *mut ReaProject, bpm: f64, _undo: bool) {
    counted(|s| {
        s.bpm = bpm;
        if let Some(first) = s.tempo_markers.first_mut() {
            first.bpm = bpm;
        }
        s.undo_dirty = true;
    })
}

#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn set_tempo_time_sig_marker(
    _proj: *mut ReaProject,
    index: c_int,
    time: f64,
    _measure: c_int,
    _beat: f64,
    bpm: f64,
    num: c_int,
    denom: c_int,
    linear: bool,
) -> bool {
    counted(|s| {
        let current = s.tempo_markers.iter().rev().find(|m| m.time <= time).map_or(s.bpm, |m| m.bpm);
        let bpm = if bpm > 0.0 { bpm } else { current };
        let (num, denom) = if num > 0 && denom > 0 { (num, denom) } else { (-1, -1) };
        if index >= 0 {
            let Some(m) = s.tempo_markers.get_mut(index as usize) else { return false };
            *m = FakeTempo { time, bpm, num, denom, linear };
        } else {
            // The first marker brings one at the start with it, holding the tempo and time signature that were there.
            if s.tempo_markers.is_empty() && time > 0.0 {
                let (num, denom) = s.time_signature;
                s.tempo_markers.push(FakeTempo { time: 0.0, bpm: s.bpm, num, denom, linear: false });
            }
            let at = s.tempo_markers.iter().position(|m| m.time > time).unwrap_or(s.tempo_markers.len());
            s.tempo_markers.insert(at, FakeTempo { time, bpm, num, denom, linear });
        }
        s.undo_dirty = true;
        true
    })
}

unsafe extern "C" fn delete_tempo_time_sig_marker(_proj: *mut ReaProject, index: c_int) -> bool {
    counted(|s| {
        if index < 0 || index as usize >= s.tempo_markers.len() {
            return false;
        }
        s.tempo_markers.remove(index as usize);
        s.undo_dirty = true;
        true
    })
}

unsafe extern "C" fn set_edit_cur_pos2(_proj: *mut ReaProject, time: f64, _move_view: bool, _seek: bool) {
    counted(|s| s.cursor = time)
}
unsafe extern "C" fn time_map_get_time_sig_at_time(_proj: *mut ReaProject, time: f64, num: *mut c_int, denom: *mut c_int, bpm: *mut f64) {
    counted(|s| {
        let marker = s.tempo_markers.iter().rev().find(|m| m.time <= time);
        unsafe {
            *num = marker.map_or(s.time_signature.0, |m| m.num);
            *denom = marker.map_or(s.time_signature.1, |m| m.denom);
            *bpm = marker.map_or(s.bpm, |m| m.bpm);
        }
    })
}
unsafe extern "C" fn time_map2_time_to_qn(_proj: *mut ReaProject, time: f64) -> f64 {
    counted(|s| time * s.bpm / 60.0)
}
unsafe extern "C" fn time_map2_qn_to_time(_proj: *mut ReaProject, qn: f64) -> f64 {
    counted(|s| qn * 60.0 / s.bpm)
}
unsafe extern "C" fn time_map2_time_to_beats(
    _proj: *mut ReaProject,
    time: f64,
    measures: *mut c_int,
    beats_in_measure: *mut c_int,
    full_beats: *mut f64,
    denom: *mut c_int,
) -> f64 {
    counted(|s| {
        let qn = time * s.bpm / 60.0;
        // A measure is `num` beats of 1/`denom` notes long: 3 quarter notes in 6/8, counted by eighths as REAPER does.
        let per_measure = s.time_signature.0 as f64 * 4.0 / s.time_signature.1 as f64;
        let whole = (qn / per_measure).floor();
        unsafe {
            *measures = whole as c_int;
            *beats_in_measure = s.time_signature.0;
            *full_beats = qn;
            *denom = s.time_signature.1;
        }
        (qn - whole * per_measure) * s.time_signature.1 as f64 / 4.0
    })
}
unsafe extern "C" fn get_set_arrange_view2(_proj: *mut ReaProject, set: bool, _x0: c_int, _x1: c_int, start: *mut f64, end: *mut f64) {
    assert!(!set, "the fake only reads");
    counted(|s| unsafe {
        *start = s.arrange_view.0;
        *end = s.arrange_view.1;
    })
}
unsafe extern "C" fn is_project_dirty(_proj: *mut ReaProject) -> c_int {
    with(|s| s.dirty as c_int)
}
unsafe extern "C" fn main_save_project(_proj: *mut ReaProject, _save_as: bool) {
    with(|s| {
        s.dirty = s.save_fails;
        s.saved += 1;
    });
}
unsafe extern "C" fn main_on_command_ex(command: c_int, _flag: c_int, _proj: *mut ReaProject) {
    let hook = with(|s| {
        s.actions.push(command);
        s.on_action
    });
    if let Some(hook) = hook {
        hook(command);
    }
}
unsafe extern "C" fn prevent_ui_refresh(count: c_int) {
    with(|s| s.ui_refresh += count);
}
unsafe extern "C" fn kbd_get_text_from_cmd(cmd: c_int, _section: *mut c_void) -> *const c_char {
    match cmd {
        40001 => c"Track: Insert new track".as_ptr(),
        40004 => c"File: Quit REAPER".as_ptr(),
        40022 => c"File: Save project as...".as_ptr(),
        40023 => c"File: New project".as_ptr(),
        40026 => c"File: Save project".as_ptr(),
        40860 => c"Close current project tab".as_ptr(),
        40886 => c"File: Close all projects".as_ptr(),
        41824 => c"File: Render project, using the most recent render settings".as_ptr(),
        54000 => c"Script: kumi.lua".as_ptr(),
        _ => c"".as_ptr(),
    }
}
unsafe extern "C" fn named_command_lookup(name: *const c_char) -> c_int {
    if unsafe { CStr::from_ptr(name) } == c"_RS_kumi" {
        54000
    } else {
        0
    }
}
unsafe extern "C" fn get_main_hwnd() -> *mut c_void {
    MAIN_WINDOW as *mut c_void
}
unsafe extern "C" fn validate_ptr2(_proj: *mut ReaProject, handle: *mut c_void, class: *const c_char) -> bool {
    let (handle, class) = (handle as u64, name_of(class));
    counted(|s| match class.as_str() {
        "MediaTrack*" => track_of(s, handle).is_some(),
        "MediaItem*" => item_of(s, handle).is_some(),
        "MediaItem_Take*" => take_of(s, handle).is_some(),
        "TrackEnvelope*" => envelope_of(s, handle).is_some(),
        _ => false,
    })
}

// ----- markers and the tempo map -----

const PROJECT_MARKER: u64 = 8;

unsafe extern "C" fn get_num_regions_or_markers(_proj: *mut ReaProject) -> c_int {
    counted(|s| s.markers.len() as c_int)
}
unsafe extern "C" fn get_region_or_marker(_proj: *mut ReaProject, index: c_int, guid: *const c_char) -> *mut api::ProjectMarker {
    let guid = if guid.is_null() { None } else { Some(name_of(guid)) };
    counted(|s| {
        let found = match (index, guid) {
            (i, _) if i >= 0 => (i as usize) < s.markers.len(),
            (_, Some(guid)) => {
                return s
                    .markers
                    .iter()
                    .position(|m| m.guid == guid)
                    .map_or(ptr::null_mut(), |i| pack(PROJECT_MARKER, i as u64, 0, 0) as *mut api::ProjectMarker)
            }
            _ => false,
        };
        if found {
            pack(PROJECT_MARKER, index as u64, 0, 0) as *mut api::ProjectMarker
        } else {
            ptr::null_mut()
        }
    })
}
unsafe extern "C" fn get_region_or_marker_info_value(_proj: *mut ReaProject, marker: *mut api::ProjectMarker, parm: *const c_char) -> f64 {
    let parm = name_of(parm);
    counted(|s| {
        let (kind, index, _, _) = unpack(marker as u64);
        let Some(m) = s.markers.get(index as usize).filter(|_| kind == PROJECT_MARKER) else { return 0.0 };
        match parm.as_str() {
            "B_ISREGION" => m.is_region as i32 as f64,
            "D_STARTPOS" => m.start,
            "D_ENDPOS" => m.end,
            "I_NUMBER" => m.number as f64,
            "I_CUSTOMCOLOR" => m.color as f64,
            "I_INDEX" => index as f64,
            _ => 0.0,
        }
    })
}
unsafe extern "C" fn get_set_region_or_marker_info_string(
    _proj: *mut ReaProject,
    marker: *mut api::ProjectMarker,
    parm: *const c_char,
    buf: *mut c_char,
    set: bool,
) -> bool {
    let parm = name_of(parm);
    if set {
        let value = name_of(buf);
        return counted(|s| {
            let (kind, index, _, _) = unpack(marker as u64);
            let Some(m) = s.markers.get_mut(index as usize).filter(|_| kind == PROJECT_MARKER) else { return false };
            if parm != "P_NAME" {
                return false;
            }
            m.name = value;
            s.undo_dirty = true;
            true
        });
    }
    counted(|s| {
        let (kind, index, _, _) = unpack(marker as u64);
        let Some(m) = s.markers.get(index as usize).filter(|_| kind == PROJECT_MARKER) else { return false };
        match parm.as_str() {
            "GUID" => write(buf, 4096, &m.guid),
            "P_NAME" => write(buf, 4096, &m.name),
            _ => return false,
        }
        true
    })
}

unsafe extern "C" fn add_region_or_marker(
    _proj: *mut ReaProject,
    region: bool,
    start: f64,
    end: f64,
    name: *const c_char,
    wanted: c_int,
    color: c_int,
) -> *mut api::ProjectMarker {
    let name = name_of(name);
    counted(|s| {
        s.made += 1;
        let number = if wanted >= 0 { wanted } else { s.markers.iter().map(|m| m.number).max().unwrap_or(0) + 1 };
        s.markers.push(FakeMarker {
            guid: format!("{{MK-NEW-{}}}", s.made),
            is_region: region,
            start,
            end: if region { end } else { start },
            name,
            number,
            color,
        });
        s.undo_dirty = true;
        pack(PROJECT_MARKER, s.markers.len() as u64 - 1, 0, 0) as *mut api::ProjectMarker
    })
}

unsafe extern "C" fn set_region_or_marker_info_value(
    _proj: *mut ReaProject,
    marker: *mut api::ProjectMarker,
    parm: *const c_char,
    value: f64,
) -> f64 {
    let parm = name_of(parm);
    counted(|s| {
        let (kind, index, _, _) = unpack(marker as u64);
        let Some(m) = s.markers.get_mut(index as usize).filter(|_| kind == PROJECT_MARKER) else { return 0.0 };
        match parm.as_str() {
            "D_STARTPOS" => {
                m.start = value;
                if !m.is_region {
                    m.end = value;
                }
            }
            "D_ENDPOS" if m.is_region => m.end = value,
            "I_NUMBER" => m.number = value as i32,
            "I_CUSTOMCOLOR" => m.color = value as i32,
            _ => return 0.0,
        }
        s.undo_dirty = true;
        1.0
    })
}

unsafe extern "C" fn delete_project_marker_by_index(_proj: *mut ReaProject, index: c_int) -> bool {
    counted(|s| {
        if index < 0 || index as usize >= s.markers.len() {
            return false;
        }
        s.markers.remove(index as usize);
        s.undo_dirty = true;
        true
    })
}
unsafe extern "C" fn count_tempo_time_sig_markers(_proj: *mut ReaProject) -> c_int {
    counted(|s| s.tempo_markers.len() as c_int)
}
unsafe extern "C" fn get_tempo_time_sig_marker(
    _proj: *mut ReaProject,
    idx: c_int,
    time: *mut f64,
    measure: *mut c_int,
    beat: *mut f64,
    bpm: *mut f64,
    num: *mut c_int,
    denom: *mut c_int,
    linear: *mut bool,
) -> bool {
    counted(|s| match s.tempo_markers.get(idx as usize) {
        Some(m) => {
            unsafe {
                *time = m.time;
                *measure = 0;
                *beat = 0.0;
                *bpm = m.bpm;
                *num = m.num;
                *denom = m.denom;
                *linear = m.linear;
            }
            true
        }
        None => false,
    })
}

// ----- tracks -----

unsafe extern "C" fn count_tracks(_proj: *mut ReaProject) -> c_int {
    counted(|s| s.tracks.len() as c_int)
}
unsafe extern "C" fn get_track(_proj: *mut ReaProject, index: c_int) -> *mut MediaTrack {
    counted(|s| if index >= 0 && (index as usize) < s.tracks.len() { track_ptr(index as usize) } else { ptr::null_mut() })
}
unsafe extern "C" fn get_master_track(_proj: *mut ReaProject) -> *mut MediaTrack {
    master_ptr()
}
unsafe extern "C" fn count_selected_tracks2(_proj: *mut ReaProject, master: bool) -> c_int {
    counted(|s| (s.tracks.iter().filter(|t| t.selected).count() + (master && s.master.selected) as usize) as c_int)
}
unsafe extern "C" fn get_selected_track2(_proj: *mut ReaProject, i: c_int, master: bool) -> *mut MediaTrack {
    counted(|s| {
        let mut selected: Vec<*mut MediaTrack> = vec![];
        if master && s.master.selected {
            selected.push(master_ptr());
        }
        selected.extend(s.tracks.iter().enumerate().filter(|(_, t)| t.selected).map(|(n, _)| track_ptr(n)));
        selected.get(i as usize).copied().unwrap_or(ptr::null_mut())
    })
}
unsafe extern "C" fn get_last_touched_track() -> *mut MediaTrack {
    counted(|s| s.tracks.iter().position(|t| t.selected).map_or(ptr::null_mut(), track_ptr))
}
unsafe extern "C" fn get_media_track_info_value(track: *mut MediaTrack, parm: *const c_char) -> f64 {
    let parm = name_of(parm);
    counted(|s| {
        let Some(t) = track_of(s, track as u64) else { return 0.0 };
        match parm.as_str() {
            "IP_TRACKNUMBER" => match unpack(track as u64).1 {
                MASTER => -1.0,
                n => n as f64 + 1.0,
            },
            "I_FOLDERDEPTH" => t.folder as f64,
            "B_MUTE" => t.mute as i32 as f64,
            "I_SOLO" => t.solo as f64,
            "I_RECARM" => t.arm as i32 as f64,
            "I_SELECTED" => t.selected as i32 as f64,
            "D_VOL" => t.volume,
            "D_PAN" => t.pan,
            "B_MAINSEND" => t.main_send as i32 as f64,
            "I_CUSTOMCOLOR" => t.color as f64,
            other => t.extra.get(other).copied().unwrap_or_else(|| new_track_default(other)),
        }
    })
}
/// What a new track holds for the attributes the fake lets a test set.
fn new_track_default(parm: &str) -> f64 {
    match parm {
        "I_NCHAN" => 2.0,
        "B_SHOWINTCP" | "B_SHOWINMIXER" | "I_FXEN" | "D_WIDTH" => 1.0,
        "D_PANLAW" | "I_RECINPUT" => -1.0,
        "I_PANMODE" => 3.0,
        _ => 0.0,
    }
}
/// `P_PARTRACK`: the nearest folder above that has not been closed by the tracks between.
unsafe extern "C" fn get_set_media_track_info(track: *mut MediaTrack, parm: *const c_char, set: *mut c_void) -> *mut c_void {
    assert!(set.is_null(), "the fake only reads");
    let parm = name_of(parm);
    counted(|s| {
        let at = unpack(track as u64).1;
        if parm != "P_PARTRACK" || track_of(s, track as u64).is_none() || at == MASTER {
            return ptr::null_mut();
        }
        let mut open: Vec<usize> = Vec::new();
        for (n, t) in s.tracks.iter().enumerate().take(at as usize) {
            if t.folder == 1 {
                open.push(n);
            } else if t.folder < 0 {
                for _ in 0..-t.folder {
                    open.pop();
                }
            }
        }
        open.last().map_or(ptr::null_mut(), |&n| track_ptr(n).cast())
    })
}
unsafe extern "C" fn get_set_media_track_info_string(track: *mut MediaTrack, parm: *const c_char, buf: *mut c_char, set: bool) -> bool {
    let parm = name_of(parm);
    if set {
        let value = name_of(buf);
        return counted(|s| {
            let at = unpack(track as u64).1;
            let t = match at {
                MASTER => Some(&mut s.master),
                n => s.tracks.get_mut(n as usize),
            };
            match (t, parm.as_str()) {
                (Some(t), "P_NAME") => {
                    t.name = value;
                    s.undo_dirty = true;
                    true
                }
                _ => false,
            }
        });
    }
    counted(|s| match (track_of(s, track as u64), parm.as_str()) {
        (Some(t), "GUID") => {
            write(buf, 4096, &t.guid);
            true
        }
        (Some(t), "P_NAME") => {
            write(buf, 4096, &t.name);
            true
        }
        (Some(t), lane) if lane.starts_with("P_LANENAME:") => {
            let name = lane["P_LANENAME:".len()..].parse::<usize>().ok().and_then(|n| t.lane_names.get(n));
            name.inspect(|name| write(buf, 4096, name)).is_some()
        }
        _ => false,
    })
}
unsafe extern "C" fn get_track_name(track: *mut MediaTrack, buf: *mut c_char, size: c_int) -> bool {
    counted(|s| match track_of(s, track as u64) {
        Some(t) => {
            write(buf, size, &t.name);
            true
        }
        None => false,
    })
}
unsafe extern "C" fn get_proj_ext_state(
    _proj: *mut ReaProject,
    section: *const c_char,
    key: *const c_char,
    buf: *mut c_char,
    size: c_int,
) -> c_int {
    let (section, key) = (name_of(section), name_of(key));
    counted(|s| match s.project_ext.get(&(section, key)) {
        Some(value) => {
            write(buf, size, value);
            value.len() as c_int
        }
        None => 0,
    })
}
/// A track's FX chain as REAPER writes it into the track's chunk: an entry per FX, the plug-in's own block inside.
fn fx_chain_text(chain: &[FakeFx]) -> String {
    if chain.is_empty() {
        return String::new();
    }
    let mut text = String::from("<FXCHAIN\nSHOW 0\nLASTSEL 0\nDOCKED 0\n");
    for fx in chain {
        text += &format!("BYPASS {} 0 0\n", if fx.enabled { 0 } else { 1 });
        match &fx.children {
            Some(_) => text += "<CONTAINER Container 0 \"\" 0\n>\n",
            None => {
                text += &format!("<VST \"{}\" fake.vst 0 \"\" 0\n", fx.name);
                for p in &fx.params {
                    text += &format!("PARAM {} {}\n", p.value, p.name);
                }
                text += ">\n";
            }
        }
        text += &format!("FLOATPOS 0 0 0 0\nFXID {}\nWAK 0 0\n", fx.guid);
    }
    text + ">\n"
}

/// An FX chain file's entries as FX: the name and parameters of the block, the bypass and the id.
fn fx_from_entries(text: &str) -> Vec<FakeFx> {
    let mut out = Vec::new();
    let mut current: Option<FakeFx> = None;
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("BYPASS ") {
            out.extend(current.take());
            let mut fx = FakeFx::new("", "");
            fx.enabled = rest.split(' ').next() == Some("0");
            current = Some(fx);
        } else if let Some(rest) = line.strip_prefix("<VST \"") {
            if let Some(fx) = current.as_mut() {
                fx.name = rest.split('"').next().unwrap_or_default().to_string();
            }
        } else if let Some(rest) = line.strip_prefix("PARAM ") {
            if let (Some(fx), Some((value, name))) = (current.as_mut(), rest.split_once(' ')) {
                fx.params.push(FakeParam::new(name, value.parse().unwrap_or(0.0), ""));
            }
        } else if let Some(guid) = line.strip_prefix("FXID ") {
            if let Some(fx) = current.as_mut() {
                fx.guid = guid.trim().to_string();
            }
        }
    }
    out.extend(current);
    out
}

/// A plug-in the fake can instantiate by name: the catalogue is REAPER's own.
fn new_fx(s: &mut State, name: &str) -> Option<FakeFx> {
    let lower = name.to_ascii_lowercase();
    let full = if lower.starts_with("js:") {
        name.to_string()
    } else {
        let known = ["ReaEQ", "ReaComp", "ReaDelay", "ReaVerb", "ReaLimit", "ReaSynth"];
        let found = known.iter().find(|k| lower.contains(&k.to_ascii_lowercase()))?;
        format!("VST: {found} (Cockos)")
    };
    s.fx_made += 1;
    let mut fx = FakeFx::new(&format!("{{FX-NEW-{}}}", s.fx_made), &full);
    let mut gain = FakeParam::linear("Gain", 0.5, -12.0, 12.0, 1, "dB");
    gain.bare = true;
    fx.params = vec![
        gain,
        FakeParam::steps("Mode", 0.0, &["Low", "Mid", "High"]),
        FakeParam::new("Mix", 1.0, "100 %"),
        FakeParam::linear("Offset", 0.5, -1.0, 1.0, 2, ""),
    ];
    Some(fx)
}

unsafe extern "C" fn track_fx_add_by_name(track: *mut MediaTrack, name: *const c_char, rec: bool, instantiate: c_int) -> c_int {
    let name = name_of(name);
    counted(|s| {
        if rec {
            return -1;
        }
        let made = if name.ends_with(".RfxChain") {
            let path = std::path::Path::new(&s.resource).join("FXChains").join(&name);
            std::fs::read_to_string(path).ok().map(|text| fx_from_entries(&text))
        } else {
            new_fx(s, &name).map(|fx| vec![fx])
        };
        let Some(made) = made.filter(|m| !m.is_empty()) else { return -1 };
        let Some(t) = track_mut(s, track as u64) else { return -1 };
        let at = if instantiate <= -1000 { ((-1000 - instantiate) as usize).min(t.fx.len()) } else { t.fx.len() };
        let first = at as c_int;
        for (n, fx) in made.into_iter().enumerate() {
            t.fx.insert(at + n, fx);
        }
        s.undo_dirty = true;
        first
    })
}

unsafe extern "C" fn track_fx_delete(track: *mut MediaTrack, fx: c_int) -> bool {
    counted(|s| {
        let Some(t) = track_mut(s, track as u64) else { return false };
        if fx < 0 || fx as usize >= t.fx.len() {
            return false;
        }
        t.fx.remove(fx as usize);
        s.undo_dirty = true;
        true
    })
}

unsafe extern "C" fn track_fx_copy_to_track(src: *mut MediaTrack, from: c_int, dest: *mut MediaTrack, to: c_int, is_move: bool) {
    counted(|s| {
        // Only within one track's chain, which is all the bridge asks.
        if src != dest || !is_move {
            return;
        }
        let Some(t) = track_mut(s, src as u64) else { return };
        if from < 0 || from as usize >= t.fx.len() || to < 0 {
            return;
        }
        let fx = t.fx.remove(from as usize);
        let to = (to as usize).min(t.fx.len());
        t.fx.insert(to, fx);
        s.undo_dirty = true;
    })
}

unsafe extern "C" fn track_fx_set_enabled(track: *mut MediaTrack, fx: c_int, enabled: bool) {
    counted(|s| {
        if let Some(f) = track_mut(s, track as u64).and_then(|t| t.fx.get_mut(fx as usize)) {
            f.enabled = enabled;
            s.undo_dirty = true;
        }
    })
}

unsafe extern "C" fn track_fx_set_param_normalized(track: *mut MediaTrack, fx: c_int, param: c_int, value: f64) -> bool {
    counted(|s| {
        let Some(p) = track_mut(s, track as u64).and_then(|t| t.fx.get_mut(fx as usize)).and_then(|f| f.params.get_mut(param as usize))
        else {
            return false;
        };
        p.value = value.clamp(0.0, 1.0);
        p.shown = p.text_for(p.value);
        s.undo_dirty = true;
        true
    })
}

unsafe extern "C" fn track_fx_format_param_value_normalized(
    track: *mut MediaTrack,
    fx: c_int,
    param: c_int,
    value: f64,
    buf: *mut c_char,
    size: c_int,
) -> bool {
    counted(|s| match track_of(s, track as u64).and_then(|t| t.fx.get(fx as usize)).and_then(|f| f.params.get(param as usize)) {
        Some(p) => {
            write(buf, size, &p.text_for(value));
            true
        }
        None => false,
    })
}

unsafe extern "C" fn track_fx_get_instrument(track: *mut MediaTrack) -> c_int {
    counted(|s| track_of(s, track as u64).and_then(|t| t.fx.iter().position(|f| f.instrument)).map_or(-1, |n| n as c_int))
}
unsafe extern "C" fn get_track_depth(track: *mut MediaTrack) -> c_int {
    counted(|s| {
        let at = unpack(track as u64).1 as usize;
        if track_of(s, track as u64).is_none() || unpack(track as u64).1 == MASTER {
            return 0;
        }
        s.tracks.iter().take(at).map(|t| t.folder).sum::<i32>().max(0)
    })
}
unsafe extern "C" fn get_track_color(track: *mut MediaTrack) -> c_int {
    counted(|s| track_of(s, track as u64).map_or(0, |t| t.color))
}
unsafe extern "C" fn color_from_native(native: c_int, r: *mut c_int, g: *mut c_int, b: *mut c_int) {
    unsafe {
        *r = native & 255;
        *g = (native >> 8) & 255;
        *b = (native >> 16) & 255;
    }
}

// ----- items and takes -----

unsafe extern "C" fn count_track_media_items(track: *mut MediaTrack) -> c_int {
    counted(|s| track_of(s, track as u64).map_or(0, |t| t.items.len() as c_int))
}
unsafe extern "C" fn get_track_media_item(track: *mut MediaTrack, index: c_int) -> *mut MediaItem {
    counted(|s| match track_of(s, track as u64) {
        Some(t) if index >= 0 && (index as usize) < t.items.len() => item_ptr(unpack(track as u64).1 as usize, index as usize),
        _ => ptr::null_mut(),
    })
}
unsafe extern "C" fn get_media_item_track(item: *mut MediaItem) -> *mut MediaTrack {
    counted(|s| if item_of(s, item as u64).is_some() { track_ptr(unpack(item as u64).1 as usize) } else { ptr::null_mut() })
}
unsafe extern "C" fn get_media_item_info_value(item: *mut MediaItem, parm: *const c_char) -> f64 {
    let parm = name_of(parm);
    counted(|s| {
        let Some(i) = item_of(s, item as u64) else { return 0.0 };
        match parm.as_str() {
            "D_POSITION" => i.position,
            "D_LENGTH" => i.length,
            "B_MUTE" => i.mute as i32 as f64,
            "I_FIXEDLANE" => i.lane as f64,
            "I_CUSTOMCOLOR" => i.color as f64,
            "B_UISEL" => i.selected as i32 as f64,
            "I_CURTAKE" => i.active as f64,
            "IP_ITEMNUMBER" => unpack(item as u64).2 as f64,
            other => i.extra.get(other).copied().unwrap_or(0.0),
        }
    })
}
unsafe extern "C" fn get_set_media_item_info_string(item: *mut MediaItem, parm: *const c_char, buf: *mut c_char, set: bool) -> bool {
    assert!(!set, "the fake only reads");
    let parm = name_of(parm);
    counted(|s| match (item_of(s, item as u64), parm.as_str()) {
        (Some(i), "GUID") => {
            write(buf, 4096, &i.guid);
            true
        }
        _ => false,
    })
}
unsafe extern "C" fn count_takes(item: *mut MediaItem) -> c_int {
    counted(|s| item_of(s, item as u64).map_or(0, |i| i.takes.len() as c_int))
}
unsafe extern "C" fn get_media_item_take(item: *mut MediaItem, index: c_int) -> *mut MediaItem_Take {
    counted(|s| match item_of(s, item as u64) {
        Some(i) if index >= 0 && (index as usize) < i.takes.len() => {
            let (_, t, n, _) = unpack(item as u64);
            take_ptr(t as usize, n as usize, index as usize)
        }
        _ => ptr::null_mut(),
    })
}
unsafe extern "C" fn get_active_take(item: *mut MediaItem) -> *mut MediaItem_Take {
    counted(|s| match item_of(s, item as u64) {
        Some(i) if i.active < i.takes.len() => {
            let (_, t, n, _) = unpack(item as u64);
            take_ptr(t as usize, n as usize, i.active)
        }
        _ => ptr::null_mut(),
    })
}
unsafe extern "C" fn get_media_item_take_item(take: *mut MediaItem_Take) -> *mut MediaItem {
    counted(|s| {
        let (_, t, n, _) = unpack(take as u64);
        if take_of(s, take as u64).is_some() {
            item_ptr(t as usize, n as usize)
        } else {
            ptr::null_mut()
        }
    })
}
unsafe extern "C" fn get_set_media_item_take_info_string(
    take: *mut MediaItem_Take,
    parm: *const c_char,
    buf: *mut c_char,
    set: bool,
) -> bool {
    assert!(!set, "the fake only reads");
    let parm = name_of(parm);
    counted(|s| match (take_of(s, take as u64), parm.as_str()) {
        (Some(t), "GUID") => {
            write(buf, 4096, &t.guid);
            true
        }
        (Some(t), "P_NAME") => {
            write(buf, 4096, &t.name);
            true
        }
        _ => false,
    })
}
unsafe extern "C" fn get_take_name(take: *mut MediaItem_Take) -> *const c_char {
    let name = counted(|s| take_of(s, take as u64).map(|t| t.name.clone()));
    name.map_or(ptr::null(), keep)
}
unsafe extern "C" fn get_media_item_take_info_value(take: *mut MediaItem_Take, parm: *const c_char) -> f64 {
    let parm = name_of(parm);
    counted(|s| take_of(s, take as u64).and_then(|t| t.extra.get(&parm).copied()).unwrap_or(0.0))
}
unsafe extern "C" fn get_media_item_take_source(take: *mut MediaItem_Take) -> *mut api::PCM_source {
    counted(|s| match take_of(s, take as u64) {
        Some(t) if t.source.is_some() => {
            let (_, a, b, c) = unpack(take as u64);
            pack(SOURCE, a, b, c) as *mut api::PCM_source
        }
        _ => ptr::null_mut(),
    })
}
unsafe extern "C" fn get_media_source_type(source: *mut api::PCM_source, buf: *mut c_char, size: c_int) {
    counted(|s| {
        if let Some(kind) = take_of(s, source as u64).and_then(|t| t.source.clone()) {
            write(buf, size, &kind);
        }
    })
}
/// The fake's GUID strings are handed around as C strings (a real GUID is 16 bytes): `stringToGuid` stores an index
/// into the strings it has been given, and `guidToString` turns a pointer from the FX functions back into text.
unsafe extern "C" fn string_to_guid(text: *const c_char, guid: *mut api::GUID) {
    let id = keep(name_of(text)) as u64;
    unsafe { ptr::write_unaligned(guid.cast::<u64>(), id) };
}
unsafe extern "C" fn guid_to_string(guid: *const api::GUID, buf: *mut c_char) {
    write(buf, 64, &name_of(guid.cast()));
}
unsafe extern "C" fn get_media_item_take_by_guid(_proj: *mut ReaProject, guid: *const api::GUID) -> *mut MediaItem_Take {
    let text = name_of(unsafe { ptr::read_unaligned(guid.cast::<u64>()) } as *const c_char);
    counted(|s| {
        for (t, track) in s.tracks.iter().enumerate() {
            for (i, item) in track.items.iter().enumerate() {
                if let Some(k) = item.takes.iter().position(|take| take.guid == text) {
                    return take_ptr(t, i, k);
                }
            }
        }
        ptr::null_mut()
    })
}

// ----- selection and focus -----

unsafe extern "C" fn count_selected_media_items(_proj: *mut ReaProject) -> c_int {
    counted(|s| s.tracks.iter().flat_map(|t| &t.items).filter(|i| i.selected).count() as c_int)
}
unsafe extern "C" fn get_selected_media_item(_proj: *mut ReaProject, n: c_int) -> *mut MediaItem {
    counted(|s| {
        let selected =
            s.tracks.iter().enumerate().flat_map(|(t, track)| track.items.iter().enumerate().map(move |(i, item)| (t, i, item.selected)));
        selected.filter(|(_, _, on)| *on).nth(n as usize).map_or(ptr::null_mut(), |(t, i, _)| item_ptr(t, i))
    })
}
unsafe extern "C" fn get_selected_envelope(_proj: *mut ReaProject) -> *mut TrackEnvelope {
    counted(|s| s.selected_envelope.map_or(ptr::null_mut(), |(t, e)| pack(ENVELOPE, t as u64, e as u64, 0) as *mut TrackEnvelope))
}
unsafe extern "C" fn get_cursor_context2(_last: bool) -> c_int {
    counted(|s| s.cursor_context)
}
unsafe extern "C" fn midi_editor_get_active() -> *mut c_void {
    counted(|s| if s.midi_editor.is_some() { MIDI_EDITOR as *mut c_void } else { ptr::null_mut() })
}
unsafe extern "C" fn midi_editor_get_take(editor: *mut c_void) -> *mut MediaItem_Take {
    counted(|s| match s.midi_editor {
        Some((t, i, k)) if editor as u64 == MIDI_EDITOR => take_ptr(t, i, k),
        _ => ptr::null_mut(),
    })
}
unsafe extern "C" fn get_touched_or_focused_fx(
    mode: c_int,
    track: *mut c_int,
    item: *mut c_int,
    take: *mut c_int,
    fx: *mut c_int,
    param: *mut c_int,
) -> bool {
    counted(|s| match if mode == 0 { s.touched_fx } else { s.focused_fx } {
        Some((t, i, k, f, p)) => {
            unsafe {
                *track = t;
                *item = i;
                *take = k;
                *fx = f;
                *param = p;
            }
            true
        }
        None => false,
    })
}

// ----- FX (the same for a track's chain and a take's) -----

fn fx_count(owner: u64) -> c_int {
    counted(|s| chain_of(s, owner, 0).map_or(0, |c| c.len() as c_int))
}
fn fx_guid(owner: u64, index: c_int) -> *mut api::GUID {
    let guid = counted(|s| fx_of(s, owner, index).map(|f| f.guid.clone()));
    guid.map_or(ptr::null_mut(), |g| keep(g) as *mut api::GUID)
}
fn fx_name(owner: u64, index: c_int, buf: *mut c_char, size: c_int) -> bool {
    counted(|s| fx_of(s, owner, index).map(|f| write(buf, size, &f.name)).is_some())
}
fn fx_enabled(owner: u64, index: c_int) -> bool {
    counted(|s| fx_of(s, owner, index).is_some_and(|f| f.enabled))
}
fn fx_offline(owner: u64, index: c_int) -> bool {
    counted(|s| fx_of(s, owner, index).is_some_and(|f| f.offline))
}
fn fx_preset(owner: u64, index: c_int, buf: *mut c_char, size: c_int) -> bool {
    counted(|s| match fx_of(s, owner, index) {
        Some(f) if !f.preset.is_empty() => {
            write(buf, size, &f.preset);
            true
        }
        _ => false,
    })
}
fn fx_num_params(owner: u64, index: c_int) -> c_int {
    counted(|s| fx_of(s, owner, index).map_or(0, |f| f.params.len() as c_int))
}
fn fx_named_config(owner: u64, index: c_int, key: *const c_char, buf: *mut c_char, size: c_int) -> bool {
    let key = name_of(key);
    counted(|s| {
        let Some(f) = fx_of(s, owner, index) else { return false };
        let path = fx_path(index);
        let input = index & INPUT_FX != 0;
        let answer = match key.as_str() {
            "container_count" => f.children.as_ref().map(|c| c.len().to_string()),
            "parallel" => Some(f.parallel.to_string()),
            "renamed_name" => Some(f.renamed.clone()),
            "fx_type" => Some(f.name.split(':').next().unwrap_or("").to_string()),
            other => other.strip_prefix("container_item.").and_then(|n| n.parse::<usize>().ok()).and_then(|n| {
                let children = f.children.as_ref()?;
                (n < children.len()).then(|| fx_address(&[path.as_slice(), &[n]].concat(), input).to_string())
            }),
        };
        match answer {
            Some(text) => {
                write(buf, size, &text);
                true
            }
            None => false,
        }
    })
}
fn fx_param_name(owner: u64, index: c_int, param: c_int, buf: *mut c_char, size: c_int) -> bool {
    counted(|s| fx_of(s, owner, index).and_then(|f| f.params.get(param as usize)).map(|p| write(buf, size, &p.name)).is_some())
}
fn fx_param_normalized(owner: u64, index: c_int, param: c_int) -> f64 {
    counted(|s| fx_of(s, owner, index).and_then(|f| f.params.get(param as usize)).map_or(0.0, |p| p.value))
}
fn fx_param_formatted(owner: u64, index: c_int, param: c_int, buf: *mut c_char, size: c_int) -> bool {
    counted(|s| {
        fx_of(s, owner, index)
            .and_then(|f| f.params.get(param as usize))
            .map(|p| match &p.display {
                Display::Linear { unit, .. } if p.bare => write(buf, size, p.shown.trim_end_matches(unit.as_str()).trim_end()),
                _ => write(buf, size, &p.shown),
            })
            .is_some()
    })
}
fn fx_param_ex(owner: u64, index: c_int, param: c_int, min: *mut f64, max: *mut f64, mid: *mut f64) -> f64 {
    counted(|s| match fx_of(s, owner, index).and_then(|f| f.params.get(param as usize)) {
        Some(p) => {
            unsafe {
                *min = p.min;
                *max = p.max;
                *mid = (p.min + p.max) / 2.0;
            }
            p.min + p.value * (p.max - p.min)
        }
        None => 0.0,
    })
}

unsafe extern "C" fn track_fx_get_count(t: *mut MediaTrack) -> c_int {
    fx_count(t as u64)
}
unsafe extern "C" fn take_fx_get_count(t: *mut MediaItem_Take) -> c_int {
    fx_count(t as u64)
}
unsafe extern "C" fn track_fx_get_rec_count(t: *mut MediaTrack) -> c_int {
    counted(|s| track_of(s, t as u64).map_or(0, |t| t.input_fx.len() as c_int))
}
unsafe extern "C" fn track_fx_get_fxguid(t: *mut MediaTrack, fx: c_int) -> *mut api::GUID {
    fx_guid(t as u64, fx)
}
unsafe extern "C" fn take_fx_get_fxguid(t: *mut MediaItem_Take, fx: c_int) -> *mut api::GUID {
    fx_guid(t as u64, fx)
}
unsafe extern "C" fn track_fx_get_fx_name(t: *mut MediaTrack, fx: c_int, buf: *mut c_char, size: c_int) -> bool {
    fx_name(t as u64, fx, buf, size)
}
unsafe extern "C" fn take_fx_get_fx_name(t: *mut MediaItem_Take, fx: c_int, buf: *mut c_char, size: c_int) -> bool {
    fx_name(t as u64, fx, buf, size)
}
unsafe extern "C" fn track_fx_get_enabled(t: *mut MediaTrack, fx: c_int) -> bool {
    fx_enabled(t as u64, fx)
}
unsafe extern "C" fn take_fx_get_enabled(t: *mut MediaItem_Take, fx: c_int) -> bool {
    fx_enabled(t as u64, fx)
}
unsafe extern "C" fn track_fx_get_offline(t: *mut MediaTrack, fx: c_int) -> bool {
    fx_offline(t as u64, fx)
}
unsafe extern "C" fn take_fx_get_offline(t: *mut MediaItem_Take, fx: c_int) -> bool {
    fx_offline(t as u64, fx)
}
unsafe extern "C" fn track_fx_get_preset(t: *mut MediaTrack, fx: c_int, buf: *mut c_char, size: c_int) -> bool {
    fx_preset(t as u64, fx, buf, size)
}
unsafe extern "C" fn take_fx_get_preset(t: *mut MediaItem_Take, fx: c_int, buf: *mut c_char, size: c_int) -> bool {
    fx_preset(t as u64, fx, buf, size)
}
unsafe extern "C" fn track_fx_get_num_params(t: *mut MediaTrack, fx: c_int) -> c_int {
    fx_num_params(t as u64, fx)
}
unsafe extern "C" fn take_fx_get_num_params(t: *mut MediaItem_Take, fx: c_int) -> c_int {
    fx_num_params(t as u64, fx)
}
unsafe extern "C" fn track_fx_get_named_config_parm(
    t: *mut MediaTrack,
    fx: c_int,
    key: *const c_char,
    buf: *mut c_char,
    size: c_int,
) -> bool {
    fx_named_config(t as u64, fx, key, buf, size)
}
unsafe extern "C" fn take_fx_get_named_config_parm(
    t: *mut MediaItem_Take,
    fx: c_int,
    key: *const c_char,
    buf: *mut c_char,
    size: c_int,
) -> bool {
    fx_named_config(t as u64, fx, key, buf, size)
}
unsafe extern "C" fn track_fx_get_param_name(t: *mut MediaTrack, fx: c_int, p: c_int, buf: *mut c_char, size: c_int) -> bool {
    fx_param_name(t as u64, fx, p, buf, size)
}
unsafe extern "C" fn take_fx_get_param_name(t: *mut MediaItem_Take, fx: c_int, p: c_int, buf: *mut c_char, size: c_int) -> bool {
    fx_param_name(t as u64, fx, p, buf, size)
}
unsafe extern "C" fn track_fx_get_param_normalized(t: *mut MediaTrack, fx: c_int, p: c_int) -> f64 {
    fx_param_normalized(t as u64, fx, p)
}
unsafe extern "C" fn take_fx_get_param_normalized(t: *mut MediaItem_Take, fx: c_int, p: c_int) -> f64 {
    fx_param_normalized(t as u64, fx, p)
}
unsafe extern "C" fn track_fx_get_formatted_param_value(t: *mut MediaTrack, fx: c_int, p: c_int, buf: *mut c_char, size: c_int) -> bool {
    fx_param_formatted(t as u64, fx, p, buf, size)
}
unsafe extern "C" fn take_fx_get_formatted_param_value(t: *mut MediaItem_Take, fx: c_int, p: c_int, buf: *mut c_char, size: c_int) -> bool {
    fx_param_formatted(t as u64, fx, p, buf, size)
}
unsafe extern "C" fn track_fx_get_param_ex(t: *mut MediaTrack, fx: c_int, p: c_int, min: *mut f64, max: *mut f64, mid: *mut f64) -> f64 {
    fx_param_ex(t as u64, fx, p, min, max, mid)
}
unsafe extern "C" fn take_fx_get_param_ex(t: *mut MediaItem_Take, fx: c_int, p: c_int, min: *mut f64, max: *mut f64, mid: *mut f64) -> f64 {
    fx_param_ex(t as u64, fx, p, min, max, mid)
}

// ----- sends -----

fn receives(s: &State, track: usize) -> Vec<(usize, usize)> {
    s.tracks
        .iter()
        .enumerate()
        .flat_map(|(t, tr)| tr.sends.iter().enumerate().filter(move |(_, send)| send.dest == track).map(move |(i, _)| (t, i)))
        .collect()
}

unsafe extern "C" fn get_track_num_sends(track: *mut MediaTrack, category: c_int) -> c_int {
    counted(|s| {
        let Some(t) = track_of(s, track as u64) else { return 0 };
        let index = unpack(track as u64).1;
        (match category {
            0 => t.sends.len(),
            c if c < 0 => {
                if index == MASTER {
                    0
                } else {
                    receives(s, index as usize).len()
                }
            }
            _ => t.hw_outs.len(),
        }) as c_int
    })
}
fn send_of(s: &State, track: u64, category: c_int, index: c_int) -> Option<&FakeSend> {
    let t = track_of(s, track)?;
    let own = unpack(track).1;
    match category {
        0 => t.sends.get(index as usize),
        c if c < 0 && own != MASTER => {
            let (src, n) = *receives(s, own as usize).get(index as usize)?;
            s.tracks.get(src)?.sends.get(n)
        }
        c if c > 0 => t.hw_outs.get(index as usize),
        _ => None,
    }
}
unsafe extern "C" fn get_track_send_info_value(track: *mut MediaTrack, category: c_int, index: c_int, parm: *const c_char) -> f64 {
    let parm = name_of(parm);
    counted(|s| {
        let Some(send) = send_of(s, track as u64, category, index) else { return 0.0 };
        match parm.as_str() {
            "D_VOL" => send.volume,
            "D_PAN" => send.pan,
            "B_MUTE" => send.mute as i32 as f64,
            "I_SENDMODE" => send.mode as f64,
            "I_SRCCHAN" => send.src_chan as f64,
            "I_DSTCHAN" => send.dst_chan as f64,
            other => send.extra.get(other).copied().unwrap_or(if other == "I_AUTOMODE" { -1.0 } else { 0.0 }),
        }
    })
}
unsafe extern "C" fn get_set_track_send_info(
    track: *mut MediaTrack,
    category: c_int,
    index: c_int,
    parm: *const c_char,
    set: *mut c_void,
) -> *mut c_void {
    assert!(set.is_null(), "the fake only reads");
    let parm = name_of(parm);
    counted(|s| {
        let own = unpack(track as u64).1;
        match (parm.as_str(), category) {
            ("P_DESTTRACK", 0) => send_of(s, track as u64, 0, index).map_or(ptr::null_mut(), |send| track_ptr(send.dest).cast()),
            (env, 0) if env.starts_with("P_ENV:") => {
                let chunk = &env["P_ENV:".len()..];
                let t = track_of(s, track as u64);
                let found = t.and_then(|t| {
                    t.envelopes.iter().position(|e| e.send.as_ref().is_some_and(|(send, c)| *send == index as usize && c == chunk))
                });
                found.map_or(ptr::null_mut(), |n| pack(ENVELOPE, own, n as u64, 0) as *mut c_void)
            }
            ("P_SRCTRACK", c) if c < 0 && own != MASTER => {
                receives(s, own as usize).get(index as usize).map_or(ptr::null_mut(), |&(src, _)| track_ptr(src).cast())
            }
            _ => ptr::null_mut(),
        }
    })
}

unsafe extern "C" fn get_set_track_send_info_string(
    track: *mut MediaTrack,
    category: c_int,
    index: c_int,
    parm: *const c_char,
    buf: *mut c_char,
    set: bool,
) -> bool {
    let parm = name_of(parm);
    if !parm.starts_with("P_EXT:") {
        return false;
    }
    let value = if set { name_of(buf) } else { String::new() };
    counted(|s| {
        if set {
            // The fake writes a send of the track itself (a send of category 0).
            let Some(send) = track_mut(s, track as u64).filter(|_| category == 0).and_then(|t| t.sends.get_mut(index as usize)) else {
                return false;
            };
            send.ext.insert(parm, value);
            s.undo_dirty = true;
            return true;
        }
        match send_of(s, track as u64, category, index).and_then(|send| send.ext.get(&parm)) {
            Some(text) => {
                write(buf, 1024, text);
                true
            }
            None => {
                write(buf, 1024, "");
                false
            }
        }
    })
}

// ----- writes -----

fn track_mut(s: &mut State, handle: u64) -> Option<&mut FakeTrack> {
    let (kind, a, _, _) = unpack(handle);
    match (kind, a) {
        (TRACK, MASTER) => Some(&mut s.master),
        (TRACK, a) => s.tracks.get_mut(a as usize),
        _ => None,
    }
}

unsafe extern "C" fn set_media_track_info_value(track: *mut MediaTrack, parm: *const c_char, value: f64) -> bool {
    let parm = name_of(parm);
    counted(|s| {
        let Some(t) = track_mut(s, track as u64) else { return false };
        match parm.as_str() {
            "D_VOL" => t.volume = value,
            "D_PAN" => t.pan = value,
            "B_MUTE" => t.mute = value != 0.0,
            "I_SOLO" => t.solo = value as i32,
            "I_RECARM" => t.arm = value != 0.0,
            "I_SELECTED" => t.selected = value != 0.0,
            "I_FOLDERDEPTH" => t.folder = value as i32,
            "I_CUSTOMCOLOR" => t.color = value as i32,
            "B_MAINSEND" => t.main_send = value != 0.0,
            other => {
                t.extra.insert(other.to_string(), value);
            }
        }
        s.undo_dirty = true;
        true
    })
}

unsafe extern "C" fn insert_track_at_index(index: c_int, _defaults: bool) {
    counted(|s| {
        s.made += 1;
        let at = (index.max(0) as usize).min(s.tracks.len());
        s.tracks.insert(at, FakeTrack::new(&format!("{{NEW-{}}}", s.made), ""));
        // Sends name their destination by position.
        for track in s.tracks.iter_mut() {
            for send in track.sends.iter_mut() {
                if send.dest >= at {
                    send.dest += 1;
                }
            }
        }
        s.undo_dirty = true;
    })
}

unsafe extern "C" fn delete_track(track: *mut MediaTrack) {
    counted(|s| {
        let at = unpack(track as u64).1;
        if at == MASTER || at as usize >= s.tracks.len() {
            return;
        }
        let at = at as usize;
        s.tracks.remove(at);
        // Sends to it go with it, and the others' destinations close up.
        for other in s.tracks.iter_mut() {
            other.sends.retain(|send| send.dest != at);
            for send in other.sends.iter_mut() {
                if send.dest > at {
                    send.dest -= 1;
                }
            }
        }
        s.undo_dirty = true;
    })
}

/// The chunk of a fake track is a token naming the state kept for it, in the shape of the head of a real one.
unsafe extern "C" fn get_track_state_chunk(track: *mut MediaTrack, buf: *mut c_char, size: c_int, _isundo: bool) -> bool {
    counted(|s| {
        let Some(mut t) = track_of(s, track as u64).cloned() else { return false };
        // A track's own sends are lines of the tracks they go to (AUXRECV), not of its chunk.
        t.sends.clear();
        let text = format!("<TRACK {}\nNAME \"{}\"\nFAKECHUNK {}\n{}>\n", t.guid, t.name, s.chunks.len(), fx_chain_text(&t.fx));
        s.chunks.push(t);
        if text.len() + 1 > size as usize {
            // Too small: REAPER fills what fits; the caller grows its buffer.
            write(buf, size, &text);
            return true;
        }
        write(buf, size, &text);
        true
    })
}

unsafe extern "C" fn set_track_state_chunk(track: *mut MediaTrack, chunk: *const c_char, _isundo: bool) -> bool {
    let chunk = name_of(chunk);
    counted(|s| {
        let Some(n) = chunk.lines().find_map(|line| line.strip_prefix("FAKECHUNK ")).and_then(|n| n.trim().parse::<usize>().ok()) else {
            return false;
        };
        let Some(state) = s.chunks.get(n).cloned() else { return false };
        let Some(slot) = track_mut(s, track as u64) else { return false };
        // A track's sends are the destination's lines, not part of its chunk.
        let sends = std::mem::take(&mut slot.sends);
        *slot = FakeTrack { sends: if state.sends.is_empty() { sends } else { state.sends }, ..state };
        s.undo_dirty = true;
        true
    })
}

unsafe extern "C" fn create_track_send(track: *mut MediaTrack, dest: *mut MediaTrack) -> c_int {
    counted(|s| {
        let dest_at = if dest.is_null() { None } else { Some(unpack(dest as u64).1 as usize) };
        s.undo_dirty = true;
        let Some(t) = track_mut(s, track as u64) else { return -1 };
        match dest_at {
            Some(dest) => {
                t.sends.push(FakeSend::to(dest));
                t.sends.len() as c_int - 1
            }
            None => {
                t.hw_outs.push(FakeSend::to(0));
                t.hw_outs.len() as c_int - 1
            }
        }
    })
}

unsafe extern "C" fn remove_track_send(track: *mut MediaTrack, category: c_int, index: c_int) -> bool {
    counted(|s| {
        let Some(t) = track_mut(s, track as u64) else { return false };
        let list = if category > 0 { &mut t.hw_outs } else { &mut t.sends };
        if category < 0 || index < 0 || index as usize >= list.len() {
            return false;
        }
        list.remove(index as usize);
        s.undo_dirty = true;
        true
    })
}

unsafe extern "C" fn set_track_send_info_value(
    track: *mut MediaTrack,
    category: c_int,
    index: c_int,
    parm: *const c_char,
    value: f64,
) -> bool {
    let parm = name_of(parm);
    counted(|s| {
        let Some(t) = track_mut(s, track as u64) else { return false };
        let list = if category > 0 { &mut t.hw_outs } else { &mut t.sends };
        if category < 0 {
            return false;
        }
        let Some(send) = list.get_mut(index.max(0) as usize) else { return false };
        match parm.as_str() {
            "D_VOL" => send.volume = value,
            "D_PAN" => send.pan = value,
            "B_MUTE" => send.mute = value != 0.0,
            "I_SENDMODE" => send.mode = value as i32,
            "I_SRCCHAN" => send.src_chan = value as i32,
            "I_DSTCHAN" => send.dst_chan = value as i32,
            other => {
                send.extra.insert(other.to_string(), value);
            }
        }
        s.undo_dirty = true;
        true
    })
}

unsafe extern "C" fn set_only_track_selected(track: *mut MediaTrack) {
    counted(|s| {
        s.master.selected = false;
        for t in s.tracks.iter_mut() {
            t.selected = false;
        }
        if let Some(t) = track_mut(s, track as u64) {
            t.selected = true;
        }
    })
}

unsafe extern "C" fn set_track_selected(track: *mut MediaTrack, selected: bool) {
    counted(|s| {
        if let Some(t) = track_mut(s, track as u64) {
            t.selected = selected;
        }
    })
}

/// Moves the selected tracks to just above `before`; the sends (which name their destination by position) follow.
unsafe extern "C" fn reorder_selected_tracks(before: c_int, _folder: c_int) -> bool {
    counted(|s| {
        let n = s.tracks.len();
        let moved: Vec<usize> = (0..n).filter(|&i| s.tracks[i].selected).collect();
        if moved.is_empty() {
            return false;
        }
        let before = (before.max(0) as usize).min(n);
        let mut order: Vec<usize> = (0..before).filter(|i| !moved.contains(i)).collect();
        order.extend(moved.iter().copied());
        order.extend((before..n).filter(|i| !moved.contains(i)));
        let mut position = vec![0usize; n];
        for (new, &old) in order.iter().enumerate() {
            position[old] = new;
        }
        let tracks = std::mem::take(&mut s.tracks);
        s.tracks = order.iter().map(|&old| tracks[old].clone()).collect();
        for track in s.tracks.iter_mut() {
            for send in track.sends.iter_mut() {
                send.dest = position[send.dest];
            }
        }
        s.undo_dirty = true;
        true
    })
}

unsafe extern "C" fn color_to_native(r: c_int, g: c_int, b: c_int) -> c_int {
    r | g << 8 | b << 16
}

unsafe extern "C" fn update_arrange() {
    with(|s| s.redraws += 1);
}

unsafe extern "C" fn track_list_adjust_windows(_minor: bool) {
    with(|s| s.redraws += 1);
}

// ----- the undo history -----

fn undo_cur_of(s: &State) -> i32 {
    s.undo_cur.unwrap_or(s.undo_entries - 1)
}

fn undo_desc_at(s: &State, index: i32) -> String {
    match index {
        0 => "--- Initial state ---".into(),
        n => s.undo_descs.get(n as usize - 1).cloned().unwrap_or_else(|| format!("Edit {n}")),
    }
}

/// The history gains a point at the end, after dropping what could still have been redone.
pub fn push_undo_point(s: &mut State, desc: &str) {
    let cur = undo_cur_of(s);
    while s.undo_descs.len() < cur as usize {
        let n = s.undo_descs.len() + 1;
        s.undo_descs.push(format!("Edit {n}"));
    }
    s.undo_descs.truncate(cur as usize);
    s.undo_descs.push(desc.to_string());
    s.undo_entries = cur + 2;
    s.undo_cur = None;
}

/// The producer's Edit > Undo: the history steps back, and REAPER counts a change.
pub fn producer_undo() {
    with(|s| {
        let cur = undo_cur_of(s);
        if cur > 0 {
            s.undo_cur = Some(cur - 1);
            s.state_count += 1;
        }
    })
}

/// The producer's Edit > Redo.
pub fn producer_redo() {
    with(|s| {
        let cur = undo_cur_of(s);
        if cur + 1 < s.undo_entries {
            s.undo_cur = Some(cur + 1);
            s.state_count += 1;
        }
    })
}

/// The producer edits something REAPER makes a point of.
pub fn producer_edit(desc: &str) {
    with(|s| {
        push_undo_point(s, desc);
        s.state_count += 1;
    })
}

unsafe extern "C" fn undo_begin_block2(_proj: *mut ReaProject) {
    with(|s| s.undo_depth += 1);
}

unsafe extern "C" fn undo_end_block2(_proj: *mut ReaProject, desc: *const c_char, flags: c_int) {
    let desc = name_of(desc);
    with(|s| {
        s.undo_depth = (s.undo_depth - 1).max(0);
        if s.undo_depth > 0 || flags == 0 {
            return;
        }
        // REAPER counts a flagged block whether or not it made a point.
        s.state_count += 1;
        if s.undo_dirty {
            push_undo_point(s, &desc);
            s.undo_dirty = false;
        }
    })
}

unsafe extern "C" fn undo_can_undo2(_proj: *mut ReaProject) -> *const c_char {
    let text = with(|s| {
        let cur = undo_cur_of(s);
        (cur > 0).then(|| undo_desc_at(s, cur))
    });
    text.map_or(ptr::null(), keep)
}

unsafe extern "C" fn undo_can_redo2(_proj: *mut ReaProject) -> *const c_char {
    let text = with(|s| {
        let cur = undo_cur_of(s);
        (cur + 1 < s.undo_entries).then(|| undo_desc_at(s, cur + 1))
    });
    text.map_or(ptr::null(), keep)
}

unsafe extern "C" fn undo_get_cur_entry(_proj: *mut ReaProject) -> c_int {
    with(|s| undo_cur_of(s))
}

unsafe extern "C" fn undo_get_entry_desc(_proj: *mut ReaProject, index: c_int) -> *const c_char {
    let text = with(|s| (index >= 0 && index < s.undo_entries).then(|| undo_desc_at(s, index)));
    text.map_or(ptr::null(), keep)
}

// ----- envelopes -----

unsafe extern "C" fn count_track_envelopes(track: *mut MediaTrack) -> c_int {
    counted(|s| track_of(s, track as u64).map_or(0, |t| t.envelopes.len() as c_int))
}
unsafe extern "C" fn get_track_envelope(track: *mut MediaTrack, index: c_int) -> *mut TrackEnvelope {
    counted(|s| match track_of(s, track as u64) {
        Some(t) if index >= 0 && (index as usize) < t.envelopes.len() => {
            pack(ENVELOPE, unpack(track as u64).1, index as u64, 0) as *mut TrackEnvelope
        }
        _ => ptr::null_mut(),
    })
}
unsafe extern "C" fn get_track_envelope_by_chunk_name(track: *mut MediaTrack, name: *const c_char) -> *mut TrackEnvelope {
    let name = name_of(name);
    counted(|s| {
        // A chunk name (`<VOLENV2`) or an envelope's GUID.
        let found = track_of(s, track as u64).and_then(|t| {
            t.envelopes.iter().position(|e| e.chunk.as_deref() == Some(name.as_str()) || (name.starts_with('{') && e.guid == name))
        });
        found.map_or(ptr::null_mut(), |n| pack(ENVELOPE, unpack(track as u64).1, n as u64, 0) as *mut TrackEnvelope)
    })
}
unsafe extern "C" fn get_fx_envelope(track: *mut MediaTrack, fx: c_int, param: c_int, create: bool) -> *mut TrackEnvelope {
    assert!(!create, "the fake only reads");
    counted(|s| {
        let Some(t) = track_of(s, track as u64) else { return ptr::null_mut() };
        let Some(guid) = fx_of(s, track as u64, fx).map(|f| f.guid.clone()) else { return ptr::null_mut() };
        let found = t.envelopes.iter().position(|e| e.fx_param.as_ref() == Some(&(guid.clone(), param)));
        found.map_or(ptr::null_mut(), |n| pack(ENVELOPE, unpack(track as u64).1, n as u64, 0) as *mut TrackEnvelope)
    })
}
/// The FX with this GUID in a chain, as the path of child positions that leads to it.
fn fx_path_of(chain: &[FakeFx], guid: &str) -> Option<Vec<usize>> {
    chain.iter().enumerate().find_map(|(n, fx)| {
        if fx.guid == guid {
            return Some(vec![n]);
        }
        let mut path = fx_path_of(fx.children.as_deref()?, guid)?;
        path.insert(0, n);
        Some(path)
    })
}
unsafe extern "C" fn envelope_get_parent_track(envelope: *mut TrackEnvelope, fx_out: *mut c_int, param_out: *mut c_int) -> *mut MediaTrack {
    counted(|s| {
        let (kind, a, _, _) = unpack(envelope as u64);
        let Some(e) = envelope_of(s, envelope as u64).filter(|_| kind == ENVELOPE) else { return ptr::null_mut() };
        let track = pack(TRACK, a, 0, 0);
        let fx_param = e.fx_param.as_ref().and_then(|(guid, param)| {
            let path = fx_path_of(&track_of(s, track)?.fx, guid)?;
            Some((fx_address(&path, false), *param))
        });
        unsafe {
            *fx_out = fx_param.map_or(-1, |(fx, _)| fx);
            *param_out = fx_param.map_or(-1, |(_, param)| param);
        }
        track as *mut MediaTrack
    })
}
unsafe extern "C" fn count_take_envelopes(take: *mut MediaItem_Take) -> c_int {
    counted(|s| take_of(s, take as u64).map_or(0, |t| t.envelopes.len() as c_int))
}
unsafe extern "C" fn get_take_envelope(take: *mut MediaItem_Take, index: c_int) -> *mut TrackEnvelope {
    counted(|s| match take_of(s, take as u64) {
        Some(t) if index >= 0 && (index as usize) < t.envelopes.len() => {
            let (_, a, b, c) = unpack(take as u64);
            pack(TAKE_ENVELOPE, a, b, c << 10 | index as u64) as *mut TrackEnvelope
        }
        _ => ptr::null_mut(),
    })
}
unsafe extern "C" fn get_envelope_name(envelope: *mut TrackEnvelope, buf: *mut c_char, size: c_int) -> bool {
    counted(|s| envelope_of(s, envelope as u64).map(|e| write(buf, size, &e.name)).is_some())
}
unsafe extern "C" fn count_envelope_points(envelope: *mut TrackEnvelope) -> c_int {
    counted(|s| envelope_of(s, envelope as u64).map_or(0, |e| e.points))
}
unsafe extern "C" fn get_set_envelope_info_string(envelope: *mut TrackEnvelope, parm: *const c_char, buf: *mut c_char, set: bool) -> bool {
    assert!(!set, "the fake only reads");
    let parm = name_of(parm);
    counted(|s| {
        let Some(e) = envelope_of(s, envelope as u64) else { return false };
        let flag = |on: bool| if on { "1" } else { "0" };
        match parm.as_str() {
            "ACTIVE" => write(buf, 4096, flag(e.active)),
            "ARM" => write(buf, 4096, flag(e.armed)),
            "VISIBLE" => write(buf, 4096, flag(e.visible)),
            "GUID" => write(buf, 4096, &e.guid),
            _ => return false,
        }
        true
    })
}

// ----- MIDI -----

unsafe extern "C" fn midi_count_evts(take: *mut MediaItem_Take, notes: *mut c_int, ccs: *mut c_int, texts: *mut c_int) -> c_int {
    counted(|s| match take_of(s, take as u64) {
        Some(t) if t.source.as_deref() == Some("MIDI") => {
            unsafe {
                *notes = t.notes.len() as c_int;
                *ccs = t.ccs.len() as c_int;
                *texts = 0;
            }
            (t.notes.len() + t.ccs.len()) as c_int
        }
        _ => 0,
    })
}
unsafe extern "C" fn midi_get_note(
    take: *mut MediaItem_Take,
    index: c_int,
    selected: *mut bool,
    muted: *mut bool,
    start: *mut f64,
    end: *mut f64,
    channel: *mut c_int,
    pitch: *mut c_int,
    velocity: *mut c_int,
) -> bool {
    counted(|s| match take_of(s, take as u64).and_then(|t| t.notes.get(index as usize)) {
        Some(n) => {
            unsafe {
                *selected = n.selected;
                *muted = n.muted;
                *start = n.start_ppq;
                *end = n.end_ppq;
                *channel = n.channel;
                *pitch = n.pitch;
                *velocity = n.velocity;
            }
            true
        }
        None => false,
    })
}
fn take_mut(s: &mut State, handle: u64) -> Option<&mut FakeTake> {
    let (kind, a, b, c) = unpack(handle);
    if kind != TAKE && kind != SOURCE {
        return None;
    }
    s.tracks.get_mut(a as usize)?.items.get_mut(b as usize)?.takes.get_mut(c as usize)
}

/// A take's notes and controller events as REAPER's event buffer holds them.
fn midi_events_of(take: &FakeTake) -> Vec<crate::midi::Event> {
    use crate::midi::Event;
    let mut keyed: Vec<((i64, u8, usize), Event)> = Vec::new();
    for (n, note) in take.notes.iter().enumerate() {
        let flag = note.selected as u8 | (note.muted as u8) << 1;
        let (start, end) = (note.start_ppq.round() as i64, note.end_ppq.round() as i64);
        keyed
            .push(((start, 2, n), Event { pos: start, flag, msg: vec![0x90 | note.channel as u8, note.pitch as u8, note.velocity as u8] }));
        keyed.push(((end, 0, n), Event { pos: end, flag, msg: vec![0x80 | note.channel as u8, note.pitch as u8, 0] }));
    }
    for (n, cc) in take.ccs.iter().enumerate() {
        let pos = cc.ppq.round() as i64;
        keyed.push(((pos, 1, n), Event { pos, flag: 0, msg: vec![cc.kind as u8 | cc.channel as u8, cc.data1 as u8, cc.data2 as u8] }));
    }
    keyed.sort_by_key(|(key, _)| *key);
    keyed.into_iter().map(|(_, event)| event).collect()
}

unsafe extern "C" fn midi_get_all_evts(take: *mut MediaItem_Take, buf: *mut c_char, size: *mut c_int) -> bool {
    counted(|s| {
        let Some(t) = take_of(s, take as u64) else { return false };
        let bytes = crate::midi::encode(&midi_events_of(t));
        let room = unsafe { *size } as usize;
        unsafe { *size = bytes.len() as c_int };
        if bytes.len() > room {
            return false;
        }
        unsafe { ptr::copy_nonoverlapping(bytes.as_ptr(), buf.cast(), bytes.len()) };
        true
    })
}

unsafe extern "C" fn midi_set_all_evts(take: *mut MediaItem_Take, buf: *const c_char, size: c_int) -> bool {
    let bytes = unsafe { std::slice::from_raw_parts(buf.cast::<u8>(), size.max(0) as usize) }.to_vec();
    counted(|s| {
        let Some(events) = crate::midi::decode(&bytes) else { return false };
        let Some(t) = take_mut(s, take as u64) else { return false };
        let (mut notes, mut ccs) = (crate::midi::notes(&events), Vec::new());
        notes.sort_by_key(|n| n.start);
        t.notes = notes
            .iter()
            .map(|n| FakeNote {
                start_ppq: n.start as f64,
                end_ppq: n.end as f64,
                channel: n.channel as i32,
                pitch: n.pitch as i32,
                velocity: n.velocity as i32,
                selected: n.selected,
                muted: n.muted,
            })
            .collect();
        for e in &events {
            if let [status, d1, d2] = e.msg.as_slice() {
                if status & 0xF0 == 0xB0 {
                    ccs.push(FakeCc {
                        ppq: e.pos as f64,
                        kind: 0xB0,
                        channel: (status & 0x0F) as i32,
                        data1: *d1 as i32,
                        data2: *d2 as i32,
                    });
                }
            }
        }
        t.ccs = ccs;
        s.undo_dirty = true;
        true
    })
}

unsafe extern "C" fn midi_get_cc(
    take: *mut MediaItem_Take,
    index: c_int,
    selected: *mut bool,
    muted: *mut bool,
    ppq: *mut f64,
    kind: *mut c_int,
    channel: *mut c_int,
    data1: *mut c_int,
    data2: *mut c_int,
) -> bool {
    counted(|s| match take_of(s, take as u64).and_then(|t| t.ccs.get(index as usize)) {
        Some(c) => {
            unsafe {
                *selected = false;
                *muted = false;
                *ppq = c.ppq;
                *kind = c.kind;
                *channel = c.channel;
                *data1 = c.data1;
                *data2 = c.data2;
            }
            true
        }
        None => false,
    })
}
/// 960 ticks to the quarter note; the take starts where its item does.
unsafe extern "C" fn midi_get_proj_qn_from_ppq_pos(take: *mut MediaItem_Take, ppq: f64) -> f64 {
    counted(|s| {
        let (_, t, i, _) = unpack(take as u64);
        let position = s.tracks.get(t as usize).and_then(|t| t.items.get(i as usize)).map_or(0.0, |i| i.position);
        position * s.bpm / 60.0 + ppq / 960.0
    })
}

unsafe extern "C" fn midi_get_ppq_pos_from_proj_qn(take: *mut MediaItem_Take, qn: f64) -> f64 {
    counted(|s| {
        let (_, t, i, _) = unpack(take as u64);
        let position = s.tracks.get(t as usize).and_then(|t| t.items.get(i as usize)).map_or(0.0, |i| i.position);
        (qn - position * s.bpm / 60.0) * 960.0
    })
}

/// The fake table: only the functions above resolve.
pub fn api() -> Api {
    Api::load(lookup)
}

/// The fake table of a REAPER without these functions, as an older one is.
pub fn api_without(missing: &[&str]) -> Api {
    Api::load(|name| if missing.iter().any(|m| m.as_bytes() == name.to_bytes()) { ptr::null_mut() } else { lookup(name) })
}

fn lookup(name: &CStr) -> *mut c_void {
    macro_rules! table {
        ($($name:literal => $f:expr),* $(,)?) => {
            match name.to_bytes() {
                $($name => $f as *mut c_void,)*
                _ => ptr::null_mut(),
            }
        };
    }
    table! {
        b"GetAppVersion" => get_app_version as unsafe extern "C" fn() -> *const c_char,
        b"GetResourcePath" => get_resource_path as unsafe extern "C" fn() -> *const c_char,
        b"EnumProjects" => enum_projects as unsafe extern "C" fn(c_int, *mut c_char, c_int) -> *mut ReaProject,
        b"GetProjectName" => get_project_name as unsafe extern "C" fn(*mut ReaProject, *mut c_char, c_int),
        b"GetCurrentProjectInLoadSave" => get_current_project_in_load_save as unsafe extern "C" fn() -> *mut ReaProject,
        b"GetPlayStateEx" => get_play_state_ex as unsafe extern "C" fn(*mut ReaProject) -> c_int,
        b"Master_GetTempo" => master_get_tempo as unsafe extern "C" fn() -> f64,
        b"GetProjectStateChangeCount" => get_project_state_change_count as unsafe extern "C" fn(*mut ReaProject) -> c_int,
        b"Undo_GetNumEntries" => undo_get_num_entries as unsafe extern "C" fn(*mut ReaProject) -> c_int,
        b"GetProjectLength" => get_project_length as unsafe extern "C" fn(*mut ReaProject) -> f64,
        b"GetCursorPositionEx" => get_cursor_position_ex as unsafe extern "C" fn(*mut ReaProject) -> f64,
        b"GetPlayPosition2Ex" => get_play_position2_ex as unsafe extern "C" fn(*mut ReaProject) -> f64,
        b"GetSet_LoopTimeRange2" => get_set_loop_time_range2 as unsafe extern "C" fn(*mut ReaProject, bool, bool, *mut f64, *mut f64, bool),
        b"GetSetRepeatEx" => get_set_repeat_ex as unsafe extern "C" fn(*mut ReaProject, c_int) -> c_int,
        b"TimeMap_GetTimeSigAtTime" => time_map_get_time_sig_at_time as unsafe extern "C" fn(*mut ReaProject, f64, *mut c_int, *mut c_int, *mut f64),
        b"TimeMap2_timeToQN" => time_map2_time_to_qn as unsafe extern "C" fn(*mut ReaProject, f64) -> f64,
        b"TimeMap2_QNToTime" => time_map2_qn_to_time as unsafe extern "C" fn(*mut ReaProject, f64) -> f64,
        b"TimeMap2_timeToBeats" => time_map2_time_to_beats as unsafe extern "C" fn(*mut ReaProject, f64, *mut c_int, *mut c_int, *mut f64, *mut c_int) -> f64,
        b"GetSet_ArrangeView2" => get_set_arrange_view2 as unsafe extern "C" fn(*mut ReaProject, bool, c_int, c_int, *mut f64, *mut f64),
        b"IsProjectDirty" => is_project_dirty as unsafe extern "C" fn(*mut ReaProject) -> c_int,
        b"Main_SaveProject" => main_save_project as unsafe extern "C" fn(*mut ReaProject, bool),
        b"Main_OnCommandEx" => main_on_command_ex as unsafe extern "C" fn(c_int, c_int, *mut ReaProject),
        b"PreventUIRefresh" => prevent_ui_refresh as unsafe extern "C" fn(c_int),
        b"kbd_getTextFromCmd" => kbd_get_text_from_cmd as unsafe extern "C" fn(c_int, *mut c_void) -> *const c_char,
        b"NamedCommandLookup" => named_command_lookup as unsafe extern "C" fn(*const c_char) -> c_int,
        b"GetMainHwnd" => get_main_hwnd as unsafe extern "C" fn() -> *mut c_void,
        b"ValidatePtr2" => validate_ptr2 as unsafe extern "C" fn(*mut ReaProject, *mut c_void, *const c_char) -> bool,
        b"GetNumRegionsOrMarkers" => get_num_regions_or_markers as unsafe extern "C" fn(*mut ReaProject) -> c_int,
        b"GetRegionOrMarker" => get_region_or_marker as unsafe extern "C" fn(*mut ReaProject, c_int, *const c_char) -> *mut api::ProjectMarker,
        b"GetRegionOrMarkerInfo_Value" => get_region_or_marker_info_value as unsafe extern "C" fn(*mut ReaProject, *mut api::ProjectMarker, *const c_char) -> f64,
        b"GetSetRegionOrMarkerInfo_String" => get_set_region_or_marker_info_string as unsafe extern "C" fn(*mut ReaProject, *mut api::ProjectMarker, *const c_char, *mut c_char, bool) -> bool,
        b"CountTempoTimeSigMarkers" => count_tempo_time_sig_markers as unsafe extern "C" fn(*mut ReaProject) -> c_int,
        b"GetTempoTimeSigMarker" => get_tempo_time_sig_marker as unsafe extern "C" fn(*mut ReaProject, c_int, *mut f64, *mut c_int, *mut f64, *mut f64, *mut c_int, *mut c_int, *mut bool) -> bool,
        b"CountTracks" => count_tracks as unsafe extern "C" fn(*mut ReaProject) -> c_int,
        b"GetTrack" => get_track as unsafe extern "C" fn(*mut ReaProject, c_int) -> *mut MediaTrack,
        b"GetMasterTrack" => get_master_track as unsafe extern "C" fn(*mut ReaProject) -> *mut MediaTrack,
        b"CountSelectedTracks2" => count_selected_tracks2 as unsafe extern "C" fn(*mut ReaProject, bool) -> c_int,
        b"GetSelectedTrack2" => get_selected_track2 as unsafe extern "C" fn(*mut ReaProject, c_int, bool) -> *mut MediaTrack,
        b"GetLastTouchedTrack" => get_last_touched_track as unsafe extern "C" fn() -> *mut MediaTrack,
        b"GetMediaTrackInfo_Value" => get_media_track_info_value as unsafe extern "C" fn(*mut MediaTrack, *const c_char) -> f64,
        b"SetMediaTrackInfo_Value" => set_media_track_info_value as unsafe extern "C" fn(*mut MediaTrack, *const c_char, f64) -> bool,
        b"InsertTrackAtIndex" => insert_track_at_index as unsafe extern "C" fn(c_int, bool),
        b"DeleteTrack" => delete_track as unsafe extern "C" fn(*mut MediaTrack),
        b"GetTrackStateChunk" => get_track_state_chunk as unsafe extern "C" fn(*mut MediaTrack, *mut c_char, c_int, bool) -> bool,
        b"SetTrackStateChunk" => set_track_state_chunk as unsafe extern "C" fn(*mut MediaTrack, *const c_char, bool) -> bool,
        b"CreateTrackSend" => create_track_send as unsafe extern "C" fn(*mut MediaTrack, *mut MediaTrack) -> c_int,
        b"RemoveTrackSend" => remove_track_send as unsafe extern "C" fn(*mut MediaTrack, c_int, c_int) -> bool,
        b"SetTrackSendInfo_Value" => set_track_send_info_value as unsafe extern "C" fn(*mut MediaTrack, c_int, c_int, *const c_char, f64) -> bool,
        b"SetOnlyTrackSelected" => set_only_track_selected as unsafe extern "C" fn(*mut MediaTrack),
        b"SetTrackSelected" => set_track_selected as unsafe extern "C" fn(*mut MediaTrack, bool),
        b"ReorderSelectedTracks" => reorder_selected_tracks as unsafe extern "C" fn(c_int, c_int) -> bool,
        b"AddRegionOrMarker" => add_region_or_marker as unsafe extern "C" fn(*mut ReaProject, bool, f64, f64, *const c_char, c_int, c_int) -> *mut api::ProjectMarker,
        b"SetRegionOrMarkerInfo_Value" => set_region_or_marker_info_value as unsafe extern "C" fn(*mut ReaProject, *mut api::ProjectMarker, *const c_char, f64) -> f64,
        b"DeleteProjectMarkerByIndex" => delete_project_marker_by_index as unsafe extern "C" fn(*mut ReaProject, c_int) -> bool,
        b"SetCurrentBPM" => set_current_bpm as unsafe extern "C" fn(*mut ReaProject, f64, bool),
        b"SetTempoTimeSigMarker" => set_tempo_time_sig_marker as unsafe extern "C" fn(*mut ReaProject, c_int, f64, c_int, f64, f64, c_int, c_int, bool) -> bool,
        b"DeleteTempoTimeSigMarker" => delete_tempo_time_sig_marker as unsafe extern "C" fn(*mut ReaProject, c_int) -> bool,
        b"SetEditCurPos2" => set_edit_cur_pos2 as unsafe extern "C" fn(*mut ReaProject, f64, bool, bool),
        b"ColorToNative" => color_to_native as unsafe extern "C" fn(c_int, c_int, c_int) -> c_int,
        b"UpdateArrange" => update_arrange as unsafe extern "C" fn(),
        b"TrackList_AdjustWindows" => track_list_adjust_windows as unsafe extern "C" fn(bool),
        b"Undo_BeginBlock2" => undo_begin_block2 as unsafe extern "C" fn(*mut ReaProject),
        b"Undo_EndBlock2" => undo_end_block2 as unsafe extern "C" fn(*mut ReaProject, *const c_char, c_int),
        b"Undo_CanUndo2" => undo_can_undo2 as unsafe extern "C" fn(*mut ReaProject) -> *const c_char,
        b"Undo_CanRedo2" => undo_can_redo2 as unsafe extern "C" fn(*mut ReaProject) -> *const c_char,
        b"Undo_GetCurEntry" => undo_get_cur_entry as unsafe extern "C" fn(*mut ReaProject) -> c_int,
        b"Undo_GetEntryDesc" => undo_get_entry_desc as unsafe extern "C" fn(*mut ReaProject, c_int) -> *const c_char,
        b"GetSetMediaTrackInfo" => get_set_media_track_info as unsafe extern "C" fn(*mut MediaTrack, *const c_char, *mut c_void) -> *mut c_void,
        b"GetSetMediaTrackInfo_String" => get_set_media_track_info_string as unsafe extern "C" fn(*mut MediaTrack, *const c_char, *mut c_char, bool) -> bool,
        b"GetTrackName" => get_track_name as unsafe extern "C" fn(*mut MediaTrack, *mut c_char, c_int) -> bool,
        b"GetProjExtState" => get_proj_ext_state as unsafe extern "C" fn(*mut ReaProject, *const c_char, *const c_char, *mut c_char, c_int) -> c_int,
        b"TrackFX_AddByName" => track_fx_add_by_name as unsafe extern "C" fn(*mut MediaTrack, *const c_char, bool, c_int) -> c_int,
        b"TrackFX_Delete" => track_fx_delete as unsafe extern "C" fn(*mut MediaTrack, c_int) -> bool,
        b"TrackFX_CopyToTrack" => track_fx_copy_to_track as unsafe extern "C" fn(*mut MediaTrack, c_int, *mut MediaTrack, c_int, bool),
        b"TrackFX_SetEnabled" => track_fx_set_enabled as unsafe extern "C" fn(*mut MediaTrack, c_int, bool),
        b"TrackFX_SetParamNormalized" => track_fx_set_param_normalized as unsafe extern "C" fn(*mut MediaTrack, c_int, c_int, f64) -> bool,
        b"TrackFX_FormatParamValueNormalized" => track_fx_format_param_value_normalized as unsafe extern "C" fn(*mut MediaTrack, c_int, c_int, f64, *mut c_char, c_int) -> bool,
        b"TrackFX_GetInstrument" => track_fx_get_instrument as unsafe extern "C" fn(*mut MediaTrack) -> c_int,
        b"GetTrackDepth" => get_track_depth as unsafe extern "C" fn(*mut MediaTrack) -> c_int,
        b"GetTrackColor" => get_track_color as unsafe extern "C" fn(*mut MediaTrack) -> c_int,
        b"ColorFromNative" => color_from_native as unsafe extern "C" fn(c_int, *mut c_int, *mut c_int, *mut c_int),
        b"CountTrackMediaItems" => count_track_media_items as unsafe extern "C" fn(*mut MediaTrack) -> c_int,
        b"GetTrackMediaItem" => get_track_media_item as unsafe extern "C" fn(*mut MediaTrack, c_int) -> *mut MediaItem,
        b"GetMediaItemTrack" => get_media_item_track as unsafe extern "C" fn(*mut MediaItem) -> *mut MediaTrack,
        b"GetMediaItemInfo_Value" => get_media_item_info_value as unsafe extern "C" fn(*mut MediaItem, *const c_char) -> f64,
        b"GetSetMediaItemInfo_String" => get_set_media_item_info_string as unsafe extern "C" fn(*mut MediaItem, *const c_char, *mut c_char, bool) -> bool,
        b"CountTakes" => count_takes as unsafe extern "C" fn(*mut MediaItem) -> c_int,
        b"GetMediaItemTake" => get_media_item_take as unsafe extern "C" fn(*mut MediaItem, c_int) -> *mut MediaItem_Take,
        b"GetActiveTake" => get_active_take as unsafe extern "C" fn(*mut MediaItem) -> *mut MediaItem_Take,
        b"GetMediaItemTake_Item" => get_media_item_take_item as unsafe extern "C" fn(*mut MediaItem_Take) -> *mut MediaItem,
        b"GetSetMediaItemTakeInfo_String" => get_set_media_item_take_info_string as unsafe extern "C" fn(*mut MediaItem_Take, *const c_char, *mut c_char, bool) -> bool,
        b"GetTakeName" => get_take_name as unsafe extern "C" fn(*mut MediaItem_Take) -> *const c_char,
        b"GetMediaItemTakeInfo_Value" => get_media_item_take_info_value as unsafe extern "C" fn(*mut MediaItem_Take, *const c_char) -> f64,
        b"GetMediaItemTake_Source" => get_media_item_take_source as unsafe extern "C" fn(*mut MediaItem_Take) -> *mut api::PCM_source,
        b"GetMediaSourceType" => get_media_source_type as unsafe extern "C" fn(*mut api::PCM_source, *mut c_char, c_int),
        b"stringToGuid" => string_to_guid as unsafe extern "C" fn(*const c_char, *mut api::GUID),
        b"guidToString" => guid_to_string as unsafe extern "C" fn(*const api::GUID, *mut c_char),
        b"GetMediaItemTakeByGUID" => get_media_item_take_by_guid as unsafe extern "C" fn(*mut ReaProject, *const api::GUID) -> *mut MediaItem_Take,
        b"CountSelectedMediaItems" => count_selected_media_items as unsafe extern "C" fn(*mut ReaProject) -> c_int,
        b"GetSelectedMediaItem" => get_selected_media_item as unsafe extern "C" fn(*mut ReaProject, c_int) -> *mut MediaItem,
        b"GetSelectedEnvelope" => get_selected_envelope as unsafe extern "C" fn(*mut ReaProject) -> *mut TrackEnvelope,
        b"GetCursorContext2" => get_cursor_context2 as unsafe extern "C" fn(bool) -> c_int,
        b"MIDIEditor_GetActive" => midi_editor_get_active as unsafe extern "C" fn() -> *mut c_void,
        b"MIDIEditor_GetTake" => midi_editor_get_take as unsafe extern "C" fn(*mut c_void) -> *mut MediaItem_Take,
        b"GetTouchedOrFocusedFX" => get_touched_or_focused_fx as unsafe extern "C" fn(c_int, *mut c_int, *mut c_int, *mut c_int, *mut c_int, *mut c_int) -> bool,
        b"TrackFX_GetCount" => track_fx_get_count as unsafe extern "C" fn(*mut MediaTrack) -> c_int,
        b"TakeFX_GetCount" => take_fx_get_count as unsafe extern "C" fn(*mut MediaItem_Take) -> c_int,
        b"TrackFX_GetRecCount" => track_fx_get_rec_count as unsafe extern "C" fn(*mut MediaTrack) -> c_int,
        b"TrackFX_GetFXGUID" => track_fx_get_fxguid as unsafe extern "C" fn(*mut MediaTrack, c_int) -> *mut api::GUID,
        b"TakeFX_GetFXGUID" => take_fx_get_fxguid as unsafe extern "C" fn(*mut MediaItem_Take, c_int) -> *mut api::GUID,
        b"TrackFX_GetFXName" => track_fx_get_fx_name as unsafe extern "C" fn(*mut MediaTrack, c_int, *mut c_char, c_int) -> bool,
        b"TakeFX_GetFXName" => take_fx_get_fx_name as unsafe extern "C" fn(*mut MediaItem_Take, c_int, *mut c_char, c_int) -> bool,
        b"TrackFX_GetEnabled" => track_fx_get_enabled as unsafe extern "C" fn(*mut MediaTrack, c_int) -> bool,
        b"TakeFX_GetEnabled" => take_fx_get_enabled as unsafe extern "C" fn(*mut MediaItem_Take, c_int) -> bool,
        b"TrackFX_GetOffline" => track_fx_get_offline as unsafe extern "C" fn(*mut MediaTrack, c_int) -> bool,
        b"TakeFX_GetOffline" => take_fx_get_offline as unsafe extern "C" fn(*mut MediaItem_Take, c_int) -> bool,
        b"TrackFX_GetPreset" => track_fx_get_preset as unsafe extern "C" fn(*mut MediaTrack, c_int, *mut c_char, c_int) -> bool,
        b"TakeFX_GetPreset" => take_fx_get_preset as unsafe extern "C" fn(*mut MediaItem_Take, c_int, *mut c_char, c_int) -> bool,
        b"TrackFX_GetNumParams" => track_fx_get_num_params as unsafe extern "C" fn(*mut MediaTrack, c_int) -> c_int,
        b"TakeFX_GetNumParams" => take_fx_get_num_params as unsafe extern "C" fn(*mut MediaItem_Take, c_int) -> c_int,
        b"TrackFX_GetNamedConfigParm" => track_fx_get_named_config_parm as unsafe extern "C" fn(*mut MediaTrack, c_int, *const c_char, *mut c_char, c_int) -> bool,
        b"TakeFX_GetNamedConfigParm" => take_fx_get_named_config_parm as unsafe extern "C" fn(*mut MediaItem_Take, c_int, *const c_char, *mut c_char, c_int) -> bool,
        b"TrackFX_GetParamName" => track_fx_get_param_name as unsafe extern "C" fn(*mut MediaTrack, c_int, c_int, *mut c_char, c_int) -> bool,
        b"TakeFX_GetParamName" => take_fx_get_param_name as unsafe extern "C" fn(*mut MediaItem_Take, c_int, c_int, *mut c_char, c_int) -> bool,
        b"TrackFX_GetParamNormalized" => track_fx_get_param_normalized as unsafe extern "C" fn(*mut MediaTrack, c_int, c_int) -> f64,
        b"TakeFX_GetParamNormalized" => take_fx_get_param_normalized as unsafe extern "C" fn(*mut MediaItem_Take, c_int, c_int) -> f64,
        b"TrackFX_GetFormattedParamValue" => track_fx_get_formatted_param_value as unsafe extern "C" fn(*mut MediaTrack, c_int, c_int, *mut c_char, c_int) -> bool,
        b"TakeFX_GetFormattedParamValue" => take_fx_get_formatted_param_value as unsafe extern "C" fn(*mut MediaItem_Take, c_int, c_int, *mut c_char, c_int) -> bool,
        b"TrackFX_GetParamEx" => track_fx_get_param_ex as unsafe extern "C" fn(*mut MediaTrack, c_int, c_int, *mut f64, *mut f64, *mut f64) -> f64,
        b"TakeFX_GetParamEx" => take_fx_get_param_ex as unsafe extern "C" fn(*mut MediaItem_Take, c_int, c_int, *mut f64, *mut f64, *mut f64) -> f64,
        b"GetTrackNumSends" => get_track_num_sends as unsafe extern "C" fn(*mut MediaTrack, c_int) -> c_int,
        b"GetTrackSendInfo_Value" => get_track_send_info_value as unsafe extern "C" fn(*mut MediaTrack, c_int, c_int, *const c_char) -> f64,
        b"GetSetTrackSendInfo_String" => get_set_track_send_info_string as unsafe extern "C" fn(*mut MediaTrack, c_int, c_int, *const c_char, *mut c_char, bool) -> bool,
        b"GetSetTrackSendInfo" => get_set_track_send_info as unsafe extern "C" fn(*mut MediaTrack, c_int, c_int, *const c_char, *mut c_void) -> *mut c_void,
        b"CountTrackEnvelopes" => count_track_envelopes as unsafe extern "C" fn(*mut MediaTrack) -> c_int,
        b"GetTrackEnvelope" => get_track_envelope as unsafe extern "C" fn(*mut MediaTrack, c_int) -> *mut TrackEnvelope,
        b"GetTrackEnvelopeByChunkName" => get_track_envelope_by_chunk_name as unsafe extern "C" fn(*mut MediaTrack, *const c_char) -> *mut TrackEnvelope,
        b"GetFXEnvelope" => get_fx_envelope as unsafe extern "C" fn(*mut MediaTrack, c_int, c_int, bool) -> *mut TrackEnvelope,
        b"Envelope_GetParentTrack" => envelope_get_parent_track as unsafe extern "C" fn(*mut TrackEnvelope, *mut c_int, *mut c_int) -> *mut MediaTrack,
        b"CountTakeEnvelopes" => count_take_envelopes as unsafe extern "C" fn(*mut MediaItem_Take) -> c_int,
        b"GetTakeEnvelope" => get_take_envelope as unsafe extern "C" fn(*mut MediaItem_Take, c_int) -> *mut TrackEnvelope,
        b"GetEnvelopeName" => get_envelope_name as unsafe extern "C" fn(*mut TrackEnvelope, *mut c_char, c_int) -> bool,
        b"CountEnvelopePoints" => count_envelope_points as unsafe extern "C" fn(*mut TrackEnvelope) -> c_int,
        b"GetSetEnvelopeInfo_String" => get_set_envelope_info_string as unsafe extern "C" fn(*mut TrackEnvelope, *const c_char, *mut c_char, bool) -> bool,
        b"MIDI_CountEvts" => midi_count_evts as unsafe extern "C" fn(*mut MediaItem_Take, *mut c_int, *mut c_int, *mut c_int) -> c_int,
        b"MIDI_GetNote" => midi_get_note as unsafe extern "C" fn(*mut MediaItem_Take, c_int, *mut bool, *mut bool, *mut f64, *mut f64, *mut c_int, *mut c_int, *mut c_int) -> bool,
        b"MIDI_GetAllEvts" => midi_get_all_evts as unsafe extern "C" fn(*mut MediaItem_Take, *mut c_char, *mut c_int) -> bool,
        b"MIDI_SetAllEvts" => midi_set_all_evts as unsafe extern "C" fn(*mut MediaItem_Take, *const c_char, c_int) -> bool,
        b"MIDI_GetCC" => midi_get_cc as unsafe extern "C" fn(*mut MediaItem_Take, c_int, *mut bool, *mut bool, *mut f64, *mut c_int, *mut c_int, *mut c_int, *mut c_int) -> bool,
        b"MIDI_GetPPQPosFromProjQN" => midi_get_ppq_pos_from_proj_qn as unsafe extern "C" fn(*mut MediaItem_Take, f64) -> f64,
        b"MIDI_GetProjQNFromPPQPos" => midi_get_proj_qn_from_ppq_pos as unsafe extern "C" fn(*mut MediaItem_Take, f64) -> f64,
    }
}

/// The same table as REAPER's GetFunc, for the entry point's `reaper_plugin_info_t`.
///
/// # Safety
/// `name` is a NUL-terminated string.
pub unsafe extern "C" fn get_func(name: *const c_char) -> *mut c_void {
    lookup(unsafe { CStr::from_ptr(name) })
}

/// A typical lab state: an unsaved project with two tracks.
pub fn reset() {
    with(|s| {
        *s = State {
            version: "7.82/macOS-arm64".into(),
            resource: "/lab/REAPER".into(),
            project: Some((String::new(), String::new())),
            tracks: vec![FakeTrack::new("{AAAA}", "Drums"), FakeTrack::new("{BBBB}", "Bass")],
            bpm: 120.0,
            ..State::default()
        }
    });
}

/// A colour as REAPER stores one (the OS's order, set flag): `(r, g, b)`.
pub fn native_color(r: i32, g: i32, b: i32) -> i32 {
    r | g << 8 | b << 16 | 0x100_0000
}

/// A small project with something of everything, for the reads' tests:
///
/// - Drums: a folder, with a ReaEQ, a container (ReaComp, and a container with a ReaDelay) and a volume envelope;
///   it sends to Reverb.
/// - Kick, Snare: Drums' children (Snare closes the folder). Kick has two audio items; Snare a MIDI item with notes.
/// - Reverb: a track with ReaVerb that receives from Drums, and a hardware output.
pub fn lab_project() {
    reset();
    with(|s| {
        s.project = Some(("/lab/Projects/Song.rpp".into(), "Song.rpp".into()));
        s.length = 64.0;
        s.state_count = 11;
        let mut eq = FakeFx::new("{FX-EQ}", "VST3: ReaEQ (Cockos)");
        eq.params = vec![
            FakeParam::new("Band 1 Freq", 0.25, "250 Hz"),
            FakeParam::new("Band 1 Gain", 0.5, "0.0 dB"),
            FakeParam::new("Wet", 1.0, "100 %"),
        ];
        eq.preset = "Warm".into();
        let comp = FakeFx::new("{FX-COMP}", "VST: ReaComp (Cockos)");
        let delay = FakeFx::new("{FX-DLY}", "VST: ReaDelay (Cockos)");
        let inner = FakeFx::container("{FX-C2}", vec![delay]);
        let mut outer = FakeFx::container("{FX-C}", vec![comp, inner]);
        outer.parallel = 0;
        let mut drums = FakeTrack::new("{AAAA}", "Drums");
        drums.folder = 1;
        drums.color = native_color(255, 0, 0);
        drums.volume = 0.5;
        drums.pan = -0.25;
        drums.fx = vec![eq, outer];
        drums.sends = vec![FakeSend::to(3)];
        drums.envelopes = vec![FakeEnvelope::own("Volume", "<VOLENV2", 4), FakeEnvelope::for_param("ReaEQ: Band 1 Gain", "{FX-EQ}", 1, 2)];
        let mut kick = FakeTrack::new("{BBBB}", "Kick");
        kick.items = vec![
            FakeItem::new("{ITEM-K1}", 0.0, 2.0, vec![FakeTake::wave("{TAKE-K1}", "kick.wav")]),
            FakeItem::new("{ITEM-K2}", 2.0, 2.0, vec![FakeTake::wave("{TAKE-K2}", "kick.wav")]),
        ];
        let mut snare = FakeTrack::new("{CCCC}", "Snare");
        snare.folder = -1;
        let mut take = FakeTake::midi("{TAKE-S1}", "snare.mid");
        take.notes = vec![
            FakeNote { start_ppq: 0.0, end_ppq: 480.0, channel: 0, pitch: 38, velocity: 100, selected: false, muted: false },
            FakeNote { start_ppq: 960.0, end_ppq: 1440.0, channel: 9, pitch: 40, velocity: 64, selected: true, muted: false },
        ];
        take.ccs = vec![
            FakeCc { ppq: 0.0, kind: 0xB0, channel: 0, data1: 7, data2: 100 },
            FakeCc { ppq: 480.0, kind: 0xB0, channel: 0, data1: 7, data2: 64 },
        ];
        snare.items = vec![FakeItem::new("{ITEM-S1}", 8.0, 4.0, vec![take])];
        let mut reverb = FakeTrack::new("{DDDD}", "Reverb");
        reverb.fx = vec![FakeFx::new("{FX-VERB}", "VST: ReaVerb (Cockos)")];
        reverb.hw_outs = vec![FakeSend { dst_chan: 2, ..FakeSend::to(0) }];
        s.tracks = vec![drums, kick, snare, reverb];
        s.markers = vec![
            FakeMarker { guid: "{MARK-1}".into(), is_region: false, start: 4.0, end: 4.0, name: "Drop".into(), number: 1, color: 0 },
            FakeMarker {
                guid: "{REG-1}".into(),
                is_region: true,
                start: 8.0,
                end: 16.0,
                name: "Verse".into(),
                number: 1,
                color: native_color(0, 255, 0),
            },
        ];
    });
}
