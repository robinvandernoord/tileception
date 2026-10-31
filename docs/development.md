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

| Crate                | Responsibility                                            |
|----------------------|-----------------------------------------------------------|
| `tileception-core`   | Layout tree, camera, focus, history and saved views       |
| `tileception`        | Smithay compositor, backends and built-in desktop         |
| `tileception-apps`   | Desktop entries, icons, application actions and autostart |
| `tileception-auth`   | PAM authentication                                        |
| `tileception-notify` | D-Bus notification service                                |
| `tileception-media`  | Audio, media playback and backlight controls              |
| `tileception-acp`    | Agent registry, installation and ACP client               |

The layout engine has no Smithay dependency or I/O. The compositor drives it
with window and input events, then renders the resulting layout. DRM/KMS runs
the desktop session, winit runs nested windows, and the headless backend renders
without taking over a display.

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

See [Troubleshooting](troubleshooting.md) for logs and runtime diagnostics.

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
