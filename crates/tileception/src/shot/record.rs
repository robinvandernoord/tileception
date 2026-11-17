//! Recording an area of the screen: the compositor reads the area about 30
//! times a second and ffmpeg makes an MP4 of it. Stopping offers to save it
//! as MP4 or GIF, or to copy it.

use std::{
    io::Write,
    path::{Path, PathBuf},
    process::{Child, Stdio},
    sync::{
        Arc, Mutex,
        mpsc::{Receiver, SyncSender, TrySendError},
    },
    time::{Duration, Instant},
};

use smithay::{
    backend::renderer::gles::GlesRenderer,
    output::Output,
    reexports::calloop::{
        RegistrationToken,
        timer::{TimeoutAction, Timer},
    },
    utils::{Buffer, Rectangle},
};
use tileception_core::Rect;
use tileception_notify::Urgency;

use crate::{
    Tileception,
    commands::Command,
    render::OutputElement,
    ui::{Align, Button, Look, Painter, PointerAction, PointerTarget, Style},
};

/// Frames a second in the video.
const RATE: u32 = 30;
/// Time between frames read from the screen.
const FRAME: Duration = Duration::from_millis(33);
/// How often a finishing ffmpeg is looked at.
const POLL: Duration = Duration::from_millis(100);
/// Frames waiting for ffmpeg; more are dropped rather than held up.
const QUEUE: usize = 6;

/// An area being recorded.
pub struct Recording {
    output: Output,
    /// The area in points, on `output`.
    rect: Rect,
    /// The area in pixels.
    region: Rectangle<i32, Buffer>,
    /// Frames on their way to ffmpeg, with when they were read; gone once
    /// stopped, which ends its input.
    frames: Option<SyncSender<(Instant, Vec<u8>)>>,
    /// When it was stopped, for the last frame to last until then.
    stopped: Arc<Mutex<Option<Instant>>>,
    child: Child,
    path: PathBuf,
    started: Instant,
    ticker: Option<RegistrationToken>,
    /// While the frame is read, the border and Stop button are left out.
    pub hidden: bool,
}

/// What to do with a finished recording, from its notification's buttons.
#[derive(Debug, Clone, PartialEq)]
pub enum Keep {
    CopyMp4(PathBuf),
    /// Made into a GIF first, which is copied.
    CopyGif(PathBuf),
    /// Saved with the other videos.
    Save(PathBuf),
}

/// Recordings nobody saved or copied stay a day, in case one is still
/// pasted somewhere, and go when the next recording starts.
fn forget_old_recordings(folder: &Path) {
    let Ok(entries) = std::fs::read_dir(folder) else { return };
    let day = Duration::from_secs(24 * 60 * 60);
    for entry in entries.flatten() {
        let old = entry
            .metadata()
            .and_then(|metadata| metadata.modified())
            .is_ok_and(|modified| modified.elapsed().is_ok_and(|age| age > day));
        if old {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// How many frames of the video fall before `elapsed`.
fn frames_before(elapsed: Duration) -> u64 {
    (elapsed.as_secs_f64() * f64::from(RATE)).floor() as u64
}

/// Writes the frames read from the screen as a video of a steady `RATE`
/// frames a second: each lasts until the next one was read, repeated as
/// often as that takes, and the last until `stopped`. Screens are read
/// when they are drawn, which is not always at the same pace, so the
/// video's length is true to the time recorded.
fn pace(
    queue: Receiver<(Instant, Vec<u8>)>,
    out: &mut impl Write,
    stopped: &Mutex<Option<Instant>>,
) -> std::io::Result<()> {
    let mut start = None;
    let mut written = 0;
    let mut last: Option<Vec<u8>> = None;
    for (read, frame) in queue {
        let begun = *start.get_or_insert(read);
        if let Some(previous) = last.as_ref() {
            while written < frames_before(read.saturating_duration_since(begun)) {
                out.write_all(previous)?;
                written += 1;
            }
        }
        last = Some(frame);
    }
    let (Some(begun), Some(previous)) = (start, last) else { return Ok(()) };
    let end = stopped.lock().ok().and_then(|stopped| *stopped).unwrap_or_else(Instant::now);
    // The last frame shows at least once.
    let total = (frames_before(end.saturating_duration_since(begun)) + 1).max(written + 1);
    while written < total {
        out.write_all(&previous)?;
        written += 1;
    }
    Ok(())
}

/// The Stop button while recording.
#[derive(PartialEq)]
struct Stop;

impl PointerAction for Stop {
    fn activate(&self, state: &mut Tileception, _rect: Rect, _point: tileception_core::Point) {
        state.stop_recording();
    }
}

impl Tileception {
    /// Starts recording `rect` (points) of the active screen.
    pub fn start_recording(&mut self, rect: Rect) {
        if self.recording.is_some() {
            return;
        }
        let Some(output) = self.output.clone() else { return };
        let (screen_w, screen_h) = self.output_pixels();
        let physical = crate::render::physical(&rect, self.scale);
        let full = Rectangle::<i32, Buffer>::from_size((screen_w, screen_h).into());
        let asked = Rectangle::<i32, Buffer>::new(
            (physical.loc.x, physical.loc.y).into(),
            (physical.size.w, physical.size.h).into(),
        );
        let Some(region) = asked.intersection(full).filter(|region| region.size.w > 1 && region.size.h > 1) else {
            return;
        };
        let recordings = super::cache_dir().join("recordings");
        forget_old_recordings(&recordings);
        let Some(path) = super::stamped(&recordings, "recording", "mp4") else {
            self.notify_locally("Recording failed", "No room for the recording in the cache folder", Urgency::Normal);
            return;
        };
        let size = format!("{}x{}", region.size.w, region.size.h);
        let spawned = std::process::Command::new("ffmpeg")
            .args(["-hide_banner", "-loglevel", "error", "-y", "-f", "rawvideo", "-pix_fmt", "rgba"])
            .args(["-video_size", &size, "-framerate", &RATE.to_string(), "-i", "-"])
            // H.264 wants an even width and height.
            .args(["-vf", "pad=ceil(iw/2)*2:ceil(ih/2)*2", "-c:v", "libx264", "-preset", "veryfast", "-crf", "23"])
            .args(["-pix_fmt", "yuv420p", "-movflags", "+faststart"])
            .arg(&path)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
        let mut child = match spawned {
            Ok(child) => child,
            Err(error) => {
                let body = if error.kind() == std::io::ErrorKind::NotFound {
                    "Recording needs ffmpeg; install it and try again".to_string()
                } else {
                    format!("ffmpeg did not start: {error}")
                };
                self.notify_locally("Recording failed", &body, Urgency::Normal);
                return;
            }
        };
        let Some(mut input) = child.stdin.take() else { return };
        let (frames, queue) = std::sync::mpsc::sync_channel(QUEUE);
        let stopped = Arc::new(Mutex::new(None));
        let stop_time = stopped.clone();
        // Writing can wait on ffmpeg, so it is done away from the compositor.
        let spawned = std::thread::Builder::new().name("recording".into()).spawn(move || {
            let _ = pace(queue, &mut input, &stop_time);
        });
        if let Err(error) = spawned {
            tracing::warn!("could not start the recording writer: {error}");
            let _ = child.kill();
            return;
        }
        let ticker = self.loop_handle.insert_source(Timer::immediate(), |_, _, state| {
            let Some(recording) = state.recording.as_ref().filter(|recording| recording.frames.is_some()) else {
                return TimeoutAction::Drop;
            };
            let (output, region) = (recording.output.clone(), recording.region);
            state.with_output(&output, |state| {
                state
                    .pending_captures
                    .push(crate::capture::PendingCapture { region, target: crate::capture::CaptureTarget::Recording });
            });
            state.schedule_render();
            TimeoutAction::ToDuration(FRAME)
        });
        self.recording = Some(Recording {
            output,
            rect,
            region,
            frames: Some(frames),
            stopped,
            child,
            path,
            started: Instant::now(),
            ticker: ticker.ok(),
            hidden: false,
        });
        self.schedule_render();
    }

    /// Leaves the Stop button and the line around the area out, while a
    /// frame is taken.
    pub fn recording_hidden(&mut self, hidden: bool) {
        if let Some(recording) = self.recording.as_mut() {
            recording.hidden = hidden;
        }
    }

    /// A frame of the recorded area, from [`Self::process_captures`].
    pub fn recording_frame(&mut self, rgba: Vec<u8>) {
        let Some(frames) = self.recording.as_ref().and_then(|recording| recording.frames.as_ref()) else { return };
        match frames.try_send((Instant::now(), rgba)) {
            Ok(()) | Err(TrySendError::Full(_)) => {}
            Err(TrySendError::Disconnected(_)) => self.recording_failed(),
        }
    }

    fn recording_failed(&mut self) {
        let Some(mut recording) = self.recording.take() else { return };
        if let Some(ticker) = recording.ticker.take() {
            self.loop_handle.remove(ticker);
        }
        let _ = recording.child.kill();
        let _ = recording.child.wait();
        let _ = std::fs::remove_file(&recording.path);
        self.notify_locally("Recording failed", "ffmpeg stopped while recording", Urgency::Normal);
        self.schedule_render();
    }

    /// Stops recording; once ffmpeg has written the file, a notification
    /// asks what to do with it.
    pub fn stop_recording(&mut self) {
        let Some(recording) = self.recording.as_mut() else { return };
        if let Ok(mut stopped) = recording.stopped.lock() {
            *stopped = Some(Instant::now());
        }
        if recording.frames.take().is_none() {
            return;
        }
        if let Some(ticker) = recording.ticker.take() {
            self.loop_handle.remove(ticker);
        }
        self.schedule_render();
        let polled = self.loop_handle.insert_source(Timer::from_duration(POLL), |_, _, state| {
            let Some(recording) = state.recording.as_mut() else { return TimeoutAction::Drop };
            match recording.child.try_wait() {
                Ok(None) => TimeoutAction::ToDuration(POLL),
                Ok(Some(status)) => {
                    let finished = state.recording.take();
                    if let Some(recording) = finished {
                        state.recording_written(&recording.path, status.success());
                    }
                    TimeoutAction::Drop
                }
                Err(error) => {
                    tracing::warn!("could not wait for ffmpeg: {error}");
                    state.recording = None;
                    TimeoutAction::Drop
                }
            }
        });
        if let Err(error) = polled {
            tracing::warn!("could not wait for the recording: {error}");
        }
    }

    fn recording_written(&mut self, path: &Path, success: bool) {
        self.schedule_render();
        if !success || !path.exists() {
            self.notify_locally("Recording failed", "ffmpeg could not write the video", Urgency::Normal);
            return;
        }
        let path = path.to_path_buf();
        let buttons = vec![
            ("Copy MP4", Some(Command::KeepRecording(Keep::CopyMp4(path.clone())))),
            ("Copy GIF", Some(Command::KeepRecording(Keep::CopyGif(path.clone())))),
            ("Save", Some(Command::KeepRecording(Keep::Save(path.clone())))),
        ];
        self.notify_locally_with(
            "Recording finished",
            "Copy it as a video or a GIF, or save it?",
            Urgency::Normal,
            buttons,
        );
    }

    /// Does what a finished recording's notification asked.
    pub fn keep_recording(&mut self, keep: Keep) {
        match keep {
            Keep::Save(path) => {
                let Some(target) = super::stamped(&super::videos_dir(), "screencast", "mp4") else { return };
                let moved = std::fs::rename(&path, &target).or_else(|_| std::fs::copy(&path, &target).map(|_| ()));
                match moved {
                    Ok(()) => {
                        let _ = std::fs::remove_file(&path);
                        self.notify_saved("Recording saved", &target, None);
                    }
                    Err(error) => {
                        self.notify_locally("Recording not saved", &error.to_string(), Urgency::Normal);
                    }
                }
            }
            Keep::CopyMp4(path) => {
                if !path.exists() {
                    self.notify_locally("Recording not copied", "The recording is gone", Urgency::Normal);
                    return;
                }
                self.copy_recording(&path, None);
                self.notify_locally("Video copied", "Paste it in a chat or a folder", Urgency::Low);
            }
            Keep::CopyGif(path) => self.copy_gif(path),
        }
    }

    /// Puts a recording on the clipboard as a file, for chats and folders,
    /// and with `data` (its type and contents) for programs that take the
    /// picture itself, as chats do with a GIF.
    fn copy_recording(&mut self, path: &Path, data: Option<(&str, Vec<u8>)>) {
        let uri = super::file_uri(path);
        let mut types: Vec<(String, Vec<u8>)> = Vec::new();
        if let Some((mime_type, bytes)) = data {
            types.push((mime_type.to_string(), bytes));
        }
        types.extend([
            ("text/uri-list".to_string(), format!("{uri}\r\n").into_bytes()),
            ("x-special/gnome-copied-files".to_string(), format!("copy\n{uri}").into_bytes()),
            ("text/plain;charset=utf-8".to_string(), path.to_string_lossy().into_owned().into_bytes()),
        ]);
        let mime_types = types.iter().map(|(mime_type, _)| mime_type.clone()).collect();
        self.offer_clipboard(mime_types, crate::launcher::Clip::Saved(Arc::from(types)));
    }

    /// Turns the MP4 into a GIF beside it, in the background, and copies that.
    fn copy_gif(&mut self, path: PathBuf) {
        let target = path.with_extension("gif");
        // A palette made from the video itself keeps the colours true.
        let filter = "fps=15,split[a][b];[a]palettegen=stats_mode=diff[p];[b][p]paletteuse=dither=sierra2_4a";
        let spawned = std::process::Command::new("ffmpeg")
            .args(["-hide_banner", "-loglevel", "error", "-y", "-i"])
            .arg(&path)
            .args(["-vf", filter])
            .arg(&target)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
        let mut child = match spawned {
            Ok(child) => child,
            Err(error) => {
                self.notify_locally("GIF not copied", &format!("ffmpeg did not start: {error}"), Urgency::Normal);
                return;
            }
        };
        self.notify_locally("Making a GIF", "It is copied in a moment", Urgency::Low);
        let polled =
            self.loop_handle.insert_source(Timer::from_duration(POLL), move |_, _, state| match child.try_wait() {
                Ok(None) => TimeoutAction::ToDuration(POLL),
                Ok(Some(status)) if status.success() => {
                    match std::fs::read(&target) {
                        Ok(bytes) => {
                            let _ = std::fs::remove_file(&path);
                            state.copy_recording(&target, Some(("image/gif", bytes)));
                            state.notify_locally("GIF copied", "Paste it in a chat or a folder", Urgency::Low);
                        }
                        Err(error) => state.notify_locally("GIF not copied", &error.to_string(), Urgency::Normal),
                    }
                    TimeoutAction::Drop
                }
                Ok(Some(_)) | Err(_) => {
                    state.notify_locally("GIF not copied", "ffmpeg could not convert the recording", Urgency::Normal);
                    TimeoutAction::Drop
                }
            });
        if let Err(error) = polled {
            tracing::warn!("could not wait for the GIF: {error}");
        }
    }

    /// The Stop button, while recording on this screen.
    pub(crate) fn recording_pointer_target(&self, point: tileception_core::Point) -> Option<PointerTarget> {
        let stop = self.recording_controls()?.1;
        stop.contains(point).then(|| PointerTarget::button(stop, Stop))
    }

    /// The bar with the time and the Stop button, and the button in it,
    /// under the area, or over it at the bottom of the screen.
    fn recording_controls(&self) -> Option<(Rect, Rect)> {
        let recording = self.recording.as_ref().filter(|recording| recording.frames.is_some() && !recording.hidden)?;
        if self.output.as_ref() != Some(&recording.output) {
            return None;
        }
        let screen = self.output_rect();
        let (width, height) = (196.0, 40.0);
        let area = recording.rect;
        let mut top = area.bottom() + 10.0;
        if top + height > screen.bottom() {
            top = area.y - 10.0 - height;
        }
        if top < screen.y {
            top = area.bottom() - height - 10.0;
        }
        let left = (area.x + area.w / 2.0 - width / 2.0).clamp(screen.x + 4.0, screen.right() - width - 4.0);
        let bar = Rect::new(left, top, width, height);
        let stop = Rect::new(bar.right() - 76.0, bar.y + 6.0, 70.0, 28.0);
        Some((bar, stop))
    }

    /// A line around the recorded area, and the bar with the time and Stop.
    pub fn recording_elements(&self, renderer: &mut GlesRenderer) -> Vec<OutputElement> {
        let Some((bar, stop)) = self.recording_controls() else { return Vec::new() };
        let Some(recording) = self.recording.as_ref() else { return Vec::new() };
        let theme = &self.theme;
        let mut painter = Painter::new(self, renderer);
        let area = recording.rect;
        let line = theme.danger.alpha(0.85);
        let edge = 2.0;
        painter.fill(Rect::new(area.x - edge, area.y - edge, area.w + edge * 2.0, edge), line, 0.0);
        painter.fill(Rect::new(area.x - edge, area.bottom(), area.w + edge * 2.0, edge), line, 0.0);
        painter.fill(Rect::new(area.x - edge, area.y, edge, area.h), line, 0.0);
        painter.fill(Rect::new(area.right(), area.y, edge, area.h), line, 0.0);
        painter.popup(bar);
        let blink = recording.started.elapsed().as_millis() % 1000 < 700;
        if blink {
            painter.fill(Rect::new(bar.x + 14.0, bar.y + bar.h / 2.0 - 5.0, 10.0, 10.0), theme.danger, 5.0);
        }
        let seconds = recording.started.elapsed().as_secs();
        let time = format!("{}:{:02}", seconds / 60, seconds % 60);
        painter.text(
            &time,
            Rect::new(bar.x + 32.0, bar.y, stop.x - bar.x - 36.0, bar.h),
            Style::new(theme.font_size, theme.text).bold().align(Align::Left),
        );
        let hover = stop.contains(tileception_core::Point::new(self.pointer.x, self.pointer.y));
        painter.button(stop, Button::new("Stop", Look::Warning), hover);
        painter.finish()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test setup and assertions should fail immediately on unexpected errors")]
mod tests {
    use super::*;

    #[test]
    fn frames_last_until_the_next_one_was_read() {
        let begun = Instant::now();
        let after = |millis| begun + Duration::from_millis(millis);
        let (frames, queue) = std::sync::mpsc::sync_channel(8);
        // Read at 0, then late at 100 ms, then at 120 ms; stopped at 200 ms.
        frames.send((begun, vec![1])).unwrap();
        frames.send((after(100), vec![2])).unwrap();
        frames.send((after(120), vec![3])).unwrap();
        drop(frames);
        let stopped = Mutex::new(Some(after(200)));
        let mut out = Vec::new();
        pace(queue, &mut out, &stopped).unwrap();
        // At 30 a second, frames 0 to 2 come before 100 ms. The second and
        // third were read within frame 3, so the newer one shows there,
        // until 200 ms: frames 3 to 6.
        assert_eq!(out, vec![1, 1, 1, 3, 3, 3, 3]);
    }

    #[test]
    fn a_single_frame_still_makes_a_video() {
        let (frames, queue) = std::sync::mpsc::sync_channel(1);
        frames.send((Instant::now(), vec![7])).unwrap();
        drop(frames);
        let mut out = Vec::new();
        pace(queue, &mut out, &Mutex::new(None)).unwrap();
        assert_eq!(out, vec![7]);
    }
}
