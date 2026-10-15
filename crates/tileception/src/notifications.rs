//! Notification popups and history. The service runs off the compositor thread.

use std::{
    cell::RefCell,
    collections::{BTreeMap, HashMap},
    sync::{mpsc, Arc},
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
use tileception_notify::{Event, Image, Notification, NotifyService, Request};

use crate::{
    render::{push_image, push_shape, OutputElement},
    shapes::Shape,
    Tileception,
};

#[derive(Default)]
pub struct Notifications {
    service: Option<NotifyService>,
    events: Option<mpsc::Receiver<Event>>,
    active: BTreeMap<u32, Arc<Notification>>,
    history: Vec<Arc<Notification>>,
    pub open: bool,
    offset: usize,
    thumbnails: RefCell<HashMap<u32, Option<MemoryRenderBuffer>>>,
}

impl Tileception {
    pub fn start_notifications(&mut self, address: Option<String>) {
        let result = (|| {
            let (ping, source) = make_ping().map_err(|e| e.to_string())?;
            let (service, events) =
                NotifyService::start(Default::default(), address, move || ping.ping()).map_err(|e| e.to_string())?;
            self.loop_handle
                .insert_source(source, |_, _, state| state.notification_events())
                .map_err(|e| e.to_string())?;
            Ok::<_, String>((service, events))
        })();
        match result {
            Ok((service, events)) => {
                self.notifications.service = Some(service);
                self.notifications.events = Some(events);
            }
            Err(error) => tracing::error!("could not start notifications: {error}"),
        }
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

    pub fn toggle_dnd(&mut self) {
        if let Some(service) = &self.notifications.service {
            service.send(Request::SetDoNotDisturb(!service.do_not_disturb()));
        }
    }

    pub fn notification_scroll(&mut self, delta: isize) {
        let len = self.notifications.history.len();
        self.notifications.offset = self.notifications.offset.saturating_add_signed(delta).min(len.saturating_sub(1));
    }

    fn notification_cards(&self) -> Vec<(Arc<Notification>, Rect)> {
        let screen = self.output_rect();
        let w = (screen.w - 24.0).clamp(1.0, 390.0);
        let items: Vec<_> = if self.notifications.open {
            self.notifications.history.iter().rev().skip(self.notifications.offset).cloned().collect()
        } else {
            let mut items: Vec<_> = self.notifications.active.values().filter(|n| n.show_popup).cloned().collect();
            items.sort_by_key(|n| std::cmp::Reverse(n.created));
            items.truncate(3);
            items
        };
        let mut y = if self.notifications.open { 72.0 } else { 20.0 };
        items
            .into_iter()
            .filter_map(|n| {
                let h = if n.actions.is_empty() { 126.0 } else { 166.0 };
                if y + h > screen.bottom() - 16.0 {
                    return None;
                }
                let rect = Rect::new(screen.right() - w - 12.0, y, w, h);
                y += h + 10.0;
                Some((n, rect))
            })
            .collect()
    }

    pub fn notifications_contain(&self, p: Point) -> bool {
        let screen = self.output_rect();
        if self.notifications.open && p.x >= screen.right() - 414.0 {
            return true;
        }
        self.notification_cards().iter().any(|(_, rect)| rect.contains(p))
    }

    pub fn notification_click(&mut self, p: Point) {
        if self.notifications.open && p.y < 64.0 {
            let screen = self.output_rect();
            if p.x > screen.right() - 80.0 {
                self.toggle_dnd();
            } else if p.x > screen.right() - 160.0 {
                self.notification_request(Request::ClearHistory);
                self.notification_request(Request::DismissAll);
            } else {
                self.notifications.open = false;
            }
            return;
        }
        for (n, rect) in self.notification_cards() {
            if !rect.contains(p) {
                continue;
            }
            if p.x >= rect.right() - 36.0 && p.y < rect.y + 36.0 {
                if self.notifications.open {
                    self.notification_request(Request::RemoveFromHistory(n.id));
                }
                self.notification_request(Request::Dismiss(n.id));
            } else if !n.actions.is_empty() && p.y >= rect.bottom() - 42.0 {
                let i = ((p.x - rect.x - 12.0).max(0.0) / ((rect.w - 24.0) / n.actions.len() as f64)) as usize;
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
        let mut out = Vec::new();
        let t = &self.theme;
        let shapes = self.shapes(renderer);
        for (n, rect) in self.notification_cards() {
            self.ui_label(
                &mut out,
                renderer,
                &n.app_name,
                Rect::new(rect.x + 14.0, rect.y + 6.0, rect.w - 56.0, 24.0),
                t.muted,
                false,
            );
            self.ui_label(
                &mut out,
                renderer,
                "x",
                Rect::new(rect.right() - 28.0, rect.y + 4.0, 20.0, 24.0),
                t.muted,
                false,
            );
            let thumbnail = {
                if !self.notifications.thumbnails.borrow().contains_key(&n.id) {
                    let image = notification_image(&n, self);
                    let mut cache = self.notifications.thumbnails.borrow_mut();
                    if cache.len() >= 128 {
                        cache.clear();
                    }
                    cache.insert(n.id, image);
                }
                self.notifications.thumbnails.borrow().get(&n.id).cloned().flatten()
            };
            let left = if thumbnail.is_some() { 76.0 } else { 14.0 };
            if let Some(image) = thumbnail {
                push_image(&mut out, renderer, &image, (rect.x + 12.0, rect.y + 42.0));
            }
            self.ui_label(
                &mut out,
                renderer,
                &n.summary,
                Rect::new(rect.x + left, rect.y + 34.0, rect.w - left - 14.0, 26.0),
                t.text,
                true,
            );
            let body = plain_body(&n.body);
            for (i, line) in body.lines().take(2).enumerate() {
                self.ui_label(
                    &mut out,
                    renderer,
                    line,
                    Rect::new(rect.x + left, rect.y + 66.0 + i as f64 * 22.0, rect.w - left - 14.0, 22.0),
                    t.muted,
                    false,
                );
            }
            let count = n.actions.len();
            for (i, (_, label)) in n.actions.iter().enumerate() {
                let w = (rect.w - 24.0) / count as f64;
                let action = Rect::new(rect.x + 12.0 + i as f64 * w, rect.bottom() - 38.0, w - 4.0, 28.0);
                self.ui_label(&mut out, renderer, label, action.inset(4.0), t.accent, false);
                push_shape(&mut out, shapes.as_ref(), &action, Shape::filled(t.surface_raised, t.radius));
            }
            push_shape(
                &mut out,
                shapes.as_ref(),
                &rect,
                Shape { border_width: 1.0, border: t.border, ..Shape::filled(t.surface, t.popup_radius) },
            );
        }
        if self.notifications.open {
            let screen = self.output_rect();
            let panel = Rect::new((screen.right() - 414.0).max(screen.x), screen.y, screen.w.min(414.0), screen.h);
            self.ui_label(
                &mut out,
                renderer,
                "Notifications",
                Rect::new(panel.x + 16.0, 16.0, panel.w - 178.0, 34.0),
                t.text,
                true,
            );
            self.ui_label(
                &mut out,
                renderer,
                "Clear",
                Rect::new(panel.right() - 152.0, 16.0, 68.0, 34.0),
                t.muted,
                false,
            );
            let dnd = self.notifications.service.as_ref().is_some_and(|s| s.do_not_disturb());
            self.ui_label(
                &mut out,
                renderer,
                if dnd { "DND on" } else { "DND off" },
                Rect::new(panel.right() - 76.0, 16.0, 68.0, 34.0),
                if dnd { t.accent } else { t.muted },
                false,
            );
            if self.notification_cards().is_empty() {
                self.ui_label(
                    &mut out,
                    renderer,
                    if self.notifications.service.is_some() {
                        "No notifications"
                    } else {
                        "Notification service unavailable"
                    },
                    Rect::new(panel.x + 20.0, 82.0, panel.w - 40.0, 32.0),
                    t.muted,
                    false,
                );
            }
            push_shape(&mut out, shapes.as_ref(), &panel, Shape::filled(t.background, 0.0));
        }
        out
    }
}

fn notification_image(n: &Notification, state: &Tileception) -> Option<MemoryRenderBuffer> {
    match &n.image {
        Some(Image::Rgba { width, height, data }) => {
            let image = image::RgbaImage::from_raw(*width, *height, data.clone())?;
            let mut scaled = image::imageops::thumbnail(&image, 56, 56);
            for pixel in scaled.pixels_mut() {
                let a = pixel.0[3] as u32;
                for c in &mut pixel.0[..3] {
                    *c = (*c as u32 * a / 255) as u8;
                }
            }
            Some(MemoryRenderBuffer::from_slice(
                scaled.as_raw(),
                Fourcc::Abgr8888,
                (scaled.width() as i32, scaled.height() as i32),
                1,
                Transform::Normal,
                None,
            ))
        }
        Some(Image::Path(path)) => {
            let path = path.strip_prefix("file://").unwrap_or(path);
            if path.starts_with('/') {
                state.launcher.icons.image(std::path::Path::new(path), 56)
            } else {
                state.launcher.app_icon(path, 56)
            }
        }
        None => n.app_icon.as_deref().and_then(|icon| state.launcher.app_icon(icon, 56)),
    }
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
