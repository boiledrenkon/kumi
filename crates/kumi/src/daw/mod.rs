//! The DAWs this Kumi works with (Live, the only one registered so far), and the one this run works with: what the
//! app says of the DAW and shows of its tools, its focus and its devices comes from here, and each DAW's provider
//! finds its bridge and makes the session's integration.
pub mod ableton;
pub mod own;
pub mod provider;

pub use provider::{
    BridgeCommand, BridgeRollbackStep, BridgeState, DawFactory, DawProvider, DawSession, EnableStep, Installed, RemovedBridge, Toggle,
    ToggleWords, WatchWords,
};

use kumi_runtime::{
    core::{
        daw::{DawId, DawProfile},
        presentation::ToolPresentation,
        view::ViewPresentation,
    },
    system::Env,
};
use std::cell::Cell;

/// Each registered DAW's provider: Live's first, then the DAWs whose bridges are Kumi's own.
pub fn providers() -> [&'static dyn DawProvider; 3] {
    [&ableton::provider::Ableton, &own::REAPER, &own::RENOISE]
}

/// A registered DAW's provider; None for one this Kumi doesn't integrate.
pub fn provider(id: DawId) -> Option<&'static dyn DawProvider> {
    providers().into_iter().find(|provider| provider.id() == id)
}

/// The DAW a session works with, and where Kumi's bridge for it says how to reach it: the one asked for (with --daw
/// or KUMI_DAW); else the first registered DAW whose bridge is installed (Live first); else, chatting without a DAW,
/// Live's words: Live stays the DAW Kumi starts with.
pub fn chosen(asked: Option<DawId>, env: &Env) -> (&'static dyn DawProvider, Option<String>) {
    if let Some(provider) = asked.and_then(provider) {
        return (provider, provider.bridge_config(env));
    }
    for provider in providers() {
        if let Some(config) = provider.bridge_config(env) {
            return (provider, Some(config));
        }
    }
    (&ableton::provider::Ableton, None)
}

thread_local! {
    static CURRENT: Cell<DawId> = const { Cell::new(DawId::Ableton) };
}

/// Sets the DAW this run works with (Live until a session says otherwise).
pub fn work_with(id: DawId) {
    CURRENT.with(|current| current.set(id));
}

/// The DAW this run works with.
pub fn current() -> &'static dyn DawProvider {
    provider(CURRENT.with(Cell::get)).expect("the DAW worked with is registered")
}

/// The DAW the app works with.
pub fn profile() -> &'static DawProfile {
    current().profile()
}

/// How the app shows the DAW's own tools.
pub fn presentation() -> &'static ToolPresentation {
    current().presentation()
}

/// How the app reads the DAW's focus and devices.
pub fn view() -> &'static ViewPresentation {
    current().view()
}
