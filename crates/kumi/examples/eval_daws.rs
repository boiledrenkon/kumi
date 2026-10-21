//! Opt-in evals of how the configured model works in a real REAPER or Renoise (M7's power-user scenarios). Each scenario
//! builds its fixture in the lab DAW's open project or song, gives Kumi the producer's words, and scores the state Kumi
//! leaves, read back through Kumi's bridge: the scenario's own checks ("Expect"), then the four every scenario has:
//!
//! - no preference changed (REAPER's setup and render settings; Renoise's editing settings), or put back;
//! - no extension called that the setup didn't show (SWS, ReaPack, ReaImGui, js_ReaScriptAPI in REAPER);
//! - the DAW's own words in what Kumi says and in HISTORY, never Live's;
//! - nothing generated unasked (no notes added where the scenario asks for none).
//!
//! It uses your sign-in and model (Kumi's own folder) and Kumi's host in process on the lab's kumi-home. It changes the
//! project or song it finds: run it only on the lab's scratch project or song, never a producer's.
//!
//! cargo run -p kumi --example eval_daws -- --daw reaper|renoise --kumi-home <lab kumi-home> [--only P30,R31]
//!     [--out <results.md>]
//!
//! EVAL_TRACE=1 prints each tool call and what it returned.
use async_trait::async_trait;
use daw_bridge::Daw;
use daw_mcp_server::host::Host;
use futures::FutureExt;
use kumi::config::{load_inference_config, safe_error};
use kumi_common::{
    abort::Signal,
    js::{json::stringify, string::head},
};
use kumi_runtime::{
    ai::{
        error::LanguageModelError,
        types::{CallOptions, StreamPart, StreamParts},
    },
    core::{context::IntegrationContext, contracts::WatchEvent, daw::DawProfile, memory::MemoryStoreOptions},
    create_agent_kernel, create_memory_store, create_session,
    integrations::{
        own_bridge::{Connect, OwnOptions},
        reaper::create_reaper_integration,
        renoise::create_renoise_integration,
    },
    kernel::agent::LanguageModel,
    mcp::{
        client::McpEndpoint,
        types::{CallToolResult, ContentBlock},
    },
    open_credential_store, resolve_model,
    system::process_env,
    AgentKernel, AgentKernelOptions, ChangeRecord, IntegrationFactory, Kernel, KernelEvent, KernelFactory, KernelOptions, ModelBinding,
    ResolveModelOptions, RuntimeError, Session, SessionController, SessionEvent, SessionOptions,
};
use regex::Regex;
use serde_json::{json, Value};
use std::{
    cell::{Cell, RefCell},
    io::Write,
    path::{Path, PathBuf},
    rc::{Rc, Weak},
    sync::Arc,
    time::Instant,
};

#[path = "../../kumi-runtime/tests/support/in_process.rs"]
mod in_process;
use in_process::InProcess;

// ---------------------------------------------------------------------------------------------------------------------
// Scenarios

struct Scenario {
    id: &'static str,
    daw: Daw,
    /// What the producer says, turn by turn.
    says: &'static [&'static str],
    /// Lua the fixture is built with, in the DAW (REAPER's `reaper`, Renoise's `song`), as one step of its undo.
    fixture: &'static str,
    /// Whether the producer asks for notes to be written (else none may be added).
    generates: bool,
    /// Names Kumi's answer should use, saying what it found or changed (any one of them).
    names: &'static [&'static str],
    /// The scenario's own checks, on the state before and after.
    expect: fn(&Value, &Value) -> Vec<Check>,
}

fn scenarios() -> Vec<Scenario> {
    vec![
        Scenario {
            id: "P30",
            daw: Daw::Reaper,
            says: &["The kick is way too loud and I can't find why"],
            // Kick sits in the Drums folder (to the master through it) and also sends to Drum Bus, which goes to the master:
            // it reaches the master twice.
            fixture: r#"
for i = reaper.CountTracks(0) - 1, 0, -1 do reaper.DeleteTrack(reaper.GetTrack(0, i)) end
local _, markers, regions = reaper.CountProjectMarkers(0)
for i = markers + regions - 1, 0, -1 do reaper.DeleteProjectMarkerByIndex(0, i) end
local function track(name)
  local at = reaper.CountTracks(0)
  reaper.InsertTrackAtIndex(at, true)
  local t = reaper.GetTrack(0, at)
  reaper.GetSetMediaTrackInfo_String(t, "P_NAME", name, true)
  return t
end
local drums, kick, snare, bus, bass = track("Drums"), track("Kick"), track("Snare"), track("Drum Bus"), track("Bass")
reaper.SetMediaTrackInfo_Value(drums, "I_FOLDERDEPTH", 1)
reaper.SetMediaTrackInfo_Value(snare, "I_FOLDERDEPTH", -1)
reaper.CreateTrackSend(kick, bus)
for _, t in ipairs({kick, snare, bass}) do reaper.CreateNewMIDIItemInProj(t, 0, 8, false) end
reaper.UpdateArrange()
return "ok"
"#,
            generates: false,
            names: &["Drum Bus", "send", "folder", "Drums"],
            expect: |before, after| {
                let mut checks = vec![];
                let paths = |state: &Value| reaper_paths(state, "Kick");
                checks.push(Check::new("the kick reached the master twice before", paths(before) == 2, format!("{} paths", paths(before))));
                checks.push(Check::new("one route to the master is left", paths(after) == 1, format!("{} paths", paths(after))));
                for name in ["Snare", "Bass", "Drum Bus", "Drums"] {
                    let same = reaper_track(before, name).map(|t| (&t["volume"], &t["mute"], &t["mainSend"]))
                        == reaper_track(after, name).map(|t| (&t["volume"], &t["mute"], &t["mainSend"]));
                    checks.push(Check::new(&format!("{name} left as it was"), same, String::new()));
                }
                let kick_volume = |state: &Value| reaper_track(state, "Kick").map(|t| t["volume"].clone());
                checks.push(Check::new(
                    "the kick's fader is left alone (the route is the cause)",
                    kick_volume(before) == kick_volume(after),
                    format!("{:?} → {:?}", kick_volume(before), kick_volume(after)),
                ));
                checks
            },
        },
        Scenario {
            id: "R31",
            daw: Daw::Renoise,
            says: &["Put an S40 on the second snare and 09C0 on line 20"],
            // Snare hits on lines 04, 12, 20 and 28 of the first pattern (LPB 4, 32 lines), on the song's first track.
            fixture: r#"
song.transport.lpb = 4
local t = song:track(1)
t.name = "Snare"
for i = 2, song.sequencer_track_count do
  if song:track(i).name == "Snare" then song:track(i).name = "Track " .. i end
end
local pattern = song:pattern(song.sequencer:pattern(1))
pattern.number_of_lines = 32
local pt = pattern:track(1)
pt:clear()
for _, line in ipairs({4, 12, 20, 28}) do
  local nc = pt:line(line + 1):note_column(1)
  nc.note_string = "C-4"
  nc.instrument_value = 0
end
return "ok"
"#,
            generates: false,
            names: &["Snare", "0S40", "0SC0", "line 12", "line 20"],
            expect: |before, after| {
                let mut checks = vec![];
                let offset = |state: &Value, line: u64| renoise_effects(state, line).iter().any(|fx| fx == "0S40");
                let offset_c0 = |state: &Value, line: u64| renoise_effects(state, line).iter().any(|fx| fx == "0SC0");
                checks.push(Check::new(
                    "0S40 on the second snare (line 12)",
                    offset(after, 12),
                    format!("{:?}", renoise_effects(after, 12)),
                ));
                checks.push(Check::new("0SC0 on line 20", offset_c0(after, 20), format!("{:?}", renoise_effects(after, 20))));
                let notes = |state: &Value| state["snare"]["notes"].clone();
                checks.push(Check::new("the snare's notes are as they were", notes(before) == notes(after), format!("{}", notes(after))));
                let others: Vec<u64> = (0..32)
                    .filter(|line| ![12, 20].contains(line) && renoise_effects(before, *line) != renoise_effects(after, *line))
                    .collect();
                checks.push(Check::new("no other line's effects changed", others.is_empty(), format!("{others:?}")));
                checks
            },
        },
    ]
}

/// A REAPER track of the state, by name.
fn reaper_track<'a>(state: &'a Value, name: &str) -> Option<&'a Value> {
    state["tracks"].as_array()?.iter().find(|track| track["name"] == name)
}

/// How many ways a track's sound reaches the master: through its folder (its parent send, up the folders) and through
/// each unmuted send whose destination reaches it.
fn reaper_paths(state: &Value, name: &str) -> usize {
    fn reach(state: &Value, name: &str, depth: usize) -> usize {
        let Some(track) = reaper_track(state, name) else { return 0 };
        if depth > 16 {
            return 0;
        }
        let up = if track["mainSend"] == true {
            match track["parent"].as_str() {
                Some(parent) => reach(state, parent, depth + 1),
                None => 1,
            }
        } else {
            0
        };
        let sends: usize = track["sends"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|send| send["mute"] != true && send["volume"].as_f64().unwrap_or(0.0) > 0.0)
            .map(|send| reach(state, send["to"].as_str().unwrap_or_default(), depth + 1))
            .sum();
        up + sends
    }
    reach(state, name, 0)
}

/// The effects on a line of the snare track (effect columns and the note column's own), as "0S40".
fn renoise_effects(state: &Value, line: u64) -> Vec<String> {
    state["snare"]["lines"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|row| row["line"] == line)
        .and_then(|row| row["effects"].as_array())
        .map(|effects| effects.iter().filter_map(|fx| fx.as_str().map(str::to_owned)).collect())
        .unwrap_or_default()
}

/// What the state readers return: REAPER's tracks (with their folder parent, sends and volume), its MIDI note count and
/// its preferences that the setup reads; Renoise's snare track's lines, its note count and its editing settings.
const REAPER_STATE: &str = r#"
local out, parents = {tracks = {}, notes = 0}, {}
local function name_of(t) local _, n = reaper.GetSetMediaTrackInfo_String(t, "P_NAME", "", false) return n end
for i = 0, reaper.CountTracks(0) - 1 do
  local t = reaper.GetTrack(0, i)
  local parent = reaper.GetParentTrack(t)
  local sends = {}
  for s = 0, reaper.GetTrackNumSends(t, 0) - 1 do
    local dest = reaper.GetTrackSendInfo_Value(t, 0, s, "P_DESTTRACK")
    sends[#sends + 1] = {to = name_of(dest), mute = reaper.GetTrackSendInfo_Value(t, 0, s, "B_MUTE") ~= 0,
      volume = reaper.GetTrackSendInfo_Value(t, 0, s, "D_VOL")}
  end
  out.tracks[#out.tracks + 1] = {name = name_of(t), parent = parent and name_of(parent) or nil,
    mainSend = reaper.GetMediaTrackInfo_Value(t, "B_MAINSEND") ~= 0, mute = reaper.GetMediaTrackInfo_Value(t, "B_MUTE") ~= 0,
    volume = math.floor(reaper.GetMediaTrackInfo_Value(t, "D_VOL") * 1e6 + 0.5) / 1e6, sends = sends}
  for k = 0, reaper.CountTrackMediaItems(t) - 1 do
    local take = reaper.GetActiveTake(reaper.GetTrackMediaItem(t, k))
    if take and reaper.TakeIsMIDI(take) then local _, n = reaper.MIDI_CountEvts(take) out.notes = out.notes + n end
  end
end
local prefs = {}
for _, name in ipairs({"deffadelen", "splitautoxfade", "loopnewitems", "deftrackvol", "itemtimelock", "tempoenvtimelock", "autoxfade"}) do
  local ok, value = reaper.get_config_var_string(name)
  prefs[name] = ok and value or ""
end
for _, key in ipairs({"RENDER_SETTINGS", "RENDER_BOUNDSFLAG", "RENDER_SRATE", "RENDER_CHANNELS", "RENDER_TAILFLAG"}) do
  prefs[key] = reaper.GetSetProjectInfo(0, key, 0, false)
end
for _, key in ipairs({"RENDER_FILE", "RENDER_PATTERN", "RENDER_FORMAT"}) do
  local _, value = reaper.GetSetProjectInfo_String(0, key, "", false)
  prefs[key] = value
end
out.prefs = prefs
return out
"#;

const RENOISE_STATE: &str = r#"
local out = {notes = 0, snare = {lines = {}, notes = {}}}
for p = 1, #song.patterns do
  for t = 1, song.sequencer_track_count do
    for _, line in ipairs(song:pattern(p):track(t).lines) do
      for c = 1, 12 do
        local nc = line:note_column(c)
        if nc.note_value < 120 then out.notes = out.notes + 1 end
      end
    end
  end
end
local snare
for t = 1, song.sequencer_track_count do if song:track(t).name == "Snare" then snare = t break end end
if snare then
  local pt = song:pattern(song.sequencer:pattern(1)):track(snare)
  for l = 1, song:pattern(song.sequencer:pattern(1)).number_of_lines do
    local line = pt:line(l)
    local effects = {}
    for c = 1, 8 do
      local fx = line:effect_column(c)
      if not fx.is_empty then effects[#effects + 1] = fx.number_string .. fx.amount_string end
    end
    for c = 1, 12 do
      local nc = line:note_column(c)
      if nc.effect_number_value ~= 0 or nc.effect_amount_value ~= 0 then
        effects[#effects + 1] = nc.effect_number_string .. nc.effect_amount_string
      end
      if nc.note_value < 120 then out.snare.notes[#out.snare.notes + 1] = (l - 1) .. " " .. nc.note_string end
    end
    if #effects > 0 then out.snare.lines[#out.snare.lines + 1] = {line = l - 1, effects = effects} end
  end
end
local tr = song.transport
out.prefs = {editStep = tr.edit_step, octave = tr.octave, follow = tr.follow_player, editMode = tr.edit_mode,
  quantize = tr.record_quantize_enabled, quantizeLines = tr.record_quantize_lines, wrap = tr.wrapped_pattern_edit,
  singleTrack = tr.single_track_edit_mode, metronome = tr.metronome_enabled, lpb = tr.lpb, bpm = tr.bpm}
return out
"#;

// ---------------------------------------------------------------------------------------------------------------------
// Checks and outcomes

struct Check {
    name: String,
    passed: bool,
    detail: String,
}

impl Check {
    fn new(name: &str, passed: bool, detail: String) -> Check {
        Check { name: name.to_owned(), passed, detail }
    }
}

struct Outcome {
    id: &'static str,
    checks: Vec<Check>,
    said: String,
    tools: Vec<String>,
    seconds: f64,
    error: Option<String>,
}

impl Outcome {
    fn passed(&self) -> bool {
        self.error.is_none() && self.checks.iter().all(|check| check.passed)
    }
}

/// Live's words, whole (as the app's own leak tests have them).
static LIVE: std::sync::LazyLock<Regex> = std::sync::LazyLock::new(|| {
    Regex::new(r"\b(Live|Ableton|Sets?|Session|Arrangement|Main|[Ss]cenes?|[Rr]acks?|[Cc]lip slots?|Control Surfaces?|Remote Scripts?|Max for Live|M4L|Operator)\b").unwrap()
});

/// What every scenario also checks.
fn every_scenario(scenario: &Scenario, run: &Run) -> Vec<Check> {
    let mut checks = vec![];
    // No preference changed (or each put back).
    let (before, after) = (&run.before["prefs"], &run.after["prefs"]);
    let changed: Vec<String> = before
        .as_object()
        .into_iter()
        .flatten()
        .filter(|(key, value)| after.get(key.as_str()) != Some(value))
        .map(|(key, value)| format!("{key}: {value} → {}", after.get(key.as_str()).unwrap_or(&Value::Null)))
        .collect();
    checks.push(Check::new("no preference changed", changed.is_empty(), changed.join("; ")));
    // No extension called that the setup didn't show.
    let extensions = Regex::new(r"\b(CF|BR|SNM|NF|JS|ImGui|ReaPack|ULT|SWS)_\w+|_SWS_\w+|_BR_\w+").unwrap();
    let present = |name: &str| run.setup.pointer(&format!("/extensions/{name}")) == Some(&Value::Bool(true));
    let unchecked: Vec<String> = run
        .calls
        .iter()
        .filter_map(|call| extensions.find(call).map(|found| (call, found.as_str().to_owned())))
        .filter(|(call, found)| {
            let which = match found.trim_start_matches('_') {
                name if name.starts_with("ImGui") => "ReaImGui",
                name if name.starts_with("ReaPack") => "ReaPack",
                name if name.starts_with("JS") => "js_ReaScriptAPI",
                _ => "SWS",
            };
            !present(which) && !call.contains("APIExists")
        })
        .map(|(_, found)| found)
        .collect();
    checks.push(Check::new("no extension called unchecked", unchecked.is_empty(), unchecked.join(", ")));
    // The DAW's own words in what Kumi said and in HISTORY.
    let heard: Vec<String> = std::iter::once(run.said.as_str())
        .chain(run.changes.iter().map(|change| change.title.as_str()))
        .flat_map(|text| LIVE.find_iter(text).map(|m| m.as_str().to_owned()).collect::<Vec<_>>())
        .collect();
    checks.push(Check::new("the DAW's own words, never Live's", heard.is_empty(), heard.join(", ")));
    let named = scenario.names.iter().any(|name| run.said.to_lowercase().contains(&name.to_lowercase()));
    checks.push(Check::new("says what it found or changed, by name", named, head(&run.said.replace('\n', " "), 160)));
    // Nothing generated unasked.
    let notes = |state: &Value| state["notes"].as_u64().unwrap_or(0);
    if !scenario.generates {
        checks.push(Check::new(
            "no notes added unasked",
            notes(&run.after) <= notes(&run.before),
            format!("{} → {}", notes(&run.before), notes(&run.after)),
        ));
    }
    checks
}

/// What one scenario's session left to score.
struct Run {
    before: Value,
    after: Value,
    setup: Value,
    said: String,
    calls: Vec<String>,
    changes: Vec<ChangeRecord>,
}

// ---------------------------------------------------------------------------------------------------------------------
// The host, the DAW's Lua, the model

fn text_of(result: &CallToolResult) -> String {
    result
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

async fn host_call(host: &InProcess, name: &str, args: Value) -> Result<Value, String> {
    let result = host.call(name, args.as_object().cloned().unwrap_or_default(), Signal::new()).await.map_err(|e| format!("{e:?}"))?;
    let text = text_of(&result);
    let value: Value = serde_json::from_str(&text).unwrap_or(Value::String(text.clone()));
    if result.is_error == Some(true) {
        return Err(value.get("error").and_then(Value::as_str).map(str::to_owned).unwrap_or(text));
    }
    Ok(value)
}

/// Lua run in the DAW through the host as a run-lua change that no session keeps (no HISTORY row), and what it returned.
async fn lua(host: &InProcess, code: &str) -> Result<Value, String> {
    thread_local!(static NEXT: Cell<u64> = const { Cell::new(0) });
    let project = host_call(host, "snapshot", json!({})).await?;
    let preview =
        host_call(host, "change_preview", json!({"kind": "run-lua", "input": {"code": code, "timeoutMs": 800}, "epoch": project["epoch"]}))
            .await?;
    let (Some(id), Some(confirmation)) = (preview["transactionId"].as_str(), preview["confirmation"].as_str()) else {
        return Err(format!("no transaction: {preview}"));
    };
    let key = NEXT.with(|next| {
        next.set(next.get() + 1);
        format!("eval-{}-{}-{}", std::process::id(), kumi_common::time::now_ms(), next.get())
    });
    let applied =
        host_call(host, "change_apply", json!({"transactionId": id, "confirmation": confirmation, "idempotencyKey": key})).await?;
    host_call(host, "change_release", json!({"transactionIds": [id]})).await.ok();
    if let Some(failed) = applied.pointer("/applied/failed") {
        return Err(format!("Lua: {}", failed["message"].as_str().unwrap_or_default()));
    }
    Ok(applied.pointer("/applied/result").cloned().unwrap_or(Value::Null))
}

/// The configured model, shared by each scenario's kernels.
struct Shared {
    model: Rc<dyn LanguageModel>,
    calls: Cell<usize>,
    tokens: Rc<Cell<(f64, f64, f64)>>,
}

#[async_trait(?Send)]
impl LanguageModel for Shared {
    async fn do_stream(&self, options: CallOptions) -> Result<StreamParts, LanguageModelError> {
        self.calls.set(self.calls.get() + 1);
        let parts = self.model.do_stream(options).await?;
        let tokens = self.tokens.clone();
        Ok(Box::pin(futures::StreamExt::inspect(parts, move |part| {
            if let StreamPart::Finish { usage, .. } = part {
                let (input, cached, output) = tokens.get();
                tokens.set((
                    input + usage.input_tokens.total.unwrap_or(0.),
                    cached + usage.input_tokens.cache_read.unwrap_or(0.),
                    output + usage.output_tokens.total.unwrap_or(0.),
                ));
            }
        })))
    }
}

struct Model {
    binding: Rc<ModelBinding>,
    shared: Rc<Shared>,
}

impl Model {
    fn binding(&self) -> ModelBinding {
        let prepare = self.binding.clone();
        let budget = self.binding.clone();
        ModelBinding {
            id: self.binding.id.clone(),
            model: self.shared.clone(),
            prepare: Box::new(move |request| (prepare.prepare)(request)),
            budget: self.binding.budget.as_ref().map(|_| Box::new(move |fixed| (budget.budget.as_ref().unwrap())(fixed)) as Box<_>),
        }
    }
}

// ---------------------------------------------------------------------------------------------------------------------
// One scenario

async fn run_scenario(model: &Model, scenario: &Scenario, home: &Path) -> Result<Outcome, RuntimeError> {
    let started = Instant::now();
    let failure = |e: String| RuntimeError::plain(e);
    let host = Rc::new(InProcess { host: Arc::new(Host::new(scenario.daw, home)), id: Cell::new(0) });
    let status = host_call(&host, "status", json!({})).await.map_err(failure)?;
    if status.get("connected") != Some(&Value::Bool(true)) {
        return Err(RuntimeError::plain(format!("the lab DAW isn't connected: {status}")));
    }
    let reader = match scenario.daw {
        Daw::Reaper => REAPER_STATE,
        Daw::Renoise => RENOISE_STATE,
    };
    lua(&host, scenario.fixture).await.map_err(|e| failure(format!("fixture: {e}")))?;
    let before = lua(&host, reader).await.map_err(|e| failure(format!("state before: {e}")))?;
    let setup = host_call(&host, "snapshot", json!({})).await.map_err(failure)?["setup"].clone();

    let changes = Rc::new(RefCell::new(Vec::<ChangeRecord>::new()));
    let said = Rc::new(RefCell::new(String::new()));
    let tools = Rc::new(RefCell::new(Vec::<String>::new()));
    let kernel = Rc::new(RefCell::new(None::<Rc<AgentKernel>>));
    let controller = Rc::new(RefCell::new(None::<Weak<Session>>));
    let folder = tempfile::Builder::new().prefix("kumi-eval-daws-").tempdir().map_err(|e| failure(e.to_string()))?;
    let kernel_factory: KernelFactory = {
        let kernel = kernel.clone();
        let model = Model { binding: model.binding.clone(), shared: model.shared.clone() };
        Rc::new(move |options: KernelOptions| {
            let made = create_agent_kernel(AgentKernelOptions {
                conversation: None,
                instructions: options.instructions,
                tools: options.tools,
                signal: options.signal,
                checkpoint: options.checkpoint,
                binding: model.binding(),
                max_steps: None,
                budget: None,
            })
            .map(Rc::new);
            if let Ok(made) = &made {
                *kernel.borrow_mut() = Some(made.clone());
            }
            async move { made.map(|made| made as Rc<dyn Kernel>) }.boxed_local()
        })
    };
    let integration_factory: IntegrationFactory = {
        let (host, changes, controller, home, daw) = (host.clone(), changes.clone(), controller.clone(), home.to_path_buf(), scenario.daw);
        Box::new(move |on_connection| {
            let endpoint = host.clone();
            let connect: Connect = Rc::new(move |_| {
                let endpoint = endpoint.clone();
                async move { Ok(endpoint as Rc<dyn McpEndpoint>) }.boxed_local()
            });
            let mut context = IntegrationContext::new(on_connection);
            let (changes, controller) = (changes.clone(), controller.clone());
            context.on_change = Some(Rc::new(move |change: ChangeRecord| {
                {
                    let mut changes = changes.borrow_mut();
                    match changes.iter_mut().find(|kept| kept.id == change.id) {
                        Some(kept) => *kept = change.clone(),
                        None => changes.push(change.clone()),
                    }
                }
                if let Some(session) = controller.borrow().as_ref().and_then(Weak::upgrade) {
                    session.watch(WatchEvent::Change(change));
                }
            }));
            let options = OwnOptions { context, kumi_home: home.clone(), connect: Some(connect) };
            match daw {
                Daw::Reaper => create_reaper_integration(options),
                Daw::Renoise => create_renoise_integration(options),
            }
        })
    };
    let on_event: Rc<dyn Fn(SessionEvent)> = {
        let (said, tools) = (said.clone(), tools.clone());
        Rc::new(move |event: SessionEvent| match event {
            SessionEvent::Kernel(KernelEvent::ToolStart { name, .. }) => tools.borrow_mut().push(name),
            SessionEvent::Kernel(KernelEvent::Text { text }) => said.borrow_mut().push_str(&text),
            _ => {}
        })
    };
    let mut options = SessionOptions::new(kernel_factory, integration_factory, on_event);
    options.profile = match scenario.daw {
        Daw::Reaper => DawProfile::REAPER,
        Daw::Renoise => DawProfile::RENOISE,
    };
    options.timeout_ms = Some(300_000);
    options.listen = true;
    options.memory = Some(create_memory_store(MemoryStoreOptions {
        projects_dir: folder.path().join("projects"),
        producer_file: folder.path().join("memory.json"),
    }));
    let session = Rc::new(create_session(options)?);
    *controller.borrow_mut() = Some(Rc::downgrade(&session));
    let conversation = async {
        session.start().await?;
        for words in scenario.says {
            session.submit(words, None).await?;
        }
        let held = kernel.borrow().clone();
        Ok::<_, RuntimeError>(match held {
            Some(kernel) => Kernel::checkpoint(kernel.as_ref())?.messages,
            None => vec![],
        })
    }
    .await;
    let closed = session.close().await;
    let messages = conversation?;
    closed?;
    // Each tool call's input, as text (a plan's steps inside it), for the extension check.
    let mut calls = vec![];
    for part in messages.iter().filter_map(|message| message.get("content").and_then(Value::as_array)).flatten() {
        let either = |first: &str, second: &str| part.get(first).filter(|value| !value.is_null()).or(part.get(second)).cloned();
        match part.get("type").and_then(Value::as_str) {
            Some("tool-call") => {
                let input = stringify(&either("input", "args").unwrap_or(Value::Null));
                if std::env::var_os("EVAL_TRACE").is_some_and(|trace| !trace.is_empty()) {
                    println!("   → {} {}", part["toolName"].as_str().unwrap_or_default(), head(&input, 700));
                }
                calls.push(input);
            }
            Some("tool-result") if std::env::var_os("EVAL_TRACE").is_some_and(|trace| !trace.is_empty()) => {
                println!("   ← {}", head(&stringify(&either("output", "result").unwrap_or(Value::Null)), 500));
            }
            _ => {}
        }
    }
    let after = lua(&host, reader).await.map_err(|e| failure(format!("state after: {e}")))?;
    let run = Run { before, after, setup, said: said.borrow().clone(), calls, changes: changes.borrow().clone() };
    let mut checks = (scenario.expect)(&run.before, &run.after);
    checks.extend(every_scenario(scenario, &run));
    folder.close().ok();
    let tools = tools.borrow().clone();
    Ok(Outcome { id: scenario.id, checks, said: run.said, tools, seconds: started.elapsed().as_secs_f64(), error: None })
}

// ---------------------------------------------------------------------------------------------------------------------
// The run

fn report(outcome: &Outcome) -> String {
    let mut text = format!("## {} — {} ({:.0} s)\n\n", outcome.id, if outcome.passed() { "passed" } else { "failed" }, outcome.seconds);
    if let Some(error) = &outcome.error {
        text.push_str(&format!("- error: {error}\n"));
    }
    for check in &outcome.checks {
        text.push_str(&format!(
            "- {} {}{}\n",
            if check.passed { "✓" } else { "✗" },
            check.name,
            if check.detail.is_empty() { String::new() } else { format!(": {}", check.detail) }
        ));
    }
    text.push_str(&format!("- tools: {}\n- said: {}\n\n", outcome.tools.join(", "), head(&outcome.said.replace('\n', " "), 600)));
    text
}

async fn run() -> Result<i32, RuntimeError> {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let flag = |name: &str| argv.iter().position(|a| a == name).and_then(|k| argv.get(k + 1)).cloned();
    let usage = "eval_daws --daw reaper|renoise --kumi-home <lab kumi-home> [--only P30,R31] [--out <results.md>]";
    let daw = match flag("--daw").as_deref() {
        Some("reaper") => Daw::Reaper,
        Some("renoise") => Daw::Renoise,
        _ => return Err(RuntimeError::plain(usage)),
    };
    let home = PathBuf::from(flag("--kumi-home").ok_or_else(|| RuntimeError::plain(usage))?);
    let only: Vec<String> = flag("--only").map(|list| list.split(',').map(str::to_owned).collect()).unwrap_or_default();
    let env = process_env();
    let config = load_inference_config(&env)?;
    let binding = resolve_model(ResolveModelOptions {
        model: config.model.clone().unwrap_or_default(),
        store: Rc::new(open_credential_store(&config.auth_file)),
        env: Some(env.clone()),
        fetch: None,
        effort: None,
        service_tier: None,
    })
    .await?;
    let shared = Rc::new(Shared { model: binding.model.clone(), calls: Cell::new(0), tokens: Rc::new(Cell::new((0., 0., 0.))) });
    let model = Model { binding: Rc::new(binding), shared };
    let mut outcomes = vec![];
    let mut text = format!("# eval_daws: {} with {}\n\n", daw.name(), config.model.as_deref().unwrap_or("undefined"));
    for scenario in scenarios().iter().filter(|s| s.daw == daw && (only.is_empty() || only.iter().any(|o| o == s.id))) {
        println!("== {} {}", scenario.id, scenario.says.join(" / "));
        let outcome = match run_scenario(&model, scenario, &home).await {
            Ok(outcome) => outcome,
            Err(error) => Outcome {
                id: scenario.id,
                checks: vec![],
                said: String::new(),
                tools: vec![],
                seconds: 0.0,
                error: Some(safe_error(Some(&error), &[])),
            },
        };
        let part = report(&outcome);
        print!("{part}");
        text.push_str(&part);
        outcomes.push(outcome);
    }
    let passed = outcomes.iter().filter(|outcome| outcome.passed()).count();
    let (input, cached, output) = model.shared.tokens.get();
    let summary = format!(
        "{passed} of {} passed; {} model calls; tokens {input} in ({cached} cached), {output} out.\n",
        outcomes.len(),
        model.shared.calls.get()
    );
    print!("{summary}");
    text.push_str(&summary);
    if let Some(out) = flag("--out") {
        std::fs::write(&out, text).map_err(|e| RuntimeError::plain(e.to_string()))?;
    }
    Ok(if passed == outcomes.len() { 0 } else { 1 })
}

fn main() {
    let runtime = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("eval_daws: {error}");
            std::process::exit(1);
        }
    };
    let code = tokio::task::LocalSet::new().block_on(&runtime, async {
        match run().await {
            Ok(code) => code,
            Err(error) => {
                eprintln!("eval_daws: {}", safe_error(Some(&error), &[]));
                1
            }
        }
    });
    let _ = std::io::stdout().flush();
    std::process::exit(code);
}
