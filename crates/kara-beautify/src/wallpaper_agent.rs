use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};

use anyhow::Result;
use crate::state::paths::KaraPaths;
use crate::state::runtime::{write_current_wallpaper, write_theme_wallpaper};

#[cfg(unix)]
fn symlink_force(target: &Path, link: &Path) -> Result<()> {
    use std::os::unix::fs::symlink;

    if link.exists() || link.is_symlink() {
        let _ = fs::remove_file(link);
        let _ = fs::remove_dir_all(link);
    }

    symlink(target, link)?;
    Ok(())
}

#[cfg(not(unix))]
fn symlink_force(target: &Path, link: &Path) -> Result<()> {
    fs::copy(target, link)?;
    Ok(())
}

pub fn apply_wallpaper(paths: &KaraPaths, theme_name: &str, wallpaper_path: &Path) -> Result<()> {
    if !wallpaper_path.is_file() {
        return Ok(());
    }

    write_current_wallpaper(paths, wallpaper_path)?;
    write_theme_wallpaper(paths, theme_name, wallpaper_path)?;
    symlink_force(wallpaper_path, &paths.current_wallpaper_link())?;

    let _ = Command::new("xwallpaper")
        .args(["--zoom", &wallpaper_path.display().to_string()])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();

    Ok(())
}
