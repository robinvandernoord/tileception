#!/bin/sh
# Installs what the Tileception login screen needs (with sudo): greetd if it is
# missing, tileception in /usr/local/bin where greetd's user can run it, the
# greeter's state directory, /etc/tileception/greeter.toml (an existing one is
# kept), and what sessions need to share their look with it: the
# org.tileception.Greeter AccountsService extension and the wallpaper drop
# folder. It does not make greetd the login screen: see the README for that.
set -e
cd "$(dirname "$0")/.."

# The package has its own copy in /usr/bin and sets up the rest itself.
if [ "$(dpkg-query -W -f='${Status}' tileception 2>/dev/null)" = "install ok installed" ]; then
    echo "The tileception package is installed and already includes the greeter." >&2
    exit 1
fi

cargo plane build --release -p tileception

user=_greetd
dm=/etc/systemd/system/display-manager.service
if ! getent passwd "$user" >/dev/null; then
    if ! command -v apt-get >/dev/null; then
        echo "Install greetd with your package manager, then run this again." >&2
        exit 1
    fi
    # Installing greetd enables it, which points display-manager.service at it.
    # Put back whatever login screen was there: switching is a separate step.
    before=$(readlink "$dm" || true)
    echo "Installing greetd (your current login screen stays in charge)"
    sudo apt-get install -y greetd
    after=$(readlink "$dm" || true)
    if [ "$after" != "$before" ]; then
        sudo systemctl disable greetd.service >/dev/null 2>&1 || true
        if [ -n "$before" ]; then
            sudo ln -sfn "$before" "$dm"
        fi
        sudo systemctl daemon-reload
    fi
    if ! getent passwd "$user" >/dev/null; then
        echo "greetd did not create its user $user; is this Debian or Ubuntu's greetd?" >&2
        exit 1
    fi
fi

sudo install -D -m 755 target/release/tileception /usr/local/bin/tileception
echo "Installed /usr/local/bin/tileception"
sudo install -d -o "$user" -g "$user" -m 0700 /var/lib/tileception-greeter
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

echo
echo "Try it on a spare console before making it the login screen:"
echo "  README.md, section \"Login screen (greeter)\""
