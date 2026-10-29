# Troubleshooting

## Find the logs

Desktop sessions write to `~/.local/state/tileception/tileception.log`, or
`$XDG_STATE_HOME/tileception/tileception.log` if set. The previous session's log
is kept as `tileception.log.old`; check it if you have already logged in again
since the problem occurred.

To read the last 100 lines of the current log:

```sh
tail -n 100 "${XDG_STATE_HOME:-$HOME/.local/state}/tileception/tileception.log"
```

Nested instances print logs to the terminal where you started them. To capture
more detail while reproducing a problem:

```sh
RUST_LOG=debug,smithay=warn tileception --nested > tileception-nested.log 2>&1
```

Close the nested window when finished. Check logs for personal information
before sharing them, including window titles, paths and commands.

## Tileception does not start

### Missing from the login screen

Check that the session entry exists:

```sh
ls /usr/share/wayland-sessions/tileception.desktop
```

If it is missing, repeat the appropriate [installation](installation.md).
For source installations, `make build` only builds the binary; `make install`
also installs the login session entry. Log out to check the session list again.

### Login returns to the login screen or shows a blank screen

Log into another working desktop and check the session log. If no log was
created, include that in your report; the session launcher may not have reached
the compositor.

Try running in a window from a terminal:

```sh
tileception --nested
```

Report whether nested mode works even if the desktop session fails. Include
messages about graphics initialization, device access or missing libraries.
For a source build, run `make check-deps` from the checkout. If you build on a
remote machine, check that it matches your CPU architecture and does not use a
newer glibc than your system.

If the issue is the Tileception login screen itself, see the
[greeter guide](greeter.md).

## Config changes do not apply

Check that you edited `~/.config/tileception/config.toml`, or the file under
`XDG_CONFIG_HOME` if you set it. Changes normally reload within a second. To
reload explicitly and see any error:

```sh
tileception msg reload
```

A failed reload keeps the last working settings. If the config is invalid when
Tileception starts, it uses the built-in defaults and logs the error instead.

Check for duplicate section headings, misspelled keys and invalid values. Merge
examples into existing sections; only `[[rules]]` entries are meant to repeat.
See the [configuration guide](configuration.md) for examples.

Autostart changes take effect on the next startup. External tools such as
Waybar have their own config files.

## Recover from a stuck session

### Switch to a text console

Press **Ctrl+Alt+F3** and log in as the same user who owns the Tileception
session. From there, check whether the compositor responds:

```sh
tileception msg tree
```

Run these commands as your normal user. If they respond, you can reload a fixed
config with `tileception msg reload` without restarting the desktop.

To return to the graphical session, use its console's Ctrl+Alt+Fn shortcut.
The console number depends on your login manager; common choices are F1, F2
and F7.

### A lock screen is stuck

After logging into the text console with your account password, run:

```sh
tileception msg unlock
```

This forcibly ends the desktop's lock so you can return to your session. If an
external locker caused the problem, check its configuration before locking
again.

### End a responsive session

If the compositor responds but you need to end the session:

```sh
tileception msg quit
```

This ends the desktop session and closes its applications, so save work first
where possible. It is rejected while the session is locked; resolve the lock
first.

In an unlocked Tileception session, **Ctrl+Alt+Backspace** also ends the session.
If the compositor no longer responds, collect the available logs from the text
console before rebooting.

### A command cannot find the session

From a text console, `tileception msg` normally finds the only running instance.
If there are several, list the sockets:

```sh
ls "$XDG_RUNTIME_DIR"/tileception-*.sock
```

Set `TILECEPTION_SOCKET` to the socket for the instance you want to control.
For example, if its display is `wayland-1`:

```sh
export TILECEPTION_SOCKET="$XDG_RUNTIME_DIR/tileception-wayland-1.sock"
tileception msg tree
```

If Doctor does not connect, see [its own checklist](doctor.md#if-it-does-not-connect).

If the graphical login screen itself is broken, follow the
[greeter restoration instructions](greeter.md#restore-the-previous-login-screen)
from the text console.

## Report a problem

[Open an issue](https://github.com/robinvandernoord/tileception/issues) with:

- The output of `tileception --version` and your distribution and release.
- Graphics hardware and driver, and whether this is nested mode or a desktop
  session.
- Steps to reproduce the problem and what you expected to happen.
- Relevant log lines and the smallest config snippet needed to reproduce it.

If an application triggers the problem, include its version and whether it runs
through Wayland or Xwayland, if known.
