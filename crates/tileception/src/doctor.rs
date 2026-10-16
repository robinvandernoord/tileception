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
    backend::renderer::gles::GlesRenderer,
    input::keyboard::{keysyms, xkb},
    reexports::calloop::{channel, RegistrationToken},
};
use tileception_acp::{Agent, Choice, Event, Login, Request, Session, Status};
use tileception_core::{Point, Rect};

use crate::{
    config::Mods,
    doctor_policy::{self, DiffLine, Mode, Scope, Sign},
    render::OutputElement,
    shapes::Shape,
    text_input::TextInput,
    theme::Colour,
    ui::{ease_out, glyph, Align, Painter, Style},
    Tileception,
};

const PANEL_W: f64 = 440.0;
const HEADER_H: f64 = 56.0;
const INPUT_H: f64 = 46.0;
const HINTS_H: f64 = 34.0;
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

/// One entry in the conversation.
#[derive(Debug, Clone)]
pub enum Message {
    User(String),
    Agent(String),
    /// From Tileception itself: errors, what is connected.
    Note(String),
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
    Answer(usize, Option<String>),
    Undo(usize),
    CycleMode,
    Example(usize),
    Stop,
}

/// A laid-out piece of the panel's middle.
enum Block {
    Bubble { rect: Rect, text: String, user: bool },
    Note { rect: Rect, text: String },
    Tool { rect: Rect, title: String, status: String },
    Heading { rect: Rect, text: String },
    Row { rect: Rect, title: String, detail: String, note: Option<String>, hit: Option<Hit>, current: bool },
    Button { rect: Rect, label: String, hit: Hit, primary: bool },
    Diff { rect: Rect, lines: Vec<DiffLine> },
    Dots { rect: Rect },
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
            | Block::Dots { rect } => *rect,
        }
    }
}

/// Where the panel's fixed parts go.
struct Frame {
    panel: Rect,
    title: Rect,
    /// The permission mode, as a switch.
    mode: Rect,
    agent: Rect,
    messages: Rect,
    field: Rect,
    text: Rect,
    send: Rect,
    hints: Rect,
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
        if self.doctor.busy {
            return;
        }
        if self.doctor.agent.is_none() && test_agent().is_none() {
            self.doctor.view = View::Picker;
            self.doctor.messages.push(Message::Note("Choose an agent first.".into()));
            return;
        }
        self.ensure_session();
        let Some(session) = &self.doctor.session else {
            self.doctor.messages.push(Message::Note("The agent is not available yet.".into()));
            return;
        };
        self.doctor.messages.push(Message::User(text.clone()));
        self.doctor.view = View::Conversation;
        self.doctor.scroll = 0.0;
        session.send(Request::Prompt(text));
        self.doctor.busy = true;
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
                if let (Ok(agents), Some(id), None) = (&result, &d.agent, &d.session) {
                    if !agents.iter().any(|a| &a.id == id) {
                        d.view = View::Picker;
                    }
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
                        d.session = None;
                        d.busy = false;
                    }
                    Status::NeedsLogin(_) => d.busy = false,
                    _ => {}
                }
                d.status = status;
            }
            Event::Text(text) => match d.messages.last_mut() {
                Some(Message::Agent(last)) => last.push_str(&text),
                _ => d.messages.push(Message::Agent(text)),
            },
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
                if let Some(note) = note {
                    d.messages.push(Message::Note(note.into()));
                }
            }
            Event::Note(text) => d.messages.push(Message::Note(text)),
            Event::Terminal(program) => self.run_login_terminal(&program),
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
                if path == crate::config::path() {
                    if let Err(error) = crate::config::Config::parse(&content) {
                        self.doctor_reply(
                            id,
                            Err(format!(
                                "Tileception would not load this configuration: {error}. Nothing was written."
                            )),
                        );
                        return;
                    }
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
        let saved = Saved { agent: self.doctor.agent.clone(), mode: self.doctor.mode };
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
    }

    /// A key while the panel has the keyboard.
    pub fn doctor_key(&mut self, sym: u32, mods: Mods) {
        self.doctor.edited = Instant::now();
        let word = mods.ctrl || mods.alt;
        let input = &mut self.doctor.input;
        match sym {
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
            keysyms::KEY_Return | keysyms::KEY_KP_Enter => {
                let text = input.text.trim().to_string();
                if !text.is_empty() && !self.doctor.busy {
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
            _ if !mods.ctrl && !mods.logo && !mods.alt => {
                if let Some(text) = crate::ui::compose_text(&mut self.doctor.compose, sym) {
                    self.doctor.input.insert(&text);
                }
            }
            _ => {}
        }
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
        if frame.send.contains(p) {
            if self.doctor.busy {
                self.doctor_hit(Hit::Stop);
            } else {
                self.doctor_key(keysyms::KEY_Return, Mods::default());
            }
            return true;
        }
        if frame.mode.contains(p) {
            self.doctor_hit(Hit::CycleMode);
            return true;
        }
        if frame.agent.contains(p) {
            self.doctor_hit(if self.doctor.view == View::Picker { Hit::ClosePicker } else { Hit::OpenPicker });
            return true;
        }
        let hit = self.doctor_blocks(frame.messages).into_iter().find_map(|b| match b {
            Block::Row { rect, hit: Some(hit), .. } | Block::Button { rect, hit, .. } if rect.contains(p) => Some(hit),
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
                if let Some(session) = &self.doctor.session {
                    session.send(Request::Login(method));
                }
            }
            Hit::LoggedIn => {
                if let Some(session) = &self.doctor.session {
                    session.send(Request::Retry);
                }
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
            Hit::CycleMode => self.set_doctor_mode(self.doctor.mode.next()),
            Hit::Example(i) => self.send_to_doctor(EXAMPLES[i].to_string()),
            Hit::Stop => {
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

    fn doctor_frame(&self) -> Frame {
        let screen = self.output_rect();
        let w = (screen.w - 24.0).clamp(1.0, PANEL_W);
        let panel = Rect::new(screen.x + 12.0, screen.y + 12.0, w, (screen.h - 24.0).max(1.0));
        let hints = Rect::new(panel.x, panel.bottom() - HINTS_H, panel.w, HINTS_H);
        let field = Rect::new(panel.x + PAD, hints.y - INPUT_H, panel.w - 2.0 * PAD, INPUT_H);
        let send = Rect::new(field.right() - 40.0, field.y + 6.0, 34.0, field.h - 12.0);
        let text = Rect::new(field.x + 14.0, field.y, send.x - field.x - 20.0, field.h);
        let top = panel.y + HEADER_H + 8.0;
        Frame {
            panel,
            title: Rect::new(panel.x + 18.0, panel.y + 12.0, 90.0, 32.0),
            mode: Rect::new(panel.x + 102.0, panel.y + 15.0, 132.0, 26.0),
            agent: Rect::new(panel.x + 240.0, panel.y + 10.0, panel.w - 252.0, 36.0),
            messages: Rect::new(panel.x + PAD, top, panel.w - 2.0 * PAD, field.y - 12.0 - top),
            field,
            text,
            send,
            hints,
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
        let style = Style::new(self.theme.font_size, self.theme.text);
        style.lines(self.fitting_lines(style, 20.0))
    }

    fn note_style(&self) -> Style {
        let style = Style::new(self.theme.font_size * 0.85, self.theme.muted).align(Align::Centre);
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

        if let Status::NeedsLogin(logins) = &self.doctor.status {
            blocks.push(Block::Heading { rect: Rect::new(x + 4.0, y, w, 24.0), text: "LOG IN".into() });
            y += 28.0;
            for Login { id, name, description, terminal } in logins {
                let detail = description.clone().unwrap_or_else(|| {
                    if terminal.is_some() {
                        "Opens a terminal".into()
                    } else {
                        String::new()
                    }
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
            if logins.iter().any(|l| l.terminal.is_some()) {
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

        if self.doctor.messages.is_empty() && !matches!(self.doctor.status, Status::NeedsLogin(_)) {
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
                Message::User(text) | Message::Agent(text) => {
                    let user = matches!(m, Message::User(_));
                    let (tw, th) = self.measure_text(text, self.body_style(), w - 64.0);
                    let bw = tw + 28.0;
                    let bx = if user { x + w - bw } else { x };
                    blocks.push(Block::Bubble { rect: Rect::new(bx, y, bw, th + 20.0), text: text.clone(), user });
                    y += th + 20.0 + GAP;
                }
                Message::Note(text) => push_note(&mut blocks, &mut y, text),
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
            && matches!(self.doctor.messages.last(), Some(Message::User(_)) | Some(Message::Tool { .. }))
        {
            blocks.push(Block::Dots { rect: Rect::new(x, y, 64.0, 30.0) });
        }
        blocks
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
            let r = match b {
                Block::Bubble { rect, .. }
                | Block::Note { rect, .. }
                | Block::Tool { rect, .. }
                | Block::Heading { rect, .. }
                | Block::Row { rect, .. }
                | Block::Button { rect, .. }
                | Block::Diff { rect, .. }
                | Block::Dots { rect } => rect,
            };
            r.y += shift;
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
            (Some(name), Status::NeedsLogin(_) | Status::LoggingIn) => (format!("{name} \u{b7} log in"), t.warning),
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
        p.popup(frame.panel);

        p.text("Doctor", frame.title, Style::new(t.font_size * 1.3, t.text).bold());
        // The permission mode; a click moves to the next one.
        let mode = self.doctor.mode;
        let colour = match mode {
            Mode::Ask => t.muted,
            Mode::Config => t.accent,
            Mode::Yolo => t.warning,
        };
        let hover = frame.mode.contains(pointer);
        p.shape(
            frame.mode,
            Shape {
                border_width: 1.0,
                border: colour.alpha(0.6),
                ..Shape::filled(if hover { t.surface_raised } else { colour.alpha(0.10) }, 13.0)
            },
        );
        p.text(mode.label(), frame.mode, Style::new(t.font_size * 0.8, colour).bold().align(Align::Centre));
        // The agent, as a button that opens the picker.
        let (label, dot) = self.status_line();
        let style = Style::new(t.font_size * 0.9, t.text);
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
        let empty = self.doctor.view == View::Conversation
            && self.doctor.messages.is_empty()
            && !matches!(self.doctor.status, Status::NeedsLogin(_));
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
        let size = t.font_size * 1.05;
        if input.text.is_empty() {
            let hint = if self.doctor.busy { "Answering\u{2026}" } else { "Ask about your setup" };
            p.text(hint, frame.text, Style::new(size, t.muted));
        }
        let field = self.text.borrow_mut().field(
            &input.text,
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
            let can_send = !input.text.trim().is_empty();
            if can_send {
                p.shape(frame.send, Shape { radius: t.radius, fill: (t.accent, t.accent_end), ..Default::default() });
            }
            p.glyph(glyph::ARROW_RIGHT, frame.send, 16, if can_send { t.on_accent } else { t.muted.alpha(0.6) });
        }

        let hints = frame.hints;
        p.fill(Rect::new(hints.x + 1.0, hints.y, hints.w - 2.0, 1.0), t.border, 0.0);
        let keys: &[(&str, &str)] = if self.doctor.busy {
            &[("Ctrl+C", "Stop"), ("PgUp", "Earlier"), ("Esc", "Close")]
        } else {
            &[("Enter", "Send"), ("PgUp", "Earlier"), ("Esc", "Close"), ("Super+/", "Toggle")]
        };
        p.key_hints(keys, hints.x + 16.0, hints.y + hints.h / 2.0, false);
        p.finish()
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
                    Style::new(t.font_size * 0.88, t.muted),
                );
            }
            Block::Heading { rect, text } => {
                p.text(text, *rect, Style::new(t.font_size * 0.75, t.muted).bold());
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
                        Style::new(t.font_size * 0.92, if hover { t.text } else { t.muted }),
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
                    Style::new(t.font_size * 0.82, t.muted),
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
