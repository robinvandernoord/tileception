#!/usr/bin/env bash
# Writes a release's changelog entry with Claude Code: the text in
# website/changelog/v<version>.toml, its pictures (taken by scenes it writes in
# target/changelog/scenes/, which are not committed), and the regenerated page. Nothing is committed; review the result first.
#
#   scripts/changelog/entry.sh 0.19.0     (make changelog VERSION=0.19.0)
#
# The prompt is scripts/changelog/prompt.md. Claude may only touch this
# release's files: its website/changelog/v<version>.toml and its scenes in
# target/changelog/scenes/v<version>/ (Edit rules, which cover every tool that
# writes files). It may run git, gh issue view, the screenshot runner and the
# page generator; everything else is refused without asking. Set
# CHANGELOG_MODEL to choose its model, and FORCE=1 to rewrite an entry that
# exists.
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/../.." && pwd)
cd "$ROOT"
version=${1:-$(git describe --tags --abbrev=0)}
version=${version#v}
toml=website/changelog/v$version.toml

git rev-parse -q --verify "refs/tags/v$version" >/dev/null || { echo "no tag v$version" >&2; exit 1; }
command -v claude >/dev/null || { echo "claude (Claude Code) is missing" >&2; exit 1; }
if [ -e "$toml" ] && [ -z "${FORCE:-}" ]; then
    echo "$toml exists; FORCE=1 writes it again" >&2
    exit 1
fi

previous=$(git tag --list 'v*' --sort=v:refname | grep -B1 -x "v$version" | head -1)
previous=${previous#v}
[ "$previous" != "$version" ] || previous=
date=$(git log -1 --format=%cs "v$version")

# Both builds first, so the scenes Claude runs start at once, and the command
# list of this release for it to read.
bin=$(scripts/changelog/build.sh "v$version")
[ -z "$previous" ] || scripts/changelog/build.sh "v$previous" >/dev/null
mkdir -p target/changelog
"$bin" msg --help >"target/changelog/msg-help-$version.txt" 2>&1 || true

# The prompt is what follows the first --- line of prompt.md.
prompt=$(sed '1,/^---$/d' scripts/changelog/prompt.md | sed \
    -e "s/{version}/$version/g" \
    -e "s/{previous}/${previous:-none}/g" \
    -e "s/{date}/$date/g")

allowed=(
    Read Glob Grep Skill
    "Edit(./$toml)" "Edit(./target/changelog/scenes/v$version/**)"
    "Bash(git log *)" "Bash(git show *)" "Bash(git tag *)" "Bash(git diff *)"
    "Bash(gh issue view *)"
    "Bash(scripts/changelog/shoot.sh $version *)"
    "Bash(python3 website/tools/changelog.py)"
    "Bash(ls *)"
    "Bash(rm target/changelog/scenes/v$version/*)"
)
model=()
[ -z "${CHANGELOG_MODEL:-}" ] || model=(--model "$CHANGELOG_MODEL")

echo "writing the changelog entry for $version (since ${previous:-the start}) with Claude Code" >&2
claude -p "$prompt" "${model[@]}" \
    --permission-mode dontAsk \
    --allowedTools "${allowed[@]}" \
    --no-session-persistence

# Pictures of scenes that were dropped, or not used in the entry.
for picture in website/public/changelog/v"$version"/*; do
    [ -e "$picture" ] || continue
    grep -q "\"$(basename "$picture")\"" "$toml" 2>/dev/null || rm "$picture"
done
python3 website/tools/changelog.py
echo
echo "Review before committing:"
git status --short -- website/changelog website/public/changelog
echo "Scenes (not committed): target/changelog/scenes/v$version/"
