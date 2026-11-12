# Configuration

Edit `~/.config/tileception/config.toml`, or
`$XDG_CONFIG_HOME/tileception/config.toml` if you set `XDG_CONFIG_HOME`.
Tileception creates a commented config on first run and reloads changes when
you save it.

The examples below are partial configs. Merge them into the matching sections
of your file instead of adding duplicate section headings. The
[default configuration](../crates/tileception/src/default-config.toml) lists all
non-plugin settings, including defaults and what leaving optional values unset
means. Commented examples may use different values from the defaults.
[Plugins](plugins.md) documents plugin settings separately.

## Shortcuts

Bindings map key combinations to the same commands used by `tileception msg`:

```toml
[binds]
"Super+Return" = "terminal"
"Super+H" = "none"
"Super+Shift+R" = ["reload", "spawn notify-send 'Tileception' 'Config reloaded'"]
```

This adds another terminal shortcut, disables Super+H, and binds Super+Shift+R
to reload the config and show a notification. Lists of commands run in order.

Modifiers are `Super`, `Alt`, `Ctrl` and `Shift`. Keys use XKB names such as
`Return`, `Left`, `bracketleft` and `Page_Up`. Mouse-wheel bindings use `WheelUp`,
`WheelDown`, `WheelLeft` or `WheelRight`. Super on its own is also a valid binding.
In a nested window, Alt stands in for Super in your bindings too.

Your bindings add to or override the defaults. To use only the bindings listed
in your file, set:

```toml
[general]
default_binds = false
```

Run `tileception msg --help` for the available commands. Use `spawn` before a
shell command, such as `"spawn firefox --private-window"`.

## Appearance

Layout spacing and animation timing live under `[general]`; colours, fonts and
wallpaper live under `[theme]`:

```toml
[general]
gap = 8
animation_ms = 200

[theme]
accent = "#e5b567"
accent_end = "#d4945f"
font_size = 14
wallpaper = "~/Pictures/wallpaper.jpg"
wallpaper_fit = "fill"
```

`gap` sets the spacing between windows and around the screen edge.
`animation_ms` sets the duration of zoom and move animations in milliseconds.
At `0`, the lock screen holds still too, as with the login screen's
[`reduce_motion`](greeter.md#settings).

Colours accept `#rrggbb` or `#rrggbbaa`. Buttons, switches and other
filled controls use `accent`. Thin lines, such as the shown tab's underline, and
the focused window's border run from `accent` to `accent_end`. Set both to the
same colour for a solid accent.
`guest` (violet by default) borders windows that come from another computer
through handoff, brighter and glowing when they have the keyboard.

Use an existing PNG, JPEG or WebP image for the wallpaper. It also appears on
the built-in lock screen. `wallpaper_fit` accepts `fill`, `fit`, `stretch` or
`center`.

Set `theme.colour_scheme` to `"dark"` or `"light"` to tell applications which
appearance you prefer, or `"default"` for no preference. The compositor's own
colours are controlled by the other theme settings.

Set `theme.frost = true` for frosted panels: the bar, launcher, notifications,
power menu, cards and lock screen turn translucent and blur what is behind
them. Windows stay solid. The login screen has its own `[theme]` in
[its settings](greeter.md#settings), where `frost = true` does the same. Frost
shows best over a colourful wallpaper. It costs a little GPU time when
something changes behind a panel; a still screen costs nothing extra.

### Fastfetch logo

In a Tileception session, [fastfetch](https://github.com/fastfetch-cli/fastfetch)
shows Tileception's tiles instead of your distribution's logo. The session adds
a fastfetch config installed with Tileception to `XDG_CONFIG_DIRS`, so nothing
is written to your home configuration and other desktops keep the usual logo.
A config of your own in `~/.config/fastfetch` takes precedence; to keep the
tiles there, copy the `logo` block from
`/usr/share/tileception/xdg/fastfetch/config.jsonc` (or
`~/.local/share/tileception/xdg/...` for a source installation).

## Windows larger than their tiles

A window whose minimum size exceeds its tile is scaled down to fit. Once the
camera settles, it remains interactive when its scale is at least
`general.min_live_scale`:

```toml
[general]
min_live_scale = 0.5
```

The default is `0.5`; finite values from `0.1` through `1.0` are accepted.
Lower values keep smaller windows interactive. Below the threshold, and while
the camera moves or Overview is open, windows use previews without direct
pointer input. Display scaling is a separate setting.

The legacy `general.sole_float_takes_desktop` option is still accepted so old
configs load, but is ignored. Its stored default is `true`; floating windows
always hang above the layout.

## Display scaling

The default `"auto"` scale is chosen from the screen's physical size and
resolution. To set a fixed scale:

```toml
[outputs."*"]
scale = 2
```

`"*"` applies to any output without its own entry. To configure a particular
connector, use its name from the session log, for example:

```toml
[outputs."eDP-1"]
scale = 2
```

Scales from 0.5 to 4 are accepted. Larger values make text and interface elements
bigger. Use `scale = "auto"` to return to automatic selection.

Some applications may lag at non-integer scales such as 1.25 or 1.5. Try 1 or 2
if that happens. X11 applications running through Xwayland are enlarged from
scale 1 and may look less sharp.

For a temporary override when starting Tileception, set `TILECEPTION_SCALE`:

```sh
TILECEPTION_SCALE=2 tileception --nested
```

This overrides the output settings for that instance.

## Display modes, rotation and arrangement

Configure each monitor by its connector name from `tileception msg outputs`:

```toml
[outputs."DP-2"]
enabled = true
transform = "90"
mode = "1920x1080@59.94"
scale = "auto"
position = [0, 0]
```

`enabled` defaults to `true`. `transform` defaults to `"normal"` and accepts
`"90"`, `"180"`, `"270"`, `"flipped"`, `"flipped-90"`, `"flipped-180"` and
`"flipped-270"`. `mode` defaults to `"preferred"`; explicit modes use
`WIDTHxHEIGHT@HZ`, with refresh rate in hertz. Physical monitors must advertise
the requested mode. Unsupported modes are rejected; startup falls back to a
working configuration when the requested configuration cannot be applied.

`position` is the top-left corner in global logical pixels. Without explicit
positions, enabled monitors form a horizontal row using their rotated sizes
and scales. Disabled connected monitors remain available for re-enabling.
Disabling every connected monitor is rejected.

Display changes made through Settings preview at runtime before saving.
Choose **Keep changes** within 15 seconds to persist them, or **Revert** to
restore the previous configuration. An expired preview or a failed save also
restores the previous runtime state. Reloading configuration or changing the
connected monitor set invalidates a pending preview. Nested mode explicitly
rejects mode, rotation and enable/disable changes it cannot support.

### Full-edge pointer crossing

To map the whole edge proportionally when the pointer crosses to an adjacent
screen, including between screens of different sizes:

```toml
[general]
stretch_screen_edges = true
```

The default is `false`, which uses the screens' positions in the desktop
arrangement. Settings calls this **Full-edge pointer crossing**. It applies
when you save and does not require a special build.

### Touchscreen mapping

Map an exact touchscreen device name from **Settings > Displays** to an output
connector from `tileception msg outputs`:

```toml
[touchscreens."Example touchscreen"]
output = "eDP-1"
```

There are no mappings by default; unmapped touches use the currently active
screen. An explicit mapping activates that output for touch input. If its
screen is disconnected, touches from that device are ignored until it returns.
Mappings apply when you save.

## Splitting a wide screen

On an ultrawide monitor, one layout across the whole width can be awkward:
zooming to one window leaves it very wide. Split the screen into side-by-side
areas instead. Each area has its own windows, zoom, overview, minimized windows
and selection, as if it were a screen of its own:

```toml
[outputs."DP-2"]
split = 2
```

`split` takes a number of equal areas, from 1 to 8, or their relative widths,
such as `split = [0.6, 0.4]` or `split = [2, 1]`. Use the connector name from
`tileception msg outputs`; `"*"` cannot be split. Areas are named from the
left, such as `DP-2.1` and `DP-2.2`, in `tileception msg outputs`,
`focus-output` and `move-output`.

Applications still see one screen, so screen sharing, screenshots and external
lockers and bars cover the whole monitor. There is one built-in bar across it.

Reducing the number of areas combines their windows side by side in the last
area that remains, each keeping its layout. Setting `split = 1` or removing it
puts everything back into one layout. Widths dragged by hand (see
[Using Tileception](usage.md#split-screens)) last until Tileception restarts or
`split` is changed; they are not written to the config.

While any screen is split, `general.restore_layout` does not save or restore
layouts.

## Desktop components

Each `[shell]` entry uses `"builtin"` for Tileception's own component or `"none"`
to disable it. The launcher, bar, notifications, power menu and lock screen also
accept a replacement command. Install replacement programs separately.

```toml
[shell]
launcher = "fuzzel"
bar = "waybar"
power_menu = "wlogout"
lock = "swaylock -f"
```

The launcher, power menu and lock commands run when invoked by their usual
shortcuts or actions. The bar starts automatically; changing its setting stops
the old bar and starts the new one. Do not also start that bar through autostart.
Set an entry back to `"builtin"` to restore Tileception's version.

The built-in bar is at the top by default. To move it to the bottom:

```toml
[shell]
bar_position = "bottom"
```

This applies when you save the config. The minimized-window strip stays at the
bottom, above the bar, and bar menus and previews open upwards. Use `"top"` to
restore the default.
External bars such as Waybar use their own position settings.

The built-in bar shows a small minimap of the whole layout next to the back and
forward buttons; [Using Tileception](usage.md#minimap) describes it. To hide it:

```toml
[shell]
bar_minimap = false
```

With NetworkManager running, the bar also shows a network chip that opens a
card for joining Wi-Fi networks ([Using Tileception](usage.md#network)). Settings
has the same switch on its Network page. To hide it:

```toml
[shell]
bar_network = false
```

Tileception answers polkit when a program needs an administrator's password,
as installing updates does ([Using Tileception](usage.md#administrator-rights)).
`shell.polkit` accepts only `"builtin"` or `"none"`; with `"none"`, start
another polkit agent through autostart.

`shell.media` accepts only `"builtin"` or `"none"`. To replace the media controls,
set it to `"none"` and bind volume, brightness and playback keys to your own
commands under `[binds]`.

### Plugins

Plugins are executable MCP servers over stdio that add launcher sources and
cards under the launcher, panel blocks and bar chips. The smart home, Bluetooth,
`z` folders and timers are bundled plugins: `[plugins.bluetooth] enabled = false`
switches one off, `[plugins.home.bar] enabled = false` hides its bar chip, and
`[plugins.<id>.launcher] cards = false` hides a plugin's cards under the
launcher. Build the example with
`cargo plane build -p tileception --bin tileception-launcher-example`, then
enable it using the executable's absolute path:

```toml
[plugins.example]
command = "/absolute/path/to/tileception-launcher-example"
[plugins.example.launcher]
# prefixes = ["echo"]
```

Type `echo hello` in the launcher to try it; a prefix needs the space after it. Omit `prefixes` to use those
reported by the plugin; set `enabled = false` to disable a source. Plugins
installed through manifests are also discovered in
`~/.local/share/tileception/plugins/` and `/usr/share/tileception/plugins/`.
Configuration overrides user manifests, which override system manifests.
Only install plugins you trust, as with autostart commands. See
[Plugins](plugins.md) for installation, actions, diagnostics and the
protocol.

### System tray

Set the number of tray slots in the built-in bar:

```toml
[shell]
tray_max_icons = 3
```

The default is 3 and the minimum is 1. If there are more applications than
slots, the final slot becomes an overflow button. For example, with five
applications and a limit of 3, the bar shows two icons and **…** for the other
three. Changes apply when you save the config.

See [System tray](usage.md#system-tray) for icon actions and overflow controls.

### Notifications

A replacement notification service needs both a daemon and a command to open
its interface. For example, with swaync installed:

```toml
[general]
autostart = ["swaync"]

[shell]
notifications = "swaync-client -t"
```

Saving this stops Tileception's notification service. Log out and back in (or
run `tileception msg restart`) to start swaync through autostart. Super+N then opens swaync's interface.

With `notifications = "none"`, Tileception's notification service is disabled,
but Super+N still opens the built-in sound controls.

## Window rules

Rules decide how newly opened windows are placed. Each `[[rules]]` entry adds
another rule:

```toml
[[rules]]
app_id = "pavucontrol"
float = true
size = [720, 480]

[[rules]]
title = "*Picture-in-Picture*"
float = true
```

`app_id` matches the Wayland app ID or X11 window class; `title` matches the
window title. If both are present, both must match. Patterns ignore case and
accept `*` for any sequence of characters and `?` for one character.

Use `tileception msg tree` to find application IDs for open windows.
`float = true` makes a matching window float; `false` makes it tiled. `size`
sets a floating window's width and height. You can also set `fullscreen = true`
or `minimized = true` to open a window in that state.

Every rule field is unset by default. A rule must specify `app_id` or `title`;
leaving both unset is rejected. Unset `float` and `size` leave the application's
request unchanged unless an earlier matching rule sets them. Unset `fullscreen`
and `minimized` keep earlier matching rules' values; otherwise both are false.
Set either explicitly to `false` to override an earlier matching rule.

All matching rules apply in file order, with later values overriding earlier
ones. Editing a rule affects newly opened windows.

## Clipboard

Keep copied content available after the source application closes:

```toml
[clipboard]
persist = true
```

The default is `false`. Session installs and Debian packages include a pinned
[wl-clip-persist](https://github.com/Linus789/wl-clip-persist) helper as
`tileception-clip-persist`, next to the compositor. `make build` and `make nested`
build it too; a bare Cargo build can use a separately installed `wl-clip-persist`
on `PATH` instead.
Tileception starts or stops its own helper when you save the config, including
in nested and headless instances. It stops the helper on exit or restart.
Do not also start it through autostart when using this setting.

Only the latest regular copy/paste clipboard is held in memory. This adds no
history or disk storage and leaves the middle-click selection alone. Copies
marked as passwords (offering `x-kde-passwordManagerHint`, as password managers
and sensitive plugin copies do) are not held. Switching
persistence off discards the helper's retained clipboard. If the helper cannot
start or exits, Tileception logs an error; reload the config after resolving
the problem to retry.

## Autostart

Use `general.autostart` for shell commands to run once when Tileception starts:

```toml
[general]
autostart = ["firefox"]
```

Add commands to your existing list if it already starts other programs, such as
the notification daemon above. Saving changes does not run the list again;
they take effect the next time Tileception starts. These commands also run in
nested instances.

Desktop sessions also run enabled XDG autostart entries, normally from
`~/.config/autostart` and `/etc/xdg/autostart`. User entries take precedence over
system entries with the same filename. To disable a system entry, put a file
with the same name in your user autostart directory and set `Hidden=true` under
`[Desktop Entry]`.

XDG autostart entries are skipped in nested instances. Avoid adding a program
to `general.autostart` if an XDG entry already starts it.

## Keyboard and touchpad

### Keyboard

The default layout is US international with AltGr accents (`us`, `altgr-intl`),
so apostrophes and quotation marks type immediately instead of acting as dead
keys. For a plain US layout with Right Alt as a Compose key:

```toml
[keyboard]
layout = "us"
variant = ""
options = "compose:ralt"
repeat_delay = 400
repeat_rate = 30
```

Use XKB layout, variant and option names. Without an explicit layout, `variant`
defaults to `"altgr-intl"`; with a layout, it defaults to `""`. No extra XKB
options are set unless specified. `model` is also accepted if you need to
specify a keyboard model; unset or `model = ""` uses XKB's default model.
The example's repeat settings are the defaults:
wait 400 milliseconds before repeating, then repeat 30 times per second.

Repeat settings apply when you save. Changes to `layout`, `variant`, `options`
or `model` currently require `tileception msg restart` (which closes open
windows), logging out and back in, or restarting a nested instance.

### Touchpad

These are the defaults:

```toml
[touchpad]
tap = true
natural_scroll = false
disable_while_typing = true
speed = 0
swipe_distance = 60
follow_fingers = true
```

With tap-to-click enabled, one finger clicks left, two click right and three
click middle. Set `natural_scroll = true` to move content in the direction your
fingers move. `disable_while_typing` suppresses touchpad input while typing.

`speed` accepts values from -1 to 1; positive values make the pointer faster.
`swipe_distance` is the distance in pixels before a swipe binding runs and must
be greater than zero. Swipes and pinches that move around move the view with
your fingers, and letting go early cancels them; set `follow_fingers = false`
to have them run all at once instead.

For physical clicks, set `click_method = "areas"` to use button areas, or
`click_method = "clickfinger"` to choose the button by finger count. Leaving it
unset uses the touchpad's default. Hardware support determines which options
can be applied.

These touchpad settings apply on save in desktop sessions. In nested mode, the
host desktop controls the physical device settings.

Gesture shortcuts belong under `[binds]`. For example, to open the launcher
with a four-finger upward swipe:

```toml
[binds]
"Swipe4Up" = "launcher"
```

See [Gestures](usage.md#gestures) for the default touchpad and touchscreen
controls and the commented default config for more binding names.

## Power and battery

### Idle timers and locking

The defaults are:

```toml
[idle]
lock_after = 0
off_after = 600
lock_before_sleep = true
```

Times are in seconds; `0` disables that timer. Automatic idle locking is off by
default, and the display turns off after ten minutes. Set `lock_after = 300`,
for example, to lock after five minutes.

`off_after` turns off the display without suspending the computer. The first
input after that, such as a key press, pointer movement or touch, only wakes the
display and is not passed to applications. Visible applications using Wayland
idle inhibition, such as video players, can pause both timers. So can
applications that ask over D-Bus (`org.freedesktop.ScreenSaver`), such as VLC
and other X11 players, whether or not their window is visible. These idle timers
do not run in nested mode.

Unless `shell.lock` is `"none"`, `lock_before_sleep` locks before suspend or
hibernation, including sleep requested by closing the lid or by another program.
Set it to `false` to disable that behaviour.

Suspend is available in the power menu by default. To disable that menu action:

```toml
[general]
suspend_enabled = false
```

### Updates

On systems with APT, the bar shows a chip while updates wait; see
[Updates](usage.md#updates). These are the defaults:

```toml
[updates]
enabled = true
notify = true
check_hours = 4
remind_after_days = 0
remind_security_only = false
```

`enabled = false` switches the update manager off: no chip, no launcher rows
and no `tileception msg updates`. `notify = false` keeps the chip and drops the
one notification when new updates arrive. `check_hours` is how often
Tileception refreshes the package lists, as `apt-get update` does. That
needs no password and is left out on a metered connection and below 30% on
battery; **Check** in the Updates window refreshes them at once.

`remind_after_days` holds the notification back until updates have waited
that many days; `0` says so at once. `remind_security_only = true` notifies
for security updates only. The chip in the bar shows every update either way.
The notification never comes over a fullscreen window or during Do not
disturb: it follows within a minute of leaving those.

### Battery

On computers with a battery, the bar shows its charge; hover for the estimated
time remaining. These features are enabled by default:

```toml
[battery]
border = true
alerts = true
hibernate_at = 5
power_profiles = true
power_saver_at = 20
```

The focus border is green while charging and changes colour at 35%, 25% and
15% on battery. Set `border = false` to keep the normal theme accent, or
customize `charging` and `levels` as shown in the
[default config](../crates/tileception/src/default-config.toml).

Alerts appear at 20%, 15% and 10%; the last remains until dismissed.
`alerts = false` disables them. At `hibernate_at`, Tileception requests
hibernation once per discharge; this requires working system hibernation.
Set `hibernate_at = 0` to disable it.

With power-profiles-daemon available, `power_profiles` selects performance
while plugged in when supported, balanced on battery, and power saver at or
below `power_saver_at`. A manually chosen profile stays until the charger or
low-battery state changes. Set `power_profiles = false` to leave profile
selection to other tools.

These battery actions are inactive on computers without a battery. Run
`tileception msg battery` to inspect the charge and current border colour.

### Bluetooth

Devices that were connected before the computer slept or Bluetooth was switched
off reconnect a few seconds after it is back. Set `reconnect = false` under
`[bluetooth]` to turn this off.

## Smart home

The notification panel can show Tuya lights, plugs and air conditioners (sold
under many brands and used with the Smart Life or Tuya app). They are controlled
on your local network, so they respond quickly and keep working without internet.

Devices behind a gateway, including Zigbee and Bluetooth sub-devices, are not
supported yet and show as unavailable. Gateway support is deferred to a future
update.

Controlling a device locally needs its local key, which only Tuya's cloud hands
out. Fetch the keys once with a free project on the
[Tuya IoT Platform](https://iot.tuya.com): create a Smart Home cloud project in
your app account's data center, and link your app account under **Devices**,
**Link App Account**. Then run:

```sh
tileception home import
```

It asks for the project's Access ID, data center and Access Secret, and saves
your devices with their homes, rooms and local keys to
`~/.config/tileception/home-devices.json`, readable by you only. Battery sensors
only report to the cloud; the
import offers to read those every 10 minutes, which keeps the Access Secret in
the same file. (`--client-id`, `--region` and `--secret-file` make the import
unattended, and `--cloud-sensors`/`--no-cloud-sensors` choose the cloud sensors
without asking.)

The import separately asks whether to refresh devices, names, homes and rooms
at compositor startup. This also needs the saved Access Secret, but works
with cloud-sensor polling switched off. Use `--startup-refresh` or
`--no-startup-refresh` to choose without asking. Startup refresh runs in the
background while smart home is enabled; cached devices remain usable during
refresh and if any request fails. Existing imports with saved credentials
also refresh at startup and keep their sensor polling enabled. Re-run the
import to change either choice, or to update devices manually when startup
refresh is off. A manual refresh action in the settings screen is planned.

Then switch it on:

```toml
[home]
enabled = true
```

`tileception msg home enable` writes `enabled = true`. This starts the bundled
`home` plugin, which shows the panel block, `home` in the launcher and a bar
chip while something is on. It connects again when the device file changes, so
a new import needs no restart.

If your account has several homes, the panel shows the one whose devices answer
on the network you are on. `devices` points to a different device file; see the
[default config](../crates/tileception/src/default-config.toml).
`tileception home status` lists the imported devices and whether they answer,
and `tileception msg home why <name>` explains why one is unreachable and what
to check. The climate widget draws a trend line of the last six hours of room
temperature and target under itself, sampled locally and kept in
`~/.local/state/tileception/home-history.json`.

## Doctor

Doctor's agent, model and permission mode are managed in its panel, separately
from `config.toml`. See the [Doctor guide](doctor.md) for setup and permissions.

Doctor uses an agent already installed on your computer when it finds one. To
run a particular program for an agent, give its command under
`[doctor.agents]`, by registry id: the program, then its arguments. It is run
directly, not through a shell; `~/` at the start of the program is your home
folder.

```toml
[doctor.agents]
opencode = ["~/.opencode/bin/opencode", "acp"]
gemini = ["/usr/local/bin/gemini", "--acp"]
```
