#!/bin/sh
# Builds the packages inside a clean container of each given distribution, so
# each .deb links against, and depends on, that release's libraries:
#
#   scripts/build-deb-docker.sh debian:trixie ubuntu:24.04
#
# Packages land in target/deb/<codename>/, versioned <version>~<codename> so
# builds for different releases can share one apt repository. Architecture is
# the host's (pass DOCKER_PLATFORM=linux/arm64 with qemu binfmt for another).
#
# Environment: DEB_VERSION and DEB_MAINTAINER as for build-deb.sh,
# RUST_TOOLCHAIN (default: stable), and DOCKER_ARGS for extra `docker run`
# options (a proxy, a CA certificate, a cargo registry cache).
set -eu
cd "$(dirname "$0")/.."

if [ $# -eq 0 ]; then
    echo "usage: scripts/build-deb-docker.sh IMAGE... (e.g. debian:trixie ubuntu:24.04)" >&2
    exit 2
fi

cargo_version=$(sed -n '/^\[workspace.package\]/,/^\[/s/^version *= *"\(.*\)"/\1/p' Cargo.toml)
version="${DEB_VERSION:-$cargo_version-1}"
maintainer="${DEB_MAINTAINER:-$(git log -1 --format='%an <%ae>')}"
epoch="${SOURCE_DATE_EPOCH:-$(git log -1 --format=%ct)}"

for image in "$@"; do
    echo "== $image"
    # The build dependencies come from deps/debian.txt, like `make deps`.
    build_deps=$(awk '/^\[/ { g = $0; next } /^[[:space:]]*(#|$)/ { next } g == "[build]" { print $1 }' deps/debian.txt | paste -sd' ' -)
    # shellcheck disable=SC2086 # DOCKER_ARGS is a list of options
    docker run --rm ${DOCKER_PLATFORM:+--platform "$DOCKER_PLATFORM"} ${DOCKER_ARGS:-} \
        -v "$(pwd):/src" -w /src \
        -e DEB_MAINTAINER="$maintainer" -e SOURCE_DATE_EPOCH="$epoch" \
        -e RUST_TOOLCHAIN="${RUST_TOOLCHAIN:-stable}" \
        -e HOST_UID="$(id -u)" -e HOST_GID="$(id -g)" \
        -e DEB_VERSION_BASE="$version" -e BUILD_DEPS="$build_deps" \
        "$image" sh -euc '
            export DEBIAN_FRONTEND=noninteractive
            apt-get update -qq
            apt-get install -qq -y --no-install-recommends \
                ca-certificates curl git binutils dpkg-dev gcc libc6-dev $BUILD_DEPS >/dev/null
            curl -sSf -o /tmp/rustup-init.sh https://sh.rustup.rs
            sh /tmp/rustup-init.sh -y -q --profile minimal --default-toolchain "$RUST_TOOLCHAIN"
            . "$HOME/.cargo/env"
            git config --global --add safe.directory /src
            . /etc/os-release
            codename="${VERSION_CODENAME:?the image has no VERSION_CODENAME}"
            export CARGO_TARGET_DIR="/src/target/docker-$codename"
            export DEB_OUT="/src/target/deb/$codename"
            export DEB_VERSION="$DEB_VERSION_BASE~$codename"
            scripts/build-deb.sh
            chown "$HOST_UID:$HOST_GID" /src/target/deb
            chown -R "$HOST_UID:$HOST_GID" "$CARGO_TARGET_DIR" "$DEB_OUT"
        '
done
