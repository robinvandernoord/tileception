#!/bin/sh
# Builds a premade Tileception ISO from Ubuntu's Desktop ISO:
#
#   iso/build.sh resolute amd64 [OUT]
#
# Fetches the newest Desktop ISO of that release and architecture
# (iso/latest.sh), checks it against Ubuntu's signed SHA256SUMS, and writes
# OUT (default target/iso):
#
#   tileception-ubuntu-<version>-<arch>.iso   Ubuntu's ISO plus autoinstall.yaml,
#                                             iso/tileception/ and our boot menu
#   tileception-ubuntu-<version>-<arch>.json  its entry for manifest.json
#
# Nothing of Tileception itself is on the ISO: the installer fetches the
# current packages, so a new ISO is only needed for a new Ubuntu ISO or a
# change under iso/. Downloads are kept in ISO_CACHE (default
# ~/.cache/tileception-iso) and reused while their checksum matches.
#
# For the smoke test (iso/smoke-test.sh): AUTOINSTALL replaces
# iso/autoinstall.yaml, BOOT_ARGS is added to the kernel command line, and
# ISO_SUFFIX to the file name.
#
# Needs curl, gpg, gpgv, xorriso, and Ubuntu's archive keyring (ubuntu-keyring),
# which signs the release checksums. distro-info-data, when installed, adds
# LTS and end of support to the entry.
set -eu
cd "$(dirname "$0")/.."

codename=${1:?usage: iso/build.sh CODENAME ARCH [OUT]}
arch=${2:?usage: iso/build.sh CODENAME ARCH [OUT]}
out=${3:-target/iso}
cache=${ISO_CACHE:-$HOME/.cache/tileception-iso}
keyring=/usr/share/keyrings/ubuntu-archive-keyring.gpg
# Vivaldi's signing key, as its package names it (vivaldi-16BD9233.gpg). When
# Vivaldi moves to its next key, the build stops here until this is updated.
vivaldi_fingerprint=8D1FA52AEF58A09D889DD4221256C34716BD9233

fail() { echo "iso/build.sh: $*" >&2; exit 1; }
for tool in curl gpg gpgv xorriso sha256sum; do
    command -v "$tool" >/dev/null 2>&1 || fail "$tool is missing"
done
[ -r "$keyring" ] || fail "$keyring is missing (install ubuntu-keyring)"


work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
mkdir -p "$cache" "$out"

# ------------------------------------------------------------ Ubuntu's ISO
latest=$(iso/latest.sh "$codename" "$arch")
url=${latest% *}
sum=${latest#* }
name=${url##*/}
version=$(echo "$name" | sed -E 's/^ubuntu-([0-9.]+)-desktop-.*/\1/')
ubuntu_iso="$cache/$name"
if [ ! -e "$ubuntu_iso" ] || ! echo "$sum  $ubuntu_iso" | sha256sum -c --status; then
    echo "Downloading $name"
    curl -fL --retry 5 --retry-all-errors --connect-timeout 30 -o "$ubuntu_iso.part" "$url"
    mv "$ubuntu_iso.part" "$ubuntu_iso"
    echo "$sum  $ubuntu_iso" | sha256sum -c --status || fail "$name does not match Ubuntu's checksum"
fi

# ------------------------------------------------------------ what we add
stage="$work/stage"
mkdir -p "$stage/tileception"
cp "${AUTOINSTALL:-iso/autoinstall.yaml}" "$stage/autoinstall.yaml"
cp iso/tileception/setup.sh iso/tileception/tileception-iso-setup.service "$stage/tileception/"
cp packaging/tileception-archive-keyring.gpg "$stage/tileception/"

curl -fsSL --retry 5 --retry-all-errors --connect-timeout 30 https://repo.vivaldi.com/stable/linux_signing_key.pub -o "$work/vivaldi.asc"
gpg --batch --dearmor < "$work/vivaldi.asc" > "$stage/tileception/vivaldi-16BD9233.gpg"
curl -fsSL --retry 5 --retry-all-errors --connect-timeout 30 https://repo.vivaldi.com/stable/deb/dists/stable/Release -o "$work/Release"
curl -fsSL --retry 5 --retry-all-errors --connect-timeout 30 https://repo.vivaldi.com/stable/deb/dists/stable/Release.gpg -o "$work/Release.gpg"
gpgv --status-fd 1 --keyring "$stage/tileception/vivaldi-16BD9233.gpg" "$work/Release.gpg" "$work/Release" 2>/dev/null \
    | grep -q "VALIDSIG $vivaldi_fingerprint" \
    || fail "Vivaldi's repository is not signed with key $vivaldi_fingerprint"

# The boot menu says what this ISO installs.
xorriso -osirrox on -indev "$ubuntu_iso" -extract /boot/grub/grub.cfg "$work/grub.cfg" 2>/dev/null
chmod u+w "$work/grub.cfg"
sed -i -e 's/Try or Install Ubuntu/Try Ubuntu or Install Tileception/' \
    -e 's/"Ubuntu (safe graphics)"/"Try Ubuntu or Install Tileception (safe graphics)"/' "$work/grub.cfg"
grep -q "Install Tileception" "$work/grub.cfg" || fail "the boot menu of $name has no entry to rename"
if [ -n "${BOOT_ARGS:-}" ]; then
    sed -i -E -e "s|^([[:space:]]*linux[[:space:]]+/casper/vmlinuz)|\\1 $BOOT_ARGS|" \
        -e 's/^set timeout=.*/set timeout=1/' "$work/grub.cfg"
fi

# md5sum.txt is what "Check disc for defects" compares against.
xorriso -osirrox on -indev "$ubuntu_iso" -extract /md5sum.txt "$work/md5sum.txt" 2>/dev/null
chmod u+w "$work/md5sum.txt"
{
    grep -v -e ' \./boot/grub/grub\.cfg$' -e ' \./autoinstall\.yaml$' -e ' \./tileception/' "$work/md5sum.txt"
    (cd "$stage" && find . -type f | sort | xargs md5sum)
    (cd "$work" && md5sum grub.cfg | sed 's| grub\.cfg$| ./boot/grub/grub.cfg|')
} > "$work/md5sum.new"

# ------------------------------------------------------------ the ISO
iso="tileception-ubuntu-$version-$arch${ISO_SUFFIX:-}.iso"
rm -f "$out/$iso"
xorriso -indev "$ubuntu_iso" -outdev "$out/$iso" \
    -map "$stage/autoinstall.yaml" /autoinstall.yaml \
    -map "$stage/tileception" /tileception \
    -map "$work/grub.cfg" /boot/grub/grub.cfg \
    -map "$work/md5sum.new" /md5sum.txt \
    -boot_image any replay 2>&1 | grep -E "^(xorriso : (FAILURE|SORRY)|libisofs: (FAILURE|SORRY))" && fail "xorriso failed"
[ -s "$out/$iso" ] || fail "xorriso wrote no ISO"

# ------------------------------------------------------------ its entry
lts=false
eol=""
csv=/usr/share/distro-info/ubuntu.csv
if [ -r "$csv" ]; then
    row=$(awk -F, -v c="$codename" '$3 == c' "$csv")
    case $(echo "$row" | cut -d, -f1) in *LTS*) lts=true ;; esac
    eol=$(echo "$row" | cut -d, -f6)
fi
cat > "$out/${iso%.iso}.json" <<EOF
{
  "file": "$iso",
  "ubuntu": "$version",
  "codename": "$codename",
  "arch": "$arch",
  "lts": $lts,
  "eol": "$eol",
  "size": $(stat -c %s "$out/$iso"),
  "sha256": "$(sha256sum "$out/$iso" | cut -d' ' -f1)",
  "from": "$name",
  "from_sha256": "$sum",
  "recipe": "$(iso/recipe.sh)",
  "built": "$(date -u +%Y-%m-%d)"
}
EOF
echo "Built $out/$iso"
