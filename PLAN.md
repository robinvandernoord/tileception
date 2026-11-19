# Tileception: plan

Tileception is a Wayland compositor written in Rust on Smithay. It reimplements the
window model of TrellisWM 0.9.3 for macOS: windows never overlap, they live in a
nested tree of tiles, and you zoom between levels of that tree. This is a clean-room
reimplementation: it copies behaviour, not code or assets (icons, copy text).

## Decisions

| Topic            | Decision                                                                                                                                                                                                                                     |
| ---------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Name             | Tileception (binary `tileception`); chosen for findability.                                                                                                                                                                                  |
| Display server   | Wayland compositor. X11 apps run through Xwayland. No native X11 WM.                                                                                                                                                                         |
| Foundation       | Written from scratch on Smithay. niri and anvil are reading material only.                                                                                                                                                                   |
| Design           | **Tide**: flat graphite surfaces, small radii (6px tiles, 10px popups), a teal to blue accent with a soft glow on focus, no blur by default (`[theme] frost = true` blurs behind Tileception's own panels and behind windows that ask for it). Reference in `docs/design.html`. |
| Shell components | Tide defaults for the launcher, top bar, notifications, power menu and lock screen. Each stays optional and swappable through `[shell]` for external tools such as fuzzel, waybar, swaync, wlogout and swaylock.                             |
| Configuration    | One TOML file, `~/.config/tileception/config.toml`, reloaded on save; bindings are key combinations mapped to commands.                                                                                                                      |
| Licensing        | Dropped. No trial or license code; updates use APT rather than an in-app updater.                                                                                                                                                            |
| Distribution     | Native `.deb` packages through `apt.tileception.space`, with CI publishing to a separate APT host backed by object storage and a CDN. The website and `/install` script deploy separately to the small OVH host.                             |

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
  tileception-updates/ # pending APT updates, kernels, release checks, apt history
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

### Next

- Dashboard surfaces, the rest. Decided:
  - The dashboard's display also offers `linux-dmabuf` (with the same
    feedback as the main display) and fractional scale, so GPU-drawn
    surfaces do not go through shared memory and a fractional output scale
    draws sharp.
  - A cap on each dashboard client's shared memory pools, on top of the
    4096-pixel cap on buffers.
  - Confining a dashboard plugin's process (Landlock or bubblewrap, no
    network) as a manifest option. The protocol limits what it can do to the
    desktop, not to the user's files.
- The premade Tileception ISO (`iso/`, `.github/workflows/iso.yml`, the
  download page), the rest. Decided:
  - On Ubuntu 26.04, `tileception-desktop`'s recommendation of `nautilus`
    pulls in GNOME Shell, GDM and Ubuntu's GNOME session: Nautilus
    recommends `xdg-desktop-portal-gnome` there, which recommends
    `gnome-shell`. (On 24.04 Nautilus takes any portal backend, and
    `xdg-desktop-portal-gtk` is one.) The ISO, which starts from Ubuntu's
    minimal desktop, purges them either way; for
    `apt install tileception-desktop` elsewhere, still to decide: another
    file manager (Thunar brings no desktop with it; Nemo and Caja bring
    parts of Cinnamon and MATE), or Nautilus left out of the
    recommendations.
  - The arm64 ISOs are built but not installed in CI: GitHub's arm64
    runners have no KVM. Try one on an arm64 machine before trusting it.
  - Before release, what someone trying the ISO in a VM meets first:
    `tileception msg` without `TILECEPTION_SOCKET` counts
    `tileception-ssh-agent.sock` as a second instance, and the VMware
    absolute mouse (the kernel's "VirtualPS/2 VMware VMMouse", which QEMU
    offers through its default `vmport` on x86, and VMware itself) moves
    nothing.

### Later

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
  - Not on the welcome screen or the login screen, not for events (window
    opened, screen added), and not for layout, input or placement.
- Plugins from GitHub releases for `plugins add`, beside the Python packages
  it installs with uv: a repository without `pyproject.toml` whose latest
  release has an archive for this architecture, downloaded and checked like
  Doctor's binary agents (`tileception-acp/src/install.rs`), with the same
  standard subcommands. Update replaces the archive with the newest release.
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
- Optional: a tiny local tool-calling model (Cactus Needle or similar) to
  map free text in the launcher onto actions such as timers, with the
  rule-based parsing kept as the default.
- Feedback from inside Tileception for people without a GitHub account: a
  small relay (OVH host or a serverless function) holding a GitHub App's
  key, which files the issue as the App's bot. The key never ships in
  Tileception. The relay needs rate limiting, a length cap and a spam check,
  and new issues should wait in triage before they show publicly.
- Doctor's window, the rest: its place and size are kept only when the
  window is closed, so a move or resize is lost when the session ends with
  it open; and a tiled window comes back as a new tile, without its place
  in the layout.
- Game mode: Tileception's own Super bindings step aside while a game has
  the keyboard, so Super and Super combinations reach the game. Decided:
  - Super+Caps Lock toggles it (check that the binding can see Caps Lock
    with Super held, and that Caps Lock itself is not toggled as well).
  - It also switches on by itself when a game starts. To find out: what
    marks a game reliably (fullscreen plus Steam's `steam_app_*` app ids,
    Proton and Wine, gamescope, Lutris and Heroic, or a per-app rule).
  - A small overlay says game mode is on, and how to leave it, when it
    switches on.
  - Still open: what Super does while it is on (nothing at all, a long hold
    that still reaches Tileception, or one reserved combination).
  - Gaming on Tileception in general, to look into before building: VRR
    and tearing (`wp_tearing_control`), direct scanout of a fullscreen
    game's buffers, Xwayland games and their pointer grabs, gamescope
    nested in Tileception, Steam and its overlay, controllers, Feral
    GameMode, and the keyboard-shortcuts-inhibit protocol that Tileception
    already honours (how it relates to game mode).
  - Smoother frames for games and video: `wp_presentation` (when a frame
    reached the screen), `wp_fifo_v1` (Mesa's Vulkan FIFO present mode uses
    it) and `wp_commit_timing_v1` (a commit shown no earlier than a set
    time). Smithay has all three.
  - `wp_content_type_v1`: a window that says it is a game or video skips
    frost (no blur redone behind it every frame) and keeps the screen
    awake. A game's content type is also a hint for game mode switching on
    by itself.
- Sharing a single window. xdg-desktop-portal-wlr 0.7 (Ubuntu 24.04) only
  captures whole outputs through `wlr-screencopy`, so a browser's "Window"
  choice shares the entire screen. Window capture came in
  xdg-desktop-portal-wlr 0.8.0 through `ext_image_copy_capture_v1`, with
  sources from `ext_image_capture_source_v1` (implied by its release notes,
  to confirm in its source) and windows listed through
  `ext_foreign_toplevel_list_v1`, which Tileception already has. Use 0.8.4
  or later: 0.8.3 sometimes stalls recordings. Smithay 0.7 has neither
  capture protocol, so they are written next to `capture.rs`'s screencopy;
  a window's capture renders only its own surfaces, so a covered window or
  one on another workspace still shares. Before building: try the portal on
  Ubuntu 26.04 to see how it behaves by default (which chooser it uses, and
  whether it needs a Tileception chooser at all).
- Dialogs over their parent: `xdg_dialog_v1` and `zxdg_foreign_v2`. Portal
  dialogs such as the file chooser already float (by their app id), but
  without xdg-foreign they cannot name the window they belong to, so they
  open where any float does instead of centred over their parent, and do
  not follow it to its area on a split screen as other child windows do.
  `xdg_dialog_v1` marks a dialog as modal, which could dim or block its
  parent until it closes. Smithay has both.
- Smart home gateway support (Zigbee and Bluetooth sub-devices), once a
  compatible Tuya gateway is available to try it with. Sub-devices need to be
  addressed by `cid` = node id over one shared gateway connection, with
  reports routed by `cid`.

### Maybe someday

- Window management gestures on the touchpad.
- Slow-resizing clients (Electron, Xwayland) are drawn stretched until they
  commit the new size; pad a buffer smaller than its slot instead, and scale
  down one that is larger (never crop it). Only if it shows up in practice.
- Login screen: `users_as = "tiles"`, message-of-the-day and accessibility
  panels, more than one screen, and the full tile-to-window morph at login.
- Handoff hardening: key rotation and revocation, pointer locks inside a
  streamed window (a program there gets relative motion and holds the pointer
  where the window is shown), audio and owner-side portal dialogs.
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
- Updates of machines linked through handoff in the update manager: what
  waits on each, and installing there from here.
- The update manager in the Super+X power menu, such as a "Restart and
  install" action; updates stay in the update manager.
- Picking Doctor's agent during `tileception-setup`.
- Bindings that repeat while held, pass through to the app or work while
  locked (beyond the media keys); click-to-kill.
- Input methods (text-input and input-method protocols).
- A built-in clipboard history.
- Restyling after LunariaOS (floating bar, pills, capsules and the like);
  Tide's look stays (decided 2026-11-02).
- Cutting off part of a window, for sharper text or any other reason. A
  window too big for its tile is scaled down; slightly soft text is far more
  usable than a hidden edge.
- Glass: blur behind every window. Blur is redone most frames while the
  camera moves, so only windows that ask for it get it (decided 2026-11-02,
  narrowed 2026-11-18).

## Testing approach

- `tileception-core`: unit tests for tree invariants (including random edit
  sequences), layout, camera, navigation, drops, floats and resizing.
- `tileception`: unit tests for config and commands, and scripted runs of the
  headless backend with screenshots. Tests never touch the live session: only
  an instance started for the test, through the socket from its own log.
- Never trigger locking or suspend during automated verification.
