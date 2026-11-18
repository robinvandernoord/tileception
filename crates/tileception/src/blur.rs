//! Blur behind windows that ask for it: a terminal with a translucent
//! background shows what is under it frosted, as Tileception's own panels
//! do. Only while `[theme] frost` is on; a translucent window without it
//! shows what is behind it sharp.
//!
//! Apps ask through one of two protocols. `ext_background_effect_v1` is the
//! standard one (foot, kitty, Quickshell); KDE's older `org_kde_kwin_blur`
//! is what Ghostty, Konsole and Qt apps use. Both name a region of a surface
//! that takes effect on the surface's next commit. Here that is
//! [`BlurState`], kept in the surface's double-buffered state and read as
//! windows are drawn. `frost.rs` draws the blur itself.

use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

use smithay::{
    backend::renderer::{element::Element, utils::with_renderer_surface_state},
    reexports::wayland_server::{
        Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource, Weak,
        backend::{ClientId, ObjectId},
        protocol::{
            wl_region::{self, WlRegion},
            wl_surface::WlSurface,
        },
    },
    utils::{Logical, Physical, Point, Rectangle, Scale, Size},
    wayland::compositor::{self, Cacheable, CompositorState, RectangleKind, RegionAttributes, RegionUserData},
};
use wayland_protocols::ext::background_effect::v1::server::{
    ext_background_effect_manager_v1::{self, ExtBackgroundEffectManagerV1},
    ext_background_effect_surface_v1::{self, ExtBackgroundEffectSurfaceV1},
};
use wayland_protocols_plasma::blur::server::{
    org_kde_kwin_blur::{self, OrgKdeKwinBlur},
    org_kde_kwin_blur_manager::{self, OrgKdeKwinBlurManager},
};

use crate::state::Tileception;

/// Reject over-budget masks in full so subtract operations are never ignored.
const MAX_REGION_OPS: usize = 128;
pub(crate) const MAX_BLUR_RECTS: usize = 128;

type LogicalMask = Arc<[Rectangle<i32, Logical>]>;

#[derive(Debug)]
struct RegionMask {
    region: RegionAttributes,
    normalized: Mutex<Option<(Size<i32, Logical>, LogicalMask)>>,
}

/// What a surface asked to have blurred behind it.
#[derive(Debug, Clone, Default)]
enum Blur {
    #[default]
    None,
    /// All of it: KDE's protocol without a region.
    Whole,
    /// Surface-local, as the client gave it.
    Region(Arc<RegionMask>),
}

impl Blur {
    fn from_region(state: &Tileception, region: Option<&WlRegion>, none: Blur) -> Blur {
        match region {
            Some(region)
                if state.blur_managers.region_ops.get(&region.id()).copied().unwrap_or(0) <= MAX_REGION_OPS =>
            {
                Blur::Region(Arc::new(RegionMask {
                    region: compositor::get_region_attributes(region),
                    normalized: Mutex::default(),
                }))
            }
            Some(_) => Blur::None,
            None => none,
        }
    }

    /// The requested mask as non-overlapping rectangles in surface-local pixels.
    fn rectangles(&self, size: Size<i32, Logical>) -> LogicalMask {
        let whole = Rectangle::from_size(size);
        match self {
            Blur::Whole if !whole.is_empty() => Arc::new([whole]),
            Blur::Region(region) if !whole.is_empty() => {
                let Ok(mut normalized) = region.normalized.lock() else { return Arc::new([]) };
                if let Some((cached_size, mask)) = &*normalized
                    && *cached_size == size
                {
                    return mask.clone();
                }
                let mask = normalize(&region.region, whole).unwrap_or_default().into();
                *normalized = Some((size, Arc::clone(&mask)));
                mask
            }
            _ => Arc::new([]),
        }
    }
}

fn normalize(region: &RegionAttributes, whole: Rectangle<i32, Logical>) -> Option<Vec<Rectangle<i32, Logical>>> {
    let mut mask = Vec::new();
    for &(kind, rect) in &region.rects {
        if rect.size.w <= 0 || rect.size.h <= 0 {
            continue;
        }
        let Some(rect) = rect.intersection(whole) else { continue };
        match kind {
            RectangleKind::Add => {
                let mut uncovered = vec![rect];
                for &covered in &mask {
                    uncovered = subtract_blur_rect(uncovered, covered)?;
                    if uncovered.is_empty() {
                        break;
                    }
                }
                if mask.len() + uncovered.len() > MAX_BLUR_RECTS {
                    return None;
                }
                mask.extend(uncovered);
            }
            RectangleKind::Subtract => mask = subtract_blur_rect(mask, rect)?,
        }
    }
    Some(mask)
}

/// One subtraction can split a rectangle into at most four pieces.
pub(crate) fn subtract_blur_rect<Kind>(
    rects: Vec<Rectangle<i32, Kind>>,
    cut: Rectangle<i32, Kind>,
) -> Option<Vec<Rectangle<i32, Kind>>> {
    let rects = Rectangle::subtract_rects_many_in_place(rects, [cut]);
    (rects.len() <= MAX_BLUR_RECTS).then_some(rects)
}

/// A surface's blur, double-buffered: requests change the pending one and
/// the surface's commit makes it current.
#[derive(Debug, Clone, Default)]
struct BlurState(Blur);

impl Cacheable for BlurState {
    fn commit(&mut self, _display: &DisplayHandle) -> Self {
        self.clone()
    }

    fn merge_into(self, into: &mut Self, _display: &DisplayHandle) {
        *into = self;
    }
}

/// Marks a surface that has an `ext_background_effect_surface_v1`, which
/// the protocol allows only one of.
#[derive(Debug, Default)]
struct HasEffect(AtomicBool);

/// The blur a surface last committed.
fn blur_of(surface: &WlSurface) -> Blur {
    compositor::with_states(surface, |states| states.cached_state.get::<BlurState>().current().0.clone())
}

fn set_pending(surface: &WlSurface, blur: Blur) {
    compositor::with_states(surface, |states| states.cached_state.get::<BlurState>().pending().0 = blur);
}

impl Tileception {
    /// Asks frost for blur behind a drawn window, if it wants it.
    pub(crate) fn blur_window(
        &self,
        window: &smithay::desktop::Window,
        drawn: &crate::render::Drawn,
        area: Option<tileception_core::Rect>,
        elements: &[crate::render::OutputElement],
    ) {
        // X11 windows cannot ask.
        let Some(toplevel) = window.toplevel() else { return };
        // Only the window itself: a region that takes in the surface's
        // client-side shadow would blur a square around the window.
        let clip = match area {
            None => drawn.content,
            Some(area) => {
                let Some(clip) = crate::render::intersection(&area, &drawn.content) else { return };
                clip
            }
        };
        let outline = crate::render::physical(&drawn.content, self.scale);
        self.blur_surface(
            toplevel.wl_surface(),
            crate::render::physical_point((drawn.origin.x, drawn.origin.y), self.scale),
            Scale::from((self.scale * drawn.scale.0, self.scale * drawn.scale.1)),
            Some(crate::render::physical(&clip, self.scale)),
            Some((outline, self.theme.radius * self.scale as f32)),
            elements,
        );
    }

    /// Registers blur under a rendered surface tree using the same origin and scale as its elements.
    pub(crate) fn blur_surface(
        &self,
        surface: &WlSurface,
        origin: Point<i32, Physical>,
        scale: Scale<f64>,
        clip: Option<Rectangle<i32, Physical>>,
        outline: Option<(Rectangle<i32, Physical>, f32)>,
        elements: &[crate::render::OutputElement],
    ) {
        if !self.frost_on() {
            return;
        }
        let Some(lowest) = elements.last() else { return };
        let blur = blur_of(surface);
        if matches!(blur, Blur::None) {
            return;
        }
        let Some(size) = with_renderer_surface_state(surface, |state| state.surface_size()).flatten() else { return };
        let mut rects: Vec<_> = blur
            .rectangles(size)
            .iter()
            .filter_map(|rect| {
                let start = origin + rect.loc.to_physical_precise_round(scale);
                let end = origin + (rect.loc + rect.size).to_physical_precise_round(scale);
                let rect = Rectangle::from_extremities(start, end);
                let rect = match clip {
                    Some(clip) => rect.intersection(clip)?,
                    None => rect,
                };
                (!rect.is_empty()).then_some(rect)
            })
            .collect();
        let physical = Scale::from(1.0);
        for element in elements {
            if rects.is_empty() {
                return;
            }
            let origin = element.geometry(physical).loc;
            for mut rect in element.opaque_regions(physical) {
                rect.loc += origin;
                let Some(remaining) = subtract_blur_rect(rects, rect) else { return };
                rects = remaining;
                if rects.is_empty() {
                    return;
                }
            }
        }
        if !rects.is_empty() {
            self.frost.borrow_mut().surface(lowest.id().clone(), rects, outline);
        }
    }
}

/// The bound background effect managers, told when blur comes and goes.
#[derive(Debug, Default)]
pub struct Managers {
    bound: Vec<ExtBackgroundEffectManagerV1>,
    region_ops: HashMap<ObjectId, usize>,
}

impl Dispatch<WlRegion, RegionUserData> for Tileception {
    fn request(
        state: &mut Self,
        client: &Client,
        region: &WlRegion,
        request: wl_region::Request,
        data: &RegionUserData,
        handle: &DisplayHandle,
        init: &mut DataInit<'_, Self>,
    ) {
        if matches!(request, wl_region::Request::Add { .. } | wl_region::Request::Subtract { .. }) {
            // Smithay exposes a clone of RegionAttributes. Check the size before
            // taking that copy, while leaving the region available for other uses.
            let count = state.blur_managers.region_ops.entry(region.id()).or_default();
            *count = (*count + 1).min(MAX_REGION_OPS + 1);
        }
        <CompositorState as Dispatch<WlRegion, RegionUserData, Self>>::request(
            state, client, region, request, data, handle, init,
        );
    }

    fn destroyed(state: &mut Self, _client: ClientId, region: &WlRegion, _data: &RegionUserData) {
        state.blur_managers.region_ops.remove(&region.id());
    }
}

pub fn init_blur(display_handle: &DisplayHandle) {
    display_handle.create_global::<Tileception, ExtBackgroundEffectManagerV1, _>(1, ());
    display_handle.create_global::<Tileception, OrgKdeKwinBlurManager, _>(1, ());
}

impl Tileception {
    fn blur_capabilities(&self) -> ext_background_effect_manager_v1::Capability {
        if self.frost_on() {
            ext_background_effect_manager_v1::Capability::Blur
        } else {
            ext_background_effect_manager_v1::Capability::empty()
        }
    }

    /// Tells the apps whether the theme and renderer both support blur.
    /// KDE's protocol has no way to say so.
    pub fn announce_blur(&mut self) {
        let capabilities = self.blur_capabilities();
        self.blur_managers.bound.retain(|manager| manager.is_alive());
        for manager in &self.blur_managers.bound {
            manager.capabilities(capabilities);
        }
    }
}

impl GlobalDispatch<ExtBackgroundEffectManagerV1, ()> for Tileception {
    fn bind(
        state: &mut Self,
        _handle: &DisplayHandle,
        _client: &Client,
        resource: New<ExtBackgroundEffectManagerV1>,
        _global_data: &(),
        data_init: &mut DataInit<'_, Self>,
    ) {
        let manager = data_init.init(resource, ());
        manager.capabilities(state.blur_capabilities());
        state.blur_managers.bound.push(manager);
    }
}

impl Dispatch<ExtBackgroundEffectManagerV1, ()> for Tileception {
    fn request(
        _state: &mut Self,
        _client: &Client,
        manager: &ExtBackgroundEffectManagerV1,
        request: ext_background_effect_manager_v1::Request,
        _data: &(),
        _handle: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        let ext_background_effect_manager_v1::Request::GetBackgroundEffect { id: made, surface } = request else {
            return;
        };
        let taken = compositor::with_states(&surface, |states| {
            states.data_map.insert_if_missing_threadsafe(HasEffect::default);
            states.data_map.get::<HasEffect>().is_some_and(|has| has.0.swap(true, Ordering::Relaxed))
        });
        if taken {
            manager.post_error(
                ext_background_effect_manager_v1::Error::BackgroundEffectExists,
                "the surface already has a background effect object",
            );
            return;
        }
        data_init.init(made, surface.downgrade());
    }

    fn destroyed(state: &mut Self, _client: ClientId, manager: &ExtBackgroundEffectManagerV1, _data: &()) {
        state.blur_managers.bound.retain(|kept| kept != manager);
    }
}

impl Dispatch<ExtBackgroundEffectSurfaceV1, Weak<WlSurface>> for Tileception {
    fn request(
        state: &mut Self,
        _client: &Client,
        effect: &ExtBackgroundEffectSurfaceV1,
        request: ext_background_effect_surface_v1::Request,
        surface: &Weak<WlSurface>,
        _handle: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        match request {
            ext_background_effect_surface_v1::Request::SetBlurRegion { region } => {
                let Ok(surface) = surface.upgrade() else {
                    effect.post_error(
                        ext_background_effect_surface_v1::Error::SurfaceDestroyed,
                        "the surface has been destroyed",
                    );
                    return;
                };
                set_pending(&surface, Blur::from_region(state, region.as_ref(), Blur::None));
            }
            ext_background_effect_surface_v1::Request::Destroy => {}
            _ => {}
        }
    }

    fn destroyed(
        _state: &mut Self,
        _client: ClientId,
        _effect: &ExtBackgroundEffectSurfaceV1,
        surface: &Weak<WlSurface>,
    ) {
        // The region goes on the surface's next commit.
        if let Ok(surface) = surface.upgrade() {
            set_pending(&surface, Blur::None);
            compositor::with_states(&surface, |states| {
                if let Some(has) = states.data_map.get::<HasEffect>() {
                    has.0.store(false, Ordering::Relaxed);
                }
            });
        }
    }
}

impl GlobalDispatch<OrgKdeKwinBlurManager, ()> for Tileception {
    fn bind(
        _state: &mut Self,
        _handle: &DisplayHandle,
        _client: &Client,
        resource: New<OrgKdeKwinBlurManager>,
        _global_data: &(),
        data_init: &mut DataInit<'_, Self>,
    ) {
        data_init.init(resource, ());
    }
}

impl Dispatch<OrgKdeKwinBlurManager, ()> for Tileception {
    fn request(
        _state: &mut Self,
        _client: &Client,
        _manager: &OrgKdeKwinBlurManager,
        request: org_kde_kwin_blur_manager::Request,
        _data: &(),
        _handle: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        match request {
            org_kde_kwin_blur_manager::Request::Create { id: made, surface } => {
                // Without a region, KWin blurs the whole surface.
                data_init
                    .init(made, KdeBlur { surface: surface.downgrade(), pending: std::sync::Mutex::new(Blur::Whole) });
            }
            org_kde_kwin_blur_manager::Request::Unset { surface } => set_pending(&surface, Blur::None),
            _ => {}
        }
    }
}

/// A KDE blur object: its region waits on the object until its own commit,
/// then on the surface until the surface's.
#[derive(Debug)]
pub struct KdeBlur {
    surface: Weak<WlSurface>,
    pending: std::sync::Mutex<Blur>,
}

impl Dispatch<OrgKdeKwinBlur, KdeBlur> for Tileception {
    fn request(
        state: &mut Self,
        _client: &Client,
        _blur: &OrgKdeKwinBlur,
        request: org_kde_kwin_blur::Request,
        data: &KdeBlur,
        _handle: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        let Ok(mut pending) = data.pending.lock() else { return };
        match request {
            org_kde_kwin_blur::Request::SetRegion { region } => {
                *pending = Blur::from_region(state, region.as_ref(), Blur::Whole);
            }
            org_kde_kwin_blur::Request::Commit => {
                if let Ok(surface) = data.surface.upgrade() {
                    set_pending(&surface, pending.clone());
                }
            }
            // Releasing the object keeps the blur, as in KWin; `unset` ends it.
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// (left, top, width, height)
    type Edges = (i32, i32, i32, i32);

    fn region(rects: &[(RectangleKind, Edges)]) -> Blur {
        Blur::Region(Arc::new(RegionMask {
            region: RegionAttributes {
                rects: rects
                    .iter()
                    .map(|(kind, (left, top, width, height))| {
                        (*kind, Rectangle::new((*left, *top).into(), (*width, *height).into()))
                    })
                    .collect(),
            },
            normalized: Mutex::default(),
        }))
    }

    fn rectangles(blur: &Blur) -> Vec<Edges> {
        blur.rectangles((800, 600).into())
            .iter()
            .map(|rect| (rect.loc.x, rect.loc.y, rect.size.w, rect.size.h))
            .collect()
    }

    #[test]
    fn nothing_and_the_whole_surface() {
        assert!(rectangles(&Blur::None).is_empty());
        assert_eq!(rectangles(&Blur::Whole), [(0, 0, 800, 600)]);
    }

    #[test]
    fn a_region_is_cut_to_the_surface() {
        // Ghostty's region: the window without its client-side shadow.
        assert_eq!(rectangles(&region(&[(RectangleKind::Add, (12, 10, 776, 580))])), [(12, 10, 776, 580)]);
        assert_eq!(rectangles(&region(&[(RectangleKind::Add, (-50, -50, 2000, 2000))])), [(0, 0, 800, 600)]);
        assert!(rectangles(&region(&[(RectangleKind::Add, (900, 0, 10, 10))])).is_empty());
    }

    #[test]
    fn subtraction_can_empty_the_region() {
        let blur = region(&[
            (RectangleKind::Add, (0, 0, 100, 40)),
            (RectangleKind::Add, (0, 40, 800, 560)),
            (RectangleKind::Subtract, (0, 0, 800, 600)),
        ]);
        assert!(rectangles(&blur).is_empty());
        assert!(rectangles(&region(&[(RectangleKind::Subtract, (0, 0, 10, 10))])).is_empty());
    }
}
