//! Opt-in check on real Live that the audio effects Kumi makes load after an instrument and let sound through. It adds
//! a Drift track playing a melody, reads its output meter dry, then makes each effect with make_device (as code, and as
//! a patch of Max's own objects), loads it after Drift, reads the meter again and takes the effect away. A device file
//! can be checked too (--file <path>.amxd): a copy goes in its kind's folder for the check. No model and no sign-in. It
//! works in the open Set (it never opens or closes one): name it, and use a disposable one. It plays about 5 seconds of
//! sound for each effect.
//!   cargo build -p ableton-mcp-server --bins
//!   cargo run -p kumi --example device_sound_live -- --set "<Set name>" [--file <path>.amxd]…
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
    system::Env,
    ChangeRecord, Integration, JsonObject, KernelTool, Observation, RuntimeError, BRIDGE_TOOLS,
};
use serde_json::{json, Value};
use std::{
    cell::RefCell,
    path::{Path, PathBuf},
    rc::Rc,
    time::Duration,
};

/// How long Live's meter takes to rise to a new level and fall from full, in milliseconds.
const SETTLE_MS: u64 = 2500;

fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let values = |flag: &str| -> Vec<String> {
        argv.windows(2).filter(|pair| pair[0] == flag).map(|pair| trim(&pair[1]).to_owned()).filter(|value| !value.is_empty()).collect()
    };
    let Some(wanted) = values("--set").into_iter().next() else {
        eprintln!("Name the open Set, as Live shows it: cargo run -p kumi --example device_sound_live -- --set \"<Set name>\"");
        eprintln!("It adds a track, plays the Set and undoes it all, so use a disposable Set.");
        std::process::exit(2);
    };
    let env: Env = std::env::vars().collect();
    let Some(bridge_config) = find_bridge_config(&env) else {
        eprintln!("The Ableton bridge isn't installed; run Kumi's bridge setup first.");
        std::process::exit(2);
    };
    let bridge = bridge_program();
    if !bridge.is_file() {
        eprintln!("The Ableton bridge isn't built ({}); build it first: cargo build -p ableton-mcp-server --bins", bridge.display());
        std::process::exit(2);
    }
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().expect("a runtime");
    let local = tokio::task::LocalSet::new();
    std::process::exit(local.block_on(&runtime, device_sound_live(wanted, values("--file"), bridge_config, bridge)));
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

/// The effects Kumi makes for the check: each as the model would ask for it.
fn effects(stamp: u128) -> Vec<(String, Value)> {
    let knob =
        |name: &str, min: f64, max: f64, default: f64| json!({"name": name, "type": "number", "min": min, "max": max, "default": default});
    let made = |label: &str, spec: Value| {
        let mut spec = spec;
        spec["type"] = json!("audio_effect");
        spec["name"] = json!(format!("Sound Check {label} {stamp}"));
        spec["about"] = json!(format!("Kumi's sound check: {label}."));
        (format!("make_device {label}"), spec)
    };
    vec![
        made("code thru", json!({"controls": [], "code": "out1 = in1;\nout2 = in2;"})),
        made(
            "code drive",
            json!({"controls": [knob("Drive", 1.0, 10.0, 2.0)], "code": "out1 = tanh(in1 * drive);\nout2 = tanh(in2 * drive);"}),
        ),
        made(
            "code gain",
            json!({"controls": [knob("Gain", -24.0, 24.0, 0.0)], "code": "g = dbtoa(gain);\nout1 = in1 * g;\nout2 = in2 * g;"}),
        ),
        made(
            "patch objects",
            json!({"controls": [knob("Gain", -24.0, 24.0, 0.0)], "patch": concat!(
                "left = [*~ 1.]\n",
                "right = [*~ 1.]\n",
                "in.0 -> left -> out.0\n",
                "in.1 -> right -> out.1\n",
                "\"Gain\" -> [dbtoa] -> both\n",
                "both = [t f f]\n",
                "both.0 -> left.1\n",
                "both.1 -> right.1\n",
            )}),
        ),
        made(
            "patch gen",
            json!({"controls": [knob("Drive", 1.0, 10.0, 2.0)], "patch": concat!(
                "fx = [gen~] {\n",
                "  Param drive(2);\n",
                "  out1 = tanh(in1 * drive);\n",
                "  out2 = tanh(in2 * drive);\n",
                "}\n",
                "in.0 -> fx\n",
                "in.1 -> fx.1\n",
                "fx -> out.0\n",
                "fx.1 -> out.1\n",
                "\"Drive\" -> [prepend drive] -> fx\n",
            )}),
        ),
    ]
}

struct Run {
    integration: Rc<dyn Integration>,
    observation: RefCell<Option<Observation>>,
    records: Rc<RefCell<Vec<ChangeRecord>>>,
    made: RefCell<Vec<PathBuf>>,
}

async fn device_sound_live(wanted: String, files: Vec<String>, bridge_config: String, bridge: PathBuf) -> i32 {
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
    let run = Run { integration: create_ableton_integration(options), observation: RefCell::default(), records, made: RefCell::default() };
    let code = match run.check(&wanted, &files).await {
        Ok(silent) => i32::from(silent > 0),
        Err(error) => {
            eprintln!("device-sound-live: {}", head(&error.message(), 300));
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

impl Run {
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
            Ok(result) => Err(head(&result.text, 400)),
            Err(error) => Err(head(&error.message(), 400)),
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

    /// Python's answer in Live, for an expression.
    async fn python(&self, code: &str) -> Result<Value, RuntimeError> {
        let done = self.call("run_python", json!({"code": code, "mode": "eval"})).await.map_err(RuntimeError::plain)?;
        let done = if done.get("ok").is_some() { done } else { done["live"].clone() };
        if done["ok"] != true {
            return Err(RuntimeError::plain(format!("Live's Python: {}", head(&done.to_string(), 300))));
        }
        Ok(done["result"].clone())
    }

    /// The loudest the track's output meter reads over `seconds` (read every 100 ms).
    async fn loudest(&self, name: &str, seconds: f64) -> f64 {
        let code = format!("[t for t in song.tracks if t.name == {}][0].output_meter_level", json!(name));
        let mut loudest = 0_f64;
        let began = std::time::Instant::now();
        while began.elapsed().as_secs_f64() < seconds {
            if let Ok(level) = self.python(&code).await {
                loudest = loudest.max(level.as_f64().unwrap_or(0.));
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        loudest
    }

    /// The devices on the track, in order, each with whether it's on.
    async fn devices(&self, name: &str) -> Value {
        let code = format!(
            "[[d.name, d.class_name, d.is_active, [p.value for p in d.parameters][:1]] for d in [t for t in song.tracks if t.name == {}][0].devices]",
            json!(name)
        );
        self.python(&code).await.unwrap_or(Value::Null)
    }

    /// A MIDI track playing a melody through Drift for sixteen bars in the Arrangement from its start.
    async fn part(&self, name: &str) -> Result<(), RuntimeError> {
        let added = self
            .call("add_tracks_and_scenes", json!({"tracks": [{"name": name, "kind": "midi"}], "scenes": []}))
            .await
            .map_err(RuntimeError::plain)?;
        let track = added["live"]["created"].as_array().and_then(|created| created.iter().find(|item| item["kind"] == "track").cloned());
        let track = track.ok_or_else(|| RuntimeError::plain("the new track didn't come back"))?;
        let rows: Vec<Value> = (0..8)
            .map(|step| json!({"pitch": 60 + [0, 2, 4, 7, 9, 7, 4, 2][step], "start": step * 2, "duration": 1.75, "velocity": 110}))
            .collect();
        self.call("write_midi_clip", json!({"trackRef": track["ref"], "sceneIndex": 0, "name": name, "length": 16, "notes": rows}))
            .await
            .map_err(RuntimeError::plain)?;
        let found = self.call("live_browser_search", json!({"category": "instruments", "query": "Drift", "limit": 1})).await;
        let item = found.ok().map(|found| found["live"]["items"][0].clone()).filter(|item| !item.is_null());
        let item = item.ok_or_else(|| RuntimeError::plain("Drift isn't in Live's browser"))?;
        self.call("load_device", json!({"itemId": item["id"], "trackRef": track["ref"]})).await.map_err(RuntimeError::plain)?;
        self.observe().await?;
        let track = self.track(name).await?;
        let slots = self.rows("clip-slot", json!({"parent": track["ref"], "fields": ["clipRef"]})).await;
        let clip = slots.first().map(|slot| slot["clipRef"].clone()).filter(Value::is_string);
        let clip = clip.ok_or_else(|| RuntimeError::plain("the clip didn't come back"))?;
        for copy in 0..8 {
            self.call("duplicate_clip", json!({"clipRef": clip, "arrangementPosition": copy * 16})).await.map_err(RuntimeError::plain)?;
        }
        Ok(())
    }

    async fn track(&self, name: &str) -> Result<Value, RuntimeError> {
        let tracks = self.rows("track", json!({"fields": ["name"]})).await;
        tracks.iter().rfind(|row| row["name"] == name).cloned().ok_or_else(|| RuntimeError::plain("the track is gone"))
    }

    /// The device loaded at the end of the track, once Live's browser lists it (a new file takes a moment).
    async fn load(&self, name: &str, item: &str) -> Result<(), RuntimeError> {
        let began = std::time::Instant::now();
        loop {
            let track = self.track(name).await?;
            match self.call("load_device", json!({"itemId": item, "trackRef": track["ref"]})).await {
                Ok(_) => return Ok(()),
                Err(why) if began.elapsed() < Duration::from_secs(30) => {
                    println!("    (not loaded yet: {})", head(&why, 120));
                    tokio::time::sleep(Duration::from_secs(2)).await;
                    self.observe().await?;
                }
                Err(why) => return Err(RuntimeError::plain(format!("Live didn't load {item}: {why}"))),
            }
        }
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

    /// The newest change still applied, taken back (the effect just loaded).
    async fn undo_last(&self) {
        let last = self.records.borrow().iter().rev().find(|record| record.state == ChangeState::Applied).cloned();
        if let Some(record) = last {
            if let Err(error) = self.integration.undo(Some(&record.id), signal()).await {
                println!("    couldn't take it away: {}", head(&error.message(), 200));
            }
        }
    }

    /// Each effect loaded after Drift and heard: how many let no sound through.
    async fn check(&self, wanted: &str, files: &[String]) -> Result<usize, RuntimeError> {
        self.integration.start(signal()).await?;
        let observation = self.observe().await?;
        let context: Value = serde_json::from_str(&observation.context).unwrap_or(Value::Null);
        let set = context["set"]["name"].as_str().unwrap_or("?").to_owned();
        if set != wanted {
            println!("The open Set is “{set}”, not “{wanted}”; nothing was changed.");
            return Err(RuntimeError::plain("not the Set named"));
        }
        let stamp = now_ms() % 10_000;
        let name = format!("Kumi Sound {stamp}");
        println!("A track of its own: “{name}” (Drift, a melody over sixteen bars)");
        self.part(&name).await?;
        self.call("set_transport", json!({"position": 0})).await.map_err(RuntimeError::plain)?;
        self.call("play", json!({"action": "start"})).await.map_err(RuntimeError::plain)?;
        tokio::time::sleep(Duration::from_millis(SETTLE_MS)).await;
        let dry = self.loudest(&name, 1.5).await;
        println!("dry (Drift alone): the meter reads {dry:.3}");
        if dry < 0.05 {
            return Err(RuntimeError::plain("Drift alone plays nothing here, so nothing can be checked"));
        }
        let mut checks: Vec<(String, Option<Value>, Option<String>)> =
            effects(stamp as u128).into_iter().map(|(label, spec)| (label, Some(spec), None)).collect();
        for (at, file) in files.iter().enumerate() {
            match self.copy(Path::new(file), &format!("Sound Check {stamp} {}", at + 1)) {
                Ok(item) => checks.push((format!("file {file}"), None, Some(item))),
                Err(why) => println!("\nfile {file}: {why}"),
            }
        }
        let mut silent = 0;
        for (label, spec, item) in checks {
            println!("\n{label}");
            let item = match (spec, item) {
                (Some(spec), _) => match self.call("make_device", spec).await {
                    Ok(made) => {
                        for key in ["wrong", "changed", "notes"] {
                            if let Some(said) = made.get(key) {
                                println!("    make_device {key}: {said}");
                            }
                        }
                        if let Some(file) = made["file"].as_str() {
                            self.made.borrow_mut().push(PathBuf::from(file));
                        }
                        made["itemId"].as_str().unwrap_or_default().to_string()
                    }
                    Err(why) => {
                        println!("    make_device refused: {why}");
                        silent += 1;
                        continue;
                    }
                },
                (None, Some(item)) => item,
                (None, None) => continue,
            };
            if let Err(error) = self.load(&name, &item).await {
                println!("    {}", head(&error.message(), 300));
                silent += 1;
                continue;
            }
            tokio::time::sleep(Duration::from_millis(SETTLE_MS)).await;
            let level = self.loudest(&name, 1.5).await;
            let heard = level > dry * 0.1;
            if !heard {
                silent += 1;
            }
            println!("    {}  the meter reads {level:.3} (dry {dry:.3})", if heard { "ok  " } else { "SILENT" });
            println!("    devices: {}", self.devices(&name).await);
            self.undo_last().await;
            self.observe().await?;
        }
        self.call("play", json!({"action": "stop"})).await.map_err(RuntimeError::plain)?;
        println!("\n{silent} silent.");
        Ok(silent)
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
