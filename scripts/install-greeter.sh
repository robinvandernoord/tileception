#!/bin/sh
# Makes the Tileception greeter the login screen (with sudo): installs greetd if
# it is missing, tileception in /usr/local/bin where greetd's user can run it,
# the greeter's state directory, /etc/tileception/greeter.toml (an existing one
# is kept), and what sessions need to share their look with it: the
# org.tileception.Greeter AccountsService extension and the wallpaper drop
# folder. Then greetd runs the greeter, and becomes the display manager in
# place of the current one, which uninstall-greeter.sh switches back to.
#
# The switch takes effect at the next boot: restarting the display manager now
# would end the graphical sessions running under it.
set -e
cd "$(dirname "$0")/.."
. scripts/greeter-lib.sh

# The package has its own copy in /usr/bin and sets up the rest itself.
if [ "$(dpkg-query -W -f='${Status}' tileception 2>/dev/null)" = "install ok installed" ]; then
    echo "The tileception package is installed and already includes the greeter." >&2
    exit 1
fi

cargo plane build --release -p tileception
# The login screen depends on this binary: it must at least start.
target/release/tileception --version >/dev/null

previous=$(current_dm)
ensure_greetd

sudo install -D -m 755 target/release/tileception /usr/local/bin/tileception
echo "Installed /usr/local/bin/tileception"
sudo install -d -o "$greeter_user" -g "$greeter_user" -m 0700 /var/lib/tileception-greeter
echo "The greeter remembers the last login in /var/lib/tileception-greeter"
# Sessions share their accent through AccountsService, which loads extensions
# when it starts, and their wallpaper through a folder anyone may add to but
# nobody may list (the greeter checks who owns each file).
sudo install -D -m 644 resources/org.tileception.Greeter.xml /usr/share/dbus-1/interfaces/org.tileception.Greeter.xml
sudo mkdir -p /usr/share/accountsservice/interfaces
sudo ln -sfn ../../dbus-1/interfaces/org.tileception.Greeter.xml \
    /usr/share/accountsservice/interfaces/org.tileception.Greeter.xml
sudo systemctl try-restart accounts-daemon.service
sudo install -d -m 1733 /var/cache/tileception-greeter
echo "Sessions can share their accent and wallpaper with the login screen"
config=/etc/tileception/greeter.toml
if [ -e "$config" ]; then
    echo "Kept your $config"
else
    sudo install -D -m 644 resources/greeter.toml "$config"
    echo "Installed $config"
fi

# greetd runs the greeter, on the console it already uses. Its own config is
# kept once, for going back.
if [ ! -e "$greetd_backup" ] && ! grep -qs "tileception --greeter" "$greetd_config"; then
    sudo cp -p "$greetd_config" "$greetd_backup"
fi
printf '%s\n' \
    "# Written by Tileception's install-greeter.sh; the previous config is" \
    "# $greetd_backup." \
    "[terminal]" \
    "vt = $(greetd_vt)" \
    "" \
    "[default_session]" \
    'command = "/usr/local/bin/tileception --greeter"' \
    "user = \"$greeter_user\"" > target/greetd-config.toml
sudo install -D -m 644 target/greetd-config.toml "$greetd_config"
echo "greetd runs the Tileception greeter"

if [ "$previous" = greetd.service ]; then
    echo "greetd is already the login screen"
else
    if [ -n "$previous" ]; then
        echo "$previous" | sudo tee "$previous_dm_file" >/dev/null
        sudo systemctl disable "$previous"
    fi
    sudo systemctl enable greetd.service
    echo "greetd replaces ${previous:-no login screen} as the login screen from the next boot"
    echo "(make uninstall-greeter switches back)"
fi
