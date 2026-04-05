use smithay::backend::input::{
    AbsolutePositionEvent, ButtonState, Event, InputEvent, KeyState, KeyboardKeyEvent,
    PointerButtonEvent,
};
use smithay::desktop::WindowSurfaceType;
use smithay::input::keyboard::{FilterResult, ModifiersState};
use smithay::input::pointer::{ButtonEvent, MotionEvent};
use smithay::utils::SERIAL_COUNTER;

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
        BindAction::Scratchpad(_) => Action::None, // TODO: M5
        BindAction::FocusNext => Action::FocusNext,
        BindAction::FocusPrev => Action::FocusPrev,
        BindAction::FocusMonitorPrev => Action::None, // TODO: M5
        BindAction::FocusMonitorNext => Action::None, // TODO: M5
        BindAction::SendMonitorPrev => Action::None, // TODO: M5
        BindAction::SendMonitorNext => Action::None, // TODO: M5
        BindAction::DecreaseMfact => Action::DecreaseMfact,
        BindAction::IncreaseMfact => Action::IncreaseMfact,
        BindAction::ZoomMaster => Action::ZoomMaster,
        BindAction::Monocle => Action::ToggleMonocle,
        BindAction::Fullscreen => Action::ToggleFullscreen,
        BindAction::ToggleSync => Action::None, // TODO: M5
        BindAction::KillClient => Action::KillClient,
        BindAction::Reload => Action::Reload,
        BindAction::Quit => Action::Quit,
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
            InputEvent::PointerMotionAbsolute { event } => self.handle_pointer_motion::<B>(event),
            InputEvent::PointerButton { event } => self.handle_pointer_button::<B>(event),
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

        // Clone keybinds to avoid borrow conflict in the closure
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
                    let sym = handle.modified_sym();
                    for bind in &keybinds {
                        if bind.sym == sym.raw() && bind.mods.matches(mods) {
                            return FilterResult::Intercept(bind.action.clone());
                        }
                    }
                }
                FilterResult::Forward
            },
        );

        if let Some(action) = action {
            self.dispatch_action(action);
        }
    }

    fn handle_pointer_motion<B: smithay::backend::input::InputBackend>(
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
        let serial = SERIAL_COUNTER.next_serial();
        let pointer = self.seat.get_pointer().unwrap();

        let under = self.space.element_under(pos)
            .and_then(|(window, loc)| {
                window
                    .surface_under(
                        (pos.x - loc.x as f64, pos.y - loc.y as f64),
                        WindowSurfaceType::ALL,
                    )
                    .map(|(s, p)| {
                        (s, (p.x as f64 + loc.x as f64, p.y as f64 + loc.y as f64).into())
                    })
            });

        pointer.motion(
            self,
            under,
            &MotionEvent {
                location: pos,
                serial,
                time: Event::time_msec(&event),
            },
        );
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
    }
}
