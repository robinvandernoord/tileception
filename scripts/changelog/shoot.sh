#!/usr/bin/env bash
# Takes a release's changelog screenshots by running its scenes in headless
# Tileceptions: once with this release and, for before/after pairs, once with
# the release before it. The pictures go to
# website/public/changelog/v<version>/ as JPEGs.
#
#   scripts/changelog/shoot.sh 0.19.0            every scene of 0.19.0
#   scripts/changelog/shoot.sh 0.19.0 desk       only scenes/v0.19.0/desk.sh
#
# Scenes are target/changelog/scenes/v<version>/<name>.sh, written with the
# helpers in scripts/changelog/lib.sh. They are working files, not committed:
# the pictures they take are what the changelog keeps. A scene whose header
# has the line
#   # shots: after
# runs only with this release; without it, the scene runs for both. Both
# binaries come from scripts/changelog/build.sh: they build on the selected
# cargo-plane builder and run here, which needs a GPU render node (or Mesa's
# software EGL device) and Chromium.
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/../.." && pwd)
VERSION=${1:?usage: scripts/changelog/shoot.sh <version> [scene...]}
VERSION=${VERSION#v}
shift
SCENES=$ROOT/target/changelog/scenes/v$VERSION
PUBLISH=$ROOT/website/public/changelog/v$VERSION
WORK=$ROOT/target/changelog/shots/v$VERSION

[ -d "$SCENES" ] || { echo "no scenes in ${SCENES#"$ROOT"/}" >&2; exit 1; }
for tool in chromium ffmpeg; do
    command -v "$tool" >/dev/null || { echo "$tool is missing" >&2; exit 1; }
done

# The release before this one, by version order of the tags.
PREVIOUS=${PREVIOUS:-$(git -C "$ROOT" tag --list 'v*' --sort=v:refname | grep -B1 -x "v$VERSION" | head -1)}
PREVIOUS=${PREVIOUS#v}
[ "$PREVIOUS" != "$VERSION" ] || PREVIOUS=

if [ $# -gt 0 ]; then
    scenes=()
    for name in "$@"; do scenes+=("$SCENES/$name.sh"); done
else
    scenes=("$SCENES"/*.sh)
fi

after_bin=$("$ROOT/scripts/changelog/build.sh" "v$VERSION")
before_bin=
needs_before=false
for scene in "${scenes[@]}"; do
    grep -qx '# shots: after' "$scene" || needs_before=true
done
if $needs_before && [ -n "$PREVIOUS" ]; then
    before_bin=$("$ROOT/scripts/changelog/build.sh" "v$PREVIOUS")
fi

mkdir -p "$WORK" "$PUBLISH"
failed=()

# run <scene file> <role> <binary>: one scene in a shell of its own, so its
# instances stop when it ends, however it ends.
run() {
    local scene=$1 role=$2 bin=$3 name
    name=$(basename "$scene" .sh)
    echo "== $name ($role, $("$bin" --version))" >&2
    rm -f "$WORK/$name"-*"-$role.png" "$WORK/$name-$role.png"
    if ! (
        export ROOT VERSION PREVIOUS ROLE=$role SCENE=$name SHOTS=$WORK
        export TILECEPTION_BIN=$bin PROMO_OUT=$ROOT/target/changelog/run
        set -euo pipefail
        . "$ROOT/scripts/changelog/lib.sh"
        trap stop_all EXIT
        . "$scene"
    ); then
        failed+=("$name ($role)")
        return
    fi
}

for scene in "${scenes[@]}"; do
    [ -f "$scene" ] || { echo "no scene ${scene#"$ROOT"/}" >&2; exit 1; }
    run "$scene" after "$after_bin"
    if ! grep -qx '# shots: after' "$scene"; then
        if [ -n "$before_bin" ]; then
            run "$scene" before "$before_bin"
        else
            failed+=("$(basename "$scene" .sh) (before: no earlier release)")
        fi
    fi
done

# Publish what was taken, 1600 pixels wide.
for png in "$WORK"/*.png; do
    [ -e "$png" ] || continue
    ffmpeg -v error -y -i "$png" -vf scale=1600:-2 -q:v 3 "$PUBLISH/$(basename "$png" .png).jpg"
    echo "${PUBLISH#"$ROOT"/}/$(basename "$png" .png).jpg"
done

if [ ${#failed[@]} -gt 0 ]; then
    printf 'failed: %s\n' "${failed[@]}" >&2
    exit 1
fi
