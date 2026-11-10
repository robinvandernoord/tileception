//! A credential-free stdio MCP launcher plugin. Try `echo hello` in the launcher.

use std::io::{BufRead, Write};

use serde_json::{Value, json};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let input = std::io::stdin();
    // Not held locked: the prompt's wait answers from another thread.
    let mut output = std::io::stdout();
    for line in input.lock().lines() {
        let request: Value = serde_json::from_str(&line?)?;
        let Some(request_id) = request.get("id") else {
            continue;
        };
        let arguments = &request["params"]["arguments"];
        let result = match request["method"].as_str().unwrap_or_default() {
            "initialize" => {
                json!({"protocolVersion":"2025-06-18","capabilities":{"tools":{}},"serverInfo":{"name":"launcher-example","version":"1.0.0"}})
            }
            "ping" => json!({}),
            "tools/list" => json!({"tools":[
                {"name":"launcher_info","description":"Launcher metadata","inputSchema":{"type":"object"}},
                {"name":"launcher_search","description":"Echo the query with several actions","inputSchema":{"type":"object","properties":{"query":{"type":"string"},"prefix":{"type":"string"}},"required":["query","prefix"]}},
                {"name":"remember","description":"Write a message to stderr","inputSchema":{"type":"object","properties":{"text":{"type":"string"}},"required":["text"]}},
                {"name":"pretend_key","description":"Pretend to wait for a security key","inputSchema":{"type":"object"}},
                {"name":"echo_form","description":"Echo a prompt's fields","inputSchema":{"type":"object","properties":{"name":{"type":"string"},"secret":{"type":"string"}}}}
            ]}),
            "tools/call" => match request["params"]["name"].as_str().unwrap_or_default() {
                "launcher_info" => tool(
                    json!({"name":"Echo example","description":"Copy text, open documentation, run printf or call a plugin tool","icon":"edit-copy","prefixes":["echo"]}),
                ),
                "launcher_search" => {
                    let query = arguments["query"].as_str().unwrap_or_default();
                    tool(
                        json!({"rows":[{"title":if query.is_empty() { "Type after echo" } else { query },"detail":"Copy text, uppercase it, or try the other example actions","icon":"edit-copy","actions":[
                            {"title":"Copy text","type":"copy","text":query},
                            {"title":"Copy uppercase","type":"copy","text":query.to_uppercase()},
                            {"title":"Open MCP documentation","type":"open","target":"https://modelcontextprotocol.io/"},
                            {"title":"Run printf","type":"run","command":"printf","args":["%s\\n",query]},
                            {"title":"Call remember tool","type":"tool","name":"remember","arguments":{"text":query}},
                            {"title":"Try a prompt","type":"prompt","prompt":prompt()}
                        ]}]}),
                    )
                }
                "remember" => {
                    eprintln!("Remember: {}", arguments["text"]);
                    tool(json!({"ok":true}))
                }
                // Answers later, from another thread, so the plugin keeps
                // listening meanwhile. A cancelled wait is answered anyway;
                // Tileception ignores the late answer.
                "pretend_key" => {
                    let request_id = request_id.clone();
                    std::thread::spawn(move || {
                        std::thread::sleep(std::time::Duration::from_secs(2));
                        let result = tool(json!({"step":"form","text":"No security key found; type instead"}));
                        let _ =
                            writeln!(std::io::stdout(), "{}", json!({"jsonrpc":"2.0","id":request_id,"result":result}));
                    });
                    continue;
                }
                "echo_form" => {
                    let name = arguments["name"].as_str().unwrap_or_default();
                    let secret = arguments["secret"].as_str().unwrap_or_default();
                    match secret {
                        "" | "wrong" => error("Wrong secret; try another"),
                        // Any other answer closes the prompt and searches again.
                        "done" => tool(json!({"ok":true})),
                        _ => {
                            tool(json!({"action":{"type":"copy","text":format!("{name}: {secret}"),"sensitive":true}}))
                        }
                    }
                }
                _ => error("Unknown tool"),
            },
            _ => {
                writeln!(
                    output,
                    "{}",
                    json!({"jsonrpc":"2.0","id":request_id,"error":{"code":-32601,"message":"Method not found"}})
                )?;
                output.flush()?;
                continue;
            }
        };
        writeln!(output, "{}", json!({"jsonrpc":"2.0","id":request_id,"result":result}))?;
        output.flush()?;
    }
    Ok(())
}

/// Waits for a pretend security key, which is never found, then asks for a
/// name and a secret and copies them as a sensitive copy.
fn prompt() -> Value {
    json!({"title":"Example prompt","detail":"Nothing here is stored","icon":"glyph:lock","steps":[
        {"id":"key","kind":"wait","text":"Pretending to wait for a security key","tool":"pretend_key","timeout":10,
         "alternate":{"title":"Type instead","step":"form"}},
        {"id":"form","kind":"fields","text":"The secret wrong fails; done closes the prompt; anything else is copied.",
         "fields":[{"name":"name","label":"Name"},{"name":"secret","label":"Secret","secret":true}],
         "submit":"Copy","tool":"echo_form","alternate":{"title":"Wait for the key","step":"key"}}
    ]})
}

fn tool(value: Value) -> Value {
    json!({"structuredContent":value,"content":[{"type":"text","text":value.to_string()}]})
}

fn error(message: &str) -> Value {
    json!({"isError":true,"content":[{"type":"text","text":message}]})
}
