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
}

impl Default for UpdatesConfig {
    fn default() -> Self {
        UpdatesConfig { enabled: true, notify: true, check_hours: 4 }
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
}

impl Task {
    fn title(&self) -> &'static str {
        match self {
            Task::Refresh => "Checking for updates",
            Task::Install(_) => "Installing updates",
            Task::Kernels { .. } => "Changing kernels",
            Task::Remove(_) => "Removing packages",
        }
    }

    fn args(&self) -> Result<Vec<String>, String> {
        match self {
            Task::Refresh => Ok(vec!["update".into()]),
            Task::Install(names) => apt::upgrade_args(names),
            Task::Kernels { install, remove, purge } => apt::transaction_args(install, remove, *purge, false),
            Task::Remove(names) => apt::purge_args(names, false),
        }
    }
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
    /// Simulate removing these packages.
    Preview(Vec<String>),
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
    },
    Preview(String),
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
    /// The number of updates last announced.
    announced: usize,
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
        ["upgrade"] => UpdatesCommand::Upgrade,
        _ => {
            return Err(
                "usage: updates [check|install [security|package...]|open [page]|show|kernels|clean|upgrade]".into()
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
                (Task::Refresh, _) | (_, 0) => "Checking".to_string(),
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
            Event::NeedsPassword(task) => {
                self.updates.job = None;
                self.open_updates_prompt(task, None);
            }
            Event::Progress { done, label } => {
                if let Some(job) = self.updates.job.as_mut() {
                    job.done = done;
                    job.label = label;
                }
            }
            Event::Finished { task, result } => self.updates_finished(task, result),
            Event::Preview(text) => self.updates.window.set_preview(text),
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
        let totals = self.updates.snapshot.totals();
        if totals.count > self.updates.announced
            && self.config.file.updates.enabled
            && self.config.file.updates.notify
            && !self.updates.busy()
        {
            let summary = if totals.count == 1 {
                "1 update available".to_string()
            } else {
                format!("{} updates available", totals.count)
            };
            let body = match totals.security {
                0 => "Click the count in the bar to install them.".to_string(),
                count => format!("{count} for security. Click the count in the bar to install them."),
            };
            self.notify_locally(&summary, &body, Urgency::Normal);
        }
        self.updates.announced = totals.count;
    }

    fn updates_finished(&mut self, task: Task, result: Result<(), Failure>) {
        self.updates.job = None;
        match result {
            Ok(()) => {
                let message = match &task {
                    Task::Refresh => "Package lists refreshed.".to_string(),
                    Task::Install(_) => "Updates installed.".to_string(),
                    Task::Kernels { .. } => "Kernel changes applied.".to_string(),
                    Task::Remove(names) if names.len() == 1 => "Removed 1 package.".to_string(),
                    Task::Remove(names) => format!("Removed {} packages.", names.len()),
                };
                if !matches!(task, Task::Refresh) {
                    self.notify_locally("Updates", &message, Urgency::Low);
                }
                self.updates.window.task_done(&task);
                self.updates.outcome = Some(Ok(message));
            }
            Err(Failure::Password) => self.open_updates_prompt(task, Some("That password did not work.".into())),
            Err(Failure::Failed(reason)) => {
                tracing::warn!("{}: {reason}", task.title());
                self.notify_locally(&format!("{} failed", task.title()), &reason, Urgency::Critical);
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
        task.args()?;
        let total = match &task {
            Task::Refresh => 0,
            Task::Install(names) if names.is_empty() => self.updates.snapshot.updates.len(),
            Task::Install(names) | Task::Remove(names) => names.len(),
            Task::Kernels { install, remove, .. } => install.len() + remove.len(),
        };
        let job = Job { task: task.clone(), done: 0, total, label: "starting".into() };
        if !self.updates_request(Request::Run { task, password }) {
            return Err("the update manager is not running".into());
        }
        self.updates.job = Some(job);
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

    /// Asks what `apt-get` would do with these arguments, which simulate.
    pub(crate) fn updates_preview(&mut self, args: Vec<String>) {
        self.updates_request(Request::Preview(args));
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
                    continue;
                }
                let offer = asked.elapsed() >= OFFER_EVERY;
                if offer {
                    asked = Instant::now();
                }
                watch.scan(offer)
            }
            Ok(Some(Request::Scan { offer })) => watch.scan(offer),
            Ok(Some(Request::Preview(names))) => Event::Preview(preview(&names)),
            Ok(Some(Request::Run { task, password })) => match authorise(password.as_deref().map(String::as_str)) {
                Access::Ask => Event::NeedsPassword(task),
                Access::Refused => Event::Finished { task, result: Err(Failure::Password) },
                Access::Impossible(reason) => Event::Finished { task, result: Err(Failure::Failed(reason)) },
                Access::Granted => {
                    let result = execute(&task, password.as_deref().map(String::as_str), events);
                    let refreshed = matches!(task, Task::Refresh);
                    if events.send(Event::Finished { task, result }).is_err() {
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

/// Runs a task's `apt-get` as root and reports its steps.
fn execute(task: &Task, password: Option<&str>, events: &channel::Sender<Event>) -> Result<(), Failure> {
    let args = task.args().map_err(Failure::Failed)?;
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
    let mut command = as_root(password);
    command.args(["apt-get", "-o", "DPkg::Lock::Timeout=60"]).args(&args).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = command.spawn().map_err(|error| Failure::Failed(format!("could not start apt-get: {error}")))?;
    feed(&mut child, password.filter(|_| !is_root()));
    let errors = child.stderr.take().map(|mut stderr| {
        thread::spawn(move || {
            let mut text = String::new();
            let _ = stderr.read_to_string(&mut text);
            text
        })
    });
    let mut done = 0;
    if let Some(stdout) = child.stdout.take() {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            let Some(progress) = apt::parse_progress(&line) else { continue };
            if matches!(progress, apt::Progress::Setup(_) | apt::Progress::Remove(_)) {
                done += 1;
            }
            if events.send(Event::Progress { done, label: progress.label() }).is_err() {
                break;
            }
        }
    }
    let status = child.wait().map_err(|error| Failure::Failed(error.to_string()))?;
    let errors = errors.and_then(|reader| reader.join().ok()).unwrap_or_default();
    if status.success() {
        if !automatic.is_empty() {
            let mut command = as_root(password);
            command.args(["apt-mark", "auto"]).args(&automatic).stdout(Stdio::null()).stderr(Stdio::null());
            let marked = command.spawn().and_then(|mut child| {
                feed(&mut child, password.filter(|_| !is_root()));
                child.wait()
            });
            if !marked.is_ok_and(|status| status.success()) {
                tracing::warn!("could not mark the installed updates as automatic again");
            }
        }
        return Ok(());
    }
    let lines = || errors.lines().map(str::trim).filter(|line| !line.is_empty());
    let reason = lines()
        .filter_map(|line| line.strip_prefix("E: "))
        .next()
        .or_else(|| lines().next_back())
        .unwrap_or("apt-get failed")
        .to_string();
    Err(Failure::Failed(reason))
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
        assert!(parse(&["remove"]).is_err());
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
