//! The render window's Cancel, pressed from a tick REAPER gives while its render runs (REAPER's render loop still calls
//! the extension's timer). Each platform finds the window its own way: AppKit's windows by title on macOS, the process's
//! top-level windows by title on Windows (not measured there). Elsewhere nothing is pressed, and the render runs to its
//! end (its files are then deleted).

/// Presses Cancel in REAPER's render window: false when no such window or button was found.
#[cfg(not(test))]
pub fn press() -> bool {
    platform::press()
}

/// Tests ask the fake REAPER.
#[cfg(test)]
pub fn press() -> bool {
    crate::reaper::fake::press_render_cancel()
}

/// The titles of REAPER's windows, for the log (what the render window is called where it couldn't be found).
#[cfg(not(test))]
pub fn titles() -> Vec<String> {
    platform::titles()
}

#[cfg(test)]
pub fn titles() -> Vec<String> {
    vec![]
}

/// Whether a window's title is the render window's ("Rendering to File…", "Render to File").
#[cfg_attr(test, allow(dead_code))]
fn is_render_window(title: &str) -> bool {
    let title = title.to_lowercase();
    title.contains("render") && !title.contains("queue")
}

#[cfg(target_os = "macos")]
#[cfg_attr(test, allow(dead_code))]
mod platform {
    use std::ffi::{c_char, c_void, CStr};

    type Id = *mut c_void;

    #[link(name = "objc")]
    unsafe extern "C" {
        fn objc_getClass(name: *const c_char) -> Id;
        fn sel_registerName(name: *const c_char) -> Id;
        fn objc_msgSend();
    }

    fn sel(name: &CStr) -> Id {
        unsafe { sel_registerName(name.as_ptr()) }
    }

    /// `[receiver selector]` returning an object.
    unsafe fn object(receiver: Id, selector: &CStr) -> Id {
        let send = unsafe { std::mem::transmute::<unsafe extern "C" fn(), unsafe extern "C" fn(Id, Id) -> Id>(objc_msgSend) };
        unsafe { send(receiver, sel(selector)) }
    }

    /// `[receiver count]`.
    unsafe fn count(receiver: Id) -> usize {
        let send = unsafe { std::mem::transmute::<unsafe extern "C" fn(), unsafe extern "C" fn(Id, Id) -> usize>(objc_msgSend) };
        unsafe { send(receiver, sel(c"count")) }
    }

    /// `[receiver objectAtIndex:index]`.
    unsafe fn at(receiver: Id, index: usize) -> Id {
        let send = unsafe { std::mem::transmute::<unsafe extern "C" fn(), unsafe extern "C" fn(Id, Id, usize) -> Id>(objc_msgSend) };
        unsafe { send(receiver, sel(c"objectAtIndex:"), index) }
    }

    /// `[receiver isKindOfClass:class]`.
    unsafe fn is_kind(receiver: Id, class: Id) -> bool {
        let send = unsafe { std::mem::transmute::<unsafe extern "C" fn(), unsafe extern "C" fn(Id, Id, Id) -> i8>(objc_msgSend) };
        unsafe { send(receiver, sel(c"isKindOfClass:"), class) != 0 }
    }

    /// `[receiver performClick:nil]`.
    unsafe fn click(receiver: Id) {
        let send = unsafe { std::mem::transmute::<unsafe extern "C" fn(), unsafe extern "C" fn(Id, Id, Id)>(objc_msgSend) };
        unsafe { send(receiver, sel(c"performClick:"), std::ptr::null_mut()) }
    }

    unsafe fn text(string: Id) -> String {
        if string.is_null() {
            return String::new();
        }
        let utf8 = unsafe { object(string, c"UTF8String") } as *const c_char;
        if utf8.is_null() {
            String::new()
        } else {
            unsafe { CStr::from_ptr(utf8) }.to_string_lossy().into_owned()
        }
    }

    unsafe fn windows() -> Vec<Id> {
        let class = unsafe { objc_getClass(c"NSApplication".as_ptr()) };
        if class.is_null() {
            return vec![];
        }
        let app = unsafe { object(class, c"sharedApplication") };
        if app.is_null() {
            return vec![];
        }
        let list = unsafe { object(app, c"windows") };
        if list.is_null() {
            return vec![];
        }
        (0..unsafe { count(list) }).map(|i| unsafe { at(list, i) }).filter(|w| !w.is_null()).collect()
    }

    /// A button titled `label` among a view's subviews, depth first.
    unsafe fn button(view: Id, button_class: Id, label: &str, depth: usize) -> Option<Id> {
        if view.is_null() || depth > 12 {
            return None;
        }
        if unsafe { is_kind(view, button_class) } && unsafe { text(object(view, c"title")) } == label {
            return Some(view);
        }
        let subviews = unsafe { object(view, c"subviews") };
        if subviews.is_null() {
            return None;
        }
        (0..unsafe { count(subviews) }).find_map(|i| unsafe { button(at(subviews, i), button_class, label, depth + 1) })
    }

    pub fn press() -> bool {
        let button_class = unsafe { objc_getClass(c"NSButton".as_ptr()) };
        if button_class.is_null() {
            return false;
        }
        for window in unsafe { windows() } {
            if !super::is_render_window(&unsafe { text(object(window, c"title")) }) {
                continue;
            }
            let content = unsafe { object(window, c"contentView") };
            if let Some(cancel) = unsafe { button(content, button_class, "Cancel", 0) } {
                unsafe { click(cancel) };
                return true;
            }
        }
        false
    }

    pub fn titles() -> Vec<String> {
        unsafe { windows() }.into_iter().map(|w| unsafe { text(object(w, c"title")) }).collect()
    }
}

#[cfg(windows)]
#[cfg_attr(test, allow(dead_code))]
mod platform {
    use std::ffi::c_void;

    #[link(name = "user32")]
    unsafe extern "system" {
        fn EnumWindows(callback: extern "system" fn(*mut c_void, isize) -> i32, data: isize) -> i32;
        fn GetWindowThreadProcessId(hwnd: *mut c_void, pid: *mut u32) -> u32;
        fn GetWindowTextW(hwnd: *mut c_void, text: *mut u16, max: i32) -> i32;
        fn IsWindowVisible(hwnd: *mut c_void) -> i32;
        fn GetDlgItem(hwnd: *mut c_void, id: i32) -> *mut c_void;
        fn PostMessageW(hwnd: *mut c_void, msg: u32, wparam: usize, lparam: isize) -> i32;
    }

    const IDCANCEL: i32 = 2;
    const BM_CLICK: u32 = 0x00F5;

    fn title(hwnd: *mut c_void) -> String {
        let mut text = [0u16; 256];
        let len = unsafe { GetWindowTextW(hwnd, text.as_mut_ptr(), text.len() as i32) };
        String::from_utf16_lossy(&text[..len.max(0) as usize])
    }

    /// This process's visible top-level windows.
    fn windows() -> Vec<*mut c_void> {
        extern "system" fn each(hwnd: *mut c_void, data: isize) -> i32 {
            let found = unsafe { &mut *(data as *mut Vec<*mut c_void>) };
            let mut pid = 0u32;
            unsafe { GetWindowThreadProcessId(hwnd, &mut pid) };
            if pid == std::process::id() && unsafe { IsWindowVisible(hwnd) } != 0 {
                found.push(hwnd);
            }
            1
        }
        let mut found: Vec<*mut c_void> = Vec::new();
        unsafe { EnumWindows(each, &mut found as *mut Vec<*mut c_void> as isize) };
        found
    }

    pub fn press() -> bool {
        for hwnd in windows() {
            if !super::is_render_window(&title(hwnd)) {
                continue;
            }
            let cancel = unsafe { GetDlgItem(hwnd, IDCANCEL) };
            if !cancel.is_null() {
                unsafe { PostMessageW(cancel, BM_CLICK, 0, 0) };
                return true;
            }
        }
        false
    }

    pub fn titles() -> Vec<String> {
        windows().into_iter().map(title).collect()
    }
}

#[cfg(all(not(windows), not(target_os = "macos")))]
#[cfg_attr(test, allow(dead_code))]
mod platform {
    pub fn press() -> bool {
        false
    }

    pub fn titles() -> Vec<String> {
        vec![]
    }
}
