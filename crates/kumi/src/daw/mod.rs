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

/// The DAW a session works with, and where Kumi's bridge for it says how to reach it:
/// 1. the one asked for (with --daw or KUMI_DAW);
/// 2. the only DAW whose bridge is installed (a producer who uses Live alone starts in Live, as always);
/// 3. with several installed: the one running now, else the one used last, else Live, else the first;
/// 4. with none installed, chatting without a DAW in Live's words.
pub fn chosen(asked: Option<DawId>, env: &Env) -> (&'static dyn DawProvider, Option<String>) {
    chosen_with(asked, env, &|provider| provider.running_now())
}

/// `chosen`, with whether each DAW runs now given (a test's answer, or one process listing each).
pub fn chosen_with(
    asked: Option<DawId>,
    env: &Env,
    running: &dyn Fn(&dyn DawProvider) -> bool,
) -> (&'static dyn DawProvider, Option<String>) {
    if let Some(provider) = asked.and_then(provider) {
        return (provider, provider.bridge_config(env));
    }
    let installed: Vec<(&'static dyn DawProvider, String)> =
        providers().into_iter().filter_map(|provider| provider.bridge_config(env).map(|config| (provider, config))).collect();
    let pick = |provider: &'static dyn DawProvider, config: &str| (provider, Some(config.to_string()));
    match installed.as_slice() {
        [] => (&ableton::provider::Ableton, None),
        [(provider, config)] => pick(*provider, config),
        several => {
            let running: Vec<&(&'static dyn DawProvider, String)> = several.iter().filter(|(provider, _)| running(*provider)).collect();
            let among = if running.is_empty() { several.iter().collect::<Vec<_>>() } else { running };
            if let [(provider, config)] = among.as_slice() {
                return pick(*provider, config);
            }
            let last = last_used(env);
            let found = among
                .iter()
                .find(|(provider, _)| Some(provider.id()) == last)
                .or_else(|| among.iter().find(|(provider, _)| provider.id() == DawId::Ableton))
                .unwrap_or(&among[0]);
            pick(found.0, &found.1)
        }
    }
}

/// Where Kumi keeps the DAW a session last worked with.
fn last_used_file(env: &Env) -> std::path::PathBuf {
    std::path::Path::new(&crate::config::kumi_dir(env)).join("last-daw")
}

/// The DAW a session last worked with, when Kumi knows.
pub fn last_used(env: &Env) -> Option<DawId> {
    DawId::parse(std::fs::read_to_string(last_used_file(env)).ok()?.trim())
}

/// Keeps the DAW a session works with, for choosing among several next time. Best effort, and written only when it
/// changes.
pub fn remember_used(id: DawId, env: &Env) {
    if last_used(env) != Some(id) {
        let _ = std::fs::write(last_used_file(env), format!("{}\n", id.as_str()));
    }
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
