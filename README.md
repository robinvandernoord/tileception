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
- Tab groups with a strip above the tile (no titles drawn yet).
- History (back and forward) and saved views 1 to 9.
- Layer shell: bars reserve their edge, launchers get the keyboard.
- Fullscreen for Wayland and X11 programs (and Super+F).
- Screen locking (ext-session-lock, for swaylock) and screen capture
  (wlr-screencopy, for grim, slurp and screen sharing through
  xdg-desktop-portal-wlr).
- Modifier taps: Super on its own opens the launcher.
- A config file with key bindings, reloaded on save.
- A command socket: `tileception msg <command>`.

Not yet: more than one monitor (only the first is lit), persistence across
restarts, tab titles, touchpad gestures, HiDPI scaling, idle handling,
taskbar window lists (foreign-toplevel), clipboard managers (data-control).

## Companion programs

The defaults call these; on Debian and Ubuntu:

```sh
sudo apt install fuzzel grim slurp swaylock sway-notification-center \
    xdg-desktop-portal-wlr wl-clipboard
```

Super opens fuzzel, Super+L locks with swaylock, Super+X opens the power
menu (wlogout if installed: `sudo apt install wlogout`, otherwise a fuzzel
list), Print and Shift+Print save
screenshots to `~/Pictures`, and `autostart = ["swaync"]` gives
notifications (`swaync-client -t` opens the history panel).

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

## Default shortcuts

| Keys | Action |
| --- | --- |
| Super (tap) | launcher (fuzzel) |
| Super+T | open a terminal beside the focused window |
| Super+L | lock the screen (swaylock) |
| Super+X | power menu: lock, log out, suspend, reboot, power off |
| Print / Shift+Print | screenshot of the screen / of a region |
| Super+Q | close the focused window |
| Super+O | Overview (Esc or a click leaves it) |
| Super+I / Super+U | step in toward the focused window / step out a level |
| Super+WheelUp / Super+WheelDown | step in toward the pointer / step out |
| Super+M | frame the focused window on its own (again: back) |
| Super+[ / Super+] | back / forward through framed spaces |
| Super+Arrows | neighbouring space |
| Super+Shift+Arrows | move the focused window that way |
| Super+Alt+Arrows | resize the focused window by 40 pixels |
| Super+J | stack the focused row, or lay the column side by side |
| Alt+Tab / Alt+Shift+Tab | focus the next / previous window |
| Super+Tab / Super+Shift+Tab | next / previous tab |
| Super+F | fullscreen |
| Super+V | float above the layout, or dock again |
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
- In Overview, drag windows without a modifier.
- Drag a tab out of its strip to undock it.
- Drag the gap between windows to resize.

## Commands

`tileception msg <command>` talks to the running compositor (inside it,
`TILECEPTION_SOCKET` is set). Bindings use the same commands: `terminal`,
`spawn <command line>`, `close`, `overview`, `step-in [pointer]`,
`step-out`, `focus <dir>`, `move <dir>`, `resize <dir> <px>`,
`toggle-split`, `cycle [next|prev]`, `back`, `forward`, `maximize`, `desktop`,
`float`, `fullscreen`, `next-tab`, `prev-tab`, `save <n>`, `go <n>`, `vt <n>`,
`drop <x> <y>`, `screenshot <file.png>`, `tree`, `reload`, `quit`.

## Layout

- `crates/tileception-core`: the layout engine (tree, camera, navigation, drops).
  No platform code; `cargo test -p tileception-core`.
- `crates/tileception`: the compositor binary.
- `resources/`: the session launcher and login screen entry.
