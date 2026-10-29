//! The desktops to choose from, from the session entries desktops install.

use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Wayland,
    X11,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Session {
    /// The entry's file name without `.desktop`, such as `tileception`.
    pub session_id: String,
    pub name: String,
    /// The command line, field codes removed.
    pub exec: String,
    pub kind: Kind,
    /// `DesktopNames`, for `XDG_CURRENT_DESKTOP`.
    pub desktop_names: Option<String>,
}

impl Session {
    /// What greetd should run, and the environment it needs.
    pub fn command(&self, xsession_wrapper: &str) -> (Vec<String>, Vec<String>) {
        let (line, kind) = match self.kind {
            Kind::Wayland => (self.exec.clone(), "wayland"),
            Kind::X11 => (format!("{} {}", xsession_wrapper.trim(), self.exec), "x11"),
        };
        let mut env = vec![format!("XDG_SESSION_TYPE={kind}"), format!("XDG_SESSION_DESKTOP={}", self.session_id)];
        if let Some(names) = &self.desktop_names {
            env.push(format!("XDG_CURRENT_DESKTOP={}", names.trim_end_matches(';').replace(';', ":")));
        }
        (vec![line], env)
    }
}

/// The keys of a `[Desktop Entry]` group that matter here.
fn parse_entry(text: &str) -> Option<(String, String, Option<String>, Option<String>)> {
    let (mut name, mut exec, mut try_exec, mut names) = (None, None, None, None);
    let mut in_entry = false;
    for line in text.lines().map(str::trim) {
        if line.starts_with('[') {
            in_entry = line == "[Desktop Entry]";
            continue;
        }
        let Some((key, value)) = line.split_once('=') else { continue };
        if !in_entry {
            continue;
        }
        let value = value.trim().to_string();
        match key.trim() {
            "Name" => name = Some(value),
            "Exec" => exec = Some(value),
            "TryExec" => try_exec = Some(value),
            "DesktopNames" => names = Some(value),
            "Hidden" | "NoDisplay" if value == "true" => return None,
            _ => {}
        }
    }
    Some((name?, strip_field_codes(&exec?), try_exec, names))
}

/// Removes `%f`, `%U` and the like; `%%` stays a `%`.
fn strip_field_codes(exec: &str) -> String {
    let mut out = String::with_capacity(exec.len());
    let mut chars = exec.chars();
    while let Some(character) = chars.next() {
        if character == '%' {
            if let Some(next) = chars.next()
                && next == '%'
            {
                out.push('%');
            }
        } else {
            out.push(character);
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn on_path(program: &str) -> bool {
    if program.contains('/') {
        return Path::new(program).is_file();
    }
    let path = std::env::var_os("PATH").unwrap_or_else(|| "/usr/local/bin:/usr/bin:/bin".into());
    std::env::split_paths(&path).any(|dir| dir.join(program).is_file())
}

/// Wayland sessions come first; an X session with the same id as a Wayland
/// one is left out, as is anything in `hidden` (by id or name).
/// [`sort_default_first`] then puts the default on top.
pub fn scan(dirs: &[(PathBuf, Kind)], hidden: &[String]) -> Vec<Session> {
    let mut sessions: Vec<Session> = Vec::new();
    for (dir, kind) in dirs {
        let Ok(entries) = std::fs::read_dir(dir) else { continue };
        let mut found: Vec<Session> = entries
            .flatten()
            .filter_map(|entry| {
                let path = entry.path();
                let session_id = path.file_name()?.to_str()?.strip_suffix(".desktop")?.to_string();
                let (name, exec, try_exec, desktop_names) = parse_entry(&std::fs::read_to_string(&path).ok()?)?;
                if try_exec.is_some_and(|program| !on_path(&program)) || exec.is_empty() {
                    return None;
                }
                Some(Session { session_id, name, exec, kind: *kind, desktop_names })
            })
            .filter(|session| {
                !hidden.iter().any(|hidden_name| *hidden_name == session.session_id || *hidden_name == session.name)
            })
            .collect();
        found.sort_by_key(|session| session.name.to_lowercase());
        for session in found {
            if !sessions.iter().any(|existing| existing.session_id == session.session_id) {
                sessions.push(session);
            }
        }
    }
    sessions
}

/// Moves the session with id `default` to the top, as the one most people want.
pub fn sort_default_first(sessions: &mut [Session], default: &str) {
    if let Some(index) = sessions.iter().position(|session| session.session_id == default) {
        sessions[..=index].rotate_right(1);
    }
}

/// The session directories, or `TILECEPTION_GREETER_SESSIONS` (Wayland
/// session directories, colon separated) for tests.
pub fn list(hidden: &[String], xsession_wrapper: &str) -> Vec<Session> {
    let dirs: Vec<(PathBuf, Kind)> = match std::env::var_os("TILECEPTION_GREETER_SESSIONS") {
        Some(dirs) => std::env::split_paths(&dirs).map(|directory| (directory, Kind::Wayland)).collect(),
        None => {
            let mut dirs = vec![
                ("/usr/local/share/wayland-sessions".into(), Kind::Wayland),
                ("/usr/share/wayland-sessions".into(), Kind::Wayland),
            ];
            // X sessions need the wrapper that starts an X server.
            match xsession_wrapper.split_whitespace().next() {
                Some(program) if on_path(program) => dirs.extend([
                    ("/usr/local/share/xsessions".into(), Kind::X11),
                    ("/usr/share/xsessions".into(), Kind::X11),
                ]),
                _ => tracing::info!("greeter: X sessions left out, {xsession_wrapper:?} is not installed"),
            }
            dirs
        }
    };
    scan(&dirs, hidden)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_entries() {
        let text = "[Desktop Entry]\nName=Sway\nName[nl]=Zwaai\nExec=sway --unsupported-gpu %U\nDesktopNames=sway;wlroots;\n\n[Desktop Action x]\nName=Other\n";
        let (name, exec, _, names) = parse_entry(text).expect("parses");
        assert_eq!(name, "Sway");
        assert_eq!(exec, "sway --unsupported-gpu");
        assert_eq!(names.as_deref(), Some("sway;wlroots;"));
        assert!(parse_entry("[Desktop Entry]\nName=Gone\nExec=x\nHidden=true\n").is_none());
        assert_eq!(strip_field_codes("echo 100%% %f"), "echo 100%");
    }

    #[test]
    fn commands_carry_the_session_type() {
        let session = Session {
            session_id: "sway".into(),
            name: "Sway".into(),
            exec: "sway".into(),
            kind: Kind::Wayland,
            desktop_names: Some("sway;wlroots".into()),
        };
        let (cmd, env) = session.command("startx /usr/bin/env");
        assert_eq!(cmd, ["sway"]);
        assert_eq!(env, ["XDG_SESSION_TYPE=wayland", "XDG_SESSION_DESKTOP=sway", "XDG_CURRENT_DESKTOP=sway:wlroots"]);
        let xsession = Session { kind: Kind::X11, exec: "cinnamon-session".into(), ..session };
        assert_eq!(xsession.command("startx /usr/bin/env").0, ["startx /usr/bin/env cinnamon-session"]);
    }

    #[test]
    fn wayland_wins_and_hidden_ones_go() {
        let root = std::env::temp_dir().join(format!("tileception-sessions-{}", std::process::id()));
        let (wayland, xorg) = (root.join("wayland"), root.join("x"));
        std::fs::create_dir_all(&wayland).expect("dir");
        std::fs::create_dir_all(&xorg).expect("dir");
        let entry = |name: &str| format!("[Desktop Entry]\nName={name}\nExec=true\n");
        std::fs::write(wayland.join("tileception.desktop"), entry("Tileception")).expect("write");
        std::fs::write(wayland.join("tileception-nested.desktop"), entry("Nested")).expect("write");
        std::fs::write(wayland.join("gnome.desktop"), entry("GNOME")).expect("write");
        std::fs::write(xorg.join("gnome.desktop"), entry("GNOME on Xorg")).expect("write");
        std::fs::write(xorg.join("i3.desktop"), entry("i3")).expect("write");
        std::fs::write(xorg.join("missing.desktop"), "[Desktop Entry]\nName=M\nExec=m\nTryExec=/no/such/file\n")
            .expect("write");
        let found = scan(&[(wayland, Kind::Wayland), (xorg, Kind::X11)], &["tileception-nested".into()]);
        let names: Vec<(&str, Kind)> = found.iter().map(|session| (session.name.as_str(), session.kind)).collect();
        assert_eq!(names, [("GNOME", Kind::Wayland), ("Tileception", Kind::Wayland), ("i3", Kind::X11)]);
        let mut sorted = found.clone();
        sort_default_first(&mut sorted, "tileception");
        let ids: Vec<&str> = sorted.iter().map(|session| session.session_id.as_str()).collect();
        assert_eq!(ids, ["tileception", "gnome", "i3"]);
        let _ = std::fs::remove_dir_all(root);
    }
}
