//! The Tuya credentials form on the Smart home page, and the import it
//! starts. The form is part of the page: it takes the keyboard once one of
//! its fields is clicked, and gives it back with Escape or a click elsewhere.
//! Network discovery stays off the compositor thread.

use smithay::reexports::calloop::{RegistrationToken, channel};
use std::{
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};
use tileception_home::{
    cloud::Credentials,
    store::{self, CloudAccess, DeviceFile},
};
use zeroize::Zeroizing;

use super::*;
use crate::{password::SecretField, text_input::TextInput};

const REGIONS: &[(&str, &str)] = &[
    ("eu", "Central Europe"),
    ("we", "Western Europe"),
    ("us", "Western America"),
    ("ue", "Eastern America"),
    ("in", "India"),
    ("cn", "China"),
    ("sg", "Singapore"),
];

#[derive(Clone, Copy, PartialEq)]
enum PointerAction {
    Cancel,
    Submit,
    Region(&'static str),
    Dismiss,
    ClientId,
    Secret,
    RegionMenu,
    Sensors,
    Refresh,
}

impl crate::ui::PointerAction for PointerAction {
    fn activate(&self, state: &mut Tileception, _: Rect, _: Point) {
        match self {
            Self::Cancel => state.cancel_settings_connect(),
            Self::Submit => state.start_settings_connect(),
            action => {
                let Some(form) = state.settings.connect.as_mut().filter(|form| form.job.is_none()) else { return };
                form.active = true;
                match action {
                    Self::Region(region) => {
                        if let Some(index) = REGIONS.iter().position(|(key, _)| key == region) {
                            form.region = index;
                        }
                        form.menu = None;
                    }
                    Self::Dismiss => form.menu = None,
                    Self::ClientId => form.focus = 0,
                    Self::Secret => form.focus = 1,
                    Self::RegionMenu => {
                        form.focus = 2;
                        form.menu = Some(form.region);
                    }
                    Self::Sensors => {
                        form.sensors = !form.sensors;
                        form.focus = 3;
                    }
                    Self::Refresh => {
                        form.refresh = !form.refresh;
                        form.focus = 4;
                    }
                    Self::Cancel | Self::Submit => unreachable!(),
                }
                state.settings.editor_epoch = state.settings.editor_epoch.wrapping_add(1);
            }
        }
        state.schedule_render();
    }
}

pub(super) struct Connect {
    client_id: TextInput,
    secret: SecretField,
    /// The Access Secret kept in the device file, used when the field is
    /// left empty.
    stored: Option<Zeroizing<String>>,
    region: usize,
    menu: Option<usize>,
    sensors: bool,
    refresh: bool,
    focus: usize,
    /// The form has the keyboard.
    active: bool,
    error: String,
    invalid: Option<usize>,
    progress: String,
    path: PathBuf,
    devices: Option<String>,
    job: Option<Job>,
}

impl Connect {
    /// An import is running.
    pub(super) fn busy(&self) -> bool {
        self.job.is_some()
    }

    /// The device file keeps the Access Secret, so refreshing needs no typing.
    pub(super) fn has_stored_secret(&self) -> bool {
        self.stored.is_some() && !self.client_id.text.trim().is_empty()
    }
}

struct Job {
    token: RegistrationToken,
    cancelled: Arc<AtomicBool>,
    inventory: Option<Zeroizing<Vec<u8>>>,
    home: crate::home::HomeConfig,
}

enum Event {
    Progress(String),
    Finished(Result<DeviceFile, String>),
}

struct Layout {
    body: Rect,
    fields: [Rect; 3],
    toggles: [Rect; 2],
    submit: Rect,
    cancel: Rect,
}

fn inventory(path: &Path) -> Result<Option<Zeroizing<Vec<u8>>>, String> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(Some(Zeroizing::new(bytes))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!("{}: {error}", path.display())),
    }
}

impl Tileception {
    pub(super) fn connect_pointer_target(&self, point: Point) -> Option<interaction::Target> {
        let layout = self.connect_layout()?;
        let form = self.settings.connect.as_ref()?;
        if self.connect_cancellable() && layout.cancel.contains(point) {
            return Some(interaction::Target::button(layout.cancel, PointerAction::Cancel));
        }
        if form.job.is_some() {
            return None;
        }
        if layout.submit.contains(point) {
            return Some(interaction::Target::button(layout.submit, PointerAction::Submit));
        }
        if form.menu.is_some() {
            let top = (layout.fields[2].bottom() + 4.).min(layout.cancel.y - REGIONS.len() as f64 * 28. - 8.);
            for (index, (region, _)) in REGIONS.iter().enumerate() {
                let rect = Rect::new(layout.body.x, top + index as f64 * 28., layout.body.w, 28.);
                if rect.contains(point) {
                    return Some(interaction::Target::button(rect, PointerAction::Region(region)));
                }
            }
            return Some(interaction::Target::new(
                self.settings_frame().panel,
                PointerAction::Dismiss,
                smithay::input::pointer::CursorIcon::Default,
            ));
        }
        for (rect, action) in
            layout.fields.iter().zip([PointerAction::ClientId, PointerAction::Secret, PointerAction::RegionMenu])
        {
            if rect.contains(point) && rect.y >= layout.body.y && rect.bottom() <= layout.body.bottom() {
                return Some(if action == PointerAction::RegionMenu {
                    interaction::Target::button(*rect, action)
                } else {
                    interaction::Target::new(*rect, action, smithay::input::pointer::CursorIcon::Text)
                });
            }
        }
        layout.toggles.iter().zip([PointerAction::Sensors, PointerAction::Refresh]).find_map(|(rect, action)| {
            (rect.contains(point) && layout.body.contains(point)).then(|| interaction::Target::button(*rect, action))
        })
    }

    pub(super) fn open_settings_connect(&mut self) {
        let devices = self.settings.value("home.devices").and_then(toml::Value::as_str).map(String::from);
        let config = crate::home::HomeConfig { enabled: true, devices: devices.clone() };
        let Some(path) = config.path() else {
            self.settings.message = "No configuration directory is available.".into();
            self.settings.error = true;
            return;
        };
        let cloud = store::load(&path).ok().and_then(|file| file.cloud);
        let client_id = cloud.as_ref().map_or_else(String::new, |access| access.client_id.clone());
        let cursor = client_id.len();
        let region = cloud.as_ref().and_then(|access| {
            REGIONS.iter().position(|(region, _)| {
                tileception_home::cloud::host(region) == tileception_home::cloud::host(&access.region)
            })
        });
        self.settings.editor_epoch = self.settings.editor_epoch.wrapping_add(1);
        self.settings.connect = Some(Connect {
            client_id: TextInput { text: client_id, cursor, anchor: None },
            secret: SecretField::default(),
            stored: cloud
                .as_ref()
                .map(|access| Zeroizing::new(access.secret.clone()))
                .filter(|secret| !secret.is_empty()),
            region: region.unwrap_or(0),
            menu: None,
            sensors: cloud.as_ref().is_some_and(|access| access.poll_sensors),
            refresh: cloud.as_ref().is_some_and(|access| access.startup_refresh),
            focus: usize::from(cloud.is_some()),
            active: false,
            error: String::new(),
            invalid: None,
            progress: String::new(),
            path,
            devices,
            job: None,
        });
        self.schedule_render();
    }

    /// The form's parts, inside the page's form area; `None` while the
    /// page does not show the form.
    fn connect_layout(&self) -> Option<Layout> {
        let body = self.settings_frame().home.and_then(|home| home.form)?;
        Some(Layout {
            body,
            fields: std::array::from_fn(|index| Rect::new(body.x, body.y + 22. + index as f64 * 66., body.w, 36.)),
            toggles: std::array::from_fn(|index| Rect::new(body.x, body.y + 202. + index as f64 * 34., body.w, 28.)),
            submit: Rect::new(body.right() - 140., body.bottom() - 36., 140., 34.),
            cancel: Rect::new(body.right() - 290., body.bottom() - 36., 140., 34.),
        })
    }

    /// Cancel shows while importing, and when refreshing devices that were
    /// set up before; the first setup has nothing to go back to.
    fn connect_cancellable(&self) -> bool {
        self.settings.home_ready || self.settings.connect.as_ref().is_some_and(Connect::busy)
    }

    fn cancel_settings_connect(&mut self) {
        let ready = self.settings.home_ready;
        if let Some(form) = self.settings.connect.as_mut()
            && let Some(job) = form.job.take()
        {
            job.cancelled.store(true, Ordering::Relaxed);
            self.loop_handle.remove(job.token);
            form.progress.clear();
            form.error = "Stopped.".into();
        }
        // A first setup keeps its form on the page.
        if ready {
            self.settings.connect = None;
        }
        self.settings.editor_epoch = self.settings.editor_epoch.wrapping_add(1);
        self.schedule_render();
    }

    pub(super) fn start_settings_connect(&mut self) {
        let scan = !self.settings_home_connected();
        let Some(form) = self.settings.connect.as_mut() else { return };
        if form.job.is_some() {
            return;
        }
        form.menu = None;
        form.invalid = None;
        if form.client_id.text.trim().is_empty() || (form.secret.is_empty() && form.stored.is_none()) {
            form.invalid = Some(if form.client_id.text.trim().is_empty() { 0 } else { 1 });
            form.error = "Enter the Access ID and Access Secret from your Tuya project.".into();
            form.focus = form.invalid.unwrap_or(0);
            form.active = true;
            self.schedule_render();
            return;
        }
        let expected = (|| {
            let home = crate::config::Config::parse(&crate::config_edit::panel_text()?)?.file.home;
            Ok::<_, String>((inventory(&form.path)?, home))
        })();
        let (expected, home) = match expected {
            Ok(expected) => expected,
            Err(error) => {
                form.error = error;
                self.schedule_render();
                return;
            }
        };
        let (sender, receiver) = channel::channel::<Event>();
        let token = match self.loop_handle.insert_source(receiver, |event, _, state| {
            match event {
                channel::Event::Msg(Event::Progress(progress)) => {
                    if let Some(form) = state.settings.connect.as_mut() {
                        form.progress = progress;
                    }
                }
                channel::Event::Msg(Event::Finished(result)) => state.finish_settings_connect(result),
                channel::Event::Closed => state.finish_settings_connect(Err("Connection worker stopped.".into())),
            }
            state.schedule_render();
        }) {
            Ok(token) => token,
            Err(error) => {
                form.error = format!("Could not start connection: {error}");
                self.schedule_render();
                return;
            }
        };
        let cancelled = Arc::new(AtomicBool::new(false));
        let worker_cancelled = cancelled.clone();
        let path = form.path.clone();
        let client_id = form.client_id.text.trim().to_string();
        let secret = match &form.stored {
            Some(stored) if form.secret.is_empty() => stored.clone(),
            _ => form.secret.take(),
        };
        let region = REGIONS[form.region].0.to_string();
        let (sensors, refresh) = (form.sensors, form.refresh);
        form.error.clear();
        form.active = false;
        form.progress = "Connecting to Tuya...".into();
        form.job = Some(Job { token, cancelled, inventory: expected, home });
        let started = std::thread::Builder::new().name("home-connect".into()).spawn(move || {
            let result = (|| {
                let credentials =
                    Credentials { client_id: client_id.clone(), secret: secret.clone(), region: region.clone() };
                let mut file = crate::home::import_devices(&path, credentials, scan, &mut |progress| {
                    if !worker_cancelled.load(Ordering::Relaxed) {
                        let _ = sender.send(Event::Progress(progress.into()));
                    }
                })?;
                if worker_cancelled.load(Ordering::Relaxed) {
                    return Err("Connection cancelled.".into());
                }
                if sensors || refresh {
                    file.cloud = Some(CloudAccess {
                        client_id,
                        region,
                        secret: secret.to_string(),
                        poll_sensors: sensors,
                        startup_refresh: refresh,
                    });
                }
                Ok(file)
            })();
            let _ = sender.send(Event::Finished(result));
        });
        if let Err(error) = started {
            if let Some(job) = form.job.take() {
                self.loop_handle.remove(job.token);
            }
            form.error = format!("Could not start connection: {error}");
        }
        self.schedule_render();
    }

    fn finish_settings_connect(&mut self, result: Result<DeviceFile, String>) {
        let Some(mut form) = self.settings.connect.take() else { return };
        let Some(job) = form.job.take() else {
            self.settings.connect = Some(form);
            return;
        };
        self.loop_handle.remove(job.token);
        if job.cancelled.load(Ordering::Relaxed) {
            return;
        }
        let result = result.and_then(|mut file| {
            let current = inventory(&form.path)?;
            if current.as_deref() != job.inventory.as_deref() {
                return Err("The device file changed while connecting. Reopen setup to use the new file.".into());
            }
            let config = crate::config::Config::parse(&crate::config_edit::panel_text()?)?;
            if config.file.home != job.home {
                return Err(
                    "Smart-home settings changed while connecting. Reopen setup to use the new settings.".into()
                );
            }
            let draft = self.settings_document()?;
            let patches = [
                ("home.enabled", Some(toml::Value::Boolean(true))),
                ("home.devices", form.devices.clone().map(toml::Value::String)),
            ];
            // Prepare the rebase before writing: preserve other pending panel changes.
            let new_before = crate::config_edit::draft_text(&self.settings.document_before, &patches)?;
            let new_draft = crate::config_edit::draft_text(&draft, &patches)?;
            let before = values(&crate::config::Config::parse(&new_before)?.file)?;
            let draft_values = values(&crate::config::Config::parse(&new_draft)?.file)?;
            let count = file.devices.len();
            self.settings_home_update_addresses(&mut file);
            store::save(&form.path, &file)?;
            crate::config_edit::apply_draft(&[
                ("home.enabled", Some(toml::Value::Boolean(job.home.enabled)), patches[0].1.clone()),
                ("home.devices", job.home.devices.clone().map(toml::Value::String), patches[1].1.clone()),
            ])
            .map_err(|error| format!("Devices imported, but could not enable the connection: {error}"))?;
            // The home plugin sees the new device file and connects to it.
            self.reload_config()
                .map_err(|error| format!("Devices imported, but could not reload settings: {error}"))?;
            self.settings.before = before;
            self.settings.draft = draft_values;
            self.settings.document_before = new_before;
            self.settings.document_draft = new_draft;
            self.settings.home_cloud_labels =
                [self.settings_home_cloud_label(true), self.settings_home_cloud_label(false)];
            self.settings.message = format!("Imported {count} devices.");
            self.settings.error = false;
            Ok(())
        });
        match result {
            Ok(()) => self.settings_home_arrive(),
            Err(error) => {
                // Problems with the Tuya project have known fixes on the page.
                let known = ["28841107", "data center", "permission", "inconsisten", "no devices are linked"];
                let lower = error.to_lowercase();
                if known.iter().any(|clue| lower.contains(clue)) {
                    self.settings.home_trouble = true;
                    form.error = format!("{error}. See Something went wrong? for how to fix this.");
                } else {
                    form.error = error;
                }
                form.progress.clear();
                form.focus = 1;
                form.active = true;
                self.settings.connect = Some(form);
            }
        }
        self.settings.editor_epoch = self.settings.editor_epoch.wrapping_add(1);
        self.schedule_render();
    }

    /// A click elsewhere on the page gives the keyboard back to the page.
    pub(super) fn settings_connect_release(&mut self) {
        if let Some(form) = self.settings.connect.as_mut() {
            form.active = false;
            form.menu = None;
        }
    }

    /// The form has the keyboard: page shortcuts wait.
    pub(super) fn settings_connect_typing(&self) -> bool {
        self.settings.connect.as_ref().is_some_and(|form| form.active && form.job.is_none())
    }

    /// Scrolling the page closes the data centre list.
    pub(super) fn settings_connect_scrolled(&mut self) {
        if let Some(form) = self.settings.connect.as_mut() {
            form.menu = None;
        }
    }

    pub(super) fn settings_connect_paste(&mut self, text: &str) {
        if let Some(form) = self.settings.connect.as_mut().filter(|form| form.job.is_none()) {
            if form.focus == 1 {
                if !form.secret.insert(text) {
                    form.error = "Paste a single line of at most 4096 bytes.".into();
                    form.invalid = Some(1);
                }
            } else if form.focus == 0 {
                let text: String = text.chars().filter(|character| !character.is_control()).collect();
                form.client_id.insert(&text);
            }
        }
        self.schedule_render();
    }

    /// A key for the form; returns whether the form took it. It only takes
    /// keys once one of its fields was clicked, or while importing (Escape
    /// stops it).
    pub(super) fn settings_connect_key(&mut self, sym: u32, mods: Mods) -> bool {
        let cancellable = self.connect_cancellable();
        let Some(form) = self.settings.connect.as_mut() else { return false };
        if form.job.is_some() {
            if sym == keysyms::KEY_Escape {
                self.cancel_settings_connect();
                return true;
            }
            return false;
        }
        if !form.active {
            return false;
        }
        if let Some(selected) = form.menu {
            match sym {
                keysyms::KEY_Escape | keysyms::KEY_Tab => form.menu = None,
                keysyms::KEY_Up => form.menu = Some((selected + REGIONS.len() - 1) % REGIONS.len()),
                keysyms::KEY_Down => form.menu = Some((selected + 1) % REGIONS.len()),
                keysyms::KEY_Return | keysyms::KEY_space => {
                    form.region = selected;
                    form.menu = None;
                }
                _ => {}
            }
        } else if sym == keysyms::KEY_Escape {
            // Gives the keyboard back to the page.
            form.active = false;
        } else if mods.ctrl && sym == keysyms::KEY_Return {
            self.start_settings_connect();
            return true;
        } else if mods.ctrl && matches!(sym, keysyms::KEY_v | keysyms::KEY_V) {
            self.paste(crate::launcher::Paste::Settings(self.settings.editor_epoch));
            return true;
        } else if sym == keysyms::KEY_Tab || sym == keysyms::KEY_ISO_Left_Tab {
            // Cancel is only there when it does something.
            let stops: Vec<usize> = (0..7).filter(|stop| *stop != 5 || cancellable).collect();
            let current = stops.iter().position(|stop| *stop == form.focus).unwrap_or(0);
            let backwards = mods.shift || sym == keysyms::KEY_ISO_Left_Tab;
            let next = if backwards { (current + stops.len() - 1) % stops.len() } else { (current + 1) % stops.len() };
            form.focus = stops[next];
            self.settings.editor_epoch = self.settings.editor_epoch.wrapping_add(1);
        } else if sym == keysyms::KEY_Return || (sym == keysyms::KEY_space && form.focus >= 2) {
            match form.focus {
                0 | 1 => {
                    form.focus += 1;
                    self.settings.editor_epoch = self.settings.editor_epoch.wrapping_add(1);
                }
                2 => form.menu = Some(form.region),
                3 => form.sensors = !form.sensors,
                4 => form.refresh = !form.refresh,
                5 => {
                    self.cancel_settings_connect();
                    return true;
                }
                6 => {
                    self.start_settings_connect();
                    return true;
                }
                _ => {}
            }
        } else if form.focus == 1 {
            // Editing a field clears its error mark.
            if form.invalid == Some(1) {
                form.invalid = None;
            }
            form.secret.edit_key(sym, mods);
        } else if form.focus == 0 {
            if form.invalid == Some(0) {
                form.invalid = None;
            }
            if mods.ctrl && matches!(sym, keysyms::KEY_c | keysyms::KEY_C | keysyms::KEY_x | keysyms::KEY_X) {
                let text = form.client_id.text[form.client_id.selection()].to_string();
                if matches!(sym, keysyms::KEY_x | keysyms::KEY_X) {
                    form.client_id.delete(false, false);
                }
                self.copy_text(text);
            } else {
                sections::edit_key(&mut form.client_id, &mut self.settings.compose, sym, mods);
            }
        }
        self.schedule_render();
        true
    }

    pub(super) fn settings_draw_connect(&self, painter: &mut Painter<'_>) {
        let Some(form) = &self.settings.connect else { return };
        let Some(layout) = self.connect_layout() else { return };
        let theme = &self.theme;
        let busy = form.job.is_some();
        // The fields show focus only while the form has the keyboard.
        let typing = form.active && !busy;
        {
            for (index, rect) in layout.fields.iter().enumerate() {
                painter.text(
                    ["Access ID", "Access Secret", "Data centre"][index],
                    Rect::new(rect.x, rect.y - 22., rect.w, 20.),
                    Style::new(12., theme.text),
                );
                painter.shape(
                    *rect,
                    Shape {
                        border_width: 1.,
                        border: if form.invalid == Some(index) {
                            theme.danger
                        } else if form.focus == index && typing {
                            theme.accent
                        } else {
                            theme.border
                        },
                        ..Shape::filled(theme.surface, 5.)
                    },
                );
                if index == 2 {
                    painter.text(
                        REGIONS[form.region].1,
                        Rect::new(rect.x + 12., rect.y, rect.w - 44., rect.h),
                        Style::new(12., theme.text),
                    );
                    painter.glyph(
                        glyph::CHEVRON_DOWN,
                        Rect::new(rect.right() - 30., rect.y, 20., rect.h),
                        14,
                        theme.muted,
                    );
                } else {
                    // One mask character per secret character, so the caret
                    // and selection line up; the field scrolls.
                    let mask = "*".repeat(form.secret.chars());
                    let (text, cursor, anchor) = if index == 0 {
                        (form.client_id.text.as_str(), form.client_id.cursor, form.client_id.anchor)
                    } else {
                        let (cursor, anchor) = form.secret.caret();
                        (mask.as_str(), cursor, anchor)
                    };
                    let field = crate::text::Field {
                        cursor,
                        anchor,
                        visible: form.focus == index && typing,
                        accent: theme.accent.straight_rgba8(),
                    };
                    if let Some(image) =
                        self.text.borrow_mut().field(text, &theme.font, 12., theme.text, (rect.w - 24.) as f32, field)
                    {
                        painter.image(&image.buffer, (rect.x + 12., rect.y + (rect.h - image.height) / 2.));
                    }
                    if index == 1 && form.secret.is_empty() && form.stored.is_some() {
                        painter.text(
                            "Saved. Leave empty to keep using it.",
                            Rect::new(rect.x + 16., rect.y, rect.w - 28., rect.h),
                            Style::new(12., theme.muted),
                        );
                    }
                }
            }
            for (index, rect) in layout.toggles.iter().enumerate() {
                let enabled = if index == 0 { form.sensors } else { form.refresh };
                painter.text(
                    ["Read cloud-only sensors", "Refresh devices at startup"][index],
                    Rect::new(rect.x, rect.y, rect.w - 50., rect.h),
                    Style::new(12., theme.text),
                );
                let switch = Rect::new(rect.right() - 34., rect.y + 4., 34., 20.);
                painter.fill(switch, if enabled { theme.accent } else { theme.surface_raised }, 10.);
                painter.fill(
                    Rect::new(switch.x + if enabled { 17. } else { 3. }, switch.y + 3., 14., 14.),
                    theme.text,
                    7.,
                );
                if form.focus == index + 3 && typing {
                    painter.shape(
                        Rect::new(switch.x - 4., switch.y - 4., 42., 28.),
                        Shape { border_width: 1., border: theme.accent, radius: 12., ..Default::default() },
                    );
                }
            }
            painter.text(
                if form.sensors || form.refresh {
                    "Both options keep the Access Secret in your private device file, readable by you only."
                } else {
                    "The Access Secret is used for this connection only and is not kept."
                },
                Rect::new(layout.body.x, layout.body.y + 270., layout.body.w, 32.),
                Style::new(11., theme.muted).lines(2),
            );
            painter.text(
                if form.error.is_empty() { &form.progress } else { &form.error },
                Rect::new(layout.body.x, layout.body.y + 304., layout.body.w, 50.),
                Style::new(12., if form.error.is_empty() { theme.accent } else { theme.danger }).lines(3),
            );
        }
        let cancel = if busy { "Stop" } else { "Cancel" };
        let submit = if busy {
            "Connecting..."
        } else if self.settings.home_ready {
            "Refresh"
        } else {
            "Connect"
        };
        let buttons: Vec<(usize, Rect, &str)> = if self.connect_cancellable() {
            vec![(5, layout.cancel, cancel), (6, layout.submit, submit)]
        } else {
            vec![(6, layout.submit, submit)]
        };
        for (index, rect, label) in buttons {
            painter.shape(
                rect,
                Shape {
                    radius: 5.,
                    fill: if index == 6 && !busy {
                        (theme.accent, theme.accent_end)
                    } else {
                        (theme.surface_raised, theme.surface_raised)
                    },
                    border_width: if form.focus == index && typing { 1. } else { 0. },
                    border: theme.accent,
                    ..Default::default()
                },
            );
            painter.text(
                label,
                rect,
                Style::new(12., if index == 6 && !busy { theme.on_accent } else { theme.text }).align(Align::Centre),
            );
        }
        if let Some(selected) = form.menu {
            let top = (layout.fields[2].bottom() + 4.).min(layout.cancel.y - REGIONS.len() as f64 * 28. - 8.);
            for (index, (_, label)) in REGIONS.iter().enumerate() {
                let rect = Rect::new(layout.body.x, top + index as f64 * 28., layout.body.w, 28.);
                painter.shape(
                    rect,
                    Shape {
                        border_width: 1.,
                        border: theme.border,
                        ..Shape::filled(if selected == index { theme.surface_raised } else { theme.surface }, 2.)
                    },
                );
                painter.text(
                    label,
                    Rect::new(rect.x + 12., rect.y, rect.w - 24., rect.h),
                    Style::new(12., if selected == index { theme.accent } else { theme.text }),
                );
            }
        }
    }
}
