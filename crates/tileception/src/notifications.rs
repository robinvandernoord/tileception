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
    /// Shown by [`Tileception::notify_locally`] so far, for their ids.
    local: u32,
}

/// The category of [`Tileception::notify_locally`]'s notifications: the only
/// ones shown on a lock screen, which keeps everyone else's to itself.
const LOCAL_CATEGORY: &str = "x-tileception.local";

/// How long a notification of Tileception's own stays up.
const LOCAL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

impl Tileception {
    pub fn start_notifications(&mut self, address: Option<String>) {
        let result = (|| {
            let (ping, source) = make_ping().map_err(|err| err.to_string())?;
            let (service, events) = NotifyService::start(Default::default(), address, move || ping.ping())
                .map_err(|err| err.to_string())?;
            let token = self
                .loop_handle
                .insert_source(source, |_, _, state| state.notification_events())
                .map_err(|err| err.to_string())?;
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
        let events: Vec<_> =
            self.notifications.events.as_ref().map(|receiver| receiver.try_iter().collect()).unwrap_or_default();
        for event in events {
            match event {
                Event::Shown(notification) | Event::Replaced(notification) => {
                    self.notifications.thumbnails.borrow_mut().remove(&notification.id);
                    self.notifications.active.insert(notification.id, Arc::new(notification));
                }
                Event::Closed { id: notification_id, .. } => {
                    self.notifications.active.remove(&notification_id);
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
            Request::RemoveFromHistory(notification_id) => {
                self.notifications.history.retain(|entry| entry.id != *notification_id)
            }
            _ => {}
        }
        if let Some(service) = &self.notifications.service {
            service.send(request);
        }
    }

    /// Shows a notification of Tileception's own without the service, such as
    /// the greeter's feedback. It goes away by itself and stays out of the history.
    pub fn notify_locally(&mut self, summary: &str, body: &str, urgency: Urgency) {
        // Counting down from the top keeps clear of the service's ids.
        self.notifications.local = self.notifications.local.wrapping_add(1);
        let notification_id = u32::MAX - self.notifications.local;
        let notification = Notification {
            id: notification_id,
            app_name: "Tileception".into(),
            app_icon: None,
            summary: summary.into(),
            body: html_escape::encode_text(body).into_owned(),
            actions: Vec::new(),
            default_action: false,
            urgency,
            image: None,
            desktop_entry: None,
            category: Some(LOCAL_CATEGORY.into()),
            resident: false,
            transient: true,
            suppress_sound: true,
            x: None,
            y: None,
            sound_file: None,
            created: std::time::SystemTime::now(),
            expires: Some(Instant::now() + LOCAL_TIMEOUT),
            show_popup: true,
        };
        self.notifications.active.insert(notification_id, Arc::new(notification));
        let timer = smithay::reexports::calloop::timer::Timer::from_duration(LOCAL_TIMEOUT);
        let removed = self.loop_handle.insert_source(timer, move |_, _, state| {
            state.notifications.active.remove(&notification_id);
            state.schedule_render();
            smithay::reexports::calloop::timer::TimeoutAction::Drop
        });
        if let Err(error) = removed {
            tracing::warn!("could not time a notification: {error}");
        }
        self.schedule_render();
    }

    pub fn do_not_disturb(&self) -> bool {
        self.notifications.service.as_ref().is_some_and(|service| service.do_not_disturb())
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
        let width = screen.w.clamp(1.0, PANEL_W);
        Rect::new(screen.right() - width, screen.y, width, screen.h.max(1.0))
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
            let mut next_top = self.panel_controls().cards_top;
            let width = panel.w - 24.0;
            let mut cards = Vec::new();
            for notification in self.notifications.history.iter().rev().skip(self.notifications.offset) {
                let card = self.card_layout(notification.clone(), panel.x + 12.0, next_top, width, false);
                if card.rect.bottom() > panel.bottom() - 52.0 {
                    break;
                }
                next_top = card.rect.bottom() + 8.0;
                cards.push(card);
            }
            return cards;
        }
        let width = (screen.w - 24.0).clamp(1.0, POPUP_W);
        let locked = self.is_locked();
        let mut items: Vec<_> = self
            .notifications
            .active
            .values()
            .filter(|notification| {
                notification.show_popup && (!locked || notification.category.as_deref() == Some(LOCAL_CATEGORY))
            })
            .cloned()
            .collect();
        // Questions waiting for an answer (no timeout, with buttons) stay in the
        // corner, so newer popups cannot push them off screen.
        items.sort_by_key(|notification| {
            (notification.expires.is_some() || notification.actions.is_empty(), std::cmp::Reverse(notification.created))
        });
        items.truncate(3);
        let mut bottom =
            self.strip_now().map_or(screen.bottom() - 14.0, |strip| (strip.y - 12.0).min(screen.bottom() - 14.0));
        let mut cards = Vec::new();
        for notification in items {
            let mut card = self.card_layout(notification, screen.right() - width - 14.0, 0.0, width, true);
            let height = card.rect.h;
            if bottom - height < screen.y + 14.0 {
                break;
            }
            card.shift(bottom - height);
            bottom -= height + 10.0;
            cards.push(card);
        }
        cards
    }

    /// Lays out one card with its top left at (x, y).
    fn card_layout(&self, notification: Arc<Notification>, left: f64, card_top: f64, width: f64, popup: bool) -> Card {
        let theme = &self.theme;
        let mut cursor = card_top + 14.0;
        let header = Rect::new(left + 16.0, cursor, width - 56.0, 20.0);
        let close = Rect::new(left + width - 36.0, card_top + 8.0, 28.0, 28.0);
        cursor = header.bottom() + 10.0;
        let preview = self.notification_preview(&notification).map(|(_, image_width, image_height)| {
            let placed = Rect::new(
                left + 16.0 + ((width - 32.0 - image_width as f64) / 2.0).max(0.0),
                cursor,
                image_width as f64,
                image_height as f64,
            );
            cursor = placed.bottom() + 12.0;
            placed
        });
        let icon = (preview.is_none() && self.notification_icon(&notification, ICON).is_some())
            .then(|| Rect::new(left + 16.0, cursor + 2.0, ICON as f64, ICON as f64));
        let text_x = icon.map_or(left + 16.0, |icon_rect| icon_rect.right() + 12.0);
        let text_w = left + width - 16.0 - text_x;
        let summary = Rect::new(text_x, cursor, text_w, 22.0);
        let body_text = plain_body(&notification.body);
        let body_h = if body_text.trim().is_empty() {
            0.0
        } else {
            self.text
                .borrow_mut()
                .styled(
                    body_text.trim(),
                    &theme.font,
                    theme.font_size * 0.95,
                    theme.muted,
                    text_w as f32,
                    400,
                    BODY_LINES,
                )
                .map_or(0.0, |rendered| rendered.height)
        };
        let body = Rect::new(text_x, summary.bottom() + 2.0, text_w, body_h);
        cursor = body.bottom().max(icon.map_or(0.0, |icon_rect| icon_rect.bottom()));
        let count = notification.actions.len();
        let actions = if count > 0 {
            let top = cursor + 12.0;
            let gap = 8.0;
            let each = (width - 32.0 - gap * (count as f64 - 1.0)) / count as f64;
            cursor = top + 32.0;
            (0..count).map(|index| Rect::new(left + 16.0 + index as f64 * (each + gap), top, each, 32.0)).collect()
        } else {
            Vec::new()
        };
        let rect = Rect::new(left, card_top, width, cursor + 16.0 - card_top);
        Card { notification, rect, header, close, preview, icon, summary, body, body_text, actions, popup }
    }

    pub fn notifications_contain(&self, point: Point) -> bool {
        if self.overlay_under(point) {
            return false;
        }
        if self.notifications.open && self.notification_panel().contains(point) {
            return true;
        }
        self.notification_cards().iter().any(|card| card.rect.contains(point))
    }

    pub fn notification_click(&mut self, point: Point) {
        if self.notifications.open {
            let controls = self.panel_controls();
            if controls.clear.contains(point) {
                self.notification_request(Request::ClearHistory);
                self.notification_request(Request::DismissAll);
                return;
            }
            if self.builtin_notifications() && controls.dnd.contains(point) {
                self.toggle_dnd();
                return;
            }
            if self.controls_press(&controls.controls, point) {
                return;
            }
        }
        for card in self.notification_cards() {
            if !card.rect.contains(point) {
                continue;
            }
            let notification = &card.notification;
            if card.close.contains(point) {
                if self.notifications.open {
                    self.notification_request(Request::RemoveFromHistory(notification.id));
                }
                self.notification_request(Request::Dismiss(notification.id));
            } else if let Some(index) = card.actions.iter().position(|button| button.contains(point)) {
                if let Some((key, _)) = notification.actions.get(index) {
                    self.notification_request(Request::Invoke {
                        id: notification.id,
                        action: key.clone(),
                        token: None,
                    });
                }
            } else if notification.default_action {
                self.notification_request(Request::Invoke {
                    id: notification.id,
                    action: "default".into(),
                    token: None,
                });
            }
            return;
        }
    }

    pub fn notification_elements(&self, renderer: &mut GlesRenderer) -> Vec<OutputElement> {
        let theme = &self.theme;
        let pointer = Point::new(self.pointer.x, self.pointer.y);
        let dnd = self.notifications.service.as_ref().is_some_and(|service| service.do_not_disturb());
        let mut painter = Painter::new(self, renderer);
        if self.notifications.open {
            let panel = self.notification_panel();
            let controls = self.panel_controls();
            painter.edge_panel(panel, false);
            let title = Style::new(theme.font_size * 1.3, theme.text).bold();
            let (title_w, _) = painter.text(
                if self.builtin_notifications() { "Notifications" } else { "Controls" },
                Rect::new(panel.x + 18.0, panel.y + 14.0, panel.w - 150.0, 32.0),
                title,
            );
            let count = self.notifications.history.len();
            if count > 0 {
                let label = count.to_string();
                let style = Style::new(theme.font_size * 0.8, theme.on_accent).bold().align(Align::Centre);
                let width = (painter.measure(&label, 100.0, style).0 + 14.0).max(22.0);
                let chip = Rect::new(panel.x + 18.0 + title_w + 10.0, panel.y + 20.0, width, 20.0);
                painter
                    .shape(chip, Shape { radius: 10.0, fill: (theme.accent, theme.accent_end), ..Default::default() });
                painter.text(&label, chip, style);
            }
            if count > 0 {
                let hover = controls.clear.contains(pointer);
                if hover {
                    painter.fill(controls.clear, theme.surface_raised, theme.radius);
                }
                let colour = if hover { theme.text } else { theme.muted };
                painter.glyph(
                    glyph::CLEAR,
                    Rect::new(controls.clear.x + 6.0, controls.clear.y, 24.0, 32.0),
                    15,
                    colour,
                );
                painter.text(
                    "Clear all",
                    Rect::new(controls.clear.x + 32.0, controls.clear.y, 60.0, 32.0),
                    Style::new(theme.font_size * 0.95, colour),
                );
            }

            self.draw_controls(&mut painter, &controls.controls, pointer);

            if self.builtin_notifications() {
                // Do not disturb, as a row with a switch.
                let row = controls.dnd;
                let hover = row.contains(pointer);
                painter.shape(
                    row,
                    Shape {
                        border_width: 1.0,
                        border: if dnd { theme.accent.alpha(0.4) } else { theme.border },
                        ..Shape::filled(if hover { theme.surface_raised } else { theme.surface }, theme.radius)
                    },
                );
                painter.glyph(
                    glyph::MOON,
                    Rect::new(row.x + 8.0, row.y, 32.0, row.h),
                    17,
                    if dnd { theme.accent } else { theme.muted },
                );
                painter.text(
                    "Do not disturb",
                    Rect::new(row.x + 44.0, row.y + 5.0, row.w - 110.0, 20.0),
                    Style::new(theme.font_size, theme.text),
                );
                let sub = if dnd { "Popups are silenced" } else { "Popups are shown" };
                painter.text(
                    sub,
                    Rect::new(row.x + 44.0, row.y + 24.0, row.w - 110.0, 18.0),
                    Style::new(theme.font_size * 0.9, theme.muted),
                );
                let track = Rect::new(row.right() - 56.0, row.y + (row.h - 24.0) / 2.0, 42.0, 24.0);
                let knob_x = if dnd { track.right() - 21.0 } else { track.x + 3.0 };
                if dnd {
                    painter.shape(
                        track,
                        Shape {
                            radius: 12.0,
                            fill: (theme.accent, theme.accent_end),
                            glow_radius: 10.0,
                            glow: theme.accent.alpha(0.25),
                            ..Default::default()
                        },
                    );
                } else {
                    painter.shape(
                        track,
                        Shape { border_width: 1.0, border: theme.border, ..Shape::filled(theme.background, 12.0) },
                    );
                }
                painter.fill(
                    Rect::new(knob_x, track.y + 3.0, 18.0, 18.0),
                    if dnd { theme.on_accent } else { theme.muted },
                    9.0,
                );
            }
            painter.fill(Rect::new(panel.x + 12.0, controls.cards_top - 10.0, panel.w - 24.0, 1.0), theme.border, 0.0);

            if self.notifications.history.is_empty() {
                let mid = (controls.cards_top + panel.bottom()) / 2.0 - 40.0;
                painter.fill(
                    Rect::new(panel.x + panel.w / 2.0 - 32.0, mid - 32.0, 64.0, 64.0),
                    theme.surface_raised,
                    32.0,
                );
                painter.glyph(glyph::BELL, Rect::new(panel.x, mid - 32.0, panel.w, 64.0), 28, theme.muted);
                let (headline, detail) = if !self.builtin_notifications() {
                    ("Notifications are handled elsewhere", "Switched off or replaced in [shell]")
                } else if self.notifications.service.is_some() {
                    ("No notifications", "You're all caught up")
                } else {
                    ("Notifications unavailable", "The notification service did not start")
                };
                painter.text(
                    headline,
                    Rect::new(panel.x, mid + 44.0, panel.w, 24.0),
                    Style::new(theme.font_size * 1.05, theme.text).bold().align(Align::Centre),
                );
                painter.text(
                    detail,
                    Rect::new(panel.x, mid + 68.0, panel.w, 22.0),
                    Style::new(theme.font_size * 0.9, theme.muted).align(Align::Centre),
                );
            }
            let hints: &[(&str, &str)] = if self.builtin_notifications() {
                &[("Esc", "Close"), ("Scroll", "Older")]
            } else {
                &[("Esc", "Close")]
            };
            painter.key_hints(hints, panel.x + 18.0, panel.bottom() - 26.0, false);
        }

        for card in self.notification_cards() {
            if card.popup {
                // Slides in from the right edge.
                let age = card.notification.created.elapsed().map(|elapsed| elapsed.as_secs_f64()).unwrap_or(1.0);
                let shown = ease_out(age / 0.24);
                painter.offset = ((1.0 - shown) * (card.rect.w + 24.0), 0.0);
                painter.alpha = shown as f32;
            }
            self.draw_card(&mut painter, &card, pointer);
            painter.offset = (0.0, 0.0);
            painter.alpha = 1.0;
        }
        painter.finish()
    }

    fn draw_card(&self, painter: &mut Painter, card: &Card, pointer: Point) {
        let theme = &self.theme;
        let notification = &card.notification;
        let rect = card.rect;
        let critical = notification.urgency == Urgency::Critical
            && notification.category.as_deref() != Some("x-tileception.prompt");
        if card.popup {
            painter.popup(rect);
        } else {
            painter.shape(
                rect,
                Shape { border_width: 1.0, border: theme.border, ..Shape::filled(theme.surface, theme.popup_radius) },
            );
        }
        // The accent stripe on the left edge; red for critical notifications.
        let stripe = Rect::new(rect.x + 5.0, rect.y + 14.0, 3.0, rect.h - 28.0);
        let (stripe_start, stripe_end) =
            if critical { (theme.danger, theme.danger) } else { (theme.accent, theme.accent_end) };
        painter.shape(stripe, Shape { radius: 1.5, fill: (stripe_start, stripe_end), ..Default::default() });

        // Header: app icon, app name and how long ago.
        let mut cursor_x = card.header.x;
        if let Some(icon) = self.notification_icon(notification, 16) {
            painter.image(&icon, (cursor_x, card.header.y + 2.0));
            cursor_x += 22.0;
        }
        let app = if notification.app_name.is_empty() { "Notification" } else { &notification.app_name };
        let header = format!("{app}  \u{00b7}  {}", ago(notification.created));
        painter.text(
            &header,
            Rect::new(cursor_x, card.header.y, card.header.right() - cursor_x, card.header.h),
            Style::new(theme.font_size * 0.9, theme.muted),
        );
        let hover_close = card.close.contains(pointer);
        if hover_close {
            painter.fill(card.close, theme.surface_raised, card.close.w as f32 / 2.0);
        }
        painter.glyph(glyph::CLOSE, card.close, 14, if hover_close { theme.text } else { theme.muted });

        if let (Some(preview_rect), Some((image, width, height))) =
            (card.preview, self.notification_preview(notification))
        {
            painter.image_sized(&image, (preview_rect.x, preview_rect.y), Some((width, height)));
            painter.shape(
                preview_rect,
                Shape { radius: theme.radius, border_width: 1.0, border: theme.border, ..Default::default() },
            );
        }
        if let (Some(icon_rect), Some(icon)) = (card.icon, self.notification_icon(notification, ICON)) {
            painter.image(&icon, (icon_rect.x, icon_rect.y));
        }
        painter.text(&notification.summary, card.summary, Style::new(theme.font_size * 1.02, theme.text).bold());
        if card.body.h > 0.0 {
            painter.text(
                card.body_text.trim(),
                card.body,
                Style::new(theme.font_size * 0.95, theme.muted).lines(BODY_LINES),
            );
        }
        for (index, (action, (_, label))) in card.actions.iter().zip(&notification.actions).enumerate() {
            let hover = action.contains(pointer);
            let primary = index == 0;
            if primary {
                let alpha = if hover { 0.32 } else { 0.2 };
                painter.shape(
                    *action,
                    Shape {
                        radius: theme.radius,
                        fill: (theme.accent.alpha(alpha), theme.accent_end.alpha(alpha * 0.6)),
                        border_width: 1.0,
                        border: theme.accent.alpha(0.5),
                        ..Default::default()
                    },
                );
            } else {
                let fill = if hover { theme.surface_raised.mix(theme.border, 0.5) } else { theme.surface_raised };
                painter.shape(
                    *action,
                    Shape { border_width: 1.0, border: theme.border, ..Shape::filled(fill, theme.radius) },
                );
            }
            let colour = if primary {
                theme.accent
            } else if hover {
                theme.text
            } else {
                theme.text.alpha(0.85)
            };
            painter.text(
                label,
                action.inset(6.0),
                Style::new(theme.font_size * 0.92, colour).bold().align(Align::Centre),
            );
        }

        // How long the popup has left, as a thin bar along the bottom.
        if let (true, Some(expires)) = (card.popup, notification.expires) {
            let left = expires.saturating_duration_since(Instant::now()).as_secs_f64();
            let total = left + notification.created.elapsed().map(|elapsed| elapsed.as_secs_f64()).unwrap_or(0.0);
            if total > 0.0 {
                let track = Rect::new(rect.x + 16.0, rect.bottom() - 6.0, rect.w - 32.0, 2.0);
                painter.fill(track, theme.border.alpha(0.6), 1.0);
                let bar = Rect::new(track.x, track.y, track.w * (left / total).clamp(0.0, 1.0), track.h);
                painter.shape(bar, Shape { radius: 1.0, fill: (stripe_start, stripe_end), ..Default::default() });
            }
        }
    }

    /// An app icon for the notification, from its hints or the sending app.
    fn notification_icon(&self, notification: &Notification, size: u32) -> Option<MemoryRenderBuffer> {
        let named = |name: &str| {
            let path = name.strip_prefix("file://").unwrap_or(name);
            if path.starts_with('/') {
                self.launcher.icons.image(std::path::Path::new(path), size)
            } else {
                self.launcher.app_icon(path, size)
            }
        };
        if let Some(Image::Path(path)) = &notification.image
            && !is_file(path)
            && let Some(icon) = named(path)
        {
            return Some(icon);
        }
        // A picture file given as the icon is shown as the preview instead.
        notification
            .app_icon
            .as_deref()
            .filter(|icon| !icon.is_empty() && (!is_file(icon) || icon.ends_with(".svg")))
            .and_then(named)
            .or_else(|| notification.desktop_entry.as_deref().and_then(|entry| self.launcher.app_icon(entry, size)))
    }

    /// A picture the notification carries (a screenshot, an album cover), scaled to fit a card.
    fn notification_preview(&self, notification: &Notification) -> Option<Preview> {
        if let Some(cached) = self.notifications.thumbnails.borrow().get(&notification.id) {
            return cached.clone();
        }
        let preview = load_preview(notification);
        let mut cache = self.notifications.thumbnails.borrow_mut();
        if cache.len() >= 128 {
            cache.clear();
        }
        cache.insert(notification.id, preview.clone());
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
    notification: Arc<Notification>,
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
    /// Moves the card down to start at `top`.
    fn shift(&mut self, top: f64) {
        let delta = top - self.rect.y;
        let down = |rect: &mut Rect| rect.y += delta;
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
fn load_preview(notification: &Notification) -> Option<Preview> {
    let image = match &notification.image {
        Some(Image::Rgba { width, height, data }) => image::RgbaImage::from_raw(*width, *height, data.clone())?,
        Some(Image::Path(path)) if is_file(path) => read_picture(path)?,
        Some(_) => return None,
        None => {
            read_picture(notification.app_icon.as_deref().filter(|icon| is_file(icon) && !icon.ends_with(".svg"))?)?
        }
    };
    // Small pictures (avatars, covers) stay small; large ones fill the card's width.
    let (width, height) = (image.width().max(1), image.height().max(1));
    let scale = (PREVIEW_W as f64 / width as f64).min(PREVIEW_H as f64 / height as f64).min(1.0);
    let (thumb_width, thumb_height) =
        (((width as f64 * scale).round() as u32).max(1), ((height as f64 * scale).round() as u32).max(1));
    let mut scaled = image::imageops::thumbnail(&image, thumb_width, thumb_height);
    for pixel in scaled.pixels_mut() {
        let alpha = pixel.0[3] as u32;
        for channel in &mut pixel.0[..3] {
            *channel = (*channel as u32 * alpha / 255) as u8;
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
    for character in markup.chars() {
        if tag {
            match character {
                '\'' | '"' if quote == Some(character) => quote = None,
                '\'' | '"' if quote.is_none() => quote = Some(character),
                '>' if quote.is_none() => tag = false,
                _ => {}
            }
        } else if character == '<' {
            tag = true;
        } else {
            out.push(character);
        }
    }
    html_escape::decode_html_entities(&out).into_owned()
}
