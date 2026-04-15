//! Wallpaper rendering — loads a still image or an animated GIF
//! and provides per-frame RGBA pixel data for uploading as a
//! `GlesTexture`.
//!
//! A still image (PNG/JPG/WebP) is represented as a single-frame
//! wallpaper with an effectively infinite delay. A GIF is decoded
//! into `Vec<Frame>` with per-frame delays read from the gif
//! chunk. The render path advances the active frame index on a
//! timer in the main event loop — see `Gate::tick_wallpaper()`
//! in state.rs.
//!
//! GPU textures are cached per frame so the first render of each
//! frame uploads and subsequent renders reuse the handle. On a
//! ~30-frame GIF at 1920x1080 that's ~240MB of uploaded texture
//! budget — high but bounded, and kara-gate only holds one
//! wallpaper at a time so a theme switch drops the old set.
//! Video wallpapers (D3) will need streaming uploads instead.

use std::path::Path;
use std::time::{Duration, Instant};

use crate::backend_udev::{KaraRenderer, KaraTexture};
use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::element::texture::TextureBuffer;
use smithay::utils::{Size, Transform};

/// A single animation frame — raw premultiplied RGBA pixels plus
/// the delay before the next frame should show. Still images have
/// exactly one frame with an infinite-ish delay.
pub struct Frame {
    rgba: Vec<u8>,
    delay: Duration,
    cached_texture: Option<TextureBuffer<KaraTexture>>,
}

/// Loaded wallpaper ready for GPU upload. May be single-frame
/// (static image) or multi-frame (GIF). Use `tick()` to advance
/// the animation; `current_texture()` returns the active frame's
/// GPU-uploaded handle.
pub struct Wallpaper {
    frames: Vec<Frame>,
    width: u32,
    height: u32,
    /// Index of the frame currently being shown.
    current: usize,
    /// Wall-clock time at which the current frame became visible.
    /// When `now - frame_started >= frames[current].delay`, tick
    /// advances to the next frame.
    frame_started: Instant,
}

impl Wallpaper {
    /// Load a wallpaper file. PNG/JPG/WebP come in as single-frame
    /// wallpapers; GIF is decoded into all its frames with per-
    /// frame delays preserved from the file.
    pub fn load(path: &Path) -> Option<Self> {
        let extension = path
            .extension()
            .and_then(|s| s.to_str())
            .map(|s| s.to_ascii_lowercase());

        match extension.as_deref() {
            Some("gif") => Self::load_gif(path),
            _ => Self::load_still(path),
        }
    }

    /// Still-image load path — one frame, no animation.
    fn load_still(path: &Path) -> Option<Self> {
        let img = image::open(path)
            .map_err(|e| tracing::error!("failed to load wallpaper '{}': {e}", path.display()))
            .ok()?;

        let rgba_img = img.to_rgba8();
        let width = rgba_img.width();
        let height = rgba_img.height();
        let data = premultiply(rgba_img.into_raw());

        Some(Self {
            frames: vec![Frame {
                rgba: data,
                // ~1 year — effectively "never advance." Used
                // instead of Duration::MAX so any future code
                // that does delay arithmetic doesn't overflow.
                delay: Duration::from_secs(60 * 60 * 24 * 365),
                cached_texture: None,
            }],
            width,
            height,
            current: 0,
            frame_started: Instant::now(),
        })
    }

    /// Animated GIF load path. Uses `image::codecs::gif::GifDecoder`
    /// which yields each frame as an RGBA buffer plus a Delay.
    fn load_gif(path: &Path) -> Option<Self> {
        use image::codecs::gif::GifDecoder;
        use image::AnimationDecoder;
        use std::fs::File;
        use std::io::BufReader;

        let file = File::open(path)
            .map_err(|e| tracing::error!("failed to open gif '{}': {e}", path.display()))
            .ok()?;
        let decoder = GifDecoder::new(BufReader::new(file))
            .map_err(|e| tracing::error!("failed to decode gif '{}': {e}", path.display()))
            .ok()?;

        let mut frames_iter = decoder.into_frames();
        let mut frames: Vec<Frame> = Vec::new();
        let mut dims: Option<(u32, u32)> = None;

        // image's AnimationDecoder returns frames one at a time.
        // Some GIFs have disposal methods (clear vs keep) that
        // affect subsequent frames; the `image` crate handles
        // disposal internally and yields already-composited
        // frames, so we just take the rgba buffer as-is.
        while let Some(result) = frames_iter.next() {
            let frame = match result {
                Ok(f) => f,
                Err(e) => {
                    tracing::warn!(
                        "gif frame decode error in '{}': {e} — stopping at {} frames",
                        path.display(),
                        frames.len()
                    );
                    break;
                }
            };

            let delay: Duration = frame.delay().into();
            // Some malformed GIFs advertise a 0ms delay which
            // would pin the CPU. Clamp to 20ms (~50fps) as a
            // practical lower bound.
            let delay = if delay.as_millis() < 20 {
                Duration::from_millis(100)
            } else {
                delay
            };

            let buffer = frame.into_buffer();
            if dims.is_none() {
                dims = Some((buffer.width(), buffer.height()));
            }
            let rgba = premultiply(buffer.into_raw());
            frames.push(Frame {
                rgba,
                delay,
                cached_texture: None,
            });
        }

        if frames.is_empty() {
            tracing::error!("gif '{}' yielded no frames", path.display());
            return None;
        }

        let (width, height) = dims.unwrap_or((1, 1));

        Some(Self {
            frames,
            width,
            height,
            current: 0,
            frame_started: Instant::now(),
        })
    }

    /// Source image dimensions in pixels. Used by the render path
    /// to compute an aspect-preserving center-crop when the
    /// wallpaper and the output don't share the same aspect ratio.
    pub fn dimensions(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// Number of frames in the wallpaper. 1 for stills, N for GIFs.
    pub fn frame_count(&self) -> usize {
        self.frames.len()
    }

    /// Whether this wallpaper needs the render loop to tick the
    /// animation forward. True for multi-frame GIFs, false for
    /// stills — kara-gate skips the tick call for stills so it
    /// doesn't wake up the main loop on a timer for nothing.
    pub fn is_animated(&self) -> bool {
        self.frames.len() > 1
    }

    /// Advance the animation if the active frame's delay has
    /// elapsed. Returns true if the frame index changed (caller
    /// should request a redraw), false if we're still on the
    /// same frame.
    pub fn tick(&mut self) -> bool {
        if self.frames.len() <= 1 {
            return false;
        }
        let delay = self.frames[self.current].delay;
        if self.frame_started.elapsed() >= delay {
            self.current = (self.current + 1) % self.frames.len();
            self.frame_started = Instant::now();
            return true;
        }
        false
    }

    /// Duration until the active frame should be replaced. Used by
    /// the main loop to schedule its next wakeup — for an animated
    /// wallpaper we want to fire a redraw ~right when the delay
    /// elapses, not sooner or later.
    pub fn next_frame_due(&self) -> Option<Duration> {
        if self.frames.len() <= 1 {
            return None;
        }
        let delay = self.frames[self.current].delay;
        let elapsed = self.frame_started.elapsed();
        Some(delay.saturating_sub(elapsed))
    }

    /// Get or create the GPU texture for the active frame. Cached
    /// per-frame; first render of a frame uploads, subsequent
    /// renders reuse the handle.
    pub fn texture(
        &mut self,
        renderer: &mut KaraRenderer<'_>,
    ) -> Option<&TextureBuffer<KaraTexture>> {
        let idx = self.current;
        let width = self.width;
        let height = self.height;
        let frame = self.frames.get_mut(idx)?;

        if frame.cached_texture.is_none() {
            frame.cached_texture = TextureBuffer::from_memory(
                renderer,
                &frame.rgba,
                Fourcc::Abgr8888,
                Size::from((width as i32, height as i32)),
                false,
                1,
                Transform::Normal,
                None,
            )
            .map_err(|e| tracing::error!("failed to upload wallpaper texture: {e:?}"))
            .ok();
        }
        frame.cached_texture.as_ref()
    }
}

/// Convert straight-alpha RGBA to premultiplied RGBA in place.
/// Required for tiny-skia / GL texture compatibility.
fn premultiply(mut data: Vec<u8>) -> Vec<u8> {
    for pixel in data.chunks_exact_mut(4) {
        let a = pixel[3] as u32;
        pixel[0] = ((pixel[0] as u32 * a) / 255) as u8;
        pixel[1] = ((pixel[1] as u32 * a) / 255) as u8;
        pixel[2] = ((pixel[2] as u32 * a) / 255) as u8;
    }
    data
}
