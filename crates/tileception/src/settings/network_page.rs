//! The Network page: the networks in range, as in the bar's card, and what
//! the card leaves out. The connections that are up with their addresses,
//! saved Wi-Fi networks to forget or stop joining by themselves, joining a
//! network by name (also a hidden one), and NetworkManager's own editor for
//! the rest.
//!
//! Unlike other pages these are not config.toml settings: they act on
//! NetworkManager at once, outside the draft.

use crate::network::manager::{Link, Request, Security, Snapshot};
use crate::text_input::TextInput;
use zeroize::{Zeroize, Zeroizing};

use super::*;

/// The page's title, as it appears in [`CATEGORIES`].
pub(super) const TITLE: &str = "Network";
/// NetworkManager's own editor, for static addresses, VPNs and enterprise Wi-Fi.
const EDITOR: &str = "nm-connection-editor";

const ROW: f64 = 60.;
const DETAILS_ROW: f64 = 96.;
const HEADING: f64 = 24.;
const GAP: f64 = 20.;
const FIELD_H: f64 = 36.;
const BUTTON_W: f64 = 96.;

/// The page's own state, kept while Settings is closed.
#[derive(Default)]
pub(super) struct Page {
    name: TextInput,
    password: TextInput,
    reveal: bool,
    /// The join form's field that has the keyboard: 0 for the name, 1 for
    /// the password.
    typing: Option<usize>,
    /// The saved network whose Forget was clicked once: the next click forgets.
    forget: Option<usize>,
    editor: std::cell::OnceCell<bool>,
}

impl Drop for Page {
    fn drop(&mut self) {
        self.password.text.zeroize();
    }
}

/// The page's index in [`CATEGORIES`].
pub(super) fn network_page() -> usize {
    CATEGORIES.iter().position(|category| category.title == TITLE).expect("Network is a page")
}

/// Where everything on the page goes, below its rows.
pub(super) struct NetworkFrame {
    /// Each card with the heading above it.
    pub cards: Vec<(Rect, &'static str)>,
    /// The switch for Wi-Fi.
    pub wifi: Option<Rect>,
    /// Each network in range: its row, its button (none for networks joined
    /// elsewhere) and its place in the list.
    pub available: Vec<(Rect, Option<Rect>, usize)>,
    pub details: Vec<(Rect, usize)>,
    /// Each saved network's row, switch and Forget button.
    pub saved: Vec<(Rect, Rect, Rect, usize)>,
    pub hidden: Option<Rect>,
    pub name: Option<Rect>,
    pub password: Option<Rect>,
    pub reveal: Option<Rect>,
    pub join: Option<Rect>,
    pub editor: Rect,
    /// A line in place of everything, without NetworkManager.
    pub missing: Option<Rect>,
    pub height: f64,
}

impl NetworkFrame {
    pub(super) fn shift(&mut self, amount: f64) {
        let move_rect = |rect: &mut Rect| rect.y += amount;
        for (rect, _) in &mut self.cards {
            move_rect(rect);
        }
        for (rect, _) in &mut self.details {
            move_rect(rect);
        }
        for (row, button, _) in &mut self.available {
            move_rect(row);
            if let Some(button) = button {
                move_rect(button);
            }
        }
        for (row, switch, forget, _) in &mut self.saved {
            move_rect(row);
            move_rect(switch);
            move_rect(forget);
        }
        for rect in [
            &mut self.wifi,
            &mut self.hidden,
            &mut self.name,
            &mut self.password,
            &mut self.reveal,
            &mut self.join,
            &mut self.missing,
        ]
        .into_iter()
        .flatten()
        {
            move_rect(rect);
        }
        move_rect(&mut self.editor);
    }

    /// The page's controls, for clicks and Tab.
    pub(super) fn hits(&self) -> Vec<(Rect, Hit)> {
        let mut hits = Vec::new();
        hits.extend(self.wifi.map(|rect| (rect, Hit::NetWifi)));
        for (_, button, index) in &self.available {
            hits.extend(button.map(|button| (button, Hit::NetVisible(*index))));
        }
        for (_, switch, forget, index) in &self.saved {
            hits.push((*switch, Hit::NetAuto(*index)));
            hits.push((*forget, Hit::NetForget(*index)));
        }
        hits.extend(self.name.map(|rect| (rect, Hit::NetName)));
        hits.extend(self.password.map(|rect| (rect, Hit::NetPassword)));
        hits.extend(self.reveal.map(|rect| (rect, Hit::NetReveal)));
        hits.extend(self.join.map(|rect| (rect, Hit::NetJoin)));
        if self.missing.is_none() {
            hits.push((self.editor, Hit::NetEditor));
        }
        hits
    }
}

/// A saved network's state, as its row says it.
fn saved_detail(snapshot: &Snapshot, ssid: &[u8], autoconnect: bool) -> String {
    let state = match snapshot.networks.iter().find(|network| network.ssid == ssid) {
        Some(network) if network.link == Link::Connected => "Connected",
        Some(network) if network.link == Link::Connecting => "Connecting",
        Some(_) => "In range",
        None => "Not in range",
    };
    let joins = if autoconnect { "joins by itself" } else { "joins only when you choose it" };
    format!("{state}, {joins}")
}

/// The lines under a connection: addresses, the gateway and DNS, and the
/// interface.
fn detail_lines(details: &crate::network::manager::Details) -> [String; 3] {
    let addresses = if details.addresses.is_empty() {
        "No address yet".to_string()
    } else {
        format!("Address {}", details.addresses.join(", "))
    };
    let mut route = Vec::new();
    if let Some(gateway) = &details.gateway {
        route.push(format!("Gateway {gateway}"));
    }
    if !details.dns.is_empty() {
        route.push(format!("DNS {}", details.dns.join(", ")));
    }
    let mut device = vec![details.interface.clone()];
    if !details.hardware.is_empty() {
        device.push(details.hardware.clone());
    }
    [addresses, route.join(" \u{b7} "), device.join(" \u{b7} ")]
}

impl Tileception {
    fn network_editor_installed(&self) -> bool {
        *self.settings.network.editor.get_or_init(|| {
            std::env::var_os("PATH")
                .is_some_and(|path| std::env::split_paths(&path).any(|dir| dir.join(EDITOR).is_file()))
        })
    }

    /// The network changed: draw the page again if it shows.
    pub(crate) fn settings_network_changed(&self) {
        if self.settings.open && self.settings.page == Some(network_page()) {
            self.invalidate_settings_client();
        }
    }

    /// Joining failed: say so on the page if it shows.
    pub(crate) fn settings_network_failed(&mut self, text: String) {
        if self.settings.open && self.settings.page == Some(network_page()) {
            self.settings.message = text;
            self.settings.error = true;
            self.invalidate_settings_client();
        }
    }

    /// The page's cards with their top left at (left, top).
    pub(super) fn settings_network_layout(&self, left: f64, top: f64, width: f64) -> NetworkFrame {
        let snapshot = &self.network.snapshot;
        let mut frame = NetworkFrame {
            cards: Vec::new(),
            wifi: None,
            available: Vec::new(),
            details: Vec::new(),
            saved: Vec::new(),
            hidden: None,
            name: None,
            password: None,
            reveal: None,
            join: None,
            editor: Rect::new(left, top, width, ROW),
            missing: None,
            height: ROW,
        };
        if !snapshot.running {
            frame.cards.push((Rect::new(left, top + HEADING, width, ROW), "NETWORKMANAGER"));
            frame.missing = Some(Rect::new(left, top + HEADING, width, ROW));
            frame.height = HEADING + ROW;
            return frame;
        }
        let mut cursor = top;
        let card = |cursor: &mut f64, height: f64, heading: &'static str, cards: &mut Vec<(Rect, &'static str)>| {
            let rect = Rect::new(left, *cursor + HEADING, width, height);
            cards.push((rect, heading));
            *cursor = rect.bottom() + GAP;
            rect
        };
        if snapshot.wifi_device.is_some() {
            let rect = card(&mut cursor, ROW, "WI-FI", &mut frame.cards);
            frame.wifi = Some(Rect::new(rect.right() - 58., rect.y + (ROW - 22.) / 2., 40., 22.));
            if snapshot.wifi_enabled && snapshot.wifi_hardware {
                let rows = snapshot.networks.len().max(1);
                let rect = card(&mut cursor, rows as f64 * ROW, "AVAILABLE NETWORKS", &mut frame.cards);
                for (index, network) in snapshot.networks.iter().enumerate() {
                    let row = Rect::new(rect.x, rect.y + index as f64 * ROW, rect.w, ROW);
                    let usable =
                        !matches!(network.security, Security::Enterprise | Security::Wep) || network.link != Link::Idle;
                    let button = usable
                        .then(|| Rect::new(row.right() - BUTTON_W - 16., row.y + (ROW - 32.) / 2., BUTTON_W, 32.));
                    frame.available.push((row, button, index));
                }
            }
        }
        let shown = snapshot.details.len().max(1);
        let rect = card(&mut cursor, shown as f64 * DETAILS_ROW, "CONNECTED", &mut frame.cards);
        for index in 0..snapshot.details.len() {
            frame.details.push((Rect::new(rect.x, rect.y + index as f64 * DETAILS_ROW, rect.w, DETAILS_ROW), index));
        }
        if snapshot.wifi_device.is_some() {
            let rows = snapshot.saved.len().max(1);
            let rect = card(&mut cursor, rows as f64 * ROW, "SAVED NETWORKS", &mut frame.cards);
            for index in 0..snapshot.saved.len() {
                let row = Rect::new(rect.x, rect.y + index as f64 * ROW, rect.w, ROW);
                let forget = Rect::new(row.right() - BUTTON_W - 16., row.y + (ROW - 32.) / 2., BUTTON_W, 32.);
                let switch = Rect::new(forget.x - 56., row.y + (ROW - 22.) / 2., 40., 22.);
                frame.saved.push((row, switch, forget, index));
            }
            // A line of explanation, the two fields and the button.
            let height = 44. + 2. * (FIELD_H + 12.) + 32. + 16.;
            let rect = card(&mut cursor, height, "JOIN A NETWORK", &mut frame.cards);
            frame.hidden = Some(rect);
            let field_x = rect.x + 140.;
            let field_w = (rect.w - 156.).max(120.);
            let name = Rect::new(field_x, rect.y + 44., field_w, FIELD_H);
            let password = Rect::new(field_x, name.bottom() + 12., field_w, FIELD_H);
            frame.reveal = Some(Rect::new(password.right() - 58., password.y + 4., 54., FIELD_H - 8.));
            frame.join = Some(Rect::new(rect.right() - BUTTON_W - 16., password.bottom() + 12., BUTTON_W, 32.));
            frame.name = Some(name);
            frame.password = Some(password);
        }
        let rect = card(&mut cursor, ROW, "MORE", &mut frame.cards);
        frame.editor = Rect::new(rect.right() - 196., rect.y + (ROW - 32.) / 2., 180., 32.);
        frame.height = cursor - GAP - top;
        frame
    }

    /// A click on one of the page's controls.
    pub(super) fn settings_network_activate(&mut self, hit: Hit) {
        let snapshot = &self.network.snapshot;
        let page = &mut self.settings.network;
        if !matches!(hit, Hit::NetForget(_)) {
            page.forget = None;
        }
        match hit {
            Hit::NetWifi => self.network_set_wifi(None),
            Hit::NetVisible(index) => {
                let Some(network) = snapshot.networks.get(index).cloned() else { return };
                let name = network.name();
                if network.link != Link::Idle {
                    self.network_request(Request::Disconnect);
                } else if matches!(network.security, Security::Password { .. }) && network.saved.is_none() {
                    // Its password goes in the form below.
                    page.name = TextInput::default();
                    page.name.insert(&name);
                    page.password.text.zeroize();
                    page.password.clear();
                    page.typing = Some(1);
                    self.settings.focus = Some(Hit::NetPassword);
                    self.settings.message = format!("Type the password for {name} below, then press Join.");
                    self.settings.error = false;
                    let frame = self.settings_frame();
                    if let Some(password) = frame.network.as_ref().and_then(|network| network.password) {
                        let area = viewport(&frame);
                        let below = (password.bottom() + 60. - area.bottom()).max(0.);
                        self.settings.scroll = (self.settings.scroll + below).min(frame.max_scroll);
                    }
                } else {
                    self.settings.message = format!("Joining {name}\u{2026}");
                    self.settings.error = false;
                    self.network_connect(network, None);
                }
            }
            Hit::NetAuto(index) => {
                if let Some(saved) = snapshot.saved.get(index) {
                    let request = Request::Autoconnect(saved.path.clone(), !saved.autoconnect);
                    self.network_request(request);
                }
            }
            Hit::NetForget(index) => {
                let Some(saved) = snapshot.saved.get(index) else { return };
                if page.forget == Some(index) {
                    page.forget = None;
                    self.settings.message = format!("Forgot {}.", saved.name);
                    self.settings.error = false;
                    let request = Request::Forget(saved.path.clone());
                    self.network_request(request);
                } else {
                    page.forget = Some(index);
                    self.settings.message = format!("Click Forget again to remove {} and its password.", saved.name);
                    self.settings.error = false;
                }
            }
            Hit::NetName => page.typing = Some(0),
            Hit::NetPassword => page.typing = Some(1),
            Hit::NetReveal => page.reveal = !page.reveal,
            Hit::NetJoin => self.settings_network_join(),
            Hit::NetEditor if self.network_editor_installed() => self.spawn(EDITOR, &[]),
            _ => {}
        }
    }

    /// A click elsewhere takes the keyboard from the hidden network's fields.
    pub(super) fn settings_network_release(&mut self, hit: Hit) {
        if !matches!(hit, Hit::NetName | Hit::NetPassword) {
            self.settings.network.typing = None;
        }
    }

    fn settings_network_join(&mut self) {
        let page = &mut self.settings.network;
        page.typing = None;
        let name = page.name.text.trim().to_string();
        if name.is_empty() {
            self.settings.message = "Type the network's name to join it.".into();
            self.settings.error = true;
            return;
        }
        let password = (!page.password.text.is_empty()).then(|| Zeroizing::new(page.password.text.clone()));
        if password.as_ref().is_some_and(|password| !crate::network::valid_password(password)) {
            self.settings.message = "Wi-Fi passwords have 8 to 63 characters.".into();
            self.settings.error = true;
            return;
        }
        page.password.text.zeroize();
        page.password.clear();
        self.settings.message = format!("Joining {name}\u{2026}");
        self.settings.error = false;
        let in_range = self.network.snapshot.networks.iter().find(|network| network.name() == name).cloned();
        match in_range {
            Some(network) => self.network_connect(network, password),
            None => self.network_request(Request::Hidden { ssid: name.into_bytes(), password }),
        }
    }

    /// Whether a hidden network's field has the keyboard: only while the
    /// page shows.
    pub(super) fn settings_network_typing(&self) -> bool {
        self.settings.network.typing.is_some()
            && self.settings.page == Some(network_page())
            && self.settings.query.is_empty()
    }

    /// A key for the hidden network's fields; returns whether they took it.
    pub(super) fn settings_network_key(&mut self, sym: u32, mods: Mods) -> bool {
        if !self.settings_network_typing() {
            self.settings.network.typing = None;
            return false;
        }
        let Some(field) = self.settings.network.typing else { return false };
        let page = &mut self.settings.network;
        match sym {
            keysyms::KEY_Escape => page.typing = None,
            keysyms::KEY_Tab | keysyms::KEY_ISO_Left_Tab => {
                let back = mods.shift || sym == keysyms::KEY_ISO_Left_Tab;
                page.typing = match (field, back) {
                    (0, false) => Some(1),
                    (1, true) => Some(0),
                    _ => None,
                };
                self.settings.focus = Some(match page.typing {
                    Some(0) => Hit::NetName,
                    Some(_) => Hit::NetPassword,
                    None if back => Hit::NetAuto(0),
                    None => Hit::NetJoin,
                });
            }
            keysyms::KEY_Return | keysyms::KEY_KP_Enter if field == 0 => {
                page.typing = Some(1);
                self.settings.focus = Some(Hit::NetPassword);
            }
            keysyms::KEY_Return | keysyms::KEY_KP_Enter => self.settings_network_join(),
            _ if mods.ctrl && matches!(sym, keysyms::KEY_v | keysyms::KEY_V) => {
                self.paste(crate::launcher::Paste::Settings(self.settings.editor_epoch));
            }
            _ if mods.ctrl && matches!(sym, keysyms::KEY_c | keysyms::KEY_C | keysyms::KEY_x | keysyms::KEY_X) => {
                // A hidden password stays on the page.
                let reveal = page.reveal;
                let input = if field == 0 { &mut page.name } else { &mut page.password };
                let range = input.selection();
                if (field == 0 || reveal) && !range.is_empty() {
                    let text = input.text[range].to_string();
                    if matches!(sym, keysyms::KEY_x | keysyms::KEY_X) {
                        input.insert("");
                    }
                    self.copy_text(text);
                }
            }
            _ => {
                let input = if field == 0 { &mut page.name } else { &mut page.password };
                sections::edit_key(input, &mut self.settings.compose, sym, mods);
            }
        }
        self.schedule_render();
        true
    }

    /// Pasted text for the field that has the keyboard, as one line.
    pub(super) fn settings_network_paste(&mut self, text: &str) -> bool {
        if !self.settings_network_typing() {
            return false;
        }
        let page = &mut self.settings.network;
        let Some(field) = page.typing else { return false };
        let text: String = text.chars().filter(|character| !character.is_control()).collect();
        if field == 0 { &mut page.name } else { &mut page.password }.insert(&text);
        true
    }

    /// A field's text as drawn, and its caret and selection there.
    fn settings_network_shown(&self, field: usize) -> (String, usize, Option<usize>) {
        let page = &self.settings.network;
        let input = if field == 0 { &page.name } else { &page.password };
        if field == 0 || page.reveal {
            return (input.text.clone(), input.cursor, input.anchor);
        }
        let dot = "\u{2022}";
        let dots_before = |offset: usize| input.text[..offset].chars().count() * dot.len();
        (dot.repeat(input.text.chars().count()), dots_before(input.cursor), input.anchor.map(dots_before))
    }

    /// Places the caret where a field was pressed; `extend` selects.
    pub(super) fn settings_network_caret(&mut self, frame: &Frame, point: Point, extend: bool) -> bool {
        let Some(field) = self.settings.network.typing else { return false };
        let Some(network) = &frame.network else { return false };
        let Some(rect) = (if field == 0 { network.name } else { network.password }) else { return false };
        let (shown, cursor, _) = self.settings_network_shown(field);
        let position = self.text.borrow_mut().field_hit(
            &shown,
            &self.theme.font,
            12.,
            (rect.w - if field == 0 { 20. } else { 76. }) as f32,
            cursor,
            (point.x - rect.x - 10.).max(0.) as f32,
        );
        let page = &mut self.settings.network;
        let input = if field == 0 { &mut page.name } else { &mut page.password };
        let position = if field == 0 || page.reveal {
            position
        } else {
            let characters = shown[..position.min(shown.len())].chars().count();
            input.text.char_indices().nth(characters).map_or(input.text.len(), |(offset, _)| offset)
        };
        input.move_to(position, extend);
        true
    }

    pub(super) fn settings_draw_network_page(&self, painter: &mut Painter<'_>, frame: &NetworkFrame, pointer: Point) {
        let theme = &self.theme;
        let snapshot = &self.network.snapshot;
        let page = &self.settings.network;
        for (card, heading) in &frame.cards {
            painter.text(
                heading,
                Rect::new(card.x + 2., card.y - 24., card.w, 20.),
                Style::new(10., theme.muted).weight(600),
            );
            painter.shape(*card, Shape { border_width: 1., border: theme.border, ..Shape::filled(theme.surface, 8.) });
        }
        let title = Style::new(12., theme.text).weight(500);
        let small = Style::new(11., theme.muted);
        let separator = |painter: &mut Painter<'_>, row: Rect, last: bool| {
            if !last {
                painter.fill(Rect::new(row.x + 16., row.bottom() - 1., row.w - 32., 1.), theme.border, 0.);
            }
        };
        let focused = |hit: Hit| self.settings.focus == Some(hit);
        let button = |painter: &mut Painter<'_>, rect: Rect, label: &str, hit: Hit, primary: bool| {
            let look = if primary { Look::Primary } else { Look::Plain };
            painter.button(rect, Button::new(label, look).focused(focused(hit)), rect.contains(pointer));
        };

        if let Some(rect) = frame.missing {
            painter.text(
                "NetworkManager is not running, so there is nothing to set here.",
                Rect::new(rect.x + 18., rect.y, rect.w - 36., rect.h),
                Style::new(12., theme.muted),
            );
            return;
        }

        if let (Some(switch), Some((card, _))) = (frame.wifi, frame.cards.first()) {
            let row = Rect::new(card.x, card.y, card.w, ROW);
            let top = row.y + (ROW - 42.) / 2.;
            painter.text("Wi-Fi", Rect::new(row.x + 18., top, row.w - 120., 21.), title);
            let state = match () {
                _ if !snapshot.wifi_hardware => "Off: a hardware switch or airplane mode",
                _ if snapshot.wifi_enabled => "On",
                _ => "Off",
            };
            painter.text(state, Rect::new(row.x + 18., top + 22., row.w - 120., 20.), small);
            let usable = snapshot.wifi_hardware;
            crate::panel::draw_switch(painter, switch, snapshot.wifi_enabled && usable, false, usable);
            if focused(Hit::NetWifi) {
                painter.shape(
                    switch.inset(-3.),
                    Shape { border_width: 1., border: theme.accent, radius: 14., ..Default::default() },
                );
            }
        }

        let available = frame.cards.iter().find(|(_, heading)| *heading == "AVAILABLE NETWORKS").map(|(rect, _)| *rect);
        if snapshot.networks.is_empty()
            && let Some(card) = available
        {
            painter.text("Looking for networks\u{2026}", Rect::new(card.x + 18., card.y, card.w - 36., card.h), small);
        }
        let last = frame.available.len().saturating_sub(1);
        for (position, (row, join, index)) in frame.available.iter().enumerate() {
            let Some(network) = snapshot.networks.get(*index) else { continue };
            separator(painter, *row, position == last);
            let lit = network.link == Link::Connected;
            let badge = Rect::new(row.x + 16., row.y + 16., 28., 28.);
            let colour = if lit { theme.accent } else { theme.muted };
            painter.fill(badge, colour.alpha(0.14), 6.);
            painter.glyph(glyph::WIFI, badge, 15, colour.alpha(0.3));
            painter.glyph(crate::network::signal(network.strength), badge, 15, colour);
            let text_x = badge.right() + 12.;
            let text_w = join.map_or(row.right(), |join| join.x) - text_x - 34.;
            let top = row.y + (ROW - 42.) / 2.;
            painter.text(&network.name(), Rect::new(text_x, top, text_w, 21.), if lit { title.bold() } else { title });
            let asked = self.network.asked_for(&network.ssid);
            let detail = crate::network::detail(network, asked);
            painter.text(detail, Rect::new(text_x, top + 22., text_w, 20.), small);
            if network.security.secured() {
                let lock = Rect::new(text_x + text_w + 4., row.y, 24., row.h);
                painter.glyph(glyph::LOCK, lock, 13, theme.muted.alpha(0.8));
            }
            if let Some(join) = join {
                let label = if network.link == Link::Idle { "Join" } else { "Disconnect" };
                button(painter, *join, label, Hit::NetVisible(*index), false);
            }
        }

        let connected = frame.cards.iter().find(|(_, heading)| *heading == "CONNECTED").map(|(rect, _)| *rect);
        if snapshot.details.is_empty()
            && let Some(card) = connected
        {
            painter.text("Not connected", Rect::new(card.x + 18., card.y, card.w - 36., card.h), small);
        }
        let last = frame.details.len().saturating_sub(1);
        for (position, (row, index)) in frame.details.iter().enumerate() {
            let Some(details) = snapshot.details.get(*index) else { continue };
            separator(painter, *row, position == last);
            let badge = Rect::new(row.x + 16., row.y + 16., 28., 28.);
            painter.fill(badge, theme.accent.alpha(0.14), 6.);
            let icon = if details.wifi { glyph::WIFI } else { glyph::ETHERNET };
            painter.glyph(icon, badge, 15, theme.accent);
            let text_x = badge.right() + 12.;
            let text_w = row.right() - text_x - 16.;
            let kind = if details.wifi { "Wi-Fi" } else { "Wired" };
            let name =
                if details.name.is_empty() { kind.to_string() } else { format!("{kind} \u{b7} {}", details.name) };
            painter.text(&name, Rect::new(text_x, row.y + 12., text_w, 21.), title);
            for (line, text) in detail_lines(details).iter().enumerate() {
                painter.text(text, Rect::new(text_x, row.y + 34. + line as f64 * 18., text_w, 18.), small);
            }
        }

        let saved_card = frame.cards.iter().find(|(_, heading)| *heading == "SAVED NETWORKS").map(|(rect, _)| *rect);
        if snapshot.saved.is_empty()
            && let Some(card) = saved_card
        {
            painter.text(
                "No saved networks yet. Networks you join are saved here.",
                Rect::new(card.x + 18., card.y, card.w - 36., card.h),
                small,
            );
        }
        let last = frame.saved.len().saturating_sub(1);
        for (position, (row, switch, forget, index)) in frame.saved.iter().enumerate() {
            let Some(saved) = snapshot.saved.get(*index) else { continue };
            separator(painter, *row, position == last);
            let top = row.y + (ROW - 42.) / 2.;
            let text_w = switch.x - row.x - 30.;
            painter.text(&saved.name, Rect::new(row.x + 18., top, text_w, 21.), title);
            let detail = saved_detail(snapshot, &saved.ssid, saved.autoconnect);
            painter.text(&detail, Rect::new(row.x + 18., top + 22., text_w, 20.), small);
            crate::panel::draw_switch(painter, *switch, saved.autoconnect, false, true);
            if focused(Hit::NetAuto(*index)) {
                painter.shape(
                    switch.inset(-3.),
                    Shape { border_width: 1., border: theme.accent, radius: 14., ..Default::default() },
                );
            }
            let armed = page.forget == Some(*index);
            button(painter, *forget, if armed { "Confirm" } else { "Forget" }, Hit::NetForget(*index), false);
        }

        if let Some(card) = frame.hidden {
            painter.text(
                "Choose a network above, or type the name of one that hides it. Leave the password empty for an open network.",
                Rect::new(card.x + 18., card.y + 10., card.w - 36., 24.),
                small,
            );
            for (field, rect, label) in [(0, frame.name, "Name"), (1, frame.password, "Password")] {
                let Some(rect) = rect else { continue };
                painter.text(label, Rect::new(card.x + 18., rect.y, 110., rect.h), Style::new(12., theme.text));
                let typing = page.typing == Some(field);
                let hit = if field == 0 { Hit::NetName } else { Hit::NetPassword };
                painter.shape(
                    rect,
                    Shape {
                        border_width: 1.,
                        border: if typing || focused(hit) { theme.accent } else { theme.border },
                        ..Shape::filled(theme.background.alpha(0.6), 6.)
                    },
                );
                let text_w = (rect.w - if field == 0 { 20. } else { 76. }) as f32;
                let (shown, cursor, anchor) = self.settings_network_shown(field);
                if shown.is_empty() && !typing {
                    let hint = if field == 0 { "As the network is called" } else { "Optional" };
                    painter.text(
                        hint,
                        Rect::new(rect.x + 10., rect.y, f64::from(text_w), rect.h),
                        Style::new(12., theme.muted.alpha(0.7)),
                    );
                }
                let look =
                    crate::text::Field { cursor, anchor, visible: typing, accent: theme.accent.straight_rgba8() };
                if let Some(image) = self.text.borrow_mut().field(&shown, &theme.font, 12., theme.text, text_w, look) {
                    painter.image(&image.buffer, (rect.x + 10., rect.y + (rect.h - image.height) / 2.));
                }
            }
            if let Some(reveal) = frame.reveal {
                let label = if page.reveal { "Hide" } else { "Show" };
                let colour = if reveal.contains(pointer) || focused(Hit::NetReveal) { theme.text } else { theme.muted };
                painter.text(label, reveal, Style::new(11., colour).align(Align::Centre));
            }
            if let Some(join) = frame.join {
                button(painter, join, "Join", Hit::NetJoin, true);
            }
        }

        if let Some((card, _)) = frame.cards.iter().find(|(_, heading)| *heading == "MORE") {
            let installed = self.network_editor_installed();
            let text = if installed {
                "Static addresses, VPNs and enterprise Wi-Fi are set up in NetworkManager's editor."
            } else {
                "Install nm-connection-editor to set up static addresses, VPNs and enterprise Wi-Fi."
            };
            let text_w = if installed { frame.editor.x - card.x - 30. } else { card.w - 36. };
            painter.text(text, Rect::new(card.x + 18., card.y, text_w, card.h), Style::new(11., theme.muted).lines(2));
            if installed {
                button(painter, frame.editor, "Open editor", Hit::NetEditor, false);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::network::manager::Details;

    #[test]
    fn says_where_a_connection_is() {
        let details = Details {
            wifi: true,
            name: "Home".into(),
            interface: "wlp2s0".into(),
            hardware: "AA:BB:CC:DD:EE:FF".into(),
            addresses: vec!["192.168.1.23/24".into(), "fd00::23/64".into()],
            gateway: Some("192.168.1.1".into()),
            dns: vec!["192.168.1.1".into()],
        };
        assert_eq!(
            detail_lines(&details),
            [
                "Address 192.168.1.23/24, fd00::23/64".to_string(),
                "Gateway 192.168.1.1 \u{b7} DNS 192.168.1.1".to_string(),
                "wlp2s0 \u{b7} AA:BB:CC:DD:EE:FF".to_string(),
            ]
        );
        let bare =
            Details { addresses: Vec::new(), gateway: None, dns: Vec::new(), hardware: String::new(), ..details };
        assert_eq!(detail_lines(&bare), ["No address yet".to_string(), String::new(), "wlp2s0".to_string()]);
    }

    #[test]
    fn saved_rows_say_whether_they_join_by_themselves() {
        let snapshot = Snapshot::default();
        assert_eq!(saved_detail(&snapshot, b"Home", true), "Not in range, joins by itself");
        assert_eq!(saved_detail(&snapshot, b"Home", false), "Not in range, joins only when you choose it");
    }
}
