//! Live's side of a fixed phrase (core/phrases.rs): the make_changes plan it is. One step, so it lands in HISTORY
//! with its undo and Kumi says what changed, with no model call. A track the phrase names that isn't in the Set, or
//! that two tracks fit, makes no plan: the request goes to the model.

use serde_json::{json, Value};

use super::set_model::SetModel;
use crate::core::phrases::{resolve, Intent, Switch, TrackPick};

/// The make_changes steps for an intent in the Set as last read: None when it can't run as asked.
pub fn steps(intent: &Intent, model: &SetModel, beats_per_bar: f64) -> Option<Value> {
    let step = match intent {
        Intent::Tempo { bpm } => json!({"tool": "set_tempo", "input": {"tempo": bpm}}),
        Intent::Track { track, switch, on } => {
            let reference = track_ref(track, model)?;
            match switch {
                Switch::Mute => json!({"tool": "set_mixer", "input": {"trackRef": reference, "mute": on}}),
                Switch::Solo => json!({"tool": "set_mixer", "input": {"trackRef": reference, "solo": on}}),
                Switch::Arm => json!({"tool": "set_routing", "input": {"trackRef": reference, "arm": on}}),
            }
        }
        Intent::Loop { from_bar, to_bar } => {
            let bar = if beats_per_bar.is_finite() && beats_per_bar > 0.0 { beats_per_bar } else { 4.0 };
            let start = f64::from(from_bar - 1) * bar;
            let length = f64::from(to_bar - from_bar + 1) * bar;
            json!({"tool": "set_transport", "input": {"loopEnabled": true, "loopStart": start, "loopLength": length}})
        }
        Intent::LoopOff => json!({"tool": "set_transport", "input": {"loopEnabled": false}}),
        Intent::Metronome { on } => json!({"tool": "set_transport", "input": {"metronome": on}}),
    };
    Some(json!({"steps": [step], "final": true}))
}

/// The track a phrase means: by name among every track but Main, or by place among the Set's own tracks.
fn track_ref(track: &TrackPick, model: &SetModel) -> Option<String> {
    let tracks: Vec<_> = model
        .tracks
        .iter()
        .filter(|node| match track {
            TrackPick::Position(_) => matches!(node.kind.as_deref(), Some("regular" | "group") | None),
            TrackPick::Name(_) => node.kind.as_deref() != Some("main"),
        })
        .collect();
    let names: Vec<&str> = tracks.iter().map(|node| node.name.as_str()).collect();
    resolve(track, &names).map(|at| tracks[at].reference.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::contracts::JsonObject;

    fn model() -> SetModel {
        let rows: Vec<JsonObject> = [
            ("1:track:0", "Drums", "regular"),
            ("1:track:1", "Bass", "regular"),
            ("1:track:2", "Keys", "group"),
            ("1:return:0", "A-Reverb", "return"),
            ("1:main", "Main", "main"),
        ]
        .iter()
        .map(|(reference, name, kind)| json!({"ref": reference, "name": name, "kind": kind}).as_object().unwrap().clone())
        .collect();
        SetModel::next(&SetModel::default(), &rows, &[], true)
    }

    fn plan(intent: Intent) -> Option<Value> {
        steps(&intent, &model(), 3.0)
    }

    #[test]
    fn each_phrase_is_one_step_of_its_change() {
        let name = |text: &str| TrackPick::Name(text.into());
        assert_eq!(plan(Intent::Tempo { bpm: 124.0 }).unwrap()["steps"][0], json!({"tool": "set_tempo", "input": {"tempo": 124.0}}));
        assert_eq!(
            plan(Intent::Track { track: name("bass"), switch: Switch::Mute, on: true }).unwrap(),
            json!({"steps": [{"tool": "set_mixer", "input": {"trackRef": "1:track:1", "mute": true}}], "final": true})
        );
        assert_eq!(
            plan(Intent::Track { track: name("drums"), switch: Switch::Solo, on: false }).unwrap()["steps"][0]["input"],
            json!({"trackRef": "1:track:0", "solo": false})
        );
        assert_eq!(
            plan(Intent::Track { track: TrackPick::Position(3), switch: Switch::Arm, on: true }).unwrap()["steps"][0],
            json!({"tool": "set_routing", "input": {"trackRef": "1:track:2", "arm": true}})
        );
        assert_eq!(
            plan(Intent::Track { track: name("a-reverb"), switch: Switch::Mute, on: true }).unwrap()["steps"][0]["input"]["trackRef"],
            "1:return:0",
            "a return by its name"
        );
        // In 3/4, bars 9 to 16 start at beat 24 and last 24 beats.
        assert_eq!(
            plan(Intent::Loop { from_bar: 9, to_bar: 16 }).unwrap()["steps"][0]["input"],
            json!({"loopEnabled": true, "loopStart": 24.0, "loopLength": 24.0})
        );
        assert_eq!(plan(Intent::LoopOff).unwrap()["steps"][0]["input"], json!({"loopEnabled": false}));
        assert_eq!(
            plan(Intent::Metronome { on: true }).unwrap()["steps"][0],
            json!({"tool": "set_transport", "input": {"metronome": true}})
        );
    }

    #[test]
    fn a_track_that_isnt_there_or_isnt_one_makes_no_plan() {
        let pick = |track: TrackPick| plan(Intent::Track { track, switch: Switch::Mute, on: true });
        assert_eq!(pick(TrackPick::Name("guitar".into())), None);
        assert_eq!(pick(TrackPick::Name("main".into())), None, "Main isn't a track to mute");
        assert_eq!(pick(TrackPick::Position(4)), None, "returns don't count as places");
    }
}
