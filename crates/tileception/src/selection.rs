//! Multiple selection and actions on the selected windows.

use std::collections::BTreeSet;

use smithay::backend::renderer::gles::GlesRenderer;
use tileception_core::{DropTarget, Point, Rect, WindowId};

use crate::{
    Tileception,
    input::Action,
    render::{OutputElement, push_image, push_shape},
    shapes::Shape,
    theme::Colour,
};

#[derive(Default)]
pub struct Selection {
    pub windows: BTreeSet<WindowId>,
    pub anchor: Option<Point>,
    pub additive: BTreeSet<WindowId>,
}

impl Tileception {
    pub fn toggle_selection(&mut self, window: WindowId) {
        if self.selection.windows.is_empty()
            && let Some(focus) = self.world.focus().filter(|focused| *focused != window)
        {
            self.selection.windows.insert(focus);
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
        let selected = self
            .drawn_windows(self.now())
            .into_iter()
            .filter(|drawn| drawn.content.intersects(&rect))
            .map(|drawn| drawn.window_id);
        self.selection.windows = self.selection.additive.iter().copied().chain(selected).collect();
    }

    pub fn selection_action(&mut self, action: Action) -> bool {
        if self.selection.windows.is_empty() {
            return false;
        }
        let windows: Vec<_> =
            self.selection.windows.iter().copied().filter(|window| self.world.contains_window(*window)).collect();
        let now = self.now();
        match action {
            Action::Close => {
                for window in windows {
                    self.close_window(window);
                }
                self.selection.windows.clear();
            }
            Action::ToggleFloat => {
                let float = windows.iter().any(|window| !self.world.tree().is_floating(*window));
                for window in windows {
                    if self.world.tree().is_floating(window) != float {
                        self.world.toggle_float(window, now);
                    }
                }
            }
            Action::Move(dir) => {
                self.world.move_windows(&windows, dir, now);
            }
            Action::StepIn => {
                self.world.frame_windows(&windows, now);
            }
            Action::ToggleSplit if windows.len() > 1 => {
                self.world.toggle_split_windows(&windows, now);
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
            Action::Minimize => {
                for window in windows {
                    self.minimize_window(window);
                }
                self.selection.windows.clear();
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
        let theme = &self.theme;
        for drawn in self.drawn_windows(self.now()) {
            if self.selection.windows.contains(&drawn.window_id) {
                // A tint and accent outline over the window, and a check mark in its top left corner.
                let badge = Rect::new(drawn.slot.x + 8.0, drawn.slot.y + 8.0, 22.0, 22.0);
                if let Some(check) = self.glyphs.image(crate::ui::glyph::CHECK, 14, theme.on_accent) {
                    push_image(&mut out, renderer, &check, (badge.x + 4.0, badge.y + 4.0), self.scale);
                }
                push_shape(
                    &mut out,
                    shapes.as_ref(),
                    &badge,
                    Shape {
                        radius: 11.0,
                        fill: (theme.accent, theme.accent_end),
                        glow_radius: 10.0,
                        glow: Colour([0.0, 0.0, 0.0, 0.4]),
                        ..Default::default()
                    },
                );
                push_shape(
                    &mut out,
                    shapes.as_ref(),
                    &drawn.slot,
                    Shape {
                        radius: theme.radius,
                        fill: (theme.accent.alpha(0.10), theme.accent_end.alpha(0.10)),
                        border_width: 2.0,
                        border: theme.accent,
                        glow_radius: theme.glow * 0.6,
                        glow: theme.accent.alpha(0.2),
                        ..Default::default()
                    },
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
                Shape { border_width: 1.0, border: theme.accent, ..Shape::filled(theme.accent.alpha(0.12), 2.0) },
            );
        }
        out
    }
}
