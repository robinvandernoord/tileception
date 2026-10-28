//! The strip of minimized windows. It hides below the bottom edge of the
//! screen and slides in when the pointer touches that edge, or briefly after
//! a window is minimized so you see where it went. Click a window to put it
//! back where it was; drag it out to place it like any dragged window.

use std::time::{Duration, Instant};

use smithay::{
    backend::renderer::gles::GlesRenderer,
    input::pointer::MotionEvent,
    utils::{Logical, Point as SPoint, SERIAL_COUNTER},
};
use tileception_core::{Point, Rect, WindowId};

use crate::{
    Tileception,
    bar::BAR_H,
    render::OutputElement,
    shapes::Shape,
    ui::{Painter, Style, ease_out, glyph},
};

/// Seconds the strip takes to slide in or out.
const SLIDE: f64 = 0.18;
/// How long the strip shows after minimizing.
const PEEK: Duration = Duration::from_millis(1600);
/// Pixels at the bottom of the screen that call the strip.
const EDGE: f64 = 2.0;
/// Pixels around the strip the pointer may stray before it hides.
const SLACK: f64 = 24.0;
const CARD_W: f64 = 208.0;
const MIN_CARD_W: f64 = 160.0;
const FOOTER_H: f64 = 30.0;
const PREVIEW_H: f64 = 112.0;
const LABEL_H: f64 = 34.0;
const PAD: f64 = 10.0;
const MARGIN: f64 = 14.0;

pub struct Strip {
    open: bool,
    /// When `open` last changed, and how far out the strip was then.
    changed: Instant,
    level_then: f64,
    /// Shown after a minimize until then, unless the pointer keeps it.
    peek_until: Option<Instant>,
    /// A card pressed and not yet let go: a click restores, a drag places.
    press: Option<(WindowId, Point)>,
    first: usize,
    scroll: f64,
}

impl Default for Strip {
    fn default() -> Self {
        Strip {
            open: false,
            changed: Instant::now(),
            level_then: 0.0,
            peek_until: None,
            press: None,
            first: 0,
            scroll: 0.0,
        }
    }
}

impl Strip {
    /// How far out the strip is, from 0 (hidden) to 1.
    fn level(&self) -> f64 {
        let p = (self.changed.elapsed().as_secs_f64() / SLIDE).clamp(0.0, 1.0);
        if self.open { self.level_then + (1.0 - self.level_then) * p } else { self.level_then * (1.0 - p) }
    }

    fn set_open(&mut self, open: bool) {
        if open != self.open {
            self.level_then = self.level();
            self.open = open;
            self.changed = Instant::now();
        }
    }
}

/// What is under the pointer in the strip.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Hit {
    Card(WindowId),
    Close(WindowId),
    Previous,
    Next,
    Strip,
}

struct Card {
    window: WindowId,
    rect: Rect,
    preview: Rect,
    label: Rect,
    close: Rect,
}

impl Tileception {
    /// Shows the strip for a moment, after a window was minimized.
    pub fn peek_minimized(&mut self) {
        self.strip.first = 0;
        self.strip.peek_until = Some(Instant::now() + PEEK);
        self.strip.set_open(true);
    }

    /// Opens or hides the strip for where the pointer is. Called on pointer
    /// motion and after each frame.
    pub fn update_strip(&mut self) {
        let available = self.world.minimized().next().is_some()
            && !self.is_locked()
            // The welcome screen leaves the strip be: minimized windows come back from it.
            && (!self.launcher.open || self.launcher.home)
            && self.power_menu.is_none()
            && self.fullscreen.is_none()
            && self.move_drag.is_none();
        let p = Point::new(self.pointer.x, self.pointer.y);
        let out = self.output_rect();
        let at_edge = p.y >= out.bottom() - EDGE;
        let kept = self.strip.open
            && self.strip_rect().is_some_and(|r| {
                let area = expand(r, SLACK);
                // Keep the strip open while crossing a bottom bar to reach it.
                Rect { h: out.bottom() - area.y, ..area }.contains(p)
            });
        let peeking = self.strip.peek_until.is_some_and(|t| Instant::now() < t);
        if !peeking {
            self.strip.peek_until = None;
        }
        let open = available && (at_edge || kept || peeking || self.strip.press.is_some());
        if !open {
            self.strip.press = None;
        }
        self.strip.set_open(open);
    }

    /// The strip where it is now as it slides, while it is out at all.
    pub fn strip_now(&self) -> Option<Rect> {
        let level = ease_out(self.strip.level());
        let strip = self.strip_rect().filter(|_| level > 0.001)?;
        let bar_height = if self.bar_at_bottom() { BAR_H } else { 0.0 };
        Some(Rect { y: strip.y + (1.0 - level) * (strip.h + MARGIN + 12.0 + bar_height), ..strip })
    }

    /// The strip at its full height, when there is anything in it.
    fn strip_rect(&self) -> Option<Rect> {
        let cards = self.strip_cards();
        let first = cards.first()?;
        let last = cards.last()?;
        Some(Rect::new(
            first.rect.x - PAD,
            first.rect.y - PAD,
            last.rect.right() - first.rect.x + 2.0 * PAD,
            first.rect.h + 2.0 * PAD + if self.strip_overflows() { FOOTER_H } else { 0.0 },
        ))
    }

    fn strip_capacity(&self) -> usize {
        (((self.output_rect().w - 2.0 * (MARGIN + PAD) + PAD) / (MIN_CARD_W + PAD)).floor() as usize).max(1)
    }

    fn strip_overflows(&self) -> bool {
        self.world.minimized().count() > self.strip_capacity()
    }

    fn strip_cards(&self) -> Vec<Card> {
        let windows: Vec<WindowId> = self.world.minimized().rev().collect();
        if windows.is_empty() {
            return Vec::new();
        }
        let out = self.output_rect();
        let capacity = self.strip_capacity();
        let first = self.strip.first.min(windows.len().saturating_sub(capacity));
        let n = windows.len().min(capacity) as f64;
        let room = (out.w - 2.0 * (MARGIN + PAD)).max(1.0);
        let w = CARD_W.min((room - (n - 1.0) * PAD) / n);
        let h = PREVIEW_H + LABEL_H;
        let total = n * w + (n - 1.0) * PAD;
        let x0 = out.x + (out.w - total) / 2.0;
        let bar_height = if self.bar_at_bottom() { BAR_H } else { 0.0 };
        let y = out.bottom() - bar_height - MARGIN - PAD - h - if self.strip_overflows() { FOOTER_H } else { 0.0 };
        windows
            .into_iter()
            .skip(first)
            .take(capacity)
            .enumerate()
            .map(|(i, window)| {
                let rect = Rect::new(x0 + i as f64 * (w + PAD), y, w, h);
                let preview = Rect::new(rect.x + 6.0, rect.y + 6.0, rect.w - 12.0, PREVIEW_H - 6.0);
                let label = Rect::new(rect.x + 8.0, rect.y + PREVIEW_H, rect.w - 16.0, LABEL_H);
                let close = Rect::new(rect.right() - 30.0, rect.y + 10.0, 22.0, 22.0);
                Card { window, rect, preview, label, close }
            })
            .collect()
    }

    /// Whether the strip has something at this point.
    pub(crate) fn strip_takes(&self, p: Point) -> bool {
        self.strip_hit(p).is_some()
    }

    fn strip_hit(&self, p: Point) -> Option<Hit> {
        if !self.strip.open || self.overlay_under(p) {
            return None;
        }
        for card in self.strip_cards() {
            if card.close.contains(p) {
                return Some(Hit::Close(card.window));
            }
            if card.rect.contains(p) {
                return Some(Hit::Card(card.window));
            }
        }
        if let Some(strip) = self.strip_rect().filter(|_| self.strip_overflows()) {
            let (previous, next) = strip_paging_buttons(strip);
            if previous.contains(p) {
                return Some(Hit::Previous);
            }
            if next.contains(p) {
                return Some(Hit::Next);
            }
        }
        self.strip_rect().filter(|r| r.contains(p)).map(|_| Hit::Strip)
    }

    fn step_minimized(&mut self, delta: isize) {
        let last = self.world.minimized().count().saturating_sub(self.strip_capacity());
        self.strip.first = self.strip.first.min(last).saturating_add_signed(delta).min(last);
        self.strip.peek_until = Some(Instant::now() + PEEK);
        self.schedule_render();
    }

    /// Scroll the strip without shrinking previews or passing the wheel to a window.
    pub fn strip_scroll(&mut self, amount: f64) -> bool {
        if self.strip_hit(Point::new(self.pointer.x, self.pointer.y)).is_none() {
            return false;
        }
        self.strip.scroll += amount;
        while self.strip.scroll.abs() >= 1.0 {
            let step = self.strip.scroll.signum();
            self.strip.scroll -= step;
            self.step_minimized(step as isize);
        }
        true
    }

    /// Pointer motion over the strip, which keeps it from the windows below.
    /// Returns whether the strip took the motion.
    pub fn strip_motion(&mut self, pos: SPoint<f64, Logical>, time: u32) -> bool {
        self.update_strip();
        let busy = self.gap_drag.is_some()
            || self.resize_drag.is_some()
            || self.selection.anchor.is_some()
            || self.pending_press.is_some();
        if busy {
            return false;
        }
        let p = Point::new(pos.x, pos.y);
        if let Some((window, at)) = self.strip.press
            && (p.x - at.x).hypot(p.y - at.y) > crate::input::DRAG_THRESHOLD
        {
            // Dragged out: the window follows the pointer, to be dropped.
            self.strip.press = None;
            self.start_move_drag(window, pos);
            self.update_strip();
            return false;
        }
        if self.strip_hit(p).is_none() && self.strip.press.is_none() {
            return false;
        }
        let pointer = self.seat.get_pointer().expect("seat has a pointer");
        pointer.motion(self, None, &MotionEvent { location: pos, serial: SERIAL_COUNTER.next_serial(), time });
        pointer.frame(self);
        true
    }

    /// A button over the strip. Returns whether the strip took it.
    pub fn strip_button(&mut self, button: u32, pressed: bool) -> bool {
        let p = Point::new(self.pointer.x, self.pointer.y);
        if !pressed {
            let Some((window, _)) = self.strip.press.take() else { return false };
            // A click: back where it was.
            let now = self.now();
            if self.world.restore(window, now) {
                self.apply_layout();
                self.persist_layout();
            }
            self.update_strip();
            return true;
        }
        let Some(hit) = self.strip_hit(p) else { return false };
        match hit {
            Hit::Card(window) if button == crate::input::BTN_LEFT => self.strip.press = Some((window, p)),
            Hit::Close(window) if button == crate::input::BTN_LEFT => self.close_window(window),
            Hit::Previous if button == crate::input::BTN_LEFT => self.step_minimized(-1),
            Hit::Next if button == crate::input::BTN_LEFT => self.step_minimized(1),
            _ => {}
        }
        true
    }

    pub fn strip_elements(&self, renderer: &mut GlesRenderer) -> Vec<OutputElement> {
        let level = ease_out(self.strip.level());
        let Some(strip) = self.strip_rect().filter(|_| level > 0.001) else { return Vec::new() };
        let t = &self.theme;
        let pointer = Point::new(self.pointer.x, self.pointer.y);
        let hit = self.strip_hit(pointer);
        let pressed = self.strip.press.map(|(w, _)| w);
        let mut p = Painter::new(self, renderer);
        p.offset = (0.0, (1.0 - level) * (strip.h + MARGIN + 12.0));
        p.alpha = level as f32;
        p.popup(strip);
        for card in self.strip_cards() {
            let hover = matches!(hit, Some(Hit::Card(w) | Hit::Close(w)) if w == card.window);
            let lit = hover || pressed == Some(card.window);
            let fill = if lit { t.surface_raised } else { t.surface };
            p.shape(
                card.rect,
                Shape {
                    border_width: 1.0,
                    border: if lit { t.accent.alpha(0.7) } else { t.border },
                    glow_radius: if lit { t.glow * 0.6 } else { 0.0 },
                    glow: t.accent.alpha(0.2),
                    ..Shape::filled(fill, t.radius)
                },
            );
            // The window as it was, fitted into the preview.
            p.fill(card.preview, t.background, (t.radius - 2.0).max(0.0));
            if let Some(fitted) = self.fit_preview(card.window, card.preview.inset(4.0)) {
                p.window(card.window, fitted);
            }
            let window = self.windows.get(&card.window);
            let mut x = card.label.x;
            let icon = window.and_then(|w| self.launcher.app_icon(&crate::state::app_id(w), 18));
            if let Some(icon) = icon {
                p.image(&icon, (x, card.label.y + (card.label.h - 18.0) / 2.0));
                x += 26.0;
            }
            let title = window.map(crate::state::window_title).unwrap_or_default();
            let colour = if lit { t.text } else { t.text.alpha(0.82) };
            p.text(
                &title,
                Rect::new(x, card.label.y, card.label.right() - x, card.label.h),
                Style::new(t.font_size * 0.92, colour),
            );
            if hover {
                let on_close = hit == Some(Hit::Close(card.window));
                p.fill(card.close, if on_close { t.surface_raised } else { t.popup() }, card.close.w as f32 / 2.0);
                p.glyph(glyph::CLOSE, card.close, 13, if on_close { t.text } else { t.muted });
            }
        }
        if self.strip_overflows() {
            let count = self.world.minimized().count();
            let capacity = self.strip_capacity();
            let first = self.strip.first.min(count.saturating_sub(capacity));
            let (previous, next) = strip_paging_buttons(strip);
            for (rect, icon, which, enabled) in [
                (previous, glyph::CHEVRON_RIGHT, Hit::Previous, first > 0),
                (next, glyph::CHEVRON_RIGHT, Hit::Next, first + capacity < count),
            ] {
                if hit == Some(which) && enabled {
                    p.fill(rect, t.surface_raised, t.radius);
                }
                let icon = if which == Hit::Previous { glyph::ARROW_LEFT } else { icon };
                p.glyph(icon, rect, 14, if enabled { t.text } else { t.muted.alpha(0.5) });
            }
            let label = format!("{} to {} of {count} · Scroll for more", first + 1, (first + capacity).min(count));
            p.text(
                &label,
                Rect::new(previous.right() + 6.0, previous.y, next.x - previous.right() - 12.0, previous.h),
                Style::new(t.font_size * 0.85, t.muted).align(crate::ui::Align::Centre),
            );
        }
        p.finish()
    }

    /// Where a window's picture goes inside `area`, keeping its proportions.
    fn fit_preview(&self, id: WindowId, area: Rect) -> Option<Rect> {
        let geo = self.windows.get(&id)?.geometry();
        if geo.size.w <= 0 || geo.size.h <= 0 {
            return None;
        }
        let (w, h) = (geo.size.w as f64, geo.size.h as f64);
        let s = (area.w / w).min(area.h / h).min(1.0);
        Some(Rect::new(area.x + (area.w - w * s) / 2.0, area.y + (area.h - h * s) / 2.0, w * s, h * s))
    }
}

fn strip_paging_buttons(strip: Rect) -> (Rect, Rect) {
    let y = strip.bottom() - PAD - FOOTER_H;
    (Rect::new(strip.x + PAD, y, 28.0, FOOTER_H), Rect::new(strip.right() - PAD - 28.0, y, 28.0, FOOTER_H))
}

fn expand(r: Rect, by: f64) -> Rect {
    Rect::new(r.x - by, r.y - by, r.w + 2.0 * by, r.h + 2.0 * by)
}
