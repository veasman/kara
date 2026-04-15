use crate::notification::{Notification, Urgency};
use kara_ipc::ThemeColors;
use kara_ui::canvas::{color_from_u32, fill_rounded_rect, stroke_rounded_rect};
use kara_ui::text::TextRenderer;
use tiny_skia::Pixmap;

const CARD_WIDTH: u32 = 380;
const CARD_HEIGHT: u32 = 90;
const GAP: u32 = 8;
const PADDING: f32 = 12.0;
const BORDER_RADIUS: f32 = 8.0;

pub struct NotificationUI {
    text: TextRenderer,
    text_small: TextRenderer,
    theme: ThemeColors,
}

impl NotificationUI {
    pub fn new(theme: ThemeColors) -> Self {
        Self {
            text: TextRenderer::new(14.0),
            text_small: TextRenderer::new(11.0),
            theme,
        }
    }

    /// Live-reload entry point for when kara-beautify pushes a new
    /// theme over IPC. Not wired to the IPC handler yet but part of
    /// the public UI surface.
    #[allow(dead_code)]
    pub fn set_theme(&mut self, theme: ThemeColors) {
        self.theme = theme;
    }

    pub fn total_height(count: usize) -> u32 {
        if count == 0 {
            return 1;
        }
        (count as u32) * CARD_HEIGHT + (count.saturating_sub(1) as u32) * GAP
    }

    pub fn card_width() -> u32 {
        CARD_WIDTH
    }

    pub fn render(&mut self, notifications: &[Notification]) -> Option<Pixmap> {
        if notifications.is_empty() {
            return None;
        }

        let height = Self::total_height(notifications.len());
        let mut pixmap = Pixmap::new(CARD_WIDTH, height)?;

        for (i, notif) in notifications.iter().enumerate() {
            let y_off = (i as u32 * (CARD_HEIGHT + GAP)) as f32;

            // Card background
            fill_rounded_rect(
                &mut pixmap,
                0.0,
                y_off,
                CARD_WIDTH as f32,
                CARD_HEIGHT as f32,
                BORDER_RADIUS,
                color_from_u32(self.theme.surface),
            );

            // Critical: accent border
            if notif.urgency == Urgency::Critical {
                stroke_rounded_rect(
                    &mut pixmap,
                    1.0,
                    y_off + 1.0,
                    CARD_WIDTH as f32 - 2.0,
                    CARD_HEIGHT as f32 - 2.0,
                    BORDER_RADIUS,
                    color_from_u32(self.theme.accent),
                    2.0,
                );
            }

            // Summary text (top)
            self.text.draw(
                &mut pixmap,
                &notif.summary,
                PADDING,
                y_off + PADDING + 14.0,
                self.theme.text,
            );

            // Body text (middle) — truncate if too long
            let body = if notif.body.len() > 60 {
                format!("{}...", &notif.body[..57])
            } else {
                notif.body.clone()
            };
            self.text_small.draw(
                &mut pixmap,
                &body,
                PADDING,
                y_off + PADDING + 34.0,
                self.theme.text_muted,
            );

            // App name (bottom-right, small)
            if !notif.app_name.is_empty() {
                let app_w = self.text_small.measure(&notif.app_name);
                self.text_small.draw(
                    &mut pixmap,
                    &notif.app_name,
                    CARD_WIDTH as f32 - PADDING - app_w as f32,
                    y_off + CARD_HEIGHT as f32 - PADDING - 2.0,
                    self.theme.text_muted,
                );
            }
        }

        Some(pixmap)
    }
}
