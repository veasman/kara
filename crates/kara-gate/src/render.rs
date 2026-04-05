//! Shared render helpers for bar and border textures.
//!
//! Used by both winit and udev backends to build custom render elements.

use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::element::texture::{TextureBuffer, TextureRenderElement};
use smithay::backend::renderer::element::Kind;
use smithay::backend::renderer::gles::{GlesRenderer, GlesTexture};
use smithay::utils::{Point, Size, Transform};

use crate::state::Gate;

/// Render the bar to a texture and return it as a render element.
pub fn render_bar(
    state: &mut Gate,
    renderer: &mut GlesRenderer,
) -> Vec<TextureRenderElement<GlesTexture>> {
    if !state.config.bar.enabled {
        return Vec::new();
    }

    let (output_w, _output_h) = state.output_size;
    let ws_ctx = state.bar_workspace_context();

    let pixmap = match state.bar_renderer.render(
        output_w as u32,
        &state.config.bar,
        &state.config.theme,
        &state.status_cache,
        &ws_ctx,
    ) {
        Some(p) => p,
        None => return Vec::new(),
    };

    let bar_y = match state.config.bar.position {
        kara_config::BarPosition::Top => 0.0,
        kara_config::BarPosition::Bottom => {
            (state.output_size.1 - state.config.bar.height) as f64
        }
    };

    // Upload pixmap as GLES texture
    // tiny-skia Pixmap data is premultiplied RGBA → Fourcc::Abgr8888 in DRM terms
    let texture_buffer = match TextureBuffer::from_memory(
        renderer,
        pixmap.data(),
        Fourcc::Abgr8888,
        Size::from((pixmap.width() as i32, pixmap.height() as i32)),
        false,
        1,
        Transform::Normal,
        None,
    ) {
        Ok(buf) => buf,
        Err(e) => {
            tracing::error!("failed to upload bar texture: {e:?}");
            return Vec::new();
        }
    };

    let element = TextureRenderElement::from_texture_buffer(
        Point::from((0.0, bar_y)),
        &texture_buffer,
        None,
        None,
        None,
        Kind::Unspecified,
    );

    vec![element]
}

/// Render border quads for all visible windows.
/// Each border is a solid-color rectangle rendered behind the window.
pub fn render_borders(
    state: &Gate,
    renderer: &mut GlesRenderer,
) -> Vec<TextureRenderElement<GlesTexture>> {
    let border_px = state.config.general.border_px;
    if border_px <= 0 {
        return Vec::new();
    }

    let accent = state.config.theme.accent;
    let border_color = state.config.theme.border;

    let mut elements = Vec::new();

    for &(rect, is_focused) in &state.border_rects {
        let color = if is_focused { accent } else { border_color };

        let w = rect.size.w.max(1) as u32;
        let h = rect.size.h.max(1) as u32;

        let r = ((color >> 16) & 0xFF) as u8;
        let g = ((color >> 8) & 0xFF) as u8;
        let b = (color & 0xFF) as u8;

        let mut pixmap = match tiny_skia::Pixmap::new(w, h) {
            Some(p) => p,
            None => continue,
        };

        let paint = tiny_skia::Paint {
            shader: tiny_skia::Shader::SolidColor(tiny_skia::Color::from_rgba8(r, g, b, 255)),
            ..Default::default()
        };
        let skia_rect = match tiny_skia::Rect::from_xywh(0.0, 0.0, w as f32, h as f32) {
            Some(r) => r,
            None => continue,
        };
        pixmap.fill_rect(skia_rect, &paint, tiny_skia::Transform::identity(), None);

        // Clear inner area where window content goes
        let inner_x = border_px as f32;
        let inner_y = border_px as f32;
        let inner_w = (w as i32 - border_px * 2).max(0) as f32;
        let inner_h = (h as i32 - border_px * 2).max(0) as f32;

        if inner_w > 0.0 && inner_h > 0.0 {
            let clear_paint = tiny_skia::Paint {
                shader: tiny_skia::Shader::SolidColor(tiny_skia::Color::from_rgba8(0, 0, 0, 0)),
                blend_mode: tiny_skia::BlendMode::Source,
                ..Default::default()
            };
            if let Some(inner_rect) = tiny_skia::Rect::from_xywh(inner_x, inner_y, inner_w, inner_h) {
                pixmap.fill_rect(inner_rect, &clear_paint, tiny_skia::Transform::identity(), None);
            }
        }

        let texture_buffer = match TextureBuffer::from_memory(
            renderer,
            pixmap.data(),
            Fourcc::Abgr8888,
            Size::from((w as i32, h as i32)),
            false,
            1,
            Transform::Normal,
            None,
        ) {
            Ok(buf) => buf,
            Err(e) => {
                tracing::error!("failed to upload border texture: {e:?}");
                continue;
            }
        };

        elements.push(TextureRenderElement::from_texture_buffer(
            Point::from((rect.loc.x as f64, rect.loc.y as f64)),
            &texture_buffer,
            None,
            None,
            None,
            Kind::Unspecified,
        ));
    }

    elements
}

/// Build all custom render elements (wallpaper + borders + bar).
pub fn build_custom_elements(
    state: &mut Gate,
    renderer: &mut GlesRenderer,
) -> Vec<TextureRenderElement<GlesTexture>> {
    let mut elements: Vec<TextureRenderElement<GlesTexture>> = Vec::new();

    // Wallpaper (rendered behind everything)
    if let Some(ref wp) = state.wallpaper {
        if let Some(tex_buf) = wp.upload(renderer) {
            elements.push(TextureRenderElement::from_texture_buffer(
                Point::from((0.0, 0.0)),
                &tex_buf,
                None,
                None,
                None,
                Kind::Unspecified,
            ));
        }
    }

    // Borders (between wallpaper and windows)
    if state.fullscreen_window.is_none() {
        elements.extend(render_borders(state, renderer));
    }

    // Bar (on top, hidden during fullscreen)
    if state.fullscreen_window.is_none() {
        elements.extend(render_bar(state, renderer));
    }

    elements
}
