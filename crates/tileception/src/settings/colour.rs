//! The colour picker: a popover under a colour field's swatch with a
//! saturation and brightness square, a hue slider and preset colours. Every
//! change goes straight into the draft, so the preview follows along.

use super::*;

/// Accents that sit well on the dark and light themes.
const PRESETS: [&str; 8] = ["#2bd4a7", "#3fb6e8", "#4f8cff", "#9b7bff", "#ff6fae", "#ff5f56", "#ff9f43", "#ecd75a"];

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Part {
    Square,
    Hue,
}

#[derive(Clone, PartialEq)]
enum PointerAction {
    Drag(Part),
    Preset(&'static str),
    Passive,
}

impl crate::ui::PointerAction for PointerAction {
    fn activate(&self, state: &mut Tileception, _rect: Rect, point: Point) {
        match self {
            Self::Drag(part) => {
                if let Some(picker) = &mut state.settings.colour {
                    picker.drag = Some(*part);
                }
                state.colour_motion(point);
            }
            Self::Preset(preset) => {
                if let Some(current) = &state.settings.colour {
                    let mut picker =
                        Picker::new(current.index, &format!("{preset}{}", current.alpha), state.theme.accent);
                    picker.level = current.level;
                    state.settings.colour = Some(picker);
                    state.colour_commit();
                }
            }
            Self::Passive => {}
        }
        state.schedule_render();
    }
}

pub(super) struct Picker {
    pub index: usize,
    /// A battery colour threshold being coloured, rather than the field itself.
    pub level: Option<usize>,
    hue: f64,
    saturation: f64,
    value: f64,
    /// The `aa` of a `#rrggbbaa` colour, kept as it was.
    alpha: String,
    pub drag: Option<Part>,
}

fn hsv(hue: f64, saturation: f64, value: f64) -> [f64; 3] {
    let sector = (hue.rem_euclid(1.) * 6.).min(5.999_999);
    let fraction = sector.fract();
    let (low, falling, rising) =
        (value * (1. - saturation), value * (1. - saturation * fraction), value * (1. - saturation * (1. - fraction)));
    match sector as u8 {
        0 => [value, rising, low],
        1 => [falling, value, low],
        2 => [low, value, rising],
        3 => [low, falling, value],
        4 => [rising, low, value],
        _ => [value, low, falling],
    }
}

fn colour([red, green, blue]: [f64; 3]) -> Colour {
    Colour([red as f32, green as f32, blue as f32, 1.])
}

impl Picker {
    fn new(index: usize, text: &str, fallback: Colour) -> Picker {
        let [red, green, blue, _] =
            Colour::parse(text).unwrap_or(fallback).straight_rgba8().map(|byte| byte as f64 / 255.);
        let value = red.max(green).max(blue);
        let spread = value - red.min(green).min(blue);
        let saturation = if value > 0. { spread / value } else { 0. };
        let hue = if spread == 0. {
            0.
        } else if value == red {
            ((green - blue) / spread).rem_euclid(6.) / 6.
        } else if value == green {
            ((blue - red) / spread + 2.) / 6.
        } else {
            ((red - green) / spread + 4.) / 6.
        };
        let digits = text.trim().trim_start_matches('#');
        let alpha =
            if digits.len() == 8 && Colour::parse(text).is_ok() { digits[6..].to_string() } else { String::new() };
        Picker { index, level: None, hue, saturation, value, alpha, drag: None }
    }

    fn hex(&self) -> String {
        let [red, green, blue] =
            hsv(self.hue, self.saturation, self.value).map(|channel| (channel * 255.).round() as u8);
        format!("#{red:02x}{green:02x}{blue:02x}{}", self.alpha)
    }
}

/// The parts of an open picker.
struct Layout {
    panel: Rect,
    square: Rect,
    hue: Rect,
    presets: Vec<Rect>,
}

fn layout(panel: Rect) -> Layout {
    let inner = panel.w - 24.;
    let size = 22.;
    let gap = (inner - size * PRESETS.len() as f64) / (PRESETS.len() - 1) as f64;
    Layout {
        panel,
        square: Rect::new(panel.x + 12., panel.y + 12., inner, 140.),
        hue: Rect::new(panel.x + 12., panel.y + 166., inner, 14.),
        presets: (0..PRESETS.len())
            .map(|position| Rect::new(panel.x + 12. + position as f64 * (size + gap), panel.y + 194., size, size))
            .collect(),
    }
}

impl Tileception {
    pub(super) fn colour_pointer_target(&self, point: Point) -> Option<interaction::Target> {
        let layout = self.colour_layout(&self.settings_frame())?;
        if !layout.panel.contains(point) {
            return None;
        }
        for (rect, part) in [(layout.square, Part::Square), (layout.hue, Part::Hue)] {
            if rect.contains(point) {
                return Some(interaction::Target::new(
                    rect,
                    PointerAction::Drag(part),
                    smithay::input::pointer::CursorIcon::Crosshair,
                ));
            }
        }
        for (rect, preset) in layout.presets.iter().zip(PRESETS) {
            if rect.contains(point) {
                return Some(interaction::Target::button(*rect, PointerAction::Preset(preset)));
            }
        }
        Some(interaction::Target::new(
            layout.panel,
            PointerAction::Passive,
            smithay::input::pointer::CursorIcon::Default,
        ))
    }

    /// Where the picker for this field opens: below its control, or above it
    /// near the bottom of Settings.
    fn colour_layout(&self, frame: &Frame) -> Option<Layout> {
        let picker = self.settings.colour.as_ref()?;
        let control = match picker.level {
            Some(level) => level_parts(frame.levels.iter().find(|(_, shown)| *shown == level)?.0).swatch,
            None => control_rect(frame.rows.iter().find(|(_, field)| *field == picker.index)?.0),
        };
        let (width, height) = (256., 228.);
        let below = control.bottom() + 6.;
        let top = if below + height > frame.panel.bottom() - 64. { control.y - 6. - height } else { below };
        Some(layout(Rect::new(control.right() - width, top.max(frame.panel.y + 70.), width, height)))
    }

    pub(super) fn open_colour_picker(&mut self, index: usize) {
        let text = self.settings.draft[index].as_ref().and_then(toml::Value::as_str).unwrap_or("");
        let fallback = self.settings.defaults[index]
            .as_ref()
            .and_then(toml::Value::as_str)
            .and_then(|text| Colour::parse(text).ok())
            .unwrap_or(self.theme.accent);
        self.settings.colour = Some(Picker::new(index, text, fallback));
        self.settings.dropdown = None;
    }

    /// The picker for a battery colour threshold. A theme colour's name
    /// starts it at that colour; choosing one writes a hex colour.
    pub(super) fn open_level_picker(&mut self, level: usize) {
        let text = self.settings.levels().get(level).map(|(_, colour)| colour.clone()).unwrap_or_default();
        let start = named_colour(&self.theme, &text).unwrap_or(self.theme.warning);
        let [red, green, blue, _] = start.straight_rgba8();
        let mut picker = Picker::new(levels_index(), &format!("#{red:02x}{green:02x}{blue:02x}"), start);
        picker.level = Some(level);
        self.settings.colour = Some(picker);
        self.settings.dropdown = None;
    }

    pub(super) fn colour_motion(&mut self, point: Point) {
        let frame = self.settings_frame();
        let Some(layout) = self.colour_layout(&frame) else { return };
        let Some(picker) = &mut self.settings.colour else { return };
        let fraction = |value: f64, start: f64, length: f64| ((value - start) / length).clamp(0., 1.);
        match picker.drag {
            Some(Part::Square) => {
                picker.saturation = fraction(point.x, layout.square.x, layout.square.w);
                picker.value = 1. - fraction(point.y, layout.square.y, layout.square.h);
            }
            Some(Part::Hue) => picker.hue = fraction(point.x, layout.hue.x, layout.hue.w).min(0.999),
            None => return,
        }
        self.colour_commit();
        self.schedule_render();
    }

    fn colour_commit(&mut self) {
        let Some(picker) = &self.settings.colour else { return };
        let (index, level, hex) = (picker.index, picker.level, picker.hex());
        if let Some(level) = level {
            let mut levels = self.settings.levels();
            if let Some(entry) = levels.get_mut(level) {
                entry.1 = hex;
                self.settings.set_levels(&levels);
            }
        } else {
            self.settings.draft[index] = Some(toml::Value::String(hex));
        }
        self.settings.error = false;
    }

    pub(super) fn draw_colour_picker(&self, painter: &mut Painter<'_>, frame: &Frame) {
        let (Some(picker), Some(layout)) = (&self.settings.colour, self.colour_layout(frame)) else { return };
        let theme = &self.theme;
        painter.shape(
            layout.panel,
            Shape {
                border_width: 1.,
                border: theme.border,
                glow_radius: 18.,
                glow: Colour([0., 0., 0., 0.4]),
                ..Shape::filled(theme.surface, 10.)
            },
        );
        // For a fixed brightness, colour is linear in saturation, so each
        // strip is an exact left to right gradient.
        let square = layout.square;
        let strips = (square.h / 2.).ceil() as usize;
        for strip in 0..strips {
            let value = 1. - strip as f64 / (strips - 1) as f64;
            painter.shape(
                Rect::new(square.x, square.y + strip as f64 * 2., square.w, 2.),
                Shape {
                    fill: (colour(hsv(picker.hue, 0., value)), colour(hsv(picker.hue, 1., value))),
                    ..Default::default()
                },
            );
        }
        painter.shape(square, Shape { border_width: 1., border: theme.border, radius: 4., ..Default::default() });
        let knob = Point::new(square.x + picker.saturation * square.w, square.y + (1. - picker.value) * square.h);
        painter.shape(
            Rect::new(knob.x - 7., knob.y - 7., 14., 14.),
            Shape {
                radius: 7.,
                border_width: 2.,
                border: Colour([1., 1., 1., 1.]),
                ..Shape::filled(colour(hsv(picker.hue, picker.saturation, picker.value)), 7.)
            },
        );
        let hue = layout.hue;
        for segment in 0..6 {
            let width = hue.w / 6.;
            painter.shape(
                Rect::new(hue.x + segment as f64 * width, hue.y, width + 0.5, hue.h),
                Shape {
                    fill: (
                        colour(hsv(segment as f64 / 6., 1., 1.)),
                        colour(hsv((segment + 1) as f64 / 6. - 1e-6, 1., 1.)),
                    ),
                    ..Default::default()
                },
            );
        }
        let marker = hue.x + picker.hue * hue.w;
        painter.shape(
            Rect::new(marker - 4., hue.y - 3., 8., hue.h + 6.),
            Shape {
                radius: 3.,
                border_width: 2.,
                border: Colour([1., 1., 1., 1.]),
                ..Shape::filled(colour(hsv(picker.hue, 1., 1.)), 3.)
            },
        );
        let current = picker.hex();
        for (rect, preset) in layout.presets.iter().zip(PRESETS) {
            let chosen = current.starts_with(preset);
            painter.shape(
                *rect,
                Shape {
                    border_width: if chosen { 2. } else { 1. },
                    border: if chosen { theme.text } else { theme.border },
                    ..Shape::filled(Colour::parse(preset).unwrap_or(theme.accent), 11.)
                },
            );
        }
    }
}
