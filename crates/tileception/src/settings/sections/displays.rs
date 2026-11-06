use super::*;

pub(super) struct Monitor {
    pub name: String,
    pub width: f64,
    pub height: f64,
    pub scale: f64,
    pub auto_scale: f64,
}

pub(super) struct Drag {
    pub row: usize,
    grab: Point,
    origin: Point,
    factor: f64,
}

impl Screen {
    pub(super) fn display_rects(&self) -> Vec<(usize, Rect)> {
        let mut next = 0.;
        let mut indices: Vec<_> = (0..self.rows.len()).filter(|index| self.rows[*index].name != "*").collect();
        indices.sort_by_key(|index| {
            (
                !self.monitors.iter().any(|monitor| monitor.name == self.rows[*index].name),
                self.rows[*index].name.clone(),
            )
        });
        let mut offline = None;
        indices
            .into_iter()
            .map(|index| {
                let row = &self.rows[index];
                let monitor = self.monitors.iter().find(|monitor| monitor.name == row.name);
                let setting = if row.inherited && !row.changed() {
                    self.rows
                        .iter()
                        .find(|row| row.name == "*")
                        .and_then(|row| row.value.get("scale"))
                        .or_else(|| row.value.get("scale"))
                } else {
                    row.value.get("scale")
                };
                let initial =
                    self.initial.iter().find(|(name, _)| *name == row.name).and_then(|(_, value)| value.get("scale"));
                let scale = if monitor.is_some() && setting == initial {
                    monitor.map_or(1., |monitor| monitor.scale)
                } else {
                    setting
                        .and_then(|value| value.as_float().or_else(|| value.as_integer().map(|value| value as f64)))
                        .filter(|value| value.is_finite() && *value >= 0.5 && *value <= 4.)
                        .map(crate::scale::tidy)
                        .unwrap_or_else(|| monitor.map_or(1., |monitor| monitor.auto_scale))
                };
                let width = monitor.map_or(1280., |monitor| monitor.width) / scale;
                let height = monitor.map_or(720., |monitor| monitor.height) / scale;
                let configured = row.value.get("position").and_then(toml::Value::as_array);
                let position = configured
                    .and_then(|position| {
                        Some(Point::new(position.first()?.as_integer()? as f64, position.get(1)?.as_integer()? as f64))
                    })
                    .unwrap_or(Point::new(if monitor.is_some() { next } else { offline.unwrap_or(next + 160.) }, 0.));
                if monitor.is_some() {
                    next = next.max(position.x + width);
                } else {
                    offline = Some(position.x + width);
                }
                (index, Rect::new(position.x, position.y, width, height))
            })
            .collect()
    }

    pub(super) fn display_map(&self, area: Rect) -> (Point, f64) {
        if let Some(drag) = &self.map_drag {
            return (drag.origin, drag.factor);
        }
        let rects = self.display_rects();
        let left = rects.iter().map(|(_, rect)| rect.x).reduce(f64::min).unwrap_or(0.);
        let top = rects.iter().map(|(_, rect)| rect.y).reduce(f64::min).unwrap_or(0.);
        let right = rects.iter().map(|(_, rect)| rect.right()).reduce(f64::max).unwrap_or(1280.);
        let bottom = rects.iter().map(|(_, rect)| rect.bottom()).reduce(f64::max).unwrap_or(720.);
        let factor = ((area.w - 48.) / (right - left).max(1.)).min((area.h - 40.) / (bottom - top).max(1.)).max(0.001);
        (
            Point::new(
                area.x + (area.w - (right - left) * factor) / 2. - left * factor,
                area.y + (area.h - (bottom - top) * factor) / 2. - top * factor,
            ),
            factor,
        )
    }

    pub(super) fn automatic_displays(&mut self) {
        for row in &mut self.rows {
            if let Some(table) = row.value.as_table_mut() {
                table.remove("position");
            }
        }
        self.map_drag = None;
    }
}

impl Tileception {
    pub(super) fn section_outputs(&self) -> Vec<String> {
        let mut names: Vec<_> = self.all_outputs().iter().map(|output| output.name()).collect();
        names.extend(self.config.file.outputs.keys().filter(|name| name.as_str() != "*").cloned());
        if let Some(screen) = &self.settings.section
            && screen.kind == Kind::Displays
        {
            names.extend(screen.rows.iter().filter(|row| row.name != "*").map(|row| row.name.clone()));
        }
        names.sort();
        names.dedup();
        names
    }

    pub(super) fn section_detected_displays(&self, screen: &mut Screen) {
        if screen.kind != Kind::Displays {
            return;
        }
        screen.monitors.clear();
        for output in self.all_outputs() {
            let Some(mode) = output.current_mode() else { continue };
            let physical = output.physical_properties().size;
            let size = output.current_transform().transform_size(mode.size);
            screen.monitors.push(Monitor {
                name: output.name(),
                width: size.w as f64,
                height: size.h as f64,
                scale: output.current_scale().fractional_scale(),
                auto_scale: crate::scale::guess((physical.w, physical.h), (mode.size.w, mode.size.h)),
            });
            if !screen.rows.iter().any(|row| row.name == output.name()) {
                let scale = screen
                    .rows
                    .iter()
                    .find(|row| row.name == "*")
                    .and_then(|row| row.value.get("scale"))
                    .cloned()
                    .unwrap_or_else(|| toml::Value::String("auto".into()));
                let inherited = toml::Value::Table(toml::Table::from_iter([("scale".into(), scale)]));
                screen.initial.push((output.name(), inherited.clone()));
                screen.rows.push(Row {
                    name: output.name(),
                    original_name: output.name(),
                    value: inherited.clone(),
                    original: inherited,
                    source: None,
                    inherited: true,
                    builtin: None,
                });
            }
        }
        if screen.selected.is_none() && !screen.rows.is_empty() {
            screen.selected = Some(0);
        }
    }

    pub(super) fn section_map_press(&mut self, point: Point, area: Rect) {
        let Some(screen) = self.settings.section.as_mut() else { return };
        if let Err(error) = screen.finish_input() {
            screen.error = error;
            return;
        }
        let (origin, factor) = screen.display_map(area);
        let rects = screen.display_rects();
        let selected = rects.iter().rev().find(|(_, rect)| {
            Rect::new(origin.x + rect.x * factor, origin.y + rect.y * factor, rect.w * factor, rect.h * factor)
                .contains(point)
        });
        if let Some((index, rect)) = selected {
            screen.selected = Some(*index);
            screen.scroll = 0.;
            screen.map_drag = Some(Drag {
                row: *index,
                grab: Point::new((point.x - origin.x) / factor - rect.x, (point.y - origin.y) / factor - rect.y),
                origin,
                factor,
            });
        }
        self.settings.editor_epoch = self.settings.editor_epoch.wrapping_add(1);
        self.schedule_render();
    }

    pub(in crate::settings) fn section_map_motion(&mut self, point: Point) {
        let Some(screen) = self.settings.section.as_mut() else { return };
        let Some(drag) = &screen.map_drag else { return };
        let rects = screen.display_rects();
        let Some((_, old)) = rects.iter().find(|(index, _)| *index == drag.row) else { return };
        let mut left = (point.x - drag.origin.x) / drag.factor - drag.grab.x;
        let mut top = (point.y - drag.origin.y) / drag.factor - drag.grab.y;
        let threshold = 12. / drag.factor;
        let dragged = drag.row;
        for (index, rect) in &rects {
            if *index == drag.row {
                continue;
            }
            for edge in [rect.x, rect.right(), rect.x - old.w, rect.right() - old.w] {
                if (edge - left).abs() < threshold {
                    left = edge;
                }
            }
            for edge in [rect.y, rect.bottom(), rect.y - old.h, rect.bottom() - old.h] {
                if (edge - top).abs() < threshold {
                    top = edge;
                }
            }
        }
        // Freeze automatically arranged neighbours when switching to a manual layout.
        let selected = screen.selected;
        for (index, rect) in rects {
            screen.selected = Some(index);
            let position = if index == dragged { [left, top] } else { [rect.x, rect.y] };
            screen.write(
                &Path::Key("position".into()),
                toml::Value::Array(
                    position
                        .map(|value| toml::Value::Integer(value.round().clamp(i32::MIN as f64, i32::MAX as f64) as i64))
                        .to_vec(),
                ),
            );
        }
        screen.selected = selected;
        self.schedule_render();
    }

    pub(in crate::settings) fn section_map_dragging(&self) -> bool {
        self.settings.section.as_ref().is_some_and(|screen| screen.map_drag.is_some())
    }

    pub(in crate::settings) fn section_map_release(&mut self) {
        if let Some(screen) = self.settings.section.as_mut() {
            screen.map_drag = None;
        }
    }
}
