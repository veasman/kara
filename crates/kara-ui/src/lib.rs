//! kara-ui: shared rendering primitives for the kara desktop environment.
//!
//! Canvas drawing (rounded rects, circles, glyph blitting) and
//! text rendering (cosmic-text measurement and drawing).

pub mod blur;
pub mod canvas;
pub mod text;

pub use canvas::*;
pub use text::TextRenderer;
