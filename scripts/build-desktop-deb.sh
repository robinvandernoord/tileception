#!/bin/sh
# Builds the two packages behind the premade ISO's desktop, once per suite:
#
#   scripts/build-desktop-deb.sh trixie sid noble resolute
#
#   target/deb/<suite>/tileception-desktop_<version>~<suite>_all.deb
#   target/deb/<suite>/tileception-desktop-sources_<version>~<suite>_all.deb
#
# tileception-desktop depends on and recommends what
# packaging/desktop/packages.txt lists, and adds:
#   /etc/xdg/tileception-xdg-terminals.list   the terminal for Tileception
#                                             sessions (Ghostty, else GNOME
#                                             Terminal)
#   /etc/xdg/tileception-mimeapps.list        what opens folders, text,
#                                             documents, images, video,
#                                             music and archives there
#   /usr/bin/bat, /usr/bin/fd                 links to Ubuntu's batcat and
#                                             fdfind (the names clash with
#                                             bacula-console-qt and fdclone,
#                                             which it therefore conflicts with)
#
# tileception-desktop-sources holds the keys of Mozilla's and GitHub CLI's
# APT repositories, whose packages leave their sources to the administrator.
# On its first install it adds a source for each that has none yet, and pins
# Mozilla's Firefox over Ubuntu's snap package; removing it removes those
# sources. Vivaldi is not here: its own package installs and maintains its
# source and keys.
#
# The keys, with the fingerprints their publishers list:
#   packaging/desktop/keys/mozilla.gpg     35BAA0B33E9EB396F59CA838C0BA5CE6DC6315A3
#   packaging/desktop/keys/github-cli.gpg  2C6106201985B60E6C7AC87323F3D4EA75716059
#                                          7F38BBB59D064DBCB3D84D725612B36462313325
#
# --control prints both packages' control fields for each suite instead of
# building, to check the relations with apt without a build.
#
# Environment: DEB_VERSION, DEB_MAINTAINER, DEB_OUT and SOURCE_DATE_EPOCH as
# for build-deb.sh; the suite is appended to the version as ~<suite>.
set -eu
cd "$(dirname "$0")/.."

control_only=0
if [ "${1:-}" = --control ]; then
    control_only=1
    shift
fi
if [ $# -eq 0 ]; then
    echo "usage: scripts/build-desktop-deb.sh [--control] SUITE... (e.g. noble resolute)" >&2
    exit 2
fi
[ "$control_only" = 1 ] || command -v dpkg-deb >/dev/null 2>&1 \
    || { echo "build-desktop-deb: dpkg-deb is missing" >&2; exit 1; }

list=packaging/desktop/packages.txt
base_version=$(scripts/deb-version.sh)
maintainer="${DEB_MAINTAINER:-$(git log -1 --format='%an <%ae>' 2>/dev/null || true)}"
if [ -z "$maintainer" ]; then
    echo "build-desktop-deb: set DEB_MAINTAINER (no git history to take it from)" >&2
    exit 1
fi
SOURCE_DATE_EPOCH="${SOURCE_DATE_EPOCH:-$(git log -1 --format=%ct 2>/dev/null || date +%s)}"
export SOURCE_DATE_EPOCH

# The relations of one group in packages.txt, comma-separated.
relations() {
    awk -v group="$1" '
        /^\[/ { g = substr($0, 2, length($0) - 2); next }
        { sub(/#.*/, ""); gsub(/^[[:space:]]+|[[:space:]]+$/, "") }
        $0 == "" { next }
        g == group { out = out (out == "" ? "" : ", ") $0 }
        END { print out }
    ' "$list"
}
depends=$(relations depends)
recommends=$(relations recommends)

# Shared parts of both packages: docs, changelog, md5sums and the build.
start() { # <package>
    root="$out/staging-desktop/$1"
    rm -rf "$root"
    mkdir -p "$root/DEBIAN"
    install -D -m 644 packaging/copyright "$root/usr/share/doc/$1/copyright"
    printf '%s (%s) unstable; urgency=medium\n\n  * Built from %s.\n\n -- %s  %s\n' \
        "$1" "$version" "$(git rev-parse --short HEAD 2>/dev/null || echo 'a source tree')" \
        "$maintainer" "$(date -R -d "@$SOURCE_DATE_EPOCH")" \
        | gzip -9n > "$root/usr/share/doc/$1/changelog.Debian.gz"
}
finish() { # <package>
    if [ "$control_only" = 1 ]; then
        cat "$root/DEBIAN/control"
        echo
        return
    fi
    (cd "$root" && find . -path ./DEBIAN -prune -o -type f -print | sed 's|^\./||' | sort \
        | xargs -r md5sum > DEBIAN/md5sums)
    deb="$out/${1}_${version}_all.deb"
    dpkg-deb --root-owner-group -Zxz --build "$root" "$deb"
    echo "Built $deb"
}
installed_size() { du -sk --exclude=DEBIAN "$root" | cut -f1; }

for suite in "$@"; do
    version="$base_version~$suite"
    out="${DEB_OUT:-target/deb}/$suite"
    if [ "$control_only" = 1 ]; then
        out=$(mktemp -d)
    fi

    # ---------------------------------------------------------- the desktop
    package=tileception-desktop
    start "$package"
    install -D -m 644 packaging/desktop/tileception-xdg-terminals.list \
        "$root/etc/xdg/tileception-xdg-terminals.list"
    install -D -m 644 packaging/desktop/tileception-mimeapps.list \
        "$root/etc/xdg/tileception-mimeapps.list"
    mkdir -p "$root/usr/bin"
    ln -s batcat "$root/usr/bin/bat"
    ln -s fdfind "$root/usr/bin/fd"
    cat > "$root/DEBIAN/control" <<EOF
Package: $package
Version: $version
Architecture: all
Maintainer: $maintainer
Installed-Size: $(installed_size)
Depends: $depends
Recommends: $recommends
Conflicts: bacula-console-qt, fdclone
Section: metapackages
Priority: optional
Homepage: https://tileception.space
Description: Tileception desktop with its default apps and services
 Turns a minimal Debian or Ubuntu system into the desktop of the premade
 Tileception ISO: the compositor with sound, network, Bluetooth, power,
 printing and keyring services, fonts, and a set of GNOME applications,
 media players, LibreOffice and command-line tools. The applications are
 recommendations, so any of them can be removed.
EOF
    echo /etc/xdg/tileception-xdg-terminals.list > "$root/DEBIAN/conffiles"
    finish "$package"

    # ---------------------------------------------------------- the sources
    package=tileception-desktop-sources
    start "$package"
    install -D -m 644 packaging/desktop/keys/mozilla.gpg \
        "$root/usr/share/keyrings/tileception-desktop-mozilla.gpg"
    install -D -m 644 packaging/desktop/keys/github-cli.gpg \
        "$root/usr/share/keyrings/tileception-desktop-github-cli.gpg"
    # Only Mozilla's packages, and only from that repository, win over
    # Ubuntu's: its `firefox` is a transitional package for the snap.
    install -d "$root/etc/apt/preferences.d"
    printf '%s\n' 'Package: *' 'Pin: origin packages.mozilla.org' 'Pin-Priority: 1000' \
        > "$root/etc/apt/preferences.d/tileception-desktop-mozilla"
    cat > "$root/DEBIAN/control" <<EOF
Package: $package
Version: $version
Architecture: all
Maintainer: $maintainer
Installed-Size: $(installed_size)
Section: misc
Priority: optional
Multi-Arch: foreign
Homepage: https://tileception.space
Description: APT sources for the Tileception desktop's browsers and tools
 Adds the APT repositories of Mozilla (Firefox as a deb rather than a snap)
 and GitHub CLI with their signing keys, unless a source for them exists
 already, and prefers Mozilla's Firefox over Ubuntu's snap package.
 Removing this package removes the sources it added.
EOF
    echo /etc/apt/preferences.d/tileception-desktop-mozilla > "$root/DEBIAN/conffiles"

    cat > "$root/DEBIAN/postinst" <<'EOF'
#!/bin/sh
set -e
dir=/etc/apt/sources.list.d

# Adds a source named tileception-desktop-<name>, unless one for its
# address exists: an administrator's own setup stays as it is.
add() { # <name> <uri> <suite>
    file="$dir/tileception-desktop-$1.sources"
    [ -e "$file" ] && return 0
    if grep -rqsF -- "$2" "$dir"; then
        return 0
    fi
    printf 'Types: deb\nURIs: %s\nSuites: %s\nComponents: main\nSigned-By: %s\n' \
        "$2" "$3" "/usr/share/keyrings/tileception-desktop-$1.gpg" > "$file.dpkg-new"
    chmod 644 "$file.dpkg-new"
    mv "$file.dpkg-new" "$file"
}

# On the first install only, so a source the administrator removed stays
# removed.
if [ "$1" = configure ] && [ -z "$2" ]; then
    add mozilla https://packages.mozilla.org/apt mozilla
    add github-cli https://cli.github.com/packages stable
fi
EOF
    cat > "$root/DEBIAN/postrm" <<'EOF'
#!/bin/sh
set -e
# apt cannot use the sources once their keys are gone.
case "$1" in
    remove|purge)
        rm -f /etc/apt/sources.list.d/tileception-desktop-mozilla.sources \
            /etc/apt/sources.list.d/tileception-desktop-github-cli.sources
        ;;
esac
EOF
    chmod 755 "$root/DEBIAN/postinst" "$root/DEBIAN/postrm"
    finish "$package"

    rm -rf "$out/staging-desktop"
    [ "$control_only" = 0 ] || rm -rf "$out"
done
