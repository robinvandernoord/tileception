# Plugins

Plugins are programs that speak MCP over stdio. A plugin can add:

- a source in the launcher, reached by a prefix such as `timer`;
- cards under the launcher;
- a block in the notification panel;
- chips in the bar;
- items on the [dashboard](#dashboard), including surfaces it draws itself;
- tools that scripts call with `tileception msg plugin`.

Plugins run as the session user, with the session's environment and
permissions. Installing or enabling one lets it run, so only install plugins
you trust.

## Bundled plugins

These plugins ship inside the `tileception` binary and need no installation:

- `home`: the [smart home](configuration.md#smart-home) block, `home` in the
  launcher and its bar chip. It runs while `[home] enabled = true`.
- `folders`: `z` in the launcher, for often visited folders.
- `timers`: `timer` in the launcher, with a card under the launcher for each
  timer. The launcher also hands sentences such as "set a timer for 10
  minutes" to it. [Using Tileception](usage.md#launcher-and-notifications)
  describes it.
- `bluetooth`: the Bluetooth block and its bar chip.

They are ordinary plugins: Tileception starts each as `tileception plugin <id>`,
which speaks MCP over stdio like any other plugin. Configure or disable
them in `[plugins.<id>]` like any other plugin. A manifest with the same ID
replaces the bundled plugin.

## Installing from GitHub

Type `plugins add owner/repo` in the launcher, for example
`plugins add robinvandernoord/tileception-2fas`. A full GitHub URL works too,
and `owner/repo@v0.2.0` picks a tag, branch or full commit hash. `git` and
[uvenv](https://github.com/robinvandernoord/uvenv) must be installed. The
installers put uvenv and [uv](https://docs.astral.sh/uv/) in `~/.local/bin`;
when they are missing, `tileception setup` installs them.

A private repository is reached over ssh, as
`ssh://git@github.com/owner/repo.git`, when GitHub does not let it be read
without signing in. That needs a key GitHub accepts that ssh can use without
asking anything, such as one in your ssh agent: check with
`ssh -T git@github.com`. Your own ssh settings apply; git config is not
changed, and nothing waits for a passphrase or a host key question.

The first Enter finds the commit to install (the default branch's, unless you
named another), reads `pyproject.toml` at that commit and shows the package,
version, description, what it needs on your machine and the commit. Nothing
is installed yet. The second Enter installs exactly that commit, even if the
repository has changed since:

```sh
uvenv install --python '>=3.14' 'tileception-2fas[security-key] @ git+https://github.com/robinvandernoord/tileception-2fas@fb54a60c53c20be83553d78bedd8093f44332e0d'
```

Each plugin gets its own environment in `~/.local/uvenv/venvs`, with its
script linked into `~/.local/bin`, and uv downloads a suitable Python when the
system has none. If another program already has the script's name there,
installing stops and says so. Plugins installed before Tileception used
uvenv move to it at their next update. The plugin then sets itself up without asking anything
(`install --non-interactive`) and plugins are reloaded. A notification says
when it is done; while it runs, `plugins` in the launcher shows the current
step. If the plugin still needs something from you, such as an API key or a
login, Tileception asks for it: in the launcher when the plugin offers a setup
prompt, otherwise in a terminal that runs its `configure` step. Plugins are
reloaded again when setup ends.

A repository without a `pyproject.toml` is installed from its latest release
instead, or from the release of the tag you named: Tileception downloads the
archive for Linux on this machine's architecture, checks it against the
SHA-256 the release publishes and unpacks it into
`$XDG_DATA_HOME/tileception/plugin-releases/<id>`. This needs `git`, `curl`,
`tar` (or `unzip`) and `sha256sum`, and neither uvenv nor Python. The launcher
shows the archive, the release's tag and the commit that tag is at before
anything is downloaded. An archive whose checksum does not match, or one
with a file that would land outside its folder, is not installed. The
checksum comes from the same release as the archive, so it shows the download
arrived whole and is the file GitHub lists; as with a Python package, it does
not vouch for whoever published it. Only public repositories install this
way.

Only one install, update or removal runs at a time. A plugin ID belongs to one
repository: installing a different repository with the same plugin ID or
Python package is refused until you remove the installed one. Installing the
same repository again is allowed.

### Managing installed plugins

| Launcher                  | Does                                                                   |
| ------------------------- | ---------------------------------------------------------------------- |
| `plugins`                 | Lists rows to add, configure and update plugins, then loaded sources and discovery errors |
| `plugins list`            | Shows where each installed plugin came from and at which commit        |
| `plugins configure [id]`  | Runs a plugin's setup again                                            |
| `plugins update [id]`     | Updates one plugin, or all of them                                     |
| `plugins remove [id]`     | Removes a plugin                                                       |

`[id]` can be the start of a plugin ID; the launcher lists the installed
plugins that match. The same works from a shell or a key binding:

```sh
tileception msg plugins add robinvandernoord/tileception-odoo
tileception msg plugins list
tileception msg plugins configure odoo
tileception msg plugins update        # every installed plugin, or name one
tileception msg plugins remove odoo
```

`tileception msg plugins add` installs without showing the package first. These
commands return as soon as the work has started; a notification reports the
result.

Updating is the only step that moves a plugin to a newer commit. It looks up
where the default branch, or the tag or branch you installed, is now, and
installs that commit. A plugin from a release moves to the newest release
(or stays at the tag you installed), and its archive replaces the one before;
it is up to date while the tag, the commit it is at and the archive's
checksum are the same. A notification lists each plugin as updated, up to date
or failed; one failure does not stop the others, and a plugin whose update
failed keeps the version it had.

Removing runs the plugin's own `uninstall` first, so it can stop its timers,
then removes the package, or the folder a release was unpacked into. It deletes the manifest only if installing created
it; a manifest you wrote before installing stays. The plugin's configuration
and cache stay unless its `uninstall` removes them.

Installed plugins are recorded in
`$XDG_DATA_HOME/tileception/installed-plugins.toml`. Plugins set up by hand
keep working, but `update`, `remove` and `configure` only know the ones in
this file.

## Adding a plugin by hand

### Manifests and configuration

Tileception reads one TOML manifest per plugin from
`/usr/share/tileception/plugins/` and `$XDG_DATA_HOME/tileception/plugins/`
(normally `~/.local/share/tileception/plugins/`). The file name without
`.toml` is the plugin ID. A user manifest replaces a system manifest with the
same ID.

A manifest and a `[plugins.<id>]` table in `config.toml` take the same fields:

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

`command` is one executable, not a shell command line: put each argument in
`args`. It must be on the compositor's `PATH` or an absolute path. Plugins do
not start in any particular folder, so give scripts by absolute path.

Config fields overlay the manifest with the same ID. To disable an installed
plugin, set `[plugins.<id>] enabled = false`; to hide only its bar chips, set
`[plugins.<id>.bar] enabled = false`. The older `[launcher.plugins.<id>]` table
and a top-level `prefixes` still work, with a warning.

Each block switches on a place:

- The panel block shows whenever the plugin offers one.
- Cards under the launcher show unless `[launcher]` says `cards = false`.
- Bar chips show only with a `[bar]` block.
- Dashboard items show only with a `[dashboard]` block.
  `surface = true` lets the plugin [draw its own items](#dashboard-surfaces).

Without `prefixes`, the plugin's own from `launcher_info` apply. An empty list
switches its launcher source off. A prefix is a nonempty word other than
`plugins`, matched case-sensitively at the start of the query and followed by
whitespace: `echo` alone still searches apps, while `echo hello` asks the
plugin. Plugins never see queries without their prefix.

Config overrides win over user manifests, which win over system manifests,
which win over bundled plugins. Within one level, plugin IDs sort
alphabetically. This order decides which plugin gets a prefix that two of them
claim, and orders panel blocks, bar chips, launcher cards and dashboard items
until the user [drags the blocks](#panel-block) into another order. A plugin
prefix takes precedence over a built-in one such as `timer`. Rejected prefixes
are logged; the plugin keeps its other prefixes.

### Reloading and diagnostics

Changes to `config.toml` apply when the config reloads. After installing,
changing or removing a manifest, run:

```sh
tileception msg plugins reload
```

This rescans manifests and restarts plugins without restarting Tileception.
It returns once discovery has started. Manifests are also read at startup and
whenever plugin configuration changes. Wizard can run it for you.

Type `plugins` in the launcher to see the loaded sources with their prefixes,
and discovery errors such as a broken manifest, a rejected prefix or a plugin
that failed to start. The same errors go to the session log, along with
anything a plugin writes to stderr.

### Adding from the command line

`tileception msg plugin add <command> [args...]` writes a `[plugins.<id>]`
entry for that command to `config.toml`, keeping the rest of the file as it
is. The config reload that follows starts the plugin:

```sh
tileception msg plugin add python3 ~/plugins/weather.py
tileception msg plugin add --bar --id lamps /opt/lamps/bin/lamps-mcp --stdio
```

The ID comes from the executable's name, without its extension or a
`tileception-` prefix. For an interpreter or runner such as `python3`, `node`,
`bash`, `deno`, `npx` or `uvx`, it comes from the script or package instead:
`python3 ~/plugins/weather.py` and `deno run -A ~/plugins/weather.ts` both
become `weather`. Tileception does not start the plugin to ask for a name.

Options go before the command:

- `--id <id>` sets another ID: lowercase letters, digits, `-` and `_`.
- `--bar` adds a `[bar]` block, so the plugin's chips show.

Adding is refused when:

- the ID is already in `config.toml`, used by a manifest or a bundled plugin,
  or reserved (`add`, `plugins`);
- the same command and arguments are already set up under another ID;
- the command is not on the compositor's `PATH` and not an absolute path;
- a relative script path does not exist (the shell expands `~/`, so use that
  or an absolute path);
- an argument is empty or contains whitespace. Edit `config.toml` for such an
  argument.

Wizard can run this command for you once you approve it.

### Try the example

The Rust example plugin needs no credentials, services or interpreter:

```sh
cargo plane build -p tileception --bin tileception-launcher-example
mkdir -p ~/.local/bin ~/.local/share/tileception/plugins
install -m755 target/debug/tileception-launcher-example ~/.local/bin/
cp resources/plugins/example.toml ~/.local/share/tileception/plugins/
tileception msg plugins reload
```

If `~/.local/bin` is not on the compositor's `PATH`, put the executable's
absolute path in the manifest's `command`.

Open the launcher and type `echo hello`. Enter shows the row's actions; choose
one with Up/Down and Enter, or go back to the results with Escape. "Call
remember tool" keeps the text on a [card under the launcher](#launcher-cards),
with buttons to copy and forget it. "Try a prompt" opens a [prompt](#prompts)
that pretends to wait for a security key, then asks for a name and a secret
and copies them as a sensitive copy.

Its source,
[`tileception-launcher-example.rs`](../crates/tileception/src/bin/tileception-launcher-example.rs),
is a complete server in one file.

## Writing a plugin

### MCP basics

Tileception follows the [MCP 2025-06-18 lifecycle](https://modelcontextprotocol.io/specification/2025-06-18/basic/lifecycle)
and [stdio transport](https://modelcontextprotocol.io/specification/2025-06-18/basic/transports):
newline-delimited UTF-8 JSON-RPC 2.0 on stdin and stdout. Write diagnostics
to stderr, never stdout.

1. Tileception sends `initialize` with an empty capability set. Answer with
   `protocolVersion` `"2025-06-18"` and a `tools` capability; anything else is
   refused.
2. Tileception sends `notifications/initialized`, then calls `tools/list`.
   A plugin with a `launcher_info` tool is a launcher source.
3. If the plugin declares the `resources` capability with `subscribe: true`,
   Tileception calls `resources/list`, subscribes to every `tileception://`
   resource listed, reads each one, and reads it again after each
   `notifications/resources/updated`.

| Resource                 | Shows                                  |
| ------------------------ | -------------------------------------- |
| `tileception://panel`    | [Panel block](#panel-block)            |
| `tileception://bar`      | [Bar chips](#bar-chips)                |
| `tileception://launcher` | [Launcher cards](#launcher-cards)      |
| `tileception://dashboard`| [Dashboard items](#dashboard)          |

A plugin that offers neither a launcher source nor a resource is stopped.

Return tool results as `structuredContent`. A JSON object in a text content
block is also accepted. Set `isError: true` for failures; the text content is
shown as the error. Resources are JSON text in `contents[0].text`. A resource
that does not parse is logged and hides that place until a valid version
arrives. Of requests from the plugin to Tileception, only `ping` is answered;
others, such as sampling, get a "method not found" error.

### Icons

An `icon` is an icon name from the desktop icon theme, an absolute path to a
local image such as PNG or SVG, or `glyph:<name>` for one of Tileception's own
symbols: `folder`, `file`, `globe`, `terminal`, `copy`, `home`, `search`,
`bulb`, `drop`, `thermometer`, `snowflake`, `flame`, `fan`, `wifi_off`,
`bluetooth`, `headphones`, `headset`, `speaker`, `mouse`, `keyboard`,
`gamepad`, `phone`, `laptop`, `monitor`, `camera`, `bell`, `lock`, `clock`,
`play`, `pause`, `stop`, `next`, `previous`, `plus`, `minus`, `check` and
`close`. Panel blocks, cards and bar chips only draw `glyph:` icons.

### Launcher

Implement two [MCP tools](https://modelcontextprotocol.io/specification/2025-06-18/server/tools):

* `launcher_info({})` returns the source's `name`, `description` and
  `prefixes`, all required, and an optional default `icon`:

  ```json
  {"name":"Echo","description":"Echo text","icon":"edit-copy","prefixes":["echo"]}
  ```

* `launcher_search({"prefix":"echo","query":"hello"})` gets the matched prefix
  and the rest of the query separately, and returns rows:

  ```json
  {"rows":[{"title":"hello","detail":"Copy this text","icon":"edit-copy","actions":[{"title":"Copy","type":"copy","text":"hello"}]}]}
  ```

A row needs a `title`. Its optional fields:

- `detail`: a second line.
- `icon`: overrides the source's icon; without either, a generic icon shows.
- `actions`: what Enter does. One action runs directly; several open a list
  to choose from. A row without actions is informational.
- `completion`: what Tab puts in the search field.
- `alternate`: an action for Ctrl+Enter.

The footer shows Tab and Ctrl+Enter when a row has them. Tileception keeps up
to 64 rows and 16 actions per row, and skips invalid rows and actions,
including unknown action types, while showing the rest.

Every action has a `title` and a `type`:

```json
{"title":"Copy","type":"copy","text":"hello"}
{"title":"Open","type":"open","target":"https://example.org"}
{"title":"Run","type":"run","command":"printf","args":["%s\\n","hello"]}
{"title":"Remember","type":"tool","name":"remember","arguments":{"text":"hello"}}
{"title":"Terminal","type":"terminal","directory":"/home/me/src"}
{"title":"Set up","type":"settings","page":"Smart home"}
{"title":"Unlock","type":"prompt","prompt":{"title":"Unlock vault","steps":[]}}
```

| Type       | Does                                                                                      |
| ---------- | ----------------------------------------------------------------------------------------- |
| `copy`     | Copies `text` to the clipboard and leaves the launcher open                               |
| `open`     | Opens a path or URL with `xdg-open`                                                       |
| `run`      | Starts an executable with literal arguments and closes the launcher                       |
| `tool`     | Calls a tool on the same plugin, keeps the launcher open and shows success or the error  |
| `terminal` | Opens the user's terminal in a folder                                                     |
| `settings` | Opens the Settings page with that title                                                   |
| `prompt`   | Asks for input in the launcher; see [Prompts](#prompts)                                   |

A `tool` action's result cannot trigger further actions.

Add `"sensitive": true` to a copy of a password or one-time code. The copy
then carries `x-kde-passwordManagerHint: secret`, which clipboard managers
following KDE's convention skip. [Clipboard
persistence](configuration.md#clipboard) does not keep it, and handoff does
not offer it to linked machines, which then hold nothing from this machine's
clipboard until the next copy. Pasting on this machine works as usual.

#### Prompts

A `prompt` action replaces the plugin's results with a small dialog, for
example to unlock a vault. It works in rows, action lists and `alternate`, not
in panel blocks, bar chips or cards.

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

The prompt opens on its first step; Escape returns to the results. A prompt
has at most eight steps. Each step has a unique `id`, optional `text`, and a
`tool` on the same plugin with optional `arguments`. There are two kinds:

- `fields`: one to four inputs. Tab and Up/Down move between them. Enter on
  the last one, or the `submit` button (default "Continue"), calls the tool
  with `arguments` plus each field's `name` and value. A field's optional
  `value` fills it when the step opens, such as a default or the current
  setting. `secret` fields always start empty, are masked, cannot be copied,
  are never logged and are wiped once sent. A step with one field and no
  `alternate` shows the button beside the input.
- `wait`: calls the tool as soon as the step opens and shows `text` meanwhile.

`timeout` is 3 to 60 seconds; the default is 30 for a wait and 3 for fields.
A plugin that misses its timeout is ended like any other, so answer with an
error shortly before it. `alternate` switches to another step, by its button
or Ctrl+Enter. Leaving a step during a call sends `notifications/cancelled`;
the plugin keeps running and may skip the answer.

The tool's answer decides what happens next:

- `isError: true`: the step shows the error and empties its secret fields.
  Enter retries a wait.
- `{"step":"<id>","text":"..."}`: switches to that step and shows the text.
- `{"action":{...}}`: closes the prompt and runs a `copy`, `open`, `run`,
  `terminal` or `settings` action, as if chosen from the row.
- Anything else: closes the prompt and searches again.

Text and messages are cut to two lines. Invalid prompts are skipped like
other invalid actions.

#### Progressive results

Each `launcher_search` call carries `_meta: {"tileception/generation": 123}`
in its arguments. A plugin can answer at once with what it has, then append
rows as background work finishes:

```json
{"jsonrpc":"2.0","method":"notifications/tileception/launcher_append","params":{"generation":123,"rows":[{"title":"Later result","actions":[{"title":"Open","type":"open","target":"https://example.org"}]}]}}
```

Echo the generation unchanged, and send additions only after the initial
answer. Appended rows go below the others, in the order they arrive, up to 64
in total, and are validated like the initial rows. The selection and scroll
position stay where they are, and duplicate rows are ignored. Tileception
ignores additions for an older query, even one with the same text, and while
the launcher is closed or an action list is open.

The initial answer still has the three-second deadline; background work does
not extend it. Drop pending work when a new search arrives, and do not put
private query text in diagnostics.

### Panel block

The `tileception://panel` resource is the plugin's block in the notification
panel. The plugin describes it; Tileception lays it out, draws it, and keeps
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

`enabled` defaults to `false` on rows, cards, buttons and footer actions, and
a disabled one is drawn faded and ignores clicks. Set `"enabled": true` on
everything the user should be able to use.

- The header shows `title` and `summary`, or `summary_open` while the block is
  unfolded. The block starts folded while `active` is 0 and unfolds when
  `active` changes; folding by hand holds until it changes again.
- `notice` (`{"text","action"}`) replaces the items, for example with a setup
  hint. Clicking the block runs its action.
- With more than one entry in `views`, the header gets a switch between them.
- `footer` has a `text`, `good` (lights the dot before the text) and
  `actions`, each with a `title`, `enabled` and `action`.
- Items are `group`, `row` and `card`. A group is a heading that folds its
  `items` when clicked, with an optional `detail`.
- A row has an `id` (unique in the block, so the selection survives updates),
  `title`, `detail`, `icon`, `tint`, `dim` (dimmer title, for something
  switched off), `alert` (detail in the danger colour), `enabled`, an optional
  `switch` (`on`, `pending`, `action`), `levels`, and a `trend` like a card's,
  drawn small at the right of a row without a switch. Clicking a row with
  levels selects it and shows them.
- A level is a bar from 0 to 100 with `title`, `value`, and `unit` or `steps`
  (words spread over the range, such as Warm, Neutral and Cool), and `look`
  (`plain` or `warmth`). Dragging sends its tool action with `value` added to
  the arguments, up to eight times a second and once more on release.
- A card is a larger tile with:
  - `id`, `title`, `detail`, `icon`, `tint` and `enabled`;
  - `value`, a big reading such as a temperature. Without it, the title takes
    its place;
  - `lit`, which makes the stepper's value and chosen options stand out while
    the thing is on;
  - `title_action`, run when the title is clicked;
  - `buttons`, right of the title. Each has a `title`, `icon`, `primary`
    (filled with the accent, for the main button), `enabled` and `action`,
    and shows its `glyph:` icon, or its title without one;
  - `stepper` (`value`, `minus`, `plus` actions) and `switch`, after the
    buttons;
  - `choices`: rows with a `title` and `options`, each with `title`,
    `selected` and `action`;
  - `trend`: `span` in seconds and `lines`, each with `points` as
    `[seconds ago, value]` pairs. `dashed` draws a target; `secondary` draws a
    second kind of reading, such as humidity beside temperature, in the
    second accent colour on its own scale;
  - `progress`: a bar along the bottom, `{"value": 0 to 100}`.
- Tints are `accent`, `cool`, `warm`, `danger`, `muted` (the default) and
  `faded`.
- Actions take the launcher's action shapes, except `prompt`. Panel tool calls
  never cancel each other.

Users can drag blocks into another order (see [Using
Tileception](usage.md#arranging-the-panels-blocks)). The order is saved in
`$XDG_STATE_HOME/tileception/plugin-order.toml` (normally
`~/.local/state/tileception/plugin-order.toml`) and also orders bar chips,
launcher cards and dashboard items. Plugins missing from the file, such as new
ones, follow in priority order. Delete the file to go back to the priority
order.

### Bar chips

The `tileception://bar` resource holds the plugin's chips. They show left of
the clock, in the panel's order, for plugins with a `[bar]` block:

```json
{"chips":[{"label":"Floor lamp","icon":"glyph:bulb","tint":"warm"}]}
```

Clicking a chip runs its `action`. A chip without one opens the notification
panel with the plugin's block unfolded. Return an empty list to show nothing.

### Launcher cards

The `tileception://launcher` resource holds cards shown under the launcher
while it is open, such as the timers plugin's card for each timer:

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

These are the panel block's cards, drawn the same way, up to four per plugin.
They stack under the built-in media card, in plugin order. Cards that do not
fit above the bottom of the screen are left out, last ones first, so a long
list of results can hide them all.

Cards respond to the pointer only. Tool and copy actions leave the launcher
open with the query and results as they were; `open`, `run`, `terminal` and
`settings` close it. To animate a card, such as a countdown, send
`notifications/resources/updated` each time it changes.

### Dashboard

The dashboard shows beside the welcome screen when the desk is clear or empty,
and behind the windows while Super+D is held. [Using
Tileception](usage.md#the-dashboard) describes how users arrange, remove and
add items.

Plugins with a `[dashboard]` block put up to six items each on it from the
`tileception://dashboard` resource:

```json
{"items":[
  {"type":"card","id":"tea","title":"Tea","value":"2:41","icon":"glyph:clock"},
  {"type":"image","id":"radar","title":"Rain radar","size":"medium",
   "src":"/home/me/.cache/radar.png","action":{"type":"open","target":"https://example.org/radar"}},
  {"type":"surface","id":"lamp","title":"Lava lamp","size":"large"}]}
```

- `card` is the panel block's card, drawn the same way and as tall as its
  content.
- `image` is a PNG, JPEG, WebP or SVG: an absolute path to a regular file of
  at most 8 MiB, or a `data:image/...;base64,` URI. Files are read again after
  each `notifications/resources/updated`, so any program that can write an
  image file can draw on the dashboard. Clicking the image runs its optional
  `action`.
- `surface` is drawn by the plugin itself; see [Dashboard
  surfaces](#dashboard-surfaces).

`size` sets the height of an image or surface: `small`, `medium` or `large`.
Every item is as wide as a dashboard column. Tileception frames images and
surfaces with their `title` and the plugin's name.

Give each item a stable, unique `id`. Tileception remembers where the user
put an item, and whether they removed it, by `<plugin id>/<item id>`, which is
also the name `tileception msg dashboard` uses. New items fill the first free
places, down the left of the welcome screen and then the right, in plugin
order; items that do not fit are left out.

An installed plugin whose `[dashboard]` block is switched off, or which is
itself switched off, is offered under the **+** in the welcome screen's
footer. Choosing it switches it on in `config.toml`.

#### Dashboard surfaces

With `surface = true` in its `[dashboard]` block, a plugin is started with
`WAYLAND_SOCKET`, one end of a Wayland connection made for it alone. Wayland
client libraries (libwayland in C, pywayland, wayland-rs and others) use it in
place of `WAYLAND_DISPLAY` without extra code. The connection offers
`wl_compositor`, `wl_subcompositor`, `wl_shm`, `wp_viewporter`, a `wl_seat`
with a pointer and no keyboard, and
[`tileception_dashboard_v1`](../resources/protocols/tileception-dashboard-v1.xml),
installed as `/usr/share/tileception/protocols/tileception-dashboard-v1.xml`.
It has no windows, outputs, clipboard or screen capture.

To draw an item:

1. List it as a `surface` item in the dashboard resource.
2. Create a `wl_surface` and pass it to `tileception_dashboard_v1.get_item`
   with the item's `id`.
3. Draw at the size and buffer scale from the item's `configure` event, in
   buffers of at most 4096 pixels a side. A size of 0 by 0 means the item is
   not on screen; keep the last buffer.

Tileception clips what is drawn to the item's size. Frame callbacks arrive
only while the item is on screen, so a plugin that draws on each callback
uses no CPU while the dashboard is hidden. The surface gets pointer motion,
buttons and scroll while the pointer is over it.

The protocol limits what the plugin can do to the desktop, not what its
process can do: it still runs as the session user.

[`resources/plugins/lava-lamp`](../resources/plugins/lava-lamp) is a complete
example in Python with pywayland: a lava lamp that changes colour when
clicked, and a card whose Shake button calls the plugin's tool. Install
pywayland, copy `lava-lamp.toml` to `~/.local/share/tileception/plugins/` and
set its `command` to the absolute path of `lava-lamp.py`.

### Commands

`tileception msg plugin <id> <tool> [json]` calls a plugin's tool with a JSON
object of arguments, for scripts and key bindings. It prints the result's
`text` field, or the whole JSON without one, and waits up to five seconds. A
plugin with the ID `add` cannot be called this way.

## Making a plugin installable

`plugins add` installs Python packages, and programs from
[release archives](#release-archives). For a Python package the repository needs a
`pyproject.toml` with a `[project]` table and one console script, such as
`tileception-odoo`. With several scripts, Tileception uses the one named like
the package, or the one `[tool.tileception] script` names. The script takes
these subcommands:

| Subcommand                  | Does                                                                                  | Required              |
| --------------------------- | ------------------------------------------------------------------------------------- | --------------------- |
| `serve`                     | Speaks MCP on stdin and stdout; the manifest runs this                                | yes                   |
| `install --non-interactive` | Writes the manifest and anything else it needs, such as systemd units, and asks nothing | yes                 |
| `configure`                 | Asks what it needs in the terminal, then finishes what `install` could not            | when it has questions |
| `status`                    | Prints JSON with `"configured"` (`true` or `false`), `"detail"` and an optional `"setup"` prompt | when it has `configure` |
| `uninstall`                 | Undoes `install`: stops timers, removes units and the manifest. `--purge` also removes configuration and cache | yes |

`install --non-interactive` runs after every install and update, so it must be
safe to run again. It has no terminal and must not wait for input. When it
cannot finish without answers, it does what it can and makes `status` report
`"configured": false`; Tileception then starts setup as described below. Keep
it quick: no other install, update or removal can run until it ends, and it is
stopped after 30 minutes. Start long work, such as a first sync, in the
background without waiting for it, for example with
`systemctl --user start --no-block` on a `Type=oneshot` service.

`status` must answer within a minute. Output that is not JSON, or a missing
`"configured"`, counts as configured.

`configure` may use `input`, `getpass`, a full-screen picker or another
program's login, such as `gh auth login`. Exit with 0 once the plugin works.

When `install` writes no manifest, Tileception writes
`~/.local/share/tileception/plugins/<id>.toml` with the script's absolute path
and `args = ["serve"]`. Prefixes then come from `launcher_info`. Write your
own manifest to add `[bar]` or `[dashboard]` blocks.

An optional `[tool.tileception]` table in `pyproject.toml` adds details:

```toml
[tool.tileception]
id = "2fas"                      # plugin ID; default: the package name without tileception-
script = "tileception-2fas"      # needed when [project.scripts] has several scripts
extras = ["security-key"]        # installed with the package
needs = ["the 2fas CLI"]         # shown before installing
```

A plugin ID may contain letters, digits, `-` and `_`, and cannot be
`plugins`. `requires-python` is passed to uvenv, whose uv finds or downloads a
Python that fits.

### Release archives

A plugin written in another language, such as Rust or Go, installs from the
repository's GitHub releases. The repository must have no `pyproject.toml` at
its top level, and its release needs:

- An archive (`.tar.gz`, `.tgz`, `.tar.xz`, `.tar.bz2` or `.zip`) per
  architecture whose file name says `linux` and the architecture: `x86_64`,
  `amd64` or `x64`, and `aarch64` or `arm64`. Target triples such as
  `tileception-dice-v1.2.0-x86_64-unknown-linux-gnu.tar.gz` and names such as
  `dice_1.2.0_Linux_arm64.tar.gz` both work. With several that fit, one built
  for glibc (`gnu`) wins over one that does not say, and that over `musl`.
- A SHA-256 for that archive. GitHub records one for every file uploaded to
  a release since mid-2025, which is enough. Otherwise publish
  `<archive>.sha256`, or a list as `sha256sum` writes it in a file whose name
  contains `checksums` or `sha256sums` (`checksums.txt`, `SHA256SUMS`). A
  release without any is refused.
- In the archive, one program that takes the subcommands above. It is the
  executable file named like the repository, the one nearest the top if there
  are several, up to three folders deep; an archive with a single executable
  file may call it anything. Other files, such as libraries beside it, are
  unpacked with it.

The plugin ID is the repository's name in lowercase, without a leading
`tileception-`. There is no `[tool.tileception]` table for an archive, so
nothing is shown under "needs" before installing: say what the plugin needs
in `status` and `configure`. The program's path changes with each update when
the archive's top folder carries the version, so a manifest your `install`
writes should take the path it runs from, and `install --non-interactive`
runs again after every update to write it afresh.

### Setting up in the launcher

A plugin whose questions fit in a few fields can ask them in the launcher
instead of a terminal. Add a `"setup"` key to the `status` output, holding a
[prompt](#prompts) in the same shape a `prompt` action takes:

```json
{"configured": false, "detail": "No API key yet",
 "setup": {"title": "Set up Odoo", "icon": "glyph:globe", "steps": [
   {"id": "connect", "kind": "fields", "tool": "setup", "timeout": 30,
    "submit": "Connect",
    "fields": [
      {"name": "web_url", "label": "Web URL", "value": "https://example.com"},
      {"name": "api_key", "label": "API key", "secret": true}]}]}}
```

After an install or update with `"configured": false`, Tileception opens this
prompt in the launcher. `plugins configure` opens it whenever `status` offers
one, so it also works for changing settings later: fill each field's `value`
with the current setting. The prompt's tools run in the plugin's `serve`
process, which saves what the fields send.

The tool answers like any prompt step: an error stays on the step and
`{"step": ...}` moves to another. Any other answer finishes setup: the
launcher closes, plugins reload and a notification says the plugin is set
up. Escape cancels setup.

Start with a `fields` step rather than a `wait`, since the plugin has only
just started when the prompt opens. Ask only what fits in fields, and keep
`configure` for everything else, such as choosing from a long list.
Tileception also runs `configure` when the screen is locked, when another
program is the launcher, when another prompt is open, or when the setup
prompt is invalid.

While it is not configured, a plugin can also answer every search with a
"Set up" row whose action is the same prompt.

## Lifecycle and limits

Each plugin runs on its own worker thread, outside the compositor's event
loop, so a slow plugin only delays itself. Every request has a three-second
deadline, unless a prompt step sets another, and messages are limited to
1 MiB. An idle worker keeps reading, so notifications arrive at once and a
crash shows up without a request.

Editing or closing the launcher cancels its pending search: queued searches
are skipped, an in-flight request gets `notifications/cancelled`, and late
answers are ignored. The plugin keeps running.

A plugin that misses a deadline, closes its pipes or breaks the protocol is
stopped, along with its process group. The next request starts it again after
a one-second pause; a plugin with resources restarts by itself after that
pause, subscribes again and is read anew. A tool error does not restart the
plugin. A failed start or `launcher_info` call shows under `plugins` and is
retried after the next configuration change or reload.

Plugins get `DISPLAY` once Xwayland is ready; plugins already running are
restarted then. All plugin processes are stopped when the configuration
replaces them and when Tileception exits.
