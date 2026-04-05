/// Bar renderer — draws the bar to a tiny-skia Pixmap.
///
/// Renders background, left/center/right sections, pill backgrounds,
/// workspace dots, volume bar, and text via kara-ui.

use tiny_skia::Pixmap;

use kara_config::{Bar as BarConfig, BarModuleKind, BarModule, BarModuleStyle, BarSection, Theme};
use kara_ui::canvas::{color_from_u32, fill_circle, fill_rounded_rect, stroke_rounded_rect};
use kara_ui::TextRenderer;

use crate::status::StatusCache;
use crate::text::{self, ModuleContext};

// ── Public API ──────────────────────────────────────────────────────

/// Persistent state for the bar renderer.
pub struct BarRenderer {
    text: TextRenderer,
}

impl BarRenderer {
    pub fn new(font_family: &str, font_size: f32) -> Self {
        Self {
            text: TextRenderer::new_with_font(font_family, font_size),
        }
    }

    /// Update font settings (e.g., after config reload).
    pub fn set_font(&mut self, font_family: &str, font_size: f32) {
        self.text.set_font_family(font_family);
        self.text.set_font_size(font_size);
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

        let mut pixmap = Pixmap::new(width, height)?;

        // Draw background
        if bar_config.background {
            let radius = bar_config.radius.max(0) as f32;
            fill_rounded_rect(
                &mut pixmap, 0.0, 0.0, width as f32, height as f32,
                radius, color_from_u32(theme.bg),
            );
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
        };

        // Separate modules by section
        let (left, center, right) = split_modules(&bar_config.modules);

        // Measure all modules
        let uses_pills = bar_config.module_style == BarModuleStyle::Pill;
        let pill_pad = if uses_pills { bar_config.padding_x.max(0) as u32 } else { 0 };
        let item_gap = bar_config.gap.max(0) as u32;
        let content_margin = bar_config.content_margin_x.max(0) as u32;

        // Compute the vertical content area (where pills sit, and where content is centered)
        let content_y = bar_config.content_margin_y.max(0);
        let content_h = (height as i32 - content_y * 2).max(1);

        // Layout LEFT section (left to right)
        let mut left_x = content_margin as i32;
        for module in &left {
            let w = self.measure_module(module, &mod_ctx, bar_config, ws_ctx) + pill_pad * 2;
            self.draw_module(
                &mut pixmap, module, &mod_ctx, bar_config, theme,
                left_x, w as i32, uses_pills, ws_ctx, content_y, content_h,
            );
            left_x += w as i32 + item_gap as i32;
        }

        // Layout RIGHT section (right to left)
        let mut right_x = width as i32 - content_margin as i32;
        for module in right.iter().rev() {
            let w = self.measure_module(module, &mod_ctx, bar_config, ws_ctx) + pill_pad * 2;
            right_x -= w as i32;
            self.draw_module(
                &mut pixmap, module, &mod_ctx, bar_config, theme,
                right_x, w as i32, uses_pills, ws_ctx, content_y, content_h,
            );
            right_x -= item_gap as i32;
        }

        // Layout CENTER section (centered, if it fits)
        if !center.is_empty() {
            let total_center_w: u32 = center.iter()
                .map(|m| self.measure_module(m, &mod_ctx, bar_config, ws_ctx) + pill_pad * 2)
                .sum::<u32>()
                + (center.len().saturating_sub(1) as u32) * item_gap;

            let safe_left = left_x;
            let safe_right = right_x;

            if (safe_right - safe_left) >= (total_center_w as i32 + 40) {
                let mut cx = ((width as i32 - total_center_w as i32) / 2)
                    .max(safe_left)
                    .min(safe_right - total_center_w as i32);

                for module in &center {
                    let w = self.measure_module(module, &mod_ctx, bar_config, ws_ctx) + pill_pad * 2;
                    self.draw_module(
                        &mut pixmap, module, &mod_ctx, bar_config, theme,
                        cx, w as i32, uses_pills, ws_ctx, content_y, content_h,
                    );
                    cx += w as i32 + item_gap as i32;
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
        _ws_ctx: &WorkspaceContext,
    ) -> u32 {
        if module.kind == BarModuleKind::Workspaces {
            return self.measure_workspaces(ctx);
        }

        let content = text::build_module_text(&module.kind, module.arg.as_deref(), ctx);
        if content.text.is_empty() {
            return 0;
        }

        let mut w = self.text.measure(&content.text);

        // Volume bar addition
        if module.kind == BarModuleKind::Volume && bar_config.volume_bar_enabled {
            w += 8 + bar_config.volume_bar_width.max(0) as u32;
        }

        w
    }

    fn measure_workspaces(&mut self, ctx: &ModuleContext) -> u32 {
        if ctx.icons {
            let dot_w = self.text.font_size as u32;
            let gap = (self.text.font_size * 0.4) as u32;
            dot_w * 9 + gap * 8
        } else {
            let digit_w = self.text.measure("0");
            let gap = (self.text.font_size * 0.3) as u32;
            digit_w * 9 + gap * 8
        }
    }

    /// Draw a single module at position x.
    /// content_y/content_h define the vertical content area (symmetric inset from bar edges).
    fn draw_module(
        &mut self,
        pixmap: &mut Pixmap,
        module: &BarModule,
        ctx: &ModuleContext,
        bar_config: &BarConfig,
        theme: &Theme,
        x: i32,
        width: i32,
        uses_pills: bool,
        ws_ctx: &WorkspaceContext,
        content_y: i32,
        content_h: i32,
    ) {
        if width <= 0 {
            return;
        }

        let bar_height = bar_config.height;
        let pill_pad = if uses_pills { bar_config.padding_x.max(0) } else { 0 };

        // Draw pill background (symmetric vertical inset)
        if uses_pills {
            let radius = bar_config.radius.max(0) as f32;
            draw_pill(pixmap, x as f32, content_y as f32, width as f32, content_h as f32, radius, theme);
        }

        let text_x = x + pill_pad;

        // Vertical centering: compute the center line for all content
        // In pill mode, center within the pill; in flat mode, center within full bar
        let center_y = if uses_pills {
            content_y as f32 + content_h as f32 / 2.0
        } else {
            bar_height as f32 / 2.0
        };

        // Special: Workspaces
        if module.kind == BarModuleKind::Workspaces {
            self.draw_workspaces(pixmap, text_x, center_y, ctx, theme, ws_ctx);
            return;
        }

        let content = text::build_module_text(&module.kind, module.arg.as_deref(), ctx);
        if content.text.is_empty() {
            return;
        }

        // Draw text — use center_y_offset to vertically center glyphs
        let text_y = self.text.center_y_offset(center_y);
        self.text.draw(pixmap, &content.text, text_x as f32, text_y, content.color);

        // Volume bar
        if module.kind == BarModuleKind::Volume && bar_config.volume_bar_enabled {
            let text_w = self.text.measure(&content.text) as i32;
            let bar_x = text_x + text_w + 8;
            let bar_w = bar_config.volume_bar_width.max(0);
            let bar_h = bar_config.volume_bar_height.max(0);
            let bar_y = center_y - bar_h as f32 / 2.0;
            let bar_r = bar_config.volume_bar_radius.max(0) as f32;
            draw_volume_bar(
                pixmap,
                bar_x as f32, bar_y, bar_w as f32, bar_h as f32, bar_r,
                &ctx.status.volume, theme,
            );
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

            if ctx.icons {
                let radius = if is_current { dot_size * 0.6 } else { dot_size * 0.45 };
                fill_circle(pixmap, cx + dot_size / 2.0, center_y, radius, color_from_u32(color));
                cx += dot_size + gap;
            } else {
                let digit = format!("{}", i + 1);
                let text_y = self.text.center_y_offset(center_y);
                self.text.draw(pixmap, &digit, cx, text_y, color);
                let digit_w = self.text.measure(&digit) as f32;
                cx += digit_w + gap;
            }
        }
    }
}

// ── Workspace context (provided by compositor) ──────────────────────

pub struct WorkspaceContext {
    pub current_ws: usize,
    pub occupied_workspaces: [bool; 9],
    pub focused_title: String,
    pub monitor_id: usize,
    pub sync_enabled: bool,
}

// ── Bar-specific drawing ────────────────────────────────────────────

fn draw_pill(pixmap: &mut Pixmap, x: f32, y: f32, w: f32, h: f32, radius: f32, theme: &Theme) {
    if w <= 0.0 || h <= 0.0 {
        return;
    }

    fill_rounded_rect(pixmap, x, y, w, h, radius, color_from_u32(theme.surface));
    stroke_rounded_rect(
        pixmap, x + 0.5, y + 0.5, w - 1.0, h - 1.0,
        (radius - 0.5).max(0.0), color_from_u32(theme.border), 1.0,
    );
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
        let fill_color = if volume.muted { theme.text_muted } else { theme.accent };
        fill_rounded_rect(pixmap, x, y, fill_w, h, radius, color_from_u32(fill_color));
    }
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
