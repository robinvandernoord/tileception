#!/bin/sh
# Undoes install-greeter.sh: switches back to the login screen it replaced and
# restores greetd's config, then removes /usr/local/bin/tileception, the
# greeter's memory of the last login, the shared looks and their AccountsService
# extension. greetd and /etc/tileception/greeter.toml are kept; pass --purge to
# remove the config too. The switch back takes effect at the next boot.
set -e
cd "$(dirname "$0")/.."
. scripts/greeter-lib.sh

if grep -qs "tileception --greeter" "$greetd_config"; then
    previous=$(cat "$previous_dm_file" 2>/dev/null || true)
    if [ "$(current_dm)" = greetd.service ]; then
        if [ -z "$previous" ]; then
            echo "greetd runs the Tileception greeter, and there is no earlier login screen to go back to." >&2
            echo "Enable another one first, such as: sudo systemctl enable --force sddm" >&2
            exit 1
        fi
        sudo systemctl disable greetd.service
        sudo systemctl enable "$previous"
        echo "$previous is the login screen again from the next boot"
    fi
    if [ -e "$greetd_backup" ]; then
        sudo mv "$greetd_backup" "$greetd_config"
        echo "Restored greetd's own config"
    fi
fi
sudo rm -f "$previous_dm_file"

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
