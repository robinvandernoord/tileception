//! The minimap: the whole tree drawn small, with the camera's frame on it.
//! A chip in the bar shows it at bar height and zooms open into a larger
//! card while the pointer is on it (a click keeps it open), where a click
//! flies to a window and a drag pans to another space at the depth framed
//! now. Tiles are flat rectangles, not window pictures, so it
//! stays readable at any size.

use tileception_core::{Node, NodeId, Point, Rect, World};

use crate::{
    Tileception,
    bar::{BAR_H, Zoom},
    shapes::Shape,
    ui::{Painter, Style, ease_out},
};

/// Padding around the map inside the card.
const PAD: f64 = 10.0;
/// The line under the map that says what a click would do.
const LABEL_H: f64 = 26.0;
/// Pixels the pointer moves before a press on the map becomes a drag.
const DRAG: f64 = 4.0;
/// Space between the areas of a split screen on the card, and on the chip.
const PART_GAP: f64 = 6.0;
const CHIP_PART_GAP: f64 = 2.0;

#[derive(Default)]
pub struct Minimap {
    pub(crate) zoom: Zoom,
    /// Where the left button went down on the map.
    press: Option<Point>,
    /// Opened by a click or the command, so it stays when the pointer leaves.
    pinned: bool,
    /// Closed while the pointer may still be on the chip: hovering opens it
    /// again only after the pointer has left.
    rearm: bool,
}

/// What the pointer does on the open card, in which area of the screen.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Gesture {
    /// Over a leaf, nothing pressed.
    Hover(usize, NodeId),
    /// Pressed on a leaf, not moved yet: letting go flies there.
    Click(usize, NodeId),
    /// Dragged: letting go frames this space.
    Pan(usize, NodeId),
}

impl Gesture {
    fn area(self) -> usize {
        match self {
            Gesture::Hover(area, _) | Gesture::Click(area, _) | Gesture::Pan(area, _) => area,
        }
    }
}

/// A rectangle in layout units, placed on `area`, which shows the unit square.
fn to_area(area: Rect, unit: Rect) -> Rect {
    Rect::new(area.x + unit.x * area.w, area.y + unit.y * area.h, unit.w * area.w, unit.h * area.h)
}

/// A point on `area` in layout units.
fn to_unit(area: Rect, point: Point) -> Point {
    Point::new((point.x - area.x) / area.w.max(1.0), (point.y - area.y) / area.h.max(1.0))
}

/// The part of `rect` inside `bounds`, if any.
fn clip(rect: Rect, bounds: Rect) -> Option<Rect> {
    let (left, top) = (rect.x.max(bounds.x), rect.y.max(bounds.y));
    let (right, bottom) = (rect.right().min(bounds.right()), rect.bottom().min(bounds.bottom()));
    (right > left && bottom > top).then(|| Rect::new(left, top, right - left, bottom - top))
}

fn dragged(start: Point, end: Point) -> bool {
    (end.x - start.x).hypot(end.y - start.y) >= DRAG
}

impl Tileception {
    /// The shape of the layout on screen: the unit square stretched to it,
    /// or on a split screen all areas side by side.
    fn minimap_aspect(&self) -> f64 {
        let screen = self.usable_rect();
        (screen.w / screen.h.max(1.0)).clamp(0.5, 4.0)
    }

    /// Each area's world and its part of `area`, left to right, as wide as
    /// the area is on screen.
    fn minimap_parts(&self, area: Rect, gap: f64) -> Vec<(usize, &World, Rect)> {
        let worlds = self.area_worlds();
        let total: f64 = worlds.iter().map(|(_, world)| world.screen().w.max(1.0)).sum();
        let room = (area.w - gap * worlds.len().saturating_sub(1) as f64).max(1.0);
        let mut left = area.x;
        worlds
            .into_iter()
            .map(|(index, world)| {
                let width = room * world.screen().w.max(1.0) / total;
                let part = Rect::new(left, area.y, width, area.h);
                left += width + gap;
                (index, world, part)
            })
            .collect()
    }

    /// The chip's place in the bar from `left`, if the bar shows one.
    pub(crate) fn minimap_chip(&self, left: f64, top: f64, height: f64) -> Option<Rect> {
        if !self.shell().bar_minimap {
            return None;
        }
        let inner = height - 8.0;
        let width = (inner * self.minimap_aspect()).clamp(inner, inner * 4.0);
        Some(Rect::new(left, top, width + 12.0, height))
    }

    /// The `minimap` command, and clicking the chip: opens the card and keeps
    /// it open, or closes it once it was kept open.
    pub fn toggle_minimap(&mut self) {
        if self.bar.minimap.zoom.open && self.bar.minimap.pinned {
            self.close_minimap();
            return;
        }
        if !self.bar_shows() || !self.shell().bar_minimap {
            tracing::info!("the minimap needs the built-in bar and [shell] bar_minimap");
            return;
        }
        self.close_calendar();
        self.close_tray();
        let duration = self.world.settings.animation;
        self.bar.minimap.zoom.set(true, duration);
        self.bar.minimap.pinned = true;
    }

    /// Zooms the card back into the chip. Returns whether it was open.
    pub fn close_minimap(&mut self) -> bool {
        let minimap = &mut self.bar.minimap;
        minimap.press = None;
        minimap.pinned = false;
        minimap.rearm = true;
        let duration = self.world.settings.animation;
        self.bar.minimap.zoom.set(false, duration)
    }

    /// Hovering the chip opens the card; it closes again when the pointer
    /// leaves the chip and the card, unless a click kept it open. `blocked`
    /// says something else has the pointer (a drag, another popover).
    pub(crate) fn update_minimap_hover(&mut self, blocked: bool) {
        let point = Point::new(self.pointer.x, self.pointer.y);
        if !self.bar.minimap.zoom.open && !self.bar_rect().is_some_and(|bar| bar.contains(point)) {
            self.bar.minimap.rearm = false;
            return;
        }
        let chip = self.bar_layout().and_then(|layout| layout.minimap);
        let over_chip = chip.is_some_and(|chip| chip.contains(point));
        if !over_chip {
            self.bar.minimap.rearm = false;
        }
        let minimap = &self.bar.minimap;
        if !minimap.zoom.open {
            if over_chip && !blocked && !minimap.rearm {
                let duration = self.world.settings.animation;
                self.bar.minimap.zoom.set(true, duration);
            }
            return;
        }
        if minimap.pinned || minimap.press.is_some() {
            return;
        }
        let card = self.minimap_card_for(chip);
        // The chip's column down to the card, so the pointer can get there.
        let bridge = chip.map(|chip| {
            let top = chip.y.min(card.y);
            Rect::new(chip.x, top, chip.w, chip.bottom().max(card.bottom()) - top)
        });
        if !over_chip && !card.contains(point) && !bridge.is_some_and(|bridge| bridge.contains(point)) {
            self.close_minimap();
        }
    }

    /// The card at its full size, below the chip (above it for a bottom bar).
    fn minimap_card(&self) -> Rect {
        self.minimap_card_for(self.bar_layout().and_then(|layout| layout.minimap))
    }

    /// `minimap_card` for a chip already placed, saving laying out the bar again.
    fn minimap_card_for(&self, chip: Option<Rect>) -> Rect {
        let out = self.output_rect();
        let width = (out.w * 0.36).clamp(280.0, 640.0).min(out.w - 16.0).max(1.0);
        let map_h = (width - 2.0 * PAD) / self.minimap_aspect();
        let height = map_h + 2.0 * PAD + LABEL_H;
        let chip_x = chip.map_or(out.x + 100.0, |chip| chip.x);
        let left = (chip_x - PAD).min(out.right() - width - 8.0).max(out.x + 8.0);
        let edge = if self.bar_at_bottom() { out.bottom() - BAR_H } else { out.y };
        Rect::new(left, self.bar_popup_y(Rect::new(out.x, edge, out.w, BAR_H), height), width, height)
    }

    /// Whether the open card covers a point, so a press there is the map's.
    pub(crate) fn minimap_covers(&self, point: Point) -> bool {
        self.bar.minimap.zoom.open && self.minimap_card().contains(point)
    }

    /// Where the unit square is drawn on the card.
    fn minimap_area(card: Rect) -> Rect {
        Rect::new(card.x + PAD, card.y + PAD, card.w - 2.0 * PAD, card.h - 2.0 * PAD - LABEL_H)
    }

    /// A press while the card is open: on the map it starts a click or drag;
    /// on the chip it keeps a hovered card open or closes a kept one; outside
    /// the card it zooms the card closed.
    pub(crate) fn minimap_press(&mut self, point: Point) {
        let chip = self.bar_layout().and_then(|layout| layout.minimap);
        let card = self.minimap_card_for(chip);
        if chip.is_some_and(|chip| chip.contains(point)) {
            self.toggle_minimap();
        } else if !card.contains(point) {
            self.close_minimap();
        } else if Self::minimap_area(card).contains(point) {
            self.bar.minimap.press = Some(point);
        }
    }

    /// Letting go after a press on the map: flies there and closes the card.
    pub(crate) fn minimap_release(&mut self, point: Point) {
        let Some((index, target)) =
            self.minimap_gesture(point, self.minimap_card()).and_then(|gesture| match gesture {
                Gesture::Click(index, node) | Gesture::Pan(index, node) => Some((index, node)),
                Gesture::Hover(..) => None,
            })
        else {
            self.bar.minimap.press = None;
            return;
        };
        self.bar.minimap.press = None;
        // Another area of a split screen becomes the active one.
        if index != self.area_index()
            && let Some(output) = self.output.clone()
            && !self.activate_area(&crate::outputs::AreaId { output, index })
        {
            return;
        }
        let now = self.now();
        self.world.frame(target, now);
        self.apply_layout();
        self.close_minimap();
    }

    /// What the pointer at `point` does on the open card, placed at `card`.
    fn minimap_gesture(&self, point: Point, card: Rect) -> Option<Gesture> {
        if !self.bar.minimap.zoom.open {
            return None;
        }
        let parts = self.minimap_parts(Self::minimap_area(card), PART_GAP);
        // A press stays with the area it started in, also while dragging out of it.
        let anchor = self.bar.minimap.press.unwrap_or(point);
        let (index, world, rect) = parts.into_iter().find(|(_, _, rect)| rect.contains(anchor))?;
        let unit = to_unit(rect, point);
        match self.bar.minimap.press {
            Some(press) if dragged(press, point) => world.pan_target(unit).map(|node| Gesture::Pan(index, node)),
            Some(_) => world.leaf_at_unit(unit).map(|node| Gesture::Click(index, node)),
            None => world.leaf_at_unit(unit).map(|node| Gesture::Hover(index, node)),
        }
    }

    pub(crate) fn draw_minimap_chip(&self, painter: &mut Painter, chip: Rect) {
        let area = Rect::new(chip.x + 6.0, chip.y + 4.0, chip.w - 12.0, chip.h - 8.0);
        painter.fill(area, self.theme.background, 2.0);
        self.draw_minimap(painter, area, false, None);
    }

    /// The chip grown into the card, along the same curve the camera uses.
    pub(crate) fn draw_minimap_card(&self, painter: &mut Painter, chip: Rect, level: f64) {
        let theme = &self.theme;
        let card = self.minimap_card_for(Some(chip));
        let rect = chip.zoom_lerp(&card, level);
        let (alpha, offset) = (painter.alpha, painter.offset);
        painter.popup(rect);
        // The map fades in once the card is nearly there, and moves with it.
        painter.alpha = alpha * (ease_out(((level - 0.7) / 0.3).clamp(0.0, 1.0)) as f32);
        let (from, target) = (card.center(), rect.center());
        painter.offset = (offset.0 + target.x - from.x, offset.1 + target.y - from.y);
        if painter.alpha > 0.0 {
            let area = Self::minimap_area(card);
            painter.fill(area.inset(-2.0), theme.background, 4.0);
            let pointer = Point::new(self.pointer.x, self.pointer.y);
            let gesture = self.minimap_gesture(pointer, card);
            self.draw_minimap(painter, area, true, gesture);
            let label = Rect::new(area.x + 2.0, area.bottom() + 4.0, area.w - 4.0, LABEL_H - 4.0);
            let style = Style::new(theme.font_size * 0.8, theme.muted);
            painter.text(&self.minimap_label(gesture), label, style);
        }
        (painter.alpha, painter.offset) = (alpha, offset);
    }

    /// The line under the map: what letting go there would show.
    fn minimap_label(&self, gesture: Option<Gesture>) -> String {
        let worlds = self.area_worlds();
        let world = gesture
            .and_then(|gesture| worlds.iter().find(|(index, _)| *index == gesture.area()))
            .map_or(&self.world, |(_, world)| *world);
        let tree = world.tree();
        let describe = |node: NodeId| -> String {
            let windows = tree.shown_windows_in(node);
            match windows.as_slice() {
                [] if node == tree.desktop() => "The Desktop".to_string(),
                [] => "An empty space".to_string(),
                [window_id] => self.windows.get(window_id).map_or_else(String::new, |window| {
                    let title = crate::state::window_title(window);
                    let app = crate::state::app_id(window);
                    if title.is_empty() { app } else { format!("{app} \u{b7} {title}") }
                }),
                many => format!("A space with {} windows", many.len()),
            }
        };
        match gesture {
            Some(Gesture::Pan(_, node)) => format!("Pan to: {}", describe(node)),
            Some(Gesture::Hover(_, node) | Gesture::Click(_, node)) => describe(node),
            None => {
                let windows: usize =
                    worlds.iter().map(|(_, world)| world.tree().windows_in(world.tree().root()).len()).sum();
                let plural = if windows == 1 { "" } else { "s" };
                format!("{windows} window{plural} \u{b7} click one to fly there, drag to pan")
            }
        }
    }

    /// The tree on `area`, or on a split screen each area's tree in its
    /// part. Only the active area shows its focus and a lit frame.
    fn draw_minimap(&self, painter: &mut Painter, area: Rect, detail: bool, gesture: Option<Gesture>) {
        let active = self.area_index();
        let gap = if detail { PART_GAP } else { CHIP_PART_GAP };
        for (index, world, part) in self.minimap_parts(area, gap) {
            let gesture = gesture.filter(|gesture| gesture.area() == index);
            if index != active {
                painter.fill(part, self.theme.background, 2.0);
            }
            self.draw_world_map(painter, world, part, detail, gesture, index == active);
        }
    }

    /// One tree on `area`: tiles, the Desktop, floating windows over the
    /// part on screen, and the camera's frame. `detail` adds app icons and
    /// hover marks for the card; the chip draws only the shapes.
    fn draw_world_map(
        &self,
        painter: &mut Painter,
        world: &World,
        area: Rect,
        detail: bool,
        gesture: Option<Gesture>,
        active: bool,
    ) {
        let theme = &self.theme;
        let tree = world.tree();
        let layout = world.layout();
        let focus = world.focus().filter(|_| active);
        let gap = if detail { 2.0 } else { 1.0 };
        let radius = if detail { 3.0 } else { 1.0 };
        for leaf in tree.leaves(tree.root()) {
            let Some(unit) = layout.rect(leaf) else { continue };
            let rect = to_area(area, unit).inset(gap / 2.0);
            match tree.node(leaf) {
                Node::Tile { views, selected } => {
                    let focused = focus.is_some_and(|window| views.contains(&window));
                    let hovered =
                        matches!(gesture, Some(Gesture::Hover(_, node) | Gesture::Click(_, node)) if node == leaf);
                    let colour = match (focused, detail) {
                        (true, _) => theme.accent.alpha(if detail { 0.4 } else { 0.85 }),
                        (false, true) if hovered => theme.text.alpha(0.24),
                        (false, true) => theme.surface_raised,
                        (false, false) => theme.text.alpha(0.32),
                    };
                    painter.fill(rect, colour, radius);
                    if !detail {
                        continue;
                    }
                    if views.len() > 1 {
                        // A tab group: a strip along the top, as on screen.
                        let strip = Rect::new(rect.x, rect.y, rect.w, 3.0_f64.min(rect.h));
                        painter.fill(strip, theme.muted.alpha(0.7), radius);
                    }
                    let icon = f64::from(crate::bar::ICON);
                    if rect.w >= icon + 6.0
                        && rect.h >= icon + 6.0
                        && let Some(window) = self.windows.get(&views[*selected])
                    {
                        let origin = (rect.center().x - icon / 2.0, rect.center().y - icon / 2.0);
                        self.app_mark(painter, &crate::state::app_id(window), origin);
                    }
                }
                Node::Desktop { .. } => {
                    let border = theme.muted.alpha(0.5);
                    painter.shape(rect, Shape { border_width: 1.0, border, radius, ..Default::default() });
                }
                Node::Split { .. } => {}
            }
        }

        // Floating windows hang over whatever is on screen, so they are drawn
        // over the camera's part of the map.
        let now = self.now();
        let view = world.view(now);
        let xform = world.xform(view);
        for window_id in tree.floats() {
            let Some(float) = world.float_rect(*window_id) else { continue };
            let corner = xform.invert_point(Point::new(float.x, float.y));
            let far = xform.invert_point(Point::new(float.right(), float.bottom()));
            let unit = Rect::new(corner.x, corner.y, far.x - corner.x, far.y - corner.y);
            let Some(rect) = clip(to_area(area, unit), area) else { continue };
            let focused = focus == Some(*window_id);
            let border = if focused { theme.accent } else { theme.text.alpha(0.6) };
            let fill = if detail { theme.surface } else { theme.text.alpha(0.5) };
            painter.shape(rect, Shape { border_width: 1.0, border, ..Shape::filled(fill, radius) });
        }

        if let Some(Gesture::Pan(_, node)) = gesture
            && let Some(unit) = layout.rect(node)
        {
            painter.shape(
                to_area(area, unit),
                Shape {
                    border_width: 1.5,
                    border: theme.accent_end,
                    ..Shape::filled(theme.accent_end.alpha(0.16), radius)
                },
            );
        }
        if let Some(frame) = clip(to_area(area, view), area.inset(-1.0)) {
            let shape = if !active {
                // Another area's camera: there, but not where input goes.
                Shape { border_width: 1.0, border: theme.muted, radius, ..Default::default() }
            } else if detail {
                Shape {
                    border_width: 1.5,
                    border: theme.accent,
                    glow_radius: theme.glow * 0.5,
                    glow: theme.accent.alpha(0.35),
                    radius,
                    ..Default::default()
                }
            } else {
                Shape { border_width: 1.0, border: theme.accent, radius, ..Default::default() }
            };
            painter.shape(frame, shape);
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test setup and assertions should fail immediately on unexpected errors")]
mod tests {
    use super::*;

    #[test]
    fn map_coordinates_round_trip() {
        let area = Rect::new(100.0, 50.0, 400.0, 200.0);
        let unit = Rect::new(0.5, 0.25, 0.25, 0.5);
        assert!(to_area(area, unit).approx_eq(&Rect::new(300.0, 100.0, 100.0, 100.0), 1e-9));
        let back = to_unit(area, Point::new(300.0, 100.0));
        assert!((back.x - 0.5).abs() < 1e-9 && (back.y - 0.25).abs() < 1e-9);
    }

    #[test]
    fn clipping_keeps_the_overlap() {
        let bounds = Rect::new(0.0, 0.0, 10.0, 10.0);
        assert!(
            clip(Rect::new(-5.0, 5.0, 10.0, 10.0), bounds).unwrap().approx_eq(&Rect::new(0.0, 5.0, 5.0, 5.0), 1e-9)
        );
        assert!(clip(Rect::new(20.0, 0.0, 5.0, 5.0), bounds).is_none());
    }

    #[test]
    fn a_small_move_is_still_a_click() {
        assert!(!dragged(Point::new(0.0, 0.0), Point::new(2.0, 2.0)));
        assert!(dragged(Point::new(0.0, 0.0), Point::new(3.0, 3.0)));
    }
}
