# Tileception: common tasks. `make` on its own lists them.

.PHONY: help tools codespace container deps check-deps build clean test fmt lint install uninstall setup install-greeter uninstall-greeter try-greeter nested video deb deb-docker release changelog

# The binary bootstrap installs here, including when CARGO_HOME is customised.
export PATH := $(if $(CARGO_HOME),$(CARGO_HOME),$(HOME)/.cargo)/bin:$(PATH)

help:
	@echo "make tools       install cargo-plane via binstall (no source builds)"
	@echo "make codespace   set up a Codespace and install remote build dependencies"
	@echo "make container   set up a build container on an SSH host (CONTAINER_HOST=name)"
	@echo "                 with the build dependencies in its image"
	@echo "make deps        install local dependencies and Rust components"
	@echo "make check-deps  report missing local system packages"
	@echo "make build       build the compositor (release)"
	@echo "make clean       remove workspace and crate build directories"
	@echo "make test        run the tests of every crate"
	@echo "make fmt         format the workspace and independent crates"
	@echo "make lint        clippy and formatting checks"
	@echo "make install     check packages, build, install the session (config kept;"
	@echo "                 SETUP_ARGS='--preset minimal' passes setup choices;"
	@echo "                 offers to restart a running session, RESTART=yes|no answers)"
	@echo "make setup       choose the parts of the desktop and the login screen"
	@echo "make uninstall   remove the session (config and logs are kept)"
	@echo "make install-greeter    make the Tileception greeter the login screen (from the next boot)"
	@echo "make uninstall-greeter  switch back to the previous login screen and remove it"
	@echo "make try-greeter        run the greeter on a spare console, login screen untouched"
	@echo "make nested      run a development build in a window"
	@echo "make video       record and edit the promo video (target/promo;"
	@echo "                 MUSIC=track.mp3 adds music, SCENES='2 4' re-records some)"
	@echo "make deb         build optimised .deb packages for this system"
	@echo "make deb-docker  build .deb packages per release in containers"
	@echo "make release     release a new version with vommit, then write its changelog entry"
	@echo "                 (VOMMIT_ARGS='--patch' passes options to vommit, such as --noop)"
	@echo "make changelog   write the changelog page's entry for the latest release"
	@echo "                 with Claude Code (VERSION=0.19.0 picks one, FORCE=1 rewrites)"

tools:
	@if ! cargo plane setup codespace --help 2>/dev/null | grep -q -- '--yes'; then \
		if ! command -v cargo-binstall >/dev/null 2>&1; then \
			bash -o pipefail -c 'curl -L --proto "=https" --tlsv1.2 -sSf https://raw.githubusercontent.com/cargo-bins/cargo-binstall/main/install-from-binstall-release.sh | bash' || exit 1; \
		fi; \
		cargo binstall --no-confirm --disable-strategies compile 'cargo-plane@>=0.2' || \
			{ echo "cargo-binstall failed; downloading the release binary directly." >&2; scripts/install-cargo-plane.sh; } || exit 1; \
	fi

# Setup reuses the configured Codespace, creating one only when needed.
# Override the creation options, e.g. CODESPACE_ARGS="--ram 32".
CODESPACE_ARGS ?= --ram 16 --idle-timeout 30

codespace: tools
	cargo plane --remote codespace setup codespace -y $(CODESPACE_ARGS)
	cargo plane --remote codespace exec sh -c 'set -eu; sudo apt-get update; sudo env DEBIAN_FRONTEND=noninteractive apt-get install -y $$(scripts/deps.sh list build) dbus-daemon; scripts/deps.sh check build'
	@echo "Codespace server is fully prepared and ready to build Tileception."

# A Docker or Podman build container on your own SSH host. The build
# dependencies go into its image, so they survive the container being
# recreated; changing the list rebuilds the image and keeps the build volumes.
# CONTAINER_HOST defaults to the host already configured for the container
# remote. CONTAINER_ARGS passes more options, e.g. CONTAINER_ARGS="--image debian:12".
CONTAINER_HOST ?= $(shell awk '/^\[/ { in_section = ($$0 == "[remotes.container]") } in_section && $$1 == "host" { gsub(/"/, "", $$3); print $$3 }' .cargo/plane.toml 2>/dev/null)
CONTAINER_ARGS ?=

container: tools
	@if [ -z "$(CONTAINER_HOST)" ]; then \
		echo "No build host configured yet; run: make container CONTAINER_HOST=<ssh host>" >&2; \
		exit 1; \
	fi
	cargo plane --remote container setup container $(CONTAINER_HOST) -y \
		--packages "$$(scripts/deps.sh list build | paste -sd, -),dbus-daemon" $(CONTAINER_ARGS)
	cargo plane --remote container exec scripts/deps.sh check build
	@echo "Build container is fully prepared and ready to build Tileception."

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
	cargo plane build --release -p tileception -p tileception-updates
	scripts/build-clipboard-helper.sh target/release

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
	cargo plane exec scripts/deps.sh check build
	RESTART=$(RESTART) ./install-session.sh $(SETUP_ARGS)

setup:
	resources/tileception-setup $(SETUP_ARGS)

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

# The same build as `make install` (the dev-install profile), so one serves both.
nested: tools
	scripts/build-clipboard-helper.sh target/dev-install
	cargo plane run --profile dev-install -p tileception -- --nested

# The promo video: headless demo sessions recorded scene by scene, then cut
# together. The duck scene asks a real agent (PROMO_AGENT, codex-acp by
# default), so that agent must be signed in on this machine.
video: build
	@scripts/deps.sh install video
	scripts/promo/record.sh $(SCENES)
	MUSIC="$(MUSIC)" scripts/promo/edit.sh

# Optimised, stripped packages in target/deb. deb-docker builds amd64 and
# arm64 for each release scripts/suites.sh lists; override with DEB_IMAGES="..." DEB_ARCHS="...".
DEB_IMAGES ?= $(shell scripts/suites.sh images)

deb:
	scripts/build-deb.sh

deb-docker:
	scripts/build-deb-docker.sh $(DEB_IMAGES)

# VOMMIT_ARGS passes options to vommit, such as --patch or --noop. The
# changelog entry follows only when vommit made a new tag; if it fails, the
# release stands and `make changelog` tries again.
VOMMIT_ARGS ?=

# The entry is written by entry.sh itself, not through $(MAKE): make runs any
# line naming $(MAKE) even under `make -n`, which would release for real.
release: tools
	@before=$$(git describe --tags --abbrev=0 2>/dev/null); \
	vommit release $(VOMMIT_ARGS) || exit $$?; \
	after=$$(git describe --tags --abbrev=0 2>/dev/null); \
	if [ "$$after" = "$$before" ]; then echo "No new tag, so no changelog entry."; exit 0; fi; \
	scripts/changelog/entry.sh "$$after" || \
		echo "$$after is released, but its changelog entry is not: run make changelog VERSION=$$after to try again." >&2

changelog: tools
	scripts/changelog/entry.sh $(VERSION)
