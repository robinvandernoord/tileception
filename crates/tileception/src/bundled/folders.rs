//! The `folders` plugin: `z words` in the launcher lists often and recently
//! visited folders from z and zoxide ([`crate::jumps`]).

use std::{
    path::{Path, PathBuf},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use serde_json::{Value, json};

use super::{Plugin, tool};
use crate::jumps;

/// Rows per search, as many as the launcher shows.
const ROWS: usize = 64;
/// How long a read of z and zoxide serves further searches, so typing does
/// not read them again for every key.
const FRESH: Duration = Duration::from_secs(5);

#[derive(Default)]
pub struct Folders {
    known: Option<(Instant, Vec<jumps::Folder>)>,
}

impl Folders {
    fn folders(&mut self, home: &Path) -> &[jumps::Folder] {
        if self.known.as_ref().is_none_or(|(read, _)| read.elapsed() > FRESH) {
            let now = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |elapsed| elapsed.as_secs() as i64);
            self.known = Some((Instant::now(), jumps::load(home, now)));
        }
        self.known.as_ref().map(|(_, folders)| folders.as_slice()).unwrap_or_default()
    }
}

impl Plugin for Folders {
    fn tools(&self) -> Value {
        json!([
            tool("launcher_info", "Launcher metadata", &[]),
            tool("launcher_search", "Often visited folders matching the words", &["prefix", "query"]),
        ])
    }

    fn call(&mut self, name: &str, arguments: &Value) -> Result<Value, String> {
        match name {
            "launcher_info" => Ok(json!({
                "name": "Folders",
                "description": "Often visited folders from z and zoxide",
                "icon": "glyph:folder",
                "prefixes": ["z"],
            })),
            "launcher_search" => {
                let home = PathBuf::from(std::env::var_os("HOME").unwrap_or_default());
                let words: Vec<&str> = arguments["query"].as_str().unwrap_or_default().split_whitespace().collect();
                let rows: Vec<Value> = jumps::matching(self.folders(&home), &words, ROWS)
                    .into_iter()
                    .map(|folder| row(&folder.path, &home))
                    .collect();
                Ok(json!({ "rows": rows }))
            }
            _ => Err(format!("Unknown tool {name}")),
        }
    }
}

/// A folder's row: its name over where it is, with `~` for home. Enter
/// opens it, Tab completes its path, Ctrl+Enter opens a terminal there.
fn row(path: &Path, home: &Path) -> Value {
    let shown = match path.strip_prefix(home) {
        Ok(rest) if !home.as_os_str().is_empty() && rest.as_os_str().is_empty() => "~".to_string(),
        Ok(rest) if !home.as_os_str().is_empty() => format!("~/{}", rest.display()),
        _ => path.display().to_string(),
    };
    let title = path.file_name().map_or_else(|| shown.clone(), |name| name.to_string_lossy().into_owned());
    let target = path.to_string_lossy();
    let completion = if shown.ends_with('/') { shown.clone() } else { format!("{shown}/") };
    json!({
        "title": title,
        "detail": shown,
        "icon": "glyph:folder",
        "completion": completion,
        "actions": [{ "title": "Open", "type": "open", "target": target }],
        "alternate": { "title": "Terminal", "type": "terminal", "directory": target },
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test assertions")]
mod tests {
    use super::*;

    #[test]
    fn rows_show_home_as_a_tilde() {
        let home = Path::new("/home/me");
        let row = row(Path::new("/home/me/src/web"), home);
        assert_eq!(row["title"], "web");
        assert_eq!(row["detail"], "~/src/web");
        assert_eq!(row["completion"], "~/src/web/");
        assert_eq!(row["actions"][0]["target"], "/home/me/src/web");
        assert_eq!(row["alternate"]["directory"], "/home/me/src/web");
        let home_row = super::row(home, home);
        assert_eq!(home_row["detail"], "~");
        assert_eq!(home_row["completion"], "~/");
        assert_eq!(super::row(Path::new("/srv/data"), home)["detail"], "/srv/data");
    }

    #[test]
    fn rows_parse_as_launcher_rows() {
        let rows =
            crate::plugins::parse_rows(json!({ "rows": [row(Path::new("/home/me/src"), Path::new("/home/me"))] }))
                .unwrap();
        assert_eq!(rows[0].completion.as_deref(), Some("~/src/"));
        assert_eq!(
            rows[0].alternate.as_ref().map(|action| &action.kind),
            Some(&crate::plugins::ActionKind::Terminal { directory: "/home/me/src".into() })
        );
    }
}
