#!/bin/sh
# Builds the Debian/Ubuntu packages for this machine's architecture:
#
#   target/deb/tileception_<version>_<arch>.deb          the session
#   target/deb/tileception-dbgsym_<version>_<arch>.deb    its debug symbols
#
# The compositor is built with the `dist` profile (fat LTO, one codegen unit),
# then its debug info is split off into the -dbgsym package and the shipped
# binary is stripped. Library dependencies come from dpkg-shlibdeps, so the
# package asks for the library versions of the system it was built on: build
# on the oldest release you want to support (scripts/build-deb-docker.sh).
#
# Environment:
#   DEB_VERSION     full package version (default: Cargo version + "-1")
#   DEB_MAINTAINER  Maintainer field (default: the last commit's author)
#   DEB_OUT         where the packages go (default: target/deb)
#   CARGO_TARGET_DIR is honoured as usual.
set -eu
cd "$(dirname "$0")/.."

for tool in cargo dpkg-deb dpkg-shlibdeps objcopy readelf; do
    if ! command -v "$tool" >/dev/null 2>&1; then
        echo "build-deb: $tool is missing (dpkg-dev and binutils provide the non-cargo ones)" >&2
        exit 1
    fi
done
if ! scripts/deps.sh check build; then
    echo "Run 'make deps' first, then try again." >&2
    exit 1
fi

package=tileception
cargo_version=$(sed -n '/^\[workspace.package\]/,/^\[/s/^version *= *"\(.*\)"/\1/p' Cargo.toml)
version="${DEB_VERSION:-$cargo_version-1}"
maintainer="${DEB_MAINTAINER:-$(git log -1 --format='%an <%ae>' 2>/dev/null || true)}"
if [ -z "$maintainer" ]; then
    echo "build-deb: set DEB_MAINTAINER (no git history to take it from)" >&2
    exit 1
fi
arch=$(dpkg --print-architecture)
# Reproducible timestamps: dpkg-deb clamps file times to this.
SOURCE_DATE_EPOCH="${SOURCE_DATE_EPOCH:-$(git log -1 --format=%ct 2>/dev/null || date +%s)}"
export SOURCE_DATE_EPOCH

target_dir="${CARGO_TARGET_DIR:-target}"
cargo build --locked --profile dist -p tileception

out="${DEB_OUT:-$target_dir/deb}"
root="$out/$package"
dbg="$out/$package-dbgsym"
rm -rf "$root" "$dbg"
mkdir -p "$root/DEBIAN" "$dbg/DEBIAN"

# Files. The same set install-session.sh puts in ~/.local, at system paths.
install -D -m 755 "$target_dir/dist/tileception" "$root/usr/bin/tileception"
for helper in tileception-session tileception-power tileception-screenshot; do
    install -D -m 755 "resources/$helper" "$root/usr/bin/$helper"
done
mkdir -p "$root/usr/share/wayland-sessions"
sed "s|@BINDIR@|/usr/bin|g" resources/tileception.desktop.in \
    > "$root/usr/share/wayland-sessions/tileception.desktop"
chmod 644 "$root/usr/share/wayland-sessions/tileception.desktop"
install -D -m 644 resources/tileception-portals.conf \
    "$root/usr/share/xdg-desktop-portal/tileception-portals.conf"
install -D -m 644 crates/tileception-auth/pam/tileception "$root/etc/pam.d/tileception"
install -D -m 644 packaging/copyright "$root/usr/share/doc/$package/copyright"
install -D -m 644 README.md "$root/usr/share/doc/$package/README.md"
gzip -9n "$root/usr/share/doc/$package/README.md"
printf '%s (%s) unstable; urgency=medium\n\n  * Built from %s.\n\n -- %s  %s\n' \
    "$package" "$version" "$(git rev-parse --short HEAD 2>/dev/null || echo 'a source tree')" \
    "$maintainer" "$(date -R -d "@$SOURCE_DATE_EPOCH")" \
    | gzip -9n > "$root/usr/share/doc/$package/changelog.Debian.gz"

# Debug symbols: keep them, by build ID, for the -dbgsym package, then strip
# the binary the way dh_strip does and point it at its debug file.
bin="$root/usr/bin/tileception"
build_id=$(readelf -n "$bin" | sed -n 's/^ *Build ID: *//p')
if [ -z "$build_id" ]; then
    echo "build-deb: the binary has no GNU build ID, so its debug symbols cannot be found later" >&2
    exit 1
fi
debug_file="$dbg/usr/lib/debug/.build-id/$(echo "$build_id" | cut -c1-2)/$(echo "$build_id" | cut -c3-).debug"
mkdir -p "$(dirname "$debug_file")"
objcopy --only-keep-debug --compress-debug-sections "$bin" "$debug_file"
chmod 644 "$debug_file"
objcopy --strip-unneeded --remove-section=.comment --remove-section=.note "$bin"
objcopy --add-gnu-debuglink="$debug_file" "$bin"
mkdir -p "$dbg/usr/share/doc"
ln -s "$package" "$dbg/usr/share/doc/$package-dbgsym"

# Library dependencies from the binary. dpkg-shlibdeps insists on running next
# to a debian/control, so give it a scratch one.
shlibs_dir=$(mktemp -d)
trap 'rm -rf "$shlibs_dir"' EXIT
mkdir "$shlibs_dir/debian"
printf 'Source: %s\n\nPackage: %s\nArchitecture: any\n' "$package" "$package" > "$shlibs_dir/debian/control"
abs_bin=$(realpath "$bin")
shlibs=$(cd "$shlibs_dir" && dpkg-shlibdeps -O "$abs_bin" | sed -n 's/^shlibs:Depends=//p')

# Companion programs, from deps/debian.txt: Xwayland is required, the rest of
# the runtime group is recommended (apt installs it by default) and the
# optional group is suggested. dbus-daemon is only there for the tests.
group() {
    awk -v group="$1" '
        /^\[/ { g = substr($0, 2, length($0) - 2); next }
        /^[[:space:]]*(#|$)/ { next }
        g == group { print $1 }
    ' deps/debian.txt
}
recommends=$(group runtime | grep -vx -e xwayland -e dbus-daemon | paste -sd, - | sed 's/,/, /g')
suggests=$(group optional | paste -sd, - | sed 's/,/, /g')

installed_size() { du -sk --exclude=DEBIAN "$1" | cut -f1; }

cat > "$root/DEBIAN/control" <<EOF
Package: $package
Version: $version
Architecture: $arch
Maintainer: $maintainer
Installed-Size: $(installed_size "$root")
Depends: $shlibs, xwayland
Recommends: $recommends
Suggests: $suggests
Section: x11
Priority: optional
Homepage: https://github.com/robinvandernoord/tileception
Description: zoomable, nesting tiling Wayland compositor
 Tileception keeps windows from ever overlapping: they live in a nested tree
 of tiles, and you zoom between levels of that tree. It runs as a login
 session on DRM/KMS, arranges X11 programs through Xwayland, and has a
 built-in launcher, notification centre, lock screen and power menu.
EOF
echo /etc/pam.d/tileception > "$root/DEBIAN/conffiles"

cat > "$dbg/DEBIAN/control" <<EOF
Package: $package-dbgsym
Source: $package
Version: $version
Architecture: $arch
Maintainer: $maintainer
Installed-Size: $(installed_size "$dbg")
Depends: $package (= $version)
Section: debug
Priority: optional
Auto-Built-Package: debug-symbols
Build-Ids: $build_id
Description: debug symbols for $package
EOF

# Checksums of everything but the conffiles, which dpkg tracks itself.
for dir in "$root" "$dbg"; do
    (cd "$dir" && find . \( -path ./DEBIAN -o -path ./etc \) -prune -o -type f -print | sed 's|^\./||' | sort \
        | xargs -r md5sum > DEBIAN/md5sums)
done

deb="$out/${package}_${version}_$arch.deb"
dbg_deb="$out/${package}-dbgsym_${version}_$arch.deb"
dpkg-deb --root-owner-group -Zxz --build "$root" "$deb"
dpkg-deb --root-owner-group -Zxz --build "$dbg" "$dbg_deb"
echo "Built $deb"
echo "Built $dbg_deb"
