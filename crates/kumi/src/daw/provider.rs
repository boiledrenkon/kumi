//! What the app needs of a DAW it works with: finding Kumi's bridge for it, making the session's integration with the
//! app's neutral context, watching for the DAW to answer, and chatting without it in its words.
use futures::future::LocalBoxFuture;
use kumi_runtime::{
    core::{
        context::IntegrationContext,
        contracts::{ConnectionState, Integration},
        daw::{DawId, DawProfile},
        presentation::ToolPresentation,
        view::ViewPresentation,
    },
    integrations::ProjectStore,
    system::Env,
};
use std::rc::Rc;

use super::ableton::AbletonFactory;

/// Makes each registered DAW's integration from its options: the app's own, or a test's stand-in.
#[derive(Clone)]
pub struct DawFactory {
    pub ableton: AbletonFactory,
}

impl DawFactory {
    /// Each DAW's production integration.
    pub fn native() -> Self {
        DawFactory { ableton: Rc::new(kumi_runtime::integrations::create_ableton_integration) }
    }
}

impl From<AbletonFactory> for DawFactory {
    fn from(ableton: AbletonFactory) -> Self {
        DawFactory { ableton }
    }
}

/// What the app hands a DAW's provider for the session's integration, beside the neutral context.
#[derive(Clone)]
pub struct DawSession {
    pub env: Env,
    /// Where the DAW's bridge says how to reach it.
    pub bridge_config: String,
    /// Where saved projects' ids and baselines are kept.
    pub project_store: Rc<dyn ProjectStore>,
    pub factory: DawFactory,
}

/// What the app says while it watches for the DAW to answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WatchWords {
    /// The DAW is open but hasn't answered yet.
    pub starting: &'static str,
    /// Kumi's bridge can't reach the DAW: how to make it answer.
    pub unreachable: &'static str,
    /// It answered, and the session connected.
    pub answered: &'static str,
}

pub trait DawProvider {
    fn id(&self) -> DawId;
    fn profile(&self) -> &'static DawProfile {
        kumi_runtime::integrations::profile(self.id()).expect("a provider's DAW is registered")
    }
    fn presentation(&self) -> &'static ToolPresentation {
        kumi_runtime::integrations::presentation(self.id()).expect("a provider's DAW is registered")
    }
    fn view(&self) -> &'static ViewPresentation {
        kumi_runtime::integrations::view(self.id()).expect("a provider's DAW is registered")
    }
    /// Where Kumi's bridge for the DAW says how to reach it, when the bridge is installed.
    fn bridge_config(&self, env: &Env) -> Option<String>;
    /// The version of the bridge this Kumi brings for the DAW.
    fn bundled_bridge_version(&self) -> Option<String>;
    /// The version of the bridge installed where a configuration says.
    fn installed_bridge_version(&self, bridge_config: &str) -> Option<String>;
    /// Whether the DAW's bridge answers now.
    fn bridge_answers(&self, bridge_config: &str) -> LocalBoxFuture<'static, bool>;
    /// Whether the DAW is open.
    fn app_open(&self, env: &Env) -> LocalBoxFuture<'static, bool>;
    fn watch_words(&self) -> WatchWords;
    /// The session's integration with the DAW: the app's neutral context, and what the DAW finds for itself.
    fn integration(&self, context: IntegrationContext, session: &DawSession) -> Rc<dyn Integration>;
    /// For the doctor: whether Kumi reaches the DAW through a bridge configuration, and what it says.
    fn probe(&self, bridge_config: String, factory: &DawFactory) -> LocalBoxFuture<'static, crate::doctor::LiveProbe>;
    /// Chatting without the DAW, in its words.
    fn without_daw(&self, on_connection: Rc<dyn Fn(ConnectionState)>) -> Rc<dyn Integration>;
}
