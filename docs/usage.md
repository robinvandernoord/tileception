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

Settings keeps unapplied drafts while you move between categories or close and
reopen its window. Back returns to the category and keeps the screen's draft,
including unfinished input. Apply changes saves and applies it in one step;
Discard screen explicitly removes that screen's edits since opening it.
Drafts last for the current session and are not saved across a compositor restart.

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
restore it. Your draft stays available to retry. A display change or config
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
- Alt-drag a window into another area to move it there.
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
| Super+D                     | Show or hide the Desktop tile                                  |
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

### The Desktop tile

**Super+D** shows or hides the Desktop tile, a space in the layout for floating
windows. Drag a tiled window onto it to make that window floating, or drag a
floating window to another tile's edge to make it tiled again.

The Desktop tile is also visible when there are no tiled windows. Floating
windows remain available when the tile is hidden.

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

"Send feedback" and `tileception msg feedback [title]` open editable title and
feedback fields. Enter adds a line in the feedback field; the usual selection,
word navigation and clipboard shortcuts work. Choose **Review** to see the
exact title and issue body, including Tileception's version.
"Include system details" starts unchecked. Checking it adds OS/version,
architecture, backend and output pixel sizes/scales to the preview.
Doctor's `/report` opens the same dialog over Doctor. The title is optional:
`/report` alone starts in the empty title field, `/report Wi-Fi drops` fills
it in and starts in the feedback field. The dialog has one more option,
"Include the last Doctor question and answer", also unchecked. Checking it
adds your last question and the agent's answer, shortened if long.
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
the lights and plugs by room, each with a switch. Click a light to show its
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
to connect each. **Pair device** searches for 30 seconds; flip a nearby
device's switch to pair it, and confirm the code when asked. Type `bt` in the
launcher to connect, disconnect or (`bt forget`) unpair devices.

### Updates

On systems that use APT (Ubuntu, Debian, Mint), a chip appears in the bar
while updates wait. It counts security updates first: `3 +9` means three
security updates and nine others. While updates install it shows progress,
and it says **Restart** when earlier updates need one or names a newer Ubuntu
release once one is out. With nothing to do there is no chip.

Click the chip for a card with the updates by kind and three buttons:
**Install all**, **Security only** and **Review**. Installing needs
administrator rights. If `sudo` asks for a password, Tileception shows its own
prompt and passes what you type to `sudo` for that one task.

**Review** opens the Updates window, which also opens with
`tileception msg updates open` or "Updates" in the launcher. It has four pages;
**Tab** and **Shift+Tab** switch between them, the arrow keys move through the
list, **Space** ticks a row and **Enter** runs the page's main button.

| Page | What it does |
| --- | --- |
| Available | Lists every update with its versions and download size. Untick the ones to leave out, then **Install**. **Check** refreshes the package lists. |
| Kernels | Does what [kerntop](https://github.com/robinvandernoord/kerntop) does. See below. |
| Release | Says whether a newer Ubuntu release is out and what to look at first: disk space, pending updates, package sources that the upgrade switches off, mains power and a snapshot. **Upgrade** runs `do-release-upgrade` in a terminal, where it asks its questions. |
| History | What apt did before, from its log. **Ask Doctor** sends the packages of one entry to Doctor; **Copy list** copies them. |

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
listed apart and queued per kind. Nothing here runs `autoremove`.

| Key | Action on the Kernels page |
| --- | --- |
| A | Switch between the recommended view and all variants |
| Q, or Space | Queue the highlighted kernel, or take it off the queue |
| I / D | Queue the highlighted kernel to install / to remove |
| U | Tick or untick the leftovers |
| P | Preview the queue, and go back |
| C | Clear the queue |
| R | Refresh the package lists |
| Enter | Apply the queue |

Type `up` in the launcher for the same actions as rows: install everything,
install the security updates, choose updates, clean up old kernels, upgrade to
the next release and check now.

From scripts and key bindings:

| Command | Action |
| --- | --- |
| `tileception msg updates` | List what is waiting |
| `tileception msg updates check` | Refresh the package lists |
| `tileception msg updates install` | Install every update |
| `tileception msg updates install security` | Install the security updates |
| `tileception msg updates install <package>...` | Install the named updates |
| `tileception msg updates kernels` | List kernels: running, fallback, old, available |
| `tileception msg updates clean` | Remove old kernels and leftover kernel packages |
| `tileception msg updates upgrade` | Start the release upgrade in a terminal |

Flatpak, Snap and firmware updates are not listed yet. See
[Configuration](configuration.md#updates) to switch the chip or its
notification off.

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
