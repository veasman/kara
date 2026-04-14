//! Shared render helpers for bar and border textures.
//!
//! Used by both winit and udev backends to build custom render elements.

use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::element::texture::{TextureBuffer, TextureRenderElement};
use smithay::backend::renderer::element::Kind;
use smithay::utils::{Point, Size, Transform};

use crate::backend_udev::{KaraRenderer, KaraTexture};
use crate::state::Gate;

/// Render the bar to a texture for a specific output.
/// CPU-side rasterization is cached and only redone when bar_dirty is set.
pub fn render_bar(
    state: &mut Gate,
    renderer: &mut KaraRenderer<'_>,
    output_idx: usize,
) -> Vec<TextureRenderElement<KaraTexture>> {
    if !state.config.bar.enabled {
        return Vec::new();
    }

    let (output_w, output_h) = state.outputs.get(output_idx)
        .map(|o| o.size)
        .unwrap_or((800, 600));

    // When the bar dirty flag flips on, drop ALL per-output caches so every
    // bar gets re-rasterized with fresh status, focus, and workspace state.
    // Then this output's specific entry is rebuilt below if missing.
    if state.bar_dirty {
        state.bar_cache.clear();
        state.bar_dirty = false;
    }

    if !state.bar_cache.contains_key(&output_idx) {
        let ws_ctx = state.bar_workspace_context(output_idx);
        if let Some(pixmap) = state.bar_renderer.render(
            output_w as u32,
            &state.config.bar,
            &state.config.theme,
            &state.status_cache,
            &ws_ctx,
        ) {
            state.bar_cache.insert(
                output_idx,
                (pixmap.data().to_vec(), pixmap.width(), pixmap.height()),
            );
        }
    }

    let (data, w, h) = match state.bar_cache.get(&output_idx) {
        Some(c) => c,
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
    renderer: &mut KaraRenderer<'_>,
) -> Vec<TextureRenderElement<KaraTexture>> {
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
    renderer: &mut KaraRenderer<'_>,
    output_idx: usize,
) -> Vec<TextureRenderElement<KaraTexture>> {
    let mut elements: Vec<TextureRenderElement<KaraTexture>> = Vec::new();

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
    renderer: &mut KaraRenderer<'_>,
    output_idx: usize,
) -> Vec<TextureRenderElement<KaraTexture>> {
    render_dim_overlay(state, renderer, output_idx)
}

/// Build scratchpad border elements. Renders IN FRONT of space windows.
pub fn build_scratchpad_borders(
    state: &mut Gate,
    renderer: &mut KaraRenderer<'_>,
    output_idx: usize,
) -> Vec<TextureRenderElement<KaraTexture>> {
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
    renderer: &mut KaraRenderer<'_>,
    x: i32, y: i32, w: i32, h: i32,
    alpha: u8,
) -> Option<TextureRenderElement<KaraTexture>> {
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
    renderer: &mut KaraRenderer<'_>,
    output_idx: usize,
) -> Vec<TextureRenderElement<KaraTexture>> {
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

    // sp_rect is in GLOBAL compositor coordinates (workarea.loc + offset).
    // make_dim_rect interprets its position as OUTPUT-LOCAL — the same
    // convention the bar uses, where Point::from((0, 0)) means the top-left
    // of each output. So we subtract the output's location to convert the
    // hole rect into output-local space before slicing the four dim bars
    // around it. On a single-monitor setup output.location is (0,0) and
    // this is a no-op; on multi-monitor it was previously offsetting the
    // hole by the output's global x and pulling the dim rects across the
    // scratchpad windows.
    let (gsx, gsy, sw, sh) = sp_rect.unwrap_or((out.location.x, out.location.y, ow, oh));
    let sx = gsx - out.location.x;
    let sy = gsy - out.location.y;

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
    renderer: &mut KaraRenderer<'_>,
    output_idx: usize,
) -> Vec<TextureRenderElement<KaraTexture>> {
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

    // Build a grouped, human-readable keybind table.
    //
    // Goal: this overlay is the entrypoint for the "kara helpers" vision — a
    // user with a fresh install should be able to hit mod+/ once and immediately
    // understand what every key does. Keep the rendering simple but the labels
    // friendly.
    let groups = build_keybind_groups(&state.keybinds);

    // Columns: if many keybinds, split across two columns so the list fits
    // without scrolling. Pick 2 columns when total rows > half-screen.
    let max_rows_per_col = ((h as f32 - 100.0) / line_height).max(4.0) as usize;
    let total_rows: usize = groups.iter().map(|g| g.1.len() + 2).sum::<usize>();
    let use_two_cols = total_rows > max_rows_per_col;

    // Render each group into a flat list of (text, is_header, is_title) tuples.
    let mut lines: Vec<(String, LineKind)> = Vec::new();
    lines.push(("Keybinds".to_string(), LineKind::Title));
    lines.push(("~/.config/kara/kara-gate.conf".to_string(), LineKind::Subtitle));
    lines.push((String::new(), LineKind::Body));

    for (group_name, entries) in &groups {
        lines.push(((*group_name).to_string(), LineKind::Section));
        for (combo, label) in entries {
            lines.push((format!("{:<22} {}", combo, label), LineKind::Body));
        }
        lines.push((String::new(), LineKind::Body));
    }

    // Precompute colors for the four line kinds. Accent for the title, a
    // slightly brighter text color for section headers, muted for the config
    // path subtitle, and normal text for body rows.
    let text_rgb = split_rgb(state.config.theme.text);
    let accent_rgb = split_rgb(state.config.theme.accent);
    let muted_rgb = split_rgb(state.config.theme.text_muted);

    // Two-column layout: split lines at the first blank line after row
    // max_rows_per_col. Column 2 starts halfway across the screen.
    let col1_x = 60.0_f32;
    let col2_x = if use_two_cols { (w as f32 / 2.0) + 20.0 } else { 0.0 };
    let start_y = 60.0_f32;
    let col_split = if use_two_cols {
        // Pick the split point at the first blank line past mid.
        let mut split = lines.len();
        let mut row = 0usize;
        for (idx, (text, _)) in lines.iter().enumerate() {
            if row >= max_rows_per_col && text.is_empty() {
                split = idx + 1;
                break;
            }
            row += 1;
        }
        split
    } else {
        lines.len()
    };

    for (i, (line, kind)) in lines.iter().enumerate() {
        if line.is_empty() {
            continue;
        }

        let (col_x, row_in_col) = if i < col_split {
            (col1_x, i)
        } else {
            (col2_x, i - col_split)
        };

        let y = start_y + (row_in_col as f32) * line_height;
        if y + font_size > h as f32 {
            continue;
        }

        let (r, g, b) = match kind {
            LineKind::Title => accent_rgb,
            LineKind::Subtitle => muted_rgb,
            LineKind::Section => accent_rgb,
            LineKind::Body => text_rgb,
        };

        // Title is larger; section headers are slightly bigger than body.
        let line_metrics = match kind {
            LineKind::Title => cosmic_text::Metrics::new(font_size * 1.6, font_size * 1.6),
            LineKind::Section => cosmic_text::Metrics::new(font_size * 1.1, font_size * 1.1),
            _ => metrics,
        };

        let attrs = cosmic_text::Attrs::new().family(cosmic_text::Family::Name(font_family));
        let mut buffer = cosmic_text::Buffer::new(&mut font_system, line_metrics);
        buffer.set_text(&mut font_system, line, &attrs, cosmic_text::Shaping::Advanced, None);
        buffer.shape_until_scroll(&mut font_system, false);

        for run in buffer.layout_runs() {
            for glyph in run.glyphs.iter() {
                let physical = glyph.physical((col_x, y), 1.0);
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

// ── Keybind overlay helpers ────────────────────────────────────────────

#[derive(Copy, Clone)]
enum LineKind {
    Title,
    Subtitle,
    Section,
    Body,
}

fn split_rgb(color: u32) -> (u8, u8, u8) {
    (
        ((color >> 16) & 0xFF) as u8,
        ((color >> 8) & 0xFF) as u8,
        (color & 0xFF) as u8,
    )
}

/// Group keybinds by action category into an ordered list of
/// (section_title, [(combo_string, action_label)]) entries.
fn build_keybind_groups(
    keybinds: &[crate::input::Keybind],
) -> Vec<(&'static str, Vec<(String, String)>)> {
    use crate::actions::Action;

    let mut windows: Vec<(String, String)> = Vec::new();
    let mut workspaces: Vec<(String, String)> = Vec::new();
    let mut scratchpads: Vec<(String, String)> = Vec::new();
    let mut layout: Vec<(String, String)> = Vec::new();
    let mut monitors: Vec<(String, String)> = Vec::new();
    let mut launch: Vec<(String, String)> = Vec::new();
    let mut session: Vec<(String, String)> = Vec::new();

    for bind in keybinds.iter() {
        let combo = format_keybind_combo(bind);
        let label = format_action_label(&bind.action);

        let bucket: &mut Vec<(String, String)> = match &bind.action {
            Action::FocusNext
            | Action::FocusPrev
            | Action::KillClient
            | Action::ToggleFloat
            | Action::ToggleFullscreen => &mut windows,
            Action::ViewWs(_) | Action::SendWs(_) => &mut workspaces,
            Action::ToggleScratchpad(_) => &mut scratchpads,
            Action::ZoomMaster
            | Action::ToggleMonocle
            | Action::DecreaseMfact
            | Action::IncreaseMfact => &mut layout,
            Action::FocusMonitorNext
            | Action::FocusMonitorPrev
            | Action::SendMonitorNext
            | Action::SendMonitorPrev
            | Action::ToggleSync => &mut monitors,
            Action::Spawn(_) | Action::SpawnRaw(_) => &mut launch,
            Action::ShowKeybinds | Action::Reload | Action::Quit => &mut session,
        };

        bucket.push((combo, label));
    }

    let mut groups: Vec<(&'static str, Vec<(String, String)>)> = Vec::new();
    if !windows.is_empty()    { groups.push(("Windows", windows)); }
    if !workspaces.is_empty() { groups.push(("Workspaces", workspaces)); }
    if !scratchpads.is_empty(){ groups.push(("Scratchpads", scratchpads)); }
    if !layout.is_empty()     { groups.push(("Layout", layout)); }
    if !monitors.is_empty()   { groups.push(("Monitors", monitors)); }
    if !launch.is_empty()     { groups.push(("Launch", launch)); }
    if !session.is_empty()    { groups.push(("Session", session)); }
    groups
}

fn format_keybind_combo(bind: &crate::input::Keybind) -> String {
    let mut combo = String::new();
    if bind.mods.logo  { combo.push_str("mod+"); }
    if bind.mods.ctrl  { combo.push_str("Ctrl+"); }
    if bind.mods.alt   { combo.push_str("Alt+"); }
    if bind.mods.shift { combo.push_str("Shift+"); }

    let raw = xkbcommon::xkb::keysym_get_name(xkbcommon::xkb::Keysym::new(bind.sym));
    combo.push_str(pretty_key_name(&raw));
    combo
}

fn pretty_key_name(raw: &str) -> &str {
    match raw {
        "slash" => "/",
        "backslash" => "\\",
        "comma" => ",",
        "period" => ".",
        "semicolon" => ";",
        "apostrophe" => "'",
        "grave" => "`",
        "minus" => "-",
        "equal" => "=",
        "bracketleft" => "[",
        "bracketright" => "]",
        "Return" => "Enter",
        "space" => "Space",
        "Escape" => "Esc",
        "BackSpace" => "Backspace",
        "Prior" => "PageUp",
        "Next" => "PageDown",
        _ => raw,
    }
}

fn format_action_label(action: &crate::actions::Action) -> String {
    use crate::actions::Action;
    match action {
        Action::Spawn(name) => format!("Launch: {name}"),
        Action::SpawnRaw(cmd) => {
            let short = if cmd.len() > 28 { format!("{}…", &cmd[..27]) } else { cmd.clone() };
            format!("Run: {short}")
        }
        Action::KillClient       => "Close window".into(),
        Action::FocusNext        => "Focus next window".into(),
        Action::FocusPrev        => "Focus previous window".into(),
        Action::ZoomMaster       => "Zoom / swap with master".into(),
        Action::ToggleMonocle    => "Toggle monocle layout".into(),
        Action::ToggleFullscreen => "Toggle fullscreen".into(),
        Action::ToggleFloat      => "Toggle floating".into(),
        Action::ToggleScratchpad(Some(name)) => format!("Toggle scratchpad: {name}"),
        Action::ToggleScratchpad(None) => "Toggle scratchpad".into(),
        Action::DecreaseMfact    => "Shrink master".into(),
        Action::IncreaseMfact    => "Grow master".into(),
        Action::FocusMonitorNext => "Focus next monitor".into(),
        Action::FocusMonitorPrev => "Focus previous monitor".into(),
        Action::SendMonitorNext  => "Move window to next monitor".into(),
        Action::SendMonitorPrev  => "Move window to previous monitor".into(),
        Action::ToggleSync       => "Toggle monitor sync".into(),
        Action::ViewWs(n)        => format!("View workspace {}", n + 1),
        Action::SendWs(n)        => format!("Move window to workspace {}", n + 1),
        Action::ShowKeybinds     => "Show keybinds (this menu)".into(),
        Action::Reload           => "Reload config".into(),
        Action::Quit             => "Quit kara".into(),
    }
}
