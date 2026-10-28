#!/bin/sh
# Undoes install-greeter.sh: removes /usr/local/bin/tileception, the greeter's
# memory of the last login, the shared looks and their AccountsService
# extension. greetd and /etc/tileception/greeter.toml are kept; pass --purge
# to remove the config too.
set -e

# Removing the binary greetd starts would leave nobody able to log in.
if grep -qs "tileception --greeter" /etc/greetd/config.toml \
    && [ "$(basename "$(readlink -f /etc/systemd/system/display-manager.service)")" = greetd.service ]; then
    echo "greetd runs the Tileception greeter as the login screen." >&2
    echo "Switch back to another login screen first, such as: sudo systemctl enable --force sddm" >&2
    exit 1
fi

if [ -e /usr/local/bin/tileception ]; then
    sudo rm -f /usr/local/bin/tileception
    echo "Removed /usr/local/bin/tileception"
fi
if [ -e /var/lib/tileception-greeter ]; then
    sudo rm -rf /var/lib/tileception-greeter
    echo "Removed /var/lib/tileception-greeter"
fi
if [ -e /var/cache/tileception-greeter ]; then
    sudo rm -rf /var/cache/tileception-greeter
    echo "Removed the shared wallpapers"
fi
extension=/usr/share/accountsservice/interfaces/org.tileception.Greeter.xml
if [ -L "$extension" ] || [ -e /usr/share/dbus-1/interfaces/org.tileception.Greeter.xml ]; then
    sudo rm -f "$extension" /usr/share/dbus-1/interfaces/org.tileception.Greeter.xml
    sudo systemctl try-restart accounts-daemon.service
    echo "Removed the org.tileception.Greeter AccountsService extension"
fi
if [ "$1" = "--purge" ] && [ -e /etc/tileception/greeter.toml ]; then
    sudo rm -f /etc/tileception/greeter.toml
    sudo rmdir /etc/tileception 2>/dev/null || true
    echo "Removed /etc/tileception/greeter.toml"
fi
