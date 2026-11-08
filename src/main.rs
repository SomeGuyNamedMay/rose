mod action;
mod bar;
mod config;
mod cursor;
mod files;
mod grab;
mod handlers;
mod input;
mod layout;
mod menu;
mod portal;
mod render;
mod state;
mod text;
mod tty;
mod wallpaper;
mod winit;
mod wm;
mod xwayland;

use std::path::PathBuf;

use smithay::reexports::{calloop::EventLoop, wayland_server::Display};

pub use state::Rose;

const USAGE: &str = "usage: rose [--config <path>] [-c <command>] [--nested | --tty]";

fn main() -> Result<(), Box<dyn std::error::Error>> {
    if let Ok(env_filter) = tracing_subscriber::EnvFilter::try_from_default_env() {
        tracing_subscriber::fmt().with_env_filter(env_filter).init();
    } else {
        tracing_subscriber::fmt().init();
    }

    let mut command = None;
    let mut config_path = None;
    // Nested if there's a session to nest in.
    let mut nested =
        std::env::var_os("WAYLAND_DISPLAY").is_some() || std::env::var_os("DISPLAY").is_some();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-c" | "--command" => command = Some(args.next().ok_or(USAGE)?),
            "--config" => config_path = Some(PathBuf::from(args.next().ok_or(USAGE)?)),
            "--nested" => nested = true,
            "--tty" => nested = false,
            "-h" | "--help" => {
                println!("{USAGE}");
                return Ok(());
            }
            _ => return Err(USAGE.into()),
        }
    }

    let config_path = config_path.unwrap_or_else(config::Config::default_path);
    let config = config::Config::load(&config_path).unwrap_or_else(|err| {
        tracing::error!("{err}; using the default config");
        config::Config::default_config()
    });

    let mut event_loop: EventLoop<'static, Rose> = EventLoop::try_new()?;
    let display: Display<Rose> = Display::new()?;
    let mut state = Rose::new(&mut event_loop, display, config, config_path);

    if nested {
        winit::init_winit(&mut event_loop, &mut state)?;
    } else {
        tty::init_tty(&mut event_loop, &mut state)?;
    }
    xwayland::init(&mut event_loop, &mut state);
    if !nested {
        tty::announce_session();
    }

    // Launch a client: `rose -c <command>`, or the configured terminal by default.
    if let Some(command) = command.or_else(|| state.config.actions.get("terminal").cloned()) {
        wm::spawn(&command);
    }

    event_loop.run(None, &mut state, |_| {})?;

    if !nested {
        tty::end_session();
    }

    Ok(())
}
