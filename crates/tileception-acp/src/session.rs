//! One running agent: newline-delimited JSON-RPC over the child's stdio.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::thread::{self, JoinHandle};

use base64::Engine as _;
use serde_json::{Value, json};

use crate::{
    Action, Agent, AgentCommand, Choice, Emit, EnvVar, Event, FileOp, Image, Login, LoginHow, OptionValue, Request,
    SessionOption, Status, install,
};

/// The protocol version spoken here.
const PROTOCOL_VERSION: u64 = 1;
/// The agent wants a login first.
const AUTH_REQUIRED: i64 = -32000;
const METHOD_NOT_FOUND: i64 = -32601;
const INTERNAL_ERROR: i64 = -32603;
/// The id given to an agent's model list from before session options.
const LEGACY_MODEL: &str = "model";

enum Msg {
    User(Request),
    /// A message from the agent program started as this generation.
    Line(u64, Value),
    Closed(u64),
    Stop,
}

/// Owns the agent's worker thread; dropping it stops the agent.
pub struct Session {
    messages: mpsc::Sender<Msg>,
    thread: Option<JoinHandle<()>>,
    /// Still downloading or installing: the worker cannot hear Stop yet.
    installing: Arc<AtomicBool>,
    /// Cleared on drop, so a stopping worker reports nothing more.
    alive: Arc<AtomicBool>,
}

impl Session {
    /// Starts `agent` in `cwd`. `context` goes along with the first prompt,
    /// telling the agent what it is helping with.
    pub fn start(agent: Agent, cwd: PathBuf, context: String, emit: Emit) -> Session {
        Self::launch(agent.name.clone(), Source::Registry(agent), cwd, context, emit)
    }

    /// Starts an agent program directly (tests, and agents already installed).
    pub fn start_program(
        name: String,
        program: install::Program,
        cwd: PathBuf,
        context: String,
        emit: Emit,
    ) -> Session {
        Self::launch(name, Source::Program(program), cwd, context, emit)
    }

    fn launch(name: String, source: Source, cwd: PathBuf, context: String, emit: Emit) -> Session {
        let (messages, incoming) = mpsc::channel();
        let sender = messages.clone();
        let installing = Arc::new(AtomicBool::new(true));
        let alive = Arc::new(AtomicBool::new(true));
        let emit: Emit = {
            let alive = alive.clone();
            Arc::new(move |event| {
                if alive.load(Ordering::Acquire) {
                    emit(event);
                }
            })
        };
        let installed = installing.clone();
        let thread = thread::Builder::new()
            .name("tileception-acp".into())
            .spawn(move || {
                let mut worker = Worker::new(name, source, cwd, context, emit.clone(), installed);
                if let Err(error) = worker.run(&sender, &incoming) {
                    emit(Event::Status(Status::Failed(error)));
                }
                worker.stop();
            })
            .map_err(|e| tracing::error!("could not start the agent thread: {e}"))
            .ok();
        Session { messages, thread, installing, alive }
    }

    pub fn send(&self, request: Request) {
        let _ = self.messages.send(Msg::User(request));
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.alive.store(false, Ordering::Release);
        let _ = self.messages.send(Msg::Stop);
        // A download can take minutes: rather than wait for it, leave the
        // worker to see Stop once it is done (it then stops the agent).
        if self.installing.load(Ordering::Acquire) {
            return;
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// What a response we wait for belongs to.
enum Pending {
    Initialize,
    NewSession,
    Authenticate,
    Prompt,
    SetOption { id: String, value: String },
}

/// Where the agent program comes from.
enum Source {
    Registry(Agent),
    Program(install::Program),
}

struct Worker {
    name: String,
    source: Option<Source>,
    cwd: PathBuf,
    context: Option<String>,
    emit: Emit,
    child: Option<Child>,
    stdin: Option<ChildStdin>,
    program: Option<install::Program>,
    /// Counts restarts, so a stopped program's last words are not mistaken
    /// for the new one's.
    generation: u64,
    /// Variables from an environment login, for every start from then on.
    login_env: std::collections::BTreeMap<String, String>,
    sender: Option<mpsc::Sender<Msg>>,
    /// Sent along with the first prompt of every session.
    context_text: Option<String>,
    /// The agent lists its models the old way (`session/set_model`).
    legacy_model: bool,
    options: Vec<SessionOption>,
    next_id: u64,
    pending: HashMap<u64, Pending>,
    logins: Vec<Login>,
    session: Option<String>,
    /// A prompt typed before the session was ready.
    queued: Option<(String, Vec<Image>)>,
    /// Permission requests not yet answered, to cancel on Cancel.
    asking: Vec<Value>,
    /// The last lines the agent wrote to stderr, for errors.
    stderr: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    /// Set while the program is downloaded or installed.
    installing: Arc<AtomicBool>,
}

impl Worker {
    fn new(
        name: String,
        source: Source,
        cwd: PathBuf,
        context: String,
        emit: Emit,
        installing: Arc<AtomicBool>,
    ) -> Self {
        Self {
            installing,
            name,
            source: Some(source),
            cwd,
            context: Some(context.clone()).filter(|c| !c.is_empty()),
            context_text: Some(context).filter(|c| !c.is_empty()),
            emit,
            child: None,
            stdin: None,
            program: None,
            generation: 0,
            login_env: Default::default(),
            sender: None,
            legacy_model: false,
            options: Vec::new(),
            next_id: 1,
            pending: HashMap::new(),
            logins: Vec::new(),
            session: None,
            queued: None,
            asking: Vec::new(),
            stderr: Default::default(),
        }
    }

    fn status(&self, status: Status) {
        (self.emit)(Event::Status(status));
    }

    fn run(&mut self, sender: &mpsc::Sender<Msg>, incoming: &mpsc::Receiver<Msg>) -> Result<(), String> {
        self.status(Status::Starting(format!("Starting {}\u{2026}", self.name)));
        let emit = self.emit.clone();
        let program = match self.source.take() {
            Some(Source::Registry(agent)) => {
                install::program(&agent, &move |text| emit(Event::Status(Status::Starting(text.into()))))?
            }
            Some(Source::Program(program)) => program,
            None => return Err("the agent was already started".into()),
        };
        self.installing.store(false, Ordering::Release);
        self.sender = Some(sender.clone());
        self.spawn(&program)?;
        self.program = Some(program);
        self.initialize()?;
        loop {
            match incoming.recv() {
                Ok(Msg::Stop) | Err(_) => return Ok(()),
                Ok(Msg::Closed(generation)) if generation == self.generation => return Err(self.exit_reason()),
                Ok(Msg::Line(generation, message)) if generation == self.generation => self.message(message)?,
                Ok(Msg::Closed(_) | Msg::Line(..)) => {}
                Ok(Msg::User(request)) => self.user(request)?,
            }
        }
    }

    fn initialize(&mut self) -> Result<(), String> {
        self.request(
            "initialize",
            json!({
                "protocolVersion": PROTOCOL_VERSION,
                "clientCapabilities": {
                    // Files go through Tileception: it shows diffs, keeps
                    // backups and applies the doctor's permission mode.
                    "fs": { "readTextFile": true, "writeTextFile": true },
                    "terminal": false,
                    "auth": { "terminal": true },
                },
                "clientInfo": { "name": "tileception", "title": "Tileception", "version": env!("CARGO_PKG_VERSION") },
            }),
            Pending::Initialize,
        )
    }

    /// Stops the agent program and starts it again, as after a login that
    /// only a fresh start notices.
    fn restart(&mut self) -> Result<(), String> {
        let program = self.program.clone().ok_or("the agent is not running")?;
        self.status(Status::Starting(format!("Restarting {}...", self.name)));
        self.stop();
        self.generation += 1;
        self.pending.clear();
        self.asking.clear();
        self.session = None;
        self.spawn(&program)?;
        self.initialize()
    }

    fn spawn(&mut self, program: &install::Program) -> Result<(), String> {
        let mut command = Command::new(&program.command);
        command.args(&program.args).current_dir(&self.cwd);
        crate::with_env(&mut command, &program.env);
        crate::with_env(&mut command, &self.login_env);
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("{} could not start: {e}", self.name))?;
        let stdout = child.stdout.take().ok_or("no agent output")?;
        let stderr = child.stderr.take().ok_or("no agent errors")?;
        self.stdin = child.stdin.take();
        self.child = Some(child);

        let lines = self.sender.clone().ok_or("the agent has no way to answer")?;
        let generation = self.generation;
        thread::Builder::new()
            .name("tileception-acp-read".into())
            .spawn(move || {
                for line in BufReader::new(stdout).lines() {
                    let Ok(line) = line else { break };
                    if line.trim().is_empty() {
                        continue;
                    }
                    match serde_json::from_str::<Value>(&line) {
                        Ok(value) => {
                            if lines.send(Msg::Line(generation, value)).is_err() {
                                return;
                            }
                        }
                        Err(_) => tracing::debug!("agent wrote something that is not JSON: {line}"),
                    }
                }
                let _ = lines.send(Msg::Closed(generation));
            })
            .map_err(|e| e.to_string())?;
        let log = self.stderr.clone();
        let name = self.name.clone();
        thread::Builder::new()
            .name("tileception-acp-log".into())
            .spawn(move || {
                for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                    tracing::debug!("{name}: {line}");
                    let mut log = log.lock().unwrap_or_else(|p| p.into_inner());
                    log.push(line);
                    if log.len() > 20 {
                        log.remove(0);
                    }
                }
            })
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    fn exit_reason(&mut self) -> String {
        let code = self.child.as_mut().and_then(|c| c.wait().ok()).and_then(|s| s.code());
        let log = self.stderr.lock().unwrap_or_else(|p| p.into_inner());
        let last = log.iter().rev().find(|l| !l.trim().is_empty()).cloned().unwrap_or_default();
        match code {
            Some(code) if !last.is_empty() => format!("{} stopped ({code}): {last}", self.name),
            Some(code) => format!("{} stopped ({code})", self.name),
            None => format!("{} stopped", self.name),
        }
    }

    fn stop(&mut self) {
        self.stdin = None;
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }

    fn write(&mut self, message: &Value) -> Result<(), String> {
        let stdin = self.stdin.as_mut().ok_or("the agent is not running")?;
        let mut line = serde_json::to_string(message).map_err(|e| e.to_string())?;
        line.push('\n');
        stdin
            .write_all(line.as_bytes())
            .and_then(|_| stdin.flush())
            .map_err(|e| format!("the agent stopped listening: {e}"))
    }

    fn request(&mut self, method: &str, params: Value, purpose: Pending) -> Result<(), String> {
        let id = self.next_id;
        self.next_id += 1;
        self.pending.insert(id, purpose);
        self.write(&json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }))
    }

    fn notify(&mut self, method: &str, params: Value) -> Result<(), String> {
        self.write(&json!({ "jsonrpc": "2.0", "method": method, "params": params }))
    }

    fn respond(&mut self, id: Value, result: Value) -> Result<(), String> {
        self.write(&json!({ "jsonrpc": "2.0", "id": id, "result": result }))
    }

    fn respond_error(&mut self, id: Value, code: i64, message: &str) -> Result<(), String> {
        self.write(&json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } }))
    }

    fn new_session(&mut self) -> Result<(), String> {
        // Every conversation starts by saying what the agent helps with.
        self.context.clone_from(&self.context_text);
        let cwd = self.cwd.display().to_string();
        self.request("session/new", json!({ "cwd": cwd, "mcpServers": [] }), Pending::NewSession)
    }

    fn prompt(&mut self, text: String, images: Vec<Image>) -> Result<(), String> {
        let Some(session) = self.session.clone() else {
            self.queued = Some((text, images));
            return Ok(());
        };
        let mut blocks = Vec::new();
        if let Some(context) = self.context.take() {
            blocks.push(json!({ "type": "text", "text": context }));
        }
        for image in images {
            let data = base64::engine::general_purpose::STANDARD.encode(&image.data);
            blocks.push(json!({ "type": "image", "mimeType": image.mime, "data": data }));
        }
        blocks.push(json!({ "type": "text", "text": text }));
        self.request("session/prompt", json!({ "sessionId": session, "prompt": blocks }), Pending::Prompt)
    }

    fn user(&mut self, request: Request) -> Result<(), String> {
        match request {
            Request::Prompt { text, images } => self.prompt(text, images)?,
            Request::Cancel => {
                if let Some(session) = self.session.clone() {
                    self.notify("session/cancel", json!({ "sessionId": session }))?;
                }
                for id in std::mem::take(&mut self.asking) {
                    self.respond(id, json!({ "outcome": { "outcome": "cancelled" } }))?;
                }
            }
            Request::Permission { id, choice } => {
                self.asking.retain(|a| *a != id);
                let outcome = match choice {
                    Some(choice) => json!({ "outcome": "selected", "optionId": choice }),
                    None => json!({ "outcome": "cancelled" }),
                };
                self.respond(id, json!({ "outcome": outcome }))?;
            }
            Request::Login { method, secrets } => match self.logins.iter().find(|l| l.id == method).cloned() {
                Some(Login { how: LoginHow::Terminal(program), .. }) => {
                    self.status(Status::LoggingIn { terminal: true });
                    (self.emit)(Event::Terminal(program));
                }
                Some(Login { how: LoginHow::Env(vars), .. }) => {
                    for (var, value) in vars.iter().zip(secrets) {
                        self.login_env.insert(var.name.clone(), value);
                    }
                    self.restart()?;
                }
                Some(Login { how, .. }) => {
                    self.status(Status::LoggingIn { terminal: false });
                    let mut params = json!({ "methodId": method });
                    if let (LoginHow::Key, Some(key)) = (how, secrets.into_iter().next()) {
                        params["_meta"] = json!({ "api-key": key });
                    }
                    self.request("authenticate", params, Pending::Authenticate)?;
                }
                None => (self.emit)(Event::Note(format!("{method} is not one of the agent's logins"))),
            },
            Request::Retry => self.restart()?,
            Request::NewSession => {
                if let Some(session) = self.session.take() {
                    self.notify("session/cancel", json!({ "sessionId": session }))?;
                }
                for id in std::mem::take(&mut self.asking) {
                    self.respond(id, json!({ "outcome": { "outcome": "cancelled" } }))?;
                }
                self.queued = None;
                self.new_session()?;
            }
            Request::SetOption { id, value } => {
                let Some(session) = self.session.clone() else {
                    (self.emit)(Event::Note("The agent is not ready yet.".into()));
                    return Ok(());
                };
                let pending = Pending::SetOption { id: id.clone(), value: value.clone() };
                if self.legacy_model && id == LEGACY_MODEL {
                    self.request("session/set_model", json!({ "sessionId": session, "modelId": value }), pending)?;
                } else {
                    let params = json!({ "sessionId": session, "configId": id, "value": value });
                    self.request("session/set_config_option", params, pending)?;
                }
            }
            Request::Reply { id, result } => match result {
                Ok(value) => self.respond(id, value)?,
                Err(message) => self.respond_error(id, INTERNAL_ERROR, &message)?,
            },
        }
        Ok(())
    }

    fn message(&mut self, message: Value) -> Result<(), String> {
        let method = message.get("method").and_then(Value::as_str).map(str::to_string);
        let id = message.get("id").cloned();
        match (method, id) {
            (Some(method), Some(id)) => {
                self.agent_request(&method, id, message.get("params").cloned().unwrap_or(Value::Null))
            }
            (Some(method), None) => {
                if method == "session/update"
                    && let Some(update) = message.get("params").and_then(|p| p.get("update"))
                {
                    self.update(update);
                }
                Ok(())
            }
            (None, Some(id)) => {
                let Some(purpose) = id.as_u64().and_then(|id| self.pending.remove(&id)) else { return Ok(()) };
                match message.get("error") {
                    Some(error) => self.failed(purpose, error),
                    None => self.answered(purpose, message.get("result").cloned().unwrap_or(Value::Null)),
                }
            }
            (None, None) => Ok(()),
        }
    }

    fn answered(&mut self, purpose: Pending, result: Value) -> Result<(), String> {
        match purpose {
            Pending::Initialize => {
                let version = result.get("protocolVersion").and_then(Value::as_u64).unwrap_or(0);
                if version != PROTOCOL_VERSION {
                    return Err(format!(
                        "{} speaks ACP version {version}; Tileception speaks {PROTOCOL_VERSION}",
                        self.name
                    ));
                }
                self.logins = logins(&result, self.program.as_ref());
                let images = result.pointer("/agentCapabilities/promptCapabilities/image").and_then(Value::as_bool);
                (self.emit)(Event::Images(images.unwrap_or(false)));
                self.new_session()
            }
            Pending::NewSession => {
                let Some(session) = result.get("sessionId").and_then(Value::as_str) else {
                    return Err("the agent started no session".into());
                };
                self.session = Some(session.to_string());
                self.status(Status::Ready);
                let (options, legacy) = session_options(&result);
                self.legacy_model = legacy;
                self.set_options(options);
                if let Some((text, images)) = self.queued.take() {
                    self.prompt(text, images)?;
                }
                Ok(())
            }
            Pending::Authenticate => self.new_session(),
            Pending::Prompt => {
                let reason = result.get("stopReason").and_then(Value::as_str).unwrap_or("end_turn");
                (self.emit)(Event::TurnEnded(reason.to_string()));
                Ok(())
            }
            Pending::SetOption { id, value } => {
                // Options come back whole; the old way, only the change.
                if result.get("configOptions").is_some() {
                    self.set_options(session_options(&result).0);
                } else {
                    self.options_changed(&id, &value);
                }
                Ok(())
            }
        }
    }

    /// Reports one option's new value, keeping the others.
    fn options_changed(&mut self, id: &str, value: &str) {
        let mut options = self.options.clone();
        if let Some(option) = options.iter_mut().find(|o| o.id == id) {
            option.current = value.to_string();
        }
        self.set_options(options);
    }

    fn set_options(&mut self, options: Vec<SessionOption>) {
        self.options.clone_from(&options);
        (self.emit)(Event::Options(options));
    }

    fn failed(&mut self, purpose: Pending, error: &Value) -> Result<(), String> {
        let code = error.get("code").and_then(Value::as_i64).unwrap_or(0);
        let mut text = error.get("message").and_then(Value::as_str).unwrap_or("unknown error").to_string();
        // The details often say what is really wrong ("no provider configured").
        if let Some(data) = error.get("data").and_then(Value::as_str).filter(|d| !d.is_empty()) {
            text = format!("{text}: {data}");
        }
        let needs_login = code == AUTH_REQUIRED && !self.logins.is_empty();
        match purpose {
            // Agents that are not set up yet often fail here in their own way;
            // their login is the way out.
            Pending::NewSession if !self.logins.is_empty() => {
                (self.emit)(Event::Note(format!("{} is not ready: {text}", self.name)));
                self.session = None;
                self.status(Status::NeedsLogin(self.logins.clone()));
                Ok(())
            }
            Pending::NewSession | Pending::Authenticate | Pending::Prompt if needs_login => {
                if matches!(purpose, Pending::Prompt) {
                    (self.emit)(Event::TurnEnded("auth_required".into()));
                }
                if matches!(purpose, Pending::Authenticate) {
                    (self.emit)(Event::Note(format!("Logging in did not work: {text}")));
                }
                self.session = None;
                self.status(Status::NeedsLogin(self.logins.clone()));
                Ok(())
            }
            // A login that did not work (a wrong key, a closed browser) is
            // no reason to stop the agent: offer the logins again.
            Pending::Authenticate if !self.logins.is_empty() => {
                (self.emit)(Event::Note(format!("Logging in did not work: {text}")));
                self.status(Status::NeedsLogin(self.logins.clone()));
                Ok(())
            }
            Pending::Prompt => {
                (self.emit)(Event::Note(format!("The agent could not answer: {text}")));
                (self.emit)(Event::TurnEnded("error".into()));
                Ok(())
            }
            Pending::SetOption { .. } => {
                (self.emit)(Event::Note(format!("The agent kept its setting: {text}")));
                Ok(())
            }
            _ => Err(format!("{}: {text}", self.name)),
        }
    }

    fn agent_request(&mut self, method: &str, id: Value, params: Value) -> Result<(), String> {
        let path = |key: &str| params.get(key).and_then(Value::as_str).map(PathBuf::from);
        match method {
            "fs/read_text_file" | "fs/write_text_file" => {
                let Some(file) = path("path") else {
                    return self.respond_error(id, INTERNAL_ERROR, "no path given");
                };
                let op = if method == "fs/read_text_file" {
                    FileOp::Read {
                        path: file,
                        line: params.get("line").and_then(Value::as_u64),
                        limit: params.get("limit").and_then(Value::as_u64),
                    }
                } else {
                    let content = params.get("content").and_then(Value::as_str).unwrap_or("").to_string();
                    FileOp::Write { path: file, content }
                };
                (self.emit)(Event::File { id, op });
                return Ok(());
            }
            "session/request_permission" => {}
            // Terminals stay with the agent's own tools.
            _ => return self.respond_error(id, METHOD_NOT_FOUND, "not offered by Tileception"),
        }
        let call = params.get("toolCall");
        let title = call
            .and_then(|c| c.get("title"))
            .and_then(Value::as_str)
            .unwrap_or("The agent wants to go ahead")
            .to_string();
        let choices = params
            .get("options")
            .and_then(Value::as_array)
            .map(|options| {
                options
                    .iter()
                    .filter_map(|o| {
                        Some(Choice {
                            id: o.get("optionId")?.as_str()?.to_string(),
                            name: o.get("name")?.as_str()?.to_string(),
                            kind: o.get("kind").and_then(Value::as_str).unwrap_or("allow_once").to_string(),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        self.asking.push(id.clone());
        let action = call.map(action_of).unwrap_or_default();
        (self.emit)(Event::Permission { id, title, choices, action });
        Ok(())
    }

    fn update(&mut self, update: &Value) {
        let kind = update.get("sessionUpdate").and_then(Value::as_str).unwrap_or("");
        match kind {
            "config_option_update" => return self.set_options(session_options(update).0),
            "current_model_update" if self.legacy_model => {
                if let Some(model) = update.get("currentModelId").and_then(Value::as_str) {
                    self.options_changed(LEGACY_MODEL, model);
                }
                return;
            }
            "available_commands_update" => {
                let commands = update
                    .get("availableCommands")
                    .and_then(Value::as_array)
                    .map(|list| {
                        list.iter()
                            .filter_map(|c| {
                                Some(AgentCommand {
                                    name: c.get("name")?.as_str()?.trim_start_matches('/').to_string(),
                                    description: c.get("description").and_then(Value::as_str).unwrap_or("").into(),
                                })
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                return (self.emit)(Event::Commands(commands));
            }
            _ => {}
        }
        let text = || {
            let content = update.get("content")?;
            (content.get("type").and_then(Value::as_str) == Some("text"))
                .then(|| content.get("text").and_then(Value::as_str).map(str::to_string))
                .flatten()
        };
        let field = |name: &str| update.get(name).and_then(Value::as_str).map(str::to_string);
        let event = match kind {
            "agent_message_chunk" => text().map(Event::Text),
            "agent_thought_chunk" => text().map(Event::Thought),
            "tool_call" | "tool_call_update" => field("toolCallId").map(|id| Event::Tool {
                id,
                title: field("title"),
                kind: field("kind"),
                status: field("status"),
            }),
            _ => None,
        };
        if let Some(event) = event {
            (self.emit)(event);
        }
    }
}

/// What a tool call is about: its kind, files, command and edits.
fn action_of(call: &Value) -> Action {
    let s = |v: Option<&Value>| v.and_then(Value::as_str).map(str::to_string);
    let paths = call
        .get("locations")
        .and_then(Value::as_array)
        .map(|l| l.iter().filter_map(|x| s(x.get("path")).map(PathBuf::from)).collect())
        .unwrap_or_default();
    let raw = call.get("rawInput");
    let command = raw.and_then(|r| {
        s(r.get("command")).or_else(|| {
            r.get("command")
                .and_then(Value::as_array)
                .map(|parts| parts.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(" "))
        })
    });
    let diffs = call
        .get("content")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter(|c| c.get("type").and_then(Value::as_str) == Some("diff"))
                .filter_map(|c| Some((PathBuf::from(s(c.get("path"))?), s(c.get("oldText")), s(c.get("newText"))?)))
                .collect()
        })
        .unwrap_or_default();
    Action { kind: s(call.get("kind")), paths, command, diffs }
}

/// The logins an agent offers, from its `initialize` answer.
fn logins(result: &Value, program: Option<&install::Program>) -> Vec<Login> {
    let methods = result.get("authMethods").and_then(Value::as_array).cloned().unwrap_or_default();
    methods
        .iter()
        .filter_map(|m| {
            let id = m.get("id")?.as_str()?.to_string();
            let name = m.get("name").and_then(Value::as_str).unwrap_or(&id).to_string();
            let description = m.get("description").and_then(Value::as_str).map(str::to_string);
            let suggested = description.as_deref().and_then(|d| suggested_command(d, program));
            let kind = m.get("type").and_then(Value::as_str);
            let meta = m.get("_meta");
            if kind == Some("env_var") {
                let vars = env_vars(m);
                return (!vars.is_empty()).then_some(Login { id, name, description, how: LoginHow::Env(vars) });
            }
            if meta.and_then(|m| m.get("api-key")).is_some() {
                return Some(Login { id, name, description, how: LoginHow::Key });
            }
            // The older way to ask for a terminal login: its own command line.
            if let Some(own) = meta.and_then(|m| m.get("terminal-auth")).and_then(terminal_auth) {
                return Some(Login { id, name, description, how: LoginHow::Terminal(own) });
            }
            let terminal = (kind == Some("terminal")).then(|| {
                let base = program.cloned().unwrap_or(install::Program {
                    command: PathBuf::new(),
                    args: Vec::new(),
                    env: Default::default(),
                });
                let args = m
                    .get("args")
                    .and_then(Value::as_array)
                    .map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect::<Vec<_>>());
                let mut env = base.env.clone();
                if let Some(extra) = m.get("env").and_then(Value::as_object) {
                    for (k, v) in extra {
                        if let Some(v) = v.as_str() {
                            env.insert(k.clone(), v.to_string());
                        }
                    }
                }
                install::Program {
                    command: base.command,
                    args: base.args.into_iter().chain(args.unwrap_or_default()).collect(),
                    env,
                }
            });
            let how = terminal.or(suggested).map_or(LoginHow::Agent, LoginHow::Terminal);
            Some(Login { id, name, description, how })
        })
        .collect()
}

/// `_meta["terminal-auth"]`: `{ command, args, env }`.
fn terminal_auth(meta: &Value) -> Option<install::Program> {
    let command = meta.get("command")?.as_str()?;
    let strings = |v: Option<&Value>| -> Vec<String> {
        v.and_then(Value::as_array)
            .map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect())
            .unwrap_or_default()
    };
    let env = meta
        .get("env")
        .and_then(Value::as_object)
        .map(|e| e.iter().filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_string()))).collect())
        .unwrap_or_default();
    Some(install::Program { command: command.into(), args: strings(meta.get("args")), env })
}

/// The variables an `env_var` login asks for: `vars`, or a single `varName`.
fn env_vars(method: &Value) -> Vec<EnvVar> {
    let name_of = |v: &Value| v.get("name").or_else(|| v.get("varName")).and_then(Value::as_str).map(str::to_string);
    match method.get("vars").and_then(Value::as_array) {
        Some(vars) => vars
            .iter()
            .filter(|v| !v.get("optional").and_then(Value::as_bool).unwrap_or(false))
            .filter_map(|v| {
                Some(EnvVar { name: name_of(v)?, label: v.get("label").and_then(Value::as_str).map(str::to_string) })
            })
            .collect(),
        None => method
            .get("varName")
            .and_then(Value::as_str)
            .map(|name| vec![EnvVar { name: name.into(), label: None }])
            .unwrap_or_default(),
    }
}

/// The session's settings from `session/new` or an update: `configOptions`
/// (selects only), or else the older `models`. Says which it was.
fn session_options(result: &Value) -> (Vec<SessionOption>, bool) {
    let s = |v: &Value, key: &str| v.get(key).and_then(Value::as_str).map(str::to_string);
    if let Some(options) = result.get("configOptions").and_then(Value::as_array) {
        let options = options
            .iter()
            .filter(|o| o.get("type").and_then(Value::as_str).is_none_or(|t| t == "select"))
            .filter_map(|o| {
                let values = o
                    .get("options")
                    .and_then(Value::as_array)?
                    .iter()
                    // Grouped values: flattened.
                    .flat_map(|v| match v.get("options").and_then(Value::as_array) {
                        Some(group) => group.clone(),
                        None => vec![v.clone()],
                    })
                    .filter_map(|v| {
                        let value = s(&v, "value")?;
                        Some(OptionValue {
                            name: s(&v, "name").unwrap_or_else(|| value.clone()),
                            value,
                            description: s(&v, "description"),
                        })
                    })
                    .collect();
                let id = s(o, "id")?;
                Some(SessionOption {
                    name: s(o, "name").unwrap_or_else(|| id.clone()),
                    category: s(o, "category"),
                    current: s(o, "currentValue").unwrap_or_default(),
                    values,
                    id,
                })
            })
            .collect();
        return (options, false);
    }
    let Some(models) = result.get("models") else { return (Vec::new(), false) };
    let values: Vec<OptionValue> = models
        .get("availableModels")
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .filter_map(|m| {
                    let value = s(m, "modelId")?;
                    Some(OptionValue {
                        name: s(m, "name").unwrap_or_else(|| value.clone()),
                        value,
                        description: s(m, "description"),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    if values.is_empty() {
        return (Vec::new(), false);
    }
    let option = SessionOption {
        id: LEGACY_MODEL.into(),
        name: "Model".into(),
        category: Some("model".into()),
        current: s(models, "currentModelId").unwrap_or_default(),
        values,
    };
    (vec![option], true)
}

/// A login whose description says to run the agent's own program, like
/// "Run `goose configure` to set up your AI provider": that, in a terminal.
/// Only the agent's own program is run, with the words that follow it.
fn suggested_command(description: &str, program: Option<&install::Program>) -> Option<install::Program> {
    let program = program?;
    let own = program.command.file_name()?.to_str()?;
    let quoted = description.split('`').nth(1)?;
    let mut words = quoted.split_whitespace();
    if words.next()? != own {
        return None;
    }
    let args: Vec<String> = words.map(str::to_string).collect();
    if args.iter().any(|a| a.contains(['|', ';', '&', '$', '>', '<'])) {
        return None;
    }
    Some(install::Program { command: program.command.clone(), args, env: program.env.clone() })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_login_methods() {
        let program = install::Program {
            command: "npx".into(),
            args: vec!["-y".into(), "agent".into()],
            env: Default::default(),
        };
        let result = json!({ "protocolVersion": 1, "authMethods": [
            { "id": "claude-login", "name": "Log in with Claude", "description": "Opens a browser" },
            { "type": "terminal", "id": "cli", "name": "Log in in a terminal", "args": ["--login"], "env": { "A": "1" } }
        ]});
        let found = logins(&result, Some(&program));
        assert_eq!(found.len(), 2);
        assert_eq!((found[0].id.as_str(), &found[0].how), ("claude-login", &LoginHow::Agent));
        let LoginHow::Terminal(terminal) = &found[1].how else { panic!("terminal login") };
        let goose = install::Program {
            command: "/agents/goose/1/goose".into(),
            args: vec!["acp".into()],
            env: Default::default(),
        };
        let hinted = logins(
            &json!({ "authMethods": [{ "id": "goose-provider", "name": "Configure Provider",
                "description": "Run `goose configure` to set up your AI provider and API key" }] }),
            Some(&goose),
        );
        let LoginHow::Terminal(configure) = &hinted[0].how else { panic!("suggested terminal login") };
        assert_eq!(
            (configure.command.to_str(), configure.args.as_slice()),
            (Some("/agents/goose/1/goose"), &["configure".to_string()][..])
        );
        assert!(suggested_command("Run `rm -rf /` first", Some(&goose)).is_none());
        assert!(suggested_command("Run `goose x; rm -rf /`", Some(&goose)).is_none());
        assert_eq!(terminal.args, ["-y", "agent", "--login"]);
        assert_eq!(terminal.env["A"], "1");
    }

    #[test]
    fn reads_other_login_kinds() {
        let found = logins(
            &json!({ "authMethods": [
                { "id": "setup", "name": "Setup", "_meta": { "terminal-auth": { "command": "/a/amp-acp", "args": ["--setup"] } } },
                { "id": "key", "name": "API key", "_meta": { "api-key": { "provider": "google" } } },
                { "id": "env", "name": "OpenAI", "type": "env_var",
                  "vars": [{ "name": "OPENAI_API_KEY", "label": "OpenAI key" }, { "name": "OPTIONAL", "optional": true }] },
                { "id": "old-env", "name": "Old", "type": "env_var", "varName": "OLD_KEY" }
            ]}),
            None,
        );
        let LoginHow::Terminal(setup) = &found[0].how else { panic!("terminal-auth login") };
        assert_eq!((setup.command.to_str(), setup.args.as_slice()), (Some("/a/amp-acp"), &["--setup".to_string()][..]));
        assert_eq!(found[1].how, LoginHow::Key);
        assert_eq!(found[1].how.asks(), ["API key"]);
        assert_eq!(
            found[2].how,
            LoginHow::Env(vec![EnvVar { name: "OPENAI_API_KEY".into(), label: Some("OpenAI key".into()) }])
        );
        assert_eq!(found[3].how, LoginHow::Env(vec![EnvVar { name: "OLD_KEY".into(), label: None }]));
    }

    #[test]
    fn reads_session_options() {
        let (options, legacy) = session_options(&json!({ "configOptions": [
            { "id": "model", "name": "Model", "category": "model", "type": "select", "currentValue": "opus",
              "options": [{ "value": "opus", "name": "Opus" }, { "group": "Old", "options": [{ "value": "old", "name": "Old" }] }] },
            { "id": "flag", "name": "Flag", "type": "boolean", "currentValue": true }
        ]}));
        assert!(!legacy);
        assert_eq!(options.len(), 1);
        assert!(options[0].is_model());
        assert_eq!(options[0].current_name(), "Opus");
        assert_eq!(options[0].values.iter().map(|v| v.value.as_str()).collect::<Vec<_>>(), ["opus", "old"]);

        let (options, legacy) = session_options(&json!({ "models": {
            "currentModelId": "b", "availableModels": [{ "modelId": "a", "name": "A" }, { "modelId": "b", "name": "B" }] } }));
        assert!(legacy);
        assert_eq!((options[0].id.as_str(), options[0].current_name()), (LEGACY_MODEL, "B"));
    }
}
