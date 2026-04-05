//! kara-theme: theme specification and rendering for the kara desktop environment.
//!
//! TOML theme specs, color resolution/derivation, and per-app config renderers.

pub mod spec;
pub mod resolved;
pub mod validate;
pub mod derive;
pub mod render;

pub use spec::*;
pub use resolved::*;
pub use validate::validate_spec;
pub use derive::{Palette16, resolve_theme};
