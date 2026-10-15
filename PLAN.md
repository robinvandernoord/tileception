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
| Shell components | Built into the compositor in the Tide style: launcher, notifications, power menu, lock screen. External tools (fuzzel, swaync, wlogout, swaylock) are stopgaps until then. |
| Configuration | One TOML file, `~/.config/tileception/config.toml`, reloaded on save; bindings are key combinations mapped to commands. |
| Licensing | Dropped. No trial, license or update code. |

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

- Run `./install-session.sh` and log in again for the latest fixes: typing after
  launcher and screenshots, lock screen frames and `tileception msg unlock`,
  divider-style resizing, capped program output in the log.
- `sudo apt install playerctl` (media keys) and
  `sudo apt remove libdisplay-info-dev` (no longer used).
- Keep fuzzel, swaylock, sway-notification-center and wlogout until their
  built-in replacements land; then
  `sudo apt remove fuzzel swaylock sway-notification-center wlogout`.

### Today

1. Tide inside Tileception: teal focus outline with glow, a `[theme]` section in
   the config, matching gaps, tile backgrounds and drop previews.
2. Text rendering in the compositor, the base for everything below.
3. Named tab strips: program name and icon per tab, the shown tab underlined in
   teal, a close button.
4. Built-in power menu (Super+X), replacing wlogout and the fuzzel list.

### Coming days

1. Built-in launcher (tap Super), with app search and icons, replacing fuzzel.
2. Built-in notifications: popups and a history panel with do-not-disturb
   (Super+N), replacing swaync. Includes screenshot feedback: saved, thumbnail,
   Copy, Open, Delete.
3. Built-in lock screen (Super+L) with the normal password, replacing swaylock.
4. Selecting several windows: Shift+click and Super+Shift+drag (plain drag in
   Overview); then Super+C closes all, Super+Shift+F floats or docks all,
   Super+G makes them tabs of one tile, Super+Shift+arrows moves them.
5. Layouts that survive a re-login.
6. Idle handling: screen off and lock after a while, not during video.

### Maybe someday (tm)

- More monitors: a layout per screen, moving windows between them.
- HiDPI and fractional scaling.
- Window rules (always float an app, open an app in a given view).
- Window lists for other tools (foreign-toplevel) and clipboard history
  (data-control).
- Bindings that repeat while held, pass through to the app, or work while
  locked; click-to-kill.
- Pointer lock for games, input methods, touchpad gestures.
- Live blur and glass styles (Smoke, Liquid glass from `design/graphite.html`).
- A settings window instead of editing the config file.

### To be planned

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
