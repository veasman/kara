//! kara-veil — screen lock for kara.
//!
//! Uses the `ext-session-lock-v1` Wayland protocol. That means:
//! - The compositor stops rendering every normal surface while the lock
//!   is up (no windows, no bar, no wallpaper).
//! - All input is contained to our lock surfaces.
//! - If kara-veil crashes, the session stays locked — the compositor
//!   renders black frames until another lock client takes over. Real
//!   crash-resilient locking, not a layer-shell imitation.
//!
//! Architecture: bind the lock manager, request a lock, create one
//! `ext_session_lock_surface_v1` per connected output, wait for each
//! surface's configure, paint the themed lock UI on each, and on a
//! successful PAM auth call `unlock_and_destroy()`.

use std::collections::HashMap;
use std::time::Duration;

use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState},
    delegate_compositor, delegate_keyboard, delegate_output, delegate_registry,
    delegate_seat, delegate_shm,
    output::{OutputHandler, OutputState},
    reexports::calloop::{self, EventLoop, timer::Timer},
    reexports::calloop_wayland_source::WaylandSource,
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
    seat::{
        Capability, SeatHandler, SeatState,
        keyboard::{KeyEvent, KeyboardHandler, Keysym, Modifiers},
    },
    shm::{Shm, ShmHandler, slot::SlotPool},
};
use wayland_client::{
    Connection, Dispatch, Proxy, QueueHandle,
    globals::{GlobalList, registry_queue_init},
    protocol::{wl_keyboard, wl_output, wl_seat, wl_shm, wl_surface},
};
use wayland_protocols::ext::session_lock::v1::client::{
    ext_session_lock_manager_v1::ExtSessionLockManagerV1,
    ext_session_lock_surface_v1::{self, ExtSessionLockSurfaceV1},
    ext_session_lock_v1::{self, ExtSessionLockV1},
};
use zeroize::Zeroize;

mod pam;
mod ui;

use ui::LockUi;

#[link(name = "pam")]
unsafe extern "C" {}

/// Per-output lock surface + its buffer pool + current configured size.
struct LockedOutput {
    surface: wl_surface::WlSurface,
    lock_surface: ExtSessionLockSurfaceV1,
    pool: SlotPool,
    width: u32,
    height: u32,
    /// Until the compositor has sent us a configure, we can't attach
    /// a buffer — the protocol spec rejects buffer attach on a
    /// lock surface before the first configure ack.
    configured: bool,
    /// Primary output gets the login card rendered over the backdrop;
    /// all other outputs just show the overlay.
    is_primary: bool,
}

enum AuthOutcome {
    Success,
    Fail(String),
}

struct Veil {
    registry_state: RegistryState,
    seat_state: SeatState,
    output_state: OutputState,
    shm: Shm,
    compositor: CompositorState,

    exit: bool,
    ext_lock: ExtSessionLockV1,
    locked_ack: bool,
    /// wl_surface pointer value → LockedOutput. wl_surface is our
    /// identity key inside lock-surface configure/close callbacks.
    surfaces: HashMap<u32, LockedOutput>,

    keyboard: Option<wl_keyboard::WlKeyboard>,

    password: Vec<u8>,
    error: Option<String>,
    auth_busy: bool,

    ui: LockUi,
    username: String,
}

impl CompositorHandler for Veil {
    fn scale_factor_changed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: i32) {}
    fn transform_changed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: wl_output::Transform) {}
    fn frame(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: u32) {}
    fn surface_enter(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: &wl_output::WlOutput) {}
    fn surface_leave(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: &wl_output::WlOutput) {}
}

impl OutputHandler for Veil {
    fn output_state(&mut self) -> &mut OutputState { &mut self.output_state }
    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
}

impl SeatHandler for Veil {
    fn seat_state(&mut self) -> &mut SeatState { &mut self.seat_state }
    fn new_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}

    fn new_capability(
        &mut self,
        _conn: &Connection,
        qh: &QueueHandle<Self>,
        seat: wl_seat::WlSeat,
        capability: Capability,
    ) {
        if capability == Capability::Keyboard && self.keyboard.is_none() {
            if let Ok(k) = self.seat_state.get_keyboard(qh, &seat, None) {
                self.keyboard = Some(k);
            }
        }
    }

    fn remove_capability(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _seat: wl_seat::WlSeat,
        capability: Capability,
    ) {
        if capability == Capability::Keyboard {
            if let Some(k) = self.keyboard.take() {
                k.release();
            }
        }
    }
}

impl KeyboardHandler for Veil {
    fn enter(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: &wl_surface::WlSurface, _: u32, _: &[u32], _: &[Keysym]) {}
    fn leave(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: &wl_surface::WlSurface, _: u32) {}

    fn press_key(
        &mut self,
        _conn: &Connection,
        qh: &QueueHandle<Self>,
        _keyboard: &wl_keyboard::WlKeyboard,
        _serial: u32,
        event: KeyEvent,
    ) {
        if self.auth_busy {
            return;
        }
        match event.keysym {
            Keysym::Return | Keysym::KP_Enter => self.submit_password(qh),
            Keysym::BackSpace => {
                self.password.pop();
                self.error = None;
                self.redraw_all();
            }
            Keysym::Escape => {
                self.password.zeroize();
                self.password.clear();
                self.error = None;
                self.redraw_all();
            }
            _ => {
                if let Some(text) = event.utf8.as_deref() {
                    if !text.is_empty() && text.chars().all(|c| !c.is_control()) {
                        self.password.extend_from_slice(text.as_bytes());
                        self.error = None;
                        self.redraw_all();
                    }
                }
            }
        }
    }

    fn release_key(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: u32, _: KeyEvent) {}
    fn update_modifiers(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: u32, _: Modifiers, _: u32) {}
}

impl ShmHandler for Veil {
    fn shm_state(&mut self) -> &mut Shm { &mut self.shm }
}

// ── ext-session-lock dispatch ──────────────────────────────────────

impl Dispatch<ExtSessionLockManagerV1, ()> for Veil {
    fn event(
        _: &mut Self,
        _: &ExtSessionLockManagerV1,
        _: <ExtSessionLockManagerV1 as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ExtSessionLockV1, ()> for Veil {
    fn event(
        state: &mut Self,
        _: &ExtSessionLockV1,
        event: <ExtSessionLockV1 as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            ext_session_lock_v1::Event::Locked => {
                state.locked_ack = true;
                state.redraw_all();
            }
            ext_session_lock_v1::Event::Finished => {
                // Compositor refused / revoked the lock. Exit cleanly
                // — staying alive would spin on a dead lock handle.
                state.exit = true;
            }
            _ => {}
        }
    }
}

impl Dispatch<ExtSessionLockSurfaceV1, u32> for Veil {
    fn event(
        state: &mut Self,
        lock_surface: &ExtSessionLockSurfaceV1,
        event: <ExtSessionLockSurfaceV1 as wayland_client::Proxy>::Event,
        surface_id: &u32,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let ext_session_lock_surface_v1::Event::Configure { serial, width, height } = event {
            if let Some(slot) = state.surfaces.get_mut(surface_id) {
                slot.width = width;
                slot.height = height;
                slot.configured = true;
                lock_surface.ack_configure(serial);
                state.redraw_one(*surface_id);
            }
        }
    }
}

delegate_compositor!(Veil);
delegate_output!(Veil);
delegate_shm!(Veil);
delegate_seat!(Veil);
delegate_keyboard!(Veil);
delegate_registry!(Veil);

impl ProvidesRegistryState for Veil {
    fn registry(&mut self) -> &mut RegistryState { &mut self.registry_state }
    registry_handlers![OutputState, SeatState];
}

impl Veil {
    fn redraw_all(&mut self) {
        let ids: Vec<u32> = self.surfaces.keys().copied().collect();
        for id in ids {
            self.redraw_one(id);
        }
    }

    fn redraw_one(&mut self, surface_id: u32) {
        let Some(slot) = self.surfaces.get_mut(&surface_id) else { return };
        if !slot.configured || slot.width == 0 || slot.height == 0 {
            return;
        }

        let pixmap = self.ui.render(
            slot.width,
            slot.height,
            slot.is_primary,
            &self.username,
            self.password.len(),
            self.error.as_deref(),
            self.auth_busy,
        );
        let _ = slot.lock_surface;

        let stride = slot.width as i32 * 4;
        let Ok((buffer, canvas)) = slot.pool.create_buffer(
            slot.width as i32,
            slot.height as i32,
            stride,
            wl_shm::Format::Argb8888,
        ) else {
            return;
        };

        // tiny-skia Pixmap is RGBA8 premultiplied; Argb8888 on LE is
        // byte order B,G,R,A. Swap.
        for (src, dst) in pixmap.data().chunks_exact(4).zip(canvas.chunks_exact_mut(4)) {
            dst[0] = src[2];
            dst[1] = src[1];
            dst[2] = src[0];
            dst[3] = src[3];
        }

        slot.surface.damage_buffer(0, 0, slot.width as i32, slot.height as i32);
        let _ = buffer.attach_to(&slot.surface);
        slot.surface.commit();
    }

    fn submit_password(&mut self, _qh: &QueueHandle<Self>) {
        let password = std::mem::take(&mut self.password);
        self.auth_busy = true;
        self.redraw_all();

        let outcome = if pam::authenticate(&self.username, &password) {
            AuthOutcome::Success
        } else {
            AuthOutcome::Fail("incorrect password".into())
        };
        let mut password = password;
        password.zeroize();
        drop(password);

        self.auth_busy = false;
        match outcome {
            AuthOutcome::Success => {
                // Tell the compositor to end the lock and destroy
                // the lock object. The compositor will re-enable
                // normal rendering and restore pre-lock focus.
                self.ext_lock.unlock_and_destroy();
                self.exit = true;
            }
            AuthOutcome::Fail(msg) => {
                self.error = Some(msg);
                self.redraw_all();
            }
        }
    }
}

// ── Main ───────────────────────────────────────────────────────────

fn main() {
    let theme = match kara_ipc::IpcClient::connect()
        .and_then(|mut c| c.request(&kara_ipc::Request::GetTheme))
    {
        Ok(kara_ipc::Response::Theme { colors }) => colors,
        _ => default_theme(),
    };
    // Primary is whichever output the user marked `primary` in their
    // `monitors { }` config — NOT the first entry (which is leftmost
    // after the compositor's spatial sort) and NOT whatever's
    // currently focused. kara-veil's login card lands where the user
    // expects to type; for multi-monitor setups that's as often the
    // middle or right monitor as the leftmost. Falls back to the
    // first-reported output only if nothing is marked.
    let primary_name: Option<String> = match kara_ipc::IpcClient::connect()
        .and_then(|mut c| c.request(&kara_ipc::Request::GetOutputs))
    {
        Ok(kara_ipc::Response::Outputs { outputs }) => outputs
            .iter()
            .find(|o| o.primary)
            .or_else(|| outputs.first())
            .map(|o| o.name.clone()),
        _ => None,
    };

    let conn = match Connection::connect_to_env() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("kara-veil: can't reach the compositor ({e})");
            std::process::exit(1);
        }
    };
    let (globals, event_queue) = registry_queue_init(&conn).expect("registry");
    let qh = event_queue.handle();

    let compositor = CompositorState::bind(&globals, &qh).expect("wl_compositor");
    let shm = Shm::bind(&globals, &qh).expect("wl_shm");

    // Bind the session-lock manager global and immediately request a
    // lock. If the compositor doesn't advertise the global, fail
    // loudly — kara-veil only makes sense on a compositor that
    // actually supports the protocol.
    let lock_manager: ExtSessionLockManagerV1 = match globals.bind(&qh, 1..=1, ()) {
        Ok(m) => m,
        Err(e) => {
            eprintln!(
                "kara-veil: compositor doesn't support ext-session-lock-v1 ({e}). Upgrade kara-gate."
            );
            std::process::exit(1);
        }
    };
    let ext_lock = lock_manager.lock(&qh, ());

    let mut veil = Veil {
        registry_state: RegistryState::new(&globals),
        seat_state: SeatState::new(&globals, &qh),
        output_state: OutputState::new(&globals, &qh),
        shm,
        compositor,
        exit: false,
        ext_lock,
        locked_ack: false,
        surfaces: HashMap::new(),
        keyboard: None,
        password: Vec::new(),
        error: None,
        auth_busy: false,
        ui: LockUi::new(theme),
        username: resolve_username(),
    };

    // Pump the raw event_queue directly BEFORE moving it into
    // calloop's WaylandSource. SCTK's xdg_output handler only populates
    // `info.name` after a few round-trips' worth of events (wl_output
    // done → xdg_output name → xdg_output done), and the earlier
    // implementation tried to wait for these via
    // `event_loop.dispatch()` — but calloop doesn't pump a freshly
    // wrapped WaylandSource with the same eagerness as a direct
    // blocking_dispatch, so names never arrived, primary_idx fell back
    // to 0, and the login card rendered on the wrong monitor.
    //
    // Mirror the kara-glimpse pattern exactly: blocking_dispatch +
    // roundtrip + dispatch_pending in a short loop against the raw
    // event_queue, bail once every advertised output has a name or
    // 500 ms elapses.
    let mut event_queue = event_queue;
    let wait_deadline = std::time::Instant::now() + Duration::from_millis(500);
    loop {
        event_queue.blocking_dispatch(&mut veil).ok();
        conn.roundtrip().ok();
        event_queue.dispatch_pending(&mut veil).ok();
        let outs: Vec<_> = veil.output_state.outputs().collect();
        let have_all_names = !outs.is_empty()
            && outs.iter().all(|wl| {
                veil.output_state
                    .info(wl)
                    .and_then(|i| i.name)
                    .is_some()
            });
        if have_all_names || std::time::Instant::now() >= wait_deadline {
            break;
        }
    }

    // First pass: figure out which output is primary. Prefer a name
    // match against the compositor's primary announcement; fall back
    // to idx=0 only if the name lookup genuinely fails (e.g. IPC
    // unreachable, or xdg_output never fired within 500 ms).
    let wl_outputs: Vec<wl_output::WlOutput> = veil.output_state.outputs().collect();
    let primary_idx = primary_name
        .as_deref()
        .and_then(|pn| {
            wl_outputs.iter().position(|wl| {
                veil.output_state
                    .info(wl)
                    .and_then(|i| i.name)
                    .as_deref()
                    == Some(pn)
            })
        })
        .unwrap_or(0);
    // Write diagnostics to /tmp/kara-veil.log so the user can triage
    // "lock on wrong monitor" without Ctrl+Alt+F<N> to whatever TTY
    // kara-gate was launched from — the lock keybind blanks every
    // output and whatever kara-veil prints to stderr is inaccessible
    // while the lock is up.
    let wl_names: Vec<String> = wl_outputs
        .iter()
        .map(|wl| {
            veil.output_state
                .info(wl)
                .and_then(|i| i.name)
                .unwrap_or_else(|| "(no-name)".into())
        })
        .collect();
    log_veil(&format!(
        "kara-veil start: IPC primary_name={:?}, {} wl_outputs=[{}], primary_idx={} → {:?}",
        primary_name,
        wl_outputs.len(),
        wl_names.join(", "),
        primary_idx,
        wl_names.get(primary_idx).cloned().unwrap_or_default(),
    ));

    // Second pass: create surfaces.
    for (idx, wl) in wl_outputs.iter().enumerate() {
        let surface = veil.compositor.create_surface(&qh);
        let surface_id = surface.id().protocol_id();
        let is_primary = idx == primary_idx;
        let lock_surface = veil.ext_lock.get_lock_surface(&surface, wl, &qh, surface_id);

        // Pool sized for a 4K surface up front — growing mid-frame
        // risks invalidating already-attached buffers.
        let pool = SlotPool::new(3840 * 2160 * 4, &veil.shm).expect("SHM pool");

        veil.surfaces.insert(
            surface_id,
            LockedOutput {
                surface,
                lock_surface,
                pool,
                width: 0,
                height: 0,
                configured: false,
                is_primary,
            },
        );
    }

    // Now hand the event_queue to calloop for the long-lived main loop.
    // Everything that needed early output metadata already ran above
    // against the raw queue; from here on we want calloop's timer
    // integration for the 1 Hz clock tick.
    let mut event_loop: EventLoop<'static, Veil> =
        EventLoop::try_new().expect("calloop");
    let loop_handle = event_loop.handle();
    WaylandSource::new(conn.clone(), event_queue)
        .insert(loop_handle.clone())
        .expect("wayland source");

    // 500 ms redraw tick. Two jobs: flip `HH:MM` on the minute without
    // user input, AND blink the password-field caret at 1 Hz (on for
    // 500 ms, off for 500 ms) so the lock screen visibly hums and the
    // user can tell the field is live even before they've typed. Two
    // redraws/sec on a mostly-static pixmap is cheap; tiny-skia plus
    // the per-output SlotPool handles it with headroom.
    loop_handle
        .insert_source(
            Timer::from_duration(Duration::from_millis(500)),
            |_, _, veil: &mut Veil| {
                veil.redraw_all();
                calloop::timer::TimeoutAction::ToDuration(Duration::from_millis(500))
            },
        )
        .ok();

    while !veil.exit {
        if event_loop.dispatch(None, &mut veil).is_err() {
            break;
        }
    }

    veil.password.zeroize();
}

fn resolve_username() -> String {
    if let Ok(u) = std::env::var("USER") {
        if !u.is_empty() { return u; }
    }
    if let Ok(u) = std::env::var("LOGNAME") {
        if !u.is_empty() { return u; }
    }
    let uid = unsafe { libc_getuid() };
    if let Ok(buf) = std::fs::read_to_string("/etc/passwd") {
        let needle = format!(":{uid}:");
        for line in buf.lines() {
            if line.contains(&needle) {
                if let Some(name) = line.split(':').next() {
                    if !name.is_empty() {
                        return name.to_string();
                    }
                }
            }
        }
    }
    "user".to_string()
}

#[inline(never)]
unsafe fn libc_getuid() -> u32 {
    unsafe extern "C" {
        fn getuid() -> u32;
    }
    unsafe { getuid() }
}

/// Append a diagnostic line to /tmp/kara-veil.log. kara-veil is spawned
/// by kara-gate's lock action via a detached child, so stderr is buried
/// on whatever TTY kara-gate was started from — and the lock keybind
/// blanks every output while it's up, so the TTY is unreachable anyway.
/// A file log lets the user just `cat /tmp/kara-veil.log` after a test
/// run to see what primary was picked, what outputs SCTK saw, etc.
fn log_veil(line: &str) {
    use std::io::Write;
    let _ = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open("/tmp/kara-veil.log")
        .and_then(|mut f| writeln!(f, "{line}"));
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
        bar_height: None,
        bar_background: None,
        bar_background_alpha: None,
        border_tile_path: None,
        border_px: Some(2),
        border_radius: Some(10),
        font_family: None,
    }
}

// Suppress "unused global list import" — GlobalList is used by the
// bind call via traits but the compiler's unused-imports check wants
// an explicit mention.
const _: Option<GlobalList> = None;
