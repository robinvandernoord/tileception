#!/usr/bin/env bash
# Builds Tileception as it was at a release tag, for the changelog's
# screenshots, and prints the binary's path. Binaries are kept in
# target/changelog/bin/<version>/, so each release is built once: the release
# that was "after" in one changelog entry is "before" in the next.
#
#   scripts/changelog/build.sh v0.19.0
#
# The source is checked out in one worktree, target/changelog/worktree, which
# moves from tag to tag so builds stay incremental. It builds on the selected
# cargo-plane builder, like every other build, and the binary runs locally.
set -euo pipefail

root=$(cd "$(dirname "$0")/../.." && pwd)
tag=${1:?usage: scripts/changelog/build.sh <tag>}
version=${tag#v}
bin=$root/target/changelog/bin/$version/tileception

if [ -x "$bin" ]; then
    echo "$bin"
    exit 0
fi
git -C "$root" rev-parse -q --verify "refs/tags/$tag" >/dev/null || {
    echo "no tag $tag" >&2
    exit 1
}

tree=$root/target/changelog/worktree
if [ -e "$tree/.git" ]; then
    git -C "$tree" checkout -q --detach "$tag"
else
    mkdir -p "$(dirname "$tree")"
    git -C "$root" worktree add -q --detach "$tree" "$tag" >&2 || {
        echo "hint: if git says the worktree is missing but already registered," >&2
        echo "hint: target/ was probably cleaned; run 'git worktree prune' and try again" >&2
        exit 1
    }
fi
# The builder settings are personal and untracked; the worktree needs its own
# copy to count as a cargo-plane project of its own.
if [ -f "$root/.cargo/plane.toml" ]; then
    mkdir -p "$tree/.cargo"
    cp "$root/.cargo/plane.toml" "$tree/.cargo/plane.toml"
fi

# Older tags have no dev-install profile, so every tag builds as release with
# its settings: no LTO, many codegen units, incremental. That is quick to
# build and fast enough to take screenshots with.
echo "building $tag" >&2
(
    cd "$tree"
    cargo plane build --release -p tileception \
        --config 'profile.release.lto="off"' \
        --config profile.release.codegen-units=64 \
        --config profile.release.incremental=true >&2
)

mkdir -p "$(dirname "$bin")"
cp "$tree/target/release/tileception" "$bin"
echo "$bin"
