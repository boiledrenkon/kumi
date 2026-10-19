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
