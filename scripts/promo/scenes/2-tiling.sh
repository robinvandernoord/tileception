# Windows tile as they open, nest in groups, and zoom in and out.
. "$(dirname "$0")/../lib.sh"

start desk "$PROMO/config/desktop.toml"
msg desk pointer 1500 700
record desk "$OUT/raw/2-tiling.mp4"
sleep 0.8
terminal desk build.sh
wait_windows desk 1
website desk home
wait_windows desk 2
sleep 1.4
terminal desk notes.sh
wait_windows desk 3
terminal desk stats.sh
wait_windows desk 4
terminal desk editor.sh
wait_windows desk 5
sleep 0.8
# Into the group around the focused window, then the window itself.
msg desk key Super+equal
sleep 1.3
msg desk key Super+equal
sleep 1.3
msg desk key Super+minus
sleep 0.9
msg desk key Super+minus
sleep 1
msg desk key Super+Left
sleep 0.6
msg desk key Super+o
sleep 1.6
msg desk key Super+o
sleep 1
stop desk
