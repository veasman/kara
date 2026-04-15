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
//! In-memory cache holds decoded pixmaps for the picker's lifetime.
//! Video wallpapers also go through a persistent disk cache at
//! `~/.cache/kara/thumbs/<hash>.png` because spinning up a gst
//! pipeline per thumbnail adds up — on picker open we'd otherwise
//! pay N * decode cost every single time. Still images are cheap
//! enough (`image::open` is fast on PNGs) that disk caching adds
//! more complexity than it saves.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::fs;
use std::hash::{Hash, Hasher};

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

    // Video containers can't be read by the `image` crate — route
    // them through a short gstreamer pipeline that pulls exactly
    // one frame and tears down.
    let is_video = matches!(
        path.extension()
            .and_then(|s| s.to_str())
            .map(|s| s.to_ascii_lowercase())
            .as_deref(),
        Some("mp4") | Some("mkv") | Some("webm") | Some("mov") | Some("m4v")
    );
    if is_video {
        // Video thumbnails are expensive (spin up a gst pipeline,
        // pull a frame, tear down). Check the persistent disk
        // cache first so second-and-later picker opens skip the
        // decode entirely. Cache key = file path + mtime + size +
        // target dimensions hashed together; any of those changing
        // invalidates naturally.
        if let Some(cached) = disk_cache_load(path, target_w, target_h) {
            return Some(cached);
        }
        let decoded = decode_video_thumb(path, target_w, target_h)?;
        disk_cache_store(path, target_w, target_h, &decoded);
        return Some(decoded);
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

/// Compute the on-disk cache key for a video thumbnail. The key
/// folds in the file path + mtime + size + target dimensions so
/// editing the source file or resizing the picker chip naturally
/// invalidates the cached thumbnail.
fn disk_cache_key(path: &Path, target_w: u32, target_h: u32) -> Option<String> {
    let meta = fs::metadata(path).ok()?;
    let mtime = meta
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs();
    let size = meta.len();
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    path.hash(&mut hasher);
    mtime.hash(&mut hasher);
    size.hash(&mut hasher);
    target_w.hash(&mut hasher);
    target_h.hash(&mut hasher);
    Some(format!("{:016x}.png", hasher.finish()))
}

fn disk_cache_dir() -> Option<PathBuf> {
    let base = dirs::cache_dir()?.join("kara").join("thumbs");
    fs::create_dir_all(&base).ok()?;
    Some(base)
}

/// Try to load a previously-cached video thumbnail from disk.
/// Returns None on cache miss or any I/O error — caller falls
/// back to live decode.
fn disk_cache_load(path: &Path, target_w: u32, target_h: u32) -> Option<Pixmap> {
    let key = disk_cache_key(path, target_w, target_h)?;
    let cache_path = disk_cache_dir()?.join(key);
    if !cache_path.is_file() {
        return None;
    }
    let img = image::open(&cache_path).ok()?;
    let rgba = img.to_rgba8();
    if rgba.width() != target_w || rgba.height() != target_h {
        return None;
    }
    let mut pix = Pixmap::new(target_w, target_h)?;
    pix.data_mut().copy_from_slice(rgba.as_raw());
    Some(pix)
}

/// Persist a freshly-decoded video thumbnail to disk. Errors are
/// swallowed — a failed cache write just means we'll decode again
/// next time.
fn disk_cache_store(path: &Path, target_w: u32, target_h: u32, pix: &Pixmap) {
    let key = match disk_cache_key(path, target_w, target_h) {
        Some(k) => k,
        None => return,
    };
    let dir = match disk_cache_dir() {
        Some(d) => d,
        None => return,
    };
    let cache_path = dir.join(key);
    let img = match image::RgbaImage::from_raw(
        pix.width(),
        pix.height(),
        pix.data().to_vec(),
    ) {
        Some(i) => i,
        None => return,
    };
    let _ = img.save(&cache_path);
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

/// Extract the first decoded frame from a video file via a
/// throwaway gstreamer pipeline, then scale-to-cover it into a
/// tiny-skia Pixmap. Returns None if gst can't handle the file,
/// the pipeline stalls, or decode otherwise fails.
fn decode_video_thumb(path: &Path, target_w: u32, target_h: u32) -> Option<Pixmap> {
    use gstreamer as gst;
    use gstreamer::prelude::*;
    use gstreamer_app as gst_app;

    if gst::init().is_err() {
        eprintln!("kara-summon: gstreamer init failed — no video thumbnails");
        return None;
    }

    let uri = match gst::glib::filename_to_uri(path, None) {
        Ok(u) => u.to_string(),
        Err(_) => return None,
    };

    let pipeline_desc = format!(
        "uridecodebin uri=\"{uri}\" ! videoconvert ! videoscale \
         ! video/x-raw,format=RGBA \
         ! appsink name=sink sync=false max-buffers=1 drop=false"
    );

    let element = gst::parse::launch(&pipeline_desc).ok()?;
    let pipeline = element.downcast::<gst::Pipeline>().ok()?;
    let sink = pipeline
        .by_name("sink")?
        .downcast::<gst_app::AppSink>()
        .ok()?;

    if pipeline.set_state(gst::State::Playing).is_err() {
        let _ = pipeline.set_state(gst::State::Null);
        return None;
    }

    // Pull the first sample with a short timeout so a stuck
    // pipeline doesn't hang the picker. 500ms is generous for a
    // local file's first keyframe.
    let sample = sink.try_pull_sample(gst::ClockTime::from_mseconds(500));
    let _ = pipeline.set_state(gst::State::Null);
    let sample = sample?;

    let buffer = sample.buffer()?;
    let caps = sample.caps()?;
    let info = gstreamer_video::VideoInfo::from_caps(caps).ok()?;
    let w = info.width();
    let h = info.height();
    let map = buffer.map_readable().ok()?;

    let rgba_buf = image::RgbaImage::from_raw(w, h, map.as_slice().to_vec())?;
    let dyn_img = image::DynamicImage::ImageRgba8(rgba_buf);
    let scaled = scale_to_cover(dyn_img, target_w, target_h);

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
