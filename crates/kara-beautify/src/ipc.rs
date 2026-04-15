//! kara-beautify IPC protocol.
//!
//! Beautify runs an optional long-lived daemon that accepts requests
//! over `$XDG_RUNTIME_DIR/kara-beautify.sock`. The same framing
//! convention as kara-ipc (4-byte LE length + JSON payload) is reused
//! via kara_ipc::{read_message, write_message}, but the message types
//! live in this crate since they're beautify-specific and shouldn't
//! bloat the kara-gate compositor protocol.
//!
//! Clients can route through the daemon (fast hot-path, no cold start)
//! OR fall back to the direct CLI path when the daemon isn't running.
//! The CLI wrapper commands (`kara-beautify toggle`, `undo`, etc.) try
//! the socket first and silently fall back to direct writes if the
//! socket isn't there. This keeps the daemon additive rather than
//! load-bearing — it crashes, the CLI still works.

use std::env;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

pub fn socket_path() -> PathBuf {
    let runtime_dir = env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".to_string());
    PathBuf::from(runtime_dir).join("kara-beautify.sock")
}

/// Request sent from a client (CLI wrapper, keybind, picker) to the
/// beautify daemon.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum Request {
    // ─── State queries ─────────────────────────────────────────────
    GetState,
    ListThemes,
    ListVariants {
        theme: String,
    },
    /// Enumerate wallpapers available for a specific (theme, variant)
    /// pair. Scans the theme's `wallpapers/` directory and returns
    /// each supported image file. The `variant` is passed so a
    /// future per-variant wallpaper override in the manifest can
    /// be honored — today all variants of a theme share the same
    /// wallpaper pool.
    ListWallpapers {
        theme: String,
        #[serde(default)]
        variant: Option<String>,
    },
    GetHistory,

    // ─── Commits ───────────────────────────────────────────────────
    /// Apply a theme (+ optional variant + optional wallpaper) and
    /// persist it as the current state.
    SetTheme {
        name: String,
        variant: Option<String>,
        wallpaper: Option<PathBuf>,
    },
    /// Swap to a specific variant within the currently-active theme.
    SetVariant {
        variant: String,
    },
    /// Cycle through the current theme's variants — this is what
    /// `mod+Shift+t` fires.
    CycleVariant {
        direction: Direction,
    },

    // ─── History ───────────────────────────────────────────────────
    Undo,
    Redo,

    // ─── Preview state machine (picker will drive this in B9) ─────
    ApplyPreview {
        theme: Option<String>,
        variant: Option<String>,
        wallpaper: Option<PathBuf>,
    },
    CommitPreview,
    CancelPreview,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Direction {
    Next,
    Prev,
}

/// Response returned for each request.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum Response {
    /// Success with no payload. Most commits return this.
    Ok,
    /// Success with a human-friendly note to surface (e.g. the new
    /// variant name) — daemon uses this for things the caller might
    /// want to pop up in a popover / log line.
    OkWithMessage {
        message: String,
    },
    Error {
        message: String,
    },

    State {
        theme: Option<String>,
        variant: Option<String>,
        preview_active: bool,
    },
    Themes {
        themes: Vec<ThemeEntry>,
    },
    Variants {
        theme: String,
        default_variant: Option<String>,
        variants: Vec<VariantEntry>,
    },
    Wallpapers {
        theme: String,
        variant: Option<String>,
        entries: Vec<WallpaperEntry>,
    },
    History {
        entries: Vec<HistoryEntry>,
    },
}

/// A single wallpaper entry for the carousel. `path` is absolute so
/// clients can pass it straight into `ApplyPreview { wallpaper: ... }`.
/// `is_animated` is reserved for D2 (GIF) / D3 (video) support —
/// today it's always false since only static images are supported,
/// but the field is in the IPC schema now so clients can plumb it
/// through the picker's carousel without a protocol bump later.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WallpaperEntry {
    pub path: PathBuf,
    pub file_name: String,
    #[serde(default)]
    pub is_animated: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThemeEntry {
    pub name: String,
    pub display_name: Option<String>,
    pub author: Option<String>,
    pub default_variant: Option<String>,
    pub variant_count: usize,
    pub source: String, // "user" | "data" | "repo" | "system"
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VariantEntry {
    pub name: String,
    pub display_name: Option<String>,
    pub preset: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryEntry {
    pub theme: String,
    pub variant: Option<String>,
    pub timestamp: String, // RFC3339
}

/// Helper: attempt to connect to the daemon and send a request. Returns
/// None if the daemon isn't running (socket missing or connection
/// refused). Callers use this to decide whether to fall back to the
/// direct CLI path.
pub fn try_request(req: &Request) -> Option<Response> {
    use std::os::unix::net::UnixStream;

    let mut stream = UnixStream::connect(socket_path()).ok()?;
    kara_ipc::write_message(&mut stream, req).ok()?;
    kara_ipc::read_message::<Response, _>(&mut stream).ok()
}
