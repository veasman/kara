use kara_ipc::ThemeColors;
use kara_ui::canvas::{color_from_u32, fill_rounded_rect_with_pattern, stroke_rounded_rect};
use tiny_skia::Pixmap;

/// Render the overlay into a caller-owned pixmap. Avoids per-frame
/// allocation — the caller pre-allocates once and hands in a &mut ref.
/// Returns `false` if the pixmap dimensions are zero.
pub fn render_overlay(
    pixmap: &mut Pixmap,
    width: u32,
    height: u32,
    highlight: (i32, i32, i32, i32),
    theme: &ThemeColors,
    tile_cache: Option<&Pixmap>,
) -> bool {
    if width == 0 || height == 0 {
        return false;
    }
    // Fast clear — memset 0 then fill dim. Much cheaper than
    // allocating a fresh pixmap every frame.
    pixmap.data_mut().fill(0);

    let (hx, hy, hw, hh) = highlight;
    // Fullscreen hover (pointer over empty desktop): the "highlight"
    // equals the entire overlay, so the normal dim-plus-outline
    // treatment would wrap a heavy frame around the whole screen and
    // hide whatever's on it. Instead, show a gentle uniform dim so
    // the user can still see their content and still reads a
    // selection-preview signal. Skip the border, clear, and glow
    // passes entirely in this case.
    let is_fullscreen =
        hx <= 0 && hy <= 0 && hw >= width as i32 && hh >= height as i32;
    if is_fullscreen {
        pixmap.fill(tiny_skia::Color::from_rgba8(0, 0, 0, 50));
        return true;
    }

    // Windowed hover / drag selection: dim everything, then cut a
    // clean transparent window where the selection lies and frame it.
    let dim = tiny_skia::Color::from_rgba8(0, 0, 0, 128);
    pixmap.fill(dim);

    let hx = hx.max(0) as u32;
    let hy = hy.max(0) as u32;
    let hw = (hw as i32).min((width as i32 - hx as i32).max(0)) as u32;
    let hh = (hh as i32).min((height as i32 - hy as i32).max(0)) as u32;

    let border_px = theme.border_px.unwrap_or(2).max(1) as f32;
    let radius = theme.border_radius.unwrap_or(0) as f32;
    let tile = tile_cache;

    // Draw the theme-driven border FIRST (so the inner clear can
    // punch through the pattern as well as the dim fill in one
    // pass). If the active theme supplies an SVG tile, paint the
    // border area with the tiled pattern exactly as kara-gate does.
    // Otherwise fall back to a flat accent stroke with the theme
    // border width.
    if let Some(tile_pm) = tile {
        let ox = hx as f32 - border_px;
        let oy = hy as f32 - border_px;
        let ow = hw as f32 + border_px * 2.0;
        let oh = hh as f32 + border_px * 2.0;
        fill_rounded_rect_with_pattern(
            pixmap,
            ox,
            oy,
            ow,
            oh,
            (radius + border_px).max(0.0),
            &tile_pm,
        );
    } else {
        stroke_rounded_rect(
            pixmap,
            hx as f32,
            hy as f32,
            hw as f32,
            hh as f32,
            radius,
            color_from_u32(theme.accent),
            border_px,
        );
    }

    // Clear the inner highlight rect to transparent so the user
    // sees whatever is underneath (original screen content).
    // Raw pixel write because fill_rect blends with the
    // semi-transparent dim layer instead of replacing it.
    let data = pixmap.data_mut();
    for row in hy..hy.saturating_add(hh).min(height) {
        for col in hx..hx.saturating_add(hw).min(width) {
            let idx = ((row * width + col) * 4) as usize;
            data[idx] = 0;
            data[idx + 1] = 0;
            data[idx + 2] = 0;
            data[idx + 3] = 0;
        }
    }

    // Draw a dark red glow at the inner edge of the selection.
    // Multiple concentric strokes with decreasing alpha simulate
    // a soft radiance that reads against the dark dim overlay
    // without the harshness of a single bright stroke.
    // Dark glow effect using accent_soft (the theme's muted accent
    // variant). Multiple concentric strokes with increasing opacity
    // from outside in. Wider outer strokes + more layers = softer,
    // more visible glow. The glow sits outside the transparent
    // cutout (on the dim overlay side) so it acts as a clear
    // separator without being harsh.
    let glow_color = theme.accent_soft;
    let gr = ((glow_color >> 16) & 0xFF) as u8;
    let gg = ((glow_color >> 8) & 0xFF) as u8;
    let gb = (glow_color & 0xFF) as u8;
    let glow_steps: &[(f32, f32, u8)] = &[
        // (inset from highlight edge, stroke width, alpha)
        (6.0, 3.0, 25),   // outermost haze
        (4.0, 2.5, 50),   // outer glow
        (2.5, 2.0, 80),   // mid glow
        (1.0, 1.5, 130),  // inner glow
        (0.0, 1.0, 200),  // core edge
    ];
    for &(inset, sw, alpha) in glow_steps {
        let gx = hx as f32 + inset;
        let gy = hy as f32 + inset;
        let gw = hw as f32 - inset * 2.0;
        let gh = hh as f32 - inset * 2.0;
        if gw > 0.0 && gh > 0.0 {
            stroke_rounded_rect(
                pixmap,
                gx,
                gy,
                gw,
                gh,
                (radius - inset).max(0.0),
                tiny_skia::Color::from_rgba8(gr, gg, gb, alpha),
                sw,
            );
        }
    }

    true
}
