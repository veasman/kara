/// Bar renderer — draws the bar to a tiny-skia Pixmap.
///
/// Renders background, left/center/right sections, pill backgrounds,
/// workspace dots, volume bar, and text via kara-ui.

use tiny_skia::{Color, Pixmap};

use kara_config::{Bar as BarConfig, BarModuleKind, BarModule, BarSection, Theme};
use kara_ui::canvas::{color_from_u32, fill_circle, fill_rounded_rect, stroke_rounded_rect};
use kara_ui::TextRenderer;

/// Build a tiny-skia `Color` from a 0xRRGGBB u32 plus an explicit 0-255 alpha.
/// Used for bar/module backgrounds, borders, and pill fills so the new
/// transparency knobs in the bar schema actually take effect.
fn rgba(c: u32, alpha: u8) -> Color {
    Color::from_rgba8(
        ((c >> 16) & 0xFF) as u8,
        ((c >> 8) & 0xFF) as u8,
        (c & 0xFF) as u8,
        alpha,
    )
}

use crate::status::StatusCache;
use crate::text::{self, ModuleContext};

// ── Public API ──────────────────────────────────────────────────────

/// Persistent state for the bar renderer.
pub struct BarRenderer {
    text: TextRenderer,
    /// Cached decoded module background tile. Re-decoded lazily
    /// when `render()` sees `bar_config.module_bg_tile` change.
    module_bg_tile: Option<(std::path::PathBuf, Pixmap)>,
}

impl BarRenderer {
    pub fn new(font_family: &str, font_size: f32) -> Self {
        Self {
            text: TextRenderer::new_with_font(font_family, font_size),
            module_bg_tile: None,
        }
    }

    /// Update font settings (e.g., after config reload).
    pub fn set_font(&mut self, font_family: &str, font_size: f32) {
        self.text.set_font_family(font_family);
        self.text.set_font_size(font_size);
    }

    /// Ensure the cached module background tile reflects the current
    /// config path. Decodes the PNG via tiny-skia on first use (or
    /// path change) and stores it in `self.module_bg_tile`. Decode
    /// failures log once and fall back to solid-color pill fills.
    fn refresh_module_bg_tile(&mut self, bar_config: &BarConfig) {
        let want = bar_config.module_bg_tile.as_ref();
        let have = self.module_bg_tile.as_ref().map(|(p, _)| p.as_path());
        match (want, have) {
            (None, None) => {}
            (None, Some(_)) => self.module_bg_tile = None,
            (Some(w), Some(h)) if w.as_path() == h => {}
            (Some(w), _) => match Pixmap::load_png(w) {
                Ok(pm) => self.module_bg_tile = Some((w.clone(), pm)),
                Err(e) => {
                    eprintln!(
                        "kara-sight: failed to decode module_bg_tile {}: {e}",
                        w.display()
                    );
                    self.module_bg_tile = None;
                }
            },
        }
    }

    /// Render the bar to a new Pixmap.
    pub fn render(
        &mut self,
        width: u32,
        bar_config: &BarConfig,
        theme: &Theme,
        status: &StatusCache,
        ws_ctx: &WorkspaceContext,
    ) -> Option<Pixmap> {
        let height = bar_config.height as u32;
        if width == 0 || height == 0 {
            return None;
        }

        self.refresh_module_bg_tile(bar_config);
        // Clone the cached tile once per frame so later calls into
        // `&mut self` (measure / draw text) don't conflict with the
        // `&self` borrow the tile reference would hold across the
        // render loop. The tile is small (typically 48×48 RGBA ≈
        // 9 KB) so the clone is cheap.
        let module_tile: Option<Pixmap> =
            self.module_bg_tile.as_ref().map(|(_, pm)| pm.clone());

        let mut pixmap = Pixmap::new(width, height)?;

        // Draw the bar surface (if enabled). The bar and its modules are
        // two separate concerns now: the bar has its own
        // rounded/bordered/transparent background, and each module
        // (optionally) has its own when `pill` is set. See the bar config
        // docs in kara-config for the spatial model.
        if bar_config.background {
            let bg = bar_config.background_color.unwrap_or(theme.bg);
            let bg_alpha = bar_config.background_alpha;
            let radius = bar_config.rounded.max(0) as f32;
            fill_rounded_rect(
                &mut pixmap, 0.0, 0.0, width as f32, height as f32,
                radius, rgba(bg, bg_alpha),
            );
            if bar_config.border_px > 0 {
                let bc = bar_config.border_color.unwrap_or(theme.border);
                stroke_rounded_rect(
                    &mut pixmap,
                    0.5, 0.5, width as f32 - 1.0, height as f32 - 1.0,
                    (radius - 0.5).max(0.0),
                    rgba(bc, 255),
                    bar_config.border_px as f32,
                );
            }
            // Note: `bar_config.blur` is parsed and stored but not yet
            // rendered — implementing real blur needs GL shader support
            // in the compositor renderer. Tracked under backlog "blur".
        }

        // Build module context
        let mod_ctx = ModuleContext {
            theme,
            icons: bar_config.icons,
            colors: bar_config.colors,
            status,
            current_ws: ws_ctx.current_ws,
            occupied_workspaces: ws_ctx.occupied_workspaces,
            focused_title: ws_ctx.focused_title.clone(),
            monitor_id: ws_ctx.monitor_id,
            sync_enabled: ws_ctx.sync_enabled,
            is_focused_monitor: ws_ctx.is_focused_monitor,
        };

        // Separate modules by section
        let (left, center, right) = split_modules(&bar_config.modules);

        // Group adjacent modules in each section by `group:<name>` id.
        // A group renders as one continuous pill containing every
        // module's content separated by `module_gap`. Standalone
        // modules (group = None) are each their own group.
        let left_groups = group_adjacent(&left);
        let center_groups = group_adjacent(&center);
        let right_groups = group_adjacent(&right);

        // Measure all modules
        let uses_pills = bar_config.pill;
        let pill_pad = if uses_pills { bar_config.module_padding_x.max(0) as u32 } else { 0 };
        let item_gap = bar_config.module_gap.max(0) as u32;
        let content_margin = bar_config.edge_padding_x.max(0) as u32;

        // Vertical layout:
        //   * `edge_padding_y` insets every pill from the bar's top
        //     and bottom edge — the gap between bar edge and pill.
        //   * `module_padding_y` further insets the pill from the
        //     content area. Previously this field was parsed but
        //     never rendered, so there was no way to make pills
        //     shorter than (bar_height - 2*edge_padding_y). With it
        //     wired in, a theme can hold edge_padding_y small (pills
        //     sit close to the bar edges) while still giving the
        //     pills themselves more vertical breathing room around
        //     their contents.
        let pad_y = if uses_pills { bar_config.module_padding_y.max(0) } else { 0 };
        let content_y = bar_config.edge_padding_y.max(0) + pad_y;
        let content_h =
            (height as i32 - bar_config.edge_padding_y.max(0) * 2 - pad_y * 2).max(1);

        // Measure a whole group's total outer width: sum of member
        // content widths + (n-1) intra-group gaps + 2*pill_pad. One
        // pill spans the entire group.
        let measure_group = |renderer: &mut Self, g: &[&BarModule]| -> u32 {
            let content_w: u32 = g
                .iter()
                .map(|m| renderer.measure_module(m, &mod_ctx, bar_config, ws_ctx))
                .sum();
            let intra_gap = (g.len().saturating_sub(1) as u32) * item_gap;
            content_w + intra_gap + pill_pad * 2
        };

        // Layout LEFT section (left to right)
        let mut left_x = content_margin as i32;
        for group in &left_groups {
            let total_w = measure_group(self, group);
            if uses_pills {
                draw_pill(
                    &mut pixmap,
                    left_x as f32,
                    content_y as f32,
                    total_w as f32,
                    content_h as f32,
                    bar_config,
                    theme,
                    module_tile.as_ref(),
                );
            }
            let mut mx = left_x + pill_pad as i32;
            for (i, module) in group.iter().enumerate() {
                let content_w = self.measure_module(module, &mod_ctx, bar_config, ws_ctx);
                self.draw_module_content(
                    &mut pixmap, module, &mod_ctx, bar_config, theme,
                    mx, content_w as i32, ws_ctx, content_y, content_h,
                );
                mx += content_w as i32;
                if i + 1 < group.len() {
                    mx += item_gap as i32;
                }
            }
            left_x += total_w as i32 + item_gap as i32;
        }

        // Layout RIGHT section (right to left by group)
        let mut right_x = width as i32 - content_margin as i32;
        for group in right_groups.iter().rev() {
            let total_w = measure_group(self, group);
            right_x -= total_w as i32;
            if uses_pills {
                draw_pill(
                    &mut pixmap,
                    right_x as f32,
                    content_y as f32,
                    total_w as f32,
                    content_h as f32,
                    bar_config,
                    theme,
                    module_tile.as_ref(),
                );
            }
            let mut mx = right_x + pill_pad as i32;
            for (i, module) in group.iter().enumerate() {
                let content_w = self.measure_module(module, &mod_ctx, bar_config, ws_ctx);
                self.draw_module_content(
                    &mut pixmap, module, &mod_ctx, bar_config, theme,
                    mx, content_w as i32, ws_ctx, content_y, content_h,
                );
                mx += content_w as i32;
                if i + 1 < group.len() {
                    mx += item_gap as i32;
                }
            }
            right_x -= item_gap as i32;
        }

        // Layout CENTER section (centered, if it fits).
        // Theme-level `bar { hide_center = true }` suppresses the entire
        // center row even if the user's config declares center modules.
        // Lets fantasy / moonlight drop the title chrome without the
        // user having to comment center modules out of their base config.
        if !center_groups.is_empty() && !bar_config.hide_center {
            let group_widths: Vec<u32> =
                center_groups.iter().map(|g| measure_group(self, g)).collect();
            let total_center_w: u32 = group_widths.iter().sum::<u32>()
                + (center_groups.len().saturating_sub(1) as u32) * item_gap;

            let safe_left = left_x;
            let safe_right = right_x;

            if (safe_right - safe_left) >= (total_center_w as i32 + 40) {
                let mut cx = ((width as i32 - total_center_w as i32) / 2)
                    .max(safe_left)
                    .min(safe_right - total_center_w as i32);

                for (group, total_w) in center_groups.iter().zip(group_widths.iter()) {
                    if uses_pills {
                        draw_pill(
                            &mut pixmap,
                            cx as f32,
                            content_y as f32,
                            *total_w as f32,
                            content_h as f32,
                            bar_config,
                            theme,
                            module_tile.as_ref(),
                        );
                    }
                    let mut mx = cx + pill_pad as i32;
                    for (i, module) in group.iter().enumerate() {
                        let content_w =
                            self.measure_module(module, &mod_ctx, bar_config, ws_ctx);
                        self.draw_module_content(
                            &mut pixmap, module, &mod_ctx, bar_config, theme,
                            mx, content_w as i32, ws_ctx, content_y, content_h,
                        );
                        mx += content_w as i32;
                        if i + 1 < group.len() {
                            mx += item_gap as i32;
                        }
                    }
                    cx += *total_w as i32 + item_gap as i32;
                }
            }
        }

        Some(pixmap)
    }

    /// Measure the content width of a module (excluding pill padding).
    fn measure_module(
        &mut self,
        module: &BarModule,
        ctx: &ModuleContext,
        bar_config: &BarConfig,
        ws_ctx: &WorkspaceContext,
    ) -> u32 {
        if module.kind == BarModuleKind::Workspaces {
            return self.measure_workspaces(ctx, module, ws_ctx);
        }

        let content = text::build_module_text(&module.kind, &module.args, ctx);
        if content.text.is_empty() {
            return 0;
        }

        let mut w = match bar_config.icon_size {
            Some(s) if s > 0.0 && s != self.text.font_size => {
                self.text.measure_row(&content.text, s)
            }
            _ => self.text.measure(&content.text),
        };

        // Volume module may carry an inline bar graph. Its size preset
        // comes from the first positional arg (`small|med|large|none`).
        if module.kind == BarModuleKind::Volume {
            if let Some(size) = volume_bar_size(module) {
                w += 8 + size.width;
            }
        }

        w
    }

    fn measure_workspaces(
        &mut self,
        ctx: &ModuleContext,
        module: &BarModule,
        ws_ctx: &WorkspaceContext,
    ) -> u32 {
        if workspaces_is_badges(module) {
            // Badges: a filled dot per visible workspace. Skip empty,
            // unused workspaces so the module shrinks to just what's
            // in use. Current dot is larger; each dot occupies the
            // current-dot size so gaps stay even regardless of focus.
            let slot = self.text.font_size * 0.9;
            let gap = self.text.font_size * 0.45;
            let count = (0..9)
                .filter(|i| *i == ws_ctx.current_ws || ws_ctx.occupied_workspaces[*i])
                .count();
            if count == 0 {
                return 0;
            }
            return (slot * count as f32 + gap * (count.saturating_sub(1)) as f32)
                .ceil() as u32;
        }
        if ctx.icons {
            let dot_size = self.text.font_size * 0.45;
            let gap = self.text.font_size * 0.4;
            (dot_size * 9.0 + gap * 8.0).ceil() as u32
        } else {
            let digit_w = self.text.measure("0") as f32;
            let gap = self.text.font_size * 0.3;
            (digit_w * 9.0 + gap * 8.0).ceil() as u32
        }
    }

    /// Draw the content (text, workspaces, volume bar) of a single
    /// module at position x. The pill background is drawn separately
    /// by the layout loop — one pill per group, spanning all member
    /// modules. `x` is the content-left position (already past any
    /// pill padding inset).
    fn draw_module_content(
        &mut self,
        pixmap: &mut Pixmap,
        module: &BarModule,
        ctx: &ModuleContext,
        bar_config: &BarConfig,
        theme: &Theme,
        x: i32,
        content_width: i32,
        ws_ctx: &WorkspaceContext,
        content_y: i32,
        content_h: i32,
    ) {
        if content_width <= 0 {
            return;
        }

        let bar_height = bar_config.height;
        let uses_pills = bar_config.pill;

        // Vertical centering: compute the center line for all content.
        // In pill mode, center within the pill; in flat mode, center
        // within full bar.
        let center_y = if uses_pills {
            content_y as f32 + content_h as f32 / 2.0
        } else {
            bar_height as f32 / 2.0
        };

        // Special: Workspaces
        if module.kind == BarModuleKind::Workspaces {
            if workspaces_is_badges(module) {
                self.draw_workspaces_badges(pixmap, x, center_y, ctx, theme, ws_ctx);
            } else {
                self.draw_workspaces(pixmap, x, center_y, ctx, theme, ws_ctx);
            }
            return;
        }

        let content = text::build_module_text(&module.kind, &module.args, ctx);
        if content.text.is_empty() {
            return;
        }

        // Draw text. When the theme provides an explicit icon_size and
        // it differs from the bar font size, fall back to draw_row /
        // measure_row which render icon codepoints at icon_size while
        // leaving regular glyphs at font_size.
        let text_w = match bar_config.icon_size {
            Some(s) if s > 0.0 && s != self.text.font_size => {
                self.text.draw_row(pixmap, &content.text, x as f32, center_y, content.color, s)
                    as i32
            }
            _ => {
                let text_y = self.text.center_y_offset(center_y);
                self.text.draw(pixmap, &content.text, x as f32, text_y, content.color);
                self.text.measure(&content.text) as i32
            }
        };

        // Volume bar — size preset comes from the module's first
        // positional arg (see `volume_bar_size`). `none` disables.
        if module.kind == BarModuleKind::Volume {
            if let Some(size) = volume_bar_size(module) {
                let bar_x = x + text_w + 8;
                let bar_w = size.width as i32;
                let bar_h = size.height as i32;
                let bar_y = center_y - bar_h as f32 / 2.0;
                let bar_r = size.radius as f32;
                draw_volume_bar(
                    pixmap,
                    bar_x as f32, bar_y, bar_w as f32, bar_h as f32, bar_r,
                    &ctx.status.volume, theme,
                );
            }
        }
    }

    /// Draw workspace indicators.
    fn draw_workspaces(
        &mut self,
        pixmap: &mut Pixmap,
        x: i32,
        center_y: f32,
        ctx: &ModuleContext,
        theme: &Theme,
        ws_ctx: &WorkspaceContext,
    ) {
        let dot_size = self.text.font_size * 0.45;
        let gap = if ctx.icons {
            self.text.font_size * 0.4
        } else {
            self.text.font_size * 0.3
        };

        let mut cx = x as f32;

        for i in 0..9 {
            let is_current = i == ws_ctx.current_ws;
            let is_occupied = ws_ctx.occupied_workspaces[i];

            let color = if is_current {
                theme.accent
            } else if is_occupied {
                theme.border
            } else {
                theme.text_muted
            };

            // Advance by element width only; add `gap` only between elements so
            // the module occupies exactly 9*elem + 8*gap, matching measure_workspaces.
            let add_gap = i < 8;

            if ctx.icons {
                let radius = if is_current { dot_size * 0.6 } else { dot_size * 0.45 };
                fill_circle(pixmap, cx + dot_size / 2.0, center_y, radius, color_from_u32(color));
                cx += dot_size;
                if add_gap { cx += gap; }
            } else {
                let digit = format!("{}", i + 1);
                let text_y = self.text.center_y_offset(center_y);
                self.text.draw(pixmap, &digit, cx, text_y, color);
                let digit_w = self.text.measure(&digit) as f32;
                cx += digit_w;
                if add_gap { cx += gap; }
            }
        }
    }

    /// Badges style (fantasy): one filled dot per visible workspace.
    /// Skips unoccupied workspaces so the module shrinks with use.
    /// The current workspace's dot is slightly larger and uses the
    /// accent color; occupied-but-unfocused dots use text_muted for a
    /// dimmer read. Each dot is allotted the same slot width so the
    /// module doesn't re-flow when focus moves between workspaces.
    fn draw_workspaces_badges(
        &mut self,
        pixmap: &mut Pixmap,
        x: i32,
        center_y: f32,
        _ctx: &ModuleContext,
        theme: &Theme,
        ws_ctx: &WorkspaceContext,
    ) {
        let slot = self.text.font_size * 0.9;
        let current_radius = slot * 0.45;
        let other_radius = slot * 0.28;
        let gap = self.text.font_size * 0.45;

        let visible: Vec<usize> = (0..9)
            .filter(|i| *i == ws_ctx.current_ws || ws_ctx.occupied_workspaces[*i])
            .collect();

        let mut cx = x as f32;
        let n = visible.len();
        for (idx, &i) in visible.iter().enumerate() {
            let is_current = i == ws_ctx.current_ws;
            let (radius, color) = if is_current {
                (current_radius, theme.accent)
            } else {
                (other_radius, theme.text_muted)
            };
            fill_circle(pixmap, cx + slot * 0.5, center_y, radius, color_from_u32(color));
            cx += slot;
            if idx + 1 < n {
                cx += gap;
            }
        }
    }
}

/// True when the workspaces module has opted into the badge style
/// (filled circles with numbers, autohiding unused workspaces).
fn workspaces_is_badges(module: &BarModule) -> bool {
    module
        .args
        .iter()
        .any(|a| a.eq_ignore_ascii_case("badges") || a.eq_ignore_ascii_case("big"))
}

// ── Workspace context (provided by compositor) ──────────────────────

pub struct WorkspaceContext {
    pub current_ws: usize,
    pub occupied_workspaces: [bool; 9],
    pub focused_title: String,
    pub monitor_id: usize,
    pub sync_enabled: bool,
    /// True when this monitor is the keyboard-focused monitor. Used by the
    /// bar's monitor module to render a "you are here" highlight.
    pub is_focused_monitor: bool,
}

// ── Bar-specific drawing ────────────────────────────────────────────

fn draw_pill(
    pixmap: &mut Pixmap,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    bar_config: &BarConfig,
    theme: &Theme,
    tile: Option<&Pixmap>,
) {
    if w <= 0.0 || h <= 0.0 {
        return;
    }

    let radius = bar_config.module_rounded.max(0) as f32;

    // Solid surface fill inside the pill — always drawn regardless
    // of whether the tile provides the outline or a flat color does.
    let fill = bar_config.module_background.unwrap_or(theme.surface);
    fill_rounded_rect(pixmap, x, y, w, h, radius, rgba(fill, bar_config.module_alpha));

    if let Some(tile_pm) = tile {
        // Theme supplies an SVG tile → stroke the pill OUTLINE with
        // the tiled pattern. Interior stays the solid surface fill
        // above. Stroke width = module_border_px or a fallback of
        // 2px so the tile is visible even without an explicit width.
        use kara_ui::canvas::stroke_rounded_rect_with_pattern;
        let stroke_w = if bar_config.module_border_px > 0 {
            bar_config.module_border_px as f32
        } else {
            2.0
        };
        stroke_rounded_rect_with_pattern(
            pixmap,
            x + stroke_w / 2.0,
            y + stroke_w / 2.0,
            w - stroke_w,
            h - stroke_w,
            (radius - stroke_w / 2.0).max(0.0),
            tile_pm,
            stroke_w,
        );
    } else if bar_config.module_border_px > 0 {
        let border = bar_config.module_border_color.unwrap_or(theme.border);
        stroke_rounded_rect(
            pixmap,
            x + 0.5,
            y + 0.5,
            w - 1.0,
            h - 1.0,
            (radius - 0.5).max(0.0),
            rgba(border, 255),
            bar_config.module_border_px as f32,
        );
    }
    // Note: `bar_config.module_blur` is parsed but not yet rendered —
    // blur needs GL shader support in the compositor.
}

// ── Inline module config helpers ────────────────────────────────────
//
// Volume is the only built-in module with per-module positional args
// right now. Its first arg picks a size preset for the inline bar
// graph: `small | med | large | none`. `none` disables the graph; any
// other value falls back to `med`.

#[derive(Debug, Clone, Copy)]
struct VolumeBarSize {
    width: u32,
    height: u32,
    radius: u32,
}

impl VolumeBarSize {
    const SMALL: Self = Self { width: 32, height: 4, radius: 2 };
    const MED: Self = Self { width: 46, height: 6, radius: 3 };
    const LARGE: Self = Self { width: 64, height: 8, radius: 4 };
}

/// Resolve a volume module's inline bar size preset.
///
/// Returns `None` when the module's first arg is `none` — meaning no
/// bar graph is rendered. Any other value (or no arg at all) picks a
/// size preset, defaulting to `med`.
fn volume_bar_size(module: &BarModule) -> Option<VolumeBarSize> {
    match module.args.first().map(String::as_str) {
        Some("none") => None,
        Some("small") => Some(VolumeBarSize::SMALL),
        Some("large") => Some(VolumeBarSize::LARGE),
        Some("med") | Some("medium") | None => Some(VolumeBarSize::MED),
        Some(_) => Some(VolumeBarSize::MED),
    }
}

fn draw_volume_bar(
    pixmap: &mut Pixmap,
    x: f32, y: f32, w: f32, h: f32, radius: f32,
    volume: &crate::status::VolumeState,
    theme: &Theme,
) {
    if w <= 0.0 || h <= 0.0 {
        return;
    }

    // Background
    fill_rounded_rect(pixmap, x, y, w, h, radius, color_from_u32(theme.border));

    // Fill
    let pct = if volume.valid && !volume.muted {
        (volume.percent as f32).clamp(0.0, 100.0)
    } else {
        0.0
    };

    let fill_w = (w * pct / 100.0).max(0.0);
    if fill_w > 0.0 {
        let fill_color = if volume.muted {
            theme.text_muted
        } else {
            // Four auto-derived tiers from the theme's accent color. Instead of a
            // smooth lerp (which looked washed out at low volumes) or four separate
            // config values (which was the old noisy approach), we scale the
            // accent's lightness in HSL to produce low/normal/loud/max tiers.
            volume_tier_color(theme.accent, pct)
        };
        fill_rounded_rect(pixmap, x, y, fill_w, h, radius, color_from_u32(fill_color));
    }
}

/// Map a volume percentage to one of four brightness tiers of the accent color.
/// Bands: 0-25% dim, 25-60% normal-dim, 60-85% normal, 85-100% bright (hot).
fn volume_tier_color(accent: u32, pct: f32) -> u32 {
    // Scale factors applied to the accent's RGB components. <1.0 darkens, >1.0
    // brightens (clamped at 255). These were picked by eye to give four visibly
    // distinct but harmonious tiers without introducing a warning-red or
    // rewriting through HSL.
    let scale = if pct < 25.0 {
        0.55
    } else if pct < 60.0 {
        0.80
    } else if pct < 85.0 {
        1.00
    } else {
        1.25
    };
    scale_rgb(accent, scale)
}

fn scale_rgb(rgb: u32, factor: f32) -> u32 {
    let scale = |shift: u32| -> u32 {
        let c = ((rgb >> shift) & 0xFF) as f32;
        (c * factor).round().clamp(0.0, 255.0) as u32
    };
    (scale(16) << 16) | (scale(8) << 8) | scale(0)
}

// ── Helpers ─────────────────────────────────────────────────────────

fn split_modules(modules: &[BarModule]) -> (Vec<&BarModule>, Vec<&BarModule>, Vec<&BarModule>) {
    let mut left = Vec::new();
    let mut center = Vec::new();
    let mut right = Vec::new();

    for m in modules {
        match m.section {
            BarSection::Left => left.push(m),
            BarSection::Center => center.push(m),
            BarSection::Right => right.push(m),
        }
    }

    (left, center, right)
}

/// Collapse adjacent modules that share a `group:<name>` id into one
/// group. Standalone modules (group = None) are each their own group.
/// Order is preserved: the returned vec matches the iteration order
/// of the input.
///
/// Example:
///   `[left monitor group:left, left workspaces group:left, left clock]`
///   → `[[monitor, workspaces], [clock]]`
///
/// Non-adjacent same-named groups are NOT merged — this keeps the
/// implementation simple and matches user intent (if you wrote the
/// group membership out of order, you probably wanted separate pills).
fn group_adjacent<'a>(modules: &[&'a BarModule]) -> Vec<Vec<&'a BarModule>> {
    let mut result: Vec<Vec<&'a BarModule>> = Vec::new();
    for m in modules {
        let extend = match (result.last(), m.group.as_ref()) {
            (Some(last), Some(name)) => last
                .last()
                .and_then(|prev| prev.group.as_ref())
                .map(|prev_name| prev_name == name)
                .unwrap_or(false),
            _ => false,
        };
        if extend {
            result.last_mut().unwrap().push(*m);
        } else {
            result.push(vec![*m]);
        }
    }
    result
}
