//! Winit backend — nested development mode.
//!
//! Runs kara-gate inside an existing compositor or X11 WM via a winit window.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use smithay::backend::renderer::damage::OutputDamageTracker;
use smithay::backend::renderer::element::texture::TextureRenderElement;
use smithay::backend::renderer::gles::{GlesRenderer, GlesTexture};
use smithay::backend::winit;
use smithay::backend::winit::WinitEvent;
use smithay::desktop::space::render_output;
use smithay::output::{Mode, Output, PhysicalProperties, Subpixel};
use smithay::reexports::calloop::EventLoop;
use smithay::reexports::wayland_server::Display;
use smithay::utils::Transform;

use crate::render::build_custom_elements;
use crate::state::Gate;

pub fn run(
    mut event_loop: EventLoop<Gate>,
    display: &mut Display<Gate>,
    socket_name: String,
    signal_flag: Arc<AtomicBool>,
) {
    // Winit backend — init BEFORE setting WAYLAND_DISPLAY so winit uses X11,
    // not our own Wayland socket (which would deadlock).
    let (mut backend, mut winit_evt) =
        winit::init::<GlesRenderer>().expect("failed to init winit");

    // Now set WAYLAND_DISPLAY so child processes connect to us.
    unsafe { std::env::set_var("WAYLAND_DISPLAY", &socket_name) };

    // Output
    let output = Output::new(
        "winit".to_string(),
        PhysicalProperties {
            size: (0, 0).into(),
            subpixel: Subpixel::Unknown,
            make: "kara-gate".to_string(),
            model: "winit".to_string(),
        },
    );

    let size = backend.window_size();
    let mode = Mode { size, refresh: 60_000 };
    output.change_current_state(
        Some(mode),
        Some(Transform::Flipped180),
        None,
        Some((0, 0).into()),
    );
    output.set_preferred(mode);

    // Compositor state
    let mut state = Gate::new(display, event_loop.get_signal());
    state.space.map_output(&output, (0, 0));
    state.add_output(output.clone(), (size.w as i32, size.h as i32), (0, 0).into());

    let mut damage_tracker = OutputDamageTracker::from_output(&output);
    let mut last_status_refresh = Instant::now();

    // Initial status poll
    state.status_cache.refresh(true);

    tracing::info!("kara-gate ready [winit] ({}x{})", size.w, size.h);

    // Run autostart commands
    state.run_autostart();

    loop {
        // Winit events
        winit_evt.dispatch_new_events(|event| match event {
            WinitEvent::Resized { size, .. } => {
                let new_mode = Mode { size, refresh: 60_000 };
                output.change_current_state(Some(new_mode), None, None, None);
                damage_tracker = OutputDamageTracker::from_output(&output);
                state.set_output_size(0, size.w as i32, size.h as i32);
                state.apply_layout();
            }
            WinitEvent::CloseRequested => {
                state.running = false;
            }
            WinitEvent::Input(event) => {
                state.handle_input_event(event);
            }
            _ => {}
        });

        if !state.running {
            tracing::info!("shutting down");
            kara_ipc::server::cleanup_socket();
            break;
        }

        // Poll IPC
        state.poll_ipc();

        // Check for reload signal
        if signal_flag.swap(false, std::sync::atomic::Ordering::Relaxed) {
            state.reload_config();
        }

        // Refresh system status (~1s interval)
        let now = Instant::now();
        if now.duration_since(last_status_refresh) >= Duration::from_secs(1) {
            state.status_cache.refresh(false);
            state.bar_dirty = true;
            state.check_config_changed();
            last_status_refresh = now;
        }

        // Send frame callbacks to visible windows
        let time = state.clock.now();
        state.space.elements().for_each(|window| {
            window.send_frame(&output, time, Some(Duration::ZERO), |_, _| {
                Some(output.clone())
            });
        });

        // Render
        let (renderer, mut framebuffer) = backend.bind().expect("failed to bind");

        let mut custom_elements = build_custom_elements(&mut state, renderer, 0);
        custom_elements.extend(crate::render::build_scratchpad_overlay(&mut state, renderer, 0));

        render_output::<_, TextureRenderElement<GlesTexture>, _, _>(
            &output,
            renderer,
            &mut framebuffer,
            1.0,
            0,
            [&state.space],
            &custom_elements,
            &mut damage_tracker,
            [0.05, 0.05, 0.05, 1.0],
        )
        .ok();
        drop(framebuffer);
        backend.submit(None).expect("failed to submit");

        // Tick animations
        state.process_completed_animations();
        if state.animations.has_active() {
            state.apply_animation_offsets();
        }

        // Dispatch
        display.dispatch_clients(&mut state).unwrap();
        display.flush_clients().unwrap();

        event_loop
            .dispatch(Some(Duration::from_millis(16)), &mut state)
            .expect("event loop error");
    }
}
