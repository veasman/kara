use std::fs;
use std::path::{Path, PathBuf};

use anyhow::Result;
use kara_theme::resolve_theme;
use kara_theme::render::{
    floorp::render_floorp_user_js, foot::render_foot_theme, fzf::render_fzf_theme,
    gtk::gtk_settings_pairs, kitty::render_kitty_theme, nvim::render_nvim_theme,
    session::render_session_theme, tmux::render_tmux_theme,
    kara_gate::render_kara_gate_theme,
};
use kara_theme::ThemeSpec;

use crate::config::BeautifyConfig;
use crate::ini_patch::patch_ini_section;
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
            ("foot".to_string(), paths.foot_theme_path()),
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
    // Per-consumer opt-outs from ~/.config/kara/kara-beautify.toml.
    // Every flag defaults to true so no config = current behavior.
    let user_cfg = BeautifyConfig::load(paths)?;
    let c = &user_cfg.consumers;

    let mut reload_plan = ReloadPlan::default();

    let kara_gate = render_kara_gate_theme(&resolved);
    let kitty = render_kitty_theme(&resolved);
    let foot = render_foot_theme(&resolved);
    let nvim = render_nvim_theme(&resolved);
    let tmux = render_tmux_theme(&resolved);
    let fzf = render_fzf_theme(&resolved);
    let session = render_session_theme(&resolved);
    let floorp_user_js = render_floorp_user_js(&resolved);
    let gtk_pairs = gtk_settings_pairs(&resolved);
    let gtk_pairs_ref: Vec<(&str, String)> =
        gtk_pairs.iter().map(|(k, v)| (*k, v.clone())).collect();

    // Resolve the Floorp user.js target — None if the user has no
    // profile yet or set consumers.floorp = false.
    let floorp_user_js_path =
        if c.floorp {
            match crate::floorp_profile::find_active_profile(&paths.floorp_root())? {
                Some(profile) => Some(profile.join("user.js")),
                None => None,
            }
        } else {
            None
        };

    if options.dry_run {
        println!("dry-run: {}", spec.meta.name);
        if c.kara_gate {
            println!("  would write: {}", paths.kara_gate_theme_path().display());
        }
        if c.kitty {
            println!("  would write: {}", paths.kitty_theme_path().display());
        }
        if c.foot {
            println!("  would write: {}", paths.foot_theme_path().display());
        }
        if c.nvim {
            println!("  would write: {}", paths.nvim_theme_path().display());
        }
        if c.tmux {
            println!("  would write: {}", paths.tmux_theme_path().display());
        }
        if c.fzf {
            println!("  would write: {}", paths.fzf_theme_path().display());
        }
        if c.session {
            println!("  would write: {}", paths.session_theme_path().display());
        }
        if c.gtk {
            println!(
                "  would patch: {} (5 keys)",
                paths.gtk3_settings_path().display()
            );
            println!(
                "  would patch: {} (5 keys)",
                paths.gtk4_settings_path().display()
            );
        }
        if let Some(ref p) = floorp_user_js_path {
            println!("  would write: {}", p.display());
        } else if c.floorp {
            println!("  would write: (floorp enabled but no profile found)");
        }
        println!(
            "  (consumers: kara_gate={} kitty={} foot={} nvim={} tmux={} fzf={} session={} gtk={} floorp={})",
            c.kara_gate, c.kitty, c.foot, c.nvim, c.tmux, c.fzf, c.session, c.gtk, c.floorp
        );

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

    // Gate every write on its consumer flag. Disabled consumers skip
    // the render output AND the reload signal that would fire later.
    if c.kara_gate {
        reload_plan.kara_gate =
            write_if_changed(&paths.kara_gate_theme_path(), &kara_gate)?;
    }
    if c.kitty {
        reload_plan.kitty = write_if_changed(&paths.kitty_theme_path(), &kitty)?;
    }
    if c.foot {
        reload_plan.foot = write_if_changed(&paths.foot_theme_path(), &foot)?;
    }
    if c.nvim {
        reload_plan.nvim = write_if_changed(&paths.nvim_theme_path(), &nvim)?;
    }
    if c.tmux {
        reload_plan.tmux = write_if_changed(&paths.tmux_theme_path(), &tmux)?;
    }
    if c.fzf {
        let _ = write_if_changed(&paths.fzf_theme_path(), &fzf)?;
    }
    if c.session {
        let _ = write_if_changed(&paths.session_theme_path(), &session)?;
    }

    if c.gtk {
        // Patch in place so user-added keys (font-name, button order,
        // overlay-scrolling, etc.) survive. See crate::ini_patch.
        let _ =
            patch_ini_section(&paths.gtk3_settings_path(), "Settings", &gtk_pairs_ref)?;
        let _ =
            patch_ini_section(&paths.gtk4_settings_path(), "Settings", &gtk_pairs_ref)?;
        sync_desktop_appearance(&resolved)?;
    }

    if let Some(ref path) = floorp_user_js_path {
        // user.js is full-file-owned by kara-beautify since we don't
        // want to merge with arbitrary user prefs — if the user wants
        // custom prefs alongside, they can set consumers.floorp = false
        // and maintain their own file. Atomic write via write_if_changed.
        let _ = write_if_changed(path, &floorp_user_js)?;
    }
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
