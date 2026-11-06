use super::*;

#[derive(PartialEq)]
pub(super) struct Monitor {
    pub name: String,
    pub width: f64,
    pub height: f64,
    pub scale: f64,
    pub auto_scale: f64,
    pub modes: Vec<smithay::output::Mode>,
}

pub(super) fn mode_string(mode: smithay::output::Mode) -> String {
    format!("{}x{}@{}", mode.size.w, mode.size.h, f64::from(mode.refresh) / 1000.)
}

pub(super) struct Drag {
    pub row: usize,
    grab: Point,
    origin: Point,
    factor: f64,
    actual: Vec<(usize, Rect)>,
    shown: Vec<(usize, Rect)>,
    position: Point,
    equal: bool,
}

impl Screen {
    pub(super) fn validate_displays(&self) -> Result<(), String> {
        if self.kind != Kind::Displays {
            return Ok(());
        }
        let rects = self.display_rects();
        for (position, (index, rect)) in rects.iter().enumerate() {
            for (other, candidate) in &rects[position + 1..] {
                if overlaps(*rect, *candidate) {
                    return Err(format!(
                        "{} overlaps {}. Drag a display to a free edge, or use Automatic layout.",
                        self.rows[*index].name, self.rows[*other].name
                    ));
                }
            }
        }
        Ok(())
    }

    pub(super) fn display_rects(&self) -> Vec<(usize, Rect)> {
        let mut next = 0.;
        let mut indices: Vec<_> = (0..self.rows.len())
            .filter(|index| {
                self.rows[*index].name != "*"
                    && self.rows[*index].value.get("enabled").and_then(toml::Value::as_bool) != Some(false)
            })
            .collect();
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
                let setting: crate::scale::OutputConfig = row.value.clone().try_into().unwrap_or_default();
                let mode = setting.mode.parse().ok().flatten();
                let size = mode.map_or_else(
                    || (monitor.map_or(1280., |monitor| monitor.width), monitor.map_or(720., |monitor| monitor.height)),
                    |mode| (f64::from(mode.size.w), f64::from(mode.size.h)),
                );
                let transform: smithay::utils::Transform = setting.transform.into();
                let size = transform.transform_size(smithay::utils::Size::<f64, smithay::utils::Physical>::from(size));
                let width = size.w / scale;
                let height = size.h / scale;
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

    /// The arrangement represents proportional edge crossing with equal
    /// rectangles. Keep the real geometry separate for saved positions.
    pub(super) fn display_map_rects(&self, equal: bool) -> Vec<(usize, Rect)> {
        if let Some(drag) = &self.map_drag {
            let mut rects = drag.shown.clone();
            if let Some((_, rect)) = rects.iter_mut().find(|(index, _)| *index == drag.row) {
                rect.x = drag.position.x;
                rect.y = drag.position.y;
            }
            return rects;
        }
        let mut rects = self.display_rects();
        if !equal {
            return rects;
        }
        let width = rects.iter().map(|(_, rect)| rect.w).reduce(f64::max).unwrap_or(1280.);
        let height = rects.iter().map(|(_, rect)| rect.h).reduce(f64::max).unwrap_or(720.);
        let axis = |horizontal: bool, extent: f64| {
            let mut entries: Vec<_> = rects
                .iter()
                .map(|(index, rect)| {
                    (*index, if horizontal { rect.x } else { rect.y }, if horizontal { rect.w } else { rect.h })
                })
                .collect();
            entries.sort_by(|left, right| left.1.total_cmp(&right.1));
            let mut result: Vec<(usize, f64)> = Vec::new();
            let mut placed: Vec<(f64, f64, f64)> = Vec::new();
            let mut previous = None;
            let mut mapped: f64 = 0.;
            for (index, start, length) in entries {
                if let Some((before, size)) = previous {
                    mapped += (start - before) / size * extent;
                }
                for (before, size, location) in &placed {
                    if start >= before + size - 0.5 {
                        mapped = mapped.max(location + extent);
                    }
                }
                result.push((index, mapped));
                placed.push((start, length, mapped));
                previous = Some((start, length));
            }
            result
        };
        let horizontal = axis(true, width);
        let vertical = axis(false, height);
        for (index, rect) in &mut rects {
            rect.x = horizontal.iter().find(|(candidate, _)| candidate == index).map_or(0., |(_, position)| *position);
            rect.y = vertical.iter().find(|(candidate, _)| candidate == index).map_or(0., |(_, position)| *position);
            rect.w = width;
            rect.h = height;
        }
        rects
    }

    pub(super) fn display_map(&self, area: Rect, equal: bool) -> (Point, f64) {
        if let Some(drag) = &self.map_drag {
            return (drag.origin, drag.factor);
        }
        let rects = self.display_map_rects(equal);
        let left = rects.iter().map(|(_, rect)| rect.x).reduce(f64::min).unwrap_or(0.);
        let top = rects.iter().map(|(_, rect)| rect.y).reduce(f64::min).unwrap_or(0.);
        let right = rects.iter().map(|(_, rect)| rect.right()).reduce(f64::max).unwrap_or(1280.);
        let bottom = rects.iter().map(|(_, rect)| rect.bottom()).reduce(f64::max).unwrap_or(720.);
        // Leave room on every side for a new neighbour, including above/below.
        let spare_width = rects.iter().map(|(_, rect)| rect.w).reduce(f64::max).unwrap_or(1280.) * 1.3;
        let spare_height = rects.iter().map(|(_, rect)| rect.h).reduce(f64::max).unwrap_or(720.) * 1.8;
        let factor = ((area.w - 48.) / (right - left + spare_width).max(1.))
            .min((area.h - 40.) / (bottom - top + spare_height).max(1.))
            .max(0.001);
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
    pub(in crate::settings) fn section_map_dragging(&self) -> bool {
        self.settings.section.as_ref().is_some_and(|screen| screen.map_drag.is_some())
    }

    pub(super) fn section_outputs(&self) -> Vec<String> {
        let mut names: Vec<_> = self.connected_monitors().into_iter().map(|monitor| monitor.name).collect();
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

    pub(in crate::settings) fn section_detected_displays(&self, screen: &mut Screen) {
        if screen.kind != Kind::Displays {
            return;
        }
        let previous = std::mem::take(&mut screen.monitors);
        for monitor in self.connected_monitors() {
            let Some(mode) = monitor.preferred_mode.or(monitor.current_mode) else { continue };
            let output = self.output_named(&monitor.name);
            let physical = output.as_ref().map(|output| output.physical_properties().size);
            let mut modes = monitor.modes;
            modes.sort_by_key(|mode| (mode.size.w, mode.size.h, mode.refresh));
            modes.dedup();
            screen.monitors.push(Monitor {
                name: monitor.name.clone(),
                width: mode.size.w as f64,
                height: mode.size.h as f64,
                scale: output.as_ref().map_or(1., |output| output.current_scale().fractional_scale()),
                auto_scale: physical
                    .map_or(1., |physical| crate::scale::guess((physical.w, physical.h), (mode.size.w, mode.size.h))),
                modes,
            });
            if !screen.rows.iter().any(|row| row.name == monitor.name) {
                let inherited = screen
                    .rows
                    .iter()
                    .find(|row| row.name == "*")
                    .map(|row| row.value.clone())
                    .unwrap_or_else(|| toml::Value::Table(toml::Table::new()));
                screen.initial.push((monitor.name.clone(), inherited.clone()));
                screen.rows.push(Row {
                    name: monitor.name.clone(),
                    original_name: monitor.name,
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
        if previous != screen.monitors {
            self.invalidate_settings_client();
        }
    }

    pub(super) fn section_map_start(&mut self, row: usize, point: Point, area: Rect) {
        let equal = self.settings.draft[super::edges()].as_ref().and_then(toml::Value::as_bool).unwrap_or(false);
        let Some(screen) = self.settings.section.as_mut() else { return };
        if let Err(error) = screen.finish_input() {
            screen.error = error;
            return;
        }
        let (origin, factor) = screen.display_map(area, equal);
        let rects = screen.display_map_rects(equal);
        let selected = rects.iter().find(|(index, _)| *index == row);
        if let Some((index, rect)) = selected {
            screen.selected = Some(*index);
            screen.scroll = 0.;
            screen.map_drag = Some(Drag {
                row: *index,
                grab: Point::new((point.x - origin.x) / factor - rect.x, (point.y - origin.y) / factor - rect.y),
                origin,
                factor,
                actual: screen.display_rects(),
                shown: rects.clone(),
                position: Point::new(rect.x, rect.y),
                equal,
            });
        }
        self.settings.editor_epoch = self.settings.editor_epoch.wrapping_add(1);
        self.schedule_render();
    }

    pub(in crate::settings) fn section_map_motion(&mut self, point: Point) {
        let Some(screen) = self.settings.section.as_mut() else { return };
        let Some(drag) = &screen.map_drag else { return };
        let rects = drag.shown.clone();
        let Some((_, old)) = rects.iter().find(|(index, _)| *index == drag.row) else { return };
        let Some((_, actual)) = drag.actual.iter().find(|(index, _)| *index == drag.row) else { return };
        let left = (point.x - drag.origin.x) / drag.factor - drag.grab.x;
        let top = (point.y - drag.origin.y) / drag.factor - drag.grab.y;
        let dragged = drag.row;
        let equal = drag.equal;
        let mut candidates = Vec::new();
        for (index, rect) in &rects {
            if *index == drag.row {
                continue;
            }
            let Some((_, real)) = drag.actual.iter().find(|(candidate, _)| candidate == index) else { continue };
            let offset_x = if equal { 0. } else { (left - rect.x).clamp(-old.w + 1., rect.w - 1.) };
            let offset_y = if equal { 0. } else { (top - rect.y).clamp(-old.h + 1., rect.h - 1.) };
            for (shown, saved) in [
                (
                    Rect::new(rect.x - old.w, rect.y + offset_y, old.w, old.h),
                    Rect::new(real.x - actual.w, real.y + offset_y, actual.w, actual.h),
                ),
                (
                    Rect::new(rect.right(), rect.y + offset_y, old.w, old.h),
                    Rect::new(real.right(), real.y + offset_y, actual.w, actual.h),
                ),
                (
                    Rect::new(rect.x + offset_x, rect.y - old.h, old.w, old.h),
                    Rect::new(real.x + offset_x, real.y - actual.h, actual.w, actual.h),
                ),
                (
                    Rect::new(rect.x + offset_x, rect.bottom(), old.w, old.h),
                    Rect::new(real.x + offset_x, real.bottom(), actual.w, actual.h),
                ),
            ] {
                if rects.iter().any(|(other, rect)| *other != dragged && overlaps(shown, *rect))
                    || drag.actual.iter().any(|(other, rect)| *other != dragged && overlaps(saved, *rect))
                {
                    continue;
                }
                let distance = (shown.x - left).powi(2) + (shown.y - top).powi(2);
                candidates.push((distance, shown, saved));
            }
        }
        candidates.sort_by(|left, right| left.0.total_cmp(&right.0));
        let Some((_, shown, saved)) = candidates.first() else { return };
        let (left, top, actual_left, actual_top) = (shown.x, shown.y, saved.x, saved.y);
        let actual_rects = drag.actual.clone();
        if let Some(drag) = &mut screen.map_drag {
            drag.position = Point::new(left, top);
        }
        // Freeze automatically arranged neighbours when switching to a manual layout.
        let selected = screen.selected;
        for (index, rect) in actual_rects {
            screen.selected = Some(index);
            let position = if index == dragged { [actual_left, actual_top] } else { [rect.x, rect.y] };
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
        screen.error = screen.validate_displays().err().unwrap_or_default();
        self.schedule_render();
    }

    pub(in crate::settings) fn section_map_release(&mut self) {
        if let Some(screen) = self.settings.section.as_mut() {
            screen.map_drag = None;
        }
    }
}

fn overlaps(left: Rect, right: Rect) -> bool {
    left.right().min(right.right()) - left.x.max(right.x) > 0.5
        && left.bottom().min(right.bottom()) - left.y.max(right.y) > 0.5
}
