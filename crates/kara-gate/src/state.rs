use smithay::delegate_compositor;
use smithay::delegate_data_device;
use smithay::delegate_layer_shell;
use smithay::delegate_output;
use smithay::delegate_primary_selection;
use smithay::delegate_seat;
use smithay::delegate_shm;
use smithay::delegate_xdg_activation;
use smithay::delegate_xdg_decoration;
use smithay::delegate_xdg_shell;
use smithay::delegate_xdg_toplevel_icon;
use smithay::desktop::{Space, Window, layer_map_for_output};
use smithay::input::{Seat, SeatHandler, SeatState};
use smithay::input::pointer::CursorImageStatus;
use smithay::reexports::calloop::LoopSignal;
use smithay::reexports::wayland_server::{Client, Display, DisplayHandle};
use smithay::reexports::wayland_server::backend::{ClientData, ClientId, DisconnectReason};
use smithay::reexports::wayland_server::protocol::wl_buffer;
use smithay::reexports::wayland_server::Resource;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::output::Output;
use smithay::utils::{Clock, Logical, Monotonic, Point, Rectangle, Serial, SERIAL_COUNTER};
use smithay::wayland::buffer::BufferHandler;
use smithay::wayland::compositor::{self, CompositorClientState, CompositorHandler, CompositorState};
use smithay::wayland::selection::SelectionHandler;
use smithay::wayland::selection::data_device::{
    ClientDndGrabHandler, DataDeviceHandler, DataDeviceState, ServerDndGrabHandler,
    set_data_device_focus,
};
use smithay::wayland::selection::primary_selection::{
    set_primary_focus, PrimarySelectionHandler, PrimarySelectionState,
};
use smithay::wayland::xdg_activation::{
    XdgActivationHandler, XdgActivationState, XdgActivationToken, XdgActivationTokenData,
};
use smithay::wayland::xdg_toplevel_icon::{XdgToplevelIconHandler, XdgToplevelIconManager};
use smithay::wayland::shell::xdg::{
    PopupSurface, PositionerState, ToplevelSurface, XdgShellHandler, XdgShellState,
    XdgToplevelSurfaceData,
};
use smithay::wayland::shell::wlr_layer::{
    Layer, LayerSurface, WlrLayerShellHandler, WlrLayerShellState,
};
use smithay::wayland::shell::xdg::decoration::{XdgDecorationHandler, XdgDecorationState};
use smithay::wayland::output::OutputManagerState;
use smithay::wayland::shm::{ShmHandler, ShmState};

use crate::input::Keybind;
use crate::layout::layout_workspace;
use crate::workspace::{Workspace, WORKSPACE_COUNT};

/// Known transient helper clients that create invisible xdg_toplevels to satisfy
/// protocol requirements (input serials, etc.) and immediately destroy them. These
/// must never be tiled or they cause real windows to flicker-resize.
fn is_helper_client(app_id: &str) -> bool {
    matches!(
        app_id,
        "io.github.bugaevc.wl-clipboard"
    )
}

/// Mark all four `tiled_*` state flags on an `xdg_toplevel` pending state so that
/// CSD clients (Firefox/Floorp, GTK apps) know they are inside a tiling layout and
/// should suppress rounded corners, drop shadows, and client-side resize handles.
fn mark_tiled(state: &mut smithay::wayland::shell::xdg::ToplevelState) {
    use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel;
    state.states.set(xdg_toplevel::State::TiledLeft);
    state.states.set(xdg_toplevel::State::TiledRight);
    state.states.set(xdg_toplevel::State::TiledTop);
    state.states.set(xdg_toplevel::State::TiledBottom);
}


/// Per-scratchpad runtime state.
pub struct ScratchpadState {
    pub config_idx: usize,
    pub workspace: Workspace,
    pub visible: bool,
    pub hiding: bool, // out-animation in progress, waiting to fully hide
    pub started: bool,
    pub output_idx: usize,
    /// When true, the next new window is captured into this scratchpad (autostart capture).
    pub pending_capture: bool,
}

/// Per-output state for multi-monitor support.
#[allow(dead_code)]
pub struct OutputState {
    pub output: Output,
    pub current_ws: usize,
    pub size: (i32, i32),
    pub workarea: Rectangle<i32, Logical>,
    pub location: Point<i32, Logical>,
    pub fullscreen_window: Option<Window>,
}

pub struct ClientState {
    pub compositor: CompositorClientState,
}

impl ClientData for ClientState {
    fn initialized(&self, _client_id: ClientId) {}
    fn disconnected(&self, _client_id: ClientId, _reason: DisconnectReason) {}
}

pub struct Gate {
    pub display_handle: DisplayHandle,
    pub loop_signal: LoopSignal,
    pub running: bool,

    // Smithay protocol state
    pub compositor_state: CompositorState,
    pub xdg_shell_state: XdgShellState,
    #[allow(dead_code)]
    pub xdg_decoration_state: XdgDecorationState,
    pub layer_shell_state: WlrLayerShellState,
    pub shm_state: ShmState,
    pub seat_state: SeatState<Self>,
    pub data_device_state: DataDeviceState,
    pub primary_selection_state: PrimarySelectionState,
    pub xdg_activation_state: XdgActivationState,
    #[allow(dead_code)] // kept alive so the protocol global stays registered
    pub xdg_toplevel_icon_manager: XdgToplevelIconManager,
    #[allow(dead_code)]
    pub output_manager_state: OutputManagerState,
    pub seat: Seat<Self>,

    // Layer surfaces (kara-summon, kara-whisper, etc.)
    #[allow(dead_code)]
    pub layer_surfaces: Vec<LayerSurface>, // kept for backward compat, LayerMap is primary

    // Desktop
    pub space: Space<Window>,
    pub clock: Clock<Monotonic>,

    // Config
    pub config: kara_config::Config,

    // Bar
    pub bar_renderer: kara_sight::BarRenderer,
    pub status_cache: kara_sight::StatusCache,

    // Window management.
    //
    // Per-monitor isolated workspace pools: `workspaces[output_idx][ws_idx]`
    // gives the workspace currently owned by output_idx at slot ws_idx.
    // Each output has its own WORKSPACE_COUNT (9) workspaces with its own
    // windows, currently displayed via `outputs[output_idx].current_ws`.
    //
    // mod+1..9 on monitor 3 only switches monitor 3's view; spawning on
    // monitor 3 always lands in monitor 3's currently-displayed workspace,
    // not in some other monitor's pool. `Gate::workspaces` stays in lockstep
    // with `Gate::outputs` — every `add_output` pushes a fresh pool, every
    // future `remove_output` would pop one (M5 work).
    pub workspaces: Vec<Vec<Workspace>>,
    pub current_ws: usize,
    pub previous_ws: usize,
    pub keybinds: std::sync::Arc<Vec<Keybind>>,
    /// Toplevels created via xdg_shell but not yet mapped (no buffer attached).
    /// Helper clients like wl-copy create a toplevel purely to obtain an input serial
    /// for set_selection and never commit a buffer — they must not enter the layout.
    pub unmapped_windows: Vec<Window>,
    /// Pending routing hints for autostart entries. When an `autostart {
    /// run "X" app_id "X" monitor N workspace M }` fires, its target is
    /// stored here. The next window that maps with a matching app_id gets
    /// routed to (N, M) and the entry is removed. Entries are dropped if
    /// not matched within a reasonable time to avoid stale routing.
    pub pending_autostart_routes: Vec<(String /* app_id */, usize /* output */, usize /* ws */)>,
    /// Helper-client toplevels (e.g. wl-clipboard) given transient keyboard focus so
    /// they can call `wl_data_device.set_selection`, but never added to a workspace.
    /// Focus is restored to the active workspace when the helper is destroyed.
    pub hidden_helpers: Vec<Window>,

    // Multi-monitor: per-output state
    pub outputs: Vec<OutputState>,
    pub output_bounds: (i32, i32), // cached (max_x, max_y) for pointer clamping
    pub focused_output: usize,

    // Wallpaper
    pub wallpaper: Option<crate::wallpaper::Wallpaper>,

    // IPC
    pub ipc_listener: Option<std::os::unix::net::UnixListener>,

    // Floating (no extra state beyond workspace.floating)

    // Scratchpads — each is an independent floating workspace
    pub scratchpads: Vec<ScratchpadState>,
    pub focused_scratchpad: Option<usize>,

    // Autostart
    pub autostart_done: bool,

    // Border rendering: cached geometry from last apply_layout
    pub border_rects: Vec<(smithay::utils::Rectangle<i32, Logical>, bool)>, // (rect, is_focused)
    pub scratchpad_border_rects: Vec<(smithay::utils::Rectangle<i32, Logical>, bool)>,
    pub layout_dirty: bool,
    pub scratchpad_layout_dirty: bool,
    // Cached border pixmap data per rect: (rgba_bytes, width, height)
    pub border_cache: Vec<(Vec<u8>, u32, u32)>,
    pub scratchpad_border_cache: Vec<(Vec<u8>, u32, u32)>,
    pub border_offsets: Vec<(f64, f64)>,
    pub scratchpad_border_offsets: Vec<(f64, f64)>,

    // Animation
    pub animations: crate::animation::AnimationManager,
    /// Windows queued for transfer to a per-monitor workspace slot once
    /// their out-animation completes. Tuple is `(window, output_idx, ws_idx)`.
    pub pending_sends: Vec<(Window, usize, usize)>,
    // Base positions from apply_layout(), keyed by window identity for offset calculation
    pub window_base_positions: Vec<(Window, smithay::utils::Point<i32, Logical>)>,

    // Pointer location (tracked explicitly for relative motion from libinput)
    pub pointer_location: smithay::utils::Point<f64, Logical>,

    // Bar rendering cache
    pub bar_dirty: bool,
    /// Per-output bar cache. The bar's content depends on `output_idx`
    /// (monitor number, focused-monitor highlight) so a single shared cache
    /// caused every monitor to display whichever output's pixmap was last
    /// rasterized while `bar_dirty` was true. Keyed by `output_idx`; the
    /// entire map is cleared whenever `bar_dirty` flips on.
    pub bar_cache: std::collections::HashMap<usize, (Vec<u8>, u32, u32)>,

    // Cursor rendering
    pub cursor_status: CursorImageStatus,
    pub cursor_cache: Option<crate::cursor::CursorCache>,
    pub named_cursor_cache: std::collections::HashMap<smithay::input::pointer::CursorIcon, crate::cursor::CursorCache>,
    pub cursor_last_moved: std::time::Instant,
    pub cursor_idle_pos: smithay::utils::Point<f64, Logical>,

    // Config auto-reload: cached mtime to detect file changes
    pub config_mtime: Option<std::time::SystemTime>,

    // Backend-specific data (UdevData for udev, None for winit)
    #[allow(dead_code)]
    pub backend_data: Option<Box<dyn std::any::Any>>,
    pub screenshot_path: Option<String>,
    pub screenshot_region: Option<(i32, i32, i32, i32)>,

    // Keybind overlay
    pub keybind_overlay_visible: bool,
}

impl Gate {
    pub fn new(display: &Display<Self>, loop_signal: LoopSignal) -> Self {
        let dh = display.handle();

        let compositor_state = CompositorState::new::<Self>(&dh);
        let xdg_shell_state = XdgShellState::new::<Self>(&dh);
        let xdg_decoration_state = XdgDecorationState::new::<Self>(&dh);
        let layer_shell_state = WlrLayerShellState::new::<Self>(&dh);
        let shm_state = ShmState::new::<Self>(&dh, vec![]);
        let mut seat_state = SeatState::new();
        let data_device_state = DataDeviceState::new::<Self>(&dh);
        let primary_selection_state = PrimarySelectionState::new::<Self>(&dh);
        let xdg_activation_state = XdgActivationState::new::<Self>(&dh);
        let xdg_toplevel_icon_manager = XdgToplevelIconManager::new::<Self>(&dh);
        let output_manager_state = OutputManagerState::new_with_xdg_output::<Self>(&dh);

        let mut seat = seat_state.new_wl_seat(&dh, "seat0");
        seat.add_keyboard(Default::default(), 500, 33).unwrap();
        seat.add_pointer();

        let config = kara_config::load_default_config();
        let keybinds = std::sync::Arc::new(crate::input::keybinds_from_config(&config));

        let scratchpad_states: Vec<ScratchpadState> = config.scratchpads.iter().enumerate()
            .map(|(i, _sc)| {
                let mut ws = Workspace::new(100 + i);
                ws.gap_px = config.general.gap_px;
                ws.mfact = config.general.default_mfact;
                ScratchpadState {
                    config_idx: i,
                    workspace: ws,
                    visible: false,
                    hiding: false,
                    started: false,
                    output_idx: 0,
                    pending_capture: false,
                }
            }).collect();

        // Per-monitor workspace pools start empty — each `add_output` call
        // pushes a fresh Vec<Workspace> of length WORKSPACE_COUNT.
        let workspaces: Vec<Vec<Workspace>> = Vec::new();

        let bar_renderer = kara_sight::BarRenderer::new(
            &config.general.font,
            config.general.font_size,
        );
        let status_cache = kara_sight::StatusCache::new();

        tracing::info!(
            "loaded config: {} keybinds, {} commands, {} rules",
            keybinds.len(),
            config.commands.len(),
            config.rules.len(),
        );

        let mut gate = Self {
            display_handle: dh,
            loop_signal,
            running: true,
            compositor_state,
            xdg_shell_state,
            xdg_decoration_state,
            layer_shell_state,
            shm_state,
            seat_state,
            data_device_state,
            primary_selection_state,
            xdg_activation_state,
            xdg_toplevel_icon_manager,
            output_manager_state,
            seat,
            layer_surfaces: Vec::new(),
            space: Space::default(),
            clock: Clock::new(),
            config,
            bar_renderer,
            status_cache,
            workspaces,
            current_ws: 0,
            previous_ws: 0,
            keybinds,
            unmapped_windows: Vec::new(),
            pending_autostart_routes: Vec::new(),
            hidden_helpers: Vec::new(),
            wallpaper: None,
            outputs: Vec::new(),
            output_bounds: (800, 600),
            focused_output: 0,
            ipc_listener: kara_ipc::server::bind_socket().ok(),
            scratchpads: scratchpad_states,
            focused_scratchpad: None,
            autostart_done: false,
            border_rects: Vec::new(),
            scratchpad_border_rects: Vec::new(),
            layout_dirty: true,
            scratchpad_layout_dirty: false,
            border_cache: Vec::new(),
            scratchpad_border_cache: Vec::new(),
            scratchpad_border_offsets: Vec::new(),
            border_offsets: Vec::new(),
            animations: crate::animation::AnimationManager::new(),
            pending_sends: Vec::new(),
            window_base_positions: Vec::new(),
            bar_dirty: true,
            bar_cache: std::collections::HashMap::new(),
            pointer_location: (0.0, 0.0).into(),
            cursor_status: CursorImageStatus::default_named(),
            cursor_cache: None,
            named_cursor_cache: std::collections::HashMap::new(),
            cursor_last_moved: std::time::Instant::now(),
            cursor_idle_pos: (0.0, 0.0).into(),
            config_mtime: Self::get_config_mtime(),
            backend_data: None,
            screenshot_path: None,
            screenshot_region: None,
            keybind_overlay_visible: false,
        };

        // Apply environment variables and cursor theme
        gate.apply_environment();
        gate.apply_cursor_theme();
        gate.load_cursor_theme();

        gate
    }

    /// Reload config from disk and apply changes.
    pub fn reload_config(&mut self) {
        tracing::info!("reloading config");
        self.config = kara_config::load_default_config();
        self.keybinds = std::sync::Arc::new(crate::input::keybinds_from_config(&self.config));

        // Re-apply environment and cursor theme
        self.apply_environment();
        self.apply_cursor_theme();
        self.load_cursor_theme();

        // Apply general config to every workspace in every per-monitor pool
        for pool in self.workspaces.iter_mut() {
            for ws in pool.iter_mut() {
                ws.gap_px = self.config.general.gap_px;
            }
        }

        // Update bar font
        self.bar_renderer.set_font(
            &self.config.general.font,
            self.config.general.font_size,
        );

        // Recompute workarea for bar height changes
        self.recompute_workarea();

        tracing::info!(
            "config reloaded: {} keybinds, {} commands, {} rules",
            self.keybinds.len(),
            self.config.commands.len(),
            self.config.rules.len(),
        );

        self.apply_layout();
        self.config_mtime = Self::get_config_mtime();
    }

    /// Get the config file's mtime for change detection.
    fn get_config_mtime() -> Option<std::time::SystemTime> {
        std::fs::metadata(kara_config::default_config_path())
            .and_then(|m| m.modified())
            .ok()
    }

    /// Check if config file has changed and reload if so.
    pub fn check_config_changed(&mut self) {
        let current_mtime = Self::get_config_mtime();
        if current_mtime != self.config_mtime {
            tracing::info!("config file changed, auto-reloading");
            self.reload_config();
        }
    }

    /// Recompute cached output bounding box for pointer clamping.
    fn recompute_output_bounds(&mut self) {
        self.output_bounds = self.outputs.iter().fold((0i32, 0i32), |(mx, my), out| {
            (mx.max(out.location.x + out.size.0), my.max(out.location.y + out.size.1))
        });
    }

    // ── Cursor idle tracking ──────────────────────────────────────────

    /// Update cursor idle state. Resets timer if pointer moved > 5px from idle position.
    pub fn update_cursor_idle(&mut self) {
        let dx = self.pointer_location.x - self.cursor_idle_pos.x;
        let dy = self.pointer_location.y - self.cursor_idle_pos.y;
        if dx * dx + dy * dy > 25.0 {
            self.cursor_last_moved = std::time::Instant::now();
            self.cursor_idle_pos = self.pointer_location;
        }
    }

    /// Returns true if cursor has been idle for more than 1 second.
    pub fn cursor_is_idle(&self) -> bool {
        self.cursor_last_moved.elapsed() > std::time::Duration::from_secs(1)
    }

    // ── Output helpers (shims for backward compat + multi-monitor) ──

    /// Convenience: focused output's size.
    pub fn output_size(&self) -> (i32, i32) {
        self.outputs.get(self.focused_output)
            .map(|o| o.size)
            .unwrap_or((800, 600))
    }

    /// Convenience: focused output's workarea.
    pub fn workarea(&self) -> Rectangle<i32, Logical> {
        self.outputs.get(self.focused_output)
            .map(|o| o.workarea)
            .unwrap_or_else(|| Rectangle::new((0, 0).into(), (800, 600).into()))
    }

    /// Add an output and compute its workarea.
    pub fn add_output(&mut self, output: Output, size: (i32, i32), location: Point<i32, Logical>) {
        // Advertise wl_output global to Wayland clients
        output.create_global::<Self>(&self.display_handle);

        let mut out_state = OutputState {
            output,
            current_ws: 0,
            size,
            workarea: Rectangle::new((0, 0).into(), size.into()),
            location,
            fullscreen_window: None,
        };
        self.recompute_workarea_for(&mut out_state);
        self.outputs.push(out_state);

        // Per-monitor workspace pool: each new output gets its own fresh
        // 9-workspace pool, with this output's gap/mfact preferences from
        // config. The outer Vec stays in lockstep with `self.outputs`.
        let new_pool: Vec<Workspace> = (0..WORKSPACE_COUNT)
            .map(|id| {
                let mut ws = Workspace::new(id);
                ws.gap_px = self.config.general.gap_px;
                ws.mfact = self.config.general.default_mfact;
                ws
            })
            .collect();
        self.workspaces.push(new_pool);

        self.recompute_output_bounds();
    }

    /// Set the output size and recompute work area for a specific output.
    pub fn set_output_size(&mut self, output_idx: usize, w: i32, h: i32) {
        if let Some(out) = self.outputs.get_mut(output_idx) {
            out.size = (w, h);
            let bar_h = if self.config.bar.enabled { self.config.bar.height } else { 0 };
            let (y, area_h) = match self.config.bar.position {
                kara_config::BarPosition::Top => (bar_h, h - bar_h),
                kara_config::BarPosition::Bottom => (0, h - bar_h),
            };
            out.workarea = Rectangle::new(
                (out.location.x, out.location.y + y).into(),
                (w, area_h.max(0)).into(),
            );
        }
        self.recompute_output_bounds();
    }

    /// Recompute workarea for a single output state.
    fn recompute_workarea_for(&self, out: &mut OutputState) {
        let (w, h) = out.size;
        let bar_h = if self.config.bar.enabled { self.config.bar.height } else { 0 };
        let (y, area_h) = match self.config.bar.position {
            kara_config::BarPosition::Top => (bar_h, h - bar_h),
            kara_config::BarPosition::Bottom => (0, h - bar_h),
        };
        out.workarea = Rectangle::new(
            (out.location.x, out.location.y + y).into(),
            (w, area_h.max(0)).into(),
        );
    }

    /// Recompute workareas for all outputs (e.g., after bar config change).
    pub fn recompute_workarea(&mut self) {
        let bar_enabled = self.config.bar.enabled;
        let bar_h = if bar_enabled { self.config.bar.height } else { 0 };
        let bar_pos = self.config.bar.position;
        for out in &mut self.outputs {
            let (w, h) = out.size;
            let (y, area_h) = match bar_pos {
                kara_config::BarPosition::Top => (bar_h, h - bar_h),
                kara_config::BarPosition::Bottom => (0, h - bar_h),
            };
            out.workarea = Rectangle::new(
                (out.location.x, out.location.y + y).into(),
                (w, area_h.max(0)).into(),
            );
        }
    }

    /// Which workspace is active on a given output.
    pub fn effective_ws(&self, output_idx: usize) -> usize {
        if self.config.general.sync_workspaces {
            self.current_ws
        } else {
            self.outputs.get(output_idx)
                .map(|o| o.current_ws)
                .unwrap_or(self.current_ws)
        }
    }

    /// Find which output contains a point (for pointer tracking).
    #[allow(dead_code)]
    pub fn output_for_point(&self, point: Point<f64, Logical>) -> usize {
        for (i, out) in self.outputs.iter().enumerate() {
            let rect = Rectangle::new(
                out.location,
                (out.size.0, out.size.1).into(),
            );
            let p: smithay::utils::Point<i32, Logical> = (point.x as i32, point.y as i32).into();
            if rect.contains(p) {
                return i;
            }
        }
        self.focused_output
    }

    /// Build the workspace context for bar rendering on a specific output.
    /// Each output's bar reflects ITS OWN per-monitor workspace pool — the
    /// occupied dots and the focused-window title are local to the output.
    pub fn bar_workspace_context(&self, output_idx: usize) -> kara_sight::WorkspaceContext {
        let ws_idx = self.effective_ws(output_idx);

        let mut occupied = [false; 9];
        if let Some(pool) = self.workspaces.get(output_idx) {
            for (i, ws) in pool.iter().enumerate() {
                if i < 9 {
                    occupied[i] = !ws.clients.is_empty();
                }
            }
        }

        let focused_title = self
            .workspaces
            .get(output_idx)
            .and_then(|pool| pool.get(ws_idx))
            .and_then(|ws| ws.focused().cloned())
            .and_then(|w| w.toplevel().cloned())
            .map(|t| {
                smithay::wayland::compositor::with_states(t.wl_surface(), |states| {
                    states
                        .data_map
                        .get::<smithay::wayland::shell::xdg::XdgToplevelSurfaceData>()
                        .and_then(|data| data.lock().ok())
                        .and_then(|attrs| attrs.title.clone())
                        .unwrap_or_default()
                })
            })
            .unwrap_or_default();

        kara_sight::WorkspaceContext {
            current_ws: ws_idx,
            occupied_workspaces: occupied,
            focused_title,
            monitor_id: output_idx,
            sync_enabled: self.config.general.sync_workspaces,
            is_focused_monitor: output_idx == self.focused_output,
        }
    }

    /// Apply the tiling layout across all outputs.
    /// Each output shows its effective workspace. Windows are positioned in global coordinates.
    pub fn apply_layout(&mut self) {
        self.layout_dirty = true;
        self.bar_dirty = true;
        self.border_rects.clear();
        self.window_base_positions.clear();

        // Collect each output's effective (output_idx, ws_idx) pair plus the
        // tile parameters needed to lay it out. With per-monitor workspace
        // pools the (output_idx, ws_idx) tuple uniquely identifies which
        // workspace to render where.
        let output_ws: Vec<(usize, usize, (i32, i32), Rectangle<i32, Logical>, Point<i32, Logical>, Option<Window>)> =
            self.outputs.iter().enumerate().map(|(i, out)| {
                (
                    i,
                    self.effective_ws(i),
                    out.size,
                    out.workarea,
                    out.location,
                    out.fullscreen_window.clone(),
                )
            }).collect();

        let border_px = self.config.general.border_px;

        // Unmap windows from non-visible workspaces. With per-monitor pools,
        // visibility is per (output, ws_idx) — every workspace not currently
        // displayed on its owning output gets its windows unmapped.
        let visible: std::collections::HashSet<(usize, usize)> = output_ws
            .iter()
            .map(|(out_idx, ws_idx, ..)| (*out_idx, *ws_idx))
            .collect();
        for (out_idx, pool) in self.workspaces.iter().enumerate() {
            for (ws_idx, ws) in pool.iter().enumerate() {
                if !visible.contains(&(out_idx, ws_idx)) {
                    for w in &ws.clients {
                        self.space.unmap_elem(w);
                    }
                }
            }
        }

        // Layout each output's workspace
        for (out_idx, ws_idx, out_size, workarea, location, fs_window) in &output_ws {
            // Fullscreen on this output
            if let Some(fs_window) = fs_window {
                let fs_window = fs_window.clone();
                let ws = &self.workspaces[*out_idx][*ws_idx];
                for w in &ws.clients {
                    if *w != fs_window {
                        self.space.unmap_elem(w);
                    }
                }
                if let Some(toplevel) = fs_window.toplevel() {
                    toplevel.with_pending_state(|state| {
                        state.size = Some((out_size.0, out_size.1).into());
                        mark_tiled(state);
                    });
                    toplevel.send_configure();
                }
                self.space.map_element(fs_window, *location, false);
                continue;
            }

            let ws = &self.workspaces[*out_idx][*ws_idx];
            let geometries = layout_workspace(ws, *workarea, border_px);

            for geom in &geometries {
                if geom.visible {
                    if let Some(toplevel) = geom.window.toplevel() {
                        toplevel.with_pending_state(|state| {
                            state.size = Some((geom.rect.size.w, geom.rect.size.h).into());
                            mark_tiled(state);
                        });
                        toplevel.send_configure();
                    }
                    self.space.map_element(geom.window.clone(), geom.rect.loc, false);
                    self.window_base_positions.push((geom.window.clone(), geom.rect.loc));

                    if let Some(br) = geom.border_rect {
                        // Store border with index matching window_base_positions
                        self.border_rects.push((br, geom.is_focused));
                    }
                } else {
                    self.space.unmap_elem(&geom.window);
                }
            }
        }

        // NOTE: previously workspace windows were unmapped from Space while a
        // scratchpad overlay was visible, to prevent them drawing on top of the
        // dim. That broke Firefox/Floorp: once smithay sent wl_output.leave for
        // a surface, Firefox treats it as "invisible" and stops committing new
        // frames — so the browser froze until the scratchpad was dismissed.
        //
        // Instead, keep workspace windows mapped, and rely on the render-order
        // change in backend_udev.rs that draws the scratchpad dim AFTER the
        // space elements. The dim has a hole cut out for the scratchpad area,
        // so scratchpad windows (raised to the top of Space) remain fully
        // visible while everything else gets dimmed.
    }

    /// Apply tiling layout for a scratchpad within its floating rect.
    pub fn apply_scratchpad_layout(&mut self, sp_idx: usize) {
        let sc = match self.config.scratchpads.get(sp_idx) {
            Some(sc) => sc.clone(),
            None => return,
        };
        let sp = match self.scratchpads.get(sp_idx) {
            Some(sp) => sp,
            None => return,
        };
        if !sp.visible {
            return;
        }

        let workarea = self.outputs.get(sp.output_idx)
            .map(|o| o.workarea)
            .unwrap_or_else(|| Rectangle::new((0, 0).into(), (800, 600).into()));

        let sp_w = (workarea.size.w as f32 * sc.width_pct as f32 / 100.0) as i32;
        let sp_h = (workarea.size.h as f32 * sc.height_pct as f32 / 100.0) as i32;
        let sp_x = workarea.loc.x + (workarea.size.w - sp_w) / 2;
        let sp_y = workarea.loc.y + (workarea.size.h - sp_h) / 2;

        // Expand rect by gap so layout_workspace's outer gap inset brings it back to intended size
        let gap = self.scratchpads[sp_idx].workspace.gap_px;
        let sp_rect = Rectangle::new(
            (sp_x - gap, sp_y - gap).into(),
            (sp_w + gap * 2, sp_h + gap * 2).into(),
        );

        let border_px = self.config.general.border_px;
        let geometries = layout_workspace(&self.scratchpads[sp_idx].workspace, sp_rect, border_px);

        // Drop stale base positions for this scratchpad before rebuilding —
        // otherwise every re-layout (e.g. spawning a new window into a visible
        // scratchpad) appends fresh entries on top of old ones, and the
        // border render path's index-based correlation with window_base_positions
        // gets out of sync → borders drawn at prior-frame coordinates.
        let sp_windows: Vec<Window> =
            self.scratchpads[sp_idx].workspace.clients.iter().cloned().collect();
        self.window_base_positions.retain(|(w, _)| !sp_windows.contains(w));

        self.scratchpad_border_rects.clear();
        let mut mapped_windows = Vec::new();
        for geom in &geometries {
            if geom.visible {
                if let Some(toplevel) = geom.window.toplevel() {
                    toplevel.with_pending_state(|state| {
                        state.size = Some((geom.rect.size.w, geom.rect.size.h).into());
                        mark_tiled(state);
                    });
                    toplevel.send_configure();
                }
                self.space.map_element(geom.window.clone(), geom.rect.loc, false);
                self.window_base_positions.push((geom.window.clone(), geom.rect.loc));
                mapped_windows.push(geom.window.clone());

                if let Some(br) = geom.border_rect {
                    self.scratchpad_border_rects.push((br, geom.is_focused));
                }
            } else {
                self.space.unmap_elem(&geom.window);
            }
        }

        // Raise all scratchpad windows to top of Space stacking after layout
        for window in &mapped_windows {
            self.space.raise_element(window, true);
        }

        self.scratchpad_layout_dirty = true;
        self.bar_dirty = true;
    }

    /// Check if a window belongs to any scratchpad.
    fn is_scratchpad_window(&self, window: &Window) -> bool {
        self.scratchpads.iter().any(|sp| sp.workspace.clients.contains(window))
    }

    /// Check if any scratchpad is visible or hiding (animated).
    fn any_scratchpad_active(&self) -> bool {
        self.scratchpads.iter().any(|sp| sp.visible || sp.hiding)
    }

    /// Re-position windows and borders with current animation offsets.
    /// Uses stored base positions from apply_layout() — no compounding.
    pub fn apply_animation_offsets(&mut self) {
        let sp_active = self.any_scratchpad_active();

        // Re-map windows from their base positions + current animation offset
        for (window, base_pos) in &self.window_base_positions {
            // Skip workspace windows when a scratchpad is visible
            if sp_active && !self.is_scratchpad_window(window) {
                continue;
            }
            if let Some((dx, dy)) = self.animations.offset_for(window) {
                let offset_loc: Point<i32, Logical> =
                    (base_pos.x + dx as i32, base_pos.y + dy as i32).into();
                self.space.map_element(window.clone(), offset_loc, false);
            }
        }

        // Build border offsets — border_rects and window_base_positions are in the same order
        // (both built during apply_layout iteration), so indices correspond 1:1.
        self.border_offsets.clear();
        for (i, _) in self.border_rects.iter().enumerate() {
            let offset = self.window_base_positions.get(i)
                .and_then(|(window, _)| self.animations.offset_for(window))
                .unwrap_or((0.0, 0.0));
            self.border_offsets.push(offset);
        }

        // Build scratchpad border offsets from scratchpad windows
        self.scratchpad_border_offsets.clear();
        // Scratchpad borders correspond to visible scratchpad windows in layout order.
        // Find the scratchpad window base positions (appended after regular ones)
        let regular_count = self.border_rects.len();
        for (i, _) in self.scratchpad_border_rects.iter().enumerate() {
            let offset = self.window_base_positions.get(regular_count + i)
                .and_then(|(window, _)| self.animations.offset_for(window))
                .unwrap_or((0.0, 0.0));
            self.scratchpad_border_offsets.push(offset);
        }
    }

    /// Process deferred workspace sends after out-animations complete.
    /// Also snaps completed in-animations back to their base positions.
    pub fn process_completed_animations(&mut self) {
        let to_unmap = self.animations.tick();

        if to_unmap.is_empty() && !self.animations.has_active() && !self.pending_sends.is_empty() {
            // Safety: if no animations but pending sends remain, flush them
        }

        let mut needs_relayout = false;

        // Check if any hiding scratchpad's animations are all done
        for sp in &mut self.scratchpads {
            if sp.hiding {
                let all_done = sp.workspace.clients.iter()
                    .all(|w| !self.animations.active.iter().any(|a| &a.window == w));
                if all_done {
                    // Batch unmap all scratchpad windows + clear borders/dim together
                    for w in &sp.workspace.clients {
                        self.space.unmap_elem(w);
                    }
                    sp.visible = false;
                    sp.hiding = false;
                    self.scratchpad_border_rects.clear();
                    self.scratchpad_border_cache.clear();
                    self.scratchpad_border_offsets.clear();
                    needs_relayout = true;
                }
            }
        }

        for window in &to_unmap {
            self.space.unmap_elem(window);

            // Check if this window has a pending send
            if let Some(pos) = self.pending_sends.iter().position(|(w, _, _)| w == window) {
                let (window, target_out, target_ws) = self.pending_sends.remove(pos);
                // Remove from any pool that currently owns this window —
                // search across every output's per-monitor pool.
                for pool in self.workspaces.iter_mut() {
                    for ws in pool.iter_mut() {
                        ws.remove_client(&window);
                    }
                }
                if let Some(pool) = self.workspaces.get_mut(target_out) {
                    if target_ws < pool.len() {
                        pool[target_ws].add_client(window);
                    }
                }
                needs_relayout = true;
            }
        }

        // Snap all non-animated windows back to their base positions
        // Skip workspace windows when a scratchpad is active
        let sp_active = self.any_scratchpad_active();
        for (window, base_pos) in &self.window_base_positions {
            if sp_active && !self.is_scratchpad_window(window) {
                continue;
            }
            if self.animations.offset_for(window).is_none() {
                self.space.map_element(window.clone(), *base_pos, false);
            }
        }

        if needs_relayout {
            self.apply_layout();
            self.apply_focus();
        }
    }

    /// Returns Some(sp_idx) iff there is a visible scratchpad anchored to the
    /// currently focused output. Most keybinds (`do_focus_next`,
    /// `do_kill_focused`, `do_toggle_float`, etc.) should only treat a
    /// scratchpad as "active" when the user is actually looking at it — so
    /// they all gate on this method instead of the `focused_scratchpad`
    /// field, which stays set as a "last toggled" anchor across monitor
    /// changes and isn't always the right thing to look at.
    pub fn active_scratchpad_for_focus(&self) -> Option<usize> {
        self.scratchpads
            .iter()
            .position(|sp| sp.visible && sp.output_idx == self.focused_output)
    }

    /// Determine which single window across the whole desktop should hold
    /// keyboard focus right now. If a scratchpad is visible on the focused
    /// output, its focused client wins; otherwise the focused workspace's
    /// focused client wins. Scratchpads on *other* monitors do not steal
    /// focus from the focused output — the user can mod+focus_monitor away
    /// from a scratchpad without losing input on the new monitor.
    fn compute_focused_window(&self) -> Option<Window> {
        if let Some(sp_idx) = self.active_scratchpad_for_focus() {
            return self.scratchpads[sp_idx].workspace.focused().cloned();
        }
        let ws_idx = self.effective_ws(self.focused_output);
        self.workspaces
            .get(self.focused_output)
            .and_then(|pool| pool.get(ws_idx))
            .and_then(|ws| ws.focused().cloned())
    }

    /// Set keyboard focus to the single window across the whole desktop that
    /// should currently be active. Deactivates every other client in every
    /// workspace and every scratchpad so the focus border (rendered when
    /// `set_activated(true)`) lives on exactly one window — even when a
    /// scratchpad is visible on a different monitor than the focused one.
    pub fn apply_focus(&mut self) {
        let serial = SERIAL_COUNTER.next_serial();

        let focused_window = self.compute_focused_window();

        // Globally deactivate every client across every per-monitor pool
        // and every scratchpad. set_activated() dedupes when the state
        // already matches, so this is cheap on subsequent calls.
        for pool in &self.workspaces {
            for ws in pool {
                for w in &ws.clients {
                    w.set_activated(false);
                }
            }
        }
        for sp in &self.scratchpads {
            for w in &sp.workspace.clients {
                w.set_activated(false);
            }
        }

        if let Some(window) = focused_window {
            window.set_activated(true);
            if let Some(toplevel) = window.toplevel() {
                let wl_surface = toplevel.wl_surface().clone();
                let keyboard = self.seat.get_keyboard().unwrap();
                keyboard.set_focus(self, Some(wl_surface), serial);
            }
            self.space.raise_element(&window, true);
        } else {
            let keyboard = self.seat.get_keyboard().unwrap();
            keyboard.set_focus(self, None::<WlSurface>, serial);
        }
    }

    /// Route a newly-mapped toplevel (first buffer commit) to a scratchpad or workspace,
    /// apply rules, re-layout, focus, and animate in.
    ///
    /// Called from the commit handler the first time an unmapped toplevel receives a
    /// buffer. Helper clients that never commit a buffer (e.g. wl-copy) skip this path
    /// entirely and are never tiled.
    fn map_new_toplevel(&mut self, window: Window) {
        let surface = match window.toplevel() {
            Some(t) => t.wl_surface().clone(),
            None => return,
        };

        // Resolve app_id now — by first commit, clients have set it.
        let app_id = compositor::with_states(&surface, |states| {
            states
                .data_map
                .get::<XdgToplevelSurfaceData>()
                .and_then(|d| d.lock().ok())
                .and_then(|attrs| attrs.app_id.clone())
        })
        .unwrap_or_default();

        tracing::debug!("map new toplevel: app_id={:?}", app_id);

        // Known transient helper clients (e.g. wl-clipboard) create an xdg_toplevel
        // purely to satisfy protocol requirements: they need a surface that receives
        // keyboard focus so wl_data_device.set_selection has a valid input serial,
        // and they destroy themselves milliseconds later. Tiling them causes real
        // windows to flicker-resize during yank/paste, but filtering them entirely
        // breaks clipboard writes. Compromise: give the helper a tiny configure and
        // transient keyboard focus without adding it to any workspace.
        if is_helper_client(&app_id) {
            tracing::debug!("routing helper client to hidden focus: {app_id}");
            if let Some(toplevel) = window.toplevel() {
                toplevel.with_pending_state(|state| {
                    state.size = Some((1, 1).into());
                });
                toplevel.send_configure();

                let serial = SERIAL_COUNTER.next_serial();
                let focus_surface = toplevel.wl_surface().clone();
                let keyboard = self.seat.get_keyboard().unwrap();
                keyboard.set_focus(self, Some(focus_surface), serial);
            }
            self.hidden_helpers.push(window);
            return;
        }

        // Scratchpad capture (app_id match)
        if !app_id.is_empty() {
            if let Some(sp_idx) = self.check_scratchpad_capture(&app_id) {
                tracing::debug!("captured scratchpad '{}' window: {app_id}",
                    self.config.scratchpads.get(sp_idx).map(|s| s.name.as_str()).unwrap_or("?"));
                self.scratchpads[sp_idx].workspace.add_client(window);
                if self.scratchpads[sp_idx].visible {
                    self.apply_scratchpad_layout(sp_idx);
                }
                return;
            }
        }

        // Autostart capture: next window goes to the waiting scratchpad
        if let Some(sp_idx) = self.scratchpads.iter().position(|sp| sp.pending_capture) {
            self.scratchpads[sp_idx].pending_capture = false;
            tracing::debug!("autostart capture for scratchpad '{}'",
                self.config.scratchpads.get(sp_idx).map(|s| s.name.as_str()).unwrap_or("?"));
            self.scratchpads[sp_idx].workspace.add_client(window);
            if self.scratchpads[sp_idx].visible {
                self.apply_scratchpad_layout(sp_idx);
                self.apply_focus();
            }
            return;
        }

        // Focused scratchpad absorbs new windows ONLY if the user is
        // currently looking at it. With multi-monitor + sandboxed scratchpads,
        // a scratchpad on M1 must not capture a window the user spawned
        // while focused on M2 — fall through to regular workspace routing
        // on the focused output instead.
        if let Some(sp_idx) = self.active_scratchpad_for_focus() {
            tracing::debug!("new window routed to focused scratchpad");
            self.scratchpads[sp_idx].workspace.add_client(window);
            self.apply_scratchpad_layout(sp_idx);
            self.apply_focus();
            return;
        }

        // Check pending autostart routes FIRST. If this app_id was queued
        // for placement at a specific (output, workspace) by a recent
        // autostart entry, consume the route and place the window there
        // regardless of where the user's focus currently is.
        if !app_id.is_empty() {
            if let Some(pos) = self
                .pending_autostart_routes
                .iter()
                .position(|(a, _, _)| a == &app_id)
            {
                let (_, target_out, target_ws) = self.pending_autostart_routes.remove(pos);
                if target_out < self.workspaces.len() && target_ws < self.workspaces[target_out].len() {
                    self.workspaces[target_out][target_ws]
                        .add_client_floating(window.clone(), false);
                    tracing::info!(
                        "autostart route: {app_id} → output {target_out} ws {}",
                        target_ws + 1,
                    );
                    self.apply_layout();
                    self.apply_focus();
                    return;
                }
            }
        }

        // Regular workspace routing via rules
        let (should_float, target_ws) = if app_id.is_empty() {
            (false, None)
        } else {
            self.check_rules(&app_id)
        };

        let ws_idx = target_ws.unwrap_or_else(|| self.effective_ws(self.focused_output));

        let target_output_name = self
            .outputs
            .get(self.focused_output)
            .map(|o| o.output.name())
            .unwrap_or_default();
        tracing::info!(
            "map {app_id:?} → workspace {} (output {} = {target_output_name}, rule_pinned={})",
            ws_idx + 1,
            self.focused_output,
            target_ws.is_some(),
        );

        self.workspaces[self.focused_output][ws_idx].add_client_floating(window.clone(), should_float);

        self.apply_layout();
        self.apply_focus();

        // Animate window in
        let preset = self.config.animations.preset;
        if preset != kara_config::AnimationPreset::Instant {
            if let Some(loc) = self.space.element_location(&window) {
                let geom = window.geometry();
                let wa = self.workarea();
                self.animations.animate_in(
                    window, preset, self.config.animations.duration_ms,
                    loc.x, loc.y, geom.size.w, geom.size.h,
                    wa.loc.x, wa.loc.y, wa.size.w, wa.size.h,
                    crate::animation::SlideDirection::Auto,
                );
            }
        }
    }

    /// Focus a specific window (e.g., on click)
    pub fn focus_window(&mut self, target: &Window) {
        let out = self.focused_output;
        let ws_idx = self.effective_ws(out);
        let ws = &mut self.workspaces[out][ws_idx];
        let mut changed = false;
        if let Some(idx) = ws.clients.iter().position(|w| w == target) {
            if ws.focused_idx != Some(idx) {
                ws.last_focused_idx = ws.focused_idx;
                ws.focused_idx = Some(idx);
                changed = true;
            }
        }
        // Skip the re-activate pass when nothing changed — avoids redundant
        // set_activated churn on same-window clicks.
        if changed {
            self.apply_focus();
        }
    }
}

// --- Smithay handler implementations ---

impl CompositorHandler for Gate {
    fn compositor_state(&mut self) -> &mut CompositorState {
        &mut self.compositor_state
    }

    fn client_compositor_state<'a>(&self, client: &'a Client) -> &'a CompositorClientState {
        &client.get_data::<ClientState>().unwrap().compositor
    }

    fn commit(&mut self, surface: &WlSurface) {
        smithay::backend::renderer::utils::on_commit_buffer_handler::<Self>(surface);

        // Handle layer surface commits
        let mut layer_needs_focus = false;
        for out in &self.outputs {
            let mut map = layer_map_for_output(&out.output);
            for layer in map.layers().cloned().collect::<Vec<_>>() {
                if layer.wl_surface() == surface {
                    let initial = compositor::with_states(surface, |states| {
                        states
                            .data_map
                            .get::<smithay::wayland::shell::wlr_layer::LayerSurfaceData>()
                            .map(|d| !d.lock().unwrap().initial_configure_sent)
                            .unwrap_or(false)
                    });
                    if initial {
                        map.arrange();
                        layer.layer_surface().send_configure();
                    } else {
                        map.arrange();
                        let wants_kb = compositor::with_states(surface, |states| {
                            states.cached_state
                                .get::<smithay::wayland::shell::wlr_layer::LayerSurfaceCachedState>()
                                .current()
                                .keyboard_interactivity
                        });
                        if wants_kb == smithay::wayland::shell::wlr_layer::KeyboardInteractivity::Exclusive {
                            layer_needs_focus = true;
                        }
                    }
                    drop(map);
                    break;
                }
            }
            if layer_needs_focus { break; }
        }
        if layer_needs_focus {
            let serial = SERIAL_COUNTER.next_serial();
            let keyboard = self.seat.get_keyboard().unwrap();
            keyboard.set_focus(self, Some(surface.clone()), serial);
            return;
        }

        // If this commit is for a known window, refresh it.
        // Check all per-monitor workspace pools + scratchpads, not just
        // space.elements(), because windows may be unmapped from space
        // (e.g. scratchpad overlay active or a non-displayed workspace).
        let window = self.space.elements().find(|w| {
            w.toplevel().map_or(false, |t| t.wl_surface() == surface)
        }).cloned().or_else(|| {
            self.workspaces
                .iter()
                .flat_map(|pool| pool.iter())
                .flat_map(|ws| ws.clients.iter())
                .chain(self.scratchpads.iter().flat_map(|sp| sp.workspace.clients.iter()))
                .find(|w| w.toplevel().map_or(false, |t| t.wl_surface() == surface))
                .cloned()
        });
        if let Some(window) = window {
            window.on_commit();
        }

        // If this commit is for a still-unmapped toplevel, check whether it now has
        // a buffer attached. If so, it's transitioning from "created" to "mapped" —
        // route it to a workspace and lay out. Clients that never commit a buffer
        // (wl-copy, etc.) are never added to the layout.
        let pending_idx = self.unmapped_windows.iter().position(|w| {
            w.toplevel().map_or(false, |t| t.wl_surface() == surface)
        });
        if let Some(idx) = pending_idx {
            if toplevel_has_buffer(surface) {
                let window = self.unmapped_windows.remove(idx);
                self.map_new_toplevel(window);
            }
        }
    }
}

/// Check whether a toplevel's surface has a committed buffer — i.e. is mapped.
fn toplevel_has_buffer(surface: &WlSurface) -> bool {
    compositor::with_states(surface, |states| {
        states
            .data_map
            .get::<smithay::backend::renderer::utils::RendererSurfaceStateUserData>()
            .map(|s| s.lock().unwrap().buffer().is_some())
            .unwrap_or(false)
    })
}

impl BufferHandler for Gate {
    fn buffer_destroyed(&mut self, _buffer: &wl_buffer::WlBuffer) {}
}

impl ShmHandler for Gate {
    fn shm_state(&self) -> &ShmState {
        &self.shm_state
    }
}

impl XdgShellHandler for Gate {
    fn xdg_shell_state(&mut self) -> &mut XdgShellState {
        &mut self.xdg_shell_state
    }

    fn new_toplevel(&mut self, surface: ToplevelSurface) {
        // Send only the tiled state flags in the initial configure, with NO
        // size. If we send a size here, Firefox/Floorp cache it as the target
        // dimensions for their first buffer and then silently ignore the real
        // size from the post-commit apply_layout pass — the window renders at
        // the wrong size until some unrelated focus/resize forces a re-ack.
        // Leaving size=None tells the client "pick a default", then we resize
        // it authoritatively when it actually joins a workspace.
        surface.with_pending_state(|state| {
            mark_tiled(state);
        });
        surface.send_configure();

        let window = Window::new_wayland_window(surface);
        self.unmapped_windows.push(window);
    }

    fn toplevel_destroyed(&mut self, surface: ToplevelSurface) {
        // Drop unmapped helper windows (e.g. wl-copy) silently — they were never
        // added to the layout.
        if let Some(idx) = self.unmapped_windows.iter().position(|w| {
            w.toplevel().map_or(false, |t| t == &surface)
        }) {
            self.unmapped_windows.remove(idx);
            return;
        }

        // Drop hidden helper windows (e.g. wl-clipboard) and restore keyboard
        // focus to the active workspace window.
        if let Some(idx) = self.hidden_helpers.iter().position(|w| {
            w.toplevel().map_or(false, |t| t == &surface)
        }) {
            self.hidden_helpers.remove(idx);
            self.apply_focus();
            return;
        }

        // Find and remove the window from whichever workspace it's on
        let target = self.space.elements().find(|w| {
            w.toplevel().map_or(false, |t| t == &surface)
        }).cloned();

        if let Some(window) = target {
            self.animations.cancel(&window);
            self.space.unmap_elem(&window);

            // Check scratchpad windows
            let mut sp_removed = None;
            for (i, sp) in self.scratchpads.iter_mut().enumerate() {
                if sp.workspace.remove_client(&window) {
                    sp_removed = Some(i);
                    break;
                }
            }

            // Check fullscreen on all outputs
            for out in &mut self.outputs {
                if out.fullscreen_window.as_ref() == Some(&window) {
                    out.fullscreen_window = None;
                }
            }

            // Search every per-monitor pool for the destroyed window. Stop
            // at the first hit — a window only lives in one pool/workspace
            // at a time.
            'find_owner: for pool in self.workspaces.iter_mut() {
                for ws in pool.iter_mut() {
                    if ws.remove_client(&window) {
                        break 'find_owner;
                    }
                }
            }

            self.apply_layout();
            if let Some(sp_idx) = sp_removed {
                // If this was the last window in the scratchpad, auto-hide it
                // and reset `started` so the next toggle re-runs the autostart
                // command (backlog #22: scratchpad lifecycle).
                let is_empty = self.scratchpads[sp_idx].workspace.clients.is_empty();
                if is_empty {
                    self.scratchpads[sp_idx].visible = false;
                    self.scratchpads[sp_idx].hiding = false;
                    self.scratchpads[sp_idx].started = false;
                    if self.focused_scratchpad == Some(sp_idx) {
                        self.focused_scratchpad = None;
                    }
                    self.scratchpad_border_rects.clear();
                    self.scratchpad_border_cache.clear();
                    self.scratchpad_border_offsets.clear();
                    self.apply_layout();
                } else if self.scratchpads[sp_idx].visible {
                    self.apply_scratchpad_layout(sp_idx);
                } else {
                    self.scratchpad_border_rects.clear();
                    self.scratchpad_border_cache.clear();
                }
            }
            self.apply_focus();
        }
    }

    fn new_popup(&mut self, _surface: PopupSurface, _positioner: PositionerState) {}

    fn grab(
        &mut self,
        _surface: PopupSurface,
        _seat: smithay::reexports::wayland_server::protocol::wl_seat::WlSeat,
        _serial: Serial,
    ) {
    }

    fn reposition_request(
        &mut self,
        _surface: PopupSurface,
        _positioner: PositionerState,
        _token: u32,
    ) {
    }
}

impl SeatHandler for Gate {
    type KeyboardFocus = WlSurface;
    type PointerFocus = WlSurface;
    type TouchFocus = WlSurface;

    fn seat_state(&mut self) -> &mut SeatState<Self> {
        &mut self.seat_state
    }

    fn focus_changed(&mut self, seat: &Seat<Self>, focused: Option<&Self::KeyboardFocus>) {
        let dh = self.display_handle.clone();
        let client = focused.and_then(|s| dh.get_client(s.id()).ok());
        set_data_device_focus(&dh, seat, client.clone());
        set_primary_focus(&dh, seat, client);
    }
    fn cursor_image(&mut self, _seat: &Seat<Self>, image: CursorImageStatus) {
        self.cursor_status = image;
    }
}

impl SelectionHandler for Gate {
    type SelectionUserData = ();
}

impl DataDeviceHandler for Gate {
    fn data_device_state(&self) -> &DataDeviceState {
        &self.data_device_state
    }
}

impl PrimarySelectionHandler for Gate {
    fn primary_selection_state(&self) -> &PrimarySelectionState {
        &self.primary_selection_state
    }
}

impl XdgActivationHandler for Gate {
    fn activation_state(&mut self) -> &mut XdgActivationState {
        &mut self.xdg_activation_state
    }

    fn request_activation(
        &mut self,
        _token: XdgActivationToken,
        _token_data: XdgActivationTokenData,
        surface: WlSurface,
    ) {
        // Find the target window by surface match and raise it. Search
        // every per-monitor pool. The target tuple is (output, ws_idx, client_idx).
        // Honoring every activation request is safe because all kara clients
        // need a valid input serial to get a token in the first place.
        let mut target: Option<(usize, usize, usize)> = None;
        'find: for (out_idx, pool) in self.workspaces.iter().enumerate() {
            for (ws_idx, ws) in pool.iter().enumerate() {
                if let Some(client_idx) = ws.clients.iter().position(|w| {
                    w.toplevel().map_or(false, |t| t.wl_surface() == &surface)
                }) {
                    target = Some((out_idx, ws_idx, client_idx));
                    break 'find;
                }
            }
        }

        if let Some((out_idx, ws_idx, client_idx)) = target {
            self.workspaces[out_idx][ws_idx].focused_idx = Some(client_idx);
            if out_idx != self.focused_output || ws_idx != self.effective_ws(self.focused_output) {
                self.focused_output = out_idx;
                if let Some(out) = self.outputs.get_mut(out_idx) {
                    out.current_ws = ws_idx;
                }
                self.apply_layout();
            }
            self.apply_focus();
        }
    }
}

impl WlrLayerShellHandler for Gate {
    fn shell_state(&mut self) -> &mut WlrLayerShellState {
        &mut self.layer_shell_state
    }

    fn new_layer_surface(
        &mut self,
        surface: LayerSurface,
        _output: Option<smithay::reexports::wayland_server::protocol::wl_output::WlOutput>,
        _layer: Layer,
        namespace: String,
    ) {
        tracing::info!("new layer surface: namespace={namespace}, layer={_layer:?}");

        // Wrap in desktop LayerSurface and map to focused output's LayerMap
        let desktop_surface = smithay::desktop::LayerSurface::new(surface, namespace);

        let output = self.outputs.get(self.focused_output)
            .map(|o| o.output.clone());

        if let Some(ref output) = output {
            let mut map = layer_map_for_output(output);
            map.map_layer(&desktop_surface).ok();
        }
        // Initial configure is sent on the client's first commit (see commit handler)
    }

    fn layer_destroyed(&mut self, surface: LayerSurface) {
        tracing::info!("layer surface destroyed");

        // Find and unmap the desktop LayerSurface from all outputs
        for out in &self.outputs {
            let mut map = layer_map_for_output(&out.output);
            // Find by matching wl_surface
            let to_remove: Vec<_> = map.layers()
                .filter(|l| l.layer_surface().wl_surface() == surface.wl_surface())
                .cloned()
                .collect();
            for l in &to_remove {
                map.unmap_layer(l);
            }
        }

        self.apply_focus();
    }
}

impl XdgDecorationHandler for Gate {
    fn new_decoration(&mut self, toplevel: ToplevelSurface) {
        // Always request server-side decorations — kara-gate draws its own
        // (themed borders/decorations from kara-beautify specs)
        use smithay::reexports::wayland_protocols::xdg::decoration::zv1::server::zxdg_toplevel_decoration_v1::Mode;
        toplevel.with_pending_state(|state| {
            state.decoration_mode = Some(Mode::ServerSide);
        });
        toplevel.send_configure();
    }

    fn request_mode(&mut self, toplevel: ToplevelSurface, _mode: smithay::reexports::wayland_protocols::xdg::decoration::zv1::server::zxdg_toplevel_decoration_v1::Mode) {
        use smithay::reexports::wayland_protocols::xdg::decoration::zv1::server::zxdg_toplevel_decoration_v1::Mode;
        toplevel.with_pending_state(|state| {
            state.decoration_mode = Some(Mode::ServerSide);
        });
        toplevel.send_configure();
    }

    fn unset_mode(&mut self, toplevel: ToplevelSurface) {
        use smithay::reexports::wayland_protocols::xdg::decoration::zv1::server::zxdg_toplevel_decoration_v1::Mode;
        toplevel.with_pending_state(|state| {
            state.decoration_mode = Some(Mode::ServerSide);
        });
        toplevel.send_configure();
    }
}

impl ClientDndGrabHandler for Gate {}
impl ServerDndGrabHandler for Gate {}
impl smithay::wayland::output::OutputHandler for Gate {}

delegate_compositor!(Gate);
delegate_xdg_shell!(Gate);
delegate_xdg_decoration!(Gate);
delegate_layer_shell!(Gate);
delegate_shm!(Gate);
delegate_seat!(Gate);
delegate_data_device!(Gate);
delegate_primary_selection!(Gate);
delegate_xdg_activation!(Gate);
delegate_xdg_toplevel_icon!(Gate);

impl XdgToplevelIconHandler for Gate {}
delegate_output!(Gate);
