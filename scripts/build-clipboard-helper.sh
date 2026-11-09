#!/bin/bash
# Build the pinned upstream executable without vendoring its source. Development
# uses the selected cargo-plane builder; --package runs inside the distribution
# build environment, just like scripts/build-deb.sh.
set -euo pipefail
cd "$(dirname "$0")/.."

# Update packaging/clipboard-helper-NOTICES.txt when changing this revision.
revision=e26fde01c13922e3a65049dafb7d5adfbc52626e # wl-clip-persist v0.5.0
repository=https://github.com/Linus789/wl-clip-persist

case "${1:-}" in
    --builder|--package)
        mode=$1
        shift
        target_dir="${CARGO_TARGET_DIR:-target}"
        # The transfer below uses this fixed project-relative path, regardless
        # of a builder's personal CARGO_TARGET_DIR. Packaging honours its own.
        if [ "$mode" = --builder ]; then target_dir=target; fi
        cargo install --git "$repository" --rev "$revision" --locked --bin wl-clip-persist \
            --root "$target_dir/clipboard-helper" \
            --target-dir "$target_dir/clipboard-helper-build" "$@"
        ;;
    *)
        destination="${1:?usage: build-clipboard-helper.sh <binary-directory>}"
        cargo plane exec scripts/build-clipboard-helper.sh --builder
        # exec rewrites remote paths in stdout. Encode the executable so that
        # this text processing cannot alter binary bytes. Normal build artifact
        # transfer does not cover cargo install.
        staging=$(mktemp -d)
        trap 'rm -rf "$staging"' EXIT
        cargo plane exec base64 target/clipboard-helper/bin/wl-clip-persist \
            | base64 --decode > "$staging/wl-clip-persist"
        install -D -m 755 "$staging/wl-clip-persist" "$destination/tileception-clip-persist"
        ;;
esac
