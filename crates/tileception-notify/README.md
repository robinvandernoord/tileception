# tileception-notify

Standalone, non-visual Desktop Notifications Specification 1.2 service. This
crate is intentionally outside the compositor workspace until integration.

```rust,no_run
use tileception_notify::{Config, NotifyService, Request};

// Run startup during initialization. Subsequent requests only enqueue work.
let (service, events) = NotifyService::start(Config::default(), None, || {
    // Ping the compositor's calloop loop here.
}).unwrap();

// In the compositor's event loop, drain events.try_iter() to update its UI.
service.send(Request::SetDoNotDisturb(true));
let history = service.history();
// Keep service alive for the compositor's lifetime.
```

`NotifyService` owns its service thread and D-Bus connection. Startup waits for
name acquisition and reports a clear error if another daemon owns the name.
`Config::replace` opts into replacing an owner that allows replacement. This
service allows replacement by a subsequent daemon. Dropping the service joins
the thread and releases its name.

`start` accepts `Some(address)` for an explicit bus, or `None` for the session
bus. Never run an experiment with `None` on a desktop running another daemon.
The example above is for eventual compositor integration.

Events are `Shown(Notification)`, `Replaced(Notification)`, and
`Closed { id, reason }`. `Shown` includes notifications suppressed by do not
disturb; check `show_popup` before displaying a popup. Wake callbacks execute
on background D-Bus/service threads while state is locked. They must only
signal the event loop, return promptly, and never call back into the service.
Callbacks must not panic.

Requests support dismissing one/all active notifications, invoking an advertised
action with an optional activation token, toggling do not disturb, and removing
or clearing history. Invoking a non-resident notification closes it as dismissed;
resident notifications remain active. Invalid or stale action requests are
ignored. Tokens precede `ActionInvoked`. The `default` action is represented by
`default_action`; other actions retain their key and label in `actions`.

Requests are asynchronous, including toggling do not disturb and history edits.
`history()` and `do_not_disturb()` provide synchronized snapshots, so a read
immediately following `send()` can precede the requested change. History is
oldest first, includes active and closed notifications, and excludes transient
notifications. Replacements update the existing entry and move it to the newest
position. Clearing/removing history does not dismiss active notifications.

Defaults are a five second timeout for low/normal urgency, no default timeout for
critical urgency, 100 history entries, and critical popups during do not disturb.
Timeout `0` never expires; positive milliseconds override urgency defaults; `-1`
uses configured defaults. Other negative values use the same default policy.
The service expires notifications itself, with up to 50 ms scheduling latency.
Replacement resets the timeout atomically. `created` is the latest submission
time. Existing popups are unaffected by toggling do not disturb.

Raw RGB/RGBA images must use eight bits per channel and valid dimensions,
channels, stride, and storage. Rows are converted to tightly packed RGBA, with a
64 MiB decoded image limit. Priority is image-data/image_data, then
image-path/image_path, then deprecated icon_data. Invalid raw hints are ignored
and can fall back to another image source. Paths/theme names are exposed without
loading files. Notification hints also expose urgency, category, desktop entry,
resident/transient, sound suppression/file, and optional x/y coordinates.

Bodies are sanitized with an HTML parser, retaining only `b`, `i`, `u`, `a href`,
and `img src/alt`. Links allow http, https, mailto, and file; images only allow
absolute local paths or file:/// URLs. Event handlers, unknown tags/attributes,
scripts, and unsafe URLs are removed. The renderer remains responsible for safe
link activation and file/image loading. No sounds or UI are produced here.

## Validation

Install Rust and `dbus-daemon`, then run from this directory:

```sh
cargo test
cargo clippy -- -D warnings
cargo fmt --check
```

Every integration test launches its own
`dbus-daemon --session --print-address --nofork`, connects only to its printed
address, and kills/reaps the daemon afterward. Tests never connect to the real
session bus. Coverage includes methods/introspection, replacement, action/token
signals, close reasons, timers, critical defaults, do not disturb, history,
transient/resident behavior, image aliases/decoding, markup, name conflicts,
explicit replacement, and release on shutdown.

Protocol reference: <https://specifications.freedesktop.org/notification/1.2/protocol.html>.
