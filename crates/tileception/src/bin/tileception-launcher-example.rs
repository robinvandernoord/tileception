//! A credential-free stdio MCP launcher plugin. Try `echo hello` in the launcher.

use std::io::{BufRead, Write};

use serde_json::{Value, json};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let input = std::io::stdin();
    let mut output = std::io::stdout().lock();
    for line in input.lock().lines() {
        let request: Value = serde_json::from_str(&line?)?;
        let Some(request_id) = request.get("id") else {
            continue;
        };
        let result = match request["method"].as_str().unwrap_or_default() {
            "initialize" => {
                json!({"protocolVersion":"2025-06-18","capabilities":{"tools":{}},"serverInfo":{"name":"launcher-example","version":"1.0.0"}})
            }
            "ping" => json!({}),
            "tools/list" => json!({"tools":[
                {"name":"launcher_info","description":"Launcher metadata","inputSchema":{"type":"object"}},
                {"name":"launcher_search","description":"Echo the query with several actions","inputSchema":{"type":"object","properties":{"query":{"type":"string"},"prefix":{"type":"string"}},"required":["query","prefix"]}},
                {"name":"remember","description":"Write a message to stderr","inputSchema":{"type":"object","properties":{"text":{"type":"string"}},"required":["text"]}}
            ]}),
            "tools/call" => match request["params"]["name"].as_str().unwrap_or_default() {
                "launcher_info" => tool(
                    json!({"name":"Echo example","description":"Copy text, open documentation, run printf or call a plugin tool","icon":"edit-copy","prefixes":["echo"]}),
                ),
                "launcher_search" => {
                    let query = request["params"]["arguments"]["query"].as_str().unwrap_or_default();
                    tool(
                        json!({"rows":[{"title":if query.is_empty() { "Type after echo" } else { query },"detail":"Copy text, uppercase it, or try the other example actions","icon":"edit-copy","actions":[
                            {"title":"Copy text","type":"copy","text":query},
                            {"title":"Copy uppercase","type":"copy","text":query.to_uppercase()},
                            {"title":"Open MCP documentation","type":"open","target":"https://modelcontextprotocol.io/"},
                            {"title":"Run printf","type":"run","command":"printf","args":["%s\\n",query]},
                            {"title":"Call remember tool","type":"tool","name":"remember","arguments":{"text":query}}
                        ]}]}),
                    )
                }
                "remember" => {
                    eprintln!("Remember: {}", request["params"]["arguments"]["text"]);
                    tool(json!({"ok":true}))
                }
                _ => json!({"isError":true,"content":[{"type":"text","text":"Unknown tool"}]}),
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

fn tool(value: Value) -> Value {
    json!({"structuredContent":value,"content":[{"type":"text","text":value.to_string()}]})
}
