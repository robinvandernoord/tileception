//! greetd's IPC: JSON messages, each after its length as a native-endian u32,
//! over the Unix socket in `GREETD_SOCK`. Every request gets one response.
//!
//! The socket is used from a worker thread, because answering a prompt blocks
//! for as long as PAM takes (seconds after a wrong password).

use std::{
    io::{Read, Write},
    os::unix::net::UnixStream,
    sync::mpsc,
};

use serde::{Deserialize, Serialize, Serializer};
use zeroize::Zeroizing;

/// What the greeter asks greetd.
#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Request {
    CreateSession {
        username: String,
    },
    /// The answer to the last prompt; none for an info or error message.
    PostAuthMessageResponse {
        #[serde(serialize_with = "secret")]
        response: Option<Zeroizing<String>>,
    },
    /// A shell command line (greetd runs it with `/bin/sh -c`) and `KEY=value` pairs.
    StartSession {
        cmd: Vec<String>,
        env: Vec<String>,
    },
    CancelSession,
}

fn secret<S: Serializer>(value: &Option<Zeroizing<String>>, serializer: S) -> Result<S::Ok, S::Error> {
    match value {
        Some(text) => serializer.serialize_some(text.as_str()),
        None => serializer.serialize_none(),
    }
}

impl std::fmt::Debug for Request {
    /// Never shows an answer: it may be a password.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Request::CreateSession { username } => write!(formatter, "CreateSession({username:?})"),
            Request::PostAuthMessageResponse { response } => {
                write!(formatter, "PostAuthMessageResponse({})", if response.is_some() { "answer" } else { "none" })
            }
            Request::StartSession { cmd, env } => write!(formatter, "StartSession({cmd:?}, {env:?})"),
            Request::CancelSession => write!(formatter, "CancelSession"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Response {
    Success,
    Error { error_type: ErrorType, description: String },
    AuthMessage { auth_message_type: Prompt, auth_message: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorType {
    /// The answers were wrong; the session must be cancelled before another try.
    AuthError,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Prompt {
    /// A question whose answer may be shown, such as a username.
    Visible,
    /// A question whose answer must be hidden, such as a password.
    Secret,
    Info,
    Error,
}

/// Writes one message: its length, then the JSON.
fn send(stream: &mut impl Write, body: &[u8]) -> std::io::Result<()> {
    let len = u32::try_from(body.len()).map_err(|_| std::io::Error::other("message too long"))?;
    stream.write_all(&len.to_ne_bytes())?;
    stream.write_all(body)?;
    stream.flush()
}

/// The longest message accepted: greetd's are a few hundred bytes.
const MAX_MESSAGE: usize = 64 * 1024;

fn receive(stream: &mut impl Read) -> std::io::Result<Zeroizing<Vec<u8>>> {
    let mut len = [0u8; 4];
    stream.read_exact(&mut len)?;
    let len = u32::from_ne_bytes(len) as usize;
    if len > MAX_MESSAGE {
        return Err(std::io::Error::other(format!("message of {len} bytes")));
    }
    let mut body = Zeroizing::new(vec![0u8; len]);
    stream.read_exact(&mut body)?;
    Ok(body)
}

fn encode(request: &Request) -> std::io::Result<Zeroizing<Vec<u8>>> {
    serde_json::to_vec(request).map(Zeroizing::new).map_err(std::io::Error::other)
}

/// The greeter's end of the connection. Responses arrive on the receiver
/// returned with it, in request order; an `Err` means the connection is gone.
pub struct Greetd {
    requests: mpsc::Sender<Request>,
}

pub type Responses = mpsc::Receiver<Result<Response, String>>;

impl Greetd {
    /// Connects to the greetd that started us.
    pub fn connect(path: &std::path::Path, wake: impl Fn() + Send + 'static) -> std::io::Result<(Greetd, Responses)> {
        Greetd::over(UnixStream::connect(path)?, wake)
    }

    /// Talks to greetd over `stream`, waking the event loop with each response.
    pub fn over(mut stream: UnixStream, wake: impl Fn() + Send + 'static) -> std::io::Result<(Greetd, Responses)> {
        let (requests, pending) = mpsc::channel::<Request>();
        let (results, responses) = mpsc::channel();
        std::thread::Builder::new().name("tileception-greetd".into()).spawn(move || {
            while let Ok(request) = pending.recv() {
                let result = encode(&request)
                    .and_then(|body| send(&mut stream, &body))
                    .and_then(|()| receive(&mut stream))
                    .and_then(|body| serde_json::from_slice::<Response>(&body).map_err(std::io::Error::other))
                    .map_err(|error| error.to_string());
                drop(request);
                let gone = result.is_err();
                if results.send(result).is_err() {
                    break;
                }
                wake();
                if gone {
                    break;
                }
            }
        })?;
        Ok((Greetd { requests }, responses))
    }

    pub fn send(&self, request: Request) -> Result<(), String> {
        tracing::debug!("greetd: {request:?}");
        self.requests.send(request).map_err(|_| "the connection to greetd is closed".to_string())
    }
}

/// A stand-in for greetd, for trying the greeter nested or headless: every
/// listed user's password is `password`, and starting a session does nothing.
pub fn demo(password: String, wake: impl Fn() + Send + 'static) -> std::io::Result<(Greetd, Responses)> {
    let (ours, mut theirs) = UnixStream::pair()?;
    std::thread::Builder::new().name("tileception-demo-greetd".into()).spawn(move || {
        let password = Zeroizing::new(password);
        let mut user: Option<String> = None;
        while let Ok(body) = receive(&mut theirs) {
            let Ok(request) = serde_json::from_slice::<serde_json::Value>(&body) else { break };
            let kind = request.get("type").and_then(|value| value.as_str()).unwrap_or_default();
            let reply = match kind {
                "create_session" => {
                    user = request.get("username").and_then(|value| value.as_str()).map(str::to_owned);
                    serde_json::json!({"type": "auth_message", "auth_message_type": "secret", "auth_message": "Password:"})
                }
                "post_auth_message_response" if user.is_some() => {
                    let answer = request.get("response").and_then(|value| value.as_str()).unwrap_or_default();
                    if answer == password.as_str() {
                        serde_json::json!({"type": "success"})
                    } else {
                        serde_json::json!({"type": "error", "error_type": "auth_error", "description": "pam_authenticate: AUTH_ERR"})
                    }
                }
                "start_session" => serde_json::json!({"type": "success"}),
                "cancel_session" => {
                    user = None;
                    serde_json::json!({"type": "success"})
                }
                _ => serde_json::json!({"type": "error", "error_type": "error", "description": "no session"}),
            };
            let Ok(reply) = serde_json::to_vec(&reply) else { break };
            if send(&mut theirs, &reply).is_err() {
                break;
            }
        }
    })?;
    Greetd::over(ours, wake)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_use_greetd_names() {
        let json = |request: &Request| String::from_utf8(encode(request).expect("encodes").to_vec()).expect("UTF-8");
        assert_eq!(
            json(&Request::CreateSession { username: "sam".into() }),
            r#"{"type":"create_session","username":"sam"}"#
        );
        assert_eq!(
            json(&Request::PostAuthMessageResponse { response: Some(Zeroizing::new("tide".into())) }),
            r#"{"type":"post_auth_message_response","response":"tide"}"#
        );
        assert_eq!(
            json(&Request::PostAuthMessageResponse { response: None }),
            r#"{"type":"post_auth_message_response","response":null}"#
        );
        assert_eq!(
            json(&Request::StartSession { cmd: vec!["sway".into()], env: vec!["XDG_SESSION_TYPE=wayland".into()] }),
            r#"{"type":"start_session","cmd":["sway"],"env":["XDG_SESSION_TYPE=wayland"]}"#
        );
        assert_eq!(json(&Request::CancelSession), r#"{"type":"cancel_session"}"#);
    }

    #[test]
    fn responses_parse() {
        let parse = |text: &str| serde_json::from_str::<Response>(text).expect("parses");
        assert_eq!(parse(r#"{"type":"success"}"#), Response::Success);
        assert_eq!(
            parse(r#"{"type":"error","error_type":"auth_error","description":"nope"}"#),
            Response::Error { error_type: ErrorType::AuthError, description: "nope".into() }
        );
        assert_eq!(
            parse(r#"{"type":"auth_message","auth_message_type":"secret","auth_message":"Password:"}"#),
            Response::AuthMessage { auth_message_type: Prompt::Secret, auth_message: "Password:".into() }
        );
    }

    #[test]
    fn answers_never_show_in_logs() {
        let request = Request::PostAuthMessageResponse { response: Some(Zeroizing::new("hunter2".into())) };
        assert!(!format!("{request:?}").contains("hunter2"));
    }

    #[test]
    fn framing_round_trips() {
        let mut wire = Vec::new();
        send(&mut wire, b"{\"type\":\"success\"}").expect("writes");
        assert_eq!(&wire[..4], &18u32.to_ne_bytes());
        let body = receive(&mut wire.as_slice()).expect("reads");
        assert_eq!(&body[..], b"{\"type\":\"success\"}");
    }

    #[test]
    fn oversized_messages_are_refused() {
        let wire = (MAX_MESSAGE as u32 + 1).to_ne_bytes();
        assert!(receive(&mut wire.as_slice()).is_err());
    }

    #[test]
    fn demo_checks_the_password() {
        let (greetd, responses) = demo("tide".into(), || {}).expect("starts");
        let ask = |request| {
            greetd.send(request).expect("sends");
            responses.recv().expect("answers").expect("succeeds")
        };
        assert!(matches!(
            ask(Request::CreateSession { username: "sam".into() }),
            Response::AuthMessage { auth_message_type: Prompt::Secret, .. }
        ));
        let wrong = ask(Request::PostAuthMessageResponse { response: Some(Zeroizing::new("sunrise".into())) });
        assert!(matches!(wrong, Response::Error { error_type: ErrorType::AuthError, .. }));
        assert_eq!(ask(Request::CancelSession), Response::Success);
        ask(Request::CreateSession { username: "sam".into() });
        let right = ask(Request::PostAuthMessageResponse { response: Some(Zeroizing::new("tide".into())) });
        assert_eq!(right, Response::Success);
        assert_eq!(ask(Request::StartSession { cmd: vec!["true".into()], env: vec![] }), Response::Success);
    }
}
