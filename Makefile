# Tileception: common tasks. `make` on its own lists them.

.PHONY: help tools codespace deps check-deps build clean test fmt lint install uninstall install-greeter uninstall-greeter try-greeter nested deb deb-docker

# The binary bootstrap installs here, including when CARGO_HOME is customised.
export PATH := $(if $(CARGO_HOME),$(CARGO_HOME),$(HOME)/.cargo)/bin:$(PATH)

help:
	@echo "make tools       install cargo-plane via binstall (no source builds)"
	@echo "make codespace   set up a Codespace and install remote build dependencies"
	@echo "make deps        install local dependencies and Rust components"
	@echo "make check-deps  report missing local system packages"
	@echo "make build       build the compositor (release)"
	@echo "make clean       remove workspace and crate build directories"
	@echo "make test        run the tests of every crate"
	@echo "make fmt         format the workspace and independent crates"
	@echo "make lint        clippy and formatting checks"
	@echo "make install     check packages, build, install the session"
	@echo "make uninstall   remove the session (config and logs are kept)"
	@echo "make install-greeter    make the Tileception greeter the login screen (from the next boot)"
	@echo "make uninstall-greeter  switch back to the previous login screen and remove it"
	@echo "make try-greeter        run the greeter on a spare console, login screen untouched"
	@echo "make nested      run a development build in a window"
	@echo "make deb         build optimised .deb packages for this system"
	@echo "make deb-docker  build .deb packages per release in containers"

tools:
	@if ! cargo plane setup codespace --help 2>/dev/null | grep -q -- '--yes'; then \
		if ! command -v cargo-binstall >/dev/null 2>&1; then \
			bash -o pipefail -c 'curl -L --proto "=https" --tlsv1.2 -sSf https://raw.githubusercontent.com/cargo-bins/cargo-binstall/main/install-from-binstall-release.sh | bash' || exit 1; \
		fi; \
		cargo binstall --no-confirm --disable-strategies compile 'cargo-plane@>=0.2'; \
	fi

# Setup reuses the configured Codespace, creating one only when needed.
# Override the creation options, e.g. CODESPACE_ARGS="--ram 32".
CODESPACE_ARGS ?= --ram 16 --idle-timeout 30

codespace: tools
	cargo plane --remote codespace setup codespace -y $(CODESPACE_ARGS)
	cargo plane --remote codespace exec sh -c 'set -eu; sudo apt-get update; sudo env DEBIAN_FRONTEND=noninteractive apt-get install -y $$(scripts/deps.sh list build) dbus-daemon; scripts/deps.sh check build'
	@echo "Codespace server is fully prepared and ready to build Tileception."

deps: tools
	@if command -v rustup >/dev/null 2>&1; then \
		rustup component add rustfmt clippy; \
	elif ! command -v rustfmt >/dev/null 2>&1 || ! command -v cargo-clippy >/dev/null 2>&1; then \
		echo "rustfmt and Clippy are required; install them with your Rust distribution or use rustup." >&2; \
		exit 1; \
	fi
	@scripts/deps.sh install

check-deps:
	@scripts/deps.sh check

build: tools
	cargo plane exec scripts/deps.sh check build
	cargo plane build --release -p tileception

clean:
	rm -rf target crates/*/target

# Support libraries keep independent workspaces. Cargo does not run dependency
# tests, so run their tests and checks explicitly too.
CRATES := $(wildcard crates/tileception-apps crates/tileception-notify crates/tileception-auth)

test: tools
	cargo plane exec scripts/deps.sh check build
	cargo plane test --workspace
	@for c in $(CRATES); do echo "== $$c"; (cd $$c && cargo plane exec env -u DBUS_SESSION_BUS_ADDRESS cargo test) || exit 1; done

fmt: tools
	cargo plane fmt --all
	@for c in $(CRATES); do echo "== $$c"; (cd $$c && cargo plane fmt) || exit 1; done

lint: tools
	cargo plane fmt --all --check
	cargo plane clippy --workspace --all-targets -- -D warnings
	@for c in $(CRATES); do echo "== $$c"; (cd $$c && cargo plane fmt --check && cargo plane clippy --all-targets -- -D warnings) || exit 1; done

install: tools
	@scripts/deps.sh check build runtime
	./install-session.sh

uninstall:
	./uninstall-session.sh

install-greeter: tools
	@scripts/deps.sh check build
	scripts/install-greeter.sh

uninstall-greeter:
	scripts/uninstall-greeter.sh

try-greeter: tools
	@scripts/deps.sh check build
	scripts/try-greeter.sh

nested: tools
	cargo plane run -p tileception -- --nested

# Optimised, stripped packages in target/deb. deb-docker builds amd64 and
# arm64 for each release; override with DEB_IMAGES="..." DEB_ARCHS="...".
DEB_IMAGES ?= debian:trixie debian:sid ubuntu:24.04 ubuntu:26.04

deb:
	scripts/build-deb.sh

deb-docker:
	scripts/build-deb-docker.sh $(DEB_IMAGES)
