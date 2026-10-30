//! Pictures of this machine's windows shown on another.
//!
//! Once a frame, each such window is drawn into a texture of its own. What
//! changed since the last picture (the damage) is read back and handed to a
//! thread that compresses it and queues it behind other traffic. The next
//! picture waits until the other machine says this one arrived, so a slow
//! network lowers the rate instead of piling pictures up.

use std::{
    sync::mpsc,
    time::{Duration, Instant},
};

use smithay::{
    backend::{
        allocator::Fourcc,
        renderer::{
            Bind, Offscreen,
            damage::OutputDamageTracker,
            element::{AsRenderElements, surface::WaylandSurfaceRenderElement},
            gles::{GlesRenderer, GlesTexture},
        },
    },
    desktop::Window,
    utils::{Buffer, Logical, Rectangle, Scale, Size, Transform},
};
use tileception_core::WindowId;
use tileception_handoff::{Area, Header, Message, Messenger, PeerId, pack};

use crate::{
    Tileception,
    capture::read_region,
    render::{OutputElement, enlarged, physical_point},
};

/// How long to wait for a picture to arrive before sending the next anyway.
const ACK_WAIT: Duration = Duration::from_secs(2);

/// A picture's pixels and the areas of it that changed.
type Captured = (Vec<u8>, Vec<Area>);

struct Job {
    header: Header,
    pixels: Vec<u8>,
    changed: Vec<Area>,
}

pub(crate) struct Stream {
    texture: Option<GlesTexture>,
    damage: Option<OutputDamageTracker>,
    /// The picture's size in pixels.
    size: (i32, i32),
    serial: u64,
    /// When the last picture went, until it arrived.
    sent: Option<Instant>,
    jobs: mpsc::Sender<Job>,
}

impl Stream {
    /// Starts the thread that sends a window's pictures to `peer`.
    pub(crate) fn start(peer: PeerId, window: u64, messenger: Messenger) -> Option<Stream> {
        let (jobs, receiver) = mpsc::channel::<Job>();
        let started = std::thread::Builder::new().name("tileception-handoff-picture".into()).spawn(move || {
            while let Ok(job) = receiver.recv() {
                for message in pack(job.header, &job.pixels, &job.changed) {
                    if !messenger.send_bulk(peer, Message::WindowPicture(message)) {
                        return;
                    }
                }
            }
        });
        if let Err(error) = started {
            tracing::warn!("handoff: cannot send pictures of window {window}: {error}");
            return None;
        }
        Some(Stream { texture: None, damage: None, size: (0, 0), serial: 0, sent: None, jobs })
    }

    pub(crate) fn acked(&mut self, serial: u64) {
        if serial == self.serial {
            self.sent = None;
        }
    }

    /// The next picture is a whole one.
    fn restart(&mut self) {
        self.texture = None;
        self.damage = None;
        self.size = (0, 0);
        self.sent = None;
    }

    fn waiting(&self) -> bool {
        self.sent.is_some_and(|since| since.elapsed() < ACK_WAIT)
    }
}

impl Tileception {
    /// Sends new pictures of this machine's windows shown elsewhere. Called
    /// by the backend once per frame, with its renderer.
    pub fn process_streams(&mut self, renderer: &mut GlesRenderer) {
        if self.handoff.windows.away.is_empty() {
            return;
        }
        let paused: Vec<WindowId> = self
            .handoff
            .windows
            .away
            .iter()
            .filter(|(_, away)| self.is_locked() || self.handoff.active_edge(away.peer).is_none())
            .map(|(window, _)| *window)
            .collect();
        let windows: Vec<WindowId> = self.handoff.windows.away.keys().copied().collect();
        let scale = self.scale;
        for window in windows {
            let Some(placed) = self.windows.get(&window).cloned() else { continue };
            let Some(away) = self.handoff.windows.away.get_mut(&window) else { continue };
            // Nothing goes while sharing is paused, and the first picture
            // afterwards is a whole one.
            if paused.contains(&window) {
                away.stream.restart();
                continue;
            }
            if away.stream.waiting() {
                continue;
            }
            match capture(renderer, &mut away.stream, &placed, scale) {
                Ok(Some((pixels, changed))) => {
                    let stream = &mut away.stream;
                    stream.serial += 1;
                    let header = Header {
                        window: window.0,
                        serial: stream.serial,
                        width: stream.size.0 as u32,
                        height: stream.size.1 as u32,
                        scale,
                    };
                    if stream.jobs.send(Job { header, pixels, changed }).is_ok() {
                        stream.sent = Some(Instant::now());
                    }
                }
                Ok(None) => {}
                Err(error) => tracing::warn!("handoff: picture of window {}: {error}", window.0),
            }
            self.update_away_info(window);
        }
        // Not on any screen here, so frame callbacks keep them drawing.
        let Some(output) = self.output.clone() else { return };
        let elapsed = self.start.elapsed();
        for window in self.handoff.windows.away.keys() {
            if let Some(placed) = self.windows.get(window) {
                placed.send_frame(&output, elapsed, Some(Duration::ZERO), |_, _| Some(output.clone()));
            }
        }
    }
}

/// Draws a window into its stream's texture, returning its pixels and what
/// changed, or nothing when nothing did.
fn capture(
    renderer: &mut GlesRenderer,
    stream: &mut Stream,
    window: &Window,
    scale: f64,
) -> Result<Option<Captured>, String> {
    let geometry: Rectangle<i32, Logical> = window.geometry();
    if geometry.size.w <= 0 || geometry.size.h <= 0 {
        return Ok(None);
    }
    let size =
        ((f64::from(geometry.size.w) * scale).round() as i32, (f64::from(geometry.size.h) * scale).round() as i32);
    if stream.size != size || stream.texture.is_none() || stream.damage.is_none() {
        let buffer_size: Size<i32, Buffer> = size.into();
        stream.texture =
            Some(renderer.create_buffer(Fourcc::Abgr8888, buffer_size).map_err(|error| error.to_string())?);
        stream.damage = Some(OutputDamageTracker::new(size, 1.0, Transform::Normal));
        stream.size = size;
    }
    let (Some(texture), Some(damage)) = (stream.texture.as_mut(), stream.damage.as_mut()) else { return Ok(None) };
    // The window's own top left (without shadows) at the texture's.
    let origin = physical_point((-f64::from(geometry.loc.x), -f64::from(geometry.loc.y)), scale);
    let surfaces: Vec<WaylandSurfaceRenderElement<GlesRenderer>> =
        window.render_elements(renderer, origin, Scale::from(1.0), 1.0);
    let elements: Vec<OutputElement> = enlarged(surfaces, origin, scale).collect();
    let changed = {
        let mut target = renderer.bind(texture).map_err(|error| error.to_string())?;
        let result = damage
            .render_output(renderer, &mut target, 1, &elements, [0.0, 0.0, 0.0, 0.0])
            .map_err(|error| format!("{error:?}"))?;
        let Some(rectangles) = result.damage.cloned() else { return Ok(None) };
        result.sync.wait().map_err(|error| format!("{error:?}"))?;
        rectangles
    };
    let changed: Vec<Area> = changed
        .iter()
        .filter(|rectangle| rectangle.size.w > 0 && rectangle.size.h > 0)
        .map(|rectangle| Area {
            left: rectangle.loc.x.max(0) as u32,
            top: rectangle.loc.y.max(0) as u32,
            width: rectangle.size.w as u32,
            height: rectangle.size.h as u32,
        })
        .collect();
    if changed.is_empty() {
        return Ok(None);
    }
    let whole = Rectangle::<i32, Buffer>::new((0, 0).into(), size.into());
    let pixels = read_region(renderer, texture, whole)?;
    Ok(Some((pixels, changed)))
}
