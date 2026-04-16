//! SVG border + bar tile rasterization.
//!
//! Loads `window_border.svg_tile` (and the bar's reserved `*_svg` slots)
//! at apply time, rasterizes each to a fixed-size PNG, and writes them
//! to `$XDG_STATE_HOME/kara/generated/borders/<theme>-<slot>.png`.
//! kara-gate reads those PNGs via its existing tiny-skia border pipeline
//! — the compositor itself stays free of resvg / usvg deps.
//!
//! Fallback policy: any error loading or rasterizing an SVG is logged,
//! the affected tile is omitted from the generated include, and
//! kara-gate falls back to its solid-color border fill for that theme.
//! A broken SVG never takes the compositor down.
//!
//! This module only handles the rasterization + disk write. The
//! corresponding config-key emission into the kara-gate include file
//! lives in `crate::render::kara_gate` and the compositor-side tiling
//! lives in `kara-gate/src/render.rs`.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use kara_theme::{ResolvedTheme, SvgTileSpec};

/// A successfully rasterized tile ready to be referenced from the
/// generated kara-gate include file. `slot` identifies which theme
/// surface the tile belongs to (e.g. `"window_border"` or `"bar_bg"`).
/// `path` is the absolute path to the written PNG.
#[derive(Debug, Clone)]
pub struct RasterizedTile {
    pub slot: &'static str,
    pub path: PathBuf,
    /// Raster dimensions — surfaced for future bar renderers that
    /// need the tile size to compute module layouts.
    #[allow(dead_code)]
    pub width: u32,
    #[allow(dead_code)]
    pub height: u32,
}

/// Output of a full rasterize-all-slots pass for one theme apply.
#[derive(Debug, Default)]
pub struct RasterizedTileSet {
    pub tiles: Vec<RasterizedTile>,
}

impl RasterizedTileSet {
    pub fn lookup(&self, slot: &'static str) -> Option<&RasterizedTile> {
        self.tiles.iter().find(|t| t.slot == slot)
    }
}

/// Rasterize every SVG tile slot declared on `theme` and write the
/// PNGs into `output_dir`. Creates `output_dir` if needed. Returns a
/// set keyed by slot so the renderer can emit config paths.
///
/// `theme_dir` is the directory of the user's theme TOML (e.g.
/// `~/.local/share/kara/themes/fantasy/`) — relative SVG paths inside
/// the spec are resolved against it.
pub fn rasterize_theme_tiles(
    theme: &ResolvedTheme,
    theme_dir: &Path,
    output_dir: &Path,
) -> Result<RasterizedTileSet> {
    fs::create_dir_all(output_dir)
        .with_context(|| format!("create output dir {}", output_dir.display()))?;

    let mut set = RasterizedTileSet::default();

    // Slot name -> (theme field access) pairs. Keep the list local so
    // adding a new slot is one-line + a new `fs::write` call. When the
    // bar renderer lands, it'll add bar_bg, bar_outline,
    // module_bg, module_outline to this list.
    if let Some(wb) = theme.window_border.as_ref() {
        if let Some(spec) = wb.svg_tile.as_ref() {
            let tint = resolve_tint(spec, theme);
            match rasterize_one_with_tint(
                spec,
                theme_dir,
                output_dir,
                theme_slug(theme),
                "window_border",
                tint,
            ) {
                Ok(tile) => set.tiles.push(tile),
                Err(err) => {
                    eprintln!(
                        "kara-beautify: svg_border rasterize failed for slot 'window_border' in theme '{}': {err:#} — falling back to solid color",
                        theme.name
                    );
                }
            }
        }
    }

    // Reserved bar slots (kara-sight picks these up in the same
    // session as window borders). Each honors its own tint slot
    // independently.
    if let Some(bar) = theme.bar.as_ref() {
        for (slot, spec) in [
            ("bar_bg", bar.background_svg.as_ref()),
            ("bar_outline", bar.outline_svg.as_ref()),
            ("bar_module_bg", bar.module_background_svg.as_ref()),
            ("bar_module_outline", bar.module_outline_svg.as_ref()),
        ] {
            let Some(spec) = spec else { continue };
            let tint = resolve_tint(spec, theme);
            match rasterize_one_with_tint(
                spec,
                theme_dir,
                output_dir,
                theme_slug(theme),
                slot,
                tint,
            ) {
                Ok(tile) => set.tiles.push(tile),
                Err(err) => {
                    eprintln!(
                        "kara-beautify: svg_border rasterize failed for slot '{slot}' in theme '{}': {err:#} — falling back to solid color",
                        theme.name
                    );
                }
            }
        }
    }

    Ok(set)
}

/// Resolve the tile's `tint_from_palette` reference into an RGB
/// triple. Returns `None` when the spec doesn't request a tint.
fn resolve_tint(spec: &SvgTileSpec, theme: &ResolvedTheme) -> Option<(u8, u8, u8)> {
    let key = spec.tint_from_palette.as_deref()?;
    // resolve_palette_ref returns `rrggbb` without a leading `#`.
    let hex = theme.resolve_palette_ref(key);
    if hex.len() != 6 {
        return None;
    }
    let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
    let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
    let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
    Some((r, g, b))
}

/// `theme.name` is `"fantasy"` or `"fantasy:blood"` — collapse it to a
/// filesystem-safe slug used in the generated PNG filenames. We keep
/// the variant in the slug so swapping variants doesn't clobber the
/// other's tiles.
fn theme_slug(theme: &ResolvedTheme) -> String {
    theme.name.replace([':', '/', '\\', ' '], "_")
}

fn rasterize_one_with_tint(
    spec: &SvgTileSpec,
    theme_dir: &Path,
    output_dir: &Path,
    theme_slug: String,
    slot: &'static str,
    tint: Option<(u8, u8, u8)>,
) -> Result<RasterizedTile> {
    // Resolve the SVG path relative to the theme's directory. Absolute
    // paths are left alone so power users can share SVG assets across
    // themes via a shared directory.
    let svg_path = if Path::new(&spec.path).is_absolute() {
        PathBuf::from(&spec.path)
    } else {
        theme_dir.join(&spec.path)
    };

    let svg_bytes = fs::read(&svg_path)
        .with_context(|| format!("read svg {}", svg_path.display()))?;

    // Rasterize at (tile_width × tile_height). If unset, fall back to
    // the SVG's own viewBox size — many hand-drawn borders come with a
    // natural size and `tile_width/height` is only there to override.
    let opt = usvg::Options::default();
    let tree = usvg::Tree::from_data(&svg_bytes, &opt)
        .with_context(|| format!("parse svg {}", svg_path.display()))?;

    let natural = tree.size();
    let target_w = spec
        .tile_width
        .map(|w| w as f32)
        .unwrap_or_else(|| natural.width());
    let target_h = spec
        .tile_height
        .map(|h| h as f32)
        .unwrap_or_else(|| natural.height());

    let target_w = target_w.max(1.0).round() as u32;
    let target_h = target_h.max(1.0).round() as u32;

    let mut pixmap = tiny_skia::Pixmap::new(target_w, target_h)
        .with_context(|| format!("alloc {target_w}x{target_h} pixmap"))?;

    // Scale the SVG's intrinsic size to the target tile size. Non-
    // uniform scaling is accepted on purpose — themes may want a
    // wide-short edge pattern (e.g. 64×8) that stretches the native
    // aspect ratio.
    let sx = target_w as f32 / natural.width().max(1.0);
    let sy = target_h as f32 / natural.height().max(1.0);
    let transform = tiny_skia::Transform::from_scale(sx, sy);
    resvg::render(&tree, transform, &mut pixmap.as_mut());

    // Apply opacity if the spec set one. resvg honors the SVG's own
    // alpha already; this multiplies on top.
    if let Some(op) = spec.opacity {
        if (0.0..1.0).contains(&op) {
            apply_global_opacity(&mut pixmap, op);
        }
    }

    // Apply palette tint if the spec requested one. Per-pixel RGB
    // multiply by the tint color so the hand-drawn art picks up the
    // variant accent. Alpha is untouched so shape / anti-alias stay
    // intact.
    if let Some((tr, tg, tb)) = tint {
        apply_tint(&mut pixmap, tr, tg, tb);
    }

    let out_path = output_dir.join(format!("{theme_slug}-{slot}.png"));
    pixmap
        .save_png(&out_path)
        .with_context(|| format!("write png {}", out_path.display()))?;

    Ok(RasterizedTile {
        slot,
        path: out_path,
        width: target_w,
        height: target_h,
    })
}

/// Tint a rasterized pixmap by multiplying each pixel's RGB channels
/// with the tint color (treated as 0..1 per channel). Preserves alpha
/// so shape / anti-aliasing stays intact. Used to re-color a single
/// hand-drawn SVG tile per theme variant — one artwork file, multiple
/// palette flavors (blood / venom / wraith) without duplicating art.
fn apply_tint(pixmap: &mut tiny_skia::Pixmap, tr: u8, tg: u8, tb: u8) {
    let rf = tr as f32 / 255.0;
    let gf = tg as f32 / 255.0;
    let bf = tb as f32 / 255.0;
    for pixel in pixmap.pixels_mut() {
        // Pre-multiplied channels — multiply each by the tint factor.
        let r = (pixel.red() as f32 * rf).round().clamp(0.0, 255.0) as u8;
        let g = (pixel.green() as f32 * gf).round().clamp(0.0, 255.0) as u8;
        let b = (pixel.blue() as f32 * bf).round().clamp(0.0, 255.0) as u8;
        *pixel = tiny_skia::PremultipliedColorU8::from_rgba(r, g, b, pixel.alpha())
            .unwrap();
    }
}

/// Multiply every pixel's alpha by `op`. Used when the theme spec
/// adds a global `opacity` on top of an SVG that already has its own
/// per-element alpha values.
fn apply_global_opacity(pixmap: &mut tiny_skia::Pixmap, op: f32) {
    let op = op.clamp(0.0, 1.0);
    for pixel in pixmap.pixels_mut() {
        let a = pixel.alpha() as f32;
        let new_a = (a * op).round().clamp(0.0, 255.0) as u8;
        // Pre-multiplied RGBA — also scale RGB by op to stay coherent.
        let r = ((pixel.red()   as f32) * op).round().clamp(0.0, 255.0) as u8;
        let g = ((pixel.green() as f32) * op).round().clamp(0.0, 255.0) as u8;
        let b = ((pixel.blue()  as f32) * op).round().clamp(0.0, 255.0) as u8;
        *pixel = tiny_skia::PremultipliedColorU8::from_rgba(r, g, b, new_a).unwrap();
    }
}
