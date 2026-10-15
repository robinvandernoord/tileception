//! The Tileception layout engine: a nested tree of tiles per display, a camera that
//! frames any node of it, and the navigation built on top. It has no platform
//! dependencies, so the compositor and the tests drive it the same way.

pub mod geom;
pub mod layout;
pub mod tree;
pub mod world;

pub use geom::{Point, Rect, Xform};
pub use layout::Layout;
pub use tree::{Axis, Node, NodeId, Side, Tree, WindowId};
pub use world::{Dir, DropTarget, Placement, Settings, TabStrip, WindowPlacement, WindowRole, World};
