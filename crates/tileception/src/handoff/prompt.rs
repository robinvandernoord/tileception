//! The card that asks where another machine is: drag it to that edge of a
//! screen. It also waits for the other user, and offers to resume after a
//! lock.

use std::collections::BTreeSet;

use smithay::{
    backend::renderer::gles::GlesRenderer,
    utils::{Logical, Point},
};
use tileception_core::Rect;
use tileception_handoff::PeerId;

use super::{Edge, Link, Pause, Side};
use crate::{
    Tileception,
    render::OutputElement,
    shapes::Shape,
    ui::{Painter, Style, glyph},
};

const BTN_LEFT: u32 = 0x110;
const WIDTH: f64 = 400.0;
const HEIGHT: f64 = 104.0;
/// How close to an edge a dragged card has to be to land there.
const REACH: f64 = 180.0;

#[derive(Default)]
pub struct Prompt {
    /// Machines this user said no to, until retried.
    pub dismissed: BTreeSet<PeerId>,
    /// While dragging: where in the card it was picked up.
    drag: Option<(f64, f64)>,
    /// The edge the dragged card would land on.
    target: Option<Side>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Card {
    /// Drag me to the edge where that machine is.
    Place,
    /// Placed; the other user has yet to.
    Waiting,
    /// Paused by this machine's lock.
    Paused,
}

struct Layout {
    card: Rect,
    close: Rect,
    resume: Rect,
}

impl Tileception {
    fn handoff_card(&self) -> Option<(PeerId, Card)> {
        if self.is_locked() {
            return None;
        }
        let peers = &self.handoff.peers;
        let find = |wanted: Card| {
            peers.iter().find_map(|(peer, entry)| {
                let card = match &entry.link {
                    Link::Asking { ours: None, .. } => Card::Place,
                    Link::Asking { ours: Some(_), .. } => Card::Waiting,
                    Link::Linked { paused: Some(Pause::Ours), .. } => Card::Paused,
                    _ => return None,
                };
                (card == wanted).then_some((*peer, card))
            })
        };
        find(Card::Place).or_else(|| find(Card::Paused)).or_else(|| find(Card::Waiting))
    }

    fn handoff_layout(&self) -> Layout {
        let screen = self.output_rect();
        let width = WIDTH.min(screen.w - 24.0);
        let card = match self.handoff.prompt.drag {
            Some((left, top)) => Rect::new(self.pointer.x - left, self.pointer.y - top, width, HEIGHT),
            None => Rect::new(screen.x + (screen.w - width) / 2.0, screen.y + screen.h * 0.16, width, HEIGHT),
        };
        Layout {
            close: Rect::new(card.right() - 38.0, card.y + 10.0, 28.0, 28.0),
            resume: Rect::new(card.right() - 112.0, card.bottom() - 42.0, 96.0, 30.0),
            card,
        }
    }

    /// The free edge of the active screen nearest a point, if it is close.
    fn handoff_drop_side(&self, point: Point<f64, Logical>) -> Option<Side> {
        let screen = self.output_rect();
        let output = self.output.as_ref()?.name();
        [
            (Side::Left, point.x - screen.x),
            (Side::Right, screen.right() - point.x),
            (Side::Top, point.y - screen.y),
            (Side::Bottom, screen.bottom() - point.y),
        ]
        .into_iter()
        .filter(|(side, distance)| {
            *distance < REACH && self.outer_edge(&output, *side) && !self.handoff.edge_taken(&output, *side)
        })
        .min_by(|(_, first), (_, second)| first.total_cmp(second))
        .map(|(side, _)| side)
    }

    /// Pointer motion over or dragging the card; true when it takes it.
    pub(crate) fn handoff_prompt_motion(&mut self, point: Point<f64, Logical>) -> bool {
        if self.handoff_card().is_none() {
            self.handoff.prompt.drag = None;
            return false;
        }
        if self.handoff.prompt.drag.is_some() {
            self.handoff.prompt.target = self.handoff_drop_side(point);
            self.schedule_render();
            return true;
        }
        self.handoff_layout().card.contains(tileception_core::Point::new(point.x, point.y))
    }

    /// A button over or dragging the card; true when it takes it.
    pub(crate) fn handoff_prompt_button(&mut self, button: u32, pressed: bool) -> bool {
        let Some((peer, card)) = self.handoff_card() else {
            self.handoff.prompt.drag = None;
            return false;
        };
        if !pressed {
            if self.handoff.prompt.drag.take().is_none() {
                return false;
            }
            if let (Some(side), Some(output)) = (self.handoff.prompt.target.take(), self.output.as_ref()) {
                let edge = Edge { output: output.name(), side };
                if let Err(error) = self.place_peer(peer, edge) {
                    tracing::warn!("handoff: {error}");
                }
            }
            self.schedule_render();
            return true;
        }
        let layout = self.handoff_layout();
        let point = tileception_core::Point::new(self.pointer.x, self.pointer.y);
        if !layout.card.contains(point) {
            return false;
        }
        if button != BTN_LEFT {
            return true;
        }
        if layout.close.contains(point) {
            self.dismiss_peer(peer);
        } else if card == Card::Paused && layout.resume.contains(point) {
            if let Err(error) = self.handoff_resume() {
                tracing::warn!("handoff: {error}");
            }
        } else if card == Card::Place {
            self.handoff.prompt.drag = Some((point.x - layout.card.x, point.y - layout.card.y));
            self.handoff.prompt.target = None;
        }
        self.schedule_render();
        true
    }

    pub fn handoff_elements(&self, renderer: &mut GlesRenderer) -> Vec<OutputElement> {
        let Some((peer, card)) = self.handoff_card() else { return Vec::new() };
        let Some(entry) = self.handoff.peers.get(&peer) else { return Vec::new() };
        let theme = &self.theme;
        let layout = self.handoff_layout();
        let point = tileception_core::Point::new(self.pointer.x, self.pointer.y);
        let mut painter = Painter::new(self, renderer);

        if let Some(side) = self.handoff.prompt.target.filter(|_| self.handoff.prompt.drag.is_some()) {
            let screen = self.output_rect();
            let thickness = 6.0;
            let bar = match side {
                Side::Left => Rect::new(screen.x, screen.y, thickness, screen.h),
                Side::Right => Rect::new(screen.right() - thickness, screen.y, thickness, screen.h),
                Side::Top => Rect::new(screen.x, screen.y, screen.w, thickness),
                Side::Bottom => Rect::new(screen.x, screen.bottom() - thickness, screen.w, thickness),
            };
            painter.shape(
                bar,
                Shape { glow_radius: 24.0, glow: theme.accent.alpha(0.5), ..Shape::filled(theme.accent, 3.0) },
            );
        }

        painter.popup(layout.card);
        let icon = Rect::new(layout.card.x + 16.0, layout.card.y + 16.0, 40.0, 40.0);
        painter.fill(icon, theme.surface_raised, 20.0);
        let symbol = match card {
            Card::Place => glyph::ARROW_RIGHT,
            Card::Waiting => glyph::CLOCK,
            Card::Paused => glyph::LOCK,
        };
        painter.glyph(symbol, icon, 18, theme.accent);
        let text_x = icon.right() + 14.0;
        let text_w = layout.close.x - 8.0 - text_x;
        let name = &entry.name;
        let (title, body) = match card {
            Card::Place => {
                (format!("Share with {name}"), format!("Drag this card to the edge of the screen where {name} is."))
            }
            Card::Waiting => {
                (format!("Waiting for {name}"), format!("Sharing starts once {name} has placed this machine."))
            }
            Card::Paused => (format!("Sharing with {name} is paused"), "Paused when this machine locked.".to_string()),
        };
        painter.text(
            &title,
            Rect::new(text_x, layout.card.y + 16.0, text_w, 22.0),
            Style::new(theme.font_size, theme.text).bold(),
        );
        let body_w =
            if card == Card::Paused { layout.resume.x - 8.0 - text_x } else { layout.card.right() - 16.0 - text_x };
        painter.text(
            &body,
            Rect::new(text_x, layout.card.y + 40.0, body_w, 44.0),
            Style::new(theme.font_size * 0.92, theme.muted).lines(2),
        );
        let on_close = layout.close.contains(point);
        if on_close {
            painter.fill(layout.close, theme.surface_raised, 14.0);
        }
        painter.glyph(glyph::CLOSE, layout.close, 13, if on_close { theme.text } else { theme.muted });
        if card == Card::Paused {
            let hover = layout.resume.contains(point);
            painter.fill(layout.resume, if hover { theme.accent_end } else { theme.accent }, 8.0);
            painter.text(
                "Resume",
                layout.resume,
                Style::new(theme.font_size, theme.on_accent).bold().align(crate::ui::Align::Centre),
            );
        }
        painter.finish()
    }
}
