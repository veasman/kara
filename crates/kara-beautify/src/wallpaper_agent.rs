use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};

use anyhow::Result;
use crate::state::paths::KaraPaths;
use crate::state::runtime::{write_current_wallpaper, write_theme_wallpaper};

/// Tell kara-gate to reload the wallpaper via its IPC socket.
/// Best-effort — if the compositor isn't running or the socket
/// isn't reachable, we silently skip. kara-gate will pick up the
/// new wallpaper on its next startup via `load_startup_wallpaper`
/// reading the state files we just wrote.
fn notify_kara_gate(wallpaper_path: &Path) {
    let path_string = wallpaper_path.display().to_string();
    if let Ok(mut client) = kara_ipc::IpcClient::connect() {
        let req = kara_ipc::Request::WallpaperChanged { path: path_string };
        let _ = client.request(&req);
    }
}

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

    // Ask kara-gate to swap its in-memory wallpaper texture. Silent
    // no-op if the compositor isn't running — startup code reads the
    // state files we just wrote as the initial value.
    notify_kara_gate(wallpaper_path);

    // Legacy X11 wallpaper setter for users still running vwm. Harmless
    // on Wayland since xwallpaper silently fails with no display.
    let _ = Command::new("xwallpaper")
        .args(["--zoom", &wallpaper_path.display().to_string()])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();

    Ok(())
}
