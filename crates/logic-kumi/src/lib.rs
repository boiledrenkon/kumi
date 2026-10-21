//! Kumi's Logic Pro bridge. Logic has no plug-in API, so the bridge is a process beside Kumi: it reaches Logic through
//! the `kumi-logic-hands` helper, a child speaking JSON lines that holds the macOS channels (Accessibility, key events,
//! MIDI), and serves Kumi's bridge wire (`kumi-daw/1`, daw-bridge) on loopback as the REAPER extension and the Renoise
//! tool do from inside their DAWs.
//!
//! M1, "bridges talk": the bridge finds Logic, attaches to it, follows the open project's identity through Logic's
//! windows, answers `ping` and `status`, sends `project` events, keeps its discovery file and goes when Logic does. The
//! rows, focus, bodies and actions of `docs/en/LOGIC_CONTRACT.md` come with M2; the changes, their undo and the
//! producer's own undo named back with M3 (`changes`); hearing Logic play, auditioning and rendering with M5
//! (`listen`).

pub mod ax;
pub mod bodies;
pub mod bridge;
pub mod changes;
pub mod disk;
pub mod fixtures;
pub mod focus;
pub mod helper;
pub mod keys;
pub mod listen;
pub mod log;
pub mod logic;
pub mod mcu;
pub mod project;
pub mod queue;
pub mod read;
pub mod refs;
pub mod rows;
pub mod scenes;
pub mod serve;
pub mod server;

pub const BRIDGE_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Runs `f`, turning a panic into `fallback` and a log line: for every thread the bridge starts, so one connection's
/// trouble never takes the process down.
pub fn guard<T>(edge: &str, fallback: T, f: impl FnOnce() -> T) -> T {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)) {
        Ok(value) => value,
        Err(payload) => {
            let why = payload.downcast_ref::<&str>().map(|s| s.to_string()).or_else(|| payload.downcast_ref::<String>().cloned());
            log::line(&format!("panic in {edge}: {}", why.unwrap_or_default()));
            fallback
        }
    }
}
