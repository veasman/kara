use crate::notification::{Notification, Urgency};
use kara_ipc::ThemeColors;
use kara_ui::canvas::{
    color_from_u32, fill_rounded_rect, fill_rounded_rect_with_pattern, stroke_rounded_rect,
};
use kara_ui::text::TextRenderer;
use tiny_skia::Pixmap;

const CARD_WIDTH: u32 = 380;
const CARD_BASE_HEIGHT: u32 = 90;
const CARD_ACTION_ROW: u32 = 28;
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

    pub fn total_height_for(notifications: &[Notification]) -> u32 {
        if notifications.is_empty() {
            return 1;
        }
        let mut h = 0u32;
        for (i, n) in notifications.iter().enumerate() {
            h += Self::card_height(n);
            if i + 1 < notifications.len() {
                h += GAP;
            }
        }
        h
    }

    fn card_height(n: &Notification) -> u32 {
        if n.actions.is_empty() {
            CARD_BASE_HEIGHT
        } else {
            CARD_BASE_HEIGHT + CARD_ACTION_ROW
        }
    }

    pub fn card_width() -> u32 {
        CARD_WIDTH
    }

    pub fn render(&mut self, notifications: &[Notification]) -> Option<Pixmap> {
        if notifications.is_empty() {
            return None;
        }

        let height = Self::total_height_for(notifications);
        let mut pixmap = Pixmap::new(CARD_WIDTH, height)?;

        // Load the theme's window border tile once per render. When
        // present, every notification card draws the same tiled
        // pattern around its chrome that kara-gate draws on real
        // windows — whisper feels like a member of the family.
        let border_tile = self
            .theme
            .border_tile_path
            .as_deref()
            .and_then(|p| Pixmap::load_png(p).ok());
        let theme_border_px = self.theme.border_px.unwrap_or(0).max(0) as f32;
        let theme_border_radius = self
            .theme
            .border_radius
            .map(|r| r as f32)
            .unwrap_or(BORDER_RADIUS);

        let mut y_off = 0.0f32;
        for (_i, notif) in notifications.iter().enumerate() {
            let card_h = Self::card_height(notif) as f32;

            // Theme-driven border chrome — draw the outer tiled
            // border first, then the inner card surface on top.
            // Falls back to a plain surface fill when the theme has
            // no tile set.
            if let (Some(tile_pm), bw) = (&border_tile, theme_border_px) {
                if bw > 0.0 {
                    fill_rounded_rect_with_pattern(
                        &mut pixmap,
                        -bw,
                        y_off - bw,
                        CARD_WIDTH as f32 + bw * 2.0,
                        card_h + bw * 2.0,
                        theme_border_radius + bw,
                        tile_pm,
                    );
                    // Inner card fills on top of the tiled border.
                    fill_rounded_rect(
                        &mut pixmap,
                        0.0,
                        y_off,
                        CARD_WIDTH as f32,
                        card_h,
                        theme_border_radius,
                        color_from_u32(self.theme.surface),
                    );
                } else {
                    fill_rounded_rect(
                        &mut pixmap,
                        0.0,
                        y_off,
                        CARD_WIDTH as f32,
                        card_h,
                        theme_border_radius,
                        color_from_u32(self.theme.surface),
                    );
                }
            } else {
                fill_rounded_rect(
                    &mut pixmap,
                    0.0,
                    y_off,
                    CARD_WIDTH as f32,
                    card_h,
                    BORDER_RADIUS,
                    color_from_u32(self.theme.surface),
                );
            }

            // Critical: accent border
            if notif.urgency == Urgency::Critical {
                stroke_rounded_rect(
                    &mut pixmap,
                    1.0,
                    y_off + 1.0,
                    CARD_WIDTH as f32 - 2.0,
                    card_h - 2.0,
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

            // App name (bottom-right of the base area, small)
            let base_bottom = y_off + CARD_BASE_HEIGHT as f32;
            if !notif.app_name.is_empty() {
                let app_w = self.text_small.measure(&notif.app_name);
                self.text_small.draw(
                    &mut pixmap,
                    &notif.app_name,
                    CARD_WIDTH as f32 - PADDING - app_w as f32,
                    base_bottom - PADDING - 2.0,
                    self.theme.text_muted,
                );
            }

            // Action buttons (below the body, if any)
            if !notif.actions.is_empty() {
                let btn_y = base_bottom + 2.0;
                let btn_h = CARD_ACTION_ROW as f32 - 4.0;
                let btn_gap = 6.0f32;
                let n_btns = notif.actions.len() as f32;
                let total_gap = btn_gap * (n_btns - 1.0).max(0.0);
                let avail = CARD_WIDTH as f32 - PADDING * 2.0 - total_gap;
                let btn_w = (avail / n_btns).min(120.0);

                let mut bx = PADDING;
                for (_id, label) in &notif.actions {
                    fill_rounded_rect(
                        &mut pixmap,
                        bx,
                        btn_y,
                        btn_w,
                        btn_h,
                        4.0,
                        color_from_u32(self.theme.accent_soft),
                    );
                    let label_w = self.text_small.measure(label) as f32;
                    let label_x = bx + (btn_w - label_w) / 2.0;
                    let label_y = self.text_small.center_y_offset(btn_y + btn_h / 2.0);
                    self.text_small.draw(
                        &mut pixmap,
                        label,
                        label_x,
                        label_y,
                        self.theme.text,
                    );
                    bx += btn_w + btn_gap;
                }
            }

            y_off += card_h + GAP as f32;
        }

        Some(pixmap)
    }
}
