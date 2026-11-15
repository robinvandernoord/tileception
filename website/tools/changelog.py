#!/usr/bin/env python3
"""Writes public/changelog/index.html from website/changelog/v*.toml.

    python3 website/tools/changelog.py           write the page
    python3 website/tools/changelog.py --check   fail if the page is out of date

Each release's TOML holds what changed in it (see scripts/changelog/prompt.md
for the fields); its images live in public/changelog/v<version>/. The page
template is tools/changelog.html. Only the standard library is used, so CI
needs nothing installed.
"""

import argparse
import html
import re
import sys
import tomllib
from pathlib import Path

WEBSITE = Path(__file__).resolve().parent.parent
DATA = WEBSITE / "changelog"
PUBLIC = WEBSITE / "public"
PAGE = PUBLIC / "changelog" / "index.html"
TEMPLATE = WEBSITE / "tools" / "changelog.html"
MARKER = "<!-- releases -->"
ISSUES = "https://github.com/robinvandernoord/tileception/issues/"

COLOURS = {
    "red": "var(--red)",
    "yellow": "var(--yellow)",
    "blue": "var(--blue)",
    "white": "var(--canvas)",
}
# Tiles without a picture take these in turn, so the grid stays De Stijl.
TILE_COLOURS = ["", "y", "", "b", "", "r"]
# Punctuation the site never uses: plain ASCII instead.
BANNED = {
    "—": "em dash",
    "–": "en dash",
    "…": "ellipsis",
    "“": "curly quote",
    "”": "curly quote",
    "‘": "curly apostrophe",
    "’": "curly apostrophe",
}

RELEASE_KEYS = {"version", "date", "lede", "features", "fixes"}
FEATURE_KEYS = {"area", "issue", "title", "text", "media"}
FIX_KEYS = {"area", "issue", "text"}
MEDIA_KEYS = {
    "compare": ({"before", "after"}, {"caption", "alt"}),
    "image": ({"src"}, {"caption", "alt"}),
    "video": ({"src"}, {"poster", "caption", "alt"}),
}


class DataError(Exception):
    pass


def version_key(version):
    return tuple(int(part) for part in version.split("."))


def check_keys(where, table, required, optional=frozenset()):
    missing = required - table.keys()
    if missing:
        raise DataError(f"{where}: missing {', '.join(sorted(missing))}")
    unknown = table.keys() - required - optional
    if unknown:
        raise DataError(f"{where}: unknown {', '.join(sorted(unknown))}")


def check_text(where, value):
    if not isinstance(value, str):
        raise DataError(f"{where}: expected text, got {value!r}")
    for character, name in BANNED.items():
        if character in value:
            raise DataError(f"{where}: {name} in {value!r}; use plain ASCII punctuation")


def load_release(path):
    with path.open("rb") as file:
        release = tomllib.load(file)
    where = path.name
    check_keys(where, release, {"version", "date"}, RELEASE_KEYS)
    version = str(release["version"])
    if path.stem != f"v{version}":
        raise DataError(f"{where}: holds version {version}, so it should be v{version}.toml")
    release["date"] = str(release["date"])
    release.setdefault("lede", "")
    release.setdefault("features", [])
    release.setdefault("fixes", [])
    check_text(f"{where} lede", release["lede"])
    if not release["features"] and not release["fixes"]:
        raise DataError(f"{where}: no features and no fixes")
    folder = PUBLIC / "changelog" / f"v{version}"
    for index, feature in enumerate(release["features"]):
        spot = f"{where} feature {index + 1}"
        check_keys(spot, feature, {"area", "title"}, FEATURE_KEYS)
        feature.setdefault("text", "")
        for key in ("area", "title", "text"):
            check_text(f"{spot} {key}", feature[key])
        media = feature.get("media")
        if media is None:
            continue
        kind = media.get("kind")
        if kind not in MEDIA_KEYS:
            raise DataError(f"{spot}: media kind must be one of {', '.join(MEDIA_KEYS)}")
        files, optional = MEDIA_KEYS[kind]
        check_keys(f"{spot} media", media, files | {"kind"}, optional)
        for key in files | ({"poster"} & media.keys()):
            if not (folder / media[key]).is_file():
                raise DataError(f"{spot}: {media[key]} is not in {folder.relative_to(WEBSITE)}")
        for key in ("caption", "alt") & media.keys():
            check_text(f"{spot} {key}", media[key])
    for index, fix in enumerate(release["fixes"]):
        spot = f"{where} fix {index + 1}"
        check_keys(spot, fix, {"area", "text"}, FIX_KEYS)
        for key in ("area", "text"):
            check_text(f"{spot} {key}", fix[key])
    return release


def load_areas():
    with (DATA / "areas.toml").open("rb") as file:
        areas = tomllib.load(file)
    for key, area in areas.items():
        check_keys(f"areas.toml [{key}]", area, set(), {"name", "colour"})
        if area.get("colour", "white") not in COLOURS:
            raise DataError(f"areas.toml [{key}]: colour must be one of {', '.join(COLOURS)}")
    return areas


def prose(text):
    """Escapes text, turning `code` into <code>."""
    return re.sub(r"`([^`]+)`", r"<code>\1</code>", html.escape(text, quote=False))


class Page:
    def __init__(self, areas):
        self.areas = areas

    def area(self, key):
        area = self.areas.get(key, {})
        name = area.get("name", key.replace("-", " ").capitalize())
        colour = COLOURS[area.get("colour", "white")]
        return f'<span class="area" style="--c: {colour}">{html.escape(name)}</span>'

    @staticmethod
    def issue(number):
        if not number:
            return ""
        return (
            f' <a class="issue" href="{ISSUES}{int(number)}" target="_blank" rel="noopener">'
            f"#{int(number)}</a>"
        )

    def labels(self, item):
        return f'<div class="labels">{self.area(item["area"])}{self.issue(item.get("issue"))}</div>'

    @staticmethod
    def media(version, feature):
        media = feature.get("media")
        if not media:
            return ""
        folder = f"v{version}/"
        alt = html.escape(media.get("alt", feature["title"]), quote=True)
        kind = media["kind"]
        if kind == "compare":
            inner = (
                '<div class="compare" style="--split: 50%" tabindex="0" role="slider" '
                'aria-label="Before and after" aria-valuemin="0" aria-valuemax="100" aria-valuenow="50">'
                f'<img src="{folder}{media["before"]}" alt="Before: {alt}" loading="lazy" />'
                f'<div class="after-layer"><img src="{folder}{media["after"]}" alt="After: {alt}" loading="lazy" /></div>'
                '<div class="handle" aria-hidden="true"></div>'
                '<span class="label before">Before</span><span class="label after">After</span>'
                "</div>"
            )
        elif kind == "video":
            poster = f' poster="{folder}{media["poster"]}"' if "poster" in media else ""
            inner = (
                f'<video src="{folder}{media["src"]}"{poster} autoplay muted loop playsinline '
                f'aria-label="{alt}"></video>'
            )
        else:
            inner = f'<img src="{folder}{media["src"]}" alt="{alt}" loading="lazy" />'
        return f'<div class="media">{inner}</div>'

    @staticmethod
    def caption(feature):
        media = feature.get("media") or {}
        if not media.get("caption"):
            return ""
        return f'<p class="caption">{prose(media["caption"])}</p>'

    def tile(self, version, feature, colour=""):
        text = f'<p>{prose(feature["text"])}</p>' if feature["text"] else ""
        return (
            f'<article class="blk cl-tile {colour}">{self.media(version, feature)}'
            f'<div class="text">{self.labels(feature)}<h3>{prose(feature["title"])}</h3>{text}'
            f"{self.caption(feature)}</div></article>"
        )

    def heading(self, release):
        lede = f'<p>{prose(release["lede"])}</p>' if release["lede"] else ""
        return (
            f'<div class="blk cl-version"><div><h2 class="display">{release["version"]}</h2>'
            f'<time datetime="{release["date"]}">{release["date"]}</time></div>{lede}</div>'
        )

    def fixes(self, release):
        if not release["fixes"]:
            return ""
        items = "".join(
            f'<li>{self.area(fix["area"])}{self.issue(fix.get("issue"))} {prose(fix["text"])}</li>'
            for fix in release["fixes"]
        )
        return f'<div class="blk k cl-fixes"><h3>Fixed</h3><ul>{items}</ul></div>'

    def compact(self, release):
        """A release without pictures: one block with its lists."""
        version = release["version"]
        items = "".join(
            f'<li>{self.area(feature["area"])}{self.issue(feature.get("issue"))} {prose(feature["title"])}</li>'
            for feature in release["features"]
        )
        items += "".join(
            f'<li>{self.area(fix["area"])}{self.issue(fix.get("issue"))} <b>Fixed:</b> {prose(fix["text"])}</li>'
            for fix in release["fixes"]
        )
        lede = f'<p>{prose(release["lede"])}</p>' if release["lede"] else ""
        return (
            f'<section class="cl-release" id="v{version}"><div class="blk cl-compact">'
            f'<div><h2 class="display">{version}</h2><time datetime="{release["date"]}">{release["date"]}</time></div>'
            f"<div>{lede}<ul>{items}</ul></div></div></section>"
        )

    def release(self, release):
        version = release["version"]
        features = release["features"]
        if not any(feature.get("media") for feature in features):
            return self.compact(release)
        # The first feature with a picture is the large tile; up to two more
        # with pictures stand beside it, and the rest form a grid below.
        hero = next(feature for feature in features if feature.get("media"))
        rest = [feature for feature in features if feature is not hero]
        side = [feature for feature in rest if feature.get("media")][:2]
        others = [feature for feature in rest if all(feature is not chosen for chosen in side)]
        text = f'<p>{prose(hero["text"])}</p>' if hero["text"] else ""
        parts = [
            f'<section class="cl-release" id="v{version}">',
            self.heading(release),
            f'<article class="blk cl-hero{"" if side else " wide"}">{self.media(version, hero)}'
            f'<div class="text">{self.labels(hero)}<h3 class="display">{prose(hero["title"])}</h3>'
            f"{text}{self.caption(hero)}</div></article>",
        ]
        if side:
            parts.append(
                '<div class="blk cl-side">' + "".join(self.tile(version, feature) for feature in side) + "</div>"
            )
        if others:
            tiles = "".join(
                self.tile(version, feature, "" if feature.get("media") else TILE_COLOURS[index % len(TILE_COLOURS)])
                for index, feature in enumerate(others)
            )
            parts.append(f'<div class="blk cl-grid">{tiles}</div>')
        parts.append(self.fixes(release))
        parts.append("</section>")
        return "\n".join(part for part in parts if part)


def render():
    releases = [load_release(path) for path in DATA.glob("v*.toml")]
    releases.sort(key=lambda release: version_key(release["version"]), reverse=True)
    page = Page(load_areas())
    body = "\n".join(page.release(release) for release in releases)
    template = TEMPLATE.read_text(encoding="utf-8")
    if MARKER not in template:
        raise DataError(f"{TEMPLATE.name} lacks {MARKER}")
    return template.replace(MARKER, body)


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--check", action="store_true", help="fail if the page is out of date")
    arguments = parser.parse_args()
    try:
        output = render()
    except (DataError, tomllib.TOMLDecodeError) as error:
        sys.exit(f"changelog: {error}")
    if arguments.check:
        if not PAGE.is_file() or PAGE.read_text(encoding="utf-8") != output:
            sys.exit("changelog: public/changelog/index.html is out of date; run python3 website/tools/changelog.py")
        return
    PAGE.parent.mkdir(parents=True, exist_ok=True)
    PAGE.write_text(output, encoding="utf-8")


if __name__ == "__main__":
    main()
