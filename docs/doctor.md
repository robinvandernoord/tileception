# Doctor

Doctor is an optional panel for asking a coding agent about your Tileception
setup and making changes in plain language. Press **Super+/** to open it;
**Escape** closes the panel.

## Choose an agent and sign in

1. Open Doctor and choose an agent from the ACP Registry.
2. Follow the sign-in options offered by that agent.
3. Make a request, such as “Please reduce the gaps between windows to 8 pixels.”

The panel shows any missing tools before you select an agent. Depending on its
distribution, an agent may need Bun (`bunx`) or Node.js (`npx`), uv (`uvx`),
or tools for downloading and extracting a binary. Doctor starts or downloads
the agent using its registry entry.

Accounts, API keys and available models depend on the agent and provider you
choose. Some agents open a browser or terminal for sign-in; others ask for an
API key in the panel. After signing in through a terminal, choose **I've logged
in** in Doctor. If you signed in elsewhere, use `/restart`.

Press **Enter** to send a message. While the agent is working, use the stop
button or **Ctrl+C** to interrupt it.

## Context

Doctor tells the agent where to find your config, session log and a fresh copy
of the built-in default config, including its comments. The default config is
exported to `~/.local/state/tileception/default-config.toml` when the agent starts,
so it is available even without a source checkout. It also tells the agent to
use `tileception msg help` for the current command list.

The Markdown guides are bundled into the binary and refreshed under
`~/.local/state/tileception/docs/` when an agent starts, with the README beside
that directory. Doctor points the agent to `docs/index.md`, so it can find and
read relevant guides without a source checkout. `XDG_STATE_HOME` is respected.

Only the index's location is included in the agent's initial context; the guides
are read as needed. Images and HTML design prototypes are not bundled.
Screenshots are supplied when you attach them.

## Permissions

Choose a mode below the input field, or use `/mode`:

| Mode                        | Automatic approvals                                   |
|-----------------------------|-------------------------------------------------------|
| Ask (`ask`)                 | None; changes and permission requests ask first       |
| Tweak (`config` or `tweak`) | Tileception config edits and limited desktop commands |
| Yolo (`yolo`)               | All agent permission requests                         |

Ask is the initial default. Tweak can read Tileception's config and state
folders, edit files in its config folder, and approve `tileception msg tree`,
`reload`, `volume`, `mic`, `brightness` and `media` commands. Other permission
requests are shown for approval.

Yolo also selects the agent's full-access mode when supported. These modes
control Doctor's approvals; they are not an operating-system sandbox.

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

| Command         | Action                                                    |
|-----------------|-----------------------------------------------------------|
| `/help`         | List panel and agent commands                             |
| `/agent`        | Choose an agent                                           |
| `/model`        | Choose a model offered by the agent                       |
| `/mode ask`     | Return to Ask mode                                        |
| `/image <file>` | Attach an image to the next message                       |
| `/clear`        | Start a new conversation with the same agent              |
| `/restart`      | Restart the agent, for example after signing in elsewhere |

Type `/` for suggestions. **Tab** completes a command; **Enter** runs it.

The chosen agent, model and permission mode are remembered in
`~/.local/state/tileception/doctor.toml`, or under `$XDG_STATE_HOME/tileception`
if set. These preferences are separate from `config.toml`.

Scripts can also open Doctor or change its mode:

```sh
tileception msg doctor
tileception msg doctor mode ask
```

## If it does not connect

- If an agent lists a missing runtime or tool, install it and refresh the agent
  list. You may need to restart your session if its PATH changed.
- If sign-in is still required, complete the agent's login flow and use
  **I've logged in** or `/restart`.
- If the agent fails to start or disconnects, check the panel's error message
  and the [session log](../README.md#help-and-development).
