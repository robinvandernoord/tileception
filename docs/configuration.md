# Configuration

Edit `~/.config/tileception/config.toml`, or
`$XDG_CONFIG_HOME/tileception/config.toml` if you set `XDG_CONFIG_HOME`.
Tileception creates a commented config on first run and reloads changes when
you save it.

The examples below are partial configs. Merge them into the matching sections
of your file instead of adding duplicate section headings. The
[default configuration](../crates/tileception/src/default-config.toml) lists all
available settings.

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

Colours accept `#rrggbb` or `#rrggbbaa`. `accent` and `accent_end` are the two
ends of the accent gradient. Set them to the same colour for a solid accent.

Use an existing PNG, JPEG or WebP image for the wallpaper. It also appears on
the built-in lock screen. `wallpaper_fit` accepts `fill`, `fit`, `stretch` or
`center`.

Set `theme.colour_scheme` to `"dark"` or `"light"` to tell applications which
appearance you prefer, or `"default"` for no preference. The compositor's own
colours are controlled by the other theme settings.

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

Only one monitor is used for now; additional monitors stay inactive.

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

`shell.media` accepts only `"builtin"` or `"none"`. To replace the media controls,
set it to `"none"` and bind volume, brightness and playback keys to your own
commands under `[binds]`.

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

Saving this stops Tileception's notification service. Log out and back in to
start swaync through autostart. Super+N then opens swaync's interface.

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

All matching rules apply in file order, with later values overriding earlier
ones. Editing a rule affects newly opened windows.

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

Use XKB layout, variant and option names. `model` is also accepted if you need
to specify a keyboard model. The example's repeat settings are the defaults:
wait 400 milliseconds before repeating, then repeat 30 times per second.

Repeat settings apply when you save. Changes to `layout`, `variant`, `options`
or `model` currently require logging out of Tileception and back in, or
restarting a nested instance.

### Touchpad

These are the defaults:

```toml
[touchpad]
tap = true
natural_scroll = false
disable_while_typing = true
speed = 0
swipe_distance = 60
```

With tap-to-click enabled, one finger clicks left, two click right and three
click middle. Set `natural_scroll = true` to move content in the direction your
fingers move. `disable_while_typing` suppresses touchpad input while typing.

`speed` accepts values from -1 to 1; positive values make the pointer faster.
`swipe_distance` is the distance in pixels before a swipe binding runs and must
be greater than zero.

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
idle inhibition, such as video players, can pause both timers. These idle timers
do not run in nested mode.

Unless `shell.lock` is `"none"`, `lock_before_sleep` locks before suspend or
hibernation, including sleep requested by closing the lid or by another program.
Set it to `false` to disable that behaviour.

Suspend is available in the power menu by default. To disable that menu action:

```toml
[general]
suspend_enabled = false
```

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

## Doctor

Doctor's agent, model and permission mode are managed in its panel, separately
from `config.toml`. See the [Doctor guide](doctor.md) for setup and permissions.
