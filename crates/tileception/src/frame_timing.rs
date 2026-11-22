//! When frames reach the screen, for programs that pace their drawing by it:
//! presentation feedback (`wp_presentation`: when a frame was shown) and
//! fifo barriers (`wp_fifo_v1`, which Mesa's Vulkan FIFO present mode and
//! SDL 3 use: a commit waits until the content before it has been shown).
//! Also the content type a surface declares (`wp_content_type_v1`), which
//! game mode reads.
//!
//! A fifo barrier belongs to the frame that first shows its surface's
//! content: it is taken when that frame is made (for the screen showing the
//! surface) and let go when the frame reaches the screen. Content committed
//! after a frame was made waits for the next one, so no frame of a FIFO
//! program is skipped.

use std::time::{Duration, Instant};

use smithay::{
    backend::renderer::element::{RenderElementPresentationState, RenderElementStates},
    delegate_content_type, delegate_fifo, delegate_presentation,
    desktop::{
        WindowSurfaceType, layer_map_for_output,
        utils::{OutputPresentationFeedback, surface_presentation_feedback_flags_from_states},
    },
    output::Output,
    reexports::{
        calloop::timer::{TimeoutAction, Timer},
        wayland_protocols::wp::{
            content_type::v1::server::wp_content_type_v1, presentation_time::server::wp_presentation_feedback,
        },
        wayland_server::{Client, DisplayHandle, Resource, protocol::wl_surface::WlSurface},
    },
    utils::{Clock, IsAlive, Monotonic},
    wayland::{
        compositor::{
            Barrier, BufferAssignment, Cacheable, CompositorHandler, SurfaceAttributes, SurfaceData, get_parent,
            with_states,
        },
        content_type::{ContentTypeState, ContentTypeSurfaceCachedState},
        fifo::{FifoBarrierCachedState, FifoManagerState},
        presentation::{PresentationFeedbackCachedState, PresentationState, Refresh},
        shell::wlr_layer::Layer,
    },
};

use tileception_core::WindowId;

use crate::Tileception;

/// How long a fifo barrier may wait, untaken or in a frame that never
/// flips (a screen turned off, unplugged or switched away), before it is
/// let go anyway.
const FIFO_STALL: Duration = Duration::from_millis(250);
/// How often the watchdog looks for such barriers.
const FIFO_WATCH: Duration = Duration::from_millis(100);

/// A fifo barrier of content already shown by its surface, not yet taken by
/// a frame. Its own copy of the barrier lets it go even after the surface is
/// destroyed.
struct Waiting {
    surface: WlSurface,
    barrier: Barrier,
    client: Option<Client>,
    since: Instant,
}

/// Fifo barriers waiting for a frame, and those in frames waiting to flip.
#[derive(Default)]
pub struct Fifo {
    waiting: Vec<Waiting>,
    in_flight: Vec<(u64, FifoBatch, Instant)>,
    next_frame: u64,
}

/// The fifo barriers a frame lets go once it reaches the screen.
#[derive(Default)]
pub struct FifoBatch(Vec<(Barrier, Option<Client>)>);

/// What a frame queued on a screen carries until its page flip: its
/// presentation feedback, and the in-flight fifo batches it lets go.
pub struct FrameDone {
    pub feedback: Option<OutputPresentationFeedback>,
    pub fifo: Vec<u64>,
}

/// Registers the three globals (feedback times use the monotonic clock),
/// and the watchdog that lets go of fifo barriers no frame will.
pub fn init(state: &mut Tileception) {
    let display = &state.display_handle;
    PresentationState::new::<Tileception>(display, libc::CLOCK_MONOTONIC as u32);
    FifoManagerState::new::<Tileception>(display);
    ContentTypeState::new::<Tileception>(display);
    let watchdog = state.loop_handle.insert_source(Timer::from_duration(FIFO_WATCH), |_, _, state| {
        state.release_stalled_fifo();
        TimeoutAction::ToDuration(FIFO_WATCH)
    });
    if let Err(error) = watchdog {
        tracing::warn!("fifo watchdog: {error}");
    }
}

delegate_presentation!(Tileception);
delegate_fifo!(Tileception);
delegate_content_type!(Tileception);

/// Whether a surface says it shows a game.
pub fn says_game(surface: &WlSurface) -> bool {
    with_states(surface, |states| {
        *states.cached_state.get::<ContentTypeSurfaceCachedState>().current().content_type()
            == wp_content_type_v1::Type::Game
    })
}

/// The refresh period of an output, for feedback.
pub fn refresh_of(output: &Output) -> Refresh {
    output
        .current_mode()
        .map(|mode| mode.refresh)
        .filter(|refresh| *refresh > 0)
        .map_or(Refresh::Unknown, |millihertz| Refresh::Fixed(Duration::from_secs_f64(1000.0 / f64::from(millihertz))))
}

/// Marks feedback shown now, for backends that cannot say when the screen
/// really changed (a nested window, or no screen at all), and for frames
/// that changed nothing on screen.
pub fn presented_now(mut feedback: OutputPresentationFeedback, refresh: Refresh) {
    let now = Clock::<Monotonic>::new().now();
    feedback.presented::<_, Monotonic>(now, refresh, 0, wp_presentation_feedback::Kind::empty());
}

/// A commit that brings new content (or removes it) without asking for
/// feedback replaces
/// content that may still hold feedback: that content was never shown, so
/// its feedback is discarded. (Smithay keeps it, and it would be reported
/// with the new content.)
///
/// Whether a commit does so is noted when it is made, and travels with it
/// through blocked transactions and synchronized subsurfaces
/// ([`Supersedes`]); the feedback is discarded when the commit is applied
/// ([`apply_superseded_feedback`]), as only then is the earlier content
/// really replaced.
pub fn note_superseding_commit(surface: &WlSurface) {
    with_states(surface, |states| {
        // A new buffer or none: either way, the one before is not shown.
        let replaced = matches!(
            states.cached_state.get::<SurfaceAttributes>().pending().buffer,
            Some(BufferAssignment::NewBuffer(_) | BufferAssignment::Removed)
        );
        let has_feedback = !states.cached_state.get::<PresentationFeedbackCachedState>().pending().callbacks.is_empty();
        *states.cached_state.get::<Supersedes>().pending() =
            Supersedes { discard: replaced && !has_feedback, has_feedback };
    });
}

/// A commit was applied: if it replaced content without asking for
/// feedback, the feedback left over is that content's, which was never
/// shown.
pub fn apply_superseded_feedback(surface: &WlSurface) {
    with_states(surface, |states| {
        let discard = std::mem::take(&mut *states.cached_state.get::<Supersedes>().current()).discard;
        if discard {
            for callback in states.cached_state.get::<PresentationFeedbackCachedState>().current().callbacks.drain(..) {
                callback.discarded();
            }
        }
    });
}

/// Whether a commit replaces the content before it without feedback of its
/// own, kept with the commit's other state until it is applied.
#[derive(Debug, Default, Clone, Copy)]
pub struct Supersedes {
    discard: bool,
    has_feedback: bool,
}

impl Cacheable for Supersedes {
    fn commit(&mut self, _display: &DisplayHandle) -> Self {
        std::mem::take(self)
    }

    /// Commits merged before they apply (a synchronized subsurface's): one
    /// with feedback replaces what came before, as smithay does with the
    /// feedback itself; one without keeps an earlier decision to discard.
    fn merge_into(self, into: &mut Self, _display: &DisplayHandle) {
        if self.has_feedback {
            *into = self;
        } else {
            into.discard |= self.discard;
        }
    }
}

fn root_of(surface: &WlSurface) -> WlSurface {
    let mut root = surface.clone();
    while let Some(parent) = get_parent(&root) {
        root = parent;
    }
    root
}

impl Tileception {
    /// The presentation feedback of what the frame being made shows on the
    /// current output: its windows, layer surfaces and lock surface. With
    /// `states`, only surfaces the frame really drew count; without (no
    /// screen at all), all of them on this screen.
    pub fn presentation_feedback(&self, states: Option<&RenderElementStates>) -> Option<OutputPresentationFeedback> {
        let output = self.output.as_ref()?;
        let mut feedback = OutputPresentationFeedback::new(output);
        // A presented area is one picture of the screen: the surfaces in it
        // count by where they are, as with no screen at all.
        let states = states.filter(|_| self.zoom_here().is_none());
        // Without states: the lock screen covers everything, and a
        // fullscreen window everything but itself and the overlay layer.
        let covered = |window: WindowId| self.is_locked() || self.fullscreen.is_some_and(|full| full != window);
        // Whether any surface had feedback to take: most frames have none.
        let taken = std::cell::Cell::new(false);
        let taken = &taken;
        let scanout = |surface: &WlSurface, data: &SurfaceData| {
            let drawn = states.is_none_or(|states| {
                states
                    .element_render_state(surface)
                    .is_some_and(|state| state.presentation_state != RenderElementPresentationState::Skipped)
            });
            if drawn && !data.cached_state.get::<PresentationFeedbackCachedState>().current().callbacks.is_empty() {
                taken.set(true);
            }
            drawn.then(|| output.clone())
        };
        let flags = |surface: &WlSurface, _: &SurfaceData| {
            states.map_or(wp_presentation_feedback::Kind::empty(), |states| {
                surface_presentation_feedback_flags_from_states(surface, states)
            })
        };
        for (window_id, window) in self.windows.iter().filter(|(window_id, _)| self.screen_shows(**window_id)) {
            if states.is_none() && covered(*window_id) {
                continue;
            }
            window.take_presentation_feedback(&mut feedback, scanout, flags);
        }
        for layer in layer_map_for_output(output).layers() {
            let hidden = self.is_locked() || (self.fullscreen.is_some() && layer.layer() != Layer::Overlay);
            if states.is_none() && hidden {
                continue;
            }
            layer.take_presentation_feedback(&mut feedback, scanout, flags);
        }
        if let Some(lock) = self.lock_surface() {
            smithay::desktop::utils::take_presentation_feedback_surface_tree(lock, &mut feedback, scanout, flags);
        }
        taken.get().then_some(feedback)
    }

    /// Feedback of a frame that changed nothing on `output`: what was
    /// committed shows from its next refresh, which is reported once that
    /// has passed, unless the screen stopped showing frames by then.
    /// A commit was applied: if it set a fifo barrier, the frame that shows
    /// it will let it go.
    pub fn track_fifo_barrier(&mut self, surface: &WlSurface) {
        let barrier = with_states(surface, |states| {
            states.cached_state.get::<FifoBarrierCachedState>().current().barrier.clone()
        });
        let Some(barrier) = barrier.filter(|barrier| !barrier.is_signaled()) else { return };
        if let Some(waiting) = self.fifo.waiting.iter_mut().find(|waiting| waiting.surface == *surface) {
            // Content that was never shown was replaced: its barrier goes.
            if waiting.barrier != barrier {
                waiting.barrier.signal();
                waiting.barrier = barrier;
                waiting.since = Instant::now();
            }
            return;
        }
        let client = surface.client();
        self.fifo.waiting.push(Waiting { surface: surface.clone(), barrier, client, since: Instant::now() });
    }

    /// Takes the fifo barriers of the content the frame being made for the
    /// current output shows. Surfaces drawn on another screen are left for
    /// that screen's frame; hidden or destroyed ones go with any.
    pub fn take_fifo_barriers(&mut self) -> FifoBatch {
        let mut batch = FifoBatch::default();
        for waiting in std::mem::take(&mut self.fifo.waiting) {
            if waiting.surface.alive() && !self.fifo_shown_here(&waiting.surface) {
                self.fifo.waiting.push(waiting);
            } else {
                batch.0.push((waiting.barrier, waiting.client));
            }
        }
        batch
    }

    /// Keeps a frame's batch until its page flip; `None` when it is empty.
    pub fn fifo_in_flight(&mut self, batch: FifoBatch) -> Option<u64> {
        if batch.0.is_empty() {
            return None;
        }
        self.fifo.next_frame += 1;
        let frame = self.fifo.next_frame;
        self.fifo.in_flight.push((frame, batch, Instant::now()));
        Some(frame)
    }

    /// The frame reached the screen, or will not: its batch goes.
    pub fn release_fifo_frame(&mut self, frame: u64) {
        if let Some(index) = self.fifo.in_flight.iter().position(|(queued, _, _)| *queued == frame) {
            let (_, batch, _) = self.fifo.in_flight.swap_remove(index);
            self.release_fifo(batch);
        }
    }

    /// Whether a surface's content would be shown by the current output's
    /// frame, or by none (minimized, not placed, not a window).
    fn fifo_shown_here(&self, surface: &WlSurface) -> bool {
        if let Some(window) = self.window_of_surface(surface) {
            return self.screen_shows(window)
                || self.minimized_anywhere(window)
                || self.area_for_window(window).is_none();
        }
        let root = root_of(surface);
        for output in self.all_outputs() {
            if layer_map_for_output(&output).layer_for_surface(&root, WindowSurfaceType::ALL).is_some() {
                return self.output.as_ref() == Some(&output);
            }
        }
        true
    }

    /// A frame reached the screen (or showed nothing new): the programs
    /// whose content it carried may commit their next.
    pub fn release_fifo(&mut self, batch: FifoBatch) {
        let mut clients: Vec<Client> = Vec::new();
        for (barrier, client) in batch.0 {
            barrier.signal();
            if let Some(client) = client.filter(|client| !clients.contains(client)) {
                clients.push(client);
            }
        }
        let display_handle = self.display_handle.clone();
        for client in clients {
            self.client_compositor_state(&client).blocker_cleared(self, &display_handle);
        }
    }

    /// Lets every fifo program go: no screen makes frames that would (no
    /// screen at all, a nested window, screens off).
    pub fn release_all_fifo(&mut self) {
        let mut batch = FifoBatch::default();
        for waiting in std::mem::take(&mut self.fifo.waiting) {
            batch.0.push((waiting.barrier, waiting.client));
        }
        for (_, frame, _) in std::mem::take(&mut self.fifo.in_flight) {
            batch.0.extend(frame.0);
        }
        self.release_fifo(batch);
    }

    /// The watchdog: barriers no frame took, and frames that never flipped,
    /// within [`FIFO_STALL`].
    fn release_stalled_fifo(&mut self) {
        let mut batch = FifoBatch::default();
        let (stalled, waiting): (Vec<_>, Vec<_>) = std::mem::take(&mut self.fifo.waiting)
            .into_iter()
            .partition(|waiting| waiting.since.elapsed() >= FIFO_STALL);
        self.fifo.waiting = waiting;
        batch.0.extend(stalled.into_iter().map(|waiting| (waiting.barrier, waiting.client)));
        let (stalled, in_flight): (Vec<_>, Vec<_>) = std::mem::take(&mut self.fifo.in_flight)
            .into_iter()
            .partition(|(_, _, since)| since.elapsed() >= FIFO_STALL);
        self.fifo.in_flight = in_flight;
        for (_, frame, _) in stalled {
            batch.0.extend(frame.0);
        }
        if !batch.0.is_empty() {
            self.release_fifo(batch);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merged_commits_keep_a_discard_until_one_brings_feedback() {
        let display = smithay::reexports::wayland_server::Display::<()>::new().expect("display");
        let handle = display.handle();
        let replacing = Supersedes { discard: true, has_feedback: false };
        let plain = Supersedes { discard: false, has_feedback: false };
        let with_feedback = Supersedes { discard: false, has_feedback: true };

        // A buffer without feedback, then a commit that changes nothing else.
        let mut cached = Supersedes::default();
        replacing.merge_into(&mut cached, &handle);
        plain.merge_into(&mut cached, &handle);
        assert!(cached.discard);

        // A later commit with its own feedback replaces the earlier ones'.
        with_feedback.merge_into(&mut cached, &handle);
        assert!(!cached.discard);

        // Committing hands the state on and starts afresh.
        let mut pending = replacing;
        assert!(pending.commit(&handle).discard);
        assert!(!pending.discard);
    }
}
