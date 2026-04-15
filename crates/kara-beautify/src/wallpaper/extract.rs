use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Result, bail};
use image::ImageReader;
use image::imageops::FilterType;

use kara_color::Color;
use kara_theme::{
    CursorSpec, FontSpec, NvimPreset, NvimSpec, PaletteSpec, StyleSpec, ThemeMeta, ThemeSpec,
    UiMode, VwmBarModules, VwmBarSpec, VwmBarStyle, WallpaperSpec,
};

#[derive(Debug, Clone, Copy)]
pub struct RankedSwatch {
    pub color: Color,
    pub score: f32,
}

pub fn extract_primary_from_image(path: &Path) -> Result<Color> {
    let ranked = ranked_swatches_from_image(path)?;
    let Some(primary) = ranked.first().map(|s| s.color) else {
        bail!("failed to derive primary from image");
    };
    Ok(primary)
}

pub fn infer_mode_from_image(path: &Path) -> Result<UiMode> {
    let swatches = extract_swatches_from_image(path)?;
    if swatches.is_empty() {
        return Ok(UiMode::Dark);
    }

    let avg_luma = swatches.iter().map(|c| c.luminance()).sum::<f32>() / swatches.len() as f32;

    Ok(if avg_luma > 0.48 {
        UiMode::Light
    } else {
        UiMode::Dark
    })
}

pub fn derive_theme_from_image(name: &str, image_path: &Path) -> Result<(ThemeSpec, PathBuf)> {
    let primary = extract_primary_from_image(image_path)?;
    let mode = infer_mode_from_image(image_path)?;

    let wallpaper_name = image_path
        .file_name()
        .map(|s| s.to_string_lossy().to_string());

    let nvim_preset = match name {
        "gruvbox" => NvimPreset::Gruvbox,
        "vague" => NvimPreset::Vague,
        _ => NvimPreset::Semantic,
    };

    let vwm_bar = match name {
        "gruvbox" => VwmBarSpec {
            style: VwmBarStyle::Docked,
            background: true,
            modules: VwmBarModules::Flat,
            icons: true,
            colors: true,
            minimal: false,
            height: 24,
            radius: 4,
            margin_x: 0,
            margin_y: 0,
            padding_y: 0,
        },
        "vague" => VwmBarSpec {
            style: VwmBarStyle::Floating,
            background: false,
            modules: VwmBarModules::Pill,
            icons: true,
            colors: true,
            minimal: false,
            height: 27,
            radius: 12,
            margin_x: 12,
            margin_y: 10,
            padding_y: 4,
        },
        _ => match mode {
            UiMode::Light => VwmBarSpec {
                style: VwmBarStyle::Floating,
                background: false,
                modules: VwmBarModules::Pill,
                icons: true,
                colors: true,
                minimal: false,
                height: 30,
                radius: 22,
                margin_x: 18,
                margin_y: 12,
                padding_y: 6,
            },
            UiMode::Dark | UiMode::Auto => VwmBarSpec {
                style: VwmBarStyle::Floating,
                background: false,
                modules: VwmBarModules::Pill,
                icons: true,
                colors: true,
                minimal: false,
                height: 27,
                radius: 12,
                margin_x: 12,
                margin_y: 10,
                padding_y: 4,
            },
        },
    };

    let spec = ThemeSpec {
        meta: ThemeMeta {
            name: name.to_string(),
            mode,
            default_variant: None,
            display_name: None,
            author: None,
        },
        variants: Default::default(),
        wallpaper: WallpaperSpec {
            default: wallpaper_name.clone(),
            per_monitor: None,
        },
        palette: PaletteSpec {
            primary: primary.to_hex(),
            ..PaletteSpec::default()
        },
        style: StyleSpec::default(),
        fonts: FontSpec::default(),
        cursor: CursorSpec::default(),
        nvim: NvimSpec {
            preset: nvim_preset,
            transparent: true,
        },
        vwm_bar,
        gtk_theme: None,
        icon_theme: None,
        file_icon_theme: None,
        window_border: None,
        bar: None,
        sounds: None,
        notification: None,
        launcher: None,
        lock_screen: None,
        animations: None,
    };

    Ok((spec, image_path.to_path_buf()))
}

pub fn extract_swatches_from_image(path: &Path) -> Result<Vec<Color>> {
    Ok(ranked_swatches_from_image(path)?
        .into_iter()
        .map(|s| s.color)
        .collect())
}

pub fn ranked_swatches_from_image(path: &Path) -> Result<Vec<RankedSwatch>> {
    let img = ImageReader::open(path)?.decode()?;
    let img = img.resize(220, 220, FilterType::Triangle).to_rgb8();

    let mut buckets: HashMap<(u8, u8, u8), u32> = HashMap::new();

    for p in img.pixels() {
        let r = quantize_channel(p[0]);
        let g = quantize_channel(p[1]);
        let b = quantize_channel(p[2]);
        *buckets.entry((r, g, b)).or_insert(0) += 1;
    }

    let mut colors: Vec<((u8, u8, u8), u32)> = buckets.into_iter().collect();
    colors.sort_by(|a, b| b.1.cmp(&a.1));

    let mut ranked = Vec::new();

    for ((r, g, b), weight) in colors.into_iter().take(16) {
        let color = Color::new(r, g, b);
        let score = score_swatch(color, weight);
        ranked.push(RankedSwatch { color, score });
    }

    ranked.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    Ok(ranked)
}

fn score_swatch(c: Color, weight: u32) -> f32 {
    let l = c.luminance();
    let chroma = rgb_chroma(c);
    let prominence = (weight as f32).ln().max(0.0) * 0.08;

    let chroma_score = chroma * 2.2;
    let luminance_penalty = if !(0.16..=0.82).contains(&l) {
        0.45
    } else {
        0.0
    };

    chroma_score + prominence - luminance_penalty
}

fn rgb_chroma(c: Color) -> f32 {
    let r = c.r as f32 / 255.0;
    let g = c.g as f32 / 255.0;
    let b = c.b as f32 / 255.0;
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    max - min
}

fn quantize_channel(v: u8) -> u8 {
    let step = 32u8;
    let q = (v / step) * step;
    q.saturating_add(step / 2)
}
