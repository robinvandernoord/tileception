//! Tileception compositor entry point.
//!
//! Usage:
//!   `tileception [--nested | --session] [-- command args...]`
//!   `tileception msg <command>` controls a running instance.
//!
//! Without a flag, Tileception runs nested when started inside another desktop
//! (`WAYLAND_DISPLAY` or `DISPLAY` is set) and as the session otherwise.

mod appearance;
mod builtin_lock;
mod capture;
mod commands;
mod config;
mod cursor;
mod focus;
mod handlers;
mod headless;
mod idle;
mod input;
mod ipc;
mod launcher;
mod layers;
mod lock;
mod notifications;
mod persistence;
mod power;
mod render;
mod selection;
mod shapes;
mod state;
mod text;
mod text_input;
mod theme;
mod udev;
mod ui;
mod wallpaper;
mod winit;
mod xwayland;

use smithay::reexports::{calloop::EventLoop, wayland_server::Display};

pub use state::Tileception;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    if argv.first().map(String::as_str) == Some("msg") {
        ipc::client(&argv[1..])?;
        return Ok(());
    }

    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info,smithay=warn"));
    tracing_subscriber::fmt().with_env_filter(filter).init();

    let headless = argv.iter().any(|a| a == "--headless");
    let nested = if argv.iter().any(|a| a == "--session") || headless {
        false
    } else {
        argv.iter().any(|a| a == "--nested")
            || std::env::var_os("WAYLAND_DISPLAY").is_some()
            || std::env::var_os("DISPLAY").is_some()
    };
    let config_path = config::path();
    let config = config::load(&config_path);

    let mut event_loop: EventLoop<Tileception> = EventLoop::try_new()?;
    let display: Display<Tileception> = Display::new()?;
    let mut state = Tileception::new(&mut event_loop, display, config, nested);
    state.init_idle();
    state.init_launcher();

    if headless {
        headless::init(&mut event_loop, &mut state)?;
    } else if nested {
        winit::init(&mut event_loop, &mut state)?;
    } else {
        udev::init(&mut event_loop, &mut state)?;
    }
    config::watch(&event_loop.handle(), config_path);
    state.start_xwayland();
    let socket = ipc::socket_path(&state.socket_name.to_string_lossy());
    ipc::listen(&mut event_loop, &socket)?;
    state.ipc_socket = Some(socket.clone());
    tracing::info!(
        "listening on WAYLAND_DISPLAY={} TILECEPTION_SOCKET={}",
        state.socket_name.to_string_lossy(),
        socket.display()
    );

    if !nested && !headless {
        state.start_appearance();
        export_session_environment(&state);
    }
    let notification_bus = std::env::var("TILECEPTION_NOTIFICATION_BUS").ok();
    if (!nested && !headless) || notification_bus.is_some() {
        state.start_notifications(notification_bus);
    }
    for line in state.config.file.general.autostart.clone() {
        if matches!(line.trim(), "swaync" | "sway-notification-center") {
            continue;
        }
        state.spawn("sh", &["-c".to_string(), line]);
    }
    let args: Vec<String> = std::env::args().skip_while(|a| a != "--").skip(1).collect();
    if let Some((cmd, rest)) = args.split_first() {
        state.spawn(cmd, rest);
    }

    event_loop.run(None, &mut state, |_| {})?;
    let _ = std::fs::remove_file(&socket);
    Ok(())
}

/// Lets D-Bus and systemd started programs (portals, notification daemons,
/// gnome-terminal-server) find this session.
fn export_session_environment(state: &Tileception) {
    std::env::set_var("WAYLAND_DISPLAY", &state.socket_name);
    std::env::set_var("XDG_SESSION_TYPE", "wayland");
    if std::env::var_os("XDG_CURRENT_DESKTOP").is_none() {
        std::env::set_var("XDG_CURRENT_DESKTOP", "Tileception");
    }
    std::env::remove_var("DISPLAY");
    let vars = ["WAYLAND_DISPLAY", "XDG_SESSION_TYPE", "XDG_CURRENT_DESKTOP"];
    let status = std::process::Command::new("dbus-update-activation-environment").arg("--systemd").args(vars).status();
    if !matches!(status, Ok(s) if s.success()) {
        tracing::warn!("could not export the session environment to D-Bus");
    }
}
