//! The network chip in the bar and the card it zooms into: the Wi-Fi
//! networks around, a switch for Wi-Fi and the wired connection, through
//! NetworkManager. A click on a network joins it; a secured one asks for its
//! password in the card. While the card is open it has the keyboard.
//!
//! [`manager`] talks to NetworkManager on threads of its own; this side
//! keeps the latest [`Snapshot`] and draws it.

pub(crate) mod manager;

use std::sync::mpsc;

use smithay::input::keyboard::{keysyms, xkb};
use smithay::input::pointer::CursorIcon;
use smithay::reexports::calloop::{RegistrationToken, channel};
use tileception_core::{Point, Rect};
use zeroize::{Zeroize, Zeroizing};

use crate::{
    Tileception,
    bar::Zoom,
    config::Mods,
    shapes::Shape,
    text_input::TextInput,
    ui::{Align, Button, Look, Painter, PointerTarget, Style, glyph},
};
use manager::{Event, Input, Link, Request, Security, Snapshot, Wifi};

const CARD_W: f64 = 340.0;
const HEADER_H: f64 = 52.0;
const ROW_H: f64 = 44.0;
/// Networks shown at once; the list scrolls for more.
const ROWS: usize = 6;
/// A line instead of the list, such as "Wi-Fi is off".
const MESSAGE_H: f64 = 56.0;
const PROMPT_H: f64 = 132.0;
const FOOTER_H: f64 = 38.0;
const PAD: f64 = 10.0;

/// What the compositor knows about the network, and the card's state.
#[derive(Default)]
pub struct Network {
    pub(crate) zoom: Zoom,
    pub snapshot: Snapshot,
    inputs: Option<mpsc::Sender<Input>>,
    source: Option<RegistrationToken>,
    /// The keyboard's network, by its place in the list.
    selected: usize,
    /// The first network shown.
    first: usize,
    /// Scrolling not yet turned into rows.
    scroll: f64,
    prompt: Option<Prompt>,
    /// A line under the list: what is happening or what went wrong.
    status: Option<Status>,
    /// The network asked for, until NetworkManager says how it went.
    asked: Option<Vec<u8>>,
    compose: Option<xkb::compose::State>,
}

/// Asking for a network's password.
struct Prompt {
    network: Wifi,
    input: TextInput,
    reveal: bool,
}

impl Drop for Prompt {
    fn drop(&mut self) {
        self.input.text.zeroize();
    }
}

struct Status {
    text: String,
    error: bool,
}

impl Network {
    /// Whether this network was asked for and has not answered yet.
    pub(crate) fn asked_for(&self, ssid: &[u8]) -> bool {
        self.asked.as_deref() == Some(ssid)
    }
}

impl Drop for Network {
    fn drop(&mut self) {
        if let Some(inputs) = self.inputs.take() {
            let _ = inputs.send(Input::Stop);
        }
    }
}

/// What a click on the card does.
#[derive(Debug, Clone, PartialEq)]
enum Hit {
    Switch,
    Network(Vec<u8>),
    Disconnect,
    Field,
    Reveal,
    Cancel,
    Join,
    Settings,
}

impl crate::ui::PointerAction for Hit {
    fn middle_click(&self, state: &mut Tileception, rect: Rect, point: Point) -> bool {
        if *self != Hit::Field {
            return false;
        }
        state.network_caret(rect, point);
        state.paste_primary(crate::launcher::Paste::Network);
        true
    }

    fn activate(&self, state: &mut Tileception, rect: Rect, point: Point) {
        match self {
            Hit::Switch => state.network_set_wifi(None),
            Hit::Network(ssid) => {
                if let Some(index) = state.network.snapshot.networks.iter().position(|network| &network.ssid == ssid) {
                    state.network.selected = index;
                    state.network_choose(index, false);
                }
            }
            Hit::Disconnect => state.network_request(Request::Disconnect),
            Hit::Field => state.network_caret(rect, point),
            Hit::Reveal => {
                if let Some(prompt) = &mut state.network.prompt {
                    prompt.reveal = !prompt.reveal;
                }
            }
            Hit::Cancel => state.network.prompt = None,
            Hit::Join => state.network_join(),
            Hit::Settings => {
                state.close_network();
                state.open_settings_named("Network");
            }
        }
        state.schedule_render();
    }
}

/// Where everything is on the card.
struct Card {
    switch: Option<Rect>,
    wired: Option<Rect>,
    /// The list or the message in its place.
    body: Rect,
    /// Each network shown: its place in the list, its row and, when it is the
    /// one Wi-Fi is on, the disconnect button.
    rows: Vec<(usize, Rect, Option<Rect>)>,
    field: Option<Rect>,
    reveal: Option<Rect>,
    cancel: Option<Rect>,
    join: Option<Rect>,
    footer: Rect,
    settings: Option<Rect>,
}

/// The signal icon for a strength in percent.
pub(crate) fn signal(strength: u8) -> &'static str {
    match strength {
        0..=24 => glyph::WIFI_0,
        25..=49 => glyph::WIFI_1,
        50..=74 => glyph::WIFI_2,
        _ => glyph::WIFI,
    }
}

/// WPA passwords have 8 to 63 characters, or are 64 hexadecimal digits.
pub(crate) fn valid_password(password: &str) -> bool {
    let length = password.chars().count();
    (8..=63).contains(&length) || (length == 64 && password.chars().all(|character| character.is_ascii_hexdigit()))
}

/// What a network row says under the name.
pub(crate) fn detail(network: &Wifi, asked: bool) -> &'static str {
    match network.link {
        Link::Connected => "Connected",
        Link::Connecting => "Connecting\u{2026}",
        Link::Idle if asked => "Connecting\u{2026}",
        Link::Idle if network.saved.is_some() => "Saved",
        Link::Idle => match network.security {
            Security::Open => "Open",
            Security::Password { .. } => "Secured",
            Security::Enterprise => "Enterprise, set up in Settings",
            Security::Wep => "WEP, not supported",
        },
    }
}

impl Tileception {
    /// Starts following NetworkManager: on `address`, a stand-in bus for
    /// tests, else on the system bus.
    pub fn start_network(&mut self, address: Option<String>) {
        let (sender, receiver) = channel::channel::<Event>();
        let token = self.loop_handle.insert_source(receiver, |event, _, state| {
            if let channel::Event::Msg(event) = event {
                state.network_event(event);
            }
        });
        let token = match token {
            Ok(token) => token,
            Err(error) => return tracing::error!("could not follow the network: {error}"),
        };
        let (inputs, incoming) = mpsc::channel();
        let watcher_inputs = inputs.clone();
        let started = std::thread::Builder::new().name("tileception-network".into()).spawn(move || {
            let bus = match &address {
                Some(address) => {
                    zbus::blocking::connection::Builder::address(address.as_str()).and_then(|builder| builder.build())
                }
                None => zbus::blocking::Connection::system(),
            };
            let bus = match bus {
                Ok(bus) => bus,
                Err(error) => return tracing::info!("network: no system bus: {error}"),
            };
            let watched = bus.clone();
            let watcher = std::thread::Builder::new().name("tileception-network-watch".into()).spawn(move || {
                if let Err(error) = manager::watch(&watched, &watcher_inputs) {
                    tracing::warn!("network: could not follow NetworkManager: {error}");
                }
            });
            if let Err(error) = watcher {
                tracing::warn!("network: could not start the watcher: {error}");
            }
            manager::run(&bus, &incoming, &|event| sender.send(event).is_ok());
        });
        if let Err(error) = started {
            return tracing::error!("could not start the network worker: {error}");
        }
        self.network.source = Some(token);
        self.network.inputs = Some(inputs);
        self.network.compose = crate::ui::compose_state();
    }

    pub(crate) fn network_request(&self, request: Request) {
        if let Some(inputs) = &self.network.inputs {
            let _ = inputs.send(Input::Request(request));
        }
    }

    fn network_event(&mut self, event: Event) {
        match event {
            Event::Snapshot(snapshot) => {
                let network = &mut self.network;
                if let Some(asked) = &network.asked
                    && snapshot
                        .networks
                        .iter()
                        .any(|candidate| &candidate.ssid == asked && candidate.link != Link::Idle)
                {
                    network.asked = None;
                    network.status = None;
                }
                network.snapshot = snapshot;
                let count = network.snapshot.networks.len();
                network.selected = network.selected.min(count.saturating_sub(1));
                network.first = network.first.min(count.saturating_sub(ROWS));
                if !network.snapshot.running {
                    self.close_network();
                }
                self.settings_network_changed();
            }
            Event::Failed { ssid, reason, password } => {
                self.network.asked = None;
                let name = String::from_utf8_lossy(&ssid).into_owned();
                let found = self.network.snapshot.networks.iter().find(|network| network.ssid == ssid).cloned();
                let retry = password
                    && found.as_ref().is_some_and(|network| matches!(network.security, Security::Password { .. }));
                if retry && self.network.zoom.open {
                    if let Some(network) = found {
                        self.network_prompt(network);
                    }
                } else if !self.network.zoom.open {
                    let body =
                        if retry { format!("{reason} Open the network card to try again.") } else { reason.clone() };
                    self.notify_locally(&format!("Could not join {name}"), &body, tileception_notify::Urgency::Normal);
                }
                self.network.status = Some(Status { text: format!("{name}: {reason}"), error: true });
                self.settings_network_failed(format!("Could not join {name}: {reason}"));
            }
        }
        self.schedule_render();
    }

    /// The bar's chip: its label, icon and whether it is lit. None without
    /// NetworkManager or network devices, or with `[shell] bar_network` off.
    pub(crate) fn network_chip(&self) -> Option<(String, &'static str, bool)> {
        let snapshot = &self.network.snapshot;
        if !self.shell().bar_network || !snapshot.running {
            return None;
        }
        let connecting = || ("Connecting\u{2026}".to_string(), glyph::WIFI_0, false);
        if snapshot.wired == Some(Link::Connected) {
            return Some(("Wired".into(), glyph::ETHERNET, true));
        }
        match snapshot.current() {
            Some(network) if network.link == Link::Connected => {
                return Some((network.name(), signal(network.strength), true));
            }
            Some(_) => return Some(connecting()),
            None => {}
        }
        if snapshot.wired == Some(Link::Connecting) || self.network.asked.is_some() {
            return Some(connecting());
        }
        if snapshot.wifi_device.is_some() && !(snapshot.wifi_enabled && snapshot.wifi_hardware) {
            return Some(("Wi-Fi off".into(), glyph::WIFI_DISABLED, false));
        }
        (snapshot.wifi_device.is_some() || snapshot.wired.is_some())
            .then(|| ("Offline".to_string(), glyph::WIFI_DISABLED, false))
    }

    /// The bar's network icon in `rect`: Wi-Fi with the strength it lacks
    /// dimmed, the cable, or Wi-Fi off, in the bar's text colour.
    pub(crate) fn draw_network_icon(&self, painter: &mut Painter, rect: Rect) {
        let theme = &self.theme;
        let Some((_, icon, lit)) = self.network_chip() else { return };
        let colour = if lit { theme.text.alpha(0.86) } else { theme.muted };
        if [glyph::WIFI_0, glyph::WIFI_1, glyph::WIFI_2, glyph::WIFI].contains(&icon) {
            painter.glyph(glyph::WIFI, rect, 15, colour.alpha(0.3));
        }
        painter.glyph(icon, rect, 15, colour);
    }

    /// What hovering the chip says: what the network is doing.
    pub(crate) fn network_tooltip(&self) -> (String, String) {
        let snapshot = &self.network.snapshot;
        let state = match self.network_chip() {
            Some((label, _, true)) if label == "Wired" => "Connected by cable".to_string(),
            Some((label, _, true)) => match snapshot.current() {
                Some(network) => format!("Connected to {label}, {}% signal", network.strength),
                None => format!("Connected to {label}"),
            },
            Some((label, ..)) => label,
            None => String::new(),
        };
        let title = if snapshot.wifi_device.is_some() { "Wi-Fi" } else { "Network" };
        (title.to_string(), format!("{state}. Click to choose a network."))
    }

    /// The `network` command and the chip: opens the card or closes it.
    pub fn toggle_network(&mut self) {
        if self.network.zoom.open {
            self.close_network();
            return;
        }
        if !self.network.snapshot.running {
            tracing::info!("the network card needs NetworkManager");
            return;
        }
        if !self.bar_shows() {
            tracing::info!("the network card needs the built-in bar");
            return;
        }
        self.close_calendar();
        self.close_minimap();
        self.close_tray();
        self.close_updates_popover();
        self.close_notifications();
        let duration = self.world.settings.animation;
        self.network.zoom.set(true, duration);
        self.network.selected = 0;
        self.network.first = 0;
        self.network.prompt = None;
        self.network_request(Request::Scan);
        self.force_keyboard_focus_sync();
    }

    /// Zooms the card back into the chip. Returns whether it was open.
    pub fn close_network(&mut self) -> bool {
        self.network.prompt = None;
        let duration = self.world.settings.animation;
        let closed = self.network.zoom.set(false, duration);
        if closed {
            self.force_keyboard_focus_sync();
        }
        closed
    }

    /// `tileception msg network ...`.
    pub fn network_command(&mut self, command: crate::commands::NetworkCommand) -> Result<String, String> {
        use crate::commands::NetworkCommand;
        if !self.network.snapshot.running {
            return Err("NetworkManager is not running".into());
        }
        match command {
            NetworkCommand::Card => self.toggle_network(),
            NetworkCommand::Status => return Ok(status_text(&self.network.snapshot)),
            NetworkCommand::Wifi(enabled) => {
                if self.network.snapshot.wifi_device.is_none() {
                    return Err("there is no Wi-Fi device".into());
                }
                self.network_set_wifi(enabled);
            }
            NetworkCommand::Disconnect => self.network_request(Request::Disconnect),
        }
        Ok("ok\n".into())
    }

    /// Switches Wi-Fi on or off; `None` flips it.
    pub(crate) fn network_set_wifi(&mut self, enabled: Option<bool>) {
        let enabled = enabled.unwrap_or(!self.network.snapshot.wifi_enabled);
        self.network.status = None;
        self.network_request(Request::Wifi(enabled));
    }

    /// Joins the network at `index`, asking for its password first when
    /// needed. From the keyboard, the network Wi-Fi is on disconnects.
    fn network_choose(&mut self, index: usize, keyboard: bool) {
        let Some(network) = self.network.snapshot.networks.get(index).cloned() else { return };
        if network.link != Link::Idle {
            if keyboard {
                self.network_request(Request::Disconnect);
            }
            return;
        }
        match network.security {
            Security::Enterprise | Security::Wep => {
                let text = format!("{} needs more than a password: set it up in Settings, Network", network.name());
                self.network.status = Some(Status { text, error: true });
            }
            Security::Password { .. } if network.saved.is_none() => self.network_prompt(network),
            _ => self.network_connect(network, None),
        }
    }

    fn network_prompt(&mut self, network: Wifi) {
        self.network.status = None;
        self.network.prompt = Some(Prompt { network, input: TextInput::default(), reveal: false });
    }

    pub(crate) fn network_connect(&mut self, network: Wifi, password: Option<Zeroizing<String>>) {
        self.network.status = Some(Status { text: format!("Joining {}\u{2026}", network.name()), error: false });
        self.network.asked = Some(network.ssid.clone());
        self.network_request(Request::Connect {
            ssid: network.ssid,
            access_point: network.access_point,
            saved: network.saved,
            security: network.security,
            password,
        });
    }

    /// Joins the network the prompt is for, with the password typed.
    fn network_join(&mut self) {
        let Some(prompt) = &self.network.prompt else { return };
        if !valid_password(&prompt.input.text) {
            self.network.status = Some(Status { text: "Wi-Fi passwords have 8 to 63 characters".into(), error: true });
            return;
        }
        let password = Zeroizing::new(prompt.input.text.clone());
        let mut network = prompt.network.clone();
        // A failed attempt may have deleted the profile held by the prompt.
        network.saved = self
            .network
            .snapshot
            .networks
            .iter()
            .find(|candidate| candidate.ssid == network.ssid)
            .and_then(|candidate| candidate.saved.clone());
        self.network.prompt = None;
        self.network_connect(network, Some(password));
    }

    /// A key while the card is open.
    pub fn network_key(&mut self, sym: u32, mods: Mods) {
        if self.network.prompt.is_some() {
            self.network_prompt_key(sym, mods);
            self.schedule_render();
            return;
        }
        let count = self.network.snapshot.networks.len();
        let network = &mut self.network;
        match sym {
            keysyms::KEY_Escape => {
                self.close_network();
            }
            keysyms::KEY_Up => network.selected = network.selected.saturating_sub(1),
            keysyms::KEY_Down => network.selected = (network.selected + 1).min(count.saturating_sub(1)),
            keysyms::KEY_Page_Up => network.selected = network.selected.saturating_sub(ROWS),
            keysyms::KEY_Page_Down => network.selected = (network.selected + ROWS).min(count.saturating_sub(1)),
            keysyms::KEY_Home => network.selected = 0,
            keysyms::KEY_End => network.selected = count.saturating_sub(1),
            keysyms::KEY_Return | keysyms::KEY_KP_Enter => {
                let selected = network.selected;
                self.network_choose(selected, true);
            }
            _ => {}
        }
        // Keep the keyboard's network in view.
        let network = &mut self.network;
        if network.selected < network.first {
            network.first = network.selected;
        } else if network.selected >= network.first + ROWS {
            network.first = network.selected + 1 - ROWS;
        }
        self.schedule_render();
    }

    /// A key for the password field: it edits like every other field.
    fn network_prompt_key(&mut self, sym: u32, mods: Mods) {
        let Some(prompt) = &mut self.network.prompt else { return };
        let input = &mut prompt.input;
        match sym {
            keysyms::KEY_Escape => self.network.prompt = None,
            keysyms::KEY_Return | keysyms::KEY_KP_Enter => self.network_join(),
            _ => {
                // A hidden password stays on the card.
                input.hidden = !prompt.reveal;
                let key = input.key(&mut self.network.compose, sym, mods);
                self.field_clipboard(key.clipboard, crate::launcher::Paste::Network);
            }
        }
    }

    /// Whether the password field takes pasted text.
    pub(crate) fn network_typing(&self) -> bool {
        self.network.zoom.open && self.network.prompt.is_some()
    }

    /// Pasted text arriving for the password, as one line.
    pub(crate) fn network_insert_paste(&mut self, text: &str) {
        if let Some(prompt) = &mut self.network.prompt {
            prompt.input.paste(text);
        }
    }

    /// Places the caret where the password field was clicked.
    fn network_caret(&mut self, field: Rect, point: Point) {
        let Some(prompt) = &mut self.network.prompt else { return };
        // A hidden password stays on the card.
        prompt.input.hidden = !prompt.reveal;
        prompt.input.click_masked(
            &mut self.text.borrow_mut(),
            "\u{2022}",
            &self.theme.font,
            self.theme.font_size * 0.92,
            (field.w - 70.0) as f32,
            (point.x - field.x - 10.0).max(0.0) as f32,
            false,
        );
        let selected = prompt.input.selected_text();
        self.offer_primary(selected);
    }

    /// Scrolling over the open card, in wheel notches (positive is down).
    pub(crate) fn network_scroll(&mut self, amount: f64) {
        let network = &mut self.network;
        network.scroll += amount;
        let count = network.snapshot.networks.len();
        while network.scroll.abs() >= 1.0 {
            let down = network.scroll > 0.0;
            network.scroll -= network.scroll.signum();
            network.first = if down {
                (network.first + 1).min(count.saturating_sub(ROWS))
            } else {
                network.first.saturating_sub(1)
            };
        }
        self.schedule_render();
    }

    /// Whether the card shows the list of networks, as opposed to a message.
    fn network_listing(&self) -> bool {
        let snapshot = &self.network.snapshot;
        snapshot.wifi_device.is_some() && snapshot.wifi_enabled && snapshot.wifi_hardware
    }

    /// The card at its full size for a chip at `chip`: below it (above it
    /// for a bottom bar), and kept on the screen.
    pub(crate) fn network_card_for(&self, chip: Rect) -> Rect {
        let out = self.output_rect();
        let snapshot = &self.network.snapshot;
        let mut height = HEADER_H + FOOTER_H;
        if snapshot.wired.is_some() {
            height += ROW_H + 4.0;
        }
        // The prompt takes the list's place without changing the card's size.
        height += if self.network_listing() {
            ROWS as f64 * ROW_H + 8.0
        } else if self.network.prompt.is_some() {
            PROMPT_H
        } else if snapshot.wifi_device.is_some() || snapshot.wired.is_none() {
            MESSAGE_H
        } else {
            0.0
        };
        let width = CARD_W.min(out.w - 16.0).max(1.0);
        let height = height.min(out.h - crate::bar::BAR_H - 16.0).max(1.0);
        let left = (chip.center().x - width / 2.0).min(out.right() - width - 8.0).max(out.x + 8.0);
        let edge = if self.bar_at_bottom() { out.bottom() - crate::bar::BAR_H } else { out.y };
        Rect::new(left, self.bar_popup_y(Rect::new(out.x, edge, out.w, crate::bar::BAR_H), height), width, height)
    }

    fn network_card(&self) -> Rect {
        self.network_card_for(self.network_anchor())
    }

    fn network_layout(&self, card: Rect) -> Card {
        let snapshot = &self.network.snapshot;
        let inner = Rect::new(card.x + PAD, card.y, card.w - 2.0 * PAD, card.h);
        let switch = snapshot
            .wifi_device
            .is_some()
            .then(|| Rect::new(inner.right() - 40.0, card.y + (HEADER_H - 20.0) / 2.0, 36.0, 20.0));
        let mut top = card.y + HEADER_H;
        let wired = snapshot.wired.map(|_| {
            let rect = Rect::new(inner.x, top, inner.w, ROW_H);
            top += ROW_H + 4.0;
            rect
        });
        let footer = Rect::new(inner.x, card.bottom() - FOOTER_H, inner.w, FOOTER_H);
        let body = Rect::new(inner.x, top, inner.w, (footer.y - top).max(0.0));
        let settings = Some(Rect::new(footer.right() - 124.0, footer.y + 5.0, 124.0, 28.0));
        let mut layout = Card {
            switch,
            wired,
            body,
            rows: Vec::new(),
            field: None,
            reveal: None,
            cancel: None,
            join: None,
            footer,
            settings,
        };
        if self.network.prompt.is_some() {
            let field = Rect::new(body.x, body.y + 34.0, body.w, 36.0);
            layout.reveal = Some(Rect::new(field.right() - 58.0, field.y + 4.0, 54.0, field.h - 8.0));
            layout.field = Some(field);
            let buttons = field.bottom() + 14.0;
            layout.join = Some(Rect::new(body.right() - 96.0, buttons, 96.0, 32.0));
            layout.cancel = Some(Rect::new(body.right() - 200.0, buttons, 96.0, 32.0));
        } else if self.network_listing() {
            let shown = snapshot.networks.iter().enumerate().skip(self.network.first).take(ROWS);
            for (row, (index, network)) in shown.enumerate() {
                let rect = Rect::new(body.x, body.y + row as f64 * ROW_H, body.w, ROW_H);
                let disconnect = (network.link != Link::Idle)
                    .then(|| Rect::new(rect.right() - 96.0, rect.y + (ROW_H - 28.0) / 2.0, 92.0, 28.0));
                layout.rows.push((index, rect, disconnect));
            }
        }
        layout
    }

    /// The controls on the open card, for hit-testing and the pointer.
    pub(crate) fn network_pointer_target(&self, point: Point) -> Option<PointerTarget> {
        let card = self.network_card();
        if !card.contains(point) {
            return None;
        }
        let layout = self.network_layout(card);
        let snapshot = &self.network.snapshot;
        let mut targets: Vec<(Rect, Hit)> = Vec::new();
        if let Some(field) = layout.field {
            if let Some(reveal) = layout.reveal
                && reveal.contains(point)
            {
                return Some(PointerTarget::button(reveal, Hit::Reveal));
            }
            if field.contains(point) {
                return Some(PointerTarget::new(field, Hit::Field, CursorIcon::Text));
            }
        }
        targets.extend(layout.switch.map(|switch| (switch, Hit::Switch)));
        targets.extend(layout.cancel.map(|cancel| (cancel, Hit::Cancel)));
        targets.extend(layout.join.map(|join| (join, Hit::Join)));
        targets.extend(layout.settings.map(|settings| (settings, Hit::Settings)));
        for (index, rect, disconnect) in &layout.rows {
            targets.extend(disconnect.map(|button| (button, Hit::Disconnect)));
            let network = &snapshot.networks[*index];
            if network.link == Link::Idle {
                targets.push((*rect, Hit::Network(network.ssid.clone())));
            }
        }
        targets.into_iter().find(|(rect, _)| rect.contains(point)).map(|(rect, hit)| PointerTarget::button(rect, hit))
    }

    /// A press while the card is open, after its controls had their turn:
    /// outside the card it zooms the card closed.
    pub(crate) fn network_press(&mut self, point: Point) {
        if !self.network_card().contains(point) {
            self.close_network();
        }
    }

    /// The chip grown into the card, along the same curve the camera uses.
    pub(crate) fn draw_network_card(&self, painter: &mut Painter, chip: Rect, level: f64) {
        let card = self.network_card_for(chip);
        let rect = chip.zoom_lerp(&card, level);
        let (alpha, offset) = (painter.alpha, painter.offset);
        painter.popup(rect);
        painter.alpha = alpha * crate::bar::card_contents(level);
        let (from, target) = (card.center(), rect.center());
        painter.offset = (offset.0 + target.x - from.x, offset.1 + target.y - from.y);
        if painter.alpha > 0.0 {
            self.draw_network_contents(painter, card);
        }
        (painter.alpha, painter.offset) = (alpha, offset);
    }

    fn draw_network_contents(&self, painter: &mut Painter, card: Rect) {
        let theme = &self.theme;
        let snapshot = &self.network.snapshot;
        let layout = self.network_layout(card);
        let pointer = Point::new(self.pointer.x, self.pointer.y);
        let hover = |rect: Rect| rect.contains(pointer);

        // The header: what Wi-Fi is doing, and its switch.
        let header = Rect::new(card.x + PAD + 6.0, card.y, card.w - 2.0 * PAD - 60.0, HEADER_H);
        let title = if snapshot.wifi_device.is_some() { "Wi-Fi" } else { "Network" };
        let title_style = Style::new(theme.font_size * 1.05, theme.text).bold();
        let (title_w, _) = painter.text(title, header, title_style);
        let summary = match snapshot.current() {
            Some(network) if network.link == Link::Connected => network.name(),
            Some(_) => "Connecting\u{2026}".into(),
            None if snapshot.wifi_device.is_none() => String::new(),
            None if !snapshot.wifi_hardware || !snapshot.wifi_enabled => "Off".into(),
            None => "Not connected".into(),
        };
        let summary_rect =
            Rect::new(header.x + title_w + 10.0, header.y, (header.w - title_w - 10.0).max(1.0), header.h);
        painter.text(&summary, summary_rect, Style::new(theme.font_size * 0.88, theme.muted));
        if let Some(switch) = layout.switch {
            let usable = snapshot.wifi_hardware;
            crate::panel::draw_switch(painter, switch, snapshot.wifi_enabled && usable, false, usable);
        }
        let line = card.y + HEADER_H - 1.0;
        painter.fill(Rect::new(card.x + PAD, line, card.w - 2.0 * PAD, 1.0), theme.border, 0.0);

        if let (Some(rect), Some(link)) = (layout.wired, snapshot.wired) {
            let detail = match link {
                Link::Connected => "Connected",
                Link::Connecting => "Connecting\u{2026}",
                Link::Idle => "Not connected",
            };
            self.draw_network_row(painter, rect, (glyph::ETHERNET, None), "Wired", detail, link == Link::Connected);
        }

        if let Some(prompt) = &self.network.prompt {
            self.draw_network_prompt(painter, &layout, prompt, pointer);
        } else if self.network_listing() {
            for (index, rect, disconnect) in &layout.rows {
                let network = &snapshot.networks[*index];
                let connected = network.link == Link::Connected;
                if *index == self.network.selected {
                    painter.fill(*rect, theme.accent.alpha(0.12), theme.radius);
                } else if network.link == Link::Idle && hover(*rect) {
                    painter.fill(*rect, theme.surface_raised, theme.radius);
                }
                let asked = self.network.asked.as_deref() == Some(&network.ssid[..]);
                let detail = detail(network, asked);
                let icon = (glyph::WIFI, Some(signal(network.strength)));
                let text_rect =
                    Rect::new(rect.x, rect.y, rect.w - if disconnect.is_some() { 100.0 } else { 30.0 }, rect.h);
                self.draw_network_row(painter, text_rect, icon, &network.name(), detail, connected);
                if network.security.secured() && disconnect.is_none() {
                    let lock = Rect::new(rect.right() - 30.0, rect.y, 24.0, rect.h);
                    painter.glyph(glyph::LOCK, lock, 13, theme.muted.alpha(0.8));
                }
                if let Some(button) = disconnect {
                    self.draw_network_button(painter, *button, "Disconnect", false, hover(*button));
                }
            }
            if snapshot.networks.is_empty() {
                let style = Style::new(theme.font_size * 0.9, theme.muted).align(Align::Centre);
                painter.text(
                    "Looking for networks\u{2026}",
                    Rect::new(layout.body.x, layout.body.y, layout.body.w, MESSAGE_H),
                    style,
                );
            } else if snapshot.networks.len() > ROWS {
                // Where the rows shown are in the whole list.
                let track = Rect::new(card.right() - 5.0, layout.body.y + 4.0, 3.0, ROWS as f64 * ROW_H - 8.0);
                let share = ROWS as f64 / snapshot.networks.len() as f64;
                let thumb_h = (track.h * share).max(16.0);
                let travel = (snapshot.networks.len() - ROWS) as f64;
                let thumb_y = track.y + (track.h - thumb_h) * self.network.first as f64 / travel;
                painter.fill(Rect::new(track.x, thumb_y, track.w, thumb_h), theme.muted.alpha(0.4), 1.5);
            }
        } else if layout.body.h > 0.0 {
            let text = match () {
                _ if snapshot.wifi_device.is_some() && !snapshot.wifi_hardware => {
                    "Wi-Fi is off: a hardware switch or airplane mode"
                }
                _ if snapshot.wifi_device.is_some() => "Wi-Fi is off",
                _ => "No network devices",
            };
            let style = Style::new(theme.font_size * 0.9, theme.muted).align(Align::Centre);
            painter.text(text, Rect::new(layout.body.x, layout.body.y, layout.body.w, MESSAGE_H), style);
        }

        // The footer: what is happening, and the settings.
        let footer = layout.footer;
        painter.fill(Rect::new(footer.x, footer.y, footer.w, 1.0), theme.border, 0.0);
        let (text, colour) = match &self.network.status {
            Some(status) => (status.text.clone(), if status.error { theme.danger } else { theme.muted }),
            None if self.network.prompt.is_some() => ("Enter joins, Esc goes back".into(), theme.muted),
            None if self.network_listing() && !snapshot.networks.is_empty() => {
                let count = snapshot.networks.len();
                (format!("{count} network{}", if count == 1 { "" } else { "s" }), theme.muted)
            }
            None => (String::new(), theme.muted),
        };
        let text_w = footer.w - layout.settings.map_or(0.0, |settings| settings.w + 8.0) - 4.0;
        painter.text(
            &text,
            Rect::new(footer.x + 4.0, footer.y, text_w.max(1.0), footer.h),
            Style::new(theme.font_size * 0.8, colour),
        );
        if let Some(settings) = layout.settings {
            self.draw_network_button(painter, settings, "Network settings", false, hover(settings));
        }
    }

    /// A row: an icon in a badge (a dim base with a lit part over it, for
    /// signal strength), a title and a detail under it.
    fn draw_network_row(
        &self,
        painter: &mut Painter,
        rect: Rect,
        icon: (&'static str, Option<&'static str>),
        title: &str,
        detail: &str,
        lit: bool,
    ) {
        let theme = &self.theme;
        let badge = Rect::new(rect.x + 6.0, rect.y + 8.0, 28.0, 28.0);
        let (fill, colour) = crate::panel::tint(
            theme,
            if lit { crate::panel::block::Tint::Accent } else { crate::panel::block::Tint::Muted },
        );
        painter.fill(badge, fill, 6.0);
        match icon {
            (base, Some(part)) => {
                painter.glyph(base, badge, 15, colour.alpha(0.3));
                painter.glyph(part, badge, 15, colour);
            }
            (base, None) => painter.glyph(base, badge, 15, colour),
        }
        let text_x = badge.right() + 10.0;
        let text_w = (rect.right() - text_x - 4.0).max(1.0);
        let title_style = Style::new(theme.font_size * 0.92, theme.text);
        painter.text(
            title,
            Rect::new(text_x, rect.y + 5.0, text_w, 18.0),
            if lit { title_style.bold() } else { title_style },
        );
        let detail_colour = if lit { theme.accent } else { theme.muted };
        painter.text(
            detail,
            Rect::new(text_x, rect.y + 22.0, text_w, 16.0),
            Style::new(theme.font_size * 0.78, detail_colour),
        );
    }

    fn draw_network_button(&self, painter: &mut Painter, rect: Rect, label: &str, primary: bool, hovered: bool) {
        let look = if primary { Look::Primary } else { Look::Plain };
        painter.button(rect, Button::new(label, look), hovered);
    }

    fn draw_network_prompt(&self, painter: &mut Painter, layout: &Card, prompt: &Prompt, pointer: Point) {
        let theme = &self.theme;
        let body = layout.body;
        let label = format!("Password for {}", prompt.network.name());
        painter.text(
            &label,
            Rect::new(body.x + 4.0, body.y + 6.0, body.w - 8.0, 24.0),
            Style::new(theme.font_size * 0.9, theme.text),
        );
        if let Some(field) = layout.field {
            painter.shape(
                field,
                Shape {
                    border_width: 1.0,
                    border: theme.accent.alpha(0.8),
                    ..Shape::filled(theme.background, theme.radius)
                },
            );
            let size = theme.font_size * 0.92;
            let text_w = (field.w - 70.0) as f32;
            if prompt.input.text.is_empty() {
                painter.text(
                    "Password",
                    Rect::new(field.x + 10.0, field.y, f64::from(text_w), field.h),
                    Style::new(size, theme.muted.alpha(0.7)),
                );
            }
            let (shown, cursor) = shown(prompt);
            let anchor = prompt.input.anchor.map(|anchor| {
                if prompt.reveal { anchor } else { prompt.input.text[..anchor].chars().count() * "\u{2022}".len() }
            });
            let look = crate::text::Field { cursor, anchor, visible: true, accent: theme.accent.straight_rgba8() };
            if let Some(image) = self.text.borrow_mut().field(&shown, &theme.font, size, theme.text, text_w, look) {
                painter.image(&image.buffer, (field.x + 10.0, field.y + (field.h - image.height) / 2.0));
            }
        }
        if let Some(reveal) = layout.reveal {
            let label = if prompt.reveal { "Hide" } else { "Show" };
            let colour = if reveal.contains(pointer) { theme.text } else { theme.muted };
            painter.text(label, reveal, Style::new(theme.font_size * 0.8, colour).align(Align::Centre));
        }
        if let Some(cancel) = layout.cancel {
            self.draw_network_button(painter, cancel, "Cancel", false, cancel.contains(pointer));
        }
        if let Some(join) = layout.join {
            self.draw_network_button(painter, join, "Join", true, join.contains(pointer));
        }
    }
}

/// What `tileception msg network status` prints.
fn status_text(snapshot: &Snapshot) -> String {
    let mut out = String::new();
    if let Some(link) = snapshot.wired {
        let state = match link {
            Link::Connected => "connected",
            Link::Connecting => "connecting",
            Link::Idle => "not connected",
        };
        out.push_str(&format!("Wired: {state}\n"));
    }
    if snapshot.wifi_device.is_none() {
        if snapshot.wired.is_none() {
            out.push_str("No network devices\n");
        }
        return out;
    }
    let wifi = match snapshot.current() {
        _ if !snapshot.wifi_hardware => "off (hardware switch or airplane mode)".to_string(),
        _ if !snapshot.wifi_enabled => "off".to_string(),
        Some(network) if network.link == Link::Connected => format!("connected to {}", network.name()),
        Some(network) => format!("connecting to {}", network.name()),
        None => "not connected".to_string(),
    };
    out.push_str(&format!("Wi-Fi: {wifi}\n"));
    for network in &snapshot.networks {
        let mark = if network.link == Link::Idle { ' ' } else { '*' };
        let detail = detail(network, false).to_lowercase();
        out.push_str(&format!("{mark} {:>3}%  {}  ({detail})\n", network.strength, network.name()));
    }
    out
}

/// The password as the field shows it, and the caret's place in that.
fn shown(prompt: &Prompt) -> (String, usize) {
    if prompt.reveal {
        return (prompt.input.text.clone(), prompt.input.cursor);
    }
    let dot = "\u{2022}";
    let shown = dot.repeat(prompt.input.text.chars().count());
    let cursor = prompt.input.text[..prompt.input.cursor].chars().count() * dot.len();
    (shown, cursor)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checks_passwords_as_wpa_does() {
        assert!(!valid_password("short"));
        assert!(valid_password("eight ch"));
        assert!(valid_password(&"x".repeat(63)));
        assert!(!valid_password(&"x".repeat(64)));
        assert!(valid_password(&"ab12".repeat(16)));
        assert!(valid_password("wachtwoord\u{e9}"));
    }

    #[test]
    fn rows_say_what_a_click_would_do() {
        let network = Wifi {
            ssid: b"Cafe".to_vec(),
            access_point: "/ap/1".into(),
            strength: 80,
            security: Security::Password { sae: false },
            saved: None,
            link: Link::Idle,
        };
        assert_eq!(detail(&network, false), "Secured");
        assert_eq!(detail(&network, true), "Connecting\u{2026}");
        assert_eq!(detail(&Wifi { saved: Some("/s/1".into()), ..network.clone() }, false), "Saved");
        assert_eq!(detail(&Wifi { link: Link::Connected, ..network.clone() }, true), "Connected");
        assert_eq!(signal(10), glyph::WIFI_0);
        assert_eq!(signal(80), glyph::WIFI);
    }

    #[test]
    fn hidden_passwords_show_one_dot_per_character() {
        let mut prompt = Prompt {
            network: Wifi {
                ssid: b"Cafe".to_vec(),
                access_point: "/ap/1".into(),
                strength: 80,
                security: Security::Password { sae: false },
                saved: None,
                link: Link::Idle,
            },
            input: TextInput::default(),
            reveal: false,
        };
        prompt.input.insert("caf\u{e9}12");
        // After the é, which takes two bytes.
        prompt.input.move_to(5, false);
        let (dots, cursor) = shown(&prompt);
        assert_eq!(dots.chars().count(), 6);
        assert_eq!(cursor, 4 * "\u{2022}".len());
        prompt.reveal = true;
        assert_eq!(shown(&prompt), ("caf\u{e9}12".to_string(), 5));
    }
}
