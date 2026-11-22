//! Screen capture: wlr-screencopy for grim, slurp and screen sharing through
//! xdg-desktop-portal-wlr before 0.8, the screens shared through
//! ext-image-copy-capture ([`crate::image_copy`]), and
//! `tileception msg screenshot <file.png>`.
//!
//! Captures are queued and taken on the next frame. Shared memory buffers and
//! PNG files are filled by rendering the scene into an off-screen texture and
//! reading it back; GPU buffers (dmabufs, which screen sharing uses) are
//! rendered into directly.

use std::{
    cell::Cell,
    collections::{HashMap, VecDeque},
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};

use smithay::{
    backend::{
        allocator::{Buffer as _, Fourcc, dmabuf::Dmabuf},
        renderer::{
            Bind, Color32F, ExportMem, Offscreen,
            damage::OutputDamageTracker,
            element::{
                RenderElement,
                utils::{Relocate, RelocateRenderElement},
            },
            gles::{GlesRenderer, GlesTexture},
        },
    },
    reexports::{
        wayland_protocols_wlr::screencopy::v1::server::{
            zwlr_screencopy_frame_v1::{self, ZwlrScreencopyFrameV1},
            zwlr_screencopy_manager_v1::{self, ZwlrScreencopyManagerV1},
        },
        wayland_server::{
            Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource,
            protocol::{wl_buffer::WlBuffer, wl_shm},
        },
    },
    utils::{Buffer, Clock, Monotonic, Rectangle, Transform},
    wayland::{
        dmabuf::get_dmabuf,
        shm::{with_buffer_contents, with_buffer_contents_mut},
    },
};

use crate::{Tileception, render::OutputElement};

/// Where a finished capture goes.
pub enum CaptureTarget {
    File(PathBuf),
    /// A copy into a buffer of a screen sharing or screenshot program.
    Copy(ClientCopy),
    /// The whole output without the rubber duck, for its question.
    Duck,
    /// The whole output, for the colour picker's magnifier.
    ColourPick,
    /// The whole output without the area picker, for the area screenshot.
    Shot,
    /// The recorded area, with the pointer, for the next frame of a recording.
    Recording,
}

/// What is on screen, ready to be rendered into each capture's buffer.
struct Scene<E = OutputElement> {
    elements: Vec<E>,
    size: (i32, i32),
    clear: [f32; 4],
}

/// The Wizard policy for a feedback still, and whether it was drawn into it.
pub(crate) struct FeedbackCapture {
    include_wizard: bool,
    drew_wizard: Cell<bool>,
}

impl FeedbackCapture {
    pub(crate) fn draw_wizard(&self) -> bool {
        if self.include_wizard {
            self.drew_wizard.set(true);
        }
        self.include_wizard
    }
}

pub struct PendingCapture {
    /// Part of the output, in pixels.
    pub region: Rectangle<i32, Buffer>,
    pub target: CaptureTarget,
}

/// A program's buffer to copy a screen into, through wlr-screencopy or
/// ext-image-copy-capture.
pub struct ClientCopy {
    pub frame: CopyFrame,
    pub buffer: ClientBuffer,
    /// Wait until the screen changes before copying (wlr's
    /// `copy_with_damage`; every ext-image-copy-capture frame).
    pub wait_for_damage: bool,
    /// With the pointer drawn in.
    pub cursor: bool,
    /// What the program was last sent, to tell it what changed since.
    pub history: ManagerDamage,
    pub output: smithay::output::WeakOutput,
}

/// A buffer a program gave to be filled.
pub enum ClientBuffer {
    Shm(WlBuffer),
    /// A GPU buffer, rendered into directly.
    Dmabuf(Dmabuf),
}

impl ClientBuffer {
    /// The buffer behind a `wl_buffer`, or nothing if it is neither kind.
    pub fn from_wl(buffer: WlBuffer) -> Option<Self> {
        if let Ok(dmabuf) = get_dmabuf(&buffer) {
            Some(Self::Dmabuf(dmabuf.clone()))
        } else if with_buffer_contents(&buffer, |_, _, _| ()).is_ok() {
            Some(Self::Shm(buffer))
        } else {
            None
        }
    }

    /// Its size in pixels.
    pub fn size(&self) -> Option<(i32, i32)> {
        match self {
            Self::Shm(buffer) => with_buffer_contents(buffer, |_, _, info| (info.width, info.height)).ok(),
            Self::Dmabuf(dmabuf) => {
                let size = dmabuf.size();
                Some((size.w, size.h))
            }
        }
    }

    /// Fills the buffer with the scene, or with the part of it in `region`
    /// for shared memory; `texture` keeps the scene rendered for the next
    /// shared memory copy of the same frame.
    fn fill<E: RenderElement<GlesRenderer>>(
        &mut self,
        renderer: &mut GlesRenderer,
        texture: &mut Option<Result<GlesTexture, String>>,
        scene: &Scene<E>,
        region: Rectangle<i32, Buffer>,
    ) -> Result<(), String> {
        match self {
            Self::Shm(buffer) => pixels(renderer, texture, scene, region)
                .and_then(|rgba| copy_into_shm(buffer, region.size.w, region.size.h, &rgba)),
            Self::Dmabuf(dmabuf) => render_scene(renderer, dmabuf, scene),
        }
    }
}

/// Why a copy could not be made.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CopyFailure {
    Unknown,
    /// The buffer no longer fits what is captured, which has changed size.
    BufferConstraints,
    /// What is captured is gone.
    Stopped,
}

/// A frame a program asked for, in either protocol.
pub enum CopyFrame {
    Wlr(ZwlrScreencopyFrameV1),
    Ext(crate::image_copy::ExtFrame),
}

impl CopyFrame {
    pub(crate) fn is_alive(&self) -> bool {
        match self {
            Self::Wlr(frame) => frame.is_alive(),
            Self::Ext(frame) => frame.is_alive(),
        }
    }

    pub fn fail(&self, reason: CopyFailure) {
        match self {
            Self::Wlr(frame) => frame.failed(),
            Self::Ext(frame) => frame.fail(reason),
        }
    }

    /// Tells the program its buffer is filled, and what changed in it.
    pub(crate) fn ready(&self, damage: &[Rectangle<i32, Buffer>], size: (i32, i32)) {
        let now: Duration = Clock::<Monotonic>::new().now().into();
        match self {
            Self::Wlr(frame) => {
                frame.flags(zwlr_screencopy_frame_v1::Flags::empty());
                if let Some(rect) = bounding(damage) {
                    frame.damage(rect.loc.x as u32, rect.loc.y as u32, rect.size.w as u32, rect.size.h as u32);
                }
                let secs = now.as_secs();
                frame.ready((secs >> 32) as u32, secs as u32, now.subsec_nanos());
            }
            Self::Ext(frame) => frame.ready(bounding(damage), size, now),
        }
    }
}

/// What a screencopy frame was asked to capture.
pub struct FrameData {
    region: Rectangle<i32, Buffer>,
    output: smithay::output::Output,
    cursor: bool,
    damage: ManagerDamage,
}

// The protocol measures damage from the previous copy on this manager.
#[derive(Clone, Default)]
pub struct ManagerDamage(Arc<Mutex<VecDeque<DamageStream>>>);

#[derive(Clone, PartialEq, Eq)]
struct DamageKey {
    output: smithay::output::WeakOutput,
    cursor: bool,
    region: Rectangle<i32, Buffer>,
}

struct DamageStream {
    key: DamageKey,
    scene: OutputDamageTracker,
    pointer: Option<OutputDamageTracker>,
    clear: [f32; 4],
}

impl ManagerDamage {
    /// Whether this screen was copied for the program before: a stream
    /// rather than a single copy. Any region counts, so a stream whose
    /// size changed (a new resolution) is still one.
    fn streaming(&self, key: &DamageKey) -> bool {
        self.0.lock().expect("screencopy damage history").iter().any(|stream| stream.key.output == key.output)
    }

    fn changes(
        &self,
        key: &DamageKey,
        scene: &Scene<&OutputElement>,
        pointer: &[OutputElement],
    ) -> Vec<Rectangle<i32, Buffer>> {
        let mut streams = self.0.lock().expect("screencopy damage history");
        let tracker = || OutputDamageTracker::new((key.region.size.w, key.region.size.h), 1.0, Transform::Normal);
        let mut stream = streams
            .iter()
            .position(|stream| stream.key == *key)
            .and_then(|index| streams.remove(index))
            .unwrap_or_else(|| DamageStream {
                key: key.clone(),
                scene: tracker(),
                pointer: key.cursor.then(tracker),
                clear: scene.clear,
            });
        let age = usize::from(stream.clear == scene.clear);
        let mut damage = capture_damage(&mut stream.scene, age, key.region, scene.elements.iter().copied());
        // Cursor entry or exit must not change the scene elements' stacking indices.
        if let Some(tracker) = &mut stream.pointer {
            let cursor_damage = capture_damage(tracker, 1, key.region, pointer.iter());
            for rect in cursor_damage {
                damage.extend(rect.subtract_rects(damage.iter().copied()));
            }
        }
        stream.clear = scene.clear;
        streams.push_front(stream);
        // Bound history when one manager asks for many different regions.
        streams.truncate(16);
        damage
    }

    fn forget(&self, key: &DamageKey) {
        self.0.lock().expect("screencopy damage history").retain(|stream| stream.key != *key);
    }
}

fn capture_damage<'a>(
    tracker: &mut OutputDamageTracker,
    age: usize,
    region: Rectangle<i32, Buffer>,
    elements: impl Iterator<Item = &'a OutputElement>,
) -> Vec<Rectangle<i32, Buffer>> {
    let elements: Vec<_> = elements
        .map(|element| RelocateRenderElement::from_element(element, (-region.loc.x, -region.loc.y), Relocate::Relative))
        .collect();
    tracker
        .damage_output(age, &elements)
        .ok()
        .and_then(|(damage, _)| damage)
        .into_iter()
        .flatten()
        .map(|rect| Rectangle::new((rect.loc.x, rect.loc.y).into(), (rect.size.w, rect.size.h).into()))
        .collect()
}

pub fn init_screencopy(display_handle: &DisplayHandle) {
    display_handle.create_global::<Tileception, ZwlrScreencopyManagerV1, _>(3, ());
}

impl Tileception {
    pub fn fail_captures(&mut self) {
        let pending = std::mem::take(&mut self.pending_captures);
        for capture in pending {
            match capture.target {
                CaptureTarget::Copy(copy) => copy.frame.fail(CopyFailure::Stopped),
                CaptureTarget::File(path) => tracing::warn!("output disconnected before capture to {}", path.display()),
                CaptureTarget::Duck => {}
                CaptureTarget::ColourPick => self.cancel_colour_pick(),
                CaptureTarget::Shot => self.cancel_shot(),
                CaptureTarget::Recording => {}
            }
        }
    }

    /// The whole output, in pixels.
    fn output_region(&self) -> Rectangle<i32, Buffer> {
        Rectangle::from_size(self.output_pixels().into())
    }

    /// Queues a screenshot of the whole output into a PNG file.
    pub fn screenshot(&mut self, path: PathBuf) {
        let region = self.output_region();
        self.pending_captures.push(PendingCapture { region, target: CaptureTarget::File(path) });
    }

    /// Queues a screenshot of the whole output for the rubber duck.
    pub fn duck_screenshot(&mut self) {
        let region = self.output_region();
        self.pending_captures.push(PendingCapture { region, target: CaptureTarget::Duck });
        self.schedule_render();
    }

    /// What is on screen now, ready to render into a capture.
    fn scene(&mut self, renderer: &mut GlesRenderer, feedback: Option<&FeedbackCapture>) -> Scene {
        self.update_settings_client(renderer);
        self.update_updates_client(renderer);
        self.update_doctor_client(renderer);
        if self.draws_lock_screen() {
            self.wallpaper_buffer();
        }
        let wallpaper = if self.is_locked() { None } else { self.wallpaper_buffer() };
        let now = self.now();
        self.begin_frost(renderer);
        let others = self.other_area_elements(renderer, now, feedback);
        let (mut elements, under) = self.render_elements(renderer, now, others, feedback);
        if let Some(drawn) = wallpaper {
            crate::render::push_wallpaper(&mut elements, renderer, &drawn, (0.0, 0.0), self.scale);
        }
        self.add_frost(renderer, &mut elements, under);
        let elements = self.zoom_elements(renderer, elements);
        Scene { elements, size: self.output_pixels(), clear: self.clear_colour() }
    }

    /// The scene with the pointer on top, for captures that asked for it.
    fn scene_with_pointer(&mut self, renderer: &mut GlesRenderer) -> Scene {
        let mut elements = self.capture_pointer(renderer);
        let scene = self.scene(renderer, None);
        elements.extend(scene.elements);
        Scene { elements, ..scene }
    }

    fn capture_pointer(&mut self, renderer: &mut GlesRenderer) -> Vec<OutputElement> {
        if !self.is_active_output() {
            return Vec::new();
        }
        let mut cursor = self.capture_cursor.take().unwrap_or_else(|| crate::cursor::Cursor::load(self.scale));
        let elements = self.cursor_elements(renderer, &mut cursor);
        self.capture_cursor = Some(cursor);
        elements
    }

    /// Captures that temporarily hide shell elements must run before building the displayed scene.
    pub fn process_special_captures(&mut self, renderer: &mut GlesRenderer) {
        // The duck's own screenshot leaves the duck out, so it has a scene of its own.
        let duck = self.pending_captures.extract_if(.., |queued| matches!(queued.target, CaptureTarget::Duck)).last();
        if let Some(capture) = duck {
            self.duck.hidden = true;
            let scene = self.scene(renderer, None);
            self.duck.hidden = false;
            let region = capture.region;
            match pixels(renderer, &mut None, &scene, region) {
                Ok(rgba) => self.duck_screen_taken(rgba, (region.size.w as u32, region.size.h as u32)),
                Err(err) => tracing::warn!("duck capture failed: {err}"),
            }
        }
        // The area picker leaves itself out of its still.
        let shot = self.pending_captures.extract_if(.., |queued| matches!(queued.target, CaptureTarget::Shot)).last();
        if let Some(capture) = shot {
            let feedback = self.shot.as_ref().and_then(|shot| shot.feedback()).map(|feedback| FeedbackCapture {
                include_wizard: feedback.shows_doctor(),
                drew_wizard: Cell::new(false),
            });
            self.shot_hidden(true);
            let scene = self.scene(renderer, feedback.as_ref());
            self.shot_hidden(false);
            let region = capture.region;
            match pixels(renderer, &mut None, &scene, region) {
                Ok(rgba) => self.shot_still_taken(
                    rgba,
                    (region.size.w, region.size.h),
                    feedback.is_some_and(|capture| capture.drew_wizard.get()),
                ),
                Err(err) => {
                    tracing::warn!("area screenshot capture failed: {err}");
                    self.cancel_shot();
                }
            }
        }
        // A recording shows the pointer, but not its own Stop button.
        let frame =
            self.pending_captures.extract_if(.., |queued| matches!(queued.target, CaptureTarget::Recording)).last();
        if let Some(capture) = frame {
            self.recording_hidden(true);
            let scene = self.scene_with_pointer(renderer);
            self.recording_hidden(false);
            match pixels(renderer, &mut None, &scene, capture.region) {
                Ok(rgba) => self.recording_frame(rgba),
                Err(err) => tracing::warn!("recording capture failed: {err}"),
            }
        }
    }

    /// Shares the displayed scene for ordinary captures. Headless builds it on demand.
    pub fn process_captures(&mut self, renderer: &mut GlesRenderer, displayed: Option<&[OutputElement]>) {
        self.pending_captures.retain(|capture| match &capture.target {
            CaptureTarget::Copy(copy) => copy.frame.is_alive(),
            _ => true,
        });
        if self.pending_captures.is_empty() {
            return;
        }
        let pending = std::mem::take(&mut self.pending_captures);
        let locked = self.is_locked();
        let built = displayed.is_none().then(|| {
            if let Some(shapes) = self.shapes.get() {
                shapes.next_frame(self.output.as_ref());
            }
            self.scene(renderer, None)
        });
        let displayed = displayed.or_else(|| built.as_ref().map(|scene| scene.elements.as_slice())).unwrap_or_default();
        let scene =
            Scene { elements: displayed.iter().collect(), size: self.output_pixels(), clear: self.clear_colour() };
        let wants_pointer =
            pending.iter().any(|capture| matches!(&capture.target, CaptureTarget::Copy(copy) if copy.cursor));
        let pointer = if wants_pointer { self.capture_pointer(renderer) } else { Vec::new() };
        let pointer_scene = (!pointer.is_empty()).then(|| Scene {
            elements: pointer.iter().chain(displayed).collect(),
            size: scene.size,
            clear: scene.clear,
        });
        // Rendered on first use, and shared by the captures read back from it.
        let mut texture = None;
        let mut pointer_texture = None;
        let mut changes: HashMap<_, Vec<_>> = HashMap::new();
        for capture in pending {
            let region = capture.region;
            let mut changed = &[][..];
            let mut tracking = None;
            if let CaptureTarget::Copy(copy) = &capture.target {
                let full = Rectangle::from_size(scene.size.into());
                let fits = match &copy.buffer {
                    ClientBuffer::Shm(_) => region.intersection(full) == Some(region),
                    ClientBuffer::Dmabuf(_) => region == full,
                };
                if !fits {
                    copy.frame.fail(CopyFailure::BufferConstraints);
                    continue;
                }
                let key = DamageKey { output: copy.output.clone(), cursor: copy.cursor, region };
                // While the screen is locked, a screen being shared sends
                // nothing new: the lock screen may show notifications. Its
                // frame waits, and the call keeps the last one from before.
                // A single copy, such as a screenshot, still goes ahead.
                if copy.wait_for_damage && locked && copy.history.streaming(&key) {
                    self.pending_captures.push(capture);
                    continue;
                }
                let coordinates = (region.loc.x, region.loc.y, region.size.w, region.size.h);
                let damage = changes
                    .entry((Arc::as_ptr(&copy.history.0), copy.cursor, coordinates))
                    .or_insert_with(|| copy.history.changes(&key, &scene, &pointer));
                if copy.wait_for_damage {
                    if damage.is_empty() {
                        self.pending_captures.push(capture);
                        continue;
                    }
                    changed = damage;
                }
                tracking = Some((copy.history.clone(), key));
            }
            match capture.target {
                CaptureTarget::File(path) => match pixels(renderer, &mut texture, &scene, region) {
                    Ok(rgba) => {
                        let (width, height) = (region.size.w as u32, region.size.h as u32);
                        match write_png(&path, width, height, &rgba) {
                            Ok(()) => tracing::info!("screenshot saved to {}", path.display()),
                            Err(err) => tracing::warn!("could not write {}: {err}", path.display()),
                        }
                    }
                    Err(err) => tracing::warn!("capture failed: {err}"),
                },
                CaptureTarget::Copy(mut copy) => {
                    let copied = if let Some(shown) = pointer_scene.as_ref().filter(|_| copy.cursor) {
                        copy.buffer.fill(renderer, &mut pointer_texture, shown, region)
                    } else {
                        copy.buffer.fill(renderer, &mut texture, &scene, region)
                    };
                    finish_frame(&copy.frame, changed, copied, tracking, (region.size.w, region.size.h));
                }
                CaptureTarget::ColourPick => match pixels(renderer, &mut texture, &scene, region) {
                    Ok(rgba) => self.colour_pick_taken(rgba, (region.size.w, region.size.h)),
                    Err(err) => {
                        tracing::warn!("colour pick capture failed: {err}");
                        self.cancel_colour_pick();
                    }
                },
                CaptureTarget::Duck | CaptureTarget::Shot | CaptureTarget::Recording => {}
            }
        }
    }
}

/// Renders the scene into a texture or GPU buffer, and waits for the GPU to
/// finish so that it can be handed over.
fn render_scene<T, E>(renderer: &mut GlesRenderer, buffer: &mut T, scene: &Scene<E>) -> Result<(), String>
where
    GlesRenderer: Bind<T>,
    E: RenderElement<GlesRenderer>,
{
    let mut target = renderer.bind(buffer).map_err(|err| err.to_string())?;
    let mut damage = OutputDamageTracker::new(scene.size, 1.0, Transform::Normal);
    let result = damage
        .render_output(renderer, &mut target, 0, &scene.elements, scene.clear)
        .map_err(|err| format!("{err:?}"))?;
    result.sync.wait().map_err(|err| format!("{err:?}"))
}

/// Reads a region of the scene as RGBA rows, rendering it into `texture` the
/// first time.
fn pixels<E: RenderElement<GlesRenderer>>(
    renderer: &mut GlesRenderer,
    texture: &mut Option<Result<GlesTexture, String>>,
    scene: &Scene<E>,
    region: Rectangle<i32, Buffer>,
) -> Result<Vec<u8>, String> {
    let rendered = texture.get_or_insert_with(|| {
        let mut texture: GlesTexture =
            renderer.create_buffer(Fourcc::Abgr8888, scene.size.into()).map_err(|err| err.to_string())?;
        render_scene(renderer, &mut texture, scene)?;
        Ok(texture)
    });
    read_region(renderer, rendered.as_ref().map_err(Clone::clone)?, region)
}

/// The box around all of a frame's damage, sent as its only damage.
///
/// xdg-desktop-portal-wlr 0.7 hands damage on to the screen sharing app in
/// a few PipeWire slots, and when the app took fewer slots than there are
/// rectangles it merges the rest into one starting at y = x (a typo in
/// `xdpw_pwr_enqueue_buffer`). On a wide screen that rectangle lies below
/// the frame, and Chromium-based browsers crashed reading it. One
/// rectangle never takes that path.
fn bounding(damage: &[Rectangle<i32, Buffer>]) -> Option<Rectangle<i32, Buffer>> {
    damage.iter().copied().filter(|rect| !rect.is_empty()).reduce(|union, rect| union.merge(rect))
}

/// Tells a program that its buffer is filled, or that it failed.
fn finish_frame(
    frame: &CopyFrame,
    damage: &[Rectangle<i32, Buffer>],
    result: Result<(), String>,
    tracking: Option<(ManagerDamage, DamageKey)>,
    size: (i32, i32),
) {
    if let Err(err) = result {
        if let Some((history, key)) = tracking {
            history.forget(&key);
        }
        tracing::warn!("screen copy: {err}");
        frame.fail(CopyFailure::Unknown);
        return;
    }
    frame.ready(damage, size);
}

/// Fills a program's buffer with `elements` (a window, for a window being
/// shared), drawn from the top left, and says what changed since the last
/// copy that `tracker` measured. Nothing happens, and nothing is returned,
/// when nothing changed.
pub(crate) fn copy_elements(
    renderer: &mut GlesRenderer,
    buffer: &mut ClientBuffer,
    tracker: &mut OutputDamageTracker,
    elements: Vec<OutputElement>,
    size: (i32, i32),
) -> Result<Option<Vec<Rectangle<i32, Buffer>>>, String> {
    let full = Rectangle::from_size(size.into());
    let damage = capture_damage(tracker, 1, full, elements.iter());
    if damage.is_empty() {
        return Ok(None);
    }
    let scene = Scene { elements, size, clear: [0.0, 0.0, 0.0, 0.0] };
    buffer.fill(renderer, &mut None, &scene, full)?;
    Ok(Some(damage))
}

/// Reads a region of a texture as RGBA rows, top to bottom.
pub fn read_region(
    renderer: &mut GlesRenderer,
    texture: &GlesTexture,
    region: Rectangle<i32, Buffer>,
) -> Result<Vec<u8>, String> {
    let mapping = renderer.copy_texture(texture, region, Fourcc::Abgr8888).map_err(|err| err.to_string())?;
    // Smithay draws into a texture with its top row first, so the rows come
    // back top-down as they are. (Its mapping reports every texture as
    // flipped, which describes GL's convention rather than these rows.)
    let data = renderer.map_texture(&mapping).map_err(|err| err.to_string())?;
    let length = region.size.w as usize * 4 * region.size.h as usize;
    data.get(..length).map(<[u8]>::to_vec).ok_or_else(|| "short read".into())
}

/// A picture drawn again and again into the same buffer: each frame draws
/// only what changed since the one before, and says where that is.
pub struct Redraw {
    size: (i32, i32),
    tracker: OutputDamageTracker,
    /// The buffer holds the frame before, so only damage needs drawing.
    drawn: bool,
}

impl Redraw {
    /// For a picture of `size` pixels, drawn from elements in pixels.
    pub fn new(size: (i32, i32)) -> Self {
        Redraw { size, tracker: OutputDamageTracker::new(size, 1.0, Transform::Normal), drawn: false }
    }

    pub fn size(&self) -> (i32, i32) {
        self.size
    }

    /// The frame drawn last did not get where it was going: the next frame
    /// draws everything again.
    pub fn forget(&mut self) {
        self.drawn = false;
    }

    /// Draws `elements` into `buffer`, the picture's, and waits for the GPU
    /// to finish. Returns the parts that changed: none when nothing did.
    pub fn draw<T, E>(
        &mut self,
        renderer: &mut GlesRenderer,
        buffer: &mut T,
        elements: &[E],
        clear: impl Into<Color32F>,
    ) -> Result<Vec<Rectangle<i32, Buffer>>, String>
    where
        GlesRenderer: Bind<T>,
        E: RenderElement<GlesRenderer>,
    {
        // A frame that fails leaves the buffer unknown: the next draws it all.
        let age = usize::from(std::mem::take(&mut self.drawn));
        let mut target = renderer.bind(buffer).map_err(|error| error.to_string())?;
        let result = self
            .tracker
            .render_output(renderer, &mut target, age, elements, clear)
            .map_err(|error| format!("{error:?}"))?;
        let damage = result.damage.cloned().unwrap_or_default();
        // What reads the picture next, another GPU or the CPU, needs it finished.
        result.sync.wait().map_err(|error| format!("{error:?}"))?;
        self.drawn = true;
        let whole = Rectangle::from_size(self.size.into());
        Ok(damage
            .iter()
            .map(|rect| Rectangle::new((rect.loc.x, rect.loc.y).into(), (rect.size.w, rect.size.h).into()))
            .filter_map(|rect: Rectangle<i32, Buffer>| rect.intersection(whole))
            .collect())
    }
}

/// Copies `rows` (as [`read_region`] gives them) into `rect` of a picture
/// `width` pixels wide, four bytes a pixel. Returns `false`, copying
/// nothing, when `rect` is not inside the picture or `rows` do not fill it.
pub fn patch_rows(picture: &mut [u8], width: i32, rect: Rectangle<i32, Buffer>, rows: &[u8]) -> bool {
    let stride = width.max(0) as usize * 4;
    let height = picture.len().checked_div(stride).unwrap_or(0);
    let whole = Rectangle::from_size((width.max(0), height as i32).into());
    let row = rect.size.w.max(0) as usize * 4;
    if !whole.contains_rect(rect) || row == 0 || rows.len() != row * rect.size.h as usize {
        return false;
    }
    for (line, source) in rows.chunks_exact(row).enumerate() {
        let start = (rect.loc.y as usize + line) * stride + rect.loc.x as usize * 4;
        picture[start..start + row].copy_from_slice(source);
    }
    true
}

#[cfg(test)]
mod patch_tests {
    use super::*;

    #[test]
    fn patch_rows_copies_inside_and_refuses_outside() {
        // A picture of 3 by 2 pixels, one byte value per pixel.
        let mut picture = vec![0u8; 3 * 2 * 4];
        let rect = Rectangle::new((1, 0).into(), (2, 2).into());
        assert!(patch_rows(&mut picture, 3, rect, &[7; 2 * 2 * 4]));
        let pixels: Vec<u8> = picture.chunks(4).map(|pixel| pixel[0]).collect();
        assert_eq!(pixels, [0, 7, 7, 0, 7, 7]);
        for outside in [Rectangle::new((2, 0).into(), (2, 1).into()), Rectangle::new((-1, 0).into(), (1, 1).into())] {
            assert!(!patch_rows(&mut picture, 3, outside, &[9; 2 * 4]));
        }
        assert!(!patch_rows(&mut picture, 3, rect, &[9; 4]), "too few rows");
        assert!(picture.iter().all(|byte| *byte != 9));
    }
}

/// Copies RGBA rows into a client's XRGB8888 shared memory buffer.
fn copy_into_shm(buffer: &WlBuffer, width: i32, height: i32, rgba: &[u8]) -> Result<(), String> {
    with_buffer_contents_mut(buffer, |ptr, len, info| {
        if info.width != width || info.height != height || info.stride < width * 4 {
            return Err(format!("buffer is {}x{}, expected {width}x{height}", info.width, info.height));
        }
        if !matches!(info.format, wl_shm::Format::Xrgb8888 | wl_shm::Format::Argb8888) {
            return Err(format!("unsupported format {:?}", info.format));
        }
        let needed = info.offset as usize + info.stride as usize * height as usize;
        if needed > len {
            return Err("buffer too small".into());
        }
        // SAFETY: The range was checked against the pool's length above. Smithay
        // provides exclusive access to the mapped buffer for this callback.
        #[allow(unsafe_code, reason = "Smithay exposes the client's mapped SHM buffer as a raw pointer")]
        let dst =
            unsafe { std::slice::from_raw_parts_mut(ptr.add(info.offset as usize), needed - info.offset as usize) };
        for line in 0..height as usize {
            let src = &rgba[line * width as usize * 4..(line + 1) * width as usize * 4];
            let row = &mut dst[line * info.stride as usize..line * info.stride as usize + width as usize * 4];
            // RGBA bytes to little-endian XRGB, which is B G R X in memory.
            for (dst_pixel, src_pixel) in row.as_chunks_mut::<4>().0.iter_mut().zip(src.as_chunks::<4>().0) {
                dst_pixel[0] = src_pixel[2];
                dst_pixel[1] = src_pixel[1];
                dst_pixel[2] = src_pixel[0];
                dst_pixel[3] = 255;
            }
        }
        Ok(())
    })
    .map_err(|err| format!("{err:?}"))?
}

fn write_png(path: &PathBuf, width: u32, height: u32, rgba: &[u8]) -> Result<(), String> {
    let file = std::fs::File::create(path).map_err(|err| err.to_string())?;
    let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().map_err(|err| err.to_string())?;
    // Opaque: the scene has no meaningful alpha.
    let opaque: Vec<u8> =
        rgba.as_chunks::<4>().0.iter().flat_map(|pixel| [pixel[0], pixel[1], pixel[2], 255]).collect();
    writer.write_image_data(&opaque).map_err(|err| err.to_string())
}

impl GlobalDispatch<ZwlrScreencopyManagerV1, ()> for Tileception {
    fn bind(
        _state: &mut Self,
        _dh: &DisplayHandle,
        _client: &Client,
        resource: New<ZwlrScreencopyManagerV1>,
        _data: &(),
        init: &mut DataInit<'_, Self>,
    ) {
        init.init(resource, ManagerDamage::default());
    }
}

impl Dispatch<ZwlrScreencopyManagerV1, ManagerDamage> for Tileception {
    fn request(
        state: &mut Self,
        _client: &Client,
        _manager: &ZwlrScreencopyManagerV1,
        request: zwlr_screencopy_manager_v1::Request,
        damage: &ManagerDamage,
        _dh: &DisplayHandle,
        init: &mut DataInit<'_, Self>,
    ) {
        let (frame, wl_output, overlay_cursor, requested) = match request {
            zwlr_screencopy_manager_v1::Request::CaptureOutput { frame, overlay_cursor, output } => {
                (frame, output, overlay_cursor, None)
            }
            zwlr_screencopy_manager_v1::Request::CaptureOutputRegion {
                frame,
                overlay_cursor,
                output,
                x: left,
                y: top,
                width,
                height,
            } => (frame, output, overlay_cursor, Some((left, top, width, height))),
            _ => return,
        };
        let cursor = overlay_cursor != 0;
        let Some(output) = smithay::output::Output::from_resource(&wl_output)
            .filter(|output| state.output_named(&output.name()).is_some())
        else {
            // The resource still needs initializing before reporting failure.
            let Some(output) = state.output.clone() else { return };
            let frame =
                init.init(frame, FrameData { region: state.output_region(), output, cursor, damage: damage.clone() });
            frame.failed();
            return;
        };
        let full = state.with_output(&output, |state| state.output_region()).expect("connected output");
        let region = if let Some((left, top, width, height)) = requested {
            // Asked for in logical pixels (slurp's), copied in physical ones.
            let rect = crate::render::physical(
                &tileception_core::Rect::new(left as f64, top as f64, width as f64, height as f64),
                output.current_scale().fractional_scale(),
            );
            let asked =
                Rectangle::<i32, Buffer>::new((rect.loc.x, rect.loc.y).into(), (rect.size.w, rect.size.h).into());
            asked.intersection(full).unwrap_or(full)
        } else {
            full
        };
        let frame = init.init(frame, FrameData { region, output, cursor, damage: damage.clone() });
        let size = region.size;
        frame.buffer(wl_shm::Format::Xrgb8888, size.w as u32, size.h as u32, size.w as u32 * 4);
        if frame.version() >= 3 {
            // Screen sharing through xdg-desktop-portal-wlr requires a GPU
            // buffer on offer. Only whole outputs are rendered into one.
            if region == full {
                frame.linux_dmabuf(Fourcc::Xrgb8888 as u32, size.w as u32, size.h as u32);
            }
            frame.buffer_done();
        }
    }
}

impl Dispatch<ZwlrScreencopyFrameV1, FrameData> for Tileception {
    fn request(
        state: &mut Self,
        _client: &Client,
        frame: &ZwlrScreencopyFrameV1,
        request: zwlr_screencopy_frame_v1::Request,
        data: &FrameData,
        _dh: &DisplayHandle,
        _init: &mut DataInit<'_, Self>,
    ) {
        let (buffer, damage) = match request {
            zwlr_screencopy_frame_v1::Request::Copy { buffer } => (buffer, false),
            zwlr_screencopy_frame_v1::Request::CopyWithDamage { buffer } => (buffer, true),
            _ => return,
        };
        let Some(buffer) = ClientBuffer::from_wl(buffer) else {
            frame.failed();
            return;
        };
        if matches!(buffer, ClientBuffer::Dmabuf(_)) && buffer.size() != Some((data.region.size.w, data.region.size.h))
        {
            frame.failed();
            return;
        }
        // A stream of frames (portals, OBS) is sharing; one copy is a screenshot.
        let sharing = damage || matches!(buffer, ClientBuffer::Dmabuf(_));
        let copy = ClientCopy {
            frame: CopyFrame::Wlr(frame.clone()),
            buffer,
            wait_for_damage: damage,
            cursor: data.cursor,
            history: data.damage.clone(),
            output: data.output.downgrade(),
        };
        // With damage, the copy waits for the screen to change; without, it
        // is taken right away.
        state.queue_copy(&data.output, data.region, copy, sharing, !damage);
    }
}

impl Tileception {
    /// Queues a copy of part of a screen for its next frame, or fails it
    /// when the screen is gone. `now` draws a frame for it right away.
    pub(crate) fn queue_copy(
        &mut self,
        output: &smithay::output::Output,
        region: Rectangle<i32, Buffer>,
        copy: ClientCopy,
        sharing: bool,
        now: bool,
    ) {
        if self.output_named(&output.name()).as_ref() != Some(output) {
            copy.frame.fail(CopyFailure::Stopped);
            return;
        }
        // Headless, nobody is watching to be warned; recordings stay clean.
        let warn = self.headless.is_none();
        self.with_output(output, |state| {
            if warn {
                state.bar.capture = Some((std::time::Instant::now(), sharing));
            }
            state.pending_captures.push(PendingCapture { region, target: CaptureTarget::Copy(copy) });
        });
        if now {
            self.schedule_render();
        }
    }

    /// The whole of a connected screen, in pixels.
    pub(crate) fn screen_region(&mut self, output: &smithay::output::Output) -> Option<Rectangle<i32, Buffer>> {
        self.output_named(&output.name()).filter(|named| named == output)?;
        self.with_output(output, |state| state.output_region())
    }
}

#[cfg(test)]
mod damage_tests {
    use super::*;

    fn rect(left: i32, top: i32, width: i32, height: i32) -> Rectangle<i32, Buffer> {
        Rectangle::new((left, top).into(), (width, height).into())
    }

    #[test]
    fn damage_is_sent_as_one_box() {
        // The bar's clock at the right and a window at the left of a 4K screen.
        let damage = [rect(3700, 0, 120, 30), rect(10, 400, 800, 600), rect(0, 0, 0, 0)];
        assert_eq!(bounding(&damage), Some(rect(10, 0, 3810, 1000)));
        assert_eq!(bounding(&[]), None);
    }
}
