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

/// Best-effort cleanup of stale `kara-screenshot-*.png` files older
/// than a week in both `/tmp` and the system picture directory.
/// These temporaries are left behind by earlier builds of kara-gate
/// that wrote to `/tmp`, and by every quick-capture that the user
/// never manually deleted. At 500 KB–1 MB each, 100+ captures
/// accumulate to nontrivial disk use — the file names carry a
/// unix-epoch timestamp so we can prune by age without stat calls
/// on the slow path.
fn prune_old_screenshots() {
    let now_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    const WEEK_SECS: u64 = 7 * 24 * 3600;
    let mut dirs: Vec<std::path::PathBuf> = vec![std::path::PathBuf::from("/tmp")];
    // Resolve the user's picture dir without pulling in the `dirs`
    // crate — follow the XDG_PICTURES_DIR / HOME/Pictures convention.
    if let Ok(p) = std::env::var("XDG_PICTURES_DIR") {
        dirs.push(std::path::PathBuf::from(p));
    } else if let Ok(home) = std::env::var("HOME") {
        dirs.push(std::path::PathBuf::from(home).join("Pictures"));
    }
    for dir in dirs {
        let rd = match std::fs::read_dir(&dir) {
            Ok(r) => r,
            Err(_) => continue,
        };
        for entry in rd.flatten() {
            let name = entry.file_name();
            let name = match name.to_str() {
                Some(s) => s,
                None => continue,
            };
            // Match `kara-screenshot-<epoch>.png` and compare epoch
            // to "now − 7d". Skip anything that doesn't parse.
            let rest = match name.strip_prefix("kara-screenshot-") {
                Some(r) => r,
                None => continue,
            };
            let digits = match rest.strip_suffix(".png") {
                Some(d) => d,
                None => continue,
            };
            let ts: u64 = match digits.parse() {
                Ok(n) => n,
                Err(_) => continue,
            };
            if now_secs.saturating_sub(ts) > WEEK_SECS {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let quick = args.iter().any(|a| a == "-q" || a == "--quick");
    let save_path = args
        .iter()
        .position(|a| a == "-o" || a == "--output")
        .and_then(|i| args.get(i + 1))
        .cloned();

    // Prune stale captures before we add another one.
    prune_old_screenshots();

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
        border_tile_cache: theme
            .border_tile_path
            .as_deref()
            .and_then(|p| tiny_skia::Pixmap::load_png(p).ok()),
        overlay_pixmap: None,
        last_highlight: (-1, -1, -1, -1),
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
        bar_height: None,
        bar_background: None,
        bar_background_alpha: None,
        font_family: None,
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

    // Multi-monitor quick capture. We ask kara-gate for every active
    // output, request one ScreenshotOutput per output, wait for the
    // PNGs to land, then compose them into one desktop-sized PNG at
    // the global coordinate bounds (using each output's x/y as the
    // paste origin). Clipboard + save-path flow from there matches
    // the single-output path. Single-output setups fall back cleanly
    // because the bounding rect collapses to one output's rect.
    let outputs = match client.request(&kara_ipc::Request::GetOutputs) {
        Ok(kara_ipc::Response::Outputs { outputs }) => outputs,
        _ => {
            // Fallback: old compositor or IPC down. Use single-output
            // Screenshot so `kara-glimpse --quick` still does *something*.
            return legacy_single_output_capture(&mut client, save_path);
        }
    };
    if outputs.is_empty() {
        return legacy_single_output_capture(&mut client, save_path);
    }

    // Desktop bounding box — union of every output's global rect.
    let mut min_x = i32::MAX;
    let mut min_y = i32::MAX;
    let mut max_x = i32::MIN;
    let mut max_y = i32::MIN;
    for o in &outputs {
        min_x = min_x.min(o.x);
        min_y = min_y.min(o.y);
        max_x = max_x.max(o.x + o.width);
        max_y = max_y.max(o.y + o.height);
    }
    let canvas_w = (max_x - min_x).max(1);
    let canvas_h = (max_y - min_y).max(1);

    // Kick off one screenshot request per output and remember the
    // expected file path + paste origin (output.x - min_x, output.y -
    // min_y).
    let mut pending: Vec<(String, i32, i32)> = Vec::new();
    for o in &outputs {
        let req = kara_ipc::Request::ScreenshotOutput { name: o.name.clone() };
        match client.request(&req) {
            Ok(kara_ipc::Response::ScreenshotDone { path }) => {
                pending.push((path, o.x - min_x, o.y - min_y));
            }
            Ok(kara_ipc::Response::Error { message }) => {
                eprintln!("kara-glimpse: compositor error for {}: {message}", o.name);
            }
            _ => {}
        }
    }
    if pending.is_empty() {
        eprintln!("kara-glimpse: no outputs captured");
        std::process::exit(1);
    }

    // Wait for all screenshot files to land.
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(1500);
    loop {
        let missing = pending
            .iter()
            .any(|(p, _, _)| !std::path::Path::new(p).exists());
        if !missing {
            break;
        }
        if std::time::Instant::now() >= deadline {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(15));
    }

    // Compose into one pixmap in global coords.
    let Some(mut canvas) = tiny_skia::Pixmap::new(canvas_w as u32, canvas_h as u32) else {
        eprintln!("kara-glimpse: canvas allocation failed ({canvas_w}x{canvas_h})");
        std::process::exit(1);
    };
    for (path, dx, dy) in &pending {
        if !std::path::Path::new(path).exists() {
            eprintln!("kara-glimpse: missing capture piece {path}");
            continue;
        }
        let Ok(pm) = tiny_skia::Pixmap::load_png(path) else {
            eprintln!("kara-glimpse: failed to load capture piece {path}");
            continue;
        };
        canvas.draw_pixmap(
            *dx,
            *dy,
            pm.as_ref(),
            &tiny_skia::PixmapPaint::default(),
            tiny_skia::Transform::identity(),
            None,
        );
        let _ = std::fs::remove_file(path);
    }

    // Save composed PNG to a stable /tmp path and route through the
    // usual wait_and_copy flow so wl-copy + notify-send behavior
    // matches the single-output path.
    let composed_path = format!(
        "/tmp/kara-screenshot-{}.png",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
    );
    if let Err(e) = canvas.save_png(&composed_path) {
        eprintln!("kara-glimpse: failed to write composed PNG: {e}");
        std::process::exit(1);
    }
    wait_and_copy(&composed_path, save_path);
}

/// Backstop for the rare case where GetOutputs fails or returns empty.
/// Mirrors the pre-multi-monitor behaviour: plain `Screenshot` IPC,
/// single focused-output capture.
fn legacy_single_output_capture(
    client: &mut kara_ipc::IpcClient,
    save_path: Option<String>,
) {
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
    let final_path = if let Some(dest) = save_path {
        if let Err(e) = std::fs::copy(capture_path, &dest) {
            eprintln!("kara-glimpse: failed to save to {dest}: {e}");
            capture_path.to_string()
        } else {
            println!("{dest}");
            dest
        }
    } else {
        println!("{capture_path}");
        capture_path.to_string()
    };

    notify_captured(&final_path);
}

/// Fire a desktop notification confirming the screenshot. Uses the
/// saved PNG itself as the notification icon so whisper renders a
/// thumbnail of the capture on the card — closes the screenshot loop
/// without a separate thumbnail pipeline. Best-effort: if notify-send
/// isn't installed or the D-Bus service is down, we silently skip.
fn notify_captured(path: &str) {
    use std::process::{Command, Stdio};

    let _ = Command::new("notify-send")
        .args([
            "--app-name=kara-glimpse",
            "--icon",
            path,
            "--expire-time=4000",
            "Screenshot captured",
            path,
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
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
    /// Cached decoded border tile pixmap — loaded once at init from
    /// `theme.border_tile_path`, not per-frame.
    border_tile_cache: Option<tiny_skia::Pixmap>,
    /// Pre-allocated overlay pixmap — reused every frame to avoid
    /// the cost of allocating + zeroing a full-screen RGBA buffer
    /// on every hover move.
    overlay_pixmap: Option<tiny_skia::Pixmap>,
    /// Last rendered highlight rect — skip redraw when unchanged.
    last_highlight: (i32, i32, i32, i32),
}

impl Glimpse {
    fn draw(&mut self, _qh: &QueueHandle<Self>) {
        if self.width == 0 || self.height == 0 {
            return;
        }

        let highlight = self.selection.highlight_rect();

        // Skip redraw when the highlight hasn't moved — avoids the
        // full overlay re-render + shm copy on every pointer micro-
        // motion within the same window/fullscreen zone.
        if highlight == self.last_highlight {
            return;
        }
        self.last_highlight = highlight;

        // Lazily allocate (or re-allocate on resize) the overlay pixmap.
        let need_alloc = self
            .overlay_pixmap
            .as_ref()
            .map(|pm| pm.width() != self.width || pm.height() != self.height)
            .unwrap_or(true);
        if need_alloc {
            self.overlay_pixmap = tiny_skia::Pixmap::new(self.width, self.height);
        }
        let pixmap = match self.overlay_pixmap.as_mut() {
            Some(pm) => pm,
            None => return,
        };
        if !overlay::render_overlay(
            pixmap,
            self.width,
            self.height,
            highlight,
            &self.theme,
            self.border_tile_cache.as_ref(),
        ) {
            return;
        }

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
                PointerEventKind::Enter { .. } => {
                    self.selection.pointer = (event.position.0, event.position.1);
                    self.selection.update_hover(&self.windows);
                    self.draw(qh);
                }
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
