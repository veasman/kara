mod dbus;
mod notification;
mod popover_ipc;
mod ui;

use std::sync::mpsc;

use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState},
    delegate_compositor, delegate_layer, delegate_output, delegate_pointer, delegate_registry,
    delegate_seat, delegate_shm,
    output::{OutputHandler, OutputState},
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
    seat::{
        pointer::{PointerEvent, PointerEventKind, PointerHandler},
        Capability, SeatHandler, SeatState,
    },
    shell::{
        wlr_layer::{
            Anchor, KeyboardInteractivity, Layer, LayerShell, LayerShellHandler, LayerSurface,
            LayerSurfaceConfigure,
        },
        WaylandSurface,
    },
    shm::{slot::SlotPool, Shm, ShmHandler},
};
use wayland_client::{
    globals::registry_queue_init,
    protocol::{wl_output, wl_pointer, wl_seat, wl_shm, wl_surface},
    Connection, QueueHandle,
};

use crate::dbus::{emit_action_invoked, emit_notification_closed, DbusEvent};
use crate::notification::{NotificationQueue, Urgency};
use crate::popover_ipc::PopoverEvent;
use crate::ui::{HitRegion, NotificationUI};

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

struct Whisper {
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

    queue: NotificationQueue,
    ui: NotificationUI,
    dbus_rx: mpsc::Receiver<DbusEvent>,
    popover_rx: mpsc::Receiver<PopoverEvent>,
    surface_visible: bool,

    // Pointer / hit-test state — populated on every render so clicks on
    // action buttons can resolve to (notification id, action id). The
    // wl_pointer here is held by SCTK once created; we track the current
    // pointer position relative to the layer surface to match against.
    pointer: Option<wl_pointer::WlPointer>,
    pointer_pos: (f64, f64),
    hit_regions: Vec<HitRegion>,

    // D-Bus connection for emitting ActionInvoked / NotificationClosed
    // signals. None if the D-Bus session setup failed at startup.
    dbus_conn: Option<zbus::blocking::Connection>,
}

impl Whisper {
    fn update_surface(&mut self, _qh: &QueueHandle<Self>) {
        let notifications = self.queue.visible();

        if notifications.is_empty() {
            if self.surface_visible {
                // Shrink to 1x1 transparent
                self.layer.set_size(NotificationUI::card_width(), 1);
                self.layer.commit();

                // Draw transparent 1x1
                let stride = NotificationUI::card_width() as i32 * 4;
                if let Ok((buffer, canvas)) = self.pool.create_buffer(
                    NotificationUI::card_width() as i32,
                    1,
                    stride,
                    wl_shm::Format::Argb8888,
                ) {
                    canvas.fill(0);
                    self.layer
                        .wl_surface()
                        .damage_buffer(0, 0, NotificationUI::card_width() as i32, 1);
                    buffer.attach_to(self.layer.wl_surface()).expect("buffer attach");
                    self.layer.commit();
                }
                self.surface_visible = false;
            }
            return;
        }

        let new_height = self.ui.total_height_for(notifications);
        let new_width = NotificationUI::card_width();

        if new_width != self.width || new_height != self.height {
            self.width = new_width;
            self.height = new_height;
            self.layer.set_size(new_width, new_height);
            self.layer.commit();
        }

        let (maybe_pixmap, hits) = self.ui.render(notifications);
        self.hit_regions = hits;
        if let Some(pixmap) = maybe_pixmap {
            let stride = self.width as i32 * 4;
            if let Ok((buffer, canvas)) = self.pool.create_buffer(
                self.width as i32,
                self.height as i32,
                stride,
                wl_shm::Format::Argb8888,
            ) {
                // Copy pixmap RGBA -> BGRA (ARGB on LE)
                let src = pixmap.data();
                for (dst, src) in canvas.chunks_exact_mut(4).zip(src.chunks_exact(4)) {
                    dst[0] = src[2]; // B
                    dst[1] = src[1]; // G
                    dst[2] = src[0]; // R
                    dst[3] = src[3]; // A
                }

                self.layer.wl_surface().damage_buffer(
                    0,
                    0,
                    self.width as i32,
                    self.height as i32,
                );
                buffer.attach_to(self.layer.wl_surface()).expect("buffer attach");
                self.layer.commit();
            }
        }

        self.surface_visible = true;
    }
}

// ── SCTK trait implementations ─────────────────────────────────────

impl CompositorHandler for Whisper {
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
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: u32,
    ) {
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

impl OutputHandler for Whisper {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }
    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {
    }
}

impl LayerShellHandler for Whisper {
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
        }
        if self.first_configure {
            self.first_configure = false;
            self.update_surface(qh);
        }
    }
}

impl SeatHandler for Whisper {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seat_state
    }
    fn new_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
    fn new_capability(
        &mut self,
        _conn: &Connection,
        qh: &QueueHandle<Self>,
        seat: wl_seat::WlSeat,
        capability: Capability,
    ) {
        // Grab the pointer capability so notification action buttons can
        // receive clicks. Without this handler nothing drives the button
        // hit-test and the cards look decorative.
        if capability == Capability::Pointer && self.pointer.is_none() {
            match self.seat_state.get_pointer(qh, &seat) {
                Ok(p) => self.pointer = Some(p),
                Err(e) => eprintln!("kara-whisper: failed to get pointer: {e}"),
            }
        }
    }
    fn remove_capability(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: wl_seat::WlSeat,
        capability: Capability,
    ) {
        if capability == Capability::Pointer {
            if let Some(p) = self.pointer.take() {
                p.release();
            }
        }
    }
    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
}

impl PointerHandler for Whisper {
    fn pointer_frame(
        &mut self,
        _conn: &Connection,
        qh: &QueueHandle<Self>,
        _pointer: &wl_pointer::WlPointer,
        events: &[PointerEvent],
    ) {
        // Snapshot the layer's wl_surface as an owned clone so we can
        // compare per-event without holding an immutable borrow of
        // self.layer across the mutable `handle_click` call.
        let layer_surface = self.layer.wl_surface().clone();
        for event in events {
            // Ignore events for unrelated surfaces — SCTK delivers the
            // full pointer frame regardless of which surface owns it.
            if event.surface != layer_surface {
                continue;
            }
            match event.kind {
                PointerEventKind::Enter { .. } | PointerEventKind::Motion { .. } => {
                    self.pointer_pos = event.position;
                }
                PointerEventKind::Leave { .. } => {
                    self.pointer_pos = (-1.0, -1.0);
                }
                PointerEventKind::Press { button, .. } => {
                    // BTN_LEFT = 0x110.
                    if button == 0x110 {
                        self.handle_click(qh);
                    }
                }
                _ => {}
            }
        }
    }
}

impl Whisper {
    /// Resolve the current pointer position against the latest hit
    /// regions. Action buttons fire ActionInvoked. Card-body clicks
    /// dismiss the notification; if the body carries an absolute-path
    /// icon (e.g. glimpse's saved screenshot), `xdg-open` is spawned so
    /// tapping the thumbnail opens the capture in the default viewer.
    /// Both paths emit NotificationClosed(reason=2 dismissed) so
    /// notify-send --wait clients unblock.
    fn handle_click(&mut self, qh: &QueueHandle<Self>) {
        use crate::ui::HitKind;

        let (px, py) = self.pointer_pos;
        if px < 0.0 || py < 0.0 {
            return;
        }
        let hit = self
            .hit_regions
            .iter()
            .find(|r| r.contains(px, py))
            .cloned();
        let Some(hit) = hit else { return };

        match &hit.kind {
            HitKind::Action { id } => {
                if let Some(conn) = self.dbus_conn.as_ref() {
                    emit_action_invoked(conn, hit.notif_id, id);
                    emit_notification_closed(conn, hit.notif_id, 2);
                }
            }
            HitKind::Body { open_path } => {
                if let Some(path) = open_path {
                    let _ = std::process::Command::new("xdg-open")
                        .arg(path)
                        .stdout(std::process::Stdio::null())
                        .stderr(std::process::Stdio::null())
                        .spawn();
                }
                // If the sender provided an implicit `default` action
                // (freedesktop spec — the click-anywhere target),
                // invoke it on body-click so apps like Thunderbird
                // still see "user activated this mail alert" instead
                // of a silent dismissal. The action button itself is
                // hidden from the card (see `Notification::button_actions`)
                // so this is the only surface that reaches the sender.
                if let Some(conn) = self.dbus_conn.as_ref() {
                    let default_id = self
                        .queue
                        .find(hit.notif_id)
                        .and_then(|n| n.default_action_id())
                        .map(|s| s.to_string());
                    if let Some(id) = default_id {
                        emit_action_invoked(conn, hit.notif_id, &id);
                    }
                    emit_notification_closed(conn, hit.notif_id, 2);
                }
            }
        }
        self.queue.remove(hit.notif_id);
        self.update_surface(qh);
    }
}

impl ShmHandler for Whisper {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

delegate_compositor!(Whisper);
delegate_output!(Whisper);
delegate_shm!(Whisper);
delegate_seat!(Whisper);
delegate_pointer!(Whisper);
delegate_layer!(Whisper);
delegate_registry!(Whisper);

impl ProvidesRegistryState for Whisper {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }
    registry_handlers![OutputState, SeatState];
}

// ── Main ───────────────────────────────────────────────────────────

fn main() {
    // Query theme from compositor
    let theme = match kara_ipc::IpcClient::connect()
        .and_then(|mut c| c.request(&kara_ipc::Request::GetTheme))
    {
        Ok(kara_ipc::Response::Theme { colors }) => colors,
        _ => default_theme(),
    };

    // Start D-Bus service. Keep the returned connection around so we
    // can emit ActionInvoked / NotificationClosed signals from the main
    // thread; without those, notify-send clients invoked with `--wait`
    // hang forever.
    let (dbus_tx, dbus_rx) = mpsc::channel();
    let dbus_conn = dbus::spawn_dbus(dbus_tx).map(|h| h.conn);

    // Start popover socket listener. Best-effort — if the socket
    // fails to bind, popovers silently degrade but notifications
    // keep working.
    let (popover_tx, popover_rx) = mpsc::channel();
    let _popover_handle = popover_ipc::spawn(popover_tx);

    // Connect to Wayland
    let wayland_display = std::env::var("WAYLAND_DISPLAY").unwrap_or_default();
    let conn = match Connection::connect_to_env() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("kara-whisper: failed to connect to Wayland compositor: {e}");
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

    // Create layer surface
    let surface = compositor.create_surface(&qh);
    let layer = layer_shell.create_layer_surface(
        &qh,
        surface,
        Layer::Overlay,
        Some("kara-whisper"),
        None,
    );
    layer.set_anchor(Anchor::TOP | Anchor::RIGHT);
    // Margin (top, right, bottom, left). The top margin clears a
    // thick-ornamental bar (fantasy at 40 px); thinner bars just see
    // a slightly larger gap below them, which reads as breathing room
    // rather than a mistake. Right margin keeps the card off the
    // monitor edge and aligned with the bar's pill insets.
    layer.set_margin(52, 12, 0, 0);
    layer.set_keyboard_interactivity(KeyboardInteractivity::None);
    layer.set_size(NotificationUI::card_width(), 1);
    layer.commit();

    let pool = SlotPool::new(
        NotificationUI::card_width() as usize * 400 * 4, // room for several cards
        &shm,
    )
    .expect("failed to create SHM pool");

    let mut whisper = Whisper {
        registry_state: RegistryState::new(&globals),
        seat_state: SeatState::new(&globals, &qh),
        output_state: OutputState::new(&globals, &qh),
        shm,

        exit: false,
        first_configure: true,
        pool,
        width: NotificationUI::card_width(),
        height: 1,
        layer,

        queue: NotificationQueue::new(),
        ui: NotificationUI::new(theme),
        dbus_rx,
        popover_rx,
        surface_visible: false,

        pointer: None,
        pointer_pos: (-1.0, -1.0),
        hit_regions: Vec::new(),
        dbus_conn,
    };

    // Poll the compositor for the live theme every second so notifications
    // recolor when the user switches themes (kara-beautify writes the
    // generated include + SIGHUPs kara-gate). Cheap enough that we don't
    // need a subscription protocol — a unix-socket round-trip at 1 Hz is
    // well below the card redraw budget. Comparing `accent` is enough:
    // every palette swap moves accent, and set_theme is idempotent.
    let mut last_theme_poll = std::time::Instant::now();
    let mut last_accent = whisper.ui.accent();
    // Main loop: multiplex Wayland events + D-Bus channel
    loop {
        // Dispatch pending Wayland events
        event_queue.dispatch_pending(&mut whisper).ok();

        // Flush outgoing Wayland messages
        if event_queue.flush().is_err() {
            break;
        }

        // Try to read more Wayland events (non-blocking)
        if let Some(guard) = event_queue.prepare_read() {
            // read() is non-blocking if the fd has no data; it may return Err
            match guard.read() {
                Ok(_) => {
                    event_queue.dispatch_pending(&mut whisper).ok();
                }
                Err(_) => {}
            }
        }

        // Process D-Bus events
        let mut changed = false;
        while let Ok(event) = whisper.dbus_rx.try_recv() {
            match event {
                DbusEvent::Notify {
                    app_name,
                    app_icon,
                    summary,
                    body,
                    actions,
                    urgency,
                    expire_timeout,
                    reply,
                } => {
                    let u = match urgency {
                        0 => Urgency::Low,
                        2 => Urgency::Critical,
                        _ => Urgency::Normal,
                    };
                    let id = whisper.queue.add(app_name, app_icon, summary, body, actions, u, expire_timeout);
                    reply.send(id).ok();
                    changed = true;
                }
                DbusEvent::Close { id } => {
                    whisper.queue.remove(id);
                    changed = true;
                }
            }
        }

        // Process popover events from kara-beautify (and anything
        // else that talks to $XDG_RUNTIME_DIR/kara-whisper-popover.sock).
        // Reuses the notification queue with a short expire timeout —
        // dedicated popover rendering (center-anchored, bigger text)
        // can replace this later; for v1 they surface as top-right
        // cards same as D-Bus notifications.
        while let Ok(event) = whisper.popover_rx.try_recv() {
            match event {
                PopoverEvent::Show { text, duration_ms } => {
                    whisper.queue.add(
                        "kara".to_string(),
                        String::new(),
                        text,
                        String::new(),
                        Vec::new(),
                        Urgency::Low,
                        duration_ms as i32,
                    );
                    changed = true;
                }
                PopoverEvent::Hide => {
                    // No-op for v1 — popovers auto-expire via the
                    // normal tick() path.
                }
            }
        }

        // Periodic theme poll — redraws with the new palette when the
        // user switches themes. 1 s cadence keeps CPU negligible.
        if last_theme_poll.elapsed() >= std::time::Duration::from_secs(1) {
            last_theme_poll = std::time::Instant::now();
            if let Ok(mut client) = kara_ipc::IpcClient::connect() {
                if let Ok(kara_ipc::Response::Theme { colors }) =
                    client.request(&kara_ipc::Request::GetTheme)
                {
                    if colors.accent != last_accent {
                        last_accent = colors.accent;
                        whisper.ui.set_theme(colors);
                        changed = true;
                    }
                }
            }
        }

        // Tick expiration. Emit NotificationClosed(id, 1=expired) for
        // each so libnotify clients invoked with `--wait` complete
        // instead of waiting forever.
        let expired = whisper.queue.tick();
        if !expired.is_empty() {
            if let Some(conn) = whisper.dbus_conn.as_ref() {
                for id in &expired {
                    emit_notification_closed(conn, *id, 1);
                }
            }
            changed = true;
        }

        if changed {
            whisper.update_surface(&qh);
        }

        if whisper.exit {
            break;
        }

        // Brief sleep to avoid busy-wait
        std::thread::sleep(std::time::Duration::from_millis(16));
    }
}
