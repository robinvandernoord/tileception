//! One running agent: newline-delimited JSON-RPC over the child's stdio.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::thread::{self, JoinHandle};

use serde_json::{json, Value};

use crate::{install, Action, Agent, Choice, Emit, Event, FileOp, Login, Request, Status};

/// The protocol version spoken here.
const PROTOCOL_VERSION: u64 = 1;
/// The agent wants a login first.
const AUTH_REQUIRED: i64 = -32000;
const METHOD_NOT_FOUND: i64 = -32601;
const INTERNAL_ERROR: i64 = -32603;

enum Msg {
    User(Request),
    Line(Value),
    Closed,
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
    next_id: u64,
    pending: HashMap<u64, Pending>,
    logins: Vec<Login>,
    session: Option<String>,
    /// A prompt typed before the session was ready.
    queued: Option<String>,
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
            context: Some(context).filter(|c| !c.is_empty()),
            emit,
            child: None,
            stdin: None,
            program: None,
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
        self.spawn(&program, sender)?;
        self.program = Some(program);
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
        )?;
        loop {
            match incoming.recv() {
                Ok(Msg::Stop) | Err(_) => return Ok(()),
                Ok(Msg::Closed) => return Err(self.exit_reason()),
                Ok(Msg::Line(message)) => self.message(message)?,
                Ok(Msg::User(request)) => self.user(request)?,
            }
        }
    }

    fn spawn(&mut self, program: &install::Program, sender: &mpsc::Sender<Msg>) -> Result<(), String> {
        let mut command = Command::new(&program.command);
        command.args(&program.args).current_dir(&self.cwd);
        crate::with_env(&mut command, &program.env);
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

        let lines = sender.clone();
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
                            if lines.send(Msg::Line(value)).is_err() {
                                return;
                            }
                        }
                        Err(_) => tracing::debug!("agent wrote something that is not JSON: {line}"),
                    }
                }
                let _ = lines.send(Msg::Closed);
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
        let cwd = self.cwd.display().to_string();
        self.request("session/new", json!({ "cwd": cwd, "mcpServers": [] }), Pending::NewSession)
    }

    fn prompt(&mut self, text: String) -> Result<(), String> {
        let Some(session) = self.session.clone() else {
            self.queued = Some(text);
            return Ok(());
        };
        let mut blocks = Vec::new();
        if let Some(context) = self.context.take() {
            blocks.push(json!({ "type": "text", "text": context }));
        }
        blocks.push(json!({ "type": "text", "text": text }));
        self.request("session/prompt", json!({ "sessionId": session, "prompt": blocks }), Pending::Prompt)
    }

    fn user(&mut self, request: Request) -> Result<(), String> {
        match request {
            Request::Prompt(text) => self.prompt(text)?,
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
            Request::Login(method) => match self.logins.iter().find(|l| l.id == method).cloned() {
                Some(Login { terminal: Some(program), .. }) => {
                    self.status(Status::LoggingIn);
                    (self.emit)(Event::Terminal(program));
                }
                Some(_) => {
                    self.status(Status::LoggingIn);
                    self.request("authenticate", json!({ "methodId": method }), Pending::Authenticate)?;
                }
                None => (self.emit)(Event::Note(format!("{method} is not one of the agent's logins"))),
            },
            Request::Retry => self.new_session()?,
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
                if method == "session/update" {
                    if let Some(update) = message.get("params").and_then(|p| p.get("update")) {
                        self.update(update);
                    }
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
                self.new_session()
            }
            Pending::NewSession => {
                let Some(session) = result.get("sessionId").and_then(Value::as_str) else {
                    return Err("the agent started no session".into());
                };
                self.session = Some(session.to_string());
                self.status(Status::Ready);
                if let Some(text) = self.queued.take() {
                    self.prompt(text)?;
                }
                Ok(())
            }
            Pending::Authenticate => self.new_session(),
            Pending::Prompt => {
                let reason = result.get("stopReason").and_then(Value::as_str).unwrap_or("end_turn");
                (self.emit)(Event::TurnEnded(reason.to_string()));
                Ok(())
            }
        }
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
            let terminal = (m.get("type").and_then(Value::as_str) == Some("terminal")).then(|| {
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
            Some(Login { id, name, description, terminal: terminal.or(suggested) })
        })
        .collect()
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
        assert_eq!((found[0].id.as_str(), found[0].terminal.is_none()), ("claude-login", true));
        let terminal = found[1].terminal.as_ref().expect("terminal login");
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
        let configure = hinted[0].terminal.as_ref().expect("suggested terminal login");
        assert_eq!(
            (configure.command.to_str(), configure.args.as_slice()),
            (Some("/agents/goose/1/goose"), &["configure".to_string()][..])
        );
        assert!(suggested_command("Run `rm -rf /` first", Some(&goose)).is_none());
        assert!(suggested_command("Run `goose x; rm -rf /`", Some(&goose)).is_none());
        assert_eq!(terminal.args, ["-y", "agent", "--login"]);
        assert_eq!(terminal.env["A"], "1");
    }
}
