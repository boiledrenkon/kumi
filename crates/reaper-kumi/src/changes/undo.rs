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
/// The two looks share the history up to some point: the newest point applied before that is applied now after the same
/// points. It is found by the descriptions, not by places: REAPER drops its oldest points at once when the history is full,
/// which moves every place. What was applied after it before was undone (and may have been replaced by a new edit, which
/// drops it). What is applied after it now, and was redoable before, was redone.
fn changes(before: &View, now: &View) -> Vec<(&'static str, Vec<String>)> {
    let kumi = |descs: Vec<String>| descs.into_iter().filter(|desc| is_kumi(desc)).collect::<Vec<_>>();
    let (kept, at) = shared(&before.tail, &now.tail, now.tail.len() >= TAIL_SCAN as usize);
    let mut found = Vec::new();
    let undone = kumi(before.tail[kept..].to_vec());
    if !undone.is_empty() {
        found.push(("undo", undone));
    }
    // (After an undo, nothing that could be redone before can be applied without what was undone first.)
    if kept == before.tail.len() {
        let redone = kumi(
            now.tail[at..]
                .iter()
                .enumerate()
                .filter(|(k, desc)| before.redo.get(*k) == Some(*desc))
                .map(|(_, desc)| desc.clone())
                .collect(),
        );
        if !redone.is_empty() {
            found.push(("redo", redone));
        }
    }
    found
}

/// How many of `before`'s points are still applied, and how many of `now`'s points lead up to the last of them: the most
/// of `before` whose points agree with `now`'s up to some place, as far back as both go. Fewer of `now`'s points than of
/// `before`'s lead up to it only when `now` was cut at its start (`cut`: more points on top of it), and then at least `AGREE`
/// must agree (or all there are), so that a run of like descriptions can't stand in for another place.
fn shared(before: &[String], now: &[String], cut: bool) -> (usize, usize) {
    const AGREE: usize = 8;
    let agree = |kept: usize, at: usize| (1..=kept.min(at)).all(|back| before[kept - back] == now[at - back]);
    for kept in (1..=before.len()).rev() {
        let least = if cut { kept.min(AGREE) } else { kept };
        if let Some(at) = (least..=now.len()).rev().find(|&at| agree(kept, at)) {
            return (kept, at);
        }
    }
    (0, 0)
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
        assert_eq!(told(&before, &now), [("undo", vec!["Kumi: Vocal volume −8 dB (kd3db)".to_string()])]);
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
