//! The loaded extension on REAPER's main thread: each tick (REAPER's timer and the control surface's Run()) runs
//! queued requests within a budget, then turns what the control surface reported into events.

use std::cell::{Cell, RefCell};
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::api::{self, Api};
use crate::csurf::Call;
use crate::queue::{self, Answer, Budget, Job};
use crate::reaper::{PlayState, ProjectInfo, Reaper, ACTION_QUIT};

pub const BRIDGE_VERSION: &str = env!("CARGO_PKG_VERSION");
const HEARTBEAT: Duration = Duration::from_millis(1000);

/// Where the main thread's events and heartbeats go: the socket server, or a test's list.
pub trait Outbox {
    fn event(&self, name: &str, data: Value);
    fn heartbeat(&self);
}

/// What the socket threads may read without the main thread: who this bridge is, and when it last ticked.
#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    pub daw_version: String,
    pub resource: String,
    pub project: Option<ProjectSummary>,
    pub last_tick: Option<Instant>,
    /// Loading, saving or rendering, as of the last tick.
    pub busy: Option<String>,
    /// `status`'s fields read on the main thread: transport, track count, tick rates.
    pub status: Value,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct ProjectSummary {
    pub name: String,
    pub path: String,
}

impl ProjectSummary {
    pub fn to_json(&self) -> Value {
        let mut project = serde_json::Map::new();
        if !self.name.is_empty() {
            project.insert("name".into(), self.name.clone().into());
        }
        if !self.path.is_empty() {
            project.insert("path".into(), self.path.clone().into());
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
}

fn bad_params(message: &str) -> (String, String) {
    ("bad-params".into(), message.into())
}

// What was last sent, so a tick sends only changes.
#[derive(Default)]
struct Sent {
    project: Option<(usize, ProjectSummary)>,
    play: Option<PlayState>,
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
    jobs: Receiver<Job>,
    out: Box<dyn Outbox>,
    snapshot: Arc<Mutex<Snapshot>>,
    budget: Budget,
    pending: RefCell<Pending>,
    sent: RefCell<Sent>,
    rates: RefCell<Rates>,
    in_tick: Cell<bool>,
    quit: Cell<Quit>,
    last_heartbeat: Cell<Option<Instant>>,
}

impl Extension {
    pub fn new(api: Api, jobs: Receiver<Job>, out: Box<dyn Outbox>, snapshot: Arc<Mutex<Snapshot>>) -> Extension {
        let extension = Extension {
            api,
            jobs,
            out,
            snapshot,
            budget: Budget::default(),
            pending: RefCell::default(),
            sent: RefCell::default(),
            rates: RefCell::default(),
            in_tick: Cell::new(false),
            quit: Cell::new(Quit::No),
            last_heartbeat: Cell::new(None),
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
            Call::TrackListChange => pending.tracks = true,
            Call::Selected(..) | Call::TrackSelection(_) => pending.selection = true,
            Call::PlayState { play, pause, rec } => pending.play = Some(PlayState { playing: play, paused: pause, recording: rec }),
            _ => {}
        }
        0
    }

    pub fn tick(&self, driver: Driver) {
        // A request's REAPER call can call back into the surface (Run included): that tick is skipped.
        if self.in_tick.replace(true) {
            return;
        }
        self.count_tick(driver);
        if self.quit.get() == Quit::Answered {
            // REAPER may unload the extension inside this call; nothing more this tick.
            self.quit.set(Quit::No);
            self.reaper().run_action(ACTION_QUIT);
            self.in_tick.set(false);
            return;
        }
        let busy = self.reaper().busy();
        if busy.is_none() {
            self.run_jobs();
        }
        self.poll_project();
        self.flush_events();
        self.refresh_snapshot(busy);
        let now = Instant::now();
        if self.last_heartbeat.get().is_none_or(|last| now.duration_since(last) >= HEARTBEAT) {
            self.last_heartbeat.set(Some(now));
            self.out.heartbeat();
        }
        self.in_tick.set(false);
    }

    fn run_jobs(&self) {
        let reaper = self.reaper();
        let mut refresh_held = false;
        queue::drain(&self.jobs, self.budget, |job| {
            if !refresh_held {
                reaper.prevent_ui_refresh(1);
                refresh_held = true;
            }
            let answer = crate::guard("request", Err(("internal".into(), "the request failed inside the bridge".into())), || {
                self.handle(&job.method, &job.params)
            });
            job.answer(answer);
        });
        if refresh_held {
            reaper.prevent_ui_refresh(-1);
        }
    }

    fn handle(&self, method: &str, params: &Value) -> Answer {
        match method {
            "action" => self.action(params),
            "quit" => self.quit(params),
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
        reaper.run_action(id);
        Ok(json!({"command": id, "name": name}))
    }

    // Quits on the next tick. REAPER would ask about unsaved changes in a dialog, so those are saved first when asked
    // (`save: true`), and refused otherwise; a project never saved can't be saved without Save As.
    fn quit(&self, params: &Value) -> Answer {
        let reaper = self.reaper();
        let save = params["save"].as_bool().unwrap_or(false);
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
            status["transport"] = reaper.play_state(project.ptr).to_json();
            status["tracks"] = reaper.track_count(project.ptr).into();
            if let Some(count) = reaper.state_change_count(project.ptr) {
                status["stateChangeCount"] = count.into();
            }
        }
        status
    }

    // A tab switch, a project opened or closed, or saved under another name.
    fn poll_project(&self) {
        let current = self.reaper().current_project().map(|p| (p.ptr.addr(), ProjectSummary { name: p.name, path: p.path }));
        let mut sent = self.sent.borrow_mut();
        if sent.project == current {
            return;
        }
        let first = sent.project.is_none();
        sent.project = current.clone();
        drop(sent);
        if !first {
            self.out.event("project", current.map(|(_, p)| p.to_json()).unwrap_or(Value::Null));
            let mut pending = self.pending.borrow_mut();
            pending.tracks = true;
            pending.selection = true;
        }
    }

    fn flush_events(&self) {
        let pending = std::mem::take(&mut *self.pending.borrow_mut());
        if !pending.tracks && !pending.selection && pending.play.is_none() {
            return;
        }
        let reaper = self.reaper();
        let Some(project) = reaper.current_project() else { return };
        if let Some(play) = pending.play {
            let mut sent = self.sent.borrow_mut();
            if sent.play != Some(play) {
                sent.play = Some(play);
                drop(sent);
                self.out.event("transport", play.to_json());
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

    fn refresh_snapshot(&self, busy: Option<&'static str>) {
        let reaper = self.reaper();
        let project = reaper.current_project();
        let status = self.status(project.as_ref());
        let mut snapshot = self.snapshot.lock().unwrap_or_else(|e| e.into_inner());
        if snapshot.daw_version.is_empty() {
            snapshot.daw_version = reaper.app_version();
            snapshot.resource = reaper.resource_path();
        }
        snapshot.project = project.map(|p| ProjectSummary { name: p.name, path: p.path });
        snapshot.busy = busy.map(String::from);
        snapshot.status = status;
        snapshot.last_tick = Some(Instant::now());
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
        waiter.wait(Duration::from_secs(1))
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
        assert_eq!(snapshot.status["api"], json!({"resolved": 20, "total": 877}));
        fake::with(|s| s.loading = true);
        extension.tick(Driver::Timer);
        assert!(extension.snapshot().busy.unwrap().contains("loading"));
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
        match waiter.wait(Duration::from_secs(1)) {
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
        assert_eq!(waiter.wait(Duration::from_millis(1)), Outcome::Cancelled);
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
        fake::with(|s| s.selected = vec![1]);
        extension.on_surface(Call::Selected(track, true));
        extension.on_surface(Call::TrackSelection(track));
        extension.on_surface(Call::PlayState { play: true, pause: false, rec: false });
        assert!(recorder.take().is_empty());
        extension.on_surface(Call::Run);
        assert_eq!(
            recorder.take(),
            [
                ("transport".to_string(), json!({"playing": true, "paused": false, "recording": false})),
                ("tracks".to_string(), json!({"count": 2})),
                ("selection".to_string(), json!({"tracks": [{"index": 2, "guid": "{BBBB}", "name": "Bass"}]})),
            ]
        );
        // The same play state again is not news.
        extension.on_surface(Call::PlayState { play: true, pause: false, rec: false });
        extension.tick(Driver::Timer);
        assert!(recorder.take().is_empty());
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
