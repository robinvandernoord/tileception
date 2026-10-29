//! `tileception --greeter`: a greetd greeter drawn by the compositor itself,
//! laid out like a Tileception desktop. The login screen is a few tiles (login,
//! clock, session), the people are tabs on the login tile, actions live in a
//! launcher, and errors arrive as notifications. See `docs/design-greeter.html`.
//!
//! The greeter is a lock state of the compositor ([`LockState::Greeter`]), so
//! nothing of the desktop can be reached; logging in hands the seat back to
//! greetd, which starts the chosen session once this process ends.
//!
//! The same screen is the session's lock screen ([`Mode::Lock`]): the login
//! tile with only the signed-in person, the clock, and what happened while
//! they were away, checking the password with PAM instead of greetd.

mod accounts;
mod card;
mod draw;
mod greetd;
mod keymap;
mod layout;
mod sessions;
mod settings;

use std::{
    cell::{OnceCell, RefCell},
    collections::HashMap,
    path::PathBuf,
    sync::mpsc::TryRecvError,
    time::{Duration, Instant},
};

use smithay::{
    backend::renderer::element::memory::MemoryRenderBuffer, input::keyboard::keysyms,
    reexports::calloop::ping::make_ping,
};
use tileception_core::{Dir, Point, Rect};
use tileception_notify::Urgency;
use zeroize::Zeroizing;

pub use card::ShareConfig;
pub use settings::GreeterFile;

use self::{
    accounts::Account,
    greetd::{ErrorType, Greetd, Prompt, Request, Response, Responses},
    sessions::Session,
    settings::{Memory, Panel},
};
use crate::{Tileception, config::Mods, lock::LockState, password::SecretField, theme::Colour};

/// The password `--greeter` accepts when there is no greetd to ask.
const DEMO_PASSWORD: &str = "tide";
/// Someone's card as read: who (by index), the card, and their wallpaper
/// decoded for the screen (RGBA, width, height).
type FoundCard = (usize, card::Card, Option<(Vec<u8>, u32, u32)>);

/// The greeter's log of its last run, `greeter.log` beside its memory file
/// (normally /var/lib/tileception-greeter), started afresh each time. None
/// when that directory cannot be written, such as when testing nested.
pub fn log_file() -> Option<std::fs::File> {
    let dir = settings::GreeterFile::load().state_path().parent()?.to_path_buf();
    std::fs::File::create(dir.join("greeter.log")).ok()
}

/// How long the greeter takes to fade out once the session starts.
const FADE_OUT: Duration = Duration::from_millis(250);

/// Where a greetd conversation is.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Auth {
    /// No session: Enter starts one.
    Idle,
    /// `create_session` is on its way.
    Creating,
    /// PAM asked something beyond the password; the field takes the answer.
    Asking { prompt: Prompt, text: String },
    /// An answer is on its way.
    Answering,
    /// Logged in: `start_session` is on its way.
    Starting,
    /// `cancel_session` is on its way.
    Cancelling,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mode {
    /// Before anyone is logged in, talking to greetd.
    Login,
    /// The session is locked.
    Lock {
        /// "18:02", said on the login tile.
        locked_at: String,
        /// How many notifications there were when the screen locked, to count
        /// the ones that came in since.
        notifications: usize,
    },
}

/// One row of the launcher.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Play or pause the player that media keys control.
    Media,
    Suspend,
    Restart,
    PowerOff,
    Firmware,
    ConfirmFirmware,
    More,
    Back,
    Session(usize),
    Person(usize),
    /// One of the keymap's own layouts, by position.
    Layout(usize),
    /// Any layout xkb knows, by position in the catalogue.
    Keymap(usize),
}

#[derive(Debug, Default)]
pub struct Palette {
    query: String,
    selected: usize,
    opened: Option<Instant>,
    page: PalettePage,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum PalettePage {
    #[default]
    Main,
    More,
    ConfirmFirmware,
}

pub struct Greeter {
    mode: Mode,
    file: GreeterFile,
    accounts: Vec<Account>,
    sessions: Vec<Session>,
    memory: Memory,
    memory_path: PathBuf,
    /// The selected person: an index into `accounts`, or `accounts.len()` for
    /// someone who is not listed.
    user: usize,
    /// The typed username, for someone who is not listed.
    name: SecretField,
    /// Someone not listed has typed their name and now types the password.
    named: bool,
    field: SecretField,
    /// Sessions chosen this boot, by login.
    chosen: HashMap<String, usize>,
    focus: Panel,
    /// The highlighted row while the Session panel has focus.
    session_cursor: usize,
    palette: Option<Palette>,
    greetd: Option<Greetd>,
    responses: Option<Responses>,
    auth: Auth,
    /// The password, until greetd asks for it.
    answer: Option<Zeroizing<String>>,
    /// The person and session of the conversation.
    attempt: Option<(String, usize)>,
    attempts: u32,
    failed_at: Option<Instant>,
    typed_at: Instant,
    caps_lock: bool,
    /// When the selected person changed, who it was, and their accent, to crossfade.
    switched_at: Instant,
    previous_user: usize,
    previous_accent: (Colour, Colour),
    /// Shared wallpapers, by person, filling the screen.
    wallpapers: HashMap<usize, MemoryRenderBuffer>,
    /// Super went down with nothing else: letting go opens the launcher.
    super_tap: bool,
    /// Started without greetd: logging in only says what it would do.
    demo: bool,
    /// A lasting problem, said in place of the subtitle.
    pub notice: Option<String>,
    /// The session is starting: fading out, then the greeter ends.
    leaving: Option<Instant>,
    hostname: String,
    /// The configured keymap's layouts and variants, and which is active.
    pub layouts: Vec<(String, String)>,
    pub layout: usize,
    /// A layout picked here instead of the configured keymap.
    keymap: Option<(String, String)>,
    /// Every layout xkb knows, read when first asked for.
    catalogue: OnceCell<Vec<keymap::Known>>,
    /// The administrator's message, for the motd panel.
    motd: String,
    /// The running kernel's release, such as "6.17.0-5-generic".
    kernel: String,
    avatars: RefCell<HashMap<(String, u32), Option<MemoryRenderBuffer>>>,
}

impl Greeter {
    fn new(file: GreeterFile) -> Greeter {
        let accounts = accounts::list(&file.hidden_users);
        let mut sessions = sessions::list(&file.hidden_sessions, &file.xsession_wrapper);
        sessions::sort_default_first(&mut sessions, &file.default_session);
        let memory_path = file.state_path();
        let memory = Memory::load(&memory_path);
        let user = memory
            .last_user
            .as_deref()
            .and_then(|login| accounts.iter().position(|account| account.login == login))
            .unwrap_or(0);
        tracing::info!("greeter: {} people, {} sessions", accounts.len(), sessions.len());
        if file.users_as == settings::UsersAs::Tiles {
            tracing::info!("greeter: users_as = \"tiles\" is not drawn yet; showing tabs");
        }
        let mut greeter = Greeter::with(file, accounts, sessions, memory, memory_path, user);
        if greeter.file.panels.columns.iter().flatten().any(|panel| *panel == Panel::Motd) {
            greeter.motd = greeter.file.motd.message();
        }
        greeter
    }

    fn with(
        file: GreeterFile,
        accounts: Vec<Account>,
        sessions: Vec<Session>,
        memory: Memory,
        memory_path: PathBuf,
        user: usize,
    ) -> Greeter {
        let hostname = std::fs::read_to_string("/proc/sys/kernel/hostname")
            .or_else(|_| std::fs::read_to_string("/etc/hostname"))
            .map(|content| content.trim().to_string())
            .unwrap_or_default();
        let kernel = std::fs::read_to_string("/proc/sys/kernel/osrelease")
            .map(|content| content.trim().to_string())
            .unwrap_or_default();
        let mut greeter = Greeter {
            mode: Mode::Login,
            file,
            accounts,
            sessions,
            memory,
            memory_path,
            user,
            name: SecretField::default(),
            named: false,
            field: SecretField::default(),
            chosen: HashMap::new(),
            focus: Panel::Login,
            session_cursor: 0,
            palette: None,
            greetd: None,
            responses: None,
            auth: Auth::Idle,
            answer: None,
            attempt: None,
            attempts: 0,
            failed_at: None,
            typed_at: Instant::now(),
            caps_lock: false,
            switched_at: Instant::now() - Duration::from_secs(10),
            previous_user: user,
            previous_accent: (Colour::TRANSPARENT, Colour::TRANSPARENT),
            wallpapers: HashMap::new(),
            super_tap: false,
            demo: false,
            notice: None,
            leaving: None,
            hostname,
            layouts: Vec::new(),
            layout: 0,
            keymap: None,
            catalogue: OnceCell::new(),
            kernel,
            motd: String::new(),
            avatars: RefCell::new(HashMap::new()),
        };
        greeter.session_cursor = greeter.session_of_selected();
        greeter
    }

    /// The lock screen of a session: only `login` is shown, the theme stays
    /// the session's, and nothing is remembered.
    pub fn lock(login: &str, notifications: usize) -> Greeter {
        let account = accounts::list(&[]).into_iter().find(|listed| listed.login == login).unwrap_or_else(|| Account {
            login: login.to_string(),
            name: login.to_string(),
            uid: 0,
            icon: None,
            accent: None,
        });
        let file = GreeterFile {
            // The administrator decides what a locked screen may show.
            lock: GreeterFile::load().lock,
            per_user: settings::PerUser { accent: false, wallpaper: false, ..Default::default() },
            ..Default::default()
        };
        let mut greeter = Greeter::with(file, vec![account], Vec::new(), Memory::default(), PathBuf::new(), 0);
        greeter.mode = Mode::Lock { locked_at: draw::local_time("%H:%M"), notifications };
        greeter
    }

    fn locked(&self) -> bool {
        matches!(self.mode, Mode::Lock { .. })
    }

    fn other(&self) -> bool {
        self.user >= self.accounts.len()
    }

    /// How many tabs there are: everyone listed, and someone who is not;
    /// locked, only the signed-in person.
    fn people(&self) -> usize {
        if self.locked() { self.accounts.len() } else { self.accounts.len() + 1 }
    }

    fn selected(&self) -> Option<&Account> {
        self.accounts.get(self.user)
    }

    /// Who logs in: the selected person, or the typed name.
    fn login(&self) -> Option<String> {
        match self.selected() {
            Some(account) => Some(account.login.clone()),
            None => Some(self.name.as_str().trim().to_string()).filter(|typed| !typed.is_empty()),
        }
    }

    fn default_session(&self) -> usize {
        self.sessions.iter().position(|session| session.session_id == self.file.default_session).unwrap_or(0)
    }

    /// The session someone starts: chosen this boot, else their last one,
    /// else the default.
    fn session_for(&self, login: &str) -> usize {
        self.chosen
            .get(login)
            .copied()
            .or_else(|| {
                let session_id = self.memory.sessions.get(login)?;
                self.sessions.iter().position(|session| &session.session_id == session_id)
            })
            .unwrap_or_else(|| self.default_session())
    }

    /// Choices for someone not listed are kept under the empty login.
    fn session_of_selected(&self) -> usize {
        match self.selected() {
            Some(account) => self.session_for(&account.login),
            None => self.chosen.get("").copied().unwrap_or_else(|| self.default_session()),
        }
    }

    /// The session remembered as someone's last, to mark it.
    fn last_session(&self, login: &str) -> Option<usize> {
        let session_id = self.memory.sessions.get(login)?;
        self.sessions.iter().position(|session| &session.session_id == session_id)
    }

    /// When `login` last logged in here, in seconds since 1970.
    fn last_login(&self, login: &str) -> Option<i64> {
        self.memory.logins.get(login).copied()
    }

    fn choose_session(&mut self, index: usize) {
        if index >= self.sessions.len() {
            return;
        }
        let login = self.selected().map(|account| account.login.clone()).unwrap_or_default();
        self.chosen.insert(login, index);
        self.session_cursor = index;
    }

    /// The accent pair for a tab: the theme's when per-user accents are off
    /// or for someone not listed, else one derived from the login.
    fn accent_of(&self, theme: &crate::theme::Theme, index: usize) -> (Colour, Colour) {
        match self.accounts.get(index) {
            Some(account) if self.file.per_user.accent => {
                account.accent.unwrap_or_else(|| accounts::accent(&account.login))
            }
            _ => (theme.accent, theme.accent_end),
        }
    }

    /// Whether a conversation with greetd is under way, so typing waits.
    fn busy(&self) -> bool {
        !matches!(self.auth, Auth::Idle | Auth::Asking { .. })
    }

    /// The launcher's rows, filtered by what is typed. `media` is the state of
    /// the player media keys control, if any.
    fn actions(&self, media: Option<tileception_media::Status>) -> Vec<(Action, String)> {
        let query = self.palette.as_ref().map(|palette| palette.query.to_lowercase()).unwrap_or_default();
        match self.palette.as_ref().map(|palette| palette.page).unwrap_or_default() {
            PalettePage::More => return vec![(Action::Firmware, "UEFI".into()), (Action::Back, "Back".into())],
            PalettePage::ConfirmFirmware => {
                return vec![(Action::Back, "Cancel".into()), (Action::ConfirmFirmware, "Restart into UEFI".into())];
            }
            PalettePage::Main => {}
        }
        let mut list = Vec::new();
        if let Some(status) = media {
            let label = if status == tileception_media::Status::Playing { "Pause media" } else { "Play media" };
            list.push((Action::Media, label.to_string()));
        }
        list.extend([
            (Action::Suspend, "Suspend".to_string()),
            (Action::Restart, "Restart".to_string()),
            (Action::PowerOff, "Shut down".to_string()),
        ]);
        if !self.locked() && std::path::Path::new("/sys/firmware/efi").exists() {
            list.push((Action::More, "More…".to_string()));
            if !query.trim().is_empty() {
                list.push((Action::Firmware, "Restart into UEFI".to_string()));
            }
        }
        let current = self.session_of_selected();
        for (index, session) in self.sessions.iter().enumerate() {
            if index != current {
                list.push((Action::Session(index), format!("Session: {}", session.name)));
            }
        }
        for (index, account) in self.accounts.iter().enumerate() {
            if index != self.user {
                list.push((Action::Person(index), format!("Log in as {}", account.name)));
            }
        }
        if !self.other() && !self.locked() {
            list.push((Action::Person(self.accounts.len()), "Log in as someone not listed".to_string()));
        }
        let active = self.active_layout();
        for (index, (layout, variant)) in self.layouts.iter().enumerate() {
            if self.keymap.is_some() || index != self.layout {
                list.push((Action::Layout(index), format!("Keyboard layout: {}", self.describe(layout, variant))));
            }
        }
        // All of xkb's layouts only once there is something to search for.
        if !query.trim().is_empty() {
            for (index, known) in self.known().iter().enumerate() {
                let pair = (&known.layout, &known.variant);
                if active != Some(pair) && !self.layouts.iter().any(|(layout, variant)| (layout, variant) == pair) {
                    let short = keymap::short(&known.layout, &known.variant);
                    list.push((Action::Keymap(index), format!("Keyboard layout: {} ({short})", known.description)));
                }
            }
        }
        // Every word typed, in any order.
        list.retain(|(action, label)| {
            let aliases = match action {
                Action::Firmware => "bios firmware setup reboot",
                Action::PowerOff => "power off shutdown",
                _ => "",
            };
            let label = format!("{} {aliases}", label.to_lowercase());
            query.split_whitespace().all(|word| label.contains(word))
        });
        list
    }

    fn known(&self) -> &[keymap::Known] {
        self.catalogue.get_or_init(keymap::catalogue)
    }

    /// The layout and variant typed with now.
    pub fn active_layout(&self) -> Option<(&String, &String)> {
        match &self.keymap {
            Some((layout, variant)) => Some((layout, variant)),
            None => self.layouts.get(self.layout).map(|(layout, variant)| (layout, variant)),
        }
    }

    /// "Dutch (nl)", or just "nl" for a layout xkb does not describe.
    fn describe(&self, layout: &str, variant: &str) -> String {
        let short = keymap::short(layout, variant);
        match self.known().iter().find(|candidate| candidate.layout == layout && candidate.variant == variant) {
            Some(known) => format!("{} ({short})", known.description),
            None => short,
        }
    }

    /// The layout `login` picked here last time, if not the configured one.
    fn remembered_keymap(&self, login: &str) -> Option<(String, String)> {
        self.memory.layouts.get(login).map(|text| keymap::recall(text))
    }
}

/// "1st", "2nd", "3rd", "4th", "11th"...
fn ordinal(number: u32) -> String {
    let suffix = match (number % 10, number % 100) {
        (_, 11..=13) => "th",
        (1, _) => "st",
        (2, _) => "nd",
        (3, _) => "rd",
        _ => "th",
    };
    format!("{number}{suffix}")
}

impl Tileception {
    /// The launcher's rows for what is on screen now.
    fn greeter_actions(&self) -> Vec<(Action, String)> {
        let Some(greeter) = self.greeter.as_deref() else { return Vec::new() };
        // Media keys control a player only in a session.
        let media = self.media.snapshot.media.active().map(|player| player.status).filter(|_| greeter.locked());
        greeter.actions(media)
    }

    /// Turns this compositor into the greeter. Without `GREETD_SOCK` (nested or
    /// headless only), a stand-in accepts the password `tide` for everyone.
    pub fn start_greeter(&mut self, file: GreeterFile) -> Result<(), String> {
        let mut greeter = Greeter::new(file);
        let (ping, source) = make_ping().map_err(|err| err.to_string())?;
        let wake = move || ping.ping();
        let connected = match std::env::var_os("GREETD_SOCK") {
            Some(path) => greetd::Greetd::connect(std::path::Path::new(&path), wake)
                .map_err(|err| format!("could not reach greetd at {}: {err}", PathBuf::from(&path).display()))?,
            None if self.nested || self.udev.is_none() => {
                tracing::warn!("greeter: GREETD_SOCK is not set; every password is {DEMO_PASSWORD:?}");
                greeter.demo = true;
                greetd::demo(DEMO_PASSWORD.into(), wake).map_err(|err| err.to_string())?
            }
            None => return Err("GREETD_SOCK is not set: the greeter must be started by greetd".into()),
        };
        self.loop_handle.insert_source(source, |_, _, state| state.greetd_ready()).map_err(|err| err.to_string())?;
        (greeter.greetd, greeter.responses) = (Some(connected.0), Some(connected.1));
        let theme = self.theme.clone();
        greeter.previous_accent = greeter.accent_of(&theme, greeter.user);
        let people: Vec<(usize, String, u32)> = greeter
            .accounts
            .iter()
            .enumerate()
            .map(|(index, account)| (index, account.login.clone(), account.uid))
            .collect();
        let wallpapers = greeter.file.per_user.wallpaper;
        self.greeter = Some(Box::new(greeter));
        self.lock = LockState::Greeter;
        self.apply_greeter_accent();
        self.read_greeter_cards(people, wallpapers);
        self.greeter_keyboard_layouts();
        self.greeter_person_keymap();
        self.force_keyboard_focus_sync();
        self.schedule_render();
        Ok(())
    }

    /// Reads everyone's card off the event loop: AccountsService may be slow
    /// to answer, and the login screen must not wait for it.
    fn read_greeter_cards(&mut self, people: Vec<(usize, String, u32)>, wallpapers: bool) {
        use smithay::reexports::calloop::channel;
        let (sender, receiver) = channel::channel::<FoundCard>();
        let inserted = self.loop_handle.insert_source(receiver, |event, _, state| {
            if let channel::Event::Msg(found) = event {
                state.greeter_card_read(found);
            }
        });
        if let Err(error) = inserted {
            tracing::warn!("greeter: cards not read: {error}");
            return;
        }
        let screen = self.output_rect();
        let size = ((screen.w * self.scale).round() as u32, (screen.h * self.scale).round() as u32);
        let spawned = std::thread::Builder::new().name("tileception-greeter-cards".into()).spawn(move || {
            let folder = card::folder();
            for (index, login, uid) in people {
                let mut found = card::read(&login, uid, &folder);
                // A wallpaper but no colours: take them from the picture.
                if found.accent.is_none() {
                    let small = found.wallpaper.as_deref().and_then(|file| card::decode(file, 96, 54));
                    found.accent = small.and_then(|rgba| card::accent_from(&rgba));
                }
                let pixels = found
                    .wallpaper
                    .as_deref()
                    .filter(|_| wallpapers)
                    .and_then(|file| card::decode(file, size.0, size.1))
                    .map(|rgba| (rgba, size.0, size.1));
                if sender.send((index, found, pixels)).is_err() {
                    break;
                }
            }
        });
        if let Err(error) = spawned {
            tracing::warn!("greeter: cards not read: {error}");
        }
    }

    fn greeter_card_read(&mut self, (index, found, pixels): FoundCard) {
        let Some(greeter) = self.greeter.as_deref_mut() else { return };
        let Some(account) = greeter.accounts.get_mut(index) else { return };
        account.accent = found.accent;
        if let Some((rgba, width, height)) = pixels {
            let buffer = MemoryRenderBuffer::from_slice(
                &rgba,
                smithay::backend::allocator::Fourcc::Abgr8888,
                (width as i32, height as i32),
                1,
                smithay::utils::Transform::Normal,
                None,
            );
            greeter.wallpapers.insert(index, buffer);
        }
        if index == greeter.user {
            // Crossfade from the made-up accent to the shared one.
            greeter.previous_accent = (self.theme.accent, self.theme.accent_end);
            greeter.previous_user = index;
            greeter.switched_at = Instant::now();
            self.apply_greeter_accent();
        }
        self.schedule_render();
    }

    /// Publishes this session's look for the login screen, off the event loop.
    pub fn publish_greeter_card(&self) {
        if !self.session_services {
            return;
        }
        let screen = self.output_rect();
        let size = ((screen.w * self.scale).round() as u32, (screen.h * self.scale).round() as u32);
        let publish = card::Publish::from_theme(&self.theme, self.config.file.greeter.share, size);
        let spawned = std::thread::Builder::new()
            .name("tileception-greeter-card".into())
            .spawn(move || publish.run(&card::folder()));
        if let Err(error) = spawned {
            tracing::warn!("greeter card: {error}");
        }
    }

    /// Shows the selected person's accent everywhere, notifications included.
    fn apply_greeter_accent(&mut self) {
        let Some(greeter) = self.greeter.as_deref().filter(|greeter| !greeter.locked()) else { return };
        let base = crate::theme::Theme::from_file(&greeter.file.theme).unwrap_or_else(|_| self.theme.clone());
        let (accent, accent_end) = greeter.accent_of(&base, greeter.user);
        self.theme.accent = accent;
        self.theme.accent_end = accent_end;
    }

    fn select_person(&mut self, index: usize) {
        let theme = self.theme.clone();
        let Some(greeter) = self.greeter.as_deref_mut() else { return };
        if index >= greeter.people() || greeter.busy() || greeter.locked() {
            return;
        }
        greeter.focus = Panel::Login;
        if index == greeter.user {
            return;
        }
        if matches!(greeter.auth, Auth::Asking { .. }) {
            greeter.cancel();
        }
        greeter.previous_accent = (theme.accent, theme.accent_end);
        greeter.previous_user = greeter.user;
        greeter.switched_at = Instant::now();
        greeter.user = index;
        greeter.field.clear();
        greeter.name.clear();
        greeter.named = false;
        greeter.attempts = 0;
        greeter.failed_at = None;
        greeter.session_cursor = greeter.session_of_selected();
        self.apply_greeter_accent();
        self.greeter_person_keymap();
    }

    fn step_person(&mut self, delta: isize) {
        let Some(greeter) = self.greeter.as_deref() else { return };
        let count = greeter.people() as isize;
        self.select_person((greeter.user as isize + delta).rem_euclid(count) as usize);
    }

    /// A key on the greeter, pressed or let go.
    /// `layout` is the keymap's active layout, which xkb options such as
    /// grp:alt_shift_toggle can change too.
    pub fn greeter_key(&mut self, sym: u32, mods: Mods, caps_lock: bool, layout: u32, pressed: bool) {
        let nested = self.nested;
        let Some(greeter) = self.greeter.as_deref_mut() else { return };
        greeter.caps_lock = caps_lock;
        greeter.layout = layout as usize;
        // Nested, Alt stands in for Super, which belongs to the outer desktop.
        let super_key = matches!(sym, keysyms::KEY_Super_L | keysyms::KEY_Super_R)
            || (nested && matches!(sym, keysyms::KEY_Alt_L | keysyms::KEY_Alt_R));
        if !pressed {
            if super_key && std::mem::take(&mut greeter.super_tap) {
                self.toggle_greeter_palette();
            }
            return;
        }
        if super_key {
            greeter.super_tap = true;
            return;
        }
        greeter.super_tap = false;
        if crate::config::modifier_of(sym).is_some() || sym == keysyms::KEY_Caps_Lock {
            return;
        }
        let logo = mods.logo || (nested && mods.alt);
        if greeter.palette.is_some() {
            self.greeter_palette_key(sym, logo);
            return;
        }
        let dir = match sym {
            keysyms::KEY_Left => Some(Dir::Left),
            keysyms::KEY_Right => Some(Dir::Right),
            keysyms::KEY_Up => Some(Dir::Up),
            keysyms::KEY_Down => Some(Dir::Down),
            _ => None,
        };
        // Up and down choose a session while that panel has focus.
        let choosing = greeter.focus == Panel::Session && matches!(dir, Some(Dir::Up | Dir::Down)) && !logo;
        if let Some(dir) = dir
            && !choosing
        {
            self.move_greeter_focus(dir);
            return;
        }
        if logo {
            match sym {
                keysyms::KEY_Tab => self.step_person(if mods.shift { -1 } else { 1 }),
                keysyms::KEY_ISO_Left_Tab => self.step_person(-1),
                keysyms::KEY_x | keysyms::KEY_X => self.toggle_greeter_palette(),
                _ => {}
            }
            return;
        }
        if mods.ctrl {
            match sym {
                keysyms::KEY_u | keysyms::KEY_U => greeter.field.clear(),
                keysyms::KEY_space if greeter.layouts.len() > 1 && greeter.keymap.is_none() => {
                    self.next_keyboard_layout();
                }
                keysyms::KEY_space => self.open_layout_picker(),
                _ => {}
            }
            return;
        }
        match sym {
            keysyms::KEY_Tab => self.step_person(if mods.shift { -1 } else { 1 }),
            keysyms::KEY_ISO_Left_Tab => self.step_person(-1),
            keysyms::KEY_Up | keysyms::KEY_Down => {
                if sym == keysyms::KEY_Up && greeter.session_cursor == 0 {
                    self.move_greeter_focus(Dir::Up);
                } else if sym == keysyms::KEY_Up {
                    greeter.session_cursor -= 1;
                } else {
                    greeter.session_cursor = (greeter.session_cursor + 1).min(greeter.sessions.len().saturating_sub(1));
                }
            }
            keysyms::KEY_Return | keysyms::KEY_KP_Enter if greeter.focus == Panel::Session && greeter.locked() => {
                self.run_greeter_action(Action::Media);
            }
            keysyms::KEY_Return | keysyms::KEY_KP_Enter if greeter.focus == Panel::Session => {
                let cursor = greeter.session_cursor;
                greeter.choose_session(cursor);
                greeter.focus = Panel::Login;
            }
            keysyms::KEY_Return | keysyms::KEY_KP_Enter => {
                greeter.focus = Panel::Login;
                self.greeter_submit();
            }
            keysyms::KEY_Escape => greeter.escape(),
            keysyms::KEY_BackSpace if !greeter.busy() => {
                greeter.focus = Panel::Login;
                greeter.typed_at = Instant::now();
                if greeter.other() && !greeter.named {
                    greeter.name.backspace();
                } else {
                    greeter.field.backspace();
                }
            }
            _ if !greeter.busy() => {
                let typed = if greeter.other() && !greeter.named {
                    greeter.name.type_key(sym)
                } else {
                    greeter.field.type_key(sym)
                };
                if typed {
                    greeter.focus = Panel::Login;
                    greeter.typed_at = Instant::now();
                    greeter.failed_at = None;
                }
            }
            _ => {}
        }
    }

    fn move_greeter_focus(&mut self, dir: Dir) {
        let placed = self.greeter_panels();
        let Some(greeter) = self.greeter.as_deref_mut() else { return };
        if let Some(next) = layout::neighbour(&placed, greeter.focus, dir) {
            if next == Panel::Session && greeter.focus != Panel::Session {
                greeter.session_cursor = greeter.session_of_selected();
            }
            greeter.focus = next;
        }
    }

    /// The keymap's layouts for the bar and the launcher, and the active one.
    pub fn greeter_keyboard_layouts(&mut self) {
        let labels = self.config.file.keyboard.layouts();
        let active = self.seat.get_keyboard().map_or(0, |keyboard| {
            keyboard.with_xkb_state(self, |context| context.xkb().lock().map_or(0, |xkb| xkb.active_layout().0))
        });
        if let Some(greeter) = self.greeter.as_deref_mut() {
            greeter.layouts = labels;
            greeter.layout = active as usize;
        }
    }

    /// Switches to the keymap's next layout. Keys reach the greeter while
    /// the keyboard is busy with them, so the switch waits until it is not.
    fn next_keyboard_layout(&mut self) {
        if self.greeter.as_deref().is_none_or(|greeter| greeter.layouts.len() < 2) {
            return;
        }
        self.loop_handle.insert_idle(|state| {
            if let Some(keyboard) = state.seat.get_keyboard() {
                keyboard.with_xkb_state(state, |mut context| context.cycle_next_layout());
            }
            state.greeter_keyboard_layouts();
            state.schedule_render();
        });
    }

    /// The launcher, searching keyboard layouts.
    fn open_layout_picker(&mut self) {
        let Some(greeter) = self.greeter.as_deref_mut() else { return };
        if greeter.busy() {
            return;
        }
        greeter.palette =
            Some(Palette { query: "layout ".into(), selected: 0, opened: Some(Instant::now()), ..Default::default() });
    }

    /// Types with `picked` instead of the configured keymap, or with the
    /// configured keymap again (at its layout `group`). Waits until the
    /// keyboard is not busy with a key.
    fn set_greeter_keymap(&mut self, picked: Option<(String, String)>, group: Option<usize>) {
        self.loop_handle.insert_idle(move |state| {
            let current = state.greeter.as_deref().and_then(|greeter| greeter.keymap.clone());
            if picked != current && !state.load_keymap(picked.as_ref()) {
                return;
            }
            if let (Some(group), Some(keyboard)) = (group, state.seat.get_keyboard()) {
                keyboard.with_xkb_state(state, |mut context| {
                    context.set_layout(smithay::input::keyboard::Layout(group as u32));
                });
            }
            if let Some(greeter) = state.greeter.as_deref_mut() {
                greeter.keymap = picked.clone();
            }
            state.greeter_keyboard_layouts();
            state.schedule_render();
        });
    }

    /// Compiles `picked` (layout, variant), or the configured keymap, with the
    /// configured model and options. Not while the keyboard handles a key.
    fn load_keymap(&mut self, picked: Option<&(String, String)>) -> bool {
        let Some(keyboard) = self.seat.get_keyboard() else { return false };
        let configured = &self.config.file.keyboard;
        let (model, layout, variant, options) = match picked {
            Some((layout, variant)) => (
                configured.model.clone().unwrap_or_default(),
                layout.clone(),
                variant.clone(),
                configured.options.clone(),
            ),
            None => configured.names(),
        };
        let xkb = smithay::input::keyboard::XkbConfig {
            layout: &layout,
            variant: &variant,
            model: &model,
            options,
            ..Default::default()
        };
        if let Err(error) = keyboard.set_xkb_config(self, xkb) {
            tracing::warn!("greeter: keyboard layout {layout:?} {variant:?} did not compile: {error:?}");
            return false;
        }
        true
    }

    /// The selected person's own layout, if they picked one here before.
    fn greeter_person_keymap(&mut self) {
        let Some(greeter) = self.greeter.as_deref() else { return };
        let wanted = greeter.selected().and_then(|account| greeter.remembered_keymap(&account.login));
        if wanted != greeter.keymap {
            self.set_greeter_keymap(wanted, None);
        }
    }

    /// Unlocking: back to the session's keymap if one was picked while locked.
    pub fn end_greeter_keymap(&mut self) {
        if self.greeter.as_deref().is_some_and(|greeter| greeter.keymap.is_some()) {
            self.loop_handle.insert_idle(|state| {
                state.load_keymap(None);
            });
        }
    }

    fn toggle_greeter_palette(&mut self) {
        let Some(greeter) = self.greeter.as_deref_mut() else { return };
        if greeter.busy() {
            return;
        }
        greeter.palette = match greeter.palette {
            Some(_) => None,
            None => Some(Palette { opened: Some(Instant::now()), ..Default::default() }),
        };
    }

    fn greeter_palette_key(&mut self, sym: u32, logo: bool) {
        let actions = self.greeter_actions();
        let Some(greeter) = self.greeter.as_deref_mut() else { return };
        let Some(palette) = greeter.palette.as_mut() else { return };
        let count = actions.len();
        match sym {
            keysyms::KEY_Escape => {
                greeter.palette = match palette.page {
                    PalettePage::Main => None,
                    PalettePage::More => Some(Palette { opened: Some(Instant::now()), ..Default::default() }),
                    PalettePage::ConfirmFirmware => {
                        Some(Palette { page: PalettePage::More, opened: Some(Instant::now()), ..Default::default() })
                    }
                };
            }
            keysyms::KEY_x | keysyms::KEY_X if logo => greeter.palette = None,
            keysyms::KEY_Down | keysyms::KEY_Tab => {
                palette.selected = (palette.selected + 1).min(count.saturating_sub(1))
            }
            keysyms::KEY_Up | keysyms::KEY_ISO_Left_Tab => palette.selected = palette.selected.saturating_sub(1),
            keysyms::KEY_BackSpace => {
                palette.query.pop();
                palette.selected = 0;
            }
            keysyms::KEY_Return | keysyms::KEY_KP_Enter => {
                let chosen = actions.get(palette.selected).map(|(action, _)| *action);
                if let Some(action) = chosen {
                    self.run_greeter_action(action);
                }
            }
            _ if !logo => {
                if let Some(character) = char::from_u32(smithay::input::keyboard::xkb::keysym_to_utf32(sym.into()))
                    && !character.is_control()
                {
                    palette.query.push(character);
                    palette.selected = 0;
                }
            }
            _ => {}
        }
    }

    fn run_greeter_action(&mut self, action: Action) {
        let Some(greeter) = self.greeter.as_deref_mut() else { return };
        let page = greeter.palette.take().map(|palette| palette.page).unwrap_or_default();
        let args: &[&str] = match action {
            Action::More | Action::Firmware | Action::Back => {
                let next = match action {
                    Action::More => PalettePage::More,
                    Action::Firmware => PalettePage::ConfirmFirmware,
                    Action::Back if page == PalettePage::ConfirmFirmware => PalettePage::More,
                    _ => PalettePage::Main,
                };
                greeter.palette = Some(Palette { page: next, opened: Some(Instant::now()), ..Default::default() });
                return;
            }
            Action::Media => {
                self.media_request(tileception_media::Request::Player(tileception_media::PlayerCommand::PlayPause));
                return;
            }
            Action::Suspend => &["suspend"],
            Action::Restart => &["reboot"],
            Action::PowerOff => &["poweroff"],
            Action::ConfirmFirmware => &["reboot", "--firmware-setup"],
            Action::Session(session) => {
                greeter.choose_session(session);
                return;
            }
            Action::Person(person) => {
                self.select_person(person);
                return;
            }
            Action::Layout(layout) => {
                self.set_greeter_keymap(None, Some(layout));
                return;
            }
            Action::Keymap(keymap) => {
                let picked = greeter.known().get(keymap).map(|known| (known.layout.clone(), known.variant.clone()));
                if picked.is_some() {
                    self.set_greeter_keymap(picked, None);
                }
                return;
            }
        };
        if greeter.demo {
            self.notify_locally("Demo", &format!("Would run systemctl {}", args.join(" ")), Urgency::Normal);
            return;
        }
        let args: Vec<String> = args.iter().map(|arg| arg.to_string()).collect();
        self.spawn("systemctl", &args);
    }

    /// Enter on the login panel.
    fn greeter_submit(&mut self) {
        let Some(greeter) = self.greeter.as_deref_mut() else { return };
        if greeter.busy() {
            return;
        }
        if greeter.locked() {
            let password = greeter.field.take();
            greeter.auth = Auth::Answering;
            self.submit_unlock(password);
            return;
        }
        if greeter.other() && !greeter.named {
            if greeter.login().is_some() {
                greeter.named = true;
                greeter.session_cursor = greeter.session_of_selected();
            } else {
                greeter.failed_at = Some(Instant::now());
            }
            return;
        }
        if let Auth::Asking { .. } = greeter.auth {
            let answer = greeter.field.take();
            greeter.auth = Auth::Answering;
            greeter.send(Request::PostAuthMessageResponse { response: Some(answer) });
            return;
        }
        let Some(login) = greeter.login() else { return };
        let session = greeter.session_of_selected();
        if greeter.sessions.get(session).is_none() {
            self.notify_locally("No session to start", "No desktop sessions are installed.", Urgency::Critical);
            return;
        }
        greeter.answer = Some(greeter.field.take());
        greeter.attempt = Some((login.clone(), session));
        greeter.auth = Auth::Creating;
        greeter.send(Request::CreateSession { username: login });
    }

    fn greetd_ready(&mut self) {
        loop {
            let Some(greeter) = self.greeter.as_deref_mut() else { return };
            let Some(responses) = greeter.responses.as_ref() else { return };
            match responses.try_recv() {
                Ok(Ok(response)) => self.greetd_response(response),
                Ok(Err(error)) => {
                    tracing::error!("greeter: greetd: {error}");
                    greeter.greetd = None;
                    greeter.responses = None;
                    greeter.auth = Auth::Idle;
                    self.notify_locally("Cannot log in", "Lost the connection to greetd.", Urgency::Critical);
                }
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => break,
            }
        }
        self.schedule_render();
    }

    fn greetd_response(&mut self, response: Response) {
        let Some(greeter) = self.greeter.as_deref_mut() else { return };
        tracing::debug!("greetd answered {:?} while {:?}", response, greeter.auth);
        match response {
            Response::AuthMessage { auth_message_type: prompt @ (Prompt::Secret | Prompt::Visible), auth_message } => {
                // The first question is the password; later ones take the field again.
                if let Some(answer) = greeter.answer.take() {
                    greeter.auth = Auth::Answering;
                    greeter.send(Request::PostAuthMessageResponse { response: Some(answer) });
                } else {
                    greeter.field.clear();
                    greeter.auth = Auth::Asking { prompt, text: auth_message.trim().to_string() };
                }
            }
            Response::AuthMessage { auth_message_type: kind, auth_message } => {
                greeter.send(Request::PostAuthMessageResponse { response: None });
                let urgency = if kind == Prompt::Error { Urgency::Critical } else { Urgency::Normal };
                self.notify_locally("Logging in", auth_message.trim(), urgency);
            }
            Response::Success => match greeter.auth {
                Auth::Creating | Auth::Answering => greeter.logged_in(),
                Auth::Starting if greeter.demo => {
                    let line = greeter
                        .attempt
                        .as_ref()
                        .and_then(|(_, session_index)| greeter.sessions.get(*session_index))
                        .map(|session| session.exec.clone());
                    let login = greeter.attempt.take().map(|(login, _)| login).unwrap_or_default();
                    greeter.auth = Auth::Idle;
                    greeter.field.clear();
                    let body = format!("Would start {} for {login}", line.unwrap_or_default());
                    self.notify_locally("Logged in (demo)", &body, Urgency::Normal);
                }
                Auth::Starting => {
                    tracing::info!("greeter: session starting, handing the seat back to greetd");
                    greeter.leaving = Some(Instant::now());
                    let timer = smithay::reexports::calloop::timer::Timer::from_duration(FADE_OUT);
                    let timed = self.loop_handle.insert_source(timer, |_, _, state| {
                        state.loop_signal.stop();
                        smithay::reexports::calloop::timer::TimeoutAction::Drop
                    });
                    // Without a timer, no fade: greetd must not wait on us.
                    if timed.is_err() {
                        self.loop_signal.stop();
                    }
                }
                _ => greeter.auth = Auth::Idle,
            },
            // Cancelling a session that already ended: nothing to undo.
            Response::Error { .. } if greeter.auth == Auth::Cancelling => greeter.auth = Auth::Idle,
            Response::Error { error_type, description } => {
                let starting = greeter.auth == Auth::Starting;
                greeter.answer = None;
                greeter.cancel();
                let login = greeter.attempt.take().map(|(login, _)| login).unwrap_or_default();
                if error_type == ErrorType::AuthError {
                    greeter.attempts += 1;
                    greeter.failed_at = Some(Instant::now());
                    let body = if greeter.other() {
                        greeter.named = false;
                        format!("Authentication failed for \u{201c}{login}\u{201d}")
                    } else {
                        format!("Wrong password for {login} ({} attempt)", ordinal(greeter.attempts))
                    };
                    self.notify_locally("Login failed", &body, Urgency::Critical);
                } else {
                    tracing::warn!("greeter: greetd: {description}");
                    greeter.failed_at = Some(Instant::now());
                    let summary = if starting { "Could not start the session" } else { "Login failed" };
                    self.notify_locally(summary, &description, Urgency::Critical);
                }
            }
        }
    }

    /// The panels where they are drawn, below the bar.
    fn greeter_panels(&self) -> Vec<(Panel, Rect)> {
        let Some(greeter) = self.greeter.as_deref() else { return Vec::new() };
        let screen = self.output_rect();
        let area = Rect::new(screen.x, screen.y + draw::BAR_H, screen.w, (screen.h - draw::BAR_H).max(0.0));
        layout::place(&greeter.file.panels, area, self.config.file.general.gap.clamp(4.0, 32.0))
    }

    /// A press of the main button.
    pub fn greeter_click(&mut self, x_pos: f64, y_pos: f64) {
        let point = Point::new(x_pos, y_pos);
        let targets = self.greeter_targets();
        let actions = self.greeter_actions();
        let Some(greeter) = self.greeter.as_deref_mut() else { return };
        let hit = targets.iter().find(|(rect, _)| rect.contains(point)).map(|(_, target)| *target);
        if greeter.palette.is_some() {
            match hit {
                Some(draw::Target::Action(index)) => {
                    let chosen = actions.get(index).map(|(action, _)| *action);
                    if let Some(action) = chosen {
                        self.run_greeter_action(action);
                    }
                }
                Some(draw::Target::Palette) => {}
                _ => greeter.palette = None,
            }
            return;
        }
        match hit {
            Some(draw::Target::Person(person)) => self.select_person(person),
            Some(draw::Target::Session(session)) => {
                if !greeter.busy() {
                    greeter.choose_session(session);
                    greeter.focus = Panel::Session;
                }
            }
            Some(draw::Target::Submit) => {
                greeter.focus = Panel::Login;
                self.greeter_submit();
            }
            Some(draw::Target::Run(action)) => self.run_greeter_action(action),
            Some(draw::Target::Launcher) => self.toggle_greeter_palette(),
            Some(draw::Target::Layout) => self.open_layout_picker(),
            Some(draw::Target::Panel(panel)) => {
                if panel == Panel::Session && greeter.focus != Panel::Session {
                    greeter.session_cursor = greeter.session_of_selected();
                }
                greeter.focus = panel;
            }
            Some(draw::Target::Action(_) | draw::Target::Palette) | None => {}
        }
    }

    /// Hovering the launcher highlights the row under the pointer.
    pub fn greeter_motion(&mut self, x_pos: f64, y_pos: f64) {
        let point = Point::new(x_pos, y_pos);
        let targets = self.greeter_targets();
        let Some(palette) = self.greeter.as_deref_mut().and_then(|greeter| greeter.palette.as_mut()) else { return };
        if let Some(index) = targets.iter().find_map(|(rect, target)| match target {
            draw::Target::Action(index) if rect.contains(point) => Some(*index),
            _ => None,
        }) {
            palette.selected = index;
        }
    }
}

impl Greeter {
    /// What PAM said about the password typed on the lock screen; the caller
    /// unlocks on success. Returns the notification to show, if any.
    pub fn unlock_result(
        &mut self,
        outcome: &Result<tileception_auth::Outcome, tileception_auth::Error>,
    ) -> Option<(&'static str, String)> {
        use tileception_auth::Outcome;
        self.auth = Auth::Idle;
        if matches!(outcome, Ok(Outcome::Success)) {
            return None;
        }
        self.failed_at = Some(Instant::now());
        Some(match outcome {
            Ok(Outcome::WrongPassword) => {
                self.attempts += 1;
                ("Wrong password", format!("The session stays locked ({} attempt)", ordinal(self.attempts)))
            }
            Ok(Outcome::AccountProblem { .. }) => ("Account unavailable", "Contact your administrator.".to_string()),
            Ok(Outcome::TryAgainLater { .. }) => {
                ("Try again later", "Too many attempts, or the password service is busy.".to_string())
            }
            _ => ("Authentication failed", "The session remains locked.".to_string()),
        })
    }

    /// The password could not be checked at all.
    pub fn unlock_unavailable(&mut self) {
        self.auth = Auth::Idle;
        self.failed_at = Some(Instant::now());
    }

    fn send(&mut self, request: Request) {
        let sent = match &self.greetd {
            Some(greetd) => greetd.send(request),
            None => Err("not connected to greetd".into()),
        };
        if let Err(error) = sent {
            tracing::error!("greeter: {error}");
            self.auth = Auth::Idle;
        }
    }

    /// Ends the conversation, for another try.
    fn cancel(&mut self) {
        self.auth = Auth::Cancelling;
        self.field.clear();
        self.send(Request::CancelSession);
    }

    /// greetd accepted the answers: remember who, and start their session.
    fn logged_in(&mut self) {
        let Some((login, index)) = self.attempt.clone() else {
            self.auth = Auth::Idle;
            return;
        };
        let Some(session) = self.sessions.get(index) else {
            self.cancel();
            return;
        };
        let (cmd, env) = session.command(&self.file.xsession_wrapper);
        // Someone not listed is not preselected next time.
        if !self.other() {
            self.memory.last_user = Some(login.clone());
        }
        self.memory.sessions.insert(login.clone(), session.session_id.clone());
        let now =
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |elapsed| elapsed.as_secs());
        self.memory.logins.insert(login.clone(), i64::try_from(now).unwrap_or(0));
        match &self.keymap {
            Some((layout, variant)) => self.memory.layouts.insert(login, keymap::remembered(layout, variant)),
            None => self.memory.layouts.remove(&login),
        };
        if !self.demo
            && let Err(error) = self.memory.save(&self.memory_path)
        {
            tracing::warn!("greeter: could not remember the login: {error}");
        }
        tracing::info!("greeter: starting {:?}", cmd);
        self.auth = Auth::Starting;
        self.send(Request::StartSession { cmd, env });
    }

    fn escape(&mut self) {
        if matches!(self.auth, Auth::Asking { .. }) {
            self.cancel();
        } else if self.busy() {
            // Too late to take anything back.
        } else if self.other() && self.named && self.field.is_empty() {
            self.named = false;
        } else if self.other() && !self.named {
            self.name.clear();
        } else {
            self.field.clear();
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn ordinals() {
        let got: Vec<String> = [1, 2, 3, 4, 11, 12, 13, 21, 22, 101, 111].into_iter().map(super::ordinal).collect();
        assert_eq!(got, ["1st", "2nd", "3rd", "4th", "11th", "12th", "13th", "21st", "22nd", "101st", "111th"]);
    }
}
