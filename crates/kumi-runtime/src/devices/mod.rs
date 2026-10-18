//! Max for Live devices: the .amxd format, read and written wherever Kumi meets one, and Live's device builder
//! (make_device), which lives with Live's integration and keeps its paths here.
pub mod amxd;
pub use crate::integrations::live_devices::*;
