//! Scratchpad backdrop blur pipeline.
//!
//! When any visible scratchpad on an output has `blur true` in its config,
//! kara runs a three-step render pass before the main DrmCompositor frame:
//!
//! 1. **Backdrop capture** — bind a plain `GlesTexture` as the render target
//!    via the MultiRenderer frame, draw the subset of elements that sit
//!    below the scratchpad dim (workspace windows + custom textures:
//!    wallpaper, bar, workspace borders), unbind.
//! 2. **Horizontal Gaussian** — reach through to the inner `GlesRenderer`,
//!    bind a second `GlesTexture` as the target, and blit the backdrop
//!    through a custom separable-Gaussian fragment shader with direction
//!    `(1/w, 0)`.
//! 3. **Vertical Gaussian** — bind a persistent GBM-backed `Dmabuf` as the
//!    target, blit the horizontal result through the same shader with
//!    direction `(0, 1/h)`. The dmabuf exists so it can be imported as a
//!    `MultiTexture` afterwards and wrapped in a `TextureRenderElement`
//!    that the main `DrmCompositor::render_frame` call can consume.
//!
//! After these three passes the driver removes the workspace+custom
//! elements from the main element vec (they're already baked into the
//! blurred backdrop) and appends a single `DrmRenderElement::Texture`
//! wrapping the imported pong dmabuf. The existing scratchpad dim rect
//! still sits above this blurred backdrop and provides the darkening
//! multiplicand, so the visual result is "blurred wallpaper+windows +
//! flat dim tint", which is the scratchpad look.
//!
//! Outputs that use the two-pass rotation path (`OutputInstance.two_pass`
//! is `Some`) skip the blur pipeline entirely and render a flat dim —
//! combining both pipelines on the same output would need another round
//! of offscreen indirection and isn't worth the complexity for M2.
//!
//! Falls back silently to the flat dim if the shader fails to compile,
//! a texture allocation fails, or a pass errors. A rate-limited error
//! log fires on each failure path so regressions surface.

use smithay::backend::allocator::dmabuf::{AsDmabuf, Dmabuf};
use smithay::backend::allocator::gbm::{GbmAllocator, GbmBufferFlags, GbmDevice};
use smithay::backend::allocator::{Allocator, Fourcc, Modifier};
use smithay::backend::drm::DrmDeviceFd;
use smithay::backend::renderer::gles::{
    GlesTexProgram, GlesTexture, UniformName, UniformType,
};
use smithay::utils::{Physical, Size};

/// Compiled-once fragment-shader program for the separable Gaussian blur.
///
/// Lives on `Gate`, shared across outputs. The three states keep us from
/// retrying a failed compile on every frame.
pub enum BlurProgram {
    /// Shader hasn't been touched yet. First blur-enabled frame transitions
    /// this to `Compiled` or `Failed`.
    Uninit,
    /// Compile succeeded. The program is used for both the horizontal and
    /// vertical passes — only the `direction` uniform changes between them.
    Compiled(GlesTexProgram),
    /// Compile failed once. Stays failed forever (no retry). Blur falls
    /// back to flat dim on every subsequent frame.
    Failed,
}

impl BlurProgram {
    pub const fn new() -> Self {
        Self::Uninit
    }

    /// Borrow the compiled program, triggering a compile on first call.
    /// Returns `None` if the program is failed or failed to compile now.
    pub fn get_or_compile(
        &mut self,
        renderer: &mut smithay::backend::renderer::gles::GlesRenderer,
    ) -> Option<&GlesTexProgram> {
        match self {
            Self::Compiled(p) => Some(p),
            Self::Failed => None,
            Self::Uninit => {
                match renderer.compile_custom_texture_shader(
                    BLUR_SHADER_SRC,
                    &[
                        UniformName::new("direction", UniformType::_2f),
                        UniformName::new("spread", UniformType::_1f),
                    ],
                ) {
                    Ok(program) => {
                        tracing::info!("blur: Gaussian shader compiled");
                        *self = Self::Compiled(program);
                        match self {
                            Self::Compiled(p) => Some(p),
                            _ => unreachable!(),
                        }
                    }
                    Err(e) => {
                        tracing::error!("blur: shader compile failed: {e:?}");
                        *self = Self::Failed;
                        None
                    }
                }
            }
        }
    }
}

impl Default for BlurProgram {
    fn default() -> Self {
        Self::new()
    }
}

/// Per-output blur working set.
///
/// Allocated eagerly for non-rotated outputs (rotated outputs skip blur),
/// because the pong dmabuf needs `primary_gbm` which is only in scope at
/// init. The intermediate GlesTextures are allocated lazily on the first
/// blur-enabled frame so outputs that never see a blurred scratchpad
/// never pay the renderer-side texture cost.
pub struct BlurState {
    /// Final blur target. Render target for the vertical pass. Lives as a
    /// GBM dmabuf because kara's main render element type wraps
    /// `MultiTexture`, and the only way to get a `MultiTexture` from a
    /// self-rendered texture is to round-trip through dmabuf+import.
    pub pong_dmabuf: Dmabuf,
    /// Physical pixel size of every buffer in this BlurState — backdrop,
    /// ping, and pong all match. Equals the output's scanout size at init;
    /// if the output resizes we currently keep using the old size until
    /// next kara-gate restart (M2 doesn't handle live resize).
    pub size: Size<i32, Physical>,
    /// Scratch texture: captures the backdrop element subset each frame.
    /// Read by the horizontal pass. Allocated on first use.
    pub backdrop: Option<GlesTexture>,
    /// Scratch texture: horizontal pass output, vertical pass input.
    /// Allocated on first use.
    pub ping: Option<GlesTexture>,
}

impl BlurState {
    /// Allocate the persistent pong dmabuf from primary_gbm. Called at
    /// OutputInstance construction time. Returns `None` on allocation
    /// failure — blur gracefully degrades for that output.
    pub fn try_new(
        gbm: &GbmDevice<DrmDeviceFd>,
        size: Size<i32, Physical>,
    ) -> Option<Self> {
        if size.w <= 0 || size.h <= 0 {
            return None;
        }
        let mut allocator = GbmAllocator::new(gbm.clone(), GbmBufferFlags::RENDERING);
        let buffer = match allocator.create_buffer(
            size.w as u32,
            size.h as u32,
            Fourcc::Abgr8888,
            &[Modifier::Linear],
        ) {
            Ok(b) => b,
            Err(e) => {
                tracing::warn!("blur: pong allocation failed ({}x{}): {e:?}", size.w, size.h);
                return None;
            }
        };
        let dmabuf = match buffer.export() {
            Ok(d) => d,
            Err(e) => {
                tracing::warn!("blur: pong dmabuf export failed: {e:?}");
                return None;
            }
        };
        Some(Self {
            pong_dmabuf: dmabuf,
            size,
            backdrop: None,
            ping: None,
        })
    }
}

/// Fragment shader source embedded at compile time. See `shaders/blur.frag`
/// for the actual GLSL. Separable 9-tap Gaussian parameterized by
/// `direction` (texel-space step vector, `(1/w, 0)` or `(0, 1/h)`) and
/// `spread` (multiplier to widen the effective kernel at 9-tap cost).
pub const BLUR_SHADER_SRC: &str = include_str!("shaders/blur.frag");
