//! kara-beautify.toml — per-user runtime config.
//!
//! Lives at `$XDG_CONFIG_HOME/kara/kara-beautify.toml` (falling back
//! to `~/.config/kara/kara-beautify.toml`). Optional: if the file
//! doesn't exist, we use defaults and every consumer is enabled.
//!
//! The primary use case is opting out of individual consumers. A user
//! with their own dotfile-managed foot.ini who doesn't want beautify
//! touching it can set `foot = false` in the `[consumers]` block and
//! beautify skips the foot renderer entirely — no file written, no
//! reload signal sent. Every consumer has an independent opt-out so
//! users can carve the boundary wherever their dotfiles end and
//! beautify-owned state begins.
//!
//! Example:
//!
//! ```toml
//! [consumers]
//! # kara_gate = true   # default
//! foot = false          # I manage foot.ini in my dotfiles
//! tmux = false          # I manage tmux.conf in my dotfiles
//! # everything else stays true
//! ```

use std::fs;
use std::path::Path;

use anyhow::Result;
use serde::Deserialize;

use crate::state::paths::KaraPaths;

/// User-facing beautify config. All fields optional, all defaults
/// set via serde's `#[serde(default)]` so a completely empty file
/// (or no file at all) is valid and produces a fully-enabled config.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct BeautifyConfig {
    #[serde(default)]
    pub consumers: ConsumerConfig,
}

/// Which renderers fire on `kara-beautify apply`. Every flag defaults
/// to `true` — a user who never writes the config gets the same
/// behavior as before. Setting a flag to `false` makes beautify skip
/// the corresponding render + write + reload pipeline.
///
/// New consumers get a new field here + a default_true() entry.
#[derive(Debug, Clone, Deserialize)]
pub struct ConsumerConfig {
    #[serde(default = "default_true")]
    pub kara_gate: bool,
    #[serde(default = "default_true")]
    pub kitty: bool,
    #[serde(default = "default_true")]
    pub foot: bool,
    #[serde(default = "default_true")]
    pub nvim: bool,
    #[serde(default = "default_true")]
    pub tmux: bool,
    #[serde(default = "default_true")]
    pub fzf: bool,
    #[serde(default = "default_true")]
    pub session: bool,
    #[serde(default = "default_true")]
    pub gtk: bool,
    #[serde(default = "default_true")]
    pub floorp: bool,
    /// Patch the active Thunderbird profile's user.js with the same
    /// dark-mode prefs we write for Floorp so the message list,
    /// compose window, and settings UI follow the active kara theme
    /// instead of defaulting to the system light palette.
    #[serde(default = "default_true")]
    pub thunderbird: bool,
}

impl Default for ConsumerConfig {
    fn default() -> Self {
        Self {
            kara_gate: true,
            kitty: true,
            foot: true,
            nvim: true,
            tmux: true,
            fzf: true,
            session: true,
            gtk: true,
            floorp: true,
            thunderbird: true,
        }
    }
}

fn default_true() -> bool {
    true
}

impl BeautifyConfig {
    pub fn load(paths: &KaraPaths) -> Result<Self> {
        let path = Self::config_file(paths);
        Self::load_from(&path)
    }

    pub fn config_file(paths: &KaraPaths) -> std::path::PathBuf {
        paths.config_home.join("kara").join("kara-beautify.toml")
    }

    fn load_from(path: &Path) -> Result<Self> {
        if !path.is_file() {
            return Ok(Self::default());
        }
        let raw = fs::read_to_string(path)?;
        let cfg: BeautifyConfig = toml::from_str(&raw)?;
        Ok(cfg)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn missing_file_yields_all_enabled() {
        let tmp = std::env::temp_dir().join("kara-beautify-missing.toml");
        let _ = fs::remove_file(&tmp);
        let cfg = BeautifyConfig::load_from(&tmp).unwrap();
        assert!(cfg.consumers.kara_gate);
        assert!(cfg.consumers.foot);
        assert!(cfg.consumers.tmux);
    }

    #[test]
    fn partial_file_overrides_only_named_keys() {
        let tmp = std::env::temp_dir().join("kara-beautify-partial.toml");
        {
            let mut f = fs::File::create(&tmp).unwrap();
            writeln!(f, "[consumers]\nfoot = false\ntmux = false").unwrap();
        }
        let cfg = BeautifyConfig::load_from(&tmp).unwrap();
        assert!(cfg.consumers.kara_gate, "untouched keys should stay true");
        assert!(!cfg.consumers.foot, "explicit false should apply");
        assert!(!cfg.consumers.tmux, "explicit false should apply");
        let _ = fs::remove_file(&tmp);
    }

    #[test]
    fn empty_file_is_valid_default() {
        let tmp = std::env::temp_dir().join("kara-beautify-empty.toml");
        fs::File::create(&tmp).unwrap();
        let cfg = BeautifyConfig::load_from(&tmp).unwrap();
        assert!(cfg.consumers.kara_gate);
        let _ = fs::remove_file(&tmp);
    }
}
