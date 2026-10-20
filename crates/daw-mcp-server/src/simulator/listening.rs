//! The bridges' listening side, simulated (M5 contract §3), for the host's tests and Kumi's: render and capture jobs that
//! write real WAV files (noise at each source's level) into a folder, previews played out loud, the audio behind a ref,
//! and scratch copies. Shared by the REAPER and Renoise scripts. A job moves on each time its status is asked, so a test
//! sees it running, then done.
use daw_bridge::Daw;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// What the sim renders at: 32-bit float stereo.
const RATE: u64 = 48_000;
const DEFAULT_LEVEL: f64 = 0.3;

type Answer = Result<Value, (String, String)>;

fn bad(message: impl Into<String>) -> (String, String) {
    ("bad-params".into(), message.into())
}

/// One job the sim runs: what it will write, and how far it has come.
struct Job {
    kind: &'static str,
    state: &'static str,
    polls: u32,
    /// Each file: its source id, path, where the part starts in it (s), its length (s), its level and its stage.
    files: Vec<(String, PathBuf, f64, f64, f64, &'static str)>,
    /// A capture's marks.
    marks: Option<Value>,
    error: Option<(String, String)>,
    notes: Vec<String>,
}

struct State {
    daw: Daw,
    folder: PathBuf,
    tempo: f64,
    jobs: HashMap<String, Job>,
    /// Polls a job takes to finish (0: done at its first status).
    steps: u32,
    /// Each track's level (its noise's peak), by ref; the master is their sum.
    levels: HashMap<String, f64>,
    /// The master's level when a test sets it (the mix as a change left it).
    master_level: Option<f64>,
    /// What the master's own gain FX add, in dB (a bridge script keeps it).
    master_gain_db: f64,
    /// A cancel can't stop a render at once (Renoise's render in flight runs to its end): it says it's stopping, and the
    /// job ends cancelled at its next status.
    slow_stop: bool,
    /// Scratch copies by set.
    scratch: HashMap<String, Vec<ScratchCopy>>,
    previews: Vec<Value>,
    playing: Option<String>,
    /// What each render asked, in order.
    renders: Vec<Value>,
    /// Times a job put back what it changed (done, failed or cancelled).
    restored: u32,
    /// The next job fails with this, at its first status.
    fail_next: Option<(String, String)>,
    demo: bool,
    playing_now: bool,
    audition_works: bool,
    next_copy: u32,
    /// Copies kept as the producer's tracks (`scratch-keep`): their name, by ref.
    kept: HashMap<String, String>,
    /// Refs whose audio is MIDI (REAPER) or that have no sample data (Renoise).
    silent_refs: Vec<String>,
    /// What the next job notes, whatever its end.
    next_notes: Vec<String>,
    /// The marks the next capture answers (where the playhead was at its first frame and at each jump).
    capture_marks: Option<Value>,
    /// What part of its source an item plays (REAPER): offset and length in the file, its play rate, reversed.
    item_part: Option<(f64, f64, f64, bool)>,
}

/// A scratch copy: its ref, name, the source's ref and the values set on it.
type ScratchCopy = (String, String, String, Vec<Value>);

/// The listening side of a simulated bridge. Clones share it.
#[derive(Clone)]
pub struct SimListening {
    state: Arc<Mutex<State>>,
}

/// A WAV of noise at `level` (peak) for `seconds`: 32-bit float, stereo, 48 kHz.
pub fn write_noise(path: &Path, seconds: f64, level: f64, seed: u32) -> std::io::Result<()> {
    let frames = (seconds.max(0.) * RATE as f64) as usize;
    let mut data = Vec::with_capacity(frames * 8);
    let mut state = seed | 1;
    for _ in 0..frames {
        state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        let sample = (((state >> 8) as f64 / (1u32 << 24) as f64) * 2. - 1.) * level;
        data.extend_from_slice(&(sample as f32).to_le_bytes());
        data.extend_from_slice(&(sample as f32).to_le_bytes());
    }
    let mut file = Vec::with_capacity(44 + data.len());
    file.extend_from_slice(b"RIFF");
    file.extend_from_slice(&(36 + data.len() as u32).to_le_bytes());
    file.extend_from_slice(b"WAVEfmt ");
    file.extend_from_slice(&16u32.to_le_bytes());
    file.extend_from_slice(&3u16.to_le_bytes());
    file.extend_from_slice(&2u16.to_le_bytes());
    file.extend_from_slice(&(RATE as u32).to_le_bytes());
    file.extend_from_slice(&(RATE as u32 * 8).to_le_bytes());
    file.extend_from_slice(&8u16.to_le_bytes());
    file.extend_from_slice(&32u16.to_le_bytes());
    file.extend_from_slice(b"data");
    file.extend_from_slice(&(data.len() as u32).to_le_bytes());
    file.extend_from_slice(&data);
    if let Some(folder) = path.parent() {
        std::fs::create_dir_all(folder)?;
    }
    std::fs::write(path, file)
}

impl SimListening {
    pub fn new(daw: Daw) -> Self {
        static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let folder = std::env::temp_dir().join(format!("kumi-sim-listening-{}-{n}", std::process::id()));
        SimListening {
            state: Arc::new(Mutex::new(State {
                daw,
                folder,
                tempo: 120.,
                jobs: HashMap::new(),
                steps: 1,
                levels: HashMap::new(),
                master_level: None,
                master_gain_db: 0.,
                slow_stop: false,
                scratch: HashMap::new(),
                previews: vec![],
                playing: None,
                renders: vec![],
                restored: 0,
                fail_next: None,
                demo: false,
                playing_now: false,
                audition_works: true,
                next_copy: 0,
                kept: HashMap::new(),
                silent_refs: vec![],
                next_notes: vec![],
                capture_marks: None,
                item_part: None,
            })),
        }
    }

    /// What the next job notes (its answer's `notes`), whatever its end.
    pub fn note_next(&self, notes: &[&str]) {
        self.state.lock().unwrap().next_notes = notes.iter().map(|note| note.to_string()).collect();
    }

    /// The marks the next capture answers: `[frame, seconds, beats, playing]` at its first frame and at each jump.
    pub fn set_capture_marks(&self, marks: Value) {
        self.state.lock().unwrap().capture_marks = Some(marks);
    }

    /// What part of its 4-second source an item plays from now on (REAPER): offset and length in the file, its play
    /// rate, and whether reversed.
    pub fn set_item_part(&self, offset: f64, length: f64, rate: f64, reversed: bool) {
        self.state.lock().unwrap().item_part = Some((offset, length, rate, reversed));
    }

    /// Renders land here (a test points it at `<home>/bridges/<daw>/listening`, as the bridge's folder is).
    pub fn set_folder(&self, folder: &Path) {
        self.state.lock().unwrap().folder = folder.to_path_buf();
    }

    pub fn folder(&self) -> PathBuf {
        self.state.lock().unwrap().folder.clone()
    }

    pub fn set_tempo(&self, tempo: f64) {
        self.state.lock().unwrap().tempo = tempo;
    }

    /// A track's level (0: silent).
    pub fn set_level(&self, reference: &str, level: f64) {
        self.state.lock().unwrap().levels.insert(reference.into(), level);
    }

    /// The master's level from now on (the mix as a change left it).
    pub fn set_master_level(&self, level: f64) {
        self.state.lock().unwrap().master_level = Some(level);
    }

    /// A cancel can't stop a render at once from now on: it ends at the job's next status.
    pub fn set_slow_stop(&self, slow: bool) {
        self.state.lock().unwrap().slow_stop = slow;
    }

    /// What the master's gain FX add from now on, in dB.
    pub fn set_master_gain_db(&self, db: f64) {
        self.state.lock().unwrap().master_gain_db = db;
    }

    /// How many polls of its status a job takes to finish.
    pub fn set_steps(&self, steps: u32) {
        self.state.lock().unwrap().steps = steps;
    }

    /// The next job fails at its first status, with this code and message.
    pub fn fail_next(&self, code: &str, message: &str) {
        self.state.lock().unwrap().fail_next = Some((code.into(), message.into()));
    }

    /// The Renoise demo: it can't render.
    pub fn set_demo(&self, demo: bool) {
        self.state.lock().unwrap().demo = demo;
    }

    pub fn demo(&self) -> bool {
        self.state.lock().unwrap().demo
    }

    /// Whether the DAW is playing (a capture needs it to).
    pub fn set_playing(&self, playing: bool) {
        self.state.lock().unwrap().playing_now = playing;
    }

    /// Whether the DAW can play a file out loud.
    pub fn set_audition(&self, works: bool) {
        self.state.lock().unwrap().audition_works = works;
    }

    /// A ref with no audio of its own (a MIDI item, an empty sample).
    pub fn set_silent_ref(&self, reference: &str) {
        self.state.lock().unwrap().silent_refs.push(reference.into());
    }

    /// The previews played out loud, in order (each `audition`'s params).
    pub fn previews(&self) -> Vec<Value> {
        self.state.lock().unwrap().previews.clone()
    }

    /// What each render asked, in order.
    pub fn renders(&self) -> Vec<Value> {
        self.state.lock().unwrap().renders.clone()
    }

    /// Times a job put back what it changed.
    pub fn restored(&self) -> u32 {
        self.state.lock().unwrap().restored
    }

    /// The scratch copies standing now, by set.
    pub fn scratch_copies(&self) -> usize {
        self.state.lock().unwrap().scratch.values().map(Vec::len).sum()
    }

    /// The values set on each copy (by its ref), in order.
    pub fn copy_values(&self, copy: &str) -> Vec<Value> {
        let state = self.state.lock().unwrap();
        state.scratch.values().flatten().find(|(reference, ..)| reference == copy).map(|(.., values)| values.clone()).unwrap_or_default()
    }

    /// The `listening` flags `status` carries.
    pub fn flags(&self) -> Value {
        let state = self.state.lock().unwrap();
        json!({
            "render": !state.demo,
            "capture": state.daw == Daw::Reaper,
            "audition": state.audition_works,
            "grid": true,
            "scratch": true,
            "version": 1,
        })
    }

    /// The copies kept as tracks (`scratch-keep`), by ref, with their names.
    pub fn kept(&self) -> Vec<(String, String)> {
        let state = self.state.lock().unwrap();
        let mut kept: Vec<(String, String)> = state.kept.iter().map(|(reference, name)| (reference.clone(), name.clone())).collect();
        kept.sort();
        kept
    }

    /// `scratch-keep {ref, name}`, as both bridges make it: a copy kept as a track of the producer's, out of its set (no
    /// drop takes it then), renamed; its restore deletes it while it is as Kumi left it.
    pub fn keep(&self, method: &str, params: &Value, epoch: &str) -> Answer {
        let mut state = self.state.lock().unwrap();
        let renoise = state.daw == Daw::Renoise;
        let desc = params["desc"].as_str().unwrap_or_default().to_string();
        if method == "change_restore" {
            let reference = params["created"]["ref"].as_str().unwrap_or_default().to_string();
            let Some(name) = state.kept.get(&reference) else { return Err(("gone".into(), "the kept track isn't there any more".into())) };
            if params["applied"]["name"].as_str() != Some(name.as_str()) && params["discard"] != true {
                return Err(("changed".into(), format!("“{name}” changed since Kumi kept it")));
            }
            state.kept.remove(&reference);
            return Ok(json!({"epoch": epoch, "restored": {"deletedRef": reference}, "display": {"title": "Kept track deleted"}}));
        }
        let reference = params["input"]["ref"].as_str().unwrap_or_default().to_string();
        let name = params["input"]["name"].as_str().unwrap_or_default().to_string();
        if name.is_empty() || name.starts_with("Kumi · try ") {
            return Err(bad("name: the kept track's own name, not a scratch copy's"));
        }
        let Some((set, at)) = state
            .scratch
            .iter()
            .find_map(|(set, copies)| copies.iter().position(|(copy, ..)| *copy == reference).map(|at| (set.clone(), at)))
        else {
            return Err(("gone".into(), format!("{reference} isn't one of Kumi's scratch copies")));
        };
        let was = state.scratch[&set][at].1.clone();
        let title = format!("Kept “{name}”");
        let mut display = json!({"title": title});
        if renoise {
            display["note"] = "Renoise's own undo takes back only its name.".into();
        }
        match method {
            "change_preview" => Ok(json!({"epoch": epoch, "target": {"ref": reference, "kind": "track"}, "prior": {"name": was},
                "fence": {"name": was, "scratchId": set}, "display": display})),
            _ => {
                if params["fence"]["name"].as_str() != Some(was.as_str()) {
                    return Err(("changed".into(), format!("“{was}” changed since the preview")));
                }
                state.scratch.get_mut(&set).unwrap().remove(at);
                state.kept.insert(reference.clone(), name.clone());
                Ok(json!({"epoch": epoch, "applied": {"name": name}, "created": {"ref": reference, "kind": "track"}, "display": display,
                    "undo": {"desc": desc, "entry": true}}))
            }
        }
    }

    /// Whether a ref is one of the scratch copies.
    pub fn is_copy(&self, reference: &str) -> bool {
        self.state.lock().unwrap().scratch.values().flatten().any(|(copy, ..)| copy == reference)
    }

    /// The method's answer; None for one that isn't listening's. `known` says whether a track ref is in the project.
    pub fn answer(&self, method: &str, params: &Value, epoch: &str, known: &dyn Fn(&str) -> bool) -> Option<Answer> {
        let mut state = self.state.lock().unwrap();
        Some(match method {
            "render_start" => Self::render_start(&mut state, params, epoch, known),
            "capture_start" => Self::capture_start(&mut state, params),
            "listen_status" => Self::status(&mut state, params),
            "listen_cancel" => Self::cancel(&mut state, params),
            "audition" => Self::audition(&mut state, params),
            "audition_stop" => {
                let stopped = match params.get("previewId").and_then(Value::as_str) {
                    Some(id) => state.playing.as_deref() == Some(id),
                    None => state.playing.is_some(),
                };
                if stopped {
                    state.playing = None;
                }
                Ok(json!({"stopped": stopped}))
            }
            "audio_file" => Self::audio_file(&mut state, params, epoch, known),
            "scratch" => Self::scratch(&mut state, params, epoch, known),
            _ => return None,
        })
    }

    fn app(state: &State) -> &'static str {
        if state.daw == Daw::Reaper {
            "REAPER"
        } else {
            "Renoise"
        }
    }

    fn level(state: &State, reference: &str) -> f64 {
        if let Some((_, _, source, values)) = state.scratch.values().flatten().find(|(copy, ..)| copy == reference) {
            // A copy is its source, turned by what was set on it: the first value's 0 to 1 halves to one and a half times it.
            let base = state.levels.get(source).copied().unwrap_or(DEFAULT_LEVEL);
            let turned = values.last().and_then(|value| value.get("normalized").or(value.get("value"))).and_then(Value::as_f64);
            return base * turned.map_or(1., |at| 0.5 + at.clamp(0., 1.));
        }
        state.levels.get(reference).copied().unwrap_or(DEFAULT_LEVEL)
    }

    fn render_start(state: &mut State, params: &Value, epoch: &str, known: &dyn Fn(&str) -> bool) -> Answer {
        if state.demo {
            return Err((
                "failed".into(),
                "The Renoise demo can't render to a file, so Kumi can't hear the song: a licensed Renoise can".into(),
            ));
        }
        if state.jobs.values().any(|job| !matches!(job.state, "done" | "failed" | "cancelled")) {
            return Err(("busy".into(), format!("{} is rendering for Kumi; nothing changed", Self::app(state))));
        }
        let id = params["jobId"].as_str().ok_or_else(|| bad("jobId is missing"))?.to_string();
        let range = &params["range"];
        let (from, beats) = (range["fromBeat"].as_f64().unwrap_or(0.), range["beats"].as_f64().unwrap_or(0.));
        let pre_roll = params["preRollBeats"].as_f64().unwrap_or(0.).min(from);
        let seconds = (beats + pre_roll) * 60. / state.tempo;
        let start = pre_roll * 60. / state.tempo;
        let sources = params["sources"].as_array().cloned().unwrap_or_default();
        let mut files = vec![];
        let mut master = 0.;
        for source in &sources {
            let source_id = source["id"].as_str().unwrap_or_default().to_string();
            let path = state.folder.join(format!("{id}-{source_id}.wav"));
            if source["what"] == "master" {
                if source["beforeChain"] == true && state.daw == Daw::Renoise {
                    return Err(bad("Renoise renders the mix after its Master track's devices"));
                }
                files.push((source_id, path, start, seconds, -1., if source["beforeChain"] == true { "before-chain" } else { "master" }));
                continue;
            }
            let reference = source["ref"].as_str().unwrap_or_default();
            // (track refs are read in an epoch: the extension and the tool both want it)
            if params.get("epoch").and_then(Value::as_str).is_none() {
                return Err(bad("epoch: the epoch the track refs were read in"));
            }
            if state.daw == Daw::Renoise && source["stage"] == "items" {
                return Err(bad("Renoise's tracks hold no audio of their own"));
            }
            if !known(reference) && !state.scratch.values().flatten().any(|(copy, ..)| copy == reference) {
                return Err(("gone".into(), format!("{reference} isn't in the project any more: read again")));
            }
            let level = Self::level(state, reference);
            master += level;
            let stage = match (state.daw, source["stage"].as_str()) {
                (Daw::Renoise, _) => "post-fader",
                (_, Some("post-fader")) => "post-fader",
                (_, Some("items")) => "items",
                _ => "pre-fader",
            };
            files.push((source_id, path, start, seconds, level, stage));
        }
        // The master is every track: the sources' levels, or the default's when none was asked.
        let master = state.master_level.unwrap_or(if master > 0. { master.min(0.9) } else { DEFAULT_LEVEL })
            * 10f64.powf(state.master_gain_db / 20.);
        for file in files.iter_mut().filter(|file| file.4 < 0.) {
            file.4 = master;
        }
        if let Some(free) = params.get("freeBytes").and_then(Value::as_u64) {
            let needed = (seconds * RATE as f64 * 8. * files.len() as f64 * 1.5) as u64;
            if needed > free {
                return Err((
                    "failed".into(),
                    format!("Kumi's folder has {} MB free; this render needs about {} MB", free / 1_000_000, needed.div_ceil(1_000_000)),
                ));
            }
        }
        state.renders.push(params.clone());
        let answer = json!({
            "jobId": id,
            "state": "running",
            "seconds": seconds,
            "files": files.iter().map(|(source, path, ..)| json!({"id": source, "file": path})).collect::<Vec<_>>(),
            "epoch": epoch,
        });
        let error = state.fail_next.take();
        let notes = std::mem::take(&mut state.next_notes);
        state.jobs.insert(id, Job { kind: "render", state: "running", polls: 0, files, marks: None, error, notes });
        Ok(answer)
    }

    fn capture_start(state: &mut State, params: &Value) -> Answer {
        if state.daw == Daw::Renoise {
            return Err(("failed".into(), "Renoise's scripts can't hear what it plays".into()));
        }
        if !state.playing_now {
            return Err(("failed".into(), "REAPER isn't playing: give from_beat and beats, and Kumi renders the part".into()));
        }
        let id = params["jobId"].as_str().ok_or_else(|| bad("jobId is missing"))?.to_string();
        let seconds = params["seconds"].as_f64().unwrap_or(8.);
        let path = state.folder.join(format!("{id}-master.wav"));
        let answer = json!({"jobId": id, "state": "running", "seconds": seconds, "files": [{"id": "master", "file": path}]});
        let marks = state.capture_marks.take().unwrap_or_else(|| json!([[0, 10.0, 20.0, true]]));
        let files = vec![("master".to_string(), path, 0., seconds, DEFAULT_LEVEL, "master")];
        let notes = std::mem::take(&mut state.next_notes);
        state.jobs.insert(id, Job { kind: "capture", state: "running", polls: 0, files, marks: Some(marks), error: None, notes });
        Ok(answer)
    }

    fn status(state: &mut State, params: &Value) -> Answer {
        let id = params["jobId"].as_str().unwrap_or_default().to_string();
        let (steps, daw) = (state.steps, state.daw);
        let mut ended = false;
        let mut failed = None;
        {
            let Some(job) = state.jobs.get_mut(&id) else { return Err(bad(format!("no job {id}"))) };
            if job.state == "running" {
                job.polls += 1;
                if let Some((code, message)) = job.error.clone() {
                    // (a stop that had to wait for the render ends cancelled, its files gone)
                    let state_word = if code == "cancelled" { "cancelled" } else { "failed" };
                    job.state = state_word;
                    if code == "cancelled" {
                        for (_, path, ..) in &job.files {
                            let _ = std::fs::remove_file(path);
                        }
                    }
                    ended = true;
                    let mut answer =
                        json!({"jobId": id, "kind": job.kind, "state": state_word, "progress": 0, "error": message, "code": code});
                    if !job.notes.is_empty() {
                        answer["notes"] = json!(job.notes);
                    }
                    failed = Some(answer);
                } else if job.polls > steps {
                    for (n, (_, path, _, seconds, level, _)) in job.files.iter().enumerate() {
                        if let Err(error) = write_noise(path, *seconds, *level, n as u32 + 7) {
                            return Err(("failed".into(), format!("the sim couldn't write {}: {error}", path.display())));
                        }
                    }
                    job.state = "done";
                    ended = true;
                }
            }
        }
        if ended {
            state.restored += 1;
        }
        if let Some(failed) = failed {
            return Ok(failed);
        }
        let job = &state.jobs[&id];
        let progress = if job.state == "done" { 1. } else { f64::from(job.polls) / f64::from(steps + 1) };
        let mut answer = json!({"jobId": id, "kind": job.kind, "state": job.state, "progress": progress});
        if job.state == "done" {
            answer["files"] = job
                .files
                .iter()
                .map(|(source, path, start, seconds, _, stage)| {
                    let mut file = json!({"id": source, "file": path, "start": start, "seconds": seconds, "channels": 2, "sampleRate": RATE, "stage": stage, "throughMaster": daw == Daw::Renoise && *stage == "post-fader"});
                    if job.kind == "capture" {
                        file["partial"] = false.into();
                    }
                    file
                })
                .collect();
            if let Some(marks) = &job.marks {
                answer["marks"] = marks.clone();
            }
        }
        if !job.notes.is_empty() {
            answer["notes"] = json!(job.notes);
        }
        Ok(answer)
    }

    fn cancel(state: &mut State, params: &Value) -> Answer {
        let id = params["jobId"].as_str().unwrap_or_default().to_string();
        if state.slow_stop && state.jobs.get(&id).is_some_and(|job| job.state == "running") {
            let job = state.jobs.get_mut(&id).unwrap();
            job.error = Some(("cancelled".into(), "Renoise finished the render before it could stop".into()));
            return Ok(json!({"jobId": id, "state": "running", "stopping": true}));
        }
        let mut stopped = false;
        let now = {
            let Some(job) = state.jobs.get_mut(&id) else { return Err(bad(format!("no job {id}"))) };
            if job.state == "running" {
                job.state = "cancelled";
                for (_, path, ..) in &job.files {
                    let _ = std::fs::remove_file(path);
                }
                stopped = true;
            }
            job.state
        };
        if stopped {
            state.restored += 1;
        }
        Ok(json!({"jobId": id, "state": now}))
    }

    fn audition(state: &mut State, params: &Value) -> Answer {
        if !state.audition_works {
            return Err(("failed".into(), format!("{} can't play a file out loud", Self::app(state))));
        }
        let file = params["file"].as_str().unwrap_or_default();
        let length = std::fs::metadata(file).map(|meta| (meta.len().saturating_sub(44)) as f64 / (RATE as f64 * 8.)).unwrap_or(0.);
        let from = params["fromSeconds"].as_f64().unwrap_or(0.);
        let seconds = params["seconds"].as_f64().unwrap_or(f64::INFINITY).min((length - from).max(0.));
        let id = params["previewId"].as_str().unwrap_or_default().to_string();
        if state.daw == Daw::Renoise {
            // Renoise plays the song itself over the part a render of its own was made from, never over the producer's
            // playing (as the tool does).
            if state.playing_now {
                return Err(("failed".into(), "Renoise is playing: Kumi doesn't move your playhead".into()));
            }
            if !state.jobs.values().flat_map(|job| &job.files).any(|(_, path, ..)| path.as_path() == Path::new(file)) {
                return Err(("failed".into(), "Renoise plays only what it rendered for Kumi".into()));
            }
        }
        state.previews.push(params.clone());
        state.playing = Some(id.clone());
        let mut answer = json!({"previewId": id, "playing": true, "seconds": seconds});
        match state.daw {
            Daw::Reaper => answer["via"] = "preview".into(),
            Daw::Renoise => {
                answer["via"] = "song".into();
                answer["note"] = "Renoise plays the song over that part".into();
            }
        }
        Ok(answer)
    }

    fn audio_file(state: &mut State, params: &Value, epoch: &str, known: &dyn Fn(&str) -> bool) -> Answer {
        let reference = params["ref"].as_str().unwrap_or_default().to_string();
        if state.silent_refs.contains(&reference) {
            return Err(match state.daw {
                Daw::Reaper => (
                    "failed".into(),
                    "That's a MIDI item, which has no sound of its own: listen to its track instead (Kumi renders it)".into(),
                ),
                Daw::Renoise => ("failed".into(), "That sample has no audio".into()),
            });
        }
        let kind = reference.split(':').nth(1).unwrap_or_default();
        match (state.daw, kind) {
            (Daw::Reaper, "item" | "take") => {
                let path = state.folder.join("media").join(format!("{}.wav", kind_file(&reference)));
                let seconds = if state.item_part.is_some() { 4. } else { 2. };
                write_noise(&path, seconds, DEFAULT_LEVEL, 3).map_err(|error| ("failed".to_string(), error.to_string()))?;
                let mut answer =
                    json!({"file": path, "seconds": seconds, "channels": 2, "sampleRate": RATE, "kind": "source", "name": "loop.wav"});
                if let Some((offset, length, rate, reversed)) = state.item_part {
                    answer["offset"] = offset.into();
                    answer["length"] = length.into();
                    if (rate - 1.).abs() > 1e-9 {
                        answer["playRate"] = rate.into();
                    }
                    if reversed {
                        answer["reversed"] = true.into();
                    }
                }
                Ok(answer)
            }
            (Daw::Renoise, "sample") => {
                let id = params["jobId"].as_str().unwrap_or("j0");
                let path = state.folder.join(format!("{id}-sample.wav"));
                write_noise(&path, 1., DEFAULT_LEVEL, 5).map_err(|error| ("failed".to_string(), error.to_string()))?;
                Ok(json!({"file": path, "seconds": 1.0, "channels": 2, "sampleRate": RATE, "kind": "exported", "name": "Kick"}))
            }
            (Daw::Renoise, "pattern" | "seq") => {
                let render = json!({"jobId": params["jobId"], "sources": [{"id": "pattern", "what": "master"}], "range": {"fromBeat": 0, "beats": 16}, "preRollBeats": 0});
                let mut answer = Self::render_start(state, &render, epoch, known)?;
                answer["kind"] = "render".into();
                Ok(answer)
            }
            _ => Err(bad(format!("{reference} has no audio of its own"))),
        }
    }

    fn scratch(state: &mut State, params: &Value, epoch: &str, known: &dyn Fn(&str) -> bool) -> Answer {
        let id = params["scratchId"].as_str().unwrap_or_default().to_string();
        let daw = state.daw;
        let steps = if daw == Daw::Renoise { 1 } else { 0 };
        match params["action"].as_str() {
            Some("make") => {
                if params.get("epoch").and_then(Value::as_str).is_none() {
                    return Err(bad("epoch: the epoch the track ref was read in"));
                }
                let source = params["ref"].as_str().unwrap_or_default().to_string();
                if !known(&source) {
                    return Err(("gone".into(), format!("{source} isn't in the project any more: read again")));
                }
                let count = params["count"].as_u64().unwrap_or(1);
                let tag: String = id.chars().rev().take(4).collect();
                let mut copies = vec![];
                for n in 1..=count {
                    state.next_copy += 1;
                    let reference = match state.daw {
                        Daw::Reaper => format!("r:track:{{C{:04}}}", state.next_copy),
                        Daw::Renoise => format!("{epoch}:track:{}", 900 + state.next_copy),
                    };
                    copies.push((reference, format!("Kumi · try {tag} {n}"), source.clone(), vec![]));
                }
                let answer = json!({
                    "scratchId": id,
                    "copies": copies.iter().map(|(reference, name, ..)| json!({"ref": reference, "name": name})).collect::<Vec<_>>(),
                    "epoch": epoch,
                    "undo": {"steps": steps},
                });
                state.scratch.insert(id, copies);
                Ok(answer)
            }
            Some("set") => {
                let Some(copies) = state.scratch.get_mut(&id) else { return Err(bad(format!("no scratch copies {id}"))) };
                let mut set = 0;
                for value in params["values"].as_array().into_iter().flatten() {
                    // REAPER sets a normalized value, Renoise the parameter's own (as each bridge takes it)
                    let (wanted, other) = if daw == Daw::Reaper { ("normalized", "value") } else { ("value", "normalized") };
                    if value.get(wanted).is_none() || value.get(other).is_some() {
                        return Err(bad(format!("each value: {wanted}")));
                    }
                    let reference = value["ref"].as_str().unwrap_or_default();
                    let copy = copies
                        .iter_mut()
                        .find(|(copy, ..)| copy == reference)
                        .ok_or_else(|| bad(format!("{reference} isn't one of {id}'s copies")))?;
                    copy.3.push(value.clone());
                    set += 1;
                }
                Ok(json!({"set": set, "undo": {"steps": steps}}))
            }
            Some("drop") => match state.scratch.remove(&id) {
                Some(copies) => Ok(json!({"dropped": copies.len(), "left": 0, "undo": {"steps": steps}})),
                // (REAPER answers a drop it doesn't know as nothing dropped; Renoise refuses it)
                None if daw == Daw::Reaper => Ok(json!({"dropped": 0, "left": 0, "undo": {"steps": 0}})),
                None => Err(bad(format!("no scratch copies {id}"))),
            },
            _ => Err(bad("action is make, set or drop")),
        }
    }
}

/// A file name from a ref's id.
fn kind_file(reference: &str) -> String {
    reference.chars().filter(|c| c.is_ascii_alphanumeric()).collect()
}
