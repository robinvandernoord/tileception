//! Notification popups and history. The service runs off the compositor thread.

use std::{
    cell::RefCell,
    collections::{BTreeMap, HashMap},
    sync::{Arc, mpsc},
    time::Instant,
};

use smithay::{
    backend::{
        allocator::Fourcc,
        renderer::{element::memory::MemoryRenderBuffer, gles::GlesRenderer},
    },
    reexports::calloop::ping::make_ping,
    utils::Transform,
};
use tileception_core::{Point, Rect};
use tileception_notify::{Event, Image, Notification, NotifyService, Request, Urgency};

use crate::{
    Tileception,
    render::OutputElement,
    shapes::Shape,
    ui::{Align, Painter, Style, ago, ease_out, glyph},
};

#[derive(Default)]
pub struct Notifications {
    service: Option<NotifyService>,
    events: Option<mpsc::Receiver<Event>>,
    source: Option<smithay::reexports::calloop::RegistrationToken>,
    active: BTreeMap<u32, Arc<Notification>>,
    history: Vec<Arc<Notification>>,
    pub open: bool,
    offset: usize,
    thumbnails: RefCell<HashMap<u32, Option<Preview>>>,
}

impl Tileception {
    pub fn start_notifications(&mut self, address: Option<String>) {
        let result = (|| {
            let (ping, source) = make_ping().map_err(|e| e.to_string())?;
            let (service, events) =
                NotifyService::start(Default::default(), address, move || ping.ping()).map_err(|e| e.to_string())?;
            let token = self
                .loop_handle
                .insert_source(source, |_, _, state| state.notification_events())
                .map_err(|e| e.to_string())?;
            Ok::<_, String>((service, events, token))
        })();
        match result {
            Ok((service, events, token)) => {
                self.notifications.service = Some(service);
                self.notifications.events = Some(events);
                self.notifications.source = Some(token);
            }
            Err(error) => tracing::error!("could not start notifications: {error}"),
        }
    }

    /// Stops the service, releasing its bus name for another daemon.
    pub fn stop_notifications(&mut self) {
        if let Some(token) = self.notifications.source.take() {
            self.loop_handle.remove(token);
        }
        self.notifications.service = None;
        self.notifications.events = None;
        self.notifications.active.clear();
        self.notifications.history.clear();
        self.schedule_render();
    }

    /// Whether Tileception's own notifications are in use (rather than
    /// switched off or replaced in `[shell]`).
    pub fn builtin_notifications(&self) -> bool {
        self.shell().notifications().is_builtin()
    }

    fn notification_events(&mut self) {
        let events: Vec<_> = self.notifications.events.as_ref().map(|rx| rx.try_iter().collect()).unwrap_or_default();
        for event in events {
            match event {
                Event::Shown(n) | Event::Replaced(n) => {
                    self.notifications.thumbnails.borrow_mut().remove(&n.id);
                    self.notifications.active.insert(n.id, Arc::new(n));
                }
                Event::Closed { id, .. } => {
                    self.notifications.active.remove(&id);
                }
            }
        }
        if self.notifications.open {
            self.refresh_notification_history();
        }
        self.schedule_render();
    }

    pub fn toggle_notifications(&mut self) {
        if self.is_locked() {
            return;
        }
        self.notifications.open = !self.notifications.open;
        self.notifications.offset = 0;
        if self.notifications.open {
            self.refresh_notification_history();
        }
    }

    fn refresh_notification_history(&mut self) {
        if let Some(service) = &self.notifications.service {
            self.notifications.history = service.history().into_iter().map(Arc::new).collect();
        }
    }

    pub fn notification_request(&mut self, request: Request) {
        match &request {
            Request::ClearHistory => self.notifications.history.clear(),
            Request::RemoveFromHistory(id) => self.notifications.history.retain(|n| n.id != *id),
            _ => {}
        }
        if let Some(service) = &self.notifications.service {
            service.send(request);
        }
    }

    /// The history's length and whether Do not disturb is on, while the
    /// built-in service runs.
    pub fn notification_status(&self) -> Option<(usize, bool)> {
        let service = self.notifications.service.as_ref()?;
        Some((service.history_len(), service.do_not_disturb()))
    }

    pub fn toggle_dnd(&mut self) {
        if let Some(service) = &self.notifications.service {
            service.send(Request::SetDoNotDisturb(!service.do_not_disturb()));
        }
    }

    pub fn notification_scroll(&mut self, delta: isize) {
        let len = self.notifications.history.len();
        self.notifications.offset = self.notifications.offset.saturating_add_signed(delta).min(len.saturating_sub(1));
    }

    /// The history panel, against the right edge.
    fn notification_panel(&self) -> Rect {
        let screen = self.world.screen();
        let w = screen.w.clamp(1.0, PANEL_W);
        Rect::new(screen.right() - w, screen.y, w, screen.h.max(1.0))
    }

    /// The history panel's own controls.
    pub fn panel_controls(&self) -> PanelControls {
        let panel = self.notification_panel();
        let controls = self.controls_layout(panel.x + 12.0, panel.y + 58.0, panel.w - 24.0);
        let below = if controls.rect.h > 0.0 { controls.rect.bottom() + 10.0 } else { panel.y + 58.0 };
        // Without the built-in service there is no Do not disturb to switch.
        let dnd_h = if self.builtin_notifications() { 48.0 } else { 0.0 };
        let dnd = Rect::new(panel.x + 12.0, below, panel.w - 24.0, dnd_h);
        PanelControls {
            clear: Rect::new(panel.right() - 16.0 - 96.0, panel.y + 14.0, 96.0, 32.0),
            controls,
            dnd,
            cards_top: dnd.bottom() + 18.0,
        }
    }

    /// The cards on screen: popups in the bottom right corner, newest
    /// lowest, or the history in the panel, newest first.
    fn notification_cards(&self) -> Vec<Card> {
        let screen = self.world.screen();
        if self.notifications.open {
            let panel = self.notification_panel();
            let mut y = self.panel_controls().cards_top;
            let w = panel.w - 24.0;
            let mut cards = Vec::new();
            for n in self.notifications.history.iter().rev().skip(self.notifications.offset) {
                let card = self.card_layout(n.clone(), panel.x + 12.0, y, w, false);
                if card.rect.bottom() > panel.bottom() - 52.0 {
                    break;
                }
                y = card.rect.bottom() + 8.0;
                cards.push(card);
            }
            return cards;
        }
        let w = (screen.w - 24.0).clamp(1.0, POPUP_W);
        let mut items: Vec<_> = self.notifications.active.values().filter(|n| n.show_popup).cloned().collect();
        // Questions waiting for an answer (no timeout, with buttons) stay in the
        // corner, so newer popups cannot push them off screen.
        items.sort_by_key(|n| (n.expires.is_some() || n.actions.is_empty(), std::cmp::Reverse(n.created)));
        items.truncate(3);
        let mut bottom = screen.bottom() - 14.0;
        let mut cards = Vec::new();
        for n in items {
            let mut card = self.card_layout(n, screen.right() - w - 14.0, 0.0, w, true);
            let h = card.rect.h;
            if bottom - h < screen.y + 14.0 {
                break;
            }
            card.shift(bottom - h);
            bottom -= h + 10.0;
            cards.push(card);
        }
        cards
    }

    /// Lays out one card with its top left at (x, y).
    fn card_layout(&self, n: Arc<Notification>, x: f64, y: f64, w: f64, popup: bool) -> Card {
        let t = &self.theme;
        let mut cursor = y + 14.0;
        let header = Rect::new(x + 16.0, cursor, w - 56.0, 20.0);
        let close = Rect::new(x + w - 36.0, y + 8.0, 28.0, 28.0);
        cursor = header.bottom() + 10.0;
        let preview = self.notification_preview(&n).map(|(_, pw, ph)| {
            let r = Rect::new(x + 16.0 + ((w - 32.0 - pw as f64) / 2.0).max(0.0), cursor, pw as f64, ph as f64);
            cursor = r.bottom() + 12.0;
            r
        });
        let icon = (preview.is_none() && self.notification_icon(&n, ICON).is_some())
            .then(|| Rect::new(x + 16.0, cursor + 2.0, ICON as f64, ICON as f64));
        let text_x = icon.map_or(x + 16.0, |i| i.right() + 12.0);
        let text_w = x + w - 16.0 - text_x;
        let summary = Rect::new(text_x, cursor, text_w, 22.0);
        let body_text = plain_body(&n.body);
        let body_h = if body_text.trim().is_empty() {
            0.0
        } else {
            self.text
                .borrow_mut()
                .styled(body_text.trim(), &t.font, t.font_size * 0.95, t.muted, text_w as f32, 400, BODY_LINES)
                .map_or(0.0, |i| i.height as f64)
        };
        let body = Rect::new(text_x, summary.bottom() + 2.0, text_w, body_h);
        cursor = body.bottom().max(icon.map_or(0.0, |i| i.bottom()));
        let count = n.actions.len();
        let actions = if count > 0 {
            let top = cursor + 12.0;
            let gap = 8.0;
            let each = (w - 32.0 - gap * (count as f64 - 1.0)) / count as f64;
            cursor = top + 32.0;
            (0..count).map(|i| Rect::new(x + 16.0 + i as f64 * (each + gap), top, each, 32.0)).collect()
        } else {
            Vec::new()
        };
        let rect = Rect::new(x, y, w, cursor + 16.0 - y);
        Card { n, rect, header, close, preview, icon, summary, body, body_text, actions, popup }
    }

    pub fn notifications_contain(&self, p: Point) -> bool {
        if self.overlay_under(p) {
            return false;
        }
        if self.notifications.open && self.notification_panel().contains(p) {
            return true;
        }
        self.notification_cards().iter().any(|card| card.rect.contains(p))
    }

    pub fn notification_click(&mut self, p: Point) {
        if self.notifications.open {
            let controls = self.panel_controls();
            if controls.clear.contains(p) {
                self.notification_request(Request::ClearHistory);
                self.notification_request(Request::DismissAll);
                return;
            }
            if self.builtin_notifications() && controls.dnd.contains(p) {
                self.toggle_dnd();
                return;
            }
            if self.controls_press(&controls.controls, p) {
                return;
            }
        }
        for card in self.notification_cards() {
            if !card.rect.contains(p) {
                continue;
            }
            let n = &card.n;
            if card.close.contains(p) {
                if self.notifications.open {
                    self.notification_request(Request::RemoveFromHistory(n.id));
                }
                self.notification_request(Request::Dismiss(n.id));
            } else if let Some(i) = card.actions.iter().position(|a| a.contains(p)) {
                if let Some((key, _)) = n.actions.get(i) {
                    self.notification_request(Request::Invoke { id: n.id, action: key.clone(), token: None });
                }
            } else if n.default_action {
                self.notification_request(Request::Invoke { id: n.id, action: "default".into(), token: None });
            }
            return;
        }
    }

    pub fn notification_elements(&self, renderer: &mut GlesRenderer) -> Vec<OutputElement> {
        let t = &self.theme;
        let pointer = Point::new(self.pointer.x, self.pointer.y);
        let dnd = self.notifications.service.as_ref().is_some_and(|s| s.do_not_disturb());
        let mut p = Painter::new(self, renderer);
        if self.notifications.open {
            let panel = self.notification_panel();
            let controls = self.panel_controls();
            p.edge_panel(panel, false);
            let title = Style::new(t.font_size * 1.3, t.text).bold();
            let (title_w, _) = p.text(
                if self.builtin_notifications() { "Notifications" } else { "Controls" },
                Rect::new(panel.x + 18.0, panel.y + 14.0, panel.w - 150.0, 32.0),
                title,
            );
            let count = self.notifications.history.len();
            if count > 0 {
                let label = count.to_string();
                let style = Style::new(t.font_size * 0.8, t.on_accent).bold().align(Align::Centre);
                let w = (p.measure(&label, 100.0, style).0 + 14.0).max(22.0);
                let chip = Rect::new(panel.x + 18.0 + title_w + 10.0, panel.y + 20.0, w, 20.0);
                p.shape(chip, Shape { radius: 10.0, fill: (t.accent, t.accent_end), ..Default::default() });
                p.text(&label, chip, style);
            }
            if count > 0 {
                let hover = controls.clear.contains(pointer);
                if hover {
                    p.fill(controls.clear, t.surface_raised, t.radius);
                }
                let colour = if hover { t.text } else { t.muted };
                p.glyph(glyph::CLEAR, Rect::new(controls.clear.x + 6.0, controls.clear.y, 24.0, 32.0), 15, colour);
                p.text(
                    "Clear all",
                    Rect::new(controls.clear.x + 32.0, controls.clear.y, 60.0, 32.0),
                    Style::new(t.font_size * 0.95, colour),
                );
            }

            self.draw_controls(&mut p, &controls.controls, pointer);

            if self.builtin_notifications() {
                // Do not disturb, as a row with a switch.
                let row = controls.dnd;
                let hover = row.contains(pointer);
                p.shape(
                    row,
                    Shape {
                        border_width: 1.0,
                        border: if dnd { t.accent.alpha(0.4) } else { t.border },
                        ..Shape::filled(if hover { t.surface_raised } else { t.surface }, t.radius)
                    },
                );
                p.glyph(
                    glyph::MOON,
                    Rect::new(row.x + 8.0, row.y, 32.0, row.h),
                    17,
                    if dnd { t.accent } else { t.muted },
                );
                p.text(
                    "Do not disturb",
                    Rect::new(row.x + 44.0, row.y + 5.0, row.w - 110.0, 20.0),
                    Style::new(t.font_size, t.text),
                );
                let sub = if dnd { "Popups are silenced" } else { "Popups are shown" };
                p.text(
                    sub,
                    Rect::new(row.x + 44.0, row.y + 24.0, row.w - 110.0, 18.0),
                    Style::new(t.font_size * 0.82, t.muted),
                );
                let track = Rect::new(row.right() - 56.0, row.y + (row.h - 24.0) / 2.0, 42.0, 24.0);
                let knob_x = if dnd { track.right() - 21.0 } else { track.x + 3.0 };
                if dnd {
                    p.shape(
                        track,
                        Shape {
                            radius: 12.0,
                            fill: (t.accent, t.accent_end),
                            glow_radius: 10.0,
                            glow: t.accent.alpha(0.25),
                            ..Default::default()
                        },
                    );
                } else {
                    p.shape(track, Shape { border_width: 1.0, border: t.border, ..Shape::filled(t.background, 12.0) });
                }
                p.fill(Rect::new(knob_x, track.y + 3.0, 18.0, 18.0), if dnd { t.on_accent } else { t.muted }, 9.0);
            }
            p.fill(Rect::new(panel.x + 12.0, controls.cards_top - 10.0, panel.w - 24.0, 1.0), t.border, 0.0);

            if self.notifications.history.is_empty() {
                let mid = (controls.cards_top + panel.bottom()) / 2.0 - 40.0;
                p.fill(Rect::new(panel.x + panel.w / 2.0 - 32.0, mid - 32.0, 64.0, 64.0), t.surface_raised, 32.0);
                p.glyph(glyph::BELL, Rect::new(panel.x, mid - 32.0, panel.w, 64.0), 28, t.muted);
                let (headline, detail) = if !self.builtin_notifications() {
                    ("Notifications are handled elsewhere", "Switched off or replaced in [shell]")
                } else if self.notifications.service.is_some() {
                    ("No notifications", "You're all caught up")
                } else {
                    ("Notifications unavailable", "The notification service did not start")
                };
                p.text(
                    headline,
                    Rect::new(panel.x, mid + 44.0, panel.w, 24.0),
                    Style::new(t.font_size * 1.05, t.text).bold().align(Align::Centre),
                );
                p.text(
                    detail,
                    Rect::new(panel.x, mid + 68.0, panel.w, 22.0),
                    Style::new(t.font_size * 0.9, t.muted).align(Align::Centre),
                );
            }
            let hints: &[(&str, &str)] = if self.builtin_notifications() {
                &[("Esc", "Close"), ("Scroll", "Older")]
            } else {
                &[("Esc", "Close")]
            };
            p.key_hints(hints, panel.x + 18.0, panel.bottom() - 26.0, false);
        }

        for card in self.notification_cards() {
            if card.popup {
                // Slides in from the right edge.
                let age = card.n.created.elapsed().map(|d| d.as_secs_f64()).unwrap_or(1.0);
                let shown = ease_out(age / 0.24);
                p.offset = ((1.0 - shown) * (card.rect.w + 24.0), 0.0);
                p.alpha = shown as f32;
            }
            self.draw_card(&mut p, &card, pointer);
            p.offset = (0.0, 0.0);
            p.alpha = 1.0;
        }
        p.finish()
    }

    fn draw_card(&self, p: &mut Painter, card: &Card, pointer: Point) {
        let t = &self.theme;
        let n = &card.n;
        let rect = card.rect;
        let critical = n.urgency == Urgency::Critical && n.category.as_deref() != Some("x-tileception.prompt");
        if card.popup {
            p.popup(rect);
        } else {
            p.shape(rect, Shape { border_width: 1.0, border: t.border, ..Shape::filled(t.surface, t.popup_radius) });
        }
        // The accent stripe on the left edge; red for critical notifications.
        let stripe = Rect::new(rect.x + 5.0, rect.y + 14.0, 3.0, rect.h - 28.0);
        let (a, b) = if critical { (t.danger, t.danger) } else { (t.accent, t.accent_end) };
        p.shape(stripe, Shape { radius: 1.5, fill: (a, b), ..Default::default() });

        // Header: app icon, app name and how long ago.
        let mut x = card.header.x;
        if let Some(icon) = self.notification_icon(n, 16) {
            p.image(&icon, (x, card.header.y + 2.0));
            x += 22.0;
        }
        let app = if n.app_name.is_empty() { "Notification" } else { &n.app_name };
        let header = format!("{app}  \u{00b7}  {}", ago(n.created));
        p.text(
            &header,
            Rect::new(x, card.header.y, card.header.right() - x, card.header.h),
            Style::new(t.font_size * 0.82, t.muted),
        );
        let hover_close = card.close.contains(pointer);
        if hover_close {
            p.fill(card.close, t.surface_raised, card.close.w as f32 / 2.0);
        }
        p.glyph(glyph::CLOSE, card.close, 14, if hover_close { t.text } else { t.muted });

        if let (Some(at), Some((image, w, h))) = (card.preview, self.notification_preview(n)) {
            p.image_sized(&image, (at.x, at.y), Some((w, h)));
            p.shape(at, Shape { radius: t.radius, border_width: 1.0, border: t.border, ..Default::default() });
        }
        if let (Some(at), Some(icon)) = (card.icon, self.notification_icon(n, ICON)) {
            p.image(&icon, (at.x, at.y));
        }
        p.text(&n.summary, card.summary, Style::new(t.font_size * 1.02, t.text).bold());
        if card.body.h > 0.0 {
            p.text(card.body_text.trim(), card.body, Style::new(t.font_size * 0.95, t.muted).lines(BODY_LINES));
        }
        for (i, (action, (_, label))) in card.actions.iter().zip(&n.actions).enumerate() {
            let hover = action.contains(pointer);
            let primary = i == 0;
            if primary {
                let alpha = if hover { 0.32 } else { 0.2 };
                p.shape(
                    *action,
                    Shape {
                        radius: t.radius,
                        fill: (t.accent.alpha(alpha), t.accent_end.alpha(alpha * 0.6)),
                        border_width: 1.0,
                        border: t.accent.alpha(0.5),
                        ..Default::default()
                    },
                );
            } else {
                let fill = if hover { t.surface_raised.mix(t.border, 0.5) } else { t.surface_raised };
                p.shape(*action, Shape { border_width: 1.0, border: t.border, ..Shape::filled(fill, t.radius) });
            }
            let colour = if primary {
                t.accent
            } else if hover {
                t.text
            } else {
                t.text.alpha(0.85)
            };
            p.text(label, action.inset(6.0), Style::new(t.font_size * 0.92, colour).bold().align(Align::Centre));
        }

        // How long the popup has left, as a thin bar along the bottom.
        if let (true, Some(expires)) = (card.popup, n.expires) {
            let left = expires.saturating_duration_since(Instant::now()).as_secs_f64();
            let total = left + n.created.elapsed().map(|d| d.as_secs_f64()).unwrap_or(0.0);
            if total > 0.0 {
                let track = Rect::new(rect.x + 16.0, rect.bottom() - 6.0, rect.w - 32.0, 2.0);
                p.fill(track, t.border.alpha(0.6), 1.0);
                let bar = Rect::new(track.x, track.y, track.w * (left / total).clamp(0.0, 1.0), track.h);
                p.shape(bar, Shape { radius: 1.0, fill: (t.accent, t.accent_end), ..Default::default() });
            }
        }
    }

    /// An app icon for the notification, from its hints or the sending app.
    fn notification_icon(&self, n: &Notification, size: u32) -> Option<MemoryRenderBuffer> {
        let named = |name: &str| {
            let path = name.strip_prefix("file://").unwrap_or(name);
            if path.starts_with('/') {
                self.launcher.icons.image(std::path::Path::new(path), size)
            } else {
                self.launcher.app_icon(path, size)
            }
        };
        if let Some(Image::Path(path)) = &n.image
            && !is_file(path)
            && let Some(icon) = named(path)
        {
            return Some(icon);
        }
        // A picture file given as the icon is shown as the preview instead.
        n.app_icon
            .as_deref()
            .filter(|icon| !icon.is_empty() && (!is_file(icon) || icon.ends_with(".svg")))
            .and_then(named)
            .or_else(|| n.desktop_entry.as_deref().and_then(|entry| self.launcher.app_icon(entry, size)))
    }

    /// A picture the notification carries (a screenshot, an album cover), scaled to fit a card.
    fn notification_preview(&self, n: &Notification) -> Option<Preview> {
        if let Some(cached) = self.notifications.thumbnails.borrow().get(&n.id) {
            return cached.clone();
        }
        let preview = load_preview(n);
        let mut cache = self.notifications.thumbnails.borrow_mut();
        if cache.len() >= 128 {
            cache.clear();
        }
        cache.insert(n.id, preview.clone());
        preview
    }
}

/// A picture scaled for a card, with its width and height.
type Preview = (MemoryRenderBuffer, i32, i32);

const POPUP_W: f64 = 380.0;
const PANEL_W: f64 = 420.0;
/// App icons beside the text when there is no picture.
const ICON: u32 = 40;
const BODY_LINES: u8 = 3;
const PREVIEW_W: u32 = 348;
const PREVIEW_H: u32 = 190;

pub struct PanelControls {
    clear: Rect,
    /// Sound, brightness and playback, under the title.
    pub controls: crate::controls::Controls,
    dnd: Rect,
    cards_top: f64,
}

/// Where the parts of one notification card go.
struct Card {
    n: Arc<Notification>,
    rect: Rect,
    header: Rect,
    close: Rect,
    preview: Option<Rect>,
    icon: Option<Rect>,
    summary: Rect,
    body: Rect,
    body_text: String,
    actions: Vec<Rect>,
    popup: bool,
}

impl Card {
    /// Moves the card down to start at `y`.
    fn shift(&mut self, y: f64) {
        let dy = y - self.rect.y;
        let down = |r: &mut Rect| r.y += dy;
        down(&mut self.rect);
        down(&mut self.header);
        down(&mut self.close);
        down(&mut self.summary);
        down(&mut self.body);
        self.preview.iter_mut().for_each(down);
        self.icon.iter_mut().for_each(down);
        self.actions.iter_mut().for_each(down);
    }
}

fn read_picture(path: &str) -> Option<image::RgbaImage> {
    let path = path.strip_prefix("file://").unwrap_or(path);
    let reader = image::ImageReader::open(path).ok()?.with_guessed_format().ok()?;
    Some(reader.decode().ok()?.to_rgba8())
}

fn is_file(name: &str) -> bool {
    name.starts_with('/') || name.starts_with("file://")
}

/// Pixel data or an image file, scaled to fit a card and premultiplied.
fn load_preview(n: &Notification) -> Option<Preview> {
    let image = match &n.image {
        Some(Image::Rgba { width, height, data }) => image::RgbaImage::from_raw(*width, *height, data.clone())?,
        Some(Image::Path(path)) if is_file(path) => read_picture(path)?,
        Some(_) => return None,
        None => read_picture(n.app_icon.as_deref().filter(|i| is_file(i) && !i.ends_with(".svg"))?)?,
    };
    // Small pictures (avatars, covers) stay small; large ones fill the card's width.
    let (w, h) = (image.width().max(1), image.height().max(1));
    let scale = (PREVIEW_W as f64 / w as f64).min(PREVIEW_H as f64 / h as f64).min(1.0);
    let (tw, th) = (((w as f64 * scale).round() as u32).max(1), ((h as f64 * scale).round() as u32).max(1));
    let mut scaled = image::imageops::thumbnail(&image, tw, th);
    for pixel in scaled.pixels_mut() {
        let a = pixel.0[3] as u32;
        for c in &mut pixel.0[..3] {
            *c = (*c as u32 * a / 255) as u8;
        }
    }
    let buffer = MemoryRenderBuffer::from_slice(
        scaled.as_raw(),
        Fourcc::Abgr8888,
        (scaled.width() as i32, scaled.height() as i32),
        1,
        Transform::Normal,
        None,
    );
    Some((buffer, scaled.width() as i32, scaled.height() as i32))
}

fn plain_body(markup: &str) -> String {
    let mut out = String::new();
    let mut tag = false;
    let mut quote = None;
    for ch in markup.chars() {
        if tag {
            match ch {
                '\'' | '"' if quote == Some(ch) => quote = None,
                '\'' | '"' if quote.is_none() => quote = Some(ch),
                '>' if quote.is_none() => tag = false,
                _ => {}
            }
        } else if ch == '<' {
            tag = true;
        } else {
            out.push(ch);
        }
    }
    html_escape::decode_html_entities(&out).into_owned()
}
