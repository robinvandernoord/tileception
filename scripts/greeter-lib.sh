# Shared by install-greeter.sh, uninstall-greeter.sh and try-greeter.sh; sourced, not run.

greeter_user=_greetd
dm_link=/etc/systemd/system/display-manager.service
# Which login screen to go back to, written when Tileception's took over.
previous_dm_file=/etc/tileception/previous-display-manager
greetd_config=/etc/greetd/config.toml
greetd_backup=/etc/greetd/config.toml.before-tileception

# The unit that is the login screen now, such as sddm.service; empty if none.
current_dm() {
    if [ -e "$dm_link" ]; then
        basename "$(readlink -f "$dm_link")"
    fi
}

# The console greetd uses, from its config; 7 when it does not say.
greetd_vt() {
    vt=$(sed -n 's/^[[:space:]]*vt[[:space:]]*=[[:space:]]*\([0-9]*\).*/\1/p' "$greetd_config" 2>/dev/null | head -n 1)
    echo "${vt:-7}"
}

# Installs greetd when its user is missing. The package enables greetd, which
# takes display-manager.service; that is put back, so installing greetd never
# changes the login screen by itself.
ensure_greetd() {
    if getent passwd "$greeter_user" >/dev/null; then
        return
    fi
    if ! command -v apt-get >/dev/null; then
        echo "Install greetd with your package manager, then run this again." >&2
        exit 1
    fi
    before=$(readlink "$dm_link" || true)
    echo "Installing greetd"
    sudo apt-get install -y greetd
    after=$(readlink "$dm_link" || true)
    if [ "$after" != "$before" ]; then
        sudo systemctl disable greetd.service >/dev/null 2>&1 || true
        if [ -n "$before" ]; then
            sudo ln -sfn "$before" "$dm_link"
        fi
        sudo systemctl daemon-reload
    fi
    if ! getent passwd "$greeter_user" >/dev/null; then
        echo "greetd did not create its user $greeter_user; is this Debian or Ubuntu's greetd?" >&2
        exit 1
    fi
}
