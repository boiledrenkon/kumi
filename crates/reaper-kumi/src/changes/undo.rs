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
/// Only Kumi's own points are looked at, and each is told by its description, which its nonce makes unique (REAPER gives the
/// producer's points like descriptions, "Adjust track volume" for every fader move, so no place is told by theirs). A point
/// of Kumi's that was applied and is redoable now was undone; one that was redoable and is applied now was redone. One that
/// was applied and is in neither list was undone and replaced by a new edit when the look saw the whole history; past what a
/// look sees (REAPER's oldest points dropped, or more points made than are looked at) where it is can't be told, and nothing
/// is said of it.
fn changes(before: &View, now: &View) -> Vec<(&'static str, Vec<String>)> {
    let kumi = |list: &[String]| list.iter().filter(|desc| is_kumi(desc)).cloned().collect::<std::collections::HashSet<String>>();
    let (applied_now, redoable_now) = (kumi(&now.tail), kumi(&now.redo));
    // The look sees the whole history only if nothing went from its front since the last: REAPER at its undo memory merges its
    // oldest points into its first one (whose description changes), which leaves a point of Kumi's lower in the history, and
    // a full look shorter.
    let place_of =
        |view: &View, desc: &String| view.tail.iter().position(|d| d == desc).map(|p| view.at + 1 - view.tail.len() as i32 + p as i32);
    let full = |view: &View| view.tail.len() >= TAIL_SCAN as usize;
    let trimmed = (!full(before) && before.tail.first() != now.tail.first())
        || (full(before) && !full(now) && now.at < before.at)
        || before
            .tail
            .iter()
            .filter(|desc| is_kumi(desc))
            .any(|desc| matches!((place_of(before, desc), place_of(now, desc)), (Some(then), Some(at)) if at < then));
    let whole = !full(now) && !trimmed;
    let undone: Vec<String> = before
        .tail
        .iter()
        .filter(|desc| is_kumi(desc) && !applied_now.contains(*desc) && (redoable_now.contains(*desc) || whole))
        .cloned()
        .collect();
    let redone: Vec<String> = before.redo.iter().filter(|desc| is_kumi(desc) && applied_now.contains(*desc)).cloned().collect();
    let mut found = Vec::new();
    if !undone.is_empty() {
        found.push(("undo", undone));
    }
    if !redone.is_empty() {
        found.push(("redo", redone));
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    fn look(tail: &[&str], redo: &[&str]) -> View {
        let tail: Vec<String> = tail.iter().map(|d| d.to_string()).collect();
        View { entries: 0, at: tail.len() as i32 - 1, count: 0, tail, redo: redo.iter().map(|d| d.to_string()).collect() }
    }

    /// A history of `n` producer points named by their number, with Kumi's point `depth` points under the newest.
    fn history(n: usize, kumi_depth: usize) -> Vec<String> {
        (0..n).map(|k| if k == n - 1 - kumi_depth { "Kumi: Vocal volume −8 dB (kd3db)".to_string() } else { format!("Edit {k}") }).collect()
    }

    fn told(before: &View, now: &View) -> Vec<(&'static str, Vec<String>)> {
        changes(before, now)
    }

    #[test]
    fn reaper_dropping_its_oldest_points_is_no_undo() {
        // 745 points, Kumi's 60 down; the producer's next edit makes REAPER drop the 246 oldest at once.
        let full = history(745, 60);
        let before = look(&full[745 - 128..].iter().map(String::as_str).collect::<Vec<_>>(), &[]);
        let mut trimmed: Vec<String> = full[246..].to_vec();
        trimmed[0] = "Multiple changes".into();
        trimmed.push("Adjust track pan".into());
        let now = look(&trimmed[trimmed.len() - 128..].iter().map(String::as_str).collect::<Vec<_>>(), &[]);
        assert_eq!(told(&before, &now), []);
        // And when the producer undid Kumi's point too, before the edit that trimmed: that is told.
        let mut undone: Vec<String> = full[246..745 - 61].to_vec();
        undone.push("Adjust track pan".into());
        let now = look(&undone[undone.len() - 128..].iter().map(String::as_str).collect::<Vec<_>>(), &[]);
        // (undone and replaced, past a look that sees all of the history: where it went can't be told, so nothing is said)
        assert_eq!(told(&before, &now), []);
    }

    #[test]
    fn like_descriptions_dont_hide_an_undo() {
        // Kumi's point among the producer's like edits; the producer undoes nine points and makes nine like ones.
        let mut before: Vec<&str> = vec!["Adjust track volume"; 10];
        before.push("Kumi: Vocal volume −8 dB (kd3db)");
        before.extend(["Adjust track volume"; 8]);
        let now = vec!["Adjust track volume"; 19];
        assert_eq!(told(&look(&before, &[]), &look(&now, &[])), [("undo", vec!["Kumi: Vocal volume −8 dB (kd3db)".to_string()])]);
        // Undone three, two new: Kumi's was among the three.
        let before = ["Init", "V", "Kumi: x (k0001)", "V", "V"];
        let now = ["Init", "V", "V", "V"];
        assert_eq!(told(&look(&before, &[]), &look(&now, &[])), [("undo", vec!["Kumi: x (k0001)".to_string()])]);
    }

    /// The last `n` points of a history of like fader moves with Kumi's change `depth` under the newest, and what can be
    /// redone after it.
    fn fader_history(n: usize, depth: usize, redo: usize) -> (Vec<String>, Vec<String>) {
        let all: Vec<String> = (0..n)
            .map(|k| if k == n - 1 - depth { "Kumi: Vocal volume −8 dB (kd3db)".into() } else { "Adjust track volume".into() })
            .collect();
        let tail = all[n.saturating_sub(128)..].to_vec();
        (tail, vec!["Adjust track volume".to_string(); redo])
    }

    fn view_of((tail, redo): &(Vec<String>, Vec<String>)) -> View {
        look(&tail.iter().map(String::as_str).collect::<Vec<_>>(), &redo.iter().map(String::as_str).collect::<Vec<_>>())
    }

    #[test]
    fn a_jump_back_over_kumis_point_among_like_fader_moves_is_an_undo_and_a_jump_forward_a_redo() {
        let kumi = vec!["Kumi: Vocal volume −8 dB (kd3db)".to_string()];
        // 300 like points, Kumi's 10 down; the producer jumps back 21 points in the Undo History.
        let before = view_of(&fader_history(300, 10, 0));
        let (tail, _) = fader_history(300, 10, 0);
        let mut redo: Vec<String> = tail[tail.len() - 21..].iter().rev().cloned().collect();
        redo.truncate(21);
        let back = look(
            &tail[..tail.len() - 21].iter().map(String::as_str).collect::<Vec<_>>(),
            &redo.iter().map(String::as_str).collect::<Vec<_>>(),
        );
        assert_eq!(told(&before, &back), [("undo", kumi.clone())]);
        // And forward again over it, many points at once.
        assert_eq!(told(&back, &before), [("redo", kumi)]);
    }

    #[test]
    fn many_points_made_between_two_looks_are_no_undo_of_kumis_point() {
        // A script makes 120 points: Kumi's change is still applied, 130 down, past what a look sees.
        let before = view_of(&fader_history(300, 10, 0));
        let after = view_of(&fader_history(420, 130, 0));
        assert_eq!(told(&before, &after), []);
        // 100 points: it is still in sight, and applied.
        let after = view_of(&fader_history(400, 110, 0));
        assert_eq!(told(&before, &after), []);
    }

    #[test]
    fn an_undo_a_redo_and_new_edits_are_told_as_before() {
        let before = look(&["Init", "A", "Kumi: x (k0001)"], &["Kumi: y (k0002)"]);
        // Redone.
        assert_eq!(
            told(&before, &look(&["Init", "A", "Kumi: x (k0001)", "Kumi: y (k0002)"], &[])),
            [("redo", vec!["Kumi: y (k0002)".to_string()])]
        );
        // Undone.
        assert_eq!(
            told(&before, &look(&["Init", "A"], &["Kumi: x (k0001)", "Kumi: y (k0002)"])),
            [("undo", vec!["Kumi: x (k0001)".to_string()])]
        );
        // A new edit of the producer's.
        assert_eq!(told(&before, &look(&["Init", "A", "Kumi: x (k0001)", "B"], &[])), []);
    }

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
