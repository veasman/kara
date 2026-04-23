use smithay::backend::input::{
    AbsolutePositionEvent, Axis, ButtonState, Event, InputEvent, KeyState, KeyboardKeyEvent,
    PointerAxisEvent, PointerButtonEvent, PointerMotionEvent,
};
use smithay::desktop::{WindowSurfaceType, layer_map_for_output};
use smithay::input::keyboard::{FilterResult, ModifiersState};
use smithay::input::pointer::{AxisFrame, ButtonEvent, MotionEvent};
use smithay::utils::SERIAL_COUNTER;
use smithay::wayland::shell::wlr_layer::Layer as WlrLayer;

use crate::actions::Action;
use crate::state::Gate;

#[derive(Debug, Clone)]
pub struct Keybind {
    pub mods: ModMask,
    pub sym: u32,
    pub action: Action,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModMask {
    pub logo: bool,
    pub shift: bool,
    pub ctrl: bool,
    pub alt: bool,
}

impl ModMask {
    pub fn matches(&self, mods: &ModifiersState) -> bool {
        self.logo == mods.logo
            && self.shift == mods.shift
            && self.ctrl == mods.ctrl
            && self.alt == mods.alt
    }
}

/// Convert config keybinds to compositor keybinds.
pub fn keybinds_from_config(config: &kara_config::Config) -> Vec<Keybind> {
    config
        .keybinds
        .iter()
        .map(|kb| Keybind {
            mods: ModMask {
                logo: kb.mods.logo,
                shift: kb.mods.shift,
                ctrl: kb.mods.ctrl,
                alt: kb.mods.alt,
            },
            sym: kb.keysym,
            action: convert_action(&kb.action),
        })
        .collect()
}

fn convert_action(action: &kara_config::BindAction) -> Action {
    use kara_config::BindAction;
    match action {
        BindAction::Spawn(name) => Action::Spawn(name.clone()),
        BindAction::Exec(cmd) => Action::SpawnRaw(cmd.clone()),
        BindAction::Scratchpad(name) => Action::ToggleScratchpad(name.clone()),
        BindAction::FocusNext => Action::FocusNext,
        BindAction::FocusPrev => Action::FocusPrev,
        BindAction::FocusMonitorPrev => Action::FocusMonitorPrev,
        BindAction::FocusMonitorNext => Action::FocusMonitorNext,
        BindAction::SendMonitorPrev => Action::SendMonitorPrev,
        BindAction::SendMonitorNext => Action::SendMonitorNext,
        BindAction::DecreaseMfact => Action::DecreaseMfact,
        BindAction::IncreaseMfact => Action::IncreaseMfact,
        BindAction::ZoomMaster => Action::ZoomMaster,
        BindAction::Monocle => Action::ToggleMonocle,
        BindAction::Fullscreen => Action::ToggleFullscreen,
        BindAction::ToggleFloat => Action::ToggleFloat,
        BindAction::ToggleSync => Action::ToggleSync,
        BindAction::KillClient => Action::KillClient,
        BindAction::Reload => Action::Reload,
        BindAction::Quit => Action::Quit,
        BindAction::ShowKeybinds => Action::ShowKeybinds,
        BindAction::Lock => Action::Lock,
        BindAction::ViewWs(idx) => Action::ViewWs(*idx),
        BindAction::SendWs(idx) => Action::SendWs(*idx),
    }
}

impl Gate {
    /// Return true if the currently-focused keyboard surface
    /// belongs to a layer surface with
    /// `keyboard_interactivity = exclusive`. Used by the keybind
    /// dispatcher to suppress global keybinds while an exclusive
    /// layer (picker, glimpse capture, summon launcher, etc.)
    /// holds focus — those surfaces expect to consume every key.
    /// Also called from `apply_focus` so the focus-recompute
    /// paths don't yank keyboard focus out from under an open
    /// exclusive layer.
    pub(crate) fn keyboard_focus_is_exclusive_layer(&self) -> bool {
        use smithay::desktop::layer_map_for_output;
        use smithay::wayland::shell::wlr_layer::KeyboardInteractivity;

        let Some(keyboard) = self.seat.get_keyboard() else {
            return false;
        };
        let Some(focus) = keyboard.current_focus() else {
            return false;
        };

        // Walk every output's layer map and check whether the
        // focused wl_surface matches any layer's root surface.
        // If it does, read that layer's cached
        // `keyboard_interactivity` to decide whether keybinds
        // should defer.
        for out in &self.outputs {
            let map = layer_map_for_output(&out.output);
            for layer in map.layers() {
                if layer.wl_surface() == &focus {
                    let interactivity = smithay::wayland::compositor::with_states(
                        layer.wl_surface(),
                        |states| {
                            states
                                .cached_state
                                .get::<smithay::wayland::shell::wlr_layer::LayerSurfaceCachedState>()
                                .current()
                                .keyboard_interactivity
                        },
                    );
                    return matches!(interactivity, KeyboardInteractivity::Exclusive);
                }
            }
        }

        false
    }

    pub fn handle_input_event<B: smithay::backend::input::InputBackend>(
        &mut self,
        event: InputEvent<B>,
    ) {
        match event {
            InputEvent::Keyboard { event } => self.handle_keyboard::<B>(event),
            InputEvent::PointerMotion { event } => self.handle_pointer_motion_relative::<B>(event),
            InputEvent::PointerMotionAbsolute { event } => self.handle_pointer_motion_absolute::<B>(event),
            InputEvent::PointerButton { event } => self.handle_pointer_button::<B>(event),
            InputEvent::PointerAxis { event } => self.handle_pointer_axis::<B>(event),
            _ => {}
        }
    }

    fn handle_keyboard<B: smithay::backend::input::InputBackend>(
        &mut self,
        event: B::KeyboardKeyEvent,
    ) {
        let serial = SERIAL_COUNTER.next_serial();
        let time = Event::time_msec(&event);
        let keycode = event.key_code();
        let key_state = event.state();

        let keyboard = self.seat.get_keyboard().unwrap();

        // Arc clone — pointer copy, no allocation
        let keybinds = self.keybinds.clone();
        let pressed = key_state == KeyState::Pressed;

        // When an exclusive-keyboard layer surface (kara-summon's
        // picker, kara-glimpse's capture overlay, etc.) holds
        // focus, the wlr-layer-shell protocol says the surface
        // consumes ALL keyboard input. Compositor-level keybinds
        // fire from THIS function regardless of focus, so without
        // this guard, pressing mod+j inside a picker still
        // triggers focus_next — stealing focus back to a window
        // behind the picker.
        //
        // Same applies while ext-session-lock-v1 is active — the
        // lock client must receive every keystroke, and compositor
        // keybinds (focus, workspace nav, kill_client, quit) would
        // be a security hole while the screen's locked. Gate on
        // both.
        let skip_keybinds =
            self.keyboard_focus_is_exclusive_layer() || self.session_lock.is_some();

        let action = keyboard.input(
            self,
            keycode,
            key_state,
            serial,
            time,
            |_state, mods, handle| {
                if pressed && !skip_keybinds {
                    let raw_syms = handle.raw_syms();
                    for bind in keybinds.iter() {
                        if raw_syms.iter().any(|sym| bind.sym == sym.raw()) && bind.mods.matches(mods) {
                            return FilterResult::Intercept(bind.action.clone());
                        }
                    }
                }
                FilterResult::Forward
            },
        );

        let was_bound = action.is_some();
        if let Some(action) = action {
            self.dispatch_action(action);
        }

        // Dismiss keybind overlay on any non-bound keypress
        if pressed && self.keybind_overlay_visible && !was_bound {
            self.keybind_overlay_visible = false;
            self.layout_dirty = true;
        }
    }

    /// Find the surface under the pointer, checking layer surfaces (overlay/top) first,
    /// then falling back to windows in the space.
    fn surface_under_pointer(
        &self,
        pos: smithay::utils::Point<f64, smithay::utils::Logical>,
    ) -> Option<(
        smithay::reexports::wayland_server::protocol::wl_surface::WlSurface,
        smithay::utils::Point<f64, smithay::utils::Logical>,
    )> {
        // While the session is locked, pointer events route exclusively
        // to the lock surface covering whichever output the cursor is
        // over. Never fall through to the window/layer search below —
        // that would leak pointer events to the covered desktop.
        if let Some(lock) = self.session_lock.as_ref() {
            for out in &self.outputs {
                let (w, h) = out.size;
                let x0 = out.location.x as f64;
                let y0 = out.location.y as f64;
                if pos.x < x0 || pos.y < y0 || pos.x >= x0 + w as f64 || pos.y >= y0 + h as f64 {
                    continue;
                }
                if let Some(surf) = lock.surfaces.get(&out.output.name()) {
                    return Some((surf.wl_surface().clone(), pos));
                }
                // Cursor is on an output the lock client hasn't painted
                // yet — still refuse to fall through to the desktop.
                return None;
            }
            return None;
        }

        // Check overlay/top layer surfaces first (kara-summon, kara-glimpse, etc.)
        if let Some(output) = self.outputs.get(self.focused_output) {
            let map = layer_map_for_output(&output.output);
            // Convert to output-local coordinates for layer map queries
            let out_loc = output.location;
            let local_pos: smithay::utils::Point<f64, smithay::utils::Logical> = (
                pos.x - out_loc.x as f64,
                pos.y - out_loc.y as f64,
            ).into();

            // Check Overlay then Top layers — iterate all layers and do manual hit-test
            for layer_type in &[WlrLayer::Overlay, WlrLayer::Top] {
                for layer in map.layers_on(*layer_type).rev() {
                    let geo = map.layer_geometry(layer);
                    if let Some(geo) = geo {
                        let geo_f = geo.to_f64();
                        if geo_f.contains(local_pos) {
                            let surface_local = (
                                local_pos.x - geo.loc.x as f64,
                                local_pos.y - geo.loc.y as f64,
                            );
                            // Try surface_under first (handles subsurfaces/popups)
                            if let Some((surface, surface_loc)) =
                                layer.surface_under(surface_local, WindowSurfaceType::ALL)
                            {
                                return Some((
                                    surface,
                                    (
                                        surface_loc.x as f64 + geo.loc.x as f64 + out_loc.x as f64,
                                        surface_loc.y as f64 + geo.loc.y as f64 + out_loc.y as f64,
                                    )
                                        .into(),
                                ));
                            }
                            // Fallback: return the layer's root wl_surface directly
                            // (handles cases where surface_under fails due to missing input region)
                            return Some((
                                layer.wl_surface().clone(),
                                pos,
                            ));
                        }
                    }
                }
            }
        }

        // Fall back to windows in the space
        self.space
            .element_under(pos)
            .and_then(|(window, loc)| {
                window
                    .surface_under(
                        (pos.x - loc.x as f64, pos.y - loc.y as f64),
                        WindowSurfaceType::ALL,
                    )
                    .map(|(s, p)| {
                        (s, (p.x as f64 + loc.x as f64, p.y as f64 + loc.y as f64).into())
                    })
            })
    }

    fn handle_pointer_motion_relative<B: smithay::backend::input::InputBackend>(
        &mut self,
        event: B::PointerMotionEvent,
    ) {
        let delta = event.delta();
        let old = self.pointer_location;
        let proposed = smithay::utils::Point::from((old.x + delta.x, old.y + delta.y));
        // Monitors may not tile the bounding rectangle (different sizes,
        // gaps, portrait mixed with landscape) — a simple clamp to
        // `output_bounds` lets the cursor escape into dead zones between
        // outputs. Clamp axis-by-axis against the union of real output
        // rects so the cursor always lands on a visible monitor.
        self.pointer_location =
            clamp_to_outputs(&self.outputs, &self.output_order, old, proposed);
        self.update_cursor_idle();

        // NOTE: pointer motion no longer updates `focused_output`. The user's
        // workflow is keyboard-driven — mod+focus_monitor_next/prev is the
        // only way to change which monitor receives spawned windows. Pointer
        // can move freely between monitors without disturbing focus.

        let pos = self.pointer_location;
        let serial = SERIAL_COUNTER.next_serial();
        let pointer = self.seat.get_pointer().unwrap();

        let under = self.surface_under_pointer(pos);

        pointer.motion(
            self,
            under,
            &MotionEvent {
                location: pos,
                serial,
                time: Event::time_msec(&event),
            },
        );
        pointer.frame(self);
    }

    fn handle_pointer_motion_absolute<B: smithay::backend::input::InputBackend>(
        &mut self,
        event: B::PointerMotionAbsoluteEvent,
    ) {
        let output = match self.space.outputs().next().cloned() {
            Some(o) => o,
            None => return,
        };

        let output_geo = match self.space.output_geometry(&output) {
            Some(g) => g,
            None => return,
        };

        let pos = event.position_transformed(output_geo.size);
        self.pointer_location = pos;
        self.update_cursor_idle();
        let serial = SERIAL_COUNTER.next_serial();
        let pointer = self.seat.get_pointer().unwrap();

        let under = self.surface_under_pointer(pos);

        pointer.motion(
            self,
            under,
            &MotionEvent {
                location: pos,
                serial,
                time: Event::time_msec(&event),
            },
        );
        pointer.frame(self);
    }

    fn handle_pointer_button<B: smithay::backend::input::InputBackend>(
        &mut self,
        event: B::PointerButtonEvent,
    ) {
        let serial = SERIAL_COUNTER.next_serial();
        let pointer = self.seat.get_pointer().unwrap();
        let pos = pointer.current_location();

        // On click, focus the window under the pointer — unless an
        // exclusive-keyboard layer surface (picker, launcher) owns
        // focus. wlr-layer-shell exclusive layers consume all
        // keyboard input; clicking a background window should route
        // the click but leave keyboard focus on the layer.
        if event.state() == ButtonState::Pressed && !self.keyboard_focus_is_exclusive_layer() {
            if let Some((window, _)) = self.space.element_under(pos) {
                let window = window.clone();
                self.focus_window(&window);
            }
        }

        pointer.button(
            self,
            &ButtonEvent {
                button: event.button_code(),
                state: event.state(),
                serial,
                time: Event::time_msec(&event),
            },
        );
        pointer.frame(self);
    }

    fn handle_pointer_axis<B: smithay::backend::input::InputBackend>(
        &mut self,
        event: B::PointerAxisEvent,
    ) {
        let pointer = self.seat.get_pointer().unwrap();
        let mut frame = AxisFrame::new(Event::time_msec(&event));

        // Populate BOTH continuous `value` AND discrete `v120` when libinput
        // provides both. Firefox/Floorp on Wayland specifically require v120
        // (high-resolution scroll) to translate scroll events into line
        // scrolls — without it the browser's internal delta computes to 0
        // and page scrolling silently breaks. The old `if / else if` meant
        // kara sent one or the other but never both.
        let h_amount = event.amount(Axis::Horizontal);
        let v_amount = event.amount(Axis::Vertical);
        let h_v120 = event.amount_v120(Axis::Horizontal);
        let v_v120 = event.amount_v120(Axis::Vertical);
        let source = event.source();
        frame = frame.source(source);

        // When the scroll source is a trackpad ("Finger"), libinput sends
        // a final event with amount == 0 on each axis at the moment the
        // user lifts their fingers. The Wayland spec requires this to be
        // translated to `wl_pointer.axis_stop`, not to a zero-value axis
        // event — Firefox/Floorp accumulate scroll deltas and wait for a
        // stop to finalize the gesture. Without it, the browser holds on
        // to kinetic/fling state and page scrolling gets stuck or jumpy.
        // Terminals don't use the kinetic-scroll path so they worked fine
        // despite the missing stop.
        let is_finger = matches!(source, smithay::backend::input::AxisSource::Finger);

        let push_axis = |frame: AxisFrame, axis: Axis, amount: Option<f64>| -> AxisFrame {
            match amount {
                Some(v) if v == 0.0 && is_finger => frame.stop(axis),
                Some(v) => frame.value(axis, v),
                None => frame,
            }
        };
        frame = push_axis(frame, Axis::Horizontal, h_amount);
        frame = push_axis(frame, Axis::Vertical, v_amount);

        if let Some(discrete) = h_v120 {
            frame = frame.v120(Axis::Horizontal, discrete as i32);
        }
        if let Some(discrete) = v_v120 {
            frame = frame.v120(Axis::Vertical, discrete as i32);
        }

        // Diagnostic for Floorp scroll overshoot. Logs one line per axis
        // event so we can compare raw libinput frame count against client
        // observations. Drop once root cause is found.
        tracing::debug!(
            target: "kara_gate::input::axis",
            ?source,
            h_amount = ?h_amount,
            v_amount = ?v_amount,
            h_v120 = ?h_v120,
            v_v120 = ?v_v120,
            "pointer axis frame"
        );

        pointer.axis(self, frame);
        pointer.frame(self);
    }
}

/// Clamp a proposed cursor position so it walks monitors in the user's
/// config-declared order rather than whatever spatial layout happens to
/// exist.
///
/// Horizontal motion is the interesting case: exiting the right edge of
/// the current monitor warps the cursor to the left edge of the
/// next-in-config-order monitor, and left-edge exit warps to the right
/// edge of the previous one. The y coordinate is carried through,
/// proportionally mapped to the destination's height so the cursor
/// doesn't end up off-screen when crossing a short→tall boundary.
///
/// Vertical motion stays inside the current monitor (clamp to its top
/// /bottom) — horizontal row layouts are the common case and letting the
/// cursor fall off top/bottom into nothing or into the wrong output is
/// confusing. If the user ever configures a vertical stack, the diagonal
/// / vertical paths still resolve via the destination's y-clamp.
///
/// Wrap-around is disabled: past the last monitor, cursor stops at the
/// edge. Matches Hyprland/sway's default.
fn clamp_to_outputs(
    outputs: &[crate::state::OutputState],
    order: &[usize],
    old: smithay::utils::Point<f64, smithay::utils::Logical>,
    new: smithay::utils::Point<f64, smithay::utils::Logical>,
) -> smithay::utils::Point<f64, smithay::utils::Logical> {
    if outputs.is_empty() {
        return new;
    }

    let inside = |p: smithay::utils::Point<f64, smithay::utils::Logical>,
                  idx: usize|
     -> bool {
        let o = &outputs[idx];
        let (w, h) = o.size;
        p.x >= o.location.x as f64
            && p.x < (o.location.x + w) as f64
            && p.y >= o.location.y as f64
            && p.y < (o.location.y + h) as f64
    };

    // Which monitor was the cursor on before this motion? Fall back to
    // the output that contains `new` if `old` is nowhere (startup / race).
    let cur_idx = (0..outputs.len())
        .find(|&i| inside(old, i))
        .or_else(|| (0..outputs.len()).find(|&i| inside(new, i)));

    let Some(cur_idx) = cur_idx else {
        // No anchor — pick the first configured monitor's top-left as a
        // safe landing spot.
        let first = order.first().copied().unwrap_or(0);
        let o = &outputs[first];
        return smithay::utils::Point::from((o.location.x as f64, o.location.y as f64));
    };

    let cur = &outputs[cur_idx];
    let (cw, ch) = cur.size;
    let cx0 = cur.location.x as f64;
    let cy0 = cur.location.y as f64;
    let cx1 = (cur.location.x + cw) as f64;
    let cy1 = (cur.location.y + ch) as f64;

    // Still inside the current monitor? Accept.
    if new.x >= cx0 && new.x < cx1 && new.y >= cy0 && new.y < cy1 {
        return new;
    }

    // Which neighbor in config order gets the cursor?
    let cur_rank = order.iter().position(|&i| i == cur_idx);
    let neighbor_in_order = |direction: i32| -> Option<usize> {
        let rank = cur_rank?;
        let next_rank = rank as i32 + direction;
        if next_rank < 0 || next_rank >= order.len() as i32 {
            return None;
        }
        Some(order[next_rank as usize])
    };

    // Horizontal crossing. Map y proportionally onto the destination so a
    // short→tall handoff doesn't dump the cursor above/below the panel.
    let map_y = |dest: &crate::state::OutputState| -> f64 {
        let frac = ((new.y - cy0) / ch as f64).clamp(0.0, 1.0);
        let dh = dest.size.1 as f64;
        let dy0 = dest.location.y as f64;
        dy0 + frac * (dh - 1.0)
    };

    if new.x >= cx1 {
        // Exiting right → next monitor in config order.
        if let Some(next_idx) = neighbor_in_order(1) {
            let dest = &outputs[next_idx];
            return smithay::utils::Point::from((dest.location.x as f64, map_y(dest)));
        }
        // No monitor to the right in config order — stop at the edge.
        return smithay::utils::Point::from((cx1 - 1.0, new.y.clamp(cy0, cy1 - 1.0)));
    }
    if new.x < cx0 {
        // Exiting left → previous monitor in config order.
        if let Some(prev_idx) = neighbor_in_order(-1) {
            let dest = &outputs[prev_idx];
            let dest_x = (dest.location.x + dest.size.0) as f64 - 1.0;
            return smithay::utils::Point::from((dest_x, map_y(dest)));
        }
        return smithay::utils::Point::from((cx0, new.y.clamp(cy0, cy1 - 1.0)));
    }

    // Vertical over-run on the current monitor — clamp to its edge.
    smithay::utils::Point::from((new.x.clamp(cx0, cx1 - 1.0), new.y.clamp(cy0, cy1 - 1.0)))
}
