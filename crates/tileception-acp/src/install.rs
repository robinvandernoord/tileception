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
    if let Some(installed) = &agent.installed {
        let (args, env) = match &agent.launch {
            Launch::Npx { args, env, .. } | Launch::Uvx { args, env, .. } | Launch::Binary { args, env, .. } => {
                (args, env)
            }
        };
        return Ok(Program {
            command: installed.command.clone(),
            args: installed.prefix.iter().chain(args).cloned().collect(),
            env: env.clone(),
        });
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
            command: crate::local::find("uvx").unwrap_or_else(|| "uvx".into()),
            args: std::iter::once(package.clone()).chain(args.iter().cloned()).collect(),
            env: env.clone(),
        }),
        Launch::Binary { archive, sha256, cmd, args, env } => {
            let dir = crate::data_dir().join("agents").join(safe(&agent.agent_id)).join(safe(&agent.version));
            let command = inside(&dir, cmd)?;
            if !command.is_file() {
                report(&format!("Downloading {} {}\u{2026}", agent.name, agent.version));
                download(archive, sha256, cmd, &dir)?;
                remove_other_versions(&dir);
            }
            if !command.is_file() {
                return Err(format!("{} is not in the downloaded archive", cmd));
            }
            Ok(Program { command, args: args.clone(), env: env.clone() })
        }
    }
}

/// Earlier versions of a downloaded agent, and downloads that never
/// finished, next to the version now in `dir`.
fn remove_other_versions(dir: &Path) {
    let (Some(parent), Some(keep)) = (dir.parent(), dir.file_name()) else { return };
    let Ok(entries) = std::fs::read_dir(parent) else { return };
    for entry in entries.flatten() {
        if entry.file_name() != keep
            && entry.file_type().is_ok_and(|kind| kind.is_dir())
            && let Err(error) = std::fs::remove_dir_all(entry.path())
        {
            tracing::debug!("{}: {error}", entry.path().display());
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

/// Downloads an archive over https into `dir`, replacing what is there, once
/// its checksum matches `sha256` and it unpacked without a member that would
/// land outside. A download that is not an archive but the program `cmd`
/// itself is kept under that name. `dir` is untouched when anything fails.
pub fn download(url: &str, sha256: &str, cmd: &str, dir: &Path) -> Result<(), String> {
    if !url.starts_with("https://") {
        return Err(format!("refusing a download that is not https: {url}"));
    }
    // Not with_extension: versions have dots of their own.
    let mut partial = dir.as_os_str().to_owned();
    partial.push(".part");
    let partial = PathBuf::from(partial);
    let _ = std::fs::remove_dir_all(&partial);
    std::fs::create_dir_all(&partial).map_err(|error| format!("{}: {error}", partial.display()))?;
    let unpacked = unpack(url, sha256, cmd, &partial);
    if let Err(error) = unpacked {
        let _ = std::fs::remove_dir_all(&partial);
        return Err(error);
    }
    let _ = std::fs::remove_dir_all(dir);
    if let Some(parent) = dir.parent() {
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    std::fs::rename(&partial, dir).map_err(|error| error.to_string())
}

/// The download, checked and unpacked into `partial`.
fn unpack(url: &str, sha256: &str, cmd: &str, partial: &Path) -> Result<(), String> {
    let name = url.rsplit('/').next().unwrap_or("archive");
    let archive = partial.join(format!("download-{}", safe(name)));
    crate::curl(url, &archive, MAX_ARCHIVE)?;
    unpack_file(&archive, name, sha256, cmd, partial)
}

/// Checks the downloaded `archive`, called `name` where it came from, and
/// unpacks it into `partial`.
fn unpack_file(archive: &Path, name: &str, sha256: &str, cmd: &str, partial: &Path) -> Result<(), String> {
    let actual = sha256_of(archive)?;
    if !actual.eq_ignore_ascii_case(sha256) {
        return Err(format!("the download's checksum does not match the published one ({actual} instead of {sha256})"));
    }
    let archive_path = archive.as_os_str();
    let destination = partial.as_os_str();
    let lower = name.to_lowercase();
    if lower.ends_with(".zip") {
        refuse_unsafe_members(&listing("unzip", &[OsStr::new("-Z1"), archive_path])?)?;
        run("unzip", &[OsStr::new("-q"), archive_path, OsStr::new("-d"), destination])?;
    } else if is_tarball(&lower) {
        refuse_unsafe_members(&listing("tar", &[OsStr::new("-tf"), archive_path])?)?;
        // tar picks the compression itself; --no-same-owner keeps files ours.
        run("tar", &[OsStr::new("-xf"), archive_path, OsStr::new("-C"), destination, OsStr::new("--no-same-owner")])?;
    } else if Path::new(cmd.trim_start_matches("./")).file_name() == Some(OsStr::new(name)) {
        // Not an archive but the program itself (sigit ships so).
        let program = partial.join(name);
        std::fs::rename(archive, &program).map_err(|error| format!("{}: {error}", program.display()))?;
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755))
            .map_err(|error| format!("{}: {error}", program.display()))?;
    } else {
        return Err(format!("unknown archive type: {name}"));
    }
    let _ = std::fs::remove_file(archive);
    Ok(())
}

fn is_tarball(lower: &str) -> bool {
    [".tar.gz", ".tgz", ".tar.bz2", ".tar.xz"].iter().any(|ending| lower.ends_with(ending))
}

/// Whether [`download`] can unpack a file of this name.
pub fn is_archive(name: &str) -> bool {
    let lower = name.to_lowercase();
    lower.ends_with(".zip") || is_tarball(&lower)
}

/// What a program prints, such as an archive's members.
fn listing(program: &str, args: &[&OsStr]) -> Result<String, String> {
    let out = std::process::Command::new(program)
        .args(args)
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|error| format!("{program}: {error}"))?;
    if !out.status.success() {
        return Err(format!("{program} could not read the archive: {}", String::from_utf8_lossy(&out.stderr).trim()));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Refuses an archive with a member that is absolute or climbs out with
/// `..`, whatever the unpacking program would have made of it.
fn refuse_unsafe_members(members: &str) -> Result<(), String> {
    let climbs = |member: &&str| {
        let path = Path::new(member);
        path.is_absolute() || path.components().any(|component| matches!(component, std::path::Component::ParentDir))
    };
    match members.lines().find(climbs) {
        Some(member) => Err(format!("the archive holds an unsafe path: {member}")),
        None => Ok(()),
    }
}

/// A file's SHA-256, in hex.
pub fn sha256_of(path: &Path) -> Result<String, String> {
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
    fn archives_may_not_unpack_outside() {
        assert_eq!(refuse_unsafe_members("plugin/\nplugin/bin/tool\n./README.md\n"), Ok(()));
        assert_eq!(refuse_unsafe_members("plugin/a..b\n"), Ok(()));
        for member in ["../.bashrc", "plugin/../../.ssh/authorized_keys", "/etc/passwd"] {
            let error = refuse_unsafe_members(&format!("plugin/tool\n{member}\n")).expect_err("refused");
            assert!(error.contains(member), "{error}");
        }
        assert!(is_archive("tool-x86_64-unknown-linux-gnu.tar.gz"));
        assert!(is_archive("Tool_Linux_amd64.ZIP"));
        assert!(!is_archive("tool-linux-amd64"));
        assert!(!is_archive("checksums.txt"));
    }

    #[test]
    fn archives_are_checked_before_they_unpack() {
        let work = std::env::temp_dir().join(format!("tileception-acp-unpack-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&work);
        let source = work.join("source/plugin");
        std::fs::create_dir_all(&source).expect("source dir");
        std::fs::write(source.join("tool"), "#!/bin/sh\n").expect("tool");
        let tar = |archive: &Path, extra: &[&str], member: &str| {
            let made = std::process::Command::new("tar")
                .arg("-czf")
                .arg(archive)
                .args(extra)
                .arg("-C")
                .arg(work.join("source/plugin"))
                .arg(member)
                .status()
                .expect("tar runs");
            assert!(made.success());
        };
        let good = work.join("good.tar.gz");
        tar(&good, &[], "tool");
        let sha256 = sha256_of(&good).expect("a checksum");
        let unpacked = |archive: &Path, sha256: &str| {
            let partial = work.join("partial");
            let _ = std::fs::remove_dir_all(&partial);
            std::fs::create_dir_all(&partial).expect("partial dir");
            let copy = partial.join("download");
            std::fs::copy(archive, &copy).expect("copy");
            unpack_file(&copy, "tool-linux.tar.gz", sha256, "", &partial).map(|()| partial.join("tool").is_file())
        };
        assert_eq!(unpacked(&good, &sha256.to_uppercase()), Ok(true));
        // Another archive than the one published is not unpacked at all.
        let wrong = "0".repeat(64);
        let error = unpacked(&good, &wrong).expect_err("a wrong checksum");
        assert!(error.contains("checksum does not match"), "{error}");
        assert!(!work.join("partial/tool").exists());
        // Nor is one with a member that climbs out, even with the right checksum.
        let climbing = work.join("climbing.tar.gz");
        tar(&climbing, &["-P"], "../plugin/tool");
        let error = unpacked(&climbing, &sha256_of(&climbing).expect("a checksum")).expect_err("an unsafe member");
        assert!(error.contains("unsafe path: ../plugin/tool"), "{error}");
        assert!(!work.join("partial/tool").exists());
        let _ = std::fs::remove_dir_all(work);
    }

    #[test]
    fn a_failed_download_leaves_what_was_installed() {
        let dir = std::env::temp_dir().join(format!("tileception-acp-download-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("installed dir");
        std::fs::write(dir.join("tool"), "old").expect("installed tool");
        assert!(download("http://example.invalid/tool.tar.gz", "00", "tool", &dir).is_err());
        assert_eq!(std::fs::read_to_string(dir.join("tool")).expect("still there"), "old");
        let _ = std::fs::remove_dir_all(dir);
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
            installed: None,
        };
        if let Some(uvx) = crate::local::find("uvx") {
            let program = program(&agent, &|_| {}).expect("uvx agent");
            assert_eq!((program.command, program.args), (uvx, vec!["x-acp==1".to_string(), "-x".to_string()]));
        } else {
            assert!(program(&agent, &|_| {}).is_err());
        }
        // An installed copy runs with the registry's arguments instead.
        let installed = Agent {
            installed: Some(crate::local::Installed {
                command: "/usr/bin/bun".into(),
                prefix: vec!["--bun".into(), "/g/x/cli.js".into()],
                version: None,
                matched: crate::local::Match::Package,
            }),
            ..agent
        };
        let program = program(&installed, &|_| {}).expect("installed agent");
        assert_eq!(program.command, PathBuf::from("/usr/bin/bun"));
        assert_eq!(program.args, ["--bun", "/g/x/cli.js", "-x"]);
    }

    #[test]
    fn keeps_only_the_current_version() {
        let agents = std::env::temp_dir().join(format!("tileception-acp-versions-{}", std::process::id()));
        for version in ["1.0", "1.1.part", "2.0"] {
            std::fs::create_dir_all(agents.join(version)).expect("version dir");
        }
        remove_other_versions(&agents.join("2.0"));
        let left: Vec<_> = std::fs::read_dir(&agents).expect("dir").flatten().map(|entry| entry.file_name()).collect();
        assert_eq!(left, ["2.0"]);
        let _ = std::fs::remove_dir_all(agents);
    }
}
