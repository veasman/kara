//! Box-blur primitive shared across kara tools.
//!
//! Separated into its own module so kara-veil (lock), future kara-gate
//! bar/picker blur fallback paths, and any one-shot screenshot blur can
//! use the same implementation. RGBA8 premultiplied-alpha input is
//! expected (matches tiny-skia Pixmap buffer layout).

/// Box-blur an RGBA8 buffer in place.
///
/// `radius` is the half-window size — a radius of 5 blurs each pixel
/// with the 11×11 kernel centered on it via two separable passes. For
/// full-screen desktop backdrops a radius of ~20 after downsampling
/// gives a strong "frosted glass" look that still runs fast.
///
/// `scratch` must be the same length as `buf`. Reusing a persistent
/// scratch across calls avoids per-frame allocation.
pub fn box_blur_rgba(buf: &mut [u8], w: usize, h: usize, radius: usize, scratch: &mut [u8]) {
    if radius == 0 || w == 0 || h == 0 {
        return;
    }
    debug_assert_eq!(scratch.len(), buf.len());
    let diam = radius * 2 + 1;
    let tmp = scratch;

    // Horizontal pass buf → tmp.
    for y in 0..h {
        let row = y * w * 4;
        for x in 0..w {
            let mut r = 0u32;
            let mut g = 0u32;
            let mut b = 0u32;
            let mut a = 0u32;
            for dx in 0..diam {
                let sx = (x + dx).saturating_sub(radius).min(w - 1);
                let i = row + sx * 4;
                r += buf[i] as u32;
                g += buf[i + 1] as u32;
                b += buf[i + 2] as u32;
                a += buf[i + 3] as u32;
            }
            let i = row + x * 4;
            tmp[i] = (r / diam as u32) as u8;
            tmp[i + 1] = (g / diam as u32) as u8;
            tmp[i + 2] = (b / diam as u32) as u8;
            tmp[i + 3] = (a / diam as u32) as u8;
        }
    }

    // Vertical pass tmp → buf.
    for x in 0..w {
        for y in 0..h {
            let mut r = 0u32;
            let mut g = 0u32;
            let mut b = 0u32;
            let mut a = 0u32;
            for dy in 0..diam {
                let sy = (y + dy).saturating_sub(radius).min(h - 1);
                let i = (sy * w + x) * 4;
                r += tmp[i] as u32;
                g += tmp[i + 1] as u32;
                b += tmp[i + 2] as u32;
                a += tmp[i + 3] as u32;
            }
            let i = (y * w + x) * 4;
            buf[i] = (r / diam as u32) as u8;
            buf[i + 1] = (g / diam as u32) as u8;
            buf[i + 2] = (b / diam as u32) as u8;
            buf[i + 3] = (a / diam as u32) as u8;
        }
    }
}
