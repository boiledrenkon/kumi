//! REAPER's "a project is loading" signal: the hook it calls whenever a project is loaded into any tab, or a new one is
//! made, before the first line of the project is read. Registered as `projectconfig`, it is the one definitive sign
//! that the project in a tab is another one, where the file, the counter and the tracks are only hints (a template
//! opened over its own unedited copy changes none of them).
//!
//! The hook has nothing to read or save: it only counts. An undo's reload (`isUndo`) is not a load.

use std::cell::Cell;
use std::cell::UnsafeCell;
use std::ffi::{c_char, c_void};

thread_local! {
    // REAPER calls the hook on its main thread, which is also the one that ticks.
    static LOADS: Cell<u64> = const { Cell::new(0) };
}

/// How many projects have been loaded or made since the extension loaded.
pub fn loads() -> u64 {
    LOADS.with(Cell::get)
}

/// A load, as REAPER's hook reports it.
pub fn note_load() {
    LOADS.with(|loads| loads.set(loads.get() + 1));
}

/// `project_config_extension_t`, as reaper_plugin.h declares it.
#[repr(C)]
pub struct ProjectConfig {
    process_extension_line: unsafe extern "C" fn(line: *const c_char, ctx: *mut c_void, is_undo: bool, reg: *mut ProjectConfig) -> bool,
    save_extension_config: unsafe extern "C" fn(ctx: *mut c_void, is_undo: bool, reg: *mut ProjectConfig),
    begin_load_project_state: unsafe extern "C" fn(is_undo: bool, reg: *mut ProjectConfig),
    user_data: *mut c_void,
}

unsafe extern "C" fn process_extension_line(_: *const c_char, _: *mut c_void, _: bool, _: *mut ProjectConfig) -> bool {
    false
}

unsafe extern "C" fn save_extension_config(_: *mut c_void, _: bool, _: *mut ProjectConfig) {}

unsafe extern "C" fn begin_load_project_state(is_undo: bool, _: *mut ProjectConfig) {
    if !is_undo {
        note_load();
    }
}

/// The registration REAPER keeps a pointer to until it is unregistered.
pub struct Registration(UnsafeCell<ProjectConfig>);

// SAFETY: REAPER reads it on its main thread; nothing here writes it after it is made.
unsafe impl Sync for Registration {}

pub static REGISTRATION: Registration = Registration(UnsafeCell::new(ProjectConfig {
    process_extension_line,
    save_extension_config,
    begin_load_project_state,
    user_data: std::ptr::null_mut(),
}));

impl Registration {
    pub fn as_ptr(&self) -> *mut c_void {
        self.0.get().cast()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_load_counts_and_an_undo_does_not() {
        let before = loads();
        let reg = REGISTRATION.as_ptr().cast::<ProjectConfig>();
        unsafe {
            ((*reg).begin_load_project_state)(false, reg);
            ((*reg).begin_load_project_state)(true, reg);
            ((*reg).begin_load_project_state)(false, reg);
            assert!(!((*reg).process_extension_line)(c"<REAPER_PROJECT".as_ptr(), std::ptr::null_mut(), false, reg));
        }
        assert_eq!(loads(), before + 2);
    }
}
