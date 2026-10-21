//! The DAWs this Kumi works with (Live, the only one registered so far), and the one this run works with: what the
//! app says of the DAW and shows of its tools, its focus and its devices comes from here, and each DAW's provider
//! finds its bridge and makes the session's integration.
pub mod ableton;
pub mod own;
pub mod provider;

pub use provider::{
    BridgeCommand, BridgeRollbackStep, BridgeState, DawFactory, DawProvider, DawSession, EnableStep, Installed, OwnHostFactory,
    RemovedBridge, Toggle, ToggleWords, WatchWords,
};

use kumi_runtime::{
    core::{
        daw::{DawId, DawProfile},
        presentation::ToolPresentation,
        view::ViewPresentation,
    },
    system::Env,
};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::{Duration, Instant};

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

/// The DAWs with a bridge in that run now beside `chosen`: a session that chose among them says so, and how to switch.
pub fn also_running(chosen: DawId, env: &Env, running: &dyn Fn(&dyn DawProvider) -> bool) -> Vec<&'static dyn DawProvider> {
    let installed: Vec<&'static dyn DawProvider> =
        providers().into_iter().filter(|provider| provider.bridge_config(env).is_some()).collect();
    if installed.len() < 2 {
        return vec![];
    }
    installed.into_iter().filter(|provider| provider.id() != chosen && running(*provider)).collect()
}

/// What a session says when it chose `chosen` while `others` run too.
pub fn also_running_words(chosen: DawId, others: &[&'static dyn DawProvider], env: &Env) -> Option<String> {
    let first = others.first()?;
    let names: Vec<&str> = others.iter().map(|provider| provider.profile().words.app).collect();
    let chosen_name = provider(chosen)?.profile().words.app;
    let why = if last_used(env) == Some(chosen) { ", the one you used last" } else { "" };
    let (open, switch) = match names.as_slice() {
        [one] => (format!("{one} is open too"), format!("/daw {} switches to {one}", command_name(first.id()))),
        many => (format!("{} are open too", many.join(" and ")), "/daw <name> switches".into()),
    };
    Some(format!("{open}: Kumi works with {chosen_name}{why}. {switch}."))
}

/// How long one process listing answers for every DAW: a start (choosing among several bridges, then saying which
/// others are open) lists the processes once.
const LISTING_MS: u64 = 2_000;

thread_local! {
    static LISTING: RefCell<Option<(Instant, Rc<Vec<(u32, String)>>)>> = const { RefCell::new(None) };
    static LISTINGS: Cell<u64> = const { Cell::new(0) };
}

/// The processes running now (pid and name), from one listing (`ps`, `tasklist` on Windows) that every DAW's
/// `running_now` matches against; a listing is reused for a moment, then made again.
pub fn processes() -> Rc<Vec<(u32, String)>> {
    if let Some((at, listed)) = LISTING.with(|listing| listing.borrow().clone()) {
        if at.elapsed() < Duration::from_millis(LISTING_MS) {
            return listed;
        }
    }
    let output = if cfg!(windows) {
        std::process::Command::new("tasklist").args(["/FO", "CSV", "/NH"]).output()
    } else {
        std::process::Command::new("ps").args(["-axo", "pid=,comm="]).output()
    };
    LISTINGS.with(|count| count.set(count.get() + 1));
    let listed = Rc::new(
        output
            .map(|output| daw_bridge::detect::parse_processes(&String::from_utf8_lossy(&output.stdout), cfg!(windows)))
            .unwrap_or_default(),
    );
    LISTING.with(|listing| *listing.borrow_mut() = Some((Instant::now(), listed.clone())));
    listed
}

/// How many process listings this Kumi has made (`processes`).
pub fn process_listings() -> u64 {
    LISTINGS.with(Cell::get)
}

/// A DAW as `/daw` takes it: "live", "reaper", "renoise".
pub fn command_name(id: DawId) -> String {
    provider(id).map(|provider| provider.profile().words.app.to_lowercase()).unwrap_or_else(|| id.as_str().into())
}

/// What `/daw` does: says which DAW Kumi works with, or opens Kumi again with another.
#[derive(Debug, PartialEq, Eq)]
pub enum DawCommand {
    Say(String),
    Switch(DawId, String),
}

/// `/daw` with `typed` after it ("", "renoise", "live").
pub fn daw_command(typed: &str) -> DawCommand {
    let current = current().id();
    let app = |id: DawId| provider(id).map(|provider| provider.profile().words.app).unwrap_or_default();
    let ids = providers().map(|provider| provider.id());
    let word = typed.trim().to_lowercase();
    if word.is_empty() {
        let others: Vec<DawId> = ids.into_iter().filter(|id| *id != current).collect();
        let commands: Vec<String> = others.iter().map(|id| format!("/daw {}", command_name(*id))).collect();
        let names: Vec<&str> = others.iter().map(|id| app(*id)).collect();
        return DawCommand::Say(format!(
            "Kumi works with {}. {} opens Kumi again with {}.",
            app(current),
            commands.join(" or "),
            names.join(" or ")
        ));
    }
    let Some(asked) = ids.into_iter().find(|id| command_name(*id) == word || id.as_str() == word) else {
        let commands: Vec<String> = ids.iter().map(|id| format!("/daw {}", command_name(*id))).collect();
        return DawCommand::Say(format!("Kumi doesn't know {}: {}.", typed.trim(), commands.join(", ")));
    };
    if asked == current {
        return DawCommand::Say(format!("Kumi works with {} already.", app(current)));
    }
    DawCommand::Switch(asked, format!("Kumi closes and opens again with {}.", app(asked)))
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

/// The DAW's words for what the app says ("Live", "Set", "Sets"): Live's for Live, so its screens keep their bytes.
pub fn words() -> &'static kumi_runtime::core::daw::DawWords {
    &profile().words
}

/// Whether the doctor speaks of `provider` on this computer: Live's checks always, unless the producer uses other DAWs
/// alone (Live's bridge isn't in, and another DAW's is); another DAW's say nothing until its folders are there.
pub fn in_use(provider: &dyn DawProvider, env: &Env) -> bool {
    provider.id() != DawId::Ableton
        || provider.bridge_installed(env)
        || !providers().into_iter().any(|other| other.id() != DawId::Ableton && other.bridge_installed(env))
}

/// The DAWs the producer works with here, by name, for the doctor's heading: those whose bridge is in (Live when none
/// is).
pub fn used_names(env: &Env) -> String {
    let names: Vec<&str> = providers()
        .into_iter()
        .filter(|provider| provider.bridge_installed(env) || (provider.id() == DawId::Ableton && in_use(*provider, env)))
        .map(|provider| provider.profile().words.app)
        .collect();
    if names.is_empty() {
        "Live".into()
    } else {
        names.join(", ")
    }
}

/// Where the DAW is downloaded from, as setup names it.
pub fn site() -> &'static str {
    match current().id() {
        DawId::Reaper => "reaper.fm",
        DawId::Renoise => "renoise.com",
        _ => "ableton.com",
    }
}

/// A word as a sentence starts it: "Set", "Project", "Song".
pub fn capitalized(word: &str) -> String {
    let mut chars = word.chars();
    chars.next().map(|first| first.to_uppercase().chain(chars).collect()).unwrap_or_default()
}

/// How the app shows the DAW's own tools.
pub fn presentation() -> &'static ToolPresentation {
    current().presentation()
}

/// How the app reads the DAW's focus and devices.
pub fn view() -> &'static ViewPresentation {
    current().view()
}
