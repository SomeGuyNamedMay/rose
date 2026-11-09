//! The desktop is an infinite plane that the output looks at part of.
//!
//! Everything in Rose is positioned relative to the output, so moving the camera translates every
//! position Rose keeps by the opposite amount. Layouts, the pointer, popups and the bar need no
//! idea of where the camera is.

use std::time::Duration;

use smithay::{
    desktop::Window,
    input::keyboard::Keysym,
    reexports::calloop::timer::{TimeoutAction, Timer},
    utils::{Logical, Point},
};

use crate::{action::Direction, wm::Mode, Rose};

impl Rose {
    /// Whether any part of the view is in the work area.
    pub fn on_screen(&self, window: &Window) -> bool {
        self.geometry(window).overlaps(self.work_area())
    }

    fn move_camera(&mut self, by: Point<i32, Logical>) {
        self.camera += by;
        for view in self.views.iter_mut().filter(|v| v.placed) {
            view.location -= by;
            view.original.loc -= by;
            if let Some(rect) = &mut view.natural {
                rect.loc -= by;
            }
            if let Some(rect) = &mut view.tile {
                rect.loc -= by;
            }
            if let Some(anchor) = &mut view.resize {
                anchor.initial.loc -= by;
            }
            if self.space.element_location(&view.window).is_some() {
                self.space
                    .map_element(view.window.clone(), view.location, false);
            }
        }
        for layout in self.sheet_layouts.iter_mut().flatten() {
            layout.origin -= by;
        }
        self.restack();
        // A different view may be under the pointer now.
        self.refresh_pointer();
    }

    /// Moves the camera so that an off-screen view comes into sight.
    pub fn reveal(&mut self, window: &Window) {
        if self.on_screen(window) {
            return;
        }
        let rect = self.geometry(window);
        let area = self.content_area();
        let shift = |start: i32, len: i32, area_start: i32, area_len: i32| {
            if start < area_start || len >= area_len {
                start - area_start
            } else {
                (start + len - (area_start + area_len)).max(0)
            }
        };
        let by = Point::from((
            shift(rect.loc.x, rect.size.w, area.loc.x, area.size.w),
            shift(rect.loc.y, rect.size.h, area.loc.y, area.size.h),
        ));
        self.move_camera(by);
    }

    /// Moves the camera by one step per direction, unless a pointer grab is holding a view.
    fn pan(&mut self, directions: impl IntoIterator<Item = Direction>) {
        if self.seat.get_pointer().unwrap().is_grabbed() {
            return;
        }
        let step = self.config.ui.step;
        let mut by = Point::<i32, Logical>::default();
        for direction in directions {
            match direction {
                Direction::Up => by.y -= step,
                Direction::Down => by.y += step,
                Direction::Left => by.x -= step,
                Direction::Right => by.x += step,
            }
        }
        if by != Point::default() {
            self.move_camera(by);
        }
    }

    /// Handles a key pressed in pan mode.
    pub fn handle_pan_key(&mut self, code: u32, sym: Keysym) {
        if sym == Keysym::Escape {
            self.exit_pan();
            return;
        }
        let direction = match sym.key_char().map(|c| c.to_ascii_lowercase()) {
            Some('h') => Direction::Left,
            Some('j') => Direction::Down,
            Some('k') => Direction::Up,
            Some('l') => Direction::Right,
            _ => return,
        };
        self.pan_keys.push((code, direction));
        self.pan([direction]);
        self.start_pan_repeat();
    }

    /// Leaves pan mode. Focus doesn't stay on a view that was panned out of sight.
    pub fn exit_pan(&mut self) {
        self.stop_pan_repeat();
        self.mode = Mode::Normal;
        if !self.focused.as_ref().is_some_and(|w| self.on_screen(w)) {
            self.focus_top();
        }
    }

    /// The backends don't repeat keys, so held keys are repeated here, at the keyboard's rate.
    fn start_pan_repeat(&mut self) {
        let keyboard = &self.config.keyboard;
        if keyboard.repeat_rate <= 0 || self.pan_timer.is_some() {
            return;
        }
        let interval = Duration::from_secs_f64(1.0 / f64::from(keyboard.repeat_rate));
        let delay = Duration::from_millis(keyboard.repeat_delay.max(0) as u64);
        self.pan_timer = self
            .loop_handle
            .insert_source(Timer::from_duration(delay), move |_, _, rose| {
                if !matches!(rose.mode, Mode::Pan) {
                    rose.pan_keys.clear();
                    rose.pan_timer = None;
                    return TimeoutAction::Drop;
                }
                let held: Vec<_> = rose.pan_keys.iter().map(|(_, d)| *d).collect();
                rose.pan(held);
                TimeoutAction::ToDuration(interval)
            })
            .map_err(|err| tracing::error!("failed to repeat panning: {err}"))
            .ok();
    }

    /// Forgets all held keys, for when their releases won't arrive.
    pub fn stop_pan_repeat(&mut self) {
        self.pan_keys.clear();
        if let Some(timer) = self.pan_timer.take() {
            self.loop_handle.remove(timer);
        }
    }

    pub fn release_pan_key(&mut self, code: u32) {
        self.pan_keys.retain(|(held, _)| *held != code);
        if self.pan_keys.is_empty() {
            self.stop_pan_repeat();
        }
    }
}
