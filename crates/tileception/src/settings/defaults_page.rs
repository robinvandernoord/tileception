//! The Default apps page: which application opens links, folders and each
//! kind of file, as `xdg-mime` manages them.
//!
//! Common kinds come first, each set for all its types at once. "Show all
//! file types" lists every type an installed application opens. Choosing an
//! application is part of the draft like any setting: Apply writes the
//! user's `mimeapps.list`, as `xdg-mime default` does, and Discard drops it.

use super::*;

/// The page's title, as it appears in [`CATEGORIES`].
pub(super) const TITLE: &str = "Default apps";

/// Common kinds of link and file: a title, what it covers, and its types.
const GROUPS: &[(&str, &str, &[&str])] = &[
    ("Web browser", "Links and web pages", &["x-scheme-handler/http", "x-scheme-handler/https", "text/html"]),
    ("Email", "Email links", &["x-scheme-handler/mailto"]),
    ("Files", "Folders", &["inode/directory"]),
    ("Text editor", "Plain text files", &["text/plain"]),
    (
        "Images",
        "Photos and pictures",
        &["image/png", "image/jpeg", "image/gif", "image/webp", "image/bmp", "image/tiff", "image/svg+xml"],
    ),
    (
        "Music",
        "Audio files",
        &[
            "audio/mpeg",
            "audio/flac",
            "audio/ogg",
            "audio/x-vorbis+ogg",
            "audio/x-wav",
            "audio/mp4",
            "audio/aac",
            "audio/opus",
        ],
    ),
    (
        "Video",
        "Video files",
        &["video/mp4", "video/x-matroska", "video/webm", "video/quicktime", "video/x-msvideo", "video/mpeg"],
    ),
    ("PDF documents", "Portable documents", &["application/pdf"]),
    (
        "Archives",
        "Zip, tar and other archives",
        &[
            "application/zip",
            "application/x-tar",
            "application/gzip",
            "application/x-compressed-tar",
            "application/x-xz-compressed-tar",
            "application/x-7z-compressed",
            "application/vnd.rar",
            "application/zstd",
        ],
    ),
    ("Calendar", "Events and invitations", &["text/calendar"]),
];

const ROW: f64 = 60.;
const TYPE_ROW: f64 = 48.;
const TOGGLE: f64 = 34.;
const MENU_ROW: f64 = 32.;

/// One row: a kind of file or a single type, the application that opens it
/// now and those that could.
#[derive(Clone, Debug)]
pub(super) struct Entry {
    pub label: String,
    pub detail: String,
    pub types: Vec<String>,
    /// The current application's id and name.
    pub current: Option<(String, String)>,
    /// The group's types open in different applications.
    pub mixed: bool,
    /// The applications that open the types, as (id, name).
    pub choices: Vec<(String, String)>,
}

/// A default chosen on the page and not applied yet.
#[derive(Clone, Debug)]
pub(super) struct Pending {
    pub types: Vec<String>,
    pub label: String,
    /// The chosen application's id and name.
    pub app: (String, String),
}

/// Where the page's rows go.
pub(super) struct AppsFrame {
    /// Each row and its entry.
    pub rows: Vec<(Rect, usize)>,
    /// The common kinds' card, and the card of all other types.
    pub cards: Vec<Rect>,
    pub toggle: Rect,
    pub height: f64,
}

impl AppsFrame {
    pub(super) fn shift(&mut self, amount: f64) {
        for (rect, _) in &mut self.rows {
            rect.y += amount;
        }
        for rect in &mut self.cards {
            rect.y += amount;
        }
        self.toggle.y += amount;
    }
}

/// The button showing an entry's application.
pub(super) fn app_control(row: Rect) -> Rect {
    Rect::new(row.right() - 254., row.y + (row.h - 36.) / 2., 230., 36.)
}

impl Tileception {
    /// Reads the current defaults for the page.
    pub(super) fn settings_load_apps(&mut self) {
        self.settings.apps_menu = None;
        self.settings.apps = self.settings_app_entries();
    }

    fn settings_app_entries(&self) -> Vec<Entry> {
        let index = self.launcher.apps();
        let files = tileception_apps::Associations::load();
        let entry = |label: String, detail: String, types: Vec<String>| {
            let borrowed: Vec<&str> = types.iter().map(String::as_str).collect();
            let defaults: Vec<Option<&tileception_apps::App>> =
                borrowed.iter().map(|mime| index.default_with(mime, &files)).collect();
            let current = defaults.iter().flatten().next().map(|app| (app.id.clone(), app.name.clone()));
            let mixed =
                defaults.iter().flatten().any(|app| current.as_ref().is_some_and(|(app_id, _)| *app_id != app.id));
            let mut choices: Vec<(String, String)> =
                index.opening(&borrowed).into_iter().map(|app| (app.id.clone(), app.name.clone())).collect();
            if let Some(current) = &current
                && !choices.iter().any(|(app_id, _)| *app_id == current.0)
            {
                choices.insert(0, current.clone());
            }
            Entry { label, detail, types, current, mixed, choices }
        };
        let mut entries: Vec<Entry> = GROUPS
            .iter()
            .map(|(label, detail, types)| {
                entry((*label).into(), (*detail).into(), types.iter().map(|mime| (*mime).to_string()).collect())
            })
            .collect();
        if self.settings.apps_all {
            let grouped: Vec<&str> = GROUPS.iter().flat_map(|(_, _, types)| types.iter().copied()).collect();
            for mime in index.mime_types().into_iter().filter(|mime| !grouped.contains(mime)) {
                entries.push(entry(mime.to_string(), String::new(), vec![mime.to_string()]));
            }
        }
        entries
    }

    /// The page's rows with their top left at (x, top).
    pub(super) fn settings_apps_layout(&self, left: f64, top: f64, width: f64) -> AppsFrame {
        let mut rows = Vec::new();
        let mut cursor = top;
        let groups = GROUPS.len().min(self.settings.apps.len());
        for index in 0..groups {
            rows.push((Rect::new(left, cursor, width, ROW), index));
            cursor += ROW;
        }
        let mut cards = vec![Rect::new(left, top, width, cursor - top)];
        let toggle = Rect::new(left, cursor + 16., width, TOGGLE);
        cursor = toggle.bottom();
        if self.settings.apps_all && self.settings.apps.len() > groups {
            // Below the toggle, with room for the card's heading.
            let start = cursor + 40.;
            cursor = start;
            for index in groups..self.settings.apps.len() {
                rows.push((Rect::new(left, cursor, width, TYPE_ROW), index));
                cursor += TYPE_ROW;
            }
            cards.push(Rect::new(left, start, width, cursor - start));
        }
        AppsFrame { rows, cards, toggle, height: cursor - top }
    }

    /// The open menu's options, below their row's button, or above it near
    /// the bottom; as many as fit.
    pub(super) fn settings_apps_menu_rects(&self, frame: &Frame, entry: usize) -> Vec<Rect> {
        let Some(apps) = &frame.apps else { return Vec::new() };
        let Some((row, _)) = apps.rows.iter().find(|(_, index)| *index == entry) else { return Vec::new() };
        let Some(choices) = self.settings.apps.get(entry).map(|entry| entry.choices.len()) else { return Vec::new() };
        let control = app_control(*row);
        let lowest = frame.panel.bottom() - 64.;
        let highest = frame.panel.y + 70.;
        let below = lowest - control.bottom() - 6.;
        let above = control.y - 6. - highest;
        let room = below.max(above);
        let count = choices.min((room / MENU_ROW).floor().max(1.) as usize);
        let height = count as f64 * MENU_ROW;
        let top = if height <= below { control.bottom() + 6. } else { (control.y - 6. - height).max(highest) };
        let width = control.w.max(260.);
        (0..count)
            .map(|position| Rect::new(control.right() - width, top + position as f64 * MENU_ROW, width, MENU_ROW))
            .collect()
    }

    /// Makes a choice the default for all of its entry's types.
    /// Makes a choice the entry's pending default, applied with the rest of
    /// the draft. Choosing what is applied now drops the pending change.
    pub(super) fn settings_choose_app(&mut self, entry: usize, choice: usize) {
        self.settings.apps_menu = None;
        let Some(data) = self.settings.apps.get(entry) else { return };
        let Some(app) = data.choices.get(choice).cloned() else { return };
        let label = data.label.clone();
        let applied = data.current.as_ref().is_some_and(|(app_id, _)| *app_id == app.0) && !data.mixed;
        let types = data.types.clone();
        self.settings.apps_pending.retain(|pending| pending.types != types);
        if applied {
            self.settings.message = format!("{label} stays with {}.", app.1);
        } else {
            self.settings.message = format!("{label} will open in {} once you apply.", app.1);
            self.settings.apps_pending.push(Pending { types, label, app });
        }
        self.settings.error = false;
    }

    /// The application an entry shows: its pending choice, else what is
    /// applied now; and whether it is pending.
    pub(super) fn settings_shown_app(&self, entry: &Entry) -> (Option<(String, String)>, bool) {
        match self.settings.apps_pending.iter().find(|pending| pending.types == entry.types) {
            Some(pending) => (Some(pending.app.clone()), true),
            None => (entry.current.clone(), false),
        }
    }

    /// Writes the pending defaults to `mimeapps.list`. Returns how many were
    /// written; those that failed stay pending.
    pub(super) fn settings_apply_apps(&mut self) -> Result<usize, String> {
        let mut written = 0;
        let mut failed = Vec::new();
        let mut first_error = None;
        for pending in std::mem::take(&mut self.settings.apps_pending) {
            let types: Vec<&str> = pending.types.iter().map(String::as_str).collect();
            match tileception_apps::set_default(&types, &pending.app.0) {
                Ok(_) => written += 1,
                Err(error) => {
                    first_error
                        .get_or_insert_with(|| format!("Could not set the default for {}: {error}", pending.label));
                    failed.push(pending);
                }
            }
        }
        self.settings.apps_pending = failed;
        if written > 0 {
            self.settings.apps = self.settings_app_entries();
        }
        first_error.map_or(Ok(written), Err)
    }

    /// Drops the pending defaults.
    pub(super) fn settings_discard_apps(&mut self) {
        self.settings.apps_pending.clear();
        self.settings.apps_menu = None;
    }

    /// Review changes' lines for the pending defaults, as `mimeapps.list`
    /// says them now or will after applying.
    pub(super) fn settings_apps_review(&self, after: bool) -> Vec<String> {
        if self.settings.apps_pending.is_empty() {
            return Vec::new();
        }
        let mut lines = vec!["# ~/.config/mimeapps.list".to_string(), "[Default Applications]".to_string()];
        let index = self.launcher.apps();
        let files = tileception_apps::Associations::load();
        for pending in &self.settings.apps_pending {
            let current = self.settings.apps.iter().find(|entry| entry.types == pending.types);
            for mime in &pending.types {
                let app = if after {
                    pending.app.0.clone()
                } else {
                    index
                        .default_with(mime, &files)
                        .map(|app| app.id.clone())
                        .or_else(|| current.and_then(|entry| entry.current.as_ref()).map(|(app_id, _)| app_id.clone()))
                        .unwrap_or_else(|| "(none)".into())
                };
                lines.push(format!("{mime}={app};"));
            }
        }
        lines
    }

    pub(super) fn settings_draw_apps_page(&self, painter: &mut Painter<'_>, apps: &AppsFrame, pointer: Point) {
        let theme = &self.theme;
        for (index, card) in apps.cards.iter().enumerate() {
            let heading = if index == 0 { "COMMON" } else { "ALL FILE TYPES" };
            painter.text(
                heading,
                Rect::new(card.x + 2., card.y - 24., card.w, 20.),
                Style::new(10., theme.muted).weight(600),
            );
            painter.shape(*card, Shape { border_width: 1., border: theme.border, ..Shape::filled(theme.surface, 8.) });
        }
        let last = apps.rows.len().saturating_sub(1);
        for (position, (row, index)) in apps.rows.iter().enumerate() {
            let Some(entry) = self.settings.apps.get(*index) else { continue };
            let next_card = apps.rows.get(position + 1).is_some_and(|(next, _)| next.y != row.bottom());
            if position != last && !next_card {
                painter.fill(Rect::new(row.x + 16., row.bottom() - 1., row.w - 32., 1.), theme.border, 0.);
            }
            let label_w = row.w - 290.;
            if entry.detail.is_empty() {
                painter.text(&entry.label, Rect::new(row.x + 18., row.y, label_w, row.h), Style::new(12., theme.text));
            } else {
                let top = row.y + (row.h - 42.) / 2.;
                painter.text(
                    &entry.label,
                    Rect::new(row.x + 18., top, label_w, 21.),
                    Style::new(12., theme.text).weight(500),
                );
                painter.text(
                    &entry.detail,
                    Rect::new(row.x + 18., top + 22., label_w, 20.),
                    Style::new(11., theme.muted),
                );
            }
            let control = app_control(*row);
            let open = self.settings.apps_menu.is_some_and(|(open, _)| open == *index);
            let usable = !entry.choices.is_empty();
            let focused = self.settings.focus == Some(Hit::AppDefault(*index));
            let hovered = usable && control.contains(pointer);
            painter.shape(
                control,
                Shape {
                    border_width: 1.,
                    border: if focused || open { theme.accent } else { theme.border },
                    ..Shape::filled(
                        if hovered || open { theme.surface_raised } else { theme.background.alpha(0.4) },
                        6.,
                    )
                },
            );
            let (shown, pending) = self.settings_shown_app(entry);
            // A pending choice is marked like a changed setting.
            if pending {
                painter.fill(Rect::new(row.x + 1., row.y + 10., 3., row.h - 20.), theme.accent, 1.);
            }
            let mut text_x = control.x + 10.;
            if let Some((app_id, _)) = &shown
                && let Some(icon) = self.launcher.app_icon(app_id, 20)
            {
                painter.image(&icon, (control.x + 8., control.y + (control.h - 20.) / 2.));
                text_x = control.x + 34.;
            }
            let (label, colour) = match &shown {
                Some((_, name)) if entry.mixed && !pending => (format!("{name}, mostly"), theme.text),
                Some((_, name)) => (name.clone(), theme.text),
                None if usable => ("Not set".into(), theme.muted),
                None => ("No app opens these".into(), theme.muted),
            };
            painter.text(
                &label,
                Rect::new(text_x, control.y, control.right() - 26. - text_x, control.h),
                Style::new(11., colour),
            );
            if usable {
                painter.glyph(
                    glyph::CHEVRON_DOWN,
                    Rect::new(control.right() - 22., control.y, 16., control.h),
                    12,
                    theme.muted,
                );
            }
        }
        let hovered = apps.toggle.contains(pointer);
        let focused = self.settings.focus == Some(Hit::AppsAll);
        painter.shape(
            apps.toggle,
            Shape {
                border_width: 1.,
                border: if focused { theme.accent } else { theme.border },
                ..Shape::filled(if hovered { theme.surface_raised } else { theme.surface }, 6.)
            },
        );
        painter.glyph(
            if self.settings.apps_all { glyph::CHEVRON_DOWN } else { glyph::CHEVRON_RIGHT },
            Rect::new(apps.toggle.x + 10., apps.toggle.y + 8., 18., 18.),
            16,
            theme.muted,
        );
        let toggle = if self.settings.apps_all {
            format!("Fewer file types ({} more shown)", self.settings.apps.len().saturating_sub(GROUPS.len()))
        } else {
            "Show all file types".into()
        };
        painter.text(
            &toggle,
            Rect::new(apps.toggle.x + 36., apps.toggle.y, apps.toggle.w - 44., apps.toggle.h),
            Style::new(12., theme.text),
        );
    }

    /// The open menu of applications, over everything else.
    pub(super) fn settings_draw_apps_menu(&self, painter: &mut Painter<'_>, frame: &Frame, pointer: Point) {
        let Some((entry, highlighted)) = self.settings.apps_menu else { return };
        let Some(entry_data) = self.settings.apps.get(entry) else { return };
        let (shown, _) = self.settings_shown_app(entry_data);
        let theme = &self.theme;
        let rects = self.settings_apps_menu_rects(frame, entry);
        let (Some(first), Some(last)) = (rects.first(), rects.last()) else { return };
        painter.shape(
            Rect::new(first.x - 4., first.y - 4., first.w + 8., last.bottom() - first.y + 8.),
            Shape {
                border_width: 1.,
                border: theme.border,
                glow_radius: 16.,
                glow: Colour([0., 0., 0., 0.35]),
                ..Shape::filled(theme.surface, 8.)
            },
        );
        for (position, rect) in rects.into_iter().enumerate() {
            let Some((app_id, name)) = entry_data.choices.get(position) else { continue };
            if position == highlighted || rect.contains(pointer) {
                painter.fill(rect, theme.surface_raised, 5.);
            }
            let chosen = shown.as_ref().is_some_and(|(current, _)| current == app_id);
            if let Some(icon) = self.launcher.app_icon(app_id, 20) {
                painter.image(&icon, (rect.x + 8., rect.y + (rect.h - 20.) / 2.));
            }
            painter.text(
                name,
                Rect::new(rect.x + 36., rect.y, rect.w - 64., rect.h),
                Style::new(12., if chosen { theme.accent } else { theme.text }),
            );
            if chosen {
                painter.glyph(glyph::CHECK, Rect::new(rect.right() - 28., rect.y, 18., rect.h), 14, theme.accent);
            }
        }
    }
}
