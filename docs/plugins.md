# Plugins

Plugins are executable commands speaking MCP over stdio. They can add launcher
sources, cards under the launcher, a block in the notification panel, chips in
the bar, items on the [dashboard](#dashboard), and tools for `tileception msg plugin`. They run as the session user, with the session's
environment and permissions. Installing a manifest enables its executable. Only
install plugins you trust.

## Bundled plugins

Some plugins ship inside the `tileception` binary and need no installation:

- `home`: the [smart home](configuration.md#smart-home) block, `home` in the
  launcher and its bar chip. It runs while `[home] enabled = true`.
- `folders`: `z` in the launcher, for often visited folders.
- `timers`: `timer` in the launcher for timers and alarms, with a [card
  under the launcher](#launcher-cards) for each. The launcher also reads
  plain sentences such as "set a timer for 10 minutes" and hands them to
  this plugin; plugins themselves only ever get what follows their prefix. [Using
  Tileception](usage.md#launcher-and-notifications) describes it.
- `bluetooth`: the Bluetooth block and its bar chip.

Tileception starts them as `tileception plugin <id>`. Change or disable them in
`[plugins.<id>]` like any other plugin; a manifest with the same ID replaces
one.

## Try the example

The Rust example needs no credentials, services or script interpreter:

```sh
cargo plane build -p tileception --bin tileception-launcher-example
mkdir -p ~/.local/bin ~/.local/share/tileception/plugins
install -m755 target/debug/tileception-launcher-example ~/.local/bin/
cp resources/plugins/example.toml ~/.local/share/tileception/plugins/
```

Ensure `~/.local/bin` is on the compositor's PATH, or put the executable's
absolute path in the manifest. Run `tileception msg plugins reload` after
installing a manifest.
Open the launcher and type `echo hello`. Enter shows the row's actions. Choose
one with Up/Down and Enter. Escape returns to search results. Copy actions
leave the launcher open and show its usual Copied feedback. "Call remember
tool" keeps the text on a [card under the launcher](#launcher-cards), with
buttons to copy and to forget it. "Try a prompt" opens a [prompt](#prompts)
that pretends to wait for a security key, then asks for a name and a secret
and copies them as a sensitive copy.

Alternatively, enable the example directly in `config.toml`:

```toml
[plugins.example]
command = "/absolute/path/to/tileception-launcher-example"
[plugins.example.launcher]
prefixes = ["echo"]
```

Changes to this config take effect when the config reloads. After installing,
changing or removing a manifest, run `tileception msg plugins reload` to rescan
manifests and restart plugins without restarting Tileception or editing its
configuration. Discovery runs asynchronously; the command returns when it has
started. Manifests are also read on compositor startup or when plugin
configuration changes. Type `plugins` in the launcher to see loaded sources,
their prefixes and discovery errors. Errors are also written to the session log.
You can ask Doctor to rediscover plugins; it can run this command directly.

## Discovery and configuration

One TOML manifest per plugin is read from `/usr/share/tileception/plugins/`
and `$XDG_DATA_HOME/tileception/plugins/` (normally
`~/.local/share/tileception/plugins/`). The filename stem is the plugin ID.
A user manifest replaces the system manifest with the same ID.

Manifest fields and `[plugins.<id>]` config fields are identical (the older
`[launcher.plugins.<id>]` and top-level `prefixes` still work, with a warning):

```toml
command = "/path/to/executable"
args = ["--stdio"]
# enabled = false
[launcher]
# prefixes = ["weather", "w"]
# cards = false
[bar]
# enabled = false
[dashboard]
# enabled = false
# surface = true
```

`command` names one executable, not a shell command line. Arguments are separate
strings. Config fields overlay the matching manifest, so disabling an installed
plugin only needs `enabled = false`, and hiding its bar chips only
`[plugins.<id>.bar] enabled = false`. Chips show only for plugins with a `[bar]`
block, and dashboard items only for plugins with a `[dashboard]` block. A panel block shows whenever the plugin offers one, and so do its cards
under the launcher unless `[launcher]` says `cards = false`.

Omitted prefixes come from `launcher_info`. An explicit empty prefix list
disables routing. Prefixes are nonempty words, matched case-sensitively at the
start of the query and followed by a space: `echo` alone still searches apps,
`echo ` asks the plugin. Plugins never receive unrelated queries.

Config overrides win over user manifests, which win over system manifests,
which win over bundled plugins. Within the same priority, plugin IDs sort
alphabetically. This order settles conflicting prefixes and orders the panel's
blocks, the bar's chips and the cards under the launcher until you choose
another (see [Panel block](#panel-block)). Every rejected prefix is logged and
shown by `plugins`; a source can retain its other prefixes. A plugin overriding a
built-in prefix takes precedence for that prefix. Reserve `plugins` for
discovery diagnostics.

## MCP convention

Tileception implements the [MCP 2025-06-18 lifecycle](https://modelcontextprotocol.io/specification/2025-06-18/basic/lifecycle)
and [stdio transport](https://modelcontextprotocol.io/specification/2025-06-18/basic/transports).
Messages are newline-delimited UTF-8 JSON-RPC 2.0. Send diagnostics to stderr,
never stdout. The client initializes with an empty capability set, requires the
server's `tools` capability, and sends `notifications/initialized` before
calling tools. Client-side requests other than `ping` are unsupported. After
starting, Tileception calls `tools/list` and, with the `resources` capability
and `subscribe`, `resources/list`. It subscribes to every listed
`tileception://` resource and reads it again after
`notifications/resources/updated`.

Return tool results as `structuredContent` in a normal MCP tool result. For
compatibility, a JSON object encoded in a text content block is also accepted.
Set `isError: true` for tool failures. Resources are JSON text in
`contents[0].text`.

### Icons

`icon` fields take an icon name from the desktop icon theme, an absolute path to
a local image such as PNG or SVG, or `glyph:<name>` for Tileception's own
symbols, drawn in its style: `folder`, `file`, `globe`, `terminal`, `copy`,
`home`, `search`, `bulb`, `drop`, `thermometer`, `snowflake`, `flame`, `fan`,
`wifi_off`, `bluetooth`, `headphones`, `headset`, `speaker`, `mouse`,
`keyboard`, `gamepad`, `phone`, `laptop`, `monitor`, `camera`, `bell`, `lock`,
`clock`, `play`, `pause`, `stop`, `next`, `previous`, `plus`, `minus`, `check`
and `close`. Panel blocks, launcher
cards and bar chips only draw `glyph:` icons.

### Launcher

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

`launcher_info.icon` sets the source's default; each row can override it with
its own `icon`. A row falls back to the plugin icon, then a generic icon.
`detail` and `actions` may be omitted. A row without actions is informational.
`completion` is what Tab puts in the field, and `alternate` is an action for
Ctrl+Enter; the footer shows both keys when a row has them.
Up to 64 rows and 16 actions per row are retained.
Invalid rows and actions, including unknown action types, are skipped. Other
results still appear.

Every action has a `title` and one of these shapes:

```json
{"title":"Copy","type":"copy","text":"hello"}
{"title":"Open","type":"open","target":"https://example.org"}
{"title":"Run","type":"run","command":"printf","args":["%s\\n","hello"]}
{"title":"Remember","type":"tool","name":"remember","arguments":{"text":"hello"}}
{"title":"Terminal","type":"terminal","directory":"/home/me/src"}
{"title":"Set up","type":"settings","page":"Smart home"}
{"title":"Unlock","type":"prompt","prompt":{"title":"Unlock vault","steps":[]}}
```

Copy uses the compositor clipboard. With `"sensitive": true`, for passwords
and one-time codes, the copy also offers `x-kde-passwordManagerHint` with the
value `secret`, which clipboard managers that follow KDE's convention skip.
[Clipboard persistence](configuration.md#clipboard) does not hold it, and
handoff does not offer it to linked machines, which then hold nothing from
this machine's clipboard until the next copy. Pasting on this machine works as
usual. Open invokes `xdg-open` with the path or
URL. Run starts an executable with literal arguments and closes the launcher.
Terminal opens the user's terminal in a folder. Settings opens the Settings
page with that title. Tool invokes a tool on the same plugin, keeps the
launcher open and displays completion or failure. Its result cannot request
further compositor actions. Prompt asks for input in the launcher; see
[Prompts](#prompts). One action runs directly on Enter; several actions open a
keyboard and pointer accessible list in the existing launcher.

#### Prompts

A `prompt` action shows a small dialog in place of the plugin's results, for
example to unlock a vault. It works in rows, action lists and `alternate`, not
in panel blocks, bar chips or launcher cards.

```json
{"title":"Unlock","type":"prompt","prompt":{
  "title":"Unlock vault","detail":"optional subtitle","icon":"glyph:lock",
  "steps":[
    {"id":"key","kind":"wait","text":"Touch your security key",
     "tool":"unlock_key","arguments":{},"timeout":30,
     "alternate":{"title":"Use passphrase instead","step":"password"}},
    {"id":"password","kind":"fields","text":"optional explanation",
     "fields":[{"name":"passphrase","label":"Passphrase","secret":true}],
     "submit":"Unlock","tool":"unlock_password","arguments":{}}
  ]}}
```

The prompt opens on its first step; Escape returns to the results. Each step
has a unique `id`, optional `text`, and a `tool` on the same plugin with
optional `arguments`:

- `fields`: one to four inputs; Tab and Up/Down move between them. Enter on the last one,
  or the `submit` button (default "Continue"), calls the tool with `arguments`
  plus each field's name and value. `secret` fields are masked, cannot be
  copied, are never logged and are wiped once sent. A step with a single
  field and no `alternate` shows the button beside the input.
- `wait`: calls the tool on entering the step and shows `text` meanwhile.

`timeout` is 3 to 60 seconds: 30 for a wait, 3 for fields by default.
`alternate` switches to another step, by button or Ctrl+Enter. Leaving a step
mid-call sends `notifications/cancelled`; the plugin may skip the answer and
keeps running. A plugin missing `timeout` is ended like any other, so answer
with an error shortly before it.

The tool's answer decides what follows:

- `isError: true`: the step shows the error and empties its secret fields;
  Enter retries a wait.
- `{"step":"<id>","text":"..."}`: switches to that step with the message.
- `{"action":{...}}`: closes the prompt and runs a `copy`, `open`, `run`,
  `terminal` or `settings` action, as if chosen from the row.
- Anything else: closes the prompt and searches again.

At most eight steps; text and messages are cut to two lines. Invalid prompts
are skipped like other invalid actions.

#### Progressive launcher results

The host includes `_meta: {"tileception/generation": 123}` in the arguments of
each `launcher_search` call. A plugin can return inexpensive rows immediately,
then send additional rows after completing local background work:

```json
{"jsonrpc":"2.0","method":"notifications/tileception/launcher_append","params":{"generation":123,"rows":[{"title":"Later result","actions":[{"title":"Open","type":"open","target":"https://example.org"}]}]}}
```

Echo the generation unchanged. Rows append in notification order, up to 64
total, using the same validation as the original response. Existing rows and
the selected index and scroll position remain unchanged. Identical rows are
ignored. Notifications for another query, a closed launcher, an outstanding
initial response or an open action list are ignored. Return the initial result
before sending additions. A new query invalidates earlier additions even when
the query text is the same. Do not put private query text in diagnostics.

This optional Tileception extension is separate from MCP request progress.
Older hosts do not supply the generation token and ignore the notification;
plugins should still return usable initial results. Cancellation of the initial
request follows the normal MCP lifecycle. Once it has answered, a plugin should
discard pending background work when a new search arrives. Background work
does not extend the initial request's three-second deadline.

### Panel block

A `tileception://panel` resource is the plugin's block in the notification
panel. The plugin describes it; Tileception lays it out and draws it, and keeps
folding, the chosen view and the selected row across updates.

```json
{"title":"Home","icon":"glyph:home","summary":"all off","summary_open":"2 on",
 "active":2,
 "views":[{"title":"Rooms","items":[
   {"type":"group","title":"Living room","detail":"21°","items":[
     {"type":"row","id":"lamp","title":"Lamp","detail":"80%","icon":"glyph:bulb",
      "tint":"warm","enabled":true,
      "switch":{"on":true,"action":{"type":"tool","name":"toggle","arguments":{"device":"lamp"}}},
      "levels":[{"title":"Brightness","value":80,"unit":"%",
                 "action":{"type":"tool","name":"set","arguments":{"device":"lamp"}}}]}]}]}],
 "footer":{"text":"2 of 3 reachable","good":true,
           "actions":[{"title":"All off","enabled":true,"action":{"type":"tool","name":"all_off"}}]}}
```

- The header shows `title` and `summary`, or `summary_open` while unfolded. The
  block starts folded while `active` is 0 and unfolds when it changes; folding
  by hand holds until it does.
- `notice` (`{"text","action"}`) replaces the items, such as a setup hint;
  clicking the block runs its action.
- Several `views` get a switch in the header.
- Items are `group` (a heading that folds its items when clicked), `row` and
  `card`. A row has an `id`, `title`, `detail`, `icon`, `tint`, `dim` (dimmer
  title), `alert` (detail in the danger colour), `enabled`, an optional
  `switch` (`on`, `pending`, `action`) and `levels`. Clicking a row with levels
  selects it and shows them.
- A level is a bar from 0 to 100 with `title`, `value`, `unit` or `steps`
  (words spread over the range) and `look` (`plain` or `warmth`). Dragging
  sends its tool action with `value` added to the arguments, up to eight times
  a second and once more on release.
- A card is a larger tile: `value` (a big reading), `title`, `detail`, `icon`,
  `tint`, `lit`, `enabled`, `title_action`, `buttons`, a `stepper` (`value`,
  `minus`, `plus`), a `switch`, rows of `choices` (`title` and `options` with
  `title`, `selected`, `action`), a `trend` (`span` in seconds and `lines` of
  `[seconds ago, value]` points, `dashed` for a target) and a `progress` bar
  along the bottom (`value` from 0 to 100). Without a `value`, the title starts
  where the reading would.
- A button has a `title`, `icon`, `primary` (filled with the accent, for the
  card's main button), `enabled` and `action`. It shows its `glyph:` icon, or
  its title when it has none. Buttons sit right of the title, before the
  stepper and the switch.
- Tints are `accent`, `cool`, `warm`, `danger`, `muted` and `faded`.
- Actions are the launcher's. Panel tool calls never cancel each other.

Drag a block by its header to put it elsewhere in the panel: once the pointer
has moved a few pixels, the header follows it and a line shows where the block
will go. Letting go there moves it; a click without moving still folds the
block. The order is saved in `$XDG_STATE_HOME/tileception/plugin-order.toml`
(normally `~/.local/state/tileception/plugin-order.toml`) and also orders the
bar's chips. Plugins it does not name, such as newly installed ones, follow
in priority order. Delete the file to return to the priority order.

### Bar chips

A `tileception://bar` resource holds the plugin's chips, shown left of the
clock while it has any, for plugins with a `[bar]` block, in the panel's order:

```json
{"chips":[{"label":"Floor lamp","icon":"glyph:bulb","tint":"warm"}]}
```

A chip with an `action` runs it when clicked. Without one, clicking opens the
notification panel with the plugin's block unfolded. Return no chips to show
nothing.

### Launcher cards

A `tileception://launcher` resource holds cards shown under the launcher while
it is open, such as the bundled timers plugin's card per timer:

```json
{"cards":[{"id":"tea","title":"Tea","detail":"Timer running","value":"2:41",
  "icon":"glyph:clock","tint":"accent","enabled":true,
  "buttons":[
    {"title":"Pause","icon":"glyph:pause","primary":true,"enabled":true,
     "action":{"type":"tool","name":"pause","arguments":{"timer":"tea"}}},
    {"title":"Stop","enabled":true,
     "action":{"type":"tool","name":"stop","arguments":{"timer":"tea"}}}],
  "progress":{"value":35}}]}
```

The cards are the panel block's `card` items, drawn the same way. They stack
under what is playing, which keeps its place: first the built-in media card,
then the plugins' cards in plugin order, up to four per plugin. Cards that do
not fit above the bottom of the screen are left out, the last ones first, so a
long list of results can hide them all. Return no cards to show nothing.

Cards take the pointer only. A tool action runs like the panel's: the launcher
stays open and what is typed and found stays as it is. Copy also leaves the
launcher open; open, run, terminal and settings actions close it. Send
`notifications/resources/updated` when a card changes, for instance each second
for a countdown; Tileception reads the resource again and redraws.

### Dashboard

The dashboard shows beside the welcome screen when the desk is clear or empty,
and behind the windows while Super+D is held (see [Using
Tileception](usage.md#the-dashboard)). Plugins with a `[dashboard]` block put
items on it from a `tileception://dashboard` resource, up to six each:

```json
{"items":[
  {"type":"card","id":"tea","title":"Tea","value":"2:41","icon":"glyph:clock"},
  {"type":"image","id":"radar","title":"Rain radar","size":"medium",
   "src":"/home/me/.cache/radar.png","action":{"type":"open","target":"https://example.org/radar"}},
  {"type":"surface","id":"lamp","title":"Lava lamp","size":"large"}]}
```

- `card` is the panel block's card, drawn the same way.
- `image` is a PNG, JPEG, WebP or SVG: an absolute path to a regular file of at
  most 8 MiB, or a `data:image/...;base64,` URI. It is read when shown and read
  again after `notifications/resources/updated`, so any program in any language
  that writes an image can draw on the dashboard. Clicking it runs its
  `action`, if it has one.
- `surface` is drawn by the plugin itself, over Wayland (below).

`size` is `small`, `medium` or `large`; every item is as wide as its column.
Tileception puts the items beside the welcome screen, down the left of it and
then the right, in the panel's plugin order and each plugin's own order, and
leaves out what does not fit. Users can drag them anywhere beside it. Pictures and surfaces get a
frame with their `title` and the plugin's name, drawn by Tileception.

#### Dashboard surfaces

A plugin whose `[dashboard]` block sets `surface = true` is started with
`WAYLAND_SOCKET`, one end of a Wayland connection made for it alone. Wayland
client libraries (libwayland in C, pywayland, wayland-rs and others) use it in
place of `WAYLAND_DISPLAY` without further code. The connection offers
`wl_compositor`, `wl_subcompositor`, `wl_shm`, `wp_viewporter`, a `wl_seat`
with a pointer and no keyboard, and
[`tileception_dashboard_v1`](../resources/protocols/tileception-dashboard-v1.xml),
installed as `/usr/share/tileception/protocols/tileception-dashboard-v1.xml`.
Nothing else exists there: no windows, outputs, clipboard or screen capture.

Create a `wl_surface` for each `surface` item and pass it to
`tileception_dashboard_v1.get_item` with the item's `id`. The `configure`
event gives the size to draw at and the buffer scale; 0 by 0 means the item is
not on screen now. Tileception clips what is drawn to that size. Frame
callbacks arrive only while the item is on screen, so a plugin that draws on
each callback uses no time while the dashboard is hidden. The surface receives
pointer motion, buttons and scroll while the pointer is over it. Buffers are
at most 4096 pixels a side.

The protocol limits what a plugin can do to the desktop, not what its process
can do: it runs as the session user like any other plugin.

[`resources/plugins/lava-lamp`](../resources/plugins/lava-lamp) is a complete
example in Python with pywayland: a lava lamp that changes colour when clicked,
and a card whose Shake button calls the plugin's tool. Point the manifest's
`command` at `lava-lamp.py` and copy it to
`~/.local/share/tileception/plugins/`.

### Commands

`tileception msg plugin <id> <tool> [json]` calls a plugin's tool with a JSON
object of arguments, for scripts and key bindings, and prints the answer: its
`text` field, else the JSON. The command waits up to five seconds.

## Lifecycle and limits

Discovery, process I/O and tool calls run outside the compositor event loop,
on a worker per plugin, so a slow plugin delays only itself. Each request has
a three-second deadline, unless a prompt step sets another, and a 1 MiB
message limit. An idle worker keeps
reading, so notifications arrive at once and a crash shows without a request.

Editing or closing the launcher cancels its current generation. Queued stale
searches are skipped and stale results never replace newer rows. An in-flight
cancelled request receives `notifications/cancelled`. The process stays running,
and late responses are ignored by request id. If a request has not finished by
its deadline, the plugin's isolated process group is terminated. Broken pipes
and protocol errors also terminate that group. The next request restarts the
source after a one-second cooldown; a plugin with resources restarts after the
cooldown by itself, subscribes again and is read anew. A failed startup or
metadata call is reported in diagnostics and retried on configuration change or
the next compositor startup. Plugins that offer neither launcher prefixes nor
resources are stopped. Processes are cleaned up during configuration
replacement and normal compositor shutdown.

Tool errors are displayed without restarting the plugin. Messages written to
stderr appear in the compositor log with the plugin name. Plugins receive
DISPLAY once Xwayland is ready; those already running start again then.

The source for the complete runnable server is
[`tileception-launcher-example.rs`](../crates/tileception/src/bin/tileception-launcher-example.rs).
