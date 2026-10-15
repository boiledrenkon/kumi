//! Opt-in check on real Live that the devices Kumi makes keep Live's time. It records a device's MIDI into the
//! Arrangement, through a second track that takes the device's track's output. While it records, it moves the
//! device's rate to a slower division and back. Then it reads the notes and says, for each run:
//! - how far each note starts from the grid line it belongs to, in milliseconds: the mean and the worst, and how many
//!   are off the grid of either division (past a tolerance);
//! - doubled notes (two starts within a millisecond);
//! - notes held past their step (a stuck or missed note-off).
//!
//! It checks a step sequencer made with make_device from Kumi's [kumi.clock] and [kumi.notes], and any device file
//! given (--file <path>.amxd, with --rate "<its rate parameter>", --from <index> --to <index> for its two divisions).
//! No model and no sign-in. It works in the open Set (it never opens or closes one): name it, and use a disposable
//! one. It sets the tempo to 120 and takes everything back after.
//!   cargo build -p ableton-mcp-server -p kumi-runtime --bins
//!   cargo run -p kumi --example device_timing_live -- --set "<Set name>" [--file <path>.amxd --rate Rate --from 4 --to 3]
use futures::FutureExt;
use kumi::config::find_bridge_config;
use kumi_common::{
    abort::{self, Signal},
    js::string::{head, trim},
    time::now_ms,
};
use kumi_runtime::{
    core::contracts::ChangeState,
    create_ableton_integration,
    devices::{amxd::decode_amxd, tool::kind_folder},
    integrations::ableton::{connection::Connect, samples::user_library, AbletonOptions},
    mcp::client,
    ChangeRecord, Integration, JsonObject, KernelTool, Observation, RuntimeError, BRIDGE_TOOLS,
};
use serde_json::{json, Value};
use std::{
    cell::RefCell,
    path::{Path, PathBuf},
    rc::Rc,
    time::Duration,
};

/// The tempo the check plays at, and how far a note may start from its line and still be on it.
const BPM: f64 = 120.0;
const ON_GRID_MS: f64 = 1.5;
/// How long it records, in beats, and when it moves the rate and back (seconds after the recording starts).
const BEATS: f64 = 16.0;
const SLOWER_AT: f64 = 3.3;
const BACK_AT: f64 = 5.7;

fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let values = |flag: &str| -> Vec<String> {
        argv.windows(2).filter(|pair| pair[0] == flag).map(|pair| trim(&pair[1]).to_owned()).filter(|value| !value.is_empty()).collect()
    };
    let Some(wanted) = values("--set").into_iter().next() else {
        eprintln!("Name the open Set, as Live shows it: cargo run -p kumi --example device_timing_live -- --set \"<Set name>\"");
        eprintln!("It adds tracks, records in the Arrangement and undoes it all, so use a disposable Set.");
        std::process::exit(2);
    };
    let file = values("--file").into_iter().next().map(|file| Subject {
        file: PathBuf::from(file),
        rate: values("--rate").into_iter().next().unwrap_or_else(|| "Rate".into()),
        from: values("--from").into_iter().next().and_then(|value| value.parse().ok()).unwrap_or(4.0),
        to: values("--to").into_iter().next().and_then(|value| value.parse().ok()).unwrap_or(3.0),
    });
    let env = std::env::vars().collect();
    let Some(bridge_config) = find_bridge_config(&env) else {
        eprintln!("The Ableton bridge isn't installed; run Kumi's bridge setup first.");
        std::process::exit(2);
    };
    let bridge = bridge_program();
    let harness = bridge.with_file_name(if cfg!(windows) { "kumi-harness.exe" } else { "kumi-harness" });
    if !bridge.is_file() || !harness.is_file() {
        eprintln!("Build the bridge and Kumi's check first: cargo build -p ableton-mcp-server -p kumi-runtime --bins");
        std::process::exit(2);
    }
    if std::env::var_os("KUMI_HARNESS_BIN").is_none() {
        std::env::set_var("KUMI_HARNESS_BIN", &harness);
    }
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().expect("a runtime");
    let local = tokio::task::LocalSet::new();
    std::process::exit(local.block_on(&runtime, device_timing_live(wanted, file, bridge_config, bridge)));
}

/// A device file to check: its rate parameter and the two values it's moved between.
struct Subject {
    file: PathBuf,
    rate: String,
    from: f64,
    to: f64,
}

fn bridge_program() -> PathBuf {
    let name = if cfg!(windows) { "ableton-mcp-server.exe" } else { "ableton-mcp-server" };
    let mut folder = std::env::current_exe().ok().and_then(|exe| exe.parent().map(Path::to_path_buf)).unwrap_or_default();
    if folder.file_name().is_some_and(|name| name == "examples") {
        folder.pop();
    }
    folder.join(name)
}

fn connect_to(bridge: PathBuf, config: String) -> Connect {
    Rc::new(move |signal| {
        client::connect_mcp(client::Options {
            signal,
            bridge_config: Some(PathBuf::from(&config)),
            entry: Some(bridge.clone()),
            cwd: bridge.parent().map(Path::to_path_buf),
            allow_tools: BRIDGE_TOOLS.clone(),
            ..Default::default()
        })
        .boxed_local()
    })
}

struct Run {
    integration: Rc<dyn Integration>,
    observation: RefCell<Option<Observation>>,
    records: Rc<RefCell<Vec<ChangeRecord>>>,
    made: RefCell<Vec<PathBuf>>,
    failed: RefCell<Vec<String>>,
}

async fn device_timing_live(wanted: String, file: Option<Subject>, bridge_config: String, bridge: PathBuf) -> i32 {
    let records = Rc::new(RefCell::new(Vec::<ChangeRecord>::new()));
    let mut options = AbletonOptions::new(Rc::new(|_, _| {}));
    options.bridge_config = Some(bridge_config.clone());
    options.connect = Some(connect_to(bridge, bridge_config));
    options.on_change = Some({
        let records = records.clone();
        Rc::new(move |change: ChangeRecord| {
            let mut records = records.borrow_mut();
            match records.iter_mut().find(|record| record.id == change.id) {
                Some(record) => *record = change,
                None => records.push(change),
            }
        })
    });
    options.change_timeout_ms = Some(30_000);
    let run =
        Run { integration: create_ableton_integration(options), observation: RefCell::default(), records, made: RefCell::default(), failed: RefCell::default() };
    let code = match run.check(&wanted, file).await {
        Ok(()) => {
            let failed = run.failed.borrow();
            println!("\n{} failed{}", failed.len(), if failed.is_empty() { String::new() } else { format!(": {}", failed.join("; ")) });
            i32::from(!failed.is_empty())
        }
        Err(error) => {
            eprintln!("device-timing-live: {}", head(&error.message(), 300));
            1
        }
    };
    let _ = run.call("play", json!({"action": "stop"})).await;
    println!("\nUndo, newest first");
    run.undo_all().await;
    let _ = run.integration.close().await;
    for file in run.made.borrow().iter() {
        let _ = std::fs::remove_file(file);
    }
    code
}

/// What the notes of one take say about its timing.
#[derive(Debug, Default)]
struct Timing {
    notes: usize,
    mean_ms: f64,
    worst_ms: f64,
    off_grid: Vec<String>,
    doubled: Vec<String>,
    held: Vec<String>,
    /// The notes furthest from their lines, worst first.
    worst: Vec<String>,
    /// How many start within 0.1, 0.5, 1 and 1.5 ms of their lines, and further.
    spread: [usize; 5],
}

/// The notes' timing against two grids, `fine` and `coarse` (step lengths in beats): each note belongs to the nearer.
fn timing(notes: &[Value], fine: f64, coarse: f64) -> Timing {
    let ms = |beats: f64| beats * 60_000.0 / BPM;
    let mut starts: Vec<(f64, f64, i64)> = notes
        .iter()
        .filter_map(|note| Some((note["start"].as_f64()?, note["duration"].as_f64()?, note["pitch"].as_i64()?)))
        .collect();
    starts.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut found = Timing { notes: starts.len(), ..Timing::default() };
    let mut total = 0.0;
    let mut offs: Vec<(f64, String)> = Vec::new();
    for (start, duration, pitch) in &starts {
        let off = |grid: f64| ms(start - (start / grid).round() * grid);
        let (a, b) = (off(fine), off(coarse));
        let off = if a.abs() <= b.abs() { a } else { b };
        offs.push((off.abs(), format!("{pitch} at beat {start:.4} ({off:+.2} ms)")));
        let bin = [0.1, 0.5, 1.0, 1.5].iter().position(|edge| off.abs() < *edge).unwrap_or(4);
        found.spread[bin] += 1;
        total += off;
        found.worst_ms = found.worst_ms.max(off.abs());
        if off.abs() > ON_GRID_MS {
            found.off_grid.push(format!("{pitch} at beat {start:.4} ({off:+.2} ms)"));
        }
        if *duration > coarse * 1.05 {
            found.held.push(format!("{pitch} at beat {start:.4} held {duration:.3} beats"));
        }
    }
    for pair in starts.windows(2) {
        if ms(pair[1].0 - pair[0].0) < 1.0 {
            found.doubled.push(format!("{} and {} at beat {:.4}", pair[0].2, pair[1].2, pair[0].0));
        }
    }
    found.mean_ms = if starts.is_empty() { 0.0 } else { total / starts.len() as f64 };
    offs.sort_by(|a, b| b.0.total_cmp(&a.0));
    found.worst = offs.into_iter().take(3).map(|(_, said)| said).collect();
    found
}

impl Run {
    fn fail(&self, what: &str) {
        self.failed.borrow_mut().push(what.to_string());
    }

    async fn observe(&self) -> Result<Observation, RuntimeError> {
        let observation = self.integration.observe(signal(), None).await?;
        *self.observation.borrow_mut() = Some(observation.clone());
        Ok(observation)
    }

    async fn call(&self, name: &str, input: Value) -> Result<Value, String> {
        let tool: Option<Rc<dyn KernelTool>> =
            self.observation.borrow().as_ref().and_then(|seen| seen.tools.iter().find(|tool| tool.name() == name).cloned());
        let Some(tool) = tool else { return Err(format!("{name} isn't offered for this Set")) };
        let input = match input {
            Value::Object(input) => input,
            _ => JsonObject::new(),
        };
        match tool.execute(input, signal()).await {
            Ok(result) if !result.is_error => Ok(serde_json::from_str(&result.text).unwrap_or(Value::String(result.text))),
            Ok(result) => Err(result.text),
            Err(error) => Err(error.message()),
        }
    }

    async fn rows(&self, kind: &str, extra: Value) -> Vec<Value> {
        let mut input = json!({"kind": kind, "limit": 100});
        input.as_object_mut().unwrap().extend(extra.as_object().cloned().unwrap_or_default());
        self.call("live_discover", input)
            .await
            .map(|body| body["live"]["items"].as_array().cloned().unwrap_or_default())
            .unwrap_or_default()
    }

    async fn python(&self, code: &str) -> Result<Value, RuntimeError> {
        let done = self.call("run_python", json!({"code": code, "mode": "eval"})).await.map_err(RuntimeError::plain)?;
        let done = if done.get("ok").is_some() { done } else { done["live"].clone() };
        if done["ok"] != true {
            return Err(RuntimeError::plain(format!("Live's Python: {}", head(&done.to_string(), 300))));
        }
        Ok(done["result"].clone())
    }

    async fn track(&self, name: &str) -> Result<Value, RuntimeError> {
        let tracks = self.rows("track", json!({"fields": ["name"]})).await;
        tracks.iter().rfind(|row| row["name"] == name).cloned().ok_or_else(|| RuntimeError::plain(format!("the track {name} is gone")))
    }

    async fn add_track(&self, name: &str) -> Result<(), RuntimeError> {
        self.call("add_tracks_and_scenes", json!({"tracks": [{"name": name, "kind": "midi"}], "scenes": []}))
            .await
            .map_err(RuntimeError::plain)?;
        self.observe().await?;
        Ok(())
    }

    /// A copy of a device file in its kind's folder, as `name`: its Browser item.
    fn copy(&self, file: &Path, name: &str) -> Result<String, String> {
        let bytes = std::fs::read(file).map_err(|error| error.to_string())?;
        let kind = decode_amxd(&bytes).ok_or("not a Max for Live device file")?.kind;
        let folder = PathBuf::from(user_library(None, None)).join("Kumi").join(kind_folder(kind));
        std::fs::create_dir_all(&folder).map_err(|error| error.to_string())?;
        let copy = folder.join(format!("{name}.amxd"));
        std::fs::write(&copy, bytes).map_err(|error| error.to_string())?;
        self.made.borrow_mut().push(copy);
        Ok(format!("user_library/Kumi/{}/{name}", kind_folder(kind)))
    }

    /// The device loaded on the track, waiting for Live's Browser to list it.
    async fn load(&self, name: &str, item: &str) -> Result<(), String> {
        for attempt in 0..15 {
            let track = self.track(name).await.map_err(|error| error.message())?;
            match self.call("load_device", json!({"itemId": item, "trackRef": track["ref"]})).await {
                Ok(_) => return Ok(()),
                Err(why) if attempt < 14 => {
                    println!("    (not loaded yet: {})", head(&why, 120));
                    tokio::time::sleep(Duration::from_secs(2)).await;
                    self.observe().await.map_err(|error| error.message())?;
                }
                Err(why) => return Err(why),
            }
        }
        Err("not loaded".into())
    }

    /// One take: the device on its own track, its MIDI recorded on another for BEATS beats while its rate moves from
    /// `from` to `to` and back. The notes recorded.
    async fn take(&self, label: &str, item: &str, rate: &str, from: f64, to: f64, stamp: u128) -> Result<Vec<Value>, RuntimeError> {
        let device_track = format!("Timing {label} {stamp}");
        let take_track = format!("Timing Take {label} {stamp}");
        self.add_track(&device_track).await?;
        self.load(&device_track, item).await.map_err(RuntimeError::plain)?;
        self.add_track(&take_track).await?;
        let take = self.track(&take_track).await?;
        self.call(
            "set_routing",
            json!({"trackRef": take["ref"], "inputType": device_track, "inputSubRouting": "Post FX", "outputType": "No Output", "arm": true, "monitoring": "off"}),
        )
        .await
        .map_err(RuntimeError::plain)?;
        let set = |value: f64| format!(
            "[p for p in [t for t in song.tracks if t.name == {}][0].devices[0].parameters if p.name == {}][0].__setattr__('value', {value})",
            json!(device_track),
            json!(rate)
        );
        self.python(&set(from)).await?;
        self.python(&format!("song.__setattr__('tempo', {BPM:.1})")).await?;
        self.call("set_transport", json!({"position": 0, "loopEnabled": false})).await.map_err(RuntimeError::plain)?;
        // A change makes refs found before it stale: the take's track is found again.
        self.observe().await?;
        let take = self.track(&take_track).await?;
        self.call("record", json!({"action": "start", "lane": "arrangement", "destinationTrackRef": take["ref"]})).await.map_err(RuntimeError::plain)?;
        let began = std::time::Instant::now();
        let at = |seconds: f64| async move {
            let left = seconds - began.elapsed().as_secs_f64();
            if left > 0.0 {
                tokio::time::sleep(Duration::from_secs_f64(left)).await;
            }
        };
        at(SLOWER_AT).await;
        self.python(&set(to)).await?;
        at(BACK_AT).await;
        self.python(&set(from)).await?;
        at(BEATS * 60.0 / BPM).await;
        self.call("play", json!({"action": "stop"})).await.map_err(RuntimeError::plain)?;
        let _ = self.call("record", json!({"action": "stop", "lane": "arrangement"})).await;
        self.observe().await?;
        let take = self.track(&take_track).await?;
        self.call("set_routing", json!({"trackRef": take["ref"], "arm": false})).await.map_err(RuntimeError::plain)?;
        self.observe().await?;
        let take = self.track(&take_track).await?;
        let clips = self.rows("arrangement-clip", json!({"parent": take["ref"]})).await;
        let Some(clip) = clips.first() else { return Ok(Vec::new()) };
        let read = self.call("read_notes", json!({"clipRef": clip["ref"], "format": "json"})).await.map_err(RuntimeError::plain)?;
        Ok(read["clips"][0]["notes"].as_array().cloned().unwrap_or_default())
    }

    fn say(&self, label: &str, notes: &[Value], fine: f64, coarse: f64) {
        let found = timing(notes, fine, coarse);
        println!(
            "  {label}: {} notes, mean {:+.2} ms from the grid, worst {:.2} ms; {} off the grid, {} doubled, {} held past a step",
            found.notes,
            found.mean_ms,
            found.worst_ms,
            found.off_grid.len(),
            found.doubled.len(),
            found.held.len()
        );
        println!("    worst: {}", found.worst.join(", "));
        let [a, b, c, d, e] = found.spread;
        println!("    from their lines: {a} under 0.1 ms, {b} under 0.5, {c} under 1, {d} under 1.5, {e} further");
        for line in found.off_grid.iter().take(80).chain(found.doubled.iter().take(8)).chain(found.held.iter().take(8)) {
            println!("    {line}");
        }
        if found.notes == 0 || !found.off_grid.is_empty() || !found.doubled.is_empty() || !found.held.is_empty() {
            self.fail(label);
        }
    }

    async fn check(&self, wanted: &str, file: Option<Subject>) -> Result<(), RuntimeError> {
        self.integration.start(signal()).await?;
        let observation = self.observe().await?;
        let context: Value = serde_json::from_str(&observation.context).unwrap_or(Value::Null);
        let set = context["set"]["name"].as_str().unwrap_or("?").to_owned();
        if set != wanted {
            println!("The open Set is “{set}”, not “{wanted}”; nothing was changed.");
            return Err(RuntimeError::plain("not the Set named"));
        }
        let stamp = (now_ms() % 10_000) as u128;
        // A step sequencer made the way the guide says: every step on, its pitch the step's index.
        let spec = json!({
            "type": "midi_effect",
            "name": format!("Timing Check {stamp}"),
            "about": "Kumi's timing check: a note on every step.",
            "controls": [{"name": "Rate", "type": "choice", "options": ["1/4", "1/8", "1/16", "1/32"], "default": "1/16", "group": "Clock"}],
            "patch": concat!(
                "clock = [kumi.clock 4n 8n 16n 32n]\n",
                "\"Rate\" -> clock\n",
                "notes = [kumi.notes]\n",
                "clock -> [+ 60] -> notes\n",
                "clock.1 -> [* 0.5] -> notes.2\n",
                "notes -> out\n",
            ),
        });
        let made = self.call("make_device", spec).await.map_err(RuntimeError::plain)?;
        for key in ["wrong", "changed", "notes"] {
            if let Some(said) = made.get(key) {
                println!("  make_device {key}: {said}");
            }
        }
        if let Some(file) = made["file"].as_str() {
            self.made.borrow_mut().push(PathBuf::from(file));
        }
        let item = made["itemId"].as_str().unwrap_or_default().to_string();
        println!("\nKumi's clock, 1/16 moved to 1/8 and back");
        let notes = self.take("clock", &item, "Rate", 2.0, 1.0, stamp).await?;
        self.say("Kumi's clock", &notes, 0.25, 0.5);
        if let Some(subject) = file {
            println!("\n{}, its {} moved from {} to {} and back", subject.file.display(), subject.rate, subject.from, subject.to);
            let item = self.copy(&subject.file, &format!("Timing File {stamp}")).map_err(RuntimeError::plain)?;
            self.observe().await?;
            let notes = self.take("file", &item, &subject.rate, subject.from, subject.to, stamp).await?;
            self.say("the file", &notes, 0.25, 0.5);
        }
        Ok(())
    }

    async fn undo_all(&self) {
        let records = self.records.borrow().clone();
        for record in records.iter().rev().filter(|record| record.state == ChangeState::Applied) {
            match self.integration.undo(Some(&record.id), signal()).await {
                Ok(after) => println!("  {:?} · {}", after.state, record.title),
                Err(error) => println!("  FAIL {}: {}", record.title, head(&error.message(), 200)),
            }
        }
    }
}

fn signal() -> Signal {
    abort::timeout(120_000)
}
