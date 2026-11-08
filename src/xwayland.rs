//! X11 support through xwayland-satellite, started on demand the way niri does it.
//!
//! Rose claims an X display itself, setting `DISPLAY` for its children, and starts
//! xwayland-satellite on the first connection, handing it the listening sockets. If satellite
//! exits, Rose listens again and starts it on the next connection.

use std::{
    fs,
    io::Write,
    os::{
        fd::{AsRawFd, BorrowedFd, RawFd},
        linux::net::SocketAddrExt,
        unix::{
            fs::{OpenOptionsExt, PermissionsExt},
            net::{SocketAddr, UnixListener},
            process::CommandExt,
        },
    },
    path::PathBuf,
    process::{Command, ExitStatus, Stdio},
    time::{Duration, Instant},
};

use smithay::reexports::calloop::{
    channel::{self, Event as ChannelEvent},
    generic::Generic,
    EventLoop, Interest, LoopHandle, Mode, PostAction, RegistrationToken,
};

use crate::Rose;

/// Satellite exiting with an error sooner than this after starting is taken as it being unable
/// to run, rather than as a crash, so it isn't restarted.
const MIN_RUN_TIME: Duration = Duration::from_secs(1);

pub struct Xwayland {
    loop_handle: LoopHandle<'static, Rose>,
    satellite: String,
    display: u32,
    lock_path: PathBuf,
    socket_path: PathBuf,
    /// The abstract and filesystem sockets, owned by their event sources.
    fds: Vec<RawFd>,
    tokens: Vec<RegistrationToken>,
    exit_sender: channel::Sender<(ExitStatus, Duration)>,
    running: bool,
}

/// Claims an X display and listens on it if xwayland-satellite is available.
pub fn init(event_loop: &mut EventLoop<'static, Rose>, state: &mut Rose) {
    if !state.config.xwayland.enabled {
        return;
    }
    let satellite = state.config.xwayland.satellite.clone();
    if !supports_listenfd(&satellite) {
        return;
    }
    let Some((number, lock_path, socket_path, listeners)) = claim_display() else {
        tracing::warn!("no free X display; X11 apps won't work");
        return;
    };

    let loop_handle = event_loop.handle();
    let mut fds = Vec::new();
    let mut tokens = Vec::new();
    for (index, listener) in listeners.into_iter().enumerate() {
        fds.push(listener.as_raw_fd());
        let token = loop_handle
            .insert_source(
                Generic::new(listener, Interest::READ, Mode::Level),
                move |_, _, state| {
                    start_satellite(state, index);
                    // Satellite accepts the connections from here on.
                    Ok(PostAction::Disable)
                },
            )
            .expect("failed to insert an X11 socket source");
        tokens.push(token);
    }

    let (exit_sender, exit_channel) = channel::channel();
    loop_handle
        .insert_source(exit_channel, |event, _, state: &mut Rose| {
            if let ChannelEvent::Msg((status, ran_for)) = event {
                satellite_exited(state, status, ran_for);
            }
        })
        .expect("failed to insert the xwayland-satellite exit channel");

    tracing::info!("listening on DISPLAY=:{number} for xwayland-satellite");
    // Safety: like `WAYLAND_DISPLAY`, set before clients are spawned.
    unsafe { std::env::set_var("DISPLAY", format!(":{number}")) };

    state.xwayland = Some(Xwayland {
        loop_handle,
        satellite,
        display: number,
        lock_path,
        socket_path,
        fds,
        tokens,
        exit_sender,
        running: false,
    });
}

/// Whether `satellite` exists and is new enough to take its sockets from Rose.
fn supports_listenfd(satellite: &str) -> bool {
    match Command::new(satellite)
        .args([":0", "--test-listenfd-support"])
        .env_remove("DISPLAY")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
    {
        Ok(status) if status.success() => true,
        Ok(_) => {
            tracing::warn!("{satellite} is too old to support -listenfd; X11 apps won't work");
            false
        }
        Err(err) => {
            tracing::info!("{satellite} not available ({err}); X11 apps won't work");
            false
        }
    }
}

/// Takes the first free X display, returning its number, lock file, socket path and the
/// abstract and filesystem listening sockets.
fn claim_display() -> Option<(u32, PathBuf, PathBuf, Vec<UnixListener>)> {
    let socket_dir = PathBuf::from("/tmp/.X11-unix");
    if !socket_dir.exists() {
        if let Err(err) = fs::create_dir(&socket_dir)
            .and_then(|()| fs::set_permissions(&socket_dir, fs::Permissions::from_mode(0o1777)))
        {
            tracing::warn!("failed to create {}: {err}", socket_dir.display());
        }
    }

    for number in 0..33 {
        let lock_path = PathBuf::from(format!("/tmp/.X{number}-lock"));
        if !lock(&lock_path) {
            continue;
        }

        let socket_path = socket_dir.join(format!("X{number}"));
        let listeners = (|| {
            let name = socket_path.as_os_str().as_encoded_bytes();
            let abstract_listener =
                UnixListener::bind_addr(&SocketAddr::from_abstract_name(name)?)?;
            // A leftover socket file is stale, since the lock is ours.
            let _ = fs::remove_file(&socket_path);
            let listener = UnixListener::bind(&socket_path)?;
            std::io::Result::Ok(vec![abstract_listener, listener])
        })();
        match listeners {
            Ok(listeners) => return Some((number, lock_path, socket_path, listeners)),
            Err(err) => {
                tracing::debug!("X display :{number} is taken: {err}");
                let _ = fs::remove_file(&lock_path);
            }
        }
    }
    None
}

/// Creates an X lock file, replacing it if the process that made it is gone.
fn lock(path: &PathBuf) -> bool {
    for _ in 0..2 {
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o444)
            .open(path)
        {
            Ok(mut file) => {
                return writeln!(file, "{:>10}", std::process::id()).is_ok();
            }
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
                let pid = fs::read_to_string(path)
                    .ok()
                    .and_then(|text| text.trim().parse::<u32>().ok());
                let stale = pid.is_some_and(|pid| !PathBuf::from(format!("/proc/{pid}")).exists());
                if !stale || fs::remove_file(path).is_err() {
                    return false;
                }
            }
            Err(_) => return false,
        }
    }
    false
}

fn start_satellite(state: &mut Rose, woken_by: usize) {
    let Some(xwayland) = &mut state.xwayland else {
        return;
    };
    if xwayland.running {
        return;
    }
    // The source that woke us disables itself; the others are disabled here.
    for (index, token) in xwayland.tokens.iter().enumerate() {
        if index != woken_by {
            let _ = xwayland.loop_handle.disable(token);
        }
    }

    let mut command = Command::new(&xwayland.satellite);
    command
        .arg(format!(":{}", xwayland.display))
        .env_remove("DISPLAY")
        .stdin(Stdio::null());
    for fd in &xwayland.fds {
        command.arg("-listenfd").arg(fd.to_string());
    }
    let fds = xwayland.fds.clone();
    // Safety: only calls fcntl, which is async-signal-safe.
    unsafe {
        command.pre_exec(move || {
            // Let satellite inherit the sockets.
            for &fd in &fds {
                rustix::io::fcntl_setfd(BorrowedFd::borrow_raw(fd), rustix::io::FdFlags::empty())?;
            }
            Ok(())
        });
    }

    match command.spawn() {
        Ok(mut child) => {
            tracing::info!(
                "started {} for DISPLAY=:{}",
                xwayland.satellite,
                xwayland.display
            );
            xwayland.running = true;
            let sender = xwayland.exit_sender.clone();
            let started = Instant::now();
            std::thread::spawn(move || {
                if let Ok(status) = child.wait() {
                    let _ = sender.send((status, started.elapsed()));
                }
            });
        }
        Err(err) => {
            tracing::error!("failed to start {}: {err}", xwayland.satellite);
            // Deferred, since a source can't be removed from within its own callback.
            xwayland.loop_handle.insert_idle(stop);
        }
    }
}

fn satellite_exited(state: &mut Rose, status: ExitStatus, ran_for: Duration) {
    let Some(xwayland) = &mut state.xwayland else {
        return;
    };
    xwayland.running = false;
    if !status.success() && ran_for < MIN_RUN_TIME {
        tracing::error!(
            "{} failed right away ({status}); X11 apps won't work",
            xwayland.satellite
        );
        stop(state);
        return;
    }
    tracing::info!(
        "{} exited ({status}); restarting it on the next X11 connection",
        xwayland.satellite
    );
    for token in &xwayland.tokens {
        if let Err(err) = xwayland.loop_handle.enable(token) {
            tracing::warn!(
                "failed to listen on DISPLAY=:{} again: {err}",
                xwayland.display
            );
        }
    }
}

/// Closes the X display so X11 clients fail instead of waiting forever.
fn stop(state: &mut Rose) {
    if let Some(mut xwayland) = state.xwayland.take() {
        for token in xwayland.tokens.drain(..) {
            xwayland.loop_handle.remove(token);
        }
    }
}

impl Drop for Xwayland {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.socket_path);
        let _ = fs::remove_file(&self.lock_path);
    }
}
