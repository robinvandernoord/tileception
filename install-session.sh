#!/bin/sh
# Builds Tileception, installs it for the current user in ~/.local/bin, and adds
# a "Tileception" entry to the login screen and its desktop portal backend
# (these need sudo, and are skipped when already up to date).
#
# Asks nothing and keeps your config: options are passed to
# resources/tileception-setup, such as --preset minimal or --greeter
# tileception, and applied at the end. `make setup` asks the questions.
#
# Safe to run from inside a Tileception session: the running compositor keeps
# going, and the new build starts at the next login.
set -e
cd "$(dirname "$0")"

# The .deb installs the same login screen entry and PAM file; mixing the two
# would leave dpkg tracking files this script overwrites or removes.
if [ "$(dpkg-query -W -f='${Status}' tileception 2>/dev/null)" = "install ok installed" ]; then
    echo "The tileception package is installed; remove it first (sudo apt remove tileception)." >&2
    exit 1
fi

# Stop early, with the exact command, if a system package is missing.
if ! scripts/deps.sh check build runtime; then
    echo "Run 'make deps' first, then try again." >&2
    exit 1
fi

# Checks the options before the build. They come back as flags.
flags=$(resources/tileception-setup --print-flags --non-interactive "$@") || exit 1

bindir="$HOME/.local/bin"
cargo plane build --release -p tileception
mkdir -p "$bindir"
install -m 755 target/release/tileception "$bindir/tileception"
install -m 755 resources/tileception-session "$bindir/tileception-session"
install -m 755 resources/tileception-power "$bindir/tileception-power"
install -m 755 resources/tileception-screenshot "$bindir/tileception-screenshot"
install -m 755 resources/tileception-setup "$bindir/tileception-setup"
echo "Installed tileception and its helpers in $bindir"

pam_service=/etc/pam.d/tileception
if ! cmp -s crates/tileception-auth/pam/tileception "$pam_service"; then
    sudo install -m 644 crates/tileception-auth/pam/tileception "$pam_service"
    echo "Installed the Tileception PAM authentication and account policy"
fi

portals="${XDG_CONFIG_HOME:-$HOME/.config}/xdg-desktop-portal"
mkdir -p "$portals"
install -m 644 resources/tileception-portals.conf "$portals/tileception-portals.conf"
echo "Installed $portals/tileception-portals.conf (screen sharing through xdg-desktop-portal-wlr)"

# The portal only looks for backends in this system directory.
portal=/usr/share/xdg-desktop-portal/portals/tileception.portal
if ! cmp -s resources/tileception.portal "$portal"; then
    sudo install -m 644 resources/tileception.portal "$portal"
    echo "Installed $portal (light or dark for applications)"
fi

# Earlier builds were called Trellis: carry the config over and remove the
# old files. A running Trellis session keeps going until you log out.
config_dir="${XDG_CONFIG_HOME:-$HOME/.config}"
if [ -d "$config_dir/trellis" ] && [ ! -e "$config_dir/tileception" ]; then
    mv "$config_dir/trellis" "$config_dir/tileception"
    echo "Moved $config_dir/trellis to $config_dir/tileception"
fi
rm -f "$bindir/trellis" "$bindir/trellis-session" "$portals/trellis-portals.conf"
if [ -e /usr/share/wayland-sessions/trellis.desktop ]; then
    sudo rm -f /usr/share/wayland-sessions/trellis.desktop
    echo "Removed the old Trellis login screen entry"
fi

desktop=/usr/share/wayland-sessions/tileception.desktop
# No TryExec: the display manager checks it as its own user, which usually
# cannot enter the home directory, and would then hide the session.
sed -e "s|@BINDIR@|$bindir|g" -e "/^TryExec=/d" resources/tileception.desktop.in > target/tileception.desktop
if cmp -s target/tileception.desktop "$desktop"; then
    echo "The login screen entry is up to date"
else
    sudo install -m 644 target/tileception.desktop "$desktop"
    echo "Added the Tileception session to the login screen"
fi

# The session's badge on Mint's (slick-greeter) login screen, generated.
badge=/usr/share/slick-greeter/badges/tileception.svg
if [ -d /usr/share/slick-greeter/badges ]; then
    scripts/session-badge.sh > target/tileception-badge.svg
    if cmp -s target/tileception-badge.svg "$badge"; then
        echo "The login screen badge is up to date"
    else
        sudo install -m 644 target/tileception-badge.svg "$badge"
        echo "Added the Tileception badge to the login screen"
    fi
fi

# The login screen runs /usr/local/bin/tileception, which cannot be the one in
# ~/.local: keep it current once make install-greeter put it there, and put it
# there when the answers switch to Tileception's login screen.
case " $flags " in
    *" --greeter tileception "*) greeter=1 ;;
    *) greeter="" ;;
esac
if [ -n "$greeter" ] || [ -e /usr/local/bin/tileception ]; then
    scripts/install-greeter.sh --files
fi

eval "set -- $flags"
TILECEPTION_BIN="$bindir/tileception" resources/tileception-setup "$@"
