//! Agents already installed on this machine, so the registry's download or
//! package runner is only needed when there is none.
//!
//! A desktop session's PATH often lacks what installers add to shell rc files
//! (`~/.opencode/bin`, `~/.bun/bin`, ...), so programs are looked up in the
//! login shell's PATH and a few well-known folders as well as our own.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::time::Duration;

use serde::Deserialize;

use crate::registry::Launch;

/// How long the login shell may take to print its environment.
const SHELL_WAIT: Duration = Duration::from_secs(3);
const BEGIN: &str = "__TILECEPTION_ENV_BEGIN__";
const END: &str = "__TILECEPTION_ENV_END__";

/// A copy of a registry agent that is already installed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Installed {
    /// The program to run, followed by `prefix` and the registry's arguments.
    pub command: PathBuf,
    /// Arguments before the registry's, such as the script Bun runs.
    pub prefix: Vec<String>,
    /// The installed version, when it can be read without running anything.
    pub version: Option<String>,
    pub matched: Match,
}

/// How sure the match is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Match {
    /// The exact npm or Python package the registry names.
    Package,
    /// Only a program with the same name, which may be another program.
    Name,
}

static DIRS: OnceLock<Vec<PathBuf>> = OnceLock::new();

/// Folders searched for programs: the login shell's PATH, then our own, then
/// folders installers are known to use. Read once; the first call may wait
/// for the login shell, so make it off the main thread.
pub fn search_path() -> &'static [PathBuf] {
    DIRS.get_or_init(|| {
        let login = login_shell_path();
        if login.is_none() {
            tracing::debug!("the login shell's PATH could not be read; using known folders");
        }
        merge_paths(login.as_deref(), std::env::var_os("PATH"), &crate::home(), &env_dirs())
    })
}

/// [`search_path`] as a PATH value, for agent programs and what they start.
/// None until something has read it, as for agents started without the
/// registry; they keep the compositor's PATH.
pub fn path_var() -> Option<OsString> {
    std::env::join_paths(DIRS.get()?).ok()
}

/// A program in [`search_path`].
pub fn find(program: &str) -> Option<PathBuf> {
    find_in(search_path(), program)
}

fn find_in(dirs: &[PathBuf], program: &str) -> Option<PathBuf> {
    if program.is_empty() || program.contains('/') {
        return None;
    }
    dirs.iter().map(|dir| dir.join(program)).find(|path| executable(path))
}

fn executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
}

/// Folders named by installers' environment variables, when set.
fn env_dirs() -> Vec<PathBuf> {
    // pnpm puts its programs in PNPM_HOME itself; the others in its bin.
    [("BUN_INSTALL", "bin"), ("VOLTA_HOME", "bin"), ("PNPM_HOME", "")]
        .iter()
        .filter_map(|(name, bin)| {
            let dir = PathBuf::from(std::env::var_os(name)?);
            Some(if bin.is_empty() { dir } else { dir.join(bin) })
        })
        .collect()
}

fn merge_paths(login: Option<&str>, own: Option<OsString>, home: &Path, extra: &[PathBuf]) -> Vec<PathBuf> {
    let known = [
        ".local/bin",
        ".opencode/bin",
        ".bun/bin",
        ".cargo/bin",
        ".npm-global/bin",
        ".volta/bin",
        ".deno/bin",
        ".local/share/pnpm",
        "go/bin",
    ];
    let mut dirs: Vec<PathBuf> = Vec::new();
    let login = login.map(|value| std::env::split_paths(value).collect::<Vec<_>>()).unwrap_or_default();
    let own = own.map(|value| std::env::split_paths(&value).collect::<Vec<_>>()).unwrap_or_default();
    let known = known.iter().map(|dir| home.join(dir));
    for dir in login.into_iter().chain(own).chain(extra.iter().cloned()).chain(known) {
        if dir.is_absolute() && !dirs.contains(&dir) {
            dirs.push(dir);
        }
    }
    dirs
}

/// PATH as an interactive login shell sets it: `.bashrc` and the like only
/// run for interactive shells, and that is where installers add their
/// folders. `env` prints it the same way in any shell.
fn login_shell_path() -> Option<String> {
    let shell = std::env::var_os("SHELL").filter(|shell| !shell.is_empty()).unwrap_or_else(|| "/bin/sh".into());
    let script = format!("echo {BEGIN}; env; echo {END}");
    let mut child = Command::new(&shell)
        .args(["-l", "-i", "-c", &script])
        .current_dir(crate::home())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| tracing::debug!("{}: {error}", Path::new(&shell).display()))
        .ok()?;
    let mut stdout = child.stdout.take()?;
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name("tileception-acp-shell".into())
        .spawn(move || {
            let mut out = Vec::new();
            let _ = stdout.read_to_end(&mut out);
            let _ = sender.send(out);
        })
        .ok()?;
    let out = receiver.recv_timeout(SHELL_WAIT);
    // Done or not: a shell that hangs in its rc files is not waited for.
    let _ = child.kill();
    let _ = child.wait();
    env_path(&String::from_utf8_lossy(&out.ok()?))
}

/// PATH from `env` output between the markers; rc files may print around it.
fn env_path(output: &str) -> Option<String> {
    let (_, rest) = output.split_once(&format!("{BEGIN}\n"))?;
    let (env, _) = rest.split_once(&format!("\n{END}"))?;
    env.lines().find_map(|line| line.strip_prefix("PATH=")).filter(|path| !path.is_empty()).map(str::to_string)
}

/// The installed copy of an agent launched so, if there is one.
pub fn installed(launch: &Launch) -> Option<Installed> {
    match launch {
        Launch::Npx { package, .. } => npm_package(&node_roots(), package),
        Launch::Uvx { package, .. } => uv_tool(&uv_tools_dir(), package),
        Launch::Binary { cmd, .. } => {
            let name = Path::new(cmd).file_name()?.to_str()?;
            let command = find(name)?;
            Some(Installed { command, prefix: Vec::new(), version: None, matched: Match::Name })
        }
    }
}

/// A package name without the version the registry pins it to.
fn package_name(spec: &str) -> &str {
    let spec = spec.split("==").next().unwrap_or(spec);
    // A scope starts with @; the version comes after the next one.
    match spec.get(1..).and_then(|rest| rest.find('@')) {
        Some(index) => &spec[..=index],
        None => spec,
    }
}

/// Global node_modules folders of npm, Bun and pnpm.
fn node_roots() -> Vec<PathBuf> {
    let home = crate::home();
    let mut roots = Vec::new();
    let bun = std::env::var_os("BUN_INSTALL").map_or_else(|| home.join(".bun"), PathBuf::from);
    roots.push(bun.join("install/global/node_modules"));
    let pnpm = std::env::var_os("PNPM_HOME").map_or_else(|| home.join(".local/share/pnpm"), PathBuf::from);
    if let Ok(entries) = std::fs::read_dir(pnpm.join("global")) {
        let mut stores: Vec<PathBuf> = entries.flatten().map(|entry| entry.path().join("node_modules")).collect();
        stores.sort();
        roots.extend(stores);
    }
    // npm keeps global packages in <prefix>/lib/node_modules, next to the
    // <prefix>/bin that holds npm or node (/usr, nvm, ~/.npm-global...).
    let prefixes =
        std::env::var_os("NPM_CONFIG_PREFIX").map(PathBuf::from).into_iter().chain(npmrc_prefix(&home)).chain(
            search_path()
                .iter()
                .filter(|dir| executable(&dir.join("npm")) || executable(&dir.join("node")))
                .filter_map(|dir| dir.parent().map(Path::to_path_buf)),
        );
    for prefix in prefixes {
        let root = prefix.join("lib/node_modules");
        if !roots.contains(&root) {
            roots.push(root);
        }
    }
    roots
}

/// `prefix=` from ~/.npmrc, where `npm config set prefix` puts it.
fn npmrc_prefix(home: &Path) -> Option<PathBuf> {
    let text = std::fs::read_to_string(home.join(".npmrc")).ok()?;
    let value = text.lines().find_map(|line| {
        let (key, value) = line.split_once('=')?;
        (key.trim() == "prefix").then(|| value.trim().trim_matches('"').to_string())
    })?;
    Some(match value.strip_prefix("~/") {
        Some(rest) => home.join(rest),
        None => PathBuf::from(value),
    })
}

#[derive(Deserialize)]
struct PackageJson {
    version: Option<String>,
    bin: Option<Bin>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum Bin {
    One(String),
    Many(BTreeMap<String, String>),
}

/// The package in the first root that has it, and the program npx would run:
/// the one named after the package, or its only one.
fn npm_package(roots: &[PathBuf], spec: &str) -> Option<Installed> {
    let name = package_name(spec);
    let (dir, json) = roots.iter().map(|root| root.join(name)).find_map(|dir| {
        let text = std::fs::read_to_string(dir.join("package.json")).ok()?;
        Some((dir, serde_json::from_str::<PackageJson>(&text).ok()?))
    })?;
    let unscoped = name.rsplit('/').next().unwrap_or(name);
    let script = match json.bin? {
        Bin::One(script) => script,
        Bin::Many(mut bins) if bins.len() == 1 => bins.pop_first()?.1,
        Bin::Many(mut bins) => bins.remove(unscoped)?,
    };
    let script = dir.join(script.trim_start_matches("./"));
    if !script.is_file() {
        return None;
    }
    // Scripts ask for node; under Bun alone, Bun runs them instead.
    let (command, prefix) = if needs_node(&script) && find("node").is_none() {
        (find("bun")?, vec!["--bun".into(), script.to_string_lossy().into_owned()])
    } else if executable(&script) {
        (script, Vec::new())
    } else {
        (find("node")?, vec![script.to_string_lossy().into_owned()])
    };
    Some(Installed { command, prefix, version: json.version, matched: Match::Package })
}

fn needs_node(script: &Path) -> bool {
    let mut head = [0u8; 128];
    let read = std::fs::File::open(script).and_then(|mut file| file.read(&mut head)).unwrap_or(0);
    let first = String::from_utf8_lossy(&head[..read]);
    first.lines().next().is_some_and(|line| line.starts_with("#!") && line.contains("node"))
}

/// Where `uv tool install` puts tools.
fn uv_tools_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("UV_TOOL_DIR") {
        return dir.into();
    }
    std::env::var_os("XDG_DATA_HOME").map_or_else(|| crate::home().join(".local/share"), PathBuf::from).join("uv/tools")
}

/// Python's spelling-insensitive package names (PEP 503).
fn normalize(name: &str) -> String {
    name.to_lowercase().replace(['_', '.'], "-")
}

/// The tool uv installed for the package, and the program named after it,
/// which is what uvx runs.
fn uv_tool(tools: &Path, spec: &str) -> Option<Installed> {
    let name = package_name(spec);
    let dir = tools.join(normalize(name));
    let command = dir.join("bin").join(name);
    if !executable(&command) {
        return None;
    }
    let version = std::fs::read_dir(dir.join("lib")).ok().into_iter().flatten().flatten().find_map(|python| {
        let packages = std::fs::read_dir(python.path().join("site-packages")).ok()?;
        packages.flatten().find_map(|entry| {
            let file = entry.file_name();
            let (package, version) = file.to_str()?.strip_suffix(".dist-info")?.rsplit_once('-')?;
            (normalize(package) == normalize(name)).then(|| version.to_string())
        })
    });
    Some(Installed { command, prefix: Vec::new(), version, matched: Match::Package })
}

/// Whether `installed` is an older version than `latest`, comparing the
/// numbers of dotted versions (2.10 is newer than 2.9).
pub fn older(installed: &str, latest: &str) -> bool {
    let numbers = |version: &str| -> Vec<u64> {
        version
            .trim_start_matches('v')
            .split(['-', '+'])
            .next()
            .unwrap_or_default()
            .split('.')
            .map(|part| part.parse().unwrap_or(0))
            .collect()
    };
    let (mut old, mut new) = (numbers(installed), numbers(latest));
    let len = old.len().max(new.len());
    old.resize(len, 0);
    new.resize(len, 0);
    old < new
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("tileception-acp-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch dir");
        dir
    }

    fn write_program(path: &Path, text: &str) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::create_dir_all(path.parent().expect("parent")).expect("dir");
        std::fs::write(path, text).expect("write");
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    }

    #[test]
    fn reads_path_between_the_markers() {
        let output = format!("welcome!\n{BEGIN}\nHOME=/home/x\nPATH=/a:/b\nX=PATH=/c\n{END}\nbye\n");
        assert_eq!(env_path(&output).as_deref(), Some("/a:/b"));
        assert_eq!(env_path("PATH=/a\n"), None);
        assert_eq!(env_path(&format!("{BEGIN}\nPATH=/a\n")), None);
    }

    #[test]
    fn login_path_comes_first_then_known_folders() {
        let home = Path::new("/home/x");
        let dirs = merge_paths(
            Some("/home/x/.opencode/bin:/usr/bin:relative"),
            Some("/usr/bin:/sbin".into()),
            home,
            &[PathBuf::from("/opt/bun/bin")],
        );
        assert_eq!(
            dirs[..4],
            [
                PathBuf::from("/home/x/.opencode/bin"),
                PathBuf::from("/usr/bin"),
                PathBuf::from("/sbin"),
                PathBuf::from("/opt/bun/bin"),
            ]
        );
        assert!(dirs.contains(&PathBuf::from("/home/x/.bun/bin")));
        assert_eq!(dirs.iter().filter(|dir| dir.ends_with(".opencode/bin")).count(), 1);
        assert!(!dirs.contains(&PathBuf::from("relative")));
    }

    #[test]
    fn package_names_drop_the_version() {
        assert_eq!(package_name("@google/gemini-cli@0.67.0"), "@google/gemini-cli");
        assert_eq!(package_name("@google/gemini-cli"), "@google/gemini-cli");
        assert_eq!(package_name("cline@3.0.74"), "cline");
        assert_eq!(package_name("fast-agent-acp==0.10.1"), "fast-agent-acp");
        assert_eq!(package_name("minion-code@0.1.44"), "minion-code");
    }

    #[test]
    fn finds_global_npm_packages() {
        let root = scratch("npm");
        let empty = root.join("empty");
        let modules = root.join("lib/node_modules");
        write_program(&modules.join("@google/gemini-cli/dist/index.js"), "#!/bin/sh\n");
        std::fs::write(
            modules.join("@google/gemini-cli/package.json"),
            r#"{"name":"@google/gemini-cli","version":"0.60.0","bin":{"gemini":"dist/index.js"}}"#,
        )
        .expect("package.json");
        write_program(&modules.join("multi/a.js"), "#!/bin/sh\n");
        std::fs::write(modules.join("multi/package.json"), r#"{"bin":{"a":"a.js","b":"b.js"}}"#).expect("json");

        let found = npm_package(&[empty, modules.clone()], "@google/gemini-cli@0.67.0").expect("installed");
        assert_eq!(found.command, modules.join("@google/gemini-cli/dist/index.js"));
        assert_eq!((found.version.as_deref(), found.matched), (Some("0.60.0"), Match::Package));
        // Several programs, none named after the package: npx would not know either.
        assert_eq!(npm_package(std::slice::from_ref(&modules), "multi"), None);
        assert_eq!(npm_package(&[modules], "absent@1"), None);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn finds_uv_tools() {
        let tools = scratch("uv");
        write_program(&tools.join("fast-agent-acp/bin/fast-agent-acp"), "#!/bin/sh\n");
        std::fs::create_dir_all(
            tools.join("fast-agent-acp/lib/python3.12/site-packages/fast_agent_acp-0.9.0.dist-info"),
        )
        .expect("dist-info");
        let found = uv_tool(&tools, "fast-agent-acp==0.10.1").expect("installed");
        assert_eq!(found.command, tools.join("fast-agent-acp/bin/fast-agent-acp"));
        assert_eq!(found.version.as_deref(), Some("0.9.0"));
        assert_eq!(uv_tool(&tools, "minion-code@0.1.44"), None);
        let _ = std::fs::remove_dir_all(tools);
    }

    #[test]
    fn finds_programs_by_name() {
        let dir = scratch("bin");
        write_program(&dir.join("opencode"), "#!/bin/sh\n");
        std::fs::write(dir.join("plain"), "not executable").expect("write");
        let dirs = [dir.join("missing"), dir.clone()];
        assert_eq!(find_in(&dirs, "opencode"), Some(dir.join("opencode")));
        assert_eq!(find_in(&dirs, "plain"), None);
        assert_eq!(find_in(&dirs, "../opencode"), None);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn compares_versions_by_number() {
        assert!(older("0.60.0", "0.67.0"));
        assert!(older("2.9", "2.10.0"));
        assert!(older("v1.0.24", "1.54.0"));
        assert!(!older("2.2.6", "2.2.6"));
        assert!(!older("2.3.0-beta", "2.2.6"));
    }
}
