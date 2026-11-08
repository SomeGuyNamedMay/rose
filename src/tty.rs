//! Runs the compositor directly on the hardware, from a VT or a display manager: DRM for the
//! screen, libinput for input and libseat for access to both. Rose uses one GPU and one screen.

use std::{
    error::Error,
    path::{Path, PathBuf},
    time::Duration,
};

use smithay::{
    backend::{
        allocator::{
            dmabuf::Dmabuf,
            gbm::{GbmAllocator, GbmBufferFlags, GbmDevice},
            Fourcc,
        },
        drm::{
            compositor::FrameFlags,
            exporter::gbm::GbmFramebufferExporter,
            output::{DrmOutput, DrmOutputManager, DrmOutputRenderElements},
            DrmDevice, DrmDeviceFd, DrmEvent, DrmNode, NodeType,
        },
        egl::{EGLContext, EGLDevice, EGLDisplay},
        input::InputEvent,
        libinput::{LibinputInputBackend, LibinputSessionInterface},
        renderer::{gles::GlesRenderer, ImportDma, ImportMemWl},
        session::{libseat::LibSeatSession, Event as SessionEvent, Session},
        udev::{all_gpus, primary_gpu, UdevBackend, UdevEvent},
    },
    input::keyboard::LedState,
    output::{Mode, Output, PhysicalProperties, Subpixel},
    reexports::{
        calloop::{
            timer::{TimeoutAction, Timer},
            EventLoop, LoopHandle, RegistrationToken,
        },
        drm::control::{connector, crtc, Device as _, ModeTypeFlags},
        input::{Device as InputDevice, DeviceCapability, Libinput},
        rustix::fs::OFlags,
        wayland_server::backend::GlobalId,
    },
    utils::DeviceFd,
    wayland::dmabuf::{DmabufFeedbackBuilder, ImportNotifier},
};

use crate::{cursor::Cursors, render::RoseRenderElement, Rose};

const COLOR_FORMATS: [Fourcc; 2] = [Fourcc::Abgr8888, Fourcc::Argb8888];

type Allocator = GbmAllocator<DrmDeviceFd>;
type Exporter = GbmFramebufferExporter<DrmDeviceFd>;

struct Screen {
    connector: connector::Handle,
    crtc: crtc::Handle,
    output: Output,
    global: GlobalId,
    drm_output: DrmOutput<Allocator, Exporter, (), DrmDeviceFd>,
}

pub struct Tty {
    session: LibSeatSession,
    libinput: Libinput,
    pub renderer: GlesRenderer,
    node: DrmNode,
    manager: DrmOutputManager<Allocator, Exporter, (), DrmDeviceFd>,
    screen: Option<Screen>,
    keyboards: Vec<InputDevice>,
    handle: LoopHandle<'static, Rose>,
    /// A frame was queued, and the next vblank will start the following one.
    frame_pending: bool,
    /// Starts the next frame when there is no vblank to wait for.
    retry: Option<RegistrationToken>,
}

/// Whether a `/dev/dri/cardN` device has a connected display, going by sysfs so that it doesn't
/// have to be opened.
fn has_display(path: &Path) -> bool {
    let Some(prefix) = path
        .file_name()
        .map(|n| format!("{}-", n.to_string_lossy()))
    else {
        return false;
    };
    let Ok(entries) = std::fs::read_dir("/sys/class/drm") else {
        return false;
    };
    entries.flatten().any(|entry| {
        entry.file_name().to_string_lossy().starts_with(&prefix)
            && std::fs::read_to_string(entry.path().join("status"))
                .is_ok_and(|status| status.trim() == "connected")
    })
}

/// The primary GPU if it drives a display, or else the first one that does.
fn choose_gpu(seat: &str) -> Result<PathBuf, Box<dyn Error>> {
    let mut gpus: Vec<PathBuf> = primary_gpu(seat)?.into_iter().collect();
    gpus.extend(all_gpus(seat)?);
    let chosen = gpus.iter().find(|gpu| has_display(gpu)).or(gpus.first());
    Ok(chosen.ok_or("no GPU found")?.clone())
}

pub fn init_tty(
    event_loop: &mut EventLoop<'static, Rose>,
    state: &mut Rose,
) -> Result<(), Box<dyn Error>> {
    let (mut session, notifier) = LibSeatSession::new()?;
    let seat = session.seat();

    let path = choose_gpu(&seat)?;
    tracing::info!("using {} for the display", path.display());
    let node = DrmNode::from_path(&path)?;
    let fd = session.open(
        &path,
        OFlags::RDWR | OFlags::CLOEXEC | OFlags::NOCTTY | OFlags::NONBLOCK,
    )?;
    let fd = DrmDeviceFd::new(DeviceFd::from(fd));
    let (drm, drm_notifier) = DrmDevice::new(fd.clone(), true)?;
    let gbm = GbmDevice::new(fd)?;

    // Safety: the display isn't used from other threads.
    let display = unsafe { EGLDisplay::new(gbm.clone())? };
    let egl_device = EGLDevice::device_for_display(&display).ok();
    let software = egl_device.as_ref().is_some_and(|d| d.is_software());
    let render_node = egl_device
        .and_then(|d| d.try_get_render_node().ok().flatten())
        .or_else(|| node.node_with_type(NodeType::Render)?.ok())
        .unwrap_or(node);
    let context = EGLContext::new(&display)?;
    let render_formats = context.dmabuf_render_formats().clone();
    // Safety: the context is only used from this thread.
    let renderer = unsafe { GlesRenderer::new(context)? };

    state.shm_state.update_formats(renderer.shm_formats());

    // Clients can only share buffers with a real GPU; on a software renderer they fall back to shm.
    if !software {
        let feedback =
            DmabufFeedbackBuilder::new(render_node.dev_id(), renderer.dmabuf_formats()).build()?;
        state.dmabuf_global = Some(
            state
                .dmabuf_state
                .create_global_with_default_feedback::<Rose>(&state.display_handle, &feedback),
        );
    }

    let manager = DrmOutputManager::new(
        drm,
        GbmAllocator::new(
            gbm.clone(),
            GbmBufferFlags::RENDERING | GbmBufferFlags::SCANOUT,
        ),
        GbmFramebufferExporter::new(gbm.clone(), Some(render_node)),
        Some(gbm),
        COLOR_FORMATS,
        render_formats,
    );

    let mut libinput =
        Libinput::new_with_udev::<LibinputSessionInterface<LibSeatSession>>(session.clone().into());
    libinput
        .udev_assign_seat(&seat)
        .map_err(|()| "failed to assign the seat to libinput")?;

    let handle = event_loop.handle();
    handle.insert_source(
        LibinputInputBackend::new(libinput.clone()),
        |event, _, state| {
            if let Some(tty) = &mut state.tty {
                tty.track_keyboards(&event);
            }
            state.process_input_event(event);
        },
    )?;
    handle.insert_source(notifier, |event, _, state| match event {
        SessionEvent::PauseSession => with_tty(state, Tty::pause),
        SessionEvent::ActivateSession => with_tty(state, Tty::activate),
    })?;
    handle.insert_source(drm_notifier, |event, _, state| match event {
        DrmEvent::VBlank(crtc) => with_tty(state, |tty, state| tty.vblank(crtc, state)),
        DrmEvent::Error(err) => tracing::error!("DRM error: {err:?}"),
    })?;

    // Screens being plugged in or removed.
    let device = node.dev_id();
    handle.insert_source(UdevBackend::new(&seat)?, move |event, _, state| {
        if matches!(event, UdevEvent::Changed { device_id } if device_id == device) {
            with_tty(state, Tty::update_screen);
        }
    })?;

    let mut tty = Tty {
        session,
        libinput,
        renderer,
        node,
        manager,
        screen: None,
        keyboards: Vec::new(),
        handle,
        frame_pending: false,
        retry: None,
    };
    state.cursors = Some(Cursors::new());
    tty.update_screen(state);
    state.tty = Some(tty);
    state.center_pointer();

    // Safety: set before any other threads are spawned for clients.
    unsafe {
        std::env::set_var("WAYLAND_DISPLAY", &state.socket_name);
        std::env::set_var("XDG_SESSION_TYPE", "wayland");
        if std::env::var_os("XDG_CURRENT_DESKTOP").is_none() {
            std::env::set_var("XDG_CURRENT_DESKTOP", "rose");
        }
    }

    state.tty_render_soon();
    Ok(())
}

/// Calls `f` with the backend taken out of the state, so that it can use both.
fn with_tty(state: &mut Rose, f: impl FnOnce(&mut Tty, &mut Rose)) {
    if let Some(mut tty) = state.tty.take() {
        f(&mut tty, state);
        state.tty = Some(tty);
    }
}

impl Tty {
    fn track_keyboards(&mut self, event: &InputEvent<LibinputInputBackend>) {
        match event {
            InputEvent::DeviceAdded { device }
                if device.has_capability(DeviceCapability::Keyboard) =>
            {
                self.keyboards.push(device.clone());
            }
            InputEvent::DeviceRemoved { device } => self.keyboards.retain(|d| d != device),
            _ => {}
        }
    }

    pub fn update_leds(&mut self, leds: LedState) {
        for keyboard in &mut self.keyboards {
            keyboard.led_update(leds.into());
        }
    }

    pub fn switch_vt(&mut self, vt: i32) {
        if let Err(err) = self.session.change_vt(vt) {
            tracing::warn!("failed to switch to VT {vt}: {err}");
        }
    }

    pub fn import_dmabuf(&mut self, dmabuf: &Dmabuf, notifier: ImportNotifier) {
        if self.renderer.import_dmabuf(dmabuf, None).is_ok() {
            dmabuf.set_node(self.node);
            let _ = notifier.successful::<Rose>();
        } else {
            notifier.failed();
        }
    }

    fn stop_retry(&mut self) {
        if let Some(token) = self.retry.take() {
            self.handle.remove(token);
        }
    }

    fn pause(&mut self, state: &mut Rose) {
        state.stop_pan_repeat();
        tracing::info!("pausing the session");
        self.libinput.suspend();
        self.manager.pause();
        self.stop_retry();
        self.frame_pending = false;
    }

    fn activate(&mut self, state: &mut Rose) {
        tracing::info!("resuming the session");
        if self.libinput.resume().is_err() {
            tracing::error!("failed to resume libinput");
        }
        if let Err(err) = self.manager.activate(false) {
            tracing::error!("failed to resume DRM: {err}");
        }
        self.update_screen(state);
        self.render(state);
    }

    fn vblank(&mut self, crtc: crtc::Handle, state: &mut Rose) {
        let Some(screen) = self.screen.as_mut().filter(|s| s.crtc == crtc) else {
            return;
        };
        if let Err(err) = screen.drm_output.frame_submitted() {
            tracing::warn!("failed to finish the frame: {err:?}");
        }
        self.frame_pending = false;
        self.render(state);
    }

    /// Follows the screen: keeps the current one while it's connected, and otherwise moves to
    /// the first one that is.
    fn update_screen(&mut self, state: &mut Rose) {
        let device = self.manager.device();
        let Ok(resources) = device.resource_handles() else {
            return;
        };
        let connected: Vec<_> = resources
            .connectors()
            .iter()
            .filter_map(|c| device.get_connector(*c, true).ok())
            .filter(|c| c.state() == connector::State::Connected && !c.modes().is_empty())
            .collect();
        if let Some(screen) = &self.screen {
            if connected.iter().any(|c| c.handle() == screen.connector) {
                return;
            }
            state.space.unmap_output(&screen.output);
            state
                .display_handle
                .remove_global::<Rose>(screen.global.clone());
            self.screen = None;
            self.frame_pending = false;
        }

        let chosen = connected.iter().find_map(|info| {
            let crtc = info.encoders().iter().find_map(|encoder| {
                let encoder = device.get_encoder(*encoder).ok()?;
                resources
                    .filter_crtcs(encoder.possible_crtcs())
                    .first()
                    .copied()
            })?;
            Some((info, crtc))
        });
        let Some((info, crtc)) = chosen else {
            tracing::warn!("no screen connected");
            state.arrange_all();
            return;
        };

        let drm_mode = info
            .modes()
            .iter()
            .find(|m| m.mode_type().contains(ModeTypeFlags::PREFERRED))
            .copied()
            .unwrap_or(info.modes()[0]);
        let mode = Mode::from(drm_mode);
        let (width, height) = info.size().unwrap_or((0, 0));
        let name = format!("{:?}-{}", info.interface(), info.interface_id());
        tracing::info!("using screen {name} at {}x{}", mode.size.w, mode.size.h);

        let output = Output::new(
            name,
            PhysicalProperties {
                size: (width as i32, height as i32).into(),
                subpixel: Subpixel::Unknown,
                make: "Unknown".into(),
                model: "Unknown".into(),
            },
        );
        let global = output.create_global::<Rose>(&state.display_handle);
        output.set_preferred(mode);
        output.change_current_state(Some(mode), None, None, Some((0, 0).into()));

        let planes = device.planes(&crtc).ok();
        let connector = info.handle();
        let drm_output = self.manager.initialize_output::<_, RoseRenderElement>(
            crtc,
            drm_mode,
            &[connector],
            &output,
            planes,
            &mut self.renderer,
            &DrmOutputRenderElements::default(),
        );
        let drm_output = match drm_output {
            Ok(drm_output) => drm_output,
            Err(err) => {
                tracing::error!("failed to set up the screen: {err:?}");
                state.display_handle.remove_global::<Rose>(global);
                return;
            }
        };

        state.space.map_output(&output, (0, 0));
        state.arrange_all();
        self.screen = Some(Screen {
            connector,
            crtc,
            output,
            global,
            drm_output,
        });
    }

    fn render(&mut self, state: &mut Rose) {
        if self.frame_pending || !self.session.is_active() {
            return;
        }
        self.stop_retry();
        let Some(screen) = &mut self.screen else {
            return;
        };

        let background = state.config.ui.colorscheme.background.0;
        let elements = state.render_elements(&mut self.renderer);
        let frame = screen.drm_output.render_frame(
            &mut self.renderer,
            &elements,
            background,
            FrameFlags::DEFAULT,
        );
        // With nothing new on screen there won't be a vblank event.
        let needs_queue = match frame {
            Ok(frame) => !frame.is_empty,
            Err(err) => {
                tracing::warn!("failed to render: {err:?}");
                false
            }
        };
        if needs_queue {
            match screen.drm_output.queue_frame(()) {
                Ok(()) => self.frame_pending = true,
                Err(err) => tracing::warn!("failed to queue the frame: {err:?}"),
            }
        }
        let output = screen.output.clone();
        if !self.frame_pending {
            self.retry_after(frame_time(&output));
        }
        state.frame_done(&output);
    }

    fn retry_after(&mut self, delay: Duration) {
        let source = Timer::from_duration(delay);
        self.retry = self
            .handle
            .insert_source(source, |_, _, state| {
                with_tty(state, |tty, state| {
                    tty.retry = None;
                    tty.render(state);
                });
                TimeoutAction::Drop
            })
            .map_err(|err| tracing::error!("failed to schedule a frame: {err}"))
            .ok();
    }
}

fn frame_time(output: &Output) -> Duration {
    let refresh = output
        .current_mode()
        .map_or(60_000, |m| m.refresh)
        .max(1_000);
    Duration::from_secs_f64(1_000.0 / refresh as f64)
}

impl Rose {
    fn tty_render_soon(&mut self) {
        if let Some(tty) = &self.tty {
            tty.handle.insert_idle(|state| with_tty(state, Tty::render));
        }
    }
}

/// Tells systemd and D-Bus about the session, so services like portals can reach it, and starts
/// the units that are waiting for it.
pub fn announce_session() {
    let variables: Vec<_> = [
        "WAYLAND_DISPLAY",
        "DISPLAY",
        "XDG_CURRENT_DESKTOP",
        "XDG_SESSION_TYPE",
    ]
    .into_iter()
    .filter(|v| std::env::var_os(v).is_some())
    .collect();
    crate::wm::spawn(&format!(
        "dbus-update-activation-environment --systemd {}; \
         systemctl --user start rose-session.target 2>/dev/null",
        variables.join(" ")
    ));
}

pub fn end_session() {
    let _ = std::process::Command::new("systemctl")
        .args(["--user", "stop", "rose-session.target"])
        .stderr(std::process::Stdio::null())
        .status();
}
