//! Tileception compositor entry point.
//!
//! Usage:
//!   `tileception [--nested | --session] [-- command args...]`
//!   `tileception --greeter` is the login screen, started by greetd.
//!   `tileception msg <command>` controls a running instance.
//!   `tileception config <command>` reads or changes one setting.
//!
//! Without a flag, Tileception runs nested when started inside another desktop
//! (`WAYLAND_DISPLAY` or `DISPLAY` is set) and as the session otherwise.

mod answers;
mod appearance;
mod bar;
mod battery;
mod builtin_lock;
mod calc;
mod capture;
mod commands;
mod config;
mod config_edit;
mod controls;
mod cursor;
mod doctor;
mod doctor_policy;
mod duck;
mod focus;
mod gestures;
mod greeter;
mod handlers;
mod headless;
mod idle;
mod inject;
mod input;
mod ipc;
mod launcher;
mod layers;
mod lock;
mod markdown;
mod media;
mod minimized;
mod notifications;
mod osd;
mod outputs;
mod password;
mod persistence;
mod pointer_lock;
mod power;
mod render;
mod rules;
mod scale;
mod selection;
mod shapes;
mod shell;
mod sleep;
mod state;
mod text;
mod text_input;
mod theme;
mod toplevels;
mod touch;
mod tray;
mod tray_bus;
mod udev;
#[allow(clippy::min_ident_chars, reason = "ui is the established name for the interface drawing module")]
mod ui;
mod wallpaper;
mod winit;
mod xwayland;
mod zones;

use smithay::reexports::{calloop::EventLoop, wayland_server::Display};

pub use state::Tileception;

const USAGE: &str = "\
Usage:
  tileception [--nested | --session | --headless] [-- command args...]
  tileception --greeter [--nested | --headless]
  tileception msg <command> [args...]
  tileception msg --help
  tileception config get|set|add <section.key> [value]

Starts the Tileception compositor, or with `msg` controls a running one.
`config` reads or changes one setting in config.toml, keeping its comments
(`tileception config --help`).
A command after `--` is started once the compositor is up.

Modes:
  (none)        nested when WAYLAND_DISPLAY or DISPLAY is set, else the session
  --nested      run in a window inside another desktop
  --session     run on the hardware (from a TTY or login manager)
  --headless    no screen, for scripted tests; screenshots still work
  --greeter     the login screen, for greetd: on the hardware unless
                nested or headless, where every password is `tide`

Options:
  -h, --help    print this help
  -V, --version print the version

Commands (`tileception msg --help` lists them all):
  tree, screenshot <file.png>, spawn <command line>, focus <dir>, ...

Files:
  $XDG_CONFIG_HOME/tileception/config.toml   settings and key bindings; written
                                             with defaults when missing, reloaded
                                             on save
  ~/.local/state/tileception/tileception.log the session's log (from
                                             tileception-session)
  /etc/tileception/greeter.toml              the greeter's settings
  /var/lib/tileception-greeter/state.toml    who logged in last, and with
                                             which session (greeter)

Environment:
  TILECEPTION_SOCKET         command socket for `msg`; set for programs inside
  TILECEPTION_TERMINAL       the terminal `terminal` opens
  TILECEPTION_SCALE          output scale, overriding [outputs]
  TILECEPTION_HEADLESS_SIZE  headless screen size, such as 1280x800
  TILECEPTION_RENDER_NODE    headless render node, such as /dev/dri/renderD128
  RUST_LOG                   log filter (default: info,smithay=warn)
  GREETD_SOCK                greetd's socket, set by greetd (greeter)
  TILECEPTION_GREETER_CONFIG the greeter's settings file, for tests
  TILECEPTION_GREETER_STATE  the greeter's memory file, for tests
  TILECEPTION_GREETER_PASSWD a passwd file to list people from, for tests
  TILECEPTION_GREETER_SESSIONS Wayland session directories, for tests
";

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    if argv.first().map(String::as_str) == Some("msg") {
        // Help needs no running compositor.
        match argv.get(1).map(String::as_str) {
            None | Some("-h" | "--help" | "help") => print!("{}", commands::HELP),
            _ => ipc::client(&argv[1..])?,
        }
        return Ok(());
    }
    if argv.first().map(String::as_str) == Some("config") {
        if let Err(err) = config_edit::run(&argv[1..]) {
            eprintln!("tileception config: {err}");
            std::process::exit(1);
        }
        return Ok(());
    }
    // Only flags before `--`: what follows is the command to start.
    for arg in argv.iter().take_while(|argument| *argument != "--") {
        match arg.as_str() {
            "-h" | "--help" | "help" => {
                print!("{USAGE}");
                return Ok(());
            }
            "-V" | "--version" => {
                println!("tileception {}", env!("CARGO_PKG_VERSION"));
                return Ok(());
            }
            "--nested" | "--session" | "--headless" | "--greeter" => {}
            // A typo must not start a compositor.
            other => {
                eprintln!("tileception: unknown argument {other:?}\n\n{USAGE}");
                std::process::exit(2);
            }
        }
    }

    let headless = argv.iter().any(|argument| argument == "--headless");
    let greeter = argv.iter().take_while(|argument| *argument != "--").any(|argument| argument == "--greeter");
    // greetd drops the greeter's output, so the greeter also keeps a log of
    // its last run, with every answer from greetd, beside its memory file.
    let greeter_log = greeter.then(greeter::log_file).flatten();
    let default_filter =
        if greeter_log.is_some() { "info,smithay=warn,tileception::greeter=debug" } else { "info,smithay=warn" };
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(default_filter));
    {
        use tracing_subscriber::prelude::*;
        // greetd's greeter must not log to stdout: when nothing drains it (a
        // console with output stopped, a full pipe), a log line blocks, and
        // with it the thread waiting for greetd, so logging in never ends.
        let on_hardware = greeter && !headless && !argv.iter().any(|argument| argument == "--nested");
        let stdout_layer = (!on_hardware).then(tracing_subscriber::fmt::layer);
        let file_layer = greeter_log
            .map(|file| tracing_subscriber::fmt::layer().with_ansi(false).with_writer(std::sync::Mutex::new(file)));
        tracing_subscriber::registry().with(filter).with(stdout_layer).with(file_layer).with(bar::ErrorWatch).init();
    }

    let nested = if argv.iter().any(|argument| argument == "--session") || headless {
        false
    } else if greeter {
        // greetd may leave WAYLAND_DISPLAY or DISPLAY set; only the flag nests.
        argv.iter().any(|argument| argument == "--nested")
    } else {
        argv.iter().any(|argument| argument == "--nested")
            || std::env::var_os("WAYLAND_DISPLAY").is_some()
            || std::env::var_os("DISPLAY").is_some()
    };
    if greeter {
        return run_greeter(nested, headless);
    }
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
    let socket = ipc::socket_path(&state.socket_name.to_string_lossy());
    ipc::listen(&mut event_loop, &socket)?;
    state.ipc_socket = Some(socket.clone());
    tracing::info!(
        "listening on WAYLAND_DISPLAY={} TILECEPTION_SOCKET={}",
        state.socket_name.to_string_lossy(),
        socket.display()
    );

    state.session_services = !nested && !headless;
    if !nested && !headless {
        state.fade_in = render::FadeIn::Waiting;
        state.publish_greeter_card();
        state.start_appearance();
        state.start_sleep_watch();
        export_session_environment(&state);
    }
    // TILECEPTION_POWER_SUPPLY points at a stand-in for /sys/class/power_supply,
    // for tests; such a battery never notifies, hibernates or changes profiles.
    match std::env::var_os("TILECEPTION_POWER_SUPPLY") {
        Some(root) => state.start_battery(Some(root.into()), false),
        None if !nested && !headless => state.start_battery(None, true),
        None => {}
    }
    let notification_bus = std::env::var("TILECEPTION_NOTIFICATION_BUS").ok();
    state.notification_bus = notification_bus.clone();
    let builtin_notifications = state.shell().notifications().is_builtin();
    if ((!nested && !headless) || notification_bus.is_some()) && builtin_notifications {
        state.start_notifications(notification_bus);
    }
    // TILECEPTION_MEDIA=session starts media controls nested or headless too;
    // any other value is a bus address for media players, for tests.
    let media_bus = std::env::var("TILECEPTION_MEDIA").ok();
    state.media_bus = media_bus.clone();
    if ((!nested && !headless) || media_bus.is_some()) && state.shell().media() {
        state.start_media(media_bus.filter(|bus| bus != "session"));
    }
    state.apply_tray();
    for line in state.config.file.general.autostart.clone() {
        // Old configs start swaync, which would fight the built-in service.
        if builtin_notifications && matches!(line.trim(), "swaync" | "sway-notification-center") {
            continue;
        }
        state.spawn("sh", &["-c".to_string(), line]);
    }
    state.apply_bar_command();
    // XDG entries belong to the login session. Nested and headless instances
    // must not start another set of desktop services or personal applications.
    if state.session_services {
        state.pending_autostart = tileception_apps::autostart_entries();
    }
    state.start_xwayland();
    let args: Vec<String> = std::env::args().skip_while(|argument| argument != "--").skip(1).collect();
    if let Some((cmd, rest)) = args.split_first() {
        state.spawn(cmd, rest);
    }

    // Events for clients (keys above all) go out as soon as they are made,
    // not with the next frame.
    event_loop.run(None, &mut state, |state| {
        let _ = state.display_handle.flush_clients();
    })?;
    let _ = std::fs::remove_file(&socket);
    Ok(())
}

/// The login screen: the compositor with no clients, the user's config or
/// desktop services, drawing the greeter on its own.
fn run_greeter(nested: bool, headless: bool) -> Result<(), Box<dyn std::error::Error>> {
    // logind gives a greeter its runtime directory, and the seat, with its
    // session; greetd started from inside another session gets neither.
    let runtime = std::env::var_os("XDG_RUNTIME_DIR").map(std::path::PathBuf::from);
    if !runtime.is_some_and(|dir| dir.is_dir()) {
        let error = "XDG_RUNTIME_DIR is not set: the greeter has no logind session. Start greetd from \
                     systemd (as a service), not from a terminal inside another session.";
        tracing::error!("{error}");
        return Err(error.into());
    }
    let file = greeter::GreeterFile::load();
    // Never the greeter user's config.toml: config::load would write one.
    let mut config = config::Config::default_config();
    config.file.theme = file.theme.clone();
    config.theme = theme::Theme::from_file(&config.file.theme).map_err(|error| format!("[theme] {error}"))?;
    config.file.keyboard = file.keymap();
    let mut event_loop: EventLoop<Tileception> = EventLoop::try_new()?;
    let display: Display<Tileception> = Display::new()?;
    let mut state = Tileception::new(&mut event_loop, display, config, nested);
    state.init_idle();
    if headless {
        headless::init(&mut event_loop, &mut state)?;
    } else if nested {
        winit::init(&mut event_loop, &mut state)?;
    } else {
        udev::init(&mut event_loop, &mut state)?;
    }
    // Scripted tests drive a nested or headless greeter through `msg`. On the
    // hardware nothing may: the socket would let the greeter user run commands.
    let socket = (nested || headless).then(|| ipc::socket_path(&state.socket_name.to_string_lossy()));
    if let Some(socket) = &socket {
        ipc::listen(&mut event_loop, socket)?;
        state.ipc_socket = Some(socket.clone());
        tracing::info!("greeter listening on TILECEPTION_SOCKET={}", socket.display());
    }
    if !nested && !headless {
        state.start_battery(None, false);
    }
    state.start_greeter(file)?;
    event_loop.run(None, &mut state, |state| {
        let _ = state.display_handle.flush_clients();
    })?;
    if let Some(socket) = socket {
        let _ = std::fs::remove_file(socket);
    }
    Ok(())
}

/// Lets D-Bus and systemd started programs (portals, notification daemons,
/// gnome-terminal-server) find this session.
fn export_session_environment(state: &Tileception) {
    let mut vars = vec!["WAYLAND_DISPLAY", "XDG_SESSION_TYPE", "XDG_CURRENT_DESKTOP"];
    if std::env::var_os("SSH_AUTH_SOCK").is_some() {
        vars.push("SSH_AUTH_SOCK");
    }
    let status = std::process::Command::new("dbus-update-activation-environment")
        .env("WAYLAND_DISPLAY", &state.socket_name)
        .env("XDG_SESSION_TYPE", "wayland")
        .env("XDG_CURRENT_DESKTOP", std::env::var_os("XDG_CURRENT_DESKTOP").unwrap_or_else(|| "Tileception".into()))
        .env_remove("DISPLAY")
        .arg("--systemd")
        .args(vars)
        .status();
    if !matches!(status, Ok(exit_status) if exit_status.success()) {
        tracing::warn!("could not export the session environment to D-Bus");
    }
}
