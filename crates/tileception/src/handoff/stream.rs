//! Pictures of this machine's windows shown on another.
//!
//! Once a frame, each such window is drawn into a texture of its own. What
//! changed since the last picture (the damage) is read back and handed to a
//! thread that compresses it and queues it behind other traffic. The next
//! picture waits until the other machine says this one arrived, so a slow
//! network lowers the rate instead of piling pictures up.

use std::{
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
        mpsc,
    },
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
    desktop::PopupManager,
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

/// A picture's pixels, the areas of it that changed, and where the window
/// and its popups are in it.
struct Captured {
    pixels: Vec<u8>,
    changed: Vec<Area>,
    body: Area,
    popups: Vec<Area>,
}

/// The largest picture sent, in pixels on a side.
const LARGEST: i32 = 16384;

struct Job {
    /// Dropped, even halfway, once the stream's round moved past this one.
    round: u64,
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
    /// Goes up whenever sharing pauses, so pictures still queued then are
    /// not sent after all.
    round: Arc<AtomicU64>,
}

impl Stream {
    /// Starts the thread that sends a window's pictures to `peer`.
    pub(crate) fn start(peer: PeerId, window: u64, messenger: Messenger) -> Option<Stream> {
        let (jobs, receiver) = mpsc::channel::<Job>();
        let round = Arc::new(AtomicU64::new(0));
        let current = round.clone();
        let started = std::thread::Builder::new().name("tileception-handoff-picture".into()).spawn(move || {
            while let Ok(job) = receiver.recv() {
                for message in pack(&job.header, &job.pixels, &job.changed) {
                    if current.load(Ordering::Acquire) != job.round {
                        break;
                    }
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
        Some(Stream { texture: None, damage: None, size: (0, 0), serial: 0, sent: None, jobs, round })
    }

    pub(crate) fn acked(&mut self, serial: u64) {
        if serial == self.serial {
            self.sent = None;
        }
    }

    /// Pictures not sent yet are dropped, and the next one is a whole one.
    fn restart(&mut self) {
        self.round.fetch_add(1, Ordering::AcqRel);
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
    /// Drops pictures not sent yet of windows shown on `peer` (any machine
    /// without one), as sharing pauses.
    pub(crate) fn pause_streams(&mut self, peer: Option<PeerId>) {
        for away in self.handoff.windows.away.values_mut() {
            if peer.is_none_or(|peer| away.peer == peer)
                && let Some(stream) = away.stream.as_mut()
            {
                stream.restart();
            }
        }
    }

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
            // None while the connection is lost.
            let Some(stream) = self.handoff.windows.away.get_mut(&window).and_then(|away| away.stream.as_mut()) else {
                continue;
            };
            // Nothing goes while sharing is paused, and the first picture
            // afterwards is a whole one.
            if paused.contains(&window) {
                stream.restart();
                continue;
            }
            if stream.waiting() {
                continue;
            }
            match capture(renderer, stream, &placed, scale) {
                Ok(Some(Captured { pixels, changed, body, popups })) => {
                    stream.serial += 1;
                    let header = Header {
                        window: window.0,
                        serial: stream.serial,
                        width: stream.size.0 as u32,
                        height: stream.size.1 as u32,
                        scale,
                        body,
                        popups,
                    };
                    let round = stream.round.load(Ordering::Acquire);
                    if stream.jobs.send(Job { round, header, pixels, changed }).is_ok() {
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
        for (window, away) in &self.handoff.windows.away {
            if away.stream.is_none() {
                continue;
            }
            if let Some(placed) = self.windows.get(window) {
                placed.send_frame(&output, elapsed, Some(Duration::ZERO), |_, _| Some(output.clone()));
            }
        }
    }
}

/// Where a window and its popups go in its picture.
#[derive(Debug, PartialEq)]
struct Placed {
    /// All of them, in logical pixels from the window's top left.
    bounds: Rectangle<i32, Logical>,
    /// The picture's size in pixels.
    size: (i32, i32),
    body: Area,
    popups: Vec<Area>,
}

/// Fits a window (`own`) and its popups, in logical pixels from its top
/// left, into one picture of `scale` pixels per logical pixel.
fn place(own: Rectangle<i32, Logical>, popups: &[Rectangle<i32, Logical>], scale: f64) -> Placed {
    let bounds = popups.iter().fold(own, |bounds, popup| bounds.merge(*popup));
    let to_pixels = |logical: i32| (f64::from(logical) * scale).round() as i32;
    let size = (to_pixels(bounds.size.w).min(LARGEST), to_pixels(bounds.size.h).min(LARGEST));
    // A rectangle from the window's top left, in the picture's pixels.
    let area = |rect: &Rectangle<i32, Logical>| {
        let left = to_pixels(rect.loc.x - bounds.loc.x).clamp(0, size.0);
        let top = to_pixels(rect.loc.y - bounds.loc.y).clamp(0, size.1);
        Area {
            left: left as u32,
            top: top as u32,
            width: to_pixels(rect.size.w).min(size.0 - left).max(0) as u32,
            height: to_pixels(rect.size.h).min(size.1 - top).max(0) as u32,
        }
    };
    Placed { bounds, size, body: area(&own), popups: popups.iter().map(area).collect() }
}

/// Draws a window and its popups into its stream's texture, returning its
/// pixels and what changed, or nothing when nothing did.
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
    // The window and its popups, from the window's own top left (without
    // shadows): popups may reach past it on any side.
    let own = Rectangle::<i32, Logical>::from_size(geometry.size);
    let popups: Vec<Rectangle<i32, Logical>> = window
        .toplevel()
        .map(|toplevel| {
            PopupManager::popups_for_surface(toplevel.wl_surface())
                .map(|(popup, offset)| Rectangle::new(offset, popup.geometry().size))
                .filter(|popup| popup.size.w > 0 && popup.size.h > 0)
                .collect()
        })
        .unwrap_or_default();
    let Placed { bounds, size, body, popups } = place(own, &popups, scale);
    if stream.size != size || stream.texture.is_none() || stream.damage.is_none() {
        let buffer_size: Size<i32, Buffer> = size.into();
        stream.texture =
            Some(renderer.create_buffer(Fourcc::Abgr8888, buffer_size).map_err(|error| error.to_string())?);
        stream.damage = Some(OutputDamageTracker::new(size, 1.0, Transform::Normal));
        stream.size = size;
    }
    let (Some(texture), Some(damage)) = (stream.texture.as_mut(), stream.damage.as_mut()) else { return Ok(None) };
    // The window's surface origin, placed so that the bounds start at the
    // texture's top left. Rendering a window includes its popups.
    let origin =
        physical_point((-f64::from(geometry.loc.x + bounds.loc.x), -f64::from(geometry.loc.y + bounds.loc.y)), scale);
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
    Ok(Some(Captured { pixels, changed, body, popups }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: i32, y: i32, w: i32, h: i32) -> Rectangle<i32, Logical> {
        Rectangle::new((x, y).into(), (w, h).into())
    }

    #[test]
    fn a_window_alone_is_the_whole_picture() {
        let placed = place(rect(0, 0, 400, 300), &[], 1.5);
        assert_eq!(placed.size, (600, 450));
        assert_eq!(placed.body, Area { left: 0, top: 0, width: 600, height: 450 });
        assert!(placed.popups.is_empty());
    }

    #[test]
    fn popups_past_the_window_widen_the_picture() {
        // A menu hanging below and to the left, and a submenu off the right.
        let menu = rect(-50, 280, 200, 100);
        let submenu = rect(380, 10, 120, 60);
        let placed = place(rect(0, 0, 400, 300), &[menu, submenu], 2.0);
        assert_eq!(placed.bounds, rect(-50, 0, 550, 380));
        assert_eq!(placed.size, (1100, 760));
        assert_eq!(placed.body, Area { left: 100, top: 0, width: 800, height: 600 });
        assert_eq!(
            placed.popups,
            vec![
                Area { left: 0, top: 560, width: 400, height: 200 },
                Area { left: 860, top: 20, width: 240, height: 120 },
            ]
        );
    }

    #[test]
    fn a_huge_picture_is_cut_off() {
        let placed = place(rect(0, 0, 400, 300), &[rect(0, 0, 20000, 10)], 1.0);
        assert_eq!(placed.size, (LARGEST, 300));
        assert_eq!(placed.popups[0].width, LARGEST as u32);
    }
}
