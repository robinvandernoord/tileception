//! The update manager: pending APT, Flatpak, Snap and firmware updates as a
//! bar chip with a popover, an Updates window (updates, kernels, the next
//! Ubuntu release, the apt history, and the mirror and automatic updates)
//! and launcher rows. Flatpak and Snap ask their servers, so they are listed
//! when the network may be used and kept in between.
//!
//! A worker thread reads the system and runs `apt-get`; what to show and do
//! about it is decided here, on the compositor thread. Changes run as root
//! in `tileception-updates-helper`, which takes a task and never a command
//! line. Polkit starts it through `pkexec` under the helper's own policy:
//! refreshing the package lists needs no password, so it also happens by
//! itself, and everything else asks for an administrator through the
//! session's polkit agent. Without polkit or its agent the helper runs
//! through `sudo`, with the password from Tileception's prompt when `sudo`
//! needs one. When Tileception is root itself, the task runs in place.

mod popover;
mod window;

use std::collections::{BTreeMap, VecDeque};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command as Process, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime};

use serde::{Deserialize, Serialize};
use smithay::reexports::calloop::{RegistrationToken, channel};
pub use tileception_updates::task::Task;
use tileception_updates::{
    self as backend, Files, Snapshot, apps, apt, automatic, firmware, kernels, mirrors, release, snapshots, task,
};
use zeroize::Zeroizing;

use crate::{
    Tileception,
    password::{Prompt, Purpose},
};
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
    /// The person, or polkit, said no.
    Declined,
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
    /// What takes a snapshot before kernel, Mesa and release upgrades.
    snapshots: Option<snapshots::Tool>,
    automatic: automatic::Automatic,
    /// The mirror in use, from the package sources.
    mirror: Option<String>,
    /// The mirror in use tested against the main archive, when this look
    /// could use the network.
    mirror_check: Option<(mirrors::Mirror, Option<i64>)>,
}

enum Request {
    Scan {
        offer: bool,
        /// Flatpak and Snap may ask their servers.
        apps: bool,
    },
    /// What changed in an update, for the sheet that asked.
    Notes { request: u64, update: Box<apt::Update> },
    /// Tests the candidate mirrors.
    Mirrors,
    Run {
        task: Task,
        password: Option<Zeroizing<String>>,
        /// Started by itself: it never asks for a password and fails quietly.
        quiet: bool,
    },
    /// Simulate a transaction, keeping its originating request identifier.
    Preview { request: u64, args: Vec<String> },
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
        quiet: bool,
    },
    Preview {
        request: u64,
        text: String,
    },
    /// The candidate mirrors as tested, the main archive's date with them.
    Mirrors(Vec<mirrors::Mirror>, Option<i64>),
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
    snapshots: Option<snapshots::Tool>,
    /// The Flatpak and Snap updates last listed.
    apps: Vec<apt::Update>,
    /// Flatpak and Snap were asked at least once, so `apps` says what
    /// they have. Until then a skipped app is not missing, only unasked.
    apps_scanned: bool,
    /// The app sources the last look could not ask: `apps` keeps what
    /// they listed before, and their skips stand.
    apps_failed: Vec<apt::Source>,
    /// Updates whose version was skipped: they show muted and wait for the
    /// next version.
    pub(crate) skipped: Vec<apt::Update>,
    /// The update identities skipped, by source and name.
    skip: BTreeMap<apt::Key, String>,
    pub(crate) automatic: automatic::Automatic,
    pub(crate) mirrors: Mirrors,
    job: Option<Job>,
    /// Tasks that follow the running one when it succeeds: one Install
    /// covers APT, Flatpak, Snap and firmware, each its own task.
    next: VecDeque<Task>,
    /// What the earlier tasks of the chain printed.
    chain_log: String,
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
    /// When the package lists were last refreshed by themselves, or tried.
    refreshed: Option<Instant>,
    /// Told to the worker: nothing may use the network by itself now.
    saving: Arc<AtomicBool>,
    /// This instance may run the update manager at all.
    allowed: bool,
    pub(crate) popover: popover::Popover,
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

/// The mirror in use and the candidates, as last tested.
#[derive(Default)]
pub(crate) struct Mirrors {
    pub(crate) current: Option<String>,
    /// The mirror in use, tested, with the main archive's date.
    pub(crate) check: Option<(mirrors::Mirror, Option<i64>)>,
    pub(crate) candidates: Vec<mirrors::Mirror>,
    /// The main archive's date when the candidates were tested.
    pub(crate) reference: Option<i64>,
    pub(crate) testing: bool,
}

impl Mirrors {
    /// Why the mirror in use should change: unreachable or stale.
    pub(crate) fn warning(&self) -> Option<String> {
        let (mirror, reference) = self.check.as_ref()?;
        match &mirror.error {
            Some(error) => Some(format!("The mirror in use cannot be reached: {error}")),
            None if mirror.stale(*reference) => {
                Some(format!("The mirror in use is {}.", mirrors::describe_lag(mirror.lag(*reference))))
            }
            None => None,
        }
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
    /// What changed in an update.
    Changelog(String),
    /// Hold packages at their version, or release them.
    Hold(Vec<String>, bool),
    /// Skip the waiting version of an update, or stop skipping it.
    Skip(String, bool),
    /// Turn automatic updates on or off, or say how they are set up.
    Automatic(Option<bool>),
    /// Test the mirrors and list them.
    Mirrors,
    /// Use this mirror.
    Mirror(String),
    /// What apt changed, newest first: since a day (`2026-11-10`), or the
    /// latest transactions.
    History(Option<String>),
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
        ["changelog", name] => UpdatesCommand::Changelog((*name).to_string()),
        ["hold", names @ ..] if !names.is_empty() => {
            UpdatesCommand::Hold(names.iter().map(|name| (*name).to_string()).collect(), true)
        }
        ["unhold", names @ ..] if !names.is_empty() => {
            UpdatesCommand::Hold(names.iter().map(|name| (*name).to_string()).collect(), false)
        }
        ["skip", name] => UpdatesCommand::Skip((*name).to_string(), true),
        ["unskip", name] => UpdatesCommand::Skip((*name).to_string(), false),
        ["automatic"] => UpdatesCommand::Automatic(None),
        ["automatic", "on"] => UpdatesCommand::Automatic(Some(true)),
        ["automatic", "off"] => UpdatesCommand::Automatic(Some(false)),
        ["mirrors"] => UpdatesCommand::Mirrors,
        ["mirror", url] => UpdatesCommand::Mirror((*url).to_string()),
        ["history"] => UpdatesCommand::History(None),
        ["history", "since", day] | ["history", day] if valid_day(day) => {
            UpdatesCommand::History(Some((*day).to_string()))
        }
        _ => {
            return Err(
                "usage: updates [check|install [security|name...]|open [page]|show|kernels|clean|autoremove|repair|log|upgrade|changelog <name>|hold|unhold <package>...|skip|unskip <name>|automatic [on|off]|mirrors|mirror <url>|history [since YYYY-MM-DD]]"
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

    /// Whether a restart would install something: a kernel waits among the
    /// updates, or firmware that fwupd flashes while the computer starts.
    pub(crate) fn installs_on_restart(&self) -> bool {
        self.snapshot.updates.iter().any(|update| matches!(update.kind, apt::Kind::Kernel | apt::Kind::Firmware))
    }

    /// A muted row's update: the skipped versions, then the held packages.
    /// The flag says it is held.
    pub(crate) fn muted(&self, index: usize) -> Option<(&apt::Update, bool)> {
        match self.skipped.get(index) {
            Some(update) => Some((update, false)),
            None => self.snapshot.held.get(index - self.skipped.len()).map(|update| (update, true)),
        }
    }

    /// The update by its name, waiting, skipped or held. See [`pick`].
    pub(crate) fn find(&self, name: &str) -> Result<&apt::Update, String> {
        pick(self.snapshot.updates.iter().chain(&self.skipped).chain(&self.snapshot.held), name)
            .map_err(|error| error.unwrap_or_else(|| format!("{name} has no update")))
    }

    /// Whether the last look listed what this source has, so that an
    /// update it no longer lists is gone rather than unasked.
    fn listed(&self, source: apt::Source) -> bool {
        match source {
            apt::Source::Apt => self.snapshot.error.is_none(),
            apt::Source::Firmware => self.snapshot.firmware_error.is_none(),
            apt::Source::Flatpak | apt::Source::FlatpakUser | apt::Source::Snap => {
                self.apps_scanned && !self.apps_failed.contains(&source)
            }
        }
    }

    /// Whether a full upgrade would install a version that was skipped:
    /// APT knows nothing of skips, so the APT updates must then be named.
    pub(crate) fn skips_apt(&self) -> bool {
        self.skipped.iter().any(|update| update.source == apt::Source::Apt)
    }

    /// Puts the apps last listed beside the rest and moves the skipped
    /// versions out of the waiting list. Security comes first, then by
    /// source, then by source package, so a package's binaries are together.
    fn merge(&mut self) {
        let mut all: Vec<apt::Update> = std::mem::take(&mut self.snapshot.updates);
        all.retain(|update| {
            !matches!(update.source, apt::Source::Flatpak | apt::Source::FlatpakUser | apt::Source::Snap)
        });
        all.extend(self.apps.iter().cloned());
        // A skipped app is among the apps listed already.
        for update in std::mem::take(&mut self.skipped) {
            let listed =
                all.iter().any(|other| other.key() == update.key() && other.skip_identity() == update.skip_identity());
            if !listed {
                all.push(update);
            }
        }
        // A skip ends when a different update than the one skipped arrives.
        // A source that failed, or was left out, proves nothing.
        let mut skip = std::mem::take(&mut self.skip);
        skip.retain(|(source, name), version| {
            !self.listed(*source)
                || all
                    .iter()
                    .any(|update| update.source == *source && update.name == *name && update.skip_identity() == version)
        });
        let (skipped, waiting): (Vec<apt::Update>, Vec<apt::Update>) = all
            .into_iter()
            .partition(|update| skip.get(&update.key()).is_some_and(|identity| identity == update.skip_identity()));
        self.skip = skip;
        let mut waiting = waiting;
        waiting.sort_by(|left, right| {
            right
                .security
                .cmp(&left.security)
                .then_with(|| left.source.cmp(&right.source))
                .then_with(|| left.group().cmp(right.group()))
                .then_with(|| left.name.cmp(&right.name))
        });
        self.snapshot.updates = waiting;
        self.skipped = skipped;
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
        self.updates.saving.store(self.updates_saving(), Ordering::Relaxed);
        let saving = self.updates.saving.clone();
        let thread = thread::Builder::new()
            .name("tileception-updates".into())
            .spawn(move || run(&sender, &incoming, interval, &saving))
            .map_err(|error| tracing::error!("could not start the updates worker: {error}"))
            .ok();
        self.updates.source = Some(token);
        self.updates.waiting = read_waiting();
        self.updates.log = read_log();
        self.updates.skip = read_skip();
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
            if self.password_prompt.as_ref().is_some_and(|prompt| matches!(prompt.purpose(), Purpose::Sudo(_))) {
                self.close_password_prompt();
                self.updates_drop_pending();
            }
            if self.updates.window.open {
                self.close_updates();
            }
        }
    }

    fn updates_event(&mut self, event: Event) {
        match event {
            Event::Scanned(scan) => self.updates_scanned(*scan),
            Event::Tick => {
                self.updates_remind();
                return self.updates_auto_refresh();
            }
            Event::NeedsPassword(task) => {
                // The rest of the install, and a restart after it, wait
                // for the password.
                self.updates.job = None;
                self.updates_ask_password(task, None);
            }
            Event::Progress { done, label } => {
                if let Some(job) = self.updates.job.as_mut() {
                    job.done = done;
                    job.label = label;
                }
            }
            Event::Finished { task, result, log, quiet } => self.updates_finished(task, result, log, quiet),
            Event::Preview { request, text } => self.updates.window.set_preview(request, text),
            Event::Mirrors(found, reference) => {
                let mirrors = &mut self.updates.mirrors;
                mirrors.testing = false;
                mirrors.reference = reference;
                // The mirror in use was just tested too: its warning follows.
                if let Some(current) = &mirrors.current
                    && let Some(mirror) = found.iter().find(|mirror| mirror.url == *current)
                {
                    mirrors.check = Some((mirror.clone(), reference));
                }
                mirrors.candidates = found;
            }
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

    fn updates_scanned(&mut self, mut scan: Scan) {
        let first = !self.updates.scanned;
        self.updates.scanned = true;
        if scan.snapshot.firmware_error.is_some() {
            scan.snapshot.updates.extend(
                self.updates
                    .snapshot
                    .updates
                    .iter()
                    .chain(&self.updates.skipped)
                    .filter(|update| update.source == apt::Source::Firmware)
                    .cloned(),
            );
        }
        self.updates.snapshot = scan.snapshot;
        if let Some(found) = self.updates.snapshot.apps.take() {
            // A source that could not be asked keeps what it listed before.
            self.updates.apps.retain(|update| found.failed.contains(&update.source));
            self.updates.apps.extend(found.updates);
            self.updates.apps_failed = found.failed;
            self.updates.apps_scanned = true;
        }
        // The new look lists the skipped versions again among the rest.
        self.updates.skipped.clear();
        self.updates.merge();
        self.updates.third_party = scan.third_party;
        self.updates.free = scan.free;
        self.updates.track = scan.track;
        self.updates.snapshots = scan.snapshots;
        self.updates.automatic = scan.automatic;
        self.updates.mirrors.current = scan.mirror;
        if scan.mirror_check.is_some() {
            self.updates.mirrors.check = scan.mirror_check;
        }
        if self
            .updates
            .mirrors
            .check
            .as_ref()
            .is_some_and(|(mirror, _)| Some(mirror.url.as_str()) != self.updates.mirrors.current.as_deref())
        {
            self.updates.mirrors.check = None;
        }
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

    /// Opens the password prompt for a task that `sudo` wants a password for.
    fn updates_ask_password(&mut self, task: Task, error: Option<String>) {
        let user = std::env::var("USER").unwrap_or_default();
        let title = task.title();
        self.open_password_prompt(Prompt::new(Purpose::Sudo(task), title, "", &user).with_error(error));
    }

    /// Whether the computer is saving power or data, so that nothing should
    /// use the network by itself: below 30% on battery. The worker adds a
    /// metered connection, which it asks NetworkManager about.
    fn updates_saving(&self) -> bool {
        self.battery
            .reading
            .is_some_and(|reading| reading.charge == crate::battery::Charge::Discharging && reading.percent < 30)
    }

    /// Refreshes the package lists once they are older than `check_hours`.
    /// Polkit lets the person at the computer do that without a password;
    /// where it does not, the worker leaves it. A failure is tried again
    /// after an hour.
    fn updates_auto_refresh(&mut self) {
        let saving = self.updates_saving();
        self.updates.saving.store(saving, Ordering::Relaxed);
        let config = &self.config.file.updates;
        let interval = Duration::from_secs(u64::from(config.check_hours.clamp(1, 168)) * 3600);
        let fresh =
            self.updates.snapshot.checked.is_some_and(|checked| checked.elapsed().is_ok_and(|age| age < interval));
        let tried = self.updates.refreshed.is_some_and(|tried| tried.elapsed() < RETRY_REFRESH);
        // An install that waits for its password would find the helper
        // busy once the password is given.
        let asking = self.password_prompt.as_ref().is_some_and(|prompt| matches!(prompt.purpose(), Purpose::Sudo(_)));
        if !config.enabled
            || !self.updates.scanned
            || self.updates.busy()
            || asking
            || fresh
            || tried
            || saving
            || self.is_locked()
        {
            return;
        }
        self.updates.refreshed = Some(Instant::now());
        if self.updates_request(Request::Run { task: Task::Refresh, password: None, quiet: true }) {
            self.updates.job = Some(Job { task: Task::Refresh, done: 0, total: 0, label: "starting".into() });
            self.updates_changed();
        }
    }

    fn updates_finished(&mut self, task: Task, result: Result<(), Failure>, log: String, quiet: bool) {
        self.updates.job = None;
        if quiet {
            return self.updates_finished_last(task, result, log, quiet);
        }
        // A wrong password is asked for again. The task is still part of its
        // install: what follows it, and the log so far, wait for the answer.
        if matches!(result, Err(Failure::Password)) {
            return self.updates_ask_password(task, Some("That password did not work.".into()));
        }
        // An install of several sources is a chain of tasks with one log.
        // The next part follows a success; a failure ends the chain.
        let mut log = std::mem::take(&mut self.updates.chain_log) + &log;
        if result.is_ok()
            && let Some(next) = self.updates.next.pop_front()
        {
            log.push('\n');
            self.updates.chain_log = log;
            let after = self.updates.after;
            if self.updates_run_part(next).is_ok() {
                self.updates.after = after;
                return;
            }
            log = std::mem::take(&mut self.updates.chain_log);
        }
        self.updates.next.clear();
        self.updates_finished_last(task, result, log, quiet)
    }

    fn updates_finished_last(&mut self, task: Task, result: Result<(), Failure>, log: String, quiet: bool) {
        if quiet {
            // Nobody asked: nothing to say. The last task's log and any
            // power action waiting for an install stay with that install.
            if let Err(Failure::Failed(reason)) = &result {
                tracing::info!("the package lists were not refreshed: {reason}");
            }
            return;
        }
        let after = self.updates.after.take();
        if !matches!(result, Err(Failure::Password | Failure::Declined)) && !log.trim().is_empty() {
            let log = Log { title: task.title(), succeeded: result.is_ok(), text: log };
            write_log(&log);
            self.updates.repair = !log.succeeded && apt::needs_repair(&log.text);
            self.updates.log = Some(log);
        }
        match result {
            Ok(()) => {
                let message = match &task {
                    Task::Refresh => "Package lists refreshed.".to_string(),
                    Task::Install(_) | Task::InstallKeeping { .. } => "Updates installed.".to_string(),
                    Task::Kernels { .. } => "Kernel changes applied.".to_string(),
                    Task::Remove(names) if names.len() == 1 => "Removed 1 package.".to_string(),
                    Task::Remove(names) => format!("Removed {} packages.", names.len()),
                    Task::Autoremove => "Unneeded packages removed.".to_string(),
                    Task::Repair => "Packages repaired.".to_string(),
                    Task::Flatpak { .. } | Task::Snap(_) => "Updates installed.".to_string(),
                    Task::Firmware(_) => {
                        "Firmware updated. Some devices finish when the computer restarts.".to_string()
                    }
                    Task::Hold { hold: true, .. } => "Held at their version.".to_string(),
                    Task::Hold { hold: false, .. } => "Released: they update again.".to_string(),
                    Task::Automatic(true) => "Automatic updates are on.".to_string(),
                    Task::Automatic(false) => "Automatic updates are off.".to_string(),
                    Task::Mirror(url) => format!("Now using {url}."),
                };
                if task.changes_packages() {
                    self.notify_locally("Updates", &message, Urgency::Low);
                }
                self.updates.window.task_done(&task);
                self.updates.outcome = Some(Ok(message));
                if let Some(action) = after {
                    self.power_menu = None;
                    self.run_power_action(action);
                }
            }
            // Asked for again in `updates_finished`, before the chain ends.
            Err(Failure::Password) => {}
            Err(Failure::Declined) => {
                self.updates.outcome = Some(Err(format!("{} was not authorised.", task.title())));
            }
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

    /// Runs a task, unless one is running already. It stands alone: an
    /// install that waited for a password and was left is not continued.
    pub(crate) fn updates_start(&mut self, task: Task) -> Result<(), String> {
        self.updates_launch(task, None)?;
        self.updates_drop_pending();
        Ok(())
    }

    /// Runs a task again with the password `sudo` asked for. It goes on
    /// with its install: the tasks after it and a restart still follow.
    pub(crate) fn updates_with_password(&mut self, task: Task, password: Zeroizing<String>) -> Result<(), String> {
        let launched = self.updates_launch(task, Some(password));
        if launched.is_err() {
            self.updates_drop_pending();
        }
        launched
    }

    /// Forgets the rest of an install and what was to follow it: when its
    /// password was not given, or another task started instead.
    pub(crate) fn updates_drop_pending(&mut self) {
        self.updates.next.clear();
        self.updates.chain_log.clear();
        self.updates.after = None;
    }

    fn updates_launch(&mut self, task: Task, password: Option<Zeroizing<String>>) -> Result<(), String> {
        if self.updates.busy() {
            return Err("another update task is running".into());
        }
        task.commands()?;
        self.updates_send(task, password)?;
        self.updates.failed = false;
        self.updates.outcome = None;
        self.updates_changed();
        Ok(())
    }

    /// Starts the next task of a chain, keeping what the chain set up.
    fn updates_run_part(&mut self, task: Task) -> Result<(), String> {
        task.commands()?;
        self.updates_send(task, None)
    }

    fn updates_constrained_task(&self, task: Task) -> Result<Task, String> {
        // Apply the same skips to previews and execution, including security-only
        // installs and later tasks in a chain. Naming updates alone does not
        // stop APT from upgrading a skipped dependency.
        Ok(match task {
            Task::Install(mut names) if self.updates.skips_apt() => {
                if names.is_empty() {
                    names = self
                        .updates
                        .snapshot
                        .updates
                        .iter()
                        .filter(|update| update.source == apt::Source::Apt)
                        .map(|update| update.name.clone())
                        .collect();
                }
                let keep = self
                    .updates
                    .skipped
                    .iter()
                    .filter(|update| update.source == apt::Source::Apt)
                    .map(|update| {
                        update.old.as_ref().map(|old| format!("{}={old}", update.name)).ok_or_else(|| {
                            format!("cannot keep skipped package {} without its installed version", update.name)
                        })
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                Task::InstallKeeping { names, keep }
            }
            task => task,
        })
    }

    fn updates_send(&mut self, task: Task, password: Option<Zeroizing<String>>) -> Result<(), String> {
        let task = self.updates_constrained_task(task)?;
        task.commands()?;
        let total = match &task {
            Task::Autoremove => self.updates.snapshot.plan.unneeded.len(),
            Task::Install(names) if names.is_empty() => {
                self.updates.snapshot.updates.iter().filter(|update| update.source == apt::Source::Apt).count()
            }
            task => task.count(),
        };
        let job = Job { task: task.clone(), done: 0, total, label: "starting".into() };
        if !self.updates_request(Request::Run { task, password, quiet: false }) {
            return Err("the update manager is not running".into());
        }
        self.updates.job = Some(job);
        self.updates_changed();
        Ok(())
    }

    /// Runs tasks one after the other, as one install. The first starts
    /// now; each of the others when the one before it succeeded.
    pub(crate) fn updates_start_all(&mut self, tasks: Vec<Task>) -> Result<(), String> {
        let mut tasks: VecDeque<Task> = tasks.into();
        let first = tasks.pop_front().ok_or("there is nothing to install")?;
        for task in &tasks {
            task.commands()?;
        }
        self.updates_start(first)?;
        self.updates.next = tasks;
        self.updates.chain_log.clear();
        Ok(())
    }

    /// Installs every update, or only those for security. Firmware is
    /// left out: it is only ever installed when chosen.
    pub(crate) fn updates_install(&mut self, security_only: bool) -> Result<(), String> {
        let updates = &self.updates.snapshot.updates;
        if updates.is_empty() {
            return Err("there are no updates to install".into());
        }
        if security_only {
            let names: Vec<String> =
                updates.iter().filter(|update| update.security).map(|update| update.name.clone()).collect();
            if names.is_empty() {
                return Err("there are no security updates".into());
            }
            return self.updates_start(Task::Install(names));
        }
        let chosen: Vec<&apt::Update> =
            updates.iter().filter(|update| update.source != apt::Source::Firmware).collect();
        let tasks = install_tasks(&chosen, !self.updates.skips_apt());
        self.updates_start_all(tasks)
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

    /// Installs everything that waits, firmware too, then restarts: what
    /// the power menu's Restart and install does.
    pub(crate) fn updates_install_and_restart(&mut self) -> Result<(), String> {
        // With a skip, the review shows what the install that keeps it does.
        if self.updates.snapshot.plan.removes() {
            return self.updates_review_install(true);
        }
        let chosen: Vec<&apt::Update> = self.updates.snapshot.updates.iter().collect();
        let tasks = install_tasks(&chosen, !self.updates.skips_apt());
        self.updates_start_all(tasks)?;
        self.updates.after = Some(crate::power::PowerAction::Restart);
        self.schedule_render();
        Ok(())
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
            snapshots: self.updates.snapshots.is_some(),
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
        let sudo = if task::is_root() { "" } else { "sudo " };
        // A snapshot first, where there is something to take it with; the
        // upgrade waits for it and is not started when it fails.
        let snapshot = match self.updates.snapshots {
            Some(tool) => {
                let commands: Vec<String> = tool
                    .commands(&format!("the upgrade to {name}"), &snapshots::stamp())
                    .iter()
                    .map(|command| format!("{sudo}{}", shell_words(command)))
                    .collect();
                format!(
                    "echo 'Taking a snapshot with {} first.'; {{ {}; }} || {{ echo 'The snapshot failed; the upgrade was not started.'; echo 'Press Enter to close.'; read -r _; exit 1; }}; ",
                    tool.name(),
                    commands.join(" && ")
                )
            }
            None => String::new(),
        };
        self.run_in_terminal(&format!(
            "{snapshot}{sudo}do-release-upgrade; echo; echo 'Press Enter to close.'; read -r _"
        ))
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
                let mut chosen = Vec::new();
                for name in &names {
                    let update = pick(pending.iter(), name)
                        .map_err(|error| error.unwrap_or_else(|| format!("{name} has no update waiting")))?;
                    chosen.push(update);
                }
                let tasks = install_tasks(&chosen, false);
                self.updates_start_all(tasks)?;
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
            UpdatesCommand::Changelog(name) => {
                self.open_updates(Page::Available);
                self.updates_show_notes(&name)?;
            }
            UpdatesCommand::Hold(names, hold) => self.updates_hold(names, hold)?,
            UpdatesCommand::Skip(name, skip) => self.updates_skip(&name, skip)?,
            UpdatesCommand::Automatic(None) => return Ok(self.updates_automatic_status()),
            UpdatesCommand::Automatic(Some(enable)) => {
                if !self.updates.automatic.available {
                    return Err("unattended-upgrades is not installed".into());
                }
                self.updates_start(Task::Automatic(enable))?;
            }
            UpdatesCommand::Mirrors => {
                self.updates_test_mirrors();
                return Ok(self.updates_mirrors_status());
            }
            UpdatesCommand::Mirror(url) => self.updates_start(Task::Mirror(url))?,
            UpdatesCommand::History(since) => {
                return Ok(history_text(&self.updates.snapshot.history, since.as_deref()));
            }
        }
        Ok(String::new())
    }

    /// Holds APT packages at their version, or lets them update again.
    pub(crate) fn updates_hold(&mut self, names: Vec<String>, hold: bool) -> Result<(), String> {
        let mut resolved = Vec::with_capacity(names.len());
        for name in &names {
            let updates = &self.updates;
            let apt = updates.snapshot.updates.iter().chain(&updates.skipped).chain(&updates.snapshot.held);
            match pick(apt.filter(|update| update.source == apt::Source::Apt), name) {
                Ok(update) => resolved.push(update.name.clone()),
                Err(_) if hold => return Err(format!("{name} has no APT update waiting")),
                // A held package may have no update to list. Strip only the
                // source qualifier, keeping any architecture qualifier.
                Err(_) => resolved.push(name.strip_prefix("apt:").unwrap_or(name).to_string()),
            }
        }
        self.updates_start(Task::Hold { names: resolved, hold })
    }

    /// Skips the version of an update that waits, or stops skipping it.
    pub(crate) fn updates_skip(&mut self, name: &str, skip: bool) -> Result<(), String> {
        let update = self.updates.find(name)?.clone();
        self.updates_skip_update(&update, skip)
    }

    /// Skips this update's version, or stops skipping it.
    pub(crate) fn updates_skip_update(&mut self, update: &apt::Update, skip: bool) -> Result<(), String> {
        if skip {
            self.updates.skip.insert(update.key(), update.skip_identity().to_string());
        } else if self.updates.skip.remove(&update.key()).is_none() {
            return Err(format!("{} is not skipped", update.name));
        }
        write_skip(&self.updates.skip);
        self.updates.merge();
        self.updates.window.settle(&self.updates.snapshot, &self.updates.kernels);
        self.updates_changed();
        Ok(())
    }

    /// Asks the worker to test the candidate mirrors.
    pub(crate) fn updates_test_mirrors(&mut self) {
        if !self.updates.mirrors.testing && self.updates_request(Request::Mirrors) {
            self.updates.mirrors.testing = true;
            self.updates_changed();
        }
    }

    /// Asks the worker what changed in an update, for the notes sheet.
    pub(crate) fn updates_request_notes(&mut self, request: u64, update: apt::Update) {
        self.updates_request(Request::Notes { request, update: Box::new(update) });
    }

    fn updates_automatic_status(&self) -> String {
        let automatic = &self.updates.automatic;
        if !automatic.available {
            return "unattended-upgrades is not installed\n".into();
        }
        let state = if automatic.enabled { "on" } else { "off" };
        format!("automatic updates are {state}; they cover {}\n", automatic.covers.join(", "))
    }

    fn updates_mirrors_status(&self) -> String {
        let mirrors = &self.updates.mirrors;
        let mut text = format!("in use: {}\n", mirrors.current.as_deref().unwrap_or("none found"));
        if let Some(warning) = mirrors.warning() {
            text.push_str(&format!("warning: {warning}\n"));
        }
        if mirrors.testing {
            text.push_str("testing the mirrors; ask again in a moment\n");
        }
        for mirror in &mirrors.candidates {
            text.push_str(&format!("  {}  {}\n", mirror.url, describe_mirror(mirror, mirrors.reference)));
        }
        text
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
        if let Some(warning) = updates.mirrors.warning() {
            text.push_str(&format!("mirror: {warning}\n"));
        }
        let line = |update: &apt::Update, mark: &str| {
            let old = update.old.as_deref().unwrap_or("new");
            let source = match update.source {
                apt::Source::Apt => String::new(),
                source => format!(" ({})", source.label().to_lowercase()),
            };
            format!("  {} {old} -> {}{source}{mark}\n", update.name, update.new)
        };
        for update in &updates.snapshot.updates {
            text.push_str(&line(update, if update.security { " [security]" } else { "" }));
        }
        for update in &updates.skipped {
            text.push_str(&line(update, " [skipped]"));
        }
        for update in &updates.snapshot.held {
            text.push_str(&line(update, " [held]"));
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

/// Whether this is a day as `YYYY-MM-DD`.
fn valid_day(day: &str) -> bool {
    let parts: Vec<&str> = day.split('-').collect();
    matches!(parts.as_slice(), [year, month, date]
        if year.len() == 4 && month.len() == 2 && date.len() == 2
            && parts.iter().all(|part| part.chars().all(|character| character.is_ascii_digit())))
}

/// The apt history as text for `updates history`: each transaction since
/// the day, newest first, with every package and its versions. Without a
/// day, the latest ten.
fn history_text(history: &[backend::history::Entry], since: Option<&str>) -> String {
    let chosen: Vec<&backend::history::Entry> = match since {
        // Dates are `YYYY-MM-DD hh:mm:ss`, so they compare as text.
        Some(day) => history.iter().filter(|entry| entry.date.as_str() >= day).collect(),
        None => history.iter().take(10).collect(),
    };
    if chosen.is_empty() {
        return match since {
            Some(day) => format!("apt changed nothing since {day}\n"),
            None => "no apt history could be read\n".into(),
        };
    }
    let mut text = String::new();
    for entry in chosen {
        text.push_str(&format!("{} by {}: {}\n", entry.date, entry.who(), entry.summary()));
        if !entry.command.is_empty() {
            text.push_str(&format!("  $ {}\n", entry.command));
        }
        for (verb, changes) in
            [("updated", &entry.upgraded), ("installed", &entry.installed), ("removed", &entry.removed)]
        {
            for change in changes {
                text.push_str(&format!("  {verb} {} {}\n", change.name, change.version));
            }
        }
    }
    text
}

/// The tasks that install these updates, a task per source. With `all_apt`,
/// the APT part is every APT update, as a full upgrade. That would install
/// skipped versions too: pass it only when no APT version is skipped.
pub(crate) fn install_tasks(chosen: &[&apt::Update], all_apt: bool) -> Vec<Task> {
    let names = |source: apt::Source| -> Vec<String> {
        chosen.iter().filter(|update| update.source == source).map(|update| update.name.clone()).collect()
    };
    let mut tasks = Vec::new();
    let apt_names = names(apt::Source::Apt);
    if all_apt && !apt_names.is_empty() {
        tasks.push(Task::Install(Vec::new()));
    } else if !apt_names.is_empty() {
        tasks.push(Task::Install(apt_names));
    }
    for (source, user) in [(apt::Source::Flatpak, false), (apt::Source::FlatpakUser, true)] {
        let refs = names(source);
        if !refs.is_empty() {
            tasks.push(Task::Flatpak { refs, user });
        }
    }
    let snaps = names(apt::Source::Snap);
    if !snaps.is_empty() {
        tasks.push(Task::Snap(snaps));
    }
    let devices = names(apt::Source::Firmware);
    if !devices.is_empty() {
        tasks.push(Task::Firmware(devices));
    }
    tasks
}

/// A command for a shell, each word quoted where it needs it.
fn shell_words(words: &[String]) -> String {
    words
        .iter()
        .map(|word| {
            if word.chars().all(|character| character.is_ascii_alphanumeric() || "-_./:=".contains(character)) {
                word.clone()
            } else {
                format!("'{}'", word.replace('\'', "'\\''"))
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// A tested mirror in a line: "2.4 MB/s · 0.12 s · up to date".
pub(crate) fn describe_mirror(mirror: &mirrors::Mirror, reference: Option<i64>) -> String {
    if let Some(error) = &mirror.error {
        return format!("unreachable: {error}");
    }
    let mut parts = Vec::new();
    if let Some(speed) = mirror.speed {
        parts.push(format!("{}/s", backend::human_size(speed)));
    }
    if let Some(latency) = mirror.latency {
        parts.push(format!("{latency:.2} s"));
    }
    parts.push(mirrors::describe_lag(mirror.lag(reference)));
    parts.join(" \u{b7} ")
}

/// Whether the release offer is asked for again: once a day.
const OFFER_EVERY: Duration = Duration::from_secs(24 * 3600);

/// One look at the system. `network` lets it ask servers: Flatpak, Snap,
/// the next release and the mirror in use.
fn scan(with_offer: bool, network: bool) -> Scan {
    let files = Files::default();
    let runner = backend::System;
    let mut snapshot = backend::scan(&runner, &files, network);
    // Restarting is easier to explain than logging out and in again.
    snapshot.restart |= replaced();
    let codename = snapshot.system.codename.clone();
    let mirror = mirrors::current(&files);
    let mirror_check = (with_offer && network && mirror.is_some())
        .then(|| mirrors::check_current(&runner, &files, &codename))
        .flatten();
    Scan {
        snapshot,
        offer: with_offer.then(|| release::offer(&runner)),
        third_party: release::third_party(&files),
        free: release::free_space(&runner),
        track: release::track(&files),
        snapshots: snapshots::tool(),
        automatic: automatic::read(&runner, &files),
        mirror,
        mirror_check,
    }
}

/// What changed in an update, asked from the tool that installs it.
fn notes(update: &apt::Update) -> String {
    let runner = backend::System;
    let result = match update.source {
        apt::Source::Apt => apt::changelog(&runner, &update.name, update.old.as_deref()),
        apt::Source::Flatpak | apt::Source::FlatpakUser => apps::flatpak_notes(&runner, update),
        apt::Source::Snap => apps::snap_notes(&runner, &update.name),
        apt::Source::Firmware => firmware::notes(&runner, &update.name),
    };
    match result {
        Ok(text) if text.trim().is_empty() => "No release notes were found.".into(),
        Ok(text) => text,
        Err(error) => format!("The release notes could not be read: {error}"),
    }
}

/// How long after a refresh by itself, or a try, the next one waits.
const RETRY_REFRESH: Duration = Duration::from_secs(3600);

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
    fn scan(&mut self, with_offer: bool, network: bool) -> Event {
        // Stamped first: a change during the scan brings another one.
        *self = Watch::default();
        Event::Scanned(Box::new(scan(with_offer, network)))
    }
}

fn run(
    events: &channel::Sender<Event>,
    incoming: &mpsc::Receiver<Option<Request>>,
    interval: Duration,
    saving: &AtomicBool,
) {
    let mut asked = Instant::now();
    let mut ticked = Instant::now();
    let mut watch = Watch::default();
    let online = || !saving.load(Ordering::Relaxed) && !metered();
    let first = online();
    if events.send(watch.scan(first, first)).is_err() {
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
                // Asking for the next release uses the network; so do
                // Flatpak and Snap, which follow the timed look.
                let timed = watch.looked.elapsed() >= interval;
                let offer = asked.elapsed() >= OFFER_EVERY && online();
                if offer {
                    asked = Instant::now();
                }
                watch.scan(offer, (offer || timed) && online())
            }
            Ok(Some(Request::Scan { offer, apps })) => watch.scan(offer, apps && online()),
            Ok(Some(Request::Preview { request, args })) => Event::Preview { request, text: preview(&args) },
            Ok(Some(Request::Notes { request, update })) => Event::Preview { request, text: notes(&update) },
            Ok(Some(Request::Mirrors)) => {
                let files = Files::default();
                let runner = backend::System;
                let codename = release::system(&files).codename;
                let found = mirrors::survey(&runner, &files, &codename);
                let reference =
                    found.iter().find(|mirror| mirror.url == mirrors::ARCHIVE).and_then(|mirror| mirror.published);
                Event::Mirrors(found, reference)
            }
            Ok(Some(Request::Run { task, password, quiet })) => {
                let mut log = String::new();
                let result = if quiet && (saving.load(Ordering::Relaxed) || metered() || !unasked(&task)) {
                    Some(Err(Failure::Declined))
                } else {
                    perform(&task, password.as_deref().map(String::as_str), events, &mut log)
                };
                let Some(result) = result else {
                    if events.send(Event::NeedsPassword(task)).is_err() {
                        return;
                    }
                    continue;
                };
                let refreshed = matches!(task, Task::Refresh) && result.is_ok();
                // After a refresh the apps are asked for too; after an
                // install of apps, they are listed again.
                let apps = (refreshed || matches!(task, Task::Flatpak { .. } | Task::Snap(_))) && online();
                let ran = !matches!(result, Err(Failure::Declined | Failure::Password));
                if events.send(Event::Finished { task, result, log, quiet }).is_err() {
                    return;
                }
                if !ran {
                    continue;
                }
                if refreshed {
                    asked = Instant::now();
                }
                watch.scan(refreshed && !quiet, apps)
            }
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

/// Feeds a password to a `sudo -S` on its standard input.
fn feed(child: &mut std::process::Child, password: Option<&str>) {
    if let (Some(password), Some(mut stdin)) = (password, child.stdin.take()) {
        let _ = stdin.write_all(password.as_bytes());
        let _ = stdin.write_all(b"\n");
    }
}

/// Whether a program is somewhere on `PATH`.
fn on_path(program: &str) -> bool {
    std::env::var_os("PATH")
        .is_some_and(|path| std::env::split_paths(&path).any(|folder| folder.join(program).is_file()))
}

/// The helper: where the package puts it, else beside this program, as in
/// a build folder.
fn helper() -> Option<PathBuf> {
    let installed = Path::new(task::HELPER);
    if installed.is_file() {
        return Some(installed.to_path_buf());
    }
    let beside = std::env::current_exe().ok()?.with_file_name("tileception-updates-helper");
    beside.is_file().then_some(beside)
}

/// Whether polkit can start the helper: the policy names the helper's
/// installed path, so only that one counts.
fn polkit_ready() -> bool {
    Path::new(task::HELPER).is_file() && Path::new(task::POLICY).is_file() && on_path("pkexec")
}

/// Whether a task may run without anyone having asked for it: only where
/// that needs no password. Polkit is asked without letting it prompt.
fn unasked(task: &Task) -> bool {
    if task::is_root() {
        return true;
    }
    let action =
        if task.changes_packages() { "org.tileception.updates.change" } else { "org.tileception.updates.refresh" };
    polkit_ready()
        && Process::new("pkcheck")
            .args(["--action-id", action, "--process", &std::process::id().to_string()])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
}

/// Whether NetworkManager counts the connection as metered. Unknown, and
/// no NetworkManager, count as not.
fn metered() -> bool {
    const NAME: &str = "org.freedesktop.NetworkManager";
    let read = || -> zbus::Result<u32> {
        let reply = zbus::blocking::Connection::system()?.call_method(
            Some(NAME),
            "/org/freedesktop/NetworkManager",
            Some("org.freedesktop.DBus.Properties"),
            "Get",
            &(NAME, "Metered"),
        )?;
        let value: zbus::zvariant::OwnedValue = reply.body().deserialize()?;
        Ok(value.downcast_ref::<u32>()?)
    };
    // 1 is yes, 3 is "guessed yes".
    matches!(read(), Ok(1 | 3))
}

/// How one line of a task's output is taken: kept, and told as progress.
struct Follow<'a> {
    events: &'a channel::Sender<Event>,
    log: &'a mut String,
    done: usize,
}

impl Follow<'_> {
    fn line(&mut self, line: &str) {
        self.log.push_str(line);
        self.log.push('\n');
        let Some(progress) = apt::parse_progress(line) else { return };
        if matches!(progress, apt::Progress::Setup(_) | apt::Progress::Remove(_)) {
            self.done += 1;
        }
        // Read on even when nobody listens: stopping would stop apt halfway.
        let _ = self.events.send(Event::Progress { done: self.done, label: progress.label() });
    }

    /// Keeps the end of a log that grew too long.
    fn trim(&mut self) {
        if self.log.len() > LOG_LIMIT {
            let mut start = self.log.len() - LOG_LIMIT;
            while !self.log.is_char_boundary(start) {
                start += 1;
            }
            self.log.replace_range(..start, "...\n");
        }
    }
}

/// How the helper ended, started through `pkexec` or `sudo`.
struct Ended {
    code: Option<i32>,
    /// What it, or what started it, wrote to standard error.
    errors: String,
}

impl Ended {
    /// Why it failed: the helper's own reason, else the last thing said.
    fn reason(&self) -> String {
        let lines = || self.errors.lines().map(str::trim).filter(|line| !line.is_empty());
        lines()
            .filter_map(|line| line.strip_prefix(task::FAILED))
            .next_back()
            .or_else(|| lines().next_back())
            .map(|line| line.trim_start_matches("Error executing command as another user: "))
            .unwrap_or("the task failed")
            .to_string()
    }
}

/// Whether `pkexec` gave up because the session has no polkit agent: it
/// says so, or fails to become one itself for want of a terminal.
fn no_agent(errors: &str) -> bool {
    errors.contains("No authentication agent") || errors.contains("textual authentication agent")
}

/// Runs the helper under what `command` starts it with, to its end.
fn drive(mut command: Process, task: &Task, password: Option<&str>, follow: &mut Follow) -> Result<Ended, String> {
    command.args(task.to_args()).stdout(Stdio::piped()).stderr(Stdio::piped());
    command.stdin(if password.is_some() { Stdio::piped() } else { Stdio::null() });
    let mut child = command.spawn().map_err(|error| format!("could not start the updates helper: {error}"))?;
    feed(&mut child, password);
    let errors = child.stderr.take().map(|mut stderr| {
        thread::spawn(move || {
            let mut text = String::new();
            let _ = stderr.read_to_string(&mut text);
            text
        })
    });
    if let Some(stdout) = child.stdout.take() {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            follow.line(&line);
        }
    }
    let status = child.wait().map_err(|error| error.to_string())?;
    let errors = errors.and_then(|reader| reader.join().ok()).unwrap_or_default();
    Ok(Ended { code: status.code(), errors })
}

/// Runs a task as root, by the first way that is open: in place when
/// Tileception is root, through polkit, or through `sudo`. None when `sudo`
/// wants a password first.
fn perform(
    task: &Task,
    password: Option<&str>,
    events: &channel::Sender<Event>,
    log: &mut String,
) -> Option<Result<(), Failure>> {
    let mut follow = Follow { events, log, done: 0 };
    if task::is_root() || !task.needs_root() {
        let result = task::run(task, &mut |line| follow.line(line)).map_err(Failure::Failed);
        follow.trim();
        return Some(result);
    }
    // A password in hand was typed for sudo: polkit asks by itself.
    if password.is_none() && polkit_ready() {
        let mut command = Process::new("pkexec");
        command.arg(task::HELPER);
        let ended = match drive(command, task, None, &mut follow) {
            Ok(ended) => ended,
            Err(reason) => return Some(Err(Failure::Failed(reason))),
        };
        follow.trim();
        match ended.code {
            Some(0) => return Some(Ok(())),
            // The person closed polkit's question.
            Some(126) => return Some(Err(Failure::Declined)),
            // Nobody to ask the question: sudo and Tileception's own prompt.
            Some(127) if no_agent(&ended.errors) => {}
            Some(127) if ended.errors.contains("Not authorized") => return Some(Err(Failure::Declined)),
            _ => return Some(Err(Failure::Failed(ended.reason()))),
        }
    }
    let Some(helper) = helper() else {
        return Some(Err(Failure::Failed(
            "Changing packages needs tileception-updates-helper, which is not installed.".into(),
        )));
    };
    if !on_path("sudo") {
        return Some(Err(Failure::Failed("Changing packages needs polkit with an agent, or sudo.".into())));
    }
    let mut command = Process::new("sudo");
    match password {
        // -k: always read the password, so it never reaches the helper.
        Some(_) => {
            let mut check = Process::new("sudo");
            check
                .args(["-S", "-k", "-p", "", "true"])
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            let taken = check.spawn().is_ok_and(|mut child| {
                feed(&mut child, password);
                child.wait().is_ok_and(|status| status.success())
            });
            if !taken {
                return Some(Err(Failure::Password));
            }
            command.args(["-S", "-k", "-p", ""]);
        }
        None => {
            let free = Process::new("sudo")
                .args(["-n", "true"])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .is_ok_and(|status| status.success());
            if !free {
                return None;
            }
            command.arg("-n");
        }
    }
    command.arg("--").arg(helper);
    let result = match drive(command, task, password, &mut follow) {
        Ok(ended) if ended.code == Some(0) => Ok(()),
        Ok(ended) => Err(Failure::Failed(ended.reason())),
        Err(reason) => Err(Failure::Failed(reason)),
    };
    follow.trim();
    Some(result)
}

/// Whether the Tileception that is running is no longer the one installed:
/// a package update replaced its program, which `/proc` then calls deleted.
fn replaced() -> bool {
    std::fs::read_link("/proc/self/exe").is_ok_and(|path| path.to_string_lossy().ends_with(" (deleted)"))
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
    let title = Task::TITLES.iter().copied().find(|known| *known == title).unwrap_or("Changing kernels");
    Some(Log { title, succeeded: state == "ok", text: text.to_string() })
}

/// The update a command names: by its name, or by `source:name` (such as
/// `snap:firefox` or `flatpak-user:org.mozilla.firefox//stable`) where two
/// sources have one by that name. The error is `None` when nothing matches.
fn pick<'a>(updates: impl Iterator<Item = &'a apt::Update>, text: &str) -> Result<&'a apt::Update, Option<String>> {
    let updates: Vec<&apt::Update> = updates.collect();
    let qualified = text.split_once(':').and_then(|(ident, name)| Some((apt::Source::from_ident(ident)?, name)));
    if let Some((source, name)) = qualified
        && let Some(update) = updates.iter().find(|update| update.source == source && update.name == name)
    {
        return Ok(update);
    }
    let mut named = updates.iter().filter(|update| update.name == text);
    let first = named.next().ok_or(None)?;
    let others: Vec<&&apt::Update> = named.filter(|update| update.source != first.source).collect();
    if others.is_empty() {
        return Ok(first);
    }
    let choices: Vec<String> = std::iter::once(first).chain(others).map(|update| update.qualified()).collect();
    Err(Some(format!("{text} is in more than one source: name one of {}", choices.join(", "))))
}

/// The updates skipped, one `source<TAB>name<TAB>identity` a line.
fn read_skip() -> BTreeMap<apt::Key, String> {
    parse_skip(&std::fs::read_to_string(state_file("updates-skipped")).unwrap_or_default())
}

fn parse_skip(text: &str) -> BTreeMap<apt::Key, String> {
    text.lines()
        .filter_map(|line| {
            let mut fields = line.split('\t');
            let source = apt::Source::from_ident(fields.next()?)?;
            let (name, version) = (fields.next()?, fields.next()?);
            Some(((source, name.to_string()), version.to_string()))
        })
        .collect()
}

fn skip_text(skip: &BTreeMap<apt::Key, String>) -> String {
    skip.iter().map(|((source, name), version)| format!("{}\t{name}\t{version}\n", source.ident())).collect()
}

fn write_skip(skip: &BTreeMap<apt::Key, String>) {
    let path = state_file("updates-skipped");
    let text = skip_text(skip);
    let written = path.parent().map_or(Ok(()), std::fs::create_dir_all).and_then(|()| std::fs::write(&path, text));
    if let Err(error) = written {
        tracing::warn!("could not keep the skipped versions: {error}");
    }
}

/// Whether updates that have waited this long are due for their reminder.
fn reminder_due(waited: Duration, after_days: u32) -> bool {
    waited >= Duration::from_secs(u64::from(after_days) * 24 * 3600)
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
            ..apt::Update::default()
        }
    }

    #[test]
    fn an_install_is_a_task_per_source() {
        let mut flatpak = update("org.mozilla.firefox//stable", false);
        flatpak.source = apt::Source::Flatpak;
        let mut snap = update("core22", false);
        snap.source = apt::Source::Snap;
        let mut device = update("362301da643102b9f38477387e2193e57abaa590", false);
        device.source = apt::Source::Firmware;
        let curl = update("curl", false);
        let tasks = install_tasks(&[&curl, &flatpak, &snap, &device], false);
        assert_eq!(
            tasks,
            [
                Task::Install(vec!["curl".into()]),
                Task::Flatpak { refs: vec!["org.mozilla.firefox//stable".into()], user: false },
                Task::Snap(vec!["core22".into()]),
                Task::Firmware(vec!["362301da643102b9f38477387e2193e57abaa590".into()]),
            ]
        );
        assert_eq!(install_tasks(&[&curl, &snap], true)[0], Task::Install(Vec::new()));
        assert!(install_tasks(&[], true).is_empty());
    }

    #[test]
    fn skipped_versions_wait_for_the_next() {
        let mut updates = Updates::default();
        updates.apps_scanned = true;
        updates.snapshot.updates = vec![update("apt", false), update("curl", false)];
        updates.skip.insert((apt::Source::Apt, "curl".into()), "2".into());
        updates.merge();
        assert_eq!(updates.snapshot.updates.len(), 1);
        assert_eq!(updates.skipped[0].name, "curl");
        // A newer version than the one skipped ends the skip.
        let mut newer = update("curl", false);
        newer.new = "3".into();
        updates.snapshot.updates = vec![update("apt", false), newer];
        updates.skipped.clear();
        updates.merge();
        assert_eq!(updates.snapshot.updates.len(), 2);
        assert!(updates.skipped.is_empty() && updates.skip.is_empty());
    }

    #[test]
    fn skipped_apps_are_listed_once() {
        let mut snap = update("firefox", false);
        snap.source = apt::Source::Snap;
        let mut updates = Updates::default();
        updates.apps_scanned = true;
        updates.apps = vec![snap.clone()];
        updates.snapshot.updates = vec![update("curl", false)];
        updates.skip.insert(snap.key(), snap.new.clone());
        updates.merge();
        // Skipping or releasing something else merges again.
        updates.merge();
        assert_eq!(updates.skipped, vec![snap.clone()]);
        updates.skip.clear();
        updates.merge();
        assert_eq!(updates.skipped, Vec::new());
        assert_eq!(updates.snapshot.updates.iter().filter(|update| update.name == "firefox").count(), 1);
    }

    #[test]
    fn app_skips_wait_for_the_apps_to_be_asked() {
        // A first look offline leaves Flatpak and Snap out.
        let mut updates = Updates::default();
        updates.snapshot.updates = vec![update("curl", false)];
        let snap = (apt::Source::Snap, "firefox".to_string());
        let package = (apt::Source::Apt, "vim".to_string());
        updates.skip.insert(snap.clone(), "2".into());
        updates.skip.insert(package.clone(), "2".into());
        updates.merge();
        assert!(updates.skip.contains_key(&snap));
        assert!(!updates.skip.contains_key(&package), "APT was listed");
        // A look where APT failed proves nothing about APT.
        updates.skip.insert(package.clone(), "2".into());
        updates.snapshot.error = Some("apt is locked".into());
        updates.merge();
        assert!(updates.skip.contains_key(&package));
        // Nor does one where the Snap Store could not be asked.
        updates.snapshot.error = None;
        updates.apps_scanned = true;
        updates.apps_failed = vec![apt::Source::Snap];
        updates.merge();
        assert!(updates.skip.contains_key(&snap));
        updates.apps_failed.clear();
        updates.merge();
        assert!(updates.skip.is_empty());
    }

    #[test]
    fn a_skipped_apt_version_rules_out_a_full_upgrade() {
        let mut updates = Updates::default();
        updates.snapshot.updates = vec![update("apt", false), update("curl", false)];
        assert!(!updates.skips_apt());
        updates.skip.insert((apt::Source::Apt, "curl".into()), "2".into());
        updates.merge();
        assert!(updates.skips_apt());
        let chosen: Vec<&apt::Update> = updates.snapshot.updates.iter().collect();
        assert_eq!(install_tasks(&chosen, !updates.skips_apt()), vec![Task::Install(vec!["apt".into()])]);
    }

    #[test]
    fn the_same_name_in_two_sources_is_skipped_apart() {
        let mut system = update("org.mozilla.firefox//stable", false);
        system.source = apt::Source::Flatpak;
        let mut user = system.clone();
        user.source = apt::Source::FlatpakUser;
        let mut updates = Updates::default();
        updates.apps_scanned = true;
        updates.apps = vec![system.clone(), user.clone()];
        updates.skip.insert(user.key(), user.new.clone());
        updates.merge();
        assert_eq!(updates.snapshot.updates, vec![system.clone()]);
        assert_eq!(updates.skipped, vec![user.clone()]);
        // A name alone is ambiguous only where both sources have it.
        updates.skipped.clear();
        updates.snapshot.updates = vec![system.clone(), user.clone(), update("curl", false)];
        let name = "org.mozilla.firefox//stable";
        assert!(updates.find(name).is_err_and(|error| error.contains("flatpak-user:org.mozilla.firefox//stable")));
        assert_eq!(updates.find(&user.qualified()), Ok(&user));
        assert_eq!(updates.find("apt:curl").map(|update| update.name.as_str()), Ok("curl"));
        assert_eq!(updates.find("curl").map(|update| update.name.as_str()), Ok("curl"));
        assert_eq!(updates.find("vim"), Err("vim has no update".into()));
    }

    #[test]
    fn skips_are_kept_with_their_source() {
        let mut skip = BTreeMap::new();
        skip.insert((apt::Source::Snap, "firefox".to_string()), "133.0-1".to_string());
        skip.insert((apt::Source::Apt, "firefox".to_string()), "1:1snap1".to_string());
        assert_eq!(parse_skip(&skip_text(&skip)), skip);
        assert!(parse_skip("firefox\t2\nnowhere\tx\t2\n").is_empty());
    }

    #[test]
    fn history_answers_since_a_day() {
        let log = "Start-Date: 2026-11-03  06:30:12\nCommandline: /usr/bin/unattended-upgrade\nUpgrade: apt:amd64 (2.8.3, 2.8.4)\nEnd-Date: x\n\n\
                   Start-Date: 2026-11-10  09:12:40\nCommandline: apt-get -y install curl\nRequested-By: robin (1000)\nInstall: curl:amd64 (8.5.0)\nEnd-Date: x\n";
        let mut history = backend::history::parse(log);
        history.reverse();
        let text = history_text(&history, Some("2026-11-10"));
        assert!(text.starts_with("2026-11-10 09:12:40 by robin: 1 installed\n"), "{text}");
        assert!(text.contains("  installed curl 8.5.0\n"));
        assert!(!text.contains("apt 2.8.3"));
        assert_eq!(history_text(&history, Some("2026-11-11")), "apt changed nothing since 2026-11-11\n");
        assert!(history_text(&history, None).contains("updated apt 2.8.3 \u{2192} 2.8.4"));
        assert_eq!(parse(&["history", "since", "2026-11-10"]), Ok(UpdatesCommand::History(Some("2026-11-10".into()))));
        assert!(parse(&["history", "since", "Tuesday"]).is_err());
    }

    #[test]
    fn words_reach_the_shell_whole() {
        let words: Vec<String> = ["timeshift", "--comments", "Tileception: before it's done"].map(String::from).into();
        assert_eq!(shell_words(&words), "timeshift --comments 'Tileception: before it'\\''s done'");
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
    fn failures_keep_the_helpers_reason() {
        let ended = Ended {
            code: Some(1),
            errors: "W: something\ntileception-updates-helper: Unable to lock the package system\n".into(),
        };
        assert_eq!(ended.reason(), "Unable to lock the package system");
        let ended = Ended {
            code: Some(127),
            errors: "Error executing command as another user: Not authorized\n\nThis incident has been reported.\n"
                .into(),
        };
        assert_eq!(ended.reason(), "This incident has been reported.");
        assert_eq!(Ended { code: None, errors: String::new() }.reason(), "the task failed");
        assert!(no_agent(
            "Error creating textual authentication agent: Error opening current controlling terminal for the process"
        ));
        assert!(no_agent("Error executing command as another user: No authentication agent found."));
        assert!(!no_agent("Error executing command as another user: Not authorized"));
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
