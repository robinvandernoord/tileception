//! The command language shared by key bindings and `tileception msg`.
//!
//! A command is one line: a verb and its arguments, such as `go 3`,
//! `move left` or `spawn firefox --new-window`.

use tileception_core::Dir;

use crate::{Tileception, input::Action};

/// `tileception msg network ...`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum NetworkCommand {
    /// Opens or closes the card.
    Card,
    Status,
    /// Switches Wi-Fi on or off; `None` flips it.
    Wifi(Option<bool>),
    Disconnect,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    Action(Action),
    /// Runs a shell command line.
    Spawn(String),
    /// Prints the layout tree.
    Tree,
    /// Lists outputs, or targets one for focus/window movement.
    Outputs,
    FocusOutput(String),
    MoveOutput(String),
    /// Drops the focused window at a screen point, as a mouse drag would.
    Drop(f64, f64),
    /// Saves what is on screen to a PNG file.
    Screenshot(std::path::PathBuf),
    /// Puts a PNG file on the clipboard.
    CopyImage(std::path::PathBuf),
    /// Puts text on the clipboard.
    CopyText(String),
    /// Picks a colour from the screen with a magnifier and copies it.
    PickColour,
    /// Gives the newest popup with buttons the keyboard, or gives it back.
    AnswerNotification,
    /// A key pressed while a popup has the keyboard (not available as text).
    NotificationKey(u32, crate::config::Mods),
    /// A key pressed while picking a colour (not available as text).
    ColourPickKey(u32, crate::config::Mods),
    ReloadConfig,
    /// Rescans plugin manifests and restarts their workers without editing config.
    ReloadPlugins,
    /// Installs a plugin from a GitHub repository (`owner/repo[@rev]`).
    AddPlugin(String),
    /// Updates one installed plugin, or all of them.
    UpdatePlugins(Option<String>),
    RemovePlugin(String),
    /// Runs a plugin's `configure` in a terminal.
    ConfigurePlugin(String),
    /// Lists the plugins installed from repositories.
    InstalledPlugins,
    /// Ends a screen lock whatever the locker does: the way out when a locker
    /// is stuck, from a console (Ctrl+Alt+F3) as the same user.
    Unlock,
    /// A key pressed while the power menu is open (not available as text).
    PowerMenuKey(u32),
    /// A key pressed in the feedback confirmation.
    FeedbackKey(u32, crate::config::Mods),
    /// Process launcher input after Smithay releases its keyboard lock.
    LauncherKey(u32, crate::config::Mods),
    Settings,
    SettingsKey(u32, crate::config::Mods),
    /// Replaces the window selection with these ids (empty clears it).
    Select(Vec<tileception_core::WindowId>),
    /// Volume, brightness and media players; these also work while locked.
    Media(crate::media::Control),
    /// The smart home devices, with the command's text for the plugin.
    Home(crate::home::HomeCommand, String),
    /// Calls a plugin's tool with JSON arguments.
    Plugin {
        plugin: String,
        tool: String,
        arguments: serde_json::Value,
    },
    /// Prints the battery's charge and the border colour it gives.
    Battery,
    /// For the bundled Bluetooth plugin: status, on, off, reconnect,
    /// connect or disconnect.
    Bluetooth(String),
    /// Lists tray entries; debug entries can be added or replaced by count.
    Tray,
    /// Prints how much the image caches hold.
    Caches,
    TrayDebug(Option<usize>),
    /// Slides out the strip of minimized windows for a moment.
    ShowMinimized,
    /// Opens or closes the doctor panel, or asks it a question.
    Doctor(Option<String>),
    /// Pretend input, for scripted tests (headless or nested only).
    Inject(crate::inject::Inject),
    /// Makes this ACP Registry agent the doctor's.
    DoctorUse(String),
    /// How much the doctor's agent may do without asking.
    DoctorMode(crate::doctor_policy::Mode),
    /// A key pressed while the doctor panel has the keyboard (not available as text).
    DoctorKey(u32, crate::config::Mods),
    /// Lets the rubber duck out, or puts it away.
    Duck,
    /// Opens a pre-filled GitHub issue in the browser, with this title.
    Feedback(String),
    /// A key pressed while the duck's question has the keyboard (not available as text).
    DuckKey(u32, crate::config::Mods),
    /// The network card, Wi-Fi and the network's status.
    Network(NetworkCommand),
    /// A key pressed while the network card is open (not available as text).
    NetworkKey(u32, crate::config::Mods),
    /// Tells applications to use their dark or light style, until the
    /// config is next loaded.
    ColourScheme(crate::theme::ColourScheme),
    /// Sharing with other machines.
    Handoff(crate::handoff::HandoffCommand),
    /// Pending updates, kernels and release upgrades.
    Updates(crate::updates::UpdatesCommand),
    /// A key pressed in the password prompt or the Updates window (not available as text).
    UpdatesKey(u32, crate::config::Mods),
}

/// What `tileception msg --help` prints. Every verb here must parse (a test
/// checks), so add new commands here too.
pub const HELP: &str = "\
Usage: tileception msg <command> [args...]

Sends one command to the running Tileception and prints its reply. Key
bindings in config.toml use the same commands. The socket is
$TILECEPTION_SOCKET, else the one for $WAYLAND_DISPLAY, else the only one
running. A reply starting with \"error\" exits with status 1.

Inspecting:
  tree                      print the layout tree, with window ids, selection,
                            lock and display-sleep status
  outputs                   list screens, positions, scales and the active screen
  focus-output <name>       direct input and desktop bindings to this screen
  move-output <name>        move the focused/selected windows to this screen
  battery                   print the charge and the border colour it gives
  bluetooth                 list Bluetooth adapters and paired devices
  bluetooth on|off          switch Bluetooth on or off
  bluetooth connect <name>  connect a paired device (also: disconnect)
  bluetooth reconnect       reconnect devices that were connected before
  network status            print the wired connection and the Wi-Fi networks
  tray                      list visible tray entries and the configured limit
  caches                    print the image caches' sizes and texture memory
  tray debug [n]            add one debug entry, or set n debug entries (0 clears)
  volume                    print outputs, inputs, levels and the media player
  media                     print the last-played player's status
  screenshot <file.png>     save what is on screen

Windows:
  terminal                  open the terminal beside the focused window
  spawn <command line>      run a shell command (also: exec)
  close                     close the focused window
  focus <dir>               frame the neighbouring space (dir: left, right,
                            up, down; also plain left, right, up, down)
  move <dir>                move the focused window past its neighbour
  resize <dir> <px>         move the divider next to the focused window
  toggle-split              stack the focused row, or lay the column side by side
  cycle [next|prev]         focus the next or previous window
  float                     float the focused window above the layout, or dock it
  fullscreen                cover the screen with the focused window
  minimize                  take the focused or selected windows out of the layout
  restore [window-id]       put the last minimized window (or this one) back
  show-minimized            slide out the strip of minimized windows for a moment
  select [window-id...]     replace the selection (no ids clears it)
  group                     group the selection
  next-tab                  next tab in the focused tile
  prev-tab                  previous tab in the focused tile
  drop <x> <y>              drop the focused window at a screen point, like a drag

Views:
  overview                  show everything (Esc or a click leaves)
  step-in [pointer]         zoom one level in, toward the focus or the pointer
  step-out                  zoom one level out
  maximize                  frame the focused window on its own (again: back)
  back                      the previous space you framed
  forward                   the next space you framed
  save <1-9>                save the current view in a slot
  go <1-9>                  go to a saved view
  desktop                   show or hide the Desktop tile
  minimap                   open or close the map of the whole tree in the bar
  network                   open or close the network card in the bar
  network wifi [on|off]     switch Wi-Fi on or off (toggles without on or off)
  network disconnect        leave the Wi-Fi network

Shell:
  launcher                  open the launcher
  settings                  open or close Settings (drafts are kept)
  notifications             open the notification list
  notifications clear       clear the history and dismiss every popup
  answer-notification       give the newest popup with buttons the keyboard:
                            arrows or numbers and Enter choose, Esc gives it back
  do-not-disturb [on|off]    toggle do not disturb, or switch it on or off
  lock                      lock the screen (see lock_command)
  unlock                    end a lock whatever the locker does (from a console)
  power-menu                lock, log out, suspend, restart or power off
  copy-image <file.png>     put a PNG on the clipboard
  copy-text <text>          put text on the clipboard
  pick-colour               pick a colour from the screen and copy it as hex
                            (also: pick-color)
  colour-scheme <s>         dark, light or default, until the config is reloaded
                            (also: color-scheme)
  doctor                    open or close the doctor panel
  doctor ask <question>     ask the doctor about your setup
  doctor use <agent>        pick its agent by ACP Registry id (e.g. claude-acp)
  doctor mode <mode>        what it may do unasked: ask, config or yolo
  duck                      let the rubber duck out, or put it away
  feedback [title]          preview and confirm a new GitHub feedback issue
                            then edit in the browser (needs a GitHub account)
  handoff                   machines sharing this mouse and keyboard
  handoff edge [machine] <output> <side>
                            place a machine at an edge (left, right, top,
                            bottom), as dragging its card there does
  handoff dismiss [machine] stop sharing with a machine, or decline it
  handoff retry             ask again about machines declined before
  handoff resume            share again after a lock paused it
  handoff home              bring input back (also Ctrl+Super+Escape)
  handoff reclaim [machine] bring this machine's windows back from others
  updates                   list pending updates and what else waits
  updates check             refresh the package lists (asks for your password)
  updates install [security|package...]
                            install every update, the security updates or
                            the named ones
  updates open [page]       open the Updates window: available, kernels,
                            release or history
  updates show              open or close the card under the bar's chip
  updates kernels           list kernels: running, fallback, old, available
  updates clean             remove old kernels and leftover kernel packages
  updates autoremove        remove packages that are no longer needed
  updates repair            finish what dpkg left half done and settle broken
                            dependencies, when apt asks for it
  updates log               print what the last task's programs printed
  updates upgrade           start the release upgrade in a terminal
  reload                    reload config.toml
  vt <n>                    switch to virtual terminal n
  quit                      end the session
  restart                   start Tileception again as installed now, in the
                            same session; open windows close

Sound, light and media:
  volume up|down [n]        change the output volume by n percent (or the step)
  volume set <n>            set it; a bare percentage works too (volume 40)
  volume mute [on|off|toggle]
  volume output|input <n>   use device n from the plain `volume` list
  mic up|down|set|mute ...  the same for the microphone
  brightness up|down|set .. the screen's backlight
  media <what>              play-pause, play, pause, next, previous or stop

Smart home ([home] in config.toml):
  home                      list the devices with their state
  home why <name>           why a device is unreachable, and what to check
  home on|off|toggle <name> switch a light, plug or air conditioner
  home brightness <name> <n>  dim a light to n percent
  home colour <name> <n>    colour temperature, 0 (warm) to 100 (cool)
  home target <name> <deg>  an air conditioner's target temperature
  home mode <name> <mode>   auto, cool, heat, dry or fan
  home fan <name> <speed>   auto, quiet, low, mid, high or strong
  home all-off              switch off every light and plug
  home select <name>        show a light's sliders in the panel
  home fold|unfold|rooms|all  the panel block's folding and view
  home enable|disable       switch the [home] block on or off in config.toml

Plugins:
  plugins reload            rediscover manifests and restart plugins without changing config
  plugins add <owner/repo>  install a plugin from GitHub with uv, then set it up
  plugins list              plugins installed from repositories
  plugins configure <id>    run a plugin's setup in a terminal
  plugins update [id]       update one installed plugin, or all of them
  plugins remove <id>       run a plugin's uninstall and remove it
  plugin <id> <tool> [json] call a plugin's tool with a JSON object of
                            arguments and print its answer, such as
                            plugin home toggle {\"device\": \"Lamp\"}

Pretend input (headless or nested only):
  pointer <x> <y>           move the pointer
  motion <dx> <dy>          move it by this much, like a mouse (crosses to
                            machines linked with handoff)
  click [x y] [left|right|middle]
  press [left|right|middle] hold a button down (drags), and let go with
  release [left|right|middle]
  key <combo>               press a key, such as Return or Super+slash
  keydown <key>             hold a key down (keydown Alt, then press and
                            motion: an Alt+drag)
  keyup <key>               let a held key go
  type <text>               type text
  gesture <name>            such as Swipe3Left, Super+Pinch2In or TouchEdgeTop
  touch <x> <y> [x2 y2]     tap, or drag to the second point

While the screen is locked only tree, screenshot, reload, unlock, vt,
battery and the sound, light and media commands work.
";

pub fn parse(line: &str) -> Result<Command, String> {
    let line = line.trim();
    let (verb, rest) = line.split_once(char::is_whitespace).unwrap_or((line, ""));
    let rest = rest.trim();
    let args: Vec<&str> = rest.split_whitespace().collect();
    let slot = || -> Result<usize, String> {
        args.first()
            .and_then(|arg| arg.parse::<usize>().ok())
            .filter(|number| (1..=9).contains(number))
            .ok_or_else(|| format!("{verb} needs a slot from 1 to 9"))
    };
    let dir = || -> Result<Dir, String> {
        match args.first().copied() {
            Some("left") => Ok(Dir::Left),
            Some("right") => Ok(Dir::Right),
            Some("up") => Ok(Dir::Up),
            Some("down") => Ok(Dir::Down),
            _ => Err(format!("{verb} needs left, right, up or down")),
        }
    };
    let action = match verb {
        "" => return Err("empty command".into()),
        "tree" => return Ok(Command::Tree),
        "outputs" if args.is_empty() => return Ok(Command::Outputs),
        "focus-output" | "move-output" => {
            let [name] = args.as_slice() else { return Err(format!("{verb} needs one output name")) };
            return Ok(if verb == "focus-output" {
                Command::FocusOutput((*name).into())
            } else {
                Command::MoveOutput((*name).into())
            });
        }
        "show-minimized" => return Ok(Command::ShowMinimized),
        "handoff" => return crate::handoff::parse(&args).map(Command::Handoff),
        "reload" => return Ok(Command::ReloadConfig),
        "plugins" => {
            return match args.as_slice() {
                ["reload"] => Ok(Command::ReloadPlugins),
                ["add", source] => Ok(Command::AddPlugin((*source).into())),
                ["update"] => Ok(Command::UpdatePlugins(None)),
                ["update", plugin_id] => Ok(Command::UpdatePlugins(Some((*plugin_id).into()))),
                ["remove", plugin_id] => Ok(Command::RemovePlugin((*plugin_id).into())),
                ["configure", plugin_id] => Ok(Command::ConfigurePlugin((*plugin_id).into())),
                ["list"] => Ok(Command::InstalledPlugins),
                _ => {
                    Err("plugins needs reload, list, add <owner/repo>, update [plugin_id], remove <plugin_id> or configure <plugin_id>"
                        .into())
                }
            };
        }
        "unlock" => return Ok(Command::Unlock),
        "battery" => return Ok(Command::Battery),
        "updates" => return crate::updates::parse(&args).map(Command::Updates),
        "bluetooth" => return Ok(Command::Bluetooth(rest.to_string())),
        "network" => {
            return match args.as_slice() {
                [] => Ok(Command::Network(NetworkCommand::Card)),
                ["status"] => Ok(Command::Network(NetworkCommand::Status)),
                ["wifi"] | ["wifi", "toggle"] => Ok(Command::Network(NetworkCommand::Wifi(None))),
                ["wifi", "on"] => Ok(Command::Network(NetworkCommand::Wifi(Some(true)))),
                ["wifi", "off"] => Ok(Command::Network(NetworkCommand::Wifi(Some(false)))),
                ["disconnect"] => Ok(Command::Network(NetworkCommand::Disconnect)),
                _ => Err("network takes nothing, status, wifi [on|off] or disconnect".into()),
            };
        }
        "caches" => return Ok(Command::Caches),
        "tray" => {
            return match args.as_slice() {
                [] => Ok(Command::Tray),
                ["debug"] => Ok(Command::TrayDebug(None)),
                ["debug", count] => count
                    .parse::<usize>()
                    .ok()
                    .filter(|parsed| *parsed <= 1000)
                    .map(|parsed| Command::TrayDebug(Some(parsed)))
                    .ok_or_else(|| "tray debug needs a count from 0 to 1000".into()),
                _ => Err("tray takes no arguments or debug [n]".into()),
            };
        }
        "select" => {
            let ids = args
                .iter()
                .map(|id_text| {
                    id_text
                        .parse::<u64>()
                        .map(tileception_core::WindowId)
                        .map_err(|_| "select needs numeric window ids".to_string())
                })
                .collect::<Result<Vec<_>, _>>()?;
            return Ok(Command::Select(ids));
        }
        "volume" | "mic" | "brightness" | "media" => return crate::media::parse(verb, &args).map(Command::Media),
        "home" => return crate::home::parse(rest).map(|command| Command::Home(command, rest.trim().to_string())),
        "plugin" => {
            let mut parts = rest.trim().splitn(3, char::is_whitespace);
            let plugin =
                parts.next().filter(|plugin| !plugin.is_empty()).ok_or("plugin needs a plugin id and a tool")?;
            let tool = parts.next().ok_or("plugin needs a tool after the plugin id")?;
            let arguments = match parts.next().map(str::trim).filter(|json| !json.is_empty()) {
                Some(json) => serde_json::from_str::<serde_json::Value>(json)
                    .ok()
                    .filter(serde_json::Value::is_object)
                    .ok_or("plugin arguments must be a JSON object, such as {\"device\": \"Lamp\"}")?,
                None => serde_json::json!({}),
            };
            return Ok(Command::Plugin { plugin: plugin.into(), tool: tool.into(), arguments });
        }
        "settings" if rest.is_empty() => return Ok(Command::Settings),
        "doctor" => {
            // Whole words only: "doctor user" is not "doctor use r".
            let (sub, tail) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
            let tail = tail.trim();
            return match sub {
                "mode" => crate::doctor_policy::Mode::parse(tail)
                    .map(Command::DoctorMode)
                    .ok_or_else(|| format!("doctor mode takes ask, tweak or yolo, not {tail:?}")),
                "use" if tail.is_empty() => Err("doctor use needs an agent id from the ACP Registry".into()),
                "use" => Ok(Command::DoctorUse(tail.to_string())),
                "ask" if tail.is_empty() => Err("doctor ask needs a question".into()),
                "ask" => Ok(Command::Doctor(Some(tail.to_string()))),
                "" => Ok(Command::Doctor(None)),
                _ => Err(format!("doctor takes nothing or \"ask <question>\", not {rest:?}")),
            };
        }
        "pointer" | "motion" | "click" | "press" | "release" | "key" | "keydown" | "keyup" | "type" | "gesture"
        | "touch" => {
            return crate::inject::parse(verb, rest).map(Command::Inject);
        }
        "colour-scheme" | "color-scheme" => {
            return crate::theme::ColourScheme::parse(rest).map(Command::ColourScheme);
        }
        "spawn" | "exec" => {
            if rest.is_empty() {
                return Err("spawn needs a command".into());
            }
            return Ok(Command::Spawn(rest.to_string()));
        }
        "screenshot" => {
            if rest.is_empty() {
                return Err("screenshot needs a file name".into());
            }
            return Ok(Command::Screenshot(rest.into()));
        }
        "copy-text" => {
            if rest.is_empty() {
                return Err("copy-text needs some text".into());
            }
            return Ok(Command::CopyText(rest.to_string()));
        }
        "copy-image" => {
            if rest.is_empty() {
                return Err("copy-image needs a PNG file".into());
            }
            return Ok(Command::CopyImage(rest.into()));
        }
        "drop" => {
            let coords: Vec<f64> = args.iter().filter_map(|arg| arg.parse().ok()).collect();
            let [x_pos, y_pos] = coords[..] else { return Err("drop needs x and y".into()) };
            return Ok(Command::Drop(x_pos, y_pos));
        }
        "answer-notification" if rest.is_empty() => return Ok(Command::AnswerNotification),
        "answer-notification" => return Err(format!("answer-notification takes nothing, not {rest:?}")),
        "pick-colour" | "pick-color" if rest.is_empty() => return Ok(Command::PickColour),
        "pick-colour" | "pick-color" => return Err(format!("pick-colour takes nothing, not {rest:?}")),
        "duck" if rest.is_empty() => return Ok(Command::Duck),
        "duck" => return Err(format!("duck takes nothing, not {rest:?}")),
        "feedback" => return Ok(Command::Feedback(rest.to_string())),
        "quit" => Action::Quit,
        "restart" => Action::Restart,
        "terminal" => Action::Spawn,
        "close" => Action::Close,
        "overview" => Action::Overview,
        "step-in" => match args.first().copied() {
            None => Action::StepIn,
            Some("pointer") => Action::StepInPointer,
            Some(other) => return Err(format!("step-in takes nothing or \"pointer\", not {other:?}")),
        },
        "cycle" => match args.first().copied() {
            None | Some("next") => Action::Cycle(true),
            Some("prev") => Action::Cycle(false),
            Some(other) => return Err(format!("cycle takes next or prev, not {other:?}")),
        },
        "toggle-split" => Action::ToggleSplit,
        "resize" => {
            let direction = dir()?;
            let pixels: f64 =
                args.get(1).and_then(|arg| arg.parse().ok()).ok_or("resize needs a direction and pixels")?;
            match direction {
                Dir::Right => Action::Resize(pixels, 0.0),
                Dir::Left => Action::Resize(-pixels, 0.0),
                Dir::Down => Action::Resize(0.0, pixels),
                Dir::Up => Action::Resize(0.0, -pixels),
            }
        }
        "step-out" => Action::StepOut,
        "focus" => Action::Neighbour(dir()?),
        "left" => Action::Neighbour(Dir::Left),
        "right" => Action::Neighbour(Dir::Right),
        "up" => Action::Neighbour(Dir::Up),
        "down" => Action::Neighbour(Dir::Down),
        "move" => Action::Move(dir()?),
        "back" => Action::Back,
        "forward" => Action::Forward,
        "maximize" => Action::Maximize,
        "desktop" => Action::ToggleDesktop,
        "float" => Action::ToggleFloat,
        "fullscreen" => Action::Fullscreen,
        "lock" => Action::Lock,
        "power-menu" => Action::PowerMenu,
        "launcher" => Action::Launcher,
        "notifications" => match args.first().copied() {
            None => Action::Notifications,
            Some("clear") => Action::ClearNotifications,
            Some(other) => return Err(format!("notifications takes nothing or \"clear\", not {other:?}")),
        },
        "do-not-disturb" => match args.first().copied() {
            None | Some("toggle") => Action::DoNotDisturb(None),
            Some("on") => Action::DoNotDisturb(Some(true)),
            Some("off") => Action::DoNotDisturb(Some(false)),
            Some(other) => return Err(format!("do-not-disturb takes on, off or toggle, not {other:?}")),
        },
        "minimap" => Action::Minimap,
        "group" => Action::Group,
        "minimize" => Action::Minimize,
        "restore" => match args.first() {
            None => Action::Restore(None),
            Some(id_text) => Action::Restore(Some(tileception_core::WindowId(
                id_text.parse().map_err(|_| "restore takes nothing or a window id".to_string())?,
            ))),
        },
        "next-tab" => Action::NextTab,
        "prev-tab" => Action::PrevTab,
        "save" => Action::SaveView(slot()?),
        "go" => Action::RecallView(slot()?),
        "vt" => {
            let number = args.first().and_then(|arg| arg.parse::<i32>().ok()).ok_or("vt needs a number")?;
            Action::Vt(number)
        }
        other => return Err(format!("unknown command {other:?}")),
    };
    Ok(Command::Action(action))
}

impl Tileception {
    /// Runs a command; the reply is for `tileception msg`.
    pub fn execute(&mut self, command: Command) -> Result<String, String> {
        // Scripts may type on Tileception's own lock screen and greeter:
        // pretend input only works nested or headless, never on a real session.
        let typing = self.draws_lock_screen() && matches!(command, Command::Inject(_));
        if self.is_locked()
            && !typing
            && !matches!(
                command,
                Command::Unlock
                    | Command::Tree
                    | Command::Outputs
                    | Command::Screenshot(_)
                    | Command::ReloadConfig
                    | Command::Action(Action::Vt(_))
                    | Command::Media(_)
                    | Command::Battery
                    | Command::Handoff(crate::handoff::HandoffCommand::Status)
            )
        {
            return Err("session is locked".into());
        }
        match command {
            Command::Action(action) => self.run(action),
            Command::Spawn(line) => self.spawn("sh", &["-c".to_string(), line]),
            Command::Tree => return Ok(crate::ipc::describe(self)),
            Command::Outputs => return Ok(self.outputs_status()),
            Command::FocusOutput(name) => {
                let area = self.area_named(&name).ok_or_else(|| format!("unknown output {name:?}"))?;
                self.activate_area(&area);
                self.on_motion(self.pointer, (self.now() * 1000.0) as u32);
            }
            Command::MoveOutput(name) => {
                let area = self.area_named(&name).ok_or_else(|| format!("unknown output {name:?}"))?;
                let windows = if self.selection.windows.is_empty() {
                    vec![self.world.focus().ok_or("nothing is focused")?]
                } else {
                    self.selection.windows.iter().copied().collect()
                };
                for window in windows {
                    self.move_window_to_area(window, &area, false)?;
                }
                self.activate_area(&area);
                self.force_keyboard_focus_sync();
            }
            Command::ShowMinimized => self.peek_minimized(),
            Command::Screenshot(path) => self.screenshot(path),
            Command::CopyImage(path) => self.copy_image(&path)?,
            Command::CopyText(text) => self.copy_text(text),
            Command::PickColour => self.pick_colour(),
            Command::AnswerNotification => self.answer_notification(),
            Command::NotificationKey(sym, mods) => self.notification_key(sym, mods),
            Command::ColourPickKey(sym, mods) => self.colour_pick_key(sym, mods),
            Command::ReloadConfig => self.reload_config()?,
            Command::ReloadPlugins => {
                self.reload_plugins()?;
                return Ok("Plugin rediscovery started\n".into());
            }
            Command::AddPlugin(source) => return self.plugin_add(&source),
            Command::UpdatePlugins(plugin_id) => return self.plugin_update(plugin_id.as_deref()),
            Command::RemovePlugin(plugin_id) => return self.plugin_remove(&plugin_id),
            Command::ConfigurePlugin(plugin_id) => return self.plugin_configure(&plugin_id),
            Command::InstalledPlugins => return Ok(crate::plugin_install::listing()),
            Command::Unlock => self.force_unlock(),
            Command::Media(control) => return self.media_command(control),
            Command::Home(command, text) => return self.home_command(command, &text),
            Command::Plugin { plugin, tool, arguments } => return self.plugin_command(&plugin, &tool, arguments),
            Command::Battery => return Ok(self.battery_status()),
            Command::Bluetooth(text) => {
                return self.plugin_command("bluetooth", "command", serde_json::json!({ "text": text }));
            }
            Command::Tray => return Ok(self.tray_status()),
            Command::Caches => return Ok(self.cache_status()),
            Command::TrayDebug(count) => return Ok(self.tray_debug(count)),
            Command::Doctor(None) => self.toggle_doctor(),
            Command::Doctor(Some(question)) => self.ask_doctor(&question),
            Command::DoctorKey(sym, mods) => self.doctor_key(sym, mods),
            Command::DoctorUse(agent_id) => self.use_doctor_agent(agent_id),
            Command::Inject(input) => self.inject(input)?,
            Command::DoctorMode(mode) => self.set_doctor_mode(mode),
            Command::Duck => self.toggle_duck(),
            Command::Feedback(title) => self.send_feedback(&title),
            Command::FeedbackKey(sym, mods) => self.feedback_key(sym, mods),
            Command::DuckKey(sym, mods) => self.duck_key(sym, mods),
            Command::Network(command) => return self.network_command(command),
            Command::NetworkKey(sym, mods) => self.network_key(sym, mods),
            Command::PowerMenuKey(sym) => self.power_menu_key(sym),
            Command::LauncherKey(sym, mods) => self.launcher_key(sym, mods),
            Command::Settings => self.toggle_settings(),
            Command::SettingsKey(sym, mods) => self.settings_key(sym, mods),
            Command::Handoff(command) => return self.handoff_command(command),
            Command::Updates(command) => return self.updates_command(command),
            Command::UpdatesKey(sym, mods) => {
                if self.password_prompt.is_some() {
                    self.password_prompt_key(sym, mods);
                } else if self.updates_window_focused() {
                    self.updates_window_key(sym, mods);
                }
            }
            Command::ColourScheme(scheme) => {
                self.theme.colour_scheme = scheme;
                self.update_appearance();
            }
            Command::Select(ids) => {
                if ids.iter().any(|window_id| !self.world.contains_window(*window_id)) {
                    return Err("select contains an unknown window id".into());
                }
                self.selection.windows = ids.into_iter().collect();
            }
            Command::Drop(x_pos, y_pos) => {
                let win = self.world.focus().ok_or("nothing is focused")?;
                let now = self.now();
                let view = self.world.view(now);
                let target = self
                    .world
                    .drop_target(tileception_core::Point::new(x_pos, y_pos), win, view)
                    .ok_or("no target there")?;
                self.world.apply_drop(win, target, now);
                self.apply_layout();
                self.persist_layout();
                return Ok(format!("{target:?}\n"));
            }
        }
        Ok("ok\n".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn help_lists_only_real_commands() {
        let verbs: Vec<&str> = HELP
            .lines()
            .filter(|line| line.starts_with("  ") && !line.starts_with("   "))
            .filter_map(|line| line.split_whitespace().next())
            .collect();
        assert!(verbs.len() > 50, "help lists {} commands", verbs.len());
        for verb in verbs {
            if let Err(err) = parse(verb) {
                assert!(!err.starts_with("unknown"), "help lists {verb:?}: {err}");
            }
        }
    }

    #[test]
    fn plugins_commands_and_their_errors() {
        assert_eq!(parse("plugins add a/b"), Ok(Command::AddPlugin("a/b".into())));
        assert_eq!(parse("plugins update"), Ok(Command::UpdatePlugins(None)));
        assert_eq!(parse("plugins update odoo"), Ok(Command::UpdatePlugins(Some("odoo".into()))));
        assert_eq!(parse("plugins remove odoo"), Ok(Command::RemovePlugin("odoo".into())));
        assert_eq!(parse("plugins configure odoo"), Ok(Command::ConfigurePlugin("odoo".into())));
        assert_eq!(parse("plugins list"), Ok(Command::InstalledPlugins));
        assert!(parse("plugins add").is_err());
        // Errors speak of ids in words, not of variable names.
        assert_eq!(parse("plugin"), Err("plugin needs a plugin id and a tool".into()));
        assert_eq!(parse("plugin home"), Err("plugin needs a tool after the plugin id".into()));
        assert_eq!(parse("doctor use"), Err("doctor use needs an agent id from the ACP Registry".into()));
        assert_eq!(parse("restore x"), Err("restore takes nothing or a window id".into()));
    }

    #[test]
    fn parses_commands() {
        assert_eq!(parse("go 3"), Ok(Command::Action(Action::RecallView(3))));
        assert_eq!(parse("copy-image /tmp/a b.png"), Ok(Command::CopyImage("/tmp/a b.png".into())));
        assert!(parse("copy-image").is_err());
        assert_eq!(parse("move left"), Ok(Command::Action(Action::Move(Dir::Left))));
        assert_eq!(parse("spawn firefox --new-window"), Ok(Command::Spawn("firefox --new-window".into())));
        assert_eq!(parse("drop 10 20.5"), Ok(Command::Drop(10.0, 20.5)));
        assert_eq!(parse("resize left 20"), Ok(Command::Action(Action::Resize(-20.0, 0.0))));
        assert_eq!(parse("step-in pointer"), Ok(Command::Action(Action::StepInPointer)));
        assert_eq!(parse("cycle prev"), Ok(Command::Action(Action::Cycle(false))));
        assert!(parse("go 0").is_err());
        assert!(parse("bogus").is_err());
        assert_eq!(parse("doctor"), Ok(Command::Doctor(None)));
        assert_eq!(parse("doctor ask why so slow?"), Ok(Command::Doctor(Some("why so slow?".into()))));
        assert!(parse("doctor ask").is_err());
        assert!(parse("doctor now").is_err());
        assert_eq!(parse("doctor mode yolo"), Ok(Command::DoctorMode(crate::doctor_policy::Mode::Yolo)));
        assert!(parse("doctor mode loud").is_err());
        assert!(parse("doctor user").is_err());
        assert!(parse("doctor asking").is_err());
        assert_eq!(parse("doctor use claude-acp"), Ok(Command::DoctorUse("claude-acp".into())));
        assert_eq!(parse("notifications"), Ok(Command::Action(Action::Notifications)));
        assert_eq!(parse("notifications clear"), Ok(Command::Action(Action::ClearNotifications)));
        assert!(parse("notifications wipe").is_err());
        assert_eq!(parse("do-not-disturb"), Ok(Command::Action(Action::DoNotDisturb(None))));
        assert_eq!(parse("do-not-disturb on"), Ok(Command::Action(Action::DoNotDisturb(Some(true)))));
        assert_eq!(parse("do-not-disturb off"), Ok(Command::Action(Action::DoNotDisturb(Some(false)))));
        assert!(parse("do-not-disturb maybe").is_err());
        assert_eq!(parse("duck"), Ok(Command::Duck));
        assert_eq!(parse("feedback"), Ok(Command::Feedback(String::new())));
        assert_eq!(parse("feedback zoom is slow"), Ok(Command::Feedback("zoom is slow".into())));
        assert!(parse("duck duck goose").is_err());
        assert_eq!(parse("colour-scheme light"), Ok(Command::ColourScheme(crate::theme::ColourScheme::Light)));
        assert!(parse("colour-scheme blue").is_err());
        assert!(parse("").is_err());
    }
}
