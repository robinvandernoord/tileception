//! Plain floating point geometry shared by the engine and the compositor.

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Point {
    pub x: f64,
    pub y: f64,
}

impl Point {
    pub const fn new(x: f64, y: f64) -> Self {
        Self { x, y }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Default, serde::Serialize, serde::Deserialize)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl Rect {
    pub const UNIT: Rect = Rect::new(0.0, 0.0, 1.0, 1.0);

    pub const fn new(x: f64, y: f64, w: f64, h: f64) -> Self {
        Self { x, y, w, h }
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

    pub fn contains(&self, p: Point) -> bool {
        p.x >= self.x && p.x < self.right() && p.y >= self.y && p.y < self.bottom()
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

    /// Shrinks the rectangle by `d` on every side, never below zero size.
    pub fn inset(&self, d: f64) -> Rect {
        let w = (self.w - 2.0 * d).max(0.0);
        let h = (self.h - 2.0 * d).max(0.0);
        Rect::new(self.x + (self.w - w) / 2.0, self.y + (self.h - h) / 2.0, w, h)
    }

    /// Linear interpolation of position, geometric interpolation of size, so a
    /// zoom looks uniform in speed instead of rushing at the small end.
    pub fn zoom_lerp(&self, to: &Rect, t: f64) -> Rect {
        let lerp = |a: f64, b: f64| a + (b - a) * t;
        let glerp = |a: f64, b: f64| {
            if a > 0.0 && b > 0.0 {
                (a.ln() + (b.ln() - a.ln()) * t).exp()
            } else {
                lerp(a, b)
            }
        };
        let w = glerp(self.w, to.w);
        let h = glerp(self.h, to.h);
        let c0 = self.center();
        let c1 = to.center();
        // Moving the centre with the same weighting as the size, per axis, keeps
        // shared edges fixed and the point being zoomed toward steady on screen.
        let progress = |size: f64, from: f64, to: f64| {
            if (to - from).abs() > 1e-12 {
                ((size - from) / (to - from)).clamp(0.0, 1.0)
            } else {
                t
            }
        };
        let cx = c0.x + (c1.x - c0.x) * progress(w, self.w, to.w);
        let cy = c0.y + (c1.y - c0.y) * progress(h, self.h, to.h);
        Rect::new(cx - w / 2.0, cy - h / 2.0, w, h)
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
    pub sx: f64,
    pub sy: f64,
    pub tx: f64,
    pub ty: f64,
}

impl Xform {
    /// The map that shows `view` (in world units) exactly filling `screen`.
    pub fn view_to_screen(view: &Rect, screen: &Rect) -> Xform {
        let sx = screen.w / view.w.max(1e-12);
        let sy = screen.h / view.h.max(1e-12);
        Xform { sx, sy, tx: screen.x - view.x * sx, ty: screen.y - view.y * sy }
    }

    pub fn apply(&self, r: &Rect) -> Rect {
        Rect::new(r.x * self.sx + self.tx, r.y * self.sy + self.ty, r.w * self.sx, r.h * self.sy)
    }

    pub fn apply_point(&self, p: Point) -> Point {
        Point::new(p.x * self.sx + self.tx, p.y * self.sy + self.ty)
    }

    pub fn invert_point(&self, p: Point) -> Point {
        Point::new((p.x - self.tx) / self.sx, (p.y - self.ty) / self.sy)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn view_maps_onto_screen() {
        let view = Rect::new(0.5, 0.0, 0.5, 1.0);
        let screen = Rect::new(0.0, 0.0, 1920.0, 1080.0);
        let x = Xform::view_to_screen(&view, &screen);
        assert!(x.apply(&view).approx_eq(&screen, 1e-9));
        let p = Point::new(100.0, 200.0);
        let back = x.apply_point(x.invert_point(p));
        assert!((back.x - p.x).abs() < 1e-9 && (back.y - p.y).abs() < 1e-9);
    }

    #[test]
    fn zoom_lerp_hits_endpoints() {
        let a = Rect::new(0.0, 0.0, 1.0, 1.0);
        let b = Rect::new(0.25, 0.5, 0.25, 0.5);
        assert!(a.zoom_lerp(&b, 0.0).approx_eq(&a, 1e-9));
        assert!(a.zoom_lerp(&b, 1.0).approx_eq(&b, 1e-9));
        let mid = a.zoom_lerp(&b, 0.5);
        assert!(a.contains_rect(&mid));
    }
}
