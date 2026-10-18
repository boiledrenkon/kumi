//! The DAWs Kumi integrates, and what their integrations share. Outside Live's own integration, only this registry
//! and the crate root name it: the crate root's constructors for Live come through here.
pub mod ableton;
pub mod fallback;
pub mod none;
pub mod shared;

use crate::core::daw::{DawId, DawProfile};
use crate::core::presentation::ToolPresentation;
use crate::core::view::ViewPresentation;

pub use ableton::devices as live_devices;
pub use ableton::focus::parse_focus;
pub use ableton::live_set;
pub use ableton::manual as live_manual;
pub use ableton::project::{create_project_store, Baseline, ProjectStore};
pub use ableton::{create_ableton_integration, create_inference_only_integration, BRIDGE_TOOLS};
pub use ableton::{ears, hands};

/// The DAWs this build integrates, by profile: Live only, for now.
pub const REGISTERED: &[&DawProfile] = &[&DawProfile::ABLETON];

/// A registered DAW's profile; None for one this build doesn't integrate.
pub fn profile(id: DawId) -> Option<&'static DawProfile> {
    REGISTERED.iter().copied().find(|profile| profile.id == Some(id))
}

/// How the app shows a registered DAW's own tools.
pub fn presentation(id: DawId) -> Option<&'static ToolPresentation> {
    match id {
        DawId::Ableton => Some(&ableton::presentation::PRESENTATION),
        _ => None,
    }
}

/// How the app reads a registered DAW's focus and devices.
pub fn view(id: DawId) -> Option<&'static ViewPresentation> {
    match id {
        DawId::Ableton => Some(&ableton::presentation::VIEW),
        _ => None,
    }
}

/// A registered DAW's manual as a tool, for a DAW whose manual Kumi reads (Live's).
pub fn manual_tool(id: DawId, options: live_manual::ManualOptions) -> Option<std::rc::Rc<dyn crate::core::contracts::KernelTool>> {
    match id {
        DawId::Ableton => Some(live_manual::manual_tool(options)),
        _ => None,
    }
}
