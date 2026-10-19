//! REAPER's and Renoise's integrations end to end: Kumi's host (daw-mcp-server's own code, in process) in front of a
//! bridge that answers like REAPER's extension, or replays what Renoise's tool answered. What the host answers is what
//! the integration reads: the observation, the model's read, the focus and FOCUS's bodies.
use async_trait::async_trait;
use daw_bridge::{files::ensure_secret, paths::BridgePaths, Daw};
use daw_mcp_server::{
    host::Host,
    simulator::{reaper::ReaperScript, renoise::RenoiseScript, Simulator},
};
use futures::FutureExt;
use kumi_common::abort::Signal;
use kumi_runtime::core::{
    context::IntegrationContext,
    contracts::*,
    errors::RuntimeError,
    view::{BodyKind, BodyRequest, DawFocus, FocusBody},
};
use kumi_runtime::integrations::{
    own_bridge::{Connect, OwnOptions},
    reaper::create_reaper_integration,
    renoise::create_renoise_integration,
};
use kumi_runtime::mcp::client::{McpEndpoint, StderrStatus};
use kumi_runtime::mcp::types::{CallToolResult, Implementation, ListToolsResult};
use serde_json::{json, Value};
use std::{cell::Cell, cell::RefCell, rc::Rc, sync::Arc};

/// Every key of `input` is one the tool's schema offers (it takes no others), and what it requires is there.
fn within_schema(schema: &JsonObject, input: &JsonObject) {
    let properties = schema["properties"].as_object().unwrap();
    for key in input.keys() {
        assert!(properties.contains_key(key), "{key} isn't in the schema: {schema:?}");
    }
    for key in schema.get("required").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str) {
        assert!(input.contains_key(key), "{key} is required");
    }
}

/// The host's MCP, called in process.
struct InProcess {
    host: Arc<Host>,
    id: Cell<u64>,
}
#[async_trait(?Send)]
impl McpEndpoint for InProcess {
    fn pid(&self) -> Option<u32> {
        None
    }
    fn server_info(&self) -> Option<Implementation> {
        None
    }
    async fn list(&self, _: Option<&str>, _: Signal) -> Result<ListToolsResult, RuntimeError> {
        Ok(serde_json::from_value(json!({"tools": daw_mcp_server::mcp::tools(&self.host)})).unwrap())
    }
    async fn call(&self, name: &str, args: JsonObject, _: Signal) -> Result<CallToolResult, RuntimeError> {
        self.id.set(self.id.get() + 1);
        let message = json!({"jsonrpc": "2.0", "id": self.id.get(), "method": "tools/call", "params": {"name": name, "arguments": args}});
        let answer = daw_mcp_server::mcp::handle(&self.host, &message).await.expect("an answer");
        Ok(serde_json::from_value(answer["result"].clone()).unwrap_or_else(|e| panic!("{answer}: {e}")))
    }
    fn on_catalog_changed(&self, _: Rc<dyn Fn()>) -> Box<dyn Fn()> {
        Box::new(|| {})
    }
    fn on_disconnect(&self, _: Rc<dyn Fn()>) -> Box<dyn Fn()> {
        Box::new(|| {})
    }
    /// The bridge's events as the host forwards them (`notifications/daw_event`).
    fn on_live_event(&self, listener: Rc<dyn Fn(JsonObject)>) -> Box<dyn Fn()> {
        let mut events = self.host.events();
        let task = tokio::task::spawn_local(async move {
            while let Ok(event) = events.recv().await {
                if let Some(event) = event.as_object() {
                    listener(event.clone());
                }
            }
        });
        Box::new(move || task.abort())
    }
    fn stderr_status(&self) -> StderrStatus {
        StderrStatus { bytes: 0, truncated: false }
    }
    async fn close(&self) -> Result<(), RuntimeError> {
        self.host.close().await;
        Ok(())
    }
}

#[tokio::test(flavor = "current_thread")]
async fn reapers_integration_reads_what_kumis_host_answers() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let home = tempfile::tempdir().unwrap();
            let paths = BridgePaths::new(home.path(), Daw::Reaper);
            let secret = ensure_secret(&paths.secret()).unwrap();
            let script = ReaperScript::new(3, 0);
            let mut config = script.config(secret, "/lab/REAPER");
            config.discovery_dir = Some(paths.discovery());
            let simulator = Simulator::start(config).await.unwrap();
            let endpoint = Rc::new(InProcess { host: Arc::new(Host::new(Daw::Reaper, home.path())), id: Cell::new(0) });
            let connect: Connect = Rc::new(move |_| {
                let endpoint = endpoint.clone();
                async move { Ok(endpoint as Rc<dyn McpEndpoint>) }.boxed_local()
            });
            let seen: Rc<RefCell<Vec<Option<DawFocus>>>> = Rc::default();
            let into = seen.clone();
            let mut context = IntegrationContext::new(Rc::new(|_, _| {}));
            context.on_focus = Some(Rc::new(move |focus| into.borrow_mut().push(focus)));
            context.focus_interval_ms = Some(50);
            let reaper = create_reaper_integration(OwnOptions { context, kumi_home: home.path().into(), connect: Some(connect) });

            reaper.start(Signal::new()).await.unwrap();
            let observed = reaper.observe(Signal::new(), None).await.unwrap();
            assert!(observed.label.starts_with("REAPER — "), "{}", observed.label);
            let context: Value = serde_json::from_str(&observed.context).unwrap();
            assert_eq!(context["daw"], "REAPER");
            let tracks = context["tracks"].as_array().unwrap();
            assert_eq!(tracks.len(), 4, "the master and three tracks: {context}");
            assert!(tracks.iter().all(|track| track["ref"].as_str().is_some_and(|r| r.starts_with("track:"))), "{context}");
            assert_eq!(context["selection"]["trackRef"], tracks[1]["ref"]);
            assert!(!observed.context.contains("r:track:"), "no long ref reaches the model: {}", observed.context);

            let read = observed.tools[0].clone();
            let page = read
                .execute(json!({"kind": "item", "parent": tracks[1]["ref"]}).as_object().unwrap().clone(), Signal::new())
                .await
                .unwrap();
            assert!(!page.is_error, "{}", page.text);
            assert!(!page.text.contains("r:item:") && page.text.contains("item:"), "{}", page.text);
            // What folders, send modes and times read as.
            for words in ["folder 1 starts a folder and -1 or less closes", "0 post-fader, 1 pre-FX, 3 pre-fader", "quarter notes"] {
                assert!(read.description().contains(words), "{words}: {}", read.description());
            }

            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            let focus = seen.borrow().iter().flatten().next().cloned().expect("the focus, as the host reads it");
            let track = focus.track_ref.clone().expect("the selected track");
            for (kind, request) in [
                (BodyKind::Timeline, BodyRequest::Timeline),
                (BodyKind::Routing, BodyRequest::Routing { track_ref: track.clone() }),
                (BodyKind::Tree, BodyRequest::Tree { track_ref: track.clone() }),
            ] {
                assert!(reaper.can_show(kind));
                let body = reaper.focus_body(&request, Signal::new()).await.unwrap();
                assert!(
                    matches!(
                        (&request, &body),
                        (BodyRequest::Timeline, Some(FocusBody::Timeline(_)))
                            | (BodyRequest::Routing { .. }, Some(FocusBody::Routing(_)))
                            | (BodyRequest::Tree { .. }, Some(FocusBody::Tree(_)))
                    ),
                    "{request:?}: {body:?}"
                );
            }
            let arrange =
                script.asked().into_iter().rev().find(|(method, params)| method == "focus_body" && params["kind"] == "arrange").unwrap();
            assert_eq!(arrange.1["around"].as_str(), Some(track.as_str()), "the page is centred on the selected track");
            reaper.close().await.unwrap();
            simulator.stop();
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn renoises_integration_reads_what_kumis_host_answers_from_the_tool() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let home = tempfile::tempdir().unwrap();
            let paths = BridgePaths::new(home.path(), Daw::Renoise);
            let secret = ensure_secret(&paths.secret()).unwrap();
            let script = RenoiseScript::new();
            let mut config = script.config(secret, "/lab/Renoise");
            config.discovery_dir = Some(paths.discovery());
            let simulator = Simulator::start(config).await.unwrap();
            let endpoint = Rc::new(InProcess { host: Arc::new(Host::new(Daw::Renoise, home.path())), id: Cell::new(0) });
            let connect: Connect = Rc::new(move |_| {
                let endpoint = endpoint.clone();
                async move { Ok(endpoint as Rc<dyn McpEndpoint>) }.boxed_local()
            });
            let seen: Rc<RefCell<Vec<Option<DawFocus>>>> = Rc::default();
            let into = seen.clone();
            let mut context = IntegrationContext::new(Rc::new(|_, _| {}));
            context.on_focus = Some(Rc::new(move |focus| into.borrow_mut().push(focus)));
            context.focus_interval_ms = Some(50);
            let renoise = create_renoise_integration(OwnOptions { context, kumi_home: home.path().into(), connect: Some(connect) });

            renoise.start(Signal::new()).await.unwrap();
            let observed = renoise.observe(Signal::new(), None).await.unwrap();
            assert_eq!(observed.label, "Renoise — Demo");
            let context: Value = serde_json::from_str(&observed.context).unwrap();
            assert_eq!((context["daw"].as_str(), context["song"]["bpm"].as_i64()), (Some("Renoise"), Some(128)), "{context}");
            let tracks = context["tracks"].as_array().unwrap();
            assert_eq!(tracks.len(), 7, "{context}");
            assert_eq!(
                (tracks[0]["ref"].as_str(), tracks[0]["groupRef"].as_str()),
                (Some("track:1"), Some("track:3")),
                "a group in short refs too"
            );
            assert_eq!(context["selection"]["patternRef"], "pattern:1");
            assert!(
                !observed.context.contains("1:track:") && !observed.context.contains("1:pattern:"),
                "no long ref reaches the model: {}",
                observed.context
            );
            assert!(observed.project.as_ref().is_some_and(|project| project.id.starts_with("renoise-")), "a saved song is its place");

            // The model reads a track's devices and a pattern's lines (which need the track) in short refs.
            let read = observed.tools[0].clone();
            let devices =
                read.execute(json!({"kind": "device", "parent": "track:1"}).as_object().unwrap().clone(), Signal::new()).await.unwrap();
            assert!(!devices.is_error && devices.text.contains("EQ 5") && devices.text.contains("device:"), "{}", devices.text);
            assert!(!devices.text.contains("1:device:"), "{}", devices.text);
            // A pattern's lines, asked as the model can ask through the tool's schema: the track by its ref, lengthened.
            let input = json!({"kind": "line", "parent": "pattern:1", "trackRef": "track:1"}).as_object().unwrap().clone();
            within_schema(&read.input_schema(), &input);
            let _ = read.execute(input, Signal::new()).await.unwrap();
            assert_eq!(
                script.asked().last().unwrap().1,
                json!({"kind": "line", "parent": "1:pattern:1", "track": "1:track:1", "layout": 0})
            );
            // The columns the track hides, asked for as the schema allows.
            let input = json!({"kind": "line", "parent": "pattern:1", "trackRef": "track:1", "hidden": true}).as_object().unwrap().clone();
            within_schema(&read.input_schema(), &input);
            let _ = read.execute(input, Signal::new()).await.unwrap();
            assert_eq!(script.asked().last().unwrap().1["hidden"], true);
            assert!(
                read.description().contains("line under a pattern") && read.description().contains("trackRef"),
                "{}",
                read.description()
            );
            // What the lines' hex columns mean, so the model reads them without guessing.
            for words in ["volume (hex 00-80, 80 full", "panning (hex 00-80, 40 centre)", "delay (hex 00-FF"] {
                assert!(read.description().contains(words), "{words}: {}", read.description());
            }

            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            let focus = seen.borrow().iter().flatten().next().cloned().expect("the focus, as the host reads it");
            assert_eq!((focus.track_ref.as_deref(), focus.device.as_deref()), (Some("1:track:1"), Some("Doofer")));
            assert_eq!(focus.item.as_ref().and_then(|item| item.item_ref.as_deref()), Some("1:pattern:1"));
            assert_eq!((focus.editor_view.as_ref(), focus.panes.as_ref(), focus.section_follows), (None, None, None), "neither pane open");

            // The pattern FOCUS asks for is the focus's own, by its ref.
            let pattern = BodyRequest::Tracker { item_ref: focus.item.as_ref().and_then(|item| item.item_ref.clone()) };
            let Some(FocusBody::Tracker(grid)) = renoise.focus_body(&pattern, Signal::new()).await.unwrap() else { panic!("the pattern") };
            let asked = script.asked();
            assert_eq!(
                asked.iter().rev().find(|(method, _)| method == "focus_body").unwrap().1,
                // (No layout: a pattern isn't on a track, so no track shift moves it.)
                json!({"kind": "pattern", "ref": "1:pattern:1"})
            );
            assert_eq!((grid.lines, grid.from, grid.cursor), (32, 0, Some(0)));
            let kick = &grid.tracks[0];
            assert_eq!(kick.cells.len(), 32, "every line of the window, though the tool sends only those with something in them");
            assert_eq!(kick.cells[0], "C-4 00 .. .. .. .... ....");
            assert_eq!(kick.cells[1], "--- .. .. .. .. .... ....");
            assert_eq!(kick.cells[4], "D#4 00 40 .. .. 0A20 ....", "line 5 where Renoise shows it");
            assert_eq!(grid.tracks[3].cells[2], "C-3 01 .. .. .. OFF .. .. .. .. ....", "both of Bass's note columns");
            assert_eq!(grid.tracks[4].cells[1], "--- .. .. .. .. 0A40 ....", "Mix's two effect columns");

            let Some(FocusBody::Matrix(matrix)) = renoise.focus_body(&BodyRequest::Matrix, Signal::new()).await.unwrap() else {
                panic!("the matrix")
            };
            assert_eq!((matrix.slots.len(), matrix.current, matrix.slots[2].muted.as_slice()), (4, Some(0), &[1][..]));
            let Some(FocusBody::Keyzones(map)) =
                renoise.focus_body(&BodyRequest::Keyzones { item_ref: "1:instrument:1".into() }, Signal::new()).await.unwrap()
            else {
                panic!("the keyzones")
            };
            assert_eq!((map.name.as_str(), map.zones.len(), map.macros[0].0.as_str()), ("Kit", 2, "Cutoff"));
            let Some(FocusBody::Tree(tree)) =
                renoise.focus_body(&BodyRequest::Tree { track_ref: "1:track:1".into() }, Signal::new()).await.unwrap()
            else {
                panic!("the DSP chain")
            };
            assert_eq!(tree.devices.iter().map(|d| d.name.as_str()).collect::<Vec<_>>(), ["EQ 5", "Doofer"]);
            assert_eq!(tree.devices[1].can_have_chains, Some(true), "a Doofer holds devices");
            assert!(renoise.can_show(BodyKind::Tracker) && !renoise.can_show(BodyKind::Timeline));

            // A track inserted above Kick: the tool says so (`tracks`), the host passes it on, and track:1 follows Kick to
            // its new place; reads name the layout their refs are at.
            simulator.emit("tracks", json!({"count": 8, "made": [1], "gone": [], "layout": 1}));
            for _ in 0..100 {
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                let _ = read.execute(json!({"kind": "device", "parent": "track:1"}).as_object().unwrap().clone(), Signal::new()).await;
                if script.asked().last().is_some_and(|(_, params)| params["parent"] == "1:track:2") {
                    break;
                }
            }
            let asked = script.asked().last().unwrap().1.clone();
            assert_eq!(asked, json!({"kind": "device", "parent": "1:track:2", "layout": 1}), "track:1 follows Kick");

            // The same shift again (it reached Kumi after the book was past it): nothing moves twice.
            simulator.emit("tracks", json!({"count": 8, "made": [1], "gone": [], "layout": 1}));
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            let _ = read.execute(json!({"kind": "device", "parent": "track:1"}).as_object().unwrap().clone(), Signal::new()).await;
            assert_eq!(script.asked().last().unwrap().1["parent"], "1:track:2", "track:1 is still Kick, not moved again");
            // A gap (layout 3 after 1): the shifts between are unknown, so the track names go.
            simulator.emit("tracks", json!({"count": 10, "made": [1], "gone": [], "layout": 3}));
            let mut last = String::new();
            for _ in 0..200 {
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                last = read
                    .execute(json!({"kind": "device", "parent": "track:1"}).as_object().unwrap().clone(), Signal::new())
                    .await
                    .unwrap()
                    .text;
                if last.contains("isn't one of this turn's refs") {
                    break;
                }
            }
            assert!(last.contains("isn't one of this turn's refs"), "{last}");

            // Events dropped on the way (the host fell behind): a shift among them is unknown, so the track names go and are
            // read again; none is left on the wrong track.
            simulator.emit("events_lost", json!({}));
            // (Until the event lands, the read goes to the simulator, which has nothing scripted for that track's devices.)
            let mut last = String::new();
            for _ in 0..200 {
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                let answer =
                    read.execute(json!({"kind": "device", "parent": "track:1"}).as_object().unwrap().clone(), Signal::new()).await.unwrap();
                last = answer.text;
                if answer.is_error && last.contains("isn't one of this turn's refs") {
                    break;
                }
            }
            assert!(last.contains("isn't one of this turn's refs"), "{last}");

            // Another song opens: the old song's refs are retired, so a stale track:1 reads nothing of the new song.
            script.switch_song();
            let next = renoise.observe(Signal::new(), None).await.unwrap();
            let context: Value = serde_json::from_str(&next.context).unwrap();
            assert_ne!(context["tracks"][0]["ref"], "track:1", "the new song's tracks get names never used before: {context}");
            let stale = next.tools[0]
                .execute(json!({"kind": "device", "parent": "track:1"}).as_object().unwrap().clone(), Signal::new())
                .await
                .unwrap();
            assert!(stale.is_error, "{}", stale.text);
            renoise.close().await.unwrap();
            simulator.stop();
        })
        .await;
}

/// One of the observation's tools, by name.
fn tool(observed: &Observation, name: &str) -> Rc<dyn KernelTool> {
    observed.tools.iter().find(|tool| tool.name() == name).cloned().unwrap_or_else(|| panic!("no {name} tool"))
}

fn input(value: Value) -> JsonObject {
    value.as_object().unwrap().clone()
}

#[tokio::test(flavor = "current_thread")]
async fn reapers_changes_go_through_kumis_host_into_history_and_back_out_as_new_steps() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let home = tempfile::tempdir().unwrap();
            let paths = BridgePaths::new(home.path(), Daw::Reaper);
            let secret = ensure_secret(&paths.secret()).unwrap();
            let script = ReaperScript::new(3, 0);
            let mut config = script.config(secret, "/lab/REAPER");
            config.discovery_dir = Some(paths.discovery());
            let simulator = Simulator::start(config).await.unwrap();
            let endpoint = Rc::new(InProcess { host: Arc::new(Host::new(Daw::Reaper, home.path())), id: Cell::new(0) });
            let connect: Connect = Rc::new(move |_| {
                let endpoint = endpoint.clone();
                async move { Ok(endpoint as Rc<dyn McpEndpoint>) }.boxed_local()
            });
            let told: Rc<RefCell<Vec<ChangeRecord>>> = Rc::default();
            let into = told.clone();
            let mut context = IntegrationContext::new(Rc::new(|_, _| {}));
            context.on_change = Some(Rc::new(move |record| into.borrow_mut().push(record)));
            let reaper = create_reaper_integration(OwnOptions { context, kumi_home: home.path().into(), connect: Some(connect) });
            reaper.start(Signal::new()).await.unwrap();
            let observed = reaper.observe(Signal::new(), None).await.unwrap();
            let tracks: Value = serde_json::from_str::<Value>(&observed.context).unwrap()["tracks"].clone();
            let (first, second) = (tracks[1]["ref"].as_str().unwrap().to_owned(), tracks[2]["ref"].as_str().unwrap().to_owned());
            assert!(observed.instructions.contains("undo_change takes back one of yours as a new step"), "{}", observed.instructions);

            // A plan: a fader, a track added and named by what the step before made, a mute. Each step is one change in
            // HISTORY, in REAPER's words, made through the host's journal.
            let plan = tool(&observed, "make_changes");
            let steps = json!({"steps": [
                {"tool": "track_volume", "input": {"ref": first, "volumeDb": -6}},
                {"tool": "track_add", "as": "pad", "input": {"name": "Pad"}},
                {"tool": "track_rename", "input": {"ref": "@pad", "name": "Pads"}},
                {"tool": "track_mute", "input": {"ref": second, "mute": true}},
            ]});
            within_schema(&plan.input_schema(), &input(steps.clone()));
            let done = plan.execute(input(steps), Signal::new()).await.unwrap();
            assert!(!done.is_error, "{}", done.text);
            assert_eq!(script.volume(1), -6.0);
            assert_eq!((script.track(4)["name"].clone(), script.track(2)["mute"].clone()), (json!("Pads"), json!(true)));
            let titles: Vec<String> = told.borrow().iter().map(|record| record.title.clone()).collect();
            assert_eq!(titles, ["Track 1 volume -6 dB", "Added track Pad", "Renamed Pad to Pads", "Track 2 muted"]);
            assert!(!done.text.contains("r:track:"), "no long ref reaches the model: {}", done.text);
            let undo_steps = script.undo_history();
            assert_eq!(undo_steps.len(), 4, "one named step of REAPER's undo each: {undo_steps:?}");
            assert!(undo_steps.iter().all(|step| step.starts_with("Kumi: ")), "{undo_steps:?}");

            // The next look lists them for the model, each applied.
            let again = reaper.observe(Signal::new(), None).await.unwrap();
            let latest = serde_json::from_str::<Value>(&again.context).unwrap()["kumiChanges"].clone();
            assert_eq!(latest.as_array().map(Vec::len), Some(4), "{latest}");
            assert!(latest.as_array().unwrap().iter().all(|row| row["state"] == "applied"), "{latest}");

            // Undo the last: the name goes back, as a new step of REAPER's undo.
            let undo = tool(&again, "undo_change");
            let undone = undo.execute(input(json!({"change": "last", "final": true})), Signal::new()).await.unwrap();
            assert!(!undone.is_error, "{}", undone.text);
            assert_eq!(undone.reply.as_deref(), Some("Undone: Track 2 muted."));
            assert_eq!(script.track(2)["mute"], json!(false));
            assert_eq!(script.undo_history().len(), 5, "the undo is a new step");

            // The producer moved the fader since: Kumi leaves it, says why, and HISTORY keeps the change.
            script.set_volume(1, -3.0);
            let volume = told.borrow()[0].id.clone();
            let kept = undo.execute(input(json!({"change": volume})), Signal::new()).await.unwrap();
            assert!(kept.is_error && kept.text.contains("changed since Kumi set it"), "{}", kept.text);
            assert_eq!(script.volume(1), -3.0);
            assert_eq!(told.borrow().last().map(|record| record.state), Some(ChangeState::Kept));

            // The producer presses REAPER's undo on the rename: the bridge says so, and HISTORY follows.
            let rename = script.undo_history().iter().find(|step| step.contains("Renamed")).cloned().unwrap();
            simulator.emit("undone_in_daw", json!({"descs": [rename], "how": "undo"}));
            let mut followed = false;
            for _ in 0..100 {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                if told.borrow().iter().any(|record| record.title == "Renamed Pad to Pads" && record.state == ChangeState::Undone) {
                    followed = true;
                    break;
                }
            }
            assert!(followed, "{:?}", told.borrow().iter().map(|r| (&r.title, r.state)).collect::<Vec<_>>());

            // A value it already has: nothing to change, no step and no row.
            let rows = told.borrow().len();
            let same = tool(&again, "track_volume").execute(input(json!({"ref": first, "volumeDb": -3})), Signal::new()).await.unwrap();
            assert!(!same.is_error && same.text.contains("\"unchanged\":true"), "{}", same.text);
            assert_eq!(told.borrow().len(), rows);
            // A ref this turn never gave: refused in Kumi's words, nothing sent.
            let unknown =
                tool(&again, "track_volume").execute(input(json!({"ref": "track:99", "volumeDb": -3})), Signal::new()).await.unwrap();
            assert!(unknown.is_error && unknown.text.contains("isn't one of this turn's refs"), "{}", unknown.text);

            // /undo in the app takes back the latest change still standing.
            let record = reaper.undo(None, Signal::new()).await.unwrap();
            assert_eq!(record.title, "Added track Pad");
            assert_eq!(script.track(3)["name"], json!("Track 3"));
            reaper.close().await.unwrap();
            simulator.stop();
        })
        .await;
}
