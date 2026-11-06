//! The colour picker (Ctrl+Print, `tileception msg pick-colour`): a
//! magnifier follows the pointer over a still of the screen; a click or
//! Enter copies the colour under it as hex, and a notification offers the
//! other notations. Esc or a right click cancels.

use std::time::Duration;

use smithay::{
    backend::renderer::gles::GlesRenderer,
    input::keyboard::keysyms,
    utils::{Logical, Point as SmithayPoint},
};
use tileception_core::Rect;
use tileception_notify::{Image, Urgency};

use crate::{
    Tileception,
    commands::Command,
    config::Mods,
    render::OutputElement,
    shapes::Shape,
    theme::Colour,
    ui::{Align, Painter, Style},
};

/// Pixels shown across the magnifier, an odd number so one is in the middle.
const ZOOM_PIXELS: i32 = 11;
/// How big each magnified pixel is drawn, in points.
const ZOOM_CELL: f64 = 10.0;
/// How long the "copied" notification stays up.
const NOTICE_TIMEOUT: Duration = Duration::from_secs(12);

/// A colour pick in progress.
#[derive(Default)]
pub struct ColourPick {
    /// The screen as it was when the pick started; until the next frame
    /// captures it, nothing shows.
    still: Option<Still>,
}

struct Still {
    rgba: Vec<u8>,
    width: i32,
    height: i32,
    /// Pixels per point when it was taken.
    scale: f64,
}

impl Still {
    /// The colour at a pixel, or none outside the screen.
    fn pixel(&self, column: i32, row: i32) -> Option<[u8; 3]> {
        if column < 0 || row < 0 || column >= self.width || row >= self.height {
            return None;
        }
        let start = (row as usize * self.width as usize + column as usize) * 4;
        let pixel = self.rgba.get(start..start + 3)?;
        Some([pixel[0], pixel[1], pixel[2]])
    }

    /// The pixel under a point.
    fn pixel_at(&self, point: SmithayPoint<f64, Logical>) -> (i32, i32) {
        ((point.x * self.scale).floor() as i32, (point.y * self.scale).floor() as i32)
    }
}

impl Tileception {
    /// Starts picking a colour: the next frame takes a still of the screen.
    pub fn pick_colour(&mut self) {
        if self.is_locked() {
            return;
        }
        // Open panels stay, so their colours can be picked too; the picker
        // takes the pointer and keyboard from them until it is done.
        self.colour_pick = Some(ColourPick::default());
        let region = smithay::utils::Rectangle::from_size(self.output_pixels().into());
        self.pending_captures
            .push(crate::capture::PendingCapture { region, target: crate::capture::CaptureTarget::ColourPick });
        self.force_keyboard_focus_sync();
        self.schedule_render();
    }

    /// The still for the pick, from [`Self::process_captures`].
    pub fn colour_pick_taken(&mut self, rgba: Vec<u8>, (width, height): (i32, i32)) {
        let scale = self.scale;
        if let Some(pick) = self.colour_pick.as_mut() {
            pick.still = Some(Still { rgba, width, height, scale });
            self.schedule_render();
        }
    }

    pub fn cancel_colour_pick(&mut self) {
        if self.colour_pick.take().is_some() {
            self.force_keyboard_focus_sync();
            self.schedule_render();
        }
    }

    /// Copies the colour under the pointer as hex, and offers the other
    /// notations in a notification.
    pub fn finish_colour_pick(&mut self) {
        let Some(pick) = self.colour_pick.take() else { return };
        self.force_keyboard_focus_sync();
        self.schedule_render();
        let Some(still) = pick.still else { return };
        let (column, row) = still.pixel_at(self.pointer);
        let Some(rgb) = still.pixel(column, row) else { return };
        let [hex, rgb_text, hsl] = crate::answers::colour_notations(rgb);
        self.copy_text(hex.clone());
        // The other notations are a button away, also from the launcher's top rows.
        let buttons = vec![
            ("Copy RGB", Some(Command::CopyText(rgb_text.clone()))),
            ("Copy HSL", Some(Command::CopyText(hsl.clone()))),
            ("Pick again", Some(Command::PickColour)),
        ];
        let body = format!("{rgb_text}\n{hsl}");
        let notification_id = self.notify_locally_with(&format!("{hex} copied"), &body, Urgency::Low, buttons);
        self.set_local_notification_image(notification_id, swatch(rgb));
        let timer = smithay::reexports::calloop::timer::Timer::from_duration(NOTICE_TIMEOUT);
        let closing = self.loop_handle.insert_source(timer, move |_, _, state| {
            state.close_local_notification(notification_id);
            smithay::reexports::calloop::timer::TimeoutAction::Drop
        });
        if let Err(error) = closing {
            tracing::warn!("could not time the colour notification: {error}");
        }
    }

    /// Keys while picking: arrows nudge the pointer a pixel (ten with
    /// Shift), Enter or Space picks, Esc cancels.
    pub fn colour_pick_key(&mut self, sym: u32, mods: Mods) {
        let step = if mods.shift { 10.0 } else { 1.0 } / self.scale.max(0.1);
        let (right, down) = match sym {
            keysyms::KEY_Escape => return self.cancel_colour_pick(),
            keysyms::KEY_Return | keysyms::KEY_KP_Enter | keysyms::KEY_space => return self.finish_colour_pick(),
            keysyms::KEY_Left => (-step, 0.0),
            keysyms::KEY_Right => (step, 0.0),
            keysyms::KEY_Up => (0.0, -step),
            keysyms::KEY_Down => (0.0, step),
            _ => return,
        };
        let screen = self.output_rect();
        let left = (self.pointer.x + right).clamp(screen.x, screen.right() - 0.01);
        let top = (self.pointer.y + down).clamp(screen.y, screen.bottom() - 0.01);
        let time = (self.now() * 1000.0) as u32;
        self.on_motion((left, top).into(), time);
    }

    /// The magnifier next to the pointer, with the hex value under it.
    pub fn colour_pick_elements(&self, renderer: &mut GlesRenderer) -> Vec<OutputElement> {
        let Some(still) = self.colour_pick.as_ref().and_then(|pick| pick.still.as_ref()) else { return Vec::new() };
        let theme = &self.theme;
        let screen = self.output_rect();
        let side = f64::from(ZOOM_PIXELS) * ZOOM_CELL;
        let label_h = 30.0;
        let gap = 24.0;
        let (width, height) = (side + 12.0, side + 12.0 + label_h);
        // Below and right of the pointer, flipped near the screen's edges.
        let mut left = self.pointer.x + gap;
        if left + width > screen.right() {
            left = self.pointer.x - gap - width;
        }
        let mut top = self.pointer.y + gap;
        if top + height > screen.bottom() {
            top = self.pointer.y - gap - height;
        }
        let card = Rect::new(left, top, width, height);
        let mut painter = Painter::new(self, renderer);
        painter.popup(card);
        let (centre_x, centre_y) = still.pixel_at(self.pointer);
        let half = ZOOM_PIXELS / 2;
        for row in 0..ZOOM_PIXELS {
            for column in 0..ZOOM_PIXELS {
                let colour = still
                    .pixel(centre_x + column - half, centre_y + row - half)
                    .map_or(Colour([0.0, 0.0, 0.0, 1.0]), opaque);
                let cell = Rect::new(
                    card.x + 6.0 + f64::from(column) * ZOOM_CELL,
                    card.y + 6.0 + f64::from(row) * ZOOM_CELL,
                    ZOOM_CELL,
                    ZOOM_CELL,
                );
                painter.fill(cell, colour, 0.0);
            }
        }
        // The pixel that would be picked, outlined in black and white.
        let middle = Rect::new(
            card.x + 6.0 + f64::from(half) * ZOOM_CELL - 1.0,
            card.y + 6.0 + f64::from(half) * ZOOM_CELL - 1.0,
            ZOOM_CELL + 2.0,
            ZOOM_CELL + 2.0,
        );
        painter.shape(
            middle,
            Shape { border_width: 1.0, border: Colour([1.0; 4]), ..Shape::filled(Colour::TRANSPARENT, 0.0) },
        );
        let inner = Rect::new(middle.x + 1.0, middle.y + 1.0, middle.w - 2.0, middle.h - 2.0);
        painter.shape(
            inner,
            Shape {
                border_width: 1.0,
                border: Colour([0.0, 0.0, 0.0, 1.0]),
                ..Shape::filled(Colour::TRANSPARENT, 0.0)
            },
        );
        if let Some(rgb) = still.pixel(centre_x, centre_y) {
            let [hex, ..] = crate::answers::colour_notations(rgb);
            let label = Rect::new(card.x + 6.0, card.y + 6.0 + side, side, label_h);
            let swatch = Rect::new(label.x, label.y + 8.0, 14.0, 14.0);
            painter.shape(swatch, Shape { border_width: 1.0, border: theme.border, ..Shape::filled(opaque(rgb), 3.0) });
            painter.text(
                &hex,
                Rect::new(label.x + 20.0, label.y, label.w - 20.0, label.h),
                Style::new(theme.font_size, theme.text).bold().align(Align::Centre),
            );
        }
        painter.finish()
    }
}

fn opaque([red, green, blue]: [u8; 3]) -> Colour {
    Colour([f32::from(red) / 255.0, f32::from(green) / 255.0, f32::from(blue) / 255.0, 1.0])
}

/// A small square of the colour, as the notification's picture.
fn swatch(rgb: [u8; 3]) -> Image {
    let side = 32;
    let data = std::iter::repeat_n([rgb[0], rgb[1], rgb[2], 255], side * side).flatten().collect();
    Image::Rgba { width: side as u32, height: side as u32, data }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test setup and assertions should fail immediately on unexpected errors")]
mod tests {
    use super::*;

    #[test]
    fn reads_pixels_under_points() {
        // Two by two: red, green / blue, white; at scale 2 a point is two pixels.
        let rgba = vec![255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 255, 255];
        let still = Still { rgba, width: 2, height: 2, scale: 2.0 };
        assert_eq!(still.pixel_at((0.6, 0.2).into()), (1, 0));
        assert_eq!(still.pixel(1, 0), Some([0, 255, 0]));
        assert_eq!(still.pixel(0, 1), Some([0, 0, 255]));
        assert_eq!(still.pixel(2, 0), None);
        assert_eq!(still.pixel(-1, 0), None);
    }

    #[test]
    fn swatches_are_solid() {
        let Image::Rgba { width, height, data } = swatch([1, 2, 3]) else { panic!("not pixels") };
        assert_eq!((width, height, data.len()), (32, 32, 32 * 32 * 4));
        assert_eq!(&data[..4], &[1, 2, 3, 255]);
    }
}
