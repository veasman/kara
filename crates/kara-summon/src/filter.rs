//! Fuzzy/prefix filtering of desktop entries and command fallback.

use crate::desktop::DesktopEntry;

/// Filter entries by query. Returns indices into the original Vec.
/// Priority: prefix match on name > substring match on name > substring match on exec.
pub fn filter(entries: &[DesktopEntry], query: &str) -> Vec<usize> {
    if query.is_empty() {
        return (0..entries.len()).collect();
    }

    let q = query.to_lowercase();

    let mut prefix = Vec::new();
    let mut name_sub = Vec::new();
    let mut exec_sub = Vec::new();

    for (i, entry) in entries.iter().enumerate() {
        let name_lower = entry.name.to_lowercase();
        let exec_lower = entry.exec.to_lowercase();

        if name_lower.starts_with(&q) {
            prefix.push(i);
        } else if name_lower.contains(&q) {
            name_sub.push(i);
        } else if exec_lower.contains(&q) {
            exec_sub.push(i);
        }
    }

    prefix.extend(name_sub);
    prefix.extend(exec_sub);
    prefix
}

/// Check if the query looks like a raw command (not an app name).
pub fn is_command(query: &str) -> bool {
    !query.is_empty() && (query.contains('/') || query.contains(' '))
}
