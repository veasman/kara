//! Text rendering via cosmic-text.
//!
//! Wraps FontSystem + SwashCache with measure and draw operations.

use cosmic_text::{Attrs, Buffer, Family, FontSystem, Metrics, Shaping, SwashCache};
use tiny_skia::Pixmap;

use crate::canvas::{blit_color, blit_mask};

/// True for codepoints that nerd fonts use for icon glyphs. All nerd
/// font icon ranges (powerline, devicons, fontawesome, weather, seti,
/// octicons, material) live inside the Unicode Private Use Area
/// U+E000..=U+F8FF, so a single range check catches them all without
/// a lookup table.
pub fn is_icon_codepoint(c: char) -> bool {
    matches!(c as u32, 0xE000..=0xF8FF)
}

/// Walk a string and yield `(&str, is_icon)` runs of consecutive
/// icon or non-icon characters. Pure helper — no allocation beyond
/// the iterator state.
fn split_icon_runs(text: &str) -> impl Iterator<Item = (&str, bool)> {
    let mut chars = text.char_indices().peekable();
    std::iter::from_fn(move || {
        let (start, first) = chars.next()?;
        let kind = is_icon_codepoint(first);
        let mut end = start + first.len_utf8();
        while let Some(&(i, c)) = chars.peek() {
            if is_icon_codepoint(c) != kind {
                break;
            }
            chars.next();
            end = i + c.len_utf8();
        }
        Some((&text[start..end], kind))
    })
}

/// Text renderer wrapping cosmic-text font system and glyph cache.
pub struct TextRenderer {
    pub font_system: FontSystem,
    pub swash_cache: SwashCache,
    pub font_size: f32,
    pub line_height: f32,
    font_family: String,
    /// Glyph-bbox center offsets, keyed by (font_family, font_size
    /// bits). Memoizing here is critical for the bar's mixed-size
    /// rendering path: `draw_row` / `measure_row` swap `font_size`
    /// between the text and icon sizes a handful of times per module
    /// per frame, and without a persistent cache each swap would
    /// reshape `"Hg"` through cosmic-text and rasterize both glyphs
    /// just to recompute the same centering offset.
    cached_center_offsets: std::collections::HashMap<(String, u32), f32>,
}

impl TextRenderer {
    pub fn new(font_size: f32) -> Self {
        Self {
            font_system: FontSystem::new(),
            swash_cache: SwashCache::new(),
            font_size,
            line_height: font_size,
            font_family: String::new(),
            cached_center_offsets: std::collections::HashMap::new(),
        }
    }

    pub fn new_with_font(font_family: &str, font_size: f32) -> Self {
        Self {
            font_system: FontSystem::new(),
            swash_cache: SwashCache::new(),
            font_size,
            line_height: font_size,
            font_family: font_family.to_string(),
            cached_center_offsets: std::collections::HashMap::new(),
        }
    }

    pub fn set_font_size(&mut self, size: f32) {
        self.font_size = size;
        self.line_height = size;
    }

    pub fn set_font_family(&mut self, family: &str) {
        self.font_family = family.to_string();
    }

    /// Compute the y offset needed so that text is vertically
    /// centered at `center_y`. Uses actual font metrics from
    /// cosmic-text (max_ascent + max_descent from a shaped "Hg"
    /// reference line) instead of a hardcoded constant. The shaped
    /// result is cheap — cosmic-text caches shaped runs internally.
    ///
    /// Centering strategy: the visible glyph span from top of
    /// ascenders to bottom of descenders is `ascent + descent`.
    /// The visual center of that span relative to the line top is
    /// `ascent - (ascent + descent) / 2 = (ascent - descent) / 2`.
    /// To place that at `center_y`: `y = center_y - (ascent - descent) / 2`.
    /// Compute the y draw-offset so that text renders vertically
    /// centered at `center_y`. Shapes a reference string ("Hg" —
    /// has both ascenders and descenders), rasterizes glyph
    /// placements to find the actual pixel top and bottom, then
    /// computes the offset that centers that bbox at `center_y`.
    ///
    /// This is font-agnostic: no hardcoded constants, no assumed
    /// metrics. Any font at any size produces correct centering.
    pub fn center_y_offset(&mut self, center_y: f32) -> f32 {
        // Cache the glyph-bbox offset — it only depends on font
        // family + size, not on center_y or the specific text. The
        // expensive shaping + glyph-image walk runs once per
        // (font_family, font_size) combination for the process
        // lifetime, so the bar's draw_row pattern (which swaps
        // font_size between text and icon sizes several times per
        // module) stays in cache after the first render.
        let key = (self.font_family.clone(), self.font_size.to_bits());
        if let Some(off) = self.cached_center_offsets.get(&key) {
            return center_y - *off;
        }
        let off = self.compute_glyph_center_offset();
        self.cached_center_offsets.insert(key, off);
        center_y - off
    }

    fn compute_glyph_center_offset(&mut self) -> f32 {
        let metrics = Metrics::new(self.font_size, self.line_height);
        let mut buffer = Buffer::new(&mut self.font_system, metrics);
        let attrs = if self.font_family.is_empty() {
            Attrs::new().family(Family::SansSerif)
        } else {
            Attrs::new().family(Family::Name(&self.font_family))
        };
        buffer.set_text(&mut self.font_system, "Hg", &attrs, Shaping::Advanced, None);
        buffer.shape_until_scroll(&mut self.font_system, false);

        let mut min_gy = i32::MAX;
        let mut max_gy_bottom = i32::MIN;
        for run in buffer.layout_runs() {
            for glyph in run.glyphs.iter() {
                let physical = glyph.physical((0.0, 0.0), 1.0);
                if let Some(image) = self.swash_cache.get_image(
                    &mut self.font_system,
                    physical.cache_key,
                ) {
                    if image.placement.height == 0 {
                        continue;
                    }
                    let gy = physical.y - image.placement.top;
                    let gy_bottom = gy + image.placement.height as i32;
                    min_gy = min_gy.min(gy);
                    max_gy_bottom = max_gy_bottom.max(gy_bottom);
                }
            }
        }

        if min_gy < max_gy_bottom {
            (min_gy + max_gy_bottom) as f32 / 2.0
        } else {
            self.font_size / 2.0
        }
    }

    /// Measure the pixel width of a text string.
    pub fn measure(&mut self, text: &str) -> u32 {
        if text.is_empty() {
            return 0;
        }

        let metrics = Metrics::new(self.font_size, self.line_height);
        let mut buffer = Buffer::new(&mut self.font_system, metrics);

        let attrs = if self.font_family.is_empty() {
            Attrs::new().family(Family::SansSerif)
        } else {
            Attrs::new().family(Family::Name(&self.font_family))
        };
        buffer.set_text(&mut self.font_system, text, &attrs, Shaping::Advanced, None);
        buffer.shape_until_scroll(&mut self.font_system, false);

        buffer
            .layout_runs()
            .map(|run| run.line_w)
            .fold(0.0_f32, f32::max)
            .ceil() as u32
    }

    /// Measure a string that mixes regular text and nerd-font icons,
    /// rendering each icon-codepoint run at `icon_size` and text runs
    /// at `self.font_size`. Some fonts (e.g. 3270 Nerd Font Mono)
    /// design icon glyphs smaller than their text glyphs so an
    /// icon_size > font_size is often needed to match visual weight.
    ///
    /// Fast-paths: if the text contains no icon codepoints, or the
    /// caller passed `icon_size == self.font_size`, skip the segment
    /// split and delegate straight to `measure` — avoids one extra
    /// Buffer shape per call that the caller doesn't need.
    pub fn measure_row(&mut self, text: &str, icon_size: f32) -> u32 {
        if text.is_empty() {
            return 0;
        }
        if icon_size == self.font_size || !text.chars().any(is_icon_codepoint) {
            return self.measure(text);
        }
        let base_size = self.font_size;
        let mut total = 0.0f32;
        for (segment, is_icon) in split_icon_runs(text) {
            let size = if is_icon { icon_size } else { base_size };
            if size != self.font_size {
                self.set_font_size(size);
            }
            total += self.measure(segment) as f32;
        }
        if self.font_size != base_size {
            self.set_font_size(base_size);
        }
        total.ceil() as u32
    }

    /// Draw a string that mixes regular text and nerd-font icons,
    /// centering each run on `center_y`. Icon-codepoint runs render
    /// at `icon_size`; text runs at `self.font_size`. Returns the
    /// total advance in pixels.
    ///
    /// Fast-paths analogous to `measure_row`.
    pub fn draw_row(
        &mut self,
        pixmap: &mut Pixmap,
        text: &str,
        x: f32,
        center_y: f32,
        color: u32,
        icon_size: f32,
    ) -> f32 {
        if text.is_empty() {
            return 0.0;
        }
        if icon_size == self.font_size || !text.chars().any(is_icon_codepoint) {
            let y = self.center_y_offset(center_y);
            self.draw(pixmap, text, x, y, color);
            return self.measure(text) as f32;
        }
        let base_size = self.font_size;
        let mut cx = x;
        for (segment, is_icon) in split_icon_runs(text) {
            let size = if is_icon { icon_size } else { base_size };
            if size != self.font_size {
                self.set_font_size(size);
            }
            let y = self.center_y_offset(center_y);
            self.draw(pixmap, segment, cx, y, color);
            cx += self.measure(segment) as f32;
        }
        if self.font_size != base_size {
            self.set_font_size(base_size);
        }
        cx - x
    }

    /// Draw text onto a pixmap at (x, y) with a 0xRRGGBB color.
    pub fn draw(&mut self, pixmap: &mut Pixmap, text: &str, x: f32, y: f32, color: u32) {
        if text.is_empty() {
            return;
        }

        let metrics = Metrics::new(self.font_size, self.line_height);
        let mut buffer = Buffer::new(&mut self.font_system, metrics);

        let attrs = if self.font_family.is_empty() {
            Attrs::new().family(Family::SansSerif)
        } else {
            Attrs::new().family(Family::Name(&self.font_family))
        };
        buffer.set_text(&mut self.font_system, text, &attrs, Shaping::Advanced, None);
        buffer.shape_until_scroll(&mut self.font_system, false);

        let r = ((color >> 16) & 0xFF) as u8;
        let g = ((color >> 8) & 0xFF) as u8;
        let b = (color & 0xFF) as u8;

        for run in buffer.layout_runs() {
            for glyph in run.glyphs.iter() {
                let physical = glyph.physical((x, y), 1.0);

                let image = match self.swash_cache.get_image(
                    &mut self.font_system,
                    physical.cache_key,
                ) {
                    Some(img) => img,
                    None => continue,
                };

                if image.placement.width == 0 || image.placement.height == 0 {
                    continue;
                }

                let gx = physical.x + image.placement.left;
                let gy = physical.y - image.placement.top;

                match image.content {
                    cosmic_text::SwashContent::Mask => {
                        blit_mask(
                            pixmap, &image.data,
                            image.placement.width as u32, image.placement.height as u32,
                            gx, gy, r, g, b,
                        );
                    }
                    cosmic_text::SwashContent::Color => {
                        blit_color(
                            pixmap, &image.data,
                            image.placement.width as u32, image.placement.height as u32,
                            gx, gy,
                        );
                    }
                    cosmic_text::SwashContent::SubpixelMask => {
                        blit_mask(
                            pixmap, &image.data,
                            image.placement.width as u32, image.placement.height as u32,
                            gx, gy, r, g, b,
                        );
                    }
                }
            }
        }
    }
}
