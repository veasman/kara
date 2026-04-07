/// Configuration types for the kara desktop environment.
///
/// All blocks from the config format are represented here.
/// Blocks that aren't wired yet (animations, bar, scratchpad) are parsed
/// and stored for future milestones.

use std::collections::HashMap;

// ── Top-level Config ────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct Config {
    pub general: General,
    pub theme: Theme,
    pub animations: Animations,
    pub bar: Bar,
    pub scratchpads: Vec<ScratchpadConfig>,
    pub rules: Vec<Rule>,
    pub autostart: Vec<AutostartEntry>,
    pub commands: HashMap<String, String>,
    pub keybinds: Vec<Keybind>,
    pub environment: Vec<EnvDirective>,
    pub input: Vec<InputDevice>,
    pub monitors: Vec<MonitorConfig>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            general: General::default(),
            theme: Theme::default(),
            animations: Animations::default(),
            bar: Bar::default(),
            scratchpads: Vec::new(),
            rules: Vec::new(),
            autostart: Vec::new(),
            commands: HashMap::new(),
            keybinds: Vec::new(),
            environment: Vec::new(),
            input: Vec::new(),
            monitors: Vec::new(),
        }
    }
}

// ── General ─────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct General {
    pub font: String,
    pub font_size: f32,
    pub border_px: i32,
    pub border_radius: i32,
    pub gap_px: i32,
    pub default_mfact: f32,
    pub sync_workspaces: bool,
    pub cursor_theme: Option<String>,
    pub cursor_size: i32,
}

impl Default for General {
    fn default() -> Self {
        Self {
            font: "monospace".into(),
            font_size: 14.0,
            border_px: 2,
            border_radius: 4,
            gap_px: 8,
            default_mfact: 0.5,
            sync_workspaces: true,
            cursor_theme: None,
            cursor_size: 24,
        }
    }
}

// ── Theme ───────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy)]
pub struct Theme {
    pub bg: u32,
    pub surface: u32,
    pub text: u32,
    pub text_muted: u32,
    pub accent: u32,
    pub accent_soft: u32,
    pub border: u32,
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            bg: 0x111111,
            surface: 0x1b1b1b,
            text: 0xf2f2f2,
            text_muted: 0x8c8c8c,
            accent: 0x6bacac,
            accent_soft: 0x458588,
            border: 0x353535,
        }
    }
}

// ── Animations ──────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnimationPreset {
    Instant,
    Clean,
    Swoosh,
}

#[derive(Debug, Clone, Copy)]
pub struct Animations {
    pub preset: AnimationPreset,
    pub duration_ms: u32,
}

impl Default for Animations {
    fn default() -> Self {
        Self {
            preset: AnimationPreset::Instant,
            duration_ms: 150,
        }
    }
}

// ── Bar ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BarPosition {
    Top,
    Bottom,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BarModuleStyle {
    Flat,
    Pill,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BarModuleKind {
    Workspaces,
    Monitor,
    Sync,
    Title,
    Status,
    Clock,
    Custom,
    Volume,
    Network,
    Battery,
    Brightness,
    Media,
    Memory,
    Cpu,
    Weather,
    Script(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BarSection {
    Left,
    Center,
    Right,
}

#[derive(Debug, Clone)]
pub struct BarModule {
    pub section: BarSection,
    pub kind: BarModuleKind,
    pub arg: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Bar {
    pub enabled: bool,
    pub background: bool,
    pub position: BarPosition,
    pub height: i32,
    pub radius: i32,
    pub module_style: BarModuleStyle,
    pub icons: bool,
    pub colors: bool,
    pub minimal: bool,
    /// Reserved: outer horizontal inset from screen edge (not yet wired)
    pub margin_x: i32,
    /// Reserved: outer vertical inset from screen edge (not yet wired)
    pub margin_y: i32,
    /// Horizontal padding from bar edge to first/last module
    pub content_margin_x: i32,
    /// Vertical inset of pill/content area from bar top/bottom edges
    pub content_margin_y: i32,
    /// Space between adjacent modules
    pub gap: i32,
    /// Horizontal padding inside pill backgrounds (pill mode only)
    pub padding_x: i32,
    /// Vertical padding inside pill backgrounds (pill mode only)
    pub padding_y: i32,
    pub volume_bar_enabled: bool,
    pub volume_bar_width: i32,
    pub volume_bar_height: i32,
    pub volume_bar_radius: i32,
    pub modules: Vec<BarModule>,
}

impl Default for Bar {
    fn default() -> Self {
        Self {
            enabled: true,
            background: true,
            position: BarPosition::Top,
            height: 24,
            radius: 18,
            module_style: BarModuleStyle::Flat,
            icons: true,
            colors: true,
            minimal: false,
            margin_x: 18,
            margin_y: 10,
            content_margin_x: 14,
            content_margin_y: 2,
            gap: 18,
            padding_x: 12,
            padding_y: 6,
            volume_bar_enabled: true,
            volume_bar_width: 46,
            volume_bar_height: 6,
            volume_bar_radius: 10,
            modules: Vec::new(),
        }
    }
}

// ── Scratchpad ──────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct ScratchpadConfig {
    pub name: String,
    pub width_pct: i32,
    pub height_pct: i32,
    pub dim_alpha: i32,
    pub blur: bool,
    pub overlay: Option<String>,
    pub autostart: Option<String>,
    pub captures: Vec<String>, // app_id patterns
}

impl ScratchpadConfig {
    pub fn new(name: &str) -> Self {
        Self {
            name: name.to_string(),
            width_pct: 92,
            height_pct: 92,
            dim_alpha: 48,
            blur: false,
            overlay: None,
            autostart: None,
            captures: Vec::new(),
        }
    }
}

// ── Rules ───────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub enum Rule {
    Float {
        app_id: String,
    },
    Workspace {
        workspace: usize,
        app_id: String,
        monitor: Option<usize>,
    },
}

// ── Autostart ───────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct AutostartEntry {
    pub command: String,
    pub app_id: Option<String>,
    pub workspace: Option<usize>,
    pub monitor: Option<usize>,
}

// ── Keybinds ────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModMask {
    pub logo: bool,
    pub shift: bool,
    pub ctrl: bool,
    pub alt: bool,
}

impl ModMask {
    pub const NONE: Self = Self { logo: false, shift: false, ctrl: false, alt: false };
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BindAction {
    /// Spawn a named command from the commands block
    Spawn(String),
    /// Execute a raw shell command directly (no command lookup)
    Exec(String),
    /// Toggle scratchpad (optionally a named one)
    Scratchpad(Option<String>),
    /// Built-in WM action
    FocusNext,
    FocusPrev,
    FocusMonitorPrev,
    FocusMonitorNext,
    SendMonitorPrev,
    SendMonitorNext,
    DecreaseMfact,
    IncreaseMfact,
    ZoomMaster,
    Monocle,
    Fullscreen,
    ToggleSync,
    ToggleFloat,
    KillClient,
    Reload,
    Quit,
    ShowKeybinds,
    ViewWs(usize),
    SendWs(usize),
}

#[derive(Debug, Clone)]
pub struct Keybind {
    pub mods: ModMask,
    pub keysym: u32,
    pub action: BindAction,
}

// ── Environment ─────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub enum EnvDirective {
    /// Set an environment variable: env KEY "value"
    Set { key: String, value: String },
    /// Source a shell file: source "path"
    Source { path: String },
}

// ── Monitor config ─────────────────────────────────────────────────

/// Rotation for a monitor output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MonitorRotation {
    Normal,
    Left,    // 90° counter-clockwise
    Right,   // 90° clockwise
    Flipped, // 180°
}

#[derive(Debug, Clone)]
pub struct MonitorConfig {
    pub name: String,
    pub resolution: Option<(i32, i32)>,
    pub refresh: Option<u32>,
    pub position: Option<(i32, i32)>,
    pub scale: Option<f64>,
    pub rotation: MonitorRotation,
    pub enabled: bool,
}

// ── Input devices ───────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccelProfile {
    Flat,
    Adaptive,
}

#[derive(Debug, Clone)]
pub struct InputDevice {
    /// Device name pattern to match (None = default/global)
    pub device: Option<String>,
    pub accel_profile: Option<AccelProfile>,
    pub accel_speed: Option<f64>,
    pub natural_scroll: Option<bool>,
    pub tap_to_click: Option<bool>,
    pub tap_and_drag: Option<bool>,
    pub dwt: Option<bool>,
    pub scroll_method: Option<String>,
    pub click_method: Option<String>,
    pub left_handed: Option<bool>,
    pub middle_emulation: Option<bool>,
}
