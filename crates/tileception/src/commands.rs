//! The command language shared by key bindings and `tileception msg`.
//!
//! A command is one line: a verb and its arguments, such as `go 3`,
//! `move left` or `spawn firefox --new-window`.

use tileception_core::Dir;

use crate::{input::Action, Tileception};

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
}

pub fn parse(line: &str) -> Result<Command, String> {
    let line = line.trim();
    let (verb, rest) = line.split_once(char::is_whitespace).unwrap_or((line, ""));
    let rest = rest.trim();
    let args: Vec<&str> = rest.split_whitespace().collect();
    let slot = || -> Result<usize, String> {
        args.first()
            .and_then(|a| a.parse::<usize>().ok())
            .filter(|n| (1..=9).contains(n))
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
        "reload" => return Ok(Command::ReloadConfig),
        "unlock" => return Ok(Command::Unlock),
        "select" => {
            let ids = args
                .iter()
                .map(|id| {
                    id.parse::<u64>()
                        .map(tileception_core::WindowId)
                        .map_err(|_| "select needs numeric window ids".to_string())
                })
                .collect::<Result<Vec<_>, _>>()?;
            return Ok(Command::Select(ids));
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
        "drop" => {
            let coords: Vec<f64> = args.iter().filter_map(|a| a.parse().ok()).collect();
            let [x, y] = coords[..] else { return Err("drop needs x and y".into()) };
            return Ok(Command::Drop(x, y));
        }
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
            let d = dir()?;
            let px: f64 = args.get(1).and_then(|a| a.parse().ok()).ok_or("resize needs a direction and pixels")?;
            match d {
                Dir::Right => Action::Resize(px, 0.0),
                Dir::Left => Action::Resize(-px, 0.0),
                Dir::Down => Action::Resize(0.0, px),
                Dir::Up => Action::Resize(0.0, -px),
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
        "next-tab" => Action::NextTab,
        "prev-tab" => Action::PrevTab,
        "save" => Action::SaveView(slot()?),
        "go" => Action::RecallView(slot()?),
        "vt" => {
            let n = args.first().and_then(|a| a.parse::<i32>().ok()).ok_or("vt needs a number")?;
            Action::Vt(n)
        }
        other => return Err(format!("unknown command {other:?}")),
    };
    Ok(Command::Action(action))
}

impl Tileception {
    /// Runs a command; the reply is for `tileception msg`.
    pub fn execute(&mut self, command: Command) -> Result<String, String> {
        if self.is_locked()
            && !matches!(
                command,
                Command::Unlock
                    | Command::Tree
                    | Command::Screenshot(_)
                    | Command::ReloadConfig
                    | Command::Action(Action::Vt(_))
            )
        {
            return Err("session is locked".into());
        }
        match command {
            Command::Action(action) => self.run(action),
            Command::Spawn(line) => self.spawn("sh", &["-c".to_string(), line]),
            Command::Tree => return Ok(crate::ipc::describe(self)),
            Command::Screenshot(path) => self.screenshot(path),
            Command::ReloadConfig => self.reload_config()?,
            Command::Unlock => self.force_unlock(),
            Command::PowerMenuKey(sym) => self.power_menu_key(sym),
            Command::LauncherKey(sym, mods) => self.launcher_key(sym, mods),
            Command::Select(ids) => {
                if ids.iter().any(|id| !self.world.contains_window(*id)) {
                    return Err("select contains an unknown window id".into());
                }
                self.selection.windows = ids.into_iter().collect();
            }
            Command::Drop(x, y) => {
                let win = self.world.focus().ok_or("nothing is focused")?;
                let now = self.now();
                let view = self.world.view(now);
                let target =
                    self.world.drop_target(tileception_core::Point::new(x, y), win, view).ok_or("no target there")?;
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
    fn parses_commands() {
        assert_eq!(parse("go 3"), Ok(Command::Action(Action::RecallView(3))));
        assert_eq!(parse("move left"), Ok(Command::Action(Action::Move(Dir::Left))));
        assert_eq!(parse("spawn firefox --new-window"), Ok(Command::Spawn("firefox --new-window".into())));
        assert_eq!(parse("drop 10 20.5"), Ok(Command::Drop(10.0, 20.5)));
        assert_eq!(parse("resize left 20"), Ok(Command::Action(Action::Resize(-20.0, 0.0))));
        assert_eq!(parse("step-in pointer"), Ok(Command::Action(Action::StepInPointer)));
        assert_eq!(parse("cycle prev"), Ok(Command::Action(Action::Cycle(false))));
        assert!(parse("go 0").is_err());
        assert!(parse("bogus").is_err());
        assert!(parse("").is_err());
    }
}
