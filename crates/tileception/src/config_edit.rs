//! `tileception config`: reads and changes single settings in config.toml,
//! keeping its comments, order and everything else. The setup script
//! (`tileception-setup`) writes the answers it collects through this.

use std::path::{Path, PathBuf};

use toml_edit::{DocumentMut, Item, Value};

/// What `tileception config --help` prints.
pub const HELP: &str = "\
Usage: tileception config [--file <path>] <command>

Reads or changes one setting in config.toml, keeping comments and the rest of
the file. A missing file is written with the defaults first. A change that
would make the config invalid is refused.

Commands:
  get <section.key>           print the value in effect: yours, else the default
  set <section.key> <value>   set a value, checked against the config schema;
                              `set idle.lock_after 300` writes a number
  add <section.key> <value>   add a string to a list, such as general.autostart,
                              unless it is there already
  path                        print the config file's path

Options:
  --file <path>   edit this file instead of $XDG_CONFIG_HOME/tileception/config.toml

Examples:
  tileception config get shell.bar
  tileception config set shell.bar waybar
  tileception config add general.autostart swaync
";

pub fn run(args: &[String]) -> Result<(), String> {
    let (file, args) = match args {
        [flag, path, rest @ ..] if flag == "--file" => (PathBuf::from(path), rest),
        _ => (crate::config::path(), args),
    };
    match args {
        [] => {
            print!("{HELP}");
            Ok(())
        }
        [help] if matches!(help.as_str(), "-h" | "--help" | "help") => {
            print!("{HELP}");
            Ok(())
        }
        [cmd] if cmd == "path" => {
            println!("{}", file.display());
            Ok(())
        }
        [cmd, key] if cmd == "get" => {
            println!("{}", get(&read(&file)?, key)?);
            Ok(())
        }
        [cmd, key, value] if cmd == "set" || cmd == "add" => {
            let mut doc = read(&file)?;
            let changed = if cmd == "set" { set(&mut doc, key, value)? } else { add(&mut doc, key, value)? };
            if changed || !file.exists() {
                write(&file, &doc)?;
            }
            Ok(())
        }
        _ => Err(format!("unknown arguments {args:?}; see tileception config --help")),
    }
}

/// The file as written, or the defaults when there is none yet.
fn read(path: &Path) -> Result<DocumentMut, String> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => crate::config::DEFAULT.to_string(),
        Err(e) => return Err(format!("{}: {e}", path.display())),
    };
    text.parse().map_err(|e| format!("{}: {e}", path.display()))
}

/// Writes through a temporary file, so a crash never leaves half a config;
/// the running compositor reloads it once it changes.
fn write(path: &Path, doc: &DocumentMut) -> Result<(), String> {
    let text = doc.to_string();
    crate::config::Config::parse(&text).map_err(|e| format!("not changed, the result would be invalid: {e}"))?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    let tmp = path.with_extension("toml.new");
    std::fs::write(&tmp, text).map_err(|e| format!("{}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("{}: {e}", path.display()))
}

fn split(key: &str) -> Result<(&str, &str), String> {
    match key.split_once('.') {
        Some((section, name)) if !section.is_empty() && !name.is_empty() && !name.contains('.') => Ok((section, name)),
        _ => Err(format!("{key:?} is not section.key, such as shell.bar")),
    }
}

fn lookup<'a>(doc: &'a DocumentMut, section: &str, name: &str) -> Option<&'a Value> {
    doc.get(section)?.get(name)?.as_value()
}

fn defaults() -> DocumentMut {
    crate::config::DEFAULT.parse().expect("built-in config parses")
}

/// Strings print bare, so scripts can compare them; anything else prints as TOML.
fn show(value: &Value) -> String {
    match value.as_str() {
        Some(text) => text.to_string(),
        None => value.clone().decorated("", "").to_string(),
    }
}

fn get(doc: &DocumentMut, key: &str) -> Result<String, String> {
    let (section, name) = split(key)?;
    if let Some(value) = lookup(doc, section, name) {
        return Ok(show(value));
    }
    // The template leaves some settings commented out. Use the same defaults
    // as deserialization, rather than maintaining another list of settings.
    let values = toml::Table::try_from(crate::config::ConfigFile::default())
        .map_err(|e| format!("cannot read config defaults: {e}"))?;
    values
        .get(section)
        .and_then(|table| table.get(name))
        .map(|value| match value {
            toml::Value::String(text) => text.clone(),
            toml::Value::Float(number) => number.to_string(),
            _ => value.to_string(),
        })
        // Built-in bindings are loaded separately from the template.
        .or_else(|| lookup(&defaults(), section, name).map(show))
        .ok_or_else(|| format!("{key} is not set and has no default"))
}

/// The text as a value of the same type as the setting's current or default
/// value. Numbers accept integers or floats; validation of the whole config
/// decides which the setting supports. When neither says (a default that is
/// only a comment), true, false, numbers and lists are taken as such and
/// anything else as a string; validation catches a wrong guess.
fn typed(like: Option<&Value>, text: &str, key: &str) -> Result<Value, String> {
    let parse = |what: &str| -> Result<Value, String> {
        let value: Value = text.parse().map_err(|_| format!("{key} takes {what}, not {text:?}"))?;
        Ok(value)
    };
    match like {
        Some(Value::Boolean(_)) => match parse("true or false")? {
            v @ Value::Boolean(_) => Ok(v),
            _ => Err(format!("{key} takes true or false, not {text:?}")),
        },
        Some(Value::Integer(_) | Value::Float(_)) => match parse("a number")? {
            v @ (Value::Integer(_) | Value::Float(_)) => Ok(v),
            _ => Err(format!("{key} takes a number, not {text:?}")),
        },
        Some(Value::Array(_)) => match parse("a list, such as [\"a\", \"b\"]")? {
            v @ Value::Array(_) => Ok(v),
            _ => Err(format!("{key} takes a list, such as [\"a\", \"b\"], not {text:?}")),
        },
        Some(_) => Ok(Value::from(text)),
        None => match text.parse::<Value>() {
            Ok(v @ (Value::Boolean(_) | Value::Integer(_) | Value::Float(_) | Value::Array(_))) => Ok(v),
            _ => Ok(Value::from(text)),
        },
    }
}

/// The section's table, made when the file has none.
fn table<'a>(doc: &'a mut DocumentMut, section: &str) -> Result<&'a mut toml_edit::Table, String> {
    doc.entry(section)
        .or_insert_with(|| Item::Table(toml_edit::Table::new()))
        .as_table_mut()
        .ok_or_else(|| format!("[{section}] is not a table"))
}

/// Sets one value, keeping the comments around it. False when it already had it.
fn set(doc: &mut DocumentMut, key: &str, text: &str) -> Result<bool, String> {
    let (section, name) = split(key)?;
    let defaults = defaults();
    let current = lookup(doc, section, name).cloned();
    let mut value = typed(current.as_ref().or_else(|| lookup(&defaults, section, name)), text, key)?;
    if let Some(old) = &current {
        if show(old) == show(&value) {
            return Ok(false);
        }
        *value.decor_mut() = old.decor().clone();
    }
    table(doc, section)?[name] = Item::Value(value);
    Ok(true)
}

/// Adds a string to a list setting. False when it was there already.
fn add(doc: &mut DocumentMut, key: &str, text: &str) -> Result<bool, String> {
    let (section, name) = split(key)?;
    let defaults = defaults();
    let mut list = match lookup(doc, section, name).or_else(|| lookup(&defaults, section, name)) {
        Some(Value::Array(list)) => list.clone(),
        None => toml_edit::Array::new(),
        Some(_) => return Err(format!("{key} is not a list")),
    };
    if list.iter().any(|v| v.as_str() == Some(text)) {
        return Ok(false);
    }
    list.push(text);
    let decor = lookup(doc, section, name).map(|v| v.decor().clone());
    let mut value = Value::Array(list);
    if let Some(decor) = decor {
        *value.decor_mut() = decor;
    }
    table(doc, section)?[name] = Item::Value(value);
    Ok(true)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test setup and assertions should fail immediately on unexpected errors")]
mod tests {
    use super::*;

    fn doc(text: &str) -> DocumentMut {
        text.parse().unwrap()
    }

    #[test]
    fn get_falls_back_to_the_default() {
        let file = doc("[shell]\nbar = \"waybar\"\n");
        assert_eq!(get(&file, "shell.bar").unwrap(), "waybar");
        assert_eq!(get(&file, "shell.launcher").unwrap(), "builtin");
        assert_eq!(get(&file, "general.gap").unwrap(), "12");
        assert!(get(&file, "shell.nothing").is_err());
        assert!(get(&file, "shell").is_err());
    }

    #[test]
    fn set_keeps_comments_and_follows_the_type() {
        let mut file = doc("[shell]\n# The bar.\nbar = \"builtin\" # here\n\n[idle]\nlock_after = 0\n");
        assert!(set(&mut file, "shell.bar", "waybar").unwrap());
        assert!(set(&mut file, "idle.lock_after", "300").unwrap());
        assert!(!set(&mut file, "idle.lock_after", "300").unwrap());
        assert_eq!(file.to_string(), "[shell]\n# The bar.\nbar = \"waybar\" # here\n\n[idle]\nlock_after = 300\n");
        assert!(set(&mut file, "idle.lock_after", "soon").is_err());
        assert!(set(&mut file, "general.gap", "wide").is_err());
        set(&mut file, "general.welcome", "maybe").unwrap();
        assert!(crate::config::Config::parse(&file.to_string()).is_err());
    }

    #[test]
    fn set_adds_missing_keys_and_sections() {
        let mut file = doc("[shell]\nbar = \"none\"\n");
        set(&mut file, "shell.doctor", "none").unwrap();
        set(&mut file, "general.welcome", "false").unwrap();
        assert_eq!(get(&file, "shell.doctor").unwrap(), "none");
        assert_eq!(get(&file, "general.welcome").unwrap(), "false");
        crate::config::Config::parse(&file.to_string()).unwrap();
    }

    #[test]
    fn add_appends_once() {
        let mut file = doc("[general]\nautostart = []\n");
        assert!(add(&mut file, "general.autostart", "swaync").unwrap());
        assert!(!add(&mut file, "general.autostart", "swaync").unwrap());
        assert_eq!(get(&file, "general.autostart").unwrap(), "[\"swaync\"]");
        assert!(add(&mut file, "shell.bar", "x").is_err());
    }

    #[test]
    fn the_defaults_edit_cleanly() {
        let mut file = defaults();
        set(&mut file, "shell.bar", "waybar").unwrap();
        let text = file.to_string();
        assert_eq!(text.lines().count(), crate::config::DEFAULT.lines().count());
        crate::config::Config::parse(&text).unwrap();
    }
}
