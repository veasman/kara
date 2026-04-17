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
    WallpaperEntry, socket_path,
};
use crate::state::paths::KaraPaths;
use crate::state::runtime::{
    read_current_theme, read_current_variant, write_current_theme, write_current_variant,
    clear_current_variant,
};

/// Daemon state held across requests.
pub struct DaemonState {
    pub repo_root: PathBuf,
    pub paths: KaraPaths,

    /// In-memory redo stack. Resets on daemon exit, which matches
    /// user intuition — "redo" is a short-term affordance after
    /// an accidental undo, not a long-term history.
    pub redo_stack: Vec<HistoryEntry>,

    /// Snapshot of the theme state at the moment the current
    /// preview session began. Populated on the first
    /// `ApplyPreview` after a clean state; cleared on
    /// `CommitPreview` or `CancelPreview`. When present, the
    /// current on-disk state is a live preview that CancelPreview
    /// can revert to by re-applying this snapshot.
    ///
    /// In-memory only — if the daemon crashes during a preview,
    /// the last-applied preview state stays on disk and there's
    /// no revert path. That matches the design trade-off
    /// documented in the B9 plan: previews are transient UX, not
    /// a durable transaction layer.
    pub preview_snapshot: Option<HistoryEntry>,
}

pub fn run(repo_root: PathBuf, paths: KaraPaths) -> Result<()> {
    let sock = socket_path();

    // If another daemon is already listening, bail out instead of
    // stealing its socket path — that leaves the previous process
    // alive with an orphaned listener FD, which is how we ended up
    // with dozens of zombie daemons accumulating across reloads.
    if sock.exists() {
        if UnixStream::connect(&sock).is_ok() {
            println!(
                "kara-beautify daemon already running at {}; exiting",
                sock.display()
            );
            return Ok(());
        }
        // Socket file exists but nobody's home — previous daemon
        // crashed. Safe to remove.
        let _ = fs::remove_file(&sock);
    }

    let listener = UnixListener::bind(&sock)
        .with_context(|| format!("binding daemon socket {}", sock.display()))?;

    println!("kara-beautify daemon listening on {}", sock.display());

    let state = Mutex::new(DaemonState {
        repo_root,
        paths,
        redo_stack: Vec::new(),
        preview_snapshot: None,
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
        Request::ListWallpapers { theme, variant } => {
            handle_list_wallpapers(&s, theme, variant.as_deref())
        }
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
    let search = s.paths.theme_search_paths_labeled(Some(&s.repo_root));
    let mut entries: Vec<ThemeEntry> = Vec::new();
    let mut seen: std::collections::BTreeSet<String> = Default::default();

    for (source, base) in search {
        if !base.is_dir() {
            continue;
        }
        for entry in fs::read_dir(&base)? {
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
                source: source.label().to_string(),
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

fn handle_list_wallpapers(
    s: &DaemonState,
    theme_name: &str,
    variant: Option<&str>,
) -> Result<Response> {
    // Resolve the theme and read its wallpapers directory. Per-variant
    // wallpaper overrides in the manifest aren't supported yet — every
    // variant of a theme gets the same pool. The variant parameter is
    // carried through the request so future schema additions won't
    // break the IPC shape.
    let theme_dir = s
        .paths
        .find_theme(theme_name, Some(&s.repo_root))
        .with_context(|| format!("theme '{theme_name}' not found"))?;

    let wallpapers_dir = theme_dir.join("wallpapers");
    let mut entries: Vec<WallpaperEntry> = Vec::new();

    if wallpapers_dir.is_dir() {
        let mut files: Vec<std::path::PathBuf> = Vec::new();
        for entry in fs::read_dir(&wallpapers_dir)? {
            let entry = entry?;
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let file_name = path
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or_default()
                .to_string();
            if file_name.starts_with('.') {
                continue;
            }
            let ext = path
                .extension()
                .and_then(|s| s.to_str())
                .map(|s| s.to_ascii_lowercase())
                .unwrap_or_default();
            let supported = matches!(
                ext.as_str(),
                "png" | "jpg" | "jpeg" | "webp" | "bmp" | "gif" | "tif" | "tiff" | "avif"
                    | "mp4" | "mkv" | "webm" | "mov" | "m4v"
            );
            if supported {
                files.push(path);
            }
        }
        files.sort();

        for path in files {
            let file_name = path
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or_default()
                .to_string();
            let is_animated = matches!(
                path.extension()
                    .and_then(|s| s.to_str())
                    .map(|s| s.to_ascii_lowercase())
                    .as_deref(),
                Some("gif") | Some("mp4") | Some("mkv") | Some("webm") | Some("mov") | Some("m4v")
            );
            entries.push(WallpaperEntry {
                path,
                file_name,
                is_animated,
            });
        }
    }

    Ok(Response::Wallpapers {
        theme: theme_name.to_string(),
        variant: variant.map(|s| s.to_string()),
        entries,
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

// ─── Preview state machine (B9c) ───────────────────────────────────
//
// Flow:
//   1. Picker opens → no snapshot yet
//   2. User navigates → ApplyPreview fires
//       - First preview captures current on-disk state as the
//         snapshot (the "revert target")
//       - Subsequent previews re-apply the new target WITHOUT
//         re-snapshotting — the original pre-picker state stays
//         as the revert point
//   3. User hits Enter → CommitPreview
//       - The live applied state is already what the user wants,
//         so this just records it in history and clears the
//         snapshot
//   4. User hits Escape → CancelPreview
//       - Re-apply the snapshot (original state), clear it
//
// No-op robustness:
//   - Commit with no snapshot: Ok (nothing to commit)
//   - Cancel with no snapshot: Ok (nothing to cancel)
//   - Apply with no snapshot: capture snapshot + apply
//   - Apply with existing snapshot: just apply (preserve snapshot)

fn handle_apply_preview(
    s: &mut DaemonState,
    theme: Option<&str>,
    variant: Option<&str>,
    wallpaper: Option<&std::path::Path>,
) -> Result<Response> {
    // Resolve the target — preview needs at least one of theme or
    // variant. Picker always passes both on navigation so we don't
    // normally hit the edge cases, but guard against an empty
    // request to avoid silently applying "the current theme" which
    // would look like a no-op to the user.
    let target_theme = theme
        .map(|s| s.to_string())
        .or_else(|| read_current_theme(&s.paths).ok().flatten())
        .context("ApplyPreview with no theme and no current theme in state")?;

    // If no snapshot exists yet, capture the pre-preview state so
    // CancelPreview can revert.
    if s.preview_snapshot.is_none() {
        let current_theme = read_current_theme(&s.paths)?;
        let current_variant = read_current_variant(&s.paths)?;
        if let Some(t) = current_theme {
            s.preview_snapshot = Some(HistoryEntry::new(&t, current_variant.as_deref()));
        }
    }

    // If the picker passed an explicit wallpaper, stage it into
    // the theme-wallpaper state file BEFORE the apply runs — the
    // apply path's `selected_wallpaper()` reads from there as its
    // highest-priority source, so this overrides whatever the
    // manifest says for the theme's default wallpaper.
    if let Some(w) = wallpaper {
        if w.is_file() {
            crate::state::runtime::write_theme_wallpaper(&s.paths, &target_theme, w)?;
        }
    }

    // Apply the target without recording in history — preview
    // navigation shouldn't pollute the undo/redo stack.
    apply_state(s, &target_theme, variant)?;

    Ok(Response::OkWithMessage {
        message: format!(
            "preview: {target_theme}{}",
            variant.map(|v| format!(":{v}")).unwrap_or_default()
        ),
    })
}

fn handle_commit_preview(s: &mut DaemonState) -> Result<Response> {
    if s.preview_snapshot.is_none() {
        // No active preview — nothing to commit. Return Ok so
        // picker-on-enter-with-no-navigation doesn't error.
        return Ok(Response::Ok);
    }

    // The on-disk state is already the preview (we wrote it in
    // ApplyPreview). Now record it in history so undo/redo works,
    // and clear the snapshot.
    let current_theme = read_current_theme(&s.paths)?
        .context("CommitPreview with no current_theme on disk")?;
    let current_variant = read_current_variant(&s.paths)?;

    let mut history = History::load(&history_path(&s.paths))?;
    history.push(HistoryEntry::new(&current_theme, current_variant.as_deref()));
    history.save(&history_path(&s.paths))?;

    s.redo_stack.clear();
    s.preview_snapshot = None;

    Ok(Response::OkWithMessage {
        message: format!(
            "committed: {current_theme}{}",
            current_variant
                .as_deref()
                .map(|v| format!(":{v}"))
                .unwrap_or_default()
        ),
    })
}

fn handle_cancel_preview(s: &mut DaemonState) -> Result<Response> {
    let Some(snapshot) = s.preview_snapshot.take() else {
        // No active preview — nothing to cancel. Ok so
        // picker-on-escape-with-no-navigation doesn't error.
        return Ok(Response::Ok);
    };

    // Re-apply the snapshot without recording.
    apply_state(s, &snapshot.theme, snapshot.variant.as_deref())?;

    Ok(Response::OkWithMessage {
        message: format!(
            "reverted: {}{}",
            snapshot.theme,
            snapshot
                .variant
                .as_deref()
                .map(|v| format!(":{v}"))
                .unwrap_or_default()
        ),
    })
}

/// Shared apply helper used by both the preview path and undo/redo.
/// Writes generated state files + fires reloads + updates the
/// current_theme/current_variant state files, but does NOT touch
/// history or the redo stack. Callers that need history tracking
/// use `apply_and_record` instead.
fn apply_state(s: &DaemonState, theme_name: &str, variant: Option<&str>) -> Result<()> {
    let theme_dir = s
        .paths
        .find_theme(theme_name, Some(&s.repo_root))
        .with_context(|| format!("theme '{theme_name}' not found"))?;
    let file = theme_dir.join("theme.toml");

    // Validate the variant before writing anything.
    let spec = ThemeSpec::load_from_file(&file)?;
    let _ = resolve_theme(&spec, variant)?;

    apply_theme_file(
        &file,
        &theme_dir,
        &s.paths,
        variant,
        ApplyOptions::default(),
    )?;
    Ok(())
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
