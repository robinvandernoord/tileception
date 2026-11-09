//! Bundled plugins: plugins that ship inside the `tileception` binary as
//! `tileception plugin <id>`, with manifests compiled in. They need no extra
//! packaging, work in nested and headless instances, and config changes or
//! disables them like any other plugin (`[plugins.<id>]`).

pub(crate) mod bluetooth;
mod folders;
pub(crate) mod home;

use std::{
    collections::HashSet,
    io::{BufRead, Write},
    sync::{Arc, Mutex, mpsc},
    time::Duration,
};

use serde_json::{Value, json};

use crate::plugins::PluginConfig;

/// The bundled plugins' ids.
pub(crate) const IDS: &[&str] = &["home", "folders", "bluetooth"];

/// What the bundled manifests depend on in the config.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Options {
    /// The smart home, when `[home]` is enabled: its device file, when not
    /// the default one.
    pub home: Option<Option<String>>,
    /// `[bluetooth] reconnect = false`.
    pub bluetooth_no_reconnect: bool,
}

/// The compiled-in manifests: this very executable, started as
/// `tileception plugin <id>`, highest priority first.
pub fn manifests(options: &Options) -> Result<Vec<(String, PluginConfig)>, String> {
    let exe = std::env::current_exe().map_err(|error| format!("Bundled plugins: {error}"))?;
    // After an upgrade replaced the binary, /proc still names the old one.
    let exe = exe.to_string_lossy();
    let exe = exe.strip_suffix(" (deleted)").unwrap_or(&exe).to_string();
    let manifest = |plugin_id: &str, extra: Vec<String>| {
        let mut args = vec!["plugin".to_string(), plugin_id.to_string()];
        args.extend(extra);
        (
            plugin_id.to_string(),
            PluginConfig { command: Some(exe.clone()), args: Some(args), ..PluginConfig::default() },
        )
    };
    let mut manifests = Vec::new();
    if let Some(devices) = &options.home {
        let extra = devices.iter().flat_map(|path| ["--devices".to_string(), path.clone()]).collect();
        let (plugin_id, mut config) = manifest("home", extra);
        config.bar = Some(crate::plugins::BarOptions::default());
        manifests.push((plugin_id, config));
    }
    manifests.push(manifest("folders", Vec::new()));
    let no_reconnect = options.bluetooth_no_reconnect.then(|| "--no-reconnect".to_string());
    let (plugin_id, mut config) = manifest("bluetooth", no_reconnect.into_iter().collect());
    config.bar = Some(crate::plugins::BarOptions::default());
    manifests.push((plugin_id, config));
    Ok(manifests)
}

/// `tileception plugin <id>`: serves that plugin over stdio until stdin closes.
pub fn run(args: &[String]) -> Result<(), String> {
    match args.first().map(String::as_str) {
        Some("folders") => serve(&mut folders::Folders::default()),
        Some("home") => serve(&mut home::HomePlugin::new(&args[1..])?),
        Some("bluetooth") => serve(&mut bluetooth::BluetoothPlugin::new(&args[1..])),
        Some("-h" | "--help" | "help") | None => {
            println!(
                "Usage: tileception plugin <id>\n\nServes a bundled plugin over MCP on stdin and stdout; Tileception starts these itself.\nBundled plugins: {}",
                IDS.join(", ")
            );
            Ok(())
        }
        Some(other) => Err(format!("no bundled plugin {other:?}; bundled plugins: {}", IDS.join(", "))),
    }
}

/// What a bundled plugin implements; [`serve`] does the MCP around it.
trait Plugin {
    /// The `tools/list` entries.
    fn tools(&self) -> Value;
    /// Runs a tool: its structured result, or an error to show.
    fn call(&mut self, name: &str, arguments: &Value) -> Result<Value, String>;
    /// The resources it offers, as (uri, name).
    fn resources(&self) -> Vec<(&'static str, &'static str)> {
        Vec::new()
    }
    /// A resource's content.
    fn read(&mut self, uri: &str) -> Result<Value, String> {
        Err(format!("no resource {uri}"))
    }
    /// Called once before serving, with the means to say a resource changed.
    fn start(&mut self, _notifier: Notifier) {}
}

/// Tells Tileception that resources changed, when it subscribed to them.
/// Changes close together are sent once.
#[derive(Clone)]
pub struct Notifier {
    sender: mpsc::Sender<String>,
}

impl Notifier {
    pub fn changed(&self, uri: &str) {
        let _ = self.sender.send(uri.to_string());
    }
}

/// What the request loop and the notifier share.
struct Shared {
    output: Mutex<Box<dyn Write + Send>>,
    subscribed: Mutex<HashSet<String>>,
}

impl Shared {
    fn write(&self, message: &Value) -> Result<(), String> {
        let mut output = self.output.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        writeln!(output, "{message}").and_then(|()| output.flush()).map_err(|error| error.to_string())
    }
}

/// How long the notifier gathers changes before saying so.
const SETTLE: Duration = Duration::from_millis(40);

/// A minimal MCP server over stdio: newline-delimited JSON-RPC, tools and
/// subscribable resources.
fn serve(plugin: &mut dyn Plugin) -> Result<(), String> {
    let shared =
        Arc::new(Shared { output: Mutex::new(Box::new(std::io::stdout())), subscribed: Mutex::new(HashSet::new()) });
    let (sender, receiver) = mpsc::channel::<String>();
    let notifying = shared.clone();
    std::thread::Builder::new()
        .name("notifier".into())
        .spawn(move || {
            while let Ok(first) = receiver.recv() {
                std::thread::sleep(SETTLE);
                let mut changed: HashSet<String> = receiver.try_iter().collect();
                changed.insert(first);
                let subscribed = notifying.subscribed.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
                for uri in changed.intersection(&subscribed) {
                    let message = json!({ "jsonrpc": "2.0", "method": "notifications/resources/updated", "params": { "uri": uri } });
                    if notifying.write(&message).is_err() {
                        return;
                    }
                }
            }
        })
        .map_err(|error| error.to_string())?;
    plugin.start(Notifier { sender });
    let input = std::io::stdin();
    for line in input.lock().lines() {
        let line = line.map_err(|error| error.to_string())?;
        if line.trim().is_empty() {
            continue;
        }
        let Some(response) = respond(plugin, &line, &shared.subscribed) else { continue };
        shared.write(&response)?;
    }
    Ok(())
}

/// The answer to one message; none for notifications.
fn respond(plugin: &mut dyn Plugin, line: &str, subscribed: &Mutex<HashSet<String>>) -> Option<Value> {
    let request: Value = match serde_json::from_str(line) {
        Ok(request) => request,
        Err(error) => {
            return Some(json!({"jsonrpc":"2.0","id":null,"error":{"code":-32700,"message":error.to_string()}}));
        }
    };
    let request_id = request.get("id")?.clone();
    let error = |code: i64, message: String| {
        Some(json!({ "jsonrpc": "2.0", "id": request_id, "error": { "code": code, "message": message } }))
    };
    let resources = plugin.resources();
    let uri = request["params"]["uri"].as_str().unwrap_or_default().to_string();
    let offered = resources.iter().any(|(offered, _)| *offered == uri);
    let result = match request["method"].as_str().unwrap_or_default() {
        "initialize" => {
            let mut capabilities = json!({ "tools": {} });
            if !resources.is_empty() {
                capabilities["resources"] = json!({ "subscribe": true });
            }
            json!({
                "protocolVersion": "2025-06-18",
                "capabilities": capabilities,
                "serverInfo": { "name": "tileception", "version": env!("CARGO_PKG_VERSION") }
            })
        }
        "ping" => json!({}),
        "tools/list" => json!({ "tools": plugin.tools() }),
        "tools/call" => {
            let name = request["params"]["name"].as_str().unwrap_or_default();
            let arguments = request["params"].get("arguments").cloned().unwrap_or_else(|| json!({}));
            match plugin.call(name, &arguments) {
                Ok(value) => {
                    json!({ "structuredContent": value, "content": [{ "type": "text", "text": value.to_string() }] })
                }
                Err(message) => json!({ "isError": true, "content": [{ "type": "text", "text": message }] }),
            }
        }
        "resources/list" if !resources.is_empty() => json!({
            "resources": resources
                .iter()
                .map(|(uri, name)| json!({ "uri": uri, "name": name, "mimeType": "application/json" }))
                .collect::<Vec<_>>()
        }),
        "resources/read" if offered => match plugin.read(&uri) {
            Ok(value) => {
                json!({ "contents": [{ "uri": uri, "mimeType": "application/json", "text": value.to_string() }] })
            }
            Err(message) => return error(-32603, message),
        },
        "resources/subscribe" | "resources/unsubscribe" if offered => {
            let mut subscribed = subscribed.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            if request["method"] == "resources/subscribe" {
                subscribed.insert(uri);
            } else {
                subscribed.remove(&uri);
            }
            json!({})
        }
        "resources/read" | "resources/subscribe" | "resources/unsubscribe" => {
            return error(-32002, format!("Resource not found: {uri}"));
        }
        _ => return error(-32601, "Method not found".into()),
    };
    Some(json!({ "jsonrpc": "2.0", "id": request_id, "result": result }))
}

/// A tool's `tools/list` entry taking string arguments.
fn tool(name: &str, description: &str, arguments: &[&str]) -> Value {
    let properties: serde_json::Map<String, Value> =
        arguments.iter().map(|argument| ((*argument).to_string(), json!({ "type": "string" }))).collect();
    json!({
        "name": name,
        "description": description,
        "inputSchema": { "type": "object", "properties": properties, "required": arguments }
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test assertions")]
mod tests {
    use super::*;

    struct Echo;

    impl Plugin for Echo {
        fn tools(&self) -> Value {
            json!([tool("echo", "Echo", &["text"])])
        }

        fn call(&mut self, name: &str, arguments: &Value) -> Result<Value, String> {
            match name {
                "echo" => Ok(json!({ "text": arguments["text"] })),
                _ => Err("Unknown tool".into()),
            }
        }

        fn resources(&self) -> Vec<(&'static str, &'static str)> {
            vec![("tileception://panel", "Panel")]
        }

        fn read(&mut self, _uri: &str) -> Result<Value, String> {
            Ok(json!({ "title": "Echo" }))
        }
    }

    fn ask(subscribed: &Mutex<HashSet<String>>, line: &str) -> Option<Value> {
        respond(&mut Echo, line, subscribed)
    }

    #[test]
    fn speaks_mcp_tools_and_resources() {
        let subscribed = Mutex::new(HashSet::new());
        let initialized = ask(&subscribed, r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#).unwrap();
        assert_eq!(initialized["result"]["protocolVersion"], "2025-06-18");
        assert!(initialized["result"]["capabilities"]["tools"].is_object());
        assert_eq!(initialized["result"]["capabilities"]["resources"]["subscribe"], true);
        assert!(ask(&subscribed, r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#).is_none());
        let called = ask(
            &subscribed,
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"echo","arguments":{"text":"hi"}}}"#,
        )
        .unwrap();
        assert_eq!(called["id"], 2);
        assert_eq!(called["result"]["structuredContent"]["text"], "hi");
        let failed =
            ask(&subscribed, r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"nope"}}"#).unwrap();
        assert_eq!(failed["result"]["isError"], true);
        let listed = ask(&subscribed, r#"{"jsonrpc":"2.0","id":4,"method":"resources/list"}"#).unwrap();
        assert_eq!(listed["result"]["resources"][0]["uri"], "tileception://panel");
        ask(
            &subscribed,
            r#"{"jsonrpc":"2.0","id":5,"method":"resources/subscribe","params":{"uri":"tileception://panel"}}"#,
        )
        .unwrap();
        assert!(subscribed.lock().unwrap().contains("tileception://panel"));
        let read = ask(
            &subscribed,
            r#"{"jsonrpc":"2.0","id":6,"method":"resources/read","params":{"uri":"tileception://panel"}}"#,
        )
        .unwrap();
        assert_eq!(read["result"]["contents"][0]["text"], r#"{"title":"Echo"}"#);
        let missing = ask(
            &subscribed,
            r#"{"jsonrpc":"2.0","id":7,"method":"resources/read","params":{"uri":"tileception://other"}}"#,
        )
        .unwrap();
        assert_eq!(missing["error"]["code"], -32002);
        let unknown = ask(&subscribed, r#"{"jsonrpc":"2.0","id":8,"method":"prompts/list"}"#).unwrap();
        assert_eq!(unknown["error"]["code"], -32601);
    }

    #[test]
    fn manifests_start_this_binary() {
        let without_home = manifests(&Options::default()).unwrap();
        let (plugin_id, config) = &without_home[0];
        assert_eq!(plugin_id, "folders");
        assert_eq!(config.args.as_deref(), Some(&["plugin".to_string(), "folders".to_string()][..]));
        assert!(config.command.as_deref().is_some_and(|command| !command.is_empty()));
        assert_eq!(
            without_home.last().unwrap().1.args.as_deref(),
            Some(&["plugin", "bluetooth"].map(String::from)[..])
        );
        let with_home =
            manifests(&Options { home: Some(Some("/tmp/devices.json".into())), bluetooth_no_reconnect: true }).unwrap();
        assert_eq!(
            with_home.last().unwrap().1.args.as_deref(),
            Some(&["plugin", "bluetooth", "--no-reconnect"].map(String::from)[..])
        );
        assert_eq!(with_home[0].0, "home");
        assert_eq!(
            with_home[0].1.args.as_deref(),
            Some(&["plugin", "home", "--devices", "/tmp/devices.json"].map(String::from)[..])
        );
    }
}
