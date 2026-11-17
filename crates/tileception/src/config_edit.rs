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
  passphrase                  print three random words to use as the
                              [handoff] passphrase (nothing is written)

Options:
  --file <path>   edit this file instead of $XDG_CONFIG_HOME/tileception/config.toml

Examples:
  tileception config get shell.bar
  tileception config set shell.bar waybar
  tileception config add general.autostart swaync
  tileception config set handoff.passphrase \"$(tileception config passphrase)\"
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
        [cmd] if cmd == "passphrase" => {
            println!("{}", crate::handoff::suggest_passphrase()?);
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

/// One setting's value in effect, for other commands.
pub fn get_value(file: &Path, key: &str) -> Result<String, String> {
    get(&read(file)?, key)
}

/// Sets one setting, for other commands; the file is checked before writing.
pub fn set_value(file: &Path, key: &str, value: &str) -> Result<(), String> {
    let mut doc = read(file)?;
    if set(&mut doc, key, value)? || !file.exists() {
        write(file, &doc)?;
    }
    Ok(())
}

/// Adds `[plugins.<id>]` with this command, for `tileception msg plugin
/// add`; refuses an ID the file already configures.
pub fn add_plugin(file: &Path, plugin_id: &str, command: &str, args: &[String], bar: bool) -> Result<(), String> {
    let mut doc = read(file)?;
    insert_plugin(&mut doc, plugin_id, command, args, bar)?;
    write(file, &doc)
}

/// Switches a plugin's dashboard items on: `[plugins.<id>.dashboard]
/// enabled = true`, and with `plugin` also `[plugins.<id>] enabled = true`.
pub fn show_plugin_dashboard(file: &Path, plugin_id: &str, plugin: bool) -> Result<(), String> {
    let mut doc = read(file)?;
    switch_on_dashboard(&mut doc, plugin_id, plugin)?;
    write(file, &doc)
}

fn switch_on_dashboard(doc: &mut DocumentMut, plugin_id: &str, plugin: bool) -> Result<(), String> {
    let plugins = doc
        .entry("plugins")
        .or_insert_with(|| {
            let mut plugins = toml_edit::Table::new();
            plugins.set_implicit(true);
            Item::Table(plugins)
        })
        .as_table_mut()
        .ok_or("[plugins] is not a table")?;
    let entry = plugins
        .entry(plugin_id)
        .or_insert_with(|| {
            // Only switches: the manifest says what the plugin is.
            let mut entry = toml_edit::Table::new();
            entry.set_implicit(true);
            entry.set_position(Some(isize::MAX));
            Item::Table(entry)
        })
        .as_table_mut()
        .ok_or_else(|| format!("[plugins.{plugin_id}] is not a table"))?;
    if plugin {
        entry.set_implicit(false);
        entry["enabled"] = toml_edit::value(true);
    }
    let after = entry.position();
    let dashboard = entry
        .entry("dashboard")
        .or_insert_with(|| {
            let mut dashboard = toml_edit::Table::new();
            dashboard.set_position(after);
            Item::Table(dashboard)
        })
        .as_table_mut()
        .ok_or_else(|| format!("[plugins.{plugin_id}.dashboard] is not a table"))?;
    dashboard["enabled"] = toml_edit::value(true);
    Ok(())
}

/// Appends the plugin's table after everything else, leaving the rest of
/// the file as it was.
fn insert_plugin(
    doc: &mut DocumentMut,
    plugin_id: &str,
    command: &str,
    args: &[String],
    bar: bool,
) -> Result<(), String> {
    let taken = [
        ("plugins", doc.get("plugins")),
        ("launcher.plugins", doc.get("launcher").and_then(|launcher| launcher.get("plugins"))),
    ]
    .into_iter()
    .find_map(|(section, plugins)| plugins?.get(plugin_id).map(|_| section));
    if let Some(section) = taken {
        return Err(format!(
            "[{section}.{plugin_id}] is already there; edit it, or add this one under another id with --id"
        ));
    }
    let plugins = doc
        .entry("plugins")
        .or_insert_with(|| {
            let mut plugins = toml_edit::Table::new();
            plugins.set_implicit(true);
            Item::Table(plugins)
        })
        .as_table_mut()
        .ok_or("[plugins] is not a table")?;
    let mut entry = toml_edit::Table::new();
    entry["command"] = toml_edit::value(command);
    if !args.is_empty() {
        entry["args"] = toml_edit::value(args.iter().map(String::as_str).collect::<toml_edit::Array>());
    }
    if bar {
        // An empty `[bar]` block is what shows a plugin's chips.
        entry["bar"] = Item::Table(toml_edit::Table::new());
    }
    // A table without a position is written straight after the last table
    // read, which can be ahead of tables added before; pin it to the end.
    entry.set_position(Some(isize::MAX));
    plugins.insert(plugin_id, Item::Table(entry));
    // Comments after the last key, such as a commented-out setting of the
    // last table, are kept as the file's trailing text, which is written
    // after every table. Left there, they would end up in the new table:
    // keep them where they were, in front of it.
    let trailing = doc.trailing().as_str().unwrap_or_default().to_string();
    if !trailing.trim().is_empty() {
        doc.set_trailing("");
        let prefix = format!("{}\n\n", trailing.trim_end());
        if let Some(entry) = doc["plugins"].get_mut(plugin_id).and_then(Item::as_table_mut) {
            entry.decor_mut().set_prefix(prefix);
        }
    }
    Ok(())
}

/// Apply a panel draft in one validated write. Read the latest file so edits
/// to unrelated settings are preserved; refuse conflicting changes.
pub fn apply_draft(changes: &[(&str, Option<toml::Value>, Option<toml::Value>)]) -> Result<(), String> {
    let path = crate::config::path();
    let mut doc = read(&path)?;
    let current = crate::config::Config::parse(&doc.to_string())?;
    let values = toml::Table::try_from(&current.file).map_err(|error| error.to_string())?;
    for (key, before, after) in changes {
        if !key.contains('.') {
            let actual = values.get(*key).cloned();
            if &actual != before && &actual != after {
                return Err(format!("{key} changed outside Settings. Discard to reload it."));
            }
            replace_root(&mut doc, key, after.as_ref())?;
            continue;
        }
        let (section, name) = split(key)?;
        let actual = values.get(section).and_then(|value| value.get(name)).cloned();
        if &actual != before && &actual != after {
            return Err(format!("{key} changed outside Settings. Discard and reopen to load it."));
        }
        let Some(after) = after else {
            table(&mut doc, section)?.remove(name);
            continue;
        };
        let mut value: Value = after.to_string().parse().map_err(|error| format!("{key}: {error}"))?;
        if let Some(old) = lookup(&doc, section, name) {
            lay_out_like(&mut value, old);
            *value.decor_mut() = old.decor().clone();
        }
        table(&mut doc, section)?[name] = Item::Value(value);
    }
    write(&path, &doc)
}

/// The editable source, with its comments and section syntax intact.
pub fn panel_text() -> Result<String, String> {
    Ok(read(&crate::config::path())?.to_string())
}

/// Extract source items, preserving their comments and explicit values.
pub fn section_text(base: &str, key: &str) -> Result<String, String> {
    let doc: DocumentMut = base.parse().map_err(|error| format!("{error}"))?;
    let mut excerpt = DocumentMut::new();
    if let Some((section, name)) = key.split_once('.') {
        if let Some(item) = doc.get(section).and_then(|item| item.get(name)) {
            table(&mut excerpt, section)?[name] = item.clone();
        } else {
            let defaults =
                toml::Table::try_from(crate::config::ConfigFile::default()).map_err(|error| error.to_string())?;
            if let Some(value) = defaults.get(section).and_then(|section| section.get(name)) {
                let example = toml::Table::from_iter([(
                    section.into(),
                    toml::Value::Table(toml::Table::from_iter([(name.into(), value.clone())])),
                )]);
                return toml::to_string_pretty(&example).map_err(|error| error.to_string());
            }
        }
    } else if let Some(item) = doc.get(key) {
        excerpt[key] = item.clone();
    } else if key == "rules" {
        return Ok("# Add [[rules]] entries here.\n".into());
    } else {
        let section = table(&mut excerpt, key)?;
        if key == "binds" {
            section.decor_mut().set_prefix("# Custom overrides only. Use \"none\" to disable a built-in binding.\n");
        }
    }
    Ok(excerpt.to_string())
}

/// Splice the edited source item back, without rebuilding it from defaults.
pub fn splice_section(base: &str, key: &str, text: &str) -> Result<String, String> {
    let mut doc: DocumentMut = base.parse().map_err(|error| format!("{error}"))?;
    let mut edited: DocumentMut = text.parse().map_err(|error| format!("{error}"))?;
    let trailing = edited.trailing().as_str().unwrap_or("").to_string();
    if let Some((section, name)) = key.split_once('.') {
        let item = edited.get_mut(section).and_then(Item::as_table_mut).and_then(|table| table.remove(name));
        if edited.get(section).and_then(Item::as_table).is_some_and(toml_edit::Table::is_empty) {
            edited.remove(section);
        }
        if !edited.is_empty() {
            return Err(format!("Edit only {key} here; use the full-file editor for other settings."));
        }
        if let Some(mut item) = item {
            keep_place(&mut item, doc.get(section).and_then(|item| item.get(name)));
            table(&mut doc, section)?[name] = item;
        } else {
            table(&mut doc, section)?.remove(name);
        }
    } else {
        let item = edited.remove(key);
        if !edited.is_empty() {
            return Err(format!("Edit only {key} here; use the full-file editor for other settings."));
        }
        if let Some(mut item) = item {
            keep_place(&mut item, doc.get(key));
            doc[key] = item;
        } else {
            doc.remove(key);
        }
    }
    if !trailing.trim().is_empty() {
        let previous = doc.trailing().as_str().unwrap_or("");
        doc.set_trailing(format!("{previous}\n{trailing}"));
    }
    crate::config::Config::parse(&doc.to_string())?;
    Ok(doc.to_string())
}

/// Gives the tables of an edited item the place in the file their
/// counterparts in `old` have. The edited item was parsed from an excerpt, so
/// its tables carry positions in that excerpt; printed as they are, the
/// section would move to the top of the file. A table without a counterpart
/// follows the one before it.
fn keep_place(item: &mut Item, old: Option<&Item>) {
    fn table(edited: &mut toml_edit::Table, old: Option<&toml_edit::Table>, fallback: Option<isize>) {
        edited.set_position(old.and_then(toml_edit::Table::position).or(fallback));
        for (key, child) in edited.iter_mut() {
            keep_place(child, old.and_then(|old| old.get(key.get())));
        }
    }
    match item {
        Item::Table(edited) => table(edited, old.and_then(Item::as_table), None),
        Item::ArrayOfTables(edited) => {
            let old = old.and_then(Item::as_array_of_tables);
            // Entries beyond the old ones stay with the last of them.
            let last = old.and_then(|old| old.iter().last()).and_then(toml_edit::Table::position);
            for (index, entry) in edited.iter_mut().enumerate() {
                table(entry, old.and_then(|old| old.get(index)), last);
            }
        }
        _ => {}
    }
}

/// A list written one entry per line stays that way when Settings changes it.
fn lay_out_like(value: &mut Value, old: &Value) {
    let (Value::Array(array), Value::Array(previous)) = (value, old) else { return };
    if !previous.to_string().contains('\n') {
        return;
    }
    for entry in array.iter_mut() {
        entry.decor_mut().set_prefix("\n    ");
        entry.decor_mut().set_suffix("");
    }
    array.set_trailing_comma(true);
    array.set_trailing("\n");
}

fn replace_root(doc: &mut DocumentMut, key: &str, value: Option<&toml::Value>) -> Result<(), String> {
    if let Some(value) = value {
        let table = toml::Table::from_iter([(key.to_string(), value.clone())]);
        let mut replacement: DocumentMut =
            toml::to_string(&table).map_err(|error| error.to_string())?.parse().map_err(|error| format!("{error}"))?;
        doc[key] = replacement.remove(key).ok_or("missing serialized setting")?;
    } else {
        doc.remove(key);
    }
    Ok(())
}

/// Overlay controls on an editor draft, without writing to disk.
pub fn draft_text(base: &str, changes: &[(&str, Option<toml::Value>)]) -> Result<String, String> {
    let mut doc: DocumentMut = base.parse().map_err(|error| format!("{error}"))?;
    for (key, value) in changes {
        if let Some((section, name)) = key.split_once('.') {
            if let Some(value) = value {
                let mut replacement: DocumentMut =
                    toml::to_string(&toml::Table::from_iter([(name.to_string(), value.clone())]))
                        .map_err(|error| error.to_string())?
                        .parse()
                        .map_err(|error| format!("{error}"))?;
                let mut item = replacement.remove(name).ok_or("missing serialized setting")?;
                if item.is_array_of_tables() {
                    // Inline, as the config file writes lists of small tables.
                    item = Item::Value(item.into_value().map_err(|_| "a list of tables did not convert")?);
                }
                if let (Some(old), Some(new)) = (lookup(&doc, section, name), item.as_value_mut()) {
                    lay_out_like(new, old);
                    *new.decor_mut() = old.decor().clone();
                }
                table(&mut doc, section)?[name] = item;
            } else {
                table(&mut doc, section)?.remove(name);
            }
        } else {
            replace_root(&mut doc, key, value.as_ref())?;
        }
    }
    crate::config::Config::parse(&doc.to_string())?;
    Ok(doc.to_string())
}

/// Full-document edits refuse concurrent changes rather than overwriting them.
pub fn apply_document(before: &str, after: &str) -> Result<(), String> {
    let path = crate::config::path();
    let current = read(&path)?.to_string();
    if current != before && current != after {
        return Err("config.toml changed outside Settings. Discard to reload it.".into());
    }
    let doc = after.parse().map_err(|error| format!("{error}"))?;
    write(&path, &doc)
}

/// The file as written, or the defaults when there is none yet.
fn read(path: &Path) -> Result<DocumentMut, String> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => crate::config::DEFAULT.to_string(),
        Err(error) => return Err(format!("{}: {error}", path.display())),
    };
    text.parse().map_err(|error| format!("{}: {error}", path.display()))
}

/// Writes through a temporary file, so a crash never leaves half a config;
/// the running compositor reloads it once it changes.
fn write(path: &Path, doc: &DocumentMut) -> Result<(), String> {
    let text = doc.to_string();
    crate::config::Config::parse(&text)
        .map_err(|error| format!("not changed, the result would be invalid: {error}"))?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|error| format!("{}: {error}", dir.display()))?;
    }
    let tmp = path.with_extension("toml.new");
    std::fs::write(&tmp, text).map_err(|error| format!("{}: {error}", tmp.display()))?;
    std::fs::rename(&tmp, path).map_err(|error| format!("{}: {error}", path.display()))
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
        .map_err(|error| format!("cannot read config defaults: {error}"))?;
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
            value @ Value::Boolean(_) => Ok(value),
            _ => Err(format!("{key} takes true or false, not {text:?}")),
        },
        Some(Value::Integer(_) | Value::Float(_)) => match parse("a number")? {
            value @ (Value::Integer(_) | Value::Float(_)) => Ok(value),
            _ => Err(format!("{key} takes a number, not {text:?}")),
        },
        Some(Value::Array(_)) => match parse("a list, such as [\"a\", \"b\"]")? {
            value @ Value::Array(_) => Ok(value),
            _ => Err(format!("{key} takes a list, such as [\"a\", \"b\"], not {text:?}")),
        },
        Some(_) => Ok(Value::from(text)),
        None => match text.parse::<Value>() {
            Ok(value @ (Value::Boolean(_) | Value::Integer(_) | Value::Float(_) | Value::Array(_))) => Ok(value),
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
    if list.iter().any(|value| value.as_str() == Some(text)) {
        return Ok(false);
    }
    list.push(text);
    let decor = lookup(doc, section, name).map(|value| value.decor().clone());
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
    fn splicing_a_section_keeps_the_file_in_order() {
        let base = "[general]\ngap = 12\n\n[[rules]]\napp_id = \"a\"\n\n[theme]\naccent = \"#ffffff\"\n\n\
                    [[rules]]\napp_id = \"b\"\n\n# Keys.\n[binds]\n\"Super+T\" = \"terminal\"\n\n[idle]\nlock_after = 0\n";
        // Unchanged excerpts go back where they came from.
        for key in ["binds", "rules", "theme"] {
            let excerpt = section_text(base, key).unwrap();
            assert_eq!(splice_section(base, key, &excerpt).unwrap(), base, "{key}");
        }
        // One changed value changes one line.
        let excerpt = section_text(base, "binds").unwrap().replace("terminal", "launcher");
        assert_eq!(splice_section(base, "binds", &excerpt).unwrap(), base.replace("terminal", "launcher"));
        // A new entry follows the last of its kind; a new section ends the file.
        let excerpt = format!("{}\n[[rules]]\napp_id = \"z\"\n", section_text(base, "rules").unwrap());
        let more = base.replace("# Keys.", "[[rules]]\napp_id = \"z\"\n\n# Keys.");
        assert_eq!(splice_section(base, "rules", &excerpt).unwrap(), more);
        let added = splice_section(base, "touchpad", "[touchpad]\nnatural_scroll = true\n").unwrap();
        assert_eq!(added, format!("{base}[touchpad]\nnatural_scroll = true\n"));
        let excerpt = section_text(base, "rules").unwrap().replace("\"b\"", "\"c\"");
        assert_eq!(splice_section(base, "rules", &excerpt).unwrap(), base.replace("\"b\"", "\"c\""));
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
    fn insert_plugin_appends_its_table_and_keeps_the_rest() {
        let original = "# Mine.\n[shell]\nbar = \"builtin\" # here\n";
        let mut file = doc(original);
        let args = vec!["/home/me/block.py".to_string(), "--stdio".to_string()];
        insert_plugin(&mut file, "block", "python3", &args, false).unwrap();
        assert_eq!(
            file.to_string(),
            format!(
                "{original}\n[plugins.block]\ncommand = \"python3\"\nargs = [\"/home/me/block.py\", \"--stdio\"]\n"
            )
        );
        insert_plugin(&mut file, "chips", "/usr/bin/chips", &[], true).unwrap();
        assert!(file.to_string().ends_with("[plugins.chips]\ncommand = \"/usr/bin/chips\"\n\n[plugins.chips.bar]\n"));
        let config = crate::config::Config::parse(&file.to_string()).unwrap();
        let block = &config.file.plugins["block"];
        assert_eq!(block.command.as_deref(), Some("python3"));
        assert_eq!(block.args.as_deref(), Some(args.as_slice()));
        assert!(!block.in_bar());
        assert!(config.file.plugins["chips"].in_bar());
    }

    #[test]
    fn insert_plugin_keeps_comments_at_the_end_out_of_its_table() {
        let original = "[binds]\n\"Super+T\" = \"spawn foot\"\n# \"Super+B\" = \"spawn firefox\"\n";
        let mut file = doc(original);
        insert_plugin(&mut file, "block", "/bin/true", &[], false).unwrap();
        assert_eq!(file.to_string(), format!("{original}\n[plugins.block]\ncommand = \"/bin/true\"\n"));
        crate::config::Config::parse(&file.to_string()).unwrap();
    }

    #[test]
    fn insert_plugin_leaves_an_earlier_plugins_comment_with_it() {
        let original = "[plugins.old]\ncommand = \"/bin/old\"\n# enabled = false\n";
        let mut file = doc(original);
        insert_plugin(&mut file, "chips", "/bin/chips", &[], true).unwrap();
        assert_eq!(
            file.to_string(),
            format!("{original}\n[plugins.chips]\ncommand = \"/bin/chips\"\n\n[plugins.chips.bar]\n")
        );
        // A comment that belongs to the table after it stays there, and the
        // new table still goes after everything.
        let original = "[plugins.old]\ncommand = \"/bin/old\"\n\n# Bar.\n[shell]\nbar = \"builtin\"\n";
        let mut file = doc(original);
        insert_plugin(&mut file, "block", "/bin/block", &[], false).unwrap();
        assert_eq!(file.to_string(), format!("{original}\n[plugins.block]\ncommand = \"/bin/block\"\n"));
        crate::config::Config::parse(&file.to_string()).unwrap();
    }

    #[test]
    fn switching_on_a_dashboard_keeps_the_rest() {
        // A plugin the file does not mention yet.
        let mut fresh = doc("[shell]\nbar = \"builtin\"\n\n[plugins.timers]\nenabled = true\n");
        switch_on_dashboard(&mut fresh, "lamp", false).unwrap();
        let text = fresh.to_string();
        assert!(
            text.ends_with("[plugins.timers]\nenabled = true\n\n[plugins.lamp.dashboard]\nenabled = true\n"),
            "{text}"
        );
        // One the file switched off, with its dashboard: both go on, and the
        // dashboard's block stays with its plugin.
        let mut off = doc(
            "[plugins.lamp]\nenabled = false\n# Mine.\n\n[plugins.lamp.dashboard]\nenabled = false\n\n[shell]\nbar = \"none\"\n",
        );
        switch_on_dashboard(&mut off, "lamp", true).unwrap();
        let text = off.to_string();
        assert_eq!(
            text,
            "[plugins.lamp]\nenabled = true\n# Mine.\n\n[plugins.lamp.dashboard]\nenabled = true\n\n[shell]\nbar = \"none\"\n"
        );
        let parsed: toml::Table = toml::from_str(&text).unwrap();
        assert_eq!(parsed["plugins"]["lamp"]["dashboard"]["enabled"].as_bool(), Some(true));
    }

    #[test]
    fn insert_plugin_refuses_an_id_in_use() {
        let original = "[plugins.block]\nenabled = false\n\n[launcher.plugins.old]\ncommand = \"/bin/old\"\n";
        let mut file = doc(original);
        let error = insert_plugin(&mut file, "block", "/bin/block", &[], false).unwrap_err();
        assert!(error.contains("[plugins.block]"), "{error}");
        let error = insert_plugin(&mut file, "old", "/bin/old", &[], false).unwrap_err();
        assert!(error.contains("[launcher.plugins.old]"), "{error}");
        assert_eq!(file.to_string(), original);
    }

    #[test]
    fn insert_plugin_into_the_defaults() {
        let mut file = defaults();
        insert_plugin(&mut file, "block", "/bin/block", &[], false).unwrap();
        let text = file.to_string();
        assert!(text.starts_with(crate::config::DEFAULT.trim_end()));
        assert!(crate::config::Config::parse(&text).unwrap().file.plugins.contains_key("block"));
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
