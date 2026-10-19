//! The fixed request set over a scene: what the fixture recorder asks the real Logic and what `tests/reads.rs` asks a
//! recorded one, through the same readers (`serve`), so the vectors the host replays are the bridge's own answers.
//! The focus is answered as the server would, from a cache refreshed just before, with its age at zero so the
//! recording is the same every time.

use std::time::Instant;

use serde_json::{json, Value};

use crate::ax::strings::Strings;
use crate::ax::Ax;
use crate::focus::Focus;
use crate::helper::Helper;
use crate::project::Project;
use crate::serve;

/// The shared brief's fixed request set, in its order, with its exact parameters.
pub fn fixed_requests() -> Vec<(&'static str, Value)> {
    vec![
        ("status", json!({})),
        ("read", json!({"kind": "project"})),
        ("read", json!({"kind": "track"})),
        ("read", json!({"kind": "track", "limit": 1})),
        ("read", json!({"kind": "track", "cursor": "1.1"})),
        ("read", json!({"kind": "region"})),
        ("read", json!({"kind": "region", "parent": "1:track:2"})),
        ("read", json!({"kind": "note"})),
        ("read", json!({"kind": "note", "parent": "1:region:1"})),
        ("read", json!({"kind": "plugin", "parent": "1:track:1"})),
        ("read", json!({"kind": "send", "parent": "1:track:1"})),
        ("read", json!({"kind": "marker"})),
        ("read", json!({"kind": "tempo"})),
        ("read", json!({"ref": "1:track:2"})),
        ("read", json!({"ref": "1:track:9"})),
        ("read", json!({"kind": "bogus"})),
        ("read", json!({"kind": "region", "parent": "0:track:2"})),
        ("snapshot", json!({})),
        ("focus", json!({})),
        ("focus_body", json!({"kind": "tree", "ref": "1:track:1"})),
        ("focus_body", json!({"kind": "tree"})),
        ("focus_body", json!({"kind": "notes", "ref": "1:region:1"})),
        ("focus_body", json!({"kind": "notes"})),
        ("focus_body", json!({"kind": "timeline"})),
        ("focus_body", json!({"kind": "timeline", "around": "1:track:2", "window": {"from": 0, "to": 16}, "limit": 1})),
        ("focus_body", json!({"kind": "routing", "ref": "1:track:1"})),
    ]
}

/// A request and the answer the bridge gave, as the vectors file keeps them.
pub fn case(method: &str, params: &Value, answer: &Result<Value, (String, String)>) -> Value {
    let response = match answer {
        Ok(result) => json!({"ok": true, "result": result}),
        Err((code, message)) => json!({"ok": false, "error": {"code": code, "message": message}}),
    };
    json!({"request": {"method": method, "params": params}, "response": response})
}

/// Runs the fixed request set over a helper (the real one, recorded, or a scene replayed) and a status to answer
/// `status` with; the project reader is the bridge's own.
pub fn run(helper: &mut dyn Helper, pid: u32, strings: Strings, status: Value, identity: (Option<&str>, Option<&str>, &str)) -> Vec<Value> {
    let mut project = Project::new(strings);
    project.set_identity(identity.0, identity.1, identity.2);
    let mut cases = vec![];
    for (method, params) in fixed_requests() {
        let answer = match method {
            "status" => Ok(status.clone()),
            "focus" => {
                let mut ax = Ax::new(helper, pid);
                match project.look(&mut ax, None) {
                    Ok((look, _, _)) => {
                        let mut focus = Focus::default();
                        focus.update(look);
                        focus.refreshed = Some(Instant::now());
                        let mut answer = focus.to_json();
                        answer["ageMs"] = 0.into();
                        Ok(answer)
                    }
                    Err(error) => Err(Project::fault(&error)),
                }
            }
            _ => serve::answer(&mut project, helper, pid, method, &params),
        };
        cases.push(case(method, &params, &answer));
    }
    cases
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_fixed_request_set_is_the_briefs_in_its_order() {
        let requests = fixed_requests();
        assert_eq!(requests.len(), 26);
        assert_eq!(requests[0].0, "status");
        assert_eq!(requests[7], ("read", json!({"kind": "note"})));
        assert_eq!(requests[25], ("focus_body", json!({"kind": "routing", "ref": "1:track:1"})));
        let ok = case("read", &json!({"kind": "track"}), &Ok(json!({"items": []})));
        assert_eq!(ok["response"]["ok"], true);
        let refused = case("read", &json!({"kind": "bogus"}), &Err(("bad-params".into(), "no".into())));
        assert_eq!(refused["response"], json!({"ok": false, "error": {"code": "bad-params", "message": "no"}}));
    }
}
