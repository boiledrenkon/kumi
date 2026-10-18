//! What the app needs of a DAW it works with: finding Kumi's bridge for it, making the session's integration with the
//! app's neutral context, watching for the DAW to answer, and chatting without it in its words.
use futures::future::LocalBoxFuture;
use kumi_runtime::{
    core::{
        context::IntegrationContext,
        contracts::{ConnectionState, Integration, SessionController},
        daw::{DawId, DawProfile},
        presentation::ToolPresentation,
        view::ViewPresentation,
    },
    integrations::ProjectStore,
    system::Env,
};
use std::{
    cell::RefCell,
    rc::{Rc, Weak},
};

use super::ableton::AbletonFactory;
use crate::doctor::{Check, DoctorIo, InstallationIo};
use crate::tui::app::DawSetup;
use kumi_runtime::RuntimeError;

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

/// What of the DAW is on this computer, read from its files without opening it: whether it can take Kumi's bridge yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Installed {
    /// A version Kumi works with has been opened here, so it has the folders the bridge goes in.
    Ready,
    /// None at all.
    Missing,
    /// Only an older one: the newest one's version ("11.3.13").
    Older(String),
    /// One Kumi works with is installed (this app), but hasn't been opened yet.
    NeverOpened(String),
}

/// The step setup shows when the DAW has to be told to use Kumi's bridge, by the producer, in the DAW (Live's
/// Control Surface): its name, what to do, what it says while it waits, and what it says when put off.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EnableStep {
    pub label: &'static str,
    pub say: &'static str,
    pub waiting: &'static str,
    pub later: &'static str,
    /// What Kumi says when the producer puts it off.
    pub put_off: &'static str,
}

/// What the session says of the DAW's bridge as it starts: in plain mode (`notice`, with the way by hand), and as
/// setup's reason to put it in place (`why`); None for each when the bridge is current.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BridgeState {
    pub notice: Option<String>,
    pub why: Option<String>,
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
    /// The step setup shows for the producer to tell the DAW to use Kumi's bridge; None when it needs none.
    fn enable_step(&self) -> Option<EnableStep>;
    /// The oldest major version of the DAW Kumi works with.
    fn min_major(&self) -> u32;
    /// The session's integration with the DAW: the app's neutral context, and what the DAW finds for itself.
    fn integration(&self, context: IntegrationContext, session: &DawSession) -> Rc<dyn Integration>;
    /// For the doctor: whether Kumi reaches the DAW through a bridge configuration, and what it says.
    fn probe(&self, bridge_config: String, factory: &DawFactory) -> LocalBoxFuture<'static, crate::doctor::LiveProbe>;
    /// What the session says of the DAW's bridge: `missing` when it isn't installed; `found` when a configuration was,
    /// which may be older than this Kumi's (`bundled`); `command` starts Kumi in the way by hand.
    fn bridge_state(&self, env: &Env, found: bool, missing: bool, bundled: Option<&str>, command: &str) -> BridgeState;
    /// The DAW's side of first-run setup: `bridge` is the configuration the session's integration starts from, which
    /// setup sets once the bridge is in place unless it was `given`.
    fn setup(
        &self,
        env: &Env,
        bridge: Rc<RefCell<Option<String>>>,
        given: bool,
        controller: Rc<RefCell<Option<Weak<dyn SessionController>>>>,
    ) -> Rc<dyn DawSetup>;
    /// The DAW's part of `kumi doctor`: its bridge, and whether Kumi reaches the DAW through it (`node` is the doctor's
    /// check of Kumi itself).
    fn doctor_checks<'a>(&'a self, io: &'a DoctorIo, node: &'a Check) -> LocalBoxFuture<'a, Vec<Check>>;
    /// What the DAW needs from this computer, for the doctor's Installation section.
    fn installation_checks<'a>(&'a self, env: &'a Env, io: &'a InstallationIo, platform: &'a str) -> LocalBoxFuture<'a, Vec<Check>>;
    /// What the DAW needs beyond its bridge, later in the doctor (Live: Kumi's hands in its menus).
    fn extra_checks<'a>(&'a self, io: &'a DoctorIo) -> LocalBoxFuture<'a, Result<Vec<Check>, RuntimeError>>;
    /// Chatting without the DAW, in its words.
    fn without_daw(&self, on_connection: Rc<dyn Fn(ConnectionState)>) -> Rc<dyn Integration>;
}
