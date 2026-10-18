//! Live as the app works with it: its bridge (the Remote Script) and its configuration, the session's integration
//! with what only Live has (its User Library, the Set opener, whether Live runs, Willington's switch), and the words
//! for watching it answer.
use super::{bridge_setup, live_app, willington::Willington};
use crate::daw::provider::{DawFactory, DawProvider, DawSession, WatchWords};
use crate::doctor::LiveProbe;
use futures::{future::LocalBoxFuture, FutureExt};
use kumi_common::abort;
use kumi_runtime::RuntimeError;
use kumi_runtime::{
    core::{
        context::IntegrationContext,
        contracts::{ConnectionState, Integration},
        daw::DawId,
    },
    integrations::ableton::AbletonOptions,
    system::Env,
};
use std::rc::Rc;

/// Live's provider.
pub struct Ableton;

/// What Kumi says while an open Live hasn't answered yet.
pub(crate) const LIVE_STARTING: &str = "Live is open; Kumi connects once it answers.";
/// What Kumi says when its bridge can't reach Live.
pub(crate) const NO_LIVE: &str = "Kumi's bridge couldn't reach Live, so this is chat without Live. Open Live and choose AbletonMcpBridge as a Control Surface (Settings → Link, Tempo & MIDI); if Live is showing a dialog, answer it. Kumi connects by itself once Live answers.";

impl DawProvider for Ableton {
    fn id(&self) -> DawId {
        DawId::Ableton
    }
    fn bridge_config(&self, env: &Env) -> Option<String> {
        crate::config::find_bridge_config(env)
    }
    fn bundled_bridge_version(&self) -> Option<String> {
        bridge_setup::bundled_bridge_version()
    }
    fn installed_bridge_version(&self, bridge_config: &str) -> Option<String> {
        crate::doctor::read_bridge_server(bridge_config).ok().and_then(|s| s.version)
    }
    fn bridge_answers(&self, bridge_config: &str) -> LocalBoxFuture<'static, bool> {
        let config = bridge_config.to_string();
        async move { bridge_setup::remote_script_answers(&config).await }.boxed_local()
    }
    fn app_open(&self, env: &Env) -> LocalBoxFuture<'static, bool> {
        let env = env.clone();
        async move { live_app::open_live(&bridge_setup::default_run(), kumi_runtime::system::platform(), &env).await.is_some() }
            .boxed_local()
    }
    fn watch_words(&self) -> WatchWords {
        WatchWords { starting: LIVE_STARTING, unreachable: NO_LIVE, answered: "Live answered, so Kumi is connected to it now." }
    }
    fn integration(&self, context: IntegrationContext, session: &DawSession) -> Rc<dyn Integration> {
        let env = session.env.clone();
        let mut options = AbletonOptions::with_context(context);
        options.bridge_config = Some(session.bridge_config.clone());
        options.project_store = Some(session.project_store.clone());
        options.user_library = crate::config::live_user_library(&env);
        // A kept conversation's Set opens in Live as a double-click would.
        options.open_file = Some({
            let env = env.clone();
            Rc::new(move |set| {
                let env = env.clone();
                async move { live_app::open_set_file(&bridge_setup::default_run(), kumi_runtime::system::platform(), &env, &set).await }
                    .boxed_local()
            })
        });
        // With Live still running, losing it means it's opening another Set, not that it closed (#188).
        options.live_running = Some(Rc::new(|| async { bridge_setup::is_live_running(bridge_setup::default_run()).await }.boxed_local()));
        // How Willington's bindings stand, read again for each new model session.
        options.willington = Some(Rc::new(move || Willington::find(&env).map(|willington| willington.switch())));
        (session.factory.ableton)(options)
    }
    fn probe(&self, bridge_config: String, factory: &DawFactory) -> LocalBoxFuture<'static, LiveProbe> {
        probe_live(bridge_config, factory.ableton.clone()).boxed_local()
    }
    fn without_daw(&self, on_connection: Rc<dyn Fn(ConnectionState)>) -> Rc<dyn Integration> {
        kumi_runtime::integrations::create_inference_only_integration(on_connection)
    }
}

/// Starts Live's integration on a bridge configuration and looks once, for the doctor.
async fn probe_live(config: String, factory: super::AbletonFactory) -> LiveProbe {
    let mut options = AbletonOptions::new(Rc::new(|_, _| {}));
    options.bridge_config = Some(config);
    let integration = factory(options);
    if let Err(error) = integration.start(abort::timeout(20000)).await {
        let _ = integration.close().await;
        return LiveProbe { started: false, said: Some(error.to_string()), ..Default::default() };
    }
    let result = async {
        let observation = integration.observe(abort::timeout(20000), None).await?;
        let context: serde_json::Value = serde_json::from_str(&observation.context).map_err(|e| RuntimeError::plain(e.to_string()))?;
        if context["mode"] == "inference-only" || observation.tools.is_empty() {
            return Ok::<_, RuntimeError>(LiveProbe { started: true, connected: Some(false), ..Default::default() });
        }
        Ok(LiveProbe {
            started: true,
            connected: Some(true),
            live_version: context["liveVersion"].as_str().map(str::to_string),
            set: context["set"]["name"].as_str().map(str::to_string),
            real_live: Some(context["provenance"] == "real-live"),
            said: None,
        })
    }
    .await;
    let _ = integration.close().await;
    result.unwrap_or(LiveProbe { started: true, connected: Some(false), ..Default::default() })
}
