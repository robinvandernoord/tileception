# The rubber duck: click it, ask for bigger gaps, and Codex changes the setting.
. "$(dirname "$0")/../lib.sh"

start duck "$PROMO/config/desktop.toml"
terminal duck build.sh
wait_windows duck 1
terminal duck editor.sh
wait_windows duck 2
website duck nest
wait_windows duck 3
msg duck duck
sleep 2.5
msg duck pointer 960 500
record duck "$OUT/raw/4-duck.mp4"
started=$(date +%s.%N)
sleep 0.6

# Follow the waddling duck, then click it.
duck_spot() { tree duck | sed -n 's/^duck @ \([0-9]*\),\([0-9]*\) \([0-9]*\)x\([0-9]*\)$/\1 \2 \3 \4/p'; }
read -r x y w h < <(duck_spot)
glide duck $((x + w / 2)) $((y + h / 2)) 0.7
read -r x y w h < <(duck_spot)
msg duck click $((x + w / 2)) $((y + h / 2))
# Where the duck sat, for the editing to zoom in on its bubble.
echo "$((x + w / 2)) $((y + h / 2))" >"$OUT/raw/4-duck.spot"
sleep 0.8
typing duck "make the gaps much bigger" 0.05
sleep 0.3
msg duck key Return
asked=$(date +%s.%N)

# Until the agent has saved the config and Tileception reloaded it.
config=$RUN/duck/config/tileception/config.toml
for _ in $(seq 1 120); do
    grep -q "config reloaded" "$RUN/duck/log" && break
    sleep 0.5
done
answered=$(date +%s.%N)
sleep 5
stop duck
# When the question went and when the answer landed, in the recording's
# seconds: the editing speeds up the wait between them.
awk -v s="$started" -v a="$asked" -v d="$answered" 'BEGIN { printf "%.2f %.2f\n", a - s, d - s }' >"$OUT/raw/4-duck.marks"
read -r ask answer <"$OUT/raw/4-duck.marks"
echo "duck: asked at ${ask}s, settings changed at ${answer}s; now $(grep -m1 '^gap' "$config" || echo 'gap unchanged')"
