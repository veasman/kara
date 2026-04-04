use smithay::backend::input::{
    AbsolutePositionEvent, ButtonState, Event, InputEvent, KeyState, KeyboardKeyEvent,
    PointerButtonEvent,
};
use smithay::desktop::WindowSurfaceType;
use smithay::input::keyboard::{FilterResult, keysyms, ModifiersState};
use smithay::input::pointer::{ButtonEvent, MotionEvent};
use smithay::utils::SERIAL_COUNTER;

use crate::actions::Action;
use crate::state::Vwm;

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

pub fn default_keybinds() -> Vec<Keybind> {
    let m = |sym: u32, action: Action| Keybind {
        mods: ModMask { logo: true, shift: false, ctrl: false, alt: false },
        sym,
        action,
    };
    let ms = |sym: u32, action: Action| Keybind {
        mods: ModMask { logo: true, shift: true, ctrl: false, alt: false },
        sym,
        action,
    };

    vec![
        m(keysyms::KEY_Return, Action::Spawn("foot".into())),
        m(keysyms::KEY_j, Action::FocusNext),
        m(keysyms::KEY_k, Action::FocusPrev),
        m(keysyms::KEY_q, Action::KillClient),
        ms(keysyms::KEY_Return, Action::ZoomMaster),
        m(keysyms::KEY_f, Action::ToggleMonocle),
        ms(keysyms::KEY_f, Action::ToggleFullscreen),
        m(keysyms::KEY_bracketleft, Action::DecreaseMfact),
        m(keysyms::KEY_bracketright, Action::IncreaseMfact),
        ms(keysyms::KEY_q, Action::Quit),
        m(keysyms::KEY_1, Action::ViewWs(0)),
        m(keysyms::KEY_2, Action::ViewWs(1)),
        m(keysyms::KEY_3, Action::ViewWs(2)),
        m(keysyms::KEY_4, Action::ViewWs(3)),
        m(keysyms::KEY_5, Action::ViewWs(4)),
        m(keysyms::KEY_6, Action::ViewWs(5)),
        m(keysyms::KEY_7, Action::ViewWs(6)),
        m(keysyms::KEY_8, Action::ViewWs(7)),
        m(keysyms::KEY_9, Action::ViewWs(8)),
        ms(keysyms::KEY_1, Action::SendWs(0)),
        ms(keysyms::KEY_2, Action::SendWs(1)),
        ms(keysyms::KEY_3, Action::SendWs(2)),
        ms(keysyms::KEY_4, Action::SendWs(3)),
        ms(keysyms::KEY_5, Action::SendWs(4)),
        ms(keysyms::KEY_6, Action::SendWs(5)),
        ms(keysyms::KEY_7, Action::SendWs(6)),
        ms(keysyms::KEY_8, Action::SendWs(7)),
        ms(keysyms::KEY_9, Action::SendWs(8)),
    ]
}

impl Vwm {
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
