#!/bin/sh
# Prints the Debian version for the Cargo version: DEB_VERSION when set,
# otherwise <cargo version>-1 with a SemVer prerelease's "-" turned into "~",
# so 1.2.0-rc.1 becomes 1.2.0~rc.1-1 and sorts below the 1.2.0-1 release.
# build-deb-docker.sh and build-keyring-deb.sh append ~<suite> to this.
set -eu
cd "$(dirname "$0")/.."

if [ -n "${DEB_VERSION:-}" ]; then
    echo "$DEB_VERSION"
    exit 0
fi
cargo_version=$(sed -n '/^\[workspace.package\]/,/^\[/s/^version *= *"\(.*\)"/\1/p' Cargo.toml)
[ -n "$cargo_version" ] || { echo "deb-version: no version in Cargo.toml's [workspace.package]" >&2; exit 1; }
# SemVer build metadata (+...) has no place in a Debian version.
echo "$(echo "${cargo_version%%+*}" | sed 's/-/~/')-1"
