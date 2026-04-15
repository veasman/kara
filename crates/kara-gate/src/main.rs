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
mod video;
mod wallpaper;
#[allow(dead_code)]
mod workspace;

use smithay::reexports::calloop::EventLoop;
use smithay::reexports::wayland_server::Display;
use smithay::wayland::socket::ListeningSocketSource;

use signal_hook::consts::{SIGHUP, SIGUSR1};

use crate::state::{ClientState, Gate};

/// Initialize tracing: write to stdout AND truncate-then-append to /tmp/kara-gate.log
/// so the log can be tailed from another terminal without restarting the compositor.
fn init_tracing() {
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;
    use tracing_subscriber::EnvFilter;

    let log_path = "/tmp/kara-gate.log";
    // Truncate on startup so each session starts with a clean file.
    let _ = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(log_path);
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path);

    let env_filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("kara_gate=debug,smithay=info"));

    let stdout_layer = tracing_subscriber::fmt::layer().with_writer(std::io::stdout);

    let registry = tracing_subscriber::registry()
        .with(env_filter)
        .with(stdout_layer);

    if let Ok(file) = file {
        let make_writer = move || file.try_clone().expect("clone kara log fd");
        let file_layer = tracing_subscriber::fmt::layer()
            .with_ansi(false)
            .with_writer(make_writer);
        registry.with(file_layer).init();
    } else {
        registry.init();
    }
}

fn main() {
    init_tracing();

    tracing::info!("starting kara-gate");
    tracing::info!("debug log: /tmp/kara-gate.log (tail -f from another terminal)");

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
