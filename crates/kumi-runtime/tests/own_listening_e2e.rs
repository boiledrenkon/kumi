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
            let since = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as i64;
            assert_eq!(reaper.first_heard(since), None, "nothing played yet");
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
            assert!(reaper.first_heard(since).is_some_and(|at| at >= since), "the producer heard Kumi's listen");
            said.push(audition.text);
            // A round REAPER couldn't play out loud is said to be rendered, not heard.
            script.listening().set_audition(false);
            let unheard = tool(&observed, "audition")
                .execute(object(json!({"candidates": [{"track": track(2)}, {"track": track(3)}], "from_beat": 16, "beats": 8})), Signal::new())
                .await
                .unwrap();
            assert!(unheard.text.contains("The producer didn't hear this round: REAPER rendered it without playing it out loud."), "{}", unheard.text);
            script.listening().set_audition(true);
            said.push(unheard.text);

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
            // A track heard on its own is said by its name, not its ref.
            let one = reaper.hear(&HearRequest { tracks: vec![track(2)], from_beat: Some(0.), beats: Some(4.), ..Default::default() }, Signal::new()).await;
            assert!(one.as_ref().is_ok_and(|heard| heard.is_ok()), "{one:?}");
            let name = context["tracks"][2]["name"].as_str().unwrap();
            assert!(told.borrow().iter().any(|title| title.ends_with(&format!("· listening to {name}"))), "{:?}", told.borrow());

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
async fn a_listen_after_kumis_last_one_ended_by_itself_says_nothing_was_stopped() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let home = tempfile::tempdir().unwrap();
            let script = ReaperScript::new(3, 0);
            let simulator = reaper_lab(&script, home.path(), |_| {}).await;
            let (reaper, told) = integration(Daw::Reaper, home.path(), create_reaper_integration);
            reaper.start(Signal::new()).await.unwrap();
            reaper.observe(Signal::new(), None).await.unwrap();
            let half_a_second = HearRequest { mix: Some(true), from_beat: Some(0.), beats: Some(1.), ..Default::default() };
            reaper.hear(&half_a_second, Signal::new()).await.unwrap().unwrap();
            // Its half second played out (and Kumi's second of slack): the next render stops nothing.
            tokio::time::sleep(std::time::Duration::from_millis(1700)).await;
            let heard = reaper.hear(&half_a_second, Signal::new()).await.unwrap().unwrap();
            let stopped = "The audition playing was stopped";
            assert!(!told.borrow().iter().any(|title| title.starts_with(stopped)), "{:?}", told.borrow());
            assert!(!heard[0].note.clone().unwrap_or_default().contains(stopped), "{heard:?}");
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
            let (reaper, told) = integration(Daw::Reaper, home.path(), create_reaper_integration);
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
            // REAPER adds the gain FX at +6 dB: what it is set to is the turn the round says, not +6 dB more.
            let said = Regex::new(r"Volume Adjustment gain \+?(-?[0-9.]+) dB").unwrap();
            let turned: f64 = said.captures(&round.text).and_then(|found| found[1].parse().ok()).unwrap_or(f64::NAN);
            assert!(
                (-150. + 300. * adjusted - turned).abs() < 0.06,
                "set to {} dB for a {turned} dB turn: {}",
                -150. + 300. * adjusted,
                round.text
            );
            assert!(round.text.contains("\"kept\":true"), "{}", round.text);
            // Each listen of the run stopped the last one playing out loud: said once.
            let stopped = told.borrow().iter().filter(|title| title.starts_with("The audition playing was stopped")).count();
            assert_eq!(stopped, 1, "{:?}", told.borrow());
            assert!(!round.text.contains("The audition playing was stopped"), "said once, at the run's first: {}", round.text);
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
            let eq = script.add_fx("r:track:{T0002}", "ReaEQ");
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
            // Never the safety limiter Kumi put last, nor REAPER's Bypass, Wet and Delta: they aren't the sound.
            assert!(slots[0].knobs.iter().all(|knob| !knob.device.contains("ReaLimit")), "{:?}", slots[0].knobs);
            assert!(slots[0].knobs.iter().all(|knob| !["Bypass", "Wet", "Delta"].contains(&knob.name.as_str())), "{:?}", slots[0].knobs);
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
            // Keeping the best: a fresh copy at these values kept as a track of the project (scratch-keep); keeping again
            // takes the last one away first (Kumi's undo of keeping it).
            assert_eq!(rig.keep_best(&slots[0].name, &knobs, &[0.7], Signal::new()).await.unwrap(), "Kumi · Goal best");
            let first = script.listening().kept();
            assert_eq!(first.len(), 1, "{first:?}");
            assert_eq!(first[0].1, "Kumi · Goal best");
            assert_eq!(rig.keep_best(&slots[0].name, &knobs, &[0.7], Signal::new()).await.unwrap(), "Kumi · Goal best");
            let second = script.listening().kept();
            assert_eq!(second.len(), 1, "the last best went: {second:?}");
            assert_ne!(second[0].0, first[0].0, "a fresh copy is kept");
            // The producer put an FX first on the track meanwhile: the winner still lands on the ReaEQ, found by its id, as
            // one change (REAPER's fx-params).
            script.insert_fx_first("r:track:{T0002}", "Volume Adjustment");
            assert_eq!(rig.settle(&slots[0].name, &knobs, &[0.7], Signal::new()).await.unwrap(), None);
            assert_eq!(script.fx_param("r:track:{T0002}", "ReaEQ", "Gain-Band 2"), Some(0.7));
            assert_eq!(
                script.fx_param("r:track:{T0002}", "Volume Adjustment", "Adjustment (dB)"),
                Some(0.52),
                "the FX now first is untouched"
            );
            let applied: Vec<Value> = script
                .asked()
                .into_iter()
                .filter(|(method, params)| method == "change_apply" && params["input"]["values"][0]["normalized"] == 0.7)
                .map(|(_, params)| params)
                .collect();
            assert_eq!(applied.len(), 1, "{applied:?}");
            assert_eq!(applied[0]["kind"], "fx-params");
            // The ReaEQ gone: nothing is set or kept, and why is said.
            script.remove_fx(&eq);
            let refused = rig.settle(&slots[0].name, &knobs, &[0.4], Signal::new()).await.unwrap();
            assert!(refused.as_deref().is_some_and(|why| why.contains("moved or went")), "{refused:?}");
            let kept = rig.keep_best(&slots[0].name, &knobs, &[0.4], Signal::new()).await.unwrap();
            assert!(kept.contains("moved or went"), "{kept}");
            rig.close().await.unwrap();
            assert_eq!(script.listening().scratch_copies(), 0, "the copies are gone");
            assert_eq!(script.listening().kept().len(), 1, "the best stays a track of the project's");
            assert_ours(&told.borrow());
            reaper.close().await.unwrap();
            simulator.stop();
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn each_candidate_is_heard_over_its_own_part_in_an_audition_and_in_a_goal() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let home = tempfile::tempdir().unwrap();
            let script = ReaperScript::new(3, 0);
            // Two parts far apart: bar 17 on Track 2, bar 49 on Track 3 (two bars each).
            let (a, _) = script.add_midi_item("r:track:{T0002}", "Verse", 64., 8., &eighths(0., 100, 100));
            let (b, _) = script.add_midi_item("r:track:{T0003}", "Chorus", 192., 8., &eighths(0., 100, 100));
            let simulator = reaper_lab(&script, home.path(), |_| {}).await;
            let (reaper, told) = integration(Daw::Reaper, home.path(), create_reaper_integration);
            reaper.start(Signal::new()).await.unwrap();
            reaper.observe(Signal::new(), None).await.unwrap();
            let candidate = |track: &str, part: &str| AuditionCandidate {
                track: track.into(),
                clip: Some(part.into()),
                label: Some(track.to_lowercase()),
                mix: None,
            };
            let reference = home.path().join("reference.wav");
            daw_mcp_server::simulator::listening::write_noise(&reference, 4., 0.4, 9).unwrap();
            let request = |candidates: Vec<AuditionCandidate>| AuditionRequest {
                candidates,
                from_beat: None,
                beats: None,
                reference: Some(reference.to_string_lossy().into_owned()),
                reference_from: None,
                reference_seconds: None,
                focus: None,
            };
            let ranges =
                |from: usize| -> Vec<Value> { script.listening().renders()[from..].iter().map(|render| render["range"].clone()).collect() };

            // An audition: each candidate rendered over its own part, not over the first one's.
            let before = script.listening().renders().len();
            let heard = reaper.audition(&request(vec![candidate("Track 2", &a), candidate("Track 3", &b)]), Signal::new()).await.unwrap();
            assert!(heard.is_ok(), "{heard:?}");
            assert_eq!(ranges(before), [json!({"fromBeat": 64.0, "beats": 8.0}), json!({"fromBeat": 192.0, "beats": 8.0})]);
            assert_eq!(script.listening().previews().len(), 1, "one candidate played out loud for the round");
            // Parts of different lengths are refused in words, nothing rendered.
            let (long, _) = script.add_midi_item("r:track:{T0002}", "Bridge", 256., 12., &eighths(0., 100, 100));
            let before = script.listening().renders().len();
            let refused =
                reaper.audition(&request(vec![candidate("Track 2", &long), candidate("Track 3", &b)]), Signal::new()).await.unwrap();
            let said = refused.map(|result| result.notes.join(" ")).unwrap_or_else(|why| why);
            assert!(said.contains("different lengths (“track 2” 12 beats, “track 3” 8 beats): give beats"), "{said}");
            assert_eq!(script.listening().renders().len(), before);
            // A part longer than 32 beats is heard for its first 32.
            let (longest, _) = script.add_midi_item("r:track:{T0003}", "Outro", 320., 40., &eighths(0., 100, 100));
            let before = script.listening().renders().len();
            let capped = reaper.audition(&request(vec![candidate("Track 3", &longest)]), Signal::new()).await.unwrap();
            assert!(capped.is_ok(), "{capped:?}");
            assert_eq!(ranges(before), [json!({"fromBeat": 320.0, "beats": 32.0})]);

            // A goal from the same candidates: each slot's copy made and heard over its own part.
            let made = |since: usize| -> Vec<Value> {
                script.asked()[since..]
                    .iter()
                    .filter(|(method, params)| method == "scratch" && params["action"] == "make" && params["label"] == "Kumi: goal search")
                    .map(|(_, params)| params["range"].clone())
                    .collect()
            };
            let asked = script.asked().len();
            let rig =
                reaper.goal(&request(vec![candidate("Track 2", &a), candidate("Track 3", &b)]), Signal::new()).await.unwrap().unwrap();
            assert_eq!(made(asked), [json!({"fromBeat": 64.0, "beats": 8.0}), json!({"fromBeat": 192.0, "beats": 8.0})]);
            let slots = rig.slots();
            let trials: Vec<GenerationTrial> = slots
                .iter()
                .map(|slot| GenerationTrial { slot: slot.name.clone(), knobs: vec![], values: vec![], fresh: Some(true) })
                .collect();
            let before = script.listening().renders().len();
            rig.generation(&trials, Signal::new(), None).await.unwrap();
            assert_eq!(ranges(before), [json!({"fromBeat": 64.0, "beats": 8.0}), json!({"fromBeat": 192.0, "beats": 8.0})]);
            rig.close().await.unwrap();
            // A goal heard again names each track's first part: where that track first plays.
            let asked = script.asked().len();
            let again = reaper
                .goal(&request(vec![candidate("Track 2", "first"), candidate("Track 3", "first")]), Signal::new())
                .await
                .unwrap()
                .unwrap();
            assert_eq!(made(asked), [json!({"fromBeat": 64.0, "beats": 8.0}), json!({"fromBeat": 192.0, "beats": 8.0})]);
            again.close().await.unwrap();
            assert_ours(&told.borrow());
            reaper.close().await.unwrap();
            simulator.stop();
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn a_renoise_goal_settles_its_winner_as_one_device_params_change_that_kumis_undo_takes_back() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let home = tempfile::tempdir().unwrap();
            let script = RenoiseScript::new();
            let eq = script.add_device(4, "EQ 10");
            let simulator = renoise_lab(&script, home.path()).await;
            let (renoise, told) = integration(Daw::Renoise, home.path(), create_renoise_integration);
            renoise.start(Signal::new()).await.unwrap();
            renoise.observe(Signal::new(), None).await.unwrap();
            let reference = home.path().join("reference.wav");
            daw_mcp_server::simulator::listening::write_noise(&reference, 4., 0.4, 9).unwrap();
            let request = AuditionRequest {
                candidates: vec![AuditionCandidate { track: "Bass".into(), clip: None, label: Some("bass".into()), mix: None }],
                from_beat: Some(0.),
                beats: Some(4.),
                reference: Some(reference.to_string_lossy().into_owned()),
                reference_from: None,
                reference_seconds: None,
                focus: None,
            };
            let rig = renoise.goal(&request, Signal::new()).await.unwrap().unwrap();
            let slots = rig.slots();
            assert_eq!(slots[0].chain, "TrackVolPan → EQ 10 → Maximizer", "a safety Maximizer last");
            let knobs: Vec<_> = slots[0].knobs.iter().filter(|knob| knob.name.starts_with("Gain 2 ")).cloned().collect();
            assert_eq!(knobs.len(), 1, "{:?}", slots[0].knobs);
            let trial = GenerationTrial { slot: slots[0].name.clone(), knobs: knobs.clone(), values: vec![6.], fresh: None };
            let generation = rig.generation(&[trial], Signal::new(), None).await.unwrap();
            assert!(generation.scores.contains_key(&slots[0].name), "{generation:?}");
            assert_eq!(rig.keep_best(&slots[0].name, &knobs, &[6.], Signal::new()).await.unwrap(), "Kumi · Goal best");
            assert_eq!(script.device_value(4, eq, "Gain 2 (63 Hz)"), Some(0.), "the track itself is untouched while searching");
            // The copies moved Renoise's epoch since the knobs were read: the winner settles all the same, in one change.
            assert_eq!(rig.settle(&slots[0].name, &knobs, &[6.], Signal::new()).await.unwrap(), None);
            assert_eq!(script.device_value(4, eq, "Gain 2 (63 Hz)"), Some(6.));
            let applied: Vec<Value> = script
                .asked()
                .into_iter()
                .filter(|(method, params)| {
                    method == "change_apply" && params["kind"].as_str().is_some_and(|kind| kind.starts_with("device-param"))
                })
                .map(|(_, params)| params["input"].clone())
                .collect();
            assert_eq!(applied.len(), 2, "the settle and the safety Maximizer's Boost at 0 dB: {applied:?}");
            assert_eq!(applied[0]["values"], json!([{"param": 2, "value": 6.0}]), "{applied:?}");
            // Kumi's undo takes the settle back as a new step.
            let observed = renoise.observe(Signal::new(), None).await.unwrap();
            let undo = tool(&observed, "undo_change").execute(object(json!({"change": "last"})), Signal::new()).await.unwrap();
            assert!(!undo.is_error && undo.text.contains("EQ 10 1 parameter"), "{}", undo.text);
            assert_eq!(script.device_value(4, eq, "Gain 2 (63 Hz)"), Some(0.));
            // The producer puts a device before the EQ: Renoise's refs are places, so the knobs' device isn't where it was
            // and nothing is set.
            script.insert_device(4, 2, "Gainer");
            let refused = rig.settle(&slots[0].name, &knobs, &[-3.], Signal::new()).await.unwrap();
            assert!(refused.as_deref().is_some_and(|why| why.contains("moved or went")), "{refused:?}");
            assert_eq!(script.device_value(4, eq + 1, "Gain 2 (63 Hz)"), Some(0.), "the EQ, now one further, is untouched");
            rig.close().await.unwrap();
            assert_ours(&told.borrow());
            renoise.close().await.unwrap();
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

#[tokio::test(flavor = "current_thread")]
async fn reaper_says_what_it_noted_when_a_render_fails_and_a_capture_is_split_where_the_playhead_jumped() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let home = tempfile::tempdir().unwrap();
            let script = ReaperScript::new(3, 0);
            let simulator = reaper_lab(&script, home.path(), |_| {}).await;
            let (reaper, told) = integration(Daw::Reaper, home.path(), create_reaper_integration);
            reaper.start(Signal::new()).await.unwrap();
            reaper.observe(Signal::new(), None).await.unwrap();
            // A render that fails says why, and what the bridge noted on the way.
            script.listening().note_next(&["REAPER stopped playback to render"]);
            script.listening().fail_next("failed", "REAPER couldn't write the file");
            let failed = reaper
                .hear(&HearRequest { mix: Some(true), from_beat: Some(0.), beats: Some(8.), ..Default::default() }, Signal::new())
                .await
                .unwrap();
            let why = failed.unwrap_err();
            assert!(why.contains("couldn't write the file (REAPER stopped playback to render)"), "{why}");
            // While REAPER plays, the loop wrapped two seconds in: the stretches aren't parts heard at once, so the first longest
            // one is measured, and the jump is said.
            script.set_playing(true);
            script.listening().set_capture_marks(json!([[0, 10.0, 20.0, true], [96000, 10.0, 20.0, true]]));
            let heard = reaper
                .hear(&HearRequest { mix: Some(true), seconds: Some(4.), ..Default::default() }, Signal::new())
                .await
                .unwrap()
                .unwrap();
            let takes: Vec<(&str, f64, Option<f64>)> = heard.iter().map(|take| (take.label.as_str(), take.start, take.seconds)).collect();
            assert_eq!(takes, [("The mix from bar 6", 0., Some(2.))]);
            let note = heard[0].note.clone().unwrap_or_default();
            assert!(note.contains("REAPER jumped while Kumi listened (The mix from bar 6, The mix from bar 6 (pass 2))"), "{note}");
            // Copies an earlier session left, swept by the bridge: the producer is told which, once.
            simulator.emit("scratch_swept", json!({"count": 1, "names": ["Kumi · try ab12 1"]}));
            for _ in 0..50 {
                if told.borrow().iter().any(|said| said.contains("an earlier session left")) {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
            let swept: Vec<String> = told.borrow().iter().filter(|said| said.contains("an earlier session left")).cloned().collect();
            assert_eq!(swept, ["REAPER removed Kumi's scratch copy an earlier session left in the project: “Kumi · try ab12 1”"]);
            // Copies REAPER's undo brought back after Kumi dropped them, removed again.
            simulator.emit("scratch_swept", json!({"count": 2, "names": ["Kumi · try cd34 1", "Kumi · try cd34 2"], "why": "undo"}));
            for _ in 0..50 {
                if told.borrow().iter().any(|said| said.contains("brought back")) {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
            let back: Vec<String> = told.borrow().iter().filter(|said| said.contains("brought back")).cloned().collect();
            assert_eq!(back, ["REAPER's undo brought back Kumi's scratch copies “Kumi · try cd34 1”, “Kumi · try cd34 2”, which Kumi had removed: removed them again"]);
            assert_ours(&told.borrow());
            reaper.close().await.unwrap();
            simulator.stop();
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn an_items_audio_is_only_what_it_plays_of_its_source_the_way_it_plays_it() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let home = tempfile::tempdir().unwrap();
            let script = ReaperScript::new(3, 0);
            let simulator = reaper_lab(&script, home.path(), |_| {}).await;
            let (reaper, _) = integration(Daw::Reaper, home.path(), create_reaper_integration);
            reaper.start(Signal::new()).await.unwrap();
            reaper.observe(Signal::new(), None).await.unwrap();
            // The item plays a second of its 4-second source from half a second in, reversed.
            script.listening().set_item_part(0.5, 1.0, 1.0, true);
            let file = reaper.audio_file("r:item:{I-T0001}", Signal::new()).await.unwrap().unwrap();
            assert!(file.contains("reversed"), "{file}");
            let bytes = std::fs::read(&file).unwrap();
            let data = u32::from_le_bytes(bytes[40..44].try_into().unwrap()) as f64;
            assert!((data / (48_000. * 2. * 4.) - 1.).abs() < 0.01, "one second of 32-bit stereo: {data} bytes");
            reaper.close().await.unwrap();
            simulator.stop();
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn a_renoise_pattern_is_auditioned_over_its_first_slot_in_the_sequence() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let home = tempfile::tempdir().unwrap();
            let script = RenoiseScript::new();
            let simulator = renoise_lab(&script, home.path()).await;
            let (renoise, told) = integration(Daw::Renoise, home.path(), create_renoise_integration);
            renoise.start(Signal::new()).await.unwrap();
            renoise.observe(Signal::new(), None).await.unwrap();
            let request = AuditionRequest {
                candidates: vec![AuditionCandidate {
                    track: "1:track:1".into(),
                    clip: Some("1:pattern:2".into()),
                    label: Some("kick".into()),
                    mix: None,
                }],
                from_beat: None,
                beats: None,
                reference: None,
                reference_from: None,
                reference_seconds: None,
                focus: None,
            };
            let result = renoise.audition(&request, Signal::new()).await.unwrap();
            assert!(result.is_ok(), "{result:?}");
            // Pattern 2 first plays in slot 2, after pattern 1's 32 lines (8 beats at 4 lines a beat).
            assert_eq!(script.listening().renders().last().unwrap()["range"], json!({"fromBeat": 8.0, "beats": 8.0}));
            let played = script.listening().previews();
            assert_eq!(played.len(), 1, "the render, played with the song's own playback");
            assert_ours(&told.borrow());
            renoise.close().await.unwrap();
            simulator.stop();
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn a_limiter_that_cant_turn_down_far_enough_leaves_the_rest_to_a_gain_fx_and_monitoring_fx_are_no_gain_stage() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let home = tempfile::tempdir().unwrap();
            let script = ReaperScript::new(3, 0);
            // ReaLimit on the master at its 0 dB threshold (as far up as it goes), and a monitoring limiter after it.
            script.add_fx("r:track:master", "ReaLimit");
            script.add_input_fx("r:track:master", "ReaLimit");
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
            // A change that takes the mix past the goal's loudness, but closer to it than before: kept, and brought back down.
            script.listening().set_master_level(0.5);
            let context: Value = serde_json::from_str(&observed.context).unwrap();
            let bass = context["tracks"][2]["ref"].as_str().unwrap().to_owned();
            let change = tool(&observed, "track_volume").execute(object(json!({"ref": bass, "volumeDb": 3})), Signal::new()).await.unwrap();
            assert!(!change.is_error, "{}", change.text);
            let round = judge.execute(object(json!({"change": "Bass up 3 dB"})), Signal::new()).await.unwrap();
            assert!(!round.is_error, "{}", round.text);
            // The main chain's ReaLimit can't raise its threshold past 0 dB: a Volume Adjustment after it turns down; the
            // monitoring ReaLimit is never touched.
            assert_eq!(
                script.fx_names("r:track:master"),
                ["VST: ReaLimit (Cockos)", "VST: ReaLimit (Cockos)", "JS: Volume Adjustment"],
                "{}",
                round.text
            );
            let adjusted = script.fx_param("r:track:master", "Volume Adjustment", "Adjustment (dB)").unwrap();
            assert!(adjusted < 0.5, "turned down: {adjusted} ({})", round.text);
            reaper.close().await.unwrap();
            simulator.stop();
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn reaper_measures_sounds_lays_out_the_form_and_tunes_a_reaeq_on_scratch_copies() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let home = tempfile::tempdir().unwrap();
            let script = ReaperScript::new(3, 0);
            let eq = script.add_fx("r:track:{T0002}", "ReaEQ");
            // (each render done at its first status: a search renders many)
            script.listening().set_steps(0);
            let simulator = reaper_lab(&script, home.path(), |_| {}).await;
            let (reaper, told) = integration(Daw::Reaper, home.path(), create_reaper_integration);
            reaper.start(Signal::new()).await.unwrap();
            let observed = reaper.observe(Signal::new(), None).await.unwrap();
            let mut said = vec![];
            let sound = tool(&observed, "sound")
                .execute(object(json!({"tracks": ["Track 2"], "from_beat": 0, "beats": 8})), Signal::new())
                .await
                .unwrap();
            assert!(!sound.is_error, "{}", sound.text);
            said.push(sound.text);
            let form = tool(&observed, "form").execute(object(json!({"from_beat": 0, "beats": 16})), Signal::new()).await.unwrap();
            assert!(!form.is_error, "{}", form.text);
            said.push(form.text);
            let judge = tool(&observed, "judge");
            let start = judge
                .execute(
                    object(json!({"goal": {"loudness": -14, "problems": false}, "track": "Track 2", "from_beat": 0, "beats": 2})),
                    Signal::new(),
                )
                .await
                .unwrap();
            assert!(!start.is_error, "{}", start.text);
            let tuned = tool(&observed, "tune")
                .execute(object(json!({"device": eq, "how": "search", "knobs": ["Gain-Band 2", "Gain-Band 3"]})), Signal::new())
                .await
                .unwrap();
            assert!(!tuned.is_error, "{}", tuned.text);
            assert!(script.listening().renders().iter().any(|render| render["sources"].to_string().contains("r:track:{C")), "copies heard");
            assert_eq!(script.listening().scratch_copies(), 0, "the copies are gone after the search");
            said.push(tuned.text);
            said.extend(told.borrow().iter().cloned());
            assert_ours(&said);
            reaper.close().await.unwrap();
            simulator.stop();
        })
        .await;
}
