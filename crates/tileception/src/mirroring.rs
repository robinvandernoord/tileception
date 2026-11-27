//! Display mirroring: a screen with `mirror = "<source>"` in `[outputs]`
//! shows another screen instead of a desktop of its own, for a projector
//! or a TV during a presentation.
//!
//! A mirror stays lit at its own mode but has no desktop: programs do not
//! see it as a `wl_output`, the pointer never moves onto it and it takes no
//! part in arranging screens. The source's whole frame (pointer, bar and
//! lock screen too) is drawn into a texture, the way a presented area is
//! (`zoom`), and the mirror shows that as large as it fits, centred between
//! black bars. Each source draws one texture for all its mirrors. Frame
//! callbacks and presentation feedback come only from the source, so
//! programs are paced by one screen. (`mirror` is not this: it hands frames
//! to a screen on another GPU.)

use std::{collections::BTreeMap, path::PathBuf};

use smithay::{
    backend::renderer::{
        element::{Kind, texture::TextureRenderElement},
        gles::GlesRenderer,
    },
    output::Output,
    utils::{Logical, Physical, Point, Rectangle, Size},
};
use tileception_core::Rect;

use crate::{Tileception, render::OutputElement, zoom::Frame};

#[derive(Default)]
pub struct Mirroring {
    /// The source each mirroring screen shows, by the mirror's name.
    pub(crate) sources: BTreeMap<String, Output>,
    /// Each source's last frame, by the source's name.
    frames: BTreeMap<String, Frame>,
    /// Screenshots of mirrors, taken when the mirror is next drawn.
    captures: Vec<Capture>,
}

struct Capture {
    mirror: String,
    path: PathBuf,
    request: u64,
}

impl Mirroring {
    /// Forgets the textures of screens nothing mirrors any more.
    pub(crate) fn tidy(&mut self) {
        let sources = &self.sources;
        self.frames.retain(|name, _| sources.values().any(|source| source.name() == *name));
    }

    /// Gives up a screenshot whose command timed out. Returns how many.
    pub(crate) fn drop_capture(&mut self, request: u64) -> usize {
        let before = self.captures.len();
        self.captures.retain(|capture| capture.request != request);
        before - self.captures.len()
    }
}

/// Where a source of `source` pixels shows on a mirror of `target` pixels:
/// as large as it fits with its shape kept, centred, on whole pixels.
pub(crate) fn letterbox(source: (i32, i32), target: (i32, i32)) -> Rectangle<i32, Physical> {
    let screen = Rect::new(0.0, 0.0, f64::from(target.0), f64::from(target.1));
    if source.0 <= 0 || source.1 <= 0 {
        return Rectangle::from_size(target.into());
    }
    let placed = crate::zoom::shown(screen, Rect::new(0.0, 0.0, f64::from(source.0), f64::from(source.1)));
    crate::render::physical(&placed, 1.0)
}

/// How far across and down the source's picture a point on a mirror of
/// `target` pixels is, from 0 to 1, kept inside the picture.
fn picture_fraction(source: (i32, i32), target: (i32, i32), point: Point<f64, Logical>) -> (f64, f64) {
    let placed = letterbox(source, target);
    let fraction =
        |offset: f64, start: i32, length: i32| ((offset - f64::from(start)) / f64::from(length.max(1))).clamp(0.0, 1.0);
    (fraction(point.x, placed.loc.x, placed.size.w), fraction(point.y, placed.loc.y, placed.size.h))
}

/// A screen's size in pixels, turned as it is mounted.
fn pixels_of(output: &Output) -> (i32, i32) {
    let size =
        output.current_mode().map(|mode| output.current_transform().transform_size(mode.size)).unwrap_or_default();
    (size.w, size.h)
}

impl Tileception {
    /// The screen `output` shows, while it mirrors one.
    pub fn mirror_source(&self, output: &Output) -> Option<&Output> {
        self.outputs.mirroring.sources.get(&output.name())
    }

    /// Whether some screen mirrors the context screen.
    pub(crate) fn is_mirrored(&self) -> bool {
        self.output
            .as_ref()
            .is_some_and(|output| self.outputs.mirroring.sources.values().any(|source| source == output))
    }

    /// Draws the context screen's frame (`scene`, with the pointer added
    /// here) for the screens mirroring it.
    pub(crate) fn draw_for_mirrors(&mut self, renderer: &mut GlesRenderer, scene: &[OutputElement]) {
        if !self.is_mirrored() {
            return;
        }
        let Some(name) = self.output.as_ref().map(Output::name) else { return };
        let pointer = self.capture_pointer(renderer);
        let elements: Vec<&OutputElement> = pointer.iter().chain(scene).collect();
        let (width, height) = self.output_pixels();
        let size: Size<i32, Physical> = (width, height).into();
        let clear = self.clear_colour();
        let frames = &mut self.outputs.mirroring.frames;
        if frames.get(&name).is_none_or(|frame| frame.size != size) {
            match Frame::new(renderer, size) {
                Ok(frame) => {
                    frames.insert(name.clone(), frame);
                }
                Err(error) => {
                    tracing::warn!("cannot mirror {name}: {error}");
                    frames.remove(&name);
                    return;
                }
            }
        }
        if let Some(frame) = frames.get_mut(&name)
            && let Err(error) = frame.draw(renderer, &elements, clear)
        {
            tracing::warn!("cannot mirror {name}: {error}");
            // Its texture may hold half a frame: start afresh next time.
            frames.remove(&name);
        }
    }

    /// What a mirroring screen shows, topmost first: its source's last
    /// frame, scaled to fit, on black. Only black before the source drew.
    pub(crate) fn mirror_elements(&self, mirror: &Output) -> Vec<OutputElement> {
        let target = pixels_of(mirror);
        let black = crate::render::solid(
            &Rect::new(0.0, 0.0, f64::from(target.0), f64::from(target.1)),
            [0.0, 0.0, 0.0, 1.0],
            1.0,
        );
        let frame = self.mirror_source(mirror).and_then(|source| self.outputs.mirroring.frames.get(&source.name()));
        let Some(frame) = frame else { return vec![black] };
        let placed = letterbox((frame.size.w, frame.size.h), target);
        let shown = TextureRenderElement::from_texture_render_buffer(
            (f64::from(placed.loc.x), f64::from(placed.loc.y)),
            &frame.buffer,
            None,
            // All of it: left out, only as much as the size below is shown.
            Some(Rectangle::from_size((f64::from(frame.size.w), f64::from(frame.size.h)).into())),
            // The mirror is drawn at scale 1, so this is in pixels.
            Some(Size::from((placed.size.w, placed.size.h))),
            Kind::Unspecified,
        );
        vec![OutputElement::Texture(shown), black]
    }

    /// Where a touch at `position` (0 to 1 across the panel) on a mirroring
    /// touchscreen lands on its source, the context screen. A touch on the
    /// bars lands on the nearest edge of the picture.
    pub(crate) fn mirror_touch_position(&self, mirror: &Output, position: Point<f64, Logical>) -> Point<f64, Logical> {
        let target = pixels_of(mirror);
        let transform = mirror.current_transform();
        let on_panel =
            crate::touch::map_absolute_position(position, (f64::from(target.0), f64::from(target.1)).into(), transform);
        let screen = self.output_rect();
        let (across, down) = picture_fraction(self.output_pixels(), target, on_panel);
        (screen.x + across * screen.w, screen.y + down * screen.h).into()
    }

    /// Takes a screenshot of the screen `name` into a PNG file, as
    /// `screenshot` does for the screen in use: a mirror shows its source.
    pub fn screenshot_output(&mut self, name: &str, path: PathBuf) -> Result<(), String> {
        if self.outputs.mirroring.sources.contains_key(name) {
            let request = self.ipc.next_request();
            self.defer_reply_for(request, crate::capture::SCREENSHOT_WAIT, "the mirror was not drawn in time");
            self.outputs.mirroring.captures.push(Capture { mirror: name.into(), path, request });
            self.schedule_render();
            return Ok(());
        }
        let output = self.output_named(name).ok_or_else(|| format!("unknown output {name:?}"))?;
        self.with_output(&output, |state| state.screenshot(path));
        Ok(())
    }

    /// Writes the screenshots waiting for `mirror`, which shows `elements`.
    pub(crate) fn process_mirror_captures(
        &mut self,
        renderer: &mut GlesRenderer,
        mirror: &Output,
        elements: &[OutputElement],
    ) {
        let name = mirror.name();
        let captures: Vec<Capture> =
            self.outputs.mirroring.captures.extract_if(.., |capture| capture.mirror == name).collect();
        if captures.is_empty() {
            return;
        }
        let size = pixels_of(mirror);
        let scene = crate::capture::Scene { elements: elements.iter().collect(), size, clear: [0.0, 0.0, 0.0, 1.0] };
        let region = Rectangle::from_size(size.into());
        let mut texture = None;
        for capture in captures {
            let written = crate::capture::pixels(renderer, &mut texture, &scene, region).and_then(|rgba| {
                crate::capture::write_png(&capture.path, size.0 as u32, size.1 as u32, &rgba)
                    .map_err(|error| format!("could not write {}: {error}", capture.path.display()))
            });
            match &written {
                Ok(()) => tracing::info!("screenshot of {name} saved to {}", capture.path.display()),
                Err(error) => tracing::warn!("screenshot of {name} failed: {error}"),
            }
            self.reply_later(capture.request, written.map(|()| "ok\n".to_string()));
        }
    }

    /// Without a screen nothing is drawn unless asked: draws the sources of
    /// mirrors with screenshots waiting, then takes those.
    pub(crate) fn process_headless_mirrors(&mut self, renderer: &mut GlesRenderer) {
        let mut waiting: Vec<String> =
            self.outputs.mirroring.captures.iter().map(|capture| capture.mirror.clone()).collect();
        waiting.sort();
        waiting.dedup();
        for name in waiting {
            let Some(source) = self.outputs.mirroring.sources.get(&name).cloned() else {
                // No longer a mirror: it went, or became a screen of its own.
                let gone: Vec<Capture> =
                    self.outputs.mirroring.captures.extract_if(.., |capture| capture.mirror == name).collect();
                for capture in gone {
                    self.reply_later(capture.request, Err(format!("{name} no longer mirrors a screen")));
                }
                continue;
            };
            self.with_output(&source, |state| {
                let scene = state.scene(renderer, None);
                state.draw_for_mirrors(renderer, &scene.elements);
            });
            let Some(mirror) = self.outputs.connected_named(&name) else { continue };
            let elements = self.mirror_elements(&mirror);
            self.process_mirror_captures(renderer, &mirror, &elements);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_wider_source_has_bars_above_and_below() {
        let placed = letterbox((1920, 1080), (1280, 800));
        assert_eq!(placed, Rectangle::new((0, 40).into(), (1280, 720).into()));
    }

    #[test]
    fn a_smaller_source_is_enlarged_with_bars_beside() {
        // 16:10 on a 16:9 TV: enlarged 1.35 times.
        let placed = letterbox((1280, 800), (1920, 1080));
        assert_eq!(placed, Rectangle::new((96, 0).into(), (1728, 1080).into()));
    }

    #[test]
    fn the_same_shape_fills_the_screen() {
        assert_eq!(letterbox((3840, 2160), (1920, 1080)), Rectangle::new((0, 0).into(), (1920, 1080).into()));
        assert_eq!(letterbox((1920, 1080), (1920, 1080)), Rectangle::new((0, 0).into(), (1920, 1080).into()));
    }

    #[test]
    fn touches_land_on_the_picture_and_its_edges() {
        // 1920x1080 on 1280x800: the picture is 1280x720, 40 pixels down.
        let fraction = |across, down| picture_fraction((1920, 1080), (1280, 800), Point::from((across, down)));
        assert_eq!(fraction(640.0, 400.0), (0.5, 0.5));
        assert_eq!(fraction(0.0, 40.0), (0.0, 0.0));
        assert_eq!(fraction(1280.0, 790.0), (1.0, 1.0));
        assert_eq!(fraction(320.0, 10.0), (0.25, 0.0));
    }

    #[test]
    fn a_turned_source_stands_in_the_middle() {
        let placed = letterbox((1080, 1920), (1920, 1080));
        assert_eq!(placed.size.h, 1080);
        assert_eq!(placed.loc.x, (1920 - placed.size.w) / 2);
    }
}
