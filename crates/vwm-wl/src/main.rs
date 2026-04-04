mod state;

use std::time::Duration;

use smithay::backend::renderer::damage::OutputDamageTracker;
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::backend::renderer::element::surface::WaylandSurfaceRenderElement;
use smithay::backend::winit;
use smithay::backend::winit::WinitEvent;
use smithay::desktop::space::render_output;
use smithay::desktop::Window;
use smithay::output::Mode;
use smithay::output::Output;
use smithay::output::PhysicalProperties;
use smithay::output::Subpixel;
use smithay::reexports::calloop::EventLoop;
use smithay::reexports::wayland_server::Display;
use smithay::utils::Transform;
use smithay::wayland::socket::ListeningSocketSource;

use crate::state::ClientState;
use crate::state::Vwm;

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter("vwm_wl=debug,smithay=info")
        .init();

    tracing::info!("starting vwm-wl");

    let mut event_loop: EventLoop<Vwm> =
        EventLoop::try_new().expect("failed to create event loop");

    let mut display: Display<Vwm> = Display::new().expect("failed to create display");

    // Set up Wayland socket via calloop source
    let listening_socket = ListeningSocketSource::new_auto().expect("failed to bind socket");
    let socket_name = listening_socket.socket_name().to_string_lossy().to_string();
    tracing::info!("listening on WAYLAND_DISPLAY={}", socket_name);
    // Safety: we set this before spawning any threads
    unsafe { std::env::set_var("WAYLAND_DISPLAY", &socket_name) };

    let display_handle = display.handle();
    event_loop
        .handle()
        .insert_source(listening_socket, move |client_stream, _, state: &mut Vwm| {
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
        .expect("failed to insert listening socket source");

    // Initialize winit backend
    let (mut backend, mut winit_evt) =
        winit::init::<GlesRenderer>().expect("failed to init winit backend");

    // Create output for the winit window
    let output = Output::new(
        "winit".to_string(),
        PhysicalProperties {
            size: (0, 0).into(),
            subpixel: Subpixel::Unknown,
            make: "vwm-wl".to_string(),
            model: "winit".to_string(),
        },
    );

    let mode = Mode {
        size: backend.window_size(),
        refresh: 60_000,
    };
    output.change_current_state(Some(mode), Some(Transform::Flipped180), None, Some((0, 0).into()));
    output.set_preferred(mode);

    let mut state = Vwm::new(&display, event_loop.get_signal());
    state.space.map_output(&output, (0, 0));

    let mut damage_tracker = OutputDamageTracker::from_output(&output);

    tracing::info!("vwm-wl running");

    loop {
        // Pump winit events
        winit_evt.dispatch_new_events(|event| match event {
            WinitEvent::Resized { size, .. } => {
                let new_mode = Mode {
                    size,
                    refresh: 60_000,
                };
                output.change_current_state(Some(new_mode), None, None, None);
                damage_tracker = OutputDamageTracker::from_output(&output);
            }
            WinitEvent::CloseRequested => {
                state.running = false;
            }
            WinitEvent::Input(event) => {
                let _ = event;
            }
            _ => {}
        });

        if !state.running {
            tracing::info!("shutting down");
            break;
        }

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

        // Dispatch wayland clients
        display.dispatch_clients(&mut state).unwrap();
        display.flush_clients().unwrap();

        // Dispatch calloop
        event_loop
            .dispatch(Some(Duration::from_millis(16)), &mut state)
            .expect("event loop error");
    }
}
