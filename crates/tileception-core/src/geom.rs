//! Plain floating point geometry shared by the engine and the compositor.

#[derive(Debug, Clone, Copy, PartialEq, Default)]
#[allow(clippy::min_ident_chars, reason = "preserve conventional public coordinate fields")]
pub struct Point {
    pub x: f64,
    pub y: f64,
}

impl Point {
    pub const fn new(x_position: f64, y_position: f64) -> Self {
        Self { x: x_position, y: y_position }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Default, serde::Serialize, serde::Deserialize)]
#[allow(clippy::min_ident_chars, reason = "preserve public and serialized geometry field names")]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl Rect {
    pub const UNIT: Rect = Rect::new(0.0, 0.0, 1.0, 1.0);

    pub const fn new(x_position: f64, y_position: f64, width: f64, height: f64) -> Self {
        Self { x: x_position, y: y_position, w: width, h: height }
    }

    pub fn right(&self) -> f64 {
        self.x + self.w
    }

    pub fn bottom(&self) -> f64 {
        self.y + self.h
    }

    pub fn center(&self) -> Point {
        Point::new(self.x + self.w / 2.0, self.y + self.h / 2.0)
    }

    pub fn contains(&self, point: Point) -> bool {
        point.x >= self.x && point.x < self.right() && point.y >= self.y && point.y < self.bottom()
    }

    /// True when `other` lies entirely inside `self`, with a small tolerance for
    /// floating point drift.
    pub fn contains_rect(&self, other: &Rect) -> bool {
        const EPS: f64 = 1e-9;
        other.x >= self.x - EPS
            && other.y >= self.y - EPS
            && other.right() <= self.right() + EPS
            && other.bottom() <= self.bottom() + EPS
    }

    pub fn intersects(&self, other: &Rect) -> bool {
        self.x < other.right() && other.x < self.right() && self.y < other.bottom() && other.y < self.bottom()
    }

    /// Shrinks the rectangle by `distance` on every side, never below zero size.
    pub fn inset(&self, distance: f64) -> Rect {
        let width = (self.w - 2.0 * distance).max(0.0);
        let height = (self.h - 2.0 * distance).max(0.0);
        Rect::new(self.x + (self.w - width) / 2.0, self.y + (self.h - height) / 2.0, width, height)
    }

    /// Linear interpolation of position, geometric interpolation of size, so a
    /// zoom looks uniform in speed instead of rushing at the small end.
    pub fn zoom_lerp(&self, target: &Rect, fraction: f64) -> Rect {
        let lerp = |start: f64, end: f64| start + (end - start) * fraction;
        let glerp = |start: f64, end: f64| {
            if start > 0.0 && end > 0.0 {
                (start.ln() + (end.ln() - start.ln()) * fraction).exp()
            } else {
                lerp(start, end)
            }
        };
        let width = glerp(self.w, target.w);
        let height = glerp(self.h, target.h);
        let start_center = self.center();
        let end_center = target.center();
        // Moving the centre with the same weighting as the size, per axis, keeps
        // shared edges fixed and the point being zoomed toward steady on screen.
        let progress = |size: f64, from: f64, target_size: f64| {
            if (target_size - from).abs() > 1e-12 {
                ((size - from) / (target_size - from)).clamp(0.0, 1.0)
            } else {
                fraction
            }
        };
        let center_x = start_center.x + (end_center.x - start_center.x) * progress(width, self.w, target.w);
        let center_y = start_center.y + (end_center.y - start_center.y) * progress(height, self.h, target.h);
        Rect::new(center_x - width / 2.0, center_y - height / 2.0, width, height)
    }

    pub fn approx_eq(&self, other: &Rect, eps: f64) -> bool {
        (self.x - other.x).abs() < eps
            && (self.y - other.y).abs() < eps
            && (self.w - other.w).abs() < eps
            && (self.h - other.h).abs() < eps
    }
}

/// Affine map without rotation: `screen = world * s + t`, per axis.
///
/// The camera maps the unit square layout onto the output. Scale can differ per
/// axis because framing a tall tile stretches it to fill a wide screen; real
/// windows are then reconfigured to the stretched size, so nothing looks
/// distorted once the camera settles.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Xform {
    pub scale_x: f64,
    pub scale_y: f64,
    pub offset_x: f64,
    pub offset_y: f64,
}

impl Xform {
    /// The map that shows `view` (in world units) exactly filling `screen`.
    pub fn view_to_screen(view: &Rect, screen: &Rect) -> Xform {
        let scale_x = screen.w / view.w.max(1e-12);
        let scale_y = screen.h / view.h.max(1e-12);
        Xform { scale_x, scale_y, offset_x: screen.x - view.x * scale_x, offset_y: screen.y - view.y * scale_y }
    }

    pub fn apply(&self, rect: &Rect) -> Rect {
        Rect::new(
            rect.x * self.scale_x + self.offset_x,
            rect.y * self.scale_y + self.offset_y,
            rect.w * self.scale_x,
            rect.h * self.scale_y,
        )
    }

    pub fn apply_point(&self, point: Point) -> Point {
        Point::new(point.x * self.scale_x + self.offset_x, point.y * self.scale_y + self.offset_y)
    }

    pub fn invert_point(&self, point: Point) -> Point {
        Point::new((point.x - self.offset_x) / self.scale_x, (point.y - self.offset_y) / self.scale_y)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn view_maps_onto_screen() {
        let view = Rect::new(0.5, 0.0, 0.5, 1.0);
        let screen = Rect::new(0.0, 0.0, 1920.0, 1080.0);
        let transform = Xform::view_to_screen(&view, &screen);
        assert!(transform.apply(&view).approx_eq(&screen, 1e-9));
        let point = Point::new(100.0, 200.0);
        let back = transform.apply_point(transform.invert_point(point));
        assert!((back.x - point.x).abs() < 1e-9 && (back.y - point.y).abs() < 1e-9);
    }

    #[test]
    fn zoom_lerp_hits_endpoints() {
        let start = Rect::new(0.0, 0.0, 1.0, 1.0);
        let end = Rect::new(0.25, 0.5, 0.25, 0.5);
        assert!(start.zoom_lerp(&end, 0.0).approx_eq(&start, 1e-9));
        assert!(start.zoom_lerp(&end, 1.0).approx_eq(&end, 1e-9));
        let mid = start.zoom_lerp(&end, 0.5);
        assert!(start.contains_rect(&mid));
    }
}
