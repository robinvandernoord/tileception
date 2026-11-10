# Development

## Set up

Follow [From source](installation.md#from-source) to install dependencies and
select a local or remote builder. Run development commands from the repository
root unless noted otherwise.

Use cargo-plane for Cargo development commands, including manual invocations
such as `cargo plane check`, `cargo plane test` and `cargo plane fmt`.
Machine settings live in the ignored `.cargo/plane.toml`.

## Common commands

| Task                          | Command                         |
|-------------------------------|---------------------------------|
| Development build in a window | `make nested`                   |
| Release build                 | `make build`                    |
| Workspace compilation check   | `cargo plane check --workspace` |
| All crate tests               | `make test`                     |
| Apply Rust formatting         | `make fmt`                      |
| Formatting and Clippy checks  | `make lint`                     |

Formatting runs locally, including when the build uses a remote machine.
`make deps` installs rustfmt and Clippy through rustup, along with the local
system dependencies. For a Rust installation without rustup, it checks that
both tools are already available.

For a focused layout-engine test run:

```sh
cargo plane test -p tileception-core
```

### Independent crates

`tileception-apps`, `tileception-auth` and `tileception-notify` have independent
workspaces. Root `--workspace` commands do not test or lint them; `make fmt`,
`make test` and `make lint` include them explicitly.

For an independent crate, run from that crate's directory. For example:

```sh
cd crates/tileception-notify
cargo plane exec env -u DBUS_SESSION_BUS_ADDRESS cargo test
```

Clearing the builder's session-bus variable lets tests create a private D-Bus
session. The plain Cargo command here runs inside the selected builder.
For hardware-dependent tests, `cargo plane test --local-run` runs locally and
skips doctests.

## Lint policy

`make lint` checks formatting and all Clippy targets, including the independent
crates. Warnings fail the check.

All crates deny unsafe code by default. Allow it at the smallest practical
scope with `#[allow(unsafe_code, reason = "why this native operation is needed")]`.
Give each unsafe block a `// SAFETY:` comment explaining its invariants. Unsafe
functions still need explicit unsafe blocks; unnecessary unsafe blocks are
errors. Clippy requires a `reason` on lint exceptions.

Production code denies `unwrap()` and `unwrap_err()`. Handle recoverable
failures, and use `expect("why this must succeed")` for invariants. Test modules
explicitly allow unwraps for setup and assertions.

The root workspace defines these lints in [Cargo.toml](../Cargo.toml). The
independent crates carry the same policy in their own manifests.

## Repository structure

### Clipboard persistence helper

`scripts/build-clipboard-helper.sh` builds wl-clip-persist from an exact upstream
Git revision with its lockfile. No upstream source is copied into this repo.
Development builds use the selected cargo-plane builder and copy the helper
back; Debian builds compile it inside their distribution build environment.
`make build`, `make nested` and session installation put it beside the compositor
as `tileception-clip-persist`. Debian packages install both in `/usr/bin`.

When updating the revision, update the version and revision in
`packaging/clipboard-helper-notices.hbs` too. Generate the committed notices
with cargo-about (0.9.3 was used for the current notices), pointing at the
upstream checkout for that exact revision:

```sh
cargo plane exec cargo about generate --locked --fail \
  --manifest-path <upstream-checkout>/Cargo.toml \
  --config packaging/clipboard-helper-about.toml \
  packaging/clipboard-helper-notices.hbs \
  --output-file packaging/clipboard-helper-NOTICES.txt
```

Run this on a builder with cargo-about installed; retrieve the resulting file
when using a remote builder. The committed notices include the helper and its
locked dependencies for all platforms, and ship with both installation paths.

### Crates and resources

| Crate                | Responsibility                                            |
|----------------------|-----------------------------------------------------------|
| `tileception-core`   | Layout tree, camera, focus, history and saved views       |
| `tileception`        | Smithay compositor, backends and built-in desktop         |
| `tileception-apps`   | Desktop entries, icons, application actions and autostart |
| `tileception-auth`   | PAM authentication                                        |
| `tileception-notify` | D-Bus notification service                                |
| `tileception-media`  | Audio, media playback and backlight controls              |
| `tileception-acp`    | Agent registry, installation and ACP client               |
| `tileception-updates` | Pending APT updates, kernels, release checks, apt history |

The layout engine has no Smithay dependency or I/O. The compositor drives it
with window and input events, then renders the resulting layout. DRM/KMS runs
the desktop session, winit runs nested windows, and the headless backend renders
without taking over a display.

Settings uses an in-process Wayland client with a real `xdg_toplevel` and
shared-memory buffers. Its UI model stays in the compositor, while pointer,
keyboard, moving, resizing and closing use the normal Wayland window paths.
Backends service its drawing through `update_settings_client`, including the
headless backend, so UI updates do not depend on taking a screenshot. Screen
captures and window previews use the client's surface like any other window.

Built-in controls share `ui::PointerTarget` for cursor feedback and button
activation. Text uses the I-beam, buttons use the pointer hand, and sliders
and draggable displays advertise their drag behaviour. Buttons activate only
after a press and release over the same control; moving away cancels the
activation. Text selection, sliders and window/display dragging start on
press. Keep hit regions aligned with the drawn controls, including disabled
states and modal dialogs. Each component owns its typed actions and defines its
controls once, using the same bindings for hit testing, cursor feedback and
activation. The shared layer stores the armed target and matches release to
press; it does not dispatch string scopes or maintain lists of control IDs.

`resources/` contains session helpers and desktop integration files.
`scripts/` contains dependency, installation and packaging tools.
`packaging/` contains package metadata and the archive signing key.
See [website/README.md](../website/README.md) for the website workflow.

## Debugging without a desktop session

The headless backend needs access to a GPU render node. Start it with:

```sh
TILECEPTION_HEADLESS_SIZE=1280x800 cargo plane run -p tileception -- --headless
```

Use the `TILECEPTION_SOCKET` path printed at startup to target that instance
with `tileception msg`, especially if your desktop is also running Tileception.
The command interface supports screenshots and input injection in headless and
nested instances. `tileception msg --help` lists the commands.

A nested or headless instance leaves updates to the desktop it runs in. Set
`TILECEPTION_UPDATES=1` to start the update manager there too. It runs
`apt-get`, `dpkg-query`, `apt-cache`, `uname`, `do-release-upgrade` and `sudo`
from `PATH`, so stand-in scripts placed first in `PATH` give it kernels, a
release offer or a `sudo` that asks for a password, without changing the
system.

Headless instances run Xwayland too, so X11 programs can be tried the same way.
Start programs inside an instance with `tileception msg spawn <command line>`,
drive them with `pointer`, `click`, `press`, `release`, `key` and `type`, and
check the result with `screenshot <file.png>`.

### Handoff between two instances

Two headless instances on one machine can stand in for two computers sharing
through handoff. Give each its own `XDG_CONFIG_HOME` and `XDG_STATE_HOME`, and a
config that links it to the other over localhost only:

```toml
[handoff]
enabled = true
passphrase = "any phrase both share"
name = "left"
port = 24870
discovery = false
peers = ["127.0.0.1:24871"]
```

The other instance uses `name = "right"`, `port = 24871` and
`peers = ["127.0.0.1:24870"]`. Once `tileception msg handoff` lists the other
machine, place them side by side from both ends:

```sh
TILECEPTION_SOCKET=<left socket> tileception msg handoff edge right headless right
TILECEPTION_SOCKET=<right socket> tileception msg handoff edge left headless left
```

The pointer then crosses at those edges, and windows, drags, the clipboard and
menus can be tried between the two with injected input, taking screenshots of
both. The handoff scene of the promo video (`scripts/promo/scenes/5-handoff.sh`,
with helpers in `scripts/promo/lib.sh`) does all of this, including private
session buses that keep the instances away from your own desktop.

See [Troubleshooting](troubleshooting.md) for logs and runtime diagnostics.

## Promo video

`make video` builds the compositor, installs the `video` packages from
`deps/debian.txt` and records the promo video into
`target/promo/tileception-promo.mp4`. Each scene in `scripts/promo/scenes`
runs headless Tileceptions with their own config, state and D-Bus session,
drives them with `tileception msg` and records them with wf-recorder.
`scripts/promo/edit.sh` then adds title cards and captions from
`scripts/promo/art/art.html`.

The duck scene asks a real agent. By default this is `codex-acp`, which uses
the Codex sign-in already on the machine; `PROMO_AGENT` chooses another ACP
Registry agent. `SCENES='2 4'` records only those scenes again before editing,
and `MUSIC=track.mp3` adds a soundtrack that fades out at the end. The handoff
scene runs two instances paired over localhost, with mDNS discovery off.

## Packaging

`make deb` builds packages for the current system into `target/deb`.
`make deb-docker` builds them in distribution-specific containers; its default
matrix is listed in the [Makefile](../Makefile).

After building for your current system, install the package with:

```sh
sudo apt install ./target/deb/tileception_*.deb
```

Packages use the `dist` profile with fat LTO and one codegen unit. The shipped
binary is stripped; a separate `tileception-dbgsym` package provides debug
symbols for file and line information in backtraces and debuggers. Install the
matching symbols when investigating a crash:

```sh
sudo apt install ./target/deb/tileception-dbgsym_*.deb
```

Container builds need Docker and produce packages under
`target/deb/<codename>/`, with a distribution suffix in the version. To build
only sid for amd64:

```sh
make deb-docker DEB_IMAGES=debian:sid DEB_ARCHS=amd64
```

`DEB_IMAGES` and `DEB_ARCHS` accept space-separated lists. Architectures other
than the host's are cross-compiled. For a direct cross-build, `make deb
DEB_ARCH=arm64` also works, provided you have the cross compiler and arm64 build
dependencies installed; the container workflow sets those up for you.

Packaging deliberately uses plain Cargo inside its build environment, so linked
libraries and detected package dependencies match the target distribution.
Use packages built for your distribution. Remove a source installation before
installing a package, since both use the same session files.

## CI workflows

- [CI](../.github/workflows/ci.yml) runs on pull requests and manual dispatch.
  Separate jobs run `make lint` and `make test` on Ubuntu with a local
  cargo-plane builder, including the independent crates.
- [Release](../.github/workflows/release.yml) runs when a `v*` tag is pushed.
  It validates the version, builds packages and publishes them as described
  [below](#releases).
- [Website](../.github/workflows/website.yml) checks changes to `website/` or
  its workflow on pull requests and pushes to `master`. It can also run
  manually. Passing runs on pushes to `master` deploy the site and installer,
  then check the live endpoints. See the
  [website guide](../website/README.md) for deployment settings.

The release workflow does not rerun the CI lint and test jobs; run the checks
before releasing.

## Releases

Releases use vommit, configured in [pyproject.toml](../pyproject.toml). It
derives the version bump from [Conventional Commits](https://www.conventionalcommits.org/)
messages such as `feat(shell): ...` and `fix(greeter): ...`, so use that format
for every commit. From a clean `master` checkout, run the checks and preview the
release:

```sh
make lint
make test
vommit release --noop
```

Then release with:

```sh
vommit release
```

Vommit determines the version bump from the commits, updates the Cargo version,
`Cargo.lock` and `CHANGELOG.md`, creates a release commit and `v<version>` tag,
and pushes them to `origin`. It asks for confirmation before proceeding. Local
build and publish hooks are empty; package building and publishing happen in CI.

Pushing the tag starts the [release workflow](../.github/workflows/release.yml):

1. Check that the tag matches the version in `Cargo.toml`.
2. Build `.deb` packages for Debian trixie and sid, and Ubuntu noble and
   resolute, on both amd64 and arm64.
3. Publish the packages and a per-distribution archive-keyring package to
   `https://apt.tileception.space`, then check the repository metadata and
   package downloads.
4. Attach the compositor and debug-symbol packages to a GitHub release.

APT publishing and GitHub release creation run as separate jobs after the
builds. Check that both succeed.

The APT repository keeps the five newest versions of each package per
distribution and architecture, so users can go back with
`sudo apt install tileception=<version>`. GitHub release assets hold the full
history.

### Prereleases

```sh
vommit release --prerelease
```

This makes a release candidate: `Cargo.toml` gets `1.2.0-rc.1` and the tag is
`v1.2.0rc1`. The release workflow accepts either tag spelling, versions the
packages `1.2.0~rc.1-1~<distribution>` (`~` sorts before the `1.2.0-1`
release), publishes them to the repository's `pre` component instead of
`main` and marks the GitHub release as a prerelease. Users who only have
`main` never see it. Testers opt in by adding `pre` to the components in
`/etc/apt/sources.list.d/tileception.sources`:

```
Components: main pre
```

apt then installs whichever version is newest, so a tester on `1.2.0~rc.2`
moves to `1.2.0` on the next upgrade, with no switch back needed.

### Internal crate versions

The root `[workspace.package].version` is inherited by `tileception`,
`tileception-core`, `tileception-media` and `tileception-acp`. Vommit updates
that shared version and refreshes the workspace lockfile automatically.

Internal dependencies currently use `path` without a `version` requirement,
so major and minor releases do not need separate dependency-range updates.
If version requirements are added later, they must also accept the new crate
versions.

The independent `tileception-apps`, `tileception-auth` and `tileception-notify`
crates have their own versions. They do not inherit the compositor's release
version.

## Documentation

When adding a Markdown guide, link it from [the documentation index](index.md)
and add it to `export_doctor_docs` in `crates/tileception/src/doctor.rs`.
Doctor bundles these guides into the binary for installations without source.
Links to other repository files do not resolve in that copy, except the default
config: write it exactly as `../crates/tileception/src/default-config.toml`,
which the export rewrites.
