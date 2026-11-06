//! Runs the ACP client against a pretend agent (tests/fake_agent.py).
#![allow(clippy::unwrap_used, reason = "test setup and assertions should fail immediately on unexpected errors")]

use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

use tileception_acp::{Event, FileOp, Request, Session, Status, install::Program};

fn mode_agent(legacy: bool) -> (Session, mpsc::Receiver<Event>) {
    let script = r#"
import json, sys
legacy = sys.argv[1] == 'true'
mode = 'read-only'
def options():
    return [
        {'id': 'model', 'name': 'Model', 'category': 'model', 'type': 'select', 'currentValue': 'fast', 'options': [{'value': 'fast', 'name': 'Fast'}]},
        {'id': 'mode', 'name': 'Mode', 'category': 'mode', 'type': 'select', 'currentValue': mode, 'options': [{'value': 'read-only', 'name': 'Read only'}, {'value': 'full-access', 'name': 'Full access'}]}
    ]
def send(value):
    print(json.dumps(value), flush=True)
for line in sys.stdin:
    message = json.loads(line)
    mid, method, params = message.get('id'), message.get('method'), message.get('params', {})
    result = {}
    if method == 'initialize':
        result = {'protocolVersion': 1, 'agentCapabilities': {}}
    elif method == 'session/new':
        result = {'sessionId': 's1', 'configOptions': options()[:1] if legacy else options()}
        if legacy:
            result['modes'] = {'currentModeId': mode, 'availableModes': [{'id': 'read-only', 'name': 'Read only'}, {'id': 'full-access', 'name': 'Full access'}]}
    elif method in ('session/set_config_option', 'session/set_mode'):
        value = params.get('value', params.get('modeId'))
        if params.get('configId') == 'model' or value == 'rejected':
            send({'jsonrpc': '2.0', 'id': mid, 'error': {'code': -32602, 'message': 'Setting rejected'}})
            continue
        if value != 'unconfirmed':
            mode = value
        result = {} if legacy else {'configOptions': options()}
    elif method == 'session/prompt':
        text = params['prompt'][-1]['text']
        if text == 'drift':
            mode = 'full-access'
            update = {'sessionUpdate': 'current_mode_update', 'currentModeId': mode} if legacy else {'sessionUpdate': 'config_option_update', 'configOptions': options()}
            send({'jsonrpc': '2.0', 'method': 'session/update', 'params': {'sessionId': 's1', 'update': update}})
        send({'jsonrpc': '2.0', 'method': 'session/update', 'params': {'sessionId': 's1', 'update': {'sessionUpdate': 'agent_message_chunk', 'content': {'type': 'text', 'text': text}}}})
        result = {'stopReason': 'end_turn'}
    else:
        continue
    send({'jsonrpc': '2.0', 'id': mid, 'result': result})
"#;
    let (sender, events) = mpsc::channel();
    let sender = Arc::new(Mutex::new(sender));
    let emit = Arc::new(move |event| {
        let _ = sender.lock().unwrap().send(event);
    });
    let program = Program {
        command: "python3".into(),
        args: vec!["-u".into(), "-c".into(), script.into(), legacy.to_string()],
        env: Default::default(),
    };
    let session = Session::start_program(
        "Mode test".into(),
        program,
        std::env::temp_dir(),
        String::new(),
        Default::default(),
        emit,
    );
    next(&events, |event| *event == Event::Status(Status::Ready));
    next(&events, |event| matches!(event, Event::Options(_)));
    (session, events)
}

#[test]
fn mode_changes_are_rechecked_and_can_be_resynced() {
    for legacy in [false, true] {
        let (session, events) = mode_agent(legacy);
        session.send(Request::SetOption { id: "mode".into(), value: "read-only".into() });
        next(&events, |event| matches!(event, Event::Options(_)));
        session.send(Request::Prompt { text: "drift".into(), images: Vec::new() });
        let Event::Options(options) = next(&events, |event| matches!(event, Event::Options(_))) else { unreachable!() };
        assert!(
            options.iter().any(|option| option.category.as_deref() == Some("mode") && option.current == "full-access")
        );
        next(&events, |event| matches!(event, Event::TurnEnded(_)));
        session.send(Request::Prompt { text: "withheld".into(), images: Vec::new() });
        let Event::Note(note) = next(&events, |event| matches!(event, Event::Note(_))) else { unreachable!() };
        assert!(note.contains("permission mode"));
        assert!(note.contains("Retry the message"));
        assert_eq!(next(&events, |event| matches!(event, Event::TurnEnded(_))), Event::TurnEnded("error".into()));
        session.send(Request::SetOption { id: "mode".into(), value: "read-only".into() });
        session.send(Request::Prompt { text: "resynced".into(), images: Vec::new() });
        assert_eq!(next(&events, |event| matches!(event, Event::Text(_))), Event::Text("resynced".into()));
        next(&events, |event| matches!(event, Event::TurnEnded(_)));
    }
}

#[test]
fn rejected_model_does_not_block_queued_or_later_prompts() {
    let (session, events) = mode_agent(false);
    session.send(Request::SetOption { id: "model".into(), value: "remembered".into() });
    session.send(Request::Prompt { text: "queued".into(), images: Vec::new() });
    assert_eq!(next(&events, |event| matches!(event, Event::Text(_))), Event::Text("queued".into()));
    next(&events, |event| matches!(event, Event::TurnEnded(_)));
    session.send(Request::Prompt { text: "later".into(), images: Vec::new() });
    assert_eq!(next(&events, |event| matches!(event, Event::Text(_))), Event::Text("later".into()));
}

#[test]
fn rejected_or_unconfirmed_mode_blocks_until_retry_succeeds() {
    for value in ["rejected", "unconfirmed"] {
        let (session, events) = mode_agent(false);
        session.send(Request::SetOption { id: "mode".into(), value: value.into() });
        session.send(Request::Prompt { text: "withheld".into(), images: Vec::new() });
        assert_eq!(next(&events, |event| matches!(event, Event::TurnEnded(_))), Event::TurnEnded("error".into()));
        session.send(Request::SetOption { id: "mode".into(), value: "read-only".into() });
        session.send(Request::Prompt { text: "retried".into(), images: Vec::new() });
        assert_eq!(next(&events, |event| matches!(event, Event::Text(_))), Event::Text("retried".into()));
    }
}

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
    let (sender, events) = mpsc::channel();
    let sender = Arc::new(Mutex::new(sender));
    let emit = Arc::new(move |event: Event| {
        let _ = sender.lock().unwrap().send(event);
    });
    let script = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fake_agent.py");
    let program = Program { command: "python3".into(), args: vec![script.into()], env: Default::default() };
    let session = Session::start_program(
        "Fake".into(),
        program,
        std::env::temp_dir(),
        "You help with Tileception.".into(),
        Default::default(),
        emit,
    );

    // It wants a login first, then works.
    let Event::Status(Status::NeedsLogin(logins)) =
        next(&events, |event| matches!(event, Event::Status(Status::NeedsLogin(_))))
    else {
        unreachable!()
    };
    assert_eq!(logins[0].method_id, "fake-login");
    assert_eq!(logins[1].how, tileception_acp::LoginHow::Key);
    // A wrong key: offered the logins again. The right one goes through.
    session.send(Request::Login { method: "fake-key".into(), secrets: vec!["wrong".into()] });
    next(&events, |event| *event == Event::Status(Status::LoggingIn { terminal: false }));
    next(&events, |event| matches!(event, Event::Status(Status::NeedsLogin(_))));
    session.send(Request::Login { method: "fake-key".into(), secrets: vec!["secret".into()] });
    next(&events, |event| *event == Event::Status(Status::Ready));

    // Its settings and commands come with the session; a setting changes.
    let Event::Options(options) = next(&events, |event| matches!(event, Event::Options(_))) else { unreachable!() };
    assert_eq!((options[0].is_model(), options[0].current_name()), (true, "Fast"));
    let Event::Commands(commands) = next(&events, |event| matches!(event, Event::Commands(_))) else { unreachable!() };
    assert_eq!(commands[0].name, "review");
    session.send(Request::SetOption { id: "model".into(), value: "smart".into() });
    let Event::Options(options) = next(&events, |event| matches!(event, Event::Options(_))) else { unreachable!() };
    assert_eq!(options[0].current_name(), "Smart");

    session.send(Request::Prompt { text: "hello".into(), images: Vec::new() });
    assert_eq!(next(&events, |event| matches!(event, Event::Text(_))), Event::Text("You said ".into()));
    assert_eq!(next(&events, |event| matches!(event, Event::Text(_))), Event::Text("hello".into()));
    let Event::Tool { title, status, .. } = next(&events, |event| matches!(event, Event::Tool { .. })) else {
        unreachable!()
    };
    assert_eq!((title.as_deref(), status.as_deref()), (Some("Edit config.toml"), Some("pending")));
    let Event::Permission { id: request_id, title, choices, action } =
        next(&events, |event| matches!(event, Event::Permission { .. }))
    else {
        unreachable!()
    };
    assert_eq!(title, "Edit config.toml");
    assert_eq!(action.kind, None);
    assert_eq!(choices.iter().map(|choice| choice.kind.as_str()).collect::<Vec<_>>(), ["allow_once", "reject_once"]);
    session.send(Request::Permission { id: request_id, choice: Some("yes".into()) });
    let Event::Tool { status, .. } = next(&events, |event| matches!(event, Event::Tool { .. })) else { unreachable!() };
    assert_eq!(status.as_deref(), Some("completed"));
    // The context went along with the first prompt: two text blocks.
    assert_eq!(next(&events, |event| matches!(event, Event::Text(_))), Event::Text(" (yes, 2 blocks)".into()));
    assert_eq!(next(&events, |event| matches!(event, Event::TurnEnded(_))), Event::TurnEnded("end_turn".into()));

    // Files go through the client: a read, then a write of what was read.
    session.send(Request::Prompt { text: "edit".into(), images: Vec::new() });
    let Event::File { id: request_id, op: operation } = next(&events, |event| matches!(event, Event::File { .. }))
    else {
        unreachable!()
    };
    assert_eq!(operation, FileOp::Read { path: "/cfg/config.toml".into(), line: Some(1), limit: Some(5) });
    session.send(Request::Reply { id: request_id, result: Ok(serde_json::json!({ "content": "gap = 12\n" })) });
    let Event::File { id: request_id, op: operation } = next(&events, |event| matches!(event, Event::File { .. }))
    else {
        unreachable!()
    };
    assert_eq!(operation, FileOp::Write { path: "/cfg/config.toml".into(), content: "gap = 6\n".into() });
    session.send(Request::Reply { id: request_id, result: Err("The user rejected this change".into()) });
    assert_eq!(
        next(&events, |event| matches!(event, Event::Text(_))),
        Event::Text("The user rejected this change".into())
    );
    assert_eq!(next(&events, |event| matches!(event, Event::TurnEnded(_))), Event::TurnEnded("end_turn".into()));
}
