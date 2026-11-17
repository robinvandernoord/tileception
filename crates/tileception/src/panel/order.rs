//! The order of plugins chosen by dragging their blocks in the panel, used
//! for the panel's blocks and the bar's chips. It is kept with Tileception's
//! other state in `$XDG_STATE_HOME/tileception/plugin-order.toml`, not in the
//! config: like the launcher's history, it is something the user did, not a
//! setting they wrote. Plugins it does not name follow it in priority order.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
struct Stored {
    order: Vec<String>,
}

/// The file the plugins' order is kept in.
const PLUGINS: &str = "plugin-order.toml";

/// The saved order, or none.
pub fn load() -> Vec<String> {
    read_state(PLUGINS)
        .and_then(|text| toml::from_str::<Stored>(&text).ok())
        .map(|stored| stored.order)
        .unwrap_or_default()
}

/// Saves an order, replacing the file at once so a crash keeps the old one.
pub fn save(order: &[String]) {
    match toml::to_string(&Stored { order: order.to_vec() }) {
        Ok(text) => write_state(PLUGINS, &text),
        Err(error) => tracing::warn!("could not save {PLUGINS}: {error}"),
    }
}

/// A file of Tileception's state directory, unless missing or too large.
pub fn read_state(file: &str) -> Option<String> {
    std::fs::read_to_string(path(file)).ok().filter(|text| text.len() <= 64 * 1024)
}

/// Writes a file of Tileception's state directory, replacing it at once so
/// a crash keeps the old one.
pub fn write_state(file: &str, text: &str) {
    let result = (|| {
        let path = path(file);
        std::fs::create_dir_all(path.parent().expect("state directory"))?;
        let temporary = path.with_extension("toml.tmp");
        std::fs::write(&temporary, text)?;
        std::fs::rename(temporary, path)
    })();
    if let Err(error) = result {
        tracing::warn!("could not save {file}: {error}");
    }
}

fn path(file: &str) -> PathBuf {
    let root = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".local/state"));
    root.join("tileception").join(file)
}

/// The plugins in `priority` (highest first) as the user arranged them: those
/// `saved` names in its order, then the rest in priority order. Saved plugins
/// that are not there are left out.
pub fn arrange<'plugin>(priority: &[&'plugin str], saved: &[String]) -> Vec<&'plugin str> {
    let mut arranged: Vec<&str> = Vec::with_capacity(priority.len());
    for plugin in saved {
        if let Some(found) = priority.iter().find(|candidate| **candidate == plugin.as_str())
            && !arranged.contains(found)
        {
            arranged.push(found);
        }
    }
    for plugin in priority {
        if !arranged.contains(plugin) {
            arranged.push(plugin);
        }
    }
    arranged
}

/// The order to save once the plugins in `shown` (the blocks on show) were
/// put in that order. Every other plugin keeps its place, including saved
/// ones that are not running now, so they come back where they were.
pub fn rearrange(priority: &[&str], saved: &[String], shown: &[&str]) -> Vec<String> {
    let mut full: Vec<String> = Vec::with_capacity(saved.len() + priority.len());
    for plugin in saved.iter().map(String::as_str).chain(priority.iter().copied()) {
        if !full.iter().any(|known| known == plugin) {
            full.push(plugin.to_string());
        }
    }
    let mut next = shown.iter();
    for slot in &mut full {
        if shown.contains(&slot.as_str())
            && let Some(plugin) = next.next()
        {
            *slot = (*plugin).to_string();
        }
    }
    full
}

/// Where a block dragged to `pointer_y` would go, as the gap it would take:
/// 0 above the first block, `middles.len()` below the last.
pub fn slot(middles: &[f64], pointer_y: f64) -> usize {
    middles.iter().filter(|middle| **middle < pointer_y).count()
}

/// The blocks' order after moving the one at `from` to the gap `slot`.
pub fn moved<'plugin>(shown: &[&'plugin str], from: usize, slot: usize) -> Vec<&'plugin str> {
    let mut order = shown.to_vec();
    if from >= order.len() {
        return order;
    }
    let plugin = order.remove(from);
    let place = if slot > from { slot - 1 } else { slot };
    order.insert(place.min(order.len()), plugin);
    order
}

#[cfg(test)]
mod tests {
    use super::*;

    fn saved(plugins: &[&str]) -> Vec<String> {
        plugins.iter().map(|plugin| (*plugin).to_string()).collect()
    }

    #[test]
    fn without_a_saved_order_priority_applies() {
        assert_eq!(arrange(&["home", "bluetooth"], &[]), ["home", "bluetooth"]);
    }

    #[test]
    fn saved_order_comes_first_and_new_plugins_follow_by_priority() {
        let priority = ["weather", "home", "bluetooth", "folders"];
        let order = arrange(&priority, &saved(&["bluetooth", "home"]));
        assert_eq!(order, ["bluetooth", "home", "weather", "folders"]);
    }

    #[test]
    fn unknown_and_repeated_saved_plugins_are_skipped() {
        let order = arrange(&["home", "bluetooth"], &saved(&["gone", "bluetooth", "bluetooth", "home"]));
        assert_eq!(order, ["bluetooth", "home"]);
    }

    #[test]
    fn rearranging_keeps_missing_and_hidden_plugins_in_place() {
        // `gone` is not running now; `folders` has no block.
        let priority = ["home", "folders", "bluetooth", "weather"];
        let before = saved(&["home", "gone", "folders", "bluetooth"]);
        let after = rearrange(&priority, &before, &["bluetooth", "home", "weather"]);
        assert_eq!(after, ["bluetooth", "gone", "folders", "home", "weather"]);
        assert_eq!(arrange(&priority, &after), ["bluetooth", "folders", "home", "weather"]);
    }

    #[test]
    fn rearranging_without_a_saved_order_starts_from_priority() {
        let after = rearrange(&["home", "bluetooth"], &[], &["bluetooth", "home"]);
        assert_eq!(after, ["bluetooth", "home"]);
    }

    #[test]
    fn slots_follow_the_middles_of_the_blocks() {
        let middles = [100.0, 200.0, 300.0];
        assert_eq!(slot(&middles, 50.0), 0);
        assert_eq!(slot(&middles, 150.0), 1);
        assert_eq!(slot(&middles, 350.0), 3);
    }

    #[test]
    fn moving_down_and_up() {
        let shown = ["a", "b", "c"];
        assert_eq!(moved(&shown, 0, 3), ["b", "c", "a"]);
        assert_eq!(moved(&shown, 0, 2), ["b", "a", "c"]);
        assert_eq!(moved(&shown, 2, 0), ["c", "a", "b"]);
        // Its own gaps leave it where it was.
        assert_eq!(moved(&shown, 1, 1), shown);
        assert_eq!(moved(&shown, 1, 2), shown);
    }
}
