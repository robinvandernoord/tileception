#!/bin/sh
# Turns a fresh Ubuntu desktop install into the Tileception desktop. The
# premade ISO puts this directory at /usr/lib/tileception-iso and runs:
#
#   setup.sh install     from the installer, inside the new system
#   setup.sh firstboot   from tileception-iso-setup.service at every boot,
#                        until everything is done
#
# The desktop needs the network. When the installer had none, or a step
# failed, the service does it at the next boot. What cannot happen inside the
# installer (removing snaps, which needs snapd running) waits for that boot
# too. Every step can run again.
set -eu

here=$(cd "$(dirname "$0")" && pwd)
state=/var/lib/tileception-iso
export DEBIAN_FRONTEND=noninteractive

say() { printf 'tileception-iso: %s\n' "$*"; }

# The person the installer created: the first ordinary account.
person() {
    getent passwd | awk -F: '$3 >= 1000 && $3 < 60000 { print $1; exit }'
}

apt_get() {
    apt-get -y -o Dpkg::Options::=--force-confdef -o Dpkg::Options::=--force-confold "$@" </dev/null
}

# Tileception's repository, with the key from the ISO rather than one
# downloaded now; tileception-archive-keyring takes both over once installed.
add_tileception() {
    install -D -m 644 "$here/tileception-archive-keyring.gpg" \
        /usr/share/keyrings/tileception-archive-keyring.gpg
    printf 'Types: deb\nURIs: https://apt.tileception.space\nSuites: %s\nComponents: main\nSigned-By: %s\n' \
        "$suite" /usr/share/keyrings/tileception-archive-keyring.gpg \
        > /etc/apt/sources.list.d/tileception.sources
}

# Vivaldi's repository, in the file and with the key that vivaldi-stable
# writes itself, so its package takes them over instead of adding a second
# source for the same repository.
add_vivaldi() {
    [ -e /etc/apt/sources.list.d/vivaldi.sources ] && return 0
    install -D -m 644 "$here/vivaldi-16BD9233.gpg" /usr/share/keyrings/vivaldi-16BD9233.gpg
    printf 'Types: deb\nURIs: https://repo.vivaldi.com/stable/deb/\nSuites: stable\nComponents: main\nArchitectures: %s\nSigned-By: %s\n' \
        "$(dpkg --print-architecture)" /usr/share/keyrings/vivaldi-16BD9233.gpg \
        > /etc/apt/sources.list.d/vivaldi.sources
}

# Packages from Ubuntu's minimal desktop stay installed when GNOME Shell
# goes: marked as chosen, nothing is removed later as no longer needed.
remove_gnome_shell() {
    apt-mark showauto | xargs -r apt-mark manual >/dev/null
    apt_get purge gdm3 gnome-shell ubuntu-session gnome-initial-setup ubuntu-desktop-minimal \
        yaru-theme-gnome-shell 'gnome-shell-extension-*' || true
    # There is no GDM left to go back to.
    rm -f /etc/tileception/previous-display-manager
}

# Shell hook for zoxide, whose folders the launcher's folder search reads:
# for new accounts, and for the one the installer made.
add_zoxide_hook() {
    # shellcheck disable=SC2016 # written to .bashrc as it is
    line='eval "$(zoxide init bash)"'
    for rc in /etc/skel/.bashrc "$home/.bashrc"; do
        [ -f "$rc" ] || continue
        grep -qsF "$line" "$rc" || printf '\n# Often used folders, for cd and for Tileception'"'"'s launcher\n%s\n' "$line" >> "$rc"
    done
}

desktop() {
    [ -e "$state/desktop" ] && return 0
    add_tileception || return 1
    add_vivaldi || return 1
    # Fails when any source cannot be fetched, such as without a network.
    apt_get update --error-on=any || { say "no network; the desktop is set up at the next boot"; return 1; }
    apt_get install tileception-archive-keyring tileception tileception-desktop tileception-desktop-sources \
        || return 1
    # Now with Mozilla's and GitHub's sources: Mozilla's Firefox replaces
    # Ubuntu's snap package (the pin allows going "down" from its version).
    apt_get update --error-on=any || return 1
    apt_get install --allow-downgrades firefox vivaldi-stable gh || return 1
    remove_gnome_shell
    tileception-setup --non-interactive --preset recommended --greeter tileception --user "$user" || return 1
    add_zoxide_hook
    touch "$state/desktop"
    say "the Tileception desktop is installed"
}

# No asterisk per character while typing a sudo password, which sudo-rs
# (Ubuntu's sudo since 25.10) shows by default.
quiet_sudo() {
    file=/etc/sudoers.d/tileception-iso
    printf '# From the Tileception ISO: no asterisks while typing a password.\nDefaults !pwfeedback\n' > "$file"
    chmod 440 "$file"
    visudo -cqf "$file" >/dev/null 2>&1 || rm -f "$file"
}

# Thunderbird has no deb outside Ubuntu's snap; Mozilla publishes it on
# Flathub.
thunderbird() {
    [ -e "$state/thunderbird" ] && return 0
    command -v flatpak >/dev/null 2>&1 || return 1
    flatpak remote-add --system --if-not-exists flathub https://dl.flathub.org/repo/flathub.flatpakrepo \
        || return 1
    flatpak install --system -y --noninteractive flathub org.mozilla.thunderbird || return 1
    touch "$state/thunderbird"
}

# The snaps Ubuntu's desktop seeds: Firefox (now a deb), the App Center
# (GNOME Software instead), the firmware updater (the updates chip has
# fwupd) and GNOME's helpers for them. snapd stays for anyone who wants snaps.
remove_snaps() {
    [ -e "$state/snaps" ] && return 0
    command -v snap >/dev/null 2>&1 || { touch "$state/snaps"; return 0; }
    # Seeding can take long on a slow machine: give up for this boot after
    # a quarter of an hour, so the rest is not held up.
    timeout 900 snap wait system seed.loaded || { say "snaps are still being seeded"; return 1; }
    for name in firefox snap-store firmware-updater desktop-security-center prompting-client \
        snapd-desktop-integration; do
        if snap list "$name" >/dev/null 2>&1; then
            snap remove --purge "$name" || return 1
        fi
    done
    touch "$state/snaps"
}

mkdir -p "$state"
# shellcheck disable=SC1091 # the system's own
. /etc/os-release
suite=$VERSION_CODENAME
user=$(person)
home=$(getent passwd "$user" | cut -d: -f6)
[ -n "$user" ] || { say "no user account found"; exit 1; }

case "${1:-}" in
    install)
        # The first boot finishes whatever this leaves.
        install -m 644 "$here/tileception-iso-setup.service" /etc/systemd/system/
        systemctl enable tileception-iso-setup.service
        quiet_sudo
        desktop || true
        thunderbird || say "Thunderbird is installed at the next boot"
        ;;
    firstboot)
        failed=0
        desktop || failed=1
        thunderbird || failed=1
        remove_snaps || failed=1
        if [ "$failed" = 0 ]; then
            systemctl disable tileception-iso-setup.service
            say "done"
        else
            say "not finished; trying again at the next boot"
        fi
        ;;
    *)
        echo "usage: setup.sh install|firstboot" >&2
        exit 2
        ;;
esac
