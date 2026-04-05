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
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct WallpaperSpec {
    #[serde(default)]
    pub default: Option<String>,
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
