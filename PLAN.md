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
| Design | **Tide**: flat graphite surfaces, small radii (6px tiles, 10px popups), a teal to blue accent with a soft glow on focus, no blur. Mockups in `design/graphite.html` (variant F). |
| Shell components | Provide Tide defaults for the Super menu/launcher, notification bar and popups, power menu and lock screen. Following Linux's modular philosophy, each must remain optional and swappable for external tools such as fuzzel, swaync, wlogout and swaylock. |
| Configuration | One TOML file, `~/.config/tileception/config.toml`, reloaded on save; bindings are key combinations mapped to commands. |
| Licensing | Dropped. No trial or license code; updates use APT rather than an in-app updater. |
| Distribution | Native `.deb` packages through `apt.tileception.space`, with CI publishing to a separate APT host backed by object storage and a CDN. The website and `/install` script deploy separately to the small OVH host. |

## 1. What Trellis is (findings from the DMG)

Sources: the extracted bundle (`Info.plist`, `Resources/ReleaseSettings.json`) and the
strings and Swift type metadata of the arm64 `TrellisWM` binary. No decompiler was
available, so the findings come from type names, field names, log messages and UI copy.
Anything marked *inferred* is a reading of those names, not observed code.

### 1.1 Identity and structure

- `app.trellisui.trelliswm` 0.9.3 (build 162), macOS 14+. It is a menu bar agent
  (`LSUIElement`) by Dan Fessler and uses Sparkle for updates.
- There are three Swift modules:
  - `SpacesEngine` (`Engine`, `ManagedWindow`): the platform-neutral layout engine.
    Its source files are `Tree`, `Arrangement`, `Plan`, `Navigation`, `Navigator`,
    `Spatial`, `Scene`, `Motion`, `Move`, `Resize`, `Collapse`, `Document`, `Types`
    and `Feel`.
  - `TrellisWM`: the macOS shell. Its source files are `MacPlatform`, `MacSpaces`,
    `MacWindowFeatures`, `AX`, `EventTap`, `Hotkeys`, `Multitouch`,
    `TrackpadGestures`, `Capture`, `Proxy`, `Stage`, `Render`, `Presenter`,
    `TabStrip`, `TrafficLights`, `MenuBarStrip`, `Settings`, `Welcome` and others.
  - `Licensing`: LemonSqueezy license handling. This module is dropped.
- A `WindowSystem` / `Platform` protocol sits between the engine and macOS
  (`MacWindowSystem`, `MacClock`). We keep the same split: a pure core crate and a
  compositor crate.

### 1.2 The window model

- **No overlap.** Windows are "arranged into places you zoom between: out to see
  everything, in to whatever you need."
- **Layout tree** (`LayoutDocument`, `LayoutNode`, persisted as JSON with the keys
  `kind`, `axis`, `weights`, `children`, `views`, `selected`, `child`, `tabbed`):
  - `SplitNode`: an axis plus weighted children. Spaces "nest inside each other as
    deep as you like."
  - `PanelNode` / `Tile`: holds one window, or a tab group (`views` plus `selected`).
  - `DesktopNode`: the special "Desktop" tile. Undocked windows float on it
    (`floats`, `floatSizes`, `dockSpots`). It can be hidden per space ("Hide or show
    the Desktop in this Space"). "The Desktop's own space always stays clear."
- **One World per display** (`Coordinator.World`, `Coordinator.Display`,
  `JSONDisplayStore`, `SavedDisplay`, `displaySignature`). Layouts are saved per
  display configuration.
- **Window identity across restarts**: `WindowInfo { appName, bundleID, title, frame,
  dockable, size }`. Placements are rematched by app and title (*inferred*).
- **Excluded apps** (`excludedBundleIDs`): "Windows of these apps are left alone:
  they float freely and are never arranged."

### 1.3 Navigation (the "Desk" and the camera)

- The **Desk** is the zoomable plane that holds the whole tree. A camera
  (`Camera`, `CameraCurve`, `Projection`, `framedNode`, `framedRect`) frames one
  node so that it fills the screen.
- Commands (shortcut identifiers from the binary): `everything` (Overview: show all),
  `stepIn` ("Step In Toward Focused Window", or toward the pointer when
  `stepTowardPointer` is set), `stepOut` ("Step out a level"), `left`, `right`,
  `up`, `down` ("Neighbouring space to the left/right/above/below"), `back` and
  `forward` ("Step back and forward through the spaces you've framed"), `maximize`
  ("Frame the focused window on its own"), `desktop` ("Hide or show the Desktop in
  this Space") and `split` ("Split the active tile with a new Finder window").
- **Overview**: "Click to go in, Esc to go out, Pinch or scroll to zoom, Drag windows
  or gaps to rearrange." In Overview, dragged windows always move as pictures and the
  layout makes room.
- **History**: `visits` and `visitIndex` drive back and forward.
- **Saved views**: modifier+1..9 saves the current view and another modifier+1..9
  recalls it (`savedViews`). "Going to and saving views need different modifiers."
- `⌘⇧`-drag a rectangle to zoom to what it frames (`RectZoom`, `MarqueePanel`).
- Clicking during an animation ends it (`interruptLandings`).

### 1.4 Rearranging

- **Dock by edge**: "Drag a window to the edge of another, or of the screen" to split
  (`DropResolver`, `DropTarget`, `DockTarget`, `DockSpot`, `dropSlot`,
  `DropPreviewPanel`).
- **Tab by centre**: "Drop a window on the middle of another to make them tabs of one
  window."
- **Hold over a target** (`dropHold`): a delay before the layout makes room. Letting
  go drops at once.
- `reflowWhileDragging` ("Windows make room live during drags") and `keepPushed`
  ("Pushed windows stay pushed while dragging").
- `previewDrags`: either "Move the real window, show where it will land" or "Move a
  picture and make room, like Overview."
- **Gap handles** (`deskGapHandles`, `GapHandleWindow`): "Drag the gaps between
  windows to resize them." This changes the split weights.
- **Closing**: `shareRemovedSpace` ("So closing a window exactly undoes adding it.
  Otherwise the neighbour after it takes all its space.").
- **Minimum sizes**: `squeezeBelowMinimum`, `fixedMinimum`, `WindowLimits`. A window
  whose tile is too small is shown as a preview ("It's a real window again as soon as
  it fits.").
- `soleFloatTakesDesktop`: a lone floating window fills the Desktop's space.
- Double-clicking a title bar frames the window on its own.

### 1.5 Rendering tricks forced by macOS (simpler on Wayland)

macOS does not let another app scale or hide windows, so Trellis fakes it:

- **Proxies**: still pictures taken with ScreenCaptureKit (`Capture`, `ProxyLayer`,
  `RestingProxy`, `PreviewView`) are animated on a full-screen overlay (the
  `Stage`, `StagePanel`, `StageRenderer`). The real windows "land" afterwards
  (`LANDING`, `SETTLED`, `LANDED` logs).
- **Parking**: windows outside the framed node are moved off screen
  (`parkOutOfViewSmall`, phase `parked`).
- Window phases: `live`, `proxy`, `covered`, `preview`, `parked`, `desk`, `moving`,
  `overview`, `landing`, `fading`.
- Covers while a window resizes: app colour plus icon (`appColours`, `coverIcon`,
  `resizeEffect`).
- Dimming and blur behind arranged windows (`DimPanel`, `BlurredBackdrop`,
  `dimOpacity`, `dimBlur`, `blurRadius`).
- Tab strips are separate overlay panels drawn above the grouped window
  (`TabStripPanel`; styles `material`, `opaque` and `tinted`).

On Wayland we own the scene graph, so we render each client surface scaled and
translated directly from its live buffer. Proxies, parking and landing collapse into
two concerns: when to send a new `configure` size to a client, and what to draw while
it has not committed a matching buffer yet.

### 1.6 Default tunables (`ReleaseSettings.json`)

These become our config defaults: `gap` 12, `tabInset` 2, `tabPadding` 8,
`tabChipMargin` 4, `dimOpacity` 0.3, `dimBlur` 20, `blurRadius` 40, `coverFade` 0.2,
`tintOpacity` 0.8, `appColourAmount` 0.25, `panSensitivity` 1.8,
`desktopEdgeStrength` 0.05, and booleans `shareRemovedSpace`, `keepPushed`,
`interruptLandings`, `soleFloatTakesDesktop`, `squeezeBelowMinimum`,
`windowButtonsActOnGroup`, `historyButtons`, `previewDrags`, `dimDocked` (all true by
default).

### 1.7 Out of scope for us

Licensing and LemonSqueezy, Sparkle, the welcome and permissions wizard, problem
reports, the menu bar strip, Mission Control and Stage Manager conflict detection,
and migration from "Spaces".

## 2. Architecture

A Cargo workspace:

```
crates/
  tileception-core/   # pure layout engine, no Smithay; unit-tested
  tileception/        # the compositor binary
resources/            # session launcher, login screen entry, power menu, portal config
design/               # HTML mockups of the design languages
```

### 2.1 `tileception-core` (the SpacesEngine counterpart)

Deterministic, no I/O, driven the same way by the compositor and the tests.

- `tree`: an arena of nodes (`Split` with axis and weights, `Tile` with tabs,
  the `Desktop` with floating windows), stable ids, redirects for removed nodes,
  boundaries for resizing, invariant checks.
- `layout`: the tree laid out in the unit square; hidden nodes take no room.
- `geom`: rectangles and the camera transform (non-uniform scale per axis).
- `world`: the camera and its animation, focus and MRU, history, saved views,
  drop targets, keyboard moves and resizes, floating windows in screen pixels,
  tab strips.

### 2.2 `tileception`

- Backends: DRM/KMS + libinput + libseat for the session, winit for running
  nested, and a headless backend (render node, no screen) for tests.
- Protocols: xdg-shell, xdg-decoration (server side), layer-shell,
  ext-session-lock, wlr-screencopy (own implementation), linux-dmabuf,
  drm-syncobj (explicit sync for NVIDIA), data-device and primary selection,
  Xwayland with clipboard bridging.
- Scene: windows drawn from their live buffers, scaled while the camera moves
  and 1:1 once settled; floating windows above the layout; layer surfaces above
  and below; cursor drawn by the compositor.
- Configure policy: windows are resized to where the camera is heading, so they
  redraw during the animation; in Overview nothing is resized.
- Input: bindings from the config (key combinations, modifier taps, wheel),
  drags for moving, resizing and gaps, built-in keys for VT switching.
- IPC: a socket per session, `tileception msg <command>`, the same command
  language as the bindings.

## 3. Status

Done and running as Robin's session:

- Nested tiling, gaps, floating windows above the layout, fullscreen.
- Camera: step in and out (keys and Super+scroll), Overview, neighbours, framing
  a single window, history back and forward, saved views 1 to 9.
- Rearranging: Alt+drag and title bar drags with drop previews (dock on edges,
  tab in the centre, float), keyboard moves, divider resizing by keyboard, mouse
  (Alt+right drag) and gap dragging, toggle split, Alt+Tab cycling.
- Tab groups (strip without titles yet).
- Session: DRM backend, Xwayland, layer-shell (bars, launchers), session lock,
  screencopy and screen sharing portal config, modifier taps (Super alone).
- Config file with hot reload, multiple commands per binding, IPC, headless
  backend with screenshots for testing.

## 4. Roadmap

### Right now (Robin)

- The coming-days implementation is ready for session verification. Run
  `make install`, then log out and back in to load it; the installer also
  installs the PAM service. The running desktop has not been changed.
- Existing default fuzzel and swaync bindings are translated when loading the
  config. If using a custom `[binds]` table, add `"Super+G" = "group"` and
  `"Super+Shift+F" = "float"` if those new shortcuts are absent.

### Today (done)

1. Tide inside Tileception: rounded tiles and window corners, hairline
   borders, the teal focus border with glow, soft shadows under floating
   windows, a Tide drop preview, and a `[theme]` section in the config.
2. Text rendering in the compositor (cosmic-text).
3. Named tab strips: the program's title per tab, the shown tab tinted and
   underlined in the accent, a close mark. (Real app icons come with the app
   index.)
4. Built-in power menu (Super+X): lock, log out, suspend, restart, power off.
5. Wallpaper: `wallpaper` and `wallpaper_fit` in `[theme]`, drawn behind
   everything; the `lock` command passes it to the locker.

### Coming days

Status: functional corrections after laptop feedback. Lock and notification
design work is reserved for Claude, using `docs/design.html` as the reference.
Suspend/resume has passed Robin's hardware test and is enabled by default, with
`general.suspend_enabled = false` available to opt out. Automatic idle locking
remains disabled. Do not trigger locking or suspend during automated
verification. Automatic layout saving/restoration is off by
default; the app-matching behavior was not agreed and is deferred.

1. Built-in lock screen (Super+L) with the normal password and the wallpaper,
   replacing swaylock (uses the `tileception-auth` crate from Codex).
2. Built-in launcher (tap Super), with app search and icons, replacing fuzzel
   (uses the `tileception-apps` crate from Codex).
3. Built-in notifications: popups and a history panel with do-not-disturb
   (Super+N), replacing swaync, including screenshot feedback (uses the
   `tileception-notify` crate from Codex).
4. Selecting several windows: Shift+click and Super+Shift+drag (plain drag in
   Overview); then Super+C closes all, Super+Shift+F floats or docks all,
   Super+G makes them tabs of one tile, Super+Shift+arrows moves them.
5. Layouts that survive a re-login: clarify the intended scope first. The
   experimental app-matching implementation is opt-in, not enabled by default.
6. Idle handling: screen off and lock after a while, not during video.

Verification checklist:

- [x] Existing 64 tests, formatting and Clippy checks pass.
- [x] Isolated screenshots: lock screen with wallpaper, launcher with app
  icons, notification history with thumbnail and action buttons.
- [x] Reopened windows recover saved split proportions and saved views across
  compositor restarts; floating geometry is serialized successfully.
- [x] Scripted selection, tab grouping, group float/dock and group close.
- [x] Short headless idle timer reaches locked and display-asleep states.
- [x] Robin verified suspend/resume on hardware and confirmed password dots.
- [ ] Manually verify launcher editing (including compose input), corrected
  window selection/group actions, notification action clicks, physical display
  wake without suspend, and video idle inhibition on the laptop. Confirm password
  rejection and successful unlock if not already covered by the suspend test.

Feedback corrections:

- [x] Copy the Tide reference, including its styles and examples, into
  `docs/design.html`; do not commit without an explicit commit request.
- [x] Add an editable launcher field with caret, selection, word navigation,
  deletion, and clipboard shortcuts. Handle focus changes outside Smithay's
  locked keyboard filter.
- [x] Replace the launcher's `wl-copy`/`wl-paste` helpers with direct clipboard
  transfers, including the X11 bridge. Clipboard helpers could create temporary
  toplevels that changed the layout underneath the launcher. Keep paste reads
  asynchronous and bounded, and ignore results after the launcher is reopened.
- [x] Remove the opaque Desktop placeholder so wallpaper fills empty space.
- [x] Include the focused window when starting a Shift+click selection and
  add a distinct selection marker. Add the missing group/float shortcuts to
  Robin's custom config.
- [x] Move selected tiled windows as one subtree, retaining internal splits,
  tab groups and proportions, instead of docking them one at a time.
- [x] Split within a zoomed tile when opening a new tiled window, instead of
  joining its outer row and exposing unrelated windows. Make selection zoom
  (Super+plus/Super+I) and split toggling (Super+J) use an exact layout subgroup.
  Preserve adjacent sibling sizes; selections across branches may need
  rearrangement. Keep pointer zoom, single-window commands and tab grouping.
- [x] Keep the screenshot save/copy chooser open until answered; allow 30
  seconds for saved-screenshot feedback. Leave visual styling to Claude.
- [x] Show password dots without retaining or drawing the password itself.
- [x] Inspect the previous boot: Linux and the compositor both resumed.
  Clear stale pre-suspend DRM frames and request fresh scanout on activation.
- [x] Robin verified the resume recovery on hardware. Enable Suspend by default
  and retain `general.suspend_enabled = false` as an opt-out. No suspend test
  was run by the agent.

Log, 2026-10-15: integrated the three standalone libraries, added multi-window
actions, layout persistence and idle handling. Checks used a private headless
compositor and private D-Bus; nothing was installed or restarted in the active
desktop. Trackpad and touchscreen improvements were requested for later only.

Follow-up, 2026-10-15: corrected the empty/floating desktop background and
launcher editing. The isolated formation check moved a vertical group with a
horizontal lower pair intact; selecting and closing all four windows left an
empty desktop. The native caret and full wallpaper were inspected visually.
All 64 existing tests and formatting/Clippy checks pass. Password dots and the
resume recovery were compiled but no lock or suspend was triggered. Robin's
custom config now includes group/float shortcuts and disables automatic idle
locking. Lock and notification visual design remains Claude's task.

Follow-up, 2026-10-15: Robin reports that suspend/resume works and the lock screen
shows password dots. Suspend now defaults to enabled, with a configuration
opt-out. Remaining manual checks are listed above; visual redesign and input
device enhancements remain separate planned work.

Follow-up, 2026-10-15: corrected launcher clipboard handling after Robin reported
phantom splits on Ctrl+C/Ctrl+V. Copy and paste now use compositor selection APIs
without creating helper windows. Manual verification in the desktop remains
pending after installing the updated build.

Follow-up, 2026-10-15 (Claude): restyled the launcher, lock screen,
notifications, power menu and selection marks after `docs/design.html`, through a
shared painter (`ui.rs`) with cached line icons, keycaps, text weights and
wrapping, and short fade/slide animations. Review of the coming-days work fixed:
plain presses in Overview no longer start a box over windows (window dragging
there works again, and a click no longer leaves a stray selection); Shift+click
on a live window reaches the program, with Super+Shift+click for selecting;
screenshot copying uses the new compositor-owned `copy-image` instead of
`wl-copy` (whose helper window got tiled); the ask-first chooser is shown with
Do not disturb on and is not pushed out by newer popups; a relock drops a stale
password check; Caps Lock no longer hides "Incorrect password"; Super+I/Super+U
are bound by default. Checked nested on Xvfb with software rendering: launcher,
lock (with and without wallpaper, typing, checking, wrong password, Caps Lock),
notifications and history, DND, chooser action click, clipboard round trip, and
Super+Shift selection. Not checked on hardware.

Follow-up, 2026-10-15: isolated headless checks with real terminal windows confirm
that opening beside a zoomed tile frames only that pair, selected split toggling
leaves other branches alone, and selection zoom frames an exact pair even across
branches. Existing workspace tests and Clippy pass. Desktop shortcut verification
remains manual after installing the new build.

### Packaging, installation and release delivery

Status: agreed architecture, not implemented. Keep package storage separate from
the website's 100 MB hosting allowance. A CDN caches downloads; the origin storage
must have sufficient capacity and an explicit retention policy.

- [x] Choose native Debian packaging and APT-managed updates and removal.
- [x] Separate `apt.tileception.space` from `tileception.space`: package storage
  can grow independently of the small OVH website host.
- [ ] Define the supported Debian/Ubuntu releases and CPU architectures. Build
  against compatible distribution libraries, declare runtime dependencies, and
  map derivative distributions to their supported base release. Do not assume
  one `.deb` works on every Debian-based system.
- [ ] Add a release CI workflow that builds versioned `.deb` packages, runs the
  required existing checks, generates repository indexes and signed metadata,
  and publishes to the custom APT origin with CDN delivery. Keep private signing
  keys and deployment credentials in protected release secrets, off the public
  hosts; document key backup, expiry monitoring and rotation.
- [ ] Provision HTTPS, DNS and origin storage for `apt.tileception.space`.
  Set storage, transfer and retention budgets; retain previous working packages
  for recovery and document downgrade instructions.
- [ ] Make repository publication safe for concurrent clients: upload immutable
  versioned packages first, then indexes, then signed release metadata. Use
  content-addressed indexes (`by-hash`), retain indexes needed by existing
  clients, serialize publication, and configure CDN caching so mutable metadata
  refreshes promptly. Never overwrite a published package version.
- [ ] Package the compositor and helpers in system directories, the login-screen
  entry, the PAM policy as system configuration, and the desktop-specific portal
  configuration in its system location. Installation must not require Rust or
  build dependencies, alter the user's default desktop, or restart a live session.
- [ ] Define migration from the source installer: identify old per-user binaries,
  session entries and portal overrides that can shadow packaged files. Preserve
  personal configuration and avoid changing unrelated users' home directories
  from package maintainer scripts.
- [x] Build the website in a separate CI deployment and publish to the small OVH
  host, including a plain-text shell script at `https://tileception.space/install`.
  Keep website deployment independent of package publishing. Done in `website/`
  and `.github/workflows/website.yml`. The page already shows the install command;
  until the repository and packages exist, `/install` only prints the source-build
  steps and exits without changing the system.
- [ ] Support `curl -fsSL https://tileception.space/install | sh`. Make the script
  compatible with `/bin/sh`, safe to rerun, and able to detect supported OS/base
  releases and architectures before changing the system. Explain the changes,
  use `sudo` for privileged operations, install the repository-specific public
  key and Deb822 `.sources` configuration with `Signed-By`, then run `apt update`
  and `apt install tileception`. Fail clearly on errors and unsupported systems.
  Provide download-and-inspect instructions alongside the one-line command.
- [ ] Give repository configuration and the public key package ownership, for
  example through `tileception-archive-keyring`, with documented removal and
  key rotation. Never use a globally trusted key or disable signature checking.
- [ ] Document the full lifecycle: choose Tileception at the next login; normal
  system updates take effect at the next Tileception login; switch to another
  desktop before removal. Normal removal keeps personal settings; package purge
  also removes system configuration, including PAM. Provide separate instructions
  for removing the repository and explicitly deleting the current user's data.
- [ ] Verify installation, upgrade, repeat installer execution, source-install
  migration, removal, purge and recovery in disposable supported environments.
  Check the deployed script, repository signatures and CDN-served metadata before
  announcing a release. Do not exercise these flows against the active desktop.

Log, 2026-10-15: agreed two independent CI delivery paths: build and publish Debian
packages to a custom APT host with CDN-backed storage; build and publish the website
and `/install` script to the small OVH host. Provider selection, supported release
matrix, credentials and deployment implementation remain pending.

### Maybe someday (tm)

- More monitors: a layout per screen, moving windows between them.
- HiDPI and fractional scaling.
- Window rules (always float an app, open an app in a given view).
- Window lists for other tools (foreign-toplevel) and clipboard history
  (data-control).
- Bindings that repeat while held, pass through to the app, or work while
  locked; click-to-kill.
- Pointer lock for games and input methods.
- Better trackpad support: configurable multi-finger actions, swipes and
  pinches for navigation, Overview and window management. Planning only for now.
- Touchscreen support: touch currently does not work on Robin's laptop,
  although the same hardware works in PikaOS. Investigate device discovery,
  coordinate mapping and touch delivery, then add direct manipulation and
  gestures. Planning only for now.
- Live blur and glass styles (Smoke, Liquid glass from `design/graphite.html`).
- A settings window instead of editing the config file.

### To be planned

- [ ] **Swappable shell components.** Add documented configuration to disable or
  replace each built-in tool independently, including the Super menu/launcher,
  notification bar and popups, power menu and lock screen. External replacements
  must be supported as permanent choices, with configurable shortcuts and
  startup commands. Release shared protocol or service ownership when a built-in
  component is disabled so it does not compete with its replacement. Preserve
  the compositor's window-management features when using external tools.
- [ ] **Built-in volume and media controls with visual feedback.** Currently these
  are Robin's personal `wpctl`/`playerctl` shortcuts, without compositor feedback.
  Add built-in actions and standard hardware-key bindings so they work with the
  default configuration. Show actual volume and mute state after changes, and
  playback feedback for play/pause, stop and track changes, including the active
  player and track when available. Report missing audio devices or active
  players clearly. Use the Tide visual language; keep personal shell shortcuts
  unnecessary for normal volume/media-key operation. Planned, not implemented.
- **Minimizing windows.** Hide a window without closing it, and bring it back
  either where it was or at a new spot (e.g. dropped onto a tile's edge or
  centre, like a drag). Decided:
  - While a window is minimized, its neighbours take its space (as when it
    closes); restoring it "where it was" docks it back beside the same
    neighbour.
  - A saved view keeps counting a minimized window as part of it: going to that
    view restores the window into it. Going to a view never minimizes
    anything; windows that are out stay out.
- **Seeing minimized windows.** A taskbar-like strip that hides itself and
  slides in when the pointer touches the screen edge, showing minimized
  windows (icon, name, maybe a small preview); click to restore, drag to place.
  Depends on text rendering and the Tide components.
- **A welcome screen instead of nothing.** When no windows are open (right
  after login, or after closing the last one), show the built-in launcher in a
  "home" form on the empty desktop: app search, maybe recent or pinned apps and
  the time. Typing searches right away; Esc dismisses it to a plain desktop.
  Depends on the built-in launcher. (Robin's current startup error comes from
  espanso running its X11 build; switching it to the Wayland build is separate.)
- **A "doctor" you can talk to (ACP).** A built-in Tide panel that talks to a
  coding agent such as Claude or Codex over the Agent Client Protocol, so you
  can ask it to change settings, shortcuts and default apps in plain language.
  The agent gets narrow tools: read and validate the config, propose an edit,
  apply it after you confirm (with a backup), run `tileception msg` commands
  such as `tree` and `reload`, and read the log. Depends on text rendering and
  a text input in the Tide components; needs a careful permission model so the
  agent can only touch Tileception's own files unless you allow more.

## 5. Risks and open questions

- **Client resize latency**: some clients (Electron, Xwayland) commit slowly after a
  configure. Windows are drawn stretched until they catch up.
- **Exact feel** (drop-band sizes, spring constants, hold delay) is not recoverable
  from strings. A decompiler pass (Ghidra) on `SpacesEngine` symbols such as
  `DropResolver` and `Feel` can recover these later if wanted.
- **Persistence matching**: Wayland gives `app_id` and title but no stable window id
  across restarts, so rematching is a best-effort heuristic.
- **Built-in lock screen**: password checking through PAM must be right; a stuck
  lock needs the console escape (`tileception msg unlock`).

## 6. Testing approach

- `tileception-core`: unit tests for tree invariants (including random edit
  sequences), layout, camera, navigation, drops, floats and resizing.
- `tileception`: unit tests for config and commands, and scripted runs of the
  headless backend with screenshots. Tests never touch the live session: only
  an instance started for the test, through the socket from its own log.
