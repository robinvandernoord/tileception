//! The update manager: pending APT updates as a bar chip with a popover, an
//! Updates window (updates, kernels, the next Ubuntu release and the apt
//! history) and launcher rows. Flatpak, Snap and firmware are not read yet.
//!
//! A worker thread reads the system and runs `apt-get`; what to show and do
//! about it is decided here, on the compositor thread. Changes run as root:
//! directly when Tileception does, else through `sudo`, with the password
//! from Tileception's own prompt when `sudo` needs one.

mod popover;
mod prompt;
mod window;

use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Command as Process, Stdio};
use std::sync::mpsc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime};

use serde::{Deserialize, Serialize};
use smithay::reexports::calloop::{RegistrationToken, channel};
use tileception_updates::{self as backend, Files, Snapshot, apt, kernels, release};
use zeroize::Zeroizing;

use crate::Tileception;
use tileception_notify::Urgency;

pub(crate) use window::Page;

/// The `[updates]` section.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct UpdatesConfig {
    /// Look for updates and show them in the bar.
    pub enabled: bool,
    /// Say so once when new updates arrive.
    pub notify: bool,
    /// Hours between looks at the package lists.
    pub check_hours: u32,
    /// Days updates wait before the notification; 0 says so at once.
    pub remind_after_days: u32,
    /// Only security updates bring a notification.
    pub remind_security_only: bool,
}

impl Default for UpdatesConfig {
    fn default() -> Self {
        UpdatesConfig { enabled: true, notify: true, check_hours: 4, remind_after_days: 0, remind_security_only: false }
    }
}

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
    /// Remove packages that are no longer needed.
    Autoremove,
    /// Finish what `dpkg` left half done, then settle broken dependencies.
    Repair,
}

impl Task {
    fn title(&self) -> &'static str {
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
    fn commands(&self) -> Result<Vec<Vec<String>>, String> {
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
    fn changes_packages(&self) -> bool {
        !matches!(self, Task::Refresh)
    }
}

/// What the last task printed, kept until the next one ends.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Log {
    pub(crate) title: &'static str,
    pub(crate) succeeded: bool,
    pub(crate) text: String,
}

/// Why a task did not finish.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Failure {
    /// `sudo` did not take the password.
    Password,
    Failed(String),
}

/// What one look at the system found.
pub struct Scan {
    snapshot: Snapshot,
    /// The next release, when this look asked for it.
    offer: Option<release::Offer>,
    third_party: Vec<String>,
    free: Option<u64>,
    track: release::Track,
    snapshots: bool,
}

enum Request {
    Scan {
        offer: bool,
    },
    Run {
        task: Task,
        password: Option<Zeroizing<String>>,
    },
    /// Simulate a transaction, keeping its originating request identifier.
    Preview {
        request: u64,
        args: Vec<String>,
    },
}

pub enum Event {
    Scanned(Box<Scan>),
    /// `sudo` wants a password before this can run.
    NeedsPassword(Task),
    Progress {
        done: usize,
        label: String,
    },
    Finished {
        task: Task,
        result: Result<(), Failure>,
        /// Everything the task's programs printed.
        log: String,
    },
    Preview {
        request: u64,
        text: String,
    },
    /// A minute went by: a reminder that had to wait may be due.
    Tick,
}

/// The task being run.
struct Job {
    task: Task,
    done: usize,
    total: usize,
    label: String,
}

/// How the bar chip looks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Tone {
    Accent,
    Warning,
    Busy,
}

/// What the compositor knows about updates and is doing about them.
#[derive(Default)]
pub struct Updates {
    pub(crate) snapshot: Snapshot,
    /// The kernels of the snapshot, in the view the window shows.
    pub(crate) kernels: Vec<kernels::Kernel>,
    /// The first look has come back.
    scanned: bool,
    offer: release::Offer,
    third_party: Vec<String>,
    free: Option<u64>,
    track: release::Track,
    snapshots: bool,
    job: Option<Job>,
    /// How the last task ended, until the next one starts.
    outcome: Option<Result<String, String>>,
    requests: Option<mpsc::Sender<Option<Request>>>,
    thread: Option<JoinHandle<()>>,
    source: Option<RegistrationToken>,
    /// What the last task printed.
    log: Option<Log>,
    /// The last failure asked for a repair.
    repair: bool,
    /// The last task failed and the failure was not put away yet.
    pub(crate) failed: bool,
    /// The number of updates last announced.
    announced: usize,
    /// Since when updates have been waiting.
    waiting: Option<SystemTime>,
    /// The power action that follows once the running task ends.
    after: Option<crate::power::PowerAction>,
    /// This instance may run the update manager at all.
    allowed: bool,
    pub(crate) popover: popover::Popover,
    pub(crate) prompt: Option<prompt::Prompt>,
    pub(crate) window: window::Window,
}

impl Drop for Updates {
    fn drop(&mut self) {
        if let Some(requests) = self.requests.take() {
            let _ = requests.send(None);
        }
        // An apt transaction must not be cut short: the thread is left to finish.
        drop(self.thread.take());
    }
}

/// `tileception msg updates ...`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdatesCommand {
    Status,
    Check,
    /// These packages, or all of them for none.
    Install(Vec<String>),
    Security,
    Open(Page),
    /// The popover under the bar chip.
    Show,
    Kernels,
    Clean,
    /// Remove packages that are no longer needed.
    Autoremove,
    /// Repair a package system that apt says is broken.
    Repair,
    /// What the last task printed.
    Log,
    Upgrade,
}

pub fn parse(args: &[&str]) -> Result<UpdatesCommand, String> {
    Ok(match args {
        [] | ["status"] => UpdatesCommand::Status,
        ["check"] => UpdatesCommand::Check,
        ["install"] => UpdatesCommand::Install(Vec::new()),
        ["install", "security"] => UpdatesCommand::Security,
        ["install", names @ ..] => UpdatesCommand::Install(names.iter().map(|name| (*name).to_string()).collect()),
        ["open"] => UpdatesCommand::Open(Page::Available),
        ["open", page] => UpdatesCommand::Open(
            Page::parse(page)
                .ok_or_else(|| format!("unknown page {page:?}: use available, kernels, release or history"))?,
        ),
        ["show"] => UpdatesCommand::Show,
        ["kernels"] => UpdatesCommand::Kernels,
        ["clean"] => UpdatesCommand::Clean,
        ["autoremove"] => UpdatesCommand::Autoremove,
        ["repair"] => UpdatesCommand::Repair,
        ["log"] => UpdatesCommand::Log,
        ["upgrade"] => UpdatesCommand::Upgrade,
        _ => {
            return Err(
                "usage: updates [check|install [security|package...]|open [page]|show|kernels|clean|autoremove|repair|log|upgrade]"
                    .into()
            );
        }
    })
}

impl Updates {
    pub(crate) fn busy(&self) -> bool {
        self.job.is_some()
    }

    /// The running task: its title, what it is doing and how far it is.
    pub(crate) fn progress(&self) -> Option<(&'static str, &str, f64)> {
        let job = self.job.as_ref()?;
        let fraction = if job.total == 0 { 0.0 } else { (job.done as f64 / job.total as f64).clamp(0.0, 1.0) };
        Some((job.task.title(), &job.label, fraction))
    }

    pub(crate) fn outcome(&self) -> Option<&Result<String, String>> {
        self.outcome.as_ref()
    }

    /// Whether the last task failed and the failure still shows.
    pub(crate) fn failed(&self) -> bool {
        self.failed && matches!(self.outcome, Some(Err(_)))
    }

    /// What the last task printed.
    pub(crate) fn log(&self) -> Option<&Log> {
        self.log.as_ref()
    }

    /// Whether apt asked for a repair: after the last failure, or when it
    /// could not even list the updates.
    pub(crate) fn repairable(&self) -> bool {
        self.repair || self.snapshot.error.as_deref().is_some_and(apt::needs_repair)
    }

    /// Whether the computer must stay on: a task is changing packages.
    pub(crate) fn holds_power(&self) -> bool {
        self.job.as_ref().is_some_and(|job| job.task.changes_packages())
    }

    /// The power action that waits for the running task.
    pub(crate) fn power_after(&self) -> Option<crate::power::PowerAction> {
        self.after
    }

    /// The next release, when there is one.
    pub(crate) fn release(&self) -> Option<&str> {
        match &self.offer {
            release::Offer::Release(name) => Some(name),
            _ => None,
        }
    }

    pub(crate) fn offer(&self) -> &release::Offer {
        &self.offer
    }

    pub(crate) fn track(&self) -> release::Track {
        self.track
    }

    /// Packages to purge to clean up: old kernels, then leftovers.
    pub(crate) fn cleanup(&self) -> Vec<String> {
        let old: Vec<String> = kernels::removable(&self.kernels).iter().map(|kernel| kernel.package.clone()).collect();
        let mut names = kernels::removal(&self.snapshot.packages, &self.snapshot.running, &old).unwrap_or_default();
        for package in kernels::leftovers(&self.snapshot.packages) {
            if !names.contains(&package.name) {
                names.push(package.name.clone());
            }
        }
        names
    }

    /// "2 old kernels, 1.4 GB", when clean-up would free something.
    pub(crate) fn cleanup_summary(&self) -> Option<String> {
        let names = self.cleanup();
        if names.is_empty() {
            return None;
        }
        let old = kernels::removable(&self.kernels).len();
        let size = backend::human_size(kernels::size_of(&self.snapshot.packages, &names));
        Some(match old {
            0 => format!("Leftover kernel packages, {size}"),
            1 => format!("1 old kernel, {size}"),
            count => format!("{count} old kernels, {size}"),
        })
    }

    /// The chip's text and look, when there is something to say.
    pub(crate) fn chip(&self) -> Option<(String, Tone)> {
        if let Some(job) = &self.job {
            let label = match (&job.task, job.total) {
                (Task::Refresh, _) => "Checking".to_string(),
                (Task::Repair, _) => "Repairing".to_string(),
                (_, 0) => "Working".to_string(),
                (_, total) => format!("{} / {total}", job.done.min(total)),
            };
            return Some((label, Tone::Busy));
        }
        let totals = self.snapshot.totals();
        if totals.security > 0 {
            let rest = totals.count - totals.security;
            let label = if rest == 0 { totals.security.to_string() } else { format!("{} +{rest}", totals.security) };
            return Some((label, Tone::Warning));
        }
        if totals.count > 0 {
            return Some((totals.count.to_string(), Tone::Accent));
        }
        if self.snapshot.restart {
            return Some(("Restart".into(), Tone::Accent));
        }
        self.release().map(|name| (name.to_string(), Tone::Accent))
    }

    pub(crate) fn checked(&self) -> String {
        match self.snapshot.checked {
            Some(when) => match crate::ui::ago(when).as_str() {
                "now" => "checked just now".into(),
                ago => format!("checked {ago} ago"),
            },
            None => "not checked yet".into(),
        }
    }
}

impl Tileception {
    /// Starts looking for updates, on systems managed with APT.
    pub fn start_updates(&mut self) {
        self.updates.allowed = true;
        if !self.config.file.updates.enabled || !backend::supported() || self.updates.requests.is_some() {
            return;
        }
        let (sender, receiver) = channel::channel::<Event>();
        let token = self.loop_handle.insert_source(receiver, |event, _, state| {
            if let channel::Event::Msg(event) = event {
                state.updates_event(event);
            }
        });
        let token = match token {
            Ok(token) => token,
            Err(error) => return tracing::error!("could not watch for updates: {error}"),
        };
        let interval = Duration::from_secs(u64::from(self.config.file.updates.check_hours.clamp(1, 168)) * 3600);
        let (requests, incoming) = mpsc::channel();
        let thread = thread::Builder::new()
            .name("tileception-updates".into())
            .spawn(move || run(&sender, &incoming, interval))
            .map_err(|error| tracing::error!("could not start the updates worker: {error}"))
            .ok();
        self.updates.source = Some(token);
        self.updates.waiting = read_waiting();
        self.updates.log = read_log();
        self.updates.requests = Some(requests);
        self.updates.thread = thread;
    }

    fn updates_request(&self, request: Request) -> bool {
        self.updates.requests.as_ref().is_some_and(|requests| requests.send(Some(request)).is_ok())
    }

    /// Whether the update manager runs here and `[updates]` has it on.
    pub(crate) fn updates_available(&self) -> bool {
        self.updates.requests.is_some() && self.config.file.updates.enabled
    }

    /// After the config was loaded again: starts the update manager when
    /// `[updates]` switched it on, and puts its windows away when off.
    pub(crate) fn updates_follow_config(&mut self) {
        if !self.updates.allowed {
            return;
        }
        if self.config.file.updates.enabled {
            self.start_updates();
        } else {
            self.close_updates_popover();
            self.close_updates_prompt();
            if self.updates.window.open {
                self.close_updates();
            }
        }
    }

    fn updates_event(&mut self, event: Event) {
        match event {
            Event::Scanned(scan) => self.updates_scanned(*scan),
            Event::Tick => return self.updates_remind(),
            Event::NeedsPassword(task) => {
                self.updates.job = None;
                self.updates.after = None;
                self.open_updates_prompt(task, None);
            }
            Event::Progress { done, label } => {
                if let Some(job) = self.updates.job.as_mut() {
                    job.done = done;
                    job.label = label;
                }
            }
            Event::Finished { task, result, log } => self.updates_finished(task, result, log),
            Event::Preview { request, text } => self.updates.window.set_preview(request, text),
        }
        self.updates_changed();
    }

    /// Lists the kernels again, after a look at the system or a change of
    /// view, and drops queue entries for kernels that are gone.
    pub(crate) fn updates_refresh_kernels(&mut self) {
        let updates = &mut self.updates;
        updates.kernels = updates.snapshot.kernels(updates.window.all_variants());
        updates.window.settle(&updates.snapshot, &updates.kernels);
    }

    /// Something the chip, the popover or the window shows has changed.
    pub(crate) fn updates_changed(&mut self) {
        self.updates.window.invalidate();
        if self.launcher.open {
            self.refresh_updates_rows();
        }
        self.schedule_render();
    }

    fn updates_scanned(&mut self, scan: Scan) {
        let first = !self.updates.scanned;
        self.updates.scanned = true;
        self.updates.snapshot = scan.snapshot;
        self.updates.third_party = scan.third_party;
        self.updates.free = scan.free;
        self.updates.track = scan.track;
        self.updates.snapshots = scan.snapshots;
        if let Some(offer) = scan.offer {
            self.updates.offer = offer;
        }
        if let Some(error) = &self.updates.snapshot.error
            && first
        {
            tracing::warn!("could not list updates: {error}");
        }
        self.updates_refresh_kernels();
        let waiting = match (
            self.updates.snapshot.error.is_none(),
            self.updates.snapshot.updates.is_empty(),
            self.updates.waiting,
        ) {
            (true, true, _) => None,
            (true, false, None) => Some(SystemTime::now()),
            (_, _, since) => since,
        };
        if waiting != self.updates.waiting {
            self.updates.waiting = waiting;
            write_waiting(waiting);
        }
        self.updates_remind();
    }

    /// Says once that updates wait, when `[updates]` wants it said: at once
    /// or after they waited some days, for every update or only for security.
    /// Never over a fullscreen window or during Do not disturb; a reminder
    /// held back that way comes with a later look.
    fn updates_remind(&mut self) {
        let config = &self.config.file.updates;
        let totals = self.updates.snapshot.totals();
        let count = if config.remind_security_only { totals.security } else { totals.count };
        if count <= self.updates.announced || !config.enabled || !config.notify || self.updates.busy() {
            self.updates.announced = count;
            return;
        }
        let waited = self.updates.waiting.and_then(|since| since.elapsed().ok()).unwrap_or_default();
        if !reminder_due(waited, config.remind_after_days) || self.do_not_disturb() || self.fullscreen.is_some() {
            return;
        }
        let summary = match (config.remind_security_only, count) {
            (false, 1) => "1 update available".to_string(),
            (false, count) => format!("{count} updates available"),
            (true, 1) => "1 security update available".to_string(),
            (true, count) => format!("{count} security updates available"),
        };
        let body = match totals.security {
            count if count > 0 && !config.remind_security_only => {
                format!("{count} for security. Click the count in the bar to install them.")
            }
            _ => "Click the count in the bar to install them.".to_string(),
        };
        self.notify_locally(&summary, &body, Urgency::Normal);
        self.updates.announced = count;
        self.schedule_render();
    }

    fn updates_finished(&mut self, task: Task, result: Result<(), Failure>, log: String) {
        self.updates.job = None;
        let after = self.updates.after.take();
        if !matches!(result, Err(Failure::Password)) && !log.trim().is_empty() {
            let log = Log { title: task.title(), succeeded: result.is_ok(), text: log };
            write_log(&log);
            self.updates.repair = !log.succeeded && apt::needs_repair(&log.text);
            self.updates.log = Some(log);
        }
        match result {
            Ok(()) => {
                let message = match &task {
                    Task::Refresh => "Package lists refreshed.".to_string(),
                    Task::Install(_) => "Updates installed.".to_string(),
                    Task::Kernels { .. } => "Kernel changes applied.".to_string(),
                    Task::Remove(names) if names.len() == 1 => "Removed 1 package.".to_string(),
                    Task::Remove(names) => format!("Removed {} packages.", names.len()),
                    Task::Autoremove => "Unneeded packages removed.".to_string(),
                    Task::Repair => "Packages repaired.".to_string(),
                };
                if !matches!(task, Task::Refresh) {
                    self.notify_locally("Updates", &message, Urgency::Low);
                }
                self.updates.window.task_done(&task);
                self.updates.outcome = Some(Ok(message));
                if let Some(action) = after {
                    self.power_menu = None;
                    self.run_power_action(action);
                }
            }
            Err(Failure::Password) => self.open_updates_prompt(task, Some("That password did not work.".into())),
            Err(Failure::Failed(reason)) => {
                tracing::warn!("{}: {reason}", task.title());
                let reason = match after {
                    Some(action) => format!("{reason} {} was called off.", action.label()),
                    None => reason,
                };
                self.notify_locally(&format!("{} failed", task.title()), &reason, Urgency::Critical);
                self.updates.failed = true;
                self.updates.outcome = Some(Err(reason));
            }
        }
    }

    /// Runs a task, unless one is running already.
    pub(crate) fn updates_start(&mut self, task: Task) -> Result<(), String> {
        self.updates_run(task, None)
    }

    pub(crate) fn updates_run(&mut self, task: Task, password: Option<Zeroizing<String>>) -> Result<(), String> {
        if self.updates.busy() {
            return Err("another update task is running".into());
        }
        task.commands()?;
        let total = match &task {
            Task::Refresh | Task::Repair => 0,
            Task::Autoremove => self.updates.snapshot.plan.unneeded.len(),
            Task::Install(names) if names.is_empty() => self.updates.snapshot.updates.len(),
            Task::Install(names) | Task::Remove(names) => names.len(),
            Task::Kernels { install, remove, .. } => install.len() + remove.len(),
        };
        let job = Job { task: task.clone(), done: 0, total, label: "starting".into() };
        if !self.updates_request(Request::Run { task, password }) {
            return Err("the update manager is not running".into());
        }
        self.updates.job = Some(job);
        self.updates.after = None;
        self.updates.failed = false;
        self.updates.outcome = None;
        self.updates_changed();
        Ok(())
    }

    /// Installs every update, or only those for security.
    pub(crate) fn updates_install(&mut self, security_only: bool) -> Result<(), String> {
        let updates = &self.updates.snapshot.updates;
        if updates.is_empty() {
            return Err("there are no updates to install".into());
        }
        let names: Vec<String> = if security_only {
            updates.iter().filter(|update| update.security).map(|update| update.name.clone()).collect()
        } else {
            Vec::new()
        };
        if security_only && names.is_empty() {
            return Err("there are no security updates".into());
        }
        self.updates_start(Task::Install(names))
    }

    /// A power action chosen while packages change: it waits for the task to
    /// end and follows by itself, and choosing it again calls it off. Returns
    /// whether the action was held back.
    pub(crate) fn updates_hold_power(&mut self, action: crate::power::PowerAction) -> bool {
        use crate::power::PowerAction;
        let waits = matches!(
            action,
            PowerAction::LogOut
                | PowerAction::Suspend
                | PowerAction::Restart
                | PowerAction::PowerOff
                | PowerAction::Uefi
        );
        if !waits || !self.updates.holds_power() {
            return false;
        }
        self.updates.after = if self.updates.after == Some(action) { None } else { Some(action) };
        self.schedule_render();
        true
    }

    /// Asks what `apt-get` would do with these arguments, which simulate.
    pub(crate) fn updates_preview(&mut self, request: u64, args: Vec<String>) {
        self.updates_request(Request::Preview { request, args });
    }

    /// What to look at before a release upgrade.
    pub(crate) fn updates_release_checks(&self) -> Vec<release::Check> {
        let totals = self.updates.snapshot.totals();
        release::checks(&release::Readiness {
            free: self.updates.free,
            pending: totals.count,
            security: totals.security,
            third_party: self.updates.third_party.clone(),
            on_battery: self.battery.reading.map(|reading| reading.charge == crate::battery::Charge::Discharging),
            snapshots: self.updates.snapshots,
            restart: self.updates.snapshot.restart,
        })
    }

    /// Starts the release upgrade in a terminal, where it asks its questions.
    pub(crate) fn updates_upgrade_release(&mut self) -> Result<(), String> {
        let Some(name) = self.updates.release().map(str::to_string) else {
            return Err("no newer release is offered".into());
        };
        if !release::ready(&self.updates_release_checks()) {
            return Err(format!("not ready for {name}: see `updates open release`"));
        }
        let sudo = if is_root() { "" } else { "sudo " };
        self.run_in_terminal(&format!("{sudo}do-release-upgrade; echo; echo 'Press Enter to close.'; read -r _"))
    }

    pub(crate) fn updates_command(&mut self, command: UpdatesCommand) -> Result<String, String> {
        if !self.updates_available() {
            return Err(if self.config.file.updates.enabled {
                "the update manager is not running (it needs apt-get)".into()
            } else {
                "updates are switched off in [updates]".into()
            });
        }
        match command {
            UpdatesCommand::Status => return Ok(self.updates_status()),
            UpdatesCommand::Check => self.updates_start(Task::Refresh)?,
            UpdatesCommand::Install(names) if names.is_empty() => self.updates_install(false)?,
            UpdatesCommand::Install(names) => {
                let pending = &self.updates.snapshot.updates;
                if let Some(unknown) = names.iter().find(|name| !pending.iter().any(|update| update.name == **name)) {
                    return Err(format!("{unknown} has no update waiting"));
                }
                self.updates_start(Task::Install(names))?;
            }
            UpdatesCommand::Security => self.updates_install(true)?,
            UpdatesCommand::Open(page) => self.open_updates(page),
            UpdatesCommand::Show => self.toggle_updates_popover(),
            UpdatesCommand::Kernels => return Ok(self.updates_kernels_status()),
            UpdatesCommand::Clean => {
                let names = self.updates.cleanup();
                if names.is_empty() {
                    return Err("there is nothing to clean up".into());
                }
                self.updates_start(Task::Remove(names))?;
            }
            UpdatesCommand::Autoremove => {
                if self.updates.snapshot.plan.unneeded.is_empty() {
                    return Err("there are no unneeded packages".into());
                }
                self.updates_start(Task::Autoremove)?;
            }
            UpdatesCommand::Repair => {
                if !self.updates.repairable() {
                    return Err("apt has not asked for a repair".into());
                }
                self.updates_start(Task::Repair)?;
            }
            UpdatesCommand::Log => {
                return match self.updates.log() {
                    Some(log) => Ok(format!(
                        "{} ({})\n{}\n",
                        log.title,
                        if log.succeeded { "finished" } else { "failed" },
                        log.text.trim_end()
                    )),
                    None => Err("no task has run yet".into()),
                };
            }
            UpdatesCommand::Upgrade => self.updates_upgrade_release()?,
        }
        Ok(String::new())
    }

    fn updates_status(&self) -> String {
        let updates = &self.updates;
        if !updates.scanned {
            return "looking for updates\n".into();
        }
        let totals = updates.snapshot.totals();
        let mut text = match (totals.count, totals.security) {
            (0, _) if updates.snapshot.error.is_some() => "updates could not be listed".to_string(),
            (0, _) => "up to date".to_string(),
            (count, 0) => format!("{count} updates, {}", backend::human_size(totals.bytes)),
            (count, security) => {
                format!("{count} updates, {security} for security, {}", backend::human_size(totals.bytes))
            }
        };
        text.push_str(&format!(" ({})\n", updates.checked()));
        if let Some((title, label, fraction)) = updates.progress() {
            text.push_str(&format!("{title}: {label} ({:.0}%)\n", fraction * 100.0));
        }
        match updates.outcome() {
            Some(Ok(message)) => text.push_str(&format!("last: {message}\n")),
            Some(Err(reason)) => text.push_str(&format!("last failed: {reason}\n")),
            None => {}
        }
        if let Some(error) = &updates.snapshot.error {
            text.push_str(&format!("error: {error}\n"));
        }
        if updates.snapshot.restart {
            text.push_str("restart needed to finish earlier updates\n");
        }
        if let Some(name) = updates.release() {
            text.push_str(&format!("release {name} is available\n"));
        }
        if let Some(summary) = updates.cleanup_summary() {
            text.push_str(&format!("clean-up: {summary}\n"));
        }
        if updates.repairable() {
            text.push_str("apt asks for a repair: `updates repair`\n");
        }
        if updates.holds_power() {
            match updates.power_after() {
                Some(action) => text.push_str(&format!("{} follows when the task ends\n", action.label())),
                None => text.push_str("restart, power off, suspend and log out wait for the task\n"),
            }
        }
        let plan = &updates.snapshot.plan;
        for (title, names) in [
            ("removed by a full install", &plan.removed),
            ("kept back", &plan.kept_back),
            ("phased, offered later", &plan.phased),
            ("no longer needed", &plan.unneeded),
        ] {
            if !names.is_empty() {
                text.push_str(&format!("{title}: {}\n", names.join(" ")));
            }
        }
        for update in &updates.snapshot.updates {
            let old = update.old.as_deref().unwrap_or("new");
            let mark = if update.security { " [security]" } else { "" };
            text.push_str(&format!("  {} {old} -> {}{mark}\n", update.name, update.new));
        }
        text
    }

    fn updates_kernels_status(&self) -> String {
        let found = &self.updates.kernels;
        if found.is_empty() {
            return format!("running {}, no kernel packages installed\n", self.updates.snapshot.running);
        }
        let keep = kernels::fallback(found).map(|kernel| kernel.package.clone());
        let mut text = String::new();
        for kernel in found {
            let state = if kernel.running {
                "running"
            } else if !kernel.installed {
                "available"
            } else if keep.as_deref() == Some(kernel.package.as_str()) {
                "fallback"
            } else {
                "old"
            };
            let size = if kernel.installed { backend::human_size(kernel.size * 1024) } else { String::new() };
            text.push_str(&format!("{:<28} {state:<10} {size}\n", kernel.identifier));
        }
        let leftovers = kernels::leftovers(&self.updates.snapshot.packages);
        if !leftovers.is_empty() {
            text.push_str(&format!("{} leftover packages\n", leftovers.len()));
        }
        text
    }
}

/// Whether Tileception itself runs as root, as in a container.
pub(crate) fn is_root() -> bool {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata("/proc/self").is_ok_and(|meta| meta.uid() == 0)
}

/// Whether the release offer is asked for again: once a day.
const OFFER_EVERY: Duration = Duration::from_secs(24 * 3600);

fn scan(with_offer: bool) -> Scan {
    let files = Files::default();
    let runner = backend::System;
    let mut snapshot = backend::scan(&runner, &files);
    // Restarting is easier to explain than logging out and in again.
    snapshot.restart |= replaced();
    Scan {
        snapshot,
        offer: with_offer.then(|| release::offer(&runner)),
        third_party: release::third_party(&files),
        free: release::free_space(&runner),
        track: release::track(&files),
        snapshots: files.exists("/usr/bin/timeshift"),
    }
}

/// How often the worker says that time passed, for reminders that waited.
const TICK_EVERY: Duration = Duration::from_secs(60);

/// How often the worker looks whether apt's files changed.
const WATCH_EVERY: Duration = Duration::from_secs(10);

/// When apt's package lists and dpkg's record of what is installed last
/// changed: any install, removal or refresh moves one of them.
fn stamp() -> [Option<SystemTime>; 3] {
    ["/var/lib/dpkg/status", "/var/lib/apt/lists", "/var/cache/apt/pkgcache.bin"]
        .map(|path| std::fs::metadata(path).and_then(|meta| meta.modified()).ok())
}

/// What the last scan saw of apt's files, and when it looked.
struct Watch {
    seen: [Option<SystemTime>; 3],
    looked: Instant,
}

impl Default for Watch {
    fn default() -> Self {
        Watch { seen: stamp(), looked: Instant::now() }
    }
}

impl Watch {
    fn scan(&mut self, with_offer: bool) -> Event {
        // Stamped first: a change during the scan brings another one.
        *self = Watch::default();
        Event::Scanned(Box::new(scan(with_offer)))
    }
}

fn run(events: &channel::Sender<Event>, incoming: &mpsc::Receiver<Option<Request>>, interval: Duration) {
    let mut asked = Instant::now();
    let mut ticked = Instant::now();
    let mut watch = Watch::default();
    if events.send(watch.scan(true)).is_err() {
        return;
    }
    loop {
        let event = match incoming.recv_timeout(WATCH_EVERY) {
            Err(mpsc::RecvTimeoutError::Timeout) => {
                // Something else ran apt, in a terminal for one: look again
                // at once instead of showing the old list until the next look.
                if watch.looked.elapsed() < interval && watch.seen == stamp() {
                    if ticked.elapsed() >= TICK_EVERY {
                        ticked = Instant::now();
                        if events.send(Event::Tick).is_err() {
                            return;
                        }
                    }
                    continue;
                }
                let offer = asked.elapsed() >= OFFER_EVERY;
                if offer {
                    asked = Instant::now();
                }
                watch.scan(offer)
            }
            Ok(Some(Request::Scan { offer })) => watch.scan(offer),
            Ok(Some(Request::Preview { request, args })) => Event::Preview { request, text: preview(&args) },
            Ok(Some(Request::Run { task, password })) => match authorise(password.as_deref().map(String::as_str)) {
                Access::Ask => Event::NeedsPassword(task),
                Access::Refused => Event::Finished { task, result: Err(Failure::Password), log: String::new() },
                Access::Impossible(reason) => {
                    Event::Finished { task, result: Err(Failure::Failed(reason)), log: String::new() }
                }
                Access::Granted => {
                    let mut log = String::new();
                    let result = execute(&task, password.as_deref().map(String::as_str), events, &mut log);
                    let refreshed = matches!(task, Task::Refresh);
                    if events.send(Event::Finished { task, result, log }).is_err() {
                        return;
                    }
                    if refreshed {
                        asked = Instant::now();
                    }
                    watch.scan(refreshed)
                }
            },
            Ok(None) | Err(mpsc::RecvTimeoutError::Disconnected) => return,
        };
        if events.send(event).is_err() {
            return;
        }
    }
}

fn preview(args: &[String]) -> String {
    if !args.iter().any(|arg| arg == "-s") {
        return "Only simulations are previewed.".into();
    }
    // Errors belong in a preview too: what apt refuses is what matters most.
    let output = Process::new("apt-get").args(args).env("LC_ALL", "C").stdin(Stdio::null()).output();
    match output {
        Ok(output) => {
            let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
            text.push_str(&String::from_utf8_lossy(&output.stderr));
            text
        }
        Err(error) => format!("apt-get: {error}"),
    }
}

enum Access {
    Granted,
    /// There is no way to become root from here.
    Impossible(String),
    /// A password is needed and none was given.
    Ask,
    /// The password was wrong.
    Refused,
}

/// Feeds a password to a `sudo -S` on its standard input.
fn feed(child: &mut std::process::Child, password: Option<&str>) {
    if let (Some(password), Some(mut stdin)) = (password, child.stdin.take()) {
        let _ = stdin.write_all(password.as_bytes());
        let _ = stdin.write_all(b"\n");
    }
}

fn authorise(password: Option<&str>) -> Access {
    if is_root() {
        return Access::Granted;
    }
    if Process::new("sudo").arg("-V").stdout(Stdio::null()).stderr(Stdio::null()).status().is_err() {
        return Access::Impossible("Changing packages needs sudo, which is not installed.".into());
    }
    let quiet = |command: &mut Process| {
        command.stdout(Stdio::null()).stderr(Stdio::null());
    };
    match password {
        None => {
            let mut command = Process::new("sudo");
            command.args(["-n", "true"]).stdin(Stdio::null());
            quiet(&mut command);
            if command.status().is_ok_and(|status| status.success()) { Access::Granted } else { Access::Ask }
        }
        Some(password) => {
            let mut command = Process::new("sudo");
            command.args(["-S", "-k", "-p", "", "true"]).stdin(Stdio::piped());
            quiet(&mut command);
            let Ok(mut child) = command.spawn() else { return Access::Refused };
            feed(&mut child, Some(password));
            if child.wait().is_ok_and(|status| status.success()) { Access::Granted } else { Access::Refused }
        }
    }
}

/// A command that runs what is added to it as root, through `sudo` when
/// Tileception is not root itself; `feed` it the password once spawned.
fn as_root(password: Option<&str>) -> Process {
    let mut command = if is_root() {
        Process::new("env")
    } else {
        let mut command = Process::new("sudo");
        match password {
            // -k: always read the password, so it never reaches the program.
            Some(_) => command.args(["-S", "-k", "-p", ""]),
            None => command.arg("-n"),
        };
        command.args(["--", "env"]);
        command
    };
    command.args(["DEBIAN_FRONTEND=noninteractive", "LC_ALL=C"]).stdin(if password.is_some() && !is_root() {
        Stdio::piped()
    } else {
        Stdio::null()
    });
    command
}

/// Whether the Tileception that is running is no longer the one installed:
/// a package update replaced its program, which `/proc` then calls deleted.
fn replaced() -> bool {
    std::fs::read_link("/proc/self/exe").is_ok_and(|path| path.to_string_lossy().ends_with(" (deleted)"))
}

/// Runs a command as root to its end, feeding `sudo` the password.
fn finish(mut command: Process, password: Option<&str>) -> bool {
    command.stdout(Stdio::null()).stderr(Stdio::null());
    command
        .spawn()
        .and_then(|mut child| {
            feed(&mut child, password.filter(|_| !is_root()));
            child.wait()
        })
        .is_ok_and(|status| status.success())
}

/// What goes before a program so that logind refuses to shut down, restart
/// or sleep until it ends: nothing where logind takes no such lock, as in a
/// container.
fn inhibitor(title: &str, password: Option<&str>) -> Vec<String> {
    let lock: Vec<String> =
        ["systemd-inhibit", "--what=shutdown:sleep", "--who=Tileception", &format!("--why={title}"), "--mode=block"]
            .iter()
            .map(|part| (*part).to_string())
            .collect();
    let mut probe = as_root(password);
    probe.args(&lock).arg("true");
    if finish(probe, password) { lock } else { Vec::new() }
}

/// The folder for what the update manager remembers between sessions.
fn state_file(name: &str) -> std::path::PathBuf {
    let root = std::env::var_os("XDG_STATE_HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".local/state"));
    root.join("tileception").join(name)
}

/// Since when updates have been waiting, across restarts of Tileception.
fn read_waiting() -> Option<SystemTime> {
    let seconds = std::fs::read_to_string(state_file("updates-waiting")).ok()?.trim().parse().ok()?;
    Some(SystemTime::UNIX_EPOCH + Duration::from_secs(seconds))
}

fn write_waiting(since: Option<SystemTime>) {
    let path = state_file("updates-waiting");
    let seconds =
        since.and_then(|since| since.duration_since(SystemTime::UNIX_EPOCH).ok()).map(|since| since.as_secs());
    let written = match seconds {
        Some(seconds) => path
            .parent()
            .map_or(Ok(()), std::fs::create_dir_all)
            .and_then(|()| std::fs::write(&path, seconds.to_string())),
        None if path.exists() => std::fs::remove_file(&path),
        None => Ok(()),
    };
    if let Err(error) = written {
        tracing::warn!("could not save how long updates have been waiting: {error}");
    }
}

/// The most a kept log holds: the end of a longer one.
const LOG_LIMIT: usize = 512 * 1024;

/// The last task's output, kept beside the other state: the first line says
/// what ran and how it ended.
fn write_log(log: &Log) {
    let path = state_file("updates-last.log");
    let text = format!("{}\t{}\n{}", log.title, if log.succeeded { "ok" } else { "failed" }, log.text);
    let written = path.parent().map_or(Ok(()), std::fs::create_dir_all).and_then(|()| std::fs::write(&path, text));
    if let Err(error) = written {
        tracing::warn!("could not keep the log of the last update task: {error}");
    }
}

fn read_log() -> Option<Log> {
    let text =
        std::fs::read_to_string(state_file("updates-last.log")).ok().filter(|text| text.len() <= LOG_LIMIT * 2)?;
    let (first, text) = text.split_once('\n')?;
    let (title, state) = first.split_once('\t')?;
    let title = [Task::Refresh, Task::Install(Vec::new()), Task::Remove(Vec::new()), Task::Autoremove, Task::Repair]
        .iter()
        .map(Task::title)
        .find(|known| *known == title)
        .unwrap_or("Changing kernels");
    Some(Log { title, succeeded: state == "ok", text: text.to_string() })
}

/// Whether updates that have waited this long are due for their reminder.
fn reminder_due(waited: Duration, after_days: u32) -> bool {
    waited >= Duration::from_secs(u64::from(after_days) * 24 * 3600)
}

/// Runs a task's programs as root, reports their steps and keeps what they
/// print in `log`.
fn execute(
    task: &Task,
    password: Option<&str>,
    events: &channel::Sender<Event>,
    log: &mut String,
) -> Result<(), Failure> {
    let commands = task.commands().map_err(Failure::Failed)?;
    // Read before the install, which marks what it names as installed by hand.
    let automatic = match task {
        Task::Install(names) if !names.is_empty() => Process::new("apt-mark")
            .arg("showmanual")
            .env("LC_ALL", "C")
            .stdin(Stdio::null())
            .output()
            .ok()
            .filter(|output| output.status.success())
            .map(|output| apt::automatic(names, &String::from_utf8_lossy(&output.stdout)))
            .unwrap_or_default(),
        _ => Vec::new(),
    };
    let lock = if task.changes_packages() { inhibitor(task.title(), password) } else { Vec::new() };
    let mut done = 0;
    let mut result = Ok(());
    for arguments in &commands {
        log.push_str(&format!("$ {}\n", arguments.join(" ")));
        let mut command = as_root(password);
        command.args(&lock).args(arguments).stdout(Stdio::piped()).stderr(Stdio::piped());
        let mut child =
            command.spawn().map_err(|error| Failure::Failed(format!("could not start {}: {error}", arguments[0])))?;
        feed(&mut child, password.filter(|_| !is_root()));
        let errors = child.stderr.take().map(|mut stderr| {
            thread::spawn(move || {
                let mut text = String::new();
                let _ = stderr.read_to_string(&mut text);
                text
            })
        });
        if let Some(stdout) = child.stdout.take() {
            // Read to the end even when nobody listens: closing the pipe
            // would stop apt halfway.
            let mut listening = true;
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                log.push_str(&line);
                log.push('\n');
                let Some(progress) = apt::parse_progress(&line) else { continue };
                if matches!(progress, apt::Progress::Setup(_) | apt::Progress::Remove(_)) {
                    done += 1;
                }
                listening = listening && events.send(Event::Progress { done, label: progress.label() }).is_ok();
            }
        }
        let status = child.wait().map_err(|error| Failure::Failed(error.to_string()))?;
        let errors = errors.and_then(|reader| reader.join().ok()).unwrap_or_default();
        log.push_str(&errors);
        result = if status.success() {
            Ok(())
        } else {
            let lines = || errors.lines().map(str::trim).filter(|line| !line.is_empty());
            let reason = lines()
                .filter_map(|line| line.strip_prefix("E: "))
                .next()
                .or_else(|| lines().next_back())
                .unwrap_or("the command failed")
                .to_string();
            Err(Failure::Failed(reason))
        };
    }
    if log.len() > LOG_LIMIT {
        let mut start = log.len() - LOG_LIMIT;
        while !log.is_char_boundary(start) {
            start += 1;
        }
        log.replace_range(..start, "...\n");
    }
    if result.is_ok() && !automatic.is_empty() {
        let mut command = as_root(password);
        command.args(["apt-mark", "auto"]).args(&automatic);
        if !finish(command, password) {
            tracing::warn!("could not mark the installed updates as automatic again");
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn update(name: &str, security: bool) -> apt::Update {
        apt::Update {
            name: name.into(),
            old: Some("1".into()),
            new: "2".into(),
            origin: String::new(),
            security,
            kind: apt::Kind::System,
            size: 1_000_000,
        }
    }

    #[test]
    fn commands_parse() {
        assert_eq!(parse(&[]), Ok(UpdatesCommand::Status));
        assert_eq!(parse(&["install"]), Ok(UpdatesCommand::Install(Vec::new())));
        assert_eq!(parse(&["install", "security"]), Ok(UpdatesCommand::Security));
        assert_eq!(parse(&["install", "curl", "apt"]), Ok(UpdatesCommand::Install(vec!["curl".into(), "apt".into()])));
        assert_eq!(parse(&["open", "kernels"]), Ok(UpdatesCommand::Open(Page::Kernels)));
        assert!(parse(&["open", "nowhere"]).is_err());
        assert_eq!(parse(&["autoremove"]), Ok(UpdatesCommand::Autoremove));
        assert_eq!(parse(&["repair"]), Ok(UpdatesCommand::Repair));
        assert_eq!(parse(&["log"]), Ok(UpdatesCommand::Log));
        assert!(parse(&["remove"]).is_err());
    }

    #[test]
    fn reminders_wait_their_days() {
        let day = Duration::from_secs(24 * 3600);
        assert!(reminder_due(Duration::ZERO, 0));
        assert!(!reminder_due(day * 2, 3));
        assert!(reminder_due(day * 3, 3));
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
    fn power_waits_only_while_packages_change() {
        let mut updates = Updates::default();
        assert!(!updates.holds_power());
        updates.job = Some(Job { task: Task::Refresh, done: 0, total: 0, label: String::new() });
        assert!(!updates.holds_power());
        updates.job = Some(Job { task: Task::Autoremove, done: 0, total: 2, label: String::new() });
        assert!(updates.holds_power());
    }

    #[test]
    fn the_chip_counts_security_first() {
        let mut updates = Updates::default();
        assert_eq!(updates.chip(), None);
        updates.snapshot.updates = vec![update("apt", false), update("curl", false)];
        assert_eq!(updates.chip(), Some(("2".into(), Tone::Accent)));
        updates.snapshot.updates.push(update("openssl", true));
        assert_eq!(updates.chip(), Some(("1 +2".into(), Tone::Warning)));
        updates.job = Some(Job { task: Task::Install(Vec::new()), done: 1, total: 3, label: String::new() });
        assert_eq!(updates.chip(), Some(("1 / 3".into(), Tone::Busy)));
        updates.job = None;
        updates.snapshot.updates.clear();
        updates.snapshot.restart = true;
        assert_eq!(updates.chip(), Some(("Restart".into(), Tone::Accent)));
        updates.snapshot.restart = false;
        updates.offer = release::Offer::Release("26.04 LTS".into());
        assert_eq!(updates.chip(), Some(("26.04 LTS".into(), Tone::Accent)));
    }
}
