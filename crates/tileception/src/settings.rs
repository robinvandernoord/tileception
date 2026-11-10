//! Native settings, with an unapplied draft and a miniature desktop preview.

use smithay::{
    backend::renderer::gles::GlesRenderer,
    input::keyboard::{keysyms, xkb},
};
use tileception_core::{Point, Rect, WindowId};

pub(crate) mod client;
mod colour;
mod connect;
mod defaults_page;
mod display_preview;
mod editor;
mod home_page;
mod interaction;
mod keymap;
mod network_page;
pub(crate) mod sections;

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
    Section,
    Keyboard,
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
        title: "Displays",
        summary: "Arrangement, scaling & touchscreens",
        description: "Display layout, scaling and input routing.",
        icon: glyph::MONITOR,
    },
    Category {
        title: "Sharing",
        summary: "Mouse, keyboard & clipboard",
        description: "Connections to other computers.",
        icon: glyph::GLOBE,
    },
    Category {
        title: defaults_page::TITLE,
        summary: "Browser, files & file types",
        description: "The applications that open links, folders and files.",
        icon: glyph::FILE,
    },
    Category {
        title: network_page::TITLE,
        summary: "Wi-Fi, saved & hidden networks",
        description: "Connections, saved Wi-Fi networks and hidden networks. Changes here apply at once.",
        icon: glyph::WIFI,
    },
];

/// The height of the sidebar's list of pages.
fn nav_height() -> f64 {
    CATEGORIES.len() as f64 * 40.
}

/// The Default apps page.
fn apps_page() -> usize {
    CATEGORIES.iter().position(|category| category.title == defaults_page::TITLE).expect("Default apps is a page")
}

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
        11,
        "shell.bar_network",
        "Network chip",
        "Wi-Fi and wired status in the bar; click it to join a network.",
        Control::Toggle,
    ),
    field(
        2,
        "shell.tray_max_icons",
        "Tray slots",
        "Extra applications move into an overflow menu.",
        Control::Number(1., 8., 1., "icons"),
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
    field(3, "keyboard.layout", "Keyboard layout", "Choose a layout and variant.", Control::Keyboard),
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
        3,
        "touchpad.follow_fingers",
        "Gestures follow your fingers",
        "Swipes and pinches move the view as you go; let go early to cancel.",
        Control::Toggle,
    ),
    field(
        4,
        "idle.lock_after",
        "Lock after idle",
        "Time without input before the screen locks.",
        Control::Number(0., 3600., 60., "s"),
    ),
    field(
        4,
        "idle.off_after",
        "Screen off after idle",
        "Time without input before the screens turn off.",
        Control::Number(0., 7200., 60., "s"),
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
        "battery.power_profiles",
        "Automatic power profiles",
        "Select profiles based on charger and battery state.",
        Control::Toggle,
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
    field(7, "general.autostart", "Autostart commands", "Commands run once when the session starts.", Control::Section),
    field(
        7,
        "general.default_binds",
        "Default shortcuts",
        "Include the built-in bindings; custom shortcuts override them.",
        Control::Toggle,
    ),
    field(7, "binds", "Keyboard & gesture shortcuts", "Record shortcuts and choose their actions.", Control::Section),
    field(7, "general.terminal", "Default terminal", "Terminal command used by the desktop.", Control::Text),
    field(7, "launch_args", "Application arguments", "Extra arguments by desktop file name.", Control::Section),
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
    field(1, "rules", "Window rules", "Floating, size, fullscreen and minimized rules.", Control::Section),
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
    field(2, "duck.size", "Rubber duck size", "1 is the normal size.", Control::Number(0.5, 4., 0.25, "x")),
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
    field(
        4,
        "battery.levels",
        "Battery colour thresholds",
        "Charge levels and their border colours.",
        Control::Section,
    ),
    field(
        6,
        "greeter.share",
        "Share appearance with login screen",
        "Publish this session's accent and wallpaper.",
        Control::Toggle,
    ),
    field(
        8,
        "outputs",
        "Displays",
        "Arrange monitors, how the pointer crosses between them, scaling and screen areas.",
        Control::Section,
    ),
    field(
        8,
        "general.stretch_screen_edges",
        "Full-edge pointer crossing",
        "Map the whole screen edge proportionally onto the neighboring screen.",
        Control::Toggle,
    ),
    field(8, "touchscreens", "Touchscreens", "Choose the display each touchscreen controls.", Control::Section),
    field(9, "handoff", "Sharing", "Connect computers to share mouse, keyboard and clipboard.", Control::Section),
];

#[derive(Clone, Copy, PartialEq, Eq)]
enum Hit {
    Page(Option<usize>),
    Card(usize),
    Search,
    Field(usize),
    /// The swatch of a colour field, which opens the colour picker.
    Swatch(usize),
    /// A battery colour threshold: its charge slider, swatch and remove button.
    LevelAt(usize),
    LevelColour(usize),
    LevelRemove(usize),
    LevelAdd,
    Reset(usize),
    ResetSection,
    Discard,
    Apply,
    Close,
    Option(usize, usize),
    EditDocument,
    /// Smart home: the how-to and troubleshooting, a link to Tuya's
    /// platform, and fetching the devices again.
    HomeGuide,
    HomeTrouble,
    HomeLink(usize),
    HomeRefresh,
    HomeCloud(bool),
    /// Default apps: an entry's application button, one of its menu's
    /// applications, and showing all file types.
    AppDefault(usize),
    AppChoice(usize, usize),
    AppsAll,
    /// Network: the Wi-Fi switch, a saved network's auto-join switch and
    /// Forget, the hidden network's fields and Join, and the editor.
    NetWifi,
    /// A network in range: join it, or leave the one Wi-Fi is on.
    NetVisible(usize),
    NetAuto(usize),
    NetForget(usize),
    NetName,
    NetPassword,
    NetReveal,
    NetJoin,
    NetEditor,
    Reference,
    AdvancedAppearance,
    ReviewChanges,
}

#[derive(Default)]
pub struct Settings {
    pub open: bool,
    /// The window Settings is shown in, while open.
    window: Option<WindowId>,
    client: Option<client::Client>,
    pointer: (f64, f64),
    pressed: Option<interaction::Target>,
    text_drag: bool,
    /// That window's size, as the layout last gave it.
    size: (f64, f64),
    page: Option<usize>,
    query: String,
    search_input: crate::text_input::TextInput,
    compose: Option<xkb::compose::State>,
    before: Vec<Option<toml::Value>>,
    draft: Vec<Option<toml::Value>>,
    defaults: Vec<Option<toml::Value>>,
    scroll: f64,
    nav_scroll: f64,
    focus: Option<Hit>,
    editing: Option<usize>,
    drag: Option<(usize, Rect)>,
    /// A battery threshold's charge slider being dragged.
    level_drag: Option<(usize, Rect)>,
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
    /// Default apps: the entries as last read, whether every file type
    /// shows, and the open menu with its highlighted application.
    apps: Vec<defaults_page::Entry>,
    apps_all: bool,
    apps_menu: Option<(usize, usize)>,
    /// Defaults chosen and not applied yet, part of the draft.
    apps_pending: Vec<defaults_page::Pending>,
    /// Network: the hidden network's form.
    network: network_page::Page,
    /// Smart home: devices were imported, so the page shows them instead of
    /// the setup steps.
    home_ready: bool,
    /// The how-to and the troubleshooting are unfolded.
    home_guide: bool,
    home_trouble: bool,
    /// Names and places from the device file, while not connected.
    home_listed: Vec<(String, String)>,
    /// The device file keeps cloud access, so its options can change.
    home_cloud_saved: bool,
    needs_reload: bool,
    connect: Option<connect::Connect>,
    section: Option<sections::model::Screen>,
    parked_sections: Vec<sections::model::Screen>,
    identify_until: Option<std::time::Instant>,
    advanced_appearance: bool,
    keymap: Option<keymap::Picker>,
    review_changes: bool,
    colour: Option<colour::Picker>,
    /// Full-edge crossing as it was when the Displays screen opened, for its Cancel.
    edges_before: Option<Option<toml::Value>>,
    display_preview: Option<display_preview::Preview>,
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

fn advanced_appearance(key: &str) -> bool {
    matches!(
        key,
        "theme.accent_end"
            | "theme.glow"
            | "theme.popup_radius"
            | "theme.background"
            | "theme.surface"
            | "theme.surface_raised"
            | "theme.border"
            | "theme.text"
            | "theme.muted"
            | "theme.on_accent"
            | "theme.danger"
            | "theme.warning"
            | "theme.guest"
    )
}

fn show(value: Option<&toml::Value>) -> String {
    match value {
        Some(toml::Value::String(text)) => text.clone(),
        Some(toml::Value::Float(number)) => number.to_string(),
        Some(value) => value.to_string(),
        None => "System default".into(),
    }
}

/// The heading a field is listed under on its category page. Fields with an
/// empty group open the page without a heading.
fn group(key: &str) -> &'static str {
    match key {
        "shell.bar" | "shell.bar_position" | "shell.bar_minimap" | "shell.bar_network" | "shell.tray_max_icons" => {
            "Bar"
        }
        "shell.launcher" | "shell.notifications" | "shell.doctor" | "shell.power_menu" | "duck.size" => "Panels",
        "shell.media" => "Media keys",
        "shell.lock" | "general.lock_command" => "Lock screen",
        "general.suspend_enabled" | "general.welcome" | "greeter.share" => "Session",
        "general.default_binds" | "binds" => "Shortcuts",
        "general.autostart" | "general.terminal" | "launch_args" => "Applications",
        "general.stretch_screen_edges" | "outputs" | "touchscreens" => "Displays",
        "handoff" => "Sharing",
        key if advanced_appearance(key) => "Advanced",
        key if key.starts_with("media.") => "Media keys",
        key if key.starts_with("keyboard.") => "Keyboard",
        key if key.starts_with("touchpad.") => "Touchpad",
        key if key.starts_with("idle.") => "Idle",
        key if key.starts_with("battery.") => "Battery",
        _ => "",
    }
}

const GROUPS: &[&str] = &[
    "",
    "Bar",
    "Panels",
    "Media keys",
    "Keyboard",
    "Touchpad",
    "Idle",
    "Battery",
    "Lock screen",
    "Session",
    "Shortcuts",
    "Applications",
    "Displays",
    "Sharing",
    "Advanced",
];

/// A config value as the panel shows it: "Built-in" rather than `builtin`.
fn choice_label(key: &str, value: &str) -> String {
    match (key, value) {
        (_, "builtin") => "Built-in".into(),
        (_, "none") => "Off".into(),
        ("theme.colour_scheme", "default") => "Each app decides".into(),
        (_, "center") => "Centre".into(),
        (_, "areas") => "Click areas".into(),
        (_, "clickfinger") => "Finger count".into(),
        (_, value) if value.chars().all(|character| character.is_ascii_lowercase()) => {
            let mut characters = value.chars();
            characters
                .next()
                .map_or_else(String::new, |first| first.to_ascii_uppercase().to_string() + characters.as_str())
        }
        (_, value) => value.into(),
    }
}

/// Seconds as a short duration: "45 s", "10 min", "1 h 30 min".
fn duration(seconds: f64) -> String {
    let seconds = seconds.round() as i64;
    if seconds < 60 {
        return format!("{seconds} s");
    }
    let minutes = seconds / 60;
    match (minutes / 60, minutes % 60) {
        (0, minutes) => format!("{minutes} min"),
        (hours, 0) => format!("{hours} h"),
        (hours, minutes) => format!("{hours} h {minutes} min"),
    }
}

fn number_label(key: &str, number: f64, unit: &str) -> String {
    if key.starts_with("idle.") {
        return if number == 0. { "Never".into() } else { duration(number) };
    }
    if unit == "x" {
        return format!("{number}x");
    }
    if key == "touchpad.speed" {
        return if number == 0. { "Default".into() } else { format!("{number:+.1}") };
    }
    let number = (number * 100.).round() / 100.;
    if unit.is_empty() { number.to_string() } else { format!("{number} {unit}") }
}

/// A battery colour as entered: a hex colour or a theme colour's name.
fn named_colour(theme: &crate::theme::Theme, value: &str) -> Option<Colour> {
    Colour::parse(value).ok().or(match value {
        "accent" => Some(theme.accent),
        "warning" => Some(theme.warning),
        "danger" => Some(theme.danger),
        "muted" => Some(theme.muted),
        "text" => Some(theme.text),
        _ => None,
    })
}

struct Frame {
    panel: Rect,
    body: Rect,
    content: Rect,
    /// Where the scrolled part of the page starts.
    top: f64,
    preview: Option<Rect>,
    hits: Vec<(Rect, Hit)>,
    rows: Vec<(Rect, usize)>,
    cards: Vec<(Rect, usize)>,
    config: Option<Rect>,
    /// Each group's heading and the card holding its rows.
    groups: Vec<(&'static str, Rect)>,
    advanced: Option<Rect>,
    /// The Smart home page's steps or devices, below its rows.
    home: Option<home_page::HomeFrame>,
    /// The Default apps page's rows.
    apps: Option<defaults_page::AppsFrame>,
    /// The Network page's cards.
    network: Option<network_page::NetworkFrame>,
    /// The battery colour thresholds, under their row on Power & battery.
    levels: Vec<(Rect, usize)>,
    max_scroll: f64,
}

fn viewport(frame: &Frame) -> Rect {
    Rect::new(
        frame.content.x - 1.,
        frame.top,
        frame.content.w + 2.,
        (frame.config.map_or(frame.body.bottom() - 8., |rect| rect.y - 12.) - frame.top).max(1.),
    )
}

pub(crate) fn intersect(left: Rect, right: Rect) -> Option<Rect> {
    let start_x = left.x.max(right.x);
    let start_y = left.y.max(right.y);
    let end_x = left.right().min(right.right());
    let end_y = left.bottom().min(right.bottom());
    (end_x > start_x && end_y > start_y).then(|| Rect::new(start_x, start_y, end_x - start_x, end_y - start_y))
}

fn control_rect(row: Rect) -> Rect {
    Rect::new(row.right() - 194., if row.h > 100. { row.bottom() - 44. } else { row.y + (row.h - 36.) / 2. }, 150., 36.)
}
fn reset_rect(row: Rect) -> Rect {
    Rect::new(row.right() - 36., control_rect(row).y + 2., 28., 32.)
}

fn levels_index() -> usize {
    FIELDS.iter().position(|field| field.key == "battery.levels").expect("battery levels are a field")
}

/// The height of one battery colour threshold under its row.
const LEVEL_ROW: f64 = 52.;

/// Whether this field's entries show on its page, rather than in a screen of their own.
fn inline_levels(index: usize, query: &str) -> bool {
    FIELDS[index].key == "battery.levels" && query.is_empty()
}

struct LevelParts {
    swatch: Rect,
    slider: Rect,
    remove: Rect,
}

fn level_parts(row: Rect) -> LevelParts {
    let control = control_rect(Rect::new(row.x, row.y, row.w, row.h));
    LevelParts {
        swatch: Rect::new(control.x, row.y + (row.h - 30.) / 2., 30., 30.),
        slider: Rect::new(control.x + 44., row.y + (row.h - 24.) / 2., control.right() - control.x - 44., 24.),
        remove: reset_rect(row),
    }
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
        let count = self.draft.iter().zip(&self.before).filter(|(draft, before)| draft != before).count()
            + self.apps_pending.len();
        count.max(usize::from(
            self.document_draft != self.document_before
                || !self.parked_editors.is_empty()
                || !self.parked_sections.is_empty()
                || self.needs_reload,
        ))
    }

    fn selected(&self) -> Vec<usize> {
        let query = self.query.to_lowercase();
        let mut selected: Vec<_> = FIELDS
            .iter()
            .enumerate()
            .filter(|(_, field)| {
                if query.is_empty() {
                    // Full-edge crossing lives in the Displays screen, beside the arrangement.
                    self.page == Some(field.category)
                        && (self.advanced_appearance || !advanced_appearance(field.key))
                        // Smart home's rows wait until devices are set up.
                        && (self.home_ready || !field.key.starts_with("home."))
                        && field.key != "general.stretch_screen_edges"
                } else {
                    format!("{} {} {}", field.label, field.description, field.key).to_lowercase().contains(&query)
                }
            })
            .map(|(index, _)| index)
            .collect();
        if query.is_empty() {
            selected.sort_by_key(|index| GROUPS.iter().position(|group_name| *group_name == group(FIELDS[*index].key)));
        } else {
            selected.sort_by_key(|index| FIELDS[*index].category);
        }
        selected
    }

    /// The heading a listed field falls under: its group on a category page,
    /// its category among search results.
    fn heading(&self, index: usize) -> &'static str {
        if self.query.is_empty() { group(FIELDS[index].key) } else { CATEGORIES[FIELDS[index].category].title }
    }

    /// Whether the draft changes anything in this category.
    fn category_changed(&self, category: usize) -> bool {
        FIELDS
            .iter()
            .enumerate()
            .any(|(index, field)| field.category == category && self.draft[index] != self.before[index])
            || self.parked_sections.iter().any(|screen| FIELDS[screen.field].category == category)
            || self
                .parked_editors
                .iter()
                .any(|editor| editor.field.is_some_and(|index| FIELDS[index].category == category))
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

    /// The battery colour thresholds in the draft: charge and colour.
    fn levels(&self) -> Vec<(i64, String)> {
        self.draft[levels_index()]
            .as_ref()
            .and_then(toml::Value::as_array)
            .map(|levels| {
                levels
                    .iter()
                    .map(|level| {
                        (
                            level.get("at").and_then(toml::Value::as_integer).unwrap_or(0),
                            level.get("colour").and_then(toml::Value::as_str).unwrap_or("").to_string(),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    fn set_levels(&mut self, levels: &[(i64, String)]) {
        let entries = levels
            .iter()
            .map(|(threshold, colour)| {
                toml::Value::Table(toml::Table::from_iter([
                    ("at".to_string(), toml::Value::Integer(*threshold)),
                    ("colour".to_string(), toml::Value::String(colour.clone())),
                ]))
            })
            .collect();
        self.draft[levels_index()] = Some(toml::Value::Array(entries));
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

    /// The options of an open dropdown, below its control (or above it near
    /// the bottom), at least as wide as the control and right-aligned with it.
    fn settings_dropdown_rects(&self, frame: &Frame, index: usize) -> Vec<Rect> {
        let Some((row, _)) = frame.rows.iter().find(|(_, field)| *field == index) else { return Vec::new() };
        let control = control_rect(*row);
        let options = self.settings.options(index);
        let widest = options
            .iter()
            .filter_map(|option| {
                let label = choice_label(FIELDS[index].key, option);
                let style = Style::new(12., self.theme.text);
                self.text
                    .borrow_mut()
                    .styled(&label, &self.theme.font, style.size, style.colour, 400., style.weight, style.lines)
                    .map(|rendered| rendered.width)
            })
            .fold(0., f64::max);
        let width = (widest + 52.).max(control.w).min(320.);
        let height = options.len() as f64 * 32.;
        let below = control.bottom() + 6.;
        let top = if below + height + 8. > frame.panel.bottom() - 64. { control.y - 6. - height } else { below };
        let top = top.max(frame.panel.y + 70.);
        (0..options.len())
            .map(|position| Rect::new(control.right() - width, top + position as f64 * 32., width, 32.))
            .collect()
    }

    /// Opens Settings in a floating window on the Desktop, or closes it.
    /// Drafts are kept while it is closed.
    /// Opens Settings on the page with this title, as plugins ask.
    pub fn open_settings_named(&mut self, title: &str) {
        match CATEGORIES.iter().position(|category| category.title.eq_ignore_ascii_case(title.trim())) {
            Some(page) => self.open_settings_page(page),
            None => tracing::warn!("no Settings page called {title:?}"),
        }
    }

    /// Opens Settings on one page, such as Smart home from the panel.
    pub fn open_settings_page(&mut self, page: usize) {
        if !self.settings.open {
            self.toggle_settings();
        }
        if !self.settings.open {
            return;
        }
        self.notifications.open = false;
        self.settings.page = Some(page);
        self.settings.query.clear();
        self.settings.search_input.clear();
        self.settings.scroll = 0.;
        self.open_settings_page_section();
        self.schedule_render();
    }

    pub fn toggle_settings(&mut self) {
        self.section_cancel_recording();
        if self.is_locked() {
            return;
        }
        if self.settings.open {
            self.close_settings();
            return;
        }
        if (self.settings.before.is_empty() || self.settings.changed() == 0)
            && self.settings.editor.is_none()
            && self.settings.editing.is_none()
            && self.settings.parked_editors.is_empty()
            && self.settings.connect.as_ref().is_none_or(|form| !form.busy())
            && self.settings.section.is_none()
            && self.settings.parked_sections.is_empty()
            && self.settings.keymap.is_none()
            && let Err(error) = self.settings_load()
        {
            tracing::warn!("could not open Settings: {error}");
            return;
        }
        self.settings.open = true;
        self.settings.compose = crate::ui::compose_state();
        self.power_menu = None;
        self.close_tray();
        self.close_launcher();
        self.notifications.open = false;
        if !self.settings.error {
            self.settings.message.clear();
        }
        self.settings.size = settings_size(self.world.screen());
        self.open_settings_page_section();
        match client::Client::start(self, client::Kind::Settings) {
            Ok(client) => self.settings.client = Some(client),
            Err(error) => {
                self.settings.open = false;
                tracing::warn!("could not open Settings client: {error}");
            }
        }
        self.schedule_render();
    }

    /// Fits the Settings window to its screen again when that screen changed
    /// size or rotated, so every control, including a display confirmation,
    /// stays reachable. A window that still fits keeps its place and size.
    pub(crate) fn settings_fit_screen(&mut self) {
        let Some(window) = self.settings.window else { return };
        if let Some(area) = self.area_for_window(window)
            && self.context_area().as_ref() != Some(&area)
        {
            self.with_area(&area, |state| state.settings_fit_screen());
            return;
        }
        let screen = self.world.screen();
        let Some(rect) = self.world.float_rect(window) else { return };
        if screen.contains_rect(&rect) {
            return;
        }
        let (width, height) = settings_size(screen);
        let (width, height) = (width.min(screen.w), height.min(screen.h));
        let fitted = Rect::new(screen.x + (screen.w - width) / 2., screen.y + (screen.h - height) / 2., width, height);
        self.world.set_float_rect(window, fitted);
        self.settings.dropdown = None;
        self.settings.colour = None;
        self.apply_layout();
        if let Some(toplevel) = self.windows.get(&window).and_then(|placed| placed.toplevel()) {
            toplevel.send_configure();
        }
        self.schedule_render();
    }

    /// Takes the Settings window away, wherever it is; the draft stays.
    pub(crate) fn close_settings(&mut self) {
        self.settings_revert_displays();
        self.settings_release();
        self.settings.open = false;
        if let Some(client) = self.settings.client.take() {
            self.loop_handle.remove(client.source);
        }
        self.settings.window = None;
        self.settings.drag = None;
        self.settings.focus = None;
        self.settings.colour = None;
        self.settings.dropdown = None;
        self.schedule_render();
    }

    pub(crate) fn is_settings_window(&self, window: WindowId) -> bool {
        self.settings.window == Some(window)
    }

    /// The layout gave the Settings window a new size; it fills it.
    pub(crate) fn settings_resized(&mut self, width: f64, height: f64) {
        if (self.settings.size.0 - width).abs() > 0.5 || (self.settings.size.1 - height).abs() > 0.5 {
            self.settings.size = (width, height);
            self.settings.dropdown = None;
            self.settings.colour = None;
            self.schedule_render();
        }
    }

    /// Settings renders in surface-local coordinates, like any other client.
    fn settings_rect(&self) -> Option<Rect> {
        self.settings.open.then(|| Rect::new(0., 0., self.settings.size.0, self.settings.size.1))
    }

    /// Whether keys go to Settings.
    pub(crate) fn settings_focused(&self) -> bool {
        self.settings.window.is_some()
            && self.world.focus() == self.settings.window
            && self.is_active_output()
            && !self.world.in_overview()
    }

    pub(crate) fn settings_client_window(
        &mut self,
        surface: &smithay::wayland::shell::xdg::ToplevelSurface,
        window: WindowId,
    ) {
        use smithay::reexports::wayland_server::Resource;
        if self
            .settings
            .client
            .as_ref()
            .is_some_and(|client| surface.wl_surface().client().is_some_and(|owner| owner.id() == client.client_id))
        {
            self.settings.window = Some(window);
            surface.with_pending_state(|pending| {
                pending.size = Some((self.settings.size.0 as i32, self.settings.size.1 as i32).into())
            });
        }
    }

    fn settings_client_event(&mut self, event: client::Event) {
        if !self.settings.open {
            return;
        }
        self.invalidate_settings_client();
        if let Some(area) = self.settings.window.and_then(|window| self.area_for_window(window))
            && self.context_area().as_ref() != Some(&area)
        {
            self.with_area(&area, |state| state.settings_client_event(event));
            return;
        }
        match event {
            client::Event::Resize(width, height) => {
                self.settings.pressed = None;
                self.settings_resized(f64::from(width), f64::from(height));
            }
            client::Event::Motion(point) => {
                self.settings.pointer = (point.x, point.y);
                self.settings_motion(point);
            }
            client::Event::Leave => {
                self.settings.pointer = (-1., -1.);
                self.settings.pressed = None;
            }
            client::Event::Button(pressed) => self.settings_pointer_button(pressed),
            client::Event::Scroll(amount) => self.settings_scroll(amount),
            client::Event::Key { sym, mods, pressed, code } => {
                if pressed {
                    self.settings_key(sym, mods);
                    if crate::key_repeat::edits(sym, mods) {
                        self.start_key_repeat(
                            code,
                            crate::key_repeat::Repeat::Commands(vec![crate::commands::Command::SettingsKey(sym, mods)]),
                        );
                    }
                } else {
                    self.key_released(code, crate::config::modifier_of(sym).is_some());
                }
            }
            client::Event::Unfocus => {
                self.stop_key_repeat();
                self.settings_release();
                self.section_cancel_recording();
            }
            client::Event::Closed => self.close_settings(),
        }
        self.settings_update_cursor();
        self.schedule_render();
    }

    fn invalidate_settings_client(&self) {
        if let Some(client) = &self.settings.client {
            client.invalidate();
        }
    }

    /// Service the client's drawing independently of screen captures and the
    /// window's visibility, in the output context that owns its UI model.
    pub(crate) fn update_settings_client(&mut self, renderer: &mut GlesRenderer) {
        self.settings_sync_display_preview();
        if self.settings.open
            && let Some(mut screen) = self.settings.section.take()
        {
            self.section_detected_displays(&mut screen);
            self.settings.section = Some(screen);
        }
        if let Some(area) = self.settings.window.and_then(|window| self.area_for_window(window))
            && self.context_area().as_ref() != Some(&area)
        {
            self.with_area(&area, |state| state.render_settings_client(renderer));
        } else {
            self.render_settings_client(renderer);
        }
    }

    /// Fill the client's shared-memory buffer. The scene then displays its
    /// wl_surface normally, including in captures, previews and Overview.
    fn render_settings_client(&self, renderer: &mut GlesRenderer) {
        use smithay::{
            backend::{
                allocator::Fourcc,
                renderer::{Bind, Offscreen, damage::OutputDamageTracker, gles::GlesTexture},
            },
            utils::{Rectangle, Transform},
        };
        let Some(client) = &self.settings.client else { return };
        if !client.needs_frame(self.scale) {
            return;
        }
        let logical = (self.settings.size.0 as i32, self.settings.size.1 as i32);
        let size =
            ((self.settings.size.0 * self.scale).round() as i32, (self.settings.size.1 * self.scale).round() as i32);
        let result = (|| -> Result<Vec<u8>, String> {
            let elements = self.settings_elements(renderer);
            let mut texture: GlesTexture =
                renderer.create_buffer(Fourcc::Abgr8888, size.into()).map_err(|error| error.to_string())?;
            {
                let mut target = renderer.bind(&mut texture).map_err(|error| error.to_string())?;
                // The elements are in physical pixels already, as on outputs.
                let mut damage = OutputDamageTracker::new(size, 1.0, Transform::Normal);
                let result = damage
                    .render_output(renderer, &mut target, 0, &elements, self.theme.background.0)
                    .map_err(|error| format!("{error:?}"))?;
                result.sync.wait().map_err(|error| format!("{error:?}"))?;
            }
            crate::capture::read_region(renderer, &texture, Rectangle::from_size(size.into()))
        })();
        match result {
            Ok(pixels) => client.frame(client::Frame { pixels, width: size.0, height: size.1, logical }),
            Err(error) => tracing::warn!("could not draw Settings client: {error}"),
        }
    }

    fn settings_frame(&self) -> Frame {
        let panel = self.settings_rect().unwrap_or_else(|| {
            let screen = self.output_rect();
            Rect::new(screen.x, screen.y, self.settings.size.0.max(320.), self.settings.size.1.max(240.))
        });
        let width = panel.w;
        let height = panel.h;
        let sidebar = if width < 900. { 64. } else { 205. };
        let body = Rect::new(panel.x + sidebar, panel.y + 65., width - sidebar, height - 125.);
        let compact = body.h < 640.;
        let content =
            Rect::new(body.x + 28., body.y + if compact { 18. } else { 26. }, (body.w - 56.).max(1.), body.h - 40.);
        let search = Rect::new(panel.right() - 340., panel.y + 15., 282., 35.);
        let mut hits =
            vec![(search, Hit::Search), (Rect::new(panel.right() - 48., panel.y + 15., 34., 35.), Hit::Close)];
        hits.push((Rect::new(panel.x + 12., body.y + 20., sidebar - 24., 40.), Hit::Page(None)));
        let nav = Rect::new(panel.x + 12., body.y + 70., sidebar - 24., (body.h - 166.).max(1.));
        let nav_pitch = 40.;
        let nav_scroll = self.settings.nav_scroll.min((CATEGORIES.len() as f64 * nav_pitch - nav.h).max(0.));
        for index in 0..CATEGORIES.len() {
            let rect = Rect::new(nav.x, nav.y + index as f64 * nav_pitch - nav_scroll, nav.w, nav_pitch - 4.);
            if let Some(rect) = intersect(rect, nav) {
                hits.push((rect, Hit::Page(Some(index))));
            }
        }
        hits.push((Rect::new(panel.x + 12., body.bottom() - 88., sidebar - 24., 36.), Hit::EditDocument));
        hits.push((Rect::new(panel.x + 12., body.bottom() - 48., sidebar - 24., 36.), Hit::Reference));
        let changed = self.settings.changed() > 0;
        if changed {
            hits.push((Rect::new(panel.x + 20., panel.bottom() - 46., 150., 32.), Hit::ReviewChanges));
        }
        hits.push((Rect::new(panel.right() - 300., panel.bottom() - 46., 134., 32.), Hit::Discard));
        hits.push((Rect::new(panel.right() - 154., panel.bottom() - 46., 130., 32.), Hit::Apply));
        let header = if compact { 64. } else { 84. };
        let mut frame = Frame {
            panel,
            body,
            content,
            top: content.y + header,
            preview: None,
            hits,
            rows: Vec::new(),
            cards: Vec::new(),
            config: None,
            groups: Vec::new(),
            advanced: None,
            home: None,
            apps: None,
            network: None,
            levels: Vec::new(),
            max_scroll: 0.,
        };
        if changed && self.settings.review_changes {
            let lines = if compact { 4 } else { 6 };
            let config_height = (42. + lines as f64 * 23.).min(body.h * 0.4).max(100.);
            frame.config = Some(Rect::new(content.x, body.bottom() - config_height - 12., content.w, config_height));
        }
        if self.settings.page.is_none() && self.settings.query.is_empty() {
            let gap = 12.;
            let col = (content.w - gap * 2.) / 3.;
            let top = frame.top;
            let row = ((content.h - header - 40.) / 4.).clamp(150., 176.);
            let short = (row * 0.85).max(150.);
            let mut rects = [
                Rect::new(content.x, top, col, row * 2. + gap),
                Rect::new(content.x + col + gap, top, col, row),
                Rect::new(content.x + 2. * (col + gap), top, col, row),
                Rect::new(content.x + col + gap, top + row + gap, col * 2. + gap, row),
                Rect::new(content.x, top + 2. * (row + gap), col, short),
                Rect::new(content.x + col + gap, top + 2. * (row + gap), col, short),
                Rect::new(content.x + 2. * (col + gap), top + 2. * (row + gap), col, short),
                Rect::new(content.x, top + 2. * (row + gap) + short + gap, col * 2. + gap, 150.),
                Rect::new(content.x + 2. * (col + gap), top + 2. * (row + gap) + short + gap, col, 150.),
                Rect::new(content.x, top + 2. * (row + gap) + short + 162., content.w, 150.),
            ];
            if content.w < 700. {
                let half = (content.w - gap) / 2.;
                rects = [
                    Rect::new(content.x, top, content.w, 300.),
                    Rect::new(content.x, top + 312., half, 150.),
                    Rect::new(content.x + half + gap, top + 312., half, 150.),
                    Rect::new(content.x, top + 474., content.w, 150.),
                    Rect::new(content.x, top + 636., half, 150.),
                    Rect::new(content.x + half + gap, top + 636., half, 150.),
                    Rect::new(content.x, top + 798., half, 150.),
                    Rect::new(content.x + half + gap, top + 798., half, 150.),
                    Rect::new(content.x, top + 960., content.w, 150.),
                    Rect::new(content.x, top + 1122., content.w, 150.),
                ];
            }
            let bottom = rects.iter().map(Rect::bottom).reduce(f64::max).unwrap_or(top);
            frame.max_scroll = (bottom - body.bottom() + 20.).max(0.);
            for (index, mut rect) in rects.into_iter().enumerate() {
                rect.y -= self.settings.scroll.min(frame.max_scroll);
                if rect.bottom() > frame.top && rect.y < body.bottom() - 8. {
                    frame.cards.push((rect, index));
                    let clipped = intersect(viewport(&frame), rect);
                    if let Some(clipped) = clipped {
                        frame.hits.push((clipped, Hit::Card(index)));
                    }
                }
            }
        } else {
            let selected = self.settings.selected();
            // The preview column is kept on every page when the window has room
            // for it, so controls stay in one place from page to page.
            let room = body.w >= 860. && body.h >= 530.;
            let show_preview =
                room && self.settings.page.is_some_and(|page| page <= 2) && self.settings.query.is_empty();
            let row_width = if room { content.w - 290. } else { content.w };
            if show_preview {
                frame.preview = Some(Rect::new(content.right() - 260., frame.top + 26., 260., 178.));
            }
            // Default apps are not config.toml settings: nothing to restore.
            if self.settings.query.is_empty() && self.settings.page.is_some() && self.settings.page != Some(apps_page())
            {
                frame.hits.push((Rect::new(content.right() - 132., content.y + 4., 132., 30.), Hit::ResetSection));
            }
            let row_height = if row_width < 480. {
                112.
            } else if compact {
                64.
            } else {
                72.
            };
            let advanced = self.settings.page == Some(0) && self.settings.query.is_empty();
            let scroll_top = frame.top;
            let mut offset = 0.;
            let mut rows = Vec::new();
            let mut level_rows = Vec::new();
            let mut current: Option<(&'static str, f64)> = None;
            let mut advanced_at = None;
            for index in &selected {
                let heading = self.settings.heading(*index);
                if current.is_none_or(|(name, _)| name != heading) {
                    if let Some((name, start)) = current.take() {
                        frame.groups.push((name, Rect::new(content.x, start, row_width, offset - start)));
                        offset += 18.;
                    }
                    if heading == "Advanced" && advanced {
                        advanced_at = Some(offset);
                        offset += 44.;
                    } else if !heading.is_empty() {
                        offset += 26.;
                    }
                    current = Some((heading, offset));
                }
                rows.push((Rect::new(content.x, offset, row_width, row_height), *index));
                offset += row_height;
                if inline_levels(*index, &self.settings.query) {
                    for level in 0..self.settings.levels().len() {
                        level_rows.push((Rect::new(content.x, offset, row_width, LEVEL_ROW), level));
                        offset += LEVEL_ROW;
                    }
                }
            }
            if let Some((name, start)) = current {
                frame.groups.push((name, Rect::new(content.x, start, row_width, offset - start)));
            }
            if advanced && advanced_at.is_none() {
                advanced_at = Some(offset + 18.);
                offset += 62.;
            }
            if self.settings.page == Some(5) && self.settings.query.is_empty() {
                let gap = if rows.is_empty() { 0. } else { 20. };
                let home = self.settings_home_layout(content.x, offset + gap, row_width);
                offset += gap + home.height + 20.;
                frame.home = Some(home);
            }
            if self.settings.page == Some(apps_page()) && self.settings.query.is_empty() {
                // Room for the first card's heading.
                let apps = self.settings_apps_layout(content.x, offset + 24., row_width);
                offset += 24. + apps.height + 20.;
                frame.apps = Some(apps);
            }
            if self.settings.page == Some(network_page::network_page()) && self.settings.query.is_empty() {
                let gap = if rows.is_empty() { 0. } else { 20. };
                let network = self.settings_network_layout(content.x, offset + gap, row_width);
                offset += gap + network.height + 20.;
                frame.network = Some(network);
            }
            frame.max_scroll = (offset - viewport(&frame).h + 20.).max(0.);
            let scroll = self.settings.scroll.min(frame.max_scroll);
            let shift = scroll_top - scroll;
            for (_, rect) in &mut frame.groups {
                rect.y += shift;
            }
            if let Some(home) = &mut frame.home {
                home.shift(shift);
            }
            if let Some(apps) = &mut frame.apps {
                apps.shift(shift);
            }
            if let Some(network) = &mut frame.network {
                network.shift(shift);
            }
            let area = viewport(&frame);
            if let Some(top) = advanced_at {
                let rect = Rect::new(content.x, top + shift, row_width, 34.);
                frame.advanced = Some(rect);
                if rect.y >= area.y && rect.bottom() <= area.bottom() {
                    frame.hits.push((rect, Hit::AdvancedAppearance));
                }
            }
            for (mut rect, index) in rows {
                rect.y += shift;
                frame.rows.push((rect, index));
                if rect.bottom() > area.y && rect.y < area.bottom() {
                    let mut control = control_rect(rect);
                    if matches!(FIELDS[index].control, Control::Number(..)) {
                        control.w = 90.;
                    }
                    if control.y >= area.y && control.bottom() <= area.bottom() {
                        let hit =
                            if inline_levels(index, &self.settings.query) { Hit::LevelAdd } else { Hit::Field(index) };
                        frame.hits.push((control, hit));
                        if matches!(FIELDS[index].control, Control::Colour) || FIELDS[index].key == "battery.charging" {
                            frame.hits.push((Rect::new(control.x, control.y, 36., control.h), Hit::Swatch(index)));
                        }
                        if self.settings.draft[index] != self.settings.defaults[index] {
                            frame.hits.push((reset_rect(rect), Hit::Reset(index)));
                        }
                    }
                }
            }
            for (mut rect, level) in level_rows {
                rect.y += shift;
                if rect.bottom() > area.y && rect.y < area.bottom() {
                    frame.levels.push((rect, level));
                    let parts = level_parts(rect);
                    if parts.slider.y >= area.y && parts.slider.bottom() <= area.bottom() {
                        frame.hits.push((parts.slider, Hit::LevelAt(level)));
                        frame.hits.push((parts.swatch, Hit::LevelColour(level)));
                        frame.hits.push((parts.remove, Hit::LevelRemove(level)));
                    }
                }
            }
            if let Some(home) = &frame.home {
                for (rect, hit) in &home.buttons {
                    if rect.y >= area.y && rect.bottom() <= area.bottom() {
                        frame.hits.push((*rect, *hit));
                    }
                }
            }
            if let Some(apps) = &frame.apps {
                let mut hits = Vec::new();
                for (row, index) in &apps.rows {
                    let control = defaults_page::app_control(*row);
                    let usable = self.settings.apps.get(*index).is_some_and(|entry| !entry.choices.is_empty());
                    if usable && control.y >= area.y && control.bottom() <= area.bottom() {
                        hits.push((control, Hit::AppDefault(*index)));
                    }
                }
                if apps.toggle.y >= area.y && apps.toggle.bottom() <= area.bottom() {
                    hits.push((apps.toggle, Hit::AppsAll));
                }
                frame.hits.extend(hits);
            }
            if let Some(network) = &frame.network {
                let hits: Vec<_> = network
                    .hits()
                    .into_iter()
                    .filter(|(rect, _)| rect.y >= area.y && rect.bottom() <= area.bottom())
                    .collect();
                frame.hits.extend(hits);
            }
        }
        if let Some((index, _)) = self.settings.dropdown {
            for (position, rect) in self.settings_dropdown_rects(&frame, index).into_iter().enumerate() {
                frame.hits.push((rect, Hit::Option(index, position)));
            }
        }
        if let Some((entry, _)) = self.settings.apps_menu {
            // Last, so the menu covers what is under it.
            for (position, rect) in self.settings_apps_menu_rects(&frame, entry).into_iter().enumerate() {
                frame.hits.push((rect, Hit::AppChoice(entry, position)));
            }
        }
        frame
    }

    pub fn settings_scroll(&mut self, amount: f64) {
        if self.settings.display_preview.is_some() {
            return;
        }
        self.settings.pressed = None;
        let frame = self.settings_frame();
        let pointer = Point::new(self.settings.pointer.0, self.settings.pointer.1);
        if pointer.x < frame.body.x && pointer.y >= frame.body.y + 70. && pointer.y < frame.body.bottom() - 96. {
            let visible = (frame.body.h - 166.).max(1.);
            self.settings.nav_scroll = (self.settings.nav_scroll + amount).clamp(0., (nav_height() - visible).max(0.));
            self.schedule_render();
            return;
        }
        if self.settings.keymap.is_some() {
            self.keymap_scroll(amount);
            return;
        }
        if self.settings.section.is_some() {
            self.section_scroll(amount);
            return;
        }
        // The page scrolls under the credentials form; its list closes.
        self.settings_connect_scrolled();
        if self.settings.editor.is_some() {
            self.settings_editor_scroll(amount);
            return;
        }
        self.settings.dropdown = None;
        self.settings.drag = None;
        self.settings.colour = None;
        let frame = self.settings_frame();
        if let Some(config) = frame.config
            && config.h > 70.
            && config.contains(Point::new(self.settings.pointer.0, self.settings.pointer.1))
        {
            let count = self.settings_config_lines(true).len();
            let shown = config.h - 42.;
            let max = (count as f64 * 23. - shown).max(0.);
            self.settings.config_scroll = (self.settings.config_scroll + amount).clamp(0., max);
            self.schedule_render();
            return;
        }
        self.settings.scroll = (self.settings.scroll + amount).clamp(0., frame.max_scroll);
        self.schedule_render();
    }

    fn settings_page_click(&mut self, rect: Rect, hit: Hit, point: Point) {
        if self.settings.colour.is_some() {
            // A press elsewhere closes the picker; on its own swatch that is all.
            let open = self.settings.colour.take().map(|picker| picker.index);
            if open.is_some_and(|index| hit == Hit::Swatch(index)) {
                self.schedule_render();
                return;
            }
        }
        if let Hit::Field(index) = hit
            && self.settings.editing == Some(index)
        {
            let inset = if matches!(FIELDS[index].control, Control::Colour) { 34. } else { 10. };
            self.settings.input.click(
                &mut self.text.borrow_mut(),
                &self.theme.font,
                11.,
                (rect.w - inset - 8.) as f32,
                (point.x - rect.x - inset).max(0.) as f32,
                false,
            );
            self.schedule_render();
            return;
        }
        if let Some((index, _)) = self.settings.dropdown
            && !matches!(hit, Hit::Option(..))
        {
            self.settings.dropdown = None;
            if hit == Hit::Field(index) {
                self.schedule_render();
                return;
            }
        }
        if let Some((entry, _)) = self.settings.apps_menu
            && !matches!(hit, Hit::AppChoice(..))
        {
            self.settings.apps_menu = None;
            if hit == Hit::AppDefault(entry) {
                self.schedule_render();
                return;
            }
        }
        self.settings_activate(hit, Some((point.x - rect.x) / rect.w));
        if let Hit::LevelAt(level) = hit {
            self.settings.level_drag = Some((level, rect));
        }
        if let Hit::Field(index) = hit
            && matches!(FIELDS[index].control, Control::Number(..))
            && self.settings.editing.is_none()
        {
            self.settings.drag = Some((index, rect));
        }
        self.schedule_render();
    }

    pub fn settings_motion(&mut self, point: Point) {
        if self.settings.text_drag {
            self.settings_text_caret(point, true);
            return;
        }
        if self.settings.colour.as_ref().is_some_and(|picker| picker.drag.is_some()) {
            self.colour_motion(point);
            return;
        }
        if self.settings.section.is_some() {
            self.section_map_motion(point);
            return;
        }
        if let Some((index, rect)) = self.settings.drag {
            self.settings_activate(Hit::Field(index), Some((point.x - rect.x) / rect.w));
        }
        if let Some((level, rect)) = self.settings.level_drag {
            self.settings_activate(Hit::LevelAt(level), Some((point.x - rect.x) / rect.w));
        }
    }

    pub fn settings_release(&mut self) {
        self.settings.pressed = None;
        self.settings.text_drag = false;
        if let Some(picker) = &mut self.settings.colour {
            picker.drag = None;
        }
        self.section_map_release();
        self.settings.drag = None;
        self.settings.level_drag = None;
    }

    fn settings_finish_edit(&mut self) -> bool {
        let Some(index) = self.settings.editing else { return true };
        let text = self.settings.input.text.trim();
        if matches!(FIELDS[index].control, Control::Colour) && Colour::parse(text).is_err() {
            self.settings.message = "Enter a colour as #rrggbb or #rrggbbaa, or press Escape to cancel.".into();
            self.settings.error = true;
            return false;
        }
        self.settings.draft[index] =
            if matches!(FIELDS[index].control, Control::Text | Control::Keyboard) && text.is_empty() {
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
        self.settings_connect_release();
        self.settings_network_release(hit);
        if hit == Hit::Discard {
            self.settings.editing = None;
        } else if !self.settings_finish_edit() {
            return;
        }
        self.settings.focus = Some(hit);
        if !matches!(hit, Hit::Field(_) | Hit::Option(..)) {
            self.settings.dropdown = None;
        }
        if !matches!(hit, Hit::AppDefault(_) | Hit::AppChoice(..)) {
            self.settings.apps_menu = None;
        }
        match hit {
            Hit::Close => self.toggle_settings(),
            Hit::Search => {
                if self.settings.search_input.text != self.settings.query {
                    self.settings.search_input = crate::text_input::TextInput::new(self.settings.query.clone());
                }
            }
            Hit::EditDocument => self.open_settings_editor(None, false),
            Hit::Reference => self.open_settings_editor(None, true),
            Hit::ReviewChanges => {
                self.settings.review_changes = !self.settings.review_changes;
                self.settings.config_scroll = 0.;
            }
            Hit::AdvancedAppearance => self.settings.advanced_appearance = !self.settings.advanced_appearance,
            Hit::HomeGuide => self.settings.home_guide = !self.settings.home_guide,
            Hit::HomeTrouble => self.settings.home_trouble = !self.settings.home_trouble,
            Hit::HomeLink(index) => {
                if let Some((_, url)) = home_page::LINKS.get(index) {
                    self.spawn("xdg-open", &[(*url).to_string()]);
                }
            }
            Hit::HomeRefresh => {
                self.open_settings_connect();
                // With a saved secret there is nothing to ask: refresh at once.
                if self.settings.connect.as_ref().is_some_and(|form| form.has_stored_secret()) {
                    self.start_settings_connect();
                }
                // The form opens at the bottom of the page: bring it into view.
                self.settings.scroll = self.settings_frame().max_scroll;
            }
            Hit::AppDefault(entry) => {
                if self.settings.apps_menu.is_some_and(|(open, _)| open == entry) {
                    self.settings.apps_menu = None;
                } else if let Some(data) = self.settings.apps.get(entry) {
                    let (shown, _) = self.settings_shown_app(data);
                    let current = shown
                        .and_then(|(app_id, _)| data.choices.iter().position(|(choice, _)| *choice == app_id))
                        .unwrap_or(0);
                    self.settings.apps_menu = Some((entry, current));
                }
            }
            Hit::AppChoice(entry, choice) => {
                self.settings_choose_app(entry, choice);
                self.settings.focus = Some(Hit::AppDefault(entry));
            }
            Hit::AppsAll => {
                self.settings.apps_all = !self.settings.apps_all;
                self.settings_load_apps();
            }
            Hit::NetWifi
            | Hit::NetVisible(_)
            | Hit::NetAuto(_)
            | Hit::NetForget(_)
            | Hit::NetName
            | Hit::NetPassword
            | Hit::NetReveal
            | Hit::NetJoin
            | Hit::NetEditor => self.settings_network_activate(hit),
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
                        self.settings.input = crate::text_input::TextInput::new(text);
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
                self.settings.search_input.clear();
                self.settings.scroll = 0.;
                self.settings.config_scroll = 0.;
                self.settings.colour = None;
                self.open_settings_page_section();
            }
            Hit::Discard => {
                self.settings.parked_sections.clear();
                self.settings.parked_editors.clear();
                self.settings_discard_apps();
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
                if !self.settings.parked_sections.is_empty() {
                    self.settings.section = Some(self.settings.parked_sections.remove(0));
                    self.settings.message = "Finish the saved screen draft before applying.".into();
                    self.schedule_render();
                    return;
                }
                if !self.settings.parked_editors.is_empty() {
                    self.settings.editor = Some(self.settings.parked_editors.remove(0));
                    self.settings.editor_epoch = self.settings.editor_epoch.wrapping_add(1);
                    self.settings.message = "Finish the saved editor draft before applying.".into();
                    self.schedule_render();
                    return;
                }
                // Default apps go to mimeapps.list, before config.toml.
                match self.settings_apply_apps() {
                    Ok(0) => {}
                    Ok(_) => {
                        self.settings.message = "Default apps applied.".into();
                        self.settings.error = false;
                    }
                    Err(error) => {
                        self.settings.message = error;
                        self.settings.error = true;
                        self.schedule_render();
                        return;
                    }
                }
                match self.settings_start_display_preview() {
                    Ok(true) => return,
                    Ok(false) => {}
                    Err(error) => {
                        self.settings.message = error;
                        self.settings.error = true;
                        self.schedule_render();
                        return;
                    }
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
                let indices = if let Hit::Reset(index) = hit {
                    vec![index]
                } else if self.settings.query.is_empty() {
                    FIELDS
                        .iter()
                        .enumerate()
                        .filter(|(_, field)| self.settings.page == Some(field.category))
                        .map(|(index, _)| index)
                        .collect()
                } else {
                    self.settings.selected()
                };
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
            Hit::Swatch(index) => {
                self.open_colour_picker(index);
                self.settings.focus = Some(Hit::Field(index));
            }
            Hit::LevelAdd => {
                let mut levels = self.settings.levels();
                let lowest = levels.iter().map(|(threshold, _)| *threshold).min();
                levels.push((lowest.map_or(20, |threshold| (threshold - 10).clamp(1, 99)), "warning".into()));
                self.settings.set_levels(&levels);
            }
            Hit::LevelRemove(level) => {
                let mut levels = self.settings.levels();
                if level < levels.len() {
                    levels.remove(level);
                    self.settings.set_levels(&levels);
                }
                self.settings.colour = None;
            }
            Hit::LevelColour(level) => self.open_level_picker(level),
            Hit::LevelAt(level) => {
                if let Some(fraction) = position {
                    let mut levels = self.settings.levels();
                    if let Some(entry) = levels.get_mut(level) {
                        entry.0 = (fraction.clamp(0., 1.) * 100.).round() as i64;
                        self.settings.set_levels(&levels);
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
                    Control::Section => {
                        self.open_settings_section(index);
                        return;
                    }
                    Control::Keyboard => {
                        self.open_keymap_picker(index);
                        return;
                    }
                    Control::Number(min, max, step, _) => {
                        let current = self.settings.number(FIELDS[index].key, min);
                        let min = min.min(current);
                        let max = max.max(current);
                        let number =
                            position.map_or(current + step, |fraction| min + fraction.clamp(0., 1.) * (max - min));
                        let number = ((number / step).round() * step).clamp(min, max);
                        if FIELDS[index].key.starts_with("keyboard.")
                            || matches!(value, Some(toml::Value::Integer(_))) && step.fract() == 0.
                        {
                            toml::Value::Integer(number.round() as i64)
                        } else {
                            toml::Value::Float((number * 1000.).round() / 1000.)
                        }
                    }
                    Control::Text | Control::Colour => {
                        let text = value.and_then(toml::Value::as_str).unwrap_or("").to_string();
                        self.settings.input = crate::text_input::TextInput::new(text);
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
        self.settings.pressed = None;
        self.invalidate_settings_client();
        if self.settings_display_preview_key(sym, mods) {
            return;
        }
        let page_navigation = self.settings.editor.is_none()
            && !self.settings_connect_typing()
            && !self.settings_network_typing()
            && self.settings.keymap.is_none();
        if !page_navigation && mods.alt && sym == keysyms::KEY_Up {
            self.settings_key(keysyms::KEY_Escape, Mods::default());
            return;
        }
        if page_navigation && mods.alt && sym == keysyms::KEY_Up {
            if self.settings.section.is_some() && !self.section_is_page() {
                self.section_keep_back();
            } else {
                self.settings_leave_section();
                self.settings_activate(Hit::Page(None), None);
            }
            return;
        }
        if page_navigation && mods.ctrl && matches!(sym, keysyms::KEY_Page_Up | keysyms::KEY_Page_Down) {
            if !self.settings_finish_edit() {
                return;
            }
            self.settings_leave_section();
            let page = self.settings.page.map_or(
                if sym == keysyms::KEY_Page_Down { 0 } else { CATEGORIES.len() - 1 },
                |page| {
                    if sym == keysyms::KEY_Page_Down {
                        (page + 1) % CATEGORIES.len()
                    } else {
                        (page + CATEGORIES.len() - 1) % CATEGORIES.len()
                    }
                },
            );
            self.settings.nav_scroll = (page as f64 * 40. - (self.settings_frame().body.h - 206.)).max(0.);
            self.settings_activate(Hit::Page(Some(page)), None);
            self.schedule_render();
            return;
        }
        if self.settings.keymap.is_some() {
            self.keymap_key(sym, mods);
            return;
        }
        if self.settings.section.is_some() {
            self.section_key(sym, mods);
            return;
        }
        if self.settings_connect_key(sym, mods) {
            return;
        }
        if self.settings_network_key(sym, mods) {
            return;
        }
        if self.settings.editor.is_some() {
            self.settings_editor_key(sym, mods);
            return;
        }
        if self.settings.colour.is_some() && matches!(sym, keysyms::KEY_Escape | keysyms::KEY_Return | keysyms::KEY_Tab)
        {
            self.settings.colour = None;
            self.schedule_render();
            return;
        }
        if let Some((entry, selected)) = self.settings.apps_menu {
            let count = self.settings.apps.get(entry).map_or(0, |data| data.choices.len()).max(1);
            match sym {
                keysyms::KEY_Escape | keysyms::KEY_Tab => self.settings.apps_menu = None,
                keysyms::KEY_Up => self.settings.apps_menu = Some((entry, (selected + count - 1) % count)),
                keysyms::KEY_Down => self.settings.apps_menu = Some((entry, (selected + 1) % count)),
                keysyms::KEY_Return | keysyms::KEY_space => {
                    self.settings_activate(Hit::AppChoice(entry, selected), None)
                }
                _ => {}
            }
            self.schedule_render();
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
                self.settings.search_input.clear();
                self.settings.scroll = 0.;
            } else {
                self.toggle_settings();
            }
        } else if mods.ctrl && matches!(sym, keysyms::KEY_f | keysyms::KEY_F) {
            if self.settings_finish_edit() {
                self.settings_activate(Hit::Search, None);
            }
        } else if mods.ctrl && matches!(sym, keysyms::KEY_s | keysyms::KEY_S) {
            self.settings_activate(Hit::Apply, None);
        } else if mods.ctrl && matches!(sym, keysyms::KEY_v | keysyms::KEY_V) {
            self.paste(crate::launcher::Paste::Settings(self.settings.editor_epoch));
        } else if matches!(sym, keysyms::KEY_Page_Down | keysyms::KEY_Page_Up) {
            self.settings_scroll(if sym == keysyms::KEY_Page_Down { 162. } else { -162. });
        } else if sym == keysyms::KEY_Tab || sym == keysyms::KEY_ISO_Left_Tab {
            if self.settings_finish_edit() {
                let frame = self.settings_frame();
                let mut hits: Vec<_> = frame
                    .hits
                    .iter()
                    .copied()
                    .filter(|(_, hit)| !matches!(hit, Hit::Field(_) | Hit::Swatch(_) | Hit::Reset(_)))
                    .filter(|(_, hit)| !matches!(hit, Hit::Apply | Hit::Discard) || self.settings.changed() > 0)
                    .collect();
                hits.extend(frame.rows.iter().map(|(rect, index)| (control_rect(*rect), Hit::Field(*index))));
                let current = hits.iter().position(|(_, hit)| Some(*hit) == self.settings.focus);
                let next = if mods.shift || sym == keysyms::KEY_ISO_Left_Tab {
                    current.map_or(hits.len() - 1, |index| (index + hits.len() - 1) % hits.len())
                } else {
                    current.map_or(0, |index| (index + 1) % hits.len())
                };
                self.settings.focus = Some(hits[next].1);
                if matches!(hits[next].1, Hit::Field(_)) {
                    let area = viewport(&frame);
                    let rect = hits[next].0;
                    self.settings.scroll = (self.settings.scroll
                        + if rect.y < area.y { rect.y - area.y } else { (rect.bottom() - area.bottom()).max(0.) })
                    .clamp(0., frame.max_scroll);
                }
            }
        } else if self.settings.editing.is_some() || self.settings.focus == Some(Hit::Search) {
            if sym == keysyms::KEY_Return {
                self.settings_finish_edit();
            } else if self.settings.editing.is_some() {
                let key = self.settings.input.key(&mut self.settings.compose, sym, mods);
                self.field_clipboard(key.clipboard, crate::launcher::Paste::Settings(self.settings.editor_epoch));
            } else {
                let key = self.settings.search_input.key(&mut self.settings.compose, sym, mods);
                self.field_clipboard(key.clipboard, crate::launcher::Paste::Settings(self.settings.editor_epoch));
                self.settings.query.clone_from(&self.settings.search_input.text);
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
                            || matches!(self.settings.draft[index], Some(toml::Value::Integer(_))) && step.fract() == 0.
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

    pub(crate) fn settings_elements(&self, renderer: &mut GlesRenderer) -> Vec<OutputElement> {
        if !self.settings.open || self.settings_rect().is_none() {
            return Vec::new();
        }
        let frame = self.settings_frame();
        let theme = &self.theme;
        let state = &self.settings;
        let pointer = Point::new(self.settings.pointer.0, self.settings.pointer.1);
        let compact = frame.body.h < 640.;
        let changes = state.changed();
        let mut painter = Painter::new(self, renderer);
        painter.shape(
            frame.panel,
            Shape { border_width: 1., border: theme.border, ..Shape::filled(theme.background, theme.radius) },
        );
        painter.fill(
            Rect::new(frame.panel.x + 1., frame.panel.y + 1., frame.panel.w - 2., 63.),
            theme.surface,
            (theme.radius - 1.).max(0.),
        );
        let title = Rect::new(frame.panel.x + 24., frame.panel.y + 15., 24., 34.);
        painter.glyph(glyph::SETTINGS, title, 22, theme.accent);
        painter.text(
            "Settings",
            Rect::new(title.right() + 12., title.y, 140., title.h),
            Style::new(15., theme.text).bold(),
        );
        painter.fill(Rect::new(frame.panel.x, frame.body.y, frame.panel.w, 1.), theme.border, 0.);
        painter.fill(Rect::new(frame.body.x, frame.body.y, 1., frame.body.h), theme.border, 0.);
        painter.fill(Rect::new(frame.panel.x, frame.panel.bottom() - 60., frame.panel.w, 1.), theme.border, 0.);
        let heading = if !state.query.is_empty() {
            "Search results"
        } else {
            state.page.map_or("Overview", |index| CATEGORIES[index].title)
        };
        painter.text(
            heading,
            Rect::new(frame.content.x, frame.content.y, frame.content.w - 150., if compact { 30. } else { 38. }),
            Style::new(if compact { 22. } else { 27. }, theme.text).weight(500),
        );
        let subtitle = if !state.query.is_empty() {
            match state.selected().len() {
                0 => format!("Nothing matches \"{}\".", state.query),
                1 => format!("1 setting matches \"{}\".", state.query),
                count => format!("{count} settings match \"{}\".", state.query),
            }
        } else if state.page == Some(5) && !state.home_ready {
            "Not set up yet: follow the three steps below.".into()
        } else if state.page == Some(5) {
            self.settings_home_status()
        } else {
            state
                .page
                .map_or("Changes stay a draft until you apply them.", |index| CATEGORIES[index].description)
                .into()
        };
        painter.text(
            &subtitle,
            Rect::new(frame.content.x, frame.content.y + if compact { 30. } else { 42. }, frame.content.w - 150., 22.),
            Style::new(12., theme.muted),
        );
        painter.fill(
            Rect::new(frame.panel.x + 24., frame.body.y + 73., frame.body.x - frame.panel.x - 48., 1.),
            theme.border,
            0.,
        );
        sections::draw_scrollbar(
            &mut painter,
            Rect::new(
                frame.panel.x + 8.,
                frame.body.y + 70.,
                frame.body.x - frame.panel.x - 12.,
                (frame.body.h - 166.).max(1.),
            ),
            state.nav_scroll,
            (nav_height() - (frame.body.h - 166.).max(1.)).max(0.),
            theme,
        );
        painter.clipped(viewport(&frame), |painter| {
            if let Some(home) = &frame.home {
                self.settings_draw_home_page(painter, home);
            }
            if let Some(apps) = &frame.apps {
                self.settings_draw_apps_page(painter, apps, pointer);
            }
            if let Some(network) = &frame.network {
                self.settings_draw_network_page(painter, network, pointer);
            }
        });
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
                    let (label, icon) = page.map_or(("Overview", glyph::OVERVIEW), |index| {
                        (CATEGORIES[index].title, CATEGORIES[index].icon)
                    });
                    let colour = if active {
                        theme.accent
                    } else if hovered {
                        theme.text
                    } else {
                        theme.muted
                    };
                    painter.glyph(icon, Rect::new(rect.x + 10., rect.y, 20., rect.h), 18, colour);
                    if rect.w > 60. {
                        painter.text(
                            label,
                            Rect::new(rect.x + 40., rect.y, rect.w - 52., rect.h),
                            Style::new(12., colour),
                        );
                    }
                    if page.is_some_and(|index| state.category_changed(index)) {
                        painter.fill(
                            Rect::new(rect.right() - 10., rect.y + rect.h / 2. - 3., 6., 6.),
                            theme.accent,
                            3.,
                        );
                    }
                }
                Hit::EditDocument | Hit::Reference => {
                    if hovered {
                        painter.fill(*rect, theme.surface, 6.);
                    }
                    let (label, icon) = if *hit == Hit::EditDocument {
                        ("Edit config.toml", glyph::FILE)
                    } else {
                        ("Option reference", glyph::SEARCH)
                    };
                    let colour = if hovered { theme.text } else { theme.muted };
                    painter.glyph(icon, Rect::new(rect.x + 10., rect.y, 20., rect.h), 16, colour);
                    if rect.w > 60. {
                        painter.text(
                            label,
                            Rect::new(rect.x + 40., rect.y, rect.w - 44., rect.h),
                            Style::new(11., colour),
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
                    if !focused || state.query.is_empty() {
                        painter.text(
                            label,
                            Rect::new(rect.x + 35., rect.y, rect.w - 45., rect.h),
                            Style::new(12., if state.query.is_empty() { theme.muted } else { theme.text }),
                        );
                    }
                    if focused
                        && let Some(image) = self.text.borrow_mut().field(
                            &state.search_input.text,
                            &theme.font,
                            12.,
                            theme.text,
                            (rect.w - 45.) as f32,
                            crate::text::Field {
                                cursor: state.search_input.cursor,
                                anchor: state.search_input.anchor,
                                visible: true,
                                accent: theme.accent.straight_rgba8(),
                            },
                        )
                    {
                        painter.image(&image.buffer, (rect.x + 35., rect.y + (rect.h - image.height) / 2.));
                    }
                }
                Hit::Close => {
                    if hovered {
                        painter.fill(*rect, theme.surface_raised, 6.);
                    }
                    painter.glyph(glyph::CLOSE, *rect, 18, if hovered { theme.text } else { theme.muted });
                }
                Hit::HomeCloud(sensors) => {
                    let label = &state.home_cloud_labels[usize::from(!sensors)];
                    let enabled = label.ends_with(": on");
                    painter.shape(
                        *rect,
                        Shape { border_width: 1., border: theme.border, ..Shape::filled(theme.surface, 6.) },
                    );
                    painter.text(
                        label.split(':').next().unwrap_or(label),
                        Rect::new(rect.x + 12., rect.y, rect.w - 60., rect.h),
                        Style::new(11., theme.text),
                    );
                    let switch = Rect::new(rect.right() - 46., rect.y + 7., 34., 20.);
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
                Hit::HomeGuide | Hit::HomeTrouble | Hit::HomeLink(_) | Hit::HomeRefresh => {
                    let (label, unfolded) = self.settings_home_label(*hit);
                    let primary = matches!(hit, Hit::HomeRefresh);
                    painter.shape(
                        *rect,
                        Shape {
                            border_width: 1.,
                            border: if focused {
                                theme.accent
                            } else if hovered {
                                theme.muted
                            } else {
                                theme.border
                            },
                            ..Shape::filled(if primary { theme.surface_raised } else { theme.surface }, 6.)
                        },
                    );
                    let text = Rect::new(rect.x + 12., rect.y, rect.w - 36., rect.h);
                    painter.text(&label, text, Style::new(11.5, theme.text));
                    let icon = match unfolded {
                        Some(true) => glyph::CHEVRON_UP,
                        Some(false) => glyph::CHEVRON_DOWN,
                        None if matches!(hit, Hit::HomeLink(_)) => glyph::ARROW_RIGHT,
                        None => glyph::RESTART,
                    };
                    painter.glyph(icon, Rect::new(rect.right() - 28., rect.y, 20., rect.h), 13, theme.muted);
                }
                Hit::ReviewChanges if state.section.is_some() => {}
                Hit::ReviewChanges => {
                    if hovered || state.review_changes {
                        painter.fill(*rect, theme.surface, 6.);
                    }
                    painter.glyph(
                        if state.review_changes { glyph::CHEVRON_DOWN } else { glyph::CHEVRON_UP },
                        Rect::new(rect.x + 6., rect.y, 18., rect.h),
                        14,
                        theme.muted,
                    );
                    painter.text(
                        "Review changes",
                        Rect::new(rect.x + 30., rect.y, rect.w - 34., rect.h),
                        Style::new(12., theme.text).weight(500),
                    );
                }
                Hit::Apply | Hit::Discard | Hit::ResetSection => {
                    let enabled = *hit == Hit::ResetSection || changes > 0;
                    let primary = *hit == Hit::Apply && enabled;
                    let fill = if primary {
                        (theme.accent, theme.accent_end)
                    } else if *hit == Hit::ResetSection && !hovered {
                        (Colour::TRANSPARENT, Colour::TRANSPARENT)
                    } else {
                        (theme.surface_raised, theme.surface_raised)
                    };
                    painter.shape(
                        *rect,
                        Shape {
                            fill,
                            radius: 6.,
                            border_width: if primary { 0. } else { 1. },
                            border: if hovered && enabled { theme.muted } else { theme.border },
                            ..Default::default()
                        },
                    );
                    let label = match hit {
                        Hit::Apply => "Apply changes",
                        Hit::Discard => "Discard",
                        _ => "Restore defaults",
                    };
                    painter.text(
                        label,
                        *rect,
                        Style::new(
                            if primary { 12. } else { 11. },
                            if primary {
                                theme.on_accent
                            } else if enabled {
                                theme.text
                            } else {
                                theme.muted.alpha(0.6)
                            },
                        )
                        .weight(if primary { 600 } else { 400 })
                        .align(Align::Centre),
                    );
                }
                _ => {}
            }
            if focused
                && !matches!(hit, Hit::Search | Hit::Card(_) | Hit::Field(_) | Hit::Reset(_) | Hit::AdvancedAppearance)
                && (!matches!(hit, Hit::Apply | Hit::Discard) || changes > 0)
            {
                painter
                    .shape(*rect, Shape { radius: 6., border_width: 1., border: theme.accent, ..Default::default() });
            }
        }
        painter.clipped(viewport(&frame), |painter| {
            for (rect, index) in &frame.cards {
                self.settings_card(painter, *rect, *index, pointer);
            }
            for (name, rect) in &frame.groups {
                if !name.is_empty() && *name != "Advanced" {
                    painter.text(
                        &name.to_uppercase(),
                        Rect::new(rect.x + 2., rect.y - 24., rect.w, 20.),
                        Style::new(10., theme.muted).weight(600),
                    );
                }
                painter
                    .shape(*rect, Shape { border_width: 1., border: theme.border, ..Shape::filled(theme.surface, 8.) });
            }
            for (rect, index) in &frame.rows {
                self.settings_row(painter, *rect, *index);
            }
            let levels = state.levels();
            for (rect, level) in &frame.levels {
                let Some((threshold, colour)) = levels.get(*level) else { continue };
                self.settings_level(painter, *rect, *level, *threshold, colour);
            }
            if let Some(rect) = frame.advanced {
                let hovered = rect.contains(pointer);
                painter.shape(
                    rect,
                    Shape {
                        border_width: 1.,
                        border: if state.focus == Some(Hit::AdvancedAppearance) { theme.accent } else { theme.border },
                        ..Shape::filled(if hovered { theme.surface_raised } else { theme.surface }, 6.)
                    },
                );
                painter.glyph(
                    if state.advanced_appearance { glyph::CHEVRON_DOWN } else { glyph::CHEVRON_RIGHT },
                    Rect::new(rect.x + 10., rect.y + 8., 18., 18.),
                    16,
                    theme.muted,
                );
                painter.text(
                    if state.advanced_appearance { "Fewer options" } else { "More colours and options" },
                    Rect::new(rect.x + 36., rect.y, rect.w - 44., rect.h),
                    Style::new(12., theme.text),
                );
            }
        });
        if (state.page.is_some() || !state.query.is_empty())
            && state.selected().is_empty()
            && frame.home.is_none()
            && frame.apps.is_none()
            && frame.network.is_none()
        {
            painter.text(
                "Try a word such as accent, gap, bar or battery.",
                Rect::new(frame.content.x, frame.top + 8., frame.content.w, 30.),
                Style::new(13., theme.muted),
            );
        }
        if let Some(rect) = frame.preview {
            painter.text(
                "LIVE PREVIEW",
                Rect::new(rect.x + 2., rect.y - 24., rect.w, 20.),
                Style::new(10., theme.muted).weight(600),
            );
            self.settings_preview(&mut painter, rect);
        }
        if let Some(rect) = frame.config {
            self.settings_config(&mut painter, rect);
        }
        if frame.max_scroll > 0. {
            let area = viewport(&frame);
            let track = Rect::new(frame.body.right() - 9., area.y, 3., area.h.max(12.));
            let height = (track.h * track.h / (track.h + frame.max_scroll)).max(22.).min(track.h);
            painter.fill(
                Rect::new(
                    track.x,
                    track.y + (track.h - height) * state.scroll.min(frame.max_scroll) / frame.max_scroll,
                    3.,
                    height,
                ),
                theme.muted.alpha(0.5),
                2.,
            );
        }
        let status_left = frame.panel.x + if changes > 0 { 182. } else { 24. };
        let status = if state.section.is_some() {
            String::new()
        } else if !state.message.is_empty() {
            state.message.clone()
        } else if changes > 0 && state.document_draft != state.document_before {
            "Unapplied changes".into()
        } else if changes == 1 {
            "1 unapplied change".into()
        } else if changes > 1 {
            format!("{changes} unapplied changes")
        } else {
            String::new()
        };
        if changes > 0 && state.message.is_empty() && state.section.is_none() {
            painter.fill(Rect::new(status_left, frame.panel.bottom() - 33., 6., 6.), theme.accent, 3.);
        }
        painter.text(
            &status,
            Rect::new(
                status_left + if changes > 0 && state.message.is_empty() { 14. } else { 0. },
                frame.panel.bottom() - 53.,
                (frame.panel.right() - 320. - status_left).max(1.),
                46.,
            ),
            Style::new(11., if state.error { theme.danger } else { theme.muted }).lines(2),
        );
        if let Some((index, highlighted)) = state.dropdown {
            let options = state.options(index);
            let rects = self.settings_dropdown_rects(&frame, index);
            let current = state.draft[index].as_ref().and_then(toml::Value::as_str).unwrap_or("");
            if let (Some(first), Some(last)) = (rects.first(), rects.last()) {
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
            }
            for (position, rect) in rects.into_iter().enumerate() {
                if position == highlighted || rect.contains(pointer) {
                    painter.fill(rect, theme.surface_raised, 5.);
                }
                let chosen = options[position].eq_ignore_ascii_case(current)
                    || current.is_empty() && FIELDS[index].key == "touchpad.click_method" && position == 0;
                painter.text(
                    &choice_label(FIELDS[index].key, &options[position]),
                    Rect::new(rect.x + 12., rect.y, rect.w - 40., rect.h),
                    Style::new(12., if chosen { theme.accent } else { theme.text }),
                );
                if chosen {
                    painter.glyph(glyph::CHECK, Rect::new(rect.right() - 28., rect.y, 18., rect.h), 14, theme.accent);
                }
            }
        }
        self.settings_draw_apps_menu(&mut painter, &frame, pointer);
        self.draw_colour_picker(&mut painter, &frame);
        self.settings_draw_editor(&mut painter, &frame);
        self.section_draw(&mut painter);
        self.settings_pointer_feedback(&mut painter);
        self.draw_keymap_picker(&mut painter);
        self.settings_draw_display_preview(&mut painter);
        painter.finish()
    }

    /// An overview tile: the category and a few of its current settings.
    fn settings_card(&self, painter: &mut Painter, rect: Rect, index: usize, pointer: Point) {
        let theme = &self.theme;
        let category = &CATEGORIES[index];
        let hovered = rect.contains(pointer);
        painter.shape(
            rect,
            Shape {
                border_width: 1.,
                border: if hovered || self.settings.focus == Some(Hit::Card(index)) {
                    theme.accent.alpha(0.8)
                } else {
                    theme.border
                },
                ..Shape::filled(if hovered { theme.surface_raised } else { theme.surface }, 8.)
            },
        );
        painter.shape(
            Rect::new(rect.x + 16., rect.y + 16., 36., 36.),
            Shape { fill: (theme.accent.alpha(0.16), theme.accent_end.alpha(0.08)), radius: 8., ..Default::default() },
        );
        painter.glyph(category.icon, Rect::new(rect.x + 16., rect.y + 16., 36., 36.), 20, theme.accent);
        painter.text(
            category.title,
            Rect::new(rect.x + 64., rect.y + 13., rect.w - 110., 24.),
            Style::new(15., theme.text).weight(500),
        );
        painter.text(
            category.summary,
            Rect::new(rect.x + 64., rect.y + 35., rect.w - 110., 18.),
            Style::new(11., theme.muted),
        );
        painter.glyph(
            glyph::ARROW_RIGHT,
            Rect::new(rect.right() - 36., rect.y + 18., 20., 24.),
            16,
            if hovered { theme.accent } else { theme.muted },
        );
        if self.settings.category_changed(index) {
            painter.fill(Rect::new(rect.right() - 50., rect.y + 27., 6., 6.), theme.accent, 3.);
        }
        if index == 0 && rect.h > 250. {
            self.settings_preview(painter, Rect::new(rect.x + 16., rect.y + 68., rect.w - 32., rect.h - 84.));
            return;
        }
        let lines = self.settings_overview(index);
        let columns = if rect.w > 640. { 2 } else { 1 };
        let width = (rect.w - 32. - (columns - 1) as f64 * 32.) / columns as f64;
        let per_column = lines.len().div_ceil(columns);
        for (position, (label, value)) in lines.iter().enumerate() {
            let column = position / per_column;
            let top = rect.y + 70. + (position % per_column) as f64 * 24.;
            if top + 22. > rect.bottom() - 6. {
                continue;
            }
            let left = rect.x + 16. + column as f64 * (width + 32.);
            painter.fill(Rect::new(left, top + 23., width, 1.), theme.border.alpha(0.5), 0.);
            painter.text(label, Rect::new(left, top, width * 0.55, 22.), Style::new(11., theme.muted));
            let style = Style::new(11., theme.text);
            let (value_width, _) = painter.measure(value, width * 0.6, style);
            painter.text(value, Rect::new(left + width - value_width, top, value_width + 1., 22.), style);
        }
    }

    /// A few current values per category for the overview.
    fn settings_overview(&self, category: usize) -> Vec<(String, String)> {
        let state = &self.settings;
        let switch = |key: &str| if state.flag(key) { "On" } else { "Off" }.to_string();
        let text = |key: &str, empty: &str| {
            let value = state.text(key);
            if value.is_empty() { empty.to_string() } else { choice_label(key, value) }
        };
        let number = |key: &str, unit: &str| number_label(key, state.number(key, 0.), unit);
        let count = |key: &str, one: &str, many: &str| {
            let count = state.value(key).map_or(0, |value| {
                value.as_array().map(Vec::len).or_else(|| value.as_table().map(toml::Table::len)).unwrap_or(0)
            });
            match count {
                0 => "None".to_string(),
                1 => format!("1 {one}"),
                count => format!("{count} {many}"),
            }
        };
        let lines = match category {
            0 => vec![
                ("Accent", text("theme.accent", "Default")),
                ("Font", format!("{} {} pt", text("theme.font", "Default"), state.number("theme.font_size", 13.))),
                ("Frosted panels", switch("theme.frost")),
            ],
            1 => vec![
                ("Window spacing", number("general.gap", "px")),
                ("Animation", number("general.animation_ms", "ms")),
                ("Drag modifier", text("general.drag_modifier", "Alt")),
            ],
            2 => vec![
                ("Bar", text("shell.bar", "Built-in")),
                ("Launcher", text("shell.launcher", "Built-in")),
                ("Notifications", text("shell.notifications", "Built-in")),
            ],
            3 => vec![
                ("Layout", text("keyboard.layout", "System default")),
                ("Repeat delay", number("keyboard.repeat_delay", "ms")),
                ("Repeat rate", number("keyboard.repeat_rate", "/s")),
                ("Tap to click", switch("touchpad.tap")),
                ("Natural scrolling", switch("touchpad.natural_scroll")),
                ("Pointer speed", number("touchpad.speed", "")),
            ],
            4 => vec![
                ("Lock after", number("idle.lock_after", "")),
                ("Screen off after", number("idle.off_after", "")),
                ("Power saver at", number("battery.power_saver_at", "%")),
            ],
            5 => vec![
                ("In the panel", switch("home.enabled")),
                ("Status", if self.settings_home_ready() { self.settings_home_status() } else { "Not set up".into() }),
            ],
            6 => vec![
                ("Locker", text("shell.lock", "Built-in")),
                ("Suspend", if state.flag("general.suspend_enabled") { "Allowed" } else { "Hidden" }.into()),
                ("Login screen look", switch("greeter.share")),
            ],
            7 => vec![
                ("Built-in shortcuts", switch("general.default_binds")),
                ("Shortcuts in config", count("binds", "shortcut", "shortcuts")),
                ("Autostart", count("general.autostart", "command", "commands")),
                ("Terminal", text("general.terminal", "System default")),
            ],
            8 => vec![
                ("Displays", format!("{} connected", self.all_outputs().len())),
                ("Full-edge crossing", switch("general.stretch_screen_edges")),
            ],
            _ => vec![("Sharing", switch("handoff.enabled")), ("Computers", self.handoff.peers.len().to_string())],
        };
        lines.into_iter().map(|(label, value)| (label.to_string(), value)).collect()
    }

    pub(crate) fn settings_identify_elements(&self, renderer: &mut GlesRenderer) -> Vec<OutputElement> {
        if !self.settings.open || self.settings.identify_until.is_none_or(|until| until <= std::time::Instant::now()) {
            return Vec::new();
        }
        let Some(output) = &self.output else { return Vec::new() };
        let screen = self.output_rect();
        let width = screen.w.min(320.);
        let rect = Rect::new(screen.x + (screen.w - width) / 2., screen.y + 40., width, 108.);
        let mut painter = Painter::new(self, renderer);
        painter.shape(
            rect,
            Shape { border_width: 2., border: self.theme.accent, ..Shape::filled(self.theme.background, 8.) },
        );
        let number = self.all_outputs().iter().position(|candidate| candidate == output).unwrap_or(0) + 1;
        painter.text(
            &number.to_string(),
            Rect::new(rect.x, rect.y + 6., rect.w, 60.),
            Style::new(36., self.theme.accent).weight(500).align(Align::Centre),
        );
        painter.text(
            &output.name(),
            Rect::new(rect.x, rect.y + 68., rect.w, 28.),
            Style::new(14., self.theme.text).align(Align::Centre),
        );
        painter.finish()
    }

    fn settings_row(&self, painter: &mut Painter, rect: Rect, index: usize) {
        let theme = &self.theme;
        let state = &self.settings;
        let field = &FIELDS[index];
        let invalid = state.error && (state.editing == Some(index) || state.message.starts_with(field.key));
        let group_bottom = state
            .selected()
            .iter()
            .position(|other| *other == index)
            .and_then(|position| state.selected().get(position + 1).copied())
            .is_none_or(|next| state.heading(next) != state.heading(index));
        if !group_bottom {
            painter.fill(Rect::new(rect.x + 16., rect.bottom() - 1., rect.w - 32., 1.), theme.border, 0.);
        }
        if invalid {
            painter.fill(Rect::new(rect.x + 1., rect.y + 10., 3., rect.h - 20.), theme.danger, 1.);
        } else if state.draft[index] != state.before[index] {
            painter.fill(Rect::new(rect.x + 1., rect.y + 10., 3., rect.h - 20.), theme.accent, 1.);
        }
        let stacked = rect.h > 100.;
        let padding = if stacked { 10. } else { (rect.h - 42.) / 2. };
        let label = Rect::new(rect.x + 18., rect.y + padding, rect.w - if stacked { 36. } else { 250. }, 21.);
        painter.text(field.label, label, Style::new(12., theme.text).weight(500));
        painter.text(
            field.description,
            Rect::new(label.x, label.bottom() + 1., label.w, 20.),
            Style::new(11., theme.muted),
        );
        let control = control_rect(rect);
        let value = state.draft[index].as_ref();
        match field.control {
            Control::Toggle => {
                let enabled = value.and_then(toml::Value::as_bool).unwrap_or(false);
                painter.text(
                    if enabled { "On" } else { "Off" },
                    Rect::new(control.x + 12., control.y, control.w - 60., control.h),
                    Style::new(12., theme.text),
                );
                let switch = Rect::new(control.right() - 38., control.y + (control.h - 20.) / 2., 34., 20.);
                painter.shape(
                    switch,
                    Shape {
                        radius: 10.,
                        fill: if enabled {
                            (theme.accent, theme.accent_end)
                        } else {
                            (theme.surface_raised, theme.surface_raised)
                        },
                        border_width: if enabled { 0. } else { 1. },
                        border: theme.border,
                        ..Default::default()
                    },
                );
                painter.fill(
                    Rect::new(switch.x + if enabled { 17. } else { 3. }, switch.y + 3., 14., 14.),
                    if enabled { theme.text } else { theme.muted },
                    7.,
                );
            }
            Control::Number(min, max, _, unit) => {
                let number = state.number(field.key, min);
                let min = min.min(number);
                let max = max.max(number);
                let track = Rect::new(control.x, control.y + control.h / 2. - 1.5, 90., 3.);
                painter.fill(track, theme.border, 2.);
                let fraction = ((number - min) / (max - min)).clamp(0., 1.);
                painter.shape(
                    Rect::new(track.x, track.y, track.w * fraction, 3.),
                    Shape { fill: (theme.accent, theme.accent_end), radius: 2., ..Default::default() },
                );
                painter.fill(Rect::new(track.x + track.w * fraction - 6., track.y - 5., 13., 13.), theme.text, 7.);
                painter.text(
                    &number_label(field.key, number, unit),
                    Rect::new(control.x + 102., control.y, 56., control.h),
                    Style::new(11., theme.text),
                );
            }
            Control::Section if inline_levels(index, &state.query) => {
                let hovered = control.contains(Point::new(self.settings.pointer.0, self.settings.pointer.1));
                painter.shape(
                    control,
                    Shape {
                        border_width: 1.,
                        border: if hovered { theme.muted } else { theme.border },
                        ..Shape::filled(theme.surface_raised, 6.)
                    },
                );
                painter.glyph(glyph::PLUS, Rect::new(control.x + 10., control.y, 16., control.h), 14, theme.text);
                painter.text(
                    "Add level",
                    Rect::new(control.x + 34., control.y, control.w - 40., control.h),
                    Style::new(11., theme.text).weight(500),
                );
            }
            Control::Section => {
                let hovered = control.contains(Point::new(self.settings.pointer.0, self.settings.pointer.1));
                painter.shape(
                    control,
                    Shape {
                        border_width: 1.,
                        border: if hovered { theme.muted } else { theme.border },
                        ..Shape::filled(theme.surface_raised, 6.)
                    },
                );
                painter.text(
                    if state.parked_sections.iter().any(|screen| screen.field == index) {
                        "Edited, continue"
                    } else {
                        "Open"
                    },
                    Rect::new(control.x + 12., control.y, control.w - 40., control.h),
                    Style::new(11., theme.text).weight(500),
                );
                painter.glyph(
                    glyph::CHEVRON_RIGHT,
                    Rect::new(control.right() - 26., control.y, 16., control.h),
                    14,
                    theme.muted,
                );
            }
            _ => {
                let editing = state.editing == Some(index);
                let hovered = control.contains(Point::new(self.settings.pointer.0, self.settings.pointer.1));
                painter.shape(
                    control,
                    Shape {
                        border_width: 1.,
                        border: if invalid {
                            theme.danger
                        } else if editing {
                            theme.accent
                        } else if hovered {
                            theme.muted
                        } else {
                            theme.border
                        },
                        ..Shape::filled(if editing { theme.background } else { theme.surface_raised }, 6.)
                    },
                );
                let label = match (value.and_then(toml::Value::as_str), field.control) {
                    (_, _) if editing => state.input.text.clone(),
                    (Some(text), Control::Choice(_)) => choice_label(field.key, text),
                    (None, Control::Choice(_)) if field.key == "touchpad.click_method" => "Device default".into(),
                    _ => show(value),
                };
                let swatch = match field.control {
                    Control::Colour => Some(
                        value
                            .and_then(toml::Value::as_str)
                            .and_then(|value| Colour::parse(value).ok())
                            .unwrap_or(theme.accent),
                    ),
                    _ if field.key == "battery.charging" => {
                        value.and_then(toml::Value::as_str).and_then(|value| named_colour(theme, value))
                    }
                    _ => None,
                };
                if let Some(swatch) = swatch {
                    let pointer = Point::new(self.settings.pointer.0, self.settings.pointer.1);
                    let active = state.colour.as_ref().is_some_and(|picker| picker.index == index)
                        || Rect::new(control.x, control.y, 36., control.h).contains(pointer);
                    painter.shape(
                        Rect::new(control.x + 9., control.y + (control.h - 18.) / 2., 18., 18.),
                        Shape {
                            border_width: if active { 2. } else { 1. },
                            border: if active { theme.text } else { theme.border },
                            ..Shape::filled(swatch, 4.)
                        },
                    );
                }
                let colour = swatch.is_some();
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
                    let unset = value.is_none() && !matches!(field.control, Control::Choice(_));
                    painter.text(
                        &label,
                        Rect::new(text_rect.x, text_rect.y, text_rect.w - 14., text_rect.h),
                        Style::new(11., if unset { theme.muted } else { theme.text }),
                    );
                }
                if matches!(field.control, Control::Choice(_) | Control::Keyboard) {
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
            let reset = reset_rect(rect);
            let hovered = reset.contains(Point::new(self.settings.pointer.0, self.settings.pointer.1));
            if hovered {
                painter.fill(reset, theme.surface_raised, 6.);
            }
            painter.glyph(glyph::RESTART, reset, 14, if hovered { theme.text } else { theme.muted });
        }
        if state.focus == Some(Hit::Field(index)) {
            let control = if matches!(field.control, Control::Toggle) {
                Rect::new(control.right() - 42., control.y + (control.h - 28.) / 2., 42., 28.)
            } else {
                control
            };
            painter.shape(
                control,
                Shape {
                    radius: if matches!(field.control, Control::Toggle) { 14. } else { 6. },
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

    /// One battery colour threshold: the charge it starts at, its colour
    /// and a button that removes it.
    fn settings_level(&self, painter: &mut Painter, rect: Rect, level: usize, threshold: i64, colour: &str) {
        let theme = &self.theme;
        let pointer = Point::new(self.settings.pointer.0, self.settings.pointer.1);
        let parts = level_parts(rect);
        painter.fill(Rect::new(rect.x + 34., rect.y, rect.w - 50., 1.), theme.border, 0.);
        painter.fill(Rect::new(rect.x + 22., rect.y, 1., rect.h / 2.), theme.border, 0.);
        painter.fill(Rect::new(rect.x + 22., rect.y + rect.h / 2., 6., 1.), theme.border, 0.);
        painter.text(
            &format!("At or below {threshold}%"),
            Rect::new(rect.x + 36., rect.y + 7., rect.w - 260., 20.),
            Style::new(12., theme.text),
        );
        let named = match colour {
            "accent" => "Accent colour".to_string(),
            "warning" => "Warning colour".to_string(),
            "danger" => "Error colour".to_string(),
            "muted" => "Secondary text colour".to_string(),
            "text" => "Text colour".to_string(),
            other => other.to_string(),
        };
        painter.text(&named, Rect::new(rect.x + 36., rect.y + 27., rect.w - 260., 18.), Style::new(11., theme.muted));
        let active = self.settings.colour.as_ref().is_some_and(|picker| picker.level == Some(level))
            || parts.swatch.contains(pointer);
        painter.shape(
            parts.swatch,
            Shape {
                border_width: if active { 2. } else { 1. },
                border: if active { theme.text } else { theme.border },
                ..Shape::filled(named_colour(theme, colour).unwrap_or(theme.warning), 6.)
            },
        );
        let track = Rect::new(parts.slider.x, parts.slider.y + parts.slider.h / 2. - 1.5, parts.slider.w, 3.);
        let fraction = (threshold as f64 / 100.).clamp(0., 1.);
        painter.fill(track, theme.border, 2.);
        painter.shape(
            Rect::new(track.x, track.y, track.w * fraction, 3.),
            Shape { fill: (theme.accent, theme.accent_end), radius: 2., ..Default::default() },
        );
        painter.fill(Rect::new(track.x + track.w * fraction - 6., track.y - 5., 13., 13.), theme.text, 7.);
        let hovered = parts.remove.contains(pointer);
        if hovered {
            painter.fill(parts.remove, theme.surface_raised, 6.);
        }
        painter.glyph(glyph::CLOSE, parts.remove, 14, if hovered { theme.text } else { theme.muted });
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
            let changed: Vec<_> =
                (0..FIELDS.len()).filter(|index| self.settings.draft[*index] != self.settings.before[*index]).collect();
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
        let mut lines: Vec<String> = match result {
            Ok(text) => text.lines().map(String::from).collect(),
            Err(error) => vec![error],
        };
        // Pending default apps, as mimeapps.list lines.
        let apps = self.settings_apps_review(after);
        if !apps.is_empty() {
            if self.settings.draft == self.settings.before {
                lines.clear();
            } else {
                lines.push(String::new());
            }
            lines.extend(apps);
        }
        lines
    }

    fn settings_config(&self, painter: &mut Painter, rect: Rect) {
        let theme = &self.theme;
        let before = self.settings_config_lines(false);
        let after = self.settings_config_lines(true);
        let before_keys = config_line_keys(&before);
        let after_keys = config_line_keys(&after);
        let before_values: std::collections::BTreeMap<_, _> = before_keys.iter().zip(&before).collect();
        let width = (rect.w - 12.) / 2.;
        for (column, title) in ["Applied now", "After applying"].iter().enumerate() {
            let left = rect.x + column as f64 * (width + 12.);
            painter.text(
                title,
                Rect::new(left + 2., rect.y, width, 22.),
                Style::new(10., if column == 0 { theme.muted } else { theme.accent }).weight(600),
            );
            let pane = Rect::new(left, rect.y + 26., width, rect.h - 26.);
            painter.shape(pane, Shape { border_width: 1., border: theme.border, ..Shape::filled(theme.surface, 6.) });
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
                        painter.fill(Rect::new(area.x, top, area.w, 23.), theme.accent.alpha(0.08), 3.);
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

/// The size Settings opens at: most of the screen, with a narrower margin on
/// small screens, within readable limits.
fn settings_size(screen: Rect) -> (f64, f64) {
    (
        (screen.w - if screen.w < 900. { 24. } else { 96. }).clamp(320., 1360.),
        (screen.h - if screen.h < 700. { 48. } else { 96. }).clamp(240., 960.),
    )
}
