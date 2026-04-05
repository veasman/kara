/// kara-config: configuration parsing for the kara desktop environment.
///
/// Parses the custom block-based config format used by kara-gate (compositor).
/// Supports `$variable` expansion, `{1-9}` range expansion in keybinds,
/// `include` directives, and hot reload.

pub mod types;
pub mod parse;
pub mod keybind;

pub use types::*;
pub use parse::{load_config, load_default_config, default_config_path};
