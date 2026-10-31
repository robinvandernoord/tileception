# Shared by the promo video scripts: headless Tileceptions with their own
# config, state and demo content, scripted input, and wf-recorder. Sourced,
# not run. Everything goes to target/promo (PROMO_OUT); your own config, state
# and running session are never touched, because every `msg` names its socket.

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
PROMO=$ROOT/scripts/promo
OUT=${PROMO_OUT:-$ROOT/target/promo}
BIN=${TILECEPTION_BIN:-$ROOT/target/release/tileception}
SIZE=1920x1080
RUN=$OUT/run

declare -A SOCKET PID WAYLAND RECORDER

# Decimal points, not commas, whatever the language: msg and awk need them.
export LC_NUMERIC=C

# The demo user's apps: no personal launcher entries (XDG_DATA_HOME) and no
# personal app config (XDG_CONFIG_HOME). Agent installs are kept between runs.
mkdir -p "$OUT/raw" "$RUN" "$OUT/share" "$OUT/cache"

# start <name> <config.toml> [VAR=value...] [-- tileception args]: a headless
# Tileception with a fresh config and state.
start() {
    local name=$1 config=$2
    shift 2
    local env=() args=(--headless)
    while [ $# -gt 0 ]; do
        if [ "$1" = -- ]; then shift; args+=("$@"); break; fi
        env+=("$1")
        shift
    done
    local dir=$RUN/$name
    rm -rf "$dir"
    mkdir -p "$dir/config/tileception" "$dir/state/tileception" "$dir/home"
    sed "s|@PROMO@|$PROMO|g; s|@OUT@|$OUT|g" "$config" >"$dir/config/tileception/config.toml"
    # The duck asks the doctor's agent, chosen before the doctor starts.
    printf 'agent = "%s"\nmode = "config"\n' "${PROMO_AGENT:-codex-acp}" >"$dir/state/tileception/doctor.toml"
    # With FAKE_HOST set, a namespace of its own gives the instance that host
    # name without root, for the greeter, which shows it. Apps inside would
    # run as a pretend root, which Chromium refuses, so only then.
    local wrap=()
    if [ -n "${FAKE_HOST:-}" ]; then
        wrap=(unshare --user --map-root-user --uts sh -c 'hostname "$0" && exec "$@"' "$FAKE_HOST")
    fi
    # A private session bus each: notifications, the tray and media players
    # stay inside the demo instead of reaching your own desktop. On it, the
    # built-in notifications can run headless too.
    env -u DBUS_SESSION_BUS_ADDRESS XDG_CONFIG_HOME="$dir/config" XDG_STATE_HOME="$dir/state" \
        XDG_DATA_HOME="$OUT/share" XDG_CACHE_HOME="$OUT/cache" \
        TILECEPTION_HEADLESS_SIZE=$SIZE RUST_LOG=info,smithay=error "${env[@]}" \
        setsid dbus-run-session -- \
        sh -c 'TILECEPTION_NOTIFICATION_BUS=$DBUS_SESSION_BUS_ADDRESS exec "$@"' sh "${wrap[@]}" "$BIN" "${args[@]}" \
        >"$dir/log" 2>&1 </dev/null &
    PID[$name]=$!
    local tries=0
    until grep -q "TILECEPTION_SOCKET=" "$dir/log" 2>/dev/null; do
        tries=$((tries + 1))
        if [ $tries -gt 100 ] || ! kill -0 "${PID[$name]}" 2>/dev/null; then
            echo "$name did not start; its log:" >&2
            cat "$dir/log" >&2
            return 1
        fi
        sleep 0.1
    done
    SOCKET[$name]=$(sed -n 's/.*TILECEPTION_SOCKET=\([^ ]*\).*/\1/p' "$dir/log" | head -1)
    # The socket is named after the display: tileception-wayland-2.sock.
    WAYLAND[$name]=$(basename "${SOCKET[$name]}" .sock)
    WAYLAND[$name]=${WAYLAND[$name]#tileception-}
    sleep 0.5
}

# msg <name> <command...>: `tileception msg` to that instance only.
msg() {
    local name=$1
    shift
    TILECEPTION_SOCKET=${SOCKET[$name]} "$BIN" msg "$@" >/dev/null
}

tree() {
    TILECEPTION_SOCKET=${SOCKET[$1]} "$BIN" msg tree
}

# The pointer's position, as "x y".
pointer_at() {
    tree "$1" | sed -n 's/.* pointer \([0-9.-]*\),\([0-9.-]*\)$/\1 \2/p'
}

# glide <name> <x> <y> [seconds]: moves the pointer there smoothly, easing in
# and out, as a hand would.
glide() {
    local name=$1 x=$2 y=$3 seconds=${4:-0.6}
    local from
    from=$(pointer_at "$name")
    local steps
    steps=$(awk -v s="$seconds" 'BEGIN { n = int(s * 40); print (n < 2 ? 2 : n) }')
    local frame
    frame=$(awk -v s="$seconds" -v n="$steps" 'BEGIN { print s / n }')
    awk -v from="$from" -v x="$x" -v y="$y" -v n="$steps" 'BEGIN {
        split(from, p, " ")
        for (i = 1; i <= n; i++) {
            t = i / n; e = t < 0.5 ? 4 * t * t * t : 1 - (-2 * t + 2) ^ 3 / 2
            printf "%.1f %.1f\n", p[1] + (x - p[1]) * e, p[2] + (y - p[2]) * e
        }
    }' | while read -r px py; do
        msg "$name" pointer "$px" "$py"
        sleep "$frame"
    done
}

# shove <name> <dx> <dy> [seconds]: moves the pointer by this much in mouse
# steps, which cross screen edges linked by handoff.
shove() {
    local name=$1 dx=$2 dy=$3 seconds=${4:-0.6}
    local steps
    steps=$(awk -v s="$seconds" 'BEGIN { n = int(s * 40); print (n < 2 ? 2 : n) }')
    local frame
    frame=$(awk -v s="$seconds" -v n="$steps" 'BEGIN { print s / n }')
    awk -v dx="$dx" -v dy="$dy" -v n="$steps" 'BEGIN {
        last_x = 0; last_y = 0
        for (i = 1; i <= n; i++) {
            t = i / n; e = t < 0.5 ? 2 * t * t : 1 - (-2 * t + 2) ^ 2 / 2
            printf "%.1f %.1f\n", dx * e - last_x, dy * e - last_y
            last_x = dx * e; last_y = dy * e
        }
    }' | while read -r step_x step_y; do
        msg "$name" motion "$step_x" "$step_y"
        sleep "$frame"
    done
}

# typing <name> <text> [seconds per character]: types like a person.
typing() {
    local name=$1 text=$2 pace=${3:-0.05} index
    for ((index = 0; index < ${#text}; index++)); do
        local character=${text:index:1}
        if [ "$character" = " " ]; then
            msg "$name" key space
        else
            msg "$name" type "$character"
        fi
        sleep "$pace"
    done
}

# spawn <name> <command line>: starts an app on that instance.
spawn() {
    msg "$1" spawn "$2"
}

# A terminal showing one of the demo scripts in content/.
terminal() {
    local name=$1 script=$2
    spawn "$name" "kitty --config NONE -o font_size=13 -o window_padding_width=14 -o background=#16191e -o foreground=#d8dee9 -o cursor_shape=beam -o confirm_os_window_close=0 -o linux_display_server=wayland bash $PROMO/content/$script"
}

# The project's own website in an app window.
website() {
    local name=$1 anchor=${2:-home}
    spawn "$name" "chromium --ozone-platform=wayland --user-data-dir=$OUT/chromium-$name --no-first-run --no-default-browser-check --disable-features=Translate --hide-crash-restore-bubble --app=file://$ROOT/website/public/index.html#$anchor"
}

# windows <name>: how many windows are on screen.
windows() {
    tree "$1" | grep -c '^ *tile ' || true
}

# wait_windows <name> <count>: until that many windows are mapped.
wait_windows() {
    local name=$1 count=$2 tries=0
    while [ "$(windows "$name")" -lt "$count" ]; do
        tries=$((tries + 1))
        [ $tries -gt 150 ] && { echo "$name: expected $count windows" >&2; return 1; }
        sleep 0.1
    done
    # Let the first frame and the opening animation settle.
    sleep 0.6
}

# record <name> <file>: starts recording that instance's screen, pointer
# included.
record() {
    local name=$1 file=$2
    rm -f "$file"
    WAYLAND_DISPLAY=${WAYLAND[$name]} wf-recorder -r 30 -c libx264 -x yuv420p \
        -p crf=16 -p preset=veryfast -f "$file" >"$RUN/$name/recorder.log" 2>&1 </dev/null &
    RECORDER[$name]=$!
    local tries=0
    until [ -s "$file" ]; do
        tries=$((tries + 1))
        if [ $tries -gt 50 ] || ! kill -0 "${RECORDER[$name]}" 2>/dev/null; then
            cat "$RUN/$name/recorder.log" >&2
            return 1
        fi
        sleep 0.1
    done
}

# stop_recording <name>: finishes the file.
stop_recording() {
    local name=$1 pid=${RECORDER[$1]:-}
    [ -n "$pid" ] || return 0
    kill -INT "$pid" 2>/dev/null || true
    wait "$pid" 2>/dev/null || true
    unset "RECORDER[$name]"
}

# stop <name>: ends an instance and everything it started.
stop() {
    local name=$1
    stop_recording "$name"
    local pid=${PID[$name]:-}
    [ -n "$pid" ] || return 0
    kill -TERM -- "-$pid" 2>/dev/null || true
    wait "$pid" 2>/dev/null || true
    unset "PID[$name]"
}

stop_all() {
    local name
    for name in "${!PID[@]}"; do
        stop "$name"
    done
}
trap stop_all EXIT
trap 'exit 130' INT TERM

# art <piece> <file.png> [scale]: renders a piece of art/art.html at
# 1920x1080 (times scale, for zooming in), with a transparent background
# where the piece has none.
art() {
    mkdir -p "$(dirname "$2")"
    chromium --headless=new --disable-gpu --hide-scrollbars --default-background-color=00000000 \
        --force-device-scale-factor="${3:-1}" \
        --virtual-time-budget=3000 --window-size=1920,1080 --user-data-dir="$OUT/chromium-art" \
        --screenshot="$2" "file://$PROMO/art/art.html?$1" >/dev/null 2>&1
    [ -s "$2" ] || { echo "could not render $1" >&2; return 1; }
}
