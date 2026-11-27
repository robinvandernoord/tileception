//! Recording an area of the screen: the compositor reads the area about 30
//! times a second and ffmpeg makes an MP4 of it. Stopping offers to save it
//! as MP4 or GIF, or to copy it.
//!
//! Sound is optional: what the speakers play (the default output's monitor)
//! and the microphone (the default input), each taken by an ffmpeg of its
//! own through PulseAudio (PipeWire serves it too). Each sound file is
//! stamped with the wall clock, as is the first frame of the video, so once
//! stopped the sound is lined up with the picture and mixed into the MP4.
//! The sound server mutes the microphone stream before it reaches ffmpeg.
//! A sound that cannot be taken is left out, and the video goes on.

use std::{
    io::Write,
    os::unix::process::CommandExt,
    path::{Path, PathBuf},
    process::{Child, Stdio},
    sync::{
        Arc, Mutex,
        mpsc::{Receiver, SyncSender, TrySendError},
    },
    time::{Duration, Instant, SystemTime},
};

use serde::{Deserialize, Serialize};

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
    ui::{Align, Button, Look, Painter, PointerAction, PointerTarget, Style, glyph},
};

/// Frames a second in the video.
const RATE: u32 = 30;
/// Time between frames read from the screen.
const FRAME: Duration = Duration::from_millis(33);
/// How often a finishing ffmpeg is looked at.
const POLL: Duration = Duration::from_millis(100);
/// Frames waiting for ffmpeg; more are dropped rather than held up.
const QUEUE: usize = 6;
/// How long a sound's ffmpeg gets to finish its file once asked to stop.
const SOUND_GRACE: Duration = Duration::from_secs(5);
/// The sound files' clock starts this long before they are started, so their
/// first timestamps are never below zero.
const SOUND_LEAD: Duration = Duration::from_secs(2);

/// The sounds to record with the picture, remembered between recordings.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Sources {
    /// What the speakers play.
    #[serde(default)]
    pub system: bool,
    #[serde(default)]
    pub microphone: bool,
}

impl Sources {
    fn path() -> PathBuf {
        std::env::var_os("XDG_STATE_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".local/state"))
            .join("tileception/recording.toml")
    }

    /// The sounds chosen last time; none the first time.
    pub fn load() -> Sources {
        std::fs::read_to_string(Self::path()).ok().and_then(|text| toml::from_str(&text).ok()).unwrap_or_default()
    }

    pub fn save(self) {
        let path = Self::path();
        let written = toml::to_string(&self).map_err(|error| error.to_string()).and_then(|text| {
            if let Some(folder) = path.parent() {
                std::fs::create_dir_all(folder).map_err(|error| error.to_string())?;
            }
            std::fs::write(&path, text).map_err(|error| error.to_string())
        });
        if let Err(error) = written {
            tracing::warn!("could not remember the recording's sounds: {error}");
        }
    }
}

/// A sound source.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Source {
    System,
    Microphone,
}

impl Source {
    /// PulseAudio's name for it: whatever is the default when recording starts.
    fn device(self) -> &'static str {
        match self {
            Source::System => "@DEFAULT_MONITOR@",
            Source::Microphone => "@DEFAULT_SOURCE@",
        }
    }

    fn name(self) -> &'static str {
        match self {
            Source::System => "system sound",
            Source::Microphone => "microphone",
        }
    }

    fn file(self) -> &'static str {
        match self {
            Source::System => "system",
            Source::Microphone => "microphone",
        }
    }
}

/// A sound being recorded into a file of its own.
struct Sound {
    source: Source,
    child: Child,
    path: PathBuf,
    /// Its ffmpeg ended by itself: there is no sound to use from it.
    failed: bool,
    /// The user was told it is left out.
    told: bool,
}

struct MicMute {
    name: String,
    pending: Option<Receiver<Result<SystemTime, String>>>,
}

impl Sound {
    fn discard(&mut self) {
        self.failed = true;
        let _ = self.child.kill();
        let _ = std::fs::remove_file(&self.path);
    }
}

impl Drop for Sound {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Where a recording is: still taking frames, finishing its files, or
/// mixing the sound into the video.
enum Stage {
    Recording,
    Stopping { since: Instant },
    Mixing(Child),
}

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
    /// The finished recording.
    path: PathBuf,
    /// Where ffmpeg writes the picture: `path` itself without sound.
    video: PathBuf,
    sounds: Vec<Sound>,
    /// The clock of the sound files starts here.
    base: SystemTime,
    /// When the video's first frame was read.
    first_frame: Option<SystemTime>,
    /// Stretches with the microphone muted; the last one is open while muted.
    muted: Vec<(SystemTime, Option<SystemTime>)>,
    mic_mute: MicMute,
    stage: Stage,
    started: Instant,
    ticker: Option<RegistrationToken>,
    /// While the frame is read, the border and Stop button are left out.
    pub hidden: bool,
}

impl Recording {
    fn recording(&self, source: Source) -> bool {
        self.sounds.iter().any(|sound| sound.source == source && !sound.failed)
    }

    fn mic_muted(&self) -> bool {
        self.muted.last().is_some_and(|(_, end)| end.is_none())
    }
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

/// Seconds from `from` to `until`, below zero when `until` comes first.
fn seconds_between(from: SystemTime, until: SystemTime) -> f64 {
    match until.duration_since(from) {
        Ok(after) => after.as_secs_f64(),
        Err(before) => -before.duration().as_secs_f64(),
    }
}

/// The ffmpeg that records one sound into a file. The sound's timestamps
/// are the wall clock (PulseAudio's input stamps it so), counted from
/// `base`, which is in seconds since the epoch.
fn sound_args(source: Source, base: f64, path: &Path, stream_name: Option<&str>) -> Vec<String> {
    let mut args: Vec<String> =
        ["-hide_banner", "-loglevel", "error", "-y", "-f", "pulse", "-thread_queue_size", "1024"]
            .map(String::from)
            .to_vec();
    if let Some(name) = stream_name {
        // The server can restore mute by application name, so use a
        // separate name for each recording as well as for its stream.
        args.extend(["-name".into(), name.into(), "-stream_name".into(), name.into()]);
    }
    args.extend(["-i".into(), source.device().into(), "-copyts".into()]);
    args.extend(["-af".into(), format!("asetpts=PTS-{base:.6}/TB"), "-c:a".into(), "flac".into()]);
    args.push(path.to_string_lossy().into_owned());
    args
}

/// A sound to mix in: its file, and the stretches (in seconds of the video)
/// to leave silent.
struct Mixed<'a> {
    path: &'a Path,
    silent: Vec<(f64, Option<f64>)>,
}

/// The ffmpeg that puts the sounds under the video. `start` is when the
/// video's first frame was read, on the sounds' clock: sound from before it
/// is cut, and silence fills in until a sound begins. With two sounds both
/// are heard at their own volume. The picture is copied as it is.
fn mix_args(video: &Path, sounds: &[Mixed<'_>], start: f64, out: &Path) -> Vec<String> {
    let mut args: Vec<String> =
        ["-hide_banner", "-loglevel", "error", "-y", "-copyts", "-i"].map(String::from).to_vec();
    args.push(video.to_string_lossy().into_owned());
    for sound in sounds {
        args.extend(["-i".into(), sound.path.to_string_lossy().into_owned()]);
    }
    let mut graph = Vec::new();
    for (index, sound) in sounds.iter().enumerate() {
        let mut chain =
            format!("[{}:a]asetpts=PTS-{start:.6}/TB,atrim=start=0,aresample=async=1:first_pts=0", index + 1);
        if !sound.silent.is_empty() {
            // aeval evaluates t for each sample, including within a frame.
            let stretches: Vec<String> = sound
                .silent
                .iter()
                .map(|(from, until)| match until {
                    Some(until) => format!("gte(t,{from:.9})*lt(t,{until:.9})"),
                    None => format!("gte(t,{from:.9})"),
                })
                .collect();
            chain.push_str(&format!(",aeval='if({},0,val(ch))':c=same", stretches.join("+")));
        }
        chain.push_str(&format!("[s{index}]"));
        graph.push(chain);
    }
    let inputs: String = (0..sounds.len()).map(|index| format!("[s{index}]")).collect();
    if sounds.len() > 1 {
        // amix halves each of two inputs; the volume brings them back.
        graph.push(format!(
            "{inputs}amix=inputs={}:duration=longest:dropout_transition=0,volume={},apad[sound]",
            sounds.len(),
            sounds.len()
        ));
    } else {
        graph.push(format!("{inputs}apad[sound]"));
    }
    args.extend(["-filter_complex".into(), graph.join(";")]);
    args.extend(["-map", "0:v", "-map", "[sound]", "-c:v", "copy", "-c:a", "aac", "-b:a", "160k"].map(String::from));
    args.extend(["-shortest", "-movflags", "+faststart"].map(String::from));
    args.push(out.to_string_lossy().into_owned());
    args
}

/// Asks a sound's ffmpeg to finish its file, as `q` at its prompt does.
fn finish_sound(sound: &mut Sound) {
    if let Some(mut input) = sound.child.stdin.take() {
        let _ = input.write_all(b"q");
    }
}

/// The Stop button while recording.
#[derive(PartialEq)]
struct Stop;

impl PointerAction for Stop {
    fn activate(&self, state: &mut Tileception, _rect: Rect, _point: tileception_core::Point) {
        state.stop_recording();
    }
}

/// The microphone's button while recording, which mutes it and back.
#[derive(PartialEq)]
struct Mute;

impl PointerAction for Mute {
    fn activate(&self, state: &mut Tileception, _rect: Rect, _point: tileception_core::Point) {
        state.toggle_recording_mute();
    }
}

/// Where the recording bar's parts are.
struct Controls {
    bar: Rect,
    stop: Rect,
    /// Shown while what the speakers play is recorded.
    system: Option<Rect>,
    /// The microphone's mute button, while it is recorded.
    microphone: Option<Rect>,
}

impl Tileception {
    /// Starts recording `rect` (points) of the active screen, with the
    /// sounds in `sources`.
    pub fn start_recording(&mut self, rect: Rect, sources: Sources) {
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
            self.notify_locally(
                "Recording failed",
                "Could not create a recording file in the cache folder",
                Urgency::Normal,
            );
            return;
        };
        let base = SystemTime::now() - SOUND_LEAD;
        let base_seconds = base.duration_since(SystemTime::UNIX_EPOCH).map_or(0.0, |since| since.as_secs_f64());
        let stem = path.with_extension("");
        let wanted = [(sources.system, Source::System), (sources.microphone, Source::Microphone)];
        // The sounds start first, so they are there when the picture starts.
        let mut sounds = Vec::new();
        let parent = std::process::id() as libc::pid_t;
        let mic_name = format!("Tileception recording {parent} {base_seconds:.6}");
        for source in wanted.into_iter().filter(|(wanted, _)| *wanted).map(|(_, source)| source) {
            // NUT keeps the sample-rate time base for aligning the mute boundaries.
            let sound_path = PathBuf::from(format!("{}-{}.nut", stem.display(), source.file()));
            let mut command = std::process::Command::new("ffmpeg");
            // Name the PulseAudio input so mute only affects this recording.
            let stream_name = (source == Source::Microphone).then_some(mic_name.as_str());
            command
                .args(sound_args(source, base_seconds, &sound_path, stream_name))
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            // PulseAudio can keep feeding FFmpeg after the compositor exits.
            // The parent check also covers an exit before the signal was set.
            // SAFETY: The callback only makes syscalls and constructs errors from errno.
            #[allow(unsafe_code, reason = "Linux sets a parent-death signal in the child before exec")]
            unsafe {
                command.pre_exec(move || {
                    if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) == -1 {
                        return Err(std::io::Error::last_os_error());
                    }
                    if libc::getppid() != parent {
                        return Err(std::io::Error::from_raw_os_error(libc::ESRCH));
                    }
                    Ok(())
                });
            }
            match command.spawn() {
                Ok(child) => sounds.push(Sound { source, child, path: sound_path, failed: false, told: false }),
                Err(error) => tracing::warn!("could not record the {}: {error}", source.name()),
            }
        }
        let video =
            if sounds.is_empty() { path.clone() } else { PathBuf::from(format!("{}-video.mp4", stem.display())) };
        let size = format!("{}x{}", region.size.w, region.size.h);
        let spawned = std::process::Command::new("ffmpeg")
            .args(["-hide_banner", "-loglevel", "error", "-y", "-f", "rawvideo", "-pix_fmt", "rgba"])
            .args(["-video_size", &size, "-framerate", &RATE.to_string(), "-i", "-"])
            // H.264 wants an even width and height.
            .args(["-vf", "pad=ceil(iw/2)*2:ceil(ih/2)*2", "-c:v", "libx264", "-preset", "veryfast", "-crf", "23"])
            .args(["-pix_fmt", "yuv420p", "-movflags", "+faststart"])
            .arg(&video)
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
            state.check_recording_sounds();
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
            video,
            sounds,
            base,
            first_frame: None,
            muted: Vec::new(),
            mic_mute: MicMute { name: mic_name, pending: None },
            stage: Stage::Recording,
            started: Instant::now(),
            ticker: ticker.ok(),
            hidden: false,
        });
        self.schedule_render();
    }

    /// A sound whose ffmpeg ended by itself while recording (no PulseAudio,
    /// or no such device) is left out, and the recording goes on silent.
    fn check_recording_sounds(&mut self) {
        let Some(recording) = self.recording.as_mut() else { return };
        let mut lost = Vec::new();
        if let Some(pending) = &recording.mic_mute.pending {
            let result = match pending.try_recv() {
                Ok(result) => Some(result),
                Err(std::sync::mpsc::TryRecvError::Empty) => None,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => Some(Err("microphone mute worker stopped".into())),
            };
            if let Some(result) = result {
                recording.mic_mute.pending = None;
                match result {
                    Ok(boundary) => match recording.muted.last_mut() {
                        Some((_, end @ None)) => *end = Some(boundary),
                        _ => recording.muted.push((boundary, None)),
                    },
                    Err(error) => {
                        tracing::warn!("could not change the microphone recording mute: {error}");
                        if let Some(sound) =
                            recording.sounds.iter_mut().find(|sound| sound.source == Source::Microphone)
                            && !sound.failed
                        {
                            sound.discard();
                            sound.told = true;
                            lost.push(sound.source.name());
                        }
                    }
                }
            }
        }
        for sound in recording.sounds.iter_mut().filter(|sound| !sound.failed) {
            if let Ok(Some(status)) = sound.child.try_wait() {
                tracing::warn!("recording the {} stopped: {status}", sound.source.name());
                sound.failed = true;
                sound.told = true;
                lost.push(sound.source.name());
            }
        }
        if !lost.is_empty() {
            let body = format!("Couldn't record the {}", lost.join(" and the "));
            self.notify_locally("Recording without sound", &body, Urgency::Normal);
            self.schedule_render();
        }
    }

    /// Mutes the microphone in the recording, or unmutes it.
    pub fn toggle_recording_mute(&mut self) {
        let Some(recording) = self.recording.as_mut() else { return };
        if !recording.recording(Source::Microphone)
            || !matches!(recording.stage, Stage::Recording)
            || recording.mic_mute.pending.is_some()
        {
            return;
        }
        let muted = !recording.mic_muted();
        let requested = SystemTime::now();
        let name = recording.mic_mute.name.clone();
        let (done, pending) = std::sync::mpsc::channel();
        if let Err(error) = std::thread::Builder::new().name("microphone-mute".into()).spawn(move || {
            let result = tileception_media::mute_capture_stream(&name, muted)
                .map(|()| if muted { requested } else { SystemTime::now() });
            let _ = done.send(result);
        }) {
            tracing::warn!("could not start the microphone mute worker: {error}");
        }
        recording.mic_mute.pending = Some(pending);
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
        let Some(recording) = self.recording.as_mut() else { return };
        let Some(frames) = recording.frames.as_ref() else { return };
        let read = Instant::now();
        let clock = SystemTime::now();
        match frames.try_send((read, rgba)) {
            Ok(()) => {
                recording.first_frame.get_or_insert(clock);
            }
            Err(TrySendError::Full(_)) => {}
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
        let _ = std::fs::remove_file(&recording.video);
        self.notify_locally("Recording failed", "ffmpeg stopped while recording", Urgency::Normal);
        self.schedule_render();
    }

    /// Stops recording; once ffmpeg has written the file (and the sound is
    /// mixed in), a notification asks what to do with it.
    pub fn stop_recording(&mut self) {
        let Some(recording) = self.recording.as_mut() else { return };
        if let Ok(mut stopped) = recording.stopped.lock() {
            *stopped = Some(Instant::now());
        }
        if recording.frames.take().is_none() {
            return;
        }
        if recording.mic_mute.pending.is_some()
            && let Some(sound) = recording.sounds.iter_mut().find(|sound| sound.source == Source::Microphone)
        {
            sound.discard();
        }
        for sound in &mut recording.sounds {
            finish_sound(sound);
        }
        recording.stage = Stage::Stopping { since: Instant::now() };
        if let Some(ticker) = recording.ticker.take() {
            self.loop_handle.remove(ticker);
        }
        self.schedule_render();
        let polled = self.loop_handle.insert_source(Timer::from_duration(POLL), |_, _, state| {
            if state.recording_finishing() { TimeoutAction::ToDuration(POLL) } else { TimeoutAction::Drop }
        });
        if let Err(error) = polled {
            tracing::warn!("could not wait for the recording: {error}");
        }
    }

    /// Looks at the ffmpegs of a stopped recording, and takes the next step
    /// once they are done. Whether to look again.
    fn recording_finishing(&mut self) -> bool {
        let Some(recording) = self.recording.as_mut() else { return false };
        match &mut recording.stage {
            Stage::Recording => false,
            Stage::Stopping { since } => {
                let late = since.elapsed() > SOUND_GRACE;
                let video = match recording.child.try_wait() {
                    Ok(None) => return true,
                    Ok(Some(status)) => status.success(),
                    Err(error) => {
                        tracing::warn!("could not wait for ffmpeg: {error}");
                        false
                    }
                };
                for sound in &mut recording.sounds {
                    match sound.child.try_wait() {
                        Ok(Some(status)) => sound.failed |= !status.success(),
                        Ok(None) if late => {
                            tracing::warn!("the {} recording did not finish; leaving it out", sound.source.name());
                            let _ = sound.child.kill();
                            let _ = sound.child.wait();
                            sound.failed = true;
                        }
                        Ok(None) => return true,
                        Err(_) => sound.failed = true,
                    }
                }
                if !video {
                    let finished = self.recording.take();
                    if let Some(recording) = finished {
                        let _ = std::fs::remove_file(&recording.video);
                        self.recording_written(&recording.path, false);
                    }
                    return false;
                }
                if recording.sounds.is_empty() {
                    let finished = self.recording.take();
                    if let Some(recording) = finished {
                        self.recording_written(&recording.path, true);
                    }
                    return false;
                }
                self.mix_recording()
            }
            Stage::Mixing(child) => match child.try_wait() {
                Ok(None) => true,
                Ok(Some(status)) => {
                    self.recording_mixed(status.success());
                    false
                }
                Err(error) => {
                    tracing::warn!("could not wait for ffmpeg: {error}");
                    self.recording_mixed(false);
                    false
                }
            },
        }
    }

    /// Starts mixing the recorded sounds under the video. Whether there is
    /// something to wait for.
    fn mix_recording(&mut self) -> bool {
        let Some(recording) = self.recording.as_mut() else { return false };
        let usable =
            |sound: &&Sound| !sound.failed && std::fs::metadata(&sound.path).is_ok_and(|metadata| metadata.len() > 0);
        let first_frame = recording.first_frame.unwrap_or(recording.base);
        let mixed: Vec<Mixed<'_>> = recording
            .sounds
            .iter()
            .filter(usable)
            .map(|sound| Mixed {
                path: &sound.path,
                silent: if sound.source == Source::Microphone {
                    recording
                        .muted
                        .iter()
                        .map(|(from, until)| {
                            let from = seconds_between(first_frame, *from).max(0.0);
                            let until = until.map(|until| seconds_between(first_frame, until).max(0.0));
                            (from, until)
                        })
                        .collect()
                } else {
                    Vec::new()
                },
            })
            .collect();
        if mixed.is_empty() {
            self.recording_mixed(false);
            return false;
        }
        let start = seconds_between(recording.base, first_frame);
        let args = mix_args(&recording.video, &mixed, start, &recording.path);
        let spawned = std::process::Command::new("ffmpeg")
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
        match spawned {
            Ok(child) => {
                recording.stage = Stage::Mixing(child);
                true
            }
            Err(error) => {
                tracing::warn!("could not mix the recording's sound: {error}");
                self.recording_mixed(false);
                false
            }
        }
    }

    /// The sound is mixed in, or (when `mixed` is false) left out: the
    /// silent video is kept, with a word about the sound.
    fn recording_mixed(&mut self, mixed: bool) {
        let Some(recording) = self.recording.take() else { return };
        // Said already while recording, or not yet.
        let untold = recording.sounds.iter().any(|sound| sound.failed && !sound.told);
        let usable = recording.sounds.iter().any(|sound| !sound.failed);
        if mixed {
            let _ = std::fs::remove_file(&recording.video);
        } else {
            let _ = std::fs::remove_file(&recording.path);
            if let Err(error) = std::fs::rename(&recording.video, &recording.path) {
                tracing::warn!("could not keep the silent recording: {error}");
            }
        }
        if untold || (!mixed && usable) {
            let body = if mixed {
                "Some audio couldn't be recorded and was omitted"
            } else if usable {
                "The sound could not be mixed in, so the video is silent"
            } else {
                "The sound could not be recorded, so the video is silent"
            };
            self.notify_locally("Sound left out", body, Urgency::Normal);
        }
        self.recording_written(&recording.path, true);
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
                    self.notify_locally(
                        "Recording not copied",
                        "The recording file is no longer available",
                        Urgency::Normal,
                    );
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
            // A GIF has no sound.
            .args(["-an", "-vf", filter])
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
        self.notify_locally("Making a GIF", "It will be copied in a moment", Urgency::Low);
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

    /// The Stop button and the microphone's, while recording on this screen.
    pub(crate) fn recording_pointer_target(&self, point: tileception_core::Point) -> Option<PointerTarget> {
        let controls = self.recording_controls()?;
        if let Some(microphone) = controls.microphone.filter(|rect| rect.contains(point)) {
            return Some(PointerTarget::button(microphone, Mute));
        }
        controls.stop.contains(point).then(|| PointerTarget::button(controls.stop, Stop))
    }

    /// The bar with the time, the sounds and the Stop button, under the
    /// area, or over it at the bottom of the screen.
    fn recording_controls(&self) -> Option<Controls> {
        let recording = self.recording.as_ref().filter(|recording| recording.frames.is_some() && !recording.hidden)?;
        if self.output.as_ref() != Some(&recording.output) {
            return None;
        }
        let screen = self.output_rect();
        let system = recording.recording(Source::System);
        let microphone = recording.recording(Source::Microphone);
        let sound_w = 34.0;
        let (width, height) = (196.0 + sound_w * f64::from(u8::from(system) + u8::from(microphone)), 40.0);
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
        let mut cursor = stop.x - 6.0;
        let mut next = |shown: bool| {
            shown.then(|| {
                cursor -= sound_w;
                Rect::new(cursor, bar.y + 6.0, 28.0, 28.0)
            })
        };
        let microphone = next(microphone);
        let system = next(system);
        Some(Controls { bar, stop, system, microphone })
    }

    /// A line around the recorded area, and the bar with the time, the
    /// sounds being recorded and Stop.
    pub fn recording_elements(&self, renderer: &mut GlesRenderer) -> Vec<OutputElement> {
        let Some(controls) = self.recording_controls() else { return Vec::new() };
        let Some(recording) = self.recording.as_ref() else { return Vec::new() };
        let Controls { bar, stop, system, microphone } = controls;
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
        let sounds_x = system.or(microphone).map_or(stop.x, |rect| rect.x);
        painter.text(
            &time,
            Rect::new(bar.x + 32.0, bar.y, sounds_x - bar.x - 36.0, bar.h),
            Style::new(theme.font_size, theme.text).bold().align(Align::Left),
        );
        let pointer = tileception_core::Point::new(self.pointer.x, self.pointer.y);
        if let Some(rect) = system {
            // Only a sign that it is recorded: nothing to press.
            painter.glyph(glyph::VOLUME_HIGH, Rect::new(rect.x + 5.0, rect.y + 5.0, 18.0, 18.0), 18, theme.text);
        }
        if let Some(rect) = microphone {
            let muted = recording.mic_muted();
            let button = Button::glyph(if muted { glyph::MIC_OFF } else { glyph::MIC }, Look::Quiet)
                .enabled(recording.mic_mute.pending.is_none())
                .focused(muted);
            painter.button(rect, button, rect.contains(pointer));
        }
        painter.button(stop, Button::new("Stop", Look::Warning), stop.contains(pointer));
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
    fn records_each_sound_on_the_wall_clock() {
        let args = sound_args(Source::Microphone, 1_700_000_000.25, Path::new("/tmp/mic.nut"), Some("recording"));
        let device = args.iter().position(|arg| arg == "-i").unwrap() + 1;
        assert_eq!(args[device], "@DEFAULT_SOURCE@");
        assert!(args.contains(&"pulse".to_string()) && args.contains(&"-copyts".to_string()));
        assert!(args.contains(&"asetpts=PTS-1700000000.250000/TB".to_string()));
        assert_eq!(args.last().unwrap(), "/tmp/mic.nut");
        let system = sound_args(Source::System, 0.0, Path::new("/tmp/system.nut"), None);
        assert!(system.contains(&"@DEFAULT_MONITOR@".to_string()));
    }

    #[test]
    fn lines_the_sound_up_with_the_first_frame() {
        let system = Mixed { path: Path::new("/tmp/system.nut"), silent: Vec::new() };
        let args = mix_args(Path::new("/tmp/video.mp4"), &[system], 2.5, Path::new("/tmp/out.mp4"));
        let graph = &args[args.iter().position(|arg| arg == "-filter_complex").unwrap() + 1];
        assert_eq!(
            graph,
            "[1:a]asetpts=PTS-2.500000/TB,atrim=start=0,aresample=async=1:first_pts=0[s0];[s0]apad[sound]"
        );
        assert!(args.windows(2).any(|pair| pair == ["-c:v", "copy"]));
        assert!(args.windows(2).any(|pair| pair == ["-c:a", "aac"]));
        assert_eq!(args.last().unwrap(), "/tmp/out.mp4");
    }

    #[test]
    fn mixes_both_sounds_with_the_microphone_muted_for_a_while() {
        let system = Mixed { path: Path::new("/tmp/system.nut"), silent: Vec::new() };
        let microphone = Mixed { path: Path::new("/tmp/mic.nut"), silent: vec![(1.0, Some(2.5)), (4.0, None)] };
        let args = mix_args(Path::new("/tmp/video.mp4"), &[system, microphone], 2.0, Path::new("/tmp/out.mp4"));
        let graph = &args[args.iter().position(|arg| arg == "-filter_complex").unwrap() + 1];
        assert!(graph.contains("[2:a]asetpts=PTS-2.000000/TB"));
        assert!(
            graph.contains(",aeval='if(gte(t,1.000000000)*lt(t,2.500000000)+gte(t,4.000000000),0,val(ch))':c=same")
        );
        assert!(graph.ends_with("[s0][s1]amix=inputs=2:duration=longest:dropout_transition=0,volume=2,apad[sound]"));
    }

    #[test]
    fn counts_seconds_either_way() {
        let base = SystemTime::UNIX_EPOCH + Duration::from_secs(100);
        assert_eq!(seconds_between(base, base + Duration::from_millis(1500)), 1.5);
        assert_eq!(seconds_between(base, base - Duration::from_millis(500)), -0.5);
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
