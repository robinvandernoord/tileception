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
- Light or dark for applications: the compositor serves the desktop portal's
  appearance settings (dark by default, `colour_scheme` in `[theme]`).
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
waits for a decision and also shows with Do not disturb on; saved-screenshot
feedback remains available for 30 seconds. Copying goes through the compositor
(`tileception msg copy-image`), so no helper window appears.

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

## Debian and Ubuntu packages

On Debian 13 (trixie), Ubuntu 24.04 (noble) or 26.04 (resolute), amd64 or arm64:

```sh
curl -fsSL https://tileception.space/install | sh
```

That adds the apt repository at `https://apt.tileception.space` (signed; the
key goes to `/etc/apt/keyrings/tileception.gpg`) and installs `tileception`.
Updates then come with the normal system updates.

### Releasing

Pushing a tag `vX.Y.Z` that matches the version in `Cargo.toml` runs
`.github/workflows/release.yml`: it builds every release and architecture with
`build-deb-docker.sh`, uploads the packages to the apt repository over SSH and
attaches them to a GitHub release. The repository itself is a separate Docker
stack (tileception-apt). Pull requests run `make lint` and `make test`
(`.github/workflows/ci.yml`).

### Building them yourself

```sh
make deb
sudo apt install ./target/deb/tileception_*.deb
```

`make deb` builds with the `dist` profile (fat LTO, one codegen unit), strips
the binary and puts its debug symbols in a separate `tileception-dbgsym`
package. Install that one too if you want file and line numbers in panic
backtraces and gdb. The package installs into `/usr/bin`, adds the login
screen entry, the PAM service and the portal config, and depends on the
library versions of the system it was built on.

To build the release set, in clean containers (needs Docker):

```sh
make deb-docker
```

That builds amd64 and arm64 packages for Debian 13 (trixie), Ubuntu 24.04 and
Ubuntu 26.04; `DEB_IMAGES` and `DEB_ARCHS` change the selection. arm64 is
cross-compiled, so an amd64 machine builds both. Each release gets its own
`target/deb/<codename>/` with versions like `0.1.0-1~noble`. `make deb`
cross-compiles too with `DEB_ARCH=arm64`, given the cross compiler and the
arm64 build dependencies. Use either the package or `install-session.sh`, not both:
they install the same login screen entry, and the scripts refuse to run while
the package is installed.

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

Scripts can also pretend to use the mouse and keyboard there (headless or
nested only, never in a real session): `tileception msg pointer 640 400`,
`click [x y] [left|right|middle]`, `key Super+slash` and `type some text`.
Keys go through the same handling as real ones.

## Lint policy

Run `make lint` to check formatting and all Clippy targets, including the independent
support-library workspaces. Warnings fail the check.

All crates deny `unsafe_code` by default. Opt in at the smallest practical scope
with `#[allow(unsafe_code, reason = "why this native operation is needed")]`, and
document each unsafe block with a `// SAFETY:` comment explaining its invariants.
Unsafe functions still require explicit unsafe blocks, and unnecessary unsafe
blocks are errors. Clippy requires a `reason` on lint exceptions.

Production code denies `unwrap()` and `unwrap_err()`. Handle recoverable failures;
use `expect("why this must succeed")` for invariants. The message's usefulness is
reviewed by humans. Test modules explicitly allow unwraps for setup and assertions.

The root workspace defines its lints in `Cargo.toml`; the independent `tileception-apps`,
`tileception-auth` and `tileception-notify` manifests carry the same policy.

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
"Super+H" = "none"
```

`[binds]` adds to and overrides the built-in bindings: a key you list
replaces its built-in command, `"none"` unbinds it, and every other built-in
shortcut stays, including ones added in later versions. Set
`default_binds = false` under `[general]` to make `[binds]` the complete list.

The built-in launcher, notifications, power menu, lock screen and volume
controls can each be kept, switched off or replaced under `[shell]`, for
example by fuzzel, swaync, wlogout and swaylock:

```toml
[general]
autostart = ["swaync"]

[shell]
launcher = "fuzzel"
notifications = "swaync-client -t"   # what Super+N runs
power_menu = "wlogout"
lock = "swaylock -f -i {wallpaper}"
```

A replaced notification service gives up its D-Bus name, so the other daemon
can take it, also after a reload without logging out.

## Look

Tileception's look is **Tide**: flat graphite, small radii, a teal to blue
accent with a soft glow on the focused window (reference in `docs/design.html`). The
`[theme]` section of the config sets the colours, corner radius, glow, font,
`danger` (wrong passwords, critical notifications), and a `wallpaper` (with
`wallpaper_fit`), which the lock screen shows too. The launcher, lock screen,
notifications, power menu and selection marks all follow the same reference:
popups on a raised graphite surface with a hairline border and soft shadow,
the accent gradient for whatever is chosen, and keycaps for the keys that work.

The lock screen checks the logged-in user's password through PAM on a worker
thread. It shows the time, date and your initial, one dot per typed character
(never the characters), a pulse while checking, and a shake with a red edge on a
wrong password. Escape or Ctrl+U clears the input; Enter submits it. Caps Lock is shown,
and compose/dead keys follow your locale. A custom `[general].lock_command`
still overrides the built-in locker. The session installer installs the PAM
service. If recovery is needed, switch to a console and run
`tileception msg unlock` as the same user.

The launcher has an editable search field with a caret, mouse selection,
Left/Right, Home/End, Shift selection, Ctrl+A, clipboard shortcuts, and
Alt/Ctrl+Left/Right word navigation. Up/Down select search results. The rest of
the chosen app's name appears greyed out after what you typed, and each result
shows its icon and description. Usage is
saved under `$XDG_STATE_HOME/tileception` (default `~/.local/state/tileception`).

Automatic layout saving/restoration is disabled. The experimental app-matching
implementation requires `[general].restore_layout = true`; its intended behavior
is deferred pending scope clarification.

Opening a tiled window while zoomed into one tile splits inside that view, so
the new window appears beside it without bringing older neighbours back onscreen.
With tiled windows selected, Super+plus (or Super+I) frames exactly the selection,
and Super+J toggles only its horizontal/vertical split. These commands create a
layout subgroup when needed, rather than tabs. Adjacent sibling tiles keep their
sizes; selecting across branches can rearrange the selected windows into their
first tile's position. Super+U steps back out; Escape clears selection. Pointer
zoom keeps targeting the pointer, and Super+G continues to create tabs.

Notifications run inside the compositor. Popups slide in at the bottom right
with the app's icon, how long ago they arrived, up to three lines of text,
pictures such as screenshots at full card width, action buttons, and a bar that
counts down until they close. A question still waiting for an answer stays in
the corner. The history panel (Super+N) is scrollable and has a Do not disturb
switch; critical notifications still appear with it on. Session startup ignores the old `swaync`
autostart entry. The old default `spawn fuzzel` and `spawn swaync-client -t`
bindings are translated to their built-in equivalents when loading the config.
New custom bindings should use `launcher`, `notifications`, `do-not-disturb`
and `group` directly.

`[idle]` defaults to `lock_after = 0` and `off_after = 600`, in seconds;
zero disables an action. Visible players using Wayland idle inhibition prevent
both actions. The display-off timer powers down the monitor, not the computer.
Input wakes the display; the wake event is consumed. Automatic locking remains
disabled by default. Suspend is enabled by default following a successful manual
hardware test of the resume recovery, which clears stale queued DRM frames.
Nested development sessions do not automatically lock or power down the host.

Super+X, then Suspend, uses the normal lock-and-suspend path. To disable the
action, set `suspend_enabled = false` under `[general]` in your config. The config
reloads automatically.

## Default shortcuts

| Keys | Action |
| --- | --- |
| Super (tap) | built-in app launcher |
| Super+T | open a terminal beside the focused window |
| Super+B | open the system's default web browser |
| Super+E | open the home folder in the system's default file manager |
| Super+L | built-in password lock screen with your wallpaper |
| Super+N | notification history and do-not-disturb control |
| Super+X | built-in power menu, including Suspend |
| Print / Shift+Print | screenshot of the screen / of a region |
| Super+C | close the focused or selected windows |
| Super+G | group selected windows into tabs |
| Super+O | Overview (Esc or a click leaves it) |
| Super+I / Super+U | frame the selection or step in toward focus / step out a level |
| Super+WheelUp / Super+WheelDown | step in toward the pointer / step out |
| Super+M | frame the focused window on its own (again: back) |
| Super+H / Super+Shift+H | minimize the focused or selected windows / restore the last one |
| Super+[ / Super+] | back / forward through framed spaces |
| Super+Arrows | neighbouring space |
| Super+Shift+Arrows | move the focused or selected windows that way |
| Super+Alt+Arrows | resize the focused window by 40 pixels |
| Super+J | toggle the selection's split, or the focused row/column |
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
- Super+Shift+click toggles window selection and Super+Shift+drag draws a
  selection box. Shift+click on a tab or an Overview preview toggles it too;
  on a live window Shift+click goes to the program (to extend a text selection).
  In Overview, dragging from empty space draws a box and dragging a window moves
  it. A plain click clears the selection.
- Drag a tab out of its strip to undock it.
- Drag the gap between windows to resize.

## Commands

`tileception msg <command>` talks to the running compositor (inside it,
`TILECEPTION_SOCKET` is set). Bindings use the same commands: `terminal`,
`spawn <command line>`, `close`, `overview`, `step-in [pointer]`,
`step-out`, `focus <dir>`, `move <dir>`, `resize <dir> <px>`,
`toggle-split`, `cycle [next|prev]`, `back`, `forward`, `maximize`, `minimize`,
`restore [window-id]`, `desktop`,
`float`, `fullscreen`, `next-tab`, `prev-tab`, `save <n>`, `go <n>`, `vt <n>`,
`drop <x> <y>`, `screenshot <file.png>`, `copy-image <file.png>`, `launcher`, `notifications`,
`do-not-disturb`, `select [window-id ...]`, `group`, `lock`, `power-menu`,
`tree`, `reload`, `quit`. `select` without ids clears the selection; window
ids and selection/lock/display-sleep status are included in `tree`.

## Layout

- `crates/tileception-core`: the layout engine (tree, camera, navigation, drops).
  No platform code; `cargo test -p tileception-core`.
- `crates/tileception`: the compositor binary.
- `resources/`: the session launcher and login screen entry.
