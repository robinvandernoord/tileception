# Helpers for changelog scenes, sourced by scripts/changelog/shoot.sh before
# each scene. On top of the promo video's helpers (scripts/promo/lib.sh:
# start, msg, spawn, website, wait_windows, glide, typing, stop_all...) a
# scene gets:
#
#   ROLE        "before" (the previous release) or "after" (this release)
#   VERSION     this release, such as 0.19.0, and PREVIOUS the one before it
#   page <instance> <file>        a local HTML file in a Chromium app window
#   shot <instance> [name]        a screenshot, saved as <scene>[-name]-<ROLE>
#   shot_held <instance> <keys> <ms> <after-ms> [name]
#                                 hold keys for <ms> and take the shot
#                                 <after-ms> into the hold
#
# Every instance gets a fresh config and state, so nothing of yours is read
# or changed, and every msg names its own socket.

. "$ROOT/scripts/promo/lib.sh"

DESKTOP_CONFIG=$PROMO/config/desktop.toml

# The promo's wallpaper, rendered once.
if [ ! -s "$OUT/art/wallpaper-laptop.png" ]; then
    art wallpaper-laptop "$OUT/art/wallpaper-laptop.png" || true
fi

page() {
    spawn "$1" "chromium --ozone-platform=wayland --user-data-dir=$OUT/chromium-$1 --no-first-run --no-default-browser-check --disable-features=Translate --hide-crash-restore-bubble --app=file://$2"
}

# The same four windows in every scene that needs a busy desk.
busy_desk() {
    local name=$1
    page "$name" "$ROOT/docs/design.html"
    wait_windows "$name" 1
    website "$name" home
    wait_windows "$name" 2
    page "$name" "$ROOT/docs/design-greeter.html"
    wait_windows "$name" 3
    page "$name" "$ROOT/website/public/changelog/index.html"
    wait_windows "$name" 4
    sleep 1.5
}

shot() {
    local name=$1 label=${2:+-$2}
    local file=$SHOTS/$SCENE$label-$ROLE.png tries=0 size=-1
    rm -f "$file"
    TILECEPTION_SOCKET=${SOCKET[$name]} "$BIN" msg screenshot "$file" >/dev/null
    # The reply comes before the file is written: wait until it stops growing,
    # so the scene cannot end and stop the instance halfway.
    while [ ! -s "$file" ] || [ "$(stat -c %s "$file")" != "$size" ]; do
        [ -s "$file" ] && size=$(stat -c %s "$file")
        tries=$((tries + 1))
        [ $tries -gt 100 ] && { echo "$SCENE: no screenshot $file" >&2; return 1; }
        sleep 0.1
    done
}

shot_held() {
    local name=$1 keys=$2 hold=$3 delay=$4 label=${5:-}
    TILECEPTION_SOCKET=${SOCKET[$name]} "$BIN" msg key "$keys" hold "$hold" >/dev/null &
    local held=$!
    sleep "$(awk -v ms="$delay" 'BEGIN { print ms / 1000 }')"
    shot "$name" "$label"
    wait "$held"
}
