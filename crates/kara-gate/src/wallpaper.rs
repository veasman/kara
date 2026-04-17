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

/// Loaded wallpaper ready for GPU upload. May be a still image,
/// a multi-frame GIF, or a live video stream.
pub struct Wallpaper {
    kind: WallpaperKind,
    width: u32,
    height: u32,
    /// Cached copy of the most recently uploaded frame's raw pixels.
    /// Used by bar blur to crop + blur without a GPU readback. Updated
    /// every time `texture()` uploads a new frame. The pixel format
    /// matches the source: premultiplied RGBA for images/GIFs, BGRA
    /// for gstreamer video — bar blur treats all channels identically
    /// (box blur on R/G/B/A independently) so the swizzle doesn't
    /// affect the blur quality.
    last_pixels: Option<Vec<u8>>,
}

enum WallpaperKind {
    /// Still image or GIF — frames are pre-decoded in memory.
    Frames {
        frames: Vec<Frame>,
        /// Index of the frame currently being shown.
        current: usize,
        /// Wall-clock time at which the current frame became visible.
        /// When `now - frame_started >= frames[current].delay`, tick
        /// advances to the next frame.
        frame_started: Instant,
    },
    /// Video stream — frames arrive asynchronously from a
    /// GStreamer pipeline. The video module handles its own
    /// frame pacing; wallpaper tick just swaps in the latest
    /// decoded frame if one is available.
    Video {
        stream: crate::video::VideoStream,
        /// Most recent uploaded texture + the serial of the frame
        /// it was built from. `tick()` compares against the stream's
        /// latest frame and rebuilds the texture if there's a newer
        /// one available.
        current_texture: Option<TextureBuffer<KaraTexture>>,
        uploaded_serial: u64,
    },
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
            Some("mp4") | Some("mkv") | Some("webm") | Some("mov") | Some("m4v") => {
                Self::load_video(path)
            }
            _ => Self::load_still(path),
        }
    }

    /// Video-wallpaper load path. Builds a GStreamer pipeline via
    /// `video::VideoStream::load`, which blocks briefly until the
    /// first decoded frame arrives so we have real dimensions.
    fn load_video(path: &Path) -> Option<Self> {
        let stream = crate::video::VideoStream::load(path)?;
        let (width, height) = stream.dimensions();
        Some(Self {
            kind: WallpaperKind::Video {
                stream,
                current_texture: None,
                uploaded_serial: 0,
            },
            width,
            height,
            last_pixels: None,
        })
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

        let last_pixels = Some(data.clone());
        Some(Self {
            kind: WallpaperKind::Frames {
                frames: vec![Frame {
                    rgba: data,
                    delay: Duration::from_secs(60 * 60 * 24 * 365),
                    cached_texture: None,
                }],
                current: 0,
                frame_started: Instant::now(),
            },
            last_pixels,
            width,
            height,
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

        let last_pixels = frames.first().map(|f| f.rgba.clone());
        Some(Self {
            kind: WallpaperKind::Frames {
                frames,
                current: 0,
                frame_started: Instant::now(),
            },
            width,
            height,
            last_pixels,
        })
    }

    /// Borrow the current frame's raw premultiplied RGBA pixel data.
    /// Used by the bar blur path to sample the bar-region pixels
    /// without a GPU readback.
    pub fn current_rgba(&self) -> Option<(&[u8], u32, u32)> {
        self.last_pixels
            .as_ref()
            .map(|p| (p.as_slice(), self.width, self.height))
    }

    /// Source image dimensions in pixels. Used by the render path
    /// to compute an aspect-preserving center-crop when the
    /// wallpaper and the output don't share the same aspect ratio.
    pub fn dimensions(&self) -> (u32, u32) {
        match &self.kind {
            WallpaperKind::Video { stream, .. } => stream.dimensions(),
            _ => (self.width, self.height),
        }
    }

    /// Advance the animation (GIF frame delay) or pull a new video
    /// frame. Returns true if the visible pixels changed so the
    /// caller knows to request a redraw.
    pub fn tick(&mut self) -> bool {
        match &mut self.kind {
            WallpaperKind::Frames {
                frames,
                current,
                frame_started,
            } => {
                if frames.len() <= 1 {
                    return false;
                }
                let delay = frames[*current].delay;
                if frame_started.elapsed() >= delay {
                    *current = (*current + 1) % frames.len();
                    *frame_started = Instant::now();
                    return true;
                }
                false
            }
            WallpaperKind::Video { stream, .. } => {
                // Only report a new frame when the decode pipeline
                // actually produced one since the last take_latest().
                // Returning `true` unconditionally made downstream
                // invalidation (bar blur, bar_dirty) fire at tick
                // cadence regardless of whether a frame changed —
                // which rebuilt the full CPU box blur of the bar
                // region 60×/sec even during idle video stretches
                // and showed up as compositor-wide choppiness.
                stream.has_pending_frame()
            }
        }
    }

    /// Duration until the next wallpaper tick should fire. For a
    /// GIF this is delay minus elapsed; for a video it's a fixed
    /// ~16ms so we sample the pipeline at roughly 60Hz; for
    /// stills it's None (don't schedule a tick).
    pub fn next_frame_due(&self) -> Option<Duration> {
        match &self.kind {
            WallpaperKind::Frames {
                frames,
                current,
                frame_started,
            } => {
                if frames.len() <= 1 {
                    return None;
                }
                let delay = frames[*current].delay;
                let elapsed = frame_started.elapsed();
                Some(delay.saturating_sub(elapsed))
            }
            WallpaperKind::Video { .. } => Some(Duration::from_millis(16)),
        }
    }

    /// Get the GPU texture for the currently-visible frame. For
    /// stills/GIFs this lazily uploads and caches per frame. For
    /// video this checks the pipeline's latest slot and re-uploads
    /// if there's a newer frame than the one already on the GPU.
    pub fn texture(
        &mut self,
        renderer: &mut KaraRenderer<'_>,
    ) -> Option<&TextureBuffer<KaraTexture>> {
        match &mut self.kind {
            WallpaperKind::Frames {
                frames, current, ..
            } => {
                let idx = *current;
                let width = self.width;
                let height = self.height;
                let frame = frames.get_mut(idx)?;

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
            WallpaperKind::Video {
                stream,
                current_texture,
                uploaded_serial,
            } => {
                // Pull the latest frame from the decode thread. If
                // there's nothing new since we last checked, reuse
                // the previously uploaded texture.
                if let Some(frame) = stream.take_latest() {
                    if frame.serial != *uploaded_serial {
                        self.width = frame.width;
                        self.height = frame.height;
                        // Convert BGRA→RGBA so downstream CPU consumers
                        // (bar blur) get a consistent pixel format.
                        let mut rgba = frame.bgra.clone();
                        for px in rgba.chunks_exact_mut(4) {
                            px.swap(0, 2); // B↔R
                        }
                        self.last_pixels = Some(rgba);
                        let new_tex = TextureBuffer::from_memory(
                            renderer,
                            &frame.bgra,
                            Fourcc::Argb8888,
                            Size::from((frame.width as i32, frame.height as i32)),
                            false,
                            1,
                            Transform::Normal,
                            None,
                        )
                        .map_err(|e| {
                            tracing::error!("failed to upload video frame: {e:?}")
                        })
                        .ok();
                        if new_tex.is_some() {
                            *current_texture = new_tex;
                            *uploaded_serial = frame.serial;
                        }
                    }
                }
                current_texture.as_ref()
            }
        }
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
