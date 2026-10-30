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
any minimized windows that belong to it.

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

**Super+N** opens notification history and sound controls. Use the panel's
**Do not disturb** control to silence ordinary notification popups.

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

### Password manager auto-type

KeePassXC 2.8 and newer can type usernames and passwords into other windows
through the desktop portal. The first time a program wants to type, a
notification asks whether to allow it: **Deny**, **Allow once**, or, when the
program asks to be remembered, **Until it quits** or **Always**. Keys go to the
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
