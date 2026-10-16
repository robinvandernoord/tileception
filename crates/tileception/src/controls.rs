//! Sound, brightness and playback controls at the top of the notification
//! panel, and the level bar they share with the on-screen display.

use std::time::Instant;

use smithay::backend::renderer::element::memory::MemoryRenderBuffer;
use tileception_core::{Point, Rect};
use tileception_media::{Change, Player, PlayerCommand, Request, Status, Target, Toggle};

use crate::{
    Tileception,
    media::{Adjust, Level, SliderId},
    shapes::Shape,
    ui::{Align, Painter, Style, glyph},
};

/// Height of one slider row.
const ROW: f64 = 32.0;
/// Within this many percent of 100, a dragged volume lands on exactly 100.
const SNAP: f64 = 3.0;
/// Sliders send at most this often while dragged.
const DRAG_INTERVAL: f64 = 1.0 / 30.0;

/// A slider row: a button (mute) on the left, the track, the value.
#[derive(Debug, Clone)]
pub struct Slider {
    pub id: SliderId,
    pub button: Rect,
    pub track: Rect,
    pub label: Rect,
    pub value: u32,
    pub max: u32,
    pub muted: bool,
    /// For a program's row: its name, drawn between the button and the track.
    pub name: Option<(Rect, String)>,
}

impl Slider {
    /// Where the pointer counts as on the track.
    fn grab_area(&self) -> Rect {
        Rect::new(self.track.x - 8.0, self.button.y, self.track.w + 16.0, self.button.h)
    }

    /// The value under the pointer, snapping to 100 when boosting is possible.
    fn value_at(&self, x: f64) -> u32 {
        let v = ((x - self.track.x) / self.track.w).clamp(0.0, 1.0) * self.max as f64;
        if self.max > 100 && (v - 100.0).abs() < SNAP { 100 } else { v.round() as u32 }
    }
}

pub struct MediaCard {
    pub rect: Rect,
    pub art: Rect,
    pub title: Rect,
    pub artist: Rect,
    pub player: Rect,
    pub previous: Rect,
    pub play: Rect,
    pub next: Rect,
    pub progress: Rect,
}

/// A device that can be made the default.
pub struct DeviceRow {
    pub rect: Rect,
    pub output: bool,
    pub name: String,
    pub description: String,
    pub current: bool,
}

/// The block's layout, worked out afresh each frame from the media snapshot.
pub struct Controls {
    pub rect: Rect,
    pub sound: Rect,
    pub device: Option<Rect>,
    /// Whether the device line opens a list to choose from.
    pub choosable: bool,
    /// The opened list: section titles, and a row per device.
    pub headings: Vec<(Rect, &'static str)>,
    pub choices: Vec<DeviceRow>,
    pub sliders: Vec<Slider>,
    pub apps: Option<Rect>,
    pub media: Option<MediaCard>,
}

impl Tileception {
    /// The controls block with its top left at (x, y).
    pub fn controls_layout(&self, x: f64, y: f64, w: f64) -> Controls {
        if !self.shell().media() {
            let none = Rect::new(x, y, w, 0.0);
            return Controls {
                rect: none,
                sound: none,
                device: None,
                choosable: false,
                headings: Vec::new(),
                choices: Vec::new(),
                sliders: Vec::new(),
                apps: None,
                media: None,
            };
        }
        let s = &self.media.snapshot;
        let max = self.config.file.media.max_volume();
        let inner = Rect::new(x + 12.0, y, w - 24.0, 0.0);
        let mut cursor = y + 10.0;
        let mut sliders = Vec::new();
        let row = |id, top: f64, value, max, muted, name: Option<String>| {
            let button = Rect::new(inner.x, top, ROW, ROW);
            let label = Rect::new(inner.right() - 46.0, top, 46.0, ROW);
            let (start, name) = match name {
                Some(name) => (inner.x + 40.0 + 118.0, Some((Rect::new(inner.x + 40.0, top, 110.0, ROW), name))),
                None => (inner.x + 44.0, None),
            };
            let track = Rect::new(start, top, label.x - 12.0 - start, ROW);
            Slider { id, button, track, label, value, max, muted, name }
        };
        // The output's name, or why there is no sound, heads the block.
        let device = Some(Rect::new(inner.x - 4.0, cursor - 3.0, inner.w + 8.0, 24.0));
        cursor += 24.0;
        let mut headings = Vec::new();
        let mut choices = Vec::new();
        let choosable = s.audio.connected && (s.audio.outputs.len() > 1 || s.audio.inputs.len() > 1);
        if s.audio.connected {
            if choosable && self.media.devices_open {
                let lists = [
                    ("OUTPUT", true, &s.audio.outputs, s.audio.output.as_ref()),
                    ("MICROPHONE", false, &s.audio.inputs, s.audio.input.as_ref()),
                ];
                for (title, output, list, current) in lists {
                    if list.is_empty() {
                        continue;
                    }
                    headings.push((Rect::new(inner.x, cursor + 2.0, inner.w, 18.0), title));
                    cursor += 22.0;
                    for choice in list.iter() {
                        choices.push(DeviceRow {
                            rect: Rect::new(inner.x - 4.0, cursor, inner.w + 8.0, 28.0),
                            output,
                            name: choice.name.clone(),
                            description: choice.description.clone(),
                            current: current.is_some_and(|d| d.name == choice.name),
                        });
                        cursor += 30.0;
                    }
                }
                cursor += 6.0;
            }
            if let Some(output) = &s.audio.output {
                sliders.push(row(SliderId::Volume, cursor, output.percent, max, output.muted, None));
                cursor += ROW + 4.0;
            }
            if let Some(input) = &s.audio.input {
                sliders.push(row(SliderId::Mic, cursor, input.percent, 100, input.muted, None));
                cursor += ROW + 4.0;
            }
        }
        if let Some(backlight) = &s.backlight {
            sliders.push(row(SliderId::Brightness, cursor, backlight.percent, 100, false, None));
            cursor += ROW + 4.0;
        }
        let mut apps = None;
        if !s.audio.apps.is_empty() {
            apps = Some(Rect::new(inner.x, cursor, inner.w, 28.0));
            cursor += 30.0;
            if self.media.apps_open {
                for app in &s.audio.apps {
                    sliders.push(row(
                        SliderId::App(app.index),
                        cursor,
                        app.percent,
                        max,
                        app.muted,
                        Some(app.app.clone()),
                    ));
                    cursor += ROW + 2.0;
                }
            }
        }
        // Slider values follow the pointer while dragged, ahead of the sound server.
        if let Some((id, _, value)) = self.media.drag
            && let Some(slider) = sliders.iter_mut().find(|s| s.id == id)
        {
            slider.value = value;
        }
        let sound = Rect::new(x, y, w, cursor + 6.0 - y);
        cursor = sound.bottom();

        let media = s.media.active().map(|_| media_card_layout(x, cursor + 8.0, w));
        let bottom = media.as_ref().map_or(sound.bottom(), |m| m.rect.bottom());
        Controls {
            rect: Rect::new(x, y, w, bottom - y),
            sound,
            device,
            choosable,
            headings,
            choices,
            sliders,
            apps,
            media,
        }
    }

    /// A press in the panel; returns whether the controls took it.
    pub fn controls_press(&mut self, controls: &Controls, p: Point) -> bool {
        if !controls.rect.contains(p) {
            return false;
        }
        if controls.choosable && controls.device.is_some_and(|r| r.contains(p)) {
            self.media.devices_open = !self.media.devices_open;
            return true;
        }
        if let Some(choice) = controls.choices.iter().find(|c| c.rect.contains(p)) {
            if !choice.current {
                self.media_request(Request::Default { output: choice.output, name: choice.name.clone() });
            }
            return true;
        }
        for slider in &controls.sliders {
            if slider.button.contains(p) {
                self.slider_mute(slider.id);
                return true;
            }
            if slider.grab_area().contains(p) {
                let value = slider.value_at(p.x);
                self.media.drag = Some((slider.id, Instant::now(), value));
                self.slider_set(slider.id, value);
                return true;
            }
        }
        if controls.apps.is_some_and(|r| r.contains(p)) {
            self.media.apps_open = !self.media.apps_open;
            return true;
        }
        if let Some(card) = &controls.media {
            self.media_card_press(card, p);
        }
        true
    }

    /// A press on a playback card's buttons.
    pub fn media_card_press(&mut self, card: &MediaCard, p: Point) {
        let command = if card.previous.contains(p) {
            Some(PlayerCommand::Previous)
        } else if card.play.contains(p) {
            Some(PlayerCommand::PlayPause)
        } else if card.next.contains(p) {
            Some(PlayerCommand::Next)
        } else {
            None
        };
        if let Some(command) = command {
            self.media_request(Request::Player(command));
        }
    }

    /// The pointer moved while a slider is held.
    pub fn controls_drag(&mut self, x: f64) {
        let Some((id, sent, _)) = self.media.drag else { return };
        let Some(controls) = self.panel_media_controls() else { return };
        let Some(slider) = controls.sliders.iter().find(|s| s.id == id) else { return };
        let value = slider.value_at(x);
        let now = Instant::now();
        if now.duration_since(sent).as_secs_f64() >= DRAG_INTERVAL {
            self.slider_set(id, value);
            self.media.drag = Some((id, now, value));
        } else {
            self.media.drag = Some((id, sent, value));
        }
    }

    /// Lets go of a slider, sending where it ended.
    pub fn controls_release(&mut self) -> bool {
        let Some((id, _, value)) = self.media.drag.take() else { return false };
        self.slider_set(id, value);
        true
    }

    /// Scrolling over a slider steps it. Returns whether it was over one.
    pub fn controls_scroll(&mut self, p: Point, amount: f64, discrete: bool) -> bool {
        let Some(controls) = self.panel_media_controls() else { return false };
        let Some(slider) = controls
            .sliders
            .iter()
            .find(|s| Rect::new(s.button.x, s.button.y, s.label.right() - s.button.x, ROW).contains(p))
        else {
            return controls.rect.contains(p);
        };
        // A wheel notch is a step; on a touchpad, every 24 pixels are.
        self.media.scrolled += if discrete { amount.signum() * 24.0 } else { amount };
        let steps = (self.media.scrolled / 24.0).trunc();
        if steps == 0.0 {
            return true;
        }
        self.media.scrolled -= steps * 24.0;
        let n = steps.abs() as u32;
        let adjust = if steps < 0.0 { Adjust::Up(None) } else { Adjust::Down(None) };
        for _ in 0..n {
            match slider.id {
                SliderId::Volume => self.adjust(Level::Volume, adjust),
                SliderId::Mic => self.adjust(Level::Mic, adjust),
                SliderId::Brightness => self.adjust(Level::Brightness, adjust),
                SliderId::App(index) => {
                    let step = self.config.file.media.volume_step.max(1) as i32;
                    let change = Change::Step(if steps < 0.0 { step } else { -step });
                    let max = self.config.file.media.max_volume();
                    self.media_request(Request::Volume { target: Target::App(index), change, max });
                }
            }
        }
        true
    }

    fn slider_set(&mut self, id: SliderId, value: u32) {
        let max = self.config.file.media.max_volume();
        let request = match id {
            SliderId::Volume => Request::Volume { target: Target::Output, change: Change::Set(value), max },
            SliderId::Mic => Request::Volume { target: Target::Input, change: Change::Set(value), max: 100 },
            SliderId::Brightness => Request::Brightness(Change::Set(value)),
            SliderId::App(index) => Request::Volume { target: Target::App(index), change: Change::Set(value), max },
        };
        self.media_request(request);
    }

    fn slider_mute(&mut self, id: SliderId) {
        let target = match id {
            SliderId::Volume => Target::Output,
            SliderId::Mic => Target::Input,
            SliderId::Brightness => return,
            SliderId::App(index) => Target::App(index),
        };
        let max = self.config.file.media.max_volume();
        self.media_request(Request::Volume { target, change: Change::Mute(Toggle::Flip), max });
    }

    pub fn draw_controls(&self, p: &mut Painter, controls: &Controls, pointer: Point) {
        if controls.rect.h <= 0.0 {
            return;
        }
        let t = &self.theme;
        let s = &self.media.snapshot;
        p.shape(controls.sound, Shape { border_width: 1.0, border: t.border, ..Shape::filled(t.surface, t.radius) });
        if let Some(device) = controls.device {
            let label = match &s.audio.output {
                _ if !s.audio.connected => "Sound unavailable: no sound server".to_string(),
                Some(output) => output.description.clone(),
                None => "No audio output".to_string(),
            };
            let hover = controls.choosable && device.contains(pointer);
            if hover {
                p.fill(device, t.surface_raised, t.radius);
            }
            let text = Rect::new(device.x + 4.0, device.y, device.w - 30.0, device.h);
            p.text(&label, text, Style::new(t.font_size * 0.85, if hover { t.text } else { t.muted }));
            if controls.choosable {
                let chevron = if self.media.devices_open { glyph::CHEVRON_DOWN } else { glyph::CHEVRON_RIGHT };
                p.glyph(chevron, Rect::new(device.right() - 26.0, device.y, 24.0, device.h), 13, t.muted);
            }
        }
        for (rect, title) in &controls.headings {
            p.text(title, *rect, Style::new(t.font_size * 0.72, t.muted).bold());
        }
        for choice in &controls.choices {
            if choice.rect.contains(pointer) && !choice.current {
                p.fill(choice.rect, t.surface_raised, t.radius);
            }
            if choice.current {
                p.glyph(glyph::CHECK, Rect::new(choice.rect.x + 4.0, choice.rect.y, 24.0, choice.rect.h), 14, t.accent);
            }
            let colour = if choice.current { t.text } else { t.text.alpha(0.8) };
            let text = Rect::new(choice.rect.x + 32.0, choice.rect.y, choice.rect.w - 36.0, choice.rect.h);
            p.text(&choice.description, text, Style::new(t.font_size * 0.92, colour));
        }
        for slider in &controls.sliders {
            let hover = slider.button.contains(pointer);
            let (icon, label) = match slider.id {
                SliderId::Volume => (volume_glyph(slider.value, slider.muted), None),
                SliderId::Mic => (if slider.muted { glyph::MIC_OFF } else { glyph::MIC }, None),
                SliderId::Brightness => (glyph::SUN, None),
                SliderId::App(_) => {
                    (if slider.muted { glyph::VOLUME_OFF } else { glyph::VOLUME_LOW }, slider.name.as_ref())
                }
            };
            if hover && slider.id != SliderId::Brightness {
                p.fill(slider.button, t.surface_raised, t.radius);
            }
            let app_icon = match slider.id {
                SliderId::App(index) => {
                    s.audio.apps.iter().find(|a| a.index == index).and_then(|a| self.stream_icon(a, 20))
                }
                _ => None,
            };
            match app_icon {
                Some(image) if !hover => p.image(&image, (slider.button.x + 6.0, slider.button.y + 6.0)),
                _ => p.glyph(icon, slider.button, 16, if slider.muted { t.muted } else { t.text }),
            }
            if let Some((rect, name)) = label {
                p.text(name, *rect, Style::new(t.font_size * 0.92, if slider.muted { t.muted } else { t.text }));
            }
            let held = self.media.drag.is_some_and(|(id, ..)| id == slider.id);
            let active = held || slider.grab_area().contains(pointer);
            level_bar(p, slider.track, slider.value, slider.max, slider.muted, Some(active));
            let (text, colour) = value_label(slider.value, slider.muted, slider.max > 100, t);
            p.text(&text, slider.label, Style::new(t.font_size * 0.9, colour).bold().align(Align::Centre));
        }
        if let Some(row) = controls.apps {
            let hover = row.contains(pointer);
            if hover {
                p.fill(row, t.surface_raised, t.radius);
            }
            let chevron = if self.media.apps_open { glyph::CHEVRON_DOWN } else { glyph::CHEVRON_RIGHT };
            p.glyph(chevron, Rect::new(row.x, row.y, ROW, row.h), 14, t.muted);
            let count = s.audio.apps.len();
            let label = if count == 1 { "1 program playing".to_string() } else { format!("{count} programs playing") };
            p.text(&label, Rect::new(row.x + 36.0, row.y, row.w - 40.0, row.h), Style::new(t.font_size * 0.9, t.muted));
        }
        if let (Some(card), Some(player)) = (&controls.media, s.media.active()) {
            self.draw_media_card(p, card, player, pointer);
        }
    }

    pub fn draw_media_card(&self, p: &mut Painter, card: &MediaCard, player: &Player, pointer: Point) {
        let t = &self.theme;
        p.shape(card.rect, Shape { border_width: 1.0, border: t.border, ..Shape::filled(t.surface, t.radius) });
        self.draw_art(p, card.art, player);
        let title = if player.title.is_empty() { player.identity.as_str() } else { player.title.as_str() };
        p.text(title, card.title, Style::new(t.font_size, t.text).bold());
        if !player.artist.is_empty() {
            p.text(&player.artist, card.artist, Style::new(t.font_size * 0.9, t.muted));
        }
        let button = |p: &mut Painter, rect: Rect, g: &'static str, enabled: bool, primary: bool| {
            let hover = enabled && rect.contains(pointer);
            if primary {
                p.shape(
                    rect,
                    Shape { radius: rect.h as f32 / 2.0, fill: (t.accent, t.accent_end), ..Default::default() },
                );
                p.glyph(g, rect, 14, t.on_accent);
                return;
            }
            if hover {
                p.fill(rect, t.surface_raised, rect.h as f32 / 2.0);
            }
            p.glyph(g, rect, 15, if enabled { t.text } else { t.muted.alpha(0.5) });
        };
        button(p, card.previous, glyph::PREVIOUS, player.can_previous, false);
        let playing = player.status == Status::Playing;
        let play_glyph = if playing { glyph::PAUSE } else { glyph::PLAY };
        button(p, card.play, play_glyph, player.can_pause || player.can_play, true);
        button(p, card.next, glyph::NEXT, player.can_next, false);

        let now = Instant::now();
        let position = player.position(now);
        let detail = match player.length {
            Some(length) => format!("{} / {}  ·  {}", clock(position), clock(length), player.identity),
            None => player.identity.clone(),
        };
        p.text(&detail, card.player, Style::new(t.font_size * 0.8, t.muted));
        if let Some(length) = player.length {
            let share = (position.as_secs_f64() / length.as_secs_f64().max(1.0)).clamp(0.0, 1.0);
            p.fill(card.progress, t.border, 1.5);
            let done = Rect::new(card.progress.x, card.progress.y, (card.progress.w * share).max(3.0), card.progress.h);
            p.shape(done, Shape { radius: 1.5, fill: (t.accent, t.accent_end), ..Default::default() });
        }
    }

    /// Cover art, or the player's icon, or a note.
    pub fn draw_art(&self, p: &mut Painter, rect: Rect, player: &Player) {
        let t = &self.theme;
        let size = rect.w as u32;
        if let Some(art) = player.art.as_deref().and_then(|path| self.cover_art(path, size)) {
            p.image(&art, (rect.x, rect.y));
            return;
        }
        p.fill(rect, t.surface_raised, t.radius);
        let icon_size = size / 2;
        let icon = player
            .desktop_entry
            .as_deref()
            .and_then(|entry| self.launcher.app_icon(entry, icon_size))
            .or_else(|| self.launcher.app_icon(&player.identity.to_lowercase(), icon_size));
        match icon {
            Some(icon) => {
                let at = rect.x + (rect.w - icon_size as f64) / 2.0;
                p.image(&icon, (at, rect.y + (rect.h - icon_size as f64) / 2.0));
            }
            None => p.glyph(glyph::MUSIC, rect, icon_size.min(28), t.muted),
        }
    }

    /// Cover art filling a square with rounded corners, cached.
    fn cover_art(&self, path: &std::path::Path, size: u32) -> Option<MemoryRenderBuffer> {
        let key = (path.to_path_buf(), size);
        if let Some(art) = self.media.art.borrow().get(&key) {
            return art.clone();
        }
        let art = load_cover(path, size, self.theme.radius);
        let mut cache = self.media.art.borrow_mut();
        if cache.len() >= 32 {
            cache.clear();
        }
        cache.insert(key, art.clone());
        art
    }

    fn stream_icon(&self, stream: &tileception_media::Stream, size: u32) -> Option<MemoryRenderBuffer> {
        [stream.icon.as_deref(), stream.binary.as_deref(), Some(stream.app.to_lowercase().as_str())]
            .into_iter()
            .flatten()
            .find_map(|name| self.launcher.app_icon(name, size))
    }

    /// The controls as laid out in the open panel.
    pub fn panel_media_controls(&self) -> Option<Controls> {
        self.notifications.open.then(|| self.panel_controls().controls)
    }
}

fn load_cover(path: &std::path::Path, size: u32, radius: f32) -> Option<MemoryRenderBuffer> {
    use smithay::{backend::allocator::Fourcc, utils::Transform};
    // Players name the file, so it may be anything: only a regular file of a
    // sensible size is opened (a pipe would block drawing).
    let meta = std::fs::metadata(path).ok()?;
    if !meta.is_file() || meta.len() > 16 * 1024 * 1024 {
        return None;
    }
    let image = image::ImageReader::open(path).ok()?.with_guessed_format().ok()?.decode().ok()?;
    let mut image = image.resize_to_fill(size, size, image::imageops::FilterType::Triangle).to_rgba8();
    let r = f64::from(radius);
    let edge = f64::from(size);
    for (x, y, pixel) in image.enumerate_pixels_mut() {
        // Distance outside the rounded corner, softened over one pixel.
        let (px, py) = (f64::from(x) + 0.5, f64::from(y) + 0.5);
        let cx = px.clamp(r, edge - r);
        let cy = py.clamp(r, edge - r);
        let outside = ((px - cx).hypot(py - cy) - r + 0.5).clamp(0.0, 1.0);
        let alpha = f64::from(pixel.0[3]) / 255.0 * (1.0 - outside);
        for channel in &mut pixel.0[..3] {
            *channel = (f64::from(*channel) * alpha).round() as u8;
        }
        pixel.0[3] = (alpha * 255.0).round() as u8;
    }
    Some(MemoryRenderBuffer::from_slice(
        &image.into_raw(),
        Fourcc::Abgr8888,
        (size as i32, size as i32),
        1,
        Transform::Normal,
        None,
    ))
}

/// A playback card with its top left at (x, y).
pub fn media_card_layout(x: f64, y: f64, w: f64) -> MediaCard {
    let rect = Rect::new(x, y, w, 100.0);
    let art = Rect::new(rect.x + 12.0, rect.y + 12.0, 64.0, 64.0);
    let text_x = art.right() + 12.0;
    let text_w = rect.right() - 12.0 - text_x;
    let buttons_y = rect.y + 50.0;
    MediaCard {
        rect,
        art,
        title: Rect::new(text_x, rect.y + 10.0, text_w, 20.0),
        artist: Rect::new(text_x, rect.y + 30.0, text_w, 18.0),
        player: Rect::new(text_x + 3.0 * 36.0 + 8.0, buttons_y, text_w - 3.0 * 36.0 - 8.0, 30.0),
        previous: Rect::new(text_x - 4.0, buttons_y, 30.0, 30.0),
        play: Rect::new(text_x + 32.0, buttons_y, 30.0, 30.0),
        next: Rect::new(text_x + 68.0, buttons_y, 30.0, 30.0),
        progress: Rect::new(rect.x + 12.0, rect.bottom() - 12.0, rect.w - 24.0, 3.0),
    }
}

/// The speaker icon for a level.
pub fn volume_glyph(value: u32, muted: bool) -> &'static str {
    match value {
        _ if muted => glyph::VOLUME_OFF,
        0 => glyph::VOLUME_NONE,
        1..=50 => glyph::VOLUME_LOW,
        _ => glyph::VOLUME_HIGH,
    }
}

/// "90%", "Muted", or a boosted level in the warning colour.
pub fn value_label(
    value: u32,
    muted: bool,
    boostable: bool,
    t: &crate::theme::Theme,
) -> (String, crate::theme::Colour) {
    if muted {
        ("Muted".into(), t.muted)
    } else if boostable && value > 100 {
        (format!("{value}%"), t.warning)
    } else {
        (format!("{value}%"), t.text)
    }
}

/// A level as a bar: the accent up to 100%, the warning colour past it with a
/// mark at 100%, and a knob when `knob` is given (larger while held).
pub fn level_bar(p: &mut Painter, area: Rect, value: u32, max: u32, muted: bool, knob: Option<bool>) {
    let t = p.theme();
    let max = max.max(1) as f64;
    let bar = Rect::new(area.x, area.y + area.h / 2.0 - 2.0, area.w, 4.0);
    p.fill(bar, t.border, 2.0);
    let x_of = |v: f64| bar.x + bar.w * (v / max).clamp(0.0, 1.0);
    let value = value as f64;
    let normal = value.min(100.0);
    let (a, b) = if muted { (t.muted.alpha(0.6), t.muted.alpha(0.6)) } else { (t.accent, t.accent_end) };
    if normal > 0.0 {
        p.shape(
            Rect::new(bar.x, bar.y, (x_of(normal) - bar.x).max(4.0), bar.h),
            Shape { radius: 2.0, fill: (a, b), ..Default::default() },
        );
    }
    if value > 100.0 {
        let colour = if muted { t.muted.alpha(0.6) } else { t.warning };
        p.fill(Rect::new(x_of(100.0) - 2.0, bar.y, x_of(value) - x_of(100.0) + 2.0, bar.h), colour, 2.0);
    }
    if max > 100.0 {
        // Where boosting starts.
        p.fill(Rect::new(x_of(100.0) - 1.0, bar.y - 4.0, 2.0, bar.h + 8.0), t.muted.alpha(0.8), 1.0);
    }
    if let Some(held) = knob {
        let r = if held { 8.0 } else { 6.0 };
        let centre = (x_of(value), bar.y + bar.h / 2.0);
        let colour = if muted {
            t.muted
        } else if value > 100.0 {
            t.warning
        } else {
            t.text
        };
        p.shape(
            Rect::new(centre.0 - r, centre.1 - r, r * 2.0, r * 2.0),
            Shape {
                glow_radius: if held { 8.0 } else { 0.0 },
                glow: t.accent.alpha(0.35),
                ..Shape::filled(colour, r as f32)
            },
        );
    }
}

/// 3:07, or 1:02:03 for long tracks.
fn clock(d: std::time::Duration) -> String {
    let s = d.as_secs();
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
    } else {
        format!("{}:{:02}", s / 60, s % 60)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn slider(max: u32) -> Slider {
        Slider {
            id: SliderId::Volume,
            button: Rect::new(0.0, 0.0, 32.0, 32.0),
            track: Rect::new(100.0, 0.0, 150.0, 32.0),
            label: Rect::new(260.0, 0.0, 46.0, 32.0),
            value: 0,
            max,
            muted: false,
            name: None,
        }
    }

    #[test]
    fn slider_values_follow_the_pointer() {
        let s = slider(150);
        assert_eq!(s.value_at(90.0), 0);
        assert_eq!(s.value_at(175.0), 75);
        assert_eq!(s.value_at(300.0), 150);
        // Close to 100% lands on it.
        assert_eq!(s.value_at(100.0 + 102.0), 100);
        assert_eq!(s.value_at(100.0 + 98.0), 100);
        assert_eq!(s.value_at(100.0 + 105.0), 105);
        // Without boosting there is nothing to snap to.
        assert_eq!(slider(100).value_at(100.0 + 148.5), 99);
    }

    #[test]
    fn clock_formats_durations() {
        assert_eq!(clock(std::time::Duration::from_secs(187)), "3:07");
        assert_eq!(clock(std::time::Duration::from_secs(3723)), "1:02:03");
    }
}
