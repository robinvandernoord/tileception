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
    pub agent_id: String,
    pub name: String,
    pub version: String,
    pub description: String,
    pub icon: Option<String>,
    pub launch: Launch,
    /// A copy already on this machine, used in place of `launch`'s download
    /// or package runner. Filled in by [`load`].
    pub installed: Option<crate::local::Installed>,
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
        if self.installed.is_some() {
            return None;
        }
        match &self.launch {
            Launch::Npx { .. } if npx().is_none() => Some("needs Node.js (npx) or Bun (bunx)".into()),
            Launch::Uvx { .. } if crate::local::find("uvx").is_none() => Some("needs uv (uvx)".into()),
            Launch::Binary { archive, .. } => {
                let tools: &[&str] = if archive.ends_with(".zip") { &["curl", "unzip"] } else { &["curl", "tar"] };
                tools.iter().find(|tool| crate::local::find(tool).is_none()).map(|tool| format!("needs {tool}"))
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
    #[serde(rename = "id")]
    agent_id: String,
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
    let file: File =
        serde_json::from_str(json).map_err(|error| format!("the ACP Registry could not be read: {error}"))?;
    let mut agents = Vec::new();
    for entry in file.agents {
        let distribution = entry.distribution;
        // Prefer a checked download; then the package managers.
        let binary = distribution.binary.and_then(|mut builds| builds.remove(platform)).and_then(|archive| {
            Some(Launch::Binary {
                archive: archive.archive,
                sha256: archive.sha256?,
                cmd: archive.cmd,
                args: archive.args,
                env: archive.env,
            })
        });
        let launch = binary
            .or_else(|| {
                distribution.npx.map(|package| Launch::Npx {
                    package: package.package,
                    args: package.args,
                    env: package.env,
                })
            })
            .or_else(|| {
                distribution.uvx.map(|package| Launch::Uvx {
                    package: package.package,
                    args: package.args,
                    env: package.env,
                })
            });
        if let Some(launch) = launch {
            agents.push(Agent {
                agent_id: entry.agent_id,
                name: entry.name,
                version: entry.version,
                description: entry.description,
                icon: entry.icon,
                launch,
                installed: None,
            });
        }
    }
    agents.sort_by_key(|agent| agent.name.to_lowercase());
    Ok(agents)
}

fn cache_file() -> PathBuf {
    crate::cache_dir().join("acp-registry.json")
}

/// The registry, from the cache when it is fresh, else downloaded, with the
/// copies of its agents already installed here. A stale cache still serves
/// when the download fails (offline). Blocks: call it off the main thread.
pub fn load(refresh: bool) -> Result<Vec<Agent>, String> {
    // Reads the login shell's PATH once, before anything looks for programs.
    crate::local::search_path();
    let path = cache_file();
    let fresh = std::fs::metadata(&path)
        .and_then(|metadata| metadata.modified())
        .is_ok_and(|modified| SystemTime::now().duration_since(modified).is_ok_and(|age| age < FRESH));
    if (!fresh || refresh)
        && let Err(error) = download(&path)
    {
        tracing::warn!("ACP Registry: {error}");
        if !path.exists() {
            return Err(error);
        }
    }
    let json = std::fs::read_to_string(&path).map_err(|error| format!("{}: {error}", path.display()))?;
    let mut agents = parse(&json, &platform())?;
    for agent in &mut agents {
        agent.installed = crate::local::installed(&agent.launch);
    }
    Ok(agents)
}

fn download(path: &Path) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|error| format!("{}: {error}", dir.display()))?;
    }
    let partial = path.with_extension("part");
    crate::curl(REGISTRY_URL, &partial, "4000000")?;
    std::fs::rename(&partial, path).map_err(|error| error.to_string())
}

/// The runner for npm packages and its arguments before the package. Bun's
/// bunx when it is installed, since it fetches and caches faster; packages
/// still run under Node.js when that is installed too, as their scripts ask.
/// Without Node.js, --bun runs them under Bun. npx otherwise.
pub fn npx() -> Option<(PathBuf, &'static [&'static str])> {
    runner(crate::local::find("bunx"), crate::local::find("node").is_some(), crate::local::find("npx"))
}

fn runner(bunx: Option<PathBuf>, node: bool, npx: Option<PathBuf>) -> Option<(PathBuf, &'static [&'static str])> {
    match (bunx, npx) {
        (Some(bunx), _) if node => Some((bunx, &[])),
        (Some(bunx), _) => Some((bunx, &["--bun"])),
        (None, Some(npx)) => Some((npx, &["-y"])),
        (None, None) => None,
    }
}

/// Whether the program is on the compositor's own PATH.
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
        .map_err(|error| format!("{program}: {error}"))?;
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
        let ids: Vec<_> = agents.iter().map(|agent| agent.agent_id.as_str()).collect();
        assert_eq!(ids, ["claude-acp", "fast-agent", "goose"]);
        assert!(matches!(&agents[2].launch, Launch::Binary { cmd, args, .. } if cmd == "./goose" && args == &["acp"]));
        assert!(matches!(&agents[1].launch, Launch::Uvx { env, .. } if env["FAST_AGENT_MODEL"] == "codexplan"));
        // No build for this machine: not offered.
        assert!(parse(SAMPLE, "linux-aarch64").expect("parses").iter().all(|agent| agent.agent_id != "goose"));
        assert!(parse("{", "linux-x86_64").is_err());
    }

    #[test]
    fn prefers_bunx_and_node() {
        let (bunx, npx) = (Some(PathBuf::from("/b/bunx")), Some(PathBuf::from("/n/npx")));
        assert_eq!(runner(bunx.clone(), true, npx.clone()), Some((PathBuf::from("/b/bunx"), &[][..])));
        assert_eq!(runner(bunx, false, npx.clone()), Some((PathBuf::from("/b/bunx"), &["--bun"][..])));
        assert_eq!(runner(None, true, npx), Some((PathBuf::from("/n/npx"), &["-y"][..])));
        assert_eq!(runner(None, true, None), None);
    }
}
