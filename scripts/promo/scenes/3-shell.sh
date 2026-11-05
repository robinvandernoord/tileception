# The launcher, a unit conversion in it, and a notification.
. "$(dirname "$0")/../lib.sh"

start shell "$PROMO/config/desktop.toml"
terminal shell notes.sh
wait_windows shell 1
website shell tile
wait_windows shell 2
msg shell pointer 1500 700
record shell "$OUT/raw/3-shell.mp4"
sleep 0.8
msg shell launcher
sleep 0.8
typing shell "calc" 0.09
sleep 1
for _ in 1 2 3 4; do msg shell key BackSpace; sleep 0.05; done
typing shell "5 km in mi" 0.07
sleep 1.4
msg shell key Escape
sleep 0.5
spawn shell "notify-send -a Duck 'Quack!' 'Your windows are looking tidy today.'"
sleep 2.2
msg shell key Super+n
sleep 1.6
msg shell key Escape
sleep 0.6
stop shell
