//! The loaded extension on REAPER's main thread: each tick (REAPER's timer and the control surface's Run()) runs
//! queued requests within a budget, then turns what the control surface reported into events.

use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::api::{self, Api};
use crate::csurf::Call;
use crate::focus::Focus;
use crate::queue::{self, Answer, Budget, Job};
use crate::read::Reads;
use crate::reaper::{Busy, PlayState, ProjectInfo, Reaper, ACTION_QUIT};

pub const BRIDGE_VERSION: &str = env!("CARGO_PKG_VERSION");
const HEARTBEAT: Duration = Duration::from_millis(daw_bridge::wire::HEARTBEAT_MS);
/// How often `focus` is polled when nothing has said it changed (Kumi asks about every 150 ms).
const FOCUS_EVERY: Duration = Duration::from_millis(50);

// Actions that close or replace the project, and so ask "Save changes?" in a dialog inside the request when it has any.
const CLOSES_CURRENT_PROJECT: [i32; 2] = [40860, 40023];
const CLOSES_ALL_PROJECTS: [i32; 1] = [40886];
// Actions that render with a progress window of their own, which holds the main thread for the whole render.
const RENDERS: [i32; 2] = [41824, 42230];
const SAVE_PROJECT: i32 = 40026;

/// Where the main thread's events and heartbeats go: the socket server, or a test's list.
pub trait Outbox {
    fn event(&self, name: &str, data: Value);
    fn heartbeat(&self);
}

/// Clears the flag when dropped.
struct ClearOnDrop<'a>(&'a Cell<bool>);

impl Drop for ClearOnDrop<'_> {
    fn drop(&mut self) {
        self.0.set(false);
    }
}

/// REAPER's UI refresh held (PreventUIRefresh +1) from the first `hold`, and let go when dropped.
struct RefreshHold<'a> {
    reaper: &'a Reaper<'a>,
    held: bool,
}

impl RefreshHold<'_> {
    fn hold(&mut self) {
        if !self.held {
            self.reaper.prevent_ui_refresh(1);
            self.held = true;
        }
    }
}

impl Drop for RefreshHold<'_> {
    fn drop(&mut self) {
        if self.held {
            self.reaper.prevent_ui_refresh(-1);
        }
    }
}

/// What the socket threads may read without the main thread: who this bridge is, and when it last ticked.
#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    pub daw_version: String,
    pub resource: String,
    pub project: Option<ProjectSummary>,
    pub last_tick: Option<Instant>,
    /// Loading, saving or rendering, as of the last tick.
    pub busy: Option<Busy>,
    /// `status`'s fields read on the main thread: transport, track count, tick rates.
    pub status: Value,
    /// What `focus` answers, polled each tick (Null until the first poll finds a project).
    pub focus: Value,
    /// When `focus` was polled.
    pub focus_at: Option<Instant>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct ProjectSummary {
    pub name: String,
    pub path: String,
}

impl ProjectSummary {
    pub fn to_json(&self) -> Value {
        let mut project = serde_json::Map::new();
        // As long as the discovery file and the welcome keep them.
        if !self.name.is_empty() {
            project.insert("name".into(), crate::server::cut(&self.name, crate::server::MAX_NAME).into());
        }
        if !self.path.is_empty() {
            project.insert("path".into(), crate::server::cut(&self.path, crate::server::MAX_PATH).into());
        }
        Value::Object(project)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Driver {
    Timer,
    Surface,
}

// What the control surface reported since the last tick.
#[derive(Default)]
struct Pending {
    tracks: bool,
    selection: bool,
    play: Option<PlayState>,
    tempo: bool,
    // Something `focus` shows moved (selection, transport, FX focus): poll it this tick.
    focus: bool,
}

/// `{playing, paused, recording, bpm}`: the keys the Renoise bridge shares (`playing`, `bpm`) plus REAPER's own.
fn transport_json(play: PlayState, bpm: Option<f64>) -> Value {
    let mut transport = play.to_json();
    if let Some(bpm) = bpm {
        transport["bpm"] = bpm.into();
    }
    transport
}

fn bad_params(message: &str) -> (String, String) {
    ("bad-params".into(), message.into())
}

// What was last sent, so a tick sends only changes.
#[derive(Default)]
struct Sent {
    project: Option<(usize, ProjectSummary)>,
    transport: Option<Value>,
}

/// How long the set of track GUIDs a project had may be before it is read again when the project changes.
const WATCH_REFRESH: Duration = Duration::from_millis(250);
/// The most track GUIDs kept to tell one project from another.
const WATCH_TRACKS: i32 = 2000;

/// What the bridge remembers of the project in the active tab, to tell a new project from the same one changed. A
/// project opened in the tab where another was (File > Open, New project) keeps the tab's pointer, so the pointer alone
/// doesn't say; the file, the state change count and the tracks' GUIDs do.
#[derive(Default)]
struct Watch {
    project: Option<usize>,
    path: String,
    name: String,
    count: Option<i32>,
    tracks: HashSet<String>,
    tracks_at: Option<Instant>,
}

fn track_guids(reaper: &Reaper, project: crate::reaper::Project) -> HashSet<String> {
    (0..reaper.track_count(project).min(WATCH_TRACKS)).filter_map(|i| reaper.track_at(project, i)).map(|t| reaper.track_guid(t)).collect()
}

/// Whether the tracks now are another project's than the tracks before: they share none; or one side has none and
/// the file changed or the counter went back; or neither has any and the file changed.
fn is_another_project(before: &HashSet<String>, now: &HashSet<String>, file_changed: bool, counter_went_back: bool) -> bool {
    match (before.is_empty(), now.is_empty()) {
        (false, false) => before.is_disjoint(now),
        (true, true) => file_changed,
        _ => file_changed || counter_went_back,
    }
}

// Ticks per second for each driver, measured over the last second.
#[derive(Default)]
struct Rates {
    window_start: Option<Instant>,
    timer: u32,
    surface: u32,
    timer_hz: u32,
    surface_hz: u32,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Quit {
    No,
    // Answered this tick; REAPER quits next tick, after the answer is on its way.
    Answered,
}

pub struct Extension {
    api: Api,
    reads: Reads,
    focus: Focus,
    jobs: Receiver<Job>,
    out: Box<dyn Outbox>,
    snapshot: Arc<Mutex<Snapshot>>,
    budget: Budget,
    pending: RefCell<Pending>,
    sent: RefCell<Sent>,
    watch: RefCell<Watch>,
    rates: RefCell<Rates>,
    in_tick: Cell<bool>,
    quit: Cell<Quit>,
    last_heartbeat: Cell<Option<Instant>>,
    focus_value: RefCell<Value>,
    focus_at: Cell<Option<Instant>>,
}

impl Extension {
    pub fn new(api: Api, jobs: Receiver<Job>, out: Box<dyn Outbox>, snapshot: Arc<Mutex<Snapshot>>) -> Extension {
        let extension = Extension {
            api,
            reads: Reads::default(),
            focus: Focus::default(),
            jobs,
            out,
            snapshot,
            budget: Budget::default(),
            pending: RefCell::default(),
            sent: RefCell::default(),
            watch: RefCell::default(),
            rates: RefCell::default(),
            in_tick: Cell::new(false),
            quit: Cell::new(Quit::No),
            last_heartbeat: Cell::new(None),
            focus_value: RefCell::new(Value::Null),
            focus_at: Cell::new(None),
        };
        extension.refresh_snapshot(None);
        extension
    }

    pub fn reaper(&self) -> Reaper<'_> {
        Reaper::new(&self.api)
    }

    /// A control surface callback. Returns what `Extended` answers.
    pub fn on_surface(&self, call: Call) -> i32 {
        let mut pending = self.pending.borrow_mut();
        match call {
            Call::Run => {
                drop(pending);
                self.tick(Driver::Surface);
            }
            Call::TrackListChange => {
                pending.tracks = true;
                pending.focus = true;
            }
            Call::Selected(..) | Call::TrackSelection(_) => {
                pending.selection = true;
                pending.focus = true;
            }
            Call::PlayState { play, pause, rec } => {
                pending.play = Some(PlayState { playing: play, paused: pause, recording: rec });
                pending.focus = true;
            }
            Call::Extended { call: api::CSURF_EXT_SETBPMANDPLAYRATE, .. } => {
                pending.tempo = true;
                pending.focus = true;
            }
            Call::Extended {
                call:
                    api::CSURF_EXT_SETFOCUSEDFX
                    | api::CSURF_EXT_SETLASTTOUCHEDFX
                    | api::CSURF_EXT_SETFXOPEN
                    | api::CSURF_EXT_SETFXCHANGE
                    | api::CSURF_EXT_SETLASTTOUCHEDTRACK,
                ..
            } => pending.focus = true,
            _ => {}
        }
        0
    }

    pub fn tick(&self, driver: Driver) {
        // A request's REAPER call can call back into the surface (Run included): that tick is skipped.
        if self.in_tick.replace(true) {
            return;
        }
        // Cleared on the way out whatever happens, so one panic caught above doesn't skip every later tick.
        let _in_tick = ClearOnDrop(&self.in_tick);
        self.count_tick(driver);
        // Only the timer quits: REAPER may unload the extension inside the action, and the control surface's Run()
        // would then be running inside the surface being deleted.
        if self.quit.get() == Quit::Answered && driver == Driver::Timer {
            // REAPER may unload the extension inside this call; nothing more this tick.
            self.quit.set(Quit::No);
            self.reaper().run_action(ACTION_QUIT);
            return;
        }
        let busy = self.reaper().busy();
        if busy.is_none() {
            self.run_jobs();
        }
        self.poll_project(busy.is_none());
        if busy.is_none() {
            self.poll_focus();
        }
        self.flush_events();
        self.refresh_snapshot(busy);
        let now = Instant::now();
        if self.last_heartbeat.get().is_none_or(|last| now.duration_since(last) >= HEARTBEAT) {
            self.last_heartbeat.set(Some(now));
            self.out.heartbeat();
        }
    }

    fn run_jobs(&self) {
        let reaper = self.reaper();
        // Held by the first request, released on the way out even if something unwinds: an unbalanced hold leaves
        // REAPER's arrange view frozen.
        let mut refresh = RefreshHold { reaper: &reaper, held: false };
        queue::drain(&self.jobs, self.budget, |job| {
            refresh.hold();
            let answer = crate::guard("request", Err(("internal".into(), "the request failed inside the bridge".into())), || {
                self.handle(&job.method, &job.params)
            });
            job.answer(answer);
        });
    }

    fn handle(&self, method: &str, params: &Value) -> Answer {
        match method {
            "action" => self.action(params),
            "quit" => self.quit(params),
            "read" => self.reads.read(&self.reaper(), params),
            "snapshot" => self.reads.snapshot(&self.reaper()),
            "focus_body" => self.reads.focus_body(&self.reaper(), params),
            other => Err(("unknown-method".into(), format!("the REAPER bridge has no method {other:?}"))),
        }
    }

    // Runs a main-section action by id or command name. Actions that open a dialog (named "…") never run unattended.
    fn action(&self, params: &Value) -> Answer {
        let reaper = self.reaper();
        let command = match &params["command"] {
            Value::Number(n) => n.to_string(),
            Value::String(s) => s.clone(),
            _ => return Err(bad_params("command: an action id or a command name")),
        };
        let Some(id) = reaper.command_id(&command) else { return Err(bad_params(&format!("no action {command:?}"))) };
        let Some(name) = reaper.action_name(id) else { return Err(bad_params(&format!("no action {command:?}"))) };
        if name.ends_with("...") || name.ends_with('…') {
            return Err(bad_params(&format!("{name:?} opens a dialog, which Kumi never runs unattended")));
        }
        if id == ACTION_QUIT {
            return Err(bad_params("use quit, which checks for unsaved changes first"));
        }
        if RENDERS.contains(&id) {
            return Err(bad_params(&format!("{name:?} renders with a progress window that holds REAPER's main thread")));
        }
        // These would ask "Save changes?" in a dialog, inside this request, if a project they close has changes.
        let closing = if CLOSES_ALL_PROJECTS.contains(&id) {
            reaper.projects()
        } else if CLOSES_CURRENT_PROJECT.contains(&id) {
            reaper.current_project().into_iter().collect()
        } else {
            vec![]
        };
        let dirty: Vec<&str> = closing.iter().filter(|p| reaper.is_dirty(p.ptr)).map(|p| p.name.as_str()).collect();
        if !dirty.is_empty() {
            let names = dirty.iter().map(|name| if name.is_empty() { "(unsaved project)" } else { name }).collect::<Vec<_>>();
            return Err(("failed".into(), format!("{name:?} would ask about unsaved changes in {}: save them first", names.join(", "))));
        }
        // Saving a project that was never saved opens Save As.
        if id == SAVE_PROJECT && reaper.current_project().is_some_and(|p| p.path.is_empty()) {
            return Err(("failed".into(), "this project was never saved, so Save would open Save As: save it in REAPER first".into()));
        }
        reaper.run_action(id);
        Ok(json!({"command": id, "name": name}))
    }

    // Quits on the next tick. REAPER would ask about unsaved changes in a dialog, so those are saved first when asked
    // (`save: true`), and refused otherwise; a project never saved can't be saved without Save As.
    fn quit(&self, params: &Value) -> Answer {
        let reaper = self.reaper();
        let save = params["save"].as_bool().unwrap_or(false);
        if reaper.projects().iter().any(|p| reaper.play_state(p.ptr).recording) {
            return Err((
                "failed".into(),
                "REAPER is recording: quitting would ask what to do with the recorded files; stop it first".into(),
            ));
        }
        let dirty: Vec<ProjectInfo> = reaper.projects().into_iter().filter(|p| reaper.is_dirty(p.ptr)).collect();
        if dirty.iter().any(|p| p.path.is_empty()) {
            return Err(("failed".into(), "a project that was never saved has changes: save it in REAPER first".into()));
        }
        if !dirty.is_empty() && !save {
            let names: Vec<&str> = dirty.iter().map(|p| p.name.as_str()).collect();
            return Err(("failed".into(), format!("unsaved changes in {}: pass save: true to save them first", names.join(", "))));
        }
        for project in &dirty {
            reaper.save(project.ptr);
        }
        // Saving can fail (a read-only folder, a plug-in refusing) and leave the project changed: REAPER would then
        // ask about it in a dialog when it quits, so look again.
        let still: Vec<&str> = dirty.iter().filter(|p| reaper.is_dirty(p.ptr)).map(|p| p.name.as_str()).collect();
        if !still.is_empty() {
            return Err((
                "failed".into(),
                format!("{} still has unsaved changes after saving: REAPER would ask about them", still.join(", ")),
            ));
        }
        self.quit.set(Quit::Answered);
        Ok(json!({"quitting": true, "saved": dirty.iter().map(|p| p.path.clone()).collect::<Vec<_>>()}))
    }

    // What `status` adds to the bridge's identity; the socket threads serve it from the snapshot.
    fn status(&self, project: Option<&ProjectInfo>) -> Value {
        let reaper = self.reaper();
        let rates = self.rates.borrow();
        let mut status = json!({
            "sdk": api::SDK_VERSION,
            "api": {"resolved": self.api.resolved(), "total": api::FUNCTION_COUNT},
            "ticksPerSecond": {"timer": rates.timer_hz, "surface": rates.surface_hz},
        });
        if let Some(project) = project {
            status["transport"] = transport_json(reaper.play_state(project.ptr), reaper.tempo());
            status["tracks"] = reaper.track_count(project.ptr).into();
            if let Some(count) = reaper.state_change_count(project.ptr) {
                status["stateChangeCount"] = count.into();
            }
        }
        status
    }

    // A tab switch, a project opened or closed, or saved under another name; and what a project's pointer can't show: a new
    // project opened in the tab where another was. `settled` is false while REAPER loads or renders.
    fn poll_project(&self, settled: bool) {
        let reaper = self.reaper();
        let info = reaper.current_project();
        let (switched, replaced) = if settled { self.watch_project(&reaper, info.as_ref()) } else { (false, false) };
        let current = info.as_ref().map(|p| (p.ptr.addr(), ProjectSummary { name: p.name.clone(), path: p.path.clone() }));
        let mut sent = self.sent.borrow_mut();
        let changed = sent.project != current;
        let first = sent.project.is_none();
        if changed {
            sent.project = current.clone();
        }
        drop(sent);
        if switched || replaced {
            // Refs name objects of the project in this tab: they mean something else now.
            self.reads.bump_epoch();
            self.focus.reset();
            if let Some(info) = info.as_ref().filter(|_| replaced) {
                self.reads.project_replaced(info.ptr);
            }
        }
        if (changed && !first) || replaced {
            self.out.event("project", current.map(|(_, p)| p.to_json()).unwrap_or(Value::Null));
            let mut pending = self.pending.borrow_mut();
            pending.tracks = true;
            pending.selection = true;
            pending.focus = true;
        }
    }

    // Compares the active tab's project with what it was: (the tab changed, another project took its place in the tab).
    fn watch_project(&self, reaper: &Reaper, info: Option<&ProjectInfo>) -> (bool, bool) {
        let mut watch = self.watch.borrow_mut();
        let Some(info) = info else {
            *watch = Watch::default();
            return (false, false);
        };
        let addr = info.ptr.addr();
        let count = reaper.state_change_count(info.ptr);
        let fresh = |watch: &mut Watch| {
            *watch = Watch {
                project: Some(addr),
                path: info.path.clone(),
                name: info.name.clone(),
                count,
                tracks: track_guids(reaper, info.ptr),
                tracks_at: Some(Instant::now()),
            };
        };
        match watch.project {
            None => {
                fresh(&mut watch);
                (false, false)
            }
            Some(before) if before != addr => {
                fresh(&mut watch);
                (true, false)
            }
            Some(_) => {
                let file_changed = watch.path != info.path || watch.name != info.name;
                let counter_went_back = matches!((watch.count, count), (Some(before), Some(now)) if now < before);
                let due = count != watch.count && watch.tracks_at.is_none_or(|at| at.elapsed() >= WATCH_REFRESH);
                let mut replaced = false;
                if file_changed || counter_went_back || due {
                    let now = track_guids(reaper, info.ptr);
                    replaced = is_another_project(&watch.tracks, &now, file_changed, counter_went_back);
                    watch.tracks = now;
                    watch.tracks_at = Some(Instant::now());
                }
                watch.path = info.path.clone();
                watch.name = info.name.clone();
                watch.count = count;
                (false, replaced)
            }
        }
    }

    // `focus`, polled every FOCUS_EVERY, or at once when the surface said something it shows moved. The selection part
    // of it is read now when REAPER said the selection moved, and by its own throttle otherwise.
    fn poll_focus(&self) {
        let (urgent, selection) = {
            let pending = self.pending.borrow();
            (pending.focus, pending.selection)
        };
        let due = urgent || self.focus_at.get().is_none_or(|at| at.elapsed() >= FOCUS_EVERY);
        if !due {
            return;
        }
        let reaper = self.reaper();
        let value = match reaper.current_project() {
            Some(project) => self.focus.poll(&self.reads, &reaper, project.ptr, selection),
            None => Value::Null,
        };
        *self.focus_value.borrow_mut() = value;
        self.focus_at.set(Some(Instant::now()));
        self.pending.borrow_mut().focus = false;
    }

    fn flush_events(&self) {
        let pending = std::mem::take(&mut *self.pending.borrow_mut());
        if !pending.tracks && !pending.selection && pending.play.is_none() && !pending.tempo {
            return;
        }
        let reaper = self.reaper();
        let Some(project) = reaper.current_project() else { return };
        if pending.play.is_some() || pending.tempo {
            let play = pending.play.unwrap_or_else(|| reaper.play_state(project.ptr));
            let transport = transport_json(play, reaper.tempo());
            let mut sent = self.sent.borrow_mut();
            if sent.transport.as_ref() != Some(&transport) {
                sent.transport = Some(transport.clone());
                drop(sent);
                self.out.event("transport", transport);
            }
        }
        if pending.tracks {
            self.out.event("tracks", json!({"count": reaper.track_count(project.ptr)}));
        }
        if pending.selection {
            let tracks: Vec<Value> = reaper.selected_tracks(project.ptr).into_iter().map(|t| reaper.track_json(t)).collect();
            self.out.event("selection", json!({"tracks": tracks}));
        }
    }

    fn refresh_snapshot(&self, busy: Option<Busy>) {
        let reaper = self.reaper();
        let project = reaper.current_project();
        let status = self.status(project.as_ref());
        let mut snapshot = self.snapshot.lock().unwrap_or_else(|e| e.into_inner());
        if snapshot.daw_version.is_empty() {
            snapshot.daw_version = reaper.app_version();
            snapshot.resource = reaper.resource_path();
        }
        snapshot.project = project.map(|p| ProjectSummary { name: p.name, path: p.path });
        snapshot.busy = busy;
        snapshot.status = status;
        snapshot.focus = self.focus_value.borrow().clone();
        snapshot.focus_at = self.focus_at.get();
        snapshot.last_tick = Some(Instant::now());
    }

    /// Answers every queued job "closing" without running it: the extension is unloading.
    pub fn cancel_jobs(&self) {
        while let Ok(job) = self.jobs.try_recv() {
            if job.claim() {
                job.answer(Err(("failed".into(), "REAPER is closing; nothing changed".into())));
            }
        }
    }

    /// A copy of what the socket threads see.
    pub fn snapshot(&self) -> Snapshot {
        self.snapshot.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    fn count_tick(&self, driver: Driver) {
        let mut rates = self.rates.borrow_mut();
        let now = Instant::now();
        let start = *rates.window_start.get_or_insert(now);
        if now.duration_since(start) >= Duration::from_secs(1) {
            rates.timer_hz = rates.timer;
            rates.surface_hz = rates.surface;
            rates.timer = 0;
            rates.surface = 0;
            rates.window_start = Some(now);
        }
        match driver {
            Driver::Timer => rates.timer += 1,
            Driver::Surface => rates.surface += 1,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::queue::Outcome;
    use crate::reaper::fake;
    use std::rc::Rc;
    use std::sync::atomic::AtomicBool;

    static OPEN: AtomicBool = AtomicBool::new(false);

    #[derive(Clone, Default)]
    struct Recorder(Rc<RefCell<Vec<(String, Value)>>>);

    impl Outbox for Recorder {
        fn event(&self, name: &str, data: Value) {
            self.0.borrow_mut().push((name.into(), data));
        }
        fn heartbeat(&self) {
            self.0.borrow_mut().push(("heartbeat".into(), Value::Null));
        }
    }

    impl Recorder {
        fn take(&self) -> Vec<(String, Value)> {
            self.0.take().into_iter().filter(|(name, _)| name != "heartbeat").collect()
        }
    }

    fn setup() -> (Extension, std::sync::mpsc::Sender<Job>, Recorder, Arc<Mutex<Snapshot>>) {
        fake::reset();
        let (tx, rx) = queue::channel();
        let recorder = Recorder::default();
        let snapshot = Arc::new(Mutex::new(Snapshot::default()));
        let extension = Extension::new(fake::api(), rx, Box::new(recorder.clone()), snapshot.clone());
        (extension, tx, recorder, snapshot)
    }

    fn ask(extension: &Extension, tx: &std::sync::mpsc::Sender<Job>, method: &str) -> Outcome {
        let (job, waiter) = queue::job(method, json!({}));
        tx.send(job).unwrap();
        extension.tick(Driver::Timer);
        waiter.wait(Duration::from_secs(1), &OPEN)
    }

    #[test]
    fn each_tick_refreshes_what_the_socket_threads_serve() {
        let (extension, _tx, _, _) = setup();
        fake::with(|s| s.play_bits = 1);
        extension.tick(Driver::Timer);
        let snapshot = extension.snapshot();
        assert_eq!(snapshot.daw_version, "7.82/macOS-arm64");
        assert_eq!(snapshot.resource, "/lab/REAPER");
        assert_eq!(snapshot.project, Some(ProjectSummary::default()));
        assert_eq!(snapshot.busy, None);
        assert!(snapshot.last_tick.is_some());
        assert_eq!(snapshot.status["tracks"], 2);
        assert_eq!(snapshot.status["transport"]["playing"], true);
        assert_eq!(snapshot.status["api"], json!({"resolved": fake::api().resolved(), "total": 877}));
        fake::with(|s| s.loading = true);
        extension.tick(Driver::Timer);
        assert_eq!(extension.snapshot().busy, Some(Busy::Loading));
    }

    #[test]
    fn unknown_methods_are_refused_by_name() {
        let (extension, tx, _, _) = setup();
        let Outcome::Answered(Err((code, message))) = ask(&extension, &tx, "explode") else { panic!() };
        assert_eq!(code, "unknown-method");
        assert!(message.contains("explode"));
    }

    fn ask_with(extension: &Extension, tx: &std::sync::mpsc::Sender<Job>, method: &str, params: Value) -> Answer {
        let (job, waiter) = queue::job(method, params);
        tx.send(job).unwrap();
        extension.tick(Driver::Timer);
        match waiter.wait(Duration::from_secs(1), &OPEN) {
            Outcome::Answered(answer) => answer,
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn actions_run_by_id_or_name_but_never_a_dialog() {
        let (extension, tx, _, _) = setup();
        assert_eq!(
            ask_with(&extension, &tx, "action", json!({"command": 40001})),
            Ok(json!({"command": 40001, "name": "Track: Insert new track"}))
        );
        assert_eq!(
            ask_with(&extension, &tx, "action", json!({"command": "_RS_kumi"})),
            Ok(json!({"command": 54000, "name": "Script: kumi.lua"}))
        );
        let refused = |params: Value| ask_with(&extension, &tx, "action", params).unwrap_err();
        assert!(refused(json!({"command": 40022})).1.contains("dialog"));
        assert!(refused(json!({"command": 40004})).1.contains("quit"));
        assert!(refused(json!({"command": 99999})).1.contains("no action"));
        assert!(refused(json!({"command": "_nope"})).1.contains("no action"));
        assert_eq!(refused(json!({})).0, "bad-params");
        assert_eq!(fake::with(|s| s.actions.clone()), [40001, 54000]);
    }

    #[test]
    fn reads_and_snapshots_are_answered_on_the_main_thread() {
        let (extension, tx, _, _) = setup();
        fake::lab_project();
        let read = ask_with(&extension, &tx, "read", json!({"kind": "track", "limit": 2})).unwrap();
        assert_eq!(read["items"][1]["name"], "Drums");
        assert!(read["nextCursor"].is_string());
        let snapshot = ask_with(&extension, &tx, "snapshot", json!({})).unwrap();
        assert_eq!(snapshot["tracks"].as_array().unwrap().len(), 5);
        assert_eq!(snapshot["epoch"], read["epoch"]);
        assert_eq!(ask_with(&extension, &tx, "read", json!({})).unwrap_err().0, "bad-params");
        let body = ask_with(&extension, &tx, "focus_body", json!({"kind": "routing", "ref": "r:track:{AAAA}"})).unwrap();
        assert_eq!(
            (&body["kind"], &body["epoch"], &body["routes"][0]["destRef"]),
            (&json!("routing"), &read["epoch"], &json!("r:track:{DDDD}"))
        );
        assert_eq!(ask_with(&extension, &tx, "focus_body", json!({"kind": "mixer"})).unwrap_err().0, "bad-params");
    }

    fn focus_of(extension: &Extension) -> Value {
        extension.snapshot().focus
    }

    #[test]
    fn focus_is_polled_each_tick_and_kept_for_the_socket_threads() {
        let (extension, _tx, _, _) = setup();
        fake::lab_project();
        extension.tick(Driver::Timer);
        let focus = focus_of(&extension);
        assert_eq!(focus["view"], "arrange");
        assert_eq!(focus["changed"], 11);
        assert_eq!(focus["selection"]["trackCount"], 0);
        assert!(extension.snapshot().focus_at.is_some());
        // Within the poll interval a tick leaves it be; a state change shows once it is due.
        fake::with(|s| s.state_count = 12);
        extension.tick(Driver::Timer);
        assert_eq!(focus_of(&extension)["changed"], 11);
        std::thread::sleep(FOCUS_EVERY + Duration::from_millis(5));
        extension.tick(Driver::Timer);
        assert_eq!(focus_of(&extension)["changed"], 12);
    }

    #[test]
    fn the_surface_saying_the_selection_moved_refreshes_focus_at_once() {
        let (extension, _tx, _, _) = setup();
        fake::lab_project();
        extension.tick(Driver::Timer);
        assert_eq!(focus_of(&extension)["selection"]["trackRef"], Value::Null);
        fake::select_tracks(&[1]);
        extension.on_surface(Call::TrackSelection(fake::track_ptr(1)));
        extension.tick(Driver::Timer);
        assert_eq!(focus_of(&extension)["selection"]["trackRef"], "r:track:{BBBB}");
        // A focused FX window comes with the surface's word for it.
        fake::with(|s| s.focused_fx = Some((0, -1, -1, 0, 0)));
        extension.on_surface(Call::Extended {
            call: api::CSURF_EXT_SETFOCUSEDFX,
            parm1: std::ptr::null_mut(),
            parm2: std::ptr::null_mut(),
            parm3: std::ptr::null_mut(),
        });
        extension.tick(Driver::Timer);
        assert_eq!((&focus_of(&extension)["view"], &focus_of(&extension)["selection"]["fxRef"]), (&json!("fx"), &json!("r:fx:{FX-EQ}")));
    }

    #[test]
    fn a_tab_switch_changes_the_epoch_and_the_focus_that_carries_it() {
        let (extension, _tx, _, _) = setup();
        fake::lab_project();
        extension.tick(Driver::Timer);
        let before = focus_of(&extension)["epoch"].clone();
        fake::with(|s| s.project_id += 1);
        extension.tick(Driver::Timer);
        let after = focus_of(&extension)["epoch"].clone();
        assert_ne!(before, after);
        assert_eq!(after, extension.reads.epoch());
    }

    /// The epoch and the project's identity as a read gives them.
    fn epoch_and_identity(extension: &Extension, tx: &std::sync::mpsc::Sender<Job>) -> (String, String) {
        let read = ask_with(extension, tx, "read", json!({"kind": "project"})).unwrap();
        (read["epoch"].as_str().unwrap().to_string(), read["items"][0]["identity"].as_str().unwrap().to_string())
    }

    #[test]
    fn saving_a_project_under_another_name_keeps_the_epoch_and_the_identity() {
        let (extension, tx, recorder, _) = setup();
        fake::lab_project();
        extension.tick(Driver::Timer);
        let before = epoch_and_identity(&extension, &tx);
        recorder.take();
        fake::with(|s| s.project = Some(("/lab/Projects/Copy.rpp".into(), "Copy.rpp".into())));
        extension.tick(Driver::Timer);
        // The project event goes out (its name changed), but every ref still means what it did.
        assert_eq!(recorder.take().first().map(|(name, _)| name.as_str()), Some("project"));
        assert_eq!(epoch_and_identity(&extension, &tx), before);
    }

    #[test]
    fn a_new_project_opened_in_the_same_tab_is_a_new_epoch_and_a_new_identity() {
        let (extension, tx, recorder, _) = setup();
        fake::lab_project();
        extension.tick(Driver::Timer);
        let before = epoch_and_identity(&extension, &tx);
        recorder.take();
        // The tab keeps its project object; the file, the tracks and the state counter are another project's.
        fake::with(|s| {
            s.project = Some(("/lab/Projects/Other.rpp".into(), "Other.rpp".into()));
            s.tracks = vec![fake::FakeTrack::new("{OTHER-1}", "Pad")];
            s.state_count = 1;
        });
        extension.tick(Driver::Timer);
        let after = epoch_and_identity(&extension, &tx);
        assert_ne!(after.0, before.0);
        assert!(after.1.starts_with(&before.1) && after.1 != before.1, "{before:?} then {after:?}");
        assert_eq!(recorder.take().iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>(), ["project", "tracks", "selection"]);
        // A ref from the old project is not read against the new one.
        assert_eq!(ask_with(&extension, &tx, "read", json!({"ref": "r:track:{AAAA}"})).unwrap_err().0, "failed");
        assert_eq!(focus_of(&extension)["epoch"], after.0);
    }

    #[test]
    fn a_new_unsaved_project_in_an_unsaved_tab_is_noticed_by_its_tracks() {
        let (extension, tx, _, _) = setup();
        extension.tick(Driver::Timer);
        let before = epoch_and_identity(&extension, &tx);
        // Nothing about the file changed (there is none); the counter moved on, and none of the old tracks is left.
        fake::with(|s| {
            s.tracks = vec![fake::FakeTrack::new("{NEW-1}", "Keys")];
            s.state_count = 5;
        });
        extension.watch.borrow_mut().tracks_at = None;
        extension.tick(Driver::Timer);
        let after = epoch_and_identity(&extension, &tx);
        assert_ne!(after.0, before.0);
        assert_ne!(after.1, before.1);
    }

    #[test]
    fn editing_the_project_is_not_a_new_project() {
        let (extension, tx, _, _) = setup();
        fake::lab_project();
        extension.tick(Driver::Timer);
        let before = epoch_and_identity(&extension, &tx);
        // A track added, one removed, an undo: the counter moves, the tracks overlap.
        for step in 0..3 {
            fake::with(|s| {
                match step {
                    0 => s.tracks.push(fake::FakeTrack::new("{EXTRA}", "Extra")),
                    1 => {
                        s.tracks.remove(0);
                    }
                    _ => {
                        s.tracks.pop();
                    }
                }
                s.state_count += 1;
            });
            extension.watch.borrow_mut().tracks_at = None;
            extension.tick(Driver::Timer);
            assert_eq!(epoch_and_identity(&extension, &tx), before, "step {step}");
        }
        // The first track of an empty project is no new project either.
        fake::with(|s| {
            s.tracks.clear();
            s.state_count += 1;
        });
        extension.watch.borrow_mut().tracks_at = None;
        extension.tick(Driver::Timer);
        fake::with(|s| {
            s.tracks.push(fake::FakeTrack::new("{FIRST}", "First"));
            s.state_count += 1;
        });
        extension.watch.borrow_mut().tracks_at = None;
        extension.tick(Driver::Timer);
        assert_eq!(epoch_and_identity(&extension, &tx), before);
    }

    #[test]
    fn a_fresh_empty_project_in_the_tab_is_noticed_when_the_counter_goes_back() {
        let (extension, tx, _, _) = setup();
        fake::lab_project();
        extension.tick(Driver::Timer);
        let before = epoch_and_identity(&extension, &tx);
        fake::with(|s| {
            s.tracks.clear();
            s.state_count = 0;
        });
        extension.tick(Driver::Timer);
        assert_ne!(epoch_and_identity(&extension, &tx).0, before.0);
    }

    #[test]
    fn the_classifier_weighs_tracks_then_file_then_counter() {
        let set = |ids: &[&str]| ids.iter().map(|id| id.to_string()).collect::<HashSet<_>>();
        let (a, ab, c, none) = (set(&["a"]), set(&["a", "b"]), set(&["c"]), set(&[]));
        assert!(!is_another_project(&a, &ab, false, false));
        assert!(!is_another_project(&ab, &a, true, true), "a file saved under another name keeps its tracks");
        assert!(is_another_project(&a, &c, false, false), "no track in common is another project, whatever the file");
        assert!(!is_another_project(&none, &a, false, false), "the first track of an empty project");
        assert!(is_another_project(&a, &none, false, true), "tracks gone and the counter back");
        assert!(!is_another_project(&a, &none, false, false), "tracks gone by editing");
        assert!(is_another_project(&none, &none, true, false), "another file, nothing to compare");
        assert!(!is_another_project(&none, &none, false, true));
    }

    #[test]
    fn nothing_is_concluded_while_reaper_loads_a_project() {
        let (extension, tx, _, _) = setup();
        fake::lab_project();
        extension.tick(Driver::Timer);
        let before = epoch_and_identity(&extension, &tx);
        // A load into the tab: the file and the tracks change while REAPER is busy, and nothing is judged then.
        fake::with(|s| {
            s.loading = true;
            s.project = Some(("/lab/Projects/Other.rpp".into(), "Other.rpp".into()));
            s.tracks = vec![fake::FakeTrack::new("{OTHER-1}", "Pad")];
            s.state_count = 1;
        });
        extension.tick(Driver::Timer);
        extension.tick(Driver::Timer);
        assert_eq!(extension.reads.epoch(), before.0);
        // When REAPER is done, it is the new project that is looked at, once.
        fake::with(|s| s.loading = false);
        extension.tick(Driver::Timer);
        assert_ne!(epoch_and_identity(&extension, &tx).0, before.0);
        let settled = epoch_and_identity(&extension, &tx);
        extension.tick(Driver::Timer);
        assert_eq!(epoch_and_identity(&extension, &tx), settled);
    }

    #[test]
    fn nothing_is_polled_while_reaper_loads_a_project() {
        let (extension, _tx, _, _) = setup();
        fake::lab_project();
        fake::with(|s| s.loading = true);
        extension.tick(Driver::Timer);
        assert_eq!(focus_of(&extension), Value::Null);
        fake::with(|s| s.loading = false);
        extension.tick(Driver::Timer);
        assert_eq!(focus_of(&extension)["view"], "arrange");
    }

    #[test]
    fn quit_refuses_unsaved_changes_unless_asked_to_save() {
        let (extension, tx, _, _) = setup();
        fake::with(|s| s.dirty = true);
        assert!(ask_with(&extension, &tx, "quit", json!({})).unwrap_err().1.contains("never saved"));
        fake::with(|s| s.project = Some(("/lab/Projects/a.rpp".into(), "a.rpp".into())));
        assert!(ask_with(&extension, &tx, "quit", json!({})).unwrap_err().1.contains("a.rpp"));
        assert!(fake::with(|s| s.actions.is_empty()));
        assert_eq!(
            ask_with(&extension, &tx, "quit", json!({"save": true})),
            Ok(json!({"quitting": true, "saved": ["/lab/Projects/a.rpp"]}))
        );
        assert_eq!(fake::with(|s| s.saved), 1);
    }

    #[test]
    fn actions_that_would_ask_to_save_or_hold_the_main_thread_are_refused() {
        let (extension, tx, _, _) = setup();
        let refused = |command: i32| ask_with(&extension, &tx, "action", json!({"command": command})).unwrap_err();
        // A render holds the main thread for as long as it takes.
        assert!(refused(41824).1.contains("renders"));
        // Closing or replacing a project with changes asks "Save changes?" inside the request.
        fake::with(|s| {
            s.project = Some(("/lab/Projects/a.rpp".into(), "a.rpp".into()));
            s.dirty = true;
        });
        for command in [40860, 40023, 40886] {
            let (code, message) = refused(command);
            assert_eq!(code, "failed");
            assert!(message.contains("a.rpp"), "{message}");
        }
        assert!(fake::with(|s| s.actions.is_empty()));
        // Without changes they run; and Save on a project never saved would open Save As.
        fake::with(|s| s.dirty = false);
        assert!(ask_with(&extension, &tx, "action", json!({"command": 40860})).is_ok());
        fake::with(|s| s.project = Some((String::new(), String::new())));
        assert!(refused(SAVE_PROJECT).1.contains("Save As"));
        fake::with(|s| s.project = Some(("/lab/Projects/a.rpp".into(), "a.rpp".into())));
        assert!(ask_with(&extension, &tx, "action", json!({"command": SAVE_PROJECT})).is_ok());
        assert_eq!(fake::with(|s| s.actions.clone()), [40860, SAVE_PROJECT]);
    }

    #[test]
    fn quit_waits_for_a_recording_to_stop() {
        let (extension, tx, _, _) = setup();
        fake::with(|s| s.play_bits = 4);
        assert!(ask_with(&extension, &tx, "quit", json!({})).unwrap_err().1.contains("recording"));
        fake::with(|s| s.play_bits = 0);
        assert!(ask_with(&extension, &tx, "quit", json!({})).is_ok());
    }

    #[test]
    fn only_the_timer_quits_never_the_control_surfaces_run() {
        let (extension, tx, _, _) = setup();
        ask(&extension, &tx, "quit");
        extension.on_surface(Call::Run);
        extension.on_surface(Call::Run);
        assert!(fake::with(|s| s.actions.is_empty()), "REAPER quit inside the surface's own Run()");
        extension.tick(Driver::Timer);
        assert_eq!(fake::with(|s| s.actions.clone()), [ACTION_QUIT]);
    }

    #[test]
    fn rendering_shows_in_the_snapshot_and_holds_requests() {
        let (extension, tx, _, _) = setup();
        fake::with(|s| s.rendering = true);
        extension.tick(Driver::Timer);
        assert_eq!(extension.snapshot().busy, Some(Busy::Rendering));
        let (job, waiter) = queue::job("action", json!({"command": 40001}));
        tx.send(job).unwrap();
        extension.tick(Driver::Timer);
        assert_eq!(waiter.wait(Duration::from_millis(1), &OPEN), Outcome::Cancelled);
        assert!(fake::with(|s| s.actions.is_empty()));
        fake::with(|s| s.rendering = false);
        extension.tick(Driver::Timer);
        assert_eq!(extension.snapshot().busy, None);
    }

    // An outbox whose heartbeat panics once: the kind of fault that gets past `guard` around one request.
    struct Panicky(Cell<bool>);

    impl Outbox for Panicky {
        fn event(&self, _: &str, _: Value) {}
        fn heartbeat(&self) {
            if self.0.replace(false) {
                panic!("a heartbeat failed");
            }
        }
    }

    #[test]
    fn a_panic_in_a_tick_doesnt_skip_every_tick_after_it() {
        fake::reset();
        let (tx, rx) = queue::channel();
        let extension = Extension::new(fake::api(), rx, Box::new(Panicky(Cell::new(true))), Arc::new(Mutex::new(Snapshot::default())));
        let first = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| extension.tick(Driver::Timer)));
        assert!(first.is_err());
        let (job, waiter) = queue::job("action", json!({"command": 40001}));
        tx.send(job).unwrap();
        extension.tick(Driver::Timer);
        assert!(matches!(waiter.wait(Duration::from_secs(1), &OPEN), Outcome::Answered(Ok(_))));
        // And the UI refresh hold is balanced when something unwinds while it is held.
        assert_eq!(fake::with(|s| s.ui_refresh), 0);
    }

    #[test]
    fn quit_looks_again_after_saving_and_stays_if_the_project_is_still_changed() {
        let (extension, tx, _, _) = setup();
        fake::with(|s| {
            s.project = Some(("/lab/Projects/a.rpp".into(), "a.rpp".into()));
            s.dirty = true;
            s.save_fails = true;
        });
        let refused = ask_with(&extension, &tx, "quit", json!({"save": true})).unwrap_err();
        assert!(refused.1.contains("still has unsaved changes") && refused.1.contains("a.rpp"), "{refused:?}");
        extension.tick(Driver::Timer);
        assert!(fake::with(|s| s.actions.is_empty()), "REAPER was told to quit with a project changed");
        fake::with(|s| s.save_fails = false);
        assert!(ask_with(&extension, &tx, "quit", json!({"save": true})).is_ok());
    }

    #[test]
    fn quit_answers_first_and_runs_reapers_quit_on_the_next_tick() {
        let (extension, tx, _, _) = setup();
        assert_eq!(ask(&extension, &tx, "quit"), Outcome::Answered(Ok(json!({"quitting": true, "saved": []}))));
        assert!(fake::with(|s| s.actions.is_empty()));
        extension.tick(Driver::Timer);
        assert_eq!(fake::with(|s| s.actions.clone()), [ACTION_QUIT]);
        extension.tick(Driver::Timer);
        assert_eq!(fake::with(|s| s.actions.len()), 1);
    }

    #[test]
    fn requests_wait_while_reaper_loads_a_project() {
        let (extension, tx, _, _) = setup();
        fake::with(|s| s.loading = true);
        let (job, waiter) = queue::job("quit", json!({}));
        tx.send(job).unwrap();
        extension.tick(Driver::Timer);
        assert_eq!(waiter.wait(Duration::from_millis(1), &OPEN), Outcome::Cancelled);
    }

    #[test]
    fn unloading_answers_queued_jobs_without_running_them() {
        let (extension, tx, _, _) = setup();
        let (job, waiter) = queue::job("action", json!({"command": 40001}));
        tx.send(job).unwrap();
        extension.cancel_jobs();
        let Outcome::Answered(Err((code, message))) = waiter.wait(Duration::from_secs(1), &OPEN) else { panic!() };
        assert_eq!(code, "failed");
        assert!(message.contains("closing"));
        assert!(fake::with(|s| s.actions.is_empty()));
    }

    #[test]
    fn requests_run_between_one_ui_refresh_hold() {
        let (extension, tx, _, _) = setup();
        ask(&extension, &tx, "explode");
        assert_eq!(fake::with(|s| s.ui_refresh), 0);
    }

    #[test]
    fn surface_calls_become_coalesced_events_on_the_next_tick() {
        let (extension, _tx, recorder, _) = setup();
        extension.tick(Driver::Timer);
        recorder.take();
        let track = 0x1008 as *mut api::MediaTrack;
        extension.on_surface(Call::TrackListChange);
        extension.on_surface(Call::TrackListChange);
        fake::select_tracks(&[1]);
        extension.on_surface(Call::Selected(track, true));
        extension.on_surface(Call::TrackSelection(track));
        extension.on_surface(Call::PlayState { play: true, pause: false, rec: false });
        assert!(recorder.take().is_empty());
        extension.on_surface(Call::Run);
        assert_eq!(
            recorder.take(),
            [
                ("transport".to_string(), json!({"playing": true, "paused": false, "recording": false, "bpm": 120.0})),
                ("tracks".to_string(), json!({"count": 2})),
                ("selection".to_string(), json!({"tracks": [{"index": 2, "guid": "{BBBB}", "name": "Bass"}]})),
            ]
        );
        // The same play state again is not news; a tempo change is.
        extension.on_surface(Call::PlayState { play: true, pause: false, rec: false });
        extension.tick(Driver::Timer);
        assert!(recorder.take().is_empty());
        fake::with(|s| {
            s.bpm = 96.0;
            s.play_bits = 1;
        });
        let tempo = Call::Extended {
            call: api::CSURF_EXT_SETBPMANDPLAYRATE,
            parm1: std::ptr::null_mut(),
            parm2: std::ptr::null_mut(),
            parm3: std::ptr::null_mut(),
        };
        extension.on_surface(tempo);
        extension.tick(Driver::Timer);
        assert_eq!(
            recorder.take(),
            [("transport".to_string(), json!({"playing": true, "paused": false, "recording": false, "bpm": 96.0}))]
        );
    }

    #[test]
    fn a_project_switch_sends_the_project_then_its_tracks_and_selection() {
        let (extension, _tx, recorder, _) = setup();
        extension.tick(Driver::Timer);
        recorder.take();
        fake::with(|s| s.project = Some(("/lab/Projects/b.rpp".into(), "b.rpp".into())));
        extension.tick(Driver::Timer);
        let events = recorder.take();
        assert_eq!(events[0], ("project".to_string(), json!({"name": "b.rpp", "path": "/lab/Projects/b.rpp"})));
        assert_eq!(events.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>(), ["project", "tracks", "selection"]);
    }

    #[test]
    fn heartbeats_come_from_the_main_thread_once_a_second() {
        let (extension, _tx, recorder, _) = setup();
        extension.tick(Driver::Timer);
        extension.tick(Driver::Surface);
        let beats = recorder.0.borrow().iter().filter(|(n, _)| n == "heartbeat").count();
        assert_eq!(beats, 1);
    }
}
