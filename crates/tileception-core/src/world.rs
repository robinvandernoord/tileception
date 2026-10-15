//! One world per display: the tree plus the camera, focus, history and saved
//! views. Everything here is deterministic; the caller passes the time in.

use std::collections::HashMap;

use crate::geom::{Point, Rect, Xform};
use crate::layout::Layout;
use crate::tree::{Axis, Node, NodeId, Side, Tree, WindowId};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Dir {
    Left,
    Right,
    Up,
    Down,
}

impl Dir {
    fn axis(self) -> Axis {
        match self {
            Dir::Left | Dir::Right => Axis::Horizontal,
            Dir::Up | Dir::Down => Axis::Vertical,
        }
    }

    fn forward(self) -> bool {
        matches!(self, Dir::Right | Dir::Down)
    }
}

#[derive(Debug, Clone)]
pub struct Settings {
    /// Pixels between windows and around the framed space.
    pub gap: f64,
    /// Seconds a camera move takes.
    pub animation: f64,
    pub share_removed_space: bool,
    /// Height in pixels of the strip of tabs above a tile with several windows.
    pub tab_height: f64,
}

impl Default for Settings {
    fn default() -> Self {
        Settings { gap: 12.0, animation: 0.28, share_removed_space: true, tab_height: 28.0 }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Placement {
    /// Dock beside the focused tile.
    Dock,
    /// Float above the layout.
    Float,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowRole {
    Tiled,
    Floating,
}

/// Where a shown window goes on screen.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WindowPlacement {
    pub window: WindowId,
    pub leaf: NodeId,
    pub rect: Rect,
    pub role: WindowRole,
}

/// What happens when a dragged window is let go.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DropTarget {
    /// Dock beside a node, on one side.
    Edge(NodeId, Side),
    /// Become a tab of a tile.
    Tab(NodeId),
    /// Float above the layout, where it was let go.
    Float,
}

/// The strip of tabs above a tile that holds several windows.
#[derive(Debug, Clone, PartialEq)]
pub struct TabStrip {
    pub tile: NodeId,
    pub rect: Rect,
    pub views: Vec<WindowId>,
    pub selected: usize,
}

/// Smallest width or height a floating window is resized to, in pixels.
const MIN_FLOAT: f64 = 48.0;

/// How far into a tile, as a fraction of its size, an edge drop reaches.
const EDGE_BAND: f64 = 0.25;
/// Pixels from the edge of the framed space that dock beside all of it.
const SCREEN_EDGE_BAND: f64 = 16.0;

#[derive(Debug, Clone, Copy)]
struct Float {
    rect: Rect,
    /// False until the client has chosen its own size.
    sized: bool,
}

#[derive(Debug, Clone)]
struct SavedView {
    node: NodeId,
    windows: Vec<WindowId>,
}

#[derive(Debug, Clone, Copy)]
struct Camera {
    from: Rect,
    to: Rect,
    start: f64,
    duration: f64,
}

impl Camera {
    fn at(&self, now: f64) -> Rect {
        let t = self.progress(now);
        // Ease out cubic: quick to respond, gentle to land.
        let e = 1.0 - (1.0 - t).powi(3);
        self.from.zoom_lerp(&self.to, e)
    }

    fn progress(&self, now: f64) -> f64 {
        if self.duration <= 0.0 {
            1.0
        } else {
            ((now - self.start) / self.duration).clamp(0.0, 1.0)
        }
    }
}

#[derive(Debug, Clone)]
pub struct World {
    pub settings: Settings,
    tree: Tree,
    layout: Layout,
    framed: NodeId,
    overview: bool,
    history: Vec<NodeId>,
    history_index: usize,
    saved: [Option<SavedView>; 9],
    focus: Option<WindowId>,
    mru: Vec<WindowId>,
    camera: Camera,
    /// Floating windows, in screen pixels.
    floats: HashMap<WindowId, Float>,
    /// Where docked windows floated last, to float them there again.
    last_float: HashMap<WindowId, Rect>,
    /// The output area the framed space fills, in pixels.
    screen: Rect,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct LayoutSnapshot {
    pub root: crate::tree::LayoutNode,
    pub floats: Vec<(WindowId, Rect)>,
    pub framed: Vec<WindowId>,
    pub saved: Vec<Vec<WindowId>>,
}

impl World {
    pub fn snapshot(&self) -> LayoutSnapshot {
        LayoutSnapshot {
            root: self.tree.snapshot(),
            floats: self.floats.iter().map(|(id, f)| (*id, f.rect)).collect(),
            framed: self.tree.windows_in(self.framed),
            saved: self.saved.iter().map(|slot| slot.as_ref().map(|s| s.windows.clone()).unwrap_or_default()).collect(),
        }
    }

    pub fn restore_layout(&mut self, snapshot: &LayoutSnapshot, remap: &HashMap<WindowId, WindowId>, now: f64) -> bool {
        let Some(tree) = Tree::restore(&snapshot.root, remap) else { return false };
        if snapshot
            .floats
            .iter()
            .any(|(_, r)| ![r.x, r.y, r.w, r.h].iter().all(|n| n.is_finite()) || r.w <= 0.0 || r.h <= 0.0)
        {
            return false;
        }
        self.tree = tree;
        self.floats.clear();
        for (old, rect) in &snapshot.floats {
            if let Some(id) = remap.get(old).filter(|w| self.tree.is_floating(**w)) {
                self.floats.insert(*id, Float { rect: *rect, sized: true });
            }
        }
        for id in self.tree.floats() {
            if !self.floats.contains_key(id) {
                self.floats.insert(*id, Float { rect: self.default_float_rect(), sized: true });
            }
        }
        let surviving = |ids: &[WindowId]| -> Vec<WindowId> {
            ids.iter().filter_map(|id| remap.get(id).copied()).filter(|id| self.tree.leaf_of(*id).is_some()).collect()
        };
        let common = |windows: &[WindowId]| -> NodeId {
            windows
                .iter()
                .filter_map(|id| self.tree.leaf_of(*id))
                .reduce(|a, b| self.tree.common_ancestor(a, b))
                .unwrap_or(self.tree.root())
        };
        self.framed = common(&surviving(&snapshot.framed));
        self.saved = std::array::from_fn(|i| {
            snapshot.saved.get(i).filter(|ids| !ids.is_empty()).and_then(|ids| {
                let windows = surviving(ids);
                (!windows.is_empty()).then(|| SavedView { node: common(&windows), windows })
            })
        });
        self.focus = None;
        self.mru.clear();
        self.history = vec![self.framed];
        self.history_index = 0;
        self.overview = false;
        self.relayout();
        self.retarget(now);
        true
    }

    pub fn new(settings: Settings, screen: Rect) -> World {
        let mut tree = Tree::new();
        tree.share_removed_space = settings.share_removed_space;
        let root = tree.root();
        let layout = Layout::compute(&tree);
        World {
            settings,
            tree,
            layout,
            framed: root,
            overview: false,
            history: vec![root],
            history_index: 0,
            saved: Default::default(),
            focus: None,
            mru: Vec::new(),
            camera: Camera { from: Rect::UNIT, to: Rect::UNIT, start: 0.0, duration: 0.0 },
            floats: HashMap::new(),
            last_float: HashMap::new(),
            screen,
        }
    }

    pub fn tree(&self) -> &Tree {
        &self.tree
    }

    pub fn layout(&self) -> &Layout {
        &self.layout
    }

    pub fn framed(&self) -> NodeId {
        self.framed
    }

    pub fn in_overview(&self) -> bool {
        self.overview
    }

    pub fn focus(&self) -> Option<WindowId> {
        self.focus
    }

    pub fn screen(&self) -> Rect {
        self.screen
    }

    pub fn set_screen(&mut self, screen: Rect) {
        self.screen = screen;
    }

    pub fn contains_window(&self, win: WindowId) -> bool {
        self.tree.leaf_of(win).is_some()
    }

    // ----- camera -----

    /// The part of the world (in layout units) on screen at time `now`.
    pub fn view(&self, now: f64) -> Rect {
        self.camera.at(now)
    }

    /// Where the camera is heading.
    pub fn target_view(&self) -> Rect {
        self.camera.to
    }

    pub fn is_animating(&self, now: f64) -> bool {
        self.camera.progress(now) < 1.0
    }

    /// Jumps to the end of the current camera move.
    pub fn finish_animation(&mut self) {
        self.camera.from = self.camera.to;
        self.camera.duration = 0.0;
    }

    /// The area the framed space fills: the screen less the outer half gap.
    /// Each tile then insets another half gap, so every gap is `gap` wide.
    fn frame_area(&self) -> Rect {
        self.screen.inset(self.settings.gap / 2.0)
    }

    pub fn xform(&self, view: Rect) -> Xform {
        Xform::view_to_screen(&view, &self.frame_area())
    }

    fn wanted_view(&self) -> Rect {
        let node = if self.overview { self.tree.root() } else { self.framed };
        self.layout.rect(node).unwrap_or(Rect::UNIT)
    }

    fn retarget(&mut self, now: f64) {
        let to = self.wanted_view();
        let from = self.view(now);
        if from.approx_eq(&to, 1e-9) {
            self.camera = Camera { from: to, to, start: now, duration: 0.0 };
        } else {
            self.camera = Camera { from, to, start: now, duration: self.settings.animation };
        }
    }

    // ----- placement -----

    /// Screen rectangles of every shown window for a given view, including
    /// windows outside the screen. Tiles are inset by half a gap.
    pub fn placements(&self, view: Rect) -> Vec<WindowPlacement> {
        let x = self.xform(view);
        let half_gap = self.settings.gap / 2.0;
        let mut out = Vec::new();
        for leaf in self.tree.leaves(self.tree.root()) {
            let Some(unit) = self.layout.rect(leaf) else { continue };
            let tile = x.apply(&unit).inset(half_gap);
            match self.tree.node(leaf) {
                Node::Tile { views, selected } => {
                    let strip = if views.len() > 1 { self.strip_height(&tile) } else { 0.0 };
                    out.push(WindowPlacement {
                        window: views[*selected],
                        leaf,
                        rect: Rect::new(tile.x, tile.y + strip, tile.w, tile.h - strip),
                        role: WindowRole::Tiled,
                    })
                }
                Node::Desktop { .. } => {}
                Node::Split { .. } => unreachable!("leaves only"),
            }
        }
        // Floating windows hang above the layout, in screen pixels, whatever
        // the camera does.
        let desktop = self.tree.desktop();
        for w in self.tree.floats() {
            if let Some(f) = self.floats.get(w) {
                out.push(WindowPlacement { window: *w, leaf: desktop, rect: f.rect, role: WindowRole::Floating });
            }
        }
        out
    }

    // ----- floating -----

    pub fn float_rect(&self, win: WindowId) -> Option<Rect> {
        self.floats.get(&win).map(|f| f.rect)
    }

    /// Whether a floating window still waits for the client to pick a size.
    pub fn float_needs_size(&self, win: WindowId) -> bool {
        self.floats.get(&win).is_some_and(|f| !f.sized)
    }

    /// Takes the size the client chose, centred on the screen.
    pub fn size_float(&mut self, win: WindowId, w: f64, h: f64) {
        let screen = self.screen;
        if let Some(f) = self.floats.get_mut(&win) {
            let (w, h) = (w.min(screen.w), h.min(screen.h));
            f.rect = Rect::new(screen.x + (screen.w - w) / 2.0, screen.y + (screen.h - h) / 2.0, w, h);
            f.sized = true;
        }
    }

    pub fn set_float_rect(&mut self, win: WindowId, rect: Rect) {
        if let Some(f) = self.floats.get_mut(&win) {
            f.rect = Rect::new(rect.x, rect.y, rect.w.max(MIN_FLOAT), rect.h.max(MIN_FLOAT));
            f.sized = true;
        }
    }

    fn strip_height(&self, tile: &Rect) -> f64 {
        self.settings.tab_height.min(tile.h * 0.25)
    }

    /// Tab strips of tiles holding several windows, for a given view.
    pub fn tab_strips(&self, view: Rect) -> Vec<TabStrip> {
        let x = self.xform(view);
        let half_gap = self.settings.gap / 2.0;
        let mut out = Vec::new();
        for leaf in self.tree.leaves(self.tree.root()) {
            let Node::Tile { views, selected } = self.tree.node(leaf) else { continue };
            if views.len() < 2 {
                continue;
            }
            let Some(unit) = self.layout.rect(leaf) else { continue };
            let tile = x.apply(&unit).inset(half_gap);
            let rect = Rect::new(tile.x, tile.y, tile.w, self.strip_height(&tile));
            out.push(TabStrip { tile: leaf, rect, views: views.clone(), selected: *selected });
        }
        out
    }

    /// Where `dragged` would land if let go at screen point `p`.
    pub fn drop_target(&self, p: Point, dragged: WindowId, view: Rect) -> Option<DropTarget> {
        let area = self.frame_area();
        let x = self.xform(view);
        // Right at the edge of the framed space: dock beside all of it.
        if !self.overview && area.contains(p) {
            let side = [
                (p.x - area.x, Side::Left),
                (area.right() - p.x, Side::Right),
                (p.y - area.y, Side::Top),
                (area.bottom() - p.y, Side::Bottom),
            ]
            .into_iter()
            .filter(|(d, _)| *d < SCREEN_EDGE_BAND)
            .min_by(|a, b| a.0.total_cmp(&b.0))
            .map(|(_, s)| s);
            if let Some(side) = side {
                return Some(DropTarget::Edge(self.framed, side));
            }
        }
        let leaf = self.leaf_at(p, view)?;
        if leaf == self.tree.desktop() {
            return Some(DropTarget::Float);
        }
        let r = x.apply(&self.layout.rect(leaf)?);
        let fx = (p.x - r.x) / r.w;
        let fy = (p.y - r.y) / r.h;
        let (d, side) = [(fx, Side::Left), (1.0 - fx, Side::Right), (fy, Side::Top), (1.0 - fy, Side::Bottom)]
            .into_iter()
            .min_by(|a, b| a.0.total_cmp(&b.0))
            .expect("four sides");
        let own_lone_tile = self.tree.tile_of(dragged) == Some(leaf) && self.tree.windows_in(leaf).len() == 1;
        if own_lone_tile {
            return None;
        }
        if d < EDGE_BAND {
            Some(DropTarget::Edge(leaf, side))
        } else if self.tree.tile_of(dragged) == Some(leaf) {
            None
        } else {
            Some(DropTarget::Tab(leaf))
        }
    }

    /// The screen area a drop would fill, to preview it while dragging.
    pub fn drop_preview(&self, target: DropTarget, view: Rect) -> Option<Rect> {
        let half_gap = self.settings.gap / 2.0;
        match target {
            DropTarget::Edge(node, side) => {
                let r = self.node_rect(self.tree.resolve(node), view)?;
                let part = match side {
                    Side::Left => Rect::new(r.x, r.y, r.w / 2.0, r.h),
                    Side::Right => Rect::new(r.x + r.w / 2.0, r.y, r.w / 2.0, r.h),
                    Side::Top => Rect::new(r.x, r.y, r.w, r.h / 2.0),
                    Side::Bottom => Rect::new(r.x, r.y + r.h / 2.0, r.w, r.h / 2.0),
                };
                Some(part.inset(half_gap))
            }
            DropTarget::Tab(tile) => self.node_rect(tile, view).map(|r| r.inset(half_gap)),
            DropTarget::Float => self.node_rect(self.tree.desktop(), view).map(|r| r.inset(half_gap)),
        }
    }

    pub fn apply_drop(&mut self, win: WindowId, target: DropTarget, now: f64) {
        match target {
            DropTarget::Edge(node, side) => self.move_window(win, node, side, now),
            DropTarget::Tab(tile) => {
                let tile = self.tree.resolve(tile);
                if self.tree.tile_of(win) == Some(tile) || !matches!(self.tree.node(tile), Node::Tile { .. }) {
                    return;
                }
                // Detaching cannot remove the target tile, which holds other windows.
                let keep = self.tree.windows_in(tile)[0];
                self.tree.detach(win);
                self.floats.remove(&win);
                let tile = self.tree.tile_of(keep).expect("target tile survives");
                self.tree.tab(win, tile);
                self.after_move(win, now);
            }
            DropTarget::Float => {
                if !self.tree.is_floating(win) {
                    self.tree.detach(win);
                    self.relayout();
                    self.add_float(win, None);
                    self.after_move(win, now);
                }
            }
        }
    }

    /// Moves the focused window past its neighbour in a direction, or to that
    /// edge of the whole layout when there is no neighbour.
    pub fn move_focused(&mut self, dir: Dir, now: f64) {
        let Some(win) = self.focus else { return };
        let Some(leaf) = self.tree.tile_of(win) else { return };
        let Some(cur) = self.layout.rect(leaf) else { return };
        let side = match dir {
            Dir::Left => Side::Left,
            Dir::Right => Side::Right,
            Dir::Up => Side::Top,
            Dir::Down => Side::Bottom,
        };
        const EPS: f64 = 1e-9;
        let best = self
            .tree
            .leaves(self.tree.root())
            .into_iter()
            .filter(|l| *l != leaf)
            .filter_map(|l| self.layout.rect(l).map(|r| (l, r)))
            .filter_map(|(l, r)| {
                let (gap, overlap) = match dir {
                    Dir::Right => (r.x - cur.right(), r.y < cur.bottom() && cur.y < r.bottom()),
                    Dir::Left => (cur.x - r.right(), r.y < cur.bottom() && cur.y < r.bottom()),
                    Dir::Down => (r.y - cur.bottom(), r.x < cur.right() && cur.x < r.right()),
                    Dir::Up => (cur.y - r.bottom(), r.x < cur.right() && cur.x < r.right()),
                };
                (gap > -EPS && overlap).then(|| {
                    let off = match dir.axis() {
                        Axis::Horizontal => (r.center().y - cur.center().y).abs(),
                        Axis::Vertical => (r.center().x - cur.center().x).abs(),
                    };
                    (l, gap, off)
                })
            })
            .min_by(|a, b| a.1.total_cmp(&b.1).then(a.2.total_cmp(&b.2)))
            .map(|(l, _, _)| l);
        let target = best.unwrap_or(self.tree.root());
        self.move_window(win, target, side, now);
    }

    /// Move a selection past an unselected neighbour, preserving its spatial order.
    pub fn move_windows(&mut self, windows: &[WindowId], dir: Dir, now: f64) {
        let side = match dir {
            Dir::Left => Side::Left,
            Dir::Right => Side::Right,
            Dir::Up => Side::Top,
            Dir::Down => Side::Bottom,
        };
        let mut tiled = Vec::new();
        for id in windows {
            if let Some(mut rect) = self.float_rect(*id) {
                match dir {
                    Dir::Left => rect.x -= 40.0,
                    Dir::Right => rect.x += 40.0,
                    Dir::Up => rect.y -= 40.0,
                    Dir::Down => rect.y += 40.0,
                }
                self.set_float_rect(*id, rect);
            } else if let Some(rect) = self.tree.tile_of(*id).and_then(|node| self.layout.rect(node)) {
                tiled.push((*id, rect));
            }
        }
        if tiled.is_empty() {
            return;
        }
        let x = tiled.iter().map(|(_, r)| r.x).fold(f64::INFINITY, f64::min);
        let y = tiled.iter().map(|(_, r)| r.y).fold(f64::INFINITY, f64::min);
        let right = tiled.iter().map(|(_, r)| r.right()).fold(f64::NEG_INFINITY, f64::max);
        let bottom = tiled.iter().map(|(_, r)| r.bottom()).fold(f64::NEG_INFINITY, f64::max);
        let target = self.tree.leaves(self.tree.root()).into_iter()
            .filter(|node| matches!(self.tree.node(*node), Node::Tile { views, .. } if !views.iter().any(|w| windows.contains(w))))
            .filter_map(|node| self.layout.rect(node).map(|rect| (node, rect)))
            .filter_map(|(node, r)| {
                let (gap, overlaps) = match dir {
                    Dir::Left => (x - r.right(), r.y < bottom && y < r.bottom()),
                    Dir::Right => (r.x - right, r.y < bottom && y < r.bottom()),
                    Dir::Up => (y - r.bottom(), r.x < right && x < r.right()),
                    Dir::Down => (r.y - bottom, r.x < right && x < r.right()),
                };
                (gap >= -1e-9 && overlaps).then_some((node, gap))
            }).min_by(|a, b| a.1.total_cmp(&b.1)).map(|(node, _)| node);
        let Some(target) = target else { return };
        let target = self.tree.windows_in(target).into_iter().find(|id| !windows.contains(id));
        let moved: Vec<_> = tiled.into_iter().map(|(id, _)| id).collect();
        if let Some(target) = target {
            if self.tree.move_group(&moved, target, side) {
                self.after_move(moved[0], now);
            }
        }
    }

    /// Screen rectangle of a node for a given view, without gap insets.
    pub fn node_rect(&self, node: NodeId, view: Rect) -> Option<Rect> {
        self.layout.rect(node).map(|r| self.xform(view).apply(&r))
    }

    /// The leaf under a screen point for a given view.
    pub fn leaf_at(&self, p: Point, view: Rect) -> Option<NodeId> {
        let world = self.xform(view).invert_point(p);
        self.tree.leaves(self.tree.root()).into_iter().find(|l| self.layout.rect(*l).is_some_and(|r| r.contains(world)))
    }

    // ----- edits -----

    fn relayout(&mut self) {
        self.layout = Layout::compute(&self.tree);
        self.framed = self.visible_ancestor(self.tree.resolve(self.framed));
    }

    fn visible_ancestor(&self, mut id: NodeId) -> NodeId {
        while self.layout.rect(id).is_none() {
            match self.tree.parent(id) {
                Some(p) => id = p,
                None => return self.tree.root(),
            }
        }
        id
    }

    /// Adds a new window and focuses it.
    pub fn add_window(&mut self, win: WindowId, placement: Placement, now: f64) {
        if self.contains_window(win) {
            return;
        }
        match placement {
            Placement::Float => {
                self.tree.float(win);
                let r = self.default_float_rect();
                self.floats.insert(win, Float { rect: r, sized: false });
            }
            Placement::Dock => {
                let target = self.dock_target();
                let side = self.split_side(target);
                self.tree.dock(win, target, side);
            }
        }
        self.relayout();
        let leaf = self.tree.leaf_of(win).expect("just added");
        if !self.tree.is_ancestor_or_self(self.framed, leaf) {
            let common = self.tree.common_ancestor(self.framed, leaf);
            self.push_frame(common);
        }
        self.set_focus(Some(win));
        self.retarget(now);
    }

    /// The leaf a new window docks beside: the focused one when it is in view,
    /// otherwise the framed space as a whole.
    fn dock_target(&self) -> NodeId {
        if let Some(leaf) = self.focus.and_then(|f| self.tree.leaf_of(f)) {
            if leaf != self.tree.desktop() && self.tree.is_ancestor_or_self(self.framed, leaf) {
                return leaf;
            }
        }
        self.framed
    }

    /// Splits along the longer side of the target as it would be on screen.
    fn split_side(&self, target: NodeId) -> Side {
        let unit = self.layout.rect(target).unwrap_or(Rect::UNIT);
        let framed = self.layout.rect(self.framed).unwrap_or(Rect::UNIT);
        let area = self.frame_area();
        let w = unit.w / framed.w * area.w;
        let h = unit.h / framed.h * area.h;
        if w >= h {
            Side::Right
        } else {
            Side::Bottom
        }
    }

    pub fn remove_window(&mut self, win: WindowId, now: f64) {
        if !self.tree.detach(win) {
            return;
        }
        self.floats.remove(&win);
        self.last_float.remove(&win);
        self.mru.retain(|w| *w != win);
        self.relayout();
        if self.focus == Some(win) {
            self.focus = None;
            let shown = self.tree.shown_windows_in(self.framed);
            let next = self.mru.iter().rev().find(|w| shown.contains(w)).copied().or(shown.first().copied());
            self.set_focus(next);
        }
        self.retarget(now);
    }

    /// Moves a window to dock beside `target` on `side`.
    pub fn move_window(&mut self, win: WindowId, target: NodeId, side: Side, now: f64) {
        let target = self.tree.resolve(target);
        if self.tree.leaf_of(win) == Some(target) && self.tree.windows_in(target).len() == 1 {
            return;
        }
        self.tree.detach(win);
        self.floats.remove(&win);
        // Detaching may collapse the target's split; resolve finds what replaced it.
        let target = self.tree.resolve(target);
        self.tree.dock(win, target, side);
        self.after_move(win, now);
    }

    /// Keeps a moved window in view and focused.
    fn after_move(&mut self, win: WindowId, now: f64) {
        self.relayout();
        if let Some(leaf) = self.tree.leaf_of(win) {
            if !self.overview && !self.tree.is_ancestor_or_self(self.framed, leaf) {
                let common = self.tree.common_ancestor(self.framed, leaf);
                self.push_frame(common);
            }
        }
        self.set_focus(Some(win));
        self.retarget(now);
    }

    pub fn toggle_float(&mut self, win: WindowId, now: f64) {
        if self.tree.is_floating(win) {
            self.tree.detach(win);
            if let Some(f) = self.floats.remove(&win) {
                self.last_float.insert(win, f.rect);
            }
            let target = self.dock_target();
            let side = self.split_side(target);
            self.tree.dock(win, target, side);
        } else if self.contains_window(win) {
            self.tree.detach(win);
            self.relayout();
            self.add_float(win, None);
        }
        self.relayout();
        self.set_focus(Some(win));
        self.retarget(now);
    }

    /// Floats a window: at `rect`, where it floated before, or centred.
    fn add_float(&mut self, win: WindowId, rect: Option<Rect>) {
        let rect = rect.or_else(|| self.last_float.get(&win).copied()).unwrap_or_else(|| self.default_float_rect());
        self.floats.insert(win, Float { rect, sized: true });
        self.tree.float(win);
    }

    /// Centred, a bit over half the screen, cascading for each extra window.
    fn default_float_rect(&self) -> Rect {
        let s = self.screen;
        let off = (self.tree.floats().len() as f64 * 32.0) % 160.0;
        Rect::new(s.x + s.w * 0.2 + off, s.y + s.h * 0.2 + off, s.w * 0.6, s.h * 0.6)
    }

    /// Shows or hides the Desktop's tile in the layout.
    pub fn toggle_desktop(&mut self, now: f64) {
        let shown = self.tree.desktop_shown();
        self.tree.set_desktop_shown(!shown);
        self.relayout();
        self.retarget(now);
    }

    /// Moves a split boundary by a distance in screen pixels.
    pub fn resize_gap(&mut self, split: NodeId, index: usize, pixels: f64, now: f64) -> bool {
        let Some(rect) = self.node_rect(split, self.target_view()) else { return false };
        let Node::Split { axis, .. } = self.tree.node(split) else { return false };
        let size = match axis {
            Axis::Horizontal => rect.w,
            Axis::Vertical => rect.h,
        };
        if size <= 0.0 || !self.tree.resize_gap(split, index, pixels / size) {
            return false;
        }
        self.relayout();
        // Keep the camera on the framed node, which may have changed size.
        let to = self.wanted_view();
        self.camera = Camera { from: to, to, start: now, duration: 0.0 };
        true
    }

    // ----- focus -----

    fn set_focus(&mut self, win: Option<WindowId>) {
        self.focus = win;
        if let Some(w) = win {
            self.mru.retain(|x| *x != w);
            self.mru.push(w);
            self.tree.select_tab(w);
            self.tree.raise_float(w);
        }
    }

    /// Focuses the next (or previous) window in layout order, floating
    /// windows last, bringing it into view.
    pub fn cycle(&mut self, forward: bool, now: f64) {
        let all = self.tree.windows_in(self.tree.root());
        if all.is_empty() {
            return;
        }
        let i = self.focus.and_then(|f| all.iter().position(|w| *w == f));
        let n = all.len();
        let next = match (i, forward) {
            (Some(i), true) => all[(i + 1) % n],
            (Some(i), false) => all[(i + n - 1) % n],
            (None, _) => all[0],
        };
        self.focus_window(next, now);
    }

    /// Stacks the focused tile's row, or lays its column side by side.
    pub fn toggle_split(&mut self, now: f64) {
        let Some(tile) = self.focus.and_then(|f| self.tree.tile_of(f)) else { return };
        let Some(parent) = self.tree.parent(tile) else { return };
        self.tree.toggle_axis(parent);
        self.relayout();
        self.retarget(now);
    }

    /// Moves the edges of a window by pixel amounts: `dx` moves the edge on
    /// `side_x`, `dy` the edge on `side_y`. Tiled windows move the split
    /// boundaries on those sides; floating windows change their rectangle.
    pub fn resize_window(&mut self, win: WindowId, side_x: Side, dx: f64, side_y: Side, dy: f64, now: f64) -> bool {
        if let Some(f) = self.floats.get(&win).copied() {
            let mut r = f.rect;
            match side_x {
                Side::Left => {
                    let d = dx.min(r.w - MIN_FLOAT);
                    r.x += d;
                    r.w -= d;
                }
                _ => r.w = (r.w + dx).max(MIN_FLOAT),
            }
            match side_y {
                Side::Top => {
                    let d = dy.min(r.h - MIN_FLOAT);
                    r.y += d;
                    r.h -= d;
                }
                _ => r.h = (r.h + dy).max(MIN_FLOAT),
            }
            self.set_float_rect(win, r);
            return true;
        }
        let Some(leaf) = self.tree.tile_of(win) else { return false };
        let mut changed = false;
        for (side, d) in [(side_x, dx), (side_y, dy)] {
            if d == 0.0 {
                continue;
            }
            if let Some((split, index)) = self.tree.boundary(leaf, side) {
                changed |= self.resize_gap(split, index, d, now);
            }
        }
        changed
    }

    /// Moves the dividers around the focused window by pixels: positive `dx`
    /// moves a divider right, positive `dy` down. The divider moved is the one
    /// on the window's right (bottom) when it has a neighbour there, otherwise
    /// the one on its left (top). Either way, the line moves the way the arrow
    /// points; whether the window grows or shrinks follows from that.
    pub fn resize_focused(&mut self, dw: f64, dh: f64, now: f64) -> bool {
        let Some(win) = self.focus else { return false };
        let leaf = self.tree.tile_of(win);
        let pick = |trailing: Side, leading: Side, d: f64| match leaf {
            Some(l) if self.tree.boundary(l, trailing).is_none() && self.tree.boundary(l, leading).is_some() => {
                (leading, d)
            }
            _ => (trailing, d),
        };
        let (sx, dx) = pick(Side::Right, Side::Left, dw);
        let (sy, dy) = pick(Side::Bottom, Side::Top, dh);
        self.resize_window(win, sx, dx, sy, dy, now)
    }

    /// Focuses a window, bringing it into view if it is outside the framed space.
    pub fn focus_window(&mut self, win: WindowId, now: f64) {
        let Some(leaf) = self.tree.leaf_of(win) else { return };
        let was_hidden_tab = !self.tree.shown_windows_in(leaf).contains(&win);
        self.set_focus(Some(win));
        if was_hidden_tab {
            self.relayout();
        }
        if !self.overview && !self.tree.is_ancestor_or_self(self.framed, leaf) {
            self.push_frame(leaf);
            self.retarget(now);
        }
    }

    // ----- navigation -----

    fn push_frame(&mut self, node: NodeId) {
        let node = self.visible_ancestor(node);
        self.overview = false;
        if self.framed == node {
            return;
        }
        self.framed = node;
        self.history.truncate(self.history_index + 1);
        self.history.push(node);
        self.history_index = self.history.len() - 1;
    }

    /// Frames a node, recording it in history.
    pub fn frame(&mut self, node: NodeId, now: f64) {
        self.push_frame(self.tree.resolve(node));
        self.focus_inside_framed();
        self.retarget(now);
    }

    fn focus_inside_framed(&mut self) {
        let shown = self.tree.shown_windows_in(self.framed);
        if self.focus.is_some_and(|f| shown.contains(&f)) {
            return;
        }
        let pick = self.mru.iter().rev().find(|w| shown.contains(w)).copied().or(shown.first().copied());
        if pick.is_some() {
            self.set_focus(pick);
        }
    }

    /// Steps into the child of the framed space that holds the focused window
    /// (or the point, when given in screen coordinates).
    pub fn step_in(&mut self, toward: Option<Point>, now: f64) {
        if self.overview {
            self.overview = false;
            self.retarget(now);
            return;
        }
        let children: Vec<NodeId> =
            self.tree.children(self.framed).iter().copied().filter(|c| self.layout.rect(*c).is_some()).collect();
        if children.is_empty() {
            return;
        }
        let by_point = toward.and_then(|p| {
            let world = self.xform(self.target_view()).invert_point(p);
            children.iter().copied().find(|c| self.layout.rect(*c).is_some_and(|r| r.contains(world)))
        });
        let by_focus = self
            .focus
            .and_then(|f| self.tree.leaf_of(f))
            .and_then(|leaf| children.iter().copied().find(|c| self.tree.is_ancestor_or_self(*c, leaf)));
        let child = by_point.or(by_focus).unwrap_or(children[0]);
        self.frame(child, now);
    }

    pub fn step_out(&mut self, now: f64) {
        if self.overview {
            return;
        }
        if let Some(p) = self.tree.parent(self.framed) {
            self.frame(p, now);
        }
    }

    /// Frames the neighbouring space in a direction, at the same depth where
    /// possible.
    pub fn neighbour(&mut self, dir: Dir, now: f64) {
        if self.overview {
            return;
        }
        let Some(target) = self.neighbour_of(self.framed, dir) else { return };
        self.frame(target, now);
    }

    fn neighbour_of(&self, from: NodeId, dir: Dir) -> Option<NodeId> {
        let mut cur = from;
        let sibling = loop {
            let parent = self.tree.parent(cur)?;
            if let Node::Split { axis, children, .. } = self.tree.node(parent) {
                if *axis == dir.axis() {
                    let shown: Vec<NodeId> =
                        children.iter().copied().filter(|c| self.layout.rect(*c).is_some()).collect();
                    let idx = shown.iter().position(|c| *c == cur)?;
                    let next =
                        if dir.forward() { shown.get(idx + 1) } else { idx.checked_sub(1).and_then(|i| shown.get(i)) };
                    if let Some(n) = next {
                        break *n;
                    }
                }
            }
            cur = parent;
        };
        // Descend toward the depth we started at, staying close to where we were.
        let want = self.tree.depth(from);
        let anchor = self.layout.rect(from)?.center();
        let mut target = sibling;
        while self.tree.depth(target) < want {
            let best =
                self.tree.children(target).iter().copied().filter_map(|c| self.layout.rect(c).map(|r| (c, r))).min_by(
                    |(_, a), (_, b)| {
                        let da = (a.center().x - anchor.x).powi(2) + (a.center().y - anchor.y).powi(2);
                        let db = (b.center().x - anchor.x).powi(2) + (b.center().y - anchor.y).powi(2);
                        da.total_cmp(&db)
                    },
                );
            match best {
                Some((c, _)) => target = c,
                None => break,
            }
        }
        Some(target)
    }

    /// Frames the focused window on its own; doing it again goes back.
    pub fn maximize(&mut self, now: f64) {
        let Some(leaf) = self.focus.and_then(|f| self.tree.leaf_of(f)) else { return };
        if self.framed == leaf && !self.overview {
            self.back(now);
        } else {
            self.frame(leaf, now);
        }
    }

    pub fn toggle_overview(&mut self, now: f64) {
        self.overview = !self.overview;
        self.retarget(now);
    }

    pub fn exit_overview(&mut self, now: f64) {
        if self.overview {
            self.overview = false;
            self.retarget(now);
        }
    }

    /// In Overview, a click frames the tile under the pointer and focuses it.
    pub fn click_in_overview(&mut self, p: Point, now: f64) {
        let Some(leaf) = self.leaf_at(p, self.view(now)) else { return };
        self.overview = false;
        if let Some(w) = self.tree.shown_windows_in(leaf).first().copied() {
            self.set_focus(Some(w));
        }
        self.frame(leaf, now);
    }

    pub fn back(&mut self, now: f64) {
        self.travel(-1, now);
    }

    pub fn forward(&mut self, now: f64) {
        self.travel(1, now);
    }

    fn travel(&mut self, delta: isize, now: f64) {
        let mut idx = self.history_index as isize;
        loop {
            idx += delta;
            if idx < 0 || idx as usize >= self.history.len() {
                return;
            }
            let node = self.visible_ancestor(self.tree.resolve(self.history[idx as usize]));
            if node != self.framed || self.overview {
                self.history_index = idx as usize;
                self.overview = false;
                self.framed = node;
                self.focus_inside_framed();
                self.retarget(now);
                return;
            }
        }
    }

    /// Saves the current view in slot 1..=9.
    pub fn save_view(&mut self, slot: usize) {
        let Some(entry) = slot.checked_sub(1).and_then(|i| self.saved.get_mut(i)) else { return };
        let node = if self.overview { self.tree.root() } else { self.framed };
        *entry = Some(SavedView { node, windows: self.tree.windows_in(node) });
    }

    /// Goes to the view saved in slot 1..=9. Returns false when the slot is empty.
    pub fn recall_view(&mut self, slot: usize, now: f64) -> bool {
        let Some(Some(view)) = slot.checked_sub(1).and_then(|i| self.saved.get(i)) else { return false };
        let mut node = self.tree.resolve(view.node);
        let leaves: Vec<NodeId> = view.windows.iter().filter_map(|w| self.tree.leaf_of(*w)).collect();
        if let Some(first) = leaves.first() {
            let lca = leaves.iter().fold(*first, |acc, l| self.tree.common_ancestor(acc, *l));
            if !self.tree.is_ancestor_or_self(node, lca) {
                node = lca;
            }
        }
        self.frame(node, now);
        true
    }

    pub fn has_saved_view(&self, slot: usize) -> bool {
        slot.checked_sub(1).and_then(|i| self.saved.get(i)).is_some_and(|s| s.is_some())
    }

    /// Cycles the selected tab of the focused tile.
    pub fn cycle_tab(&mut self, delta: isize) {
        let Some(tile) = self.focus.and_then(|f| self.tree.tile_of(f)) else { return };
        let Node::Tile { views, selected } = self.tree.node(tile) else { return };
        let n = views.len() as isize;
        let next = views[((*selected as isize + delta).rem_euclid(n)) as usize];
        self.set_focus(Some(next));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn world() -> World {
        World::new(Settings { gap: 0.0, animation: 0.0, ..Settings::default() }, Rect::new(0.0, 0.0, 1600.0, 900.0))
    }

    fn w(n: u64) -> WindowId {
        WindowId(n)
    }

    fn rect_of(world: &World, win: WindowId) -> Rect {
        world.placements(world.view(1e9)).into_iter().find(|p| p.window == win).unwrap().rect
    }

    #[test]
    fn first_window_fills_the_screen() {
        let mut wd = world();
        wd.add_window(w(1), Placement::Dock, 0.0);
        assert_eq!(wd.focus(), Some(w(1)));
        assert!(rect_of(&wd, w(1)).approx_eq(&Rect::new(0.0, 0.0, 1600.0, 900.0), 1e-6));
    }

    #[test]
    fn windows_split_along_the_longer_side() {
        let mut wd = world();
        wd.add_window(w(1), Placement::Dock, 0.0);
        wd.add_window(w(2), Placement::Dock, 0.0);
        assert!(rect_of(&wd, w(2)).approx_eq(&Rect::new(800.0, 0.0, 800.0, 900.0), 1e-6));
        wd.add_window(w(3), Placement::Dock, 0.0);
        assert!(rect_of(&wd, w(3)).approx_eq(&Rect::new(800.0, 450.0, 800.0, 450.0), 1e-6));
    }

    #[test]
    fn stepping_in_and_out() {
        let mut wd = world();
        wd.add_window(w(1), Placement::Dock, 0.0);
        wd.add_window(w(2), Placement::Dock, 0.0);
        wd.add_window(w(3), Placement::Dock, 0.0);
        let root = wd.tree().root();
        // Focus is on 3, inside the right column.
        wd.step_in(None, 0.0);
        let col = wd.framed();
        assert_eq!(wd.tree().parent(col), Some(root));
        assert!(rect_of(&wd, w(3)).approx_eq(&Rect::new(0.0, 450.0, 1600.0, 450.0), 1e-6));
        wd.step_in(None, 0.0);
        assert!(rect_of(&wd, w(3)).approx_eq(&Rect::new(0.0, 0.0, 1600.0, 900.0), 1e-6));
        wd.step_out(0.0);
        assert_eq!(wd.framed(), col);
        wd.step_out(0.0);
        assert_eq!(wd.framed(), root);
    }

    #[test]
    fn neighbours_keep_depth() {
        let mut wd = world();
        wd.add_window(w(1), Placement::Dock, 0.0);
        wd.add_window(w(2), Placement::Dock, 0.0);
        wd.add_window(w(3), Placement::Dock, 0.0);
        let t1 = wd.tree().tile_of(w(1)).unwrap();
        let t3 = wd.tree().tile_of(w(3)).unwrap();
        let t2 = wd.tree().tile_of(w(2)).unwrap();
        wd.frame(t1, 0.0);
        wd.neighbour(Dir::Right, 0.0);
        // From the left half we land on the right column, then go down to 3.
        assert_eq!(wd.tree().parent(t2), Some(wd.framed()));
        wd.frame(t2, 0.0);
        wd.neighbour(Dir::Down, 0.0);
        assert_eq!(wd.framed(), t3);
        wd.neighbour(Dir::Left, 0.0);
        assert_eq!(wd.framed(), t1);
        assert_eq!(wd.focus(), Some(w(1)));
    }

    #[test]
    fn history_and_saved_views() {
        let mut wd = world();
        wd.add_window(w(1), Placement::Dock, 0.0);
        wd.add_window(w(2), Placement::Dock, 0.0);
        let root = wd.tree().root();
        let t1 = wd.tree().tile_of(w(1)).unwrap();
        let t2 = wd.tree().tile_of(w(2)).unwrap();
        wd.frame(t1, 0.0);
        wd.save_view(1);
        wd.frame(t2, 0.0);
        wd.back(0.0);
        assert_eq!(wd.framed(), t1);
        wd.back(0.0);
        assert_eq!(wd.framed(), root);
        wd.forward(0.0);
        assert_eq!(wd.framed(), t1);
        wd.frame(t2, 0.0);
        assert!(wd.recall_view(1, 0.0));
        assert_eq!(wd.framed(), t1);
        assert!(!wd.recall_view(2, 0.0));
    }

    #[test]
    fn saved_view_survives_its_node_being_removed() {
        let mut wd = world();
        wd.add_window(w(1), Placement::Dock, 0.0);
        wd.add_window(w(2), Placement::Dock, 0.0);
        wd.add_window(w(3), Placement::Dock, 0.0);
        let col = wd.tree().parent(wd.tree().tile_of(w(3)).unwrap()).unwrap();
        wd.frame(col, 0.0);
        wd.save_view(4);
        wd.remove_window(w(2), 0.0);
        wd.frame(wd.tree().root(), 0.0);
        wd.recall_view(4, 0.0);
        assert_eq!(wd.framed(), wd.tree().tile_of(w(3)).unwrap());
    }

    #[test]
    fn removing_the_framed_window_reframes_upward() {
        let mut wd = world();
        wd.add_window(w(1), Placement::Dock, 0.0);
        wd.add_window(w(2), Placement::Dock, 0.0);
        wd.frame(wd.tree().tile_of(w(2)).unwrap(), 0.0);
        wd.remove_window(w(2), 0.0);
        assert_eq!(wd.focus(), Some(w(1)));
        assert!(rect_of(&wd, w(1)).approx_eq(&Rect::new(0.0, 0.0, 1600.0, 900.0), 1e-6));
    }

    #[test]
    fn new_window_beside_a_framed_leaf_widens_the_frame() {
        let mut wd = world();
        wd.add_window(w(1), Placement::Dock, 0.0);
        wd.add_window(w(2), Placement::Dock, 0.0);
        let t1 = wd.tree().tile_of(w(1)).unwrap();
        wd.frame(t1, 0.0);
        wd.add_window(w(3), Placement::Dock, 0.0);
        let t3 = wd.tree().tile_of(w(3)).unwrap();
        assert!(wd.tree().is_ancestor_or_self(wd.framed(), t3));
        let on_screen = wd.screen();
        assert!(on_screen.contains_rect(&rect_of(&wd, w(3))));
    }

    #[test]
    fn overview_shows_everything_and_click_goes_in() {
        let mut wd = world();
        wd.add_window(w(1), Placement::Dock, 0.0);
        wd.add_window(w(2), Placement::Dock, 0.0);
        let t2 = wd.tree().tile_of(w(2)).unwrap();
        wd.frame(t2, 0.0);
        wd.toggle_overview(0.0);
        assert!(wd.view(1.0).approx_eq(&Rect::UNIT, 1e-9));
        wd.click_in_overview(Point::new(100.0, 100.0), 1.0);
        assert!(!wd.in_overview());
        assert_eq!(wd.framed(), wd.tree().tile_of(w(1)).unwrap());
        assert_eq!(wd.focus(), Some(w(1)));
    }

    #[test]
    fn floats_hang_above_the_layout() {
        let mut wd = world();
        wd.add_window(w(1), Placement::Dock, 0.0);
        wd.add_window(w(2), Placement::Float, 0.0);
        assert!(!wd.tree().desktop_visible());
        assert!(rect_of(&wd, w(1)).approx_eq(&Rect::new(0.0, 0.0, 1600.0, 900.0), 1e-6));
        assert!(wd.float_needs_size(w(2)));
        wd.size_float(w(2), 400.0, 300.0);
        assert!(rect_of(&wd, w(2)).approx_eq(&Rect::new(600.0, 300.0, 400.0, 300.0), 1e-6));
        // Zooming does not move floating windows.
        wd.frame(wd.tree().tile_of(w(1)).unwrap(), 0.0);
        assert!(rect_of(&wd, w(2)).approx_eq(&Rect::new(600.0, 300.0, 400.0, 300.0), 1e-6));
        // Docking and floating again returns it to where it was.
        wd.toggle_float(w(2), 0.0);
        assert!(!wd.tree().is_floating(w(2)));
        wd.toggle_float(w(2), 0.0);
        assert!(rect_of(&wd, w(2)).approx_eq(&Rect::new(600.0, 300.0, 400.0, 300.0), 1e-6));
        let p = wd.placements(wd.view(1.0));
        assert_eq!(p.last().unwrap().window, w(2), "floats come last, on top");
    }

    #[test]
    fn resizing_tiles_and_floats() {
        let mut wd = world();
        wd.add_window(w(1), Placement::Dock, 0.0);
        wd.add_window(w(2), Placement::Dock, 0.0);
        // Focus is on 2 (right half), which has no right neighbour: moving the
        // divider right moves its left edge, so it shrinks.
        assert!(wd.resize_focused(100.0, 0.0, 0.0));
        assert!(rect_of(&wd, w(2)).approx_eq(&Rect::new(900.0, 0.0, 700.0, 900.0), 1e-6));
        assert!(rect_of(&wd, w(1)).approx_eq(&Rect::new(0.0, 0.0, 900.0, 900.0), 1e-6));
        // Moving it left again grows it back.
        assert!(wd.resize_focused(-100.0, 0.0, 0.0));
        assert!(rect_of(&wd, w(2)).approx_eq(&Rect::new(800.0, 0.0, 800.0, 900.0), 1e-6));
        // Nothing above or below: height cannot change.
        assert!(!wd.resize_focused(0.0, 50.0, 0.0));
        wd.add_window(w(3), Placement::Float, 0.0);
        wd.size_float(w(3), 400.0, 300.0);
        wd.resize_window(w(3), Side::Left, -100.0, Side::Bottom, 50.0, 0.0);
        assert!(rect_of(&wd, w(3)).approx_eq(&Rect::new(500.0, 300.0, 500.0, 350.0), 1e-6));
    }

    #[test]
    fn cycling_and_split_toggle() {
        let mut wd = world();
        wd.add_window(w(1), Placement::Dock, 0.0);
        wd.add_window(w(2), Placement::Dock, 0.0);
        wd.add_window(w(3), Placement::Dock, 0.0);
        wd.cycle(true, 0.0);
        assert_eq!(wd.focus(), Some(w(1)));
        wd.cycle(false, 0.0);
        assert_eq!(wd.focus(), Some(w(3)));
        // 2 and 3 are stacked in the right column; toggling lays them side by side.
        wd.toggle_split(0.0);
        assert!(rect_of(&wd, w(3)).approx_eq(&Rect::new(1200.0, 0.0, 400.0, 900.0), 1e-6));
    }

    #[test]
    fn camera_animates_then_settles() {
        let mut wd = World::new(Settings { gap: 0.0, ..Settings::default() }, Rect::new(0.0, 0.0, 1600.0, 900.0));
        wd.add_window(w(1), Placement::Dock, 0.0);
        wd.add_window(w(2), Placement::Dock, 0.0);
        wd.frame(wd.tree().tile_of(w(2)).unwrap(), 10.0);
        assert!(wd.is_animating(10.1));
        let mid = wd.view(10.1);
        assert!(mid.w < 1.0 && mid.w > 0.5);
        assert!(!wd.is_animating(11.0));
        assert!(wd.view(11.0).approx_eq(&Rect::new(0.5, 0.0, 0.5, 1.0), 1e-9));
    }

    #[test]
    fn drop_targets_by_position() {
        let mut wd = world();
        wd.add_window(w(1), Placement::Dock, 0.0);
        wd.add_window(w(2), Placement::Dock, 0.0);
        let t1 = wd.tree().tile_of(w(1)).unwrap();
        let t2 = wd.tree().tile_of(w(2)).unwrap();
        let v = wd.view(1.0);
        // Middle of the left tile: become its tab.
        assert_eq!(wd.drop_target(Point::new(400.0, 450.0), w(2), v), Some(DropTarget::Tab(t1)));
        // Near its bottom edge: dock below it.
        assert_eq!(wd.drop_target(Point::new(400.0, 850.0), w(2), v), Some(DropTarget::Edge(t1, Side::Bottom)));
        // Over its own lone tile: nothing.
        assert_eq!(wd.drop_target(Point::new(1200.0, 450.0), w(2), v), None);
        // At the very edge of the screen: beside everything.
        let root = wd.tree().root();
        assert_eq!(wd.drop_target(Point::new(1595.0, 450.0), w(1), v), Some(DropTarget::Edge(root, Side::Right)));
        let _ = t2;
    }

    #[test]
    fn dropping_rearranges() {
        let mut wd = world();
        wd.add_window(w(1), Placement::Dock, 0.0);
        wd.add_window(w(2), Placement::Dock, 0.0);
        wd.add_window(w(3), Placement::Dock, 0.0);
        let t1 = wd.tree().tile_of(w(1)).unwrap();
        wd.apply_drop(w(3), DropTarget::Tab(t1), 0.0);
        wd.tree().check_invariants();
        assert_eq!(wd.tree().windows_in(t1), vec![w(1), w(3)]);
        assert_eq!(wd.focus(), Some(w(3)));
        assert!(rect_of(&wd, w(3)).approx_eq(&Rect::new(0.0, 28.0, 800.0, 872.0), 1e-6));
        let strips = wd.tab_strips(wd.view(1.0));
        assert_eq!(strips.len(), 1);
        assert_eq!(strips[0].selected, 1);
        wd.apply_drop(w(3), DropTarget::Edge(t1, Side::Top), 0.0);
        wd.tree().check_invariants();
        assert!(rect_of(&wd, w(3)).approx_eq(&Rect::new(0.0, 0.0, 800.0, 450.0), 1e-6));
        wd.apply_drop(w(3), DropTarget::Float, 0.0);
        assert!(wd.tree().is_floating(w(3)));
    }

    #[test]
    fn keyboard_moves() {
        let mut wd = world();
        wd.add_window(w(1), Placement::Dock, 0.0);
        wd.add_window(w(2), Placement::Dock, 0.0);
        // [1 | 2], focus 2. Moving 2 left swaps them.
        wd.move_focused(Dir::Left, 0.0);
        assert!(rect_of(&wd, w(2)).approx_eq(&Rect::new(0.0, 0.0, 800.0, 900.0), 1e-6));
        // Moving it up at the top edge docks it above everything.
        wd.move_focused(Dir::Up, 0.0);
        wd.tree().check_invariants();
        assert!(rect_of(&wd, w(2)).approx_eq(&Rect::new(0.0, 0.0, 1600.0, 450.0), 1e-6));
        assert!(rect_of(&wd, w(1)).approx_eq(&Rect::new(0.0, 450.0, 1600.0, 450.0), 1e-6));
    }

    #[test]
    fn tabs_cycle() {
        let mut wd = world();
        wd.add_window(w(1), Placement::Dock, 0.0);
        let t = wd.tree().tile_of(w(1)).unwrap();
        wd.tree.tab(w(2), t);
        wd.relayout();
        wd.focus_window(w(1), 0.0);
        assert_eq!(wd.tree().shown_windows_in(t), vec![w(1)]);
        wd.cycle_tab(1);
        assert_eq!(wd.focus(), Some(w(2)));
        wd.cycle_tab(1);
        assert_eq!(wd.focus(), Some(w(1)));
        assert_eq!(wd.tree().shown_windows_in(t), vec![w(1)]);
    }
}
