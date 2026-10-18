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
            let lines = read
                .execute(json!({"kind": "line", "parent": "pattern:1", "track": 1}).as_object().unwrap().clone(), Signal::new())
                .await
                .unwrap();
            assert!(!lines.is_error && lines.text.contains("D#4"), "{}", lines.text);

            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            let focus = seen.borrow().iter().flatten().next().cloned().expect("the focus, as the host reads it");
            assert_eq!((focus.track_ref.as_deref(), focus.device.as_deref()), (Some("1:track:1"), Some("Doofer")));
            assert_eq!(focus.item.as_ref().and_then(|item| item.item_ref.as_deref()), Some("1:pattern:1"));
            assert_eq!((focus.editor_view.as_ref(), focus.panes.as_ref(), focus.section_follows), (None, None, None), "neither pane open");

            let Some(FocusBody::Tracker(grid)) = renoise.focus_body(&BodyRequest::Tracker { item_ref: None }, Signal::new()).await.unwrap()
            else {
                panic!("the pattern")
            };
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
            renoise.close().await.unwrap();
            simulator.stop();
        })
        .await;
}
