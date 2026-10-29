#!/bin/sh
# Sanity checks for the static site. Run from the repository root: website/tools/check.sh
set -eu
cd "$(dirname "$0")/../public"
fail() { echo "check: $*" >&2; exit 1; }

for f in index.html 404.html .htaccess install setup robots.txt sitemap.xml site.webmanifest \
         favicon.svg favicon-32.png apple-touch-icon.png icon-192.png icon-512.png og.png \
         assets/site.css assets/zoom.js; do
  [ -s "$f" ] || fail "missing $f"
done

# Every local asset the pages reference must exist.
for page in index.html 404.html; do
  grep -o '\(href\|src\)="/[^"]*"' "$page" | sed 's/.*="\/\([^"#?]*\).*/\1/' | while read -r ref; do
    [ -z "$ref" ] || [ -e "$ref" ] || fail "$page references missing /$ref"
  done
done
grep -o 'url("[^"]*")' assets/site.css | sed 's/url("\(.*\)")/\1/' | while read -r ref; do
  [ -e "assets/$ref" ] || fail "site.css references missing $ref"
done

# Metadata that link previews and search engines read.
for tag in 'rel="canonical"' 'name="description"' 'property="og:title"' 'property="og:image"' \
           'name="twitter:card"' 'application/ld+json'; do
  grep -q "$tag" index.html || fail "index.html lacks $tag"
done
[ "$(grep -c '<h1' index.html)" = 1 ] || fail "index.html needs exactly one h1"

python3 - <<'PY'
import json, re, sys, xml.dom.minidom
html = open('index.html', encoding='utf-8').read()
for block in re.findall(r'<script type="application/ld\+json">(.*?)</script>', html, re.S):
    json.loads(block)
json.load(open('site.webmanifest'))
xml.dom.minidom.parse('sitemap.xml')
PY

sh -n install
# The installer fetches setup from the site; it must be the one the packages ship.
cmp -s setup ../../resources/tileception-setup || fail "setup differs from resources/tileception-setup; copy it over"
if command -v node >/dev/null 2>&1; then node --check assets/zoom.js; fi
echo "check: ok"
