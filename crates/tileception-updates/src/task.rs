//! The changes the update manager makes to the system, each a fixed set of
//! programs with checked arguments. A task is run as root: in the helper
//! that polkit starts, or in Tileception itself when that runs as root.

use std::io::{BufRead, BufReader, Read};
use std::process::{Command, Stdio};

use crate::{Files, apps, apt, automatic, firmware, mirrors, snapshots};

/// Where the helper is installed. The polkit policy names this path.
pub const HELPER: &str = "/usr/libexec/tileception-updates-helper";

/// The polkit policy that lets the helper run.
pub const POLICY: &str = "/usr/share/polkit-1/actions/org.tileception.updates.policy";

/// What the helper writes before the reason a task failed.
pub const FAILED: &str = "tileception-updates-helper: ";

/// A change to the system, run as root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Task {
    /// Refresh the package lists.
    Refresh,
    /// Install these updates, or every one for none.
    Install(Vec<String>),
    /// The kernel queue: packages to install and to remove in one
    /// transaction, purging the configuration of what goes or keeping it.
    Kernels { install: Vec<String>, remove: Vec<String>, purge: bool },
    /// Remove exactly these packages.
    Remove(Vec<String>),
    /// Remove the dependencies nothing needs any more.
    Autoremove,
    /// Finish what `dpkg` left half done, then settle broken dependencies.
    Repair,
    /// Update these Flatpaks, of the user's installation or the system's.
    Flatpak { refs: Vec<String>, user: bool },
    /// Refresh these snaps.
    Snap(Vec<String>),
    /// Flash the newest firmware for these devices.
    Firmware(Vec<String>),
    /// Hold these packages at their version, or let them update again.
    Hold { names: Vec<String>, hold: bool },
    /// Turn the periodic run of `unattended-upgrades` on or off.
    Automatic(bool),
    /// Use this mirror for the release and its updates, then refresh.
    Mirror(String),
}

impl Task {
    pub fn title(&self) -> &'static str {
        match self {
            Task::Refresh => "Checking for updates",
            Task::Install(_) => "Installing updates",
            Task::Kernels { .. } => "Changing kernels",
            Task::Remove(_) => "Removing packages",
            Task::Autoremove => "Removing unneeded packages",
            Task::Repair => "Repairing packages",
            Task::Flatpak { .. } => "Updating Flatpaks",
            Task::Snap(_) => "Refreshing snaps",
            Task::Firmware(_) => "Updating firmware",
            Task::Hold { hold: true, .. } => "Holding packages",
            Task::Hold { hold: false, .. } => "Releasing held packages",
            Task::Automatic(true) => "Turning on automatic updates",
            Task::Automatic(false) => "Turning off automatic updates",
            Task::Mirror(_) => "Changing the mirror",
        }
    }

    /// Every title a task can have, to read a kept log back.
    pub const TITLES: [&'static str; 14] = [
        "Checking for updates",
        "Installing updates",
        "Changing kernels",
        "Removing packages",
        "Removing unneeded packages",
        "Repairing packages",
        "Updating Flatpaks",
        "Refreshing snaps",
        "Updating firmware",
        "Holding packages",
        "Releasing held packages",
        "Turning on automatic updates",
        "Turning off automatic updates",
        "Changing the mirror",
    ];

    /// Whether the task needs root. A user's own Flatpaks are theirs to update.
    pub fn needs_root(&self) -> bool {
        !matches!(self, Task::Flatpak { user: true, .. })
    }

    /// How many things it changes, for a progress count; 0 when unknown.
    pub fn count(&self) -> usize {
        match self {
            Task::Install(names) | Task::Remove(names) | Task::Snap(names) | Task::Firmware(names) => names.len(),
            Task::Hold { names, .. } => names.len(),
            Task::Flatpak { refs, .. } => refs.len(),
            Task::Kernels { install, remove, .. } => install.len() + remove.len(),
            _ => 0,
        }
    }

    /// The programs to run as root, in order, each with its arguments. The
    /// last one decides whether the task succeeded.
    pub fn commands(&self) -> Result<Vec<Vec<String>>, String> {
        let apt = |args: Vec<String>| {
            let mut command = vec!["apt-get".to_string(), "-o".into(), "DPkg::Lock::Timeout=60".into()];
            command.extend(args);
            command
        };
        Ok(match self {
            Task::Refresh => vec![apt(vec!["update".into()])],
            Task::Install(names) => vec![apt(apt::upgrade_args(names, false)?)],
            Task::Kernels { install, remove, purge } => {
                vec![apt(apt::transaction_args(install, remove, *purge, false)?)]
            }
            Task::Remove(names) => vec![apt(apt::purge_args(names, false)?)],
            Task::Autoremove => vec![apt(apt::autoremove_args(false))],
            Task::Repair => vec![
                ["dpkg", "--force-confdef", "--force-confold", "--configure", "-a"].map(String::from).to_vec(),
                apt(apt::repair_args(false)),
            ],
            Task::Flatpak { refs, user } => {
                checked(refs, apps::valid_flatpak, "a Flatpak")?;
                let mut command: Vec<String> = ["flatpak", "update", "-y", "--noninteractive"].map(String::from).into();
                command.push(if *user { "--user" } else { "--system" }.into());
                command.extend(refs.iter().cloned());
                vec![command]
            }
            Task::Snap(names) => {
                checked(names, apps::valid_snap, "a snap")?;
                let mut command: Vec<String> = ["snap", "refresh"].map(String::from).into();
                command.extend(names.iter().cloned());
                vec![command]
            }
            Task::Firmware(ids) => {
                checked(ids, firmware::valid_device, "a device")?;
                // One device at a time: fwupdmgr takes one.
                ids.iter()
                    .map(|device| {
                        ["fwupdmgr", "update", "--assume-yes", "--no-reboot-check", device.as_str()]
                            .map(String::from)
                            .to_vec()
                    })
                    .collect()
            }
            Task::Hold { names, hold } => {
                checked(names, apt::valid_name, "a package")?;
                let mut command = vec!["apt-mark".to_string(), if *hold { "hold" } else { "unhold" }.to_string()];
                command.extend(names.iter().cloned());
                vec![command]
            }
            // The file is written by `run`; nothing else runs.
            Task::Automatic(_) => Vec::new(),
            Task::Mirror(url) => {
                if !mirrors::valid(url) {
                    return Err(format!("{url:?} is not a mirror address"));
                }
                vec![apt(vec!["update".into()])]
            }
        })
    }

    /// Whether the task changes packages, so that the computer must stay on
    /// until it ends. Refreshing the lists can be cut short.
    pub fn changes_packages(&self) -> bool {
        !matches!(self, Task::Refresh | Task::Hold { .. } | Task::Automatic(_) | Task::Mirror(_))
    }

    /// The helper's arguments for this task. The first one is what the
    /// polkit policy tells apart: `refresh` needs no password, `change` does.
    pub fn to_args(&self) -> Vec<String> {
        let change = |kind: &str, rest: Vec<String>| {
            let mut args = vec!["change".to_string(), kind.to_string()];
            args.extend(rest);
            args
        };
        match self {
            Task::Refresh => vec!["refresh".into()],
            Task::Install(names) => change("install", names.clone()),
            Task::Kernels { install, remove, purge } => {
                let mut rest = vec![if *purge { "purge" } else { "keep" }.to_string()];
                rest.extend(install.iter().map(|name| format!("+{name}")));
                rest.extend(remove.iter().map(|name| format!("-{name}")));
                change("kernels", rest)
            }
            Task::Remove(names) => change("remove", names.clone()),
            Task::Autoremove => change("autoremove", Vec::new()),
            Task::Repair => change("repair", Vec::new()),
            Task::Flatpak { refs, user } => {
                let mut rest = vec![if *user { "user" } else { "system" }.to_string()];
                rest.extend(refs.iter().cloned());
                change("flatpak", rest)
            }
            Task::Snap(names) => change("snap", names.clone()),
            Task::Firmware(ids) => change("firmware", ids.clone()),
            Task::Hold { names, hold } => change(if *hold { "hold" } else { "unhold" }, names.clone()),
            Task::Automatic(enable) => change("automatic", vec![if *enable { "on" } else { "off" }.to_string()]),
            Task::Mirror(url) => change("mirror", vec![url.clone()]),
        }
    }

    /// The task the helper was asked for. Nothing but these forms is taken,
    /// and every package name is checked before anything runs.
    pub fn from_args(args: &[String]) -> Result<Task, String> {
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        let owned = |names: &[&str]| names.iter().map(|name| (*name).to_string()).collect::<Vec<_>>();
        let task = match args.as_slice() {
            ["refresh"] => Task::Refresh,
            ["change", "install", names @ ..] => Task::Install(owned(names)),
            ["change", "remove", names @ ..] => Task::Remove(owned(names)),
            ["change", "autoremove"] => Task::Autoremove,
            ["change", "repair"] => Task::Repair,
            ["change", "flatpak", which @ ("system" | "user"), refs @ ..] if !refs.is_empty() => {
                Task::Flatpak { refs: owned(refs), user: *which == "user" }
            }
            ["change", "snap", names @ ..] if !names.is_empty() => Task::Snap(owned(names)),
            ["change", "firmware", ids @ ..] if !ids.is_empty() => Task::Firmware(owned(ids)),
            ["change", which @ ("hold" | "unhold"), names @ ..] if !names.is_empty() => {
                Task::Hold { names: owned(names), hold: *which == "hold" }
            }
            ["change", "automatic", which @ ("on" | "off")] => Task::Automatic(*which == "on"),
            ["change", "mirror", url] => Task::Mirror((*url).to_string()),
            ["change", "kernels", mode @ ("purge" | "keep"), names @ ..] => {
                let mut install = Vec::new();
                let mut remove = Vec::new();
                for name in names {
                    match (name.strip_prefix('+'), name.strip_prefix('-')) {
                        (Some(name), _) => install.push(name.to_string()),
                        (_, Some(name)) => remove.push(name.to_string()),
                        _ => return Err(format!("{name:?} says neither install (+) nor remove (-)")),
                    }
                }
                Task::Kernels { install, remove, purge: *mode == "purge" }
            }
            _ => {
                return Err(
                    "usage: refresh | change install|remove|autoremove|repair|kernels|flatpak|snap|firmware|hold|unhold|automatic|mirror ..."
                        .into(),
                );
            }
        };
        // Refuses what is not a package name, and an empty removal.
        task.commands()?;
        Ok(task)
    }
}

/// Refuses a list with a name its tool would not take as one.
fn checked(names: &[String], valid: fn(&str) -> bool, what: &str) -> Result<(), String> {
    match names.iter().find(|name| !valid(name)) {
        Some(name) => Err(format!("{name:?} is not {what}")),
        None if names.is_empty() => Err("nothing to change".into()),
        None => Ok(()),
    }
}

/// Whether this process runs as root.
pub fn is_root() -> bool {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata("/proc/self").is_ok_and(|meta| meta.uid() == 0)
}

/// A program as root runs it: without questions, in the C locale so its
/// output parses.
fn program(arguments: &[String]) -> Option<Command> {
    let (name, rest) = arguments.split_first()?;
    let mut command = Command::new(name);
    command.args(rest).env("DEBIAN_FRONTEND", "noninteractive").env("LC_ALL", "C").stdin(Stdio::null());
    Some(command)
}

/// What goes before a program so that logind refuses to shut down, restart
/// or sleep until it ends: nothing where logind takes no such lock, as in a
/// container.
fn inhibitor(title: &str) -> Vec<String> {
    let lock: Vec<String> =
        ["systemd-inhibit", "--what=shutdown:sleep", "--who=Tileception", &format!("--why={title}"), "--mode=block"]
            .iter()
            .map(|part| (*part).to_string())
            .collect();
    let mut probe = lock.clone();
    probe.push("true".into());
    let works = program(&probe).is_some_and(|mut command| {
        command.stdout(Stdio::null()).stderr(Stdio::null()).status().is_ok_and(|status| status.success())
    });
    if works { lock } else { Vec::new() }
}

/// Why a program failed, from what it wrote to its standard error: apt's
/// first error, else the last line.
fn reason(errors: &str) -> String {
    let lines = || errors.lines().map(str::trim).filter(|line| !line.is_empty());
    lines()
        .filter_map(|line| line.strip_prefix("E: "))
        .next()
        .or_else(|| lines().next_back())
        .unwrap_or("the command failed")
        .to_string()
}

/// Runs a task's programs. This process must be root. Every line they print
/// goes to `line`, each command first as `$ command`; the error is why the
/// last program failed.
pub fn run(task: &Task, line: &mut dyn FnMut(&str)) -> Result<(), String> {
    let commands = task.commands()?;
    let files = Files::default();
    if let Task::Automatic(enable) = task {
        let text = automatic::rewrite(&files.read(automatic::CONFIG).unwrap_or_default(), *enable);
        line(&format!("Writing {}", automatic::CONFIG));
        return std::fs::write(files.path(automatic::CONFIG), text)
            .map_err(|error| format!("could not write {}: {error}", automatic::CONFIG));
    }
    // The sources as they were, to put back when the new mirror fails.
    let mut restore: Vec<(&str, String)> = Vec::new();
    if let Task::Mirror(url) = task {
        let current = mirrors::current(&files).ok_or("no Ubuntu mirror was found in the package sources")?;
        for path in [mirrors::SOURCES, mirrors::LIST] {
            let Some(text) = files.read(path) else { continue };
            let changed = mirrors::rewrite(&text, &current, url);
            if changed != text {
                line(&format!("{path}: {current} \u{2192} {url}"));
                std::fs::write(files.path(path), changed)
                    .map_err(|error| format!("could not write {path}: {error}"))?;
                restore.push((path, text));
            }
        }
    }
    if let Task::Install(names) = task {
        snapshot(names, line);
    }
    // Read before the install, which marks what it names as installed by hand.
    let automatic = match task {
        Task::Install(names) if !names.is_empty() => program(&["apt-mark".into(), "showmanual".into()])
            .and_then(|mut command| command.output().ok())
            .filter(|output| output.status.success())
            .map(|output| apt::automatic(names, &String::from_utf8_lossy(&output.stdout)))
            .unwrap_or_default(),
        _ => Vec::new(),
    };
    let lock = if task.changes_packages() { inhibitor(task.title()) } else { Vec::new() };
    let mut result = Ok(());
    for arguments in &commands {
        line(&format!("$ {}", arguments.join(" ")));
        let locked: Vec<String> = lock.iter().chain(arguments).cloned().collect();
        let mut command = program(&locked).ok_or("nothing to run")?;
        let mut child = command
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|error| format!("could not start {}: {error}", arguments[0]))?;
        let errors = child.stderr.take().map(|mut stderr| {
            std::thread::spawn(move || {
                let mut text = String::new();
                let _ = stderr.read_to_string(&mut text);
                text
            })
        });
        if let Some(stdout) = child.stdout.take() {
            // Read to the end whatever happens: closing the pipe would stop
            // apt halfway.
            for printed in BufReader::new(stdout).lines().map_while(Result::ok) {
                line(&printed);
            }
        }
        let status = child.wait().map_err(|error| error.to_string())?;
        let errors = errors.and_then(|reader| reader.join().ok()).unwrap_or_default();
        for printed in errors.lines() {
            line(printed);
        }
        result = if status.success() { Ok(()) } else { Err(reason(&errors)) };
        // A repair goes on after dpkg fails: settling dependencies may fix it.
        if result.is_err() && !matches!(task, Task::Repair) {
            break;
        }
    }
    if result.is_err() {
        for (path, text) in restore {
            line(&format!("Putting {path} back"));
            if let Err(error) = std::fs::write(files.path(path), text) {
                line(&format!("W: could not put {path} back: {error}"));
            }
        }
    }
    if result.is_ok() && !automatic.is_empty() {
        let mut arguments = vec!["apt-mark".to_string(), "auto".to_string()];
        arguments.extend(automatic);
        let marked = program(&arguments).is_some_and(|mut command| {
            command.stdout(Stdio::null()).stderr(Stdio::null()).status().is_ok_and(|status| status.success())
        });
        if !marked {
            line("W: could not mark the installed updates as automatic again");
        }
    }
    result
}

/// Takes a snapshot before an install that brings a kernel or Mesa, where
/// a tool for it is present. A failed snapshot is said, not fatal: it must
/// not keep security updates out.
fn snapshot(names: &[String], line: &mut dyn FnMut(&str)) {
    let Some(tool) = snapshots::tool() else { return };
    let names = if names.is_empty() {
        let simulation = program(&["apt-get".into(), "-s".into(), "dist-upgrade".into()])
            .and_then(|mut command| command.output().ok())
            .map(|output| String::from_utf8_lossy(&output.stdout).into_owned())
            .unwrap_or_default();
        apt::parse_simulation(&simulation).into_iter().map(|update| update.name).collect()
    } else {
        names.to_vec()
    };
    let Some(reason) = snapshots::reason(&names) else { return };
    line(&format!("Taking a snapshot with {} before {reason}", tool.name()));
    for arguments in tool.commands(reason, &snapshots::stamp()) {
        line(&format!("$ {}", arguments.join(" ")));
        let output = program(&arguments).and_then(|mut command| command.output().ok());
        let Some(output) = output else {
            line(&format!("W: could not start {}; installing without a snapshot", arguments[0]));
            return;
        };
        for printed in String::from_utf8_lossy(&output.stdout).lines() {
            line(printed);
        }
        if !output.status.success() {
            line(&format!("W: {}; installing without a snapshot", reason_of(&output.stderr)));
            return;
        }
    }
}

fn reason_of(errors: &[u8]) -> String {
    reason(&String::from_utf8_lossy(errors))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(args: &[&str]) -> Vec<String> {
        args.iter().map(|arg| (*arg).to_string()).collect()
    }

    #[test]
    fn tasks_survive_the_trip_to_the_helper() {
        let tasks = [
            Task::Refresh,
            Task::Install(Vec::new()),
            Task::Install(strings(&["curl", "libssl3t64"])),
            Task::Remove(strings(&["linux-image-6.8.0-85-generic"])),
            Task::Autoremove,
            Task::Repair,
            Task::Kernels {
                install: strings(&["linux-image-6.14.0-36-generic"]),
                remove: strings(&["linux-image-6.8.0-85-generic", "linux-headers-6.8.0-85"]),
                purge: true,
            },
            Task::Kernels { install: Vec::new(), remove: strings(&["linux-image-6.8.0-85-generic"]), purge: false },
            Task::Flatpak { refs: strings(&["org.mozilla.firefox//stable"]), user: true },
            Task::Flatpak { refs: strings(&["org.gnome.Platform//47"]), user: false },
            Task::Snap(strings(&["firefox", "core22"])),
            Task::Firmware(strings(&["362301da643102b9f38477387e2193e57abaa590"])),
            Task::Hold { names: strings(&["docker-ce"]), hold: true },
            Task::Hold { names: strings(&["docker-ce"]), hold: false },
            Task::Automatic(true),
            Task::Automatic(false),
            Task::Mirror("https://mirror.example.org/ubuntu".into()),
        ];
        for task in tasks {
            assert_eq!(Task::from_args(&task.to_args()), Ok(task));
        }
    }

    #[test]
    fn the_policy_tells_refreshing_from_changing() {
        assert_eq!(Task::Refresh.to_args(), ["refresh"]);
        for task in [Task::Install(Vec::new()), Task::Autoremove, Task::Repair, Task::Remove(strings(&["curl"]))] {
            assert_eq!(task.to_args()[0], "change");
        }
    }

    #[test]
    fn the_helper_takes_nothing_else() {
        for args in [
            &[][..],
            &["refresh", "now"],
            &["update"],
            &["change"],
            &["change", "install", "--allow-unauthenticated"],
            &["change", "install", "curl; reboot"],
            &["change", "remove"],
            &["change", "remove", "-o", "APT::Update::Pre-Invoke::=id"],
            &["change", "autoremove", "curl"],
            &["change", "kernels", "purge"],
            &["change", "kernels", "maybe", "+linux-image-6.14.0-36-generic"],
            &["change", "kernels", "keep", "linux-image-6.14.0-36-generic"],
            &["change", "kernels", "keep", "+-o"],
            &["apt-get", "install", "curl"],
            &["change", "flatpak", "system"],
            &["change", "flatpak", "both", "org.x"],
            &["change", "flatpak", "user", "--installation=x"],
            &["change", "snap"],
            &["change", "snap", "--classic"],
            &["change", "firmware", "--force"],
            &["change", "hold", "-o"],
            &["change", "automatic", "maybe"],
            &["change", "mirror", "file:///etc"],
            &["change", "mirror", "http://a", "http://b"],
        ] {
            assert!(Task::from_args(&strings(args)).is_err(), "{args:?} was taken");
        }
    }

    #[test]
    fn a_repair_finishes_dpkg_before_apt() {
        let commands = Task::Repair.commands().unwrap_or_default();
        assert_eq!(commands.len(), 2);
        assert_eq!(commands[0][0], "dpkg");
        assert_eq!(commands[1].last().map(String::as_str), Some("install"));
        assert!(commands[1].contains(&"--fix-broken".to_string()));
        assert!(Task::Repair.changes_packages() && Task::Autoremove.changes_packages());
        assert!(!Task::Refresh.changes_packages());
    }

    #[test]
    fn only_system_changes_need_root() {
        assert!(!Task::Flatpak { refs: strings(&["org.x"]), user: true }.needs_root());
        assert!(Task::Flatpak { refs: strings(&["org.x"]), user: false }.needs_root());
        assert!(Task::Snap(strings(&["firefox"])).needs_root());
        let firmware = Task::Firmware(strings(&[
            "362301da643102b9f38477387e2193e57abaa590",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        ]));
        assert_eq!(firmware.commands().map(|commands| commands.len()), Ok(2));
        assert!(Task::TITLES.contains(&Task::Mirror(String::new()).title()));
    }

    #[test]
    fn failures_are_told_in_apts_words() {
        assert_eq!(reason("W: something\nE: Unable to lock\nE: second\n"), "Unable to lock");
        assert_eq!(reason("dpkg: error\n  \n"), "dpkg: error");
        assert_eq!(reason(""), "the command failed");
    }
}
