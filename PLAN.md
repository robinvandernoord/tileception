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
| Design           | **Tide**: flat graphite surfaces, small radii (6px tiles, 10px popups), a teal to blue accent with a soft glow on focus, no blur by default (`[theme] frost = true` blurs behind Tileception's own panels). Reference in `docs/design.html`. |
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
- A premade Tileception ISO: one ISO per supported Ubuntu release and
  architecture with our defaults, no configurator. Decided:
  - No per-user ISOs or selection codes: patching the selection into each
    download would make our own server carry every download.
  - Packages for every supported Ubuntu release, interim releases included.
    The deb matrix in `release.yml` and `DEB_IMAGES` follows
    `ubuntu-distro-info --supported` (and `debian-distro-info`): a new
    release joins at its beta, an old one leaves at its end of life. The
    install script and `docs/installation.md` describe that rule instead of
    naming releases.
  - Built on Ubuntu's Desktop ISO for its graphical installer. A baked-in
    `autoinstall.yaml` keeps language, keyboard, disk and user interactive;
    it installs the minimal desktop, removes GNOME Shell, Ubuntu's GNOME
    session and the Firefox snap, adds the repositories below and installs
    `tileception-desktop`, runs `ubuntu-drivers install`, installs
    Thunderbird from Flathub, adds zoxide's hook to `/etc/skel/.bashrc`,
    and applies the greeter and parts with
    `tileception-setup --non-interactive`. Packages come from the network
    during the install, so ISOs are only rebuilt for new Ubuntu releases and
    point releases. The live session stays GNOME.
  - A `tileception-desktop` metapackage (`Architecture: all`) holds the
    defaults, so changing one needs no new ISO and existing installs can
    get the same desktop with one `apt install`. Depends: greetd, PipeWire
    with pipewire-pulse and WirePlumber, playerctl, NetworkManager, BlueZ,
    UPower, power-profiles-daemon, fwupd, gnome-keyring with
    libpam-gnome-keyring, udisks2 and gvfs, CUPS with
    system-config-printer, systemd-zram-generator, Noto fonts (core, CJK,
    colour emoji) and adwaita-icon-theme. Recommends, so they can be
    swapped: Ptyxis, Nautilus, GNOME Text Editor, Loupe, Papers, File
    Roller with unzip, 7zip and unrar, Showtime, VLC, Amberol, GNOME
    Software with its Flatpak plugin, GNOME Calculator, System Monitor,
    Disks, Disk Usage, Document Scanner, Seahorse, Characters, Calendar,
    Sound Recorder, Clocks, Weather, Maps, LibreOffice, the GStreamer
    libav, ugly and bad plugins, ffmpeg, Firefox, Vivaldi, gh, htop, btop,
    ripgrep, bat, fd-find, fzf, jq, tree, ncdu, duf, tealdeer, fastfetch,
    git, wget, tmux, neovim and zoxide. It also installs
    `/etc/xdg/tileception-xdg-terminals.list` naming Ptyxis, and `bat` and
    `fd` aliases for Ubuntu's `batcat` and `fdfind`.
  - Debs over snaps. Firefox from Mozilla's APT repository (pinned over
    Ubuntu's snap package), Vivaldi from Vivaldi's and gh from GitHub's;
    a small package next to `tileception-archive-keyring` carries their
    keys, sources and the Mozilla pin. Thunderbird has no deb and comes
    from Flathub. No Chromium: Vivaldi covers Chromium-only sites, and
    Ubuntu, Mozilla and trustworthy repositories have no Chromium deb.
    Ghostty stays out: it needs OpenGL 4.3, which VMs often lack.
  - The build lives in this repository (`iso/` and `.github/workflows/iso.yml`),
    beside the setup flags and package names it depends on, and runs
    weekly when Ubuntu's ISOs have changed, when `iso/` changes, or by
    hand; not on every Tileception release, since installs fetch the
    current packages anyway. It remasters the Desktop ISO for each
    supported release and architecture, boots it headless in QEMU (KVM)
    through an unattended install to the greeter, and uploads the ISOs
    straight to Cloudflare R2 behind a download host (no egress fees).
    Only `SHA256SUMS` goes to the apt server, through a restricted SSH
    command next to `publish`, which signs it with the archive key, so the
    key stays off CI. Old ISOs leave when their release reaches end of
    life.
  - `tileception-desktop` and the vendor sources package are built in
    `release.yml`, next to the keyring package.
  - A static download page on the website, generated from a manifest the
    ISO job writes (as `changelog.py` generates the changelog page): one
    entry per release and architecture with size, SHA256, signature, LTS
    or interim and the end of support, and a short guide to writing the
    ISO to a USB stick. `docs/installation.md` gets an ISO section.
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
