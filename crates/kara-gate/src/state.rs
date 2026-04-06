use smithay::delegate_compositor;
use smithay::delegate_data_device;
use smithay::delegate_layer_shell;
use smithay::delegate_output;
use smithay::delegate_seat;
use smithay::delegate_shm;
use smithay::delegate_xdg_decoration;
use smithay::delegate_xdg_shell;
use smithay::desktop::{Space, Window};
use smithay::input::{Seat, SeatHandler, SeatState};
use smithay::input::pointer::CursorImageStatus;
use smithay::reexports::calloop::LoopSignal;
use smithay::reexports::wayland_server::{Client, Display, DisplayHandle};
use smithay::reexports::wayland_server::backend::{ClientData, ClientId, DisconnectReason};
use smithay::reexports::wayland_server::protocol::wl_buffer;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::output::Output;
use smithay::utils::{Clock, Logical, Monotonic, Point, Rectangle, Serial, SERIAL_COUNTER};
use smithay::wayland::buffer::BufferHandler;
use smithay::wayland::compositor::{self, CompositorClientState, CompositorHandler, CompositorState};
use smithay::wayland::selection::SelectionHandler;
use smithay::wayland::selection::data_device::{
    ClientDndGrabHandler, DataDeviceHandler, DataDeviceState, ServerDndGrabHandler,
};
use smithay::wayland::shell::xdg::{
    PopupSurface, PositionerState, ToplevelSurface, XdgShellHandler, XdgShellState,
    XdgToplevelSurfaceData,
};
use smithay::wayland::shell::wlr_layer::{
    Layer, LayerSurface, WlrLayerShellHandler, WlrLayerShellState,
};
use smithay::wayland::shell::xdg::decoration::{XdgDecorationHandler, XdgDecorationState};
use smithay::wayland::shm::{ShmHandler, ShmState};

use crate::input::Keybind;
use crate::layout::layout_workspace;
use crate::workspace::{Workspace, WORKSPACE_COUNT};

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
    pub seat: Seat<Self>,

    // Layer surfaces (kara-summon, kara-whisper, etc.)
    pub layer_surfaces: Vec<LayerSurface>,

    // Desktop
    pub space: Space<Window>,
    pub clock: Clock<Monotonic>,

    // Config
    pub config: kara_config::Config,

    // Bar
    pub bar_renderer: kara_sight::BarRenderer,
    pub status_cache: kara_sight::StatusCache,

    // Window management
    pub workspaces: Vec<Workspace>,
    pub current_ws: usize,
    pub previous_ws: usize,
    pub keybinds: std::sync::Arc<Vec<Keybind>>,

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
    pub pending_sends: Vec<(Window, usize)>,
    // Base positions from apply_layout(), keyed by window identity for offset calculation
    pub window_base_positions: Vec<(Window, smithay::utils::Point<i32, Logical>)>,

    // Pointer location (tracked explicitly for relative motion from libinput)
    pub pointer_location: smithay::utils::Point<f64, Logical>,

    // Bar rendering cache
    pub bar_dirty: bool,
    pub bar_cache: Option<(Vec<u8>, u32, u32)>, // (rgba_bytes, width, height)

    // Cursor rendering
    pub cursor_status: CursorImageStatus,
    pub cursor_cache: Option<crate::cursor::CursorCache>,
    pub cursor_last_moved: std::time::Instant,
    pub cursor_idle_pos: smithay::utils::Point<f64, Logical>,

    // Config auto-reload: cached mtime to detect file changes
    pub config_mtime: Option<std::time::SystemTime>,

    // Backend-specific data (UdevData for udev, None for winit)
    #[allow(dead_code)]
    pub backend_data: Option<Box<dyn std::any::Any>>,
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

        let workspaces: Vec<Workspace> = (0..WORKSPACE_COUNT)
            .map(|id| {
                let mut ws = Workspace::new(id);
                ws.gap_px = config.general.gap_px;
                ws.mfact = config.general.default_mfact;
                ws
            })
            .collect();

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
            bar_cache: None,
            pointer_location: (0.0, 0.0).into(),
            cursor_status: CursorImageStatus::default_named(),
            cursor_cache: None,
            cursor_last_moved: std::time::Instant::now(),
            cursor_idle_pos: (0.0, 0.0).into(),
            config_mtime: Self::get_config_mtime(),
            backend_data: None,
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

        // Apply general config to workspaces
        for ws in &mut self.workspaces {
            ws.gap_px = self.config.general.gap_px;
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
    pub fn bar_workspace_context(&self, output_idx: usize) -> kara_sight::WorkspaceContext {
        let ws_idx = self.effective_ws(output_idx);

        let mut occupied = [false; 9];
        for (i, ws) in self.workspaces.iter().enumerate() {
            if i < 9 {
                occupied[i] = !ws.clients.is_empty();
            }
        }

        let focused_title = self.workspaces[ws_idx]
            .focused()
            .and_then(|w| w.toplevel())
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
        }
    }

    /// Apply the tiling layout across all outputs.
    /// Each output shows its effective workspace. Windows are positioned in global coordinates.
    pub fn apply_layout(&mut self) {
        self.layout_dirty = true;
        self.bar_dirty = true;
        self.border_rects.clear();
        self.window_base_positions.clear();

        // Collect which workspaces are visible on which outputs
        let output_ws: Vec<(usize, (i32, i32), Rectangle<i32, Logical>, Point<i32, Logical>, Option<Window>)> =
            self.outputs.iter().enumerate().map(|(i, out)| {
                (
                    self.effective_ws(i),
                    out.size,
                    out.workarea,
                    out.location,
                    out.fullscreen_window.clone(),
                )
            }).collect();

        let border_px = self.config.general.border_px;

        // Track which workspaces are visible (to unmap windows on non-visible workspaces)
        let mut visible_ws: Vec<usize> = output_ws.iter().map(|(ws, ..)| *ws).collect();
        visible_ws.sort();
        visible_ws.dedup();

        // Unmap windows from non-visible workspaces
        for (i, ws) in self.workspaces.iter().enumerate() {
            if !visible_ws.contains(&i) {
                for w in &ws.clients {
                    self.space.unmap_elem(w);
                }
            }
        }

        // Layout each output's workspace
        for (ws_idx, out_size, workarea, location, fs_window) in &output_ws {
            // Fullscreen on this output
            if let Some(fs_window) = fs_window {
                let fs_window = fs_window.clone();
                let ws = &self.workspaces[*ws_idx];
                for w in &ws.clients {
                    if *w != fs_window {
                        self.space.unmap_elem(w);
                    }
                }
                if let Some(toplevel) = fs_window.toplevel() {
                    toplevel.with_pending_state(|state| {
                        state.size = Some((out_size.0, out_size.1).into());
                    });
                    toplevel.send_configure();
                }
                self.space.map_element(fs_window, *location, false);
                continue;
            }

            let ws = &self.workspaces[*ws_idx];
            let geometries = layout_workspace(ws, *workarea, border_px);

            for geom in &geometries {
                if geom.visible {
                    if let Some(toplevel) = geom.window.toplevel() {
                        toplevel.with_pending_state(|state| {
                            state.size = Some((geom.rect.size.w, geom.rect.size.h).into());
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

        self.scratchpad_border_rects.clear();
        let mut mapped_windows = Vec::new();
        for geom in &geometries {
            if geom.visible {
                if let Some(toplevel) = geom.window.toplevel() {
                    toplevel.with_pending_state(|state| {
                        state.size = Some((geom.rect.size.w, geom.rect.size.h).into());
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
            if let Some(pos) = self.pending_sends.iter().position(|(w, _)| w == window) {
                let (window, target_ws) = self.pending_sends.remove(pos);
                for ws in &mut self.workspaces {
                    ws.remove_client(&window);
                }
                if target_ws < self.workspaces.len() {
                    self.workspaces[target_ws].add_client(window);
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

    /// Set keyboard focus to the currently focused window on the focused output's workspace.
    pub fn apply_focus(&mut self) {
        let serial = SERIAL_COUNTER.next_serial();

        // If a scratchpad is focused, use its workspace for focus
        let focused_window = if let Some(sp_idx) = self.focused_scratchpad {
            self.scratchpads[sp_idx].workspace.focused().cloned()
        } else {
            let ws_idx = self.effective_ws(self.focused_output);
            self.workspaces[ws_idx].focused().cloned()
        };

        if let Some(window) = focused_window {
            // Deactivate all windows in the active workspace
            if let Some(sp_idx) = self.focused_scratchpad {
                for w in &self.scratchpads[sp_idx].workspace.clients {
                    w.set_activated(false);
                }
            }
            let ws_idx = self.effective_ws(self.focused_output);
            for w in &self.workspaces[ws_idx].clients {
                w.set_activated(false);
            }
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

    /// Focus a specific window (e.g., on click)
    pub fn focus_window(&mut self, target: &Window) {
        let ws_idx = self.effective_ws(self.focused_output);
        let ws = &mut self.workspaces[ws_idx];
        if let Some(idx) = ws.clients.iter().position(|w| w == target) {
            if ws.focused_idx != Some(idx) {
                ws.last_focused_idx = ws.focused_idx;
                ws.focused_idx = Some(idx);
            }
        }
        self.apply_focus();
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

        // Handle layer surface initial configure
        for layer in &self.layer_surfaces {
            if layer.wl_surface() == surface {
                layer.ensure_configured();
                return;
            }
        }

        // If this commit is for a mapped window, refresh the space
        if let Some(window) = self.space.elements().find(|w| {
            w.toplevel().map_or(false, |t| t.wl_surface() == surface)
        }).cloned() {
            window.on_commit();
        }
    }
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
        let window = Window::new_wayland_window(surface.clone());

        // Get app_id for rule matching
        let app_id = compositor::with_states(surface.wl_surface(), |states| {
            states
                .data_map
                .get::<XdgToplevelSurfaceData>()
                .and_then(|d| d.lock().ok())
                .and_then(|attrs| attrs.app_id.clone())
        })
        .unwrap_or_default();

        tracing::debug!("new toplevel: app_id={:?}", app_id);

        // Check if this window should be captured by a scratchpad (app_id match)
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

        // Check if any scratchpad is waiting for its autostart window
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

        // If a scratchpad is focused, new windows go into it
        if let Some(sp_idx) = self.focused_scratchpad {
            if self.scratchpads[sp_idx].visible {
                tracing::debug!("new window routed to focused scratchpad");
                self.scratchpads[sp_idx].workspace.add_client(window);
                self.apply_scratchpad_layout(sp_idx);
                self.apply_focus();
                return;
            }
        }

        // Check window rules
        let (should_float, target_ws) = if app_id.is_empty() {
            (false, None)
        } else {
            self.check_rules(&app_id)
        };

        let ws_idx = target_ws.unwrap_or_else(|| self.effective_ws(self.focused_output));

        // Add to workspace with floating state
        self.workspaces[ws_idx].add_client_floating(window.clone(), should_float);

        // Re-layout and focus
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

    fn toplevel_destroyed(&mut self, surface: ToplevelSurface) {
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

            for ws in &mut self.workspaces {
                if ws.remove_client(&window) {
                    break;
                }
            }

            self.apply_layout();
            if let Some(sp_idx) = sp_removed {
                if self.scratchpads[sp_idx].visible {
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

    fn focus_changed(&mut self, _seat: &Seat<Self>, _focused: Option<&Self::KeyboardFocus>) {}
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

impl WlrLayerShellHandler for Gate {
    fn shell_state(&mut self) -> &mut WlrLayerShellState {
        &mut self.layer_shell_state
    }

    fn new_layer_surface(
        &mut self,
        surface: LayerSurface,
        _output: Option<smithay::reexports::wayland_server::protocol::wl_output::WlOutput>,
        layer: Layer,
        namespace: String,
    ) {
        tracing::info!("new layer surface: namespace={namespace}, layer={layer:?}");

        // Configure with the full output width, let the client choose height
        let (w, _h) = self.output_size();
        surface.with_pending_state(|state| {
            state.size = Some((w, 0).into());
        });
        surface.send_configure();

        self.layer_surfaces.push(surface);
    }

    fn layer_destroyed(&mut self, surface: LayerSurface) {
        tracing::info!("layer surface destroyed");
        self.layer_surfaces.retain(|s| s != &surface);
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
delegate_output!(Gate);
