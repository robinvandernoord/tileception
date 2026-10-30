//! The launcher (tap Super) and the welcome screen: app search with a small
//! persistent usage history, plus open windows, Tileception's own commands,
//! desktop actions, instant answers (sums, conversions, times, colours,
//! characters), paths and links, and `>` to run a command and see its output.

use std::{
    collections::HashMap,
    path::PathBuf,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};
use smithay::{
    backend::renderer::gles::GlesRenderer,
    input::keyboard::{keysyms, xkb},
};
use tileception_apps::{AppIndex, IconLookup, Usage};
use tileception_core::{Point, Rect, WindowId};

use smithay::wayland::selection::{
    SelectionTarget,
    data_device::{
        SelectionRequestError, current_data_device_selection_userdata, request_data_device_client_selection,
        set_data_device_selection,
    },
};

use crate::{
    Tileception,
    answers::{self, Answer, Look, Place},
    power::PowerAction,
    render::OutputElement,
    shapes::Shape,
    theme::Colour,
    ui::{Align, Icons, LocalTime, Painter, Style, ease_out, glyph},
};

/// Clipboard contents the compositor owns itself: launcher text, a
/// screenshot from `tileception msg copy-image`, or what another machine
/// offers (fetched when pasted).
#[derive(Debug, Clone)]
pub enum Clip {
    Text(String),
    Png(std::sync::Arc<[u8]>),
    Remote(crate::handoff::RemoteClip),
}

impl Clip {
    pub fn text(&self) -> Option<&str> {
        match self {
            Clip::Text(text) => Some(text),
            Clip::Png(_) | Clip::Remote(_) => None,
        }
    }

    /// Serves the contents without starting a Wayland client. Another
    /// machine's selection goes through `Tileception::handoff_fetch` instead.
    pub fn send(&self, pipe: std::os::fd::OwnedFd) {
        use std::io::Write;
        let clip = self.clone();
        std::thread::spawn(move || {
            let bytes = match &clip {
                Clip::Text(text) => text.as_bytes(),
                Clip::Png(png) => png,
                Clip::Remote(_) => return,
            };
            let _ = std::fs::File::from(pipe).write_all(bytes);
        });
    }
}

#[derive(Default, Serialize, Deserialize)]
struct StoredUsage {
    count: u32,
    last_used: u64,
}

/// Where a paste goes: the launcher as opened then, or the doctor (which
/// also takes pictures).
#[derive(Debug, Clone, Copy)]
pub enum Paste {
    Launcher(u64),
    Doctor,
}

/// One row of results.
#[derive(Debug, Clone, PartialEq)]
enum Entry {
    /// A desktop entry, by id.
    App(String),
    /// One of an app's desktop actions, by app id and index.
    Action { app: String, action: usize },
    /// An open or minimized window: Enter flies there.
    Window(WindowId),
    /// One of [`COMMANDS`].
    Command(usize),
    /// Something Enter copies.
    Answer(Answer),
    /// A file, folder or link Enter opens.
    Place(Place),
    /// `>`: the command line to run.
    Run(String),
    /// "Ask Tileception", with the question.
    Ask(String),
}

/// What a command row does.
#[derive(Debug, Clone, Copy)]
enum Does {
    /// A `tileception msg` command line.
    Line(&'static str),
    Suspend,
    /// Opens the power menu on this button, so ending the session takes a
    /// second Enter.
    Power(PowerAction),
}

struct Builtin {
    name: &'static str,
    /// More words it is found by.
    words: &'static str,
    glyph: &'static str,
    does: Does,
}

const fn builtin(name: &'static str, words: &'static str, glyph: &'static str, does: Does) -> Builtin {
    Builtin { name, words, glyph, does }
}

/// Tileception's own commands, found by typing their name.
const COMMANDS: &[Builtin] = &[
    builtin("Overview", "show everything all windows zoom out", glyph::OVERVIEW, Does::Line("overview")),
    builtin("Lock screen", "lock", glyph::LOCK, Does::Line("lock")),
    builtin("Suspend", "sleep", glyph::MOON, Does::Suspend),
    builtin("Log out", "sign out exit quit leave", glyph::LOG_OUT, Does::Power(PowerAction::LogOut)),
    builtin("Restart", "reboot", glyph::RESTART, Does::Power(PowerAction::Restart)),
    builtin("Restart into UEFI", "bios firmware setup reboot", glyph::RESTART, Does::Power(PowerAction::Uefi)),
    builtin("Power off", "shut down shutdown turn off", glyph::POWER, Does::Power(PowerAction::PowerOff)),
    builtin("Do not disturb", "dnd quiet silence notifications", glyph::BELL, Does::Line("do-not-disturb")),
    builtin("Notifications", "history messages", glyph::BELL, Does::Line("notifications")),
    builtin("Take a screenshot", "print screen capture", glyph::CAMERA, Does::Line("spawn tileception-screenshot")),
    builtin(
        "Screenshot of a region",
        "print screen capture area select",
        glyph::CAMERA,
        Does::Line("spawn tileception-screenshot region --ask"),
    ),
    builtin(
        "Dark style for apps",
        "dark mode theme colour color scheme night",
        glyph::CONTRAST,
        Does::Line("colour-scheme dark"),
    ),
    builtin(
        "Light style for apps",
        "light mode theme colour color scheme day",
        glyph::CONTRAST,
        Does::Line("colour-scheme light"),
    ),
    builtin("Reload configuration", "config settings refresh", glyph::RESTART, Does::Line("reload")),
    builtin("Ask Tileception", "doctor help settings", glyph::CHAT, Does::Line("doctor")),
    builtin("Rubber duck", "quack pet debugging", glyph::DUCK, Does::Line("duck")),
    builtin("Default terminal", "terminal shell console", glyph::TERMINAL, Does::Line("terminal")),
    builtin("Maximize window", "frame alone", glyph::WINDOW, Does::Line("maximize")),
    builtin("Minimize window", "hide", glyph::WINDOW, Does::Line("minimize")),
    builtin("Restore minimized window", "unminimize bring back", glyph::WINDOW, Does::Line("restore")),
    builtin("Float or dock window", "floating tile", glyph::WINDOW, Does::Line("float")),
    builtin("Fullscreen", "full screen", glyph::WINDOW, Does::Line("fullscreen")),
    builtin("Close window", "quit", glyph::CLOSE, Does::Line("close")),
    builtin("Toggle split", "stack side by side rotate", glyph::OVERVIEW, Does::Line("toggle-split")),
    builtin("Step in", "zoom in", glyph::ZOOM_IN, Does::Line("step-in")),
    builtin("Step out", "zoom out", glyph::ZOOM_OUT, Does::Line("step-out")),
    builtin("Back", "previous view history", glyph::ARROW_LEFT, Does::Line("back")),
    builtin("Forward", "next view history", glyph::ARROW_RIGHT, Does::Line("forward")),
    builtin("Show desktop", "desktop tile", glyph::OVERVIEW, Does::Line("desktop")),
];

/// A command started with `>` and what it printed.
struct Run {
    line: String,
    started: Instant,
    output: Option<RunOutput>,
}

#[derive(Debug, Clone)]
pub struct RunOutput {
    text: String,
    status: RunStatus,
    truncated: bool,
}

#[derive(Debug, Clone)]
enum RunStatus {
    Exited(i32),
    Signal(i32),
    TimedOut,
    Failed(String),
}

pub struct Launcher {
    pub open: bool,
    /// Open as the welcome screen on an empty desktop.
    pub home: bool,
    /// Super put the welcome screen away until the desktop next becomes empty.
    home_dismissed: bool,
    /// Just launched something from the welcome screen: give it time to open
    /// a window before the welcome screen comes back.
    quiet_until: Option<Instant>,
    input: crate::text_input::TextInput,
    edited: Instant,
    opened: Instant,
    selecting: bool,
    clipboard: Option<smithay::reexports::calloop::channel::Sender<(Paste, Clip)>>,
    finished: Option<smithay::reexports::calloop::channel::Sender<(u64, String, RunOutput)>>,
    epoch: u64,
    selected: usize,
    first: usize,
    /// The rows for what is typed, worked out when it changes.
    entries: Vec<Entry>,
    /// What Enter last copied, and when, to say so on its row.
    copied: Option<(String, Instant)>,
    run: Option<Run>,
    /// Lines the output is scrolled back from its end.
    output_scroll: usize,
    index: AppIndex,
    lookup: IconLookup,
    pub icons: Icons,
    usage: HashMap<String, Usage>,
    compose: Option<xkb::compose::State>,
}

impl Default for Launcher {
    fn default() -> Self {
        let usage: HashMap<String, StoredUsage> =
            std::fs::read_to_string(usage_path()).ok().and_then(|text| toml::from_str(&text).ok()).unwrap_or_default();
        Self {
            open: false,
            home: false,
            home_dismissed: false,
            quiet_until: None,
            input: Default::default(),
            edited: Instant::now(),
            opened: Instant::now(),
            selecting: false,
            clipboard: None,
            finished: None,
            epoch: 0,
            selected: 0,
            first: 0,
            entries: Vec::new(),
            copied: None,
            run: None,
            output_scroll: 0,
            index: AppIndex::scan_default(),
            lookup: IconLookup::new("hicolor", &IconLookup::default_dirs()),
            icons: Icons::default(),
            compose: crate::ui::compose_state(),
            usage: usage
                .into_iter()
                .map(|(app_id, value)| {
                    (app_id, Usage { count: value.count, last_used: UNIX_EPOCH + Duration::from_secs(value.last_used) })
                })
                .collect(),
        }
    }
}

impl Launcher {
    pub fn app_icon(
        &self,
        app_id: &str,
        size: u32,
    ) -> Option<smithay::backend::renderer::element::memory::MemoryRenderBuffer> {
        let desktop_id = if app_id.ends_with(".desktop") { app_id.to_owned() } else { format!("{app_id}.desktop") };
        let icon = self.index.get(&desktop_id).and_then(|app| app.icon.as_deref()).unwrap_or(app_id);
        self.icon(icon, size)
    }

    fn icon(&self, name: &str, size: u32) -> Option<smithay::backend::renderer::element::memory::MemoryRenderBuffer> {
        let path = self.lookup.find(name, self.icons.pixels(size), 1)?;
        self.icons.image(&path, size)
    }

    /// The command output to show: that of the command typed now.
    fn shown_run(&self) -> Option<&Run> {
        let line = answers::command(&self.input.text)?;
        self.run.as_ref().filter(|run| run.line == line)
    }

    /// Whether Enter just copied this.
    fn just_copied(&self, text: &str) -> bool {
        self.copied.as_ref().is_some_and(|(copied, copied_at)| copied == text && copied_at.elapsed() < COPIED_FOR)
    }
}

impl Tileception {
    /// Puts a PNG file on the clipboard, owned by the compositor, so copying a
    /// screenshot needs no helper window.
    pub fn copy_image(&mut self, path: &std::path::Path) -> Result<(), String> {
        use std::io::Read;
        let mut png = Vec::new();
        std::fs::File::open(path)
            .and_then(|file| file.take(MAX_IMAGE + 1).read_to_end(&mut png))
            .map_err(|err| format!("{}: {err}", path.display()))?;
        if png.len() as u64 > MAX_IMAGE || !png.starts_with(b"\x89PNG") {
            return Err(format!("{} is not a PNG of at most 64 MB", path.display()));
        }
        let mime_types = vec!["image/png".to_string()];
        set_data_device_selection(&self.display_handle, &self.seat, mime_types.clone(), Some(Clip::Png(png.into())));
        self.selection_offered(SelectionTarget::Clipboard, Some(mime_types.clone()));
        if let Some(xwm) = self.xwm.as_mut()
            && let Err(error) = xwm.new_selection(SelectionTarget::Clipboard, Some(mime_types))
        {
            tracing::warn!("could not offer the image to X11: {error:?}");
        }
        Ok(())
    }

    /// Puts text on the clipboard, owned by the compositor, for Wayland and X11.
    pub(crate) fn copy_text(&mut self, text: String) {
        let mime_types = vec!["text/plain;charset=utf-8".into(), "text/plain".into()];
        set_data_device_selection(&self.display_handle, &self.seat, mime_types.clone(), Some(Clip::Text(text)));
        self.selection_offered(SelectionTarget::Clipboard, Some(mime_types.clone()));
        if let Some(xwm) = self.xwm.as_mut()
            && let Err(error) = xwm.new_selection(SelectionTarget::Clipboard, Some(mime_types))
        {
            tracing::warn!("could not offer the text to X11: {error:?}");
        }
    }

    pub fn init_launcher(&mut self) {
        use smithay::reexports::calloop::channel;
        let (sender, receiver) = channel::channel::<(Paste, Clip)>();
        self.launcher.clipboard = Some(sender);
        self.loop_handle
            .insert_source(receiver, |event, _, state| {
                if let channel::Event::Msg((target, clip)) = event {
                    state.insert_paste(target, clip);
                }
            })
            .expect("clipboard event source");
        let (sender, receiver) = channel::channel::<(u64, String, RunOutput)>();
        self.launcher.finished = Some(sender);
        self.loop_handle
            .insert_source(receiver, |event, _, state| {
                if let channel::Event::Msg((epoch, line, output)) = event {
                    state.run_finished(epoch, line, output);
                }
            })
            .expect("command output event source");
    }

    /// A paste arriving, if what it was meant for is still there.
    fn insert_paste(&mut self, target: Paste, clip: Clip) {
        match (target, clip) {
            (Paste::Launcher(epoch), Clip::Text(text)) if self.launcher.open && self.launcher.epoch == epoch => {
                self.launcher_insert_paste(&text);
            }
            (Paste::Doctor, Clip::Text(text)) if self.doctor.open => self.doctor_insert_paste(&text),
            (Paste::Doctor, Clip::Png(png)) if self.doctor.open => self.doctor_attach(png),
            _ => return,
        }
        self.schedule_render();
    }

    pub fn toggle_launcher(&mut self) {
        if self.is_locked() {
            return;
        }
        if self.launcher.open {
            if self.launcher.home {
                self.launcher.home_dismissed = true;
            }
            self.close_launcher();
            return;
        }
        self.open_launcher(false);
    }

    /// Shows the launcher as the welcome screen while nothing is open.
    /// Minimized windows and windows that never showed do not count: what
    /// matters is an empty layout.
    pub fn update_home(&mut self) {
        // Other machines' windows shown here count too.
        if self
            .windows
            .keys()
            .chain(self.handoff.windows.remote.keys())
            .any(|window_id| self.world.contains_window(*window_id))
        {
            self.launcher.home_dismissed = false;
            self.launcher.quiet_until = None;
            if self.launcher.home {
                self.close_launcher();
            }
            return;
        }
        let welcome = self.config.file.general.welcome && self.shell().launcher().is_builtin();
        if self.launcher.home && !welcome {
            self.close_launcher();
        }
        let quiet = self.launcher.quiet_until.is_some_and(|until| Instant::now() < until);
        if welcome
            && !self.launcher.open
            && !self.launcher.home_dismissed
            && !quiet
            && !self.is_locked()
            && self.power_menu.is_none()
            && !self.doctor.open
        {
            self.open_launcher(true);
        }
    }

    fn open_launcher(&mut self, home: bool) {
        if self.launcher.index.changed_since_scan() {
            self.launcher.index.refresh();
        }
        self.power_menu = None;
        self.launcher.open = true;
        self.launcher.home = home;
        self.launcher.selecting = false;
        self.launcher.epoch = self.launcher.epoch.wrapping_add(1);
        self.launcher.input.clear();
        self.launcher.edited = Instant::now();
        self.launcher.opened = Instant::now();
        self.launcher.copied = None;
        self.launcher.run = None;
        if let Some(compose) = &mut self.launcher.compose {
            compose.reset();
        }
        self.refresh_launcher();
        self.force_keyboard_focus_sync();
        let pointer = self.seat.get_pointer().expect("seat has a pointer");
        let serial = smithay::utils::SERIAL_COUNTER.next_serial();
        pointer.motion(self, None, &smithay::input::pointer::MotionEvent { location: self.pointer, serial, time: 0 });
        pointer.frame(self);
    }

    pub fn close_launcher(&mut self) {
        self.launcher.open = false;
        self.launcher.home = false;
        self.launcher.selecting = false;
        self.force_keyboard_focus_sync();
    }

    /// Works out the rows for what is typed now.
    fn refresh_launcher(&mut self) {
        self.launcher.entries = self.launcher_entries();
        self.launcher.selected = 0;
        self.launcher.first = 0;
    }

    fn launcher_entries(&self) -> Vec<Entry> {
        let text = self.launcher.input.text.trim();
        let launcher = &self.launcher;
        if text.is_empty() {
            return launcher
                .index
                .search("", &launcher.usage, MAX_ROWS)
                .iter()
                .map(|app| Entry::App(app.id.clone()))
                .collect();
        }
        if let Some(line) = answers::command(text) {
            return if line.is_empty() { Vec::new() } else { vec![Entry::Run(line.to_string())] };
        }
        let now = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |elapsed| elapsed.as_secs() as i64);
        let found = answers::answers(text, now);
        if text.starts_with(':') {
            return found.into_iter().map(Entry::Answer).collect();
        }
        let home = PathBuf::from(std::env::var_os("HOME").unwrap_or_default());
        let places = answers::places(text, &home);
        if !places.is_empty() || text.starts_with(['/', '~']) {
            return places.into_iter().map(Entry::Place).collect();
        }

        // Apps, windows, actions and commands, best matches first, with apps
        // ahead of the rest when they match as well.
        let query = text.to_lowercase();
        let terminal_matches = query.chars().count() >= 2
            && (quality(&query, "default terminal").is_some()
                || ["terminal", "shell", "console"].iter().any(|word| word.starts_with(&query)));
        let mut ranked: Vec<(u8, u8, Entry)> = Vec::new();
        for app in launcher.index.search(text, &launcher.usage, MAX_ROWS) {
            // A generic desktop alias for the configured terminal duplicates
            // our action. Keep named terminal applications as separate choices.
            if terminal_matches && app.name.eq_ignore_ascii_case("terminal") && app.exec.trim() == self.terminal.trim()
            {
                continue;
            }
            let quality = quality(&query, &app.name.to_lowercase()).unwrap_or(4);
            ranked.push((quality, 0, Entry::App(app.id.clone())));
        }
        let mut windows: Vec<(u8, WindowId)> = self
            .windows
            .iter()
            .filter(|(window_id, _)| self.world.contains_window(**window_id) || self.world.is_minimized(**window_id))
            .filter_map(|(window_id, window)| {
                let title = crate::state::window_title(window).to_lowercase();
                let app = crate::state::app_id(window).to_lowercase();
                let best = quality(&query, &title).into_iter().chain(quality(&query, &app)).min()?;
                Some((best, *window_id))
            })
            .collect();
        windows.sort_by_key(|(quality, window_id)| (*quality, window_id.0));
        ranked.extend(windows.into_iter().take(3).map(|(quality, window_id)| (quality, 1, Entry::Window(window_id))));
        if query.chars().count() >= 2 {
            let commands = COMMANDS
                .iter()
                .enumerate()
                .filter(|(_, command)| {
                    !matches!(command.does, Does::Suspend) || self.config.file.general.suspend_enabled
                })
                .filter(|(_, command)| !matches!(command.does, Does::Line("doctor")) || self.shell().doctor())
                .filter_map(|(index, command)| {
                    let by_name = quality(&query, &command.name.to_lowercase());
                    let by_words = query
                        .split_whitespace()
                        .all(|word| command.words.split(' ').any(|keyword| keyword.starts_with(word)));
                    let quality = by_name.or(by_words.then_some(3))?;
                    Some((quality, 2, Entry::Command(index)))
                })
                .take(3);
            ranked.extend(commands);
            for (app, action) in launcher.index.search_actions(text, 3) {
                let index = app.actions.iter().position(|app_action| app_action.id == action.id).unwrap_or(0);
                let quality = quality(&query, &action.name.to_lowercase()).unwrap_or(3);
                ranked.push((quality, 3, Entry::Action { app: app.id.clone(), action: index }));
            }
        }
        // A stable sort keeps each kind's own order among equals.
        ranked.sort_by_key(|(quality, kind, _)| (*quality, *kind));

        let mut entries: Vec<Entry> = found.into_iter().map(Entry::Answer).collect();
        let room = MAX_ROWS.saturating_sub(entries.len()).max(2);
        entries.extend(ranked.into_iter().take(room).map(|(_, _, entry)| entry));
        // Sums and conversions are answers, not questions for the doctor.
        let answered = entries.iter().any(|entry| matches!(entry, Entry::Answer(_)));
        if !answered
            && !crate::calc::looks_like_conversion(text)
            && let Some(question) = question_in(text)
        {
            entries.push(Entry::Ask(question));
        }
        entries
    }

    pub fn launcher_key(&mut self, sym: u32, mods: crate::config::Mods) {
        self.launcher.edited = Instant::now();
        let before = self.launcher.input.text.clone();
        self.launcher_key_inner(sym, mods);
        if self.launcher.open && self.launcher.input.text != before {
            self.refresh_launcher();
        }
    }

    fn launcher_key_inner(&mut self, sym: u32, mods: crate::config::Mods) {
        let word = mods.ctrl || mods.alt;
        // On the welcome screen's grid, arrows move between the tiles.
        if self.launcher.home && self.launcher.input.text.is_empty() {
            match sym {
                keysyms::KEY_Left => return self.launcher_step(-1),
                keysyms::KEY_Right => return self.launcher_step(1),
                keysyms::KEY_Up => return self.home_row_step(-1),
                keysyms::KEY_Down => return self.home_row_step(1),
                _ => {}
            }
        }
        // Up and down scroll a command's output, a line or a page at a time.
        if let Some(lines) =
            self.launcher.shown_run().and_then(|run| run.output.as_ref()).map(|output| output.text.lines().count())
        {
            let room = self.launcher_output_lines();
            let page = room.saturating_sub(1).max(1);
            let most = lines.saturating_sub(room);
            let scroll = &mut self.launcher.output_scroll;
            match sym {
                keysyms::KEY_Up => return *scroll = (*scroll + 1).min(most),
                keysyms::KEY_Down => return *scroll = scroll.saturating_sub(1),
                keysyms::KEY_Page_Up => return *scroll = (*scroll + page).min(most),
                keysyms::KEY_Page_Down => return *scroll = scroll.saturating_sub(page),
                _ => {}
            }
        }
        match sym {
            // The welcome screen is the desktop, not something to close:
            // Esc only clears the search there. Super still hides it.
            keysyms::KEY_Escape if self.launcher.home => self.launcher.input.clear(),
            keysyms::KEY_Escape => self.close_launcher(),
            keysyms::KEY_Return | keysyms::KEY_KP_Enter => self.launch_selected(mods),
            // Tab completes a path; otherwise it moves like the arrows.
            keysyms::KEY_Tab if !mods.shift && self.selected_completion().is_some() => {
                if let Some(completion) = self.selected_completion() {
                    self.launcher.input.clear();
                    self.launcher.input.insert(&completion);
                }
            }
            keysyms::KEY_Up | keysyms::KEY_ISO_Left_Tab => self.launcher_step(-1),
            keysyms::KEY_Down | keysyms::KEY_Tab => self.launcher_step(1),
            keysyms::KEY_Left => self.launcher.input.move_horizontal(false, word, mods.shift),
            keysyms::KEY_Right => self.launcher.input.move_horizontal(true, word, mods.shift),
            keysyms::KEY_Home => self.launcher.input.move_to(0, mods.shift),
            keysyms::KEY_End => self.launcher.input.move_to(self.launcher.input.text.len(), mods.shift),
            keysyms::KEY_BackSpace => self.launcher.input.delete(false, word),
            keysyms::KEY_Delete => self.launcher.input.delete(true, word),
            _ if mods.ctrl && matches!(sym, keysyms::KEY_a | keysyms::KEY_A) => self.launcher.input.select_all(),
            _ if mods.ctrl && matches!(sym, keysyms::KEY_c | keysyms::KEY_C | keysyms::KEY_x | keysyms::KEY_X) => {
                let range = self.launcher.input.selection();
                if !range.is_empty() {
                    let text = self.launcher.input.text[range].to_owned();
                    self.copy_text(text);
                    if matches!(sym, keysyms::KEY_x | keysyms::KEY_X) {
                        self.launcher.input.insert("");
                    }
                }
            }
            _ if mods.ctrl && matches!(sym, keysyms::KEY_v | keysyms::KEY_V) => {
                self.paste(Paste::Launcher(self.launcher.epoch));
            }
            _ if mods.ctrl && matches!(sym, keysyms::KEY_u | keysyms::KEY_U) => self.launcher.input.clear(),
            _ if !mods.ctrl && !mods.logo && !mods.alt => {
                if let Some(text) = crate::ui::compose_text(&mut self.launcher.compose, sym) {
                    self.launcher.input.insert(&text);
                }
            }
            _ => {}
        }
    }

    fn selected_completion(&self) -> Option<String> {
        match self.launcher.entries.get(self.launcher.selected) {
            Some(Entry::Place(place)) => place.completion.clone(),
            _ => None,
        }
    }

    /// Pastes the clipboard's text into the launcher's or the doctor's
    /// field, or a picture into the doctor's next message.
    pub fn paste(&mut self, target: Paste) {
        use std::os::unix::net::UnixStream;
        let owned = current_data_device_selection_userdata(&self.seat).and_then(|data| data.clone());
        if let Some(clip) = owned.as_ref().filter(|clip| !matches!(clip, Clip::Remote(_))) {
            self.insert_paste(target, clip.clone());
            return;
        }
        let Some(sender) = self.launcher.clipboard.clone() else { return };
        let Ok((reader, writer)) = UnixStream::pair() else { return };
        // Bound stalled or oversized transfers without blocking the compositor.
        if reader.set_read_timeout(Some(Duration::from_secs(3))).is_err() {
            return;
        }
        let mut requested = false;
        let text = ["text/plain;charset=utf-8", "text/plain", "UTF8_STRING", "STRING"];
        // Pictures first where they are welcome: a copied image often also
        // offers its address as text.
        let mime_types = match target {
            Paste::Doctor => [&["image/png"][..], &text].concat(),
            Paste::Launcher(_) => text.to_vec(),
        };
        if let Some(Clip::Remote(remote)) = owned {
            let Some(wanted) =
                mime_types.into_iter().find(|mime_type| remote.mime_types.iter().any(|offered| offered == mime_type))
            else {
                return;
            };
            self.handoff_fetch(&remote, wanted.into(), writer.into());
            self.launcher_read_paste(reader, sender, target, wanted.starts_with("image/"));
            return;
        }
        let mut wanted = "";
        for mime_type in mime_types {
            wanted = mime_type;
            let Ok(pipe) = writer.try_clone() else { return };
            match request_data_device_client_selection(&self.seat, mime_type.into(), pipe.into()) {
                Ok(()) => {
                    requested = true;
                    break;
                }
                Err(SelectionRequestError::InvalidMimetype) => continue,
                // From X11, where offered types are not known: text only.
                Err(SelectionRequestError::ServerSideSelection) if mime_type.starts_with("image/") => continue,
                Err(SelectionRequestError::ServerSideSelection) => {
                    if let Some(xwm) = self.xwm.as_mut() {
                        requested = xwm
                            .send_selection(
                                SelectionTarget::Clipboard,
                                mime_type.into(),
                                writer.into(),
                                self.loop_handle.clone(),
                            )
                            .is_ok();
                    }
                    // This writer is transferred to Xwayland or dropped here.
                    if requested {
                        self.launcher_read_paste(reader, sender, target, false);
                    }
                    return;
                }
                Err(SelectionRequestError::NoSelection) => return,
            }
        }
        drop(writer);
        if requested {
            self.launcher_read_paste(reader, sender, target, wanted.starts_with("image/"));
        }
    }

    fn launcher_read_paste(
        &self,
        reader: std::os::unix::net::UnixStream,
        sender: smithay::reexports::calloop::channel::Sender<(Paste, Clip)>,
        target: Paste,
        picture: bool,
    ) {
        use std::io::Read;
        std::thread::spawn(move || {
            if picture {
                let mut png = Vec::new();
                if reader.take(MAX_IMAGE + 1).read_to_end(&mut png).is_ok() && png.len() as u64 <= MAX_IMAGE {
                    let _ = sender.send((target, Clip::Png(png.into())));
                }
                return;
            }
            let mut text = String::new();
            if reader.take(4097).read_to_string(&mut text).is_ok() && text.len() <= 4096 {
                let _ = sender.send((target, Clip::Text(text)));
            }
        });
    }

    fn launcher_insert_paste(&mut self, text: &str) {
        let text: String = text.chars().map(|character| if character.is_control() { ' ' } else { character }).collect();
        self.launcher.input.insert(&text);
        self.launcher.edited = Instant::now();
        self.refresh_launcher();
    }

    pub fn launcher_step(&mut self, delta: isize) {
        let len = self.launcher.entries.len();
        if len > 0 {
            self.select_launcher((self.launcher.selected as isize + delta).rem_euclid(len as isize) as usize);
        }
    }

    fn select_launcher(&mut self, index: usize) {
        self.launcher.first = self.launcher_layout().first;
        self.launcher.selected = index;
        self.launcher.first = self.launcher_layout().first;
    }

    pub fn launcher_scroll(&mut self, delta: isize) {
        if let Some(lines) =
            self.launcher.shown_run().and_then(|run| run.output.as_ref()).map(|output| output.text.lines().count())
        {
            let most = lines.saturating_sub(self.launcher_output_lines());
            self.launcher.output_scroll = self.launcher.output_scroll.saturating_add_signed(-delta).min(most);
        } else {
            self.launcher_step(delta);
        }
    }

    fn launcher_output_lines(&self) -> usize {
        self.launcher_layout()
            .output
            .map_or(1, |area| ((area.h - 2.0 * OUTPUT_PAD) / OUTPUT_LINE_H).floor().max(2.0) as usize - 1)
    }

    fn home_columns(&self) -> usize {
        let width = (self.output_rect().w - 32.0).clamp(1.0, PANEL_W);
        (((width - 2.0 * PAD) / 140.0).floor() as usize).clamp(1, HOME_COLUMNS)
    }

    /// Up or down a row of the welcome screen's grid, staying on the grid.
    fn home_row_step(&mut self, rows: isize) {
        let len = self.launcher.entries.len() as isize;
        let next = self.launcher.selected as isize + rows * self.home_columns() as isize;
        if (0..len).contains(&next) {
            self.select_launcher(next as usize);
        }
    }

    /// Enter on the chosen row. Ctrl+Enter runs a `>` command in a terminal.
    fn launch_selected(&mut self, mods: crate::config::Mods) {
        let Some(entry) = self.launcher.entries.get(self.launcher.selected).cloned() else { return };
        match entry {
            Entry::App(app_id) => {
                let Some(command) = self.launcher.index.get(&app_id).map(|app| {
                    let extra = self.config.launch_args(&app.id);
                    tileception_apps::command_line_with(app, &format!("{} -e", self.terminal), &extra)
                }) else {
                    return;
                };
                self.launch_command(command, &app_id);
            }
            Entry::Action { app, action } => {
                let Some(command) = self.launcher.index.get(&app).and_then(|entry| {
                    let action = entry.actions.get(action)?;
                    Some(tileception_apps::action_command_line(entry, action, &format!("{} -e", self.terminal)))
                }) else {
                    return;
                };
                self.launch_command(command, &app);
            }
            Entry::Window(window_id) => {
                self.close_launcher();
                self.fly_to_window(window_id);
            }
            Entry::Command(index) => {
                self.close_launcher();
                self.run_builtin(COMMANDS[index].does);
            }
            Entry::Answer(answer) => {
                self.copy_text(answer.copy.clone());
                self.launcher.copied = Some((answer.copy, Instant::now()));
            }
            Entry::Place(place) => {
                self.close_launcher();
                self.spawn("xdg-open", &[place.target]);
            }
            Entry::Run(line) if mods.ctrl => {
                self.close_launcher();
                // The terminal stays open on a shell afterwards, to read the output.
                let script = format!("{line}; exec \"${{SHELL:-sh}}\"");
                let command = format!("{} -e sh -c {}", self.terminal, shell_quote(&script));
                self.spawn("sh", &["-c".into(), command]);
            }
            Entry::Run(line) => self.start_run(line),
            Entry::Ask(question) => {
                self.close_launcher();
                self.ask_doctor(&question);
            }
        }
    }

    /// Starts an app's command line and counts it towards its usage.
    fn launch_command(&mut self, command: String, app_id: &str) {
        if self.launcher.home {
            self.launcher.quiet_until = Some(Instant::now() + HOME_QUIET);
        }
        self.close_launcher();
        self.spawn("sh", &["-c".into(), command]);
        let entry = self.launcher.usage.entry(app_id.to_string()).or_insert(Usage { count: 0, last_used: UNIX_EPOCH });
        entry.count = entry.count.saturating_add(1);
        entry.last_used = SystemTime::now();
        let stored: HashMap<_, _> = self
            .launcher
            .usage
            .iter()
            .map(|(app_id, usage)| {
                (
                    app_id.clone(),
                    StoredUsage {
                        count: usage.count,
                        last_used: usage.last_used.duration_since(UNIX_EPOCH).unwrap_or_default().as_secs(),
                    },
                )
            })
            .collect();
        if let Ok(data) = toml::to_string(&stored) {
            let path = usage_path();
            if let Some(parent) = path.parent()
                && let Err(error) = std::fs::create_dir_all(parent).and_then(|_| std::fs::write(&path, data))
            {
                tracing::warn!("could not save launcher usage: {error}");
            }
        }
    }

    /// Brings a window into view: restored when minimized, else focused,
    /// with the camera moving to it when it is outside the framed space.
    fn fly_to_window(&mut self, window_id: WindowId) {
        if let Some(output) = self.output_for_window(window_id) {
            self.activate_output(&output);
        }
        let now = self.now();
        if let Some(full) = self.fullscreen
            && full != window_id
        {
            self.set_fullscreen(full, false);
        }
        if self.world.in_overview() {
            self.world.exit_overview(now);
        }
        if self.world.is_minimized(window_id) {
            self.world.restore(window_id, now);
        } else {
            self.world.focus_window(window_id, now);
        }
        self.apply_layout();
        self.persist_layout();
    }

    fn run_builtin(&mut self, does: Does) {
        match does {
            Does::Line(line) => match crate::commands::parse(line) {
                Ok(command) => {
                    if let Err(error) = self.execute(command) {
                        tracing::warn!("launcher command {line:?}: {error}");
                    }
                }
                Err(error) => tracing::warn!("launcher command {line:?}: {error}"),
            },
            Does::Suspend => self.run_power_action(PowerAction::Suspend),
            Does::Power(PowerAction::Uefi) => self.confirm_uefi_restart(),
            Does::Power(action) => {
                self.shell_power_menu();
                if let Some(menu) = self.power_menu.as_mut() {
                    menu.selected = PowerAction::ALL.iter().position(|candidate| *candidate == action).unwrap_or(0);
                }
            }
        }
    }

    /// The shortcut bound to a command, such as "Super+O".
    fn shortcut(&self, does: Does) -> Option<String> {
        let line = match does {
            Does::Line(line) => line,
            Does::Power(_) => "power-menu",
            Does::Suspend => return None,
        };
        let command = crate::commands::parse(line).ok()?;
        let bind = self.config.binds.iter().find(|candidate| candidate.commands.first() == Some(&command))?;
        Some(combo_label(bind.mods, bind.key))
    }

    /// Runs a `>` command in the background; its output comes back to the
    /// launcher if it is still open for the same command.
    fn start_run(&mut self, line: String) {
        let Some(sender) = self.launcher.finished.clone() else { return };
        let mut command = self.command("sh", &["-c".into(), line.clone()]);
        if let Some(home) = std::env::var_os("HOME") {
            command.current_dir(home);
        }
        let epoch = self.launcher.epoch;
        self.launcher.run = Some(Run { line: line.clone(), started: Instant::now(), output: None });
        self.launcher.output_scroll = 0;
        std::thread::spawn(move || {
            let output = capture(command, RUN_LIMIT, RUN_TIMEOUT);
            let _ = sender.send((epoch, line, output));
        });
    }

    fn run_finished(&mut self, epoch: u64, line: String, output: RunOutput) {
        if !self.launcher.open || self.launcher.epoch != epoch {
            return;
        }
        if let Some(run) = self.launcher.run.as_mut()
            && run.line == line
            && run.output.is_none()
        {
            run.output = Some(output);
            self.schedule_render();
        }
    }

    /// Where everything in the launcher goes, for drawing and for the pointer.
    fn launcher_layout(&self) -> LauncherLayout {
        let results = self.launcher.entries.len();
        let screen = self.output_rect();
        let width = (screen.w - 32.0).clamp(1.0, PANEL_W);
        let grid = self.home_grid();
        let columns = if grid { self.home_columns() } else { 1 };
        let row_h = if grid { TILE_H } else { ROW_H };
        let available = (screen.h - 32.0 - PAD * 2.0 - FIELD_H - CAPTION_H - FOOTER_H).max(row_h);
        let output_h = self
            .launcher
            .shown_run()
            .map(|_| (OUTPUT_LINES as f64 * OUTPUT_LINE_H + 2.0 * OUTPUT_PAD).min((available - row_h - 6.0).max(0.0)))
            .filter(|output_height| *output_height >= 2.0 * OUTPUT_LINE_H + 2.0 * OUTPUT_PAD);
        let reserved = output_h.map_or(0.0, |output_height| output_height + 6.0);
        let visible_rows = (((available - reserved) / row_h).floor() as usize).max(1);
        let row_count = results.div_ceil(columns).max(1);
        let selected_row = self.launcher.selected / columns;
        let first_row = (self.launcher.first / columns)
            .min(row_count.saturating_sub(visible_rows))
            .min(selected_row)
            .max(selected_row.saturating_sub(visible_rows - 1));
        let first = first_row * columns;
        let visible = results.saturating_sub(first).min(visible_rows * columns);
        let list_h = row_count.min(visible_rows) as f64 * row_h + reserved;
        let height = PAD + FIELD_H + CAPTION_H + list_h + PAD + FOOTER_H;
        // A fixed top edge, so the field stays put while results come and go.
        // The welcome screen sits lower, under its clock.
        let top = if self.launcher.home {
            (screen.y + screen.h * 0.30).max(screen.y + 180.0).min(screen.bottom() - 16.0 - height).max(screen.y + 16.0)
        } else {
            (screen.y + screen.h * 0.18).min(screen.bottom() - 16.0 - height).max(screen.y + 16.0)
        };
        let panel = Rect::new(screen.x + (screen.w - width) / 2.0, top, width, height);
        let field = Rect::new(panel.x + PAD, panel.y + PAD, panel.w - 2.0 * PAD, FIELD_H);
        let caption = Rect::new(panel.x + PAD + 6.0, field.bottom(), panel.w - 2.0 * PAD - 12.0, CAPTION_H);
        let rows: Vec<Rect> = if grid {
            let tile_w = (panel.w - 2.0 * PAD) / columns as f64;
            (0..visible)
                .map(|index| {
                    let (column, row) = ((index % columns) as f64, (index / columns) as f64);
                    let left = panel.x + PAD + column * tile_w;
                    Rect::new(left + 2.0, caption.bottom() + row * TILE_H, tile_w - 4.0, TILE_H - 4.0)
                })
                .collect()
        } else {
            (0..visible)
                .map(|index| {
                    Rect::new(panel.x + PAD, caption.bottom() + index as f64 * ROW_H, panel.w - 2.0 * PAD, ROW_H - 2.0)
                })
                .collect()
        };
        let output = output_h.map(|output_height| {
            let output_top = caption.bottom() + row_count.min(visible_rows) as f64 * row_h + 6.0;
            Rect::new(panel.x + PAD, output_top, panel.w - 2.0 * PAD, output_height)
        });
        let footer = Rect::new(panel.x, panel.bottom() - FOOTER_H, panel.w, FOOTER_H);
        let text = Rect::new(field.x + 44.0, field.y, field.w - 58.0, field.h);
        let media = self
            .media
            .snapshot
            .media
            .active()
            .is_some()
            .then(|| crate::controls::media_card_layout(panel.x, panel.bottom() + 12.0, panel.w))
            .filter(|card| card.rect.bottom() <= screen.bottom() - 8.0);
        LauncherLayout { panel, field, text, caption, rows, first, output, footer, media }
    }

    /// The welcome screen with nothing typed: apps as a grid of tiles.
    fn home_grid(&self) -> bool {
        self.launcher.home && self.launcher.input.text.is_empty()
    }

    /// Whether the welcome screen takes the pointer here. Elsewhere the
    /// desktop (bars, the notification panel) gets it as usual.
    pub fn home_contains(&self, point: Point) -> bool {
        let layout = self.launcher_layout();
        layout.panel.contains(point) || layout.media.is_some_and(|card| card.rect.contains(point))
    }

    pub fn launcher_at(&self, point: Point) -> Option<usize> {
        let layout = self.launcher_layout();
        layout.rows.iter().position(|row| row.contains(point)).map(|offset| layout.first + offset)
    }

    pub fn launcher_motion(&mut self, point: Point) {
        if self.launcher.selecting {
            self.launcher_cursor_at(point, true);
            return;
        }
        if let Some(index) = self.launcher_at(point) {
            self.select_launcher(index);
        }
    }

    pub fn launcher_click(&mut self, point: Point) {
        let layout = self.launcher_layout();
        if layout.field.contains(point) {
            let extend = self.seat.get_keyboard().expect("seat has a keyboard").modifier_state().shift;
            self.launcher_cursor_at(point, extend);
            self.launcher.selecting = true;
            self.launcher.input.anchor.get_or_insert(self.launcher.input.cursor);
            return;
        }
        if let Some(card) = &layout.media
            && card.rect.contains(point)
        {
            self.media_card_press(card, point);
            return;
        }
        if let Some(index) = self.launcher_at(point) {
            self.select_launcher(index);
            self.launch_selected(crate::config::Mods::default());
        } else if !layout.panel.contains(point) && !self.launcher.home {
            self.close_launcher();
        }
    }

    pub fn launcher_release(&mut self) {
        self.launcher.selecting = false;
    }

    fn launcher_cursor_at(&mut self, point: Point, extend: bool) {
        let text = self.launcher_layout().text;
        let input = &self.launcher.input;
        let position = self.text.borrow_mut().field_hit(
            &input.text,
            &self.theme.font,
            self.theme.font_size * SEARCH_SCALE,
            text.w as f32,
            input.cursor,
            (point.x - text.x) as f32,
        );
        self.launcher.input.move_to(position, extend);
        self.launcher.edited = Instant::now();
    }

    pub fn launcher_elements(&self, renderer: &mut GlesRenderer) -> Vec<OutputElement> {
        if !self.launcher.open {
            return Vec::new();
        }
        let theme = &self.theme;
        let entries = &self.launcher.entries;
        let layout = self.launcher_layout();
        let input = &self.launcher.input;
        let mut painter = Painter::new(self, renderer);

        // Fades in while dropping into place.
        let shown = ease_out(self.launcher.opened.elapsed().as_secs_f64() / 0.16);
        painter.alpha = shown as f32;
        let clock_space = layout.panel.y - self.output_rect().y - 48.0;
        if self.launcher.home && clock_space >= 100.0 {
            // The welcome screen leaves the wallpaper as it is, under a clock.
            let now = LocalTime::now();
            let clock = now.as_ref().map(LocalTime::clock).unwrap_or_default();
            let date = now.as_ref().map(LocalTime::date).unwrap_or_default();
            let scale = (clock_space / 156.0).min(1.0);
            let top = layout.panel.y - 156.0 * scale;
            let shadow = Colour([0.0, 0.0, 0.0, 0.45]);
            let clock_style =
                Style::new(theme.font_size * 5.4 * scale as f32, theme.text).weight(300).align(Align::Centre);
            let date_style = Style::new(theme.font_size * 1.25, theme.text.alpha(0.85)).align(Align::Centre);
            let clock_rect = Rect::new(layout.panel.x, top, layout.panel.w, 96.0 * scale);
            let date_rect = Rect::new(layout.panel.x, top + 96.0 * scale, layout.panel.w, 30.0);
            // A soft shadow keeps the clock readable on any wallpaper.
            painter.text(
                &clock,
                Rect::new(clock_rect.x + 1.5, clock_rect.y + 2.0, clock_rect.w, clock_rect.h),
                Style { colour: shadow, ..clock_style },
            );
            painter.text(&clock, clock_rect, clock_style);
            painter.text(
                &date,
                Rect::new(date_rect.x + 1.0, date_rect.y + 1.5, date_rect.w, date_rect.h),
                Style { colour: shadow, ..date_style },
            );
            painter.text(&date, date_rect, date_style);
        } else if !self.launcher.home {
            painter.fill(self.output_rect(), Colour([0.0, 0.0, 0.0, 0.32]), 0.0);
        }
        painter.offset = (0.0, (shown - 1.0) * 12.0);
        painter.popup(layout.panel);

        // The search field, glowing softly as the place where typing goes.
        painter.shape(
            layout.field,
            Shape {
                border_width: 1.0,
                border: theme.accent.alpha(0.45),
                glow_radius: theme.glow * 0.6,
                glow: theme.accent.alpha(0.12),
                ..Shape::filled(theme.background, theme.radius)
            },
        );
        let typing = !input.text.is_empty();
        let running = answers::command(&input.text).is_some();
        painter.glyph(
            if running { glyph::TERMINAL } else { glyph::SEARCH },
            Rect::new(layout.field.x + 8.0, layout.field.y, 28.0, layout.field.h),
            18,
            if typing { theme.accent } else { theme.muted },
        );
        let size = theme.font_size * SEARCH_SCALE;
        if !typing {
            let hint = Rect::new(layout.text.x + 4.0, layout.text.y, layout.text.w - 4.0, layout.text.h);
            painter.text("Search, calculate, or > run a command", hint, Style::new(size, theme.muted));
        }
        let field = self.text.borrow_mut().field(
            &input.text,
            &theme.font,
            size,
            theme.text,
            layout.text.w as f32,
            crate::text::Field {
                cursor: input.cursor,
                anchor: input.anchor,
                visible: self.launcher.edited.elapsed().as_millis() % 1000 < 600,
                accent: theme.accent.straight_rgba8(),
            },
        );
        if let Some(image) = field {
            painter.image(&image.buffer, (layout.text.x, layout.text.y + (layout.text.h - image.height) / 2.0));
        }
        // The rest of the chosen name, greyed out after what was typed.
        let chosen = match entries.get(self.launcher.selected) {
            Some(Entry::App(app_id)) => self.launcher.index.get(app_id).map(|app| app.name.as_str()),
            _ => None,
        };
        if let Some(rest) = chosen.and_then(|name| completion(&input.text, name))
            && input.cursor == input.text.len()
            && input.anchor.is_none()
        {
            let typed = painter.measure(&input.text, 4096.0, Style::new(size, theme.text)).0;
            let room = layout.text.w - typed - 4.0;
            if room > 24.0 {
                let slot = Rect::new(layout.text.x + typed + 1.0, layout.text.y, room, layout.text.h);
                painter.text(rest, slot, Style::new(size, theme.muted.alpha(0.8)));
            }
        }

        let grid = self.home_grid();
        let trimmed = input.text.trim_start();
        let caption = if running {
            "COMMAND"
        } else if trimmed.starts_with(':') {
            "CHARACTERS"
        } else if typing {
            "RESULTS"
        } else if self.launcher.usage.is_empty() {
            "APPLICATIONS"
        } else {
            "FREQUENTLY USED"
        };
        let caption = if layout.rows.len() < entries.len() {
            format!(
                "{caption} · {} to {} of {} · Scroll for more",
                layout.first + 1,
                layout.first + layout.rows.len(),
                entries.len()
            )
        } else {
            caption.to_string()
        };
        painter.text(&caption, layout.caption, Style::new(theme.font_size * 0.85, theme.muted).bold());

        if entries.is_empty() {
            let area = Rect::new(
                layout.panel.x,
                layout.caption.bottom(),
                layout.panel.w,
                layout.footer.y - layout.caption.bottom(),
            );
            let (icon, message) = if running {
                (glyph::TERMINAL, "Type a command to run".to_string())
            } else if trimmed.starts_with(':') && trimmed.len() == 1 {
                (glyph::SEARCH, "Type a name, such as :thumbs or :arrow".to_string())
            } else {
                (glyph::SEARCH, format!("Nothing matches \u{201c}{}\u{201d}", input.text.trim()))
            };
            painter.glyph(icon, Rect::new(area.x + 12.0, area.y, 36.0, area.h), 24, theme.muted.alpha(0.7));
            painter.text(
                &message,
                Rect::new(area.x + 52.0, area.y, area.w - 68.0, area.h),
                Style::new(theme.font_size, theme.muted).align(Align::Centre),
            );
        }
        for (index, (entry, row)) in entries.iter().skip(layout.first).zip(&layout.rows).enumerate() {
            let selected = layout.first + index == self.launcher.selected;
            if grid {
                if let Entry::App(app_id) = entry
                    && let Some(app) = self.launcher.index.get(app_id)
                {
                    self.draw_tile(&mut painter, app, *row, selected);
                }
                continue;
            }
            self.draw_entry(&mut painter, entry, *row, selected);
        }
        if let (Some(area), Some(run)) = (layout.output, self.launcher.shown_run()) {
            self.draw_output(&mut painter, area, run);
        }

        // Keys along the bottom, under a hairline.
        let footer = layout.footer;
        painter.fill(Rect::new(footer.x + 1.0, footer.y, footer.w - 2.0, 1.0), theme.border, 0.0);
        let selected = entries.get(self.launcher.selected);
        let enter = match selected {
            Some(Entry::Answer(_)) => "Copy",
            Some(Entry::Run(_)) => "Run",
            Some(Entry::Place(_) | Entry::Window(_)) => "Open",
            Some(Entry::Command(_)) => "Do it",
            Some(Entry::Ask(_)) => "Ask",
            _ => "Launch",
        };
        let close = if self.launcher.home { ("Esc", "Clear") } else { ("Esc", "Close") };
        let hints: Vec<(&str, &str)> = if self.launcher.home && !typing {
            vec![("Type", "Search"), ("\u{2190}\u{2192}", "Choose"), ("Enter", "Launch")]
        } else if matches!(selected, Some(Entry::Run(_))) {
            vec![("Enter", "Run here"), ("Ctrl+Enter", "In a terminal"), close]
        } else if matches!(selected, Some(Entry::Place(place)) if place.completion.is_some()) {
            vec![("\u{2191}\u{2193}", "Choose"), ("Tab", "Complete"), ("Enter", "Open"), close]
        } else {
            vec![("\u{2191}\u{2193}", "Choose"), ("Enter", enter), close]
        };
        painter.key_hints(&hints, footer.x + 16.0, footer.y + footer.h / 2.0, false);

        if let (Some(card), Some(player)) = (&layout.media, self.media.snapshot.media.active()) {
            painter.popup(card.rect);
            let pointer = Point::new(self.pointer.x, self.pointer.y);
            self.draw_media_card(&mut painter, card, player, pointer);
        }
        painter.finish()
    }

    /// One row of results: an icon, a title over a detail line, and on the
    /// right the key that acts on it (Enter when chosen, else its shortcut).
    fn draw_entry(&self, painter: &mut Painter, entry: &Entry, row: Rect, selected: bool) {
        let theme = &self.theme;
        if selected {
            painter.shape(
                row,
                Shape {
                    radius: theme.radius,
                    fill: (theme.accent.alpha(0.20), theme.accent_end.alpha(0.06)),
                    ..Default::default()
                },
            );
            painter.shape(Rect::new(row.x, row.y + 12.0, 3.0, row.h - 24.0), Shape::filled(theme.accent, 1.5));
        }
        let icon = Rect::new(row.x + 14.0, row.y + (row.h - 32.0) / 2.0, 32.0, 32.0);
        let glyph_box = |painter: &mut Painter, glyph: &'static str, colour: Colour| {
            painter.fill(icon, theme.surface_raised, theme.radius);
            painter.glyph(glyph, icon, 18, colour);
        };
        let mut title_style =
            Style::new(theme.font_size * 1.05, if selected { theme.text } else { theme.text.alpha(0.88) });
        if selected {
            title_style = title_style.bold();
        }
        let mut right: Option<String> = None;
        let mut copied = false;
        let (title, detail): (String, Option<String>) = match entry {
            Entry::App(app_id) => {
                let Some(app) = self.launcher.index.get(app_id) else { return };
                self.draw_app_icon(painter, icon, &app.id, app.icon.as_deref(), &app.name);
                (app.name.clone(), subtitle(app).map(str::to_string))
            }
            Entry::Action { app, action } => {
                let Some(entry) = self.launcher.index.get(app) else { return };
                let Some(action) = entry.actions.get(*action) else { return };
                let icon_name = action.icon.as_deref().or(entry.icon.as_deref());
                self.draw_app_icon(painter, icon, &entry.id, icon_name, &entry.name);
                (action.name.clone(), Some(entry.name.clone()))
            }
            Entry::Window(window_id) => {
                let Some(window) = self.windows.get(window_id) else { return };
                let app = crate::state::app_id(window);
                self.draw_app_icon(painter, icon, &app, None, &app);
                let state = if self.world.is_minimized(*window_id) { "Minimized window" } else { "Open window" };
                let name = self
                    .launcher
                    .index
                    .get(&format!("{app}.desktop"))
                    .map_or(app.clone(), |desktop| desktop.name.clone());
                (crate::state::window_title(window), Some(format!("{name} \u{b7} {state}")))
            }
            Entry::Command(index) => {
                let command = &COMMANDS[*index];
                glyph_box(painter, command.glyph, theme.accent);
                right = self.shortcut(command.does);
                let detail = match command.does {
                    Does::Line("terminal") => format!("Opens {}", self.terminal),
                    Does::Line("do-not-disturb") => {
                        let enabled = self.do_not_disturb();
                        format!("Tileception \u{b7} now {}", if enabled { "on" } else { "off" })
                    }
                    Does::Power(_) => "Tileception \u{b7} opens the power menu to confirm".into(),
                    _ => "Tileception".into(),
                };
                (command.name.to_string(), Some(detail))
            }
            Entry::Answer(answer) => {
                match answer.look {
                    Look::Sum => glyph_box(painter, glyph::CALCULATOR, theme.accent),
                    Look::Clock => glyph_box(painter, glyph::CLOCK, theme.accent),
                    Look::Calendar => glyph_box(painter, glyph::CALENDAR, theme.accent),
                    Look::Colour([red, green, blue]) => {
                        let colour =
                            Colour([f32::from(red) / 255.0, f32::from(green) / 255.0, f32::from(blue) / 255.0, 1.0]);
                        painter.shape(
                            icon,
                            Shape { border_width: 1.0, border: theme.border, ..Shape::filled(colour, theme.radius) },
                        );
                    }
                    Look::Character => {
                        painter.fill(icon, theme.surface_raised, theme.radius);
                        painter.text(
                            &answer.title,
                            icon,
                            Style::new(theme.font_size * 1.3, theme.text).align(Align::Centre),
                        );
                    }
                }
                copied = self.launcher.just_copied(&answer.copy);
                title_style = Style::new(theme.font_size * 1.15, theme.text).bold();
                let title = if answer.look == Look::Character { answer.detail.clone() } else { answer.title.clone() };
                let detail = if answer.look == Look::Character { None } else { Some(answer.detail.clone()) };
                (title, detail)
            }
            Entry::Place(place) => {
                let glyph = if place.link {
                    glyph::GLOBE
                } else if place.folder {
                    glyph::FOLDER
                } else {
                    glyph::FILE
                };
                glyph_box(painter, glyph, theme.accent);
                (place.title.clone(), Some(place.detail.clone()))
            }
            Entry::Run(line) => {
                glyph_box(painter, glyph::TERMINAL, theme.accent);
                (format!("Run {line}"), Some("Shows the output here; Ctrl+Enter runs it in a terminal".into()))
            }
            Entry::Ask(question) => {
                glyph_box(painter, glyph::CHAT, theme.accent);
                title_style = Style::new(theme.font_size * 1.05, theme.text).bold();
                ("Ask Tileception".into(), Some(question.clone()))
            }
        };

        // What is on the right: "Copied", the Enter key, or a shortcut.
        let mut right_w = 0.0;
        if copied {
            let label_style = Style::new(theme.font_size * 0.9, theme.accent).bold();
            let width = painter.measure("Copied", 200.0, label_style).0;
            let left = row.right() - 12.0 - width;
            painter.text("Copied", Rect::new(left, row.y, width, row.h), label_style);
            painter.glyph(glyph::CHECK, Rect::new(left - 22.0, row.y, 18.0, row.h), 16, theme.accent);
            right_w = width + 28.0;
        } else if selected {
            let width = painter.keycap_width("Enter");
            painter.keycap("Enter", row.right() - 12.0 - width, row.y + row.h / 2.0);
            right_w = width + 4.0;
        } else if let Some(keys) = &right {
            let width = painter.keycap_width(keys);
            painter.keycap(keys, row.right() - 12.0 - width, row.y + row.h / 2.0);
            right_w = width + 4.0;
        }
        let text_x = icon.right() + 14.0;
        let text_w = row.right() - text_x - 12.0 - right_w - 8.0;
        match detail.filter(|text| !text.is_empty()) {
            Some(detail) => {
                painter.text(&title, Rect::new(text_x, row.y + 7.0, text_w, 22.0), title_style);
                painter.text(
                    &detail,
                    Rect::new(text_x, row.y + 27.0, text_w, 18.0),
                    Style::new(theme.font_size * 0.92, theme.muted),
                );
            }
            None => {
                painter.text(&title, Rect::new(text_x, row.y, text_w, row.h), title_style);
            }
        }
    }

    /// An app's icon, or its initial on the accent when it has none.
    fn draw_app_icon(&self, painter: &mut Painter, bounds: Rect, app_id: &str, icon: Option<&str>, name: &str) {
        let theme = &self.theme;
        let image = icon.and_then(|icon| self.launcher.icon(icon, 32)).or_else(|| self.launcher.app_icon(app_id, 32));
        if let Some(image) = image {
            painter.image(&image, (bounds.x, bounds.y));
        } else {
            painter.shape(
                bounds,
                Shape { radius: theme.radius, fill: (theme.accent, theme.accent_end), ..Default::default() },
            );
            let initial: String = name.chars().take(1).collect::<String>().to_uppercase();
            painter.text(
                &initial,
                bounds,
                Style::new(theme.font_size * 1.2, theme.on_accent).bold().align(Align::Centre),
            );
        }
    }

    /// What a `>` command printed: the last lines that fit, and how it ended.
    fn draw_output(&self, painter: &mut Painter, area: Rect, run: &Run) {
        let theme = &self.theme;
        painter.shape(
            area,
            Shape { border_width: 1.0, border: theme.border, ..Shape::filled(theme.background, theme.radius) },
        );
        let inner = Rect::new(area.x + 12.0, area.y + OUTPUT_PAD, area.w - 24.0, area.h - 2.0 * OUTPUT_PAD);
        let line_style = Style::new(theme.font_size * 0.88, theme.text.alpha(0.9));
        let status_style = Style::new(theme.font_size * 0.85, theme.muted);
        let Some(output) = &run.output else {
            let dots = ".".repeat(1 + (run.started.elapsed().as_millis() / 400 % 3) as usize);
            painter.text(&format!("Running{dots}"), Rect::new(inner.x, inner.y, inner.w, OUTPUT_LINE_H), status_style);
            return;
        };
        let status = match &output.status {
            RunStatus::Exited(0) => "Finished".to_string(),
            RunStatus::Exited(code) => format!("Exit status {code}"),
            RunStatus::Signal(signal) => format!("Stopped by signal {signal}"),
            RunStatus::TimedOut => format!("Stopped after {} s", RUN_TIMEOUT.as_secs()),
            RunStatus::Failed(error) => format!("Could not run: {error}"),
        };
        let status = if output.truncated { format!("{status} \u{b7} output cut at 64 KB") } else { status };
        let lines: Vec<&str> = output.text.lines().collect();
        let room = ((inner.h / OUTPUT_LINE_H).floor() as usize).saturating_sub(1).max(1);
        let end = lines.len().saturating_sub(self.launcher.output_scroll);
        let start = end.saturating_sub(room);
        let shown = &lines[start..end];
        if lines.is_empty() {
            painter.text("No output", Rect::new(inner.x, inner.y, inner.w, OUTPUT_LINE_H), status_style);
        }
        for (index, line) in shown.iter().enumerate() {
            let line_rect = Rect::new(inner.x, inner.y + index as f64 * OUTPUT_LINE_H, inner.w, OUTPUT_LINE_H);
            painter.text(line, line_rect, line_style);
        }
        let status = if lines.len() > room {
            format!(
                "{status} \u{b7} lines {}\u{2013}{end} of {} \u{b7} \u{2191}\u{2193} scroll",
                start + 1,
                lines.len()
            )
        } else {
            status
        };
        let succeeded = matches!(output.status, RunStatus::Exited(0));
        let colour = if succeeded { theme.muted } else { Colour([0.95, 0.55, 0.35, 1.0]) };
        let status_rect = Rect::new(inner.x, inner.bottom() - OUTPUT_LINE_H, inner.w, OUTPUT_LINE_H);
        painter.text(&status, status_rect, Style { colour, ..status_style });
    }

    /// One app on the welcome screen's grid: its icon over its name.
    fn draw_tile(&self, painter: &mut Painter, app: &tileception_apps::App, tile: Rect, selected: bool) {
        let theme = &self.theme;
        let hover = tile.contains(Point::new(self.pointer.x, self.pointer.y));
        if selected {
            painter.shape(
                tile,
                Shape {
                    radius: theme.radius,
                    fill: (theme.accent.alpha(0.20), theme.accent_end.alpha(0.08)),
                    border_width: 1.0,
                    border: theme.accent.alpha(0.35),
                    ..Default::default()
                },
            );
        } else if hover {
            painter.fill(tile, theme.surface_raised, theme.radius);
        }
        let icon = Rect::new(tile.x + (tile.w - 48.0) / 2.0, tile.y + 12.0, 48.0, 48.0);
        if let Some(image) = self.launcher.app_icon(&app.id, 48) {
            painter.image(&image, (icon.x, icon.y));
        } else {
            painter.shape(
                icon,
                Shape { radius: theme.radius * 1.5, fill: (theme.accent, theme.accent_end), ..Default::default() },
            );
            let initial: String = app.name.chars().take(1).collect::<String>().to_uppercase();
            painter.text(
                &initial,
                icon,
                Style::new(theme.font_size * 1.6, theme.on_accent).bold().align(Align::Centre),
            );
        }
        let name = Rect::new(tile.x + 6.0, icon.bottom() + 6.0, tile.w - 12.0, 40.0);
        let style = Style::new(theme.font_size * 0.92, if selected { theme.text } else { theme.text.alpha(0.88) })
            .align(Align::Centre)
            .lines(2);
        painter.text(&app.name, name, if selected { style.bold() } else { style });
    }
}

/// How well `query` matches `text` (both lower case): 0 exactly, 1 at the
/// start, 2 at the start of a word, 3 anywhere.
fn quality(query: &str, text: &str) -> Option<u8> {
    if text == query {
        Some(0)
    } else if text.starts_with(query) {
        Some(1)
    } else if text.split(|character: char| !character.is_alphanumeric()).any(|word| word.starts_with(query)) {
        Some(2)
    } else if text.contains(query) {
        Some(3)
    } else {
        None
    }
}

/// "Super+O", "Ctrl+Alt+Delete", "Print".
fn combo_label(mods: crate::config::Mods, key: u32) -> String {
    let mut parts = Vec::new();
    for (held, name) in [(mods.logo, "Super"), (mods.ctrl, "Ctrl"), (mods.alt, "Alt"), (mods.shift, "Shift")] {
        if held {
            parts.push(name.to_string());
        }
    }
    if crate::config::modifier_of(key).is_none() && key != crate::config::TAP {
        parts.push(key_label(key));
    }
    parts.join("+")
}

fn key_label(key: u32) -> String {
    use crate::config::{WHEEL_DOWN, WHEEL_LEFT, WHEEL_RIGHT, WHEEL_UP};
    match key {
        WHEEL_UP => return "Wheel up".into(),
        WHEEL_DOWN => return "Wheel down".into(),
        WHEEL_LEFT => return "Wheel left".into(),
        WHEEL_RIGHT => return "Wheel right".into(),
        _ => {}
    }
    let name = xkb::keysym_get_name(xkb::Keysym::from(key));
    match name.as_str() {
        "equal" => "=".into(),
        "minus" => "-".into(),
        "plus" => "+".into(),
        "slash" => "/".into(),
        "backslash" => "\\".into(),
        "bracketleft" => "[".into(),
        "bracketright" => "]".into(),
        "comma" => ",".into(),
        "period" => ".".into(),
        "space" => "Space".into(),
        "Return" => "Enter".into(),
        "Escape" => "Esc".into(),
        "Prior" => "Page Up".into(),
        "Next" => "Page Down".into(),
        "KP_Add" => "Keypad +".into(),
        "KP_Subtract" => "Keypad -".into(),
        single if single.chars().count() == 1 => single.to_uppercase(),
        other => other.to_string(),
    }
}

/// Runs a command with its output piped back, at most `limit` bytes of it,
/// and stops it (with everything it started) after `timeout`.
fn capture(mut command: std::process::Command, limit: usize, timeout: Duration) -> RunOutput {
    use std::{
        io::Read,
        os::unix::process::{CommandExt, ExitStatusExt},
        process::Stdio,
        sync::{Arc, Mutex},
    };
    // A process group of its own, so a timeout can stop what it started too.
    command.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).process_group(0);
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            return RunOutput { text: String::new(), status: RunStatus::Failed(error.to_string()), truncated: false };
        }
    };
    let buffer = Arc::new(Mutex::new((Vec::new(), false)));
    let pipes: Vec<Box<dyn Read + Send>> = [
        child.stdout.take().map(|stream| Box::new(stream) as Box<dyn Read + Send>),
        child.stderr.take().map(|stream| Box::new(stream) as Box<dyn Read + Send>),
    ]
    .into_iter()
    .flatten()
    .collect();
    let readers: Vec<_> = pipes
        .into_iter()
        .map(|mut pipe| {
            let buffer = buffer.clone();
            // Past the limit the rest is read and dropped, so the command never
            // blocks on a full pipe.
            std::thread::spawn(move || {
                let mut chunk = [0u8; 4096];
                while let Ok(count) = pipe.read(&mut chunk) {
                    if count == 0 {
                        break;
                    }
                    let Ok(mut buffer) = buffer.lock() else { break };
                    let room = limit.saturating_sub(buffer.0.len());
                    buffer.0.extend_from_slice(&chunk[..count.min(room)]);
                    buffer.1 |= count > room;
                }
            })
        })
        .collect();
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if Instant::now() >= deadline => {
                kill_group(child.id());
                let _ = child.wait();
                break None;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(_) => break None,
        }
    };
    // A moment for the last output; something left running in the background
    // may keep the pipes open, and is not waited for.
    let settle = Instant::now() + Duration::from_millis(200);
    for reader in &readers {
        while !reader.is_finished() && Instant::now() < settle {
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    let (bytes, truncated) = buffer.lock().map(|shared| (shared.0.clone(), shared.1)).unwrap_or_default();
    let status = match status {
        None => RunStatus::TimedOut,
        Some(status) => match (status.code(), status.signal()) {
            (Some(code), _) => RunStatus::Exited(code),
            (None, Some(signal)) => RunStatus::Signal(signal),
            (None, None) => RunStatus::Exited(-1),
        },
    };
    RunOutput { text: clean_output(&String::from_utf8_lossy(&bytes)), status, truncated }
}

#[allow(unsafe_code, reason = "stopping a process group needs libc's kill")]
fn kill_group(pid: u32) {
    let Ok(pid) = libc::pid_t::try_from(pid) else { return };
    // SAFETY: kill only sends a signal; a negative pid names the process group
    // the command was started in, which holds nothing but it and its children.
    unsafe {
        libc::kill(-pid, libc::SIGKILL);
    }
}

/// Output as plain lines: colour codes and carriage returns removed, tabs as spaces.
fn clean_output(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(character) = chars.next() {
        match character {
            '\u{1b}' => {
                // CSI sequences such as ESC [ 1 ; 31 m end in a letter.
                if chars.peek() == Some(&'[') {
                    chars.next();
                    for character in chars.by_ref() {
                        if character.is_ascii_alphabetic() {
                            break;
                        }
                    }
                }
            }
            '\r' => {}
            '\t' => out.push_str("    "),
            character if character.is_control() && character != '\n' => {}
            character => out.push(character),
        }
    }
    out
}

/// A string as one shell word.
fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

/// See [`Launcher::question`].
fn question_in(typed: &str) -> Option<String> {
    let typed = typed.trim();
    if let Some(rest) = typed.strip_prefix('?') {
        return Some(rest.trim().to_string()).filter(|question| !question.is_empty());
    }
    (typed.ends_with('?') || typed.split_whitespace().count() >= 3).then(|| typed.to_string())
}

/// The part of `name` still to come after `typed`, when `name` starts with it.
fn completion<'a>(typed: &str, name: &'a str) -> Option<&'a str> {
    if typed.is_empty() {
        return None;
    }
    let mut rest = name.char_indices();
    for typed in typed.chars() {
        let (_, character) = rest.next()?;
        if !character.to_lowercase().eq(typed.to_lowercase()) {
            return None;
        }
    }
    let offset = rest.next().map(|(index, _)| index)?;
    Some(&name[offset..])
}

/// A short description under the name: the generic name, or else the comment.
fn subtitle(app: &tileception_apps::App) -> Option<&str> {
    app.generic_name
        .as_deref()
        .filter(|generic| !generic.is_empty() && !generic.eq_ignore_ascii_case(&app.name))
        .or(app.comment.as_deref())
        .filter(|text| !text.is_empty())
}

/// The largest image `copy-image` takes.
const MAX_IMAGE: u64 = 64 * 1024 * 1024;
const PANEL_W: f64 = 620.0;
const PAD: f64 = 10.0;
const FIELD_H: f64 = 52.0;
const CAPTION_H: f64 = 32.0;
const ROW_H: f64 = 54.0;
const FOOTER_H: f64 = 44.0;
/// Rows of results at most, besides "Ask Tileception".
const MAX_ROWS: usize = 8;
/// Tiles per row on the welcome screen.
const HOME_COLUMNS: usize = 4;
const TILE_H: f64 = 114.0;
/// How long the welcome screen waits for a launched app's window.
const HOME_QUIET: Duration = Duration::from_secs(10);
/// The search text is a little larger than the rest of the interface.
const SEARCH_SCALE: f32 = 1.2;
/// How long a row says "Copied".
const COPIED_FOR: Duration = Duration::from_millis(1600);
/// A `>` command's output: how much is kept, and how long it may run.
const RUN_LIMIT: usize = 64 * 1024;
const RUN_TIMEOUT: Duration = Duration::from_secs(10);
/// Lines of output shown, the last one being the status.
const OUTPUT_LINES: usize = 12;
const OUTPUT_LINE_H: f64 = 19.0;
const OUTPUT_PAD: f64 = 10.0;

struct LauncherLayout {
    panel: Rect,
    /// What is playing, under the launcher.
    media: Option<crate::controls::MediaCard>,
    field: Rect,
    /// Where the typed text goes inside the field.
    text: Rect,
    caption: Rect,
    rows: Vec<Rect>,
    /// Index of the first visible result; keys and wheel keep selection in view.
    first: usize,
    /// A `>` command's output, under its row.
    output: Option<Rect>,
    footer: Rect,
}

fn usage_path() -> PathBuf {
    let root = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".local/state"));
    root.join("tileception/launcher-usage.toml")
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test setup and assertions should fail immediately on unexpected errors")]
mod tests {
    use super::{RunStatus, capture, clean_output, combo_label, completion, quality, question_in};
    use std::time::{Duration, Instant};

    #[test]
    fn questions_go_to_the_doctor() {
        assert_eq!(question_in("? make gaps smaller"), Some("make gaps smaller".into()));
        assert_eq!(question_in("?"), None);
        assert_eq!(question_in("why is it dark?"), Some("why is it dark?".into()));
        assert_eq!(question_in("use ghostty please"), Some("use ghostty please".into()));
        assert_eq!(question_in("viv"), None);
        assert_eq!(question_in("text editor"), None);
    }

    #[test]
    fn completes_names_that_start_with_the_query() {
        assert_eq!(completion("viv", "Vivaldi"), Some("aldi"));
        assert_eq!(completion("VIV", "Vivaldi"), Some("aldi"));
        assert_eq!(completion("ée", "Éether"), Some("ther"));
        assert_eq!(completion("ald", "Vivaldi"), None);
        assert_eq!(completion("vivaldi", "Vivaldi"), None);
        assert_eq!(completion("", "Vivaldi"), None);
    }

    #[test]
    fn match_quality() {
        assert_eq!(quality("lock", "lock screen"), Some(1));
        assert_eq!(quality("screen", "lock screen"), Some(2));
        assert_eq!(quality("cree", "lock screen"), Some(3));
        assert_eq!(quality("overview", "overview"), Some(0));
        assert_eq!(quality("xyz", "overview"), None);
    }

    #[test]
    fn shortcut_labels() {
        let logo = crate::config::Mods { logo: true, ..Default::default() };
        let shift = crate::config::Mods { logo: true, shift: true, ..Default::default() };
        assert_eq!(combo_label(logo, u32::from('o')), "Super+O");
        assert_eq!(combo_label(logo, 0x3d), "Super+=");
        assert_eq!(combo_label(shift, 0xff51), "Super+Shift+Left");
        assert_eq!(combo_label(Default::default(), 0xff61), "Print");
        assert_eq!(combo_label(logo, 0xffeb), "Super");
    }

    #[test]
    fn captured_commands() {
        let run = |line: &str, limit: usize, timeout: Duration| {
            let mut command = std::process::Command::new("sh");
            command.args(["-c", line]);
            capture(command, limit, timeout)
        };
        let out = run("echo out; echo err >&2; exit 3", 1024, Duration::from_secs(5));
        assert!(matches!(out.status, RunStatus::Exited(3)), "{:?}", out.status);
        assert!(out.text.contains("out\n") && out.text.contains("err\n"), "{:?}", out.text);
        let big = run("head -c 100000 /dev/zero | tr '\\0' x", 1000, Duration::from_secs(5));
        assert_eq!((big.text.len(), big.truncated), (1000, true));
        let started = Instant::now();
        let slow = run("sleep 30 & sleep 30", 1024, Duration::from_millis(300));
        assert!(matches!(slow.status, RunStatus::TimedOut));
        assert!(started.elapsed() < Duration::from_secs(5), "the whole group was stopped");
    }

    #[test]
    fn output_is_plain_text() {
        assert_eq!(clean_output("\u{1b}[1;31mred\u{1b}[0m\r\n\tx"), "red\n    x");
    }
}
