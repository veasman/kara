//! Lock-screen rendering.
//!
//! Every output (primary and secondary) gets a semi-opaque dim fill
//! over the compositor-side blurred wallpaper so text pops no matter
//! how bright the desktop behind was. Primary output additionally
//! paints: a big centered clock (pushed off the top edge), a date, a
//! "Locked · hostname" badge, and a translucent login card anchored
//! bottom-right.
//!
//! Typography follows `theme.font_family` (served by kara-gate from
//! `general.font`). Border strokes honour `theme.border_tile_path` so
//! fantasy's ornamental chrome carries through.

use chrono::Local;
use kara_ipc::ThemeColors;
use kara_ui::canvas::{
    color_from_u32, fill_rounded_rect, stroke_rounded_rect_with_pattern,
};
use kara_ui::text::TextRenderer;
use tiny_skia::Pixmap;

/// Card width at 1080p — scales up for larger monitors via the same
/// linear factor as font sizes. Height is **derived** from content +
/// padding in `draw_login_card` so the panel never has a dead zone at
/// the bottom, which is what made the 1440p rendering feel off.
const CARD_W_BASE: f32 = 400.0;
const CARD_MIN_SCALE: f32 = 1.0;
const CARD_MAX_SCALE: f32 = 1.5;
/// How far the card sits from the bottom-right corner, as a fraction
/// of the output's smaller dimension. Feels better than a hardcoded
/// pixel margin when the surface can be 1080p, 1440p, or portrait.
const CARD_MARGIN_FRAC: f32 = 0.05;

const CARD_RADIUS: f32 = 16.0;
/// Card alpha — tuned so the blurred wallpaper still reads through
/// while the card obviously floats above it.
const CARD_ALPHA: u8 = 170;

/// Darkening overlay the lock paints on every output before its own
/// content. Provides readable contrast over bright wallpapers without
/// killing the blur aesthetic — about 55% toward black.
const DIM_ALPHA: u8 = 140;

// Password-field geometry inside the card.
const FIELD_H_BASE: f32 = 42.0;
const FIELD_RADIUS: f32 = 8.0;
const BULLET_RADIUS: f32 = 3.25;
const BULLET_GAP: f32 = 10.0;

pub struct LockUi {
    theme: ThemeColors,
    /// Cached border-tile PNG — loaded once at construction, reused.
    border_tile: Option<Pixmap>,
    clock: TextRenderer,
    date: TextRenderer,
    badge: TextRenderer,
    card_title: TextRenderer,
    card_body: TextRenderer,
}

impl LockUi {
    pub fn new(theme: ThemeColors) -> Self {
        let border_tile = theme
            .border_tile_path
            .as_deref()
            .and_then(|p| Pixmap::load_png(p).ok());
        let font_family = theme
            .font_family
            .clone()
            .filter(|s| !s.is_empty());
        let make = |size: f32| match font_family.as_deref() {
            Some(f) => TextRenderer::new_with_font(f, size),
            None => TextRenderer::new(size),
        };
        Self {
            theme,
            border_tile,
            clock: make(128.0),
            date: make(22.0),
            badge: make(14.0),
            card_title: make(20.0),
            card_body: make(12.0),
        }
    }

    pub fn render(
        &mut self,
        width: u32,
        height: u32,
        is_primary: bool,
        username: &str,
        password_len: usize,
        error: Option<&str>,
        busy: bool,
    ) -> Pixmap {
        let mut pm = Pixmap::new(width.max(1), height.max(1))
            .expect("failed to allocate lock pixmap");

        // Dim overlay — every output gets this so bright wallpapers
        // don't defeat the text on top. theme.bg provides a tint that
        // matches the palette; alpha is tuned below-half so the blur
        // is still clearly visible underneath.
        let (dr, dg, db) = split_rgb(self.theme.bg);
        pm.fill(tiny_skia::Color::from_rgba8(dr, dg, db, DIM_ALPHA));

        // Non-primary outputs stop here — dim only, no widgets or card.
        if !is_primary {
            return pm;
        }

        self.draw_clock_and_widgets(&mut pm, width, height);
        self.draw_login_card(
            &mut pm,
            width,
            height,
            username,
            password_len,
            error,
            busy,
        );

        pm
    }

    fn draw_clock_and_widgets(&mut self, pm: &mut Pixmap, width: u32, height: u32) {
        let base = (width.min(height) as f32).max(360.0);
        let clock_size = (base * 0.16).clamp(80.0, 224.0);
        self.clock.set_font_size(clock_size);

        let now = Local::now();
        let clock_text = now.format("%H:%M").to_string();
        let clock_w = self.clock.measure(&clock_text) as f32;
        let clock_x = (width as f32 - clock_w) * 0.5;
        // Clock centered vertically in the upper half — not pinned to
        // the top, not all the way to the middle. Previously at 20%
        // which felt glued; 32% lands the HH:MM comfortably above
        // the exact centerline so the date + badge rows below it
        // still read as "upper cluster" rather than "stuff crossing
        // the middle of the screen".
        let clock_y = height as f32 * 0.32;
        self.clock
            .draw(pm, &clock_text, clock_x, clock_y, self.theme.text);

        // Date beneath the clock.
        let date_size = (base * 0.028).clamp(16.0, 34.0);
        self.date.set_font_size(date_size);
        let date_text = now.format("%A, %B %-d").to_string();
        let date_w = self.date.measure(&date_text) as f32;
        let date_y = clock_y + clock_size + 12.0;
        self.date.draw(
            pm,
            &date_text,
            (width as f32 - date_w) * 0.5,
            date_y,
            self.theme.text,
        );

        // "Locked" badge. Nerd Font padlock glyph + label + hostname.
        let badge_size = (base * 0.022).clamp(13.0, 20.0);
        self.badge.set_font_size(badge_size);
        let badge_text = format!(
            "\u{F033E}  Locked  ·  {}",
            hostname().unwrap_or_else(|| "session".into())
        );
        let badge_w = self.badge.measure(&badge_text) as f32;
        let badge_y = date_y + date_size + 16.0;
        self.badge.draw(
            pm,
            &badge_text,
            (width as f32 - badge_w) * 0.5,
            badge_y,
            self.theme.accent,
        );
    }

    fn draw_login_card(
        &mut self,
        pm: &mut Pixmap,
        width: u32,
        height: u32,
        username: &str,
        password_len: usize,
        error: Option<&str>,
        busy: bool,
    ) {
        // Scale everything off the smaller screen dimension so 1080p
        // → 1440p → 4K all get proportional sizing. Portrait outputs
        // (rotated panels) get the same treatment based on their
        // narrow dimension.
        let min_dim = width.min(height) as f32;
        let scale = (min_dim / 1080.0).clamp(CARD_MIN_SCALE, CARD_MAX_SCALE);

        // Interior spacing — all scaled together so padding feels
        // matched to content size at every resolution. Top padding is
        // a touch larger than the bottom so the username title has
        // breathing room above it (cosmic-text's `y` is the top of
        // the em-box, not the visual ascender, so a flat 24 looked
        // like the title was clinging to the card edge).
        let pad_x = 24.0 * scale;
        let pad_y_top = 34.0 * scale;
        let pad_y_bottom = 24.0 * scale;
        let title_size = 20.0 * scale;
        let title_to_divider = 12.0 * scale;
        let divider_to_field = 14.0 * scale;
        let field_h = FIELD_H_BASE * scale;
        let field_to_status = 22.0 * scale; // more air — status line no longer hugs the input
        let status_size = 12.0 * scale;

        // Derive card height from content so the bottom padding
        // always matches the top padding (no dead space on big
        // monitors) and status text can never overflow.
        let card_w = CARD_W_BASE * scale;
        let card_h = pad_y_top
            + title_size
            + title_to_divider
            + 1.0 // divider hairline
            + divider_to_field
            + field_h
            + field_to_status
            + status_size
            + pad_y_bottom;

        let margin = (min_dim * CARD_MARGIN_FRAC).clamp(40.0, 96.0);
        let card_x = width as f32 - card_w - margin;
        let card_y = height as f32 - card_h - margin;
        let panel_radius = self
            .theme
            .border_radius
            .map(|r| (r as f32).max(CARD_RADIUS))
            .unwrap_or(CARD_RADIUS);

        // Card surface — translucent, no visible border on non-tiled
        // themes. Fantasy (and any theme shipping a border_tile_path)
        // still paints its ornamental pattern; plain themes get a
        // clean rounded silhouette against the dim+blur behind.
        let (sr, sg, sb) = split_rgb(self.theme.surface);
        fill_rounded_rect(
            pm,
            card_x,
            card_y,
            card_w,
            card_h,
            panel_radius,
            tiny_skia::Color::from_rgba8(sr, sg, sb, CARD_ALPHA),
        );
        if let Some(tile) = &self.border_tile {
            let border_w = self
                .theme
                .border_px
                .map(|b| b as f32)
                .unwrap_or(2.0)
                .max(1.0);
            let inset = border_w * 0.5;
            stroke_rounded_rect_with_pattern(
                pm,
                card_x + inset,
                card_y + inset,
                card_w - border_w,
                card_h - border_w,
                (panel_radius - inset).max(0.0),
                tile,
                border_w,
            );
        }

        // Lay out content top-to-bottom using the pre-computed spacing
        // constants. Each `cursor` assignment advances by the element's
        // height + the gap below it, and the final cursor lands exactly
        // at `card_y + card_h - pad_y_bottom` — so the bottom padding
        // is guaranteed to match the top.
        let mut cursor = card_y + pad_y_top;

        // Username.
        self.card_title.set_font_size(title_size);
        self.card_title
            .draw(pm, username, card_x + pad_x, cursor, self.theme.accent);
        cursor += title_size + title_to_divider;

        // Hairline divider — subtle visual hierarchy between the
        // user identity and the password entry.
        let (borr, borg, borb) = split_rgb(self.theme.border);
        pm.fill_rect(
            tiny_skia::Rect::from_xywh(
                card_x + pad_x,
                cursor,
                card_w - pad_x * 2.0,
                1.0,
            )
            .unwrap(),
            &tiny_skia::Paint {
                shader: tiny_skia::Shader::SolidColor(tiny_skia::Color::from_rgba8(
                    borr, borg, borb, 140,
                )),
                ..Default::default()
            },
            tiny_skia::Transform::identity(),
            None,
        );
        cursor += 1.0 + divider_to_field;

        // Password field.
        let field_y = cursor;
        let field_w = card_w - pad_x * 2.0;
        let (bgr, bgg, bgb) = split_rgb(self.theme.bg);
        fill_rounded_rect(
            pm,
            card_x + pad_x,
            field_y,
            field_w,
            field_h,
            FIELD_RADIUS,
            tiny_skia::Color::from_rgba8(bgr, bgg, bgb, 220),
        );

        // Bullets per password character.
        let bullet_r = BULLET_RADIUS * scale;
        let bullet_gap = BULLET_GAP * scale;
        let bullet_color = color_from_u32(if busy {
            self.theme.text_muted
        } else {
            self.theme.text
        });
        let bullets_inset = 14.0 * scale;
        let bullets_origin_x = card_x + pad_x + bullets_inset;
        let bullets_cy = field_y + field_h * 0.5;
        let bullets_max =
            ((field_w - bullets_inset * 2.0) / (bullet_r * 2.0 + bullet_gap)).floor() as usize;
        let visible = password_len.min(bullets_max.max(1));
        for i in 0..visible {
            let cx = bullets_origin_x + bullet_r + i as f32 * (bullet_r * 2.0 + bullet_gap);
            kara_ui::canvas::fill_circle(pm, cx, bullets_cy, bullet_r, bullet_color);
        }

        cursor += field_h + field_to_status;

        // Status line. Kept at the same left inset as the field
        // content (pad_x + bullets_inset) so it aligns visually with
        // the text inside the password field rather than the card
        // edge. With `field_to_status` bumped above, it now has real
        // breathing room from the field.
        self.card_body.set_font_size(status_size);
        let (status_text, status_color) = if busy {
            ("checking…", self.theme.text)
        } else if let Some(e) = error {
            (e, self.theme.accent)
        } else {
            ("enter password  ↵", self.theme.text)
        };
        self.card_body.draw(
            pm,
            status_text,
            card_x + pad_x + bullets_inset,
            cursor,
            status_color,
        );
    }
}

fn split_rgb(c: u32) -> (u8, u8, u8) {
    (
        ((c >> 16) & 0xFF) as u8,
        ((c >> 8) & 0xFF) as u8,
        (c & 0xFF) as u8,
    )
}

fn hostname() -> Option<String> {
    std::fs::read_to_string("/etc/hostname")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}
