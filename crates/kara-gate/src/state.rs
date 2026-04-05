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
    pub keybinds: Vec<Keybind>,

    // Multi-monitor: per-output state
    pub outputs: Vec<OutputState>,
    pub focused_output: usize,

    // Wallpaper
    pub wallpaper: Option<crate::wallpaper::Wallpaper>,

    // IPC
    pub ipc_listener: Option<std::os::unix::net::UnixListener>,

    // Floating (no extra state beyond workspace.floating)

    // Scratchpad
    pub scratchpad_visible: bool,
    pub scratchpad_windows: Vec<Window>,
    pub scratchpad_started: bool,

    // Autostart
    pub autostart_done: bool,

    // Border rendering: cached geometry from last apply_layout
    pub border_rects: Vec<(smithay::utils::Rectangle<i32, Logical>, bool)>, // (rect, is_focused)

    // Pointer location (tracked explicitly for relative motion from libinput)
    pub pointer_location: smithay::utils::Point<f64, Logical>,

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
        seat.add_keyboard(Default::default(), 200, 25).unwrap();
        seat.add_pointer();

        let config = kara_config::load_default_config();
        let keybinds = crate::input::keybinds_from_config(&config);

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

        let gate = Self {
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
            focused_output: 0,
            ipc_listener: kara_ipc::server::bind_socket().ok(),
            scratchpad_visible: false,
            scratchpad_windows: Vec::new(),
            scratchpad_started: false,
            autostart_done: false,
            border_rects: Vec::new(),
            pointer_location: (0.0, 0.0).into(),
            backend_data: None,
        };

        // Apply environment variables and cursor theme
        gate.apply_environment();
        gate.apply_cursor_theme();

        gate
    }

    /// Reload config from disk and apply changes.
    pub fn reload_config(&mut self) {
        tracing::info!("reloading config");
        self.config = kara_config::load_default_config();
        self.keybinds = crate::input::keybinds_from_config(&self.config);

        // Re-apply environment and cursor theme
        self.apply_environment();
        self.apply_cursor_theme();

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
    }

    // ── Output helpers (shims for backward compat + multi-monitor) ──

    /// Convenience: focused output's size.
    pub fn output_size(&self) -> (i32, i32) {
        self.outputs.get(self.focused_output)
            .map(|o| o.size)
            .unwrap_or((800, 600))
    }

    /// Convenience: focused output's workarea.
    #[allow(dead_code)]
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
        self.border_rects.clear();

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

                    if let Some(br) = geom.border_rect {
                        self.border_rects.push((br, geom.is_focused));
                    }
                } else {
                    self.space.unmap_elem(&geom.window);
                }
            }
        }
    }

    /// Set keyboard focus to the currently focused window on the focused output's workspace.
    pub fn apply_focus(&mut self) {
        let ws_idx = self.effective_ws(self.focused_output);
        let ws = &self.workspaces[ws_idx];
        let serial = SERIAL_COUNTER.next_serial();

        if let Some(window) = ws.focused() {
            let window = window.clone();
            // Deactivate all, activate focused
            for w in &ws.clients {
                w.set_activated(false);
            }
            window.set_activated(true);

            if let Some(toplevel) = window.toplevel() {
                let wl_surface = toplevel.wl_surface().clone();
                let keyboard = self.seat.get_keyboard().unwrap();
                keyboard.set_focus(self, Some(wl_surface), serial);
            }

            // Raise focused window
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

        // Check if this window should be captured by scratchpad
        if !app_id.is_empty() && self.check_scratchpad_capture(&app_id) {
            tracing::debug!("captured scratchpad window: {app_id}");
            self.scratchpad_windows.push(window);
            // If scratchpad is currently visible, map it; otherwise leave unmapped
            if self.scratchpad_visible {
                // Re-trigger scratchpad display
                self.scratchpad_visible = false;
                self.dispatch_action(crate::actions::Action::ToggleScratchpad(None));
            }
            return;
        }

        // Check window rules
        let (should_float, target_ws) = if app_id.is_empty() {
            (false, None)
        } else {
            self.check_rules(&app_id)
        };

        let ws_idx = target_ws.unwrap_or_else(|| self.effective_ws(self.focused_output));

        // Add to workspace with floating state
        self.workspaces[ws_idx].add_client_floating(window, should_float);

        // Re-layout and focus
        self.apply_layout();
        self.apply_focus();
    }

    fn toplevel_destroyed(&mut self, surface: ToplevelSurface) {
        // Find and remove the window from whichever workspace it's on
        let target = self.space.elements().find(|w| {
            w.toplevel().map_or(false, |t| t == &surface)
        }).cloned();

        if let Some(window) = target {
            self.space.unmap_elem(&window);

            // Check scratchpad windows
            if let Some(pos) = self.scratchpad_windows.iter().position(|w| w == &window) {
                self.scratchpad_windows.remove(pos);
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
    fn cursor_image(&mut self, _seat: &Seat<Self>, _image: CursorImageStatus) {}
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
