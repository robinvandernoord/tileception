# Wizard

Wizard is an optional panel for asking a coding agent about your Tileception
setup and making changes in plain language. Press **Super+/** to open it;
**Escape** closes the panel.

The Wizard used to be called Doctor. The old names still work: `tileception
msg doctor`, `[doctor.agents]` and `doctor` under `[shell]` and in key
bindings.

## Choose an agent and sign in

1. Open Wizard and choose an agent from the
   [Agent Client Protocol](https://agentclientprotocol.com/) (ACP) registry.
2. Follow the sign-in options offered by that agent.
3. Make a request, such as "Please reduce the gaps between windows to 8 pixels."

The agent list opens with the agents you have set up: the one in use, any
you chose a copy for and any in `[wizard.agents]`. Agents installed on your
computer come next, then the rest of the registry. Each agent shows its source
and status: **In use**, **Starting**, **Needs login**, or **Stopped** for the
active agent, and **Update available** if a newer version is in the registry.
Type to search the list by name; the arrow
keys move through it, and **Enter** picks the first agent found. Click the
agent's name at the top of the panel, or type `/agent`, to open the list from
a conversation.

The panel shows any missing tools before you select an agent. Depending on its
distribution, an agent may need Bun (`bunx`) or Node.js (`npx`), uv (`uvx`),
or tools for downloading and extracting a binary. Wizard starts or downloads
the agent using its registry entry. With Bun installed, npm packages are
fetched with `bunx`; they still run under Node.js when that is installed.
Downloaded agents are kept under `~/.local/share/tileception/agents/`, one
version each.

### Agents you already installed

Wizard uses a copy of an agent that is already on your computer, and says
where it found it in the agent list:

- npm packages installed globally with npm, Bun or pnpm, such as
  `npm install -g @google/gemini-cli`;
- Python packages installed with `uv tool install`;
- other agents by the name of their program, such as `opencode` or `goose`.

Programs are looked for in your shell's `PATH`, as an interactive login shell
sets it, so folders that installers add in `~/.bashrc` (like
`~/.opencode/bin`) count, and in common folders such as `~/.local/bin`,
`~/.bun/bin` and `~/.cargo/bin`. The agent then runs with that `PATH` too.

A program found only by its name may be a different program with the same
name, so the first time Wizard asks whether to use it, download the agent or
run another program. Desktop apps are passed over, even when they have an
agent's name: OpenCode Desktop, for example, is not the `opencode` agent.

### Which copy runs

Under the chosen agent, the list offers each copy it can run:

- **Wizard's copy**, which Wizard downloads and keeps, or runs with `bunx`,
  `npx` or `uvx`;
- **Installed copy**, the one Wizard found on your computer;
- **Another program**, any program you name by its path, such as
  `~/.local/bin/opencode`. Wizard starts it with the registry's arguments
  for that agent.

Choosing a copy starts the agent again with it, in a new conversation, and
Wizard remembers the choice for that agent. A command set under
`[wizard.agents]` in the config (see [Configuration](configuration.md#wizard))
takes precedence; remove it there to choose in the list.

### Updates

Wizard keeps agents it downloaded or starts with `bunx`, `npx` or `uvx` at the
registry's version. When a newer version is available in the registry, Wizard
shows an update notice in the conversation. Click **Update now** to restart on
the new version (starting a new conversation). Otherwise the update arrives
the next time the agent starts.

Wizard does not update agents you installed yourself or set in the config. It
still tells you when a newer version is out, and the agent list shows both
versions; update the agent the way you installed it. The registry is checked
again when you open Wizard, at most once a day.

Accounts, API keys and available models depend on the agent and provider you
choose. Some agents open a browser or terminal for sign-in; others ask for an
API key in the panel. After signing in through a terminal, choose **I've logged
in** in Wizard. If you signed in elsewhere, use `/restart`.

Press **Enter** to send a message. While the agent is working, use the stop
button or **Ctrl+C** to interrupt it. If the agent has not stopped after 8
seconds, Wizard restarts it; press stop or **Ctrl+C** again to restart it right
away. A restarted agent starts a new conversation; the panel keeps the earlier
messages.

## Context

Wizard tells the agent which Tileception version is running and where to find
your config, session log, the commented default config, these guides and the
changelog. Copies of the default config, guides and changelog are written
under `~/.local/state/tileception/` (or `$XDG_STATE_HOME/tileception/`) when an
agent starts, so they are available without a source checkout. The agent reads
them only as needed, for example the changelog when you ask what changed in a
version or when something arrived. On a test release, Wizard says that its
changes may not be in the changelog yet, which lists only releases. Existing
exports are rewritten only when their bundled contents have changed; missing
files are restored.

Your messages, attachments and anything the agent reads, such as config files
and log lines, are sent to the agent's provider. Session logs can contain window
titles, paths and commands.

## Permissions

Choose a mode below the input field, or use `/mode`:

| Mode                        | Automatic approvals                                   |
|-----------------------------|-------------------------------------------------------|
| Ask (`ask`)                 | None; changes and permission requests ask first       |
| Tweak (`config` or `tweak`) | Tileception config edits and limited desktop commands |
| Yolo (`yolo`)               | All agent permission requests                         |

Ask is the initial default. Tweak can read Tileception's config and state
folders, edit files in its config folder, and approve `tileception msg tree`,
`reload`, `plugins reload`, `volume`, `mic`, `brightness`, `media` and `home`
commands (which switch smart home devices), `tileception home status`, and
`tileception config set home.enabled true|false`. Other permission requests
are shown for approval.

Wizard also sets the agent's own permission mode, using the modes it actually
advertises. Codex uses `read-only` for Ask and Tweak, and `agent-full-access`
or `full-access` for Yolo. Claude uses `default` for Ask and Tweak, and
`bypassPermissions` for Yolo. Wizard waits for setting changes to be
acknowledged before sending a message. If the agent rejects a setting, the
message is withheld; retry the setting or restart the agent.

OpenCode (both the `opencode` and `opencode-v2` registry entries) starts with
`OPENCODE_PERMISSION='{"edit":"ask","bash":"ask"}'`
in Ask and Tweak, so its edits and commands reach Wizard for approval.
For OpenCode 2, which no longer reads that variable, Wizard also merges those
rules into the child process's `OPENCODE_CONFIG_CONTENT`. Other inline config
values are preserved. Invalid or non-JSON inline config shows a warning because
Wizard cannot merge its rules safely. Your agent's config files are not changed.
Yolo changes the two rules to `allow`. Switching OpenCode's mode restarts
its process and starts a new agent conversation; the panel retains the earlier
messages. Login environment variables entered in Wizard survive that restart.

An agent without a known permission mode shows a warning underneath the mode
switch: its own tools may act without asking. Agent settings and administrative
policies can also impose their own limits. These modes control Wizard's
approvals; they are not an operating-system sandbox.

The next message after a mode switch restates the current mode to the agent.
Paths approved for a change are remembered only for that message, then cleared.

### Plugin tools

[Plugins](plugins.md#wizard-tools) can offer the agent tools, such as the smart
home's: listing devices, explaining why one is unreachable, and switching,
dimming or setting them. Each plugin becomes an MCP server named after it
(`home`), given to the agent when a conversation starts. Calls pass through
Tileception, which decides by the Wizard's mode whatever the agent's own settings
say:

| Mode  | Plugin tool calls                                                      |
|-------|------------------------------------------------------------------------|
| Ask   | Every call asks, showing the plugin, the tool and its arguments        |
| Tweak | Bundled plugins' tools marked read-only and the smart home's go ahead; the rest ask |
| Yolo  | Every call goes ahead                                                  |

Tweak treats the smart home's tools like the `tileception msg home` commands
it already runs; a plugin installed in place of the bundled `home` plugin is
not trusted that way. Other plugins' tools ask in Tweak even when marked
read-only: the mark is the plugin's own claim. Unanswered tool calls time out
and are denied after ten minutes, or sooner if the agent cancels the request.
While the screen is locked, plugin
tools do not run. The agent may also ask on its own before using a tool. The
answers go to the agent's provider like anything else the agent reads. To keep
a plugin's tools from the Wizard, set `[plugins.<id>.doctor] enabled = false`
in `config.toml`.

### Changes and undo

Edits made through Wizard's file tools show a change summary and an **Undo**
button. Existing files are backed up before writing. A replacement
`config.toml` is checked before it is saved; an invalid config is rejected.

Backups live in `~/.local/state/tileception/doctor-backups`. Shell commands and
changes made through the agent's own tools do not necessarily use this backup
and undo mechanism.

## Screenshots and images

Use **Print** or **Shift+Print** to capture the screen or a region, choose
**Copy**, then paste into Wizard with **Ctrl+V**. A thumbnail appears beside
your next message. You can also attach an existing image:

```text
/image ~/Pictures/Screenshots/example.png
```

Attachments are sent to your chosen agent with the next message. Image support
depends on the agent and model. If the agent does not accept images directly,
Wizard saves the image locally and includes its file path instead.

## Commands and settings

| Command           | Action                                                    |
|-------------------|-----------------------------------------------------------|
| `/help`           | List panel and agent commands                             |
| `/agent`          | Choose an agent                                           |
| `/model`          | Choose a model offered by the agent                       |
| `/mode <name>`    | Switch to `ask`, `tweak` or `yolo` mode                   |
| `/image <file>`   | Attach an image to the next message                       |
| `/clear`          | Start a new conversation with the same agent              |
| `/restart`        | Restart the agent, for example after signing in elsewhere |
| `/report [title]` | Report a problem on GitHub; the title is optional         |

Type `/` for suggestions. **Tab** completes a command; **Enter** runs it.

The chosen agent, model and permission mode are remembered in
`~/.local/state/tileception/doctor.toml`, or under `$XDG_STATE_HOME/tileception`
if set. These preferences are separate from `config.toml`.

Scripts can also open Wizard or change its mode:

```sh
tileception msg wizard
tileception msg wizard mode ask
```

## If it does not connect

- If an agent lists a missing runtime or tool, install it and refresh the agent
  list. You may need to restart your session if its PATH changed.
- If sign-in is still required, complete the agent's login flow and use
  **I've logged in** or `/restart`.
- If the agent fails to start or disconnects, check the panel's error message
  and the [session log](../README.md#help-and-development).
