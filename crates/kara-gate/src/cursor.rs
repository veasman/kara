//! Software cursor rendering via XCursor theme loading.

use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::element::texture::{TextureBuffer, TextureRenderElement};
use smithay::backend::renderer::element::Kind;
use crate::backend_udev::{KaraRenderer, KaraTexture};
use smithay::input::pointer::CursorImageStatus;
use smithay::utils::{Point, Rectangle, Size, Transform};
use std::time::Instant;

use crate::state::Gate;

/// A single animation frame from an XCursor file.
pub struct CursorFrame {
    pub pixels_rgba: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub xhot: i32,
    pub yhot: i32,
    /// Hold this frame for `delay_ms` before advancing. XCursor
    /// stores this as milliseconds per frame; 0 (or one-frame
    /// cursors) means "static — never advance".
    pub delay_ms: u32,
}

/// Cached cursor data loaded from an XCursor theme. Always a
/// non-empty `frames` vec: single-frame cursors are represented
/// as a one-element vec with `delay_ms = 0`, so the render path
/// doesn't need to special-case static vs animated.
pub struct CursorCache {
    pub frames: Vec<CursorFrame>,
    /// GPU uploads parallel to `frames`. Populated lazily on first
    /// use of each frame so a short-lived cursor animation doesn't
    /// force uploads for frames the user never actually sees.
    pub textures: Vec<Option<TextureBuffer<KaraTexture>>>,
    /// Wall-clock origin the animation loops against.
    pub started_at: Instant,
    /// Sum of every frame's delay_ms. 0 when the cursor is static.
    pub total_cycle_ms: u32,
    /// Native pixel size of the xcursor images chosen. GPU scale
    /// factor = target_size / native_size.
    pub native_size: u32,
    /// Logical cursor size requested by config.
    pub target_size: u32,
}

impl CursorCache {
    /// Frame index to render right now, honoring the animation loop.
    /// Returns 0 for static (single-frame or 0-cycle) cursors.
    pub fn current_frame_idx(&self, now: Instant) -> usize {
        if self.frames.len() <= 1 || self.total_cycle_ms == 0 {
            return 0;
        }
        let elapsed_ms =
            (now.duration_since(self.started_at).as_millis() as u64) % (self.total_cycle_ms as u64);
        let mut acc: u64 = 0;
        for (i, f) in self.frames.iter().enumerate() {
            acc += f.delay_ms as u64;
            if elapsed_ms < acc {
                return i;
            }
        }
        self.frames.len() - 1
    }
}

/// Load cursor image from XCursor theme.
///
/// Returns None if theme or icon can't be found.
pub fn load_xcursor(theme_name: &str, icon_name: &str, size: u32) -> Option<CursorCache> {
    let theme = xcursor::CursorTheme::load(theme_name);

    // Try the requested icon name, fall back to common alternatives
    let names = if icon_name == "default" {
        vec!["default", "left_ptr"]
    } else {
        vec![icon_name, "default", "left_ptr"]
    };

    let path = names.iter().find_map(|name| theme.load_icon(name))?;
    let data = std::fs::read(&path).ok()?;
    let images = xcursor::parser::parse_xcursor(&data)?;

    if images.is_empty() {
        return None;
    }

    // Pick the native size closest to the requested size, then take
    // every image at THAT size as the animation sequence. An XCursor
    // file can pack multiple sizes (16/24/32/48/…) AND, within each
    // size, multiple frames with per-frame delays for animated
    // cursors (Miku / Koishi / spinners). Filtering to a single size
    // avoids interleaving frames of different resolutions.
    let closest_size = images
        .iter()
        .min_by_key(|img| (img.size as i32 - size as i32).unsigned_abs())
        .map(|img| img.size)
        .unwrap();

    // Keep pixels at native resolution and let the GPU scale during
    // render with hardware filtering. CPU nearest-neighbor rescale
    // killed the cursor's antialiased edges and produced visibly
    // blocky artifacts at any size that isn't on the theme's native
    // grid. Display size is carried as `target_size` below; frames
    // store native dimensions only.
    let mut frames: Vec<CursorFrame> = Vec::new();
    let mut total_cycle_ms: u32 = 0;
    for img in images.iter().filter(|img| img.size == closest_size) {
        total_cycle_ms = total_cycle_ms.saturating_add(img.delay);
        frames.push(CursorFrame {
            pixels_rgba: img.pixels_rgba.clone(),
            width: img.width,
            height: img.height,
            xhot: img.xhot as i32,
            yhot: img.yhot as i32,
            delay_ms: img.delay,
        });
    }
    if frames.is_empty() {
        return None;
    }
    let textures = (0..frames.len()).map(|_| None).collect();
    Some(CursorCache {
        frames,
        textures,
        started_at: Instant::now(),
        total_cycle_ms,
        native_size: closest_size,
        target_size: size,
    })
}

/// Build a cursor render element for a specific output.
///
/// Returns None if cursor is hidden, not on this output, or theme unavailable.
pub fn build_cursor_element(
    state: &mut Gate,
    renderer: &mut KaraRenderer<'_>,
    output_idx: usize,
) -> Option<TextureRenderElement<KaraTexture>> {
    // Check pointer-on-output and idle-hide using immutable borrows
    // BEFORE taking a &mut cache reference. Otherwise the mutable
    // borrow required to lazily upload per-frame textures collides
    // with the immutable calls into `state`.
    let status = state.cursor_status.clone();
    if matches!(status, CursorImageStatus::Hidden) {
        return None;
    }
    let out = state.outputs.get(output_idx)?;
    let out_rect = Rectangle::new(out.location, (out.size.0, out.size.1).into());
    let px = state.pointer_location.x;
    let py = state.pointer_location.y;
    let pointer_point: Point<i32, smithay::utils::Logical> = (px as i32, py as i32).into();
    if !out_rect.contains(pointer_point) {
        return None;
    }
    let out_loc = out.location;
    let idle = state.cursor_is_idle();

    // Make sure the named cursor is loaded. This is the only path
    // that needs `state` mutably outside of the texture-upload step
    // below, and it only fires once per icon.
    if let CursorImageStatus::Named(icon) = &status {
        if !state.named_cursor_cache.contains_key(icon) {
            let theme_name = state
                .config
                .general
                .cursor_theme
                .as_deref()
                .unwrap_or("default");
            let size = state.config.general.cursor_size as u32;
            if let Some(loaded) = load_xcursor(theme_name, icon.name(), size) {
                state.named_cursor_cache.insert(*icon, loaded);
            }
        }
    }

    // Grab the cache mutably just for the texture upload.
    let cache: &mut CursorCache = match &status {
        CursorImageStatus::Named(icon) => state.named_cursor_cache.get_mut(icon)?,
        CursorImageStatus::Surface(_) => state.cursor_cache.as_mut()?,
        CursorImageStatus::Hidden => return None,
    };

    // Idle-hide: static cursors fade out on idle, animated cursors
    // keep playing. Users watch them play.
    let is_animated = cache.frames.len() > 1 && cache.total_cycle_ms > 0;
    if !is_animated && idle {
        return None;
    }

    let idx = cache.current_frame_idx(Instant::now());
    let frame = cache.frames.get(idx)?;
    let native_size = cache.native_size.max(1);
    let target_size = cache.target_size.max(1);

    // Upload this frame's texture lazily and cache it. The xcursor
    // crate's `pixels_rgba` field is misnamed — the X cursor file
    // format stores ARGB32 in native byte order, which on little-
    // endian is [B, G, R, A]. `Fourcc::Argb8888` is the right
    // match; using `Abgr8888` would swap blue and red.
    let tex_slot = cache.textures.get_mut(idx)?;
    if tex_slot.is_none() {
        *tex_slot = TextureBuffer::from_memory(
            renderer,
            &frame.pixels_rgba,
            Fourcc::Argb8888,
            Size::from((frame.width as i32, frame.height as i32)),
            false,
            1,
            Transform::Normal,
            None,
        )
        .ok();
    }
    let texture_buffer = tex_slot.as_ref()?;

    // Scale factor from native xcursor grid to the user's requested
    // size. Applied to both the dest rect and the hotspot so the
    // cursor tip stays under the pointer regardless of native size.
    let scale = target_size as f64 / native_size as f64;
    let dest_w = (frame.width as f64 * scale).round() as i32;
    let dest_h = (frame.height as f64 * scale).round() as i32;
    let hot_x = frame.xhot as f64 * scale;
    let hot_y = frame.yhot as f64 * scale;

    let local_x = px - out_loc.x as f64 - hot_x;
    let local_y = py - out_loc.y as f64 - hot_y;

    // Pass the target size to the render element — smithay/GL scales
    // the native-sized texture to `dest_w × dest_h` using hardware
    // bilinear filtering. Crisper and cheaper than a CPU rescale.
    Some(TextureRenderElement::from_texture_buffer(
        Point::from((local_x, local_y)),
        texture_buffer,
        None,
        None,
        Some(Size::from((dest_w, dest_h))),
        Kind::Cursor,
    ))
}
