#!/bin/sh
# Installs the newest cargo-plane release binary (>= 0.2) without compiling it.
#
# `make tools` runs this when cargo-binstall fails. binstall first looks the
# crate's repository up on github.com and its API, which proxies and API rate
# limits can refuse even though the release download itself works. This
# script reads the version from the crates.io index and downloads the same
# release archive binstall would.
set -eu

repo=https://github.com/robinvandernoord/cargo-plane
index=https://index.crates.io/ca/rg/cargo-plane
bin_dir="${CARGO_HOME:-$HOME/.cargo}/bin"

target=$(rustc -vV | sed -n 's/^host: //p')
if [ -z "$target" ]; then
    echo "install-cargo-plane: could not read the Rust host target from rustc -vV" >&2
    exit 1
fi

# The newest version that is not yanked or a pre-release, from 0.2 on.
listing=$(curl --proto '=https' --tlsv1.2 -sSf "$index") || {
    echo "install-cargo-plane: could not read $index" >&2
    exit 1
}
version=$(printf '%s\n' "$listing" |
    grep '"yanked":false' |
    sed -n 's/.*"vers":"\([0-9][0-9.]*\)".*/\1/p' |
    grep -v '^0\.[01]\.' |
    sort -V | tail -n 1)
if [ -z "$version" ]; then
    echo "install-cargo-plane: no cargo-plane release >= 0.2 in the crates.io index" >&2
    exit 1
fi

url="$repo/releases/download/v$version/cargo-plane-$target-v$version.tgz"
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

echo "Downloading cargo-plane $version for $target"
if http_status=$(curl --proto '=https' --tlsv1.2 -sSfL -w '%{http_code}' -o "$work/cargo-plane.tgz" "$url"); then
    :
else
    curl_status=$?
    if [ "$curl_status" -eq 22 ] && [ "$http_status" = 404 ]; then
        echo "install-cargo-plane: no binary at $url" >&2
        echo "Wait for a release for this platform; compiling from source is disabled." >&2
    else
        echo "install-cargo-plane: could not download $url" >&2
    fi
    exit 1
fi
tar -xzf "$work/cargo-plane.tgz" -C "$work"
if [ ! -f "$work/cargo-plane" ]; then
    echo "install-cargo-plane: the archive has no cargo-plane binary" >&2
    exit 1
fi
mkdir -p "$bin_dir"
install -m 755 "$work/cargo-plane" "$bin_dir/cargo-plane"
installed=$("$bin_dir/cargo-plane" --version) || {
    echo "install-cargo-plane: the downloaded binary does not run on this system" >&2
    exit 1
}
echo "Installed $installed in $bin_dir"
