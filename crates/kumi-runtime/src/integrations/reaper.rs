//! REAPER, through Kumi's extension and host: its refs (REAPER's GUIDs, which hold across reloads), its read tool and
//! its instructions. The integration itself is the one every bridge of Kumi's own shares (`own_bridge`).
use super::own_bridge::{create_own_integration, OwnDaw, OwnOptions};
use super::own_views::{self, body_args, focus_track, selected, view};
use crate::core::{
    contracts::{Integration, JsonObject},
    daw::DawProfile,
    presentation::{ToolActivity, ToolPresentation},
    refs::{Moved, RefScheme, Shift},
    view::{BodyKind, BodyRequest, DawFocus, FocusBody, FocusItem, FocusParameter, ItemKind, ViewKind, ViewPresentation},
};
use crate::mcp::client::BridgeDescriptor;
use regex::Regex;
use serde_json::Value;
use std::{rc::Rc, sync::LazyLock};

/// The instructions REAPER's turns carry (M7 brings the full instruction pack).
const INSTRUCTIONS: &str = "You work in REAPER through Kumi's extension. Read the project with reaper_read before you describe it: its tracks (depth and folder show the folders), items and takes, FX and their parameters, sends and receives, markers, regions and envelopes. A ref such as track:3 is this turn's: pass it back as parent to read what's inside. Kumi reads REAPER here; changing it comes later.";

pub const DAW: OwnDaw = OwnDaw {
    profile: &DawProfile::REAPER,
    descriptor: BridgeDescriptor::REAPER,
    host_flag: "reaper",
    instructions: INSTRUCTIONS,
    read_tool: "reaper_read",
    read_description: "Read REAPER: the project, its tracks, and what's in them (items, FX, parameters, sends, markers, regions, MIDI notes, envelopes), a page at a time. Rows carry refs to pass back as parent; a page with nextCursor has more. Tracks come in project order: depth is how deep in folders a track sits (0 at the top), folder 1 starts a folder and -1 or less closes one or more; pan runs -1 to 1. A send's mode is 0 post-fader, 1 pre-FX, 3 pre-fader (modeName says it). Items' position and length are seconds; markers and regions add positionBeats; a note's start (from its item's start) and length are quarter notes, its channel 1-16.",
    read_kinds: &["project", "track", "item", "take", "fx", "param", "send", "marker", "region", "note", "envelope"],
    project_kind: "project",
    fill_focus,
    bodies: &[BodyKind::Tree, BodyKind::Notes, BodyKind::Timeline, BodyKind::Routing],
    body_request,
    to_body,
};

/// What the producer looks at in REAPER: the arrange view always, with the MIDI editor, an FX chain or a track's
/// routing over it when focused; the selected track, item (by its take, whose notes the MIDI editor shows) and FX.
fn fill_focus(focus: &Value) -> DawFocus {
    let selection = focus.get("selection").cloned().unwrap_or(Value::Null);
    let editor = match focus.get("view").and_then(Value::as_str) {
        Some("midi") => Some(view(ViewKind::NoteEditor, "MIDI editor")),
        Some("fx") => Some(view(ViewKind::DeviceChain, "FX chain")),
        Some("routing") => Some(view(ViewKind::Routing, "Routing")),
        _ => None,
    };
    let item = selected(&selection, "takeRef").or_else(|| selected(&selection, "itemRef")).map(|reference| FocusItem {
        kind: if selected(&selection, "takeRef").is_some() { ItemKind::Take } else { ItemKind::Item },
        item_ref: Some(reference),
        name: selection.get("itemName").and_then(Value::as_str).map(str::to_owned),
    });
    let fx = selection.get("fxName").and_then(Value::as_str).filter(|name| !name.is_empty()).map(str::to_owned);
    let parameter = selection.get("param").and_then(|param| {
        Some(FocusParameter {
            name: param.get("name")?.as_str()?.to_owned(),
            value: param.get("value").map(|value| value.as_str().map(str::to_owned).unwrap_or_else(|| value.to_string())),
            owner: fx.clone(),
        })
    });
    DawFocus {
        track: focus_track(&selection),
        track_ref: selected(&selection, "trackRef"),
        device_ref: selected(&selection, "fxRef"),
        device: fx,
        parameter,
        main_view: Some(view(ViewKind::Timeline, "Arrange")),
        editor_view: editor,
        item,
        ..Default::default()
    }
}

/// FOCUS draws a screenful of lanes, so it asks for that many tracks, not a page of every track in the project.
const ARRANGE_LANES: u32 = 24;

fn body_request(request: &BodyRequest) -> Option<JsonObject> {
    Some(match request {
        BodyRequest::Tree { track_ref } => body_args("fx", Some(track_ref)),
        BodyRequest::Notes { item_ref } => body_args("midi", Some(item_ref)),
        BodyRequest::Timeline => {
            let mut args = body_args("arrange", None);
            args.insert("limit".into(), ARRANGE_LANES.into());
            args
        }
        BodyRequest::Routing { track_ref } => body_args("routing", Some(track_ref)),
        _ => return None,
    })
}

fn to_body(request: &BodyRequest, body: &Value) -> Option<FocusBody> {
    Some(match request {
        BodyRequest::Tree { track_ref } => FocusBody::Tree(own_views::tree(track_ref, body)),
        BodyRequest::Notes { item_ref } => FocusBody::Notes(own_views::notes(item_ref, body)),
        BodyRequest::Timeline => FocusBody::Timeline(own_views::timeline(body)),
        BodyRequest::Routing { track_ref } => FocusBody::Routing(own_views::routing(track_ref, body)),
        _ => return None,
    })
}

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

/// How the app reads REAPER's focus and FX: the focus comes neutral, and a take in the MIDI editor shows its notes.
pub const VIEW: ViewPresentation = ViewPresentation {
    heading: |kind| match kind {
        BodyKind::Tree => "FX",
        BodyKind::Notes => "MIDI",
        BodyKind::Timeline => "Arrange",
        BodyKind::Routing => "Routing",
        other => (ViewPresentation::NONE.heading)(other),
    },
    timeline_notes: true,
    ..ViewPresentation::NONE
};

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
        // A send is named by its two tracks (and which repeat of the pair), a marker by its GUID: never by a number.
        assert_eq!(refs.kind_of("r:send:{A1}:-1:{B2}:2"), Some("send"));
        assert_eq!(
            refs.refs_in("r:send:{A1}:0:{B2}, r:send:{A1}:1:hw3 and r:marker:{C3}"),
            ["r:send:{A1}:0:{B2}", "r:send:{A1}:1:hw3", "r:marker:{C3}"]
        );
    }
}
