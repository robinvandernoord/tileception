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
/// An archive may not unpack to more bytes than this.
pub const MAX_UNPACKED: u64 = 2_000_000_000;

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
/// it is [staged](stage). `dir` is untouched when anything fails.
pub fn download(url: &str, sha256: &str, cmd: &str, dir: &Path) -> Result<(), String> {
    let staged = stage(url, sha256, cmd, dir)?;
    let _ = std::fs::remove_dir_all(dir);
    std::fs::rename(&staged, dir).map_err(|error| error.to_string())
}

/// Downloads an archive over https and unpacks it beside `dir`, in the
/// folder this returns, leaving `dir` as it is. The archive's checksum must
/// match `sha256`, it may not unpack to more than [`MAX_UNPACKED`] bytes, and
/// nothing in it may land or point outside its folder. A download that is not
/// an archive but the program `cmd` itself is kept under that name. Nothing
/// is left behind when it fails.
pub fn stage(url: &str, sha256: &str, cmd: &str, dir: &Path) -> Result<PathBuf, String> {
    if !url.starts_with("https://") {
        return Err(format!("refusing a download that is not https: {url}"));
    }
    let name = url.rsplit('/').next().unwrap_or("archive");
    stage_from(|archive| crate::curl(url, archive, MAX_ARCHIVE), name, sha256, cmd, dir, MAX_UNPACKED)
}

/// [`stage`], with the archive called `name` brought in by `fetch`.
fn stage_from(
    fetch: impl FnOnce(&Path) -> Result<(), String>,
    name: &str,
    sha256: &str,
    cmd: &str,
    dir: &Path,
    most: u64,
) -> Result<PathBuf, String> {
    // Not with_extension: versions have dots of their own.
    let mut partial = dir.as_os_str().to_owned();
    partial.push(".part");
    let partial = PathBuf::from(partial);
    let _ = std::fs::remove_dir_all(&partial);
    std::fs::create_dir_all(&partial).map_err(|error| format!("{}: {error}", partial.display()))?;
    let archive = partial.join(format!("download-{}", safe(name)));
    let unpacked = fetch(&archive).and_then(|()| unpack_file(&archive, name, sha256, cmd, &partial, most));
    match unpacked {
        Ok(()) => Ok(partial),
        Err(error) => {
            let _ = std::fs::remove_dir_all(&partial);
            Err(error)
        }
    }
}

/// Checks the downloaded `archive`, called `name` where it came from, and
/// unpacks it into `partial`.
fn unpack_file(archive: &Path, name: &str, sha256: &str, cmd: &str, partial: &Path, most: u64) -> Result<(), String> {
    let actual = sha256_of(archive)?;
    if !actual.eq_ignore_ascii_case(sha256) {
        return Err(format!("the download's checksum does not match the published one ({actual} instead of {sha256})"));
    }
    let archive_path = archive.as_os_str();
    let destination = partial.as_os_str();
    let lower = name.to_lowercase();
    if lower.ends_with(".zip") {
        refuse_unsafe_members(&zip_members(archive_path)?)?;
        refuse_larger(most, "unzip", &[OsStr::new("-p"), archive_path])?;
        run("unzip", &[OsStr::new("-q"), archive_path, OsStr::new("-d"), destination])?;
    } else if is_tarball(&lower) {
        refuse_unsafe_members(&listing("tar", &[OsStr::new("-tf"), archive_path])?)?;
        refuse_larger(most, "tar", &[OsStr::new("-xOf"), archive_path])?;
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
    refuse_unsafe_files(partial, most)
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

/// The members of a zip archive, one a line, however this system's unzip
/// lists them: Info-ZIP's `-Z1`, `zipinfo`, or the table `unzip -l` prints,
/// which is all BusyBox has. An archive that cannot be listed is refused:
/// what lands outside the folder would not be seen afterwards.
fn zip_members(archive: &OsStr) -> Result<String, String> {
    listing("unzip", &[OsStr::new("-Z1"), archive])
        .or_else(|_| listing("zipinfo", &[OsStr::new("-1"), archive]))
        .or_else(|_| listing("unzip", &[OsStr::new("-l"), archive]).and_then(|table| names_in_table(&table)))
        .map_err(|error| format!("the zip archive could not be listed, so it is not unpacked ({error})"))
}

/// The names in what `unzip -l` prints: every row between the two dashed
/// lines starts with a length, a date and a time, and there are as many as
/// the last line counts. A row that does not, such as the rest of a name
/// with a line break in it, refuses the archive.
fn names_in_table(table: &str) -> Result<String, String> {
    let name = |line: &str| {
        let mut rest = line;
        for column in 0..3 {
            rest = rest.trim_start();
            let end = rest.find(char::is_whitespace)?;
            let word = &rest[..end];
            let fits = match column {
                0 => word.chars().all(|letter| letter.is_ascii_digit()),
                1 => word.contains('-'),
                _ => word.contains(':'),
            };
            if !fits {
                return None;
            }
            rest = &rest[end..];
        }
        Some(rest.trim_start().to_string())
    };
    let unreadable = || "unzip -l printed a list that cannot be read".to_string();
    let lines: Vec<&str> = table.lines().collect();
    let dashed: Vec<usize> =
        lines.iter().enumerate().filter(|(_, line)| line.trim_start().starts_with("---")).map(|(row, _)| row).collect();
    let [first, last] = dashed[..] else { return Err(unreadable()) };
    let names =
        lines[first + 1..last].iter().map(|line| name(line)).collect::<Option<Vec<_>>>().ok_or_else(unreadable)?;
    let counted = lines
        .get(last + 1)
        .and_then(|footer| footer.split_whitespace().nth(1))
        .and_then(|count| count.parse::<usize>().ok());
    if counted != Some(names.len()) {
        return Err(unreadable());
    }
    Ok(names.join("\n"))
}

/// Refuses an archive with a member that is absolute or climbs out with
/// `..`, whatever the unpacking program would have made of it. A backslash
/// counts as a slash, as it does where zip archives are made.
fn refuse_unsafe_members(members: &str) -> Result<(), String> {
    let climbs = |member: &&str| {
        let member = member.replace('\\', "/");
        let path = Path::new(&member);
        path.is_absolute() || path.components().any(|component| matches!(component, std::path::Component::ParentDir))
    };
    match members.lines().find(climbs) {
        Some(member) => Err(format!("the archive holds an unsafe path: {member}")),
        None => Ok(()),
    }
}

/// Refuses an archive whose files are more than `most` bytes together,
/// before any of them is written: `program` writes them all to its output,
/// which is counted and thrown away.
fn refuse_larger(most: u64, program: &str, args: &[&OsStr]) -> Result<(), String> {
    use std::io::Read;
    let mut child = std::process::Command::new(program)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|error| format!("{program}: {error}"))?;
    let mut total: u64 = 0;
    if let Some(mut output) = child.stdout.take() {
        let mut chunk = vec![0; 1 << 16];
        while total <= most {
            match output.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(length) => total += length as u64,
            }
        }
    }
    let _ = child.kill();
    let _ = child.wait();
    if total > most { Err(too_large(most)) } else { Ok(()) }
}

/// Whether `target`, followed from `folder` inside `root` one part at a
/// time, never leaves `root`.
fn stays_within(root: &Path, folder: &Path, target: &Path) -> bool {
    use std::path::Component;
    let Ok(start) = folder.strip_prefix(root) else { return false };
    let mut depth = start.components().count();
    for part in target.components() {
        match part {
            Component::Normal(_) => depth += 1,
            Component::CurDir => {}
            Component::ParentDir if depth > 0 => depth -= 1,
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => return false,
        }
    }
    true
}

fn too_large(most: u64) -> String {
    format!("the archive unpacks to more than {} MB", most / 1_000_000)
}

/// Looks over what was unpacked into `root`, and refuses it when a link
/// points outside or nowhere, when something is neither a file, a folder
/// nor a link, or when the files are more than `most` bytes together.
fn refuse_unsafe_files(root: &Path, most: u64) -> Result<(), String> {
    let inside = root.canonicalize().map_err(|error| format!("{}: {error}", root.display()))?;
    let shown = |path: &Path| path.strip_prefix(root).unwrap_or(path).display().to_string();
    let mut total: u64 = 0;
    let mut folders = vec![root.to_path_buf()];
    while let Some(folder) = folders.pop() {
        let entries = std::fs::read_dir(&folder).map_err(|error| format!("{}: {error}", folder.display()))?;
        for entry in entries {
            let path = entry.map_err(|error| error.to_string())?.path();
            let file = std::fs::symlink_metadata(&path).map_err(|error| format!("{}: {error}", path.display()))?;
            let kind = file.file_type();
            if kind.is_dir() {
                folders.push(path);
            } else if kind.is_file() {
                total = total.saturating_add(file.len());
                if total > most {
                    return Err(too_large(most));
                }
            } else if kind.is_symlink() {
                let target = std::fs::read_link(&path).map_err(|error| format!("{}: {error}", path.display()))?;
                // Not out even to come back, as the folder is renamed once
                // it is whole; and followed all the way, through other links.
                let within = path.parent().is_some_and(|folder| stays_within(root, folder, &target));
                let stays = within && path.canonicalize().is_ok_and(|real| real.starts_with(&inside));
                if !stays {
                    return Err(format!(
                        "the archive holds a link that points outside it: {} to {}",
                        shown(&path),
                        target.display()
                    ));
                }
            } else {
                return Err(format!("the archive holds something that is not a file: {}", shown(&path)));
            }
        }
    }
    Ok(())
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
        for member in
            ["../.bashrc", "plugin/../../.ssh/authorized_keys", "/etc/passwd", "..\\..\\.bashrc", "plugin\\..\\..\\x"]
        {
            let error = refuse_unsafe_members(&format!("plugin/tool\n{member}\n")).expect_err("refused");
            assert!(error.contains(member), "{error}");
        }
        assert!(is_archive("tool-x86_64-unknown-linux-gnu.tar.gz"));
        assert!(is_archive("Tool_Linux_amd64.ZIP"));
        assert!(!is_archive("tool-linux-amd64"));
        assert!(!is_archive("checksums.txt"));
    }

    #[test]
    fn zip_members_are_read_from_unzips_table() {
        // As Info-ZIP and BusyBox print it.
        let table = "Archive:  tool.zip\n  Length      Date    Time    Name\n---------  ---------- -----   ----\n        0  2026-11-19 20:00   tool/\n     1234  2026-11-19 20:00   tool/bin/my tool\n       12  11-19-2026 20:00   ../climbs\n---------                     -------\n     1246                     3 files\n";
        let names = names_in_table(table).expect("a table");
        assert_eq!(names, "tool/\ntool/bin/my tool\n../climbs");
        assert!(refuse_unsafe_members(&names).is_err());
        // A name with a line break hides the rest on a row of its own.
        let broken = table.replace("tool/bin/my tool\n", "tool/bin/my tool\n../../../x\n");
        assert!(names_in_table(&broken).is_err());
        // So does a count that does not match, or no table at all.
        assert!(names_in_table(&table.replace("3 files", "4 files")).is_err());
        assert!(names_in_table("Archive:  tool.zip\n").is_err());
    }

    #[test]
    fn links_may_not_leave_even_to_come_back() {
        let root = Path::new("/r/installed.part");
        let folder = root.join("lib");
        assert!(stays_within(root, &folder, Path::new("../tool")));
        assert!(stays_within(root, &folder, Path::new("./x/../y")));
        assert!(stays_within(root, root, Path::new("lib/tool.so")));
        assert!(!stays_within(root, root, Path::new("../installed.part/tool")));
        assert!(!stays_within(root, &folder, Path::new("../../installed.part/tool")));
        assert!(!stays_within(root, &folder, Path::new("/r/installed.part/tool")));
    }

    /// A folder to work in, and a way to make an archive of `source/plugin`.
    fn workshop(name: &str) -> PathBuf {
        let work = std::env::temp_dir().join(format!("tileception-acp-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&work);
        std::fs::create_dir_all(work.join("source/plugin")).expect("source dir");
        work
    }

    fn tar(work: &Path, archive: &str, extra: &[&str], member: &str) -> (PathBuf, String) {
        let archive = work.join(archive);
        let made = std::process::Command::new("tar")
            .arg("-czf")
            .arg(&archive)
            .args(extra)
            .arg("-C")
            .arg(work.join("source/plugin"))
            .arg(member)
            .status()
            .expect("tar runs");
        assert!(made.success());
        let sha256 = sha256_of(&archive).expect("a checksum");
        (archive, sha256)
    }

    /// Stages a local archive for `work/installed`, as a download would be.
    fn staged(work: &Path, archive: &Path, sha256: &str, most: u64) -> Result<PathBuf, String> {
        let copy = |into: &Path| std::fs::copy(archive, into).map(drop).map_err(|error| error.to_string());
        stage_from(copy, "tool-linux.tar.gz", sha256, "", &work.join("installed"), most)
    }

    #[test]
    fn archives_are_checked_before_they_unpack() {
        let work = workshop("unpack");
        std::fs::write(work.join("source/plugin/tool"), "#!/bin/sh\n").expect("tool");
        let (good, sha256) = tar(&work, "good.tar.gz", &[], "tool");
        let partial = staged(&work, &good, &sha256.to_uppercase(), MAX_UNPACKED).expect("a good archive");
        assert_eq!(partial, work.join("installed.part"));
        assert!(partial.join("tool").is_file());
        // Only what the archive held: the download itself is gone.
        assert_eq!(std::fs::read_dir(&partial).expect("staged").count(), 1);
        // Another archive than the one published is not unpacked at all.
        let error = staged(&work, &good, &"0".repeat(64), MAX_UNPACKED).expect_err("a wrong checksum");
        assert!(error.contains("checksum does not match"), "{error}");
        assert!(!partial.exists());
        // Nor is one with a member that climbs out, even with the right
        // checksum: from installed.part, this one would land in `work`.
        std::fs::write(work.join("source/climbed"), "out").expect("the member");
        let (climbing, sha256) = tar(&work, "climbing.tar.gz", &["-P"], "../climbed");
        let error = staged(&work, &climbing, &sha256, MAX_UNPACKED).expect_err("an unsafe member");
        assert!(error.contains("unsafe path: ../climbed"), "{error}");
        assert!(!work.join("climbed").exists(), "the member landed outside");
        assert!(!partial.exists());
        let _ = std::fs::remove_dir_all(work);
    }

    #[test]
    fn unpacked_links_stay_inside() {
        let work = workshop("links");
        let plugin = work.join("source/plugin");
        std::fs::create_dir_all(plugin.join("lib")).expect("lib dir");
        std::fs::write(plugin.join("lib/tool.so"), "x").expect("a library");
        std::os::unix::fs::symlink("lib/tool.so", plugin.join("tool.so")).expect("a link inside");
        let (inside, sha256) = tar(&work, "inside.tar.gz", &[], ".");
        let partial = staged(&work, &inside, &sha256, MAX_UNPACKED).expect("links inside are fine");
        assert!(partial.join("tool.so").is_file());

        for (name, target) in
            [("home", "/home"), ("up", "../.."), ("nowhere", "lib/missing"), ("back", "../installed.part/lib/tool.so")]
        {
            std::os::unix::fs::symlink(target, plugin.join(name)).expect("a link");
            let (archive, sha256) = tar(&work, "outside.tar.gz", &[], ".");
            let error = staged(&work, &archive, &sha256, MAX_UNPACKED).expect_err("a link outside");
            assert!(error.contains("a link that points outside it") && error.contains(name), "{error}");
            assert!(!partial.exists(), "{name} stayed");
            std::fs::remove_file(plugin.join(name)).expect("remove the link");
        }
        // Out through another link, where each alone looks harmless.
        std::os::unix::fs::symlink("..", plugin.join("lib/back")).expect("a link to the top");
        std::os::unix::fs::symlink("lib/back/..", plugin.join("chained")).expect("a chained link");
        let (archive, sha256) = tar(&work, "chained.tar.gz", &[], ".");
        let error = staged(&work, &archive, &sha256, MAX_UNPACKED).expect_err("a chain outside");
        assert!(error.contains("chained"), "{error}");
        let _ = std::fs::remove_dir_all(work);
    }

    #[test]
    fn archives_that_unpack_too_large_are_refused() {
        let work = workshop("large");
        std::fs::write(work.join("source/plugin/tool"), vec![0; 300_000]).expect("zeros pack small");
        let (archive, sha256) = tar(&work, "large.tar.gz", &[], "tool");
        assert!(std::fs::metadata(&archive).expect("archive").len() < 10_000);
        let error = staged(&work, &archive, &sha256, 200_000).expect_err("too large");
        assert_eq!(error, "the archive unpacks to more than 0 MB");
        assert!(!work.join("installed.part").exists());
        assert!(staged(&work, &archive, &sha256, 300_000).is_ok());
        // The files on disk are counted too, whatever the archive said.
        assert!(refuse_unsafe_files(&work.join("installed.part"), 299_999).is_err());
        let _ = std::fs::remove_dir_all(work);
    }

    #[test]
    fn a_failed_download_leaves_what_was_installed() {
        let work = workshop("failed");
        let dir = work.join("installed");
        std::fs::create_dir_all(&dir).expect("installed dir");
        std::fs::write(dir.join("tool"), "old").expect("installed tool");
        std::fs::write(work.join("source/plugin/tool"), "new").expect("tool");
        let (archive, sha256) = tar(&work, "new.tar.gz", &[], "tool");
        let old = || std::fs::read_to_string(dir.join("tool")).expect("still there");
        // Cut off on the way, damaged, or not what was published.
        let cut = |_: &Path| Err("download failed: timed out".to_string());
        assert!(stage_from(cut, "tool.tar.gz", &sha256, "", &dir, MAX_UNPACKED).is_err());
        assert_eq!(old(), "old");
        std::fs::write(work.join("damaged"), "not gzip").expect("a damaged archive");
        let hash = sha256_of(&work.join("damaged")).expect("a checksum");
        let error = staged(&work, &work.join("damaged"), &hash, MAX_UNPACKED).expect_err("tar refuses it");
        assert!(error.contains("tar"), "{error}");
        assert!(staged(&work, &archive, &"0".repeat(64), MAX_UNPACKED).is_err());
        assert_eq!(old(), "old");
        assert!(!work.join("installed.part").exists());
        assert!(download("http://example.invalid/tool.tar.gz", &sha256, "tool", &dir).is_err());
        // Staging leaves it too; only placing what was staged replaces it.
        let partial = staged(&work, &archive, &sha256, MAX_UNPACKED).expect("staged");
        assert_eq!(old(), "old");
        assert_eq!(std::fs::read_to_string(partial.join("tool")).expect("staged tool"), "new");
        let _ = std::fs::remove_dir_all(work);
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
