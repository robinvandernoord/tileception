# Tileception for Linux

A Wayland compositor in Rust (Smithay) that reimplements the window model of
TrellisWM for macOS: windows never overlap, they live in a nested tree of
tiles, and you zoom between levels of that tree. See `PLAN.md` for the
findings and the roadmap.

## Status

- Runs as your login session (DRM/KMS, libinput, libseat) or nested in a
  window on another desktop for development.
- Xwayland: X11 programs are arranged like everything else, with the
  clipboard shared both ways.
- Nested tiling: new windows split the focused tile along its longer side.
- Zooming: step in and out of the tree, Overview of everything, neighbouring
  spaces, framing one window on its own, all animated.
- Rearranging: drag windows onto edges (dock) or centres (tab) of tiles, move
  windows with the keyboard, drag gaps to resize.
- Tab groups with titles and app icons above the tile.
- History (back and forward) and saved views 1 to 9.
- Layer shell: bars reserve their edge, launchers get the keyboard.
- Fullscreen for Wayland and X11 programs (and Super+F).
- Screen locking (ext-session-lock, for swaylock) and screen capture
  (wlr-screencopy, for grim, slurp and screen sharing through
  xdg-desktop-portal-wlr).
- Modifier taps: Super on its own opens the launcher.
- A config file with key bindings, reloaded on save.
- A command socket: `tileception msg <command>`.

Not yet: more than one monitor (only the first is lit), trackpad gestures,
touchscreen support, HiDPI scaling,
taskbar window lists (foreign-toplevel), clipboard managers (data-control).

## Companion programs

The screenshot helpers use these; on Debian and Ubuntu:

```sh
sudo apt install grim slurp \
    xdg-desktop-portal-wlr wl-clipboard
```

The launcher, notifications and power menu are built in. Print and Shift+Print
save screenshots to `~/Pictures/Screenshots`. The screenshot save/copy chooser
waits for a decision; saved-screenshot feedback remains available for 30 seconds.

## Installing as a session

```sh
./install-session.sh
```

This builds a release binary, installs `tileception` and `tileception-session` into
`~/.local/bin`, and (with sudo) adds a "Tileception" entry to the login screen.
Log out and pick Tileception in the session menu.

To try it without the login screen first: switch to a free console
(Ctrl+Alt+F3), log in, and run `~/.local/bin/tileception-session`. Ctrl+Alt+F2
(or whichever F-key your desktop is on) switches back while it runs.

Getting out: Super+Shift+E quits, Ctrl+Alt+BackSpace always quits, and
Ctrl+Alt+F1..F12 switch consoles. The log is
`~/.local/state/tileception/tileception.log` (the previous run's is `.log.old`).

## Running nested or headless

```sh
cargo run -p tileception -- -- lxterminal
```

Inside another desktop Tileception opens a window. There, Alt stands in for
Super, because the host desktop keeps Super for itself.

`tileception --headless` runs without any screen, rendering on the GPU's render
node only when asked for a capture. Programs can still connect to it, so it
is handy for scripted tests next to a running session:

```sh
tileception --headless &
export TILECEPTION_SOCKET=$(ls -t $XDG_RUNTIME_DIR/tileception-wayland-*.sock | head -1)
tileception msg terminal
tileception msg screenshot /tmp/shot.png
```

## Configuration

`~/.config/tileception/config.toml` is written with the defaults on first run
and reloaded whenever it is saved. It holds the gap, animation length, drag
modifier, terminal, keyboard layout, autostart commands and the key
bindings; the file documents every option. Bindings map a key combination
to a command:

```toml
[binds]
"Super+T" = "terminal"
"Super+Return" = "spawn firefox"
"Super+1" = "go 1"
"Super+Shift+1" = "save 1"
```

## Look

Tileception's look is **Tide**: flat graphite, small radii, a teal to blue
accent with a soft glow on the focused window (reference in `docs/design.html`). The
`[theme]` section of the config sets the colours, corner radius, glow, font,
and a `wallpaper` (with `wallpaper_fit`), which the lock screen shows too.

The lock screen checks the logged-in user's password through PAM on a worker
thread. Escape or Ctrl+U clears the input; Enter submits it. Caps Lock is shown,
and compose/dead keys follow your locale. A custom `[general].lock_command`
still overrides the built-in locker. The session installer installs the PAM
service. If recovery is needed, switch to a console and run
`tileception msg unlock` as the same user.

The launcher has an editable search field with a caret, mouse selection,
Left/Right, Home/End, Shift selection, Ctrl+A, clipboard shortcuts, and
Alt/Ctrl+Left/Right word navigation. Up/Down select search results. Usage is
saved under `$XDG_STATE_HOME/tileception` (default `~/.local/state/tileception`).

Automatic layout saving/restoration is disabled. The experimental app-matching
implementation requires `[general].restore_layout = true`; its intended behavior
is deferred pending scope clarification.

Notifications run inside the compositor, with popups, thumbnails, actions and
a scrollable history panel. The panel's DND control toggles do not disturb;
critical notifications still appear. Session startup ignores the old `swaync`
autostart entry. The old default `spawn fuzzel` and `spawn swaync-client -t`
bindings are translated to their built-in equivalents when loading the config.
New custom bindings should use `launcher`, `notifications`, `do-not-disturb`
and `group` directly.

`[idle]` defaults to `lock_after = 0` and `off_after = 600`, in seconds;
zero disables an action. Visible players using Wayland idle inhibition prevent
both actions. The display-off timer powers down the monitor, not the computer.
Input wakes the display; the wake event is consumed. Automatic locking and the
power menu's Suspend action are disabled while lock/resume failures are being
investigated. The resume path clears stale queued DRM frames, but that recovery
has not been verified on hardware.
Nested development sessions do not automatically lock or power down the host.

## Default shortcuts

| Keys | Action |
| --- | --- |
| Super (tap) | built-in app launcher |
| Super+T | open a terminal beside the focused window |
| Super+B | open the system's default web browser |
| Super+E | open the home folder in the system's default file manager |
| Super+L | built-in password lock screen with your wallpaper |
| Super+N | notification history and do-not-disturb control |
| Super+X | built-in power menu; Suspend is currently unavailable |
| Print / Shift+Print | screenshot of the screen / of a region |
| Super+C | close the focused or selected windows |
| Super+G | group selected windows into tabs |
| Super+O | Overview (Esc or a click leaves it) |
| Super+I / Super+U | step in toward the focused window / step out a level |
| Super+WheelUp / Super+WheelDown | step in toward the pointer / step out |
| Super+M | frame the focused window on its own (again: back) |
| Super+[ / Super+] | back / forward through framed spaces |
| Super+Arrows | neighbouring space |
| Super+Shift+Arrows | move the focused or selected windows that way |
| Super+Alt+Arrows | resize the focused window by 40 pixels |
| Super+J | stack the focused row, or lay the column side by side |
| Alt+Tab / Alt+Shift+Tab | focus the next / previous window |
| Super+Tab / Super+Shift+Tab | next / previous tab |
| Super+F | fullscreen |
| Super+V | float above the layout, or dock again |
| Super+Shift+F | float or dock the selected windows together |
| Super+D | show or hide the Desktop tile |
| Super+1..9 / Super+Shift+1..9 | go to / save a view |
| Super+Shift+E | quit |

Mouse:

- Click a window to focus it. In Overview, click a tile to go in.
- Alt+drag a docked window, or drag its title bar: drop near a tile's edge
  to dock there, on its middle to make it a tab, on the Desktop tile to
  float, or at the very edge of the screen to dock beside everything.
- Alt+drag a floating window to move it anywhere.
- Alt+right-drag a window to resize it from the nearest corner.
- Shift+click toggles window selection. Super+Shift+drag draws a selection box;
  in Overview, plain drag draws the box. A plain click clears the selection.
- Drag a tab out of its strip to undock it.
- Drag the gap between windows to resize.

## Commands

`tileception msg <command>` talks to the running compositor (inside it,
`TILECEPTION_SOCKET` is set). Bindings use the same commands: `terminal`,
`spawn <command line>`, `close`, `overview`, `step-in [pointer]`,
`step-out`, `focus <dir>`, `move <dir>`, `resize <dir> <px>`,
`toggle-split`, `cycle [next|prev]`, `back`, `forward`, `maximize`, `desktop`,
`float`, `fullscreen`, `next-tab`, `prev-tab`, `save <n>`, `go <n>`, `vt <n>`,
`drop <x> <y>`, `screenshot <file.png>`, `launcher`, `notifications`,
`do-not-disturb`, `select [window-id ...]`, `group`, `lock`, `power-menu`,
`tree`, `reload`, `quit`. `select` without ids clears the selection; window
ids and selection/lock/display-sleep status are included in `tree`.

## Layout

- `crates/tileception-core`: the layout engine (tree, camera, navigation, drops).
  No platform code; `cargo test -p tileception-core`.
- `crates/tileception`: the compositor binary.
- `resources/`: the session launcher and login screen entry.
