# The login screen: a made-up user and host, typing the demo password.
. "$(dirname "$0")/../lib.sh"

dir=$RUN/greeter-files
rm -rf "$dir"
mkdir -p "$dir/sessions" "$dir/home/.config/tileception"
printf 'ada:x:1500:1500:Ada:%s:/bin/bash\n' "$dir/home" >"$dir/passwd"
printf '[Desktop Entry]\nName=Tileception\nExec=tileception-session\nType=Application\n' \
    >"$dir/sessions/tileception.desktop"
sed 's/^hidden_sessions.*/hidden_sessions = []/' "$ROOT/resources/greeter.toml" >"$dir/greeter.toml"
# Ada's own wallpaper and colours, which the greeter shows for her.
sed "s|@OUT@|$OUT|g" "$PROMO/config/desktop.toml" >"$dir/home/.config/tileception/config.toml"

FAKE_HOST=demo-laptop start greeter "$PROMO/config/desktop.toml" \
    TILECEPTION_GREETER_PASSWD="$dir/passwd" TILECEPTION_GREETER_SESSIONS="$dir/sessions" \
    TILECEPTION_GREETER_CONFIG="$dir/greeter.toml" TILECEPTION_GREETER_STATE="$dir/state.toml" \
    -- --greeter
sleep 1.5
msg greeter pointer 1900 1060
record greeter "$OUT/raw/1-greeter.mp4"
sleep 1.2
typing greeter "tide" 0.18
sleep 0.4
msg greeter key Return
sleep 2
stop greeter
