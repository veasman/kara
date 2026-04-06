mod actions;
mod animation;
mod backend_udev;
mod backend_winit;
mod cursor;
mod input;
mod ipc;
mod layout;
mod render;
mod state;
mod wallpaper;
#[allow(dead_code)]
mod workspace;

use smithay::reexports::calloop::EventLoop;
use smithay::reexports::wayland_server::Display;
use smithay::wayland::socket::ListeningSocketSource;

use signal_hook::consts::{SIGHUP, SIGUSR1};

use crate::state::{ClientState, Gate};

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter("kara_gate=debug,smithay=info")
        .init();

    tracing::info!("starting kara-gate");

    let event_loop: EventLoop<Gate> =
        EventLoop::try_new().expect("failed to create event loop");

    let mut display: Display<Gate> = Display::new().expect("failed to create display");

    // Wayland socket
    let listening_socket = ListeningSocketSource::new_auto().expect("failed to bind socket");
    let socket_name = listening_socket.socket_name().to_string_lossy().to_string();
    tracing::info!("listening on WAYLAND_DISPLAY={}", socket_name);

    event_loop
        .handle()
        .insert_source(listening_socket, |client_stream, _, state: &mut Gate| {
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

    // Signal handling for config reload (SIGUSR1 / SIGHUP)
    let signal_flag = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    for sig in [SIGUSR1, SIGHUP] {
        signal_hook::flag::register(sig, std::sync::Arc::clone(&signal_flag))
            .expect("failed to register signal handler");
    }

    // Backend selection: KARA_BACKEND env var, or auto-detect
    let backend = std::env::var("KARA_BACKEND").unwrap_or_else(|_| {
        if std::env::var("WAYLAND_DISPLAY").is_ok() || std::env::var("DISPLAY").is_ok() {
            "winit".to_string()
        } else {
            "udev".to_string()
        }
    });

    tracing::info!("selected backend: {}", backend);

    match backend.as_str() {
        "winit" => backend_winit::run(event_loop, &mut display, socket_name, signal_flag),
        "udev" => backend_udev::run(event_loop, &mut display, socket_name, signal_flag),
        other => {
            tracing::error!("unknown backend: {other} (expected 'winit' or 'udev')");
            std::process::exit(1);
        }
    }
}
