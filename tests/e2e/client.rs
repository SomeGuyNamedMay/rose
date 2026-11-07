//! A test client that shows one window filled with a solid color.
//!
//! Left-clicking the window asks the compositor to move it; right-clicking asks it to resize the
//! window from its top-left corner.

use std::{
    fs::File,
    io::Write,
    os::{fd::AsFd, unix::net::UnixStream},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    thread::JoinHandle,
};

use wayland_client::{
    globals::{registry_queue_init, GlobalListContents},
    protocol::{
        wl_buffer::{self, WlBuffer},
        wl_compositor::WlCompositor,
        wl_pointer::{self, WlPointer},
        wl_registry::WlRegistry,
        wl_seat::{self, WlSeat},
        wl_shm::{self, WlShm},
        wl_shm_pool::WlShmPool,
        wl_surface::WlSurface,
    },
    Connection, Dispatch, QueueHandle, WEnum,
};
use wayland_protocols::xdg::shell::client::{
    xdg_surface::{self, XdgSurface},
    xdg_toplevel::{self, XdgToplevel},
    xdg_wm_base::{self, XdgWmBase},
};

const BTN_LEFT: u32 = 0x110;
const BTN_RIGHT: u32 = 0x111;

/// Size used until the compositor picks one.
pub const DEFAULT_SIZE: (i32, i32) = (400, 300);

pub struct Client {
    closed: Arc<AtomicBool>,
    _thread: JoinHandle<()>,
}

impl Client {
    pub fn spawn(socket: &Path, app_id: &str, color: [u8; 3]) -> Self {
        let stream = UnixStream::connect(socket).expect("failed to connect to rose");
        let closed = Arc::new(AtomicBool::new(false));
        let flag = closed.clone();
        let app_id = app_id.to_string();
        let dir = socket.parent().unwrap().to_path_buf();
        let thread = std::thread::spawn(move || run(stream, app_id, color, dir, flag));
        Self {
            closed,
            _thread: thread,
        }
    }

    /// Whether the compositor asked the window to close and it did.
    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }
}

struct State {
    shm: WlShm,
    surface: WlSurface,
    toplevel: XdgToplevel,
    seat: WlSeat,
    color: [u8; 3],
    size: (i32, i32),
    dir: PathBuf,
    app_id: String,
    buffers: u32,
    running: bool,
}

fn run(stream: UnixStream, app_id: String, color: [u8; 3], dir: PathBuf, closed: Arc<AtomicBool>) {
    let conn = Connection::from_socket(stream).unwrap();
    let (globals, mut queue) = registry_queue_init::<State>(&conn).unwrap();
    let qh = queue.handle();
    let compositor: WlCompositor = globals.bind(&qh, 4..=5, ()).unwrap();
    let shm: WlShm = globals.bind(&qh, 1..=1, ()).unwrap();
    let wm_base: XdgWmBase = globals.bind(&qh, 1..=6, ()).unwrap();
    let seat: WlSeat = globals.bind(&qh, 1..=7, ()).unwrap();

    let surface = compositor.create_surface(&qh, ());
    let xdg_surface = wm_base.get_xdg_surface(&surface, &qh, ());
    let toplevel = xdg_surface.get_toplevel(&qh, ());
    toplevel.set_app_id(app_id.clone());
    surface.commit();

    let mut state = State {
        shm,
        surface,
        toplevel,
        seat,
        color,
        size: DEFAULT_SIZE,
        dir,
        app_id,
        buffers: 0,
        running: true,
    };
    while state.running {
        if queue.blocking_dispatch(&mut state).is_err() {
            return;
        }
    }

    state.toplevel.destroy();
    xdg_surface.destroy();
    state.surface.destroy();
    let _ = conn.flush();
    closed.store(true, Ordering::SeqCst);
}

impl State {
    fn draw(&mut self, qh: &QueueHandle<Self>) {
        let (w, h) = self.size;
        let stride = w * 4;
        let len = (stride * h) as usize;
        let [r, g, b] = self.color;
        let data: Vec<u8> = [b, g, r, 0xFF].into_iter().cycle().take(len).collect();

        let path = self
            .dir
            .join(format!("{}-buffer-{}", self.app_id, self.buffers));
        self.buffers += 1;
        let mut file = File::options()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(&path)
            .unwrap();
        file.write_all(&data).unwrap();
        std::fs::remove_file(&path).unwrap();

        let pool = self.shm.create_pool(file.as_fd(), len as i32, qh, ());
        let buffer = pool.create_buffer(0, w, h, stride, wl_shm::Format::Argb8888, qh, ());
        pool.destroy();
        self.surface.attach(Some(&buffer), 0, 0);
        self.surface.damage_buffer(0, 0, w, h);
        self.surface.commit();
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

impl Dispatch<XdgWmBase, ()> for State {
    fn event(
        _: &mut Self,
        base: &XdgWmBase,
        event: xdg_wm_base::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_wm_base::Event::Ping { serial } = event {
            base.pong(serial);
        }
    }
}

impl Dispatch<XdgSurface, ()> for State {
    fn event(
        state: &mut Self,
        xdg_surface: &XdgSurface,
        event: xdg_surface::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let xdg_surface::Event::Configure { serial } = event {
            xdg_surface.ack_configure(serial);
            state.draw(qh);
        }
    }
}

impl Dispatch<XdgToplevel, ()> for State {
    fn event(
        state: &mut Self,
        _: &XdgToplevel,
        event: xdg_toplevel::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            xdg_toplevel::Event::Configure { width, height, .. } if width > 0 && height > 0 => {
                state.size = (width, height);
            }
            xdg_toplevel::Event::Close => state.running = false,
            _ => {}
        }
    }
}

impl Dispatch<WlSeat, ()> for State {
    fn event(
        _: &mut Self,
        seat: &WlSeat,
        event: wl_seat::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_seat::Event::Capabilities {
            capabilities: WEnum::Value(capabilities),
        } = event
        {
            if capabilities.contains(wl_seat::Capability::Pointer) {
                seat.get_pointer(qh, ());
            }
        }
    }
}

impl Dispatch<WlPointer, ()> for State {
    fn event(
        state: &mut Self,
        _: &WlPointer,
        event: wl_pointer::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_pointer::Event::Button {
            serial,
            button,
            state: WEnum::Value(wl_pointer::ButtonState::Pressed),
            ..
        } = event
        {
            match button {
                BTN_LEFT => state.toplevel._move(&state.seat, serial),
                BTN_RIGHT => {
                    state
                        .toplevel
                        .resize(&state.seat, serial, xdg_toplevel::ResizeEdge::TopLeft)
                }
                _ => {}
            }
        }
    }
}

impl Dispatch<WlBuffer, ()> for State {
    fn event(
        _: &mut Self,
        buffer: &WlBuffer,
        event: wl_buffer::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_buffer::Event::Release = event {
            buffer.destroy();
        }
    }
}

wayland_client::delegate_noop!(State: WlCompositor);
wayland_client::delegate_noop!(State: WlShmPool);
wayland_client::delegate_noop!(State: ignore WlShm);
wayland_client::delegate_noop!(State: ignore WlSurface);
