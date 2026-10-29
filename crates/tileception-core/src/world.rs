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

/// Where a minimized window was, to put it back there.
#[derive(Debug, Clone)]
enum Home {
    /// A tab of the tile holding these windows.
    Tab(Vec<WindowId>),
    /// On `side` of the node holding these windows, with this share of the split.
    Beside { windows: Vec<WindowId>, side: Side, share: f64 },
    /// Floating at this rectangle.
    Float(Rect),
    /// The only tiled window: it docks like a new one.
    Alone,
}

#[derive(Debug, Clone)]
struct Minimized {
    window: WindowId,
    home: Home,
}

#[derive(Debug, Clone, Copy)]
struct Camera {
    from: Rect,
    target: Rect,
    start: f64,
    duration: f64,
}

impl Camera {
    fn rect_at(&self, now: f64) -> Rect {
        let progress = self.progress(now);
        // Ease out cubic: quick to respond, gentle to land.
        let eased = 1.0 - (1.0 - progress).powi(3);
        self.from.zoom_lerp(&self.target, eased)
    }

    fn progress(&self, now: f64) -> f64 {
        if self.duration <= 0.0 { 1.0 } else { ((now - self.start) / self.duration).clamp(0.0, 1.0) }
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
    /// Minimized windows, out of the tree, oldest first.
    minimized: Vec<Minimized>,
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
            floats: self.floats.iter().map(|(window_id, float)| (*window_id, float.rect)).collect(),
            framed: self.tree.windows_in(self.framed),
            saved: self
                .saved
                .iter()
                .map(|slot| slot.as_ref().map(|saved| saved.windows.clone()).unwrap_or_default())
                .collect(),
        }
    }

    pub fn restore_layout(&mut self, snapshot: &LayoutSnapshot, remap: &HashMap<WindowId, WindowId>, now: f64) -> bool {
        let Some(tree) = Tree::restore(&snapshot.root, remap) else { return false };
        if snapshot.floats.iter().any(|(_, rect)| {
            ![rect.x, rect.y, rect.w, rect.h].iter().all(|coordinate| coordinate.is_finite())
                || rect.w <= 0.0
                || rect.h <= 0.0
        }) {
            return false;
        }
        self.tree = tree;
        self.floats.clear();
        for (old, rect) in &snapshot.floats {
            if let Some(window_id) = remap.get(old).filter(|window_id| self.tree.is_floating(**window_id)) {
                self.floats.insert(*window_id, Float { rect: *rect, sized: true });
            }
        }
        for window_id in self.tree.floats() {
            if !self.floats.contains_key(window_id) {
                self.floats.insert(*window_id, Float { rect: self.default_float_rect(), sized: true });
            }
        }
        let surviving = |ids: &[WindowId]| -> Vec<WindowId> {
            ids.iter()
                .filter_map(|window_id| remap.get(window_id).copied())
                .filter(|window_id| self.tree.leaf_of(*window_id).is_some())
                .collect()
        };
        let common = |windows: &[WindowId]| -> NodeId {
            windows
                .iter()
                .filter_map(|window_id| self.tree.leaf_of(*window_id))
                .reduce(|first, second| self.tree.common_ancestor(first, second))
                .unwrap_or(self.tree.root())
        };
        self.framed = common(&surviving(&snapshot.framed));
        self.saved = std::array::from_fn(|index| {
            snapshot.saved.get(index).filter(|ids| !ids.is_empty()).and_then(|ids| {
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
            camera: Camera { from: Rect::UNIT, target: Rect::UNIT, start: 0.0, duration: 0.0 },
            floats: HashMap::new(),
            last_float: HashMap::new(),
            minimized: Vec::new(),
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
        self.camera.rect_at(now)
    }

    /// Where the camera is heading.
    pub fn target_view(&self) -> Rect {
        self.camera.target
    }

    pub fn is_animating(&self, now: f64) -> bool {
        self.camera.progress(now) < 1.0
    }

    /// Jumps to the end of the current camera move.
    pub fn finish_animation(&mut self) {
        self.camera.from = self.camera.target;
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
        let target = self.wanted_view();
        let from = self.view(now);
        if from.approx_eq(&target, 1e-9) {
            self.camera = Camera { from: target, target, start: now, duration: 0.0 };
        } else {
            self.camera = Camera { from, target, start: now, duration: self.settings.animation };
        }
    }

    // ----- placement -----

    /// Screen rectangles of every shown window for a given view, including
    /// windows outside the screen. Tiles are inset by half a gap.
    pub fn placements(&self, view: Rect) -> Vec<WindowPlacement> {
        let transform = self.xform(view);
        let half_gap = self.settings.gap / 2.0;
        let mut out = Vec::new();
        for leaf in self.tree.leaves(self.tree.root()) {
            let Some(unit) = self.layout.rect(leaf) else { continue };
            let tile = transform.apply(&unit).inset(half_gap);
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
        for window_id in self.tree.floats() {
            if let Some(float) = self.floats.get(window_id) {
                out.push(WindowPlacement {
                    window: *window_id,
                    leaf: desktop,
                    rect: float.rect,
                    role: WindowRole::Floating,
                });
            }
        }
        out
    }

    // ----- floating -----

    pub fn float_rect(&self, win: WindowId) -> Option<Rect> {
        self.floats.get(&win).map(|float| float.rect)
    }

    /// Whether a floating window still waits for the client to pick a size.
    pub fn float_needs_size(&self, win: WindowId) -> bool {
        self.floats.get(&win).is_some_and(|float| !float.sized)
    }

    /// Takes the size the client chose, centred on the screen.
    pub fn size_float(&mut self, win: WindowId, width: f64, height: f64) {
        let screen = self.screen;
        if let Some(float) = self.floats.get_mut(&win) {
            let (width, height) = (width.min(screen.w), height.min(screen.h));
            float.rect =
                Rect::new(screen.x + (screen.w - width) / 2.0, screen.y + (screen.h - height) / 2.0, width, height);
            float.sized = true;
        }
    }

    /// Grows a floating window's rectangle to a size its program insists on
    /// (a dialog that grew), keeping its centre and staying on screen.
    pub fn grow_float(&mut self, win: WindowId, width: f64, height: f64) {
        let screen = self.screen;
        if let Some(float) = self.floats.get_mut(&win) {
            let (width, height) = (width.max(float.rect.w).min(screen.w), height.max(float.rect.h).min(screen.h));
            let (center_x, center_y) = (float.rect.x + float.rect.w / 2.0, float.rect.y + float.rect.h / 2.0);
            let left = (center_x - width / 2.0).clamp(screen.x, screen.right() - width);
            let top = (center_y - height / 2.0).clamp(screen.y, screen.bottom() - height);
            float.rect = Rect::new(left, top, width, height);
        }
    }

    pub fn set_float_rect(&mut self, win: WindowId, rect: Rect) {
        if let Some(float) = self.floats.get_mut(&win) {
            float.rect = Rect::new(rect.x, rect.y, rect.w.max(MIN_FLOAT), rect.h.max(MIN_FLOAT));
            float.sized = true;
        }
    }

    fn strip_height(&self, tile: &Rect) -> f64 {
        self.settings.tab_height.min(tile.h * 0.25)
    }

    /// Tab strips of tiles holding several windows, for a given view.
    pub fn tab_strips(&self, view: Rect) -> Vec<TabStrip> {
        let transform = self.xform(view);
        let half_gap = self.settings.gap / 2.0;
        let mut out = Vec::new();
        for leaf in self.tree.leaves(self.tree.root()) {
            let Node::Tile { views, selected } = self.tree.node(leaf) else { continue };
            if views.len() < 2 {
                continue;
            }
            let Some(unit) = self.layout.rect(leaf) else { continue };
            let tile = transform.apply(&unit).inset(half_gap);
            let rect = Rect::new(tile.x, tile.y, tile.w, self.strip_height(&tile));
            out.push(TabStrip { tile: leaf, rect, views: views.clone(), selected: *selected });
        }
        out
    }

    /// Where `dragged` would land if let go at screen `point`.
    pub fn drop_target(&self, point: Point, dragged: WindowId, view: Rect) -> Option<DropTarget> {
        let area = self.frame_area();
        let transform = self.xform(view);
        // Right at the edge of the framed space: dock beside all of it.
        if !self.overview && area.contains(point) {
            let side = [
                (point.x - area.x, Side::Left),
                (area.right() - point.x, Side::Right),
                (point.y - area.y, Side::Top),
                (area.bottom() - point.y, Side::Bottom),
            ]
            .into_iter()
            .filter(|(distance, _)| *distance < SCREEN_EDGE_BAND)
            .min_by(|first, second| first.0.total_cmp(&second.0))
            .map(|(_, side)| side);
            if let Some(side) = side {
                return Some(DropTarget::Edge(self.framed, side));
            }
        }
        let leaf = self.leaf_at(point, view)?;
        if leaf == self.tree.desktop() {
            return Some(DropTarget::Float);
        }
        let rect = transform.apply(&self.layout.rect(leaf)?);
        let fraction_x = (point.x - rect.x) / rect.w;
        let fraction_y = (point.y - rect.y) / rect.h;
        let (distance, side) = [
            (fraction_x, Side::Left),
            (1.0 - fraction_x, Side::Right),
            (fraction_y, Side::Top),
            (1.0 - fraction_y, Side::Bottom),
        ]
        .into_iter()
        .min_by(|first, second| first.0.total_cmp(&second.0))
        .expect("four sides");
        let own_lone_tile = self.tree.tile_of(dragged) == Some(leaf) && self.tree.windows_in(leaf).len() == 1;
        if own_lone_tile {
            return None;
        }
        if distance < EDGE_BAND {
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
                let rect = self.node_rect(self.tree.resolve(node), view)?;
                let part = match side {
                    Side::Left => Rect::new(rect.x, rect.y, rect.w / 2.0, rect.h),
                    Side::Right => Rect::new(rect.x + rect.w / 2.0, rect.y, rect.w / 2.0, rect.h),
                    Side::Top => Rect::new(rect.x, rect.y, rect.w, rect.h / 2.0),
                    Side::Bottom => Rect::new(rect.x, rect.y + rect.h / 2.0, rect.w, rect.h / 2.0),
                };
                Some(part.inset(half_gap))
            }
            DropTarget::Tab(tile) => self.node_rect(tile, view).map(|rect| rect.inset(half_gap)),
            DropTarget::Float => self.node_rect(self.tree.desktop(), view).map(|rect| rect.inset(half_gap)),
        }
    }

    /// Lets go of a dragged window: a window in the layout moves, a minimized
    /// one is restored at the target instead of where it was.
    pub fn apply_drop(&mut self, win: WindowId, target: DropTarget, now: f64) {
        self.minimized.retain(|minimized| minimized.window != win);
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
            .filter(|candidate| *candidate != leaf)
            .filter_map(|candidate| self.layout.rect(candidate).map(|rect| (candidate, rect)))
            .filter_map(|(candidate, rect)| {
                let (gap, overlap) = match dir {
                    Dir::Right => (rect.x - cur.right(), rect.y < cur.bottom() && cur.y < rect.bottom()),
                    Dir::Left => (cur.x - rect.right(), rect.y < cur.bottom() && cur.y < rect.bottom()),
                    Dir::Down => (rect.y - cur.bottom(), rect.x < cur.right() && cur.x < rect.right()),
                    Dir::Up => (cur.y - rect.bottom(), rect.x < cur.right() && cur.x < rect.right()),
                };
                (gap > -EPS && overlap).then(|| {
                    let off = match dir.axis() {
                        Axis::Horizontal => (rect.center().y - cur.center().y).abs(),
                        Axis::Vertical => (rect.center().x - cur.center().x).abs(),
                    };
                    (candidate, gap, off)
                })
            })
            .min_by(|first, second| first.1.total_cmp(&second.1).then(first.2.total_cmp(&second.2)))
            .map(|(candidate, _, _)| candidate);
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
        for window_id in windows {
            if let Some(mut rect) = self.float_rect(*window_id) {
                match dir {
                    Dir::Left => rect.x -= 40.0,
                    Dir::Right => rect.x += 40.0,
                    Dir::Up => rect.y -= 40.0,
                    Dir::Down => rect.y += 40.0,
                }
                self.set_float_rect(*window_id, rect);
            } else if let Some(rect) = self.tree.tile_of(*window_id).and_then(|node| self.layout.rect(node)) {
                tiled.push((*window_id, rect));
            }
        }
        if tiled.is_empty() {
            return;
        }
        let left = tiled.iter().map(|(_, rect)| rect.x).fold(f64::INFINITY, f64::min);
        let top = tiled.iter().map(|(_, rect)| rect.y).fold(f64::INFINITY, f64::min);
        let right = tiled.iter().map(|(_, rect)| rect.right()).fold(f64::NEG_INFINITY, f64::max);
        let bottom = tiled.iter().map(|(_, rect)| rect.bottom()).fold(f64::NEG_INFINITY, f64::max);
        let target = self.tree.leaves(self.tree.root()).into_iter()
            .filter(|node| matches!(self.tree.node(*node), Node::Tile { views, .. } if !views.iter().any(|window| windows.contains(window))))
            .filter_map(|node| self.layout.rect(node).map(|rect| (node, rect)))
            .filter_map(|(node, rect)| {
                let (gap, overlaps) = match dir {
                    Dir::Left => (left - rect.right(), rect.y < bottom && top < rect.bottom()),
                    Dir::Right => (rect.x - right, rect.y < bottom && top < rect.bottom()),
                    Dir::Up => (top - rect.bottom(), rect.x < right && left < rect.right()),
                    Dir::Down => (rect.y - bottom, rect.x < right && left < rect.right()),
                };
                (gap >= -1e-9 && overlaps).then_some((node, gap))
            }).min_by(|first, second| first.1.total_cmp(&second.1)).map(|(node, _)| node);
        let Some(target) = target else { return };
        let target = self.tree.windows_in(target).into_iter().find(|window_id| !windows.contains(window_id));
        let moved: Vec<_> = tiled.into_iter().map(|(window_id, _)| window_id).collect();
        if let Some(target) = target
            && self.tree.move_group(&moved, target, side)
        {
            self.after_move(moved[0], now);
        }
    }

    /// Screen rectangle of a node for a given view, without gap insets.
    pub fn node_rect(&self, node: NodeId, view: Rect) -> Option<Rect> {
        self.layout.rect(node).map(|rect| self.xform(view).apply(&rect))
    }

    /// The leaf under a screen point for a given view.
    pub fn leaf_at(&self, point: Point, view: Rect) -> Option<NodeId> {
        let world = self.xform(view).invert_point(point);
        self.tree
            .leaves(self.tree.root())
            .into_iter()
            .find(|leaf| self.layout.rect(*leaf).is_some_and(|rect| rect.contains(world)))
    }

    // ----- edits -----

    fn relayout(&mut self) {
        self.layout = Layout::compute(&self.tree);
        self.framed = self.visible_ancestor(self.tree.resolve(self.framed));
    }

    fn visible_ancestor(&self, mut node_id: NodeId) -> NodeId {
        while self.layout.rect(node_id).is_none() {
            match self.tree.parent(node_id) {
                Some(parent) => node_id = parent,
                None => return self.tree.root(),
            }
        }
        node_id
    }

    /// Adds a new window and focuses it.
    pub fn add_window(&mut self, win: WindowId, placement: Placement, now: f64) {
        if self.contains_window(win) {
            return;
        }
        match placement {
            Placement::Float => {
                self.tree.float(win);
                let rect = self.default_float_rect();
                self.floats.insert(win, Float { rect, sized: false });
            }
            Placement::Dock => self.dock_new(win),
        }
        self.relayout();
        // Floating windows show whatever is framed, so only tiles widen the frame.
        if let Some(tile) = self.tree.tile_of(win)
            && !self.tree.is_ancestor_or_self(self.framed, tile)
        {
            let common = self.tree.common_ancestor(self.framed, tile);
            self.push_frame(common);
        }
        self.set_focus(Some(win));
        self.retarget(now);
    }

    /// Docks a detached window where a new one would go.
    fn dock_new(&mut self, win: WindowId) {
        let target = self.dock_target();
        let side = self.split_side(target);
        if !self.overview && target == self.framed && target != self.tree.desktop() {
            self.tree.dock_local(win, target, side);
        } else {
            self.tree.dock(win, target, side);
        }
    }

    /// The leaf a new window docks beside: the focused one when it is in view,
    /// otherwise the framed space as a whole.
    fn dock_target(&self) -> NodeId {
        if let Some(leaf) = self.focus.and_then(|focused| self.tree.leaf_of(focused))
            && leaf != self.tree.desktop()
            && self.tree.is_ancestor_or_self(self.framed, leaf)
        {
            return leaf;
        }
        self.framed
    }

    /// Splits along the longer side of the target as it would be on screen.
    fn split_side(&self, target: NodeId) -> Side {
        let unit = self.layout.rect(target).unwrap_or(Rect::UNIT);
        let framed = self.layout.rect(self.framed).unwrap_or(Rect::UNIT);
        let area = self.frame_area();
        let width = unit.w / framed.w * area.w;
        let height = unit.h / framed.h * area.h;
        if width >= height { Side::Right } else { Side::Bottom }
    }

    pub fn remove_window(&mut self, win: WindowId, now: f64) {
        self.minimized.retain(|minimized| minimized.window != win);
        if !self.tree.detach(win) {
            return;
        }
        self.floats.remove(&win);
        self.last_float.remove(&win);
        self.left_layout(win, now);
    }

    /// After a window left the tree: forget it in focus history, lay out the
    /// rest and focus the next window in view.
    fn left_layout(&mut self, win: WindowId, now: f64) {
        self.mru.retain(|window| *window != win);
        self.relayout();
        if self.focus == Some(win) {
            self.focus = None;
            let shown = self.tree.shown_windows_in(self.framed);
            let next = self.mru.iter().rev().find(|window| shown.contains(window)).copied().or(shown.first().copied());
            self.set_focus(next);
        }
        self.retarget(now);
    }

    // ----- minimizing -----

    /// Minimized windows, oldest first.
    pub fn minimized(&self) -> impl DoubleEndedIterator<Item = WindowId> + '_ {
        self.minimized.iter().map(|minimized| minimized.window)
    }

    pub fn is_minimized(&self, win: WindowId) -> bool {
        self.minimized.iter().any(|minimized| minimized.window == win)
    }

    /// Takes a window out of the layout without closing it; its neighbours
    /// take its space, as when it closes. Returns whether it was minimized.
    pub fn minimize(&mut self, win: WindowId, now: f64) -> bool {
        let Some(home) = self.home_of(win) else { return false };
        self.tree.detach(win);
        if let Some(float) = self.floats.remove(&win) {
            self.last_float.insert(win, float.rect);
        }
        self.minimized.push(Minimized { window: win, home });
        self.left_layout(win, now);
        true
    }

    /// Where a window in the layout is, put so that it can go back there
    /// after its neighbours have changed.
    fn home_of(&self, win: WindowId) -> Option<Home> {
        if let Some(float) = self.floats.get(&win) {
            return Some(Home::Float(float.rect));
        }
        let tile = self.tree.tile_of(win)?;
        let views = self.tree.windows_in(tile);
        if views.len() > 1 {
            return Some(Home::Tab(views.into_iter().filter(|window| *window != win).collect()));
        }
        let Some(parent) = self.tree.parent(tile) else { return Some(Home::Alone) };
        let Node::Split { axis, children, .. } = self.tree.node(parent) else { return Some(Home::Alone) };
        let tiled = |node: NodeId| -> Vec<WindowId> {
            self.tree.windows_in(node).into_iter().filter(|window| !self.tree.is_floating(*window)).collect()
        };
        let index = children.iter().position(|child| *child == tile)?;
        let usable = |child: &&NodeId| self.tree.is_visible(**child) && !tiled(**child).is_empty();
        let (trailing, leading) = match axis {
            Axis::Horizontal => (Side::Right, Side::Left),
            Axis::Vertical => (Side::Bottom, Side::Top),
        };
        // Beside the neighbour before it, or else the one after it.
        let (neighbour, side) = match children[..index].iter().rev().find(usable) {
            Some(before) => (*before, trailing),
            None => match children[index + 1..].iter().find(usable) {
                Some(after) => (*after, leading),
                None => return Some(Home::Alone),
            },
        };
        let share = self.tree.share_of(tile).unwrap_or(0.5);
        Some(Home::Beside { windows: tiled(neighbour), side, share })
    }

    /// Puts a minimized window back where it was and focuses it. Returns
    /// false when it is not minimized.
    pub fn restore(&mut self, win: WindowId, now: f64) -> bool {
        let Some(index) = self.minimized.iter().position(|minimized| minimized.window == win) else { return false };
        let home = self.minimized.remove(index).home;
        self.place_home(win, home);
        if self.tree.is_floating(win) {
            // Floating windows hang over whatever is framed.
            self.relayout();
            self.set_focus(Some(win));
            self.retarget(now);
        } else {
            self.after_move(win, now);
        }
        true
    }

    /// Puts the most recently minimized window back.
    pub fn restore_last(&mut self, now: f64) -> bool {
        match self.minimized.last() {
            Some(minimized) => self.restore(minimized.window, now),
            None => false,
        }
    }

    fn place_home(&mut self, win: WindowId, home: Home) {
        match home {
            Home::Float(rect) => self.add_float(win, Some(rect)),
            Home::Tab(others) => match others.iter().find_map(|window| self.tree.tile_of(*window)) {
                Some(tile) => self.tree.tab(win, tile),
                None => self.dock_new(win),
            },
            Home::Beside { windows, side, share } => {
                let alive: Vec<_> = windows.into_iter().filter(|window| self.tree.tile_of(*window).is_some()).collect();
                // The neighbour as it was, or failing that its window nearest
                // to where this one sat.
                let nearest = if side.is_leading() { alive.first() } else { alive.last() };
                let nearest = nearest.and_then(|window| self.tree.tile_of(*window));
                match self.tree.node_holding(&alive, side.axis()).or(nearest) {
                    Some(target) => {
                        let tile = self.tree.dock(win, target, side);
                        self.tree.set_share(tile, share);
                    }
                    None => self.dock_new(win),
                }
            }
            Home::Alone => self.dock_new(win),
        }
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
        if let Some(leaf) = self.tree.leaf_of(win)
            && !self.overview
            && !self.tree.is_ancestor_or_self(self.framed, leaf)
        {
            let common = self.tree.common_ancestor(self.framed, leaf);
            self.push_frame(common);
        }
        self.set_focus(Some(win));
        self.retarget(now);
    }

    pub fn toggle_float(&mut self, win: WindowId, now: f64) {
        if self.tree.is_floating(win) {
            self.tree.detach(win);
            if let Some(float) = self.floats.remove(&win) {
                self.last_float.insert(win, float.rect);
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
        let screen = self.screen;
        let off = (self.tree.floats().len() as f64 * 32.0) % 160.0;
        Rect::new(screen.x + screen.w * 0.2 + off, screen.y + screen.h * 0.2 + off, screen.w * 0.6, screen.h * 0.6)
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
        let target = self.wanted_view();
        self.camera = Camera { from: target, target, start: now, duration: 0.0 };
        true
    }

    // ----- focus -----

    fn set_focus(&mut self, win: Option<WindowId>) {
        self.focus = win;
        if let Some(window) = win {
            self.mru.retain(|recent| *recent != window);
            self.mru.push(window);
            self.tree.select_tab(window);
            self.tree.raise_float(window);
        }
    }

    /// Focuses the next (or previous) window in layout order, floating
    /// windows last, bringing it into view.
    pub fn cycle(&mut self, forward: bool, now: f64) {
        let all = self.tree.windows_in(self.tree.root());
        if all.is_empty() {
            return;
        }
        let index = self.focus.and_then(|focused| all.iter().position(|window| *window == focused));
        let count = all.len();
        let next = match (index, forward) {
            (Some(index), true) => all[(index + 1) % count],
            (Some(index), false) => all[(index + count - 1) % count],
            (None, _) => all[0],
        };
        self.focus_window(next, now);
    }

    /// Stacks the focused tile's row, or lays its column side by side.
    pub fn toggle_split(&mut self, now: f64) {
        let Some(tile) = self.focus.and_then(|focused| self.tree.tile_of(focused)) else { return };
        let Some(parent) = self.tree.parent(tile) else { return };
        self.tree.toggle_axis(parent);
        self.relayout();
        self.retarget(now);
    }

    /// Change only the selected windows' split, creating their subgroup if needed.
    pub fn toggle_split_windows(&mut self, windows: &[WindowId], now: f64) {
        let Some(group) = self.tree.group_windows(windows) else { return };
        self.tree.toggle_axis(group);
        self.relayout();
        self.retarget(now);
    }

    /// Moves the edges of a window by pixel amounts: `delta_x` moves the edge on
    /// `side_x`, `delta_y` the edge on `side_y`. Tiled windows move the split
    /// boundaries on those sides; floating windows change their rectangle.
    pub fn resize_window(
        &mut self,
        win: WindowId,
        side_x: Side,
        delta_x: f64,
        side_y: Side,
        delta_y: f64,
        now: f64,
    ) -> bool {
        if let Some(float) = self.floats.get(&win).copied() {
            let mut rect = float.rect;
            match side_x {
                Side::Left => {
                    let distance = delta_x.min(rect.w - MIN_FLOAT);
                    rect.x += distance;
                    rect.w -= distance;
                }
                _ => rect.w = (rect.w + delta_x).max(MIN_FLOAT),
            }
            match side_y {
                Side::Top => {
                    let distance = delta_y.min(rect.h - MIN_FLOAT);
                    rect.y += distance;
                    rect.h -= distance;
                }
                _ => rect.h = (rect.h + delta_y).max(MIN_FLOAT),
            }
            self.set_float_rect(win, rect);
            return true;
        }
        let Some(leaf) = self.tree.tile_of(win) else { return false };
        let mut changed = false;
        for (side, distance) in [(side_x, delta_x), (side_y, delta_y)] {
            if distance == 0.0 {
                continue;
            }
            if let Some((split, index)) = self.tree.boundary(leaf, side) {
                changed |= self.resize_gap(split, index, distance, now);
            }
        }
        changed
    }

    /// Moves the dividers around the focused window by pixels: positive `dx`
    /// moves a divider right, positive `dy` down. The divider moved is the one
    /// on the window's right (bottom) when it has a neighbour there, otherwise
    /// the one on its left (top). Either way, the line moves the way the arrow
    /// points; whether the window grows or shrinks follows from that.
    pub fn resize_focused(&mut self, delta_width: f64, delta_height: f64, now: f64) -> bool {
        let Some(win) = self.focus else { return false };
        let leaf = self.tree.tile_of(win);
        let pick = |trailing: Side, leading: Side, distance: f64| match leaf {
            Some(tile)
                if self.tree.boundary(tile, trailing).is_none() && self.tree.boundary(tile, leading).is_some() =>
            {
                (leading, distance)
            }
            _ => (trailing, distance),
        };
        let (side_x, delta_x) = pick(Side::Right, Side::Left, delta_width);
        let (side_y, delta_y) = pick(Side::Bottom, Side::Top, delta_height);
        self.resize_window(win, side_x, delta_x, side_y, delta_y, now)
    }

    /// Focuses a window, bringing it into view if it is outside the framed space.
    pub fn focus_window(&mut self, win: WindowId, now: f64) {
        let Some(leaf) = self.tree.leaf_of(win) else { return };
        let was_hidden_tab = !self.tree.shown_windows_in(leaf).contains(&win);
        self.set_focus(Some(win));
        if was_hidden_tab {
            self.relayout();
        }
        if !self.overview && !self.tree.is_floating(win) && !self.tree.is_ancestor_or_self(self.framed, leaf) {
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

    /// Frame exactly the tiled selection, instead of its unselected neighbours.
    pub fn frame_windows(&mut self, windows: &[WindowId], now: f64) {
        let Some(group) = self.tree.group_windows(windows) else { return };
        self.relayout();
        self.frame(group, now);
    }

    fn focus_inside_framed(&mut self) {
        let shown = self.tree.shown_windows_in(self.framed);
        if self.focus.is_some_and(|focused| shown.contains(&focused)) {
            return;
        }
        let pick = self.mru.iter().rev().find(|window| shown.contains(window)).copied().or(shown.first().copied());
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
        let children: Vec<NodeId> = self
            .tree
            .children(self.framed)
            .iter()
            .copied()
            .filter(|child| self.layout.rect(*child).is_some())
            .collect();
        if children.is_empty() {
            return;
        }
        let by_point = toward.and_then(|point| {
            let world = self.xform(self.target_view()).invert_point(point);
            children.iter().copied().find(|child| self.layout.rect(*child).is_some_and(|rect| rect.contains(world)))
        });
        let by_focus = self
            .focus
            .and_then(|focused| self.tree.leaf_of(focused))
            .and_then(|leaf| children.iter().copied().find(|child| self.tree.is_ancestor_or_self(*child, leaf)));
        let child = by_point.or(by_focus).unwrap_or(children[0]);
        self.frame(child, now);
    }

    pub fn step_out(&mut self, now: f64) {
        if self.overview {
            return;
        }
        if let Some(parent) = self.tree.parent(self.framed) {
            self.frame(parent, now);
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
            if let Node::Split { axis, children, .. } = self.tree.node(parent)
                && *axis == dir.axis()
            {
                let shown: Vec<NodeId> =
                    children.iter().copied().filter(|child| self.layout.rect(*child).is_some()).collect();
                let idx = shown.iter().position(|child| *child == cur)?;
                let next = if dir.forward() {
                    shown.get(idx + 1)
                } else {
                    idx.checked_sub(1).and_then(|index| shown.get(index))
                };
                if let Some(neighbour) = next {
                    break *neighbour;
                }
            }
            cur = parent;
        };
        // Descend toward the depth we started at, staying close to where we were.
        let want = self.tree.depth(from);
        let anchor = self.layout.rect(from)?.center();
        let mut target = sibling;
        while self.tree.depth(target) < want {
            let best = self
                .tree
                .children(target)
                .iter()
                .copied()
                .filter_map(|child| self.layout.rect(child).map(|rect| (child, rect)))
                .min_by(|(_, first), (_, second)| {
                    let first_distance = (first.center().x - anchor.x).powi(2) + (first.center().y - anchor.y).powi(2);
                    let second_distance =
                        (second.center().x - anchor.x).powi(2) + (second.center().y - anchor.y).powi(2);
                    first_distance.total_cmp(&second_distance)
                });
            match best {
                Some((child, _)) => target = child,
                None => break,
            }
        }
        Some(target)
    }

    /// Frames the focused window on its own; doing it again goes back.
    pub fn maximize(&mut self, now: f64) {
        let Some(leaf) = self.focus.and_then(|focused| self.tree.leaf_of(focused)) else { return };
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
    pub fn click_in_overview(&mut self, point: Point, now: f64) {
        let Some(leaf) = self.leaf_at(point, self.view(now)) else { return };
        self.overview = false;
        if let Some(window) = self.tree.shown_windows_in(leaf).first().copied() {
            self.set_focus(Some(window));
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
        let Some(entry) = slot.checked_sub(1).and_then(|index| self.saved.get_mut(index)) else { return };
        let node = if self.overview { self.tree.root() } else { self.framed };
        *entry = Some(SavedView { node, windows: self.tree.windows_in(node) });
    }

    /// Goes to the view saved in slot 1..=9. Returns false when the slot is empty.
    pub fn recall_view(&mut self, slot: usize, now: f64) -> bool {
        let Some(Some(view)) = slot.checked_sub(1).and_then(|index| self.saved.get(index)) else { return false };
        let view = view.clone();
        // Minimized windows still belong to the view: they come back into it.
        // Going to a view never minimizes anything.
        let back: Vec<_> = self.minimized().filter(|window| view.windows.contains(window)).collect();
        for win in back {
            let index =
                self.minimized.iter().position(|minimized| minimized.window == win).expect("listed as minimized");
            let home = self.minimized.remove(index).home;
            self.place_home(win, home);
        }
        self.relayout();
        let mut node = self.tree.resolve(view.node);
        let leaves: Vec<NodeId> = view.windows.iter().filter_map(|window| self.tree.leaf_of(*window)).collect();
        if let Some(first) = leaves.first() {
            let lca = leaves.iter().fold(*first, |ancestor, leaf| self.tree.common_ancestor(ancestor, *leaf));
            if !self.tree.is_ancestor_or_self(node, lca) {
                node = lca;
            }
        }
        self.frame(node, now);
        true
    }

    pub fn has_saved_view(&self, slot: usize) -> bool {
        slot.checked_sub(1).and_then(|index| self.saved.get(index)).is_some_and(|saved| saved.is_some())
    }

    /// The node a saved view would frame now, without going there.
    pub fn saved_view_node(&self, slot: usize) -> Option<NodeId> {
        let view = slot.checked_sub(1).and_then(|index| self.saved.get(index))?.as_ref()?;
        let node = self.visible_ancestor(self.tree.resolve(view.node));
        let lca = view
            .windows
            .iter()
            .filter_map(|window| self.tree.leaf_of(*window))
            .reduce(|first, second| self.tree.common_ancestor(first, second));
        Some(match lca {
            Some(lca) if !self.tree.is_ancestor_or_self(node, lca) => lca,
            _ => node,
        })
    }

    /// Whether `back` (-1) or `forward` (1) would go anywhere.
    pub fn can_travel(&self, delta: isize) -> bool {
        let mut idx = self.history_index as isize;
        loop {
            idx += delta;
            if idx < 0 || idx as usize >= self.history.len() {
                return false;
            }
            let node = self.visible_ancestor(self.tree.resolve(self.history[idx as usize]));
            if node != self.framed || self.overview {
                return true;
            }
        }
    }

    /// Whether the camera has framed this node before, as far as history goes.
    pub fn visited(&self, node: NodeId) -> bool {
        self.history.iter().any(|node_id| self.tree.resolve(*node_id) == node)
    }

    /// Cycles the selected tab of the focused tile.
    pub fn cycle_tab(&mut self, delta: isize) {
        let Some(tile) = self.focus.and_then(|focused| self.tree.tile_of(focused)) else { return };
        let Node::Tile { views, selected } = self.tree.node(tile) else { return };
        let count = views.len() as isize;
        let next = views[((*selected as isize + delta).rem_euclid(count)) as usize];
        self.set_focus(Some(next));
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test setup and assertions should fail immediately on unexpected errors")]
mod tests {
    use super::*;

    fn world() -> World {
        World::new(Settings { gap: 0.0, animation: 0.0, ..Settings::default() }, Rect::new(0.0, 0.0, 1600.0, 900.0))
    }

    fn rect_of(world: &World, win: WindowId) -> Rect {
        world.placements(world.view(1e9)).into_iter().find(|placement| placement.window == win).unwrap().rect
    }

    #[test]
    fn first_window_fills_the_screen() {
        let mut world = world();
        world.add_window(WindowId(1), Placement::Dock, 0.0);
        assert_eq!(world.focus(), Some(WindowId(1)));
        assert!(rect_of(&world, WindowId(1)).approx_eq(&Rect::new(0.0, 0.0, 1600.0, 900.0), 1e-6));
    }

    #[test]
    fn windows_split_along_the_longer_side() {
        let mut world = world();
        world.add_window(WindowId(1), Placement::Dock, 0.0);
        world.add_window(WindowId(2), Placement::Dock, 0.0);
        assert!(rect_of(&world, WindowId(2)).approx_eq(&Rect::new(800.0, 0.0, 800.0, 900.0), 1e-6));
        world.add_window(WindowId(3), Placement::Dock, 0.0);
        assert!(rect_of(&world, WindowId(3)).approx_eq(&Rect::new(800.0, 450.0, 800.0, 450.0), 1e-6));
    }

    #[test]
    fn stepping_in_and_out() {
        let mut world = world();
        world.add_window(WindowId(1), Placement::Dock, 0.0);
        world.add_window(WindowId(2), Placement::Dock, 0.0);
        world.add_window(WindowId(3), Placement::Dock, 0.0);
        let root = world.tree().root();
        // Focus is on 3, inside the right column.
        world.step_in(None, 0.0);
        let col = world.framed();
        assert_eq!(world.tree().parent(col), Some(root));
        assert!(rect_of(&world, WindowId(3)).approx_eq(&Rect::new(0.0, 450.0, 1600.0, 450.0), 1e-6));
        world.step_in(None, 0.0);
        assert!(rect_of(&world, WindowId(3)).approx_eq(&Rect::new(0.0, 0.0, 1600.0, 900.0), 1e-6));
        world.step_out(0.0);
        assert_eq!(world.framed(), col);
        world.step_out(0.0);
        assert_eq!(world.framed(), root);
    }

    #[test]
    fn neighbours_keep_depth() {
        let mut world = world();
        world.add_window(WindowId(1), Placement::Dock, 0.0);
        world.add_window(WindowId(2), Placement::Dock, 0.0);
        world.add_window(WindowId(3), Placement::Dock, 0.0);
        let first_tile = world.tree().tile_of(WindowId(1)).unwrap();
        let third_tile = world.tree().tile_of(WindowId(3)).unwrap();
        let second_tile = world.tree().tile_of(WindowId(2)).unwrap();
        world.frame(first_tile, 0.0);
        world.neighbour(Dir::Right, 0.0);
        // From the left half we land on the right column, then go down to 3.
        assert_eq!(world.tree().parent(second_tile), Some(world.framed()));
        world.frame(second_tile, 0.0);
        world.neighbour(Dir::Down, 0.0);
        assert_eq!(world.framed(), third_tile);
        world.neighbour(Dir::Left, 0.0);
        assert_eq!(world.framed(), first_tile);
        assert_eq!(world.focus(), Some(WindowId(1)));
    }

    #[test]
    fn history_and_saved_views() {
        let mut world = world();
        world.add_window(WindowId(1), Placement::Dock, 0.0);
        world.add_window(WindowId(2), Placement::Dock, 0.0);
        let root = world.tree().root();
        let first_tile = world.tree().tile_of(WindowId(1)).unwrap();
        let second_tile = world.tree().tile_of(WindowId(2)).unwrap();
        world.frame(first_tile, 0.0);
        world.save_view(1);
        world.frame(second_tile, 0.0);
        assert_eq!(world.saved_view_node(1), Some(first_tile));
        assert_eq!(world.saved_view_node(2), None);
        assert!(world.visited(first_tile) && world.visited(second_tile));
        assert!(world.can_travel(-1) && !world.can_travel(1));
        world.back(0.0);
        assert_eq!(world.framed(), first_tile);
        world.back(0.0);
        assert_eq!(world.framed(), root);
        assert!(!world.can_travel(-1) && world.can_travel(1));
        world.forward(0.0);
        assert_eq!(world.framed(), first_tile);
        world.frame(second_tile, 0.0);
        assert!(world.recall_view(1, 0.0));
        assert_eq!(world.framed(), first_tile);
        assert!(!world.recall_view(2, 0.0));
    }

    #[test]
    fn saved_view_survives_its_node_being_removed() {
        let mut world = world();
        world.add_window(WindowId(1), Placement::Dock, 0.0);
        world.add_window(WindowId(2), Placement::Dock, 0.0);
        world.add_window(WindowId(3), Placement::Dock, 0.0);
        let col = world.tree().parent(world.tree().tile_of(WindowId(3)).unwrap()).unwrap();
        world.frame(col, 0.0);
        world.save_view(4);
        world.remove_window(WindowId(2), 0.0);
        world.frame(world.tree().root(), 0.0);
        world.recall_view(4, 0.0);
        assert_eq!(world.framed(), world.tree().tile_of(WindowId(3)).unwrap());
    }

    #[test]
    fn removing_the_framed_window_reframes_upward() {
        let mut world = world();
        world.add_window(WindowId(1), Placement::Dock, 0.0);
        world.add_window(WindowId(2), Placement::Dock, 0.0);
        world.frame(world.tree().tile_of(WindowId(2)).unwrap(), 0.0);
        world.remove_window(WindowId(2), 0.0);
        assert_eq!(world.focus(), Some(WindowId(1)));
        assert!(rect_of(&world, WindowId(1)).approx_eq(&Rect::new(0.0, 0.0, 1600.0, 900.0), 1e-6));
    }

    #[test]
    fn new_window_beside_a_framed_leaf_widens_the_frame() {
        let mut world = world();
        world.add_window(WindowId(1), Placement::Dock, 0.0);
        world.add_window(WindowId(2), Placement::Dock, 0.0);
        let first_tile = world.tree().tile_of(WindowId(1)).unwrap();
        world.frame(first_tile, 0.0);
        world.add_window(WindowId(3), Placement::Dock, 0.0);
        let third_tile = world.tree().tile_of(WindowId(3)).unwrap();
        assert!(world.tree().is_ancestor_or_self(world.framed(), third_tile));
        let on_screen = world.screen();
        assert!(on_screen.contains_rect(&rect_of(&world, WindowId(3))));
    }

    #[test]
    fn floating_window_keeps_the_frame() {
        let mut world = world();
        world.add_window(WindowId(1), Placement::Dock, 0.0);
        world.add_window(WindowId(2), Placement::Dock, 0.0);
        let first_tile = world.tree().tile_of(WindowId(1)).unwrap();
        world.frame(first_tile, 0.0);
        world.add_window(WindowId(3), Placement::Float, 0.0);
        assert_eq!(world.framed(), first_tile);
        assert_eq!(world.focus(), Some(WindowId(3)));
        world.focus_window(WindowId(1), 0.0);
        world.focus_window(WindowId(3), 0.0);
        assert_eq!(world.framed(), first_tile);
    }

    #[test]
    fn overview_shows_everything_and_click_goes_in() {
        let mut world = world();
        world.add_window(WindowId(1), Placement::Dock, 0.0);
        world.add_window(WindowId(2), Placement::Dock, 0.0);
        let second_tile = world.tree().tile_of(WindowId(2)).unwrap();
        world.frame(second_tile, 0.0);
        world.toggle_overview(0.0);
        assert!(world.view(1.0).approx_eq(&Rect::UNIT, 1e-9));
        world.click_in_overview(Point::new(100.0, 100.0), 1.0);
        assert!(!world.in_overview());
        assert_eq!(world.framed(), world.tree().tile_of(WindowId(1)).unwrap());
        assert_eq!(world.focus(), Some(WindowId(1)));
    }

    #[test]
    fn grown_floats_keep_their_centre_on_screen() {
        let mut world = world();
        world.add_window(WindowId(1), Placement::Float, 0.0);
        world.size_float(WindowId(1), 400.0, 300.0);
        let before = world.float_rect(WindowId(1)).unwrap();
        world.grow_float(WindowId(1), 600.0, 200.0);
        let after = world.float_rect(WindowId(1)).unwrap();
        // Wider as asked, never shorter, same centre.
        assert_eq!((after.w, after.h), (600.0, 300.0));
        assert!((after.x + after.w / 2.0 - (before.x + before.w / 2.0)).abs() < 1e-9);
        // Larger than the screen: kept on it.
        let screen = world.screen();
        world.grow_float(WindowId(1), 1e6, 1e6);
        let huge = world.float_rect(WindowId(1)).unwrap();
        assert_eq!((huge.x, huge.y, huge.w, huge.h), (screen.x, screen.y, screen.w, screen.h));
    }

    #[test]
    fn floats_hang_above_the_layout() {
        let mut world = world();
        world.add_window(WindowId(1), Placement::Dock, 0.0);
        world.add_window(WindowId(2), Placement::Float, 0.0);
        assert!(!world.tree().desktop_visible());
        assert!(rect_of(&world, WindowId(1)).approx_eq(&Rect::new(0.0, 0.0, 1600.0, 900.0), 1e-6));
        assert!(world.float_needs_size(WindowId(2)));
        world.size_float(WindowId(2), 400.0, 300.0);
        assert!(rect_of(&world, WindowId(2)).approx_eq(&Rect::new(600.0, 300.0, 400.0, 300.0), 1e-6));
        // Zooming does not move floating windows.
        world.frame(world.tree().tile_of(WindowId(1)).unwrap(), 0.0);
        assert!(rect_of(&world, WindowId(2)).approx_eq(&Rect::new(600.0, 300.0, 400.0, 300.0), 1e-6));
        // Docking and floating again returns it to where it was.
        world.toggle_float(WindowId(2), 0.0);
        assert!(!world.tree().is_floating(WindowId(2)));
        world.toggle_float(WindowId(2), 0.0);
        assert!(rect_of(&world, WindowId(2)).approx_eq(&Rect::new(600.0, 300.0, 400.0, 300.0), 1e-6));
        let placements = world.placements(world.view(1.0));
        assert_eq!(placements.last().unwrap().window, WindowId(2), "floats come last, on top");
    }

    #[test]
    fn resizing_tiles_and_floats() {
        let mut world = world();
        world.add_window(WindowId(1), Placement::Dock, 0.0);
        world.add_window(WindowId(2), Placement::Dock, 0.0);
        // Focus is on 2 (right half), which has no right neighbour: moving the
        // divider right moves its left edge, so it shrinks.
        assert!(world.resize_focused(100.0, 0.0, 0.0));
        assert!(rect_of(&world, WindowId(2)).approx_eq(&Rect::new(900.0, 0.0, 700.0, 900.0), 1e-6));
        assert!(rect_of(&world, WindowId(1)).approx_eq(&Rect::new(0.0, 0.0, 900.0, 900.0), 1e-6));
        // Moving it left again grows it back.
        assert!(world.resize_focused(-100.0, 0.0, 0.0));
        assert!(rect_of(&world, WindowId(2)).approx_eq(&Rect::new(800.0, 0.0, 800.0, 900.0), 1e-6));
        // Nothing above or below: height cannot change.
        assert!(!world.resize_focused(0.0, 50.0, 0.0));
        world.add_window(WindowId(3), Placement::Float, 0.0);
        world.size_float(WindowId(3), 400.0, 300.0);
        world.resize_window(WindowId(3), Side::Left, -100.0, Side::Bottom, 50.0, 0.0);
        assert!(rect_of(&world, WindowId(3)).approx_eq(&Rect::new(500.0, 300.0, 500.0, 350.0), 1e-6));
    }

    #[test]
    fn cycling_and_split_toggle() {
        let mut world = world();
        world.add_window(WindowId(1), Placement::Dock, 0.0);
        world.add_window(WindowId(2), Placement::Dock, 0.0);
        world.add_window(WindowId(3), Placement::Dock, 0.0);
        world.cycle(true, 0.0);
        assert_eq!(world.focus(), Some(WindowId(1)));
        world.cycle(false, 0.0);
        assert_eq!(world.focus(), Some(WindowId(3)));
        // 2 and 3 are stacked in the right column; toggling lays them side by side.
        world.toggle_split(0.0);
        assert!(rect_of(&world, WindowId(3)).approx_eq(&Rect::new(1200.0, 0.0, 400.0, 900.0), 1e-6));
    }

    #[test]
    fn camera_animates_then_settles() {
        let mut world = World::new(Settings { gap: 0.0, ..Settings::default() }, Rect::new(0.0, 0.0, 1600.0, 900.0));
        world.add_window(WindowId(1), Placement::Dock, 0.0);
        world.add_window(WindowId(2), Placement::Dock, 0.0);
        world.frame(world.tree().tile_of(WindowId(2)).unwrap(), 10.0);
        assert!(world.is_animating(10.1));
        let mid = world.view(10.1);
        assert!(mid.w < 1.0 && mid.w > 0.5);
        assert!(!world.is_animating(11.0));
        assert!(world.view(11.0).approx_eq(&Rect::new(0.5, 0.0, 0.5, 1.0), 1e-9));
    }

    #[test]
    fn drop_targets_by_position() {
        let mut world = world();
        world.add_window(WindowId(1), Placement::Dock, 0.0);
        world.add_window(WindowId(2), Placement::Dock, 0.0);
        let first_tile = world.tree().tile_of(WindowId(1)).unwrap();
        let second_tile = world.tree().tile_of(WindowId(2)).unwrap();
        let view = world.view(1.0);
        // Middle of the left tile: become its tab.
        assert_eq!(world.drop_target(Point::new(400.0, 450.0), WindowId(2), view), Some(DropTarget::Tab(first_tile)));
        // Near its bottom edge: dock below it.
        assert_eq!(
            world.drop_target(Point::new(400.0, 850.0), WindowId(2), view),
            Some(DropTarget::Edge(first_tile, Side::Bottom))
        );
        // Over its own lone tile: nothing.
        assert_eq!(world.drop_target(Point::new(1200.0, 450.0), WindowId(2), view), None);
        // At the very edge of the screen: beside everything.
        let root = world.tree().root();
        assert_eq!(
            world.drop_target(Point::new(1595.0, 450.0), WindowId(1), view),
            Some(DropTarget::Edge(root, Side::Right))
        );
        let _ = second_tile;
    }

    #[test]
    fn dropping_rearranges() {
        let mut world = world();
        world.add_window(WindowId(1), Placement::Dock, 0.0);
        world.add_window(WindowId(2), Placement::Dock, 0.0);
        world.add_window(WindowId(3), Placement::Dock, 0.0);
        let first_tile = world.tree().tile_of(WindowId(1)).unwrap();
        world.apply_drop(WindowId(3), DropTarget::Tab(first_tile), 0.0);
        world.tree().check_invariants();
        assert_eq!(world.tree().windows_in(first_tile), vec![WindowId(1), WindowId(3)]);
        assert_eq!(world.focus(), Some(WindowId(3)));
        assert!(rect_of(&world, WindowId(3)).approx_eq(&Rect::new(0.0, 28.0, 800.0, 872.0), 1e-6));
        let strips = world.tab_strips(world.view(1.0));
        assert_eq!(strips.len(), 1);
        assert_eq!(strips[0].selected, 1);
        world.apply_drop(WindowId(3), DropTarget::Edge(first_tile, Side::Top), 0.0);
        world.tree().check_invariants();
        assert!(rect_of(&world, WindowId(3)).approx_eq(&Rect::new(0.0, 0.0, 800.0, 450.0), 1e-6));
        world.apply_drop(WindowId(3), DropTarget::Float, 0.0);
        assert!(world.tree().is_floating(WindowId(3)));
    }

    #[test]
    fn keyboard_moves() {
        let mut world = world();
        world.add_window(WindowId(1), Placement::Dock, 0.0);
        world.add_window(WindowId(2), Placement::Dock, 0.0);
        // [1 | 2], focus 2. Moving 2 left swaps them.
        world.move_focused(Dir::Left, 0.0);
        assert!(rect_of(&world, WindowId(2)).approx_eq(&Rect::new(0.0, 0.0, 800.0, 900.0), 1e-6));
        // Moving it up at the top edge docks it above everything.
        world.move_focused(Dir::Up, 0.0);
        world.tree().check_invariants();
        assert!(rect_of(&world, WindowId(2)).approx_eq(&Rect::new(0.0, 0.0, 1600.0, 450.0), 1e-6));
        assert!(rect_of(&world, WindowId(1)).approx_eq(&Rect::new(0.0, 450.0, 1600.0, 450.0), 1e-6));
    }

    #[test]
    fn tabs_cycle() {
        let mut world = world();
        world.add_window(WindowId(1), Placement::Dock, 0.0);
        let tile = world.tree().tile_of(WindowId(1)).unwrap();
        world.tree.tab(WindowId(2), tile);
        world.relayout();
        world.focus_window(WindowId(1), 0.0);
        assert_eq!(world.tree().shown_windows_in(tile), vec![WindowId(1)]);
        world.cycle_tab(1);
        assert_eq!(world.focus(), Some(WindowId(2)));
        world.cycle_tab(1);
        assert_eq!(world.focus(), Some(WindowId(1)));
        assert_eq!(world.tree().shown_windows_in(tile), vec![WindowId(1)]);
    }

    fn rects(world: &World) -> Vec<(WindowId, Rect)> {
        let mut out: Vec<_> =
            world.placements(world.view(1e9)).into_iter().map(|placement| (placement.window, placement.rect)).collect();
        out.sort_by_key(|(window, _)| *window);
        out
    }

    fn same_rects(first: &[(WindowId, Rect)], second: &[(WindowId, Rect)]) -> bool {
        first.len() == second.len()
            && first.iter().zip(second).all(|((first_window, first_rect), (second_window, second_rect))| {
                first_window == second_window && first_rect.approx_eq(second_rect, 1e-6)
            })
    }

    /// [1 | [2 / [3 | 4]]], with the column resized off centre.
    fn four() -> World {
        let mut world = world();
        for number in 1..=4 {
            world.add_window(WindowId(number), Placement::Dock, 0.0);
        }
        let (split, index) = world.tree().boundary(world.tree().tile_of(WindowId(1)).unwrap(), Side::Right).unwrap();
        assert!(world.resize_gap(split, index, 160.0, 0.0));
        world
    }

    #[test]
    fn minimizing_gives_the_space_away_and_restoring_takes_it_back() {
        let mut world = four();
        let before = rects(&world);
        world.focus_window(WindowId(3), 0.0);
        assert!(world.minimize(WindowId(3), 0.0));
        world.tree().check_invariants();
        assert!(world.is_minimized(WindowId(3)));
        assert!(!world.contains_window(WindowId(3)));
        assert!(rects(&world).iter().all(|(window, _)| *window != WindowId(3)));
        // Its neighbour takes the space, and focus moves on.
        assert!(rect_of(&world, WindowId(4)).approx_eq(&Rect::new(960.0, 450.0, 640.0, 450.0), 1e-6));
        assert_ne!(world.focus(), Some(WindowId(3)));
        assert!(world.restore(WindowId(3), 0.0));
        world.tree().check_invariants();
        assert!(!world.is_minimized(WindowId(3)));
        assert_eq!(world.focus(), Some(WindowId(3)));
        assert!(same_rects(&rects(&world), &before), "{:?}\nvs {before:?}", rects(&world));
    }

    #[test]
    fn restoring_rebuilds_a_split_that_collapsed() {
        let mut world = four();
        let before = rects(&world);
        // Without 2, its column collapses and [3 | 4] joins the top row.
        world.minimize(WindowId(2), 0.0);
        world.tree().check_invariants();
        assert_eq!(world.tree().children(world.tree().root()).len(), 4, "desktop, 1, 3, 4");
        world.restore(WindowId(2), 0.0);
        world.tree().check_invariants();
        assert!(same_rects(&rects(&world), &before), "{:?}\nvs {before:?}", rects(&world));
    }

    #[test]
    fn restoring_after_the_neighbour_closed_still_finds_a_place() {
        let mut world = four();
        world.minimize(WindowId(4), 0.0);
        world.remove_window(WindowId(3), 0.0);
        world.restore(WindowId(4), 0.0);
        world.tree().check_invariants();
        assert!(world.contains_window(WindowId(4)));
        assert_eq!(world.focus(), Some(WindowId(4)));
    }

    #[test]
    fn tabs_and_floats_go_back_where_they_were() {
        let mut world = world();
        world.add_window(WindowId(1), Placement::Dock, 0.0);
        let tile = world.tree().tile_of(WindowId(1)).unwrap();
        world.apply_drop(WindowId(2), DropTarget::Tab(tile), 0.0);
        world.add_window(WindowId(3), Placement::Float, 0.0);
        let float = Rect::new(100.0, 120.0, 400.0, 300.0);
        world.set_float_rect(WindowId(3), float);
        world.minimize(WindowId(2), 0.0);
        world.minimize(WindowId(3), 0.0);
        assert_eq!(world.tree().windows_in(tile), vec![WindowId(1)]);
        assert!(world.tree().floats().is_empty());
        assert_eq!(world.minimized().collect::<Vec<_>>(), vec![WindowId(2), WindowId(3)]);
        let framed = world.framed();
        assert!(world.restore_last(0.0));
        assert_eq!(world.float_rect(WindowId(3)), Some(float));
        assert_eq!(world.framed(), framed, "a floating window does not move the camera");
        assert!(world.restore_last(0.0));
        assert_eq!(world.tree().windows_in(tile), vec![WindowId(1), WindowId(2)]);
        assert!(!world.restore_last(0.0));
        world.tree().check_invariants();
    }

    #[test]
    fn going_to_a_saved_view_brings_its_minimized_windows_back() {
        let mut world = four();
        let column = world.tree().parent(world.tree().tile_of(WindowId(2)).unwrap()).unwrap();
        world.frame(column, 0.0);
        world.save_view(1);
        world.minimize(WindowId(2), 0.0);
        world.minimize(WindowId(1), 0.0);
        world.frame(world.tree().tile_of(WindowId(4)).unwrap(), 0.0);
        assert!(world.recall_view(1, 0.0));
        world.tree().check_invariants();
        assert!(!world.is_minimized(WindowId(2)), "part of the view: restored");
        assert!(world.is_minimized(WindowId(1)), "not part of the view: stays out");
        assert_eq!(world.tree().windows_in(world.framed()), vec![WindowId(2), WindowId(3), WindowId(4)]);
    }

    #[test]
    fn dropping_a_minimized_window_restores_it_there() {
        let mut world = four();
        world.minimize(WindowId(1), 0.0);
        let fourth_tile = world.tree().tile_of(WindowId(4)).unwrap();
        world.apply_drop(WindowId(1), DropTarget::Edge(fourth_tile, Side::Right), 0.0);
        world.tree().check_invariants();
        assert!(!world.is_minimized(WindowId(1)));
        assert_eq!(world.tree().parent(world.tree().tile_of(WindowId(1)).unwrap()), world.tree().parent(fourth_tile));
        assert_eq!(world.focus(), Some(WindowId(1)));
    }

    #[test]
    fn closing_a_minimized_window_forgets_it() {
        let mut world = four();
        world.minimize(WindowId(2), 0.0);
        world.remove_window(WindowId(2), 0.0);
        assert!(!world.is_minimized(WindowId(2)));
        assert!(!world.restore(WindowId(2), 0.0));
        assert!(!world.minimize(WindowId(2), 0.0));
    }

    /// Docking and closing never leave a split directly inside one along the
    /// same axis; restoring must not either.
    fn assert_no_nested_same_axis(tree: &Tree) {
        for node_id in tree.subtree(tree.root()) {
            if let Node::Split { axis, children, .. } = tree.node(node_id) {
                for child in children {
                    assert!(
                        !matches!(tree.node(*child), Node::Split { axis: child_axis, .. } if child_axis == axis),
                        "split {child:?} nested in split {node_id:?} along the same axis"
                    );
                }
            }
        }
    }

    #[test]
    fn restoring_beside_a_row_does_not_nest_a_row_in_it() {
        let mut world = world();
        // [1 | [2 / 3] | 4]
        let desktop = world.tree().desktop();
        let first_tile = world.tree.dock(WindowId(1), desktop, Side::Right);
        let second_tile = world.tree.dock(WindowId(2), first_tile, Side::Right);
        world.tree.dock(WindowId(3), second_tile, Side::Bottom);
        let column = world.tree().parent(second_tile).unwrap();
        world.tree.dock(WindowId(4), column, Side::Right);
        world.relayout();
        // 4 remembers the column [2 / 3] as its neighbour.
        world.minimize(WindowId(4), 0.0);
        // Then 2 and 3 end up side by side in the top row: [1 | 2 | 3].
        world.tree.detach(WindowId(3));
        let second_tile = world.tree().tile_of(WindowId(2)).unwrap();
        world.tree.dock(WindowId(3), second_tile, Side::Right);
        world.relayout();
        world.restore(WindowId(4), 0.0);
        world.tree().check_invariants();
        assert_no_nested_same_axis(world.tree());
        assert_eq!(world.tree().children(world.tree().root()).len(), 5, "desktop, 1, 2, 3, 4 in one row");
    }

    #[test]
    fn random_minimizing_keeps_every_window() {
        let mut seed: u64 = 7;
        let mut next = |limit: usize| {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            ((seed >> 33) % limit as u64) as usize
        };
        for _ in 0..50 {
            let mut world = world();
            let mut count = 0;
            for _ in 0..40 {
                match next(6) {
                    0 | 1 => {
                        count += 1;
                        let placement = if next(4) == 0 { Placement::Float } else { Placement::Dock };
                        world.add_window(WindowId(count), placement, 0.0);
                    }
                    2 | 3 if count > 0 => {
                        world.minimize(WindowId(1 + next(count as usize) as u64), 0.0);
                    }
                    _ => {
                        let out: Vec<_> = world.minimized().collect();
                        if !out.is_empty() {
                            assert!(world.restore(out[next(out.len())], 0.0));
                        }
                    }
                }
                world.tree().check_invariants();
                let mut all = world.tree().windows_in(world.tree().root());
                all.extend(world.minimized());
                all.sort();
                assert_eq!(all, (1..=count).map(WindowId).collect::<Vec<_>>());
            }
        }
    }
}
