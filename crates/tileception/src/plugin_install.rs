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
/// How long output is still read after a program ended, while something it
/// left running holds its pipes open.
const OUTPUT_GRACE: Duration = Duration::from_secs(2);
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

    /// The URL of a file in the repository at a commit.
    fn raw_url(&self, commit: &str, file: &str) -> String {
        format!("https://raw.githubusercontent.com/{}/{}/{commit}/{file}", self.owner, self.repo)
    }

    /// The repository as git takes it.
    fn clone_url(&self) -> String {
        format!("https://github.com/{}/{}", self.owner, self.repo)
    }

    /// The repository at a commit, as pip and uv take it.
    fn git_url(&self, commit: &str) -> String {
        format!("git+{}@{commit}", self.clone_url())
    }

    /// `owner/repo`, without the revision.
    fn repository(&self) -> String {
        format!("{}/{}", self.owner, self.repo)
    }

    /// Whether both name the same repository, whatever the revision: GitHub
    /// names do not depend on case.
    fn same_repository(&self, other: &Self) -> bool {
        self.owner.eq_ignore_ascii_case(&other.owner) && self.repo.eq_ignore_ascii_case(&other.repo)
    }
}

/// Whether `text` is a full commit hash.
fn is_commit(text: &str) -> bool {
    text.len() == 40 && text.chars().all(|letter| letter.is_ascii_hexdigit())
}

/// The first seven characters of a commit, as GitHub shows it.
pub fn short(commit: &str) -> &str {
    commit.get(..7).unwrap_or(commit)
}

/// The refs `git ls-remote` is asked for to find `rev`: the default branch
/// without one, else a tag (and the commit an annotated tag points at) or a
/// branch of that exact name.
fn ls_remote_patterns(rev: Option<&str>) -> Vec<String> {
    match rev {
        None => vec!["HEAD".into()],
        Some(rev) => vec![format!("refs/tags/{rev}"), format!("refs/tags/{rev}^{{}}"), format!("refs/heads/{rev}")],
    }
}

/// The commit `git ls-remote` says `rev` is at, from its output. A tag wins
/// over a branch of the same name, as it does for git, and an annotated tag
/// gives the commit it points at rather than the tag object.
fn pick_commit(listing: &str, rev: Option<&str>) -> Option<String> {
    let refs: Vec<(&str, &str)> = listing
        .lines()
        .filter_map(|line| line.split_once('\t'))
        .map(|(commit, name)| (commit.trim(), name.trim()))
        .filter(|(commit, _)| is_commit(commit))
        .collect();
    let wanted = match rev {
        None => vec!["HEAD".to_string()],
        Some(rev) => vec![format!("refs/tags/{rev}^{{}}"), format!("refs/tags/{rev}"), format!("refs/heads/{rev}")],
    };
    wanted
        .iter()
        .find_map(|wanted| refs.iter().find(|(_, name)| name == wanted).map(|(commit, _)| commit.to_ascii_lowercase()))
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

/// A package as looked up: what its `pyproject.toml` says at the commit it
/// was read from. That commit is what installs, so what is installed is what
/// was shown, whatever the repository does in the meantime.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lookup {
    pub package: Package,
    /// The full commit hash.
    pub commit: String,
}

/// The requirement `uv tool install` gets, extras and all.
fn requirement(source: &Source, lookup: &Lookup) -> String {
    let package = &lookup.package;
    let extras = if package.extras.is_empty() { String::new() } else { format!("[{}]", package.extras.join(",")) };
    format!("{}{extras} @ {}", package.name, source.git_url(&lookup.commit))
}

fn install_args(source: &Source, lookup: &Lookup) -> Vec<String> {
    let mut args = vec!["tool".to_string(), "install".to_string()];
    if let Some(python) = &lookup.package.python {
        args.extend(["--python".to_string(), python.trim().to_string()]);
    }
    args.push(requirement(source, lookup));
    args
}

/// A plugin installed from a repository.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Installed {
    /// `owner/repo`, with `@rev` when one was asked for: what `update`
    /// looks up again.
    pub source: String,
    /// The commit installed. Only `update` moves it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
    /// The Python package, for `uv tool uninstall`.
    pub package: String,
    /// The script's absolute path.
    pub command: PathBuf,
    /// Whether the manifest was made by installing (by Tileception or the
    /// plugin's `install`), so removing may delete it. A manifest that was
    /// there before stays.
    #[serde(default)]
    pub wrote_manifest: bool,
}

/// Every installed plugin, by ID.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Record {
    #[serde(default)]
    pub plugins: BTreeMap<String, Installed>,
}

/// Package names as Python compares them.
fn same_package(one: &str, other: &str) -> bool {
    let normal = |name: &str| name.to_lowercase().replace(['_', '.'], "-");
    normal(one) == normal(other)
}

/// Refuses an install that would take over another plugin: one with the same
/// ID from another repository, whose record it would overwrite, or one with
/// the same package under another ID, whose uv tool it would replace.
/// Installing the same repository again, at any revision, is fine.
fn conflict(record: &Record, source: &Source, package: &Package) -> Result<(), String> {
    if let Some(installed) = record.plugins.get(&package.plugin_id) {
        let same = Source::parse(&installed.source).is_ok_and(|other| other.same_repository(source));
        if !same {
            let from = Source::parse(&installed.source).map_or(installed.source.clone(), |other| other.repository());
            return Err(format!("plugin ID {} is taken by {from}; remove it first", package.plugin_id));
        }
    }
    if let Some((plugin_id, installed)) = record.plugins.iter().find(|(plugin_id, installed)| {
        **plugin_id != package.plugin_id && same_package(&installed.package, &package.name)
    }) {
        return Err(format!("{} is already installed as the plugin {plugin_id}; remove it first", installed.package));
    }
    Ok(())
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

/// The manifest Tileception writes when a plugin's `install` wrote none.
fn minimal_manifest(command: &Path) -> String {
    let quoted = toml::Value::String(command.display().to_string());
    format!("command = {quoted}\nargs = [\"serve\"]\n")
}

/// Deletes a removed plugin's manifest if installing made it, and says so
/// when it stays. Older records do not say who made it; the manifest
/// Tileception writes itself is recognised by its text.
fn remove_manifest(installed: &Installed, manifest: &Path) -> Result<Option<String>, String> {
    if !manifest.exists() {
        return Ok(None);
    }
    let ours = installed.wrote_manifest
        || std::fs::read_to_string(manifest).is_ok_and(|text| text == minimal_manifest(&installed.command));
    if !ours {
        return Ok(Some(format!("kept {}, which was there before it was installed", manifest.display())));
    }
    std::fs::remove_file(manifest).map_err(|error| format!("{}: {error}", manifest.display()))?;
    Ok(None)
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
    Install(Source, Box<Lookup>),
    Update(Vec<String>),
    Remove(String),
}

/// What a job reports back.
pub enum Report {
    Looked { source: Source, result: Box<Result<Lookup, String>> },
    Done { title: String, result: Result<String, String>, configure: Option<String> },
}

/// The launcher's view of installing.
#[derive(Default)]
pub struct Installer {
    sender: Option<channel::Sender<Report>>,
    /// What runs now, in words, such as "Installing tileception-odoo".
    pub busy: Option<String>,
    /// The last repository looked up, and what it said.
    pub looked: Option<(Source, Result<Lookup, String>)>,
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
        .map(|(plugin_id, installed)| {
            let commit = installed.commit.as_deref().map_or("-", short);
            format!("{plugin_id}\t{}\t{commit}\t{}\n", installed.source, installed.command.display())
        })
        .collect()
}

/// A file of its own for each lookup: two can run at once, and one must
/// not read what the other fetched.
fn lookup_file() -> String {
    static LOOKUPS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let lookup = LOOKUPS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    format!("plugin-pyproject-{}-{lookup}.toml", std::process::id())
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
    // Output is gathered as it comes, and each reader says when its pipe
    // closed. Something the program started in the background may hold the
    // pipes open long after it ended, so they are not waited for forever.
    let (closed, pipes_closed) = std::sync::mpsc::channel::<()>();
    let gather = |pipe: Option<Box<dyn Read + Send>>| {
        let bytes = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let gathered = bytes.clone();
        let closed = closed.clone();
        std::thread::spawn(move || {
            if let Some(mut pipe) = pipe {
                let mut chunk = [0; 8192];
                while let Ok(length @ 1..) = pipe.read(&mut chunk) {
                    gathered
                        .lock()
                        .expect("output buffer mutex must not be poisoned")
                        .extend_from_slice(&chunk[..length]);
                }
            }
            let _ = closed.send(());
        });
        bytes
    };
    let stdout = gather(child.stdout.take().map(|pipe| Box::new(pipe) as Box<dyn Read + Send>));
    let stderr = gather(child.stderr.take().map(|pipe| Box::new(pipe) as Box<dyn Read + Send>));
    drop(closed);
    let take = |bytes: std::sync::Arc<std::sync::Mutex<Vec<u8>>>| {
        std::mem::take(&mut *bytes.lock().expect("output buffer mutex must not be poisoned"))
    };
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
    let grace = Instant::now() + OUTPUT_GRACE;
    for _ in 0..2 {
        if pipes_closed.recv_timeout(grace.saturating_duration_since(Instant::now())).is_err() {
            break;
        }
    }
    Ok(Output { status, stdout: take(stdout), stderr: take(stderr) })
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

    /// The commit `source` is at now: its default branch, or the tag,
    /// branch or full commit it names.
    fn resolve(&self, source: &Source) -> Result<String, String> {
        if let Some(rev) = source.rev.as_deref().filter(|rev| is_commit(rev)) {
            return Ok(rev.to_ascii_lowercase());
        }
        let patterns = ls_remote_patterns(source.rev.as_deref());
        let url = source.clone_url();
        let mut args = vec!["ls-remote", "--", url.as_str()];
        args.extend(patterns.iter().map(String::as_str));
        let mut command = self.command("git", &args);
        // A repository that does not exist, or is private, asks for a login.
        command.env("GIT_TERMINAL_PROMPT", "0");
        let output = run(command, QUICK_TIMEOUT)
            .map_err(|error| format!("Could not ask GitHub about {} ({error})", source.repository()))?;
        if !output.status.success() {
            // GitHub asks for a login rather than say a repository is missing.
            let error = tail(&output);
            return Err(if error.contains("could not read Username") {
                format!("{} is not a public GitHub repository", source.repository())
            } else {
                format!("Could not ask GitHub about {}: {error}", source.repository())
            });
        }
        pick_commit(&String::from_utf8_lossy(&output.stdout), source.rev.as_deref()).ok_or_else(|| match &source.rev {
            Some(rev) => format!(
                "{} has no tag or branch {rev}; name a tag, a branch or a full commit hash",
                source.repository()
            ),
            None => format!("{} has no default branch", source.repository()),
        })
    }

    /// Finds the commit to install and reads its `pyproject.toml`, refusing
    /// a plugin that would take over another one.
    fn look(&self, source: &Source) -> Result<Lookup, String> {
        let lookup = self.read(source)?;
        conflict(&Record::load(), source, &lookup.package)?;
        Ok(lookup)
    }

    /// Finds the commit to install and reads its `pyproject.toml`.
    fn read(&self, source: &Source) -> Result<Lookup, String> {
        let commit = self.resolve(source)?;
        let cache = std::env::var_os("XDG_CACHE_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".cache"))
            .join("tileception");
        std::fs::create_dir_all(&cache).map_err(|error| format!("{}: {error}", cache.display()))?;
        let path = cache.join(lookup_file());
        tileception_acp::curl(&source.raw_url(&commit, "pyproject.toml"), &path, MAX_PYPROJECT).map_err(|error| {
            format!("{} has no pyproject.toml to read at {} ({error})", source.repository(), short(&commit))
        })?;
        let text = std::fs::read_to_string(&path).map_err(|error| error.to_string());
        let _ = std::fs::remove_file(&path);
        Ok(Lookup { package: package(&text?)?, commit })
    }

    /// Installs the package at the commit looked up, lets the plugin set
    /// itself up without asking anything, makes sure a manifest exists, and
    /// asks whether it still needs configuring.
    fn install(&self, source: &Source, lookup: &Lookup) -> Result<(String, Option<String>), String> {
        let package = &lookup.package;
        // Again, as something else may have been installed since the lookup.
        conflict(&Record::load(), source, package)?;
        let uv_path = self.find_uv()?;
        let args = install_args(source, lookup);
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        self.check("uv tool install", self.command(&uv_path, &args), UV_TIMEOUT)?;
        let bin = self.check("uv tool dir", self.command(&uv_path, &["tool", "dir", "--bin"]), QUICK_TIMEOUT)?;
        let command = PathBuf::from(String::from_utf8_lossy(&bin.stdout).trim()).join(&package.script);
        if !command.is_file() {
            return Err(format!("{} did not install {}", package.name, command.display()));
        }
        let manifest = manifest_path(&package.plugin_id);
        let had_manifest = manifest.exists();
        self.setup(&package.plugin_id, &command)?;
        let mut record = Record::load();
        let wrote_before = record.plugins.get(&package.plugin_id).is_some_and(|installed| installed.wrote_manifest);
        record.plugins.insert(
            package.plugin_id.clone(),
            Installed {
                source: source.to_string(),
                commit: Some(lookup.commit.clone()),
                package: package.name.clone(),
                command: command.clone(),
                wrote_manifest: wrote_before || (!had_manifest && manifest.exists()),
            },
        );
        record.save()?;
        let configure = (!self.configured(&command)).then(|| package.plugin_id.clone());
        let version = if package.version.is_empty() { String::new() } else { format!(" {}", package.version) };
        let installed = format!("Installed {}{version} at {}", package.name, short(&lookup.commit));
        let body = match &configure {
            Some(_) => format!("{installed}; set it up in the terminal"),
            None => installed,
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
            std::fs::write(&manifest, minimal_manifest(command))
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

    /// Moves one plugin to the commit its source is at now: the only step
    /// that installs something other than what was installed before.
    fn update_one(&self, plugin_id: &str) -> Result<Updated, String> {
        let record = Record::load();
        let installed =
            record.plugins.get(plugin_id).ok_or_else(|| format!("{plugin_id} was not installed from a repository"))?;
        let source = Source::parse(&installed.source)?;
        let lookup = self.read(&source)?;
        if lookup.package.plugin_id != plugin_id {
            return Err(format!(
                "{} now installs the plugin {}; remove {plugin_id} and add it again",
                source.repository(),
                lookup.package.plugin_id
            ));
        }
        if installed.commit.as_deref() == Some(lookup.commit.as_str()) {
            return Ok(Updated::Current);
        }
        let from = installed.commit.clone();
        self.install(&source, &lookup)?;
        Ok(Updated::Moved { from, to: lookup.commit })
    }

    /// Updates each plugin in turn, whatever happens to the others.
    fn update(&self, ids: &[String]) -> Vec<(String, Result<Updated, String>)> {
        ids.iter().map(|plugin_id| (plugin_id.clone(), self.update_one(plugin_id))).collect()
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
        if let Some(note) = remove_manifest(&installed, &manifest_path(plugin_id))? {
            notes.push(note);
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

/// What updating one plugin did.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Updated {
    /// Installed a newer commit.
    Moved { from: Option<String>, to: String },
    /// Its source is still at the commit installed.
    Current,
}

/// The notification for updating plugins: a title, then which failed, which
/// were updated and which were up to date, then a line for each failure and
/// each update. Ok when every one succeeded.
fn update_summary(results: &[(String, Result<Updated, String>)]) -> (String, Result<String, String>) {
    let names = |keep: fn(&Result<Updated, String>) -> bool| -> String {
        let names: Vec<&str> =
            results.iter().filter(|(_, result)| keep(result)).map(|(plugin_id, _)| plugin_id.as_str()).collect();
        names.join(", ")
    };
    let failed = names(Result::is_err);
    let moved = names(|result| matches!(result, Ok(Updated::Moved { .. })));
    let current = names(|result| matches!(result, Ok(Updated::Current)));
    let mut lines = Vec::new();
    for (label, names) in [("Failed", &failed), ("Updated", &moved), ("Up to date", &current)] {
        if !names.is_empty() {
            lines.push(format!("{label}: {names}"));
        }
    }
    for (plugin_id, result) in results {
        match result {
            Err(error) => lines.push(format!("{plugin_id}: {error}")),
            Ok(Updated::Moved { from, to }) => {
                let from = from.as_deref().map_or_else(String::new, |from| format!("{} ", short(from)));
                lines.push(format!("{plugin_id}: {from}\u{2192} {}", short(to)));
            }
            Ok(Updated::Current) => {}
        }
    }
    let body = lines.join("\n");
    let failures = results.iter().filter(|(_, result)| result.is_err()).count();
    match failures {
        0 => ("Plugins".into(), Ok(body)),
        _ if results.len() == 1 => (format!("Could not update {}", results[0].0), Err(body)),
        _ => (format!("Could not update {failures} of {} plugins", results.len()), Err(body)),
    }
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
                        let result = Box::new(runner.look(&source));
                        Report::Looked { source, result }
                    }
                    Job::Install(source, lookup) => {
                        let (result, configure) = match runner.install(&source, &lookup) {
                            Ok((body, configure)) => (Ok(body), configure),
                            Err(error) => (Err(error), None),
                        };
                        Report::Done { title: busy, result, configure }
                    }
                    Job::Add(source) => {
                        let installed = runner.look(&source).and_then(|lookup| runner.install(&source, &lookup));
                        let (result, configure) = match installed {
                            Ok((body, configure)) => (Ok(body), configure),
                            Err(error) => (Err(error), None),
                        };
                        Report::Done { title: busy, result, configure }
                    }
                    Job::Update(ids) => {
                        let results = runner.update(&ids);
                        for (plugin_id, result) in &results {
                            match result {
                                Ok(updated) => tracing::info!("updating {plugin_id}: {updated:?}"),
                                Err(error) => tracing::warn!("updating {plugin_id}: {error}"),
                            }
                        }
                        let (title, result) = update_summary(&results);
                        Report::Done { title, result, configure: None }
                    }
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
        // Only once it runs: a lookup refused now must not look started,
        // or Enter would wait for it for good.
        self.start_job(String::new(), Job::Look(source.clone()))?;
        self.installer.looking = Some(source);
        Ok(())
    }

    /// Installs what [`Self::plugin_look_up`] found.
    pub(crate) fn plugin_install(&mut self, source: Source, lookup: Lookup) -> Result<String, String> {
        let busy = format!("Installing {} at {}", lookup.package.name, short(&lookup.commit));
        self.start_job(busy.clone(), Job::Install(source, Box::new(lookup)))?;
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
                self.installer.looked = Some((source, *result));
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
#[allow(clippy::unwrap_used, reason = "test setup and assertions should fail immediately on unexpected errors")]
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
        let commit = "fb54a60c53c20be83553d78bedd8093f44332e0d";
        assert_eq!(
            tagged.git_url(commit),
            "git+https://github.com/robinvandernoord/tileception-2fas@fb54a60c53c20be83553d78bedd8093f44332e0d"
        );
        assert_eq!(
            tagged.raw_url(commit, "pyproject.toml"),
            "https://raw.githubusercontent.com/robinvandernoord/tileception-2fas/fb54a60c53c20be83553d78bedd8093f44332e0d/pyproject.toml"
        );
        assert!(tagged.same_repository(&Source::parse("RobinVanDerNoord/Tileception-2fas").expect("a source")));
        assert!(!tagged.same_repository(&Source::parse("someone/tileception-2fas").expect("a source")));
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
        let commit = "0123456789abcdef0123456789abcdef01234567".to_string();
        let lookup = Lookup { package: plain, commit: commit.clone() };
        // What installs is the commit that was looked up, not whatever the
        // default branch is at by then.
        assert_eq!(
            install_args(&source, &lookup),
            [
                "tool",
                "install",
                "--python",
                ">=3.14",
                "tileception-odoo @ git+https://github.com/robinvandernoord/tileception-odoo@0123456789abcdef0123456789abcdef01234567"
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
            requirement(&source, &Lookup { package: declared, commit }),
            "tileception-2fas[security-key] @ git+https://github.com/robinvandernoord/tileception-odoo@0123456789abcdef0123456789abcdef01234567"
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
    fn a_program_that_leaves_something_running_still_returns() {
        // An `install` that starts a background process, which keeps its
        // output open after the plugin's own process has ended.
        let mut command = Command::new("sh");
        command.args(["-c", "sleep 30 & echo started"]);
        let started = Instant::now();
        let output = run(command, Duration::from_secs(2)).expect("sh runs");
        assert!(started.elapsed() < Duration::from_secs(10), "took {:?}", started.elapsed());
        assert!(output.status.success());
        assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "started");
    }

    #[test]
    fn lookups_do_not_share_a_file() {
        assert_ne!(lookup_file(), lookup_file());
    }

    fn installed(source: &str, package: &str) -> Installed {
        Installed {
            source: source.into(),
            commit: Some("0123456789abcdef0123456789abcdef01234567".into()),
            package: package.into(),
            command: format!("/home/me/.local/bin/{package}").into(),
            wrote_manifest: true,
        }
    }

    #[test]
    fn records_round_trip() {
        let mut record = Record::default();
        record.plugins.insert("odoo".into(), installed("robinvandernoord/tileception-odoo", "tileception-odoo"));
        let text = toml::to_string(&record).expect("the record serializes");
        assert_eq!(toml::from_str::<Record>(&text).expect("the record reads back"), record);
        // A record from before commits and manifests were kept still reads.
        let old = toml::from_str::<Record>("[plugins.odoo]\nsource = \"a/b\"\npackage = \"b\"\ncommand = \"/bin/b\"\n")
            .expect("an older record reads");
        assert_eq!((old.plugins["odoo"].commit.as_deref(), old.plugins["odoo"].wrote_manifest), (None, false));
    }

    #[test]
    fn commits_from_ls_remote() {
        let head = "fb54a60c53c20be83553d78bedd8093f44332e0d\tHEAD\n";
        assert_eq!(pick_commit(head, None).as_deref(), Some("fb54a60c53c20be83553d78bedd8093f44332e0d"));
        assert_eq!(ls_remote_patterns(None), ["HEAD"]);
        assert_eq!(ls_remote_patterns(Some("v1")), ["refs/tags/v1", "refs/tags/v1^{}", "refs/heads/v1"]);
        // An annotated tag: the commit it points at, not the tag object; and a
        // tag before a branch of the same name.
        let listing = "\
1111111111111111111111111111111111111111\trefs/heads/v1
2222222222222222222222222222222222222222\trefs/tags/v1
3333333333333333333333333333333333333333\trefs/tags/v1^{}
";
        assert_eq!(pick_commit(listing, Some("v1")).as_deref(), Some("3333333333333333333333333333333333333333"));
        let lightweight = "2222222222222222222222222222222222222222\trefs/tags/v1\n";
        assert_eq!(pick_commit(lightweight, Some("v1")).as_deref(), Some("2222222222222222222222222222222222222222"));
        let branch = "1111111111111111111111111111111111111111\trefs/heads/main\n";
        assert_eq!(pick_commit(branch, Some("main")).as_deref(), Some("1111111111111111111111111111111111111111"));
        // Only exact names count, and only real hashes.
        assert_eq!(pick_commit("1111111111111111111111111111111111111111\trefs/heads/x/v1\n", Some("v1")), None);
        assert_eq!(pick_commit("nonsense\tHEAD\n", None), None);
        assert_eq!(pick_commit("", None), None);
        assert!(is_commit("0123456789abcdef0123456789ABCDEF01234567"));
        assert!(!is_commit("0123456"));
        assert_eq!(short("fb54a60c53c20be83553d78bedd8093f44332e0d"), "fb54a60");
    }

    #[test]
    fn another_repository_cannot_take_an_installed_id() {
        let mut record = Record::default();
        record.plugins.insert("2fas".into(), installed("robinvandernoord/tileception-2fas", "tileception-2fas"));
        let pyproject = |name: &str| {
            package(&format!("[project]\nname = \"{name}\"\n[project.scripts]\n{name} = \"m:main\"\n"))
                .expect("a pyproject")
        };
        let same = pyproject("tileception-2fas");
        // Installing or updating the same repository, at any revision.
        for text in ["robinvandernoord/tileception-2fas", "RobinVanDerNoord/tileception-2fas@v0.3.0"] {
            assert_eq!(conflict(&record, &Source::parse(text).expect("a source"), &same), Ok(()), "{text}");
        }
        let fork = Source::parse("someone/tileception-2fas").expect("a source");
        assert_eq!(
            conflict(&record, &fork, &same),
            Err("plugin ID 2fas is taken by robinvandernoord/tileception-2fas; remove it first".into())
        );
        // The same package under another ID would replace its uv tool.
        let renamed = package(
            "[project]\nname = \"tileception_2fas\"\n[project.scripts]\nx = \"m:main\"\n[tool.tileception]\nid = \"totp\"\n",
        )
        .expect("a pyproject");
        assert!(conflict(&record, &fork, &renamed).is_err());
        // Something else entirely is fine.
        assert_eq!(conflict(&record, &fork, &pyproject("tileception-odoo")), Ok(()));
    }

    /// A directory of its own under the system's temporary directory.
    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("tileception-plugin-install-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("a scratch directory");
        dir
    }

    #[test]
    fn removing_deletes_only_a_manifest_installing_made() {
        let dir = scratch("manifests");
        let manifest = dir.join("2fas.toml");
        let mut plugin = installed("robinvandernoord/tileception-2fas", "tileception-2fas");

        // Written by hand before installing: it stays, and the user is told.
        plugin.wrote_manifest = false;
        std::fs::write(&manifest, "command = \"/opt/my-2fas\"\nargs = [\"serve\", \"--verbose\"]\n").unwrap();
        let note = remove_manifest(&plugin, &manifest).unwrap();
        assert!(manifest.exists());
        assert!(note.is_some_and(|note| note.contains("kept")));

        // Made by installing: it goes.
        plugin.wrote_manifest = true;
        assert_eq!(remove_manifest(&plugin, &manifest), Ok(None));
        assert!(!manifest.exists());

        // An older record that does not say, with the manifest Tileception
        // writes itself: that is recognised.
        plugin.wrote_manifest = false;
        std::fs::write(&manifest, minimal_manifest(&plugin.command)).unwrap();
        assert_eq!(remove_manifest(&plugin, &manifest), Ok(None));
        assert!(!manifest.exists());

        // Already removed by the plugin's own uninstall.
        assert_eq!(remove_manifest(&plugin, &manifest), Ok(None));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn updating_reports_every_plugin() {
        let commit = |digit: char| std::iter::repeat_n(digit, 40).collect::<String>();
        let results = vec![
            ("2fas".to_string(), Ok(Updated::Moved { from: Some(commit('0')), to: commit('f') })),
            ("github".to_string(), Err("uv tool install failed: no network".to_string())),
            ("odoo".to_string(), Ok(Updated::Current)),
            ("weather".to_string(), Err("weather was not installed from a repository".to_string())),
        ];
        let (title, result) = update_summary(&results);
        assert_eq!(title, "Could not update 2 of 4 plugins");
        assert_eq!(
            result,
            Err("Failed: github, weather\n\
                 Updated: 2fas\n\
                 Up to date: odoo\n\
                 2fas: 0000000 \u{2192} fffffff\n\
                 github: uv tool install failed: no network\n\
                 weather: weather was not installed from a repository"
                .into())
        );
        let (title, result) = update_summary(&results[..1]);
        assert_eq!((title.as_str(), result), ("Plugins", Ok("Updated: 2fas\n2fas: 0000000 \u{2192} fffffff".into())));
        let (title, _) = update_summary(&results[1..2]);
        assert_eq!(title, "Could not update github");
    }
}
