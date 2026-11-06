//! Prefix-gated stdio MCP launcher sources. All filesystem and process I/O is
//! owned by a worker; the compositor only sends messages and receives events.

use std::{
    collections::BTreeMap,
    ffi::OsString,
    io::{BufReader, Read, Write},
    os::{
        fd::AsRawFd,
        unix::{net::UnixDatagram, process::CommandExt},
    },
    path::PathBuf,
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
    sync::{
        Arc, RwLock,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use smithay::reexports::calloop::channel;

pub type Environment = Vec<(OsString, Option<OsString>)>;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LauncherConfig {
    pub plugins: BTreeMap<String, PluginConfig>,
}

/// Optional values overlay the discovered manifest with the same file stem.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PluginConfig {
    pub command: Option<String>,
    pub args: Option<Vec<String>>,
    pub prefixes: Option<Vec<String>>,
    pub enabled: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Row {
    pub title: String,
    #[serde(default)]
    pub detail: String,
    #[serde(default)]
    pub icon: Option<String>,
    #[serde(default)]
    pub actions: Vec<Action>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Action {
    pub title: String,
    #[serde(flatten)]
    pub kind: ActionKind,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ActionKind {
    Copy {
        text: String,
    },
    Open {
        target: String,
    },
    Run {
        command: String,
        #[serde(default)]
        args: Vec<String>,
    },
    Tool {
        name: String,
        #[serde(default = "empty_object")]
        arguments: Value,
    },
}

fn empty_object() -> Value {
    json!({})
}

#[derive(Debug, Clone, Deserialize)]
pub struct Info {
    pub name: String,
    pub description: String,
    #[serde(default)]
    pub icon: Option<String>,
    pub prefixes: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct Route {
    pub plugin_id: String,
    pub info: Info,
    pub prefixes: Vec<String>,
}

pub enum Event {
    Ready { routes: Vec<Route>, diagnostics: Vec<String> },
    Rows { generation: u64, plugin: String, result: Result<Vec<Row>, String> },
    Action { generation: u64, result: Result<(), String> },
}

enum Request {
    Configure(LauncherConfig),
    Search { generation: u64, plugin: String, prefix: String, query: String },
    Tool { generation: u64, plugin: String, name: String, arguments: Value },
    Stop,
}

pub struct Plugins {
    sender: mpsc::Sender<Request>,
    generation: Arc<AtomicU64>,
    config: LauncherConfig,
    stopping: Arc<AtomicBool>,
    wake: Arc<UnixDatagram>,
    environment: Arc<RwLock<Environment>>,
    worker: Option<std::thread::JoinHandle<()>>,
    pub routes: Vec<Route>,
    pub diagnostics: Vec<String>,
}

impl Plugins {
    pub fn start(config: LauncherConfig, events: channel::Sender<Event>, environment: Environment) -> Self {
        let (sender, requests) = mpsc::channel();
        let generation = Arc::new(AtomicU64::new(0));
        let current = generation.clone();
        let stopping = Arc::new(AtomicBool::new(false));
        let shutdown = stopping.clone();
        let (wake, wake_reader) = UnixDatagram::pair().expect("plugin cancellation socket");
        wake.set_nonblocking(true).expect("nonblocking plugin cancellation sender");
        wake_reader.set_nonblocking(true).expect("nonblocking plugin cancellation receiver");
        let wake = Arc::new(wake);
        let environment = Arc::new(RwLock::new(environment));
        let spawn_environment = environment.clone();
        let worker = Some(std::thread::spawn(move || {
            worker(requests, events, current, shutdown, spawn_environment, Arc::new(wake_reader))
        }));
        let _ = sender.send(Request::Configure(config.clone()));
        Self {
            sender,
            generation,
            config,
            stopping,
            wake,
            environment,
            worker,
            routes: Vec::new(),
            diagnostics: Vec::new(),
        }
    }

    pub fn configure(&mut self, config: &LauncherConfig) {
        if self.config != *config {
            self.cancel();
            self.config = config.clone();
            self.routes.clear();
            let _ = self.sender.send(Request::Configure(config.clone()));
        }
    }

    pub fn cancel(&self) -> u64 {
        let generation = self.generation.fetch_add(1, Ordering::Relaxed) + 1;
        let _ = self.wake.send(&[1]);
        generation
    }

    /// Gives plugins Xwayland's display once it is ready. Plugins already
    /// running started without it, so they start again.
    pub fn set_display(&mut self, display: u32) {
        {
            let mut environment = self.environment.write().expect("plugin environment lock");
            environment.retain(|(key, _)| key != "DISPLAY");
            environment.push(("DISPLAY".into(), Some(format!(":{display}").into())));
        }
        self.cancel();
        self.routes.clear();
        let _ = self.sender.send(Request::Configure(self.config.clone()));
    }

    pub fn matches(&self, text: &str) -> Option<(&Route, &str)> {
        self.routes.iter().find_map(|route| {
            route.prefixes.iter().find_map(|prefix| {
                let rest = text.strip_prefix(prefix)?;
                (rest.is_empty() || rest.starts_with(char::is_whitespace)).then_some((route, prefix.as_str()))
            })
        })
    }

    pub fn search(&self, text: &str) -> Option<(u64, String)> {
        let generation = self.cancel();
        let (route, prefix) = self.matches(text)?;
        let _ = self.sender.send(Request::Search {
            generation,
            plugin: route.plugin_id.clone(),
            prefix: prefix.into(),
            query: text[prefix.len()..].trim_start().into(),
        });
        Some((generation, route.plugin_id.clone()))
    }

    pub fn tool(&self, plugin: String, name: String, arguments: Value) -> u64 {
        let generation = self.cancel();
        let _ = self.sender.send(Request::Tool { generation, plugin, name, arguments });
        generation
    }
}

impl Drop for Plugins {
    fn drop(&mut self) {
        self.stopping.store(true, Ordering::Relaxed);
        self.cancel();
        let _ = self.sender.send(Request::Stop);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

struct Source {
    name: String,
    config: PluginConfig,
    session: Option<Session>,
    retry_after: Instant,
    stopping: Arc<AtomicBool>,
    environment: Arc<RwLock<Environment>>,
    wake: Arc<UnixDatagram>,
}

fn worker(
    requests: mpsc::Receiver<Request>,
    events: channel::Sender<Event>,
    current: Arc<AtomicU64>,
    stopping: Arc<AtomicBool>,
    environment: Arc<RwLock<Environment>>,
    wake: Arc<UnixDatagram>,
) {
    let mut sources = BTreeMap::<String, Source>::new();
    loop {
        for source in sources.values_mut() {
            if let Some(session) = &mut source.session
                && let Err(error) = session.reap_cancelled()
            {
                tracing::warn!("Plugin {}: {error}", source.name);
                source.session = None;
                source.retry_after = Instant::now() + Duration::from_secs(1);
            }
        }
        let deadline =
            sources.values().filter_map(|source| source.session.as_ref()?.cancelled.values().min().copied()).min();
        let request = if let Some(deadline) = deadline {
            match requests.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
                Ok(request) => request,
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        } else {
            let Ok(request) = requests.recv() else { break };
            request
        };
        if stopping.load(Ordering::Relaxed) {
            break;
        }
        match request {
            Request::Stop => break,
            Request::Configure(config) => {
                if !configure_sources(config, &mut sources, &events, &stopping, &environment, &wake) {
                    break;
                }
            }
            Request::Search { generation, plugin, prefix, query } => {
                if current.load(Ordering::Relaxed) != generation {
                    continue;
                }
                let result = invoke(
                    &mut sources,
                    &plugin,
                    "launcher_search",
                    json!({"query":query,"prefix":prefix}),
                    &current,
                    generation,
                )
                .and_then(parse_rows);
                if current.load(Ordering::Relaxed) == generation
                    && events.send(Event::Rows { generation, plugin, result }).is_err()
                {
                    break;
                }
            }
            Request::Tool { generation, plugin, name, arguments } => {
                if current.load(Ordering::Relaxed) != generation {
                    continue;
                }
                let result = invoke(&mut sources, &plugin, &name, arguments, &current, generation).map(|_| ());
                if current.load(Ordering::Relaxed) == generation
                    && events.send(Event::Action { generation, result }).is_err()
                {
                    break;
                }
            }
        }
    }
}

fn configure_sources(
    config: LauncherConfig,
    sources: &mut BTreeMap<String, Source>,
    events: &channel::Sender<Event>,
    stopping: &Arc<AtomicBool>,
    environment: &Arc<RwLock<Environment>>,
    wake: &Arc<UnixDatagram>,
) -> bool {
    sources.clear();
    let (configs, mut diagnostics) = discover(config);
    let mut routes = Vec::new();
    let mut claimed = BTreeMap::<String, String>::new();
    for (plugin_id, config) in configs {
        if stopping.load(Ordering::Relaxed) {
            return false;
        }
        if config.enabled == Some(false) {
            continue;
        }
        let mut source = Source {
            name: plugin_id.clone(),
            wake: wake.clone(),
            config,
            session: None,
            retry_after: Instant::now(),
            stopping: stopping.clone(),
            environment: environment.clone(),
        };
        // Metadata discovery is independent of search generations.
        let result = source.connect().and_then(|session| session.tool("launcher_info", json!({}), None));
        match result.and_then(|value| {
            serde_json::from_value::<Info>(value).map_err(|error| PluginError::Tool(error.to_string()))
        }) {
            Ok(info) => {
                let mut prefixes = source.config.prefixes.clone().unwrap_or_else(|| info.prefixes.clone());
                prefixes.retain(|prefix| {
                    if prefix.is_empty() || prefix == "plugins" || prefix.chars().any(char::is_whitespace) {
                        diagnostics
                            .push(format!("Plugin {plugin_id}: prefix must be a nonempty word other than plugins"));
                        return false;
                    }
                    if let Some(winner) = claimed.get(prefix) {
                        diagnostics.push(format!("Plugin {plugin_id}: prefix {prefix:?} belongs to {winner}"));
                        return false;
                    }
                    claimed.insert(prefix.clone(), plugin_id.clone());
                    true
                });
                if prefixes.is_empty() {
                    diagnostics.push(format!("Plugin {plugin_id}: no usable prefixes"));
                } else {
                    routes.push(Route { plugin_id: plugin_id.clone(), info, prefixes });
                    sources.insert(plugin_id, source);
                }
            }
            Err(error) => diagnostics.push(format!("Plugin {plugin_id}: {error}")),
        }
    }
    for diagnostic in &diagnostics {
        tracing::warn!("{diagnostic}");
    }
    events.send(Event::Ready { routes, diagnostics }).is_ok()
}

fn invoke(
    sources: &mut BTreeMap<String, Source>,
    plugin: &str,
    name: &str,
    arguments: Value,
    current: &AtomicU64,
    generation: u64,
) -> Result<Value, String> {
    let source = sources.get_mut(plugin).ok_or("Plugin is unavailable")?;
    if Instant::now() < source.retry_after {
        return Err("Plugin is restarting; try again shortly".into());
    }
    let result = source.connect().and_then(|session| session.tool(name, arguments, Some((current, generation))));
    if matches!(result, Err(PluginError::Transport(_))) {
        source.session = None;
        source.retry_after = Instant::now() + Duration::from_secs(1);
    }
    result.map_err(|error| error.to_string())
}

impl Source {
    fn connect(&mut self) -> Result<&mut Session, PluginError> {
        if self.session.is_none() {
            if Instant::now() < self.retry_after {
                return Err("Plugin is restarting; try again shortly".into());
            }
            let environment = self.environment.read().expect("plugin environment lock").clone();
            self.session =
                Some(Session::start(&self.name, &self.config, self.stopping.clone(), &environment, self.wake.clone())?);
        }
        self.session.as_mut().ok_or_else(|| "Plugin failed to start".into())
    }
}

/// Highest-priority manifests are visited first so prefix collisions have a
/// deterministic winner. Config overrides inherit unspecified manifest fields.
fn discover(config: LauncherConfig) -> (Vec<(String, PluginConfig)>, Vec<String>) {
    let home = PathBuf::from(std::env::var_os("HOME").unwrap_or_default());
    let user = std::env::var_os("XDG_DATA_HOME").map(PathBuf::from).unwrap_or_else(|| home.join(".local/share"));
    let mut manifests = BTreeMap::<String, (u8, PluginConfig)>::new();
    let mut diagnostics = Vec::new();
    for (priority, directory) in
        [(0, PathBuf::from("/usr/share/tileception/plugins")), (1, user.join("tileception/plugins"))]
    {
        let entries = match std::fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                diagnostics.push(format!("{}: {error}", directory.display()));
                continue;
            }
        };
        let mut paths: Vec<_> = entries.filter_map(Result::ok).map(|entry| entry.path()).collect();
        paths.sort();
        for path in paths {
            if path.extension().is_none_or(|extension| extension != "toml") {
                continue;
            }
            let Some(plugin_id) = path.file_stem().and_then(|stem| stem.to_str()) else {
                continue;
            };
            match std::fs::read_to_string(&path)
                .map_err(|error| error.to_string())
                .and_then(|text| toml::from_str(&text).map_err(|error| error.to_string()))
            {
                Ok(value) => {
                    manifests.insert(plugin_id.into(), (priority, value));
                }
                Err(error) => diagnostics.push(format!("{}: {error}", path.display())),
            }
        }
    }
    for (plugin_id, overlay) in config.plugins {
        let (priority, base) = manifests.entry(plugin_id).or_default();
        *priority = 2;
        if overlay.command.is_some() {
            base.command = overlay.command;
        }
        if overlay.args.is_some() {
            base.args = overlay.args;
        }
        if overlay.prefixes.is_some() {
            base.prefixes = overlay.prefixes;
        }
        if overlay.enabled.is_some() {
            base.enabled = overlay.enabled;
        }
    }
    let mut values: Vec<_> = manifests.into_iter().collect();
    values.sort_by(|(left_id, (left, _)), (right_id, (right, _))| right.cmp(left).then(left_id.cmp(right_id)));
    (values.into_iter().map(|(plugin_id, (_, config))| (plugin_id, config)).collect(), diagnostics)
}

const LIMIT: usize = 1024 * 1024;
const TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Debug, PartialEq)]
enum PluginError {
    Tool(String),
    Transport(String),
    Cancelled,
}

impl From<String> for PluginError {
    fn from(message: String) -> Self {
        Self::Transport(message)
    }
}

impl From<&str> for PluginError {
    fn from(message: &str) -> Self {
        Self::Transport(message.into())
    }
}

impl std::fmt::Display for PluginError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Tool(message) | Self::Transport(message) => formatter.write_str(message),
            Self::Cancelled => formatter.write_str("Plugin request cancelled"),
        }
    }
}

fn parse_rows(value: Value) -> Result<Vec<Row>, String> {
    let rows = value["rows"].as_array().ok_or("Plugin result has no rows array")?;
    let mut parsed = Vec::new();
    for value in rows {
        let mut value = value.clone();
        let actions = value.as_object_mut().and_then(|row| row.remove("actions"));
        match serde_json::from_value::<Row>(value) {
            Ok(mut row) => {
                if let Some(actions) = actions {
                    if let Some(actions) = actions.as_array() {
                        for action in actions {
                            match serde_json::from_value::<Action>(action.clone()) {
                                Ok(action) => row.actions.push(action),
                                Err(error) => tracing::warn!("Skipping invalid launcher plugin action: {error}"),
                            }
                            if row.actions.len() == 16 {
                                break;
                            }
                        }
                    } else {
                        tracing::warn!("Skipping invalid launcher plugin actions: expected an array");
                    }
                }
                parsed.push(row);
                if parsed.len() == 64 {
                    break;
                }
            }
            Err(error) => tracing::warn!("Skipping invalid launcher plugin row: {error}"),
        }
    }
    Ok(parsed)
}

fn tool_result(result: Value) -> Result<Value, PluginError> {
    if result["isError"] == true {
        let message = result["content"]
            .as_array()
            .map(|content| content.iter().filter_map(|item| item["text"].as_str()).collect::<Vec<_>>().join("\n"))
            .filter(|message| !message.is_empty())
            .unwrap_or_else(|| "Plugin tool failed".into());
        return Err(PluginError::Tool(message));
    }
    if let Some(value) = result.get("structuredContent") {
        return Ok(value.clone());
    }
    let text = result["content"]
        .as_array()
        .and_then(|content| {
            content.iter().find_map(|item| (item["type"] == "text").then(|| item["text"].as_str()).flatten())
        })
        .ok_or_else(|| PluginError::Tool("Plugin tool returned no structured content".into()))?;
    serde_json::from_str(text).map_err(|error| PluginError::Tool(error.to_string()))
}

struct Session {
    child: Child,
    input: ChildStdin,
    output: ChildStdout,
    buffered: Vec<u8>,
    next_id: u64,
    stopping: Arc<AtomicBool>,
    wake: Arc<UnixDatagram>,
    cancelled: BTreeMap<u64, Instant>,
}

impl Session {
    fn start(
        name: &str,
        config: &PluginConfig,
        stopping: Arc<AtomicBool>,
        environment: &Environment,
        wake: Arc<UnixDatagram>,
    ) -> Result<Self, PluginError> {
        let command =
            config.command.as_deref().filter(|command| !command.is_empty()).ok_or("Missing executable command")?;
        let mut process = Command::new(command);
        for (key, value) in environment {
            match value {
                Some(value) => {
                    process.env(key, value);
                }
                None => {
                    process.env_remove(key);
                }
            }
        }
        let mut child = process
            .args(config.args.as_deref().unwrap_or_default())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0)
            .spawn()
            .map_err(|error| error.to_string())?;
        let input = child.stdin.take().ok_or("Missing plugin stdin")?;
        let output = child.stdout.take().ok_or("Missing plugin stdout")?;
        let stderr = child.stderr.take().ok_or("Missing plugin stderr")?;
        let name = name.to_string();
        std::thread::spawn(move || {
            let mut reader = BufReader::new(stderr);
            let mut buffer = [0; 4096];
            let mut line = Vec::new();
            loop {
                match reader.read(&mut buffer) {
                    Ok(0) | Err(_) => break,
                    Ok(count) => {
                        for byte in &buffer[..count] {
                            if *byte == b'\n' || line.len() == 4096 {
                                tracing::info!("Plugin {name}: {}", String::from_utf8_lossy(&line));
                                line.clear();
                            }
                            if *byte != b'\n' {
                                line.push(*byte);
                            }
                        }
                    }
                }
            }
            if !line.is_empty() {
                tracing::info!("Plugin {name}: {}", String::from_utf8_lossy(&line));
            }
        });
        let mut session =
            Self { child, input, output, buffered: Vec::new(), next_id: 0, stopping, wake, cancelled: BTreeMap::new() };
        for descriptor in [session.input.as_raw_fd(), session.output.as_raw_fd()] {
            #[allow(unsafe_code, reason = "fcntl sets nonblocking mode on owned plugin pipes")]
            // SAFETY: these descriptors belong to live pipes owned by this session.
            let result = unsafe { libc::fcntl(descriptor, libc::F_SETFL, libc::O_NONBLOCK) };
            if result < 0 {
                return Err(std::io::Error::last_os_error().to_string().into());
            }
        }
        let initialized = session.request(
            "initialize",
            json!({
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": { "name": "tileception-launcher", "version": env!("CARGO_PKG_VERSION") }
            }),
            None,
        )?;
        if initialized["protocolVersion"] != "2025-06-18" || !initialized["capabilities"]["tools"].is_object() {
            return Err("Plugin must support MCP 2025-06-18 tools".into());
        }
        session.write(json!({"jsonrpc":"2.0","method":"notifications/initialized"}), Instant::now() + TIMEOUT, None)?;
        Ok(session)
    }

    fn tool(&mut self, name: &str, arguments: Value, cancel: Option<(&AtomicU64, u64)>) -> Result<Value, PluginError> {
        let result = self.request("tools/call", json!({ "name": name, "arguments": arguments }), cancel)?;
        tool_result(result)
    }

    fn write(&mut self, value: Value, deadline: Instant, cancel: Option<(&AtomicU64, u64)>) -> Result<(), PluginError> {
        let mut bytes = serde_json::to_vec(&value).map_err(|error| error.to_string())?;
        if bytes.len() > LIMIT {
            return Err("Plugin request exceeds 1 MiB".into());
        }
        bytes.push(b'\n');
        let mut offset = 0;
        while offset < bytes.len() {
            if self.stopping.load(Ordering::Relaxed) {
                return Err("Plugin host is stopping".into());
            }
            check(deadline, cancel)?;
            match self.input.write(&bytes[offset..]) {
                Ok(0) => return Err("Plugin closed stdin".into()),
                Ok(count) => offset += count,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    self.wait(self.input.as_raw_fd(), libc::POLLOUT, deadline)?;
                }
                Err(error) => return Err(error.to_string().into()),
            }
        }
        Ok(())
    }

    fn request(
        &mut self,
        method: &str,
        params: Value,
        cancel: Option<(&AtomicU64, u64)>,
    ) -> Result<Value, PluginError> {
        self.next_id += 1;
        let request_id = self.next_id;
        let deadline = Instant::now() + TIMEOUT;
        check(deadline, cancel)?;
        // Finish the frame before observing cancellation so the stream stays valid.
        self.write(json!({ "jsonrpc": "2.0", "id": request_id, "method": method, "params": params }), deadline, None)?;
        loop {
            if self.stopping.load(Ordering::Relaxed) {
                return Err("Plugin host is stopping".into());
            }
            if let Err(error) = check(deadline, cancel) {
                if error == PluginError::Cancelled {
                    self.write(
                        json!({
                            "jsonrpc": "2.0",
                            "method": "notifications/cancelled",
                            "params": { "requestId": request_id, "reason": error.to_string() }
                        }),
                        deadline,
                        None,
                    )?;
                    self.cancelled.insert(request_id, deadline);
                }
                return Err(error);
            }
            if let Some(end) = self.buffered.iter().position(|byte| *byte == b'\n') {
                let line: Vec<_> = self.buffered.drain(..=end).collect();
                let message: Value = serde_json::from_slice(&line).map_err(|error| error.to_string())?;
                if message["jsonrpc"] != "2.0" {
                    return Err("Invalid JSON-RPC version".into());
                }
                if message.get("method").is_some() {
                    if message.get("id").is_some() {
                        let response = if message["method"] == "ping" {
                            json!({"jsonrpc":"2.0","id":message["id"],"result":{}})
                        } else {
                            json!({
                                "jsonrpc": "2.0", "id": message["id"],
                                "error": { "code": -32601, "message": "Unsupported client method" }
                            })
                        };
                        self.write(response, deadline, None)?;
                    }
                    continue;
                }
                if message["id"] != request_id {
                    if let Some(request_id) = message["id"].as_u64() {
                        self.cancelled.remove(&request_id);
                    }
                    continue;
                }
                if let Some(error) = message.get("error") {
                    return Err(format!("MCP error: {error}").into());
                }
                return message.get("result").cloned().ok_or_else(|| "MCP response has no result".into());
            }
            let mut buffer = [0; 8192];
            match self.output.read(&mut buffer) {
                Ok(0) => return Err("Plugin exited or closed stdout".into()),
                Ok(count) => {
                    self.buffered.extend_from_slice(&buffer[..count]);
                    if self.buffered.len() > LIMIT {
                        return Err("Plugin response exceeds 1 MiB".into());
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    self.wait(self.output.as_raw_fd(), libc::POLLIN, deadline)?;
                }
                Err(error) => return Err(error.to_string().into()),
            }
        }
    }

    fn wait(&self, descriptor: i32, events: i16, deadline: Instant) -> Result<(), PluginError> {
        let deadline = self.cancelled.values().copied().min().map_or(deadline, |pending| pending.min(deadline));
        check(deadline, None)?;
        let mut descriptors = [
            libc::pollfd { fd: descriptor, events, revents: 0 },
            libc::pollfd { fd: self.wake.as_raw_fd(), events: libc::POLLIN, revents: 0 },
        ];
        let remaining = deadline.saturating_duration_since(Instant::now());
        let timeout = i32::try_from(remaining.as_millis().saturating_add(1)).unwrap_or(i32::MAX);
        #[allow(unsafe_code, reason = "poll waits on owned plugin and cancellation descriptors")]
        // SAFETY: descriptors points to two initialized pollfd values for live descriptors.
        let result = unsafe { libc::poll(descriptors.as_mut_ptr(), descriptors.len() as libc::nfds_t, timeout) };
        if result < 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() != std::io::ErrorKind::Interrupted {
                return Err(error.to_string().into());
            }
        }
        if descriptors[1].revents != 0 {
            let mut buffer = [0; 128];
            while self.wake.recv(&mut buffer).is_ok() {}
        }
        Ok(())
    }

    /// Drain superseded responses while idle, enforcing their original deadlines.
    fn reap_cancelled(&mut self) -> Result<(), PluginError> {
        if self.cancelled.is_empty() {
            return Ok(());
        }
        let mut buffer = [0; 8192];
        loop {
            match self.output.read(&mut buffer) {
                Ok(0) => return Err("Plugin exited or closed stdout".into()),
                Ok(count) => {
                    self.buffered.extend_from_slice(&buffer[..count]);
                    if self.buffered.len() > LIMIT {
                        return Err("Plugin response exceeds 1 MiB".into());
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(error) => return Err(error.to_string().into()),
            }
        }
        while let Some(end) = self.buffered.iter().position(|byte| *byte == b'\n') {
            let line: Vec<_> = self.buffered.drain(..=end).collect();
            let message: Value = serde_json::from_slice(&line).map_err(|error| error.to_string())?;
            if message["jsonrpc"] != "2.0" {
                return Err("Invalid JSON-RPC version".into());
            }
            if message.get("method").is_some() && message.get("id").is_some() {
                let response = if message["method"] == "ping" {
                    json!({ "jsonrpc": "2.0", "id": message["id"], "result": {} })
                } else {
                    json!({
                        "jsonrpc": "2.0", "id": message["id"],
                        "error": { "code": -32601, "message": "Unsupported client method" }
                    })
                };
                self.write(response, Instant::now() + TIMEOUT, None)?;
            } else if let Some(request_id) = message["id"].as_u64() {
                self.cancelled.remove(&request_id);
            }
        }
        if let Some(deadline) = self.cancelled.values().min() {
            check(*deadline, None)?;
        }
        Ok(())
    }
}

fn check(deadline: Instant, cancel: Option<(&AtomicU64, u64)>) -> Result<(), PluginError> {
    if cancel.is_some_and(|(current, generation)| current.load(Ordering::Relaxed) != generation) {
        return Err(PluginError::Cancelled);
    }
    if Instant::now() >= deadline {
        return Err("Plugin timed out after 3 seconds".into());
    }
    Ok(())
}

impl Drop for Session {
    fn drop(&mut self) {
        // Kill the isolated process group, including plugin subprocesses.
        if let Ok(pid) = i32::try_from(self.child.id()) {
            #[allow(unsafe_code, reason = "terminate only the process group created for this plugin")]
            // SAFETY: the positive child pid identifies our isolated process group.
            unsafe {
                libc::kill(-pid, libc::SIGKILL);
            }
        }
        let _ = self.child.wait();
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test setup and assertions")]
mod tests {
    use super::*;

    #[test]
    fn skips_invalid_rows_and_actions_independently() {
        let rows = parse_rows(json!({ "rows": [
            { "title": "First", "actions": [
                { "title": "Future", "type": "future" },
                { "title": "Copy", "type": "copy", "text": "hello" },
                { "type": "copy", "text": "missing title" }
            ] },
            { "detail": "missing title" },
            { "title": 17 },
            { "title": "Last" }
        ] }))
        .unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].title, "First");
        assert_eq!(rows[0].actions.len(), 1);
        assert_eq!(rows[0].actions[0].kind, ActionKind::Copy { text: "hello".into() });
        assert_eq!(rows[1].title, "Last");
    }

    #[test]
    fn tool_error_is_a_normal_answer() {
        assert_eq!(
            tool_result(json!({
                "isError": true, "content": [{ "type": "text", "text": "Please sign in" }]
            })),
            Err(PluginError::Tool("Please sign in".into()))
        );
    }

    #[test]
    fn tool_error_and_cancelled_search_keep_the_session() {
        let script = r#"
import json, os, sys
sys.stderr.write('startup log ' * 8192 + '\n')
sys.stderr.flush()
first = None
for line in sys.stdin:
    message = json.loads(line)
    mid = message.get('id')
    method = message.get('method')
    if method == 'initialize':
        result = {'protocolVersion': '2025-06-18', 'capabilities': {'tools': {}}}
    elif method == 'tools/call':
        name = message['params']['name']
        if name == 'slow':
            first = mid
            continue
        if first is not None:
            print(json.dumps({'jsonrpc': '2.0', 'id': first, 'result': {'structuredContent': {'stale': True}}}), flush=True)
            first = None
        result = {'isError': True, 'content': [{'type': 'text', 'text': 'Please sign in'}]} if name == 'fail' else {'structuredContent': {'ok': True}}
        if name == 'display':
            result = {'structuredContent': {'display': os.environ.get('DISPLAY')}}
    else:
        continue
    print(json.dumps({'jsonrpc': '2.0', 'id': mid, 'result': result}), flush=True)
"#;
        let (wake, reader) = UnixDatagram::pair().unwrap();
        wake.set_nonblocking(true).unwrap();
        reader.set_nonblocking(true).unwrap();
        let mut sources = BTreeMap::from([(
            "test".into(),
            Source {
                name: "test".into(),
                config: PluginConfig {
                    command: Some("python3".into()),
                    args: Some(vec!["-u".into(), "-c".into(), script.into()]),
                    ..Default::default()
                },
                session: None,
                retry_after: Instant::now(),
                stopping: Arc::new(AtomicBool::new(false)),
                environment: Arc::new(RwLock::new(Vec::new())),
                wake: Arc::new(reader),
            },
        )]);
        let current = Arc::new(AtomicU64::new(0));
        assert_eq!(invoke(&mut sources, "test", "fail", json!({}), &current, 0), Err("Please sign in".into()));
        let pid = sources["test"].session.as_ref().unwrap().child.id();
        let generation = current.clone();
        let cancel = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            generation.store(1, Ordering::Relaxed);
            wake.send(&[1]).unwrap();
        });
        assert_eq!(
            invoke(&mut sources, "test", "slow", json!({}), &current, 0),
            Err("Plugin request cancelled".into())
        );
        cancel.join().unwrap();
        assert_eq!(invoke(&mut sources, "test", "ok", json!({}), &current, 1), Ok(json!({ "ok": true })));
        assert_eq!(sources["test"].session.as_ref().unwrap().child.id(), pid);
        let source = sources.get_mut("test").unwrap();
        source.session = None;
        source.environment.write().unwrap().push(("DISPLAY".into(), Some(":42".into())));
        assert_eq!(invoke(&mut sources, "test", "display", json!({}), &current, 1), Ok(json!({ "display": ":42" })));
    }
}
