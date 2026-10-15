//! Turns the tree into rectangles in world units, where the root fills the unit
//! square. The camera later maps any node's rectangle onto the screen.

use std::collections::HashMap;

use crate::geom::Rect;
use crate::tree::{Axis, Node, NodeId, Tree};

#[derive(Debug, Clone, Default)]
pub struct Layout {
    rects: HashMap<NodeId, Rect>,
}

impl Layout {
    pub fn compute(tree: &Tree) -> Layout {
        let mut layout = Layout::default();
        layout.place(tree, tree.root(), Rect::UNIT);
        layout
    }

    /// The node's rectangle, or none when it takes no room (a hidden Desktop).
    pub fn rect(&self, id: NodeId) -> Option<Rect> {
        self.rects.get(&id).copied()
    }

    pub fn iter(&self) -> impl Iterator<Item = (NodeId, Rect)> + '_ {
        self.rects.iter().map(|(k, v)| (*k, *v))
    }

    fn place(&mut self, tree: &Tree, id: NodeId, rect: Rect) {
        if !tree.is_visible(id) {
            return;
        }
        self.rects.insert(id, rect);
        let Node::Split { axis, children, weights } = tree.node(id) else { return };
        let shown: Vec<(NodeId, f64)> =
            children.iter().zip(weights).filter(|(c, _)| tree.is_visible(**c)).map(|(c, w)| (*c, *w)).collect();
        let total: f64 = shown.iter().map(|(_, w)| w).sum();
        let mut offset = 0.0;
        for (child, weight) in shown {
            let frac = weight / total;
            let r = match axis {
                Axis::Horizontal => Rect::new(rect.x + offset * rect.w, rect.y, frac * rect.w, rect.h),
                Axis::Vertical => Rect::new(rect.x, rect.y + offset * rect.h, rect.w, frac * rect.h),
            };
            offset += frac;
            self.place(tree, child, r);
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test setup and assertions should fail immediately on unexpected errors")]
mod tests {
    use super::*;
    use crate::tree::{Side, WindowId};

    #[test]
    fn hidden_desktop_takes_no_room() {
        let mut t = Tree::new();
        let a = t.dock(WindowId(1), t.desktop(), Side::Right);
        let b = t.dock(WindowId(2), a, Side::Bottom);
        let l = Layout::compute(&t);
        assert!(l.rect(t.desktop()).is_none());
        assert!(l.rect(a).unwrap().approx_eq(&Rect::new(0.0, 0.0, 1.0, 0.5), 1e-9));
        assert!(l.rect(b).unwrap().approx_eq(&Rect::new(0.0, 0.5, 1.0, 0.5), 1e-9));
    }

    #[test]
    fn shown_desktop_shares_the_row() {
        let mut t = Tree::new();
        let a = t.dock(WindowId(1), t.desktop(), Side::Right);
        t.set_desktop_shown(true);
        let l = Layout::compute(&t);
        assert!(l.rect(t.desktop()).unwrap().approx_eq(&Rect::new(0.0, 0.0, 0.5, 1.0), 1e-9));
        assert!(l.rect(a).unwrap().approx_eq(&Rect::new(0.5, 0.0, 0.5, 1.0), 1e-9));
    }
}
