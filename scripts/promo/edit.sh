#!/usr/bin/env bash
# Cuts the recorded scenes (target/promo/raw) into the promo: title cards,
# captions, speed-ups and a zoom on the duck, joined with quick wipes on the
# beat. Writes target/promo/tileception-promo.mp4. With MUSIC=<file>, that
# track plays under it and the outro lasts until it ends; BPM and
# FIRST_BEAT (seconds) place the cuts on its beat, and MUSIC_LOOP="<from>
# <to>" makes it longer by jumping back once, from one bar line to another
# (empty: play it as it is). The defaults suit "Quirky Music" by Echoes of
# Lumen (Pixabay), which the video was cut to.
set -euo pipefail
. "$(dirname "$0")/lib.sh"
trap - EXIT

EDIT=$OUT/edit
ART=$OUT/art
RAW=$OUT/raw
mkdir -p "$EDIT" "$ART"
FPS=30
WIPE=0.35
BPM=${BPM:-107.7}
FIRST_BEAT=${FIRST_BEAT:-0.023}
# Eight bars from 24.5 s back to 6.7 s: the closest-sounding pair of bar
# lines that far apart, measured, not listened to.
MUSIC_LOOP=${MUSIC_LOOP-24.536 6.708}
# How long each part lasts, in beats, from cut to cut: intro, greeter,
# tiling, shell, duck and handoff, close to the speed they were recorded at.
# The outro lasts until the music ends.
BEATS=(8 6 24 16 20 12)
ENCODE=(-c:v libx264 -preset medium -crf 18 -pix_fmt yuv420p -r $FPS -an)

for scene in 1-greeter 2-tiling 3-shell 4-duck 5-handoff-laptop 5-handoff-desktop; do
    [ -s "$RAW/$scene.mp4" ] || { echo "$RAW/$scene.mp4 is missing: record it first (scripts/promo/record.sh)" >&2; exit 1; }
done

echo "== artwork"
art intro "$ART/intro@2x.png" 2
art outro "$ART/outro@2x.png" 2
art handoff "$ART/handoff.png"
caption() { art "caption&$2&$3&$4" "$ART/caption-$1.png"; }
caption greeter "Hello, Ada." "A login screen that matches your desktop" blue
caption tiling "Tile it. Nest it. Zoom it." "Windows split as they open, groups nest, Super + = zooms in" red
caption shell "Built in, not bolted on." "Launcher, calculator and notifications included" yellow
caption duck "Ask the duck." "It looks at your screen and changes your settings" yellow
caption handoff "Hand it over." "Drag a window onto your other computer" blue

# with_caption <in.mp4> <caption.png> <out.mp4> [from seconds]: the caption
# slides in from the left and stays.
with_caption() {
    local from=${4:-0.3}
    ffmpeg -loglevel error -y -i "$1" -loop 1 -i "$2" -filter_complex \
        "[1]format=rgba,fade=in:st=$from:d=0.3:alpha=1[c];
         [0][c]overlay=x='-140*(1-min(1,max(0,t-$from)/0.3))^3':y=0:shortest=1" \
        "${ENCODE[@]}" "$3"
}

# fit <in> <out> <start> <end> <seconds>: a part of a recording, sped up
# (or slowed down) to last exactly that long.
fit() {
    ffmpeg -loglevel error -y -ss "$3" -to "$4" -i "$1" -vf \
        "setpts=(PTS-STARTPTS)*$5/($4-$3),fps=$FPS,tpad=stop_mode=clone:stop_duration=1,trim=duration=$5" \
        "${ENCODE[@]}" "$2"
}

calc() { awk "BEGIN { print $* }"; }

# Where the cuts fall: on beats, each wipe centred on its beat. LENGTH holds
# each part's length, with the wipes it shares with its neighbours.
beat=$(calc "60 / $BPM")
cuts=() at=0
for beats in "${BEATS[@]}"; do
    at=$((at + beats))
    cuts+=("$(calc "$FIRST_BEAT + $at * $beat")")
done
if [ -n "${MUSIC:-}" ]; then
    if [ -n "$MUSIC_LOOP" ]; then
        read -r loop_from loop_to <<<"$MUSIC_LOOP"
        # Up to the jump, then on from the earlier bar line, blended over a
        # few milliseconds so the join does not click.
        ffmpeg -loglevel error -y -i "$MUSIC" -filter_complex \
            "[0]asplit[a][b];[a]atrim=0:$loop_from[first];[b]atrim=$loop_to,asetpts=PTS-STARTPTS[rest];
             [first][rest]acrossfade=d=0.04:c1=tri:c2=tri" -c:a pcm_s16le "$EDIT/music.wav"
        MUSIC=$EDIT/music.wav
    fi
    total=$(ffprobe -v error -show_entries format=duration -of csv=p=0 "$MUSIC")
else
    total=$(calc "${cuts[-1]} + 9 * $beat")
fi
LENGTH=("$(calc "${cuts[0]} + $WIPE / 2")")
for ((index = 1; index < ${#cuts[@]}; index++)); do
    LENGTH+=("$(calc "${cuts[index]} - ${cuts[index - 1]} + $WIPE")")
done
LENGTH+=("$(calc "$total - ${cuts[-1]} + $WIPE / 2")")

echo "== intro"
# Zooming out of the nested blocks to the whole card: zoom it.
ffmpeg -loglevel error -y -loop 1 -t "${LENGTH[0]}" -i "$ART/intro@2x.png" -vf \
    "scale=w='1920*(2.6-1.6*(1-(1-min(1,t/1.3))^3))':h=-1:eval=frame,
     crop=1920:1080:x='(in_w-1920)*0.83':y='(in_h-1080)*0.30',fps=$FPS" \
    "${ENCODE[@]}" "$EDIT/0-intro.mp4"

echo "== greeter"
fit "$RAW/1-greeter.mp4" "$EDIT/1a.mp4" 0.3 4.3 "${LENGTH[1]}"
with_caption "$EDIT/1a.mp4" "$ART/caption-greeter.png" "$EDIT/1-greeter.mp4" 0.2

echo "== tiling"
fit "$RAW/2-tiling.mp4" "$EDIT/2a.mp4" 0.5 14.9 "${LENGTH[2]}"
with_caption "$EDIT/2a.mp4" "$ART/caption-tiling.png" "$EDIT/2-tiling.mp4" 0.4

echo "== shell"
fit "$RAW/3-shell.mp4" "$EDIT/3a.mp4" 0.6 9.8 "${LENGTH[3]}"
with_caption "$EDIT/3a.mp4" "$ART/caption-shell.png" "$EDIT/3-shell.mp4" 0.3

echo "== duck"
read -r ask answer <"$RAW/4-duck.marks"
read -r duck_x duck_y <"$RAW/4-duck.spot"
# Up close while asking: zoomed 1.9 times around the duck and its bubble,
# then back out as the answer lands, so the wider gaps show.
# zoom <in> <out> <zoom expression, in seconds t> [ffmpeg input options]
zoom() {
    local z="${3//t/(on\/$FPS)}"
    ffmpeg -loglevel error -y "${@:4}" -i "$1" -vf \
        "zoompan=z='$z':d=1:s=1920x1080:fps=$FPS:x='max(0,min(iw-iw/zoom,$duck_x-iw/zoom/2))':y='max(0,min(ih-ih/zoom,$duck_y-160-ih/zoom/2))'" \
        "${ENCODE[@]}" "$2"
}
# Asking and the answer as they happened, and the wait
# in between squeezed into what is left, labelled with how much faster.
asked=$(calc "$ask + 0.3")
asking=$asked
after=3.5
waiting=$(calc "${LENGTH[4]} - $asking - $after")
faster=$(awk -v w="$(calc "$answer - $asked")" -v l="$waiting" 'BEGIN { printf "%d", w / l + 0.5 }')
art "chip&${faster}× faster" "$ART/chip-faster.png"
zoom "$RAW/4-duck.mp4" "$EDIT/4a-zoomed.mp4" "1+0.9*(1-(1-min(1,max(0,t-1)/0.8))^3)" -to "$asked"
fit "$EDIT/4a-zoomed.mp4" "$EDIT/4a.mp4" 0 "$asked" "$asking"
fit "$RAW/4-duck.mp4" "$EDIT/4b-plain.mp4" "$asked" "$answer" "$waiting"
zoom "$EDIT/4b-plain.mp4" "$EDIT/4b-zoomed.mp4" "1.9"
ffmpeg -loglevel error -y -i "$EDIT/4b-zoomed.mp4" -loop 1 -i "$ART/chip-faster.png" \
    -filter_complex "[0][1]overlay=shortest=1" "${ENCODE[@]}" "$EDIT/4b.mp4"
zoom "$RAW/4-duck.mp4" "$EDIT/4c.mp4" "1+0.9*(1-min(1,t/0.7))^3" -ss "$answer" -t "$after"
printf "file '%s'\n" "$EDIT/4a.mp4" "$EDIT/4b.mp4" "$EDIT/4c.mp4" >"$EDIT/4.txt"
ffmpeg -loglevel error -y -f concat -safe 0 -i "$EDIT/4.txt" "${ENCODE[@]}" "$EDIT/4-joined.mp4"
with_caption "$EDIT/4-joined.mp4" "$ART/caption-duck.png" "$EDIT/4-duck.mp4" 0.3

echo "== handoff"
# The two computers' screens in their frames, as recorded side by side.
ffmpeg -loglevel error -y -loop 1 -i "$ART/handoff.png" -i "$RAW/5-handoff-laptop.mp4" -i "$RAW/5-handoff-desktop.mp4" \
    -filter_complex "[1]scale=800:450[l];[2]scale=800:450[d];
        [0][l]overlay=100:230:shortest=1[a];[a][d]overlay=1020:230:shortest=1,fps=$FPS" \
    "${ENCODE[@]}" "$EDIT/5-side-by-side.mp4"
fit "$EDIT/5-side-by-side.mp4" "$EDIT/5a.mp4" 0.3 6.7 "${LENGTH[5]}"
with_caption "$EDIT/5a.mp4" "$ART/caption-handoff.png" "$EDIT/5-handoff.mp4" 0.3

echo "== outro"
ffmpeg -loglevel error -y -loop 1 -t "${LENGTH[6]}" -i "$ART/outro@2x.png" -vf \
    "scale=w='3840*(0.5+0.025*t/${LENGTH[6]})':h=-1:eval=frame,crop=1920:1080:x='(in_w-1920)/2':y='(in_h-1080)/2',fps=$FPS" \
    "${ENCODE[@]}" "$EDIT/6-outro.mp4"

echo "== joining"
parts=("$EDIT/0-intro.mp4" "$EDIT/1-greeter.mp4" "$EDIT/2-tiling.mp4" "$EDIT/3-shell.mp4"
    "$EDIT/4-duck.mp4" "$EDIT/5-handoff.mp4" "$EDIT/6-outro.mp4")
# Each cut is a quick wipe, centred on its beat. Every part is held on its
# last frame or trimmed to its planned length first: one a frame short would
# move every later cut off the beat.
inputs=() filter="" previous="[p0]"
for index in "${!parts[@]}"; do
    inputs+=(-i "${parts[$index]}")
    filter+="[$index:v]tpad=stop_mode=clone:stop_duration=2,trim=duration=${LENGTH[index]},setpts=PTS-STARTPTS[p$index];"
    if [ "$index" -gt 0 ]; then
        offset=$(calc "${cuts[index - 1]} - $WIPE / 2")
        filter+="${previous}[p$index]xfade=transition=wipeleft:duration=$WIPE:offset=$offset[v$index];"
        previous="[v$index]"
    fi
done
video=$OUT/tileception-promo.mp4
if [ -n "${MUSIC:-}" ]; then
    # The track ends the video; a short fade only smooths its last frame.
    inputs+=(-i "$MUSIC")
    filter+="[${#parts[@]}:a]atrim=0:$total,afade=t=out:st=$(calc "$total - 0.3"):d=0.3[music]"
    ffmpeg -loglevel error -y "${inputs[@]}" -filter_complex "${filter}" -map "$previous" -map "[music]" \
        -c:v libx264 -preset slow -crf 18 -pix_fmt yuv420p -c:a aac -b:a 192k -movflags +faststart "$video"
else
    ffmpeg -loglevel error -y "${inputs[@]}" -filter_complex "${filter%;}" -map "$previous" \
        -c:v libx264 -preset slow -crf 18 -pix_fmt yuv420p -movflags +faststart "$video"
fi
printf "%s: %.1f seconds\n" "$video" "$(ffprobe -v error -show_entries format=duration -of csv=p=0 "$video")"
