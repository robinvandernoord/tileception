//! Presenting a snipped area: the screen shows only that area, live and as
//! large as it fits, with dark bars beside it. The pointer stays inside the
//! area and moves at the enlarged scale, so programs there keep working.
//! Escape ends it, and so does locking the screen.
//!
//! The whole screen is drawn into a texture first, the way `mirror` draws a
//! frame for another GPU, and the texture's part for the area is shown.

use smithay::{
    backend::{
        allocator::Fourcc,
        renderer::{
            Bind, Offscreen,
            damage::OutputDamageTracker,
            element::{
                Kind, RenderElement,
                texture::{TextureRenderBuffer, TextureRenderElement},
            },
            gles::{GlesRenderer, GlesTexture},
        },
    },
    output::Output,
    utils::{Buffer, Logical, Physical, Point, Rectangle, Size, Transform},
};
use tileception_core::Rect;

use crate::{Tileception, render::OutputElement};

/// The smallest area worth presenting, in logical points.
const SMALLEST: f64 = 8.0;

pub struct Zoom {
    output: Output,
    /// The presented area, in the screen's logical points.
    area: Rect,
    frame: Option<Frame>,
}

/// The screen at full size, drawn again only where it changed. Mirrored
/// screens show one too (see `mirroring`).
pub(crate) struct Frame {
    pub(crate) size: Size<i32, Physical>,
    damage: OutputDamageTracker,
    pub(crate) buffer: TextureRenderBuffer<GlesTexture>,
    /// The buffer holds the last frame, so only damage needs drawing.
    drawn: bool,
}

impl Frame {
    pub(crate) fn new(renderer: &mut GlesRenderer, size: Size<i32, Physical>) -> Result<Self, String> {
        let texture: GlesTexture = renderer
            .create_buffer(Fourcc::Abgr8888, Size::<i32, Buffer>::from((size.w, size.h)))
            .map_err(|error| error.to_string())?;
        let full = Rectangle::<i32, Buffer>::from_size((size.w, size.h).into());
        Ok(Self {
            size,
            damage: OutputDamageTracker::new(size, 1.0, Transform::Normal),
            buffer: TextureRenderBuffer::from_texture(renderer, texture, 1, Transform::Normal, Some(vec![full])),
            drawn: false,
        })
    }

    pub(crate) fn draw<E: RenderElement<GlesRenderer>>(
        &mut self,
        renderer: &mut GlesRenderer,
        elements: &[E],
        clear: [f32; 4],
    ) -> Result<(), String> {
        let age = usize::from(std::mem::take(&mut self.drawn));
        let full = Rectangle::from_size(self.size);
        let tracker = &mut self.damage;
        self.buffer.render().draw(|texture| {
            let mut target = renderer.bind(texture).map_err(|error| error.to_string())?;
            let result = tracker
                .render_output(renderer, &mut target, age, elements, clear)
                .map_err(|error| format!("{error:?}"))?;
            let damage: Vec<Rectangle<i32, Buffer>> = result
                .damage
                .into_iter()
                .flatten()
                .filter_map(|rect| rect.intersection(full))
                .map(|rect| Rectangle::new((rect.loc.x, rect.loc.y).into(), (rect.size.w, rect.size.h).into()))
                .collect();
            Ok::<_, String>(damage)
        })?;
        self.drawn = true;
        Ok(())
    }
}

/// Where an area of `screen` shows when it is enlarged to fit, centred.
pub(crate) fn shown(screen: Rect, area: Rect) -> Rect {
    let factor = (screen.w / area.w).min(screen.h / area.h);
    let (width, height) = (area.w * factor, area.h * factor);
    Rect::new(screen.x + (screen.w - width) / 2.0, screen.y + (screen.h - height) / 2.0, width, height)
}

/// A point kept inside `area`, short of its right and bottom edges.
fn inside(area: Rect, point: Point<f64, Logical>) -> Point<f64, Logical> {
    let x_pos = point.x.clamp(area.x, (area.right() - 1.0).max(area.x));
    let y_pos = point.y.clamp(area.y, (area.bottom() - 1.0).max(area.y));
    (x_pos, y_pos).into()
}

impl Tileception {
    /// Starts presenting `area` of the context screen.
    pub(crate) fn start_zoom(&mut self, area: Rect) {
        let Some(output) = self.output.clone() else { return };
        let screen = self.output_rect();
        let (left, top) = (area.x.max(screen.x), area.y.max(screen.y));
        let (right, bottom) = (area.right().min(screen.right()), area.bottom().min(screen.bottom()));
        if right - left < SMALLEST || bottom - top < SMALLEST {
            return;
        }
        let area = Rect::new(left, top, right - left, bottom - top);
        self.zoom = Some(Zoom { output, area, frame: None });
        // The pointer starts inside, so the first motion does not jump.
        let pointer = inside(area, self.pointer);
        if pointer != self.pointer {
            self.on_motion(pointer, (self.now() * 1000.0) as u32);
        }
        self.schedule_render();
    }

    /// Ends the presentation. Returns whether there was one.
    pub(crate) fn end_zoom(&mut self) -> bool {
        let ended = self.zoom.take().is_some();
        if ended {
            self.schedule_render();
        }
        ended
    }

    /// The presented area and where it shows, when the context screen
    /// presents one.
    pub(crate) fn zoom_here(&self) -> Option<(Rect, Rect)> {
        let zoom = self.zoom.as_ref()?;
        if self.output.as_ref() != Some(&zoom.output) || self.is_locked() {
            return None;
        }
        Some((zoom.area, shown(self.output_rect(), zoom.area)))
    }

    /// Relative motion from the pointer: slower by the enlargement while
    /// presenting, and kept inside the area.
    pub(crate) fn zoom_relative(&self, delta: Point<f64, Logical>) -> Point<f64, Logical> {
        match self.zoom_here() {
            Some((area, shown)) => inside(area, self.pointer + delta.downscale(shown.w / area.w)),
            None => self.pointer + delta,
        }
    }

    /// A point on the screen as the eye sees it (a touch, a tablet, `msg
    /// pointer`): the point in the area under it while presenting.
    pub(crate) fn zoom_absolute(&self, point: Point<f64, Logical>) -> Point<f64, Logical> {
        match self.zoom_here() {
            Some((area, shown)) => {
                let factor = shown.w / area.w;
                let mapped =
                    Point::from((area.x + (point.x - shown.x) / factor, area.y + (point.y - shown.y) / factor));
                inside(area, mapped)
            }
            None => point,
        }
    }

    /// Where the pointer shows: enlarged with the area while presenting.
    pub(crate) fn zoom_pointer(&self) -> Point<f64, Logical> {
        match self.zoom_here() {
            Some((area, shown)) => {
                let factor = shown.w / area.w;
                (shown.x + (self.pointer.x - area.x) * factor, shown.y + (self.pointer.y - area.y) * factor).into()
            }
            None => self.pointer,
        }
    }

    /// The frame as shown: unchanged, or only the presented area enlarged.
    pub(crate) fn zoom_elements(
        &mut self,
        renderer: &mut GlesRenderer,
        elements: Vec<OutputElement>,
    ) -> Vec<OutputElement> {
        if self.zoom.as_ref().is_some_and(|_| self.is_locked()) {
            self.zoom = None;
        }
        let Some((area, shown)) = self.zoom_here() else { return elements };
        let (width, height) = self.output_pixels();
        let size = Size::from((width, height));
        let clear = self.clear_colour();
        let scale = self.scale;
        let Some(zoom) = self.zoom.as_mut() else { return elements };
        if zoom.frame.as_ref().is_none_or(|frame| frame.size != size) {
            zoom.frame = match Frame::new(renderer, size) {
                Ok(frame) => Some(frame),
                Err(error) => {
                    tracing::warn!("cannot present the area: {error}");
                    self.zoom = None;
                    return elements;
                }
            };
        }
        let Some(frame) = zoom.frame.as_mut() else { return elements };
        if let Err(error) = frame.draw(renderer, &elements, clear) {
            tracing::warn!("cannot present the area: {error}");
            self.zoom = None;
            return elements;
        }
        let source = crate::render::physical(&area, scale);
        let place = crate::render::physical(&shown, scale);
        let enlarged = TextureRenderElement::from_texture_render_buffer(
            (f64::from(place.loc.x), f64::from(place.loc.y)),
            &frame.buffer,
            None,
            Some(Rectangle::new(
                (f64::from(source.loc.x), f64::from(source.loc.y)).into(),
                (f64::from(source.size.w), f64::from(source.size.h)).into(),
            )),
            Some(Size::from(((shown.w).round() as i32, (shown.h).round() as i32))),
            Kind::Unspecified,
        );
        vec![OutputElement::Texture(enlarged), crate::render::solid(&self.output_rect(), [0.0, 0.0, 0.0, 1.0], scale)]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_wide_area_fills_the_width_with_bars_above_and_below() {
        let placed = shown(Rect::new(0.0, 0.0, 1600.0, 900.0), Rect::new(100.0, 100.0, 400.0, 100.0));
        assert_eq!(placed, Rect::new(0.0, 250.0, 1600.0, 400.0));
    }

    #[test]
    fn a_tall_area_fills_the_height_with_bars_beside() {
        let placed = shown(Rect::new(0.0, 0.0, 1600.0, 900.0), Rect::new(0.0, 0.0, 300.0, 300.0));
        assert_eq!(placed, Rect::new(350.0, 0.0, 900.0, 900.0));
    }

    #[test]
    fn the_pointer_stays_short_of_the_far_edges() {
        let area = Rect::new(10.0, 20.0, 100.0, 50.0);
        assert_eq!(inside(area, (0.0, 500.0).into()), Point::from((10.0, 69.0)));
        assert_eq!(inside(area, (500.0, 0.0).into()), Point::from((109.0, 20.0)));
    }
}
