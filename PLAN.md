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

### Parallel execution, started 2026-11-06

Workspace credits interrupted the display backend, display UI and remaining
verification workers; work resumed after the user's Continue. Combined
`make test` and `make lint` pass, including the independent crates. The parent
built and exercised the combined binary on isolated desktop and compact
headless instances. Evidence: `target/integration-verification/final-parent/REPORT.md`.

- [x] Implemented: Doctor permission modes and bundled-document export.
  Existing ACP and Doctor-policy checks, build and controlled headless checks
  passed. Verified acknowledged mode changes, per-message approvals, mode
  warnings and unchanged exports. Real provider-backed edits remain untried:
  Claude/Codex authentication was unavailable and OpenCode was checked through
  primary source and fixtures.
- [x] Implemented: live scaled windows, with a configurable 0.5 threshold
  and percentage-based clipping deferred. Headless checks cover updating
  Wayland buffers, mapped clicks and drags, popup activation and the preview
  threshold. Existing 170 compositor tests, build, formatting and strict
  Clippy passed in the worker's snapshot.
- [x] Implemented: launcher plugin foundation and one example plugin.
  Configuration wiring, the commented default-config example and configuration
  guide link are integrated. Worker reports successful headless search/action,
  cancellation, timeout/restart, prefix-conflict and process-cleanup checks;
  final build and smoke verification passed. URL opening and compositor exec
  restart were not manually exercised.
- [x] Coordinator: integrate shared-file changes and existing frost work,
  then verify the combined compositor in isolated headless instances.
  Existing frost implementation from `d6f6e9f` applied as file changes;
  configuration documentation updated. Greeter frost and popup layering
  passed; parent verified the combined plugin/feedback/Settings/display
  preview and revert paths, including unchanged config hashes.
- [x] Implemented: Settings compact layouts, control clarity and draft
  navigation, retaining the current visual design. Headless desktop and
  640x480 checks covered Apply, Back, invalid/source draft retention,
  keyboard access, scrolling and clipboard editing. Dead-key composition
  remains unverified.
- [x] Implemented: feedback confirmation with opt-in system details and an
  exact preview. Intercepted opener payloads matched preview titles/bodies,
  including Unicode; compact layout checked down to 320x240. Actual browser
  navigation was not exercised. Usage documentation updated.
- [x] Implemented: display backend using the approved output configuration
  contract below. Headless checks cover rotation/capture geometry, fractional
  refresh, disable/re-enable, all-disabled rejection, timeout, confirmation,
  save-failure rollback, stale timers and window/focus restoration. Existing
  scale/output tests, build and strict Clippy passed. Physical DRM modesetting
  and nested interaction remain untried. Configuration documentation updated.
- [x] Implemented: display Settings UI using the backend preview API.
  Parent verified desktop and 640x480 rendering, disabling a rotated second
  monitor through preview, and Escape restoring it without writes. Worker
  checks also passed rotation/flip, fractional refresh, disable/re-enable,
  all-disabled rejection, Keep/Revert, Back/close/timeout/reload cancellation,
  comment-preserving persistence and save-failure recovery. Physical hotplug
  and hardware-specific unsupported modes remain untried. Final handoff did
  not change source after the parent's accepted combined build.
- [x] Remaining scaled-window and frost headless verification completed on
  the worker's frozen binary. Passed relative motion, constraints/hints,
  continuous scroll/wheel notches, cross-area drag, localhost handoff with
  popup selection, X11 popup at output scale 1.5, and greeter frost/popup
  layering. Resting X11 parent shrink below 1.0 remains unverified. Evidence in
  `target/integration-verification/REPORT.md`.

### Next

- Settings panel: replace the eight TOML section editors with native screens.
  - [x] Displays: detected monitors, draggable arrangement, scale and splits.
  - [x] Sharing: guided setup, discovery, manual peers and connection status.
  - [x] Touchscreen mappings, shortcut recording and action selection.
  - [x] Autostart, application arguments, window rules and battery thresholds.
  - [x] Check desktop and compact layouts in isolated instances, then request
    Opus code and visual review. Keep raw source as an advanced option.
  Usage and configuration documentation updated. No commits by the coding agent.
  Native forms retain source comments and keep unaccepted drafts when returning
  to Settings. Verified all eight screens saving through the UI, arranging two
  headless monitors, and connecting two isolated sharing instances on localhost.
  Review fixes preserve comments above keys, literal strings, multiline arrays
  and threshold tables. Verified modifier-tap and Super+Escape recording, native
  overrides of built-ins, and unchanged default sections. Build, strict Clippy,
  formatting and all 160 existing compositor tests pass.
  - [x] Replace the launcher app-slot entry with a dedicated gear and add
    Super+comma. Collapse advanced appearance controls and add an XKB picker.
  - [x] Keep all nine overview categories in a masonry tile layout, remove
    retention notices, and add optional proportional full-edge screen crossing.
  Verified horizontal, reverse and vertical proportional crossing on unequal
  headless outputs, the launcher gear in app-grid mode, the layout picker saving
  Dutch, the Advanced disclosure and the nine-category masonry overview.
  - [x] Collect evaluation-only scorecards from Opus, Mistral Large 4 and
    GLM 5.3 (the latter two via Vibe). Report them before acting on feedback.
  Evaluations collected: Opus 7.5/10 overall from direct screenshot inspection;
  Mistral 8/10 using OCR/pixel analysis; GLM declined an overall score without
  image access. No implementation changes based on these evaluations.
  - [x] Review follow-up: one-step Apply in native screens, optional TOML
    comparison, constrained forms, consistent actions and a distinct sharing guide.
  - [x] Verify the updates, then collect evaluation-only scorecards from Opus
    and a fresh-context Sol reviewer. Do not use Vibe for future visual reviews.
    Build, formatting, strict Clippy and 160 existing tests passed. Isolated
    headless checks verified one-click saving and Back staging valid edits.
    Opus scored 7.5/10 overall; fresh-context Sol scored 8/10. Both inspected
    screenshots directly. Remaining concerns include compact-screen clipping,
    subdued affordances and draft navigation clarity.
  - [x] Follow-up approved 2026-11-06: fix compact-screen clipping, clarify
    controls and draft navigation, retaining the current visual design.
- Frost on the lock screen and the login screen, so `[theme] frost = true`
  covers every panel Tileception draws. Part of `feature/frost`; finish it
  before that branch merges.
  Existing implementation integrated as file changes. Standalone headless
  greeter frost on/off and popup layering passed; built-in locking was not
  triggered, following the automated-verification restriction below.

### Later

- Doctor's Ask and Tweak modes the same for every agent. Implementation
  completed 2026-11-06; provider-backed editing remains untried. Decided:
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
  - Verification target: OpenCode, Codex and Claude in each mode. Controlled
    fixtures passed; actual provider-backed edits remain untried.
- Small windows stay usable. Implemented 2026-11-06.
  Headless scrolling, pointer constraints, cross-area drags, handoff
  and X11 popup at output scale 1.5 passed. A resting X11 parent shrunk below
  1.0 remains unverified. Decided:
  - A window is live, drawn from its own updating buffers and scaled down
    if needed, with all pointer input (hover, cursor shape, scrolling,
    clicks, popups, drags) mapped through that scale.
  - It becomes a preview only below a configurable scale threshold,
    defaulting to 0.5, and still while the camera moves and in Overview.
  - First version replaces `MAX_CLIP` and the exact 1:1 rule with live
    scaling. Optional percentage-based clipping is deferred
    (decided 2026-11-06).
- Launcher plugins: any program (Python script, Rust binary, anything) as an
  MCP server over stdio. Decided:
  - First release: launcher discovery, search, actions and lifecycle, with
    one example plugin. Migration of built-in sources and integration with
    the bar, notification panel and lock screen are later phases
    (decided 2026-11-06).
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
  - Other places, through the same MCP plugin: each place only asks a
    plugin for what it offers.
    - Bar: small status items with a click action (CI status, VPN, timers),
      as MCP resources the bar subscribes to. The strongest next place.
    - Notification panel: blocks like the smart home one, which becomes the
      first, bundled.
    - Key bindings: `tileception msg plugin <name> <tool>`.
    - Lock screen: only read-only status items from plugins explicitly
      allowed there (`lock = true`), following the lock screen's privacy
      level, without input or actions. Never launcher plugins.
    - Not on the welcome screen or the login screen, not for events (window
      opened, screen added), and not for layout, input or placement.
- Notifications on Tileception's own lock screen (external lockers draw
  only themselves). Today only Tileception's own notifications show there,
  plus a count. Decided:

  ```toml
  [lock]
  notifications = "count"   # "none" | "count" | "apps" | "full"
  [lock.notification_apps]  # exceptions per app
  "org.gnome.Calendar" = "full"
  ```

  - `notifications` is what every app gets; `notification_apps` holds the
    exceptions. A low default with "full" exceptions is opt-in; "full"
    with lower exceptions is opt-out. The default stays "count".
  - A "Show on lock screen" toggle per app in the notification panel
    writes that exception, so opting in needs no config editing.
  - No buttons on the lock screen (they would run an app's action without
    unlocking), except Tileception's own. Do not disturb applies there too.
- Display settings in that window, like Cinnamon's: rotation, resolution and
  refresh rate, scale, arrangement and turning screens off. Backend controls
  and safe preview implemented 2026-11-06. Settings UI final verification,
  combined checks and parent preview/revert smoke verification passed.
  - Split backend support from Settings UI work after agreeing the
    configuration fields. Disruptive changes need confirmation and
    automatic rollback (decided 2026-11-06).
  - Configuration approved 2026-11-06, under `[outputs."connector"]`:
    `enabled = true`, `transform = "normal"` (also `"90"`, `"180"`,
    `"270"` and flipped variants), and `mode = "preferred"` or a mode
    string such as `"1920x1080@59.94"`. Existing scale, position and split
    fields remain. Settings changes need confirmation within 15 seconds or
    revert; disabling every connected screen is rejected.
- More launcher actions in plain words, such as "set a timer for 10 minutes"
  or "remind me at 3", and better unit conversions. Optional: a tiny local
  tool-calling model (Cactus Needle or similar) to map free text onto those
  actions, with the rule-based parsing kept as the default.
- A way to send feedback and feature requests from inside Tileception, for
  example from the launcher or Doctor.
  - Done: "Send feedback" in the launcher and `tileception msg feedback
    [title]` open a pre-filled new-issue page on GitHub (title, body with
    the version, `feedback` label) in the browser, filed under the user's
    own account.
  - Done 2026-11-06: a confirmation view before the browser opens, with an unchecked
    "Include system details" option and an exact preview of what will be
    included. The anonymous relay remains deferred (decided 2026-11-06).
  - Done 2026-11-06: editable title and multiline feedback before Review,
    using shared text-field editing. Desktop/320x240 checks covered compose,
    selection, clipboard editing and exact browser payloads. Created the
    repository's missing `feedback`
    label; the issue template still needs to reach the default branch for
    automatic template labels for non-maintainers.
  - [ ] Optional screenshot attachment: explicit opt-in, preview the captured
    image before including it, and allow removing it before opening the issue.
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
