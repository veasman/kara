//! Minimal INI patcher for GTK `settings.ini` and similar files.
//!
//! The problem: GTK's `settings.ini` has no include mechanism, so any
//! theming tool that writes the whole file destroys whatever other
//! keys the user had there (font-name, overlay-scrolling, button
//! order, etc.). Users managing their config via a dotfiles repo
//! cannot tolerate that — beautify would clobber every dotfile `make
//! install`.
//!
//! This patcher reads an existing INI file, updates ONLY the specified
//! keys inside a given section, and writes it back preserving every
//! other line in order. Unknown sections, comments, blank lines, and
//! any keys we don't own survive untouched. If the target section or
//! any of the target keys don't exist yet, we append them cleanly.
//!
//! The file is written via a temporary rename so an interrupted run
//! never leaves a half-baked settings.ini on disk.

use std::collections::HashSet;
use std::fs;
use std::io::Write;
use std::path::Path;

use anyhow::{Context, Result};

/// Patch (or create) `path`, setting every `(key, value)` pair inside
/// `[section]`. Returns `true` if the file content changed, `false`
/// if the patched output was byte-identical to what was already on disk
/// (so callers can skip reload signals in the no-op case).
pub fn patch_ini_section(
    path: &Path,
    section: &str,
    pairs: &[(&str, String)],
) -> Result<bool> {
    let existing = match fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    };

    let new_content = patch_ini_string(&existing, section, pairs);
    if new_content == existing {
        return Ok(false);
    }

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    // Write via tempfile + rename so a crash mid-write can't leave
    // the target settings.ini truncated.
    let tmp_path = path.with_extension(format!(
        "{}.kara-tmp",
        path.extension()
            .map(|e| e.to_string_lossy().to_string())
            .unwrap_or_default()
    ));
    {
        let mut tmp = fs::File::create(&tmp_path)
            .with_context(|| format!("creating {}", tmp_path.display()))?;
        tmp.write_all(new_content.as_bytes())?;
        tmp.sync_all()?;
    }
    fs::rename(&tmp_path, path)
        .with_context(|| format!("renaming {} → {}", tmp_path.display(), path.display()))?;
    Ok(true)
}

/// Pure-string version of the patcher — the core logic is split out
/// so unit tests don't touch the filesystem.
fn patch_ini_string(existing: &str, section: &str, pairs: &[(&str, String)]) -> String {
    if pairs.is_empty() {
        return existing.to_string();
    }

    let target_header = format!("[{section}]");
    let owned_keys: HashSet<&str> = pairs.iter().map(|(k, _)| *k).collect();
    let mut overridden: HashSet<&str> = HashSet::new();

    let mut out_lines: Vec<String> = Vec::new();
    let mut in_target_section = false;
    let mut saw_target_section = false;
    let mut target_section_last_line: Option<usize> = None;

    for line in existing.lines() {
        let trimmed = line.trim();

        // Section headers
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            // Leaving the previous section — if it was the target, the
            // last-line marker stays pointing at the last kept line.
            if in_target_section {
                in_target_section = false;
            }

            if trimmed == target_header {
                in_target_section = true;
                saw_target_section = true;
            }
            out_lines.push(line.to_string());
            continue;
        }

        if in_target_section {
            // Is this a `key=value` line where key is one we own?
            if let Some(eq) = line.find('=') {
                let (raw_key, _) = line.split_at(eq);
                let key = raw_key.trim();
                if owned_keys.contains(key) {
                    // Find the matching pair and substitute.
                    if let Some((_, new_value)) = pairs.iter().find(|(k, _)| *k == key) {
                        out_lines.push(format!("{key}={new_value}"));
                        overridden.insert(key);
                        target_section_last_line = Some(out_lines.len() - 1);
                        continue;
                    }
                }
            }

            // Non-owned line inside target section — keep as is, track
            // as the last line of the section so we can append new keys
            // after it.
            out_lines.push(line.to_string());
            target_section_last_line = Some(out_lines.len() - 1);
            continue;
        }

        out_lines.push(line.to_string());
    }

    // If the target section existed, append any owned keys that weren't
    // already in it, right after the section's last kept line. If not,
    // append a fresh section at the end.
    let missing: Vec<&(&str, String)> = pairs
        .iter()
        .filter(|(k, _)| !overridden.contains(k))
        .collect();

    if !missing.is_empty() {
        if saw_target_section {
            let insert_at = match target_section_last_line {
                Some(idx) => idx + 1,
                None => {
                    // Target section was present but had no lines after
                    // the header — find the header line and insert after it.
                    out_lines
                        .iter()
                        .position(|l| l.trim() == target_header)
                        .map(|i| i + 1)
                        .unwrap_or_else(|| out_lines.len())
                }
            };
            let mut new_lines = Vec::with_capacity(missing.len());
            for (k, v) in &missing {
                new_lines.push(format!("{k}={v}"));
            }
            out_lines.splice(insert_at..insert_at, new_lines);
        } else {
            if !out_lines.is_empty() && !out_lines.last().map_or(true, |l| l.is_empty()) {
                out_lines.push(String::new());
            }
            out_lines.push(target_header);
            for (k, v) in &missing {
                out_lines.push(format!("{k}={v}"));
            }
        }
    }

    // Preserve trailing newline convention — emit lines joined by \n
    // with a final \n so GTK's parser doesn't choke.
    let mut joined = out_lines.join("\n");
    if !joined.ends_with('\n') {
        joined.push('\n');
    }
    joined
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creates_file_when_missing() {
        let pairs = vec![
            ("gtk-theme-name", "Adwaita-dark".to_string()),
            ("gtk-application-prefer-dark-theme", "1".to_string()),
        ];
        let out = patch_ini_string("", "Settings", &pairs);
        assert!(out.contains("[Settings]"));
        assert!(out.contains("gtk-theme-name=Adwaita-dark"));
        assert!(out.contains("gtk-application-prefer-dark-theme=1"));
    }

    #[test]
    fn updates_existing_keys_preserves_others() {
        let existing = "[Settings]\n\
            gtk-theme-name=OldTheme\n\
            gtk-font-name=Fira Sans 11\n\
            gtk-overlay-scrolling=true\n";
        let pairs = vec![("gtk-theme-name", "NewTheme".to_string())];
        let out = patch_ini_string(existing, "Settings", &pairs);
        assert!(out.contains("gtk-theme-name=NewTheme"));
        assert!(out.contains("gtk-font-name=Fira Sans 11"));
        assert!(out.contains("gtk-overlay-scrolling=true"));
        assert!(!out.contains("gtk-theme-name=OldTheme"));
    }

    #[test]
    fn appends_missing_keys_to_existing_section() {
        let existing = "[Settings]\ngtk-font-name=Fira Sans 11\n";
        let pairs = vec![
            ("gtk-theme-name", "Adwaita".to_string()),
            ("gtk-cursor-theme-size", "24".to_string()),
        ];
        let out = patch_ini_string(existing, "Settings", &pairs);
        assert!(out.contains("gtk-font-name=Fira Sans 11"));
        assert!(out.contains("gtk-theme-name=Adwaita"));
        assert!(out.contains("gtk-cursor-theme-size=24"));
    }

    #[test]
    fn preserves_unrelated_sections() {
        let existing = "[Other]\nsome-key=value\n\n[Settings]\ngtk-theme-name=Old\n";
        let pairs = vec![("gtk-theme-name", "New".to_string())];
        let out = patch_ini_string(existing, "Settings", &pairs);
        assert!(out.contains("[Other]"));
        assert!(out.contains("some-key=value"));
        assert!(out.contains("gtk-theme-name=New"));
    }

    #[test]
    fn idempotent_when_nothing_changes() {
        let existing = "[Settings]\ngtk-theme-name=NewTheme\n";
        let pairs = vec![("gtk-theme-name", "NewTheme".to_string())];
        let out = patch_ini_string(existing, "Settings", &pairs);
        assert_eq!(out.trim(), existing.trim());
    }
}
