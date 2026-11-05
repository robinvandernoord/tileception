#!/bin/sh
# Checks or installs the system packages listed in deps/debian.txt.
#
#   scripts/deps.sh check [group...]     report what is missing (exit 1 if any)
#   scripts/deps.sh install [group...]   install what is missing, after asking
#   scripts/deps.sh list [group...]      print the package names (for CI)
#
# Groups are build, runtime and optional; without any, all three. video
# (the promo video's tools) is only checked when asked for.
set -eu
cd "$(dirname "$0")/.."

list=deps/debian.txt
mode="${1:-check}"
[ $# -gt 0 ] && shift
groups="${*:-build runtime optional}"

if ! command -v dpkg-query >/dev/null 2>&1; then
    echo "This checker knows Debian, Ubuntu and Mint (dpkg). On other systems, see $list for what to install." >&2
    exit 2
fi

# The packages of the requested groups, without comments.
packages() {
    awk -v groups=" $groups " '
        /^\[/ { g = substr($0, 2, length($0) - 2); next }
        /^[[:space:]]*(#|$)/ { next }
        index(groups, " " g " ") { print $1 }
    ' "$list"
}

if [ "$mode" = list ]; then
    packages
    exit 0
fi

# With other architectures added (cross builds), a package can be installed
# once per architecture; only this machine's own copy counts.
native=$(dpkg --print-architecture)
missing=""
for p in $(packages); do
    if ! dpkg-query -W -f='${Status} ${Architecture}\n' "$p" 2>/dev/null \
        | grep -qxE "install ok installed ($native|all)"; then
        missing="$missing $p"
    fi
done

if ! command -v cargo >/dev/null 2>&1; then
    echo "Rust is not installed: get it from https://rustup.rs (cargo is needed to build)."
    [ "$mode" = check ] && exit 1
fi

if [ -z "$missing" ]; then
    echo "All packages for: $groups"
    exit 0
fi

case "$mode" in
    check)
        echo "Missing packages ($groups):$missing"
        echo "Install them with:  make deps   (or: sudo apt install$missing)"
        exit 1
        ;;
    install)
        echo "Missing packages ($groups):$missing"
        printf "Install them with sudo apt install? [Y/n] "
        read -r answer
        case "$answer" in
            ""|y|Y|yes) sudo apt update && sudo apt install$missing ;;
            *) echo "Nothing installed."; exit 1 ;;
        esac
        ;;
    *)
        echo "usage: scripts/deps.sh check|install|list [build|runtime|optional|video ...]" >&2
        exit 2
        ;;
esac
