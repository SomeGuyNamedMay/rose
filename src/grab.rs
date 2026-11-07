//! Pointer grabs that move or resize a view, started by clients, mouse bindings or the
//! keyboard-driven move and resize modes.

use smithay::{
    desktop::Window,
    input::pointer::{
        AxisFrame, ButtonEvent, Focus, GestureHoldBeginEvent, GestureHoldEndEvent,
        GesturePinchBeginEvent, GesturePinchEndEvent, GesturePinchUpdateEvent,
        GestureSwipeBeginEvent, GestureSwipeEndEvent, GestureSwipeUpdateEvent,
        GrabStartData as PointerGrabStartData, MotionEvent, PointerGrab, PointerInnerHandle,
        RelativeMotionEvent,
    },
    reexports::{
        wayland_protocols::xdg::shell::server::xdg_toplevel,
        wayland_server::protocol::wl_surface::WlSurface,
    },
    utils::{Logical, Point, Rectangle, Serial, Size, SERIAL_COUNTER},
    wayland::{compositor, shell::xdg::SurfaceCachedState},
};

use crate::{
    wm::{Mode, ResizeAnchor},
    Rose,
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Edges {
    pub top: bool,
    pub bottom: bool,
    pub left: bool,
    pub right: bool,
}

impl Edges {
    pub const BOTTOM_RIGHT: Self = Self {
        top: false,
        bottom: true,
        left: false,
        right: true,
    };

    /// The edges nearest to `point`, for resizing from wherever a view was grabbed.
    pub fn nearest(rect: Rectangle<i32, Logical>, point: Point<f64, Logical>) -> Self {
        let center_x = rect.loc.x as f64 + rect.size.w as f64 / 2.0;
        let center_y = rect.loc.y as f64 + rect.size.h as f64 / 2.0;
        Self {
            top: point.y < center_y,
            bottom: point.y >= center_y,
            left: point.x < center_x,
            right: point.x >= center_x,
        }
    }
}

impl From<xdg_toplevel::ResizeEdge> for Edges {
    fn from(edge: xdg_toplevel::ResizeEdge) -> Self {
        let bits = edge as u32;
        Self {
            top: bits & 1 != 0,
            bottom: bits & 2 != 0,
            left: bits & 4 != 0,
            right: bits & 8 != 0,
        }
    }
}

enum GrabKind {
    Move {
        initial_location: Point<i32, Logical>,
    },
    Resize {
        edges: Edges,
        initial: Rectangle<i32, Logical>,
        last_size: Size<i32, Logical>,
    },
}

pub struct WindowGrab {
    start_data: PointerGrabStartData<Rose>,
    window: Window,
    kind: GrabKind,
    /// Mouse grabs end when the buttons are released; keyboard grabs end with their mode.
    release_on_button: bool,
}

impl Rose {
    pub fn start_move(
        &mut self,
        window: Window,
        start_data: PointerGrabStartData<Rose>,
        serial: Serial,
        release_on_button: bool,
    ) {
        self.clear_maximized(&window);
        let grab = WindowGrab {
            start_data,
            kind: GrabKind::Move {
                initial_location: self.geometry(&window).loc,
            },
            window,
            release_on_button,
        };
        self.seat
            .get_pointer()
            .unwrap()
            .set_grab(self, grab, serial, Focus::Clear);
    }

    pub fn start_resize(
        &mut self,
        window: Window,
        edges: Edges,
        start_data: PointerGrabStartData<Rose>,
        serial: Serial,
        release_on_button: bool,
    ) {
        self.clear_maximized(&window);
        let initial = self.geometry(&window);
        if let Some(view) = self.view_mut(&window) {
            view.resize = Some(ResizeAnchor {
                edges,
                initial,
                finished: false,
            });
        }

        let toplevel = window.toplevel().unwrap();
        toplevel.with_pending_state(|state| {
            state.states.set(xdg_toplevel::State::Resizing);
        });
        toplevel.send_pending_configure();

        let grab = WindowGrab {
            start_data,
            window,
            kind: GrabKind::Resize {
                edges,
                initial,
                last_size: initial.size,
            },
            release_on_button,
        };
        self.seat
            .get_pointer()
            .unwrap()
            .set_grab(self, grab, serial, Focus::Clear);
    }

    /// Starts moving or resizing the focused view with the pointer, until a key is released.
    pub fn start_keyboard_grab(&mut self, resize: bool) {
        let pointer = self.seat.get_pointer().unwrap();
        let Some(window) = self.focused.clone() else {
            return;
        };
        if pointer.is_grabbed() {
            return;
        }

        let start_data = PointerGrabStartData {
            focus: None,
            button: 0,
            location: pointer.current_location(),
        };
        let serial = SERIAL_COUNTER.next_serial();
        if resize {
            self.mode = Mode::Resize;
            self.start_resize(window, Edges::BOTTOM_RIGHT, start_data, serial, false);
        } else {
            self.mode = Mode::Move;
            self.start_move(window, start_data, serial, false);
        }
    }

    /// Keeps the opposite edges in place while resizing from the top or left.
    /// Called on every commit of a toplevel.
    pub fn handle_resize_commit(&mut self, window: &Window) {
        let Some(anchor) = self.view(window).and_then(|v| v.resize) else {
            return;
        };
        let mut location = self.geometry(window).loc;
        let size = window.geometry().size;
        if anchor.edges.left {
            location.x = anchor.initial.loc.x + anchor.initial.size.w - size.w;
        }
        if anchor.edges.top {
            location.y = anchor.initial.loc.y + anchor.initial.size.h - size.h;
        }
        if anchor.finished {
            if let Some(view) = self.view_mut(window) {
                view.resize = None;
            }
        }
        self.set_location(window, location);
    }
}

impl WindowGrab {
    fn resize_to(&mut self, delta: Point<f64, Logical>) {
        let GrabKind::Resize {
            edges,
            initial,
            last_size,
        } = &mut self.kind
        else {
            return;
        };

        let mut width = initial.size.w;
        let mut height = initial.size.h;
        if edges.left {
            width -= delta.x as i32;
        } else if edges.right {
            width += delta.x as i32;
        }
        if edges.top {
            height -= delta.y as i32;
        } else if edges.bottom {
            height += delta.y as i32;
        }

        let toplevel = self.window.toplevel().unwrap();
        let (min_size, max_size) = compositor::with_states(toplevel.wl_surface(), |states| {
            let mut guard = states.cached_state.get::<SurfaceCachedState>();
            let data = guard.current();
            (data.min_size, data.max_size)
        });
        let max = |m: i32| if m == 0 { i32::MAX } else { m };
        *last_size = Size::from((
            width.clamp(min_size.w.max(1), max(max_size.w).max(min_size.w.max(1))),
            height.clamp(min_size.h.max(1), max(max_size.h).max(min_size.h.max(1))),
        ));

        toplevel.with_pending_state(|state| {
            state.states.set(xdg_toplevel::State::Resizing);
            state.size = Some(*last_size);
        });
        toplevel.send_pending_configure();
    }
}

impl PointerGrab<Rose> for WindowGrab {
    fn motion(
        &mut self,
        data: &mut Rose,
        handle: &mut PointerInnerHandle<'_, Rose>,
        _focus: Option<(WlSurface, Point<f64, Logical>)>,
        event: &MotionEvent,
    ) {
        // While the grab is active, no client has pointer focus.
        handle.motion(data, None, event);

        let delta = event.location - self.start_data.location;
        match self.kind {
            GrabKind::Move { initial_location } => {
                let location = (initial_location.to_f64() + delta).to_i32_round();
                data.set_location(&self.window, location);
            }
            GrabKind::Resize { .. } => self.resize_to(delta),
        }
    }

    fn relative_motion(
        &mut self,
        data: &mut Rose,
        handle: &mut PointerInnerHandle<'_, Rose>,
        focus: Option<(WlSurface, Point<f64, Logical>)>,
        event: &RelativeMotionEvent,
    ) {
        handle.relative_motion(data, focus, event);
    }

    fn button(
        &mut self,
        data: &mut Rose,
        handle: &mut PointerInnerHandle<'_, Rose>,
        event: &ButtonEvent,
    ) {
        handle.button(data, event);
        if self.release_on_button && handle.current_pressed().is_empty() {
            handle.unset_grab(self, data, event.serial, event.time, true);
        }
    }

    fn axis(
        &mut self,
        data: &mut Rose,
        handle: &mut PointerInnerHandle<'_, Rose>,
        details: AxisFrame,
    ) {
        handle.axis(data, details)
    }

    fn frame(&mut self, data: &mut Rose, handle: &mut PointerInnerHandle<'_, Rose>) {
        handle.frame(data);
    }

    fn gesture_swipe_begin(
        &mut self,
        data: &mut Rose,
        handle: &mut PointerInnerHandle<'_, Rose>,
        event: &GestureSwipeBeginEvent,
    ) {
        handle.gesture_swipe_begin(data, event)
    }

    fn gesture_swipe_update(
        &mut self,
        data: &mut Rose,
        handle: &mut PointerInnerHandle<'_, Rose>,
        event: &GestureSwipeUpdateEvent,
    ) {
        handle.gesture_swipe_update(data, event)
    }

    fn gesture_swipe_end(
        &mut self,
        data: &mut Rose,
        handle: &mut PointerInnerHandle<'_, Rose>,
        event: &GestureSwipeEndEvent,
    ) {
        handle.gesture_swipe_end(data, event)
    }

    fn gesture_pinch_begin(
        &mut self,
        data: &mut Rose,
        handle: &mut PointerInnerHandle<'_, Rose>,
        event: &GesturePinchBeginEvent,
    ) {
        handle.gesture_pinch_begin(data, event)
    }

    fn gesture_pinch_update(
        &mut self,
        data: &mut Rose,
        handle: &mut PointerInnerHandle<'_, Rose>,
        event: &GesturePinchUpdateEvent,
    ) {
        handle.gesture_pinch_update(data, event)
    }

    fn gesture_pinch_end(
        &mut self,
        data: &mut Rose,
        handle: &mut PointerInnerHandle<'_, Rose>,
        event: &GesturePinchEndEvent,
    ) {
        handle.gesture_pinch_end(data, event)
    }

    fn gesture_hold_begin(
        &mut self,
        data: &mut Rose,
        handle: &mut PointerInnerHandle<'_, Rose>,
        event: &GestureHoldBeginEvent,
    ) {
        handle.gesture_hold_begin(data, event)
    }

    fn gesture_hold_end(
        &mut self,
        data: &mut Rose,
        handle: &mut PointerInnerHandle<'_, Rose>,
        event: &GestureHoldEndEvent,
    ) {
        handle.gesture_hold_end(data, event)
    }

    fn start_data(&self) -> &PointerGrabStartData<Rose> {
        &self.start_data
    }

    fn unset(&mut self, data: &mut Rose) {
        if let GrabKind::Resize { last_size, .. } = self.kind {
            let toplevel = self.window.toplevel().unwrap();
            toplevel.with_pending_state(|state| {
                state.states.unset(xdg_toplevel::State::Resizing);
                state.size = Some(last_size);
            });
            toplevel.send_pending_configure();
            if let Some(anchor) = data.view_mut(&self.window).and_then(|v| v.resize.as_mut()) {
                anchor.finished = true;
            }
        }
        if matches!(data.mode, Mode::Move | Mode::Resize) {
            data.mode = Mode::Normal;
        }
    }
}
