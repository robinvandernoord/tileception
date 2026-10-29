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
        fn visit(tree: &Tree, node_id: NodeId) -> LayoutNode {
            match tree.node(node_id) {
                Node::Split { axis, children, weights } => LayoutNode::Split {
                    axis: *axis,
                    children: children.iter().map(|child_id| visit(tree, *child_id)).collect(),
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
                for window_id in views {
                    if !seen.insert(*window_id) {
                        return Err(());
                    }
                    if let Some(live) = remap.get(window_id) {
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
                    let selected = remap
                        .get(&views[*selected])
                        .and_then(|window_id| mapped.iter().position(|mapped_window| mapped_window == window_id))
                        .unwrap_or(0);
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
                        || weights.iter().any(|weight| !weight.is_finite() || *weight <= 0.0)
                    {
                        return Err(());
                    }
                    let mut kept = Vec::new();
                    let mut sizes = Vec::new();
                    for (child, weight) in children.iter().zip(weights) {
                        if let Some(child_id) = visit(tree, child, remap, depth + 1, seen, desktop)? {
                            kept.push(child_id);
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

    pub fn contains(&self, node_id: NodeId) -> bool {
        self.slots.get(node_id.0 as usize).is_some_and(|slot| slot.is_some())
    }

    pub fn node(&self, node_id: NodeId) -> &Node {
        &self.slot(node_id).node
    }

    pub fn parent(&self, node_id: NodeId) -> Option<NodeId> {
        self.slot(node_id).parent
    }

    pub fn children(&self, node_id: NodeId) -> &[NodeId] {
        match self.node(node_id) {
            Node::Split { children, .. } => children,
            _ => &[],
        }
    }

    pub fn is_leaf(&self, node_id: NodeId) -> bool {
        !matches!(self.node(node_id), Node::Split { .. })
    }

    /// Maps an id that may have been removed to the live node that replaced it.
    pub fn resolve(&self, mut node_id: NodeId) -> NodeId {
        let mut guard = 0;
        while !self.contains(node_id) {
            match self.redirect.get(&node_id) {
                Some(next) => node_id = *next,
                None => return self.root,
            }
            guard += 1;
            if guard > 10_000 {
                return self.root;
            }
        }
        node_id
    }

    /// Ancestors from `node_id` (inclusive) up to the root.
    pub fn ancestors(&self, node_id: NodeId) -> Vec<NodeId> {
        let mut out = vec![node_id];
        let mut cur = node_id;
        while let Some(parent) = self.parent(cur) {
            out.push(parent);
            cur = parent;
        }
        out
    }

    pub fn depth(&self, node_id: NodeId) -> usize {
        self.ancestors(node_id).len() - 1
    }

    pub fn is_ancestor_or_self(&self, ancestor: NodeId, node_id: NodeId) -> bool {
        self.ancestors(node_id).contains(&ancestor)
    }

    pub fn common_ancestor(&self, first: NodeId, second: NodeId) -> NodeId {
        let ancestors = self.ancestors(first);
        self.ancestors(second).into_iter().find(|node_id| ancestors.contains(node_id)).unwrap_or(self.root)
    }

    /// Every node in the subtree, parents before children, in child order.
    pub fn subtree(&self, node_id: NodeId) -> Vec<NodeId> {
        let mut out = Vec::new();
        let mut stack = vec![node_id];
        while let Some(current) = stack.pop() {
            out.push(current);
            for child in self.children(current).iter().rev() {
                stack.push(*child);
            }
        }
        out
    }

    pub fn leaves(&self, node_id: NodeId) -> Vec<NodeId> {
        self.subtree(node_id).into_iter().filter(|node_id| self.is_leaf(*node_id)).collect()
    }

    /// The tile holding `win` as one of its views.
    pub fn tile_of(&self, win: WindowId) -> Option<NodeId> {
        self.live_ids().find(|node_id| matches!(self.node(*node_id), Node::Tile { views, .. } if views.contains(&win)))
    }

    pub fn is_floating(&self, win: WindowId) -> bool {
        matches!(self.node(self.desktop), Node::Desktop { floats, .. } if floats.contains(&win))
    }

    /// The leaf (tile or Desktop) that holds `win`.
    pub fn leaf_of(&self, win: WindowId) -> Option<NodeId> {
        if self.is_floating(win) { Some(self.desktop) } else { self.tile_of(win) }
    }

    /// All windows in the subtree, in layout order; floating windows included.
    pub fn windows_in(&self, node_id: NodeId) -> Vec<WindowId> {
        let mut out = Vec::new();
        for child_id in self.subtree(node_id) {
            match self.node(child_id) {
                Node::Tile { views, .. } => out.extend(views.iter().copied()),
                Node::Desktop { floats, .. } => out.extend(floats.iter().copied()),
                Node::Split { .. } => {}
            }
        }
        out
    }

    /// Windows that are actually shown in the layout: the selected view of
    /// each tile. Floating windows are not part of any space.
    pub fn shown_windows_in(&self, node_id: NodeId) -> Vec<WindowId> {
        let mut out = Vec::new();
        for child_id in self.subtree(node_id) {
            if let Node::Tile { views, selected } = self.node(child_id) {
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
        if let Node::Desktop { floats, .. } = &mut self.slot_mut(self.desktop).node
            && let Some(index) = floats.iter().position(|window| *window == win)
        {
            let window = floats.remove(index);
            floats.push(window);
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
    pub fn boundary(&self, node_id: NodeId, side: Side) -> Option<(NodeId, usize)> {
        let mut cur = node_id;
        while let Some(parent) = self.parent(cur) {
            if let Node::Split { axis, children, .. } = self.node(parent)
                && *axis == side.axis()
            {
                let index = children.iter().position(|child| *child == cur).expect("child of its parent");
                let shown = |candidate: &usize| self.is_visible(children[*candidate]);
                if side.is_leading() {
                    if let Some(previous) = (0..index).rev().find(shown) {
                        return Some((parent, previous));
                    }
                } else if (index + 1..children.len()).any(|candidate| shown(&candidate)) {
                    return Some((parent, index));
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
    pub fn is_visible(&self, node_id: NodeId) -> bool {
        if node_id == self.desktop {
            return self.desktop_visible();
        }
        match self.node(node_id) {
            Node::Split { children, .. } => children.iter().any(|child| self.is_visible(*child)),
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

    /// Split inside a framed tile instead of joining its outer row.
    pub fn dock_local(&mut self, win: WindowId, target: NodeId, side: Side) -> NodeId {
        debug_assert!(self.leaf_of(win).is_none(), "window must be detached before docking");
        let tile = self.alloc(Node::Tile { views: vec![win], selected: 0 }, None);
        self.wrap_beside(tile, target, side);
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

    fn project(&self, node_id: NodeId, windows: &[WindowId]) -> Option<LayoutNode> {
        match self.node(node_id) {
            Node::Desktop { .. } => None,
            Node::Tile { views, selected } => {
                let kept: Vec<_> = views.iter().filter(|window| windows.contains(window)).copied().collect();
                if kept.is_empty() {
                    return None;
                }
                let selected = kept.iter().position(|window| *window == views[*selected]).unwrap_or(0);
                Some(LayoutNode::Tile { views: kept, selected })
            }
            Node::Split { axis, children, weights } => {
                let (mut kept, mut sizes) = (Vec::new(), Vec::new());
                for (child, weight) in children.iter().zip(weights) {
                    if let Some(node) = self.project(*child, windows) {
                        kept.push(node);
                        sizes.push(*weight);
                    }
                }
                match kept.len() {
                    0 => None,
                    1 => kept.pop(),
                    _ => Some(LayoutNode::Split { axis: *axis, children: kept, weights: std::mem::take(&mut sizes) }),
                }
            }
        }
    }

    fn append(&mut self, node: LayoutNode) -> NodeId {
        match node {
            LayoutNode::Tile { views, selected } => self.alloc(Node::Tile { views, selected }, None),
            LayoutNode::Split { axis, children, weights } => {
                let children: Vec<_> = children.into_iter().map(|node| self.append(node)).collect();
                let parent = self.alloc(Node::Split { axis, children: children.clone(), weights }, None);
                for child in children {
                    self.slot_mut(child).parent = Some(parent);
                }
                parent
            }
            LayoutNode::Desktop { .. } => unreachable!("selected projection excludes the Desktop"),
        }
    }

    /// Extract the selected part of the tree and move it as one subtree.
    pub fn move_group(&mut self, windows: &[WindowId], target: WindowId, side: Side) -> bool {
        if windows.contains(&target) || self.tile_of(target).is_none() {
            return false;
        }
        let Some(group) = self.project(self.root, windows) else { return false };
        for window in windows {
            self.detach(*window);
        }
        let target = self.tile_of(target).expect("unselected target survives extraction");
        let group = self.append(group);
        self.insert_beside(group, target, side);
        true
    }

    /// Make an exact tiled selection a subtree, preserving its internal layout.
    pub fn group_windows(&mut self, windows: &[WindowId]) -> Option<NodeId> {
        let tiles: Vec<_> = windows.iter().filter_map(|window| self.tile_of(*window)).collect();
        if tiles.len() != windows.len() || tiles.is_empty() {
            return None;
        }
        let common = tiles.iter().copied().reduce(|first, second| self.common_ancestor(first, second))?;
        if self.windows_in(common).iter().all(|window| windows.contains(window)) {
            return Some(common);
        }
        // Whole sibling branches retain their sizes and outside proportions.
        if let Node::Split { axis, children, weights } = self.node(common).clone() {
            let selected: Vec<_> = children
                .iter()
                .map(|child| {
                    let contents = self.windows_in(*child);
                    (!contents.is_empty() && contents.iter().all(|window| windows.contains(window)))
                        && !self.subtree(*child).contains(&self.desktop)
                })
                .collect();
            let covered: Vec<_> = children
                .iter()
                .zip(&selected)
                .filter(|(_, keep)| **keep)
                .flat_map(|(child, _)| self.windows_in(*child))
                .collect();
            if windows.iter().all(|window| covered.contains(window)) {
                let grouped: Vec<_> =
                    children.iter().zip(&selected).filter(|(_, keep)| **keep).map(|(child_id, _)| *child_id).collect();
                let sizes: Vec<_> =
                    weights.iter().zip(&selected).filter(|(_, keep)| **keep).map(|(weight, _)| *weight).collect();
                let size: f64 = sizes.iter().sum();
                let group = self.alloc(Node::Split { axis, children: grouped.clone(), weights: sizes }, Some(common));
                for child in grouped {
                    self.slot_mut(child).parent = Some(group);
                }
                let mut kept = Vec::new();
                let mut kept_sizes = Vec::new();
                let mut inserted = false;
                for ((child, weight), keep) in children.into_iter().zip(weights).zip(selected) {
                    if keep {
                        if !inserted {
                            kept.push(group);
                            kept_sizes.push(size);
                            inserted = true;
                        }
                    } else {
                        kept.push(child);
                        kept_sizes.push(weight);
                    }
                }
                self.slot_mut(common).node = Node::Split { axis, children: kept, weights: kept_sizes };
                return Some(group);
            }
        }
        // Selections across branches need extraction. Keep the first tile's
        // position, and leave unselected windows in their existing branches.
        let layout = self.project(common, windows)?;
        let anchor = self.windows_in(common).into_iter().find(|window| windows.contains(window))?;
        for window in windows.iter().copied().filter(|window| *window != anchor) {
            self.detach(window);
        }
        let tile = self.tile_of(anchor)?;
        let group = self.append(layout);
        if self.windows_in(tile).len() == 1 {
            self.replace_child(self.parent(tile), tile, group);
            self.free(tile, group);
        } else {
            self.detach(anchor);
            self.wrap_beside(group, tile, Side::Left);
        }
        Some(group)
    }

    /// The node holding exactly these tiled windows, without moving any
    /// window: an existing node, or a new split grouping a run of adjacent
    /// siblings, to dock something beside along `across`. A run is only
    /// grouped in a split along the other axis: grouping along the same axis
    /// would nest a split in one like it. None when the windows are not
    /// together like that.
    pub fn node_holding(&mut self, windows: &[WindowId], across: Axis) -> Option<NodeId> {
        let tiles: Vec<_> = windows.iter().filter_map(|window| self.tile_of(*window)).collect();
        if tiles.len() != windows.len() || tiles.is_empty() {
            return None;
        }
        let common = tiles.iter().copied().reduce(|first, second| self.common_ancestor(first, second))?;
        if self.windows_in(common).iter().all(|window| windows.contains(window)) {
            return Some(common);
        }
        let Node::Split { axis, children, weights } = self.node(common).clone() else { return None };
        if axis == across {
            return None;
        }
        let whole = |child: &NodeId| {
            let contents = self.windows_in(*child);
            !contents.is_empty() && contents.iter().all(|window| windows.contains(window))
        };
        let first = children.iter().position(whole)?;
        let run = children[first..].iter().take_while(|child| whole(child)).count();
        let covered: Vec<_> = children[first..first + run].iter().flat_map(|child| self.windows_in(*child)).collect();
        if run < 2 || !windows.iter().all(|window| covered.contains(window)) {
            return None;
        }
        let grouped = children[first..first + run].to_vec();
        let sizes = weights[first..first + run].to_vec();
        let size = sizes.iter().sum();
        let group = self.alloc(Node::Split { axis, children: grouped.clone(), weights: sizes }, Some(common));
        for child in grouped {
            self.slot_mut(child).parent = Some(group);
        }
        let (mut children, mut weights) = (children, weights);
        children.splice(first..first + run, [group]);
        weights.splice(first..first + run, [size]);
        self.slot_mut(common).node = Node::Split { axis, children, weights };
        Some(group)
    }

    /// A node's share of its split, among the children that take up room.
    pub fn share_of(&self, node_id: NodeId) -> Option<f64> {
        let parent = self.parent(node_id)?;
        let Node::Split { children, weights, .. } = self.node(parent) else { return None };
        let total: f64 =
            children.iter().zip(weights).filter(|(child, _)| self.is_visible(**child)).map(|(_, weight)| *weight).sum();
        let index = children.iter().position(|child| *child == node_id)?;
        (total > 0.0).then(|| weights[index] / total)
    }

    /// Gives a node this share of its split; the others keep their proportions.
    pub fn set_share(&mut self, node_id: NodeId, share: f64) {
        let Some(parent) = self.parent(node_id) else { return };
        let visible: Vec<bool> = self.children(parent).iter().map(|child| self.is_visible(*child)).collect();
        let Node::Split { children, weights, .. } = &mut self.slot_mut(parent).node else { return };
        let Some(index) = children.iter().position(|child| *child == node_id) else { return };
        let others: f64 = weights
            .iter()
            .zip(&visible)
            .enumerate()
            .filter(|(candidate, (_, visible))| *candidate != index && **visible)
            .map(|(_, (weight, _))| *weight)
            .sum();
        let share = share.clamp(MIN_WEIGHT_FRACTION, 1.0 - MIN_WEIGHT_FRACTION);
        if others > 0.0 {
            weights[index] = share / (1.0 - share) * others;
        }
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
            *selected = views.iter().position(|window| *window == win).unwrap_or(*selected);
        }
        true
    }

    /// Removes `win` from wherever it is. A tile left empty is removed too and
    /// its split collapses if only one child remains.
    pub fn detach(&mut self, win: WindowId) -> bool {
        if self.is_floating(win) {
            if let Node::Desktop { floats, .. } = &mut self.slot_mut(self.desktop).node {
                floats.retain(|window| *window != win);
            }
            return true;
        }
        let Some(tile) = self.tile_of(win) else { return false };
        let now_empty = match &mut self.slot_mut(tile).node {
            Node::Tile { views, selected } => {
                let idx = views.iter().position(|window| *window == win).expect("tile_of found it");
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
        let visible: Vec<bool> = self.children(split).iter().map(|child| self.is_visible(*child)).collect();
        let Some(before) = (0..=index.min(visible.len().saturating_sub(1))).rev().find(|candidate| visible[*candidate])
        else {
            return false;
        };
        let Some(after) = (index + 1..visible.len()).find(|candidate| visible[*candidate]) else { return false };
        let Node::Split { weights, .. } = &mut self.slot_mut(split).node else { return false };
        let total: f64 = weights.iter().zip(&visible).filter(|(_, visible)| **visible).map(|(weight, _)| *weight).sum();
        let min = MIN_WEIGHT_FRACTION * total;
        let adjustment = delta * total;
        let before_weight = weights[before];
        let after_weight = weights[after];
        let adjustment = adjustment.clamp(min - before_weight, after_weight - min);
        if adjustment.abs() < 1e-12 {
            return false;
        }
        weights[before] = before_weight + adjustment;
        weights[after] = after_weight - adjustment;
        true
    }

    /// Ids of every live node, in allocation order.
    pub fn live_ids(&self) -> impl Iterator<Item = NodeId> + '_ {
        self.slots.iter().enumerate().filter(|(_, slot)| slot.is_some()).map(|(index, _)| NodeId(index as u32))
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
        for node_id in reachable {
            match self.node(node_id) {
                Node::Split { children, weights, .. } => {
                    assert!(children.len() >= 2, "split {node_id:?} has {} children", children.len());
                    assert_eq!(children.len(), weights.len());
                    assert!(weights.iter().all(|weight| *weight > 0.0 && weight.is_finite()));
                    for child in children {
                        assert_eq!(self.parent(*child), Some(node_id), "bad parent link");
                    }
                }
                Node::Tile { views, selected } => {
                    assert!(!views.is_empty(), "empty tile {node_id:?}");
                    assert!(*selected < views.len());
                    for window in views {
                        assert!(seen.insert(*window), "window {window:?} appears twice");
                    }
                }
                Node::Desktop { floats, .. } => {
                    for window in floats {
                        assert!(seen.insert(*window), "window {window:?} appears twice");
                    }
                }
            }
        }
    }

    fn slot(&self, node_id: NodeId) -> &Slot {
        self.slots[node_id.0 as usize].as_ref().unwrap_or_else(|| panic!("node {node_id:?} was removed"))
    }

    fn slot_mut(&mut self, node_id: NodeId) -> &mut Slot {
        self.slots[node_id.0 as usize].as_mut().unwrap_or_else(|| panic!("node {node_id:?} was removed"))
    }

    fn alloc(&mut self, node: Node, parent: Option<NodeId>) -> NodeId {
        let node_id = NodeId(self.slots.len() as u32);
        self.slots.push(Some(Slot { node, parent }));
        node_id
    }

    fn free(&mut self, node_id: NodeId, replaced_by: NodeId) {
        self.slots[node_id.0 as usize] = None;
        self.redirect.insert(node_id, replaced_by);
    }

    fn insert_beside(&mut self, new: NodeId, target: NodeId, side: Side) {
        let axis = side.axis();
        if let Some(parent) = self.parent(target) {
            let visible: Vec<bool> = self.children(parent).iter().map(|child| self.is_visible(*child)).collect();
            let share = self.share_removed_space;
            if let Node::Split { axis: parent_axis, children, weights } = &mut self.slot_mut(parent).node
                && *parent_axis == axis
            {
                let idx = children.iter().position(|child| *child == target).expect("child of its parent");
                let weight = if share {
                    let shown: Vec<f64> = weights
                        .iter()
                        .zip(&visible)
                        .filter(|(_, visible)| **visible)
                        .map(|(weight, _)| *weight)
                        .collect();
                    shown.iter().sum::<f64>() / shown.len().max(1) as f64
                } else {
                    weights[idx] /= 2.0;
                    weights[idx]
                };
                let insertion_index = if side.is_leading() { idx } else { idx + 1 };
                children.insert(insertion_index, new);
                weights.insert(insertion_index, weight);
                self.slot_mut(new).parent = Some(parent);
                return;
            }
        }
        self.wrap_beside(new, target, side);
    }

    fn wrap_beside(&mut self, new: NodeId, target: NodeId, side: Side) {
        // Wrap the target in a new split that takes its place.
        let axis = side.axis();
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
            Some(parent_id) => {
                if let Node::Split { children, .. } = &mut self.slot_mut(parent_id).node {
                    let idx = children.iter().position(|child| *child == old).expect("child of its parent");
                    children[idx] = new;
                }
            }
            None => self.root = new,
        }
        self.slot_mut(new).parent = parent;
    }

    fn remove_node(&mut self, node_id: NodeId) {
        assert_ne!(node_id, self.desktop, "the Desktop is never removed");
        let parent = self.parent(node_id).expect("only the Desktop can be a parentless leaf");
        let share = self.share_removed_space;
        let heir = {
            let Node::Split { children, weights, .. } = &mut self.slot_mut(parent).node else { unreachable!() };
            let idx = children.iter().position(|child| *child == node_id).expect("child of its parent");
            children.remove(idx);
            let freed = weights.remove(idx);
            let heir_idx = idx.min(children.len() - 1);
            if !share {
                weights[heir_idx] += freed;
            }
            children[heir_idx]
        };
        self.free(node_id, heir);
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

        let Some(grandparent) = grand else { return };
        let (Node::Split { axis: parent_axis, .. }, Node::Split { axis: child_axis, .. }) =
            (self.node(grandparent), self.node(child))
        else {
            return;
        };
        if parent_axis != child_axis {
            return;
        }
        let Node::Split { children: child_children, weights: child_weights, .. } = self.node(child).clone() else {
            unreachable!()
        };
        let Node::Split { children, weights, .. } = &mut self.slot_mut(grandparent).node else { unreachable!() };
        let idx = children.iter().position(|candidate| *candidate == child).expect("child of its parent");
        let share = weights[idx];
        let sum: f64 = child_weights.iter().sum();
        children.splice(idx..=idx, child_children.iter().copied());
        weights.splice(idx..=idx, child_weights.iter().map(|weight| weight / sum * share));
        for child_id in child_children {
            self.slot_mut(child_id).parent = Some(grandparent);
        }
        self.free(child, grandparent);
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test setup and assertions should fail immediately on unexpected errors")]
mod tests {
    use super::*;

    fn window_id(number: u64) -> WindowId {
        WindowId(number)
    }

    #[test]
    fn starts_with_a_lone_visible_desktop() {
        let tree = Tree::new();
        tree.check_invariants();
        assert_eq!(tree.root(), tree.desktop());
        assert!(tree.desktop_visible());
    }

    #[test]
    fn docking_beside_the_desktop_hides_it() {
        let mut tree = Tree::new();
        let tile = tree.dock(window_id(1), tree.desktop(), Side::Right);
        tree.check_invariants();
        assert!(!tree.desktop_visible());
        assert_eq!(tree.tile_of(window_id(1)), Some(tile));
        assert!(matches!(tree.node(tree.root()), Node::Split { axis: Axis::Horizontal, .. }));
    }

    #[test]
    fn same_axis_dock_joins_the_split() {
        let mut tree = Tree::new();
        let first_tile = tree.dock(window_id(1), tree.desktop(), Side::Right);
        let second_tile = tree.dock(window_id(2), first_tile, Side::Right);
        tree.check_invariants();
        let root = tree.root();
        let Node::Split { children, weights, .. } = tree.node(root) else { panic!() };
        assert_eq!(children, &vec![tree.desktop(), first_tile, second_tile]);
        assert_eq!(weights[1], weights[2]);
    }

    #[test]
    fn cross_axis_dock_nests_a_split() {
        let mut tree = Tree::new();
        let first_tile = tree.dock(window_id(1), tree.desktop(), Side::Right);
        let second_tile = tree.dock(window_id(2), first_tile, Side::Bottom);
        tree.check_invariants();
        let parent = tree.parent(second_tile).unwrap();
        assert!(matches!(tree.node(parent), Node::Split { axis: Axis::Vertical, .. }));
        assert_eq!(tree.children(parent), &[first_tile, second_tile]);
    }

    fn shares(tree: &Tree, split: NodeId) -> Vec<f64> {
        let Node::Split { children, weights, .. } = tree.node(split) else { panic!() };
        let shown: Vec<(NodeId, f64)> = children
            .iter()
            .zip(weights)
            .filter(|(child, _)| tree.is_visible(**child))
            .map(|(child, weight)| (*child, *weight))
            .collect();
        let sum: f64 = shown.iter().map(|(_, weight)| weight).sum();
        shown.iter().map(|(_, weight)| weight / sum).collect()
    }

    #[test]
    fn closing_undoes_adding_when_sharing_space() {
        let mut tree = Tree::new();
        let first_tile = tree.dock(window_id(1), tree.desktop(), Side::Right);
        let second_tile = tree.dock(window_id(2), first_tile, Side::Right);
        tree.resize_gap(tree.root(), 1, 0.2);
        let before = shares(&tree, tree.root());
        tree.dock(window_id(3), second_tile, Side::Right);
        let with_c = shares(&tree, tree.root());
        assert!((with_c[2] - 1.0 / 3.0).abs() < 1e-9, "newcomer gets an equal share: {with_c:?}");
        tree.detach(window_id(3));
        tree.check_invariants();
        let after = shares(&tree, tree.root());
        for (before_weight, after_weight) in before.iter().zip(&after) {
            assert!((before_weight - after_weight).abs() < 1e-9, "{before:?} vs {after:?}");
        }
    }

    #[test]
    fn neighbour_takes_the_space_when_not_sharing() {
        let mut tree = Tree::new();
        tree.share_removed_space = false;
        let first_tile = tree.dock(window_id(1), tree.desktop(), Side::Right);
        let second_tile = tree.dock(window_id(2), first_tile, Side::Right);
        let third_tile = tree.dock(window_id(3), second_tile, Side::Right);
        assert_eq!(shares(&tree, tree.root()), vec![0.5, 0.25, 0.25]);
        tree.detach(window_id(2));
        tree.check_invariants();
        assert_eq!(shares(&tree, tree.root()), vec![0.5, 0.5]);
        assert_eq!(tree.resolve(second_tile), third_tile);
    }

    #[test]
    fn removing_collapses_and_redirects() {
        let mut tree = Tree::new();
        let first_tile = tree.dock(window_id(1), tree.desktop(), Side::Right);
        let second_tile = tree.dock(window_id(2), first_tile, Side::Bottom);
        let split = tree.parent(second_tile).unwrap();
        tree.detach(window_id(2));
        tree.check_invariants();
        assert!(!tree.contains(split));
        assert_eq!(tree.resolve(split), first_tile);
        assert_eq!(tree.resolve(second_tile), first_tile);
    }

    #[test]
    fn collapse_merges_same_axis_splits() {
        let mut tree = Tree::new();
        let first_tile = tree.dock(window_id(1), tree.desktop(), Side::Right);
        let second_tile = tree.dock(window_id(2), first_tile, Side::Bottom);
        let _third_tile = tree.dock(window_id(3), second_tile, Side::Right);
        // b and c share a horizontal split inside the vertical one; removing a
        // collapses the vertical split, and the horizontal one joins the root.
        tree.detach(window_id(1));
        tree.check_invariants();
        assert_eq!(tree.children(tree.root()).len(), 3);
    }

    #[test]
    fn tabs_and_selection() {
        let mut tree = Tree::new();
        let tile = tree.dock(window_id(1), tree.desktop(), Side::Right);
        tree.tab(window_id(2), tile);
        tree.tab(window_id(3), tile);
        assert_eq!(tree.shown_windows_in(tile), vec![window_id(3)]);
        tree.select_tab(window_id(1));
        assert_eq!(tree.shown_windows_in(tile), vec![window_id(1)]);
        tree.detach(window_id(1));
        tree.check_invariants();
        assert_eq!(tree.windows_in(tile), vec![window_id(2), window_id(3)]);
        tree.detach(window_id(2));
        tree.detach(window_id(3));
        tree.check_invariants();
        assert!(!tree.contains(tile));
        assert_eq!(tree.root(), tree.desktop());
    }

    #[test]
    fn floating_leaves_the_layout_alone() {
        let mut tree = Tree::new();
        tree.dock(window_id(1), tree.desktop(), Side::Right);
        tree.float(window_id(2));
        tree.float(window_id(3));
        assert!(!tree.desktop_visible());
        assert_eq!(tree.leaf_of(window_id(2)), Some(tree.desktop()));
        tree.raise_float(window_id(2));
        assert_eq!(tree.floats(), &[window_id(3), window_id(2)]);
        tree.detach(window_id(2));
        assert_eq!(tree.floats(), &[window_id(3)]);
    }

    #[test]
    fn boundaries_and_axis_toggle() {
        let mut tree = Tree::new();
        let first_tile = tree.dock(window_id(1), tree.desktop(), Side::Right);
        let second_tile = tree.dock(window_id(2), first_tile, Side::Right);
        let third_tile = tree.dock(window_id(3), second_tile, Side::Bottom);
        let root = tree.root();
        // a's left neighbour is the hidden Desktop: no boundary there.
        assert_eq!(tree.boundary(first_tile, Side::Left), None);
        assert_eq!(tree.boundary(first_tile, Side::Right), Some((root, 1)));
        assert_eq!(tree.boundary(third_tile, Side::Left), Some((root, 1)));
        let col = tree.parent(third_tile).unwrap();
        assert_eq!(tree.boundary(third_tile, Side::Top), Some((col, 0)));
        assert_eq!(tree.boundary(third_tile, Side::Bottom), None);
        assert!(tree.toggle_axis(col));
        assert!(matches!(tree.node(col), Node::Split { axis: Axis::Horizontal, .. }));
    }

    #[test]
    fn resize_gap_clamps() {
        let mut tree = Tree::new();
        let tile = tree.dock(window_id(1), tree.desktop(), Side::Right);
        tree.dock(window_id(2), tile, Side::Right);
        let root = tree.root();
        // Only the hidden Desktop is before index 0: nothing to resize there.
        assert!(!tree.resize_gap(root, 0, 0.1));
        assert!(tree.resize_gap(root, 1, 0.9));
        let Node::Split { weights, .. } = tree.node(root) else { panic!() };
        let total = weights[1] + weights[2];
        assert!((weights[2] / total - MIN_WEIGHT_FRACTION).abs() < 1e-9);
    }

    /// A small deterministic generator, so the random-edit test needs no crates.
    struct Lcg(u64);
    impl Lcg {
        fn next(&mut self, limit: usize) -> usize {
            self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            ((self.0 >> 33) % limit as u64) as usize
        }
    }

    #[test]
    fn random_edits_keep_invariants() {
        for seed in 0..200 {
            let mut rng = Lcg(seed);
            let mut tree = Tree::new();
            tree.share_removed_space = seed % 2 == 0;
            let mut live: Vec<WindowId> = Vec::new();
            let mut next = 1;
            for _ in 0..60 {
                match rng.next(5) {
                    0 | 1 => {
                        let leaves = tree.leaves(tree.root());
                        let target = leaves[rng.next(leaves.len())];
                        let side = [Side::Left, Side::Right, Side::Top, Side::Bottom][rng.next(4)];
                        tree.dock(window_id(next), target, side);
                        live.push(window_id(next));
                        next += 1;
                    }
                    2 => {
                        let tiles: Vec<NodeId> =
                            tree.leaves(tree.root()).into_iter().filter(|node_id| *node_id != tree.desktop()).collect();
                        if tiles.is_empty() {
                            tree.float(window_id(next));
                        } else {
                            tree.tab(window_id(next), tiles[rng.next(tiles.len())]);
                        }
                        live.push(window_id(next));
                        next += 1;
                    }
                    _ => {
                        if !live.is_empty() {
                            let index = rng.next(live.len());
                            assert!(tree.detach(live.remove(index)));
                        }
                    }
                }
                tree.check_invariants();
                let mut all = tree.windows_in(tree.root());
                all.sort();
                let mut expect = live.clone();
                expect.sort();
                assert_eq!(all, expect);
            }
        }
    }
}
