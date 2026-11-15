# Changelog entry prompt

The prompt `scripts/changelog/entry.sh` (`make changelog`) gives `claude -p`
to write a release's entry on tileception.space/changelog: everything after
the line below. `{version}` (such as 0.19.0), `{previous}` (the release before
it) and `{date}` (the tag's date) are filled in.

---

Write the changelog entry for Tileception {version} on tileception.space/changelog.
Readers use the desktop; they don't develop it. You write the entry's text,
take its screenshots and check them; Robin reviews the result.

You can only write this release's files: `website/changelog/v{version}.toml`
and the scenes in `target/changelog/scenes/v{version}/`. Other releases'
entries, pictures and scenes stay as they are. You can run `git log`,
`git show`, `git tag`, `git diff`, `gh issue view`,
`scripts/changelog/shoot.sh`, `python3 website/tools/changelog.py`, `ls`, and
`rm` for a scene in `target/changelog/scenes/v{version}/`.
Other commands are refused, so don't try them; run each command on its own,
without pipes.

If the `humanize-writing` skill is available, use it (generation mode, editorial
register) and run its self-audit on everything you write before you finish.

## Sources

Use only these. Never add a fact they don't support; leave the claim out instead.

- `git log --format='%s%n%b' v{previous}..v{version}`
- `gh issue view <n>` for every issue a commit closes. The bodies are detailed and
  often have a "Before" section.
- `docs/*.md` for how a feature is described to users, its keys and its settings.

## What to include

- Every `feat` commit, merged when several commits make up one change.
- `fix` and `perf` commits that repair something users of {previous} could run
  into. Leave out fixes to things added in this same release: nobody had them.
- Nothing from `docs`, `chore`, `refactor`, `test` or `build` commits, and nothing
  about the website.

## How to write

- Get straight to the point. Short sentences with a simple structure: subject, verb,
  what it does. No chains of clauses, no semicolons joining two thoughts, no
  sentences that need rereading.
- Name the real thing. Give the actual key (Super+N, Backspace), setting
  (`reduce_motion`, `[calendar]` in `config.toml`), command
  (`tileception msg plugin add`) or Settings page, so readers can act on it.
- Keep it short. A feature's text is one or two sentences, at most about 35 words.
  The `lede` is one or two sentences about the release's biggest changes. A fix is
  one sentence saying what works now, from the user's side.
- Titles are plain statements of what changed. No slogans or catchy fragments
  ("Every update in one window").
- A playful line is welcome when it is true and specific ("Headphones look like
  headphones, mice like mice"). Filler that only describes the page or the release
  ("Every release, newest first") is not.
- Plain ASCII punctuation only: no em or en dashes, no curly quotes, no ellipsis
  character. The page generator refuses them.
- Put file names, settings and commands in backticks; the page shows them as code.

## Output

Write `website/changelog/v{version}.toml` in this shape:

```toml
version = "{version}"
date = {date}
lede = "..."

[[features]]                 # the most visible change first: it gets the large tile
area = "desk"                # the commit scope
issue = 87                   # leave out when there is none
title = "..."
text = "..."

[features.media]             # only for screenshots that exist, see below
kind = "compare"             # or "image" (src = ...) or "video" (src, poster)
before = "desk-before.jpg"
after = "desk-after.jpg"
alt = "Super+D with four windows open"
caption = "Super+D in 0.18.0, and Super+D held in 0.19.0."

[[fixes]]
area = "launcher"
issue = 12                   # optional
text = "..."
```

Media files live in `website/public/changelog/v{version}/`. Only refer to files
that are there. An area's colour and display name come from
`website/changelog/areas.toml`; a new scope works without an entry there, so
leave that file alone.

## Screenshots

Write the text first, then pick up to four features that a picture shows
better than words: something on screen that looks or behaves differently.
Skip what a still picture can't show, such as settings files, plugins,
behaviour across machines or anything that needs hardware.

For each, write a scene: `target/changelog/scenes/v{version}/<name>.sh`. Scenes
are working files: only the pictures are kept.
Read `scripts/changelog/lib.sh` and `scripts/promo/lib.sh` for the helpers,
and any scenes of other releases in `target/changelog/scenes/` for examples.
`target/changelog/msg-help-{version}.txt` lists the commands an instance takes
in {version}; `tileception msg` is not available to you directly, only
through a scene. A scene can write its own `config.toml` next to itself and
pass it to `start`.

- A change to something that existed in {previous} gets a before/after pair:
  the scene runs with both releases, `$ROLE` says which, and both shots show
  the same situation. Name them in the TOML as `<name>-before.jpg` and
  `<name>-after.jpg` with `kind = "compare"`, and say in the caption what each
  side shows.
- Something new in {version} has no "before": give the scene a line
  `# shots: after` and use `kind = "image"` with `<name>-after.jpg`.

Run `scripts/changelog/shoot.sh {version} <name>` and look at every picture
it reports. Check that it shows what the entry says, at a size where the
change can be seen. If a scene fails or its picture is wrong, fix the scene
and run it again. After three tries, delete the scene with `rm` and leave that
feature without media; its pictures are removed afterwards. A missing picture is better than a misleading one.

Then add the media to the TOML, run `python3 website/tools/changelog.py` and
fix whatever it reports. End with a short report: what you wrote, which
scenes you kept or dropped and why, and any claim you left out for lack of a
source.
