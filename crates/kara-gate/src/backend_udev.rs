//! udev/DRM production backend — boots from TTY.
//!
//! Uses libseat for session management, udev for device discovery,
//! DRM/GBM for display output, and libinput for input devices.
//! Supports multiple monitors.

use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

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
use smithay::utils::{Buffer, Physical, Rectangle, Scale};

use crate::render::build_custom_elements;
use crate::state::Gate;

/// Combined render element for DRM output: custom textures + wayland surfaces.
pub enum DrmRenderElement {
    Texture(TextureRenderElement<GlesTexture>),
    Surface(WaylandSurfaceRenderElement<GlesRenderer>),
}

impl Element for DrmRenderElement {
    fn id(&self) -> &Id {
        match self {
            Self::Texture(e) => e.id(),
            Self::Surface(e) => e.id(),
        }
    }

    fn current_commit(&self) -> CommitCounter {
        match self {
            Self::Texture(e) => e.current_commit(),
            Self::Surface(e) => e.current_commit(),
        }
    }

    fn geometry(&self, scale: Scale<f64>) -> Rectangle<i32, Physical> {
        match self {
            Self::Texture(e) => e.geometry(scale),
            Self::Surface(e) => e.geometry(scale),
        }
    }

    fn src(&self) -> Rectangle<f64, Buffer> {
        match self {
            Self::Texture(e) => e.src(),
            Self::Surface(e) => e.src(),
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
            Self::Texture(e) => RenderElement::<GlesRenderer>::draw(e, frame, src, dst, damage, opaque_regions),
            Self::Surface(e) => RenderElement::<GlesRenderer>::draw(e, frame, src, dst, damage, opaque_regions),
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
    crtc: crtc::Handle,
    frame_pending: bool,
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

    // Discover all GPUs. Pick the one with the most connected outputs
    // by checking sysfs (avoids opening/closing DRM fds which breaks sessions).
    let all_gpus = udev::all_gpus(&seat_name).unwrap_or_default();
    let primary = udev::primary_gpu(&seat_name).ok().flatten();

    let mut best_gpu: Option<(std::path::PathBuf, usize)> = None;
    for g in &all_gpus {
        let is_primary = primary.as_ref() == Some(g);
        // Count connected connectors via sysfs
        let card_name = g.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("");
        let sysfs_dir = format!("/sys/class/drm/{}", card_name);
        let conn_count = std::fs::read_dir(&sysfs_dir)
            .map(|entries| {
                entries.filter_map(|e| e.ok())
                    .filter(|e| {
                        let name = e.file_name().to_string_lossy().to_string();
                        if !name.starts_with(card_name) || name == card_name {
                            return false;
                        }
                        // Check connector status
                        let status_path = e.path().join("status");
                        std::fs::read_to_string(status_path)
                            .map(|s| s.trim() == "connected")
                            .unwrap_or(false)
                    })
                    .count()
            })
            .unwrap_or(0);
        tracing::info!(
            "found GPU: {} — {conn_count} connected output(s){}",
            g.display(),
            if is_primary { " (primary)" } else { "" }
        );
        let prev = best_gpu.as_ref().map(|(_, c)| *c).unwrap_or(0);
        if conn_count > prev {
            best_gpu = Some((g.clone(), conn_count));
        }
    }

    let gpu_path = best_gpu.map(|(p, _)| p)
        .or(primary)
        .or_else(|| all_gpus.into_iter().next())
        .expect("no GPU found");
    tracing::info!("using GPU: {}", gpu_path.display());

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

    // If any monitors are explicitly configured, only use those (skip unconfigured ones).
    // This gives explicit control over which outputs are active.
    let has_monitor_config = !state.config.monitors.is_empty();

    for conn_handle in resources.connectors() {
        let conn_info = match drm_device.get_connector(*conn_handle, false) {
            Ok(info) => info,
            Err(_) => continue,
        };

        if conn_info.state() != connector::State::Connected || conn_info.modes().is_empty() {
            continue;
        }

        // Build canonical output name for config matching
        let output_name = format!("{}-{}",
            match conn_info.interface() {
                connector::Interface::HDMIA => "HDMI-A",
                connector::Interface::HDMIB => "HDMI-B",
                connector::Interface::DisplayPort => "DP",
                connector::Interface::EmbeddedDisplayPort => "eDP",
                connector::Interface::VGA => "VGA",
                connector::Interface::DVII => "DVI-I",
                connector::Interface::DVID => "DVI-D",
                connector::Interface::DVIA => "DVI-A",
                connector::Interface::LVDS => "LVDS",
                _ => "Unknown",
            },
            conn_info.interface_id()
        );

        tracing::info!("detected connector: {output_name}");

        // Look up monitor config — clone relevant fields to avoid borrowing state
        let mon_config = state.config.monitors.iter().find(|m| m.name == output_name).cloned();

        // If monitors are configured, skip any connector not in config
        if has_monitor_config && mon_config.is_none() {
            tracing::info!("monitor {output_name} not in config, skipping");
            continue;
        }

        // Skip explicitly disabled monitors
        if let Some(mc) = mon_config.as_ref() {
            if !mc.enabled {
                tracing::info!("monitor {output_name} disabled by config, skipping");
                continue;
            }
        }

        // Mode selection — prefer config resolution over preferred mode
        let drm_mode = if let Some(Some((w, h))) = mon_config.as_ref().map(|mc| mc.resolution) {
            let refresh = mon_config.as_ref().and_then(|mc| mc.refresh).unwrap_or(0);
            conn_info.modes().iter()
                .find(|m| {
                    let (mw, mh) = m.size();
                    mw as i32 == w && mh as i32 == h
                        && (refresh == 0 || m.vrefresh() == refresh)
                })
                .or_else(|| conn_info.modes().iter()
                    .find(|m| m.mode_type().contains(ModeTypeFlags::PREFERRED)))
                .copied()
                .unwrap_or(conn_info.modes()[0])
        } else {
            conn_info.modes().iter()
                .find(|m| m.mode_type().contains(ModeTypeFlags::PREFERRED))
                .copied()
                .unwrap_or(conn_info.modes()[0])
        };

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

        // Position override — use configured position instead of auto x_offset
        let mon_position = if let Some(Some((px, py))) = mon_config.as_ref().map(|mc| mc.position) {
            (px, py)
        } else {
            (x_offset, 0)
        };

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
            Some(mon_position.into()),
        );
        output.set_preferred(output_mode);

        // Map in space and add to Gate
        state.space.map_output(&output, mon_position);
        state.add_output(
            output.clone(),
            (mode_size.0 as i32, mode_size.1 as i32),
            mon_position.into(),
        );

        // Log rotation config (not yet applied to DRM — needs GPU-side rotation support)
        let mon_rotation = mon_config.as_ref().map(|mc| mc.rotation).unwrap_or(kara_config::MonitorRotation::Normal);
        if mon_rotation != kara_config::MonitorRotation::Normal {
            tracing::warn!(
                "monitor {output_name}: rotation '{:?}' configured but not yet applied (needs GPU rotation support)",
                mon_rotation
            );
        }

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
            "output {output_name}: {}x{}@{}Hz at pos=({},{}) crtc={:?}",
            mode_size.0, mode_size.1, drm_mode.vrefresh(),
            mon_position.0, mon_position.1, crtc_handle
        );

        output_instances.push(OutputInstance {
            drm_compositor,
            output,
            crtc: crtc_handle,
            frame_pending: false,
        });

        x_offset += mode_size.0 as i32;
    }

    if output_instances.is_empty() {
        tracing::error!("no connected displays found!");
        tracing::error!("if you have monitor blocks in config, only those exact names are used.");
        tracing::error!("remove all monitor blocks from config to auto-detect, or fix the names.");
        // Give user time to read the error + detected connector names above
        std::thread::sleep(std::time::Duration::from_secs(10));
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

    // DRM device notifier — vblank signals per-CRTC frame completion
    let vblank_crtcs: Arc<std::sync::Mutex<Vec<crtc::Handle>>> =
        Arc::new(std::sync::Mutex::new(Vec::new()));
    let vblank_crtcs_clone = vblank_crtcs.clone();
    loop_handle
        .insert_source(drm_notifier, move |event, _metadata, _state: &mut Gate| {
            match event {
                DrmEvent::VBlank(crtc) => {
                    if let Ok(mut crtcs) = vblank_crtcs_clone.lock() {
                        crtcs.push(crtc);
                    }
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
                state.bar_dirty = true;
                state.check_config_changed();
                TimeoutAction::ToDuration(Duration::from_secs(1))
            },
        )
        .expect("failed to insert status timer");

    // --- 7. Initial render + main loop ---
    for (idx, instance) in output_instances.iter_mut().enumerate() {
        render_frame(instance, &mut renderer, &mut state, idx);
    }

    loop {
        // 1. Dispatch events FIRST — process input, vblank, timers, wayland clients.
        //    This ensures input state is up-to-date before we render.
        display.dispatch_clients(&mut state).unwrap();
        display.flush_clients().unwrap();

        event_loop
            .dispatch(Some(Duration::from_millis(1)), &mut state)
            .expect("event loop error");

        // 2. Housekeeping
        state.poll_ipc();

        if signal_flag.swap(false, std::sync::atomic::Ordering::Relaxed) {
            state.reload_config();
        }

        if !state.running {
            tracing::info!("shutting down");
            kara_ipc::server::cleanup_socket();
            break;
        }

        // 3. Process vblank events — acknowledge completed frames per-CRTC
        if let Ok(mut crtcs) = vblank_crtcs.lock() {
            for vblank_crtc in crtcs.drain(..) {
                for instance in output_instances.iter_mut() {
                    if instance.crtc == vblank_crtc && instance.frame_pending {
                        let _ = instance.drm_compositor.frame_submitted();
                        instance.frame_pending = false;
                    }
                }
            }
        }

        // 4. Tick animations
        state.process_completed_animations();
        if state.animations.has_active() {
            state.apply_animation_offsets();
        }

        // 5. Send frame callbacks to ALL windows so clients don't stall.
        let time = state.clock.now();
        for out_state in &state.outputs {
            let output = &out_state.output;

            // Windows in space
            state.space.elements().for_each(|window| {
                window.send_frame(output, time, Some(Duration::ZERO), |_, _| {
                    Some(output.clone())
                });
            });

            // Windows not in space (unmapped due to scratchpad, other workspace, etc.)
            for ws in &state.workspaces {
                for w in &ws.clients {
                    if state.space.element_location(w).is_none() {
                        w.send_frame(output, time, Some(Duration::ZERO), |_, _| {
                            Some(output.clone())
                        });
                    }
                }
            }
            for sp in &state.scratchpads {
                for w in &sp.workspace.clients {
                    if state.space.element_location(w).is_none() {
                        w.send_frame(output, time, Some(Duration::ZERO), |_, _| {
                            Some(output.clone())
                        });
                    }
                }
            }

            // Layer surfaces
            let map = smithay::desktop::layer_map_for_output(output);
            for layer in map.layers() {
                layer.send_frame(output, time, Some(Duration::ZERO), |_, _| {
                    Some(output.clone())
                });
            }
        }

        // 6. Render outputs not waiting for a pending frame
        for (idx, instance) in output_instances.iter_mut().enumerate() {
            if !instance.frame_pending {
                render_frame(instance, &mut renderer, &mut state, idx);
            }
        }
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
    let sp_borders = crate::render::build_scratchpad_borders(state, renderer, output_idx);
    let sp_dim = crate::render::build_scratchpad_dim(state, renderer, output_idx);

    // Use render_elements_for_region to get ONLY window elements (no layer surfaces).
    // Layer surfaces are rendered separately with correct positions from LayerMap.
    let output_geo = match state.space.output_geometry(&instance.output) {
        Some(g) => g,
        None => return,
    };
    let space_elements: Vec<_> = state.space.render_elements_for_region(
        renderer, &output_geo, 1.0, 1.0,
    );

    // Element order (front-to-back for DrmCompositor):
    // cursor > sp borders > space windows > dim (around sp hole) > custom
    //
    // Dim renders as four rects AROUND the scratchpad area, not over it.
    let mut elements: Vec<DrmRenderElement> =
        Vec::with_capacity(custom_elements.len() + sp_borders.len() + sp_dim.len() + space_elements.len() + 1);

    // Cursor (frontmost)
    if let Some(cursor_elem) = crate::cursor::build_cursor_element(state, renderer, output_idx) {
        elements.push(DrmRenderElement::Texture(cursor_elem));
    }

    // Keybind overlay (in front of everything except cursor)
    elements.extend(
        crate::render::build_keybind_overlay(state, renderer, output_idx)
            .into_iter()
            .map(DrmRenderElement::Texture),
    );

    // Overlay/Top layer surfaces (e.g., kara-summon) — with correct arranged positions
    {
        use smithay::backend::renderer::element::AsRenderElements;
        let map = smithay::desktop::layer_map_for_output(&instance.output);
        for layer in map.layers().rev() {
            if matches!(layer.layer(), smithay::wayland::shell::wlr_layer::Layer::Top | smithay::wayland::shell::wlr_layer::Layer::Overlay) {
                if let Some(geo) = map.layer_geometry(layer) {
                    let layer_elements = AsRenderElements::<GlesRenderer>::render_elements::<
                        WaylandSurfaceRenderElement<GlesRenderer>,
                    >(layer, renderer, geo.loc.to_physical_precise_round(1.0), smithay::utils::Scale::from(1.0), 1.0);
                    elements.extend(layer_elements.into_iter().map(DrmRenderElement::Surface));
                }
            }
        }
    }

    // Scratchpad borders (in front of windows)
    elements.extend(sp_borders.into_iter().map(DrmRenderElement::Texture));

    // Space windows (scratchpad raised to top, regular behind)
    elements.extend(space_elements.into_iter().map(DrmRenderElement::Surface));

    // Dim rects around scratchpad area (dims background, not scratchpad content)
    elements.extend(sp_dim.into_iter().map(DrmRenderElement::Texture));

    // Custom elements: wallpaper, workspace borders, bar (behind everything)
    elements.extend(custom_elements.into_iter().map(DrmRenderElement::Texture));

    match instance.drm_compositor.render_frame(
        renderer,
        &elements,
        [0.05, 0.05, 0.05, 1.0],
        FrameFlags::empty(),
    ) {
        Ok(result) => {
            if !result.is_empty {
                match instance.drm_compositor.queue_frame(()) {
                    Ok(()) => instance.frame_pending = true,
                    Err(e) => tracing::error!("failed to queue frame: {e:?}"),
                }
            }

            // Screenshot capture — render to offscreen and save PNG
            if let Some(path) = state.screenshot_path.take() {
                let region = state.screenshot_region.take();
                capture_screenshot(renderer, &elements, state, output_idx, &path, region);
            }
        }
        Err(err) => {
            tracing::error!("render_frame failed: {err:?}");
        }
    }
}

fn capture_screenshot(
    renderer: &mut GlesRenderer,
    elements: &[DrmRenderElement],
    state: &Gate,
    output_idx: usize,
    path: &str,
    region: Option<(i32, i32, i32, i32)>,
) {
    use smithay::backend::renderer::{Bind, ExportMem, Frame, Offscreen};
    use smithay::backend::allocator::Fourcc;

    let (w, h) = match state.outputs.get(output_idx) {
        Some(o) => o.size,
        None => return,
    };

    // Create offscreen render target
    let mut offscreen: smithay::backend::renderer::gles::GlesRenderbuffer =
        match Offscreen::<smithay::backend::renderer::gles::GlesRenderbuffer>::create_buffer(
            renderer,
            Fourcc::Abgr8888,
            Size::from((w, h)),
        ) {
            Ok(b) => b,
            Err(e) => {
                tracing::error!("screenshot: failed to create offscreen buffer: {e:?}");
                return;
            }
        };

    use smithay::backend::renderer::Renderer;

    let mut target = match renderer.bind(&mut offscreen) {
        Ok(t) => t,
        Err(e) => {
            tracing::error!("screenshot: failed to bind offscreen: {e:?}");
            return;
        }
    };

    {
        let output_size: Size<i32, smithay::utils::Physical> = (w, h).into();
        let mut frame = match renderer.render(&mut target, output_size, Transform::Normal) {
            Ok(f) => f,
            Err(e) => {
                tracing::error!("screenshot: failed to start render: {e:?}");
                return;
            }
        };

        let output_rect = smithay::utils::Rectangle::from_size(Size::from((w, h)));
        frame.clear(
            smithay::backend::renderer::Color32F::from([0.05, 0.05, 0.05, 1.0]),
            &[output_rect],
        ).ok();

        // Draw elements back-to-front (last element in vec = backmost)
        for elem in elements.iter().rev() {
            let geo = elem.geometry(smithay::utils::Scale::from(1.0));
            let src = elem.src();
            RenderElement::<GlesRenderer>::draw(&*elem, &mut frame, src, geo, &[geo], &[]).ok();
        }
    }

    // Read pixels back
    let fb_region = smithay::utils::Rectangle::from_size(
        Size::<i32, smithay::utils::Buffer>::from((w, h)),
    );
    match ExportMem::copy_framebuffer(renderer, &target, fb_region, Fourcc::Abgr8888) {
        Ok(mapping) => {
            match ExportMem::map_texture(renderer, &mapping) {
                Ok(data) => {
                    if let Some(img) = image::RgbaImage::from_raw(w as u32, h as u32, data.to_vec()) {
                        let final_img: image::RgbaImage = if let Some((rx, ry, rw, rh)) = region {
                            let rx = (rx as u32).min(img.width());
                            let ry = (ry as u32).min(img.height());
                            let rw = (rw as u32).min(img.width().saturating_sub(rx));
                            let rh = (rh as u32).min(img.height().saturating_sub(ry));
                            if rw > 0 && rh > 0 {
                                image::imageops::crop_imm(&img, rx, ry, rw, rh).to_image()
                            } else {
                                img
                            }
                        } else {
                            img
                        };
                        match final_img.save(path) {
                            Ok(()) => tracing::info!("screenshot saved: {path}"),
                            Err(e) => tracing::error!("screenshot save failed: {e}"),
                        }
                    }
                }
                Err(e) => tracing::error!("screenshot: map_texture failed: {e:?}"),
            }
        }
        Err(e) => tracing::error!("screenshot: copy_framebuffer failed: {e:?}"),
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
