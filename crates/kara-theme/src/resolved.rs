use kara_color::Color;
use crate::{CursorSpec, Density, FontSpec, NvimPreset, SurfaceStyle, UiMode, WindowBorderSpec, BarStyleSpec};

#[derive(Debug, Clone, Copy)]
pub struct SemanticColors {
    pub bg0: Color,
    pub bg1: Color,
    pub bg2: Color,
    pub fg0: Color,
    pub fg1: Color,
    pub fg_muted: Color,
    pub accent: Color,
    pub accent_soft: Color,
    pub accent_contrast: Color,
    pub border_subtle: Color,
    pub border_strong: Color,
    pub selection_bg: Color,
    pub selection_fg: Color,
    pub success: Color,
    pub warning: Color,
    pub danger: Color,
    pub info: Color,
}

#[derive(Debug, Clone)]
pub struct ResolvedStyle {
    pub radius_px: u16,
    pub opacity: f32,
    pub blur: bool,
    pub density: Density,
    pub surface_style: SurfaceStyle,
}

#[derive(Debug, Clone)]
pub struct VwmBarResolved {
    pub style: String,
    pub background: bool,
    pub modules: String,
    pub icons: bool,
    pub colors: bool,
    pub minimal: bool,
    pub height: u16,
    pub radius: u16,
    pub margin_x: u16,
    pub margin_y: u16,
    pub padding_y: u16,
}

#[derive(Debug, Clone)]
pub struct ResolvedTheme {
    pub name: String,
    pub mode: UiMode,
    pub wallpaper: Option<String>,
    pub primary: Color,
    pub semantic: SemanticColors,
    pub ansi: [Color; 16],
    pub base16: [Color; 16],
    pub style: ResolvedStyle,
    pub fonts: FontSpec,
    pub cursor: CursorSpec,
    pub nvim_preset: NvimPreset,
    pub nvim_transparent: bool,
    pub vwm_bar: VwmBarResolved,
    /// User-selected GTK theme name from the theme TOML. Falls back to
    /// Adwaita / Adwaita-dark (mode-dependent) when unset.
    pub gtk_theme_override: Option<String>,
    /// User-selected app icon theme. Falls back to Adwaita when unset.
    pub icon_theme_override: Option<String>,
    /// User-selected file icon theme. Falls back to
    /// `icon_theme_override` when unset — most users pick one theme
    /// that covers both.
    pub file_icon_theme_override: Option<String>,
    /// Theme-driven window border settings. When present, the kara-gate
    /// renderer emits `general { border_px border_radius }` and
    /// `theme { accent border }` keys derived from this spec, overriding
    /// whatever the user's static config had.
    pub window_border: Option<WindowBorderSpec>,
    /// Theme-driven bar settings. Reserved — kara-sight renderer will
    /// consume these once module config cleanup lands.
    pub bar: Option<BarStyleSpec>,
    /// The preset key that materialized this theme's semantic palette.
    /// One of "gruvbox", "vague", "nord", ... or None for the
    /// derive-from-primary path. Lets downstream renderers dispatch to
    /// hand-tuned plugins (nvim: nord.nvim, gruvbox.nvim, vague.nvim)
    /// when a known preset is in play, and fall back to generic
    /// base16-driven output otherwise.
    pub variant_preset: Option<String>,
}

impl ResolvedTheme {
    pub fn gtk_theme_name(&self) -> &str {
        if let Some(name) = self.gtk_theme_override.as_deref() {
            return name;
        }
        match self.mode {
            UiMode::Light => "Adwaita",
            UiMode::Dark | UiMode::Auto => "Adwaita-dark",
        }
    }

    pub fn gtk_icon_theme_name(&self) -> &str {
        self.icon_theme_override
            .as_deref()
            .unwrap_or("Adwaita")
    }

    /// File icon theme for file managers. Falls back to the app
    /// `icon_theme_name` when unset — this is the typical behavior for
    /// themes that don't differentiate.
    pub fn gtk_file_icon_theme_name(&self) -> &str {
        self.file_icon_theme_override
            .as_deref()
            .unwrap_or_else(|| self.gtk_icon_theme_name())
    }

    /// GTK `gtk-font-name` value — the font family followed by the
    /// point size, in GTK's Pango-style format.
    pub fn gtk_font_name(&self) -> String {
        format!("{} {}", self.fonts.ui_family, self.fonts.ui_size)
    }

    pub fn prefer_dark_flag(&self) -> u8 {
        match self.mode {
            UiMode::Light => 0,
            UiMode::Dark | UiMode::Auto => 1,
        }
    }

    pub fn gsettings_color_scheme(&self) -> &'static str {
        match self.mode {
            UiMode::Light => "prefer-light",
            UiMode::Dark | UiMode::Auto => "prefer-dark",
        }
    }

    /// Resolve a palette-reference string to a concrete hex color.
    ///
    /// Accepts three forms:
    ///   * `"$accent"` / `"$bg2"` / etc — looked up in `semantic`
    ///   * `"#1a2b3c"` or `"1a2b3c"` — returned as lowercase hex
    ///   * anything else — returned unchanged (caller decides whether
    ///     to accept it)
    ///
    /// Returned strings are **without** a leading `#` so callers can
    /// prefix as needed (kara-gate uses `0x…`, CSS uses `#…`).
    pub fn resolve_palette_ref(&self, input: &str) -> String {
        if let Some(key) = input.strip_prefix('$') {
            let c = &self.semantic;
            let color = match key {
                "bg" | "bg0" => c.bg0,
                "bg1" | "surface" => c.bg1,
                "bg2" | "bg_alt" | "bg_dim" => c.bg2,
                "fg" | "fg0" | "text" => c.fg0,
                "fg1" => c.fg1,
                "fg_muted" | "text_muted" => c.fg_muted,
                "accent" => c.accent,
                "accent_soft" => c.accent_soft,
                "accent_contrast" => c.accent_contrast,
                "border" | "border_subtle" => c.border_subtle,
                "border_strong" => c.border_strong,
                "selection_bg" => c.selection_bg,
                "selection_fg" => c.selection_fg,
                "success" => c.success,
                "warning" => c.warning,
                "danger" | "red" => c.danger,
                "info" => c.info,
                _ => return input.trim_start_matches('#').to_ascii_lowercase(),
            };
            return color.to_hex().trim_start_matches('#').to_ascii_lowercase();
        }
        input.trim_start_matches('#').to_ascii_lowercase()
    }
}
