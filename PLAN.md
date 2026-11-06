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

- Settings panel: replace the eight TOML section editors with native screens.
  - [x] Displays: detected monitors, draggable arrangement, scale and splits.
  - [x] Sharing: guided setup, discovery, manual peers and connection status.
  - [x] Touchscreen mappings, shortcut recording and action selection.
  - [x] Autostart, application arguments, window rules and battery thresholds.
  - [x] Check desktop and compact layouts in isolated instances, then request
    Opus code and visual review. Keep raw source as an advanced option.
  Documentation is deferred until functionality is finalized. No commits by
  the coding agent.
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
    subdued affordances and draft navigation clarity. No changes made from
    these new evaluations pending reporting them to Robin.
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
