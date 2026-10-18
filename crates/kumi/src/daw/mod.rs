//! The DAW this Kumi works with: Live, the only one registered so far. What the app says of the DAW and shows of its
//! tools comes from here.
use kumi_runtime::core::{
    daw::{DawId, DawProfile},
    presentation::ToolPresentation,
};

/// The DAW the app works with.
pub fn profile() -> &'static DawProfile {
    kumi_runtime::integrations::profile(DawId::Ableton).expect("Live is registered")
}

/// How the app shows the DAW's own tools.
pub fn presentation() -> &'static ToolPresentation {
    kumi_runtime::integrations::presentation(DawId::Ableton).expect("Live is registered")
}
