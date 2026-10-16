//! The doctor: a panel along the left edge for asking a coding agent about
//! your setup in plain language. It stays beside the desktop (it does not
//! block it), so changes can be watched as they land.
//!
//! Agents come from the ACP Registry and speak the Agent Client Protocol
//! (`tileception-acp`). The panel picks one, starts it, logs in when the agent
//! asks, and shows the conversation with tool calls and permission questions.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use serde::{Deserialize, Serialize};
use smithay::{
    backend::{
        allocator::Fourcc,
        renderer::{element::memory::MemoryRenderBuffer, gles::GlesRenderer},
    },
    input::keyboard::{keysyms, xkb},
    reexports::calloop::{RegistrationToken, channel},
    utils::Transform,
};
use tileception_acp::{Agent, AgentCommand, Choice, Event, Login, LoginHow, Request, Session, SessionOption, Status};
use tileception_core::{Point, Rect};

use crate::{
    Tileception,
    config::Mods,
    doctor_policy::{self, DiffLine, Mode, Scope, Sign},
    launcher::Paste,
    markdown::{self, Kind, Piece, Span},
    render::OutputElement,
    shapes::Shape,
    text::{RichImage, RichStyle},
    text_input::TextInput,
    theme::Colour,
    ui::{Align, Painter, Style, ease_out, glyph},
};

const PANEL_W: f64 = 480.0;
const HEADER_H: f64 = 56.0;
const INPUT_H: f64 = 46.0;
/// The row under the field: permission mode and model.
const TOOLBAR_H: f64 = 38.0;
const HINTS_H: f64 = 34.0;
/// A row in a menu over the conversation (slash commands, models).
const MENU_ROW_H: f64 = 32.0;
/// Suggestions shown at most while typing a slash command.
const MENU_ROWS: usize = 7;
const PAD: f64 = 12.0;
/// Lines a single message may wrap to before it is cut short.
const MAX_LINES: u8 = 200;
const SCROLL_STEP: f64 = 48.0;
const ROW_H: f64 = 56.0;
const BUTTON_H: f64 = 34.0;
const GAP: f64 = 10.0;
/// Lines of a diff shown in a question; the rest are counted.
const DIFF_LINES: usize = 24;
const DIFF_LINE_H: f64 = 17.0;
/// Pictures waiting to go with the next message, at most.
const MAX_ATTACHMENTS: usize = 6;
/// A waiting or sent picture's square thumbnail.
const THUMB: i32 = 56;
/// Pictures in answers are scaled to fit this high.
const PICTURE_H: u32 = 280;
/// Pictures sent to the agent are scaled to fit this on their longest side.
const SEND_SIZE: u32 = 1568;
/// Space between the parts of one answer.
const PIECE_GAP: f64 = 8.0;

/// A small picture, premultiplied, ready to draw.
#[derive(Debug, Clone)]
pub struct Thumb {
    buffer: MemoryRenderBuffer,
    size: (i32, i32),
}

impl Thumb {
    fn new(rgba: &[u8], (w, h): (u32, u32)) -> Thumb {
        let size = (w as i32, h as i32);
        Thumb { buffer: MemoryRenderBuffer::from_slice(rgba, Fourcc::Abgr8888, size, 1, Transform::Normal, None), size }
    }
}

/// A picture to send with the next message.
struct Attachment {
    png: Arc<[u8]>,
    thumb: Thumb,
}

/// A picture in an answer, by address.
enum Picture {
    Loading,
    Ready(Thumb),
    Failed,
}

/// Pixels made off the compositor thread: (rgba, width, height).
type Pixels = (Vec<u8>, u32, u32);

/// Work done on other threads, coming back.
enum Loaded {
    Picture(String, Option<Pixels>),
    /// A pasted picture: as PNG to send, and its thumbnail.
    Attachment(Result<(Vec<u8>, Pixels), String>),
}

/// One entry in the conversation.
#[derive(Debug, Clone)]
pub enum Message {
    User(String),
    /// Pictures the user sent, before their message.
    Attached(Vec<Thumb>),
    Agent(String),
    /// From Tileception itself: errors, what is connected.
    Note(String),
    /// From Tileception, longer and left-aligned, like the command list.
    Info(String),
    /// A tool call: what it does and how far it got.
    Tool {
        id: String,
        title: String,
        status: String,
    },
    /// The agent asks before going ahead; `answer` once answered.
    Ask {
        reply: AskReply,
        title: String,
        choices: Vec<Choice>,
        /// The change it would make, when it shows one.
        diff: Vec<DiffLine>,
        answer: Option<String>,
    },
    /// A file the agent changed, with the copy from before for undoing.
    Changed {
        path: PathBuf,
        backup: Option<PathBuf>,
        added: usize,
        removed: usize,
        undone: bool,
    },
}

/// Where the answer to a question goes.
#[derive(Debug, Clone)]
pub enum AskReply {
    /// The agent's own permission request.
    Agent { id: serde_json::Value, paths: Vec<PathBuf> },
    /// A file the agent wants to read outside Tileception's places.
    Read { id: serde_json::Value, path: PathBuf, line: Option<u64>, limit: Option<u64> },
    /// A file the agent wants to write.
    Write { id: serde_json::Value, path: PathBuf, content: String },
}

/// What the panel shows in place of the conversation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum View {
    Conversation,
    Picker,
}

/// The remembered choice of agent.
#[derive(Default, Serialize, Deserialize)]
struct Saved {
    agent: Option<String>,
    #[serde(default)]
    mode: Mode,
    /// The model chosen per agent, by registry id.
    #[serde(default)]
    models: std::collections::BTreeMap<String, String>,
}

/// Typing a secret a login asks for, one at a time.
struct SecretEntry {
    method: String,
    labels: Vec<String>,
    values: Vec<String>,
}

/// One of the panel's own slash commands: name, argument, what it does.
const COMMANDS: [(&str, &str, &str); 7] = [
    ("help", "", "Show these commands"),
    ("clear", "", "Start a new conversation"),
    ("model", "[name]", "Choose the agent's model"),
    ("mode", "[ask|config|yolo]", "What the agent may do without asking"),
    ("agent", "[id]", "Choose another agent"),
    ("restart", "", "Restart the agent, as after logging in elsewhere"),
    ("image", "<file>", "Send a picture with the next message"),
];

/// A slash command offered while typing one.
#[derive(Debug, Clone, PartialEq)]
struct Suggestion {
    name: String,
    hint: String,
    description: String,
}

pub struct Doctor {
    pub open: bool,
    /// Typing goes to the panel (rather than the focused window).
    pub focused: bool,
    opened: Instant,
    edited: Instant,
    input: TextInput,
    compose: Option<xkb::compose::State>,
    pub messages: Vec<Message>,
    /// Pixels scrolled away from the newest message (or the top of the picker).
    scroll: f64,
    /// An answer is still coming in.
    pub busy: bool,
    view: View,
    registry: Option<Result<Vec<Agent>, String>>,
    /// What each registry agent still needs here, looked up once per load
    /// rather than searching PATH on every frame.
    missing: std::collections::HashMap<String, String>,
    loading: bool,
    /// The chosen agent's registry id.
    agent: Option<String>,
    session: Option<Session>,
    status: Status,
    events: Option<(channel::Sender<Event>, RegistrationToken)>,
    /// How much the agent may do without asking.
    pub mode: Mode,
    /// Files the user allowed the agent to change during this answer.
    approved: Vec<PathBuf>,
    /// The session's settings (model and such), as the agent reports them.
    options: Vec<SessionOption>,
    /// The agent's own slash commands.
    commands: Vec<AgentCommand>,
    /// The logins last offered, to go back to.
    logins: Vec<Login>,
    secret: Option<SecretEntry>,
    /// The open menu of a session option's values, by option id.
    menu: Option<String>,
    /// The highlighted row of the menu or of the slash command suggestions.
    menu_selected: usize,
    /// The remembered model per agent; put back on every new session.
    models: std::collections::BTreeMap<String, String>,
    restore_model: bool,
    /// Pictures pasted for the next message.
    attachments: Vec<Attachment>,
    /// The agent takes pictures in a prompt; unknown until it has started.
    takes_images: Option<bool>,
    /// A message with pictures, sent once the agent says how it takes them.
    waiting: Option<(String, Vec<Attachment>)>,
    /// Pictures in answers, by address.
    pictures: std::collections::HashMap<String, Picture>,
    loads: Option<(channel::Sender<Loaded>, RegistrationToken)>,
}

impl Default for Doctor {
    fn default() -> Self {
        let saved: Saved =
            std::fs::read_to_string(saved_path()).ok().and_then(|s| toml::from_str(&s).ok()).unwrap_or_default();
        Self {
            open: false,
            focused: false,
            opened: Instant::now(),
            edited: Instant::now(),
            input: TextInput::default(),
            compose: crate::ui::compose_state(),
            messages: Vec::new(),
            scroll: 0.0,
            busy: false,
            view: if saved.agent.is_some() { View::Conversation } else { View::Picker },
            registry: None,
            missing: Default::default(),
            loading: false,
            agent: saved.agent,
            session: None,
            status: Status::Starting(String::new()),
            events: None,
            mode: saved.mode,
            approved: Vec::new(),
            options: Vec::new(),
            commands: Vec::new(),
            logins: Vec::new(),
            secret: None,
            menu: None,
            menu_selected: 0,
            models: saved.models,
            restore_model: false,
            attachments: Vec::new(),
            takes_images: None,
            waiting: None,
            pictures: Default::default(),
            loads: None,
        }
    }
}

fn saved_path() -> PathBuf {
    let root = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".local/state"));
    root.join("tileception/doctor.toml")
}

/// What a click on a part of the panel does.
#[derive(Debug, Clone, PartialEq)]
enum Hit {
    Pick(String),
    OpenPicker,
    ClosePicker,
    ReloadRegistry,
    Login(String),
    LoggedIn,
    /// Back from a login in progress to the list of logins.
    BackToLogins,
    Answer(usize, Option<String>),
    Undo(usize),
    SetMode(Mode),
    OpenMenu(String),
    SetOption(String, String),
    /// A slash command suggestion, by its place in the list.
    Suggestion(usize),
    Example(usize),
    Stop,
    /// A link or picture in an answer.
    Open(String),
    /// A waiting picture, by its place.
    Detach(usize),
}

/// A row of a menu over the conversation.
struct MenuRow {
    rect: Rect,
    label: String,
    detail: String,
    /// The setting's value now.
    current: bool,
    /// Highlighted for the keyboard.
    selected: bool,
    hit: Hit,
}

/// What the toolbar's model button says: the value, and the setting's name
/// when it is not simply "Model".
fn model_label(option: &SessionOption) -> String {
    if option.name.eq_ignore_ascii_case("model") {
        option.current_name().to_string()
    } else {
        format!("{}: {}", option.name, option.current_name())
    }
}

/// A laid-out piece of the panel's middle.
enum Block {
    Bubble {
        rect: Rect,
        text: String,
        user: bool,
    },
    Note {
        rect: Rect,
        text: String,
    },
    Tool {
        rect: Rect,
        title: String,
        status: String,
    },
    Heading {
        rect: Rect,
        text: String,
    },
    Row {
        rect: Rect,
        title: String,
        detail: String,
        note: Option<String>,
        hit: Option<Hit>,
        current: bool,
    },
    Button {
        rect: Rect,
        label: String,
        hit: Hit,
        primary: bool,
    },
    Diff {
        rect: Rect,
        lines: Vec<DiffLine>,
    },
    Dots {
        rect: Rect,
    },
    /// Part of an answer: a paragraph, heading, list item or quote.
    Rich {
        rect: Rect,
        spans: Vec<Span>,
        style: RichStyle,
        marker: Option<(Rect, String)>,
        quote: bool,
    },
    Code {
        rect: Rect,
        spans: Vec<Span>,
        style: RichStyle,
    },
    Picture {
        rect: Rect,
        url: String,
        open: String,
    },
    Rule {
        rect: Rect,
    },
    /// Pictures the user sent, along the right.
    Thumbs {
        rect: Rect,
        thumbs: Vec<Thumb>,
    },
}

impl Block {
    fn rect(&self) -> Rect {
        match self {
            Block::Bubble { rect, .. }
            | Block::Note { rect, .. }
            | Block::Tool { rect, .. }
            | Block::Heading { rect, .. }
            | Block::Row { rect, .. }
            | Block::Button { rect, .. }
            | Block::Diff { rect, .. }
            | Block::Dots { rect }
            | Block::Rich { rect, .. }
            | Block::Code { rect, .. }
            | Block::Picture { rect, .. }
            | Block::Rule { rect }
            | Block::Thumbs { rect, .. } => *rect,
        }
    }

    fn rect_mut(&mut self) -> &mut Rect {
        match self {
            Block::Bubble { rect, .. }
            | Block::Note { rect, .. }
            | Block::Tool { rect, .. }
            | Block::Heading { rect, .. }
            | Block::Row { rect, .. }
            | Block::Button { rect, .. }
            | Block::Diff { rect, .. }
            | Block::Dots { rect }
            | Block::Rich { rect, .. }
            | Block::Code { rect, .. }
            | Block::Picture { rect, .. }
            | Block::Rule { rect }
            | Block::Thumbs { rect, .. } => rect,
        }
    }
}

/// Where the panel's fixed parts go.
struct Frame {
    panel: Rect,
    title: Rect,
    agent: Rect,
    messages: Rect,
    field: Rect,
    text: Rect,
    send: Rect,
    /// The permission mode, one segment per mode.
    modes: [Rect; 3],
    /// The model, as a button that opens its menu.
    model: Rect,
    hints: Rect,
    /// Pasted pictures waiting above the field, one square each.
    attached: Vec<Rect>,
}

/// Questions offered on an empty conversation; a click asks them.
const EXAMPLES: [&str; 3] =
    ["Make the gaps between windows smaller", "Use Ghostty as my terminal", "Why is my wallpaper not showing?"];

impl Tileception {
    /// The `doctor` command: opens the panel (taking the keyboard), or
    /// closes it when it already has the keyboard.
    pub fn toggle_doctor(&mut self) {
        if self.is_locked() {
            return;
        }
        if self.doctor.open && self.doctor.focused {
            self.close_doctor();
        } else {
            self.open_doctor();
        }
    }

    pub fn open_doctor(&mut self) {
        if !self.doctor.open {
            self.doctor.opened = Instant::now();
        }
        if self.launcher.open {
            self.close_launcher();
        }
        self.doctor.open = true;
        self.doctor.focused = true;
        self.doctor.edited = Instant::now();
        if self.doctor.registry.is_none() && !self.doctor.loading {
            self.load_registry(false);
        }
        self.force_keyboard_focus_sync();
        self.schedule_render();
    }

    pub fn close_doctor(&mut self) {
        self.doctor.open = false;
        self.doctor.focused = false;
        self.force_keyboard_focus_sync();
        self.schedule_render();
    }

    /// Opens the panel and asks `question` straight away.
    pub fn ask_doctor(&mut self, question: &str) {
        self.open_doctor();
        let question = question.trim();
        if !question.is_empty() {
            self.send_to_doctor(question.to_string());
        }
    }

    /// Where agent events come in, on the compositor thread.
    fn doctor_emitter(&mut self) -> Option<tileception_acp::Emit> {
        if self.doctor.events.is_none() {
            let (sender, receiver) = channel::channel::<Event>();
            let token = self
                .loop_handle
                .insert_source(receiver, |event, _, state| {
                    if let channel::Event::Msg(event) = event {
                        state.doctor_event(event);
                    }
                })
                .map_err(|e| tracing::warn!("doctor events: {e}"))
                .ok()?;
            self.doctor.events = Some((sender, token));
        }
        let sender = Mutex::new(self.doctor.events.as_ref()?.0.clone());
        Some(Arc::new(move |event| {
            let _ = sender.lock().unwrap_or_else(|p| p.into_inner()).send(event);
        }))
    }

    fn load_registry(&mut self, refresh: bool) {
        if let Some(emit) = self.doctor_emitter() {
            self.doctor.loading = true;
            tileception_acp::fetch_registry(refresh, emit);
        }
    }

    fn chosen_agent(&self) -> Option<Agent> {
        let id = self.doctor.agent.as_ref()?;
        self.doctor.registry.as_ref()?.as_ref().ok()?.iter().find(|a| &a.id == id).cloned()
    }

    /// Starts the chosen agent if it is not running yet.
    fn ensure_session(&mut self) {
        if self.doctor.session.is_some() {
            return;
        }
        let test_agent = test_agent();
        let agent = self.chosen_agent();
        if agent.is_none() && test_agent.is_none() {
            return;
        }
        let Some(emit) = self.doctor_emitter() else { return };
        // The documented defaults, for the agent to read.
        let defaults = state_dir().join("default-config.toml");
        if let Err(error) =
            std::fs::create_dir_all(state_dir()).and_then(|_| std::fs::write(&defaults, crate::config::DEFAULT))
        {
            tracing::warn!("{}: {error}", defaults.display());
        }
        let cwd = crate::config::path().parent().map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
        if let Err(error) = std::fs::create_dir_all(&cwd) {
            tracing::warn!("{}: {error}", cwd.display());
        }
        let context = doctor_context(self.doctor.mode.label());
        self.doctor.options.clear();
        self.doctor.commands.clear();
        self.doctor.menu = None;
        self.doctor.restore_model = true;
        self.doctor.takes_images = None;
        self.doctor.session = Some(match (test_agent, agent) {
            (Some(line), _) => {
                let mut words = line.split_whitespace().map(str::to_string);
                let command = words.next().unwrap_or_default().into();
                let program =
                    tileception_acp::install::Program { command, args: words.collect(), env: Default::default() };
                self.doctor.agent.get_or_insert_with(|| "test agent".into());
                Session::start_program("Test agent".into(), program, cwd, context, emit)
            }
            (None, Some(agent)) => {
                self.doctor.status = Status::Starting(format!("Starting {}\u{2026}", agent.name));
                Session::start(agent, cwd, context, emit)
            }
            (None, None) => return,
        });
    }

    /// `doctor use <id>`: picks an agent by its registry id.
    pub fn use_doctor_agent(&mut self, id: String) {
        self.open_doctor();
        self.pick_agent(id);
    }

    fn pick_agent(&mut self, id: String) {
        if self.doctor.agent.as_ref() != Some(&id) {
            self.doctor.session = None;
            self.doctor.busy = false;
            self.doctor.agent = Some(id);
            self.save_doctor();
        }
        self.doctor.view = View::Conversation;
        self.doctor.scroll = 0.0;
        self.ensure_session();
    }

    fn send_to_doctor(&mut self, text: String) {
        if text.starts_with('/') && self.doctor_command(&text) {
            return;
        }
        if self.doctor.busy {
            return;
        }
        if self.doctor.agent.is_none() && test_agent().is_none() {
            self.doctor.view = View::Picker;
            self.doctor.messages.push(Message::Note("Choose an agent first.".into()));
            return;
        }
        self.ensure_session();
        if self.doctor.session.is_none() {
            self.doctor.messages.push(Message::Note("The agent is not available yet.".into()));
            return;
        }
        let attachments = std::mem::take(&mut self.doctor.attachments);
        if !attachments.is_empty() {
            self.doctor.messages.push(Message::Attached(attachments.iter().map(|a| a.thumb.clone()).collect()));
        }
        if !text.is_empty() {
            self.doctor.messages.push(Message::User(text.clone()));
        }
        self.doctor.view = View::Conversation;
        self.doctor.scroll = 0.0;
        self.doctor.busy = true;
        // How pictures go depends on the agent, which may not have said yet.
        if self.doctor.takes_images.is_none() && !attachments.is_empty() {
            self.doctor.waiting = Some((text, attachments));
        } else {
            self.prompt_doctor(text, attachments);
        }
    }

    fn prompt_doctor(&mut self, text: String, attachments: Vec<Attachment>) {
        let Some(session) = &self.doctor.session else { return };
        let (text, images) = if self.doctor.takes_images == Some(true) {
            let images = attachments
                .into_iter()
                .map(|a| tileception_acp::Image { mime: "image/png".into(), data: a.png })
                .collect();
            (text, images)
        } else {
            // An agent that takes no pictures can still open a file.
            (with_saved_pictures(text, &attachments), Vec::new())
        };
        session.send(Request::Prompt { text, images });
    }

    /// Where pictures loaded on other threads come in.
    fn doctor_loads(&mut self) -> Option<channel::Sender<Loaded>> {
        if self.doctor.loads.is_none() {
            let (sender, receiver) = channel::channel::<Loaded>();
            let token = self
                .loop_handle
                .insert_source(receiver, |event, _, state| {
                    if let channel::Event::Msg(loaded) = event {
                        state.doctor_loaded(loaded);
                    }
                })
                .map_err(|e| tracing::warn!("doctor pictures: {e}"))
                .ok()?;
            self.doctor.loads = Some((sender, token));
        }
        Some(self.doctor.loads.as_ref()?.0.clone())
    }

    fn doctor_loaded(&mut self, loaded: Loaded) {
        match loaded {
            Loaded::Picture(url, pixels) => {
                let picture = match pixels {
                    Some((rgba, w, h)) => Picture::Ready(Thumb::new(&rgba, (w, h))),
                    None => Picture::Failed,
                };
                self.doctor.pictures.insert(url, picture);
            }
            Loaded::Attachment(Ok((png, (rgba, w, h)))) => {
                if self.doctor.attachments.len() < MAX_ATTACHMENTS {
                    self.doctor.attachments.push(Attachment { png: png.into(), thumb: Thumb::new(&rgba, (w, h)) });
                }
            }
            Loaded::Attachment(Err(error)) => self.doctor.messages.push(Message::Note(error)),
        }
        self.schedule_render();
    }

    /// Starts loading the pictures in the newest answer that are not loaded yet.
    fn load_pictures(&mut self) {
        let Some(Message::Agent(text)) = self.doctor.messages.last() else { return };
        let wanted: Vec<String> =
            markdown::images(text).into_iter().filter(|url| !self.doctor.pictures.contains_key(url)).collect();
        if wanted.is_empty() {
            return;
        }
        let Some(sender) = self.doctor_loads() else { return };
        for url in wanted {
            self.doctor.pictures.insert(url.clone(), Picture::Loading);
            let sender = sender.clone();
            std::thread::spawn(move || {
                let pixels = load_picture(&url);
                let _ = sender.send(Loaded::Picture(url, pixels));
            });
        }
    }

    /// A pasted or opened picture, made ready to send on another thread.
    pub fn doctor_attach(&mut self, bytes: Arc<[u8]>) {
        if self.doctor.attachments.len() >= MAX_ATTACHMENTS {
            let note = format!("At most {MAX_ATTACHMENTS} pictures go with one message.");
            self.doctor.messages.push(Message::Note(note));
            return;
        }
        let Some(sender) = self.doctor_loads() else { return };
        std::thread::spawn(move || {
            let _ = sender.send(Loaded::Attachment(prepare_attachment(&bytes)));
        });
    }

    /// A link or picture from an answer, in the browser or the app for it.
    fn open_link(&mut self, url: &str) {
        let known = ["https://", "http://", "mailto:", "file://"].iter().any(|s| url.starts_with(s));
        if known || url.starts_with('/') {
            self.spawn("xdg-open", &[url.to_string()]);
        }
    }

    fn doctor_event(&mut self, event: Event) {
        // Questions and files depend on the permission mode.
        let event = match event {
            Event::Permission { id, title, choices, action } => {
                self.doctor_permission(id, title, choices, action);
                self.schedule_render();
                return;
            }
            Event::File { id, op } => {
                self.doctor_file(id, op);
                self.schedule_render();
                return;
            }
            other => other,
        };
        let d = &mut self.doctor;
        match event {
            Event::Registry(result) => {
                d.loading = false;
                // A saved agent that left the registry: choose again.
                if let (Ok(agents), Some(id), None) = (&result, &d.agent, &d.session)
                    && !agents.iter().any(|a| &a.id == id)
                {
                    d.view = View::Picker;
                }
                d.missing = match &result {
                    Ok(agents) => agents.iter().filter_map(|a| Some((a.id.clone(), a.missing()?))).collect(),
                    Err(_) => Default::default(),
                };
                d.registry = Some(result);
                if d.view == View::Conversation && d.open {
                    self.ensure_session();
                }
            }
            Event::Status(status) => {
                match &status {
                    Status::Failed(error) => {
                        d.messages.push(Message::Note(error.clone()));
                        d.waiting = None;
                        d.session = None;
                        d.busy = false;
                        d.options.clear();
                        d.menu = None;
                    }
                    Status::NeedsLogin(logins) => {
                        d.busy = false;
                        d.logins.clone_from(logins);
                    }
                    _ => {}
                }
                d.status = status;
            }
            Event::Text(text) => {
                match d.messages.last_mut() {
                    Some(Message::Agent(last)) => last.push_str(&text),
                    _ => d.messages.push(Message::Agent(text)),
                }
                self.load_pictures();
            }
            Event::Thought(_) => {}
            Event::Tool { id, title, status, .. } => {
                let existing = d.messages.iter_mut().rev().find_map(|m| match m {
                    Message::Tool { id: tid, title, status } if *tid == id => Some((title, status)),
                    _ => None,
                });
                match existing {
                    Some((old_title, old_status)) => {
                        if let Some(title) = title {
                            *old_title = title;
                        }
                        if let Some(status) = status {
                            *old_status = status;
                        }
                    }
                    None => d.messages.push(Message::Tool {
                        id,
                        title: title.unwrap_or_else(|| "Working".into()),
                        status: status.unwrap_or_else(|| "pending".into()),
                    }),
                }
            }
            Event::Permission { .. } | Event::File { .. } => {}
            Event::TurnEnded(reason) => {
                d.busy = false;
                d.approved.clear();
                let note = match reason.as_str() {
                    "refusal" => Some("The agent declined to answer."),
                    "max_tokens" | "max_turn_requests" => Some("The agent stopped: its answer got too long."),
                    "cancelled" => Some("Stopped."),
                    _ => None,
                };
                // Not after /clear: the conversation it ended is gone.
                if let Some(note) = note.filter(|_| !d.messages.is_empty()) {
                    d.messages.push(Message::Note(note.into()));
                }
            }
            Event::Note(text) => d.messages.push(Message::Note(text)),
            Event::Terminal(program) => self.run_login_terminal(&program),
            Event::Options(options) => {
                d.options = options;
                // The model chosen before, for a new session too.
                let wanted = d.agent.as_ref().and_then(|agent| d.models.get(agent));
                let model = d.options.iter().find(|o| o.is_model());
                if let (true, Some(wanted), Some(model)) = (std::mem::take(&mut d.restore_model), wanted, model)
                    && *wanted != model.current
                    && model.values.iter().any(|v| v.value == *wanted)
                    && let Some(session) = &d.session
                {
                    session.send(Request::SetOption { id: model.id.clone(), value: wanted.clone() });
                }
            }
            Event::Commands(commands) => d.commands = commands,
            Event::Images(takes) => {
                d.takes_images = Some(takes);
                if let Some((text, attachments)) = d.waiting.take() {
                    self.prompt_doctor(text, attachments);
                }
            }
        }
        self.schedule_render();
    }

    fn doctor_reply(&self, id: serde_json::Value, result: Result<serde_json::Value, String>) {
        if let Some(session) = &self.doctor.session {
            session.send(Request::Reply { id, result });
        }
    }

    /// The agent's own permission request: decided by the mode, or asked.
    fn doctor_permission(
        &mut self,
        id: serde_json::Value,
        title: String,
        choices: Vec<Choice>,
        action: tileception_acp::Action,
    ) {
        let paths: Vec<PathBuf> =
            action.paths.iter().cloned().chain(action.diffs.iter().map(|d| d.0.clone())).collect();
        if let Some(choice) = doctor_policy::auto_choice(self.doctor.mode, &action, &choices, &doctor_scope()) {
            self.doctor.approved.extend(paths);
            if let Some(session) = &self.doctor.session {
                session.send(Request::Permission { id, choice: Some(choice) });
            }
            self.doctor.messages.push(Message::Note(format!("Went ahead ({}): {title}", self.doctor.mode.label())));
            return;
        }
        let diff = action
            .diffs
            .first()
            .map(|(_, old, new)| doctor_policy::diff(old.as_deref().unwrap_or(""), new).0)
            .unwrap_or_default();
        self.doctor.messages.push(Message::Ask {
            reply: AskReply::Agent { id, paths },
            title,
            choices,
            diff,
            answer: None,
        });
        self.doctor.scroll = 0.0;
    }

    /// A file the agent reads or writes through Tileception.
    fn doctor_file(&mut self, id: serde_json::Value, op: tileception_acp::FileOp) {
        let scope = doctor_scope();
        let mode = self.doctor.mode;
        let yes_no = |yes: &str, no: &str| {
            vec![
                Choice { id: "allow".into(), name: yes.into(), kind: "allow_once".into() },
                Choice { id: "deny".into(), name: no.into(), kind: "reject_once".into() },
            ]
        };
        match op {
            tileception_acp::FileOp::Read { path, line, limit } => {
                if doctor_policy::may_read(mode, &path, &scope) {
                    self.doctor_reply(id, read_for_agent(&path, line, limit));
                } else {
                    let title = format!("Read {}", path.display());
                    let reply = AskReply::Read { id, path, line, limit };
                    let choices = yes_no("Allow", "Deny");
                    self.doctor.messages.push(Message::Ask { reply, title, choices, diff: Vec::new(), answer: None });
                    self.doctor.scroll = 0.0;
                }
            }
            tileception_acp::FileOp::Write { path, content } => {
                // A configuration that would not load is sent back, not saved.
                if path == crate::config::path()
                    && let Err(error) = crate::config::Config::parse(&content)
                {
                    self.doctor_reply(
                        id,
                        Err(format!("Tileception would not load this configuration: {error}. Nothing was written.")),
                    );
                    return;
                }
                if doctor_policy::may_write(mode, &path, &scope, &self.doctor.approved) {
                    self.apply_write(id, path, content);
                } else {
                    let old = std::fs::read_to_string(&path).unwrap_or_default();
                    let diff = doctor_policy::diff(&old, &content).0;
                    let title = format!("Change {}", path.display());
                    let reply = AskReply::Write { id, path, content };
                    let choices = yes_no("Apply", "Reject");
                    self.doctor.messages.push(Message::Ask { reply, title, choices, diff, answer: None });
                    self.doctor.scroll = 0.0;
                }
            }
        }
    }

    /// Writes a file for the agent, keeping a copy of what was there.
    fn apply_write(&mut self, id: serde_json::Value, path: PathBuf, content: String) {
        let old = std::fs::read_to_string(&path).unwrap_or_default();
        let result = doctor_policy::backup(&path, &backups_dir()).and_then(|backup| {
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
            }
            std::fs::write(&path, &content).map_err(|e| format!("{}: {e}", path.display()))?;
            Ok(backup)
        });
        match result {
            Ok(backup) => {
                let (_, added, removed) = doctor_policy::diff(&old, &content);
                self.doctor.messages.push(Message::Changed { path, backup, added, removed, undone: false });
                self.doctor_reply(id, Ok(serde_json::json!({})));
            }
            Err(error) => {
                self.doctor.messages.push(Message::Note(format!("Could not save: {error}")));
                self.doctor_reply(id, Err(error));
            }
        }
    }

    /// Puts a changed file back as it was.
    fn undo_change(&mut self, index: usize) {
        let Some(Message::Changed { path, backup, undone: undone @ false, .. }) = self.doctor.messages.get_mut(index)
        else {
            return;
        };
        let result = match backup {
            Some(copy) => std::fs::copy(&*copy, &*path).map(|_| ()),
            None => std::fs::remove_file(&*path),
        };
        match result {
            Ok(()) => *undone = true,
            Err(error) => {
                let note = format!("Could not undo: {error}");
                self.doctor.messages.push(Message::Note(note));
            }
        }
    }

    pub fn set_doctor_mode(&mut self, mode: Mode) {
        self.doctor.mode = mode;
        self.save_doctor();
        self.schedule_render();
    }

    fn save_doctor(&self) {
        let saved =
            Saved { agent: self.doctor.agent.clone(), mode: self.doctor.mode, models: self.doctor.models.clone() };
        let path = saved_path();
        let written = path
            .parent()
            .map_or(Ok(()), std::fs::create_dir_all)
            .and_then(|_| std::fs::write(&path, toml::to_string(&saved).unwrap_or_default()));
        if let Err(error) = written {
            tracing::warn!("could not remember the doctor's settings: {error}");
        }
    }

    /// Runs an agent's own login in the terminal.
    fn run_login_terminal(&mut self, program: &tileception_acp::install::Program) {
        let quote = |s: &str| format!("'{}'", s.replace('\'', "'\\''"));
        let mut line = String::from("env");
        for (key, value) in &program.env {
            line += &format!(" {}={}", quote(key), quote(value));
        }
        line += &format!(" {}", quote(&program.command.display().to_string()));
        for arg in &program.args {
            line += &format!(" {}", quote(arg));
        }
        line += "; echo; echo 'Done: go back to the doctor panel.'; read -r _";
        let terminal = format!("{} -e sh -c {}", self.terminal, quote(&line));
        self.spawn("sh", &["-c".into(), terminal]);
        self.doctor
            .messages
            .push(Message::Note("Log in in the terminal, then choose \u{201c}I've logged in\u{201d}.".into()));
        self.doctor.scroll = 0.0;
    }

    /// A key while the panel has the keyboard.
    pub fn doctor_key(&mut self, sym: u32, mods: Mods) {
        self.doctor.edited = Instant::now();
        let word = mods.ctrl || mods.alt;
        // A menu over the conversation takes the arrows, Enter and Esc.
        let rows = match &self.doctor.menu {
            Some(_) => self.menu_values().len(),
            None => self.suggestions().len(),
        };
        if rows > 0 {
            let selected = self.doctor.menu_selected.min(rows - 1);
            match sym {
                keysyms::KEY_Up => {
                    self.doctor.menu_selected = (selected + rows - 1) % rows;
                    return;
                }
                keysyms::KEY_Down => {
                    self.doctor.menu_selected = (selected + 1) % rows;
                    return;
                }
                keysyms::KEY_Escape if self.doctor.menu.is_some() => {
                    self.doctor.menu = None;
                    return;
                }
                keysyms::KEY_Return | keysyms::KEY_KP_Enter if self.doctor.menu.is_some() => {
                    return self.choose_menu_value(selected);
                }
                keysyms::KEY_Tab => return self.complete_suggestion(selected, false),
                keysyms::KEY_Return | keysyms::KEY_KP_Enter => return self.complete_suggestion(selected, true),
                _ => {}
            }
        }
        let input = &mut self.doctor.input;
        match sym {
            keysyms::KEY_Escape if self.doctor.secret.is_some() => {
                self.doctor.secret = None;
                input.clear();
            }
            keysyms::KEY_Escape if self.doctor.view == View::Picker && self.doctor.agent.is_some() => {
                self.doctor.view = View::Conversation;
            }
            keysyms::KEY_Escape => self.close_doctor(),
            _ if mods.ctrl
                && matches!(sym, keysyms::KEY_c | keysyms::KEY_C)
                && self.doctor.busy
                && input.selection().is_empty() =>
            {
                self.doctor_hit(Hit::Stop);
            }
            keysyms::KEY_Return | keysyms::KEY_KP_Enter if self.doctor.secret.is_some() => {
                let value = input.text.trim().to_string();
                input.clear();
                if !value.is_empty() {
                    self.secret_entered(value);
                }
            }
            keysyms::KEY_Return | keysyms::KEY_KP_Enter => {
                let text = input.text.trim().to_string();
                let attached = !self.doctor.attachments.is_empty();
                // Commands also work while an answer comes in (/clear, /mode).
                if (!text.is_empty() || attached) && (!self.doctor.busy || text.starts_with('/')) {
                    input.clear();
                    self.send_to_doctor(text);
                }
            }
            keysyms::KEY_Page_Up => self.doctor_scroll(-4.0),
            keysyms::KEY_Page_Down => self.doctor_scroll(4.0),
            keysyms::KEY_Left => input.move_horizontal(false, word, mods.shift),
            keysyms::KEY_Right => input.move_horizontal(true, word, mods.shift),
            keysyms::KEY_Home => input.move_to(0, mods.shift),
            keysyms::KEY_End => input.move_to(input.text.len(), mods.shift),
            keysyms::KEY_BackSpace => input.delete(false, word),
            keysyms::KEY_Delete => input.delete(true, word),
            _ if mods.ctrl && matches!(sym, keysyms::KEY_a | keysyms::KEY_A) => input.select_all(),
            _ if mods.ctrl && matches!(sym, keysyms::KEY_u | keysyms::KEY_U) => input.clear(),
            _ if mods.ctrl && matches!(sym, keysyms::KEY_v | keysyms::KEY_V) => self.paste(Paste::Doctor),
            _ if !mods.ctrl && !mods.logo && !mods.alt => {
                if let Some(text) = crate::ui::compose_text(&mut self.doctor.compose, sym) {
                    self.doctor.input.insert(&text);
                }
            }
            _ => {}
        }
        self.doctor.menu_selected = 0;
    }

    pub fn doctor_insert_paste(&mut self, text: &str) {
        let text: String = if self.doctor.secret.is_some() {
            text.trim().chars().filter(|c| !c.is_control()).collect()
        } else {
            text.chars().map(|c| if c.is_control() { ' ' } else { c }).collect()
        };
        self.doctor.input.insert(&text);
        self.doctor.edited = Instant::now();
    }

    /// Scrolls; positive goes towards newer messages (or down the picker).
    pub fn doctor_scroll(&mut self, notches: f64) {
        let frame = self.doctor_frame();
        let content = self.doctor_content_height(frame.messages);
        let max = (content - frame.messages.h).max(0.0);
        let delta = notches * SCROLL_STEP;
        self.doctor.scroll = match self.doctor.view {
            View::Conversation => {
                // Only whole blocks are drawn, so the conversation moves a
                // block at a time: some block always sits whole at the bottom.
                let area = frame.messages;
                let mut stops: Vec<f64> = self
                    .doctor_content(area)
                    .iter()
                    .map(|b| (content - (b.rect().bottom() - area.y)).clamp(0.0, max))
                    .chain([0.0, max])
                    .collect();
                stops.sort_by(f64::total_cmp);
                stops.dedup_by(|a, b| (*a - *b).abs() < 0.5);
                // One block per press or wheel notch, whatever its size.
                let at = self.doctor.scroll;
                if notches < 0.0 {
                    stops.iter().copied().find(|s| *s > at + 0.5).unwrap_or(max)
                } else {
                    stops.iter().rev().copied().find(|s| *s < at - 0.5).unwrap_or(0.0)
                }
            }
            View::Picker => (self.doctor.scroll + delta).clamp(0.0, max),
        };
    }

    pub fn doctor_contains(&self, p: Point) -> bool {
        self.doctor.open && self.doctor_frame().panel.contains(p)
    }

    /// A press anywhere: inside, the panel takes the keyboard and acts on
    /// what was clicked; outside, the desktop gets the keyboard back.
    pub fn doctor_press(&mut self, p: Point) -> bool {
        if !self.doctor.open {
            return false;
        }
        let frame = self.doctor_frame();
        if !frame.panel.contains(p) {
            if self.doctor.focused {
                self.doctor.focused = false;
                self.force_keyboard_focus_sync();
            }
            return false;
        }
        if !self.doctor.focused {
            self.doctor.focused = true;
            self.force_keyboard_focus_sync();
        }
        // An open menu or the slash command list first: they lie on top.
        if let Some(hit) = self.menu_hit(p) {
            self.doctor_hit(hit);
            return true;
        }
        if self.doctor.menu.take().is_some() {
            self.schedule_render();
            return true;
        }
        if frame.send.contains(p) {
            if self.doctor.busy {
                self.doctor_hit(Hit::Stop);
            } else {
                self.doctor_key(keysyms::KEY_Return, Mods::default());
            }
            return true;
        }
        if let Some(i) = frame.modes.iter().position(|r| r.contains(p)) {
            self.doctor_hit(Hit::SetMode(Mode::ALL[i]));
            return true;
        }
        if let Some((option, _)) = self.model_button(&frame).filter(|(_, r)| r.contains(p)) {
            self.doctor_hit(Hit::OpenMenu(option.id.clone()));
            return true;
        }
        if frame.agent.contains(p) {
            self.doctor_hit(if self.doctor.view == View::Picker { Hit::ClosePicker } else { Hit::OpenPicker });
            return true;
        }
        if let Some(i) = frame.attached.iter().position(|r| r.contains(p)) {
            self.doctor_hit(Hit::Detach(i));
            return true;
        }
        let hit = self.doctor_blocks(frame.messages).into_iter().find_map(|b| match b {
            Block::Row { rect, hit: Some(hit), .. } | Block::Button { rect, hit, .. } if rect.contains(p) => Some(hit),
            Block::Picture { rect, open, .. } if rect.contains(p) => Some(Hit::Open(open)),
            Block::Rich { rect, spans, style, .. } if rect.contains(p) => {
                let image = self.rich_image(&spans, style, rect.w)?;
                image.links.into_iter().find_map(|([x, y, w, h], url)| {
                    let link = Rect::new(rect.x + f64::from(x), rect.y + f64::from(y), f64::from(w), f64::from(h));
                    link.contains(p).then_some(Hit::Open(url))
                })
            }
            _ => None,
        });
        if let Some(hit) = hit {
            self.doctor_hit(hit);
        }
        true
    }

    fn doctor_hit(&mut self, hit: Hit) {
        match hit {
            Hit::Pick(id) => self.pick_agent(id),
            Hit::OpenPicker => {
                self.doctor.view = View::Picker;
                self.doctor.scroll = 0.0;
                if self.doctor.registry.as_ref().is_none_or(|r| r.is_err()) && !self.doctor.loading {
                    self.load_registry(false);
                }
            }
            Hit::ClosePicker if self.doctor.agent.is_some() => self.doctor.view = View::Conversation,
            Hit::ClosePicker => {}
            Hit::ReloadRegistry => self.load_registry(true),
            Hit::Login(method) => {
                let asks = self.doctor.logins.iter().find(|l| l.id == method).map(|l| l.how.asks()).unwrap_or_default();
                if asks.is_empty() {
                    if let Some(session) = &self.doctor.session {
                        session.send(Request::Login { method, secrets: Vec::new() });
                    }
                } else {
                    self.doctor.input.clear();
                    self.doctor.secret = Some(SecretEntry { method, labels: asks, values: Vec::new() });
                    self.doctor.focused = true;
                    self.force_keyboard_focus_sync();
                }
            }
            Hit::LoggedIn => {
                if let Some(session) = &self.doctor.session {
                    session.send(Request::Retry);
                }
            }
            Hit::BackToLogins => {
                self.doctor.secret = None;
                self.doctor.status = Status::NeedsLogin(self.doctor.logins.clone());
            }
            Hit::Answer(index, choice) => {
                let Some(Message::Ask { reply, answer: answer @ None, choices, .. }) =
                    self.doctor.messages.get_mut(index)
                else {
                    return;
                };
                let picked = choice.as_ref().and_then(|c| choices.iter().find(|x| &x.id == c));
                let allowed = picked.is_some_and(|c| c.kind.starts_with("allow"));
                *answer = Some(picked.map_or_else(|| "Declined".to_string(), |c| c.name.clone()));
                let reply = reply.clone();
                match reply {
                    AskReply::Agent { id, paths } => {
                        if allowed {
                            self.doctor.approved.extend(paths);
                        }
                        if let Some(session) = &self.doctor.session {
                            session.send(Request::Permission { id, choice });
                        }
                    }
                    AskReply::Read { id, path, line, limit } => {
                        let result =
                            if allowed { read_for_agent(&path, line, limit) } else { Err("The user declined.".into()) };
                        self.doctor_reply(id, result);
                    }
                    AskReply::Write { id, path, content } => {
                        if allowed {
                            self.apply_write(id, path, content);
                        } else {
                            self.doctor_reply(id, Err("The user rejected this change; nothing was written.".into()));
                        }
                    }
                }
            }
            Hit::Undo(index) => self.undo_change(index),
            Hit::SetMode(mode) => self.set_doctor_mode(mode),
            Hit::OpenMenu(id) => {
                let open = self.doctor.menu.as_ref() == Some(&id);
                self.doctor.menu = (!open).then_some(id);
                // Start on the current value.
                self.doctor.menu_selected = self.menu_values().iter().position(|v| v.1).unwrap_or(0);
            }
            Hit::SetOption(id, value) => {
                self.doctor.menu = None;
                if let Some(option) = self.doctor.options.iter().find(|o| o.id == id)
                    && option.is_model()
                    && let Some(agent) = self.doctor.agent.clone()
                {
                    self.doctor.models.insert(agent, value.clone());
                    self.save_doctor();
                }
                if let Some(session) = &self.doctor.session {
                    session.send(Request::SetOption { id, value });
                }
            }
            Hit::Suggestion(i) => self.complete_suggestion(i, true),
            Hit::Open(url) => self.open_link(&url),
            Hit::Detach(i) => {
                if i < self.doctor.attachments.len() {
                    self.doctor.attachments.remove(i);
                }
            }
            Hit::Example(i) => self.send_to_doctor(EXAMPLES[i].to_string()),
            Hit::Stop => {
                // Not sent yet: nothing to cancel but the wait.
                if self.doctor.waiting.take().is_some() {
                    self.doctor.busy = false;
                }
                if let Some(session) = &self.doctor.session {
                    session.send(Request::Cancel);
                }
                // The agent's own questions are cancelled with the turn; its
                // file requests still need an answer, or it waits for ever.
                let mut unanswered = Vec::new();
                for m in &mut self.doctor.messages {
                    if let Message::Ask { answer: answer @ None, reply, .. } = m {
                        *answer = Some("Stopped".into());
                        if let AskReply::Read { id, .. } | AskReply::Write { id, .. } = reply {
                            unanswered.push(id.clone());
                        }
                    }
                }
                for id in unanswered {
                    self.doctor_reply(id, Err("The user stopped the agent.".into()));
                }
            }
        }
        self.schedule_render();
    }

    /// Runs one of the panel's slash commands. False for the agent's own,
    /// which go to the agent as they are.
    fn doctor_command(&mut self, line: &str) -> bool {
        let line = line.trim().trim_start_matches('/');
        let (name, arg) = line.split_once(char::is_whitespace).map_or((line, ""), |(n, a)| (n, a.trim()));
        let note = |state: &mut Self, text: String| state.doctor.messages.push(Message::Note(text));
        match name {
            "help" => {
                let mut text = String::from("Commands:");
                for (name, hint, what) in COMMANDS {
                    text += &format!("\n/{name} {hint}  {what}");
                }
                if !self.doctor.commands.is_empty() {
                    text += "\n\nFrom the agent:";
                    for c in &self.doctor.commands {
                        text += &format!("\n/{}  {}", c.name, c.description);
                    }
                }
                text += "\n\nTab fills in a command, Enter runs it. Ctrl+V pastes text or a picture.";
                self.doctor.messages.push(Message::Info(text));
            }
            "clear" => self.clear_doctor(),
            "model" => self.model_command(arg),
            "mode" => match Mode::parse(&arg.to_lowercase()) {
                Some(mode) => {
                    self.set_doctor_mode(mode);
                    note(self, format!("Mode: {}. {}.", mode.short(), mode.description()));
                }
                None if arg.is_empty() => {
                    let mode = self.doctor.mode;
                    note(
                        self,
                        format!(
                            "Mode: {}. {}. Change it with /mode ask, config or yolo.",
                            mode.short(),
                            mode.description()
                        ),
                    );
                }
                None => note(self, format!("There is no mode {arg}: use ask, config or yolo.")),
            },
            "agent" if arg.is_empty() => self.doctor_hit(Hit::OpenPicker),
            "agent" => {
                let wanted = arg.to_lowercase();
                let found = self.doctor.registry.as_ref().and_then(|r| r.as_ref().ok()).and_then(|agents| {
                    agents.iter().find(|a| a.id == wanted || a.name.to_lowercase() == wanted).map(|a| a.id.clone())
                });
                match found {
                    Some(id) => self.pick_agent(id),
                    None => note(self, format!("There is no agent {arg} in the ACP Registry. /agent lists them.")),
                }
            }
            "image" if arg.is_empty() => note(self, "Which picture? /image ~/Pictures/screenshot.png".into()),
            "image" => {
                let path = match arg.strip_prefix("~/") {
                    Some(rest) => PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(rest),
                    None => PathBuf::from(arg),
                };
                match std::fs::read(&path) {
                    Ok(bytes) => self.doctor_attach(bytes.into()),
                    Err(error) => note(self, format!("{}: {error}", path.display())),
                }
            }
            "restart" => match &self.doctor.session {
                Some(session) => session.send(Request::Retry),
                None => self.ensure_session(),
            },
            _ if self.doctor.commands.iter().any(|c| c.name == name) => return false,
            _ => note(self, format!("There is no command /{name}. /help lists them.")),
        }
        self.doctor.scroll = 0.0;
        self.schedule_render();
        true
    }

    /// `/clear`: a new conversation with the same agent.
    fn clear_doctor(&mut self) {
        if self.doctor.busy {
            self.doctor_hit(Hit::Stop);
        }
        self.doctor.messages.clear();
        self.doctor.approved.clear();
        self.doctor.waiting = None;
        self.doctor.busy = false;
        if let Some(session) = &self.doctor.session {
            session.send(Request::NewSession);
            self.doctor.restore_model = true;
        }
    }

    /// `/model [name]`: the menu, or straight to a model by id or name.
    fn model_command(&mut self, arg: &str) {
        let Some(option) = self.doctor.options.iter().find(|o| o.is_model()).cloned() else {
            let text = if self.doctor.session.is_some() {
                "This agent offers no choice of model here."
            } else {
                "The agent is not running yet: ask it something first."
            };
            self.doctor.messages.push(Message::Note(text.into()));
            return;
        };
        if arg.is_empty() {
            self.doctor.menu = None;
            return self.doctor_hit(Hit::OpenMenu(option.id));
        }
        let wanted = arg.to_lowercase();
        let found = option
            .values
            .iter()
            .find(|v| v.value.to_lowercase() == wanted || v.name.to_lowercase() == wanted)
            .or_else(|| option.values.iter().find(|v| v.name.to_lowercase().contains(&wanted)));
        match found {
            Some(value) => {
                self.doctor.messages.push(Message::Note(format!("Model: {}", value.name)));
                self.doctor_hit(Hit::SetOption(option.id.clone(), value.value.clone()));
            }
            None => {
                let names: Vec<&str> = option.values.iter().map(|v| v.name.as_str()).collect();
                let text = format!("There is no model {arg}. Choose from: {}.", names.join(", "));
                self.doctor.messages.push(Message::Note(text));
            }
        }
    }

    /// One secret typed for a login; the login goes ahead once all are in.
    fn secret_entered(&mut self, value: String) {
        let Some(entry) = &mut self.doctor.secret else { return };
        entry.values.push(value);
        if entry.values.len() < entry.labels.len() {
            return;
        }
        let Some(entry) = self.doctor.secret.take() else { return };
        if let Some(session) = &self.doctor.session {
            session.send(Request::Login { method: entry.method, secrets: entry.values });
        }
    }

    fn toolbar_style(&self) -> Style {
        Style::new(self.theme.font_size * 0.95, self.theme.text)
    }

    /// The model setting and its button at the right of the toolbar, when
    /// the agent offers a choice.
    fn model_button(&self, frame: &Frame) -> Option<(&SessionOption, Rect)> {
        let option = self.doctor.options.iter().find(|o| o.is_model())?;
        let label = model_label(option);
        let w = (self.measure_text(&label, self.toolbar_style(), frame.model.w).0 + 40.0).min(frame.model.w);
        Some((option, Rect::new(frame.model.right() - w, frame.model.y, w, frame.model.h)))
    }

    /// The open menu's values, each with whether it is the current one.
    fn menu_values(&self) -> Vec<(tileception_acp::OptionValue, bool)> {
        let Some(id) = &self.doctor.menu else { return Vec::new() };
        let Some(option) = self.doctor.options.iter().find(|o| &o.id == id) else { return Vec::new() };
        option.values.iter().map(|v| (v.clone(), v.value == option.current)).collect()
    }

    fn choose_menu_value(&mut self, index: usize) {
        let (Some(id), Some((value, _))) = (self.doctor.menu.clone(), self.menu_values().get(index).cloned()) else {
            return;
        };
        self.doctor_hit(Hit::SetOption(id, value.value));
    }

    /// Slash commands starting with what is typed: the panel's, then the agent's.
    fn suggestions(&self) -> Vec<Suggestion> {
        let text = &self.doctor.input.text;
        if self.doctor.secret.is_some() || !text.starts_with('/') || text.contains(char::is_whitespace) {
            return Vec::new();
        }
        let typed = text[1..].to_lowercase();
        let ours = COMMANDS.iter().map(|(name, hint, description)| Suggestion {
            name: name.to_string(),
            hint: hint.to_string(),
            description: description.to_string(),
        });
        let theirs = self
            .doctor
            .commands
            .iter()
            .filter(|c| !COMMANDS.iter().any(|(n, ..)| *n == c.name))
            .map(|c| Suggestion { name: c.name.clone(), hint: String::new(), description: c.description.clone() });
        ours.chain(theirs).filter(|c| c.name.to_lowercase().starts_with(&typed)).collect()
    }

    /// Takes a suggestion: runs it, or with `run` false fills it in to add
    /// an argument.
    fn complete_suggestion(&mut self, index: usize, run: bool) {
        let Some(suggestion) = self.suggestions().get(index).cloned() else { return };
        let line = format!("/{}", suggestion.name);
        self.doctor.input.clear();
        self.doctor.menu_selected = 0;
        if run {
            self.send_to_doctor(line);
        } else {
            self.doctor.input.insert(&format!("{line} "));
        }
        self.schedule_render();
    }

    /// The open menu (a setting's values, or slash commands while typing
    /// one): its outline and rows, scrolled to keep the highlighted row.
    fn menu_layout(&self, frame: &Frame) -> Option<(Rect, Vec<MenuRow>)> {
        let selected = self.doctor.menu_selected;
        let row = |label: String, detail: String, current: bool, hit: Hit| MenuRow {
            rect: Rect::new(0.0, 0.0, 0.0, 0.0),
            label,
            detail,
            current,
            selected: false,
            hit,
        };
        let (mut rows, anchor, w): (Vec<MenuRow>, Rect, f64) = match &self.doctor.menu {
            Some(id) => {
                let (_, button) = self.model_button(frame)?;
                let rows = self
                    .menu_values()
                    .into_iter()
                    .map(|(v, current)| {
                        row(v.name, v.description.unwrap_or_default(), current, Hit::SetOption(id.clone(), v.value))
                    })
                    .collect();
                // Over the field, lined up with the button's right edge.
                let anchor = Rect::new(button.x, frame.field.y, button.w, button.h);
                (rows, anchor, 320.0_f64.min(frame.field.w))
            }
            None => {
                let rows = self
                    .suggestions()
                    .into_iter()
                    .enumerate()
                    .map(|(i, s)| {
                        let label = format!("/{} {}", s.name, s.hint).trim_end().to_string();
                        row(label, s.description, false, Hit::Suggestion(i))
                    })
                    .collect();
                (rows, frame.field, frame.field.w)
            }
        };
        if rows.is_empty() {
            return None;
        }
        let room = ((anchor.y - 10.0 - frame.messages.y) / MENU_ROW_H).floor().max(1.0) as usize;
        let limit = if self.doctor.menu.is_some() { usize::MAX } else { MENU_ROWS };
        let shown = rows.len().min(room).min(limit);
        let first = selected.saturating_sub(shown - 1).min(rows.len() - shown);
        let h = shown as f64 * MENU_ROW_H + 8.0;
        let x = (anchor.right() - w).max(frame.field.x);
        let outline = Rect::new(x, anchor.y - 6.0 - h, w, h);
        for (i, row) in rows.iter_mut().enumerate() {
            let place = i as f64 - first as f64;
            row.rect = Rect::new(x + 4.0, outline.y + 4.0 + place * MENU_ROW_H, w - 8.0, MENU_ROW_H);
            row.selected = i == selected;
        }
        let rows = rows.into_iter().skip(first).take(shown).collect();
        Some((outline, rows))
    }

    fn menu_hit(&self, p: Point) -> Option<Hit> {
        let (_, rows) = self.menu_layout(&self.doctor_frame())?;
        rows.into_iter().find(|r| r.rect.contains(p)).map(|r| r.hit)
    }

    /// A login, a key being typed or a login in progress is on screen.
    fn login_showing(&self) -> bool {
        self.doctor.secret.is_some() || matches!(self.doctor.status, Status::NeedsLogin(_) | Status::LoggingIn { .. })
    }

    fn doctor_frame(&self) -> Frame {
        let screen = self.output_rect();
        let w = screen.w.clamp(1.0, PANEL_W);
        let panel = Rect::new(screen.x, screen.y, w, screen.h.max(1.0));
        let hints = Rect::new(panel.x, panel.bottom() - HINTS_H, panel.w, HINTS_H);
        let toolbar = Rect::new(panel.x + PAD, hints.y - TOOLBAR_H, panel.w - 2.0 * PAD, TOOLBAR_H);
        let field = Rect::new(panel.x + PAD, toolbar.y - INPUT_H, panel.w - 2.0 * PAD, INPUT_H);
        let send = Rect::new(field.right() - 40.0, field.y + 6.0, 34.0, field.h - 12.0);
        let text = Rect::new(field.x + 14.0, field.y, send.x - field.x - 20.0, field.h);
        let top = panel.y + HEADER_H + 8.0;
        let size = f64::from(THUMB);
        let attached: Vec<Rect> = (0..self.doctor.attachments.len())
            .map(|i| Rect::new(field.x + i as f64 * (size + 8.0), field.y - size - 10.0, size, size))
            .collect();
        let bottom = attached.first().map_or(field.y, |r| r.y);
        // The mode switch at the left of the toolbar, a segment per mode.
        let style = self.toolbar_style();
        let mut x = toolbar.x;
        let modes = Mode::ALL.map(|mode| {
            let w = self.measure_text(mode.short(), style.bold(), 200.0).0 + 24.0;
            let r = Rect::new(x, toolbar.y + 6.0, w, toolbar.h - 12.0);
            x += w;
            r
        });
        let model_x = x + 12.0;
        Frame {
            panel,
            title: Rect::new(panel.x + 18.0, panel.y + 12.0, 90.0, 32.0),
            agent: Rect::new(panel.x + 110.0, panel.y + 10.0, panel.w - 122.0, 36.0),
            messages: Rect::new(panel.x + PAD, top, panel.w - 2.0 * PAD, bottom - 12.0 - top),
            field,
            text,
            send,
            modes,
            model: Rect::new(model_x, toolbar.y + 4.0, toolbar.right() - model_x, toolbar.h - 8.0),
            hints,
            attached,
        }
    }

    fn measure_text(&self, text: &str, style: Style, width: f64) -> (f64, f64) {
        let t = &self.theme;
        self.text
            .borrow_mut()
            .styled(text, &t.font, style.size, style.colour, width.max(1.0) as f32, style.weight, style.lines)
            .map(|i| (i.width as f64, i.height as f64))
            .unwrap_or((0.0, 0.0))
    }

    fn body_style(&self) -> Style {
        let style = Style::new(self.theme.font_size * 1.08, self.theme.text);
        style.lines(self.fitting_lines(style, 20.0))
    }

    fn note_style(&self) -> Style {
        let style = Style::new(self.theme.font_size * 0.95, self.theme.text.alpha(0.7)).align(Align::Centre);
        style.lines(self.fitting_lines(style, 12.0))
    }

    /// How many lines of `style` fit in the conversation with `padding`
    /// around them: only whole blocks are drawn, so a taller message would
    /// never show at all. Longer ones are cut short instead.
    fn fitting_lines(&self, style: Style, padding: f64) -> u8 {
        let (_, line) = self.measure_text("Ag", style.lines(1), 1000.0);
        if line <= 0.0 {
            return MAX_LINES;
        }
        let room = self.doctor_frame().messages.h - padding;
        (room / line).floor().clamp(1.0, f64::from(MAX_LINES)) as u8
    }

    /// The middle of the panel, laid out unscrolled from the top.
    fn doctor_content(&self, area: Rect) -> Vec<Block> {
        let mut blocks = Vec::new();
        let mut y = area.y;
        let w = area.w;
        let x = area.x;
        let push_note = |blocks: &mut Vec<Block>, y: &mut f64, text: &str| {
            let (_, h) = self.measure_text(text, self.note_style(), w - 24.0);
            blocks.push(Block::Note { rect: Rect::new(x + 12.0, *y, w - 24.0, h + 12.0), text: text.into() });
            *y += h + 12.0 + GAP;
        };
        if self.doctor.view == View::Picker {
            blocks.push(Block::Heading { rect: Rect::new(x + 4.0, y, w, 24.0), text: "CHOOSE AN AGENT".into() });
            y += 28.0;
            match &self.doctor.registry {
                None => push_note(&mut blocks, &mut y, "Loading the ACP Registry\u{2026}"),
                Some(Err(error)) => {
                    push_note(&mut blocks, &mut y, error);
                    blocks.push(Block::Button {
                        rect: Rect::new(x + w / 2.0 - 60.0, y, 120.0, BUTTON_H),
                        label: "Try again".into(),
                        hit: Hit::ReloadRegistry,
                        primary: true,
                    });
                }
                Some(Ok(agents)) => {
                    for agent in agents {
                        let missing = self.doctor.missing.get(&agent.id).cloned();
                        let detail = if agent.description.is_empty() {
                            agent.kind().to_string()
                        } else {
                            agent.description.clone()
                        };
                        blocks.push(Block::Row {
                            rect: Rect::new(x, y, w, ROW_H),
                            title: agent.name.clone(),
                            detail,
                            note: missing.clone(),
                            hit: missing.is_none().then(|| Hit::Pick(agent.id.clone())),
                            current: self.doctor.agent.as_ref() == Some(&agent.id),
                        });
                        y += ROW_H + 4.0;
                    }
                    push_note(&mut blocks, &mut y, "From the official ACP Registry. Agents bring their own login.");
                }
            }
            return blocks;
        }

        let back = |blocks: &mut Vec<Block>, y: &mut f64, logged_in: bool| {
            let mut bx = x + w / 2.0 - if logged_in { 140.0 } else { 60.0 };
            if logged_in {
                blocks.push(Block::Button {
                    rect: Rect::new(bx, *y, 160.0, BUTTON_H),
                    label: "I've logged in".into(),
                    hit: Hit::LoggedIn,
                    primary: true,
                });
                bx += 168.0;
            }
            blocks.push(Block::Button {
                rect: Rect::new(bx, *y, 112.0, BUTTON_H),
                label: "Back".into(),
                hit: Hit::BackToLogins,
                primary: false,
            });
            *y += BUTTON_H + 2.0 * GAP;
        };
        if let Some(secret) = &self.doctor.secret {
            let label = secret.labels.get(secret.values.len()).map_or("key", String::as_str);
            blocks.push(Block::Heading { rect: Rect::new(x + 4.0, y, w, 24.0), text: "LOG IN".into() });
            y += 28.0;
            let text = format!("Paste the {label} into the field below and press Enter. Tileception does not save it.");
            push_note(&mut blocks, &mut y, &text);
            back(&mut blocks, &mut y, false);
        } else if let Status::LoggingIn { terminal } = self.doctor.status {
            blocks.push(Block::Heading { rect: Rect::new(x + 4.0, y, w, 24.0), text: "LOGGING IN".into() });
            y += 28.0;
            let text = if terminal {
                "Finish logging in in the terminal that opened, then choose \"I've logged in\"."
            } else {
                "Finish logging in where the agent sent you, usually your browser."
            };
            push_note(&mut blocks, &mut y, text);
            back(&mut blocks, &mut y, terminal);
        } else if let Status::NeedsLogin(logins) = &self.doctor.status {
            blocks.push(Block::Heading { rect: Rect::new(x + 4.0, y, w, 24.0), text: "LOG IN".into() });
            y += 28.0;
            for Login { id, name, description, how } in logins {
                let detail = description.clone().filter(|d| !d.trim().is_empty()).unwrap_or_else(|| {
                    match how {
                        LoginHow::Terminal(_) => "Opens a terminal",
                        LoginHow::Key | LoginHow::Env(_) => "Asks for an API key",
                        LoginHow::Agent => "The agent opens its own login",
                    }
                    .into()
                });
                blocks.push(Block::Row {
                    rect: Rect::new(x, y, w, ROW_H),
                    title: name.clone(),
                    detail,
                    note: None,
                    hit: Some(Hit::Login(id.clone())),
                    current: false,
                });
                y += ROW_H + 4.0;
            }
            if logins.iter().any(|l| matches!(l.how, LoginHow::Terminal(_))) {
                blocks.push(Block::Button {
                    rect: Rect::new(x + w / 2.0 - 80.0, y + 4.0, 160.0, BUTTON_H),
                    label: "I've logged in".into(),
                    hit: Hit::LoggedIn,
                    primary: false,
                });
                y += BUTTON_H + 4.0 + GAP;
            }
            y += GAP;
        }

        if self.doctor.messages.is_empty() && !self.login_showing() {
            for (i, example) in EXAMPLES.iter().enumerate() {
                blocks.push(Block::Row {
                    rect: Rect::new(x + 16.0, y + 150.0 + i as f64 * 40.0, w - 32.0, 32.0),
                    title: (*example).into(),
                    detail: String::new(),
                    note: None,
                    hit: Some(Hit::Example(i)),
                    current: false,
                });
            }
            return blocks;
        }

        for (index, m) in self.doctor.messages.iter().enumerate() {
            match m {
                Message::User(text) => {
                    let (tw, th) = self.measure_text(text, self.body_style(), w - 64.0);
                    let bw = tw + 28.0;
                    blocks.push(Block::Bubble {
                        rect: Rect::new(x + w - bw, y, bw, th + 20.0),
                        text: text.clone(),
                        user: true,
                    });
                    y += th + 20.0 + GAP;
                }
                Message::Attached(thumbs) => {
                    let size = f64::from(THUMB);
                    let tw = thumbs.len() as f64 * (size + 6.0) - 6.0;
                    blocks.push(Block::Thumbs { rect: Rect::new(x + w - tw, y, tw, size), thumbs: thumbs.clone() });
                    y += size + 6.0;
                }
                Message::Agent(text) => y = self.layout_answer(&mut blocks, text, Rect::new(x, y, w, 0.0)) + GAP,
                Message::Note(text) => push_note(&mut blocks, &mut y, text),
                Message::Info(text) => {
                    let (_, th) = self.measure_text(text, self.body_style(), w - 28.0);
                    blocks.push(Block::Bubble { rect: Rect::new(x, y, w, th + 20.0), text: text.clone(), user: false });
                    y += th + 20.0 + GAP;
                }
                Message::Tool { title, status, .. } => {
                    blocks.push(Block::Tool {
                        rect: Rect::new(x, y, w, 30.0),
                        title: title.clone(),
                        status: status.clone(),
                    });
                    y += 30.0 + 4.0;
                }
                Message::Changed { path, added, removed, undone, .. } => {
                    let name = path
                        .file_name()
                        .map_or_else(|| path.display().to_string(), |n| n.to_string_lossy().into_owned());
                    let text = if *undone {
                        format!("Changed {name}: undone")
                    } else {
                        format!("Changed {name} (+{added} \u{2212}{removed}), with a backup")
                    };
                    blocks.push(Block::Tool {
                        rect: Rect::new(x, y, w - 84.0, 30.0),
                        title: text,
                        status: "completed".into(),
                    });
                    if !*undone {
                        blocks.push(Block::Button {
                            rect: Rect::new(x + w - 78.0, y, 78.0, 30.0),
                            label: "Undo".into(),
                            hit: Hit::Undo(index),
                            primary: false,
                        });
                    }
                    y += 30.0 + 4.0;
                }
                Message::Ask { title, choices, diff, answer, .. } => {
                    let style = self.body_style();
                    let (_, th) = self.measure_text(title, style, w - 28.0);
                    blocks.push(Block::Bubble {
                        rect: Rect::new(x, y, w, th + 20.0),
                        text: title.clone(),
                        user: false,
                    });
                    y += th + 20.0 + 6.0;
                    if !diff.is_empty() && answer.is_none() {
                        let shown: Vec<DiffLine> = diff.iter().take(DIFF_LINES).cloned().collect();
                        let h = shown.len() as f64 * DIFF_LINE_H
                            + 12.0
                            + if diff.len() > DIFF_LINES { DIFF_LINE_H } else { 0.0 };
                        blocks.push(Block::Diff { rect: Rect::new(x, y, w, h), lines: diff.clone() });
                        y += h + 6.0;
                    }
                    match answer {
                        Some(answer) => push_note(&mut blocks, &mut y, &format!("You chose: {answer}")),
                        None => {
                            let n = choices.len().max(1) as f64;
                            let bw = (w - 8.0 * (n - 1.0)) / n;
                            for (i, choice) in choices.iter().enumerate() {
                                blocks.push(Block::Button {
                                    rect: Rect::new(x + i as f64 * (bw + 8.0), y, bw, BUTTON_H),
                                    label: choice.name.clone(),
                                    hit: Hit::Answer(index, Some(choice.id.clone())),
                                    primary: choice.kind.starts_with("allow") && i == 0,
                                });
                            }
                            if choices.is_empty() {
                                blocks.push(Block::Button {
                                    rect: Rect::new(x, y, w, BUTTON_H),
                                    label: "Decline".into(),
                                    hit: Hit::Answer(index, None),
                                    primary: false,
                                });
                            }
                            y += BUTTON_H + GAP;
                        }
                    }
                }
            }
        }
        if self.doctor.busy
            && matches!(
                self.doctor.messages.last(),
                Some(Message::User(_) | Message::Attached(_) | Message::Tool { .. })
            )
        {
            blocks.push(Block::Dots { rect: Rect::new(x, y, 64.0, 30.0) });
        }
        blocks
    }

    /// How answers look: `scale` times the body size, wrapped to as many
    /// lines as fit the conversation (only whole blocks are drawn).
    fn rich_style(&self, scale: f32, weight: u16) -> RichStyle {
        let t = &self.theme;
        let size = t.font_size * 1.08 * scale;
        let line = f64::from((size * 1.4).ceil());
        let room = self.doctor_frame().messages.h - 20.0;
        let lines = (room / line).floor().clamp(1.0, f64::from(MAX_LINES)) as u8;
        RichStyle { size, colour: t.text, accent: t.accent, weight, lines, mono: false }
    }

    fn rich_image(&self, spans: &[Span], style: RichStyle, width: f64) -> Option<RichImage> {
        self.text.borrow_mut().rich(spans, &self.theme.font, style, width as f32)
    }

    /// An answer's markdown, laid out from the top of `at` across its
    /// width, a block per paragraph, item, code block or picture. Returns
    /// where it ends.
    fn layout_answer(&self, blocks: &mut Vec<Block>, text: &str, at: Rect) -> f64 {
        let (x, w) = (at.x, at.w);
        let mut y = at.y;
        let body = self.rich_style(1.0, 400);
        let pieces = markdown::parse(text);
        for (i, piece) in pieces.iter().enumerate() {
            let after_item = i > 0 && matches!(pieces[i - 1], Piece::Text { kind: Kind::Item { .. }, .. });
            match piece {
                Piece::Text { kind, spans } => {
                    let (style, indent, marker, quote) = match kind {
                        Kind::Heading(level) => {
                            if i > 0 {
                                y += 4.0;
                            }
                            let scale = match level {
                                1 => 1.3,
                                2 => 1.18,
                                _ => 1.06,
                            };
                            (self.rich_style(scale, 600), 0.0, None, false)
                        }
                        Kind::Item { depth, marker } => {
                            if after_item {
                                y -= PIECE_GAP / 2.0;
                            }
                            (body, 2.0 + f64::from(*depth) * 18.0, Some(marker.as_str()), false)
                        }
                        Kind::Quote => (RichStyle { colour: self.theme.text.alpha(0.75), ..body }, 14.0, None, true),
                        Kind::Paragraph => (body, 0.0, None, false),
                    };
                    let gutter = if marker.is_some() { 20.0 } else { 0.0 };
                    let rect = Rect::new(x + 2.0 + indent + gutter, y, w - 2.0 - indent - gutter, 0.0);
                    let Some(image) = self.rich_image(spans, style, rect.w) else { continue };
                    let h = f64::from(image.image.height);
                    let line = f64::from((style.size * 1.4).ceil());
                    let marker = marker
                        .filter(|m| !m.is_empty())
                        .map(|m| (Rect::new(x + 2.0 + indent, y, gutter, line), m.to_string()));
                    blocks.push(Block::Rich { rect: Rect { h, ..rect }, spans: spans.clone(), style, marker, quote });
                    y += h + PIECE_GAP;
                }
                Piece::Code(code) => {
                    let style = RichStyle { size: body.size * 0.9, mono: true, ..body };
                    let spans = vec![Span::plain(code.clone())];
                    let Some(image) = self.rich_image(&spans, style, w - 24.0) else { continue };
                    let h = f64::from(image.image.height) + 16.0;
                    blocks.push(Block::Code { rect: Rect::new(x, y, w, h), spans, style });
                    y += h + PIECE_GAP;
                }
                Piece::Image { url, alt, link } => {
                    let open = link.clone().unwrap_or_else(|| url.clone());
                    match self.doctor.pictures.get(url) {
                        Some(Picture::Failed) => {
                            let name = if alt.trim().is_empty() { url.clone() } else { alt.clone() };
                            let spans = vec![Span::plain("Picture: "), Span { link: Some(open), ..Span::plain(name) }];
                            let Some(image) = self.rich_image(&spans, body, w - 2.0) else { continue };
                            let h = f64::from(image.image.height);
                            let rect = Rect::new(x + 2.0, y, w - 2.0, h);
                            blocks.push(Block::Rich { rect, spans, style: body, marker: None, quote: false });
                            y += h + PIECE_GAP;
                        }
                        picture => {
                            let (pw, ph) = match picture {
                                Some(Picture::Ready(thumb)) => {
                                    let (tw, th) = (f64::from(thumb.size.0), f64::from(thumb.size.1));
                                    let scale = (w / tw).min(1.0);
                                    (tw * scale, th * scale)
                                }
                                _ => (w, 120.0),
                            };
                            blocks.push(Block::Picture { rect: Rect::new(x, y, pw, ph), url: url.clone(), open });
                            y += ph + PIECE_GAP;
                        }
                    }
                }
                Piece::Rule => {
                    blocks.push(Block::Rule { rect: Rect::new(x, y + 4.0, w, 1.0) });
                    y += 9.0 + PIECE_GAP;
                }
            }
        }
        if y > at.y { y - PIECE_GAP } else { y }
    }

    fn doctor_content_height(&self, area: Rect) -> f64 {
        let blocks = self.doctor_content(area);
        blocks.iter().map(|b| b.rect().bottom()).fold(area.y, f64::max) - area.y
    }

    /// The middle, scrolled into place: the picker from the top, the
    /// conversation stuck to its newest message. Only whole blocks show.
    fn doctor_blocks(&self, area: Rect) -> Vec<Block> {
        let mut blocks = self.doctor_content(area);
        let height = blocks.iter().map(|b| b.rect().bottom()).fold(area.y, f64::max) - area.y;
        let shift = match self.doctor.view {
            View::Picker => -self.doctor.scroll,
            // Taller than the panel: the newest message sits at the bottom.
            View::Conversation if !self.doctor.messages.is_empty() => (area.h - height).min(0.0) + self.doctor.scroll,
            View::Conversation => 0.0,
        };
        for b in &mut blocks {
            b.rect_mut().y += shift;
        }
        blocks.retain(|b| {
            let r = b.rect();
            r.y >= area.y - 0.5 && r.bottom() <= area.bottom() + 0.5
        });
        blocks
    }

    fn status_line(&self) -> (String, Colour) {
        let t = &self.theme;
        let name = self.chosen_agent().map(|a| a.name).or_else(|| self.doctor.agent.clone());
        match (&name, &self.doctor.status) {
            (None, _) => ("Choose an agent".into(), t.muted),
            (Some(name), _) if self.doctor.session.is_none() => (name.clone(), t.muted),
            (Some(name), Status::Ready) => (name.clone(), t.accent),
            (Some(name), Status::NeedsLogin(_) | Status::LoggingIn { .. }) => {
                (format!("{name} \u{b7} log in"), t.warning)
            }
            (Some(_), Status::Starting(text)) => (text.clone(), t.warning),
            (Some(name), Status::Failed(_)) => (format!("{name} \u{b7} stopped"), t.danger),
        }
    }

    pub fn doctor_elements(&self, renderer: &mut GlesRenderer) -> Vec<OutputElement> {
        if !self.doctor.open {
            return Vec::new();
        }
        let t = &self.theme;
        let frame = self.doctor_frame();
        let pointer = Point::new(self.pointer.x, self.pointer.y);
        let mut p = Painter::new(self, renderer);
        // Slides in from the left edge.
        let shown = ease_out(self.doctor.opened.elapsed().as_secs_f64() / 0.2);
        p.alpha = shown as f32;
        p.offset = ((shown - 1.0) * 24.0, 0.0);
        p.edge_panel(frame.panel, true);

        p.text("Doctor", frame.title, Style::new(t.font_size * 1.3, t.text).bold());
        // The agent, as a button that opens the picker.
        let (label, dot) = self.status_line();
        let style = Style::new(t.font_size, t.text);
        let (lw, _) = p.measure(&label, frame.agent.w - 48.0, style);
        let button = Rect::new(frame.agent.right() - lw - 46.0, frame.agent.y, lw + 46.0, frame.agent.h);
        if button.contains(pointer) {
            p.fill(button, t.surface_raised, t.radius);
        }
        p.fill(Rect::new(button.x + 12.0, button.y + button.h / 2.0 - 4.0, 8.0, 8.0), dot, 4.0);
        p.text(&label, Rect::new(button.x + 26.0, button.y, lw + 1.0, button.h), style);
        let chevron = if self.doctor.view == View::Picker { glyph::CHEVRON_DOWN } else { glyph::CHEVRON_RIGHT };
        p.glyph(chevron, Rect::new(button.right() - 20.0, button.y, 16.0, button.h), 12, t.muted);
        p.fill(Rect::new(frame.panel.x + 12.0, frame.panel.y + HEADER_H, frame.panel.w - 24.0, 1.0), t.border, 0.0);

        let area = frame.messages;
        let empty = self.doctor.view == View::Conversation && self.doctor.messages.is_empty() && !self.login_showing();
        if empty {
            let mid = area.y + 40.0;
            p.fill(Rect::new(area.x + area.w / 2.0 - 32.0, mid, 64.0, 64.0), t.surface_raised, 32.0);
            p.glyph(glyph::CHAT, Rect::new(area.x, mid, area.w, 64.0), 28, t.accent);
            p.text(
                "Ask about your setup",
                Rect::new(area.x, mid + 76.0, area.w, 24.0),
                Style::new(t.font_size * 1.1, t.text).bold().align(Align::Centre),
            );
        }
        for block in self.doctor_blocks(area) {
            self.draw_block(&mut p, &block, pointer);
        }
        self.draw_toolbar(&mut p, &frame, pointer);
        // Pictures waiting to go with the next message; a click takes one off.
        for (attachment, r) in self.doctor.attachments.iter().zip(&frame.attached) {
            p.image_sized(&attachment.thumb.buffer, (r.x, r.y), Some((THUMB, THUMB)));
            let hover = r.contains(pointer);
            let border = if hover { t.accent.alpha(0.6) } else { t.border };
            p.shape(*r, Shape { border_width: 1.0, border, ..Shape::filled(Colour::TRANSPARENT, 2.0) });
            if hover {
                let x = Rect::new(r.right() - 22.0, r.y + 2.0, 20.0, 20.0);
                p.fill(x, Colour([0.0, 0.0, 0.0, 0.6]), 10.0);
                p.glyph(glyph::CLOSE, x, 10, t.text);
            }
        }

        // Where typing goes, glowing while it has the keyboard.
        let focused = self.doctor.focused;
        p.shape(
            frame.field,
            Shape {
                border_width: 1.0,
                border: if focused { t.accent.alpha(0.45) } else { t.border },
                glow_radius: if focused { t.glow * 0.5 } else { 0.0 },
                glow: t.accent.alpha(0.10),
                ..Shape::filled(t.background, t.radius)
            },
        );
        let input = &self.doctor.input;
        let size = t.font_size * 1.08;
        let secret = self.doctor.secret.as_ref().and_then(|s| s.labels.get(s.values.len()));
        if input.text.is_empty() {
            let hint = match secret {
                Some(label) => format!("Paste the {label}"),
                None if self.doctor.busy => "Answering...".into(),
                None => "Ask about your setup, or / for commands".into(),
            };
            p.text(&hint, frame.text, Style::new(size, t.muted));
        }
        // A key shows as stars, one per byte so the cursor stays in place.
        let shown = if secret.is_some() { "*".repeat(input.text.len()) } else { input.text.clone() };
        let field = self.text.borrow_mut().field(
            &shown,
            &t.font,
            size,
            t.text,
            frame.text.w as f32,
            crate::text::Field {
                cursor: input.cursor,
                anchor: input.anchor,
                visible: focused && self.doctor.edited.elapsed().as_millis() % 1000 < 600,
                accent: t.accent.straight_rgba8(),
            },
        );
        if let Some(image) = field {
            p.image(&image.buffer, (frame.text.x, frame.text.y + (frame.text.h - image.height as f64) / 2.0));
        }
        if self.doctor.busy {
            // While answering, the button stops it.
            p.fill(frame.send, t.surface_raised, t.radius);
            p.glyph(glyph::STOP, frame.send, 14, t.text);
        } else {
            let can_send = !input.text.trim().is_empty() || !self.doctor.attachments.is_empty();
            if can_send {
                p.shape(frame.send, Shape { radius: t.radius, fill: (t.accent, t.accent_end), ..Default::default() });
            }
            p.glyph(glyph::ARROW_RIGHT, frame.send, 16, if can_send { t.on_accent } else { t.muted.alpha(0.6) });
        }

        let hints = frame.hints;
        p.fill(Rect::new(hints.x + 1.0, hints.y, hints.w - 2.0, 1.0), t.border, 0.0);
        // Over the mode switch, what the mode means instead of keys.
        if let Some(i) = frame.modes.iter().position(|r| r.contains(pointer)) {
            let text = Rect::new(hints.x + 16.0, hints.y, hints.w - 32.0, hints.h);
            p.text(Mode::ALL[i].description(), text, Style::new(t.font_size * 0.9, t.muted));
        } else {
            let keys: &[(&str, &str)] = if self.doctor.menu.is_some() {
                &[("\u{2191}\u{2193}", "Choose"), ("Enter", "Pick"), ("Esc", "Close")]
            } else if !self.suggestions().is_empty() {
                &[("\u{2191}\u{2193}", "Choose"), ("Tab", "Fill in"), ("Enter", "Run")]
            } else if self.doctor.secret.is_some() {
                &[("Ctrl+V", "Paste"), ("Enter", "Log in"), ("Esc", "Cancel")]
            } else if self.doctor.busy {
                &[("Ctrl+C", "Stop"), ("PgUp", "Earlier"), ("Esc", "Close")]
            } else {
                &[("Enter", "Send"), ("/", "Commands"), ("PgUp", "Earlier"), ("Esc", "Close")]
            };
            p.key_hints(keys, hints.x + 16.0, hints.y + hints.h / 2.0, false);
        }
        // Menus lie over everything else.
        if let Some((outline, rows)) = self.menu_layout(&frame) {
            p.popup(outline);
            for row in rows {
                let hover = row.rect.contains(pointer);
                if row.selected {
                    p.fill(row.rect, t.accent.alpha(0.14), t.radius);
                } else if hover {
                    p.fill(row.rect, t.surface_raised, t.radius);
                }
                let colour = if row.current { t.accent } else { t.text };
                let label_w = p.measure(&row.label, row.rect.w * 0.6, Style::new(t.font_size, colour).bold()).0;
                p.text(
                    &row.label,
                    Rect::new(row.rect.x + 10.0, row.rect.y, label_w + 1.0, row.rect.h),
                    Style::new(t.font_size, colour).bold(),
                );
                let detail_x = row.rect.x + 22.0 + label_w;
                let detail = Rect::new(detail_x, row.rect.y, row.rect.right() - 10.0 - detail_x, row.rect.h);
                p.text(&row.detail, detail, Style::new(t.font_size * 0.88, t.muted).lines(1));
                if row.current {
                    p.glyph(
                        glyph::CHECK,
                        Rect::new(row.rect.right() - 26.0, row.rect.y, 20.0, row.rect.h),
                        12,
                        t.accent,
                    );
                }
            }
        }
        p.finish()
    }

    /// Under the field: the permission mode as a switch, and the model.
    fn draw_toolbar(&self, p: &mut Painter, frame: &Frame, pointer: Point) {
        let t = &self.theme;
        let style = self.toolbar_style();
        let whole =
            Rect::new(frame.modes[0].x, frame.modes[0].y, frame.modes[2].right() - frame.modes[0].x, frame.modes[0].h);
        p.shape(
            whole,
            Shape { border_width: 1.0, border: t.border, ..Shape::filled(t.surface, (whole.h / 2.0) as f32) },
        );
        for (mode, rect) in Mode::ALL.into_iter().zip(frame.modes) {
            let colour = match mode {
                Mode::Ask => t.text,
                Mode::Config => t.accent,
                Mode::Yolo => t.warning,
            };
            let chosen = mode == self.doctor.mode;
            if chosen {
                p.shape(
                    rect,
                    Shape {
                        border_width: 1.0,
                        border: colour.alpha(0.5),
                        ..Shape::filled(colour.alpha(0.16), (rect.h / 2.0) as f32)
                    },
                );
            } else if rect.contains(pointer) {
                p.fill(rect, t.surface_raised, (rect.h / 2.0) as f32);
            }
            let style = if chosen { style.bold().colour(colour) } else { style.colour(t.muted) };
            p.text(mode.short(), rect, style.align(Align::Centre));
        }
        match self.model_button(frame) {
            Some((option, button)) => {
                let open = self.doctor.menu.as_ref() == Some(&option.id);
                if open || button.contains(pointer) {
                    p.fill(button, t.surface_raised, t.radius);
                }
                let label = model_label(option);
                p.text(&label, Rect::new(button.x + 10.0, button.y, button.w - 34.0, button.h), style.lines(1));
                let chevron = if open { glyph::CHEVRON_DOWN } else { glyph::CHEVRON_UP };
                p.glyph(chevron, Rect::new(button.right() - 24.0, button.y, 16.0, button.h), 12, t.muted);
            }
            // Running, but with no choice of model to offer.
            None if matches!(self.doctor.status, Status::Ready) => {
                let text = "Agent's default model";
                let w = self.measure_text(text, style, frame.model.w).0;
                let r = Rect::new(frame.model.right() - w - 10.0, frame.model.y, w + 1.0, frame.model.h);
                p.text(text, r, style.colour(t.muted));
            }
            None => {}
        }
    }

    fn draw_block(&self, p: &mut Painter, block: &Block, pointer: Point) {
        let t = &self.theme;
        match block {
            Block::Bubble { rect, text, user } => {
                let shape = if *user {
                    Shape {
                        radius: t.popup_radius,
                        fill: (t.accent.alpha(0.22), t.accent_end.alpha(0.16)),
                        ..Default::default()
                    }
                } else {
                    Shape { border_width: 1.0, border: t.border, ..Shape::filled(t.surface_raised, t.popup_radius) }
                };
                p.shape(*rect, shape);
                p.text(text, Rect::new(rect.x + 14.0, rect.y + 10.0, rect.w - 27.0, rect.h - 20.0), self.body_style());
            }
            Block::Note { rect, text } => {
                p.text(text, *rect, self.note_style());
            }
            Block::Tool { rect, title, status } => {
                let (g, colour) = match status.as_str() {
                    "completed" => (glyph::CHECK, t.accent),
                    "failed" => (glyph::CLOSE, t.danger),
                    _ => (glyph::RESTART, t.warning),
                };
                p.fill(*rect, t.surface, t.radius);
                p.glyph(g, Rect::new(rect.x + 4.0, rect.y, 26.0, rect.h), 13, colour);
                p.text(
                    title,
                    Rect::new(rect.x + 32.0, rect.y, rect.w - 40.0, rect.h),
                    Style::new(t.font_size * 0.95, t.text.alpha(0.75)).lines(1),
                );
            }
            Block::Heading { rect, text } => {
                p.text(text, *rect, Style::new(t.font_size * 0.85, t.muted).bold());
            }
            Block::Row { rect, title, detail, note, hit, current } => {
                let hover = hit.is_some() && rect.contains(pointer);
                let fill = if *current {
                    t.accent.alpha(0.14)
                } else if hover {
                    t.surface_raised
                } else {
                    t.surface
                };
                p.shape(
                    *rect,
                    Shape {
                        border_width: 1.0,
                        border: if *current { t.accent.alpha(0.4) } else { t.border },
                        ..Shape::filled(fill, t.radius)
                    },
                );
                let enabled = hit.is_some();
                let colour = if enabled { t.text } else { t.muted };
                if detail.is_empty() {
                    p.text(
                        title,
                        Rect::new(rect.x + 12.0, rect.y, rect.w - 24.0, rect.h),
                        Style::new(t.font_size, if hover { t.text } else { t.text.alpha(0.7) }),
                    );
                    return;
                }
                let note_w = note
                    .as_ref()
                    .map_or(0.0, |n| p.measure(n, 200.0, Style::new(t.font_size * 0.78, t.warning)).0 + 12.0);
                p.text(
                    title,
                    Rect::new(rect.x + 14.0, rect.y + 8.0, rect.w - 28.0 - note_w, 22.0),
                    Style::new(t.font_size, colour).bold(),
                );
                p.text(
                    detail,
                    Rect::new(rect.x + 14.0, rect.y + 29.0, rect.w - 28.0, 18.0),
                    Style::new(t.font_size * 0.9, t.text.alpha(0.65)).lines(1),
                );
                if let Some(note) = note {
                    let style = Style::new(t.font_size * 0.78, t.warning);
                    p.text(note, Rect::new(rect.right() - note_w - 2.0, rect.y + 8.0, note_w, 22.0), style);
                }
            }
            Block::Button { rect, label, primary, .. } => {
                let hover = rect.contains(pointer);
                if *primary {
                    p.shape(*rect, Shape { radius: t.radius, fill: (t.accent, t.accent_end), ..Default::default() });
                } else {
                    let fill = if hover { t.surface_raised } else { t.surface };
                    p.shape(*rect, Shape { border_width: 1.0, border: t.border, ..Shape::filled(fill, t.radius) });
                }
                let colour = if *primary { t.on_accent } else { t.text };
                p.text(label, *rect, Style::new(t.font_size * 0.9, colour).bold().align(Align::Centre));
            }
            Block::Diff { rect, lines } => {
                p.shape(*rect, Shape { border_width: 1.0, border: t.border, ..Shape::filled(t.background, t.radius) });
                let size = t.font_size * 0.82;
                for (i, line) in lines.iter().take(DIFF_LINES).enumerate() {
                    let row =
                        Rect::new(rect.x + 8.0, rect.y + 6.0 + i as f64 * DIFF_LINE_H, rect.w - 16.0, DIFF_LINE_H);
                    let (mark, colour) = match line.sign {
                        Sign::Added => ("+", t.accent),
                        Sign::Removed => ("\u{2212}", t.danger),
                        Sign::Same => (" ", t.muted),
                        Sign::Skipped => ("\u{22ef}", t.muted.alpha(0.6)),
                    };
                    if line.sign == Sign::Added {
                        p.fill(row, t.accent.alpha(0.08), 2.0);
                    } else if line.sign == Sign::Removed {
                        p.fill(row, t.danger.alpha(0.08), 2.0);
                    }
                    p.text(mark, Rect::new(row.x + 2.0, row.y, 12.0, row.h), Style::new(size, colour).bold());
                    p.text(&line.text, Rect::new(row.x + 16.0, row.y, row.w - 18.0, row.h), Style::new(size, colour));
                }
                if lines.len() > DIFF_LINES {
                    let more = format!("and {} more lines", lines.len() - DIFF_LINES);
                    let row = Rect::new(
                        rect.x + 24.0,
                        rect.y + 6.0 + DIFF_LINES as f64 * DIFF_LINE_H,
                        rect.w - 32.0,
                        DIFF_LINE_H,
                    );
                    p.text(&more, row, Style::new(size, t.muted));
                }
            }
            Block::Rich { rect, spans, style, marker, quote } => {
                if let Some((at, marker)) = marker {
                    p.text(marker, *at, Style::new(style.size, t.muted).align(Align::Centre));
                }
                if *quote {
                    p.fill(Rect::new(rect.x - 12.0, rect.y + 2.0, 3.0, rect.h - 4.0), t.accent.alpha(0.5), 1.5);
                }
                if let Some(image) = self.rich_image(spans, *style, rect.w) {
                    let hover = image.links.iter().any(|([x, y, w, h], _)| {
                        Rect::new(rect.x + f64::from(*x), rect.y + f64::from(*y), f64::from(*w), f64::from(*h))
                            .contains(pointer)
                    });
                    if hover {
                        for ([x, y, w, h], _) in &image.links {
                            let r =
                                Rect::new(rect.x + f64::from(*x), rect.y + f64::from(*y), f64::from(*w), f64::from(*h));
                            if r.contains(pointer) {
                                p.fill(r, t.accent.alpha(0.14), 3.0);
                            }
                        }
                    }
                    p.image(&image.image.buffer, (rect.x, rect.y));
                }
            }
            Block::Code { rect, spans, style } => {
                p.shape(*rect, Shape { border_width: 1.0, border: t.border, ..Shape::filled(t.background, t.radius) });
                if let Some(image) = self.rich_image(spans, *style, rect.w - 24.0) {
                    p.image(&image.image.buffer, (rect.x + 12.0, rect.y + 8.0));
                }
            }
            Block::Picture { rect, url, .. } => {
                let hover = rect.contains(pointer);
                match self.doctor.pictures.get(url) {
                    Some(Picture::Ready(thumb)) => {
                        p.image_sized(&thumb.buffer, (rect.x, rect.y), Some((rect.w as i32, rect.h as i32)));
                        let border = if hover { t.accent.alpha(0.6) } else { t.border };
                        p.shape(*rect, Shape { border_width: 1.0, border, ..Shape::filled(Colour::TRANSPARENT, 2.0) });
                    }
                    _ => {
                        p.shape(
                            *rect,
                            Shape { border_width: 1.0, border: t.border, ..Shape::filled(t.surface, t.radius) },
                        );
                        let style = Style::new(t.font_size * 0.9, t.muted).align(Align::Centre);
                        p.text("Loading picture...", *rect, style);
                    }
                }
            }
            Block::Rule { rect } => p.fill(*rect, t.border, 0.0),
            Block::Thumbs { rect, thumbs } => {
                let size = f64::from(THUMB);
                for (i, thumb) in thumbs.iter().enumerate() {
                    let r = Rect::new(rect.x + i as f64 * (size + 6.0), rect.y, size, size);
                    p.image_sized(&thumb.buffer, (r.x, r.y), Some((THUMB, THUMB)));
                    p.shape(
                        r,
                        Shape { border_width: 1.0, border: t.border, ..Shape::filled(Colour::TRANSPARENT, 2.0) },
                    );
                }
            }
            Block::Dots { rect } => {
                let phase = (self.doctor.edited.elapsed().as_secs_f64() * 3.0) as usize % 3;
                p.shape(
                    *rect,
                    Shape { border_width: 1.0, border: t.border, ..Shape::filled(t.surface_raised, t.popup_radius) },
                );
                for i in 0..3 {
                    let colour = if i == phase { t.accent } else { t.muted.alpha(0.6) };
                    p.fill(Rect::new(rect.x + 17.0 + i as f64 * 11.0, rect.y + 12.0, 6.0, 6.0), colour, 3.0);
                }
            }
        }
    }
}

/// For developing the panel: a stand-in agent program instead of the
/// registry's (TILECEPTION_DOCTOR_TEST_AGENT="python3 fake_agent.py"). Only in
/// development builds; released ones run registry agents only.
fn test_agent() -> Option<String> {
    if !cfg!(debug_assertions) {
        return None;
    }
    std::env::var("TILECEPTION_DOCTOR_TEST_AGENT").ok().filter(|c| !c.trim().is_empty())
}

fn state_dir() -> PathBuf {
    std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".local/state"))
        .join("tileception")
}

fn doctor_scope() -> Scope {
    let config = crate::config::path().parent().map(PathBuf::from).unwrap_or_default();
    Scope { config, state: state_dir() }
}

/// Copies of files from before the agent changed them.
fn backups_dir() -> PathBuf {
    state_dir().join("doctor-backups")
}

fn read_for_agent(path: &std::path::Path, line: Option<u64>, limit: Option<u64>) -> Result<serde_json::Value, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(serde_json::json!({ "content": doctor_policy::read_lines(&text, line, limit) }))
}

/// Sent with the first question, so the agent knows what it is helping with.
fn doctor_context(mode: &str) -> String {
    let config = crate::config::path();
    let state = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".local/state"));
    format!(
        "You are the doctor inside Tileception, a zoomable, nesting tiling Wayland compositor. You help the user \
         with Tileception's settings, shortcuts and default apps, in plain language.\n\
         - The configuration is {config} (TOML). Tileception reloads it as soon as it is saved. Its comments \
         document every option; `tileception msg reload` reports errors.\n\
         - `tileception msg <command>` runs a compositor command, for example `tree` (the layout) or `volume`.\n\
         - Every option with its default and an explanation is in {defaults}.\n\
         - The log is {log}.\n\
         - Read and write files with the file tools the client offers, so the user sees diffs and gets backups. \
         The user's permission mode is \"{mode}\".\n\
         Keep changes small, say what you change, and keep the file's comments. Answer briefly.",
        config = config.display(),
        log = state.join("tileception/tileception.log").display(),
        defaults = state.join("tileception/default-config.toml").display(),
        mode = mode,
    )
}

/// Premultiplied, as the renderer wants pictures.
fn premultiplied(image: image::RgbaImage) -> Pixels {
    let (w, h) = image.dimensions();
    let mut raw = image.into_raw();
    for pixel in raw.as_chunks_mut::<4>().0 {
        let alpha = u32::from(pixel[3]);
        for channel in &mut pixel[..3] {
            *channel = (u32::from(*channel) * alpha / 255) as u8;
        }
    }
    (raw, w, h)
}

/// A picture from an answer, downloaded (https only) or read from a file,
/// and scaled down to fit the panel.
fn load_picture(url: &str) -> Option<Pixels> {
    use std::io::Read;
    const MAX: u64 = 32 << 20;
    let local = url.strip_prefix("file://").or_else(|| url.starts_with('/').then_some(url));
    let bytes = if let Some(path) = local {
        let mut bytes = Vec::new();
        std::fs::File::open(path).ok()?.take(MAX).read_to_end(&mut bytes).ok()?;
        bytes
    } else if url.starts_with("https://") {
        use std::hash::{Hash, Hasher};
        let dir = tileception_acp::cache_dir().join("pictures");
        std::fs::create_dir_all(&dir).ok()?;
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        url.hash(&mut hasher);
        let file = dir.join(format!("{:016x}", hasher.finish()));
        if !file.exists() {
            let part = file.with_extension("part");
            if let Err(error) = tileception_acp::curl(url, &part, &MAX.to_string()) {
                tracing::debug!("{url}: {error}");
                return None;
            }
            std::fs::rename(&part, &file).ok()?;
        }
        std::fs::read(&file).ok()?
    } else {
        return None;
    };
    let image = image::load_from_memory(&bytes).ok()?;
    let (w, h) = ((PANEL_W - 2.0 * PAD) as u32, PICTURE_H);
    let image = if image.width() > w || image.height() > h { image.thumbnail(w, h) } else { image };
    Some(premultiplied(image.to_rgba8()))
}

/// A pasted picture as a PNG small enough to send, and its thumbnail.
fn prepare_attachment(bytes: &[u8]) -> Result<(Vec<u8>, Pixels), String> {
    use image::imageops::FilterType;
    let image = image::load_from_memory(bytes).map_err(|e| format!("That picture could not be read: {e}"))?;
    let image = if image.width().max(image.height()) > SEND_SIZE {
        image.resize(SEND_SIZE, SEND_SIZE, FilterType::Triangle)
    } else {
        image
    };
    let mut png = Vec::new();
    image
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .map_err(|e| format!("That picture could not be prepared: {e}"))?;
    let size = THUMB as u32;
    Ok((png, premultiplied(image.resize_to_fill(size, size, FilterType::Triangle).to_rgba8())))
}

/// `text` with the pictures saved where the agent may read them, and
/// where they are, for agents that take no pictures in a prompt.
fn with_saved_pictures(text: String, attachments: &[Attachment]) -> String {
    if attachments.is_empty() {
        return text;
    }
    let dir = state_dir().join("pictures");
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis());
    let mut text = text;
    for (i, attachment) in attachments.iter().enumerate() {
        let path = dir.join(format!("pasted-{stamp}-{i}.png"));
        match std::fs::create_dir_all(&dir).and_then(|_| std::fs::write(&path, &attachment.png)) {
            Ok(()) => text += &format!("\n\n[The user attached a picture: {}]", path.display()),
            Err(error) => tracing::warn!("{}: {error}", path.display()),
        }
    }
    text
}
