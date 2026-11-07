//! Synthetic keyboard and pointer input, sent to the host sway through its virtual input
//! protocols so that it reaches Rose's window like real input.

use std::{
    io::Write,
    os::{fd::AsFd, unix::net::UnixStream},
    path::Path,
    time::{Duration, Instant},
};

use wayland_client::{
    globals::{registry_queue_init, GlobalListContents},
    protocol::{wl_pointer::ButtonState, wl_registry::WlRegistry, wl_seat::WlSeat},
    Connection, Dispatch, EventQueue, QueueHandle,
};
use wayland_protocols_misc::zwp_virtual_keyboard_v1::client::{
    zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1,
    zwp_virtual_keyboard_v1::ZwpVirtualKeyboardV1,
};
use wayland_protocols_wlr::virtual_pointer::v1::client::{
    zwlr_virtual_pointer_manager_v1::ZwlrVirtualPointerManagerV1,
    zwlr_virtual_pointer_v1::ZwlrVirtualPointerV1,
};

use crate::harness::{OUTPUT_HEIGHT, OUTPUT_WIDTH};

/// Pause after each event so that the host and Rose process them in order.
const EVENT_DELAY: Duration = Duration::from_millis(30);

pub struct VirtualInput {
    conn: Connection,
    queue: EventQueue<State>,
    keyboard: ZwpVirtualKeyboardV1,
    pointer: ZwlrVirtualPointerV1,
    start: Instant,
}

pub struct State;

#[derive(Clone, Copy)]
pub enum Button {
    Left,
    Right,
}

impl VirtualInput {
    /// Connects to the host compositor and uploads `keymap`, an xkb keymap whose keycodes are evdev
    /// codes plus 8, so that the codes sent here mean the same thing to Rose.
    pub fn connect(socket: &Path, keymap: &str) -> Self {
        let conn = Connection::from_socket(UnixStream::connect(socket).unwrap()).unwrap();
        let (globals, mut queue) = registry_queue_init::<State>(&conn).unwrap();
        let qh = queue.handle();
        let seat: WlSeat = globals.bind(&qh, 1..=7, ()).unwrap();
        let keyboards: ZwpVirtualKeyboardManagerV1 = globals.bind(&qh, 1..=1, ()).unwrap();
        let pointers: ZwlrVirtualPointerManagerV1 = globals.bind(&qh, 1..=2, ()).unwrap();
        let keyboard = keyboards.create_virtual_keyboard(&seat, &qh, ());
        let pointer = pointers.create_virtual_pointer(Some(&seat), &qh, ());

        let mut data = keymap.as_bytes().to_vec();
        data.push(0);
        let path = socket.with_file_name("keymap");
        let mut file = std::fs::File::options()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(&path)
            .unwrap();
        file.write_all(&data).unwrap();
        std::fs::remove_file(&path).unwrap();
        keyboard.keymap(1, file.as_fd(), data.len() as u32);
        queue.roundtrip(&mut State).unwrap();

        let mut input = Self {
            conn,
            queue,
            keyboard,
            pointer,
            start: Instant::now(),
        };
        // wlroots drops the first key of a new virtual keyboard while it switches keymaps.
        input.key(&["ctrl"]);
        std::thread::sleep(Duration::from_millis(150));
        input
    }

    fn time(&self) -> u32 {
        self.start.elapsed().as_millis() as u32
    }

    fn send(&mut self) {
        self.conn.flush().unwrap();
        self.queue.dispatch_pending(&mut State).unwrap();
        std::thread::sleep(EVENT_DELAY);
    }

    /// Presses a key while holding it, e.g. `hold("alt")`.
    pub fn hold(&mut self, key: &str) {
        self.keyboard.key(self.time(), code(key), 1);
        self.send();
    }

    pub fn release(&mut self, key: &str) {
        self.keyboard.key(self.time(), code(key), 0);
        self.send();
    }

    /// Presses a key combination like `["alt", "shift", "2"]` and releases it in reverse order.
    pub fn key(&mut self, keys: &[&str]) {
        for key in keys {
            self.hold(key);
        }
        for key in keys.iter().rev() {
            self.release(key);
        }
    }

    /// Types lowercase letters, digits, spaces and dashes.
    pub fn type_text(&mut self, text: &str) {
        for c in text.chars() {
            self.key(&[&c.to_string()]);
        }
    }

    pub fn move_to(&mut self, x: u32, y: u32) {
        self.pointer
            .motion_absolute(self.time(), x, y, OUTPUT_WIDTH, OUTPUT_HEIGHT);
        self.pointer.frame();
        self.send();
    }

    pub fn button(&mut self, button: Button, pressed: bool) {
        let code = match button {
            Button::Left => 0x110,
            Button::Right => 0x111,
        };
        let state = if pressed {
            ButtonState::Pressed
        } else {
            ButtonState::Released
        };
        self.pointer.button(self.time(), code, state);
        self.pointer.frame();
        self.send();
    }

    pub fn click(&mut self, x: u32, y: u32) {
        self.move_to(x, y);
        self.button(Button::Left, true);
        self.button(Button::Left, false);
    }

    /// Presses `button` at `from`, moves to `to` in a few steps and releases it.
    pub fn drag(&mut self, button: Button, from: (u32, u32), to: (u32, u32)) {
        self.move_to(from.0, from.1);
        self.button(button, true);
        for step in 1..=4 {
            let lerp = |a: u32, b: u32| (a as i64 + (b as i64 - a as i64) * step / 4) as u32;
            self.move_to(lerp(from.0, to.0), lerp(from.1, to.1));
        }
        self.button(button, false);
    }
}

/// The evdev code for a key name.
fn code(name: &str) -> u32 {
    // Letters in evdev order; `.` marks codes that aren't letters.
    const LETTERS: &str = "qwertyuiop....asdfghjkl.....zxcvbnm";
    match name {
        "escape" => 1,
        "0" => 11,
        "minus" | "-" => 12,
        "equal" => 13,
        "backspace" => 14,
        "tab" => 15,
        "bracketleft" => 26,
        "bracketright" => 27,
        "return" => 28,
        "ctrl" => 29,
        "semicolon" => 39,
        "apostrophe" => 40,
        "shift" => 42,
        "comma" => 51,
        "period" => 52,
        "slash" => 53,
        "alt" => 56,
        "space" | " " => 57,
        "up" => 103,
        "left" => 105,
        "right" => 106,
        "down" => 108,
        "super" => 125,
        digit if digit.len() == 1 && digit.as_bytes()[0].is_ascii_digit() => {
            1 + (digit.as_bytes()[0] - b'0') as u32
        }
        letter if letter.len() == 1 && LETTERS.contains(letter) => {
            16 + LETTERS.find(letter).unwrap() as u32
        }
        _ => panic!("unknown key `{name}`"),
    }
}

impl Dispatch<WlRegistry, GlobalListContents> for State {
    fn event(
        _: &mut Self,
        _: &WlRegistry,
        _: <WlRegistry as wayland_client::Proxy>::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

wayland_client::delegate_noop!(State: ignore WlSeat);
wayland_client::delegate_noop!(State: ZwpVirtualKeyboardManagerV1);
wayland_client::delegate_noop!(State: ZwpVirtualKeyboardV1);
wayland_client::delegate_noop!(State: ZwlrVirtualPointerManagerV1);
wayland_client::delegate_noop!(State: ZwlrVirtualPointerV1);
