//! The Smart home page: setting up step by step, then the devices.
//!
//! Before any devices are imported the page is three numbered steps: a Tuya
//! cloud project (with an expandable how-to and troubleshooting), the
//! credentials form from `connect`, and the devices it finds. Once a device
//! file exists the page shows the usual rows, the devices and their state,
//! and Refresh devices, which opens the same form again.

use tileception_home::{Reach, store};

use super::*;

/// The steps of setting up a Tuya cloud project, shown under "How to set it up".
const GUIDE: &[&str] = &[
    "Create a free account on the Tuya IoT Platform (a developer account, separate from your Smart Life login).",
    "Open Cloud, Development and create a cloud project. Choose Smart Home as the industry and as the development \
     method, and the data centre your Smart Life account uses (Central Europe for most of Europe).",
    "When asked for API services, keep IoT Core, Authorization Token Management and Smart Home Basic Service.",
    "Open Cloud, Cloud Services, IoT Core and start the free trial (approval can take a little while). Under \
     Authorized Projects, check that your project is listed.",
    "In your project, open Devices, Link App Account, Add App Account. In Smart Life, tap Me and the scan icon at \
     the top right, and scan the code.",
    "Copy the Access ID and Access Secret from the project's Overview, and choose the same data centre below.",
];

/// Known problems and their fixes, shown under "Something went wrong?".
const TROUBLE: &[(&str, &str)] = &[
    (
        "Linking asks for the Smart Industry app",
        "The project was created with the Smart Industry method. Create a new project with Smart Home as the \
         development method.",
    ),
    (
        "Data centers inconsistency",
        "Your app account lives in another data centre. Smart Life shows your country under Me, Settings, Account \
         and Security. Add that data centre to the project and select it at the top right of the platform.",
    ),
    (
        "The data center is suspended (28841107)",
        "In the project's Overview, remove the data centre, save, add it back and save again. Also check that the \
         IoT Core trial is active.",
    ),
    (
        "No devices are found",
        "Devices only appear once your app account is linked to this project, under Devices, Link App Account.",
    ),
];

/// Pages the link buttons open.
pub(super) const LINKS: &[(&str, &str)] =
    &[("Open Tuya IoT Platform", "https://iot.tuya.com"), ("Cloud projects", "https://platform.tuya.com/cloud")];

/// The credentials form's height, as `connect` lays it out.
pub(super) const FORM_H: f64 = 404.;
const PAD: f64 = 20.;
const BUTTON_H: f64 = 32.;
const DEVICE_ROW: f64 = 30.;

/// One card on the page: a numbered step, or the devices.
pub(super) struct Card {
    pub rect: Rect,
    pub step: Option<u8>,
    pub title: &'static str,
    pub subtitle: String,
    /// A finished step: its number shows a check.
    pub done: bool,
}

/// Where everything on the page goes, below the page's rows.
pub(super) struct HomeFrame {
    pub cards: Vec<Card>,
    pub buttons: Vec<(Rect, Hit)>,
    pub guide: Option<Rect>,
    pub trouble: Option<Rect>,
    pub form: Option<Rect>,
    pub devices: Option<Rect>,
    pub height: f64,
}

impl HomeFrame {
    pub(super) fn shift(&mut self, amount: f64) {
        let move_rect = |rect: &mut Rect| rect.y += amount;
        for card in &mut self.cards {
            move_rect(&mut card.rect);
        }
        for (rect, _) in &mut self.buttons {
            move_rect(rect);
        }
        for rect in [&mut self.guide, &mut self.trouble, &mut self.form, &mut self.devices].into_iter().flatten() {
            move_rect(rect);
        }
    }
}

/// Roughly how tall wrapped text gets, for laying out before drawing.
fn text_height(text: &str, width: f64, size: f64) -> f64 {
    let per_line = (width / (size * 0.52)).max(10.);
    let lines = (text.chars().count() as f64 / per_line).ceil().max(1.);
    lines * (size * 1.45)
}

fn guide_height(width: f64) -> f64 {
    GUIDE.iter().map(|step| text_height(step, width - 28., 12.) + 10.).sum::<f64>() + 8.
}

fn trouble_height(width: f64) -> f64 {
    TROUBLE.iter().map(|(_, fix)| 20. + text_height(fix, width, 12.) + 12.).sum::<f64>() + 8.
}

/// A device as the page lists it: name, home and room, and its state.
pub(super) struct Listed {
    pub name: String,
    pub place: String,
    pub status: String,
    pub answering: bool,
}

impl Tileception {
    /// Whether devices were imported: the device file exists.
    pub(super) fn settings_home_ready(&self) -> bool {
        let devices = self.settings.value("home.devices").and_then(toml::Value::as_str).map(String::from);
        crate::home::HomeConfig { enabled: true, devices }.path().is_some_and(|path| path.exists())
    }

    /// Notes what the page needs to know on arriving: whether setup is done,
    /// and the devices from the file. Without devices the form shows at once.
    pub(super) fn settings_home_arrive(&mut self) {
        self.settings.home_ready = self.settings_home_ready();
        let file = self.config.file.home.path().and_then(|path| store::load(&path).ok());
        self.settings.home_cloud_saved = file.as_ref().is_some_and(|file| file.cloud.is_some());
        self.settings.home_cloud_labels = [self.settings_home_cloud_label(true), self.settings_home_cloud_label(false)];
        self.settings.home_listed = file
            .map(|file| {
                file.devices
                    .into_iter()
                    .map(|device| {
                        let place = [device.home, device.room].into_iter().flatten().collect::<Vec<_>>().join(", ");
                        (device.name, place)
                    })
                    .collect()
            })
            .unwrap_or_default();
        if !self.settings.home_ready && self.settings.connect.is_none() {
            self.open_settings_connect();
        }
    }

    /// The devices to list: live from the service, else from the file.
    pub(super) fn settings_home_listed(&self) -> Vec<Listed> {
        if self.settings_home_connected() {
            return self
                .home
                .snapshot
                .devices
                .iter()
                .map(|device| Listed {
                    name: device.name.clone(),
                    place: [device.home.clone(), device.room.clone()]
                        .into_iter()
                        .flatten()
                        .collect::<Vec<_>>()
                        .join(", "),
                    status: crate::home::device_detail(device),
                    answering: matches!(device.reach, Reach::Online | Reach::Cloud { updated: Some(_) }),
                })
                .collect();
        }
        self.settings
            .home_listed
            .iter()
            .map(|(name, place)| Listed {
                name: name.clone(),
                place: place.clone(),
                status: "Not connected".into(),
                answering: false,
            })
            .collect()
    }

    /// Lays the page out from `top`, before scrolling.
    pub(super) fn settings_home_layout(&self, left: f64, top: f64, width: f64) -> HomeFrame {
        let mut frame = HomeFrame {
            cards: Vec::new(),
            buttons: Vec::new(),
            guide: None,
            trouble: None,
            form: None,
            devices: None,
            height: 0.,
        };
        let inner = width - 2. * PAD;
        let mut cursor = top;
        let card_gap = 14.;
        let header = 82.;
        let button_row = |frame: &mut HomeFrame, row_y: f64, hits: &[(Hit, f64)]| {
            let mut row_x = left + PAD;
            for (hit, button_width) in hits {
                frame.buttons.push((Rect::new(row_x, row_y, *button_width, BUTTON_H), *hit));
                row_x += button_width + 10.;
            }
        };
        let connecting = self.settings.connect.as_ref().is_some_and(connect::Connect::busy);
        let listed = self.settings_home_listed();

        if !self.settings.home_ready {
            // 1: the Tuya project.
            let start = cursor;
            cursor += header;
            button_row(
                &mut frame,
                cursor,
                &[(Hit::HomeLink(0), 196.), (Hit::HomeGuide, 176.), (Hit::HomeTrouble, 196.)],
            );
            cursor += BUTTON_H + 12.;
            if self.settings.home_guide {
                let height = guide_height(inner);
                frame.guide = Some(Rect::new(left + PAD, cursor, inner, height));
                cursor += height + 4.;
            }
            if self.settings.home_trouble {
                let height = trouble_height(inner);
                frame.trouble = Some(Rect::new(left + PAD, cursor, inner, height));
                cursor += height + 4.;
            }
            cursor += 8.;
            frame.cards.push(Card {
                rect: Rect::new(left, start, width, cursor - start),
                step: Some(1),
                title: "Create a Tuya cloud project",
                subtitle: "Devices are controlled on your own network; a free Tuya project is needed once, for \
                           their keys."
                    .into(),
                done: false,
            });
            cursor += card_gap;

            // 2: the credentials.
            let start = cursor;
            cursor += header;
            frame.form = Some(Rect::new(left + PAD, cursor, inner, FORM_H));
            cursor += FORM_H + 8.;
            frame.cards.push(Card {
                rect: Rect::new(left, start, width, cursor - start),
                step: Some(2),
                title: "Connect your Tuya account",
                subtitle: "Use the Access ID and Access Secret from your project's Overview page.".into(),
                done: false,
            });
            cursor += card_gap;

            // 3: what it found.
            let start = cursor;
            cursor += header + 8.;
            frame.cards.push(Card {
                rect: Rect::new(left, start, width, cursor - start),
                step: Some(3),
                title: "Your devices",
                subtitle: if connecting {
                    "Looking for your devices...".into()
                } else {
                    "They appear in the notification panel (Super+N) once connected, with the homes and rooms from \
                     your app."
                        .into()
                },
                done: false,
            });
        } else {
            let start = cursor;
            cursor += header;
            if !listed.is_empty() {
                let height = listed.len() as f64 * DEVICE_ROW + 8.;
                frame.devices = Some(Rect::new(left + PAD, cursor, inner, height));
                cursor += height + 8.;
            }
            let mut hits = vec![(Hit::HomeRefresh, 160.), (Hit::HomeLink(1), 150.)];
            if self.settings.connect.is_none() {
                hits.push((Hit::HomeTrouble, 196.));
            } else {
                hits.remove(0);
            }
            button_row(&mut frame, cursor, &hits);
            cursor += BUTTON_H + 10.;
            let cloud = self.settings.home_cloud_saved;
            if cloud {
                button_row(&mut frame, cursor, &[(Hit::HomeCloud(true), 220.), (Hit::HomeCloud(false), 220.)]);
                cursor += BUTTON_H + 10.;
            }
            if self.settings.home_trouble {
                let height = trouble_height(inner);
                frame.trouble = Some(Rect::new(left + PAD, cursor, inner, height));
                cursor += height + 4.;
            }
            cursor += 6.;
            let answering = listed.iter().filter(|device| device.answering).count();
            let homes = {
                let mut places: Vec<&str> = listed
                    .iter()
                    .filter_map(|device| device.place.split(", ").next())
                    .filter(|home| !home.is_empty())
                    .collect();
                places.sort_unstable();
                places.dedup();
                places.len()
            };
            let subtitle = if !self.settings_home_connected() {
                format!(
                    "{} devices imported. Switch on Smart home in the panel above and apply to connect to them.",
                    listed.len()
                )
            } else {
                let homes = if homes > 1 { format!(" in {homes} homes") } else { String::new() };
                format!("{} devices{homes}; {answering} answer right now.", listed.len())
            };
            frame.cards.push(Card {
                rect: Rect::new(left, start, width, cursor - start),
                step: None,
                title: "Devices",
                subtitle,
                done: false,
            });
            if self.settings.connect.is_some() {
                cursor += card_gap;
                let start = cursor;
                cursor += header;
                frame.form = Some(Rect::new(left + PAD, cursor, inner, FORM_H));
                cursor += FORM_H + 8.;
                frame.cards.push(Card {
                    rect: Rect::new(left, start, width, cursor - start),
                    step: None,
                    title: "Refresh devices",
                    subtitle: "Fetch the device list again, for example after adding a light in the app.".into(),
                    done: false,
                });
            }
        }
        frame.height = cursor - top;
        frame
    }

    /// Draws the page's cards, texts and the credentials form; the buttons
    /// are drawn with the other hits.
    pub(super) fn settings_draw_home_page(&self, painter: &mut Painter<'_>, home: &HomeFrame) {
        let theme = &self.theme;
        for card in &home.cards {
            painter
                .shape(card.rect, Shape { border_width: 1., border: theme.border, ..Shape::filled(theme.surface, 8.) });
            let mut title_x = card.rect.x + PAD;
            if let Some(step) = card.step {
                let badge = Rect::new(card.rect.x + PAD, card.rect.y + 18., 26., 26.);
                painter
                    .shape(badge, Shape { radius: 13., fill: (theme.accent, theme.accent_end), ..Default::default() });
                let label = if card.done { "\u{2713}".to_string() } else { step.to_string() };
                painter.text(&label, badge, Style::new(12., theme.on_accent).weight(700).align(Align::Centre));
                title_x = badge.right() + 12.;
            }
            painter.text(
                card.title,
                Rect::new(title_x, card.rect.y + 14., card.rect.right() - title_x - PAD, 22.),
                Style::new(15., theme.text).weight(600),
            );
            painter.text(
                &card.subtitle,
                Rect::new(title_x, card.rect.y + 38., card.rect.right() - title_x - PAD, 36.),
                Style::new(11.5, theme.muted).lines(2),
            );
        }
        if let Some(area) = home.guide {
            painter.fill(area, theme.background.alpha(0.5), 6.);
            let mut line_y = area.y + 8.;
            for (number, step) in GUIDE.iter().enumerate() {
                let height = text_height(step, area.w - 28., 12.);
                painter.text(
                    &format!("{}.", number + 1),
                    Rect::new(area.x + 10., line_y, 18., 18.),
                    Style::new(12., theme.accent).weight(600),
                );
                painter.text(
                    step,
                    Rect::new(area.x + 30., line_y, area.w - 40., height + 4.),
                    Style::new(12., theme.text).lines(4),
                );
                line_y += height + 10.;
            }
        }
        if let Some(area) = home.trouble {
            painter.fill(area, theme.background.alpha(0.5), 6.);
            let mut line_y = area.y + 8.;
            for (problem, fix) in TROUBLE {
                painter.text(
                    problem,
                    Rect::new(area.x + 10., line_y, area.w - 20., 18.),
                    Style::new(12., theme.text).weight(600),
                );
                line_y += 20.;
                let height = text_height(fix, area.w, 12.);
                painter.text(
                    fix,
                    Rect::new(area.x + 10., line_y, area.w - 20., height + 4.),
                    Style::new(12., theme.muted).lines(4),
                );
                line_y += height + 12.;
            }
        }
        if let Some(area) = home.devices {
            let listed = self.settings_home_listed();
            for (index, device) in listed.iter().enumerate() {
                let row = Rect::new(area.x, area.y + index as f64 * DEVICE_ROW, area.w, DEVICE_ROW);
                if index % 2 == 0 {
                    painter.fill(row, theme.background.alpha(0.35), 4.);
                }
                painter.fill(
                    Rect::new(row.x + 10., row.y + row.h / 2. - 3., 6., 6.),
                    if device.answering { theme.accent } else { theme.muted.alpha(0.5) },
                    3.,
                );
                let third = (row.w - 24.) / 3.;
                painter.text(&device.name, Rect::new(row.x + 24., row.y, third, row.h), Style::new(12., theme.text));
                painter.text(
                    &device.place,
                    Rect::new(row.x + 24. + third, row.y, third, row.h),
                    Style::new(12., theme.muted),
                );
                painter.text(
                    &device.status,
                    Rect::new(row.x + 24. + 2. * third, row.y, third - 8., row.h),
                    Style::new(12., theme.muted),
                );
            }
        }
        if home.form.is_some() {
            self.settings_draw_connect(painter);
        }
    }

    /// What a page button says.
    pub(super) fn settings_home_label(&self, hit: Hit) -> (String, Option<bool>) {
        match hit {
            Hit::HomeLink(index) => (LINKS.get(index).map_or("", |(label, _)| label).to_string(), None),
            Hit::HomeGuide => ("How to set it up".into(), Some(self.settings.home_guide)),
            Hit::HomeTrouble => ("Something went wrong?".into(), Some(self.settings.home_trouble)),
            Hit::HomeRefresh => ("Refresh devices".into(), None),
            _ => (String::new(), None),
        }
    }
}
