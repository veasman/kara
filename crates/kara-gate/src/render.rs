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
        state.bar_texture_cache.clear();
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

    // Reuse cached GPU texture when pixel data hasn't changed.
    // Only re-upload on bar_dirty (cache was cleared above).
    if !state.bar_texture_cache.contains_key(&output_idx) {
        match TextureBuffer::from_memory(
            renderer,
            data,
            Fourcc::Abgr8888,
            Size::from((*w as i32, *h as i32)),
            false,
            1,
            Transform::Normal,
            None,
        ) {
            Ok(buf) => { state.bar_texture_cache.insert(output_idx, buf); }
            Err(e) => {
                tracing::error!("failed to upload bar texture: {e:?}");
                return Vec::new();
            }
        }
    }

    let texture_buffer = match state.bar_texture_cache.get(&output_idx) {
        Some(tb) => tb,
        None => return Vec::new(),
    };

    vec![TextureRenderElement::from_texture_buffer(
        Point::from((0.0, bar_y)),
        texture_buffer,
        None,
        None,
        None,
        Kind::Unspecified,
    )]
}

/// Ensure `state.border_tile_pixmap` reflects the current
/// `config.general.border_tile` path. Re-decodes the PNG when the
/// path changes (or is set for the first time), clears the cache
/// when the path is cleared. Called before every border rasterize
/// pass so a theme apply is picked up on the next frame.
fn refresh_border_tile_cache(state: &mut Gate) {
    let want = state.config.general.border_tile.as_ref();
    let have = state.border_tile_pixmap.as_ref().map(|(p, _)| p.as_path());
    match (want, have) {
        (None, None) => {}
        (None, Some(_)) => {
            state.border_tile_pixmap = None;
        }
        (Some(want_path), Some(have_path)) if want_path == have_path => {}
        (Some(want_path), _) => {
            state.border_tile_pixmap = load_border_tile_pixmap(want_path)
                .map(|pm| (want_path.clone(), pm));
            // Force a re-rasterize so the new pattern shows on the
            // next frame even if layout hasn't otherwise changed.
            state.layout_dirty = true;
            state.scratchpad_layout_dirty = true;
        }
    }
}

/// Load a PNG into a tiny_skia Pixmap for use as a border tile
/// pattern. Returns `None` on any failure (missing file, decode
/// error) — kara-gate falls back to solid-color border rendering.
pub fn load_border_tile_pixmap(path: &std::path::Path) -> Option<tiny_skia::Pixmap> {
    let img = match image::open(path) {
        Ok(i) => i.to_rgba8(),
        Err(e) => {
            tracing::warn!("border_tile: failed to decode {}: {e}", path.display());
            return None;
        }
    };
    let (w, h) = (img.width(), img.height());
    let mut pixmap = tiny_skia::Pixmap::new(w, h)?;
    // Copy the image bytes in. tiny-skia wants premultiplied RGBA;
    // `image` gives straight RGBA. Premultiply per pixel.
    let dst = pixmap.data_mut();
    for (i, px) in img.pixels().enumerate() {
        let [r, g, b, a] = px.0;
        let af = a as f32 / 255.0;
        let base = i * 4;
        dst[base]     = (r as f32 * af).round() as u8;
        dst[base + 1] = (g as f32 * af).round() as u8;
        dst[base + 2] = (b as f32 * af).round() as u8;
        dst[base + 3] = a;
    }
    Some(pixmap)
}

/// Four CPU-side pixmap strips that together form one window's border
/// ring. Replaces the old "full window-sized pixmap, 99% transparent"
/// model. Strip sizes: top/bottom = rect.w × border_px, left/right =
/// border_px × (rect.h - 2·border_px). Any strip may be empty when
/// the rect is degenerate.
#[derive(Default)]
pub struct BorderStrips {
    pub top: (Vec<u8>, u32, u32),
    pub bottom: (Vec<u8>, u32, u32),
    pub left: (Vec<u8>, u32, u32),
    pub right: (Vec<u8>, u32, u32),
}

/// GPU-side counterpart to `BorderStrips` — holds up to four uploaded
/// `TextureBuffer`s, one per strip. `None` entries mean "no upload yet"
/// or "no strip needed" (empty pixels).
#[derive(Default)]
pub struct BorderStripTextures {
    pub top: Option<TextureBuffer<KaraTexture>>,
    pub bottom: Option<TextureBuffer<KaraTexture>>,
    pub left: Option<TextureBuffer<KaraTexture>>,
    pub right: Option<TextureBuffer<KaraTexture>>,
}

/// Rasterize one strip: a w × h pixmap filled with the theme's border
/// paint (solid color or tile pattern) and, when the window is
/// focused and we have a tile, brightened with an accent overlay so
/// the tile's focus state reads. Returns `(bytes, w, h)` — an empty
/// tuple when the strip is degenerate.
fn rasterize_strip(
    w: u32,
    h: u32,
    color_rgba: (u8, u8, u8, u8),
    tile: Option<&tiny_skia::Pixmap>,
    tile_offset: (f32, f32),
    accent_overlay: Option<(u8, u8, u8, u8)>,
) -> (Vec<u8>, u32, u32) {
    if w == 0 || h == 0 {
        return (Vec::new(), 0, 0);
    }
    let mut pixmap = match tiny_skia::Pixmap::new(w, h) {
        Some(p) => p,
        None => return (Vec::new(), 0, 0),
    };
    let rect = match tiny_skia::Rect::from_xywh(0.0, 0.0, w as f32, h as f32) {
        Some(r) => r,
        None => return (Vec::new(), 0, 0),
    };
    let paint = if let Some(tile_pm) = tile {
        // Translate the pattern so each strip samples the theme's tile
        // from a continuous virtual grid — otherwise adjacent strips
        // would re-start the tile at (0,0) and break the pattern at
        // the window's corners.
        tiny_skia::Paint {
            shader: tiny_skia::Pattern::new(
                tile_pm.as_ref(),
                tiny_skia::SpreadMode::Repeat,
                tiny_skia::FilterQuality::Nearest,
                1.0,
                tiny_skia::Transform::from_translate(-tile_offset.0, -tile_offset.1),
            ),
            anti_alias: false,
            ..Default::default()
        }
    } else {
        let (r, g, b, a) = color_rgba;
        tiny_skia::Paint {
            shader: tiny_skia::Shader::SolidColor(tiny_skia::Color::from_rgba8(r, g, b, a)),
            anti_alias: false,
            ..Default::default()
        }
    };
    pixmap.fill_rect(rect, &paint, tiny_skia::Transform::identity(), None);

    if let Some((ar, ag, ab, aa)) = accent_overlay {
        let overlay = tiny_skia::Paint {
            shader: tiny_skia::Shader::SolidColor(tiny_skia::Color::from_rgba8(ar, ag, ab, aa)),
            blend_mode: tiny_skia::BlendMode::Plus,
            anti_alias: false,
            ..Default::default()
        };
        pixmap.fill_rect(rect, &overlay, tiny_skia::Transform::identity(), None);
    }

    (pixmap.data().to_vec(), w, h)
}

/// Rasterize a set of border pixmaps (CPU-side) — only when dirty.
fn rasterize_border_set(
    rects: &[(smithay::utils::Rectangle<i32, smithay::utils::Logical>, bool)],
    cache: &mut Vec<BorderStrips>,
    dirty: bool,
    border_px: i32,
    _radius: f32,
    accent: u32,
    border_color: u32,
    tile: Option<&tiny_skia::Pixmap>,
) {
    if !dirty {
        return;
    }

    cache.clear();
    cache.reserve_exact(rects.len());

    for &(rect, is_focused) in rects {
        let color = if is_focused { accent } else { border_color };
        let w = rect.size.w.max(1) as u32;
        let h = rect.size.h.max(1) as u32;
        let bp = border_px.max(0) as u32;

        if bp == 0 || w == 0 || h == 0 {
            cache.push(BorderStrips::default());
            continue;
        }

        let color_rgba = (
            ((color >> 16) & 0xFF) as u8,
            ((color >> 8) & 0xFF) as u8,
            (color & 0xFF) as u8,
            255u8,
        );
        let accent_overlay = if is_focused && tile.is_some() {
            Some((
                ((accent >> 16) & 0xFF) as u8,
                ((accent >> 8) & 0xFF) as u8,
                (accent & 0xFF) as u8,
                55u8,
            ))
        } else {
            None
        };

        let mid_h = h.saturating_sub(bp * 2);
        // Strip placements relative to the border rect origin (0, 0):
        //   top    at (0, 0)             size (w, bp)
        //   bottom at (0, h - bp)        size (w, bp)
        //   left   at (0, bp)            size (bp, mid_h)
        //   right  at (w - bp, bp)       size (bp, mid_h)
        // Each strip samples the theme tile with an offset equal to
        // its placement so the pattern stays continuous across the
        // ring instead of restarting per strip.
        let top = rasterize_strip(w, bp, color_rgba, tile, (0.0, 0.0), accent_overlay);
        let bottom = rasterize_strip(
            w,
            bp,
            color_rgba,
            tile,
            (0.0, (h - bp) as f32),
            accent_overlay,
        );
        let left = if mid_h > 0 {
            rasterize_strip(bp, mid_h, color_rgba, tile, (0.0, bp as f32), accent_overlay)
        } else {
            (Vec::new(), 0, 0)
        };
        let right = if mid_h > 0 {
            rasterize_strip(
                bp,
                mid_h,
                color_rgba,
                tile,
                ((w - bp) as f32, bp as f32),
                accent_overlay,
            )
        } else {
            (Vec::new(), 0, 0)
        };

        cache.push(BorderStrips { top, bottom, left, right });
    }
}

/// Upload or reuse a single strip as a GPU TextureBuffer and push a
/// render element positioned at `(base_x + dx, base_y + dy)` in
/// output-local coords. Returns the (possibly newly allocated)
/// TextureBuffer so the caller stores it back in its cache slot.
fn upload_and_place_strip(
    data: &(Vec<u8>, u32, u32),
    texture: &mut Option<TextureBuffer<KaraTexture>>,
    base_x: f64,
    base_y: f64,
    dx: f64,
    dy: f64,
    renderer: &mut KaraRenderer<'_>,
    out_elements: &mut Vec<TextureRenderElement<KaraTexture>>,
) {
    let (ref bytes, w, h) = *data;
    if bytes.is_empty() || w == 0 || h == 0 {
        return;
    }
    if texture.is_none() {
        match TextureBuffer::from_memory(
            renderer,
            bytes,
            Fourcc::Abgr8888,
            Size::from((w as i32, h as i32)),
            false,
            1,
            Transform::Normal,
            None,
        ) {
            Ok(buf) => *texture = Some(buf),
            Err(e) => {
                tracing::error!("failed to upload border strip texture: {e:?}");
                return;
            }
        }
    }
    if let Some(tb) = texture.as_ref() {
        out_elements.push(TextureRenderElement::from_texture_buffer(
            Point::from((base_x + dx, base_y + dy)),
            tb,
            None,
            None,
            None,
            Kind::Unspecified,
        ));
    }
}

/// Upload cached border strips to GPU and position them around each
/// window. Positions are derived from the window's actual render
/// location in the Space so we stay in sync with surface commits.
fn render_border_set(
    rects: &[(smithay::utils::Rectangle<i32, smithay::utils::Logical>, bool)],
    cache: &[BorderStrips],
    texture_cache: &mut Vec<BorderStripTextures>,
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
    texture_cache.resize_with(cache.len(), BorderStripTextures::default);

    let out = match output {
        Some(o) => o,
        None => return Vec::new(),
    };

    let mut elements = Vec::new();

    for (i, &(rect, _)) in rects.iter().enumerate() {
        let strips = &cache[i];
        let textures = &mut texture_cache[i];

        let border_loc = if let Some((window, _base)) = windows.get(i) {
            if let Some(map_loc) = space.element_location(window) {
                (map_loc.x - border_px, map_loc.y - border_px)
            } else {
                (rect.loc.x, rect.loc.y)
            }
        } else {
            (rect.loc.x, rect.loc.y)
        };
        let (off_x, off_y) = offsets.get(i).copied().unwrap_or((0.0, 0.0));
        let base_x = (border_loc.0 - out.location.x) as f64 + off_x;
        let base_y = (border_loc.1 - out.location.y) as f64 + off_y;

        let bp = border_px.max(0) as f64;
        let rh = rect.size.h as f64;

        // top strip at (0, 0)
        upload_and_place_strip(
            &strips.top, &mut textures.top,
            base_x, base_y, 0.0, 0.0,
            renderer, &mut elements,
        );
        // bottom strip at (0, h - bp)
        upload_and_place_strip(
            &strips.bottom, &mut textures.bottom,
            base_x, base_y, 0.0, rh - bp,
            renderer, &mut elements,
        );
        // left strip at (0, bp)
        upload_and_place_strip(
            &strips.left, &mut textures.left,
            base_x, base_y, 0.0, bp,
            renderer, &mut elements,
        );
        // right strip at (w - bp, bp)
        upload_and_place_strip(
            &strips.right, &mut textures.right,
            base_x, base_y, rect.size.w as f64 - bp, bp,
            renderer, &mut elements,
        );
    }

    elements
}

/// Build all custom render elements for a specific output (bar + borders + wallpaper).
///
/// Smithay's element vec is front-to-back: index 0 is topmost, index
/// N-1 is bottommost. Painting happens last-to-first, so the last
/// vec entry is drawn before the first. Inside the `custom_elements`
/// block (which sits at the END of the overall frame vec), we must
/// push **in z-order from top to bottom**: bar first, borders
/// next, wallpaper LAST — so the wallpaper is painted first, then
/// borders over it, then the bar over them. Reversing this order
/// leaves the wallpaper painting OVER the bar, hiding it.
/// Compute an aspect-preserving center-crop source rect for
/// drawing a wallpaper onto an output.
///
/// Given a source image of size `(src_w, src_h)` and a destination
/// output of size `(dst_w, dst_h)`, picks a sub-rectangle of the
/// source that:
///   1. Has the same aspect ratio as the destination
///   2. Is as large as possible while still fitting inside the source
///   3. Is centered on the source
///
/// When `from_texture_buffer` gets that `src` plus a `size` of the
/// destination, the image covers the output fully (no letterbox or
/// pillarbox) without stretching — the edges of a too-tall source
/// are cropped equally top/bottom, too-wide source is cropped
/// equally left/right.
///
/// Returns a Rectangle in buffer-pixel coordinates.
fn center_crop_src_rect(
    src_w: u32,
    src_h: u32,
    dst_w: i32,
    dst_h: i32,
) -> smithay::utils::Rectangle<f64, smithay::utils::Logical> {
    use smithay::utils::{Point, Rectangle, Size};

    if src_w == 0 || src_h == 0 || dst_w <= 0 || dst_h <= 0 {
        // Degenerate input — fall back to "use the whole source".
        // Avoids divide-by-zero and keeps the render path robust.
        return Rectangle::new(
            Point::from((0.0f64, 0.0f64)),
            Size::from((src_w as f64, src_h as f64)),
        );
    }

    let src_ratio = src_w as f64 / src_h as f64;
    let dst_ratio = dst_w as f64 / dst_h as f64;

    let (crop_w, crop_h) = if src_ratio > dst_ratio {
        // Source is wider than destination — crop left/right.
        let h = src_h as f64;
        let w = h * dst_ratio;
        (w, h)
    } else {
        // Source is taller than destination — crop top/bottom.
        let w = src_w as f64;
        let h = w / dst_ratio;
        (w, h)
    };

    let offset_x = (src_w as f64 - crop_w) / 2.0;
    let offset_y = (src_h as f64 - crop_h) / 2.0;

    Rectangle::new(
        Point::from((offset_x, offset_y)),
        Size::from((crop_w, crop_h)),
    )
}

pub fn build_custom_elements(
    state: &mut Gate,
    renderer: &mut KaraRenderer<'_>,
    output_idx: usize,
) -> Vec<TextureRenderElement<KaraTexture>> {
    let mut elements: Vec<TextureRenderElement<KaraTexture>> = Vec::new();

    let has_fullscreen = state.outputs.get(output_idx)
        .map(|o| o.fullscreen_window.is_some())
        .unwrap_or(false);

    // Bar (on top of the custom block, hidden during fullscreen).
    if !has_fullscreen {
        elements.extend(render_bar(state, renderer, output_idx));
    }

    // Bar blur backdrop — semi-transparent bar over a blurred crop
    // of the wallpaper. CPU-side box blur on the bar-sized region.
    // Sits behind the bar but above the wallpaper, so the bar's
    // background_alpha creates the frosted glass look.
    if !has_fullscreen && state.config.bar.enabled && state.config.bar.blur {
        if let Some(blurred) = render_bar_blur(state, renderer, output_idx) {
            elements.push(blurred);
        }
    }

    // Workspace borders (behind the bar, in front of the wallpaper).
    if !has_fullscreen {
        refresh_border_tile_cache(state);
        let tile = state.border_tile_pixmap.as_ref().map(|(_, pm)| pm);
        if state.layout_dirty {
            state.border_texture_cache.clear();
        }
        rasterize_border_set(
            &state.border_rects, &mut state.border_cache, state.layout_dirty,
            state.config.general.border_px, state.config.general.border_radius as f32,
            state.config.theme.accent, state.config.theme.border,
            tile,
        );
        state.layout_dirty = false;
        elements.extend(render_border_set(
            &state.border_rects, &state.border_cache,
            &mut state.border_texture_cache,
            &state.border_offsets,
            &state.window_base_positions, &state.space, state.config.general.border_px,
            state.outputs.get(output_idx), renderer,
        ));
    }

    // Wallpaper (absolute bottom — pushed last so it lands at the end
    // of the custom block, and therefore at the end of the overall
    // frame vec, making it the first thing painted and thus the
    // bottommost layer on screen).
    //
    // D1.1: aspect-preserving center-crop. Computes a source
    // sub-rect that matches the output's aspect ratio, centered
    // on the image, and lets smithay scale it to the output's
    // logical size. The source-rect-plus-dest-size combo in
    // TextureRenderElement::from_texture_buffer handles the crop
    // + scale in one pass without an intermediate bitmap.
    if let Some(ref mut wp) = state.wallpaper {
        if let Some(output_state) = state.outputs.get(output_idx) {
            let (out_w, out_h) = output_state.size;
            let (src_w, src_h) = wp.dimensions();

            if let Some(tex_buf) = wp.texture(renderer) {
                let src_rect = center_crop_src_rect(src_w, src_h, out_w, out_h);

                elements.push(TextureRenderElement::from_texture_buffer(
                    Point::from((0.0, 0.0)),
                    tex_buf,
                    None,
                    Some(src_rect),
                    Some(smithay::utils::Size::from((out_w, out_h))),
                    Kind::Unspecified,
                ));
            }
        }
    }

    elements
}

/// CPU-side bar blur — crop the wallpaper to the bar rect, apply
/// iterated box blur, upload as a TextureRenderElement at the bar
/// position. Sits behind the bar surface (which has background_alpha
/// < 255), creating a frosted glass effect without any GL shaders.
///
/// The blurred pixmap is cached in `state.bar_blur_cache` and only
/// re-rasterized when the bar geometry or wallpaper changes (bar_dirty).
fn render_bar_blur(
    state: &mut Gate,
    renderer: &mut KaraRenderer<'_>,
    output_idx: usize,
) -> Option<TextureRenderElement<KaraTexture>> {
    let output_state = state.outputs.get(output_idx)?;
    let (out_w, out_h) = output_state.size;
    let bar_h = state.config.bar.height;
    let bar_y = match state.config.bar.position {
        kara_config::BarPosition::Top => 0,
        kara_config::BarPosition::Bottom => out_h - bar_h,
    };

    // Re-rasterize on bar_dirty (includes first frame).
    if state.bar_blur_cache.is_none() || state.bar_dirty {
        let wp = state.wallpaper.as_ref()?;
        let (rgba, src_w, src_h) = wp.current_rgba()?;

        // Compute the crop rect on the source image that corresponds
        // to the bar's screen position. Uses the same center-crop
        // logic as the main wallpaper renderer.
        let crop = center_crop_src_rect(src_w, src_h, out_w, out_h);

        // Map bar screen rect → source image rect.
        let bar_src_y = crop.loc.y + (bar_y as f64 * crop.size.h / out_h as f64);
        let bar_src_h = (bar_h as f64 * crop.size.h / out_h as f64).ceil() as u32;
        let bar_src_x = crop.loc.x;
        let bar_src_w = crop.size.w.ceil() as u32;
        let bar_src_y = bar_src_y.floor() as u32;

        // Extract the bar-region pixels from the wallpaper RGBA.
        let bw = bar_src_w.min(src_w) as usize;
        let bh = bar_src_h.min(src_h - bar_src_y) as usize;
        if bw == 0 || bh == 0 {
            return None;
        }
        let mut buf: Vec<u8> = Vec::with_capacity(bw * bh * 4);
        for row in 0..bh {
            let y = (bar_src_y as usize + row).min(src_h as usize - 1);
            let start = (y * src_w as usize + bar_src_x as usize) * 4;
            let end = start + bw * 4;
            if end <= rgba.len() {
                buf.extend_from_slice(&rgba[start..end]);
            }
        }

        // Iterated box blur (3 passes ≈ Gaussian approximation).
        // One scratch vec shared across passes so we don't allocate
        // a fresh full-sized tmp buffer inside `box_blur_rgba` three
        // times per rebuild.
        let mut scratch: Vec<u8> = vec![0u8; buf.len()];
        for _ in 0..3 {
            box_blur_rgba(&mut buf, bw, bh, &mut scratch);
        }

        // Scale to output bar dimensions.
        let mut pixmap = tiny_skia::Pixmap::new(out_w as u32, bar_h as u32)?;
        // Simple nearest-neighbor scale from bw×bh → out_w×bar_h.
        let sx = bw as f64 / out_w as f64;
        let sy = bh as f64 / bar_h as f64;
        let dst = pixmap.data_mut();
        for dy in 0..bar_h as usize {
            for dx in 0..out_w as usize {
                let src_px = ((dy as f64 * sy) as usize).min(bh - 1);
                let src_qx = ((dx as f64 * sx) as usize).min(bw - 1);
                let si = (src_px * bw + src_qx) * 4;
                let di = (dy * out_w as usize + dx) * 4;
                if si + 3 < buf.len() && di + 3 < dst.len() {
                    dst[di] = buf[si];
                    dst[di + 1] = buf[si + 1];
                    dst[di + 2] = buf[si + 2];
                    dst[di + 3] = buf[si + 3];
                }
            }
        }

        state.bar_blur_cache = Some((pixmap.data().to_vec(), out_w as u32, bar_h as u32));
        // Pixel cache rebuilt → drop the stale GPU texture so the
        // next access uploads fresh bytes.
        state.bar_blur_texture = None;
    }

    // Upload the blurred bytes to the GPU once per cache rebuild,
    // then reuse the `TextureBuffer` across every subsequent frame.
    // Before this caching step the bar would reupload its full
    // bar-width-by-height texture on every frame, wasting driver
    // bandwidth for a surface that only changes on wallpaper or bar
    // geometry moves.
    if state.bar_blur_texture.is_none() {
        let (data, w, h) = state.bar_blur_cache.as_ref()?;
        state.bar_blur_texture = TextureBuffer::from_memory(
            renderer,
            data,
            Fourcc::Abgr8888,
            Size::from((*w as i32, *h as i32)),
            false,
            1,
            Transform::Normal,
            None,
        )
        .ok();
    }
    let texture_buffer = state.bar_blur_texture.as_ref()?;

    Some(TextureRenderElement::from_texture_buffer(
        Point::from((0.0, bar_y as f64)),
        texture_buffer,
        None,
        None,
        None,
        Kind::Unspecified,
    ))
}

/// Blur the wallpaper crop that sits under a layer-shell surface
/// (currently the theme picker) and return it as a textured element
/// positioned at the surface's screen rect. Same CPU box-blur pipeline
/// as `render_bar_blur`; caches in `state.picker_blur_cache` and only
/// re-rasterizes when the requested rect changes.
///
/// Returns `None` when there's no wallpaper, the rect is degenerate,
/// or we can't decode a current frame.
pub(crate) fn render_picker_blur(
    state: &mut Gate,
    renderer: &mut KaraRenderer<'_>,
    rect: smithay::utils::Rectangle<i32, smithay::utils::Logical>,
) -> Option<TextureRenderElement<KaraTexture>> {
    use smithay::backend::allocator::Fourcc;
    use smithay::utils::{Size, Transform};

    let rx = rect.loc.x;
    let ry = rect.loc.y;
    let rw = rect.size.w.max(1);
    let rh = rect.size.h.max(1);

    // Find the output this rect sits on so we can center-crop the
    // wallpaper the same way the main wallpaper renderer does.
    let output = state.outputs.iter().find(|o| {
        rx >= o.location.x
            && ry >= o.location.y
            && rx < o.location.x + o.size.0
            && ry < o.location.y + o.size.1
    })?;
    let (out_w, out_h) = output.size;
    let out_x = output.location.x;
    let out_y = output.location.y;

    let cache_matches = state
        .picker_blur_cache
        .as_ref()
        .map(|(_, w, h, x, y)| *w == rw as u32 && *h == rh as u32 && *x == rx && *y == ry)
        .unwrap_or(false);

    if !cache_matches {
        let wp = state.wallpaper.as_ref()?;
        let (rgba, src_w, src_h) = wp.current_rgba()?;
        let crop = center_crop_src_rect(src_w, src_h, out_w, out_h);

        // Map the rect's output-local Y range into the wallpaper crop.
        let local_x = (rx - out_x).max(0) as f64;
        let local_y = (ry - out_y).max(0) as f64;
        let sub_src_x = crop.loc.x + (local_x * crop.size.w / out_w as f64);
        let sub_src_y = crop.loc.y + (local_y * crop.size.h / out_h as f64);
        let sub_src_w = (rw as f64 * crop.size.w / out_w as f64).ceil() as u32;
        let sub_src_h = (rh as f64 * crop.size.h / out_h as f64).ceil() as u32;
        let sub_src_x = sub_src_x.floor() as u32;
        let sub_src_y = sub_src_y.floor() as u32;

        let bw = sub_src_w.min(src_w.saturating_sub(sub_src_x)) as usize;
        let bh = sub_src_h.min(src_h.saturating_sub(sub_src_y)) as usize;
        if bw == 0 || bh == 0 {
            return None;
        }

        let mut buf: Vec<u8> = Vec::with_capacity(bw * bh * 4);
        for row in 0..bh {
            let y = (sub_src_y as usize + row).min(src_h as usize - 1);
            let start = (y * src_w as usize + sub_src_x as usize) * 4;
            let end = start + bw * 4;
            if end <= rgba.len() {
                buf.extend_from_slice(&rgba[start..end]);
            }
        }
        // Iterated box blur (3 passes ≈ Gaussian approximation) —
        // same recipe as the bar's blur so the two surfaces look
        // like one frosted pane. Single scratch buffer shared across
        // the three passes.
        let mut scratch: Vec<u8> = vec![0u8; buf.len()];
        for _ in 0..3 {
            box_blur_rgba(&mut buf, bw, bh, &mut scratch);
        }

        let mut pixmap = tiny_skia::Pixmap::new(rw as u32, rh as u32)?;
        let sx = bw as f64 / rw as f64;
        let sy = bh as f64 / rh as f64;
        let dst = pixmap.data_mut();
        for dy in 0..rh as usize {
            for dx in 0..rw as usize {
                let src_py = ((dy as f64 * sy) as usize).min(bh - 1);
                let src_px = ((dx as f64 * sx) as usize).min(bw - 1);
                let si = (src_py * bw + src_px) * 4;
                let di = (dy * rw as usize + dx) * 4;
                if si + 3 < buf.len() && di + 3 < dst.len() {
                    dst[di] = buf[si];
                    dst[di + 1] = buf[si + 1];
                    dst[di + 2] = buf[si + 2];
                    dst[di + 3] = buf[si + 3];
                }
            }
        }
        state.picker_blur_cache = Some((
            pixmap.data().to_vec(),
            rw as u32,
            rh as u32,
            rx,
            ry,
        ));
        // Pixel cache rebuilt → drop the stale GPU texture so the
        // next access uploads fresh bytes.
        state.picker_blur_texture = None;
    }

    // Upload once per cache rebuild, reuse the TextureBuffer across
    // every subsequent frame the picker is open. Before this we were
    // reuploading the full picker-rect blurred texture to the GPU on
    // every compositor frame while the picker was visible.
    if state.picker_blur_texture.is_none() {
        let (data, w, h, _, _) = state.picker_blur_cache.as_ref()?;
        state.picker_blur_texture = TextureBuffer::from_memory(
            renderer,
            data,
            Fourcc::Abgr8888,
            Size::from((*w as i32, *h as i32)),
            false,
            1,
            Transform::Normal,
            None,
        )
        .ok();
    }
    let texture_buffer = state.picker_blur_texture.as_ref()?;

    // Position is output-local (the caller is operating in output
    // coordinates for its frame). Subtract the output origin here.
    Some(TextureRenderElement::from_texture_buffer(
        Point::from(((rx - out_x) as f64, (ry - out_y) as f64)),
        texture_buffer,
        None,
        None,
        None,
        Kind::Unspecified,
    ))
}

/// Two-pass separable box blur on an RGBA buffer. Callers pass in a
/// `scratch` slice of the same length as `buf` so iterated passes can
/// share a single temporary — the old signature allocated a fresh
/// `Vec<u8>` sized to the image inside every call, which cost three
/// full-rect allocations per blur rebuild.
fn box_blur_rgba(buf: &mut [u8], w: usize, h: usize, scratch: &mut [u8]) {
    let radius = 5usize;
    let diam = radius * 2 + 1;
    debug_assert_eq!(scratch.len(), buf.len());
    let tmp = scratch;

    // Horizontal pass → tmp
    for y in 0..h {
        let row = y * w * 4;
        for x in 0..w {
            let mut r = 0u32;
            let mut g = 0u32;
            let mut b = 0u32;
            let mut a = 0u32;
            for dx in 0..diam {
                let sx = (x + dx).saturating_sub(radius).min(w - 1);
                let i = row + sx * 4;
                r += buf[i] as u32;
                g += buf[i + 1] as u32;
                b += buf[i + 2] as u32;
                a += buf[i + 3] as u32;
            }
            let i = row + x * 4;
            tmp[i] = (r / diam as u32) as u8;
            tmp[i + 1] = (g / diam as u32) as u8;
            tmp[i + 2] = (b / diam as u32) as u8;
            tmp[i + 3] = (a / diam as u32) as u8;
        }
    }

    // Vertical pass → buf
    for x in 0..w {
        for y in 0..h {
            let mut r = 0u32;
            let mut g = 0u32;
            let mut b = 0u32;
            let mut a = 0u32;
            for dy in 0..diam {
                let sy = (y + dy).saturating_sub(radius).min(h - 1);
                let i = (sy * w + x) * 4;
                r += tmp[i] as u32;
                g += tmp[i + 1] as u32;
                b += tmp[i + 2] as u32;
                a += tmp[i + 3] as u32;
            }
            let i = (y * w + x) * 4;
            buf[i] = (r / diam as u32) as u8;
            buf[i + 1] = (g / diam as u32) as u8;
            buf[i + 2] = (b / diam as u32) as u8;
            buf[i + 3] = (a / diam as u32) as u8;
        }
    }
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
        refresh_border_tile_cache(state);
        let tile = state.border_tile_pixmap.as_ref().map(|(_, pm)| pm);
        if state.scratchpad_layout_dirty {
            state.scratchpad_border_texture_cache.clear();
        }
        rasterize_border_set(
            &state.scratchpad_border_rects, &mut state.scratchpad_border_cache,
            state.scratchpad_layout_dirty,
            state.config.general.border_px, state.config.general.border_radius as f32,
            state.config.theme.accent, state.config.theme.border,
            tile,
        );
        state.scratchpad_layout_dirty = false;
        let sp_window_offset = state.border_rects.len();
        let sp_windows: Vec<_> = state.window_base_positions.get(sp_window_offset..).unwrap_or(&[]).to_vec();
        elements.extend(render_border_set(
            &state.scratchpad_border_rects, &state.scratchpad_border_cache,
            &mut state.scratchpad_border_texture_cache,
            &state.scratchpad_border_offsets,
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

/// Render a full-screen dim rect for a visible scratchpad.
///
/// With the caller in render_frame splitting space elements into a workspace
/// group (below dim) and a scratchpad group (above dim), the dim just needs
/// to cover the whole output. The scratchpad windows draw on top, so the
/// in-scratchpad gaps between tiled scratchpad windows now show dim (the
/// workspace is dimmed through them) instead of showing unaltered workspace
/// content through a pre-cut hole.
fn render_dim_overlay(
    state: &Gate,
    renderer: &mut KaraRenderer<'_>,
    output_idx: usize,
) -> Vec<TextureRenderElement<KaraTexture>> {
    // Pick the highest dim alpha among visible scratchpads on this output.
    let mut best_alpha = 0i32;
    for sp in &state.scratchpads {
        if !sp.visible || sp.output_idx != output_idx {
            continue;
        }
        if let Some(sc) = state.config.scratchpads.get(sp.config_idx) {
            if sc.dim_alpha > best_alpha {
                best_alpha = sc.dim_alpha;
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

    // One full-screen output-local rect. Partitioning space elements and
    // ordering the dim between workspace and scratchpad elements in
    // render_frame is what keeps scratchpad windows visible on top.
    let mut elements = Vec::new();
    if let Some(e) = make_dim_rect(renderer, 0, 0, ow, oh, alpha) {
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
