# tileception-apps

Standalone launcher support. This crate has its own empty workspace and does not
depend on Smithay or connect to the desktop session.

```rust
use std::collections::HashMap;
use tileception_apps::{command_line, AppIndex, IconLookup};

let mut index = AppIndex::scan_default();
let icons = IconLookup::new("hicolor", &IconLookup::default_dirs());
for app in index.search("editor", &HashMap::new(), 10) {
    let command = command_line(app, "ghostty -e");
    let icon = app.icon.as_deref().and_then(|name| icons.find(name, 32, 1));
    // Pass command and icon to the launcher.
}
if index.changed_since_scan() {
    index.refresh();
}
```

`AppIndex::scan(dirs, desktop)` accepts application directories in priority order.
`scan_default()` uses XDG data directories, the message locale (LC_ALL,
LC_MESSAGES, LANG), PATH, and XDG_CURRENT_DESKTOP (default Tileception).
`scan_with_options(dirs, desktop, locale, path)` makes all those choices explicit
for tests and callers that manage their own environment. Refresh retains the
original scan configuration. Nested directories are included; directory symlinks
are not followed to avoid cycles. Missing or unreadable paths and malformed
entries are ignored. The first desktop id reserves its position even if invalid
or disabled, preventing a lower-priority copy from reappearing.

`get(id)` also returns hidden entries, including minimal Hidden=true tombstones.
Search excludes hidden entries and precomputes Unicode decomposition, accent
removal, and lowercase text. Name match quality takes precedence over usage
count, then last-used time, then alphabetical name and id. Empty queries sort
only by usage and name. `Usage` contains `count` and `SystemTime last_used`.
`App` exposes the parsed desktop metadata and localized `keywords`.

`App.exec` is shell-quoted for `sh -c`, with desktop field codes removed and
literal percent codes decoded. Unknown field codes and unbalanced quoting reject
the entry. Shell metacharacters in desktop arguments remain literal. The
terminal prefix passed to `command_line` is trusted shell configuration; it
should include any execution flag, such as `ghostty -e`. An empty terminal prefix
returns the app command unchanged. The library never executes commands.

`IconLookup::new(theme, roots)` reads each theme index once, follows inheritance
with cycle detection, and ends with hicolor. It selects exact nominal size and
scale first, then the closest physical size within that theme. Supported formats
are PNG, SVG, and XPM, in that order. Unthemed roots, sibling pixmaps directories,
and `/usr/share/pixmaps` provide final fallbacks. Absolute icon paths must exist.
Both successful lookups and misses are cached. Recreate IconLookup after icon
installation or theme changes. Icon roots default to XDG_DATA_HOME/icons and
XDG_DATA_DIRS/icons.

`changed_since_scan` compares directory and desktop-file mtimes and lengths,
including nested paths. It detects additions, removals, and ordinary file edits;
changes that preserve both mtime and length cannot be detected. It checks files
without reparsing entries. Re-run or refresh the index if executable availability
changes without an application-directory change.

Run from this directory:

```sh
cargo test
cargo clippy -- -D warnings
cargo fmt --check
```

Fixture tests cover locale fallback, quoting, visibility, overrides, TryExec,
ranking, usage, refresh, icon inheritance, scaling, and caching. No desktop
session is used. Parsing and icon selection follow the
[Desktop Entry specification](https://specifications.freedesktop.org/desktop-entry/latest/)
and [Icon Theme specification](https://specifications.freedesktop.org/icon-theme/latest/),
with field codes intentionally removed as required by this launcher's API.
