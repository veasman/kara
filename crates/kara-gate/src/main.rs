mod actions;
mod input;
mod ipc;
mod layout;
mod state;
mod wallpaper;
mod workspace;

use std::time::{Duration, Instant};

use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::damage::OutputDamageTracker;
use smithay::backend::renderer::element::texture::{TextureBuffer, TextureRenderElement};
use smithay::backend::renderer::element::Kind;
use smithay::backend::renderer::gles::{GlesRenderer, GlesTexture};
use smithay::backend::winit;
use smithay::backend::winit::WinitEvent;
use smithay::desktop::space::render_output;
use smithay::output::{Mode, Output, PhysicalProperties, Subpixel};
use smithay::reexports::calloop::EventLoop;
use smithay::reexports::wayland_server::Display;
use smithay::utils::{Point, Size, Transform};
use smithay::wayland::socket::ListeningSocketSource;

use signal_hook::consts::{SIGHUP, SIGUSR1};

use crate::state::{ClientState, Gate};

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter("kara_gate=debug,smithay=info")
        .init();

    tracing::info!("starting kara-gate");

    let mut event_loop: EventLoop<Gate> =
        EventLoop::try_new().expect("failed to create event loop");

    let mut display: Display<Gate> = Display::new().expect("failed to create display");

    // Wayland socket
    let listening_socket = ListeningSocketSource::new_auto().expect("failed to bind socket");
    let socket_name = listening_socket.socket_name().to_string_lossy().to_string();
    tracing::info!("listening on WAYLAND_DISPLAY={}", socket_name);
    unsafe { std::env::set_var("WAYLAND_DISPLAY", &socket_name) };

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
    let signal_flag_ref = std::sync::Arc::clone(&signal_flag);

    // Winit backend
    let (mut backend, mut winit_evt) =
        winit::init::<GlesRenderer>().expect("failed to init winit");

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
    let mut state = Gate::new(&display, event_loop.get_signal());
    state.space.map_output(&output, (0, 0));
    state.set_output_size(size.w as i32, size.h as i32);

    let mut damage_tracker = OutputDamageTracker::from_output(&output);
    let mut last_status_refresh = Instant::now();

    // Initial status poll
    state.status_cache.refresh(true);

    tracing::info!("kara-gate ready ({}x{})", size.w, size.h);

    // Run autostart commands
    state.run_autostart();

    loop {
        // Winit events
        winit_evt.dispatch_new_events(|event| match event {
            WinitEvent::Resized { size, .. } => {
                let new_mode = Mode { size, refresh: 60_000 };
                output.change_current_state(Some(new_mode), None, None, None);
                damage_tracker = OutputDamageTracker::from_output(&output);
                state.set_output_size(size.w as i32, size.h as i32);
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
        if signal_flag_ref.swap(false, std::sync::atomic::Ordering::Relaxed) {
            state.reload_config();
        }

        // Refresh system status (~1s interval)
        let now = Instant::now();
        if now.duration_since(last_status_refresh) >= Duration::from_secs(1) {
            state.status_cache.refresh(false);
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

        // Build custom elements: wallpaper (bottom) + bar (top)
        let mut custom_elements: Vec<TextureRenderElement<GlesTexture>> = Vec::new();

        // Wallpaper (rendered behind everything)
        if let Some(ref wp) = state.wallpaper {
            if let Some(tex_buf) = wp.upload(renderer) {
                custom_elements.push(TextureRenderElement::from_texture_buffer(
                    Point::from((0.0, 0.0)),
                    &tex_buf,
                    None,
                    None,
                    None,
                    Kind::Unspecified,
                ));
            }
        }

        // Borders (rendered between wallpaper and windows)
        if state.fullscreen_window.is_none() {
            custom_elements.extend(render_borders(&state, renderer));
        }

        // Bar (rendered on top, hidden during fullscreen)
        if state.fullscreen_window.is_none() {
            custom_elements.extend(render_bar(&mut state, renderer));
        }

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

        // Dispatch
        display.dispatch_clients(&mut state).unwrap();
        display.flush_clients().unwrap();

        event_loop
            .dispatch(Some(Duration::from_millis(16)), &mut state)
            .expect("event loop error");
    }
}

/// Render the bar to a texture and return it as a render element.
fn render_bar(
    state: &mut Gate,
    renderer: &mut GlesRenderer,
) -> Vec<TextureRenderElement<GlesTexture>> {
    if !state.config.bar.enabled {
        return Vec::new();
    }

    let (output_w, _output_h) = state.output_size;
    let ws_ctx = state.bar_workspace_context();

    let pixmap = match state.bar_renderer.render(
        output_w as u32,
        &state.config.bar,
        &state.config.theme,
        &state.status_cache,
        &ws_ctx,
    ) {
        Some(p) => p,
        None => return Vec::new(),
    };

    let bar_y = match state.config.bar.position {
        kara_config::BarPosition::Top => 0.0,
        kara_config::BarPosition::Bottom => {
            (state.output_size.1 - state.config.bar.height) as f64
        }
    };

    // Upload pixmap as GLES texture
    // tiny-skia Pixmap data is premultiplied RGBA → Fourcc::Abgr8888 in DRM terms
    let texture_buffer = match TextureBuffer::from_memory(
        renderer,
        pixmap.data(),
        Fourcc::Abgr8888,
        Size::from((pixmap.width() as i32, pixmap.height() as i32)),
        false,
        1,
        Transform::Normal,
        None,
    ) {
        Ok(buf) => buf,
        Err(e) => {
            tracing::error!("failed to upload bar texture: {e:?}");
            return Vec::new();
        }
    };

    let element = TextureRenderElement::from_texture_buffer(
        Point::from((0.0, bar_y)),
        &texture_buffer,
        None,
        None,
        None,
        Kind::Unspecified,
    );

    vec![element]
}

/// Render border quads for all visible windows.
/// Each border is a solid-color rectangle rendered behind the window.
fn render_borders(
    state: &Gate,
    renderer: &mut GlesRenderer,
) -> Vec<TextureRenderElement<GlesTexture>> {
    let border_px = state.config.general.border_px;
    if border_px <= 0 {
        return Vec::new();
    }

    let accent = state.config.theme.accent;
    let border_color = state.config.theme.border;

    let mut elements = Vec::new();

    for &(rect, is_focused) in &state.border_rects {
        let color = if is_focused { accent } else { border_color };

        // Create a solid-color pixmap for the border rect
        let w = rect.size.w.max(1) as u32;
        let h = rect.size.h.max(1) as u32;

        let r = ((color >> 16) & 0xFF) as u8;
        let g = ((color >> 8) & 0xFF) as u8;
        let b = (color & 0xFF) as u8;

        let mut pixmap = match tiny_skia::Pixmap::new(w, h) {
            Some(p) => p,
            None => continue,
        };

        // Fill entire rect with border color
        let paint = tiny_skia::Paint {
            shader: tiny_skia::Shader::SolidColor(tiny_skia::Color::from_rgba8(r, g, b, 255)),
            ..Default::default()
        };
        let skia_rect = match tiny_skia::Rect::from_xywh(0.0, 0.0, w as f32, h as f32) {
            Some(r) => r,
            None => continue,
        };
        pixmap.fill_rect(skia_rect, &paint, tiny_skia::Transform::identity(), None);

        // Clear the inner area (where the window content will be)
        // The inner area is inset by border_px on all sides
        let inner_x = border_px as f32;
        let inner_y = border_px as f32;
        let inner_w = (w as i32 - border_px * 2).max(0) as f32;
        let inner_h = (h as i32 - border_px * 2).max(0) as f32;

        if inner_w > 0.0 && inner_h > 0.0 {
            let clear_paint = tiny_skia::Paint {
                shader: tiny_skia::Shader::SolidColor(tiny_skia::Color::from_rgba8(0, 0, 0, 0)),
                blend_mode: tiny_skia::BlendMode::Source,
                ..Default::default()
            };
            if let Some(inner_rect) = tiny_skia::Rect::from_xywh(inner_x, inner_y, inner_w, inner_h) {
                pixmap.fill_rect(inner_rect, &clear_paint, tiny_skia::Transform::identity(), None);
            }
        }

        let texture_buffer = match TextureBuffer::from_memory(
            renderer,
            pixmap.data(),
            Fourcc::Abgr8888,
            Size::from((w as i32, h as i32)),
            false,
            1,
            Transform::Normal,
            None,
        ) {
            Ok(buf) => buf,
            Err(e) => {
                tracing::error!("failed to upload border texture: {e:?}");
                continue;
            }
        };

        elements.push(TextureRenderElement::from_texture_buffer(
            Point::from((rect.loc.x as f64, rect.loc.y as f64)),
            &texture_buffer,
            None,
            None,
            None,
            Kind::Unspecified,
        ));
    }

    elements
}
