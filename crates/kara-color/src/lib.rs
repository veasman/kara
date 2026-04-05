//! kara-color: color math library for the kara desktop environment.
//!
//! Hex parsing, HSL conversion, color mixing, lightening/darkening,
//! luminance, contrast ratio, hue shifting, and saturation.

use anyhow::{Result, anyhow, bail};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Color {
    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }

    pub fn from_hex(input: &str) -> Result<Self> {
        let s = input.trim().trim_start_matches('#');
        if s.len() != 6 {
            bail!("expected 6-digit hex color, got: {input}");
        }

        let r = u8::from_str_radix(&s[0..2], 16).map_err(|_| anyhow!("invalid hex: {input}"))?;
        let g = u8::from_str_radix(&s[2..4], 16).map_err(|_| anyhow!("invalid hex: {input}"))?;
        let b = u8::from_str_radix(&s[4..6], 16).map_err(|_| anyhow!("invalid hex: {input}"))?;
        Ok(Self { r, g, b })
    }

    pub fn to_hex(self) -> String {
        format!("#{:02x}{:02x}{:02x}", self.r, self.g, self.b)
    }

    pub fn mix(self, other: Self, t: f32) -> Self {
        let t = t.clamp(0.0, 1.0);
        let f = |a: u8, b: u8| -> u8 {
            ((a as f32) + ((b as f32) - (a as f32)) * t)
                .round()
                .clamp(0.0, 255.0) as u8
        };
        Self::new(f(self.r, other.r), f(self.g, other.g), f(self.b, other.b))
    }

    pub fn lighten(self, amount: f32) -> Self {
        self.mix(Self::new(255, 255, 255), amount)
    }

    pub fn darken(self, amount: f32) -> Self {
        self.mix(Self::new(0, 0, 0), amount)
    }

    pub fn luminance(self) -> f32 {
        fn channel(v: u8) -> f32 {
            let s = v as f32 / 255.0;
            if s <= 0.04045 {
                s / 12.92
            } else {
                ((s + 0.055) / 1.055).powf(2.4)
            }
        }

        let r = channel(self.r);
        let g = channel(self.g);
        let b = channel(self.b);
        0.2126 * r + 0.7152 * g + 0.0722 * b
    }

    pub fn contrast_ratio(self, other: Self) -> f32 {
        let l1 = self.luminance();
        let l2 = other.luminance();
        let (bright, dark) = if l1 > l2 { (l1, l2) } else { (l2, l1) };
        (bright + 0.05) / (dark + 0.05)
    }

    pub fn shift_hue(self, degrees: f32) -> Self {
        let (mut h, s, l) = self.to_hsl();
        h = (h + degrees / 360.0).rem_euclid(1.0);
        Self::from_hsl(h, s, l)
    }

    pub fn saturate(self, factor: f32) -> Self {
        let (h, s, l) = self.to_hsl();
        let s = (s * factor).clamp(0.0, 1.0);
        Self::from_hsl(h, s, l)
    }

    pub fn desaturate(self, amount: f32) -> Self {
        let (h, s, l) = self.to_hsl();
        let s = (s * (1.0 - amount.clamp(0.0, 1.0))).clamp(0.0, 1.0);
        Self::from_hsl(h, s, l)
    }

    fn to_hsl(self) -> (f32, f32, f32) {
        let r = self.r as f32 / 255.0;
        let g = self.g as f32 / 255.0;
        let b = self.b as f32 / 255.0;

        let max = r.max(g).max(b);
        let min = r.min(g).min(b);
        let l = (max + min) / 2.0;

        if (max - min).abs() < f32::EPSILON {
            return (0.0, 0.0, l);
        }

        let d = max - min;
        let s = d / (1.0 - (2.0 * l - 1.0).abs());

        let h = if (max - r).abs() < f32::EPSILON {
            ((g - b) / d).rem_euclid(6.0)
        } else if (max - g).abs() < f32::EPSILON {
            ((b - r) / d) + 2.0
        } else {
            ((r - g) / d) + 4.0
        } / 6.0;

        (h, s, l)
    }

    fn from_hsl(h: f32, s: f32, l: f32) -> Self {
        if s <= f32::EPSILON {
            let v = (l * 255.0).round().clamp(0.0, 255.0) as u8;
            return Self::new(v, v, v);
        }

        let q = if l < 0.5 {
            l * (1.0 + s)
        } else {
            l + s - l * s
        };
        let p = 2.0 * l - q;

        fn hue_to_rgb(p: f32, q: f32, t: f32) -> f32 {
            let t = t.rem_euclid(1.0);
            if t < 1.0 / 6.0 {
                p + (q - p) * 6.0 * t
            } else if t < 1.0 / 2.0 {
                q
            } else if t < 2.0 / 3.0 {
                p + (q - p) * (2.0 / 3.0 - t) * 6.0
            } else {
                p
            }
        }

        let r = hue_to_rgb(p, q, h + 1.0 / 3.0);
        let g = hue_to_rgb(p, q, h);
        let b = hue_to_rgb(p, q, h - 1.0 / 3.0);

        Self::new(
            (r * 255.0).round().clamp(0.0, 255.0) as u8,
            (g * 255.0).round().clamp(0.0, 255.0) as u8,
            (b * 255.0).round().clamp(0.0, 255.0) as u8,
        )
    }
}
