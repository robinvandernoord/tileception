#!/bin/sh
# The releases Tileception has packages for, one "<docker image> <suite>" line
# each: every Debian and Ubuntu release that distro-info calls supported and
# that has been released, from Debian 13 and Ubuntu 24.04 on, and Debian
# unstable. A new release joins on its release day and an old one leaves at
# its end of life, once the distro-info-data package knows about them.
#
#   scripts/suites.sh            image and suite per line
#   scripts/suites.sh ubuntu     only Ubuntu's suites, on one line
#   scripts/suites.sh debian     only Debian's suites, on one line
#   scripts/suites.sh images     only the images, on one line
#   scripts/suites.sh matrix     release.yml's build matrix, as JSON
#
# Needs distro-info (and jq for matrix).
set -eu

# Oldest releases built for: older ones lack libraries Tileception needs.
min_debian=13
min_ubuntu=24.04

released() { # <ubuntu|debian> <series>: has the release day passed?
    days=$("$1-distro-info" --series="$2" --days=release 2>/dev/null) || return 1
    # "(unknown)" for Debian's testing, unstable and experimental.
    case $days in -* | 0) ;; *) return 1 ;; esac
}

at_least() { # <version> <minimum>
    [ "$(printf '%s\n%s\n' "$1" "$2" | sort -V | head -n 1)" = "$2" ]
}

list() {
    for series in $(debian-distro-info --supported); do
        released debian "$series" || continue
        version=$(debian-distro-info --series="$series" -r)
        at_least "$version" "$min_debian" && echo "debian:$series $series"
    done
    for series in $(ubuntu-distro-info --supported); do
        released ubuntu "$series" || continue
        version=$(ubuntu-distro-info --series="$series" -r | cut -d' ' -f1)
        at_least "$version" "$min_ubuntu" && echo "ubuntu:$version $series"
    done
    echo "debian:sid sid"
}

case "${1:-}" in
    "") list ;;
    ubuntu | debian) list | awk -v d="$1:" 'index($1, d) == 1 { print $2 }' | paste -sd' ' ;;
    images) list | awk '{ print $1 }' | paste -sd' ' ;;
    matrix) list | jq -Rnc '[inputs | split(" ") | { image: .[0], suite: .[1] }]' ;;
    *)
        echo "usage: scripts/suites.sh [ubuntu|debian|images|matrix]" >&2
        exit 2
        ;;
esac
