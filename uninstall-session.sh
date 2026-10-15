#!/bin/sh
# Undoes install-session.sh: removes the login screen entry (needs sudo) and
# the binaries in ~/.local/bin.
#
# Your config (~/.config/tileception) and logs (~/.local/state/tileception) are kept;
# pass --purge to remove those too.
set -e

bindir="$HOME/.local/bin"
desktop=/usr/share/wayland-sessions/tileception.desktop

if [ -e "$desktop" ]; then
    sudo rm -f "$desktop"
    echo "Removed the Tileception session from the login screen"
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
