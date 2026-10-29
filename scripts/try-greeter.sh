#!/bin/sh
# Tries the greeter as it is built now, on a spare console, without changing
# the login screen: greetd (installed if missing) runs it with a temporary
# config until Ctrl+C here. VT=9 make try-greeter picks another console.
set -e
cd "$(dirname "$0")/.."
TILECEPTION_SETUP_LIB=1 . resources/tileception-setup

vt=${VT:-8}
cargo plane build --release -p tileception
ensure_greetd

# greetd's user cannot read the build directory, usually inside a home folder.
dir=$(mktemp -d /tmp/tileception-try.XXXXXX)
chmod 755 "$dir"
install -m 755 target/release/tileception "$dir/tileception"
printf '%s\n' \
    "[terminal]" \
    "vt = $vt" \
    "" \
    "[default_session]" \
    "command = \"$dir/tileception --greeter\"" \
    "user = \"$greeter_user\"" > "$dir/greetd.toml"

# A service, as at boot: started from this terminal, greetd would stay in this
# login session, and logind gives the greeter no session (no runtime directory,
# no seat) of its own then.
unit=tileception-try-greeter
stop() {
    trap - EXIT INT TERM
    sudo systemctl stop "$unit" 2>/dev/null || true
    echo
    sudo journalctl -u "$unit" -n 40 --no-pager -o cat || true
    rm -rf "$dir"
}
trap stop EXIT INT TERM
sudo systemd-run --unit="$unit" --collect --quiet -p KeyringMode=shared \
    "$(command -v greetd || echo /usr/sbin/greetd)" --config "$dir/greetd.toml"

here=$(sed 's/^tty//' /sys/class/tty/tty0/active 2>/dev/null)
echo "The greeter is on console $vt (Ctrl+Alt+F$vt)."
echo "Come back with Ctrl+Alt+F${here:-1} and press Ctrl+C here to stop it; its log follows."
echo "Logging in as yourself starts a second session next to this one."
while systemctl is-active --quiet "$unit"; do
    sleep 1
done
echo "greetd stopped by itself."
