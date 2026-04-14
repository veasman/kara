//! udev/DRM production backend — boots from TTY.
//!
//! Uses libseat for session management, udev for device discovery,
//! DRM/GBM for display output, and libinput for input devices.
//! Supports multiple monitors.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use smithay::backend::allocator::{Fourcc, Modifier};
use smithay::backend::allocator::gbm::{GbmAllocator, GbmBufferFlags, GbmDevice};
use smithay::backend::drm::compositor::{DrmCompositor, FrameFlags};
use smithay::backend::drm::exporter::gbm::GbmFramebufferExporter;
use smithay::backend::drm::{DrmDevice, DrmDeviceFd, DrmEvent, DrmNode, NodeType};
use smithay::backend::libinput::{LibinputInputBackend, LibinputSessionInterface};
use smithay::backend::renderer::element::texture::TextureRenderElement;
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::backend::renderer::multigpu::{GpuManager, MultiRenderer, MultiTexture};
use smithay::backend::renderer::multigpu::gbm::GbmGlesBackend;
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

/// The multi-GPU graphics API kara uses: GBM + GLES. Single-GPU systems still
/// go through this; the `MultiRenderer` zero-cost-falls-back to a single
/// `GlesRenderer` when render + target nodes match.
pub type KaraApi = GbmGlesBackend<GlesRenderer, DrmDeviceFd>;

/// Short-lived renderer borrowed from the GpuManager for one frame. Both
/// lifetimes on `MultiRenderer` collapse onto `'a`, which is bound to the
/// `&mut GpuManager` borrow — the renderer must not outlive the scope of the
/// function that obtained it.
pub type KaraRenderer<'a> = MultiRenderer<'a, 'a, KaraApi, KaraApi>;

/// `TextureId` of our `MultiRenderer` — kept as a type alias so renderers in
/// render.rs / cursor.rs / kara-sight don't have to spell out the full path.
pub type KaraTexture = MultiTexture;

/// Combined render element for DRM output: custom textures + wayland surfaces.
///
/// The `'a` lifetime is tied to the `KaraRenderer` borrow that built the
/// surface elements — all DrmRenderElement values must be consumed (passed to
/// `DrmCompositor::render_frame`) and dropped before the renderer goes out of
/// scope.
pub enum DrmRenderElement<'a> {
    Texture(TextureRenderElement<KaraTexture>),
    Surface(WaylandSurfaceRenderElement<KaraRenderer<'a>>),
}

impl<'a> Element for DrmRenderElement<'a> {
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

impl<'a> RenderElement<KaraRenderer<'a>> for DrmRenderElement<'a> {
    fn draw(
        &self,
        frame: &mut <KaraRenderer<'a> as smithay::backend::renderer::RendererSuper>::Frame<'_, '_>,
        src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        opaque_regions: &[Rectangle<i32, Physical>],
    ) -> Result<(), <KaraRenderer<'a> as smithay::backend::renderer::RendererSuper>::Error> {
        match self {
            Self::Texture(e) => RenderElement::<KaraRenderer<'a>>::draw(e, frame, src, dst, damage, opaque_regions),
            Self::Surface(e) => RenderElement::<KaraRenderer<'a>>::draw(e, frame, src, dst, damage, opaque_regions),
        }
    }
}

/// Per-output DRM state. Each output belongs to exactly one device (`node`);
/// for M1 that's always the single primary render node, but M3 will add
/// outputs owned by evdi devices that render elsewhere.
struct OutputInstance {
    drm_compositor: DrmCompositor<
        GbmAllocator<DrmDeviceFd>,
        GbmFramebufferExporter<DrmDeviceFd>,
        (),
        DrmDeviceFd,
    >,
    output: Output,
    crtc: crtc::Handle,
    /// DRM node that owns this output's CRTC. Vblank routing keys on
    /// `(node, crtc)` so multiple devices don't collide on the same handle.
    node: DrmNode,
    frame_pending: bool,
}

/// Per-GPU runtime state. Every DRM device kara can see is opened and stashed
/// here, even if it can't render on its own (evdi). M1 only actively drives
/// outputs on the primary render node; later milestones route other devices
/// through the GpuManager for cross-GPU scan-out.
struct DeviceEntry {
    #[allow(dead_code)] // M2 will also read this via &mut elsewhere
    drm_device: DrmDevice,
    #[allow(dead_code)] // M2 will construct an EGLDisplay per device
    drm_fd: DrmDeviceFd,
    gbm_device: GbmDevice<DrmDeviceFd>,
    /// Render node path (e.g. `/dev/dri/renderD128`). `None` for scan-out-only
    /// devices like evdi that have no rendering hardware.
    render_node: Option<DrmNode>,
    /// Number of connected outputs at enumeration time — used to pick the
    /// primary render node.
    connected_outputs: usize,
    /// Card-level sysfs name, kept around for logging.
    card_name: String,
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

    // Discover every GPU kara can see. For M1 we open *all* of them (including
    // scan-out-only evdi devices from a DisplayLink dock), stash them in a
    // HashMap keyed by DrmNode, then pick a "primary render node" — the
    // device with the most connected outputs that actually has a render node
    // — as the target for all compositor output in this milestone.
    // Later milestones route non-primary devices through GpuManager.
    let all_gpus = udev::all_gpus(&seat_name).unwrap_or_default();
    let primary = udev::primary_gpu(&seat_name).ok().flatten();

    let mut devices: HashMap<DrmNode, DeviceEntry> = HashMap::new();
    let mut drm_notifiers: Vec<(DrmNode, smithay::backend::drm::DrmDeviceNotifier)> = Vec::new();

    for g in &all_gpus {
        let card_name = g
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("")
            .to_string();
        let is_primary_hint = primary.as_ref() == Some(g);

        // Probe the DrmNode BEFORE opening so we know whether this device has
        // a render node (normal GPU) or is scan-out-only (evdi/DisplayLink).
        // Both DrmNode::from_path and node_with_type are pure stat() calls.
        let card_node = match DrmNode::from_path(g) {
            Ok(n) => n,
            Err(e) => {
                tracing::warn!("skipping GPU {}: DrmNode::from_path failed: {e}", g.display());
                continue;
            }
        };
        let render_node = card_node.node_with_type(NodeType::Render).and_then(|r| r.ok());
        let is_scanout_only = render_node.is_none();

        // Count connected connectors via sysfs. Connector entries live in
        // `/sys/class/drm/` as siblings of the card (e.g. `card0-DVI-I-1`),
        // NOT inside `/sys/class/drm/card0/`. The previous code iterated the
        // card's own directory, which happens to work on amdgpu (symlink
        // quirk) but always returns zero on evdi — the actual connectors
        // were never counted.
        let card_prefix = format!("{card_name}-");
        let connected_outputs = std::fs::read_dir("/sys/class/drm")
            .map(|entries| {
                entries
                    .filter_map(|e| e.ok())
                    .filter(|e| {
                        e.file_name()
                            .to_string_lossy()
                            .starts_with(&card_prefix)
                    })
                    .filter(|e| {
                        std::fs::read_to_string(e.path().join("status"))
                            .map(|s| s.trim() == "connected")
                            .unwrap_or(false)
                    })
                    .count()
            })
            .unwrap_or(0);

        // Open via libseat so the device follows VT switches.
        let fd = match session.open(
            g,
            smithay::reexports::rustix::fs::OFlags::RDWR
                | smithay::reexports::rustix::fs::OFlags::CLOEXEC
                | smithay::reexports::rustix::fs::OFlags::NOCTTY,
        ) {
            Ok(fd) => fd,
            Err(e) => {
                tracing::warn!("skipping GPU {}: open failed: {e}", g.display());
                continue;
            }
        };
        let drm_fd = DrmDeviceFd::new(DeviceFd::from(fd));

        // `disable_connectors` tells smithay whether to reset every connector's
        // existing CRTC binding at construction. For the render-capable primary
        // device we pass `true` (we're about to reconfigure it anyway). For
        // scan-out-only devices we pass `false` — evdi already has the
        // DisplayLinkManager daemon talking to it, and an aggressive reset on
        // one device appears to destabilize shared DRM state enough that the
        // primary device's subsequent mode set silently no-ops. Non-destructive
        // open is safe here because M3-a only reads connector info; the actual
        // mode set will happen in M3-b with its own DrmCompositor.
        let (drm_device, drm_notifier) = match DrmDevice::new(drm_fd.clone(), !is_scanout_only) {
            Ok(pair) => pair,
            Err(e) => {
                tracing::warn!("skipping GPU {}: DrmDevice::new failed: {e}", g.display());
                continue;
            }
        };
        let gbm_device = match GbmDevice::new(drm_fd.clone()) {
            Ok(d) => d,
            Err(e) => {
                tracing::warn!("skipping GPU {}: GbmDevice::new failed: {e}", g.display());
                continue;
            }
        };

        if is_scanout_only {
            tracing::info!(
                "found GPU: {} ({connected_outputs} connected output(s), scan-out only — opened non-destructively for M3 enumeration)",
                g.display(),
            );
        } else {
            tracing::info!(
                "found GPU: {} ({connected_outputs} connected output(s){}, render node present)",
                g.display(),
                if is_primary_hint { ", udev primary" } else { "" },
            );
        }

        drm_notifiers.push((card_node, drm_notifier));
        devices.insert(
            card_node,
            DeviceEntry {
                drm_device,
                drm_fd,
                gbm_device,
                render_node,
                connected_outputs,
                card_name,
            },
        );
    }

    if devices.is_empty() {
        panic!("no usable GPUs found");
    }

    // Pick the primary render node: device with the most connected outputs
    // among render-capable devices. Fall back to udev's primary hint, then to
    // any render-capable device.
    let primary_node: DrmNode = {
        let render_capable: Vec<(DrmNode, usize)> = devices
            .iter()
            .filter(|(_, d)| d.render_node.is_some())
            .map(|(n, d)| (*n, d.connected_outputs))
            .collect();

        if render_capable.is_empty() {
            panic!("no render-capable GPU found");
        }

        // Best by connected output count, ties broken by udev primary hint.
        let primary_hint_node = primary
            .as_ref()
            .and_then(|p| DrmNode::from_path(p).ok())
            .and_then(|n| n.node_with_type(NodeType::Primary).and_then(|r| r.ok()))
            .or_else(|| primary.as_ref().and_then(|p| DrmNode::from_path(p).ok()));

        render_capable
            .iter()
            .max_by_key(|(node, count)| {
                let hint_bonus = if Some(*node) == primary_hint_node { 1 } else { 0 };
                (*count, hint_bonus)
            })
            .map(|(n, _)| *n)
            .unwrap()
    };

    tracing::info!(
        "primary render node: {} ({})",
        devices[&primary_node].card_name,
        primary_node,
    );

    // --- 4. GbmGlesBackend + GpuManager ---
    //
    // Each frame borrows a short-lived `MultiRenderer` from the GpuManager via
    // `single_renderer(&instance.node)`. On a single-GPU system the render
    // and target devices match, so the MultiRenderer collapses onto a bare
    // GlesRenderer with no copy overhead. M3 adds evdi-owning outputs via
    // `renderer(render_node, target_node, Xrgb8888)` for the cross-GPU path.
    let mut api_backend = KaraApi::default();
    for (node, device) in &devices {
        if let Err(e) = api_backend.add_node(*node, device.gbm_device.clone()) {
            tracing::warn!("failed to register {} with GbmGlesBackend: {e}", device.card_name);
        }
    }
    let mut gpu_manager: GpuManager<KaraApi> = GpuManager::new(api_backend)
        .expect("failed to create GpuManager");

    // Dmabuf formats supported by the primary render node. Obtained via a
    // scoped MultiRenderer so the mut borrow on gpu_manager doesn't outlive
    // this block.
    let renderer_formats: Vec<_> = {
        let renderer = gpu_manager
            .single_renderer(&primary_node)
            .expect("failed to obtain primary MultiRenderer for format probe");
        renderer.dmabuf_formats().into_iter().collect()
    };
    let cursor_size = devices[&primary_node].drm_device.cursor_size();

    // --- 5. Enumerate connected outputs on the primary device ---
    // M1 only drives outputs on the primary render node. Evdi/other-render
    // devices are in `devices` but we don't create OutputInstances for them
    // yet — M3 adds the cross-GPU plumbing.
    let mut output_instances: Vec<OutputInstance> = Vec::new();
    let mut used_crtcs: HashSet<(DrmNode, crtc::Handle)> = HashSet::new();
    let mut x_offset: i32 = 0;

    // Set WAYLAND_DISPLAY and create Gate before output setup
    unsafe { std::env::set_var("WAYLAND_DISPLAY", &socket_name) };
    let mut state = Gate::new(display, event_loop.get_signal());

    // Only iterate connectors on the primary device for M1/M2.
    // Clone the gbm_device up front so we can pass it to the GbmAllocator
    // inside the connector loop without holding a borrow on the devices map.
    let primary_gbm: GbmDevice<DrmDeviceFd> = devices[&primary_node].gbm_device.clone();
    let resources = devices[&primary_node]
        .drm_device
        .resource_handles()
        .expect("failed to get DRM resources");
    let drm_device = &mut devices.get_mut(&primary_node).unwrap().drm_device;

    for conn_handle in resources.connectors() {
        let conn_info = match drm_device.get_connector(*conn_handle, false) {
            Ok(info) => info,
            Err(_) => continue,
        };

        if conn_info.state() != connector::State::Connected || conn_info.modes().is_empty() {
            continue;
        }

        // Build canonical output name for config matching
        let output_name = format_connector_name(&conn_info);

        tracing::info!("detected connector: {output_name}");

        // Look up monitor config — clone relevant fields to avoid borrowing state.
        // Every detected connector is used by default; the config is a set of
        // overrides (resolution, position, rotation, primary, etc.) that
        // only apply when a matching entry exists. To exclude a monitor,
        // use `enabled false` explicitly. This is the "auto-detect +
        // overrides" model used by sway/hyprland — the user can leave full
        // work-laptop configs in place and unplug monitors freely without
        // kara refusing to boot on "only the laptop lid".
        let mon_config = state.config.monitors.iter().find(|m| m.name == output_name).cloned();

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
        let crtc_handle = match find_crtc_for_connector(
            drm_device,
            &resources,
            *conn_handle,
            primary_node,
            &used_crtcs,
        ) {
            Some(c) => c,
            None => {
                tracing::warn!("no CRTC available for {:?}", conn_info.interface());
                continue;
            }
        };
        used_crtcs.insert((primary_node, crtc_handle));

        let mode_size = drm_mode.size();
        let output_mode = OutputMode {
            size: Size::from((mode_size.0 as i32, mode_size.1 as i32)),
            refresh: (drm_mode.vrefresh() * 1000) as i32,
        };

        // Resolve the user's rotation config and convert to a smithay
        // Transform. The transform is what the compositor applies when it
        // composites elements onto the output's framebuffer; the underlying
        // DRM mode is still the unrotated physical resolution.
        let mon_rotation = mon_config
            .as_ref()
            .map(|mc| mc.rotation)
            .unwrap_or(kara_config::MonitorRotation::Normal);
        let transform = monitor_rotation_to_transform(mon_rotation);

        // For 90°/270° rotations the OUTPUT's logical (user-facing) size is
        // the mode size with width/height swapped — a 1920x1080 panel in
        // portrait reports as 1080x1920 logical, and the workarea/window
        // tile geometry must use the swapped dimensions.
        let logical_size = if matches!(transform,
            Transform::_90 | Transform::_270 | Transform::Flipped90 | Transform::Flipped270)
        {
            (mode_size.1 as i32, mode_size.0 as i32)
        } else {
            (mode_size.0 as i32, mode_size.1 as i32)
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
            Some(transform),
            None,
            Some(mon_position.into()),
        );
        output.set_preferred(output_mode);

        // Map in space and add to Gate using the logical (rotated) size so
        // downstream layout/tile logic sees the correct usable area.
        state.space.map_output(&output, mon_position);
        state.add_output(
            output.clone(),
            logical_size,
            mon_position.into(),
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
            primary_gbm.clone(),
            GbmBufferFlags::RENDERING | GbmBufferFlags::SCANOUT,
        );
        let gbm_exporter = GbmFramebufferExporter::new(primary_gbm.clone(), None);

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
            Some(primary_gbm.clone()),
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
            node: primary_node,
            frame_pending: false,
        });

        x_offset += mode_size.0 as i32;
    }

    // --- 5b. Enumerate connected connectors on non-primary (evdi) devices ---
    //
    // M3-b: for each evdi connector that's actually connected, create an
    // Output, a DRM surface, and a DrmCompositor with `gbm: None` (software
    // cursor) and a format set filtered to Linear modifiers only (evdi can
    // only scan out Linear buffers). The OutputInstance is tagged with the
    // evdi DrmNode, and render_frame dispatches to the cross-GPU render path
    // via `gpu_manager.renderer(primary, evdi, Xrgb8888)`.
    //
    // force_probe=true is required on get_connector: evdi doesn't maintain
    // fresh cached state, so a freshly-opened fd reports connectors as
    // disconnected until explicitly probed.
    //
    // Evdi-compatible renderer formats: filter to Linear-only. AMD dmabuf
    // exports can include a bunch of tiled modifiers; passing those to an
    // evdi DrmCompositor makes primary-plane allocation fail.
    let evdi_renderer_formats: Vec<_> = renderer_formats
        .iter()
        .filter(|f| f.modifier == Modifier::Linear)
        .filter(|f| matches!(f.code, Fourcc::Xrgb8888 | Fourcc::Argb8888))
        .copied()
        .collect();

    let evdi_nodes: Vec<DrmNode> = devices
        .keys()
        .copied()
        .filter(|n| *n != primary_node)
        .collect();

    for evdi_node in evdi_nodes {
        let entry = match devices.get_mut(&evdi_node) {
            Some(e) => e,
            None => continue,
        };
        let card_name = entry.card_name.clone();
        let evdi_gbm: GbmDevice<DrmDeviceFd> = entry.gbm_device.clone();

        let evdi_resources = match entry.drm_device.resource_handles() {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!("{card_name}: failed to get DRM resources: {e}");
                continue;
            }
        };
        let evdi_drm = &mut entry.drm_device;

        for conn_handle in evdi_resources.connectors() {
            let conn_info = match evdi_drm.get_connector(*conn_handle, true) {
                Ok(info) => info,
                Err(_) => continue,
            };
            if conn_info.state() != connector::State::Connected || conn_info.modes().is_empty() {
                tracing::debug!(
                    "{card_name}: {} {:?}",
                    format_connector_name(&conn_info),
                    conn_info.state()
                );
                continue;
            }

            let output_name = format_connector_name(&conn_info);
            tracing::info!("{card_name}: detected connector {output_name}");

            let mon_config = state
                .config
                .monitors
                .iter()
                .find(|m| m.name == output_name)
                .cloned();
            // See the primary loop's comment — monitor config is overrides,
            // not a gate. `enabled false` explicitly excludes one.
            if let Some(mc) = mon_config.as_ref() {
                if !mc.enabled {
                    tracing::info!("monitor {output_name} disabled by config, skipping");
                    continue;
                }
            }

            // Mode selection (same logic as primary loop)
            let drm_mode = if let Some(Some((w, h))) =
                mon_config.as_ref().map(|mc| mc.resolution)
            {
                let refresh = mon_config.as_ref().and_then(|mc| mc.refresh).unwrap_or(0);
                conn_info
                    .modes()
                    .iter()
                    .find(|m| {
                        let (mw, mh) = m.size();
                        mw as i32 == w
                            && mh as i32 == h
                            && (refresh == 0 || m.vrefresh() == refresh)
                    })
                    .or_else(|| {
                        conn_info
                            .modes()
                            .iter()
                            .find(|m| m.mode_type().contains(ModeTypeFlags::PREFERRED))
                    })
                    .copied()
                    .unwrap_or(conn_info.modes()[0])
            } else {
                conn_info
                    .modes()
                    .iter()
                    .find(|m| m.mode_type().contains(ModeTypeFlags::PREFERRED))
                    .copied()
                    .unwrap_or(conn_info.modes()[0])
            };

            let crtc_handle = match find_crtc_for_connector(
                evdi_drm,
                &evdi_resources,
                *conn_handle,
                evdi_node,
                &used_crtcs,
            ) {
                Some(c) => c,
                None => {
                    tracing::warn!(
                        "no CRTC available for {output_name} on {card_name}"
                    );
                    continue;
                }
            };
            used_crtcs.insert((evdi_node, crtc_handle));

            let mode_size = drm_mode.size();
            let output_mode = OutputMode {
                size: Size::from((mode_size.0 as i32, mode_size.1 as i32)),
                refresh: (drm_mode.vrefresh() * 1000) as i32,
            };

            // Same rotation handling as the primary loop — see the matching
            // block above. Evdi monitors hung off a DisplayLink dock can also
            // be portrait-rotated and the user's config controls them with
            // the same `monitor "..." { rotate left }` syntax.
            let mon_rotation = mon_config
                .as_ref()
                .map(|mc| mc.rotation)
                .unwrap_or(kara_config::MonitorRotation::Normal);
            let transform = monitor_rotation_to_transform(mon_rotation);
            let logical_size = if matches!(transform,
                Transform::_90 | Transform::_270 | Transform::Flipped90 | Transform::Flipped270)
            {
                (mode_size.1 as i32, mode_size.0 as i32)
            } else {
                (mode_size.0 as i32, mode_size.1 as i32)
            };

            let mon_position = if let Some(Some((px, py))) =
                mon_config.as_ref().map(|mc| mc.position)
            {
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
                    model: "displaylink".to_string(),
                },
            );
            output.change_current_state(
                Some(output_mode),
                Some(transform),
                None,
                Some(mon_position.into()),
            );
            output.set_preferred(output_mode);

            state.space.map_output(&output, mon_position);
            state.add_output(
                output.clone(),
                logical_size,
                mon_position.into(),
            );

            let drm_surface = match evdi_drm.create_surface(
                crtc_handle,
                drm_mode,
                &[*conn_handle],
            ) {
                Ok(s) => s,
                Err(e) => {
                    tracing::error!(
                        "{card_name}: failed to create DRM surface for {output_name}: {e}"
                    );
                    continue;
                }
            };

            // Hybrid swapchain: ALLOCATE on the primary GbmDevice (AMD), but
            // EXPORT framebuffers via evdi's GbmDevice. Why?
            //
            // - Evdi has no real GPU. Allocating buffers via evdi's GbmDevice
            //   returns dumb / software-EGL (llvmpipe) buffers; the
            //   MultiRenderer cross-GPU path then silently CPU-copies frames
            //   into them, which is slow and produces glitchy partial
            //   updates on scanout. We saw this in M3-b's first attempt.
            //
            // - Allocating AND exporting on the AMD GbmDevice fails to
            //   register the framebuffer on evdi: smithay's
            //   GbmFramebufferExporter takes the "native" framebuffer_from_bo
            //   path when the exporter's drm_node matches the buffer's
            //   source node, which calls drmModeAddFB on evdi using an
            //   AMD-side GEM handle that doesn't exist on evdi → ENOENT
            //   "Failed to add framebuffer" and DrmCompositor::new aborts.
            //
            // - Allocating on primary_gbm and exporting via evdi_gbm makes
            //   the foreign check in GbmFramebufferExporter::add_framebuffer
            //   (gbm.rs:74-78) fire, which routes through framebuffer_from_dmabuf
            //   — the proper cross-device path: export the AMD buffer as a
            //   dmabuf, re-import it on evdi's GbmDevice, and then drmModeAddFB2
            //   the imported buffer onto evdi's DrmSurface. Linear + Xrgb/Argb
            //   is required for the import (we filtered evdi_renderer_formats
            //   to that subset above).
            let gbm_allocator = GbmAllocator::new(
                primary_gbm.clone(),
                GbmBufferFlags::RENDERING | GbmBufferFlags::SCANOUT,
            );
            let gbm_exporter = GbmFramebufferExporter::new(evdi_gbm.clone(), None);

            // Last arg is cursor_gbm: None disables HW cursor plane. evdi has
            // no cursor plane; the cursor is composited into the primary
            // framebuffer on the render GPU instead.
            let drm_compositor = match DrmCompositor::new(
                &output,
                drm_surface,
                None,
                gbm_allocator,
                gbm_exporter,
                [
                    smithay::reexports::drm::buffer::DrmFourcc::Xrgb8888,
                    smithay::reexports::drm::buffer::DrmFourcc::Argb8888,
                ],
                evdi_renderer_formats.clone(),
                cursor_size,
                None,
            ) {
                Ok(c) => c,
                Err(e) => {
                    tracing::error!(
                        "{card_name}: failed to create DrmCompositor for {output_name}: {e:?}"
                    );
                    continue;
                }
            };

            tracing::info!(
                "{card_name}: output {output_name}: {}x{}@{}Hz at pos=({},{}) crtc={:?}",
                mode_size.0,
                mode_size.1,
                drm_mode.vrefresh(),
                mon_position.0,
                mon_position.1,
                crtc_handle,
            );

            output_instances.push(OutputInstance {
                drm_compositor,
                output,
                crtc: crtc_handle,
                node: evdi_node,
                frame_pending: false,
            });

            x_offset += mode_size.0 as i32;
        }
    }

    if output_instances.is_empty() {
        tracing::error!("no connected displays found!");
        tracing::error!("if you have monitor blocks in config, only those exact names are used.");
        tracing::error!("remove all monitor blocks from config to auto-detect, or fix the names.");
        // Give user time to read the error + detected connector names above
        std::thread::sleep(std::time::Duration::from_secs(10));
        std::process::exit(1);
    }

    // Sort outputs by their x-coordinate so mod+focus_monitor_next/prev cycles
    // left-to-right deterministically, regardless of the (HashMap-derived)
    // order kara opened the GPUs in. `state.outputs` and `output_instances`
    // are parallel vectors — the render loop calls
    // `state.outputs.get(idx)` where `idx` is the index in `output_instances`
    // — so they must be sorted in lockstep. Pair, sort, unpair.
    {
        let mut paired: Vec<_> = state
            .outputs
            .drain(..)
            .zip(output_instances.drain(..))
            .collect();
        paired.sort_by_key(|(o, _)| (o.location.x, o.location.y));
        for (o, inst) in paired {
            state.outputs.push(o);
            output_instances.push(inst);
        }
    }

    // Determine the primary monitor: pick the first MonitorConfig with
    // `primary` set. Fall back to whichever monitor is at x=0 (leftmost), or
    // index 0.
    let primary_idx = {
        let primary_name = state
            .config
            .monitors
            .iter()
            .find(|m| m.primary && m.enabled)
            .map(|m| m.name.clone());
        let by_config = primary_name.as_ref().and_then(|name| {
            state
                .outputs
                .iter()
                .position(|o| o.output.name() == *name)
        });
        by_config
            .or_else(|| state.outputs.iter().position(|o| o.location.x == 0))
            .unwrap_or(0)
    };
    state.focused_output = primary_idx;

    // Log any monitor config entries that didn't match a connected output —
    // they're silently dropped. This lets the user leave work + home
    // monitors in the same config file without commenting blocks in and out:
    // whichever aren't plugged in right now just log a debug line and
    // don't affect runtime state.
    let connected_names: std::collections::HashSet<String> = state
        .outputs
        .iter()
        .map(|o| o.output.name())
        .collect();
    for mc in &state.config.monitors {
        if !connected_names.contains(&mc.name) {
            tracing::info!(
                "monitor config for '{}' — not connected right now, ignored",
                mc.name
            );
        }
    }

    // Set initial workspace assignments for independent mode
    for (i, out) in state.outputs.iter_mut().enumerate() {
        out.current_ws = i % state.workspaces.len();
    }

    // Center pointer on the primary output
    if let Some(out) = state.outputs.get(primary_idx) {
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

    // DRM device notifiers — one per device. Vblank events are tagged with
    // their owning DrmNode so we can route them to the right OutputInstance
    // even when multiple devices share CRTC handle values.
    let vblank_crtcs: Arc<std::sync::Mutex<Vec<(DrmNode, crtc::Handle)>>> =
        Arc::new(std::sync::Mutex::new(Vec::new()));
    for (node, notifier) in drm_notifiers.drain(..) {
        let vblank_crtcs_clone = vblank_crtcs.clone();
        loop_handle
            .insert_source(notifier, move |event, _metadata, _state: &mut Gate| {
                match event {
                    DrmEvent::VBlank(crtc) => {
                        if let Ok(mut crtcs) = vblank_crtcs_clone.lock() {
                            crtcs.push((node, crtc));
                        }
                    }
                    DrmEvent::Error(err) => {
                        tracing::error!("DRM error on {node}: {err}");
                    }
                }
            })
            .expect("failed to insert DRM notifier");
    }

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
        render_frame(instance, &mut gpu_manager, primary_node, &mut state, idx);
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

        // 3. Process vblank events — acknowledge completed frames per
        // (DrmNode, CRTC). CRTC handles can collide between devices so we
        // always match on the pair.
        if let Ok(mut crtcs) = vblank_crtcs.lock() {
            for (vblank_node, vblank_crtc) in crtcs.drain(..) {
                for instance in output_instances.iter_mut() {
                    if instance.node == vblank_node
                        && instance.crtc == vblank_crtc
                        && instance.frame_pending
                    {
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
            // Walk every per-monitor workspace pool.
            for pool in &state.workspaces {
                for ws in pool {
                    for w in &ws.clients {
                        if state.space.element_location(w).is_none() {
                            w.send_frame(output, time, Some(Duration::ZERO), |_, _| {
                                Some(output.clone())
                            });
                        }
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
                render_frame(instance, &mut gpu_manager, primary_node, &mut state, idx);
            }
        }
    }
}

/// Render a frame for a specific output via its DrmCompositor.
///
/// Every frame is rendered on the primary render node (AMD). Evdi outputs
/// get their swapchains allocated from the primary GbmDevice at construction
/// time, so their DrmCompositor renders directly into AMD-backed dmabufs and
/// then wraps them as evdi framebuffers via `drmModeAddFB2` when scheduling
/// page flips — no cross-GPU copy, no llvmpipe software fallback.
fn render_frame(
    instance: &mut OutputInstance,
    gpu_manager: &mut GpuManager<KaraApi>,
    primary_node: DrmNode,
    state: &mut Gate,
    output_idx: usize,
) {
    let mut renderer = match gpu_manager.single_renderer(&primary_node) {
        Ok(r) => r,
        Err(e) => {
            tracing::error!(
                "failed to obtain primary MultiRenderer for {:?}: {e}",
                instance.node
            );
            return;
        }
    };

    let custom_elements = build_custom_elements(state, &mut renderer, output_idx);
    let sp_borders = crate::render::build_scratchpad_borders(state, &mut renderer, output_idx);
    let sp_dim = crate::render::build_scratchpad_dim(state, &mut renderer, output_idx);

    let output_geo = match state.space.output_geometry(&instance.output) {
        Some(g) => g,
        None => return,
    };

    // Partition mapped space elements into two groups so the scratchpad dim
    // can be rendered BETWEEN them: workspace windows draw below the dim
    // (so they get dimmed) and scratchpad windows draw above it (so they
    // remain visible AND the gaps between tiled scratchpad windows also
    // show dim instead of bleeding through to unaltered workspace content).
    //
    // `space.render_elements_for_region` returns one flat Vec in z-order,
    // so we walk `space.elements()` directly, render each window via
    // AsRenderElements, and sort into the right bucket. On outputs with no
    // visible scratchpad, the scratchpad group is empty and the workspace
    // group holds everything — same visual result as before.
    use smithay::backend::renderer::element::AsRenderElements;
    let mut workspace_elements: Vec<WaylandSurfaceRenderElement<KaraRenderer<'_>>> = Vec::new();
    let mut scratchpad_elements: Vec<WaylandSurfaceRenderElement<KaraRenderer<'_>>> = Vec::new();
    let space_windows: Vec<_> = state.space.elements().cloned().collect();
    for window in space_windows {
        // Skip windows whose bbox doesn't intersect this output.
        let loc = match state.space.element_location(&window) {
            Some(l) => l,
            None => continue,
        };
        let bbox = window.bbox_with_popups();
        let abs_bbox = smithay::utils::Rectangle::new(
            (loc.x + bbox.loc.x, loc.y + bbox.loc.y).into(),
            bbox.size,
        );
        if abs_bbox.intersection(output_geo).is_none() {
            continue;
        }

        let is_scratchpad = state
            .scratchpads
            .iter()
            .any(|sp| sp.workspace.clients.contains(&window));

        let win_elements = AsRenderElements::<KaraRenderer<'_>>::render_elements::<
            WaylandSurfaceRenderElement<KaraRenderer<'_>>,
        >(
            &window,
            &mut renderer,
            loc.to_physical_precise_round(1.0),
            smithay::utils::Scale::from(1.0),
            1.0,
        );

        if is_scratchpad {
            scratchpad_elements.extend(win_elements);
        } else {
            workspace_elements.extend(win_elements);
        }
    }

    // Element order (front-to-back for DrmCompositor — first is topmost):
    //   cursor > keybind > layers > sp_borders > scratchpad_elements
    //          > sp_dim > workspace_elements > custom
    //
    // Drawn back-to-front the sequence is:
    //   custom (wallpaper, bar, workspace borders)
    //   → workspace windows (dimmed if sp_dim is present)
    //   → sp_dim (full-screen rect — dims workspace even in the in-scratchpad
    //     window gaps, which the old four-rects-around-a-hole approach left
    //     showing through)
    //   → scratchpad windows (unaffected by the dim)
    //   → sp_borders → layers → keybind overlay → cursor
    let mut elements: Vec<DrmRenderElement<'_>> = Vec::with_capacity(
        custom_elements.len()
            + sp_borders.len()
            + sp_dim.len()
            + workspace_elements.len()
            + scratchpad_elements.len()
            + 1,
    );

    // Cursor (frontmost)
    if let Some(cursor_elem) = crate::cursor::build_cursor_element(state, &mut renderer, output_idx) {
        elements.push(DrmRenderElement::Texture(cursor_elem));
    }

    // Keybind overlay (in front of everything except cursor)
    elements.extend(
        crate::render::build_keybind_overlay(state, &mut renderer, output_idx)
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
                    let layer_elements = AsRenderElements::<KaraRenderer<'_>>::render_elements::<
                        WaylandSurfaceRenderElement<KaraRenderer<'_>>,
                    >(layer, &mut renderer, geo.loc.to_physical_precise_round(1.0), smithay::utils::Scale::from(1.0), 1.0);
                    elements.extend(layer_elements.into_iter().map(DrmRenderElement::Surface));
                }
            }
        }
    }

    // Scratchpad borders (in front of everything window-like)
    elements.extend(sp_borders.into_iter().map(DrmRenderElement::Texture));

    // Scratchpad windows — above the dim so they're not dimmed themselves,
    // and so the in-scratchpad window gaps show dim instead of bleeding
    // workspace content through.
    elements.extend(scratchpad_elements.into_iter().map(DrmRenderElement::Surface));

    // Full-screen dim (if any visible scratchpad on this output)
    elements.extend(sp_dim.into_iter().map(DrmRenderElement::Texture));

    // Workspace windows — drawn below the dim so they get dimmed.
    elements.extend(workspace_elements.into_iter().map(DrmRenderElement::Surface));

    // Custom elements: wallpaper, workspace borders, bar (behind everything)
    elements.extend(custom_elements.into_iter().map(DrmRenderElement::Texture));

    match instance.drm_compositor.render_frame(
        &mut renderer,
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
                capture_screenshot(&mut renderer, &elements, state, output_idx, &path, region);
            }
        }
        Err(err) => {
            tracing::error!("render_frame failed: {err:?}");
        }
    }
}

fn capture_screenshot<'a>(
    renderer: &mut KaraRenderer<'a>,
    elements: &[DrmRenderElement<'a>],
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

    // Create an offscreen GlesRenderbuffer. MultiRenderer<GbmGles, GbmGles>
    // forwards `Offscreen<GlesRenderbuffer>` to the underlying GlesRenderer,
    // so this works without needing a dmabuf round-trip.
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
            RenderElement::<KaraRenderer<'a>>::draw(&*elem, &mut frame, src, geo, &[geo], &[]).ok();
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

/// Map a kara monitor rotation config to a smithay `Transform`.
///
/// Convention: kara's `rotate left` matches xrandr's `--rotate left` —
/// monitor's top edge points left, content rotated 90° counter-clockwise.
/// In smithay's `Transform` enum that's `_270` (rotate 270° clockwise).
/// `rotate right` is 90° clockwise (`_90`); `rotate flipped` is 180°.
fn monitor_rotation_to_transform(r: kara_config::MonitorRotation) -> Transform {
    match r {
        kara_config::MonitorRotation::Normal => Transform::Normal,
        kara_config::MonitorRotation::Left => Transform::_270,
        kara_config::MonitorRotation::Right => Transform::_90,
        kara_config::MonitorRotation::Flipped => Transform::_180,
    }
}

/// Canonical `<iface>-<id>` name for a DRM connector (e.g. `DP-2`, `DVI-I-1`).
/// Used for both monitor-config matching and logging.
fn format_connector_name(conn_info: &connector::Info) -> String {
    let iface = match conn_info.interface() {
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
    };
    format!("{iface}-{}", conn_info.interface_id())
}

/// Find a CRTC that can drive the given connector, excluding already-used CRTCs.
fn find_crtc_for_connector(
    device: &DrmDevice,
    resources: &smithay::reexports::drm::control::ResourceHandles,
    connector: connector::Handle,
    node: DrmNode,
    used: &HashSet<(DrmNode, crtc::Handle)>,
) -> Option<crtc::Handle> {
    let conn_info = device.get_connector(connector, false).ok()?;
    let is_free = |c: &crtc::Handle| !used.contains(&(node, *c));

    for encoder_handle in conn_info.encoders() {
        let encoder = match device.get_encoder(*encoder_handle) {
            Ok(e) => e,
            Err(_) => continue,
        };

        let compatible = resources.filter_crtcs(encoder.possible_crtcs());
        for &crtc in &compatible {
            if is_free(&crtc) {
                return Some(crtc);
            }
        }
    }

    // Fallback: first unused CRTC
    resources.crtcs().iter().find(|c| is_free(c)).copied()
}
