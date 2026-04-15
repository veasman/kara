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

/// A single bar module with its positional inline arguments.
///
/// Everything after `<section> <module-name>` on a module declaration
/// line is collected, in order, into `args`. Different modules consume
/// different args — see each module's docs in the example config.
///
/// Examples:
///   `right clock "%H:%M"`        → args=["%H:%M"]
///   `right volume med`           → args=["med"]
///   `right custom "uptime -p"`   → args=["uptime -p"]
///   `left workspaces`            → args=[]
#[derive(Debug, Clone)]
pub struct BarModule {
    pub section: BarSection,
    pub kind: BarModuleKind,
    pub args: Vec<String>,
}

/// Bar configuration. The bar and its modules are two separate concerns with
/// parallel appearance knobs:
///
/// Spatial model, from the outside in:
///   bar  (full width × `height`, its own rounded/bordered/transparent/blurred surface)
///     → `edge_padding_x/y` inset
///       → row of modules separated by `module_gap`
///         → each module has `module_padding_x/y` inside and, when `pill`
///           is true, its own rounded/bordered/transparent/blurred background
///
/// The pill toggle is global — all modules render pill-style or none do.
/// Per-module color and anything else small lives inline on the module
/// declaration in the `modules { }` block.
#[derive(Debug, Clone)]
pub struct Bar {
    pub enabled: bool,
    pub position: BarPosition,
    pub height: i32,

    // ── Bar-level appearance ──────────────────────────────────────────
    /// Draw the bar's own background fill (rounded/bordered/transparent/
    /// blurred according to the fields below). `false` means no bar
    /// background at all — modules float over whatever is behind.
    pub background: bool,
    /// Explicit background color. `None` → fall back to the active theme's
    /// surface color.
    pub background_color: Option<u32>,
    /// 0-255 opacity for the bar background. 255 = fully opaque.
    pub background_alpha: u8,
    /// Corner radius of the bar surface in pixels. 0 = square corners.
    pub rounded: i32,
    /// Border thickness around the bar surface in pixels.
    pub border_px: i32,
    /// Border color. `None` → theme border.
    pub border_color: Option<u32>,
    /// Blur the content behind the bar surface. Parsed and stored but not
    /// yet rendered — requires GL shader support (deferred).
    pub blur: bool,

    // ── Module-level appearance (shared by all modules) ──────────────
    /// If true, every module renders with its own background fill
    /// (rounded/bordered/transparent/blurred according to module_*).
    /// If false, modules are text-only and module_* appearance fields
    /// are ignored.
    pub pill: bool,
    pub module_background: Option<u32>,
    pub module_alpha: u8,
    pub module_rounded: i32,
    pub module_border_px: i32,
    pub module_border_color: Option<u32>,
    pub module_blur: bool,

    pub icons: bool,
    pub colors: bool,
    pub minimal: bool,

    /// Inset from bar left/right edge to first/last module.
    pub edge_padding_x: i32,
    /// Vertical inset of the module content area from bar top/bottom edges.
    pub edge_padding_y: i32,
    /// Space between adjacent modules.
    pub module_gap: i32,
    /// Horizontal padding inside a pill module background. Pill mode only.
    pub module_padding_x: i32,
    /// Vertical padding inside a pill module background. Pill mode only.
    pub module_padding_y: i32,

    pub modules: Vec<BarModule>,
}

impl Default for Bar {
    fn default() -> Self {
        Self {
            enabled: true,
            position: BarPosition::Top,
            height: 24,

            background: true,
            background_color: None,
            background_alpha: 255,
            rounded: 0,
            border_px: 0,
            border_color: None,
            blur: false,

            pill: false,
            module_background: None,
            module_alpha: 255,
            module_rounded: 8,
            module_border_px: 0,
            module_border_color: None,
            module_blur: false,

            icons: true,
            colors: true,
            minimal: false,
            edge_padding_x: 14,
            edge_padding_y: 2,
            module_gap: 18,
            module_padding_x: 12,
            module_padding_y: 6,
            modules: Vec::new(),
        }
    }
}

// ── Scratchpad ──────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct ScratchpadConfig {
    pub name: String,
    /// Inset from the workarea edge on all four sides in pixels. The
    /// scratchpad area is `workarea` shrunk by `gap_px` on every side.
    /// Replaces the old percentage-based `width_pct` / `height_pct`
    /// knobs so scratchpad size is consistent across differently-sized
    /// monitors without math.
    pub gap_px: i32,
    pub dim_alpha: i32,
    pub blur: bool,
    pub overlay: Option<String>,
    /// Commands to spawn the first time the scratchpad is toggled
    /// visible (and again after it goes empty via auto-hide). Each
    /// entry spawns a separate process and each resulting window is
    /// captured into the scratchpad in declaration order.
    pub autostart: Vec<String>,
    pub captures: Vec<String>, // app_id patterns
}

impl ScratchpadConfig {
    pub fn new(name: &str) -> Self {
        Self {
            name: name.to_string(),
            gap_px: 30,
            dim_alpha: 48,
            blur: false,
            overlay: None,
            autostart: Vec::new(),
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

/// Condition that gates when an autostart entry runs. All `required_monitors`
/// must be connected and none of `forbidden_monitors` may be connected.
/// An empty condition (both lists empty) always matches — used for the base
/// set of autostart entries written directly inside `autostart { }` with no
/// enclosing `when` block.
#[derive(Debug, Clone, Default)]
pub struct AutostartCondition {
    pub required_monitors: Vec<String>,
    pub forbidden_monitors: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct AutostartEntry {
    pub command: String,
    pub app_id: Option<String>,
    pub workspace: Option<usize>,
    pub monitor: Option<usize>,
    pub condition: AutostartCondition,
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
    /// Marks this monitor as the primary output. The primary monitor receives
    /// initial focus when kara starts and is the default destination for new
    /// windows when no other output has the pointer. At most one monitor
    /// should set this; if multiple do, the first one wins.
    pub primary: bool,
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
