# Tileception: common tasks. `make` on its own lists them.

.PHONY: help deps check-deps build test lint install uninstall nested

help:
	@echo "make deps        install missing system packages (asks first)"
	@echo "make check-deps  report missing system packages"
	@echo "make build       build the compositor (release)"
	@echo "make test        run the tests of every crate"
	@echo "make lint        clippy and formatting checks"
	@echo "make install     check packages, build, install the session"
	@echo "make uninstall   remove the session (config and logs are kept)"
	@echo "make nested      run a development build in a window"

deps:
	@scripts/deps.sh install

check-deps:
	@scripts/deps.sh check

build:
	@scripts/deps.sh check build
	cargo build --release -p tileception

# The library crates from .codex-work are standalone until they are wired in,
# so they are tested on their own.
CRATES := $(wildcard crates/tileception-apps crates/tileception-notify crates/tileception-auth)

test:
	@scripts/deps.sh check build
	cargo test --workspace
	@for c in $(CRATES); do echo "== $$c"; (cd $$c && env -u DBUS_SESSION_BUS_ADDRESS cargo test) || exit 1; done

lint:
	cargo fmt --all --check
	cargo clippy --workspace --all-targets -- -D warnings
	@for c in $(CRATES); do echo "== $$c"; (cd $$c && cargo fmt --check && cargo clippy --all-targets -- -D warnings) || exit 1; done

install:
	@scripts/deps.sh check build runtime
	./install-session.sh

uninstall:
	./uninstall-session.sh

nested:
	cargo run -p tileception -- --nested
