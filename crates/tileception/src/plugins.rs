//! Plugins: executables speaking MCP over stdio, found through manifests and
//! `[plugins.<id>]`. For now they add launcher sources.
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
    /// Launcher prefixes from before the `[launcher]` block.
    pub prefixes: Option<Vec<String>>,
}

impl PluginConfig {
    /// Whether the plugin's chips show in the bar.
    pub fn in_bar(&self) -> bool {
        self.bar.as_ref().is_some_and(|bar| bar.enabled != Some(false))
    }
}

/// A plugin's `[launcher]` block.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LauncherOptions {
    pub prefixes: Option<Vec<String>>,
}

/// A plugin's `[bar]` block.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct BarOptions {
    pub enabled: Option<bool>,
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
    if let Some(prefixes) = overlay.launcher.and_then(|launcher| launcher.prefixes) {
        base.launcher.get_or_insert_default().prefixes = Some(prefixes);
    }
    if let Some(bar) = overlay.bar {
        let base = base.bar.get_or_insert_default();
        if bar.enabled.is_some() {
            base.enabled = bar.enabled;
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

/// What [`Host::handle`] changed.
#[derive(Debug, Default)]
pub struct Changed {
    pub routes: bool,
    pub panels: bool,
    pub bar: bool,
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
    Tool { place: Place, generation: u64, name: String, arguments: Value },
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
            infos: BTreeMap::new(),
            found: Vec::new(),
            workers: BTreeMap::new(),
            retired: Vec::new(),
            routes: Vec::new(),
            diagnostics: Vec::new(),
            panels: BTreeMap::new(),
            bars: BTreeMap::new(),
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
        Changed { routes, panels: reloaded, bar: reloaded, resources: Vec::new() }
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
        } else {
            self.resources.insert((plugin.clone(), uri.clone()), value);
            changed.resources.push((plugin, uri));
        }
        changed
    }

    /// The panel blocks, highest priority first.
    pub fn panels(&self) -> Vec<(&str, &crate::panel::block::Block)> {
        self.order.iter().filter_map(|(plugin_id, _)| Some((plugin_id.as_str(), self.panels.get(plugin_id)?))).collect()
    }

    /// The bar chips of plugins with a `[bar]` block, highest priority first.
    pub fn bar_chips(&self) -> Vec<(&str, &crate::panel::block::BarChip)> {
        self.order
            .iter()
            .filter(|(_, config)| config.in_bar())
            .filter_map(|(plugin_id, _)| Some((plugin_id.as_str(), self.bars.get(plugin_id)?)))
            .flat_map(|(plugin_id, bar)| bar.chips.iter().map(move |chip| (plugin_id, chip)))
            .collect()
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
        let generation = if place.cancels() {
            self.cancel(place)
        } else {
            self.generations.get(place).fetch_add(1, Ordering::Relaxed) + 1
        };
        if let Some(worker) = self.workers.get(&plugin) {
            worker.send(Request::Tool { place, generation, name, arguments });
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
        if changed.bar {
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
            self.session =
                Some(Session::start(&self.name, &self.config, self.stopping.clone(), &environment, self.wake.clone())?);
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
            let value = session.tool("launcher_info", json!({}), None)?;
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
            Request::Tool { place, generation, name, arguments } if !place.cancels() => {
                let result = self.call(&name, arguments, None);
                Some(Event::Action { place, generation, result })
            }
            Request::Tool { place, generation, name, arguments } => {
                let current = generations.get(place);
                if current.load(Ordering::Relaxed) != generation {
                    return None;
                }
                let result = self.invoke(&name, arguments, current, generation);
                (current.load(Ordering::Relaxed) == generation).then_some(Event::Action { place, generation, result })
            }
        }
    }

    fn invoke(&mut self, name: &str, arguments: Value, current: &AtomicU64, generation: u64) -> Result<Value, String> {
        self.call(name, arguments, Some((current, generation)))
    }

    fn call(&mut self, name: &str, arguments: Value, cancel: Option<(&AtomicU64, u64)>) -> Result<Value, String> {
        if Instant::now() < self.retry_after {
            return Err("Plugin is restarting; try again shortly".into());
        }
        let result = self.connect().and_then(|session| session.tool(name, arguments, cancel));
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
        session.write(json!({"jsonrpc":"2.0","method":"notifications/initialized"}), Instant::now() + TIMEOUT, None)?;
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
            self.write(response, deadline, None)?;
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
        self.poll(self.output.as_raw_fd(), libc::POLLIN, timeout)?;
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
    }
}
