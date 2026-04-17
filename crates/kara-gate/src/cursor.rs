//! Software cursor rendering via XCursor theme loading.

use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::element::texture::{TextureBuffer, TextureRenderElement};
use smithay::backend::renderer::element::Kind;
use crate::backend_udev::{KaraRenderer, KaraTexture};
use smithay::input::pointer::CursorImageStatus;
use smithay::utils::{Point, Rectangle, Size, Transform};

use crate::state::Gate;

/// Cached cursor data loaded from an XCursor theme.
pub struct CursorCache {
    pub pixels_rgba: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub xhot: i32,
    pub yhot: i32,
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

    // Pick the image closest to the requested size. Xcursor files
    // only store discrete sizes (typically 16 / 24 / 32 / 48 / 64),
    // so the closest one won't usually match `size` exactly.
    let image = images
        .iter()
        .min_by_key(|img| (img.size as i32 - size as i32).unsigned_abs())
        .unwrap();

    if image.size == size {
        return Some(CursorCache {
            pixels_rgba: image.pixels_rgba.clone(),
            width: image.width,
            height: image.height,
            xhot: image.xhot as i32,
            yhot: image.yhot as i32,
        });
    }

    // Rescale the nearest-size image to honor the configured
    // `cursor_size` — otherwise requesting size 32 on a theme that
    // only ships a 24px variant silently renders at 24px and the
    // config value appears ignored. Nearest-neighbor keeps cursor
    // edges crisp; a cursor bitmap is tiny so the cost is noise.
    let scale = size as f32 / image.size as f32;
    let new_w = ((image.width as f32) * scale).round().max(1.0) as u32;
    let new_h = ((image.height as f32) * scale).round().max(1.0) as u32;
    let mut out = Vec::with_capacity((new_w * new_h * 4) as usize);
    let src = &image.pixels_rgba;
    let src_w = image.width as usize;
    let src_h = image.height as usize;
    for dy in 0..new_h {
        let sy = ((dy as f32 + 0.5) / scale).floor() as usize;
        let sy = sy.min(src_h - 1);
        for dx in 0..new_w {
            let sx = ((dx as f32 + 0.5) / scale).floor() as usize;
            let sx = sx.min(src_w - 1);
            let i = (sy * src_w + sx) * 4;
            out.push(src[i]);
            out.push(src[i + 1]);
            out.push(src[i + 2]);
            out.push(src[i + 3]);
        }
    }
    let scale_hot = |h: u32| ((h as f32) * scale).round() as i32;

    Some(CursorCache {
        pixels_rgba: out,
        width: new_w,
        height: new_h,
        xhot: scale_hot(image.xhot),
        yhot: scale_hot(image.yhot),
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
    // Don't render cursor if idle
    if state.cursor_is_idle() {
        return None;
    }

    // Determine which cursor cache to use based on cursor_status
    let cache = match &state.cursor_status {
        CursorImageStatus::Hidden => return None,

        CursorImageStatus::Named(icon) => {
            let icon = *icon;
            // Load and cache named cursors on demand
            if !state.named_cursor_cache.contains_key(&icon) {
                let theme_name = state.config.general.cursor_theme
                    .as_deref()
                    .unwrap_or("default");
                let size = state.config.general.cursor_size as u32;
                if let Some(cache) = load_xcursor(theme_name, icon.name(), size) {
                    state.named_cursor_cache.insert(icon, cache);
                }
            }
            state.named_cursor_cache.get(&icon)
        }

        // Client-provided surface cursor — fall back to default cursor for now
        // TODO: render the client's wl_surface as cursor (requires WaylandSurfaceRenderElement)
        CursorImageStatus::Surface(_) => state.cursor_cache.as_ref(),
    };

    let cache = cache?;

    let out = state.outputs.get(output_idx)?;
    let out_rect = Rectangle::new(
        out.location,
        (out.size.0, out.size.1).into(),
    );

    // Check if pointer is on this output
    let px = state.pointer_location.x;
    let py = state.pointer_location.y;
    let pointer_point: Point<i32, smithay::utils::Logical> = (px as i32, py as i32).into();
    if !out_rect.contains(pointer_point) {
        return None;
    }

    // The xcursor crate's `pixels_rgba` field is misnamed — the X
    // cursor file format stores ARGB32 in native byte order, which
    // on little-endian is the byte sequence [B, G, R, A]. Uploading
    // those bytes as Abgr8888 (which expects [R, G, B, A] on LE)
    // swaps blue and red: a yellow Banana cursor renders blue, a
    // blue Banana cursor renders yellow. Argb8888 is the Fourcc
    // that treats the buffer as BGRA bytes.
    let texture_buffer = TextureBuffer::from_memory(
        renderer,
        &cache.pixels_rgba,
        Fourcc::Argb8888,
        Size::from((cache.width as i32, cache.height as i32)),
        false,
        1,
        Transform::Normal,
        None,
    )
    .ok()?;

    // Position in output-local coordinates, adjusted for hotspot
    let local_x = px - out.location.x as f64 - cache.xhot as f64;
    let local_y = py - out.location.y as f64 - cache.yhot as f64;

    Some(TextureRenderElement::from_texture_buffer(
        Point::from((local_x, local_y)),
        &texture_buffer,
        None,
        None,
        None,
        Kind::Cursor,
    ))
}
