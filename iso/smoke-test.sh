#!/bin/sh
# Installs a premade ISO in QEMU without anyone at the keyboard, boots the
# result and checks over SSH that it became the Tileception desktop:
#
#   iso/smoke-test.sh resolute
#
# Builds a test variant of the amd64 ISO (iso/test/autoinstall.yaml, which
# asks nothing and wipes the virtual disk) in WORK (default target/iso-test),
# so it needs what iso/build.sh needs, plus qemu-system-x86_64, qemu-img, ssh
# and KVM. The install downloads the whole desktop, so it takes a while.
#
# WORK keeps the disk afterwards: `sh iso/smoke-test.sh --boot resolute`
# boots it again in a window to look around (tester, password tileception).
set -eu
cd "$(dirname "$0")/.."

boot_only=0
if [ "${1:-}" = --boot ]; then
    boot_only=1
    shift
fi
codename=${1:?usage: iso/smoke-test.sh [--boot] CODENAME}
work=$(realpath -m "${WORK:-target/iso-test}")
disk="$work/disk.qcow2"
key="$work/id_ed25519"
port=${SSH_PORT:-2229}
mkdir -p "$work"

fail() { echo "smoke-test: $*" >&2; exit 1; }

machine() { # [extra qemu arguments...]
    qemu-system-x86_64 -name "Tileception smoke test" -enable-kvm -machine q35,vmport=off -cpu host \
        -smp 4 -m 8G -drive file="$disk",if=virtio \
        -nic user,model=virtio-net-pci,hostfwd=tcp:127.0.0.1:"$port"-:22 \
        -device virtio-vga -device qemu-xhci -device usb-tablet "$@"
}

if [ "$boot_only" = 1 ]; then
    [ -e "$disk" ] || fail "no installed disk in $work"
    machine -display gtk
    exit 0
fi

for tool in qemu-system-x86_64 qemu-img ssh ssh-keygen; do
    command -v "$tool" >/dev/null 2>&1 || fail "$tool is missing"
done
[ -w /dev/kvm ] || fail "no access to /dev/kvm"

# ------------------------------------------------------------ the test ISO
rm -f "$key" "$key.pub"
ssh-keygen -q -t ed25519 -N "" -C tileception-smoke-test -f "$key"
sed "s|@SMOKE_TEST_KEY@|$(cat "$key.pub")|" iso/test/autoinstall.yaml > "$work/autoinstall.yaml"
rm -f "$work"/tileception-ubuntu-*-test.*
AUTOINSTALL="$work/autoinstall.yaml" BOOT_ARGS="autoinstall console=tty0 console=ttyS0,115200" ISO_SUFFIX=-test \
    iso/build.sh "$codename" amd64 "$work"
iso=$(ls "$work"/tileception-ubuntu-*-amd64-test.iso)

# ------------------------------------------------------------ install
rm -f "$disk"
qemu-img create -q -f qcow2 "$disk" 40G
echo "Installing $(basename "$iso") (the installer powers off when done)"
timeout "${INSTALL_TIMEOUT:-5400}" qemu-system-x86_64 -name "Tileception smoke test install" -enable-kvm \
    -machine q35,vmport=off -cpu host -smp 4 -m 8G -drive file="$disk",if=virtio -cdrom "$iso" -boot d \
    -nic user,model=virtio-net-pci -device virtio-vga -display none -serial file:"$work/install-serial.log" \
    || fail "the install did not finish (see $work/install-serial.log)"

# ------------------------------------------------------------ first boot
echo "Booting the installed system"
machine -display none -serial file:"$work/boot-serial.log" -daemonize -pidfile "$work/qemu.pid"
trap 'kill "$(cat "$work/qemu.pid")" 2>/dev/null || true' EXIT
remote() {
    ssh -i "$key" -p "$port" -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null \
        -o LogLevel=ERROR -o ConnectTimeout=5 -o BatchMode=yes tester@127.0.0.1 "$@"
}
tries=0
until remote true 2>/dev/null; do
    tries=$((tries + 1))
    [ "$tries" -lt 120 ] || fail "no SSH after 10 minutes (see $work/boot-serial.log)"
    sleep 5
done

# The first boot finishes what the installer could not (removing snaps).
tries=0
while remote systemctl is-enabled --quiet tileception-iso-setup.service 2>/dev/null; do
    tries=$((tries + 1))
    [ "$tries" -lt 180 ] || { remote journalctl -b -u tileception-iso-setup.service --no-pager | tail -20;
        fail "the first boot's setup did not finish in 30 minutes"; }
    sleep 10
done

# ------------------------------------------------------------ checks
failed=0
check() { # <what> <remote shell test>
    if remote "$2" >/dev/null 2>&1; then
        echo "ok      $1"
    else
        echo "FAILED  $1"
        failed=1
    fi
}
check "the installer's part is done" "test -e /var/lib/tileception-iso/desktop"
check "tileception-desktop is installed" "dpkg-query -W -f='\${Status}' tileception-desktop | grep -q 'ok installed'"
check "GNOME Shell is gone" "! dpkg-query -W -f='\${Status}' gnome-shell 2>/dev/null | grep -q 'ok installed'"
check "greetd is the login screen" "test \"\$(basename \"\$(readlink -f /etc/systemd/system/display-manager.service)\")\" = greetd.service"
check "the Tileception greeter is running" "pgrep -f '[t]ileception --greeter'"
check "Firefox is Mozilla's deb" "apt-cache policy firefox | grep -A1 '^ \*\*\*' | grep -q packages.mozilla.org"
check "no Firefox snap" "! snap list firefox"
check "Vivaldi has its own source only" "test \"\$(grep -rl repo.vivaldi.com /etc/apt/sources.list.d | wc -l)\" = 1"
check "apt has no source problems" "! apt-cache policy 2>&1 >/dev/null | grep -q -E '^(E|W):'"
check "Thunderbird is installed" "flatpak info --system org.mozilla.thunderbird"
check "Ptyxis is the session's terminal" "grep -q Ptyxis /etc/xdg/tileception-xdg-terminals.list && test -e /usr/share/applications/org.gnome.Ptyxis.desktop"
check "zoxide's hook is in .bashrc" "grep -q 'zoxide init bash' ~/.bashrc"
check "bat and fd work" "bat --version && fd --version"
[ "$failed" = 0 ] || fail "some checks failed; boot the disk with: sh iso/smoke-test.sh --boot $codename"
echo "All checks passed"
