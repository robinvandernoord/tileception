# Using Tileception

These are the default controls. Your [configuration](../README.md#configuration)
can change them. Super is usually the Windows key; in a nested window, use Alt
in its place for Super shortcuts.

## The layout

The first tiled window fills the available space. Opening another splits the
focused tile along its longer side. Each split can contain more splits, so your
layout becomes a tree of windows and groups.

For example, keep a browser and notes in one group, and an editor, preview and
terminal in another. Zoom into the development group while working, then zoom
out to see both groups again. The windows stay in the same layout as you move
between views.

## Settings

Settings keeps unapplied drafts while you move between categories. Back returns
to the category and keeps the screen's draft, including unfinished input. Apply
changes saves and applies it in one step; Discard screen explicitly removes that
screen's edits since opening it.

Closing Settings with unapplied changes asks first: **Apply** saves them and
closes, **Discard** closes without them, and **Cancel** (or Escape) keeps
Settings open. Enter chooses Apply. Closing keeps no draft, so Settings always
opens on `config.toml` as it is then.

While Settings is open it follows `config.toml`: when the file changes on disk,
settings you have not touched show the new values. A setting or screen you have
edited keeps your edit, and Settings says so when the file changed it too;
applying then saves your version over the file's.

In Settings, use **Ctrl+Page Up/Down** to switch categories and **Alt+Up** to
go Back. **Tab** and **Shift+Tab** move between controls, **Page Up/Down**
scroll the page, and **Ctrl+S** applies the draft. On compact screens the
category sidebar scrolls independently; move the pointer over it to scroll.

### Display Settings

Open **Displays** to arrange screens, choose resolution and refresh rate,
rotate or mirror a screen, change its scale, or split it into workspace areas.
Disabled connected screens stay in the list so you can enable them again.
At least one connected screen must remain enabled. Rotation labels follow
Wayland's counterclockwise convention; mirroring happens before rotation.

**Apply changes** previews display changes for 15 seconds without saving the
configuration. Choose **Keep changes** to save, or **Revert** to restore the
previous setup. Escape, Alt+Up, closing Settings and the countdown ending also
restore it. Your draft stays available to retry, unless you close Settings and
discard it. A display change or config
reload cancels an active preview. If saving fails, the preview reverts and the
error appears in Settings.

## Moving around

| Shortcut                | Action                                               |
|-------------------------|------------------------------------------------------|
| Super+Arrow keys        | Focus a neighbouring window or group                 |
| Alt+Tab / Alt+Shift+Tab | Cycle through windows forward / backward             |
| Super+= / Super+I       | Zoom one level in towards the focused window         |
| Super+- / Super+U       | Zoom one level out                                   |
| Super+Scroll up / down  | Zoom in towards the pointer / zoom out               |
| Super+O                 | Toggle the overview                                  |
| Super+F                 | Toggle app fullscreen; the app may hide its controls |
| Super+M                 | Toggle zoom to the focused window; keep app controls |
| Super+[ / Super+]       | Go back / forward through previous views             |

Each zoom step moves one level through the tree.

The overview shows the whole layout. Click a window to zoom to it, or press
Escape to leave the overview and return to your previous view.

## Saved views

Use slots 1 through 9 to return to useful parts of the layout:

1. Zoom to the window or group you want to remember.
2. Press **Super+Shift+1** to save that view in slot 1.
3. Move elsewhere, then press **Super+1** to return.

Use another number for another slot. Saving to an occupied slot replaces its
previous view. Saved slots also appear in the bar; click one to return to it.

Saved views refer to windows in the running session. Recalling one also restores
any minimized windows that belong to it. On a split screen, a slot holds the view
of every area: recalling it restores each area that has a view saved there.

## Minimap

The minimap in the bar is a small outline of the whole layout, with the part on
screen outlined. Hover it to open a larger map, or click it to keep the map open
until you click it again, click elsewhere or press **Escape**. On the map:

- Click a window to zoom to it.
- Drag to pan to another part of the layout at the current zoom level.

The line below the map says what letting go would show. On a split screen the
map shows every area side by side, and choosing something in another area makes
that area active. You can also open the map by typing **Minimap** in the
launcher. [Configuration](configuration.md#desktop-components) shows how to hide
it.

## Split screens

A wide screen can be [split into areas](configuration.md#splitting-a-wide-screen),
each with its own layout. Shortcuts act on the active area, except Super+Scroll,
which acts on the area under the pointer and makes it active. Click or touch
another area to make it active; moving the pointer over it lets its windows
scroll and show hover effects without taking the keyboard.

- Drag the gap between two areas to change their widths. Resizing a window on
  the side where its area meets the next one moves that gap too.
- Alt-drag a window into another area to move it there. A floating window
  dropped there lands wholly inside it; floating windows also shrink and move
  to stay inside their area when it narrows.
- **Super+M** zooms to the focused window within its area. **Super+F** covers
  the whole monitor.
- The overview and the minimized-window strip belong to the active area.

## Managing windows

### Selecting several windows

Focus one window, then **Super+Shift+click** others to add or remove them from
the selection. You can also hold **Super+Shift** and drag a selection box, or
drag from empty space in the overview. Press **Escape** to clear the selection.

Move, close, minimize and float commands apply to the selection when one exists.
Otherwise, they act on the focused window.

With tiled windows selected, **Super+I** zooms to exactly that selection, and
**Super+J** switches just those windows between rows and columns. If the
selected windows are in different groups, they are first gathered into a new
group where the first selected window was. Use **Super+U** to zoom back out, or
**Escape** to clear the selection.

### Shortcuts

| Shortcut                    | Action                                                         |
|-----------------------------|----------------------------------------------------------------|
| Super+Shift+Arrow keys      | Move windows in the chosen direction                           |
| Super+Alt+Arrow keys        | Resize the focused window in 40-pixel steps                    |
| Super+J                     | Switch the selection or current split between rows and columns |
| Super+G                     | Combine selected windows into tabs                             |
| Super+Tab / Super+Shift+Tab | Switch to the next / previous tab                              |
| Super+V / Super+Shift+F     | Switch windows between floating and tiled                      |
| Super+D                     | Clear the desk, or bring back what it cleared                  |
| Super+H                     | Minimize windows                                               |
| Super+Shift+H               | Restore the most recently minimized window                     |
| Super+C                     | Close windows                                                  |

### Moving and resizing

Hold **Alt** and drag a window with the left mouse button to move it. The drop
preview shows where it will go:

- Drop at a tile's edge to place the window beside it.
- Drop at a tile's centre to combine the windows into tabs.
- Drop at the edge of the current view to place the window beside that whole
  group.

Drag a gap between tiles to resize their split. **Alt+right-drag** resizes a
window from the corner nearest the pointer. You can change Alt to another
modifier with `general.drag_modifier` in the config.

In the overview, drag windows directly without holding Alt.

### Tabs and floating windows

Click a tab or use **Super+Tab** and **Super+Shift+Tab** to switch between windows
in a tile. Drag a tab to another tile's edge to separate it into its own tile.

**Super+V** switches a window between floating above the layout and being tiled.
Floating windows can be moved and resized with the same Alt-drag controls.

### Clearing the desk

**Super+D** clears the desk of the screen: every window there, tiled and
floating, goes to the minimized-window strip. On a split screen this clears
every area, each into its own strip, so the welcome screen and its dashboard
show. Press it again to bring them back exactly where they were. Windows you had minimized before stay
minimized, and windows closed in the meantime are left out.

While the desk is clear you can still open windows, or restore single ones
from the strip. While any window is on the desk, **Super+D** clears it, and
these windows join the ones waiting; once the desk is empty, **Super+D**
brings them all back together. Windows you minimize yourself with **Super+H**
stay minimized.

Hold **Super+D** instead of tapping it to peek: windows fade to outlines over
the dashboard for as long as you hold it, and come back when you let go. The desk is not
cleared. A hold lasts past the keyboard's repeat delay.

### The dashboard

When the desk is clear or empty, the welcome screen shows a dashboard beside
it: how many notifications there are (click to open them) and what
[plugins](plugins.md#dashboard) put there, such as cards, pictures and
animations. Holding **Super+D** fades the windows to outlines and shows the
dashboard behind them. The dashboard needs room beside the welcome screen; on a narrow screen it does not
show. Tap **Super** to put the welcome screen and dashboard away for a plain
desktop, and tap it again to bring them back.

Drag an item on the welcome screen's dashboard to put it anywhere beside the
welcome screen. It snaps to an 8-pixel grid, and an item dropped on another
moves to the nearest free place; the others stay where they are. Drag a
picture or plugin surface by its title, anything else from anywhere but its
buttons. Where the items are is saved in
`~/.local/state/tileception/dashboard-layout.toml`, from the nearest corner of
the screen so they keep their place on a screen of another size; delete it to
go back to the first-come layout.

To remove an item, point at it and click the round **x** on its corner. It
stays away, also after a restart, until you bring it back with the **+** beside
the settings button in the welcome screen's footer. The **+** shows while there
is something to add: it lists the items you removed, and installed plugins with
dashboard items that are switched off. Choosing a plugin there switches its
dashboard items on in `config.toml`, and the plugin too when it was off.

`tileception msg dashboard` lists the items by name (`notifications`, or a
plugin's id and the item's, such as `timers/tea`) and says which are removed;
`tileception msg dashboard hide <item>` removes one, and
`tileception msg dashboard show <item>` (or `show all`) brings them back.
Removed items are kept in the same file, so deleting it brings them back too.

### Minimized windows

Minimizing removes a window from the visible layout while leaving its application
running. Move the pointer to the bottom edge of the screen to reveal the
minimized-window strip. If the bar is also at the bottom, the strip appears
just above it.

Click a preview to restore the window to its previous place, or drag it out to
choose a new position. **Super+Shift+H** restores the most recently minimized
window.

## Desktop controls

| Shortcut      | Action                                |
|---------------|---------------------------------------|
| Super         | Open the launcher                     |
| Super+T       | Open a terminal                       |
| Super+B       | Open the default browser              |
| Super+E       | Open your home folder                 |
| Super+N       | Open notifications and sound controls |
| Super+Enter   | Answer a popup's buttons by keyboard  |
| Ctrl+Print    | Pick a colour from the screen         |
| Super+/       | Open Doctor                           |
| Super+L       | Lock the screen                       |
| Super+X       | Open the power menu                   |
| Super+Shift+E | End the Tileception session           |

### Launcher and notifications

Press **Super**, type an application or window name, and press **Enter** to
open the selected result. Use the arrow keys to choose another result and
**Escape** to close the launcher.

The launcher also handles calculations, unit conversions, file paths and URLs.
Prefix a shell command with `>` to run it and see its output.

Conversions work offline for length, area, volume, mass, temperature, time,
speed, data, energy, power, pressure, angle and frequency. Write them as
`5 ft in cm`, `convert 100 f to c` or `how many ml in a cup`. Amounts can come
in parts, as in `5 ft 10 in in cm` or `1 h 30 min in s`, and so can results:
`180 cm in ft and in` shows `5 ft 10.87 in`. **Enter** copies the result.

Press **Alt+Enter** on an application to see its desktop entry: the command it
runs (including `[launch_args]`), the `.desktop` file it comes from and
whether that is a system, Flatpak or Snap entry or your own. From there you can
copy either, open the file in your text editor, or **Edit a copy**: a copy in
`~/.local/share/applications` takes precedence over the system entry, so
your changes survive package updates. **Escape** goes back to the results.

Type `z` and a space, then parts of a folder's path, to list the folders you
visit most, as [z](https://github.com/rupa/z) and
[zoxide](https://github.com/ajeetdsouza/zoxide) know them: `z proj web` finds
`~/projects/website`. **Enter** opens the folder in the file manager,
**Ctrl+Enter** opens a terminal there and **Tab** completes its path. Folders
from typed paths work the same way.

Type `timer` and a space, then how long and what for: `timer 10m tea`,
`timer 1h30m bread`, `timer 90s` or `timer 2:30 eggs`. A bare number is
minutes, and the name is optional and may come first. **Enter** starts the
timer. With `at` and a time of day it is an alarm: `timer at 15:30 call the
dentist`, `timer at 7am` or `timer at noon`. A time such as `at 3`, without am
or pm, is the next three o'clock, morning or afternoon; write `03:00` or
`15:00` to say which.

Plain sentences work too, without `timer` in front: "set a timer for 10
minutes", "10 minute timer for tea", "set an alarm at 7", "remind me at 3 to
call the dentist", "remind me to stretch in 20 minutes" or "wake me up at
6:30". The first row says what Enter will start and when it rings.

Each timer shows as a card under the launcher whenever it is open, with the
time left, a bar for how far it is, and buttons to add a minute, pause or
resume, and stop. `timer` and a space also lists the timers, where **Enter**
offers the same actions. When the time is up, a notification says so and stays
until you dismiss it; the card stays for ten minutes, or until its tick is
clicked. An alarm or reminder rings with its name, and has no pause. Timers keep running while Tileception restarts, and one that ended
meanwhile rings as soon as it is back. There can be 16 timers of up to a day;
the launcher shows the cards of the four that end first.

Scripts and key bindings use the same tools:

```sh
tileception msg plugin timers start '{"duration": "25m", "label": "focus"}'
tileception msg plugin timers start '{"at": "17:00", "label": "leave"}'
tileception msg plugin timers list
tileception msg plugin timers pause '{"timer": "focus"}'
tileception msg plugin timers stop '{"timer": "focus"}'
```

`[plugins.timers] enabled = false` switches timers off.

"Send feedback" and `tileception msg feedback [title]` open editable title and
feedback fields. Enter adds a line in the feedback field; the usual selection,
word navigation and clipboard shortcuts work. Choose **Review** to see the
exact title and issue body, including Tileception's version.
"Include system details" starts unchecked. Checking it adds OS/version,
architecture, backend and output pixel sizes/scales to the preview.
"Include a screenshot of the screen" starts unchecked too. Checking it takes a
screenshot of the screen under the dialog, without the dialog, and shows a
preview of it with its size; unchecking it removes the screenshot again. A
GitHub link cannot carry a picture, so **Open in browser** saves the
screenshot as `tileception-feedback-screenshot-<uid>.png` in the temporary
folder (usually `/tmp`, readable only by you, replacing the last one) and puts
it on the clipboard, and the issue body gets a hidden note where it belongs:
press **Ctrl+V** there to attach it, or drag the file into the issue. What you
had copied (up to 32 MB) comes back to the clipboard once the screenshot is
pasted, or after two minutes (only the latter with clipboard persistence on,
whose helper serves the paste), unless you copy something else first; another
machine's clipboard shared through handoff stays replaced. Nothing is copied
or saved before you open the issue.
Doctor's `/report` opens the same dialog over Doctor. The title is optional:
`/report` alone starts in the empty title field, `/report Wi-Fi drops` fills
it in and starts in the feedback field. The dialog has one more option,
"Include the last Doctor question and answer", also unchecked. Checking it
adds your last question and the agent's answer, shortened if long. The
screenshot leaves the Doctor panel out unless this option is checked, since the
panel shows more than the last exchange; changing the option takes the
screenshot again.
**Tab** and **Shift+Tab** move between controls; arrow and page keys scroll.
**Edit** or **Escape** returns from the preview to your draft; **Cancel** or
Escape while editing closes the dialog. **Open in browser** opens the
prefilled GitHub issue, where you can edit and submit it using your GitHub
account. Tileception does not submit the issue. Anonymous feedback remains
deferred.

Popups never take the keyboard by themselves. To answer one with buttons, such
as the screenshot's Copy, Save and Save and copy, press **Super+Enter** (the
popup shows the hint): its buttons get numbers. Press a number, or choose with
the arrow keys or **Tab** and press **Enter**. **Escape**, or **Super+Enter**
again, gives the keyboard back without answering.

**Super+N** opens notification history and sound controls, and your smart home
devices once [set up](#smart-home). Use the panel's **Do not disturb** control
to silence ordinary notification popups.

The panel takes the keyboard when it opens, and again when you click in it; a
click elsewhere gives the keyboard back to your windows. While it has the
keyboard:

| Key                         | Does                                           |
| --------------------------- | ---------------------------------------------- |
| **Delete** or **C**         | Clear all                                      |
| **Up** / **Down**           | Scroll the history by one                      |
| **Page Up** / **Page Down** | Scroll by five                                 |
| **Home** / **End**          | Jump to the newest or the oldest               |
| **Tab** / **Shift+Tab**     | Choose the next or previous notification       |
| **Enter**                   | Open the chosen notification, as a click would |
| **Backspace**               | Dismiss the chosen notification                |
| **D**                       | Switch Do not disturb                          |
| **M**                       | Mute or unmute the sound                       |
| **Escape**                  | Close the panel                                |

Super shortcuts, and keys that do not type such as volume and media keys, keep
working while the panel has the keyboard.

The panel's actions work from scripts and key bindings too:
`tileception msg notifications clear` does what **Clear all** does, and
`tileception msg do-not-disturb on` (or `off`, or nothing to toggle) switches
Do not disturb.

In nested mode, notifications and media controls are handled by your host
desktop by default.

### System tray

The built-in bar shows application tray icons. Left-click an icon to activate
the application, or right-click for its menu. Some applications use left-click
to open their menu too. Middle-click invokes the application's secondary action,
if it provides one.

When there are more icons than fit, click **…** to see the remaining applications.
Left-click and right-click work in this list too. Scroll longer lists and menus;
click outside to close them.

By default, the bar has three tray slots, including the overflow button. Change
the limit in the [tray settings](configuration.md#system-tray).

### Screenshots

**Print** captures the whole screen; **Shift+Print** lets you select a region.
Both then ask whether to copy the image, save it, or do both. Dismiss the prompt
to discard the capture, or press **Escape** during region selection to cancel.

Saved images go into `Screenshots` inside your configured Pictures folder,
usually `~/Pictures/Screenshots`.

### Colour picker

**Ctrl+Print**, or "Pick a colour" in the launcher, shows a magnifier next to
the pointer. Click, or press **Enter**, to copy the colour under it as hex,
such as `#2acea2`. The arrow keys move a pixel at a time, ten with **Shift**;
**Escape** or a right click cancels. A notification then offers the RGB and
HSL notations and another pick.

The pick follows the pointer onto your other screens and, with handoff, onto
other machines: the colour is picked and copied on the machine the pointer is
on, and handoff shares that clipboard as usual.

### Password manager auto-type

KeePassXC 2.8 and newer can type usernames and passwords into other windows
through the desktop portal. The first time a program wants to type, a
notification asks whether to allow it: **Deny**, **Allow once** or **Always**.
**Always** is remembered in `~/.local/state/tileception/typing.toml`; remove the
program's line there to be asked again. Keys go to the
window you are using, and nothing is typed while the screen is locked.

For KeePassXC's global auto-type shortcut, open its settings, go to
**Auto-Type** and click **Configure** next to the global shortcut. Tileception
asks you to press the keys, such as Ctrl+Alt+A; Escape or **Skip** leaves the
shortcut unset. The keys are remembered for that program, and clicking
**Configure** again changes them. Keys Tileception already uses are refused.
On Wayland, KeePassXC cannot see window titles, so the shortcut always opens its
entry picker: search, press Enter, and it types into the window you came from.

The portal reads Tileception's portal files when it starts. After installing or
updating Tileception, log out and back in before trying auto-type.

### Sound, brightness and playback

The standard volume, microphone mute, brightness and media keys work with the
built-in controls. These alternatives are available without dedicated keys:

| Shortcut                       | Action                   |
|--------------------------------|--------------------------|
| Super+Ctrl+Up / Down           | Raise / lower volume     |
| Super+Ctrl+M                   | Mute or unmute sound     |
| Super+Ctrl+Space               | Play or pause media      |
| Super+Ctrl+Left / Right        | Previous / next track    |
| Super+Ctrl+Page Up / Page Down | Raise / lower brightness |

### Smart home

With [smart home devices](configuration.md#smart-home) set up, the notification
panel has a **Home** block. An air conditioner shows its room temperature, a
target you change with **-** and **+**, its mode and fan speed, and a trend
line of the last six hours of temperature and target underneath. Below it are
the lights, plugs and sensors by room, each light and plug with a switch, and
each sensor with a small trend line of its last six hours. Temperature is drawn
in the accent colour and humidity in the second accent colour, each on its own
scale. For a device that reports both, **Settings**, **Smart home**,
**History** chooses temperature, humidity or both. Click a light to show its
brightness and colour sliders. **All** lists everything in one list instead,
with what is on first, and **All off** switches off every light and plug.

The block folds to one line while nothing is on and unfolds when something
turns on; click its title to fold or unfold it yourself, or a room's name to
fold that room. Devices that are switched off at the wall show as unreachable
at the bottom of their room. While something is on, a chip in the bar says
what; click it to open the block.

In the launcher, type `home` and a space to switch devices and rooms:
`home living` switches a room, `home airco 21` sets the air conditioner and
`home desk 40` dims a light.

`tileception msg home` lists the devices, `tileception msg home why <name>`
explains why one is unreachable, and commands such as
`tileception msg home toggle Couch` or `tileception msg home brightness Couch 40`
control them from scripts and key bindings; `tileception msg --help` lists them
all.

### Bluetooth

The notification panel's **Bluetooth** block lists paired devices with a switch
to connect each, each with a symbol for its kind (headphones, headset, speaker,
mouse, keyboard, game controller, phone, computer, display or camera) when the
device says what it is. **Pair device** searches for 30 seconds; flip a nearby
device's switch to pair it, and confirm the code when asked. Type `bt` in the
launcher to connect, disconnect or (`bt forget`) unpair devices.

### Arranging the panel's blocks

Drag a block such as Home or Bluetooth by its title to move it above or below
the others; a line shows where it will go. Tileception remembers the order and
shows the bar's chips in the same order. See [the plugin guide](plugins.md#panel-block)
for where it is kept.

### Updates

On systems that use APT (Ubuntu, Debian, Mint), a chip appears in the bar
while updates wait: APT packages, Flatpaks, snaps and firmware from fwupd. It counts security updates first: `3 +9` means three
security updates and nine others. While updates install it shows progress,
and it says **Restart** when earlier updates need one or names a newer Ubuntu
release once one is out. With nothing to do there is no chip.

Click the chip for a card with the updates by kind and three buttons:
**Install all**, **Security only** and **Review**. Installing needs an
administrator: polkit asks for the password in Tileception's prompt and
remembers it for a few minutes. See [Administrator rights](#administrator-rights).

Tileception refreshes the package lists by itself every few hours, which
needs no password. It leaves that on a metered connection and below 30% on
battery; **Check** still works then.

**Review** opens the Updates window, which also opens with
`tileception msg updates open` or "Updates" in the launcher. It has five pages;
**Tab** and **Shift+Tab** switch between them, the arrow keys move through the
list, **Space** ticks a row and **Enter** runs the page's main button.

| Page      | What it does                                                                                                                                                                                                                                                                                                                                 |
| --------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Available | Lists every update with its versions and download size: one row per source package, with its description and the packages built from it, then the apps and the firmware. Untick the ones to leave out, then **Install**. **Preview** shows first what `apt-get` would do with the ticked APT updates. **Check** refreshes the package lists. |
| Kernels   | Does what [kerntop](https://github.com/robinvandernoord/kerntop) does. See below.                                                                                                                                                                                                                                                            |
| Release   | Says whether a newer Ubuntu release is out and what to look at first: disk space, pending updates, package sources that the upgrade switches off, mains power and a snapshot. **Upgrade** runs `do-release-upgrade` in a terminal, where it asks its questions.                                                                              |
| History   | What apt did before, from its log. **Ask Doctor** sends the packages of one entry to Doctor; **Copy list** copies them. **Last task log** opens everything the last task printed.                                                                                                                                                            |
| Settings  | Automatic updates, and the Ubuntu mirror. See below.                                                                                                                                                                                                                                                                                         |

Flatpaks of the system and of your own installation, and snaps, are listed
under **Apps**. Asking for them uses the network, so they are listed when the
package lists are refreshed and when the window opens, not on a metered
connection or low battery; in between the last list stays. Firmware from
fwupd is listed under **Firmware** and is never ticked by itself: a firmware
update is installed only when you tick it, and some devices finish it while
the computer restarts. One **Install** installs the APT updates first, then
the Flatpaks, the snaps and the firmware, each with its own tool.

The strip under the list acts on the highlighted row. **Notes** shows what
changed: the changelog entries since the installed version for APT
(`apt-get changelog`, which asks the archive), the commit messages of a
Flatpak's remote, what `snap info` says, or the release notes of firmware.
Press **N** for the same. **Skip version** leaves that version out: the row
moves to **Skipped and held**, muted, and comes back when a newer version
arrives. **Hold** keeps an APT package at its version with `apt-mark hold`
until **Release** lets it update again. Skipping is remembered by Tileception
alone; a hold is apt's own and also applies in a terminal.

Below the updates, the Available page says what else installing everything
does: packages it removes, updates apt keeps back, and updates the archive
is still phasing in, which follow later by themselves. When installing
everything would remove packages, **Install all** in the card and **Install**
in the window show the preview first; nothing is removed unseen.

Under **Housekeeping** the page offers the dependencies that nothing needs any
more. **Review** shows what `apt-get autoremove` would take, and **Remove**
there does it.

While packages are installed or removed the computer stays on: restarting,
switching off, suspending and logging out wait for the task. Choose one in the
power menu and it follows by itself once the task ends; choose it again to
call it off. If the task fails, it is called off. Where systemd's logind runs,
other programs asking to restart or sleep are held off for as long too.

Everything the last task printed is kept, also across a restart of
Tileception. After a failure the window offers **Show log**, and **Repair**
when apt says the package system needs one. Repair shows what
`apt-get --fix-broken install` would do, then runs `dpkg --configure -a`
followed by that.

The Kernels page lists kernels by series; click a series, or press **Space**
on it, to fold it. It shows every installed kernel and every kernel apt offers
for the flavour you are running, with unsigned and debug builds left out.
**All variants** adds the other flavours and those builds, with only the series
you have kernels of unfolded.

The running kernel is protected and the newest other one is marked as the
fallback. **Install** and **Remove** put a kernel on the queue instead of acting
at once. Installing takes the matching headers along; removing takes the
modules and headers that nothing else then uses. **Preview** shows what
`apt-get` would do with the whole queue, **Apply queue** does it as one
transaction and **Clear** empties it. **Purge: on** also removes the
configuration of what goes; switch it off to keep that. **Queue old kernels**
queues everything except the running kernel and the fallback. The page warns
when no fallback is installed, and when the queue would leave none.

Leftover headers, and leftover modules and tools, of kernels that are gone are
listed apart and queued per kind. Nothing on this page runs `autoremove`.

While a kernel or firmware update waits, the power menu shows **Restart and
install** (key **N**) beside **Restart**: it installs everything that waits,
firmware included, and restarts once that has succeeded.

The Settings page turns automatic updates on and off. Tileception does not
schedule anything itself: the switch turns the periodic run of
`unattended-upgrades` on or off in `/etc/apt/apt.conf.d/20auto-upgrades`, and
the row says what it covers, such as security updates or Ubuntu Pro. On
Linux Mint it switches Update Manager's automatic upgrades timer instead,
preserving its system blacklist, and covers what Update Manager installs: every update
but its blacklist, or fewer when `/etc/mintupdate-automatic-upgrades.conf`
narrows it. Where neither is installed, the row says so.

Below that, the page shows the Ubuntu mirror in use. **Test**, in the row
below the mirror, asks the mirror in use, the main archive and the mirrors Ubuntu lists for your country
for a small file and part of a larger one, and lists them fastest first with
their speed, how long they took to answer and how far behind the main archive
they are. **Use** switches to one: it rewrites the Ubuntu entries of the
package sources, leaving the security pocket alone, and refreshes the lists.
On Linux Mint these are in `official-package-repositories.list`, beside Mint's
own mirror, which stays as it is;
when that refresh fails, the old sources are put back. Once a day the mirror
in use is tested by itself, and the page and the window's sidebar warn when it
cannot be reached or is a day or more behind.

| Key         | Action on the Kernels page                             |
| ----------- | ------------------------------------------------------ |
| A           | Switch between the recommended view and all variants   |
| Q, or Space | Queue the highlighted kernel, or take it off the queue |
| I / D       | Queue the highlighted kernel to install / to remove    |
| U           | Tick or untick the leftovers                           |
| P           | Preview the queue, and go back                         |
| C           | Clear the queue                                        |
| R           | Refresh the package lists                              |
| Enter       | Apply the queue                                        |

Type `up` in the launcher for the same actions as rows. The first row always
opens Updates; below it are install everything, install the security updates,
clean up old kernels, upgrade to the next release and check now.

From scripts and key bindings:

| Command                                              | Action                                                         |
| ---------------------------------------------------- | -------------------------------------------------------------- |
| `tileception msg updates`                            | List what is waiting                                           |
| `tileception msg updates check`                      | Refresh the package lists                                      |
| `tileception msg updates install`                    | Install every update                                           |
| `tileception msg updates install security`           | Install the security updates                                   |
| `tileception msg updates install <package>...`       | Install the named updates                                      |
| `tileception msg updates kernels`                    | List kernels: running, fallback, old, available                |
| `tileception msg updates clean`                      | Remove old kernels and leftover kernel packages                |
| `tileception msg updates autoremove`                 | Remove packages that are no longer needed                      |
| `tileception msg updates repair`                     | Repair the package system, when apt asks for it                |
| `tileception msg updates log`                        | Print what the last task printed                               |
| `tileception msg updates upgrade`                    | Start the release upgrade in a terminal                        |
| `tileception msg updates changelog <name>`           | Show what changed in an update                                 |
| `tileception msg updates hold <package>...`          | Hold APT packages at their version; `unhold` releases them     |
| `tileception msg updates skip <name>`                | Skip the waiting version of an update; `unskip` brings it back |
| `tileception msg updates automatic [on\|off]`        | Say how automatic updates are set up, or switch them           |
| `tileception msg updates mirrors`                    | Test the mirrors; ask again for the results                    |
| `tileception msg updates mirror <url>`               | Use this mirror                                                |
| `tileception msg updates history [since YYYY-MM-DD]` | What apt changed, with versions, newest first                  |

Where two sources have an update by the same name, such as a Flatpak in both
the system and your own installation, or a package and a snap, name it with
its source in front: `apt:`, `flatpak:`, `flatpak-user:`, `snap:` or
`firmware:`, as in `tileception msg updates skip snap:firefox`. A name alone
then says which to choose from.

Doctor reads the history the same way, so it can answer "what changed since
Tuesday?".

#### Administrator rights

Changes to packages are made by a small helper,
`/usr/libexec/tileception-updates-helper`, that polkit starts as root. It
takes a task such as "install these updates", never a command line, and
checks every package, Flatpak, snap, device and mirror address. Its policy has two actions: refreshing the package
lists is allowed without a password for the person at the computer, and
everything else asks for an administrator. To change who may do what, add a
polkit rule for `org.tileception.updates.refresh` or
`org.tileception.updates.change`.

Tileception is the session's polkit agent, so the same prompt answers other
programs that ask polkit for an administrator, such as `pkexec`. The password
goes to polkit's own checker and nowhere else. To use another agent, set
`[shell] polkit = "none"` and start that agent yourself.

Where polkit or an agent is missing, the helper runs through `sudo` instead,
and Tileception's prompt passes what you type to `sudo` for that one task.

Flatpaks of your own installation are updated as you, without the helper.
See [Configuration](configuration.md#updates) to switch the chip or its
notification off, or to choose when the notification comes.

### Network

With NetworkManager running, a Wi-Fi or cable icon in the bar shows how you
are connected, with the signal strength; hover it for the network's name.
Click it, or type **Network** in the launcher, to open the network card:

- The switch at the top turns Wi-Fi on or off.
- Click a network to join it. Saved and open networks join at once; for a
  secured one, type its password and press **Enter**. **Show** reveals what you
  typed; copying works only while it shows.
- **Disconnect** leaves the network you are on.

While the card is open it has the keyboard: **Up** and **Down** choose a
network, **Enter** joins it (or leaves the one you are on), and **Escape** goes
back from the password or closes the card. If a password turns out wrong, the
card asks for it again. **Network settings** at the bottom of the card opens
the Network page in Settings.

The Network page lists the same networks with a **Join** button each, and holds
what the card leaves out. It acts on NetworkManager at once rather than as part
of the Settings draft:

- The connections that are up, with their addresses, gateway, DNS servers,
  interface and hardware address.
- Saved Wi-Fi networks. The switch decides whether one joins by itself when in
  range; **Forget**, clicked twice, removes it with its password.
- Joining a network by name: a secured one chosen above asks for its password
  here, and a name that is not in the list joins as a hidden network. Leave the
  password empty for an open network.
- **Open editor** starts `nm-connection-editor`, when it is installed, for
  static addresses, VPNs and enterprise Wi-Fi.

`tileception msg network status` prints the same information, and
`tileception msg network wifi off` switches Wi-Fi off from a script or binding.

## Gestures

### Touchpad

| Gesture                         | Action                                         |
|---------------------------------|------------------------------------------------|
| Three-finger swipe left / right | Focus the window or group to the right / left  |
| Three-finger swipe up / down    | Focus the window or group below / above        |
| Three-finger pinch in / out     | Zoom one level out / in                        |
| Four-finger swipe up / down     | Show the overview / zoom to the focused window |
| Four-finger swipe left / right  | Go forward / back through previous views       |

Swipes move the layout with your fingers, so swiping left brings the window on
the right into focus.

You can also hold **Super** and pinch with two fingers to zoom out or in.
Gestures without a Tileception binding are passed to the application, so an
ordinary two-finger pinch can zoom a web page.

### Touchscreen

Swipe in from the top edge for the overview, the bottom for minimized windows,
or the right for notifications. Three-finger swipes and pinches navigate like
their touchpad equivalents.

In the overview, a two-finger pinch out zooms into a tile; pinching in leaves
the overview. Ordinary touches on application windows are passed to the app.

The [default configuration](../crates/tileception/src/default-config.toml) lists
all gesture bindings and touchpad options.

## Doctor

**Super+/** opens an optional coding-agent panel for questions and changes to
your desktop setup. See the [Doctor guide](doctor.md) for agent setup, sign-in,
permissions and image attachments.
