//! Plugins: executables speaking MCP over stdio, found through manifests and
//! `[plugins.<id>]`. They add launcher sources and cards, panel blocks, bar
//! chips and commands.
//!
//! The compositor owns one [`Host`]. Every plugin gets a worker thread that
//! owns its process and all of its I/O, so a slow plugin holds up only
//! itself; the compositor only sends requests and receives events. Each
//! place cancels only its own requests, and an idle worker keeps reading, so
//! a plugin can speak first (notifications) and a crash shows at once.

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
    thread::JoinHandle,
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use smithay::reexports::calloop::channel;
use zeroize::{Zeroize, Zeroizing};

use crate::Tileception;

pub type Environment = Vec<(OsString, Option<OsString>)>;

/// Where a plugin shows up. Each place cancels only its own requests, so
/// one place refreshing never cancels another's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Place {
    Launcher,
    /// The notification panel's blocks.
    Panel,
    /// `tileception msg plugin` and key bindings.
    Command,
}

impl Place {
    /// Whether a new request cancels the place's earlier ones. The panel's
    /// clicks and commands all go through; each gets its own number.
    fn cancels(self) -> bool {
        self == Place::Launcher
    }
}

/// The current request generation of every place; for places that do not
/// cancel, the last request's number.
#[derive(Default)]
struct Generations([AtomicU64; 3]);

impl Generations {
    fn get(&self, place: Place) -> &AtomicU64 {
        &self.0[place as usize]
    }
}

/// A manifest, or `[plugins.<id>]` overriding the manifest with that name.
/// Each place has a block with the user's options for it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PluginConfig {
    pub command: Option<String>,
    pub args: Option<Vec<String>>,
    pub enabled: Option<bool>,
    pub launcher: Option<LauncherOptions>,
    /// Chips in the bar: only with this block, unless it says
    /// `enabled = false`.
    pub bar: Option<BarOptions>,
    /// Items on the dashboard of a cleared desk: only with this block,
    /// unless it says `enabled = false`.
    pub dashboard: Option<DashboardOptions>,
    /// Launcher prefixes from before the `[launcher]` block.
    pub prefixes: Option<Vec<String>>,
}

impl PluginConfig {
    /// Whether the plugin's chips show in the bar.
    pub fn in_bar(&self) -> bool {
        self.bar.as_ref().is_some_and(|bar| bar.enabled != Some(false))
    }

    /// Whether the plugin draws dashboard surfaces, over a connection of
    /// its own.
    pub fn draws_surfaces(&self) -> bool {
        self.on_dashboard() && self.dashboard.as_ref().is_some_and(|dashboard| dashboard.surface == Some(true))
    }

    /// Whether the plugin's items show on the dashboard.
    pub fn on_dashboard(&self) -> bool {
        self.dashboard.as_ref().is_some_and(|dashboard| dashboard.enabled != Some(false))
    }

    /// Whether the plugin's cards show under the launcher.
    pub fn launcher_cards(&self) -> bool {
        self.launcher.as_ref().and_then(|launcher| launcher.cards) != Some(false)
    }
}

/// A plugin's `[launcher]` block.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LauncherOptions {
    pub prefixes: Option<Vec<String>>,
    /// `false` hides the plugin's cards under the launcher.
    pub cards: Option<bool>,
}

/// A plugin's `[bar]` block.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct BarOptions {
    pub enabled: Option<bool>,
}

/// A plugin's `[dashboard]` block.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DashboardOptions {
    pub enabled: Option<bool>,
    /// Started with `WAYLAND_SOCKET`, a connection of its own on which it
    /// may draw its `surface` items.
    pub surface: Option<bool>,
}

/// The config's `[launcher]` section, where plugins were configured before
/// `[plugins]`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LauncherConfig {
    pub plugins: BTreeMap<String, PluginConfig>,
}

/// The plugin config, with the old places folded into the new ones.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Settings {
    plugins: BTreeMap<String, PluginConfig>,
    warnings: Vec<String>,
    /// What the bundled plugins' manifests depend on.
    pub bundled: crate::bundled::Options,
}

impl Settings {
    pub fn new(plugins: &BTreeMap<String, PluginConfig>, legacy: &LauncherConfig) -> Self {
        let mut warnings = Vec::new();
        let mut merged = BTreeMap::new();
        for (plugin_id, config) in &legacy.plugins {
            warnings.push(format!("[launcher.plugins.{plugin_id}] is now [plugins.{plugin_id}]"));
            merged.insert(plugin_id.clone(), normalize(plugin_id, config.clone(), &mut warnings));
        }
        for (plugin_id, config) in plugins {
            let config = normalize(plugin_id, config.clone(), &mut warnings);
            overlay(merged.entry(plugin_id.clone()).or_default(), config);
        }
        Self { plugins: merged, warnings, bundled: crate::bundled::Options::default() }
    }
}

/// Moves top-level `prefixes` into the `[launcher]` block.
fn normalize(plugin_id: &str, mut config: PluginConfig, warnings: &mut Vec<String>) -> PluginConfig {
    if let Some(prefixes) = config.prefixes.take() {
        warnings.push(format!("Plugin {plugin_id}: prefixes now go in its [launcher] block"));
        config.launcher.get_or_insert_default().prefixes.get_or_insert(prefixes);
    }
    config
}

/// Lays the fields `overlay` sets over `base`.
fn overlay(base: &mut PluginConfig, overlay: PluginConfig) {
    if overlay.command.is_some() {
        base.command = overlay.command;
    }
    if overlay.args.is_some() {
        base.args = overlay.args;
    }
    if overlay.enabled.is_some() {
        base.enabled = overlay.enabled;
    }
    if let Some(launcher) = overlay.launcher {
        if let Some(prefixes) = launcher.prefixes {
            base.launcher.get_or_insert_default().prefixes = Some(prefixes);
        }
        if launcher.cards.is_some() {
            base.launcher.get_or_insert_default().cards = launcher.cards;
        }
    }
    if let Some(bar) = overlay.bar {
        let base = base.bar.get_or_insert_default();
        if bar.enabled.is_some() {
            base.enabled = bar.enabled;
        }
    }
    if let Some(dashboard) = overlay.dashboard {
        let base = base.dashboard.get_or_insert_default();
        if dashboard.enabled.is_some() {
            base.enabled = dashboard.enabled;
        }
        if dashboard.surface.is_some() {
            base.surface = dashboard.surface;
        }
    }
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
    /// What Tab puts in the launcher's field.
    #[serde(default)]
    pub completion: Option<String>,
    /// What Ctrl+Enter does instead of the first action.
    #[serde(default)]
    pub alternate: Option<Box<Action>>,
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
        /// A password or code: offered with `x-kde-passwordManagerHint`,
        /// kept out of clipboard persistence and handoff.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        sensitive: bool,
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
    /// Opens the user's terminal in a folder.
    Terminal {
        directory: String,
    },
    /// Opens a page of Tileception's Settings by its title, such as
    /// "Smart home".
    Settings {
        page: String,
    },
    /// Asks for input or waits in the launcher, in steps that call the
    /// plugin's tools. Launcher only.
    Prompt {
        prompt: Box<Prompt>,
    },
}

impl ActionKind {
    /// What a prompt's tool may ask the launcher to do afterwards: what a
    /// row may do, except calling tools or prompting again.
    pub fn after_prompt(&self) -> bool {
        !matches!(self, ActionKind::Tool { .. } | ActionKind::Prompt { .. })
    }
}

fn empty_object() -> Value {
    json!({})
}

/// Steps and fields a prompt may have at most.
pub const PROMPT_STEPS: usize = 8;
/// Few enough fields that a prompt with its text and an error fits on a
/// 768 pixel high screen.
pub const PROMPT_FIELDS: usize = 4;

/// A small dialog in the launcher for a plugin, such as unlocking a vault
/// with a passphrase or a security key. It opens on its first step.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "PromptShape")]
pub struct Prompt {
    pub title: String,
    #[serde(default)]
    pub detail: String,
    #[serde(default)]
    pub icon: Option<String>,
    pub steps: Vec<Step>,
}

/// A prompt as sent, before it is checked.
#[derive(Deserialize)]
struct PromptShape {
    title: String,
    #[serde(default)]
    detail: String,
    #[serde(default)]
    icon: Option<String>,
    steps: Vec<Step>,
}

impl TryFrom<PromptShape> for Prompt {
    type Error = String;

    fn try_from(shape: PromptShape) -> Result<Self, String> {
        let PromptShape { title, detail, icon, steps } = shape;
        if steps.is_empty() || steps.len() > PROMPT_STEPS {
            return Err(format!("a prompt needs 1 to {PROMPT_STEPS} steps"));
        }
        let mut ids = std::collections::BTreeSet::new();
        for step in &steps {
            if step.step_id.is_empty() || !ids.insert(step.step_id.as_str()) {
                return Err("prompt step ids must be nonempty and unique".into());
            }
            if step.tool.is_empty() || !step.arguments.is_object() {
                return Err(format!("prompt step {}: needs a tool and object arguments", step.step_id));
            }
            if let StepKind::Fields { fields, .. } = &step.kind {
                let mut names = std::collections::BTreeSet::new();
                if fields.is_empty() || fields.len() > PROMPT_FIELDS {
                    return Err(format!("prompt step {}: needs 1 to {PROMPT_FIELDS} fields", step.step_id));
                }
                if fields.iter().any(|field| field.name.is_empty() || !names.insert(field.name.as_str())) {
                    return Err(format!("prompt step {}: field names must be nonempty and unique", step.step_id));
                }
            }
        }
        for step in &steps {
            if let Some(alternate) = &step.alternate
                && !ids.contains(alternate.step.as_str())
            {
                return Err(format!("prompt step {}: no step {:?} to switch to", step.step_id, alternate.step));
            }
        }
        Ok(Prompt { title, detail, icon, steps })
    }
}

impl Prompt {
    pub fn step_index(&self, step_id: &str) -> Option<usize> {
        self.steps.iter().position(|step| step.step_id == step_id)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Step {
    #[serde(rename = "id")]
    pub step_id: String,
    /// Shown above the step's fields, or while waiting.
    #[serde(default)]
    pub text: String,
    /// A second way through, such as a passphrase instead of a key.
    #[serde(default)]
    pub alternate: Option<Alternate>,
    pub tool: String,
    #[serde(default = "empty_object")]
    pub arguments: Value,
    /// Seconds the tool may take.
    #[serde(default)]
    pub timeout: Option<f64>,
    #[serde(flatten)]
    pub kind: StepKind,
}

impl Step {
    /// How long the step's tool may take: its `timeout`, from 3 to 60
    /// seconds; else 30 for a wait and the usual 3 for fields.
    pub fn timeout(&self) -> Duration {
        let default = match self.kind {
            StepKind::Wait => Duration::from_secs(30),
            StepKind::Fields { .. } => TIMEOUT,
        };
        let seconds = self.timeout.unwrap_or(default.as_secs_f64());
        Duration::from_secs_f64(seconds.clamp(TIMEOUT.as_secs_f64(), 60.0))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum StepKind {
    /// Calls its tool on entering and waits for the answer, such as a
    /// security key being touched.
    Wait,
    /// Fields to fill in; submitting calls the tool with their values.
    Fields {
        fields: Vec<PromptField>,
        /// The submit button's label.
        #[serde(default)]
        submit: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PromptField {
    pub name: String,
    #[serde(default)]
    pub label: String,
    /// Masked, wiped from memory once sent, and never logged.
    #[serde(default)]
    pub secret: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Alternate {
    pub title: String,
    pub step: String,
}

/// A tool call's arguments. Secret ones, such as a typed passphrase, are
/// wiped from memory when dropped, however the call ends.
pub struct Arguments {
    value: Value,
    secret: bool,
}

impl Arguments {
    pub fn secret(value: Value) -> Self {
        Self { value, secret: true }
    }

    pub fn value_mut(&mut self) -> &mut Value {
        &mut self.value
    }
}

impl From<Value> for Arguments {
    fn from(value: Value) -> Self {
        Self { value, secret: false }
    }
}

impl Drop for Arguments {
    fn drop(&mut self) {
        if self.secret {
            wipe(&mut self.value);
        }
    }
}

/// Overwrites every string in a JSON value.
fn wipe(value: &mut Value) {
    match value {
        Value::String(text) => text.zeroize(),
        Value::Array(items) => items.iter_mut().for_each(wipe),
        Value::Object(map) => map.values_mut().for_each(wipe),
        _ => {}
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct Info {
    pub name: String,
    pub description: String,
    #[serde(default)]
    pub icon: Option<String>,
    pub prefixes: Vec<String>,
}

/// What a plugin offers, from `tools/list` and `resources/list`.
#[derive(Debug, Clone, Default)]
pub struct Described {
    /// Its launcher source, when it has `launcher_info`.
    pub launcher: Option<Info>,
    /// The `tileception://` resources it offers, subscribed to.
    pub resources: Vec<String>,
}

/// The resource holding a plugin's panel block.
pub const PANEL: &str = "tileception://panel";
/// The resource holding a plugin's bar chips.
pub const BAR: &str = "tileception://bar";
/// The resource holding a plugin's cards under the launcher.
pub const LAUNCHER: &str = "tileception://launcher";
/// The resource holding a plugin's items on the dashboard.
pub const DASHBOARD: &str = "tileception://dashboard";

/// What [`Host::handle`] changed.
#[derive(Debug, Default)]
pub struct Changed {
    pub routes: bool,
    pub panels: bool,
    pub bar: bool,
    /// The cards under the launcher.
    pub cards: bool,
    /// The dashboard's items.
    pub dashboard: bool,
    /// Whose dashboard items were read again; all when unset.
    pub dashboard_plugin: Option<String>,
    /// Other resources read anew, as (plugin, uri).
    pub resources: Vec<(String, String)>,
}

#[derive(Debug, Clone)]
pub struct Route {
    pub plugin_id: String,
    pub info: Info,
    pub prefixes: Vec<String>,
}

pub enum Event {
    /// The manifests and config of one configuration.
    Discovered {
        epoch: u64,
        plugins: Vec<(String, PluginConfig)>,
        diagnostics: Vec<String>,
    },
    /// A plugin started and described itself, or failed to.
    Info {
        epoch: u64,
        plugin: String,
        result: Result<Described, String>,
    },
    /// A subscribed resource was read, after starting or a change.
    Resource {
        epoch: u64,
        plugin: String,
        uri: String,
        result: Result<Value, String>,
    },
    /// A plugin that draws dashboard surfaces started: Tileception's end
    /// of its Wayland connection.
    DashboardClient {
        plugin: String,
        stream: std::os::unix::net::UnixStream,
    },
    /// A message a plugin sent on its own.
    Notification {
        plugin: String,
        method: String,
        params: Value,
    },
    Rows {
        generation: u64,
        plugin: String,
        result: Result<Vec<Row>, String>,
    },
    /// Additional rows for an already answered search, appended without moving selection.
    MoreRows {
        generation: u64,
        plugin: String,
        rows: Vec<Row>,
    },
    Action {
        place: Place,
        generation: u64,
        result: Result<Value, String>,
    },
}

enum Request {
    Search { generation: u64, prefix: String, query: String },
    Tool { place: Place, generation: u64, name: String, arguments: Arguments, terms: Terms },
}

/// How a request is made: its deadline, and whether a cancelled one is let
/// go rather than awaited until its deadline.
#[derive(Debug, Clone, Copy)]
struct Terms {
    timeout: Duration,
    /// For the launcher's prompts: a plugin told to stop waiting for a
    /// security key need not answer, and is not ended for that.
    forget: bool,
    /// The message holds a secret: serialized without spare copies and wiped.
    secret: bool,
}

impl Default for Terms {
    fn default() -> Self {
        Self { timeout: TIMEOUT, forget: false, secret: false }
    }
}

/// The compositor's end of one plugin's worker thread.
struct Worker {
    requests: mpsc::Sender<Request>,
    /// Rung after every request and cancellation; the worker waits on it.
    bell: UnixDatagram,
    stopping: Arc<AtomicBool>,
    thread: JoinHandle<()>,
}

impl Worker {
    fn spawn(
        name: String,
        config: PluginConfig,
        epoch: u64,
        events: channel::Sender<Event>,
        generations: Arc<Generations>,
        environment: Arc<RwLock<Environment>>,
    ) -> Result<Self, String> {
        let (bell, ear) = UnixDatagram::pair().map_err(|error| error.to_string())?;
        bell.set_nonblocking(true).map_err(|error| error.to_string())?;
        ear.set_nonblocking(true).map_err(|error| error.to_string())?;
        let (requests, receiver) = mpsc::channel();
        let stopping = Arc::new(AtomicBool::new(false));
        let source = Source {
            name: name.clone(),
            config,
            session: None,
            retry_after: Instant::now(),
            stopping: stopping.clone(),
            environment,
            wake: Arc::new(ear),
            resources: Vec::new(),
            dirty: std::collections::BTreeSet::new(),
            events: Some(events.clone()),
        };
        let thread = std::thread::Builder::new()
            .name(format!("plugin {name}"))
            .spawn(move || work(source, epoch, &receiver, &events, &generations))
            .map_err(|error| error.to_string())?;
        Ok(Self { requests, bell, stopping, thread })
    }

    fn send(&self, request: Request) {
        let _ = self.requests.send(request);
        self.ring();
    }

    fn ring(&self) {
        let _ = self.bell.send(&[1]);
    }

    /// Asks the worker to end, which also ends its plugin's processes.
    fn stop(self) -> JoinHandle<()> {
        self.stopping.store(true, Ordering::Relaxed);
        self.ring();
        self.thread
    }
}

/// All plugins: what they are, their workers, and the launcher's routes.
pub struct Host {
    settings: Settings,
    events: channel::Sender<Event>,
    environment: Arc<RwLock<Environment>>,
    generations: Arc<Generations>,
    /// Counts configurations; events from older ones are ignored.
    epoch: u64,
    /// Enabled plugins, highest priority first.
    order: Vec<(String, PluginConfig)>,
    /// The order the user chose for the panel and the bar, by plugin.
    arrangement: Vec<String>,
    infos: BTreeMap<String, Result<Described, String>>,
    /// Diagnostics from reading manifests and config.
    found: Vec<String>,
    workers: BTreeMap<String, Worker>,
    /// Workers of earlier configurations, still ending.
    retired: Vec<JoinHandle<()>>,
    pub routes: Vec<Route>,
    pub diagnostics: Vec<String>,
    /// Panel blocks by plugin.
    panels: BTreeMap<String, crate::panel::block::Block>,
    /// Bar chips by plugin.
    bars: BTreeMap<String, crate::panel::block::Bar>,
    /// Cards under the launcher by plugin.
    cards: BTreeMap<String, Vec<crate::panel::block::Card>>,
    /// Dashboard items by plugin.
    dashboards: BTreeMap<String, Vec<crate::dashboard::Item>>,
    /// Other resources by (plugin, uri), as last read.
    resources: BTreeMap<(String, String), Value>,
}

impl Host {
    pub fn start(settings: Settings, events: channel::Sender<Event>, environment: Environment) -> Self {
        let mut host = Self {
            settings,
            events,
            environment: Arc::new(RwLock::new(environment)),
            generations: Arc::default(),
            epoch: 0,
            order: Vec::new(),
            arrangement: crate::panel::order::load(),
            infos: BTreeMap::new(),
            found: Vec::new(),
            workers: BTreeMap::new(),
            retired: Vec::new(),
            routes: Vec::new(),
            diagnostics: Vec::new(),
            panels: BTreeMap::new(),
            bars: BTreeMap::new(),
            cards: BTreeMap::new(),
            dashboards: BTreeMap::new(),
            resources: BTreeMap::new(),
        };
        host.reload();
        host
    }

    pub fn configure(&mut self, settings: Settings) {
        if self.settings != settings {
            self.settings = settings;
            self.reload();
        }
    }

    /// Gives plugins Xwayland's display once it is ready. Plugins already
    /// running started without it, so they start again.
    pub fn set_display(&mut self, display: u32) {
        {
            let mut environment = self.environment.write().expect("plugin environment lock");
            environment.retain(|(key, _)| key != "DISPLAY");
            environment.push(("DISPLAY".into(), Some(format!(":{display}").into())));
        }
        self.reload();
    }

    /// Ends every worker and reads the manifests again, off this thread.
    fn reload(&mut self) {
        self.cancel(Place::Launcher);
        self.epoch += 1;
        for (_, worker) in std::mem::take(&mut self.workers) {
            self.retired.push(worker.stop());
        }
        self.retired.retain(|thread| !thread.is_finished());
        self.order.clear();
        self.infos.clear();
        self.found.clear();
        self.routes.clear();
        self.diagnostics.clear();
        self.panels.clear();
        self.bars.clear();
        self.cards.clear();
        self.dashboards.clear();
        self.resources.clear();
        let epoch = self.epoch;
        let settings = self.settings.clone();
        let events = self.events.clone();
        std::thread::spawn(move || {
            let (plugins, diagnostics) = discover(settings);
            let _ = events.send(Event::Discovered { epoch, plugins, diagnostics });
        });
    }

    /// Takes in what discovery and the workers report.
    pub fn handle(&mut self, event: Event) -> Changed {
        let reloaded = matches!(&event, Event::Discovered { epoch, .. } if *epoch == self.epoch);
        let routes = self.handle_inner(event);
        Changed {
            routes,
            panels: reloaded,
            bar: reloaded,
            cards: reloaded,
            dashboard: reloaded,
            dashboard_plugin: None,
            resources: Vec::new(),
        }
    }

    /// A resource was read: panels are kept parsed, others as they are.
    pub fn handle_resource(
        &mut self,
        epoch: u64,
        plugin: String,
        uri: String,
        result: Result<Value, String>,
    ) -> Changed {
        let mut changed = Changed::default();
        if epoch != self.epoch || !self.workers.contains_key(&plugin) {
            return changed;
        }
        let value = match result {
            Ok(value) => value,
            Err(error) => {
                tracing::warn!("Plugin {plugin}: could not read {uri}: {error}");
                return changed;
            }
        };
        if uri == PANEL {
            match serde_json::from_value::<crate::panel::block::Block>(value) {
                Ok(block) => {
                    self.panels.insert(plugin, block);
                }
                Err(error) => {
                    tracing::warn!("Plugin {plugin}: invalid panel block: {error}");
                    self.panels.remove(&plugin);
                }
            }
            changed.panels = true;
        } else if uri == BAR {
            match serde_json::from_value::<crate::panel::block::Bar>(value) {
                Ok(bar) => {
                    self.bars.insert(plugin, bar);
                }
                Err(error) => {
                    tracing::warn!("Plugin {plugin}: invalid bar chips: {error}");
                    self.bars.remove(&plugin);
                }
            }
            changed.bar = true;
        } else if uri == LAUNCHER {
            match serde_json::from_value::<crate::panel::block::Launcher>(value) {
                Ok(mut launcher) => {
                    launcher.cards.truncate(crate::panel::block::LAUNCHER_CARDS);
                    self.cards.insert(plugin, launcher.cards);
                }
                Err(error) => {
                    tracing::warn!("Plugin {plugin}: invalid launcher cards: {error}");
                    self.cards.remove(&plugin);
                }
            }
            changed.cards = true;
        } else if uri == DASHBOARD {
            match serde_json::from_value::<crate::dashboard::Items>(value) {
                Ok(mut items) => {
                    items.items.truncate(crate::dashboard::PLUGIN_ITEMS);
                    items.seal();
                    self.dashboards.insert(plugin.clone(), items.items);
                }
                Err(error) => {
                    tracing::warn!("Plugin {plugin}: invalid dashboard items: {error}");
                    self.dashboards.remove(&plugin);
                }
            }
            changed.dashboard = true;
            changed.dashboard_plugin = Some(plugin);
        } else {
            self.resources.insert((plugin.clone(), uri.clone()), value);
            changed.resources.push((plugin, uri));
        }
        changed
    }

    /// Enabled plugins in the user's order, else highest priority first.
    fn arranged(&self) -> Vec<&(String, PluginConfig)> {
        let priority: Vec<&str> = self.order.iter().map(|(plugin_id, _)| plugin_id.as_str()).collect();
        crate::panel::order::arrange(&priority, &self.arrangement)
            .into_iter()
            .filter_map(|plugin_id| self.order.iter().find(|(candidate, _)| candidate == plugin_id))
            .collect()
    }

    /// The panel blocks, in the user's order.
    pub fn panels(&self) -> Vec<(&str, &crate::panel::block::Block)> {
        self.arranged()
            .into_iter()
            .filter_map(|(plugin_id, _)| Some((plugin_id.as_str(), self.panels.get(plugin_id)?)))
            .collect()
    }

    /// The bar chips of plugins with a `[bar]` block, in the user's order.
    pub fn bar_chips(&self) -> Vec<(&str, &crate::panel::block::BarChip)> {
        self.arranged()
            .into_iter()
            .filter(|(_, config)| config.in_bar())
            .filter_map(|(plugin_id, _)| Some((plugin_id.as_str(), self.bars.get(plugin_id)?)))
            .flat_map(|(plugin_id, bar)| bar.chips.iter().map(move |chip| (plugin_id, chip)))
            .collect()
    }

    /// The cards under the launcher, in the user's order, except of
    /// plugins whose `[launcher]` block says `cards = false`.
    pub fn launcher_cards(&self) -> Vec<(&str, &crate::panel::block::Card)> {
        self.arranged()
            .into_iter()
            .filter(|(_, config)| config.launcher_cards())
            .filter_map(|(plugin_id, _)| Some((plugin_id.as_str(), self.cards.get(plugin_id)?)))
            .flat_map(|(plugin_id, cards)| cards.iter().map(move |card| (plugin_id, card)))
            .collect()
    }

    /// The dashboard items of plugins with a `[dashboard]` block, in the
    /// user's order.
    pub fn dashboard_items(&self) -> Vec<(&str, &crate::dashboard::Item)> {
        self.arranged()
            .into_iter()
            .filter(|(_, config)| config.on_dashboard())
            .filter_map(|(plugin_id, _)| Some((plugin_id.as_str(), self.dashboards.get(plugin_id)?)))
            .flat_map(|(plugin_id, items)| items.iter().map(move |item| (plugin_id, item)))
            .collect()
    }

    /// Puts the plugins with blocks in this order, keeping the others'
    /// places, and saves it.
    pub fn arrange_panels(&mut self, shown: &[&str]) {
        let priority: Vec<&str> = self.order.iter().map(|(plugin_id, _)| plugin_id.as_str()).collect();
        self.arrangement = crate::panel::order::rearrange(&priority, &self.arrangement, shown);
        crate::panel::order::save(&self.arrangement);
    }

    /// A resource as last read.
    pub fn resource(&self, plugin: &str, uri: &str) -> Option<&Value> {
        self.resources.get(&(plugin.to_string(), uri.to_string()))
    }

    /// Whether a plugin is running.
    pub fn running(&self, plugin: &str) -> bool {
        self.workers.contains_key(plugin)
    }

    fn handle_inner(&mut self, event: Event) -> bool {
        match event {
            Event::Discovered { epoch, plugins, diagnostics } if epoch == self.epoch => {
                self.found = diagnostics;
                for (plugin_id, config) in plugins {
                    if config.enabled == Some(false) {
                        continue;
                    }
                    match Worker::spawn(
                        plugin_id.clone(),
                        config.clone(),
                        epoch,
                        self.events.clone(),
                        self.generations.clone(),
                        self.environment.clone(),
                    ) {
                        Ok(worker) => {
                            self.workers.insert(plugin_id.clone(), worker);
                        }
                        Err(error) => {
                            self.infos.insert(plugin_id.clone(), Err(error));
                        }
                    }
                    self.order.push((plugin_id, config));
                }
                self.route();
                true
            }
            Event::Info { epoch, plugin, result } if epoch == self.epoch => {
                self.infos.insert(plugin, result);
                self.route();
                true
            }
            Event::Notification { plugin, method, params } => {
                tracing::debug!("Plugin {plugin} sent {method}: {params}");
                false
            }
            _ => false,
        }
    }

    /// Works out the launcher's routes from the plugins described so far,
    /// in priority order, so the outcome does not depend on who answered
    /// first. Once all have, plugins with neither a route nor resources stop.
    fn route(&mut self) {
        let mut diagnostics = self.found.clone();
        let mut routes = Vec::new();
        let mut claimed = BTreeMap::<String, String>::new();
        for (plugin_id, config) in &self.order {
            let info = match self.infos.get(plugin_id) {
                None => continue,
                Some(Err(error)) => {
                    diagnostics.push(format!("Plugin {plugin_id}: {error}"));
                    continue;
                }
                Some(Ok(Described { launcher: Some(info), .. })) => info,
                Some(Ok(Described { launcher: None, resources })) => {
                    if resources.is_empty() {
                        diagnostics.push(format!("Plugin {plugin_id}: offers nothing Tileception uses"));
                    }
                    continue;
                }
            };
            let mut prefixes = config
                .launcher
                .as_ref()
                .and_then(|launcher| launcher.prefixes.clone())
                .unwrap_or_else(|| info.prefixes.clone());
            prefixes.retain(|prefix| {
                if prefix.is_empty() || prefix == "plugins" || prefix.chars().any(char::is_whitespace) {
                    diagnostics.push(format!("Plugin {plugin_id}: prefix must be a nonempty word other than plugins"));
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
                routes.push(Route { plugin_id: plugin_id.clone(), info: info.clone(), prefixes });
            }
        }
        if self.order.iter().all(|(plugin_id, _)| self.infos.contains_key(plugin_id)) {
            for diagnostic in &diagnostics {
                tracing::warn!("{diagnostic}");
            }
            let unused: Vec<_> = self
                .workers
                .keys()
                .filter(|plugin_id| !routes.iter().any(|route: &Route| &route.plugin_id == *plugin_id))
                .filter(|plugin_id| {
                    !self
                        .infos
                        .get(*plugin_id)
                        .is_some_and(|info| info.as_ref().is_ok_and(|info| !info.resources.is_empty()))
                })
                .cloned()
                .collect();
            for plugin_id in unused {
                if let Some(worker) = self.workers.remove(&plugin_id) {
                    self.retired.push(worker.stop());
                }
            }
        }
        self.routes = routes;
        self.diagnostics = diagnostics;
    }

    /// Cancels the place's requests and returns its new generation.
    pub fn cancel(&self, place: Place) -> u64 {
        let generation = self.generations.get(place).fetch_add(1, Ordering::Relaxed) + 1;
        for worker in self.workers.values() {
            worker.ring();
        }
        generation
    }

    /// The route for what is typed: a prefix and a space, so typing the
    /// start of an app's name never switches to a plugin.
    pub fn matches(&self, text: &str) -> Option<(&Route, &str)> {
        route_for(&self.routes, text)
    }

    pub fn search(&self, text: &str) -> Option<(u64, String)> {
        let generation = self.cancel(Place::Launcher);
        let (route, prefix) = self.matches(text)?;
        let worker = self.workers.get(&route.plugin_id)?;
        worker.send(Request::Search { generation, prefix: prefix.into(), query: text[prefix.len()..].trim().into() });
        Some((generation, route.plugin_id.clone()))
    }

    /// Calls a plugin's tool from a place; the result comes back as
    /// [`Event::Action`] with the returned number.
    pub fn tool(&self, place: Place, plugin: String, name: String, arguments: Value) -> u64 {
        self.send_tool(place, plugin, name, arguments.into(), Terms::default())
    }

    /// Calls a tool for a launcher prompt: within `timeout`, and when
    /// cancelled the plugin is told so but not waited for.
    pub fn prompt_tool(&self, plugin: String, name: String, arguments: Arguments, timeout: Duration) -> u64 {
        let terms = Terms { timeout, forget: true, secret: false };
        self.send_tool(Place::Launcher, plugin, name, arguments, terms)
    }

    fn send_tool(&self, place: Place, plugin: String, name: String, arguments: Arguments, terms: Terms) -> u64 {
        let generation = if place.cancels() {
            self.cancel(place)
        } else {
            self.generations.get(place).fetch_add(1, Ordering::Relaxed) + 1
        };
        if let Some(worker) = self.workers.get(&plugin) {
            worker.send(Request::Tool { place, generation, name, arguments, terms });
        } else {
            let _ = self.events.send(Event::Action { place, generation, result: Err("Plugin is unavailable".into()) });
        }
        generation
    }
}

/// See [`Host::matches`].
fn route_for<'a>(routes: &'a [Route], text: &str) -> Option<(&'a Route, &'a str)> {
    routes.iter().find_map(|route| {
        route.prefixes.iter().find_map(|prefix| {
            let rest = text.strip_prefix(prefix.as_str())?;
            rest.starts_with(char::is_whitespace).then_some((route, prefix.as_str()))
        })
    })
}

impl Drop for Host {
    fn drop(&mut self) {
        for (_, worker) in std::mem::take(&mut self.workers) {
            self.retired.push(worker.stop());
        }
        for thread in self.retired.drain(..) {
            let _ = thread.join();
        }
    }
}

impl Tileception {
    pub fn init_plugins(&mut self) {
        let (sender, receiver) = channel::channel();
        let environment = self
            .command("", &[])
            .get_envs()
            .map(|(key, value)| (key.to_os_string(), value.map(std::ffi::OsStr::to_os_string)))
            .collect();
        self.plugins = Some(Host::start(self.plugin_settings(), sender, environment));
        self.loop_handle
            .insert_source(receiver, |event, _, state| {
                if let channel::Event::Msg(event) = event {
                    state.plugin_event(event);
                }
            })
            .expect("plugin event source");
    }

    pub fn plugin_settings(&self) -> Settings {
        let mut settings = Settings::new(&self.config.file.plugins, &self.config.file.launcher);
        let home = &self.config.file.home;
        settings.bundled = crate::bundled::Options {
            home: home.enabled.then(|| home.devices.clone()),
            bluetooth_no_reconnect: !self.config.file.bluetooth.reconnect,
            timers_unsaved: self.nested || self.headless.is_some(),
        };
        settings
    }

    /// Picks up a changed config.
    pub fn apply_plugins(&mut self) {
        let settings = self.plugin_settings();
        if let Some(plugins) = &mut self.plugins {
            plugins.configure(settings);
        }
    }

    /// Explicitly rescans manifests even when the config has not changed.
    pub(crate) fn reload_plugins(&mut self) -> Result<(), String> {
        let plugins = self.plugins.as_mut().ok_or("Plugin host is not running")?;
        plugins.reload();
        Ok(())
    }

    pub fn stop_plugins(&mut self) {
        self.plugins = None;
    }

    /// Xwayland is ready: plugins get its display from now on.
    pub(crate) fn plugins_display_ready(&mut self, display: u32) {
        if let Some(plugins) = &mut self.plugins {
            plugins.set_display(display);
        }
    }

    fn plugin_event(&mut self, event: Event) {
        match event {
            Event::Rows { .. } | Event::MoreRows { .. } | Event::Action { place: Place::Launcher, .. } => {
                self.launcher_plugin_event(event)
            }
            Event::Notification { plugin, method, params } if method == "notifications/tileception/launcher_append" => {
                if let Some(generation) = params["generation"].as_u64()
                    && let Ok(rows) = parse_rows(params)
                {
                    self.launcher_plugin_event(Event::MoreRows { generation, plugin, rows });
                }
            }
            Event::Action { place: Place::Panel, result, .. } => {
                if let Err(error) = result {
                    tracing::warn!("Panel plugin action failed: {error}");
                }
            }
            Event::Action { place: Place::Command, generation, result } => {
                self.plugin_command_reply(generation, result)
            }
            Event::DashboardClient { plugin, stream } => self.dashboard_client(plugin, stream),
            Event::Resource { epoch, plugin, uri, result } => {
                let Some(plugins) = &mut self.plugins else { return };
                let changed = plugins.handle_resource(epoch, plugin, uri, result);
                self.plugins_changed(changed);
            }
            event => {
                let Some(plugins) = &mut self.plugins else { return };
                let changed = plugins.handle(event);
                self.plugins_changed(changed);
            }
        }
    }

    /// `tileception msg plugin <id> <tool> [arguments]`: calls the tool; the
    /// command is answered when the plugin is.
    pub(crate) fn plugin_command(&mut self, plugin: &str, tool: &str, arguments: Value) -> Result<String, String> {
        let plugins = self.plugins.as_ref().ok_or("plugins are not running")?;
        if !plugins.running(plugin) {
            return Err(format!("plugin {plugin} is not running; type plugins in the launcher to see why"));
        }
        let request = plugins.tool(Place::Command, plugin.into(), tool.into(), arguments);
        self.defer_reply(request);
        Ok(String::new())
    }

    /// A tool's answer for `tileception msg`: its `text`, else its JSON.
    fn plugin_command_reply(&mut self, request: u64, result: Result<Value, String>) {
        let reply = result.map(|value| match value["text"].as_str() {
            Some(text) if text.is_empty() || text.ends_with('\n') => text.to_string(),
            Some(text) => format!("{text}\n"),
            None => format!("{value:#}\n"),
        });
        self.reply_later(request, reply);
    }

    fn plugins_changed(&mut self, changed: Changed) {
        if changed.routes {
            self.launcher_routes_changed();
        }
        if changed.panels {
            self.panel_changed();
        }
        if changed.dashboard {
            self.dashboard_changed(changed.dashboard_plugin.as_deref());
        }
        if changed.bar || (changed.cards && self.launcher.open) {
            self.schedule_render();
        }
        if !changed.resources.is_empty() && self.settings.open {
            self.schedule_render();
        }
    }
}

/// Owns one plugin's process, on its worker thread.
struct Source {
    name: String,
    config: PluginConfig,
    session: Option<Session>,
    retry_after: Instant,
    stopping: Arc<AtomicBool>,
    environment: Arc<RwLock<Environment>>,
    wake: Arc<UnixDatagram>,
    /// The resources subscribed to; a plugin with any is restarted at once
    /// when it crashes, and subscribed again.
    resources: Vec<String>,
    /// Resources the plugin said changed, not yet read again.
    dirty: std::collections::BTreeSet<String>,
    /// Where a dashboard connection goes as soon as the plugin starts.
    events: Option<channel::Sender<Event>>,
}

/// A worker: describe the plugin, then serve requests, read changed
/// resources and listen while idle, until stopped.
fn work(
    mut source: Source,
    epoch: u64,
    requests: &mpsc::Receiver<Request>,
    events: &channel::Sender<Event>,
    generations: &Generations,
) {
    let described = source.describe();
    let failed = described.is_err();
    source.forward(events);
    let result = described.map_err(|error| error.to_string());
    if events.send(Event::Info { epoch, plugin: source.name.clone(), result }).is_err() || failed {
        return;
    }
    source.read_all(epoch, events);
    loop {
        if source.stopping.load(Ordering::Relaxed) {
            return;
        }
        match requests.try_recv() {
            Ok(request) => {
                let event = source.serve(request, generations);
                if let Some(event) = event
                    && events.send(event).is_err()
                {
                    return;
                }
                source.forward(events);
            }
            Err(mpsc::TryRecvError::Disconnected) => return,
            Err(mpsc::TryRecvError::Empty) if !source.dirty.is_empty() && source.session.is_some() => {
                source.read_dirty(epoch, events);
            }
            Err(mpsc::TryRecvError::Empty) if source.session.is_none() && !source.resources.is_empty() => {
                source.restart(epoch, events);
            }
            Err(mpsc::TryRecvError::Empty) => {
                source.idle();
                source.forward(events);
            }
        }
    }
}

impl Source {
    fn connect(&mut self) -> Result<&mut Session, PluginError> {
        if self.session.is_none() {
            if Instant::now() < self.retry_after {
                return Err("Plugin is restarting; try again shortly".into());
            }
            let environment = self.environment.read().expect("plugin environment lock").clone();
            self.session = Some(Session::start(
                &self.name,
                &self.config,
                self.stopping.clone(),
                &environment,
                self.wake.clone(),
                self.events.as_ref(),
            )?);
        }
        self.session.as_mut().ok_or_else(|| "Plugin failed to start".into())
    }

    /// Starts the plugin and asks what it offers: its launcher source and
    /// its `tileception://` resources, which it is subscribed to.
    fn describe(&mut self) -> Result<Described, PluginError> {
        let session = self.connect()?;
        let tools = session.request("tools/list", json!({}), None)?;
        let has_launcher =
            tools["tools"].as_array().is_some_and(|tools| tools.iter().any(|tool| tool["name"] == "launcher_info"));
        let launcher = if has_launcher {
            let value = session.tool("launcher_info", json!({}).into(), None, Terms::default())?;
            Some(serde_json::from_value::<Info>(value).map_err(|error| PluginError::Tool(error.to_string()))?)
        } else {
            None
        };
        let resources = session.subscribe_all()?;
        self.resources = resources.clone();
        Ok(Described { launcher, resources })
    }

    /// Reads every subscribed resource.
    fn read_all(&mut self, epoch: u64, events: &channel::Sender<Event>) {
        self.dirty = self.resources.iter().cloned().collect();
        self.read_dirty(epoch, events);
    }

    /// Reads the resources that changed, once each however often they did.
    fn read_dirty(&mut self, epoch: u64, events: &channel::Sender<Event>) {
        while let Some(uri) = self.dirty.pop_first() {
            let Some(session) = &mut self.session else { return };
            let result = session.read(&uri);
            if matches!(result, Err(PluginError::Transport(_))) {
                self.lost();
            }
            self.forward(events);
            let result = result.map_err(|error| error.to_string());
            let _ = events.send(Event::Resource { epoch, plugin: self.name.clone(), uri, result });
        }
    }

    /// A plugin with resources crashed: starts it again after the cooldown,
    /// subscribes again and reads everything.
    fn restart(&mut self, epoch: u64, events: &channel::Sender<Event>) {
        let wait = self.retry_after.saturating_duration_since(Instant::now());
        if !wait.is_zero() {
            wait_for_bell_until(&self.wake, Some(wait));
            return;
        }
        let subscribed = self.connect().and_then(Session::subscribe_all);
        match subscribed {
            Ok(resources) => {
                tracing::info!("Plugin {}: restarted", self.name);
                self.resources = resources;
                self.read_all(epoch, events);
            }
            Err(error) => {
                tracing::warn!("Plugin {}: could not restart: {error}", self.name);
                self.lost();
            }
        }
    }

    /// Answers one request, unless its place has moved on.
    fn serve(&mut self, request: Request, generations: &Generations) -> Option<Event> {
        match request {
            Request::Search { generation, prefix, query } => {
                let current = generations.get(Place::Launcher);
                if current.load(Ordering::Relaxed) != generation {
                    return None;
                }
                let result = self
                    .invoke(
                        "launcher_search",
                        json!({"query":query,"prefix":prefix,"_meta":{"tileception/generation":generation}}),
                        current,
                        generation,
                    )
                    .and_then(parse_rows);
                (current.load(Ordering::Relaxed) == generation).then(|| Event::Rows {
                    generation,
                    plugin: self.name.clone(),
                    result,
                })
            }
            Request::Tool { place, generation, name, arguments, terms } if !place.cancels() => {
                let result = self.call(&name, arguments, None, terms);
                Some(Event::Action { place, generation, result })
            }
            Request::Tool { place, generation, name, arguments, terms } => {
                let current = generations.get(place);
                if current.load(Ordering::Relaxed) != generation {
                    return None;
                }
                let result = self.call(&name, arguments, Some((current, generation)), terms);
                (current.load(Ordering::Relaxed) == generation).then_some(Event::Action { place, generation, result })
            }
        }
    }

    fn invoke(&mut self, name: &str, arguments: Value, current: &AtomicU64, generation: u64) -> Result<Value, String> {
        self.call(name, arguments.into(), Some((current, generation)), Terms::default())
    }

    fn call(
        &mut self,
        name: &str,
        arguments: Arguments,
        cancel: Option<(&AtomicU64, u64)>,
        terms: Terms,
    ) -> Result<Value, String> {
        if Instant::now() < self.retry_after {
            return Err("Plugin is restarting; try again shortly".into());
        }
        let result = self.connect().and_then(|session| session.tool(name, arguments, cancel, terms));
        if matches!(result, Err(PluginError::Transport(_))) {
            self.lost();
        }
        result.map_err(|error| error.to_string())
    }

    /// Waits for a request or cancellation, reading what the plugin sends
    /// meanwhile.
    fn idle(&mut self) {
        let result = match &mut self.session {
            Some(session) => session.idle(),
            None => {
                wait_for_bell(&self.wake);
                Ok(())
            }
        };
        if let Err(error) = result {
            tracing::warn!("Plugin {}: {error}", self.name);
            self.lost();
        }
    }

    /// Ends a broken process; the next request starts it again.
    fn lost(&mut self) {
        self.session = None;
        self.retry_after = Instant::now() + Duration::from_secs(1);
    }

    /// Passes on what the plugin sent on its own; a subscribed resource
    /// that changed is marked to be read again instead.
    fn forward(&mut self, events: &channel::Sender<Event>) {
        let Some(session) = &mut self.session else { return };
        for (method, params) in session.notifications.drain(..) {
            if method == "notifications/resources/updated"
                && let Some(uri) = params["uri"].as_str()
                && self.resources.iter().any(|resource| resource == uri)
            {
                self.dirty.insert(uri.to_string());
                continue;
            }
            let _ = events.send(Event::Notification { plugin: self.name.clone(), method, params });
        }
    }
}

fn wait_for_bell(wake: &UnixDatagram) {
    wait_for_bell_until(wake, None);
}

/// Waits for the bell, or at most `limit`.
fn wait_for_bell_until(wake: &UnixDatagram, limit: Option<Duration>) {
    let timeout = limit.map_or(-1, |limit| i32::try_from(limit.as_millis().saturating_add(1)).unwrap_or(i32::MAX));
    let mut descriptors = [libc::pollfd { fd: wake.as_raw_fd(), events: libc::POLLIN, revents: 0 }];
    #[allow(unsafe_code, reason = "poll waits on the owned wake descriptor")]
    // SAFETY: descriptors points to one initialized pollfd for a live descriptor.
    unsafe {
        libc::poll(descriptors.as_mut_ptr(), 1, timeout);
    }
    let mut buffer = [0; 128];
    while wake.recv(&mut buffer).is_ok() {}
}

/// Highest-priority manifests are visited first so prefix collisions have a
/// deterministic winner: config, then user manifests, then system ones, then
/// the bundled plugins. Config overrides inherit unspecified manifest fields.
fn discover(settings: Settings) -> (Vec<(String, PluginConfig)>, Vec<String>) {
    let home = PathBuf::from(std::env::var_os("HOME").unwrap_or_default());
    let user = std::env::var_os("XDG_DATA_HOME").map(PathBuf::from).unwrap_or_else(|| home.join(".local/share"));
    let mut manifests = BTreeMap::<String, (u8, PluginConfig)>::new();
    let mut diagnostics = settings.warnings;
    match crate::bundled::manifests(&settings.bundled) {
        Ok(bundled) => manifests.extend(bundled.into_iter().map(|(plugin_id, config)| (plugin_id, (0, config)))),
        Err(error) => diagnostics.push(error),
    }
    for (priority, directory) in
        [(1, PathBuf::from("/usr/share/tileception/plugins")), (2, user.join("tileception/plugins"))]
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
                    let value = normalize(plugin_id, value, &mut diagnostics);
                    manifests.insert(plugin_id.into(), (priority, value));
                }
                Err(error) => diagnostics.push(format!("{}: {error}", path.display())),
            }
        }
    }
    for (plugin_id, config) in settings.plugins {
        let (priority, base) = manifests.entry(plugin_id).or_default();
        *priority = 3;
        overlay(base, config);
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

pub fn parse_rows(value: Value) -> Result<Vec<Row>, String> {
    let rows = value["rows"].as_array().ok_or("Plugin result has no rows array")?;
    let mut parsed = Vec::new();
    for value in rows {
        let mut value = value.clone();
        let actions = value.as_object_mut().and_then(|row| row.remove("actions"));
        let alternate = value.as_object_mut().and_then(|row| row.remove("alternate"));
        match serde_json::from_value::<Row>(value) {
            Ok(mut row) => {
                if let Some(alternate) = alternate {
                    match serde_json::from_value::<Action>(alternate) {
                        Ok(action) => row.alternate = Some(Box::new(action)),
                        Err(error) => tracing::warn!("Skipping invalid launcher plugin alternate action: {error}"),
                    }
                }
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

/// Notifications kept between forwards; a flood beyond this is dropped.
const NOTIFICATIONS: usize = 256;

struct Session {
    child: Child,
    input: ChildStdin,
    output: ChildStdout,
    buffered: Vec<u8>,
    next_id: u64,
    stopping: Arc<AtomicBool>,
    wake: Arc<UnixDatagram>,
    cancelled: BTreeMap<u64, Instant>,
    /// Notifications from the plugin, by method, not yet forwarded.
    notifications: Vec<(String, Value)>,
    /// What the plugin said it supports when initialized.
    capabilities: Value,
}

impl Session {
    fn start(
        name: &str,
        config: &PluginConfig,
        stopping: Arc<AtomicBool>,
        environment: &Environment,
        wake: Arc<UnixDatagram>,
        events: Option<&channel::Sender<Event>>,
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
        // Its own Wayland connection, for drawing on the dashboard only.
        let mut dashboard = None;
        if config.draws_surfaces() {
            let (ours, theirs) = std::os::unix::net::UnixStream::pair().map_err(|error| error.to_string())?;
            let descriptor = theirs.as_raw_fd();
            process.env("WAYLAND_SOCKET", descriptor.to_string());
            #[allow(unsafe_code, reason = "the plugin inherits its end of the dashboard connection")]
            // SAFETY: only async-signal-safe fcntl runs between fork and exec,
            // on a descriptor this process owns until the child has started.
            unsafe {
                process.pre_exec(move || {
                    if libc::fcntl(descriptor, libc::F_SETFD, 0) < 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
            dashboard = Some((ours, theirs));
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
        // Our end joins the dashboard's display before the plugin is asked
        // anything, so it may connect first; closing our copy of its end
        // leaves the plugin's the only one.
        if let Some((ours, _theirs)) = dashboard
            && let Some(events) = events
        {
            let _ = events.send(Event::DashboardClient { plugin: name.to_string(), stream: ours });
        }
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
        let mut session = Self {
            child,
            input,
            output,
            buffered: Vec::new(),
            next_id: 0,
            stopping,
            wake,
            cancelled: BTreeMap::new(),
            notifications: Vec::new(),
            capabilities: Value::Null,
        };
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
                "clientInfo": { "name": "tileception", "version": env!("CARGO_PKG_VERSION") }
            }),
            None,
        )?;
        if initialized["protocolVersion"] != "2025-06-18" || !initialized["capabilities"]["tools"].is_object() {
            return Err("Plugin must support MCP 2025-06-18 tools".into());
        }
        session.write(
            &json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
            Instant::now() + TIMEOUT,
            None,
        )?;
        session.capabilities = initialized["capabilities"].clone();
        Ok(session)
    }

    /// Subscribes to every `tileception://` resource the plugin lists, when
    /// it supports subscriptions; returns their URIs.
    fn subscribe_all(&mut self) -> Result<Vec<String>, PluginError> {
        if self.capabilities["resources"]["subscribe"] != true {
            return Ok(Vec::new());
        }
        let listed = self.request("resources/list", json!({}), None)?;
        let uris: Vec<String> = listed["resources"]
            .as_array()
            .map(|resources| {
                resources
                    .iter()
                    .filter_map(|resource| resource["uri"].as_str())
                    .filter(|uri| uri.starts_with("tileception://"))
                    .map(String::from)
                    .collect()
            })
            .unwrap_or_default();
        for uri in &uris {
            self.request("resources/subscribe", json!({ "uri": uri }), None)?;
        }
        Ok(uris)
    }

    /// A resource's JSON text, parsed.
    fn read(&mut self, uri: &str) -> Result<Value, PluginError> {
        let result = self.request("resources/read", json!({ "uri": uri }), None)?;
        let text = result["contents"]
            .as_array()
            .and_then(|contents| contents.iter().find_map(|content| content["text"].as_str()))
            .ok_or_else(|| PluginError::Tool(format!("{uri} has no text")))?;
        serde_json::from_str(text).map_err(|error| PluginError::Tool(format!("{uri}: {error}")))
    }

    fn tool(
        &mut self,
        name: &str,
        mut arguments: Arguments,
        cancel: Option<(&AtomicU64, u64)>,
        terms: Terms,
    ) -> Result<Value, PluginError> {
        // Moved, not copied as `json!` would, so a secret has one copy.
        let mut params = serde_json::Map::new();
        params.insert("name".into(), name.into());
        params.insert("arguments".into(), std::mem::take(&mut arguments.value));
        let terms = Terms { secret: arguments.secret, ..terms };
        let result = self.request_with("tools/call", Value::Object(params), cancel, terms)?;
        tool_result(result)
    }

    fn write(
        &mut self,
        value: &Value,
        deadline: Instant,
        cancel: Option<(&AtomicU64, u64)>,
    ) -> Result<(), PluginError> {
        self.write_with(value, deadline, cancel, false)
    }

    /// Writes one message. A secret one is serialized into a buffer of the
    /// exact size, so growing it leaves no copies behind, and wiped after.
    fn write_with(
        &mut self,
        value: &Value,
        deadline: Instant,
        cancel: Option<(&AtomicU64, u64)>,
        secret: bool,
    ) -> Result<(), PluginError> {
        let capacity = if secret { serialized_len(value)? + 1 } else { 0 };
        let mut bytes = Zeroizing::new(Vec::with_capacity(capacity));
        serde_json::to_writer(&mut *bytes, value).map_err(|error| error.to_string())?;
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
        self.request_with(method, params, cancel, Terms::default())
    }

    fn request_with(
        &mut self,
        method: &str,
        params: Value,
        cancel: Option<(&AtomicU64, u64)>,
        terms: Terms,
    ) -> Result<Value, PluginError> {
        self.exchange(method, params, cancel, terms).map_err(|error| match error {
            PluginError::Transport(message) if message == TIMED_OUT => {
                format!("Plugin timed out after {} seconds", terms.timeout.as_secs()).into()
            }
            error => error,
        })
    }

    fn exchange(
        &mut self,
        method: &str,
        params: Value,
        cancel: Option<(&AtomicU64, u64)>,
        terms: Terms,
    ) -> Result<Value, PluginError> {
        self.next_id += 1;
        let request_id = self.next_id;
        let deadline = Instant::now() + terms.timeout;
        let mut message = serde_json::Map::new();
        message.insert("jsonrpc".into(), "2.0".into());
        message.insert("id".into(), request_id.into());
        message.insert("method".into(), method.into());
        message.insert("params".into(), params);
        // Wiped when it goes, if secret, whichever way this ends.
        let message = Arguments { value: Value::Object(message), secret: terms.secret };
        check(deadline, cancel)?;
        // Finish the frame before observing cancellation so the stream stays valid.
        self.write_with(&message.value, deadline, None, terms.secret)?;
        drop(message);
        loop {
            if self.stopping.load(Ordering::Relaxed) {
                return Err("Plugin host is stopping".into());
            }
            if let Err(error) = check(deadline, cancel) {
                if error == PluginError::Cancelled {
                    self.write(
                        &json!({
                            "jsonrpc": "2.0",
                            "method": "notifications/cancelled",
                            "params": { "requestId": request_id, "reason": error.to_string() }
                        }),
                        deadline,
                        None,
                    )?;
                    if !terms.forget {
                        self.cancelled.insert(request_id, deadline);
                    }
                }
                return Err(error);
            }
            if let Some(message) = self.next_message()? {
                if self.unsolicited(&message, deadline)? {
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
            if !self.fill()? {
                self.wait(self.output.as_raw_fd(), libc::POLLIN, deadline)?;
            }
        }
    }

    /// The next complete message already read, if any.
    fn next_message(&mut self) -> Result<Option<Value>, PluginError> {
        let Some(end) = self.buffered.iter().position(|byte| *byte == b'\n') else {
            return Ok(None);
        };
        let line: Vec<_> = self.buffered.drain(..=end).collect();
        let message: Value = serde_json::from_slice(&line).map_err(|error| error.to_string())?;
        if message["jsonrpc"] != "2.0" {
            return Err("Invalid JSON-RPC version".into());
        }
        Ok(Some(message))
    }

    /// Reads what is available. False when nothing was.
    fn fill(&mut self) -> Result<bool, PluginError> {
        let mut buffer = [0; 8192];
        let mut read = false;
        loop {
            match self.output.read(&mut buffer) {
                Ok(0) => return Err("Plugin exited or closed stdout".into()),
                Ok(count) => {
                    read = true;
                    self.buffered.extend_from_slice(&buffer[..count]);
                    if self.buffered.len() > LIMIT {
                        return Err("Plugin response exceeds 1 MiB".into());
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => return Ok(read),
                Err(error) => return Err(error.to_string().into()),
            }
        }
    }

    /// Handles a request or notification from the plugin. False for a
    /// response, which the caller handles.
    fn unsolicited(&mut self, message: &Value, deadline: Instant) -> Result<bool, PluginError> {
        let Some(method) = message.get("method") else {
            return Ok(false);
        };
        if message.get("id").is_some() {
            let response = if method == "ping" {
                json!({"jsonrpc":"2.0","id":message["id"],"result":{}})
            } else {
                json!({
                    "jsonrpc": "2.0", "id": message["id"],
                    "error": { "code": -32601, "message": "Unsupported client method" }
                })
            };
            self.write(&response, deadline, None)?;
        } else if let Some(method) = method.as_str()
            && self.notifications.len() < NOTIFICATIONS
        {
            self.notifications.push((method.into(), message.get("params").cloned().unwrap_or(Value::Null)));
        }
        Ok(true)
    }

    fn wait(&self, descriptor: i32, events: i16, deadline: Instant) -> Result<(), PluginError> {
        let deadline = self.cancelled.values().copied().min().map_or(deadline, |pending| pending.min(deadline));
        check(deadline, None)?;
        let remaining = deadline.saturating_duration_since(Instant::now());
        let timeout = i32::try_from(remaining.as_millis().saturating_add(1)).unwrap_or(i32::MAX);
        self.poll(descriptor, events, timeout)
    }

    /// Waits for the descriptor, the wake socket or the timeout (-1: none),
    /// and empties the wake socket.
    fn poll(&self, descriptor: i32, events: i16, timeout: i32) -> Result<(), PluginError> {
        let mut descriptors = [
            libc::pollfd { fd: descriptor, events, revents: 0 },
            libc::pollfd { fd: self.wake.as_raw_fd(), events: libc::POLLIN, revents: 0 },
        ];
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

    /// Waits while no request is in flight: reads what the plugin sends on
    /// its own, drains superseded responses within their original
    /// deadlines, and notices the plugin exiting.
    fn idle(&mut self) -> Result<(), PluginError> {
        let timeout = self.cancelled.values().min().map_or(-1, |deadline| {
            let remaining = deadline.saturating_duration_since(Instant::now());
            i32::try_from(remaining.as_millis().saturating_add(1)).unwrap_or(i32::MAX)
        });
        // A message read along with the last answer is already here.
        if !self.buffered.contains(&b'\n') {
            self.poll(self.output.as_raw_fd(), libc::POLLIN, timeout)?;
        }
        let closed = self.fill().err();
        while let Some(message) = self.next_message()? {
            if !self.unsolicited(&message, Instant::now() + TIMEOUT)?
                && let Some(request_id) = message["id"].as_u64()
            {
                self.cancelled.remove(&request_id);
            }
        }
        if let Some(error) = closed {
            return Err(error);
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
        return Err(TIMED_OUT.into());
    }
    Ok(())
}

/// What [`check`] says when time is up; [`Session::request_with`] adds how long.
const TIMED_OUT: &str = "Plugin timed out";

/// How many bytes a value serializes to.
fn serialized_len(value: &Value) -> Result<usize, PluginError> {
    struct Count(usize);
    impl Write for Count {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0 += bytes.len();
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut count = Count(0);
    serde_json::to_writer(&mut count, value).map_err(|error| error.to_string())?;
    Ok(count.0)
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
        assert_eq!(rows[0].actions[0].kind, ActionKind::Copy { text: "hello".into(), sensitive: false });
        assert_eq!(rows[1].title, "Last");
    }

    #[test]
    fn prompts_are_checked_and_bad_ones_skipped() {
        let prompt = |steps: Value| json!({ "title": "Prompt", "type": "prompt", "prompt": { "title": "Unlock", "steps": steps } });
        let wait = json!({ "id": "key", "kind": "wait", "text": "Touch your security key", "tool": "unlock_key",
            "timeout": 90, "alternate": { "title": "Use passphrase instead", "step": "password" } });
        let fields = json!({ "id": "password", "kind": "fields", "submit": "Unlock", "tool": "unlock_password",
            "fields": [{ "name": "passphrase", "label": "Passphrase", "secret": true }] });
        let rows = parse_rows(json!({ "rows": [{ "title": "Vault", "actions": [
            prompt(json!([wait, fields])),
            // The alternate goes nowhere.
            prompt(json!([wait])),
            prompt(json!([fields, fields])),
            prompt(json!([])),
            prompt(json!([{ "id": "empty", "kind": "fields", "tool": "t", "fields": [] }])),
            prompt(json!([{ "id": "many", "kind": "fields", "tool": "t",
                "fields": (0..5).map(|index| json!({ "name": format!("f{index}") })).collect::<Vec<_>>() }])),
            prompt(json!([{ "id": "odd", "kind": "dance", "tool": "t" }])),
            prompt(json!((0..9).map(|index| json!({ "id": format!("s{index}"), "kind": "wait", "tool": "t" })).collect::<Vec<_>>())),
            { "title": "Copy code", "type": "copy", "text": "123456", "sensitive": true }
        ] }] }))
        .unwrap();
        let actions = &rows[0].actions;
        assert_eq!(actions.len(), 2);
        let ActionKind::Prompt { prompt } = &actions[0].kind else { panic!("a prompt") };
        assert_eq!(prompt.steps.len(), 2);
        assert_eq!(prompt.step_index("password"), Some(1));
        assert_eq!(prompt.steps[0].kind, StepKind::Wait);
        assert_eq!(prompt.steps[0].timeout(), Duration::from_secs(60), "clamped");
        assert_eq!(prompt.steps[1].timeout(), TIMEOUT);
        let StepKind::Fields { fields, submit } = &prompt.steps[1].kind else { panic!("fields") };
        assert!(fields[0].secret);
        assert_eq!(submit.as_deref(), Some("Unlock"));
        assert_eq!(actions[1].kind, ActionKind::Copy { text: "123456".into(), sensitive: true });
        assert!(!actions[0].kind.after_prompt() && actions[1].kind.after_prompt());
    }

    #[test]
    fn secret_arguments_are_wiped() {
        let mut value = json!({ "passphrase": "hunter2", "nested": ["one", { "two": "2" }], "count": 3 });
        wipe(&mut value);
        assert_eq!(value, json!({ "passphrase": "", "nested": ["", { "two": "" }], "count": 3 }));
        let message = json!({ "jsonrpc": "2.0", "params": { "text": "caf\u{e9} \"quoted\"" } });
        assert_eq!(serialized_len(&message), Ok(serde_json::to_vec(&message).unwrap().len()));
    }

    #[test]
    fn a_prefix_needs_a_space() {
        let info = Info { name: "Folders".into(), description: String::new(), icon: None, prefixes: vec![] };
        let routes = [Route { plugin_id: "folders".into(), info, prefixes: vec!["z".into()] }];
        assert_eq!(route_for(&routes, "z ").map(|(_, prefix)| prefix), Some("z"));
        assert_eq!(route_for(&routes, "z proj web").map(|(_, prefix)| prefix), Some("z"));
        assert!(route_for(&routes, "z").is_none());
        assert!(route_for(&routes, "zoom").is_none());
    }

    #[test]
    fn rows_keep_completion_and_skip_a_bad_alternate() {
        let rows = parse_rows(json!({ "rows": [
            { "title": "Good", "completion": "~/src/", "alternate": { "title": "Terminal", "type": "terminal", "directory": "/src" } },
            { "title": "Bad alternate", "alternate": { "title": "Future", "type": "future" } }
        ] }))
        .unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].completion.as_deref(), Some("~/src/"));
        assert_eq!(rows[0].alternate.as_ref().map(|action| action.title.as_str()), Some("Terminal"));
        assert!(rows[1].alternate.is_none());
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
    fn chips_need_a_bar_block() {
        let file: crate::config::ConfigFile = toml::from_str(
            r#"
            [plugins.plain]
            command = "plain"

            [plugins.chips]
            command = "chips"
            [plugins.chips.bar]

            [plugins.hidden.bar]
            enabled = false
            "#,
        )
        .unwrap();
        let settings = Settings::new(&file.plugins, &file.launcher);
        assert!(!settings.plugins["plain"].in_bar());
        assert!(settings.plugins["chips"].in_bar());
        // A manifest's block, switched off in the config.
        let mut manifest = PluginConfig { bar: Some(BarOptions::default()), ..PluginConfig::default() };
        assert!(manifest.in_bar());
        overlay(&mut manifest, settings.plugins["hidden"].clone());
        assert!(!manifest.in_bar());
    }

    #[test]
    fn dashboard_items_and_surfaces_are_opt_in() {
        let file: crate::config::ConfigFile = toml::from_str(
            r#"
            [plugins.plain]
            command = "plain"

            [plugins.cards]
            command = "cards"
            [plugins.cards.dashboard]

            [plugins.lamp]
            command = "lamp"
            [plugins.lamp.dashboard]
            surface = true

            [plugins.off.dashboard]
            enabled = false
            "#,
        )
        .unwrap();
        let settings = Settings::new(&file.plugins, &file.launcher);
        assert!(!settings.plugins["plain"].on_dashboard());
        assert!(settings.plugins["cards"].on_dashboard() && !settings.plugins["cards"].draws_surfaces());
        assert!(settings.plugins["lamp"].draws_surfaces());
        // Switched off in the config: no items and no connection either.
        let dashboard = DashboardOptions { enabled: None, surface: Some(true) };
        let mut manifest = PluginConfig { dashboard: Some(dashboard), ..PluginConfig::default() };
        assert!(manifest.draws_surfaces());
        overlay(&mut manifest, settings.plugins["off"].clone());
        assert!(!manifest.on_dashboard() && !manifest.draws_surfaces());
    }

    #[test]
    fn launcher_cards_show_unless_switched_off() {
        let file: crate::config::ConfigFile = toml::from_str(
            r#"
            [plugins.plain]
            command = "plain"

            [plugins.hidden.launcher]
            cards = false
            "#,
        )
        .unwrap();
        let settings = Settings::new(&file.plugins, &file.launcher);
        assert!(settings.plugins["plain"].launcher_cards());
        assert!(!settings.plugins["hidden"].launcher_cards());
        // A manifest's prefixes stay when the config only hides its cards.
        let launcher = LauncherOptions { prefixes: Some(vec!["t".into()]), cards: None };
        let mut manifest = PluginConfig { launcher: Some(launcher), ..PluginConfig::default() };
        assert!(manifest.launcher_cards());
        overlay(&mut manifest, settings.plugins["hidden"].clone());
        assert!(!manifest.launcher_cards());
        assert_eq!(manifest.launcher.unwrap().prefixes, Some(vec!["t".into()]));
    }

    #[test]
    fn old_launcher_config_folds_into_plugins() {
        let file: crate::config::ConfigFile = toml::from_str(
            r#"
            [launcher.plugins.old]
            command = "old"
            prefixes = ["o"]

            [launcher.plugins.both]
            command = "first"
            prefixes = ["b"]

            [plugins.both]
            command = "second"

            [plugins.new]
            command = "new"
            launcher = { prefixes = ["n"] }
            "#,
        )
        .unwrap();
        let settings = Settings::new(&file.plugins, &file.launcher);
        let prefixes = |plugin_id: &str| settings.plugins[plugin_id].launcher.as_ref().unwrap().prefixes.clone();
        assert_eq!(prefixes("old"), Some(vec!["o".into()]));
        assert_eq!(prefixes("both"), Some(vec!["b".into()]));
        assert_eq!(prefixes("new"), Some(vec!["n".into()]));
        assert_eq!(settings.plugins["both"].command.as_deref(), Some("second"));
        assert!(settings.plugins.values().all(|config| config.prefixes.is_none()));
        assert!(settings.warnings.iter().any(|warning| warning == "[launcher.plugins.old] is now [plugins.old]"));
        assert_eq!(settings.warnings.len(), 4);
    }

    #[test]
    fn tool_error_and_cancelled_search_keep_the_session() {
        let script = r#"
import json, os, sys, time
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
        if name == 'notify':
            print(json.dumps({'jsonrpc': '2.0', 'method': 'notifications/early', 'params': {'n': 1}}), flush=True)
        result = {'isError': True, 'content': [{'type': 'text', 'text': 'Please sign in'}]} if name == 'fail' else {'structuredContent': {'ok': True}}
        if name == 'display':
            result = {'structuredContent': {'display': os.environ.get('DISPLAY')}}
        if name == 'later':
            print(json.dumps({'jsonrpc': '2.0', 'id': mid, 'result': result}), flush=True)
            time.sleep(0.1)
            print(json.dumps({'jsonrpc': '2.0', 'method': 'notifications/later', 'params': {'n': 2}}), flush=True)
            continue
        if name == 'together':
            answer = json.dumps({'jsonrpc': '2.0', 'id': mid, 'result': result})
            news = json.dumps({'jsonrpc': '2.0', 'method': 'notifications/together', 'params': {'n': 3}})
            sys.stdout.write(answer + '\n' + news + '\n')
            sys.stdout.flush()
            continue
        if name == 'exit':
            print(json.dumps({'jsonrpc': '2.0', 'id': mid, 'result': result}), flush=True)
            sys.exit(0)
    else:
        continue
    print(json.dumps({'jsonrpc': '2.0', 'id': mid, 'result': result}), flush=True)
"#;
        let (wake, reader) = UnixDatagram::pair().unwrap();
        wake.set_nonblocking(true).unwrap();
        reader.set_nonblocking(true).unwrap();
        let mut source = Source {
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
            resources: Vec::new(),
            dirty: std::collections::BTreeSet::new(),
            events: None,
        };
        let current = Arc::new(AtomicU64::new(0));
        assert_eq!(source.invoke("fail", json!({}), &current, 0), Err("Please sign in".into()));
        let pid = source.session.as_ref().unwrap().child.id();
        let generation = current.clone();
        let wake = Arc::new(wake);
        let bell = wake.clone();
        let cancel = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            generation.store(1, Ordering::Relaxed);
            bell.send(&[1]).unwrap();
        });
        assert_eq!(source.invoke("slow", json!({}), &current, 0), Err("Plugin request cancelled".into()));
        cancel.join().unwrap();
        assert_eq!(source.invoke("ok", json!({}), &current, 1), Ok(json!({ "ok": true })));
        assert_eq!(source.session.as_ref().unwrap().child.id(), pid);

        // Notifications arrive both during a request and while idle.
        assert_eq!(source.invoke("notify", json!({}), &current, 1), Ok(json!({ "ok": true })));
        assert_eq!(source.invoke("later", json!({}), &current, 1), Ok(json!({ "ok": true })));
        // Idle returns early for a stray wake byte too.
        for _ in 0..10 {
            if source.session.as_ref().unwrap().notifications.len() == 2 {
                break;
            }
            source.idle();
        }
        let methods: Vec<_> =
            source.session.as_mut().unwrap().notifications.drain(..).map(|(method, _)| method).collect();
        assert_eq!(methods, ["notifications/early", "notifications/later"]);

        // One that came in the same read as an answer does not wait for the
        // plugin to write again.
        assert_eq!(source.invoke("together", json!({}), &current, 1), Ok(json!({ "ok": true })));
        let mut bells = [0; 8];
        while source.wake.recv(&mut bells).is_ok() {}
        source.idle();
        let methods: Vec<_> =
            source.session.as_mut().unwrap().notifications.drain(..).map(|(method, _)| method).collect();
        assert_eq!(methods, ["notifications/together"]);

        // A plugin exiting while idle is noticed without a request.
        assert_eq!(source.invoke("exit", json!({}), &current, 1), Ok(json!({ "ok": true })));
        for _ in 0..10 {
            if source.session.is_none() {
                break;
            }
            source.idle();
        }
        assert!(source.session.is_none());

        source.retry_after = Instant::now();
        source.environment.write().unwrap().push(("DISPLAY".into(), Some(":42".into())));
        assert_eq!(source.invoke("display", json!({}), &current, 1), Ok(json!({ "display": ":42" })));

        // A prompt's cancelled wait is let go: the plugin need not answer
        // and keeps running past the request's deadline.
        let pid = source.session.as_ref().unwrap().child.id();
        let prompt = Terms { timeout: Duration::from_millis(300), forget: true, secret: false };
        let generation = current.clone();
        let bell = wake.clone();
        let cancel = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            generation.store(2, Ordering::Relaxed);
            bell.send(&[1]).unwrap();
        });
        assert_eq!(
            source.call("slow", json!({}).into(), Some((&current, 1)), prompt),
            Err("Plugin request cancelled".into())
        );
        cancel.join().unwrap();
        std::thread::sleep(Duration::from_millis(400));
        // Nothing is pending, so idling waits for the bell: ring it.
        wake.send(&[1]).unwrap();
        source.idle();
        assert_eq!(source.session.as_ref().map(|session| session.child.id()), Some(pid));
        // The late answer to it is ignored by request id.
        assert_eq!(source.invoke("ok", json!({}), &current, 2), Ok(json!({ "ok": true })));

        // A secret is sent like anything else.
        let secret = Arguments::secret(json!({ "passphrase": "hunter2" }));
        assert_eq!(source.call("ok", secret, None, Terms::default()), Ok(json!({ "ok": true })));

        // A prompt's own deadline ends a plugin that never answers.
        let result = source.call("slow", json!({}).into(), None, prompt);
        assert!(result.as_ref().is_err_and(|error| error.starts_with("Plugin timed out")), "{result:?}");
        assert!(source.session.is_none());
    }
}
