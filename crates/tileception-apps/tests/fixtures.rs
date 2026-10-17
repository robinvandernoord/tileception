#![allow(clippy::unwrap_used, reason = "test setup and assertions should fail immediately on unexpected errors")]

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};
use tileception_apps::{action_command_line, command_line, command_line_with, AppIndex, IconLookup, Usage};

static NEXT: AtomicU64 = AtomicU64::new(0);

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "tileception-apps-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    fn write(&self, path: &str, text: &str) -> PathBuf {
        let path = self.0.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, text).unwrap();
        path
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn fixture_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn app_dirs() -> Vec<PathBuf> {
    ["apps/first", "apps/second"].iter().map(|path| fixture_root().join(path)).collect()
}

fn scan(locale: &str, desktop: &str) -> AppIndex {
    AppIndex::scan_with_options(&app_dirs(), desktop, locale, &[])
}

#[test]
fn discovery_locales_visibility_and_override() {
    let index = scan("nl_NL.UTF-8@formal", "Tileception:GNOME");
    let app = index.get("editor.desktop").unwrap();
    assert_eq!(app.name, "Caf\u{e9} Editor");
    assert_eq!(app.generic_name.as_deref(), Some("Tekstbewerker"));
    assert_eq!(app.comment.as_deref(), Some("Bewerk tekst"));
    assert_eq!(app.keywords, ["tekst", "programmeren", "semi;colon"]);
    assert_eq!(app.categories, ["Utility", "TextEditor"]);
    assert_eq!(app.wm_class.as_deref(), Some("CafeEditor"));
    assert!(!app.hidden);
    for id in ["hidden.desktop", "nodisplay.desktop", "excluded.desktop", "other.desktop"] {
        assert!(index.get(id).unwrap().hidden, "{id}");
    }
    assert_eq!(index.search("", &HashMap::new(), 100).len(), 1);
    assert!(index.get("missing.desktop").is_none());
    assert!(index.get("nested-tool.desktop").is_none());
    assert_eq!(scan("nl_BE.UTF-8@formal", "Tileception").get("editor.desktop").unwrap().name, "Formal Editor");
    assert_eq!(scan("nl_BE.UTF-8", "Tileception").get("editor.desktop").unwrap().name, "Caf\u{e9} Bewerker");
    assert_eq!(scan("C.UTF-8", "Tileception").get("editor.desktop").unwrap().name, "Cafe Editor");
}

#[test]
fn desktop_actions() {
    let fixture = Fixture::new();
    fixture.write(
        "apps/browser.desktop",
        "[Desktop Entry]\nType=Application\nName=Browser\nExec=browser %u\nActions=new-window;private;broken;missing;\n\n\
         [Desktop Action new-window]\nName=New Window\nName[nl]=Nieuw venster\nExec=browser --new-window %u\n\n\
         [Desktop Action private]\nName=New Private Window\nIcon=browser-private\nExec=browser --private-window\n\n\
         [Desktop Action broken]\nName=Broken\nExec=browser %Z\n\n\
         [Desktop Action unlisted]\nName=Unlisted\nExec=browser --unlisted\n",
    );
    fixture.write(
        "apps/hidden.desktop",
        "[Desktop Entry]\nType=Application\nName=Hidden\nExec=hidden\nNoDisplay=true\nActions=new;\n\n\
         [Desktop Action new]\nName=New Window\nExec=hidden --new\n",
    );
    let index = AppIndex::scan_with_options(&[fixture.0.join("apps")], "Tileception", "C", &[]);
    let app = index.get("browser.desktop").unwrap();
    let names = app.actions.iter().map(|a| a.name.as_str()).collect::<Vec<_>>();
    assert_eq!(names, ["New Window", "New Private Window"], "listed, named and runnable only");
    assert_eq!(app.actions[0].exec, "browser --new-window");
    assert_eq!(app.actions[1].icon.as_deref(), Some("browser-private"));
    assert_eq!(action_command_line(app, &app.actions[1], "ghostty -e"), "browser --private-window");
    let localized = AppIndex::scan_with_options(&[fixture.0.join("apps")], "Tileception", "nl_NL.UTF-8", &[]);
    assert_eq!(localized.get("browser.desktop").unwrap().actions[0].name, "Nieuw venster");

    let found = |query: &str| {
        index
            .search_actions(query, 10)
            .iter()
            .map(|(app, action)| format!("{}: {}", app.name, action.name))
            .collect::<Vec<_>>()
    };
    assert_eq!(found("private"), ["Browser: New Private Window"]);
    assert_eq!(found("new"), ["Browser: New Window", "Browser: New Private Window"], "hidden apps stay out");
    assert_eq!(found("browser priv"), ["Browser: New Private Window"]);
    assert!(found("browser").is_empty(), "the app's name alone is the app, not its actions");
    assert!(found("").is_empty());
}

#[test]
fn field_codes_and_shell_safe_quoting() {
    let index = scan("C", "Tileception");
    let app = index.get("editor.desktop").unwrap();
    assert_eq!(app.exec, "'/opt/Cafe Editor/bin/editor' 'hello world' % '\"quoted\"' '$HOME' '' 'a'\\''b'");
    assert_eq!(command_line(app, "ghostty -e"), format!("ghostty -e {}", app.exec));
    assert_eq!(command_line(app, ""), app.exec);
    let mut app = app.clone();
    app.terminal = false;
    assert_eq!(command_line(&app, "ghostty -e"), app.exec);
}

#[test]
fn extra_arguments_go_where_files_would() {
    let fixture = Fixture::new();
    let entries = [
        ("native", "spotify %U"),
        ("snap", "env BAMF_DESKTOP_FILE_HINT=/var/lib/snapd/desktop/applications/spotify_spotify.desktop /snap/bin/spotify %U"),
        ("flatpak", "/usr/bin/flatpak run --branch=stable --arch=x86_64 --command=spotify --file-forwarding com.spotify.Client @@u %U @@"),
        ("plain", "spotify --minimized"),
        ("chosen", "spotify --ozone-platform=x11 %U"),
        ("bare", "spotify --ozone-platform x11"),
        ("inline", "viewer --file=%f --quiet"),
    ];
    for (id, exec) in entries {
        fixture.write(
            &format!("apps/{id}.desktop"),
            &format!("[Desktop Entry]\nType=Application\nName={id}\nExec={exec}\n"),
        );
    }
    let index = AppIndex::scan_with_options(&[fixture.0.join("apps")], "Tileception", "C", &[]);
    let extra = ["--ozone-platform=wayland".to_owned(), "two words".to_owned()];
    let line = |id: &str| command_line_with(index.get(&format!("{id}.desktop")).unwrap(), "", &extra);
    assert_eq!(line("native"), "spotify --ozone-platform=wayland 'two words'");
    assert_eq!(
        line("snap"),
        "env BAMF_DESKTOP_FILE_HINT=/var/lib/snapd/desktop/applications/spotify_spotify.desktop /snap/bin/spotify --ozone-platform=wayland 'two words'"
    );
    assert_eq!(
        line("flatpak"),
        "/usr/bin/flatpak run --branch=stable --arch=x86_64 --command=spotify --file-forwarding com.spotify.Client --ozone-platform=wayland 'two words' @@u @@"
    );
    assert_eq!(line("plain"), "spotify --minimized --ozone-platform=wayland 'two words'");
    assert_eq!(line("chosen"), "spotify --ozone-platform=x11 'two words'");
    assert_eq!(line("bare"), "spotify --ozone-platform x11 'two words'");
    assert_eq!(line("inline"), "viewer --ozone-platform=wayland 'two words' --file= --quiet");
    let app = index.get("native.desktop").unwrap();
    assert_eq!(command_line_with(app, "", &[]), command_line(app, ""));
    assert_eq!(command_line_with(app, "", &["$(boom)".to_owned()]), "spotify '$(boom)'");
}

#[test]
fn executable_checks_and_nested_ids() {
    let fixture = Fixture::new();
    let executable = fixture.write("bin/fixture-tool", "fixture, never executed\n");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o755)).unwrap();
    }
    let index = AppIndex::scan_with_options(&app_dirs(), "Tileception", "C", &[fixture.0.join("bin")]);
    assert_eq!(index.get("nested-tool.desktop").unwrap().name, "Nested Tool");
    fixture.write(
        "apps/absolute.desktop",
        &format!(
            "[Desktop Entry]\nType=Application\nName=Absolute\nExec=fixture-tool\nTryExec={}\n",
            executable.display()
        ),
    );
    let index = AppIndex::scan_with_options(&[fixture.0.join("apps")], "Tileception", "C", &[]);
    assert!(index.get("absolute.desktop").is_some());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o644)).unwrap();
        let index = AppIndex::scan_with_options(&[fixture.0.join("apps")], "Tileception", "C", &[]);
        assert!(index.get("absolute.desktop").is_none());
    }
}

#[test]
fn ranked_search_usage_accents_and_limits() {
    let fixture = Fixture::new();
    let names = ["Cafe", "Cafe Plus", "The Cafe", "Decafeinated", "C-a-f-e", "Unrelated"];
    for (id, name) in names.iter().enumerate() {
        fixture.write(&format!("apps/{id}.desktop"), &format!("[Desktop Entry]\nType=Application\nName={name}\nExec=/usr/bin/runner\nGenericName=Coffee\nKeywords=caf\u{e9};\n"));
    }
    let index = AppIndex::scan_with_options(&[fixture.0.join("apps")], "Tileception", "C", &[]);
    let usage = HashMap::from([("5.desktop".to_owned(), Usage { count: 100, last_used: SystemTime::UNIX_EPOCH })]);
    let names = index.search("CAF\u{c9}", &usage, 100).iter().map(|app| app.name.as_str()).collect::<Vec<_>>();
    assert_eq!(names, ["Cafe", "Cafe Plus", "The Cafe", "Decafeinated", "C-a-f-e", "Unrelated"]);
    assert_eq!(index.search("", &usage, 1)[0].name, "Unrelated");
    assert_eq!(index.search("runner", &usage, 1)[0].name, "Unrelated");
    assert_eq!(index.search("coffee", &usage, 1)[0].name, "Unrelated");
    assert!(index.search("no-such-app", &usage, 20).is_empty());
    assert!(index.search("", &usage, 0).is_empty());
    let usage = HashMap::from([
        ("0.desktop".to_owned(), Usage { count: 10, last_used: SystemTime::UNIX_EPOCH }),
        ("1.desktop".to_owned(), Usage { count: 10, last_used: SystemTime::UNIX_EPOCH + Duration::from_secs(1) }),
    ]);
    assert_eq!(
        index.search("", &usage, 2).iter().map(|app| app.id.as_str()).collect::<Vec<_>>(),
        ["1.desktop", "0.desktop"]
    );
    assert_eq!(index.search("", &HashMap::new(), 1)[0].name, "C-a-f-e");
}

#[test]
fn refresh_detects_nested_add_remove_and_content_changes() {
    let fixture = Fixture::new();
    let dirs = [fixture.0.join("apps")];
    let mut index = AppIndex::scan_with_options(&dirs, "Tileception", "C", &[]);
    assert!(!index.changed_since_scan());
    let path = fixture.write("apps/nested/new.desktop", "[Desktop Entry]\nType=Application\nName=New\nExec=new\n");
    assert!(index.changed_since_scan());
    index.refresh();
    assert!(!index.changed_since_scan());
    assert!(index.get("nested-new.desktop").is_some());
    fs::write(&path, "[Desktop Entry]\nType=Application\nName=Changed Name\nExec=new\n").unwrap();
    assert!(index.changed_since_scan());
    index.refresh();
    assert_eq!(index.get("nested-new.desktop").unwrap().name, "Changed Name");
    fs::remove_file(path).unwrap();
    assert!(index.changed_since_scan());
    index.refresh();
    assert!(index.get("nested-new.desktop").is_none());
}

#[test]
fn icon_inheritance_size_scale_fallback_and_cycles() {
    let root = fixture_root().join("icons");
    let icons = IconLookup::new("current", std::slice::from_ref(&root));
    for (icon, size, scale, expected) in [
        ("editor", 16, 1, "current/16/apps/editor.svg"),
        ("editor", 40, 1, "current/48/apps/editor.svg"),
        ("editor", 32, 1, "current/16/apps/editor.svg"),
        ("editor", 24, 2, "current/24@2/apps/editor.svg"),
        ("vector", 80, 1, "current/scalable/apps/vector.svg"),
        ("inherited", 32, 1, "parent/32/apps/inherited.svg"),
        ("fallback", 32, 1, "hicolor/32/apps/fallback.svg"),
    ] {
        assert_eq!(icons.find(icon, size, scale), Some(root.join(expected)));
    }
    assert_eq!(icons.find("unthemed", 32, 1), Some(fixture_root().join("pixmaps/unthemed.xpm")));
    let absolute = root.join("current/16/apps/editor.svg");
    assert_eq!(icons.find(absolute.to_str().unwrap(), 32, 1), Some(absolute));
    assert!(icons.find("missing-tileception-fixture-icon", 32, 1).is_none());
    assert_eq!(
        IconLookup::new("missing-theme", std::slice::from_ref(&root)).find("fallback", 32, 1),
        Some(root.join("hicolor/32/apps/fallback.svg"))
    );
}

#[test]
fn icon_indexes_and_misses_are_cached() {
    let fixture = Fixture::new();
    fixture.write("icons/theme/index.theme", "[Icon Theme]\nDirectories=32/apps\n[32/apps]\nSize=32\nType=Fixed\n");
    let path = fixture.write("icons/theme/32/apps/one.svg", "<svg/>\n");
    let icons = IconLookup::new("theme", &[fixture.0.join("icons")]);
    fs::remove_file(fixture.0.join("icons/theme/index.theme")).unwrap();
    assert_eq!(icons.find("one", 32, 1), Some(path.clone()));
    assert!(icons.find("two", 32, 1).is_none());
    fixture.write("icons/theme/32/apps/two.svg", "<svg/>\n");
    assert!(icons.find("two", 32, 1).is_none());
    fs::remove_file(&path).unwrap();
    assert_eq!(icons.find("one", 32, 1), Some(path));
}

#[test]
fn malformed_exec_and_non_application_groups_are_ignored() {
    let fixture = Fixture::new();
    for (id, entry) in [
        ("unknown", "[Desktop Entry]\nType=Application\nName=Invalid\nExec=program %z\n"),
        ("unclosed", "[Desktop Entry]\nType=Application\nName=Invalid\nExec=program \"unclosed\n"),
        ("link", "[Desktop Entry]\nType=Link\nName=Link\nExec=program\n"),
        ("wrong-group", "[Desktop Action test]\nType=Application\nName=Wrong\nExec=program\n"),
    ] {
        fixture.write(&format!("apps/{id}.desktop"), entry);
    }
    let index = AppIndex::scan_with_options(&[fixture.0.join("apps")], "Tileception", "C", &[]);
    assert!(index.search("", &HashMap::new(), 100).is_empty());
}
