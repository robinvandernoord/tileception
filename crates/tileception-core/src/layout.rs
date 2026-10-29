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
    pub fn rect(&self, node_id: NodeId) -> Option<Rect> {
        self.rects.get(&node_id).copied()
    }

    pub fn iter(&self) -> impl Iterator<Item = (NodeId, Rect)> + '_ {
        self.rects.iter().map(|(node_id, rect)| (*node_id, *rect))
    }

    fn place(&mut self, tree: &Tree, node_id: NodeId, rect: Rect) {
        if !tree.is_visible(node_id) {
            return;
        }
        self.rects.insert(node_id, rect);
        let Node::Split { axis, children, weights } = tree.node(node_id) else { return };
        let shown: Vec<(NodeId, f64)> = children
            .iter()
            .zip(weights)
            .filter(|(child, _)| tree.is_visible(**child))
            .map(|(child, weight)| (*child, *weight))
            .collect();
        let total: f64 = shown.iter().map(|(_, weight)| weight).sum();
        let mut offset = 0.0;
        for (child, weight) in shown {
            let frac = weight / total;
            let child_rect = match axis {
                Axis::Horizontal => Rect::new(rect.x + offset * rect.w, rect.y, frac * rect.w, rect.h),
                Axis::Vertical => Rect::new(rect.x, rect.y + offset * rect.h, rect.w, frac * rect.h),
            };
            offset += frac;
            self.place(tree, child, child_rect);
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
        let mut tree = Tree::new();
        let top_tile = tree.dock(WindowId(1), tree.desktop(), Side::Right);
        let bottom_tile = tree.dock(WindowId(2), top_tile, Side::Bottom);
        let layout = Layout::compute(&tree);
        assert!(layout.rect(tree.desktop()).is_none());
        assert!(layout.rect(top_tile).unwrap().approx_eq(&Rect::new(0.0, 0.0, 1.0, 0.5), 1e-9));
        assert!(layout.rect(bottom_tile).unwrap().approx_eq(&Rect::new(0.0, 0.5, 1.0, 0.5), 1e-9));
    }

    #[test]
    fn shown_desktop_shares_the_row() {
        let mut tree = Tree::new();
        let window_tile = tree.dock(WindowId(1), tree.desktop(), Side::Right);
        tree.set_desktop_shown(true);
        let layout = Layout::compute(&tree);
        assert!(layout.rect(tree.desktop()).unwrap().approx_eq(&Rect::new(0.0, 0.0, 0.5, 1.0), 1e-9));
        assert!(layout.rect(window_tile).unwrap().approx_eq(&Rect::new(0.5, 0.0, 0.5, 1.0), 1e-9));
    }
}
