//! Shared text and icon drawing for the compositor's interface.

use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    path::{Path, PathBuf},
};

use smithay::input::keyboard::xkb;
use smithay::{
    backend::{
        allocator::Fourcc,
        renderer::{element::memory::MemoryRenderBuffer, gles::GlesRenderer},
    },
    utils::Transform,
};
use tileception_core::Rect;
use zeroize::Zeroizing;

use crate::{
    Tileception,
    render::{OutputElement, push_image},
    shapes::{Shape, Shapes},
    theme::Colour,
};

/// Stroked line icons on a 24 by 24 grid (the Tide set from `docs/design.html`).
pub mod glyph {
    pub const LOCK: &str = r#"<rect x="5" y="11" width="14" height="10" rx="2"/><path d="M8 11V7a4 4 0 0 1 8 0v4"/>"#;
    pub const LOG_OUT: &str = r#"<path d="M16 17l5-5-5-5M21 12H9M12 21H5V3h7"/>"#;
    pub const KEYBOARD: &str = r#"<rect x="2" y="6" width="20" height="12" rx="2"/><path d="M6 10h.01M10 10h.01M14 10h.01M18 10h.01M7 14h10"/>"#;
    pub const MOON: &str = r#"<path d="M21 12.8A9 9 0 1 1 11.2 3a7 7 0 0 0 9.8 9.8z"/>"#;
    pub const RESTART: &str = r#"<path d="M3 12a9 9 0 1 0 3-6.7L3 8M3 3v5h5"/>"#;
    pub const POWER: &str = r#"<path d="M12 2v10M18.4 6.6a9 9 0 1 1-12.8 0"/>"#;
    pub const SEARCH: &str = r#"<circle cx="11" cy="11" r="7"/><path d="M20 20l-4-4"/>"#;
    pub const BELL: &str = r#"<path d="M6 8a6 6 0 0 1 12 0c0 7 3 9 3 9H3s3-2 3-9M10.3 21a1.9 1.9 0 0 0 3.4 0"/>"#;
    pub const CLOSE: &str = r#"<path d="M18 6L6 18M6 6l12 12"/>"#;
    pub const COPY: &str = r#"<rect x="9" y="9" width="12" height="12" rx="2"/><path d="M5 15H4.5A1.5 1.5 0 0 1 3 13.5v-9A1.5 1.5 0 0 1 4.5 3h9A1.5 1.5 0 0 1 15 4.5V5"/>"#;
    pub const CHECK: &str = r#"<path d="M5 12.5l4.5 4.5L19 7.5"/>"#;
    pub const ARROW_RIGHT: &str = r#"<path d="M5 12h14M13 6l6 6-6 6"/>"#;
    pub const CAPS: &str = r#"<path d="M12 4l8 8h-4v5H8v-5H4z"/><path d="M8 21h8"/>"#;
    pub const CLEAR: &str = r#"<path d="M4 7h16M10 11v6M14 11v6M6 7l1 13h10l1-13M9 7V4h6v3"/>"#;
    pub const VOLUME_OFF: &str = r#"<path d="M11 5L6 9H2v6h4l5 4z"/><path d="M22 9l-6 6M16 9l6 6"/>"#;
    pub const VOLUME_LOW: &str = r#"<path d="M11 5L6 9H2v6h4l5 4z"/><path d="M15.5 8.5a5 5 0 0 1 0 7"/>"#;
    pub const VOLUME_HIGH: &str =
        r#"<path d="M11 5L6 9H2v6h4l5 4z"/><path d="M15.5 8.5a5 5 0 0 1 0 7M18.5 5.5a9.5 9.5 0 0 1 0 13"/>"#;
    pub const VOLUME_NONE: &str = r#"<path d="M11 5L6 9H2v6h4l5 4z"/>"#;
    pub const MIC: &str =
        r#"<rect x="9" y="2" width="6" height="12" rx="3"/><path d="M5 10v1a7 7 0 0 0 14 0v-1M12 18v4M8 22h8"/>"#;
    pub const MIC_OFF: &str = r#"<rect x="9" y="2" width="6" height="12" rx="3"/><path d="M5 10v1a7 7 0 0 0 14 0v-1M12 18v4M8 22h8M3 3l18 18"/>"#;
    pub const SUN: &str = r#"<circle cx="12" cy="12" r="4"/><path d="M12 2v2M12 20v2M4.9 4.9l1.4 1.4M17.7 17.7l1.4 1.4M2 12h2M20 12h2M4.9 19.1l1.4-1.4M17.7 6.3l1.4-1.4"/>"#;
    pub const PLAY: &str = r#"<path d="M7 4.5l12 7.5-12 7.5z"/>"#;
    pub const PAUSE: &str = r#"<path d="M8 5v14M16 5v14"/>"#;
    pub const NEXT: &str = r#"<path d="M5 5l10 7-10 7zM19 5v14"/>"#;
    pub const PREVIOUS: &str = r#"<path d="M19 19L9 12l10-7zM5 19V5"/>"#;
    pub const STOP: &str = r#"<rect x="6" y="6" width="12" height="12" rx="1.5"/>"#;
    pub const MUSIC: &str =
        r#"<path d="M9 18V5l12-2v13"/><circle cx="6" cy="18" r="3"/><circle cx="18" cy="16" r="3"/>"#;
    pub const CHEVRON_DOWN: &str = r#"<path d="M6 9l6 6 6-6"/>"#;
    pub const CHEVRON_UP: &str = r#"<path d="M6 15l6-6 6 6"/>"#;
    pub const CHEVRON_RIGHT: &str = r#"<path d="M9 6l6 6-6 6"/>"#;
    pub const CHAT: &str = r#"<path d="M21 12a8 8 0 0 1-11.6 7.1L4 20l1.1-4.4A8 8 0 1 1 21 12z"/><path d="M8.5 12h.01M12 12h.01M15.5 12h.01"/>"#;
    pub const ALERT: &str = r#"<path d="M12 3.5l9.5 16.5h-19z"/><path d="M12 10v4.5M12 17.5v.01"/>"#;
    pub const CALCULATOR: &str = r#"<rect x="5" y="3" width="14" height="18" rx="2"/><path d="M8 7h8M8 12h.01M12 12h.01M16 12h.01M8 16h.01M12 16h.01M16 16h.01"/>"#;
    pub const CLOCK: &str = r#"<circle cx="12" cy="12" r="9"/><path d="M12 7v5l3 2"/>"#;
    pub const CALENDAR: &str = r#"<rect x="3" y="5" width="18" height="16" rx="2"/><path d="M3 10h18M8 3v4M16 3v4"/>"#;
    pub const TERMINAL: &str = r#"<path d="M5 8l4 4-4 4M12 17h7"/><rect x="2" y="3" width="20" height="18" rx="2"/>"#;
    pub const FOLDER: &str = r#"<path d="M3 6.5A1.5 1.5 0 0 1 4.5 5H9l2 2.5h8.5A1.5 1.5 0 0 1 21 9v9.5a1.5 1.5 0 0 1-1.5 1.5h-15A1.5 1.5 0 0 1 3 18.5z"/>"#;
    pub const FILE: &str = r#"<path d="M14 3H6.5A1.5 1.5 0 0 0 5 4.5v15A1.5 1.5 0 0 0 6.5 21h11a1.5 1.5 0 0 0 1.5-1.5V8z"/><path d="M14 3v5h5"/>"#;
    pub const GLOBE: &str =
        r#"<circle cx="12" cy="12" r="9"/><path d="M3 12h18M12 3a14 14 0 0 1 0 18M12 3a14 14 0 0 0 0 18"/>"#;
    pub const OVERVIEW: &str = r#"<rect x="3" y="3" width="7" height="7" rx="1"/><rect x="14" y="3" width="7" height="7" rx="1"/><rect x="3" y="14" width="7" height="7" rx="1"/><rect x="14" y="14" width="7" height="7" rx="1"/>"#;
    pub const CAMERA: &str = r#"<path d="M4 8h3l2-3h6l2 3h3a1 1 0 0 1 1 1v10a1 1 0 0 1-1 1H4a1 1 0 0 1-1-1V9a1 1 0 0 1 1-1z"/><circle cx="12" cy="13.5" r="3.5"/>"#;
    pub const CONTRAST: &str = r#"<circle cx="12" cy="12" r="9"/><path d="M12 3v18M12 7.5h5M12 12h6M12 16.5h5"/>"#;
    pub const WINDOW: &str = r#"<rect x="3" y="4" width="18" height="16" rx="2"/><path d="M3 9h18"/>"#;
    pub const ZOOM_IN: &str = r#"<circle cx="11" cy="11" r="7"/><path d="M20 20l-4-4M8 11h6M11 8v6"/>"#;
    pub const ZOOM_OUT: &str = r#"<circle cx="11" cy="11" r="7"/><path d="M20 20l-4-4M8 11h6"/>"#;
    pub const ARROW_LEFT: &str = r#"<path d="M19 12H5M11 6l-6 6 6 6"/>"#;
    pub const DUCK: &str = r#"<circle cx="10" cy="8" r="3.5"/><path d="M13.5 8.5l3 .5-3 1.2M7 11.5C4.5 11.5 3 13 3 14.8 3 18 6.5 20 11 20s8.5-2.5 9-7c-2 1-4.5 1-6.5-.6"/>"#;
}

/// App and file icons, rasterised at the output's scale and cached by path
/// and size in physical pixels.
pub struct Icons {
    cache: RefCell<HashMap<(PathBuf, u32), Option<MemoryRenderBuffer>>>,
    scale: Cell<f64>,
}

impl Default for Icons {
    fn default() -> Self {
        Icons { cache: Default::default(), scale: Cell::new(1.0) }
    }
}

pub fn compose_state() -> Option<xkb::compose::State> {
    let locale = ["LC_ALL", "LC_CTYPE", "LANG"]
        .iter()
        .find_map(|key| std::env::var_os(key).filter(|value| !value.is_empty()))
        .unwrap_or_else(|| "C.UTF-8".into());
    let context = xkb::Context::new(xkb::CONTEXT_NO_FLAGS);
    let table = xkb::compose::Table::new_from_locale(&context, &locale, xkb::compose::COMPILE_NO_FLAGS).ok()?;
    Some(xkb::compose::State::new(&table, xkb::compose::STATE_NO_FLAGS))
}

pub fn compose_text(compose: &mut Option<xkb::compose::State>, sym: u32) -> Option<Zeroizing<String>> {
    if let Some(state) = compose {
        state.feed(sym.into());
        match state.status() {
            xkb::compose::Status::Composing => return None,
            xkb::compose::Status::Cancelled => {
                state.reset();
                return None;
            }
            xkb::compose::Status::Composed => {
                let text = state.utf8().map(Zeroizing::new);
                state.reset();
                return text;
            }
            xkb::compose::Status::Nothing => {}
        }
    }
    char::from_u32(xkb::keysym_to_utf32(sym.into()))
        .filter(|character| !character.is_control())
        .map(|character| Zeroizing::new(character.to_string()))
}

impl Icons {
    pub fn set_scale(&self, scale: f64) {
        self.scale.set(scale);
    }

    /// Physical pixels for `size` logical ones.
    pub fn pixels(&self, size: u32) -> u32 {
        (f64::from(size) * self.scale.get()).round().max(1.0) as u32
    }

    /// The icon at `path`, `size` logical pixels square.
    pub fn image(&self, path: &Path, size: u32) -> Option<MemoryRenderBuffer> {
        let size = self.pixels(size);
        let key = (path.to_path_buf(), size);
        if let Some(image) = self.cache.borrow().get(&key) {
            return image.clone();
        }
        let image = load_icon(path, size);
        let mut cache = self.cache.borrow_mut();
        if cache.len() >= 512 {
            cache.clear();
        }
        cache.insert(key, image.clone());
        image
    }
}

/// Line icons rasterised once per size and colour, at the output's scale.
pub struct Glyphs {
    cache: RefCell<HashMap<GlyphKey, Option<MemoryRenderBuffer>>>,
    scale: Cell<f64>,
}

impl Default for Glyphs {
    fn default() -> Self {
        Glyphs { cache: Default::default(), scale: Cell::new(1.0) }
    }
}

/// A glyph's path data, size in physical pixels and straight RGBA colour.
type GlyphKey = (&'static str, u32, [u8; 4]);

impl Glyphs {
    pub fn set_scale(&self, scale: f64) {
        self.scale.set(scale);
    }

    /// The glyph `size` logical pixels square.
    pub fn image(&self, glyph: &'static str, size: u32, colour: Colour) -> Option<MemoryRenderBuffer> {
        let size = (f64::from(size) * self.scale.get()).round().max(1.0) as u32;
        let key = (glyph, size, colour.straight_rgba8());
        if let Some(image) = self.cache.borrow().get(&key) {
            return image.clone();
        }
        let image = render_glyph(glyph, size, colour);
        let mut cache = self.cache.borrow_mut();
        if cache.len() >= 256 {
            cache.clear();
        }
        cache.insert(key, image.clone());
        image
    }
}

fn render_glyph(glyph: &str, size: u32, colour: Colour) -> Option<MemoryRenderBuffer> {
    let [red, green, blue, alpha] = colour.straight_rgba8();
    let svg = format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="rgb({red},{green},{blue})" stroke-opacity="{}" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">{glyph}</svg>"#,
        alpha as f32 / 255.0,
    );
    let tree = resvg::usvg::Tree::from_str(&svg, &resvg::usvg::Options::default()).ok()?;
    let mut pixmap = resvg::tiny_skia::Pixmap::new(size, size)?;
    let scale = size as f32 / 24.0;
    resvg::render(&tree, resvg::tiny_skia::Transform::from_scale(scale, scale), &mut pixmap.as_mut());
    // tiny-skia pixels are premultiplied RGBA, which is what the renderer wants.
    Some(MemoryRenderBuffer::from_slice(
        pixmap.data(),
        Fourcc::Abgr8888,
        (size as i32, size as i32),
        1,
        Transform::Normal,
        None,
    ))
}

/// Eases a 0 to 1 progress so movement settles softly.
pub fn ease_out(progress: f64) -> f64 {
    let progress = progress.clamp(0.0, 1.0);
    1.0 - (1.0 - progress).powi(3)
}

/// The wall clock in the local time zone.
pub struct LocalTime {
    pub year: i32,
    pub hour: i32,
    pub minute: i32,
    pub weekday: usize,
    pub day: i32,
    pub month: usize,
}

impl LocalTime {
    #[allow(unsafe_code, reason = "the local time zone is applied through libc's reentrant localtime_r")]
    pub fn now() -> Option<LocalTime> {
        // SAFETY: time accepts a null pointer and only returns the current time.
        let now = unsafe { libc::time(std::ptr::null_mut()) };
        let mut calendar = std::mem::MaybeUninit::<libc::tm>::uninit();
        // SAFETY: localtime_r reads `now` and fills the caller-owned `calendar`, which lives past the call.
        let filled = unsafe { libc::localtime_r(&now, calendar.as_mut_ptr()) };
        if filled.is_null() {
            return None;
        }
        // SAFETY: a non-null result means localtime_r initialised `calendar`.
        let calendar = unsafe { calendar.assume_init() };
        Some(LocalTime {
            year: calendar.tm_year + 1900,
            hour: calendar.tm_hour,
            minute: calendar.tm_min,
            weekday: calendar.tm_wday.clamp(0, 6) as usize,
            day: calendar.tm_mday,
            month: calendar.tm_mon.clamp(0, 11) as usize,
        })
    }

    pub fn clock(&self) -> String {
        format!("{:02}:{:02}", self.hour, self.minute)
    }

    /// "Thursday 15 October"
    pub fn date(&self) -> String {
        const DAYS: [&str; 7] = ["Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday"];
        const MONTHS: [&str; 12] = [
            "January",
            "February",
            "March",
            "April",
            "May",
            "June",
            "July",
            "August",
            "September",
            "October",
            "November",
            "December",
        ];
        format!("{} {} {}", DAYS[self.weekday], self.day, MONTHS[self.month])
    }
}

/// "now", "4 min", "2 h", "3 d": how long ago something happened.
pub fn ago(when: std::time::SystemTime) -> String {
    let secs = when.elapsed().map(|duration| duration.as_secs()).unwrap_or(0);
    match secs {
        0..=59 => "now".into(),
        60..=3599 => format!("{} min", secs / 60),
        3600..=86_399 => format!("{} h", secs / 3600),
        _ => format!("{} d", secs / 86_400),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Align {
    Left,
    Centre,
}

/// How a piece of interface text looks.
#[derive(Debug, Clone, Copy)]
pub struct Style {
    pub size: f32,
    pub colour: Colour,
    pub weight: u16,
    /// 0 for one line, otherwise wrapped to at most this many lines.
    pub lines: u8,
    pub align: Align,
}

impl Style {
    pub fn new(size: f32, colour: Colour) -> Style {
        Style { size, colour, weight: 400, lines: 0, align: Align::Left }
    }
    pub fn weight(self, weight: u16) -> Style {
        Style { weight, ..self }
    }
    pub fn colour(self, colour: Colour) -> Style {
        Style { colour, ..self }
    }
    pub fn bold(self) -> Style {
        self.weight(600)
    }
    pub fn lines(self, lines: u8) -> Style {
        Style { lines, ..self }
    }
    pub fn align(self, align: Align) -> Style {
        Style { align, ..self }
    }
}

/// Draws the compositor's own interface back to front (in painting order),
/// optionally faded and shifted as a whole for animations.
pub struct Painter<'a> {
    state: &'a Tileception,
    renderer: &'a mut GlesRenderer,
    shapes: Option<Shapes>,
    layers: Vec<OutputElement>,
    pub alpha: f32,
    pub offset: (f64, f64),
}

impl<'a> Painter<'a> {
    pub fn new(state: &'a Tileception, renderer: &'a mut GlesRenderer) -> Painter<'a> {
        let shapes = state.shapes(renderer);
        Painter { state, renderer, shapes, layers: Vec::new(), alpha: 1.0, offset: (0.0, 0.0) }
    }

    pub fn theme(&self) -> &'a crate::theme::Theme {
        &self.state.theme
    }

    /// The elements, topmost first as the renderer wants them.
    pub fn finish(mut self) -> Vec<OutputElement> {
        self.layers.reverse();
        self.layers
    }

    fn shifted(&self, rect: Rect) -> Rect {
        Rect::new(rect.x + self.offset.0, rect.y + self.offset.1, rect.w, rect.h)
    }

    pub fn shape(&mut self, rect: Rect, shape: Shape) {
        let rect = self.shifted(rect);
        if self.alpha <= 0.0 {
            return;
        }
        if let Some(element) = self.shapes.as_ref().and_then(|shapes| shapes.element_faded(&rect, shape, self.alpha)) {
            self.layers.push(OutputElement::Shape(element));
        }
    }

    pub fn fill(&mut self, rect: Rect, colour: Colour, radius: f32) {
        self.shape(rect, Shape::filled(colour, radius));
    }

    /// An image rasterised at the output's scale, at its own size; or a
    /// picture of `size` pixels drawn `size` logical pixels large, so it is
    /// enlarged with the scale.
    pub fn image_sized(&mut self, buffer: &MemoryRenderBuffer, position: (f64, f64), size: Option<(i32, i32)>) {
        if self.alpha <= 0.0 {
            return;
        }
        let scale = self.state.scale;
        let position = (((position.0 + self.offset.0) * scale).round(), ((position.1 + self.offset.1) * scale).round());
        let alpha = (self.alpha < 1.0).then_some(self.alpha);
        let (src, size) = match size {
            Some((width, height)) => {
                let (width, height) = (width.max(1), height.max(1));
                let shown = ((f64::from(width) * scale).round() as i32, (f64::from(height) * scale).round() as i32);
                (
                    Some(smithay::utils::Rectangle::from_size((f64::from(width), f64::from(height)).into())),
                    Some(shown.into()),
                )
            }
            None => (None, None),
        };
        if let Ok(element) = smithay::backend::renderer::element::memory::MemoryRenderBufferRenderElement::from_buffer(
            self.renderer,
            position,
            buffer,
            alpha,
            src,
            size,
            smithay::backend::renderer::element::Kind::Unspecified,
        ) {
            self.layers.push(OutputElement::Memory(element));
        }
    }

    /// A window's picture fitted into `rect`.
    pub fn window(&mut self, window_id: tileception_core::WindowId, rect: Rect) {
        if self.alpha <= 0.0 {
            return;
        }
        let rect = self.shifted(rect);
        let elements = self.state.picture_faded(self.renderer, window_id, &rect, self.alpha);
        // Pictures come topmost first; layers are kept in painting order.
        self.layers.extend(elements.into_iter().rev());
    }

    pub fn image(&mut self, buffer: &MemoryRenderBuffer, position: (f64, f64)) {
        self.image_sized(buffer, position, None);
    }

    /// Text inside `rect`, centred vertically and aligned horizontally.
    /// Returns its size, also when nothing could be drawn.
    pub fn text(&mut self, label: &str, rect: Rect, style: Style) -> (f64, f64) {
        let theme = self.theme();
        let image = self.state.text.borrow_mut().styled(
            label,
            &theme.font,
            style.size,
            style.colour,
            rect.w.max(1.0) as f32,
            style.weight,
            style.lines,
        );
        let Some(image) = image else { return (0.0, 0.0) };
        let (width, height) = (image.width, image.height);
        let left = match style.align {
            Align::Left => rect.x,
            Align::Centre => rect.x + (rect.w - width) / 2.0,
        };
        self.image(&image.buffer, (left, rect.y + (rect.h - height) / 2.0));
        (width, height)
    }

    /// The size text would take, without drawing it.
    pub fn measure(&self, label: &str, max_width: f64, style: Style) -> (f64, f64) {
        let theme = self.theme();
        self.state
            .text
            .borrow_mut()
            .styled(label, &theme.font, style.size, style.colour, max_width.max(1.0) as f32, style.weight, style.lines)
            .map(|rendered| (rendered.width, rendered.height))
            .unwrap_or((0.0, 0.0))
    }

    /// A line icon centred in `rect`.
    pub fn glyph(&mut self, glyph: &'static str, rect: Rect, size: u32, colour: Colour) {
        if let Some(image) = self.state.glyphs.image(glyph, size, colour) {
            let position = (rect.x + (rect.w - size as f64) / 2.0, rect.y + (rect.h - size as f64) / 2.0);
            self.image(&image, position);
        }
    }

    fn keycap_style(&self) -> Style {
        let theme = self.theme();
        Style::new(theme.font_size * 0.8, theme.text).bold()
    }

    pub fn keycap_width(&self, label: &str) -> f64 {
        (self.measure(label, 200.0, self.keycap_style()).0 + 12.0).max(20.0)
    }

    /// A key on the keyboard, drawn as a small cap with its label. Returns its width.
    pub fn keycap(&mut self, label: &str, left: f64, centre_y: f64) -> f64 {
        let theme = self.theme();
        let style = self.keycap_style();
        let cap = Rect::new(left, centre_y - 10.0, self.keycap_width(label), 20.0);
        self.shape(Rect::new(cap.x, cap.y + 1.0, cap.w, cap.h), Shape::filled(Colour([0.0, 0.0, 0.0, 0.35]), 4.0));
        self.shape(cap, Shape { border_width: 1.0, border: theme.border, ..Shape::filled(theme.surface_raised, 4.0) });
        self.text(label, cap, style.align(Align::Centre));
        cap.w
    }

    /// Keycaps followed by what they do, in a row; returns the row's width.
    pub fn key_hints(&mut self, hints: &[(&str, &str)], left: f64, centre_y: f64, measure_only: bool) -> f64 {
        let theme = self.theme();
        let style = Style::new(theme.font_size * 0.85, theme.muted);
        let mut cursor = left;
        for (index, (key, what)) in hints.iter().enumerate() {
            if index > 0 {
                cursor += 16.0;
            }
            let cap_w = if measure_only { self.keycap_width(key) } else { self.keycap(key, cursor, centre_y) };
            cursor += cap_w + 6.0;
            let (text_width, _) = if measure_only {
                self.measure(what, 300.0, style)
            } else {
                self.text(what, Rect::new(cursor, centre_y - 12.0, 300.0, 24.0), style)
            };
            cursor += text_width;
        }
        cursor - left
    }

    /// A popup surface: rounded, with a hairline border and a soft shadow.
    pub fn popup(&mut self, rect: Rect) {
        let theme = self.theme();
        self.shape(
            Rect::new(rect.x, rect.y + 6.0, rect.w, rect.h),
            Shape {
                radius: theme.popup_radius,
                glow_radius: 28.0,
                glow: Colour([0.0, 0.0, 0.0, 0.42]),
                ..Default::default()
            },
        );
        self.shape(
            rect,
            Shape { border_width: 1.0, border: theme.border, ..Shape::filled(theme.popup(), theme.popup_radius) },
        );
    }

    /// A panel against a screen edge, full height: square, with a hairline
    /// and a soft shadow only on the side that faces the desktop.
    pub fn edge_panel(&mut self, rect: Rect, left: bool) {
        let theme = self.theme();
        self.shape(rect, Shape { glow_radius: 28.0, glow: Colour([0.0, 0.0, 0.0, 0.42]), ..Default::default() });
        self.fill(rect, theme.popup(), 0.0);
        let edge = if left { rect.right() - 1.0 } else { rect.x };
        self.fill(Rect::new(edge, rect.y, 1.0, rect.h), theme.border, 0.0);
    }
}

fn load_icon(path: &Path, size: u32) -> Option<MemoryRenderBuffer> {
    let pixels = if path.extension().is_some_and(|extension| extension == "svg") {
        let data = std::fs::read(path).ok()?;
        let options =
            resvg::usvg::Options { resources_dir: path.parent().map(Path::to_path_buf), ..Default::default() };
        let tree = resvg::usvg::Tree::from_data(&data, &options).ok()?;
        let mut pixels = resvg::tiny_skia::Pixmap::new(size, size)?;
        let scale = (size as f32 / tree.size().width()).min(size as f32 / tree.size().height());
        let transform = resvg::tiny_skia::Transform::from_translate(
            (size as f32 - tree.size().width() * scale) / 2.0,
            (size as f32 - tree.size().height() * scale) / 2.0,
        )
        .pre_scale(scale, scale);
        resvg::render(&tree, transform, &mut pixels.as_mut());
        pixels.take()
    } else {
        let reader = image::ImageReader::open(path).ok()?.with_guessed_format().ok()?;
        let image = reader.decode().ok()?.resize(size, size, image::imageops::FilterType::Triangle).to_rgba8();
        let mut canvas = image::RgbaImage::new(size, size);
        image::imageops::overlay(
            &mut canvas,
            &image,
            ((size - image.width()) / 2) as i64,
            ((size - image.height()) / 2) as i64,
        );
        for pixel in canvas.pixels_mut() {
            let alpha = pixel.0[3] as u32;
            for channel in &mut pixel.0[..3] {
                *channel = (*channel as u32 * alpha / 255) as u8;
            }
        }
        canvas.into_raw()
    };
    Some(MemoryRenderBuffer::from_slice(
        &pixels,
        Fourcc::Abgr8888,
        (size as i32, size as i32),
        1,
        Transform::Normal,
        None,
    ))
}

impl Tileception {
    pub fn ui_label(
        &self,
        out: &mut Vec<OutputElement>,
        renderer: &mut GlesRenderer,
        label: &str,
        rect: Rect,
        colour: Colour,
        bold: bool,
    ) {
        if let Some(image) = self.text.borrow_mut().line(
            label,
            &self.theme.font,
            self.theme.font_size,
            colour,
            rect.w.max(1.0) as f32,
            bold,
        ) {
            push_image(out, renderer, &image.buffer, (rect.x, rect.y + (rect.h - image.height) / 2.0), self.scale);
        }
    }
}
