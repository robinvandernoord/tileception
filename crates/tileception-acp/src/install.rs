//! Turns a registry agent into a command line, downloading and checking its
//! binary first when it comes as an archive.

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use crate::registry::{Agent, Launch, run};

/// A ready-to-run agent program.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Program {
    pub command: PathBuf,
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,
}

/// Archives larger than this are refused.
const MAX_ARCHIVE: &str = "400000000";

pub fn program(agent: &Agent, report: &dyn Fn(&str)) -> Result<Program, String> {
    if let Some(missing) = agent.missing() {
        return Err(format!("{} {missing}", agent.name));
    }
    match &agent.launch {
        Launch::Npx { package, args, env } => {
            let (command, flags) = crate::registry::npx().ok_or_else(|| format!("{} needs Node.js", agent.name))?;
            Ok(Program {
                command,
                args: flags
                    .iter()
                    .map(|flag| flag.to_string())
                    .chain([package.clone()])
                    .chain(args.iter().cloned())
                    .collect(),
                env: env.clone(),
            })
        }
        Launch::Uvx { package, args, env } => Ok(Program {
            command: "uvx".into(),
            args: std::iter::once(package.clone()).chain(args.iter().cloned()).collect(),
            env: env.clone(),
        }),
        Launch::Binary { archive, sha256, cmd, args, env } => {
            let dir = crate::data_dir().join("agents").join(safe(&agent.agent_id)).join(safe(&agent.version));
            let command = inside(&dir, cmd)?;
            if !command.is_file() {
                report(&format!("Downloading {} {}\u{2026}", agent.name, agent.version));
                download(archive, sha256, cmd, &dir)?;
            }
            if !command.is_file() {
                return Err(format!("{} is not in the downloaded archive", cmd));
            }
            Ok(Program { command, args: args.clone(), env: env.clone() })
        }
    }
}

/// The program's path inside the agent's folder; it may not climb out.
fn inside(dir: &Path, cmd: &str) -> Result<PathBuf, String> {
    let relative = Path::new(cmd.trim_start_matches("./"));
    if relative.is_absolute()
        || relative.components().any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return Err(format!("the registry names an unsafe program path: {cmd}"));
    }
    Ok(dir.join(relative))
}

/// Keeps registry ids and versions usable as folder names.
fn safe(name: &str) -> String {
    name.chars()
        .map(|character| if character.is_ascii_alphanumeric() || "._-".contains(character) { character } else { '_' })
        .collect()
}

fn download(url: &str, sha256: &str, cmd: &str, dir: &Path) -> Result<(), String> {
    if !url.starts_with("https://") {
        return Err(format!("refusing a download that is not https: {url}"));
    }
    // Not with_extension: versions have dots of their own.
    let mut partial = dir.as_os_str().to_owned();
    partial.push(".part");
    let partial = PathBuf::from(partial);
    let _ = std::fs::remove_dir_all(&partial);
    std::fs::create_dir_all(&partial).map_err(|error| format!("{}: {error}", partial.display()))?;
    let name = url.rsplit('/').next().unwrap_or("archive");
    let archive = partial.join(format!("download-{}", safe(name)));
    crate::curl(url, &archive, MAX_ARCHIVE)?;
    let actual = sha256_of(&archive)?;
    if !actual.eq_ignore_ascii_case(sha256) {
        let _ = std::fs::remove_dir_all(&partial);
        return Err(format!("the download's checksum does not match the registry ({actual} instead of {sha256})"));
    }
    let archive_path = archive.as_os_str();
    let destination = partial.as_os_str();
    let lower = name.to_lowercase();
    if lower.ends_with(".zip") {
        run("unzip", &[OsStr::new("-q"), archive_path, OsStr::new("-d"), destination])?;
    } else if lower.ends_with(".tar.gz")
        || lower.ends_with(".tgz")
        || lower.ends_with(".tar.bz2")
        || lower.ends_with(".tar.xz")
    {
        // tar picks the compression itself; --no-same-owner keeps files ours.
        run("tar", &[OsStr::new("-xf"), archive_path, OsStr::new("-C"), destination, OsStr::new("--no-same-owner")])?;
    } else if Path::new(cmd.trim_start_matches("./")).file_name() == Some(OsStr::new(name)) {
        // Not an archive but the program itself (sigit ships so).
        let program = partial.join(name);
        std::fs::rename(&archive, &program).map_err(|error| format!("{}: {error}", program.display()))?;
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755))
            .map_err(|error| format!("{}: {error}", program.display()))?;
    } else {
        return Err(format!("unknown archive type: {name}"));
    }
    let _ = std::fs::remove_file(&archive);
    let _ = std::fs::remove_dir_all(dir);
    if let Some(parent) = dir.parent() {
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    std::fs::rename(&partial, dir).map_err(|error| error.to_string())
}

fn sha256_of(path: &Path) -> Result<String, String> {
    let out =
        std::process::Command::new("sha256sum").arg(path).output().map_err(|error| format!("sha256sum: {error}"))?;
    if !out.status.success() {
        return Err("sha256sum failed".into());
    }
    let text = String::from_utf8_lossy(&out.stdout);
    text.split_whitespace().next().map(str::to_string).ok_or_else(|| "sha256sum printed nothing".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn program_paths_stay_inside() {
        let dir = Path::new("/agents/goose/1");
        assert_eq!(inside(dir, "./goose"), Ok(PathBuf::from("/agents/goose/1/goose")));
        assert_eq!(inside(dir, "bin/kilo"), Ok(PathBuf::from("/agents/goose/1/bin/kilo")));
        assert!(inside(dir, "../../bin/sh").is_err());
        assert!(inside(dir, "/bin/sh").is_err());
        assert_eq!(safe("@scope/pkg 1"), "_scope_pkg_1");
    }

    #[test]
    fn package_agents_become_commands() {
        let agent = Agent {
            agent_id: "x".into(),
            name: "X".into(),
            version: "1".into(),
            description: String::new(),
            icon: None,
            launch: Launch::Uvx { package: "x-acp==1".into(), args: vec!["-x".into()], env: BTreeMap::new() },
        };
        if crate::registry::on_path("uvx") {
            let program = program(&agent, &|_| {}).expect("uvx agent");
            assert_eq!(
                (program.command, program.args),
                (PathBuf::from("uvx"), vec!["x-acp==1".to_string(), "-x".to_string()])
            );
        } else {
            assert!(program(&agent, &|_| {}).is_err());
        }
    }
}
