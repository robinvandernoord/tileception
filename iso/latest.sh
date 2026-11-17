#!/bin/sh
# Prints the newest Ubuntu Desktop ISO of a release and architecture, as
# "<url> <sha256>", after checking Ubuntu's signature on its SHA256SUMS:
#
#   iso/latest.sh resolute amd64
#
# amd64 ISOs are on releases.ubuntu.com, the others on cdimage.ubuntu.com.
# Needs curl, gpgv and Ubuntu's archive keyring (ubuntu-keyring).
set -eu

codename=${1:?usage: iso/latest.sh CODENAME ARCH}
arch=${2:?usage: iso/latest.sh CODENAME ARCH}
keyring=/usr/share/keyrings/ubuntu-archive-keyring.gpg

case $arch in
    amd64) base="https://releases.ubuntu.com/$codename" ;;
    *) base="https://cdimage.ubuntu.com/releases/$codename/release" ;;
esac

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
curl -fsSL "$base/SHA256SUMS" -o "$work/SHA256SUMS"
curl -fsSL "$base/SHA256SUMS.gpg" -o "$work/SHA256SUMS.gpg"
gpgv --keyring "$keyring" "$work/SHA256SUMS.gpg" "$work/SHA256SUMS" 2>/dev/null || {
    echo "iso/latest.sh: $base/SHA256SUMS is not signed by Ubuntu" >&2
    exit 1
}
line=$(grep -E "\*ubuntu-[0-9.]+-desktop-$arch\.iso\$" "$work/SHA256SUMS" | sort -t- -k2,2V | tail -n 1)
[ -n "$line" ] || { echo "iso/latest.sh: no Desktop ISO for $arch at $base" >&2; exit 1; }
echo "$base/${line#*\*} ${line%% *}"
