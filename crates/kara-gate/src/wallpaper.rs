//! Wallpaper rendering — loads an image and provides it as RGBA pixel data
//! for uploading as a GlesTexture.

use std::path::Path;

use smithay::backend::renderer::gles::{GlesRenderer, GlesTexture};
use smithay::backend::renderer::element::texture::TextureBuffer;
use smithay::backend::allocator::Fourcc;
use smithay::utils::{Size, Transform};

/// Loaded wallpaper ready for GPU upload.
pub struct Wallpaper {
    pub rgba: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

impl Wallpaper {
    /// Load an image file and convert to RGBA.
    pub fn load(path: &Path) -> Option<Self> {
        let img = image::open(path)
            .map_err(|e| tracing::error!("failed to load wallpaper '{}': {e}", path.display()))
            .ok()?;

        let rgba_img = img.to_rgba8();
        let width = rgba_img.width();
        let height = rgba_img.height();

        // Convert RGBA to premultiplied RGBA for tiny-skia/GL compatibility
        let mut data = rgba_img.into_raw();
        for pixel in data.chunks_exact_mut(4) {
            let a = pixel[3] as u32;
            pixel[0] = ((pixel[0] as u32 * a) / 255) as u8;
            pixel[1] = ((pixel[1] as u32 * a) / 255) as u8;
            pixel[2] = ((pixel[2] as u32 * a) / 255) as u8;
        }

        Some(Self {
            rgba: data,
            width,
            height,
        })
    }

    /// Upload as a GlesTexture.
    pub fn upload(&self, renderer: &mut GlesRenderer) -> Option<TextureBuffer<GlesTexture>> {
        TextureBuffer::from_memory(
            renderer,
            &self.rgba,
            Fourcc::Abgr8888,
            Size::from((self.width as i32, self.height as i32)),
            false,
            1,
            Transform::Normal,
            None,
        )
        .map_err(|e| tracing::error!("failed to upload wallpaper texture: {e:?}"))
        .ok()
    }
}
