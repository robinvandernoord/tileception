//! Shake to find the pointer: moving the mouse quickly back and forth makes
//! the arrow grow, and it shrinks back once the shaking stops. There is no
//! upper limit; an arrow that outgrows the screen bursts into a shower of
//! small arrows.

use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

/// The `[pointer]` section of the config.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct PointerConfig {
    /// Shake the mouse to make the pointer grow.
    pub shake_to_find: bool,
}

/// A stroke shorter than this, in logical pixels, does not count towards a shake.
const MIN_STROKE: f64 = 24.0;
/// Turns closer together than this count as one shake.
const WINDOW: Duration = Duration::from_millis(800);
/// Turns within `WINDOW` before the pointer grows.
const TURNS: usize = 3;
/// Slowest stroke that counts, in logical pixels per millisecond.
const MIN_SPEED: f64 = 0.6;
/// Enlargement per logical pixel moved while shaking.
const GROWTH: f64 = 0.006;
/// How long the pointer keeps its size after the shaking stops.
const HOLD: Duration = Duration::from_millis(350);
/// How long it then takes to lose half of its extra size.
const HALF_LIFE: Duration = Duration::from_millis(120);
/// How long the burst of small arrows lasts.
pub const BURST: Duration = Duration::from_millis(1800);
/// How many small arrows burst out.
const PIECES: usize = 48;

/// One run of motion in roughly one direction.
#[derive(Debug, Clone, Copy)]
struct Stroke {
    /// Summed motion, in logical pixels.
    across: f64,
    down: f64,
    /// Distance travelled along it.
    length: f64,
    started: Instant,
}

/// Follows the pointer's motion to tell when it is being shaken.
#[derive(Debug, Default)]
pub struct Shake {
    stroke: Option<Stroke>,
    /// When the recent changes of direction happened.
    turns: Vec<Instant>,
    /// The enlargement when it last grew, and when that was.
    grown: Option<(f64, Instant)>,
    /// Where and when the arrow burst.
    burst: Option<(f64, f64, Instant)>,
}

/// One of the small arrows of a burst, in logical pixels around the pointer.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Piece {
    pub left: f64,
    pub top: f64,
    /// Times the pointer's normal size.
    pub size: f64,
    pub alpha: f32,
}

impl Shake {
    /// Takes the pointer moving by `(across, down)`.
    pub fn motion(&mut self, across: f64, down: f64, now: Instant) {
        let length = across.hypot(down);
        if length == 0.0 {
            return;
        }
        let stroke = self.stroke.get_or_insert(Stroke { across: 0.0, down: 0.0, length: 0.0, started: now });
        let along = stroke.across.hypot(stroke.down);
        // Back against the stroke so far: a turn, when the stroke was long and fast.
        let reversed = along > 0.0 && (across * stroke.across + down * stroke.down) / (length * along) < -0.5;
        if reversed {
            let millis = now.duration_since(stroke.started).as_secs_f64() * 1000.0;
            if stroke.length >= MIN_STROKE && stroke.length / millis.max(1.0) >= MIN_SPEED {
                self.turns.push(now);
            } else {
                self.turns.clear();
            }
            *stroke = Stroke { across: 0.0, down: 0.0, length: 0.0, started: now };
        }
        stroke.across += across;
        stroke.down += down;
        stroke.length += length;
        self.turns.retain(|turn| now.duration_since(*turn) < WINDOW);
        if self.turns.len() >= TURNS {
            let scale = self.scale(now) + length * GROWTH;
            self.grown = Some((scale, now));
        }
    }

    /// How many times its normal size the pointer shows now.
    pub fn scale(&self, now: Instant) -> f64 {
        let Some((scale, when)) = self.grown else { return 1.0 };
        let shrinking = now.duration_since(when).saturating_sub(HOLD);
        1.0 + (scale - 1.0) * 0.5f64.powf(shrinking.as_secs_f64() / HALF_LIFE.as_secs_f64())
    }

    /// Whether the pointer is enlarged or bursting, so frames are still needed.
    pub fn active(&self, now: Instant) -> bool {
        self.scale(now) > 1.01 || self.burst.is_some_and(|(_, _, when)| now.duration_since(when) < BURST)
    }

    /// Bursts the arrow at `(left, top)`: it goes back to its normal size and
    /// small arrows fly out.
    pub fn burst(&mut self, left: f64, top: f64, now: Instant) {
        self.grown = None;
        self.turns.clear();
        self.burst = Some((left, top, now));
    }

    /// The small arrows of a burst, `pointer` being the normal pointer
    /// height in logical pixels.
    pub fn pieces(&self, pointer: f64, now: Instant) -> Vec<Piece> {
        let Some((left, top, when)) = self.burst else { return Vec::new() };
        let elapsed = now.duration_since(when);
        if elapsed >= BURST {
            return Vec::new();
        }
        let secs = elapsed.as_secs_f64();
        let remaining = 1.0 - secs / BURST.as_secs_f64();
        (0..PIECES)
            .map(|index| {
                // Spread evenly round the circle, with a little scatter.
                let jitter = scatter(index);
                let angle = index as f64 / PIECES as f64 * std::f64::consts::TAU + jitter;
                let speed = pointer * (14.0 + 18.0 * scatter(index + PIECES));
                let gravity = pointer * 40.0;
                Piece {
                    left: left + angle.cos() * speed * secs,
                    top: top + angle.sin() * speed * secs + gravity * secs * secs / 2.0,
                    size: 0.6 + 1.4 * scatter(index + 2 * PIECES),
                    alpha: (remaining * remaining) as f32,
                }
            })
            .collect()
    }
}

impl crate::state::Tileception {
    /// Takes the mouse moving the pointer to `pos`, before it gets there.
    pub(crate) fn shake_motion(&mut self, pos: smithay::utils::Point<f64, smithay::utils::Logical>) {
        if !self.config.file.pointer.shake_to_find {
            return;
        }
        let now = Instant::now();
        self.shake.motion(pos.x - self.pointer.x, pos.y - self.pointer.y, now);
        if self.shake.active(now) {
            self.schedule_render();
        }
    }

    /// The enlarged arrow and the burst's small arrows, with the hotspot at
    /// `pos` in logical pixels; `None` while the pointer has its normal size
    /// and nothing is bursting.
    pub(crate) fn shaken_pointer(
        &mut self,
        renderer: &mut smithay::backend::renderer::gles::GlesRenderer,
        cursor: &mut crate::cursor::Cursor,
        pos: smithay::utils::Point<f64, smithay::utils::Logical>,
    ) -> Option<Vec<crate::render::OutputElement>> {
        let now = Instant::now();
        if !self.config.file.pointer.shake_to_find || !self.shake.active(now) {
            return None;
        }
        let scale = self.scale;
        // The normal arrow's height, in logical pixels.
        let pointer = f64::from(cursor.arrow().height) / scale;
        let mut factor = self.shake.scale(now);
        // Taller than the screen: it bursts.
        if pointer * factor >= self.output_rect().h {
            self.shake.burst(pos.x, pos.y, now);
            factor = 1.0;
        }
        self.wake_for_cursor(Duration::from_millis(16));
        let physical = |left: f64, top: f64| (left * scale, top * scale);
        let mut elements = Vec::new();
        let frame = if factor > 1.01 { cursor.big_arrow() } else { cursor.arrow() };
        elements.extend(scaled(renderer, frame, physical(pos.x, pos.y), pointer * factor * scale, 1.0));
        let arrow = cursor.arrow();
        for piece in self.shake.pieces(pointer, now) {
            elements.extend(scaled(
                renderer,
                arrow,
                physical(piece.left, piece.top),
                pointer * piece.size * scale,
                piece.alpha,
            ));
        }
        Some(elements)
    }
}

/// `frame` drawn `height` physical pixels tall, its hotspot at `hotspot` in
/// physical pixels.
fn scaled(
    renderer: &mut smithay::backend::renderer::gles::GlesRenderer,
    frame: &crate::cursor::Frame,
    hotspot: (f64, f64),
    height: f64,
    alpha: f32,
) -> Option<crate::render::OutputElement> {
    use smithay::{
        backend::renderer::element::{Kind, memory::MemoryRenderBufferRenderElement},
        utils::Rectangle,
    };
    let factor = height / f64::from(frame.height.max(1));
    let size = ((f64::from(frame.width) * factor).round() as i32, height.round() as i32);
    let location = (
        (hotspot.0 - f64::from(frame.hotspot.x) * factor).round(),
        (hotspot.1 - f64::from(frame.hotspot.y) * factor).round(),
    );
    let source = Rectangle::from_size((f64::from(frame.width), f64::from(frame.height)).into());
    // Not `Kind::Cursor`: an enlarged arrow does not fit a cursor plane.
    MemoryRenderBufferRenderElement::from_buffer(
        renderer,
        location,
        &frame.buffer,
        Some(alpha),
        Some(source),
        Some(size.into()),
        Kind::Unspecified,
    )
    .ok()
    .map(crate::render::OutputElement::Memory)
}

/// A fixed number from 0 to 1 for `index`, so every burst looks the same.
fn scatter(index: usize) -> f64 {
    let mut value = (index as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);
    value ^= value >> 29;
    value = value.wrapping_mul(0xBF58_476D_1CE4_E5B9);
    value ^= value >> 32;
    (value % 10_000) as f64 / 10_000.0
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Moves the pointer back and forth `strokes` times, `step` pixels per
    /// event and `per_stroke` events per stroke, one event every 8 ms.
    fn shake(shake: &mut Shake, start: Instant, strokes: usize, per_stroke: usize, step: f64) -> Instant {
        let mut now = start;
        for stroke in 0..strokes {
            let sign = if stroke % 2 == 0 { 1.0 } else { -1.0 };
            for _ in 0..per_stroke {
                now += Duration::from_millis(8);
                shake.motion(sign * step, 0.0, now);
            }
        }
        now
    }

    #[test]
    fn shaking_makes_the_pointer_grow_without_limit() {
        let mut tracker = Shake::default();
        let start = Instant::now();
        let now = shake(&mut tracker, start, 6, 10, 10.0);
        let grown = tracker.scale(now);
        assert!(grown > 1.5, "grew to {grown}");
        let now = shake(&mut tracker, now, 60, 10, 10.0);
        assert!(tracker.scale(now) > 10.0, "kept growing to {}", tracker.scale(now));
    }

    #[test]
    fn moving_straight_or_slowly_is_not_a_shake() {
        let mut tracker = Shake::default();
        let start = Instant::now();
        let now = shake(&mut tracker, start, 1, 200, 10.0);
        assert_eq!(tracker.scale(now), 1.0);
        // Back and forth, but slowly.
        let now = shake(&mut tracker, now, 10, 40, 1.0);
        assert_eq!(tracker.scale(now), 1.0);
        // Back and forth quickly, but only a few pixels.
        let now = shake(&mut tracker, now, 10, 1, 4.0);
        assert_eq!(tracker.scale(now), 1.0);
    }

    #[test]
    fn the_pointer_shrinks_back_after_shaking() {
        let mut tracker = Shake::default();
        let now = shake(&mut tracker, Instant::now(), 8, 10, 10.0);
        let grown = tracker.scale(now);
        assert_eq!(tracker.scale(now + HOLD / 2), grown);
        assert!(tracker.scale(now + HOLD + HALF_LIFE) < grown);
        assert!(tracker.scale(now + Duration::from_secs(3)) < 1.01);
        assert!(!tracker.active(now + Duration::from_secs(3)));
    }

    #[test]
    fn a_burst_resets_the_size_and_fades() {
        let mut tracker = Shake::default();
        let now = shake(&mut tracker, Instant::now(), 8, 10, 10.0);
        tracker.burst(100.0, 100.0, now);
        assert_eq!(tracker.scale(now), 1.0);
        let pieces = tracker.pieces(24.0, now + Duration::from_millis(300));
        assert_eq!(pieces.len(), PIECES);
        assert!(pieces.iter().any(|piece| piece.left < 100.0) && pieces.iter().any(|piece| piece.left > 100.0));
        assert!(tracker.active(now + BURST / 2));
        assert!(tracker.pieces(24.0, now + BURST).is_empty());
    }
}
