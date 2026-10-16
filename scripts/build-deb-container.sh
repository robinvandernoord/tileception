#!/bin/sh
# Runs inside a fresh Debian or Ubuntu container (see build-deb-docker.sh):
# installs the toolchain and build dependencies, then builds the packages for
# DEB_ARCH, cross-compiling when that is not the container's own architecture.
# One architecture per container: some -dev packages (Ubuntu 24.04's
# libgudev-1.0-dev) cannot be installed for two architectures at once.
#
# Expects the source at /src and DEB_ARCH, DEB_VERSION_BASE, HOST_UID,
# HOST_GID and RUST_TOOLCHAIN in the environment.
set -eu
cd /src
export DEBIAN_FRONTEND=noninteractive
# shellcheck disable=SC1091 # the container's, not ours
. /etc/os-release
codename="${VERSION_CODENAME:?the image has no VERSION_CODENAME}"
native=$(dpkg --print-architecture)
arch="$DEB_ARCH"

build_deps=$(awk '/^\[/ { g = $0; next } /^[[:space:]]*(#|$)/ { next } g == "[build]" { print $1 }' deps/debian.txt)
packages="ca-certificates curl git binutils dpkg-dev gcc libc6-dev"
rust_target=""

if [ "$arch" = "$native" ]; then
    packages="$packages $build_deps"
else
    dpkg --add-architecture "$arch"
    # pkg-config runs on the build machine; the libraries are the target's.
    for dep in $build_deps; do
        if [ "$dep" = pkg-config ]; then
            packages="$packages $dep"
        else
            packages="$packages $dep:$arch"
        fi
    done
    # Some -dev packages (Ubuntu 24.04's libinput-dev, through glib) need
    # python3:any; without the native one apt picks the target's, whose
    # install scripts cannot run here.
    packages="$packages crossbuild-essential-$arch python3"
    case "$arch" in
        amd64) rust_target=x86_64-unknown-linux-gnu ;;
        arm64) rust_target=aarch64-unknown-linux-gnu ;;
        armhf) rust_target=armv7-unknown-linux-gnueabihf ;;
        *) echo "build-deb-container: no Rust target known for $arch" >&2; exit 1 ;;
    esac
    # Ubuntu serves amd64 and i386 from its main archive and every other
    # architecture from ports, so each source has to say which it carries.
    if [ "$ID" = ubuntu ]; then
        case "$arch" in
            amd64|i386) uri=http://archive.ubuntu.com/ubuntu/ ;;
            *) uri=http://ports.ubuntu.com/ubuntu-ports/ ;;
        esac
        sed -i "/^Types:/a Architectures: $native" /etc/apt/sources.list.d/ubuntu.sources
        cat > "/etc/apt/sources.list.d/ubuntu-$arch.sources" <<EOF
Types: deb
URIs: $uri
Suites: $codename $codename-updates $codename-security
Components: main universe
Architectures: $arch
Signed-By: /usr/share/keyrings/ubuntu-archive-keyring.gpg
EOF
    fi
fi

apt-get update -qq
# shellcheck disable=SC2086 # word-split package list
if ! apt-get install -qq -y --no-install-recommends $packages >/tmp/apt.log 2>&1; then
    grep -A3 -e '^dpkg: error' -e '^E:' /tmp/apt.log >&2 || tail -40 /tmp/apt.log >&2
    exit 1
fi

curl -sSf -o /tmp/rustup-init.sh https://sh.rustup.rs
sh /tmp/rustup-init.sh -y -q --profile minimal --default-toolchain "$RUST_TOOLCHAIN"
# shellcheck disable=SC1091 # written by rustup just now
. "$HOME/.cargo/env"
if [ -n "$rust_target" ]; then
    rustup target add "$rust_target"
fi
git config --global --add safe.directory /src

export CARGO_TARGET_DIR="/src/target/docker-$codename-$arch"
export DEB_OUT="/src/target/deb/$codename"
export DEB_VERSION="$DEB_VERSION_BASE~$codename"
echo "== $codename $arch"
scripts/build-deb.sh
rm -rf "$DEB_OUT/staging-$arch"

chown "$HOST_UID:$HOST_GID" /src/target/deb
chown -R "$HOST_UID:$HOST_GID" "$CARGO_TARGET_DIR" "$DEB_OUT"
