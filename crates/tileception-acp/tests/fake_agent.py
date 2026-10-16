"""A pretend ACP agent for tests: it wants a login, then streams an answer,
reports a tool call and asks permission before finishing."""
import json
import os
import sys

# FAKE_AGENT_NO_LOGIN=1 skips the login; FAKE_AGENT_FILE is the file "edit" changes.
logged_in = os.environ.get("FAKE_AGENT_NO_LOGIN") == "1"
FILE = os.environ.get("FAKE_AGENT_FILE", "/cfg/config.toml")
asked = None  # (prompt request id, permission request id)
model = "fast"


def options():
    values = [{"value": "fast", "name": "Fast"}, {"value": "smart", "name": "Smart", "description": "Thinks longer"}]
    return [{"id": "model", "name": "Model", "category": "model", "type": "select", "currentValue": model, "options": values}]


def send(message):
    sys.stdout.write(json.dumps(message) + "\n")
    sys.stdout.flush()


def update(session, body):
    send({"jsonrpc": "2.0", "method": "session/update", "params": {"sessionId": session, "update": body}})


for line in sys.stdin:
    message = json.loads(line)
    method, mid, params = message.get("method"), message.get("id"), message.get("params") or {}
    if method == "initialize":
        assert params["clientCapabilities"]["auth"]["terminal"] is True
        assert params["clientCapabilities"]["fs"] == {"readTextFile": True, "writeTextFile": True}
        send({"jsonrpc": "2.0", "id": mid, "result": {
            "protocolVersion": 1,
            "agentCapabilities": {},
            "authMethods": [
                {"id": "fake-login", "name": "Log in", "description": "Pretend to log in"},
                {"id": "fake-key", "name": "API key", "_meta": {"api-key": {"provider": "fake"}}},
            ],
        }})
    elif method == "session/new":
        if logged_in:
            send({"jsonrpc": "2.0", "id": mid, "result": {"sessionId": "s1", "configOptions": options()}})
            update("s1", {"sessionUpdate": "available_commands_update",
                          "availableCommands": [{"name": "review", "description": "Review the configuration"}]})
        else:
            send({"jsonrpc": "2.0", "id": mid, "error": {"code": -32000, "message": "Authentication required"}})
    elif method == "session/set_config_option":
        assert params["configId"] == "model"
        model = params["value"]
        send({"jsonrpc": "2.0", "id": mid, "result": {"configOptions": options()}})
    elif method == "authenticate":
        key = (params.get("_meta") or {}).get("api-key")
        logged_in = params.get("methodId") == "fake-login" or (params.get("methodId") == "fake-key" and key == "secret")
        send({"jsonrpc": "2.0", "id": mid, "result": {}})
    elif method == "session/prompt" and params["prompt"][-1]["text"] == "edit":
        assert params.get("sessionId") == "s1"
        asked = (mid, 0)
        send({"jsonrpc": "2.0", "id": 800, "method": "fs/read_text_file",
              "params": {"sessionId": "s1", "path": FILE, "line": 1, "limit": 5}})
    elif method is None and mid == 800:
        old = message["result"]["content"]
        send({"jsonrpc": "2.0", "id": 801, "method": "fs/write_text_file",
              "params": {"sessionId": "s1", "path": FILE, "content": old.replace("12", "6")}})
    elif method is None and mid == 801:
        ok = "error" not in message
        update("s1", {"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": "saved" if ok else message["error"]["message"]}})
        send({"jsonrpc": "2.0", "id": asked[0], "result": {"stopReason": "end_turn"}})
    elif method == "session/prompt":
        texts = [b["text"] for b in params["prompt"] if b["type"] == "text"]
        update("s1", {"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": "You said "}})
        update("s1", {"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": texts[-1]}})
        update("s1", {"sessionUpdate": "tool_call", "toolCallId": "t1", "title": "Edit config.toml", "kind": "edit", "status": "pending"})
        send({"jsonrpc": "2.0", "id": 900, "method": "session/request_permission", "params": {
            "sessionId": "s1",
            "toolCall": {"toolCallId": "t1", "title": "Edit config.toml"},
            "options": [
                {"optionId": "yes", "name": "Allow", "kind": "allow_once"},
                {"optionId": "no", "name": "Reject", "kind": "reject_once"},
            ],
        }})
        asked = (mid, len(texts))
    elif method is None and mid == 900:
        choice = message["result"]["outcome"].get("optionId", "cancelled")
        update("s1", {"sessionUpdate": "tool_call_update", "toolCallId": "t1", "status": "completed" if choice == "yes" else "failed"})
        update("s1", {"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": f" ({choice}, {asked[1]} blocks)"}})
        send({"jsonrpc": "2.0", "id": asked[0], "result": {"stopReason": "end_turn"}})
    elif method == "fs/read_text_file":
        pass
    elif mid is not None and method:
        send({"jsonrpc": "2.0", "id": mid, "error": {"code": -32601, "message": "unknown"}})
