//! Which of Live's tools every request carries. The rest wait behind find_tools (kernel/deferred.rs), so a
//! request stays small: changes and actions are core where changes.json or actions.json say so, the other
//! tools where `CORE` lists them, picked by what producers' conversations call most. A change or action
//! that isn't core is still a make_changes step, and make_changes' description gives each one's input.

use super::actions::ACTIONS;
use super::changes::CHANGES;
use crate::core::contracts::JsonObject;
use kumi_common::js::string::{head, trim};

/// Live's tools besides changes and actions that every request carries.
pub const CORE: &[&str] = &[
    "live_discover",
    "live_browser_search",
    "live_song_state",
    "make_changes",
    "undo_change",
    "run_python",
    "find_sounds",
    "read_notes",
    "audition",
];

/// The tool that runs changes and actions as steps.
pub const PLAN_TOOL: &str = "make_changes";

/// Whether a Live tool waits behind find_tools instead of going with every request.
pub fn deferred(name: &str) -> bool {
    if let Some(kind) = CHANGES.iter().find(|kind| kind.tool == name) {
        return !kind.core;
    }
    if let Some(kind) = ACTIONS.iter().find(|kind| kind.tool == name) {
        return !kind.core;
    }
    !CORE.contains(&name)
}

/// Whether a Live tool is a change or an action: a step make_changes can run.
pub fn step(name: &str) -> bool {
    CHANGES.iter().any(|kind| kind.tool == name) || ACTIONS.iter().any(|kind| kind.tool == name)
}

/// The most of a step's line, and of what it does: its fields come before more words about it.
const LINE: usize = 170;
const WHAT: usize = 72;

/// One line a step that isn't a tool of its own gets in make_changes' description: its name, what it does, and
/// the fields its input takes (`?` after one it can leave out), from its own schema.
pub fn step_line(name: &str, description: &str, schema: &JsonObject) -> String {
    let mut what = clause(description);
    if what.len() > WHAT {
        let cut = head(&what, WHAT);
        what = format!("{}…", cut.rfind(' ').filter(|at| *at > WHAT / 2).map_or(cut.as_str(), |at| &cut[..at]));
    }
    let fields = crate::kernel::schema::fields(schema);
    let line = if fields.is_empty() { format!("{name}: {what}") } else { format!("{name}: {what} ({})", fields.join(", ")) };
    if line.len() > LINE {
        let cut = head(&line, LINE - 1);
        format!("{}…)", cut.trim_end_matches([',', ' ', '?']).trim_end_matches(|c: char| c.is_alphanumeric() || c == '_'))
    } else {
        line
    }
}

/// What a tool does, from its description: up to the end of its first sentence or clause.
fn clause(description: &str) -> String {
    let text = trim(description);
    let end = [". ", ": ", "; ", " (", " — "].iter().filter_map(|stop| text.find(stop)).min().unwrap_or(text.len());
    trim(text[..end].trim_end_matches('.')).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn core_changes_and_actions_come_from_their_files_and_the_rest_wait() {
        for kind in CHANGES.iter() {
            assert_eq!(deferred(&kind.tool), !kind.core, "{}", kind.tool);
            assert!(step(&kind.tool));
        }
        assert!(!deferred("set_device_parameter") && !deferred("play"));
        assert!(deferred("set_scale") && deferred("select"));
        assert!(!deferred("live_discover") && !deferred("make_changes"));
        assert!(deferred("judge") && deferred("live_key_estimate") && deferred("arrange"));
        assert!(!step("judge") && !step("make_changes"));
    }

    #[test]
    fn a_steps_line_names_its_fields_required_first() {
        let schema = json!({"type":"object","required":["trackRef"],"properties":{
            "enabled":{"type":"boolean"},"trackRef":{"type":"string"},"root":{"type":"integer"}
        }});
        assert_eq!(
            step_line("set_scale", "Set Scale Mode and its root. More words.", schema.as_object().unwrap()),
            "set_scale: Set Scale Mode and its root (trackRef, enabled?, root?)"
        );
        assert_eq!(step_line("wait", "Wait.", &JsonObject::new()), "wait: Wait");
        let long = step_line("edit", &"word ".repeat(80), schema.as_object().unwrap());
        assert!(long.starts_with("edit: word word") && long.ends_with("… (trackRef, enabled?, root?)"), "{long}");
        let many: serde_json::Map<String, serde_json::Value> = (0..40).map(|n| (format!("field{n}"), json!({"type": "number"}))).collect();
        let wide = step_line("wide", "Many fields.", &json!({"type":"object","properties":many}).as_object().unwrap().clone());
        assert!(wide.len() <= LINE + 4 && wide.ends_with("…)") && !wide.contains("?…"), "{wide}");
    }
}
