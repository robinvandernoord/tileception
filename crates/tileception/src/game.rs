//! Game mode: while a game has the keyboard, Tileception's own Super
//! bindings step aside so Super and its combinations reach the game.
//!
//! Super+Caps Lock (the `game-mode` command) switches it for the focused
//! window. It also comes on by itself, whichever launcher started the game:
//! for the focused window when it is a game (see [`identify`]) and is
//! fullscreen or holds a pointer lock, and for any fullscreen window that
//! locks the pointer and hides the cursor, as mouse-look does. It ends when
//! the game loses the keyboard, and when one it came on for by itself
//! leaves fullscreen and lets go of the pointer.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use smithay::{
    backend::renderer::gles::GlesRenderer,
    input::{
        keyboard::{Keycode, keysyms},
        pointer::CursorImageStatus,
    },
    reexports::wayland_server::{Resource, protocol::wl_surface::WlSurface},
    wayland::{
        pointer_constraints::{PointerConstraint, with_pointer_constraint},
        seat::WaylandFocus,
    },
};
use tileception_apps::AppIndex;
use tileception_core::{Rect, WindowId};

use crate::{
    Tileception,
    commands::Command,
    config::Mods,
    render::OutputElement,
    ui::{Painter, Style, ease_out, glyph},
};

/// The Caps Lock key's keycode (evdev 58, plus xkb's 8), for the way out
/// on layouts that give it another keysym.
const CAPS_LOCK_KEYCODE: u32 = 66;
/// How long an unchanged window goes before its rules and process are
/// asked again (a title rule, a content type that changed).
const RECHECK: Duration = Duration::from_secs(1);
/// How long the toast stays before it fades.
const TOAST: Duration = Duration::from_millis(2500);
const FADE_IN: f64 = 0.12;
const FADE_OUT: f64 = 0.4;
/// Ancestors looked at before giving up, well past any launcher's depth.
const MAX_ANCESTORS: usize = 32;

/// `tileception msg game-mode ...`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GameModeCommand {
    Toggle,
    On,
    Off,
    Status,
}

pub fn parse(args: &[&str]) -> Result<GameModeCommand, String> {
    match args {
        [] | ["toggle"] => Ok(GameModeCommand::Toggle),
        ["on"] => Ok(GameModeCommand::On),
        ["off"] => Ok(GameModeCommand::Off),
        ["status"] => Ok(GameModeCommand::Status),
        _ => Err("game-mode takes on, off, toggle or status".into()),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Active {
    window: WindowId,
    /// Switched on with the command rather than by itself.
    manual: bool,
    /// The game was fullscreen when it last had the keyboard: if it leaves
    /// fullscreen while another window has it, it stopped playing.
    fullscreen: bool,
}

#[derive(Default)]
pub struct GameMode {
    active: Option<Active>,
    /// Windows game mode was switched off for, which it does not come back
    /// for by itself until that window stops playing or closes; each with
    /// whether it was fullscreen when it last had the keyboard.
    declined: HashMap<WindowId, bool>,
    /// Why each window's process is a game (`None`: it is not), worked out
    /// once per app id: a window that changes it is looked at again.
    identities: HashMap<WindowId, (String, Option<String>)>,
    /// When the toast was shown, and whether it says game mode is on.
    toast: Option<(Instant, bool)>,
    /// The game had the keyboard at the last look, so the toast shows again
    /// when it gets it back.
    game_focused: bool,
    /// What the last look saw (the focused window and what it was doing),
    /// and when: the rules and the process are asked again only when that
    /// changes, or after a second.
    last_seen: Option<(Option<WindowId>, Play)>,
    last_check: Option<Instant>,
    /// The Caps Lock key held down that switched game mode, whose release
    /// is not for the keyboard either.
    chord_key: Option<Keycode>,
}

enum Verdict {
    Game(String),
    NotGame,
    /// A window rule says it is not a game.
    RuledOut,
}

/// What the focused window is doing that game mode looks at.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Play {
    fullscreen: bool,
    locked: bool,
    cursor_hidden: bool,
}

impl Play {
    fn playing(self, game: bool) -> bool {
        (game && (self.fullscreen || self.locked)) || (self.fullscreen && self.locked && self.cursor_hidden)
    }
}

impl Tileception {
    /// The window game mode is on for, if any.
    pub(crate) fn game_window(&self) -> Option<WindowId> {
        self.game.active.map(|active| active.window)
    }

    /// Whether keys go to the game without Tileception's shortcuts: game
    /// mode is on and the game has the keyboard.
    pub(crate) fn game_has_keys(&self) -> bool {
        self.game.active.is_some_and(|active| self.keyboard_window() == Some(active.window))
    }

    /// The window whose surface holds the keyboard: a window's own menus
    /// and subsurfaces count as the window.
    fn keyboard_window(&self) -> Option<WindowId> {
        let focus = self.seat.get_keyboard()?.current_focus()?;
        let surface = focus.wl_surface()?;
        self.window_of_surface(&surface)
    }

    /// The window a surface belongs to, through subsurfaces and popups.
    pub(crate) fn window_of_surface(&self, surface: &WlSurface) -> Option<WindowId> {
        let mut root = surface.clone();
        while let Some(parent) = smithay::wayland::compositor::get_parent(&root) {
            root = parent;
        }
        if let Some(window) = self.window_id(&root) {
            return Some(window);
        }
        let popup = self.popups.find_popup(&root)?;
        self.window_id(&smithay::desktop::find_popup_root_surface(&popup).ok()?)
    }

    pub(crate) fn game_mode_command(&mut self, command: GameModeCommand) -> Result<String, String> {
        // The focused window is the one meant, when game mode is on for
        // another one that does not have the keyboard.
        let on_focused = self.game.active.is_some_and(|active| self.keyboard_window() == Some(active.window));
        match command {
            GameModeCommand::Status => return Ok(self.game_mode_status()),
            GameModeCommand::On if on_focused => {
                // Taken over from coming on by itself: it now stays on until
                // switched off or the game closes.
                if let Some(active) = self.game.active.as_mut() {
                    active.manual = true;
                }
            }
            GameModeCommand::Toggle if on_focused => self.leave_game_mode(true),
            GameModeCommand::Off => self.leave_game_mode(true),
            GameModeCommand::Toggle | GameModeCommand::On => {
                let window = self.keyboard_window().ok_or("game-mode: no window has the keyboard")?;
                self.game.active = None;
                self.enter_game_mode(window, true);
            }
        }
        Ok(self.game_mode_status())
    }

    fn game_mode_status(&mut self) -> String {
        let Some(active) = self.game.active else { return "game mode off".into() };
        let how = if active.manual { "switched on" } else { "by itself" };
        let why = match self.verdict(active.window) {
            Verdict::Game(reason) => reason,
            Verdict::NotGame => "-".into(),
            Verdict::RuledOut => "not a game, by window rule".into(),
        };
        format!("game mode on window {} ({how}); game: {why}", active.window.0)
    }

    fn enter_game_mode(&mut self, window: WindowId, manual: bool) {
        if self.game.active.is_some_and(|active| active.window == window) {
            return;
        }
        tracing::info!(window = window.0, manual, "game mode on");
        let fullscreen = self.play(window).fullscreen;
        self.game.active = Some(Active { window, manual, fullscreen });
        self.game.declined.remove(&window);
        self.game.game_focused = self.keyboard_window() == Some(window);
        self.show_game_toast();
    }

    /// Ends game mode; `declined` keeps it from coming back by itself for
    /// the same window straight away.
    fn leave_game_mode(&mut self, declined: bool) {
        let Some(active) = self.game.active.take() else { return };
        tracing::info!(window = active.window.0, "game mode off");
        if declined {
            let fullscreen = self.fullscreen_on_its_screen(active.window);
            self.game.declined.insert(active.window, fullscreen);
        }
        self.game.toast = Some((Instant::now(), false));
        self.schedule_render();
    }

    /// Super+Caps Lock (any binding of `game-mode` to Caps Lock), taken
    /// before the keyboard state sees it: Caps Lock does not change, and the
    /// program gets neither half. While a game has the keyboard, Super+Caps
    /// Lock leaves game mode even when no binding says so. Returns whether
    /// the key was taken.
    pub(crate) fn game_mode_chord(&mut self, code: Keycode, pressed: bool) -> bool {
        if !pressed {
            if self.game.chord_key == Some(code) {
                self.game.chord_key = None;
                return true;
            }
            return false;
        }
        if self.is_locked() || self.asking_for_shortcut() || self.settings_recording() {
            return false;
        }
        let Some(keyboard) = self.seat.get_keyboard() else { return false };
        let game_keys = self.game_has_keys();
        // A virtual machine or remote desktop holding every key gets this one
        // too, unless it is the game being left.
        if !game_keys && self.shortcuts_inhibited() {
            return false;
        }
        let caps_lock = keyboard.with_xkb_state(self, |context| {
            context.xkb().lock().is_ok_and(|xkb| {
                xkb.raw_syms_for_key_in_layout(code, xkb.active_layout())
                    .iter()
                    .any(|sym| sym.raw() == keysyms::KEY_Caps_Lock)
            })
        });
        // The way out works with Caps Lock given another meaning too.
        if !caps_lock && !(game_keys && code.raw() == CAPS_LOCK_KEYCODE) {
            return false;
        }
        let mut held = Mods::from_state(&keyboard.modifier_state());
        if self.nested && held.alt && !held.logo {
            held.alt = false;
            held.logo = true;
        }
        let bound = self
            .config
            .lookup(held, keysyms::KEY_Caps_Lock)
            .filter(|commands| commands.iter().any(|command| matches!(command, Command::GameMode(_))))
            .map(<[Command]>::to_vec);
        let commands = if game_keys && held == (Mods { logo: true, ..Mods::default() }) {
            // The way out, whatever the bindings say.
            vec![Command::GameMode(GameModeCommand::Off)]
        } else if let Some(commands) = bound {
            commands
        } else {
            return false;
        };
        self.game.chord_key = Some(code);
        // Super was not let go on its own.
        self.tap_armed = None;
        for command in commands {
            if let Err(error) = self.execute(command) {
                tracing::warn!("{error}");
            }
        }
        true
    }

    /// The keys that leave game mode, for the toast: Super+Caps Lock always
    /// does (Alt for Super in a nested window).
    fn game_exit_label(&self) -> &'static str {
        if self.nested { "Alt+Caps Lock" } else { "Super+Caps Lock" }
    }

    /// Super pressed on its own while game mode is on, switching it on, and
    /// coming back to the game.
    pub(crate) fn show_game_toast(&mut self) {
        self.game.toast = Some((Instant::now(), true));
        self.schedule_render();
    }

    /// Whether the pointer is over the game while it has the keyboard: its
    /// clicks and scrolling are the game's too, with Super held or not.
    pub(crate) fn game_has_pointer(&self) -> bool {
        if !self.game_has_keys() {
            return false;
        }
        let focus = self.seat.get_pointer().and_then(|pointer| pointer.current_focus());
        let surface = focus.and_then(|focus| focus.wl_surface().map(|surface| surface.into_owned()));
        surface.and_then(|surface| self.window_of_surface(&surface)) == self.game_window()
    }

    /// Follows the focused window into and out of game mode. Game mode stays
    /// with its game while another window has the keyboard (Super works
    /// there as usual) and shows its toast again when the game gets it
    /// back. It ends when the game closes, or, having come on by itself,
    /// when the game stops playing while it has the keyboard. Switched off,
    /// it stays off for that game until it stops playing or closes.
    ///
    /// Called after every batch of events, so it looks closer only when the
    /// focused window or what it is doing changed, or after [`RECHECK`]; and
    /// asks a window's rules and process only while it is fullscreen or
    /// holds the pointer.
    pub(crate) fn game_mode_refresh(&mut self) {
        let focused = self.keyboard_window();
        let seen = (focused, focused.map(|window| self.play(window)).unwrap_or_default());
        let now = Instant::now();
        if self.game.last_seen == Some(seen) && self.game.last_check.is_some_and(|last| now - last < RECHECK) {
            return;
        }
        self.game.last_seen = Some(seen);
        self.game.last_check = Some(now);
        if let Some(active) = self.game.active.as_mut()
            && focused == Some(active.window)
        {
            active.fullscreen = seen.1.fullscreen;
        }
        if let Some(active) = self.game.active {
            let has_keys = focused == Some(active.window);
            // Without the keyboard, a game cannot hold the pointer, so only
            // leaving fullscreen tells that it stopped.
            let stopped = if has_keys {
                !self.window_playing(active.window)
            } else {
                active.fullscreen && !self.fullscreen_on_its_screen(active.window)
            };
            if !self.windows.contains_key(&active.window) || (!active.manual && stopped) {
                self.leave_game_mode(false);
            } else if has_keys && !self.game.game_focused {
                self.show_game_toast();
            }
            self.game.game_focused = has_keys;
        }
        if !self.game.declined.is_empty() {
            let declined = std::mem::take(&mut self.game.declined);
            // As with game mode itself: without the keyboard, only leaving
            // fullscreen tells that a game stopped.
            self.game.declined = declined
                .into_iter()
                .filter_map(|(window, fullscreen)| {
                    if !self.windows.contains_key(&window) {
                        None
                    } else if focused == Some(window) {
                        self.window_playing(window).then_some((window, seen.1.fullscreen))
                    } else {
                        (!fullscreen || self.fullscreen_on_its_screen(window)).then_some((window, fullscreen))
                    }
                })
                .collect();
        }
        // Another game that is played takes game mode over.
        if let Some(window) = focused
            && self.game.active.is_none_or(|active| active.window != window)
            && !self.game.declined.contains_key(&window)
            && self.window_playing(window)
        {
            self.game.active = None;
            self.enter_game_mode(window, false);
        }
        if self.game.identities.len() > self.windows.len() {
            let windows = &self.windows;
            self.game.identities.retain(|window, _| windows.contains_key(window));
        }
    }

    /// Whether `window` is being played: a game (by rule or by what runs
    /// it) that is fullscreen or holds the pointer, or anything fullscreen
    /// that steers by mouse with the cursor hidden.
    fn window_playing(&mut self, window: WindowId) -> bool {
        let play = self.play(window);
        if !play.fullscreen && !play.locked {
            return false;
        }
        let Some(game) = self.is_game(window) else { return false };
        play.playing(game)
    }

    /// Whether `window` is fullscreen on the screen (or area) it is on,
    /// which need not be the one in use.
    fn fullscreen_on_its_screen(&mut self, window: WindowId) -> bool {
        let Some(area) = self.area_for_window(window) else { return false };
        self.with_area(&area, |state| state.fullscreen == Some(window)).unwrap_or(false)
    }

    fn play(&self, window: WindowId) -> Play {
        let locked = self.windows.get(&window).and_then(|window| window.wl_surface()).is_some_and(|surface| {
            let pointer = self.seat.get_pointer().expect("seat has a pointer");
            with_pointer_constraint(&surface, &pointer, |constraint| {
                constraint.is_some_and(|constraint| {
                    constraint.is_active() && matches!(&*constraint, PointerConstraint::Locked(_))
                })
            })
        });
        Play {
            fullscreen: self.fullscreen == Some(window),
            locked,
            cursor_hidden: matches!(self.cursor_status, CursorImageStatus::Hidden),
        }
    }

    /// Whether a window is a game: `None` when a rule says it is not, which
    /// also keeps game mode from coming on by itself.
    fn is_game(&mut self, window_id: WindowId) -> Option<bool> {
        match self.verdict(window_id) {
            Verdict::Game(_) => Some(true),
            Verdict::NotGame => Some(false),
            Verdict::RuledOut => None,
        }
    }

    /// Whether, and why, a window is a game. Rules and the content type are
    /// asked every time, as they can change; the process once per app id.
    fn verdict(&mut self, window_id: WindowId) -> Verdict {
        let Some(window) = self.windows.get(&window_id) else { return Verdict::NotGame };
        let app_id = crate::state::app_id(window);
        let title = crate::state::window_title(window);
        match crate::rules::decide(&self.config.file.rules, &app_id, &title).game {
            Some(true) => return Verdict::Game("window rule".into()),
            Some(false) => return Verdict::RuledOut,
            None => {}
        }
        if window.wl_surface().is_some_and(|surface| crate::frame_timing::says_game(&surface)) {
            return Verdict::Game("it says so (content type)".into());
        }
        if self.game.identities.get(&window_id).is_none_or(|(seen, _)| *seen != app_id) {
            let pid = window_pid(self, window);
            let reason = identify(Path::new("/proc"), pid, std::process::id(), &app_id, self.launcher.apps());
            tracing::info!(window = window_id.0, app = app_id, pid, reason, "game check");
            self.game.identities.insert(window_id, (app_id, reason));
        }
        match self.game.identities.get(&window_id) {
            Some((_, Some(reason))) => Verdict::Game(reason.clone()),
            _ => Verdict::NotGame,
        }
    }

    /// The toast at the top centre saying game mode is on and how to leave
    /// it, or that it is off: a struck-through, quiet gamepad then, so the
    /// two are told apart at a glance.
    pub fn game_toast_elements(&self, renderer: &mut GlesRenderer) -> Vec<OutputElement> {
        let Some((shown, turned_on)) = self.game.toast else { return Vec::new() };
        let now = Instant::now();
        let until = shown + TOAST;
        if now >= until + Duration::from_secs_f64(FADE_OUT) {
            return Vec::new();
        }
        let theme = &self.theme;
        let appear = ease_out(now.duration_since(shown).as_secs_f64() / FADE_IN);
        let leaving = now.saturating_duration_since(until).as_secs_f64() / FADE_OUT;
        let alpha = (appear * (1.0 - leaving).clamp(0.0, 1.0)) as f32;
        let screen = self.output_rect();
        let width = 340.0_f64.min(screen.w - 24.0);
        let height = 64.0;
        let card = Rect::new(screen.x + (screen.w - width) / 2.0, screen.y + 16.0, width, height);
        let mut painter = Painter::new(self, renderer);
        painter.alpha = alpha;
        painter.offset = (0.0, (appear - 1.0) * 12.0);
        painter.popup(card);
        let icon = Rect::new(card.x + 14.0, card.y + (card.h - 36.0) / 2.0, 36.0, 36.0);
        if turned_on {
            painter.fill(icon, theme.accent.alpha(0.18), 18.0);
            painter.glyph(glyph::GAMEPAD, icon, 18, theme.accent);
        } else {
            painter.fill(icon, theme.surface_raised, 18.0);
            painter.glyph(glyph::GAMEPAD_OFF, icon, 18, theme.muted);
        }
        let text_x = icon.right() + 14.0;
        let text_w = card.right() - 16.0 - text_x;
        painter.text(
            if turned_on { "Game mode on" } else { "Game mode off" },
            Rect::new(text_x, card.y + 11.0, text_w, 22.0),
            Style::new(theme.font_size, theme.text).bold(),
        );
        let keys = if turned_on {
            format!("{} to turn off", self.game_exit_label())
        } else {
            let mods = if self.nested { "Alt" } else { "Super" };
            format!("{mods} shortcuts work again")
        };
        painter.text(
            &keys,
            Rect::new(text_x, card.y + 33.0, text_w, 20.0),
            Style::new(theme.font_size * 0.92, theme.muted),
        );
        painter.finish()
    }
}

/// The process behind a window: the X11 client's own, or the Wayland
/// client's (an X11 window's Wayland client would be Xwayland).
fn window_pid(state: &Tileception, window: &smithay::desktop::Window) -> Option<u32> {
    if let Some(x11) = window.x11_surface() {
        return x11.pid();
    }
    let client = window.toplevel()?.wl_surface().client()?;
    let credentials = client.get_credentials(&state.display_handle).ok()?;
    u32::try_from(credentials.pid).ok()
}

/// What `/proc` says about one process.
#[derive(Debug, Default)]
struct Process {
    exe: Option<PathBuf>,
    cmdline: Vec<String>,
    parent: u32,
    cgroup: String,
}

fn read_process(proc: &Path, pid: u32) -> Option<Process> {
    let dir = proc.join(pid.to_string());
    let stat = std::fs::read_to_string(dir.join("stat")).ok()?;
    let cmdline = std::fs::read(dir.join("cmdline")).unwrap_or_default();
    Some(Process {
        exe: std::fs::read_link(dir.join("exe")).ok(),
        cmdline: cmdline
            .split(|byte| *byte == 0)
            .filter(|arg| !arg.is_empty())
            .map(|arg| String::from_utf8_lossy(arg).into_owned())
            .collect(),
        parent: parent_of(&stat)?,
        cgroup: std::fs::read_to_string(dir.join("cgroup")).unwrap_or_default(),
    })
}

/// The parent pid from `/proc/<pid>/stat`. The name before it is in
/// parentheses and may hold spaces and parentheses itself.
fn parent_of(stat: &str) -> Option<u32> {
    let after_name = &stat[stat.rfind(')')? + 1..];
    after_name.split_whitespace().nth(1)?.parse().ok()
}

/// Wine runs every Windows program, whoever starts it: its loader, or a
/// command line naming a `.exe`.
fn runs_wine(process: &Process) -> bool {
    let exe_name = process.exe.as_deref().and_then(Path::file_name).and_then(|name| name.to_str()).unwrap_or("");
    exe_name.starts_with("wine")
        || process.cmdline.first().is_some_and(|first| first.to_ascii_lowercase().ends_with(".exe"))
}

/// The Flatpak app a process runs in, from its systemd scope
/// (`app-flatpak-com.valvesoftware.Steam-1234.scope`).
fn flatpak_app(cgroup: &str) -> Option<&str> {
    let scope = cgroup.lines().find_map(|line| line.rsplit('/').find(|part| part.starts_with("app-flatpak-")))?;
    let rest = scope.strip_prefix("app-flatpak-")?.strip_suffix(".scope")?;
    Some(rest.rsplit_once('-').map_or(rest, |(app, _)| app))
}

/// The desktop entries that may describe a process: its executable, and its
/// command line's first word, or the script an interpreter runs.
fn entries_for<'a>(process: &Process, apps: &'a AppIndex) -> Vec<&'a tileception_apps::App> {
    let mut programs: Vec<&Path> = process.exe.iter().map(PathBuf::as_path).collect();
    programs.extend(process.cmdline.iter().take(2).filter(|arg| !arg.starts_with('-')).map(Path::new));
    programs.into_iter().filter_map(|program| apps.by_program(program)).collect()
}

fn program_name(process: &Process) -> String {
    let program = process.exe.as_deref().or_else(|| process.cmdline.first().map(Path::new));
    program.and_then(Path::file_name).map_or_else(|| "?".into(), |name| name.to_string_lossy().into_owned())
}

fn is_game_entry(app: &tileception_apps::App) -> bool {
    app.categories.iter().any(|category| category == "Game")
}

/// Why the window of process `pid` with `app_id` is a game, or `None`.
/// A game is whatever runs under Wine, or whatever has a desktop entry in
/// the `Game` category (its own, by app id, Flatpak or program), or was
/// started, however indirectly, by a program that has one: Steam, Lutris,
/// Heroic or a game's own launcher. The search stops at `own_pid`, so
/// programs started from Tileception's launcher are not all games.
fn identify(proc: &Path, pid: Option<u32>, own_pid: u32, app_id: &str, apps: &AppIndex) -> Option<String> {
    if let Some(app) = apps.get(&format!("{app_id}.desktop")).filter(|app| is_game_entry(app)) {
        return Some(format!("{} is a game", app.id));
    }
    let mut pid = pid?;
    let first = read_process(proc, pid)?;
    if runs_wine(&first) {
        return Some("runs under Wine".into());
    }
    if let Some(app) = flatpak_app(&first.cgroup)
        .and_then(|app_id| apps.get(&format!("{app_id}.desktop")))
        .filter(|app| is_game_entry(app))
    {
        return Some(format!("Flatpak {} is a game", app.id));
    }
    let mut process = first;
    for depth in 0..MAX_ANCESTORS {
        if let Some(app) = entries_for(&process, apps).into_iter().find(|app| is_game_entry(app)) {
            // Named by the program, not the entry: a launcher such as Steam
            // is also run by every game shortcut it makes.
            return Some(if depth == 0 {
                format!("{} is a game", app.id)
            } else {
                format!("started by {} (a game launcher)", program_name(&process))
            });
        }
        pid = process.parent;
        if pid <= 1 || pid == own_pid {
            return None;
        }
        process = read_process(proc, pid)?;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_commands() {
        assert_eq!(parse(&[]), Ok(GameModeCommand::Toggle));
        assert_eq!(parse(&["on"]), Ok(GameModeCommand::On));
        assert_eq!(parse(&["status"]), Ok(GameModeCommand::Status));
        assert!(parse(&["sideways"]).is_err());
    }

    #[test]
    fn reads_the_parent_past_odd_names() {
        assert_eq!(parent_of("1234 (hl_linux) S 1200 1234 1234 0"), Some(1200));
        assert_eq!(parent_of("77 (Web Content (x)) R 5 77"), Some(5));
        assert_eq!(parent_of("garbage"), None);
    }

    #[test]
    fn finds_flatpak_apps() {
        let cgroup = "0::/user.slice/user-1000.slice/user@1000.service/app.slice/app-flatpak-com.valvesoftware.Steam-4021.scope\n";
        assert_eq!(flatpak_app(cgroup), Some("com.valvesoftware.Steam"));
        assert_eq!(flatpak_app("0::/user.slice/session-2.scope\n"), None);
    }

    #[test]
    fn mouse_look_counts_without_a_known_game() {
        let play = |fullscreen, locked, cursor_hidden| Play { fullscreen, locked, cursor_hidden };
        assert!(play(true, false, false).playing(true));
        assert!(play(false, true, false).playing(true));
        assert!(!play(false, false, false).playing(true));
        // A video player: fullscreen, but it never takes the pointer.
        assert!(!play(true, false, true).playing(false));
        assert!(play(true, true, true).playing(false));
        assert!(!play(false, true, true).playing(false));
    }

    /// A pretend `/proc` and applications folder.
    struct Fixture {
        root: PathBuf,
    }

    impl Fixture {
        fn new(name: &str) -> Self {
            let root = std::env::temp_dir().join(format!("tileception-game-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&root);
            std::fs::create_dir_all(root.join("proc")).expect("fixture dir");
            std::fs::create_dir_all(root.join("applications")).expect("fixture dir");
            Self { root }
        }

        fn process(&self, pid: u32, parent: u32, exe: &str, cmdline: &[&str]) {
            let dir = self.root.join("proc").join(pid.to_string());
            std::fs::create_dir_all(&dir).expect("process dir");
            std::fs::write(dir.join("stat"), format!("{pid} (x) S {parent} 0 0")).expect("stat");
            std::fs::write(dir.join("cmdline"), cmdline.join("\0")).expect("cmdline");
            std::os::unix::fs::symlink(exe, dir.join("exe")).expect("exe link");
        }

        fn entry(&self, desktop_id: &str, exec: &str, categories: &str) {
            let text =
                format!("[Desktop Entry]\nType=Application\nName={desktop_id}\nExec={exec}\nCategories={categories}\n");
            std::fs::write(self.root.join("applications").join(format!("{desktop_id}.desktop")), text).expect("entry");
        }

        fn apps(&self) -> AppIndex {
            AppIndex::scan_with_options(&[self.root.join("applications")], "Tileception", "", &[])
        }

        fn proc(&self) -> PathBuf {
            self.root.join("proc")
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn a_launchers_category_reaches_its_games() {
        let fixture = Fixture::new("launcher");
        fixture.entry("steam", "/usr/bin/steam %U", "Network;FileTransfer;Game;");
        fixture.entry("vlc", "/usr/bin/vlc %U", "AudioVideo;Player;");
        fixture.process(100, 1, "/home/u/.local/share/Steam/steamrt64/steam", &["steam", "-silent"]);
        fixture.process(200, 100, "/usr/bin/bash", &["/bin/sh", "-c", "reaper"]);
        fixture.process(300, 200, "/games/Half-Life/hl_linux", &["/games/Half-Life/hl_linux", "-steam"]);
        fixture.process(400, 50, "/usr/bin/vlc", &["/usr/bin/vlc", "film.mkv"]);
        fixture.process(50, 1, "/usr/bin/tileception", &["tileception"]);
        let apps = fixture.apps();
        let reason = identify(&fixture.proc(), Some(300), 50, "hl_linux", &apps);
        assert_eq!(reason.as_deref(), Some("started by steam (a game launcher)"));
        assert_eq!(identify(&fixture.proc(), Some(400), 50, "vlc", &apps), None);
    }

    #[test]
    fn the_search_stops_at_tileception() {
        let fixture = Fixture::new("stop");
        // Tileception itself started from something filed as a game.
        fixture.entry("arcade", "/usr/bin/arcade-session", "Game;");
        fixture.process(10, 1, "/usr/bin/arcade-session", &["arcade-session"]);
        fixture.process(50, 10, "/usr/bin/tileception", &["tileception"]);
        fixture.process(60, 50, "/usr/bin/foot", &["foot"]);
        assert_eq!(identify(&fixture.proc(), Some(60), 50, "foot", &fixture.apps()), None);
    }

    #[test]
    fn wine_and_own_entries_are_games() {
        let fixture = Fixture::new("wine");
        fixture.entry("org.example.Puzzle", "puzzle", "Game;LogicGame;");
        fixture.process(70, 1, "/usr/lib/wine/wine64-preloader", &["C:\\Games\\Hitman.Exe"]);
        fixture.process(80, 1, "/usr/bin/puzzle", &["puzzle"]);
        let apps = fixture.apps();
        assert_eq!(identify(&fixture.proc(), Some(70), 50, "hitman.exe", &apps).as_deref(), Some("runs under Wine"));
        assert_eq!(
            identify(&fixture.proc(), None, 50, "org.example.Puzzle", &apps).as_deref(),
            Some("org.example.Puzzle.desktop is a game")
        );
        assert_eq!(
            identify(&fixture.proc(), Some(80), 50, "", &apps).as_deref(),
            Some("org.example.Puzzle.desktop is a game")
        );
    }
}
