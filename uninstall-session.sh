#!/bin/sh
# Undoes install-session.sh: removes the login screen entry and portal backend
# (needs sudo) and the binaries in ~/.local/bin.
#
# Your config (~/.config/tileception) and logs (~/.local/state/tileception) are kept;
# pass --purge to remove those too.
set -e

# The .deb installs the same login screen entry and PAM file; mixing the two
# would leave dpkg tracking files this script overwrites or removes.
if [ "$(dpkg-query -W -f='${Status}' tileception 2>/dev/null)" = "install ok installed" ]; then
    echo "The tileception package is installed; remove it with: sudo apt remove tileception" >&2
    exit 1
fi

bindir="$HOME/.local/bin"
desktop=/usr/share/wayland-sessions/tileception.desktop
portal=/usr/share/xdg-desktop-portal/portals/tileception.portal
badge=/usr/share/slick-greeter/badges/tileception.svg

if [ -e "$desktop" ]; then
    sudo rm -f "$desktop"
    echo "Removed the Tileception session from the login screen"
fi
if [ -e "$badge" ]; then
    sudo rm -f "$badge"
    echo "Removed the login screen badge"
fi
if [ -e "$portal" ]; then
    sudo rm -f "$portal"
    echo "Removed $portal"
fi

portals="${XDG_CONFIG_HOME:-$HOME/.config}/xdg-desktop-portal/tileception-portals.conf"
for f in "$bindir/tileception" "$bindir/tileception-session" "$bindir/tileception-power" "$bindir/tileception-screenshot" "$portals"; do
    if [ -e "$f" ]; then
        rm -f "$f"
        echo "Removed $f"
    fi
done

if [ "$1" = "--purge" ]; then
    rm -rf "${XDG_CONFIG_HOME:-$HOME/.config}/tileception" "${XDG_STATE_HOME:-$HOME/.local/state}/tileception"
    echo "Removed the Tileception config and logs"
fi
