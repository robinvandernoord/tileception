//! Tileception as polkit's authentication agent for its session: when a
//! program needs an administrator (`pkexec`, the update manager's helper,
//! a disk tool), polkit asks here and the password prompt opens.
//!
//! The password never goes to the program that asked. It is handed to
//! polkit's own helper, which checks it with PAM and tells polkit: through
//! the socket `/run/polkit/agent-helper.socket` where polkit has one, else
//! by running the setuid `polkit-agent-helper-1`.
//!
//! The agent lives on its own thread with its own connection to the system
//! bus. `[shell] polkit = "none"` leaves the job to another agent.

use std::collections::{HashMap, VecDeque};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex, MutexGuard, mpsc};

use smithay::reexports::calloop::{RegistrationToken, channel};
use zbus::zvariant::{OwnedValue, Value};
use zeroize::Zeroizing;

use crate::{
    Tileception,
    password::{Prompt, Purpose},
};

const AUTHORITY: &str = "org.freedesktop.PolicyKit1";
const AUTHORITY_PATH: &str = "/org/freedesktop/PolicyKit1/Authority";
const AUTHORITY_INTERFACE: &str = "org.freedesktop.PolicyKit1.Authority";
const AGENT_PATH: &str = "/org/tileception/PolkitAgent";

/// Where polkit listens for agents on releases without a setuid helper.
const HELPER_SOCKET: &str = "/run/polkit/agent-helper.socket";
/// Where distributions put the setuid helper.
const HELPERS: [&str; 3] = [
    "/usr/lib/polkit-1/polkit-agent-helper-1",
    "/usr/libexec/polkit-agent-helper-1",
    "/usr/lib/policykit-1/polkit-agent-helper-1",
];

/// What the person answered.
enum Answer {
    Password(Zeroizing<String>),
    Cancel,
}

/// What the agent tells the compositor.
pub enum Event {
    /// Polkit asks for this account's password.
    Asked { cookie: String, message: String, user: String },
    /// The password did not work; the person may try again.
    Rejected { cookie: String, note: String },
    /// The authentication ended, whichever way.
    Over { cookie: String },
}

/// What the bus thread and the compositor share.
struct Shared {
    /// By cookie: where the answer to a running authentication goes.
    answers: Mutex<HashMap<String, async_channel::Sender<Answer>>>,
    events: Mutex<channel::Sender<Event>>,
}

impl Shared {
    fn answers(&self) -> MutexGuard<'_, HashMap<String, async_channel::Sender<Answer>>> {
        // Plain data, so a panic elsewhere cannot leave it half written.
        self.answers.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn tell(&self, event: Event) {
        let _ = self.events.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).send(event);
    }
}

/// An authentication waiting for its turn at the prompt.
struct Asked {
    cookie: String,
    message: String,
    user: String,
}

/// The compositor's side of the agent.
#[derive(Default)]
pub struct Polkit {
    shared: Option<Arc<Shared>>,
    source: Option<RegistrationToken>,
    /// Ends the bus thread, which then gives the registration up.
    stop: Option<mpsc::Sender<()>>,
    waiting: VecDeque<Asked>,
}

#[derive(Debug, zbus::DBusError)]
#[zbus(prefix = "org.freedesktop.PolicyKit1.Error")]
enum AgentError {
    #[zbus(error)]
    ZBus(zbus::Error),
    Cancelled(String),
    Failed(String),
}

struct Agent(Arc<Shared>);

#[zbus::interface(name = "org.freedesktop.PolicyKit1.AuthenticationAgent")]
impl Agent {
    async fn begin_authentication(
        &self,
        action_id: &str,
        message: &str,
        _icon_name: &str,
        _details: HashMap<String, String>,
        cookie: &str,
        identities: Vec<(String, HashMap<String, OwnedValue>)>,
    ) -> Result<(), AgentError> {
        let Some(user) = identity(&identities) else {
            return Err(AgentError::Failed("none of the accounts that may authenticate is a user".into()));
        };
        tracing::info!("polkit asks for {user}'s password: {action_id}");
        let (sender, answers) = async_channel::bounded(1);
        self.0.answers().insert(cookie.to_string(), sender);
        self.0.tell(Event::Asked { cookie: cookie.to_string(), message: message.to_string(), user: user.clone() });
        let result = loop {
            let Ok(Answer::Password(password)) = answers.recv().await else {
                break Err(AgentError::Cancelled("the authentication was cancelled".into()));
            };
            // PAM takes its time, above all to say no: off this thread, so
            // that polkit can still cancel.
            let (done, verdict) = async_channel::bounded(1);
            let (user, secret) = (user.clone(), cookie.to_string());
            std::thread::spawn(move || {
                let _ = done.send_blocking(converse(&user, &secret, &password));
            });
            match verdict.recv().await {
                Ok(Verdict::Accepted) => break Ok(()),
                Ok(Verdict::Rejected(note)) => self.0.tell(Event::Rejected { cookie: cookie.to_string(), note }),
                Ok(Verdict::Broken(reason)) => break Err(AgentError::Failed(reason)),
                Err(_) => break Err(AgentError::Failed("the password check ended early".into())),
            }
        };
        self.0.answers().remove(cookie);
        self.0.tell(Event::Over { cookie: cookie.to_string() });
        result
    }

    async fn cancel_authentication(&self, cookie: &str) {
        if let Some(sender) = self.0.answers().get(cookie) {
            let _ = sender.try_send(Answer::Cancel);
        }
    }
}

/// The account to ask: the person's own when it may authenticate, else the
/// first user polkit names (root, where nobody is an administrator).
fn identity(identities: &[(String, HashMap<String, OwnedValue>)]) -> Option<String> {
    let users: Vec<u32> = identities
        .iter()
        .filter(|(kind, _)| kind == "unix-user")
        .filter_map(|(_, details)| details.get("uid").and_then(|uid| uid.downcast_ref::<u32>().ok()))
        .collect();
    let own = own_uid();
    let uid = users.iter().copied().find(|uid| Some(*uid) == own).or_else(|| users.first().copied())?;
    user_name(uid)
}

fn own_uid() -> Option<u32> {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata("/proc/self").ok().map(|meta| meta.uid())
}

/// The name in a line of `passwd` with this uid.
fn name_in(line: &str, uid: u32) -> Option<String> {
    let mut fields = line.split(':');
    let name = fields.next()?;
    (fields.nth(1)?.parse() == Ok(uid) && !name.is_empty()).then(|| name.to_string())
}

fn user_name(uid: u32) -> Option<String> {
    let local =
        std::fs::read_to_string("/etc/passwd").ok().and_then(|text| text.lines().find_map(|line| name_in(line, uid)));
    // Accounts from a directory are not in the file.
    local.or_else(|| {
        let output = Command::new("getent").args(["passwd", &uid.to_string()]).output().ok()?;
        String::from_utf8_lossy(&output.stdout).lines().find_map(|line| name_in(line, uid))
    })
}

/// How polkit's helper judged a password.
#[derive(Debug, PartialEq, Eq)]
enum Verdict {
    Accepted,
    /// Wrong, with what PAM said about it.
    Rejected(String),
    /// The helper could not be used.
    Broken(String),
}

/// A conversation with polkit's helper: where to write, what to read, and
/// the process to end when it is one.
struct Helper {
    input: Box<dyn Write>,
    output: Box<dyn BufRead>,
    child: Option<Child>,
}

impl Helper {
    /// Through polkit's socket where there is one, else the setuid helper.
    fn start(user: &str, cookie: &str) -> Result<Helper, String> {
        if Path::new(HELPER_SOCKET).exists()
            && let Ok(stream) = UnixStream::connect(HELPER_SOCKET)
            && let Ok(reader) = stream.try_clone()
        {
            let mut helper = Helper { input: Box::new(stream), output: Box::new(BufReader::new(reader)), child: None };
            helper.say(user)?;
            helper.say(cookie)?;
            return Ok(helper);
        }
        let path =
            HELPERS.iter().find(|path| Path::new(path).exists()).ok_or("polkit's agent helper is not installed")?;
        let mut child = Command::new(path)
            .arg(user)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|error| format!("could not start polkit's agent helper: {error}"))?;
        let (Some(input), Some(output)) = (child.stdin.take(), child.stdout.take()) else {
            return Err("polkit's agent helper has no pipes".into());
        };
        let mut helper =
            Helper { input: Box::new(input), output: Box::new(BufReader::new(output)), child: Some(child) };
        // On its standard input, where other processes cannot see it.
        helper.say(cookie)?;
        Ok(helper)
    }

    fn say(&mut self, line: &str) -> Result<(), String> {
        self.input
            .write_all(line.as_bytes())
            .and_then(|()| self.input.write_all(b"\n"))
            .and_then(|()| self.input.flush())
            .map_err(|error| format!("polkit's agent helper stopped listening: {error}"))
    }
}

impl Drop for Helper {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// What the helper says in one line.
#[derive(Debug, PartialEq, Eq)]
enum Said {
    /// It wants something typed unseen: the password.
    Secret,
    /// It wants something typed in the open, which this prompt cannot ask.
    Open,
    Note(String),
    Success,
    Failure,
    Unknown,
}

fn said(line: &str) -> Said {
    let line = line.trim_end_matches(['\n', '\r']);
    if line.starts_with("PAM_PROMPT_ECHO_OFF") {
        Said::Secret
    } else if line.starts_with("PAM_PROMPT_ECHO_ON") {
        Said::Open
    } else if let Some(note) = line.strip_prefix("PAM_ERROR_MSG ").or_else(|| line.strip_prefix("PAM_TEXT_INFO ")) {
        Said::Note(note.trim().to_string())
    } else if line == "SUCCESS" {
        Said::Success
    } else if line == "FAILURE" {
        Said::Failure
    } else {
        Said::Unknown
    }
}

/// Lets polkit's helper check a password for one authentication.
fn converse(user: &str, cookie: &str, password: &str) -> Verdict {
    const WRONG: &str = "That password did not work.";
    // A line break would end the password early and answer the next question.
    if password.contains(['\n', '\r']) {
        return Verdict::Rejected(WRONG.into());
    }
    let mut helper = match Helper::start(user, cookie) {
        Ok(helper) => helper,
        Err(reason) => return Verdict::Broken(reason),
    };
    let mut answered = false;
    let mut line = String::new();
    loop {
        line.clear();
        if helper.output.read_line(&mut line).unwrap_or(0) == 0 {
            return Verdict::Rejected(WRONG.into());
        }
        match said(&line) {
            Said::Secret if !answered => {
                answered = true;
                if let Err(reason) = helper.say(password) {
                    return Verdict::Broken(reason);
                }
            }
            // A second question is not about this password: a new one, or a code.
            Said::Secret | Said::Open => {
                return Verdict::Broken(
                    "This account asks for more than a password, which Tileception cannot enter.".into(),
                );
            }
            Said::Note(_) | Said::Unknown => {}
            Said::Success => return Verdict::Accepted,
            Said::Failure => return Verdict::Rejected(WRONG.into()),
        }
    }
}

/// When this process started, as the kernel counts it: polkit tells
/// processes with one number apart by it.
fn start_time() -> Option<u64> {
    let stat = std::fs::read_to_string("/proc/self/stat").ok()?;
    // The name, in brackets, may hold spaces: count from behind it.
    stat.rsplit_once(')')?.1.split_whitespace().nth(19)?.parse().ok()
}

/// Registers as the agent of this session, or of this process and what it
/// starts where there is no session (a container, a nested instance).
fn register(shared: &Arc<Shared>) -> zbus::Result<zbus::blocking::Connection> {
    let connection =
        zbus::blocking::connection::Builder::system()?.serve_at(AGENT_PATH, Agent(shared.clone()))?.build()?;
    let mut details: HashMap<&str, Value<'_>> = HashMap::new();
    let kind = match std::env::var("XDG_SESSION_ID").ok().filter(|session| !session.is_empty()) {
        Some(session) => {
            details.insert("session-id", Value::from(session));
            "unix-session"
        }
        None => {
            details.insert("pid", Value::from(std::process::id()));
            details.insert("start-time", Value::from(start_time().unwrap_or(0)));
            "unix-process"
        }
    };
    let locale = std::env::var("LANG").unwrap_or_else(|_| "C".into());
    connection.call_method(
        Some(AUTHORITY),
        AUTHORITY_PATH,
        Some(AUTHORITY_INTERFACE),
        "RegisterAuthenticationAgent",
        &((kind, details), locale.as_str(), AGENT_PATH),
    )?;
    Ok(connection)
}

fn serve(shared: &Arc<Shared>, stop: &mpsc::Receiver<()>) {
    let connection = match register(shared) {
        Ok(connection) => connection,
        // Another agent has the session, or there is no polkit: both fine.
        Err(error) => return tracing::info!("not the polkit authentication agent: {error}"),
    };
    tracing::info!("registered as the polkit authentication agent");
    // Until the compositor says stop, or goes away.
    let _ = stop.recv();
    drop(connection);
}

impl Tileception {
    /// Becomes the session's polkit agent, unless `[shell]` leaves that to
    /// another one or one is registered already.
    pub fn start_polkit(&mut self) {
        if self.polkit.shared.is_some() || !self.shell().polkit() {
            return;
        }
        let (sender, receiver) = channel::channel::<Event>();
        let token = self.loop_handle.insert_source(receiver, |event, _, state| {
            if let channel::Event::Msg(event) = event {
                state.polkit_event(event);
            }
        });
        let token = match token {
            Ok(token) => token,
            Err(error) => return tracing::error!("could not listen for polkit: {error}"),
        };
        let shared = Arc::new(Shared { answers: Mutex::default(), events: Mutex::new(sender) });
        let (stop, stopped) = mpsc::channel();
        let serving = shared.clone();
        let thread =
            std::thread::Builder::new().name("tileception-polkit".into()).spawn(move || serve(&serving, &stopped));
        if let Err(error) = thread {
            self.loop_handle.remove(token);
            return tracing::error!("could not start the polkit agent: {error}");
        }
        self.polkit.shared = Some(shared);
        self.polkit.source = Some(token);
        self.polkit.stop = Some(stop);
    }

    /// Gives the registration up, for another agent to take.
    pub fn stop_polkit(&mut self) {
        let waiting: Vec<String> = self.polkit.waiting.drain(..).map(|asked| asked.cookie).collect();
        if let Some(shared) = &self.polkit.shared {
            for sender in shared.answers().values() {
                let _ = sender.try_send(Answer::Cancel);
            }
        }
        if self.password_prompt.as_ref().is_some_and(|prompt| matches!(prompt.purpose(), Purpose::Polkit(_))) {
            self.close_password_prompt();
        }
        drop(waiting);
        self.polkit.stop = None;
        self.polkit.shared = None;
        if let Some(token) = self.polkit.source.take() {
            self.loop_handle.remove(token);
        }
    }

    fn polkit_event(&mut self, event: Event) {
        match event {
            Event::Asked { cookie, message, user } => self.polkit.waiting.push_back(Asked { cookie, message, user }),
            Event::Rejected { cookie, note } => {
                if let Some(prompt) = self.password_prompt.as_mut()
                    && *prompt.purpose() == Purpose::Polkit(cookie)
                {
                    prompt.retry(note);
                    self.schedule_render();
                }
            }
            Event::Over { cookie } => {
                self.polkit.waiting.retain(|asked| asked.cookie != cookie);
                if self.password_prompt.as_ref().is_some_and(|prompt| *prompt.purpose() == Purpose::Polkit(cookie)) {
                    self.close_password_prompt();
                }
            }
        }
        self.polkit_next();
    }

    /// Shows the next authentication that waits, once the prompt is free.
    pub(crate) fn polkit_next(&mut self) {
        if self.password_prompt.is_some() {
            return;
        }
        let Some(asked) = self.polkit.waiting.front() else { return };
        let prompt =
            Prompt::new(Purpose::Polkit(asked.cookie.clone()), "Authentication required", &asked.message, &asked.user);
        self.open_password_prompt(prompt);
    }

    /// Hands the person's answer to the authentication with this cookie.
    pub(crate) fn polkit_answer(&mut self, cookie: &str, password: Option<Zeroizing<String>>) {
        let answer = match password {
            Some(password) => Answer::Password(password),
            None => {
                // Not asked again: polkit hears the refusal.
                self.polkit.waiting.retain(|asked| asked.cookie != cookie);
                Answer::Cancel
            }
        };
        if let Some(shared) = &self.polkit.shared
            && let Some(sender) = shared.answers().get(cookie)
        {
            let _ = sender.try_send(answer);
        }
        self.polkit_next();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_helpers_lines_are_understood() {
        assert_eq!(said("PAM_PROMPT_ECHO_OFF Password: \n"), Said::Secret);
        assert_eq!(said("PAM_PROMPT_ECHO_ON login:\n"), Said::Open);
        assert_eq!(said("PAM_ERROR_MSG Account locked\n"), Said::Note("Account locked".into()));
        assert_eq!(said("PAM_TEXT_INFO Place your finger\n"), Said::Note("Place your finger".into()));
        assert_eq!(said("SUCCESS\n"), Said::Success);
        assert_eq!(said("FAILURE\n"), Said::Failure);
        assert_eq!(said("SUCCESSFUL\n"), Said::Unknown);
    }

    #[test]
    fn passwd_lines_name_users() {
        assert_eq!(name_in("robin:x:1000:1000:Robin:/home/robin:/bin/bash", 1000), Some("robin".into()));
        assert_eq!(name_in("robin:x:1000:1000:Robin:/home/robin:/bin/bash", 0), None);
        assert_eq!(name_in("broken", 0), None);
    }

    #[test]
    fn a_password_cannot_answer_twice() {
        assert_eq!(converse("nobody", "cookie", "one\ntwo"), Verdict::Rejected("That password did not work.".into()));
    }

    #[test]
    fn this_process_has_a_start_time() {
        assert!(start_time().is_some_and(|ticks| ticks > 0));
    }
}
