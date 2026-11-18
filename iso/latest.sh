#!/bin/sh
# Prints the newest Ubuntu Desktop ISO of a release and architecture, as
# "<url> <sha256>", after checking Ubuntu's signature on its SHA256SUMS:
#
#   iso/latest.sh resolute amd64
#
# amd64 ISOs are on releases.ubuntu.com, the others on cdimage.ubuntu.com.
# Exits with 3 when Ubuntu publishes no Desktop ISO there, and with 1 for any
# other failure. Ubuntu's servers do not always answer CI runners at once,
# hence the retries. Needs curl, gpgv and Ubuntu's archive keyring (ubuntu-keyring).
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
status=$(curl -sSL --retry 5 --retry-all-errors --connect-timeout 30 -o "$work/SHA256SUMS" -w '%{http_code}' \
    "$base/SHA256SUMS") || { echo "iso/latest.sh: could not fetch $base/SHA256SUMS" >&2; exit 1; }
case $status in
    200) ;;
    404) echo "iso/latest.sh: Ubuntu publishes no $codename ISOs at $base" >&2; exit 3 ;;
    *) echo "iso/latest.sh: $base/SHA256SUMS answered $status" >&2; exit 1 ;;
esac
curl -fsSL --retry 5 --retry-all-errors --connect-timeout 30 "$base/SHA256SUMS.gpg" -o "$work/SHA256SUMS.gpg"
gpgv --keyring "$keyring" "$work/SHA256SUMS.gpg" "$work/SHA256SUMS" 2>/dev/null || {
    echo "iso/latest.sh: $base/SHA256SUMS is not signed by Ubuntu" >&2
    exit 1
}
line=$(grep -E "\*ubuntu-[0-9.]+-desktop-$arch\.iso\$" "$work/SHA256SUMS" | sort -t- -k2,2V | tail -n 1)
[ -n "$line" ] || { echo "iso/latest.sh: no Desktop ISO for $arch at $base" >&2; exit 3; }
echo "$base/${line#*\*} ${line%% *}"
