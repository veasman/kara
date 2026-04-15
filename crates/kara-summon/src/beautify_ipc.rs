//! Minimal client for the kara-beautify daemon IPC protocol.
//!
//! These types mirror `crates/kara-beautify/src/ipc.rs` — they're
//! duplicated here (not shared through a library crate) so
//! kara-summon doesn't take a transitive dependency on kara-beautify.
//! The protocol is small and stable; drift risk is bounded by the
//! request/response shapes both sides serialize as JSON over the
//! same Unix socket.
//!
//! Keep in sync with kara-beautify/src/ipc.rs when adding new
//! request variants. Unknown variants serialized by one side are
//! silently ignored by the other via serde's default unknown-field
//! handling for `#[serde(tag = ...)]` enums.

use std::env;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

pub fn socket_path() -> PathBuf {
    let runtime_dir = env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".to_string());
    PathBuf::from(runtime_dir).join("kara-beautify.sock")
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum Request {
    GetState,
    ListThemes,
    ListVariants {
        theme: String,
    },
    ListWallpapers {
        theme: String,
        #[serde(default)]
        variant: Option<String>,
    },
    #[allow(dead_code)]
    GetHistory,
    SetTheme {
        name: String,
        variant: Option<String>,
        wallpaper: Option<PathBuf>,
    },
    #[allow(dead_code)]
    SetVariant {
        variant: String,
    },
    #[allow(dead_code)]
    CycleVariant {
        direction: Direction,
    },
    // Preview state machine — for B9c live-preview wiring.
    #[allow(dead_code)]
    ApplyPreview {
        theme: Option<String>,
        variant: Option<String>,
        wallpaper: Option<PathBuf>,
    },
    #[allow(dead_code)]
    CommitPreview,
    #[allow(dead_code)]
    CancelPreview,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[allow(dead_code)]
pub enum Direction {
    Next,
    Prev,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum Response {
    Ok,
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
    #[allow(dead_code)]
    History {
        entries: Vec<HistoryEntry>,
    },
}

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
    pub source: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VariantEntry {
    pub name: String,
    pub display_name: Option<String>,
    pub preset: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(dead_code)]
pub struct HistoryEntry {
    pub theme: String,
    pub variant: Option<String>,
    pub timestamp: String,
}

/// Best-effort request: connect, write, read, return None on any
/// socket-level failure (no daemon running, crash mid-request, etc.).
pub fn try_request(req: &Request) -> Option<Response> {
    use std::os::unix::net::UnixStream;

    let mut stream = UnixStream::connect(socket_path()).ok()?;
    kara_ipc::write_message(&mut stream, req).ok()?;
    kara_ipc::read_message::<Response, _>(&mut stream).ok()
}
