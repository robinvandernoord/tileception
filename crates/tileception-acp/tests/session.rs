//! Runs the ACP client against a pretend agent (tests/fake_agent.py).
#![allow(clippy::unwrap_used, reason = "test setup and assertions should fail immediately on unexpected errors")]

use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

use tileception_acp::{Event, FileOp, Request, Session, Status, install::Program};

fn next(events: &mpsc::Receiver<Event>, want: impl Fn(&Event) -> bool) -> Event {
    loop {
        let event = events.recv_timeout(Duration::from_secs(10)).expect("an event from the agent");
        if want(&event) {
            return event;
        }
    }
}

#[test]
fn logs_in_streams_and_asks_permission() {
    let (tx, events) = mpsc::channel();
    let tx = Arc::new(Mutex::new(tx));
    let emit = Arc::new(move |e: Event| {
        let _ = tx.lock().unwrap().send(e);
    });
    let script = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fake_agent.py");
    let program = Program { command: "python3".into(), args: vec![script.into()], env: Default::default() };
    let session =
        Session::start_program("Fake".into(), program, std::env::temp_dir(), "You help with Tileception.".into(), emit);

    // It wants a login first, then works.
    let Event::Status(Status::NeedsLogin(logins)) =
        next(&events, |e| matches!(e, Event::Status(Status::NeedsLogin(_))))
    else {
        unreachable!()
    };
    assert_eq!(logins[0].id, "fake-login");
    assert_eq!(logins[1].how, tileception_acp::LoginHow::Key);
    // A wrong key: offered the logins again. The right one goes through.
    session.send(Request::Login { method: "fake-key".into(), secrets: vec!["wrong".into()] });
    next(&events, |e| *e == Event::Status(Status::LoggingIn { terminal: false }));
    next(&events, |e| matches!(e, Event::Status(Status::NeedsLogin(_))));
    session.send(Request::Login { method: "fake-key".into(), secrets: vec!["secret".into()] });
    next(&events, |e| *e == Event::Status(Status::Ready));

    // Its settings and commands come with the session; a setting changes.
    let Event::Options(options) = next(&events, |e| matches!(e, Event::Options(_))) else { unreachable!() };
    assert_eq!((options[0].is_model(), options[0].current_name()), (true, "Fast"));
    let Event::Commands(commands) = next(&events, |e| matches!(e, Event::Commands(_))) else { unreachable!() };
    assert_eq!(commands[0].name, "review");
    session.send(Request::SetOption { id: "model".into(), value: "smart".into() });
    let Event::Options(options) = next(&events, |e| matches!(e, Event::Options(_))) else { unreachable!() };
    assert_eq!(options[0].current_name(), "Smart");

    session.send(Request::Prompt { text: "hello".into(), images: Vec::new() });
    assert_eq!(next(&events, |e| matches!(e, Event::Text(_))), Event::Text("You said ".into()));
    assert_eq!(next(&events, |e| matches!(e, Event::Text(_))), Event::Text("hello".into()));
    let Event::Tool { title, status, .. } = next(&events, |e| matches!(e, Event::Tool { .. })) else { unreachable!() };
    assert_eq!((title.as_deref(), status.as_deref()), (Some("Edit config.toml"), Some("pending")));
    let Event::Permission { id, title, choices, action } = next(&events, |e| matches!(e, Event::Permission { .. }))
    else {
        unreachable!()
    };
    assert_eq!(title, "Edit config.toml");
    assert_eq!(action.kind, None);
    assert_eq!(choices.iter().map(|c| c.kind.as_str()).collect::<Vec<_>>(), ["allow_once", "reject_once"]);
    session.send(Request::Permission { id, choice: Some("yes".into()) });
    let Event::Tool { status, .. } = next(&events, |e| matches!(e, Event::Tool { .. })) else { unreachable!() };
    assert_eq!(status.as_deref(), Some("completed"));
    // The context went along with the first prompt: two text blocks.
    assert_eq!(next(&events, |e| matches!(e, Event::Text(_))), Event::Text(" (yes, 2 blocks)".into()));
    assert_eq!(next(&events, |e| matches!(e, Event::TurnEnded(_))), Event::TurnEnded("end_turn".into()));

    // Files go through the client: a read, then a write of what was read.
    session.send(Request::Prompt { text: "edit".into(), images: Vec::new() });
    let Event::File { id, op } = next(&events, |e| matches!(e, Event::File { .. })) else { unreachable!() };
    assert_eq!(op, FileOp::Read { path: "/cfg/config.toml".into(), line: Some(1), limit: Some(5) });
    session.send(Request::Reply { id, result: Ok(serde_json::json!({ "content": "gap = 12\n" })) });
    let Event::File { id, op } = next(&events, |e| matches!(e, Event::File { .. })) else { unreachable!() };
    assert_eq!(op, FileOp::Write { path: "/cfg/config.toml".into(), content: "gap = 6\n".into() });
    session.send(Request::Reply { id, result: Err("The user rejected this change".into()) });
    assert_eq!(next(&events, |e| matches!(e, Event::Text(_))), Event::Text("The user rejected this change".into()));
    assert_eq!(next(&events, |e| matches!(e, Event::TurnEnded(_))), Event::TurnEnded("end_turn".into()));
}
