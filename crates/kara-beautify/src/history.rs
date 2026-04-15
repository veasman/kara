//! Theme apply history — small JSON ring buffer at
//! `~/.local/state/kara/history.json`.
//!
//! Every time beautify commits a theme (`apply` or daemon
//! SetTheme/SetVariant/CycleVariant), it pushes a HistoryEntry onto
//! the front of this buffer. `undo` pops the head, moves it onto an
//! in-memory redo stack, and reapplies the next entry. `redo` mirrors.
//!
//! The buffer is capped at HISTORY_MAX entries (default 20) so the
//! file stays small. Persistence across reboots means you can
//! `kara-beautify undo` a commit from yesterday.
//!
//! The redo stack is intentionally in-memory only — once you exit
//! the daemon (or haven't started one), redo resets. That matches
//! user intuition: redo is a short-term "I just undid and want it
//! back" affordance, not a long-term history.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::Result;
use chrono::Utc;
use serde::{Deserialize, Serialize};

const HISTORY_MAX: usize = 20;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryEntry {
    pub theme: String,
    #[serde(default)]
    pub variant: Option<String>,
    /// Wallpaper per output. Empty when wallpaper state isn't
    /// tracked yet (pre-D). Key is output name ("eDP-1") or "*" for
    /// mirror; value is absolute path.
    #[serde(default)]
    pub wallpapers: std::collections::BTreeMap<String, PathBuf>,
    /// RFC3339 timestamp in UTC.
    pub timestamp: String,
}

impl HistoryEntry {
    pub fn new(theme: &str, variant: Option<&str>) -> Self {
        Self {
            theme: theme.to_string(),
            variant: variant.map(|s| s.to_string()),
            wallpapers: Default::default(),
            timestamp: Utc::now().to_rfc3339(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct History {
    pub entries: Vec<HistoryEntry>,
}

impl History {
    pub fn load(path: &Path) -> Result<Self> {
        if !path.is_file() {
            return Ok(Self::default());
        }
        let raw = fs::read_to_string(path)?;
        let history: Self = serde_json::from_str(&raw).unwrap_or_default();
        Ok(history)
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let raw = serde_json::to_string_pretty(self)?;
        fs::write(path, raw)?;
        Ok(())
    }

    /// Push a new entry to the front. Caps at HISTORY_MAX. If the
    /// head already matches the new entry (same theme + variant),
    /// we skip — repeat commits of the same state don't pollute.
    pub fn push(&mut self, entry: HistoryEntry) {
        if let Some(head) = self.entries.first() {
            if head.theme == entry.theme && head.variant == entry.variant {
                return;
            }
        }
        self.entries.insert(0, entry);
        if self.entries.len() > HISTORY_MAX {
            self.entries.truncate(HISTORY_MAX);
        }
    }

    /// Pop the head entry and return it. None when history is empty.
    /// The NEW head (entries[0] after pop) is what caller should
    /// reapply as the "undone" state.
    pub fn pop_front(&mut self) -> Option<HistoryEntry> {
        if self.entries.is_empty() {
            None
        } else {
            Some(self.entries.remove(0))
        }
    }

    pub fn head(&self) -> Option<&HistoryEntry> {
        self.entries.first()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_caps_at_max() {
        let mut h = History::default();
        for i in 0..(HISTORY_MAX + 10) {
            h.push(HistoryEntry::new(&format!("theme-{i}"), None));
        }
        assert_eq!(h.entries.len(), HISTORY_MAX);
        // Newest is first.
        assert_eq!(h.entries[0].theme, format!("theme-{}", HISTORY_MAX + 9));
    }

    #[test]
    fn duplicate_head_is_skipped() {
        let mut h = History::default();
        h.push(HistoryEntry::new("default", Some("nord")));
        h.push(HistoryEntry::new("default", Some("nord")));
        h.push(HistoryEntry::new("default", Some("nord")));
        assert_eq!(h.entries.len(), 1);
    }

    #[test]
    fn different_variant_is_new_entry() {
        let mut h = History::default();
        h.push(HistoryEntry::new("default", Some("nord")));
        h.push(HistoryEntry::new("default", Some("vague")));
        assert_eq!(h.entries.len(), 2);
        assert_eq!(h.entries[0].variant.as_deref(), Some("vague"));
    }

    #[test]
    fn pop_returns_head_and_shrinks() {
        let mut h = History::default();
        h.push(HistoryEntry::new("a", None));
        h.push(HistoryEntry::new("b", None));
        let popped = h.pop_front().unwrap();
        assert_eq!(popped.theme, "b");
        assert_eq!(h.entries.len(), 1);
        assert_eq!(h.entries[0].theme, "a");
    }
}
