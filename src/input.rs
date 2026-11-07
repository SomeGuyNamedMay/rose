use smithay::{
    backend::input::{
        AbsolutePositionEvent, Axis, AxisSource, ButtonState, Event, InputBackend, InputEvent,
        KeyState, KeyboardKeyEvent, PointerAxisEvent, PointerButtonEvent,
    },
    input::{
        keyboard::{Keysym, KeysymHandle, ModifiersState},
        pointer::{AxisFrame, ButtonEvent, GrabStartData, MotionEvent},
    },
    utils::{Serial, SERIAL_COUNTER},
};

use crate::{
    action::{Action, ModeKind},
    config::KeyBinding,
    grab::Edges,
    wm::Mode,
    Rose,
};

/// What to do with a key once the keyboard has been updated.
enum KeyCommand {
    Action(Action, KeyBinding),
    ModeKey(Keysym),
    MenuKey(Keysym, ModifiersState),
    ExitInputGrab,
    /// A key was released while moving or resizing with the keyboard.
    EndGrab,
}

impl Rose {
    pub fn process_input_event<I: InputBackend>(&mut self, event: InputEvent<I>) {
        match event {
            InputEvent::Keyboard { event, .. } => {
                let serial = SERIAL_COUNTER.next_serial();
                let time = Event::time_msec(&event);
                let keycode = event.key_code();
                let state = event.state();

                let keyboard = self.seat.get_keyboard().unwrap();
                let ((forward, command), mods_changed) =
                    keyboard.input_intercept(self, keycode, state, |rose, mods, handle| {
                        rose.filter_key(keycode.raw(), state, mods, &handle)
                    });
                if forward {
                    keyboard.input_forward(self, keycode, state, serial, time, mods_changed);
                }
                if let Some(command) = command {
                    self.run_key_command(command, serial, time);
                }
            }
            InputEvent::PointerMotionAbsolute { event, .. } => {
                let output = self.space.outputs().next().unwrap();
                let output_geo = self.space.output_geometry(output).unwrap();
                let pos = event.position_transformed(output_geo.size) + output_geo.loc.to_f64();

                let serial = SERIAL_COUNTER.next_serial();
                let pointer = self.seat.get_pointer().unwrap();
                let under = self.surface_under(pos);

                pointer.motion(
                    self,
                    under,
                    &MotionEvent {
                        location: pos,
                        serial,
                        time: event.time_msec(),
                    },
                );
                pointer.frame(self);
            }
            InputEvent::PointerButton { event, .. } => {
                let serial = SERIAL_COUNTER.next_serial();
                let button = event.button_code();
                let state = event.state();

                if state == ButtonState::Pressed {
                    if !self.handle_button_press(button, serial) {
                        self.suppressed_buttons.insert(button);
                        return;
                    }
                } else if self.suppressed_buttons.remove(&button) {
                    return;
                }

                let pointer = self.seat.get_pointer().unwrap();
                pointer.button(
                    self,
                    &ButtonEvent {
                        button,
                        state,
                        serial,
                        time: event.time_msec(),
                    },
                );
                pointer.frame(self);
            }
            InputEvent::PointerAxis { event, .. } => {
                let source = event.source();
                let pointer = self.seat.get_pointer().unwrap();
                if self.over_panels(pointer.current_location()) {
                    // Scrolling over the menu moves the selection.
                    let amount = event
                        .amount_v120(Axis::Vertical)
                        .or_else(|| event.amount(Axis::Vertical))
                        .unwrap_or(0.0);
                    if let Some(menu) = self.menu.as_mut().filter(|_| amount != 0.0) {
                        menu.move_selection(amount.signum() as isize);
                    }
                    return;
                }
                let mut frame = AxisFrame::new(event.time_msec()).source(source);

                for axis in [Axis::Horizontal, Axis::Vertical] {
                    let v120 = event.amount_v120(axis);
                    let amount = event
                        .amount(axis)
                        .unwrap_or_else(|| v120.unwrap_or(0.0) * 15.0 / 120.0);

                    if amount != 0.0 {
                        frame = frame.value(axis, amount);
                        if let Some(discrete) = v120 {
                            frame = frame.v120(axis, discrete as i32);
                        }
                    }
                    if source == AxisSource::Finger && event.amount(axis) == Some(0.0) {
                        frame = frame.stop(axis);
                    }
                }

                let pointer = self.seat.get_pointer().unwrap();
                pointer.axis(self, frame);
                pointer.frame(self);
            }
            _ => {}
        }
    }

    /// Decides whether a key goes to the client, and what the compositor does with it.
    fn filter_key(
        &mut self,
        code: u32,
        state: KeyState,
        mods: &ModifiersState,
        handle: &KeysymHandle<'_>,
    ) -> (bool, Option<KeyCommand>) {
        let grabbing = matches!(self.mode, Mode::Move | Mode::Resize);
        if state == KeyState::Released {
            let forward = !self.suppressed_keys.remove(&code);
            return (forward, grabbing.then_some(KeyCommand::EndGrab));
        }

        // Clients always see modifiers, so they stay in sync.
        if handle.modified_sym().is_modifier_key() {
            return (true, None);
        }

        let command = match &self.mode {
            Mode::Normal => match self.config.key_action(mods, handle) {
                Some((binding, action)) => Some(KeyCommand::Action(action, binding)),
                None => return (true, None),
            },
            Mode::InputGrab { exit } => {
                if !exit.matches(mods, handle) {
                    return (true, None);
                }
                Some(KeyCommand::ExitInputGrab)
            }
            Mode::Move | Mode::Resize => None,
            Mode::Menu => Some(KeyCommand::MenuKey(handle.modified_sym(), *mods)),
            _ => Some(KeyCommand::ModeKey(handle.modified_sym())),
        };
        self.suppressed_keys.insert(code);
        (false, command)
    }

    fn run_key_command(&mut self, command: KeyCommand, serial: Serial, time: u32) {
        match command {
            KeyCommand::Action(action, binding) => self.run_action(action, Some(binding)),
            KeyCommand::ModeKey(sym) => self.handle_mode_key(sym),
            KeyCommand::MenuKey(sym, mods) => self.handle_menu_key(sym, &mods),
            KeyCommand::ExitInputGrab => self.mode = Mode::Normal,
            KeyCommand::EndGrab => {
                let pointer = self.seat.get_pointer().unwrap();
                pointer.unset_grab(self, serial, time);
                self.mode = Mode::Normal;
            }
        }
    }

    /// Handles mouse bindings and click to focus. Returns whether the press goes to the client.
    fn handle_button_press(&mut self, button: u32, serial: Serial) -> bool {
        let pointer = self.seat.get_pointer().unwrap();
        if pointer.is_grabbed() {
            return true;
        }
        let location = pointer.current_location();
        // Clicking outside the menu closes it.
        if self.panel_click(location) {
            return false;
        }
        if self.menu.is_some() {
            self.close_menu();
            return false;
        }

        let mods = self.seat.get_keyboard().unwrap().modifier_state();
        let under = self.space.element_under(location).map(|(w, _)| w.clone());

        let Some(action) = self.config.mouse_action(&mods, button) else {
            // Click to focus; clicking the background unfocuses.
            self.focus(under);
            return true;
        };

        if under.is_some() {
            self.focus(under.clone());
        }
        let start_data = GrabStartData {
            focus: None,
            button,
            location,
        };
        match (action, under) {
            // The press is forwarded so the grab sees the button go down and back up.
            (Action::EnterMode(ModeKind::Move), Some(window)) => {
                self.start_move(window, start_data, serial, true);
                true
            }
            (Action::EnterMode(ModeKind::Resize), Some(window)) => {
                let edges = Edges::nearest(self.geometry(&window), location);
                self.start_resize(window, edges, start_data, serial, true);
                true
            }
            (Action::EnterMode(ModeKind::Move | ModeKind::Resize), None) => false,
            (action, _) => {
                self.run_action(action, None);
                false
            }
        }
    }
}
