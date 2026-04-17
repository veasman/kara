use std::fs;
use std::path::Path;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::validate::validate_spec;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum UiMode {
    #[default]
    Dark,
    Light,
    Auto,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum AccentStrategy {
    Vivid,
    #[default]
    Balanced,
    Muted,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ContrastLevel {
    Low,
    #[default]
    Medium,
    High,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Radius {
    None,
    Small,
    #[default]
    Medium,
    Large,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Density {
    Compact,
    #[default]
    Normal,
    Roomy,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum SurfaceStyle {
    Flat,
    #[default]
    Soft,
    Elevated,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum NvimPreset {
    #[default]
    Semantic,
    Gruvbox,
    Vague,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum VwmBarStyle {
    #[default]
    Docked,
    Floating,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum VwmBarModules {
    Flat,
    #[default]
    Pill,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThemeMeta {
    pub name: String,
    #[serde(default)]
    pub mode: UiMode,
    /// Name of the variant used when `kara-beautify apply <theme>` is
    /// called without an explicit `--variant` flag. Must match a key in
    /// the top-level `[variants]` table if that table is non-empty.
    #[serde(default)]
    pub default_variant: Option<String>,
    /// Human-readable label for picker UIs.
    #[serde(default)]
    pub display_name: Option<String>,
    /// Author string for picker UIs / `kara-beautify list`.
    #[serde(default)]
    pub author: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct WallpaperSpec {
    #[serde(default)]
    pub default: Option<String>,
    /// Per-monitor wallpaper overrides keyed by output connector name
    /// (e.g. `"DP-2"`, `"HDMI-A-1"`). Reserved slot — no renderer reads
    /// this yet; beautify currently applies `default` to all outputs.
    #[serde(default)]
    pub per_monitor: Option<std::collections::BTreeMap<String, String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PaletteSpec {
    pub primary: String,
    #[serde(default)]
    pub accent_strategy: AccentStrategy,
    #[serde(default)]
    pub contrast: ContrastLevel,
}

impl Default for PaletteSpec {
    fn default() -> Self {
        Self {
            primary: "#7aa2f7".to_string(),
            accent_strategy: AccentStrategy::Balanced,
            contrast: ContrastLevel::Medium,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StyleSpec {
    #[serde(default)]
    pub radius: Radius,
    #[serde(default)]
    pub density: Density,
    #[serde(default)]
    pub surface_style: SurfaceStyle,
    #[serde(default = "default_transparency")]
    pub transparency: f32,
    #[serde(default = "default_true")]
    pub blur: bool,
}

fn default_transparency() -> f32 {
    0.94
}

fn default_true() -> bool {
    true
}

impl Default for StyleSpec {
    fn default() -> Self {
        Self {
            radius: Radius::Medium,
            density: Density::Normal,
            surface_style: SurfaceStyle::Soft,
            transparency: 0.94,
            blur: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FontSpec {
    #[serde(default = "default_ui_family")]
    pub ui_family: String,
    #[serde(default = "default_ui_size")]
    pub ui_size: u16,
    #[serde(default = "default_mono_family")]
    pub mono_family: String,
    #[serde(default = "default_mono_size")]
    pub mono_size: u16,
}

fn default_ui_family() -> String {
    "FiraCode Nerd Font".to_string()
}

fn default_mono_family() -> String {
    "FiraCode Nerd Font".to_string()
}

fn default_ui_size() -> u16 {
    11
}

fn default_mono_size() -> u16 {
    13
}

impl Default for FontSpec {
    fn default() -> Self {
        Self {
            ui_family: default_ui_family(),
            ui_size: default_ui_size(),
            mono_family: default_mono_family(),
            mono_size: default_mono_size(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NvimSpec {
    #[serde(default)]
    pub preset: NvimPreset,
    #[serde(default = "default_true")]
    pub transparent: bool,
}

impl Default for NvimSpec {
    fn default() -> Self {
        Self {
            preset: NvimPreset::Semantic,
            transparent: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VwmBarSpec {
    #[serde(default)]
    pub style: VwmBarStyle,
    #[serde(default = "default_true")]
    pub background: bool,
    #[serde(default)]
    pub modules: VwmBarModules,
    #[serde(default = "default_true")]
    pub icons: bool,
    #[serde(default = "default_true")]
    pub colors: bool,
    #[serde(default = "default_false")]
    pub minimal: bool,
    #[serde(default = "default_bar_height")]
    pub height: u16,
    #[serde(default = "default_bar_radius")]
    pub radius: u16,
    #[serde(default = "default_bar_margin_x")]
    pub margin_x: u16,
    #[serde(default = "default_bar_margin_y")]
    pub margin_y: u16,
    #[serde(default = "default_bar_padding_y")]
    pub padding_y: u16,
}

fn default_false() -> bool {
    false
}

fn default_bar_height() -> u16 {
    28
}

fn default_bar_radius() -> u16 {
    12
}

fn default_bar_margin_x() -> u16 {
    12
}

fn default_bar_margin_y() -> u16 {
    10
}

fn default_bar_padding_y() -> u16 {
    4
}

impl Default for VwmBarSpec {
    fn default() -> Self {
        Self {
            style: VwmBarStyle::Docked,
            background: true,
            modules: VwmBarModules::Pill,
            icons: true,
            colors: true,
            minimal: false,
            height: default_bar_height(),
            radius: default_bar_radius(),
            margin_x: default_bar_margin_x(),
            margin_y: default_bar_margin_y(),
            padding_y: default_bar_padding_y(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CursorSpec {
    #[serde(default = "default_cursor_theme")]
    pub theme: String,
    #[serde(default = "default_cursor_size")]
    pub size: u16,
}

fn default_cursor_theme() -> String {
    "Adwaita".to_string()
}

fn default_cursor_size() -> u16 {
    24
}

impl Default for CursorSpec {
    fn default() -> Self {
        Self {
            theme: default_cursor_theme(),
            size: default_cursor_size(),
        }
    }
}

/// Decorative SVG artwork anchored at a point on a surface — used for
/// corner glyphs, notification accent art, launcher logos, etc. Not
/// tiled or stretched across an edge. For repeating edge patterns use
/// `SvgTileSpec` instead. Reserved slot: no renderer yet.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SvgOverlaySpec {
    pub path: String,
    /// How the SVG is placed relative to the surface it decorates.
    /// Interpretation is renderer-specific. Common values: `"corner"`,
    /// `"edge"`, `"full"`.
    #[serde(default)]
    pub anchor: Option<String>,
    /// Draw mode hint for renderers that support multiple layering
    /// modes (e.g. `"background"`, `"accent"`, `"frame"`).
    #[serde(default)]
    pub mode: Option<String>,
    #[serde(default)]
    pub opacity: Option<f32>,
    /// Optional palette-key reference for renderer tinting
    /// (e.g. `"$accent"`). Resolved lazily by each consumer.
    #[serde(default)]
    pub tint_from_palette: Option<String>,
}

/// Tileable SVG graphic used in place of a solid-color border or
/// background. The renderer rasterizes the SVG once per tile size and
/// repeats (or 9-slices) it around the target surface. Used by window
/// borders and bar backgrounds / outlines / module surfaces.
///
/// **Reserved slot — no renderer consumes this yet.** When the SVG
/// rasterizer lands (planned: add `resvg` crate, per-size pixmap cache,
/// TextureBuffer element wrapping each tile), this spec becomes the
/// wire format. Until then, theme TOMLs can carry it and solid-color
/// fallbacks apply.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SvgTileSpec {
    /// Path to the .svg file. Relative paths are resolved against the
    /// theme's directory (e.g. `themes/fantasy/borders/tile.svg`).
    pub path: String,
    /// Tile width in logical pixels. Controls how often the pattern
    /// repeats along the horizontal edge.
    #[serde(default)]
    pub tile_width: Option<u16>,
    /// Tile height in logical pixels.
    #[serde(default)]
    pub tile_height: Option<u16>,
    /// How to fill the target surface: `"repeat"` (default — tiles
    /// repeat across the edge), `"stretch"` (single copy stretched to
    /// fit), `"nine_slice"` (9-slice border with the `slice_px` inset
    /// below treated as corner/edge regions).
    #[serde(default)]
    pub edge_mode: Option<String>,
    /// Inset in pixels for 9-slice rendering — the distance from each
    /// edge of the SVG that forms the corner region. Ignored unless
    /// `edge_mode = "nine_slice"`.
    #[serde(default)]
    pub slice_px: Option<u16>,
    #[serde(default)]
    pub opacity: Option<f32>,
    /// Optional palette-key reference for renderer tinting
    /// (e.g. `"$accent"`). Resolved lazily by each consumer.
    #[serde(default)]
    pub tint_from_palette: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct WindowBorderSpec {
    #[serde(default)]
    pub width: Option<u16>,
    #[serde(default)]
    pub radius: Option<u16>,
    #[serde(default)]
    pub color_focused: Option<String>,
    #[serde(default)]
    pub color_unfocused: Option<String>,
    #[serde(default)]
    pub color_urgent: Option<String>,
    /// Decorative overlay artwork (corner glyphs, etc). Reserved slot,
    /// no renderer yet.
    #[serde(default)]
    pub svg_overlay: Option<SvgOverlaySpec>,
    /// Repeating SVG tile that replaces the solid-color border edges.
    /// When set, the compositor draws the SVG around the window in
    /// place of the `color_focused` / `color_unfocused` fill. Reserved
    /// slot — needs the SVG rasterizer renderer; until then the
    /// compositor falls back to the solid colors above.
    #[serde(default)]
    pub svg_tile: Option<SvgTileSpec>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct BarStyleSpec {
    /// Whether the bar draws its own background surface. When false,
    /// modules float over whatever is behind the bar. Maps to
    /// `bar { background = ... }` in the generated kara-gate include.
    #[serde(default)]
    pub background: Option<bool>,
    /// Solid background color override. String form accepts palette
    /// refs like `"$bg"` or literal hex `"#1a1a1a"`.
    #[serde(default)]
    pub background_color: Option<String>,
    /// 0-255 bar background alpha.
    #[serde(default)]
    pub background_alpha: Option<u8>,
    /// Bar height in pixels.
    #[serde(default)]
    pub height: Option<u16>,
    /// When true, every module draws its own background pill with
    /// `module_*` appearance. Maps to `bar { pill = true }`.
    #[serde(default)]
    pub pill: Option<bool>,
    /// Inset from bar left/right edge to the first/last module.
    #[serde(default)]
    pub edge_padding_x: Option<u16>,
    /// Vertical inset of modules from bar top/bottom.
    #[serde(default)]
    pub edge_padding_y: Option<u16>,
    /// Horizontal gap between adjacent modules.
    #[serde(default)]
    pub module_gap: Option<u16>,
    /// Horizontal padding inside each module pill.
    #[serde(default)]
    pub module_padding_x: Option<u16>,
    /// Vertical padding inside each module pill.
    #[serde(default)]
    pub module_padding_y: Option<u16>,
    /// Corner radius of each module pill.
    #[serde(default)]
    pub module_rounded: Option<u16>,
    /// Blur the desktop content behind the bar surface. kara-gate
    /// crops and box-blurs the wallpaper at the bar rect, compositing
    /// the bar's semi-transparent fill on top for a frosted glass
    /// look. Only effective when `background = true` and
    /// `background_alpha < 255`.
    #[serde(default)]
    pub blur: Option<bool>,
    /// When `true`, kara-sight skips rendering any center-section
    /// modules even if the user's config declares them. Lets themes
    /// like fantasy/moonlight suppress the center title without the
    /// user having to comment modules out of their base config.
    /// Default theme leaves it unset so the user's modules render
    /// as configured.
    #[serde(default)]
    pub hide_center: Option<bool>,
    #[serde(default)]
    pub font_family: Option<String>,
    #[serde(default)]
    pub font_size: Option<u16>,
    /// Explicit render size for nerd-font icon glyphs inside bar
    /// module text. Useful when the chosen font designs icon glyphs
    /// smaller than its text glyphs (e.g. 3270 Nerd Font Mono) —
    /// set `icon_size` larger than `font_size` to match the weight.
    #[serde(default)]
    pub icon_size: Option<u16>,
    #[serde(default)]
    pub module_fg: Option<String>,
    #[serde(default)]
    pub module_bg: Option<String>,
    /// Outline color drawn around the bar itself (when non-zero
    /// outline width is configured).
    #[serde(default)]
    pub outline_color: Option<String>,
    /// Outline color drawn around each module pill.
    #[serde(default)]
    pub module_outline_color: Option<String>,
    /// Decorative overlay artwork positioned on the bar. Reserved.
    #[serde(default)]
    pub svg_overlay: Option<SvgOverlaySpec>,
    /// Tileable SVG that replaces the bar's solid background fill.
    /// Reserved slot — needs kara-sight module cleanup.
    #[serde(default)]
    pub background_svg: Option<SvgTileSpec>,
    /// Tileable SVG drawn as the bar's outer frame/outline.
    /// Reserved slot.
    #[serde(default)]
    pub outline_svg: Option<SvgTileSpec>,
    /// Tileable SVG used as the background of each module pill.
    /// Reserved slot.
    #[serde(default)]
    pub module_background_svg: Option<SvgTileSpec>,
    /// Tileable SVG drawn as the outline of each module pill.
    /// Reserved slot.
    #[serde(default)]
    pub module_outline_svg: Option<SvgTileSpec>,
    /// Theme-driven module layout override. Each string is one raw
    /// module declaration line (e.g. `"left monitor group:nav"`,
    /// `"center workspaces badges"`). When present, the generated
    /// kara-gate include emits a `bar { modules { ... } }` block that
    /// fully replaces the user's base module layout at parse time —
    /// themes without `modules` leave the user's layout untouched.
    #[serde(default)]
    pub modules: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SoundsSpec {
    /// Freedesktop sound theme name (looked up in
    /// `/usr/share/sounds/<theme_name>/`). Reserved slot — whisper and
    /// kara-gate event hooks will read this when sound support lands.
    #[serde(default)]
    pub theme_name: Option<String>,
    /// Per-event sound overrides. Values are relative filenames inside
    /// the sound theme directory (e.g. `"message.oga"`).
    #[serde(default)]
    pub events: Option<std::collections::BTreeMap<String, String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct NotificationStyleSpec {
    #[serde(default)]
    pub background: Option<String>,
    #[serde(default)]
    pub border: Option<String>,
    #[serde(default)]
    pub fg: Option<String>,
    #[serde(default)]
    pub font_family: Option<String>,
    #[serde(default)]
    pub font_size: Option<u16>,
    #[serde(default)]
    pub radius: Option<u16>,
    #[serde(default)]
    pub svg_overlay: Option<SvgOverlaySpec>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct LauncherStyleSpec {
    #[serde(default)]
    pub background: Option<String>,
    #[serde(default)]
    pub accent: Option<String>,
    #[serde(default)]
    pub fg: Option<String>,
    #[serde(default)]
    pub selected_bg: Option<String>,
    #[serde(default)]
    pub font_family: Option<String>,
    #[serde(default)]
    pub font_size: Option<u16>,
    #[serde(default)]
    pub radius: Option<u16>,
    #[serde(default)]
    pub svg_overlay: Option<SvgOverlaySpec>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct LockScreenStyleSpec {
    #[serde(default)]
    pub background: Option<String>,
    #[serde(default)]
    pub accent: Option<String>,
    #[serde(default)]
    pub fg: Option<String>,
    #[serde(default)]
    pub svg_overlay: Option<SvgOverlaySpec>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AnimationsSpec {
    #[serde(default)]
    pub duration_ms_fast: Option<u32>,
    #[serde(default)]
    pub duration_ms_normal: Option<u32>,
    #[serde(default)]
    pub duration_ms_slow: Option<u32>,
    /// Named easing curve (e.g. `"ease-out-cubic"`, `"linear"`).
    #[serde(default)]
    pub easing: Option<String>,
}

/// One named palette swap within a theme. A variant either references a
/// built-in preset by name (fast path, hand-tuned colors) OR specifies
/// its palette inline via `primary` + the standard PaletteSpec knobs —
/// the inline path is how user-authored themes express themselves
/// without needing to add Rust code to kara-theme.
///
/// Themes can also grow extended-theming blocks per-variant (borders,
/// bar graphics, cursor theme overrides, etc.). The parser accepts
/// unknown top-level keys inside a variant via `#[serde(flatten)]` on
/// an `extra` map so user themes can start authoring richer content
/// now even though v1 renderers don't consume it yet.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct VariantSpec {
    /// Optional label for UI pickers. Falls back to the variant key.
    #[serde(default)]
    pub display_name: Option<String>,
    /// Reference a built-in preset by name (e.g. "gruvbox", "vague",
    /// "nord"). When set, palette/style below are used as overrides
    /// on top of the preset. When unset, the variant uses its inline
    /// `palette` section (or falls back to defaults).
    #[serde(default)]
    pub preset: Option<String>,
    /// Inline palette spec — used when `preset` is None, or to override
    /// a preset's primary color.
    #[serde(default)]
    pub palette: Option<PaletteSpec>,
    /// Per-variant wallpaper override.
    #[serde(default)]
    pub wallpaper: Option<String>,
    /// Extended theming extension blocks — `borders`, `bar_graphics`,
    /// `cursor`, `icons`, `glimpse`, `sounds`, etc. Captured as opaque
    /// `toml::Value` so v1 accepts them without consuming them. When
    /// v2 renderers land they'll deserialize from this map.
    #[serde(flatten)]
    pub extensions: std::collections::BTreeMap<String, toml::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThemeSpec {
    pub meta: ThemeMeta,
    #[serde(default)]
    pub wallpaper: WallpaperSpec,
    #[serde(default)]
    pub palette: PaletteSpec,
    #[serde(default)]
    pub style: StyleSpec,
    #[serde(default)]
    pub fonts: FontSpec,
    #[serde(default)]
    pub cursor: CursorSpec,
    #[serde(default)]
    pub nvim: NvimSpec,
    #[serde(default)]
    pub vwm_bar: VwmBarSpec,
    /// GTK theme name override. When omitted, resolver falls back to
    /// `Adwaita` / `Adwaita-dark` based on `meta.mode`. Renderer:
    /// `gtk-theme-name` key in `gtk-settings.ini`.
    #[serde(default)]
    pub gtk_theme: Option<String>,
    /// App icon theme. Rendered as `gtk-icon-theme-name`. Falls back to
    /// `Adwaita` when unset.
    #[serde(default)]
    pub icon_theme: Option<String>,
    /// File icon theme for file managers. Falls back to `icon_theme`
    /// when unset — most users pick one theme that covers both.
    /// Reserved for renderers that can differentiate (e.g. nautilus /
    /// thunar file icon lookups).
    #[serde(default)]
    pub file_icon_theme: Option<String>,
    /// Window border styling (width, radius, colors, SVG overlay).
    /// Partially consumed — kara-gate reads `width`/colors from the
    /// compositor config today; this block is the theme-driven
    /// equivalent. SVG overlay is reserved.
    #[serde(default)]
    pub window_border: Option<WindowBorderSpec>,
    /// Bar (kara-sight) styling. Reserved — kara-sight renderer will
    /// pick these up once module config cleanup lands.
    #[serde(default)]
    pub bar: Option<BarStyleSpec>,
    /// Sound theme + per-event overrides. Reserved — whisper + kara-gate
    /// event hooks will consume these when sound support lands.
    #[serde(default)]
    pub sounds: Option<SoundsSpec>,
    /// Notification (kara-whisper) style. Reserved.
    #[serde(default)]
    pub notification: Option<NotificationStyleSpec>,
    /// Launcher (kara-summon) style. Reserved.
    #[serde(default)]
    pub launcher: Option<LauncherStyleSpec>,
    /// Lock screen (kara-veil) style. Reserved.
    #[serde(default)]
    pub lock_screen: Option<LockScreenStyleSpec>,
    /// Animation durations and easing curves. Reserved — kara-gate and
    /// kara-beautify animation path will consume these.
    #[serde(default)]
    pub animations: Option<AnimationsSpec>,
    /// Named variants. Empty map means "single-palette theme" and the
    /// resolver uses the top-level palette block. Populated means the
    /// theme is multi-variant and `default_variant` (or `--variant`)
    /// picks which one to apply.
    #[serde(default)]
    pub variants: std::collections::BTreeMap<String, VariantSpec>,
}

impl ThemeSpec {
    pub fn load_from_file(path: &Path) -> Result<Self> {
        let raw = fs::read_to_string(path)?;
        let spec: ThemeSpec = toml::from_str(&raw)?;
        validate_spec(&spec)?;
        Ok(spec)
    }

    pub fn save_to_file(&self, path: &Path) -> Result<()> {
        validate_spec(self)?;
        let raw = toml::to_string_pretty(self)?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(path, raw)?;
        Ok(())
    }
}
