# Login screen

> **Temporary status:** The greeter does not yet have a production installation
> path. The instructions below are for development and testing. An improved
> installer is planned for this branch's merge and will ask whether you want to
> enable the greeter.

Tileception includes an optional login screen for greetd. It lists local users
and installed desktop sessions, including other Wayland and X11 desktops.
Choosing it changes the login screen for the whole machine, not just Tileception.

Installing the Tileception session does not enable this greeter automatically.

## Preview in a window

With Tileception installed, run this inside your current desktop:

```sh
tileception --greeter --nested
```

This is a visual preview: the password is `tide`, and it does not log you into a
real desktop session. Close the window when finished.

## Test a real login

From a [prepared source checkout](installation.md#from-source), run:

```sh
make try-greeter
```

This builds Tileception and starts a temporary greetd service on console 8,
installing greetd if needed. It uses sudo and leaves your configured login screen
in place.

Switch to the test with **Ctrl+Alt+F8**. Use your normal account password; logging
in starts a second desktop session alongside your current one. The command
prints the console number to return to. Return there and press **Ctrl+C** to stop
the test and display its logs.

To use a different spare console:

```sh
VT=9 make try-greeter
```

## Use it as the login screen

The commands in this section are for source installations on Debian, Ubuntu and
Linux Mint. The APT package includes the greeter binary, but
`make install-greeter` currently refuses to run while that package is installed.

After [installing the session from source](installation.md#install-as-a-session)
and testing a real login, run from the repository root:

```sh
make install-greeter
```

The installer builds a separate copy in `/usr/local/bin/tileception`, configures
greetd to run it, and enables greetd in place of your previous display manager.
It saves the previous display manager and greetd configuration for restoration.
An existing `/etc/tileception/greeter.toml` is kept.

The switch takes effect at the **next boot**. It does not restart your current
login screen or end your graphical session.

## Settings

Edit `/etc/tileception/greeter.toml` with sudo. The
[commented defaults](../resources/greeter.toml) cover the default session, hidden
users and sessions, panel layout, clock, theme and keyboard layout. Settings are
read when the greeter starts.

By default, Tileception sessions share their accent colour and wallpaper with
the greeter. To turn sharing off, set `share = false` in the existing `[greeter]`
section of your personal `~/.config/tileception/config.toml`.

## Restore the previous login screen

From the repository root, run:

```sh
make uninstall-greeter
```

This restores the saved display manager and greetd configuration. Reboot to
complete the switch. If greetd was still running, the script keeps its Tileception
binary until then; run `make uninstall-greeter` again after reboot to remove it.

The greetd package and `/etc/tileception/greeter.toml` are kept. To remove that
configuration too, use `scripts/uninstall-greeter.sh --purge` instead.

If there was no previous display manager, the script asks you to enable another
one before it removes the greeter. If the graphical login is unavailable, switch
to a text console with **Ctrl+Alt+F3**, log in, and run the restore command there.
