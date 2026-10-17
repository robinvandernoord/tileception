use crate::{App, AppAction};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

pub(crate) fn groups(text: &str) -> HashMap<String, HashMap<String, String>> {
    let mut result = HashMap::new();
    let mut group = String::new();
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('#') || line.is_empty() {
            continue;
        }
        if line.starts_with('[') && line.ends_with(']') {
            group = line[1..line.len() - 1].to_owned();
        } else if let Some((key, value)) = line.split_once('=') {
            result
                .entry(group.clone())
                .or_insert_with(HashMap::new)
                .insert(key.trim().to_owned(), value.trim().to_owned());
        }
    }
    result
}

fn unescape(value: &str) -> String {
    let mut output = String::new();
    let mut chars = value.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            output.push(c);
            continue;
        }
        match chars.next() {
            Some('s') => output.push(' '),
            Some('n') => output.push('\n'),
            Some('t') => output.push('\t'),
            Some('r') => output.push('\r'),
            Some('\\') => output.push('\\'),
            Some(c) => {
                output.push('\\');
                output.push(c);
            }
            None => output.push('\\'),
        }
    }
    output
}

fn list(value: &str) -> Vec<String> {
    let mut values = Vec::new();
    let mut current = String::new();
    let mut chars = value.chars();
    while let Some(c) = chars.next() {
        match c {
            ';' => {
                if !current.is_empty() {
                    values.push(unescape(&current));
                }
                current.clear();
            }
            '\\' => match chars.next() {
                Some(';') => current.push(';'),
                Some(c) => {
                    current.push('\\');
                    current.push(c);
                }
                None => current.push('\\'),
            },
            c => current.push(c),
        }
    }
    if !current.is_empty() {
        values.push(unescape(&current));
    }
    values
}

fn locale_keys(locale: &str) -> Vec<String> {
    let (base, modifier) = locale.split_once('@').map_or((locale, None), |(base, modifier)| (base, Some(modifier)));
    let base = base.split('.').next().unwrap_or(base);
    if base == "C" || base == "POSIX" {
        return Vec::new();
    }
    let language = base.split('_').next().unwrap_or(base);
    let mut keys = Vec::new();
    if let Some(modifier) = modifier {
        keys.push(format!("{base}@{modifier}"));
    }
    keys.push(base.to_owned());
    if language != base {
        if let Some(modifier) = modifier {
            keys.push(format!("{language}@{modifier}"));
        }
        keys.push(language.to_owned());
    }
    keys
}

pub(crate) fn parse(text: &str, id: &str, desktop: &str, locale: &str, path: &[PathBuf]) -> Option<App> {
    let groups = groups(text);
    let values = groups.get("Desktop Entry")?;
    let boolean = |key: &str| values.get(key).is_some_and(|value| value == "true");
    let hidden = boolean("Hidden");
    if values.get("Type").map(String::as_str) != Some("Application") && !hidden {
        return None;
    }
    if let Some(executable) = values.get("TryExec") {
        let executable = unescape(executable);
        let executable = Path::new(&executable);
        let found = if executable.is_absolute() {
            executable_file(executable)
        } else if executable.components().count() == 1 {
            path.iter().any(|dir| executable_file(&dir.join(executable)))
        } else {
            false
        };
        if !found {
            return None;
        }
    }
    let locales = locale_keys(locale);
    let localized = |key: &str| {
        locales.iter().find_map(|locale| values.get(&format!("{key}[{locale}]"))).or_else(|| values.get(key)).cloned()
    };
    let string = |key: &str| values.get(key).map(|value| unescape(value));
    let desktops = desktop.split(':').collect::<Vec<_>>();
    let includes =
        values.get("OnlyShowIn").is_none_or(|value| list(value).iter().any(|name| desktops.contains(&name.as_str())));
    let excludes =
        values.get("NotShowIn").is_some_and(|value| list(value).iter().any(|name| desktops.contains(&name.as_str())));
    let name = localized("Name").map(|value| unescape(&value));
    let exec = values.get("Exec").and_then(|value| exec_line(&unescape(value)));
    if !hidden && (name.as_ref().is_none_or(String::is_empty) || exec.is_none()) {
        return None;
    }
    Some(App {
        id: id.to_owned(),
        name: name.unwrap_or_else(|| id.to_owned()),
        generic_name: localized("GenericName").map(|value| unescape(&value)),
        comment: localized("Comment").map(|value| unescape(&value)),
        keywords: localized("Keywords").map(|value| list(&value)).unwrap_or_default(),
        icon: string("Icon"),
        exec: exec.unwrap_or_default(),
        terminal: boolean("Terminal"),
        categories: values.get("Categories").map(|value| list(value)).unwrap_or_default(),
        wm_class: string("StartupWMClass"),
        hidden: hidden || boolean("NoDisplay") || !includes || excludes,
        actions: values.get("Actions").map(|value| actions(&groups, value, &locales)).unwrap_or_default(),
    })
}

/// The `[Desktop Action id]` groups named in `Actions=`; those without a
/// name or a usable Exec line are left out.
fn actions(groups: &HashMap<String, HashMap<String, String>>, ids: &str, locales: &[String]) -> Vec<AppAction> {
    list(ids)
        .into_iter()
        .filter_map(|id| {
            let values = groups.get(&format!("Desktop Action {id}"))?;
            let name = locales
                .iter()
                .find_map(|locale| values.get(&format!("Name[{locale}]")))
                .or_else(|| values.get("Name"))
                .map(|value| unescape(value))
                .filter(|name| !name.is_empty())?;
            let exec = exec_line(&unescape(values.get("Exec")?))?;
            Some(AppAction { id, name, icon: values.get("Icon").map(|value| unescape(value)), exec })
        })
        .collect()
}

fn executable_file(path: &Path) -> bool {
    let Ok(metadata) = fs::metadata(path) else { return false };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

fn exec_line(value: &str) -> Option<String> {
    let args = arguments(value)?;
    let mut cleaned = Vec::new();
    for arg in args {
        let mut output = String::new();
        let mut chars = arg.chars();
        let mut removed = false;
        while let Some(c) = chars.next() {
            if c != '%' {
                output.push(c);
                continue;
            }
            match chars.next()? {
                '%' => output.push('%'),
                'f' | 'F' | 'u' | 'U' | 'i' | 'c' | 'k' | 'd' | 'D' | 'n' | 'N' | 'v' | 'm' => removed = true,
                _ => return None,
            }
        }
        if !output.is_empty() || !removed {
            cleaned.push(shell_quote(&output));
        }
    }
    if cleaned.is_empty() {
        None
    } else {
        Some(cleaned.join(" "))
    }
}

fn arguments(value: &str) -> Option<Vec<String>> {
    let mut result = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    let mut started = false;
    let mut chars = value.chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => {
                quoted = !quoted;
                started = true;
            }
            '\\' => {
                let next = chars.next()?;
                if quoted && !matches!(next, '"' | '`' | '$' | '\\') {
                    current.push('\\');
                }
                current.push(next);
                started = true;
            }
            c if c.is_whitespace() && !quoted => {
                if started {
                    result.push(std::mem::take(&mut current));
                    started = false;
                }
            }
            c => {
                current.push(c);
                started = true;
            }
        }
    }
    if quoted {
        return None;
    }
    if started {
        result.push(current);
    }
    Some(result)
}

fn shell_quote(value: &str) -> String {
    if !value.is_empty() && value.chars().all(|c| c.is_ascii_alphanumeric() || "_@%+=:,./-".contains(c)) {
        value.to_owned()
    } else {
        format!("'{}'", value.replace('\'', "'\\''"))
    }
}

pub(crate) fn program(exec: &str) -> String {
    // The shell representation is generated by us, so single quotes can be decoded here.
    let mut output = String::new();
    let mut quoted = false;
    let mut chars = exec.chars();
    while let Some(c) = chars.next() {
        match c {
            '\'' => quoted = !quoted,
            '\\' if !quoted => {
                if let Some(c) = chars.next() {
                    output.push(c);
                }
            }
            c if c.is_whitespace() && !quoted => break,
            c => output.push(c),
        }
    }
    Path::new(&output).file_name().map(|value| value.to_string_lossy().into_owned()).unwrap_or(output)
}
