//! kara-beautify daemon — Unix socket server for fast-path theme
//! operations.
//!
//! Listens on `$XDG_RUNTIME_DIR/kara-beautify.sock`, accepts requests
//! defined in `crate::ipc`, and delegates to the same apply / state
//! helpers the CLI path uses. This is an additive layer — the CLI
//! works fine without a daemon running, the daemon just accelerates
//! the hot path (keybind cycle, picker navigation) by avoiding the
//! cold-start cost of walking the theme tree + loading config +
//! binding clap on every keypress.
//!
//! Architecture: blocking one-connection-at-a-time model. Each
//! request is quick (filesystem write + gsettings call + SIGUSR1),
//! never blocks for long. A future upgrade path would swap this for
//! calloop, but the blocking model is ~50 lines vs ~300 for the
//! same functionality and the latency difference is irrelevant for
//! a theme picker that fires maybe once a minute.

use std::fs;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::Mutex;

use anyhow::{Context, Result};
use kara_theme::{ThemeSpec, resolve_theme};

use crate::apply::{ApplyOptions, apply_theme_file};
use crate::config::BeautifyConfig;
use crate::history::{History, HistoryEntry};
use crate::ipc::{
    Direction, HistoryEntry as IpcHistoryEntry, Request, Response, ThemeEntry, VariantEntry,
    socket_path,
};
use crate::state::paths::KaraPaths;
use crate::state::runtime::{
    read_current_theme, read_current_variant, write_current_theme, write_current_variant,
    clear_current_variant,
};

/// Daemon state held across requests. The in-memory redo stack lives
/// here (resets on daemon exit, which matches user intuition —
/// "redo" is a short-term affordance after an accidental undo).
pub struct DaemonState {
    pub repo_root: PathBuf,
    pub paths: KaraPaths,
    pub redo_stack: Vec<HistoryEntry>,
}

pub fn run(repo_root: PathBuf, paths: KaraPaths) -> Result<()> {
    let sock = socket_path();

    // Remove any stale socket from a crashed previous daemon.
    let _ = fs::remove_file(&sock);

    let listener = UnixListener::bind(&sock)
        .with_context(|| format!("binding daemon socket {}", sock.display()))?;

    println!("kara-beautify daemon listening on {}", sock.display());

    let state = Mutex::new(DaemonState {
        repo_root,
        paths,
        redo_stack: Vec::new(),
    });

    // Simple accept loop. Each client is handled to completion
    // before the next is accepted — fine for a low-traffic theme
    // daemon. If we ever hit contention, swap for a thread-per-conn
    // or calloop model.
    for incoming in listener.incoming() {
        match incoming {
            Ok(mut stream) => {
                if let Err(e) = handle_one(&mut stream, &state) {
                    eprintln!("daemon request failed: {e:#}");
                }
            }
            Err(e) => {
                eprintln!("daemon accept failed: {e}");
            }
        }
    }

    let _ = fs::remove_file(&sock);
    Ok(())
}

fn handle_one(stream: &mut UnixStream, state: &Mutex<DaemonState>) -> Result<()> {
    let req: Request = kara_ipc::read_message(stream)?;
    let resp = match dispatch(&req, state) {
        Ok(r) => r,
        Err(e) => Response::Error {
            message: format!("{e:#}"),
        },
    };
    kara_ipc::write_message(stream, &resp)?;
    Ok(())
}

fn dispatch(req: &Request, state: &Mutex<DaemonState>) -> Result<Response> {
    let mut s = state.lock().unwrap();
    match req {
        Request::GetState => handle_get_state(&s),
        Request::ListThemes => handle_list_themes(&s),
        Request::ListVariants { theme } => handle_list_variants(&s, theme),
        Request::GetHistory => handle_get_history(&s),

        Request::SetTheme {
            name,
            variant,
            wallpaper,
        } => handle_set_theme(&mut s, name, variant.as_deref(), wallpaper.as_deref()),
        Request::SetVariant { variant } => handle_set_variant(&mut s, variant),
        Request::CycleVariant { direction } => handle_cycle_variant(&mut s, *direction),

        Request::Undo => handle_undo(&mut s),
        Request::Redo => handle_redo(&mut s),

        Request::ApplyPreview {
            theme,
            variant,
            wallpaper,
        } => handle_apply_preview(
            &mut s,
            theme.as_deref(),
            variant.as_deref(),
            wallpaper.as_deref(),
        ),
        Request::CommitPreview => handle_commit_preview(&mut s),
        Request::CancelPreview => handle_cancel_preview(&mut s),
    }
}

// ─── Request handlers ──────────────────────────────────────────────

fn handle_get_state(s: &DaemonState) -> Result<Response> {
    Ok(Response::State {
        theme: read_current_theme(&s.paths)?,
        variant: read_current_variant(&s.paths)?,
        preview_active: false, // TODO when preview is wired
    })
}

fn handle_list_themes(s: &DaemonState) -> Result<Response> {
    let search = s.paths.theme_search_paths(Some(&s.repo_root));
    let mut entries: Vec<ThemeEntry> = Vec::new();
    let mut seen: std::collections::BTreeSet<String> = Default::default();

    for (idx, base) in search.iter().enumerate() {
        if !base.is_dir() {
            continue;
        }
        for entry in fs::read_dir(base)? {
            let entry = entry?;
            let path = entry.path();
            let manifest = path.join("theme.toml");
            if !manifest.is_file() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().to_string();
            if !seen.insert(name.clone()) {
                continue; // higher-priority path already registered this theme
            }

            let spec = match ThemeSpec::load_from_file(&manifest) {
                Ok(s) => s,
                Err(_) => continue,
            };
            entries.push(ThemeEntry {
                name: spec.meta.name.clone(),
                display_name: spec.meta.display_name.clone(),
                author: spec.meta.author.clone(),
                default_variant: spec.meta.default_variant.clone(),
                variant_count: spec.variants.len(),
                source: match idx {
                    0 => "user".to_string(),
                    1 => "data".to_string(),
                    2 => "repo".to_string(),
                    _ => "system".to_string(),
                },
            });
        }
    }

    Ok(Response::Themes { themes: entries })
}

fn handle_list_variants(s: &DaemonState, theme_name: &str) -> Result<Response> {
    let theme_dir = s
        .paths
        .find_theme(theme_name, Some(&s.repo_root))
        .with_context(|| format!("theme '{theme_name}' not found"))?;
    let spec = ThemeSpec::load_from_file(&theme_dir.join("theme.toml"))?;

    let variants = spec
        .variants
        .iter()
        .map(|(name, v)| VariantEntry {
            name: name.clone(),
            display_name: v.display_name.clone(),
            preset: v.preset.clone(),
        })
        .collect();

    Ok(Response::Variants {
        theme: spec.meta.name,
        default_variant: spec.meta.default_variant,
        variants,
    })
}

fn handle_get_history(s: &DaemonState) -> Result<Response> {
    let history = History::load(&history_path(&s.paths))?;
    let entries = history
        .entries
        .into_iter()
        .map(|e| IpcHistoryEntry {
            theme: e.theme,
            variant: e.variant,
            timestamp: e.timestamp,
        })
        .collect();
    Ok(Response::History { entries })
}

fn handle_set_theme(
    s: &mut DaemonState,
    name: &str,
    variant: Option<&str>,
    _wallpaper: Option<&std::path::Path>,
) -> Result<Response> {
    apply_and_record(s, name, variant)?;
    let msg = match variant {
        Some(v) => format!("{name}: {v}"),
        None => name.to_string(),
    };
    Ok(Response::OkWithMessage { message: msg })
}

fn handle_set_variant(s: &mut DaemonState, variant: &str) -> Result<Response> {
    let current = read_current_theme(&s.paths)?
        .context("no current theme to swap variant on — run `kara-beautify apply <theme>` first")?;
    apply_and_record(s, &current, Some(variant))?;
    Ok(Response::OkWithMessage {
        message: format!("{current}: {variant}"),
    })
}

fn handle_cycle_variant(s: &mut DaemonState, dir: Direction) -> Result<Response> {
    let current_theme = read_current_theme(&s.paths)?
        .context("no current theme — run `kara-beautify apply <theme>` first")?;
    let current_variant = read_current_variant(&s.paths)?;

    let theme_dir = s
        .paths
        .find_theme(&current_theme, Some(&s.repo_root))
        .with_context(|| format!("current theme '{current_theme}' not found"))?;
    let spec = ThemeSpec::load_from_file(&theme_dir.join("theme.toml"))?;

    if spec.variants.is_empty() {
        return Ok(Response::Error {
            message: format!("theme '{current_theme}' has no variants to cycle"),
        });
    }

    // Build an ordered list of variant names (BTreeMap keys → sorted
    // alphabetically, which matches `kara-beautify list-variants`
    // output). Then advance one step.
    let variants: Vec<&String> = spec.variants.keys().collect();
    let current_idx = current_variant
        .as_deref()
        .and_then(|v| variants.iter().position(|k| k.as_str() == v))
        .unwrap_or(0);
    let next_idx = match dir {
        Direction::Next => (current_idx + 1) % variants.len(),
        Direction::Prev => (current_idx + variants.len() - 1) % variants.len(),
    };
    let next = variants[next_idx].clone();

    apply_and_record(s, &current_theme, Some(&next))?;

    // Fire a popover notification via kara-whisper's socket if
    // available. Silent failure if whisper isn't listening.
    crate::popover::try_show(&format!("{current_theme}: {next}"), 1500);

    Ok(Response::OkWithMessage {
        message: format!("{current_theme}: {next}"),
    })
}

fn handle_undo(s: &mut DaemonState) -> Result<Response> {
    let mut history = History::load(&history_path(&s.paths))?;

    // Pop the current state (the head) and move it onto the redo stack.
    let popped = history
        .pop_front()
        .context("nothing to undo — history is empty")?;
    s.redo_stack.push(popped);

    // The new head is the state we're undoing TO.
    let target = history
        .head()
        .cloned()
        .context("nothing left after undo")?;
    history.save(&history_path(&s.paths))?;

    // Apply the target without recording (we already own the history).
    let theme_dir = s
        .paths
        .find_theme(&target.theme, Some(&s.repo_root))
        .with_context(|| format!("undo target theme '{}' not found", target.theme))?;
    apply_theme_file(
        &theme_dir.join("theme.toml"),
        &theme_dir,
        &s.paths,
        target.variant.as_deref(),
        ApplyOptions::default(),
    )?;

    // apply_theme_file writes current_theme/variant; don't double-record.
    update_current_state_files(&s.paths, &target)?;

    Ok(Response::OkWithMessage {
        message: format!(
            "undo → {}{}",
            target.theme,
            target
                .variant
                .as_deref()
                .map(|v| format!(":{v}"))
                .unwrap_or_default()
        ),
    })
}

fn handle_redo(s: &mut DaemonState) -> Result<Response> {
    let target = s
        .redo_stack
        .pop()
        .context("nothing to redo — redo stack is empty")?;

    let theme_dir = s
        .paths
        .find_theme(&target.theme, Some(&s.repo_root))
        .with_context(|| format!("redo target theme '{}' not found", target.theme))?;
    apply_theme_file(
        &theme_dir.join("theme.toml"),
        &theme_dir,
        &s.paths,
        target.variant.as_deref(),
        ApplyOptions::default(),
    )?;

    // Push it back onto the history head so undo can pop it again.
    let mut history = History::load(&history_path(&s.paths))?;
    history.push(target.clone());
    history.save(&history_path(&s.paths))?;

    Ok(Response::OkWithMessage {
        message: format!(
            "redo → {}{}",
            target.theme,
            target
                .variant
                .as_deref()
                .map(|v| format!(":{v}"))
                .unwrap_or_default()
        ),
    })
}

// Preview handlers — stubbed until the picker (B9) wires them.
fn handle_apply_preview(
    _s: &mut DaemonState,
    _theme: Option<&str>,
    _variant: Option<&str>,
    _wallpaper: Option<&std::path::Path>,
) -> Result<Response> {
    Ok(Response::Error {
        message: "preview not yet implemented — lands with the picker (B9)".to_string(),
    })
}

fn handle_commit_preview(_s: &mut DaemonState) -> Result<Response> {
    Ok(Response::Error {
        message: "preview not yet implemented".to_string(),
    })
}

fn handle_cancel_preview(_s: &mut DaemonState) -> Result<Response> {
    Ok(Response::Error {
        message: "preview not yet implemented".to_string(),
    })
}

// ─── Helpers ───────────────────────────────────────────────────────

fn apply_and_record(s: &mut DaemonState, theme_name: &str, variant: Option<&str>) -> Result<()> {
    let theme_dir = s
        .paths
        .find_theme(theme_name, Some(&s.repo_root))
        .with_context(|| format!("theme '{theme_name}' not found"))?;
    let file = theme_dir.join("theme.toml");

    // Validate + resolve the variant before writing anything so a
    // bad request doesn't leave state files half-updated.
    let spec = ThemeSpec::load_from_file(&file)?;
    let _ = resolve_theme(&spec, variant)?;

    apply_theme_file(
        &file,
        &theme_dir,
        &s.paths,
        variant,
        ApplyOptions::default(),
    )?;

    // Record on history ring buffer (apply_theme_file already wrote
    // current_theme/current_variant state files, so we only touch
    // history here).
    let mut history = History::load(&history_path(&s.paths))?;
    history.push(HistoryEntry::new(theme_name, variant));
    history.save(&history_path(&s.paths))?;

    // A fresh commit invalidates the redo stack.
    s.redo_stack.clear();

    let _ = BeautifyConfig::load(&s.paths); // touch to validate
    Ok(())
}

/// Write current_theme / current_variant state files directly. Used
/// by undo/redo which bypass the normal apply() → write path because
/// they need tighter control over history ordering.
fn update_current_state_files(paths: &KaraPaths, entry: &HistoryEntry) -> Result<()> {
    write_current_theme(paths, &entry.theme)?;
    match entry.variant.as_deref() {
        Some(v) => write_current_variant(paths, v)?,
        None => clear_current_variant(paths)?,
    }
    Ok(())
}

fn history_path(paths: &KaraPaths) -> PathBuf {
    paths.kara_state_dir().join("history.json")
}
