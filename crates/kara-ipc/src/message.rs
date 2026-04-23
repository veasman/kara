//! IPC message types for communication between kara-gate and tools.

use serde::{Deserialize, Serialize};

/// Request sent from a tool to the compositor.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum Request {
    // Queries
    GetWorkspaces,
    GetActiveWindow,
    GetOutputs,
    GetTheme,

    // Actions
    ViewWorkspace { index: usize },
    SendToWorkspace { index: usize },
    FocusNext,
    FocusPrev,
    KillClient,
    Reload,
    Spawn { command: String },

    // Appearance (kara-beautify → kara-gate)
    ThemeChanged { theme_name: String },
    WallpaperChanged { path: String },

    // Screenshot
    Screenshot,
    ScreenshotRegion { x: i32, y: i32, w: i32, h: i32 },
    /// Capture a specific output by connector name (e.g. "DP-2", "DVI-I-1").
    /// Lets kara-veil capture one pixmap per monitor for its per-output
    /// blurred lock backdrops without fighting over compositor focus.
    ScreenshotOutput { name: String },

    // Queries
    GetWindowGeometries,

    // Event subscription
    Subscribe,
    Unsubscribe,
}

/// Response sent from the compositor back to a tool.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum Response {
    Ok,
    Error { message: String },
    Workspaces {
        current: usize,
        occupied: Vec<bool>,
    },
    ActiveWindow {
        title: String,
        app_id: String,
    },
    Outputs {
        outputs: Vec<OutputInfo>,
    },
    Theme {
        colors: ThemeColors,
    },
    ScreenshotDone {
        path: String,
    },
    WindowGeometries {
        windows: Vec<WindowGeometry>,
    },
}

/// Compositor event pushed to subscribed tools.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum Event {
    WorkspaceChanged { index: usize },
    WindowOpened { app_id: String },
    WindowClosed { app_id: String },
    FocusChanged { title: String, app_id: String },
    ThemeReloaded,
    OutputChanged,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WindowGeometry {
    pub app_id: String,
    pub title: String,
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutputInfo {
    pub name: String,
    pub width: i32,
    pub height: i32,
    /// Global position of this output's top-left corner in the
    /// compositor's logical coordinate space. Needed by multi-monitor
    /// tools (glimpse all-monitor screenshots, kara-veil per-output
    /// lock surfaces) so they can reason about where each output sits.
    /// `#[serde(default)]` keeps older tools (built before this field)
    /// parse-compatible — they'll just see (0, 0).
    #[serde(default)]
    pub x: i32,
    #[serde(default)]
    pub y: i32,
    /// True for the output the user marked `primary` in their
    /// `monitors { }` config. Consumers that want to render
    /// "primary-only" chrome (kara-veil's login card, a future
    /// widgets layer) look for this flag rather than assuming the
    /// first entry or the leftmost monitor — neither is reliable
    /// when the user has a dedicated "work" monitor in the middle
    /// or on the right. Defaults to false so pre-field clients
    /// parse cleanly.
    #[serde(default)]
    pub primary: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThemeColors {
    pub bg: u32,
    pub surface: u32,
    pub text: u32,
    pub text_muted: u32,
    pub accent: u32,
    pub accent_soft: u32,
    pub border: u32,

    // ── Bar geometry ────────────────────────────────────────────────
    /// Bar height in pixels. Used by kara-summon to position the
    /// theme picker directly below the bar.
    #[serde(default)]
    pub bar_height: Option<u16>,
    /// Whether the bar draws its own background surface.
    #[serde(default)]
    pub bar_background: Option<bool>,
    /// Bar background alpha (0-255).
    #[serde(default)]
    pub bar_background_alpha: Option<u8>,

    // ── Window decoration (optional) ───────────────────────────────
    /// Theme-driven border width in pixels. `None` → consumers use
    /// their own default (typically 2px).
    #[serde(default)]
    pub border_px: Option<u16>,
    /// Corner radius for theme-driven borders. `None` → consumers
    /// use their own default (typically 0).
    #[serde(default)]
    pub border_radius: Option<u16>,
    /// Absolute path to a pre-rasterized PNG tile used as the border
    /// pattern fill. Written by kara-beautify when the active theme
    /// declares `window_border.svg_tile`. Consumers that honor this
    /// (kara-gate border, kara-glimpse selection, kara-whisper
    /// notification chrome) tile the PNG instead of using a solid
    /// color. `None` → solid-color borders.
    #[serde(default)]
    pub border_tile_path: Option<String>,

    // ── Typography ─────────────────────────────────────────────────
    /// Font family name from `general.font` in kara-gate.conf (or the
    /// active theme's override). When `None`, consumers fall back to
    /// their built-in default. Everything that speaks to the user —
    /// kara-whisper notifications, kara-veil lock prompt, kara-summon
    /// picker — should pick this up so the session's typography
    /// matches across tools.
    #[serde(default)]
    pub font_family: Option<String>,
}
