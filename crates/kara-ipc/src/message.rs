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
pub struct OutputInfo {
    pub name: String,
    pub width: i32,
    pub height: i32,
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
}
