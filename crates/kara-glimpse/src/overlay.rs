use kara_ipc::ThemeColors;
use kara_ui::canvas::{color_from_u32, stroke_rounded_rect};
use tiny_skia::Pixmap;

pub fn render_overlay(
    width: u32,
    height: u32,
    highlight: (i32, i32, i32, i32),
    theme: &ThemeColors,
) -> Option<Pixmap> {
    let mut pixmap = Pixmap::new(width, height)?;

    // Fill with semi-transparent dark overlay
    let dim = tiny_skia::Color::from_rgba8(0, 0, 0, 128);
    pixmap.fill(dim);

    // Clear the highlight region to transparent (creates bright cutout)
    let (hx, hy, hw, hh) = highlight;
    let hx = hx.max(0) as u32;
    let hy = hy.max(0) as u32;
    let hw = (hw as i32).min((width as i32 - hx as i32).max(0)) as u32;
    let hh = (hh as i32).min((height as i32 - hy as i32).max(0)) as u32;

    // Clear pixels in highlight rect to transparent
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

    // Draw accent border around highlight region (2px)
    stroke_rounded_rect(
        &mut pixmap,
        hx as f32,
        hy as f32,
        hw as f32,
        hh as f32,
        0.0,
        color_from_u32(theme.accent),
        2.0,
    );

    Some(pixmap)
}
