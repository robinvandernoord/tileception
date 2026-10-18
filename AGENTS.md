# Building and checking Tileception

Use [cargo-plane](https://github.com/robinvandernoord/cargo-plane) for Cargo
development commands so the same workflow works locally and on a remote builder.
Run setup from the repository root before building. Do not silently fall back to
plain Cargo when a remote or binary installation fails.

## Install and choose a builder

Rust/Cargo, Make, curl and Bash must be installed locally. Install cargo-plane:

```sh
make tools
```

This installs cargo-binstall with its upstream binary install script if needed,
then installs cargo-plane with `cargo binstall --no-confirm --disable-strategies
compile 'cargo-plane@>=0.2'`. Existing cargo-plane installations are upgraded
if they lack unattended setup support. Source compilation is
disabled to keep installation usable on small machines. If no binary is
available, report the failure and wait for the release to become available.
`make deps` also runs this tool installation before checking system packages.

On a machine with more than 4 GB of RAM, the simplest setup is local execution:

```sh
cargo plane setup local
make deps
```

On a machine with 4 GB of RAM or less, use a remote builder, for example a
GitHub Codespace with more than 4 GB of RAM. Install `ssh`, `rsync` and the
[GitHub CLI](https://cli.github.com/) locally, then run:

```sh
gh auth login --scopes codespace
make codespace
export CARGO_PLANE_REMOTE=codespace
```

Skip login when already authenticated with Codespaces access. `make codespace`
uses cargo-plane 0.2's unattended setup to reuse the configured `codespace`
remote or create one when needed. New machines request at least 16 GB of RAM
(normally 4 CPUs) and a 30-minute idle timeout. Setup installs the basic remote
Rust build tools. The target then runs `apt-get update` on that machine and
installs only this project's build dependencies and `dbus-daemon` for tests.
It always selects the named `codespace` remote, even if another builder is
currently selected. Repeating the target refreshes dependencies on the same
machine. Creation options do not resize an existing Codespace.

Override creation options or explicitly choose an existing Codespace:

```sh
make codespace CODESPACE_ARGS='--ram 32 --idle-timeout 60'
make codespace CODESPACE_ARGS='--codespace existing-codespace-name'
```

Use `cargo plane shell` for interactive remote provisioning. `cargo plane exec`
runs noninteractive commands in the synced project, for example
`cargo plane exec scripts/deps.sh check build`. `make deps` installs system
packages on the local machine, including runtime and optional desktop helpers;
it refreshes apt indexes before installing missing packages. Use `make codespace`
to provision the remote builder instead.

Setup stores personal machine settings in `.cargo/plane.toml` and ignores that
file via `.cargo/.gitignore`. Keep machine settings untracked. Configuration is
also found from the independent crates below the root. To select among existing
builders, use `CARGO_PLANE_REMOTE=local` or `CARGO_PLANE_REMOTE=codespace`, or
`cargo plane --remote <name> <command>`. Check with `cargo plane status`; stop a
remote builder with `cargo plane land` when finished. Codespaces consume GitHub
compute and storage allowances; choose an idle timeout longer than a full build.

## Commands

Prefer the Make targets, which use the selected cargo-plane builder:

| Task | Make target | Cargo equivalent |
| --- | --- | --- |
| Release compositor | `make build` | `cargo plane build --release -p tileception` |
| All tests, including independent crates | `make test` | `cargo plane test --workspace`, plus independent crate tests |
| Formatting and Clippy, including independent crates | `make lint` | `cargo plane fmt --all --check` and `cargo plane clippy --workspace --all-targets -- -D warnings`, plus independent crate checks |
| Nested compositor | `make nested` | `cargo plane run -p tileception -- --nested` |
| Quick check | | `cargo plane check --workspace` |
| One crate's tests | | `cargo plane test -p tileception-core` |

Replace `cargo build`, `check`, `test`, `clippy`, `run` and `fmt` with
`cargo plane build`, `check`, `test`, `clippy`, `run` and `fmt` respectively,
preserving arguments and the `--` separator. Formatting still runs locally;
install the local rustfmt component with `rustup component add rustfmt`.

`tileception-apps`, `tileception-auth` and `tileception-notify` have independent
workspaces. Root `--workspace` commands do not check or test them. Run commands
from each crate's directory, as Make does. Their tests run on the builder;
Make uses `cargo plane exec env -u DBUS_SESSION_BUS_ADDRESS cargo test` to
clear the builder's session bus variable for tests that start a private bus.
The plain Cargo invocation inside `exec` runs on the selected builder.

`cargo plane build` copies binaries back to `target/`; `cargo plane run` builds
on the builder and runs locally. For locally runnable artifacts, match the
CPU architecture and use a builder with glibc no newer than the local system.
The local system also needs the linked runtime libraries. Use
`cargo plane test --local-run` for tests that need local hardware; this skips
doctests. A nested compositor needs a local graphical session.

`make install` and `./install-session.sh` build through cargo-plane, then
install the session locally and request sudo for system files. Set up the
builder first and use `make deps` for local build/runtime packages.
Debian packaging (`make deb` and `make deb-docker`) deliberately uses plain
Cargo inside its distribution-specific build environment so library dependency
detection and cross-compilation use that environment's libraries and tools.

CI installs cargo-plane with `make tools` and selects `cargo plane setup local`
before running the same lint and test targets.
