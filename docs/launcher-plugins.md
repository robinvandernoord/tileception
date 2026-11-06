# Launcher plugins

Launcher plugins are executable commands speaking MCP over stdio. They run
as the session user, with the session's environment and permissions. Installing
a manifest enables its executable. Only install plugins you trust.

The first release supports launcher search and actions. Existing built-in
sources remain built in. Plugins have no bar, notification, lock-screen or
Doctor integration.

## Try the example

The Rust example needs no credentials, services or script interpreter:

```sh
cargo plane build -p tileception --bin tileception-launcher-example
mkdir -p ~/.local/bin ~/.local/share/tileception/plugins
install -m755 target/debug/tileception-launcher-example ~/.local/bin/
cp resources/plugins/example.toml ~/.local/share/tileception/plugins/
```

Ensure `~/.local/bin` is on the compositor's PATH, or put the executable's
absolute path in the manifest. Restart Tileception after installing a manifest.
Open the launcher and type `echo hello`. Enter shows the row's actions. Choose
one with Up/Down and Enter. Escape returns to search results. Copy actions
leave the launcher open and show its usual Copied feedback.

Alternatively, enable the example directly in `config.toml`:

```toml
[launcher.plugins.example]
command = "/absolute/path/to/tileception-launcher-example"
prefixes = ["echo"]
```

Changes to this config take effect when the launcher next opens after config
reload. New or changed manifest files are read on compositor startup or when
the plugin configuration changes. Type `plugins` to see loaded sources,
their prefixes and discovery errors. Errors are also written to the session log.

## Discovery and configuration

One TOML manifest per plugin is read from `/usr/share/tileception/plugins/`
and `$XDG_DATA_HOME/tileception/plugins/` (normally
`~/.local/share/tileception/plugins/`). The filename stem is the plugin ID.
A user manifest replaces the system manifest with the same ID.

Manifest fields and `[launcher.plugins.<id>]` config fields are identical:

```toml
command = "/path/to/executable"
args = ["--stdio"]
# enabled = false
# prefixes = ["weather", "w"]
```

`command` names one executable, not a shell command line. Arguments are separate
strings. Config fields overlay the matching manifest, so disabling an installed
plugin only needs `enabled = false`. Omitted prefixes come from `launcher_info`.
An explicit empty prefix list disables routing. Prefixes are nonempty words,
matched case-sensitively at the start of the trimmed query, followed by whitespace
or the end of the query. Plugins never receive unrelated queries.

For conflicting prefixes, config overrides win over user manifests, which win
over system manifests. Within the same priority, plugin IDs sort alphabetically.
Every rejected prefix is logged and shown by `plugins`; a source can retain its
other prefixes. A plugin overriding a built-in prefix takes precedence for that
prefix. Reserve `plugins` for discovery diagnostics.

## MCP convention

Tileception implements the [MCP 2025-06-18 lifecycle](https://modelcontextprotocol.io/specification/2025-06-18/basic/lifecycle)
and [stdio transport](https://modelcontextprotocol.io/specification/2025-06-18/basic/transports).
Messages are newline-delimited UTF-8 JSON-RPC 2.0. Send diagnostics to stderr,
never stdout. The client initializes with an empty capability set, requires the
server's `tools` capability, and sends `notifications/initialized` before
calling tools. Client-side requests other than `ping` are unsupported.

Implement these [MCP tools](https://modelcontextprotocol.io/specification/2025-06-18/server/tools):

* `launcher_info({})` returns metadata:

  ```json
  {"name":"Echo","description":"Echo text","icon":"edit-copy","prefixes":["echo"]}
  ```

* `launcher_search({"prefix":"echo","query":"hello"})` receives the matched
  prefix separately from the rest of the query. It returns:

  ```json
  {"rows":[{"title":"hello","detail":"Copy this text","icon":"edit-copy","actions":[{"title":"Copy","type":"copy","text":"hello"}]}]}
  ```

Return these objects as `structuredContent` in a normal MCP tool result. For
compatibility, a JSON object encoded in a text content block is also accepted.
Set `isError: true` for tool failures. `icon` is optional: use an icon name from
the desktop icon theme or an absolute path to a local image such as PNG or SVG.
`launcher_info.icon` sets the source's default. Each search result can override
it with its own `icon`, choosing a different icon for each query. A row falls
back to the plugin icon, then a generic icon. This is per-result imagery,
not an animation or a push-update API.
`detail` and `actions` may be omitted. A row without actions is informational.
Up to 64 rows and 16 actions per row are retained.

Every action has a `title` and one of these shapes:

```json
{"title":"Copy","type":"copy","text":"hello"}
{"title":"Open","type":"open","target":"https://example.org"}
{"title":"Run","type":"run","command":"printf","args":["%s\\n","hello"]}
{"title":"Remember","type":"tool","name":"remember","arguments":{"text":"hello"}}
```

Copy uses the compositor clipboard. Open invokes `xdg-open` with the path or
URL. Run starts an executable with literal arguments and closes the launcher.
Tool invokes a tool on the same plugin, keeps the launcher open and displays
completion or failure. Its result cannot request further compositor actions.
One action runs directly on Enter; several actions open a keyboard and pointer
accessible list in the existing launcher. Search-field editing keeps the usual
TextInput, compose, selection, clipboard and word movement behaviour.

## Lifecycle and limits

Discovery, process I/O and tool calls run on a worker, outside the compositor
event loop. Each request has a three-second deadline and a 1 MiB message limit.
The worker serializes requests. A slow discovery can delay other plugin sources,
while the compositor and built-in searches remain responsive.

Editing or closing the launcher cancels its current generation. Queued stale
searches are skipped and stale results never replace newer rows. An in-flight
cancelled request receives `notifications/cancelled`; the host then terminates
the isolated process group so an uncooperative server cannot hold later searches.
Timeouts, broken pipes and protocol errors also terminate that group. The next
request restarts the source after a one-second cooldown. A failed startup or
metadata call is reported in diagnostics and retried on configuration change or
the next compositor startup. Processes are cleaned up during configuration
replacement and normal compositor shutdown.

The source for the complete runnable server is
[`tileception-launcher-example.rs`](../crates/tileception/src/bin/tileception-launcher-example.rs).
