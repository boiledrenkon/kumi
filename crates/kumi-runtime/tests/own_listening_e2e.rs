//! Listening in REAPER and Renoise end to end: Kumi's integration and the shared listening tools over Kumi's host (in
//! process) in front of a bridge that renders, captures and plays like REAPER's extension or Renoise's tool. A part is
//! rendered offline and the main thing heard played out loud after; REAPER hears what it plays while it plays; a listen
//! stopped part way stops its render; the demo and an older bridge offer no listening. Nothing said is in Live's words,
//! and nothing says "clip".
use daw_bridge::{files::ensure_secret, paths::BridgePaths, Daw};
use daw_mcp_server::{
    host::Host,
    simulator::{reaper::ReaperScript, renoise::RenoiseScript, Simulator, SimulatorConfig},
};
use futures::FutureExt;
use kumi_common::abort::Signal;
use kumi_runtime::core::{context::IntegrationContext, contracts::*, errors::RuntimeError};
use kumi_runtime::integrations::{
    own_bridge::{Connect, OwnOptions},
    reaper::create_reaper_integration,
    renoise::create_renoise_integration,
};
use kumi_runtime::mcp::client::McpEndpoint;
use regex::Regex;
use serde_json::{json, Value};
use std::{cell::Cell, cell::RefCell, path::Path, rc::Rc, sync::Arc, sync::LazyLock};

#[path = "support/in_process.rs"]
mod in_process;
use in_process::InProcess;

/// Live's words, whole, and a clip: none may reach the producer or the model over REAPER or Renoise.
static NOT_OURS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\b(Live|Set|Sets|Ableton|Session|Arrangement|Remote Script|Control Surface|Main|Drum Rack|Operator|EQ Eight|Utility|[Rr]acks?|[Cc]lips?|[Ss]cenes?)\b")
        .unwrap()
});

fn assert_ours(said: &[String]) {
    let found: Vec<String> = said
        .iter()
        .flat_map(|text| {
            NOT_OURS.find_iter(text).map(|word| format!("{:?} in {}", word.as_str(), &text[..text.len().min(400)])).collect::<Vec<_>>()
        })
        .collect();
    assert!(found.is_empty(), "{}", found.join("\n"));
}

fn object(value: Value) -> JsonObject {
    value.as_object().cloned().unwrap()
}

/// An integration over Kumi's host in front of `config`'s simulated bridge, with what it tells the app.
fn integration(daw: Daw, home: &Path, create: fn(OwnOptions) -> Rc<dyn Integration>) -> (Rc<dyn Integration>, Rc<RefCell<Vec<String>>>) {
    let endpoint = Rc::new(InProcess { host: Arc::new(Host::new(daw, home)), id: Cell::new(0) });
    let connect: Connect = Rc::new(move |_| {
        let endpoint = endpoint.clone();
        async move { Ok(endpoint as Rc<dyn McpEndpoint>) }.boxed_local()
    });
    let told: Rc<RefCell<Vec<String>>> = Rc::default();
    let into = told.clone();
    let mut context = IntegrationContext::new(Rc::new(|_, _| {}));
    context.on_action = Some(Rc::new(move |event: ActionEvent| into.borrow_mut().push(event.title)));
    context.focus_interval_ms = Some(50);
    (create(OwnOptions { context, kumi_home: home.into(), connect: Some(connect) }), told)
}

async fn reaper_lab(script: &ReaperScript, home: &Path, keep: impl FnOnce(&mut SimulatorConfig)) -> Simulator {
    let paths = BridgePaths::new(home, Daw::Reaper);
    let secret = ensure_secret(&paths.secret()).unwrap();
    script.listening().set_folder(&paths.root.join("listening"));
    let mut config = script.config(secret, "/lab/REAPER");
    config.discovery_dir = Some(paths.discovery());
    keep(&mut config);
    Simulator::start(config).await.unwrap()
}

async fn renoise_lab(script: &RenoiseScript, home: &Path) -> Simulator {
    let paths = BridgePaths::new(home, Daw::Renoise);
    let secret = ensure_secret(&paths.secret()).unwrap();
    script.listening().set_folder(&paths.root.join("listening"));
    let mut config = script.config(secret, "/lab/Renoise");
    config.discovery_dir = Some(paths.discovery());
    Simulator::start(config).await.unwrap()
}

fn tool(observed: &Observation, name: &str) -> Rc<dyn KernelTool> {
    observed.tools.iter().find(|tool| tool.name() == name).cloned().unwrap_or_else(|| panic!("no {name} tool"))
}

/// The tools' descriptions and schemas, as the model reads them.
fn described(observed: &Observation) -> Vec<String> {
    observed
        .tools
        .iter()
        .map(|tool| json!({"name": tool.name(), "description": tool.description(), "inputSchema": tool.input_schema()}).to_string())
        .collect()
}

#[tokio::test(flavor = "current_thread")]
async fn reaper_renders_a_part_to_hear_it_and_plays_what_it_heard_out_loud() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let home = tempfile::tempdir().unwrap();
            let script = ReaperScript::new(3, 0);
            let simulator = reaper_lab(&script, home.path(), |_| {}).await;
            let (reaper, told) = integration(Daw::Reaper, home.path(), create_reaper_integration);
            reaper.start(Signal::new()).await.unwrap();
            let observed = reaper.observe(Signal::new(), None).await.unwrap();
            let names: Vec<&str> = observed.tools.iter().map(|tool| tool.name()).collect();
            for listening in ["audition", "judge", "tune", "sound", "form", "render"] {
                assert!(names.contains(&listening), "{listening} offered: {names:?}");
            }
            assert!(reaper.has_hear() && reaper.has_audition() && reaper.has_audio_file());
            let context: Value = serde_json::from_str(&observed.context).unwrap();
            let track = |at: usize| context["tracks"][at]["ref"].as_str().unwrap().to_owned();
            let mut said = described(&observed);

            // An audition: both candidates rendered offline in one job, before their faders, the first played out loud.
            let audition = tool(&observed, "audition")
                .execute(
                    object(json!({"candidates": [{"track": track(2), "label": "warm"}, {"track": track(3), "label": "bright"}], "from_beat": 16, "beats": 8})),
                    Signal::new(),
                )
                .await
                .unwrap();
            assert!(!audition.is_error, "{}", audition.text);
            let reply: Value = serde_json::from_str(&audition.text).unwrap();
            assert_eq!(reply["takes"].as_array().unwrap().len(), 2, "{reply}");
            assert!(reply["takes"].as_array().unwrap().iter().all(|take| take["heard"].is_string()), "{reply}");
            assert!(!audition.text.contains("didn't hear this round"), "the first candidate was played out loud: {}", audition.text);
            let asked = script.listening().renders();
            assert_eq!(asked[0]["range"], json!({"fromBeat": 16.0, "beats": 8.0}));
            assert_eq!(asked[0]["preRollBeats"], 4.0, "a bar before the part, so effects are warm");
            let stages: Vec<&Value> = asked[0]["sources"].as_array().unwrap().iter().map(|source| &source["stage"]).collect();
            assert_eq!(stages, [&json!("pre-fader"), &json!("pre-fader")]);
            let played = script.listening().previews();
            assert_eq!(played.len(), 1, "the first candidate, out loud");
            assert_eq!(played[0]["fromSeconds"], 2.0, "from where the part starts, past the pre-roll");
            said.push(audition.text);

            // A judged run's first listen: the mix, rendered and played out loud.
            let judge = tool(&observed, "judge")
                .execute(object(json!({"goal": {"loudness": -14}, "from_beat": 0, "beats": 8})), Signal::new())
                .await
                .unwrap();
            assert!(!judge.is_error, "{}", judge.text);
            let mix = script.listening().renders().last().cloned().unwrap();
            assert!(mix["sources"].as_array().unwrap().iter().any(|source| source["what"] == "master"), "{mix}");
            said.push(judge.text);

            // The listen tool's hearing of the project: rendered, played out loud, said so.
            let heard = reaper.hear(&HearRequest { mix: Some(true), from_beat: Some(0.), beats: Some(8.), ..Default::default() }, Signal::new()).await.unwrap().unwrap();
            assert!(!heard[0].live && Path::new(&heard[0].file).is_file(), "{heard:?}");
            assert!(heard[0].note.as_deref().is_some_and(|note| note.contains("rendered it offline")), "{heard:?}");

            // While REAPER plays: what it plays, heard live (the master output); tracks on their own are rendered instead.
            script.set_playing(true);
            let live = reaper.hear(&HearRequest { mix: Some(true), seconds: Some(4.), ..Default::default() }, Signal::new()).await.unwrap().unwrap();
            assert!(live[0].live, "{live:?}");
            let tracks = reaper.hear(&HearRequest { tracks: vec![track(2)], ..Default::default() }, Signal::new()).await.unwrap();
            assert!(tracks.as_ref().is_err_and(|why| why.contains("give from_beat and beats")), "{tracks:?}");
            script.set_playing(false);

            // The render tool: a track's own items before its FX.
            let render = tool(&observed, "render").execute(object(json!({"track": track(1), "from_beat": 0, "beats": 4})), Signal::new()).await.unwrap();
            assert!(!render.is_error, "{}", render.text);
            assert!(render.text.contains("The track's own items, before its devices"), "{}", render.text);
            assert_eq!(script.listening().renders().last().unwrap()["sources"][0]["stage"], "items");
            said.push(render.text);

            let told = told.borrow().clone();
            assert!(told.iter().any(|title| title.starts_with("Rendering bars")), "{told:?}");
            assert!(told.iter().any(|title| title.starts_with("Playing bars") && title.ends_with("out loud")), "{told:?}");
            assert!(!told.iter().any(|title| title.starts_with("Playing bars") && title.contains(" · ")), "no part is said to play while it renders: {told:?}");
            said.extend(told);
            said.extend(heard.iter().chain(live.iter()).filter_map(|take| take.note.clone()));
            assert_ours(&said);
            reaper.close().await.unwrap();
            simulator.stop();
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn a_listen_stopped_part_way_stops_its_render_and_reaper_is_put_back() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let home = tempfile::tempdir().unwrap();
            let script = ReaperScript::new(3, 0);
            script.listening().set_steps(1_000);
            let simulator = reaper_lab(&script, home.path(), |_| {}).await;
            let (reaper, _) = integration(Daw::Reaper, home.path(), create_reaper_integration);
            reaper.start(Signal::new()).await.unwrap();
            reaper.observe(Signal::new(), None).await.unwrap();
            let signal = Signal::new();
            let stop = signal.clone();
            tokio::task::spawn_local(async move {
                tokio::time::sleep(std::time::Duration::from_millis(400)).await;
                stop.cancel();
            });
            let heard =
                reaper.hear(&HearRequest { mix: Some(true), from_beat: Some(0.), beats: Some(8.), ..Default::default() }, signal).await;
            assert!(matches!(heard, Err(RuntimeError::Aborted)), "{heard:?}");
            assert_eq!(script.listening().restored(), 1, "the render was stopped and what it changed put back");
            // A render that can't stop at once (it says it's stopping) is waited for, so the next listen finds the DAW free.
            script.listening().set_slow_stop(true);
            let signal = Signal::new();
            let stop = signal.clone();
            tokio::task::spawn_local(async move {
                tokio::time::sleep(std::time::Duration::from_millis(400)).await;
                stop.cancel();
            });
            let heard =
                reaper.hear(&HearRequest { mix: Some(true), from_beat: Some(0.), beats: Some(8.), ..Default::default() }, signal).await;
            assert!(matches!(heard, Err(RuntimeError::Aborted)), "{heard:?}");
            assert_eq!(script.listening().restored(), 2, "the stop waited for the render's end");
            script.listening().set_steps(1);
            let next = reaper
                .hear(&HearRequest { mix: Some(true), from_beat: Some(0.), beats: Some(8.), ..Default::default() }, Signal::new())
                .await
                .unwrap();
            assert!(next.is_ok(), "the next listen isn't refused as busy: {next:?}");
            reaper.close().await.unwrap();
            simulator.stop();
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn renoise_renders_each_track_through_its_master_and_offers_no_render_of_a_tracks_own_audio() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let home = tempfile::tempdir().unwrap();
            let script = RenoiseScript::new();
            let simulator = renoise_lab(&script, home.path()).await;
            let (renoise, _) = integration(Daw::Renoise, home.path(), create_renoise_integration);
            renoise.start(Signal::new()).await.unwrap();
            let observed = renoise.observe(Signal::new(), None).await.unwrap();
            let names: Vec<&str> = observed.tools.iter().map(|tool| tool.name()).collect();
            assert!(names.contains(&"audition") && names.contains(&"judge") && !names.contains(&"render"), "{names:?}");
            let context: Value = serde_json::from_str(&observed.context).unwrap();
            let first = context["tracks"][0]["ref"].as_str().unwrap().to_owned();
            let audition = tool(&observed, "audition")
                .execute(object(json!({"candidates": [{"track": first, "label": "kick"}], "from_beat": 0, "beats": 4})), Signal::new())
                .await
                .unwrap();
            assert!(!audition.is_error, "{}", audition.text);
            assert_eq!(script.listening().previews().len(), 1, "played out loud");
            let mut said = described(&observed);
            said.push(audition.text);
            assert_ours(&said);
            renoise.close().await.unwrap();
            simulator.stop();
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn the_renoise_demo_and_an_older_bridge_offer_no_listening_and_say_why() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let home = tempfile::tempdir().unwrap();
            let script = RenoiseScript::new();
            script.listening().set_demo(true);
            let simulator = renoise_lab(&script, home.path()).await;
            let (renoise, _) = integration(Daw::Renoise, home.path(), create_renoise_integration);
            renoise.start(Signal::new()).await.unwrap();
            let observed = renoise.observe(Signal::new(), None).await.unwrap();
            assert!(!observed.tools.iter().any(|tool| tool.name() == "audition"));
            assert!(!renoise.has_hear());
            let refused =
                renoise.hear(&HearRequest { mix: Some(true), from_beat: Some(0.), ..Default::default() }, Signal::new()).await.unwrap();
            assert!(refused.is_err_and(|why| why.starts_with("The Renoise demo can't render")));
            renoise.close().await.unwrap();
            simulator.stop();

            let home = tempfile::tempdir().unwrap();
            let script = ReaperScript::new(2, 0);
            // (a bridge from before listening: its status says nothing of it)
            let simulator = reaper_lab(&script, home.path(), |config| config.status_more = None).await;
            let (reaper, _) = integration(Daw::Reaper, home.path(), create_reaper_integration);
            reaper.start(Signal::new()).await.unwrap();
            let observed = reaper.observe(Signal::new(), None).await.unwrap();
            assert!(!observed.tools.iter().any(|tool| tool.name() == "audition"));
            let refused =
                reaper.hear(&HearRequest { mix: Some(true), from_beat: Some(0.), ..Default::default() }, Signal::new()).await.unwrap();
            assert!(refused.is_err_and(|why| why.contains("update Kumi's bridge")));
            reaper.close().await.unwrap();
            simulator.stop();
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn a_judged_round_brings_reapers_level_back_with_a_volume_fx_it_adds_on_the_master() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let home = tempfile::tempdir().unwrap();
            let script = ReaperScript::new(3, 0);
            let simulator = reaper_lab(&script, home.path(), |_| {}).await;
            let (reaper, _) = integration(Daw::Reaper, home.path(), create_reaper_integration);
            reaper.start(Signal::new()).await.unwrap();
            let observed = reaper.observe(Signal::new(), None).await.unwrap();
            let judge = tool(&observed, "judge");
            let start = judge
                .execute(object(json!({"goal": {"loudness": -6, "problems": false}, "from_beat": 0, "beats": 8})), Signal::new())
                .await
                .unwrap();
            assert!(!start.is_error, "{}", start.text);
            // The model changes something that makes the mix louder, toward the goal: kept, and the rest of the way is the
            // gain stage's (a gain FX added last on the master, and turned up).
            script.listening().set_master_level(0.4);
            let context: Value = serde_json::from_str(&observed.context).unwrap();
            let bass = context["tracks"][2]["ref"].as_str().unwrap().to_owned();
            let change =
                tool(&observed, "track_volume").execute(object(json!({"ref": bass, "volumeDb": -3})), Signal::new()).await.unwrap();
            assert!(!change.is_error, "{}", change.text);
            let round = judge.execute(object(json!({"change": "Bass down 3 dB"})), Signal::new()).await.unwrap();
            assert!(!round.is_error, "{}", round.text);
            assert_eq!(script.fx_names("r:track:master"), ["JS: Volume Adjustment"], "the gain FX rebalancing added: {}", round.text);
            let adjusted = script.fx_param("r:track:master", "Volume Adjustment", "Adjustment (dB)").unwrap();
            assert!(adjusted > 0.5, "turned up toward −6 LUFS: {adjusted}");
            assert!(round.text.contains("\"kept\":true"), "{}", round.text);
            assert_ours(&[round.text]);
            reaper.close().await.unwrap();
            simulator.stop();
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn a_goal_searches_scratch_copies_of_its_candidates_and_settles_the_winner_on_the_track() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let home = tempfile::tempdir().unwrap();
            let script = ReaperScript::new(3, 0);
            script.add_fx("r:track:{T0002}", "ReaEQ");
            let simulator = reaper_lab(&script, home.path(), |_| {}).await;
            let (reaper, told) = integration(Daw::Reaper, home.path(), create_reaper_integration);
            reaper.start(Signal::new()).await.unwrap();
            reaper.observe(Signal::new(), None).await.unwrap();
            assert!(reaper.has_goal());
            let reference = home.path().join("reference.wav");
            daw_mcp_server::simulator::listening::write_noise(&reference, 4., 0.4, 9).unwrap();
            let request = AuditionRequest {
                candidates: vec![AuditionCandidate { track: "Track 2".into(), clip: None, label: Some("pad".into()), mix: None }],
                from_beat: Some(0.),
                beats: Some(4.),
                reference: Some(reference.to_string_lossy().into_owned()),
                reference_from: None,
                reference_seconds: None,
                focus: None,
            };
            let rig = reaper.goal(&request, Signal::new()).await.unwrap().unwrap();
            assert!(!rig.screens(), "two seconds: heard whole");
            let slots = rig.slots();
            assert_eq!(slots.len(), 1);
            assert_eq!(script.fx_names("r:track:{T0002}"), ["VST: ReaEQ (Cockos)", "VST: ReaLimit (Cockos)"], "a safety limiter last");
            assert!(slots[0].chain.contains("ReaEQ") && slots[0].knobs.iter().any(|knob| knob.name == "Gain-Band 2"), "{:?}", slots[0]);
            // A generation: the trial's values on the slot's copy, its copy rendered and scored.
            let knobs: Vec<_> = slots[0].knobs.iter().filter(|knob| knob.name == "Gain-Band 2").cloned().collect();
            let trial = GenerationTrial { slot: slots[0].name.clone(), knobs: knobs.clone(), values: vec![0.7], fresh: None };
            let generation = rig.generation(&[trial.clone()], Signal::new(), None).await.unwrap();
            assert!(generation.scores.contains_key(&slots[0].name), "{generation:?}");
            let rendered = script.listening().renders().last().cloned().unwrap();
            let copy = rendered["sources"][0]["ref"].as_str().unwrap().to_owned();
            assert!(copy.starts_with("r:track:{C"), "the copy is heard, not the track: {rendered}");
            assert_eq!(script.listening().copy_values(&copy)[0]["normalized"], 0.7);
            assert_eq!(
                script.fx_param("r:track:{T0002}", "ReaEQ", "Gain-Band 2"),
                Some(0.5),
                "the track itself is untouched while searching"
            );
            // The same values again come from what was heard.
            let again = rig.generation(&[trial], Signal::new(), None).await.unwrap();
            assert_eq!(again.cached, 1);
            // Keeping the best needs the bridge to keep a copy as a track; until then it says so.
            let kept = rig.keep_best(&slots[0].name, &knobs, &[0.7], Signal::new()).await.unwrap();
            assert!(kept.contains("update Kumi's bridge"), "{kept}");
            // The winner settles on the track itself, as one change.
            assert_eq!(rig.settle(&slots[0].name, &knobs, &[0.7], Signal::new()).await.unwrap(), None);
            assert_eq!(script.fx_param("r:track:{T0002}", "ReaEQ", "Gain-Band 2"), Some(0.7));
            rig.close().await.unwrap();
            assert_eq!(script.listening().scratch_copies(), 0, "the copies are gone");
            assert_ours(&told.borrow());
            reaper.close().await.unwrap();
            simulator.stop();
        })
        .await;
}

/// A two-bar 8th-note part: on the beat at `on`, off it at `off`, `late` beats behind the 8th.
fn eighths(late: f64, on: i64, off: i64) -> Vec<(i64, f64, f64, i64)> {
    let mut notes: Vec<(i64, f64, f64, i64)> = (0..16)
        .map(|n| {
            let offbeat = n % 2 == 1;
            (42, n as f64 * 0.5 + if offbeat { late } else { 0. }, 0.25, if offbeat { off } else { on })
        })
        .collect();
    notes.extend((0..4).map(|n| (36, n as f64 * 2., 0.5, 110)));
    notes
}

#[tokio::test(flavor = "current_thread")]
async fn groove_moves_a_reaper_items_notes_toward_a_references_feel_in_one_change_of_its_take() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let home = tempfile::tempdir().unwrap();
            let script = ReaperScript::new(3, 0);
            let (part, take) = script.add_midi_item("r:track:{T0002}", "Drums", 0., 8., &eighths(0., 100, 100));
            let (reference, _) = script.add_midi_item("r:track:{T0003}", "Reference", 0., 8., &eighths(0.08, 115, 70));
            let simulator = reaper_lab(&script, home.path(), |_| {}).await;
            let (reaper, told) = integration(Daw::Reaper, home.path(), create_reaper_integration);
            reaper.start(Signal::new()).await.unwrap();
            let observed = reaper.observe(Signal::new(), None).await.unwrap();
            let groove = tool(&observed, "groove");
            assert!(groove.input_schema()["properties"].get("item").is_some(), "the part is an item");
            let start = groove.execute(object(json!({"item": part, "reference": reference})), Signal::new()).await.unwrap();
            assert!(!start.is_error, "{}", start.text);
            assert!(start.text.contains("“Drums”") && start.text.contains("“Reference”"), "{}", start.text);
            // The model pushes an off-beat early, away from the reference's late ones: the round takes it back with Kumi's
            // undo of the take's notes.
            let before = script.midi_notes(&take);
            let pushed = tool(&observed, "notes_change")
                .execute(object(json!({"ref": take, "changes": [{"index": 2, "start": 0.3}]})), Signal::new())
                .await
                .unwrap();
            assert!(!pushed.is_error, "{}", pushed.text);
            assert_ne!(script.midi_notes(&take), before);
            let judged = groove.execute(object(json!({"change": "pushed the first off-beat"})), Signal::new()).await.unwrap();
            let reply: Value = serde_json::from_str(&judged.text).unwrap();
            assert_eq!(reply["kept"], false, "{reply}");
            assert!(reply["log"].to_string().contains("taken back"), "{reply}");
            assert_eq!(script.midi_notes(&take), before, "put back");
            let applied = groove.execute(object(json!({"apply": true})), Signal::new()).await.unwrap();
            assert!(!applied.is_error, "{}", applied.text);
            let reply: Value = serde_json::from_str(&applied.text).unwrap();
            assert_eq!(reply["kept"], true, "{reply}");
            let notes = script.midi_notes(&take);
            let offbeat = notes.iter().find(|note| note.0 == 42 && note.1 > 0.4 && note.1 < 0.7).copied().unwrap();
            assert!(offbeat.1 > 0.55 && offbeat.3 < 100, "the off-beat moved later and softer, toward the reference: {offbeat:?}");
            assert_eq!(notes.len(), 20, "only moved");
            let done = groove.execute(object(json!({"done": true})), Signal::new()).await.unwrap();
            assert!(!done.is_error, "{}", done.text);
            let mut said = vec![start.text, judged.text, applied.text, done.text];
            said.extend(told.borrow().iter().cloned());
            said.extend(described(&observed).into_iter().filter(|text| text.contains("\"groove\"")));
            assert_ours(&said);
            reaper.close().await.unwrap();
            simulator.stop();
        })
        .await;
}

/// A pattern's 8th notes as Renoise's lines (32 lines at 4 a beat): on-beat cells plain, off-beat ones with `delay` and
/// `volume` when given.
fn pattern_eighths(delay: Option<&str>, volume: Option<&str>) -> Value {
    let rows: Vec<Value> = (0..16)
        .map(|n| {
            let mut cell = json!({"col": 1, "note": "C-4", "instrument": "00"});
            if n % 2 == 1 {
                if let Some(delay) = delay {
                    cell["delay"] = delay.into();
                }
                if let Some(volume) = volume {
                    cell["volume"] = volume.into();
                }
            }
            json!({"line": n * 2 + 1, "notes": [cell]})
        })
        .collect();
    Value::Array(rows)
}

#[tokio::test(flavor = "current_thread")]
async fn groove_moves_a_renoise_pattern_tracks_notes_by_their_delays_and_volumes_in_one_write() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let home = tempfile::tempdir().unwrap();
            let script = RenoiseScript::new();
            script.set_lines(1, 1, pattern_eighths(None, None));
            script.set_lines(3, 1, pattern_eighths(Some("50"), Some("48")));
            let simulator = renoise_lab(&script, home.path()).await;
            let (renoise, told) = integration(Daw::Renoise, home.path(), create_renoise_integration);
            renoise.start(Signal::new()).await.unwrap();
            let observed = renoise.observe(Signal::new(), None).await.unwrap();
            let groove = tool(&observed, "groove");
            let start = groove
                .execute(object(json!({"pattern": "1:pattern:1 1:track:1", "reference": "1:pattern:3 1:track:1"})), Signal::new())
                .await
                .unwrap();
            assert!(!start.is_error, "{}", start.text);
            assert!(start.text.contains("“Kick” in pattern 1"), "{}", start.text);
            let applied = groove.execute(object(json!({"apply": true})), Signal::new()).await.unwrap();
            assert!(!applied.is_error, "{}", applied.text);
            let reply: Value = serde_json::from_str(&applied.text).unwrap();
            assert_eq!(reply["kept"], true, "{reply}");
            let lines = script.lines(1, 1);
            let offbeat = lines.as_array().unwrap().iter().find(|row| row["line"] == 3).unwrap()["notes"][0].clone();
            assert!(offbeat["delay"].is_string() && offbeat["volume"].is_string(), "the off-beat delayed and softer: {offbeat}");
            assert_eq!(lines.as_array().unwrap().len(), 16, "the notes stay on their lines");
            // A slot and a track name the part too, and a ref that isn't one says how to name it.
            let wrong = groove.execute(object(json!({"done": true})), Signal::new()).await.unwrap();
            assert!(!wrong.is_error, "{}", wrong.text);
            let again =
                groove.execute(object(json!({"pattern": "1:seq:3 1:track:1", "reference": "1:track:1"})), Signal::new()).await.unwrap();
            assert!(again.is_error && again.text.contains("its pattern's (or a sequence slot's) ref and its track's"), "{}", again.text);
            let mut said = vec![start.text, applied.text, again.text];
            said.extend(told.borrow().iter().cloned());
            assert_ours(&said);
            renoise.close().await.unwrap();
            simulator.stop();
        })
        .await;
}
