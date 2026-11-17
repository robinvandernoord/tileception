# tileception.space

The landing page: plain HTML, CSS and one script, no build step.

```
website/
  public/            everything that is uploaded to the web root
    index.html       the page; all content is in the markup
    assets/site.css  De Stijl look, stacked layout and zoom layout
    assets/zoom.js   the scroll-driven zoom
    assets/changelog.css, assets/changelog.js
                     the changelog page's tiles and before/after slider
    changelog/       the changelog page, generated, and each release's pictures
    assets/video     the promo video, its silent preview and their stills
    install          the `curl -fsSL https://tileception.space/install | sh` script:
                     asks how to set up, adds apt.tileception.space and installs
                     the package
    setup            a symlink to resources/tileception-setup, which install
                     fetches to ask its questions first (deploy uploads the file)
    .htaccess        Apache settings for OVH (HTTPS redirect, types, caching)
  changelog/
    v<version>.toml  what changed in a release, written for the changelog page
    areas.toml       the colour and name of each area (commit scope)
  tools/
    changelog.py     writes public/changelog/index.html from changelog/
    changelog.html   the changelog page's template
    check.sh         sanity checks, run in CI
    deploy.sh        mirrors public/ to OVH over FTP or SFTP
    og.html          source of og.png (the link preview image)
    render-images.js renders og.png and the PNG icons with Playwright
```

## How the page works

Each section is a `<section class="lvl">`. One block in each section, the portal,
contains the next section. With JavaScript and without reduced motion, `zoom.js`
adds `html.zoom`: every level becomes a fixed canvas (1600×1000, or 900×1500–1900
on portrait screens), placed into its parent's portal, and scrolling moves a
camera from one level into the next. Without it, the same markup is a stacked
page, which is also what search engines and screen readers read.

Colours have meanings: red is tile, yellow is nest, blue is zoom.

## The changelog page

`/changelog/` shows what changed in each release, with screenshots. Each
release has a `changelog/v<version>.toml`; `scripts/changelog/prompt.md`
describes its fields and how the entries are written. After changing the data,
the template or `areas.toml`, regenerate the page:

```sh
python3 website/tools/changelog.py
```

`check.sh` fails when the page is out of date, when a picture is missing, and
on dashes, curly quotes or ellipses in the text.

`make release` runs this for the new version after vommit. `make changelog
VERSION=0.19.0` writes a release's entry, scenes and pictures with Claude Code (`scripts/changelog/entry.sh`), which may only edit
files under `website/changelog/`. Nothing is committed: review the TOML, the
scenes and the pictures first. Without `VERSION` it takes the latest tag;
`FORCE=1` rewrites an entry that exists.

Screenshots come from scenes in `target/changelog/scenes/v<version>/`,
working files that are not committed, run in headless Tileceptions with this
release and the one before:

```sh
scripts/changelog/shoot.sh 0.19.0          # every scene of 0.19.0
scripts/changelog/shoot.sh 0.19.0 desk     # one scene
```

`scripts/changelog/build.sh` builds each release once on the cargo-plane
builder, in a worktree at `target/changelog/worktree`, and keeps the binary in
`target/changelog/bin/`. The scenes run locally, so they need a render node
(or Mesa's software EGL device) and Chromium. `scripts/changelog/lib.sh`
lists the helpers a scene can use; a scene with the line `# shots: after`
runs only with the new release.

## Preview locally

```sh
python3 -m http.server -d website/public 8000
```

Then open http://localhost:8000. You can also open `website/public/index.html`
directly in your browser; its stylesheet and script use relative paths.

After changing `tools/og.html` or `public/favicon.svg` (the source of the PNG icons and `favicon.ico`):

```sh
node website/tools/render-images.js
```

## Deployment

`.github/workflows/website.yml` runs `check.sh` on pull requests and on pushes to
`master`. Pushes to `master` that touch `website/` then upload `public/` to the
OVH web hosting with `lftp mirror` and fetch the live page as a smoke test.
Files in the remote web root that are not in `public/` are deleted.

Set these on GitHub under Settings → Environments → `website` (or as
repository secrets and variables):

| Name               | Kind     | Value                                                                        |
| ------------------ | -------- | ---------------------------------------------------------------------------- |
| `OVH_FTP_HOST`     | secret   | FTP server from the OVH control panel, e.g. `ftp.cluster0XX.hosting.ovh.net` |
| `OVH_FTP_USER`     | secret   | FTP login                                                                    |
| `OVH_FTP_PASSWORD` | secret   | its password                                                                 |
| `OVH_FTP_PROTOCOL` | variable | `ftp` (default) or `sftp` on plans with SSH access                           |
| `OVH_FTP_DIR`      | variable | remote web root, default `www`                                               |

OVH lists the FTP server and login under Web Cloud → Hosting plans → your plan →
FTP-SSH. Enable the free SSL certificate for `tileception.space` there as well;
`.htaccess` redirects every request to `https://tileception.space`.
