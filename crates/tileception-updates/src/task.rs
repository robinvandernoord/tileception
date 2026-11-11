//! The changes the update manager makes to the system, each a fixed set of
//! programs with checked arguments. A task is run as root: in the helper
//! that polkit starts, or in Tileception itself when that runs as root.

use std::io::{BufRead, BufReader, Read};
use std::process::{Command, Stdio};

use crate::apt;

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
        })
    }

    /// Whether the task changes packages, so that the computer must stay on
    /// until it ends. Refreshing the lists can be cut short.
    pub fn changes_packages(&self) -> bool {
        !matches!(self, Task::Refresh)
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
            _ => return Err("usage: refresh | change install|remove|autoremove|repair|kernels ...".into()),
        };
        // Refuses what is not a package name, and an empty removal.
        task.commands()?;
        Ok(task)
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
    fn failures_are_told_in_apts_words() {
        assert_eq!(reason("W: something\nE: Unable to lock\nE: second\n"), "Unable to lock");
        assert_eq!(reason("dpkg: error\n  \n"), "dpkg: error");
        assert_eq!(reason(""), "the command failed");
    }
}
