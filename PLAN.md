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
  idle handling with video inhibition.
- Tide theming, wallpaper, text rendering, HiDPI and fractional scaling.
- Built-in shell: launcher (calculator, conversions, windows, commands, app
  actions, times, colours, characters, paths, `>` commands), welcome screen,
  top bar with breadcrumb and calendar, notifications with do not disturb,
  power menu, lock screen, volume/media/brightness controls with audio device
  switching, minimizing with the bottom strip. All swappable through `[shell]`.
- The doctor: an ACP agent panel (Super+/) with ask, config and yolo modes.
- Touchpad: swipe and pinch bindings in `[binds]` (`Swipe3Left`,
  `Super+Pinch2In`), unbound gestures passed to programs (pointer-gestures),
  `[touchpad]` for tap, natural scrolling, typing, speed and click method.
- Touchscreen: touches on windows go to programs (wl_touch); on
  Tileception's own interface a finger acts as the left button.
- Window rules, per-app launch arguments, ext-foreign-toplevel-list,
  data-control, pointer constraints and relative pointer.
- Config hot reload, IPC, headless backend with screenshots for testing.
- Packaging: release CI for trixie, sid, noble and resolute (amd64 and arm64),
  signed APT repository, `tileception-archive-keyring`, `/install` script.

## Roadmap

### Known bugs

- A second foot opened beside a first crashes: it gets keyboard focus
  (`wl_keyboard.enter`) before its first configure.

### Next

- More monitors: a layout per screen, moving windows between them. Comes first
  once Robin can test it on a second screen.

### Later

- Touchscreen gestures for the camera (pinch to step in and out, swipes
  between spaces) and long-press to move windows.
- Touchpad gestures that follow the fingers: the camera tracking a pinch or
  swipe live instead of one step per gesture, and window management gestures.
- Live blur and glass styles.
- A settings window instead of editing the config file.
- Top bar extras: a minimap (live outline of the tree with the camera's frame;
  click to fly, drag to pan), `position = "bottom"` moving the minimized strip
  to the top, calendar events from an .ics file or URL.
- Slow-resizing clients (Electron, Xwayland) are drawn stretched until they
  commit the new size; crop or pad instead, as niri does. Only if it shows up
  in practice.

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
