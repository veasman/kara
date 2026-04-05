//! udev/DRM production backend — boots from TTY.
//!
//! Uses libseat for session management, udev for device discovery,
//! DRM/GBM for display output, and libinput for input devices.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use smithay::backend::allocator::gbm::{GbmAllocator, GbmBufferFlags, GbmDevice};
use smithay::backend::drm::compositor::{DrmCompositor, FrameFlags};
use smithay::backend::drm::exporter::gbm::GbmFramebufferExporter;
use smithay::backend::drm::{DrmDevice, DrmDeviceFd, DrmEvent};
use smithay::backend::egl::{EGLContext, EGLDisplay};
use smithay::backend::libinput::{LibinputInputBackend, LibinputSessionInterface};
use smithay::backend::renderer::gles::{GlesRenderer, GlesTexture};
use smithay::backend::renderer::ImportDma;
use smithay::backend::session::libseat::LibSeatSession;
use smithay::backend::session::{Event as SessionEvent, Session};
use smithay::backend::udev::{self, UdevBackend, UdevEvent};
use smithay::output::{Mode as OutputMode, Output, PhysicalProperties, Subpixel};
use smithay::reexports::calloop::EventLoop;
use smithay::reexports::calloop::timer::{TimeoutAction, Timer};
use smithay::reexports::drm::control::{connector, crtc, Device as ControlDevice, ModeTypeFlags};
use smithay::reexports::input::Libinput;
use smithay::reexports::wayland_server::Display;
use smithay::utils::{DeviceFd, Size, Transform};

use smithay::backend::renderer::element::texture::TextureRenderElement;

use crate::render::build_custom_elements;
use crate::state::Gate;

use smithay::backend::renderer::element::surface::WaylandSurfaceRenderElement;
use smithay::backend::renderer::element::{Element, Id, RenderElement};
use smithay::desktop::space::SpaceRenderElements;
use smithay::utils::{Buffer, Physical, Rectangle, Scale};

type SpaceElement = SpaceRenderElements<GlesRenderer, WaylandSurfaceRenderElement<GlesRenderer>>;

/// Combined render element for DRM output: window surfaces + custom textures.
pub enum DrmRenderElement {
    Space(SpaceElement),
    Texture(TextureRenderElement<GlesTexture>),
}

impl Element for DrmRenderElement {
    fn id(&self) -> &Id {
        match self {
            Self::Space(e) => e.id(),
            Self::Texture(e) => e.id(),
        }
    }

    fn current_commit(&self) -> smithay::backend::renderer::utils::CommitCounter {
        match self {
            Self::Space(e) => e.current_commit(),
            Self::Texture(e) => e.current_commit(),
        }
    }

    fn geometry(&self, scale: Scale<f64>) -> Rectangle<i32, Physical> {
        match self {
            Self::Space(e) => e.geometry(scale),
            Self::Texture(e) => e.geometry(scale),
        }
    }

    fn src(&self) -> Rectangle<f64, Buffer> {
        match self {
            Self::Space(e) => e.src(),
            Self::Texture(e) => e.src(),
        }
    }
}

impl RenderElement<GlesRenderer> for DrmRenderElement {
    fn draw(
        &self,
        frame: &mut <GlesRenderer as smithay::backend::renderer::RendererSuper>::Frame<'_, '_>,
        src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        opaque_regions: &[Rectangle<i32, Physical>],
    ) -> Result<(), <GlesRenderer as smithay::backend::renderer::RendererSuper>::Error> {
        match self {
            Self::Space(e) => RenderElement::<GlesRenderer>::draw(e, frame, src, dst, damage, opaque_regions),
            Self::Texture(e) => RenderElement::<GlesRenderer>::draw(e, frame, src, dst, damage, opaque_regions),
        }
    }
}

pub fn run(
    mut event_loop: EventLoop<Gate>,
    display: &mut Display<Gate>,
    socket_name: String,
    signal_flag: Arc<AtomicBool>,
) {
    // --- 1. Session (libseat) ---
    let (mut session, notifier) =
        LibSeatSession::new().expect("failed to create libseat session");
    let seat_name = session.seat();
    tracing::info!("libseat session on seat: {seat_name}");

    // --- 2. udev device discovery ---
    let udev_backend =
        UdevBackend::new(&seat_name).expect("failed to create udev backend");

    // Find the primary GPU
    let gpu_path = udev::primary_gpu(&seat_name)
        .expect("failed to find primary GPU")
        .expect("no GPU found");
    tracing::info!("primary GPU: {}", gpu_path.display());

    // --- 3. Open DRM device via session ---
    let gpu_fd = session
        .open(
            &gpu_path,
            smithay::reexports::rustix::fs::OFlags::RDWR
                | smithay::reexports::rustix::fs::OFlags::CLOEXEC
                | smithay::reexports::rustix::fs::OFlags::NOCTTY,
        )
        .expect("failed to open GPU device");

    let drm_fd = DrmDeviceFd::new(DeviceFd::from(gpu_fd));
    let (mut drm_device, drm_notifier) =
        DrmDevice::new(drm_fd.clone(), true).expect("failed to create DRM device");

    // --- 4. GBM + EGL + GlesRenderer ---
    let gbm_device =
        GbmDevice::new(drm_fd.clone()).expect("failed to create GBM device");
    let egl_display = unsafe { EGLDisplay::new(gbm_device.clone()) }
        .expect("failed to create EGL display");
    let egl_context =
        EGLContext::new(&egl_display).expect("failed to create EGL context");
    let mut renderer = unsafe { GlesRenderer::new(egl_context) }
        .expect("failed to create GLES renderer");

    // --- 5. Find connected output ---
    let resources = drm_device
        .resource_handles()
        .expect("failed to get DRM resources");

    let mut selected_connector = None;
    let mut selected_mode = None;

    for conn_handle in resources.connectors() {
        let conn_info = drm_device
            .get_connector(*conn_handle, false)
            .expect("failed to get connector info");

        if conn_info.state() == connector::State::Connected && !conn_info.modes().is_empty() {
            // Prefer the mode flagged as PREFERRED, fallback to first (usually native res)
            let mode = conn_info
                .modes()
                .iter()
                .find(|m| m.mode_type().contains(ModeTypeFlags::PREFERRED))
                .copied()
                .unwrap_or(conn_info.modes()[0]);

            tracing::info!(
                "found output: {:?} {}x{}@{}Hz",
                conn_info.interface(),
                mode.size().0,
                mode.size().1,
                mode.vrefresh(),
            );

            selected_connector = Some(*conn_handle);
            selected_mode = Some(mode);
            break;
        }
    }

    let conn_handle = selected_connector.expect("no connected display found");
    let drm_mode = selected_mode.unwrap();

    // Find a CRTC for this connector
    let crtc_handle = find_crtc_for_connector(&drm_device, &resources, conn_handle)
        .expect("no CRTC available for connector");

    // --- 6. DRM surface + DrmCompositor ---
    let drm_surface = drm_device
        .create_surface(crtc_handle, drm_mode, &[conn_handle])
        .expect("failed to create DRM surface");

    let gbm_allocator = GbmAllocator::new(
        gbm_device.clone(),
        GbmBufferFlags::RENDERING | GbmBufferFlags::SCANOUT,
    );
    let gbm_exporter = GbmFramebufferExporter::new(gbm_device.clone(), None);

    let mode_size = drm_mode.size();
    let output_mode = OutputMode {
        size: Size::from((mode_size.0 as i32, mode_size.1 as i32)),
        refresh: (drm_mode.vrefresh() * 1000) as i32,
    };

    // Create the smithay Output
    let output = Output::new(
        format!("{:?}", drm_device.get_connector(conn_handle, false)
            .map(|c| c.interface())
            .unwrap_or(connector::Interface::Unknown)),
        PhysicalProperties {
            size: (0, 0).into(),
            subpixel: Subpixel::Unknown,
            make: "kara-gate".to_string(),
            model: "drm".to_string(),
        },
    );
    output.change_current_state(
        Some(output_mode),
        Some(Transform::Normal),
        None,
        Some((0, 0).into()),
    );
    output.set_preferred(output_mode);

    let renderer_formats = renderer.dmabuf_formats().into_iter().collect::<Vec<_>>();
    let cursor_size = drm_device.cursor_size();

    let mut drm_compositor = DrmCompositor::new(
        &output,
        drm_surface,
        None,
        gbm_allocator,
        gbm_exporter,
        [
            smithay::reexports::drm::buffer::DrmFourcc::Argb8888,
            smithay::reexports::drm::buffer::DrmFourcc::Xrgb8888,
        ],
        renderer_formats,
        cursor_size,
        Some(gbm_device.clone()),
    )
    .expect("failed to create DRM compositor");

    // --- 7. Libinput ---
    let mut libinput_context = Libinput::new_with_udev(
        LibinputSessionInterface::from(session.clone()),
    );
    libinput_context
        .udev_assign_seat(&seat_name)
        .expect("failed to assign libinput seat");
    let libinput_backend = LibinputInputBackend::new(libinput_context);

    // --- 8. Set WAYLAND_DISPLAY and create Gate ---
    unsafe { std::env::set_var("WAYLAND_DISPLAY", &socket_name) };

    let mut state = Gate::new(display, event_loop.get_signal());
    state.space.map_output(&output, (0, 0));
    state.set_output_size(mode_size.0 as i32, mode_size.1 as i32);
    state.pointer_location = (mode_size.0 as f64 / 2.0, mode_size.1 as f64 / 2.0).into();

    // Initial status poll
    state.status_cache.refresh(true);

    tracing::info!(
        "kara-gate ready [udev] ({}x{}@{}Hz)",
        mode_size.0, mode_size.1, drm_mode.vrefresh()
    );

    // Run autostart
    state.run_autostart();

    // --- 9. Insert event sources into calloop ---
    let loop_handle = event_loop.handle();

    // Session notifier (VT switching)
    loop_handle
        .insert_source(notifier, move |event, _, _state: &mut Gate| {
            match event {
                SessionEvent::PauseSession => {
                    tracing::info!("session paused (VT switch away)");
                }
                SessionEvent::ActivateSession => {
                    tracing::info!("session resumed (VT switch back)");
                }
            }
        })
        .expect("failed to insert session source");

    // DRM device notifier (vblank / page-flip events)
    let output_clone = output.clone();
    loop_handle
        .insert_source(drm_notifier, move |event, _metadata, state: &mut Gate| {
            match event {
                DrmEvent::VBlank(crtc) => {
                    tracing::trace!("vblank on crtc {:?}", crtc);
                    // frame_submitted will be called from the render path
                    let _ = &output_clone;
                }
                DrmEvent::Error(err) => {
                    tracing::error!("DRM error: {err}");
                }
            }
            let _ = state;
        })
        .expect("failed to insert DRM notifier");

    // Udev hotplug monitoring
    loop_handle
        .insert_source(udev_backend, |event, _, _state: &mut Gate| {
            match event {
                UdevEvent::Added { device_id, path } => {
                    tracing::info!("udev device added: {:?} at {}", device_id, path.display());
                }
                UdevEvent::Changed { device_id } => {
                    tracing::debug!("udev device changed: {:?}", device_id);
                }
                UdevEvent::Removed { device_id } => {
                    tracing::info!("udev device removed: {:?}", device_id);
                }
            }
        })
        .expect("failed to insert udev source");

    // Libinput
    loop_handle
        .insert_source(libinput_backend, |event, _, state: &mut Gate| {
            state.handle_input_event(event);
        })
        .expect("failed to insert libinput source");

    // Status refresh timer (every 1 second)
    loop_handle
        .insert_source(
            Timer::from_duration(Duration::from_secs(1)),
            |_deadline, _, state: &mut Gate| {
                state.status_cache.refresh(false);
                TimeoutAction::ToDuration(Duration::from_secs(1))
            },
        )
        .expect("failed to insert status timer");

    // --- 10. Render + main loop ---
    // Do an initial render
    render_frame(&mut drm_compositor, &mut renderer, &mut state, &output);

    let mut last_render = Instant::now();

    loop {
        // Poll IPC
        state.poll_ipc();

        // Check for reload signal
        if signal_flag.swap(false, std::sync::atomic::Ordering::Relaxed) {
            state.reload_config();
        }

        if !state.running {
            tracing::info!("shutting down");
            kara_ipc::server::cleanup_socket();
            break;
        }

        // Send frame callbacks
        let time = state.clock.now();
        state.space.elements().for_each(|window| {
            window.send_frame(&output, time, Some(Duration::ZERO), |_, _| {
                Some(output.clone())
            });
        });

        // Render at ~60fps if there's potential damage
        let now = Instant::now();
        if now.duration_since(last_render) >= Duration::from_millis(16) {
            // Handle vblank: acknowledge any submitted frame
            let _ = drm_compositor.frame_submitted();
            render_frame(&mut drm_compositor, &mut renderer, &mut state, &output);
            last_render = now;
        }

        // Dispatch wayland clients
        display.dispatch_clients(&mut state).unwrap();
        display.flush_clients().unwrap();

        // Dispatch calloop (blocks up to 16ms)
        event_loop
            .dispatch(Some(Duration::from_millis(16)), &mut state)
            .expect("event loop error");
    }
}

/// Render a frame via DrmCompositor, including both space windows and custom elements.
fn render_frame(
    drm_compositor: &mut DrmCompositor<
        GbmAllocator<DrmDeviceFd>,
        GbmFramebufferExporter<DrmDeviceFd>,
        (),
        DrmDeviceFd,
    >,
    renderer: &mut GlesRenderer,
    state: &mut Gate,
    output: &Output,
) {
    let custom_elements = build_custom_elements(state, renderer);

    // Get space render elements (window surfaces)
    let space_elements = match state.space.render_elements_for_output(renderer, output, 1.0) {
        Ok(e) => e,
        Err(e) => {
            tracing::error!("failed to get space render elements: {e:?}");
            return;
        }
    };

    // Build combined element list: custom first (bar on top), then windows
    let mut elements: Vec<DrmRenderElement> =
        Vec::with_capacity(custom_elements.len() + space_elements.len());
    elements.extend(custom_elements.into_iter().map(DrmRenderElement::Texture));
    elements.extend(space_elements.into_iter().map(DrmRenderElement::Space));

    match drm_compositor.render_frame(
        renderer,
        &elements,
        [0.05, 0.05, 0.05, 1.0],
        FrameFlags::empty(),
    ) {
        Ok(result) => {
            if !result.is_empty {
                if let Err(e) = drm_compositor.queue_frame(()) {
                    tracing::error!("failed to queue frame: {e:?}");
                }
            }
        }
        Err(err) => {
            tracing::error!("render_frame failed: {err:?}");
        }
    }
}

/// Find a CRTC that can drive the given connector.
fn find_crtc_for_connector(
    device: &DrmDevice,
    resources: &smithay::reexports::drm::control::ResourceHandles,
    connector: connector::Handle,
) -> Option<crtc::Handle> {
    let conn_info = device.get_connector(connector, false).ok()?;

    // Try each encoder the connector supports
    for encoder_handle in conn_info.encoders() {
        let encoder = match device.get_encoder(*encoder_handle) {
            Ok(e) => e,
            Err(_) => continue,
        };

        // Use filter_crtcs to get compatible CRTCs for this encoder
        let compatible = resources.filter_crtcs(encoder.possible_crtcs());
        if let Some(&crtc) = compatible.first() {
            return Some(crtc);
        }
    }

    // Fallback: just use the first CRTC
    resources.crtcs().first().copied()
}
