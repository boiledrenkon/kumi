//! `notes-quantize`: a region's notes quantized through the Event List, which has its own Quantize pop-up (Off, the
//! note values, triplets, swings) and a Quantize Selected Events button: the region's events shown, the pop-up set to
//! the grid (its menu lists as menu items once it is pressed), every row selected through the list's Edit menu
//! (Select All), the button pressed, the rows read back and the pop-up put back as it was. The positions are what
//! the notes' rows say, rounded to the grid from the project's start, and are what a restore puts back: Logic's
//! own undo while the step is on top, else each note's position stepped back as `notes-change` does. The Event
//! List's quantize has no strength: 100 is the one strength taken.

use std::time::Duration;

use serde_json::{json, Value};

use super::notes::{self, Shown, Spec};
use super::nudge::{self, Control};
use super::{changed, gone, Applied, Ctx, Done, Epoch, Preview, Restore, Restored, Watch};
use crate::refs::{bad, failed};
use crate::rows::Position;

/// The grids Logic's Event List offers, in beats: the note values and their triplets (a swing or a tuplet isn't a
/// grid in beats).
const GRIDS: [(f64, &str); 13] = [
    (4.0, "1/1 Note"),
    (2.0, "1/2 Note"),
    (1.0, "1/4 Note"),
    (0.5, "1/8 Note"),
    (0.25, "1/16 Note"),
    (0.125, "1/32 Note"),
    (0.0625, "1/64 Note"),
    (4.0 / 3.0, "1/2 Triplet (1/3)"),
    (2.0 / 3.0, "1/4 Triplet (1/6)"),
    (1.0 / 3.0, "1/8 Triplet (1/12)"),
    (1.0 / 6.0, "1/16 Triplet (1/24)"),
    (1.0 / 12.0, "1/32 Triplet (1/48)"),
    (1.0 / 24.0, "1/64 Triplet (1/96)"),
];
const OFF: &str = "Off";
const LIST_SETTLE: Duration = Duration::from_millis(300);
const LIST_SETTLE_MS: u64 = 300;
/// The most notes one quantize may move: each is read back, and the host waits 30 s.
const MOST_NOTES: usize = 10000;

/// The pop-up item for a grid in beats, and the grid as Logic's ticks.
fn grid_of(input: &Value) -> Done<(f64, &'static str)> {
    let grid = input
        .get("grid")
        .and_then(Value::as_f64)
        .filter(|g| g.is_finite() && *g > 0.0)
        .ok_or_else(|| bad("grid must be a number of beats above 0 (0.25 is a sixteenth)"))?;
    if let Some(strength) = input.get("strength").filter(|v| !v.is_null()) {
        if strength.as_f64() != Some(100.0) {
            return Err(bad("Logic Pro's Event List quantize has no strength: leave strength out, or give 100"));
        }
    }
    GRIDS.iter().find(|(beats, _)| (beats - grid).abs() < 1e-3).map(|(beats, item)| (*beats, *item)).ok_or_else(|| {
        let known: Vec<String> = GRIDS.iter().map(|(beats, item)| format!("{} ({item})", rows_trim(*beats))).collect();
        bad(format!("Logic Pro's Event List quantizes to one of its grids, in beats: {}", known.join(", ")))
    })
}

fn rows_trim(beats: f64) -> String {
    let text = format!("{beats:.4}");
    text.trim_end_matches('0').trim_end_matches('.').to_string()
}

/// A note's position rounded to the grid, counted from the project's start.
fn quantized(spec: &Spec, grid: f64, beats_per_bar: u32) -> Spec {
    let beats = spec.position.beats(beats_per_bar);
    let at = (beats / grid).round() * grid;
    Spec { position: Position::from_beats(at, beats_per_bar), ..*spec }
}

fn target(ctx: &Ctx, place: usize) -> Value {
    json!({"ref": super::ref_text(ctx, "region", &[place]), "kind": "region"})
}

fn notes_json(notes: &[Spec]) -> Vec<Value> {
    notes.iter().map(Spec::json).collect()
}

fn notes_of(value: &Value, key: &str) -> Done<Vec<Spec>> {
    value[key]
        .as_array()
        .ok_or_else(|| bad(format!("{key}: the notes as the change kept them")))?
        .iter()
        .map(|n| Spec::from_json(n).ok_or_else(|| bad("a note is not {position, pitch, velocity, lengthTicks, channel}")))
        .collect()
}

fn title(moved: usize, label: &str, item: &str) -> String {
    format!("Quantized {moved} note{} in {label} to {item}", if moved == 1 { "" } else { "s" })
}

// ----- the Event List's quantize controls -----

/// The list's Quantize pop-up (the one after its "Quantize:" label) and the Quantize Selected Events button.
fn controls(ctx: &mut Ctx) -> Done<(Control, String, Control)> {
    let group = nudge::read(ctx.project.list_group(ctx.ax))?;
    let strings = &ctx.project.strings;
    let children: Vec<&crate::ax::Node> = group.children.first().map(|g| g.children.iter().collect()).unwrap_or_default();
    let label_at = children.iter().position(|n| n.role == "AXStaticText" && strings.starts_with(&n.value_text(), "Quantize"));
    let popup = label_at
        .and_then(|at| children[at + 1..].iter().find(|n| n.role == "AXPopUpButton"))
        .or_else(|| {
            children
                .iter()
                .find(|n| n.role == "AXPopUpButton" && (n.value_text() == OFF || GRIDS.iter().any(|(_, item)| n.value_text() == *item)))
        })
        .ok_or_else(|| failed("Logic Pro's Event List shows no Quantize pop-up"))?;
    let button = group
        .find(&|n| n.role == "AXButton" && strings.is(&n.desc, "Quantize Selected Events"))
        .ok_or_else(|| failed("Logic Pro's Event List shows no Quantize Selected Events button"))?;
    Ok((Control::new(&ctx.project.root, &popup.path), popup.value_text(), Control::new(&ctx.project.root, &button.path)))
}

/// Chooses an item of the Quantize pop-up through its menu.
fn choose(ctx: &mut Ctx, popup: &Control, item: &str) -> Done<()> {
    nudge::wrote(ctx.ax.action(&popup.root, &popup.path, "AXPress", LIST_SETTLE_MS))?;
    let menu = nudge::wrote(ctx.ax.tree_in_popup(&popup.root, &popup.path, 2, 60, Some(&["role", "title"])))?;
    let strings = &ctx.project.strings;
    let Some(found) = menu.find(&|n| n.role == "AXMenuItem" && strings.is(&n.title, item)) else {
        let _ = ctx.ax.action_in_popup(&popup.root, &format!("{}.0", popup.path), "AXCancel", 100);
        return Err(failed(format!("Logic Pro's Quantize pop-up has no {item:?}")));
    };
    nudge::wrote(ctx.ax.action_in_popup(&popup.root, &found.path, "AXPress", LIST_SETTLE_MS))?;
    super::pause(LIST_SETTLE);
    let attrs = nudge::wrote(ctx.ax.get(&popup.root, &popup.path, &["AXValue"]))?;
    if attrs.get("AXValue").and_then(Value::as_str) != Some(item) {
        return Err(super::uncertain(format!("Logic Pro's Quantize pop-up shows {:?} after Kumi chose {item:?}", attrs.get("AXValue"))));
    }
    Ok(())
}

/// Selects every event of the list: its Edit menu's Select All.
fn select_all(ctx: &mut Ctx) -> Done<()> {
    let group = nudge::read(ctx.project.list_group(ctx.ax))?;
    let strings = &ctx.project.strings;
    let edit = group
        .find(&|n| n.role == "AXMenuButton" && strings.is(&n.desc, "Edit"))
        .ok_or_else(|| failed("Logic Pro's Event List shows no Edit menu"))?;
    let edit = Control::new(&ctx.project.root, &edit.path);
    nudge::wrote(ctx.ax.action(&edit.root, &edit.path, "AXPress", LIST_SETTLE_MS))?;
    let menu = nudge::wrote(ctx.ax.tree_in_popup(&edit.root, &edit.path, 3, 60, Some(&["role", "title"])))?;
    let strings = &ctx.project.strings;
    let Some(item) = menu.find(&|n| n.role == "AXMenuItem" && strings.is(&n.title, "Select All")) else {
        let _ = ctx.ax.action_in_popup(&edit.root, &format!("{}.0", edit.path), "AXCancel", 100);
        return Err(failed("Logic Pro's Event List Edit menu has no Select All"));
    };
    nudge::wrote(ctx.ax.action_in_popup(&edit.root, &item.path, "AXPress", LIST_SETTLE_MS))?;
    super::pause(LIST_SETTLE);
    Ok(())
}

/// Quantizes the shown region's events to the grid: the pop-up set, every row selected, the button pressed, the
/// pop-up put back. The notes as read after.
fn quantize_shown(ctx: &mut Ctx, shown: &Shown, item: &str) -> Done<Vec<Spec>> {
    let (popup, was, button) = controls(ctx)?;
    choose(ctx, &popup, item)?;
    let result: Done<()> = (|| {
        select_all(ctx)?;
        nudge::wrote(nudge::press(ctx.ax, &button, LIST_SETTLE_MS))?;
        super::pause(LIST_SETTLE);
        Ok(())
    })();
    if was != item {
        let _ = choose(ctx, &popup, &was);
    }
    result?;
    Ok(notes::specs(&notes::rows(ctx, shown).map_err(|(_, why)| super::uncertain(why))?))
}

// ----- preview, apply, restore -----

pub fn preview(ctx: &mut Ctx, input: &Value) -> Done<Preview> {
    let place = super::region_place(ctx, input, "ref", Epoch::Current)?;
    let (grid, item) = grid_of(input)?;
    let shown = notes::show(ctx, place)?;
    let result = (|| {
        let before = notes::specs(&notes::rows(ctx, &shown)?);
        if before.is_empty() {
            return Err(bad(format!("{} holds no notes to quantize", notes::label(&shown.region))));
        }
        if before.len() > MOST_NOTES {
            return Err(bad(format!("{} holds {} notes: more than Kumi quantizes at once", notes::label(&shown.region), before.len())));
        }
        let after: Vec<Spec> = before.iter().map(|spec| quantized(spec, grid, shown.beats_per_bar)).collect();
        let moved = before.iter().zip(&after).filter(|(a, b)| a != b).count();
        let label = notes::label(&shown.region);
        let mut display = json!({"title": title(moved, &label, item), "notes": before.len(), "moved": moved, "grid": item});
        if moved == 0 {
            display["unchanged"] = true.into();
        }
        let fence = notes::fence_of(&before);
        Ok(Preview { target: target(ctx, place), prior: json!({"notes": notes_json(&before), "fence": fence.clone()}), fence, display })
    })();
    notes::hide(ctx, shown);
    result
}

pub fn apply(ctx: &mut Ctx, input: &Value, fence: &Value) -> Done<Applied> {
    let place = super::region_place(ctx, input, "ref", Epoch::Current)?;
    let (grid, item) = grid_of(input)?;
    let shown = notes::show(ctx, place)?;
    let result = (|| {
        let before = notes::specs(&notes::rows(ctx, &shown)?);
        if notes::fence_of(&before) != *fence {
            return Err(changed("the notes in that region changed since the preview"));
        }
        let expected: Vec<Spec> = before.iter().map(|spec| quantized(spec, grid, shown.beats_per_bar)).collect();
        let moved = before.iter().zip(&expected).filter(|(a, b)| a != b).count();
        let label = notes::label(&shown.region);
        if moved == 0 {
            return Ok(Applied {
                applied: json!({"notes": notes_json(&before), "moved": 0}),
                display: json!({"title": title(0, &label, item), "unchanged": true}),
                ..Applied::default()
            });
        }
        let after = quantize_shown(ctx, &shown, item)?;
        let off_grid = notes::difference(&after, &expected);
        if !off_grid.is_empty() || after.len() != expected.len() {
            return Err(super::uncertain(format!(
                "Logic Pro shows {} notes after the quantize, {} of them not on the {item} grid",
                after.len(),
                off_grid.len()
            )));
        }
        Ok(Applied {
            applied: json!({"notes": notes_json(&after), "moved": moved, "grid": item}),
            display: json!({"title": title(moved, &label, item), "moved": moved}),
            step: Some("Quantize"),
            ..Applied::default()
        })
    })();
    notes::hide(ctx, shown);
    result
}

pub fn restore(ctx: &mut Ctx, watch: &mut Watch, restore: &Restore) -> Done<Restored> {
    let place = super::region_place(ctx, restore.target, "ref", Epoch::Any).map_err(|f| if f.0 == "failed" { gone(f.1) } else { f })?;
    let (prior, applied) = (notes_of(restore.prior, "notes")?, notes_of(restore.applied, "notes")?);
    let shown = notes::show(ctx, place).map_err(|f| if f.0 == "failed" { gone(f.1) } else { f })?;
    let result = (|| {
        let current = notes::specs(&notes::rows(ctx, &shown)?);
        let label = notes::label(&shown.region);
        if notes::difference(&prior, &current).is_empty() && current.len() == prior.len() {
            return Ok(Restored::by_value(
                json!({"notes": current.len()}),
                json!({"title": format!("The notes in {label} are where they were")}),
            ));
        }
        if !restore.discard && !(notes::difference(&applied, &current).is_empty() && current.len() == applied.len()) {
            return Err(changed(format!("the notes in {label} have been changed in Logic Pro since Kumi quantized them")));
        }
        // Logic's own undo first, while the quantize is its latest step; else each note back by value.
        if let Ok(pressed) = super::structure::undo_in_logic(ctx, watch, &restore.key, "Quantize", None) {
            super::pause(LIST_SETTLE);
            let after = notes::specs(&notes::rows(ctx, &shown).map_err(|(_, why)| super::uncertain(why))?);
            if notes::difference(&prior, &after).is_empty() && after.len() == prior.len() {
                return Ok(Restored {
                    restored: json!({"notes": after.len(), "pressed": pressed}),
                    display: json!({"title": format!("Notes in {label} back where they were"), "note": "Put back with Logic Pro's own undo."}),
                    undone_in_logic: true,
                });
            }
            return Err(super::uncertain(format!("Logic Pro's {pressed:?} was pressed, but the notes in {label} aren't where they were")));
        }
        let remove: Vec<Spec> = applied.iter().zip(&prior).filter(|(a, p)| a != p).map(|(a, _)| *a).collect();
        let add: Vec<Spec> = applied.iter().zip(&prior).filter(|(a, p)| a != p).map(|(_, p)| *p).collect();
        let after = notes::write_notes(ctx, &shown, &remove, &add, true)?;
        if !notes::difference(&prior, &after).is_empty() {
            return Err(super::uncertain(format!("the notes in {label} aren't all where they were after the undo")));
        }
        Ok(Restored::by_value(
            json!({"notes": after.len()}),
            json!({"title": format!("Notes in {label} back where they were ({} moved back)", add.len())}),
        ))
    })();
    notes::hide(ctx, shown);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grids_are_logics_note_values_and_notes_round_to_them_from_the_projects_start() {
        assert_eq!(grid_of(&json!({"grid": 0.25})).unwrap(), (0.25, "1/16 Note"));
        assert_eq!(grid_of(&json!({"grid": 1, "strength": 100})).unwrap().1, "1/4 Note");
        assert_eq!(grid_of(&json!({"grid": 0.3333})).unwrap().1, "1/8 Triplet (1/12)");
        assert!(grid_of(&json!({"grid": 0.3})).unwrap_err().1.contains("1/16 Note"));
        assert!(grid_of(&json!({"grid": 0.25, "strength": 50})).is_err());
        assert!(grid_of(&json!({})).is_err());
        let late = Spec {
            position: Position { bar: 1, beat: 2, division: 2, tick: 100 },
            pitch: 60,
            velocity: 100,
            length_ticks: 240,
            channel: 1,
        };
        let on_sixteenth = quantized(&late, 0.25, 4);
        assert_eq!(on_sixteenth.position, Position { bar: 1, beat: 2, division: 2, tick: 1 });
        let on_beat = quantized(&late, 1.0, 4);
        assert_eq!(on_beat.position, Position { bar: 1, beat: 2, division: 1, tick: 1 });
        // Past the half: up to the next.
        let later = Spec { position: Position { bar: 1, beat: 2, division: 3, tick: 200 }, ..late };
        assert_eq!(quantized(&later, 1.0, 4).position, Position { bar: 1, beat: 3, division: 1, tick: 1 });
        assert_eq!(title(3, "“Keys”", "1/16 Note"), "Quantized 3 notes in “Keys” to 1/16 Note");
        assert_eq!(rows_trim(0.25), "0.25");
        assert_eq!(rows_trim(1.0), "1");
    }
}
