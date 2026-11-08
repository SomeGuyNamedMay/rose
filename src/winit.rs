use smithay::{
    backend::{
        renderer::{damage::OutputDamageTracker, gles::GlesRenderer},
        winit::{self, WinitEvent},
    },
    input::pointer::CursorImageStatus,
    output::{Mode, Output, PhysicalProperties, Subpixel},
    reexports::calloop::EventLoop,
    utils::{Rectangle, Transform},
};

use crate::Rose;

/// Runs the compositor nested inside a window on an existing Wayland or X11 session.
pub fn init_winit(
    event_loop: &mut EventLoop<Rose>,
    state: &mut Rose,
) -> Result<(), Box<dyn std::error::Error>> {
    let (mut backend, winit) = winit::init::<GlesRenderer>()?;

    let mode = Mode {
        size: backend.window_size(),
        refresh: 60_000,
    };

    let output = Output::new(
        "winit".to_string(),
        PhysicalProperties {
            size: (0, 0).into(),
            subpixel: Subpixel::Unknown,
            make: "Rose".into(),
            model: "Winit".into(),
        },
    );
    let _global = output.create_global::<Rose>(&state.display_handle);
    output.change_current_state(
        Some(mode),
        Some(Transform::Flipped180),
        None,
        Some((0, 0).into()),
    );
    output.set_preferred(mode);

    state.space.map_output(&output, (0, 0));

    let mut damage_tracker = OutputDamageTracker::from_output(&output);
    // The host cursor shown over the window, so it is only updated when it changes.
    let mut host_cursor: Option<CursorImageStatus> = None;

    // Safety: set before any other threads are spawned.
    unsafe { std::env::set_var("WAYLAND_DISPLAY", &state.socket_name) };

    event_loop
        .handle()
        .insert_source(winit, move |event, _, state| match event {
            WinitEvent::Resized { size, .. } => {
                output.change_current_state(
                    Some(Mode {
                        size,
                        refresh: 60_000,
                    }),
                    None,
                    None,
                    None,
                );
                state.arrange_all();
            }
            WinitEvent::Input(event) => state.process_input_event(event),
            WinitEvent::Redraw => {
                let size = backend.window_size();
                let damage = Rectangle::from_size(size);
                let background = state.config.ui.colorscheme.background.0;

                {
                    let elements = state.render_elements(backend.renderer());
                    let (renderer, mut framebuffer) = backend.bind().unwrap();
                    damage_tracker
                        .render_output(renderer, &mut framebuffer, 0, &elements, background)
                        .unwrap();
                }
                backend.submit(Some(&[damage])).unwrap();

                // Named cursors are drawn by the host; client cursor surfaces are drawn by us.
                if host_cursor.as_ref() != Some(&state.cursor_status) {
                    let window = backend.window();
                    match &state.cursor_status {
                        CursorImageStatus::Named(icon) => {
                            window.set_cursor(*icon);
                            window.set_cursor_visible(true);
                        }
                        CursorImageStatus::Hidden | CursorImageStatus::Surface(_) => {
                            window.set_cursor_visible(false)
                        }
                    }
                    host_cursor = Some(state.cursor_status.clone());
                }

                state.frame_done(&output);
                backend.window().request_redraw();
            }
            WinitEvent::CloseRequested => state.loop_signal.stop(),
            _ => (),
        })?;

    Ok(())
}
