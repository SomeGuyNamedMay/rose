use std::{
    collections::{HashMap, HashSet},
    ffi::OsString,
    path::PathBuf,
    sync::Arc,
};

use smithay::{
    desktop::{PopupManager, Space, Window, WindowSurfaceType},
    input::{keyboard::XkbConfig, pointer::CursorImageStatus, Seat, SeatState},
    reexports::{
        calloop::{
            channel::{self, Event as ChannelEvent},
            generic::Generic,
            EventLoop, Interest, LoopHandle, LoopSignal, Mode, PostAction, RegistrationToken,
        },
        wayland_server::{
            backend::{ClientData, ClientId, DisconnectReason},
            protocol::wl_surface::WlSurface,
            Display, DisplayHandle,
        },
    },
    utils::{Logical, Point},
    wayland::{
        compositor::{CompositorClientState, CompositorState},
        dmabuf::{DmabufGlobal, DmabufState},
        output::OutputManagerState,
        selection::data_device::DataDeviceState,
        shell::xdg::{decoration::XdgDecorationState, XdgShellState},
        shm::ShmState,
        socket::ListeningSocketSource,
        viewporter::ViewporterState,
    },
};

use crate::{
    action::Direction,
    bar::Bar,
    config::Config,
    cursor::Cursors,
    menu::Menu,
    tty::Tty,
    wallpaper::Wallpaper,
    wm::{Mode as WmMode, SheetLayout, View},
    xwayland::Xwayland,
};

pub struct Rose {
    pub start_time: std::time::Instant,
    pub socket_name: OsString,
    pub display_handle: DisplayHandle,
    pub loop_signal: LoopSignal,
    pub loop_handle: LoopHandle<'static, Rose>,

    pub config: Config,
    pub config_path: PathBuf,

    /// Visible views. Hidden views are unmapped from the space but kept in `views`.
    pub space: Space<Window>,
    pub popups: PopupManager,

    /// Every view, ordered bottom to top.
    pub views: Vec<View>,
    pub next_view_id: u64,
    pub focused: Option<Window>,
    pub current_sheet: u8,
    pub alternate_sheet: u8,
    pub sheet_layouts: [Option<SheetLayout>; 10],
    pub marks: HashMap<char, Window>,
    pub mode: WmMode,
    /// How far the view of the plane has moved from where it started.
    pub camera: Point<i32, Logical>,
    /// Keys held in pan mode, with the direction each one pans.
    pub pan_keys: Vec<(u32, Direction)>,
    /// Repeats panning while keys are held.
    pub pan_timer: Option<RegistrationToken>,
    /// Keys and buttons whose press was consumed by a binding, so their release is consumed too.
    pub suppressed_keys: HashSet<u32>,
    pub suppressed_buttons: HashSet<u32>,
    pub cursor_status: CursorImageStatus,
    /// Set by the first `quit`; the compositor exits once all views have closed.
    pub quit_requested: bool,
    pub bar: Bar,
    pub menu: Option<Menu>,
    pub wallpaper: Wallpaper,
    /// Where the file manager was last closed.
    pub files_dir: Option<PathBuf>,
    /// Present while Rose is serving an X display through xwayland-satellite.
    pub xwayland: Option<Xwayland>,

    pub compositor_state: CompositorState,
    pub xdg_shell_state: XdgShellState,
    pub xdg_decoration_state: XdgDecorationState,
    pub shm_state: ShmState,
    pub output_manager_state: OutputManagerState,
    pub seat_state: SeatState<Rose>,
    pub data_device_state: DataDeviceState,
    pub viewporter_state: ViewporterState,
    pub dmabuf_state: DmabufState,
    pub dmabuf_global: Option<DmabufGlobal>,

    /// Present when running on the hardware rather than nested.
    pub tty: Option<Tty>,
    /// Present when Rose draws named cursors itself.
    pub cursors: Option<Cursors>,

    pub seat: Seat<Self>,
}

impl Rose {
    pub fn new(
        event_loop: &mut EventLoop<'static, Self>,
        display: Display<Self>,
        config: Config,
        config_path: PathBuf,
    ) -> Self {
        let dh = display.handle();

        let compositor_state = CompositorState::new::<Self>(&dh);
        let xdg_shell_state = XdgShellState::new::<Self>(&dh);
        let xdg_decoration_state = XdgDecorationState::new::<Self>(&dh);
        let shm_state = ShmState::new::<Self>(&dh, vec![]);
        let output_manager_state = OutputManagerState::new_with_xdg_output::<Self>(&dh);
        let data_device_state = DataDeviceState::new::<Self>(&dh);
        let viewporter_state = ViewporterState::new::<Self>(&dh);

        // A single seat with a keyboard and pointer that are assumed to always be present.
        let mut seat_state = SeatState::new();
        let mut seat: Seat<Self> = seat_state.new_wl_seat(&dh, "seat0");
        let keyboard = &config.keyboard;
        if let Err(err) = seat.add_keyboard(
            keyboard.xkb_config(),
            keyboard.repeat_delay,
            keyboard.repeat_rate,
        ) {
            tracing::error!("invalid xkb config, using the default keymap: {err:?}");
            seat.add_keyboard(
                XkbConfig::default(),
                keyboard.repeat_delay,
                keyboard.repeat_rate,
            )
            .expect("failed to add keyboard");
        }
        seat.add_pointer();

        let socket_name = Self::init_wayland_listener(display, event_loop);
        tracing::info!(
            "listening on WAYLAND_DISPLAY={}",
            socket_name.to_string_lossy()
        );

        // Lines printed by the bar's status command.
        let (status_sender, status_channel) = channel::channel();
        event_loop
            .handle()
            .insert_source(status_channel, |event, _, state: &mut Self| {
                if let ChannelEvent::Msg((generation, line)) = event {
                    state.bar.set_status(generation, line);
                }
            })
            .expect("failed to insert the status channel");
        let mut bar = Bar::new(status_sender);
        bar.configure(&config.bar);

        // Wallpapers decoded on another thread.
        let (wallpaper_sender, wallpaper_channel) = channel::channel();
        event_loop
            .handle()
            .insert_source(wallpaper_channel, |event, _, state: &mut Self| {
                if let ChannelEvent::Msg((generation, frames)) = event {
                    state.wallpaper.set_frames(generation, frames);
                }
            })
            .expect("failed to insert the wallpaper channel");

        // Wallpapers from applications, through the portal.
        let (portal_sender, portal_channel) = channel::channel();
        event_loop
            .handle()
            .insert_source(portal_channel, |event, _, state: &mut Self| {
                if let ChannelEvent::Msg(request) = event {
                    state.handle_portal_request(request);
                }
            })
            .expect("failed to insert the portal channel");
        crate::portal::spawn(portal_sender);

        let mut state = Self {
            start_time: std::time::Instant::now(),
            socket_name,
            display_handle: dh,
            loop_signal: event_loop.get_signal(),
            loop_handle: event_loop.handle(),

            config,
            config_path,

            space: Space::default(),
            popups: PopupManager::default(),

            views: Vec::new(),
            next_view_id: 0,
            focused: None,
            current_sheet: 1,
            alternate_sheet: 1,
            sheet_layouts: Default::default(),
            marks: HashMap::new(),
            mode: WmMode::Normal,
            camera: Point::default(),
            pan_keys: Vec::new(),
            pan_timer: None,
            suppressed_keys: HashSet::new(),
            suppressed_buttons: HashSet::new(),
            cursor_status: CursorImageStatus::default_named(),
            quit_requested: false,
            bar,
            menu: None,
            wallpaper: Wallpaper::new(wallpaper_sender),
            files_dir: None,
            xwayland: None,

            compositor_state,
            xdg_shell_state,
            xdg_decoration_state,
            shm_state,
            output_manager_state,
            seat_state,
            data_device_state,
            viewporter_state,
            dmabuf_state: DmabufState::new(),
            dmabuf_global: None,

            tty: None,
            cursors: None,

            seat,
        };
        state.wallpaper.configure(state.wallpaper_path());
        state
    }

    fn init_wayland_listener(display: Display<Self>, event_loop: &mut EventLoop<Self>) -> OsString {
        // Picks the next free `wayland-N` socket name.
        let listening_socket =
            ListeningSocketSource::new_auto().expect("failed to create wayland socket");
        let socket_name = listening_socket.socket_name().to_os_string();

        let loop_handle = event_loop.handle();

        loop_handle
            .insert_source(listening_socket, |client_stream, _, state| {
                state
                    .display_handle
                    .insert_client(client_stream, Arc::new(ClientState::default()))
                    .unwrap();
            })
            .expect("failed to init the wayland event source");

        // Dispatch client requests whenever the display fd becomes readable.
        loop_handle
            .insert_source(
                Generic::new(display, Interest::READ, Mode::Level),
                |_, display, state| {
                    // Safety: the display is never dropped while the source is alive.
                    unsafe {
                        display.get_mut().dispatch_clients(state).unwrap();
                    }
                    Ok(PostAction::Continue)
                },
            )
            .expect("failed to insert display source");

        socket_name
    }

    /// Finds the surface under `pos` along with that surface's location in global coordinates.
    pub fn surface_under(
        &self,
        pos: Point<f64, Logical>,
    ) -> Option<(WlSurface, Point<f64, Logical>)> {
        if self.over_panels(pos) {
            return None;
        }
        self.space
            .element_under(pos)
            .and_then(|(window, location)| {
                window
                    .surface_under(pos - location.to_f64(), WindowSurfaceType::ALL)
                    .map(|(s, p)| (s, (p + location).to_f64()))
            })
    }

    pub fn window_for_surface(&self, surface: &WlSurface) -> Option<Window> {
        self.views
            .iter()
            .find(|v| v.window.toplevel().unwrap().wl_surface() == surface)
            .map(|v| v.window.clone())
    }
}

#[derive(Default)]
pub struct ClientState {
    pub compositor_state: CompositorClientState,
}

impl ClientData for ClientState {
    fn initialized(&self, _client_id: ClientId) {}
    fn disconnected(&self, _client_id: ClientId, _reason: DisconnectReason) {}
}
