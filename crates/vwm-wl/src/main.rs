mod actions;
mod input;
mod layout;
mod state;
mod workspace;

use std::time::Duration;

use smithay::backend::renderer::damage::OutputDamageTracker;
use smithay::backend::renderer::element::surface::WaylandSurfaceRenderElement;
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::backend::winit;
use smithay::backend::winit::WinitEvent;
use smithay::desktop::space::render_output;
use smithay::output::{Mode, Output, PhysicalProperties, Subpixel};
use smithay::reexports::calloop::EventLoop;
use smithay::reexports::wayland_server::Display;
use smithay::utils::{Rectangle, Transform};
use smithay::wayland::socket::ListeningSocketSource;

use crate::state::{ClientState, Vwm};

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter("vwm_wl=debug,smithay=info")
        .init();

    tracing::info!("starting vwm-wl");

    let mut event_loop: EventLoop<Vwm> =
        EventLoop::try_new().expect("failed to create event loop");

    let mut display: Display<Vwm> = Display::new().expect("failed to create display");

    // Wayland socket
    let listening_socket = ListeningSocketSource::new_auto().expect("failed to bind socket");
    let socket_name = listening_socket.socket_name().to_string_lossy().to_string();
    tracing::info!("listening on WAYLAND_DISPLAY={}", socket_name);
    unsafe { std::env::set_var("WAYLAND_DISPLAY", &socket_name) };

    event_loop
        .handle()
        .insert_source(listening_socket, |client_stream, _, state: &mut Vwm| {
            state
                .display_handle
                .insert_client(
                    client_stream,
                    std::sync::Arc::new(ClientState {
                        compositor: Default::default(),
                    }),
                )
                .unwrap();
        })
        .expect("failed to insert socket source");

    // Winit backend
    let (mut backend, mut winit_evt) =
        winit::init::<GlesRenderer>().expect("failed to init winit");

    // Output
    let output = Output::new(
        "winit".to_string(),
        PhysicalProperties {
            size: (0, 0).into(),
            subpixel: Subpixel::Unknown,
            make: "vwm-wl".to_string(),
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
    let mut state = Vwm::new(&display, event_loop.get_signal());
    state.space.map_output(&output, (0, 0));
    state.set_workarea(Rectangle::from_loc_and_size(
        (0, 0),
        (size.w as i32, size.h as i32),
    ));

    let mut damage_tracker = OutputDamageTracker::from_output(&output);

    tracing::info!("vwm-wl ready ({}x{})", size.w, size.h);

    loop {
        // Winit events
        winit_evt.dispatch_new_events(|event| match event {
            WinitEvent::Resized { size, .. } => {
                let new_mode = Mode { size, refresh: 60_000 };
                output.change_current_state(Some(new_mode), None, None, None);
                damage_tracker = OutputDamageTracker::from_output(&output);
                state.set_workarea(Rectangle::from_loc_and_size(
                    (0, 0),
                    (size.w as i32, size.h as i32),
                ));
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
            break;
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
        render_output::<_, WaylandSurfaceRenderElement<GlesRenderer>, _, _>(
            &output,
            renderer,
            &mut framebuffer,
            1.0,
            0,
            [&state.space],
            &[] as &[WaylandSurfaceRenderElement<GlesRenderer>],
            &mut damage_tracker,
            [0.05, 0.05, 0.05, 1.0],
        )
        .ok();
        drop(framebuffer);
        backend.submit(None).expect("failed to submit");

        // Dispatch
        display.dispatch_clients(&mut state).unwrap();
        display.flush_clients().unwrap();

        event_loop
            .dispatch(Some(Duration::from_millis(16)), &mut state)
            .expect("event loop error");
    }
}
