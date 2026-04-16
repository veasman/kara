//! Drawing primitives for tiny-skia pixmaps.
//!
//! Rounded rects, circles, filled rects, glyph blitting.

use tiny_skia::{Color, FillRule, Paint, PathBuilder, Pixmap, Rect, Transform};

/// Convert a 0xRRGGBB u32 to a tiny-skia Color (opaque).
pub fn color_from_u32(c: u32) -> Color {
    Color::from_rgba8(
        ((c >> 16) & 0xFF) as u8,
        ((c >> 8) & 0xFF) as u8,
        (c & 0xFF) as u8,
        255,
    )
}

/// Build a rounded rectangle path.
pub fn rounded_rect_path(x: f32, y: f32, w: f32, h: f32, r: f32) -> Option<tiny_skia::Path> {
    if w <= 0.0 || h <= 0.0 {
        return None;
    }

    let r = r.min(w / 2.0).min(h / 2.0).max(0.0);
    let mut pb = PathBuilder::new();

    if r == 0.0 {
        pb.move_to(x, y);
        pb.line_to(x + w, y);
        pb.line_to(x + w, y + h);
        pb.line_to(x, y + h);
        pb.close();
    } else {
        let k = 0.5522848_f32 * r;

        pb.move_to(x + r, y);
        pb.line_to(x + w - r, y);
        pb.cubic_to(x + w - r + k, y, x + w, y + r - k, x + w, y + r);
        pb.line_to(x + w, y + h - r);
        pb.cubic_to(x + w, y + h - r + k, x + w - r + k, y + h, x + w - r, y + h);
        pb.line_to(x + r, y + h);
        pb.cubic_to(x + r - k, y + h, x, y + h - r + k, x, y + h - r);
        pb.line_to(x, y + r);
        pb.cubic_to(x, y + r - k, x + r - k, y, x + r, y);
        pb.close();
    }

    pb.finish()
}

/// Fill a rounded rectangle on a pixmap.
pub fn fill_rounded_rect(pixmap: &mut Pixmap, x: f32, y: f32, w: f32, h: f32, r: f32, color: Color) {
    let mut paint = Paint::default();
    paint.set_color(color);
    paint.anti_alias = true;

    if r > 0.0 {
        if let Some(path) = rounded_rect_path(x, y, w, h, r) {
            pixmap.fill_path(&path, &paint, FillRule::Winding, Transform::identity(), None);
        }
    } else if let Some(rect) = Rect::from_xywh(x, y, w, h) {
        pixmap.fill_rect(rect, &paint, Transform::identity(), None);
    }
}

/// Fill a rounded rectangle with a repeating pattern sourced from
/// another pixmap. Used by kara-sight for pill module backgrounds
/// that tile an SVG-rasterized border pattern, and could be used
/// anywhere a tileable texture replaces a solid fill. The tile
/// pattern origin is anchored to the filled rectangle's top-left.
pub fn fill_rounded_rect_with_pattern(
    pixmap: &mut Pixmap,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    r: f32,
    tile: &Pixmap,
) {
    let pattern_transform = Transform::from_translate(x, y);
    let shader = tiny_skia::Pattern::new(
        tile.as_ref(),
        tiny_skia::SpreadMode::Repeat,
        tiny_skia::FilterQuality::Nearest,
        1.0,
        pattern_transform,
    );
    let paint = Paint {
        shader,
        anti_alias: r > 0.0,
        ..Default::default()
    };
    if r > 0.0 {
        if let Some(path) = rounded_rect_path(x, y, w, h, r) {
            pixmap.fill_path(&path, &paint, FillRule::Winding, Transform::identity(), None);
        }
    } else if let Some(rect) = Rect::from_xywh(x, y, w, h) {
        pixmap.fill_rect(rect, &paint, Transform::identity(), None);
    }
}

/// Stroke a rounded rectangle outline with a repeating pattern.
pub fn stroke_rounded_rect_with_pattern(
    pixmap: &mut Pixmap,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    r: f32,
    tile: &Pixmap,
    stroke_width: f32,
) {
    let pattern_transform = Transform::from_translate(x, y);
    let shader = tiny_skia::Pattern::new(
        tile.as_ref(),
        tiny_skia::SpreadMode::Repeat,
        tiny_skia::FilterQuality::Nearest,
        1.0,
        pattern_transform,
    );
    let paint = Paint {
        shader,
        anti_alias: r > 0.0,
        ..Default::default()
    };
    let stroke = tiny_skia::Stroke {
        width: stroke_width,
        ..Default::default()
    };
    if let Some(path) = rounded_rect_path(x, y, w, h, r) {
        pixmap.stroke_path(&path, &paint, &stroke, Transform::identity(), None);
    }
}

/// Stroke a rounded rectangle border on a pixmap.
pub fn stroke_rounded_rect(pixmap: &mut Pixmap, x: f32, y: f32, w: f32, h: f32, r: f32, color: Color, width: f32) {
    let mut paint = Paint::default();
    paint.set_color(color);
    paint.anti_alias = true;

    let stroke = tiny_skia::Stroke {
        width,
        ..Default::default()
    };

    if let Some(path) = rounded_rect_path(x, y, w, h, r) {
        pixmap.stroke_path(&path, &paint, &stroke, Transform::identity(), None);
    }
}

/// Fill a circle on a pixmap.
pub fn fill_circle(pixmap: &mut Pixmap, cx: f32, cy: f32, radius: f32, color: Color) {
    let mut paint = Paint::default();
    paint.set_color(color);
    paint.anti_alias = true;

    let mut pb = PathBuilder::new();
    let k = 0.5522848;
    let r = radius;

    pb.move_to(cx + r, cy);
    pb.cubic_to(cx + r, cy + r * k, cx + r * k, cy + r, cx, cy + r);
    pb.cubic_to(cx - r * k, cy + r, cx - r, cy + r * k, cx - r, cy);
    pb.cubic_to(cx - r, cy - r * k, cx - r * k, cy - r, cx, cy - r);
    pb.cubic_to(cx + r * k, cy - r, cx + r, cy - r * k, cx + r, cy);
    pb.close();

    if let Some(path) = pb.finish() {
        pixmap.fill_path(&path, &paint, FillRule::Winding, Transform::identity(), None);
    }
}

/// Blit a grayscale glyph mask onto a pixmap with the given color.
pub fn blit_mask(
    pixmap: &mut Pixmap,
    data: &[u8],
    glyph_w: u32,
    glyph_h: u32,
    gx: i32,
    gy: i32,
    r: u8,
    g: u8,
    b: u8,
) {
    let pm_w = pixmap.width() as i32;
    let pm_h = pixmap.height() as i32;
    let pixels = pixmap.data_mut();

    for row in 0..glyph_h as i32 {
        let py = gy + row;
        if py < 0 || py >= pm_h {
            continue;
        }
        for col in 0..glyph_w as i32 {
            let px = gx + col;
            if px < 0 || px >= pm_w {
                continue;
            }

            let alpha = data[(row as u32 * glyph_w + col as u32) as usize];
            if alpha == 0 {
                continue;
            }

            let idx = ((py * pm_w + px) * 4) as usize;
            if idx + 3 >= pixels.len() {
                continue;
            }

            let a = alpha as u32;
            let inv_a = 255 - a;

            pixels[idx] = ((r as u32 * a + pixels[idx] as u32 * inv_a) / 255) as u8;
            pixels[idx + 1] = ((g as u32 * a + pixels[idx + 1] as u32 * inv_a) / 255) as u8;
            pixels[idx + 2] = ((b as u32 * a + pixels[idx + 2] as u32 * inv_a) / 255) as u8;
            pixels[idx + 3] = ((a * 255 + pixels[idx + 3] as u32 * inv_a) / 255) as u8;
        }
    }
}

/// Blit a color glyph (RGBA) onto a pixmap.
pub fn blit_color(
    pixmap: &mut Pixmap,
    data: &[u8],
    glyph_w: u32,
    glyph_h: u32,
    gx: i32,
    gy: i32,
) {
    let pm_w = pixmap.width() as i32;
    let pm_h = pixmap.height() as i32;
    let pixels = pixmap.data_mut();

    for row in 0..glyph_h as i32 {
        let py = gy + row;
        if py < 0 || py >= pm_h {
            continue;
        }
        for col in 0..glyph_w as i32 {
            let px = gx + col;
            if px < 0 || px >= pm_w {
                continue;
            }

            let src_idx = ((row as u32 * glyph_w + col as u32) * 4) as usize;
            if src_idx + 3 >= data.len() {
                continue;
            }

            let sr = data[src_idx];
            let sg = data[src_idx + 1];
            let sb = data[src_idx + 2];
            let sa = data[src_idx + 3];

            if sa == 0 {
                continue;
            }

            let idx = ((py * pm_w + px) * 4) as usize;
            if idx + 3 >= pixels.len() {
                continue;
            }

            let a = sa as u32;
            let inv_a = 255 - a;

            pixels[idx] = ((sr as u32 * a + pixels[idx] as u32 * inv_a) / 255) as u8;
            pixels[idx + 1] = ((sg as u32 * a + pixels[idx + 1] as u32 * inv_a) / 255) as u8;
            pixels[idx + 2] = ((sb as u32 * a + pixels[idx + 2] as u32 * inv_a) / 255) as u8;
            pixels[idx + 3] = ((a * 255 + pixels[idx + 3] as u32 * inv_a) / 255) as u8;
        }
    }
}
