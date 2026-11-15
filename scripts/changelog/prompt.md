# Changelog entry prompt

The prompt the release step gives `claude -p` to write a release's entry on
tileception.space/changelog. `{version}` (such as 0.19.0) and `{previous}` (the
release before it) are filled in by the release step.

---

Write the changelog entry for Tileception {version} on tileception.space/changelog.
Readers use the desktop; they don't develop it.

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

Write `website/changelog/v{version}.toml` and nothing else:

```toml
version = "{version}"
date = 2026-11-13            # the tag's date
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
`website/changelog/areas.toml`; a new scope works without an entry there.

Then run `python3 website/tools/changelog.py` and fix whatever it reports.
