use anyhow::Result;

use kara_color::Color;
use crate::{
    AccentStrategy, ContrastLevel, FontSpec, ResolvedStyle, ResolvedTheme, SemanticColors,
    ThemeSpec, UiMode, VwmBarModules, VwmBarResolved, VwmBarStyle,
};

#[derive(Debug, Clone, Copy)]
pub struct Palette16(pub [Color; 16]);

/// Resolve a theme to its materialized form.
///
/// `variant` selects which entry in `spec.variants` to use. If None, the
/// resolver picks in this order:
///   1. `spec.meta.default_variant` (if the theme declares one)
///   2. The first key in `spec.variants` (if any)
///   3. The top-level `spec.palette` block (single-palette theme)
///
/// When a variant is used, its `preset` field (if any) selects a
/// built-in preset by name. Unknown preset names fall back to the
/// derive-from-primary code path using the variant's inline palette.
pub fn resolve_theme(spec: &ThemeSpec, variant: Option<&str>) -> Result<ResolvedTheme> {
    // If the caller explicitly asked for a variant, it must exist.
    // Previously unknown variants silently fell through to deriving
    // from the top-level palette, which hides typos ("--variant noord")
    // behind a mystery fallback. Now they're a hard error so the user
    // knows immediately.
    if let Some(requested) = variant {
        if !spec.variants.is_empty() && !spec.variants.contains_key(requested) {
            let available: Vec<&str> =
                spec.variants.keys().map(|s| s.as_str()).collect();
            anyhow::bail!(
                "variant '{requested}' not found in theme '{}'. available: {}",
                spec.meta.name,
                available.join(", ")
            );
        }
    }

    // Figure out which variant (if any) to apply.
    let variant_name = variant
        .map(|s| s.to_string())
        .or_else(|| spec.meta.default_variant.clone())
        .or_else(|| spec.variants.keys().next().cloned());

    let variant_spec = variant_name
        .as_deref()
        .and_then(|name| spec.variants.get(name));

    // Pick the effective palette + preset key. Variant's palette wins
    // over the top-level palette when present.
    let palette_spec = variant_spec
        .and_then(|v| v.palette.as_ref())
        .unwrap_or(&spec.palette);
    let primary = Color::from_hex(&palette_spec.primary)?;

    // Which preset identifier to use? Priority:
    //   1. variant.preset (explicit opt-in to a hand-tuned preset)
    //   2. variant name itself (convention: the variant key is the preset name)
    //   3. spec.meta.name (legacy behavior — preset keyed by theme name)
    let preset_key = variant_spec
        .and_then(|v| v.preset.clone())
        .or_else(|| variant_name.clone())
        .unwrap_or_else(|| spec.meta.name.clone());

    let style = resolve_style(&spec.style);
    let fonts = spec.fonts.clone();
    let vwm_bar = resolve_vwm_bar(spec);

    let (semantic, ansi, base16) = match preset_by_name(&preset_key) {
        Some(preset) => preset,
        None => {
            let semantic = derive_semantic(
                spec.meta.mode,
                primary,
                palette_spec.accent_strategy,
                palette_spec.contrast,
            );
            let ansi = derive_ansi(semantic, primary).0;
            let base16 = derive_base16(semantic, ansi);
            (semantic, ansi, base16)
        }
    };

    // Wallpaper priority: variant's wallpaper override, then top-level.
    let wallpaper = variant_spec
        .and_then(|v| v.wallpaper.clone())
        .or_else(|| spec.wallpaper.default.clone());

    // The resolved name encodes both theme and variant so generated
    // output files can be traced back to their source.
    let resolved_name = match &variant_name {
        Some(v) => format!("{}:{}", spec.meta.name, v),
        None => spec.meta.name.clone(),
    };

    Ok(ResolvedTheme {
        name: resolved_name,
        mode: spec.meta.mode,
        wallpaper,
        primary,
        semantic,
        ansi,
        base16,
        style,
        fonts,
        cursor: spec.cursor.clone(),
        nvim_preset: spec.nvim.preset,
        nvim_transparent: spec.nvim.transparent,
        vwm_bar,
    })
}

/// Look up a built-in preset by name. Returns None if not found, in
/// which case the caller falls back to the derive-from-primary path.
fn preset_by_name(name: &str) -> Option<(SemanticColors, [Color; 16], [Color; 16])> {
    match name {
        "gruvbox" => Some(preset_gruvbox()),
        "vague" => Some(preset_vague()),
        "nord" => Some(preset_nord()),
        _ => None,
    }
}

fn resolve_style(style: &crate::StyleSpec) -> ResolvedStyle {
    let radius_px = match style.radius {
        crate::Radius::None => 0,
        crate::Radius::Small => 6,
        crate::Radius::Medium => 12,
        crate::Radius::Large => 18,
    };

    ResolvedStyle {
        radius_px,
        opacity: style.transparency.clamp(0.05, 1.0),
        blur: style.blur,
        density: style.density,
        surface_style: style.surface_style,
    }
}

fn resolve_vwm_bar(spec: &ThemeSpec) -> VwmBarResolved {
    VwmBarResolved {
        style: match spec.vwm_bar.style {
            VwmBarStyle::Docked => "docked".to_string(),
            VwmBarStyle::Floating => "floating".to_string(),
        },
        background: spec.vwm_bar.background,
        modules: match spec.vwm_bar.modules {
            VwmBarModules::Flat => "flat".to_string(),
            VwmBarModules::Pill => "pill".to_string(),
        },
        icons: spec.vwm_bar.icons,
        colors: spec.vwm_bar.colors,
        minimal: spec.vwm_bar.minimal,
        height: spec.vwm_bar.height,
        radius: spec.vwm_bar.radius,
        margin_x: spec.vwm_bar.margin_x,
        margin_y: spec.vwm_bar.margin_y,
        padding_y: spec.vwm_bar.padding_y,
    }
}

fn preset_gruvbox() -> (SemanticColors, [Color; 16], [Color; 16]) {
    let semantic = SemanticColors {
        bg0: Color::new(0x1d, 0x20, 0x21),
        bg1: Color::new(0x28, 0x28, 0x28),
        bg2: Color::new(0x32, 0x30, 0x2f),
        fg0: Color::new(0xeb, 0xdb, 0xb2),
        fg1: Color::new(0xd5, 0xc4, 0xa1),
        fg_muted: Color::new(0xa8, 0x99, 0x84),
        accent: Color::new(0xd7, 0x99, 0x21),
        accent_soft: Color::new(0xb5, 0x76, 0x14),
        accent_contrast: Color::new(0x28, 0x28, 0x28),
        border_subtle: Color::new(0x50, 0x49, 0x45),
        border_strong: Color::new(0x66, 0x5c, 0x54),
        selection_bg: Color::new(0x45, 0x85, 0x88),
        selection_fg: Color::new(0xfb, 0xf1, 0xc7),
        success: Color::new(0xb8, 0xbb, 0x26),
        warning: Color::new(0xfa, 0xbd, 0x2f),
        danger: Color::new(0xfb, 0x49, 0x34),
        info: Color::new(0x83, 0xa5, 0x98),
    };

    let ansi = [
        Color::new(0x28, 0x28, 0x28),
        Color::new(0xcc, 0x24, 0x1d),
        Color::new(0x98, 0x97, 0x1a),
        Color::new(0xd7, 0x99, 0x21),
        Color::new(0x45, 0x85, 0x88),
        Color::new(0xb1, 0x62, 0x86),
        Color::new(0x68, 0x9d, 0x6a),
        Color::new(0xa8, 0x99, 0x84),
        Color::new(0x92, 0x83, 0x74),
        Color::new(0xfb, 0x49, 0x34),
        Color::new(0xb8, 0xbb, 0x26),
        Color::new(0xfa, 0xbd, 0x2f),
        Color::new(0x83, 0xa5, 0x98),
        Color::new(0xd3, 0x86, 0x9b),
        Color::new(0x8e, 0xc0, 0x7c),
        Color::new(0xeb, 0xdb, 0xb2),
    ];

    let base16 = [
        Color::new(0x28, 0x28, 0x28),
        Color::new(0x3c, 0x38, 0x36),
        Color::new(0x50, 0x49, 0x45),
        Color::new(0x66, 0x5c, 0x54),
        Color::new(0xbd, 0xae, 0x93),
        Color::new(0xd5, 0xc4, 0xa1),
        Color::new(0xeb, 0xdb, 0xb2),
        Color::new(0xfb, 0xf1, 0xc7),
        Color::new(0xfb, 0x49, 0x34),
        Color::new(0xfe, 0x80, 0x19),
        Color::new(0xfa, 0xbd, 0x2f),
        Color::new(0xb8, 0xbb, 0x26),
        Color::new(0x8e, 0xc0, 0x7c),
        Color::new(0x83, 0xa5, 0x98),
        Color::new(0xd3, 0x86, 0x9b),
        Color::new(0xd6, 0x5d, 0x0e),
    ];

    (semantic, ansi, base16)
}

fn preset_vague() -> (SemanticColors, [Color; 16], [Color; 16]) {
    let semantic = SemanticColors {
        bg0: Color::new(0x14, 0x14, 0x15),
        bg1: Color::new(0x1c, 0x1c, 0x24),
        bg2: Color::new(0x25, 0x25, 0x30),
        fg0: Color::new(0xcd, 0xcd, 0xcd),
        fg1: Color::new(0xc3, 0xc3, 0xd5),
        fg_muted: Color::new(0x60, 0x60, 0x79),
        accent: Color::new(0x8f, 0x72, 0x9b),
        accent_soft: Color::new(0x6e, 0x5b, 0x78),
        accent_contrast: Color::new(0xf2, 0xee, 0xf5),
        border_subtle: Color::new(0x37, 0x37, 0x45),
        border_strong: Color::new(0x87, 0x87, 0x87),
        selection_bg: Color::new(0x33, 0x37, 0x38),
        selection_fg: Color::new(0xf2, 0xee, 0xf5),
        success: Color::new(0x7f, 0xa5, 0x63),
        warning: Color::new(0xf3, 0xbe, 0x7c),
        danger: Color::new(0xd8, 0x64, 0x7e),
        info: Color::new(0x7e, 0x98, 0xe8),
    };

    let ansi = [
        Color::new(0x14, 0x14, 0x15),
        Color::new(0xd8, 0x64, 0x7e),
        Color::new(0x7f, 0xa5, 0x63),
        Color::new(0xf3, 0xbe, 0x7c),
        Color::new(0x7e, 0x98, 0xe8),
        Color::new(0x8f, 0x72, 0x9b),
        Color::new(0xb4, 0xd4, 0xcf),
        Color::new(0xcd, 0xcd, 0xcd),
        Color::new(0x60, 0x60, 0x79),
        Color::new(0xe0, 0x74, 0x8c),
        Color::new(0x92, 0xb0, 0x72),
        Color::new(0xf3, 0xbe, 0x7c),
        Color::new(0x90, 0xa8, 0xf0),
        Color::new(0xa2, 0x85, 0xae),
        Color::new(0xc0, 0xdd, 0xd8),
        Color::new(0xf2, 0xee, 0xf5),
    ];

    let base16 = [
        Color::new(0x14, 0x14, 0x15),
        Color::new(0x1c, 0x1c, 0x24),
        Color::new(0x25, 0x25, 0x30),
        Color::new(0x60, 0x60, 0x79),
        Color::new(0x87, 0x87, 0x87),
        Color::new(0xcd, 0xcd, 0xcd),
        Color::new(0xdd, 0xdd, 0xdd),
        Color::new(0xf2, 0xee, 0xf5),
        Color::new(0xd8, 0x64, 0x7e),
        Color::new(0xe0, 0xa3, 0x63),
        Color::new(0xf3, 0xbe, 0x7c),
        Color::new(0x7f, 0xa5, 0x63),
        Color::new(0xb4, 0xd4, 0xcf),
        Color::new(0x7e, 0x98, 0xe8),
        Color::new(0x8f, 0x72, 0x9b),
        Color::new(0xbb, 0x9d, 0xbd),
    ];

    (semantic, ansi, base16)
}

fn preset_nord() -> (SemanticColors, [Color; 16], [Color; 16]) {
    // Nord palette by Arctic Ice Studio
    // https://www.nordtheme.com/docs/colors-and-palettes
    let nord0 = Color::new(0x2e, 0x34, 0x40); // polar night 0
    let nord1 = Color::new(0x3b, 0x42, 0x52); // polar night 1
    let nord2 = Color::new(0x43, 0x4c, 0x5e); // polar night 2
    let nord3 = Color::new(0x4c, 0x56, 0x6a); // polar night 3
    let nord4 = Color::new(0xd8, 0xde, 0xe9); // snow storm 0
    let nord5 = Color::new(0xe5, 0xe9, 0xf0); // snow storm 1
    let nord6 = Color::new(0xec, 0xef, 0xf4); // snow storm 2
    let nord7 = Color::new(0x8f, 0xbc, 0xbb); // frost 0
    let nord8 = Color::new(0x88, 0xc0, 0xd0); // frost 1
    let nord9 = Color::new(0x81, 0xa1, 0xc1); // frost 2
    let nord10 = Color::new(0x5e, 0x81, 0xac); // frost 3
    let nord11 = Color::new(0xbf, 0x61, 0x6a); // aurora red
    let nord12 = Color::new(0xd0, 0x87, 0x70); // aurora orange
    let nord13 = Color::new(0xeb, 0xcb, 0x8b); // aurora yellow
    let nord14 = Color::new(0xa3, 0xbe, 0x8c); // aurora green
    let nord15 = Color::new(0xb4, 0x8e, 0xad); // aurora purple

    let semantic = SemanticColors {
        bg0: nord0,
        bg1: nord1,
        bg2: nord2,
        fg0: nord6,
        fg1: nord5,
        fg_muted: nord3,
        accent: nord8,
        accent_soft: nord10,
        accent_contrast: nord0,
        border_subtle: nord2,
        border_strong: nord3,
        selection_bg: nord9,
        selection_fg: nord6,
        success: nord14,
        warning: nord13,
        danger: nord11,
        info: nord7,
    };

    let ansi = [
        nord1, nord11, nord14, nord13, nord9, nord15, nord7, nord5, nord3, nord11, nord14, nord13,
        nord9, nord15, nord7, nord6,
    ];

    let base16 = [
        nord0, nord1, nord2, nord3, nord4, nord5, nord6, nord6, nord11, nord12, nord13, nord14,
        nord8, nord9, nord15, nord10,
    ];

    (semantic, ansi, base16)
}

fn derive_semantic(
    mode: UiMode,
    primary: Color,
    accent_strategy: AccentStrategy,
    contrast: ContrastLevel,
) -> SemanticColors {
    let accent = match accent_strategy {
        AccentStrategy::Vivid => primary.saturate(1.18),
        AccentStrategy::Balanced => primary,
        AccentStrategy::Muted => primary.desaturate(0.25),
    };

    let contrast_boost = match contrast {
        ContrastLevel::Low => 0.0,
        ContrastLevel::Medium => 0.04,
        ContrastLevel::High => 0.08,
    };

    match mode {
        UiMode::Light => {
            let accent = match accent_strategy {
                AccentStrategy::Vivid => primary.saturate(1.12),
                AccentStrategy::Balanced => primary.saturate(1.04),
                AccentStrategy::Muted => primary.desaturate(0.12),
            };

            let base = accent.desaturate(0.82);

            let bg0 = base.lighten(0.94);
            let bg1 = base.lighten(0.88);
            let bg2 = base.lighten(0.80);

            let fg0 = Color::new(28, 32, 38);
            let fg1 = Color::new(52, 58, 66);
            let fg_muted = Color::new(92, 98, 108);

            let accent_soft = accent.mix(bg0, 0.78);
            let accent_contrast = if accent.luminance() > 0.50 {
                Color::new(24, 24, 24)
            } else {
                Color::new(250, 250, 250)
            };

            let border_subtle = bg2.darken(0.10 + contrast_boost * 0.2);
            let border_strong = accent.mix(bg2, 0.30);
            let selection_bg = accent.mix(bg0, 0.42);
            let selection_fg = accent_contrast;

            SemanticColors {
                bg0,
                bg1,
                bg2,
                fg0,
                fg1,
                fg_muted,
                accent,
                accent_soft,
                accent_contrast,
                border_subtle,
                border_strong,
                selection_bg,
                selection_fg,
                success: Color::new(56, 138, 74),
                warning: Color::new(191, 123, 17),
                danger: Color::new(191, 72, 72),
                info: accent.shift_hue(12.0).saturate(1.02),
            }
        }
        UiMode::Dark | UiMode::Auto => {
            let bg0 = accent.desaturate(0.82).darken(0.82);
            let bg1 = bg0.lighten(0.04 + contrast_boost * 0.35);
            let bg2 = bg1.lighten(0.05 + contrast_boost * 0.35);

            let fg0 = Color::new(232, 236, 241);
            let fg1 = fg0.darken(0.10);
            let fg_muted = fg0.mix(bg0, 0.52);

            let accent_soft = accent.mix(bg1, 0.62);
            let accent_contrast = if accent.luminance() > 0.45 {
                Color::new(18, 18, 18)
            } else {
                Color::new(248, 248, 248)
            };

            let border_subtle = bg2.lighten(0.05);
            let border_strong = accent.mix(bg2, 0.35);
            let selection_bg = accent.mix(bg1, 0.48);
            let selection_fg = accent_contrast;

            SemanticColors {
                bg0,
                bg1,
                bg2,
                fg0,
                fg1,
                fg_muted,
                accent,
                accent_soft,
                accent_contrast,
                border_subtle,
                border_strong,
                selection_bg,
                selection_fg,
                success: accent.shift_hue(110.0).saturate(0.95).lighten(0.04),
                warning: accent.shift_hue(45.0).saturate(1.00).lighten(0.05),
                danger: accent.shift_hue(-28.0).saturate(1.05).lighten(0.05),
                info: accent.shift_hue(18.0).saturate(0.98).lighten(0.04),
            }
        }
    }
}

fn derive_ansi(semantic: SemanticColors, primary: Color) -> Palette16 {
    let black = semantic.bg2;
    let red = semantic.danger;
    let green = semantic.success;
    let yellow = semantic.warning;
    let blue = primary;
    let magenta = primary.shift_hue(38.0).lighten(0.04);
    let cyan = primary.shift_hue(-24.0).lighten(0.02);
    let white = semantic.fg1;

    let bright_black = black.darken(0.10);
    let bright_red = red.lighten(0.08);
    let bright_green = green.lighten(0.08);
    let bright_yellow = yellow.lighten(0.08);
    let bright_blue = blue.lighten(0.10);
    let bright_magenta = magenta.lighten(0.08);
    let bright_cyan = cyan.lighten(0.08);
    let bright_white = semantic.fg0;

    Palette16([
        black,
        red,
        green,
        yellow,
        blue,
        magenta,
        cyan,
        white,
        bright_black,
        bright_red,
        bright_green,
        bright_yellow,
        bright_blue,
        bright_magenta,
        bright_cyan,
        bright_white,
    ])
}

fn derive_base16(semantic: SemanticColors, ansi: [Color; 16]) -> [Color; 16] {
    [
        semantic.bg0,
        semantic.bg1,
        semantic.bg2,
        semantic.fg_muted,
        semantic.fg1,
        semantic.fg0,
        semantic.fg0.lighten(0.04),
        semantic.fg0.lighten(0.08),
        ansi[1],
        ansi[9],
        ansi[3],
        ansi[2],
        ansi[14],
        ansi[12],
        ansi[13],
        ansi[8],
    ]
}

#[allow(dead_code)]
fn _font_passthrough(fonts: &FontSpec) -> FontSpec {
    fonts.clone()
}
