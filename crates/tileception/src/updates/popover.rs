//! The card under the bar's updates chip: what is waiting by kind, the
//! housekeeping offers, and the buttons that install.

use tileception_core::{Point, Rect};
use tileception_updates::{self as backend, apt};

use super::{Page, Task};
use crate::{
    Tileception,
    bar::Zoom,
    shapes::Shape,
    ui::{Align, Button, Look, Painter, PointerTarget, Style, glyph},
};

#[derive(Default)]
pub(crate) struct Popover {
    pub(crate) zoom: Zoom,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Hit {
    Check,
    Install,
    Security,
    Review,
    Clean,
    Release,
    Restart,
}

impl crate::ui::PointerAction for Hit {
    fn activate(&self, state: &mut Tileception, _rect: Rect, _point: Point) {
        state.updates_popover_activate(*self);
    }
}

/// One block of the card, top to bottom.
enum Part {
    Header,
    Group { label: &'static str, warning: bool, names: String, count: usize },
    Rule,
    Offer { title: String, detail: String, button: &'static str, hit: Hit },
    Progress,
    Note { text: String, error: bool },
    Buttons(Vec<(&'static str, Hit)>),
    Footer(String),
}

impl Part {
    fn height(&self) -> f64 {
        match self {
            Part::Header => 46.0,
            Part::Group { .. } => 28.0,
            Part::Rule => 13.0,
            Part::Offer { .. } => 46.0,
            Part::Progress => 50.0,
            Part::Note { .. } => 26.0,
            Part::Buttons(_) => 42.0,
            Part::Footer(_) => 22.0,
        }
    }
}

const WIDTH: f64 = 344.0;
const PAD: f64 = 14.0;

/// "openssl, curl, 2 more".
fn names(updates: &[&apt::Update]) -> String {
    let shown: Vec<&str> = updates.iter().take(2).map(|update| update.name.as_str()).collect();
    match updates.len().saturating_sub(shown.len()) {
        0 => shown.join(", "),
        more => format!("{}, {more} more", shown.join(", ")),
    }
}

impl Tileception {
    pub(crate) fn toggle_updates_popover(&mut self) {
        if self.updates.popover.zoom.open {
            self.close_updates_popover();
            return;
        }
        if self.is_locked() || !self.updates_available() {
            return;
        }
        self.close_calendar();
        self.close_tray();
        self.close_minimap();
        self.close_network();
        let duration = self.world.settings.animation;
        self.updates.popover.zoom.set(true, duration);
        self.schedule_render();
    }

    /// Zooms the card back into its chip. Returns whether it was open.
    pub(crate) fn close_updates_popover(&mut self) -> bool {
        let duration = self.world.settings.animation;
        let closed = self.updates.popover.zoom.set(false, duration);
        if closed {
            self.schedule_render();
        }
        closed
    }

    fn updates_popover_activate(&mut self, hit: Hit) {
        let result = match hit {
            Hit::Check => self.updates_start(Task::Refresh),
            Hit::Install => self.updates_install(false),
            Hit::Security => self.updates_install(true),
            Hit::Review => {
                self.close_updates_popover();
                self.open_updates(Page::Available);
                Ok(())
            }
            Hit::Clean => {
                self.close_updates_popover();
                self.open_updates(Page::Kernels);
                self.updates_queue_suggested();
                Ok(())
            }
            Hit::Release => {
                self.close_updates_popover();
                self.open_updates(Page::Release);
                Ok(())
            }
            Hit::Restart => {
                self.close_updates_popover();
                self.shell_power_menu();
                if let Some(menu) = self.power_menu.as_mut() {
                    let restart = crate::power::PowerAction::Restart;
                    menu.selected =
                        crate::power::PowerAction::ALL.iter().position(|action| *action == restart).unwrap_or(0);
                }
                Ok(())
            }
        };
        if let Err(error) = result {
            self.updates.outcome = Some(Err(error));
        }
        self.schedule_render();
    }

    fn updates_popover_parts(&self) -> Vec<Part> {
        let updates = &self.updates;
        let pending = &updates.snapshot.updates;
        let mut parts = vec![Part::Header];
        if updates.busy() {
            parts.push(Part::Progress);
        }
        let group = |label: &'static str, warning: bool, wanted: &dyn Fn(&apt::Update) -> bool| {
            let matching: Vec<&apt::Update> = pending.iter().filter(|update| wanted(update)).collect();
            (!matching.is_empty()).then(|| Part::Group {
                label,
                warning,
                names: names(&matching),
                count: matching.len(),
            })
        };
        parts.extend(group("Security", true, &|update| update.security));
        parts.extend(group("Kernel", false, &|update| !update.security && update.kind == apt::Kind::Kernel));
        parts.extend(group("Desktop", false, &|update| !update.security && update.kind == apt::Kind::Desktop));
        parts.extend(group("Software", false, &|update| !update.security && update.kind == apt::Kind::System));
        if let Some(error) = &updates.snapshot.error {
            parts.push(Part::Note { text: error.clone(), error: true });
        } else if pending.is_empty() && !updates.busy() {
            let text = if updates.scanned { "Everything is up to date." } else { "Looking for updates..." };
            parts.push(Part::Note { text: text.into(), error: false });
        }
        match updates.outcome() {
            Some(Err(reason)) => parts.push(Part::Note { text: reason.clone(), error: true }),
            Some(Ok(message)) if pending.is_empty() => parts.push(Part::Note { text: message.clone(), error: false }),
            _ => {}
        }
        let mut offers = Vec::new();
        if updates.snapshot.restart {
            offers.push(Part::Offer {
                title: "Restart to finish".into(),
                detail: "Earlier updates are waiting for it".into(),
                button: "Restart",
                hit: Hit::Restart,
            });
        }
        if let Some(summary) = updates.cleanup_summary() {
            offers.push(Part::Offer {
                title: summary,
                detail: "Keeps the running kernel and a fallback".into(),
                button: "Clean up",
                hit: Hit::Clean,
            });
        }
        if let Some(name) = updates.release() {
            let system = &updates.snapshot.system;
            let brand = system.pretty.split_whitespace().next().unwrap_or("Release");
            offers.push(Part::Offer {
                title: format!("{brand} {name}"),
                detail: format!("You're on {}", system.pretty),
                button: "Details",
                hit: Hit::Release,
            });
        }
        if !offers.is_empty() {
            parts.push(Part::Rule);
            parts.extend(offers);
        }
        parts.push(Part::Rule);
        if updates.busy() {
            parts.push(Part::Buttons(vec![("Open Updates", Hit::Review)]));
        } else if pending.is_empty() {
            parts.push(Part::Buttons(vec![("Check now", Hit::Check), ("Open Updates", Hit::Review)]));
        } else {
            let totals = updates.snapshot.totals();
            let mut buttons = vec![("Install all", Hit::Install)];
            if totals.security > 0 && totals.security < totals.count {
                buttons.push(("Security only", Hit::Security));
            }
            buttons.push(("Review", Hit::Review));
            parts.push(Part::Buttons(buttons));
            if totals.bytes > 0 {
                parts.push(Part::Footer(format!("{} download", backend::human_size(totals.bytes))));
            }
        }
        parts
    }

    /// The card at its full size, and each part's place in it.
    fn updates_popover_layout(&self) -> (Rect, Vec<(Rect, Part)>) {
        let parts = self.updates_popover_parts();
        let height = parts.iter().map(Part::height).sum::<f64>() + 16.0;
        let card = self.bar_card(self.updates_chip_rect(), WIDTH, height);
        let mut top = card.y + 8.0;
        let placed = parts
            .into_iter()
            .map(|part| {
                let rect = Rect::new(card.x + PAD, top, card.w - 2.0 * PAD, part.height());
                top += rect.h;
                (rect, part)
            })
            .collect();
        (card, placed)
    }

    fn button_rects(row: Rect, count: usize) -> Vec<Rect> {
        let gap = 8.0;
        let width = (row.w - gap * (count.saturating_sub(1)) as f64) / count.max(1) as f64;
        (0..count).map(|index| Rect::new(row.x + index as f64 * (width + gap), row.y + 6.0, width, 30.0)).collect()
    }

    fn offer_button(row: Rect) -> Rect {
        Rect::new(row.right() - 84.0, row.y + 8.0, 84.0, 28.0)
    }

    fn updates_popover_controls(&self) -> Vec<(Rect, Hit)> {
        let (_, parts) = self.updates_popover_layout();
        let mut controls = Vec::new();
        for (rect, part) in &parts {
            match part {
                Part::Header if !self.updates.busy() => {
                    controls.push((Rect::new(rect.right() - 28.0, rect.y + 8.0, 28.0, 28.0), Hit::Check));
                }
                Part::Offer { hit, .. } => controls.push((Self::offer_button(*rect), *hit)),
                Part::Buttons(buttons) => {
                    let rects = Self::button_rects(*rect, buttons.len());
                    controls.extend(rects.into_iter().zip(buttons.iter().map(|(_, hit)| *hit)));
                }
                _ => {}
            }
        }
        controls
    }

    pub(crate) fn updates_popover_contains(&self, point: Point) -> bool {
        self.updates_popover_layout().0.contains(point)
    }

    pub(crate) fn updates_popover_target(&self, point: Point) -> Option<PointerTarget> {
        self.updates_popover_controls()
            .into_iter()
            .find(|(rect, _)| rect.contains(point))
            .map(|(rect, hit)| PointerTarget::button(rect, hit))
    }

    pub(crate) fn draw_updates_popover(&self, painter: &mut Painter, level: f64) {
        let theme = &self.theme;
        let updates = &self.updates;
        let chip = self.updates_chip_rect();
        let (card, parts) = self.updates_popover_layout();
        let rect = chip.zoom_lerp(&card, level);
        let (alpha, offset) = (painter.alpha, painter.offset);
        painter.popup(rect);
        painter.alpha = alpha * crate::bar::card_contents(level);
        let (from, target) = (card.center(), rect.center());
        painter.offset = (offset.0 + target.x - from.x, offset.1 + target.y - from.y);
        if painter.alpha <= 0.0 {
            (painter.alpha, painter.offset) = (alpha, offset);
            return;
        }
        let pointer = Point::new(self.pointer.x, self.pointer.y);
        let small = Style::new(theme.font_size * 0.84, theme.muted);
        let body = Style::new(theme.font_size * 0.92, theme.text);
        let button = |painter: &mut Painter, rect: Rect, label: &str, primary: bool, warning: bool| {
            let look = match (primary, warning) {
                (true, _) => Look::Primary,
                (false, true) => Look::Warning,
                (false, false) => Look::Plain,
            };
            painter.button(rect, Button::new(label, look), rect.contains(pointer));
        };
        for (rect, part) in &parts {
            let rect = *rect;
            match part {
                Part::Header => {
                    let totals = updates.snapshot.totals();
                    let title = match totals.count {
                        0 => "Updates".to_string(),
                        1 => "1 update".to_string(),
                        count => format!("{count} updates"),
                    };
                    let (width, _) = painter.text(
                        &title,
                        Rect::new(rect.x, rect.y, rect.w - 36.0, rect.h),
                        Style::new(theme.font_size * 1.2, theme.text).bold(),
                    );
                    painter.text(
                        &updates.checked(),
                        Rect::new(rect.x + width + 10.0, rect.y + 2.0, rect.w - width - 46.0, rect.h),
                        small,
                    );
                    if !updates.busy() {
                        let refresh = Rect::new(rect.right() - 28.0, rect.y + 8.0, 28.0, 28.0);
                        if refresh.contains(pointer) {
                            painter.fill(refresh, theme.surface_raised, theme.radius);
                        }
                        painter.glyph(glyph::RESTART, refresh, 15, theme.text.alpha(0.86));
                    }
                }
                Part::Group { label, warning, names, count } => {
                    let tint = if *warning { theme.warning } else { theme.muted };
                    let badge = Rect::new(rect.x, rect.y + 5.0, 66.0, 18.0);
                    painter.shape(
                        badge,
                        Shape { border_width: 1.0, border: tint.alpha(0.7), ..Shape::filled(tint.alpha(0.1), 4.0) },
                    );
                    painter.text(
                        &label.to_uppercase(),
                        badge,
                        Style::new(theme.font_size * 0.66, tint).bold().align(Align::Centre),
                    );
                    painter.text(names, Rect::new(rect.x + 76.0, rect.y, rect.w - 110.0, rect.h), small);
                    painter.text(&count.to_string(), Rect::new(rect.right() - 30.0, rect.y, 30.0, rect.h), body.bold());
                }
                Part::Rule => painter.fill(Rect::new(rect.x, rect.y + 6.0, rect.w, 1.0), theme.border, 0.0),
                Part::Offer { title, detail, button: label, .. } => {
                    painter.text(title, Rect::new(rect.x, rect.y + 4.0, rect.w - 96.0, 20.0), body);
                    painter.text(detail, Rect::new(rect.x, rect.y + 22.0, rect.w - 96.0, 18.0), small);
                    button(painter, Self::offer_button(rect), label, false, false);
                }
                Part::Progress => {
                    let (title, label, fraction) = updates.progress().unwrap_or(("", "", 0.0));
                    painter.text(title, Rect::new(rect.x, rect.y, rect.w, 20.0), body.bold());
                    painter.text(label, Rect::new(rect.x, rect.y + 18.0, rect.w, 18.0), small);
                    let track = Rect::new(rect.x, rect.y + 40.0, rect.w, 4.0);
                    painter.fill(track, theme.border, 2.0);
                    let filled = Rect::new(track.x, track.y, (track.w * fraction.max(0.04)).min(track.w), track.h);
                    painter.shape(
                        filled,
                        Shape { fill: (theme.accent, theme.accent_end), ..Shape::filled(theme.accent, 2.0) },
                    );
                }
                Part::Note { text, error } => {
                    painter.text(text, rect, small.colour(if *error { theme.danger } else { theme.muted }));
                }
                Part::Buttons(buttons) => {
                    let rects = Self::button_rects(rect, buttons.len());
                    for (place, (label, hit)) in rects.into_iter().zip(buttons) {
                        button(painter, place, label, *hit == Hit::Install, *hit == Hit::Security);
                    }
                }
                Part::Footer(text) => {
                    painter.text(text, rect, small.colour(theme.muted.alpha(0.8)).align(Align::Centre));
                }
            }
        }
        (painter.alpha, painter.offset) = (alpha, offset);
    }
}
