//! kara-beautify theme picker mode.
//!
//! Invoked as `kara-summon --mode themes`. Shows a two-section
//! keyboard-driven picker:
//!
//!   ┌──────────────────────────────────┐
//!   │ THEME    default  fantasy        │
//!   ├──────────────────────────────────┤
//!   │ VARIANT  vague  gruvbox  nord    │
//!   ├──────────────────────────────────┤
//!   │ [Tab] section  [↑↓/jk/C-n C-p]   │
//!   │ navigate  [Enter] apply  [Esc]   │
//!   │ cancel                           │
//!   └──────────────────────────────────┘
//!
//! Keybinds (matches the plan's B9 spec):
//!   Tab / Shift+Tab     — next/previous section (Theme ↔ Variant)
//!   j / k / ↓ / ↑       — next/previous item within the current section
//!   Ctrl+n / Ctrl+p     — same (vim-alt)
//!   Enter               — commit the current (theme, variant) selection
//!                         to kara-beautify via SetTheme IPC
//!   Escape / Ctrl+c     — cancel, close without applying
//!
//! Wallpaper carousel (B9b) and live-preview-on-navigation (B9c)
//! come in follow-up passes. This module ships the theme picker
//! skeleton (B9a) which unlocks keyboard-driven theme switching —
//! the primary UX promise of variants.

use kara_ipc::ThemeColors;
use kara_ui::canvas::{color_from_u32, fill_rounded_rect, stroke_rounded_rect};
use kara_ui::TextRenderer;
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
use tiny_skia::Pixmap;
use wayland_client::{
    globals::registry_queue_init,
    protocol::{wl_keyboard, wl_output, wl_seat, wl_shm, wl_surface},
    Connection, QueueHandle,
};

use crate::beautify_ipc::{self, Request, Response, ThemeEntry, VariantEntry};

const WIDTH: u32 = 560;
const HEIGHT: u32 = 280;
const PADDING: i32 = 18;
const SECTION_LABEL_WIDTH: i32 = 80;
const CHIP_HEIGHT: i32 = 32;
const CHIP_GAP: i32 = 8;
const BORDER_RADIUS: f32 = 12.0;

/// Which horizontal section of the picker has keyboard focus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Focus {
    Theme,
    Variant,
}

impl Focus {
    fn next(self) -> Self {
        match self {
            Focus::Theme => Focus::Variant,
            Focus::Variant => Focus::Theme,
        }
    }
    fn prev(self) -> Self {
        self.next()
    }
}

/// Per-theme group: the theme entry plus its variants, fetched
/// lazily when the user focuses that theme for the first time.
struct ThemeGroup {
    entry: ThemeEntry,
    variants: Option<Vec<VariantEntry>>,
    default_variant: Option<String>,
}

pub fn run(theme: ThemeColors) {
    // Fetch themes from the kara-beautify daemon. If the daemon
    // isn't running, print a clear error and exit instead of
    // showing an empty picker.
    let themes = match beautify_ipc::try_request(&Request::ListThemes) {
        Some(Response::Themes { themes }) if !themes.is_empty() => themes,
        Some(Response::Themes { .. }) => {
            eprintln!(
                "kara-summon: no themes installed. Run `kara-beautify list` \
                 to see the search paths, or install the kara package so \
                 /usr/share/kara/themes is populated."
            );
            std::process::exit(2);
        }
        Some(Response::Error { message }) => {
            eprintln!("kara-summon: daemon returned error: {message}");
            std::process::exit(1);
        }
        _ => {
            eprintln!(
                "kara-summon: kara-beautify daemon isn't running \
                 (socket: {}). Start it with `kara-beautify daemon &` or \
                 add it to your autostart.",
                beautify_ipc::socket_path().display()
            );
            std::process::exit(1);
        }
    };

    // Also fetch the current state so the picker opens with the
    // live theme highlighted.
    let (current_theme, current_variant) = match beautify_ipc::try_request(&Request::GetState) {
        Some(Response::State { theme, variant, .. }) => (theme, variant),
        _ => (None, None),
    };

    // Build groups and pre-fetch the currently-active theme's
    // variants so the picker opens with both sections populated.
    let mut groups: Vec<ThemeGroup> = themes
        .into_iter()
        .map(|entry| ThemeGroup {
            entry,
            variants: None,
            default_variant: None,
        })
        .collect();

    // Find which group is the current theme and pre-fetch its variants.
    let initial_theme_idx = current_theme
        .as_deref()
        .and_then(|name| groups.iter().position(|g| g.entry.name == name))
        .unwrap_or(0);
    fetch_variants_for(&mut groups, initial_theme_idx);

    let initial_variant_idx = if let Some(ref vs) = groups[initial_theme_idx].variants {
        current_variant
            .as_deref()
            .and_then(|v| vs.iter().position(|ve| ve.name == v))
            .unwrap_or(0)
    } else {
        0
    };

    let conn = match Connection::connect_to_env() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("kara-summon: failed to connect to Wayland: {e}");
            std::process::exit(1);
        }
    };
    let (globals, mut event_queue) = registry_queue_init(&conn).expect("failed to init registry");
    let qh = event_queue.handle();

    let compositor = CompositorState::bind(&globals, &qh).expect("wl_compositor not available");
    let layer_shell = LayerShell::bind(&globals, &qh).expect("layer shell not available");
    let shm = Shm::bind(&globals, &qh).expect("wl_shm not available");

    let surface = compositor.create_surface(&qh);
    let layer =
        layer_shell.create_layer_surface(&qh, surface, Layer::Overlay, Some("kara-picker"), None);
    layer.set_keyboard_interactivity(KeyboardInteractivity::Exclusive);
    layer.set_size(WIDTH, HEIGHT);
    layer.commit();

    let pool = SlotPool::new(WIDTH as usize * HEIGHT as usize * 4, &shm)
        .expect("failed to create SHM pool");

    let mut picker = Picker {
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

        theme_colors: theme,
        text: TextRenderer::new_with_font("sans-serif", 13.0),

        groups,
        theme_idx: initial_theme_idx,
        variant_idx: initial_variant_idx,
        focus: Focus::Theme,
        ctrl_held: false,

        commit_on_exit: false,
    };

    loop {
        event_queue.blocking_dispatch(&mut picker).unwrap();
        if picker.exit {
            break;
        }
    }

    if picker.commit_on_exit {
        if let Some(group) = picker.groups.get(picker.theme_idx) {
            let variant = group
                .variants
                .as_ref()
                .and_then(|vs| vs.get(picker.variant_idx))
                .map(|v| v.name.clone());
            let _ = beautify_ipc::try_request(&Request::SetTheme {
                name: group.entry.name.clone(),
                variant,
                wallpaper: None,
            });
        }
    }
}

/// Fetch a theme's variants via IPC and cache them in the group.
/// No-op if variants are already loaded.
fn fetch_variants_for(groups: &mut [ThemeGroup], idx: usize) {
    let Some(group) = groups.get_mut(idx) else {
        return;
    };
    if group.variants.is_some() {
        return;
    }
    match beautify_ipc::try_request(&Request::ListVariants {
        theme: group.entry.name.clone(),
    }) {
        Some(Response::Variants {
            default_variant,
            variants,
            ..
        }) => {
            group.default_variant = default_variant;
            group.variants = Some(variants);
        }
        _ => {
            // Failure modes: daemon went away between ListThemes
            // and this call, or the theme no longer parses. Store
            // an empty list so we don't re-query on every keypress.
            group.variants = Some(Vec::new());
        }
    }
}

struct Picker {
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

    theme_colors: ThemeColors,
    text: TextRenderer,

    groups: Vec<ThemeGroup>,
    theme_idx: usize,
    variant_idx: usize,
    focus: Focus,
    ctrl_held: bool,

    commit_on_exit: bool,
}

impl Picker {
    fn active_variants(&self) -> &[VariantEntry] {
        self.groups
            .get(self.theme_idx)
            .and_then(|g| g.variants.as_deref())
            .unwrap_or(&[])
    }

    fn move_selection(&mut self, delta: isize) {
        match self.focus {
            Focus::Theme => {
                if self.groups.is_empty() {
                    return;
                }
                let new_idx = wrap(self.theme_idx as isize + delta, self.groups.len());
                if new_idx != self.theme_idx {
                    self.theme_idx = new_idx;
                    // Load the new theme's variants on first focus.
                    fetch_variants_for(&mut self.groups, self.theme_idx);
                    self.variant_idx = 0;
                }
            }
            Focus::Variant => {
                let n = self.active_variants().len();
                if n == 0 {
                    return;
                }
                self.variant_idx = wrap(self.variant_idx as isize + delta, n);
            }
        }
    }

    fn draw(&mut self) {
        let w = self.width;
        let h = self.height;
        let mut pixmap = match Pixmap::new(w, h) {
            Some(p) => p,
            None => return,
        };
        // Clone the palette up front so we don't hold an immutable
        // borrow on self while the draw helpers take `&mut self`.
        let t = self.theme_colors.clone();

        // Background
        fill_rounded_rect(
            &mut pixmap,
            0.0, 0.0, w as f32, h as f32,
            BORDER_RADIUS, color_from_u32(t.surface),
        );
        stroke_rounded_rect(
            &mut pixmap,
            0.5, 0.5, w as f32 - 1.0, h as f32 - 1.0,
            BORDER_RADIUS, color_from_u32(t.border), 1.0,
        );

        // THEME row
        let theme_row_y = PADDING;
        self.draw_section_label(&mut pixmap, "THEME", theme_row_y);
        let theme_names: Vec<String> = self
            .groups
            .iter()
            .map(|g| {
                g.entry
                    .display_name
                    .clone()
                    .unwrap_or_else(|| g.entry.name.clone())
            })
            .collect();
        let theme_refs: Vec<&str> = theme_names.iter().map(|s| s.as_str()).collect();
        let theme_focused = self.focus == Focus::Theme;
        let theme_selected = self.theme_idx;
        self.draw_chip_row(
            &mut pixmap,
            &theme_refs,
            theme_selected,
            theme_focused,
            theme_row_y,
        );

        // Separator
        let sep_y = theme_row_y + CHIP_HEIGHT + PADDING;
        fill_rounded_rect(
            &mut pixmap,
            PADDING as f32,
            sep_y as f32,
            (w as i32 - PADDING * 2) as f32,
            1.0,
            0.0,
            color_from_u32(t.border),
        );

        // VARIANT row
        let variant_row_y = sep_y + PADDING;
        self.draw_section_label(&mut pixmap, "VARIANT", variant_row_y);

        // Clone variant names out of self so we don't hold a borrow
        // while calling &mut self methods below.
        let variant_names: Vec<String> = self
            .active_variants()
            .iter()
            .map(|v| v.display_name.clone().unwrap_or_else(|| v.name.clone()))
            .collect();
        if variant_names.is_empty() {
            let label_x = PADDING + SECTION_LABEL_WIDTH;
            let label_y = self
                .text
                .center_y_offset(variant_row_y as f32 + CHIP_HEIGHT as f32 / 2.0);
            self.text.draw(
                &mut pixmap,
                "(no variants)",
                label_x as f32,
                label_y,
                t.text_muted,
            );
        } else {
            let variant_refs: Vec<&str> =
                variant_names.iter().map(|s| s.as_str()).collect();
            let variant_focused = self.focus == Focus::Variant;
            let variant_selected = self.variant_idx;
            self.draw_chip_row(
                &mut pixmap,
                &variant_refs,
                variant_selected,
                variant_focused,
                variant_row_y,
            );
        }

        // Footer: keybind hints
        let hint = "[Tab] section   [← →] navigate   [Enter] apply   [Esc] cancel";
        let hint_y = (h as i32 - PADDING - 4) as f32;
        self.text
            .draw(&mut pixmap, hint, PADDING as f32, hint_y, t.text_muted);

        // Commit to buffer
        let stride = w as i32 * 4;
        let (buffer, canvas) = self
            .pool
            .create_buffer(w as i32, h as i32, stride, wl_shm::Format::Argb8888)
            .expect("create buffer");
        let src = pixmap.data();
        for (dst, src) in canvas.chunks_exact_mut(4).zip(src.chunks_exact(4)) {
            dst[0] = src[2]; // B
            dst[1] = src[1]; // G
            dst[2] = src[0]; // R
            dst[3] = src[3]; // A
        }
        self.layer
            .wl_surface()
            .damage_buffer(0, 0, w as i32, h as i32);
        buffer
            .attach_to(self.layer.wl_surface())
            .expect("buffer attach");
        self.layer.commit();
    }

    fn draw_section_label(&mut self, pixmap: &mut Pixmap, label: &str, row_y: i32) {
        let t = &self.theme_colors;
        let label_y = self
            .text
            .center_y_offset(row_y as f32 + CHIP_HEIGHT as f32 / 2.0);
        self.text.draw(
            pixmap,
            label,
            PADDING as f32,
            label_y,
            t.text_muted,
        );
    }

    fn draw_chip_row(
        &mut self,
        pixmap: &mut Pixmap,
        items: &[&str],
        selected: usize,
        section_focused: bool,
        row_y: i32,
    ) {
        let t = &self.theme_colors;
        let start_x = PADDING + SECTION_LABEL_WIDTH;
        let mut cursor_x = start_x;

        for (i, label) in items.iter().enumerate() {
            let text_w = self.text.measure(label) as i32;
            let chip_w = text_w + 20; // 10px horizontal padding on each side
            let is_sel = i == selected;

            if is_sel {
                let fill = if section_focused {
                    t.accent_soft
                } else {
                    t.overlay_or_surface()
                };
                fill_rounded_rect(
                    pixmap,
                    cursor_x as f32,
                    row_y as f32,
                    chip_w as f32,
                    CHIP_HEIGHT as f32,
                    8.0,
                    color_from_u32(fill),
                );
                if section_focused {
                    stroke_rounded_rect(
                        pixmap,
                        cursor_x as f32 + 0.5,
                        row_y as f32 + 0.5,
                        chip_w as f32 - 1.0,
                        CHIP_HEIGHT as f32 - 1.0,
                        8.0,
                        color_from_u32(t.accent),
                        1.5,
                    );
                }
            }

            let text_color = if is_sel { t.text } else { t.text_muted };
            let text_x = cursor_x + 10;
            let text_y = self
                .text
                .center_y_offset(row_y as f32 + CHIP_HEIGHT as f32 / 2.0);
            self.text.draw(pixmap, label, text_x as f32, text_y, text_color);

            cursor_x += chip_w + CHIP_GAP;

            if cursor_x > self.width as i32 - PADDING {
                // Out of horizontal room — scroll would be the right
                // fix but for now the selection marker communicates
                // which chip is active even when off-screen.
                break;
            }
        }
    }
}

/// ThemeColors doesn't expose `overlay` in the public struct (it's
/// only `bg/surface/text/text_muted/accent/accent_soft/border`).
/// Fall back to surface for the "focused section, not-focused
/// selected chip" fill so the look degrades gracefully.
trait OverlayOrSurface {
    fn overlay_or_surface(&self) -> u32;
}
impl OverlayOrSurface for ThemeColors {
    fn overlay_or_surface(&self) -> u32 {
        self.surface
    }
}

fn wrap(value: isize, modulus: usize) -> usize {
    if modulus == 0 {
        return 0;
    }
    let m = modulus as isize;
    (((value % m) + m) % m) as usize
}

// ─── SCTK trait impls ──────────────────────────────────────────────

impl CompositorHandler for Picker {
    fn scale_factor_changed(
        &mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: i32,
    ) {
    }
    fn transform_changed(
        &mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface,
        _: wl_output::Transform,
    ) {
    }
    fn frame(
        &mut self, _: &Connection, _qh: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: u32,
    ) {
        self.draw();
    }
    fn surface_enter(
        &mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface,
        _: &wl_output::WlOutput,
    ) {
    }
    fn surface_leave(
        &mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface,
        _: &wl_output::WlOutput,
    ) {
    }
}

impl OutputHandler for Picker {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }
    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
}

impl LayerShellHandler for Picker {
    fn closed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &LayerSurface) {
        self.exit = true;
    }
    fn configure(
        &mut self, _: &Connection, _qh: &QueueHandle<Self>, _: &LayerSurface,
        configure: LayerSurfaceConfigure, _: u32,
    ) {
        if configure.new_size.0 > 0 && configure.new_size.1 > 0 {
            self.width = configure.new_size.0;
            self.height = configure.new_size.1;
        }
        if self.first_configure {
            self.first_configure = false;
            self.draw();
        }
    }
}

impl SeatHandler for Picker {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seat_state
    }
    fn new_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
    fn new_capability(
        &mut self, _: &Connection, qh: &QueueHandle<Self>, seat: wl_seat::WlSeat,
        capability: Capability,
    ) {
        if capability == Capability::Keyboard && self.keyboard.is_none() {
            let kb = self
                .seat_state
                .get_keyboard(qh, &seat, None)
                .expect("failed to get keyboard");
            self.keyboard = Some(kb);
        }
    }
    fn remove_capability(
        &mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat,
        capability: Capability,
    ) {
        if capability == Capability::Keyboard {
            if let Some(kb) = self.keyboard.take() {
                kb.release();
            }
        }
    }
    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
}

impl KeyboardHandler for Picker {
    fn enter(
        &mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard,
        _: &wl_surface::WlSurface, _: u32, _: &[u32], _: &[Keysym],
    ) {
    }
    fn leave(
        &mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard,
        _: &wl_surface::WlSurface, _: u32,
    ) {
    }

    fn press_key(
        &mut self, _: &Connection, _qh: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: u32,
        event: KeyEvent,
    ) {
        // ─── Section navigation ───────────────────────────────
        if event.keysym == Keysym::Tab {
            self.focus = self.focus.next();
            self.draw();
            return;
        }
        if event.keysym == Keysym::ISO_Left_Tab {
            self.focus = self.focus.prev();
            self.draw();
            return;
        }

        // ─── Exit ─────────────────────────────────────────────
        if event.keysym == Keysym::Escape
            || (self.ctrl_held && event.keysym == Keysym::c)
        {
            self.exit = true;
            return;
        }

        // ─── Commit ───────────────────────────────────────────
        if event.keysym == Keysym::Return || event.keysym == Keysym::KP_Enter {
            self.commit_on_exit = true;
            self.exit = true;
            return;
        }

        // ─── Intra-section navigation ────────────────────────
        // Horizontal primary (h/l/←/→) because chips are laid out
        // in a row; vertical aliases (j/k/↑/↓) map to the same
        // thing so users don't have to think about direction.
        let prev = matches!(
            event.keysym,
            Keysym::h | Keysym::k | Keysym::Left | Keysym::Up
        ) || (self.ctrl_held && event.keysym == Keysym::p);
        let next = matches!(
            event.keysym,
            Keysym::l | Keysym::j | Keysym::Right | Keysym::Down
        ) || (self.ctrl_held && event.keysym == Keysym::n);

        if prev {
            self.move_selection(-1);
            self.draw();
            return;
        }
        if next {
            self.move_selection(1);
            self.draw();
            return;
        }
    }

    fn release_key(
        &mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: u32,
        _: KeyEvent,
    ) {
    }
    fn update_modifiers(
        &mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: u32,
        modifiers: Modifiers, _: u32,
    ) {
        self.ctrl_held = modifiers.ctrl;
    }
}

impl ShmHandler for Picker {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

delegate_compositor!(Picker);
delegate_output!(Picker);
delegate_shm!(Picker);
delegate_seat!(Picker);
delegate_keyboard!(Picker);
delegate_layer!(Picker);
delegate_registry!(Picker);

impl ProvidesRegistryState for Picker {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }
    registry_handlers![OutputState, SeatState];
}
