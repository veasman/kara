//! Software cursor rendering via XCursor theme loading.

use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::element::texture::{TextureBuffer, TextureRenderElement};
use smithay::backend::renderer::element::Kind;
use smithay::backend::renderer::gles::{GlesRenderer, GlesTexture};
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

    // Pick the image closest to the requested size
    let image = images
        .iter()
        .min_by_key(|img| (img.size as i32 - size as i32).unsigned_abs())
        .unwrap();

    Some(CursorCache {
        pixels_rgba: image.pixels_rgba.clone(),
        width: image.width,
        height: image.height,
        xhot: image.xhot as i32,
        yhot: image.yhot as i32,
    })
}

/// Build a cursor render element for a specific output.
///
/// Returns None if cursor is hidden, not on this output, or theme unavailable.
pub fn build_cursor_element(
    state: &mut Gate,
    renderer: &mut GlesRenderer,
    output_idx: usize,
) -> Option<TextureRenderElement<GlesTexture>> {
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

    let texture_buffer = TextureBuffer::from_memory(
        renderer,
        &cache.pixels_rgba,
        Fourcc::Abgr8888,
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
