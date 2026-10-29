//! The rubber duck: a small pet that drops onto the bottom of the screen and
//! waddles about. Drag it and it dangles, throw it and it falls back down.
//! Click it and it takes a look at the screen and asks what you want to know.
//!
//! Questions go to the doctor's agent with a screenshot (taken without the
//! duck in it), so the duck answers in a speech bubble and "Open in doctor"
//! carries on with the same conversation in the panel.

use std::{
    cell::RefCell,
    collections::HashMap,
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};
use smithay::{
    backend::{
        allocator::Fourcc,
        renderer::{element::memory::MemoryRenderBuffer, gles::GlesRenderer},
    },
    input::keyboard::{keysyms, xkb},
    output::Output,
    reexports::calloop::{RegistrationToken, channel},
    utils::Transform,
};
use tileception_core::{Point, Rect};

use crate::{
    Tileception,
    config::Mods,
    doctor::Message,
    render::OutputElement,
    shapes::Shape,
    text_input::TextInput,
    ui::{Align, Painter, Style, ease_out, glyph},
};

/// The duck's sprite, square, in logical pixels, at size 1.
const SIZE: f64 = 56.0;
const GRAVITY: f64 = 2400.0;
const WALK_SPEED: f64 = 42.0;
/// How often a walking, sleeping or pointing duck changes on screen. Pixel
/// art moves in steps anyway, and each change costs a frame.
const STEPS_PER_SEC: f64 = 8.0;
/// The fastest it can be thrown, in pixels per second.
const MAX_THROW: f64 = 2600.0;
/// How far the pointer moves on a press before it picks the duck up.
const DRAG_THRESHOLD: f64 = 4.0;
/// Space kept between the duck and the screen's sides.
const SIDE: f64 = 24.0;
/// An answer goes away on its own this long after it is complete, unless
/// the pointer is on it.
const ANSWER_FOR: Duration = Duration::from_secs(45);
/// Lines of an answer shown in the bubble; the rest is in the doctor.
const ANSWER_LINES: u8 = 8;
const BUBBLE_W: f64 = 340.0;
const PAD: f64 = 14.0;
const LINK_H: f64 = 22.0;
/// The screenshot sent with a question is scaled to fit this on its longest side.
const SEND_SIZE: u32 = 1568;
/// The screenshot's preview in the question bubble.
const PREVIEW: (u32, u32) = (64, 40);
/// The doctor's square thumbnail of a sent picture.
const THUMB: u32 = 56;

/// The `[duck]` section of the config.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct DuckConfig {
    /// How big the duck is: 1 is normal, 2 twice as big (0.5 to 4).
    pub size: f64,
}

impl Default for DuckConfig {
    fn default() -> Self {
        Self { size: 1.0 }
    }
}

/// How the duck stands, for its sprite.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Pose {
    Stand,
    Blink,
    Step(u8),
    Sleep,
    Held(bool),
    Fall,
    Squash,
    Listen,
    Think,
    Talk(bool),
    Point,
}

/// Whether the duck is on something, in the air or in the hand.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Motion {
    Ground,
    Air,
    Held,
}

/// What it does while left alone on the ground.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Activity {
    Idle { until: Instant },
    Walk { target: f64 },
    Sleep { until: Instant },
}

/// The conversation in the bubble.
#[derive(Debug, Clone, PartialEq)]
enum Chat {
    Closed,
    /// The question field is open.
    Asking,
    /// Asked; waiting for the screenshot to be ready before sending.
    Sending {
        question: String,
    },
    /// Sent to the doctor: the answer is what came in after `mark`.
    Asked {
        mark: usize,
        done: Option<Instant>,
    },
    /// Something Tileception says itself, such as that no agent is chosen.
    Note(String),
}

/// What the doctor has said so far to the duck's question.
#[derive(Debug, Clone, PartialEq)]
enum Reply {
    Thinking,
    Text { text: String, done: bool },
    NeedsOk,
}

/// The screen as it was when the duck was clicked, ready to send.
pub struct Shot {
    png: Arc<[u8]>,
    /// The size of the picture that is sent, for pointing back at it.
    sent: (u32, u32),
    /// The doctor's thumbnail: RGBA, width, height.
    thumb: (Vec<u8>, u32, u32),
    preview: Option<(MemoryRenderBuffer, (i32, i32))>,
}

/// A screenshot made ready off the compositor thread.
struct Prepared {
    png: Vec<u8>,
    sent: (u32, u32),
    thumb: (Vec<u8>, u32, u32),
    preview: (Vec<u8>, u32, u32),
}

/// A prepared screenshot, with the click it was taken for.
type ShotReady = (u64, Option<Prepared>);

/// Under or over, fill, border and width in pixels.
type TailKey = (bool, [u8; 4], [u8; 4], u32);

/// A press on the duck that is a click until the pointer moves.
#[derive(Debug, Clone, Copy)]
struct Press {
    origin: Point,
    /// From the duck's feet to the pointer.
    grab: (f64, f64),
}

pub struct Duck {
    pub out: bool,
    /// Left out of the frame for a moment, while the duck takes its screenshot.
    pub hidden: bool,
    placed: bool,
    /// The screen it dropped onto, where it stays until dragged to another.
    home: Option<Output>,
    /// The middle of its feet.
    pos: (f64, f64),
    vel: (f64, f64),
    left: bool,
    motion: Motion,
    activity: Activity,
    /// When the pose last changed meaning (walking, landing), for animations.
    since: Instant,
    landed: Option<Instant>,
    last_tick: Instant,
    /// Walking time not yet taken as a step.
    stride: f64,
    press: Option<Press>,
    /// The last two pointer samples while held, for the speed of a throw.
    trail: [(Point, Instant); 2],
    chat: Chat,
    input: TextInput,
    compose: Option<xkb::compose::State>,
    edited: Instant,
    shot: Option<Shot>,
    /// Screenshots asked for since the click; only the newest counts.
    shot_epoch: u64,
    shots: Option<(channel::Sender<ShotReady>, RegistrationToken)>,
    /// A place on screen the answer is about, in logical pixels.
    spot: Option<(f64, f64)>,
    /// When the answer last grew, for flapping the beak.
    grew: (usize, Instant),
    seed: u64,
    sprites: RefCell<HashMap<(Pose, bool, u32), Option<MemoryRenderBuffer>>>,
    tails: RefCell<HashMap<TailKey, Option<MemoryRenderBuffer>>>,
}

impl Default for Duck {
    fn default() -> Self {
        let now = Instant::now();
        let seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0x9e37_79b9, |since_epoch| since_epoch.as_nanos() as u64)
            | 1;
        Duck {
            out: saved_path().exists(),
            hidden: false,
            placed: false,
            home: None,
            pos: (0.0, 0.0),
            vel: (0.0, 0.0),
            left: false,
            motion: Motion::Air,
            activity: Activity::Idle { until: now },
            since: now,
            landed: None,
            last_tick: now,
            stride: 0.0,
            press: None,
            trail: [(Point::new(0.0, 0.0), now); 2],
            chat: Chat::Closed,
            input: TextInput::default(),
            compose: crate::ui::compose_state(),
            edited: now,
            shot: None,
            shot_epoch: 0,
            shots: None,
            spot: None,
            grew: (0, now),
            seed,
            sprites: Default::default(),
            tails: Default::default(),
        }
    }
}

impl Duck {
    /// The question field has the keyboard.
    pub fn typing(&self) -> bool {
        self.out && self.chat == Chat::Asking
    }

    /// A number from 0 to 1 (xorshift; the duck needs no better).
    fn random(&mut self) -> f64 {
        self.seed ^= self.seed << 13;
        self.seed ^= self.seed >> 7;
        self.seed ^= self.seed << 17;
        (self.seed >> 11) as f64 / (1u64 << 53) as f64
    }

    fn between(&mut self, low: f64, high: f64) -> f64 {
        low + (high - low) * self.random()
    }

    fn idle_for(&mut self, low: f64, high: f64) {
        let secs = self.between(low, high);
        self.activity = Activity::Idle { until: Instant::now() + Duration::from_secs_f64(secs) };
        self.since = Instant::now();
    }
}

fn saved_path() -> PathBuf {
    std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".local/state"))
        .join("tileception/duck")
}

/// Where the bubble's parts go.
struct Bubble {
    rect: Rect,
    /// Under the duck rather than above it (when it is held up high).
    below: bool,
    /// Where the tail meets the bubble.
    tail_x: f64,
    close: Option<Rect>,
    link: Option<(Rect, &'static str)>,
    field: Option<Rect>,
    preview: Option<Rect>,
    text: Option<(Rect, String)>,
    thinking: bool,
}

impl Tileception {
    /// The `duck` command: lets the duck out, or puts it away.
    pub fn toggle_duck(&mut self) {
        let duck = &mut self.duck;
        duck.out = !duck.out;
        let path = saved_path();
        let saved = if duck.out {
            path.parent().map_or(Ok(()), std::fs::create_dir_all).and_then(|_| std::fs::write(&path, "quack\n"))
        } else {
            std::fs::remove_file(&path)
                .or_else(|err| if err.kind() == std::io::ErrorKind::NotFound { Ok(()) } else { Err(err) })
        };
        if let Err(error) = saved {
            tracing::warn!("{}: {error}", path.display());
        }
        if duck.out {
            duck.placed = false;
        } else {
            self.close_duck_chat();
        }
        self.schedule_render();
    }

    /// Whether the duck is on screen now, and this is its screen.
    fn duck_shown(&self) -> bool {
        self.duck.out
            && self.duck.placed
            && self.duck.home == self.output
            && !self.is_locked()
            && self.fullscreen.is_none()
    }

    /// The configured size, as a factor on the duck's measurements.
    fn duck_scale(&self) -> f64 {
        let size = self.config.file.duck.size;
        if size.is_finite() { size.clamp(0.5, 4.0) } else { 1.0 }
    }

    /// The ground under `x_pos`: the bottom of the usable screen, or the strip of
    /// minimized windows while it is out.
    fn duck_floor(&self, x_pos: f64) -> f64 {
        let mut floor = self.world.screen().bottom() - 2.0;
        if let Some(strip) = self.strip_now()
            && x_pos >= strip.x
            && x_pos <= strip.right()
        {
            floor = floor.min(strip.y);
        }
        floor
    }

    /// Left and right ends of where the duck may be, and where it may
    /// wander (not under the doctor panel).
    fn duck_walls(&self) -> (f64, f64, f64) {
        let screen = self.output_rect();
        let side = SIDE * self.duck_scale();
        let left_wall = screen.x + side;
        let right_wall = screen.right() - side;
        let wander =
            if self.doctor_here() { (left_wall + crate::doctor::PANEL_W + 16.0).min(right_wall) } else { left_wall };
        (left_wall, right_wall, wander)
    }

    fn duck_rect(&self) -> Rect {
        let (duck_x, duck_y) = self.duck.pos;
        let scale = self.duck_scale();
        Rect::new(duck_x - 26.0 * scale, duck_y - 50.0 * scale, 52.0 * scale, 50.0 * scale)
    }

    /// Moves the duck on by the time since the last frame, on its own
    /// screen. Called after each frame.
    pub fn update_duck(&mut self) {
        let now = Instant::now();
        let delta = now.duration_since(self.duck.last_tick).as_secs_f64().min(0.05);
        self.duck.last_tick = now;
        if !self.duck.out || self.is_locked() {
            return;
        }
        let elsewhere = self.duck.home.clone().filter(|home| self.output.as_ref() != Some(home));
        if let Some(home) = &elsewhere
            && self.with_output(home, |state| state.duck_tick(delta, now)).is_some()
        {
            return;
        }
        if elsewhere.is_some() {
            // Its screen went away: it drops onto this one.
            self.duck.placed = false;
        }
        self.duck_tick(delta, now);
    }

    fn duck_tick(&mut self, delta: f64, now: Instant) {
        let screen = self.output_rect();
        if !self.duck.placed {
            self.duck.home = self.output.clone();
            // Dropped in from above, somewhere on the right.
            let drop_x = screen.x + screen.w * self.duck.between(0.55, 0.85);
            self.duck.pos = (drop_x, screen.y - SIZE * self.duck_scale());
            self.duck.vel = (0.0, 0.0);
            self.duck.motion = Motion::Air;
            self.duck.placed = true;
        }
        self.duck_chat_tick();
        let (left_wall, right_wall, wander) = self.duck_walls();
        let duck = &mut self.duck;
        match duck.motion {
            Motion::Held => {}
            Motion::Air => {
                duck.vel.1 += GRAVITY * delta;
                duck.pos.0 += duck.vel.0 * delta;
                duck.pos.1 += duck.vel.1 * delta;
                if duck.pos.0 < left_wall || duck.pos.0 > right_wall {
                    duck.pos.0 = duck.pos.0.clamp(left_wall, right_wall);
                    duck.vel.0 = -duck.vel.0 * 0.5;
                }
                if duck.vel.0.abs() > 1.0 {
                    duck.left = duck.vel.0 < 0.0;
                }
                let floor = self.duck_floor(self.duck.pos.0);
                let duck = &mut self.duck;
                if duck.pos.1 >= floor {
                    duck.pos.1 = floor;
                    if duck.vel.1 > 700.0 {
                        // A little bounce.
                        duck.vel = (duck.vel.0 * 0.6, -duck.vel.1 * 0.28);
                    } else {
                        duck.vel = (0.0, 0.0);
                        duck.motion = Motion::Ground;
                        duck.landed = Some(now);
                        duck.idle_for(1.0, 3.0);
                    }
                }
            }
            Motion::Ground => self.duck_on_ground_tick(delta, now, (left_wall, right_wall, wander)),
        }
    }

    /// Walking, idling and sleeping, and falling off when the ground goes.
    fn duck_on_ground_tick(&mut self, delta: f64, now: Instant, (left_wall, right_wall, wander): (f64, f64, f64)) {
        let floor = self.duck_floor(self.duck.pos.0);
        let scale = self.duck_scale();
        let duck = &mut self.duck;
        duck.pos.0 = duck.pos.0.clamp(left_wall, right_wall);
        if duck.pos.1 < floor - 0.5 {
            // The ground went (the strip slid away): fall.
            duck.motion = Motion::Air;
            duck.vel = (0.0, 0.0);
            return;
        }
        // The ground came up under it (the strip slid out): ride along.
        duck.pos.1 = floor;
        let talking = duck.chat != Chat::Closed;
        if talking {
            // Stay put while talking, apart from going to show the spot.
            match duck.spot {
                Some((spot_x, _)) => {
                    let target = (spot_x + (if spot_x > duck.pos.0 { -36.0 } else { 36.0 }) * scale)
                        .clamp(left_wall, right_wall);
                    if (target - duck.pos.0).abs() > 2.0 {
                        if !matches!(duck.activity, Activity::Walk { .. }) {
                            duck.since = now;
                        }
                        duck.activity = Activity::Walk { target };
                    } else {
                        duck.left = spot_x < duck.pos.0;
                        duck.activity = Activity::Idle { until: now + Duration::from_secs(5) };
                    }
                }
                None => {
                    if !matches!(duck.activity, Activity::Idle { .. }) {
                        duck.activity = Activity::Idle { until: now + Duration::from_secs(5) };
                    }
                }
            }
        }
        match duck.activity {
            Activity::Walk { target } => {
                // In whole steps, a few times a second, except when hurrying
                // to show the spot.
                duck.stride += delta;
                if !talking && duck.stride < 1.0 / STEPS_PER_SEC {
                    return;
                }
                let step = WALK_SPEED * scale * std::mem::take(&mut duck.stride) * if talking { 4.0 } else { 1.0 };
                let remaining = target - duck.pos.0;
                if remaining.abs() <= step {
                    duck.pos.0 = target;
                    if !talking {
                        duck.idle_for(2.0, 7.0);
                    }
                } else {
                    duck.left = remaining < 0.0;
                    duck.pos.0 += step * remaining.signum();
                }
            }
            Activity::Idle { until } if now >= until && !talking => {
                let roll = duck.random();
                if duck.pos.0 < wander {
                    // Out from under the doctor panel.
                    let target = duck.between(wander, (wander + 200.0).min(right_wall));
                    duck.activity = Activity::Walk { target };
                    duck.since = now;
                } else if roll < 0.6 {
                    let distance = duck.between(40.0, 320.0);
                    let way = if duck.random() < 0.5 { -1.0 } else { 1.0 };
                    let mut target = duck.pos.0 + way * distance;
                    if target < wander || target > right_wall {
                        target = duck.pos.0 - way * distance;
                    }
                    duck.activity = Activity::Walk { target: target.clamp(wander, right_wall) };
                    duck.since = now;
                } else if roll < 0.72 {
                    let secs = duck.between(15.0, 40.0);
                    duck.activity = Activity::Sleep { until: now + Duration::from_secs_f64(secs) };
                    duck.since = now;
                } else {
                    duck.idle_for(2.0, 6.0);
                }
            }
            Activity::Sleep { until } if now >= until || talking => duck.idle_for(1.0, 3.0),
            _ => {}
        }
    }

    /// Follows the doctor's answer: sends a question once the screenshot is
    /// ready, finds the spot it points at, and puts the answer away after a while.
    fn duck_chat_tick(&mut self) {
        match self.duck.chat.clone() {
            Chat::Sending { question } if self.duck.shot.is_some() => self.duck_send(question),
            Chat::Asked { mark, done } => {
                let reply = self.duck_reply(mark);
                let now = Instant::now();
                match &reply {
                    Some(Reply::Text { text, done: finished }) => {
                        if text.len() != self.duck.grew.0 {
                            self.duck.grew = (text.len(), now);
                        }
                        if *finished && done.is_none() {
                            self.duck.chat = Chat::Asked { mark, done: Some(now) };
                            self.duck.spot = self.duck_spot(text);
                        }
                    }
                    None => self.close_duck_chat(),
                    _ => {}
                }
                let over = self
                    .duck_bubble()
                    .is_some_and(|bubble| bubble.rect.contains(Point::new(self.pointer.x, self.pointer.y)));
                if let Chat::Asked { done: Some(done_at), .. } = self.duck.chat
                    && now.duration_since(done_at) > ANSWER_FOR
                    && !over
                {
                    self.close_duck_chat();
                }
            }
            _ => {}
        }
    }

    /// The answer so far, from the doctor's conversation; `None` once the
    /// conversation it was in is gone.
    fn duck_reply(&self, mark: usize) -> Option<Reply> {
        let messages = self.doctor.messages.get(mark..)?;
        if messages.iter().any(|message| matches!(message, Message::Ask { answer: None, .. })) {
            return Some(Reply::NeedsOk);
        }
        let text: Vec<&str> = messages
            .iter()
            .filter_map(|message| match message {
                Message::Agent(text) => Some(text.as_str()),
                _ => None,
            })
            .collect();
        let busy = self.doctor.busy;
        if text.is_empty() {
            if busy {
                return Some(Reply::Thinking);
            }
            // Ended without words: what Tileception said about it, if anything.
            let notes: Vec<&str> = messages
                .iter()
                .filter_map(|message| match message {
                    Message::Note(text) => Some(text.as_str()),
                    _ => None,
                })
                .collect();
            let text = if notes.is_empty() { "Hmm, I've got nothing.".to_string() } else { notes.join("\n") };
            return Some(Reply::Text { text, done: true });
        }
        Some(Reply::Text { text: text.join("\n\n"), done: !busy })
    }

    /// The `point: X,Y` line an answer may end with, as a place on screen.
    fn duck_spot(&self, text: &str) -> Option<(f64, f64)> {
        let (point_x, point_y) = parse_point(text)?;
        let (shot_width, shot_height) = self.duck.shot.as_ref()?.sent;
        let screen = self.output_rect();
        let screen_x = screen.x + point_x / f64::from(shot_width) * screen.w;
        let screen_y = screen.y + point_y / f64::from(shot_height) * screen.h;
        screen.contains(Point::new(screen_x, screen_y)).then_some((screen_x, screen_y))
    }

    fn close_duck_chat(&mut self) {
        let typing = self.duck.typing();
        self.duck.chat = Chat::Closed;
        self.duck.spot = None;
        self.duck.input.clear();
        if typing {
            self.force_keyboard_focus_sync();
        }
        self.schedule_render();
    }

    /// A click on the duck: it looks at the screen and asks what you want to know.
    fn duck_clicked(&mut self) {
        if self.duck.chat == Chat::Asking {
            return self.close_duck_chat();
        }
        if matches!(self.duck.chat, Chat::Sending { .. }) || self.doctor.busy {
            // Still on the last question; the bubble says so.
            return;
        }
        self.duck.chat = Chat::Asking;
        self.duck.spot = None;
        self.duck.input.clear();
        self.duck.edited = Instant::now();
        self.duck.shot = None;
        self.duck.shot_epoch += 1;
        if matches!(self.duck.activity, Activity::Sleep { .. }) {
            self.duck.idle_for(1.0, 2.0);
        }
        self.duck.left = self.pointer.x < self.duck.pos.0;
        self.duck_screenshot();
        if self.doctor.open && self.doctor.focused {
            self.doctor.focused = false;
        }
        self.force_keyboard_focus_sync();
        self.schedule_render();
    }

    /// The screen without the duck, from the next frame (see `capture.rs`).
    pub fn duck_screen_taken(&mut self, rgba: Vec<u8>, (width, height): (u32, u32)) {
        let epoch = self.duck.shot_epoch;
        let Some(sender) = self.duck_shots() else { return };
        std::thread::spawn(move || {
            let _ = sender.send((epoch, prepare_shot(rgba, width, height)));
        });
    }

    fn duck_shots(&mut self) -> Option<channel::Sender<ShotReady>> {
        if self.duck.shots.is_none() {
            let (sender, receiver) = channel::channel::<ShotReady>();
            let token = self
                .loop_handle
                .insert_source(receiver, |event, _, state| {
                    if let channel::Event::Msg((epoch, prepared)) = event {
                        state.duck_shot_ready(epoch, prepared);
                    }
                })
                .map_err(|err| tracing::warn!("duck screenshots: {err}"))
                .ok()?;
            self.duck.shots = Some((sender, token));
        }
        Some(self.duck.shots.as_ref()?.0.clone())
    }

    fn duck_shot_ready(&mut self, epoch: u64, prepared: Option<Prepared>) {
        if epoch != self.duck.shot_epoch {
            return;
        }
        let Some(prepared) = prepared else {
            self.duck.chat = Chat::Note("I couldn't see the screen just now.".into());
            self.schedule_render();
            return;
        };
        let (rgba, width, height) = prepared.preview;
        let size = (width as i32, height as i32);
        let preview = MemoryRenderBuffer::from_slice(&rgba, Fourcc::Abgr8888, size, 1, Transform::Normal, None);
        self.duck.shot = Some(Shot {
            png: prepared.png.into(),
            sent: prepared.sent,
            thumb: prepared.thumb,
            preview: Some((preview, size)),
        });
        self.schedule_render();
    }

    /// Sends the question with the screenshot to the doctor's agent.
    fn duck_send(&mut self, question: String) {
        let Some(shot) = &self.duck.shot else { return };
        let (width, height) = shot.sent;
        let prompt = format!(
            "[Asked through Tileception's rubber duck. A screenshot of the whole screen is attached \
             ({width}x{height} pixels). Answer as a friendly rubber duck: short, at most four sentences, plain \
             text, no code blocks unless they are essential. If the answer is about one place on the \
             screen, end with a line `point: X,Y` with that place in the screenshot's pixels.]\n\n{question}"
        );
        let thumb = shot.thumb.clone();
        let png = shot.png.clone();
        self.duck.chat = match self.doctor_ask_from_duck(question, prompt, png, thumb) {
            Ok(mark) => Chat::Asked { mark, done: None },
            Err(note) => Chat::Note(note.into()),
        };
        self.duck.grew = (0, Instant::now());
        self.schedule_render();
    }

    /// A key while the question field has the keyboard.
    pub fn duck_key(&mut self, sym: u32, mods: Mods) {
        self.duck.edited = Instant::now();
        let word = mods.ctrl || mods.alt;
        let input = &mut self.duck.input;
        match sym {
            keysyms::KEY_Escape => self.close_duck_chat(),
            keysyms::KEY_Return | keysyms::KEY_KP_Enter => {
                let question = input.text.trim().to_string();
                if question.is_empty() {
                    return;
                }
                input.clear();
                self.duck.chat = Chat::Sending { question };
                self.force_keyboard_focus_sync();
                self.duck_chat_tick();
            }
            keysyms::KEY_Left => input.move_horizontal(false, word, mods.shift),
            keysyms::KEY_Right => input.move_horizontal(true, word, mods.shift),
            keysyms::KEY_Home => input.move_to(0, mods.shift),
            keysyms::KEY_End => input.move_to(input.text.len(), mods.shift),
            keysyms::KEY_BackSpace => input.delete(false, word),
            keysyms::KEY_Delete => input.delete(true, word),
            _ if mods.ctrl && matches!(sym, keysyms::KEY_a | keysyms::KEY_A) => input.select_all(),
            _ if mods.ctrl && matches!(sym, keysyms::KEY_u | keysyms::KEY_U) => input.clear(),
            _ if !mods.ctrl && !mods.logo && !mods.alt => {
                if let Some(text) = crate::ui::compose_text(&mut self.duck.compose, sym) {
                    self.duck.input.insert(&text);
                }
            }
            _ => {}
        }
        self.schedule_render();
    }

    /// The duck is pressed or held.
    pub fn duck_pressed(&self) -> bool {
        self.duck.press.is_some()
    }

    /// Pointer motion while the duck is pressed or held. True when it took it.
    pub fn duck_motion(&mut self, position: Point) -> bool {
        let Some(press) = self.duck.press else { return false };
        let now = Instant::now();
        if self.duck.motion != Motion::Held {
            if (position.x - press.origin.x).hypot(position.y - press.origin.y) < DRAG_THRESHOLD {
                return true;
            }
            self.duck.motion = Motion::Held;
            self.duck.since = now;
            self.duck.trail = [(position, now); 2];
        }
        // Dragged across screens, it moves to the one under the pointer,
        // whose coordinates `position` is in.
        self.duck.home = self.output.clone();
        self.duck.pos = (position.x - press.grab.0, position.y - press.grab.1);
        if (position.x - self.duck.trail[1].0.x).abs() > 0.5 {
            self.duck.left = position.x < self.duck.trail[1].0.x;
        }
        if now.duration_since(self.duck.trail[1].1) > Duration::from_millis(8) {
            self.duck.trail = [self.duck.trail[1], (position, now)];
        }
        true
    }

    /// A button over the duck or its bubble. True when the duck took it.
    pub fn duck_button(&mut self, left: bool, pressed: bool, position: Point) -> bool {
        if !self.duck_shown() {
            self.duck.press = None;
            return false;
        }
        if !pressed {
            if self.duck.press.take().filter(|_| left).is_none() {
                return false;
            }
            if self.duck.motion == Motion::Held {
                self.duck_thrown();
            } else {
                self.duck_clicked();
            }
            return true;
        }
        if let Some(bubble) = self.duck_bubble() {
            if let Some((link, _)) = bubble.link
                && link.contains(position)
            {
                if left {
                    self.close_duck_chat();
                    self.open_doctor();
                }
                return true;
            }
            if bubble.close.is_some_and(|close| close.contains(position)) {
                if left {
                    self.close_duck_chat();
                }
                return true;
            }
            if bubble.rect.contains(position) {
                return true;
            }
        }
        if self.duck_rect().contains(position) {
            if left {
                self.duck.press = Some(Press {
                    origin: position,
                    grab: (position.x - self.duck.pos.0, position.y - self.duck.pos.1),
                });
            }
            return true;
        }
        // A click elsewhere puts the question or answer away, and goes on.
        if self.duck.chat != Chat::Closed && !matches!(self.duck.chat, Chat::Sending { .. }) {
            if matches!(self.duck.chat, Chat::Asked { done: None, .. }) {
                // Still answering: stays, so the answer is not missed.
                return false;
            }
            self.close_duck_chat();
        }
        false
    }

    fn duck_thrown(&mut self) {
        let [(start, start_time), (end, end_time)] = self.duck.trail;
        let elapsed = end_time.duration_since(start_time).as_secs_f64();
        let fresh = end_time.elapsed() < Duration::from_millis(80);
        let (mut vel_x, mut vel_y) = if elapsed > 0.0 && fresh {
            ((end.x - start.x) / elapsed, (end.y - start.y) / elapsed)
        } else {
            (0.0, 0.0)
        };
        let speed = vel_x.hypot(vel_y);
        if speed > MAX_THROW {
            vel_x *= MAX_THROW / speed;
            vel_y *= MAX_THROW / speed;
        }
        self.duck.vel = (vel_x, vel_y);
        self.duck.motion = Motion::Air;
        self.duck.since = Instant::now();
    }

    /// The bubble for what the duck is saying, laid out.
    fn duck_bubble(&self) -> Option<Bubble> {
        if !self.duck_shown() {
            return None;
        }
        let theme = &self.theme;
        let screen = self.output_rect();
        let mut text = None;
        let mut link = None;
        let mut field = None;
        let mut preview = None;
        let mut close = None;
        let mut thinking = false;
        let (width, height) = match &self.duck.chat {
            Chat::Closed => return None,
            Chat::Asking => (BUBBLE_W, PAD * 2.0 + f64::from(PREVIEW.1)),
            Chat::Sending { .. } => {
                thinking = true;
                (76.0, 40.0)
            }
            Chat::Asked { mark, .. } => match self.duck_reply(*mark)? {
                Reply::Thinking => {
                    thinking = true;
                    (76.0, 40.0)
                }
                Reply::NeedsOk => {
                    link = Some("Open the doctor \u{2192}");
                    text = Some("I need your OK for that.".to_string());
                    (0.0, 0.0)
                }
                Reply::Text { text: answer, .. } => {
                    let shown = plain(&answer);
                    let style = Style::new(theme.font_size, theme.text).lines(ANSWER_LINES);
                    let (_, full_h) = self.duck_measure(&shown, BUBBLE_W - PAD * 2.0, style.lines(60));
                    let (_, cut_h) = self.duck_measure(&shown, BUBBLE_W - PAD * 2.0, style);
                    let more = full_h > cut_h + 1.0 || answer.contains("```");
                    link = Some(if more { "More in the doctor \u{2192}" } else { "Open in doctor \u{2192}" });
                    text = Some(shown);
                    (0.0, 0.0)
                }
            },
            Chat::Note(note) => {
                link = Some("Open the doctor \u{2192}");
                text = Some(note.clone());
                (0.0, 0.0)
            }
        };
        // Text bubbles fit their text.
        let (width, height) = match &text {
            Some(message) => {
                let style = Style::new(theme.font_size, theme.text).lines(ANSWER_LINES);
                let (text_width, text_height) = self.duck_measure(message, BUBBLE_W - PAD * 2.0 - 20.0, style);
                ((text_width + PAD * 2.0 + 20.0).clamp(200.0, BUBBLE_W), text_height + PAD * 2.0 + LINK_H + 4.0)
            }
            None => (width, height),
        };
        let (duck_x, duck_y) = self.duck.pos;
        let head = duck_x + if self.duck.left { -8.0 } else { 8.0 };
        let gap = if thinking { 30.0 } else { 12.0 };
        let mut below = false;
        let mut top = duck_y - SIZE * self.duck_scale() - gap - height;
        if top < screen.y + 8.0 {
            below = true;
            top = duck_y + gap;
        }
        let left = (head - width * if self.duck.left { 0.75 } else { 0.25 })
            .clamp(screen.x + 8.0, screen.right() - 8.0 - width);
        let rect = Rect::new(left, top, width, height);
        let tail_x = head.clamp(rect.x + 18.0, rect.right() - 18.0);
        let inner = Rect::new(rect.x + PAD, rect.y + PAD, rect.w - PAD * 2.0, rect.h - PAD * 2.0);
        match &self.duck.chat {
            Chat::Asking => {
                let (preview_width, preview_height) = (f64::from(PREVIEW.0), f64::from(PREVIEW.1));
                preview = Some(Rect::new(inner.x, inner.y, preview_width, preview_height));
                field = Some(Rect::new(
                    inner.x + preview_width + 12.0,
                    inner.y,
                    inner.w - preview_width - 12.0,
                    preview_height,
                ));
            }
            _ if text.is_some() => {
                close = Some(Rect::new(rect.right() - 28.0, rect.y + 6.0, 22.0, 22.0));
                let body = Rect::new(inner.x, inner.y, inner.w - 20.0, inner.h - LINK_H - 4.0);
                let link_rect = Rect::new(inner.x, inner.bottom() - LINK_H, inner.w, LINK_H);
                return Some(Bubble {
                    rect,
                    below,
                    tail_x,
                    close,
                    link: link.map(|label| (link_rect, label)),
                    field,
                    preview,
                    text: text.map(|message| (body, message)),
                    thinking,
                });
            }
            _ => {}
        }
        Some(Bubble { rect, below, tail_x, close, link: None, field, preview, text: None, thinking })
    }

    fn duck_measure(&self, label: &str, max_width: f64, style: Style) -> (f64, f64) {
        self.text
            .borrow_mut()
            .styled(
                label,
                &self.theme.font,
                style.size,
                style.colour,
                max_width.max(1.0) as f32,
                style.weight,
                style.lines,
            )
            .map(|image| (image.width, image.height))
            .unwrap_or((0.0, 0.0))
    }

    /// The pose for this frame.
    fn duck_pose(&self) -> Pose {
        let duck = &self.duck;
        let now = Instant::now();
        let elapsed = now.duration_since(duck.since).as_secs_f64();
        match duck.motion {
            Motion::Held => return Pose::Held(((elapsed * 7.0) as u64).is_multiple_of(2)),
            Motion::Air => return Pose::Fall,
            Motion::Ground => {}
        }
        if duck.landed.is_some_and(|landed_at| now.duration_since(landed_at) < Duration::from_millis(140)) {
            return Pose::Squash;
        }
        if let Activity::Walk { .. } = duck.activity {
            return Pose::Step(((elapsed * 8.0) as u64 % 4) as u8);
        }
        match &duck.chat {
            Chat::Asking => return Pose::Listen,
            Chat::Sending { .. } => return Pose::Think,
            Chat::Asked { mark, .. } => match self.duck_reply(*mark) {
                Some(Reply::Thinking) => return Pose::Think,
                Some(Reply::Text { done: false, .. })
                    if now.duration_since(duck.grew.1) < Duration::from_millis(400) =>
                {
                    return Pose::Talk(((elapsed * 8.0) as u64).is_multiple_of(2));
                }
                _ if duck.spot.is_some() => return Pose::Point,
                _ => {}
            },
            _ => {}
        }
        if let Activity::Sleep { .. } = duck.activity {
            return Pose::Sleep;
        }
        // A blink now and then.
        if (now.duration_since(self.start).as_millis() % 3700) < 130 { Pose::Blink } else { Pose::Stand }
    }

    pub fn duck_elements(&self, renderer: &mut GlesRenderer) -> Vec<OutputElement> {
        if self.duck.hidden || !self.duck_shown() {
            return Vec::new();
        }
        let theme = &self.theme;
        let pose = self.duck_pose();
        let (duck_x, duck_y) = self.duck.pos;
        let scale = self.duck_scale();
        let size = SIZE * scale;
        let bubble = self.duck_bubble();
        let mut painter = Painter::new(self, renderer);

        // The place an answer is about: a soft ring that breathes.
        if let Some((spot_x, spot_y)) = self.duck.spot.filter(|_| self.duck.chat != Chat::Closed) {
            let phase = (stepped(self.start.elapsed()) * 2.5).sin() * 0.5 + 0.5;
            let radius = 18.0 + phase * 6.0;
            painter.shape(
                Rect::new(spot_x - radius, spot_y - radius, radius * 2.0, radius * 2.0),
                Shape {
                    radius: radius as f32,
                    border_width: 3.0,
                    border: theme.accent,
                    glow_radius: 18.0,
                    glow: theme.accent.alpha(0.35),
                    ..Default::default()
                },
            );
        }

        if let Some(sprite) = self.duck_sprite(pose) {
            painter.image(&sprite, (duck_x - size / 2.0, duck_y - size));
        }

        if pose == Pose::Sleep {
            // Zs drifting up from its head.
            let secs = stepped(self.duck.since.elapsed());
            for index in 0..3 {
                let progress = (secs * 0.25 + f64::from(index) / 3.0).fract();
                // A few sizes in one colour, faded as a whole: each new size
                // or colour would be text rendered again.
                let font = theme.font_size * (0.8 + (progress * 4.0).floor() as f32 / 8.0);
                let side = if self.duck.left { -1.0 } else { 1.0 };
                let z_x = duck_x + side * (12.0 + progress * 14.0) * scale - 6.0;
                let z_y = duck_y - size - progress * 26.0;
                painter.alpha = ((1.0 - progress) * ease_out(progress * 4.0)) as f32;
                painter.text("z", Rect::new(z_x, z_y, 20.0, 20.0), Style::new(font, theme.text).bold());
            }
            painter.alpha = 1.0;
        }

        if let Some(bubble) = bubble {
            self.draw_duck_bubble(&mut painter, &bubble);
        }
        painter.finish()
    }

    fn draw_duck_bubble(&self, painter: &mut Painter, bubble: &Bubble) {
        let theme = &self.theme;
        let (duck_x, duck_y) = self.duck.pos;
        if bubble.thinking {
            // Little thought circles up to the bubble.
            let scale = self.duck_scale();
            let head = (duck_x + (if self.duck.left { -10.0 } else { 10.0 }) * scale, duck_y - (SIZE - 6.0) * scale);
            let target = (bubble.tail_x, if bubble.below { bubble.rect.y } else { bubble.rect.bottom() });
            for (along, radius) in [(0.35, 3.5), (0.7, 5.5)] {
                let circle_x = head.0 + (target.0 - head.0) * along;
                let circle_y = head.1 + (target.1 - head.1) * along;
                painter.shape(
                    Rect::new(circle_x - radius, circle_y - radius, radius * 2.0, radius * 2.0),
                    Shape { border_width: 1.0, border: theme.border, ..Shape::filled(theme.popup(), radius as f32) },
                );
            }
        }
        painter.popup(bubble.rect);
        if !bubble.thinking
            && let Some(tail) = self.duck_tail(bubble.below)
        {
            let tail_at = if bubble.below {
                (bubble.tail_x - 10.0, bubble.rect.y - 11.0)
            } else {
                (bubble.tail_x - 10.0, bubble.rect.bottom() - 1.0)
            };
            painter.image(&tail, tail_at);
        }
        if bubble.thinking {
            // Three dots taking turns.
            let secs = self.start.elapsed().as_secs_f64();
            for index in 0..3 {
                let bounce = ((secs * 3.0 - f64::from(index) * 0.4).sin() * 0.5 + 0.5) as f32;
                let dot_x = bubble.rect.x + bubble.rect.w / 2.0 + (f64::from(index) - 1.0) * 14.0;
                let dot_y = bubble.rect.y + bubble.rect.h / 2.0 - f64::from(bounce) * 3.0;
                painter.fill(
                    Rect::new(dot_x - 4.0, dot_y - 4.0, 8.0, 8.0),
                    theme.muted.alpha(0.45 + 0.55 * bounce),
                    4.0,
                );
            }
            return;
        }
        if let Some(rect) = bubble.preview {
            match self.duck.shot.as_ref().and_then(|shot| shot.preview.as_ref()) {
                Some((buffer, size)) => {
                    let preview_at =
                        (rect.x + (rect.w - f64::from(size.0)) / 2.0, rect.y + (rect.h - f64::from(size.1)) / 2.0);
                    painter.fill(
                        Rect::new(
                            preview_at.0 - 1.0,
                            preview_at.1 - 1.0,
                            f64::from(size.0) + 2.0,
                            f64::from(size.1) + 2.0,
                        ),
                        theme.border,
                        3.0,
                    );
                    painter.image_sized(buffer, preview_at, Some(*size));
                }
                None => {
                    let pulse = (self.start.elapsed().as_secs_f64() * 4.0).sin() * 0.5 + 0.5;
                    painter.fill(rect, theme.surface_raised.alpha(0.6 + 0.4 * pulse as f32), 4.0);
                    painter.glyph(glyph::CAMERA, rect, 16, theme.muted);
                }
            }
        }
        if let Some(rect) = bubble.field {
            let size = theme.font_size;
            let line = Rect::new(rect.x, rect.y, rect.w, 26.0);
            if self.duck.input.text.is_empty() {
                painter.text("What do you want to know?", line, Style::new(size, theme.muted));
            }
            let field = self.text.borrow_mut().field(
                &self.duck.input.text,
                &theme.font,
                size,
                theme.text,
                line.w as f32,
                crate::text::Field {
                    cursor: self.duck.input.cursor,
                    anchor: self.duck.input.anchor,
                    visible: self.duck.edited.elapsed().as_millis() % 1000 < 600,
                    accent: theme.accent.straight_rgba8(),
                },
            );
            if let Some(image) = field {
                painter.image(&image.buffer, (line.x, line.y + (line.h - image.height) / 2.0));
            }
            painter.fill(Rect::new(rect.x, line.bottom() + 2.0, rect.w, 1.0), theme.accent.alpha(0.6), 0.0);
            let hints = Rect::new(rect.x, line.bottom() + 4.0, rect.w, rect.h - line.h - 4.0);
            painter.text(
                "I can see your screen \u{00b7} Enter asks, Esc closes",
                hints,
                Style::new(theme.font_size * 0.8, theme.muted),
            );
        }
        if let Some((rect, text)) = &bubble.text {
            painter.text(text, *rect, Style::new(theme.font_size, theme.text).lines(ANSWER_LINES));
        }
        let pointer = Point::new(self.pointer.x, self.pointer.y);
        if let Some(rect) = bubble.close {
            let hover = rect.contains(pointer);
            if hover {
                painter.fill(rect, theme.surface_raised, 11.0);
            }
            painter.glyph(glyph::CLOSE, rect, 12, if hover { theme.text } else { theme.muted });
        }
        if let Some((rect, label)) = bubble.link {
            let style = Style::new(theme.font_size * 0.9, theme.accent).bold();
            let (label_width, _) = painter.text(label, rect, style.align(Align::Left));
            if rect.contains(pointer) {
                painter.fill(Rect::new(rect.x, rect.y + rect.h / 2.0 + 9.0, label_width, 1.0), theme.accent, 0.0);
            }
        }
    }

    /// The duck's sprite for a pose, facing its way, at the output's scale.
    fn duck_sprite(&self, pose: Pose) -> Option<MemoryRenderBuffer> {
        let pixels = (SIZE * self.duck_scale() * self.scale).round().max(1.0) as u32;
        let key = (pose, self.duck.left, pixels);
        if let Some(sprite) = self.duck.sprites.borrow().get(&key) {
            return sprite.clone();
        }
        let rgba = duck_pixels(pose, self.duck.left, pixels);
        let size = (pixels as i32, pixels as i32);
        let sprite = Some(MemoryRenderBuffer::from_slice(&rgba, Fourcc::Abgr8888, size, 1, Transform::Normal, None));
        let mut sprites = self.duck.sprites.borrow_mut();
        if sprites.len() > 64 {
            sprites.clear();
        }
        sprites.insert(key, sprite.clone());
        sprite
    }

    /// The speech bubble's tail, in the theme's colours.
    fn duck_tail(&self, below: bool) -> Option<MemoryRenderBuffer> {
        let theme = &self.theme;
        let fill = theme.popup().straight_rgba8();
        let border = theme.border.straight_rgba8();
        let (width, height) = ((20.0 * self.scale).round() as u32, (12.0 * self.scale).round() as u32);
        let key = (below, fill, border, width);
        if let Some(tail) = self.duck.tails.borrow().get(&key) {
            return tail.clone();
        }
        let flip = if below { r#" transform="translate(0 12) scale(1 -1)""# } else { "" };
        let svg = format!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 20 12"><g{flip}><path d="M0 0L10 11L20 0Z" fill="{}"/><path d="M1 0.5L10 11L19 0.5" fill="none" stroke="{}" stroke-width="1"/></g></svg>"#,
            css(fill),
            css(border),
        );
        let tail = rasterise(&svg, width, height);
        self.duck.tails.borrow_mut().insert(key, tail.clone());
        tail
    }
}

/// Seconds of `elapsed`, in whole steps.
fn stepped(elapsed: Duration) -> f64 {
    (elapsed.as_secs_f64() * STEPS_PER_SEC).floor() / STEPS_PER_SEC
}

fn css([red, green, blue, alpha]: [u8; 4]) -> String {
    format!("rgba({red},{green},{blue},{:.3})", f32::from(alpha) / 255.0)
}

fn rasterise(svg: &str, width: u32, height: u32) -> Option<MemoryRenderBuffer> {
    let tree = resvg::usvg::Tree::from_str(svg, &resvg::usvg::Options::default()).ok()?;
    let mut pixmap = resvg::tiny_skia::Pixmap::new(width, height)?;
    let scale = (width as f32 / tree.size().width()).min(height as f32 / tree.size().height());
    resvg::render(&tree, resvg::tiny_skia::Transform::from_scale(scale, scale), &mut pixmap.as_mut());
    // tiny-skia pixels are premultiplied RGBA, which is what the renderer wants.
    Some(MemoryRenderBuffer::from_slice(
        pixmap.data(),
        Fourcc::Abgr8888,
        (width as i32, height as i32),
        1,
        Transform::Normal,
        None,
    ))
}

/// The duck as pixel art, 16 by 16 and facing right, standing on the last
/// row. One letter per pixel; see `paint`.
const BASE: [&str; GRID] = [
    "................",
    "................",
    "......DDDDD.....",
    ".....DYYYYYD....",
    "....DYLLYYYYD...",
    "....DYLYYYEYD...",
    "....DYYYYYEYOOD.",
    "....DYYYYYYOOOOD",
    "....DYYBYYYDOOD.",
    ".DD..DYYYYYDDD..",
    "DYYDDYYYYYYYD...",
    "DYYYYYYYYYYYYD..",
    ".DYYYSSSSYYYYD..",
    ".DYYYYSSSSYYYD..",
    "..DSYYYYYYYYSD..",
    "...DDDDDDDDDD...",
];
const GRID: usize = 16;

/// Each pose is its own drawing: pixel art can't be turned or nudged by
/// fractions of a pixel without smearing, so poses swap rows instead.
fn frame(pose: Pose) -> [[u8; GRID]; GRID] {
    let mut grid = BASE.map(|row| <[u8; GRID]>::try_from(row.as_bytes()).expect("rows are 16 wide"));
    let set = |grid: &mut [[u8; GRID]; GRID], rows: &[(usize, &str)]| {
        for (row_index, row) in rows {
            grid[*row_index].copy_from_slice(row.as_bytes());
        }
    };
    let eyes_shut = |grid: &mut [[u8; GRID]; GRID]| set(grid, &[(5, "....DYLYYYYYD..."), (6, "....DYYYYDDYOOD.")]);
    let eyes_up = |grid: &mut [[u8; GRID]; GRID]| {
        set(grid, &[(4, "....DYLLYYEYD..."), (5, "....DYLYYYEYD..."), (6, "....DYYYYYYYOOD.")])
    };
    let beak_open = |grid: &mut [[u8; GRID]; GRID]| {
        set(grid, &[(5, "....DYLYYYEYOOD."), (6, "....DYYYYYEYD..."), (7, "....DYYYYYYD...."), (8, "....DYYBYYYDOOD.")])
    };
    let wing_up = |grid: &mut [[u8; GRID]; GRID]| {
        set(
            grid,
            &[(10, "DYYDDSSSSYYYD..."), (11, "DYYYYYSSSSYYYD.."), (12, ".DYYYYYYYYYYYD.."), (13, ".DYYYYYYYYYYYD..")],
        )
    };
    // Up off the ground by `count` rows.
    let lift = |grid: &mut [[u8; GRID]; GRID], count: usize| {
        grid.rotate_left(count);
        grid[GRID - count..].fill([b'.'; GRID]);
    };
    // Flattened by dropping the plain row across the body.
    let squash = |grid: &mut [[u8; GRID]; GRID]| {
        grid.copy_within(0..11, 1);
        grid[0] = [b'.'; GRID];
    };
    match pose {
        Pose::Stand => {}
        Pose::Blink | Pose::Sleep => eyes_shut(&mut grid),
        // A hop: crouch, up, top, down.
        Pose::Step(0) => squash(&mut grid),
        Pose::Step(2) => lift(&mut grid, 2),
        Pose::Step(_) => lift(&mut grid, 1),
        Pose::Held(flap) => {
            if flap {
                wing_up(&mut grid);
                beak_open(&mut grid);
            }
        }
        Pose::Fall => {
            wing_up(&mut grid);
            beak_open(&mut grid);
        }
        Pose::Squash => squash(&mut grid),
        Pose::Listen => eyes_up(&mut grid),
        Pose::Think => {
            eyes_up(&mut grid);
            wing_up(&mut grid);
        }
        Pose::Talk(open) => {
            if open {
                beak_open(&mut grid);
            }
        }
        Pose::Point => wing_up(&mut grid),
    }
    grid
}

/// A pixel's colour, as RGBA.
fn paint(letter: u8) -> Option<[u8; 4]> {
    Some(match letter {
        b'D' => [0x3a, 0x2a, 0x0a, 0xff], // outline
        b'Y' => [0xff, 0xd9, 0x3b, 0xff], // body
        b'L' => [0xff, 0xf6, 0xb8, 0xff], // shine
        b'S' => [0xe8, 0xb4, 0x16, 0xff], // wing
        b'O' => [0xff, 0x8a, 0x1f, 0xff], // beak
        b'E' => [0x1a, 0x1a, 0x1a, 0xff], // eye
        b'B' => [0xff, 0x8f, 0xa3, 0xff], // blush
        _ => return None,
    })
}

/// The pose blown up to `pixels` square with hard edges, facing right (or left).
/// RGBA; every pixel is fully opaque or fully clear, so it is also premultiplied.
fn duck_pixels(pose: Pose, left: bool, pixels: u32) -> Vec<u8> {
    let grid = frame(pose);
    let pixels = pixels as usize;
    let mut rgba = vec![0; pixels * pixels * 4];
    for y_pos in 0..pixels {
        let row = &grid[y_pos * GRID / pixels];
        for x_pos in 0..pixels {
            let source_x = if left { pixels - 1 - x_pos } else { x_pos };
            let col = source_x * GRID / pixels;
            if let Some(colour) = paint(row[col]) {
                rgba[(y_pos * pixels + x_pos) * 4..][..4].copy_from_slice(&colour);
            }
        }
    }
    rgba
}

/// An answer's Markdown as plain lines for the bubble, without the
/// `point:` line.
fn plain(markdown: &str) -> String {
    use crate::markdown::{Kind, Piece};
    let source: String = markdown.lines().filter(|line| point_in(line).is_none()).collect::<Vec<_>>().join("\n");
    let mut out: Vec<String> = Vec::new();
    for piece in crate::markdown::parse(&source) {
        match piece {
            Piece::Text { kind, spans } => {
                let text: String = spans.iter().map(|span| span.text.as_str()).collect();
                out.push(match kind {
                    Kind::Item { marker, .. } if !marker.is_empty() => format!("{marker} {text}"),
                    _ => text,
                });
            }
            Piece::Code { text, .. } => out.push(text.trim_end().to_string()),
            Piece::Image { alt, .. } if !alt.is_empty() => out.push(alt),
            Piece::Image { .. } | Piece::Rule => {}
        }
    }
    out.join("\n")
}

/// The place the last `point: X,Y` line names, in screenshot pixels.
fn parse_point(text: &str) -> Option<(f64, f64)> {
    text.lines().rev().find_map(point_in)
}

fn point_in(line: &str) -> Option<(f64, f64)> {
    let line = line.trim().trim_matches(|character| character == '`' || character == '*' || character == '_').trim();
    let rest = line.get(..6).filter(|prefix| prefix.eq_ignore_ascii_case("point:")).map(|_| &line[6..])?;
    let rest = rest.trim().trim_start_matches('(').trim_end_matches(')');
    let (x_text, y_text) = rest.split_once(',')?;
    let x_pos: f64 = x_text.trim().parse().ok()?;
    let y_pos: f64 = y_text.trim().parse().ok()?;
    (x_pos.is_finite() && y_pos.is_finite() && x_pos >= 0.0 && y_pos >= 0.0).then_some((x_pos, y_pos))
}

/// The screen scaled for sending, as PNG, with its thumbnails.
fn prepare_shot(mut rgba: Vec<u8>, width: u32, height: u32) -> Option<Prepared> {
    use image::imageops::FilterType;
    // The scene has no meaningful alpha.
    for pixel in rgba.as_chunks_mut::<4>().0 {
        pixel[3] = 255;
    }
    let image = image::DynamicImage::ImageRgba8(image::RgbaImage::from_raw(width, height, rgba)?);
    let sent =
        if width.max(height) > SEND_SIZE { image.resize(SEND_SIZE, SEND_SIZE, FilterType::Triangle) } else { image };
    let mut png = Vec::new();
    image::DynamicImage::ImageRgb8(sent.to_rgb8())
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .map_err(|err| tracing::warn!("duck screenshot: {err}"))
        .ok()?;
    let thumb = sent.resize_to_fill(THUMB, THUMB, FilterType::Triangle).to_rgba8();
    let thumb = (thumb.as_raw().clone(), thumb.width(), thumb.height());
    let preview = sent.resize(PREVIEW.0, PREVIEW.1, FilterType::Triangle).to_rgba8();
    let preview = (preview.as_raw().clone(), preview.width(), preview.height());
    Some(Prepared { png, sent: (sent.width(), sent.height()), thumb, preview })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_point_line() {
        assert_eq!(parse_point("It's the gear icon.\npoint: 120, 40"), Some((120.0, 40.0)));
        assert_eq!(parse_point("There.\n`point: 7,8`"), Some((7.0, 8.0)));
        assert_eq!(parse_point("Point: (3, 4)"), Some((3.0, 4.0)));
        assert_eq!(parse_point("No place here."), None);
        assert_eq!(parse_point("point: left, top"), None);
        assert_eq!(parse_point("point: -3, 4"), None);
    }

    #[test]
    fn plain_text_drops_the_point_line() {
        let text = plain("Click **Settings**.\n\n- one\n- two\n\npoint: 10,20");
        assert!(text.starts_with("Click Settings."), "{text}");
        assert!(text.contains("one"), "{text}");
        assert!(!text.contains("point"), "{text}");
    }

    #[test]
    fn every_pose_draws() {
        let poses = [
            Pose::Stand,
            Pose::Blink,
            Pose::Step(0),
            Pose::Step(1),
            Pose::Step(2),
            Pose::Step(3),
            Pose::Sleep,
            Pose::Held(true),
            Pose::Held(false),
            Pose::Fall,
            Pose::Squash,
            Pose::Listen,
            Pose::Think,
            Pose::Talk(true),
            Pose::Point,
        ];
        for pose in poses {
            let grid = frame(pose);
            for row in &grid {
                assert!(
                    row.iter().all(|&letter| letter == b'.' || paint(letter).is_some()),
                    "{pose:?}: {}",
                    String::from_utf8_lossy(row)
                );
            }
            assert!(
                grid.iter().flatten().any(|&letter| letter == b'E') || matches!(pose, Pose::Blink | Pose::Sleep),
                "{pose:?} has eyes"
            );
            let right = duck_pixels(pose, false, 56);
            let left = duck_pixels(pose, true, 56);
            assert_eq!(right.len(), 56 * 56 * 4);
            // Facing left is the mirror image.
            let pixel = |rgba: &[u8], x_pos: usize, y_pos: usize| rgba[(y_pos * 56 + x_pos) * 4..][..4].to_vec();
            assert_eq!(pixel(&right, 10, 40), pixel(&left, 45, 40), "{pose:?}");
        }
    }

    #[test]
    fn a_screenshot_is_prepared() {
        let prepared = prepare_shot(vec![200; 3200 * 1800 * 4], 3200, 1800).expect("prepared");
        assert_eq!(prepared.sent, (SEND_SIZE, 882));
        assert_eq!((prepared.thumb.1, prepared.thumb.2), (THUMB, THUMB));
        assert!(prepared.preview.1 <= PREVIEW.0 && prepared.preview.2 <= PREVIEW.1);
        assert!(prepared.png.starts_with(b"\x89PNG"));
    }

    #[test]
    fn the_duck_wanders_with_numbers_between_zero_and_one() {
        let mut duck = Duck::default();
        for _ in 0..1000 {
            let value = duck.random();
            assert!((0.0..1.0).contains(&value));
        }
    }
}
