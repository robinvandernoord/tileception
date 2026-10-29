//! The command language shared by key bindings and `tileception msg`.
//!
//! A command is one line: a verb and its arguments, such as `go 3`,
//! `move left` or `spawn firefox --new-window`.

use tileception_core::Dir;

use crate::{Tileception, input::Action};

#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    Action(Action),
    /// Runs a shell command line.
    Spawn(String),
    /// Prints the layout tree.
    Tree,
    /// Drops the focused window at a screen point, as a mouse drag would.
    Drop(f64, f64),
    /// Saves what is on screen to a PNG file.
    Screenshot(std::path::PathBuf),
    /// Puts a PNG file on the clipboard.
    CopyImage(std::path::PathBuf),
    ReloadConfig,
    /// Ends a screen lock whatever the locker does: the way out when a locker
    /// is stuck, from a console (Ctrl+Alt+F3) as the same user.
    Unlock,
    /// A key pressed while the power menu is open (not available as text).
    PowerMenuKey(u32),
    /// Process launcher input after Smithay releases its keyboard lock.
    LauncherKey(u32, crate::config::Mods),
    /// Replaces the window selection with these ids (empty clears it).
    Select(Vec<tileception_core::WindowId>),
    /// Volume, brightness and media players; these also work while locked.
    Media(crate::media::Control),
    /// Prints the battery's charge and the border colour it gives.
    Battery,
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
    /// A key pressed while the duck's question has the keyboard (not available as text).
    DuckKey(u32, crate::config::Mods),
    /// Tells applications to use their dark or light style, until the
    /// config is next loaded.
    ColourScheme(crate::theme::ColourScheme),
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
  battery                   print the charge and the border colour it gives
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

Shell:
  launcher                  open the launcher
  notifications             open the notification list
  do-not-disturb            toggle do not disturb
  lock                      lock the screen (see lock_command)
  unlock                    end a lock whatever the locker does (from a console)
  power-menu                lock, log out, suspend, restart or power off
  copy-image <file.png>     put a PNG on the clipboard
  colour-scheme <s>         dark, light or default, until the config is reloaded
                            (also: color-scheme)
  doctor                    open or close the doctor panel
  doctor ask <question>     ask the doctor about your setup
  doctor use <agent>        pick its agent by ACP Registry id (e.g. claude-acp)
  doctor mode <mode>        what it may do unasked: ask, config or yolo
  duck                      let the rubber duck out, or put it away
  reload                    reload config.toml
  vt <n>                    switch to virtual terminal n
  quit                      end the session

Sound, light and media:
  volume up|down [n]        change the output volume by n percent (or the step)
  volume set <n>            set it; a bare percentage works too (volume 40)
  volume mute [on|off|toggle]
  volume output|input <n>   use device n from the plain `volume` list
  mic up|down|set|mute ...  the same for the microphone
  brightness up|down|set .. the screen's backlight
  media <what>              play-pause, play, pause, next, previous or stop

Pretend input (headless or nested only):
  pointer <x> <y>           move the pointer
  click [x y] [left|right|middle]
  key <combo>               press a key, such as Return or Super+slash
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
        "show-minimized" => return Ok(Command::ShowMinimized),
        "reload" => return Ok(Command::ReloadConfig),
        "unlock" => return Ok(Command::Unlock),
        "battery" => return Ok(Command::Battery),
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
        "pointer" | "click" | "key" | "type" | "gesture" | "touch" => {
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
        "duck" if rest.is_empty() => return Ok(Command::Duck),
        "duck" => return Err(format!("duck takes nothing, not {rest:?}")),
        "quit" => Action::Quit,
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
        "notifications" => Action::Notifications,
        "do-not-disturb" => Action::DoNotDisturb,
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
                    | Command::Screenshot(_)
                    | Command::ReloadConfig
                    | Command::Action(Action::Vt(_))
                    | Command::Media(_)
                    | Command::Battery
            )
        {
            return Err("session is locked".into());
        }
        match command {
            Command::Action(action) => self.run(action),
            Command::Spawn(line) => self.spawn("sh", &["-c".to_string(), line]),
            Command::Tree => return Ok(crate::ipc::describe(self)),
            Command::ShowMinimized => self.peek_minimized(),
            Command::Screenshot(path) => self.screenshot(path),
            Command::CopyImage(path) => self.copy_image(&path)?,
            Command::ReloadConfig => self.reload_config()?,
            Command::Unlock => self.force_unlock(),
            Command::Media(control) => return self.media_command(control),
            Command::Battery => return Ok(self.battery_status()),
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
            Command::DuckKey(sym, mods) => self.duck_key(sym, mods),
            Command::PowerMenuKey(sym) => self.power_menu_key(sym),
            Command::LauncherKey(sym, mods) => self.launcher_key(sym, mods),
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
        assert_eq!(parse("duck"), Ok(Command::Duck));
        assert!(parse("duck duck goose").is_err());
        assert_eq!(parse("colour-scheme light"), Ok(Command::ColourScheme(crate::theme::ColourScheme::Light)));
        assert!(parse("colour-scheme blue").is_err());
        assert!(parse("").is_err());
    }
}
