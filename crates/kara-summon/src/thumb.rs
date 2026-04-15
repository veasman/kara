//! Wallpaper thumbnail cache for the picker's wallpaper row.
//!
//! The picker wallpaper section needs to show small image previews
//! instead of bare filenames. To keep the picker responsive we
//! decode each wallpaper exactly once per session and cache the
//! result as a tiny-skia `Pixmap` ready for blit.
//!
//! Decoding happens lazily on first display request — the picker
//! calls `get_or_decode(&path, target_w, target_h)` from its draw
//! path, and the helper either returns a cached pixmap or decodes
//! the source file synchronously, scales it via `image::imageops`,
//! and caches the result.
//!
//! There's no on-disk cache — the in-memory map dies with the
//! picker process. A persistent `~/.local/state/kara/thumbs/<hash>.png`
//! cache is a B9b.2 nicety for users with very large wallpaper
//! collections; for the typical 5–20 wallpaper case the in-memory
//! cache is plenty fast.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use tiny_skia::Pixmap;

/// In-memory thumbnail cache. Owned by the Picker and dropped on
/// exit. Key is the source wallpaper's absolute path; value is a
/// pre-scaled pixmap at the requested size.
#[derive(Default)]
pub struct ThumbCache {
    cache: HashMap<PathBuf, Option<Pixmap>>,
}

impl ThumbCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// Get a pre-decoded thumbnail, decoding + scaling the source
    /// image on first call. Returns None if the file can't be read,
    /// can't be decoded, or doesn't fit in target dimensions
    /// (degenerate inputs). The None result is cached too — a
    /// broken file is checked at most once per picker session.
    ///
    /// `target_w` / `target_h` are the chip's pixel dimensions in
    /// the picker's render surface. The thumbnail is scaled to
    /// COVER the chip (aspect-preserving center-crop), which keeps
    /// every chip the same size regardless of the source image's
    /// aspect ratio.
    pub fn get_or_decode(
        &mut self,
        path: &Path,
        target_w: u32,
        target_h: u32,
    ) -> Option<&Pixmap> {
        if !self.cache.contains_key(path) {
            let decoded = decode_thumb(path, target_w, target_h);
            self.cache.insert(path.to_path_buf(), decoded);
        }
        self.cache.get(path).and_then(|opt| opt.as_ref())
    }
}

/// Synchronous decode + cover-crop scale. Returns the pixmap
/// ready for blit, or None on any error along the way.
fn decode_thumb(path: &Path, target_w: u32, target_h: u32) -> Option<Pixmap> {
    if target_w == 0 || target_h == 0 {
        return None;
    }

    let img = match image::open(path) {
        Ok(i) => i,
        Err(e) => {
            eprintln!(
                "kara-summon: failed to decode wallpaper thumbnail {}: {e}",
                path.display()
            );
            return None;
        }
    };

    // Scale via image::imageops with COVER semantics so the result
    // matches the picker's chip aspect ratio. We over-resize then
    // crop to keep the cheap nearest-neighbor path lined up with
    // chip pixels.
    let scaled = scale_to_cover(img, target_w, target_h);

    // Convert the scaled RGBA into a tiny-skia Pixmap. tiny-skia
    // expects premultiplied RGBA which matches kara-gate's
    // wallpaper renderer convention.
    let mut data = scaled.into_raw();
    for px in data.chunks_exact_mut(4) {
        let a = px[3] as u32;
        px[0] = ((px[0] as u32 * a) / 255) as u8;
        px[1] = ((px[1] as u32 * a) / 255) as u8;
        px[2] = ((px[2] as u32 * a) / 255) as u8;
    }

    let mut pix = Pixmap::new(target_w, target_h)?;
    pix.data_mut().copy_from_slice(&data);
    Some(pix)
}

/// Aspect-preserving cover-crop scale. Resizes the source so the
/// SHORTER dimension matches the corresponding target dimension
/// (so the longer dimension overflows), then center-crops.
///
/// Mirrors `kara-gate::render::center_crop_src_rect()` so the
/// picker's chip preview lines up visually with what the
/// compositor will actually render at full size.
fn scale_to_cover(
    img: image::DynamicImage,
    target_w: u32,
    target_h: u32,
) -> image::RgbaImage {
    use image::imageops::FilterType;
    use image::GenericImageView;

    let (src_w, src_h) = img.dimensions();
    if src_w == 0 || src_h == 0 || target_w == 0 || target_h == 0 {
        return image::RgbaImage::new(target_w.max(1), target_h.max(1));
    }

    let scale_x = target_w as f32 / src_w as f32;
    let scale_y = target_h as f32 / src_h as f32;
    let scale = scale_x.max(scale_y);

    let scaled_w = ((src_w as f32 * scale).round() as u32).max(target_w);
    let scaled_h = ((src_h as f32 * scale).round() as u32).max(target_h);

    let resized = img.resize_exact(scaled_w, scaled_h, FilterType::Triangle);

    let crop_x = scaled_w.saturating_sub(target_w) / 2;
    let crop_y = scaled_h.saturating_sub(target_h) / 2;

    resized.crop_imm(crop_x, crop_y, target_w, target_h).to_rgba8()
}
