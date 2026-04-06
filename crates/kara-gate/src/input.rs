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
        BindAction::ViewWs(idx) => Action::ViewWs(*idx),
        BindAction::SendWs(idx) => Action::SendWs(*idx),
    }
}

impl Gate {
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

        let action = keyboard.input(
            self,
            keycode,
            key_state,
            serial,
            time,
            |_state, mods, handle| {
                if pressed {
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
        let (max_x, max_y) = self.output_bounds;
        let new_x = (self.pointer_location.x + delta.x).clamp(0.0, max_x as f64 - 1.0);
        let new_y = (self.pointer_location.y + delta.y).clamp(0.0, max_y as f64 - 1.0);
        self.pointer_location = (new_x, new_y).into();
        self.update_cursor_idle();

        // Update focused output based on pointer position
        let new_output = self.output_for_point(self.pointer_location);
        if new_output != self.focused_output {
            self.focused_output = new_output;
        }

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

        // On click, focus the window under the pointer
        if event.state() == ButtonState::Pressed {
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

        if let Some(amount) = event.amount(Axis::Horizontal) {
            frame = frame.value(Axis::Horizontal, amount);
        } else if let Some(discrete) = event.amount_v120(Axis::Horizontal) {
            frame = frame.v120(Axis::Horizontal, discrete as i32);
        }

        if let Some(amount) = event.amount(Axis::Vertical) {
            frame = frame.value(Axis::Vertical, amount);
        } else if let Some(discrete) = event.amount_v120(Axis::Vertical) {
            frame = frame.v120(Axis::Vertical, discrete as i32);
        }

        frame = frame.source(event.source());

        pointer.axis(self, frame);
        pointer.frame(self);
    }
}
