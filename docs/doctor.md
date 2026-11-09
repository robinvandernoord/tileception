# Doctor

Doctor is an optional panel for asking a coding agent about your Tileception
setup and making changes in plain language. Press **Super+/** to open it;
**Escape** closes the panel.

## Choose an agent and sign in

1. Open Doctor and choose an agent from the
   [Agent Client Protocol](https://agentclientprotocol.com/) (ACP) registry,
   unless you chose one in `tileception setup` already.
2. Follow the sign-in options offered by that agent.
3. Make a request, such as "Please reduce the gaps between windows to 8 pixels."

The panel shows any missing tools before you select an agent. Depending on its
distribution, an agent may need Bun (`bunx`) or Node.js (`npx`), uv (`uvx`),
or tools for downloading and extracting a binary. Doctor starts or downloads
the agent using its registry entry. With Bun installed, npm packages are
fetched with `bunx`; they still run under Node.js when that is installed.
Downloaded agents are kept under `~/.local/share/tileception/agents/`, one
version each.

### Agents you already installed

Doctor uses a copy of an agent that is already on your computer, and says
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
name, so the first time Doctor asks whether to use it or download the agent.
It remembers the answer for that program's path. When an installed package
is older than the registry's version, the agent list shows both versions; the
installed one is still used. To run a particular program for an agent, set its
command under `[doctor.agents]` in the config (see
[Configuration](configuration.md#doctor)).

Accounts, API keys and available models depend on the agent and provider you
choose. Some agents open a browser or terminal for sign-in; others ask for an
API key in the panel. After signing in through a terminal, choose **I've logged
in** in Doctor. If you signed in elsewhere, use `/restart`.

Press **Enter** to send a message. While the agent is working, use the stop
button or **Ctrl+C** to interrupt it. If the agent has not stopped after 8
seconds, Doctor restarts it; press stop or **Ctrl+C** again to restart it right
away. A restarted agent starts a new conversation; the panel keeps the earlier
messages.

## Context

Doctor tells the agent where to find your config, session log, the commented
default config and these guides. Copies of the default config and guides are
written under `~/.local/state/tileception/` (or `$XDG_STATE_HOME/tileception/`)
when an agent starts, so they are available without a source checkout. The
agent reads them only as needed. Existing exports are rewritten only when
their bundled contents have changed; missing files are restored.

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

Doctor also sets the agent's own permission mode, using the modes it actually
advertises. Codex uses `read-only` for Ask and Tweak, and `agent-full-access`
or `full-access` for Yolo. Claude uses `default` for Ask and Tweak, and
`bypassPermissions` for Yolo. Doctor waits for setting changes to be
acknowledged before sending a message. If the agent rejects a setting, the
message is withheld; retry the setting or restart the agent.

OpenCode (both the `opencode` and `opencode-v2` registry entries) starts with
`OPENCODE_PERMISSION='{"edit":"ask","bash":"ask"}'`
in Ask and Tweak, so its edits and commands reach Doctor for approval.
For OpenCode 2, which no longer reads that variable, Doctor also merges those
rules into the child process's `OPENCODE_CONFIG_CONTENT`. Other inline config
values are preserved. Invalid or non-JSON inline config shows a warning because
Doctor cannot merge its rules safely. Your agent's config files are not changed.
Yolo changes the two rules to `allow`. Switching OpenCode's mode restarts
its process and starts a new agent conversation; the panel retains the earlier
messages. Login environment variables entered in Doctor survive that restart.

An agent without a known permission mode shows a warning underneath the mode
switch: its own tools may act without asking. Agent settings and administrative
policies can also impose their own limits. These modes control Doctor's
approvals; they are not an operating-system sandbox.

The next message after a mode switch restates the current mode to the agent.
Paths approved for a change are remembered only for that message, then cleared.

### Changes and undo

Edits made through Doctor's file tools show a change summary and an **Undo**
button. Existing files are backed up before writing. A replacement
`config.toml` is checked before it is saved; an invalid config is rejected.

Backups live in `~/.local/state/tileception/doctor-backups`. Shell commands and
changes made through the agent's own tools do not necessarily use this backup
and undo mechanism.

## Screenshots and images

Use **Print** or **Shift+Print** to capture the screen or a region, choose
**Copy**, then paste into Doctor with **Ctrl+V**. A thumbnail appears beside
your next message. You can also attach an existing image:

```text
/image ~/Pictures/Screenshots/example.png
```

Attachments are sent to your chosen agent with the next message. Image support
depends on the agent and model. If the agent does not accept images directly,
Doctor saves the image locally and includes its file path instead.

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

Scripts can also open Doctor or change its mode:

```sh
tileception msg doctor
tileception msg doctor mode ask
```

`tileception setup` asks which agent Doctor uses, listing those already
installed first; `--doctor-agent <id>` answers up front. From a terminal,
`tileception doctor` does the same:

```sh
tileception doctor agents         # registry id, name, where it is installed
tileception doctor use claude-acp # remembered in doctor.toml
```

A running session reads `doctor.toml` only when it starts, and writes its own
agent back when you change Doctor's mode or model. To switch agents in a
running session, use `tileception msg doctor use <id>` instead.

## If it does not connect

- If an agent lists a missing runtime or tool, install it and refresh the agent
  list. You may need to restart your session if its PATH changed.
- If sign-in is still required, complete the agent's login flow and use
  **I've logged in** or `/restart`.
- If the agent fails to start or disconnects, check the panel's error message
  and the [session log](../README.md#help-and-development).
