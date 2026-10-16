#!/bin/sh
# Runs inside a fresh Debian or Ubuntu container (see build-deb-docker.sh):
# installs the toolchain and build dependencies, then builds the packages for
# each architecture in DEB_ARCHS, cross-compiling where it is not the
# container's own.
#
# Expects the source at /src and DEB_ARCHS, DEB_VERSION_BASE, HOST_UID,
# HOST_GID and RUST_TOOLCHAIN in the environment.
set -eu
cd /src
export DEBIAN_FRONTEND=noninteractive
. /etc/os-release
codename="${VERSION_CODENAME:?the image has no VERSION_CODENAME}"
native=$(dpkg --print-architecture)

build_deps=$(awk '/^\[/ { g = $0; next } /^[[:space:]]*(#|$)/ { next } g == "[build]" { print $1 }' deps/debian.txt)
packages="ca-certificates curl git binutils dpkg-dev gcc libc6-dev $build_deps"
rust_targets=""

for arch in $DEB_ARCHS; do
    [ "$arch" = "$native" ] && continue
    dpkg --add-architecture "$arch"
    # pkg-config runs on the build machine; the libraries are the target's.
    for dep in $build_deps; do
        [ "$dep" = pkg-config ] || packages="$packages $dep:$arch"
    done
    packages="$packages crossbuild-essential-$arch"
    case "$arch" in
        amd64) rust_targets="$rust_targets x86_64-unknown-linux-gnu" ;;
        arm64) rust_targets="$rust_targets aarch64-unknown-linux-gnu" ;;
        armhf) rust_targets="$rust_targets armv7-unknown-linux-gnueabihf" ;;
    esac
    # Ubuntu serves amd64 and i386 from its main archive and every other
    # architecture from ports, so each source has to say which it carries.
    if [ "$ID" = ubuntu ]; then
        case "$arch" in
            amd64|i386) uri=http://archive.ubuntu.com/ubuntu/ ;;
            *) uri=http://ports.ubuntu.com/ubuntu-ports/ ;;
        esac
        if ! grep -q '^Architectures:' /etc/apt/sources.list.d/ubuntu.sources; then
            sed -i "/^Types:/a Architectures: $native" /etc/apt/sources.list.d/ubuntu.sources
        fi
        cat > "/etc/apt/sources.list.d/ubuntu-$arch.sources" <<EOF
Types: deb
URIs: $uri
Suites: $codename $codename-updates $codename-security
Components: main universe
Architectures: $arch
Signed-By: /usr/share/keyrings/ubuntu-archive-keyring.gpg
EOF
    fi
done

apt-get update -qq
# shellcheck disable=SC2086 # word-split package list
apt-get install -qq -y --no-install-recommends $packages >/dev/null

curl -sSf -o /tmp/rustup-init.sh https://sh.rustup.rs
sh /tmp/rustup-init.sh -y -q --profile minimal --default-toolchain "$RUST_TOOLCHAIN"
. "$HOME/.cargo/env"
for target in $rust_targets; do
    rustup target add "$target"
done
git config --global --add safe.directory /src

export CARGO_TARGET_DIR="/src/target/docker-$codename"
export DEB_OUT="/src/target/deb/$codename"
export DEB_VERSION="$DEB_VERSION_BASE~$codename"
for arch in $DEB_ARCHS; do
    echo "== $codename $arch"
    DEB_ARCH="$arch" scripts/build-deb.sh
done
rm -rf "$DEB_OUT"/staging-*

chown "$HOST_UID:$HOST_GID" /src/target/deb
chown -R "$HOST_UID:$HOST_GID" "$CARGO_TARGET_DIR" "$DEB_OUT"
