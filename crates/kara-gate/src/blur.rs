//! Blur / effects pipeline scaffolding.
//!
//! This is M1 of the plan in `~/.claude/plans/kara-blur-pipeline.md`:
//! a minimal foothold on the GLES context from the udev backend, gated
//! behind `KARA_BLUR_TWO_PASS=1` so it can't regress normal rendering.
//!
//! The pre-pass creates a fresh offscreen `GlesTexture` every frame,
//! binds it as the render target, clears it, and drops it. The result
//! is discarded — the whole point of M1 is to confirm that tearing
//! into `MultiRenderer::bind` from the udev render loop is safe and
//! doesn't wreck `DrmCompositor::render_frame`'s subsequent bind.
//!
//! M2 will cache the texture across frames and actually render the
//! backdrop subset of elements into it. M3 adds the kawase shader
//! pass and wires `bar.blur` / `module_blur`. M4 adds scratchpad blur
//! and lock-screen blur via the same primitive. See the plan for the
//! full breakdown.

use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::gles::GlesTexture;
use smithay::backend::renderer::{Bind, Color32F, Frame, Offscreen, Renderer};
use smithay::utils::{Rectangle, Size, Transform};

use crate::backend_udev::KaraRenderer;

/// M1 pre-pass: validate the offscreen-bind plumbing on each frame
/// without using the result.
///
/// Returns `Ok(())` on success. Errors are logged and swallowed so a
/// transient allocator failure doesn't take down the frame — the main
/// render path still runs after this call regardless.
///
/// Gated by the caller via `KARA_BLUR_TWO_PASS`; this helper does not
/// check the env var itself.
pub fn pre_pass(renderer: &mut KaraRenderer<'_>, size: (i32, i32)) {
    let (w, h) = size;
    if w <= 0 || h <= 0 {
        return;
    }

    // Offscreen GlesTexture, freshly allocated each frame. M2 will
    // cache this per output — for M1 we just prove the allocation
    // and bind cycle works on MultiRenderer<KaraApi, KaraApi>.
    let mut texture: GlesTexture =
        match Offscreen::<GlesTexture>::create_buffer(renderer, Fourcc::Abgr8888, Size::from((w, h))) {
            Ok(t) => t,
            Err(e) => {
                tracing::warn!("blur::pre_pass: create_buffer failed: {e:?}");
                return;
            }
        };

    // Bind the texture as the render target. The returned `target`
    // holds the binding; dropping it unbinds and restores whatever
    // the renderer had before. The DrmCompositor::render_frame call
    // immediately after this helper rebinds its own dmabuf target,
    // so as long as our drop happens before that call we're safe.
    let mut target = match renderer.bind(&mut texture) {
        Ok(t) => t,
        Err(e) => {
            tracing::warn!("blur::pre_pass: bind failed: {e:?}");
            return;
        }
    };

    // Scoped frame so `frame` drops before `target` drops. Clear to
    // a distinct color so if something down the line ends up sampling
    // this texture by accident it's visually obvious.
    {
        let physical_size: Size<i32, smithay::utils::Physical> = (w, h).into();
        let mut frame = match renderer.render(&mut target, physical_size, Transform::Normal) {
            Ok(f) => f,
            Err(e) => {
                tracing::warn!("blur::pre_pass: render start failed: {e:?}");
                return;
            }
        };
        let rect = Rectangle::from_size(physical_size);
        // Bright magenta is a debugging tell — if we ever see it on
        // screen, we know someone's sampling the pre-pass texture
        // without a blur shader in front of it.
        let _ = frame.clear(Color32F::from([1.0, 0.0, 1.0, 1.0]), &[rect]);
    }

    // Drop target → unbind. Explicit for clarity; the compiler would
    // drop it at the end of the function anyway, but we want the
    // unbind to happen BEFORE DrmCompositor::render_frame runs, and
    // the caller dispatches that immediately after pre_pass returns.
    drop(target);
}
