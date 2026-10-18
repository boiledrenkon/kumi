//! The DAWs Kumi integrates, and what their integrations share. Outside Live's own integration, only this registry
//! and the crate root name it: the crate root's constructors for Live come through here.
pub mod ableton;
pub mod fallback;
pub mod none;
pub mod shared;

use crate::core::daw::{DawId, DawProfile};

pub use ableton::focus::parse_focus;
pub use ableton::project::{create_project_store, Baseline, ProjectStore};
pub use ableton::{create_ableton_integration, create_inference_only_integration, BRIDGE_TOOLS};

/// The DAWs this build integrates, by profile: Live only, for now.
pub const REGISTERED: &[&DawProfile] = &[&DawProfile::ABLETON];

/// A registered DAW's profile; None for one this build doesn't integrate.
pub fn profile(id: DawId) -> Option<&'static DawProfile> {
    REGISTERED.iter().copied().find(|profile| profile.id == Some(id))
}
