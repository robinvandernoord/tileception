# Installation

## Packages

### Supported systems

Packages are available for **amd64** and **arm64** on:

| Distribution | Releases                        |
|--------------|---------------------------------|
| Debian       | 13 (trixie), unstable (sid)     |
| Ubuntu       | 24.04 (noble), 26.04 (resolute) |

Derivatives based on these releases are supported too, including **Linux Mint**.
The installer selects packages for the underlying Debian or Ubuntu release.

Tileception is in alpha and currently supports one monitor. Additional monitors
stay inactive in a Tileception desktop session.

### Install

Run this as your normal user, with sudo available:

```sh
curl -fsSL https://tileception.space/install | sh
```

The [installer](../website/public/install) adds the signed APT repository at
`https://apt.tileception.space` and installs `tileception` and
`tileception-archive-keyring`. The keyring package manages the repository's
signing key and APT source. The installer asks for your sudo password if needed;
package installation then runs without further questions.

Try Tileception in a window inside your current Wayland or X11 desktop:

```sh
tileception --nested
```

To use it as your desktop session, log out and select **Tileception** on the
login screen. Your existing login screen stays in place; installing the package
does not switch it to the Tileception greeter.

See [Getting around](../README.md#getting-around) for the first shortcuts.
In a nested window, use Alt in place of Super.

### Update

Updates arrive through your normal system updates. To update Tileception
explicitly:

```sh
sudo apt update
sudo apt install --only-upgrade tileception tileception-archive-keyring
```

Log out of Tileception and back in to start the new version.

### Remove

Log in to another desktop before removing Tileception. If you configured
Tileception as your greetd login screen, restore your previous login setup first.

```sh
sudo apt remove tileception
```

Your personal configuration and session logs are kept. To also remove the APT
repository and its signing key:

```sh
sudo apt remove tileception-archive-keyring
```

Removing the keyring package also removes
`/etc/apt/sources.list.d/tileception.sources`.

## From source

Install Rust/Cargo, Git, Make, curl and Bash first. The dependency helpers below
target Debian, Ubuntu and Linux Mint; the package list is in
[`deps/debian.txt`](../deps/debian.txt).

Clone the repository and run `make tools` to install the build tooling:

```sh
git clone https://github.com/robinvandernoord/tileception.git
cd tileception
make tools
```

`make tools` installs [cargo-plane](https://github.com/robinvandernoord/cargo-plane),
which runs builds locally or on a remote machine.

### Choose a builder

On a machine with more than 4 GB of RAM, use a local builder:

```sh
cargo plane setup local
```

On smaller machines, use a remote builder instead.

<details>
<summary>Set up a GitHub Codespace as the builder</summary>

Install SSH, rsync and the GitHub CLI locally. Authenticate with Codespaces
access, unless you are already signed in with that access:

```sh
gh auth login --scopes codespace
```

Then set up and select the builder:

```sh
make codespace
export CARGO_PLANE_REMOTE=codespace
```

This reuses the configured Codespace or creates one requesting 16 GB of RAM
and a 30-minute idle timeout. It installs the remote build dependencies too.
Set this variable in every terminal you build from, or add it to your shell
profile.

The builder must match your CPU architecture and use a glibc version no newer
than your local system. Builds return the binaries to your machine, where they
run using your local runtime libraries.

Codespaces use your GitHub compute and storage allowances. Stop the builder
with `cargo plane land` when finished. See the
[builder instructions](../AGENTS.md#install-and-choose-a-builder) for choosing an
existing Codespace and adjusting creation options.

</details>

### Install dependencies and try it

From the repository root, install the local dependencies. This installs rustfmt
and Clippy through rustup and asks before installing missing system packages
with sudo, including when using a remote builder:

```sh
make deps
```

Then build and run inside your current Wayland or X11 desktop:

```sh
make nested
```

The compositor runs locally even when the build runs remotely. Use Alt in place
of Super for shortcuts in the nested window.

### Install as a session

If the `tileception` APT package is installed, remove it before installing from
source; both installations use the same system files.

```sh
make install
```

Run this as your normal user. It builds the release binary, installs it and its
helpers in `~/.local/bin`, and requests sudo for system files such as the login
session entry and PAM policy.

Log out, then select **Tileception** on the login screen.

### Update or remove

To update a checkout without local changes and reinstall:

```sh
git pull --ff-only
make install
```

The running session keeps its current binary until you log out and back in.

To remove a source installation, log in to another desktop and run this from the
repository root:

```sh
make uninstall
```

Your personal configuration and logs are kept. If you also installed the
Tileception greeter, switch back to your previous login screen before removing
the session.

## Start from a text console

After installing the package or source session, you can start Tileception
without choosing it from a login screen. Switch to a free console with
**Ctrl+Alt+F3**, log in as your normal user, and run:

```sh
tileception-session
```

For a source installation, use `~/.local/bin/tileception-session` if that
directory is not on your PATH. The wrapper starts a hardware session and writes
its log to `~/.local/state/tileception/tileception.log`.

Use **Ctrl+Alt+F2**, or the function key for your existing desktop's console,
to switch back while Tileception runs. **Super+Shift+E** ends Tileception and
returns to the text console. **Ctrl+Alt+Backspace** also quits while the session
is unlocked. See [Troubleshooting](troubleshooting.md) for recovery and logs.

## Optional login screen

Tileception also includes a greetd login screen. See the
[greeter guide](greeter.md) for previewing it, testing a real login, enabling it
and restoring your previous login screen.
