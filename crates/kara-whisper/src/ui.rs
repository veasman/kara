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
/// Square side length for the card's leading icon thumbnail. Sized
/// so glimpse's capture previews still read at typical card heights.
const ICON_SIZE: u32 = 60;
/// Column offset for the text block when the notification carries an
/// `app_icon` — leaves `ICON_SIZE` + padding of room for the thumbnail.
const TEXT_X_WITH_ICON: f32 = PADDING + ICON_SIZE as f32 + PADDING * 0.5;
/// Card background alpha. Kept just shy of fully opaque so there's a
/// subtle hint that the surface is a floating overlay rather than a
/// painted-on decal, while still being readable over busy wallpapers.
/// Earlier the default was 200 (~78%) which bled too much wallpaper
/// through on dense photos; 240 reads as "glass panel, barely tinted".
const CARD_ALPHA: u8 = 240;

pub struct NotificationUI {
    text: TextRenderer,
    text_small: TextRenderer,
    theme: ThemeColors,
    /// Cached decode of the theme's border tile PNG. The path is
    /// rarely the same string twice in a row (a theme swap updates
    /// it) but within a theme it stays fixed, so we key the cache on
    /// the resolved path and only re-decode when the path changes.
    /// Without this, every `render()` call re-read and re-decoded the
    /// PNG from disk — whisper redraws on every new notification and
    /// on the 1 Hz theme poll, so that was a lot of wasted I/O + zlib.
    border_tile_cache: Option<(String, Pixmap)>,
}

impl NotificationUI {
    pub fn new(theme: ThemeColors) -> Self {
        Self {
            text: TextRenderer::new(14.0),
            text_small: TextRenderer::new(11.0),
            theme,
            border_tile_cache: None,
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

    pub fn total_height_for(&self, notifications: &[Notification]) -> u32 {
        if notifications.is_empty() {
            return 1;
        }
        let border_pad = self.border_pad();
        let mut h = 0u32;
        for (i, n) in notifications.iter().enumerate() {
            h += Self::card_height(n, border_pad);
            if i + 1 < notifications.len() {
                h += GAP;
            }
        }
        h
    }

    /// Extra vertical breathing room reserved at the bottom of each card
    /// so its content (especially the action-button row) never gets eaten
    /// by a thick ornamental border. Matches `stroke_w` in the render loop.
    fn border_pad(&self) -> u32 {
        let b = self.theme.border_px.unwrap_or(0).max(0) as f32;
        let stroke_w = if b > 0.0 { b } else { 2.0 };
        (stroke_w + 4.0).ceil() as u32
    }

    fn card_height(n: &Notification, border_pad: u32) -> u32 {
        let base = if n.has_button_actions() {
            CARD_BASE_HEIGHT + CARD_ACTION_ROW
        } else {
            CARD_BASE_HEIGHT
        };
        base + border_pad
    }

    pub fn card_width() -> u32 {
        CARD_WIDTH
    }

    /// Render all visible notifications and return the hit regions for
    /// action buttons and card bodies so the pointer handler can
    /// resolve a click to either "invoke action N" or "dismiss card N".
    /// Regions are in pixmap-local coordinates, matching the layer
    /// surface's buffer space. Action regions are pushed first so the
    /// hit-test can prefer them over the enclosing card-body region.
    pub fn render(
        &mut self,
        notifications: &[Notification],
    ) -> (Option<Pixmap>, Vec<HitRegion>) {
        let mut hits: Vec<HitRegion> = Vec::new();
        if notifications.is_empty() {
            return (None, hits);
        }

        let height = self.total_height_for(notifications);
        let mut pixmap = match Pixmap::new(CARD_WIDTH, height) {
            Some(p) => p,
            None => return (None, hits),
        };

        // Resolve the theme's window border tile to a decoded Pixmap.
        // Previously we re-decoded the PNG on every render; now the
        // decode is cached on `NotificationUI` and only rebuilt when
        // the configured path actually changes.
        let border_tile = {
            let want = self.theme.border_tile_path.as_deref();
            let have = self.border_tile_cache.as_ref().map(|(p, _)| p.as_str());
            match (want, have) {
                (Some(w), Some(h)) if w == h => {}
                (None, None) => {}
                (None, Some(_)) => {
                    self.border_tile_cache = None;
                }
                (Some(w), _) => {
                    self.border_tile_cache =
                        Pixmap::load_png(w).ok().map(|pm| (w.to_string(), pm));
                }
            }
            self.border_tile_cache.as_ref().map(|(_, pm)| pm)
        };
        let theme_border_px = self.theme.border_px.unwrap_or(0).max(0) as f32;
        let theme_border_radius = self
            .theme
            .border_radius
            .map(|r| r as f32)
            .unwrap_or(CARD_RADIUS);

        let border_pad = self.border_pad();
        let mut y_off = 0.0f32;
        for (_i, notif) in notifications.iter().enumerate() {
            let card_h = Self::card_height(notif, border_pad) as f32;

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

            // Optional leading thumbnail. notify-send --icon accepts a
            // filesystem path; glimpse passes the saved PNG so the
            // screenshot preview appears directly on the card. XDG
            // theme-name lookup (org.freedesktop.Notifications style)
            // is not supported yet — only direct paths.
            let has_icon = draw_app_icon(
                &mut pixmap,
                &notif.app_icon,
                PADDING,
                y_off + PADDING,
                ICON_SIZE,
            );
            let text_x = if has_icon { TEXT_X_WITH_ICON } else { PADDING };

            // Summary text (top)
            self.text.draw(
                &mut pixmap,
                &notif.summary,
                text_x,
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
                text_x,
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

            // Action buttons (below the body, if any). Buttons are
            // uniformly sized and the whole cluster is centered
            // horizontally — an off-center cluster with a big empty
            // strip on the right (the old behavior when btn_w was
            // capped at 120 and bx started at PADDING) was the main
            // "line up weird" complaint.
            //
            // The `default` action is excluded from the button row
            // per freedesktop spec — it's the implicit click target
            // and body-click handles it. Apps like Thunderbird ship
            // a `("default", "Activate")` pair; rendering it here
            // produced a redundant "Activate" button whose behaviour
            // was identical to tapping the card itself.
            let button_actions: Vec<&(String, String)> =
                notif.button_actions().collect();
            if !button_actions.is_empty() {
                let btn_y = base_bottom + 2.0;
                let btn_h = CARD_ACTION_ROW as f32 - 4.0;
                let btn_gap = 6.0f32;
                let n_btns = button_actions.len() as f32;
                let total_gap = btn_gap * (n_btns - 1.0).max(0.0);
                let avail = CARD_WIDTH as f32 - PADDING * 2.0 - total_gap;
                let btn_w = (avail / n_btns).min(120.0);
                let cluster_w = btn_w * n_btns + total_gap;
                let mut bx = (CARD_WIDTH as f32 - cluster_w) / 2.0;

                for (id, label) in button_actions {
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
                    hits.push(HitRegion {
                        notif_id: notif.id,
                        kind: HitKind::Action { id: id.clone() },
                        rect: (bx, btn_y, btn_w, btn_h),
                    });
                    bx += btn_w + btn_gap;
                }
            }

            // Whole-card dismiss region. Pushed AFTER any action buttons
            // so the click handler matches action rects first and only
            // falls back to this envelope when the user clicks elsewhere
            // on the card. The open_path carries the notification's icon
            // path when it's an absolute file — glimpse uses this to
            // turn the capture thumbnail into a one-click viewer.
            let open_path = if notif.app_icon.starts_with('/')
                && std::path::Path::new(&notif.app_icon).is_file()
            {
                Some(notif.app_icon.clone())
            } else {
                None
            };
            hits.push(HitRegion {
                notif_id: notif.id,
                kind: HitKind::Body { open_path },
                rect: (0.0, y_off, CARD_WIDTH as f32, card_h),
            });

            y_off += card_h + GAP as f32;
        }

        (Some(pixmap), hits)
    }
}

/// A clickable rect inside the rendered notification stack. Either an
/// action button (invoke that action on click) or a card body (dismiss
/// on click, optionally opening the notification's icon path). Action
/// regions are pushed first so the hit-test can prefer them over the
/// enclosing card-body region.
#[derive(Debug, Clone)]
pub struct HitRegion {
    pub notif_id: u32,
    pub kind: HitKind,
    pub rect: (f32, f32, f32, f32),
}

#[derive(Debug, Clone)]
pub enum HitKind {
    Action { id: String },
    /// Click anywhere on the card body. If `open_path` is `Some`, it's
    /// an absolute filesystem path to hand to xdg-open — glimpse sets
    /// this via `--icon /tmp/kara-screenshot-…png` so a tap on the card
    /// opens the capture. Always dismisses the notification regardless.
    Body { open_path: Option<String> },
}

impl HitRegion {
    pub fn contains(&self, x: f64, y: f64) -> bool {
        let (rx, ry, rw, rh) = self.rect;
        x >= rx as f64 && x < (rx + rw) as f64 && y >= ry as f64 && y < (ry + rh) as f64
    }
}

/// Try to load and draw an icon image at `(x, y)` scaled to fit a
/// `size × size` box (aspect-preserving, letterboxed). Returns `true`
/// if something was drawn. Empty paths, missing files, or decode
/// failures all quietly return `false`, so the card just falls back
/// to the text-only layout.
fn draw_app_icon(pixmap: &mut Pixmap, path_str: &str, x: f32, y: f32, size: u32) -> bool {
    if path_str.is_empty() {
        return false;
    }
    if !path_str.starts_with('/') {
        // Only absolute filesystem paths today. XDG icon theme
        // lookup (name-only identifiers like "firefox") would need
        // a full icon-theme resolver — out of scope for this patch.
        return false;
    }
    let path = std::path::Path::new(path_str);
    if !path.is_file() {
        return false;
    }

    let img = match image::open(path) {
        Ok(i) => i.to_rgba8(),
        Err(_) => return false,
    };
    let (src_w, src_h) = (img.width(), img.height());
    if src_w == 0 || src_h == 0 {
        return false;
    }

    // Aspect-preserving fit into the target box.
    let scale = (size as f32 / src_w as f32).min(size as f32 / src_h as f32);
    let dst_w = (src_w as f32 * scale).round() as u32;
    let dst_h = (src_h as f32 * scale).round() as u32;
    if dst_w == 0 || dst_h == 0 {
        return false;
    }
    let off_x = x + (size as f32 - dst_w as f32) * 0.5;
    let off_y = y + (size as f32 - dst_h as f32) * 0.5;

    // Resize into a pixmap and blit with tiny-skia Pattern fill —
    // piggybacks on the existing render path instead of pulling in a
    // separate blitter.
    let mut icon_pm = match tiny_skia::Pixmap::new(dst_w, dst_h) {
        Some(p) => p,
        None => return false,
    };
    let sx = src_w as f32 / dst_w as f32;
    let sy = src_h as f32 / dst_h as f32;
    let dst = icon_pm.data_mut();
    for py in 0..dst_h {
        for px in 0..dst_w {
            let qx = ((px as f32 * sx) as u32).min(src_w - 1);
            let qy = ((py as f32 * sy) as u32).min(src_h - 1);
            let src_px = img.get_pixel(qx, qy).0;
            // Premultiply for tiny-skia.
            let a = src_px[3] as f32 / 255.0;
            let di = (py * dst_w + px) as usize * 4;
            dst[di] = (src_px[0] as f32 * a).round() as u8;
            dst[di + 1] = (src_px[1] as f32 * a).round() as u8;
            dst[di + 2] = (src_px[2] as f32 * a).round() as u8;
            dst[di + 3] = src_px[3];
        }
    }

    let paint = tiny_skia::PixmapPaint::default();
    pixmap.draw_pixmap(
        off_x.round() as i32,
        off_y.round() as i32,
        icon_pm.as_ref(),
        &paint,
        tiny_skia::Transform::identity(),
        None,
    );
    true
}
