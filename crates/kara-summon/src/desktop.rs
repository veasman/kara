//! .desktop file discovery and parsing for XDG applications.

use std::collections::HashSet;
use std::fs;
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct DesktopEntry {
    pub name: String,
    pub exec: String,
    pub comment: Option<String>,
    pub terminal: bool,
}

/// Discover all .desktop application entries from XDG data directories.
pub fn discover() -> Vec<DesktopEntry> {
    let mut dirs = Vec::new();

    // User-local applications first (higher priority)
    if let Some(home) = dirs::data_local_dir() {
        dirs.push(home.join("applications"));
    }

    // XDG_DATA_DIRS (system-wide)
    let data_dirs = std::env::var("XDG_DATA_DIRS")
        .unwrap_or_else(|_| "/usr/local/share:/usr/share".to_string());
    for dir in data_dirs.split(':') {
        if !dir.is_empty() {
            dirs.push(PathBuf::from(dir).join("applications"));
        }
    }

    let mut seen_names = HashSet::new();
    let mut entries = Vec::new();

    for dir in &dirs {
        let read_dir = match fs::read_dir(dir) {
            Ok(r) => r,
            Err(_) => continue,
        };

        for entry in read_dir.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("desktop") {
                continue;
            }

            if let Some(de) = parse_desktop_file(&path) {
                if !seen_names.contains(&de.name) {
                    seen_names.insert(de.name.clone());
                    entries.push(de);
                }
            }
        }
    }

    entries.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    entries
}

/// Parse a single .desktop file.
fn parse_desktop_file(path: &std::path::Path) -> Option<DesktopEntry> {
    let content = fs::read_to_string(path).ok()?;

    let mut in_desktop_entry = false;
    let mut name = None;
    let mut exec = None;
    let mut comment = None;
    let mut terminal = false;
    let mut no_display = false;
    let mut hidden = false;
    let mut entry_type = None;

    for line in content.lines() {
        let line = line.trim();

        if line.starts_with('[') {
            in_desktop_entry = line == "[Desktop Entry]";
            continue;
        }

        if !in_desktop_entry {
            continue;
        }

        if let Some((key, value)) = line.split_once('=') {
            let key = key.trim();
            let value = value.trim();
            match key {
                "Name" => name = Some(value.to_string()),
                "Exec" => exec = Some(strip_field_codes(value)),
                "Comment" => comment = Some(value.to_string()),
                "Terminal" => terminal = value.eq_ignore_ascii_case("true"),
                "NoDisplay" => no_display = value.eq_ignore_ascii_case("true"),
                "Hidden" => hidden = value.eq_ignore_ascii_case("true"),
                "Type" => entry_type = Some(value.to_string()),
                _ => {}
            }
        }
    }

    // Skip non-application entries, hidden, or NoDisplay
    if entry_type.as_deref() != Some("Application") {
        return None;
    }
    if no_display || hidden {
        return None;
    }

    let name = name?;
    let exec = exec?;

    Some(DesktopEntry { name, exec, comment, terminal })
}

/// Strip %f, %F, %u, %U, etc. field codes from Exec value.
fn strip_field_codes(exec: &str) -> String {
    let mut result = String::with_capacity(exec.len());
    let mut chars = exec.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '%' {
            // Skip the next character (the field code letter)
            chars.next();
        } else {
            result.push(c);
        }
    }
    result.trim().to_string()
}
