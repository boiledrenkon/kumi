//! Kumi's REAPER extension: loaded by REAPER from UserPlugins, it serves Kumi's bridge on loopback.
//!
//! REAPER calls in on its main thread only: the entry point, the registered timer, the hidden control surface and
//! atexit. Each is guarded so a Rust panic never unwinds into REAPER. The socket threads never touch REAPER: they
//! queue work that the timer and the surface's Run() drain (see `extension`).

pub mod api;
pub mod csurf;
pub mod extension;
pub mod log;
pub mod queue;
pub mod reaper;
pub mod server;

use std::cell::{Cell, RefCell};
use std::ffi::{c_char, c_int, c_void, CStr};
use std::panic::{self, AssertUnwindSafe};
use std::path::Path;
use std::rc::Rc;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use api::{Api, PluginInfo};
use csurf::{Call, Surface};
use extension::{Driver, Extension, Snapshot};
use queue::Job;
use server::{Config, Hub, Server};

/// Runs `f`, turning a panic into `fallback` and a log line: for every call that comes from REAPER or a thread.
pub fn guard<T>(edge: &str, fallback: T, f: impl FnOnce() -> T) -> T {
    match panic::catch_unwind(AssertUnwindSafe(f)) {
        Ok(value) => value,
        Err(payload) => {
            let why = payload.downcast_ref::<&str>().map(|s| s.to_string()).or_else(|| payload.downcast_ref::<String>().cloned());
            log::line(&format!("panic in {edge}: {}", why.unwrap_or_default()));
            fallback
        }
    }
}

type Register = unsafe extern "C" fn(name: *const c_char, info: *mut c_void) -> c_int;

/// How often a bridge without a server tries again (no reference yet, or every port taken).
const SERVER_RETRY: Duration = Duration::from_secs(5);

struct Loaded {
    register: Register,
    extension: Extension,
    surface: RefCell<Option<Surface>>,
    hub: Hub,
    jobs: Sender<Job>,
    snapshot: Arc<Mutex<Snapshot>>,
    server: RefCell<Option<Server>>,
    last_try: Cell<Option<Instant>>,
    last_failure: RefCell<String>,
    stopped: Cell<bool>,
}

thread_local! {
    static LOADED: RefCell<Option<Rc<Loaded>>> = const { RefCell::new(None) };
}

// The Rc is cloned out, so REAPER may unload the extension from inside a call (the quit action) without a borrow
// still held.
fn loaded() -> Option<Rc<Loaded>> {
    LOADED.with(|l| l.borrow().clone())
}

impl Loaded {
    fn register(&self, name: &CStr, info: *mut c_void) -> bool {
        unsafe { (self.register)(name.as_ptr(), info) != 0 }
    }

    fn tick(&self, driver: Driver) {
        if self.stopped.get() {
            return;
        }
        self.extension.tick(driver);
        self.ensure_server();
        if let Some(server) = self.server.borrow_mut().as_mut() {
            server.publish(&self.extension.snapshot());
        }
    }

    fn ensure_server(&self) {
        if self.server.borrow().is_some() || self.last_try.get().is_some_and(|t| t.elapsed() < SERVER_RETRY) {
            return;
        }
        self.last_try.set(Some(Instant::now()));
        let resource = self.extension.reaper().resource_path();
        let reference_path = server::reference_path(Path::new(&resource));
        let reference = match daw_bridge::files::Reference::read(&reference_path) {
            Ok(reference) => reference,
            Err(error) => {
                self.note(format!("not serving: {} ({error})", reference_path.display()));
                return;
            }
        };
        let config = Config::from_reference(&reference);
        log::set_dir(config.log_dir.clone());
        let started =
            config.bind().and_then(|listener| Server::start(listener, config, self.hub.clone(), self.jobs.clone(), self.snapshot.clone()));
        match started {
            Ok(server) => {
                log::line(&format!("serving on 127.0.0.1:{} (REAPER {})", server.port(), self.extension.reaper().app_version()));
                *self.server.borrow_mut() = Some(server);
            }
            Err(error) => self.note(format!("couldn't listen: {error}")),
        }
    }

    // A retry's failure, logged when it differs from the last one.
    fn note(&self, message: String) {
        if *self.last_failure.borrow() != message {
            log::line(&message);
            *self.last_failure.borrow_mut() = message;
        }
    }

    // Idempotent: atexit, then the unload entry call.
    fn stop(&self, reason: &str) {
        if self.stopped.replace(true) {
            return;
        }
        self.register(c"-timer", on_timer as *mut c_void);
        if let Some(surface) = self.surface.borrow_mut().take() {
            self.register(c"-csurf_inst", surface.as_ptr());
            drop(surface);
        }
        if let Some(server) = self.server.borrow_mut().take() {
            server.stop(reason);
        }
        log::line(&format!("stopped: {reason}"));
    }
}

extern "C" fn on_timer() {
    guard("timer", (), || {
        if let Some(loaded) = loaded() {
            loaded.tick(Driver::Timer);
        }
    })
}

fn on_surface(call: Call) -> i32 {
    let Some(loaded) = loaded() else { return 0 };
    if loaded.stopped.get() {
        return 0;
    }
    if call == Call::Run {
        loaded.tick(Driver::Surface);
        return 0;
    }
    loaded.extension.on_surface(call)
}

extern "C" fn on_exit() {
    guard("atexit", (), || {
        if let Some(loaded) = loaded() {
            loaded.stop("REAPER is quitting");
        }
    })
}

fn load(info: &PluginInfo) -> c_int {
    if info.caller_version != api::REAPER_PLUGIN_VERSION {
        return 0;
    }
    let (Some(register), Some(get_func)) = (info.register, info.get_func) else { return 0 };
    if loaded().is_some() {
        return 1;
    }
    let api = Api::load(|name| unsafe { get_func(name.as_ptr()) });
    let hub = Hub::default();
    let (jobs, jobs_rx) = queue::channel();
    let snapshot = Arc::new(Mutex::new(Snapshot::default()));
    let extension = Extension::new(api, jobs_rx, Box::new(hub.clone()), snapshot.clone());
    let Some(surface) = Surface::new(on_surface) else { return 0 };
    let loaded = Rc::new(Loaded {
        register,
        extension,
        surface: RefCell::new(None),
        hub,
        jobs,
        snapshot,
        server: RefCell::new(None),
        last_try: Cell::new(None),
        last_failure: RefCell::new(String::new()),
        stopped: Cell::new(false),
    });
    loaded.register(c"ext_name", c"Kumi".as_ptr() as *mut c_void);
    loaded.register(c"csurf_inst", surface.as_ptr());
    *loaded.surface.borrow_mut() = Some(surface);
    loaded.register(c"timer", on_timer as *mut c_void);
    // REAPER 7.55+; older ones still call the entry point to unload.
    loaded.register(c"atexit", on_exit as *mut c_void);
    LOADED.with(|l| *l.borrow_mut() = Some(loaded.clone()));
    loaded.ensure_server();
    1
}

fn unload() {
    if let Some(loaded) = LOADED.with(|l| l.borrow_mut().take()) {
        loaded.stop("REAPER unloaded the extension");
    }
}

/// REAPER's entry point: `info` to load, null to unload.
///
/// # Safety
/// Called by REAPER on its main thread with a valid `reaper_plugin_info_t` or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ReaperPluginEntry(_instance: *mut c_void, info: *mut PluginInfo) -> c_int {
    guard("entry", 0, || match unsafe { info.as_ref() } {
        Some(info) => load(info),
        None => {
            unload();
            0
        }
    })
}
