use smithay::delegate_compositor;
use smithay::delegate_data_device;
use smithay::delegate_output;
use smithay::delegate_seat;
use smithay::delegate_shm;
use smithay::delegate_xdg_shell;
use smithay::desktop::{Space, Window};
use smithay::input::{Seat, SeatHandler, SeatState};
use smithay::input::pointer::CursorImageStatus;
use smithay::reexports::calloop::LoopSignal;
use smithay::reexports::wayland_server::{Client, Display, DisplayHandle};
use smithay::reexports::wayland_server::backend::{ClientData, ClientId, DisconnectReason};
use smithay::reexports::wayland_server::protocol::wl_buffer;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::{Clock, Logical, Monotonic, Rectangle, Serial, SERIAL_COUNTER};
use smithay::wayland::buffer::BufferHandler;
use smithay::wayland::compositor::{CompositorClientState, CompositorHandler, CompositorState};
use smithay::wayland::selection::SelectionHandler;
use smithay::wayland::selection::data_device::{
    ClientDndGrabHandler, DataDeviceHandler, DataDeviceState, ServerDndGrabHandler,
};
use smithay::wayland::shell::xdg::{
    PopupSurface, PositionerState, ToplevelSurface, XdgShellHandler, XdgShellState,
};
use smithay::wayland::shm::{ShmHandler, ShmState};

use crate::input::Keybind;
use crate::layout::layout_workspace;
use crate::workspace::{Workspace, WORKSPACE_COUNT};

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
    pub shm_state: ShmState,
    pub seat_state: SeatState<Self>,
    pub data_device_state: DataDeviceState,
    pub seat: Seat<Self>,

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

    // Output geometry (full size) and work area (minus bar)
    pub output_size: (i32, i32),
    pub workarea: Rectangle<i32, Logical>,

    // IPC
    pub ipc_listener: Option<std::os::unix::net::UnixListener>,
}

impl Gate {
    pub fn new(display: &Display<Self>, loop_signal: LoopSignal) -> Self {
        let dh = display.handle();

        let compositor_state = CompositorState::new::<Self>(&dh);
        let xdg_shell_state = XdgShellState::new::<Self>(&dh);
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

        Self {
            display_handle: dh,
            loop_signal,
            running: true,
            compositor_state,
            xdg_shell_state,
            shm_state,
            seat_state,
            data_device_state,
            seat,
            space: Space::default(),
            clock: Clock::new(),
            config,
            bar_renderer,
            status_cache,
            workspaces,
            current_ws: 0,
            previous_ws: 0,
            keybinds,
            output_size: (800, 600),
            workarea: Rectangle::from_loc_and_size((0, 0), (800, 600)),
            ipc_listener: kara_ipc::server::bind_socket().ok(),
        }
    }

    /// Reload config from disk and apply changes.
    pub fn reload_config(&mut self) {
        tracing::info!("reloading config");
        self.config = kara_config::load_default_config();
        self.keybinds = crate::input::keybinds_from_config(&self.config);

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

    /// Set the output size and recompute work area (accounting for bar).
    pub fn set_output_size(&mut self, w: i32, h: i32) {
        self.output_size = (w, h);
        self.recompute_workarea();
    }

    /// Recompute work area from output size and bar config.
    fn recompute_workarea(&mut self) {
        let (w, h) = self.output_size;
        let bar_h = if self.config.bar.enabled {
            self.config.bar.height
        } else {
            0
        };

        let (y, area_h) = match self.config.bar.position {
            kara_config::BarPosition::Top => (bar_h, h - bar_h),
            kara_config::BarPosition::Bottom => (0, h - bar_h),
        };

        self.workarea = Rectangle::from_loc_and_size((0, y), (w, area_h.max(0)));
    }

    /// Build the workspace context for bar rendering.
    pub fn bar_workspace_context(&self) -> kara_sight::WorkspaceContext {
        let mut occupied = [false; 9];
        for (i, ws) in self.workspaces.iter().enumerate() {
            if i < 9 {
                occupied[i] = !ws.clients.is_empty();
            }
        }

        let focused_title = self.workspaces[self.current_ws]
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
            current_ws: self.current_ws,
            occupied_workspaces: occupied,
            focused_title,
            monitor_id: 0,
            sync_enabled: self.config.general.sync_workspaces,
        }
    }

    /// Apply the tiling layout for the current workspace and map windows in the Space
    pub fn apply_layout(&mut self) {
        let ws = &self.workspaces[self.current_ws];
        let geometries = layout_workspace(ws, self.workarea);

        for geom in &geometries {
            if geom.visible {
                // Configure the toplevel with the target size
                if let Some(toplevel) = geom.window.toplevel() {
                    toplevel.with_pending_state(|state| {
                        state.size = Some((geom.rect.size.w, geom.rect.size.h).into());
                    });
                    toplevel.send_configure();
                }
                // Map in the space at the layout position
                self.space.map_element(geom.window.clone(), geom.rect.loc, false);
            } else {
                self.space.unmap_elem(&geom.window);
            }
        }
    }

    /// Set keyboard focus to the currently focused window in the workspace
    pub fn apply_focus(&mut self) {
        let ws = &self.workspaces[self.current_ws];
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
        let ws = &mut self.workspaces[self.current_ws];
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
        let window = Window::new_wayland_window(surface);

        // Add to current workspace
        self.workspaces[self.current_ws].add_client(window);

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

impl ClientDndGrabHandler for Gate {}
impl ServerDndGrabHandler for Gate {}
impl smithay::wayland::output::OutputHandler for Gate {}

delegate_compositor!(Gate);
delegate_xdg_shell!(Gate);
delegate_shm!(Gate);
delegate_seat!(Gate);
delegate_data_device!(Gate);
delegate_output!(Gate);
