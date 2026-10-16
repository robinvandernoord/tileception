//! The doctor's connection to coding agents: the ACP Registry (which agents
//! exist and how to start them) and an Agent Client Protocol client that
//! runs one agent as a child process, speaking JSON-RPC over its stdio.
//!
//! Everything blocking happens on worker threads; results come back as
//! [`Event`]s through a callback.

pub mod install;
pub mod registry;
mod session;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub use registry::{Agent, Launch};
pub use session::Session;

/// A way to log in that an agent offers.
#[derive(Debug, Clone, PartialEq)]
pub struct Login {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
    /// Runs the agent's own login in a terminal, rather than asking the agent.
    pub terminal: Option<install::Program>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Status {
    /// Downloading or starting the agent.
    Starting(String),
    /// The agent wants a login first.
    NeedsLogin(Vec<Login>),
    /// Waiting for a terminal login to finish.
    LoggingIn,
    Ready,
    /// The agent stopped or never started.
    Failed(String),
}

/// What a permission request is about, for deciding without asking.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Action {
    /// read, edit, delete, move, search, execute, think, fetch, ...
    pub kind: Option<String>,
    /// Files it touches.
    pub paths: Vec<PathBuf>,
    /// The command line, for commands.
    pub command: Option<String>,
    /// Edits it would make: path, old text (none for a new file), new text.
    pub diffs: Vec<(PathBuf, Option<String>, String)>,
}

/// A file request from the agent; answer with [`Request::Reply`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileOp {
    Read { path: PathBuf, line: Option<u64>, limit: Option<u64> },
    Write { path: PathBuf, content: String },
}

/// A choice offered when the agent asks for permission.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Choice {
    pub id: String,
    pub name: String,
    /// allow_once, allow_always, reject_once or reject_always.
    pub kind: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    Registry(Result<Vec<Agent>, String>),
    Status(Status),
    /// Part of the agent's answer.
    Text(String),
    /// Part of its reasoning, for those that share it.
    Thought(String),
    /// A tool call started or changed.
    Tool {
        id: String,
        title: Option<String>,
        kind: Option<String>,
        status: Option<String>,
    },
    /// The agent asks before doing something; answer with [`Request::Permission`].
    Permission {
        id: serde_json::Value,
        title: String,
        choices: Vec<Choice>,
        action: Action,
    },
    /// The agent reads or writes a file through Tileception.
    File {
        id: serde_json::Value,
        op: FileOp,
    },
    /// The agent finished answering: end_turn, cancelled, refusal, ...
    TurnEnded(String),
    /// Something to tell the user that is not the agent's answer.
    Note(String),
    /// Run this in a terminal for a terminal login, then send [`Request::Retry`].
    Terminal(install::Program),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Request {
    Prompt(String),
    Cancel,
    /// `None` declines.
    Permission {
        id: serde_json::Value,
        choice: Option<String>,
    },
    /// Logs in with one of the agent's methods.
    Login(String),
    /// Tries again after a terminal login.
    Retry,
    /// Answers an [`Event::File`]: the result, or why not.
    Reply {
        id: serde_json::Value,
        result: Result<serde_json::Value, String>,
    },
}

pub type Emit = std::sync::Arc<dyn Fn(Event) + Send + Sync>;

/// Loads the registry on a worker thread, reporting [`Event::Registry`].
pub fn fetch_registry(refresh: bool, emit: Emit) {
    let spawned = std::thread::Builder::new()
        .name("tileception-acp-registry".into())
        .spawn(move || emit(Event::Registry(registry::load(refresh))));
    if let Err(error) = spawned {
        tracing::warn!("could not load the ACP Registry: {error}");
    }
}

fn home() -> PathBuf {
    PathBuf::from(std::env::var_os("HOME").unwrap_or_default())
}

pub fn cache_dir() -> PathBuf {
    std::env::var_os("XDG_CACHE_HOME").map(PathBuf::from).unwrap_or_else(|| home().join(".cache")).join("tileception")
}

pub fn data_dir() -> PathBuf {
    std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join(".local/share"))
        .join("tileception")
}

/// Downloads `url` to `to` over http(s), bounded in size and time.
pub(crate) fn curl(url: &str, to: &Path, max_bytes: &str) -> Result<(), String> {
    let agent = concat!("Tileception/", env!("CARGO_PKG_VERSION"), " (doctor; +https://tileception.space)");
    let out = std::process::Command::new("curl")
        .args(["--fail", "--silent", "--show-error", "--location", "--max-redirs", "5"])
        .args(["--proto", "=https", "--proto-redir", "=https", "--user-agent", agent])
        .args(["--max-filesize", max_bytes, "--max-time", "300", "--output"])
        .arg(to)
        .arg("--")
        .arg(url)
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|e| format!("curl: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        let _ = std::fs::remove_file(to);
        Err(format!("download failed: {}", String::from_utf8_lossy(&out.stderr).trim()))
    }
}

/// Environment for a program: ours plus its own variables.
pub(crate) fn with_env(command: &mut std::process::Command, env: &BTreeMap<String, String>) {
    for (key, value) in env {
        command.env(key, value);
    }
}
