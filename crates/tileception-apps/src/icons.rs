use crate::{data_dirs, desktop::groups};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

type Cache = HashMap<(String, u32, u32), Option<PathBuf>>;

pub struct IconLookup {
    dirs: Vec<PathBuf>,
    themes: Vec<Theme>,
    cache: RefCell<Cache>,
}

struct Theme {
    name: String,
    directories: Vec<Directory>,
}

struct Directory {
    path: String,
    scale: u32,
    min: u32,
    max: u32,
}

impl Directory {
    fn distance(&self, size: u32, scale: u32) -> u64 {
        let target = u64::from(size) * u64::from(scale);
        let min = u64::from(self.min) * u64::from(self.scale);
        let max = u64::from(self.max) * u64::from(self.scale);
        if target < min {
            min - target
        } else {
            target.saturating_sub(max)
        }
    }

    fn matches(&self, size: u32, scale: u32) -> bool {
        self.scale == scale && (self.min..=self.max).contains(&size)
    }
}

impl IconLookup {
    /// Search roots contain theme directories. Recreate this value to invalidate caches.
    pub fn new(theme: &str, search_dirs: &[PathBuf]) -> Self {
        let mut themes = Vec::new();
        let mut visited = HashSet::new();
        load_theme(if theme.is_empty() { "hicolor" } else { theme }, search_dirs, &mut visited, &mut themes);
        load_theme("hicolor", search_dirs, &mut visited, &mut themes);
        Self { dirs: search_dirs.to_vec(), themes, cache: RefCell::new(HashMap::new()) }
    }

    pub fn default_dirs() -> Vec<PathBuf> {
        data_dirs().into_iter().map(|dir| dir.join("icons")).collect()
    }

    pub fn find(&self, icon: &str, size: u32, scale: u32) -> Option<PathBuf> {
        let key = (icon.to_owned(), size, scale.max(1));
        if let Some(cached) = self.cache.borrow().get(&key) {
            return cached.clone();
        }
        let result = self.lookup(icon, size, scale.max(1));
        self.cache.borrow_mut().insert(key, result.clone());
        result
    }

    fn lookup(&self, icon: &str, size: u32, scale: u32) -> Option<PathBuf> {
        let path = Path::new(icon);
        if path.is_absolute() {
            return path.is_file().then(|| path.to_owned());
        }
        if icon.is_empty() || path.components().count() != 1 {
            return None;
        }
        for theme in &self.themes {
            let mut directories = theme.directories.iter().collect::<Vec<_>>();
            directories.sort_by_key(|dir| (!dir.matches(size, scale), dir.distance(size, scale)));
            for directory in directories {
                for root in &self.dirs {
                    if let Some(file) = find_file(&root.join(&theme.name).join(&directory.path), icon) {
                        return Some(file);
                    }
                }
            }
        }
        for root in &self.dirs {
            if let Some(path) = find_file(root, icon) {
                return Some(path);
            }
        }
        for root in &self.dirs {
            if let Some(path) = root.parent().and_then(|parent| find_file(&parent.join("pixmaps"), icon)) {
                return Some(path);
            }
        }
        find_file(Path::new("/usr/share/pixmaps"), icon)
    }
}

fn find_file(dir: &Path, icon: &str) -> Option<PathBuf> {
    if Path::new(icon).extension().is_some_and(|extension| matches!(extension.to_str(), Some("png" | "svg" | "xpm"))) {
        let path = dir.join(icon);
        return path.is_file().then_some(path);
    }
    ["png", "svg", "xpm"].iter().map(|extension| dir.join(format!("{icon}.{extension}"))).find(|path| path.is_file())
}

fn load_theme(name: &str, roots: &[PathBuf], visited: &mut HashSet<String>, themes: &mut Vec<Theme>) {
    if name.is_empty() || Path::new(name).components().count() != 1 || !visited.insert(name.to_owned()) {
        return;
    }
    let Some(text) = roots.iter().find_map(|root| fs::read_to_string(root.join(name).join("index.theme")).ok()) else {
        return;
    };
    let sections = groups(&text);
    let Some(header) = sections.get("Icon Theme") else {
        return;
    };
    let mut directories = Vec::new();
    for path in ["Directories", "ScaledDirectories"]
        .iter()
        .filter_map(|key| header.get(*key))
        .flat_map(|value| value.split(','))
        .map(str::trim)
    {
        if Path::new(path).is_absolute()
            || Path::new(path).components().any(|part| matches!(part, std::path::Component::ParentDir))
        {
            continue;
        }
        let Some(section) = sections.get(path) else {
            continue;
        };
        let number =
            |key: &str, default| section.get(key).and_then(|value| value.parse::<u32>().ok()).unwrap_or(default);
        let size = number("Size", 0);
        if size == 0 {
            continue;
        }
        let (min, max) = match section.get("Type").map(String::as_str) {
            Some("Fixed") => (size, size),
            Some("Scalable") => (number("MinSize", size), number("MaxSize", size)),
            _ => {
                let threshold = number("Threshold", 2);
                (size.saturating_sub(threshold), size.saturating_add(threshold))
            }
        };
        directories.push(Directory { path: path.to_owned(), scale: number("Scale", 1).max(1), min, max });
    }
    themes.push(Theme { name: name.to_owned(), directories });
    for parent in header.get("Inherits").into_iter().flat_map(|value| value.split(',')).map(str::trim) {
        // Keep the mandatory fallback last, even when a theme lists it first.
        if parent != "hicolor" {
            load_theme(parent, roots, visited, themes);
        }
    }
}
