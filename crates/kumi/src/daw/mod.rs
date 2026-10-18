//! The DAW this Kumi works with: Live, the only one registered so far. What the app says of the DAW and shows of its
//! tools, its focus and its devices comes from here.
pub mod ableton;

use kumi_runtime::core::{
    daw::{DawId, DawProfile},
    presentation::ToolPresentation,
    view::ViewPresentation,
};

/// The DAW the app works with.
pub fn profile() -> &'static DawProfile {
    kumi_runtime::integrations::profile(DawId::Ableton).expect("Live is registered")
}

/// How the app shows the DAW's own tools.
pub fn presentation() -> &'static ToolPresentation {
    kumi_runtime::integrations::presentation(DawId::Ableton).expect("Live is registered")
}

/// How the app reads the DAW's focus and devices.
pub fn view() -> &'static ViewPresentation {
    kumi_runtime::integrations::view(DawId::Ableton).expect("Live is registered")
}
