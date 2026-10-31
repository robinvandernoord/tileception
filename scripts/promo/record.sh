#!/usr/bin/env bash
# Records the promo video's scenes into target/promo/raw: each in headless
# Tileceptions of its own. `scripts/promo/record.sh 2 4` records only those.
set -euo pipefail
. "$(dirname "$0")/lib.sh"

for tool in wf-recorder ffmpeg chromium kitty notify-send; do
    command -v "$tool" >/dev/null || { echo "$tool is missing: run make video, which installs it" >&2; exit 1; }
done
[ -x "$BIN" ] || { echo "no $BIN: build it first (make build)" >&2; exit 1; }

art wallpaper-laptop "$OUT/art/wallpaper-laptop.png"
art wallpaper-desktop "$OUT/art/wallpaper-desktop.png"

wanted=("$@")
for scene in "$PROMO"/scenes/*.sh; do
    number=$(basename "$scene" | cut -d- -f1)
    if [ ${#wanted[@]} -gt 0 ] && [[ ! " ${wanted[*]} " == *" $number "* ]]; then
        continue
    fi
    echo "== scene $(basename "$scene" .sh)"
    bash "$scene"
done
