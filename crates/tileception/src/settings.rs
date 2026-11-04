//! Native settings, with an unapplied draft and a miniature desktop preview.

use smithay::{
    backend::renderer::gles::GlesRenderer,
    input::keyboard::{keysyms, xkb},
};
use tileception_core::{Point, Rect};

mod connect;
mod editor;

use crate::{
    Tileception,
    config::{ConfigFile, Mods},
    render::OutputElement,
    shapes::Shape,
    theme::Colour,
    ui::{Align, Painter, Style, glyph},
};

#[derive(Clone, Copy)]
enum Control {
    Toggle,
    Number(f64, f64, f64, &'static str),
    Choice(&'static [&'static str]),
    Text,
    Colour,
    Toml,
}

struct Field {
    category: usize,
    key: &'static str,
    label: &'static str,
    description: &'static str,
    control: Control,
}

struct Category {
    title: &'static str,
    summary: &'static str,
    description: &'static str,
    icon: &'static str,
}

const CATEGORIES: &[Category] = &[
    Category {
        title: "Appearance",
        summary: "Accent, type & surfaces",
        description: "Colours, fonts and surfaces.",
        icon: glyph::CONTRAST,
    },
    Category {
        title: "Layout & motion",
        summary: "Spacing, tabs & animation",
        description: "Window spacing, tabs and animation.",
        icon: glyph::OVERVIEW,
    },
    Category {
        title: "Desktop components",
        summary: "Bar, launcher & notifications",
        description: "Built-in tools and external replacements.",
        icon: glyph::WINDOW,
    },
    Category {
        title: "Keyboard & touchpad",
        summary: "Keys, taps & scrolling",
        description: "Keyboard layout, repeat and touchpad behaviour.",
        icon: glyph::KEYBOARD,
    },
    Category {
        title: "Power & battery",
        summary: "Idle, locking & battery",
        description: "Idle time, screen locking and battery care.",
        icon: glyph::POWER,
    },
    Category {
        title: "Smart home",
        summary: "Connection & device import",
        description: "Smart-home connection and import settings.",
        icon: glyph::HOME,
    },
    Category {
        title: "Lock & session",
        summary: "Locker & session behaviour",
        description: "Lock screen and session settings.",
        icon: glyph::LOCK,
    },
    Category {
        title: "Shortcuts & startup",
        summary: "Bindings, commands & autostart",
        description: "Keyboard shortcuts and startup commands.",
        icon: glyph::TERMINAL,
    },
    Category {
        title: "Displays & sharing",
        summary: "Outputs, touchscreens & peers",
        description: "Display layout, input routing and connections to other computers.",
        icon: glyph::GLOBE,
    },
];

const fn field(
    category: usize,
    key: &'static str,
    label: &'static str,
    description: &'static str,
    control: Control,
) -> Field {
    Field { category, key, label, description, control }
}

const FIELDS: &[Field] = &[
    field(0, "theme.accent", "Accent colour", "The start of the focused window's gradient.", Control::Colour),
    field(0, "theme.accent_end", "Accent end", "Use the same colour for a solid accent.", Control::Colour),
    field(
        0,
        "theme.font_size",
        "Font size",
        "Text in the bar, launcher and built-in panels.",
        Control::Number(10., 22., 1., "pt"),
    ),
    field(0, "theme.radius", "Tile corners", "Corner radius of desktop tiles.", Control::Number(0., 18., 1., "px")),
    field(
        0,
        "theme.glow",
        "Focus glow",
        "How far the focused window's glow reaches.",
        Control::Number(0., 36., 1., "px"),
    ),
    field(
        0,
        "theme.colour_scheme",
        "Application appearance",
        "The preference sent to applications.",
        Control::Choice(&["dark", "light", "default"]),
    ),
    field(0, "theme.frost", "Frosted panels", "Translucent bar, menus and notifications.", Control::Toggle),
    field(
        1,
        "general.gap",
        "Window spacing",
        "Between windows and around the screen edge.",
        Control::Number(0., 40., 1., "px"),
    ),
    field(
        1,
        "general.animation_ms",
        "Animation length",
        "Zoom and move duration. Zero turns motion off.",
        Control::Number(0., 600., 10., "ms"),
    ),
    field(
        1,
        "general.tab_height",
        "Tab height",
        "The strip above a tile with multiple windows.",
        Control::Number(20., 44., 1., "px"),
    ),
    field(
        1,
        "general.drag_modifier",
        "Drag modifier",
        "Hold to move with left click or resize with right click.",
        Control::Choice(&["Alt", "Super", "Ctrl", "Shift"]),
    ),
    field(
        1,
        "general.share_removed_space",
        "Redistribute closed windows",
        "Spread the space over the row when a window closes.",
        Control::Toggle,
    ),
    field(
        1,
        "general.restore_layout",
        "Restore window positions",
        "Remember positions as apps reopen. Experimental; one output. Does not start apps.",
        Control::Toggle,
    ),
    field(
        2,
        "shell.bar",
        "Desktop bar",
        "Built-in, off, or keep your external command.",
        Control::Choice(&["builtin", "none"]),
    ),
    field(
        2,
        "shell.bar_position",
        "Bar position",
        "The edge used by the built-in bar.",
        Control::Choice(&["top", "bottom"]),
    ),
    field(2, "shell.bar_minimap", "Layout minimap", "A small map of your window tree in the bar.", Control::Toggle),
    field(
        2,
        "shell.tray_max_icons",
        "Tray slots",
        "Extra applications move into an overflow menu.",
        Control::Number(1., 8., 1., ""),
    ),
    field(2, "shell.launcher", "Launcher", "The panel opened with Super.", Control::Choice(&["builtin", "none"])),
    field(
        2,
        "shell.notifications",
        "Notifications",
        "Built-in notifications or your external command.",
        Control::Choice(&["builtin", "none"]),
    ),
    field(
        2,
        "shell.doctor",
        "Doctor",
        "The assistant panel opened with Super+/.",
        Control::Choice(&["builtin", "none"]),
    ),
    field(3, "keyboard.layout", "Keyboard layout", "XKB layout names, such as us or us,nl.", Control::Text),
    field(
        3,
        "keyboard.repeat_delay",
        "Repeat delay",
        "Delay before a held key repeats.",
        Control::Number(150., 1000., 25., "ms"),
    ),
    field(
        3,
        "keyboard.repeat_rate",
        "Repeat rate",
        "Characters per second when holding a key.",
        Control::Number(10., 60., 1., "/s"),
    ),
    field(3, "touchpad.tap", "Tap to click", "One finger for left click, two for right click.", Control::Toggle),
    field(3, "touchpad.natural_scroll", "Natural scrolling", "Content follows your fingers.", Control::Toggle),
    field(
        3,
        "touchpad.disable_while_typing",
        "Ignore touchpad while typing",
        "Avoid accidental input while using the keyboard.",
        Control::Toggle,
    ),
    field(3, "touchpad.speed", "Pointer speed", "From slower to faster.", Control::Number(-1., 1., 0.1, "")),
    field(
        4,
        "idle.lock_after",
        "Lock after idle",
        "Seconds of inactivity. Zero means never.",
        Control::Number(0., 1800., 60., "s"),
    ),
    field(
        4,
        "idle.off_after",
        "Screen off after idle",
        "Seconds of inactivity. Zero means never.",
        Control::Number(0., 3600., 60., "s"),
    ),
    field(
        4,
        "idle.lock_before_sleep",
        "Lock before sleep",
        "Keep your desktop private when it wakes.",
        Control::Toggle,
    ),
    field(
        4,
        "battery.power_saver_at",
        "Power saver at",
        "Switch profiles at or below this charge.",
        Control::Number(0., 50., 1., "%"),
    ),
    field(
        4,
        "battery.hibernate_at",
        "Hibernate at",
        "Once per discharge. Zero disables hibernation.",
        Control::Number(0., 20., 1., "%"),
    ),
    field(4, "battery.alerts", "Low battery alerts", "Notifications at 20, 15 and 10 percent.", Control::Toggle),
    field(
        5,
        "home.enabled",
        "Smart home in the panel",
        "Show connected devices in the notification panel.",
        Control::Toggle,
    ),
    field(5, "home.devices", "Device file", "Empty uses the standard home-devices.json path.", Control::Text),
    field(
        6,
        "shell.lock",
        "Locker",
        "Built-in, off, or an external locker command.",
        Control::Choice(&["builtin", "none"]),
    ),
    field(
        6,
        "general.lock_command",
        "Lock command",
        "Optional custom command. {wallpaper} expands to the wallpaper path.",
        Control::Text,
    ),
    field(6, "general.suspend_enabled", "Allow suspend", "Show suspend in the power menu.", Control::Toggle),
    field(6, "general.welcome", "Welcome launcher", "Show the launcher on an empty desktop.", Control::Toggle),
    field(7, "general.autostart", "Autostart commands", "Commands run once when the session starts.", Control::Toml),
    field(
        7,
        "general.default_binds",
        "Default shortcuts",
        "Include the built-in bindings; custom shortcuts override them.",
        Control::Toggle,
    ),
    field(7, "binds", "Keyboard & gesture shortcuts", "Map key combinations and gestures to commands.", Control::Toml),
    field(7, "general.terminal", "Default terminal", "Terminal command used by the desktop.", Control::Text),
    field(7, "launch_args", "Application arguments", "Extra arguments by desktop file name.", Control::Toml),
    field(0, "theme.font", "Interface font", "Font family used by built-in panels.", Control::Text),
    field(0, "theme.wallpaper", "Wallpaper", "Image path. Empty uses the background colour.", Control::Text),
    field(
        0,
        "theme.wallpaper_fit",
        "Wallpaper fit",
        "How the image fits the display.",
        Control::Choice(&["fill", "fit", "stretch", "center"]),
    ),
    field(
        0,
        "theme.popup_radius",
        "Panel corners",
        "Corner radius of menus and panels.",
        Control::Number(0., 24., 1., "px"),
    ),
    field(0, "theme.background", "Background", "Desktop background colour.", Control::Colour),
    field(0, "theme.surface", "Panel colour", "Standard panel surface.", Control::Colour),
    field(0, "theme.surface_raised", "Raised surface", "Buttons and raised panels.", Control::Colour),
    field(0, "theme.border", "Border colour", "Unfocused borders and dividers.", Control::Colour),
    field(0, "theme.text", "Text colour", "Primary interface text.", Control::Colour),
    field(0, "theme.muted", "Secondary text", "Descriptions and inactive controls.", Control::Colour),
    field(0, "theme.on_accent", "Text on accent", "Text on primary buttons.", Control::Colour),
    field(0, "theme.danger", "Error colour", "Errors and critical notifications.", Control::Colour),
    field(0, "theme.warning", "Warning colour", "Warnings and caution states.", Control::Colour),
    field(0, "theme.guest", "Guest window colour", "Borders of windows from another computer.", Control::Colour),
    field(1, "rules", "Window rules", "Floating, size, fullscreen and minimized rules.", Control::Toml),
    field(
        2,
        "shell.power_menu",
        "Power menu",
        "Built-in, off, or an external command.",
        Control::Choice(&["builtin", "none"]),
    ),
    field(2, "shell.media", "Media controls", "Built-in media controls or off.", Control::Choice(&["builtin", "none"])),
    field(
        2,
        "media.volume_step",
        "Volume step",
        "Percentage changed by each volume key press.",
        Control::Number(1., 25., 1., "%"),
    ),
    field(
        2,
        "media.max_volume",
        "Maximum volume",
        "Allow amplification beyond 100 percent.",
        Control::Number(100., 200., 5., "%"),
    ),
    field(
        2,
        "media.brightness_step",
        "Brightness step",
        "Percentage changed by each brightness key press.",
        Control::Number(1., 25., 1., "%"),
    ),
    field(2, "media.osd", "On-screen indicators", "Show volume and brightness changes.", Control::Toggle),
    field(
        2,
        "media.osd_timeout_ms",
        "Indicator duration",
        "How long an indicator remains visible.",
        Control::Number(100., 5000., 100., "ms"),
    ),
    field(2, "duck.size", "Rubber duck size", "Size of the desktop duck.", Control::Number(24., 160., 4., "px")),
    field(3, "keyboard.variant", "Keyboard variant", "XKB variant, such as altgr-intl.", Control::Text),
    field(3, "keyboard.options", "Keyboard options", "XKB options, such as compose:ralt.", Control::Text),
    field(3, "keyboard.model", "Keyboard model", "Empty uses the default XKB model.", Control::Text),
    field(
        3,
        "touchpad.click_method",
        "Touchpad click method",
        "Empty uses the device default.",
        Control::Choice(&["areas", "clickfinger"]),
    ),
    field(
        3,
        "touchpad.swipe_distance",
        "Swipe distance",
        "Distance before a swipe binding triggers.",
        Control::Number(10., 300., 5., "px"),
    ),
    field(
        4,
        "battery.border",
        "Battery border colours",
        "Colour the focused border by battery charge.",
        Control::Toggle,
    ),
    field(4, "battery.charging", "Charging colour", "A hex colour or a named theme colour.", Control::Text),
    field(4, "battery.levels", "Battery colour thresholds", "Charge levels and their border colours.", Control::Toml),
    field(
        4,
        "battery.power_profiles",
        "Automatic power profiles",
        "Select profiles based on charger and battery state.",
        Control::Toggle,
    ),
    field(
        6,
        "greeter.share",
        "Share appearance with login screen",
        "Publish this session's accent and wallpaper.",
        Control::Toggle,
    ),
    field(8, "touchscreens", "Touchscreens", "Map touchscreen device names to outputs.", Control::Toml),
    field(8, "outputs", "Displays", "Scale, position and split areas by connector.", Control::Toml),
    field(8, "handoff", "Sharing", "Peer names, addresses, discovery, port and passphrase.", Control::Toml),
];

#[derive(Clone, Copy, PartialEq, Eq)]
enum Hit {
    Page(Option<usize>),
    Card(usize),
    Search,
    Field(usize),
    Reset(usize),
    ResetSection,
    Discard,
    Apply,
    Close,
    Option(usize, usize),
    EditDocument,
    HomeConnect,
    HomeDisconnect,
    HomeImport,
    HomeCloud(bool),
    Reference,
}

#[derive(Default)]
pub struct Settings {
    pub open: bool,
    page: Option<usize>,
    query: String,
    before: Vec<Option<toml::Value>>,
    draft: Vec<Option<toml::Value>>,
    defaults: Vec<Option<toml::Value>>,
    scroll: f64,
    focus: Option<Hit>,
    editing: Option<usize>,
    drag: Option<(usize, Rect)>,
    input: crate::text_input::TextInput,
    message: String,
    error: bool,
    dropdown: Option<(usize, usize)>,
    document_before: String,
    document_draft: String,
    editor: Option<editor::Editor>,
    parked_editors: Vec<editor::Editor>,
    editor_epoch: u64,
    config_scroll: f64,
    home_cloud_labels: [String; 2],
    needs_reload: bool,
    connect: Option<connect::Connect>,
}

fn values(file: &ConfigFile) -> Result<Vec<Option<toml::Value>>, String> {
    let table = toml::Table::try_from(file).map_err(|error| error.to_string())?;
    Ok(FIELDS
        .iter()
        .map(|field| match field.key.split_once('.') {
            Some((section, key)) => table.get(section).and_then(|section| section.get(key)).cloned(),
            None => table.get(field.key).cloned(),
        })
        .collect())
}

fn show(value: Option<&toml::Value>) -> String {
    match value {
        Some(toml::Value::String(text)) => text.clone(),
        Some(toml::Value::Float(number)) => number.to_string(),
        Some(value) => value.to_string(),
        None => "System default".into(),
    }
}

struct Frame {
    panel: Rect,
    body: Rect,
    content: Rect,
    preview: Option<Rect>,
    hits: Vec<(Rect, Hit)>,
    rows: Vec<(Rect, usize)>,
    cards: Vec<(Rect, usize)>,
    config: Option<Rect>,
    group: Option<Rect>,
    max_scroll: f64,
}

fn viewport(frame: &Frame) -> Rect {
    let offset = if frame.config.is_some() && frame.body.h < 500. { 88. } else { 100. };
    Rect::new(
        frame.content.x - 1.,
        frame.content.y + offset,
        frame.content.w + 2.,
        (frame.config.map_or(frame.body.bottom() - 8., |rect| rect.y - 12.) - frame.content.y - offset).max(1.),
    )
}

fn intersect(left: Rect, right: Rect) -> Option<Rect> {
    let start_x = left.x.max(right.x);
    let start_y = left.y.max(right.y);
    let end_x = left.right().min(right.right());
    let end_y = left.bottom().min(right.bottom());
    (end_x > start_x && end_y > start_y).then(|| Rect::new(start_x, start_y, end_x - start_x, end_y - start_y))
}

fn control_rect(row: Rect) -> Rect {
    Rect::new(row.right() - 194., row.y + 18., 150., 38.)
}
fn reset_rect(row: Rect) -> Rect {
    Rect::new(row.right() - 36., row.y + 21., 28., 32.)
}

fn custom_command(index: usize) -> bool {
    matches!(
        FIELDS[index].key,
        "shell.bar" | "shell.launcher" | "shell.notifications" | "shell.lock" | "shell.power_menu"
    )
}

fn config_line_keys(lines: &[String]) -> Vec<String> {
    let mut section = String::new();
    let mut counts = std::collections::BTreeMap::<String, usize>::new();
    lines
        .iter()
        .map(|line| {
            let text = line.trim();
            if text.starts_with('[') {
                let count = counts.entry(text.into()).or_default();
                *count += 1;
                section = format!("{text}:{}", count);
                return section.clone();
            }
            let key = text.split_once('=').map_or(text, |(key, _)| key.trim());
            format!("{section}/{key}")
        })
        .collect()
}

impl Settings {
    fn options(&self, index: usize) -> Vec<String> {
        let Control::Choice(choices) = FIELDS[index].control else { return Vec::new() };
        let mut options: Vec<String> = choices.iter().map(|choice| (*choice).into()).collect();
        if FIELDS[index].key == "touchpad.click_method" {
            options.insert(0, "Device default".into());
        }
        if let Some(original) = self.before[index].as_ref().and_then(toml::Value::as_str)
            && !options.iter().any(|choice| choice.eq_ignore_ascii_case(original))
        {
            options.insert(0, original.into());
        }
        if custom_command(index) {
            options.push("Custom command...".into());
        }
        options
    }
    fn changed(&self) -> usize {
        let count = self.draft.iter().zip(&self.before).filter(|(draft, before)| draft != before).count();
        count.max(usize::from(
            self.document_draft != self.document_before || !self.parked_editors.is_empty() || self.needs_reload,
        ))
    }

    fn selected(&self) -> Vec<usize> {
        let query = self.query.to_lowercase();
        FIELDS
            .iter()
            .enumerate()
            .filter(|(_, field)| {
                if query.is_empty() {
                    self.page == Some(field.category)
                } else {
                    format!("{} {} {}", field.label, field.description, field.key).to_lowercase().contains(&query)
                }
            })
            .map(|(index, _)| index)
            .collect()
    }

    fn number(&self, key: &str, fallback: f64) -> f64 {
        self.value(key)
            .and_then(|value| value.as_float().or_else(|| value.as_integer().map(|value| value as f64)))
            .unwrap_or(match key {
                "keyboard.repeat_delay" => 400.,
                "keyboard.repeat_rate" => 30.,
                _ => fallback,
            })
    }

    fn value(&self, key: &str) -> Option<&toml::Value> {
        if let Some(index) = FIELDS.iter().position(|field| field.key == key) {
            return self.draft[index].as_ref();
        }
        let (section, name) = key.split_once('.')?;
        let index = FIELDS.iter().position(|field| field.key == section)?;
        self.draft[index].as_ref()?.get(name)
    }

    fn text(&self, key: &str) -> &str {
        self.value(key).and_then(toml::Value::as_str).unwrap_or("")
    }

    fn flag(&self, key: &str) -> bool {
        self.value(key).and_then(toml::Value::as_bool).unwrap_or(false)
    }
}

impl Tileception {
    fn settings_load(&mut self) -> Result<(), String> {
        let text = crate::config_edit::panel_text()?;
        let config = crate::config::Config::parse(&text)?;
        let before = values(&config.file)?;
        let defaults = values(&ConfigFile::default())?;
        self.settings.draft = before.clone();
        self.settings.before = before;
        self.settings.defaults = defaults;
        self.settings.document_before = text.clone();
        self.settings.document_draft = text;
        self.settings.home_cloud_labels = [self.settings_home_cloud_label(true), self.settings_home_cloud_label(false)];
        Ok(())
    }

    fn settings_document(&self) -> Result<String, String> {
        let config = crate::config::Config::parse(&self.settings.document_draft)?;
        let current = values(&config.file)?;
        let changes: Vec<_> = FIELDS
            .iter()
            .enumerate()
            .filter(|(index, _)| current[*index] != self.settings.draft[*index])
            .map(|(index, field)| (field.key, self.settings.draft[index].clone()))
            .collect();
        crate::config_edit::draft_text(&self.settings.document_draft, &changes)
    }

    fn settings_after_save(&mut self) -> Result<(), String> {
        // Advance the saved snapshot even if applying it to the session fails.
        self.settings_load()?;
        self.settings.needs_reload = true;
        self.reload_config()
            .map_err(|error| format!("Config saved, but could not reload: {error}. Apply to retry."))?;
        self.settings.needs_reload = false;
        self.settings_load()
    }

    fn settings_dropdown_rects(&self, frame: &Frame, index: usize) -> Vec<Rect> {
        let Some((row, _)) = frame.rows.iter().find(|(_, field)| *field == index) else { return Vec::new() };
        let control = control_rect(*row);
        let count = self.settings.options(index).len();
        let top = (control.bottom() + 4.).min(frame.panel.bottom() - 70. - count as f64 * 34.).max(frame.panel.y + 70.);
        (0..count).map(|position| Rect::new(control.x, top + position as f64 * 34., 180., 34.)).collect()
    }

    pub fn toggle_settings(&mut self) {
        if self.is_locked() {
            return;
        }
        if self.settings.open {
            // Keep the draft when closed, so Escape never silently loses work.
            self.settings.open = false;
            self.settings.drag = None;
            self.settings.focus = None;
        } else {
            if (self.settings.before.is_empty() || self.settings.changed() == 0)
                && self.settings.editor.is_none()
                && self.settings.editing.is_none()
                && self.settings.parked_editors.is_empty()
                && self.settings.connect.is_none()
                && let Err(error) = self.settings_load()
            {
                tracing::warn!("could not open Settings: {error}");
                return;
            }
            self.settings.open = true;
            self.power_menu = None;
            self.close_tray();
            self.close_launcher();
            self.close_doctor();
            self.notifications.open = false;
            if !self.settings.error {
                self.settings.message.clear();
            }
        }
        self.force_keyboard_focus_sync();
        self.schedule_render();
    }

    fn settings_frame(&self) -> Frame {
        let screen = self.output_rect();
        let width = (screen.w - 32.).clamp(320., 1420.).min(screen.w);
        let height = (screen.h - 64.).max(240.).min(screen.h);
        let panel = Rect::new(screen.x + (screen.w - width) / 2., screen.y + (screen.h - height) / 2., width, height);
        let sidebar = if width < 900. { 64. } else { 205. };
        let body = Rect::new(panel.x + sidebar, panel.y + 65., width - sidebar, height - 125.);
        let content = Rect::new(body.x + 28., body.y + 26., (body.w - 56.).max(1.), body.h - 52.);
        let search = Rect::new(panel.right() - 340., panel.y + 15., 282., 35.);
        let mut hits =
            vec![(search, Hit::Search), (Rect::new(panel.right() - 48., panel.y + 15., 34., 35.), Hit::Close)];
        hits.push((Rect::new(panel.x + 12., body.y + 20., sidebar - 24., 40.), Hit::Page(None)));
        let nav_pitch = ((body.h - 100.) / CATEGORIES.len() as f64).clamp(26., 46.);
        for index in 0..CATEGORIES.len() {
            hits.push((
                Rect::new(panel.x + 12., body.y + 86. + index as f64 * nav_pitch, sidebar - 24., nav_pitch - 4.),
                Hit::Page(Some(index)),
            ));
        }
        hits.push((Rect::new(panel.right() - 300., panel.bottom() - 46., 134., 32.), Hit::Discard));
        hits.push((Rect::new(panel.right() - 154., panel.bottom() - 46., 130., 32.), Hit::Apply));
        let mut frame = Frame {
            panel,
            body,
            content,
            preview: None,
            hits,
            rows: Vec::new(),
            cards: Vec::new(),
            config: None,
            group: None,
            max_scroll: 0.,
        };
        if self.settings.page.is_none() && self.settings.query.is_empty() {
            let gap = 12.;
            let col = (content.w - gap * 2.) / 3.;
            let top = content.y + 100.;
            let row = ((content.h - 116.) / 3.).clamp(140., 180.);
            let mut rects = [
                Rect::new(content.x, top, col, row * 2. + gap),
                Rect::new(content.x + col + gap, top, col, row),
                Rect::new(content.x + 2. * (col + gap), top, col, row),
                Rect::new(content.x + col + gap, top + row + gap, col * 2. + gap, row),
                Rect::new(content.x, top + 2. * (row + gap), col, (row * 0.8).max(140.)),
                Rect::new(content.x + col + gap, top + 2. * (row + gap), col * 2. + gap, (row * 0.8).max(140.)),
            ];
            if content.w < 700. || content.h < 500. {
                let half = (content.w - gap) / 2.;
                rects = [
                    Rect::new(content.x, top, content.w, 156.),
                    Rect::new(content.x, top + 168., half, 140.),
                    Rect::new(content.x + half + gap, top + 168., half, 140.),
                    Rect::new(content.x, top + 320., half, 140.),
                    Rect::new(content.x + half + gap, top + 320., half, 140.),
                    Rect::new(content.x, top + 472., content.w, 140.),
                ];
            }
            let mut cards = rects.to_vec();
            let bottom = rects[5].bottom() + 12.;
            for index in 6..CATEGORIES.len() {
                cards.push(Rect::new(content.x, bottom + (index - 6) as f64 * 152., content.w, 140.));
            }
            frame.max_scroll = (cards.last().map_or(bottom, Rect::bottom) - body.bottom() + 20.).max(0.);
            for (index, mut rect) in cards.into_iter().enumerate() {
                rect.y -= self.settings.scroll.min(frame.max_scroll);
                if rect.bottom() > content.y + 100. && rect.y < body.bottom() - 8. {
                    frame.cards.push((rect, index));
                    let clipped = intersect(viewport(&frame), rect);
                    if let Some(clipped) = clipped {
                        frame.hits.push((clipped, Hit::Card(index)));
                    }
                }
            }
        } else {
            let selected = self.settings.selected();
            let show_preview = body.w >= 860.
                && body.h >= 530.
                && self.settings.page.is_some_and(|page| page <= 2)
                && self.settings.query.is_empty();
            let row_width = if show_preview { content.w - 290. } else { content.w };
            if show_preview {
                frame.preview = Some(Rect::new(content.right() - 260., content.y + 108., 260., 330.));
            }
            frame.hits.push((Rect::new(content.right() - 112., content.y, 112., 30.), Hit::ResetSection));
            let lines = selected.len().min(if body.h < 650. { 2 } else { 5 });
            let config_height = if body.h < 500. { 36. } else { 109. + lines as f64 * 23. };
            let rows_top = content.y + if body.h < 500. { 90. } else { 110. };
            if !selected.is_empty() {
                frame.config =
                    Some(Rect::new(content.x, body.bottom() - config_height - 12., content.w, config_height));
            }
            let total = selected.len() as f64 * 78. + if self.settings.page == Some(5) { 130. } else { 0. };
            frame.max_scroll = (total - viewport(&frame).h + 20.).max(0.);
            let scroll = self.settings.scroll.min(frame.max_scroll);
            if !selected.is_empty() {
                frame.group = Some(Rect::new(content.x, rows_top - scroll, row_width, selected.len() as f64 * 78.));
            }
            for (position, index) in selected.iter().enumerate() {
                let rect = Rect::new(content.x, rows_top + position as f64 * 78. - scroll, row_width, 78.);
                if rect.bottom() > viewport(&frame).y && rect.y < viewport(&frame).bottom() {
                    frame.rows.push((rect, *index));
                    let mut control = control_rect(rect);
                    if matches!(FIELDS[*index].control, Control::Number(..)) {
                        control.w = 90.;
                    }
                    if control.y >= viewport(&frame).y && control.bottom() <= viewport(&frame).bottom() {
                        frame.hits.push((control, Hit::Field(*index)));
                        if self.settings.draft[*index] != self.settings.defaults[*index] {
                            frame.hits.push((reset_rect(rect), Hit::Reset(*index)));
                        }
                    }
                }
            }
            if self.settings.page == Some(5) {
                let top = rows_top + 34. + total - 130. - scroll;
                for (position, hit) in [
                    if self.settings_home_connected() { Hit::HomeDisconnect } else { Hit::HomeConnect },
                    Hit::HomeImport,
                    Hit::HomeCloud(true),
                    Hit::HomeCloud(false),
                ]
                .into_iter()
                .enumerate()
                {
                    let rect = Rect::new(
                        content.x + (position % 2) as f64 * 200.,
                        top + (position / 2) as f64 * 44.,
                        188.,
                        34.,
                    );
                    if rect.y >= viewport(&frame).y && rect.bottom() <= viewport(&frame).bottom() {
                        frame.hits.push((rect, hit));
                    }
                }
            }
        }
        if let Some(config) = frame.config {
            frame.hits.push((Rect::new(config.right() - 206., config.y + 4., 84., 28.), Hit::Reference));
            frame.hits.push((Rect::new(config.right() - 112., config.y + 4., 100., 28.), Hit::EditDocument));
        }
        if let Some((index, _)) = self.settings.dropdown {
            for (position, rect) in self.settings_dropdown_rects(&frame, index).into_iter().enumerate() {
                frame.hits.push((rect, Hit::Option(index, position)));
            }
        }
        frame
    }

    pub fn settings_scroll(&mut self, amount: f64) {
        if self.settings.connect.is_some() {
            self.settings_connect_scroll(amount);
            return;
        }
        if self.settings.editor.is_some() {
            self.settings_editor_scroll(amount);
            return;
        }
        self.settings.dropdown = None;
        self.settings.drag = None;
        let frame = self.settings_frame();
        if let Some(config) = frame.config
            && config.h > 70.
            && config.contains(Point::new(self.pointer.x, self.pointer.y))
        {
            let count = self.settings_config_lines(true).len();
            let shown = config.h - 78.;
            let max = (count as f64 * 23. - shown).max(0.);
            self.settings.config_scroll = (self.settings.config_scroll + amount).clamp(0., max);
            self.schedule_render();
            return;
        }
        self.settings.scroll = (self.settings.scroll + amount).clamp(0., frame.max_scroll);
        self.schedule_render();
    }

    pub fn settings_click(&mut self, point: Point) {
        if self.settings.connect.is_some() {
            self.settings_connect_click(point);
            return;
        }
        if self.settings.editor.is_some() {
            self.settings_editor_click(point);
            return;
        }
        let frame = self.settings_frame();
        if let Some((rect, hit)) = frame.hits.iter().rev().find(|(rect, _)| rect.contains(point)) {
            if let Hit::Field(index) = hit
                && self.settings.editing == Some(*index)
            {
                let inset = if matches!(FIELDS[*index].control, Control::Colour) { 34. } else { 10. };
                let cursor = self.text.borrow_mut().field_hit(
                    &self.settings.input.text,
                    &self.theme.font,
                    11.,
                    (rect.w - inset - 8.) as f32,
                    self.settings.input.cursor,
                    (point.x - rect.x - inset).max(0.) as f32,
                );
                self.settings.input.move_to(cursor, false);
                self.schedule_render();
                return;
            }
            if let Some((index, _)) = self.settings.dropdown
                && !matches!(hit, Hit::Option(..))
            {
                self.settings.dropdown = None;
                if *hit == Hit::Field(index) {
                    self.schedule_render();
                    return;
                }
            }
            self.settings_activate(*hit, Some((point.x - rect.x) / rect.w));
            if let Hit::Field(index) = hit
                && matches!(FIELDS[*index].control, Control::Number(..))
                && self.settings.editing.is_none()
            {
                self.settings.drag = Some((*index, *rect));
            }
        } else {
            self.settings.dropdown = None;
            self.schedule_render();
        }
    }

    pub fn settings_motion(&mut self, point: Point) {
        if let Some((index, rect)) = self.settings.drag {
            self.settings_activate(Hit::Field(index), Some((point.x - rect.x) / rect.w));
        }
    }

    pub fn settings_release(&mut self) {
        self.settings.drag = None;
    }

    fn settings_finish_edit(&mut self) -> bool {
        let Some(index) = self.settings.editing else { return true };
        let text = self.settings.input.text.trim();
        if matches!(FIELDS[index].control, Control::Colour) && Colour::parse(text).is_err() {
            self.settings.message = "Enter a colour as #rrggbb or #rrggbbaa, or press Escape to cancel.".into();
            self.settings.error = true;
            return false;
        }
        self.settings.draft[index] = if matches!(FIELDS[index].control, Control::Text) && text.is_empty() {
            None
        } else {
            Some(toml::Value::String(text.into()))
        };
        self.settings.editing = None;
        self.settings.editor_epoch = self.settings.editor_epoch.wrapping_add(1);
        self.settings.error = false;
        true
    }

    fn settings_activate(&mut self, hit: Hit, position: Option<f64>) {
        if hit == Hit::Discard {
            self.settings.editing = None;
        } else if !self.settings_finish_edit() {
            return;
        }
        self.settings.focus = Some(hit);
        if !matches!(hit, Hit::Field(_) | Hit::Option(..)) {
            self.settings.dropdown = None;
        }
        match hit {
            Hit::Close => self.toggle_settings(),
            Hit::Search => {}
            Hit::EditDocument => self.open_settings_editor(None, false),
            Hit::Reference => self.open_settings_editor(None, true),
            Hit::HomeConnect => {
                if self.config.file.home.enabled {
                    self.apply_home();
                    self.settings.message = self.settings_home_status();
                    self.settings.error = false;
                } else {
                    self.settings.message = "Enable smart home and apply first.".into();
                }
            }
            Hit::HomeDisconnect => {
                self.stop_home();
                self.settings.error = false;
                self.settings.message = "Disconnected. Saved devices and credentials are kept.".into();
            }
            Hit::HomeImport => self.open_settings_connect(),
            Hit::HomeCloud(sensors) => match self.settings_home_cloud(sensors) {
                Ok(()) => {
                    self.settings.message = self.settings_home_cloud_label(sensors);
                    self.settings.home_cloud_labels =
                        [self.settings_home_cloud_label(true), self.settings_home_cloud_label(false)];
                }
                Err(error) => {
                    self.settings.message = error;
                    self.settings.error = true;
                }
            },
            Hit::Option(index, position) => {
                let options = self.settings.options(index);
                if let Some(option) = options.get(position) {
                    if custom_command(index) && position + 1 == options.len() {
                        let text =
                            self.settings.draft[index].as_ref().and_then(toml::Value::as_str).unwrap_or("").to_string();
                        self.settings.input = crate::text_input::TextInput { cursor: text.len(), text, anchor: None };
                        self.settings.editor_epoch = self.settings.editor_epoch.wrapping_add(1);
                        self.settings.editing = Some(index);
                    } else if FIELDS[index].key == "touchpad.click_method" && position == 0 {
                        self.settings.draft[index] = None;
                    } else {
                        self.settings.draft[index] = Some(toml::Value::String(option.clone()));
                    }
                }
                self.settings.dropdown = None;
                self.settings.focus = Some(Hit::Field(index));
            }
            Hit::Page(_) | Hit::Card(_) => {
                self.settings.page = match hit {
                    Hit::Page(page) => page,
                    Hit::Card(index) => Some(index),
                    _ => None,
                };
                self.settings.query.clear();
                self.settings.scroll = 0.;
                self.settings.config_scroll = 0.;
            }
            Hit::Discard => {
                self.settings.parked_editors.clear();
                match self.settings_load() {
                    Ok(()) => {
                        self.settings.message = "Draft discarded. Loaded current desktop settings.".into();
                        self.settings.error = false;
                        self.settings.focus = None;
                    }
                    Err(error) => {
                        self.settings.message = error;
                        self.settings.error = true;
                    }
                }
            }
            Hit::Apply => {
                if !self.settings.parked_editors.is_empty() {
                    self.settings.editor = Some(self.settings.parked_editors.remove(0));
                    self.settings.editor_epoch = self.settings.editor_epoch.wrapping_add(1);
                    self.settings.message = "Finish the saved editor draft before applying.".into();
                    self.schedule_render();
                    return;
                }
                let changes: Vec<_> = FIELDS
                    .iter()
                    .enumerate()
                    .filter(|(index, _)| self.settings.draft[*index] != self.settings.before[*index])
                    .map(|(index, field)| {
                        (field.key, self.settings.before[index].clone(), self.settings.draft[index].clone())
                    })
                    .collect();
                if !changes.is_empty() {
                    match if self.settings.document_draft != self.settings.document_before {
                        self.settings_document()
                            .and_then(|text| crate::config_edit::apply_document(&self.settings.document_before, &text))
                    } else {
                        crate::config_edit::apply_draft(&changes)
                    }
                    .and_then(|()| self.settings_after_save())
                    {
                        Ok(()) => {
                            self.settings.message = "Settings applied to your desktop.".into();
                            self.settings.error = false;
                            self.settings.focus = None;
                        }
                        Err(error) => {
                            self.settings.message = error;
                            self.settings.error = true;
                        }
                    }
                } else if self.settings.document_draft != self.settings.document_before {
                    match self
                        .settings_document()
                        .and_then(|text| crate::config_edit::apply_document(&self.settings.document_before, &text))
                        .and_then(|()| self.settings_after_save())
                    {
                        Ok(()) => {
                            self.settings.message = "Settings applied.".into();
                            self.settings.error = false;
                        }
                        Err(error) => {
                            self.settings.message = error;
                            self.settings.error = true;
                        }
                    }
                } else if self.settings.needs_reload {
                    match self.settings_after_save() {
                        Ok(()) => {
                            self.settings.message = "Settings applied.".into();
                            self.settings.error = false;
                        }
                        Err(error) => {
                            self.settings.message = error;
                            self.settings.error = true;
                        }
                    }
                }
            }
            Hit::Reset(_) | Hit::ResetSection => {
                let indices = if let Hit::Reset(index) = hit { vec![index] } else { self.settings.selected() };
                let changes: Vec<_> = indices.into_iter().map(|index| (FIELDS[index].key, None)).collect();
                let result = self
                    .settings_document()
                    .and_then(|document| crate::config_edit::draft_text(&document, &changes))
                    .and_then(|document| {
                        let config = crate::config::Config::parse(&document)?;
                        Ok((values(&config.file)?, document))
                    });
                match result {
                    Ok((draft, document)) => {
                        self.settings.draft = draft;
                        self.settings.document_draft = document;
                    }
                    Err(error) => {
                        self.settings.message = error;
                        self.settings.error = true;
                    }
                }
            }
            Hit::Field(index) => {
                let value = self.settings.draft[index].as_ref();
                let next = match FIELDS[index].control {
                    Control::Toggle => toml::Value::Boolean(!value.and_then(toml::Value::as_bool).unwrap_or(false)),
                    Control::Choice(_) => {
                        let options = self.settings.options(index);
                        let current = value.and_then(toml::Value::as_str).unwrap_or("");
                        let selected =
                            options.iter().position(|option| option.eq_ignore_ascii_case(current)).unwrap_or(0);
                        self.settings.dropdown = Some((index, selected));
                        self.schedule_render();
                        return;
                    }
                    Control::Toml => {
                        self.open_settings_editor(Some(index), false);
                        return;
                    }
                    Control::Number(min, max, step, _) => {
                        let current = self.settings.number(FIELDS[index].key, min);
                        let min = min.min(current);
                        let max = max.max(current);
                        let number =
                            position.map_or(current + step, |fraction| min + fraction.clamp(0., 1.) * (max - min));
                        let number = ((number / step).round() * step).clamp(min, max);
                        if matches!(value, Some(toml::Value::Integer(_))) || FIELDS[index].key.starts_with("keyboard.")
                        {
                            toml::Value::Integer(number.round() as i64)
                        } else {
                            toml::Value::Float((number * 1000.).round() / 1000.)
                        }
                    }
                    Control::Text | Control::Colour => {
                        let text = value.and_then(toml::Value::as_str).unwrap_or("").to_string();
                        self.settings.input = crate::text_input::TextInput { cursor: text.len(), text, anchor: None };
                        self.settings.editor_epoch = self.settings.editor_epoch.wrapping_add(1);
                        self.settings.editing = Some(index);
                        self.schedule_render();
                        return;
                    }
                };
                self.settings.draft[index] = Some(next);
            }
        }
        self.schedule_render();
    }

    pub fn settings_key(&mut self, sym: u32, mods: Mods) {
        if self.settings.connect.is_some() {
            self.settings_connect_key(sym, mods);
            return;
        }
        if self.settings.editor.is_some() {
            self.settings_editor_key(sym, mods);
            return;
        }
        if let Some((index, selected)) = self.settings.dropdown {
            let count = self.settings.options(index).len();
            match sym {
                keysyms::KEY_Escape | keysyms::KEY_Tab => self.settings.dropdown = None,
                keysyms::KEY_Up => self.settings.dropdown = Some((index, (selected + count - 1) % count)),
                keysyms::KEY_Down => self.settings.dropdown = Some((index, (selected + 1) % count)),
                keysyms::KEY_Return | keysyms::KEY_space => self.settings_activate(Hit::Option(index, selected), None),
                _ => {}
            }
            self.schedule_render();
            return;
        }
        if sym == keysyms::KEY_Escape {
            if self.settings.editing.take().is_some() {
                self.settings.error = false;
                self.settings.message = "Text edit cancelled. Your draft is unchanged.".into();
            } else if !self.settings.query.is_empty() {
                self.settings.query.clear();
                self.settings.scroll = 0.;
            } else {
                self.toggle_settings();
            }
        } else if mods.ctrl && matches!(sym, keysyms::KEY_f | keysyms::KEY_F) {
            if self.settings_finish_edit() {
                self.settings.focus = Some(Hit::Search);
            }
        } else if mods.ctrl && matches!(sym, keysyms::KEY_s | keysyms::KEY_S) {
            self.settings_activate(Hit::Apply, None);
        } else if mods.ctrl && matches!(sym, keysyms::KEY_v | keysyms::KEY_V) {
            self.paste(crate::launcher::Paste::Settings(self.settings.editor_epoch));
        } else if matches!(sym, keysyms::KEY_Page_Down | keysyms::KEY_Page_Up) {
            self.settings_scroll(if sym == keysyms::KEY_Page_Down { 162. } else { -162. });
        } else if sym == keysyms::KEY_Tab || sym == keysyms::KEY_ISO_Left_Tab {
            if self.settings_finish_edit() {
                let hits: Vec<_> = self
                    .settings_frame()
                    .hits
                    .into_iter()
                    .filter(|(_, hit)| !matches!(hit, Hit::Apply | Hit::Discard) || self.settings.changed() > 0)
                    .collect();
                let current = hits.iter().position(|(_, hit)| Some(*hit) == self.settings.focus);
                let next = if mods.shift || sym == keysyms::KEY_ISO_Left_Tab {
                    current.map_or(hits.len() - 1, |index| (index + hits.len() - 1) % hits.len())
                } else {
                    current.map_or(0, |index| (index + 1) % hits.len())
                };
                self.settings.focus = Some(hits[next].1);
            }
        } else if self.settings.editing.is_some() || self.settings.focus == Some(Hit::Search) {
            if sym == keysyms::KEY_Return {
                self.settings_finish_edit();
            } else if self.settings.editing.is_some() {
                if mods.ctrl && matches!(sym, keysyms::KEY_c | keysyms::KEY_C | keysyms::KEY_x | keysyms::KEY_X) {
                    let text = self.settings.input.text[self.settings.input.selection()].to_string();
                    self.copy_text(text);
                    if matches!(sym, keysyms::KEY_x | keysyms::KEY_X) {
                        self.settings.input.delete(false, false);
                    }
                } else {
                    let input = &mut self.settings.input;
                    match sym {
                        keysyms::KEY_a | keysyms::KEY_A if mods.ctrl => input.select_all(),
                        keysyms::KEY_Left | keysyms::KEY_Right => {
                            input.move_horizontal(sym == keysyms::KEY_Right, mods.ctrl, mods.shift)
                        }
                        keysyms::KEY_Home | keysyms::KEY_End => {
                            input.move_to(if sym == keysyms::KEY_Home { 0 } else { input.text.len() }, mods.shift)
                        }
                        keysyms::KEY_BackSpace | keysyms::KEY_Delete => {
                            input.delete(sym == keysyms::KEY_Delete, mods.ctrl)
                        }
                        _ if !mods.ctrl && !mods.alt && !mods.logo => {
                            if let Some(character) = char::from_u32(xkb::keysym_to_utf32(sym.into()))
                                .filter(|character| !character.is_control())
                            {
                                input.insert(&character.to_string());
                            }
                        }
                        _ => {}
                    }
                }
            } else {
                let target = &mut self.settings.query;
                if sym == keysyms::KEY_BackSpace {
                    target.pop();
                } else if mods.ctrl && matches!(sym, keysyms::KEY_a | keysyms::KEY_A) {
                    target.clear();
                } else if !mods.ctrl
                    && !mods.alt
                    && !mods.logo
                    && let Some(character) =
                        char::from_u32(xkb::keysym_to_utf32(sym.into())).filter(|character| !character.is_control())
                    && target.len() < 128
                {
                    target.push(character);
                }
                self.settings.scroll = 0.;
            }
        } else if matches!(sym, keysyms::KEY_Return | keysyms::KEY_space) {
            if let Some(hit) = self.settings.focus {
                self.settings_activate(hit, None);
            }
        } else if matches!(sym, keysyms::KEY_Left | keysyms::KEY_Right) {
            if let Some(Hit::Field(index)) = self.settings.focus {
                if let Control::Number(min, max, step, _) = FIELDS[index].control {
                    let current = self.settings.number(FIELDS[index].key, min);
                    let next = current + if sym == keysyms::KEY_Left { -step } else { step };
                    let number = if current < min || current > max { next } else { next.clamp(min, max) };
                    self.settings.draft[index] = Some(
                        if FIELDS[index].key.starts_with("keyboard.")
                            || matches!(self.settings.draft[index], Some(toml::Value::Integer(_)))
                        {
                            toml::Value::Integer(number.round() as i64)
                        } else {
                            toml::Value::Float((number * 1000.).round() / 1000.)
                        },
                    );
                } else {
                    self.settings_activate(Hit::Field(index), None);
                }
            }
        } else if matches!(sym, keysyms::KEY_Down | keysyms::KEY_Page_Down | keysyms::KEY_Up | keysyms::KEY_Page_Up) {
            self.settings_scroll(if matches!(sym, keysyms::KEY_Down | keysyms::KEY_Page_Down) { 54. } else { -54. });
        }
        self.schedule_render();
    }

    pub fn settings_elements(&self, renderer: &mut GlesRenderer) -> Vec<OutputElement> {
        if !self.settings.open || !self.is_active_output() {
            return Vec::new();
        }
        let frame = self.settings_frame();
        let theme = &self.theme;
        let state = &self.settings;
        let pointer = Point::new(self.pointer.x, self.pointer.y);
        let mut painter = Painter::new(self, renderer);
        painter.fill(self.output_rect(), Colour([0., 0., 0., 0.55]), 0.);
        painter.shape(
            frame.panel,
            Shape { border_width: 1., border: theme.border, ..Shape::filled(theme.background, 10.) },
        );
        painter.fill(Rect::new(frame.panel.x + 1., frame.panel.y + 1., frame.panel.w - 2., 63.), theme.surface, 9.);
        let title = Rect::new(frame.panel.x + 24., frame.panel.y + 15., 24., 34.);
        painter.glyph(glyph::MINIMAP, title, 22, theme.accent);
        painter.text(
            "Settings",
            Rect::new(title.right() + 12., title.y, 140., title.h),
            Style::new(15., theme.text).bold(),
        );
        painter.fill(Rect::new(frame.panel.x, frame.body.y, frame.panel.w, 1.), theme.border, 0.);
        painter.fill(Rect::new(frame.body.x, frame.body.y, 1., frame.body.h), theme.border, 0.);
        painter.fill(Rect::new(frame.panel.x, frame.panel.bottom() - 60., frame.panel.w, 1.), theme.border, 0.);
        let heading = state.page.map_or("Your workspace.", |index| CATEGORIES[index].title);
        painter.text(
            if state.query.is_empty() { heading } else { "Search results" },
            Rect::new(frame.content.x, frame.content.y, frame.content.w - 125., 38.),
            Style::new(27., theme.text).weight(500),
        );
        let subtitle = if state.page == Some(5) {
            self.settings_home_status()
        } else if !state.query.is_empty() {
            "Matching settings across your desktop.".into()
        } else {
            state.page.map_or("Desktop settings", |index| CATEGORIES[index].description).into()
        };
        {
            painter.text(
                &subtitle,
                Rect::new(
                    frame.content.x,
                    frame.content.y + if frame.body.h < 500. { 40. } else { 44. },
                    frame.content.w,
                    if frame.body.h < 500. { 22. } else { 35. },
                ),
                Style::new(12., theme.muted).lines(if frame.body.h < 500. { 1 } else { 2 }),
            );
        }
        painter.fill(
            Rect::new(frame.panel.x + 24., frame.body.y + 73., frame.body.x - frame.panel.x - 48., 1.),
            theme.border,
            0.,
        );
        for (rect, hit) in &frame.hits {
            let focused = state.focus == Some(*hit);
            let hovered = rect.contains(pointer);
            match *hit {
                Hit::Page(page) if rect.x < frame.body.x => {
                    let active = state.page == page && state.query.is_empty();
                    if active {
                        painter.shape(
                            *rect,
                            Shape {
                                fill: (theme.accent.alpha(0.18), theme.accent_end.alpha(0.07)),
                                radius: 6.,
                                ..Default::default()
                            },
                        );
                    } else if hovered {
                        painter.fill(*rect, theme.surface, 6.);
                    }
                    let (label, icon) = page.map_or(("Overview", glyph::MINIMAP), |index| {
                        (CATEGORIES[index].title, CATEGORIES[index].icon)
                    });
                    let colour = if active { theme.accent } else { theme.muted };
                    painter.glyph(icon, Rect::new(rect.x + 10., rect.y, 20., rect.h), 18, colour);
                    if rect.w > 60. {
                        painter.text(
                            label,
                            Rect::new(rect.x + 40., rect.y, rect.w - 44., rect.h),
                            Style::new(12., colour),
                        );
                    }
                }
                Hit::Search => {
                    painter.shape(
                        *rect,
                        Shape {
                            border_width: 1.,
                            border: if focused { theme.accent } else { theme.border },
                            ..Shape::filled(theme.background, 6.)
                        },
                    );
                    painter.glyph(glyph::SEARCH, Rect::new(rect.x + 10., rect.y, 18., rect.h), 16, theme.muted);
                    let label = if state.query.is_empty() { "Search settings" } else { &state.query };
                    let (text_width, _) = painter.text(
                        label,
                        Rect::new(rect.x + 35., rect.y, rect.w - 45., rect.h),
                        Style::new(12., if state.query.is_empty() { theme.muted } else { theme.text }),
                    );
                    if focused {
                        let left = rect.x + 35. + if state.query.is_empty() { 0. } else { text_width };
                        painter.fill(Rect::new(left.min(rect.right() - 9.), rect.y + 10., 1., 15.), theme.text, 0.);
                    }
                }
                Hit::Close => painter.glyph(glyph::CLOSE, *rect, 18, theme.muted),
                Hit::HomeCloud(sensors) => {
                    let label = &state.home_cloud_labels[usize::from(!sensors)];
                    let enabled = label.ends_with(": on");
                    painter.text(
                        label.split(':').next().unwrap_or(label),
                        Rect::new(rect.x + 10., rect.y, rect.w - 58., rect.h),
                        Style::new(11., theme.text),
                    );
                    let switch = Rect::new(rect.right() - 44., rect.y + 7., 34., 20.);
                    painter.fill(switch, if enabled { theme.accent } else { theme.surface_raised }, 10.);
                    painter.fill(
                        Rect::new(switch.x + if enabled { 17. } else { 3. }, switch.y + 3., 14., 14.),
                        theme.text,
                        7.,
                    );
                }
                Hit::EditDocument | Hit::Reference | Hit::HomeConnect | Hit::HomeDisconnect | Hit::HomeImport => {
                    let label = match hit {
                        Hit::EditDocument => "Edit config.toml",
                        Hit::Reference => "Configuration reference",
                        Hit::HomeConnect => "Reconnect",
                        Hit::HomeDisconnect => "Disconnect",
                        Hit::HomeImport => "Set up devices...",
                        _ => "",
                    };
                    painter.fill(*rect, theme.surface_raised, 6.);
                    painter.text(label, *rect, Style::new(11., theme.text).align(Align::Centre));
                }
                Hit::Apply | Hit::Discard | Hit::ResetSection => {
                    let enabled = *hit == Hit::ResetSection || state.changed() > 0;
                    let primary = *hit == Hit::Apply && enabled;
                    let fill = if primary {
                        (theme.accent, theme.accent_end)
                    } else {
                        (theme.surface_raised, theme.surface_raised)
                    };
                    painter.shape(
                        *rect,
                        Shape {
                            fill,
                            radius: 6.,
                            border_width: if primary { 0. } else { 1. },
                            border: if hovered { theme.muted } else { theme.border },
                            ..Default::default()
                        },
                    );
                    let label = match hit {
                        Hit::Apply => "Apply changes",
                        Hit::Discard => "Discard changes",
                        _ => "Reset section",
                    };
                    painter.text(
                        label,
                        *rect,
                        Style::new(
                            11.,
                            if primary {
                                theme.on_accent
                            } else if enabled {
                                theme.text
                            } else {
                                theme.muted
                            },
                        )
                        .align(Align::Centre),
                    );
                }
                _ => {}
            }
            if focused
                && !matches!(hit, Hit::Search | Hit::Card(_))
                && (!matches!(hit, Hit::Apply | Hit::Discard) || state.changed() > 0)
            {
                painter
                    .shape(*rect, Shape { radius: 6., border_width: 1., border: theme.accent, ..Default::default() });
            }
        }
        painter.clipped(viewport(&frame), |painter| {
            if state.page == Some(5)
                && let Some((first, _)) =
                    frame.hits.iter().find(|(_, hit)| matches!(hit, Hit::HomeConnect | Hit::HomeDisconnect))
            {
                painter.text(
                    "Connection and cloud changes apply immediately.",
                    Rect::new(first.x, first.y - 26., frame.content.w, 20.),
                    Style::new(11., theme.muted),
                );
            }
            for (rect, index) in &frame.cards {
                let category = &CATEGORIES[*index];
                painter.shape(
                    *rect,
                    Shape {
                        border_width: 1.,
                        border: if rect.contains(pointer) { theme.accent } else { theme.border },
                        ..Shape::filled(theme.surface, 6.)
                    },
                );
                painter.glyph(category.icon, Rect::new(rect.x + 20., rect.y + 20., 25., 25.), 24, theme.accent);
                painter.glyph(
                    glyph::ARROW_RIGHT,
                    Rect::new(rect.right() - 36., rect.y + 16., 20., 24.),
                    16,
                    theme.muted,
                );
                if *index == 0 && rect.h > 250. {
                    self.settings_preview(painter, Rect::new(rect.x + 20., rect.y + 70., rect.w - 40., rect.h - 168.));
                }
                painter.text(
                    category.title,
                    Rect::new(rect.x + 20., rect.bottom() - 80., rect.w - 38., 40.),
                    Style::new(18., theme.text).weight(500).lines(2),
                );
                painter.text(
                    category.summary,
                    Rect::new(rect.x + 20., rect.bottom() - 39., rect.w - 38., 28.),
                    Style::new(11., theme.muted).lines(2),
                );
                if state.focus == Some(Hit::Card(*index)) {
                    painter.shape(
                        *rect,
                        Shape { radius: 6., border_width: 1., border: theme.accent, ..Default::default() },
                    );
                }
            }
            if let Some(group) = frame.group {
                painter
                    .shape(group, Shape { border_width: 1., border: theme.border, ..Shape::filled(theme.surface, 6.) });
            }
            for (rect, index) in &frame.rows {
                self.settings_row(painter, *rect, *index);
            }
        });
        if frame.group.is_some() {
            let label = state.page.map_or("MATCHING SETTINGS", |index| {
                [
                    "LOOK & FEEL",
                    "WINDOWS & MOVEMENT",
                    "DESKTOP TOOLS",
                    "KEYBOARD & TOUCHPAD",
                    "IDLE & BATTERY",
                    "CONNECTION",
                    "LOCK & SESSION",
                    "SHORTCUTS & STARTUP",
                    "DISPLAYS & SHARING",
                ]
                .get(index)
                .copied()
                .unwrap_or("SETTINGS")
            });
            painter.text(
                label,
                Rect::new(
                    frame.content.x,
                    frame.content.y + if frame.body.h < 500. { 64. } else { 80. },
                    frame.content.w,
                    19.,
                ),
                Style::new(10., theme.muted),
            );
        }
        if (state.page.is_some() || !state.query.is_empty()) && state.selected().is_empty() {
            painter.text(
                "No settings match. Try accent, gap or bar.",
                Rect::new(frame.content.x, frame.content.y + 120., frame.content.w, 35.),
                Style::new(13., theme.muted),
            );
        }
        if let Some(rect) = frame.preview {
            painter.text("LIVE PREVIEW", Rect::new(rect.x, rect.y - 28., rect.w, 20.), Style::new(10., theme.muted));
            self.settings_preview(&mut painter, Rect::new(rect.x, rect.y, rect.w, 178.));
        }
        if let Some(rect) = frame.config {
            self.settings_config(&mut painter, rect);
        }
        if frame.max_scroll > 0. {
            let area = viewport(&frame);
            let track = Rect::new(frame.body.right() - 7., area.y, 3., area.h.max(12.));
            painter.fill(track, theme.border, 2.);
            let height = (track.h * track.h / (track.h + frame.max_scroll)).max(22.).min(track.h);
            painter.fill(
                Rect::new(
                    track.x,
                    track.y + (track.h - height) * state.scroll.min(frame.max_scroll) / frame.max_scroll,
                    3.,
                    height,
                ),
                theme.muted,
                2.,
            );
        }
        painter.text(
            &state.message,
            Rect::new(frame.panel.x + 24., frame.panel.bottom() - 53., frame.panel.w - 345., 46.),
            Style::new(11., if state.error { theme.danger } else { theme.muted }).lines(2),
        );
        if let Some((index, selected)) = state.dropdown {
            let options = state.options(index);
            for (position, rect) in self.settings_dropdown_rects(&frame, index).into_iter().enumerate() {
                painter.shape(
                    rect,
                    Shape {
                        border_width: 1.,
                        border: theme.border,
                        ..Shape::filled(
                            if position == selected || rect.contains(pointer) {
                                theme.surface_raised
                            } else {
                                theme.surface
                            },
                            2.,
                        )
                    },
                );
                painter.text(
                    &options[position],
                    Rect::new(rect.x + 12., rect.y, rect.w - 24., rect.h),
                    Style::new(12., if position == selected { theme.accent } else { theme.text }),
                );
            }
        }
        self.settings_draw_editor(&mut painter, &frame);
        self.settings_draw_connect(&mut painter);
        painter.finish()
    }

    fn settings_row(&self, painter: &mut Painter, rect: Rect, index: usize) {
        let theme = &self.theme;
        let state = &self.settings;
        let field = &FIELDS[index];
        let invalid = state.error && (state.editing == Some(index) || state.message.starts_with(field.key));
        painter.fill(Rect::new(rect.x + 1., rect.bottom() - 1., rect.w - 2., 1.), theme.border, 0.);
        if invalid {
            painter.fill(Rect::new(rect.x + 1., rect.y + 4., 2., rect.h - 8.), theme.danger, 0.);
        }
        if state.draft[index] != state.before[index] {
            painter.fill(Rect::new(rect.x, rect.y + 8., 2., rect.h - 16.), theme.accent, 1.);
        }
        let label = Rect::new(rect.x + 17., rect.y + 10., rect.w - 235., 25.);
        painter.text(field.label, label, Style::new(12., theme.text).weight(500));
        painter.text(
            field.description,
            Rect::new(label.x, label.bottom(), label.w, 34.),
            Style::new(11., theme.muted).lines(2),
        );
        let control = control_rect(rect);
        let value = state.draft[index].as_ref();
        match field.control {
            Control::Toggle => {
                let enabled = value.and_then(toml::Value::as_bool).unwrap_or(false);
                let switch = Rect::new(control.right() - 38., control.y + 9., 34., 20.);
                painter.shape(
                    switch,
                    Shape {
                        radius: 10.,
                        fill: if enabled {
                            (theme.accent, theme.accent_end)
                        } else {
                            (theme.surface_raised, theme.surface_raised)
                        },
                        ..Default::default()
                    },
                );
                painter.fill(
                    Rect::new(switch.x + if enabled { 17. } else { 3. }, switch.y + 3., 14., 14.),
                    theme.text,
                    7.,
                );
            }
            Control::Number(min, max, _, unit) => {
                let number = state.number(field.key, min);
                let min = min.min(number);
                let max = max.max(number);
                let track = Rect::new(control.x, control.y + 18., 90., 3.);
                painter.fill(track, theme.border, 2.);
                let fraction = ((number - min) / (max - min)).clamp(0., 1.);
                painter.fill(Rect::new(track.x, track.y, track.w * fraction, 3.), theme.accent, 2.);
                painter.fill(Rect::new(track.x + track.w * fraction - 4., track.y - 3., 9., 9.), theme.text, 5.);
                let label = if number == 0. && field.key.starts_with("idle.") {
                    "Never".into()
                } else {
                    format!("{} {}", number, unit)
                };
                painter.text(
                    &label,
                    Rect::new(control.x + 98., control.y, 52., control.h),
                    Style::new(11., theme.muted).align(Align::Centre),
                );
            }
            Control::Toml => {
                painter.fill(control, theme.surface_raised, 6.);
                painter.glyph(glyph::FILE, Rect::new(control.x + 10., control.y, 18., control.h), 15, theme.muted);
                painter.text(
                    "Edit section",
                    Rect::new(control.x + 34., control.y, control.w - 40., control.h),
                    Style::new(11., theme.text).weight(500),
                );
            }
            _ => {
                let editing = state.editing == Some(index);
                painter.shape(
                    control,
                    Shape {
                        border_width: 1.,
                        border: if invalid {
                            theme.danger
                        } else if editing {
                            theme.accent
                        } else {
                            theme.border
                        },
                        ..Shape::filled(theme.surface_raised, 6.)
                    },
                );
                let label = if editing {
                    state.input.text.clone()
                } else if matches!(field.control, Control::Toml) {
                    "Edit...".into()
                } else {
                    show(value)
                };
                let colour = matches!(field.control, Control::Colour);
                if colour {
                    let swatch = value
                        .and_then(toml::Value::as_str)
                        .and_then(|value| Colour::parse(value).ok())
                        .unwrap_or(theme.accent);
                    painter.fill(Rect::new(control.x + 8., control.y + 10., 18., 18.), swatch, 4.);
                }
                let text_rect = Rect::new(
                    control.x + if colour { 34. } else { 10. },
                    control.y,
                    control.w - if colour { 40. } else { 18. },
                    control.h,
                );
                if editing {
                    let field = crate::text::Field {
                        cursor: state.input.cursor,
                        anchor: state.input.anchor,
                        visible: true,
                        accent: theme.accent.straight_rgba8(),
                    };
                    let image = self.text.borrow_mut().field(
                        &state.input.text,
                        &theme.font,
                        11.,
                        theme.text,
                        text_rect.w as f32,
                        field,
                    );
                    if let Some(image) = image {
                        painter.image(&image.buffer, (text_rect.x, text_rect.y + (text_rect.h - image.height) / 2.));
                    }
                } else {
                    painter.text(&label, text_rect, Style::new(11., theme.text));
                }
                if matches!(field.control, Control::Choice(_)) {
                    painter.glyph(
                        glyph::CHEVRON_DOWN,
                        Rect::new(control.right() - 22., control.y, 16., control.h),
                        12,
                        theme.muted,
                    );
                }
            }
        }
        if state.draft[index] != state.defaults[index] {
            painter.glyph(glyph::RESTART, reset_rect(rect), 14, theme.muted);
        }
        if state.focus == Some(Hit::Field(index)) {
            let control = if matches!(field.control, Control::Toggle) {
                Rect::new(control.right() - 42., control.y + 5., 42., 28.)
            } else {
                control
            };
            painter.shape(
                control,
                Shape {
                    radius: 6.,
                    border_width: 1.,
                    border: if invalid { theme.danger } else { theme.accent },
                    ..Default::default()
                },
            );
        }
        if state.focus == Some(Hit::Reset(index)) {
            painter.shape(
                reset_rect(rect),
                Shape { radius: 6., border_width: 1., border: theme.accent, ..Default::default() },
            );
        }
    }

    fn settings_preview(&self, painter: &mut Painter, rect: Rect) {
        let theme = &self.theme;
        let state = &self.settings;
        let accent = Colour::parse(state.text("theme.accent")).unwrap_or(theme.accent);
        let end = Colour::parse(state.text("theme.accent_end")).unwrap_or(theme.accent_end);
        let frosted = state.flag("theme.frost");
        painter.shape(rect, Shape { border_width: 1., border: theme.border, ..Shape::filled(theme.background, 6.) });
        let bar = state.text("shell.bar") == "builtin";
        let bottom = state.text("shell.bar_position") == "bottom";
        if bar {
            let top = if bottom { rect.bottom() - 22. } else { rect.y };
            painter.shape(
                Rect::new(rect.x + 1., top + 1., rect.w - 2., 20.),
                Shape {
                    radius: 4.,
                    fill: if frosted {
                        (theme.surface.mix(accent, 0.22), theme.surface.mix(end, 0.22))
                    } else {
                        (theme.surface, theme.surface)
                    },
                    ..Default::default()
                },
            );
            if state.flag("shell.bar_minimap") {
                painter.glyph(glyph::MINIMAP, Rect::new(rect.x + 6., top, 20., 22.), 14, accent);
            }
            painter.text("Workspace", Rect::new(rect.x + 32., top, rect.w - 75., 22.), Style::new(7., theme.muted));
            painter.text("10:42", Rect::new(rect.right() - 38., top, 32., 22.), Style::new(7., theme.text));
        }
        let gap = (state.number("general.gap", 12.) / 2.).clamp(2., 20.);
        let top = rect.y + if bar && !bottom { 22. } else { 0. };
        let height = rect.h - if bar { 22. } else { 0. };
        let width = (rect.w - 3. * gap) * 0.55;
        let radius = state.number("theme.radius", 6.) as f32 / 1.5;
        let tiles = [
            Rect::new(rect.x + gap, top + gap, width, height - 2. * gap),
            Rect::new(rect.x + width + 2. * gap, top + gap, rect.w - width - 3. * gap, (height - 3. * gap) / 2.),
            Rect::new(
                rect.x + width + 2. * gap,
                top + (height + gap) / 2.,
                rect.w - width - 3. * gap,
                (height - 3. * gap) / 2.,
            ),
        ];
        let light = state.text("theme.colour_scheme") == "light";
        for (index, tile) in tiles.iter().enumerate() {
            let fill = if light { theme.text } else { theme.surface };
            painter.shape(
                *tile,
                Shape {
                    radius,
                    fill: (fill, fill),
                    border_width: 1.,
                    border: if index == 0 { accent } else { theme.border },
                    glow_radius: if index == 0 { state.number("theme.glow", 18.) as f32 / 2. } else { 0. },
                    glow: accent.alpha(0.22),
                    ..Default::default()
                },
            );
            let tab_height = (state.number("general.tab_height", 28.) * 0.45).clamp(7., 22.);
            if index == 1 {
                painter.fill(Rect::new(tile.x + 1., tile.y + 1., tile.w - 2., tab_height), theme.background, radius);
                painter.text(
                    "Browser     Files",
                    Rect::new(tile.x + 6., tile.y + 1., tile.w - 12., tab_height),
                    Style::new(7., if light { theme.background } else { theme.text }),
                );
                painter.fill(Rect::new(tile.x + 6., tile.y + tab_height - 1., tile.w * 0.42, 1.), accent, 0.);
            } else {
                painter.text(
                    ["Terminal", "Browser", "Notes"][index],
                    Rect::new(tile.x + 8., tile.y + 5., tile.w - 16., 16.),
                    Style::new(
                        (state.number("theme.font_size", 13.) * 0.62) as f32,
                        if light { theme.background } else { theme.text },
                    ),
                );
            }
            for line in 0..3 {
                let width = (tile.w - 18.) * if line % 2 == 0 { 0.7 } else { 0.95 };
                if tile.h > 48. {
                    painter.fill(
                        Rect::new(
                            tile.x + 8.,
                            tile.y + if index == 1 { tab_height + 9. } else { 30. } + line as f64 * 8.,
                            width,
                            2.,
                        ),
                        theme.border,
                        1.,
                    );
                }
            }
            if index == 0 {
                painter.shape(
                    Rect::new(tile.x + 8., tile.y + 21., 26., 2.),
                    Shape { fill: (accent, end), ..Default::default() },
                );
            }
        }
        let toast = Rect::new(rect.x + rect.w * 0.43, rect.bottom() - 50., rect.w * 0.53, 36.);
        painter.shape(
            toast,
            Shape {
                radius: 5.,
                border_width: 1.,
                border: if frosted { end.alpha(0.6) } else { theme.border },
                fill: if frosted {
                    (theme.surface.mix(accent, 0.20).alpha(0.86), theme.surface.mix(end, 0.24).alpha(0.86))
                } else {
                    (theme.surface_raised, theme.surface_raised)
                },
                ..Default::default()
            },
        );
        painter.glyph(glyph::BELL, Rect::new(toast.x + 7., toast.y, 14., toast.h), 11, theme.muted);
        painter.text(
            "Notification",
            Rect::new(toast.x + 27., toast.y, toast.w - 33., toast.h),
            Style::new(8., theme.text),
        );
    }

    fn settings_config_lines(&self, after: bool) -> Vec<String> {
        let text = match if after { self.settings_document() } else { Ok(self.settings.document_before.clone()) } {
            Ok(text) => text,
            Err(error) => return vec![error],
        };
        let result = (|| {
            let config = crate::config::Config::parse(&text)?;
            let all = toml::Table::try_from(&config.file).map_err(|error| error.to_string())?;
            let mut table = toml::Table::new();
            let selected = self.settings.selected();
            let changed: Vec<_> = selected
                .iter()
                .copied()
                .filter(|index| self.settings.draft[*index] != self.settings.before[*index])
                .collect();
            for index in if changed.is_empty() { selected } else { changed } {
                let key = FIELDS[index].key;
                if let Some((section, name)) = key.split_once('.') {
                    let target = table.entry(section).or_insert_with(|| toml::Value::Table(toml::Table::new()));
                    if let (Some(target), Some(value)) =
                        (target.as_table_mut(), all.get(section).and_then(|value| value.get(name)))
                    {
                        target.insert(name.into(), value.clone());
                    }
                } else if let Some(value) = all.get(key) {
                    table.insert(key.into(), value.clone());
                }
            }
            toml::to_string_pretty(&table).map_err(|error| error.to_string())
        })();
        match result {
            Ok(text) => text.lines().map(String::from).collect(),
            Err(error) => vec![error],
        }
    }

    fn settings_config(&self, painter: &mut Painter, rect: Rect) {
        let theme = &self.theme;
        painter.text(
            "config.toml",
            Rect::new(rect.x, rect.y, rect.w - 32., 36.),
            Style::new(12., theme.text).weight(500),
        );
        let edit = Rect::new(rect.right() - 112., rect.y + 4., 100., 28.);
        painter.fill(edit, theme.surface_raised, 5.);
        painter.text("Edit full file...", edit, Style::new(11., theme.text).align(Align::Centre));
        let reference = Rect::new(rect.right() - 206., rect.y + 4., 84., 28.);
        painter.fill(reference, theme.surface_raised, 5.);
        painter.text("Reference", reference, Style::new(11., theme.text).align(Align::Centre));
        if self.settings.focus == Some(Hit::EditDocument) {
            painter.shape(edit, Shape { radius: 5., border_width: 1., border: theme.accent, ..Default::default() });
        }
        if self.settings.focus == Some(Hit::Reference) {
            painter
                .shape(reference, Shape { radius: 5., border_width: 1., border: theme.accent, ..Default::default() });
        }
        if rect.h < 70. {
            return;
        }
        let before = self.settings_config_lines(false);
        let after = self.settings_config_lines(true);
        let before_keys = config_line_keys(&before);
        let after_keys = config_line_keys(&after);
        let before_values: std::collections::BTreeMap<_, _> = before_keys.iter().zip(&before).collect();
        let width = (rect.w - 12.) / 2.;
        for (column, title) in ["Before", "After"].iter().enumerate() {
            let left = rect.x + column as f64 * (width + 12.);
            painter.text(
                title,
                Rect::new(left, rect.y + 36., width, 22.),
                Style::new(11., if column == 0 { theme.muted } else { theme.accent }),
            );
            let pane = Rect::new(left, rect.y + 62., width, rect.h - 62.);
            painter
                .shape(pane, Shape { border_width: 1., border: theme.border, ..Shape::filled(theme.background, 6.) });
            let area = Rect::new(left + 8., pane.y + 8., width - 16., pane.h - 16.);
            let lines = if column == 0 { &before } else { &after };
            painter.clipped(area, |painter| {
                for (line, label) in lines.iter().enumerate() {
                    let top = area.y + line as f64 * 23. - self.settings.config_scroll;
                    if top < area.y || top + 23. > area.bottom() {
                        continue;
                    }
                    let changed = column == 1 && before_values.get(&after_keys[line]) != Some(&label);
                    if changed {
                        painter.fill(Rect::new(area.x, top, area.w, 23.), theme.accent.alpha(0.06), 0.);
                    }
                    painter.text_font(
                        label,
                        Rect::new(area.x + 4., top, area.w - 8., 23.),
                        Style::new(10., if changed || label.starts_with('[') { theme.accent } else { theme.muted }),
                        "monospace",
                    );
                }
            });
        }
    }
}
