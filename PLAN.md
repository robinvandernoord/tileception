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

This roadmap lists **future work only**: what is still to be built, with the
decisions about how. It is not a record of what has been built, how far a
change has got, or what has or has not been tried. When something is
finished, remove its item (keep any part still to come) instead of ticking it
off or adding notes such as "implemented", "verified" or "remains untried".
Report testing in the conversation, the commit or the pull request instead.

### Later

- Doctor reads `CHANGELOG.md` the way it reads the docs: compiled into the
  binary with `include_str!` next to the `docs/` files in `doctor.rs`, so it
  can answer what changed in a version and when something arrived.
- Plugins in more places (the launcher conventions are in
  [the plugin guide](docs/plugins.md)). Decided:
  - New places are new `Place`s in the shared plugin host, like the
    launcher, the panel and commands. Each place's content is a
    `tileception://` resource the host subscribes to, as the panel's is.
  - Each place gets a manifest block with the user's options for it, next
    to `[launcher]`; the plugin itself reports over MCP what it offers
    there. `[lock]` is opt-in: without it, nothing on the lock screen.
    Config overrides use the same blocks (`[plugins.home.lock]`).
  - The instant pure launcher sources (calculations, conversions, times,
    colours, characters) and those needing compositor state (apps, windows,
    commands, app details, `>`, the colour picker) stay built in, as do the
    capture, Do not disturb and media chips.
  - The bundled smart home plugin goes into the remaining places first:
    - Lock screen: the indoor temperature, read-only, following the lock
      screen's privacy level, without input or actions. Never launcher
      plugins.
    - Doctor: plugins offered to Doctor's agent as MCP servers, under
      Doctor's policy for running tools.
  - `tileception msg home` stays as a shorthand for the home plugin's
    `command` tool until key bindings in existing configs have moved to
    `tileception msg plugin home ...`.
  - Timers come later, as a plugin of their own.
  - Possibly: `tileception msg plugin add <command>` writes the config
    entry, so Doctor can set a plugin up on request.
  - Not on the welcome screen or the login screen, not for events (window
    opened, screen added), and not for layout, input or placement.
- Windows larger than their tiles: optionally draw a window only slightly
  too big (within 5 to 10 percent) 1:1 and cut it off instead of scaling
  it, so its text stays sharp.
- Notifications on Tileception's own lock screen (external lockers draw
  only themselves), beyond Tileception's own notifications and a count.
  Decided:

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
- More launcher actions in plain words, such as "set a timer for 10 minutes"
  or "remind me at 3", and better unit conversions. Optional: a tiny local
  tool-calling model (Cactus Needle or similar) to map free text onto those
  actions, with the rule-based parsing kept as the default.
- Feedback from inside Tileception:
  - Optional screenshot attachment: explicit opt-in, preview the captured
    image before including it, and allow removing it before opening the
    issue.
  - For people without a GitHub account: a small relay (OVH host or a
    serverless function) holding a GitHub App's key, which files the issue
    as the App's bot. The key never ships in Tileception. The relay needs
    rate limiting, a length cap and a spam check, and new issues should wait
    in triage before they show publicly.
- Smart home in Settings: import devices, configure cloud sensors, switch
  `[home] enabled` and refresh the device inventory by hand.
- Smart home history: support temperature, humidity or both, including
  standalone sensors. Let users choose which readings to show in Settings,
  according to what each device reports.
- Smart home gateway support (Zigbee and Bluetooth sub-devices), once a
  compatible Tuya gateway is available to try it with. Sub-devices need to be
  addressed by `cid` = node id over one shared gateway connection, with
  reports routed by `cid`.
- Bluetooth device icons by type: the block, nearby list, launcher rows and
  bar chip use one Bluetooth glyph for every device. Pick a glyph from BlueZ's
  `Icon` property (`audio-headset`, `audio-headphones`, `input-mouse`,
  `input-keyboard`, `phone`, ...), adding the few missing glyphs to `ui.rs`,
  and keep the Bluetooth glyph for unknown types.

### Maybe someday

- Window management gestures on the touchpad.
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
