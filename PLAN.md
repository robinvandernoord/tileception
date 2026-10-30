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
| Design | **Tide**: flat graphite surfaces, small radii (6px tiles, 10px popups), a teal to blue accent with a soft glow on focus, no blur. Reference in `docs/design.html`. |
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

Released up to v0.4.0 and running as Robin's session:

- Nested tiling, gaps, floating windows, fullscreen, tab groups with named strips.
- Camera: step in and out, Overview, neighbours, framing one window, history,
  saved views 1 to 9.
- Rearranging: drags with drop previews, keyboard moves, divider and gap
  resizing, multi-window selection with group close, float, tab and move.
- Session: DRM backend, Xwayland, layer-shell, session lock, screencopy and
  screen sharing, suspend (opt out with `general.suspend_enabled = false`),
  idle handling with video inhibition, locking before every sleep (logind
  delay inhibitor).
- Battery: charge in the bar, a focus border that follows it, low-battery
  alerts, hibernating when nearly empty and power profiles that follow the
  charger. Only with a battery; `[battery]` switches each part off.
- Tide theming, wallpaper, text rendering, HiDPI and fractional scaling.
- Built-in shell: launcher (calculator, conversions, windows, commands, app
  actions, times, colours, characters, paths, `>` commands), welcome screen,
  top or bottom bar with breadcrumb and calendar, notifications with do not disturb,
  power menu, lock screen, volume/media/brightness controls with audio device
  switching, minimizing with the bottom strip. All swappable through `[shell]`.
- The doctor: an ACP agent panel (Super+/) with ask, config and yolo modes.
- Touchpad: swipe and pinch bindings in `[binds]` (`Swipe3Left`,
  `Super+Pinch2In`), unbound gestures passed to programs (pointer-gestures),
  `[touchpad]` for tap, natural scrolling, typing, speed and click method.
- Touchscreen: touches on windows go to programs (wl_touch); on
  Tileception's own interface a finger acts as the left button. Edge swipes
  (`TouchEdgeTop` and the rest), three-finger swipes and pinches taken back
  from programs, and a two-finger pinch in Overview.
- Window rules, per-app launch arguments, ext-foreign-toplevel-list,
  data-control, pointer constraints and relative pointer.
- Config hot reload, IPC, headless backend with screenshots for testing.
- Packaging: release CI for trixie, sid, noble and resolute (amd64 and arm64),
  signed APT repository, `tileception-archive-keyring`, `/install` script.

## Roadmap

### Known bugs

None known. (Fixed 2026-10-17: a second foot opened beside a first crashed
because it got keyboard focus before its first configure; a window now gets
the keyboard only once its first configure is sent.)

### Next

#### Multiple screens

**In progress, 2026-10-29.** Build local multi-screen support before handoff;
Robin has connected a second monitor through the Intel GPU and enabled it in
BIOS. Multi-GPU scanout still needs hardware verification. Each output has
its own tree, camera, saved views, minimized windows and fullscreen state.
Bindings act on the active screen and its focused window. Handoff will use
the same targeting semantics for its active machine and screen.

- [ ] Drive connected outputs across GPUs, with scanout and buffer lifetimes
  tracked per device; retain the single-output nested backend. Secondary GPUs
  use CPU frame copies from the primary renderer for driver compatibility.
- [ ] Pointer crossing activates the destination screen; desktop shortcuts
  target it. Add explicit output focus and window-transfer commands.
- [ ] Move windows by command and compositor drag. On unplug, move windows
  (including minimized windows) to a surviving screen; preserve clients when
  the last screen disappears.
- [ ] Per-output scale and logical positions in `[outputs]`, automatic horizontal
  arrangement otherwise; route commits, popups, layers and capture to their owner.
- [ ] Lock every output. The built-in lock UI follows the active screen; other
  outputs remain black. External lockers can create one surface per output.
- [ ] Verify using multiple headless outputs and existing checks, without adding
  tests or driving the live session. Hardware hotplug, mixed refresh rates and
  mixed scales still need verification with the second monitor.

Log:

- 2026-10-29: Prioritized local outputs before network handoff. Implementation
  started with independent output contexts and active-screen input routing.
- 2026-10-29: Added per-GPU discovery, connector scans, page-flip routing and
  scanout, using frame copies for secondary GPUs. Build and Clippy pass;
  physical NVIDIA/Intel output verification remains pending.

#### Login screen

On the `feature/greeter` branch, not merged or released. See
[docs/greeter.md](docs/greeter.md) and the design in
[docs/design-greeter.html](docs/design-greeter.html).

- [x] `tileception --greeter` for greetd: people as tabs, the sessions (Wayland,
  and X through `xsession_wrapper`), an action launcher (power, sessions, people),
  wrong passwords and PAM messages as notifications, the last user and each
  person's last session remembered, `/etc/tileception/greeter.toml`, the system
  keymap.
- [x] The built-in lock screen is the same tiles, checking the password with PAM,
  with counts instead of notification text and media without titles.
- [x] Login fade: the greeter fades out, the session's first frames fade in.
- [x] Per-person looks: sessions put their accent in AccountsService (the
  `org.tileception.Greeter` extension) and a copy of their wallpaper in
  `/var/cache/tileception-greeter` (the greeter checks each file's owner);
  `[greeter] share = false` takes them back.
- [x] `make try-greeter` (a spare console), `make install-greeter` and
  `make uninstall-greeter` (switching the display manager and back). The `.deb`
  ships the config and the extension but does not switch.
- [ ] Try it on hardware with `make try-greeter`, then as the login screen.
- [x] One setup for `.deb` and `/install` users, shared with the make targets:
  `tileception-setup --greeter tileception|previous` (see Guided installation).
- [ ] Not built yet: `users_as = "tiles"`, message-of-the-day and accessibility
  panels, Ctrl+Space layout switching, more than one screen, reduced motion, an
  accent taken from a shared wallpaper, and the full tile-to-window morph at login.

#### Guided installation

Decided 2026-10-18. Built 2026-10-29 as `tileception-setup` (POSIX sh, shipped in
the `.deb` and `~/.local/bin`, a copy served as tileception.space/setup) plus
`tileception config get|set|add`, which edits config.toml with toml_edit and
keeps its comments. `/install` runs setup with `--print-flags` before
downloading, then applies the flags at the end. `make install` asks nothing and
keeps the config (`SETUP_ARGS` passes choices); `make setup` asks.

- [x] Collect all setup answers early, after quick compatibility and existing-install
  checks and before downloads, builds or system changes, with a summary to confirm.
- [x] "How would you like to set up Tileception?": Recommended, Customize or
  Minimal (no bar, notifications or Doctor; the launcher, Super+T and the lock
  screen stay), plus Keep when a config exists.
- [x] Customize: bar (and its edge), launcher, notifications, power menu, lock
  screen as Built-in, Turned off or another program, saying which run for the
  whole session and which on demand; media keys on or off. Another notification
  daemon goes into `general.autostart`, with what Super+N runs asked separately.
- [x] The greeter question for Recommended, Customize and Keep, defaulting to the
  current login screen; Minimal skips it unless `--greeter` says. Removing the
  package switches back (prerm). make install-greeter, uninstall-greeter and
  try-greeter use the same functions.
- [x] Doctor separately (`[shell] doctor = "builtin" | "none"`), explaining
  agents, accounts and approvals; no agent is configured during setup.
- [x] `--non-interactive`, `--preset`, `--greeter`, `--bar` and the other parts,
  `--notification-daemon`, `--user`, `--yes`; `make install SETUP_ARGS=...`.
- [x] Reinstall reads the desktop user's config (also under sudo, via
  SUDO_USER or `--user`). Precedence: flags and answers, then the existing
  config, then the preset; unattended without a preset keeps everything.
- [x] Answers come from `/dev/tty`; without a terminal nothing is asked and the
  current settings are kept.
- [x] Reopen with `tileception-setup` (or `make setup`); finishes with how to
  enter the session and change things later.
- [ ] Try `/install` end to end on a clean trixie and noble machine, once the
  package with tileception-setup and the site's /setup are published together.
- [ ] Pick a Doctor agent during setup, if that turns out to be wanted.

### Later

- [ ] Compare contents or hashes before exporting Doctor's bundled documentation
  and default config. Write only missing or changed files to avoid unnecessary
  writes and preserve timestamps for unchanged files.
- Touchpad gestures that follow the fingers: the camera tracking a pinch or
  swipe live instead of one step per gesture, and window management gestures.
- Live blur and glass styles.
- A settings window instead of editing the config file.
- Top bar extras: a minimap (live outline of the tree with the camera's frame;
  click to fly, drag to pan), calendar events from an .ics file or URL.
- Slow-resizing clients (Electron, Xwayland) are drawn stretched until they
  commit the new size; crop or pad instead, as niri does. Only if it shows up
  in practice.

#### Handoff between machines

Proposed and decided 2026-10-29; milestone 1 built on `feature/handoff` the same
day (see the review log). Several Tileception machines on
the same network share one mouse and keyboard, a clipboard and windows, opt-in
and only between machines that share a key (in the spirit of Barrier, Input
Leap, lan-mouse and waypipe).

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

- [x] Input: pointer and keyboard to the neighbouring machine and back, active
  machine/screen binding targeting, and the fixed return-home shortcut
  (Ctrl+Super+Escape). Release each source's held input on transfer,
  disconnect, lock or suspend; forwarded input is never passed on. Drags do
  not cross. Touchpad gestures are not forwarded; touching the local screen
  brings input home.
- [x] Clipboard: every MIME type, clipboard and primary, offered on crossing
  with per-machine versions against echoes, 64 MB and 10 s stall limits, and
  cleared when the owner disappears.
- [x] Window handoff, first version (2026-10-30): dragging a window (by
  Super+drag or its title bar) past a shared edge takes it along; dragging
  it back, `handoff reclaim` or a lost connection brings it home; closing it
  there closes it. The owner draws it off screen and sends only what changed
  (LZ4-compressed tiles, the next picture after the last arrived, queued
  behind input). Input, focus and tile size travel back. Verified with
  headless instances in both directions, with typing from either keyboard.
- [ ] Window handoff, still to do: menus and popups past the window's own
  rectangle are cut off; the program's own cursor is not shown over it (the
  plain arrow is); client-side resizing and drag and drop do not cross;
  disconnecting brings windows home at once instead of the decided
  unavailable tile with resume on reconnection; video-heavy windows need a
  real video codec rather than compressed tiles.
- [x] Security: SPAKE2 plus Noise NNpsk0 (ChaCha20-Poly1305), rate-limited
  failures. The lock policy applies to input and clipboard transfers,
  including in-flight ones; streamed windows still to do.
- [x] Discovery (mDNS and fixed `peers`), the edge card on both machines and
  mutual acceptance; the passphrase comes from the config.
- [ ] Dragging files across.
- [ ] Define stable machine identities, key rotation/revocation, keyboard-layout
  ownership, edge mapping for unequal monitors and pointer-locked apps, and
  shortcut routing inside a streamed window. Decide initial support for
  popups/dialogs, Xwayland, audio and owner-side portal dialogs.
- [x] First milestone after local multi-screen support: two machines with
  authenticated discovery and mutually selected edges, input and text
  clipboard. Verified with headless instances on one machine.
- [ ] Try milestone 1 on two real machines: hardware input and acceleration,
  keyboard layouts that differ, lock and resume, suspend, Xwayland pastes.
- [ ] Then prove one already-open streamed window with a popup, resizing,
  locking and disconnect recovery before broadening window support.

Known behaviour (accepted for now, 2026-10-30):

- Both machines need the same build: window handoff changed the protocol, so
  older builds refuse the connection.
- Each machine's keyboard follows that machine's own pointer. The desktop's
  keyboard types on the laptop only after the desktop's mouse crossed there;
  moving the pointer onto the laptop with the laptop's touchpad does not bring
  the desktop's keyboard along.

Review log:

- 2026-10-29: Chose compositor-managed window streaming; confirmed active
  machine/screen shortcut semantics shared with multi-monitor work, coexistence
  of local and forwarded input, pausing sharing on lock with explicit
  reactivation, and owner reclaim with guarded reconnection after disconnect.
- 2026-10-29: Edge choices are independent, with no saved arrangements. Both
  machines must choose through a popup before connecting; dismiss or timeout
  cancels. Local multi-screen support comes first.
- 2026-10-29: Built milestone 1: the `tileception-handoff` crate (service,
  handshake, wire, held input) and the compositor's `handoff` module, with
  `tileception msg handoff ...` and a `motion` pretend-input verb. Chose a
  user passphrase with SPAKE2 over a generated key, and offering selections
  on crossing over announcing every copy. Checked with two and more headless
  instances: fixed addresses and mDNS, a wrong passphrase refused, dragging
  the card, crossing on left/right and top/bottom edges, keys and bindings on
  the other machine, Ctrl+Super+Escape, clipboard and primary both ways (a
  400 kB paste intact), disconnect while forwarding, dismiss and retry.
  Locking was not triggered in tests; the card has no timeout yet.

### Design ideas from LunariaOS

Visual ideas from [LunariaOS](https://lunariaos.com) (a Debian-based desktop,
floating windows, not released, no public source). Its videos show mockups,
so this is about the look, not code: Tide's shape and colours stay, and the
dock, the window buttons and the frosted glass are left out. Screenshots are
frames from the site's videos
([intro](https://lunariaos.com/videos/introduction/startanimation.mp4),
[design](https://lunariaos.com/videos/overview/newuniquedsg.mp4),
[fluidity](https://lunariaos.com/videos/overview/unmatchedfluidity.mp4)),
kept locally in `design/lunaria/` (gitignored, not ours to commit).

Most worthwhile first: the floating bar, the status capsule, concentric radii
and the launcher categories.

- Floating bar: inset by `gap` on every side and rounded with `radius`, so it
  reads as one more tile instead of a strip on the edge. The hairline and
  slide-over behaviour while a window is framed stays.
  [floating-bar](design/lunaria/floating-bar.png)
- Status capsule: the relevant chips (network, volume, battery) share one
  inner pill that grows and shrinks, with the clock beside it, apart from the
  breadcrumb and saved views.
  [status-capsule](design/lunaria/status-capsule.png)
- Concentric radii: a shape inside another is rounded by the outer radius less
  the padding between them. Going further, tile radius could follow depth or
  zoom level so the nesting shows in the shapes.
  [nested-capsules](design/lunaria/nested-capsules.png),
  [window-controls](design/lunaria/window-controls.png)
- Selection pills: the selected item is a filled accent pill, the hovered one
  an outlined pill. For launcher rows, breadcrumb crumbs and saved views.
  [selection-pills](design/lunaria/selection-pills.png)
- Launcher categories: filter chips above the results, taken from the
  `Categories=` key of the `.desktop` files (nothing to name by hand), and a
  footer with the user, files and the power menu.
  [launcher-categories](design/lunaria/launcher-categories.png),
  [launcher-open](design/lunaria/launcher-open.png)
- Notification cards: app icon left, app name bold with the time
  right-aligned on the same line, at most two lines of body, stacked just
  above the bar. [notifications](design/lunaria/notifications.png)
- Generated wallpaper: a soft, defocused gradient drawn by the compositor as
  the default, shifting with the time of day or `colour_scheme`, in Tide's
  colours. [floating-bar](design/lunaria/floating-bar.png) shows theirs.
- Popups open from their origin: the launcher, calendar and notifications
  scale and fade out of the bar element that opened them.
  [launcher-open](design/lunaria/launcher-open.png)

### Not wanted

- App-matched layout restoration across logins.
- The wlr foreign-toplevel management protocol for external taskbars.
- Window rules for saved views; views stay purely about the camera.
- Bindings that repeat while held, pass through to the app or work while
  locked (beyond the media keys); click-to-kill.
- Input methods (text-input and input-method protocols).
- A built-in clipboard history.

## Testing approach

- `tileception-core`: unit tests for tree invariants (including random edit
  sequences), layout, camera, navigation, drops, floats and resizing.
- `tileception`: unit tests for config and commands, and scripted runs of the
  headless backend with screenshots. Tests never touch the live session: only
  an instance started for the test, through the socket from its own log.
- Never trigger locking or suspend during automated verification.
