#!/bin/sh
# Prints, as a JSON list for a CI matrix, the ISOs that need building:
#
#   SUITES="noble resolute" iso/plan.sh [manifest.json URL or file]
#
# One per supported release in SUITES (ubuntu-distro-info --supported) and
# each architecture in ARCHES (default "amd64 arm64") that Ubuntu publishes a
# Desktop ISO for, whose entry in the published manifest is missing, was made
# from another Ubuntu ISO, or with another recipe (iso/recipe.sh). FORCE=1
# lists them all. Needs curl, gpgv, jq, distro-info and ubuntu-keyring.
set -eu
cd "$(dirname "$0")/.."

suites=${SUITES:?set SUITES to the releases apt.tileception.space has packages for}
arches=${ARCHES:-amd64 arm64}
source=${1:-}
recipe=$(iso/recipe.sh)
supported=$(ubuntu-distro-info --supported | tr "\n" " ")

manifest='{"isos": []}'
if [ -f "$source" ]; then
    manifest=$(cat "$source")
elif [ -n "$source" ]; then
    manifest=$(curl -fsSL "$source" 2>/dev/null || echo '{"isos": []}')
fi

for codename in $suites; do
    case " $supported " in *" $codename "*) ;; *) continue ;; esac
    for arch in $arches; do
        latest=$(iso/latest.sh "$codename" "$arch" 2>/dev/null) || continue
        sum=${latest#* }
        current=$(echo "$manifest" | jq -r --arg c "$codename" --arg a "$arch" \
            '.isos[] | select(.codename == $c and .arch == $a) | "\(.from_sha256) \(.recipe)"')
        if [ "${FORCE:-0}" = 1 ] || [ "$current" != "$sum $recipe" ]; then
            jq -nc --arg c "$codename" --arg a "$arch" '{codename: $c, arch: $a}'
        fi
    done
done | jq -sc .
