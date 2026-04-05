//! udev/DRM production backend — boots from TTY.
//!
//! Uses libseat for session management, udev for device discovery,
//! DRM/GBM for display output, and libinput for input devices.
//! Supports multiple monitors.

use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use smithay::backend::allocator::gbm::{GbmAllocator, GbmBufferFlags, GbmDevice};
use smithay::backend::drm::compositor::{DrmCompositor, FrameFlags};
use smithay::backend::drm::exporter::gbm::GbmFramebufferExporter;
use smithay::backend::drm::{DrmDevice, DrmDeviceFd, DrmEvent};
use smithay::backend::egl::{EGLContext, EGLDisplay};
use smithay::backend::libinput::{LibinputInputBackend, LibinputSessionInterface};
use smithay::backend::renderer::element::texture::TextureRenderElement;
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

use smithay::backend::renderer::element::surface::WaylandSurfaceRenderElement;
use smithay::backend::renderer::element::{Element, Id, RenderElement};
use smithay::backend::renderer::utils::CommitCounter;
use smithay::desktop::space::SpaceRenderElements;
use smithay::utils::{Buffer, Physical, Rectangle, Scale};

use crate::render::build_custom_elements;
use crate::state::Gate;

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

    fn current_commit(&self) -> CommitCounter {
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

/// Per-output DRM state.
struct OutputInstance {
    drm_compositor: DrmCompositor<
        GbmAllocator<DrmDeviceFd>,
        GbmFramebufferExporter<DrmDeviceFd>,
        (),
        DrmDeviceFd,
    >,
    output: Output,
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

    let renderer_formats = renderer.dmabuf_formats().into_iter().collect::<Vec<_>>();
    let cursor_size = drm_device.cursor_size();

    // --- 5. Enumerate ALL connected outputs ---
    let resources = drm_device
        .resource_handles()
        .expect("failed to get DRM resources");

    let mut output_instances: Vec<OutputInstance> = Vec::new();
    let mut used_crtcs: HashSet<crtc::Handle> = HashSet::new();
    let mut x_offset: i32 = 0;

    // Set WAYLAND_DISPLAY and create Gate before output setup
    unsafe { std::env::set_var("WAYLAND_DISPLAY", &socket_name) };
    let mut state = Gate::new(display, event_loop.get_signal());

    for conn_handle in resources.connectors() {
        let conn_info = match drm_device.get_connector(*conn_handle, false) {
            Ok(info) => info,
            Err(_) => continue,
        };

        if conn_info.state() != connector::State::Connected || conn_info.modes().is_empty() {
            continue;
        }

        let drm_mode = conn_info
            .modes()
            .iter()
            .find(|m| m.mode_type().contains(ModeTypeFlags::PREFERRED))
            .copied()
            .unwrap_or(conn_info.modes()[0]);

        // Find an available CRTC
        let crtc_handle = match find_crtc_for_connector(&drm_device, &resources, *conn_handle, &used_crtcs) {
            Some(c) => c,
            None => {
                tracing::warn!("no CRTC available for {:?}", conn_info.interface());
                continue;
            }
        };
        used_crtcs.insert(crtc_handle);

        let mode_size = drm_mode.size();
        let output_mode = OutputMode {
            size: Size::from((mode_size.0 as i32, mode_size.1 as i32)),
            refresh: (drm_mode.vrefresh() * 1000) as i32,
        };

        let output_name = format!("{:?}-{}", conn_info.interface(), conn_info.interface_id());
        let output = Output::new(
            output_name.clone(),
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
            Some((x_offset, 0).into()),
        );
        output.set_preferred(output_mode);

        // Map in space and add to Gate
        state.space.map_output(&output, (x_offset, 0));
        state.add_output(
            output.clone(),
            (mode_size.0 as i32, mode_size.1 as i32),
            (x_offset, 0).into(),
        );

        // Create DRM surface + compositor for this output
        let drm_surface = match drm_device.create_surface(crtc_handle, drm_mode, &[*conn_handle]) {
            Ok(s) => s,
            Err(e) => {
                tracing::error!("failed to create DRM surface for {output_name}: {e}");
                continue;
            }
        };

        let gbm_allocator = GbmAllocator::new(
            gbm_device.clone(),
            GbmBufferFlags::RENDERING | GbmBufferFlags::SCANOUT,
        );
        let gbm_exporter = GbmFramebufferExporter::new(gbm_device.clone(), None);

        let drm_compositor = match DrmCompositor::new(
            &output,
            drm_surface,
            None,
            gbm_allocator,
            gbm_exporter,
            [
                smithay::reexports::drm::buffer::DrmFourcc::Argb8888,
                smithay::reexports::drm::buffer::DrmFourcc::Xrgb8888,
            ],
            renderer_formats.clone(),
            cursor_size,
            Some(gbm_device.clone()),
        ) {
            Ok(c) => c,
            Err(e) => {
                tracing::error!("failed to create DRM compositor for {output_name}: {e:?}");
                continue;
            }
        };

        tracing::info!(
            "output {output_name}: {}x{}@{}Hz at x={x_offset}",
            mode_size.0, mode_size.1, drm_mode.vrefresh()
        );

        output_instances.push(OutputInstance {
            drm_compositor,
            output,
        });

        x_offset += mode_size.0 as i32;
    }

    if output_instances.is_empty() {
        tracing::error!("no connected displays found");
        std::process::exit(1);
    }

    // Set initial workspace assignments for independent mode
    for (i, out) in state.outputs.iter_mut().enumerate() {
        out.current_ws = i % state.workspaces.len();
    }

    // Center pointer on first output
    if let Some(out) = state.outputs.first() {
        state.pointer_location = (
            out.location.x as f64 + out.size.0 as f64 / 2.0,
            out.location.y as f64 + out.size.1 as f64 / 2.0,
        ).into();
    }

    state.status_cache.refresh(true);

    tracing::info!(
        "kara-gate ready [udev] ({} output{})",
        output_instances.len(),
        if output_instances.len() == 1 { "" } else { "s" }
    );

    state.run_autostart();

    // --- 6. Insert event sources into calloop ---
    let loop_handle = event_loop.handle();

    // Session notifier
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

    // DRM device notifier
    loop_handle
        .insert_source(drm_notifier, move |event, _metadata, _state: &mut Gate| {
            match event {
                DrmEvent::VBlank(crtc) => {
                    tracing::trace!("vblank on crtc {:?}", crtc);
                }
                DrmEvent::Error(err) => {
                    tracing::error!("DRM error: {err}");
                }
            }
        })
        .expect("failed to insert DRM notifier");

    // Udev hotplug
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
    let mut libinput_context = Libinput::new_with_udev(
        LibinputSessionInterface::from(session.clone()),
    );
    libinput_context
        .udev_assign_seat(&seat_name)
        .expect("failed to assign libinput seat");
    let libinput_backend = LibinputInputBackend::new(libinput_context);

    loop_handle
        .insert_source(libinput_backend, |event, _, state: &mut Gate| {
            state.handle_input_event(event);
        })
        .expect("failed to insert libinput source");

    // Status refresh timer
    loop_handle
        .insert_source(
            Timer::from_duration(Duration::from_secs(1)),
            |_deadline, _, state: &mut Gate| {
                state.status_cache.refresh(false);
                TimeoutAction::ToDuration(Duration::from_secs(1))
            },
        )
        .expect("failed to insert status timer");

    // --- 7. Initial render + main loop ---
    for (idx, instance) in output_instances.iter_mut().enumerate() {
        render_frame(instance, &mut renderer, &mut state, idx);
    }

    let mut last_render = Instant::now();

    loop {
        state.poll_ipc();

        if signal_flag.swap(false, std::sync::atomic::Ordering::Relaxed) {
            state.reload_config();
        }

        if !state.running {
            tracing::info!("shutting down");
            kara_ipc::server::cleanup_socket();
            break;
        }

        // Send frame callbacks per output
        let time = state.clock.now();
        for out_state in &state.outputs {
            let output = &out_state.output;
            state.space.elements().for_each(|window| {
                window.send_frame(output, time, Some(Duration::ZERO), |_, _| {
                    Some(output.clone())
                });
            });
        }

        // Render all outputs at ~60fps
        let now = Instant::now();
        if now.duration_since(last_render) >= Duration::from_millis(16) {
            for (idx, instance) in output_instances.iter_mut().enumerate() {
                let _ = instance.drm_compositor.frame_submitted();
                render_frame(instance, &mut renderer, &mut state, idx);
            }
            last_render = now;
        }

        display.dispatch_clients(&mut state).unwrap();
        display.flush_clients().unwrap();

        event_loop
            .dispatch(Some(Duration::from_millis(16)), &mut state)
            .expect("event loop error");
    }
}

/// Render a frame for a specific output via its DrmCompositor.
fn render_frame(
    instance: &mut OutputInstance,
    renderer: &mut GlesRenderer,
    state: &mut Gate,
    output_idx: usize,
) {
    let custom_elements = build_custom_elements(state, renderer, output_idx);

    let space_elements = match state.space.render_elements_for_output(
        renderer, &instance.output, 1.0,
    ) {
        Ok(e) => e,
        Err(e) => {
            tracing::error!("failed to get space render elements: {e:?}");
            return;
        }
    };

    let mut elements: Vec<DrmRenderElement> =
        Vec::with_capacity(custom_elements.len() + space_elements.len());
    elements.extend(custom_elements.into_iter().map(DrmRenderElement::Texture));
    elements.extend(space_elements.into_iter().map(DrmRenderElement::Space));

    match instance.drm_compositor.render_frame(
        renderer,
        &elements,
        [0.05, 0.05, 0.05, 1.0],
        FrameFlags::empty(),
    ) {
        Ok(result) => {
            if !result.is_empty {
                if let Err(e) = instance.drm_compositor.queue_frame(()) {
                    tracing::error!("failed to queue frame: {e:?}");
                }
            }
        }
        Err(err) => {
            tracing::error!("render_frame failed: {err:?}");
        }
    }
}

/// Find a CRTC that can drive the given connector, excluding already-used CRTCs.
fn find_crtc_for_connector(
    device: &DrmDevice,
    resources: &smithay::reexports::drm::control::ResourceHandles,
    connector: connector::Handle,
    used: &HashSet<crtc::Handle>,
) -> Option<crtc::Handle> {
    let conn_info = device.get_connector(connector, false).ok()?;

    for encoder_handle in conn_info.encoders() {
        let encoder = match device.get_encoder(*encoder_handle) {
            Ok(e) => e,
            Err(_) => continue,
        };

        let compatible = resources.filter_crtcs(encoder.possible_crtcs());
        for &crtc in &compatible {
            if !used.contains(&crtc) {
                return Some(crtc);
            }
        }
    }

    // Fallback: first unused CRTC
    resources.crtcs().iter().find(|c| !used.contains(c)).copied()
}
