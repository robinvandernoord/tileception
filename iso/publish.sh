#!/bin/sh
# Publishes the premade ISOs' index after CI uploaded new ISOs:
#
#   iso/publish.sh ENTRIES_DIR [published manifest.json URL]
#
# Merges the entries of the ISOs just built (ENTRIES_DIR/*.json, written by
# iso/build.sh) into the published manifest, drops releases past their end of
# support, has the apt server sign SHA256SUMS with the archive key, and
# uploads manifest.json, SHA256SUMS and SHA256SUMS.gpg to the ISO bucket.
# ISOs in the bucket that the manifest no longer lists are deleted.
#
# Environment:
#   ISO_BUCKET                  the R2 bucket
#   R2_ENDPOINT                 https://<account>.r2.cloudflarestorage.com
#   AWS_ACCESS_KEY_ID           an R2 API token's key, with write access to the bucket
#   AWS_SECRET_ACCESS_KEY       and its secret
#   APT_SSH_HOST, APT_SSH_PORT  the apt server, as for release.yml's publish job,
#                               with the key in ~/.ssh/apt
# Needs jq, aws (the AWS CLI), ssh and distro-info.
set -eu
cd "$(dirname "$0")/.."

entries=${1:?usage: iso/publish.sh ENTRIES_DIR [MANIFEST_URL]}
source=${2:-}
: "${ISO_BUCKET:?set ISO_BUCKET}" "${R2_ENDPOINT:?set R2_ENDPOINT}" "${APT_SSH_HOST:?set APT_SSH_HOST}"
export AWS_DEFAULT_REGION=auto
s3() { aws s3 --endpoint-url "$R2_ENDPOINT" "$@"; }

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

old='{"isos": []}'
[ -z "$source" ] || old=$(curl -fsSL "$source" 2>/dev/null || echo "$old")
supported=$(ubuntu-distro-info --supported | jq -R . | jq -sc .)

# New entries replace old ones of the same release and architecture.
jq -s . "$entries"/*.json > "$work/new.json"
echo "$old" | jq --slurpfile new "$work/new.json" --argjson supported "$supported" '
    {
      generated: (now | strftime("%Y-%m-%d")),
      isos: (
        [.isos[] | select(. as $o | $new[0] | all(.codename != $o.codename or .arch != $o.arch))]
        + $new[0]
        | map(select(.codename as $c | $supported | index($c)))
        | sort_by(.ubuntu | split(".") | map(tonumber)) | reverse
      )
    }' > "$work/manifest.json"
jq -r '.isos[] | "\(.sha256)  \(.file)"' "$work/manifest.json" > "$work/SHA256SUMS"
[ -s "$work/SHA256SUMS" ] || { echo "iso/publish.sh: no ISOs to publish" >&2; exit 1; }

ssh -i ~/.ssh/apt -p "${APT_SSH_PORT:-2222}" -o BatchMode=yes publish@"$APT_SSH_HOST" sign-checksums \
    < "$work/SHA256SUMS" > "$work/SHA256SUMS.gpg"
grep -q "BEGIN PGP SIGNATURE" "$work/SHA256SUMS.gpg" || { echo "iso/publish.sh: no signature came back" >&2; exit 1; }

for file in manifest.json SHA256SUMS SHA256SUMS.gpg; do
    s3 cp "$work/$file" "s3://$ISO_BUCKET/$file" --cache-control "max-age=300"
done

s3 ls "s3://$ISO_BUCKET/" | awk '{ print $4 }' | grep -E '^tileception-ubuntu-.*\.iso$' | while read -r file; do
    if ! jq -e --arg f "$file" 'any(.isos[]; .file == $f)' "$work/manifest.json" >/dev/null; then
        echo "Removing $file, which the manifest no longer lists"
        s3 rm "s3://$ISO_BUCKET/$file"
    fi
done
jq -r '.isos[] | "Published \(.file)"' "$work/manifest.json"
