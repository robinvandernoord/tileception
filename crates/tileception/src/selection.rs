//! Multiple selection and actions on the selected windows.

use std::collections::BTreeSet;

use smithay::backend::renderer::gles::GlesRenderer;
use tileception_core::{DropTarget, Point, Rect, WindowId};

use crate::{
    input::Action,
    render::{push_shape, OutputElement},
    shapes::Shape,
    Tileception,
};

#[derive(Default)]
pub struct Selection {
    pub windows: BTreeSet<WindowId>,
    pub anchor: Option<Point>,
    pub additive: BTreeSet<WindowId>,
}

impl Tileception {
    pub fn toggle_selection(&mut self, window: WindowId) {
        if self.selection.windows.is_empty() {
            if let Some(focus) = self.world.focus().filter(|id| *id != window) {
                self.selection.windows.insert(focus);
            }
        }
        if !self.selection.windows.remove(&window) {
            self.selection.windows.insert(window);
        }
    }

    pub fn selection_motion(&mut self, point: Point) {
        let Some(anchor) = self.selection.anchor else { return };
        let rect = Rect::new(
            anchor.x.min(point.x),
            anchor.y.min(point.y),
            (point.x - anchor.x).abs(),
            (point.y - anchor.y).abs(),
        );
        let selected = self.drawn_windows(self.now()).into_iter().filter(|d| d.content.intersects(&rect)).map(|d| d.id);
        self.selection.windows = self.selection.additive.iter().copied().chain(selected).collect();
    }

    pub fn selection_action(&mut self, action: Action) -> bool {
        if self.selection.windows.is_empty() {
            return false;
        }
        let windows: Vec<_> =
            self.selection.windows.iter().copied().filter(|w| self.world.contains_window(*w)).collect();
        let now = self.now();
        match action {
            Action::Close => {
                for window in windows {
                    self.close_window(window);
                }
                self.selection.windows.clear();
            }
            Action::ToggleFloat => {
                let float = windows.iter().any(|w| !self.world.tree().is_floating(*w));
                for window in windows {
                    if self.world.tree().is_floating(window) != float {
                        self.world.toggle_float(window, now);
                    }
                }
            }
            Action::Move(dir) => {
                self.world.move_windows(&windows, dir, now);
            }
            Action::Group => {
                let Some(first) = windows.first().copied() else { return true };
                if self.world.tree().is_floating(first) {
                    self.world.toggle_float(first, now);
                }
                for window in windows.into_iter().skip(1) {
                    if let Some(tile) = self.world.tree().tile_of(first) {
                        self.world.apply_drop(window, DropTarget::Tab(tile), now);
                    }
                }
            }
            _ => return false,
        }
        self.apply_layout();
        true
    }

    pub fn selection_elements(&self, renderer: &mut GlesRenderer) -> Vec<OutputElement> {
        if self.selection.windows.is_empty() && self.selection.anchor.is_none() {
            return Vec::new();
        }
        let mut out = Vec::new();
        let shapes = self.shapes(renderer);
        let t = &self.theme;
        for d in self.drawn_windows(self.now()) {
            if self.selection.windows.contains(&d.id) {
                let badge = Rect::new(d.slot.right() - 26.0, d.slot.y + 6.0, 20.0, 20.0);
                self.ui_label(&mut out, renderer, "+", badge.inset(4.0), t.on_accent, true);
                push_shape(&mut out, shapes.as_ref(), &badge, Shape::filled(t.accent, 4.0));
                push_shape(
                    &mut out,
                    shapes.as_ref(),
                    &d.slot,
                    Shape { radius: t.radius, border_width: 2.0, border: t.accent, ..Default::default() },
                );
            }
        }
        if let Some(anchor) = self.selection.anchor {
            let point = Point::new(self.pointer.x, self.pointer.y);
            let rect = Rect::new(
                anchor.x.min(point.x),
                anchor.y.min(point.y),
                (point.x - anchor.x).abs(),
                (point.y - anchor.y).abs(),
            );
            push_shape(
                &mut out,
                shapes.as_ref(),
                &rect,
                Shape { border_width: 1.0, border: t.accent, ..Shape::filled(t.accent.alpha(0.12), 2.0) },
            );
        }
        out
    }
}
