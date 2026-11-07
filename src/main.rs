mod action;
mod config;
mod grab;
mod handlers;
mod input;
mod layout;
mod render;
mod state;
mod winit;
mod wm;

use std::path::PathBuf;

use smithay::reexports::{calloop::EventLoop, wayland_server::Display};

pub use state::Rose;

const USAGE: &str = "usage: rose [--config <path>] [-c <command>]";

fn main() -> Result<(), Box<dyn std::error::Error>> {
    if let Ok(env_filter) = tracing_subscriber::EnvFilter::try_from_default_env() {
        tracing_subscriber::fmt().with_env_filter(env_filter).init();
    } else {
        tracing_subscriber::fmt().init();
    }

    let mut command = None;
    let mut config_path = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-c" | "--command" => command = Some(args.next().ok_or(USAGE)?),
            "--config" => config_path = Some(PathBuf::from(args.next().ok_or(USAGE)?)),
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

    let mut event_loop: EventLoop<Rose> = EventLoop::try_new()?;
    let display: Display<Rose> = Display::new()?;
    let mut state = Rose::new(&mut event_loop, display, config, config_path);

    winit::init_winit(&mut event_loop, &mut state)?;

    // Launch a client: `rose -c <command>`, or the configured terminal by default.
    if let Some(command) = command.or_else(|| state.config.actions.get("terminal").cloned()) {
        wm::spawn(&command);
    }

    event_loop.run(None, &mut state, |_| {})?;

    Ok(())
}
