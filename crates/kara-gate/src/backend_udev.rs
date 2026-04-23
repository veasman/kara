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
use smithay::backend::renderer::element::{Element, Id, Kind, RenderElement};
use smithay::backend::renderer::utils::{CommitCounter, DamageSet, OpaqueRegions};
use smithay::utils::{Buffer, Physical, Point, Rectangle, Scale};

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
    // IMPORTANT: every method on `Element` must be forwarded to the
    // inner element. The trait ships default impls for `transform`,
    // `location`, `damage_since`, `opaque_regions`, `alpha`, and `kind`
    // that silently return wrong values (Normal transform, empty
    // opaque regions, full-element damage, etc.) — on a non-rotated
    // output the defaults happen to render correctly, but on a
    // rotated output the damage tracker's transform-aware math
    // produces a half-output scissor and half the framebuffer never
    // gets painted. If you add a new method to the wrapper, forward
    // it here too.
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

    fn location(&self, scale: Scale<f64>) -> Point<i32, Physical> {
        match self {
            Self::Texture(e) => e.location(scale),
            Self::Surface(e) => e.location(scale),
        }
    }

    fn src(&self) -> Rectangle<f64, Buffer> {
        match self {
            Self::Texture(e) => e.src(),
            Self::Surface(e) => e.src(),
        }
    }

    fn transform(&self) -> Transform {
        match self {
            Self::Texture(e) => e.transform(),
            Self::Surface(e) => e.transform(),
        }
    }

    fn geometry(&self, scale: Scale<f64>) -> Rectangle<i32, Physical> {
        match self {
            Self::Texture(e) => e.geometry(scale),
            Self::Surface(e) => e.geometry(scale),
        }
    }

    fn damage_since(
        &self,
        scale: Scale<f64>,
        commit: Option<CommitCounter>,
    ) -> DamageSet<i32, Physical> {
        match self {
            Self::Texture(e) => e.damage_since(scale, commit),
            Self::Surface(e) => e.damage_since(scale, commit),
        }
    }

    fn opaque_regions(&self, scale: Scale<f64>) -> OpaqueRegions<i32, Physical> {
        match self {
            Self::Texture(e) => e.opaque_regions(scale),
            Self::Surface(e) => e.opaque_regions(scale),
        }
    }

    fn alpha(&self) -> f32 {
        match self {
            Self::Texture(e) => e.alpha(),
            Self::Surface(e) => e.alpha(),
        }
    }

    fn kind(&self) -> Kind {
        match self {
            Self::Texture(e) => e.kind(),
            Self::Surface(e) => e.kind(),
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
    /// Cached connector name. `Output::name()` locks an internal
    /// mutex and clones the String on every call; we read it many
    /// times per frame (error logging, rate limiter) so caching it
    /// once at construction avoids the per-frame allocation + lock.
    /// Arc<str> so `render_frame` can clone the handle (ref-count
    /// bump, no heap alloc) and still mutably borrow the rest of
    /// `instance` at the same time.
    cached_name: std::sync::Arc<str>,
    crtc: crtc::Handle,
    /// DRM node that owns this output's CRTC. Vblank routing keys on
    /// `(node, crtc)` so multiple devices don't collide on the same handle.
    node: DrmNode,
    frame_pending: bool,
    /// True until the first successful queue_frame on this output. Used
    /// to force an initial commit even when the first render_frame
    /// reports `is_empty` — without that commit the DRM surface never
    /// flips off the stale getty buffer, and on lid-closed docked
    /// boots the user just sees black on the external monitor until
    /// something happens to force a second render (opening the lid
    /// being the most common external trigger, since it produces a
    /// udev hotplug event).
    needs_initial_commit: bool,
    /// Backlog item #3 diagnostic: count of `render_frame` + `queue_frame`
    /// failures since the output came up. The first error is logged
    /// immediately and every 60th error after that is summarized. When the
    /// DisplayLink flashing-line bug reproduces, these logs surface which
    /// output / path is actually failing — most likely a cross-GPU dmabuf
    /// import on an evdi output under load.
    render_errors: u64,
    queue_errors: u64,
    /// Present when kara is rendering this output via the two-pass
    /// rotation path (bypassing smithay's broken rotated-render code
    /// path). See [`TwoPassState`] and `render_frame_two_pass`.
    two_pass: Option<TwoPassState>,
    /// Per-output working set for scratchpad backdrop blur. Eagerly
    /// allocated at init on non-rotated outputs (the pong dmabuf needs
    /// `primary_gbm` which is only in scope here); outputs using the
    /// two-pass rotation path get `None` and fall back to flat dim
    /// when a blurred scratchpad is visible on them.
    blur: Option<crate::blur::BlurState>,
}

/// Per-output state for kara's two-pass rotation render.
///
/// **Why this exists**: smithay 0.7 + this repo's evdi hybrid-swapchain
/// setup clips half the physical framebuffer when the Output has a
/// non-Normal transform (see `session_2026_04_15_rotation_bug` memory
/// note for the full investigation). Rather than try to fix smithay
/// from the outside, kara does the rotation itself:
///
///   1. Allocate an offscreen `GlesTexture` at the **portrait** logical
///      size (e.g. 1080x1920 for a `rotate right` 1920x1080 panel).
///   2. Each frame, render all kara elements (wallpaper, bar, borders,
///      windows, cursor, layer surfaces) into that texture with
///      `Transform::Normal` — a plain un-rotated portrait render.
///   3. Wrap the offscreen texture in a single `TextureRenderElement`
///      with element `transform = rotation`, covering the full
///      **landscape** scanout buffer.
///   4. Hand that single element to the output's `DrmCompositor`, which
///      was constructed with `OutputModeSource::Static { transform:
///      Normal, .. }` so its damage tracker runs landscape-only and
///      never hits the broken rotated path.
///
/// The Output object itself still carries the real `Transform::_90`
/// (etc) transform so `wl_output.transform` advertising and
/// `Space::output_geometry` both stay portrait, which means kara's
/// layout, workarea, input handling, and client-side buffer rotation
/// all Just Work without further changes.
struct TwoPassState {
    /// Persistent offscreen render target. Allocated from primary_gbm
    /// as a Linear Abgr8888 buffer at portrait dimensions and exported
    /// as a Dmabuf once — the Dmabuf is both the bind target for
    /// Phase A (portrait-space render) and gets imported as a
    /// `MultiTexture` each frame for Phase B (landscape blit).
    dmabuf: smithay::backend::allocator::dmabuf::Dmabuf,
    /// Portrait dimensions of `dmabuf` in physical pixels (e.g.
    /// 1080x1920). Matches `state.outputs[idx].size` for this output.
    portrait_size: Size<i32, Physical>,
    /// Landscape (DRM mode) dimensions of the scanout buffer this
    /// offscreen eventually blits into.
    landscape_size: Size<i32, Physical>,
    /// The real configured rotation transform — applied as the
    /// element transform on the single `TextureRenderElement` that
    /// wraps `offscreen` in the landscape blit pass.
    rotation: Transform,
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

    // Pre-scan: count how many connected outputs (across all DRM devices)
    // would survive the `enabled false` filter from the config. If the
    // answer is zero — typically because the user's config disables the
    // laptop's built-in panel and all dock monitors happen to be
    // unplugged right now — flip a fallback flag that tells both
    // enumeration loops below to ignore `enabled false`. This lets the
    // user leave `monitor "eDP-1" { enabled false }` in place for the
    // docked workflow and still boot cleanly when undocked, instead of
    // exiting with "no connected displays found".
    let force_enable_all = {
        let mut any_enabled = false;
        'scan: for entry in devices.values_mut() {
            let drm_device = &mut entry.drm_device;
            let resources = match drm_device.resource_handles() {
                Ok(r) => r,
                Err(_) => continue,
            };
            let conn_handles: Vec<_> = resources.connectors().to_vec();
            for conn_handle in &conn_handles {
                let Ok(conn_info) = drm_device.get_connector(*conn_handle, false) else {
                    continue;
                };
                if conn_info.state() != connector::State::Connected
                    || conn_info.modes().is_empty()
                {
                    continue;
                }
                let name = format_connector_name(&conn_info);
                let disabled_by_config = state
                    .config
                    .monitors
                    .iter()
                    .any(|m| m.name == name && !m.enabled);
                if !disabled_by_config {
                    any_enabled = true;
                    break 'scan;
                }
            }
        }
        !any_enabled
    };
    if force_enable_all {
        tracing::warn!(
            "all connected outputs are disabled by config; ignoring `enabled false` to avoid \
             startup failure (this is the automatic undocked/laptop-alone fallback — remove \
             `enabled false` from your monitor block or leave as-is for docked use)"
        );
    }

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
            if !mc.enabled && !force_enable_all {
                tracing::info!("monitor {output_name} disabled by config, skipping");
                continue;
            }
        }

        // Resolve rotation FIRST so mode selection can swap width/height
        // for portrait-rotated monitors: the config's `resolution` field
        // is the user-visible (post-rotation) size, but DRM modes are
        // always physical-panel-oriented.
        let mon_rotation = mon_config
            .as_ref()
            .map(|mc| mc.rotation)
            .unwrap_or(kara_config::MonitorRotation::Normal);
        let (transform, two_pass) = resolve_rotation(mon_rotation);
        let rotated_portrait = matches!(
            transform,
            Transform::_90 | Transform::_270 | Transform::Flipped90 | Transform::Flipped270
        );

        // Mode selection — prefer config resolution over preferred mode.
        // See `select_mode` for precedence + warning behavior.
        let drm_mode = select_mode(
            &output_name,
            conn_info.modes(),
            mon_config.as_ref().and_then(|mc| mc.resolution),
            mon_config.as_ref().and_then(|mc| mc.refresh),
            rotated_portrait,
        );

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

        // For 90°/270° rotations the OUTPUT's logical (user-facing) size is
        // the mode size with width/height swapped — a 1920x1080 panel in
        // portrait reports as 1080x1920 logical, and the workarea/window
        // tile geometry must use the swapped dimensions.
        let logical_size = if rotated_portrait {
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
            Some(smithay::output::Scale::Integer(1)),
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

        // Build the mode source for this output's DrmCompositor. For the
        // two-pass rotation path we pin it to Static(landscape, Normal) so
        // smithay's damage tracker never sees a rotated output (which is
        // the path that clips half the framebuffer on this repo's
        // AMD+evdi hybrid swapchain). For non-rotated outputs we pass the
        // Output directly and let Auto tracking run as before.
        let landscape_size: Size<i32, Physical> =
            Size::from((mode_size.0 as i32, mode_size.1 as i32));
        let portrait_size: Size<i32, Physical> =
            Size::from((logical_size.0, logical_size.1));
        let mode_source = if two_pass {
            smithay::output::OutputModeSource::Static {
                size: landscape_size,
                scale: smithay::utils::Scale::from(1.0),
                transform: Transform::Normal,
            }
        } else {
            smithay::output::OutputModeSource::Auto(output.clone())
        };

        let drm_compositor = match DrmCompositor::new(
            mode_source,
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

        // Two-pass rotation: allocate a persistent Dmabuf from the
        // primary GBM device at portrait dimensions. Reused as the
        // bind target for the per-frame portrait render, and re-
        // imported as a MultiTexture each frame for the landscape
        // blit pass.
        let two_pass_state = if two_pass {
            use smithay::backend::allocator::{dmabuf::AsDmabuf, Allocator, Fourcc, Modifier};
            let mut allocator = GbmAllocator::new(
                primary_gbm.clone(),
                GbmBufferFlags::RENDERING,
            );
            match allocator
                .create_buffer(
                    portrait_size.w as u32,
                    portrait_size.h as u32,
                    Fourcc::Abgr8888,
                    &[Modifier::Linear],
                )
                .and_then(|buffer| buffer.export().map_err(std::io::Error::other))
            {
                Ok(dmabuf) => Some(TwoPassState {
                    dmabuf,
                    portrait_size,
                    landscape_size,
                    rotation: config_rotation_transform(mon_rotation),
                }),
                Err(e) => {
                    tracing::error!(
                        "two-pass: offscreen dmabuf alloc failed for {output_name}: {e:?}"
                    );
                    continue;
                }
            }
        } else {
            None
        };

        tracing::info!(
            "output {output_name}: {}x{}@{}Hz at pos=({},{}) crtc={:?} two_pass={}",
            mode_size.0, mode_size.1, drm_mode.vrefresh(),
            mon_position.0, mon_position.1, crtc_handle, two_pass
        );

        // Blur runs before the rotation two-pass, so on rotated outputs
        // it must work in the element coordinate space (portrait) — not
        // the scanout space (landscape). The blur produces a
        // `portrait_size` element which is then captured and rotated by
        // `run_two_pass` along with all the other elements. On
        // non-rotated outputs portrait_size == landscape_size so this
        // is a no-op.
        let blur_size = if two_pass_state.is_some() {
            portrait_size
        } else {
            landscape_size
        };
        let blur_state = crate::blur::BlurState::try_new(&primary_gbm, blur_size);

        let cached_name: std::sync::Arc<str> = std::sync::Arc::from(output.name().as_str());
        output_instances.push(OutputInstance {
            drm_compositor,
            output,
            cached_name,
            crtc: crtc_handle,
            node: primary_node,
            frame_pending: false,
            needs_initial_commit: true,
            render_errors: 0,
            queue_errors: 0,
            two_pass: two_pass_state,
            blur: blur_state,
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
            // not a gate. `enabled false` explicitly excludes one, unless
            // the `force_enable_all` fallback is active (no other connected
            // output is enabled, so disabling this one would leave zero
            // active outputs and kara would fail to boot).
            if let Some(mc) = mon_config.as_ref() {
                if !mc.enabled && !force_enable_all {
                    tracing::info!("monitor {output_name} disabled by config, skipping");
                    continue;
                }
            }

            // Same rotation handling as the primary loop — resolve first
            // so select_mode can swap W/H for portrait-config monitors.
            // Evdi monitors hung off a DisplayLink dock can also be
            // portrait-rotated and the user's config controls them with
            // the same `monitor "..." { rotate left }` syntax.
            let mon_rotation = mon_config
                .as_ref()
                .map(|mc| mc.rotation)
                .unwrap_or(kara_config::MonitorRotation::Normal);
            let (transform, two_pass) = resolve_rotation(mon_rotation);
            let rotated_portrait = matches!(
                transform,
                Transform::_90 | Transform::_270 | Transform::Flipped90 | Transform::Flipped270
            );

            // Mode selection (same helper as the primary loop).
            let drm_mode = select_mode(
                &output_name,
                conn_info.modes(),
                mon_config.as_ref().and_then(|mc| mc.resolution),
                mon_config.as_ref().and_then(|mc| mc.refresh),
                rotated_portrait,
            );

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

            let logical_size = if rotated_portrait {
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
                Some(smithay::output::Scale::Integer(1)),
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

            // Same two-pass branch as the primary loop — see the matching
            // comment there. For rotated evdi outputs we pin the
            // DrmCompositor's mode source to Static(landscape, Normal) so
            // smithay's damage tracker never runs rotated (the broken
            // path), and kara handles rotation via the offscreen texture
            // in `render_frame_two_pass`.
            let evdi_landscape_size: Size<i32, Physical> =
                Size::from((mode_size.0 as i32, mode_size.1 as i32));
            let evdi_portrait_size: Size<i32, Physical> =
                Size::from((logical_size.0, logical_size.1));
            let evdi_mode_source = if two_pass {
                smithay::output::OutputModeSource::Static {
                    size: evdi_landscape_size,
                    scale: smithay::utils::Scale::from(1.0),
                    transform: Transform::Normal,
                }
            } else {
                smithay::output::OutputModeSource::Auto(output.clone())
            };

            // Last arg is cursor_gbm: None disables HW cursor plane. evdi has
            // no cursor plane; the cursor is composited into the primary
            // framebuffer on the render GPU instead.
            let drm_compositor = match DrmCompositor::new(
                evdi_mode_source,
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

            // Two-pass offscreen dmabuf allocation — see the primary
            // loop's matching block for why.
            let evdi_two_pass_state = if two_pass {
                use smithay::backend::allocator::{dmabuf::AsDmabuf, Allocator, Fourcc, Modifier};
                let mut allocator = GbmAllocator::new(
                    primary_gbm.clone(),
                    GbmBufferFlags::RENDERING,
                );
                match allocator
                    .create_buffer(
                        evdi_portrait_size.w as u32,
                        evdi_portrait_size.h as u32,
                        Fourcc::Abgr8888,
                        &[Modifier::Linear],
                    )
                    .and_then(|buffer| buffer.export().map_err(std::io::Error::other))
                {
                    Ok(dmabuf) => Some(TwoPassState {
                        dmabuf,
                        portrait_size: evdi_portrait_size,
                        landscape_size: evdi_landscape_size,
                        rotation: config_rotation_transform(mon_rotation),
                    }),
                    Err(e) => {
                        tracing::error!(
                            "{card_name}: two-pass offscreen dmabuf alloc failed for {output_name}: {e:?}"
                        );
                        continue;
                    }
                }
            } else {
                None
            };

            tracing::info!(
                "{card_name}: output {output_name}: {}x{}@{}Hz at pos=({},{}) crtc={:?} two_pass={}",
                mode_size.0,
                mode_size.1,
                drm_mode.vrefresh(),
                mon_position.0,
                mon_position.1,
                crtc_handle,
                two_pass,
            );

            let evdi_blur_size = if evdi_two_pass_state.is_some() {
                evdi_portrait_size
            } else {
                evdi_landscape_size
            };
            let evdi_blur_state = crate::blur::BlurState::try_new(&primary_gbm, evdi_blur_size);

            let cached_name: std::sync::Arc<str> = std::sync::Arc::from(output.name().as_str());
            output_instances.push(OutputInstance {
                drm_compositor,
                output,
                cached_name,
                crtc: crtc_handle,
                node: evdi_node,
                frame_pending: false,
                needs_initial_commit: true,
                render_errors: 0,
                queue_errors: 0,
                two_pass: evdi_two_pass_state,
                blur: evdi_blur_state,
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
    // order kara opened the GPUs in. `state.outputs`, `output_instances`,
    // and `state.workspaces` are parallel vectors (the render loop / action
    // handlers index them all by the same output index) — so they must be
    // reshuffled in lockstep. Pair, sort, unpair.
    {
        // Build a triple of (output, output_instance, workspace_pool) so
        // the sort-by-x reshuffles all three in unison.
        let pools = std::mem::take(&mut state.workspaces);
        let mut triples: Vec<_> = state
            .outputs
            .drain(..)
            .zip(output_instances.drain(..))
            .zip(pools.into_iter())
            .map(|((o, i), w)| (o, i, w))
            .collect();
        triples.sort_by_key(|(o, _, _)| (o.location.x, o.location.y));
        for (o, inst, pool) in triples {
            state.outputs.push(o);
            output_instances.push(inst);
            state.workspaces.push(pool);
        }
        // `output_order` was computed during each `add_output` call (which
        // happened before this sort). After reshuffling `state.outputs`,
        // the indices inside `output_order` point at the WRONG outputs —
        // position 1 still says "the monitor that was index 1 before the
        // sort", which is no longer the middle monitor. Recompute bounds
        // (which also recomputes output_order) now that the vectors are
        // in their final post-sort order. Without this recompute,
        // `focus_monitor_next/prev` walks monitors in stale order and
        // `monitor N` in autostart routes to the wrong physical monitor.
        state.refresh_output_geometry();
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
        let leftmost = state.outputs.iter().position(|o| o.location.x == 0);
        let chosen = by_config.or(leftmost).unwrap_or(0);
        let picked_name = state
            .outputs
            .get(chosen)
            .map(|o| o.output.name())
            .unwrap_or_default();
        tracing::info!(
            "primary monitor: config_name={:?} by_config_idx={:?} leftmost_idx={:?} chosen_idx={} chosen_name={}",
            primary_name, by_config, leftmost, chosen, picked_name,
        );
        chosen
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
            // Track keyboard-capable libinput devices so the SeatHandler
            // `led_state_changed` hook can push caps/num/scroll lock state
            // back to the hardware. Without tracking here, laptop caps-lock
            // LEDs never light up regardless of xkb state.
            match &event {
                smithay::backend::input::InputEvent::DeviceAdded { device } => {
                    if device.has_capability(
                        smithay::reexports::input::DeviceCapability::Keyboard,
                    ) {
                        state.libinput_keyboards.push(device.clone());
                    }
                }
                smithay::backend::input::InputEvent::DeviceRemoved { device } => {
                    state.libinput_keyboards.retain(|d| d != device);
                }
                _ => {}
            }
            state.handle_input_event(event);
        })
        .expect("failed to insert libinput source");

    // Status refresh timer
    loop_handle
        .insert_source(
            Timer::from_duration(Duration::from_secs(1)),
            |_deadline, _, state: &mut Gate| {
                // Skip status + bar work while the session is locked —
                // the lock render path doesn't draw the bar anyway,
                // and refreshing wpctl / playerctl / network state in
                // the background burns CPU for output the user can't
                // see. Config-change check stays live so a hot-reload
                // keybind while locked wouldn't get lost.
                if state.session_lock.is_none() {
                    state.status_cache.refresh(false);
                    state.bar_dirty = true;
                }
                state.check_config_changed();
                TimeoutAction::ToDuration(Duration::from_secs(1))
            },
        )
        .expect("failed to insert status timer");

    // Pre-lock-pending watchdog. If the user hit `mod+Shift+x` but
    // kara-veil never managed to acquire the protocol lock (binary
    // missing, PAM broken, Wayland connect failed), the blanked
    // render path would stick forever. Clear the pending flag after
    // 5 s so the desktop comes back instead of the user being
    // stranded at a black screen.
    loop_handle
        .insert_source(
            Timer::from_duration(Duration::from_millis(500)),
            |_deadline, _, state: &mut Gate| {
                if let Some(started) = state.lock_pending_since {
                    if state.session_lock.is_none()
                        && started.elapsed() > Duration::from_secs(5)
                    {
                        tracing::warn!(
                            "lock_pending timed out (kara-veil never acquired lock) — \
                             restoring desktop"
                        );
                        state.lock_pending_since = None;
                        state.bar_dirty = true;
                        state.layout_dirty = true;
                    }
                }
                TimeoutAction::ToDuration(Duration::from_millis(500))
            },
        )
        .expect("failed to insert lock-pending watchdog");

    // Dead-window sweep timer. `sweep_dead_windows` walks every
    // per-monitor workspace pool, scratchpads, and calls
    // `Space::refresh()` — which is O(windows * outputs) as it emits
    // wl_surface.enter/leave events. Running it every main-loop
    // iteration (thousands of times per second when input is flowing)
    // was pinning a core at idle. A half-second cadence is still
    // fast enough that a ghost-after-crash (Floorp abnormal exit)
    // resolves within the time it takes the user to reach for the
    // mouse to kill it, while keeping the hot loop clean.
    loop_handle
        .insert_source(
            Timer::from_duration(Duration::from_millis(500)),
            |_deadline, _, state: &mut Gate| {
                if state.sweep_dead_windows() {
                    tracing::info!("swept dead windows, re-laying out");
                    state.apply_layout();
                    state.apply_focus();
                }
                TimeoutAction::ToDuration(Duration::from_millis(500))
            },
        )
        .expect("failed to insert dead-window sweep timer");

    // Wallpaper animation tick — reschedules itself based on
    // each frame's per-frame delay so fast GIFs get accurate
    // frame pacing without keeping the loop hot.
    //
    // Idle interval is 250ms (not 60s) so a fresh wallpaper
    // applied via WallpaperChanged IPC starts animating within
    // a quarter second of being loaded — the timer wakes,
    // notices the new wallpaper is animated, and starts pacing
    // it. The wakeup cost when nothing is animating is
    // negligible (one no-op tick + a reschedule).
    loop_handle
        .insert_source(
            Timer::from_duration(Duration::from_millis(100)),
            |_deadline, _, state: &mut Gate| {
                if let Some(ref mut wp) = state.wallpaper {
                    if wp.tick() {
                        // Frame advanced — request a redraw on
                        // every output. The bar_dirty flag is
                        // the simplest signal the existing
                        // render scheduler watches; using it
                        // costs an extra bar redraw per frame
                        // but bar rendering is cheap (~1 ms).
                        state.bar_dirty = true;
                        // Wallpaper pixels moved under the bar,
                        // so the cached blur is stale. Drop both
                        // the pixel cache and its GPU texture so
                        // the next bar render rebuilds from the
                        // new frame. Without this the frosted
                        // glass behind the bar would freeze on
                        // the first wallpaper frame.
                        state.bar_blur_cache = None;
                        state.bar_blur_texture = None;
                    }
                    // Reschedule per frame for animations, or
                    // 250ms for stills/no-wallpaper so a new
                    // GIF applied while the timer is sleeping
                    // wakes within a quarter second.
                    let next = wp
                        .next_frame_due()
                        .unwrap_or(Duration::from_millis(250))
                        .max(Duration::from_millis(20));
                    return TimeoutAction::ToDuration(next);
                }
                // No wallpaper → check back in 250ms so a
                // freshly-loaded animated wallpaper picks up
                // its animation quickly.
                TimeoutAction::ToDuration(Duration::from_millis(250))
            },
        )
        .expect("failed to insert wallpaper animation timer");

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
        state.popup_manager.cleanup();
        // Note: dead-window sweep (`sweep_dead_windows`) runs on its
        // own 500ms timer — it walks every per-monitor pool and calls
        // `Space::refresh()` (O(windows * outputs)), so doing it per
        // main-loop iteration pinned a core under input load.

        if signal_flag.swap(false, std::sync::atomic::Ordering::Relaxed) {
            state.reload_config();
        }

        if !state.running {
            tracing::info!("shutting down");
            kara_ipc::server::cleanup_socket();

            // Pause every DrmDevice before the Drop impls run. Pausing
            // clears each device's internal "active" flag, which makes
            // smithay's Drop skip its best-effort state restore — that
            // restore tries to issue an atomic commit with
            // ALLOW_MODESET and fails EACCES here because kara was
            // opened in unprivileged mode (see `Unable to become drm
            // master, assuming unprivileged mode` warnings earlier in
            // the log), which in turn leaves the DisplayLink/evdi
            // devices wedged until the user re-runs their
            // `displaylink-setup.sh`. Skipping the doomed restore lets
            // the kernel / DisplayLinkManager daemon hand the panels
            // back to getty cleanly on its own.
            //
            // Dropping output_instances first forces any DrmCompositor
            // / DrmSurface drops to run while the devices are still
            // around (their own Drop impls are quieter once their
            // parent device is paused).
            drop(output_instances);
            for entry in devices.values_mut() {
                entry.drm_device.pause();
            }

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

    // ext-session-lock-v1 render path. Two entry points:
    //   • `session_lock.is_some()`: a lock client (kara-veil) has
    //     actually acquired the protocol lock; render_frame_locked
    //     paints blurred wallpaper + the client's lock surface.
    //   • `lock_pending_since.is_some()`: the `Lock` keybind was
    //     pressed and kara-veil is still starting up — we hit this
    //     path IMMEDIATELY so the desktop vanishes this frame, not
    //     300 ms later when the client finally connects. The lock
    //     surface HashMap is empty during pre-lock, so the render is
    //     wallpaper + blur only. As soon as kara-veil's request
    //     lands, `lock_pending_since` clears and we're on the real
    //     locked path.
    if state.session_lock.is_some() || state.lock_pending_since.is_some() {
        render_frame_locked(instance, &mut renderer, state, output_idx, primary_node);
        return;
    }

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
    // Three window buckets:
    //   - scratchpad_elements: windows in any scratchpad's workspace
    //   - floating_elements:   non-scratchpad windows marked floating in
    //                          their regular workspace
    //   - workspace_elements:  non-scratchpad tiled windows
    // Floating sits above tiled in the final vec so floaters stay visible
    // when a tiling layout is in use.
    let mut workspace_elements: Vec<WaylandSurfaceRenderElement<KaraRenderer<'_>>> = Vec::new();
    let mut floating_elements: Vec<WaylandSurfaceRenderElement<KaraRenderer<'_>>> = Vec::new();
    let mut focused_float_elements: Vec<WaylandSurfaceRenderElement<KaraRenderer<'_>>> = Vec::new();
    let mut scratchpad_elements: Vec<WaylandSurfaceRenderElement<KaraRenderer<'_>>> = Vec::new();

    // Pre-compute window classification once instead of O(n²) per-window
    // lookup into scratchpads + workspace pools. Window implements Hash+Eq.
    // Reuses a scratch HashMap on Gate so render_frame doesn't allocate a
    // fresh map per output per frame (was 60 fps × N outputs = hundreds of
    // transient allocations per second, each growing+rehashing as windows
    // were inserted).
    let window_class = &mut state.window_class_scratch;
    window_class.clear();
    for sp in &state.scratchpads {
        for w in &sp.workspace.clients {
            window_class.insert(w.clone(), (true, false));
        }
    }
    for out_pool in &state.workspaces {
        for ws in out_pool {
            for (idx, w) in ws.clients.iter().enumerate() {
                // Single hash lookup — `entry` consumes `w.clone()`
                // only when the key is vacant, so scratchpad
                // overrides from the loop above stay authoritative.
                window_class
                    .entry(w.clone())
                    .or_insert((false, ws.is_floating(idx)));
            }
        }
    }

    let space_windows: Vec<_> = state.space.elements().cloned().collect();
    for window in space_windows {
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

        let (is_scratchpad, is_floating) = window_class
            .get(&window)
            .copied()
            .unwrap_or((false, false));

        // Mirror smithay's Space::render_elements_for_output math:
        // render_location = element_location - window.geometry().loc - output_geo.loc
        // The geometry.loc subtraction is what positions CSD clients (Firefox,
        // GTK) correctly — their buffer origin sits above/left of their
        // logical content origin by the shadow margin, so the render origin
        // must back up by that amount to make the content land where the
        // compositor placed it.
        let render_loc = loc - window.geometry().loc - output_geo.loc;
        let win_elements = AsRenderElements::<KaraRenderer<'_>>::render_elements::<
            WaylandSurfaceRenderElement<KaraRenderer<'_>>,
        >(
            &window,
            &mut renderer,
            render_loc.to_physical_precise_round(1.0),
            smithay::utils::Scale::from(1.0),
            1.0,
        );

        if is_scratchpad {
            scratchpad_elements.extend(win_elements);
        } else if is_floating {
            // Check if this is the focused window — it needs to
            // render ON TOP of all other floats. We collect focused
            // float elements separately and prepend them to the vec
            // (front-to-back = first is topmost).
            let is_focused = state.seat.get_keyboard()
                .and_then(|kb| kb.current_focus())
                .map(|focus_surface| {
                    window.toplevel()
                        .map(|t| *t.wl_surface() == focus_surface)
                        .unwrap_or(false)
                })
                .unwrap_or(false);
            if is_focused {
                focused_float_elements.extend(win_elements);
            } else {
                floating_elements.extend(win_elements);
            }
        } else {
            workspace_elements.extend(win_elements);
        }
    }

    // Element order (front-to-back for DrmCompositor — first is topmost):
    //   cursor > keybind > layers > sp_borders > scratchpad_elements
    //          > sp_dim > floating_elements > workspace_elements > custom
    //
    // Drawn back-to-front the sequence is:
    //   custom (wallpaper, bar, workspace borders)
    //   → workspace windows — tiled (dimmed if sp_dim is present)
    //   → floating windows (above tiled, also dimmed by sp_dim)
    //   → sp_dim (full-screen rect — dims workspace + floaters even in
    //     the in-scratchpad window gaps)
    //   → scratchpad windows (unaffected by the dim)
    //   → sp_borders → layers → keybind overlay → cursor
    let mut elements: Vec<DrmRenderElement<'_>> = Vec::with_capacity(
        custom_elements.len()
            + sp_borders.len()
            + sp_dim.len()
            + floating_elements.len()
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

    // Overlay/Top layer surfaces (e.g., kara-summon) — with correct arranged positions.
    //
    // When the theme picker (namespace "kara-picker") is up and the
    // bar has blur enabled, we also emit a backdrop-blur texture
    // immediately after the picker's surface element. Element index
    // order is front→back, so pushing the blur AFTER the picker places
    // it behind the picker at paint time. That gives the translucent
    // picker the same frosted-glass look the bar has without needing
    // a compositor-level blur extension protocol.
    let picker_blur_rect: Option<smithay::utils::Rectangle<i32, smithay::utils::Logical>> = {
        use smithay::backend::renderer::element::AsRenderElements;
        let map = smithay::desktop::layer_map_for_output(&instance.output);
        let mut picker_rect = None;
        for layer in map.layers().rev() {
            if matches!(layer.layer(), smithay::wayland::shell::wlr_layer::Layer::Top | smithay::wayland::shell::wlr_layer::Layer::Overlay) {
                if let Some(geo) = map.layer_geometry(layer) {
                    let layer_elements = AsRenderElements::<KaraRenderer<'_>>::render_elements::<
                        WaylandSurfaceRenderElement<KaraRenderer<'_>>,
                    >(layer, &mut renderer, geo.loc.to_physical_precise_round(1.0), smithay::utils::Scale::from(1.0), 1.0);
                    elements.extend(layer_elements.into_iter().map(DrmRenderElement::Surface));
                    if layer.namespace() == "kara-picker" && state.config.bar.blur {
                        // Output-local -> global coords for the blur helper.
                        let out_loc = state
                            .outputs
                            .get(output_idx)
                            .map(|o| (o.location.x, o.location.y))
                            .unwrap_or((0, 0));
                        picker_rect = Some(smithay::utils::Rectangle::new(
                            (geo.loc.x + out_loc.0, geo.loc.y + out_loc.1).into(),
                            geo.size,
                        ));
                    }
                }
            }
        }
        picker_rect
    };
    if let Some(rect) = picker_blur_rect {
        if let Some(elem) = crate::render::render_picker_blur(state, &mut renderer, rect) {
            elements.push(DrmRenderElement::Texture(elem));
        }
    } else if state.picker_blur_cache.is_some() {
        // Picker closed: drop the cached blur so we don't hold a
        // full-rect wallpaper crop past its usefulness.
        state.picker_blur_cache = None;
        state.picker_blur_texture = None;
    }

    // Scratchpad borders (in front of everything window-like)
    elements.extend(sp_borders.into_iter().map(DrmRenderElement::Texture));

    // Scratchpad windows — above the dim so they're not dimmed themselves,
    // and so the in-scratchpad window gaps show dim instead of bleeding
    // workspace content through.
    elements.extend(scratchpad_elements.into_iter().map(DrmRenderElement::Surface));

    // Full-screen dim (if any visible scratchpad on this output)
    elements.extend(sp_dim.into_iter().map(DrmRenderElement::Texture));

    // Floating windows — above tiled workspace windows so they stay
    // visible in tiling mode, below sp_dim so scratchpad mode dims them
    // along with the rest of the workspace backdrop.
    // Focused float first (= topmost in front-to-back ordering),
    // then unfocused floats behind it.
    let mut all_floats = focused_float_elements;
    all_floats.extend(floating_elements);
    let floating_len_saved = all_floats.len();
    elements.extend(all_floats.into_iter().map(DrmRenderElement::Surface));

    // Workspace windows — drawn below the dim so they get dimmed.
    let workspace_len_saved = workspace_elements.len();
    elements.extend(workspace_elements.into_iter().map(DrmRenderElement::Surface));

    // Custom elements: wallpaper, workspace borders, bar (behind everything)
    let custom_len_saved = custom_elements.len();
    elements.extend(custom_elements.into_iter().map(DrmRenderElement::Texture));

    let is_evdi = instance.node != primary_node;
    // Cached at OutputInstance construction so `render_frame` doesn't
    // pay a mutex-locked String clone per frame. Arc<str> clone here
    // is a single ref-count bump; the returned handle is independent
    // of `instance` so we can freely `&mut instance` below.
    let output_name = instance.cached_name.clone();

    // Scratchpad backdrop blur: if any visible scratchpad on this output
    // has `blur true`, replace the workspace+custom tail of `elements`
    // with a single Texture element wrapping a blurred capture of those
    // same elements. The existing sp_dim rect still sits above this new
    // tail element and provides the darkening tint — so the visual is
    // "blurred wallpaper+windows + flat dim multiply".
    //
    // On rotated outputs the blur runs at `portrait_size` (element
    // coordinate space, not scanout space). It must run *before*
    // `run_two_pass` — the blurred element is captured along with the
    // rest of the vec into the rotation offscreen and blits rotated
    // into the landscape scanout. The branches below enforce that.
    let want_blur = instance.blur.is_some() && scratchpad_wants_blur(state, output_idx);
    if want_blur {
        // The blur backdrop covers everything below sp_dim: the floating
        // bucket, the tiled workspace bucket, and the custom texture
        // block (wallpaper/bar/borders). Drain all three in one shot.
        let backdrop_len = floating_len_saved + workspace_len_saved + custom_len_saved;
        let blur_passes = scratchpad_max_blur_passes(state, output_idx);
        if backdrop_len > 0 && blur_passes > 0 {
            if let Err(phase) = run_scratchpad_blur(
                &mut renderer,
                instance,
                &mut state.blur_program,
                &mut elements,
                backdrop_len,
                blur_passes,
                &output_name,
            ) {
                log_rate_limited_error(
                    &mut instance.render_errors,
                    phase,
                    &output_name,
                    is_evdi,
                    "scratchpad blur failed (falling back to flat dim)",
                );
                // Fall through — the element vec is unchanged on error,
                // so we still render a flat-dim scratchpad.
            }
        }
    }

    // Screenshot capture runs BEFORE run_two_pass so rotated outputs
    // see the same portrait/logical element list that matches the output
    // size `capture_screenshot` renders into. After run_two_pass, the
    // element vec is replaced by a single landscape-sized blit element
    // (the offscreen portrait texture being rotated back into the
    // landscape scanout buffer), which no longer lines up with either
    // the portrait output size or glimpse's region coords — a portrait
    // monitor would end up with a clipped/wrong crop.
    // Screenshot trigger. Drain any queue entries that target THIS
    // output — `output_idx=None` in the entry means "focused output",
    // which matches only when we're rendering the focused one.
    // Multiple entries can exist when the client batched requests
    // (glimpse region across N monitors, veil backdrop across N
    // monitors); each fires independently and writes to its own path.
    let focused_output = state.focused_output;
    let to_capture: Vec<crate::state::PendingScreenshot> = {
        let q = &mut state.screenshot_queue;
        let mut drained = Vec::new();
        // Walk in-place and pull out entries whose target matches.
        let mut i = 0;
        while i < q.len() {
            let matches = match q[i].output_idx {
                Some(idx) => idx == output_idx,
                None => output_idx == focused_output,
            };
            if matches {
                drained.push(q.remove(i));
            } else {
                i += 1;
            }
        }
        drained
    };
    for entry in to_capture {
        capture_screenshot(
            &mut renderer,
            &elements,
            state,
            output_idx,
            &entry.path,
            entry.region,
        );
    }

    // Two-pass rotation: render the portrait element vec into the
    // offscreen dmabuf, then replace `elements` with a single
    // landscape-sized TextureRenderElement wrapping that dmabuf (with
    // the element transform set so the sampled content rotates back
    // into the landscape scanout buffer). The DrmCompositor was
    // built with OutputModeSource::Static(landscape, Normal), so its
    // damage tracker runs landscape-only and never touches the broken
    // rotated-render path.
    if instance.two_pass.is_some() {
        if let Err(phase) = run_two_pass(
            &mut renderer,
            instance,
            &mut elements,
            primary_node,
            &output_name,
        ) {
            log_rate_limited_error(
                &mut instance.render_errors,
                phase,
                &output_name,
                is_evdi,
                "two-pass pipeline failed",
            );
            return;
        }
    }

    match instance.drm_compositor.render_frame(
        &mut renderer,
        &elements,
        [0.05, 0.05, 0.05, 1.0],
        FrameFlags::empty(),
    ) {
        Ok(result) => {
            // Force the initial frame to commit even when smithay reports
            // "no damage" — on docked boots with the laptop lid closed
            // there are no windows/layers at first_render time, so
            // `is_empty` is true; skipping queue_frame then leaves the
            // DRM surface on the stale getty buffer (which the kernel
            // tends to render as black after the mode swap). One forced
            // commit puts our blank-but-correct frame on screen so the
            // user sees kara's background instead of a frozen terminal.
            let force = instance.needs_initial_commit;
            if force {
                tracing::info!(
                    "first render: output={} is_empty={} force=true",
                    output_name,
                    result.is_empty
                );
            }
            if !result.is_empty || force {
                // evdi / DisplayLink outputs pull the dmabuf over USB
                // without honoring GPU implicit sync. If the AMD GPU is
                // still writing the framebuffer when evdi starts its USB
                // blit, the user sees vertical tearing / glitch lines.
                //
                // We drop an EGLFence at the end of the render stream
                // and wait on it with SYNC_FLUSH_COMMANDS_BIT set, which
                // guarantees every GL command issued up to this point
                // has completed before `queue_frame` schedules the
                // flip. This is narrower than `glFinish` — the wait
                // targets a specific sync point instead of draining
                // every outstanding driver operation — so the main
                // render thread stalls less while still giving evdi a
                // fully-written buffer. Falls back to `glFinish` if
                // EGL_KHR_fence_sync is unavailable. Only runs on evdi
                // outputs; the native AMD DP monitor pays no cost.
                if is_evdi {
                    use smithay::backend::egl::fence::EGLFence;
                    let gles = renderer.as_mut();
                    let display = gles.egl_context().display().clone();
                    let _ = gles.with_context(|gl| {
                        // `with_context` calls make_current on the
                        // EGLContext before invoking us, so the fence
                        // lands in the same stream of GL commands that
                        // just finished render_frame.
                        match EGLFence::create(&display) {
                            Ok(fence) => {
                                // timeout=None blocks forever; flush=true
                                // sets SYNC_FLUSH_COMMANDS_BIT so the
                                // fence is actually submitted to the GPU.
                                let _ = fence.client_wait(None, true);
                            }
                            Err(_) => {
                                // Extension missing or creation failed.
                                // glFinish is always available.
                                unsafe { gl.Finish(); }
                            }
                        }
                    });
                }
                match instance.drm_compositor.queue_frame(()) {
                    Ok(()) => {
                        instance.frame_pending = true;
                        instance.needs_initial_commit = false;
                    }
                    Err(e) => {
                        log_rate_limited_error(
                            &mut instance.queue_errors,
                            "queue_frame",
                            &output_name,
                            is_evdi,
                            &format!("{e:?}"),
                        );
                    }
                }
            }

        }
        Err(err) => {
            log_rate_limited_error(
                &mut instance.render_errors,
                "render_frame",
                &output_name,
                is_evdi,
                &format!("{err:?}"),
            );
        }
    }
}

/// Lock-mode render path. Called in place of the normal render when
/// `state.session_lock.is_some()`. Builds a minimal element list from
/// just the ext-session-lock surface on this output (or an empty list
/// if the client hasn't created one yet) and queues a black frame.
///
/// All the normal-path work (wallpaper, bar, window classification,
/// scratchpad, borders, two-pass rotation, blur) is skipped — while
/// the session is locked we must never paint anything that could leak
/// the desktop, and skipping the heavy pipeline also stops the lock
/// from costing what the active desktop was costing.
fn render_frame_locked<'a>(
    instance: &mut OutputInstance,
    renderer: &mut KaraRenderer<'a>,
    state: &mut Gate,
    output_idx: usize,
    primary_node: DrmNode,
) {
    use smithay::backend::renderer::element::surface::render_elements_from_surface_tree;
    use smithay::backend::renderer::element::Kind;

    let output_name = instance.cached_name.clone();
    let is_evdi = instance.node != primary_node;

    // Find this output's lock surface if the client has created one yet.
    let out_name = state
        .outputs
        .get(output_idx)
        .map(|o| o.output.name())
        .unwrap_or_default();

    // Build the backdrop: wallpaper ONLY (explicitly not the bar, nor
    // the window-border stack — those would show through the blur as
    // recognisable horizontal stripes and defeat the "desktop is
    // hidden" contract of the lock). Wallpaper-only also means
    // kara-sight's status timer stops driving bar repaints while
    // we're locked, which was the "bar still renders" artefact.
    let mut elements: Vec<DrmRenderElement<'_>> = Vec::new();
    if let Some(wp) = crate::render::build_wallpaper_element(state, renderer, output_idx) {
        elements.push(DrmRenderElement::Texture(wp));
    }
    let backdrop_len = elements.len();

    // Apply strong blur to the backdrop. Reuses the same BlurState
    // that scratchpads use — allocated at output construction at the
    // output's LOGICAL size (portrait for rotated outputs, landscape
    // otherwise), which matches the coordinate space the wallpaper
    // element uses, so the blur runs correctly on rotated outputs too.
    // The two-pass rotation runs AFTER the blur (below), rotating the
    // already-blurred portrait content into the landscape scanout.
    // More passes than a scratchpad (3 vs 1) for a frostier look.
    let blur_passes_for_lock: u32 = 3;
    if backdrop_len > 0 && instance.blur.is_some() {
        if let Err(phase) = run_scratchpad_blur(
            renderer,
            instance,
            &mut state.blur_program,
            &mut elements,
            backdrop_len,
            blur_passes_for_lock,
            &output_name,
        ) {
            log_rate_limited_error(
                &mut instance.render_errors,
                phase,
                &output_name,
                is_evdi,
                "lock-backdrop blur failed — falling back to plain wallpaper",
            );
            // Fall through — elements still contains the unblurred
            // wallpaper/bar/border stack, which is better than nothing.
        }
    }

    // Lock surface goes ON TOP of the blurred backdrop.
    if let Some(lock) = state.session_lock.as_ref() {
        if let Some(lock_surf) = lock.surfaces.get(&out_name) {
            let surface_elements: Vec<WaylandSurfaceRenderElement<KaraRenderer<'a>>> =
                render_elements_from_surface_tree(
                    renderer,
                    lock_surf.wl_surface(),
                    (0, 0),
                    1.0,
                    1.0,
                    Kind::Unspecified,
                );
            // Push surface elements at the FRONT of the vec — kara's
            // smithay setup renders elements in reverse (first = top),
            // so the lock UI covers the blurred chrome underneath.
            let mut new_list: Vec<DrmRenderElement<'_>> = Vec::with_capacity(
                surface_elements.len() + elements.len(),
            );
            for e in surface_elements {
                new_list.push(DrmRenderElement::Surface(e));
            }
            new_list.append(&mut elements);
            elements = new_list;
        }
    }

    // Two-pass rotated outputs still need their rotation applied — the
    // lock surface client sends a portrait-sized buffer for portrait
    // outputs because smithay advertised the logical size in the
    // configure. Route through run_two_pass so the offscreen rotation
    // still works.
    if instance.two_pass.is_some() {
        if let Err(phase) = run_two_pass(
            renderer,
            instance,
            &mut elements,
            instance.node,
            &output_name,
        ) {
            log_rate_limited_error(
                &mut instance.render_errors,
                phase,
                &output_name,
                is_evdi,
                "two-pass lock-render failed",
            );
            return;
        }
    }

    // Render + queue. Black clear color so the frame is visually
    // correct even before the client's first commit lands.
    match instance.drm_compositor.render_frame(
        renderer,
        &elements,
        [0.0, 0.0, 0.0, 1.0],
        FrameFlags::empty(),
    ) {
        Ok(result) => {
            let force = instance.needs_initial_commit;
            if !result.is_empty || force {
                match instance.drm_compositor.queue_frame(()) {
                    Ok(()) => {
                        instance.frame_pending = true;
                        instance.needs_initial_commit = false;
                    }
                    Err(e) => {
                        log_rate_limited_error(
                            &mut instance.queue_errors,
                            "queue_frame (lock)",
                            &output_name,
                            is_evdi,
                            &format!("{e:?}"),
                        );
                    }
                }
            }
        }
        Err(err) => {
            log_rate_limited_error(
                &mut instance.render_errors,
                "render_frame (lock)",
                &output_name,
                is_evdi,
                &format!("{err:?}"),
            );
        }
    }
}

/// Rate-limited error logger for `render_frame`/`queue_frame` failures.
///
/// Backlog #3: the DisplayLink flashing-line bug only reproduces with
/// three monitors plugged into the dock, and used to hide in raw
/// `tracing::error!` spam. First failure per output/path gets a loud
/// log with the is_evdi tag so it's obvious which path is failing;
/// after that, every 60th failure re-emits a summary so a healthy log
/// isn't drowned but sustained failure is still visible. Once the bug
/// is root-caused and fixed, this helper can stay — it's a useful
/// defensive log for any future DRM flake.
fn log_rate_limited_error(
    counter: &mut u64,
    path: &str,
    output_name: &str,
    is_evdi: bool,
    err: &str,
) {
    *counter = counter.saturating_add(1);
    let first = *counter == 1;
    let periodic = *counter % 60 == 0;
    if first {
        tracing::error!(
            target: "kara_gate::render_errors",
            "first {path} failure on {output_name} (is_evdi={is_evdi}): {err}",
        );
    } else if periodic {
        tracing::error!(
            target: "kara_gate::render_errors",
            "{path} failure #{count} on {output_name} (is_evdi={is_evdi}): {err}",
            count = *counter,
        );
    }
}

/// Two-pass render pipeline for rotated outputs.
///
/// Phase A: bind the output's persistent offscreen dmabuf, render
/// `elements` into it at portrait dimensions with `Transform::Normal`.
/// Phase B: import the dmabuf as a `MultiTexture`, wrap it in a single
/// `TextureRenderElement` sized to the landscape scanout buffer with
/// element transform set to the configured rotation, and overwrite
/// `elements` so the caller's `DrmCompositor::render_frame` sees only
/// that one landscape-sized blit element.
///
/// Returns `Err(&'static str)` naming the failing phase so the caller
/// can feed it into the rate-limited error logger. On success the
/// element vec is mutated in place and the caller proceeds straight
/// into `drm_compositor.render_frame` as usual.
fn run_two_pass<'a>(
    renderer: &mut KaraRenderer<'a>,
    instance: &mut OutputInstance,
    elements: &mut Vec<DrmRenderElement<'a>>,
    _primary_node: DrmNode,
    output_name: &str,
) -> Result<(), &'static str> {
    use smithay::backend::renderer::element::Kind;
    use smithay::backend::renderer::element::texture::{
        TextureBuffer, TextureRenderElement,
    };
    use smithay::backend::renderer::{Bind, Frame as _, ImportDma, Renderer as _};
    use smithay::backend::renderer::Color32F;
    use smithay::utils::{Point, Rectangle};

    let ts = match instance.two_pass.as_mut() {
        Some(ts) => ts,
        None => return Ok(()),
    };

    let portrait_size = ts.portrait_size;
    let landscape_size = ts.landscape_size;
    let rotation = ts.rotation;
    let portrait_rect: Rectangle<i32, Physical> = Rectangle::from_size(portrait_size);
    let scale_one = Scale::from(1.0);

    // ── Phase A: render portrait element vec into the offscreen dmabuf ──
    {
        let mut target = renderer
            .bind(&mut ts.dmabuf)
            .map_err(|e| {
                tracing::error!(
                    target: "kara_gate::two_pass",
                    "bind failed output={} err={:?}", output_name, e,
                );
                "two_pass_bind"
            })?;
        {
            let mut frame = renderer
                .render(&mut target, portrait_size, Transform::Normal)
                .map_err(|e| {
                    tracing::error!(
                        target: "kara_gate::two_pass",
                        "render start failed output={} err={:?}", output_name, e,
                    );
                    "two_pass_render_start"
                })?;
            frame
                .clear(
                    Color32F::from([0.05, 0.05, 0.05, 1.0]),
                    &[portrait_rect],
                )
                .map_err(|e| {
                    tracing::error!(
                        target: "kara_gate::two_pass",
                        "clear failed output={} err={:?}", output_name, e,
                    );
                    "two_pass_clear"
                })?;
            // The element vec is ordered front-to-back (index 0 =
            // topmost). Iterate in reverse so later elements paint on
            // top of earlier ones. Individual element draw failures are
            // silently skipped — one bad surface shouldn't blank the
            // whole rotated output.
            for elem in elements.iter().rev() {
                let geo = elem.geometry(scale_one);
                let src = elem.src();
                let _ = RenderElement::<KaraRenderer<'_>>::draw(
                    elem,
                    &mut frame,
                    src,
                    geo,
                    &[Rectangle::from_size(geo.size)],
                    &[],
                );
            }
            // Drop frame before target to satisfy the RAII order.
            let _ = frame.finish();
        }
        // target drops → unbind
    }

    // ── Phase B: build a single rotated blit element ──
    let multi_tex = renderer
        .import_dmabuf(&ts.dmabuf, None)
        .map_err(|e| {
            tracing::error!(
                target: "kara_gate::two_pass",
                "import_dmabuf failed output={} err={:?}", output_name, e,
            );
            "two_pass_import"
        })?;

    // `TextureBuffer::from_texture`'s `transform` arg is the **source**
    // transform of the texture content (wl_buffer semantics) — smithay
    // inverts it during render to map sample space → destination space.
    // We captured the content in Transform::Normal and want it to appear
    // rotated by `rotation` in the landscape scanout, so we hand smithay
    // the inverse. Passing `rotation` directly produces an upside-down
    // image (180° off).
    let buffer = TextureBuffer::from_texture(
        &*renderer,
        multi_tex,
        1,
        rotation.invert(),
        None,
    );

    let blit_element: TextureRenderElement<KaraTexture> =
        TextureRenderElement::from_texture_buffer(
            Point::from((0.0, 0.0)),
            &buffer,
            None,
            None,
            Some(landscape_size.to_logical(1)),
            Kind::Unspecified,
        );

    elements.clear();
    elements.push(DrmRenderElement::Texture(blit_element));
    Ok(())
}

/// Returns true if any visible scratchpad on `output_idx` has
/// `blur = true` in its config. Used by the scratchpad blur dispatcher
/// to decide whether to run the blur pipeline for a given frame.
fn scratchpad_wants_blur(state: &Gate, output_idx: usize) -> bool {
    state.scratchpads.iter().any(|sp| {
        sp.visible
            && sp.output_idx == output_idx
            && state
                .config
                .scratchpads
                .get(sp.config_idx)
                .map(|sc| sc.blur)
                .unwrap_or(false)
    })
}

/// Returns the maximum `blur_passes` across all visible blurred
/// scratchpads on `output_idx`, or 0 if none qualify. Multiple visible
/// scratchpads take the max so a user with both a light-blur and a
/// heavy-blur scratchpad open at once gets the heavier one (same logic
/// as `dim_alpha` selection).
fn scratchpad_max_blur_passes(state: &Gate, output_idx: usize) -> u32 {
    state
        .scratchpads
        .iter()
        .filter(|sp| sp.visible && sp.output_idx == output_idx)
        .filter_map(|sp| state.config.scratchpads.get(sp.config_idx))
        .filter(|sc| sc.blur)
        .map(|sc| sc.blur_passes)
        .max()
        .unwrap_or(0)
}

/// Scratchpad backdrop blur pipeline (full-res iterated Gaussian).
///
/// Input: `elements` has already been built in full. The last
/// `backdrop_len` entries are the workspace + custom subset (workspace
/// windows, wallpaper, workspace borders, bar).
///
/// Steps:
/// 1. **Capture** the trailing elements into `instance.blur.backdrop`
///    (full-res GlesTexture) via the MultiRenderer frame path.
/// 2. **Iterate** `blur_passes` times: separable 9-tap Gaussian at
///    full res, ping-ponging `backdrop` ↔ `scratch`. Every iteration
///    leaves the result in `backdrop` (original captured content is
///    consumed by iter 0's horizontal pass and can be safely
///    overwritten by iter 0's vertical pass onward). The final
///    iteration's vertical pass writes to `pong_dmabuf` instead so
///    the result can be imported as a `MultiTexture` for the final
///    render element.
/// 3. **Drain** the backdrop tail from `elements`, **import**
///    `pong_dmabuf` as a `MultiTexture`, wrap it in a
///    `TextureRenderElement` at full size, and push it on the vec.
///
/// Radius (variance compounds as σ² sums, so total σ = √N × per-iter σ):
///   - `blur_passes = 1`: 2σ ≈ 6 px (very soft edge softening)
///   - `blur_passes = 3`: 2σ ≈ 10 px (mild)
///   - `blur_passes = 5`: 2σ ≈ 13 px (mild-moderate)
///
/// Full-res rather than downsample-then-upsample because bilinear
/// upscale from a downsampled intermediate produces visible block
/// artifacts on high-frequency wallpaper content.
///
/// On any error, `elements` is left untouched and the caller falls
/// back to rendering the flat dim.
fn run_scratchpad_blur<'a>(
    renderer: &mut KaraRenderer<'a>,
    instance: &mut OutputInstance,
    blur_program: &mut crate::blur::BlurProgram,
    elements: &mut Vec<DrmRenderElement<'a>>,
    backdrop_len: usize,
    blur_passes: u32,
    output_name: &str,
) -> Result<(), &'static str> {
    use smithay::backend::allocator::Fourcc;
    use smithay::backend::renderer::element::Kind;
    use smithay::backend::renderer::element::texture::{
        TextureBuffer, TextureRenderElement,
    };
    use smithay::backend::renderer::gles::Uniform;
    use smithay::backend::renderer::{Bind, Color32F, Frame as _, ImportDma, Offscreen, Renderer as _};
    use smithay::utils::{Point, Rectangle};

    let blur = instance.blur.as_mut().ok_or("blur_missing_state")?;
    let size = blur.size;
    if size.w <= 0 || size.h <= 0 {
        return Err("blur_bad_size");
    }
    let elements_len = elements.len();
    if backdrop_len == 0 || backdrop_len > elements_len {
        return Err("blur_bad_len");
    }
    let backdrop_start = elements_len - backdrop_len;
    let passes = blur_passes.clamp(1, 5);

    let full_rect: Rectangle<i32, Physical> = Rectangle::from_size(size);
    let full_src = Rectangle::<f64, smithay::utils::Buffer>::from_size(
        (size.w as f64, size.h as f64).into(),
    );
    let scale_one = Scale::from(1.0);

    // Lazily allocate intermediate GlesTextures on first blur frame.
    if blur.backdrop.is_none() {
        match Offscreen::<smithay::backend::renderer::gles::GlesTexture>::create_buffer(
            renderer,
            Fourcc::Abgr8888,
            size.to_logical(1).to_buffer(1, Transform::Normal),
        ) {
            Ok(t) => blur.backdrop = Some(t),
            Err(_) => return Err("blur_alloc_backdrop"),
        }
    }
    if blur.scratch.is_none() {
        match Offscreen::<smithay::backend::renderer::gles::GlesTexture>::create_buffer(
            renderer,
            Fourcc::Abgr8888,
            size.to_logical(1).to_buffer(1, Transform::Normal),
        ) {
            Ok(t) => blur.scratch = Some(t),
            Err(_) => return Err("blur_alloc_scratch"),
        }
    }

    // ── Phase 1: capture backdrop into `blur.backdrop` via MultiFrame ──
    {
        let backdrop_tex = blur.backdrop.as_mut().ok_or("blur_backdrop_none")?;
        let mut target = renderer
            .bind(backdrop_tex)
            .map_err(|e| {
                tracing::error!(
                    target: "kara_gate::blur",
                    "backdrop bind failed output={} err={:?}", output_name, e,
                );
                "blur_bind_backdrop"
            })?;
        {
            let mut frame = renderer
                .render(&mut target, size, Transform::Normal)
                .map_err(|e| {
                    tracing::error!(
                        target: "kara_gate::blur",
                        "backdrop render start failed output={} err={:?}", output_name, e,
                    );
                    "blur_render_backdrop"
                })?;
            frame
                .clear(Color32F::from([0.0, 0.0, 0.0, 1.0]), &[full_rect])
                .map_err(|_| "blur_clear_backdrop")?;
            // Draw backdrop elements in reverse (vec is front-to-back,
            // draw back-to-front for correct layering).
            for elem in elements[backdrop_start..].iter().rev() {
                let geo = elem.geometry(scale_one);
                let src = elem.src();
                let _ = RenderElement::<KaraRenderer<'_>>::draw(
                    elem,
                    &mut frame,
                    src,
                    geo,
                    &[Rectangle::from_size(geo.size)],
                    &[],
                );
            }
            let _ = frame.finish();
        }
    }

    // ── Phase 2: N iterations of separable Gaussian at full res ──
    //
    // Ping-pong: H: backdrop → scratch; V: scratch → backdrop (or
    // pong_dmabuf on the final iteration). After iter 0's H pass
    // consumes the original captured content, backdrop is free for
    // reuse as the V-pass destination.
    let texel_w = 1.0f32 / (size.w as f32);
    let texel_h = 1.0f32 / (size.h as f32);
    const BLUR_SPREAD: f32 = 2.0;

    {
        let gles = renderer.as_mut();
        let program = blur_program
            .get_or_compile(gles)
            .ok_or("blur_shader_uncompiled")?
            .clone();

        for i in 0..passes {
            let is_last = i == passes - 1;

            // Horizontal: backdrop → scratch
            {
                let scratch_tex = blur.scratch.as_mut().ok_or("blur_scratch_none")?;
                let mut target = gles.bind(scratch_tex).map_err(|e| {
                    tracing::error!(
                        target: "kara_gate::blur",
                        "H bind failed (iter {}) output={} err={:?}",
                        i, output_name, e,
                    );
                    "blur_bind_h"
                })?;
                let mut frame = gles
                    .render(&mut target, size, Transform::Normal)
                    .map_err(|_| "blur_render_h")?;
                frame
                    .clear(Color32F::from([0.0, 0.0, 0.0, 1.0]), &[full_rect])
                    .map_err(|_| "blur_clear_h")?;
                let backdrop_tex = blur.backdrop.as_ref().ok_or("blur_backdrop_none2")?;
                frame
                    .render_texture_from_to(
                        backdrop_tex,
                        full_src,
                        full_rect,
                        &[full_rect],
                        &[],
                        Transform::Normal,
                        1.0,
                        Some(&program),
                        &[
                            Uniform::new("direction", (texel_w, 0.0f32)),
                            Uniform::new("spread", BLUR_SPREAD),
                        ],
                    )
                    .map_err(|e| {
                        tracing::error!(
                            target: "kara_gate::blur",
                            "H pass failed (iter {}) output={} err={:?}",
                            i, output_name, e,
                        );
                        "blur_pass_h"
                    })?;
                let _ = frame.finish();
            }

            // Vertical: scratch → (backdrop | pong_dmabuf if last)
            if !is_last {
                let backdrop_tex = blur.backdrop.as_mut().ok_or("blur_backdrop_none3")?;
                let mut target = gles.bind(backdrop_tex).map_err(|e| {
                    tracing::error!(
                        target: "kara_gate::blur",
                        "V bind failed (iter {}) output={} err={:?}",
                        i, output_name, e,
                    );
                    "blur_bind_v"
                })?;
                let mut frame = gles
                    .render(&mut target, size, Transform::Normal)
                    .map_err(|_| "blur_render_v")?;
                frame
                    .clear(Color32F::from([0.0, 0.0, 0.0, 1.0]), &[full_rect])
                    .map_err(|_| "blur_clear_v")?;
                let scratch_tex = blur.scratch.as_ref().ok_or("blur_scratch_none2")?;
                frame
                    .render_texture_from_to(
                        scratch_tex,
                        full_src,
                        full_rect,
                        &[full_rect],
                        &[],
                        Transform::Normal,
                        1.0,
                        Some(&program),
                        &[
                            Uniform::new("direction", (0.0f32, texel_h)),
                            Uniform::new("spread", BLUR_SPREAD),
                        ],
                    )
                    .map_err(|e| {
                        tracing::error!(
                            target: "kara_gate::blur",
                            "V pass failed (iter {}) output={} err={:?}",
                            i, output_name, e,
                        );
                        "blur_pass_v"
                    })?;
                let _ = frame.finish();
            } else {
                let mut target = gles.bind(&mut blur.pong_dmabuf).map_err(|e| {
                    tracing::error!(
                        target: "kara_gate::blur",
                        "pong bind failed (final iter) output={} err={:?}",
                        output_name, e,
                    );
                    "blur_bind_pong"
                })?;
                let mut frame = gles
                    .render(&mut target, size, Transform::Normal)
                    .map_err(|_| "blur_render_pong")?;
                frame
                    .clear(Color32F::from([0.0, 0.0, 0.0, 1.0]), &[full_rect])
                    .map_err(|_| "blur_clear_pong")?;
                let scratch_tex = blur.scratch.as_ref().ok_or("blur_scratch_none3")?;
                frame
                    .render_texture_from_to(
                        scratch_tex,
                        full_src,
                        full_rect,
                        &[full_rect],
                        &[],
                        Transform::Normal,
                        1.0,
                        Some(&program),
                        &[
                            Uniform::new("direction", (0.0f32, texel_h)),
                            Uniform::new("spread", BLUR_SPREAD),
                        ],
                    )
                    .map_err(|e| {
                        tracing::error!(
                            target: "kara_gate::blur",
                            "final V pass failed output={} err={:?}",
                            output_name, e,
                        );
                        "blur_pass_v_final"
                    })?;
                let _ = frame.finish();
            }
        }
    }

    // ── Phase 4: drain backdrop tail, import pong as MultiTexture, push ──
    let blur_ref = instance.blur.as_ref().ok_or("blur_missing_state2")?;
    let multi_tex = renderer
        .import_dmabuf(&blur_ref.pong_dmabuf, None)
        .map_err(|e| {
            tracing::error!(
                target: "kara_gate::blur",
                "import_dmabuf pong failed output={} err={:?}", output_name, e,
            );
            "blur_import_pong"
        })?;
    let buffer = TextureBuffer::from_texture(
        &*renderer,
        multi_tex,
        1,
        Transform::Normal,
        None,
    );
    let blurred_element: TextureRenderElement<KaraTexture> =
        TextureRenderElement::from_texture_buffer(
            Point::from((0.0, 0.0)),
            &buffer,
            None,
            None,
            Some(size.to_logical(1)),
            Kind::Unspecified,
        );

    elements.drain(backdrop_start..);
    elements.push(DrmRenderElement::Texture(blurred_element));
    Ok(())
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
                        // Write to a sibling temp path first and rename
                        // into place. kara-glimpse polls `exists()` to
                        // know when the PNG is ready — but a naive
                        // `save(path)` creates the file empty, then
                        // streams bytes in, leaving a window where
                        // glimpse sees the file, tries to load it, and
                        // hits an EOF in the PNG decoder. POSIX rename
                        // is atomic: glimpse either doesn't see the
                        // path yet or sees the fully-written file.
                        let tmp = format!("{path}.tmp");
                        match final_img.save(&tmp) {
                            Ok(()) => {
                                if let Err(e) = std::fs::rename(&tmp, path) {
                                    tracing::error!(
                                        "screenshot rename {tmp} -> {path} failed: {e}"
                                    );
                                    let _ = std::fs::remove_file(&tmp);
                                } else {
                                    tracing::info!("screenshot saved: {path}");
                                }
                            }
                            Err(e) => {
                                tracing::error!("screenshot save failed: {e}");
                                let _ = std::fs::remove_file(&tmp);
                            }
                        }
                    }
                }
                Err(e) => tracing::error!("screenshot: map_texture failed: {e:?}"),
            }
        }
        Err(e) => tracing::error!("screenshot: copy_framebuffer failed: {e:?}"),
    }
}

/// When `true`, kara honors configured monitor rotation via its own
/// two-pass render path (render portrait elements into an offscreen
/// texture, then blit that texture rotated into the landscape scanout
/// buffer). The Output object still carries the real transform so
/// `wl_output.transform` and `Space::output_geometry` behave correctly,
/// but the DrmCompositor is configured with `OutputModeSource::Static`
/// at landscape + Normal so its damage tracker never runs smithay's
/// buggy rotated-render path.
///
/// **Currently off.** The pipeline compiled cleanly and all log
/// targets confirm it runs without per-frame errors, but on first-run
/// the whole render loop produces blank output on every monitor —
/// something about `run_two_pass` poisons shared renderer state so
/// even the non-rotated outputs stop drawing anything visible. The
/// plumbing (per-output `TwoPassState`, Dmabuf allocator, Static
/// mode-source DrmCompositor, `run_two_pass` dispatcher) is all in
/// place, so picking this back up is just "figure out what run_two_pass
/// is doing wrong". Flip this back to `true` once that's diagnosed
/// (see session handoff for debug plan).
///
/// When `false`, rotation falls back to the smithay-native path (see
/// [`ROTATION_SMITHAY_NATIVE`]) or to the kill-switch "render landscape
/// sideways" fallback.
const ROTATION_TWO_PASS: bool = true;

/// When `true`, configured rotation uses smithay's built-in rotated
/// render path (setting the Output transform and letting damage tracker
/// handle rotation). Known broken on this repo's AMD + DisplayLink/evdi
/// hybrid swapchain combination — half the physical framebuffer never
/// gets painted. Left toggleable so anyone who doesn't hit the bug (pure
/// single-GPU without evdi scanout) can use the upstream path.
///
/// If both this and [`ROTATION_TWO_PASS`] are `true`, two-pass wins.
/// If both are `false`, rotation is kill-switched to Normal with a
/// warning.
const ROTATION_SMITHAY_NATIVE: bool = false;

/// Direct mapping of a kara monitor rotation config to a smithay
/// `Transform`, ignoring both kill switches. Used everywhere that needs
/// to know the user's *intent*, regardless of which rendering path is
/// active.
///
/// Convention: kara's `rotate left` matches xrandr's `--rotate left` —
/// monitor's top edge points left, content rotated 90° counter-clockwise.
/// In smithay's `Transform` enum that's `_270` (rotate 270° clockwise).
/// `rotate right` is 90° clockwise (`_90`); `rotate flipped` is 180°.
fn config_rotation_transform(r: kara_config::MonitorRotation) -> Transform {
    match r {
        kara_config::MonitorRotation::Normal => Transform::Normal,
        kara_config::MonitorRotation::Left => Transform::_270,
        kara_config::MonitorRotation::Right => Transform::_90,
        kara_config::MonitorRotation::Flipped => Transform::_180,
    }
}

/// Pick the actual `Transform` to hand to smithay's `Output` (which
/// controls `wl_output.transform`, `Space::output_geometry`, and —
/// under the smithay-native path only — the DrmCompositor's damage
/// tracker). Also returns whether kara's internal two-pass rendering
/// should run for this output.
///
/// The return tuple is `(output_transform, use_two_pass)`:
///   - `output_transform` is what to pass to `change_current_state`.
///   - `use_two_pass` is whether the two-pass offscreen render path
///     should drive this output's frames.
fn resolve_rotation(r: kara_config::MonitorRotation) -> (Transform, bool) {
    let config = config_rotation_transform(r);
    if config == Transform::Normal {
        return (Transform::Normal, false);
    }

    if ROTATION_TWO_PASS {
        // Keep Output::transform = config so wl_output advertising and
        // Space geometry are portrait. The DrmCompositor will be created
        // with a Static mode source that bypasses smithay's rotation.
        (config, true)
    } else if ROTATION_SMITHAY_NATIVE {
        // Let smithay rotate. Broken on AMD + evdi, works elsewhere.
        (config, false)
    } else {
        tracing::warn!(
            "monitor rotation '{:?}' requested in config but all rotation \
             paths are disabled. Falling back to Transform::Normal — \
             physically-rotated panels will render sideways until the \
             two-pass fix is re-enabled. See ROTATION_TWO_PASS in \
             crates/kara-gate/src/backend_udev.rs.",
            r
        );
        (Transform::Normal, false)
    }
}

/// Pick a DRM mode for a connector.
///
/// Precedence: exact (w,h,refresh) → same (w,h) at highest available refresh
/// → PREFERRED from EDID → first mode. Logs a warning whenever the requested
/// mode isn't available and we fall back, including the full list of modes
/// the connector/link actually offers.
///
/// `requested_is_physical` swaps the requested (W, H) for same-resolution
/// matching when the monitor is rotated 90/270 — the config's `resolution`
/// field is the user-visible (logical) size, but DRM modes are always in
/// physical panel orientation.
fn select_mode(
    output_name: &str,
    modes: &[smithay::reexports::drm::control::Mode],
    requested: Option<(i32, i32)>,
    requested_refresh: Option<u32>,
    rotated_portrait: bool,
) -> smithay::reexports::drm::control::Mode {
    let fallback = || {
        modes
            .iter()
            .find(|m| m.mode_type().contains(ModeTypeFlags::PREFERRED))
            .copied()
            .unwrap_or(modes[0])
    };

    let Some((req_logical_w, req_logical_h)) = requested else {
        return fallback();
    };

    // DRM modes are always physical-panel-oriented: a 1920x1080 panel
    // rotated right exposes 1920x1080 modes, not 1080x1920. Swap the
    // comparison dimensions when the user asked for a logical (rotated)
    // size so portrait-config users still land on their native mode.
    let (w, h) = if rotated_portrait {
        (req_logical_h, req_logical_w)
    } else {
        (req_logical_w, req_logical_h)
    };

    let refresh_suffix = |r: Option<u32>| match r {
        Some(v) => format!("@{v}Hz"),
        None => String::new(),
    };

    if let Some(m) = modes.iter().find(|m| {
        let (mw, mh) = m.size();
        mw as i32 == w
            && mh as i32 == h
            && requested_refresh.map_or(true, |r| m.vrefresh() == r)
    }) {
        return *m;
    }

    if let Some(m) = modes
        .iter()
        .filter(|m| {
            let (mw, mh) = m.size();
            mw as i32 == w && mh as i32 == h
        })
        .max_by_key(|m| m.vrefresh())
    {
        tracing::warn!(
            "{output_name}: requested {w}x{h}{} not available — using {w}x{h}@{}Hz instead",
            refresh_suffix(requested_refresh),
            m.vrefresh()
        );
        return *m;
    }

    let mut dedup: Vec<(i32, i32, u32)> = modes
        .iter()
        .map(|m| {
            let (mw, mh) = m.size();
            (mw as i32, mh as i32, m.vrefresh())
        })
        .collect();
    dedup.sort();
    dedup.dedup();
    let available: Vec<String> = dedup
        .into_iter()
        .map(|(mw, mh, r)| format!("{mw}x{mh}@{r}Hz"))
        .collect();
    let picked = fallback();
    let (pw, ph) = picked.size();
    tracing::warn!(
        "{output_name}: requested {w}x{h}{} not offered by EDID/link \
         (cable/dock may be bandwidth-limited — the EDID can still claim \
         the resolution while the link filter hides it). Available modes: \
         [{}]. Falling back to {pw}x{ph}@{}Hz.",
        refresh_suffix(requested_refresh),
        available.join(", "),
        picked.vrefresh(),
    );
    picked
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
