use kara_color::Color;
use crate::{CursorSpec, Density, FontSpec, NvimPreset, SurfaceStyle, UiMode};

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
}

impl ResolvedTheme {
    pub fn gtk_theme_name(&self) -> &'static str {
        match self.mode {
            UiMode::Light => "Adwaita",
            UiMode::Dark | UiMode::Auto => "Adwaita-dark",
        }
    }

    pub fn gtk_icon_theme_name(&self) -> &'static str {
        "Adwaita"
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
}
