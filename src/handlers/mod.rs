mod compositor;
mod xdg_shell;

use smithay::{
    backend::allocator::dmabuf::Dmabuf,
    delegate_data_device, delegate_dmabuf, delegate_output, delegate_seat,
    input::{keyboard::LedState, pointer::CursorImageStatus, Seat, SeatHandler, SeatState},
    reexports::wayland_server::{protocol::wl_surface::WlSurface, Resource},
    wayland::{
        dmabuf::{DmabufGlobal, DmabufHandler, DmabufState, ImportNotifier},
        output::OutputHandler,
        selection::{
            data_device::{
                set_data_device_focus, ClientDndGrabHandler, DataDeviceHandler, DataDeviceState,
                ServerDndGrabHandler,
            },
            SelectionHandler,
        },
    },
};

use crate::Rose;

// wl_seat

impl SeatHandler for Rose {
    type KeyboardFocus = WlSurface;
    type PointerFocus = WlSurface;
    type TouchFocus = WlSurface;

    fn seat_state(&mut self) -> &mut SeatState<Self> {
        &mut self.seat_state
    }

    fn cursor_image(&mut self, _seat: &Seat<Self>, image: CursorImageStatus) {
        self.cursor_status = image;
    }

    fn led_state_changed(&mut self, _seat: &Seat<Self>, leds: LedState) {
        if let Some(tty) = &mut self.tty {
            tty.update_leds(leds);
        }
    }

    fn focus_changed(&mut self, seat: &Seat<Self>, focused: Option<&WlSurface>) {
        // Clipboard offers follow keyboard focus.
        let dh = &self.display_handle;
        let client = focused.and_then(|s| dh.get_client(s.id()).ok());
        set_data_device_focus(dh, seat, client);
    }
}

delegate_seat!(Rose);

// wl_data_device (clipboard and drag-and-drop)

impl SelectionHandler for Rose {
    type SelectionUserData = ();
}

impl DataDeviceHandler for Rose {
    fn data_device_state(&self) -> &DataDeviceState {
        &self.data_device_state
    }
}

impl ClientDndGrabHandler for Rose {}
impl ServerDndGrabHandler for Rose {}

delegate_data_device!(Rose);

// zwp_linux_dmabuf_v1 (buffers shared with the GPU, on the hardware only)

impl DmabufHandler for Rose {
    fn dmabuf_state(&mut self) -> &mut DmabufState {
        &mut self.dmabuf_state
    }

    fn dmabuf_imported(
        &mut self,
        _global: &DmabufGlobal,
        dmabuf: Dmabuf,
        notifier: ImportNotifier,
    ) {
        match &mut self.tty {
            Some(tty) => tty.import_dmabuf(&dmabuf, notifier),
            None => notifier.failed(),
        }
    }
}

delegate_dmabuf!(Rose);

// wl_output and xdg_output

impl OutputHandler for Rose {}

delegate_output!(Rose);
