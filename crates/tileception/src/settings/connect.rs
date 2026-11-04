//! Native Tuya setup. Network discovery stays off the compositor thread.

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

pub(super) struct Connect {
    client_id: TextInput,
    secret: SecretField,
    region: usize,
    menu: Option<usize>,
    sensors: bool,
    refresh: bool,
    focus: usize,
    scroll: f64,
    error: String,
    invalid: Option<usize>,
    progress: String,
    path: PathBuf,
    devices: Option<String>,
    job: Option<Job>,
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
    panel: Rect,
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
            region: region.unwrap_or(0),
            menu: None,
            sensors: cloud.as_ref().is_some_and(|access| access.poll_sensors),
            refresh: cloud.as_ref().is_some_and(|access| access.startup_refresh),
            focus: usize::from(cloud.is_some()),
            scroll: 0.,
            error: String::new(),
            invalid: None,
            progress: String::new(),
            path,
            devices,
            job: None,
        });
        self.schedule_render();
    }

    fn connect_layout(&self) -> Layout {
        let screen = self.output_rect();
        let width = (screen.w - 48.).clamp(280., 720.).min(screen.w);
        let height = (screen.h - 48.).clamp(240., 620.).min(screen.h);
        let panel = Rect::new(screen.x + (screen.w - width) / 2., screen.y + (screen.h - height) / 2., width, height);
        let body = Rect::new(panel.x + 24., panel.y + 100., panel.w - 48., (panel.h - 172.).max(1.));
        let scroll = self.settings.connect.as_ref().map_or(0., |form| form.scroll);
        Layout {
            panel,
            body,
            fields: std::array::from_fn(|index| {
                Rect::new(body.x, body.y + index as f64 * 68. + 20. - scroll, body.w, 36.)
            }),
            toggles: std::array::from_fn(|index| {
                Rect::new(body.x, body.y + 208. + index as f64 * 34. - scroll, body.w, 28.)
            }),
            submit: Rect::new(panel.right() - 136., panel.bottom() - 54., 112., 34.),
            cancel: Rect::new(panel.right() - 260., panel.bottom() - 54., 112., 34.),
        }
    }

    fn cancel_settings_connect(&mut self) {
        if let Some(mut form) = self.settings.connect.take()
            && let Some(job) = form.job.take()
        {
            job.cancelled.store(true, Ordering::Relaxed);
            self.loop_handle.remove(job.token);
        }
        self.settings.editor_epoch = self.settings.editor_epoch.wrapping_add(1);
        self.schedule_render();
    }

    fn start_settings_connect(&mut self) {
        let scan = !self.settings_home_connected();
        let Some(form) = self.settings.connect.as_mut() else { return };
        if form.job.is_some() {
            return;
        }
        form.menu = None;
        form.invalid = None;
        if form.client_id.text.trim().is_empty() || form.secret.is_empty() {
            form.invalid = Some(if form.client_id.text.trim().is_empty() { 0 } else { 1 });
            form.error = "Enter the Access ID and Access Secret from your Tuya project.".into();
            form.focus = form.invalid.unwrap_or(0);
            form.scroll = 0.;
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
        let secret = form.secret.take();
        let region = REGIONS[form.region].0.to_string();
        let (sensors, refresh) = (form.sensors, form.refresh);
        form.error.clear();
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
            let old_home = self.config.file.home.clone();
            self.reload_config()
                .map_err(|error| format!("Devices imported, but could not reload settings: {error}"))?;
            if old_home == self.config.file.home {
                self.apply_home();
            }
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
        if let Err(error) = result {
            form.error = error;
            form.progress.clear();
            form.focus = 1;
            self.settings.connect = Some(form);
        }
        self.settings.editor_epoch = self.settings.editor_epoch.wrapping_add(1);
        self.schedule_render();
    }

    pub(super) fn settings_connect_scroll(&mut self, amount: f64) {
        let layout = self.connect_layout();
        if let Some(form) = self.settings.connect.as_mut() {
            form.scroll = (form.scroll + amount).clamp(0., (370. - layout.body.h).max(0.));
            form.menu = None;
        }
        self.schedule_render();
    }

    pub(super) fn settings_connect_click(&mut self, point: Point) {
        let layout = self.connect_layout();
        if layout.cancel.contains(point) {
            self.cancel_settings_connect();
            return;
        }
        if layout.submit.contains(point) {
            self.start_settings_connect();
            return;
        }
        let Some(form) = self.settings.connect.as_mut() else { return };
        if form.job.is_some() {
            return;
        }
        if form.menu.is_some() {
            let top = (layout.fields[2].bottom() + 4.).min(layout.cancel.y - REGIONS.len() as f64 * 28. - 8.);
            if point.x >= layout.body.x
                && point.x <= layout.body.right()
                && point.y >= top
                && point.y < top + REGIONS.len() as f64 * 28.
            {
                form.region = ((point.y - top) / 28.) as usize;
            }
            form.menu = None;
        } else if let Some(index) = layout
            .fields
            .iter()
            .position(|rect| rect.contains(point) && rect.y >= layout.body.y && rect.bottom() <= layout.body.bottom())
        {
            form.focus = index;
            self.settings.editor_epoch = self.settings.editor_epoch.wrapping_add(1);
            if index == 2 {
                form.menu = Some(form.region);
            }
        } else if layout.toggles[0].contains(point) && layout.body.contains(point) {
            form.sensors = !form.sensors;
            form.focus = 3;
            self.settings.editor_epoch = self.settings.editor_epoch.wrapping_add(1);
        } else if layout.toggles[1].contains(point) && layout.body.contains(point) {
            form.refresh = !form.refresh;
            form.focus = 4;
            self.settings.editor_epoch = self.settings.editor_epoch.wrapping_add(1);
        }
        self.schedule_render();
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

    pub(super) fn settings_connect_key(&mut self, sym: u32, mods: Mods) {
        let Some(form) = self.settings.connect.as_mut() else { return };
        if form.job.is_some() {
            if sym == keysyms::KEY_Escape {
                self.cancel_settings_connect();
            }
            return;
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
            self.cancel_settings_connect();
            return;
        } else if mods.ctrl && sym == keysyms::KEY_Return {
            self.start_settings_connect();
            return;
        } else if mods.ctrl && matches!(sym, keysyms::KEY_v | keysyms::KEY_V) {
            self.paste(crate::launcher::Paste::Settings(self.settings.editor_epoch));
            return;
        } else if sym == keysyms::KEY_Tab || sym == keysyms::KEY_ISO_Left_Tab {
            form.focus = (form.focus + if mods.shift || sym == keysyms::KEY_ISO_Left_Tab { 6 } else { 1 }) % 7;
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
                    return;
                }
                6 => {
                    self.start_settings_connect();
                    return;
                }
                _ => {}
            }
        } else if form.focus == 1 {
            match sym {
                keysyms::KEY_a | keysyms::KEY_A if mods.ctrl => form.secret.select_all(),
                keysyms::KEY_BackSpace | keysyms::KEY_Delete => form.secret.backspace(),
                _ if !mods.ctrl && !mods.alt && !mods.logo => {
                    form.secret.type_key(sym);
                }
                _ => {}
            }
        } else if form.focus == 0 {
            match sym {
                keysyms::KEY_a | keysyms::KEY_A if mods.ctrl => form.client_id.select_all(),
                keysyms::KEY_Left | keysyms::KEY_Right => {
                    form.client_id.move_horizontal(sym == keysyms::KEY_Right, mods.ctrl, mods.shift)
                }
                keysyms::KEY_Home | keysyms::KEY_End => form
                    .client_id
                    .move_to(if sym == keysyms::KEY_Home { 0 } else { form.client_id.text.len() }, mods.shift),
                keysyms::KEY_BackSpace | keysyms::KEY_Delete => {
                    form.client_id.delete(sym == keysyms::KEY_Delete, mods.ctrl)
                }
                _ if !mods.ctrl && !mods.alt && !mods.logo => {
                    if let Some(character) =
                        char::from_u32(xkb::keysym_to_utf32(sym.into())).filter(|character| !character.is_control())
                    {
                        form.client_id.insert(&character.to_string());
                    }
                }
                _ => {}
            }
        }
        self.schedule_render();
    }

    pub(super) fn settings_draw_connect(&self, painter: &mut Painter<'_>) {
        let Some(form) = &self.settings.connect else { return };
        let layout = self.connect_layout();
        let theme = &self.theme;
        let busy = form.job.is_some();
        painter.fill(self.output_rect(), Colour([0., 0., 0., 0.55]), 0.);
        painter.shape(
            layout.panel,
            Shape { border_width: 1., border: theme.border, ..Shape::filled(theme.background, 8.) },
        );
        painter.text(
            "Connect smart home",
            Rect::new(layout.body.x, layout.panel.y + 20., layout.body.w, 32.),
            Style::new(23., theme.text).weight(500),
        );
        painter.text(
            "Use your Tuya IoT project's Access ID and Access Secret.",
            Rect::new(layout.body.x, layout.panel.y + 60., layout.body.w, 32.),
            Style::new(12., theme.muted).lines(2),
        );
        painter.clipped(layout.body, |painter| {
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
                        } else if form.focus == index && !busy {
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
                    let mask = "*".repeat(form.secret.chars().min(64));
                    let (text, cursor, anchor) = if index == 0 {
                        (form.client_id.text.as_str(), form.client_id.cursor, form.client_id.anchor)
                    } else {
                        (mask.as_str(), mask.len(), form.secret.is_selected().then_some(0))
                    };
                    let field = crate::text::Field {
                        cursor,
                        anchor,
                        visible: form.focus == index && !busy,
                        accent: theme.accent.straight_rgba8(),
                    };
                    if let Some(image) =
                        self.text.borrow_mut().field(text, &theme.font, 12., theme.text, (rect.w - 24.) as f32, field)
                    {
                        painter.image(&image.buffer, (rect.x + 12., rect.y + (rect.h - image.height) / 2.));
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
                if form.focus == index + 3 && !busy {
                    painter.shape(
                        Rect::new(switch.x - 4., switch.y - 4., 42., 28.),
                        Shape { border_width: 1., border: theme.accent, radius: 12., ..Default::default() },
                    );
                }
            }
            painter.text(
                if form.sensors || form.refresh {
                    "These options store the Access Secret in your private device file."
                } else {
                    "Credentials are used for this connection only."
                },
                Rect::new(layout.body.x, layout.body.y + 280. - form.scroll, layout.body.w, 32.),
                Style::new(11., theme.muted).lines(2),
            );
            painter.text(
                if form.error.is_empty() { &form.progress } else { &form.error },
                Rect::new(layout.body.x, layout.body.y + 320. - form.scroll, layout.body.w, 44.),
                Style::new(12., if form.error.is_empty() { theme.accent } else { theme.danger }).lines(3),
            );
        });
        for (index, rect, label) in
            [(5, layout.cancel, "Cancel"), (6, layout.submit, if busy { "Connecting..." } else { "Connect" })]
        {
            painter.shape(
                rect,
                Shape {
                    radius: 5.,
                    fill: if index == 6 && !busy {
                        (theme.accent, theme.accent_end)
                    } else {
                        (theme.surface_raised, theme.surface_raised)
                    },
                    border_width: if form.focus == index { 1. } else { 0. },
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
