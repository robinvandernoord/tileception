# Two computers, both on this machine: a window is dragged from the laptop
# across the shared edge onto the desktop.
. "$(dirname "$0")/../lib.sh"

start laptop "$PROMO/config/handoff-laptop.toml"
start desktop "$PROMO/config/handoff-desktop.toml"
# Until they have found each other, then side by side: desktop on the right.
for _ in $(seq 1 50); do
    TILECEPTION_SOCKET=${SOCKET[laptop]} "$BIN" msg handoff | grep -q "desktop" && break
    sleep 0.2
done
msg laptop handoff edge desktop headless right
msg desktop handoff edge laptop headless left

terminal laptop notes.sh
wait_windows laptop 1
terminal laptop build.sh
wait_windows laptop 2
terminal desktop stats.sh
wait_windows desktop 1
website desktop zoom
wait_windows desktop 2
msg laptop key Super+Left
msg laptop pointer 600 500
record laptop "$OUT/raw/5-handoff-laptop.mp4"
record desktop "$OUT/raw/5-handoff-desktop.mp4"
date +%s.%N >"$OUT/raw/5-handoff.start"
sleep 1
# Alt+drag the notes from the laptop, over the edge, into the desktop's left half.
msg laptop keydown Alt
msg laptop press left
shove laptop 1500 40 1.6
shove laptop 500 -60 0.7
sleep 0.3
msg laptop release left
msg laptop keyup Alt
sleep 2.5
# Both recordings end before either computer goes, which would send the
# window home.
stop_recording laptop
stop_recording desktop
stop laptop
stop desktop
