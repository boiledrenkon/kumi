//! REAPER's and Renoise's integrations over Kumi's host, with a stand-in host that answers as the M2 contract says:
//! the observation carries the project as the DAW names it, its tracks in short refs, and the conversation's place;
//! the model's read pages in short refs and passes them back long; a DAW that isn't there is said in its words.
use async_trait::async_trait;
use futures::FutureExt;
use kumi_common::abort::Signal;
use kumi_runtime::core::{context::IntegrationContext, contracts::*, errors::RuntimeError};
use kumi_runtime::integrations::{
    own_bridge::{Connect, OwnOptions},
    reaper::create_reaper_integration,
    renoise::create_renoise_integration,
};
use kumi_runtime::mcp::client::{McpEndpoint, StderrStatus};
use kumi_runtime::mcp::types::{CallToolResult, Implementation, ListToolsResult};
use serde_json::{json, Value};
use std::{cell::RefCell, rc::Rc};

/// A host that answers each tool from a table, and remembers what it was asked.
struct Host {
    answers: RefCell<Vec<(&'static str, Value, bool)>>,
    asked: RefCell<Vec<(String, Value)>>,
}
#[async_trait(?Send)]
impl McpEndpoint for Host {
    fn pid(&self) -> Option<u32> {
        None
    }
    fn server_info(&self) -> Option<Implementation> {
        None
    }
    async fn list(&self, _: Option<&str>, _: Signal) -> Result<ListToolsResult, RuntimeError> {
        Ok(serde_json::from_value(json!({"tools": []})).unwrap())
    }
    async fn call(&self, name: &str, args: JsonObject, _: Signal) -> Result<CallToolResult, RuntimeError> {
        self.asked.borrow_mut().push((name.into(), Value::Object(args)));
        let answers = self.answers.borrow();
        let (_, answer, error) = answers.iter().rev().find(|(tool, _, _)| *tool == name).cloned().expect("an answer for each tool asked");
        Ok(serde_json::from_value(json!({"content": [{"type": "text", "text": answer.to_string()}], "isError": error})).unwrap())
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
        Ok(())
    }
}

fn options(host: Rc<Host>, states: Rc<RefCell<Vec<ConnectionState>>>) -> OwnOptions {
    let connect: Connect = Rc::new(move |_| {
        let host = host.clone();
        async move { Ok(host as Rc<dyn McpEndpoint>) }.boxed_local()
    });
    OwnOptions {
        context: IntegrationContext::new(Rc::new(move |state, _| states.borrow_mut().push(state))),
        kumi_home: std::env::temp_dir(),
        connect: Some(connect),
    }
}

const PROJECT_ID: &str = "0123456789abcdef0123456789abcdef";
const BASS: &str = "r:track:{8A5D2C1E-0000-4000-8000-000000000002}";

fn reaper_snapshot() -> Value {
    json!({
        "epoch": "1",
        "project": {"id": PROJECT_ID, "identity": "4242:0x7f01", "name": "Night Drive", "tempo": 124, "timeSignature": [4, 4]},
        "transport": {"playing": false, "position": 0},
        "tracks": [
            {"ref": "r:track:{8A5D2C1E-0000-4000-8000-000000000001}", "index": 1, "name": "Drums", "depth": 0, "folder": 1},
            {"ref": BASS, "index": 2, "name": "Bass", "depth": 0, "folder": 0, "selected": true},
        ],
        "selection": {"trackRef": BASS},
    })
}

#[tokio::test(flavor = "current_thread")]
async fn reapers_observation_is_its_project_in_short_refs_at_its_place() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let host = Rc::new(Host {
                answers: RefCell::new(vec![("status", json!({"connected": true}), false), ("snapshot", reaper_snapshot(), false)]),
                asked: RefCell::default(),
            });
            let states = Rc::new(RefCell::new(vec![]));
            let reaper = create_reaper_integration(options(host.clone(), states.clone()));
            reaper.start(Signal::new()).await.unwrap();
            assert_eq!(*states.borrow(), [ConnectionState::Connecting, ConnectionState::Connected]);
            let seen = reaper.observe(Signal::new(), None).await.unwrap();
            assert_eq!(seen.label, "REAPER — Night Drive");
            assert_eq!(seen.project.as_ref().map(|p| p.id.as_str()), Some(format!("reaper-{PROJECT_ID}").as_str()));
            assert_eq!(seen.set.as_deref(), Some("4242:0x7f01"));
            assert_eq!(seen.tracks.as_deref(), Some(&["Drums".to_string(), "Bass".to_string()][..]));
            let context: Value = serde_json::from_str(&seen.context).unwrap();
            assert_eq!(context["daw"], "REAPER");
            assert_eq!(context["project"]["name"], "Night Drive");
            assert_eq!(context["tracks"][1]["ref"], "track:2");
            assert_eq!(context["selection"]["trackRef"], "track:2");
            assert!(!seen.context.contains("{8A5D"), "no GUID reaches the model: {}", seen.context);
            assert_eq!(seen.tools.iter().map(|tool| tool.name().to_string()).collect::<Vec<_>>(), ["reaper_read"]);

            // The model reads the Bass track's items in short refs; the host is asked in long ones.
            host.answers.borrow_mut().push((
                "read",
                json!({"items": [{"ref": "r:item:{FF00-1}", "position": 4.0, "length": 8.0, "name": "Bass DI", "takeRef": "r:take:{FF00-2}"}], "epoch": "1"}),
                false,
            ));
            let read = seen.tools[0].clone();
            let page = read.execute(json!({"kind": "item", "parent": "track:2"}).as_object().unwrap().clone(), Signal::new()).await.unwrap();
            assert!(!page.is_error, "{}", page.text);
            assert_eq!(host.asked.borrow().last().unwrap().1, json!({"kind": "item", "parent": BASS}));
            let page: Value = serde_json::from_str(&page.text).unwrap();
            assert_eq!(page["items"][0]["ref"], "item:1");
            assert_eq!(page["items"][0]["takeRef"], "take:1");
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn an_unsaved_project_is_the_dawss_unsaved_place_and_a_daw_not_there_is_said_in_its_words() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let mut snapshot = reaper_snapshot();
            snapshot["project"]["id"] = Value::Null;
            let host = Rc::new(Host {
                answers: RefCell::new(vec![("status", json!({"connected": true}), false), ("snapshot", snapshot, false)]),
                asked: RefCell::default(),
            });
            let states = Rc::new(RefCell::new(vec![]));
            let reaper = create_reaper_integration(options(host.clone(), states.clone()));
            reaper.start(Signal::new()).await.unwrap();
            let seen = reaper.observe(Signal::new(), None).await.unwrap();
            assert!(seen.project.is_none());
            assert_eq!(seen.set.as_deref(), Some("4242:0x7f01"));

            // REAPER quits: the host says so, and Kumi carries on without it, in REAPER's words.
            host.answers.borrow_mut().push((
                "snapshot",
                json!({"error": "no REAPER with Kumi's bridge is running", "connected": false}),
                true,
            ));
            let away = reaper.observe(Signal::new(), None).await.unwrap();
            assert!(away.label.contains("No REAPER access"), "{}", away.label);
            assert!(away.label.contains("no REAPER with Kumi's bridge is running"), "{}", away.label);
            assert!(away.tools.is_empty());
            assert_eq!(states.borrow().last(), Some(&ConnectionState::Disconnected));
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn renoises_refs_are_positions_under_the_songs_epoch_and_a_new_song_retires_them() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let snapshot = |epoch: &str, name: &str| {
                json!({
                    "epoch": epoch,
                    "project": {"id": null, "identity": format!("99:{epoch}"), "name": name, "bpm": 140, "lpb": 4},
                    "transport": {"playing": true},
                    "tracks": [{"ref": format!("{epoch}:track:1"), "index": 1, "name": "Kick", "type": "sequencer"}],
                    "selection": {"trackRef": format!("{epoch}:track:1")},
                })
            };
            let host = Rc::new(Host {
                answers: RefCell::new(vec![("status", json!({"connected": true}), false), ("snapshot", snapshot("3", "Acid"), false)]),
                asked: RefCell::default(),
            });
            let renoise = create_renoise_integration(options(host.clone(), Rc::new(RefCell::new(vec![]))));
            renoise.start(Signal::new()).await.unwrap();
            let seen = renoise.observe(Signal::new(), None).await.unwrap();
            assert_eq!(seen.label, "Renoise — Acid");
            let context: Value = serde_json::from_str(&seen.context).unwrap();
            assert_eq!(context["song"]["bpm"], 140, "Renoise's project is its song");
            assert_eq!(context["tracks"][0]["ref"], "track:1");

            // Another song opens: the old refs are retired, and the new song's tracks get names never used before, so
            // a stale track:1 in the conversation can't read the new song's first track.
            host.answers.borrow_mut().push(("snapshot", snapshot("4", "Breaks"), false));
            let next = renoise.observe(Signal::new(), None).await.unwrap();
            let context: Value = serde_json::from_str(&next.context).unwrap();
            assert_eq!(context["tracks"][0]["ref"], "track:2");
            host.answers.borrow_mut().push(("read", json!({"items": [], "epoch": "4"}), false));
            next.tools[0]
                .execute(json!({"kind": "device", "parent": "track:2"}).as_object().unwrap().clone(), Signal::new())
                .await
                .unwrap();
            assert_eq!(host.asked.borrow().last().unwrap().1["parent"], "4:track:1", "the new name reads the new song's track");
            next.tools[0]
                .execute(json!({"kind": "device", "parent": "track:1"}).as_object().unwrap().clone(), Signal::new())
                .await
                .unwrap();
            assert_eq!(host.asked.borrow().last().unwrap().1["parent"], "track:1", "the retired name reads nothing of the new song");
        })
        .await;
}
