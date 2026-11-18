//! The doctor: a panel along the left edge for asking a coding agent about
//! your setup in plain language. It stays beside the desktop (it does not
//! block it), so changes can be watched as they land. It can also pop out
//! into a window of its own, drawn like Settings through a real xdg client;
//! both are laid out and drawn by the same code, in the area given to it.
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
    output::Output,
    reexports::calloop::{RegistrationToken, channel},
    utils::Transform,
};
use tileception_acp::{Agent, AgentCommand, Choice, Event, Login, LoginHow, Request, Session, SessionOption, Status};
use tileception_core::{Point, Rect, WindowId};

use crate::{
    Tileception,
    config::Mods,
    doctor_policy::{self, DiffLine, Mode, Scope, Sign},
    launcher::Paste,
    markdown::{self, Kind, Piece, Span},
    render::OutputElement,
    settings::client::{self, Client},
    shapes::Shape,
    text::{RichImage, RichStyle, TextImage},
    text_input::TextInput,
    theme::Colour,
    ui::{Align, Button, Look, Painter, Style, ease_out, glyph},
};

pub(crate) const PANEL_W: f64 = 480.0;
const HEADER_H: f64 = 56.0;
const INPUT_H: f64 = 46.0;
/// The row under the field: permission mode and model.
const TOOLBAR_H: f64 = 38.0;
const HINTS_H: f64 = 34.0;
/// A row in a menu over the conversation (slash commands, models).
const MENU_ROW_H: f64 = 32.0;
/// Suggestions shown at most while typing a slash command.
const MENU_ROWS: usize = 7;
/// Values shown at once in a setting's menu; more scroll.
const MENU_VALUE_ROWS: usize = 10;
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
/// The bar along the top of a code block: its language and buttons.
const CODE_BAR: f64 = 28.0;
/// A copy or run button in that bar.
const CODE_BUTTON: f64 = 24.0;
/// How long a copied code block shows a check.
const COPIED_FOR: std::time::Duration = std::time::Duration::from_millis(1500);
/// How often opening the panel reads the registry again. It is downloaded
/// again only when its cached copy is a day old.
const REGISTRY_AGAIN: std::time::Duration = std::time::Duration::from_secs(60 * 60);
/// How long a stopped agent may take to end its turn before it is restarted.
const STOP_WAIT: std::time::Duration = std::time::Duration::from_secs(8);

/// A small picture, premultiplied, ready to draw.
#[derive(Debug, Clone)]
pub struct Thumb {
    buffer: MemoryRenderBuffer,
    size: (i32, i32),
}

impl Thumb {
    fn new(rgba: &[u8], (width, height): (u32, u32)) -> Thumb {
        let size = (width as i32, height as i32);
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
        tool_id: String,
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
    /// A newer version of the running agent is in the registry. When it
    /// comes from `outside` Tileception, updating it is left to the user.
    Update {
        name: String,
        running: String,
        latest: String,
        outside: Option<Outside>,
        done: bool,
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

/// The agent now running, as far as updates go.
#[derive(Debug, Clone, Default)]
struct Running {
    version: Option<String>,
    /// Where it comes from when Tileception did not install it, and so
    /// cannot update it.
    outside: Option<Outside>,
}

/// An agent program that Tileception did not install.
#[derive(Debug, Clone, PartialEq)]
pub enum Outside {
    /// A copy already on this computer.
    Installed(PathBuf),
    /// The command under `[wizard.agents]`.
    Configured,
}

/// Where the answer to a question goes.
#[derive(Debug, Clone)]
pub enum AskReply {
    /// The agent's own permission request.
    Agent { request_id: serde_json::Value, paths: Vec<PathBuf> },
    /// A file the agent wants to read outside Tileception's places.
    Read { request_id: serde_json::Value, path: PathBuf, line: Option<u64>, limit: Option<u64> },
    /// A file the agent wants to write.
    Write { request_id: serde_json::Value, path: PathBuf, content: String },
    /// One question of the form the agent asked for ([`Doctor::form`]), by
    /// its place among the form's fields.
    Form { request_id: serde_json::Value, field: usize },
}

/// A form the agent asked to have filled in, asked one question at a time.
struct Form {
    request_id: serde_json::Value,
    fields: Vec<tileception_acp::FormField>,
    /// What was answered so far, by field id.
    answers: Vec<(String, serde_json::Value)>,
    error: Option<&'static str>,
}

fn form_value(kind: &tileception_acp::FieldKind, value: &str) -> Result<serde_json::Value, &'static str> {
    use serde_json::Value;
    use tileception_acp::FieldKind;
    let trimmed = value.trim();
    match kind {
        FieldKind::Choice(options) => options
            .iter()
            .find(|option| option.value == value)
            .map(|option| Value::String(option.value.clone()))
            .ok_or("Choose one of the listed answers."),
        FieldKind::Bool => match trimmed.to_ascii_lowercase().as_str() {
            "true" | "yes" => Ok(Value::Bool(true)),
            "false" | "no" => Ok(Value::Bool(false)),
            _ => Err("Enter Yes or No."),
        },
        FieldKind::Number(true) => trimmed
            .parse::<i64>()
            .map(Value::from)
            .or_else(|_| trimmed.parse::<u64>().map(Value::from))
            .map_err(|_| "Enter a whole number."),
        FieldKind::Number(false) => trimmed
            .parse::<f64>()
            .ok()
            .and_then(serde_json::Number::from_f64)
            .map(Value::Number)
            .ok_or("Enter a number."),
        _ => Ok(Value::String(value.to_owned())),
    }
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
    /// Programs found only by name that the user chose to use, by registry id.
    #[serde(default)]
    use_installed: std::collections::BTreeMap<String, PathBuf>,
    /// Programs found only by name that the user chose not to use.
    #[serde(default)]
    skip_installed: std::collections::BTreeMap<String, PathBuf>,
    /// The version agents not kept by Tileception last reported, by the
    /// program's path, for those whose version cannot be read from disk.
    #[serde(default)]
    versions: std::collections::BTreeMap<String, String>,
    /// Last shown in a window of its own rather than along the edge.
    #[serde(default)]
    window: bool,
    /// Where that window floated when it was closed (x, y, width, height);
    /// none when it was tiled.
    #[serde(default)]
    float: Option<[f64; 4]>,
}

/// The `[doctor]` section of the config.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct DoctorConfig {
    /// The command line for an agent, by registry id: the program and its
    /// arguments, used in place of an installed copy or the registry's.
    pub agents: std::collections::BTreeMap<String, Vec<String>>,
}

impl DoctorConfig {
    pub fn validate(&self) -> Result<(), String> {
        match self.agents.iter().find(|(_, command)| command.first().is_none_or(|program| program.trim().is_empty())) {
            Some((agent_id, _)) => Err(format!("wizard.agents.{agent_id} needs a program to run")),
            None => Ok(()),
        }
    }

    /// The configured command for an agent, with `~/` expanded in the program.
    fn program(&self, agent_id: &str, env: &tileception_acp::Env) -> Option<tileception_acp::install::Program> {
        let (program, args) = self.agents.get(agent_id)?.split_first()?;
        let command = match program.strip_prefix("~/") {
            Some(rest) => PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(rest),
            None => PathBuf::from(program),
        };
        Some(tileception_acp::install::Program { command, args: args.to_vec(), env: env.clone() })
    }
}

/// A program found only by its name, waiting for the user to say whether
/// it is the agent.
struct Unconfirmed {
    agent_id: String,
    path: PathBuf,
}

/// Typing a secret a login asks for, one at a time.
struct SecretEntry {
    method: String,
    labels: Vec<String>,
    values: Vec<String>,
}

/// One of the panel's own slash commands: name, argument, what it does.
const COMMANDS: [(&str, &str, &str); 8] = [
    ("help", "", "Show these commands"),
    ("clear", "", "Start a new conversation"),
    ("model", "[name]", "Choose the agent's model"),
    ("mode", "[ask|tweak|yolo]", "What the agent may do without asking"),
    ("agent", "[id]", "Choose another agent"),
    ("restart", "", "Restart the agent, as after logging in elsewhere"),
    ("image", "<file>", "Send a picture with the next message"),
    ("report", "[title]", "Report a problem on GitHub"),
];

/// How much of the last question and answer `/report` offers to include,
/// in characters, so the new-issue link stays short enough for browsers.
const REPORT_QUESTION: usize = 1000;
const REPORT_ANSWER: usize = 3000;

/// The last question and the agent's answer to it, shortened to fit a
/// new-issue link. None before the first question.
fn last_exchange(messages: &[Message]) -> Option<String> {
    let asked = messages.iter().rposition(|message| matches!(message, Message::User(_)))?;
    let Message::User(question) = &messages[asked] else { return None };
    let answer = messages[asked + 1..]
        .iter()
        .filter_map(|message| match message {
            Message::Agent(text) => Some(text.trim()),
            _ => None,
        })
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n");
    let shorten = |text: &str, limit: usize| match text.char_indices().nth(limit) {
        Some((end, _)) => format!("{}\n[shortened]", &text[..end]),
        None => text.to_string(),
    };
    let mut text = format!("Last Wizard question:\n{}\n", shorten(question.trim(), REPORT_QUESTION));
    if !answer.is_empty() {
        text += &format!("\nWizard's answer:\n{}\n", shorten(&answer, REPORT_ANSWER));
    }
    Some(text)
}

/// A slash command offered while typing one.
#[derive(Debug, Clone, PartialEq)]
struct Suggestion {
    name: String,
    hint: String,
    description: String,
}

pub struct Doctor {
    pub open: bool,
    /// The screen the panel was opened on.
    pub home: Option<Output>,
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
    /// When the registry last arrived, to look again for new versions.
    loaded: Option<Instant>,
    /// The chosen agent's registry id.
    agent: Option<String>,
    use_installed: std::collections::BTreeMap<String, PathBuf>,
    skip_installed: std::collections::BTreeMap<String, PathBuf>,
    versions: std::collections::BTreeMap<String, String>,
    /// Asking whether to use a program found by name before starting it.
    unconfirmed: Option<Unconfirmed>,
    session: Option<Session>,
    /// The running agent: its version, once it says, and the program when
    /// Tileception did not install it (an installed copy or the config's).
    running: Running,
    /// Agents and versions already offered as an update, so each is
    /// offered once.
    offered: std::collections::BTreeSet<(String, String)>,
    session_generation: u64,
    status: Status,
    events: Option<(channel::Sender<DoctorEvent>, RegistrationToken)>,
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
    /// What is typed to narrow down the open menu's values.
    menu_filter: crate::text_input::TextInput,
    /// The field the pointer is selecting in while its button is down: the
    /// open menu's filter (true), or else the message.
    field_drag: Option<bool>,
    /// The open menu's first shown value, when it has more than fit.
    menu_first: usize,
    /// The remembered model per agent; put back on every new session.
    models: std::collections::BTreeMap<String, String>,
    restore_model: bool,
    /// The mode change already requested, until the next explicit switch.
    agent_mode: Option<String>,
    options_ready: bool,
    restate_mode: bool,
    mode_env_warning: Option<String>,
    /// Pictures pasted for the next message.
    attachments: Vec<Attachment>,
    /// The agent takes pictures in a prompt; unknown until it has started.
    takes_images: Option<bool>,
    /// A message with pictures, sent once the agent says how it takes them.
    waiting: Option<(String, Vec<Attachment>)>,
    /// When the agent was asked to stop, until its turn ends. Stopping again
    /// meanwhile, or waiting [`STOP_WAIT`], restarts it.
    stopping: Option<Instant>,
    /// Pictures in answers, by address.
    pictures: std::collections::HashMap<String, Picture>,
    loads: Option<(channel::Sender<Loaded>, RegistrationToken)>,
    /// The code last copied from an answer, and when.
    copied: Option<(String, Instant)>,
    /// Text selected in the conversation with the pointer.
    selected: Option<Selected>,
    /// The button or row the keyboard is on instead of the message field:
    /// one of [`Tileception::doctor_focusables`].
    focus: Option<Hit>,
    /// A message typed while a question was open, sent once the turn the
    /// question was in has ended: its prompt and pictures.
    follow_up: Option<(String, Vec<Attachment>)>,
    /// Where that message is shown in `messages`: text the stopped turn
    /// still sends goes in before it, not into the next answer.
    follow_up_at: usize,
    /// Shown in a window of its own rather than along the left edge; the
    /// toggle opens it the way it was last used.
    pub windowed: bool,
    /// The agent's form being answered, if any.
    form: Option<Form>,
    /// Where its window floated when last closed; `None` when it was tiled.
    /// A new window takes the same place.
    pub float: Option<Rect>,
    /// The window's client while it is open, and its window once mapped.
    client: Option<Client>,
    window: Option<WindowId>,
    /// The window's size, and the pointer in it.
    pub size: (f64, f64),
    pointer: (f64, f64),
}

/// A place in the conversation's text: a block, by its place in
/// `doctor_content`, and a byte in the text drawn for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct TextAt {
    block: usize,
    offset: usize,
}

struct Selected {
    anchor: TextAt,
    head: TextAt,
    /// The button is still down.
    dragging: bool,
    /// The texts at both ends when selected: when the conversation changes
    /// under them, the selection goes.
    ends: (Arc<str>, Arc<str>),
}

impl Default for Doctor {
    fn default() -> Self {
        let saved: Saved =
            std::fs::read_to_string(saved_path()).ok().and_then(|text| toml::from_str(&text).ok()).unwrap_or_default();
        Self {
            open: false,
            home: None,
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
            loaded: None,
            agent: saved.agent,
            use_installed: saved.use_installed,
            skip_installed: saved.skip_installed,
            versions: saved.versions,
            unconfirmed: None,
            session: None,
            running: Running::default(),
            offered: Default::default(),
            session_generation: 0,
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
            menu_filter: Default::default(),
            field_drag: None,
            menu_first: 0,
            models: saved.models,
            restore_model: false,
            agent_mode: None,
            options_ready: false,
            restate_mode: true,
            mode_env_warning: None,
            attachments: Vec::new(),
            takes_images: None,
            waiting: None,
            stopping: None,
            pictures: Default::default(),
            loads: None,
            copied: None,
            selected: None,
            focus: None,
            follow_up: None,
            follow_up_at: 0,
            form: None,
            windowed: saved.window,
            float: saved.float.map(|[left, top, width, height]| Rect::new(left, top, width, height)),
            client: None,
            window: None,
            size: (PANEL_W + 160.0, 720.0),
            pointer: (-1.0, -1.0),
        }
    }
}

/// Session events carry their owner; registry events have no owner.
type DoctorEvent = (Option<u64>, Event);

impl Doctor {
    fn retire_questions(&mut self) {
        if self.form.take().is_some() {
            self.input.clear();
        }
        for message in &mut self.messages {
            if let Message::Ask { answer: answer @ None, .. } = message {
                *answer = Some("Cancelled".into());
            }
        }
        if matches!(self.focus, Some(Hit::Answer(..))) {
            self.focus = None;
        }
    }

    fn discard_session(&mut self) {
        self.session_generation += 1;
        self.session = None;
        self.retire_questions();
        self.follow_up = None;
        self.stopping = None;
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
    /// Whether to use the program found by name ([`Unconfirmed`]).
    UseInstalled(bool),
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
    /// A code block's text, onto the clipboard.
    Copy(String),
    /// A code block's command, run in a terminal.
    Run(String),
    /// Into a window of its own (true), or back to the left edge.
    PopOut(bool),
    /// Restart the agent on the registry's newer version, by its message.
    Update(usize),
}

/// A row of a menu over the conversation.
#[derive(Clone, PartialEq)]
enum PointerAction {
    Hit(Hit),
    Send,
    /// Text of the conversation.
    Text,
    /// The message field.
    Field,
    /// The filter of a setting's open menu.
    Filter,
    /// The header of the window, which moves it.
    Move,
}

impl crate::ui::PointerAction for PointerAction {
    fn activate(&self, state: &mut Tileception, _rect: Rect, point: Point) {
        state.doctor_take_keys();
        match self {
            Self::Hit(hit) => {
                state.doctor.selected = None;
                // Moving the doctor keeps the keyboard where it was.
                if !matches!(hit, Hit::PopOut(_)) {
                    state.doctor.focus = None;
                }
                state.doctor_hit(hit.clone());
            }
            Self::Send if state.doctor.busy && !state.doctor_redirects() => state.doctor_hit(Hit::Stop),
            Self::Send => state.doctor_key(keysyms::KEY_Return, Mods::default()),
            Self::Text => state.doctor_select_from(point),
            Self::Field => state.doctor_field_press(point, false),
            Self::Filter => state.doctor_field_press(point, true),
            Self::Move => {
                if let Some(client) = &state.doctor.client {
                    client.start_move();
                }
            }
        }
    }

    fn middle_click(&self, state: &mut Tileception, _rect: Rect, point: Point) -> bool {
        let filter = match self {
            Self::Field => false,
            Self::Filter => true,
            _ => return false,
        };
        state.doctor_take_keys();
        state.doctor.focus = None;
        state.doctor.selected = None;
        state.doctor_field_caret(point, filter, false);
        state.paste_primary(Paste::Doctor);
        true
    }
}

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
    /// A button; greyed out without a `hit`.
    Button {
        rect: Rect,
        label: String,
        hit: Option<Hit>,
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
        lang: String,
        /// What the copy button copies.
        copy: String,
        /// What the run button runs; no button when not shell.
        run: Option<String>,
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
    /// The whole area drawn in: the panel at the left edge, or the window.
    panel: Rect,
    title: Rect,
    agent: Rect,
    /// Pops out into a window, or docks back at the edge.
    pop: Rect,
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

/// A code block's copy button and, for shell, its run button, right to left
/// in its top bar.
/// The byte of `image`'s text nearest to (`x_pos`, `y_pos`) within it: before
/// everything above it, after everything below.
fn offset_at(image: &TextImage, x_pos: f64, y_pos: f64) -> usize {
    let Some(first) = image.glyphs.first() else { return 0 };
    if y_pos < first.top {
        return 0;
    }
    let mut line =
        image.glyphs.iter().filter(|glyph| y_pos >= glyph.top && y_pos < glyph.top + glyph.height).peekable();
    if line.peek().is_none() {
        return image.text.len();
    }
    let mut end = 0;
    for glyph in line {
        if x_pos < glyph.left + glyph.width / 2.0 {
            return glyph.start;
        }
        end = glyph.end;
    }
    end
}

fn code_buttons(rect: Rect, run: bool) -> (Rect, Option<Rect>) {
    let top = rect.y + (CODE_BAR - CODE_BUTTON) / 2.0;
    let copy = Rect::new(rect.right() - 6.0 - CODE_BUTTON, top, CODE_BUTTON, CODE_BUTTON);
    let run = run.then_some(Rect { x: copy.x - 4.0 - CODE_BUTTON, ..copy });
    (copy, run)
}

/// What a key does while the keyboard is on a button or row
/// ([`Doctor::focus`]) rather than in the message field.
#[derive(Debug, Clone, Copy, PartialEq)]
enum FocusKey {
    /// To the button or row at this place among them.
    Move(usize),
    /// Press the one at this place.
    Choose(usize),
    /// Back to the message field, doing nothing else.
    Leave,
    Scroll(f64),
    /// Nothing to do, and the focus stays (a modifier on its own).
    Keep,
    /// Back to the message field, which takes the key: typing goes on there.
    Pass,
}

/// `sym` with `mods` on the `current` of `count` buttons or rows. With
/// `question`, they are a question's answers, which digits pick directly.
fn focus_key(sym: u32, mods: Mods, current: usize, count: usize, question: bool) -> FocusKey {
    let count = count.max(1);
    let (next, previous) = ((current + 1) % count, (current + count - 1) % count);
    if crate::config::modifier_of(sym).is_some() {
        return FocusKey::Keep;
    }
    if mods.ctrl || mods.alt || mods.logo {
        return FocusKey::Pass;
    }
    match sym {
        keysyms::KEY_Escape => FocusKey::Leave,
        keysyms::KEY_ISO_Left_Tab => FocusKey::Move(previous),
        keysyms::KEY_Tab if mods.shift => FocusKey::Move(previous),
        keysyms::KEY_Tab | keysyms::KEY_Right | keysyms::KEY_Down => FocusKey::Move(next),
        keysyms::KEY_Left | keysyms::KEY_Up => FocusKey::Move(previous),
        keysyms::KEY_Home => FocusKey::Move(0),
        keysyms::KEY_End => FocusKey::Move(count - 1),
        keysyms::KEY_Return | keysyms::KEY_KP_Enter | keysyms::KEY_space | keysyms::KEY_KP_Space => {
            FocusKey::Choose(current)
        }
        keysyms::KEY_Page_Up => FocusKey::Scroll(-4.0),
        keysyms::KEY_Page_Down => FocusKey::Scroll(4.0),
        keysyms::KEY_1..=keysyms::KEY_9 | keysyms::KEY_KP_1..=keysyms::KEY_KP_9 if question => {
            let first = if sym >= keysyms::KEY_KP_1 { keysyms::KEY_KP_1 } else { keysyms::KEY_1 };
            let picked = (sym - first) as usize;
            if picked < count { FocusKey::Choose(picked) } else { FocusKey::Keep }
        }
        _ => FocusKey::Pass,
    }
}

/// From the message field, the key that goes over to the `count` buttons
/// or rows above it, and to which: Shift+Tab, or Tab, Up or Down while
/// nothing is typed. A question's answers start at the first (the main
/// one); a list from its end nearest the field going up, else its top.
fn focus_entry(sym: u32, mods: Mods, field_empty: bool, count: usize, question: bool) -> Option<usize> {
    if count == 0 || mods.ctrl || mods.alt || mods.logo {
        return None;
    }
    let upwards = if question { 0 } else { count - 1 };
    match sym {
        keysyms::KEY_ISO_Left_Tab => Some(upwards),
        keysyms::KEY_Tab if mods.shift => Some(upwards),
        keysyms::KEY_Up if field_empty => Some(upwards),
        keysyms::KEY_Tab if field_empty => Some(0),
        keysyms::KEY_Down if field_empty && !question => Some(0),
        _ => None,
    }
}

/// The line under an open question's buttons: its keys while it has the
/// keyboard, or how to get to it from the message field.
fn ask_hint(buttons: usize, focused: bool) -> String {
    if !focused {
        return "Shift+Tab or Up to answer here, or send a message to decline it".into();
    }
    let pick = match buttons.min(9) {
        0 | 1 => "1 picks".to_string(),
        last => format!("1-{last} pick"),
    };
    format!("Enter chooses, arrows move, {pick}, Esc to the message field")
}

/// The one-time rejection when redirecting a turn, or cancellation.
fn reject_choice(choices: &[Choice]) -> Option<String> {
    choices.iter().find(|choice| choice.kind == "reject_once").map(|choice| choice.choice_id.clone())
}

/// Questions offered on an empty conversation; a click asks them.
const EXAMPLES: [&str; 3] =
    ["Make the gaps between windows smaller", "Use Ghostty as my terminal", "Why is my wallpaper not showing?"];

impl Tileception {
    /// The `doctor` command: opens the panel (taking the keyboard), or
    /// closes it when it already has the keyboard. Popped out, it focuses
    /// the window, or closes it when it is focused already.
    pub fn toggle_doctor(&mut self) {
        if self.is_locked() {
            return;
        }
        if self.doctor.windowed {
            if self.doctor_window_focused() {
                self.close_doctor();
            } else {
                self.open_doctor();
            }
            return;
        }
        // Open is enough: after a click elsewhere took the keyboard, the same
        // key should still put the panel away. Clicking the panel focuses it.
        // Open on another screen, it comes over to this one.
        if self.doctor_here() {
            self.close_doctor();
        } else {
            self.open_doctor();
        }
    }

    /// Opens the doctor the way it was last used: along the edge, or as its
    /// window, which is focused when it is open already. Does nothing when
    /// the doctor is switched off in `[shell]`.
    pub fn open_doctor(&mut self) {
        if !self.shell().doctor() {
            tracing::info!("the doctor is switched off in [shell]");
            return;
        }
        if self.launcher.open {
            self.close_launcher();
        }
        self.notifications.focused = false;
        self.doctor.edited = Instant::now();
        // Loaded again now and then, so new versions of agents show.
        let stale = self.doctor.loaded.is_none_or(|loaded| loaded.elapsed() > REGISTRY_AGAIN);
        if (self.doctor.registry.is_none() || stale) && !self.doctor.loading {
            self.load_registry(false);
        }
        if self.doctor.windowed {
            self.open_doctor_window();
            return self.doctor_changed();
        }
        if !self.doctor_here() {
            self.doctor.opened = Instant::now();
        }
        self.doctor.home = self.outputs.active.clone();
        self.doctor.open = true;
        self.doctor.focused = true;
        self.force_keyboard_focus_sync();
        self.doctor_changed();
    }

    /// Starts the doctor's window, or brings it forward when it is open.
    fn open_doctor_window(&mut self) {
        if let Some(window) = self.doctor.window {
            self.fly_to_window(window);
            return;
        }
        if self.doctor.client.is_some() {
            // Started; its window is on its way.
            return;
        }
        self.doctor.open = true;
        self.doctor.focused = false;
        self.doctor.pointer = (-1.0, -1.0);
        match Client::start(self, client::Kind::Doctor) {
            Ok(started) => self.doctor.client = Some(started),
            Err(error) => {
                self.doctor.open = false;
                tracing::warn!("could not open the doctor's window: {error}");
            }
        }
        self.force_keyboard_focus_sync();
    }

    /// Puts the doctor away, panel or window. The conversation stays.
    pub fn close_doctor(&mut self) {
        // Floating or tiled, the next window comes back the same way.
        if let Some(window) = self.doctor.window {
            let float = match self.area_for_window(window) {
                Some(area) if self.context_area().as_ref() != Some(&area) => {
                    self.with_area(&area, |state| state.world.float_rect(window)).flatten()
                }
                _ => self.world.float_rect(window),
            };
            if float != self.doctor.float {
                self.doctor.float = float;
                self.save_doctor();
            }
        }
        self.doctor.open = false;
        self.doctor.focused = false;
        self.doctor.field_drag = None;
        self.doctor.window = None;
        if let Some(started) = self.doctor.client.take() {
            self.loop_handle.remove(started.source);
        }
        self.force_keyboard_focus_sync();
        self.doctor_changed();
    }

    /// Pops the doctor out into its own window, or docks it at the left
    /// edge again, and remembers which for next time.
    fn set_doctor_windowed(&mut self, windowed: bool) {
        if self.doctor.windowed == windowed {
            return;
        }
        let open = self.doctor.open;
        self.close_doctor();
        self.doctor.windowed = windowed;
        self.save_doctor();
        if open {
            self.open_doctor();
        }
    }

    /// The doctor has changed: draw it again, in its window too.
    fn doctor_changed(&mut self) {
        if let Some(started) = &self.doctor.client {
            started.invalidate();
        }
        self.schedule_render();
    }

    pub(crate) fn is_doctor_window(&self, window: WindowId) -> bool {
        self.doctor.window == Some(window)
    }

    /// Whether the doctor's window is open and has the keyboard.
    fn doctor_window_focused(&self) -> bool {
        self.doctor.window.is_some()
            && self.world.focus() == self.doctor.window
            && self.is_active_output()
            && !self.world.in_overview()
    }

    /// Whether typing goes to the doctor now, in either form.
    pub(crate) fn doctor_has_keys(&self) -> bool {
        if self.doctor.windowed { self.doctor_window_focused() } else { self.doctor.open && self.doctor.focused }
    }

    /// A click in the doctor gives it the keyboard. Its window has it
    /// already: windows take the keyboard when clicked.
    fn doctor_take_keys(&mut self) {
        if !self.doctor.windowed && !self.doctor.focused {
            self.doctor.focused = true;
            self.force_keyboard_focus_sync();
        }
    }

    /// Where the pointer is, in the coordinates the doctor is laid out in.
    fn doctor_pointer(&self) -> Point {
        if self.doctor.windowed {
            Point::new(self.doctor.pointer.0, self.doctor.pointer.1)
        } else {
            Point::new(self.pointer.x, self.pointer.y)
        }
    }

    /// A new toplevel: the doctor's own window, when it is from its client.
    pub(crate) fn doctor_client_window(
        &mut self,
        surface: &smithay::wayland::shell::xdg::ToplevelSurface,
        window: WindowId,
    ) {
        use smithay::reexports::wayland_server::Resource;
        let ours =
            self.doctor.client.as_ref().is_some_and(|started| {
                surface.wl_surface().client().is_some_and(|owner| owner.id() == started.client_id)
            });
        if ours {
            self.doctor.window = Some(window);
        }
    }

    pub(crate) fn doctor_client_event(
        &mut self,
        client_id: &smithay::reexports::wayland_server::backend::ClientId,
        event: client::Event,
    ) {
        if self.doctor.client.as_ref().is_none_or(|started| started.client_id != *client_id) {
            return;
        }
        // Moving over the window redraws it only when what is under the
        // pointer changes (a hover) or something is being dragged: each
        // frame of it costs the compositor a full draw.
        let hover = |state: &Self| state.doctor_pointer_target(state.doctor_pointer()).map(|target| target.rect);
        let before = matches!(event, client::Event::Motion(_)).then(|| hover(self));
        if before.is_none() {
            self.doctor_changed();
        }
        if let Some(area) = self.doctor.window.and_then(|window| self.area_for_window(window))
            && self.context_area().as_ref() != Some(&area)
        {
            let client_id = client_id.clone();
            self.with_area(&area, move |state| state.doctor_client_event(&client_id, event));
            return;
        }
        match event {
            client::Event::Resize(width, height) => {
                self.doctor.size = (f64::from(width), f64::from(height));
                if self.doctor.menu.is_some() {
                    self.reveal_menu_selected();
                }
            }
            client::Event::Motion(point) => {
                self.doctor.pointer = (point.x, point.y);
                let dragged = self.doctor_drag_at(point);
                if dragged || before != Some(hover(self)) {
                    self.doctor_changed();
                }
            }
            client::Event::Leave => self.doctor.pointer = (-1.0, -1.0),
            client::Event::Button(true) => {
                let point = self.doctor_pointer();
                self.doctor_press_at(point);
            }
            client::Event::Button(false) => {
                self.doctor_release_at();
            }
            client::Event::MiddleClick => {
                let point = self.doctor_pointer();
                if let Some(target) = self.doctor_pointer_target(point) {
                    target.middle_click(self, point);
                }
            }
            client::Event::Scroll(amount) if amount != 0.0 => {
                self.doctor_scroll(if amount > 0.0 { 1.0 } else { -1.0 });
            }
            client::Event::Scroll(_) => {}
            client::Event::Key { sym, mods, pressed, code } => {
                if pressed {
                    self.doctor_key(sym, mods);
                    if crate::key_repeat::edits(sym, mods) {
                        self.start_key_repeat(
                            code,
                            crate::key_repeat::Repeat::Commands(vec![crate::commands::Command::DoctorKey(sym, mods)]),
                        );
                    }
                } else {
                    self.key_released(code, crate::config::modifier_of(sym).is_some());
                }
            }
            client::Event::Unfocus => {
                self.stop_key_repeat();
                self.doctor.field_drag = None;
            }
            // Like the panel, the window keeps its conversation when closed.
            client::Event::Closed | client::Event::CloseRequested => return self.close_doctor(),
        }
        let cursor = self
            .doctor_pointer_target(self.doctor_pointer())
            .map_or(smithay::input::pointer::CursorIcon::Default, |target| target.cursor);
        if let Some(started) = &self.doctor.client {
            started.cursor(cursor);
        }
    }

    /// Draws the doctor's window when it changed, in the context of the
    /// screen that shows it.
    pub(crate) fn update_doctor_client(&mut self, renderer: &mut GlesRenderer) {
        if self.doctor.client.is_none() {
            return;
        }
        if let Some(area) = self.doctor.window.and_then(|window| self.area_for_window(window))
            && self.context_area().as_ref() != Some(&area)
        {
            self.with_area(&area, |state| state.render_doctor_client(renderer));
        } else {
            self.render_doctor_client(renderer);
        }
    }

    fn render_doctor_client(&self, renderer: &mut GlesRenderer) {
        use smithay::{
            backend::renderer::{Bind, Offscreen, damage::OutputDamageTracker, gles::GlesTexture},
            utils::Rectangle,
        };
        let Some(started) = &self.doctor.client else { return };
        let physical =
            ((self.doctor.size.0 * self.scale).round() as i32, (self.doctor.size.1 * self.scale).round() as i32);
        if !started.needs_frame(self.scale, physical) {
            return;
        }
        let logical = (self.doctor.size.0 as i32, self.doctor.size.1 as i32);
        let size = ((self.doctor.size.0 * self.scale).round() as i32, (self.doctor.size.1 * self.scale).round() as i32);
        let result = (|| -> Result<Vec<u8>, String> {
            let elements = self.doctor_view_elements(renderer);
            let mut texture: GlesTexture =
                renderer.create_buffer(Fourcc::Abgr8888, size.into()).map_err(|error| error.to_string())?;
            {
                let mut target = renderer.bind(&mut texture).map_err(|error| error.to_string())?;
                let mut damage = OutputDamageTracker::new(size, 1.0, Transform::Normal);
                let result = damage
                    .render_output(renderer, &mut target, 0, &elements, self.theme.background.0)
                    .map_err(|error| format!("{error:?}"))?;
                result.sync.wait().map_err(|error| format!("{error:?}"))?;
            }
            crate::capture::read_region(renderer, &texture, Rectangle::from_size(size.into()))
        })();
        match result {
            Ok(pixels) => started.frame(client::Frame { pixels, width: size.0, height: size.1, logical }),
            Err(error) => tracing::warn!("could not draw the doctor's window: {error}"),
        }
    }

    /// Opens the panel and asks `question` straight away.
    pub fn ask_doctor(&mut self, question: &str) {
        self.open_doctor();
        if !self.doctor.open {
            return;
        }
        let question = question.trim();
        if !question.is_empty() {
            self.send_to_doctor(question.to_string());
        }
    }

    /// Where agent events come in, on the compositor thread.
    fn doctor_emitter(&mut self, generation: Option<u64>) -> Option<tileception_acp::Emit> {
        if self.doctor.events.is_none() {
            let (sender, receiver) = channel::channel::<DoctorEvent>();
            let token = self
                .loop_handle
                .insert_source(receiver, |event, _, state| {
                    if let channel::Event::Msg((generation, event)) = event
                        && generation.is_none_or(|generation| generation == state.doctor.session_generation)
                    {
                        state.doctor_event(event);
                    }
                })
                .map_err(|err| tracing::warn!("doctor events: {err}"))
                .ok()?;
            self.doctor.events = Some((sender, token));
        }
        let sender = Mutex::new(self.doctor.events.as_ref()?.0.clone());
        Some(Arc::new(move |event| {
            let _ = sender.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).send((generation, event));
        }))
    }

    fn load_registry(&mut self, refresh: bool) {
        if let Some(emit) = self.doctor_emitter(None) {
            self.doctor.loading = true;
            tileception_acp::fetch_registry(refresh, emit);
        }
    }

    /// Tells the user once when the registry has a newer version of the
    /// running agent than it reports, with a button to update it when
    /// Tileception installed it.
    fn offer_update(&mut self) {
        let (Some(running), Some(agent)) = (self.doctor.running.version.clone(), self.chosen_agent()) else { return };
        if self.doctor.session.is_none() || !tileception_acp::local::older(&running, &agent.version) {
            return;
        }
        if !self.doctor.offered.insert((agent.agent_id.clone(), agent.version.clone())) {
            return;
        }
        self.doctor.messages.push(Message::Update {
            name: agent.name,
            running,
            latest: agent.version,
            outside: self.doctor.running.outside.clone(),
            done: false,
        });
        self.doctor_changed();
    }

    /// Starts the agent again on the registry's version, which Tileception
    /// downloads or fetches as it would for a first start.
    fn update_agent(&mut self, index: usize) {
        let Some(Message::Update { name, latest, outside: None, done, .. }) = self.doctor.messages.get_mut(index)
        else {
            return;
        };
        if *done {
            return;
        }
        *done = true;
        let note =
            format!("Updating {name} to {latest}. It starts a new conversation and no longer remembers this one.");
        self.doctor.messages.push(Message::Note(note));
        self.doctor.discard_session();
        self.doctor.busy = false;
        self.ensure_session();
        self.doctor.scroll = 0.0;
        self.doctor_changed();
    }

    /// Where the agent comes from, for the picker, when not the registry.
    fn agent_source(&self, agent: &Agent) -> Option<String> {
        if let Some(command) = self.config.file.doctor.agents.get(&agent.agent_id) {
            return Some(format!("From your config: {}", command.join(" ")));
        }
        let installed = agent.installed.as_ref()?;
        let path = tilde(&installed.command);
        let reported = self.doctor.versions.get(installed.command.to_string_lossy().as_ref());
        Some(match installed.version.as_ref().or(reported) {
            Some(version) if tileception_acp::local::older(version, &agent.version) => {
                format!("Installed {version} (the registry has {}): {path}", agent.version)
            }
            Some(version) => format!("Installed {version}: {path}"),
            None if self.doctor.skip_installed.get(&agent.agent_id) == Some(&installed.command) => {
                return None;
            }
            None => format!("Installed: {path}"),
        })
    }

    fn chosen_agent(&self) -> Option<Agent> {
        let agent_id = self.doctor.agent.as_ref()?;
        self.doctor.registry.as_ref()?.as_ref().ok()?.iter().find(|agent| &agent.agent_id == agent_id).cloned()
    }

    /// Starts the chosen agent if it is not running yet.
    fn ensure_session(&mut self) {
        if self.doctor.session.is_some() {
            return;
        }
        let test_agent = test_agent();
        let mut agent = self.chosen_agent();
        if agent.is_none() && test_agent.is_none() {
            return;
        }
        let configured = agent.as_ref().and_then(|agent| {
            let env = match &agent.launch {
                tileception_acp::Launch::Npx { env, .. }
                | tileception_acp::Launch::Uvx { env, .. }
                | tileception_acp::Launch::Binary { env, .. } => env,
            };
            self.config.file.doctor.program(&agent.agent_id, env)
        });
        // A program found only by name may be another one: the user says once.
        if test_agent.is_none()
            && configured.is_none()
            && let Some(agent) = &mut agent
            && let Some(installed) = &agent.installed
            && installed.matched == tileception_acp::local::Match::Name
        {
            let path = installed.command.clone();
            if self.doctor.skip_installed.get(&agent.agent_id) == Some(&path) {
                agent.installed = None;
            } else if self.doctor.use_installed.get(&agent.agent_id) != Some(&path) {
                self.doctor.unconfirmed = Some(Unconfirmed { agent_id: agent.agent_id.clone(), path });
                return;
            }
        }
        self.doctor.unconfirmed = None;
        let Some(emit) = self.doctor_emitter(Some(self.doctor.session_generation)) else { return };
        // The documented defaults, for the agent to read.
        let defaults = state_dir().join("default-config.toml");
        if let Err(error) =
            std::fs::create_dir_all(state_dir()).and_then(|_| write_if_changed(&defaults, crate::config::DEFAULT))
        {
            tracing::warn!("{}: {error}", defaults.display());
        }
        if let Err(error) = export_doctor_docs() {
            tracing::warn!("could not export the doctor's documentation: {error}");
        }
        let cwd = crate::config::path().parent().map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
        if let Err(error) = std::fs::create_dir_all(&cwd) {
            tracing::warn!("{}: {error}", cwd.display());
        }
        let context = doctor_context(self.doctor.mode.label());
        self.doctor.options.clear();
        self.doctor.options_ready = false;
        self.doctor.agent_mode = None;
        self.doctor.restate_mode = true;
        self.doctor.mode_env_warning = None;
        self.doctor.commands.clear();
        self.doctor.menu = None;
        self.doctor.restore_model = true;
        self.doctor.takes_images = None;
        // Its commands reach this session, without looking for the socket.
        let mut env: tileception_acp::Env = self
            .ipc_socket
            .iter()
            .map(|socket| ("TILECEPTION_SOCKET".to_string(), socket.to_string_lossy().into_owned()))
            .collect();
        if is_opencode(self.doctor.agent.as_deref()) {
            match opencode_env(self.doctor.mode) {
                Ok(permissions) => env.extend(permissions),
                Err(error) => self.doctor.mode_env_warning = Some(error),
            }
        }
        if is_codex(self.doctor.agent.as_deref()) {
            match codex_config(std::env::var("CODEX_CONFIG").ok().as_deref()) {
                Ok(config) => {
                    env.insert("CODEX_CONFIG".into(), config);
                }
                Err(error) => tracing::warn!("Codex keeps its config as it is: {error}"),
            }
        }
        self.doctor.session = Some(match (test_agent, agent) {
            (Some(line), _) => {
                let mut words = line.split_whitespace().map(str::to_string);
                let command = words.next().unwrap_or_default().into();
                let program =
                    tileception_acp::install::Program { command, args: words.collect(), env: Default::default() };
                self.doctor.agent.get_or_insert_with(|| "test agent".into());
                Session::start_program("Test agent".into(), program, cwd, context, env, emit)
            }
            (None, Some(agent)) => {
                self.doctor.status = Status::Starting(format!("Starting {}\u{2026}", agent.name));
                self.doctor.running.outside = if configured.is_some() {
                    Some(Outside::Configured)
                } else {
                    agent.installed.as_ref().map(|installed| Outside::Installed(installed.command.clone()))
                };
                match configured {
                    Some(program) => Session::start_program(agent.name, program, cwd, context, env, emit),
                    None => Session::start(agent, cwd, context, env, emit),
                }
            }
            (None, None) => return,
        });
    }

    /// `doctor use <id>`: picks an agent by its registry id.
    pub fn use_doctor_agent(&mut self, agent_id: String) {
        self.open_doctor();
        if !self.doctor.open {
            return;
        }
        self.pick_agent(agent_id);
    }

    fn pick_agent(&mut self, agent_id: String) {
        if self.doctor.agent.as_ref() != Some(&agent_id) {
            self.doctor.discard_session();
            self.doctor.unconfirmed = None;
            self.doctor.busy = false;
            self.doctor.agent = Some(agent_id);
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
        self.submit_to_doctor(text.clone(), text);
    }

    /// A question from the rubber duck, with its screenshot: shown in the
    /// conversation as `question`, sent to the agent as `prompt`. Returns
    /// where in the conversation the answer starts, or why it was not sent.
    pub fn doctor_ask_from_duck(
        &mut self,
        question: String,
        prompt: String,
        png: Arc<[u8]>,
        (rgba, width, height): (Vec<u8>, u32, u32),
    ) -> Result<usize, &'static str> {
        if self.doctor.busy {
            return Err("I'm still thinking about the last one. It's with the Wizard.");
        }
        if self.doctor.agent.is_none() && test_agent().is_none() {
            return Err("Pick an agent in the Wizard first, then ask me again.");
        }
        // The agent list loads when the panel first opens; the duck may come first.
        if self.doctor.registry.is_none() && !self.doctor.loading {
            self.load_registry(false);
        }
        self.doctor.attachments.push(Attachment { png, thumb: Thumb::new(&rgba, (width, height)) });
        let mark = self.doctor.messages.len();
        if !self.submit_to_doctor(question, prompt) {
            self.doctor.attachments.pop();
            return Err("The Wizard's agent is not available yet. Have a look in the Wizard.");
        }
        Ok(mark)
    }

    /// Sends a message, showing `shown` in the conversation and prompting
    /// the agent with `prompt`. False when nothing was sent.
    fn submit_to_doctor(&mut self, shown: String, prompt: String) -> bool {
        if self.doctor.busy {
            return false;
        }
        if self.doctor.agent.is_none() && test_agent().is_none() {
            self.doctor.view = View::Picker;
            self.doctor.messages.push(Message::Note("Choose an agent first.".into()));
            return false;
        }
        self.ensure_session();
        // Right after login the agent list may still be loading: keep the
        // question until the agent has started.
        let starting = self.doctor.session.is_none() && (self.doctor.loading || self.doctor.unconfirmed.is_some());
        if self.doctor.session.is_none() && !starting {
            self.doctor.messages.push(Message::Note("The agent is not available yet.".into()));
            return false;
        }
        let attachments = self.show_sent(shown);
        self.start_turn(prompt, attachments);
        true
    }

    /// Puts a message and the pictures waiting to go with it into the
    /// conversation; returns the pictures.
    fn show_sent(&mut self, shown: String) -> Vec<Attachment> {
        let attachments = std::mem::take(&mut self.doctor.attachments);
        if !attachments.is_empty() {
            self.doctor
                .messages
                .push(Message::Attached(attachments.iter().map(|attachment| attachment.thumb.clone()).collect()));
        }
        if !shown.is_empty() {
            self.doctor.messages.push(Message::User(shown));
        }
        self.doctor.view = View::Conversation;
        self.doctor.scroll = 0.0;
        attachments
    }

    /// Prompts the agent, now or once it is ready for it.
    fn start_turn(&mut self, prompt: String, attachments: Vec<Attachment>) {
        // Right after login the agent list may still be loading: keep the
        // question until the agent has started.
        let starting = self.doctor.session.is_none() && (self.doctor.loading || self.doctor.unconfirmed.is_some());
        self.doctor.busy = true;
        self.doctor.stopping = None;
        self.doctor.approved.clear();
        // How pictures go depends on the agent, which may not have said yet.
        if starting || !self.doctor.options_ready || (self.doctor.takes_images.is_none() && !attachments.is_empty()) {
            self.doctor.waiting = Some((prompt, attachments));
        } else {
            self.prompt_doctor(prompt, attachments);
        }
    }

    fn prompt_doctor(&mut self, text: String, attachments: Vec<Attachment>) {
        // Confirm the mode again when the agent no longer reports it, for
        // example after it switched by itself or rejected the last change.
        if doctor_mode_target(&self.doctor).is_some_and(|(option, wanted)| option.current != wanted) {
            self.doctor.agent_mode = None;
        }
        self.sync_agent_mode();
        let Some(session) = &self.doctor.session else { return };
        if std::mem::take(&mut self.doctor.restate_mode) {
            session.send(Request::Context(format!(
                "Tileception permission mode: {}. {}. This applies to this message and supersedes earlier mode instructions.",
                self.doctor.mode.short(),
                self.doctor.mode.description()
            )));
        }
        let (text, images) = if self.doctor.takes_images == Some(true) {
            let images = attachments
                .into_iter()
                .map(|attachment| tileception_acp::Image { mime: "image/png".into(), data: attachment.png })
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
                .map_err(|err| tracing::warn!("doctor pictures: {err}"))
                .ok()?;
            self.doctor.loads = Some((sender, token));
        }
        Some(self.doctor.loads.as_ref()?.0.clone())
    }

    fn doctor_loaded(&mut self, loaded: Loaded) {
        match loaded {
            Loaded::Picture(url, pixels) => {
                let picture = match pixels {
                    Some((rgba, width, height)) => Picture::Ready(Thumb::new(&rgba, (width, height))),
                    None => Picture::Failed,
                };
                self.doctor.pictures.insert(url, picture);
            }
            Loaded::Attachment(Ok((png, (rgba, width, height)))) => {
                if self.doctor.attachments.len() < MAX_ATTACHMENTS {
                    self.doctor
                        .attachments
                        .push(Attachment { png: png.into(), thumb: Thumb::new(&rgba, (width, height)) });
                }
            }
            Loaded::Attachment(Err(error)) => self.doctor.messages.push(Message::Note(error)),
        }
        self.doctor_changed();
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
        let known = ["https://", "http://", "mailto:", "file://"].iter().any(|scheme| url.starts_with(scheme));
        if known || url.starts_with('/') {
            self.spawn("xdg-open", &[url.to_string()]);
        }
    }

    fn doctor_event(&mut self, event: Event) {
        // Questions and files depend on the permission mode.
        let event = match event {
            Event::Permission { id: request_id, title, choices, action } => {
                self.doctor_permission(request_id, title, choices, action);
                self.doctor_changed();
                return;
            }
            Event::File { id: request_id, op: file_op } => {
                self.doctor_file(request_id, file_op);
                self.doctor_changed();
                return;
            }
            Event::Form { id: request_id, message, fields } => {
                self.doctor_form(request_id, message, fields);
                self.doctor_changed();
                return;
            }
            other => other,
        };
        let doctor = &mut self.doctor;
        match event {
            Event::Registry(result) => {
                doctor.loading = false;
                doctor.loaded = Some(Instant::now());
                // A saved agent that left the registry: choose again.
                if let (Ok(agents), Some(agent_id), None) = (&result, &doctor.agent, &doctor.session)
                    && !agents.iter().any(|agent| &agent.agent_id == agent_id)
                {
                    doctor.view = View::Picker;
                }
                doctor.missing = match &result {
                    Ok(agents) => {
                        agents.iter().filter_map(|agent| Some((agent.agent_id.clone(), agent.missing()?))).collect()
                    }
                    Err(_) => Default::default(),
                };
                doctor.registry = Some(result);
                if doctor.view == View::Conversation && (doctor.open || doctor.waiting.is_some()) {
                    self.ensure_session();
                }
                self.offer_update();
                // A question asked while the list loaded, for an agent that did not start.
                let doctor = &mut self.doctor;
                if doctor.session.is_none() && doctor.waiting.take().is_some() {
                    doctor.busy = false;
                    doctor.messages.push(Message::Note("The agent is not available.".into()));
                }
            }
            Event::Status(status) => {
                match &status {
                    Status::Starting(_) => {
                        doctor.running.version = None;
                        doctor.retire_questions();
                        doctor.options_ready = false;
                        doctor.agent_mode = None;
                        doctor.restate_mode = true;
                    }
                    Status::Failed(error) => {
                        doctor.messages.push(Message::Note(error.clone()));
                        doctor.waiting = None;
                        doctor.discard_session();
                        doctor.busy = false;
                        doctor.options.clear();
                        doctor.menu = None;
                    }
                    Status::NeedsLogin(logins) => {
                        doctor.retire_questions();
                        doctor.busy = false;
                        doctor.logins.clone_from(logins);
                    }
                    _ => {}
                }
                doctor.status = status;
            }
            // Late text from a turn stopped for a typed message belongs
            // before that message.
            Event::Text(text) if doctor.follow_up.is_some() && doctor.follow_up_at <= doctor.messages.len() => {
                let place = doctor.follow_up_at;
                match place.checked_sub(1).and_then(|before| doctor.messages.get_mut(before)) {
                    Some(Message::Agent(last)) => last.push_str(&text),
                    _ => {
                        doctor.messages.insert(place, Message::Agent(text));
                        doctor.follow_up_at += 1;
                    }
                }
                self.load_pictures();
            }
            Event::Text(text) => {
                match doctor.messages.last_mut() {
                    Some(Message::Agent(last)) => last.push_str(&text),
                    _ => doctor.messages.push(Message::Agent(text)),
                }
                self.load_pictures();
            }
            Event::Thought(_) => {}
            Event::Tool { id: tool_id, title, status, .. } => {
                let existing = doctor.messages.iter_mut().rev().find_map(|message| match message {
                    Message::Tool { tool_id: existing_id, title, status } if *existing_id == tool_id => {
                        Some((title, status))
                    }
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
                    None => doctor.messages.push(Message::Tool {
                        tool_id,
                        title: title.unwrap_or_else(|| "Working".into()),
                        status: status.unwrap_or_else(|| "pending".into()),
                    }),
                }
            }
            Event::Permission { .. } | Event::File { .. } | Event::Form { .. } => {}
            Event::TurnEnded(reason) => {
                doctor.busy = false;
                doctor.stopping = None;
                doctor.approved.clear();
                // Stopped for a message typed instead of an answer, which
                // goes now; that says enough.
                let redirected = doctor.follow_up.is_some();
                let note = match reason.as_str() {
                    "refusal" => Some("The agent declined to answer."),
                    "max_tokens" | "max_turn_requests" => Some("The agent stopped: its answer got too long."),
                    "cancelled" if !redirected => Some("Stopped."),
                    _ => None,
                };
                // Not after /clear: the conversation it ended is gone.
                if let Some(note) = note.filter(|_| !doctor.messages.is_empty()) {
                    doctor.messages.push(Message::Note(note.into()));
                }
                if let Some((prompt, attachments)) = self.doctor.follow_up.take() {
                    self.start_turn(prompt, attachments);
                }
            }
            Event::Note(text) => doctor.messages.push(Message::Note(text)),
            Event::Terminal(program) => self.run_login_terminal(&program),
            Event::Options(options) => {
                let first_options = !doctor.options_ready;
                doctor.options = options;
                doctor.options_ready = true;
                if let Some((option, _)) = doctor_mode_target(doctor)
                    && doctor.agent_mode.as_deref().is_some_and(|confirmed| confirmed != option.current)
                {
                    doctor.agent_mode = None;
                }
                // The model chosen before, for a new session too.
                let wanted = doctor.agent.as_ref().and_then(|agent| doctor.models.get(agent));
                let model = doctor.options.iter().find(|option| option.is_model());
                if let (true, Some(wanted), Some(model)) = (std::mem::take(&mut doctor.restore_model), wanted, model)
                    && *wanted != model.current
                    && model.values.iter().any(|value| value.value == *wanted)
                    && let Some(session) = &doctor.session
                {
                    session.send(Request::SetOption { id: model.option_id.clone(), value: wanted.clone() });
                }
                if first_options {
                    self.sync_agent_mode();
                }
                if (self.doctor.takes_images.is_some()
                    || self.doctor.waiting.as_ref().is_some_and(|(_, attachments)| attachments.is_empty()))
                    && let Some((text, attachments)) = self.doctor.waiting.take()
                {
                    self.prompt_doctor(text, attachments);
                }
            }
            Event::Commands(commands) => doctor.commands = commands,
            Event::Version(version) => {
                // Remembered for the agent list, where an installed
                // program's version cannot be read without running it.
                if let Some(Outside::Installed(path)) = &doctor.running.outside {
                    let path = path.to_string_lossy().into_owned();
                    if doctor.versions.get(&path) != Some(&version) {
                        doctor.versions.insert(path, version.clone());
                        self.save_doctor();
                    }
                }
                self.doctor.running.version = Some(version);
                self.offer_update();
            }
            Event::Images(takes) => {
                doctor.takes_images = Some(takes);
                if doctor.options_ready
                    && let Some((text, attachments)) = doctor.waiting.take()
                {
                    self.prompt_doctor(text, attachments);
                }
            }
        }
        self.doctor_changed();
    }

    fn doctor_reply(&self, request_id: serde_json::Value, result: Result<serde_json::Value, String>) {
        if let Some(session) = &self.doctor.session {
            session.send(Request::Reply { id: request_id, result });
        }
    }

    /// The agent's own permission request: decided by the mode, or asked.
    fn doctor_permission(
        &mut self,
        request_id: serde_json::Value,
        title: String,
        choices: Vec<Choice>,
        action: tileception_acp::Action,
    ) {
        let paths: Vec<PathBuf> =
            action.paths.iter().cloned().chain(action.diffs.iter().map(|change| change.0.clone())).collect();
        if let Some(choice) = doctor_policy::auto_choice(self.doctor.mode, &action, &choices, &doctor_scope()) {
            self.doctor.approved.extend(paths);
            if let Some(session) = &self.doctor.session {
                session.send(Request::Permission { id: request_id, choice: Some(choice) });
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
            reply: AskReply::Agent { request_id, paths },
            title,
            choices,
            diff,
            answer: None,
        });
        self.doctor.scroll = 0.0;
        self.focus_new_question();
    }

    /// A file the agent reads or writes through Tileception.
    fn doctor_file(&mut self, request_id: serde_json::Value, file_op: tileception_acp::FileOp) {
        let scope = doctor_scope();
        let mode = self.doctor.mode;
        let yes_no = |yes: &str, decline: &str| {
            vec![
                Choice { choice_id: "allow".into(), name: yes.into(), kind: "allow_once".into() },
                Choice { choice_id: "deny".into(), name: decline.into(), kind: "reject_once".into() },
            ]
        };
        match file_op {
            tileception_acp::FileOp::Read { path, line, limit } => {
                if doctor_policy::may_read(mode, &path, &scope) {
                    self.doctor_reply(request_id, read_for_agent(&path, line, limit));
                } else {
                    let title = format!("Read {}", path.display());
                    let reply = AskReply::Read { request_id, path, line, limit };
                    let choices = yes_no("Allow", "Deny");
                    self.doctor.messages.push(Message::Ask { reply, title, choices, diff: Vec::new(), answer: None });
                    self.doctor.scroll = 0.0;
                    self.focus_new_question();
                }
            }
            tileception_acp::FileOp::Write { path, content } => {
                // A configuration that would not load is sent back, not saved.
                if path == crate::config::path()
                    && let Err(error) = crate::config::Config::parse(&content)
                {
                    self.doctor_reply(
                        request_id,
                        Err(format!("Tileception would not load this configuration: {error}. Nothing was written.")),
                    );
                    return;
                }
                if doctor_policy::may_write(mode, &path, &scope, &self.doctor.approved) {
                    self.apply_write(request_id, path, content);
                } else {
                    let old = std::fs::read_to_string(&path).unwrap_or_default();
                    let diff = doctor_policy::diff(&old, &content).0;
                    let title = format!("Change {}", path.display());
                    let reply = AskReply::Write { request_id, path, content };
                    let choices = yes_no("Apply", "Reject");
                    self.doctor.messages.push(Message::Ask { reply, title, choices, diff, answer: None });
                    self.doctor.scroll = 0.0;
                    self.focus_new_question();
                }
            }
        }
    }

    /// The agent asks the user to fill in a form: its questions come one by
    /// one, as questions with buttons, or answered by typing.
    fn doctor_form(&mut self, request_id: serde_json::Value, message: String, fields: Vec<tileception_acp::FormField>) {
        use tileception_acp::FieldKind;
        if self.doctor.form.is_some() || fields.iter().any(|field| field.required && field.kind == FieldKind::Other) {
            if let Some(session) = &self.doctor.session {
                session.send(Request::Form { id: request_id, answers: None });
            }
            self.doctor.messages.push(Message::Note("The agent asked something the Wizard cannot show yet.".into()));
            return;
        }
        if !message.trim().is_empty() {
            self.doctor.messages.push(Message::Info(message));
        }
        self.doctor.form = Some(Form { request_id, fields, answers: Vec::new(), error: None });
        self.ask_form_field(None);
    }

    /// Asks the form's next question: `note`'s field when given, else the
    /// first one not answered yet. With none left, the answers go back.
    fn ask_form_field(&mut self, note: Option<usize>) {
        use tileception_acp::FieldKind;
        let Some(form) = &self.doctor.form else { return };
        let answered = |key: &str| form.answers.iter().any(|(answered, _)| answered == key);
        let next = note.or_else(|| {
            form.fields
                .iter()
                .position(|field| field.note_for.is_none() && field.kind != FieldKind::Other && !answered(&field.key))
        });
        let Some(index) = next else {
            let form = self.doctor.form.take();
            if let (Some(form), Some(session)) = (form, &self.doctor.session) {
                session.send(Request::Form { id: form.request_id, answers: Some(form.answers) });
            }
            return;
        };
        let field = &form.fields[index];
        let choice =
            |value: &str, name: &str| Choice { choice_id: value.into(), name: name.into(), kind: "allow_once".into() };
        let choices = match &field.kind {
            FieldKind::Choice(options) => options.iter().map(|option| choice(&option.value, &option.label)).collect(),
            FieldKind::Bool => vec![choice("true", "Yes"), choice("false", "No")],
            _ => Vec::new(),
        };
        let mut title = if field.note_for.is_some() { "Your answer".to_string() } else { field.title.clone() };
        if let Some(description) = field.description.as_deref().map(str::trim)
            && !description.is_empty()
            && description != title
        {
            title.push('\n');
            title.push_str(description);
        }
        let reply = AskReply::Form { request_id: form.request_id.clone(), field: index };
        self.doctor.messages.push(Message::Ask { reply, title, choices, diff: Vec::new(), answer: None });
        self.doctor.scroll = 0.0;
        self.focus_new_question();
    }

    /// Validates and answers the form's question, or declines it with `None`.
    fn answer_form(&mut self, index: usize, value: Option<String>, typed: bool) {
        use tileception_acp::FieldKind;
        let Some(form) = self.doctor.form.as_mut() else { return };
        let Some(Message::Ask { reply: AskReply::Form { request_id, field }, choices, answer: answer @ None, .. }) =
            self.doctor.messages.get_mut(index)
        else {
            return;
        };
        if *request_id != form.request_id {
            return;
        }
        let Some(asked) = form.fields.get(*field) else { return };
        let Some(value) = value else {
            *answer = Some("Declined".into());
            self.doctor.form = None;
            if let Some(session) = &self.doctor.session {
                session.send(Request::Form { id: request_id.clone(), answers: None });
            }
            return self.focus_pending_question();
        };
        let key = asked.key.clone();
        let other = match &asked.kind {
            FieldKind::Choice(options) => options.iter().find(|option| option.label.eq_ignore_ascii_case("other")),
            _ => None,
        };
        let note = other
            .and_then(|_| form.fields.iter().position(|candidate| candidate.note_for.as_deref() == Some(key.as_str())));
        let parsed = form_value(&asked.kind, &value).map(|parsed| (parsed, None)).or_else(|error| {
            let Some((other, note)) = other.zip(note).filter(|_| typed) else { return Err(error) };
            let field = &form.fields[note];
            if field.kind != FieldKind::Text {
                return Err(error);
            }
            Ok((
                serde_json::Value::String(other.value.clone()),
                Some((field.key.clone(), serde_json::Value::String(value.clone()))),
            ))
        });
        let (parsed, note_answer) = match parsed {
            Ok(answer) => answer,
            Err(error) => {
                form.error = Some(error);
                self.doctor.focus = None;
                self.doctor.scroll = 0.0;
                return;
            }
        };
        let label = if typed {
            self.doctor.input.clear();
            value.clone()
        } else {
            choices
                .iter()
                .find(|choice| choice.choice_id == value)
                .map_or_else(|| value.clone(), |choice| choice.name.clone())
        };
        *answer = Some(label);
        form.error = None;
        // Choosing "Other" asks for the words that go with it, unless they were typed already.
        let next = if let Some(answer) = note_answer {
            form.answers.push(answer);
            None
        } else {
            note.filter(|_| other.is_some_and(|option| option.value == value))
        };
        form.answers.retain(|(answered, _)| *answered != key);
        form.answers.push((key, parsed));
        self.ask_form_field(next);
        if self.doctor.form.is_none() {
            self.focus_pending_question();
        }
    }

    /// Writes a file for the agent, keeping a copy of what was there.
    fn apply_write(&mut self, request_id: serde_json::Value, path: PathBuf, content: String) {
        let old = std::fs::read_to_string(&path).unwrap_or_default();
        let result = doctor_policy::backup(&path, &backups_dir()).and_then(|backup| {
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir).map_err(|err| format!("{}: {err}", dir.display()))?;
            }
            std::fs::write(&path, &content).map_err(|err| format!("{}: {err}", path.display()))?;
            Ok(backup)
        });
        match result {
            Ok(backup) => {
                let (_, added, removed) = doctor_policy::diff(&old, &content);
                self.doctor.messages.push(Message::Changed { path, backup, added, removed, undone: false });
                self.doctor_reply(request_id, Ok(serde_json::json!({})));
            }
            Err(error) => {
                self.doctor.messages.push(Message::Note(format!("Could not save: {error}")));
                self.doctor_reply(request_id, Err(error));
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
        if self.doctor.mode == mode {
            // Selecting the same mode retries an agent's rejected setting.
            self.doctor.agent_mode = None;
            self.sync_agent_mode();
            self.doctor_changed();
            return;
        }
        self.doctor.mode = mode;
        self.doctor.restate_mode = true;
        self.doctor.agent_mode = None;
        self.doctor.approved.clear();
        self.save_doctor();
        if is_opencode(self.doctor.agent.as_deref()) && self.doctor.session.is_some() {
            self.doctor_hit(Hit::Stop);
            self.doctor.busy = false;
            self.doctor.options_ready = false;
            self.doctor.options.clear();
            self.doctor.takes_images = None;
            self.doctor.restore_model = true;
            let env = match opencode_env(mode) {
                Ok(env) => {
                    self.doctor.mode_env_warning = None;
                    env
                }
                Err(error) => {
                    self.doctor.mode_env_warning = Some(error);
                    Default::default()
                }
            };
            if let Some(session) = &self.doctor.session {
                session.send(Request::RestartEnv(env));
            }
            self.doctor.messages.push(Message::Note(
                "OpenCode restarted for the new permission mode; a new conversation starts with your next message."
                    .into(),
            ));
        } else {
            self.sync_agent_mode();
        }
        self.doctor_changed();
    }

    /// Select a verified agent mode for every Tileception permission mode.
    fn sync_agent_mode(&mut self) {
        let doctor = &mut self.doctor;
        let Some((option, wanted)) = doctor_mode_target(doctor) else { return };
        if doctor.agent_mode.as_deref() != Some(wanted)
            && let Some(session) = &doctor.session
        {
            session.send(Request::SetOption { id: option.option_id.clone(), value: wanted.into() });
            doctor.agent_mode = Some(wanted.into());
        }
    }

    fn save_doctor(&self) {
        let saved = Saved {
            agent: self.doctor.agent.clone(),
            mode: self.doctor.mode,
            models: self.doctor.models.clone(),
            use_installed: self.doctor.use_installed.clone(),
            skip_installed: self.doctor.skip_installed.clone(),
            versions: self.doctor.versions.clone(),
            window: self.doctor.windowed,
            float: self.doctor.float.map(|rect| [rect.x, rect.y, rect.w, rect.h]),
        };
        let path = saved_path();
        let written = path
            .parent()
            .map_or(Ok(()), std::fs::create_dir_all)
            .and_then(|_| std::fs::write(&path, toml::to_string(&saved).unwrap_or_default()));
        if let Err(error) = written {
            tracing::warn!("could not remember the doctor's settings: {error}");
        }
    }

    /// Runs `line` with `sh` in a new terminal window.
    fn open_terminal(&self, line: &str) {
        if let Err(error) = self.run_in_terminal(line) {
            tracing::warn!("{error}");
        }
    }

    /// Runs a command from an answer in a terminal, so `sudo` can ask for the
    /// password there. It is shown first and runs once Enter is pressed.
    fn run_code(&self, command: &str) {
        let shell = if tileception_acp::registry::on_path("bash") { "bash" } else { "sh" };
        let line = format!(
            "printf '%s\\n\\n' {command}; printf 'Press Enter to run, or close this window to cancel. '; read -r _ || exit; \
             echo; {shell} -c {command}; status=$?; echo; echo \"Finished (exit status $status). Press Enter to close.\"; read -r _",
            command = shell_quote(command),
        );
        self.open_terminal(&line);
    }

    /// Runs an agent's own login in the terminal.
    fn run_login_terminal(&mut self, program: &tileception_acp::install::Program) {
        let quote = shell_quote;
        let mut line = String::from("env");
        for (key, value) in &program.env {
            line += &format!(" {}={}", quote(key), quote(value));
        }
        line += &format!(" {}", quote(&program.command.display().to_string()));
        for arg in &program.args {
            line += &format!(" {}", quote(arg));
        }
        line += "; echo; echo 'Done: go back to the Wizard.'; read -r _";
        self.open_terminal(&line);
        self.doctor
            .messages
            .push(Message::Note("Log in in the terminal, then choose \u{201c}I've logged in\u{201d}.".into()));
        self.doctor.scroll = 0.0;
    }

    /// A key while the panel has the keyboard.
    pub fn doctor_key(&mut self, sym: u32, mods: Mods) {
        self.doctor.edited = Instant::now();
        // A setting's menu takes the keyboard: typing narrows it down.
        if self.doctor.menu.is_some() {
            return self.menu_key(sym, mods);
        }
        // A key being entered is masked, and so not copied or cut.
        self.doctor.input.hidden = self.doctor.secret.is_some();
        // On a button or row, the keys move between them and press them;
        // anything else goes back to the field.
        let focusables = self.doctor_focusables();
        let focused = self.doctor.focus.as_ref().and_then(|hit| focusables.iter().position(|place| place == hit));
        if let Some(current) = focused {
            let question = matches!(focusables[current], Hit::Answer(..));
            match focus_key(sym, mods, current, focusables.len(), question) {
                FocusKey::Move(place) => {
                    self.doctor.focus = Some(focusables[place].clone());
                    self.reveal_doctor_focus();
                    return self.doctor_changed();
                }
                FocusKey::Choose(place) => {
                    self.doctor.focus = None;
                    self.doctor.selected = None;
                    self.doctor_hit(focusables[place].clone());
                    return self.doctor_changed();
                }
                FocusKey::Leave => {
                    self.doctor.focus = None;
                    return self.doctor_changed();
                }
                FocusKey::Scroll(notches) => return self.doctor_scroll(notches),
                FocusKey::Keep => return,
                FocusKey::Pass => self.doctor.focus = None,
            }
        } else {
            self.doctor.focus = None;
        }
        // With nothing to copy in the field, Ctrl+C copies what is selected
        // in the conversation.
        if mods.ctrl
            && matches!(sym, keysyms::KEY_c | keysyms::KEY_C)
            && (self.doctor.input.hidden || self.doctor.input.selection().is_empty())
            && let Some(text) = self.doctor_selected_text()
        {
            self.copy_text(text);
            return;
        }
        // Slash command suggestions take the arrows, Enter and Tab.
        let rows = self.suggestions().len();
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
                keysyms::KEY_Tab => return self.complete_suggestion(selected, false),
                keysyms::KEY_Return | keysyms::KEY_KP_Enter => return self.complete_suggestion(selected, true),
                _ => {}
            }
        }
        // Over to the question's buttons, or a view's rows, above the field.
        let question = focusables.iter().any(|hit| matches!(hit, Hit::Answer(..)));
        if let Some(place) = focus_entry(sym, mods, self.doctor.input.text.is_empty(), focusables.len(), question) {
            self.doctor.focus = Some(focusables[place].clone());
            self.reveal_doctor_focus();
            return self.doctor_changed();
        }
        let redirects = self.doctor_redirects();
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
                if redirects {
                    // A message instead of an answer turns the question
                    // down and goes to the agent after it.
                    self.doctor_redirect(text);
                } else if (!text.is_empty() || attached) && (!self.doctor.busy || text.starts_with('/')) {
                    // Commands also work while an answer comes in (/clear, /mode).
                    input.clear();
                    self.send_to_doctor(text);
                }
            }
            keysyms::KEY_Page_Up => self.doctor_scroll(-4.0),
            keysyms::KEY_Page_Down => self.doctor_scroll(4.0),
            // Everything else is the message field's.
            _ => {
                let key = input.key(&mut self.doctor.compose, sym, mods);
                self.field_clipboard(key.clipboard, Paste::Doctor);
            }
        }
        self.doctor.menu_selected = 0;
        self.doctor_changed();
    }

    /// Whether the open question is one of the agent's form's.
    fn pending_form_question(&self) -> bool {
        self.pending_ask().is_some_and(|index| {
            matches!(self.doctor.messages.get(index), Some(Message::Ask { reply: AskReply::Form { .. }, .. }))
        })
    }

    /// The newest question still waiting for an answer, by its place in
    /// the conversation.
    fn pending_ask(&self) -> Option<usize> {
        self.doctor.messages.iter().rposition(|message| matches!(message, Message::Ask { answer: None, .. }))
    }

    /// What the keyboard can move across instead of typing, in order: the
    /// open question's answers, or else the rows and buttons of what is
    /// shown (agents, logins, the installed agent, example questions).
    fn doctor_focusables(&self) -> Vec<Hit> {
        let pending = self.pending_ask();
        self.doctor_content(self.doctor_frame().messages)
            .into_iter()
            .filter_map(|block| match block {
                Block::Row { hit: Some(hit), .. } | Block::Button { hit: Some(hit), .. } => Some(hit),
                _ => None,
            })
            .filter(|hit| match hit {
                Hit::Answer(index, _) => Some(*index) == pending,
                // Changes are undone with the pointer.
                Hit::Undo(_) => false,
                _ => pending.is_none(),
            })
            .collect()
    }

    /// The button or row the keyboard is on, while it is still shown.
    fn doctor_focus(&self) -> Option<Hit> {
        let focus = self.doctor.focus.as_ref()?;
        self.doctor_focusables().into_iter().find(|hit| hit == focus)
    }

    /// A question just came in: the keyboard goes to its first answer,
    /// unless a message is being typed, where a space or Enter meant for
    /// the message would answer it.
    fn focus_new_question(&mut self) {
        let typing =
            !self.doctor.input.text.is_empty() && self.doctor.edited.elapsed() < std::time::Duration::from_millis(1500);
        if !typing {
            self.focus_pending_question();
        }
    }

    fn focus_pending_question(&mut self) {
        let typed = self.pending_ask().is_some_and(|index| {
            matches!(&self.doctor.messages[index],
                Message::Ask { reply: AskReply::Form { .. }, choices, .. } if choices.is_empty())
        });
        self.doctor.focus =
            if typed { None } else { self.doctor_focusables().into_iter().find(|hit| matches!(hit, Hit::Answer(..))) };
    }

    /// Scrolls so the focused button or row shows whole.
    fn reveal_doctor_focus(&mut self) {
        let Some(focus) = self.doctor.focus.clone() else { return };
        let area = self.doctor_frame().messages;
        let blocks = self.doctor_content(area);
        let Some(rect) = blocks.iter().find_map(|block| match block {
            Block::Row { rect, hit: Some(hit), .. } | Block::Button { rect, hit: Some(hit), .. } if *hit == focus => {
                Some(*rect)
            }
            _ => None,
        }) else {
            return;
        };
        let height = blocks.iter().map(|block| block.rect().bottom()).fold(area.y, f64::max) - area.y;
        let most = (height - area.h).max(0.0);
        // A little of what is around it shows too.
        let (top, bottom) = (rect.y - GAP, rect.bottom() + GAP);
        self.doctor.scroll = match self.doctor.view {
            // Scrolled down from the top.
            View::Picker => {
                let scroll = self.doctor.scroll;
                let scroll = if top - scroll < area.y { top - area.y } else { scroll };
                let scroll = if bottom - scroll > area.bottom() { bottom - area.bottom() } else { scroll };
                scroll.clamp(0.0, most)
            }
            View::Conversation if self.doctor.messages.is_empty() => 0.0,
            // Scrolled up from the newest message at the bottom.
            View::Conversation => {
                let shift = (area.h - height).min(0.0);
                let scroll = self.doctor.scroll;
                let scroll =
                    if bottom + shift + scroll > area.bottom() { area.bottom() - bottom - shift } else { scroll };
                let scroll = if top + shift + scroll < area.y { area.y - top - shift } else { scroll };
                scroll.clamp(0.0, most)
            }
        };
    }

    /// A message typed while a question is open goes instead of an answer.
    fn doctor_redirects(&self) -> bool {
        let text = self.doctor.input.text.trim();
        if self.doctor.secret.is_some() {
            return false;
        }
        if self.pending_form_question() {
            let command = text.trim_start_matches('/').split_whitespace().next();
            return !text.is_empty()
                && !(text.starts_with('/') && COMMANDS.iter().any(|(name, ..)| Some(*name) == command));
        }
        self.pending_ask().is_some()
            && !text.starts_with('/')
            && (!text.is_empty() || !self.doctor.attachments.is_empty())
    }

    /// Sends `text` instead of answering the open questions, as Claude Code
    /// does: each is turned down first, then the turn they were in is
    /// stopped, and the message goes as the next prompt once it has ended.
    /// The answers go before the prompt, which the agent would not take
    /// while a turn is still running.
    fn doctor_redirect(&mut self, text: String) {
        // Typed words answer the agent's form question instead.
        if let Some(index) = self.pending_ask()
            && matches!(&self.doctor.messages[index], Message::Ask { reply: AskReply::Form { .. }, .. })
        {
            return self.answer_form(index, Some(text), true);
        }
        self.doctor.input.clear();
        while let Some(index) = self.pending_ask() {
            let Some(Message::Ask { choices, .. }) = self.doctor.messages.get(index) else { break };
            let choice = reject_choice(choices);
            self.doctor_hit(Hit::Answer(index, choice));
        }
        self.doctor.focus = None;
        self.doctor.follow_up_at = self.doctor.messages.len();
        let attachments = self.show_sent(text.clone());
        if self.doctor.busy {
            self.doctor.follow_up = Some((text, attachments));
            self.doctor_hit(Hit::Stop);
        } else {
            self.start_turn(text, attachments);
        }
    }

    /// Keys while a setting's menu is open: arrows and pages choose, typing
    /// filters, Enter picks and Esc clears the filter, then closes.
    fn menu_key(&mut self, sym: u32, mods: Mods) {
        let rows = self.menu_values().len();
        let selected = self.doctor.menu_selected.min(rows.saturating_sub(1));
        let page = self.menu_layout(&self.doctor_frame()).map_or(1, |(_, rows)| rows.len().max(1));
        let before = self.doctor.menu_filter.text.clone();
        let filter = &mut self.doctor.menu_filter;
        match sym {
            keysyms::KEY_Up if rows > 0 => self.doctor.menu_selected = (selected + rows - 1) % rows,
            keysyms::KEY_Down if rows > 0 => self.doctor.menu_selected = (selected + 1) % rows,
            keysyms::KEY_Page_Up => self.doctor.menu_selected = selected.saturating_sub(page),
            keysyms::KEY_Page_Down => self.doctor.menu_selected = (selected + page).min(rows.saturating_sub(1)),
            keysyms::KEY_Return | keysyms::KEY_KP_Enter if rows > 0 => return self.choose_menu_value(selected),
            keysyms::KEY_Escape if !filter.text.is_empty() => filter.clear(),
            keysyms::KEY_Escape => {
                self.doctor.menu = None;
                return self.doctor_changed();
            }
            // The filter edits like every other field.
            _ => {
                let key = filter.key(&mut self.doctor.compose, sym, mods);
                self.field_clipboard(key.clipboard, Paste::Doctor);
            }
        }
        self.doctor.edited = Instant::now();
        if self.doctor.menu_filter.text != before {
            self.menu_filter_changed();
        }
        self.reveal_menu_selected();
        self.doctor_changed();
    }

    /// Whether a setting's menu (with its filter field) is open.
    pub fn doctor_menu_open(&self) -> bool {
        self.doctor.menu.is_some()
    }

    /// The filter's text changed: back to the top, on the current value
    /// while it still matches.
    fn menu_filter_changed(&mut self) {
        self.doctor.menu_first = 0;
        self.doctor.menu_selected = self.menu_values().iter().position(|entry| entry.1).unwrap_or(0);
        self.reveal_menu_selected();
    }

    /// Scrolls the open menu so its highlighted value shows.
    fn reveal_menu_selected(&mut self) {
        let Some((_, rows)) = self.menu_layout(&self.doctor_frame()) else { return };
        let shown = rows.len().max(1);
        let selected = self.doctor.menu_selected;
        let first = &mut self.doctor.menu_first;
        if selected < *first {
            *first = selected;
        } else if selected >= *first + shown {
            *first = selected + 1 - shown;
        }
    }

    pub fn doctor_insert_paste(&mut self, text: &str) {
        // Into the open menu's filter, as a single line.
        if self.doctor.menu.is_some() {
            self.doctor.menu_filter.paste(text.trim());
            self.doctor.edited = Instant::now();
            self.menu_filter_changed();
            return self.doctor_changed();
        }
        // A key being entered comes without the space around it.
        self.doctor.input.paste(if self.doctor.secret.is_some() { text.trim() } else { text });
        self.doctor.edited = Instant::now();
        self.doctor_changed();
    }

    /// Scrolls; positive goes towards newer messages (or down the picker).
    pub fn doctor_scroll(&mut self, notches: f64) {
        let frame = self.doctor_frame();
        // Over an open setting's menu, the wheel scrolls its values.
        if self.doctor.menu.is_some()
            && let Some((outline, rows)) = self.menu_layout(&frame)
            && outline.contains(self.doctor_pointer())
        {
            let most = self.menu_values().len().saturating_sub(rows.len());
            let first = self.doctor.menu_first.min(most) as f64 + notches;
            self.doctor.menu_first = first.clamp(0.0, most as f64) as usize;
            return self.doctor_changed();
        }
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
                    .map(|block| (content - (block.rect().bottom() - area.y)).clamp(0.0, max))
                    .chain([0.0, max])
                    .collect();
                stops.sort_by(f64::total_cmp);
                stops.dedup_by(|left, right| (*left - *right).abs() < 0.5);
                // One block per press or wheel notch, whatever its size.
                let current = self.doctor.scroll;
                if notches < 0.0 {
                    stops.iter().copied().find(|stop| *stop > current + 0.5).unwrap_or(max)
                } else {
                    stops.iter().rev().copied().find(|stop| *stop < current - 0.5).unwrap_or(0.0)
                }
            }
            View::Picker => (self.doctor.scroll + delta).clamp(0.0, max),
        };
    }

    /// The panel is open on the output being drawn or handled (never when
    /// the doctor is in its own window).
    pub fn doctor_here(&self) -> bool {
        self.doctor.open
            && !self.doctor.windowed
            && match &self.doctor.home {
                Some(home) => self.output.as_ref() == Some(home),
                None => self.is_active_output(),
            }
    }

    pub fn doctor_contains(&self, point: Point) -> bool {
        self.doctor_here() && self.doctor_frame().panel.contains(point) && !self.overlay_under(point)
    }

    /// A press anywhere: inside, the panel takes the keyboard and acts on
    /// what was clicked; outside, the desktop gets the keyboard back.
    pub fn doctor_press(&mut self, point: Point) -> bool {
        if !self.doctor.open || self.doctor.windowed || self.overlay_under(point) {
            return false;
        }
        if !self.doctor_here() || !self.doctor_frame().panel.contains(point) {
            if self.doctor.focused {
                self.doctor.focused = false;
                self.force_keyboard_focus_sync();
            }
            return false;
        }
        self.doctor_take_keys();
        self.doctor_press_at(point);
        true
    }

    /// A press at `point` in the panel or the window, which has the keyboard.
    fn doctor_press_at(&mut self, point: Point) {
        let frame = self.doctor_frame();
        let target = self.doctor_pointer_target(point);
        if self.doctor.menu.is_some() && target.is_none() {
            // Clicking the menu's edges keeps it open.
            if self.menu_layout(&frame).is_some_and(|(outline, _)| outline.contains(point)) {
                return;
            }
            self.doctor.menu = None;
            return self.doctor_changed();
        }
        if let Some(target) = target {
            target.activate(self, point);
        } else {
            self.doctor_select_from(point);
        }
        self.doctor_changed();
    }

    pub(crate) fn doctor_pointer_target(&self, point: Point) -> Option<crate::ui::PointerTarget> {
        use crate::ui::PointerTarget;
        use smithay::input::pointer::CursorIcon;
        let frame = self.doctor_frame();
        if let Some((outline, rows)) = self.menu_layout(&frame) {
            // A setting's menu has its filter at the foot.
            let filter = self.menu_filter_layout(outline).0;
            if self.doctor.menu.is_some() && filter.contains(point) {
                return Some(PointerTarget::new(filter, PointerAction::Filter, CursorIcon::Text));
            }
            return rows.iter().find_map(|row| {
                row.rect.contains(point).then(|| PointerTarget::button(row.rect, PointerAction::Hit(row.hit.clone())))
            });
        }
        // The send button lies inside the field, so it is found first.
        if frame.send.contains(point) {
            return Some(PointerTarget::button(frame.send, PointerAction::Send));
        }
        if frame.field.contains(point) {
            return Some(PointerTarget::new(frame.field, PointerAction::Field, CursorIcon::Text));
        }
        let mut buttons = vec![
            (
                self.agent_button(&frame),
                PointerAction::Hit(if self.doctor.view == View::Picker { Hit::ClosePicker } else { Hit::OpenPicker }),
            ),
            (frame.pop, PointerAction::Hit(Hit::PopOut(!self.doctor.windowed))),
        ];
        if let Some((option, rect)) = self.model_button(&frame) {
            buttons.push((rect, PointerAction::Hit(Hit::OpenMenu(option.option_id.clone()))));
        }
        buttons.extend(
            frame.modes.iter().zip(Mode::ALL).map(|(rect, mode)| (*rect, PointerAction::Hit(Hit::SetMode(mode)))),
        );
        buttons.extend(
            frame.attached.iter().enumerate().map(|(index, rect)| (*rect, PointerAction::Hit(Hit::Detach(index)))),
        );
        for (rect, action) in buttons {
            if rect.contains(point) {
                return Some(PointerTarget::button(rect, action));
            }
        }
        // The rest of a window's header picks it up, as a title bar would.
        let header = Rect::new(frame.panel.x, frame.panel.y, frame.panel.w, HEADER_H);
        if self.doctor.windowed && header.contains(point) {
            return Some(PointerTarget::new(header, PointerAction::Move, CursorIcon::Grab));
        }
        for block in self.doctor_blocks(frame.messages) {
            match block {
                Block::Row { rect, hit: Some(hit), .. } | Block::Button { rect, hit: Some(hit), .. }
                    if rect.contains(point) =>
                {
                    return Some(PointerTarget::button(rect, PointerAction::Hit(hit)));
                }
                Block::Picture { rect, open, .. } if rect.contains(point) => {
                    return Some(PointerTarget::button(rect, PointerAction::Hit(Hit::Open(open))));
                }
                Block::Code { rect, copy, run, .. } if rect.contains(point) => {
                    let (copy_at, run_at) = code_buttons(rect, run.is_some());
                    if copy_at.contains(point) {
                        return Some(PointerTarget::button(copy_at, PointerAction::Hit(Hit::Copy(copy))));
                    }
                    if let Some(button) = run_at
                        && button.contains(point)
                        && let Some(run) = run
                    {
                        return Some(PointerTarget::button(button, PointerAction::Hit(Hit::Run(run))));
                    }
                    return Some(PointerTarget::new(rect, PointerAction::Text, CursorIcon::Text));
                }
                Block::Rich { rect, spans, style, .. } if rect.contains(point) => {
                    if let Some(image) = self.rich_image(&spans, style, rect.w) {
                        for ([left, top, width, height], url) in &image.links {
                            let link = Rect::new(
                                rect.x + f64::from(*left),
                                rect.y + f64::from(*top),
                                f64::from(*width),
                                f64::from(*height),
                            );
                            if link.contains(point) {
                                return Some(PointerTarget::button(link, PointerAction::Hit(Hit::Open(url.clone()))));
                            }
                        }
                    }
                    return Some(PointerTarget::new(rect, PointerAction::Text, CursorIcon::Text));
                }
                _ => {}
            }
        }
        None
    }

    /// The text drawn for a block that has some, with where it is drawn.
    fn block_text(&self, block: &Block) -> Option<(Point, TextImage)> {
        match block {
            Block::Rich { rect, spans, style, .. } => {
                Some((Point::new(rect.x, rect.y), self.rich_image(spans, *style, rect.w)?.image))
            }
            Block::Code { rect, spans, style, .. } => Some((
                Point::new(rect.x + 12.0, rect.y + CODE_BAR + 9.0),
                self.rich_image(spans, *style, rect.w - 24.0)?.image,
            )),
            Block::Bubble { rect, text, .. } => self.placed_text(
                text,
                Rect::new(rect.x + 14.0, rect.y + 10.0, rect.w - 27.0, rect.h - 20.0),
                self.body_style(),
            ),
            Block::Note { rect, text } => self.placed_text(text, *rect, self.note_style()),
            _ => None,
        }
    }

    /// Text as [`Painter::text`] draws it in `rect`.
    fn placed_text(&self, text: &str, rect: Rect, style: Style) -> Option<(Point, TextImage)> {
        let image = self.text.borrow_mut().styled(
            text,
            &self.theme.font,
            style.size,
            style.colour,
            rect.w.max(1.0) as f32,
            style.weight,
            style.lines,
        )?;
        let left = match style.align {
            Align::Left => rect.x,
            Align::Centre => rect.x + (rect.w - image.width) / 2.0,
        };
        Some((Point::new(left, rect.y + (rect.h - image.height) / 2.0), image))
    }

    /// The text under or nearest to `point`, in the conversation on screen.
    fn doctor_text_at(&self, point: Point) -> Option<TextAt> {
        let texts: Vec<(usize, Point, TextImage)> = self
            .doctor_blocks_indexed(self.doctor_frame().messages)
            .into_iter()
            .filter_map(|(block, drawn)| self.block_text(&drawn).map(|(origin, image)| (block, origin, image)))
            .collect();
        for (block, origin, image) in &texts {
            if point.y < origin.y + image.height {
                let offset = offset_at(image, point.x - origin.x, point.y - origin.y);
                return Some(TextAt { block: *block, offset });
            }
        }
        texts.last().map(|(block, _, image)| TextAt { block: *block, offset: image.text.len() })
    }

    fn doctor_select_from(&mut self, point: Point) {
        let ends = self.doctor_text_at(point).filter(|_| self.doctor_frame().messages.contains(point));
        self.doctor.selected = ends.and_then(|place| {
            let text = self.content_text(place.block)?;
            Some(Selected { anchor: place, head: place, dragging: true, ends: (text.clone(), text) })
        });
        self.doctor_changed();
    }

    /// The pointer pressed in the message field, or with `filter` in the
    /// open menu's: it places the caret there, and a drag on from it selects.
    fn doctor_field_press(&mut self, point: Point, filter: bool) {
        self.doctor.selected = None;
        self.doctor.focus = None;
        self.doctor.field_drag = Some(filter);
        let extend = self.seat.get_keyboard().is_some_and(|keyboard| keyboard.modifier_state().shift);
        self.doctor_field_caret(point, filter, extend);
    }

    fn doctor_field_caret(&mut self, point: Point, filter: bool, extend: bool) {
        let frame = self.doctor_frame();
        // As each is drawn, in `draw_menu_filter` and `doctor_elements`.
        let area = if filter {
            let Some((outline, _)) = self.menu_layout(&frame) else { return };
            self.menu_filter_layout(outline).1
        } else {
            frame.text
        };
        let (font, size) = (&self.theme.font, self.theme.font_size);
        let (width, x_pos) = (area.w as f32, (point.x - area.x).max(0.0) as f32);
        let mut text = self.text.borrow_mut();
        if filter {
            self.doctor.menu_filter.click(&mut text, font, size, width, x_pos, extend);
        } else {
            // A key being entered is masked, a star for each byte.
            let input = &mut self.doctor.input;
            input.hidden = self.doctor.secret.is_some();
            input.click_masked(&mut text, "*", font, size * 1.08, width, x_pos, extend);
        }
        drop(text);
        self.doctor.edited = Instant::now();
        self.doctor_changed();
    }

    /// Pointer motion over the desktop while selecting text in the panel.
    /// True when it took it.
    pub fn doctor_drag(&mut self, point: Point) -> bool {
        !self.doctor.windowed && self.doctor_drag_at(point)
    }

    /// Pointer motion in the panel's or the window's coordinates.
    fn doctor_drag_at(&mut self, point: Point) -> bool {
        if let Some(filter) = self.doctor.field_drag {
            self.doctor_field_caret(point, filter, true);
            return true;
        }
        if !self.doctor.open || !self.doctor.selected.as_ref().is_some_and(|selected| selected.dragging) {
            return false;
        }
        if let Some(head) = self.doctor_text_at(point)
            && let Some(text) = self.content_text(head.block)
            && let Some(selected) = self.doctor.selected.as_mut()
        {
            selected.head = head;
            selected.ends.1 = text;
        }
        self.doctor_changed();
        true
    }

    /// The button coming up over the desktop after selecting text in the
    /// panel. True when it was selecting.
    pub fn doctor_release(&mut self) -> bool {
        !self.doctor.windowed && self.doctor_release_at()
    }

    fn doctor_release_at(&mut self) -> bool {
        if let Some(filter) = self.doctor.field_drag.take() {
            let input = if filter { &self.doctor.menu_filter } else { &self.doctor.input };
            self.offer_primary(input.selected_text());
            return true;
        }
        let Some(selected) = self.doctor.selected.as_mut().filter(|selected| selected.dragging) else { return false };
        selected.dragging = false;
        if selected.anchor == selected.head {
            self.doctor.selected = None;
        }
        true
    }

    /// The text of a block in the conversation, by its place in `doctor_content`.
    fn content_text(&self, block: usize) -> Option<Arc<str>> {
        let blocks = self.doctor_content(self.doctor_frame().messages);
        Some(self.block_text(blocks.get(block)?)?.1.text)
    }

    /// The selection, start first, while the conversation under it is unchanged.
    fn doctor_selection(&self) -> Option<(TextAt, TextAt)> {
        let selected = self.doctor.selected.as_ref()?;
        let unchanged = |place: TextAt, text: &Arc<str>| self.content_text(place.block).is_some_and(|now| now == *text);
        if selected.anchor == selected.head
            || !unchanged(selected.anchor, &selected.ends.0)
            || !unchanged(selected.head, &selected.ends.1)
        {
            return None;
        }
        Some((selected.anchor.min(selected.head), selected.anchor.max(selected.head)))
    }

    /// The bytes of block `block`'s text that are selected.
    fn selected_in(selection: Option<(TextAt, TextAt)>, block: usize, len: usize) -> Option<(usize, usize)> {
        let (start, end) = selection?;
        if block < start.block || block > end.block {
            return None;
        }
        let from = if block == start.block { start.offset } else { 0 };
        let until = if block == end.block { end.offset } else { len };
        (from < until).then_some((from, until))
    }

    /// The selected text, a blank line between blocks.
    fn doctor_selected_text(&self) -> Option<String> {
        let selection = self.doctor_selection()?;
        let blocks = self.doctor_content(self.doctor_frame().messages);
        let pieces: Vec<String> = (selection.0.block..=selection.1.block)
            .filter_map(|block| {
                let (_, image) = self.block_text(blocks.get(block)?)?;
                let (from, until) = Self::selected_in(Some(selection), block, image.text.len())?;
                Some(image.text.get(from..until)?.to_string())
            })
            .collect();
        (!pieces.is_empty()).then(|| pieces.join("\n\n"))
    }

    /// Shading behind the selected part of a block's text.
    fn draw_selected(
        &self,
        painter: &mut Painter,
        selection: Option<(usize, usize)>,
        origin: Point,
        image: &TextImage,
    ) {
        let Some((from, until)) = selection else { return };
        let mut line: Option<Rect> = None;
        let mut shade = |line: Option<Rect>| {
            if let Some(rect) = line {
                painter.fill(rect, self.theme.accent.alpha(0.3), 2.0);
            }
        };
        for glyph in image.glyphs.iter().filter(|glyph| glyph.start < until && glyph.end > from) {
            let rect = Rect::new(origin.x + glyph.left, origin.y + glyph.top, glyph.width, glyph.height);
            line = match line {
                Some(current) if current.y == rect.y => {
                    let (left, right) = (current.x.min(rect.x), current.right().max(rect.right()));
                    Some(Rect::new(left, current.y, right - left, current.h))
                }
                _ => {
                    shade(line);
                    Some(rect)
                }
            };
        }
        shade(line);
    }

    fn doctor_hit(&mut self, hit: Hit) {
        match hit {
            Hit::Pick(agent_id) => self.pick_agent(agent_id),
            Hit::UseInstalled(choice) => {
                if let Some(Unconfirmed { agent_id, path }) = self.doctor.unconfirmed.take() {
                    let (chosen, other) = if choice {
                        (&mut self.doctor.use_installed, &mut self.doctor.skip_installed)
                    } else {
                        (&mut self.doctor.skip_installed, &mut self.doctor.use_installed)
                    };
                    other.remove(&agent_id);
                    chosen.insert(agent_id, path);
                    self.save_doctor();
                    self.ensure_session();
                }
            }
            Hit::OpenPicker => {
                self.doctor.view = View::Picker;
                self.doctor.scroll = 0.0;
                if self.doctor.registry.as_ref().is_none_or(|registry| registry.is_err()) && !self.doctor.loading {
                    self.load_registry(false);
                }
            }
            Hit::ClosePicker if self.doctor.agent.is_some() => self.doctor.view = View::Conversation,
            Hit::ClosePicker => {}
            Hit::ReloadRegistry => self.load_registry(true),
            Hit::Update(index) => self.update_agent(index),
            Hit::Login(method) => {
                let asks = self
                    .doctor
                    .logins
                    .iter()
                    .find(|login| login.method_id == method)
                    .map(|login| login.how.asks())
                    .unwrap_or_default();
                if asks.is_empty() {
                    if let Some(session) = &self.doctor.session {
                        session.send(Request::Login { method, secrets: Vec::new() });
                    }
                } else {
                    self.doctor.input.clear();
                    self.doctor.secret = Some(SecretEntry { method, labels: asks, values: Vec::new() });
                    // The key is typed into the message field.
                    self.doctor.focus = None;
                    self.doctor_take_keys();
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
                if matches!(self.doctor.messages.get(index), Some(Message::Ask { reply: AskReply::Form { .. }, .. })) {
                    self.answer_form(index, choice, false);
                    return self.doctor_changed();
                }
                let Some(Message::Ask { reply, answer: answer @ None, choices, .. }) =
                    self.doctor.messages.get_mut(index)
                else {
                    return;
                };
                let picked =
                    choice.as_ref().and_then(|choice_id| choices.iter().find(|option| &option.choice_id == choice_id));
                let allowed = picked.is_some_and(|chosen| chosen.kind.starts_with("allow"));
                *answer = Some(picked.map_or_else(|| "Declined".to_string(), |chosen| chosen.name.clone()));
                let reply = reply.clone();
                match reply {
                    AskReply::Agent { request_id, paths } => {
                        if allowed {
                            self.doctor.approved.extend(paths);
                        }
                        if let Some(session) = &self.doctor.session {
                            session.send(Request::Permission { id: request_id, choice });
                        }
                    }
                    AskReply::Read { request_id, path, line, limit } => {
                        let result =
                            if allowed { read_for_agent(&path, line, limit) } else { Err("The user declined.".into()) };
                        self.doctor_reply(request_id, result);
                    }
                    AskReply::Write { request_id, path, content } => {
                        if allowed {
                            self.apply_write(request_id, path, content);
                        } else {
                            self.doctor_reply(
                                request_id,
                                Err("The user rejected this change; nothing was written.".into()),
                            );
                        }
                    }
                    AskReply::Form { .. } => unreachable!(),
                }
                // On to the next question still open, if any.
                self.focus_pending_question();
            }
            Hit::Undo(index) => self.undo_change(index),
            Hit::SetMode(mode) => self.set_doctor_mode(mode),
            Hit::OpenMenu(option_id) => {
                let open = self.doctor.menu.as_ref() == Some(&option_id);
                self.doctor.menu = (!open).then_some(option_id);
                self.doctor.menu_filter.clear();
                self.doctor.menu_first = 0;
                // Start on the current value.
                self.doctor.menu_selected = self.menu_values().iter().position(|entry| entry.1).unwrap_or(0);
                self.reveal_menu_selected();
            }
            Hit::SetOption(option_id, value) => {
                self.doctor.menu = None;
                if let Some(option) = self.doctor.options.iter().find(|option| option.option_id == option_id)
                    && option.is_model()
                    && let Some(agent) = self.doctor.agent.clone()
                {
                    self.doctor.models.insert(agent, value.clone());
                    self.save_doctor();
                }
                if let Some(session) = &self.doctor.session {
                    session.send(Request::SetOption { id: option_id, value });
                }
            }
            Hit::Suggestion(index) => self.complete_suggestion(index, true),
            Hit::Open(url) => self.open_link(&url),
            Hit::Detach(index) => {
                if index < self.doctor.attachments.len() {
                    self.doctor.attachments.remove(index);
                }
            }
            Hit::Copy(text) => {
                self.copy_text(text.clone());
                self.doctor.copied = Some((text, Instant::now()));
                // Back from the check to the copy button.
                let timer = smithay::reexports::calloop::timer::Timer::from_duration(COPIED_FOR);
                let _ = self.loop_handle.insert_source(timer, |_, _, state| {
                    state.doctor_changed();
                    smithay::reexports::calloop::timer::TimeoutAction::Drop
                });
            }
            Hit::Run(command) => self.run_code(&command),
            Hit::PopOut(windowed) => self.set_doctor_windowed(windowed),
            Hit::Example(index) => self.send_to_doctor(EXAMPLES[index].to_string()),
            Hit::Stop if self.doctor.busy && self.doctor.stopping.is_some() => self.restart_stuck_agent(),
            Hit::Stop => {
                // Not sent yet: nothing to cancel but the wait.
                if self.doctor.waiting.take().is_some() {
                    self.doctor.busy = false;
                }
                if let Some(session) = &self.doctor.session {
                    session.send(Request::Cancel);
                    // An agent that never ends its turn would keep the panel
                    // busy for ever.
                    if self.doctor.busy {
                        let asked = Instant::now();
                        self.doctor.stopping = Some(asked);
                        let timer = smithay::reexports::calloop::timer::Timer::from_duration(STOP_WAIT);
                        let _ = self.loop_handle.insert_source(timer, move |_, _, state| {
                            if state.doctor.busy && state.doctor.stopping == Some(asked) {
                                state.restart_stuck_agent();
                            }
                            smithay::reexports::calloop::timer::TimeoutAction::Drop
                        });
                    }
                }
                // The agent's own questions and forms are cancelled with the
                // turn; its file requests still need an answer, or it waits
                // for ever.
                self.doctor.form = None;
                let mut unanswered = Vec::new();
                for message in &mut self.doctor.messages {
                    if let Message::Ask { answer: answer @ None, reply, .. } = message {
                        *answer = Some("Stopped".into());
                        if let AskReply::Read { request_id, .. } | AskReply::Write { request_id, .. } = reply {
                            unanswered.push(request_id.clone());
                        }
                    }
                }
                for request_id in unanswered {
                    self.doctor_reply(request_id, Err("The user stopped the agent.".into()));
                }
            }
        }
        self.doctor_changed();
    }

    /// Ends a turn the agent did not stop when asked, by restarting it.
    fn restart_stuck_agent(&mut self) {
        self.doctor.retire_questions();
        self.doctor.stopping = None;
        self.doctor.busy = false;
        self.doctor.approved.clear();
        // A message waiting for the stopped turn goes to the restarted agent
        // once it is ready.
        if let Some(follow_up) = self.doctor.follow_up.take() {
            self.doctor.waiting = Some(follow_up);
            self.doctor.options_ready = false;
            self.doctor.busy = true;
        }
        self.doctor.messages.push(Message::Note(
            "The agent did not stop, so it was restarted. It no longer remembers this conversation.".into(),
        ));
        if let Some(session) = &self.doctor.session {
            session.send(Request::Retry);
        }
        self.doctor.scroll = 0.0;
        self.doctor_changed();
    }

    /// Runs one of the panel's slash commands. False for the agent's own,
    /// which go to the agent as they are.
    fn doctor_command(&mut self, line: &str) -> bool {
        let line = line.trim().trim_start_matches('/');
        let (name, arg) =
            line.split_once(char::is_whitespace).map_or((line, ""), |(command, rest)| (command, rest.trim()));
        let note = |state: &mut Self, text: String| state.doctor.messages.push(Message::Note(text));
        match name {
            "help" => {
                let mut text = String::from("Commands:");
                for (name, hint, what) in COMMANDS {
                    text += &format!("\n/{name} {hint}  {what}");
                }
                if !self.doctor.commands.is_empty() {
                    text += "\n\nFrom the agent:";
                    for command in &self.doctor.commands {
                        text += &format!("\n/{}  {}", command.name, command.description);
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
                            "Mode: {}. {}. Change it with /mode ask, tweak or yolo.",
                            mode.short(),
                            mode.description()
                        ),
                    );
                }
                None => note(self, format!("There is no mode {arg}: use ask, tweak or yolo.")),
            },
            "agent" if arg.is_empty() => self.doctor_hit(Hit::OpenPicker),
            "agent" => {
                let wanted = arg.to_lowercase();
                let found =
                    self.doctor.registry.as_ref().and_then(|registry| registry.as_ref().ok()).and_then(|agents| {
                        agents
                            .iter()
                            .find(|agent| agent.agent_id == wanted || agent.name.to_lowercase() == wanted)
                            .map(|agent| agent.agent_id.clone())
                    });
                match found {
                    Some(agent_id) => self.pick_agent(agent_id),
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
            "report" => {
                let conversation = last_exchange(&self.doctor.messages);
                self.open_feedback(arg, conversation);
            }
            "restart" => {
                self.doctor.retire_questions();
                match &self.doctor.session {
                    Some(session) => session.send(Request::Retry),
                    None => self.ensure_session(),
                }
            }
            _ if self.doctor.commands.iter().any(|command| command.name == name) => return false,
            _ => note(self, format!("There is no command /{name}. /help lists them.")),
        }
        self.doctor.scroll = 0.0;
        self.doctor_changed();
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
        self.doctor.follow_up = None;
        self.doctor.form = None;
        self.doctor.focus = None;
        self.doctor.busy = false;
        if let Some(session) = &self.doctor.session {
            session.send(Request::NewSession);
            self.doctor.restore_model = true;
            self.doctor.options_ready = false;
            self.doctor.agent_mode = None;
            self.doctor.restate_mode = true;
        }
    }

    /// `/model [name]`: the menu, or straight to a model by id or name.
    fn model_command(&mut self, arg: &str) {
        let Some(option) = self.doctor.options.iter().find(|option| option.is_model()).cloned() else {
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
            return self.doctor_hit(Hit::OpenMenu(option.option_id));
        }
        let wanted = arg.to_lowercase();
        let found = option
            .values
            .iter()
            .find(|value| value.value.to_lowercase() == wanted || value.name.to_lowercase() == wanted)
            .or_else(|| option.values.iter().find(|value| value.name.to_lowercase().contains(&wanted)));
        match found {
            Some(value) => {
                self.doctor.messages.push(Message::Note(format!("Model: {}", value.name)));
                self.doctor_hit(Hit::SetOption(option.option_id.clone(), value.value.clone()));
            }
            None => {
                let names: Vec<&str> = option.values.iter().map(|value| value.name.as_str()).collect();
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
        let option = self.doctor.options.iter().find(|option| option.is_model())?;
        let label = model_label(option);
        let width = (self.measure_text(&label, self.toolbar_style(), frame.model.w).0 + 40.0).min(frame.model.w);
        Some((option, Rect::new(frame.model.right() - width, frame.model.y, width, frame.model.h)))
    }

    /// The open menu's values, each with whether it is the current one.
    fn menu_values(&self) -> Vec<(tileception_acp::OptionValue, bool)> {
        let Some(option_id) = &self.doctor.menu else { return Vec::new() };
        let Some(option) = self.doctor.options.iter().find(|option| &option.option_id == option_id) else {
            return Vec::new();
        };
        let filter = self.doctor.menu_filter.text.to_lowercase();
        option
            .values
            .iter()
            .filter(|value| {
                let text =
                    format!("{} {}", value.name, value.description.as_deref().unwrap_or_default()).to_lowercase();
                filter.split_whitespace().all(|word| text.contains(word))
            })
            .map(|value| (value.clone(), value.value == option.current))
            .collect()
    }

    fn choose_menu_value(&mut self, index: usize) {
        let (Some(option_id), Some((value, _))) = (self.doctor.menu.clone(), self.menu_values().get(index).cloned())
        else {
            return;
        };
        self.doctor_hit(Hit::SetOption(option_id, value.value));
    }

    /// Slash commands starting with what is typed: the panel's, then the agent's.
    fn suggestions(&self) -> Vec<Suggestion> {
        let text = &self.doctor.input.text;
        if self.doctor.secret.is_some()
            || self.pending_form_question()
            || !text.starts_with('/')
            || text.contains(char::is_whitespace)
        {
            return Vec::new();
        }
        let typed = text[1..].to_lowercase();
        let ours = COMMANDS.iter().map(|(name, hint, description)| Suggestion {
            name: name.to_string(),
            hint: hint.to_string(),
            description: description.to_string(),
        });
        let theirs =
            self.doctor.commands.iter().filter(|command| !COMMANDS.iter().any(|(name, ..)| *name == command.name)).map(
                |command| Suggestion {
                    name: command.name.clone(),
                    hint: String::new(),
                    description: command.description.clone(),
                },
            );
        ours.chain(theirs).filter(|suggestion| suggestion.name.to_lowercase().starts_with(&typed)).collect()
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
        self.doctor_changed();
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
        let (mut rows, anchor, width): (Vec<MenuRow>, Rect, f64) = match &self.doctor.menu {
            Some(option_id) => {
                let (_, button) = self.model_button(frame)?;
                let rows = self
                    .menu_values()
                    .into_iter()
                    .map(|(value, current)| {
                        row(
                            value.name,
                            value.description.unwrap_or_default(),
                            current,
                            Hit::SetOption(option_id.clone(), value.value),
                        )
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
                    .map(|(index, suggestion)| {
                        let label = format!("/{} {}", suggestion.name, suggestion.hint).trim_end().to_string();
                        row(label, suggestion.description, false, Hit::Suggestion(index))
                    })
                    .collect();
                (rows, frame.field, frame.field.w)
            }
        };
        // A setting's menu keeps a filter row at its foot, even when nothing matches.
        let filter_row = usize::from(self.doctor.menu.is_some());
        if rows.is_empty() && filter_row == 0 {
            return None;
        }
        let room = ((anchor.y - 10.0 - frame.messages.y) / MENU_ROW_H).floor().max(1.0) as usize;
        let limit = if self.doctor.menu.is_some() { MENU_VALUE_ROWS } else { MENU_ROWS };
        let shown = rows.len().min(room.saturating_sub(filter_row).max(1)).min(limit);
        let first = if self.doctor.menu.is_some() {
            self.doctor.menu_first.min(rows.len() - shown)
        } else {
            selected.saturating_sub(shown.saturating_sub(1)).min(rows.len() - shown)
        };
        let height = (shown + filter_row) as f64 * MENU_ROW_H + 8.0;
        let left = (anchor.right() - width).max(frame.field.x);
        let outline = Rect::new(left, anchor.y - 6.0 - height, width, height);
        for (index, row) in rows.iter_mut().enumerate() {
            let place = index as f64 - first as f64;
            row.rect = Rect::new(left + 4.0, outline.y + 4.0 + place * MENU_ROW_H, width - 8.0, MENU_ROW_H);
            row.selected = index == selected;
        }
        let rows = rows.into_iter().skip(first).take(shown).collect();
        Some((outline, rows))
    }

    /// A login, a key being typed or a login in progress is on screen.
    fn login_showing(&self) -> bool {
        self.doctor.unconfirmed.is_some()
            || self.doctor.secret.is_some()
            || matches!(self.doctor.status, Status::NeedsLogin(_) | Status::LoggingIn { .. })
    }

    /// Where the doctor is drawn: the panel along the screen's left edge,
    /// or its whole window, in the window's own coordinates.
    fn doctor_area(&self) -> Rect {
        if self.doctor.windowed {
            return Rect::new(0.0, 0.0, self.doctor.size.0.max(1.0), self.doctor.size.1.max(1.0));
        }
        let screen = self.output_rect();
        Rect::new(screen.x, screen.y, screen.w.clamp(1.0, PANEL_W), screen.h.max(1.0))
    }

    fn doctor_frame(&self) -> Frame {
        let panel = self.doctor_area();
        let hints = Rect::new(panel.x, panel.bottom() - HINTS_H, panel.w, HINTS_H);
        let toolbar = Rect::new(panel.x + PAD, hints.y - TOOLBAR_H, panel.w - 2.0 * PAD, TOOLBAR_H);
        let field = Rect::new(panel.x + PAD, toolbar.y - INPUT_H, panel.w - 2.0 * PAD, INPUT_H);
        let send = Rect::new(field.right() - 40.0, field.y + 6.0, 34.0, field.h - 12.0);
        let text = Rect::new(field.x + 14.0, field.y, send.x - field.x - 20.0, field.h);
        let top = panel.y + HEADER_H + 8.0;
        let size = f64::from(THUMB);
        let attached: Vec<Rect> = (0..self.doctor.attachments.len())
            .map(|index| Rect::new(field.x + index as f64 * (size + 8.0), field.y - size - 10.0, size, size))
            .collect();
        let bottom = attached.first().map_or(field.y, |rect| rect.y);
        // The mode switch at the left of the toolbar, a segment per mode.
        let style = self.toolbar_style();
        let mut left = toolbar.x;
        let modes = Mode::ALL.map(|mode| {
            let segment_width = self.measure_text(mode.short(), style.bold(), 200.0).0 + 24.0;
            let segment = Rect::new(left, toolbar.y + 6.0, segment_width, toolbar.h - 12.0);
            left += segment_width;
            segment
        });
        let model_x = left + 12.0;
        Frame {
            panel,
            title: Rect::new(panel.x + 18.0, panel.y + 12.0, 90.0, 32.0),
            agent: Rect::new(panel.x + 110.0, panel.y + 10.0, panel.w - 162.0, 36.0),
            pop: Rect::new(panel.right() - 48.0, panel.y + 10.0, 36.0, 36.0),
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

    /// The agent's button in the header, as wide as its label: only it opens
    /// the picker, so the rest of the header is free (a window moves by it).
    fn agent_button(&self, frame: &Frame) -> Rect {
        let style = Style::new(self.theme.font_size, self.theme.text);
        let (label_width, _) = self.measure_text(&self.status_line().0, style, frame.agent.w - 48.0);
        Rect::new(frame.agent.right() - label_width - 46.0, frame.agent.y, label_width + 46.0, frame.agent.h)
    }

    pub(crate) fn measure_text(&self, text: &str, style: Style, width: f64) -> (f64, f64) {
        let theme = &self.theme;
        self.text
            .borrow_mut()
            .styled_size(text, &theme.font, style.size, style.colour, width.max(1.0) as f32, style.weight, style.lines)
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
        let mut top = area.y;
        let width = area.w;
        let left = area.x;
        let push_note = |blocks: &mut Vec<Block>, top: &mut f64, text: &str| {
            let (_, height) = self.measure_text(text, self.note_style(), width - 24.0);
            blocks.push(Block::Note {
                rect: Rect::new(left + 12.0, *top, width - 24.0, height + 12.0),
                text: text.into(),
            });
            *top += height + 12.0 + GAP;
        };
        if self.doctor.view == View::Picker {
            blocks
                .push(Block::Heading { rect: Rect::new(left + 4.0, top, width, 24.0), text: "CHOOSE AN AGENT".into() });
            top += 28.0;
            match &self.doctor.registry {
                None => push_note(&mut blocks, &mut top, "Loading the ACP Registry\u{2026}"),
                Some(Err(error)) => {
                    push_note(&mut blocks, &mut top, error);
                    blocks.push(Block::Button {
                        rect: Rect::new(left + width / 2.0 - 60.0, top, 120.0, BUTTON_H),
                        label: "Try again".into(),
                        hit: Some(Hit::ReloadRegistry),
                        primary: true,
                    });
                }
                Some(Ok(agents)) => {
                    for agent in agents {
                        let missing = self.doctor.missing.get(&agent.agent_id).cloned();
                        let detail = self.agent_source(agent).unwrap_or_else(|| {
                            if agent.description.is_empty() {
                                agent.kind().to_string()
                            } else {
                                agent.description.clone()
                            }
                        });
                        blocks.push(Block::Row {
                            rect: Rect::new(left, top, width, ROW_H),
                            title: agent.name.clone(),
                            detail,
                            note: missing.clone(),
                            hit: missing.is_none().then(|| Hit::Pick(agent.agent_id.clone())),
                            current: self.doctor.agent.as_ref() == Some(&agent.agent_id),
                        });
                        top += ROW_H + 4.0;
                    }
                    push_note(&mut blocks, &mut top, "From the official ACP Registry. Agents bring their own login.");
                }
            }
            return blocks;
        }

        let back = |blocks: &mut Vec<Block>, top: &mut f64, logged_in: bool| {
            let mut button_x = left + width / 2.0 - if logged_in { 140.0 } else { 60.0 };
            if logged_in {
                blocks.push(Block::Button {
                    rect: Rect::new(button_x, *top, 160.0, BUTTON_H),
                    label: "I've logged in".into(),
                    hit: Some(Hit::LoggedIn),
                    primary: true,
                });
                button_x += 168.0;
            }
            blocks.push(Block::Button {
                rect: Rect::new(button_x, *top, 112.0, BUTTON_H),
                label: "Back".into(),
                hit: Some(Hit::BackToLogins),
                primary: false,
            });
            *top += BUTTON_H + 2.0 * GAP;
        };
        if let Some(Unconfirmed { agent_id, path }) = &self.doctor.unconfirmed {
            let name = self.chosen_agent().map_or_else(|| agent_id.clone(), |agent| agent.name);
            blocks
                .push(Block::Heading { rect: Rect::new(left + 4.0, top, width, 24.0), text: "INSTALLED AGENT".into() });
            top += 28.0;
            let text =
                format!("{} is on this computer. Is it {name}? Another program can have the same name.", tilde(path));
            push_note(&mut blocks, &mut top, &text);
            let version = self.chosen_agent().map(|agent| agent.version).unwrap_or_default();
            for (title, detail, choice) in [
                ("Use the installed one".to_string(), tilde(path), true),
                (format!("Download {name} {version}"), "From the ACP Registry, kept by Tileception".into(), false),
            ] {
                blocks.push(Block::Row {
                    rect: Rect::new(left, top, width, ROW_H),
                    title,
                    detail,
                    note: None,
                    hit: Some(Hit::UseInstalled(choice)),
                    current: false,
                });
                top += ROW_H + 4.0;
            }
            top += GAP;
        } else if let Some(secret) = &self.doctor.secret {
            let label = secret.labels.get(secret.values.len()).map_or("key", String::as_str);
            blocks.push(Block::Heading { rect: Rect::new(left + 4.0, top, width, 24.0), text: "LOG IN".into() });
            top += 28.0;
            let text = format!("Paste the {label} into the field below and press Enter. Tileception does not save it.");
            push_note(&mut blocks, &mut top, &text);
            back(&mut blocks, &mut top, false);
        } else if let Status::LoggingIn { terminal } = self.doctor.status {
            blocks.push(Block::Heading { rect: Rect::new(left + 4.0, top, width, 24.0), text: "LOGGING IN".into() });
            top += 28.0;
            let text = if terminal {
                "Finish logging in in the terminal that opened, then choose \"I've logged in\"."
            } else {
                "Finish logging in where the agent sent you, usually your browser."
            };
            push_note(&mut blocks, &mut top, text);
            back(&mut blocks, &mut top, terminal);
        } else if let Status::NeedsLogin(logins) = &self.doctor.status {
            blocks.push(Block::Heading { rect: Rect::new(left + 4.0, top, width, 24.0), text: "LOG IN".into() });
            top += 28.0;
            for Login { method_id, name, description, how } in logins {
                let detail =
                    description.clone().filter(|detail_text| !detail_text.trim().is_empty()).unwrap_or_else(|| {
                        match how {
                            LoginHow::Terminal(_) => "Opens a terminal",
                            LoginHow::Key | LoginHow::Env(_) => "Asks for an API key",
                            LoginHow::Agent => "The agent opens its own login",
                        }
                        .into()
                    });
                blocks.push(Block::Row {
                    rect: Rect::new(left, top, width, ROW_H),
                    title: name.clone(),
                    detail,
                    note: None,
                    hit: Some(Hit::Login(method_id.clone())),
                    current: false,
                });
                top += ROW_H + 4.0;
            }
            if logins.iter().any(|login| matches!(login.how, LoginHow::Terminal(_))) {
                blocks.push(Block::Button {
                    rect: Rect::new(left + width / 2.0 - 80.0, top + 4.0, 160.0, BUTTON_H),
                    label: "I've logged in".into(),
                    hit: Some(Hit::LoggedIn),
                    primary: false,
                });
                top += BUTTON_H + 4.0 + GAP;
            }
            top += GAP;
        }

        if self.doctor.messages.is_empty() && !self.login_showing() {
            for (index, example) in EXAMPLES.iter().enumerate() {
                blocks.push(Block::Row {
                    rect: Rect::new(left + 16.0, top + 150.0 + index as f64 * 40.0, width - 32.0, 32.0),
                    title: (*example).into(),
                    detail: String::new(),
                    note: None,
                    hit: Some(Hit::Example(index)),
                    current: false,
                });
            }
            return blocks;
        }

        for (index, message) in self.doctor.messages.iter().enumerate() {
            match message {
                Message::User(text) => {
                    let (text_width, text_height) = self.measure_text(text, self.body_style(), width - 64.0);
                    let bubble_width = text_width + 28.0;
                    blocks.push(Block::Bubble {
                        rect: Rect::new(left + width - bubble_width, top, bubble_width, text_height + 20.0),
                        text: text.clone(),
                        user: true,
                    });
                    top += text_height + 20.0 + GAP;
                }
                Message::Attached(thumbs) => {
                    let size = f64::from(THUMB);
                    let strip_width = thumbs.len() as f64 * (size + 6.0) - 6.0;
                    blocks.push(Block::Thumbs {
                        rect: Rect::new(left + width - strip_width, top, strip_width, size),
                        thumbs: thumbs.clone(),
                    });
                    top += size + 6.0;
                }
                Message::Agent(text) => {
                    top = self.layout_answer(&mut blocks, text, Rect::new(left, top, width, 0.0)) + GAP
                }
                Message::Note(text) => push_note(&mut blocks, &mut top, text),
                Message::Update { name, running, latest, outside, done } => {
                    let text = match outside {
                        None if *done => format!("{name} {latest} is available; this was {running}."),
                        None => format!(
                            "{name} {latest} is available; this is {running}. Wizard updates it when the agent next \
                             starts, or now with the button."
                        ),
                        Some(Outside::Installed(path)) => format!(
                            "{name} {latest} is available; this is {running}, from {}. You installed that copy \
                             yourself, so update it the way you installed it.",
                            tilde(path)
                        ),
                        Some(Outside::Configured) => format!(
                            "{name} {latest} is available; this is {running}, started by the command in your \
                             config. Update it the way you installed it."
                        ),
                    };
                    push_note(&mut blocks, &mut top, &text);
                    if !*done {
                        blocks.push(Block::Button {
                            rect: Rect::new(left + width / 2.0 - 70.0, top, 140.0, BUTTON_H),
                            label: "Update now".into(),
                            hit: outside.is_none().then_some(Hit::Update(index)),
                            primary: outside.is_none(),
                        });
                        top += BUTTON_H + GAP;
                    }
                }
                Message::Info(text) => {
                    let (_, text_height) = self.measure_text(text, self.body_style(), width - 28.0);
                    blocks.push(Block::Bubble {
                        rect: Rect::new(left, top, width, text_height + 20.0),
                        text: text.clone(),
                        user: false,
                    });
                    top += text_height + 20.0 + GAP;
                }
                Message::Tool { title, status, .. } => {
                    blocks.push(Block::Tool {
                        rect: Rect::new(left, top, width, 30.0),
                        title: title.clone(),
                        status: status.clone(),
                    });
                    top += 30.0 + 4.0;
                }
                Message::Changed { path, added, removed, undone, .. } => {
                    let name = path.file_name().map_or_else(
                        || path.display().to_string(),
                        |file_name| file_name.to_string_lossy().into_owned(),
                    );
                    let text = if *undone {
                        format!("Changed {name}: undone")
                    } else {
                        format!("Changed {name} (+{added} \u{2212}{removed}), with a backup")
                    };
                    blocks.push(Block::Tool {
                        rect: Rect::new(left, top, width - 84.0, 30.0),
                        title: text,
                        status: "completed".into(),
                    });
                    if !*undone {
                        blocks.push(Block::Button {
                            rect: Rect::new(left + width - 78.0, top, 78.0, 30.0),
                            label: "Undo".into(),
                            hit: Some(Hit::Undo(index)),
                            primary: false,
                        });
                    }
                    top += 30.0 + 4.0;
                }
                Message::Ask { reply, title, choices, diff, answer } => {
                    let style = self.body_style();
                    let (_, text_height) = self.measure_text(title, style, width - 28.0);
                    blocks.push(Block::Bubble {
                        rect: Rect::new(left, top, width, text_height + 20.0),
                        text: title.clone(),
                        user: false,
                    });
                    top += text_height + 20.0 + 6.0;
                    if !diff.is_empty() && answer.is_none() {
                        let shown: Vec<DiffLine> = diff.iter().take(DIFF_LINES).cloned().collect();
                        let height = shown.len() as f64 * DIFF_LINE_H
                            + 12.0
                            + if diff.len() > DIFF_LINES { DIFF_LINE_H } else { 0.0 };
                        blocks.push(Block::Diff { rect: Rect::new(left, top, width, height), lines: diff.clone() });
                        top += height + 6.0;
                    }
                    match answer {
                        Some(answer) => push_note(&mut blocks, &mut top, &format!("You chose: {answer}")),
                        None => {
                            let count = choices.len().max(1) as f64;
                            let button_width = (width - 8.0 * (count - 1.0)) / count;
                            for (choice_index, choice) in choices.iter().enumerate() {
                                blocks.push(Block::Button {
                                    rect: Rect::new(
                                        left + choice_index as f64 * (button_width + 8.0),
                                        top,
                                        button_width,
                                        BUTTON_H,
                                    ),
                                    label: choice.name.clone(),
                                    hit: Some(Hit::Answer(index, Some(choice.choice_id.clone()))),
                                    primary: choice.kind.starts_with("allow") && choice_index == 0,
                                });
                            }
                            if choices.is_empty() {
                                blocks.push(Block::Button {
                                    rect: Rect::new(left, top, width, BUTTON_H),
                                    label: "Decline".into(),
                                    hit: Some(Hit::Answer(index, None)),
                                    primary: false,
                                });
                            }
                            top += BUTTON_H + GAP;
                            // How to answer it from the keyboard.
                            if self.pending_ask() == Some(index) {
                                let focused =
                                    matches!(&self.doctor.focus, Some(Hit::Answer(asked, _)) if *asked == index);
                                let form = matches!(reply, AskReply::Form { .. });
                                if form && !focused {
                                    push_note(
                                        &mut blocks,
                                        &mut top,
                                        "Enter submits your answer, Shift+Tab or Up moves to the buttons",
                                    );
                                } else {
                                    push_note(&mut blocks, &mut top, &ask_hint(choices.len().max(1), focused));
                                }
                                if form && let Some(error) = self.doctor.form.as_ref().and_then(|form| form.error) {
                                    push_note(&mut blocks, &mut top, error);
                                }
                            }
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
            blocks.push(Block::Dots { rect: Rect::new(left, top, 64.0, 30.0) });
        }
        blocks
    }

    /// How answers look: `scale` times the body size, wrapped to as many
    /// lines as fit the conversation (only whole blocks are drawn).
    fn rich_style(&self, scale: f32, weight: u16) -> RichStyle {
        let theme = &self.theme;
        let size = theme.font_size * 1.08 * scale;
        let line = f64::from((size * 1.4).ceil());
        let room = self.doctor_frame().messages.h - 20.0;
        let lines = (room / line).floor().clamp(1.0, f64::from(MAX_LINES)) as u8;
        RichStyle { size, colour: theme.text, accent: theme.accent, weight, lines, mono: false }
    }

    fn rich_image(&self, spans: &[Span], style: RichStyle, width: f64) -> Option<RichImage> {
        self.text.borrow_mut().rich(spans, &self.theme.font, style, width as f32)
    }

    /// The size `rich_image` would have, without drawing it: laying out the
    /// whole conversation measures every answer, but draws only those shown.
    fn rich_size(&self, spans: &[Span], style: RichStyle, width: f64) -> Option<(f64, f64)> {
        self.text.borrow_mut().rich_size(spans, &self.theme.font, style, width as f32)
    }

    /// An answer's markdown, laid out from the top of `area` across its
    /// width, a block per paragraph, item, code block or picture. Returns
    /// where it ends.
    fn layout_answer(&self, blocks: &mut Vec<Block>, text: &str, area: Rect) -> f64 {
        let (left, width) = (area.x, area.w);
        let mut top = area.y;
        let body = self.rich_style(1.0, 400);
        let pieces = markdown::parse(text);
        for (index, piece) in pieces.iter().enumerate() {
            let after_item = index > 0 && matches!(pieces[index - 1], Piece::Text { kind: Kind::Item { .. }, .. });
            match piece {
                Piece::Text { kind, spans } => {
                    let (style, indent, marker, quote) = match kind {
                        Kind::Heading(level) => {
                            if index > 0 {
                                top += 4.0;
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
                                top -= PIECE_GAP / 2.0;
                            }
                            (body, 2.0 + f64::from(*depth) * 18.0, Some(marker.as_str()), false)
                        }
                        Kind::Quote => (RichStyle { colour: self.theme.text.alpha(0.75), ..body }, 14.0, None, true),
                        Kind::Paragraph => (body, 0.0, None, false),
                    };
                    let gutter = if marker.is_some() { 20.0 } else { 0.0 };
                    let rect = Rect::new(left + 2.0 + indent + gutter, top, width - 2.0 - indent - gutter, 0.0);
                    let Some((_, height)) = self.rich_size(spans, style, rect.w) else { continue };
                    let line = f64::from((style.size * 1.4).ceil());
                    let marker = marker
                        .filter(|bullet| !bullet.is_empty())
                        .map(|bullet| (Rect::new(left + 2.0 + indent, top, gutter, line), bullet.to_string()));
                    blocks.push(Block::Rich {
                        rect: Rect { h: height, ..rect },
                        spans: spans.clone(),
                        style,
                        marker,
                        quote,
                    });
                    top += height + PIECE_GAP;
                }
                Piece::Code { lang, text } => {
                    let style = RichStyle { size: body.size * 0.9, mono: true, ..body };
                    let spans = vec![Span::plain(text.clone())];
                    let Some((_, text_height)) = self.rich_size(&spans, style, width - 24.0) else { continue };
                    let height = CODE_BAR + text_height + 18.0;
                    let run = markdown::shell_command(lang, text);
                    let copy = run.clone().unwrap_or_else(|| text.clone());
                    let rect = Rect::new(left, top, width, height);
                    blocks.push(Block::Code { rect, spans, style, lang: lang.clone(), copy, run });
                    top += height + PIECE_GAP;
                }
                Piece::Image { url, alt, link } => {
                    let open = link.clone().unwrap_or_else(|| url.clone());
                    match self.doctor.pictures.get(url) {
                        Some(Picture::Failed) => {
                            let name = if alt.trim().is_empty() { url.clone() } else { alt.clone() };
                            let spans = vec![Span::plain("Picture: "), Span { link: Some(open), ..Span::plain(name) }];
                            let Some((_, height)) = self.rich_size(&spans, body, width - 2.0) else { continue };
                            let rect = Rect::new(left + 2.0, top, width - 2.0, height);
                            blocks.push(Block::Rich { rect, spans, style: body, marker: None, quote: false });
                            top += height + PIECE_GAP;
                        }
                        picture => {
                            let (picture_width, picture_height) = match picture {
                                Some(Picture::Ready(thumb)) => {
                                    let (thumb_width, thumb_height) =
                                        (f64::from(thumb.size.0), f64::from(thumb.size.1));
                                    let scale = (width / thumb_width).min(1.0);
                                    (thumb_width * scale, thumb_height * scale)
                                }
                                _ => (width, 120.0),
                            };
                            blocks.push(Block::Picture {
                                rect: Rect::new(left, top, picture_width, picture_height),
                                url: url.clone(),
                                open,
                            });
                            top += picture_height + PIECE_GAP;
                        }
                    }
                }
                Piece::Rule => {
                    blocks.push(Block::Rule { rect: Rect::new(left, top + 4.0, width, 1.0) });
                    top += 9.0 + PIECE_GAP;
                }
            }
        }
        if top > area.y { top - PIECE_GAP } else { top }
    }

    fn doctor_content_height(&self, area: Rect) -> f64 {
        let blocks = self.doctor_content(area);
        blocks.iter().map(|block| block.rect().bottom()).fold(area.y, f64::max) - area.y
    }

    /// The middle, scrolled into place: the picker from the top, the
    /// conversation stuck to its newest message. Only whole blocks show.
    fn doctor_blocks(&self, area: Rect) -> Vec<Block> {
        self.doctor_blocks_indexed(area).into_iter().map(|(_, block)| block).collect()
    }

    /// [`Self::doctor_blocks`] with each block's place in `doctor_content`.
    fn doctor_blocks_indexed(&self, area: Rect) -> Vec<(usize, Block)> {
        let mut blocks: Vec<(usize, Block)> = self.doctor_content(area).into_iter().enumerate().collect();
        let height = blocks.iter().map(|(_, block)| block.rect().bottom()).fold(area.y, f64::max) - area.y;
        let shift = match self.doctor.view {
            View::Picker => -self.doctor.scroll,
            // Taller than the panel: the newest message sits at the bottom.
            View::Conversation if !self.doctor.messages.is_empty() => (area.h - height).min(0.0) + self.doctor.scroll,
            View::Conversation => 0.0,
        };
        for (_, block) in &mut blocks {
            block.rect_mut().y += shift;
        }
        blocks.retain(|(_, block)| {
            let rect = block.rect();
            rect.y >= area.y - 0.5 && rect.bottom() <= area.bottom() + 0.5
        });
        blocks
    }

    fn status_line(&self) -> (String, Colour) {
        let theme = &self.theme;
        let name = self.chosen_agent().map(|agent| agent.name).or_else(|| self.doctor.agent.clone());
        match (&name, &self.doctor.status) {
            (None, _) => ("Choose an agent".into(), theme.muted),
            (Some(name), _) if self.doctor.session.is_none() => (name.clone(), theme.muted),
            (Some(name), Status::Ready) => (name.clone(), theme.accent),
            (Some(name), Status::NeedsLogin(_) | Status::LoggingIn { .. }) => {
                (format!("{name} \u{b7} log in"), theme.warning)
            }
            (Some(_), Status::Starting(text)) => (text.clone(), theme.warning),
            (Some(name), Status::Failed(_)) => (format!("{name} \u{b7} stopped"), theme.danger),
        }
    }

    /// The panel along the left edge, on the screen it is open on.
    pub fn doctor_elements(&self, renderer: &mut GlesRenderer) -> Vec<OutputElement> {
        if !self.doctor_here() {
            return Vec::new();
        }
        self.doctor_view_elements(renderer)
    }

    /// The doctor as the panel or as its window: the same drawing in the
    /// area of either.
    fn doctor_view_elements(&self, renderer: &mut GlesRenderer) -> Vec<OutputElement> {
        let theme = &self.theme;
        let frame = self.doctor_frame();
        let pointer = self.doctor_pointer();
        let mut painter = Painter::new(self, renderer);
        if self.doctor.windowed {
            painter.fill(frame.panel, theme.popup(), 0.0);
        } else {
            // Slides in from the left edge.
            let shown = ease_out(self.doctor.opened.elapsed().as_secs_f64() / 0.2);
            painter.alpha = shown as f32;
            painter.offset = ((shown - 1.0) * 24.0, 0.0);
            painter.edge_panel(frame.panel, true);
        }
        // Into a window of its own, or back to the edge.
        let pop = if self.doctor.windowed { glyph::ARROW_LEFT } else { glyph::ARROW_UP_RIGHT };
        painter.button(frame.pop, Button::glyph(pop, Look::Quiet).round(), frame.pop.contains(pointer));

        painter.text("Wizard", frame.title, Style::new(theme.font_size * 1.3, theme.text).bold());
        // The agent, as a button that opens the picker.
        let (label, dot) = self.status_line();
        let style = Style::new(theme.font_size, theme.text);
        let button = self.agent_button(&frame);
        let label_width = button.w - 46.0;
        if button.contains(pointer) {
            painter.fill(button, theme.surface_raised, theme.radius);
        }
        painter.fill(Rect::new(button.x + 12.0, button.y + button.h / 2.0 - 4.0, 8.0, 8.0), dot, 4.0);
        painter.text(&label, Rect::new(button.x + 26.0, button.y, label_width + 1.0, button.h), style);
        let chevron = if self.doctor.view == View::Picker { glyph::CHEVRON_DOWN } else { glyph::CHEVRON_RIGHT };
        painter.glyph(chevron, Rect::new(button.right() - 20.0, button.y, 16.0, button.h), 12, theme.muted);
        painter.fill(
            Rect::new(frame.panel.x + 12.0, frame.panel.y + HEADER_H, frame.panel.w - 24.0, 1.0),
            theme.border,
            0.0,
        );

        let area = frame.messages;
        let empty = self.doctor.view == View::Conversation && self.doctor.messages.is_empty() && !self.login_showing();
        if empty {
            let mid = area.y + 40.0;
            painter.fill(Rect::new(area.x + area.w / 2.0 - 32.0, mid, 64.0, 64.0), theme.surface_raised, 32.0);
            painter.glyph(glyph::CHAT, Rect::new(area.x, mid, area.w, 64.0), 28, theme.accent);
            painter.text(
                "Ask about your setup",
                Rect::new(area.x, mid + 76.0, area.w, 24.0),
                Style::new(theme.font_size * 1.1, theme.text).bold().align(Align::Centre),
            );
        }
        let selection = self.doctor_selection();
        // The keyboard's place, shown while the doctor has the keyboard.
        let focus = self.doctor_focus().filter(|_| self.doctor_has_keys());
        for (index, block) in self.doctor_blocks_indexed(area) {
            let selected = |len| Self::selected_in(selection, index, len);
            self.draw_block(&mut painter, &block, pointer, selected, focus.as_ref());
        }
        self.draw_toolbar(&mut painter, &frame, pointer);
        // Pictures waiting to go with the next message; a click takes one off.
        for (attachment, slot) in self.doctor.attachments.iter().zip(&frame.attached) {
            painter.image_sized(&attachment.thumb.buffer, (slot.x, slot.y), Some((THUMB, THUMB)));
            let hover = slot.contains(pointer);
            let border = if hover { theme.accent.alpha(0.6) } else { theme.border };
            painter.shape(*slot, Shape { border_width: 1.0, border, ..Shape::filled(Colour::TRANSPARENT, 2.0) });
            if hover {
                let close = Rect::new(slot.right() - 22.0, slot.y + 2.0, 20.0, 20.0);
                painter.fill(close, Colour([0.0, 0.0, 0.0, 0.6]), 10.0);
                painter.glyph(glyph::CLOSE, close, 10, theme.text);
            }
        }

        // Where typing goes, glowing while it has the keyboard (not while
        // the keyboard is on a button or row).
        let focused = self.doctor_has_keys() && focus.is_none();
        painter.shape(
            frame.field,
            Shape {
                border_width: 1.0,
                border: if focused { theme.accent.alpha(0.45) } else { theme.border },
                glow_radius: if focused { theme.glow * 0.5 } else { 0.0 },
                glow: theme.accent.alpha(0.10),
                ..Shape::filled(theme.background, theme.radius)
            },
        );
        let input = &self.doctor.input;
        let size = theme.font_size * 1.08;
        let secret = self.doctor.secret.as_ref().and_then(|entry| entry.labels.get(entry.values.len()));
        if input.text.is_empty() {
            let hint = match secret {
                Some(label) => format!("Paste the {label}"),
                None if self.pending_form_question() => "Or type your answer".into(),
                None if self.pending_ask().is_some() => "Or say what to do instead".into(),
                None if self.doctor.busy => "Answering...".into(),
                None => "Ask about your setup, or / for commands".into(),
            };
            painter.text(&hint, frame.text, Style::new(size, theme.muted));
        }
        // A key shows as stars, one per byte so the cursor stays in place.
        let shown = if secret.is_some() { "*".repeat(input.text.len()) } else { input.text.clone() };
        let field = self.text.borrow_mut().field(
            &shown,
            &theme.font,
            size,
            theme.text,
            frame.text.w as f32,
            crate::text::Field {
                cursor: input.cursor,
                anchor: input.anchor,
                // The window is drawn when it changes rather than to blink.
                visible: focused && (self.doctor.windowed || self.doctor.edited.elapsed().as_millis() % 1000 < 600),
                accent: theme.accent.straight_rgba8(),
            },
        );
        if let Some(image) = field {
            painter.image(&image.buffer, (frame.text.x, frame.text.y + (frame.text.h - image.height) / 2.0));
        }
        if self.doctor.busy && !self.doctor_redirects() {
            // While answering, the button stops it.
            // Once asked, pressing it again restarts the agent.
            let look = if self.doctor.stopping.is_some() { Look::Warning } else { Look::Plain };
            painter.button(frame.send, Button::glyph(glyph::STOP, look), frame.send.contains(pointer));
        } else {
            let can_send = !input.text.trim().is_empty() || !self.doctor.attachments.is_empty();
            painter.button(
                frame.send,
                Button::glyph(glyph::ARROW_RIGHT, Look::Primary).enabled(can_send),
                frame.send.contains(pointer),
            );
        }

        let hints = frame.hints;
        painter.fill(Rect::new(hints.x + 1.0, hints.y, hints.w - 2.0, 1.0), theme.border, 0.0);
        // Over the mode switch, what the mode means instead of keys.
        if let Some(warning) = doctor_mode_warning(&self.doctor) {
            let text = Rect::new(hints.x + 16.0, hints.y, hints.w - 32.0, hints.h);
            painter.text(warning, text, Style::new(theme.font_size * 0.9, theme.warning).lines(1));
        } else if let Some(index) = frame.modes.iter().position(|rect| rect.contains(pointer)) {
            let text = Rect::new(hints.x + 16.0, hints.y, hints.w - 32.0, hints.h);
            painter.text(Mode::ALL[index].description(), text, Style::new(theme.font_size * 0.9, theme.muted));
        } else {
            let keys: &[(&str, &str)] = if self.doctor.menu.is_some() {
                &[("Type", "Filter"), ("\u{2191}\u{2193}", "Choose"), ("Enter", "Pick"), ("Esc", "Close")]
            } else if focus.as_ref().is_some_and(|hit| matches!(hit, Hit::Answer(..))) {
                &[("Enter", "Choose"), ("\u{2190}\u{2192}", "Move"), ("Type", "Instead"), ("Esc", "Message")]
            } else if focus.is_some() {
                &[("Enter", "Choose"), ("\u{2191}\u{2193}", "Move"), ("Esc", "Message")]
            } else if self.doctor_redirects() {
                &[("Enter", "Decline and send"), ("Shift+Tab", "Answer"), ("Esc", "Close")]
            } else if !self.suggestions().is_empty() {
                &[("\u{2191}\u{2193}", "Choose"), ("Tab", "Fill in"), ("Enter", "Run")]
            } else if self.doctor.secret.is_some() {
                &[("Ctrl+V", "Paste"), ("Enter", "Log in"), ("Esc", "Cancel")]
            } else if self.doctor.busy && self.doctor.stopping.is_some() {
                &[("Ctrl+C", "Force stop"), ("PgUp", "Earlier"), ("Esc", "Close")]
            } else if self.doctor.busy {
                &[("Ctrl+C", "Stop"), ("PgUp", "Earlier"), ("Esc", "Close")]
            } else {
                &[("Enter", "Send"), ("/", "Commands"), ("PgUp", "Earlier"), ("Esc", "Close")]
            };
            painter.key_hints(keys, hints.x + 16.0, hints.y + hints.h / 2.0, false);
        }
        // Menus lie over everything else.
        if let Some((outline, rows)) = self.menu_layout(&frame) {
            painter.popup(outline);
            if self.doctor.menu.is_some() {
                self.draw_menu_filter(&mut painter, outline);
            }
            for row in rows {
                let hover = row.rect.contains(pointer);
                if row.selected {
                    painter.fill(row.rect, theme.accent.alpha(0.14), theme.radius);
                } else if hover {
                    painter.fill(row.rect, theme.surface_raised, theme.radius);
                }
                let colour = if row.current { theme.accent } else { theme.text };
                // The name takes the whole row when there is no description.
                let room = if row.detail.is_empty() { row.rect.w - 46.0 } else { row.rect.w * 0.6 };
                let label_w = painter.measure(&row.label, room, Style::new(theme.font_size, colour).bold()).0;
                painter.text(
                    &row.label,
                    Rect::new(row.rect.x + 10.0, row.rect.y, label_w + 1.0, row.rect.h),
                    Style::new(theme.font_size, colour).bold(),
                );
                let detail_x = row.rect.x + 22.0 + label_w;
                let detail = Rect::new(detail_x, row.rect.y, row.rect.right() - 10.0 - detail_x, row.rect.h);
                painter.text(&row.detail, detail, Style::new(theme.font_size * 0.88, theme.muted).lines(1));
                if row.current {
                    painter.glyph(
                        glyph::CHECK,
                        Rect::new(row.rect.right() - 26.0, row.rect.y, 20.0, row.rect.h),
                        12,
                        theme.accent,
                    );
                }
            }
        }
        painter.finish()
    }

    /// The foot of a setting's menu, the part of it the filter is typed in,
    /// and how many values match.
    fn menu_filter_layout(&self, outline: Rect) -> (Rect, Rect, String) {
        let row = Rect::new(outline.x + 4.0, outline.bottom() - 4.0 - MENU_ROW_H, outline.w - 8.0, MENU_ROW_H);
        let all = self
            .doctor
            .options
            .iter()
            .find(|option| Some(&option.option_id) == self.doctor.menu.as_ref())
            .map_or(0, |option| option.values.len());
        let matching = self.menu_values().len();
        let count =
            if self.doctor.menu_filter.text.is_empty() { format!("{all}") } else { format!("{matching} of {all}") };
        let count_w = self
            .text
            .borrow_mut()
            .styled(
                &count,
                &self.theme.font,
                self.theme.font_size * 0.88,
                self.theme.muted,
                (row.w / 3.0).max(1.0) as f32,
                400,
                0,
            )
            .map_or(0.0, |rendered| rendered.width);
        (row, Rect::new(row.x + 30.0, row.y, row.w - 50.0 - count_w, row.h), count)
    }

    /// The foot of a setting's menu: what is typed to filter it, and how
    /// many values match.
    fn draw_menu_filter(&self, painter: &mut Painter, outline: Rect) {
        let theme = &self.theme;
        let (row, text, count) = self.menu_filter_layout(outline);
        painter.fill(Rect::new(row.x + 6.0, row.y, row.w - 12.0, 1.0), theme.border, 0.0);
        painter.glyph(glyph::SEARCH, Rect::new(row.x + 4.0, row.y, 20.0, row.h), 12, theme.muted);
        let count_style = Style::new(theme.font_size * 0.88, theme.muted);
        let count_x = text.right() + 10.0;
        painter.text(&count, Rect::new(count_x, row.y, row.right() - 9.0 - count_x, row.h), count_style);
        let filter = &self.doctor.menu_filter;
        if filter.text.is_empty() {
            painter.text("Type to filter", text, Style::new(theme.font_size, theme.muted).lines(1));
        }
        // Drawn like the other fields: caret, selection and scrolling.
        let field = self.text.borrow_mut().field(
            &filter.text,
            &theme.font,
            theme.font_size,
            theme.text,
            text.w as f32,
            crate::text::Field {
                cursor: filter.cursor,
                anchor: filter.anchor,
                visible: self.doctor.edited.elapsed().as_millis() % 1000 < 600,
                accent: theme.accent.straight_rgba8(),
            },
        );
        if let Some(image) = field {
            painter.image(&image.buffer, (text.x, text.y + (text.h - image.height) / 2.0));
        }
    }

    /// Under the field: the permission mode as a switch, and the model.
    fn draw_toolbar(&self, painter: &mut Painter, frame: &Frame, pointer: Point) {
        let theme = &self.theme;
        let style = self.toolbar_style();
        let whole =
            Rect::new(frame.modes[0].x, frame.modes[0].y, frame.modes[2].right() - frame.modes[0].x, frame.modes[0].h);
        painter.shape(
            whole,
            Shape { border_width: 1.0, border: theme.border, ..Shape::filled(theme.surface, (whole.h / 2.0) as f32) },
        );
        for (mode, rect) in Mode::ALL.into_iter().zip(frame.modes) {
            let colour = match mode {
                Mode::Ask => theme.text,
                Mode::Config => theme.accent,
                Mode::Yolo => theme.warning,
            };
            let chosen = mode == self.doctor.mode;
            if chosen {
                painter.shape(
                    rect,
                    Shape {
                        border_width: 1.0,
                        border: colour.alpha(0.5),
                        ..Shape::filled(colour.alpha(0.16), (rect.h / 2.0) as f32)
                    },
                );
            } else if rect.contains(pointer) {
                painter.fill(rect, theme.surface_raised, (rect.h / 2.0) as f32);
            }
            let style = if chosen { style.bold().colour(colour) } else { style.colour(theme.muted) };
            painter.text(mode.short(), rect, style.align(Align::Centre));
        }
        match self.model_button(frame) {
            Some((option, button)) => {
                let open = self.doctor.menu.as_ref() == Some(&option.option_id);
                if open || button.contains(pointer) {
                    painter.fill(button, theme.surface_raised, theme.radius);
                }
                let label = model_label(option);
                painter.text(&label, Rect::new(button.x + 10.0, button.y, button.w - 34.0, button.h), style.lines(1));
                let chevron = if open { glyph::CHEVRON_DOWN } else { glyph::CHEVRON_UP };
                painter.glyph(chevron, Rect::new(button.right() - 24.0, button.y, 16.0, button.h), 12, theme.muted);
            }
            // Running, but with no choice of model to offer.
            None if matches!(self.doctor.status, Status::Ready) => {
                let text = "Agent's default model";
                let width = self.measure_text(text, style, frame.model.w).0;
                let rect = Rect::new(frame.model.right() - width - 10.0, frame.model.y, width + 1.0, frame.model.h);
                painter.text(text, rect, style.colour(theme.muted));
            }
            None => {}
        }
    }

    /// `selected` gives the selected bytes of the block's text, from its length.
    fn draw_block(
        &self,
        painter: &mut Painter,
        block: &Block,
        pointer: Point,
        selected: impl Fn(usize) -> Option<(usize, usize)>,
        focus: Option<&Hit>,
    ) {
        let theme = &self.theme;
        // Over a block's fill, under its text.
        let shade = |painter: &mut Painter| {
            if let Some((origin, image)) = self.block_text(block) {
                self.draw_selected(painter, selected(image.text.len()), origin, &image);
            }
        };
        match block {
            Block::Bubble { rect, text, user } => {
                let shape = if *user {
                    Shape {
                        radius: theme.popup_radius,
                        fill: (theme.accent.alpha(0.18), theme.accent.alpha(0.18)),
                        ..Default::default()
                    }
                } else {
                    Shape {
                        border_width: 1.0,
                        border: theme.border,
                        ..Shape::filled(theme.surface_raised, theme.popup_radius)
                    }
                };
                painter.shape(*rect, shape);
                shade(painter);
                painter.text(
                    text,
                    Rect::new(rect.x + 14.0, rect.y + 10.0, rect.w - 27.0, rect.h - 20.0),
                    self.body_style(),
                );
            }
            Block::Note { rect, text } => {
                shade(painter);
                painter.text(text, *rect, self.note_style());
            }
            Block::Tool { rect, title, status } => {
                let (icon, colour) = match status.as_str() {
                    "completed" => (glyph::CHECK, theme.accent),
                    "failed" => (glyph::CLOSE, theme.danger),
                    _ => (glyph::RESTART, theme.warning),
                };
                painter.fill(*rect, theme.surface, theme.radius);
                painter.glyph(icon, Rect::new(rect.x + 4.0, rect.y, 26.0, rect.h), 13, colour);
                painter.text(
                    title,
                    Rect::new(rect.x + 32.0, rect.y, rect.w - 40.0, rect.h),
                    Style::new(theme.font_size * 0.95, theme.text.alpha(0.75)).lines(1),
                );
            }
            Block::Heading { rect, text } => {
                painter.text(text, *rect, Style::new(theme.font_size * 0.85, theme.muted).bold());
            }
            Block::Row { rect, title, detail, note, hit, current } => {
                let hover = hit.is_some() && rect.contains(pointer);
                let focused = hit.is_some() && hit.as_ref() == focus;
                let fill = if *current {
                    theme.accent.alpha(0.14)
                } else if hover {
                    theme.surface_raised
                } else {
                    theme.surface
                };
                // The keyboard's ring, as on a focused button.
                let (border_width, border) = if focused {
                    (2.0, theme.accent)
                } else if *current {
                    (1.0, theme.accent.alpha(0.4))
                } else {
                    (1.0, theme.border)
                };
                painter.shape(*rect, Shape { border_width, border, ..Shape::filled(fill, theme.radius) });
                let enabled = hit.is_some();
                let colour = if enabled { theme.text } else { theme.muted };
                if detail.is_empty() {
                    painter.text(
                        title,
                        Rect::new(rect.x + 12.0, rect.y, rect.w - 24.0, rect.h),
                        Style::new(theme.font_size, if hover || focused { theme.text } else { theme.text.alpha(0.7) }),
                    );
                    return;
                }
                let note_w = note.as_ref().map_or(0.0, |note_text| {
                    painter.measure(note_text, 200.0, Style::new(theme.font_size * 0.78, theme.warning)).0 + 12.0
                });
                painter.text(
                    title,
                    Rect::new(rect.x + 14.0, rect.y + 8.0, rect.w - 28.0 - note_w, 22.0),
                    Style::new(theme.font_size, colour).bold(),
                );
                painter.text(
                    detail,
                    Rect::new(rect.x + 14.0, rect.y + 29.0, rect.w - 28.0, 18.0),
                    Style::new(theme.font_size * 0.9, theme.text.alpha(0.65)).lines(1),
                );
                if let Some(note) = note {
                    let style = Style::new(theme.font_size * 0.78, theme.warning);
                    painter.text(note, Rect::new(rect.right() - note_w - 2.0, rect.y + 8.0, note_w, 22.0), style);
                }
            }
            Block::Button { rect, label, primary, hit } => {
                let look = if *primary { Look::Primary } else { Look::Plain };
                let focused = hit.is_some() && focus == hit.as_ref();
                let button = Button::new(label, look).focused(focused).enabled(hit.is_some());
                painter.button(*rect, button, hit.is_some() && rect.contains(pointer));
            }
            Block::Diff { rect, lines } => {
                painter.shape(
                    *rect,
                    Shape { border_width: 1.0, border: theme.border, ..Shape::filled(theme.background, theme.radius) },
                );
                let size = theme.font_size * 0.82;
                for (index, line) in lines.iter().take(DIFF_LINES).enumerate() {
                    let row =
                        Rect::new(rect.x + 8.0, rect.y + 6.0 + index as f64 * DIFF_LINE_H, rect.w - 16.0, DIFF_LINE_H);
                    let (mark, colour) = match line.sign {
                        Sign::Added => ("+", theme.accent),
                        Sign::Removed => ("\u{2212}", theme.danger),
                        Sign::Same => (" ", theme.muted),
                        Sign::Skipped => ("\u{22ef}", theme.muted.alpha(0.6)),
                    };
                    if line.sign == Sign::Added {
                        painter.fill(row, theme.accent.alpha(0.08), 2.0);
                    } else if line.sign == Sign::Removed {
                        painter.fill(row, theme.danger.alpha(0.08), 2.0);
                    }
                    painter.text(mark, Rect::new(row.x + 2.0, row.y, 12.0, row.h), Style::new(size, colour).bold());
                    painter.text(
                        &line.text,
                        Rect::new(row.x + 16.0, row.y, row.w - 18.0, row.h),
                        Style::new(size, colour),
                    );
                }
                if lines.len() > DIFF_LINES {
                    let more = format!("and {} more lines", lines.len() - DIFF_LINES);
                    let row = Rect::new(
                        rect.x + 24.0,
                        rect.y + 6.0 + DIFF_LINES as f64 * DIFF_LINE_H,
                        rect.w - 32.0,
                        DIFF_LINE_H,
                    );
                    painter.text(&more, row, Style::new(size, theme.muted));
                }
            }
            Block::Rich { rect, spans, style, marker, quote } => {
                if let Some((marker_rect, marker)) = marker {
                    painter.text(marker, *marker_rect, Style::new(style.size, theme.muted).align(Align::Centre));
                }
                if *quote {
                    painter.fill(
                        Rect::new(rect.x - 12.0, rect.y + 2.0, 3.0, rect.h - 4.0),
                        theme.accent.alpha(0.5),
                        1.5,
                    );
                }
                if let Some(image) = self.rich_image(spans, *style, rect.w) {
                    let hover = image.links.iter().any(|([left, top, width, height], _)| {
                        Rect::new(
                            rect.x + f64::from(*left),
                            rect.y + f64::from(*top),
                            f64::from(*width),
                            f64::from(*height),
                        )
                        .contains(pointer)
                    });
                    if hover {
                        for ([left, top, width, height], _) in &image.links {
                            let link = Rect::new(
                                rect.x + f64::from(*left),
                                rect.y + f64::from(*top),
                                f64::from(*width),
                                f64::from(*height),
                            );
                            if link.contains(pointer) {
                                painter.fill(link, theme.accent.alpha(0.14), 3.0);
                            }
                        }
                    }
                    shade(painter);
                    painter.image(&image.image.buffer, (rect.x, rect.y));
                }
            }
            Block::Code { rect, spans, style, lang, copy, run } => {
                painter.shape(
                    *rect,
                    Shape { border_width: 1.0, border: theme.border, ..Shape::filled(theme.background, theme.radius) },
                );
                painter.fill(Rect::new(rect.x + 1.0, rect.y + CODE_BAR, rect.w - 2.0, 1.0), theme.border, 0.0);
                let small = Style::new(theme.font_size * 0.8, theme.muted).lines(1);
                painter.text(lang, Rect::new(rect.x + 12.0, rect.y, rect.w - 80.0, CODE_BAR), small);
                let (copy_at, run_at) = code_buttons(*rect, run.is_some());
                let copied = self
                    .doctor
                    .copied
                    .as_ref()
                    .is_some_and(|(text, since)| text == copy && since.elapsed() < COPIED_FOR);
                let buttons = [
                    Some((copy_at, if copied { glyph::CHECK } else { glyph::COPY }, copied)),
                    run_at.map(|run_button| (run_button, glyph::PLAY, false)),
                ];
                for (button_rect, icon, done) in buttons.into_iter().flatten() {
                    let hover = button_rect.contains(pointer);
                    if hover {
                        painter.fill(button_rect, theme.surface_raised, theme.radius);
                    }
                    let colour = if done {
                        theme.accent
                    } else if hover {
                        theme.text
                    } else {
                        theme.muted
                    };
                    painter.glyph(icon, button_rect, 14, colour);
                }
                shade(painter);
                if let Some(image) = self.rich_image(spans, *style, rect.w - 24.0) {
                    painter.image(&image.image.buffer, (rect.x + 12.0, rect.y + CODE_BAR + 9.0));
                }
            }
            Block::Picture { rect, url, .. } => {
                let hover = rect.contains(pointer);
                match self.doctor.pictures.get(url) {
                    Some(Picture::Ready(thumb)) => {
                        painter.image_sized(&thumb.buffer, (rect.x, rect.y), Some((rect.w as i32, rect.h as i32)));
                        let border = if hover { theme.accent.alpha(0.6) } else { theme.border };
                        painter.shape(
                            *rect,
                            Shape { border_width: 1.0, border, ..Shape::filled(Colour::TRANSPARENT, 2.0) },
                        );
                    }
                    _ => {
                        painter.shape(
                            *rect,
                            Shape {
                                border_width: 1.0,
                                border: theme.border,
                                ..Shape::filled(theme.surface, theme.radius)
                            },
                        );
                        let style = Style::new(theme.font_size * 0.9, theme.muted).align(Align::Centre);
                        painter.text("Loading picture...", *rect, style);
                    }
                }
            }
            Block::Rule { rect } => painter.fill(*rect, theme.border, 0.0),
            Block::Thumbs { rect, thumbs } => {
                let size = f64::from(THUMB);
                for (index, thumb) in thumbs.iter().enumerate() {
                    let slot = Rect::new(rect.x + index as f64 * (size + 6.0), rect.y, size, size);
                    painter.image_sized(&thumb.buffer, (slot.x, slot.y), Some((THUMB, THUMB)));
                    painter.shape(
                        slot,
                        Shape { border_width: 1.0, border: theme.border, ..Shape::filled(Colour::TRANSPARENT, 2.0) },
                    );
                }
            }
            Block::Dots { rect } => {
                let phase = (self.doctor.edited.elapsed().as_secs_f64() * 3.0) as usize % 3;
                painter.shape(
                    *rect,
                    Shape {
                        border_width: 1.0,
                        border: theme.border,
                        ..Shape::filled(theme.surface_raised, theme.popup_radius)
                    },
                );
                for dot in 0..3 {
                    let colour = if dot == phase { theme.accent } else { theme.muted.alpha(0.6) };
                    painter.fill(Rect::new(rect.x + 17.0 + dot as f64 * 11.0, rect.y + 12.0, 6.0, 6.0), colour, 3.0);
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
    std::env::var("TILECEPTION_DOCTOR_TEST_AGENT").ok().filter(|command| !command.trim().is_empty())
}

/// `text` as one word for `sh`.
fn shell_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
}

fn state_dir() -> PathBuf {
    std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".local/state"))
        .join("tileception")
}

/// Refresh the version-matched guides without adding their contents to prompts.
fn export_doctor_docs() -> std::io::Result<()> {
    export_docs_to(&state_dir())
}

/// The README, changelog and guides under `root`, laid out as in the source
/// tree so their links between each other keep working.
fn export_docs_to(root: &std::path::Path) -> std::io::Result<()> {
    let docs = root.join("docs");
    std::fs::create_dir_all(&docs)?;
    let readme = include_str!("../../../README.md")
        .replace("(crates/tileception/src/default-config.toml)", "(default-config.toml)");
    write_if_changed(&root.join("README.md"), &readme)?;
    write_if_changed(&root.join("CHANGELOG.md"), include_str!("../../../CHANGELOG.md"))?;
    for (name, content) in [
        ("usage.md", include_str!("../../../docs/usage.md")),
        ("configuration.md", include_str!("../../../docs/configuration.md")),
        ("plugins.md", include_str!("../../../docs/plugins.md")),
        ("installation.md", include_str!("../../../docs/installation.md")),
        ("iso.md", include_str!("../../../docs/iso.md")),
        ("greeter.md", include_str!("../../../docs/greeter.md")),
        ("wizard.md", include_str!("../../../docs/wizard.md")),
        ("troubleshooting.md", include_str!("../../../docs/troubleshooting.md")),
        ("development.md", include_str!("../../../docs/development.md")),
        ("index.md", include_str!("../../../docs/index.md")),
    ] {
        let content = content.replace("(../crates/tileception/src/default-config.toml)", "(../default-config.toml)");
        write_if_changed(&docs.join(name), &content)?;
    }
    // The old bundled guide was renamed to wizard.md.
    match std::fs::remove_file(docs.join("doctor.md")) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        result => result,
    }
}

/// Keep exported guides' timestamps stable when the bundled contents match.
fn write_if_changed(path: &std::path::Path, content: &str) -> std::io::Result<()> {
    match std::fs::read(path) {
        Ok(previous) if previous == content.as_bytes() => Ok(()),
        Ok(_) => std::fs::write(path, content),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => std::fs::write(path, content),
        Err(error) => Err(error),
    }
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
    let text = std::fs::read_to_string(path).map_err(|err| format!("{}: {err}", path.display()))?;
    Ok(serde_json::json!({ "content": doctor_policy::read_lines(&text, line, limit) }))
}

/// OpenCode, under any of its registry ids (`opencode`, `opencode-v2`): the
/// same program, which takes its permission rules from the environment.
/// A path with the home folder shortened to ~.
fn tilde(path: &std::path::Path) -> String {
    let home = PathBuf::from(std::env::var_os("HOME").unwrap_or_default());
    match path.strip_prefix(&home) {
        Ok(rest) if !home.as_os_str().is_empty() => format!("~/{}", rest.display()),
        _ => path.display().to_string(),
    }
}

fn is_opencode(agent: Option<&str>) -> bool {
    agent.is_some_and(|agent_id| agent_id == "opencode" || agent_id.starts_with("opencode-"))
}

fn opencode_permission(mode: Mode) -> &'static str {
    if mode == Mode::Yolo { r#"{"edit":"allow","bash":"allow"}"# } else { r#"{"edit":"ask","bash":"ask"}"# }
}

/// V1 reads OPENCODE_PERMISSION; V2 migrates the same keys from inline config.
/// Merge only this child process's rules without changing the user's files.
/// Codex through codex-acp, by any registry id.
fn is_codex(agent: Option<&str>) -> bool {
    agent.is_some_and(|agent_id| agent_id == "codex" || agent_id.starts_with("codex-"))
}

/// The config codex-acp hands Codex (`CODEX_CONFIG`, JSON), from `inherited`
/// with Codex's ask tool switched on outside its Plan mode too, so the agent
/// can ask questions with choices. A setting of the user's own is kept.
fn codex_config(inherited: Option<&str>) -> Result<String, String> {
    let mut config: serde_json::Value =
        serde_json::from_str(inherited.unwrap_or("{}")).map_err(|_| "CODEX_CONFIG is not valid JSON".to_string())?;
    let features = config
        .as_object_mut()
        .ok_or("CODEX_CONFIG is not an object")?
        .entry("features")
        .or_insert_with(|| serde_json::json!({}))
        .as_object_mut()
        .ok_or("CODEX_CONFIG's features are not an object")?;
    features.entry("default_mode_request_user_input").or_insert(serde_json::Value::Bool(true));
    Ok(config.to_string())
}

fn opencode_env(mode: Mode) -> Result<tileception_acp::Env, String> {
    let inherited = std::env::var("OPENCODE_CONFIG_CONTENT").unwrap_or_else(|_| "{}".into());
    let mut content: serde_json::Value = serde_json::from_str(&inherited)
        .map_err(|_| "Cannot apply OpenCode permissions: inline config is not valid JSON".to_string())?;
    let object = content
        .as_object_mut()
        .ok_or_else(|| "Cannot apply OpenCode permissions: inline config is not an object".to_string())?;
    let mut permission = match object.remove("permission") {
        Some(serde_json::Value::Object(value)) => value,
        Some(serde_json::Value::String(value)) => serde_json::Map::from_iter([("*".into(), value.into())]),
        None => serde_json::Map::new(),
        Some(_) => return Err("Cannot apply OpenCode permissions: invalid inline permission rules".into()),
    };
    let effect = if mode == Mode::Yolo { "allow" } else { "ask" };
    for action in ["edit", "bash"] {
        // Reinsert at the end for OpenCode's last-match-wins permission order.
        permission.remove(action);
        permission.insert(action.into(), effect.into());
    }
    object.insert("permission".into(), permission.into());
    if let Some(native) = object.get_mut("permissions") {
        let rules = native
            .as_array_mut()
            .ok_or_else(|| "Cannot apply OpenCode permissions: invalid native permission rules".to_string())?;
        for action in ["edit", "shell"] {
            rules.push(serde_json::json!({ "action": action, "resource": "*", "effect": effect }));
        }
    }
    Ok(std::collections::BTreeMap::from([
        ("OPENCODE_PERMISSION".into(), opencode_permission(mode).into()),
        ("OPENCODE_CONFIG_CONTENT".into(), content.to_string()),
    ]))
}

/// Only use known permission ids actually advertised by this agent.
fn doctor_mode_target(doctor: &Doctor) -> Option<(&SessionOption, &str)> {
    let agent = doctor.agent.as_deref()?;
    let candidates: &[&str] = if agent.contains("codex") {
        if doctor.mode == Mode::Yolo { &["agent-full-access", "full-access"] } else { &["read-only"] }
    } else if agent.contains("claude") {
        if doctor.mode == Mode::Yolo { &["bypassPermissions"] } else { &["default"] }
    } else {
        return None;
    };
    let option = doctor.options.iter().find(|option| option.category.as_deref() == Some("mode"))?;
    let value = candidates.iter().find_map(|candidate| option.values.iter().find(|value| value.value == *candidate))?;
    Some((option, &value.value))
}

fn doctor_mode_warning(doctor: &Doctor) -> Option<&str> {
    if let Some(warning) = &doctor.mode_env_warning {
        return Some(warning);
    }
    if !doctor.options_ready || is_opencode(doctor.agent.as_deref()) {
        return None;
    }
    match doctor_mode_target(doctor) {
        Some((option, target)) if option.current != target => Some("Agent permission mode is not confirmed yet"),
        Some(_) => None,
        None if doctor.mode == Mode::Yolo => {
            Some("Agent offers no known full-access mode; its restrictions may remain")
        }
        None => Some("Agent offers no known ask-first mode; its own tools may act without asking"),
    }
}

/// Sent with the first question, so the agent knows what it is helping with.
fn doctor_context(mode: &str) -> String {
    let config = crate::config::path();
    let state = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".local/state"));
    format!(
        "You are the Wizard inside Tileception, a zoomable, nesting tiling Wayland compositor. You help the user \
         with Tileception's settings, shortcuts and default apps, in plain language.\n\
         - The configuration is {config} (TOML). Tileception reloads it as soon as it is saved. Its comments \
         document every option; `tileception msg reload` reports errors.\n\
         - `tileception msg <command>` runs a compositor command, for example `tree` (the layout) or `volume`. \
         `tileception msg help` lists every command; features that are switched on and off rather than \
         configured, such as the rubber duck (`duck`), are only there.\n\
         - Plugins: `tileception msg plugins reload` rescans installed plugin manifests and restarts plugins \
         without restarting Tileception or changing config.toml. Use it after installing or changing a manifest. \
         The launcher query `plugins` shows discovery results and errors. To set up a plugin the user asks \
         for, run `tileception msg plugin add [--id <id>] [--bar] <command> [args...]`, which writes its \
         `[plugins.<id>]` entry to config.toml and so starts it; plugins.md explains the id and refusals.\n\
         - Smart home devices (Tuya lights, plugs, air conditioners, sensors): `tileception msg home` lists them \
         with their state, and `tileception msg home why <name>` explains why one is unreachable and what to check. \
         `tileception msg home on|off|toggle <name>`, `home brightness <name> <percent>`, `home target <name> <degrees>`, \
         `home mode <name> cool|heat|dry|fan|auto` control them; `tileception home status` connects for a few seconds \
         and lists what answers. The panel's Home block is switched on with `[home] enabled = true` in the config, \
         `tileception config set home.enabled true` or `tileception msg home enable`. To set up devices or fetch their \
         local keys again, the user opens Settings > Smart home (`tileception msg settings` opens Settings), which \
         walks through the Tuya project, imports and lists the devices, and has Refresh devices and switches for \
         reading cloud sensors and refreshing at startup. In a terminal, `tileception home import` does the same \
         import: it asks for the Tuya IoT Platform project's Access ID, data center and Access Secret (`--client-id`, `--region`, `--secret-file` make it \
         unattended; `--cloud-sensors`/`--no-cloud-sensors` choose whether battery sensors are read from the cloud \
         every 10 minutes). Devices behind a Zigbee or Bluetooth gateway are not \
         supported yet and show as unavailable.\n\
         - Bluetooth: `tileception msg bluetooth` lists adapters and paired devices with their state, and says \
         what is wrong when Bluetooth cannot be used (bluetooth.service not running, no adapter, blocked by \
         rfkill, switched off). `bluetooth on|off`, `connect|disconnect|forget <name>`, `search` and \
         `pair <name>` act on it. Failed connections show a notification with the reason; sound output follows \
         the sound server (PipeWire/WirePlumber).\n\
         - Updates: `tileception msg updates` lists what waits (APT, Flatpak, Snap, firmware). \
         `tileception msg updates history since YYYY-MM-DD` lists what apt updated, installed and removed from \
         that day on, with versions, newest first; use it for questions such as \"what changed since Tuesday?\", \
         working out the date with `date` first. `updates history` alone shows the latest ten transactions.\n\
         - Every option with its default and an explanation is in {defaults}.\n\
         - Tileception's documentation index is {docs}. Read it to find relevant guides when needed. \
         These guides match this binary and are available without a source checkout.\n\
         - {version} What changed in each version, and when, is in {changelog}, newest first.\n\
         - The log is {log}.\n\
         - Read and write files with the file tools the client offers, so the user sees diffs and gets backups. \
         The user's permission mode is \"{mode}\".\n\
         Keep changes small, say what you change, and keep the file's comments. Answer briefly.",
        config = config.display(),
        log = state.join("tileception/tileception.log").display(),
        defaults = state.join("tileception/default-config.toml").display(),
        docs = state.join("tileception/docs/index.md").display(),
        changelog = state.join("tileception/CHANGELOG.md").display(),
        version = running_version(env!("CARGO_PKG_VERSION")),
        mode = mode,
    )
}

/// Which Tileception this is, for the agent. A prerelease such as `1.2.0-rc.1`
/// has no entry in the changelog, which leaves prereleases out until the
/// release itself.
fn running_version(version: &str) -> String {
    match version.split_once('-') {
        Some((release, _)) => format!(
            "This is Tileception {version}, a test release of {release}; its changes may not be in the \
             changelog yet."
        ),
        None => format!("This is Tileception {version}."),
    }
}

/// Premultiplied, as the renderer wants pictures.
fn premultiplied(image: image::RgbaImage) -> Pixels {
    let (width, height) = image.dimensions();
    let mut raw = image.into_raw();
    for pixel in raw.as_chunks_mut::<4>().0 {
        let alpha = u32::from(pixel[3]);
        for channel in &mut pixel[..3] {
            *channel = (u32::from(*channel) * alpha / 255) as u8;
        }
    }
    (raw, width, height)
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
    let (max_width, max_height) = ((PANEL_W - 2.0 * PAD) as u32, PICTURE_H);
    let image = if image.width() > max_width || image.height() > max_height {
        image.thumbnail(max_width, max_height)
    } else {
        image
    };
    Some(premultiplied(image.to_rgba8()))
}

/// A pasted picture as a PNG small enough to send, and its thumbnail.
fn prepare_attachment(bytes: &[u8]) -> Result<(Vec<u8>, Pixels), String> {
    use image::imageops::FilterType;
    let image = image::load_from_memory(bytes).map_err(|err| format!("That picture could not be read: {err}"))?;
    let image = if image.width().max(image.height()) > SEND_SIZE {
        image.resize(SEND_SIZE, SEND_SIZE, FilterType::Triangle)
    } else {
        image
    };
    let mut png = Vec::new();
    image
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .map_err(|err| format!("That picture could not be prepared: {err}"))?;
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
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since_epoch| since_epoch.as_millis());
    let mut text = text;
    for (index, attachment) in attachments.iter().enumerate() {
        let path = dir.join(format!("pasted-{stamp}-{index}.png"));
        match std::fs::create_dir_all(&dir).and_then(|_| std::fs::write(&path, &attachment.png)) {
            Ok(()) => text += &format!("\n\n[The user attached a picture: {}]", path.display()),
            Err(error) => tracing::warn!("{}: {error}", path.display()),
        }
    }
    text
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test setup and assertions should fail immediately on unexpected errors")]
mod tests {
    use super::*;

    const PLAIN: Mods = Mods { logo: false, alt: false, ctrl: false, shift: false };
    const SHIFT: Mods = Mods { logo: false, alt: false, ctrl: false, shift: true };
    const CTRL: Mods = Mods { logo: false, alt: false, ctrl: true, shift: false };

    #[test]
    fn keys_move_between_and_press_a_questions_answers() {
        let key = |sym, mods, current| focus_key(sym, mods, current, 3, true);
        // Around the three answers, both ways, wrapping at the ends.
        assert_eq!(key(keysyms::KEY_Right, PLAIN, 0), FocusKey::Move(1));
        assert_eq!(key(keysyms::KEY_Tab, PLAIN, 2), FocusKey::Move(0));
        assert_eq!(key(keysyms::KEY_Left, PLAIN, 0), FocusKey::Move(2));
        assert_eq!(key(keysyms::KEY_Tab, SHIFT, 1), FocusKey::Move(0));
        assert_eq!(key(keysyms::KEY_ISO_Left_Tab, SHIFT, 1), FocusKey::Move(0));
        assert_eq!(key(keysyms::KEY_Down, PLAIN, 1), FocusKey::Move(2));
        // Enter and Space press the focused one; digits pick by number.
        assert_eq!(key(keysyms::KEY_Return, PLAIN, 1), FocusKey::Choose(1));
        assert_eq!(key(keysyms::KEY_KP_Enter, PLAIN, 2), FocusKey::Choose(2));
        assert_eq!(key(keysyms::KEY_space, PLAIN, 0), FocusKey::Choose(0));
        assert_eq!(key(keysyms::KEY_2, PLAIN, 0), FocusKey::Choose(1));
        assert_eq!(key(keysyms::KEY_KP_3, PLAIN, 0), FocusKey::Choose(2));
        assert_eq!(key(keysyms::KEY_4, PLAIN, 0), FocusKey::Keep);
        // Esc goes back to the message field without answering.
        assert_eq!(key(keysyms::KEY_Escape, PLAIN, 1), FocusKey::Leave);
        // Typing goes on in the field; the shift held for it changes nothing.
        assert_eq!(key(keysyms::KEY_a, PLAIN, 0), FocusKey::Pass);
        assert_eq!(key(keysyms::KEY_Shift_L, PLAIN, 0), FocusKey::Keep);
        assert_eq!(key(keysyms::KEY_c, CTRL, 0), FocusKey::Pass);
        assert_eq!(key(keysyms::KEY_Page_Up, PLAIN, 0), FocusKey::Scroll(-4.0));
    }

    #[test]
    fn digits_are_typed_on_a_list_but_pick_a_questions_answer() {
        assert_eq!(focus_key(keysyms::KEY_1, PLAIN, 2, 5, false), FocusKey::Pass);
        assert_eq!(focus_key(keysyms::KEY_1, PLAIN, 2, 5, true), FocusKey::Choose(0));
        assert_eq!(focus_key(keysyms::KEY_Up, PLAIN, 0, 5, false), FocusKey::Move(4));
        assert_eq!(focus_key(keysyms::KEY_End, PLAIN, 0, 5, false), FocusKey::Move(4));
    }

    #[test]
    fn the_field_hands_the_keyboard_to_what_is_above_it() {
        // A question: to its first, main answer.
        assert_eq!(focus_entry(keysyms::KEY_ISO_Left_Tab, SHIFT, false, 2, true), Some(0));
        assert_eq!(focus_entry(keysyms::KEY_Up, PLAIN, true, 2, true), Some(0));
        // Up only while nothing is typed, so the caret still moves in text.
        assert_eq!(focus_entry(keysyms::KEY_Up, PLAIN, false, 2, true), None);
        assert_eq!(focus_entry(keysyms::KEY_Down, PLAIN, true, 2, true), None);
        // A list: up to its end nearest the field, or down from its top.
        assert_eq!(focus_entry(keysyms::KEY_Up, PLAIN, true, 4, false), Some(3));
        assert_eq!(focus_entry(keysyms::KEY_Tab, PLAIN, true, 4, false), Some(0));
        assert_eq!(focus_entry(keysyms::KEY_Down, PLAIN, true, 4, false), Some(0));
        assert_eq!(focus_entry(keysyms::KEY_Tab, PLAIN, false, 4, false), None);
        // Nothing to go to, or a key the field keeps.
        assert_eq!(focus_entry(keysyms::KEY_Up, PLAIN, true, 0, false), None);
        assert_eq!(focus_entry(keysyms::KEY_a, PLAIN, true, 3, false), None);
        assert_eq!(focus_entry(keysyms::KEY_Up, CTRL, true, 3, false), None);
    }

    #[test]
    fn a_message_instead_of_an_answer_rejects_once() {
        let choice = |choice_id: &str, kind: &str| Choice {
            choice_id: choice_id.into(),
            name: choice_id.into(),
            kind: kind.into(),
        };
        let choices = [
            choice("yes", "allow_once"),
            choice("always", "allow_always"),
            choice("never", "reject_always"),
            choice("no", "reject_once"),
        ];
        assert_eq!(reject_choice(&choices), Some("no".into()));
        assert_eq!(reject_choice(&choices[..3]), None);
        // No way to say no: declined.
        assert_eq!(reject_choice(&choices[..2]), None);
        assert_eq!(reject_choice(&[]), None);
    }

    #[test]
    fn the_question_says_how_to_answer_it() {
        for (buttons, focused) in [(1, true), (2, true), (12, true), (2, false)] {
            let hint = ask_hint(buttons, focused);
            assert!(hint.is_ascii(), "{hint}");
        }
        assert!(ask_hint(3, true).contains("1-3 pick"));
        assert!(ask_hint(12, true).contains("1-9 pick"));
        assert!(ask_hint(1, true).contains("1 picks"));
        assert!(ask_hint(2, false).contains("Shift+Tab"));
    }

    #[test]
    fn reports_the_last_question_and_answer() {
        assert_eq!(last_exchange(&[Message::Note("Connected".into())]), None);
        let messages = [
            Message::User("old".into()),
            Message::Agent("old answer".into()),
            Message::User("Why is Wi-Fi off?".into()),
            Message::Agent("Checking.".into()),
            Message::Tool { tool_id: "1".into(), title: "nmcli".into(), status: "completed".into() },
            Message::Agent("Airplane mode is on.".into()),
        ];
        assert_eq!(
            last_exchange(&messages).expect("a question was asked"),
            "Last Wizard question:\nWhy is Wi-Fi off?\n\nWizard's answer:\nChecking.\n\nAirplane mode is on.\n"
        );
        let long = [Message::User("é".repeat(REPORT_QUESTION + 5))];
        let text = last_exchange(&long).expect("a question was asked");
        assert!(text.contains(&format!("{}\n[shortened]", "é".repeat(REPORT_QUESTION))));
        assert!(!text.contains("answer"));
    }

    #[test]
    fn exports_the_changelog_beside_the_guides() {
        let root = std::env::temp_dir().join(format!("tileception-doctor-docs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        export_docs_to(&root).unwrap();
        let changelog = std::fs::read_to_string(root.join("CHANGELOG.md")).unwrap();
        assert_eq!(changelog, include_str!("../../../CHANGELOG.md"));
        // A prerelease (1.2.0-rc.1) gets no entry of its own: vommit leaves
        // prereleases out of the changelog.
        let version = env!("CARGO_PKG_VERSION");
        if !version.contains('-') {
            assert!(changelog.contains(&format!("## v{version}")));
        }
        // Every guide the index and README point to was exported with them.
        for (file, text) in [
            (root.join("docs/index.md"), std::fs::read_to_string(root.join("docs/index.md")).unwrap()),
            (root.join("README.md"), std::fs::read_to_string(root.join("README.md")).unwrap()),
        ] {
            let folder = file.parent().unwrap();
            for link in text.split("](").skip(1).filter_map(|rest| rest.split(')').next()) {
                let target = link.split('#').next().unwrap();
                if target.ends_with(".md") && target != "PLAN.md" {
                    assert!(folder.join(target).is_file(), "{} links to missing {target}", file.display());
                }
            }
        }
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn exports_only_missing_or_changed_files() {
        let root = std::env::temp_dir().join(format!("tileception-doctor-unchanged-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        export_docs_to(&root).unwrap();
        let (kept, edited, removed) = (root.join("docs/usage.md"), root.join("README.md"), root.join("CHANGELOG.md"));
        let bundled = std::fs::read_to_string(&edited).unwrap();
        let old = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000_000);
        std::fs::File::options().write(true).open(&kept).unwrap().set_modified(old).unwrap();
        std::fs::write(&edited, "changed by hand").unwrap();
        std::fs::remove_file(&removed).unwrap();
        export_docs_to(&root).unwrap();
        assert_eq!(std::fs::metadata(&kept).unwrap().modified().unwrap(), old, "an unchanged file was rewritten");
        assert_eq!(std::fs::read_to_string(&edited).unwrap(), bundled);
        assert_eq!(std::fs::read_to_string(&removed).unwrap(), include_str!("../../../CHANGELOG.md"));
        // A guide no longer shipped goes; other files are left alone.
        let (stale, other) = (root.join("docs/doctor.md"), root.join("docs/notes.txt"));
        std::fs::write(&stale, "the old name").unwrap();
        std::fs::write(&other, "mine").unwrap();
        export_docs_to(&root).unwrap();
        assert!(!stale.exists());
        assert!(other.exists());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn tells_the_agent_about_the_changelog() {
        let context = doctor_context("Ask");
        assert!(context.contains("tileception/CHANGELOG.md"));
        assert!(context.contains(env!("CARGO_PKG_VERSION")));
    }

    #[test]
    fn says_when_this_is_a_test_release() {
        assert_eq!(running_version("1.2.0"), "This is Tileception 1.2.0.");
        assert_eq!(
            running_version("1.2.0-rc.1"),
            "This is Tileception 1.2.0-rc.1, a test release of 1.2.0; its changes may not be in the changelog yet."
        );
    }

    #[test]
    fn knows_opencode_by_any_registry_id() {
        assert!(is_opencode(Some("opencode")));
        assert!(is_opencode(Some("opencode-v2")));
        assert!(!is_opencode(Some("opencoder")));
        assert!(is_codex(Some("codex-acp")));
        assert!(!is_codex(Some("codexy")));
        assert_eq!(codex_config(None).unwrap(), r#"{"features":{"default_mode_request_user_input":true}}"#);
        // The user's own choice and other settings stay.
        let own = r#"{"features":{"default_mode_request_user_input":false},"model":"x"}"#;
        assert_eq!(codex_config(Some(own)).unwrap(), own);
        assert!(codex_config(Some("[]")).is_err());
        assert!(!is_opencode(Some("claude-acp")));
        assert!(!is_opencode(None));
    }
}
