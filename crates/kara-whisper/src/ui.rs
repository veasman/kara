use crate::notification::{Notification, Urgency};
use kara_ipc::ThemeColors;
use kara_ui::canvas::{
    color_from_u32, fill_rounded_rect, stroke_rounded_rect,
};
use kara_ui::text::TextRenderer;
use tiny_skia::Pixmap;

const CARD_WIDTH: u32 = 380;
const CARD_BASE_HEIGHT: u32 = 90;
const CARD_ACTION_ROW: u32 = 28;
const GAP: u32 = 8;
const PADDING: f32 = 14.0;
const CARD_RADIUS: f32 = 10.0;
/// Card background alpha — semi-transparent so wallpaper bleeds
/// through if the layer surface composits over it.
const CARD_ALPHA: u8 = 200;

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
    pub fn set_theme(&mut self, theme: ThemeColors) {
        self.theme = theme;
    }

    /// Current accent color as a fingerprint for "did the palette
    /// change?" polling in the main loop. Cheaper than diffing the
    /// whole ThemeColors struct and moves on every theme swap.
    pub fn accent(&self) -> u32 {
        self.theme.accent
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
            .unwrap_or(CARD_RADIUS);

        let mut y_off = 0.0f32;
        for (_i, notif) in notifications.iter().enumerate() {
            let card_h = Self::card_height(notif) as f32;

            // Card background — semi-transparent surface fill with
            // rounded corners. The alpha lets the wallpaper bleed
            // through subtly, matching the bar's visual language.
            let bg = self.theme.surface;
            let bg_r = ((bg >> 16) & 0xFF) as u8;
            let bg_g = ((bg >> 8) & 0xFF) as u8;
            let bg_b = (bg & 0xFF) as u8;
            // Card radius follows the theme's window border radius so
            // notifications inherit the same silhouette as windows.
            let card_radius = theme_border_radius;
            fill_rounded_rect(
                &mut pixmap,
                0.0,
                y_off,
                CARD_WIDTH as f32,
                card_h,
                card_radius,
                tiny_skia::Color::from_rgba8(bg_r, bg_g, bg_b, CARD_ALPHA),
            );

            // Border chrome — tile pattern when available (matches
            // compositor window borders), accent stroke fallback.
            // Stroke width follows the theme's window border_px so
            // themes with thick ornamental borders read the same on
            // notifications as they do on windows.
            let stroke_w = if theme_border_px > 0.0 { theme_border_px } else { 2.0 };
            let inset = stroke_w * 0.5;
            if let Some(tile_pm) = &border_tile {
                use kara_ui::canvas::stroke_rounded_rect_with_pattern;
                stroke_rounded_rect_with_pattern(
                    &mut pixmap,
                    inset,
                    y_off + inset,
                    CARD_WIDTH as f32 - stroke_w,
                    card_h - stroke_w,
                    (card_radius - inset).max(0.0),
                    tile_pm,
                    stroke_w,
                );
            } else {
                // Accent stroke — gives the card a visible themed edge
                // even without an SVG tile. Critical uses bright accent;
                // normal/low use the muted border color.
                let border_color = if notif.urgency == Urgency::Critical {
                    self.theme.accent
                } else {
                    self.theme.border
                };
                stroke_rounded_rect(
                    &mut pixmap,
                    inset,
                    y_off + inset,
                    CARD_WIDTH as f32 - stroke_w,
                    card_h - stroke_w,
                    (card_radius - inset).max(0.0),
                    color_from_u32(border_color),
                    stroke_w,
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
