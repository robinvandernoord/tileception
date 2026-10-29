#!/bin/sh
# Makes the Tileception greeter the login screen (with sudo), from a checkout:
# installs tileception in /usr/local/bin where greetd's user can run it, the
# greeter's state directory, /etc/tileception/greeter.toml (an existing one is
# kept), and what sessions need to share their look with it: the
# org.tileception.Greeter AccountsService extension and the wallpaper drop
# folder. Then tileception-setup installs greetd if needed and makes it run
# the greeter in place of the current login screen, which
# uninstall-greeter.sh switches back to.
#
# --files installs the files only (install-session.sh's step); the switch is
# then left to tileception-setup. The switch takes effect at the next boot:
# restarting the display manager now would end the graphical sessions running
# under it.
set -e
cd "$(dirname "$0")/.."
TILECEPTION_SETUP_LIB=1 . resources/tileception-setup

# The package has its own copy in /usr/bin and sets up the rest itself.
if package_installed; then
    echo "The tileception package is installed and already includes the greeter." >&2
    exit 1
fi

if [ ! -x target/release/tileception ] || [ "$1" != --files ]; then
    cargo plane build --release -p tileception
fi
# The login screen depends on this binary: it must at least start.
target/release/tileception --version >/dev/null

if cmp -s target/release/tileception /usr/local/bin/tileception; then
    echo "The login screen's tileception is up to date"
else
    sudo install -D -m 755 target/release/tileception /usr/local/bin/tileception
    echo "Installed /usr/local/bin/tileception"
fi
if getent passwd "$greeter_user" >/dev/null; then
    sudo install -d -o "$greeter_user" -g "$greeter_user" -m 0700 /var/lib/tileception-greeter
fi
# Sessions share their accent through AccountsService, which loads extensions
# when it starts, and their wallpaper through a folder anyone may add to but
# nobody may list (the greeter checks who owns each file).
extension=/usr/share/dbus-1/interfaces/org.tileception.Greeter.xml
if ! cmp -s resources/org.tileception.Greeter.xml "$extension"; then
    sudo install -D -m 644 resources/org.tileception.Greeter.xml "$extension"
    sudo mkdir -p /usr/share/accountsservice/interfaces
    sudo ln -sfn ../../dbus-1/interfaces/org.tileception.Greeter.xml \
        /usr/share/accountsservice/interfaces/org.tileception.Greeter.xml
    sudo systemctl try-restart accounts-daemon.service
    echo "Sessions can share their accent with the login screen"
fi
sudo install -d -m 1733 /var/cache/tileception-greeter
config=/etc/tileception/greeter.toml
if [ ! -e "$config" ]; then
    sudo install -D -m 644 resources/greeter.toml "$config"
    echo "Installed $config"
fi

if [ "$1" != --files ]; then
    resources/tileception-setup --non-interactive --greeter tileception
fi
