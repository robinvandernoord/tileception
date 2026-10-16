#!/bin/sh
# Builds tileception-archive-keyring, the package that owns the apt
# repository's signing key and source, once per suite:
#
#   scripts/build-keyring-deb.sh trixie sid noble resolute
#
#   target/deb/<suite>/tileception-archive-keyring_<version>~<suite>_all.deb
#
# It installs the key at /usr/share/keyrings/tileception-archive-keyring.gpg
# and, from its maintainer scripts, keeps /etc/apt/sources.list.d/
# tileception.sources pointing at it: an existing source keeps its URIs and
# suite (a mirror, or the suite a derivative maps to), and the key the
# installer used to put in /etc/apt/keyrings is removed. Removing the package
# removes the source too, since apt cannot use it without the key.
#
# The key comes from packaging/tileception-archive-keyring.gpg. To rotate it,
# add the new key to that file next to the old one and release; switch the
# repository to signing with the new key once that release is out; drop the
# old key in a later release.
#
# Environment: DEB_VERSION, DEB_MAINTAINER, DEB_OUT and SOURCE_DATE_EPOCH as
# for build-deb.sh; the suite is appended to the version as ~<suite>.
set -eu
cd "$(dirname "$0")/.."

if [ $# -eq 0 ]; then
    echo "usage: scripts/build-keyring-deb.sh SUITE... (e.g. trixie noble)" >&2
    exit 2
fi
command -v dpkg-deb >/dev/null 2>&1 || { echo "build-keyring-deb: dpkg-deb is missing" >&2; exit 1; }

package=tileception-archive-keyring
key=packaging/tileception-archive-keyring.gpg
cargo_version=$(sed -n '/^\[workspace.package\]/,/^\[/s/^version *= *"\(.*\)"/\1/p' Cargo.toml)
base_version="${DEB_VERSION:-$cargo_version-1}"
maintainer="${DEB_MAINTAINER:-$(git log -1 --format='%an <%ae>' 2>/dev/null || true)}"
if [ -z "$maintainer" ]; then
    echo "build-keyring-deb: set DEB_MAINTAINER (no git history to take it from)" >&2
    exit 1
fi
SOURCE_DATE_EPOCH="${SOURCE_DATE_EPOCH:-$(git log -1 --format=%ct 2>/dev/null || date +%s)}"
export SOURCE_DATE_EPOCH

for suite in "$@"; do
    version="$base_version~$suite"
    out="${DEB_OUT:-target/deb}/$suite"
    root="$out/staging-keyring/$package"
    rm -rf "$root"
    mkdir -p "$root/DEBIAN"

    install -D -m 644 "$key" "$root/usr/share/keyrings/$package.gpg"
    install -D -m 644 packaging/copyright "$root/usr/share/doc/$package/copyright"
    printf '%s (%s) unstable; urgency=medium\n\n  * Built from %s.\n\n -- %s  %s\n' \
        "$package" "$version" "$(git rev-parse --short HEAD 2>/dev/null || echo 'a source tree')" \
        "$maintainer" "$(date -R -d "@$SOURCE_DATE_EPOCH")" \
        | gzip -9n > "$root/usr/share/doc/$package/changelog.Debian.gz"

    cat > "$root/DEBIAN/control" <<EOF
Package: $package
Version: $version
Architecture: all
Maintainer: $maintainer
Installed-Size: $(du -sk --exclude=DEBIAN "$root" | cut -f1)
Section: misc
Priority: optional
Multi-Arch: foreign
Homepage: https://github.com/robinvandernoord/tileception
Description: signing key and apt source for the Tileception repository
 Installs the key that signs https://apt.tileception.space and the apt
 source that uses it, so key changes arrive with normal updates. Removing
 this package also removes the source.
EOF

    cat > "$root/DEBIAN/postinst" <<EOF
#!/bin/sh
set -e
sources=/etc/apt/sources.list.d/tileception.sources
keyring=/usr/share/keyrings/$package.gpg
old_keyring=/etc/apt/keyrings/tileception.gpg
if [ "\$1" = configure ]; then
    uris=https://apt.tileception.space
    suites=$suite
    if [ -f "\$sources" ]; then
        uris=\$(sed -n 's/^URIs:[[:space:]]*//p' "\$sources" | head -n 1)
        suites=\$(sed -n 's/^Suites:[[:space:]]*//p' "\$sources" | head -n 1)
        [ -n "\$uris" ] || uris=https://apt.tileception.space
        [ -n "\$suites" ] || suites=$suite
    fi
    tmp="\$sources.dpkg-new"
    printf 'Types: deb\nURIs: %s\nSuites: %s\nComponents: main\nSigned-By: %s\n' \\
        "\$uris" "\$suites" "\$keyring" > "\$tmp"
    chmod 644 "\$tmp"
    mv "\$tmp" "\$sources"
    # The installer's copy, from before this package existed.
    if [ -f "\$old_keyring" ] && cmp -s "\$old_keyring" "\$keyring"; then
        rm -f "\$old_keyring"
    fi
fi
EOF

    cat > "$root/DEBIAN/postrm" <<'EOF'
#!/bin/sh
set -e
# apt cannot use the source once the key is gone.
case "$1" in
    remove|purge) rm -f /etc/apt/sources.list.d/tileception.sources ;;
esac
EOF
    chmod 755 "$root/DEBIAN/postinst" "$root/DEBIAN/postrm"

    (cd "$root" && find . -path ./DEBIAN -prune -o -type f -print | sed 's|^\./||' | sort \
        | xargs -r md5sum > DEBIAN/md5sums)
    deb="$out/${package}_${version}_all.deb"
    dpkg-deb --root-owner-group -Zxz --build "$root" "$deb"
    rm -rf "$out/staging-keyring"
    echo "Built $deb"
done
