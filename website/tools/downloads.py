#!/usr/bin/env python3
"""Writes public/download/index.html from the premade ISOs' manifest.json.

    python3 website/tools/downloads.py                  from the published manifest
    python3 website/tools/downloads.py --manifest FILE  from a file or another URL

The ISO workflow (.github/workflows/iso.yml) publishes manifest.json next to
the ISOs, and the website deploy runs this before uploading, so the page lists
what is there. Without a manifest (none published yet, or no network) the page
says no ISO is ready. The template is tools/download.html; ISO links go
through /download/, which .htaccess redirects to the ISO bucket. Only the
standard library is used.
"""

import argparse
import html
import json
import sys
import urllib.error
import urllib.request
from datetime import date
from pathlib import Path

WEBSITE = Path(__file__).resolve().parent.parent
TEMPLATE = WEBSITE / "tools" / "download.html"
PAGE = WEBSITE / "public" / "download" / "index.html"
MARKER = "<!-- isos -->"
MANIFEST = "https://iso.su6.nl/manifest.json"
ARCHES = {"amd64": "PCs (amd64)", "arm64": "ARM computers (arm64)"}
COLOURS = ["", "y", "b"]


def load(source):
    if source.startswith(("https://", "http://")):
        # Cloudflare refuses Python's own user agent.
        request = urllib.request.Request(source, headers={"User-Agent": "tileception-website"})
        with urllib.request.urlopen(request, timeout=30) as response:
            return json.load(response)
    return json.loads(Path(source).read_text(encoding="utf-8"))


def size(n):
    return f"{n / 1e9:.1f} GB"


def until(eol):
    try:
        day = date.fromisoformat(eol)
    except (TypeError, ValueError):
        return ""
    return f"Supported until {day.strftime('%B %Y')}"


def release(version):
    """Ubuntu's release, without the point release: 24.04 for 24.04.5.1."""
    return ".".join(version.split(".")[:2])


def releases(isos):
    """The ISOs grouped by Ubuntu release, newest release first. One release's
    ISOs can be from different point releases (24.04.5.1 for amd64, 24.04.5
    for arm64), which each file shows."""
    groups = {}
    for iso in isos:
        groups.setdefault(release(iso["ubuntu"]), []).append(iso)
    order = sorted(groups, key=lambda v: [int(p) for p in v.split(".")], reverse=True)
    return [(version, sorted(groups[version], key=lambda i: list(ARCHES).index(i["arch"])
                             if i["arch"] in ARCHES else 99)) for version in order]


def render(manifest):
    isos = [iso for iso in manifest.get("isos", []) if iso.get("file")]
    if not isos:
        return (
            '      <section class="dl-release dl-none">\n'
            '        <div class="blk y"><p>No ISO is ready yet. Until then, install Tileception on '
            'Ubuntu or Debian with <a href="../#get">the command on the home page</a>.</p></div>\n'
            "      </section>\n"
        )
    out = []
    for n, (version, files) in enumerate(releases(isos)):
        first = files[0]
        kind = "LTS" if first.get("lts") else ""
        support = until(first.get("eol", ""))
        colour = COLOURS[n % len(COLOURS)]
        out.append('      <section class="dl-release">')
        out.append(f'        <div class="{"blk " + colour if colour else "blk"}">')
        out.append(f"          <h2>Ubuntu {html.escape(version)}{' ' + kind if kind else ''}</h2>")
        if support:
            out.append(f'          <p class="support">{html.escape(support)}</p>')
        out.append("        </div>")
        out.append('        <div class="blk dl-files">')
        for iso in files:
            name = html.escape(iso["file"])
            label = html.escape(ARCHES.get(iso["arch"], iso["arch"]))
            out.append('          <div class="dl-file">')
            out.append(f'            <a class="get" href="{name}" download>Download for {label}, '
                       f'{size(iso["size"])}</a>')
            out.append(f'            <span class="sum">Ubuntu {html.escape(iso["ubuntu"])} &middot; '
                       f'SHA256 {html.escape(iso["sha256"])}</span>')
            out.append("          </div>")
        out.append("        </div>")
        out.append("      </section>")
    return "\n".join(out) + "\n"


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--manifest", default=MANIFEST, help="manifest.json, as a URL or a file")
    parser.add_argument("--out", type=Path, default=PAGE, help="where to write the page")
    args = parser.parse_args()
    try:
        manifest = load(args.manifest)
    except urllib.error.HTTPError as error:
        # Only a missing manifest means none is published yet; anything else
        # would drop the published ISOs from the page.
        if error.code != 404:
            sys.exit(f"downloads: could not fetch {args.manifest}: {error}")
        print(f"downloads: no manifest at {args.manifest}; the page says no ISO is ready", file=sys.stderr)
        manifest = {"isos": []}
    template = TEMPLATE.read_text(encoding="utf-8")
    if MARKER not in template:
        sys.exit(f"downloads: {TEMPLATE} lacks {MARKER}")
    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text(template.replace(MARKER + "\n", render(manifest)), encoding="utf-8")
    print(f"downloads: wrote {args.out}")


if __name__ == "__main__":
    main()
