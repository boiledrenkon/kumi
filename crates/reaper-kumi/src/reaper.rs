//! The REAPER calls the bridge makes, as safe functions over the API table. Main thread only (Api isn't Send).
//! A function this REAPER lacks reads as empty or None rather than failing.

use std::ffi::{c_char, CString};
use std::ptr;

use serde_json::{json, Value};

use crate::api::{self, Api, MediaTrack, ReaProject};

/// File: Quit REAPER.
pub const ACTION_QUIT: i32 = 40004;

pub struct Reaper<'a> {
    api: &'a Api,
}

/// A project REAPER handed out; only this module makes one, so the safe calls below get REAPER's own pointers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Project(*mut ReaProject);

/// A track REAPER handed out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Track(*mut MediaTrack);

impl Project {
    /// Tells projects apart (a tab switch) without exposing the pointer.
    pub fn addr(self) -> usize {
        self.0 as usize
    }
}

/// The project in the active tab.
#[derive(Debug, Clone, PartialEq)]
pub struct ProjectInfo {
    pub ptr: Project,
    /// The .rpp file; empty while unsaved.
    pub path: String,
    pub name: String,
}

/// Why REAPER can't take work: `status`'s `busy`, and the reason given with a busy error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Busy {
    Loading,
    Rendering,
    /// The main thread stopped ticking: a modal dialog, or a long load or render.
    Modal,
    Starting,
}

impl Busy {
    pub fn code(self) -> &'static str {
        match self {
            Busy::Loading => "loading",
            Busy::Rendering => "rendering",
            Busy::Modal => "modal",
            Busy::Starting => "starting",
        }
    }

    pub fn reason(self) -> &'static str {
        match self {
            Busy::Loading => "REAPER is loading or saving a project",
            Busy::Rendering => "REAPER is rendering",
            Busy::Modal => "REAPER isn't answering: a dialog may be open",
            Busy::Starting => "REAPER is starting",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PlayState {
    pub playing: bool,
    pub paused: bool,
    pub recording: bool,
}

impl PlayState {
    pub fn from_bits(bits: i32) -> PlayState {
        PlayState { playing: bits & 1 != 0, paused: bits & 2 != 0, recording: bits & 4 != 0 }
    }

    pub fn to_json(self) -> Value {
        json!({"playing": self.playing, "paused": self.paused, "recording": self.recording})
    }
}

impl<'a> Reaper<'a> {
    pub fn new(api: &'a Api) -> Self {
        Reaper { api }
    }

    pub fn api(&self) -> &'a Api {
        self.api
    }

    pub fn app_version(&self) -> String {
        self.api.GetAppVersion().map(|f| unsafe { api::string(f()) }).unwrap_or_default()
    }

    pub fn resource_path(&self) -> String {
        self.api.GetResourcePath().map(|f| unsafe { api::string(f()) }).unwrap_or_default()
    }

    pub fn current_project(&self) -> Option<ProjectInfo> {
        self.project_at(-1)
    }

    /// Every open project tab, in tab order.
    pub fn projects(&self) -> Vec<ProjectInfo> {
        (0..).map_while(|i| self.project_at(i)).collect()
    }

    fn project_at(&self, index: i32) -> Option<ProjectInfo> {
        let enum_projects = self.api.EnumProjects()?;
        let mut proj = ptr::null_mut();
        let path = api::read_buffer(|buf, size| proj = unsafe { enum_projects(index, buf, size) });
        if proj.is_null() {
            return None;
        }
        let name = match self.api.GetProjectName() {
            Some(f) => api::read_buffer(|buf, size| unsafe { f(proj, buf, size) }),
            None => String::new(),
        };
        Some(ProjectInfo { ptr: Project(proj), path, name })
    }

    pub fn is_dirty(&self, proj: Project) -> bool {
        self.api.IsProjectDirty().is_some_and(|f| unsafe { f(proj.0) } != 0)
    }

    /// Saves a project that has a file (one without would open Save As).
    pub fn save(&self, proj: Project) -> bool {
        match self.api.Main_SaveProject() {
            Some(f) => {
                unsafe { f(proj.0, false) };
                true
            }
            None => false,
        }
    }

    /// Whether a project is loading or saving, or one is rendering: no edits then. REAPER reports loading and saving
    /// as one state, so both read as `loading`.
    pub fn busy(&self) -> Option<Busy> {
        if self.api.GetCurrentProjectInLoadSave().is_some_and(|f| !unsafe { f() }.is_null()) {
            return Some(Busy::Loading);
        }
        if self.api.EnumProjects().is_some_and(|f| !unsafe { f(0x4000_0000, ptr::null_mut(), 0) }.is_null()) {
            return Some(Busy::Rendering);
        }
        None
    }

    pub fn play_state(&self, proj: Project) -> PlayState {
        PlayState::from_bits(self.api.GetPlayStateEx().map(|f| unsafe { f(proj.0) }).unwrap_or(0))
    }

    pub fn state_change_count(&self, proj: Project) -> Option<i32> {
        self.api.GetProjectStateChangeCount().map(|f| unsafe { f(proj.0) })
    }

    /// The project's tempo now, in beats per minute.
    pub fn tempo(&self) -> Option<f64> {
        self.api.Master_GetTempo().map(|f| unsafe { f() })
    }

    pub fn track_count(&self, proj: Project) -> i32 {
        self.api.CountTracks().map(|f| unsafe { f(proj.0) }).unwrap_or(0)
    }

    /// The selected tracks, the master first when it is selected.
    pub fn selected_tracks(&self, proj: Project) -> Vec<Track> {
        let (Some(count), Some(get)) = (self.api.CountSelectedTracks2(), self.api.GetSelectedTrack2()) else { return vec![] };
        let n = unsafe { count(proj.0, true) };
        (0..n).map(|i| unsafe { get(proj.0, i, true) }).filter(|t| !t.is_null()).map(Track).collect()
    }

    /// `{index, guid, name}`; index is 1-based as REAPER shows it, 0 for the master.
    pub fn track_json(&self, Track(track): Track) -> Value {
        let number = match self.api.GetMediaTrackInfo_Value() {
            Some(f) => unsafe { f(track, c"IP_TRACKNUMBER".as_ptr()) },
            None => 0.0,
        };
        let name = match self.api.GetTrackName() {
            Some(f) => api::read_buffer(|buf, size| {
                unsafe { f(track, buf, size) };
            }),
            None => String::new(),
        };
        let guid = match (self.api.GetTrackGUID(), self.api.guidToString()) {
            (Some(get), Some(to_string)) => {
                let guid = unsafe { get(track) };
                let mut buf = [0 as c_char; 64];
                if !guid.is_null() {
                    unsafe { to_string(guid, buf.as_mut_ptr()) };
                }
                unsafe { api::string(buf.as_ptr()) }
            }
            _ => String::new(),
        };
        let index = if number < 0.0 { 0 } else { number as i64 };
        json!({"index": index, "guid": guid, "name": name})
    }

    /// A main-section action's command id: a number, or a named command such as `_SWS_ABOUT`.
    pub fn command_id(&self, command: &str) -> Option<i32> {
        if let Ok(id) = command.parse::<i32>() {
            return (id > 0).then_some(id);
        }
        let lookup = self.api.NamedCommandLookup()?;
        let name = CString::new(command).ok()?;
        let id = unsafe { lookup(name.as_ptr()) };
        (id > 0).then_some(id)
    }

    /// The action's name as the action list shows it; None when there is no such action.
    pub fn action_name(&self, command: i32) -> Option<String> {
        let f = self.api.kbd_getTextFromCmd()?;
        let name = unsafe { api::string(f(command, ptr::null_mut())) };
        (!name.is_empty()).then_some(name)
    }

    pub fn run_action(&self, command: i32) -> bool {
        match self.api.Main_OnCommandEx() {
            Some(f) => {
                unsafe { f(command, 0, ptr::null_mut()) };
                true
            }
            None => false,
        }
    }

    pub fn prevent_ui_refresh(&self, count: i32) {
        if let Some(f) = self.api.PreventUIRefresh() {
            unsafe { f(count) }
        }
    }

    pub fn console(&self, text: &str) {
        if let (Some(f), Ok(text)) = (self.api.ShowConsoleMsg(), CString::new(text)) {
            unsafe { f(text.as_ptr()) }
        }
    }
}

/// A fake REAPER for tests: an API table whose functions read and write a thread-local state.
#[cfg(test)]
pub mod fake {
    use super::*;
    use std::cell::RefCell;
    use std::ffi::{c_int, c_void, CStr};

    #[derive(Default, Clone)]
    pub struct State {
        pub version: String,
        pub resource: String,
        /// None: no project (REAPER never has none, but the bridge must not care).
        pub project: Option<(String, String)>,
        pub play_bits: i32,
        pub bpm: f64,
        pub state_count: i32,
        pub tracks: Vec<(String, String)>,
        pub selected: Vec<usize>,
        pub loading: bool,
        pub dirty: bool,
        pub saved: u32,
        pub actions: Vec<i32>,
        pub ui_refresh: i32,
    }

    thread_local! {
        pub static STATE: RefCell<State> = RefCell::new(State::default());
    }

    pub fn with<T>(f: impl FnOnce(&mut State) -> T) -> T {
        STATE.with(|s| f(&mut s.borrow_mut()))
    }

    const PROJECT: *mut ReaProject = 0x10 as *mut ReaProject;

    fn track_ptr(index: usize) -> *mut MediaTrack {
        (0x1000 + index * 8) as *mut MediaTrack
    }

    fn track_index(track: *mut MediaTrack) -> usize {
        (track as usize - 0x1000) / 8
    }

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

    unsafe extern "C" fn get_app_version() -> *const c_char {
        keep(with(|s| s.version.clone()))
    }
    unsafe extern "C" fn get_resource_path() -> *const c_char {
        keep(with(|s| s.resource.clone()))
    }
    unsafe extern "C" fn enum_projects(idx: c_int, buf: *mut c_char, size: c_int) -> *mut ReaProject {
        with(|s| match (&s.project, idx) {
            (Some((path, _)), -1 | 0) => {
                write(buf, size, path);
                PROJECT
            }
            _ => ptr::null_mut(),
        })
    }
    unsafe extern "C" fn get_project_name(_proj: *mut ReaProject, buf: *mut c_char, size: c_int) {
        with(|s| write(buf, size, s.project.as_ref().map(|p| p.1.as_str()).unwrap_or("")))
    }
    unsafe extern "C" fn get_current_project_in_load_save() -> *mut ReaProject {
        if with(|s| s.loading) {
            PROJECT
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
        with(|s| s.state_count)
    }
    unsafe extern "C" fn count_tracks(_proj: *mut ReaProject) -> c_int {
        with(|s| s.tracks.len() as c_int)
    }
    unsafe extern "C" fn count_selected_tracks2(_proj: *mut ReaProject, _master: bool) -> c_int {
        with(|s| s.selected.len() as c_int)
    }
    unsafe extern "C" fn get_selected_track2(_proj: *mut ReaProject, i: c_int, _master: bool) -> *mut MediaTrack {
        with(|s| s.selected.get(i as usize).map(|&t| track_ptr(t)).unwrap_or(ptr::null_mut()))
    }
    unsafe extern "C" fn get_media_track_info_value(track: *mut MediaTrack, parm: *const c_char) -> f64 {
        assert_eq!(unsafe { CStr::from_ptr(parm) }, c"IP_TRACKNUMBER");
        track_index(track) as f64 + 1.0
    }
    unsafe extern "C" fn get_track_name(track: *mut MediaTrack, buf: *mut c_char, size: c_int) -> bool {
        with(|s| write(buf, size, &s.tracks[track_index(track)].1));
        true
    }
    unsafe extern "C" fn get_track_guid(track: *mut MediaTrack) -> *mut api::GUID {
        (track as usize + 1) as *mut api::GUID
    }
    unsafe extern "C" fn guid_to_string(guid: *const api::GUID, buf: *mut c_char) {
        let index = track_index((guid as usize - 1) as *mut MediaTrack);
        with(|s| write(buf, 64, &s.tracks[index].0));
    }
    unsafe extern "C" fn main_on_command_ex(command: c_int, _flag: c_int, _proj: *mut ReaProject) {
        with(|s| s.actions.push(command));
    }
    unsafe extern "C" fn prevent_ui_refresh(count: c_int) {
        with(|s| s.ui_refresh += count);
    }
    unsafe extern "C" fn is_project_dirty(_proj: *mut ReaProject) -> c_int {
        with(|s| s.dirty as c_int)
    }
    unsafe extern "C" fn main_save_project(_proj: *mut ReaProject, _save_as: bool) {
        with(|s| {
            s.dirty = false;
            s.saved += 1;
        });
    }
    unsafe extern "C" fn kbd_get_text_from_cmd(cmd: c_int, _section: *mut c_void) -> *const c_char {
        match cmd {
            40001 => c"Track: Insert new track".as_ptr(),
            40004 => c"File: Quit REAPER".as_ptr(),
            40022 => c"File: Save project as...".as_ptr(),
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

    /// The fake table: only the functions above resolve.
    pub fn api() -> Api {
        Api::load(|name| {
            let f: *mut c_void = match name.to_bytes() {
                b"GetAppVersion" => get_app_version as *mut c_void,
                b"GetResourcePath" => get_resource_path as *mut c_void,
                b"EnumProjects" => enum_projects as *mut c_void,
                b"GetProjectName" => get_project_name as *mut c_void,
                b"GetCurrentProjectInLoadSave" => get_current_project_in_load_save as *mut c_void,
                b"GetPlayStateEx" => get_play_state_ex as *mut c_void,
                b"Master_GetTempo" => master_get_tempo as *mut c_void,
                b"GetProjectStateChangeCount" => get_project_state_change_count as *mut c_void,
                b"CountTracks" => count_tracks as *mut c_void,
                b"CountSelectedTracks2" => count_selected_tracks2 as *mut c_void,
                b"GetSelectedTrack2" => get_selected_track2 as *mut c_void,
                b"GetMediaTrackInfo_Value" => get_media_track_info_value as *mut c_void,
                b"GetTrackName" => get_track_name as *mut c_void,
                b"GetTrackGUID" => get_track_guid as *mut c_void,
                b"guidToString" => guid_to_string as *mut c_void,
                b"Main_OnCommandEx" => main_on_command_ex as *mut c_void,
                b"PreventUIRefresh" => prevent_ui_refresh as *mut c_void,
                b"IsProjectDirty" => is_project_dirty as *mut c_void,
                b"Main_SaveProject" => main_save_project as *mut c_void,
                b"kbd_getTextFromCmd" => kbd_get_text_from_cmd as *mut c_void,
                b"NamedCommandLookup" => named_command_lookup as *mut c_void,
                _ => ptr::null_mut(),
            };
            f
        })
    }

    /// A typical lab state: an unsaved project with two tracks.
    pub fn reset() {
        with(|s| {
            *s = State {
                version: "7.82/macOS-arm64".into(),
                resource: "/lab/REAPER".into(),
                project: Some((String::new(), String::new())),
                tracks: vec![("{AAAA}".into(), "Drums".into()), ("{BBBB}".into(), "Bass".into())],
                bpm: 120.0,
                ..State::default()
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_go_through_the_table() {
        fake::reset();
        fake::with(|s| {
            s.project = Some(("/lab/Projects/a.rpp".into(), "a.rpp".into()));
            s.play_bits = 5;
            s.selected = vec![1];
        });
        let api = fake::api();
        let reaper = Reaper::new(&api);
        assert_eq!(reaper.app_version(), "7.82/macOS-arm64");
        assert_eq!(reaper.resource_path(), "/lab/REAPER");
        let project = reaper.current_project().unwrap();
        assert_eq!((project.path.as_str(), project.name.as_str()), ("/lab/Projects/a.rpp", "a.rpp"));
        assert_eq!(reaper.play_state(project.ptr), PlayState { playing: true, paused: false, recording: true });
        assert_eq!(reaper.track_count(project.ptr), 2);
        let selected = reaper.selected_tracks(project.ptr);
        assert_eq!(selected.len(), 1);
        assert_eq!(reaper.track_json(selected[0]), json!({"index": 2, "guid": "{BBBB}", "name": "Bass"}));
        assert_eq!(reaper.busy(), None);
        fake::with(|s| s.loading = true);
        assert_eq!(reaper.busy(), Some(Busy::Loading));
    }

    #[test]
    fn missing_functions_read_as_empty() {
        let api = Api::load(|_| ptr::null_mut());
        let reaper = Reaper::new(&api);
        assert_eq!(reaper.app_version(), "");
        assert_eq!(reaper.current_project(), None);
        assert_eq!(reaper.busy(), None);
        assert!(!reaper.run_action(ACTION_QUIT));
    }
}
