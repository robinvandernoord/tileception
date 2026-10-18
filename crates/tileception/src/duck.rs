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

use serde::Deserialize;
use smithay::{
    backend::{
        allocator::Fourcc,
        renderer::{element::memory::MemoryRenderBuffer, gles::GlesRenderer},
    },
    input::keyboard::{keysyms, xkb},
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
#[derive(Debug, Clone, Deserialize)]
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
    Walk { to: f64 },
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
    at: Point,
    /// From the duck's feet to the pointer.
    grab: (f64, f64),
}

pub struct Duck {
    pub out: bool,
    /// Left out of the frame for a moment, while the duck takes its screenshot.
    pub hidden: bool,
    placed: bool,
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
            .map_or(0x9e37_79b9, |d| d.as_nanos() as u64)
            | 1;
        Duck {
            out: saved_path().exists(),
            hidden: false,
            placed: false,
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

    fn between(&mut self, lo: f64, hi: f64) -> f64 {
        lo + (hi - lo) * self.random()
    }

    fn idle_for(&mut self, lo: f64, hi: f64) {
        let secs = self.between(lo, hi);
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
                .or_else(|e| if e.kind() == std::io::ErrorKind::NotFound { Ok(()) } else { Err(e) })
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

    /// Whether the duck is on screen now.
    fn duck_shown(&self) -> bool {
        self.duck.out && self.duck.placed && !self.is_locked() && self.fullscreen.is_none()
    }

    /// The configured size, as a factor on the duck's measurements.
    fn duck_scale(&self) -> f64 {
        let size = self.config.file.duck.size;
        if size.is_finite() { size.clamp(0.5, 4.0) } else { 1.0 }
    }

    /// The ground under `x`: the bottom of the usable screen, or the strip of
    /// minimized windows while it is out.
    fn duck_floor(&self, x: f64) -> f64 {
        let mut floor = self.world.screen().bottom() - 2.0;
        if let Some(strip) = self.strip_now()
            && x >= strip.x
            && x <= strip.right()
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
        let lo = screen.x + side;
        let hi = screen.right() - side;
        let wander = if self.doctor.open { (lo + crate::doctor::PANEL_W + 16.0).min(hi) } else { lo };
        (lo, hi, wander)
    }

    fn duck_rect(&self) -> Rect {
        let (x, y) = self.duck.pos;
        let k = self.duck_scale();
        Rect::new(x - 26.0 * k, y - 50.0 * k, 52.0 * k, 50.0 * k)
    }

    /// Moves the duck on by the time since the last frame. Called after each frame.
    pub fn update_duck(&mut self) {
        let now = Instant::now();
        let dt = now.duration_since(self.duck.last_tick).as_secs_f64().min(0.05);
        self.duck.last_tick = now;
        if !self.duck.out || self.is_locked() {
            return;
        }
        let screen = self.output_rect();
        if !self.duck.placed {
            // Dropped in from above, somewhere on the right.
            let x = screen.x + screen.w * self.duck.between(0.55, 0.85);
            self.duck.pos = (x, screen.y - SIZE * self.duck_scale());
            self.duck.vel = (0.0, 0.0);
            self.duck.motion = Motion::Air;
            self.duck.placed = true;
        }
        self.duck_chat_tick();
        let (lo, hi, wander) = self.duck_walls();
        let d = &mut self.duck;
        match d.motion {
            Motion::Held => {}
            Motion::Air => {
                d.vel.1 += GRAVITY * dt;
                d.pos.0 += d.vel.0 * dt;
                d.pos.1 += d.vel.1 * dt;
                if d.pos.0 < lo || d.pos.0 > hi {
                    d.pos.0 = d.pos.0.clamp(lo, hi);
                    d.vel.0 = -d.vel.0 * 0.5;
                }
                if d.vel.0.abs() > 1.0 {
                    d.left = d.vel.0 < 0.0;
                }
                let floor = self.duck_floor(self.duck.pos.0);
                let d = &mut self.duck;
                if d.pos.1 >= floor {
                    d.pos.1 = floor;
                    if d.vel.1 > 700.0 {
                        // A little bounce.
                        d.vel = (d.vel.0 * 0.6, -d.vel.1 * 0.28);
                    } else {
                        d.vel = (0.0, 0.0);
                        d.motion = Motion::Ground;
                        d.landed = Some(now);
                        d.idle_for(1.0, 3.0);
                    }
                }
            }
            Motion::Ground => self.duck_on_ground_tick(dt, now, (lo, hi, wander)),
        }
    }

    /// Walking, idling and sleeping, and falling off when the ground goes.
    fn duck_on_ground_tick(&mut self, dt: f64, now: Instant, (lo, hi, wander): (f64, f64, f64)) {
        let floor = self.duck_floor(self.duck.pos.0);
        let k = self.duck_scale();
        let d = &mut self.duck;
        d.pos.0 = d.pos.0.clamp(lo, hi);
        if d.pos.1 < floor - 0.5 {
            // The ground went (the strip slid away): fall.
            d.motion = Motion::Air;
            d.vel = (0.0, 0.0);
            return;
        }
        // The ground came up under it (the strip slid out): ride along.
        d.pos.1 = floor;
        let talking = d.chat != Chat::Closed;
        if talking {
            // Stay put while talking, apart from going to show the spot.
            match d.spot {
                Some((x, _)) => {
                    let to = (x + (if x > d.pos.0 { -36.0 } else { 36.0 }) * k).clamp(lo, hi);
                    if (to - d.pos.0).abs() > 2.0 {
                        if !matches!(d.activity, Activity::Walk { .. }) {
                            d.since = now;
                        }
                        d.activity = Activity::Walk { to };
                    } else {
                        d.left = x < d.pos.0;
                        d.activity = Activity::Idle { until: now + Duration::from_secs(5) };
                    }
                }
                None => {
                    if !matches!(d.activity, Activity::Idle { .. }) {
                        d.activity = Activity::Idle { until: now + Duration::from_secs(5) };
                    }
                }
            }
        }
        match d.activity {
            Activity::Walk { to } => {
                // In whole steps, a few times a second, except when hurrying
                // to show the spot.
                d.stride += dt;
                if !talking && d.stride < 1.0 / STEPS_PER_SEC {
                    return;
                }
                let step = WALK_SPEED * k * std::mem::take(&mut d.stride) * if talking { 4.0 } else { 1.0 };
                let dx = to - d.pos.0;
                if dx.abs() <= step {
                    d.pos.0 = to;
                    if !talking {
                        d.idle_for(2.0, 7.0);
                    }
                } else {
                    d.left = dx < 0.0;
                    d.pos.0 += step * dx.signum();
                }
            }
            Activity::Idle { until } if now >= until && !talking => {
                let roll = d.random();
                if d.pos.0 < wander {
                    // Out from under the doctor panel.
                    let to = d.between(wander, (wander + 200.0).min(hi));
                    d.activity = Activity::Walk { to };
                    d.since = now;
                } else if roll < 0.6 {
                    let distance = d.between(40.0, 320.0);
                    let way = if d.random() < 0.5 { -1.0 } else { 1.0 };
                    let mut to = d.pos.0 + way * distance;
                    if to < wander || to > hi {
                        to = d.pos.0 - way * distance;
                    }
                    d.activity = Activity::Walk { to: to.clamp(wander, hi) };
                    d.since = now;
                } else if roll < 0.72 {
                    let secs = d.between(15.0, 40.0);
                    d.activity = Activity::Sleep { until: now + Duration::from_secs_f64(secs) };
                    d.since = now;
                } else {
                    d.idle_for(2.0, 6.0);
                }
            }
            Activity::Sleep { until } if now >= until || talking => d.idle_for(1.0, 3.0),
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
                let over =
                    self.duck_bubble().is_some_and(|b| b.rect.contains(Point::new(self.pointer.x, self.pointer.y)));
                if let Chat::Asked { done: Some(at), .. } = self.duck.chat
                    && now.duration_since(at) > ANSWER_FOR
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
        if messages.iter().any(|m| matches!(m, Message::Ask { answer: None, .. })) {
            return Some(Reply::NeedsOk);
        }
        let text: Vec<&str> = messages
            .iter()
            .filter_map(|m| match m {
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
                .filter_map(|m| match m {
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
        let (x, y) = parse_point(text)?;
        let (w, h) = self.duck.shot.as_ref()?.sent;
        let screen = self.output_rect();
        let x = screen.x + x / f64::from(w) * screen.w;
        let y = screen.y + y / f64::from(h) * screen.h;
        screen.contains(Point::new(x, y)).then_some((x, y))
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
    pub fn duck_screen_taken(&mut self, rgba: Vec<u8>, (w, h): (u32, u32)) {
        let epoch = self.duck.shot_epoch;
        let Some(sender) = self.duck_shots() else { return };
        std::thread::spawn(move || {
            let _ = sender.send((epoch, prepare_shot(rgba, w, h)));
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
                .map_err(|e| tracing::warn!("duck screenshots: {e}"))
                .ok()?;
            self.duck.shots = Some((sender, token));
        }
        Some(self.duck.shots.as_ref()?.0.clone())
    }

    fn duck_shot_ready(&mut self, epoch: u64, prepared: Option<Prepared>) {
        if epoch != self.duck.shot_epoch {
            return;
        }
        let Some(p) = prepared else {
            self.duck.chat = Chat::Note("I couldn't see the screen just now.".into());
            self.schedule_render();
            return;
        };
        let (rgba, w, h) = p.preview;
        let size = (w as i32, h as i32);
        let preview = MemoryRenderBuffer::from_slice(&rgba, Fourcc::Abgr8888, size, 1, Transform::Normal, None);
        self.duck.shot = Some(Shot { png: p.png.into(), sent: p.sent, thumb: p.thumb, preview: Some((preview, size)) });
        self.schedule_render();
    }

    /// Sends the question with the screenshot to the doctor's agent.
    fn duck_send(&mut self, question: String) {
        let Some(shot) = &self.duck.shot else { return };
        let (w, h) = shot.sent;
        let prompt = format!(
            "[Asked through Tileception's rubber duck. A screenshot of the whole screen is attached \
             ({w}x{h} pixels). Answer as a friendly rubber duck: short, at most four sentences, plain \
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
    pub fn duck_motion(&mut self, at: Point) -> bool {
        let Some(press) = self.duck.press else { return false };
        let now = Instant::now();
        if self.duck.motion != Motion::Held {
            if (at.x - press.at.x).hypot(at.y - press.at.y) < DRAG_THRESHOLD {
                return true;
            }
            self.duck.motion = Motion::Held;
            self.duck.since = now;
            self.duck.trail = [(at, now); 2];
        }
        self.duck.pos = (at.x - press.grab.0, at.y - press.grab.1);
        if (at.x - self.duck.trail[1].0.x).abs() > 0.5 {
            self.duck.left = at.x < self.duck.trail[1].0.x;
        }
        if now.duration_since(self.duck.trail[1].1) > Duration::from_millis(8) {
            self.duck.trail = [self.duck.trail[1], (at, now)];
        }
        true
    }

    /// A button over the duck or its bubble. True when the duck took it.
    pub fn duck_button(&mut self, left: bool, pressed: bool, at: Point) -> bool {
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
                && link.contains(at)
            {
                if left {
                    self.close_duck_chat();
                    self.open_doctor();
                }
                return true;
            }
            if bubble.close.is_some_and(|c| c.contains(at)) {
                if left {
                    self.close_duck_chat();
                }
                return true;
            }
            if bubble.rect.contains(at) {
                return true;
            }
        }
        if self.duck_rect().contains(at) {
            if left {
                self.duck.press = Some(Press { at, grab: (at.x - self.duck.pos.0, at.y - self.duck.pos.1) });
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
        let [(a, ta), (b, tb)] = self.duck.trail;
        let dt = tb.duration_since(ta).as_secs_f64();
        let fresh = tb.elapsed() < Duration::from_millis(80);
        let (mut vx, mut vy) = if dt > 0.0 && fresh { ((b.x - a.x) / dt, (b.y - a.y) / dt) } else { (0.0, 0.0) };
        let speed = vx.hypot(vy);
        if speed > MAX_THROW {
            vx *= MAX_THROW / speed;
            vy *= MAX_THROW / speed;
        }
        self.duck.vel = (vx, vy);
        self.duck.motion = Motion::Air;
        self.duck.since = Instant::now();
    }

    /// The bubble for what the duck is saying, laid out.
    fn duck_bubble(&self) -> Option<Bubble> {
        if !self.duck_shown() {
            return None;
        }
        let t = &self.theme;
        let screen = self.output_rect();
        let mut text = None;
        let mut link = None;
        let mut field = None;
        let mut preview = None;
        let mut close = None;
        let mut thinking = false;
        let (w, h) = match &self.duck.chat {
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
                    let style = Style::new(t.font_size, t.text).lines(ANSWER_LINES);
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
        let (w, h) = match &text {
            Some(s) => {
                let style = Style::new(t.font_size, t.text).lines(ANSWER_LINES);
                let (tw, th) = self.duck_measure(s, BUBBLE_W - PAD * 2.0 - 20.0, style);
                ((tw + PAD * 2.0 + 20.0).clamp(200.0, BUBBLE_W), th + PAD * 2.0 + LINK_H + 4.0)
            }
            None => (w, h),
        };
        let (x, y) = self.duck.pos;
        let head = x + if self.duck.left { -8.0 } else { 8.0 };
        let gap = if thinking { 30.0 } else { 12.0 };
        let mut below = false;
        let mut top = y - SIZE * self.duck_scale() - gap - h;
        if top < screen.y + 8.0 {
            below = true;
            top = y + gap;
        }
        let left =
            (head - w * if self.duck.left { 0.75 } else { 0.25 }).clamp(screen.x + 8.0, screen.right() - 8.0 - w);
        let rect = Rect::new(left, top, w, h);
        let tail_x = head.clamp(rect.x + 18.0, rect.right() - 18.0);
        let inner = Rect::new(rect.x + PAD, rect.y + PAD, rect.w - PAD * 2.0, rect.h - PAD * 2.0);
        match &self.duck.chat {
            Chat::Asking => {
                let (pw, ph) = (f64::from(PREVIEW.0), f64::from(PREVIEW.1));
                preview = Some(Rect::new(inner.x, inner.y, pw, ph));
                field = Some(Rect::new(inner.x + pw + 12.0, inner.y, inner.w - pw - 12.0, ph));
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
                    link: link.map(|l| (link_rect, l)),
                    field,
                    preview,
                    text: text.map(|s| (body, s)),
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
            .map(|i| (i.width, i.height))
            .unwrap_or((0.0, 0.0))
    }

    /// The pose for this frame.
    fn duck_pose(&self) -> Pose {
        let d = &self.duck;
        let now = Instant::now();
        let t = now.duration_since(d.since).as_secs_f64();
        match d.motion {
            Motion::Held => return Pose::Held(((t * 7.0) as u64).is_multiple_of(2)),
            Motion::Air => return Pose::Fall,
            Motion::Ground => {}
        }
        if d.landed.is_some_and(|at| now.duration_since(at) < Duration::from_millis(140)) {
            return Pose::Squash;
        }
        if let Activity::Walk { .. } = d.activity {
            return Pose::Step(((t * 8.0) as u64 % 4) as u8);
        }
        match &d.chat {
            Chat::Asking => return Pose::Listen,
            Chat::Sending { .. } => return Pose::Think,
            Chat::Asked { mark, .. } => match self.duck_reply(*mark) {
                Some(Reply::Thinking) => return Pose::Think,
                Some(Reply::Text { done: false, .. }) if now.duration_since(d.grew.1) < Duration::from_millis(400) => {
                    return Pose::Talk(((t * 8.0) as u64).is_multiple_of(2));
                }
                _ if d.spot.is_some() => return Pose::Point,
                _ => {}
            },
            _ => {}
        }
        if let Activity::Sleep { .. } = d.activity {
            return Pose::Sleep;
        }
        // A blink now and then.
        if (now.duration_since(self.start).as_millis() % 3700) < 130 { Pose::Blink } else { Pose::Stand }
    }

    pub fn duck_elements(&self, renderer: &mut GlesRenderer) -> Vec<OutputElement> {
        if self.duck.hidden || !self.duck_shown() {
            return Vec::new();
        }
        let t = &self.theme;
        let pose = self.duck_pose();
        let (x, y) = self.duck.pos;
        let scale = self.duck_scale();
        let size = SIZE * scale;
        let bubble = self.duck_bubble();
        let mut p = Painter::new(self, renderer);

        // The place an answer is about: a soft ring that breathes.
        if let Some((sx, sy)) = self.duck.spot.filter(|_| self.duck.chat != Chat::Closed) {
            let phase = (stepped(self.start.elapsed()) * 2.5).sin() * 0.5 + 0.5;
            let r = 18.0 + phase * 6.0;
            p.shape(
                Rect::new(sx - r, sy - r, r * 2.0, r * 2.0),
                Shape {
                    radius: r as f32,
                    border_width: 3.0,
                    border: t.accent,
                    glow_radius: 18.0,
                    glow: t.accent.alpha(0.35),
                    ..Default::default()
                },
            );
        }

        if let Some(sprite) = self.duck_sprite(pose) {
            p.image(&sprite, (x - size / 2.0, y - size));
        }

        if pose == Pose::Sleep {
            // Zs drifting up from its head.
            let secs = stepped(self.duck.since.elapsed());
            for i in 0..3 {
                let k = (secs * 0.25 + f64::from(i) / 3.0).fract();
                // A few sizes in one colour, faded as a whole: each new size
                // or colour would be text rendered again.
                let font = t.font_size * (0.8 + (k * 4.0).floor() as f32 / 8.0);
                let side = if self.duck.left { -1.0 } else { 1.0 };
                let zx = x + side * (12.0 + k * 14.0) * scale - 6.0;
                let zy = y - size - k * 26.0;
                p.alpha = ((1.0 - k) * ease_out(k * 4.0)) as f32;
                p.text("z", Rect::new(zx, zy, 20.0, 20.0), Style::new(font, t.text).bold());
            }
            p.alpha = 1.0;
        }

        if let Some(b) = bubble {
            self.draw_duck_bubble(&mut p, &b);
        }
        p.finish()
    }

    fn draw_duck_bubble(&self, p: &mut Painter, b: &Bubble) {
        let t = &self.theme;
        let (x, y) = self.duck.pos;
        if b.thinking {
            // Little thought circles up to the bubble.
            let k = self.duck_scale();
            let head = (x + (if self.duck.left { -10.0 } else { 10.0 }) * k, y - (SIZE - 6.0) * k);
            let to = (b.tail_x, if b.below { b.rect.y } else { b.rect.bottom() });
            for (k, r) in [(0.35, 3.5), (0.7, 5.5)] {
                let cx = head.0 + (to.0 - head.0) * k;
                let cy = head.1 + (to.1 - head.1) * k;
                p.shape(
                    Rect::new(cx - r, cy - r, r * 2.0, r * 2.0),
                    Shape { border_width: 1.0, border: t.border, ..Shape::filled(t.popup(), r as f32) },
                );
            }
        }
        p.popup(b.rect);
        if !b.thinking
            && let Some(tail) = self.duck_tail(b.below)
        {
            let at =
                if b.below { (b.tail_x - 10.0, b.rect.y - 11.0) } else { (b.tail_x - 10.0, b.rect.bottom() - 1.0) };
            p.image(&tail, at);
        }
        if b.thinking {
            // Three dots taking turns.
            let secs = self.start.elapsed().as_secs_f64();
            for i in 0..3 {
                let k = ((secs * 3.0 - f64::from(i) * 0.4).sin() * 0.5 + 0.5) as f32;
                let cx = b.rect.x + b.rect.w / 2.0 + (f64::from(i) - 1.0) * 14.0;
                let cy = b.rect.y + b.rect.h / 2.0 - f64::from(k) * 3.0;
                p.fill(Rect::new(cx - 4.0, cy - 4.0, 8.0, 8.0), t.muted.alpha(0.45 + 0.55 * k), 4.0);
            }
            return;
        }
        if let Some(rect) = b.preview {
            match self.duck.shot.as_ref().and_then(|s| s.preview.as_ref()) {
                Some((buffer, size)) => {
                    let at = (rect.x + (rect.w - f64::from(size.0)) / 2.0, rect.y + (rect.h - f64::from(size.1)) / 2.0);
                    p.fill(
                        Rect::new(at.0 - 1.0, at.1 - 1.0, f64::from(size.0) + 2.0, f64::from(size.1) + 2.0),
                        t.border,
                        3.0,
                    );
                    p.image_sized(buffer, at, Some(*size));
                }
                None => {
                    let pulse = (self.start.elapsed().as_secs_f64() * 4.0).sin() * 0.5 + 0.5;
                    p.fill(rect, t.surface_raised.alpha(0.6 + 0.4 * pulse as f32), 4.0);
                    p.glyph(glyph::CAMERA, rect, 16, t.muted);
                }
            }
        }
        if let Some(rect) = b.field {
            let size = t.font_size;
            let line = Rect::new(rect.x, rect.y, rect.w, 26.0);
            if self.duck.input.text.is_empty() {
                p.text("What do you want to know?", line, Style::new(size, t.muted));
            }
            let field = self.text.borrow_mut().field(
                &self.duck.input.text,
                &t.font,
                size,
                t.text,
                line.w as f32,
                crate::text::Field {
                    cursor: self.duck.input.cursor,
                    anchor: self.duck.input.anchor,
                    visible: self.duck.edited.elapsed().as_millis() % 1000 < 600,
                    accent: t.accent.straight_rgba8(),
                },
            );
            if let Some(image) = field {
                p.image(&image.buffer, (line.x, line.y + (line.h - image.height) / 2.0));
            }
            p.fill(Rect::new(rect.x, line.bottom() + 2.0, rect.w, 1.0), t.accent.alpha(0.6), 0.0);
            let hints = Rect::new(rect.x, line.bottom() + 4.0, rect.w, rect.h - line.h - 4.0);
            p.text(
                "I can see your screen \u{00b7} Enter asks, Esc closes",
                hints,
                Style::new(t.font_size * 0.8, t.muted),
            );
        }
        if let Some((rect, text)) = &b.text {
            p.text(text, *rect, Style::new(t.font_size, t.text).lines(ANSWER_LINES));
        }
        let pointer = Point::new(self.pointer.x, self.pointer.y);
        if let Some(rect) = b.close {
            let hover = rect.contains(pointer);
            if hover {
                p.fill(rect, t.surface_raised, 11.0);
            }
            p.glyph(glyph::CLOSE, rect, 12, if hover { t.text } else { t.muted });
        }
        if let Some((rect, label)) = b.link {
            let style = Style::new(t.font_size * 0.9, t.accent).bold();
            let (w, _) = p.text(label, rect, style.align(Align::Left));
            if rect.contains(pointer) {
                p.fill(Rect::new(rect.x, rect.y + rect.h / 2.0 + 9.0, w, 1.0), t.accent, 0.0);
            }
        }
    }

    /// The duck's sprite for a pose, facing its way, at the output's scale.
    fn duck_sprite(&self, pose: Pose) -> Option<MemoryRenderBuffer> {
        let px = (SIZE * self.duck_scale() * self.scale).round().max(1.0) as u32;
        let key = (pose, self.duck.left, px);
        if let Some(sprite) = self.duck.sprites.borrow().get(&key) {
            return sprite.clone();
        }
        let rgba = duck_pixels(pose, self.duck.left, px);
        let size = (px as i32, px as i32);
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
        let t = &self.theme;
        let fill = t.popup().straight_rgba8();
        let border = t.border.straight_rgba8();
        let (w, h) = ((20.0 * self.scale).round() as u32, (12.0 * self.scale).round() as u32);
        let key = (below, fill, border, w);
        if let Some(tail) = self.duck.tails.borrow().get(&key) {
            return tail.clone();
        }
        let flip = if below { r#" transform="translate(0 12) scale(1 -1)""# } else { "" };
        let svg = format!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 20 12"><g{flip}><path d="M0 0L10 11L20 0Z" fill="{}"/><path d="M1 0.5L10 11L19 0.5" fill="none" stroke="{}" stroke-width="1"/></g></svg>"#,
            css(fill),
            css(border),
        );
        let tail = rasterise(&svg, w, h);
        self.duck.tails.borrow_mut().insert(key, tail.clone());
        tail
    }
}

/// Seconds of `elapsed`, in whole steps.
fn stepped(elapsed: Duration) -> f64 {
    (elapsed.as_secs_f64() * STEPS_PER_SEC).floor() / STEPS_PER_SEC
}

fn css([r, g, b, a]: [u8; 4]) -> String {
    format!("rgba({r},{g},{b},{:.3})", f32::from(a) / 255.0)
}

fn rasterise(svg: &str, w: u32, h: u32) -> Option<MemoryRenderBuffer> {
    let tree = resvg::usvg::Tree::from_str(svg, &resvg::usvg::Options::default()).ok()?;
    let mut pixmap = resvg::tiny_skia::Pixmap::new(w, h)?;
    let scale = (w as f32 / tree.size().width()).min(h as f32 / tree.size().height());
    resvg::render(&tree, resvg::tiny_skia::Transform::from_scale(scale, scale), &mut pixmap.as_mut());
    // tiny-skia pixels are premultiplied RGBA, which is what the renderer wants.
    Some(MemoryRenderBuffer::from_slice(
        pixmap.data(),
        Fourcc::Abgr8888,
        (w as i32, h as i32),
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
    let mut f = BASE.map(|row| <[u8; GRID]>::try_from(row.as_bytes()).expect("rows are 16 wide"));
    let set = |f: &mut [[u8; GRID]; GRID], rows: &[(usize, &str)]| {
        for (i, row) in rows {
            f[*i].copy_from_slice(row.as_bytes());
        }
    };
    let eyes_shut = |f: &mut [[u8; GRID]; GRID]| set(f, &[(5, "....DYLYYYYYD..."), (6, "....DYYYYDDYOOD.")]);
    let eyes_up = |f: &mut [[u8; GRID]; GRID]| {
        set(f, &[(4, "....DYLLYYEYD..."), (5, "....DYLYYYEYD..."), (6, "....DYYYYYYYOOD.")])
    };
    let beak_open = |f: &mut [[u8; GRID]; GRID]| {
        set(f, &[(5, "....DYLYYYEYOOD."), (6, "....DYYYYYEYD..."), (7, "....DYYYYYYD...."), (8, "....DYYBYYYDOOD.")])
    };
    let wing_up = |f: &mut [[u8; GRID]; GRID]| {
        set(
            f,
            &[(10, "DYYDDSSSSYYYD..."), (11, "DYYYYYSSSSYYYD.."), (12, ".DYYYYYYYYYYYD.."), (13, ".DYYYYYYYYYYYD..")],
        )
    };
    // Up off the ground by `n` rows.
    let lift = |f: &mut [[u8; GRID]; GRID], n: usize| {
        f.rotate_left(n);
        f[GRID - n..].fill([b'.'; GRID]);
    };
    // Flattened by dropping the plain row across the body.
    let squash = |f: &mut [[u8; GRID]; GRID]| {
        f.copy_within(0..11, 1);
        f[0] = [b'.'; GRID];
    };
    match pose {
        Pose::Stand => {}
        Pose::Blink | Pose::Sleep => eyes_shut(&mut f),
        // A hop: crouch, up, top, down.
        Pose::Step(0) => squash(&mut f),
        Pose::Step(2) => lift(&mut f, 2),
        Pose::Step(_) => lift(&mut f, 1),
        Pose::Held(flap) => {
            if flap {
                wing_up(&mut f);
                beak_open(&mut f);
            }
        }
        Pose::Fall => {
            wing_up(&mut f);
            beak_open(&mut f);
        }
        Pose::Squash => squash(&mut f),
        Pose::Listen => eyes_up(&mut f),
        Pose::Think => {
            eyes_up(&mut f);
            wing_up(&mut f);
        }
        Pose::Talk(open) => {
            if open {
                beak_open(&mut f);
            }
        }
        Pose::Point => wing_up(&mut f),
    }
    f
}

/// A pixel's colour, as RGBA.
fn paint(c: u8) -> Option<[u8; 4]> {
    Some(match c {
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

/// The pose blown up to `px` square with hard edges, facing right (or left).
/// RGBA; every pixel is fully opaque or fully clear, so it is also premultiplied.
fn duck_pixels(pose: Pose, left: bool, px: u32) -> Vec<u8> {
    let f = frame(pose);
    let px = px as usize;
    let mut rgba = vec![0; px * px * 4];
    for y in 0..px {
        let row = &f[y * GRID / px];
        for x in 0..px {
            let source_x = if left { px - 1 - x } else { x };
            let col = source_x * GRID / px;
            if let Some(colour) = paint(row[col]) {
                rgba[(y * px + x) * 4..][..4].copy_from_slice(&colour);
            }
        }
    }
    rgba
}

/// An answer's Markdown as plain lines for the bubble, without the
/// `point:` line.
fn plain(markdown: &str) -> String {
    use crate::markdown::{Kind, Piece};
    let source: String = markdown.lines().filter(|l| point_in(l).is_none()).collect::<Vec<_>>().join("\n");
    let mut out: Vec<String> = Vec::new();
    for piece in crate::markdown::parse(&source) {
        match piece {
            Piece::Text { kind, spans } => {
                let text: String = spans.iter().map(|s| s.text.as_str()).collect();
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
    let line = line.trim().trim_matches(|c| c == '`' || c == '*' || c == '_').trim();
    let rest = line.get(..6).filter(|p| p.eq_ignore_ascii_case("point:")).map(|_| &line[6..])?;
    let rest = rest.trim().trim_start_matches('(').trim_end_matches(')');
    let (x, y) = rest.split_once(',')?;
    let x: f64 = x.trim().parse().ok()?;
    let y: f64 = y.trim().parse().ok()?;
    (x.is_finite() && y.is_finite() && x >= 0.0 && y >= 0.0).then_some((x, y))
}

/// The screen scaled for sending, as PNG, with its thumbnails.
fn prepare_shot(mut rgba: Vec<u8>, w: u32, h: u32) -> Option<Prepared> {
    use image::imageops::FilterType;
    // The scene has no meaningful alpha.
    for pixel in rgba.as_chunks_mut::<4>().0 {
        pixel[3] = 255;
    }
    let image = image::DynamicImage::ImageRgba8(image::RgbaImage::from_raw(w, h, rgba)?);
    let sent = if w.max(h) > SEND_SIZE { image.resize(SEND_SIZE, SEND_SIZE, FilterType::Triangle) } else { image };
    let mut png = Vec::new();
    image::DynamicImage::ImageRgb8(sent.to_rgb8())
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .map_err(|e| tracing::warn!("duck screenshot: {e}"))
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
            let f = frame(pose);
            for row in &f {
                assert!(
                    row.iter().all(|&c| c == b'.' || paint(c).is_some()),
                    "{pose:?}: {}",
                    String::from_utf8_lossy(row)
                );
            }
            assert!(
                f.iter().flatten().any(|&c| c == b'E') || matches!(pose, Pose::Blink | Pose::Sleep),
                "{pose:?} has eyes"
            );
            let right = duck_pixels(pose, false, 56);
            let left = duck_pixels(pose, true, 56);
            assert_eq!(right.len(), 56 * 56 * 4);
            // Facing left is the mirror image.
            let pixel = |rgba: &[u8], x: usize, y: usize| rgba[(y * 56 + x) * 4..][..4].to_vec();
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
            let r = duck.random();
            assert!((0.0..1.0).contains(&r));
        }
    }
}
