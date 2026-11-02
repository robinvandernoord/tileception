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
| Design | **Tide**: flat graphite surfaces, small radii (6px tiles, 10px popups), a teal to blue accent with a soft glow on focus, no blur by default (Frost planned as a `[theme]` option). Reference in `docs/design.html`. |
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

## Status

Released up to v0.10.0 and running as Robin's session:

- Nested tiling, gaps, floating windows, fullscreen, tab groups with named strips.
- Camera: step in and out, Overview, neighbours, framing one window, history,
  saved views 1 to 9.
- Rearranging: drags with drop previews, keyboard moves, divider and gap
  resizing, multi-window selection with group close, float, tab and move.
- Session: DRM backend, Xwayland, layer-shell, session lock, screencopy and
  screen sharing, suspend (opt out with `general.suspend_enabled = false`),
  idle handling with video inhibition, locking before every sleep (logind
  delay inhibitor), restarting in place (`tileception msg restart`).
- Battery: charge in the bar, a focus border that follows it, low-battery
  alerts, hibernating when nearly empty and power profiles that follow the
  charger. Only with a battery; `[battery]` switches each part off.
- Multiple screens: one tree and camera per output, across GPUs, with
  windows moved by drag or command and rescued on unplug.
- Split screens: one monitor divided into areas (`[outputs."<name>"] split`),
  each with its own tree and camera; see
  [docs/configuration.md](docs/configuration.md).
- Handoff between machines: shared input, clipboard, drag and drop and
  streamed windows (see Handoff between machines below).
- Login screen for greetd ([docs/greeter.md](docs/greeter.md)) and the guided
  `tileception-setup`.
- Tide theming, wallpaper, text rendering, HiDPI and fractional scaling.
- Built-in shell: launcher (calculator, conversions, windows, commands, app
  actions, times, colours, characters, paths, `>` commands), welcome screen,
  top or bottom bar with breadcrumb, minimap and calendar, notifications with
  do not disturb, power menu, lock screen, volume/media/brightness controls
  with audio device switching, minimizing with the bottom strip. All
  swappable through `[shell]`.
- The doctor: an ACP agent panel (Super+/) with ask, config and yolo modes.
- Touchpad: swipe and pinch bindings in `[binds]` (`Swipe3Left`,
  `Super+Pinch2In`), unbound gestures passed to programs (pointer-gestures),
  `[touchpad]` for tap, natural scrolling, typing, speed and click method.
- Touchscreen: touches on windows go to programs (wl_touch); on
  Tileception's own interface a finger acts as the left button. Edge swipes
  (`TouchEdgeTop` and the rest), three-finger swipes and pinches taken back
  from programs, and a two-finger pinch in Overview.
- Window rules, per-app launch arguments, ext-foreign-toplevel-list,
  data-control, pointer constraints, relative pointer and cursor-shape-v1
  (named pointer shapes drawn from Tileception's theme).
- Config hot reload, IPC, headless backend with screenshots for testing.
- Packaging: release CI for trixie, sid, noble and resolute (amd64 and arm64),
  signed APT repository, `tileception-archive-keyring`, `/install` script.

## Roadmap

### Known bugs

- Right-click menus in streamed (handoff) windows do not show on the other
  machine (found 2026-10-30). Not found by reading; the owner logs each menu
  of a streamed window (position and room, grab taken or refused, closed for
  lost keyboard focus) and the other machine logs how many it shows.

### Later

- A settings window instead of editing the config file.
- Frost: Tileception's own bar, launcher, notifications, power menu and cards
  turn translucent and blur what is behind them; windows stay solid.
  `[theme] frost = true`, flat Tide by default. Mockup in
  `docs/design-glass.html` (decided 2026-11-02). Built 2026-11-02 on
  `feature/frost` (`frost.rs`): what is under the panels is rendered
  offscreen, only near a panel and only where it changed, and blurred with a
  dual Kawase blur down to an eighth; panels do not blur one another. Not yet
  the lock screen and greeter, and not tried on hardware.

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
  inside a streamed window, audio and owner-side portal dialogs.
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

## Handoff between machines

Built 2026-10-29 to 2026-11-02 (`tileception-handoff` crate and the
compositor's `handoff` module); the history is in git and the CHANGELOG.
Several Tileception machines on the same network share one mouse and
keyboard, a clipboard and windows, opt-in and only between machines that
share a key (in the spirit of Barrier, Input Leap, lan-mouse and waypipe).
[Development](docs/development.md) describes trying it with two headless
instances.

Decisions:

- Tileception to Tileception only, with our own protocol; no Deskflow, Input
  Leap or lan-mouse compatibility.
- Pairing by a passphrase in each machine's config.toml (`[handoff]`), chosen
  by the user. SPAKE2 turns it into a strong key, so a recorded exchange gives
  nothing to guess against offline; failed handshakes are rate-limited. That
  key is the pre-shared key of a Noise NNpsk0 handshake. Nothing listens
  unless handoff is turned on and a passphrase is set.
- Discovery through mDNS on the LAN, with optional fixed addresses in the
  config for networks that block it.
- The pointer crosses at screen edges, onto the machine arranged on that side.
- On authenticated discovery, both machines show a dismissible popup naming the
  other machine. Each user drags it to any edge of a particular screen; edges
  are independent and need not be opposite. Connect only after both choose.
  Dismissal or timeout on either side cancels the attempt. Do not persist the
  arrangement; each new connection repeats this flow. Suppress repeated prompts
  after cancellation until an explicit retry.
- Desktop bindings act on the active machine, not necessarily the machine with
  the physical keyboard. Within it, they target the active screen and focused
  window, exactly as with multiple local monitors. A fixed return-home shortcut
  is always intercepted by the machine supplying input.
- Local and forwarded input are both accepted; neither requires an exclusive
  controller mode. Normal use assumes only one is active at a time. Track each
  source's held keys and buttons so disconnecting one cannot leave them stuck
  or release another source's input.
- The clipboard and the primary selection are shared lazily, and nothing is
  announced on copy: when the pointer leaves for another machine, that machine
  learns the MIME types on offer (only when they changed), and the content
  goes over only when it is pasted there. A selection from another machine is
  never offered back or passed on.
- Window handoff uses compositor-managed streaming: an already-open app stays
  connected to its owning Tileception compositor, which renders its window for
  an interactive tile on the destination. Input and resize requests travel back
  to the owner. This does not require launching apps through a Wayland proxy.
- On disconnect, the app survives on its owner and the destination shows an
  unavailable tile. The owner can reclaim the window immediately; reconnection
  resumes it only if it has not been reclaimed. There is only one active window
  destination at a time. A sleeping owner makes the window unavailable.
- Locking either participant returns forwarded input home and pauses input,
  window streaming and clipboard transfer between them. Unlocking does not
  automatically resume sharing; reactivation is explicit.

Known behaviour (accepted for now, 2026-10-30):

- Both machines need the same protocol version. A machine that knows the
  passphrase but speaks another version gets a notification on both sides
  naming it and both versions (once per version per session, and it is
  not counted as a failed passphrase guess); older builds than protocol 5
  still only log it.
- Each machine's keyboard follows that machine's own pointer. The desktop's
  keyboard types on the laptop only after the desktop's mouse crossed there;
  moving the pointer onto the laptop with the laptop's touchpad does not bring
  the desktop's keyboard along.

## Testing approach

- `tileception-core`: unit tests for tree invariants (including random edit
  sequences), layout, camera, navigation, drops, floats and resizing.
- `tileception`: unit tests for config and commands, and scripted runs of the
  headless backend with screenshots. Tests never touch the live session: only
  an instance started for the test, through the socket from its own log.
- Never trigger locking or suspend during automated verification.
