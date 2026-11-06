# Tileception: plan

Tileception is a Wayland compositor written in Rust on Smithay. It reimplements the
window model of TrellisWM 0.9.3 for macOS: windows never overlap, they live in a
nested tree of tiles, and you zoom between levels of that tree. This is a clean-room
reimplementation: it copies behaviour, not code or assets (icons, copy text).

## Decisions

| Topic | Decision |
| --- | --- |
| Name | Tileception (binary `tileception`); chosen for findability. |
| Display server | Wayland compositor. X11 apps run through Xwayland. No native X11 WM. |
| Foundation | Written from scratch on Smithay. niri and anvil are reading material only. |
| Design | **Tide**: flat graphite surfaces, small radii (6px tiles, 10px popups), a teal to blue accent with a soft glow on focus, no blur by default (`[theme] frost = true` blurs behind Tileception's own panels). Reference in `docs/design.html`. |
| Shell components | Tide defaults for the launcher, top bar, notifications, power menu and lock screen. Each stays optional and swappable through `[shell]` for external tools such as fuzzel, waybar, swaync, wlogout and swaylock. |
| Configuration | One TOML file, `~/.config/tileception/config.toml`, reloaded on save; bindings are key combinations mapped to commands. |
| Licensing | Dropped. No trial or license code; updates use APT rather than an in-app updater. |
| Distribution | Native `.deb` packages through `apt.tileception.space`, with CI publishing to a separate APT host backed by object storage and a CDN. The website and `/install` script deploy separately to the small OVH host. |

## Architecture

A Cargo workspace:

```
crates/
  tileception-core/    # pure layout engine, no Smithay; unit-tested
  tileception/         # the compositor binary
  tileception-auth/    # PAM password checks for the lock screen
  tileception-apps/    # desktop file index, icons and desktop actions
  tileception-notify/  # org.freedesktop.Notifications service
  tileception-media/   # volume (PulseAudio protocol), MPRIS, backlight
  tileception-acp/     # ACP Registry, agent installs and the ACP client
  tileception-handoff/ # handoff protocol: discovery, handshake, wire format
resources/             # session launcher, login screen entry, power menu, portal config
packaging/             # Debian packaging
website/               # tileception.space and the /install script
```

`tileception-core` is deterministic with no I/O, driven the same way by the
compositor and the tests: the node tree (splits with weights, tiles with tabs,
the Desktop with floating windows), layout in the unit square, the camera
transform, and the world (camera animation, focus, history, saved views, drop
targets, keyboard moves and resizes).

`tileception` has DRM/KMS, winit (nested) and headless backends; a scene drawn
from live client buffers, scaled while the camera moves and 1:1 once settled;
a configure policy that resizes windows to where the camera is heading; and an
IPC socket per session (`tileception msg <command>`) using the same command
language as the bindings.

## Roadmap

### Next

- Frost on the lock screen and the login screen, so `[theme] frost = true`
  covers every panel Tileception draws. Part of `feature/frost`; finish it
  before that branch merges.
- Doctor is missing commands for things the panels can do: asked to clear
  notifications, it found no `tileception msg` command for it and pointed at
  the panel's Clear all button instead. Add `msg` commands for the panel
  actions (clear notifications, Do not disturb, and the like), and pass
  `TILECEPTION_SOCKET` to Doctor's agent so it does not have to look for the
  socket first.

### Later

- Doctor's Ask and Tweak modes the same for every agent. Today only Yolo
  switches the agent's own mode, so OpenCode edits without asking in Ask and
  Codex's sandbox refuses instead of asking. Decided:
  - Every Tileception mode sets the agent's own mode, through the config
    option of category "mode" and the older `session/set_mode`, from a small
    table of known ids: Codex `read-only` for Ask and Tweak and
    `agent-full-access` for Yolo; Claude `default` and `bypassPermissions`
    (check the ids against what Claude lists). Tweak's own policy then
    approves config edits and safe commands as they arrive.
  - OpenCode has no such modes: Doctor starts it with
    `OPENCODE_PERMISSION='{"edit":"ask","bash":"ask"}'` outside Yolo, and
    restarts it when the mode changes. First check that `opencode acp`
    sends "ask" as a permission request rather than rejecting it.
  - An agent without a known ask-first mode gets a warning beside the mode
    switch instead of a silent, weaker Ask.
  - The mode is restated with the next message after a switch, and approved
    paths last for one message, not the whole conversation.
  - Tried headless with OpenCode, Codex and Claude in each mode.
- Small windows stay usable. Today a resting tiled window more than 32 px
  bigger than its tile (usually from a minimum size) becomes a preview: a
  frozen picture that takes no pointer input. Decided:
  - A window is live, drawn from its own updating buffers and scaled down
    if needed, with all pointer input (hover, cursor shape, scrolling,
    clicks, popups, drags) mapped through that scale.
  - It becomes a preview only below a scale threshold, configurable (for
    example 0.5), and still while the camera moves and in Overview.
  - Optionally, a window only slightly too big (within 5 to 10 percent) is
    drawn 1:1 and cut off instead of scaled, so its text stays sharp. This
    replaces `MAX_CLIP` and the exact 1:1 rule.
- Launcher plugins: any program (Python script, Rust binary, anything) as an
  MCP server over stdio. Decided:
  - A small documented convention on top of MCP: a `launcher_search` tool
    taking `{ query }` and returning rows (title, detail, icon, actions) as
    structured output. Actions are mostly done by Tileception itself (copy,
    open a path or link, run a command) or call another of the plugin's
    tools, so every plugin feels like the rest of the launcher.
  - A prefix is required: a plugin only sees queries starting with one of
    its keywords (several allowed, such as `timer` and `remind`). No plugins
    that see every query; a locally matched pattern may come later.
  - Asynchronous: the compositor never waits on a plugin; stale queries are
    cancelled. Plugins are long-running processes with timeouts and a
    restart, trusted like `spawn`.
  - Built-in sources share one internal result interface with plugins.
    `z` folders, timers and the smart home become bundled plugins; the
    instant pure ones (calculations, conversions, times, colours,
    characters) and those needing compositor state (apps, windows, commands,
    app details, `>`, the colour picker) stay built in.
  - Adding one means pointing at a command (any executable, with
    arguments). The plugin reports its name, description, icon and default
    prefixes over MCP (for example a `launcher_info` tool); the user only
    overrides what they want:

    ```toml
    [launcher.plugins.weather]
    command = "~/bin/weather --units metric"
    # prefixes = ["w", "weather"]   # the plugin's own by default
    # enabled = false
    ```

  - The same fields as a `.toml` file per plugin are read from
    `~/.local/share/tileception/plugins/` (installed by the user, such as
    with pip) and `/usr/share/tileception/plugins/` (packages and the bundled
    plugins), so installing one needs no edit to `config.toml`.
    `[launcher.plugins]` in `config.toml` can switch any of them off or change
    their prefixes.
  - Clashing prefixes: `config.toml` wins over the user folder, which wins
    over the system folder; the one that loses is shown in the log and in
    Doctor, never dropped silently.
  - Possibly later: `tileception msg plugin add <command>` writes the config
    entry, so Doctor can set a plugin up on request.
  - The same plugins can later be offered to Doctor's agent as MCP servers.
- A settings window instead of editing the config file.
- Display settings in that window, like Cinnamon's: rotation, resolution and
  refresh rate, scale, arrangement and turning screens off. Screens cannot be
  rotated at all yet, not even through `[outputs]`; add `transform` there
  first.
- More launcher actions in plain words, such as "set a timer for 10 minutes"
  or "remind me at 3", and better unit conversions. Optional: a tiny local
  tool-calling model (Cactus Needle or similar) to map free text onto those
  actions, with the rule-based parsing kept as the default.
- A way to send feedback and feature requests from inside Tileception, for
  example from the launcher or Doctor.
  - First version: "Send feedback" opens a pre-filled new-issue page on
    GitHub (`github.com/robinvandernoord/tileception/issues/new` with title,
    body and a `feedback` label in the query string) in the browser. No
    infrastructure; the issue is filed under the user's own GitHub account,
    so it needs one. The body holds the version; system details only if the
    user ticks a box, shown before sending.
  - Later, for people without a GitHub account: a small relay (OVH host or a
    serverless function) holding a GitHub App's key, which files the issue
    as the App's bot. The key never ships in Tileception. The relay needs
    rate limiting, a length cap and a spam check, and new issues should wait
    in triage before they show publicly.
- Smart home settings window integration: import devices, configure cloud
  sensors and switch `[home] enabled` from the settings window. Deferred to a
  future update; not a blocker for merging `feature/smart-home`.
- Smart home inventory refresh: add a manual refresh action in the future
  settings screen. Startup refresh is implemented in the background, with
  credential retention chosen during import independently of cloud-sensor
  polling. Cached data stays usable during refresh and if a request fails.
- Smart home history: support temperature, humidity or both, including
  standalone sensors. Let users choose which readings to show through the
  future settings panel, according to what each device reports.
- Smart home gateway support (Zigbee and Bluetooth sub-devices). Deferred
  until a compatible Tuya gateway is available for real-device validation;
  not a blocker for merging `feature/smart-home`. These devices currently
  show as unavailable. Sub-devices need to be addressed by `cid` = node id
  over one shared gateway connection, with reports routed by `cid`.

### Maybe someday

- Touchpad gestures that follow the fingers: the camera tracking a pinch or
  swipe live instead of one step per gesture, and window management gestures.
- Top bar calendar events from an .ics file or URL.
- Compare contents or hashes before exporting Doctor's bundled documentation
  and default config, writing only missing or changed files.
- Pick a Doctor agent during `tileception-setup`.
- Slow-resizing clients (Electron, Xwayland) are drawn stretched until they
  commit the new size; crop or pad instead, as niri does. Only if it shows up
  in practice.
- Login screen: `users_as = "tiles"`, message-of-the-day and accessibility
  panels, Ctrl+Space layout switching, more than one screen, reduced motion,
  an accent taken from a shared wallpaper, and the full tile-to-window morph
  at login.
- Handoff hardening: key rotation and revocation, keyboard-layout ownership,
  edge mapping for unequal monitors and pointer-locked apps, shortcut routing
  inside a streamed window, audio and owner-side portal dialogs. Each machine's
  keyboard follows only that machine's own pointer.
- Handoff video: video-heavy windows need a real video codec rather than
  compressed tiles (choose between VA-API, openh264 or ffmpeg first). Popups
  stay where they opened when the window's place there changes.
- Handoff version tolerance, if mismatched versions turn out to be common:
  each machine lists its optional features (window streaming, drag and drop,
  program pointers) in its hello, features only one side has are switched
  off instead of refusing the connection, and unknown messages are skipped
  instead of dropping it. Input and the clipboard stay the core that must
  match. Not now: every feature would need a tested path for each
  combination, and machines updating from the same APT repository match.

### Not wanted

- App-matched layout restoration across logins.
- The wlr foreign-toplevel management protocol for external taskbars.
- Window rules for saved views; views stay purely about the camera.
- Bindings that repeat while held, pass through to the app or work while
  locked (beyond the media keys); click-to-kill.
- Input methods (text-input and input-method protocols).
- A built-in clipboard history.
- Restyling after LunariaOS (floating bar, pills, capsules and the like);
  Tide's look stays (decided 2026-11-02).
- Glass: translucent, blurred windows. Blur behind every window is redone
  most frames while the camera moves; Frost covers what matters
  (decided 2026-11-02).

## Testing approach

- `tileception-core`: unit tests for tree invariants (including random edit
  sequences), layout, camera, navigation, drops, floats and resizing.
- `tileception`: unit tests for config and commands, and scripted runs of the
  headless backend with screenshots. Tests never touch the live session: only
  an instance started for the test, through the socket from its own log.
- Never trigger locking or suspend during automated verification.
