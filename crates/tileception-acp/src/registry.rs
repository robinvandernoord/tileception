//! The official ACP Registry: which agents exist and how to start them.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime};

use serde::Deserialize;

pub const REGISTRY_URL: &str = "https://cdn.agentclientprotocol.com/registry/v1/latest/registry.json";
/// A cached copy younger than this is used without asking again.
const FRESH: Duration = Duration::from_secs(24 * 60 * 60);

/// An agent listed in the registry.
#[derive(Debug, Clone, PartialEq)]
pub struct Agent {
    pub id: String,
    pub name: String,
    pub version: String,
    pub description: String,
    pub icon: Option<String>,
    pub launch: Launch,
}

/// How an agent is started.
#[derive(Debug, Clone, PartialEq)]
pub enum Launch {
    Npx {
        package: String,
        args: Vec<String>,
        env: BTreeMap<String, String>,
    },
    Uvx {
        package: String,
        args: Vec<String>,
        env: BTreeMap<String, String>,
    },
    /// An archive to download and check, then a program inside it.
    Binary {
        archive: String,
        sha256: String,
        cmd: String,
        args: Vec<String>,
        env: BTreeMap<String, String>,
    },
}

impl Agent {
    /// What is missing to run this agent here, in words for the user.
    pub fn missing(&self) -> Option<String> {
        match &self.launch {
            Launch::Npx { .. } if npx().is_none() => Some("needs Node.js (npx) or Bun (bunx)".into()),
            Launch::Uvx { .. } if !on_path("uvx") => Some("needs uv (uvx)".into()),
            Launch::Binary { archive, .. } => {
                let tools: &[&str] = if archive.ends_with(".zip") { &["curl", "unzip"] } else { &["curl", "tar"] };
                tools.iter().find(|t| !on_path(t)).map(|t| format!("needs {t}"))
            }
            _ => None,
        }
    }

    /// How it is installed, for the picker.
    pub fn kind(&self) -> &'static str {
        match self.launch {
            Launch::Npx { .. } => "npx",
            Launch::Uvx { .. } => "uvx",
            Launch::Binary { .. } => "download",
        }
    }
}

#[derive(Deserialize)]
struct File {
    agents: Vec<Entry>,
}

#[derive(Deserialize)]
struct Entry {
    id: String,
    name: String,
    version: String,
    #[serde(default)]
    description: String,
    icon: Option<String>,
    distribution: Distribution,
}

#[derive(Deserialize)]
struct Distribution {
    npx: Option<Package>,
    uvx: Option<Package>,
    binary: Option<BTreeMap<String, Archive>>,
}

#[derive(Deserialize)]
struct Package {
    package: String,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    env: BTreeMap<String, String>,
}

#[derive(Deserialize)]
struct Archive {
    archive: String,
    cmd: String,
    #[serde(default)]
    args: Vec<String>,
    sha256: Option<String>,
    #[serde(default)]
    env: BTreeMap<String, String>,
}

/// This machine, in the registry's platform names.
pub fn platform() -> String {
    let arch = std::env::consts::ARCH;
    format!("{}-{arch}", std::env::consts::OS)
}

/// Reads the registry JSON. Binary agents without a build for `platform`
/// (or without a checksum) are left out; a downloaded binary must be checked.
pub fn parse(json: &str, platform: &str) -> Result<Vec<Agent>, String> {
    let file: File = serde_json::from_str(json).map_err(|e| format!("the ACP Registry could not be read: {e}"))?;
    let mut agents = Vec::new();
    for e in file.agents {
        let d = e.distribution;
        // Prefer a checked download; then the package managers.
        let binary = d.binary.and_then(|mut b| b.remove(platform)).and_then(|a| {
            Some(Launch::Binary { archive: a.archive, sha256: a.sha256?, cmd: a.cmd, args: a.args, env: a.env })
        });
        let launch = binary
            .or_else(|| d.npx.map(|p| Launch::Npx { package: p.package, args: p.args, env: p.env }))
            .or_else(|| d.uvx.map(|p| Launch::Uvx { package: p.package, args: p.args, env: p.env }));
        if let Some(launch) = launch {
            agents.push(Agent {
                id: e.id,
                name: e.name,
                version: e.version,
                description: e.description,
                icon: e.icon,
                launch,
            });
        }
    }
    agents.sort_by_key(|a| a.name.to_lowercase());
    Ok(agents)
}

fn cache_file() -> PathBuf {
    crate::cache_dir().join("acp-registry.json")
}

/// The registry, from the cache when it is fresh, else downloaded. A stale
/// cache still serves when the download fails (offline).
pub fn load(refresh: bool) -> Result<Vec<Agent>, String> {
    let path = cache_file();
    let fresh = std::fs::metadata(&path)
        .and_then(|m| m.modified())
        .is_ok_and(|t| SystemTime::now().duration_since(t).is_ok_and(|age| age < FRESH));
    if (!fresh || refresh)
        && let Err(error) = download(&path)
    {
        tracing::warn!("ACP Registry: {error}");
        if !path.exists() {
            return Err(error);
        }
    }
    let json = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    parse(&json, &platform())
}

fn download(path: &Path) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    let partial = path.with_extension("part");
    crate::curl(REGISTRY_URL, &partial, "4000000")?;
    std::fs::rename(&partial, path).map_err(|e| e.to_string())
}

/// The runner for npm packages and its arguments before the package: npx,
/// or Bun's bunx when Node.js is not installed. Without Node.js, --bun runs
/// packages whose scripts ask for node under Bun. Bun puts bunx in
/// ~/.bun/bin, which a login session's PATH often leaves out.
pub fn npx() -> Option<(PathBuf, &'static [&'static str])> {
    if on_path("npx") {
        return Some(("npx".into(), &["-y"]));
    }
    if on_path("bunx") {
        return Some(("bunx".into(), &["--bun"]));
    }
    let bun = std::env::var_os("BUN_INSTALL")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| Path::new(&home).join(".bun")))?;
    let bunx = bun.join("bin").join("bunx");
    bunx.is_file().then_some((bunx, &["--bun"]))
}

pub fn on_path(program: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|paths| std::env::split_paths(&paths).any(|dir| dir.join(program).is_file()))
}

/// Runs a program quietly, for tools used while installing.
pub(crate) fn run(program: &str, args: &[&std::ffi::OsStr]) -> Result<(), String> {
    let status = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| format!("{program}: {e}"))?;
    if status.status.success() {
        Ok(())
    } else {
        let err = String::from_utf8_lossy(&status.stderr);
        Err(format!("{program} failed: {}", err.lines().last().unwrap_or("").trim()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"{"version":"1","agents":[
      {"id":"claude-acp","name":"Claude Agent","version":"0.92.0","description":"ACP wrapper",
       "distribution":{"npx":{"package":"@agentclientprotocol/claude-agent-acp@0.92.0"}}},
      {"id":"goose","name":"goose","version":"1.54.0","description":"",
       "distribution":{"binary":{"linux-x86_64":{"archive":"https://x/goose.tar.bz2","cmd":"./goose","args":["acp"],"sha256":"ab"}}}},
      {"id":"mac-only","name":"Mac only","version":"1","description":"",
       "distribution":{"binary":{"darwin-aarch64":{"archive":"https://x/a.zip","cmd":"./a","sha256":"cd"}}}},
      {"id":"fast-agent","name":"fast-agent","version":"0.10.1","description":"",
       "distribution":{"uvx":{"package":"fast-agent-acp==0.10.1","args":["-x"],"env":{"FAST_AGENT_MODEL":"codexplan"}}}}
    ],"extensions":[]}"#;

    #[test]
    fn reads_the_registry_for_this_platform() {
        let agents = parse(SAMPLE, "linux-x86_64").expect("parses");
        let ids: Vec<_> = agents.iter().map(|a| a.id.as_str()).collect();
        assert_eq!(ids, ["claude-acp", "fast-agent", "goose"]);
        assert!(matches!(&agents[2].launch, Launch::Binary { cmd, args, .. } if cmd == "./goose" && args == &["acp"]));
        assert!(matches!(&agents[1].launch, Launch::Uvx { env, .. } if env["FAST_AGENT_MODEL"] == "codexplan"));
        // No build for this machine: not offered.
        assert!(parse(SAMPLE, "linux-aarch64").expect("parses").iter().all(|a| a.id != "goose"));
        assert!(parse("{", "linux-x86_64").is_err());
    }
}
