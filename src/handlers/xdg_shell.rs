use smithay::{
    delegate_xdg_decoration, delegate_xdg_shell,
    desktop::{find_popup_root_surface, get_popup_toplevel_coords, PopupKind, Window},
    input::{pointer::GrabStartData as PointerGrabStartData, Seat},
    reexports::{
        wayland_protocols::xdg::{
            decoration::zv1::server::zxdg_toplevel_decoration_v1::Mode as DecorationMode,
            shell::server::xdg_toplevel,
        },
        wayland_server::{
            protocol::{wl_seat, wl_surface::WlSurface},
            Resource,
        },
    },
    utils::Serial,
    wayland::shell::xdg::{
        decoration::XdgDecorationHandler, PopupSurface, PositionerState, ToplevelSurface,
        XdgShellHandler, XdgShellState,
    },
};

use crate::Rose;

impl XdgShellHandler for Rose {
    fn xdg_shell_state(&mut self) -> &mut XdgShellState {
        &mut self.xdg_shell_state
    }

    fn new_toplevel(&mut self, surface: ToplevelSurface) {
        // Rose draws borders, so ask clients not to draw their own decorations.
        surface.with_pending_state(|state| {
            state.decoration_mode = Some(DecorationMode::ServerSide);
        });
        // The view is placed and shown once its first buffer arrives.
        self.add_view(Window::new_wayland_window(surface));
    }

    fn toplevel_destroyed(&mut self, surface: ToplevelSurface) {
        if let Some(window) = self.window_for_surface(surface.wl_surface()) {
            self.remove_view(&window);
        }
    }

    fn new_popup(&mut self, surface: PopupSurface, _positioner: PositionerState) {
        self.unconstrain_popup(&surface);
        let _ = self.popups.track_popup(PopupKind::Xdg(surface));
    }

    fn reposition_request(
        &mut self,
        surface: PopupSurface,
        positioner: PositionerState,
        token: u32,
    ) {
        surface.with_pending_state(|state| {
            state.geometry = positioner.get_geometry();
            state.positioner = positioner;
        });
        self.unconstrain_popup(&surface);
        surface.send_repositioned(token);
    }

    fn move_request(&mut self, surface: ToplevelSurface, seat: wl_seat::WlSeat, serial: Serial) {
        let seat = Seat::from_resource(&seat).unwrap();
        let Some(start_data) = check_grab(&seat, surface.wl_surface(), serial) else {
            return;
        };
        if let Some(window) = self.window_for_surface(surface.wl_surface()) {
            self.start_move(window, start_data, serial, true);
        }
    }

    fn resize_request(
        &mut self,
        surface: ToplevelSurface,
        seat: wl_seat::WlSeat,
        serial: Serial,
        edges: xdg_toplevel::ResizeEdge,
    ) {
        let seat = Seat::from_resource(&seat).unwrap();
        let Some(start_data) = check_grab(&seat, surface.wl_surface(), serial) else {
            return;
        };
        if let Some(window) = self.window_for_surface(surface.wl_surface()) {
            self.start_resize(window, edges.into(), start_data, serial, true);
        }
    }

    fn maximize_request(&mut self, surface: ToplevelSurface) {
        match self.window_for_surface(surface.wl_surface()) {
            Some(window) if self.view(&window).is_some_and(|v| v.placed) => {
                self.set_maximized(&window, true)
            }
            _ => {
                if surface.is_initial_configure_sent() {
                    surface.send_configure();
                }
            }
        }
    }

    fn unmaximize_request(&mut self, surface: ToplevelSurface) {
        if let Some(window) = self.window_for_surface(surface.wl_surface()) {
            self.set_maximized(&window, false);
        }
    }

    fn grab(&mut self, _surface: PopupSurface, _seat: wl_seat::WlSeat, _serial: Serial) {
        // Popup grabs are not supported.
    }
}

delegate_xdg_shell!(Rose);

impl XdgDecorationHandler for Rose {
    fn new_decoration(&mut self, toplevel: ToplevelSurface) {
        set_server_side(&toplevel);
    }

    fn request_mode(&mut self, toplevel: ToplevelSurface, _mode: DecorationMode) {
        set_server_side(&toplevel);
    }

    fn unset_mode(&mut self, toplevel: ToplevelSurface) {
        set_server_side(&toplevel);
    }
}

delegate_xdg_decoration!(Rose);

fn set_server_side(toplevel: &ToplevelSurface) {
    toplevel.with_pending_state(|state| {
        state.decoration_mode = Some(DecorationMode::ServerSide);
    });
    if toplevel.is_initial_configure_sent() {
        toplevel.send_pending_configure();
    }
}

/// Returns the pointer grab data if `surface` is allowed to start an interactive grab with `serial`.
fn check_grab(
    seat: &Seat<Rose>,
    surface: &WlSurface,
    serial: Serial,
) -> Option<PointerGrabStartData<Rose>> {
    let pointer = seat.get_pointer()?;

    // The client must hold an implicit grab from a button press.
    if !pointer.has_grab(serial) {
        return None;
    }

    let start_data = pointer.grab_start_data()?;
    let (focus, _) = start_data.focus.as_ref()?;
    if !focus.id().same_client_as(&surface.id()) {
        return None;
    }

    Some(start_data)
}

/// Sends initial configures, places new views and tracks resizes. Called on every `wl_surface.commit`.
pub fn handle_commit(state: &mut Rose, surface: &WlSurface) {
    if let Some(window) = state.window_for_surface(surface) {
        let toplevel = window.toplevel().unwrap();
        if !toplevel.is_initial_configure_sent() {
            toplevel.send_configure();
        } else if !state.view(&window).is_some_and(|v| v.placed) {
            if !window.geometry().size.is_empty() {
                state.place_view(&window);
            }
        } else {
            state.handle_resize_commit(&window);
        }
    }

    state.popups.commit(surface);
    if let Some(PopupKind::Xdg(ref xdg)) = state.popups.find_popup(surface) {
        if !xdg.is_initial_configure_sent() {
            // The initial configure is always allowed, so this cannot fail.
            xdg.send_configure().expect("initial configure failed");
        }
    }
}

impl Rose {
    /// Keeps popups inside the output so menus don't open off-screen.
    fn unconstrain_popup(&self, popup: &PopupSurface) {
        let Ok(root) = find_popup_root_surface(&PopupKind::Xdg(popup.clone())) else {
            return;
        };
        let Some(window) = self.window_for_surface(&root) else {
            return;
        };
        let Some(output) = self.space.outputs().next() else {
            return;
        };
        let output_geo = self.space.output_geometry(output).unwrap();
        let window_geo = self.geometry(&window);

        // The target rectangle is relative to the popup's parent.
        let mut target = output_geo;
        target.loc -= get_popup_toplevel_coords(&PopupKind::Xdg(popup.clone()));
        target.loc -= window_geo.loc;

        popup.with_pending_state(|state| {
            state.geometry = state.positioner.get_unconstrained_geometry(target);
        });
    }
}
