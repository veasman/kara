//! Launcher UI rendering — draws the search bar and results list to a Pixmap.

use kara_ipc::ThemeColors;
use kara_ui::canvas::{color_from_u32, fill_rounded_rect, stroke_rounded_rect};
use kara_ui::TextRenderer;
use tiny_skia::Pixmap;

use crate::desktop::DesktopEntry;

const PADDING: i32 = 16;
const SEARCH_HEIGHT: i32 = 44;
const ITEM_HEIGHT: i32 = 36;
const BORDER_RADIUS: f32 = 12.0;
const MAX_VISIBLE: usize = 10;

pub struct LauncherUI {
    pub text: TextRenderer,
    pub theme: ThemeColors,
    pub width: u32,
    pub height: u32,
}

impl LauncherUI {
    pub fn new(theme: ThemeColors, width: u32, height: u32, font: &str, font_size: f32) -> Self {
        Self {
            text: TextRenderer::new_with_font(font, font_size),
            theme,
            width,
            height,
        }
    }

    /// Render the launcher UI and return pixel data + dimensions.
    pub fn render(
        &mut self,
        query: &str,
        entries: &[DesktopEntry],
        filtered: &[usize],
        selected: usize,
        scroll_offset: usize,
        show_command_fallback: bool,
    ) -> Option<Pixmap> {
        let w = self.width;
        let h = self.height;
        let mut pixmap = Pixmap::new(w, h)?;

        let t = &self.theme;

        // Background
        fill_rounded_rect(&mut pixmap, 0.0, 0.0, w as f32, h as f32, BORDER_RADIUS, color_from_u32(t.surface));
        stroke_rounded_rect(&mut pixmap, 0.5, 0.5, w as f32 - 1.0, h as f32 - 1.0, BORDER_RADIUS, color_from_u32(t.border), 1.0);

        // Search bar background
        let search_y = PADDING;
        let search_w = w as i32 - PADDING * 2;
        fill_rounded_rect(
            &mut pixmap,
            PADDING as f32, search_y as f32,
            search_w as f32, SEARCH_HEIGHT as f32,
            8.0, color_from_u32(t.bg),
        );

        // Search prompt + query text
        let text_x = PADDING + 12;
        let text_y = self.text.center_y_offset(search_y as f32 + SEARCH_HEIGHT as f32 / 2.0);

        let prompt = if query.is_empty() { "Search..." } else { "" };
        if !prompt.is_empty() {
            self.text.draw(&mut pixmap, prompt, text_x as f32, text_y, t.text_muted);
        }
        if !query.is_empty() {
            self.text.draw(&mut pixmap, query, text_x as f32, text_y, t.text);
            // Cursor
            let cursor_x = text_x as f32 + self.text.measure(query) as f32 + 2.0;
            let cursor_y = search_y as f32 + 8.0;
            let cursor_h = SEARCH_HEIGHT as f32 - 16.0;
            fill_rounded_rect(&mut pixmap, cursor_x, cursor_y, 2.0, cursor_h, 1.0, color_from_u32(t.accent));
        }

        // Separator
        let sep_y = search_y + SEARCH_HEIGHT + 8;
        fill_rounded_rect(
            &mut pixmap,
            PADDING as f32, sep_y as f32,
            search_w as f32, 1.0,
            0.0, color_from_u32(t.border),
        );

        // Results list
        let list_y = sep_y + 8;
        let visible_count = filtered.len().min(MAX_VISIBLE);

        for (vi, &idx) in filtered.iter().skip(scroll_offset).take(MAX_VISIBLE).enumerate() {
            let item_y = list_y + (vi as i32 * ITEM_HEIGHT);
            let is_selected = scroll_offset + vi == selected;

            // Selection highlight
            if is_selected {
                fill_rounded_rect(
                    &mut pixmap,
                    PADDING as f32, item_y as f32,
                    search_w as f32, ITEM_HEIGHT as f32,
                    6.0, color_from_u32(t.accent_soft),
                );
            }

            let entry = &entries[idx];
            let name_x = PADDING + 12;
            let name_y = self.text.center_y_offset(item_y as f32 + ITEM_HEIGHT as f32 / 2.0);
            let name_color = if is_selected { t.text } else { t.text };
            self.text.draw(&mut pixmap, &entry.name, name_x as f32, name_y, name_color);

            // Comment on the right (dimmed)
            if let Some(ref comment) = entry.comment {
                let truncated = if comment.len() > 40 {
                    format!("{}...", &comment[..37])
                } else {
                    comment.clone()
                };
                let comment_w = self.text.measure(&truncated) as i32;
                let comment_x = w as i32 - PADDING - 12 - comment_w;
                self.text.draw(&mut pixmap, &truncated, comment_x as f32, name_y, t.text_muted);
            }
        }

        // Command fallback at bottom
        if show_command_fallback && !query.is_empty() {
            let fb_y = list_y + (visible_count as i32 * ITEM_HEIGHT);
            let is_selected = selected >= filtered.len();

            if is_selected {
                fill_rounded_rect(
                    &mut pixmap,
                    PADDING as f32, fb_y as f32,
                    search_w as f32, ITEM_HEIGHT as f32,
                    6.0, color_from_u32(t.accent_soft),
                );
            }

            let run_text = format!("Run: {query}");
            let name_y = self.text.center_y_offset(fb_y as f32 + ITEM_HEIGHT as f32 / 2.0);
            self.text.draw(&mut pixmap, &run_text, (PADDING + 12) as f32, name_y, t.accent);
        }

        Some(pixmap)
    }
}

