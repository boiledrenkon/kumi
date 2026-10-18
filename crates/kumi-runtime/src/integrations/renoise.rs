//! Renoise, through Kumi's tool and host: its refs (positions, as Renoise numbers them, under the song's epoch), its
//! read tool and its instructions. The integration itself is the one every bridge of Kumi's own shares (`own_bridge`).
use super::own_bridge::{create_own_integration, OwnDaw, OwnOptions};
use crate::core::{
    contracts::{Integration, JsonObject},
    daw::DawProfile,
    presentation::{ToolActivity, ToolPresentation},
    refs::{moved_index, Moved, RefScheme, Shift},
    view::ViewPresentation,
};
use crate::mcp::client::BridgeDescriptor;
use regex::Regex;
use std::{rc::Rc, sync::LazyLock};

/// The instructions Renoise's turns carry (M7 brings the full instruction pack).
const INSTRUCTIONS: &str = "You work in Renoise through Kumi's tool. Read the song with renoise_read before you describe it: its tracks (sequencer, group, send and master), the pattern sequence and its patterns, a pattern's lines in Renoise's own notation, instruments with their samples, keyzones and phrases, and each track's devices and their parameters. A ref such as track:3 is this turn's: pass it back as parent (a pattern for its lines, with track) to read what's inside. Kumi reads Renoise here; changing it comes later.";

pub const DAW: OwnDaw = OwnDaw {
    profile: &DawProfile::RENOISE,
    descriptor: BridgeDescriptor::RENOISE,
    host_flag: "renoise",
    instructions: INSTRUCTIONS,
    read_tool: "renoise_read",
    read_description: "Read Renoise: the song, its tracks, and what's in them (the sequence, patterns and their lines, instruments, samples, phrases, devices and parameters), a page at a time. Rows carry refs to pass back as parent; a page with nextCursor has more.",
    read_kinds: &["song", "track", "sequence", "pattern", "line", "instrument", "sample", "phrase", "device", "param"],
    project_kind: "song",
};

/// Renoise's refs: `<epoch>:<kind>:<numbers>`, numbered as Renoise shows them (from 1). A track inserted or deleted
/// moves the tracks after it, and the devices and parameters on them.
#[derive(Default)]
pub struct RenoiseRefs;

/// A ref's parts: its epoch, kind and the numbers after it.
fn parts(reference: &str) -> Option<(&str, &str, Vec<usize>)> {
    let mut parts = reference.splitn(3, ':');
    let (epoch, kind, numbers) = (parts.next()?, parts.next()?, parts.next()?);
    if epoch.is_empty() || kind.is_empty() {
        return None;
    }
    let numbers: Option<Vec<usize>> = numbers.split('.').map(|n| n.parse().ok()).collect();
    Some((epoch, kind, numbers?))
}

impl RefScheme for RenoiseRefs {
    fn positional(&self) -> bool {
        true
    }
    fn kind_of<'a>(&self, reference: &'a str) -> Option<&'a str> {
        parts(reference).map(|(_, kind, _)| kind)
    }
    fn moved(&self, shift: &Shift, reference: &str) -> Moved {
        let Some((epoch, kind, mut numbers)) = parts(reference) else { return Moved::Same };
        if !matches!(kind, "track" | "device" | "param") || numbers.is_empty() {
            return Moved::Same;
        }
        match moved_index(numbers[0], &shift.tracks_gone, &shift.tracks_made) {
            Some(now) if now == numbers[0] => Moved::Same,
            Some(now) => {
                numbers[0] = now;
                let numbers: Vec<String> = numbers.iter().map(usize::to_string).collect();
                Moved::To(format!("{epoch}:{kind}:{}", numbers.join(".")))
            }
            None => Moved::Gone,
        }
    }
    fn place_of(&self, reference: &str) -> String {
        reference.into()
    }
    fn refs_in<'a>(&self, text: &'a str) -> Vec<&'a str> {
        static REF: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[0-9A-Za-z]+:[a-z]+:[0-9]+(?:\.[0-9]+)*").unwrap());
        REF.find_iter(text).map(|found| found.as_str()).collect()
    }
    fn reference_fields(&self) -> &[String] {
        &[]
    }
    fn parents(&self, kind: &str) -> Option<Vec<String>> {
        let parents: &[&str] = match kind {
            "line" => &["pattern"],
            "sample" | "phrase" => &["instrument"],
            "device" => &["track"],
            "param" => &["device"],
            _ => return None,
        };
        Some(parents.iter().map(|parent| parent.to_string()).collect())
    }
    fn parent_hint(&self, kind: &str, _parent: &str) -> &'static str {
        match kind {
            "line" => "read a pattern's lines with its ref as parent and the track as track",
            _ => "",
        }
    }
    fn row_refs(&self, _kind: &str, _row: &JsonObject) -> Vec<(String, String, bool)> {
        vec![]
    }
    fn track_of(&self, reference: &str) -> Option<String> {
        let (epoch, kind, numbers) = parts(reference)?;
        match kind {
            "track" | "device" | "param" => Some(format!("{epoch}:track:{}", numbers.first()?)),
            _ => None,
        }
    }
}

/// Renoise's tools as the app shows them.
pub const PRESENTATION: ToolPresentation = ToolPresentation {
    label: |tool| match tool {
        "renoise_read" => Some("looked at your song"),
        _ => None,
    },
    doing: |tool| match tool {
        "renoise_read" => Some("looking at your song"),
        _ => None,
    },
    activity: |tool| (tool == "renoise_read").then_some(ToolActivity::Read),
    plan: |tool| tool == "make_changes",
    acts: |_| false,
    read_prefix: Some("renoise_"),
};

/// How the app reads Renoise's focus and devices (FOCUS's bodies come with M2's views).
pub const VIEW: ViewPresentation = ViewPresentation::NONE;

/// Renoise's integration.
pub fn create_renoise_integration(options: OwnOptions) -> Rc<dyn Integration> {
    create_own_integration::<RenoiseRefs>(&DAW, options)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_track_inserted_moves_the_tracks_after_it_with_their_devices_and_parameters() {
        let refs = RenoiseRefs;
        let shift = Shift { tracks_made: vec![2], ..Default::default() };
        assert_eq!(refs.moved(&shift, "7:track:1"), Moved::Same);
        assert_eq!(refs.moved(&shift, "7:track:2"), Moved::To("7:track:3".into()));
        assert_eq!(refs.moved(&shift, "7:device:4.2"), Moved::To("7:device:5.2".into()));
        assert_eq!(refs.moved(&shift, "7:param:2.1.9"), Moved::To("7:param:3.1.9".into()));
        assert_eq!(refs.moved(&shift, "7:pattern:2"), Moved::Same);
        let gone = Shift { tracks_gone: vec![2], ..Default::default() };
        assert_eq!(refs.moved(&gone, "7:device:2.1"), Moved::Gone);
        assert_eq!(refs.track_of("7:param:3.1.9"), Some("7:track:3".into()));
        assert_eq!(refs.kind_of("7:sample:2.4"), Some("sample"));
        assert_eq!(refs.kind_of("track:3"), None);
    }
}
