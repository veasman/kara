//! Locate the active Floorp / Firefox profile directory.
//!
//! Floorp stores a `profiles.ini` at `~/.floorp/profiles.ini` (and
//! Firefox at `~/.mozilla/firefox/profiles.ini`). The file looks
//! roughly like:
//!
//! ```ini
//! [Install614411E63160022F]
//! Default=lec3lkuh.default-default-1775107091913
//! Locked=1
//!
//! [Profile0]
//! Name=default
//! IsRelative=1
//! Path=lec3lkuh.default-default-1775107091913
//! Default=1
//! ```
//!
//! The `[Install*]` section's `Default=` points at the profile
//! Floorp is actually running with when launched without
//! `--profile`. If there are multiple Install sections the first
//! one wins — Floorp itself uses the install-dir hash in the
//! section name to disambiguate, but for our purposes any will do.
//!
//! If no Install section exists (rare / legacy configs), we fall
//! back to whichever `[Profile*]` block has `Default=1`.
//!
//! Returns the absolute path to the profile directory.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

pub fn find_active_profile(floorp_root: &Path) -> Result<Option<PathBuf>> {
    let ini_path = floorp_root.join("profiles.ini");
    if !ini_path.is_file() {
        return Ok(None);
    }

    let content = fs::read_to_string(&ini_path)
        .with_context(|| format!("reading {}", ini_path.display()))?;

    let mut install_default: Option<String> = None;
    let mut profile_default_path: Option<String> = None;
    let mut profile_default_is_default: bool = false;

    let mut section: Option<String> = None;
    let mut cur_path: Option<String> = None;
    let mut cur_is_default: bool = false;

    let commit_profile = |section: &Option<String>,
                          cur_path: &mut Option<String>,
                          cur_is_default: &mut bool,
                          profile_default_path: &mut Option<String>,
                          profile_default_is_default: &mut bool| {
        if let Some(s) = section {
            if s.starts_with("Profile") {
                if *cur_is_default {
                    if let Some(p) = cur_path.take() {
                        *profile_default_path = Some(p);
                        *profile_default_is_default = true;
                    }
                }
            }
        }
        *cur_path = None;
        *cur_is_default = false;
    };

    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }

        if line.starts_with('[') && line.ends_with(']') {
            // Closing the previous section — commit if it was a profile.
            commit_profile(
                &section,
                &mut cur_path,
                &mut cur_is_default,
                &mut profile_default_path,
                &mut profile_default_is_default,
            );
            section = Some(line[1..line.len() - 1].to_string());
            continue;
        }

        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        let value = value.trim();

        match section.as_deref() {
            Some(s) if s.starts_with("Install") => {
                if key == "Default" && install_default.is_none() {
                    install_default = Some(value.to_string());
                }
            }
            Some(s) if s.starts_with("Profile") => {
                if key == "Path" {
                    cur_path = Some(value.to_string());
                } else if key == "Default" && value == "1" {
                    cur_is_default = true;
                }
            }
            _ => {}
        }
    }
    // Final flush.
    commit_profile(
        &section,
        &mut cur_path,
        &mut cur_is_default,
        &mut profile_default_path,
        &mut profile_default_is_default,
    );

    let relative = install_default
        .or(profile_default_path)
        .map(|p| floorp_root.join(p));

    Ok(relative.filter(|p| p.is_dir()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mkroot() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        tmp
    }

    #[test]
    fn returns_none_when_profiles_ini_missing() {
        let tmp = mkroot();
        assert!(find_active_profile(tmp.path()).unwrap().is_none());
    }

    #[test]
    fn picks_install_default_over_profile_default() {
        let tmp = mkroot();
        let profile_a = tmp.path().join("aaa.default");
        let profile_b = tmp.path().join("bbb.default-default");
        fs::create_dir_all(&profile_a).unwrap();
        fs::create_dir_all(&profile_b).unwrap();

        let ini = "\
[Install614411E63160022F]
Default=bbb.default-default
Locked=1

[Profile1]
Name=default
IsRelative=1
Path=aaa.default
Default=1

[Profile0]
Name=default-default
IsRelative=1
Path=bbb.default-default

[General]
StartWithLastProfile=1
Version=2
";
        fs::write(tmp.path().join("profiles.ini"), ini).unwrap();
        let got = find_active_profile(tmp.path()).unwrap();
        assert_eq!(got, Some(profile_b));
    }

    #[test]
    fn falls_back_to_profile_default_when_no_install_section() {
        let tmp = mkroot();
        let profile_a = tmp.path().join("aaa.default");
        fs::create_dir_all(&profile_a).unwrap();

        let ini = "\
[Profile0]
Name=default
IsRelative=1
Path=aaa.default
Default=1
";
        fs::write(tmp.path().join("profiles.ini"), ini).unwrap();
        let got = find_active_profile(tmp.path()).unwrap();
        assert_eq!(got, Some(profile_a));
    }

    #[test]
    fn returns_none_if_resolved_profile_dir_missing() {
        let tmp = mkroot();
        let ini = "\
[Install1]
Default=missing-profile
";
        fs::write(tmp.path().join("profiles.ini"), ini).unwrap();
        assert!(find_active_profile(tmp.path()).unwrap().is_none());
    }
}
