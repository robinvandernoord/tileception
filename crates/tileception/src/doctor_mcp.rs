//! Plugins' tools for Doctor's agent. Doctor gives its agent one MCP server
//! per plugin that offers tools, each `tileception doctor-mcp <plugin>`:
//! this program, speaking MCP over stdio to the agent. It holds no tools of
//! its own. It asks the compositor which tools the plugin offers, and hands
//! every call to the compositor, which applies Doctor's permission mode
//! (asking in the panel when the mode says so) and calls the plugin that
//! is already running. The agent never starts plugins itself.

use std::{
    io::{BufRead, Write},
    path::Path,
    sync::{Arc, Mutex},
};

use serde_json::{Value, json};

/// Ties the server's calls to the agent session Doctor started it for.
pub const TOKEN: &str = "TILECEPTION_DOCTOR_TOKEN";
/// MCP versions this server speaks, newest first.
const PROTOCOLS: [&str; 3] = ["2025-06-18", "2025-03-26", "2024-11-05"];
/// The command socket reads lines of up to 4096 bytes.
const LINE_LIMIT: usize = 4000;
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;
const INTERNAL_ERROR: i64 = -32603;

/// The servers for an agent session: one per plugin, named after it.
pub fn servers(exe: &Path, plugins: &[&str], env: &tileception_acp::Env) -> Vec<tileception_acp::McpServer> {
    plugins
        .iter()
        .map(|plugin| tileception_acp::McpServer {
            name: (*plugin).to_string(),
            command: exe.to_path_buf(),
            args: vec!["doctor-mcp".into(), (*plugin).to_string()],
            env: env.clone(),
        })
        .collect()
}

/// A random token for one agent session.
pub fn new_token() -> Option<String> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).ok()?;
    Some(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

/// What `tools/call` answers for the compositor's reply: the plugin's
/// `text` (or its JSON) for the agent to read, and its structured result.
pub fn call_result(reply: Result<String, String>) -> Value {
    match reply.and_then(|text| serde_json::from_str::<Value>(&text).map_err(|error| error.to_string())) {
        Ok(value) => {
            let text = value["text"].as_str().map_or_else(|| format!("{value:#}"), str::to_string);
            let mut result = json!({ "content": [{ "type": "text", "text": text }] });
            if value.is_object() {
                result["structuredContent"] = value;
            }
            result
        }
        Err(error) => json!({ "content": [{ "type": "text", "text": error }], "isError": true }),
    }
}

/// The `initialize` answer, in the client's MCP version when it is one of ours.
fn initialized(params: &Value, plugin: &str, instructions: Option<String>) -> Value {
    let asked = params["protocolVersion"].as_str().unwrap_or_default();
    let version = PROTOCOLS.iter().find(|version| **version == asked).unwrap_or(&PROTOCOLS[0]);
    let mut result = json!({
        "protocolVersion": version,
        "capabilities": { "tools": {} },
        "serverInfo": { "name": plugin, "version": env!("CARGO_PKG_VERSION") },
    });
    if let Some(instructions) = instructions {
        result["instructions"] = instructions.into();
    }
    result
}

/// The command a call becomes, or why it cannot be sent.
fn call_command(token: &str, plugin: &str, params: &Value) -> Result<Vec<String>, String> {
    let name = params["name"].as_str().filter(|name| !name.is_empty() && !name.contains(char::is_whitespace));
    let name = name.ok_or("The call names no tool.")?;
    let arguments = match params.get("arguments") {
        None | Some(Value::Null) => json!({}),
        Some(arguments) if arguments.is_object() => arguments.clone(),
        Some(_) => return Err("The arguments must be a JSON object.".into()),
    };
    let words: Vec<String> = ["doctor", "call", token, plugin, name].into_iter().map(String::from).collect();
    let words = [words, vec![arguments.to_string()]].concat();
    if words.iter().map(|word| word.len() + 1).sum::<usize>() > LINE_LIMIT {
        return Err("The arguments are too long to pass to Tileception.".into());
    }
    Ok(words)
}

/// One plugin's server, asking the compositor.
#[derive(Clone)]
struct Bridge {
    plugin: String,
    token: String,
}

impl Bridge {
    /// A command's reply, or the compositor's error.
    fn ask(&self, words: &[String]) -> Result<String, String> {
        let reply = crate::ipc::request(words).map_err(|error| format!("Tileception: {error}"))?;
        match reply.strip_prefix("error: ") {
            Some(error) => Err(error.trim().to_string()),
            None => Ok(reply),
        }
    }

    /// The plugin's tools for Doctor and its instructions.
    fn offer(&self) -> Result<Value, String> {
        let words: Vec<String> = ["doctor", "tools", &self.token, &self.plugin].into_iter().map(String::from).collect();
        let reply = self.ask(&words)?;
        serde_json::from_str(&reply).map_err(|error| error.to_string())
    }

    fn answer(&self, request_id: Value, method: &str, params: &Value) -> Value {
        let result = match method {
            "initialize" => {
                let instructions = self.offer().ok().and_then(|offer| offer["instructions"].as_str().map(String::from));
                Ok(initialized(params, &self.plugin, instructions))
            }
            "ping" => Ok(json!({})),
            "tools/list" => {
                self.offer().map(|offer| json!({ "tools": offer["tools"] })).map_err(|error| (INTERNAL_ERROR, error))
            }
            "tools/call" => match call_command(&self.token, &self.plugin, params) {
                Ok(words) => Ok(call_result(self.ask(&words))),
                Err(error) => Err((INVALID_PARAMS, error)),
            },
            _ => Err((METHOD_NOT_FOUND, format!("Method not found: {method}"))),
        };
        match result {
            Ok(result) => json!({ "jsonrpc": "2.0", "id": request_id, "result": result }),
            Err((code, message)) => {
                json!({ "jsonrpc": "2.0", "id": request_id, "error": { "code": code, "message": message } })
            }
        }
    }
}

fn write(output: &Mutex<std::io::Stdout>, message: &Value) -> Result<(), String> {
    let mut output = output.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    writeln!(output, "{message}").and_then(|()| output.flush()).map_err(|error| error.to_string())
}

/// `tileception doctor-mcp <plugin>`: serves until the agent closes stdin.
pub fn run(args: &[String]) -> Result<(), String> {
    let [plugin] = args else {
        return Err("usage: tileception doctor-mcp <plugin id>; Doctor starts this for its agent".into());
    };
    let token = std::env::var(TOKEN).map_err(|_| format!("{TOKEN} is not set; Doctor starts this for its agent"))?;
    let bridge = Bridge { plugin: plugin.clone(), token };
    let output = Arc::new(Mutex::new(std::io::stdout()));
    for line in std::io::stdin().lock().lines() {
        let line = line.map_err(|error| error.to_string())?;
        if line.trim().is_empty() {
            continue;
        }
        let message: Value = match serde_json::from_str(&line) {
            Ok(message) => message,
            Err(error) => {
                let reply =
                    json!({ "jsonrpc": "2.0", "id": null, "error": { "code": -32700, "message": error.to_string() } });
                write(&output, &reply)?;
                continue;
            }
        };
        // Notifications (initialized, cancelled) need no answer. A call the
        // user has not answered yet stays asked in the panel.
        let Some(request_id) = message.get("id").cloned() else { continue };
        let method = message["method"].as_str().unwrap_or_default().to_string();
        let params = message.get("params").cloned().unwrap_or(Value::Null);
        if method == "tools/call" {
            // It may wait for the user: answer the rest meanwhile.
            let (bridge, output) = (bridge.clone(), output.clone());
            std::thread::spawn(move || {
                let reply = bridge.answer(request_id, &method, &params);
                if let Err(error) = write(&output, &reply) {
                    eprintln!("tileception doctor-mcp: {error}");
                }
            });
        } else {
            write(&output, &bridge.answer(request_id, &method, &params))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_server_per_plugin_with_the_session_token() {
        let env: tileception_acp::Env = [
            ("TILECEPTION_SOCKET".to_string(), "/run/user/1000/tileception-wayland-1.sock".to_string()),
            (TOKEN.to_string(), "0f3a".to_string()),
        ]
        .into();
        let servers = servers(Path::new("/usr/bin/tileception"), &["home", "weather"], &env);
        assert_eq!(servers.len(), 2);
        assert_eq!(
            servers[0].to_json(),
            json!({
                "name": "home",
                "command": "/usr/bin/tileception",
                "args": ["doctor-mcp", "home"],
                "env": [
                    { "name": TOKEN, "value": "0f3a" },
                    { "name": "TILECEPTION_SOCKET", "value": "/run/user/1000/tileception-wayland-1.sock" },
                ],
            })
        );
        assert_eq!(servers[1].args, ["doctor-mcp", "weather"]);
        let (first, second) = (new_token().unwrap_or_default(), new_token().unwrap_or_default());
        assert_eq!(first.len(), 32);
        assert_ne!(first, second);
    }

    #[test]
    fn calls_become_one_command_line() {
        let params = json!({ "name": "toggle", "arguments": { "device": "Floor lamp" } });
        let words = call_command("0f3a", "home", &params).unwrap_or_default();
        let line = words.join(" ");
        assert_eq!(line, r#"doctor call 0f3a home toggle {"device":"Floor lamp"}"#);
        // What the compositor reads back is what was asked.
        assert_eq!(
            crate::commands::parse(&line),
            Ok(crate::commands::Command::DoctorCall {
                token: "0f3a".into(),
                plugin: "home".into(),
                tool: "toggle".into(),
                arguments: json!({ "device": "Floor lamp" }),
            })
        );
        let no_arguments = call_command("0f3a", "home", &json!({ "name": "all_off" })).unwrap_or_default();
        assert_eq!(no_arguments.last().map(String::as_str), Some("{}"));
        assert!(call_command("0f3a", "home", &json!({ "name": "rm -rf" })).is_err());
        assert!(call_command("0f3a", "home", &json!({ "name": "toggle", "arguments": [1] })).is_err());
        let long = json!({ "name": "toggle", "arguments": { "device": "x".repeat(LINE_LIMIT) } });
        assert!(call_command("0f3a", "home", &long).is_err());
    }

    #[test]
    fn answers_carry_text_and_structure() {
        assert_eq!(
            call_result(Ok("{\"text\":\"Lamp: on\"}\n".into())),
            json!({ "content": [{ "type": "text", "text": "Lamp: on" }], "structuredContent": { "text": "Lamp: on" } })
        );
        let plain = call_result(Ok("{\"ok\":true}\n".into()));
        assert_eq!(plain["content"][0]["text"], "{\n  \"ok\": true\n}");
        assert_eq!(plain["structuredContent"], json!({ "ok": true }));
        assert_eq!(
            call_result(Err("The user declined.".into())),
            json!({ "content": [{ "type": "text", "text": "The user declined." }], "isError": true })
        );
    }

    #[test]
    fn speaks_the_clients_version() {
        let answer = initialized(&json!({ "protocolVersion": "2025-03-26" }), "home", Some("Lights".into()));
        assert_eq!(answer["protocolVersion"], "2025-03-26");
        assert_eq!(answer["serverInfo"]["name"], "home");
        assert_eq!(answer["instructions"], "Lights");
        assert_eq!(
            initialized(&json!({ "protocolVersion": "1999-01-01" }), "home", None)["protocolVersion"],
            PROTOCOLS[0]
        );
    }

    #[test]
    fn unknown_methods_are_refused() {
        let bridge = Bridge { plugin: "home".into(), token: "0f3a".into() };
        let answer = bridge.answer(json!(7), "resources/list", &Value::Null);
        assert_eq!(answer["id"], 7);
        assert_eq!(answer["error"]["code"], METHOD_NOT_FOUND);
        assert_eq!(bridge.answer(json!(8), "ping", &Value::Null)["result"], json!({}));
        let refused = bridge.answer(json!(9), "tools/call", &json!({ "name": "toggle", "arguments": "Lamp" }));
        assert_eq!(refused["error"]["code"], INVALID_PARAMS);
    }
}
