//! Winit backend — nested development mode.
//!
//! TEMPORARILY DISABLED as of DisplayLink M2.
//!
//! M2 converted the udev backend from a bare `GlesRenderer` to smithay's
//! `MultiRenderer` / `GpuManager` stack, and the `build_custom_elements` /
//! `build_keybind_overlay` / etc. helpers in `render.rs` now take a
//! `&mut KaraRenderer<'_>` parameter. The winit backend can't easily hand
//! those helpers a `KaraRenderer` because it doesn't open any DRM device
//! (the whole point of winit is to run nested inside another compositor).
//!
//! The user daily-drives the udev backend, so this stub just prints an error
//! and exits. Re-enabling winit would require either:
//!   1. Making the render helpers generic over any `Renderer + ImportMem`
//!      so both `GlesRenderer` and `MultiRenderer` implementations work; or
//!   2. Constructing a `GpuManager` fed from the winit renderer's EGL
//!      device so the helpers receive a real `KaraRenderer`.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use smithay::reexports::calloop::EventLoop;
use smithay::reexports::wayland_server::Display;

use crate::state::Gate;

pub fn run(
    _event_loop: EventLoop<Gate>,
    _display: &mut Display<Gate>,
    _socket_name: String,
    _signal_flag: Arc<AtomicBool>,
) {
    eprintln!(
        "kara-gate: winit backend is temporarily disabled (DisplayLink M2 refactor).\n\
         Run with KARA_BACKEND=udev or unset to use the default udev backend."
    );
    std::process::exit(1);
}
