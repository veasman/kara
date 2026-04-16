mod overlay;
mod selection;

use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState},
    delegate_compositor, delegate_keyboard, delegate_layer, delegate_output,
    delegate_pointer, delegate_registry, delegate_seat, delegate_shm,
    output::{OutputHandler, OutputState},
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
    seat::{
        keyboard::{KeyEvent, KeyboardHandler, Keysym, Modifiers},
        pointer::{PointerEvent, PointerEventKind, PointerHandler},
        Capability, SeatHandler, SeatState,
    },
    shell::{
        wlr_layer::{
            Anchor, KeyboardInteractivity, Layer, LayerShell, LayerShellHandler,
            LayerSurface, LayerSurfaceConfigure,
        },
        WaylandSurface,
    },
    shm::{slot::SlotPool, Shm, ShmHandler},
};
use wayland_client::{
    globals::registry_queue_init,
    protocol::{wl_keyboard, wl_output, wl_pointer, wl_seat, wl_shm, wl_surface},
    Connection, QueueHandle,
};

use selection::SelectionState;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let quick = args.iter().any(|a| a == "-q" || a == "--quick");
    let save_path = args
        .iter()
        .position(|a| a == "-o" || a == "--output")
        .and_then(|i| args.get(i + 1))
        .cloned();

    if quick {
        quick_capture(save_path);
        return;
    }

    // Interactive mode
    // 1. Get theme + window geometries from compositor
    let (theme, windows) = match kara_ipc::IpcClient::connect() {
        Ok(mut c) => {
            let theme = match c.request(&kara_ipc::Request::GetTheme) {
                Ok(kara_ipc::Response::Theme { colors }) => colors,
                _ => default_theme(),
            };
            let windows = match kara_ipc::IpcClient::connect()
                .and_then(|mut c2| c2.request(&kara_ipc::Request::GetWindowGeometries))
            {
                Ok(kara_ipc::Response::WindowGeometries { windows }) => windows,
                _ => vec![],
            };
            (theme, windows)
        }
        Err(_) => (default_theme(), vec![]),
    };

    // 2. Connect to Wayland
    let wayland_display = std::env::var("WAYLAND_DISPLAY").unwrap_or_default();
    let conn = match Connection::connect_to_env() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("kara-glimpse: failed to connect to Wayland: {e}");
            eprintln!("  WAYLAND_DISPLAY={wayland_display:?}");
            eprintln!("  XDG_RUNTIME_DIR={:?}", std::env::var("XDG_RUNTIME_DIR").unwrap_or_default());
            std::process::exit(1);
        }
    };
    let (globals, mut event_queue) = registry_queue_init(&conn).expect("failed to init registry");
    let qh = event_queue.handle();

    let compositor = CompositorState::bind(&globals, &qh).expect("wl_compositor not available");
    let layer_shell = LayerShell::bind(&globals, &qh).expect("layer shell not available");
    let shm = Shm::bind(&globals, &qh).expect("wl_shm not available");

    // 3. Create fullscreen layer surface
    let surface = compositor.create_surface(&qh);
    let layer = layer_shell.create_layer_surface(
        &qh,
        surface,
        Layer::Overlay,
        Some("kara-glimpse"),
        None,
    );
    layer.set_keyboard_interactivity(KeyboardInteractivity::Exclusive);
    layer.set_anchor(Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT);
    layer.set_exclusive_zone(-1);
    layer.commit();

    let pool = SlotPool::new(1920 * 1080 * 4, &shm).expect("failed to create SHM pool");

    let mut glimpse = Glimpse {
        registry_state: RegistryState::new(&globals),
        seat_state: SeatState::new(&globals, &qh),
        output_state: OutputState::new(&globals, &qh),
        shm,
        exit: false,
        confirmed: false,
        first_configure: true,
        pool,
        width: 0,
        height: 0,
        layer,
        keyboard: None,
        pointer: None,
        theme,
        windows,
        selection: SelectionState::new(0, 0),
        save_path,
    };

    loop {
        event_queue.blocking_dispatch(&mut glimpse).unwrap();
        if glimpse.exit {
            break;
        }
    }

    // If confirmed, capture the selected region
    if glimpse.confirmed {
        let (x, y, w, h) = glimpse.selection.end_press();
        let save_path_for_capture = glimpse.save_path.clone();

        // Unmap the overlay and round-trip so the compositor processes the
        // destroy before we ask it for a screenshot. If we just drop glimpse,
        // the destroy request sits in the outgoing buffer and the overlay ends
        // up baked into the captured frame (and the overlay stays on screen
        // until the next unrelated render).
        glimpse.layer.wl_surface().attach(None, 0, 0);
        glimpse.layer.wl_surface().commit();
        let _ = event_queue.roundtrip(&mut glimpse);

        drop(glimpse);

        do_capture(x, y, w, h, save_path_for_capture);
    }
}

fn default_theme() -> kara_ipc::ThemeColors {
    kara_ipc::ThemeColors {
        bg: 0x111111,
        surface: 0x1b1b1b,
        text: 0xf2f2f2,
        text_muted: 0x5c5c5c,
        accent: 0x6bacac,
        accent_soft: 0x458588,
        border: 0x353535,
        border_px: None,
        border_radius: None,
        border_tile_path: None,
    }
}

fn quick_capture(save_path: Option<String>) {
    let mut client = match kara_ipc::IpcClient::connect() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("kara-glimpse: failed to connect to compositor: {e}");
            std::process::exit(1);
        }
    };
    let response = match client.request(&kara_ipc::Request::Screenshot) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("kara-glimpse: IPC error: {e}");
            std::process::exit(1);
        }
    };
    let capture_path = match response {
        kara_ipc::Response::ScreenshotDone { path } => path,
        kara_ipc::Response::Error { message } => {
            eprintln!("kara-glimpse: compositor error: {message}");
            std::process::exit(1);
        }
        _ => {
            eprintln!("kara-glimpse: unexpected response");
            std::process::exit(1);
        }
    };
    wait_and_copy(&capture_path, save_path);
}

fn do_capture(x: i32, y: i32, w: i32, h: i32, save_path: Option<String>) {
    let mut client = match kara_ipc::IpcClient::connect() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("kara-glimpse: failed to connect to compositor: {e}");
            std::process::exit(1);
        }
    };
    let response = match client.request(&kara_ipc::Request::ScreenshotRegion { x, y, w, h }) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("kara-glimpse: IPC error: {e}");
            std::process::exit(1);
        }
    };
    let capture_path = match response {
        kara_ipc::Response::ScreenshotDone { path } => path,
        kara_ipc::Response::Error { message } => {
            eprintln!("kara-glimpse: compositor error: {message}");
            std::process::exit(1);
        }
        _ => {
            eprintln!("kara-glimpse: unexpected response");
            std::process::exit(1);
        }
    };
    wait_and_copy(&capture_path, save_path);
}

fn wait_and_copy(capture_path: &str, save_path: Option<String>) {
    for _ in 0..50 {
        if std::path::Path::new(capture_path).exists() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    if !std::path::Path::new(capture_path).exists() {
        eprintln!("kara-glimpse: screenshot file not created: {capture_path}");
        std::process::exit(1);
    }
    // Copy to clipboard via wl-copy. wl-copy forks and daemonizes to keep
    // serving the selection after we exit, so we must NOT wait on it — doing
    // so can pin kara-glimpse alive indefinitely. Spawn and walk away.
    use std::process::Stdio;
    match std::fs::File::open(capture_path) {
        Ok(file) => {
            if std::process::Command::new("wl-copy")
                .args(["--type", "image/png"])
                .stdin(Stdio::from(file))
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .is_err()
            {
                eprintln!("kara-glimpse: failed to run wl-copy, clipboard copy skipped");
            }
        }
        Err(e) => eprintln!("kara-glimpse: failed to open capture for clipboard: {e}"),
    }
    if let Some(dest) = save_path {
        if let Err(e) = std::fs::copy(capture_path, &dest) {
            eprintln!("kara-glimpse: failed to save to {dest}: {e}");
        } else {
            println!("{dest}");
        }
    } else {
        println!("{capture_path}");
    }
}

struct Glimpse {
    registry_state: RegistryState,
    seat_state: SeatState,
    output_state: OutputState,
    shm: Shm,

    exit: bool,
    confirmed: bool,
    first_configure: bool,
    pool: SlotPool,
    width: u32,
    height: u32,
    layer: LayerSurface,
    keyboard: Option<wl_keyboard::WlKeyboard>,
    pointer: Option<wl_pointer::WlPointer>,

    theme: kara_ipc::ThemeColors,
    windows: Vec<kara_ipc::WindowGeometry>,
    selection: SelectionState,
    save_path: Option<String>,
}

impl Glimpse {
    fn draw(&mut self, _qh: &QueueHandle<Self>) {
        if self.width == 0 || self.height == 0 {
            return;
        }

        let highlight = self.selection.highlight_rect();
        let pixmap = match overlay::render_overlay(self.width, self.height, highlight, &self.theme)
        {
            Some(p) => p,
            None => return,
        };

        let stride = self.width as i32 * 4;
        let (buffer, canvas) = self
            .pool
            .create_buffer(
                self.width as i32,
                self.height as i32,
                stride,
                wl_shm::Format::Argb8888,
            )
            .expect("create buffer");

        // RGBA -> BGRA swizzle
        let src = pixmap.data();
        for (dst, src) in canvas.chunks_exact_mut(4).zip(src.chunks_exact(4)) {
            dst[0] = src[2];
            dst[1] = src[1];
            dst[2] = src[0];
            dst[3] = src[3];
        }

        self.layer
            .wl_surface()
            .damage_buffer(0, 0, self.width as i32, self.height as i32);
        buffer
            .attach_to(self.layer.wl_surface())
            .expect("buffer attach");
        self.layer.commit();
    }
}

// ── SCTK trait implementations ─────────────────────────────────────

impl CompositorHandler for Glimpse {
    fn scale_factor_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: i32,
    ) {
    }
    fn transform_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: wl_output::Transform,
    ) {
    }
    fn frame(
        &mut self,
        _: &Connection,
        qh: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: u32,
    ) {
        self.draw(qh);
    }
    fn surface_enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: &wl_output::WlOutput,
    ) {
    }
    fn surface_leave(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: &wl_output::WlOutput,
    ) {
    }
}

impl OutputHandler for Glimpse {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }
    fn new_output(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: wl_output::WlOutput,
    ) {
    }
    fn update_output(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: wl_output::WlOutput,
    ) {
    }
    fn output_destroyed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: wl_output::WlOutput,
    ) {
    }
}

impl LayerShellHandler for Glimpse {
    fn closed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &LayerSurface) {
        self.exit = true;
    }

    fn configure(
        &mut self,
        _: &Connection,
        qh: &QueueHandle<Self>,
        _: &LayerSurface,
        configure: LayerSurfaceConfigure,
        _: u32,
    ) {
        if configure.new_size.0 > 0 && configure.new_size.1 > 0 {
            self.width = configure.new_size.0;
            self.height = configure.new_size.1;
            self.selection = SelectionState::new(self.width as i32, self.height as i32);
            // Resize pool if needed
            let needed = self.width as usize * self.height as usize * 4;
            if self.pool.len() < needed {
                self.pool.resize(needed).ok();
            }
        }
        if self.first_configure {
            self.first_configure = false;
            self.selection.update_hover(&self.windows);
            self.draw(qh);
        }
    }
}

impl SeatHandler for Glimpse {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seat_state
    }
    fn new_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
    fn new_capability(
        &mut self,
        _: &Connection,
        qh: &QueueHandle<Self>,
        seat: wl_seat::WlSeat,
        capability: Capability,
    ) {
        if capability == Capability::Keyboard && self.keyboard.is_none() {
            let kb = self
                .seat_state
                .get_keyboard(qh, &seat, None)
                .expect("get keyboard");
            self.keyboard = Some(kb);
        }
        if capability == Capability::Pointer && self.pointer.is_none() {
            let ptr = self
                .seat_state
                .get_pointer(qh, &seat)
                .expect("get pointer");
            self.pointer = Some(ptr);
        }
    }
    fn remove_capability(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: wl_seat::WlSeat,
        capability: Capability,
    ) {
        if capability == Capability::Keyboard {
            if let Some(kb) = self.keyboard.take() {
                kb.release();
            }
        }
        if capability == Capability::Pointer {
            if let Some(ptr) = self.pointer.take() {
                ptr.release();
            }
        }
    }
    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
}

impl KeyboardHandler for Glimpse {
    fn enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: &wl_surface::WlSurface,
        _: u32,
        _: &[u32],
        _: &[Keysym],
    ) {
    }
    fn leave(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: &wl_surface::WlSurface,
        _: u32,
    ) {
    }
    fn press_key(
        &mut self,
        _: &Connection,
        _qh: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: u32,
        event: KeyEvent,
    ) {
        if event.keysym == Keysym::Escape {
            self.exit = true;
        } else if event.keysym == Keysym::Return || event.keysym == Keysym::KP_Enter {
            self.confirmed = true;
            self.exit = true;
        }
    }
    fn release_key(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: u32,
        _: KeyEvent,
    ) {
    }
    fn update_modifiers(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: u32,
        _: Modifiers,
        _: u32,
    ) {
    }
}

impl PointerHandler for Glimpse {
    fn pointer_frame(
        &mut self,
        _: &Connection,
        qh: &QueueHandle<Self>,
        _: &wl_pointer::WlPointer,
        events: &[PointerEvent],
    ) {
        for event in events {
            match event.kind {
                PointerEventKind::Motion { .. } => {
                    self.selection.pointer = (event.position.0, event.position.1);
                    self.selection.update_hover(&self.windows);
                    self.draw(qh);
                }
                PointerEventKind::Press { button, .. } => {
                    if button == 0x110 {
                        // BTN_LEFT
                        self.selection.start_press();
                    }
                }
                PointerEventKind::Release { button, .. } => {
                    if button == 0x110 {
                        // BTN_LEFT — confirm
                        self.confirmed = true;
                        self.exit = true;
                    } else if button == 0x111 {
                        // BTN_RIGHT — cancel
                        self.exit = true;
                    }
                }
                _ => {}
            }
        }
    }
}

impl ShmHandler for Glimpse {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

delegate_compositor!(Glimpse);
delegate_output!(Glimpse);
delegate_shm!(Glimpse);
delegate_seat!(Glimpse);
delegate_keyboard!(Glimpse);
delegate_pointer!(Glimpse);
delegate_layer!(Glimpse);
delegate_registry!(Glimpse);

impl ProvidesRegistryState for Glimpse {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }
    registry_handlers![OutputState, SeatState];
}
