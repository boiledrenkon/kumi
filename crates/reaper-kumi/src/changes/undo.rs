//! Noticing that the producer undid or redid a change of Kumi's in REAPER. REAPER tells an extension nothing of its own undo
//! (the control surface's callbacks fire for every undo and say nothing of what changed), so the history is looked at: Kumi's
//! points carry a nonce in their description, and a point that moves from the undone side of the history to the applied one,
//! or back, is the producer's Edit > Undo or Redo.
//!
//! The history is a list of points with the project at one of them. The points after it can be redone. When that list gains
//! entries at its front, the producer undid them; when it loses entries from its front, they were redone (the points the
//! project moved over are checked, since a new edit by the producer drops the whole list the same way).

use serde_json::json;

use crate::extension::Outbox;
use crate::reaper::{Project, Reaper};

/// The most redoable points looked at: the history can hold hundreds, and what the producer undid is at its front.
const REDO_SCAN: i32 = 256;
/// The most points looked at behind the one the project is at: what the producer can have undone in the time of a look.
const TAIL_SCAN: i32 = 128;

/// Whether a point's description is one of Kumi's: `Kumi: …` ending in a nonce, `k` and four base-36 characters.
pub fn is_kumi(desc: &str) -> bool {
    let Some(rest) = desc.strip_suffix(')') else { return false };
    let Some(open) = rest.rfind("(k") else { return false };
    let nonce = &rest[open + 2..];
    desc.starts_with("Kumi: ") && nonce.len() == 4 && nonce.bytes().all(|b| b.is_ascii_digit() || b.is_ascii_lowercase())
}

/// The history as last looked at.
#[derive(Clone, Debug, PartialEq, Eq)]
struct View {
    entries: i32,
    at: i32,
    count: i32,
    /// The descriptions of the points up to `at` (the last `TAIL_SCAN` of them), oldest first.
    tail: Vec<String>,
    /// The descriptions of the points after `at`, nearest first.
    redo: Vec<String>,
}

impl View {
    /// The place in the history of `tail`'s first point.
    fn tail_start(&self) -> i32 {
        self.at + 1 - self.tail.len() as i32
    }
}

fn view(reaper: &Reaper, proj: Project) -> Option<View> {
    let (entries, at) = reaper.undo_position(proj)?;
    let count = reaper.state_change_count(proj).unwrap_or(0);
    let first = (at + 1 - TAIL_SCAN).max(0);
    let tail = (first..=at).map(|index| reaper.undo_entry(proj, index).unwrap_or_default()).collect();
    let last = (entries - 1).min(at + REDO_SCAN);
    let redo = (at + 1..=last).map(|index| reaper.undo_entry(proj, index).unwrap_or_default()).collect();
    Some(View { entries, at, count, tail, redo })
}

#[derive(Default)]
pub struct Watch {
    seen: Option<View>,
    epoch: String,
    nudged: bool,
}

impl Watch {
    /// Kumi just made a point of its own (or closed a plan's): it is the new baseline, not the producer's undo.
    pub fn resync(&mut self, reaper: &Reaper, proj: Project) {
        self.seen = view(reaper, proj);
    }

    /// REAPER ran an action: the history is looked at on this tick even if nothing else says it moved.
    pub fn nudge(&mut self) {
        self.nudged = true;
    }

    /// Looks at the history; the producer's undo or redo of Kumi's points is an `undone_in_daw` event.
    pub fn poll(&mut self, reaper: &Reaper, proj: Project, epoch: &str, out: &dyn Outbox) {
        if self.epoch != epoch {
            // Another project in the tab: its history is not this one's to compare with.
            self.epoch = epoch.to_string();
            self.seen = None;
        }
        let nudged = std::mem::take(&mut self.nudged);
        let Some((entries, at)) = reaper.undo_position(proj) else { return };
        let count = reaper.state_change_count(proj).unwrap_or(0);
        if let Some(seen) = &mut self.seen {
            if !nudged && seen.entries == entries && seen.at == at {
                if seen.count == count {
                    return;
                }
                // Bare edits, unless the point the project is at is another one: an undo and a new edit between two looks
                // leave the same number of points with the project at the same place.
                if reaper.undo_entry(proj, at).as_ref() == seen.tail.last() {
                    seen.count = count;
                    return;
                }
            }
        }
        let Some(now) = view(reaper, proj) else { return };
        if let Some(before) = self.seen.take() {
            for (how, descs) in changes(&before, &now) {
                out.event("undone_in_daw", json!({"descs": descs, "how": how, "epoch": epoch}));
            }
        }
        self.seen = Some(now);
    }
}

/// What the producer did to Kumi's points between two looks: `("undo" | "redo", their descriptions)`.
///
/// The points up to where the project is agree up to some place and differ after it: what was applied after that place
/// before the producer undid (and may have replaced by a new edit, which drops them). What is applied after it now, and
/// was redoable before, was redone.
fn changes(before: &View, now: &View) -> Vec<(&'static str, Vec<String>)> {
    let kumi = |descs: Vec<String>| descs.into_iter().filter(|desc| is_kumi(desc)).collect::<Vec<_>>();
    let (before_start, now_start) = (before.tail_start(), now.tail_start());
    let common_to = before.at.min(now.at);
    let diverges = (before_start.max(now_start)..=common_to)
        .find(|&index| before.tail[(index - before_start) as usize] != now.tail[(index - now_start) as usize])
        .unwrap_or(common_to + 1);
    let mut found = Vec::new();
    let undone = kumi((diverges.max(before_start)..=before.at).map(|index| before.tail[(index - before_start) as usize].clone()).collect());
    if !undone.is_empty() {
        found.push(("undo", undone));
    }
    let redone = kumi(
        (diverges.max(before.at + 1).max(now_start)..=now.at)
            .filter(|&index| before.redo.get((index - before.at - 1) as usize) == now.tail.get((index - now_start) as usize))
            .map(|index| now.tail[(index - now_start) as usize].clone())
            .collect(),
    );
    if !redone.is_empty() {
        found.push(("redo", redone));
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kumis_points_are_told_by_the_nonce_at_the_end() {
        assert!(is_kumi("Kumi: Bass volume −6 dB (k7f3a)"));
        assert!(is_kumi("Kumi: undo Deleted track Pad (k0000)"));
        for other in [
            "Kumi: Bass volume (k7f3)",
            "Bass volume (k7f3a)",
            "Kumi: x (K7f3a)",
            "Kumi: x (k7f3a) ",
            "Kumi: x (k7F3a)",
            "Adjust track volume",
            "",
        ] {
            assert!(!is_kumi(other), "{other:?}");
        }
    }
}
