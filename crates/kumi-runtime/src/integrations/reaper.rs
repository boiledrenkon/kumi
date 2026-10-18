//! REAPER, through Kumi's extension and host: its refs (REAPER's GUIDs, which hold across reloads), its read tool and
//! its instructions. The integration itself is the one every bridge of Kumi's own shares (`own_bridge`).
use super::own_bridge::{create_own_integration, OwnDaw, OwnOptions};
use crate::core::{
    contracts::{Integration, JsonObject},
    daw::DawProfile,
    presentation::{ToolActivity, ToolPresentation},
    refs::{Moved, RefScheme, Shift},
    view::ViewPresentation,
};
use crate::mcp::client::BridgeDescriptor;
use regex::Regex;
use std::{rc::Rc, sync::LazyLock};

/// The instructions REAPER's turns carry (M7 brings the full instruction pack).
const INSTRUCTIONS: &str = "You work in REAPER through Kumi's extension. Read the project with reaper_read before you describe it: its tracks (depth and folder show the folders), items and takes, FX and their parameters, sends and receives, markers, regions and envelopes. A ref such as track:3 is this turn's: pass it back as parent to read what's inside. Kumi reads REAPER here; changing it comes later.";

pub const DAW: OwnDaw = OwnDaw {
    profile: &DawProfile::REAPER,
    descriptor: BridgeDescriptor::REAPER,
    host_flag: "reaper",
    instructions: INSTRUCTIONS,
    read_tool: "reaper_read",
    read_description: "Read REAPER: the project, its tracks, and what's in them (items, FX, parameters, sends, markers, regions, MIDI notes, envelopes), a page at a time. Rows carry refs to pass back as parent; a page with nextCursor has more.",
    read_kinds: &["project", "track", "item", "fx", "param", "send", "marker", "region", "note", "envelope"],
    project_kind: "project",
};

/// REAPER's refs: `r:<kind>:<GUID or number>`. They name the same object across reloads, so a restructure moves none.
#[derive(Default)]
pub struct ReaperRefs;

impl RefScheme for ReaperRefs {
    fn positional(&self) -> bool {
        false
    }
    fn kind_of<'a>(&self, reference: &'a str) -> Option<&'a str> {
        let mut parts = reference.splitn(3, ':');
        match (parts.next(), parts.next(), parts.next()) {
            (Some("r"), Some(kind), Some(rest)) if !kind.is_empty() && !rest.is_empty() => Some(kind),
            _ => None,
        }
    }
    fn moved(&self, _shift: &Shift, _reference: &str) -> Moved {
        Moved::Same
    }
    fn place_of(&self, reference: &str) -> String {
        reference.into()
    }
    fn refs_in<'a>(&self, text: &'a str) -> Vec<&'a str> {
        static REF: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"r:[a-z]+:[0-9A-Za-z{}:.-]+").unwrap());
        REF.find_iter(text).map(|found| found.as_str()).collect()
    }
    fn reference_fields(&self) -> &[String] {
        &[]
    }
    fn parents(&self, kind: &str) -> Option<Vec<String>> {
        let parents: &[&str] = match kind {
            "item" | "send" => &["track"],
            "fx" => &["track", "take"],
            "param" => &["fx"],
            "note" => &["take"],
            "envelope" => &["track", "fx"],
            _ => return None,
        };
        Some(parents.iter().map(|parent| parent.to_string()).collect())
    }
    fn parent_hint(&self, _kind: &str, _parent: &str) -> &'static str {
        ""
    }
    fn row_refs(&self, _kind: &str, _row: &JsonObject) -> Vec<(String, String, bool)> {
        vec![]
    }
    fn track_of(&self, reference: &str) -> Option<String> {
        (self.kind_of(reference) == Some("track")).then(|| reference.to_owned())
    }
}

/// REAPER's tools as the app shows them.
pub const PRESENTATION: ToolPresentation = ToolPresentation {
    label: |tool| match tool {
        "reaper_read" => Some("looked at your project"),
        _ => None,
    },
    doing: |tool| match tool {
        "reaper_read" => Some("looking at your project"),
        _ => None,
    },
    activity: |tool| (tool == "reaper_read").then_some(ToolActivity::Read),
    plan: |tool| tool == "make_changes",
    acts: |_| false,
    read_prefix: Some("reaper_"),
};

/// How the app reads REAPER's focus and FX (FOCUS's bodies come with M2's views).
pub const VIEW: ViewPresentation = ViewPresentation::NONE;

/// REAPER's integration.
pub fn create_reaper_integration(options: OwnOptions) -> Rc<dyn Integration> {
    create_own_integration::<ReaperRefs>(&DAW, options)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_reaper_ref_is_its_kind_and_its_guid() {
        let refs = ReaperRefs;
        assert_eq!(refs.kind_of("r:track:{0A1B2C3D-0000-4000-8000-000000000001}"), Some("track"));
        assert_eq!(refs.kind_of("r:track:master"), Some("track"));
        assert_eq!(refs.kind_of("r:param:{AB}:3"), Some("param"));
        assert_eq!(refs.kind_of("track:3"), None);
        assert_eq!(refs.kind_of("r:track:"), None);
        assert!(!refs.positional());
        assert_eq!(refs.refs_in("set r:fx:{AB-CD} and r:track:master"), ["r:fx:{AB-CD}", "r:track:master"]);
    }
}
