//! Updates of the machines linked through handoff. Each machine tells the
//! others what waits in its update manager, and their update manager's card
//! lists it under their own, with a button that asks for an install there.
//!
//! Asking is all another machine can do. The machine asked puts the question
//! to the person at it, and only their answer there starts the install, which
//! then runs as any install there does: through its own polkit or `sudo`
//! prompt. Nothing another machine sends is ever a reason to skip either.

use std::collections::BTreeMap;

use tileception_handoff::{MAX_UPDATE_NAMES, Message, PeerId, PendingUpdates, UPDATE_TEXT, UpdatesTask};

use super::Link;
use crate::{
    Tileception,
    updates::{Tone, Updates},
};

/// The keys of the question an install asked from elsewhere puts here.
const INSTALL: &str = "install";
const LATER: &str = "later";

#[derive(Default)]
pub(crate) struct RemoteUpdates {
    /// What waits on each linked machine, as it last said.
    pub(crate) machines: BTreeMap<PeerId, Machine>,
    /// What this machine last told each one, so only changes are sent.
    sent: BTreeMap<PeerId, Option<PendingUpdates>>,
    /// An install another machine asked for, waiting for the person here.
    asked: Option<Asked>,
}

pub(crate) struct Machine {
    pub(crate) pending: PendingUpdates,
    /// About an install asked for from here: waiting for it to be taken up
    /// (Ok), or why it was not (Err).
    pub(crate) note: Option<Result<String, String>>,
}

struct Asked {
    notification: u32,
    peer: PeerId,
    security_only: bool,
}

/// One machine's line in the card.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Row {
    pub(crate) title: String,
    pub(crate) detail: String,
    /// The detail is a failure.
    pub(crate) error: bool,
    /// An install may be asked for now.
    pub(crate) install: bool,
}

fn clip(text: &str) -> String {
    text.chars().take(UPDATE_TEXT).collect()
}

/// What waits here, for the other machines.
pub(crate) fn summary(updates: &Updates, asking: bool) -> PendingUpdates {
    let snapshot = &updates.snapshot;
    let totals = snapshot.totals();
    let mut names: Vec<String> = Vec::new();
    for update in &snapshot.updates {
        let label = clip(update.label());
        if names.len() < MAX_UPDATE_NAMES && !names.contains(&label) {
            names.push(label);
        }
    }
    PendingUpdates {
        count: u32::try_from(totals.count).unwrap_or(u32::MAX),
        security: u32::try_from(totals.security).unwrap_or(u32::MAX),
        bytes: totals.bytes,
        names,
        restart: snapshot.restart,
        error: snapshot.error.as_deref().map(clip),
        task: updates.progress().map(|(title, _, fraction)| UpdatesTask {
            title: clip(title),
            percent: (fraction * 100.0).round().clamp(0.0, 100.0) as u8,
        }),
        asking,
        outcome: updates.outcome().map(|outcome| outcome.as_ref().map(|text| clip(text)).map_err(|text| clip(text))),
    }
}

/// How a machine's line in the card reads, and whether it offers an install.
pub(crate) fn row(name: &str, pending: &PendingUpdates, note: Option<&Result<String, String>>) -> Row {
    let title = match (pending.count, pending.security) {
        (0, _) if pending.error.is_some() => name.to_string(),
        (0, _) => format!("{name}: up to date"),
        (1, 0) => format!("{name}: 1 update"),
        (count, 0) => format!("{name}: {count} updates"),
        (count, security) => format!("{name}: {count} updates, {security} for security"),
    };
    let quiet = |detail: String| (detail, false);
    let (detail, error) = if let Some(Err(reason)) = note.filter(|_| pending.task.is_none() && !pending.asking) {
        (reason.clone(), true)
    } else if let Some(task) = &pending.task {
        quiet(format!("{} {}%", task.title, task.percent))
    } else if pending.asking {
        quiet(format!("Waiting for an answer on {name}"))
    } else if let Some(Ok(waiting)) = note {
        quiet(waiting.clone())
    } else if let Some(error) = &pending.error {
        (error.clone(), true)
    } else if let Some(Err(reason)) = &pending.outcome {
        (reason.clone(), true)
    } else if pending.count > 0 {
        let more = (pending.count as usize).saturating_sub(pending.names.len());
        quiet(match more {
            0 => pending.names.join(", "),
            more => format!("{}, {more} more", pending.names.join(", ")),
        })
    } else if pending.restart {
        quiet("Restart to finish earlier updates".into())
    } else if let Some(Ok(message)) = &pending.outcome {
        quiet(message.clone())
    } else {
        quiet("Nothing waits".into())
    };
    let install = pending.count > 0 && pending.task.is_none() && !pending.asking && !matches!(note, Some(Ok(_)));
    Row { title, detail, error, install }
}

/// The bar chip's text when nothing waits here but on other machines.
pub(crate) fn chip<'list>(
    machines: impl IntoIterator<Item = (&'list str, &'list PendingUpdates)>,
) -> Option<(String, Tone)> {
    let waiting: Vec<(&str, &PendingUpdates)> = machines.into_iter().filter(|(_, pending)| pending.count > 0).collect();
    let count: u64 = waiting.iter().map(|(_, pending)| u64::from(pending.count)).sum();
    let tone = if waiting.iter().any(|(_, pending)| pending.security > 0) { Tone::Warning } else { Tone::Accent };
    match waiting.as_slice() {
        [] => None,
        [(name, _)] => {
            let name: String = name.chars().take(16).collect();
            Some((format!("{count} on {name}"), tone))
        }
        _ => Some((format!("{count} elsewhere"), tone)),
    }
}

impl Tileception {
    fn sharing_with(&self, peer: PeerId) -> bool {
        self.handoff.peers.get(&peer).is_some_and(|entry| entry.link != Link::Dismissed)
    }

    /// Tells the linked machines what waits here, where that changed since
    /// they were last told. Machines this user said no to are left out.
    pub(crate) fn share_updates(&mut self) {
        if self.handoff.peers.is_empty() {
            return;
        }
        let pending = self.updates_available().then(|| summary(&self.updates, self.handoff.updates.asked.is_some()));
        let peers: Vec<PeerId> = self.handoff.peers.keys().copied().filter(|peer| self.sharing_with(*peer)).collect();
        for peer in peers {
            if self.handoff.updates.sent.get(&peer) == Some(&pending) {
                continue;
            }
            if self.handoff.send(peer, Message::Updates(pending.clone())) {
                self.handoff.updates.sent.insert(peer, pending.clone());
            }
        }
    }

    /// A machine disconnected or was dismissed: its updates leave the card,
    /// and a question it asked here is taken back.
    pub(crate) fn updates_peer_gone(&mut self, peer: PeerId) {
        let remote = &mut self.handoff.updates;
        remote.machines.remove(&peer);
        remote.sent.remove(&peer);
        if let Some(asked) = remote.asked.take_if(|asked| asked.peer == peer) {
            self.withdraw_question(asked.notification);
            self.share_updates();
        }
        self.schedule_render();
    }

    /// Forgets every machine's updates, as handoff stops.
    pub(crate) fn updates_handoff_stopped(&mut self) {
        if let Some(asked) = self.handoff.updates.asked.take() {
            self.withdraw_question(asked.notification);
        }
        self.handoff.updates = RemoteUpdates::default();
    }

    pub(crate) fn updates_message(&mut self, peer: PeerId, message: Message) {
        if !self.sharing_with(peer) {
            return;
        }
        match message {
            Message::Updates(Some(pending)) => {
                let machine = self
                    .handoff
                    .updates
                    .machines
                    .entry(peer)
                    .or_insert_with(|| Machine { pending: PendingUpdates::default(), note: None });
                machine.pending = pending;
                // Asking the person there, or installing, answers a request
                // that was waiting and puts an earlier refusal away. A
                // refusal comes as `UpdatesDeclined` instead.
                if machine.pending.task.is_some() || machine.pending.asking {
                    machine.note = None;
                }
            }
            Message::Updates(None) => {
                self.handoff.updates.machines.remove(&peer);
            }
            Message::InstallUpdates { security_only } => self.install_asked(peer, security_only),
            Message::UpdatesDeclined { reason } => {
                if let Some(machine) = self.handoff.updates.machines.get_mut(&peer) {
                    machine.note = Some(Err(clip(&reason)));
                }
            }
            _ => {}
        }
        self.schedule_render();
    }

    /// Another machine asks for an install here: the person here is asked.
    fn install_asked(&mut self, peer: PeerId, security_only: bool) {
        let name = self.handoff.peers.get(&peer).map(|entry| entry.name.clone()).unwrap_or_default();
        let totals = self.updates.snapshot.totals();
        let count = if security_only { totals.security } else { totals.count };
        let refusal = if !self.updates_available() {
            Some("Its update manager is not running.".to_string())
        } else if self.is_locked() {
            Some("It is locked.".into())
        } else if self.handoff.updates.asked.is_some() {
            Some("It is already asking about an install.".into())
        } else if let Some((title, ..)) = self.updates.progress() {
            Some(format!("It is busy: {title}."))
        } else if count == 0 {
            Some("Nothing waits there.".into())
        } else {
            None
        };
        if let Some(reason) = refusal {
            self.handoff.send(peer, Message::UpdatesDeclined { reason });
            return;
        }
        let what = match (security_only, count) {
            (true, 1) => "1 security update".to_string(),
            (true, count) => format!("{count} security updates"),
            (false, 1) => "1 update".to_string(),
            (false, count) => format!("{count} updates"),
        };
        let summary = format!("{name} asks to install updates");
        let body =
            format!("Install {what} on this computer? Installing asks for an administrator here, as it always does.");
        let actions = vec![(INSTALL.to_string(), "Install".to_string()), (LATER.to_string(), "Not now".to_string())];
        let notification = self.ask_locally(&summary, &body, None, actions);
        self.handoff.updates.asked = Some(Asked { notification, peer, security_only });
        self.share_updates();
    }

    /// The person here answered an install another machine asked for, or
    /// closed the question (None). Only an answer here installs, through
    /// this machine's own administrator prompt.
    pub(crate) fn answer_install_question(&mut self, notification: u32, key: Option<&str>) {
        let Some(asked) = self.handoff.updates.asked.take_if(|asked| asked.notification == notification) else {
            return;
        };
        let result = match key {
            Some(INSTALL) if asked.security_only => self.updates_install(true),
            Some(INSTALL) => self.updates_install_reviewed(),
            _ => Err("Not now, said the person there.".into()),
        };
        if let Err(reason) = result {
            self.handoff.send(asked.peer, Message::UpdatesDeclined { reason });
        }
        self.share_updates();
    }

    /// Asks another machine to install what waits there. It puts the
    /// question to the person there; the card says how it went, and why
    /// when it could not be asked.
    pub(crate) fn install_on(&mut self, peer: PeerId, security_only: bool) -> Result<(), String> {
        let asked = self.ask_to_install(peer, security_only);
        if let (Err(reason), Some(machine)) = (&asked, self.handoff.updates.machines.get_mut(&peer)) {
            machine.note = Some(Err(reason.clone()));
            self.schedule_render();
        }
        asked
    }

    fn ask_to_install(&mut self, peer: PeerId, security_only: bool) -> Result<(), String> {
        let name = self.handoff.peers.get(&peer).map(|entry| entry.name.clone()).ok_or("that machine is gone")?;
        let Some(machine) = self.handoff.updates.machines.get(&peer) else {
            return Err(format!("{name} has not said what updates wait there"));
        };
        let wanted = if security_only { machine.pending.security } else { machine.pending.count };
        if !row(&name, &machine.pending, machine.note.as_ref()).install || wanted == 0 {
            return Err(match (security_only, wanted) {
                (true, 0) => format!("{name} has no security updates waiting"),
                (false, 0) => format!("{name} has no updates waiting"),
                _ => format!("{name} cannot take an install now"),
            });
        }
        if !self.handoff.send(peer, Message::InstallUpdates { security_only }) {
            return Err(format!("{name} could not be reached"));
        }
        if let Some(machine) = self.handoff.updates.machines.get_mut(&peer) {
            machine.note = Some(Ok(format!("Asked; answer on {name}")));
        }
        self.schedule_render();
        Ok(())
    }

    /// The other machines' lines for the card, in the order of their ids.
    pub(crate) fn remote_update_rows(&self) -> Vec<(PeerId, Row)> {
        self.handoff
            .updates
            .machines
            .iter()
            .filter_map(|(peer, machine)| {
                let name = &self.handoff.peers.get(peer)?.name;
                Some((*peer, row(name, &machine.pending, machine.note.as_ref())))
            })
            .collect()
    }

    /// The updates chip: this machine's, else what waits elsewhere.
    pub(crate) fn updates_chip(&self) -> Option<(String, Tone)> {
        self.updates.chip().or_else(|| {
            chip(
                self.handoff.updates.machines.iter().filter_map(|(peer, machine)| {
                    Some((self.handoff.peers.get(peer)?.name.as_str(), &machine.pending))
                }),
            )
        })
    }

    /// `handoff updates`: what waits on each linked machine.
    pub(crate) fn remote_updates_status(&self) -> String {
        let rows = self.remote_update_rows();
        if rows.is_empty() {
            return "no linked machine has said what updates wait there\n".into();
        }
        rows.iter().map(|(_, row)| format!("{}: {}\n", row.title, row.detail)).collect()
    }
}

#[cfg(test)]
mod tests {
    use tileception_updates::apt;

    use super::*;

    fn update(name: &str, title: &str, security: bool) -> apt::Update {
        apt::Update {
            name: name.into(),
            title: title.into(),
            new: "2".into(),
            security,
            size: 1_000,
            ..apt::Update::default()
        }
    }

    fn pending(count: u32, security: u32, names: &[&str]) -> PendingUpdates {
        PendingUpdates {
            count,
            security,
            names: names.iter().map(|name| (*name).to_string()).collect(),
            ..PendingUpdates::default()
        }
    }

    #[test]
    fn the_summary_names_the_first_few_once() {
        let mut updates = Updates::default();
        updates.snapshot.updates = vec![
            update("openssl", "", true),
            update("libssl3", "", true),
            update("org.mozilla.firefox//stable", "Firefox", false),
            update("org.mozilla.firefox//beta", "Firefox", false),
            update("curl", "", false),
        ];
        let summary = summary(&updates, true);
        assert_eq!(summary.count, 5);
        assert_eq!(summary.security, 2);
        assert_eq!(summary.bytes, 5_000);
        assert_eq!(summary.names, ["openssl", "libssl3", "Firefox"]);
        assert!(summary.asking);
        assert_eq!(summary.task, None);
    }

    #[test]
    fn the_summary_clips_long_texts() {
        let mut updates = Updates::default();
        updates.snapshot.error = Some("x".repeat(UPDATE_TEXT * 2));
        let summary = summary(&updates, false);
        assert_eq!(summary.error.map(|error| error.chars().count()), Some(UPDATE_TEXT));
    }

    #[test]
    fn a_row_says_what_waits_and_offers_an_install() {
        let row = row("laptop", &pending(4, 1, &["openssl", "curl"]), None);
        assert_eq!(row.title, "laptop: 4 updates, 1 for security");
        assert_eq!(row.detail, "openssl, curl, 2 more");
        assert!(row.install && !row.error);
        let row = super::row("laptop", &pending(1, 0, &["curl"]), None);
        assert_eq!((row.title.as_str(), row.detail.as_str()), ("laptop: 1 update", "curl"));
    }

    #[test]
    fn a_row_with_nothing_waiting_offers_nothing() {
        let row = row("laptop", &PendingUpdates::default(), None);
        assert_eq!(
            (row.title.as_str(), row.detail.as_str(), row.install),
            ("laptop: up to date", "Nothing waits", false)
        );
        let restart = PendingUpdates { restart: true, ..PendingUpdates::default() };
        assert_eq!(super::row("laptop", &restart, None).detail, "Restart to finish earlier updates");
        let installed = PendingUpdates { outcome: Some(Ok("Updates installed.".into())), ..PendingUpdates::default() };
        assert_eq!(super::row("laptop", &installed, None).detail, "Updates installed.");
        let broken = PendingUpdates { error: Some("apt-get failed".into()), ..PendingUpdates::default() };
        let row = super::row("laptop", &broken, None);
        assert_eq!((row.title.as_str(), row.detail.as_str(), row.error), ("laptop", "apt-get failed", true));
    }

    #[test]
    fn no_install_is_offered_while_one_runs_or_is_asked_about() {
        let mut busy = pending(3, 0, &["curl"]);
        busy.task = Some(UpdatesTask { title: "Install updates".into(), percent: 40 });
        let row = row("laptop", &busy, None);
        assert_eq!((row.detail.as_str(), row.install), ("Install updates 40%", false));
        let asking = PendingUpdates { asking: true, ..pending(3, 0, &["curl"]) };
        let row = super::row("laptop", &asking, None);
        assert_eq!((row.detail.as_str(), row.install), ("Waiting for an answer on laptop", false));
        let waiting = Ok("Asked; answer on laptop".to_string());
        let row = super::row("laptop", &pending(3, 0, &["curl"]), Some(&waiting));
        assert_eq!((row.detail.as_str(), row.install), ("Asked; answer on laptop", false));
    }

    #[test]
    fn a_refusal_shows_until_the_next_install() {
        let refused = Err("It is locked.".to_string());
        let row = row("laptop", &pending(3, 0, &["curl"]), Some(&refused));
        assert_eq!((row.detail.as_str(), row.error, row.install), ("It is locked.", true, true));
        let mut busy = pending(3, 0, &["curl"]);
        busy.task = Some(UpdatesTask { title: "Install updates".into(), percent: 5 });
        assert_eq!(super::row("laptop", &busy, Some(&refused)).detail, "Install updates 5%");
    }

    #[test]
    fn a_failure_there_shows_before_what_waits() {
        let failed = PendingUpdates { outcome: Some(Err("dpkg was interrupted".into())), ..pending(2, 0, &["a", "b"]) };
        let row = row("laptop", &failed, None);
        assert_eq!((row.detail.as_str(), row.error, row.install), ("dpkg was interrupted", true, true));
    }

    #[test]
    fn the_chip_counts_what_waits_elsewhere() {
        let up_to_date = PendingUpdates::default();
        assert_eq!(chip([("laptop", &up_to_date)]), None);
        let some = pending(3, 0, &[]);
        assert_eq!(chip([("laptop", &some), ("tv", &up_to_date)]), Some(("3 on laptop".into(), Tone::Accent)));
        let urgent = pending(2, 1, &[]);
        assert_eq!(chip([("laptop", &some), ("tv", &urgent)]), Some(("5 elsewhere".into(), Tone::Warning)));
    }
}
