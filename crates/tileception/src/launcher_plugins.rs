//! Prefix-gated stdio MCP launcher sources. All filesystem and process I/O is
//! owned by a worker; the compositor only sends messages and receives events.

use std::{
    collections::BTreeMap,
    ffi::OsString,
    io::{Read, Write},
    os::{fd::AsRawFd, unix::process::CommandExt},
    path::PathBuf,
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
    sync::{
        Arc,
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
        let worker = Some(std::thread::spawn(move || worker(requests, events, current, shutdown, environment)));
        let _ = sender.send(Request::Configure(config.clone()));
        Self { sender, generation, config, stopping, worker, routes: Vec::new(), diagnostics: Vec::new() }
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
        self.generation.fetch_add(1, Ordering::Relaxed) + 1
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
    config: PluginConfig,
    session: Option<Session>,
    retry_after: Instant,
    stopping: Arc<AtomicBool>,
    environment: Arc<Environment>,
}

fn worker(
    requests: mpsc::Receiver<Request>,
    events: channel::Sender<Event>,
    current: Arc<AtomicU64>,
    stopping: Arc<AtomicBool>,
    environment: Environment,
) {
    let environment = Arc::new(environment);
    let mut sources = BTreeMap::<String, Source>::new();
    while let Ok(request) = requests.recv() {
        if stopping.load(Ordering::Relaxed) {
            break;
        }
        match request {
            Request::Stop => break,
            Request::Configure(config) => {
                sources.clear();
                let (configs, mut diagnostics) = discover(config);
                let mut routes = Vec::new();
                let mut claimed = BTreeMap::<String, String>::new();
                for (plugin_id, config) in configs {
                    if stopping.load(Ordering::Relaxed) {
                        return;
                    }
                    if config.enabled == Some(false) {
                        continue;
                    }
                    let mut source = Source {
                        config,
                        session: None,
                        retry_after: Instant::now(),
                        stopping: stopping.clone(),
                        environment: environment.clone(),
                    };
                    // Metadata discovery is independent of search generations.
                    let result = source.connect().and_then(|session| session.tool("launcher_info", json!({}), None));
                    match result
                        .and_then(|value| serde_json::from_value::<Info>(value).map_err(|error| error.to_string()))
                    {
                        Ok(info) => {
                            let mut prefixes = source.config.prefixes.clone().unwrap_or_else(|| info.prefixes.clone());
                            prefixes.retain(|prefix| {
                                if prefix.is_empty() || prefix == "plugins" || prefix.chars().any(char::is_whitespace) {
                                    diagnostics.push(format!(
                                        "Plugin {plugin_id}: prefix must be a nonempty word other than plugins"
                                    ));
                                    return false;
                                }
                                if let Some(winner) = claimed.get(prefix) {
                                    diagnostics
                                        .push(format!("Plugin {plugin_id}: prefix {prefix:?} belongs to {winner}"));
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
                if events.send(Event::Ready { routes, diagnostics }).is_err() {
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
                .and_then(|value| {
                    #[derive(Deserialize)]
                    struct Results {
                        rows: Vec<Row>,
                    }
                    let mut rows = serde_json::from_value::<Results>(value).map_err(|error| error.to_string())?.rows;
                    rows.truncate(64);
                    for row in &mut rows {
                        row.actions.truncate(16);
                    }
                    Ok(rows)
                });
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
    if result.is_err() {
        source.session = None;
        source.retry_after = Instant::now()
            + if current.load(Ordering::Relaxed) != generation { Duration::ZERO } else { Duration::from_secs(1) };
    }
    result
}

impl Source {
    fn connect(&mut self) -> Result<&mut Session, String> {
        if self.session.is_none() {
            if Instant::now() < self.retry_after {
                return Err("Plugin is restarting; try again shortly".into());
            }
            self.session = Some(Session::start(&self.config, self.stopping.clone(), &self.environment)?);
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

struct Session {
    child: Child,
    input: ChildStdin,
    output: ChildStdout,
    buffered: Vec<u8>,
    next_id: u64,
    stopping: Arc<AtomicBool>,
}

impl Session {
    fn start(config: &PluginConfig, stopping: Arc<AtomicBool>, environment: &Environment) -> Result<Self, String> {
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
            .stderr(Stdio::null())
            .process_group(0)
            .spawn()
            .map_err(|error| error.to_string())?;
        let input = child.stdin.take().ok_or("Missing plugin stdin")?;
        let output = child.stdout.take().ok_or("Missing plugin stdout")?;
        let mut session = Self { child, input, output, buffered: Vec::new(), next_id: 0, stopping };
        for descriptor in [session.input.as_raw_fd(), session.output.as_raw_fd()] {
            #[allow(unsafe_code, reason = "fcntl sets nonblocking mode on owned plugin pipes")]
            // SAFETY: these descriptors belong to live pipes owned by this session.
            let result = unsafe { libc::fcntl(descriptor, libc::F_SETFL, libc::O_NONBLOCK) };
            if result < 0 {
                return Err(std::io::Error::last_os_error().to_string());
            }
        }
        let initialized = session.request("initialize", json!({"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"tileception-launcher","version":env!("CARGO_PKG_VERSION")}}), None)?;
        if initialized["protocolVersion"] != "2025-06-18" || !initialized["capabilities"]["tools"].is_object() {
            return Err("Plugin must support MCP 2025-06-18 tools".into());
        }
        session.write(json!({"jsonrpc":"2.0","method":"notifications/initialized"}), Instant::now() + TIMEOUT, None)?;
        Ok(session)
    }

    fn tool(&mut self, name: &str, arguments: Value, cancel: Option<(&AtomicU64, u64)>) -> Result<Value, String> {
        let result = self.request("tools/call", json!({"name":name,"arguments":arguments}), cancel)?;
        if result["isError"] == true {
            return Err(format!("Plugin tool failed: {}", result["content"]));
        }
        if let Some(value) = result.get("structuredContent") {
            return Ok(value.clone());
        }
        let text = result["content"]
            .as_array()
            .and_then(|content| {
                content.iter().find_map(|item| (item["type"] == "text").then(|| item["text"].as_str()).flatten())
            })
            .ok_or("Plugin tool returned no structured content")?;
        serde_json::from_str(text).map_err(|error| error.to_string())
    }

    fn write(&mut self, value: Value, deadline: Instant, cancel: Option<(&AtomicU64, u64)>) -> Result<(), String> {
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
                    std::thread::sleep(Duration::from_millis(10))
                }
                Err(error) => return Err(error.to_string()),
            }
        }
        Ok(())
    }

    fn request(&mut self, method: &str, params: Value, cancel: Option<(&AtomicU64, u64)>) -> Result<Value, String> {
        self.next_id += 1;
        let request_id = self.next_id;
        let deadline = Instant::now() + TIMEOUT;
        self.write(json!({"jsonrpc":"2.0","id":request_id,"method":method,"params":params}), deadline, cancel)?;
        loop {
            if self.stopping.load(Ordering::Relaxed) {
                return Err("Plugin host is stopping".into());
            }
            if let Err(error) = check(deadline, cancel) {
                let _ = self.write(json!({"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":request_id,"reason":error}}), Instant::now() + Duration::from_millis(50), None);
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
                            json!({"jsonrpc":"2.0","id":message["id"],"error":{"code":-32601,"message":"Unsupported client method"}})
                        };
                        self.write(response, deadline, cancel)?;
                    }
                    continue;
                }
                if message["id"] != request_id {
                    continue;
                }
                if let Some(error) = message.get("error") {
                    return Err(format!("MCP error: {error}"));
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
                    std::thread::sleep(Duration::from_millis(10))
                }
                Err(error) => return Err(error.to_string()),
            }
        }
    }
}

fn check(deadline: Instant, cancel: Option<(&AtomicU64, u64)>) -> Result<(), String> {
    if cancel.is_some_and(|(current, generation)| current.load(Ordering::Relaxed) != generation) {
        return Err("Search cancelled".into());
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
