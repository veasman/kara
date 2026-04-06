mod desktop;
mod filter;
mod ui;

use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState},
    delegate_compositor, delegate_keyboard, delegate_layer, delegate_output,
    delegate_registry, delegate_seat, delegate_shm,
    output::{OutputHandler, OutputState},
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
    seat::{
        keyboard::{KeyEvent, KeyboardHandler, Keysym, Modifiers},
        Capability, SeatHandler, SeatState,
    },
    shell::{
        wlr_layer::{
            KeyboardInteractivity, Layer, LayerShell, LayerShellHandler, LayerSurface,
            LayerSurfaceConfigure,
        },
        WaylandSurface,
    },
    shm::{slot::SlotPool, Shm, ShmHandler},
};
use wayland_client::{
    globals::registry_queue_init,
    protocol::{wl_keyboard, wl_output, wl_seat, wl_shm, wl_surface},
    Connection, QueueHandle,
};

use crate::desktop::DesktopEntry;
use crate::ui::LauncherUI;

const WIDTH: u32 = 600;
const HEIGHT: u32 = 420;

fn main() {
    // Query theme from compositor
    let theme = match kara_ipc::IpcClient::connect()
        .and_then(|mut c| c.request(&kara_ipc::Request::GetTheme))
    {
        Ok(kara_ipc::Response::Theme { colors }) => colors,
        _ => {
            // Fallback theme
            kara_ipc::ThemeColors {
                bg: 0x111111,
                surface: 0x1b1b1b,
                text: 0xf2f2f2,
                text_muted: 0x5c5c5c,
                accent: 0x6bacac,
                accent_soft: 0x458588,
                border: 0x353535,
            }
        }
    };

    // Discover .desktop files
    let entries = desktop::discover();

    // Connect to Wayland
    let conn = Connection::connect_to_env().expect("failed to connect to Wayland");
    let (globals, mut event_queue) = registry_queue_init(&conn).expect("failed to init registry");
    let qh = event_queue.handle();

    let compositor = CompositorState::bind(&globals, &qh).expect("wl_compositor not available");
    let layer_shell = LayerShell::bind(&globals, &qh).expect("layer shell not available");
    let shm = Shm::bind(&globals, &qh).expect("wl_shm not available");

    // Create layer surface
    let surface = compositor.create_surface(&qh);
    let layer = layer_shell.create_layer_surface(
        &qh, surface, Layer::Overlay, Some("kara-summon"), None,
    );
    layer.set_keyboard_interactivity(KeyboardInteractivity::Exclusive);
    layer.set_size(WIDTH, HEIGHT);
    layer.commit();

    let pool = SlotPool::new(WIDTH as usize * HEIGHT as usize * 4, &shm)
        .expect("failed to create SHM pool");

    let launcher_ui = LauncherUI::new(theme, WIDTH, HEIGHT, "monospace", 14.0);
    let filtered = filter::filter(&entries, "");

    let mut launcher = Launcher {
        registry_state: RegistryState::new(&globals),
        seat_state: SeatState::new(&globals, &qh),
        output_state: OutputState::new(&globals, &qh),
        shm,

        exit: false,
        first_configure: true,
        pool,
        width: WIDTH,
        height: HEIGHT,
        layer,
        keyboard: None,

        entries,
        query: String::new(),
        filtered,
        selected: 0,
        scroll_offset: 0,
        ui: launcher_ui,
        launch_command: None,
    };

    loop {
        event_queue.blocking_dispatch(&mut launcher).unwrap();
        if launcher.exit {
            break;
        }
    }

    // Launch the selected command
    if let Some(cmd) = launcher.launch_command {
        std::process::Command::new("sh")
            .args(["-c", &cmd])
            .spawn()
            .ok();
    }
}

struct Launcher {
    registry_state: RegistryState,
    seat_state: SeatState,
    output_state: OutputState,
    shm: Shm,

    exit: bool,
    first_configure: bool,
    pool: SlotPool,
    width: u32,
    height: u32,
    layer: LayerSurface,
    keyboard: Option<wl_keyboard::WlKeyboard>,

    entries: Vec<DesktopEntry>,
    query: String,
    filtered: Vec<usize>,
    selected: usize,
    scroll_offset: usize,
    ui: LauncherUI,
    launch_command: Option<String>,
}

impl Launcher {
    fn update_filter(&mut self) {
        self.filtered = filter::filter(&self.entries, &self.query);
        self.selected = 0;
        self.scroll_offset = 0;
    }

    fn selected_command(&self) -> Option<String> {
        if self.selected < self.filtered.len() {
            Some(self.entries[self.filtered[self.selected]].exec.clone())
        } else if filter::is_command(&self.query) || (!self.query.is_empty() && self.filtered.is_empty()) {
            Some(self.query.clone())
        } else {
            None
        }
    }

    fn draw(&mut self, _qh: &QueueHandle<Self>) {
        let show_fallback = !self.query.is_empty()
            && (filter::is_command(&self.query) || self.filtered.is_empty());
        let pixmap = match self.ui.render(
            &self.query, &self.entries, &self.filtered,
            self.selected, self.scroll_offset, show_fallback,
        ) {
            Some(p) => p,
            None => return,
        };

        let width = self.width;
        let height = self.height;
        let stride = width as i32 * 4;

        let (buffer, canvas) = self.pool
            .create_buffer(width as i32, height as i32, stride, wl_shm::Format::Argb8888)
            .expect("create buffer");

        // Copy pixmap data to buffer, swizzling RGBA → BGRA (ARGB on LE)
        let src = pixmap.data();
        for (dst, src) in canvas.chunks_exact_mut(4).zip(src.chunks_exact(4)) {
            dst[0] = src[2]; // B
            dst[1] = src[1]; // G
            dst[2] = src[0]; // R
            dst[3] = src[3]; // A
        }

        self.layer.wl_surface().damage_buffer(0, 0, width as i32, height as i32);
        buffer.attach_to(self.layer.wl_surface()).expect("buffer attach");
        self.layer.commit();
    }
}

// ── SCTK trait implementations ─────────────────────────────────────

impl CompositorHandler for Launcher {
    fn scale_factor_changed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: i32) {}
    fn transform_changed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: wl_output::Transform) {}
    fn frame(&mut self, _: &Connection, qh: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: u32) {
        self.draw(qh);
    }
    fn surface_enter(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: &wl_output::WlOutput) {}
    fn surface_leave(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: &wl_output::WlOutput) {}
}

impl OutputHandler for Launcher {
    fn output_state(&mut self) -> &mut OutputState { &mut self.output_state }
    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
}

impl LayerShellHandler for Launcher {
    fn closed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &LayerSurface) {
        self.exit = true;
    }

    fn configure(&mut self, _: &Connection, qh: &QueueHandle<Self>, _: &LayerSurface, configure: LayerSurfaceConfigure, _: u32) {
        if configure.new_size.0 > 0 && configure.new_size.1 > 0 {
            self.width = configure.new_size.0;
            self.height = configure.new_size.1;
        }
        if self.first_configure {
            self.first_configure = false;
            self.draw(qh);
        }
    }
}

impl SeatHandler for Launcher {
    fn seat_state(&mut self) -> &mut SeatState { &mut self.seat_state }
    fn new_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
    fn new_capability(&mut self, _: &Connection, qh: &QueueHandle<Self>, seat: wl_seat::WlSeat, capability: Capability) {
        if capability == Capability::Keyboard && self.keyboard.is_none() {
            let kb = self.seat_state.get_keyboard(qh, &seat, None).expect("failed to get keyboard");
            self.keyboard = Some(kb);
        }
    }
    fn remove_capability(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat, capability: Capability) {
        if capability == Capability::Keyboard {
            if let Some(kb) = self.keyboard.take() {
                kb.release();
            }
        }
    }
    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
}

impl KeyboardHandler for Launcher {
    fn enter(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: &wl_surface::WlSurface, _: u32, _: &[u32], _: &[Keysym]) {}
    fn leave(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: &wl_surface::WlSurface, _: u32) {}

    fn press_key(&mut self, _: &Connection, qh: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: u32, event: KeyEvent) {
        match event.keysym {
            Keysym::Escape => {
                self.exit = true;
            }
            Keysym::Return | Keysym::KP_Enter => {
                self.launch_command = self.selected_command();
                self.exit = true;
            }
            Keysym::BackSpace => {
                self.query.pop();
                self.update_filter();
                self.draw(qh);
            }
            Keysym::Up => {
                if self.selected > 0 {
                    self.selected -= 1;
                    if self.selected < self.scroll_offset {
                        self.scroll_offset = self.selected;
                    }
                }
                self.draw(qh);
            }
            Keysym::Down | Keysym::Tab => {
                let max = self.filtered.len()
                    + if !self.query.is_empty() && (filter::is_command(&self.query) || self.filtered.is_empty()) { 1 } else { 0 };
                if max > 0 && self.selected + 1 < max {
                    self.selected += 1;
                    if self.selected >= self.scroll_offset + 10 {
                        self.scroll_offset = self.selected.saturating_sub(9);
                    }
                }
                self.draw(qh);
            }
            _ => {
                if let Some(text) = event.utf8 {
                    if !text.is_empty() && text.chars().all(|c| !c.is_control()) {
                        self.query.push_str(&text);
                        self.update_filter();
                        self.draw(qh);
                    }
                }
            }
        }
    }

    fn release_key(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: u32, _: KeyEvent) {}
    fn update_modifiers(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: u32, _: Modifiers, _: u32) {}
}

impl ShmHandler for Launcher {
    fn shm_state(&mut self) -> &mut Shm { &mut self.shm }
}

delegate_compositor!(Launcher);
delegate_output!(Launcher);
delegate_shm!(Launcher);
delegate_seat!(Launcher);
delegate_keyboard!(Launcher);
delegate_layer!(Launcher);
delegate_registry!(Launcher);

impl ProvidesRegistryState for Launcher {
    fn registry(&mut self) -> &mut RegistryState { &mut self.registry_state }
    registry_handlers![OutputState, SeatState];
}
