//! Where the panels go: columns of rows, like tiles on the desktop, with the
//! desktop's gaps. On a portrait screen the columns become rows.

use tileception_core::{Dir, Rect};

use super::settings::{Panel, Panels};

/// How tall a panel is against the others in its column.
fn weight(panel: Panel) -> f64 {
    match panel {
        Panel::Login => 1.0,
        Panel::Clock => 0.8,
        Panel::Session => 1.0,
    }
}

/// Each panel's rectangle inside `area`, `gap` pixels apart and from the edges.
pub fn place(panels: &Panels, area: Rect, gap: f64) -> Vec<(Panel, Rect)> {
    let columns: Vec<&Vec<Panel>> = panels.columns.iter().filter(|c| !c.is_empty()).collect();
    if columns.is_empty() {
        return Vec::new();
    }
    let inner = area.inset(gap);
    // Columns run along the longer side.
    let across = inner.w >= inner.h;
    let ratio = |i: usize| panels.ratios.get(i).copied().filter(|r| *r > 0.0 && r.is_finite()).unwrap_or(1.0);
    let total: f64 = (0..columns.len()).map(ratio).sum();
    let length = if across { inner.w } else { inner.h };
    let free = (length - gap * (columns.len() - 1) as f64).max(0.0);
    let mut out = Vec::new();
    let mut at = 0.0;
    for (i, column) in columns.iter().enumerate() {
        let size = free * ratio(i) / total;
        let strip = if across {
            Rect::new(inner.x + at, inner.y, size, inner.h)
        } else {
            Rect::new(inner.x, inner.y + at, inner.w, size)
        };
        at += size + gap;
        let weights: f64 = column.iter().map(|p| weight(*p)).sum();
        let room = ((if across { strip.h } else { strip.w }) - gap * (column.len() - 1) as f64).max(0.0);
        let mut along = 0.0;
        for panel in column.iter() {
            let size = room * weight(*panel) / weights;
            let rect = if across {
                Rect::new(strip.x, strip.y + along, strip.w, size)
            } else {
                Rect::new(strip.x + along, strip.y, size, strip.h)
            };
            along += size + gap;
            out.push((*panel, rect));
        }
    }
    out
}

/// The panel next to `from` in a direction: the nearest one whose centre
/// lies that way and that overlaps it across the direction.
pub fn neighbour(placed: &[(Panel, Rect)], from: Panel, dir: Dir) -> Option<Panel> {
    let (_, start) = placed.iter().find(|(p, _)| *p == from)?;
    let c = start.center();
    let ahead = |r: &Rect| {
        let d = r.center();
        match dir {
            Dir::Left => d.x < c.x - 1.0,
            Dir::Right => d.x > c.x + 1.0,
            Dir::Up => d.y < c.y - 1.0,
            Dir::Down => d.y > c.y + 1.0,
        }
    };
    let overlaps = |r: &Rect| match dir {
        Dir::Left | Dir::Right => r.y < start.bottom() && r.bottom() > start.y,
        Dir::Up | Dir::Down => r.x < start.right() && r.right() > start.x,
    };
    let distance = |r: &Rect| (r.center().x - c.x).hypot(r.center().y - c.y);
    placed
        .iter()
        .filter(|(p, r)| *p != from && ahead(r) && overlaps(r))
        .min_by(|a, b| distance(&a.1).total_cmp(&distance(&b.1)))
        .map(|(p, _)| *p)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(placed: &[(Panel, Rect)], panel: Panel) -> Rect {
        placed.iter().find(|(p, _)| *p == panel).map(|(_, r)| *r).expect("placed")
    }

    #[test]
    fn landscape_has_columns() {
        let placed = place(&Panels::default(), Rect::new(0.0, 0.0, 1100.0, 600.0), 12.0);
        let (login, clock, session) =
            (rect(&placed, Panel::Login), rect(&placed, Panel::Clock), rect(&placed, Panel::Session));
        assert_eq!((login.x, login.y), (12.0, 12.0));
        assert!((login.w / clock.w - 1.75).abs() < 1e-9);
        assert_eq!(login.h, 600.0 - 24.0);
        assert_eq!(clock.x, session.x);
        assert!((session.y - clock.bottom() - 12.0).abs() < 1e-9);
        assert!((session.bottom() - 588.0).abs() < 1e-9);
        assert!((clock.right() - 1088.0).abs() < 1e-9);
    }

    #[test]
    fn portrait_stacks() {
        let placed = place(&Panels::default(), Rect::new(0.0, 0.0, 600.0, 1100.0), 12.0);
        let (login, clock) = (rect(&placed, Panel::Login), rect(&placed, Panel::Clock));
        assert_eq!(login.w, 600.0 - 24.0);
        assert!(clock.y > login.bottom());
    }

    #[test]
    fn arrows_find_neighbours() {
        let placed = place(&Panels::default(), Rect::new(0.0, 0.0, 1100.0, 600.0), 12.0);
        assert_eq!(neighbour(&placed, Panel::Login, Dir::Right), Some(Panel::Session));
        assert_eq!(neighbour(&placed, Panel::Clock, Dir::Down), Some(Panel::Session));
        assert_eq!(neighbour(&placed, Panel::Session, Dir::Up), Some(Panel::Clock));
        assert_eq!(neighbour(&placed, Panel::Session, Dir::Left), Some(Panel::Login));
        assert_eq!(neighbour(&placed, Panel::Login, Dir::Left), None);
        assert_eq!(neighbour(&placed, Panel::Login, Dir::Up), None);
    }
}
