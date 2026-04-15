use std::fs;
use std::path::{Path, PathBuf};

use anyhow::Result;
use kara_theme::resolve_theme;
use kara_theme::render::{
    fzf::render_fzf_theme, gtk::render_gtk_settings,
    kitty::render_kitty_theme, nvim::render_nvim_theme,
    session::render_session_theme, tmux::render_tmux_theme,
    kara_gate::render_kara_gate_theme,
};
use kara_theme::ThemeSpec;

use crate::state::paths::KaraPaths;
use crate::state::runtime::{read_theme_wallpaper, write_current_theme};
use crate::desktop::sync_desktop_appearance;
use crate::reload::{ReloadPlan, apply_runtime_reloads};
use crate::wallpaper_agent::apply_wallpaper;

#[derive(Debug, Clone, Copy)]
pub struct ApplyOptions {
    pub reload: bool,
    pub dry_run: bool,
}

impl Default for ApplyOptions {
    fn default() -> Self {
        Self {
            reload: true,
            dry_run: false,
        }
    }
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct DebugInfo {
    pub theme_name: String,
    pub theme_file: PathBuf,
    pub theme_root: PathBuf,
    pub selected_wallpaper: Option<PathBuf>,
    pub generated_paths: Vec<(String, PathBuf)>,
}

pub fn debug_theme_file(
    theme_file: &Path,
    theme_root: &Path,
    paths: &KaraPaths,
    variant: Option<&str>,
) -> Result<DebugInfo> {
    let spec = ThemeSpec::load_from_file(theme_file)?;
    let _ = variant; // reserved for per-variant wallpaper preview
    let selected_wallpaper = selected_wallpaper(
        theme_root,
        paths,
        &spec.meta.name,
        spec.wallpaper.default.as_deref(),
    );

    Ok(DebugInfo {
        theme_name: spec.meta.name,
        theme_file: theme_file.to_path_buf(),
        theme_root: theme_root.to_path_buf(),
        selected_wallpaper,
        generated_paths: vec![
            ("kara-gate".to_string(), paths.kara_gate_theme_path()),
            ("kitty".to_string(), paths.kitty_theme_path()),
            ("nvim".to_string(), paths.nvim_theme_path()),
            ("tmux".to_string(), paths.tmux_theme_path()),
            ("fzf".to_string(), paths.fzf_theme_path()),
            ("session".to_string(), paths.session_theme_path()),
            ("gtk3".to_string(), paths.gtk3_settings_path()),
            ("gtk4".to_string(), paths.gtk4_settings_path()),
        ],
    })
}

pub fn apply_theme_file(
    theme_file: &Path,
    theme_root: &Path,
    paths: &KaraPaths,
    variant: Option<&str>,
    options: ApplyOptions,
) -> Result<()> {
    paths.ensure_runtime_dirs()?;

    let spec = ThemeSpec::load_from_file(theme_file)?;
    let resolved = resolve_theme(&spec, variant)?;

    let mut reload_plan = ReloadPlan::default();

    let kara_gate = render_kara_gate_theme(&resolved);
    let kitty = render_kitty_theme(&resolved);
    let nvim = render_nvim_theme(&resolved);
    let tmux = render_tmux_theme(&resolved);
    let fzf = render_fzf_theme(&resolved);
    let session = render_session_theme(&resolved);
    let gtk = render_gtk_settings(&resolved);

    if options.dry_run {
        println!("dry-run: {}", spec.meta.name);
        println!("  would write: {}", paths.kara_gate_theme_path().display());
        println!("  would write: {}", paths.kitty_theme_path().display());
        println!("  would write: {}", paths.nvim_theme_path().display());
        println!("  would write: {}", paths.tmux_theme_path().display());
        println!("  would write: {}", paths.fzf_theme_path().display());
        println!("  would write: {}", paths.session_theme_path().display());
        println!("  would write: {}", paths.gtk3_settings_path().display());
        println!("  would write: {}", paths.gtk4_settings_path().display());

        if let Some(wallpaper) = selected_wallpaper(
            theme_root,
            paths,
            &spec.meta.name,
            resolved.wallpaper.as_deref(),
        ) {
            println!("  would apply wallpaper: {}", wallpaper.display());
        } else {
            println!("  would apply wallpaper: none");
        }

        return Ok(());
    }

    reload_plan.kara_gate = write_if_changed(&paths.kara_gate_theme_path(), &kara_gate)?;
    reload_plan.kitty = write_if_changed(&paths.kitty_theme_path(), &kitty)?;
    reload_plan.nvim = write_if_changed(&paths.nvim_theme_path(), &nvim)?;
    reload_plan.tmux = write_if_changed(&paths.tmux_theme_path(), &tmux)?;
    let _ = write_if_changed(&paths.fzf_theme_path(), &fzf)?;
    let _ = write_if_changed(&paths.session_theme_path(), &session)?;

    let _ = write_if_changed(&paths.gtk3_settings_path(), &gtk)?;
    let _ = write_if_changed(&paths.gtk4_settings_path(), &gtk)?;

    sync_desktop_appearance(&resolved)?;
    // write_current_theme takes the bare theme name (not "theme:variant"
    // because the state file is also how kara-gate and kara-summon
    // identify which theme is active, and they shouldn't have to parse
    // the colon form).
    write_current_theme(paths, &spec.meta.name)?;
    if let Some(v) = variant {
        crate::state::runtime::write_current_variant(paths, v)?;
    } else {
        crate::state::runtime::clear_current_variant(paths)?;
    }

    if let Some(wallpaper_path) = selected_wallpaper(
        theme_root,
        paths,
        &spec.meta.name,
        resolved.wallpaper.as_deref(),
    ) {
        apply_wallpaper(paths, &resolved.name, &wallpaper_path)?;
    } else if let Some(wallpaper) = resolved.wallpaper.as_ref() {
        eprintln!(
            "[kara] warning: wallpaper '{}' not found for theme '{}'; skipping wallpaper apply",
            wallpaper, resolved.name
        );
    }

    if options.reload {
        apply_runtime_reloads(reload_plan, &paths.tmux_theme_path());
    }

    println!("applied theme: {}", spec.meta.name);
    Ok(())
}

fn write_if_changed(path: &Path, content: &str) -> Result<bool> {
    let old = fs::read_to_string(path).ok();
    if old.as_deref() == Some(content) {
        return Ok(false);
    }

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    fs::write(path, content)?;
    Ok(true)
}

fn selected_wallpaper(
    theme_root: &Path,
    paths: &KaraPaths,
    theme_name: &str,
    fallback_name: Option<&str>,
) -> Option<PathBuf> {
    if let Ok(Some(saved)) = read_theme_wallpaper(paths, theme_name) {
        if saved.is_file() {
            return Some(saved);
        }
    }

    if let Some(name) = fallback_name {
        resolve_wallpaper_path(theme_root, name)
    } else {
        None
    }
}

fn resolve_wallpaper_path(theme_root: &Path, value: &str) -> Option<PathBuf> {
    let exact = theme_root.join("wallpapers").join(value);
    if exact.is_file() {
        return Some(exact);
    }

    let no_ext = theme_root.join("wallpapers").join(value);
    for ext in [
        "png", "jpg", "jpeg", "webp", "gif", "bmp", "tiff", "tif", "avif",
    ] {
        let candidate = no_ext.with_extension(ext);
        if candidate.is_file() {
            return Some(candidate);
        }
    }

    None
}
