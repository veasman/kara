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

/// Rasterize a set of border pixmaps (CPU-side) — only when dirty.
fn rasterize_border_set(
    rects: &[(smithay::utils::Rectangle<i32, smithay::utils::Logical>, bool)],
    cache: &mut Vec<(Vec<u8>, u32, u32)>,
    dirty: bool,
    border_px: i32,
    radius: f32,
    accent: u32,
    border_color: u32,
) {
    if !dirty {
        return;
    }

    cache.clear();

    for &(rect, is_focused) in rects {
        let color = if is_focused { accent } else { border_color };
        let w = rect.size.w.max(1) as u32;
        let h = rect.size.h.max(1) as u32;

        let r = ((color >> 16) & 0xFF) as u8;
        let g = ((color >> 8) & 0xFF) as u8;
        let b = (color & 0xFF) as u8;

        let mut pixmap = match tiny_skia::Pixmap::new(w, h) {
            Some(p) => p,
            None => {
                cache.push((Vec::new(), 0, 0));
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

        cache.push((pixmap.data().to_vec(), w, h));
    }
}

/// Upload cached border pixmaps to GPU and position for a specific output.
/// Positions are derived from the window's actual render location in the Space,
/// not from cached layout rects, to stay in sync after surface commits.
fn render_border_set(
    rects: &[(smithay::utils::Rectangle<i32, smithay::utils::Logical>, bool)],
    cache: &[(Vec<u8>, u32, u32)],
    offsets: &[(f64, f64)],
    windows: &[(smithay::desktop::Window, smithay::utils::Point<i32, smithay::utils::Logical>)],
    space: &smithay::desktop::Space<smithay::desktop::Window>,
    border_px: i32,
    output: Option<&crate::state::OutputState>,
    renderer: &mut GlesRenderer,
) -> Vec<TextureRenderElement<GlesTexture>> {
    if cache.len() != rects.len() {
        return Vec::new();
    }

    let out = match output {
        Some(o) => o,
        None => return Vec::new(),
    };

    let mut elements = Vec::new();

    for (i, &(rect, _)) in rects.iter().enumerate() {
        let (ref data, w, h) = cache[i];
        if data.is_empty() {
            continue;
        }

        // Border sits just outside the window's *geometry* rect. smithay's
        // `Space::element_location` returns the geometry top-left, which is
        // already the correct reference for the visible window — we do NOT
        // subtract `geo.loc` here (that would offset to the buffer origin and
        // leave the border stranded in the CSD shadow margin for clients like
        // Firefox/Floorp that draw their own shadows outside the geometry).
        let border_loc = if let Some((window, _base)) = windows.get(i) {
            if let Some(map_loc) = space.element_location(window) {
                (map_loc.x - border_px, map_loc.y - border_px)
            } else {
                (rect.loc.x, rect.loc.y)
            }
        } else {
            (rect.loc.x, rect.loc.y)
        };

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

        let (off_x, off_y) = offsets.get(i).copied().unwrap_or((0.0, 0.0));
        elements.push(TextureRenderElement::from_texture_buffer(
            Point::from((
                (border_loc.0 - out.location.x) as f64 + off_x,
                (border_loc.1 - out.location.y) as f64 + off_y,
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

    // Workspace borders (behind dim overlay)
    if !has_fullscreen {
        rasterize_border_set(
            &state.border_rects, &mut state.border_cache, state.layout_dirty,
            state.config.general.border_px, state.config.general.border_radius as f32,
            state.config.theme.accent, state.config.theme.border,
        );
        state.layout_dirty = false;
        elements.extend(render_border_set(
            &state.border_rects, &state.border_cache, &state.border_offsets,
            &state.window_base_positions, &state.space, state.config.general.border_px,
            state.outputs.get(output_idx), renderer,
        ));
    }

    // Bar (on top, hidden during fullscreen)
    if !has_fullscreen {
        elements.extend(render_bar(state, renderer, output_idx));
    }

    elements
}

/// Build dim overlay for visible scratchpads. Renders BEHIND space windows.
pub fn build_scratchpad_dim(
    state: &Gate,
    renderer: &mut GlesRenderer,
    output_idx: usize,
) -> Vec<TextureRenderElement<GlesTexture>> {
    render_dim_overlay(state, renderer, output_idx)
}

/// Build scratchpad border elements. Renders IN FRONT of space windows.
pub fn build_scratchpad_borders(
    state: &mut Gate,
    renderer: &mut GlesRenderer,
    output_idx: usize,
) -> Vec<TextureRenderElement<GlesTexture>> {
    let mut elements = Vec::new();
    let has_fullscreen = state.outputs.get(output_idx)
        .map(|o| o.fullscreen_window.is_some())
        .unwrap_or(false);
    if !has_fullscreen {
        rasterize_border_set(
            &state.scratchpad_border_rects, &mut state.scratchpad_border_cache,
            state.scratchpad_layout_dirty,
            state.config.general.border_px, state.config.general.border_radius as f32,
            state.config.theme.accent, state.config.theme.border,
        );
        state.scratchpad_layout_dirty = false;
        // Scratchpad borders — window_base_positions has scratchpad windows after regular ones
        let sp_window_offset = state.border_rects.len();
        let sp_windows: Vec<_> = state.window_base_positions.get(sp_window_offset..).unwrap_or(&[]).to_vec();
        elements.extend(render_border_set(
            &state.scratchpad_border_rects, &state.scratchpad_border_cache, &state.scratchpad_border_offsets,
            &sp_windows, &state.space, state.config.general.border_px,
            state.outputs.get(output_idx), renderer,
        ));
    }

    elements
}

/// Helper: create a dim rect texture at a position.
fn make_dim_rect(
    renderer: &mut GlesRenderer,
    x: i32, y: i32, w: i32, h: i32,
    alpha: u8,
) -> Option<TextureRenderElement<GlesTexture>> {
    if w <= 0 || h <= 0 {
        return None;
    }
    let pixel: [u8; 4] = [0, 0, 0, alpha];
    let mut data = vec![0u8; (w * h * 4) as usize];
    for chunk in data.chunks_exact_mut(4) {
        chunk.copy_from_slice(&pixel);
    }
    let texture_buffer = TextureBuffer::from_memory(
        renderer, &data, Fourcc::Abgr8888,
        Size::from((w, h)), false, 1, Transform::Normal, None,
    ).ok()?;
    Some(TextureRenderElement::from_texture_buffer(
        Point::from((x as f64, y as f64)),
        &texture_buffer, None, None, None, Kind::Unspecified,
    ))
}

/// Render dim overlay as four rects AROUND the scratchpad area.
/// This dims the background without affecting scratchpad window content.
fn render_dim_overlay(
    state: &Gate,
    renderer: &mut GlesRenderer,
    output_idx: usize,
) -> Vec<TextureRenderElement<GlesTexture>> {
    // Find the visible scratchpad on this output with highest dim
    let mut best_alpha = 0i32;
    let mut sp_rect: Option<(i32, i32, i32, i32)> = None;

    for sp in &state.scratchpads {
        if !sp.visible || sp.output_idx != output_idx {
            continue;
        }
        if let Some(sc) = state.config.scratchpads.get(sp.config_idx) {
            if sc.dim_alpha > best_alpha {
                best_alpha = sc.dim_alpha;
                let workarea = state.outputs.get(sp.output_idx)
                    .map(|o| o.workarea)
                    .unwrap_or_else(|| smithay::utils::Rectangle::new((0, 0).into(), (800, 600).into()));
                let sw = (workarea.size.w as f32 * sc.width_pct as f32 / 100.0) as i32;
                let sh = (workarea.size.h as f32 * sc.height_pct as f32 / 100.0) as i32;
                let sx = workarea.loc.x + (workarea.size.w - sw) / 2;
                let sy = workarea.loc.y + (workarea.size.h - sh) / 2;
                sp_rect = Some((sx, sy, sw, sh));
            }
        }
    }

    let alpha = match best_alpha {
        a if a > 0 => a as u8,
        _ => return Vec::new(),
    };

    let out = match state.outputs.get(output_idx) {
        Some(o) => o,
        None => return Vec::new(),
    };
    let ow = out.size.0;
    let oh = out.size.1;

    let (sx, sy, sw, sh) = sp_rect.unwrap_or((0, 0, ow, oh));

    // Four rects around the scratchpad hole (output-local coords)
    let mut elements = Vec::new();

    // Top bar (full width, from top to scratchpad top)
    if let Some(e) = make_dim_rect(renderer, 0, 0, ow, sy, alpha) {
        elements.push(e);
    }
    // Bottom bar (full width, from scratchpad bottom to output bottom)
    if let Some(e) = make_dim_rect(renderer, 0, sy + sh, ow, oh - sy - sh, alpha) {
        elements.push(e);
    }
    // Left bar (scratchpad height, from left edge to scratchpad left)
    if let Some(e) = make_dim_rect(renderer, 0, sy, sx, sh, alpha) {
        elements.push(e);
    }
    // Right bar (scratchpad height, from scratchpad right to right edge)
    if let Some(e) = make_dim_rect(renderer, sx + sw, sy, ow - sx - sw, sh, alpha) {
        elements.push(e);
    }

    elements
}


/// Build the keybind overlay texture when visible.
pub fn build_keybind_overlay(
    state: &Gate,
    renderer: &mut GlesRenderer,
    output_idx: usize,
) -> Vec<TextureRenderElement<GlesTexture>> {
    if !state.keybind_overlay_visible {
        return Vec::new();
    }

    let (output_w, output_h) = state.outputs.get(output_idx)
        .map(|o| o.size)
        .unwrap_or((800, 600));

    let w = output_w as u32;
    let h = output_h as u32;

    let mut pixmap = match tiny_skia::Pixmap::new(w, h) {
        Some(p) => p,
        None => return Vec::new(),
    };

    // Semi-transparent black background
    let bg_pixel: [u8; 4] = [0, 0, 0, 200];
    for chunk in pixmap.data_mut().chunks_exact_mut(4) {
        chunk.copy_from_slice(&bg_pixel);
    }

    // Render keybind text using cosmic-text
    let font_size = state.config.general.font_size.max(14.0);
    let line_height = font_size * 1.6;
    let metrics = cosmic_text::Metrics::new(font_size, font_size);
    let mut font_system = cosmic_text::FontSystem::new();
    let mut swash_cache = cosmic_text::SwashCache::new();

    let font_family = if state.config.general.font.is_empty() {
        "monospace"
    } else {
        &state.config.general.font
    };

    // Build keybind lines
    let mut lines: Vec<String> = Vec::new();
    lines.push(format!("Keybinds  (~/.config/kara/kara-gate.conf)"));
    lines.push(String::new()); // blank separator

    for bind in state.keybinds.iter() {
        let mut combo = String::new();
        if bind.mods.logo { combo.push_str("mod+"); }
        if bind.mods.shift { combo.push_str("Shift+"); }
        if bind.mods.ctrl { combo.push_str("Ctrl+"); }
        if bind.mods.alt { combo.push_str("Alt+"); }

        let key_name = xkbcommon::xkb::keysym_get_name(
            xkbcommon::xkb::Keysym::new(bind.sym),
        );
        combo.push_str(&key_name);

        let action_str = format!("{:?}", bind.action);
        lines.push(format!("{:<30} {}", combo, action_str));
    }

    // Draw text
    let text_color_r = ((state.config.theme.text >> 16) & 0xFF) as u8;
    let text_color_g = ((state.config.theme.text >> 8) & 0xFF) as u8;
    let text_color_b = (state.config.theme.text & 0xFF) as u8;

    let start_x = 40.0_f32;
    let start_y = 40.0_f32;

    for (i, line) in lines.iter().enumerate() {
        if line.is_empty() {
            continue;
        }

        let y = start_y + (i as f32) * line_height;
        if y + font_size > h as f32 {
            break; // don't render past output
        }

        let (r, g, b) = if i == 0 {
            // Title in accent color
            let ar = ((state.config.theme.accent >> 16) & 0xFF) as u8;
            let ag = ((state.config.theme.accent >> 8) & 0xFF) as u8;
            let ab = (state.config.theme.accent & 0xFF) as u8;
            (ar, ag, ab)
        } else {
            (text_color_r, text_color_g, text_color_b)
        };

        let attrs = cosmic_text::Attrs::new().family(cosmic_text::Family::Name(font_family));
        let mut buffer = cosmic_text::Buffer::new(&mut font_system, metrics);
        buffer.set_text(&mut font_system, line, &attrs, cosmic_text::Shaping::Advanced, None);
        buffer.shape_until_scroll(&mut font_system, false);

        for run in buffer.layout_runs() {
            for glyph in run.glyphs.iter() {
                let physical = glyph.physical((start_x, y), 1.0);
                let image = match swash_cache.get_image(&mut font_system, physical.cache_key) {
                    Some(img) => img,
                    None => continue,
                };
                if image.placement.width == 0 || image.placement.height == 0 {
                    continue;
                }
                let gx = physical.x + image.placement.left;
                let gy = physical.y - image.placement.top;

                // Blit glyph onto pixmap
                let gw = image.placement.width as i32;
                let gh = image.placement.height as i32;
                for row in 0..gh {
                    for col in 0..gw {
                        let px = gx + col;
                        let py = gy + row;
                        if px < 0 || py < 0 || px >= w as i32 || py >= h as i32 {
                            continue;
                        }
                        let src_idx = (row * gw + col) as usize;
                        match image.content {
                            cosmic_text::SwashContent::Mask => {
                                if src_idx >= image.data.len() { continue; }
                                let alpha = image.data[src_idx];
                                if alpha == 0 { continue; }
                                let dst_idx = ((py as u32 * w + px as u32) * 4) as usize;
                                let data = pixmap.data_mut();
                                // Alpha blend
                                let a = alpha as u32;
                                let inv_a = 255 - a;
                                data[dst_idx]     = ((r as u32 * a + data[dst_idx] as u32 * inv_a) / 255) as u8;
                                data[dst_idx + 1] = ((g as u32 * a + data[dst_idx + 1] as u32 * inv_a) / 255) as u8;
                                data[dst_idx + 2] = ((b as u32 * a + data[dst_idx + 2] as u32 * inv_a) / 255) as u8;
                                data[dst_idx + 3] = 255;
                            }
                            cosmic_text::SwashContent::Color => {
                                let si = src_idx * 4;
                                if si + 3 >= image.data.len() { continue; }
                                let dst_idx = ((py as u32 * w + px as u32) * 4) as usize;
                                let data = pixmap.data_mut();
                                data[dst_idx]     = image.data[si];
                                data[dst_idx + 1] = image.data[si + 1];
                                data[dst_idx + 2] = image.data[si + 2];
                                data[dst_idx + 3] = image.data[si + 3];
                            }
                            _ => {
                                // SubpixelMask — treat like Mask
                                if src_idx >= image.data.len() { continue; }
                                let alpha = image.data[src_idx];
                                if alpha == 0 { continue; }
                                let dst_idx = ((py as u32 * w + px as u32) * 4) as usize;
                                let data = pixmap.data_mut();
                                let a = alpha as u32;
                                let inv_a = 255 - a;
                                data[dst_idx]     = ((r as u32 * a + data[dst_idx] as u32 * inv_a) / 255) as u8;
                                data[dst_idx + 1] = ((g as u32 * a + data[dst_idx + 1] as u32 * inv_a) / 255) as u8;
                                data[dst_idx + 2] = ((b as u32 * a + data[dst_idx + 2] as u32 * inv_a) / 255) as u8;
                                data[dst_idx + 3] = 255;
                            }
                        }
                    }
                }
            }
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
            tracing::error!("failed to upload keybind overlay texture: {e:?}");
            return Vec::new();
        }
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
