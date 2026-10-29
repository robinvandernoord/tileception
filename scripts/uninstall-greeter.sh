#!/bin/sh
# Undoes install-greeter.sh: tileception-setup switches back to the login
# screen it replaced and restores greetd's config, then this removes
# /usr/local/bin/tileception, the greeter's memory of the last login, the
# shared looks and their AccountsService extension. greetd and
# /etc/tileception/greeter.toml are kept; pass --purge to remove the config
# too. The switch back takes effect at the next boot.
set -e
cd "$(dirname "$0")/.."
TILECEPTION_SETUP_LIB=1 . resources/tileception-setup

restore_login_screen

# greetd only reads its config when it starts: while it runs, it would start
# the greeter again after a logout, so the binary stays until the next boot.
if [ -e /usr/local/bin/tileception ]; then
    if systemctl is-active --quiet greetd.service; then
        echo "Kept /usr/local/bin/tileception while greetd runs; run this again after a reboot to remove it"
    else
        sudo rm -f /usr/local/bin/tileception
        echo "Removed /usr/local/bin/tileception"
    fi
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
