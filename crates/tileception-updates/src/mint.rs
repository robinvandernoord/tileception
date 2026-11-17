//! Linux Mint's Update Manager integration. Keep its blacklist in place
//! throughout activation: the vendor upgrade helper resets it after starting
//! the timer, so it cannot safely enable an existing installation.

use crate::{Files, Runner, automatic};

const HELPER: &str = "/usr/bin/mintupdate-automation";
const ENABLED: &str = "/var/lib/linuxmint/mintupdate-automatic-upgrades-enabled";
const OPTIONS: &str = "/etc/mintupdate-automatic-upgrades.conf";
const BLACKLIST: &str = "/etc/mintupdate.blacklist";
const DEFAULT_BLACKLIST: &str = "/usr/share/linuxmint/mintupdate/automation/blacklist.default";
const TIMER: &str = "mintupdate-automation-upgrade.timer";
const SERVICE: &str = "mintupdate-automation-upgrade.service";
const CLEANUP: &str = "mintupdate-automation-cleanup.service";
const ACTIVATING: &str = "/var/lib/linuxmint/tileception-updates-activation-pending";
const ACTIVATION_CONDITION: &str =
    "/etc/systemd/system/mintupdate-automation-upgrade.service.d/99-tileception-activation.conf";

/// Mint's Software Sources file contains both Mint and Ubuntu repositories.
pub(crate) const SOURCES: &str = "/etc/apt/sources.list.d/official-package-repositories.list";

pub(crate) fn available(files: &Files) -> bool {
    files.exists(HELPER)
}

/// Query failures must not be mistaken for a disabled timer. Runtime-only
/// enablement is insufficient: automatic upgrades must survive a reboot.
struct TimerState {
    active: bool,
    installed: String,
}

fn timer_state(runner: &dyn Runner) -> Result<TimerState, String> {
    let output =
        runner.run("/bin/systemctl", &["show", "--property=ActiveState", "--property=UnitFileState", TIMER])?;
    let state = |key: &str| {
        output.lines().find_map(|line| line.strip_prefix(key)).ok_or_else(|| format!("systemctl did not report {key}"))
    };
    let active = state("ActiveState=")?;
    let installed = state("UnitFileState=")?;
    if !matches!(active, "active" | "inactive" | "failed") {
        return Err(format!("Update Manager's timer is in a transitional state: {active}"));
    }
    if !matches!(installed, "enabled" | "enabled-runtime" | "disabled" | "masked" | "masked-runtime") {
        return Err(format!("Update Manager's timer has an unsupported installation state: {installed}"));
    }
    Ok(TimerState { active: active == "active", installed: installed.to_string() })
}

fn timer_matches(runner: &dyn Runner, enable: bool) -> Result<bool, String> {
    let state = timer_state(runner)?;
    Ok(if enable {
        state.active && state.installed == "enabled"
    } else {
        !state.active && matches!(state.installed.as_str(), "disabled" | "masked" | "masked-runtime")
    })
}

/// Each non-comment line is one argument to mintupdate-cli.
fn covers(options: &str) -> Vec<String> {
    let options: Vec<&str> =
        options.lines().map(str::trim).filter(|line| !line.is_empty() && !line.starts_with('#')).collect();
    let has = |short: &str, long: &str| options.iter().any(|option| *option == short || *option == long);
    let mut covers = vec![if has("-s", "--only-security") {
        "security updates".to_string()
    } else if has("-k", "--only-kernel") {
        "kernel updates".to_string()
    } else {
        "every update".to_string()
    }];
    covers.push("except packages in /etc/mintupdate.blacklist".into());
    covers.push("export Update Manager's ignored packages to that system blacklist first".into());
    covers
}

pub(crate) fn read(runner: &dyn Runner, files: &Files, unattended: automatic::Automatic) -> automatic::Automatic {
    let mut covers = covers(&files.read(OPTIONS).unwrap_or_default());
    let marked = files.exists(ENABLED);
    let enabled = if files.exists(ACTIVATING) {
        covers.push("Update Manager's activation is incomplete; upgrades are blocked, turn on to retry".into());
        false
    } else if marked {
        match timer_matches(runner, true) {
            Ok(true) => true,
            Ok(false) => {
                covers.push("Update Manager's switch is on but its timer needs repair; turn on to retry".into());
                false
            }
            Err(error) => {
                covers.push(format!("could not verify Update Manager's timer: {error}"));
                marked
            }
        }
    } else {
        false
    };
    if unattended.enabled {
        covers.push(format!(
            "unattended-upgrades is also enabled ({}) and will be disabled when this setting changes",
            unattended.covers.join(", ")
        ));
    }
    automatic::Automatic {
        tool: automatic::Tool::Mint,
        available: true,
        enabled: enabled || unattended.enabled,
        covers,
    }
}

/// Publish a complete default blacklist without replacing any existing file.
/// A failed write must not leave a partial blacklist for the next activation.
fn prepare_blacklist(files: &Files) -> Result<(), String> {
    let path = files.path(BLACKLIST);
    match std::fs::read_to_string(&path) {
        Ok(_) => return Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(format!("could not read {BLACKLIST}: {error}")),
    }
    let default = std::fs::read_to_string(files.path(DEFAULT_BLACKLIST))
        .map_err(|error| format!("could not read {DEFAULT_BLACKLIST}: {error}"))?;
    publish(&path, &default)
}

/// Publish a complete file without overwriting an existing path.
fn publish(path: &std::path::Path, contents: &str) -> Result<(), String> {
    use std::io::Write;

    let name = path.file_name().ok_or("file has no name")?.to_string_lossy();
    let temporary = path.with_file_name(format!(".{name}.tileception-{}", std::process::id()));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)
        .map_err(|error| format!("could not create {}: {error}", temporary.display()))?;
    let result = (|| -> std::io::Result<()> {
        file.write_all(contents.as_bytes())?;
        file.sync_all()?;
        // Unlike rename, this refuses to overwrite a concurrently created file
        // or a dangling symlink. Both names are on the same filesystem.
        std::fs::hard_link(&temporary, path)?;
        std::fs::File::open(path.parent().ok_or_else(|| std::io::Error::other("file has no directory"))?)?.sync_all()
    })();
    let cleanup = std::fs::remove_file(&temporary);
    result.and(cleanup).map_err(|error| format!("could not initialize {}: {error}", path.display()))
}

fn command(runner: &dyn Runner, args: &[&str], line: &mut dyn FnMut(&str)) -> Result<(), String> {
    line(&format!("$ /bin/systemctl {}", args.join(" ")));
    for printed in runner.run("/bin/systemctl", args)?.lines() {
        line(printed);
    }
    Ok(())
}

/// The timer may run while activation is checked, but its service must not.
/// Keep the condition and pending file persistent so an interrupted activation
/// also remains blocked after reboot. Removing the pending file commits it.
struct ActivationGuard {
    path: std::path::PathBuf,
    _lock: std::fs::File,
    recovering: bool,
}

impl ActivationGuard {
    fn acquire(runner: &dyn Runner, files: &Files, line: &mut dyn FnMut(&str)) -> Result<Self, String> {
        use std::os::unix::fs::MetadataExt;

        let path = files.path(ACTIVATING);
        let parent = path.parent().ok_or("activation guard has no directory")?;
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        // A live activation owns this lock. A crashed one leaves the condition
        // blocked but releases the lock, allowing an explicit retry to recover.
        let (lock, recovering) = match std::fs::OpenOptions::new().read(true).write(true).create_new(true).open(&path) {
            Ok(lock) => (lock, false),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                let lock = std::fs::OpenOptions::new()
                    .read(true)
                    .write(true)
                    .open(&path)
                    .map_err(|error| format!("could not open {ACTIVATING}: {error}"))?;
                (lock, true)
            }
            Err(error) => return Err(format!("could not create {ACTIVATING}: {error}")),
        };
        lock.try_lock().map_err(|error| format!("could not lock {ACTIVATING}: {error}"))?;
        let locked = lock.metadata().map_err(|error| error.to_string())?;
        let named = std::fs::metadata(&path).map_err(|error| error.to_string())?;
        // Another activation may have committed between open and try_lock.
        // Never continue with an unlinked guard that the condition cannot see.
        if locked.dev() != named.dev() || locked.ino() != named.ino() {
            return Err("Update Manager's activation guard changed; retry activation".into());
        }
        lock.sync_all().map_err(|error| error.to_string())?;
        std::fs::File::open(parent).and_then(|directory| directory.sync_all()).map_err(|error| error.to_string())?;
        let guard = Self { path, _lock: lock, recovering };
        let result = (|| {
            let condition = files.path(ACTIVATION_CONDITION);
            let parent = condition.parent().ok_or("activation condition has no directory")?;
            std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
            let text = format!("[Unit]\nConditionPathExists=!{ACTIVATING}\n");
            match std::fs::read_to_string(&condition) {
                Ok(existing) if existing == text => {}
                Ok(_) => return Err(format!("{ACTIVATION_CONDITION} has unexpected contents")),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => publish(&condition, &text)?,
                Err(error) => return Err(format!("could not read {ACTIVATION_CONDITION}: {error}")),
            }
            command(runner, &["daemon-reload"], line)?;
            let active = runner.run("/bin/systemctl", &["show", "--property=ActiveState", "--value", SERVICE])?;
            if !matches!(active.trim(), "inactive" | "failed") {
                return Err("Update Manager is already upgrading; retry after it finishes".into());
            }
            Ok(())
        })();
        if let Err(error) = result {
            if guard.recovering {
                return Err(error);
            }
            return match guard.release() {
                Ok(()) => Err(error),
                Err(failure) => Err(format!("{error}; releasing activation guard failed: {failure}")),
            };
        }
        Ok(guard)
    }

    fn release(&self) -> Result<(), String> {
        std::fs::remove_file(&self.path).map_err(|error| format!("could not remove {ACTIVATING}: {error}"))
    }
}

impl TimerState {
    fn restore(&self, runner: &dyn Runner, line: &mut dyn FnMut(&str)) -> Result<(), String> {
        if !self.active {
            command(runner, &["stop", TIMER], line)?;
        }
        match self.installed.as_str() {
            "enabled" => command(runner, &["enable", TIMER], line)?,
            "enabled-runtime" => {
                command(runner, &["disable", TIMER], line)?;
                command(runner, &["enable", "--runtime", TIMER], line)?;
            }
            "disabled" => command(runner, &["disable", TIMER], line)?,
            "masked" => command(runner, &["mask", TIMER], line)?,
            "masked-runtime" => command(runner, &["mask", "--runtime", TIMER], line)?,
            _ => return Err("cannot restore an unsupported timer installation state".into()),
        }
        if self.active {
            command(runner, &["start", TIMER], line)?;
        }
        let restored = timer_state(runner)?;
        if restored.active != self.active || restored.installed != self.installed {
            return Err("Update Manager's previous timer state was not restored".into());
        }
        Ok(())
    }
}

/// Retire the old backend before enabling Mint. The caller restores its APT
/// configuration if either retirement or the subsequent activation fails.
pub(crate) fn set(
    runner: &dyn Runner,
    files: &Files,
    enable: bool,
    line: &mut dyn FnMut(&str),
    retire: impl FnOnce(&mut dyn FnMut(&str)) -> Result<(), String>,
) -> Result<(), String> {
    let marker = files.path(ENABLED);
    let guard = if enable { Some(ActivationGuard::acquire(runner, files, line)?) } else { None };
    let mut previous = None;
    let mut created_marker = false;
    let mut changed_timer = false;
    let result = (|| {
        if enable {
            previous = Some(timer_state(runner)?);
            prepare_blacklist(files)?;
            command(runner, &["enable", CLEANUP], line)?;
        } else {
            // Check the stop/disable result directly. Mint's helper ignores it.
            command(runner, &["disable", "--now", TIMER], line)?;
            if !timer_matches(runner, false)? {
                return Err("Update Manager's timer did not turn off".into());
            }
            match std::fs::remove_file(&marker) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(format!("could not remove {ENABLED}: {error}")),
            }
        }
        // No new Mint activation may run while retirement can still fail.
        retire(line)?;
        if enable {
            changed_timer = true;
            command(runner, &["enable", TIMER], line)?;
            let parent = marker.parent().ok_or("Update Manager's switch has no directory")?;
            std::fs::create_dir_all(parent)
                .map_err(|error| format!("could not create {}: {error}", parent.display()))?;
            match std::fs::OpenOptions::new().write(true).create_new(true).open(&marker) {
                Ok(file) => {
                    created_marker = true;
                    file.sync_all().map_err(|error| format!("could not save {ENABLED}: {error}"))?;
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists && marker.is_file() => {}
                Err(error) => return Err(format!("could not create {ENABLED}: {error}")),
            }
            command(runner, &["start", TIMER], line)?;
            if !timer_matches(runner, true)? {
                return Err("Update Manager's timer did not turn on".into());
            }
        }
        // This is the commit point. All fallible activation checks and APT
        // migration have finished before the upgrade service is allowed to run.
        if let Some(guard) = &guard {
            guard.release()?;
        }
        Ok(())
    })();
    if let Err(error) = result {
        let mut failures = Vec::new();
        if created_marker && let Err(error) = std::fs::remove_file(&marker) {
            failures.push(format!("could not remove {ENABLED}: {error}"));
        }
        if changed_timer
            && let Some(previous) = previous
            && let Err(error) = previous.restore(runner, line)
        {
            failures.push(error);
        }
        if guard.is_some()
            && let Err(error) = command(runner, &["stop", SERVICE], line)
        {
            // No upgrade was running when the guard was acquired. While its
            // condition is installed, stopping only drains skipped/queued jobs.
            failures.push(error);
        }
        // If rollback fails, retain the guard so no newly triggered upgrade
        // can run while the caller restores unattended-upgrades.
        if failures.is_empty()
            && let Some(guard) = &guard
            && !guard.recovering
            && let Err(error) = guard.release()
        {
            failures.push(error);
        }
        if !failures.is_empty() {
            return Err(format!("{error}; activation rollback failed: {}", failures.join("; ")));
        }
        return Err(error);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mint_says_what_it_installs() {
        assert_eq!(covers("")[0], "every update");
        assert_eq!(covers("# comment\n--only-security\n")[0], "security updates");
        assert_eq!(covers("-k\n")[0], "kernel updates");
    }
}
