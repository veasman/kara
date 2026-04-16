//! Text rendering via cosmic-text.
//!
//! Wraps FontSystem + SwashCache with measure and draw operations.

use cosmic_text::{Attrs, Buffer, Family, FontSystem, Metrics, Shaping, SwashCache};
use tiny_skia::Pixmap;

use crate::canvas::{blit_color, blit_mask};

/// Text renderer wrapping cosmic-text font system and glyph cache.
pub struct TextRenderer {
    pub font_system: FontSystem,
    pub swash_cache: SwashCache,
    pub font_size: f32,
    pub line_height: f32,
    font_family: String,
}

impl TextRenderer {
    pub fn new(font_size: f32) -> Self {
        Self {
            font_system: FontSystem::new(),
            swash_cache: SwashCache::new(),
            font_size,
            line_height: font_size,
            font_family: String::new(),
        }
    }

    pub fn new_with_font(font_family: &str, font_size: f32) -> Self {
        Self {
            font_system: FontSystem::new(),
            swash_cache: SwashCache::new(),
            font_size,
            line_height: font_size,
            font_family: font_family.to_string(),
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
    pub fn center_y_offset(&mut self, center_y: f32) -> f32 {
        let metrics = Metrics::new(self.font_size, self.line_height);
        let mut buffer = Buffer::new(&mut self.font_system, metrics);
        let attrs = if self.font_family.is_empty() {
            Attrs::new().family(Family::SansSerif)
        } else {
            Attrs::new().family(Family::Name(&self.font_family))
        };
        buffer.set_text(&mut self.font_system, "Hg", &attrs, Shaping::Advanced, None);
        buffer.shape_until_scroll(&mut self.font_system, false);

        if let Some(run) = buffer.layout_runs().next() {
            // line_top = y offset to top of the line area (usually 0
            // for a single-line buffer). line_height = full vertical
            // extent of the line including leading. The visual center
            // of the line area is at line_top + line_height / 2.
            // Setting y so that visual center lands at center_y:
            let visual_center = run.line_top + run.line_height / 2.0;
            center_y - visual_center
        } else {
            center_y - self.font_size / 2.0
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
