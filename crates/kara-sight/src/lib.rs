/// kara-sight: status bar rendering for the kara desktop environment.
///
/// Renders the bar to a tiny-skia Pixmap which kara-gate (compositor) uploads
/// as a GLES texture each frame.

pub mod status;
pub mod text;
pub mod render;

pub use render::{BarRenderer, WorkspaceContext};
pub use status::StatusCache;
