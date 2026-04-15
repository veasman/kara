mod state;
mod wallpaper;
mod config;
mod doctor;
mod apply;
mod desktop;
mod ini_patch;
mod reload;
mod wallpaper_agent;

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::{Args, Parser, Subcommand, ValueEnum};

use crate::apply::{ApplyOptions, apply_theme_file, debug_theme_file};
use crate::doctor::{DoctorStatus, run_doctor_checks};
use crate::state::paths::KaraPaths;
use crate::state::runtime::{
    begin_preview_state, clear_preview_state, preview_is_active, read_current_theme,
    read_current_wallpaper, read_preview_theme, read_theme_wallpaper, write_theme_wallpaper,
};
use crate::wallpaper::{derive_theme_from_image, ranked_swatches_from_image};

use kara_theme::resolve_theme;
use kara_theme::render::{
    foot::render_foot_theme, fzf::render_fzf_theme, gtk::render_gtk_settings,
    kitty::render_kitty_theme, nvim::render_nvim_theme,
    session::render_session_theme, tmux::render_tmux_theme,
    kara_gate::render_kara_gate_theme,
};
use kara_theme::{ThemeSpec, UiMode, validate_spec};
use serde_json::json;

#[derive(Debug, Parser)]
#[command(name = "kara-beautify")]
#[command(about = "palette-first appearance manager")]
struct Cli {
    #[arg(long, default_value = ".")]
    repo_root: PathBuf,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    List,
    ListVariants {
        theme: String,
    },
    Doctor,
    Status,
    Debug(DebugArgs),
    Validate {
        theme: Option<String>,
    },
    Resolve {
        theme: String,
        #[arg(long)]
        variant: Option<String>,
        #[arg(long)]
        json: bool,
    },
    Apply {
        theme: String,
        #[arg(long)]
        variant: Option<String>,
        #[arg(long)]
        no_reload: bool,
        #[arg(long)]
        dry_run: bool,
    },
    Preview {
        theme: String,
        #[arg(long)]
        variant: Option<String>,
        #[arg(long)]
        no_reload: bool,
    },
    Revert {
        #[arg(long)]
        no_reload: bool,
    },
    Wallpaper {
        theme: String,
        #[command(subcommand)]
        command: WallpaperCommands,
    },
    Render {
        theme: String,
        target: RenderTarget,
        #[arg(long)]
        variant: Option<String>,
    },
    DeriveImage {
        image: PathBuf,
        #[arg(long)]
        name: String,
        #[arg(long)]
        mode: Option<ModeArg>,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Debug, Args)]
struct DebugArgs {
    theme: Option<String>,
    #[arg(long)]
    variant: Option<String>,
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Subcommand)]
enum WallpaperCommands {
    List {
        #[arg(long)]
        json: bool,
    },
    Set {
        file: String,
    },
    Current {
        #[arg(long)]
        json: bool,
    },
    Random,
    Next,
    Prev,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum RenderTarget {
    Gtk,
    Kitty,
    Foot,
    Nvim,
    Tmux,
    KaraGate,
    Fzf,
    Session,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum ModeArg {
    Dark,
    Light,
    Auto,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let repo_root = cli.repo_root.canonicalize().unwrap_or(cli.repo_root);
    let paths = KaraPaths::from_env()?;

    match cli.command {
        Commands::List => list_themes(&repo_root, &paths),
        Commands::ListVariants { theme } => list_variants_command(&repo_root, &paths, &theme),
        Commands::Doctor => doctor(&paths),
        Commands::Status => status_command(&paths),
        Commands::Debug(args) => debug_command(&repo_root, &paths, args),
        Commands::Validate { theme } => validate_command(&repo_root, &paths, theme),
        Commands::Resolve {
            theme,
            variant,
            json,
        } => resolve_command(&repo_root, &paths, &theme, variant.as_deref(), json),
        Commands::Apply {
            theme,
            variant,
            no_reload,
            dry_run,
        } => apply_command(
            &repo_root,
            &paths,
            &theme,
            variant.as_deref(),
            no_reload,
            dry_run,
        ),
        Commands::Preview {
            theme,
            variant,
            no_reload,
        } => preview_command(&repo_root, &paths, &theme, variant.as_deref(), no_reload),
        Commands::Revert { no_reload } => revert_command(&repo_root, &paths, no_reload),
        Commands::Wallpaper { theme, command } => {
            wallpaper_command(&repo_root, &paths, &theme, command)
        }
        Commands::Render {
            theme,
            target,
            variant,
        } => render_command(&repo_root, &paths, &theme, target, variant.as_deref()),
        Commands::DeriveImage {
            image,
            name,
            mode,
            json,
        } => derive_image_command(&repo_root, &paths, &image, &name, mode, json),
    }
}

/// Resolve a theme name to its on-disk directory via the XDG search
/// path. Returns the theme dir (containing `theme.toml`). Errors with
/// the full search chain when nothing is found.
fn resolve_theme_dir(repo_root: &Path, paths: &KaraPaths, theme_name: &str) -> Result<PathBuf> {
    if let Some(dir) = paths.find_theme(theme_name, Some(repo_root)) {
        return Ok(dir);
    }
    let searched: Vec<String> = paths
        .theme_search_paths(Some(repo_root))
        .iter()
        .map(|p| format!("  - {}", p.display()))
        .collect();
    anyhow::bail!(
        "theme '{theme_name}' not found. searched:\n{}",
        searched.join("\n")
    );
}

fn theme_file(repo_root: &Path, paths: &KaraPaths, theme_name: &str) -> Result<PathBuf> {
    Ok(resolve_theme_dir(repo_root, paths, theme_name)?.join("theme.toml"))
}

fn theme_root(repo_root: &Path, paths: &KaraPaths, theme_name: &str) -> Result<PathBuf> {
    resolve_theme_dir(repo_root, paths, theme_name)
}

fn theme_wallpapers_dir(repo_root: &Path, paths: &KaraPaths, theme_name: &str) -> Result<PathBuf> {
    Ok(resolve_theme_dir(repo_root, paths, theme_name)?.join("wallpapers"))
}

fn list_themes(repo_root: &Path, paths: &KaraPaths) -> Result<()> {
    use std::collections::BTreeMap;
    // theme name → (search-path index, absolute dir). Lower index wins.
    let mut seen: BTreeMap<String, (usize, PathBuf)> = BTreeMap::new();

    for (idx, base) in paths.theme_search_paths(Some(repo_root)).iter().enumerate() {
        if !base.is_dir() {
            continue;
        }
        for entry in fs::read_dir(base)? {
            let entry = entry?;
            let path = entry.path();
            if path.is_dir() && path.join("theme.toml").is_file() {
                let name = entry.file_name().to_string_lossy().to_string();
                seen.entry(name)
                    .and_modify(|(existing_idx, existing_path)| {
                        if idx < *existing_idx {
                            *existing_idx = idx;
                            *existing_path = path.clone();
                        }
                    })
                    .or_insert((idx, path));
            }
        }
    }

    for (name, (idx, path)) in seen {
        let source = match idx {
            0 => "user",
            1 => "data",
            2 => "repo",
            _ => "system",
        };
        println!("{name:<16} [{source}] {}", path.display());
    }

    Ok(())
}

fn doctor(paths: &KaraPaths) -> Result<()> {
    for check in run_doctor_checks(paths) {
        let status = match check.status {
            DoctorStatus::Pass => "PASS",
            DoctorStatus::Warn => "WARN",
            DoctorStatus::Fail => "FAIL",
        };
        println!("{status:>4}  {:<24} {}", check.name, check.detail);
    }
    Ok(())
}

fn status_command(paths: &KaraPaths) -> Result<()> {
    let current = read_current_theme(paths)?;
    let wallpaper = read_current_wallpaper(paths)?;
    let preview = preview_is_active(paths);

    println!("theme: {}", current.unwrap_or_else(|| "none".to_string()));
    println!(
        "wallpaper: {}",
        wallpaper
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "none".to_string())
    );
    println!("preview_active: {}", if preview { "yes" } else { "no" });

    Ok(())
}

fn debug_command(repo_root: &Path, paths: &KaraPaths, args: DebugArgs) -> Result<()> {
    let theme_name = if let Some(theme) = args.theme {
        theme
    } else if let Some(current) = read_current_theme(paths)? {
        current
    } else {
        anyhow::bail!("no theme provided and no current theme set");
    };

    let file = theme_file(repo_root, paths, &theme_name)?;
    let root = theme_root(repo_root, paths, &theme_name)?;
    let spec = ThemeSpec::load_from_file(&file)?;
    let resolved = resolve_theme(&spec, args.variant.as_deref())?;
    let debug = debug_theme_file(&file, &root, paths, args.variant.as_deref())?;

    if args.json {
        let value = json!({
            "theme": resolved.name,
            "theme_file": debug.theme_file,
            "theme_root": debug.theme_root,
            "mode": format!("{:?}", resolved.mode),
            "primary": resolved.primary.to_hex(),
            "nvim": {
                "preset": format!("{:?}", resolved.nvim_preset),
                "transparent": resolved.nvim_transparent
            },
            "vwm_bar": {
                "style": resolved.vwm_bar.style,
                "background": resolved.vwm_bar.background,
                "modules": resolved.vwm_bar.modules,
                "icons": resolved.vwm_bar.icons,
                "colors": resolved.vwm_bar.colors,
                "minimal": resolved.vwm_bar.minimal,
                "height": resolved.vwm_bar.height,
                "radius": resolved.vwm_bar.radius,
                "margin_x": resolved.vwm_bar.margin_x,
                "margin_y": resolved.vwm_bar.margin_y,
                "padding_y": resolved.vwm_bar.padding_y
            },
            "selected_wallpaper": debug.selected_wallpaper,
            "writes": debug.generated_paths.iter().map(|(name, path)| {
                json!({
                    "target": name,
                    "path": path
                })
            }).collect::<Vec<_>>()
        });

        println!("{}", serde_json::to_string_pretty(&value)?);
        return Ok(());
    }

    println!("theme: {}", resolved.name);
    println!("theme_file: {}", debug.theme_file.display());
    println!("theme_root: {}", debug.theme_root.display());
    println!("mode: {:?}", resolved.mode);
    println!("primary: {}", resolved.primary.to_hex());
    println!("nvim_preset: {:?}", resolved.nvim_preset);
    println!("nvim_transparent: {}", resolved.nvim_transparent);
    println!("vwm_bar.style: {}", resolved.vwm_bar.style);
    println!("vwm_bar.background: {}", resolved.vwm_bar.background);
    println!("vwm_bar.modules: {}", resolved.vwm_bar.modules);
    println!("vwm_bar.icons: {}", resolved.vwm_bar.icons);
    println!("vwm_bar.colors: {}", resolved.vwm_bar.colors);
    println!("vwm_bar.minimal: {}", resolved.vwm_bar.minimal);
    println!("vwm_bar.height: {}", resolved.vwm_bar.height);
    println!("vwm_bar.radius: {}", resolved.vwm_bar.radius);
    println!("vwm_bar.margin_x: {}", resolved.vwm_bar.margin_x);
    println!("vwm_bar.margin_y: {}", resolved.vwm_bar.margin_y);
    println!("vwm_bar.padding_y: {}", resolved.vwm_bar.padding_y);
    println!(
        "selected_wallpaper: {}",
        debug
            .selected_wallpaper
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "none".to_string())
    );
    println!("writes:");
    for (name, path) in debug.generated_paths {
        println!("  {:<8} {}", name, path.display());
    }

    Ok(())
}

fn validate_command(repo_root: &Path, paths: &KaraPaths, theme: Option<String>) -> Result<()> {
    if let Some(theme) = theme {
        let spec = ThemeSpec::load_from_file(&theme_file(repo_root, paths, &theme)?)?;
        validate_spec(&spec)?;
        println!("valid: {}", spec.meta.name);
        return Ok(());
    }

    let mut failed = false;
    let mut seen: std::collections::BTreeSet<String> = Default::default();

    for base in paths.theme_search_paths(Some(repo_root)) {
        if !base.is_dir() {
            continue;
        }
        for entry in fs::read_dir(&base)? {
            let entry = entry?;
            let path = entry.path();
            let file = path.join("theme.toml");
            if !file.is_file() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().to_string();
            // Skip shadowed names — only validate the highest-priority copy.
            if !seen.insert(name.clone()) {
                continue;
            }

            match ThemeSpec::load_from_file(&file) {
                Ok(spec) => println!("valid: {}", spec.meta.name),
                Err(err) => {
                    failed = true;
                    eprintln!("invalid: {} -> {}", file.display(), err);
                }
            }
        }
    }

    if failed {
        anyhow::bail!("one or more themes failed validation");
    }

    Ok(())
}

fn resolve_command(
    repo_root: &Path,
    paths: &KaraPaths,
    theme: &str,
    variant: Option<&str>,
    as_json: bool,
) -> Result<()> {
    let spec = ThemeSpec::load_from_file(&theme_file(repo_root, paths, theme)?)?;
    let resolved = resolve_theme(&spec, variant)?;

    if as_json {
        let value = json!({
            "name": resolved.name,
            "mode": format!("{:?}", resolved.mode),
            "wallpaper": resolved.wallpaper,
            "primary": resolved.primary.to_hex(),
            "semantic": {
                "bg0": resolved.semantic.bg0.to_hex(),
                "bg1": resolved.semantic.bg1.to_hex(),
                "bg2": resolved.semantic.bg2.to_hex(),
                "fg0": resolved.semantic.fg0.to_hex(),
                "fg1": resolved.semantic.fg1.to_hex(),
                "fg_muted": resolved.semantic.fg_muted.to_hex(),
                "accent": resolved.semantic.accent.to_hex(),
                "accent_soft": resolved.semantic.accent_soft.to_hex(),
                "accent_contrast": resolved.semantic.accent_contrast.to_hex(),
                "border_subtle": resolved.semantic.border_subtle.to_hex(),
                "border_strong": resolved.semantic.border_strong.to_hex(),
                "selection_bg": resolved.semantic.selection_bg.to_hex(),
                "selection_fg": resolved.semantic.selection_fg.to_hex(),
                "success": resolved.semantic.success.to_hex(),
                "warning": resolved.semantic.warning.to_hex(),
                "danger": resolved.semantic.danger.to_hex(),
                "info": resolved.semantic.info.to_hex()
            },
            "ansi": resolved.ansi.iter().map(|c| c.to_hex()).collect::<Vec<_>>(),
            "base16": resolved.base16.iter().map(|c| c.to_hex()).collect::<Vec<_>>(),
            "style": {
                "radius_px": resolved.style.radius_px,
                "opacity": resolved.style.opacity,
                "blur": resolved.style.blur,
                "density": format!("{:?}", resolved.style.density),
                "surface_style": format!("{:?}", resolved.style.surface_style)
            },
            "nvim": {
                "preset": format!("{:?}", resolved.nvim_preset),
                "transparent": resolved.nvim_transparent
            },
            "vwm_bar": {
                "style": resolved.vwm_bar.style,
                "background": resolved.vwm_bar.background,
                "modules": resolved.vwm_bar.modules,
                "icons": resolved.vwm_bar.icons,
                "colors": resolved.vwm_bar.colors,
                "minimal": resolved.vwm_bar.minimal,
                "height": resolved.vwm_bar.height,
                "radius": resolved.vwm_bar.radius,
                "margin_x": resolved.vwm_bar.margin_x,
                "margin_y": resolved.vwm_bar.margin_y,
                "padding_y": resolved.vwm_bar.padding_y
            },
            "fonts": {
                "ui_family": resolved.fonts.ui_family,
                "ui_size": resolved.fonts.ui_size,
                "mono_family": resolved.fonts.mono_family,
                "mono_size": resolved.fonts.mono_size
            }
        });

        println!("{}", serde_json::to_string_pretty(&value)?);
        return Ok(());
    }

    println!("name: {}", resolved.name);
    println!("mode: {:?}", resolved.mode);
    println!("primary: {}", resolved.primary.to_hex());
    println!("bg0: {}", resolved.semantic.bg0.to_hex());
    println!("bg1: {}", resolved.semantic.bg1.to_hex());
    println!("bg2: {}", resolved.semantic.bg2.to_hex());
    println!("fg0: {}", resolved.semantic.fg0.to_hex());
    println!("accent: {}", resolved.semantic.accent.to_hex());
    println!("accent_soft: {}", resolved.semantic.accent_soft.to_hex());
    println!("selection_bg: {}", resolved.semantic.selection_bg.to_hex());

    Ok(())
}

fn apply_command(
    repo_root: &Path,
    paths: &KaraPaths,
    theme: &str,
    variant: Option<&str>,
    no_reload: bool,
    dry_run: bool,
) -> Result<()> {
    let file = theme_file(repo_root, paths, theme)?;
    let root = theme_root(repo_root, paths, theme)?;
    apply_theme_file(
        &file,
        &root,
        paths,
        variant,
        ApplyOptions {
            reload: !no_reload,
            dry_run,
        },
    )
}

fn preview_command(
    repo_root: &Path,
    paths: &KaraPaths,
    theme: &str,
    variant: Option<&str>,
    no_reload: bool,
) -> Result<()> {
    let current_theme = read_current_theme(paths)?;
    let current_wallpaper = read_current_wallpaper(paths)?;

    begin_preview_state(paths, current_theme.as_deref(), current_wallpaper.as_ref())?;

    let file = theme_file(repo_root, paths, theme)?;
    let root = theme_root(repo_root, paths, theme)?;
    apply_theme_file(
        &file,
        &root,
        paths,
        variant,
        ApplyOptions {
            reload: !no_reload,
            dry_run: false,
        },
    )
}

fn revert_command(repo_root: &Path, paths: &KaraPaths, no_reload: bool) -> Result<()> {
    let Some(theme) = read_preview_theme(paths)? else {
        anyhow::bail!("no preview state saved");
    };

    let file = theme_file(repo_root, paths, &theme)?;
    let root = theme_root(repo_root, paths, &theme)?;

    apply_theme_file(
        &file,
        &root,
        paths,
        None,
        ApplyOptions {
            reload: !no_reload,
            dry_run: false,
        },
    )?;
    clear_preview_state(paths)?;
    Ok(())
}

fn list_variants_command(repo_root: &Path, paths: &KaraPaths, theme: &str) -> Result<()> {
    let spec = ThemeSpec::load_from_file(&theme_file(repo_root, paths, theme)?)?;

    if spec.variants.is_empty() {
        println!("(single-palette theme — no variants)");
        return Ok(());
    }

    let default = spec.meta.default_variant.clone();
    for (name, variant) in &spec.variants {
        let is_default = default.as_deref() == Some(name.as_str());
        let mark = if is_default { "*" } else { " " };
        let label = variant.display_name.as_deref().unwrap_or(name);
        let preset = variant.preset.as_deref().unwrap_or("(inline)");
        println!("{mark} {name:<16} {label:<20} preset={preset}");
    }
    Ok(())
}

fn wallpaper_command(
    repo_root: &Path,
    paths: &KaraPaths,
    theme: &str,
    command: WallpaperCommands,
) -> Result<()> {
    let dir = theme_wallpapers_dir(repo_root, paths, theme)?;
    let files = list_wallpaper_files(&dir)?;

    match command {
        WallpaperCommands::List { json } => {
            let current = read_theme_wallpaper(paths, theme)?;
            if json {
                let value = json!({
                    "theme": theme,
                    "current": current,
                    "files": files
                });
                println!("{}", serde_json::to_string_pretty(&value)?);
                return Ok(());
            }

            for file in files {
                let mark = if current.as_ref() == Some(&file) {
                    "*"
                } else {
                    " "
                };
                println!(
                    "{} {}",
                    mark,
                    file.file_name().unwrap_or_default().to_string_lossy()
                );
            }
            Ok(())
        }
        WallpaperCommands::Set { file } => {
            let selected = dir.join(&file);
            if !selected.is_file() {
                anyhow::bail!("wallpaper not found: {}", selected.display());
            }

            write_theme_wallpaper(paths, theme, &selected)?;
            println!("set wallpaper for {}: {}", theme, selected.display());
            Ok(())
        }
        WallpaperCommands::Current { json } => {
            let current = read_theme_wallpaper(paths, theme)?;
            if json {
                let value = json!({
                    "theme": theme,
                    "current": current
                });
                println!("{}", serde_json::to_string_pretty(&value)?);
                return Ok(());
            }

            if let Some(path) = current {
                println!("{}", path.display());
            } else {
                println!("none");
            }
            Ok(())
        }
        WallpaperCommands::Random => {
            let selected = choose_random_wallpaper(&files)?;
            write_theme_wallpaper(paths, theme, &selected)?;
            println!("set wallpaper for {}: {}", theme, selected.display());
            Ok(())
        }
        WallpaperCommands::Next => {
            let current = read_theme_wallpaper(paths, theme)?;
            let selected = cycle_wallpaper(&files, current.as_ref(), true)?;
            write_theme_wallpaper(paths, theme, &selected)?;
            println!("set wallpaper for {}: {}", theme, selected.display());
            Ok(())
        }
        WallpaperCommands::Prev => {
            let current = read_theme_wallpaper(paths, theme)?;
            let selected = cycle_wallpaper(&files, current.as_ref(), false)?;
            write_theme_wallpaper(paths, theme, &selected)?;
            println!("set wallpaper for {}: {}", theme, selected.display());
            Ok(())
        }
    }
}

fn list_wallpaper_files(dir: &Path) -> Result<Vec<PathBuf>> {
    if !dir.is_dir() {
        anyhow::bail!("wallpaper directory not found: {}", dir.display());
    }

    let mut files = vec![];
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if !path.is_file() {
            continue;
        }

        let name = path.file_name().unwrap_or_default().to_string_lossy();
        if name.starts_with('.') {
            continue;
        }

        let ext = path
            .extension()
            .map(|e| e.to_string_lossy().to_ascii_lowercase())
            .unwrap_or_default();

        let ok = matches!(
            ext.as_str(),
            "png" | "jpg" | "jpeg" | "webp" | "bmp" | "gif" | "tif" | "tiff" | "avif"
        );

        if ok {
            files.push(path);
        }
    }

    files.sort();

    if files.is_empty() {
        anyhow::bail!("no wallpapers found in {}", dir.display());
    }

    Ok(files)
}

fn choose_random_wallpaper(files: &[PathBuf]) -> Result<PathBuf> {
    use std::time::{SystemTime, UNIX_EPOCH};

    if files.is_empty() {
        anyhow::bail!("no wallpapers available");
    }

    let seed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock before unix epoch")?
        .as_nanos() as usize;

    Ok(files[seed % files.len()].clone())
}

fn cycle_wallpaper(files: &[PathBuf], current: Option<&PathBuf>, forward: bool) -> Result<PathBuf> {
    if files.is_empty() {
        anyhow::bail!("no wallpapers available");
    }

    let idx = current
        .and_then(|c| files.iter().position(|f| f == c))
        .unwrap_or(0);

    let next_idx = if forward {
        (idx + 1) % files.len()
    } else if idx == 0 {
        files.len() - 1
    } else {
        idx - 1
    };

    Ok(files[next_idx].clone())
}

fn render_command(
    repo_root: &Path,
    paths: &KaraPaths,
    theme: &str,
    target: RenderTarget,
    variant: Option<&str>,
) -> Result<()> {
    let spec = ThemeSpec::load_from_file(&theme_file(repo_root, paths, theme)?)?;
    let resolved = resolve_theme(&spec, variant)?;

    let out = match target {
        RenderTarget::Gtk => render_gtk_settings(&resolved),
        RenderTarget::Kitty => render_kitty_theme(&resolved),
        RenderTarget::Foot => render_foot_theme(&resolved),
        RenderTarget::Nvim => render_nvim_theme(&resolved),
        RenderTarget::Tmux => render_tmux_theme(&resolved),
        RenderTarget::KaraGate => render_kara_gate_theme(&resolved),
        RenderTarget::Fzf => render_fzf_theme(&resolved),
        RenderTarget::Session => render_session_theme(&resolved),
    };

    print!("{out}");
    Ok(())
}

fn derive_image_command(
    _repo_root: &Path,
    paths: &KaraPaths,
    image: &Path,
    name: &str,
    mode: Option<ModeArg>,
    as_json: bool,
) -> Result<()> {
    let ranked = ranked_swatches_from_image(image)?;
    let (mut spec, original_image) = derive_theme_from_image(name, image)?;

    if let Some(mode) = mode {
        spec.meta.mode = match mode {
            ModeArg::Dark => UiMode::Dark,
            ModeArg::Light => UiMode::Light,
            ModeArg::Auto => UiMode::Auto,
        };
    }

    // Write derived themes to the user's config path so they live
    // alongside hand-authored themes. Previously this wrote into the
    // repo's themes/ dir which is awkward for users not running from source.
    let root = paths.config_home.join("kara").join("themes").join(name);
    let wallpapers = root.join("wallpapers");
    fs::create_dir_all(&wallpapers)?;

    let file_name = original_image
        .file_name()
        .context("image path had no file name")?
        .to_string_lossy()
        .to_string();

    let dest_image = wallpapers.join(&file_name);
    fs::copy(&original_image, &dest_image)?;

    spec.wallpaper.default = Some(file_name);
    spec.save_to_file(&root.join("theme.toml"))?;

    if as_json {
        let value = json!({
            "theme_name": name,
            "mode": format!("{:?}", spec.meta.mode),
            "primary": spec.palette.primary,
            "theme_root": root,
            "swatches": ranked.iter().map(|s| json!({
                "hex": s.color.to_hex(),
                "score": s.score
            })).collect::<Vec<_>>()
        });

        println!("{}", serde_json::to_string_pretty(&value)?);
        return Ok(());
    }

    println!("derived theme created: {}", root.display());
    println!("primary: {}", spec.palette.primary);
    println!("mode: {:?}", spec.meta.mode);
    println!("swatches:");
    for swatch in ranked {
        println!("  {} ({:.3})", swatch.color.to_hex(), swatch.score);
    }

    Ok(())
}
