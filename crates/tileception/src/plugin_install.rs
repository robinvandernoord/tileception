//! Installs plugins from GitHub repositories with `uv tool`, and configures,
//! updates and removes them again.
//!
//! A plugin repository is a Python package with one console script that
//! takes the standard subcommands (`serve`, `install --non-interactive`,
//! `configure`, `status` and `uninstall`; see `docs/plugins.md`). Its
//! `pyproject.toml` is read before anything is installed, so the launcher
//! can say what is about to run. What was installed this way is kept in
//! `$XDG_DATA_HOME/tileception/installed-plugins.toml`, by plugin ID.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use smithay::reexports::calloop::channel;

use crate::Tileception;

/// How long `uv tool install` and `uv tool upgrade` may take.
const UV_TIMEOUT: Duration = Duration::from_secs(15 * 60);
/// How long a plugin's own `install` may take: some download a model.
const INSTALL_TIMEOUT: Duration = Duration::from_secs(30 * 60);
/// How long `status` and `uninstall` may take.
const QUICK_TIMEOUT: Duration = Duration::from_secs(60);
/// The largest `pyproject.toml` read.
const MAX_PYPROJECT: &str = "1000000";

/// A GitHub repository, optionally at a tag, branch or commit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Source {
    pub owner: String,
    pub repo: String,
    pub rev: Option<String>,
}

impl Source {
    /// Reads `owner/repo`, `owner/repo@v1.2.0` or a github.com URL.
    pub fn parse(text: &str) -> Result<Self, String> {
        let text = text.trim();
        let mut rest = text;
        for prefix in ["git+https://", "https://", "http://"] {
            rest = rest.strip_prefix(prefix).unwrap_or(rest);
        }
        rest = rest.strip_prefix("www.").unwrap_or(rest);
        rest = rest.strip_prefix("github.com/").unwrap_or(rest);
        let (path, rev) = match rest.rsplit_once('@') {
            Some((path, rev)) => (path, Some(rev)),
            None => (rest, None),
        };
        let path = path.trim_end_matches('/');
        let path = path.strip_suffix(".git").unwrap_or(path);
        let mut parts = path.split('/');
        let (Some(owner), Some(repo), None) = (parts.next(), parts.next(), parts.next()) else {
            return Err(format!("{text} is not owner/repo or a GitHub repository URL"));
        };
        let name = |part: &str| {
            !part.is_empty()
                && !part.starts_with('.')
                && part.chars().all(|letter| letter.is_ascii_alphanumeric() || "-_.".contains(letter))
        };
        if !name(owner) || !name(repo) {
            return Err(format!("{text} is not owner/repo or a GitHub repository URL"));
        }
        if let Some(rev) = rev
            && (rev.is_empty()
                || rev.contains("..")
                || rev.starts_with(['-', '/'])
                || !rev.chars().all(|letter| letter.is_ascii_alphanumeric() || "-_./".contains(letter)))
        {
            return Err(format!("{rev} is not a tag, branch or commit"));
        }
        Ok(Self { owner: owner.into(), repo: repo.into(), rev: rev.map(Into::into) })
    }

    /// The URL of a file in the repository, at its revision or the
    /// default branch.
    fn raw_url(&self, file: &str) -> String {
        let rev = self.rev.as_deref().unwrap_or("HEAD");
        format!("https://raw.githubusercontent.com/{}/{}/{rev}/{file}", self.owner, self.repo)
    }

    /// The repository as pip and uv take it.
    fn git_url(&self) -> String {
        match &self.rev {
            Some(rev) => format!("git+https://github.com/{}/{}@{rev}", self.owner, self.repo),
            None => format!("git+https://github.com/{}/{}", self.owner, self.repo),
        }
    }
}

impl std::fmt::Display for Source {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}/{}", self.owner, self.repo)?;
        if let Some(rev) = &self.rev {
            write!(formatter, "@{rev}")?;
        }
        Ok(())
    }
}

/// What a repository's `pyproject.toml` says about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Package {
    /// The Python package name.
    pub name: String,
    pub version: String,
    pub description: String,
    /// The console script that takes the plugin subcommands.
    pub script: String,
    /// The plugin ID, which names its manifest.
    pub plugin_id: String,
    /// Extras installed with it.
    pub extras: Vec<String>,
    /// What it needs on the machine, in words, shown before installing.
    pub needs: Vec<String>,
    /// Its `requires-python`, passed to uv so it finds or fetches a Python.
    pub python: Option<String>,
}

#[derive(Deserialize)]
struct Pyproject {
    project: Option<Project>,
    #[serde(default)]
    tool: Tools,
}

#[derive(Deserialize)]
#[serde(rename_all = "kebab-case")]
struct Project {
    name: String,
    #[serde(default)]
    version: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    requires_python: Option<String>,
    #[serde(default)]
    scripts: BTreeMap<String, String>,
}

#[derive(Deserialize, Default)]
struct Tools {
    #[serde(default)]
    tileception: Option<Metadata>,
}

/// `[tool.tileception]`.
#[derive(Deserialize, Default)]
struct Metadata {
    #[serde(rename = "id")]
    plugin_id: Option<String>,
    script: Option<String>,
    #[serde(default)]
    extras: Vec<String>,
    #[serde(default)]
    needs: Vec<String>,
}

/// Reads a plugin's `pyproject.toml`.
pub fn package(text: &str) -> Result<Package, String> {
    let pyproject: Pyproject = toml::from_str(text).map_err(|error| format!("pyproject.toml: {}", error.message()))?;
    let project = pyproject.project.ok_or("pyproject.toml has no [project]: plugins install as Python packages")?;
    let metadata = pyproject.tool.tileception.unwrap_or_default();
    let word = |value: &str| {
        !value.is_empty() && value.chars().all(|letter| letter.is_ascii_alphanumeric() || "-_.".contains(letter))
    };
    if !word(&project.name) {
        return Err(format!("{} is not a package name", project.name));
    }
    let script = match metadata.script {
        Some(script) => script,
        None if project.scripts.len() == 1 => project.scripts.keys().next().cloned().unwrap_or_default(),
        None if project.scripts.contains_key(&project.name) => project.name.clone(),
        None => {
            return Err(format!("{} has no single console script; name it in [tool.tileception] script", project.name));
        }
    };
    if !word(&script) {
        return Err(format!("{script} is not a script name"));
    }
    let plugin_id = metadata.plugin_id.unwrap_or_else(|| {
        let name = project.name.to_lowercase();
        name.strip_prefix("tileception-").unwrap_or(&name).to_string()
    });
    if !word(&plugin_id) || plugin_id == "plugins" || plugin_id.contains('.') {
        return Err(format!("{plugin_id} cannot be a plugin ID"));
    }
    if let Some(extra) = metadata.extras.iter().find(|extra| !word(extra)) {
        return Err(format!("{extra} is not an extra"));
    }
    if let Some(python) = &project.requires_python
        && !python.chars().all(|letter| letter.is_ascii_alphanumeric() || " <>=!~.,*".contains(letter))
    {
        return Err(format!("{python} is not a Python version"));
    }
    Ok(Package {
        name: project.name,
        version: project.version.unwrap_or_default(),
        description: project.description.unwrap_or_default(),
        script,
        plugin_id,
        extras: metadata.extras,
        needs: metadata.needs,
        python: project.requires_python,
    })
}

/// The requirement `uv tool install` gets, extras and all.
fn requirement(source: &Source, package: &Package) -> String {
    let extras = if package.extras.is_empty() { String::new() } else { format!("[{}]", package.extras.join(",")) };
    format!("{}{extras} @ {}", package.name, source.git_url())
}

fn install_args(source: &Source, package: &Package) -> Vec<String> {
    let mut args = vec!["tool".to_string(), "install".to_string()];
    if let Some(python) = &package.python {
        args.extend(["--python".to_string(), python.trim().to_string()]);
    }
    args.push(requirement(source, package));
    args
}

/// A plugin installed from a repository.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Installed {
    /// `owner/repo`, with `@rev` when one was asked for.
    pub source: String,
    /// The Python package, for `uv tool upgrade` and `uninstall`.
    pub package: String,
    /// The script's absolute path.
    pub command: PathBuf,
}

/// Every installed plugin, by ID.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Record {
    #[serde(default)]
    pub plugins: BTreeMap<String, Installed>,
}

fn data_dir() -> PathBuf {
    let home = PathBuf::from(std::env::var_os("HOME").unwrap_or_default());
    std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".local/share"))
        .join("tileception")
}

fn record_path() -> PathBuf {
    data_dir().join("installed-plugins.toml")
}

fn manifest_path(plugin_id: &str) -> PathBuf {
    data_dir().join("plugins").join(format!("{plugin_id}.toml"))
}

impl Record {
    pub fn load() -> Self {
        Self::read(&record_path())
    }

    fn read(path: &Path) -> Self {
        match std::fs::read_to_string(path) {
            Ok(text) => toml::from_str(&text).unwrap_or_else(|error| {
                tracing::warn!("{}: {error}", path.display());
                Self::default()
            }),
            Err(_) => Self::default(),
        }
    }

    fn save(&self) -> Result<(), String> {
        let path = record_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|error| format!("{}: {error}", parent.display()))?;
        }
        let text = toml::to_string(self).map_err(|error| error.to_string())?;
        let partial = path.with_extension("toml.partial");
        std::fs::write(&partial, text).map_err(|error| format!("{}: {error}", partial.display()))?;
        std::fs::rename(&partial, &path).map_err(|error| format!("{}: {error}", path.display()))
    }
}

/// Work done off the event loop.
enum Job {
    Look(Source),
    /// Looks up and installs without showing it first.
    Add(Source),
    Install(Source, Package),
    Update(Vec<String>),
    Remove(String),
}

/// What a job reports back.
pub enum Report {
    Looked { source: Source, result: Result<Package, String> },
    Done { title: String, result: Result<String, String>, configure: Option<String> },
}

/// The launcher's view of installing.
#[derive(Default)]
pub struct Installer {
    sender: Option<channel::Sender<Report>>,
    /// What runs now, in words, such as "Installing tileception-odoo".
    pub busy: Option<String>,
    /// The last repository looked up, and what it said.
    pub looked: Option<(Source, Result<Package, String>)>,
    /// The repository being looked up.
    pub looking: Option<Source>,
    /// What is installed, read again after every change.
    pub record: Record,
}

/// `tileception msg plugins list`: one line per installed plugin.
pub fn listing() -> String {
    let record = Record::load();
    if record.plugins.is_empty() {
        return "No plugins were installed from a repository\n".into();
    }
    record
        .plugins
        .iter()
        .map(|(plugin_id, installed)| format!("{plugin_id}\t{}\t{}\n", installed.source, installed.command.display()))
        .collect()
}

/// Runs `command` without a terminal on stdin, gathering its output, and
/// ends it after `timeout`.
fn run(mut command: Command, timeout: Duration) -> Result<Output, String> {
    let program = command.get_program().to_string_lossy().into_owned();
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("{program}: {error}"))?;
    let gather = |pipe: Option<Box<dyn Read + Send>>| {
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            if let Some(mut pipe) = pipe {
                let _ = pipe.read_to_end(&mut bytes);
            }
            bytes
        })
    };
    let stdout = gather(child.stdout.take().map(|pipe| Box::new(pipe) as Box<dyn Read + Send>));
    let stderr = gather(child.stderr.take().map(|pipe| Box::new(pipe) as Box<dyn Read + Send>));
    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().map_err(|error| format!("{program}: {error}"))? {
            break status;
        }
        if started.elapsed() > timeout {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!("{program} took longer than {} minutes", timeout.as_secs().div_ceil(60)));
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    Ok(Output { status, stdout: stdout.join().unwrap_or_default(), stderr: stderr.join().unwrap_or_default() })
}

/// The last lines a failed program wrote, for a notification.
fn tail(output: &Output) -> String {
    let text = if output.stderr.iter().any(|byte| !byte.is_ascii_whitespace()) {
        String::from_utf8_lossy(&output.stderr)
    } else {
        String::from_utf8_lossy(&output.stdout)
    };
    let lines: Vec<&str> = text.lines().map(str::trim).filter(|line| !line.is_empty()).collect();
    lines[lines.len().saturating_sub(3)..].join("\n")
}

/// Runs a job's programs with the session's environment.
struct Runner {
    environment: Vec<(OsString, Option<OsString>)>,
}

impl Runner {
    fn command(&self, program: impl AsRef<std::ffi::OsStr>, args: &[&str]) -> Command {
        let mut command = Command::new(program);
        command.args(args);
        for (key, value) in &self.environment {
            match value {
                Some(value) => command.env(key, value),
                None => command.env_remove(key),
            };
        }
        command
    }

    /// Runs and fails with what it wrote when it does not succeed.
    fn check(&self, what: &str, command: Command, timeout: Duration) -> Result<Output, String> {
        let output = run(command, timeout)?;
        if output.status.success() { Ok(output) } else { Err(format!("{what} failed: {}", tail(&output))) }
    }

    fn find_uv(&self) -> Result<PathBuf, String> {
        tileception_acp::local::find("uv")
            .ok_or_else(|| "Plugins install with uv, which is not installed: see https://docs.astral.sh/uv/".into())
    }

    fn look(&self, source: &Source) -> Result<Package, String> {
        let cache = std::env::var_os("XDG_CACHE_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".cache"))
            .join("tileception");
        std::fs::create_dir_all(&cache).map_err(|error| format!("{}: {error}", cache.display()))?;
        let path = cache.join("plugin-pyproject.toml");
        tileception_acp::curl(&source.raw_url("pyproject.toml"), &path, MAX_PYPROJECT)
            .map_err(|error| format!("{source} has no pyproject.toml to read ({error})"))?;
        let text = std::fs::read_to_string(&path).map_err(|error| error.to_string());
        let _ = std::fs::remove_file(&path);
        package(&text?)
    }

    /// Installs the package, lets the plugin set itself up without asking
    /// anything, makes sure a manifest exists, and asks whether it still
    /// needs configuring.
    fn install(&self, source: &Source, package: &Package) -> Result<(String, Option<String>), String> {
        let uv_path = self.find_uv()?;
        let args = install_args(source, package);
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        self.check("uv tool install", self.command(&uv_path, &args), UV_TIMEOUT)?;
        let bin = self.check("uv tool dir", self.command(&uv_path, &["tool", "dir", "--bin"]), QUICK_TIMEOUT)?;
        let command = PathBuf::from(String::from_utf8_lossy(&bin.stdout).trim()).join(&package.script);
        if !command.is_file() {
            return Err(format!("{} did not install {}", package.name, command.display()));
        }
        self.setup(&package.plugin_id, &command)?;
        let mut record = Record::load();
        record.plugins.insert(
            package.plugin_id.clone(),
            Installed { source: source.to_string(), package: package.name.clone(), command: command.clone() },
        );
        record.save()?;
        let configure = (!self.configured(&command)).then(|| package.plugin_id.clone());
        let body = match &configure {
            Some(_) => format!("Installed {} {}; set it up in the terminal", package.name, package.version),
            None => format!("Installed {} {}", package.name, package.version),
        };
        Ok((body, configure))
    }

    /// `install --non-interactive`, then a manifest if the plugin wrote none.
    fn setup(&self, plugin_id: &str, command: &Path) -> Result<(), String> {
        let output = run(self.command(command, &["install", "--non-interactive"]), INSTALL_TIMEOUT)?;
        // argparse ends with 2 on an unknown option: an older plugin whose
        // `install` asks nothing without a terminal anyway.
        let output = if output.status.code() == Some(2) && !output.status.success() {
            run(self.command(command, &["install"]), INSTALL_TIMEOUT)?
        } else {
            output
        };
        if !output.status.success() {
            return Err(format!("{} install failed: {}", command.display(), tail(&output)));
        }
        let manifest = manifest_path(plugin_id);
        if !manifest.exists() {
            if let Some(parent) = manifest.parent() {
                std::fs::create_dir_all(parent).map_err(|error| format!("{}: {error}", parent.display()))?;
            }
            let quoted = toml::Value::String(command.display().to_string());
            std::fs::write(&manifest, format!("command = {quoted}\nargs = [\"serve\"]\n"))
                .map_err(|error| format!("{}: {error}", manifest.display()))?;
        }
        Ok(())
    }

    /// What `status` says about `configured`; a plugin without it, or one
    /// that does not answer, counts as configured.
    fn configured(&self, command: &Path) -> bool {
        let Ok(output) = run(self.command(command, &["status"]), QUICK_TIMEOUT) else { return true };
        configured(&output.stdout)
    }

    fn update(&self, ids: &[String]) -> Result<String, String> {
        let uv_path = self.find_uv()?;
        let record = Record::load();
        let mut updated = Vec::new();
        for plugin_id in ids {
            let installed = record
                .plugins
                .get(plugin_id)
                .ok_or_else(|| format!("{plugin_id} was not installed from a repository"))?;
            self.check(
                &format!("uv tool upgrade {}", installed.package),
                self.command(&uv_path, &["tool", "upgrade", &installed.package]),
                UV_TIMEOUT,
            )?;
            self.setup(plugin_id, &installed.command)?;
            updated.push(installed.package.clone());
        }
        Ok(format!("Updated {}", updated.join(", ")))
    }

    fn remove(&self, plugin_id: &str) -> Result<String, String> {
        let uv_path = self.find_uv()?;
        let mut record = Record::load();
        let installed = record
            .plugins
            .get(plugin_id)
            .cloned()
            .ok_or_else(|| format!("{plugin_id} was not installed from a repository"))?;
        let mut notes = Vec::new();
        match run(self.command(&installed.command, &["uninstall"]), QUICK_TIMEOUT) {
            Ok(output) if output.status.success() => {}
            // argparse ends with 2 on an unknown subcommand: a plugin from
            // before `uninstall`, with nothing of its own to undo here.
            Ok(output) if output.status.code() == Some(2) => notes.push("it has no uninstall step".into()),
            Ok(output) => notes.push(format!("its uninstall failed: {}", tail(&output))),
            Err(error) => notes.push(error),
        }
        self.check(
            &format!("uv tool uninstall {}", installed.package),
            self.command(&uv_path, &["tool", "uninstall", &installed.package]),
            UV_TIMEOUT,
        )?;
        let manifest = manifest_path(plugin_id);
        if manifest.exists() {
            std::fs::remove_file(&manifest).map_err(|error| format!("{}: {error}", manifest.display()))?;
        }
        record.plugins.remove(plugin_id);
        record.save()?;
        let mut body = format!("Removed {}", installed.package);
        if !notes.is_empty() {
            body += &format!("; {}", notes.join("; "));
        }
        Ok(body)
    }
}

/// Reads `status` output: `{"configured": false}` asks for `configure`.
fn configured(stdout: &[u8]) -> bool {
    serde_json::from_slice::<serde_json::Value>(stdout)
        .ok()
        .and_then(|status| status.get("configured").and_then(serde_json::Value::as_bool))
        .unwrap_or(true)
}

impl Tileception {
    pub fn init_installer(&mut self) {
        let (sender, receiver) = channel::channel();
        self.installer.sender = Some(sender);
        self.installer.record = Record::load();
        self.loop_handle
            .insert_source(receiver, |event, _, state| {
                if let channel::Event::Msg(report) = event {
                    state.installer_report(report);
                }
            })
            .expect("plugin installer source");
    }

    /// Starts a job unless one runs already.
    fn start_job(&mut self, busy: String, job: Job) -> Result<(), String> {
        if let Some(running) = &self.installer.busy {
            return Err(format!("Wait for this first: {running}"));
        }
        let sender = self.installer.sender.clone().ok_or("The plugin installer is not running")?;
        let environment = self
            .command("", &[])
            .get_envs()
            .map(|(key, value)| (key.to_os_string(), value.map(std::ffi::OsStr::to_os_string)))
            .collect();
        let runner = Runner { environment };
        if !matches!(job, Job::Look(_)) {
            self.installer.busy = Some(busy.clone());
        }
        std::thread::Builder::new()
            .name("plugin installer".into())
            .spawn(move || {
                let report = match job {
                    Job::Look(source) => {
                        let result = runner.look(&source);
                        Report::Looked { source, result }
                    }
                    Job::Install(source, package) => {
                        let (result, configure) = match runner.install(&source, &package) {
                            Ok((body, configure)) => (Ok(body), configure),
                            Err(error) => (Err(error), None),
                        };
                        Report::Done { title: busy, result, configure }
                    }
                    Job::Add(source) => {
                        let installed = runner.look(&source).and_then(|package| runner.install(&source, &package));
                        let (result, configure) = match installed {
                            Ok((body, configure)) => (Ok(body), configure),
                            Err(error) => (Err(error), None),
                        };
                        Report::Done { title: busy, result, configure }
                    }
                    Job::Update(ids) => Report::Done { title: busy, result: runner.update(&ids), configure: None },
                    Job::Remove(plugin_id) => {
                        Report::Done { title: busy, result: runner.remove(&plugin_id), configure: None }
                    }
                };
                let _ = sender.send(report);
            })
            .map_err(|error| {
                self.installer.busy = None;
                error.to_string()
            })?;
        Ok(())
    }

    /// Reads a repository's `pyproject.toml`, to show before installing.
    pub(crate) fn plugin_look_up(&mut self, source: Source) -> Result<(), String> {
        if self.installer.looking.as_ref() == Some(&source) {
            return Ok(());
        }
        self.installer.looking = Some(source.clone());
        self.start_job(String::new(), Job::Look(source))
    }

    /// Installs what [`Self::plugin_look_up`] found.
    pub(crate) fn plugin_install(&mut self, source: Source, package: Package) -> Result<String, String> {
        let busy = format!("Installing {}", package.name);
        self.start_job(busy.clone(), Job::Install(source, package))?;
        self.notify_locally("Plugins", &format!("{busy}\u{2026}"), tileception_notify::Urgency::Low);
        Ok(format!("{busy}; a notification follows when it is done\n"))
    }

    /// `tileception msg plugins add`: looks the repository up and installs
    /// it in one go, as asking for it on the command line is the consent.
    pub(crate) fn plugin_add(&mut self, text: &str) -> Result<String, String> {
        let source = Source::parse(text)?;
        let busy = format!("Installing {source}");
        self.start_job(busy.clone(), Job::Add(source))?;
        Ok(format!("{busy}; a notification follows when it is done\n"))
    }

    /// Updates the named plugins, or every installed one.
    pub(crate) fn plugin_update(&mut self, plugin_id: Option<&str>) -> Result<String, String> {
        let record = Record::load();
        let ids: Vec<String> = match plugin_id {
            Some(plugin_id) if record.plugins.contains_key(plugin_id) => vec![plugin_id.to_string()],
            Some(plugin_id) => return Err(format!("{plugin_id} was not installed from a repository")),
            None if record.plugins.is_empty() => return Err("No plugins were installed from a repository".into()),
            None => record.plugins.keys().cloned().collect(),
        };
        let busy = format!("Updating {}", ids.join(", "));
        self.start_job(busy.clone(), Job::Update(ids))?;
        Ok(format!("{busy}; a notification follows when it is done\n"))
    }

    pub(crate) fn plugin_remove(&mut self, plugin_id: &str) -> Result<String, String> {
        if !Record::load().plugins.contains_key(plugin_id) {
            return Err(format!("{plugin_id} was not installed from a repository"));
        }
        let busy = format!("Removing {plugin_id}");
        self.start_job(busy.clone(), Job::Remove(plugin_id.to_string()))?;
        Ok(format!("{busy}; a notification follows when it is done\n"))
    }

    /// Runs a plugin's `configure` in a terminal, then reloads plugins.
    pub(crate) fn plugin_configure(&mut self, plugin_id: &str) -> Result<String, String> {
        let record = Record::load();
        let installed =
            record.plugins.get(plugin_id).ok_or_else(|| format!("{plugin_id} was not installed from a repository"))?;
        let tileception =
            std::env::current_exe().map_or_else(|_| "tileception".into(), |path| path.display().to_string());
        let line = format!(
            "{command} configure; status=$?; {tileception} msg plugins reload >/dev/null; echo; \
             if [ $status = 0 ]; then echo 'Done. Press Enter to close.'; \
             else echo \"Setup ended with exit status $status. Press Enter to close.\"; fi; read -r _",
            command = crate::state::shell_word(&installed.command.display().to_string()),
            tileception = crate::state::shell_word(&tileception),
        );
        self.run_in_terminal(&line)?;
        Ok(format!("Configuring {plugin_id} in a terminal\n"))
    }

    fn installer_report(&mut self, report: Report) {
        match report {
            Report::Looked { source, result } => {
                if self.installer.looking.as_ref() == Some(&source) {
                    self.installer.looking = None;
                }
                self.installer.looked = Some((source, result));
            }
            Report::Done { title, result, configure } => {
                self.installer.busy = None;
                self.installer.looked = None;
                self.installer.record = Record::load();
                let _ = self.reload_plugins();
                match result {
                    Ok(body) => self.notify_locally("Plugins", &body, tileception_notify::Urgency::Normal),
                    Err(error) => {
                        tracing::warn!("{title}: {error}");
                        self.notify_locally(&title, &error, tileception_notify::Urgency::Normal)
                    }
                }
                if let Some(plugin_id) = configure
                    && let Err(error) = self.plugin_configure(&plugin_id)
                {
                    self.notify_locally("Plugins", &error, tileception_notify::Urgency::Normal);
                }
            }
        }
        self.installer_changed();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sources() {
        let odoo = Source { owner: "robinvandernoord".into(), repo: "tileception-odoo".into(), rev: None };
        for text in [
            "robinvandernoord/tileception-odoo",
            " robinvandernoord/tileception-odoo ",
            "https://github.com/robinvandernoord/tileception-odoo",
            "https://github.com/robinvandernoord/tileception-odoo/",
            "github.com/robinvandernoord/tileception-odoo.git",
            "git+https://github.com/robinvandernoord/tileception-odoo",
        ] {
            assert_eq!(Source::parse(text), Ok(odoo.clone()), "{text}");
        }
        let tagged = Source::parse("robinvandernoord/tileception-2fas@v0.2.0").expect("a tagged source");
        assert_eq!(tagged.rev.as_deref(), Some("v0.2.0"));
        assert_eq!(tagged.to_string(), "robinvandernoord/tileception-2fas@v0.2.0");
        assert_eq!(tagged.git_url(), "git+https://github.com/robinvandernoord/tileception-2fas@v0.2.0");
        assert_eq!(
            tagged.raw_url("pyproject.toml"),
            "https://raw.githubusercontent.com/robinvandernoord/tileception-2fas/v0.2.0/pyproject.toml"
        );
        assert_eq!(odoo.raw_url("x"), "https://raw.githubusercontent.com/robinvandernoord/tileception-odoo/HEAD/x");
        for text in [
            "",
            "odoo",
            "a/b/c",
            "../b",
            "a/.git",
            "a b/c",
            "a/b@",
            "a/b@../../x",
            "a/b@-x",
            "https://gitlab.com/a/b",
            "a/b@x y",
        ] {
            assert!(Source::parse(text).is_err(), "{text}");
        }
    }

    #[test]
    fn packages() {
        let plain = package(
            r#"
[project]
name = "tileception-odoo"
version = "0.1.0"
description = "Odoo search"
requires-python = ">=3.14"
[project.scripts]
tileception-odoo = "tileception_odoo.cli:main"
"#,
        )
        .expect("a plain pyproject");
        assert_eq!(plain.plugin_id, "odoo");
        assert_eq!(plain.script, "tileception-odoo");
        assert_eq!(plain.python.as_deref(), Some(">=3.14"));
        let source = Source::parse("robinvandernoord/tileception-odoo").expect("a plain source");
        assert_eq!(
            install_args(&source, &plain),
            [
                "tool",
                "install",
                "--python",
                ">=3.14",
                "tileception-odoo @ git+https://github.com/robinvandernoord/tileception-odoo"
            ]
        );

        let declared = package(
            r#"
[project]
name = "tileception-2fas"
[project.scripts]
tileception-2fas = "a:main"
other = "b:main"
[tool.tileception]
id = "totp"
script = "tileception-2fas"
extras = ["security-key"]
needs = ["the 2fas CLI"]
"#,
        )
        .expect("a declared pyproject");
        assert_eq!((declared.plugin_id.as_str(), declared.needs.len()), ("totp", 1));
        assert_eq!(
            requirement(&source, &declared),
            "tileception-2fas[security-key] @ git+https://github.com/robinvandernoord/tileception-odoo"
        );

        // Two scripts and none named: which one serves is unknown.
        assert!(package("[project]\nname = \"x\"\n[project.scripts]\na = \"a:m\"\nb = \"b:m\"\n").is_err());
        assert!(package("[tool.poetry]\nname = \"x\"\n").is_err());
        assert!(
            package("[project]\nname = \"x\"\n[project.scripts]\nx = \"m\"\n[tool.tileception]\nid = \"../x\"\n")
                .is_err()
        );
        assert!(
            package("[project]\nname = \"x\"\n[project.scripts]\nx = \"m\"\n[tool.tileception]\nid = \"plugins\"\n")
                .is_err()
        );
        assert!(
            package(
                "[project]\nname = \"x\"\n[project.scripts]\nx = \"m\"\n[tool.tileception]\nextras = [\"a]; rm\"]\n"
            )
            .is_err()
        );
    }

    #[test]
    fn status() {
        assert!(!configured(br#"{"configured": false, "detail": "no API key"}"#));
        assert!(configured(br#"{"configured": true}"#));
        // Older plugins print other status, or none.
        assert!(configured(br#"{"synced": 3}"#));
        assert!(configured(b"all fine"));
    }

    #[test]
    fn runs_with_a_deadline() {
        let output = {
            let mut command = Command::new("sh");
            command.args(["-c", "echo out; echo err >&2; exit 3"]);
            run(command, QUICK_TIMEOUT)
        }
        .expect("sh runs");
        assert_eq!(output.status.code(), Some(3));
        assert_eq!(tail(&output), "err");
        let mut sleep = Command::new("sleep");
        sleep.arg("5");
        assert!(run(sleep, Duration::from_millis(200)).is_err());
    }

    #[test]
    fn records_round_trip() {
        let mut record = Record::default();
        record.plugins.insert(
            "odoo".into(),
            Installed {
                source: "robinvandernoord/tileception-odoo".into(),
                package: "tileception-odoo".into(),
                command: "/home/me/.local/bin/tileception-odoo".into(),
            },
        );
        let text = toml::to_string(&record).expect("the record serializes");
        assert_eq!(toml::from_str::<Record>(&text).expect("the record reads back"), record);
    }
}
