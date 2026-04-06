//! Shared render helpers for bar and border textures.
//!
//! Used by both winit and udev backends to build custom render elements.

use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::element::texture::{TextureBuffer, TextureRenderElement};
use smithay::backend::renderer::element::Kind;
use smithay::backend::renderer::gles::{GlesRenderer, GlesTexture};
use smithay::utils::{Point, Size, Transform};

use crate::state::Gate;

/// Render the bar to a texture for a specific output.
/// CPU-side rasterization is cached and only redone when bar_dirty is set.
pub fn render_bar(
    state: &mut Gate,
    renderer: &mut GlesRenderer,
    output_idx: usize,
) -> Vec<TextureRenderElement<GlesTexture>> {
    if !state.config.bar.enabled {
        return Vec::new();
    }

    let (output_w, output_h) = state.outputs.get(output_idx)
        .map(|o| o.size)
        .unwrap_or((800, 600));

    // Re-rasterize bar only when dirty
    if state.bar_dirty {
        let ws_ctx = state.bar_workspace_context(output_idx);
        if let Some(pixmap) = state.bar_renderer.render(
            output_w as u32,
            &state.config.bar,
            &state.config.theme,
            &state.status_cache,
            &ws_ctx,
        ) {
            state.bar_cache = Some((
                pixmap.data().to_vec(),
                pixmap.width(),
                pixmap.height(),
            ));
        }
        state.bar_dirty = false;
    }

    let (data, w, h) = match state.bar_cache {
        Some(ref c) => c,
        None => return Vec::new(),
    };

    let bar_y = match state.config.bar.position {
        kara_config::BarPosition::Top => 0.0,
        kara_config::BarPosition::Bottom => {
            (output_h - state.config.bar.height) as f64
        }
    };

    let texture_buffer = match TextureBuffer::from_memory(
        renderer,
        data,
        Fourcc::Abgr8888,
        Size::from((*w as i32, *h as i32)),
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

    vec![TextureRenderElement::from_texture_buffer(
        Point::from((0.0, bar_y)),
        &texture_buffer,
        None,
        None,
        None,
        Kind::Unspecified,
    )]
}

/// Rasterize border pixmaps (CPU-side) — only when layout has changed.
fn rasterize_borders(state: &mut Gate) {
    if !state.layout_dirty {
        return;
    }

    let border_px = state.config.general.border_px;
    let accent = state.config.theme.accent;
    let border_color = state.config.theme.border;
    let radius = state.config.general.border_radius as f32;

    state.border_cache.clear();

    for &(rect, is_focused) in &state.border_rects {
        let color = if is_focused { accent } else { border_color };
        let w = rect.size.w.max(1) as u32;
        let h = rect.size.h.max(1) as u32;

        let r = ((color >> 16) & 0xFF) as u8;
        let g = ((color >> 8) & 0xFF) as u8;
        let b = (color & 0xFF) as u8;

        let mut pixmap = match tiny_skia::Pixmap::new(w, h) {
            Some(p) => p,
            None => {
                state.border_cache.push((Vec::new(), 0, 0));
                continue;
            }
        };

        let paint = tiny_skia::Paint {
            shader: tiny_skia::Shader::SolidColor(tiny_skia::Color::from_rgba8(r, g, b, 255)),
            anti_alias: radius > 0.0,
            ..Default::default()
        };

        if let Some(path) = rounded_rect_path(0.0, 0.0, w as f32, h as f32, radius) {
            pixmap.fill_path(&path, &paint, tiny_skia::FillRule::Winding, tiny_skia::Transform::identity(), None);
        }

        let inner_x = border_px as f32;
        let inner_y = border_px as f32;
        let inner_w = (w as i32 - border_px * 2).max(0) as f32;
        let inner_h = (h as i32 - border_px * 2).max(0) as f32;
        let inner_radius = (radius - border_px as f32).max(0.0);

        if inner_w > 0.0 && inner_h > 0.0 {
            let clear_paint = tiny_skia::Paint {
                shader: tiny_skia::Shader::SolidColor(tiny_skia::Color::from_rgba8(0, 0, 0, 0)),
                blend_mode: tiny_skia::BlendMode::Source,
                anti_alias: inner_radius > 0.0,
                ..Default::default()
            };
            if let Some(path) = rounded_rect_path(inner_x, inner_y, inner_w, inner_h, inner_radius) {
                pixmap.fill_path(&path, &clear_paint, tiny_skia::FillRule::Winding, tiny_skia::Transform::identity(), None);
            }
        }

        state.border_cache.push((pixmap.data().to_vec(), w, h));
    }
}

/// Upload cached border pixmaps to GPU and position for a specific output.
fn render_borders(
    state: &Gate,
    renderer: &mut GlesRenderer,
    output_idx: usize,
) -> Vec<TextureRenderElement<GlesTexture>> {
    let border_px = state.config.general.border_px;
    if border_px <= 0 || state.border_cache.len() != state.border_rects.len() {
        return Vec::new();
    }

    let out = match state.outputs.get(output_idx) {
        Some(o) => o,
        None => return Vec::new(),
    };
    let out_rect = smithay::utils::Rectangle::new(
        out.location,
        (out.size.0, out.size.1).into(),
    );

    let mut elements = Vec::new();

    for (i, &(rect, _is_focused)) in state.border_rects.iter().enumerate() {
        if !out_rect.overlaps(rect) {
            continue;
        }

        let (ref data, w, h) = state.border_cache[i];
        if data.is_empty() {
            continue;
        }

        let texture_buffer = match TextureBuffer::from_memory(
            renderer,
            data,
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

        let (off_x, off_y) = state.border_offsets.get(i).copied().unwrap_or((0.0, 0.0));
        elements.push(TextureRenderElement::from_texture_buffer(
            Point::from((
                (rect.loc.x - out.location.x) as f64 + off_x,
                (rect.loc.y - out.location.y) as f64 + off_y,
            )),
            &texture_buffer,
            None,
            None,
            None,
            Kind::Unspecified,
        ));
    }

    elements
}

/// Build all custom render elements for a specific output (wallpaper + borders + bar).
pub fn build_custom_elements(
    state: &mut Gate,
    renderer: &mut GlesRenderer,
    output_idx: usize,
) -> Vec<TextureRenderElement<GlesTexture>> {
    let mut elements: Vec<TextureRenderElement<GlesTexture>> = Vec::new();

    let has_fullscreen = state.outputs.get(output_idx)
        .map(|o| o.fullscreen_window.is_some())
        .unwrap_or(false);

    // Wallpaper (rendered behind everything, at output-local origin — texture cached)
    if let Some(ref mut wp) = state.wallpaper {
        if let Some(tex_buf) = wp.texture(renderer) {
            elements.push(TextureRenderElement::from_texture_buffer(
                Point::from((0.0, 0.0)),
                tex_buf,
                None,
                None,
                None,
                Kind::Unspecified,
            ));
        }
    }

    // Borders (between wallpaper and windows, hidden during fullscreen)
    // Rasterize only when layout changed (CPU-side caching)
    if !has_fullscreen {
        rasterize_borders(state);
        state.layout_dirty = false;
        elements.extend(render_borders(state, renderer, output_idx));
    }

    // Bar (on top, hidden during fullscreen)
    if !has_fullscreen {
        elements.extend(render_bar(state, renderer, output_idx));
    }

    // Dim overlay for visible scratchpads (behind scratchpad windows, above bar)
    elements.extend(render_dim_overlay(state, renderer, output_idx));

    elements
}

/// Render dim overlay for visible scratchpads on this output.
fn render_dim_overlay(
    state: &Gate,
    renderer: &mut GlesRenderer,
    output_idx: usize,
) -> Vec<TextureRenderElement<GlesTexture>> {
    let max_alpha = state.scratchpads.iter()
        .filter(|sp| sp.visible && sp.output_idx == output_idx)
        .filter_map(|sp| state.config.scratchpads.get(sp.config_idx))
        .map(|sc| sc.dim_alpha)
        .max();

    let alpha = match max_alpha {
        Some(a) if a > 0 => a as u8,
        _ => return Vec::new(),
    };

    let (w, h) = match state.outputs.get(output_idx) {
        Some(o) => o.size,
        None => return Vec::new(),
    };

    // Single-pixel black with alpha, stretched to output size
    let pixel: [u8; 4] = [0, 0, 0, alpha];
    let mut data = vec![0u8; (w * h * 4) as usize];
    for chunk in data.chunks_exact_mut(4) {
        chunk.copy_from_slice(&pixel);
    }

    let texture_buffer = match TextureBuffer::from_memory(
        renderer,
        &data,
        Fourcc::Abgr8888,
        Size::from((w, h)),
        false,
        1,
        Transform::Normal,
        None,
    ) {
        Ok(buf) => buf,
        Err(_) => return Vec::new(),
    };

    vec![TextureRenderElement::from_texture_buffer(
        Point::from((0.0, 0.0)),
        &texture_buffer,
        None,
        None,
        None,
        Kind::Unspecified,
    )]
}

/// Build a rounded rectangle path with quadratic bezier corners.
fn rounded_rect_path(x: f32, y: f32, w: f32, h: f32, r: f32) -> Option<tiny_skia::Path> {
    if r <= 0.0 {
        // No rounding — simple rect
        let mut pb = tiny_skia::PathBuilder::new();
        pb.push_rect(tiny_skia::Rect::from_xywh(x, y, w, h)?);
        return pb.finish();
    }

    let r = r.min(w / 2.0).min(h / 2.0);
    let mut pb = tiny_skia::PathBuilder::new();

    // Top edge (left to right)
    pb.move_to(x + r, y);
    pb.line_to(x + w - r, y);
    pb.quad_to(x + w, y, x + w, y + r);

    // Right edge (top to bottom)
    pb.line_to(x + w, y + h - r);
    pb.quad_to(x + w, y + h, x + w - r, y + h);

    // Bottom edge (right to left)
    pb.line_to(x + r, y + h);
    pb.quad_to(x, y + h, x, y + h - r);

    // Left edge (bottom to top)
    pb.line_to(x, y + r);
    pb.quad_to(x, y, x + r, y);

    pb.close();
    pb.finish()
}
