//! The nested layout tree of one world (one display).
//!
//! Leaves are tiles (one window, or several as tabs) and the single Desktop node,
//! which holds floating windows. Inner nodes are splits along one axis with a
//! weight per child. Node ids stay valid for the life of the node; when an edit
//! removes a node, `resolve` redirects its id to whatever took its place so that
//! history entries and saved views degrade gracefully.

use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NodeId(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
pub struct WindowId(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum Axis {
    /// Children sit side by side, left to right.
    Horizontal,
    /// Children are stacked, top to bottom.
    Vertical,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Side {
    Left,
    Right,
    Top,
    Bottom,
}

impl Side {
    pub fn axis(self) -> Axis {
        match self {
            Side::Left | Side::Right => Axis::Horizontal,
            Side::Top | Side::Bottom => Axis::Vertical,
        }
    }

    /// Whether the side comes first along its axis.
    pub fn is_leading(self) -> bool {
        matches!(self, Side::Left | Side::Top)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Node {
    Split { axis: Axis, children: Vec<NodeId>, weights: Vec<f64> },
    Tile { views: Vec<WindowId>, selected: usize },
    Desktop { floats: Vec<WindowId>, shown: bool },
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind")]
pub enum LayoutNode {
    Split { axis: Axis, children: Vec<LayoutNode>, weights: Vec<f64> },
    Tile { views: Vec<WindowId>, selected: usize },
    Desktop { floats: Vec<WindowId>, shown: bool },
}

#[derive(Debug, Clone)]
struct Slot {
    node: Node,
    parent: Option<NodeId>,
}

/// Smallest share a child may be resized down to, as a fraction of its split.
pub const MIN_WEIGHT_FRACTION: f64 = 0.05;

#[derive(Debug, Clone)]
pub struct Tree {
    slots: Vec<Option<Slot>>,
    root: NodeId,
    desktop: NodeId,
    redirect: HashMap<NodeId, NodeId>,
    /// When a tile leaves, spread its space over the whole split (so closing a
    /// window exactly undoes adding it). Otherwise the next neighbour takes it.
    pub share_removed_space: bool,
}

impl Default for Tree {
    fn default() -> Self {
        Self::new()
    }
}

impl Tree {
    pub fn snapshot(&self) -> LayoutNode {
        fn visit(tree: &Tree, id: NodeId) -> LayoutNode {
            match tree.node(id) {
                Node::Split { axis, children, weights } => LayoutNode::Split {
                    axis: *axis,
                    children: children.iter().map(|id| visit(tree, *id)).collect(),
                    weights: weights.clone(),
                },
                Node::Tile { views, selected } => LayoutNode::Tile { views: views.clone(), selected: *selected },
                Node::Desktop { floats, shown } => LayoutNode::Desktop { floats: floats.clone(), shown: *shown },
            }
        }
        visit(self, self.root)
    }

    pub fn restore(snapshot: &LayoutNode, remap: &HashMap<WindowId, WindowId>) -> Option<Self> {
        fn visit(
            tree: &mut Tree,
            node: &LayoutNode,
            remap: &HashMap<WindowId, WindowId>,
            depth: usize,
            seen: &mut std::collections::HashSet<WindowId>,
            desktop: &mut bool,
        ) -> Result<Option<NodeId>, ()> {
            if depth > 64 || tree.slots.len() > 10000 {
                return Err(());
            }
            let mut map_views = |views: &[WindowId]| -> Result<Vec<WindowId>, ()> {
                let mut mapped = Vec::new();
                for id in views {
                    if !seen.insert(*id) {
                        return Err(());
                    }
                    if let Some(live) = remap.get(id) {
                        mapped.push(*live);
                    }
                }
                Ok(mapped)
            };
            match node {
                LayoutNode::Tile { views, selected } => {
                    if views.is_empty() || *selected >= views.len() {
                        return Err(());
                    }
                    let mapped = map_views(views)?;
                    if mapped.is_empty() {
                        return Ok(None);
                    }
                    let selected =
                        remap.get(&views[*selected]).and_then(|id| mapped.iter().position(|w| w == id)).unwrap_or(0);
                    Ok(Some(tree.alloc(Node::Tile { views: mapped, selected }, None)))
                }
                LayoutNode::Desktop { floats, shown } => {
                    if *desktop {
                        return Err(());
                    }
                    *desktop = true;
                    tree.slot_mut(tree.desktop).node = Node::Desktop { floats: map_views(floats)?, shown: *shown };
                    Ok(Some(tree.desktop))
                }
                LayoutNode::Split { axis, children, weights } => {
                    if children.len() < 2
                        || children.len() != weights.len()
                        || weights.iter().any(|w| !w.is_finite() || *w <= 0.0)
                    {
                        return Err(());
                    }
                    let mut kept = Vec::new();
                    let mut sizes = Vec::new();
                    for (child, weight) in children.iter().zip(weights) {
                        if let Some(id) = visit(tree, child, remap, depth + 1, seen, desktop)? {
                            kept.push(id);
                            sizes.push(*weight);
                        }
                    }
                    match kept.len() {
                        0 => Ok(None),
                        1 => Ok(kept.pop()),
                        _ => {
                            let parent =
                                tree.alloc(Node::Split { axis: *axis, children: kept.clone(), weights: sizes }, None);
                            for child in kept {
                                tree.slot_mut(child).parent = Some(parent);
                            }
                            Ok(Some(parent))
                        }
                    }
                }
            }
        }
        let mut tree = Tree::new();
        let mut desktop = false;
        let root = visit(&mut tree, snapshot, remap, 0, &mut Default::default(), &mut desktop).ok()??;
        if !desktop {
            return None;
        }
        tree.root = root;
        tree.slot_mut(root).parent = None;
        Some(tree)
    }

    pub fn new() -> Self {
        let desktop = NodeId(0);
        Tree {
            slots: vec![Some(Slot { node: Node::Desktop { floats: Vec::new(), shown: false }, parent: None })],
            root: desktop,
            desktop,
            redirect: HashMap::new(),
            share_removed_space: true,
        }
    }

    pub fn root(&self) -> NodeId {
        self.root
    }

    pub fn desktop(&self) -> NodeId {
        self.desktop
    }

    pub fn contains(&self, id: NodeId) -> bool {
        self.slots.get(id.0 as usize).is_some_and(|s| s.is_some())
    }

    pub fn node(&self, id: NodeId) -> &Node {
        &self.slot(id).node
    }

    pub fn parent(&self, id: NodeId) -> Option<NodeId> {
        self.slot(id).parent
    }

    pub fn children(&self, id: NodeId) -> &[NodeId] {
        match self.node(id) {
            Node::Split { children, .. } => children,
            _ => &[],
        }
    }

    pub fn is_leaf(&self, id: NodeId) -> bool {
        !matches!(self.node(id), Node::Split { .. })
    }

    /// Maps an id that may have been removed to the live node that replaced it.
    pub fn resolve(&self, mut id: NodeId) -> NodeId {
        let mut guard = 0;
        while !self.contains(id) {
            match self.redirect.get(&id) {
                Some(next) => id = *next,
                None => return self.root,
            }
            guard += 1;
            if guard > 10_000 {
                return self.root;
            }
        }
        id
    }

    /// Ancestors from `id` (inclusive) up to the root.
    pub fn ancestors(&self, id: NodeId) -> Vec<NodeId> {
        let mut out = vec![id];
        let mut cur = id;
        while let Some(p) = self.parent(cur) {
            out.push(p);
            cur = p;
        }
        out
    }

    pub fn depth(&self, id: NodeId) -> usize {
        self.ancestors(id).len() - 1
    }

    pub fn is_ancestor_or_self(&self, ancestor: NodeId, id: NodeId) -> bool {
        self.ancestors(id).contains(&ancestor)
    }

    pub fn common_ancestor(&self, a: NodeId, b: NodeId) -> NodeId {
        let up_a = self.ancestors(a);
        self.ancestors(b).into_iter().find(|n| up_a.contains(n)).unwrap_or(self.root)
    }

    /// Every node in the subtree, parents before children, in child order.
    pub fn subtree(&self, id: NodeId) -> Vec<NodeId> {
        let mut out = Vec::new();
        let mut stack = vec![id];
        while let Some(n) = stack.pop() {
            out.push(n);
            for c in self.children(n).iter().rev() {
                stack.push(*c);
            }
        }
        out
    }

    pub fn leaves(&self, id: NodeId) -> Vec<NodeId> {
        self.subtree(id).into_iter().filter(|n| self.is_leaf(*n)).collect()
    }

    /// The tile holding `win` as one of its views.
    pub fn tile_of(&self, win: WindowId) -> Option<NodeId> {
        self.live_ids().find(|id| matches!(self.node(*id), Node::Tile { views, .. } if views.contains(&win)))
    }

    pub fn is_floating(&self, win: WindowId) -> bool {
        matches!(self.node(self.desktop), Node::Desktop { floats, .. } if floats.contains(&win))
    }

    /// The leaf (tile or Desktop) that holds `win`.
    pub fn leaf_of(&self, win: WindowId) -> Option<NodeId> {
        if self.is_floating(win) {
            Some(self.desktop)
        } else {
            self.tile_of(win)
        }
    }

    /// All windows in the subtree, in layout order; floating windows included.
    pub fn windows_in(&self, id: NodeId) -> Vec<WindowId> {
        let mut out = Vec::new();
        for n in self.subtree(id) {
            match self.node(n) {
                Node::Tile { views, .. } => out.extend(views.iter().copied()),
                Node::Desktop { floats, .. } => out.extend(floats.iter().copied()),
                Node::Split { .. } => {}
            }
        }
        out
    }

    /// Windows that are actually shown in the layout: the selected view of
    /// each tile. Floating windows are not part of any space.
    pub fn shown_windows_in(&self, id: NodeId) -> Vec<WindowId> {
        let mut out = Vec::new();
        for n in self.subtree(id) {
            if let Node::Tile { views, selected } = self.node(n) {
                out.push(views[*selected]);
            }
        }
        out
    }

    /// Floating windows, bottom to top.
    pub fn floats(&self) -> &[WindowId] {
        match self.node(self.desktop) {
            Node::Desktop { floats, .. } => floats,
            _ => unreachable!("desktop id always names the Desktop node"),
        }
    }

    /// Puts a floating window above the others.
    pub fn raise_float(&mut self, win: WindowId) {
        if let Node::Desktop { floats, .. } = &mut self.slot_mut(self.desktop).node {
            if let Some(i) = floats.iter().position(|w| *w == win) {
                let w = floats.remove(i);
                floats.push(w);
            }
        }
    }

    /// Turns a split's children from side by side to stacked, or back.
    pub fn toggle_axis(&mut self, split: NodeId) -> bool {
        if let Node::Split { axis, .. } = &mut self.slot_mut(split).node {
            *axis = match axis {
                Axis::Horizontal => Axis::Vertical,
                Axis::Vertical => Axis::Horizontal,
            };
            return true;
        }
        false
    }

    /// The split boundary on one side of a node: the split and the index of
    /// the child before the boundary, for `resize_gap`.
    pub fn boundary(&self, id: NodeId, side: Side) -> Option<(NodeId, usize)> {
        let mut cur = id;
        while let Some(parent) = self.parent(cur) {
            if let Node::Split { axis, children, .. } = self.node(parent) {
                if *axis == side.axis() {
                    let i = children.iter().position(|c| *c == cur).expect("child of its parent");
                    let shown = |j: &usize| self.is_visible(children[*j]);
                    if side.is_leading() {
                        if let Some(j) = (0..i).rev().find(shown) {
                            return Some((parent, j));
                        }
                    } else if (i + 1..children.len()).any(|j| shown(&j)) {
                        return Some((parent, i));
                    }
                }
            }
            cur = parent;
        }
        None
    }

    /// The Desktop takes up room in the layout when asked to, or when there
    /// is nothing else to show. Floating windows hang above the layout and do
    /// not need it.
    pub fn desktop_visible(&self) -> bool {
        match self.node(self.desktop) {
            Node::Desktop { shown, .. } => *shown || self.root == self.desktop,
            _ => unreachable!("desktop id always names the Desktop node"),
        }
    }

    /// Whether a node takes up any room in the layout.
    pub fn is_visible(&self, id: NodeId) -> bool {
        if id == self.desktop {
            return self.desktop_visible();
        }
        match self.node(id) {
            Node::Split { children, .. } => children.iter().any(|c| self.is_visible(*c)),
            _ => true,
        }
    }

    pub fn set_desktop_shown(&mut self, show: bool) {
        if let Node::Desktop { shown, .. } = &mut self.slot_mut(self.desktop).node {
            *shown = show;
        }
    }

    pub fn desktop_shown(&self) -> bool {
        matches!(self.node(self.desktop), Node::Desktop { shown: true, .. })
    }

    /// Puts `win` in a new tile beside `target`. Returns the new tile.
    ///
    /// When the target already sits in a split along the same axis, the new tile
    /// joins that split. With `share_removed_space` it takes an equal share from
    /// the whole row (so removing it again restores every other share exactly);
    /// otherwise it takes half of the target's share. A target in any other
    /// position is wrapped in a new two-way split.
    pub fn dock(&mut self, win: WindowId, target: NodeId, side: Side) -> NodeId {
        debug_assert!(self.leaf_of(win).is_none(), "window must be detached before docking");
        let tile = self.alloc(Node::Tile { views: vec![win], selected: 0 }, None);
        self.insert_beside(tile, target, side);
        tile
    }

    /// Adds `win` as a new tab of `tile` and selects it.
    pub fn tab(&mut self, win: WindowId, tile: NodeId) {
        debug_assert!(self.leaf_of(win).is_none(), "window must be detached before tabbing");
        if let Node::Tile { views, selected } = &mut self.slot_mut(tile).node {
            views.push(win);
            *selected = views.len() - 1;
        } else {
            panic!("tab target {tile:?} is not a tile");
        }
    }

    /// Extract the selected part of the tree and move it as one subtree.
    pub fn move_group(&mut self, windows: &[WindowId], target: WindowId, side: Side) -> bool {
        fn project(tree: &Tree, id: NodeId, windows: &[WindowId]) -> Option<LayoutNode> {
            match tree.node(id) {
                Node::Desktop { .. } => None,
                Node::Tile { views, selected } => {
                    let kept: Vec<_> = views.iter().filter(|w| windows.contains(w)).copied().collect();
                    if kept.is_empty() {
                        return None;
                    }
                    let selected = kept.iter().position(|w| *w == views[*selected]).unwrap_or(0);
                    Some(LayoutNode::Tile { views: kept, selected })
                }
                Node::Split { axis, children, weights } => {
                    let (mut kept, mut sizes) = (Vec::new(), Vec::new());
                    for (child, weight) in children.iter().zip(weights) {
                        if let Some(node) = project(tree, *child, windows) {
                            kept.push(node);
                            sizes.push(*weight);
                        }
                    }
                    match kept.len() {
                        0 => None,
                        1 => kept.pop(),
                        _ => {
                            Some(LayoutNode::Split { axis: *axis, children: kept, weights: std::mem::take(&mut sizes) })
                        }
                    }
                }
            }
        }
        fn append(tree: &mut Tree, node: LayoutNode) -> NodeId {
            match node {
                LayoutNode::Tile { views, selected } => tree.alloc(Node::Tile { views, selected }, None),
                LayoutNode::Split { axis, children, weights } => {
                    let children: Vec<_> = children.into_iter().map(|node| append(tree, node)).collect();
                    let parent = tree.alloc(Node::Split { axis, children: children.clone(), weights }, None);
                    for child in children {
                        tree.slot_mut(child).parent = Some(parent);
                    }
                    parent
                }
                LayoutNode::Desktop { .. } => unreachable!("selected projection excludes the Desktop"),
            }
        }
        if windows.contains(&target) || self.tile_of(target).is_none() {
            return false;
        }
        let Some(group) = project(self, self.root, windows) else { return false };
        for window in windows {
            self.detach(*window);
        }
        let target = self.tile_of(target).expect("unselected target survives extraction");
        let group = append(self, group);
        self.insert_beside(group, target, side);
        true
    }

    /// Puts `win` on the Desktop as a floating window.
    pub fn float(&mut self, win: WindowId) {
        debug_assert!(self.leaf_of(win).is_none(), "window must be detached before floating");
        if let Node::Desktop { floats, .. } = &mut self.slot_mut(self.desktop).node {
            floats.push(win);
        }
    }

    pub fn select_tab(&mut self, win: WindowId) -> bool {
        let Some(tile) = self.tile_of(win) else { return false };
        if let Node::Tile { views, selected } = &mut self.slot_mut(tile).node {
            *selected = views.iter().position(|v| *v == win).unwrap_or(*selected);
        }
        true
    }

    /// Removes `win` from wherever it is. A tile left empty is removed too and
    /// its split collapses if only one child remains.
    pub fn detach(&mut self, win: WindowId) -> bool {
        if self.is_floating(win) {
            if let Node::Desktop { floats, .. } = &mut self.slot_mut(self.desktop).node {
                floats.retain(|w| *w != win);
            }
            return true;
        }
        let Some(tile) = self.tile_of(win) else { return false };
        let now_empty = match &mut self.slot_mut(tile).node {
            Node::Tile { views, selected } => {
                let idx = views.iter().position(|v| *v == win).expect("tile_of found it");
                views.remove(idx);
                if *selected > idx || *selected >= views.len() {
                    *selected = selected.saturating_sub(1);
                }
                views.is_empty()
            }
            _ => unreachable!(),
        };
        if now_empty {
            self.remove_node(tile);
        }
        true
    }

    /// Moves the boundary between child `index` and `index + 1` of a split by
    /// `delta`, a fraction of the split's total size. Returns whether anything
    /// changed.
    ///
    /// Hidden children (a hidden Desktop) are skipped: the boundary is between
    /// the nearest shown child at or before `index` and the one after it.
    pub fn resize_gap(&mut self, split: NodeId, index: usize, delta: f64) -> bool {
        let visible: Vec<bool> = self.children(split).iter().map(|c| self.is_visible(*c)).collect();
        let Some(before) = (0..=index.min(visible.len().saturating_sub(1))).rev().find(|j| visible[*j]) else {
            return false;
        };
        let Some(after) = (index + 1..visible.len()).find(|j| visible[*j]) else { return false };
        let Node::Split { weights, .. } = &mut self.slot_mut(split).node else { return false };
        let total: f64 = weights.iter().zip(&visible).filter(|(_, v)| **v).map(|(w, _)| *w).sum();
        let min = MIN_WEIGHT_FRACTION * total;
        let d = delta * total;
        let a = weights[before];
        let b = weights[after];
        let d = d.clamp(min - a, b - min);
        if d.abs() < 1e-12 {
            return false;
        }
        weights[before] = a + d;
        weights[after] = b - d;
        true
    }

    /// Ids of every live node, in allocation order.
    pub fn live_ids(&self) -> impl Iterator<Item = NodeId> + '_ {
        self.slots.iter().enumerate().filter(|(_, s)| s.is_some()).map(|(i, _)| NodeId(i as u32))
    }

    /// Panics with a description when an invariant is broken. Meant for tests.
    pub fn check_invariants(&self) {
        assert!(self.contains(self.root));
        assert!(self.parent(self.root).is_none());
        assert!(matches!(self.node(self.desktop), Node::Desktop { .. }));
        let reachable = self.subtree(self.root);
        assert!(reachable.contains(&self.desktop), "desktop must stay in the tree");
        assert_eq!(reachable.len(), self.live_ids().count(), "orphaned nodes");
        let mut seen = std::collections::HashSet::new();
        for id in reachable {
            match self.node(id) {
                Node::Split { children, weights, .. } => {
                    assert!(children.len() >= 2, "split {id:?} has {} children", children.len());
                    assert_eq!(children.len(), weights.len());
                    assert!(weights.iter().all(|w| *w > 0.0 && w.is_finite()));
                    for c in children {
                        assert_eq!(self.parent(*c), Some(id), "bad parent link");
                    }
                }
                Node::Tile { views, selected } => {
                    assert!(!views.is_empty(), "empty tile {id:?}");
                    assert!(*selected < views.len());
                    for v in views {
                        assert!(seen.insert(*v), "window {v:?} appears twice");
                    }
                }
                Node::Desktop { floats, .. } => {
                    for v in floats {
                        assert!(seen.insert(*v), "window {v:?} appears twice");
                    }
                }
            }
        }
    }

    fn slot(&self, id: NodeId) -> &Slot {
        self.slots[id.0 as usize].as_ref().unwrap_or_else(|| panic!("node {id:?} was removed"))
    }

    fn slot_mut(&mut self, id: NodeId) -> &mut Slot {
        self.slots[id.0 as usize].as_mut().unwrap_or_else(|| panic!("node {id:?} was removed"))
    }

    fn alloc(&mut self, node: Node, parent: Option<NodeId>) -> NodeId {
        let id = NodeId(self.slots.len() as u32);
        self.slots.push(Some(Slot { node, parent }));
        id
    }

    fn free(&mut self, id: NodeId, replaced_by: NodeId) {
        self.slots[id.0 as usize] = None;
        self.redirect.insert(id, replaced_by);
    }

    fn insert_beside(&mut self, new: NodeId, target: NodeId, side: Side) {
        let axis = side.axis();
        if let Some(parent) = self.parent(target) {
            let visible: Vec<bool> = self.children(parent).iter().map(|c| self.is_visible(*c)).collect();
            let share = self.share_removed_space;
            if let Node::Split { axis: pa, children, weights } = &mut self.slot_mut(parent).node {
                if *pa == axis {
                    let idx = children.iter().position(|c| *c == target).expect("child of its parent");
                    let weight = if share {
                        let shown: Vec<f64> =
                            weights.iter().zip(&visible).filter(|(_, v)| **v).map(|(w, _)| *w).collect();
                        shown.iter().sum::<f64>() / shown.len().max(1) as f64
                    } else {
                        weights[idx] /= 2.0;
                        weights[idx]
                    };
                    let at = if side.is_leading() { idx } else { idx + 1 };
                    children.insert(at, new);
                    weights.insert(at, weight);
                    self.slot_mut(new).parent = Some(parent);
                    return;
                }
            }
        }
        // Wrap the target in a new split that takes its place.
        let children = if side.is_leading() { vec![new, target] } else { vec![target, new] };
        let parent = self.parent(target);
        let split = self.alloc(Node::Split { axis, children, weights: vec![1.0, 1.0] }, parent);
        self.replace_child(parent, target, split);
        self.slot_mut(target).parent = Some(split);
        self.slot_mut(new).parent = Some(split);
    }

    /// Points `parent` (or the root, when `parent` is none) at `new` instead of `old`.
    fn replace_child(&mut self, parent: Option<NodeId>, old: NodeId, new: NodeId) {
        match parent {
            Some(p) => {
                if let Node::Split { children, .. } = &mut self.slot_mut(p).node {
                    let idx = children.iter().position(|c| *c == old).expect("child of its parent");
                    children[idx] = new;
                }
            }
            None => self.root = new,
        }
        self.slot_mut(new).parent = parent;
    }

    fn remove_node(&mut self, id: NodeId) {
        assert_ne!(id, self.desktop, "the Desktop is never removed");
        let parent = self.parent(id).expect("only the Desktop can be a parentless leaf");
        let share = self.share_removed_space;
        let heir = {
            let Node::Split { children, weights, .. } = &mut self.slot_mut(parent).node else { unreachable!() };
            let idx = children.iter().position(|c| *c == id).expect("child of its parent");
            children.remove(idx);
            let freed = weights.remove(idx);
            let heir_idx = idx.min(children.len() - 1);
            if !share {
                weights[heir_idx] += freed;
            }
            children[heir_idx]
        };
        self.free(id, heir);
        if self.children(parent).len() == 1 {
            self.collapse(parent);
        }
    }

    /// Replaces a single-child split with its child, merging the child into the
    /// grandparent when both split along the same axis.
    fn collapse(&mut self, split: NodeId) {
        let child = self.children(split)[0];
        let grand = self.parent(split);
        self.replace_child(grand, split, child);
        self.free(split, child);

        let Some(g) = grand else { return };
        let (Node::Split { axis: ga, .. }, Node::Split { axis: ca, .. }) = (self.node(g), self.node(child)) else {
            return;
        };
        if ga != ca {
            return;
        }
        let Node::Split { children: cc, weights: cw, .. } = self.node(child).clone() else { unreachable!() };
        let Node::Split { children, weights, .. } = &mut self.slot_mut(g).node else { unreachable!() };
        let idx = children.iter().position(|c| *c == child).expect("child of its parent");
        let share = weights[idx];
        let sum: f64 = cw.iter().sum();
        children.splice(idx..=idx, cc.iter().copied());
        weights.splice(idx..=idx, cw.iter().map(|w| w / sum * share));
        for c in cc {
            self.slot_mut(c).parent = Some(g);
        }
        self.free(child, g);
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test setup and assertions should fail immediately on unexpected errors")]
mod tests {
    use super::*;

    fn w(n: u64) -> WindowId {
        WindowId(n)
    }

    #[test]
    fn starts_with_a_lone_visible_desktop() {
        let t = Tree::new();
        t.check_invariants();
        assert_eq!(t.root(), t.desktop());
        assert!(t.desktop_visible());
    }

    #[test]
    fn docking_beside_the_desktop_hides_it() {
        let mut t = Tree::new();
        let a = t.dock(w(1), t.desktop(), Side::Right);
        t.check_invariants();
        assert!(!t.desktop_visible());
        assert_eq!(t.tile_of(w(1)), Some(a));
        assert!(matches!(t.node(t.root()), Node::Split { axis: Axis::Horizontal, .. }));
    }

    #[test]
    fn same_axis_dock_joins_the_split() {
        let mut t = Tree::new();
        let a = t.dock(w(1), t.desktop(), Side::Right);
        let b = t.dock(w(2), a, Side::Right);
        t.check_invariants();
        let root = t.root();
        let Node::Split { children, weights, .. } = t.node(root) else { panic!() };
        assert_eq!(children, &vec![t.desktop(), a, b]);
        assert_eq!(weights[1], weights[2]);
    }

    #[test]
    fn cross_axis_dock_nests_a_split() {
        let mut t = Tree::new();
        let a = t.dock(w(1), t.desktop(), Side::Right);
        let b = t.dock(w(2), a, Side::Bottom);
        t.check_invariants();
        let p = t.parent(b).unwrap();
        assert!(matches!(t.node(p), Node::Split { axis: Axis::Vertical, .. }));
        assert_eq!(t.children(p), &[a, b]);
    }

    fn shares(t: &Tree, split: NodeId) -> Vec<f64> {
        let Node::Split { children, weights, .. } = t.node(split) else { panic!() };
        let shown: Vec<(NodeId, f64)> =
            children.iter().zip(weights).filter(|(c, _)| t.is_visible(**c)).map(|(c, w)| (*c, *w)).collect();
        let sum: f64 = shown.iter().map(|(_, w)| w).sum();
        shown.iter().map(|(_, w)| w / sum).collect()
    }

    #[test]
    fn closing_undoes_adding_when_sharing_space() {
        let mut t = Tree::new();
        let a = t.dock(w(1), t.desktop(), Side::Right);
        let b = t.dock(w(2), a, Side::Right);
        t.resize_gap(t.root(), 1, 0.2);
        let before = shares(&t, t.root());
        t.dock(w(3), b, Side::Right);
        let with_c = shares(&t, t.root());
        assert!((with_c[2] - 1.0 / 3.0).abs() < 1e-9, "newcomer gets an equal share: {with_c:?}");
        t.detach(w(3));
        t.check_invariants();
        let after = shares(&t, t.root());
        for (x, y) in before.iter().zip(&after) {
            assert!((x - y).abs() < 1e-9, "{before:?} vs {after:?}");
        }
    }

    #[test]
    fn neighbour_takes_the_space_when_not_sharing() {
        let mut t = Tree::new();
        t.share_removed_space = false;
        let a = t.dock(w(1), t.desktop(), Side::Right);
        let b = t.dock(w(2), a, Side::Right);
        let c = t.dock(w(3), b, Side::Right);
        assert_eq!(shares(&t, t.root()), vec![0.5, 0.25, 0.25]);
        t.detach(w(2));
        t.check_invariants();
        assert_eq!(shares(&t, t.root()), vec![0.5, 0.5]);
        assert_eq!(t.resolve(b), c);
    }

    #[test]
    fn removing_collapses_and_redirects() {
        let mut t = Tree::new();
        let a = t.dock(w(1), t.desktop(), Side::Right);
        let b = t.dock(w(2), a, Side::Bottom);
        let split = t.parent(b).unwrap();
        t.detach(w(2));
        t.check_invariants();
        assert!(!t.contains(split));
        assert_eq!(t.resolve(split), a);
        assert_eq!(t.resolve(b), a);
    }

    #[test]
    fn collapse_merges_same_axis_splits() {
        let mut t = Tree::new();
        let a = t.dock(w(1), t.desktop(), Side::Right);
        let b = t.dock(w(2), a, Side::Bottom);
        let _c = t.dock(w(3), b, Side::Right);
        // b and c share a horizontal split inside the vertical one; removing a
        // collapses the vertical split, and the horizontal one joins the root.
        t.detach(w(1));
        t.check_invariants();
        assert_eq!(t.children(t.root()).len(), 3);
    }

    #[test]
    fn tabs_and_selection() {
        let mut t = Tree::new();
        let a = t.dock(w(1), t.desktop(), Side::Right);
        t.tab(w(2), a);
        t.tab(w(3), a);
        assert_eq!(t.shown_windows_in(a), vec![w(3)]);
        t.select_tab(w(1));
        assert_eq!(t.shown_windows_in(a), vec![w(1)]);
        t.detach(w(1));
        t.check_invariants();
        assert_eq!(t.windows_in(a), vec![w(2), w(3)]);
        t.detach(w(2));
        t.detach(w(3));
        t.check_invariants();
        assert!(!t.contains(a));
        assert_eq!(t.root(), t.desktop());
    }

    #[test]
    fn floating_leaves_the_layout_alone() {
        let mut t = Tree::new();
        t.dock(w(1), t.desktop(), Side::Right);
        t.float(w(2));
        t.float(w(3));
        assert!(!t.desktop_visible());
        assert_eq!(t.leaf_of(w(2)), Some(t.desktop()));
        t.raise_float(w(2));
        assert_eq!(t.floats(), &[w(3), w(2)]);
        t.detach(w(2));
        assert_eq!(t.floats(), &[w(3)]);
    }

    #[test]
    fn boundaries_and_axis_toggle() {
        let mut t = Tree::new();
        let a = t.dock(w(1), t.desktop(), Side::Right);
        let b = t.dock(w(2), a, Side::Right);
        let c = t.dock(w(3), b, Side::Bottom);
        let root = t.root();
        // a's left neighbour is the hidden Desktop: no boundary there.
        assert_eq!(t.boundary(a, Side::Left), None);
        assert_eq!(t.boundary(a, Side::Right), Some((root, 1)));
        assert_eq!(t.boundary(c, Side::Left), Some((root, 1)));
        let col = t.parent(c).unwrap();
        assert_eq!(t.boundary(c, Side::Top), Some((col, 0)));
        assert_eq!(t.boundary(c, Side::Bottom), None);
        assert!(t.toggle_axis(col));
        assert!(matches!(t.node(col), Node::Split { axis: Axis::Horizontal, .. }));
    }

    #[test]
    fn resize_gap_clamps() {
        let mut t = Tree::new();
        let a = t.dock(w(1), t.desktop(), Side::Right);
        t.dock(w(2), a, Side::Right);
        let root = t.root();
        // Only the hidden Desktop is before index 0: nothing to resize there.
        assert!(!t.resize_gap(root, 0, 0.1));
        assert!(t.resize_gap(root, 1, 0.9));
        let Node::Split { weights, .. } = t.node(root) else { panic!() };
        let total = weights[1] + weights[2];
        assert!((weights[2] / total - MIN_WEIGHT_FRACTION).abs() < 1e-9);
    }

    /// A small deterministic generator, so the random-edit test needs no crates.
    struct Lcg(u64);
    impl Lcg {
        fn next(&mut self, n: usize) -> usize {
            self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            ((self.0 >> 33) % n as u64) as usize
        }
    }

    #[test]
    fn random_edits_keep_invariants() {
        for seed in 0..200 {
            let mut rng = Lcg(seed);
            let mut t = Tree::new();
            t.share_removed_space = seed % 2 == 0;
            let mut live: Vec<WindowId> = Vec::new();
            let mut next = 1;
            for _ in 0..60 {
                match rng.next(5) {
                    0 | 1 => {
                        let leaves = t.leaves(t.root());
                        let target = leaves[rng.next(leaves.len())];
                        let side = [Side::Left, Side::Right, Side::Top, Side::Bottom][rng.next(4)];
                        t.dock(w(next), target, side);
                        live.push(w(next));
                        next += 1;
                    }
                    2 => {
                        let tiles: Vec<NodeId> = t.leaves(t.root()).into_iter().filter(|n| *n != t.desktop()).collect();
                        if tiles.is_empty() {
                            t.float(w(next));
                        } else {
                            t.tab(w(next), tiles[rng.next(tiles.len())]);
                        }
                        live.push(w(next));
                        next += 1;
                    }
                    _ => {
                        if !live.is_empty() {
                            let i = rng.next(live.len());
                            assert!(t.detach(live.remove(i)));
                        }
                    }
                }
                t.check_invariants();
                let mut all = t.windows_in(t.root());
                all.sort();
                let mut expect = live.clone();
                expect.sort();
                assert_eq!(all, expect);
            }
        }
    }
}
