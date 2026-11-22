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

- The premade Tileception ISO (`iso/`, `.github/workflows/iso.yml`, the
  download page):
  - Nautilus stays the file manager on the ISO, which starts from Ubuntu's
    minimal desktop and purges GNOME Shell afterwards. For
    `apt install tileception-desktop` elsewhere on Ubuntu 26.04, Nautilus
    pulls in the whole GNOME desktop: it recommends
    `xdg-desktop-portal-gnome`, which depends on `gnome-shell` (Mutter,
    GNOME Settings, Evolution Data Server), which recommends GDM, Ubuntu's
    GNOME session and GNOME's control centre. GDM then competes with
    greetd for the login screen. None of the other GNOME defaults in
    `packaging/desktop/packages.txt` brings GNOME Shell. (On 24.04 Nautilus
    takes any portal backend, and `xdg-desktop-portal-gtk` is one.) Still
    to decide, after trying them: Thunar (brings no desktop), Nemo or Caja
    (bring parts of Cinnamon and MATE), or Nautilus left out of
    `tileception-desktop`'s recommendations with the ISO installing it
    itself.
  - The arm64 ISOs are built but not installed in CI: GitHub's arm64
    runners have no KVM. Try one on an arm64 machine before trusting it.
  - Before release, what someone trying the ISO in a VM meets first: the
    VMware absolute mouse (the kernel's "VirtualPS/2 VMware VMMouse", which
    QEMU offers through its default `vmport` on x86, and VMware itself)
    moves nothing.
- Looser plugin management commands, in the launcher and in
  `tileception msg`. Decided:
  - `plugin` and `plugins` both manage plugins, with aliases for each verb:
    `add` and `install`; `remove`, `uninstall` and `delete`; `update` and
    `upgrade`; `list`; `configure`. These words are reserved and cannot be
    plugin ids, so `plugin <id> <tool>` still calls a plugin's tool.
  - Hints and docs keep showing one form, `plugins add owner/repo`.
  - Key hints in rows read "Press Enter to …". The rows' wording, in
    `launcher.rs`:
    - "Type plugins add owner/repo, such as …" becomes "Type plugins add
      owner/repo".
    - "Wait for it to finish; a notification says when" (twice) and "A
      notification says when it is done" become "You'll get a notification
      when it finishes", with the busy step (`{busy}…`) as the title.
    - "Finding its commit and reading its pyproject.toml from GitHub"
      becomes "Getting plugin details from GitHub".
    - "Reads its pyproject.toml from GitHub; nothing is installed yet"
      becomes "Press Enter to preview the plugin".
    - "runs its code as you" becomes "Runs with your permissions".
    - "Runs its setup · {source}" becomes "Press Enter to run setup again".
    - "From {source}" on an update row becomes "Checks {source} for
      updates".
    - "{source} at {commit} · Enter configures it" becomes "{source} at
      {commit}. Press Enter to configure".
    - "Runs its uninstall, then removes the package and the manifest
      installing made" becomes "Runs the plugin's uninstall step, then
      removes the package".
    - "No installed plugin starts with {argument}" becomes "No installed
      plugin ID starts with "{argument}"".
    - "No plugins were installed from a repository" becomes "No plugins
      installed from GitHub".

### Later

- The greeter and lock screen on the shared widgets, as AGENTS.md asks of
  every button and field: the submit arrow through `Painter::button`, and
  the visible username field and the masked password on
  `text_input::TextInput` and `SecretField`, drawn with `text::field`, with
  their keys (Ctrl+V in the username field included) and the pointer
  (caret placement, selection, middle click) going through the shared
  paths instead of the greeter's own handler.
- Optional: a tiny local tool-calling model (Cactus Needle or similar) to
  map free text in the launcher onto actions such as timers, with the
  rule-based parsing kept as the default. Switching it on downloads the
  model; nothing ships in a package.
- Feedback from inside Tileception for people without a GitHub account: a
  small relay holding a GitHub App's key, which files the issue as the App's
  bot. The key never ships in Tileception. The relay needs rate limiting, a
  length cap and a spam check. Decided:
  - The relay is self-hosted, run with docker-compose.
  - People write it in the existing "Send feedback" dialog
    (`feedback.rs`), which gets a way to send through the relay beside
    opening the issue in the browser.
  - Issues are filed right away and are public at once, with a label
    marking them as sent through the relay. Spam is closed afterwards.
  - A screenshot attached in the dialog goes to the relay too, which
    stores it and links it in the issue body, so nobody pastes it by hand.
- Game mode: Tileception's own Super bindings step aside while a game has
  the keyboard, so Super and Super combinations reach the game. Decided:
  - Super+Caps Lock toggles it (check that the binding can see Caps Lock
    with Super held, and that Caps Lock itself is not toggled as well).
  - It also switches on by itself when a game starts. To find out: what
    marks a game reliably (fullscreen plus Steam's `steam_app_*` app ids,
    Proton and Wine, gamescope, Lutris and Heroic, or a per-app rule).
  - When it switches on, a small toast at the top centre says game mode is
    on and that Super+Caps Lock leaves it, then fades out.
  - While it is on, Super and its combinations all reach the game. Super
    pressed on its own also shows the same toast again.
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

### Maybe someday

- Smart home gateway support (Zigbee and Bluetooth sub-devices), once a
  compatible Tuya gateway is available to try it with. Sub-devices need to be
  addressed by `cid` = node id over one shared gateway connection, with
  reports routed by `cid`.
- Slow-resizing clients (Electron, Xwayland) are drawn stretched until they
  commit the new size; pad a buffer smaller than its slot instead, and scale
  down one that is larger (never crop it). Only if it shows up in practice.
- Login screen: screen reader and on-screen keyboard switches beside larger
  text and high contrast (nothing in the compositor provides either yet), more
  than one screen, and the full tile-to-window morph at login.
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
- Picking the Wizard's agent during `tileception-setup`.
- Plugins on the welcome screen or the login screen, for events (window
  opened, screen added), or for layout, input or placement. The instant
  launcher sources (calculations, conversions, times, colours, characters),
  those needing compositor state (apps, windows, commands, app details, `>`,
  the colour picker) and the capture, Do not disturb and media chips stay
  built in.
- Retiring `tileception msg home`; it stays as a shorthand for the home
  plugin's `command` tool, beside `tileception msg plugin home ...`.
- Plugins installed from GitHub release archives; `plugins add` installs
  Python packages with uv(env) only.
- Putting a tiled Wizard window back in its old place in the layout when it
  opens again; like any new window, it comes back as a new tile.
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
