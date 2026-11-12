//! Where along a shared edge the pointer crosses, between screens that need
//! not match.
//!
//! Each machine shares the part of its edge that no other of its screens
//! lies beyond: all of it for a lone screen, less beside a taller or wider
//! neighbour. The machines tell each other how long their parts are. As
//! between screens of one machine, the parts meet centred, pixel for pixel,
//! and the pointer crosses only where they face each other; with
//! `stretch_screen_edges` each spans the whole of the other. The machine the
//! pointer leaves maps it, so the fraction in `Enter` or `Return` is already
//! one of the receiver's part.

use std::collections::BTreeMap;

use tileception_core::Rect;
use tileception_handoff::{Message, PeerId};

use super::{Link, Side, output_bounds};
use crate::Tileception;

/// Shorter free parts are slivers left by rounding, not room to cross.
const MIN_SPAN: f64 = 2.0;

/// The part of an edge where the pointer crosses, in its screen's logical
/// pixels along the edge from the screen's top or left.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Span {
    pub start: f64,
    pub length: f64,
}

impl Span {
    /// How far into the span a point along the edge is, if it is in it.
    pub(crate) fn offset(self, along: f64) -> Option<f64> {
        let offset = along - self.start;
        (0.0..=self.length).contains(&offset).then_some(offset)
    }

    /// The point along the edge at a fraction of the span, kept a pixel
    /// short of its end so it stays on the screen.
    pub(crate) fn point(self, fraction: f64) -> f64 {
        self.start + (fraction.clamp(0.0, 1.0) * self.length).min((self.length - 1.0).max(0.0))
    }
}

/// What each machine knows of the other's edge.
#[derive(Default)]
pub(crate) struct Edges {
    /// How long each machine's part is, as it last said.
    theirs: BTreeMap<PeerId, f64>,
    /// What each machine was last told about this one's part.
    told: BTreeMap<PeerId, f64>,
}

impl Edges {
    /// The next `share_edges` tells `peer` again.
    pub(crate) fn retell(&mut self, peer: PeerId) {
        self.told.remove(&peer);
    }

    pub(crate) fn forget(&mut self, peer: PeerId) {
        self.theirs.remove(&peer);
        self.told.remove(&peer);
    }
}

/// The free part of a screen's edge: the longest stretch that no other
/// screen of the machine lies beyond. All rectangles are in the machine's
/// global logical pixels; the span is along `screen`'s edge.
pub(crate) fn free_span(screen: Rect, side: Side, others: &[Rect]) -> Option<Span> {
    // The edge from `low` to `high` along it, and the line just beyond it.
    let (low, high) = match side {
        Side::Left | Side::Right => (screen.y, screen.bottom()),
        Side::Top | Side::Bottom => (screen.x, screen.right()),
    };
    let mut blocked: Vec<(f64, f64)> = others
        .iter()
        .filter(|other| match side {
            Side::Left => other.x < screen.x && other.right() >= screen.x - 1.0,
            Side::Right => other.right() > screen.right() && other.x <= screen.right() + 1.0,
            Side::Top => other.y < screen.y && other.bottom() >= screen.y - 1.0,
            Side::Bottom => other.bottom() > screen.bottom() && other.y <= screen.bottom() + 1.0,
        })
        .map(|other| match side {
            Side::Left | Side::Right => (other.y.max(low), other.bottom().min(high)),
            Side::Top | Side::Bottom => (other.x.max(low), other.right().min(high)),
        })
        .filter(|(from, until)| until > from)
        .collect();
    blocked.sort_by(|first, second| first.0.total_cmp(&second.0));
    let mut best: Option<(f64, f64)> = None;
    let mut free_from = low;
    for (from, until) in blocked.into_iter().chain(std::iter::once((high, high))) {
        if from - free_from > best.map_or(0.0, |(start, end)| end - start) {
            best = Some((free_from, from));
        }
        free_from = free_from.max(until);
    }
    best.filter(|(start, end)| end - start >= MIN_SPAN)
        .map(|(start, end)| Span { start: start - low, length: end - start })
}

/// Where the pointer arrives on the other machine, as a fraction of its
/// part, when it leaves `offset` pixels into a part `from` long here. The
/// other part is `theirs` long, if that machine said; `None` when nothing
/// faces that point.
pub(crate) fn across(offset: f64, from: f64, theirs: Option<f64>, stretch: bool) -> Option<f64> {
    let fraction = (offset / from.max(1.0)).clamp(0.0, 1.0);
    let theirs = match theirs {
        // A machine that has not said yet is met whole.
        None => return Some(fraction),
        Some(length) if length < MIN_SPAN => return None,
        Some(length) => length,
    };
    if stretch {
        return Some(fraction);
    }
    let there = offset - (from - theirs) / 2.0;
    (0.0..=theirs).contains(&there).then(|| there / theirs)
}

impl Tileception {
    /// The free part of an edge of one of this machine's screens.
    pub(crate) fn edge_span(&self, output: &str, side: Side) -> Option<Span> {
        let target = self.output_named(output)?;
        let screen = output_bounds(&target)?;
        let others: Vec<Rect> =
            self.all_outputs().iter().filter(|other| **other != target).filter_map(output_bounds).collect();
        free_span(screen, side, &others)
    }

    /// Where the pointer arrives on `peer` when it leaves here at `along`
    /// (logical pixels along the edge of the active screen), as a fraction
    /// of that machine's part; `None` when it may not cross there.
    pub(crate) fn cross_at(&self, peer: PeerId, output: &str, side: Side, along: f64) -> Option<f64> {
        let span = self.edge_span(output, side)?;
        let offset = span.offset(along)?;
        let theirs = self.handoff.edges.theirs.get(&peer).copied();
        across(offset, span.length, theirs, self.config.file.general.stretch_screen_edges)
    }

    /// What another machine says about its part of the edge.
    pub(crate) fn peer_span(&mut self, peer: PeerId, length: f64) {
        let length = if length.is_finite() { length.clamp(0.0, 1_000_000.0) } else { 0.0 };
        self.handoff.edges.theirs.insert(peer, length);
    }

    /// Tells linked machines how long this machine's part of their edge is,
    /// when that changed: after linking and whenever screens change.
    pub(crate) fn share_edges(&mut self) {
        let linked: Vec<(PeerId, super::Edge)> = self
            .handoff
            .peers
            .iter()
            .filter_map(|(peer, entry)| match &entry.link {
                Link::Linked { edge, .. } => Some((*peer, edge.clone())),
                _ => None,
            })
            .collect();
        for (peer, edge) in linked {
            let length = self.edge_span(&edge.output, edge.side).map_or(0.0, |span| span.length);
            if self.handoff.edges.told.get(&peer) == Some(&length) {
                continue;
            }
            if self.handoff.send(peer, Message::Span { length }) {
                self.handoff.edges.told.insert(peer, length);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(left: f64, top: f64, width: f64, height: f64) -> Rect {
        Rect::new(left, top, width, height)
    }

    #[test]
    fn a_lone_screen_shares_its_whole_edge() {
        let screen = rect(0.0, 0.0, 1920.0, 1080.0);
        for side in [Side::Left, Side::Right, Side::Top, Side::Bottom] {
            let length = if matches!(side, Side::Left | Side::Right) { 1080.0 } else { 1920.0 };
            assert_eq!(free_span(screen, side, &[]), Some(Span { start: 0.0, length }));
        }
    }

    #[test]
    fn a_shorter_neighbour_leaves_the_rest_of_the_edge_free() {
        // A 1440 tall screen with a 1080 tall one to its right, tops level.
        let tall = rect(0.0, 0.0, 2560.0, 1440.0);
        let short = rect(2560.0, 0.0, 1920.0, 1080.0);
        assert_eq!(free_span(tall, Side::Right, &[short]), Some(Span { start: 1080.0, length: 360.0 }));
        // The shorter one's edge towards the taller is covered whole.
        assert_eq!(free_span(short, Side::Left, &[tall]), None);
        // Edges facing away are free.
        assert_eq!(free_span(tall, Side::Left, &[short]), Some(Span { start: 0.0, length: 1440.0 }));
        assert_eq!(free_span(short, Side::Right, &[tall]), Some(Span { start: 0.0, length: 1080.0 }));
    }

    #[test]
    fn the_longest_free_stretch_is_shared() {
        // A neighbour in the middle leaves two parts; the lower is longer.
        let screen = rect(0.0, 0.0, 1000.0, 1000.0);
        let middle = rect(1000.0, 300.0, 500.0, 300.0);
        assert_eq!(free_span(screen, Side::Right, &[middle]), Some(Span { start: 600.0, length: 400.0 }));
        // Neighbours that touch only at a corner or lie elsewhere do not count.
        let corner = rect(1000.0, 1000.0, 500.0, 500.0);
        let below = rect(0.0, 1000.0, 1000.0, 500.0);
        assert_eq!(free_span(screen, Side::Right, &[corner, below]), Some(Span { start: 0.0, length: 1000.0 }));
        // Slivers left by rounding are not room to cross.
        let almost = rect(1000.0, 0.0, 500.0, 999.5);
        assert_eq!(free_span(screen, Side::Right, &[almost]), None);
    }

    #[test]
    fn spans_measure_from_the_screens_own_corner() {
        let screen = rect(-1280.0, 200.0, 1280.0, 800.0);
        let above = rect(-1280.0, -880.0, 640.0, 1080.0);
        assert_eq!(free_span(screen, Side::Top, &[above]), Some(Span { start: 640.0, length: 640.0 }));
        let span = Span { start: 640.0, length: 640.0 };
        assert_eq!(span.offset(700.0), Some(60.0));
        assert_eq!(span.offset(100.0), None);
        assert_eq!(span.point(0.0), 640.0);
        assert_eq!(span.point(1.0), 1279.0);
    }

    #[test]
    fn edges_meet_centred_pixel_for_pixel() {
        // From a 1440 tall edge to a 1080 tall one: the shorter faces the
        // middle of the taller.
        assert_eq!(across(720.0, 1440.0, Some(1080.0), false), Some(0.5));
        assert_eq!(across(180.0, 1440.0, Some(1080.0), false), Some(0.0));
        assert_eq!(across(1260.0, 1440.0, Some(1080.0), false), Some(1.0));
        // Above and below the shorter one nothing faces the pointer.
        assert_eq!(across(100.0, 1440.0, Some(1080.0), false), None);
        assert_eq!(across(1400.0, 1440.0, Some(1080.0), false), None);
        // From the shorter, every point has somewhere to go.
        assert_eq!(across(0.0, 1080.0, Some(1440.0), false), Some(180.0 / 1440.0));
        assert_eq!(across(1080.0, 1080.0, Some(1440.0), false), Some(1260.0 / 1440.0));
        // Equal edges map straight across.
        assert_eq!(across(270.0, 1080.0, Some(1080.0), false), Some(0.25));
    }

    #[test]
    fn stretched_edges_map_whole_onto_whole() {
        assert_eq!(across(100.0, 1440.0, Some(1080.0), true), Some(100.0 / 1440.0));
        assert_eq!(across(1440.0, 1440.0, Some(1080.0), true), Some(1.0));
    }

    #[test]
    fn an_unknown_part_is_met_whole_and_a_missing_one_not_at_all() {
        assert_eq!(across(540.0, 1080.0, None, false), Some(0.5));
        assert_eq!(across(540.0, 1080.0, Some(0.0), false), None);
        assert_eq!(across(540.0, 1080.0, Some(0.0), true), None);
    }
}
