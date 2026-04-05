use std::fs;
use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::state::paths::KaraPaths;

pub fn read_current_theme(paths: &KaraPaths) -> Result<Option<String>> {
    read_trimmed(paths.current_theme_file())
}

pub fn write_current_theme(paths: &KaraPaths, theme: &str) -> Result<()> {
    fs::write(paths.current_theme_file(), format!("{theme}\n"))?;
    Ok(())
}

pub fn read_current_wallpaper(paths: &KaraPaths) -> Result<Option<PathBuf>> {
    Ok(read_trimmed(paths.current_wallpaper_file())?.map(PathBuf::from))
}

pub fn write_current_wallpaper(paths: &KaraPaths, wallpaper: &Path) -> Result<()> {
    fs::write(
        paths.current_wallpaper_file(),
        format!("{}\n", wallpaper.display()),
    )?;
    Ok(())
}

pub fn preview_is_active(paths: &KaraPaths) -> bool {
    paths.preview_active_file().exists()
}

pub fn begin_preview_state(
    paths: &KaraPaths,
    theme: Option<&str>,
    wallpaper: Option<&PathBuf>,
) -> Result<()> {
    if preview_is_active(paths) {
        return Ok(());
    }

    fs::write(paths.preview_active_file(), "1\n")?;

    if let Some(theme) = theme {
        fs::write(paths.preview_original_theme_file(), format!("{theme}\n"))?;
    }

    if let Some(wallpaper) = wallpaper {
        fs::write(
            paths.preview_original_wallpaper_file(),
            format!("{}\n", wallpaper.display()),
        )?;
    }

    Ok(())
}

pub fn clear_preview_state(paths: &KaraPaths) -> Result<()> {
    let _ = fs::remove_file(paths.preview_active_file());
    let _ = fs::remove_file(paths.preview_original_theme_file());
    let _ = fs::remove_file(paths.preview_original_wallpaper_file());
    Ok(())
}

pub fn read_preview_theme(paths: &KaraPaths) -> Result<Option<String>> {
    read_trimmed(paths.preview_original_theme_file())
}

#[allow(dead_code)]
pub fn read_preview_wallpaper(paths: &KaraPaths) -> Result<Option<PathBuf>> {
    Ok(read_trimmed(paths.preview_original_wallpaper_file())?.map(PathBuf::from))
}

pub fn theme_wallpaper_state_file(paths: &KaraPaths, theme: &str) -> PathBuf {
    paths
        .themes_state_dir()
        .join(theme)
        .join("current_wallpaper")
}

pub fn read_theme_wallpaper(paths: &KaraPaths, theme: &str) -> Result<Option<PathBuf>> {
    Ok(read_trimmed(theme_wallpaper_state_file(paths, theme))?.map(PathBuf::from))
}

pub fn write_theme_wallpaper(paths: &KaraPaths, theme: &str, wallpaper: &Path) -> Result<()> {
    let path = theme_wallpaper_state_file(paths, theme);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, format!("{}\n", wallpaper.display()))?;
    Ok(())
}

fn read_trimmed(path: PathBuf) -> Result<Option<String>> {
    if !path.is_file() {
        return Ok(None);
    }

    let raw = fs::read_to_string(path)?;
    let value = raw.trim().to_string();

    if value.is_empty() {
        Ok(None)
    } else {
        Ok(Some(value))
    }
}
