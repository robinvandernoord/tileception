#!/bin/sh
# Builds the packages inside a clean container of each given distribution, so
# each .deb links against, and depends on, that release's libraries:
#
#   scripts/build-deb-docker.sh debian:trixie debian:sid ubuntu:24.04 ubuntu:26.04
#
# Packages land in target/deb/<codename>/, versioned <version>~<codename> so
# builds for different releases can share one apt repository. Each image
# builds every architecture in DEB_ARCHS (default: amd64 arm64), each in its
# own container; those that are not the host's are cross-compiled, which is
# much faster than emulation.
#
# Environment: DEB_VERSION and DEB_MAINTAINER as for build-deb.sh, DEB_ARCHS,
# DEB_SUITE (override the image's codename; debian:sid defaults to sid),
# RUST_TOOLCHAIN (default: stable), and DOCKER_ARGS for extra `docker run`
# options (a proxy, a CA certificate, a cargo registry cache).
set -eu
cd "$(dirname "$0")/.."

if [ $# -eq 0 ]; then
    echo "usage: scripts/build-deb-docker.sh IMAGE... (e.g. debian:trixie ubuntu:24.04)" >&2
    exit 2
fi

version=$(scripts/deb-version.sh)
maintainer="${DEB_MAINTAINER:-$(git log -1 --format='%an <%ae>')}"
epoch="${SOURCE_DATE_EPOCH:-$(git log -1 --format=%ct)}"

for image in "$@"; do
    suite="${DEB_SUITE:-}"
    if [ -z "$suite" ]; then
        case "$image" in
            debian:sid) suite=sid ;;
        esac
    fi
    for arch in ${DEB_ARCHS:-amd64 arm64}; do
        echo "== $image $arch"
        # shellcheck disable=SC2086 # DOCKER_ARGS is a list of options
        docker run --rm ${DOCKER_ARGS:-} \
            -v "$(pwd):/src" \
            -e DEB_MAINTAINER="$maintainer" -e SOURCE_DATE_EPOCH="$epoch" \
            -e DEB_VERSION_BASE="$version" -e DEB_ARCH="$arch" \
            -e DEB_SUITE="$suite" \
            -e RUST_TOOLCHAIN="${RUST_TOOLCHAIN:-stable}" \
            -e HOST_UID="$(id -u)" -e HOST_GID="$(id -g)" \
            "$image" /src/scripts/build-deb-container.sh
    done
done
