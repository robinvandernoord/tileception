//! The compositor's own locker. Password buffers never enter command logs or text caches.

use std::{
    ffi::CStr,
    sync::mpsc,
    time::{Duration, Instant},
};

use smithay::{
    backend::renderer::gles::GlesRenderer,
    input::keyboard::{keysyms, xkb},
    reexports::calloop::ping::make_ping,
};
use tileception_auth::{Authenticator, Outcome, SecretString};
use tileception_core::Rect;
use zeroize::{Zeroize, Zeroizing};

use crate::{
    lock::LockState,
    render::OutputElement,
    shapes::Shape,
    theme::Colour,
    ui::{glyph, Align, LocalTime, Painter, Style},
    Tileception,
};

pub struct BuiltinLocker {
    user: Option<String>,
    password: Zeroizing<[u8; 4096]>,
    len: usize,
    worker: Option<Authenticator>,
    results: Option<mpsc::Receiver<Result<Outcome, tileception_auth::Error>>>,
    epoch: u64,
    submitted: Option<u64>,
    message: &'static str,
    /// The message reports a failure, drawn in the danger colour.
    error: bool,
    /// When the last password was rejected, for the shake.
    failed_at: Option<Instant>,
    /// When a key was last typed, for the caret's blink.
    typed_at: Instant,
    caps_lock: bool,
    compose: Option<xkb::compose::State>,
    pub suspend_after_lock: bool,
}

impl Default for BuiltinLocker {
    fn default() -> Self {
        Self {
            user: current_user(),
            password: Zeroizing::new([0; 4096]),
            len: 0,
            worker: None,
            results: None,
            epoch: 0,
            submitted: None,
            message: "Enter your password",
            error: false,
            failed_at: None,
            typed_at: Instant::now(),
            caps_lock: false,
            compose: crate::ui::compose_state(),
            suspend_after_lock: false,
        }
    }
}

impl BuiltinLocker {
    pub fn clear(&mut self) {
        self.password.zeroize();
        self.len = 0;
        if let Some(compose) = &mut self.compose {
            compose.reset();
        }
    }
}

impl Tileception {
    /// The signed-in account's name.
    pub fn builtin_locker_user(&self) -> Option<&str> {
        self.builtin_locker.user.as_deref()
    }

    pub fn lock_session(&mut self, suspend: bool) {
        if suspend && !self.config.file.general.suspend_enabled {
            tracing::warn!("suspend is disabled by [general] suspend_enabled = false");
            return;
        }
        if self.is_locked() {
            return;
        }
        let command = match self.shell().lock() {
            crate::shell::Part::Off => {
                tracing::warn!("locking is switched off in [shell]");
                return;
            }
            crate::shell::Part::Command(line) => Some(self.expand_wallpaper(&line)),
            crate::shell::Part::Builtin => self.config.file.general.lock_command.is_some().then(|| self.lock_command()),
        };
        if let Some(lock) = command {
            let command = if suspend { format!("{lock} && systemctl suspend") } else { lock };
            self.spawn("sh", &["-c".into(), command]);
        } else {
            self.begin_builtin_lock();
            self.builtin_locker.suspend_after_lock = suspend;
        }
    }

    pub fn begin_builtin_lock(&mut self) {
        if self.is_locked() {
            return;
        }
        if self.builtin_locker.worker.is_none() {
            let started = (|| {
                let (ping, source) = make_ping()?;
                let (worker, results) = Authenticator::start(move || ping.ping())?;
                self.loop_handle
                    .insert_source(source, |_, _, state| state.authentication_ready())
                    .map_err(|e| std::io::Error::other(e.to_string()))?;
                Ok::<_, std::io::Error>((worker, results))
            })();
            match started {
                Ok((worker, results)) => {
                    self.builtin_locker.worker = Some(worker);
                    self.builtin_locker.results = Some(results);
                }
                Err(error) => tracing::error!("could not start password worker: {error}"),
            }
        }
        let locker = &mut self.builtin_locker;
        locker.clear();
        locker.epoch = locker.epoch.wrapping_add(1);
        // A check still running from an earlier lock no longer counts.
        locker.submitted = None;
        locker.caps_lock = false;
        let unavailable = locker.user.is_none() || locker.worker.is_none();
        locker.message = if unavailable {
            "Authentication unavailable. Switch to a console for recovery."
        } else {
            "Enter your password"
        };
        locker.error = unavailable;
        locker.failed_at = None;
        locker.typed_at = Instant::now();
        self.lock = LockState::BuiltIn;
        self.power_menu = None;
        self.launcher.open = false;
        self.move_drag = None;
        self.resize_drag = None;
        self.gap_drag = None;
        self.pending_press = None;
        self.selection.anchor = None;
        self.tap_armed = None;
        self.tap_fired.clear();
        self.seat.get_keyboard().expect("seat has a keyboard").unset_grab(self);
        self.force_keyboard_focus_sync();
        let pointer = self.seat.get_pointer().expect("seat has a pointer");
        let serial = smithay::utils::SERIAL_COUNTER.next_serial();
        pointer.unset_grab(self, serial, 0);
        pointer.motion(self, None, &smithay::input::pointer::MotionEvent { location: self.pointer, serial, time: 0 });
        pointer.frame(self);
        self.schedule_render();
    }

    pub fn builtin_lock_key(&mut self, sym: u32, ctrl: bool, caps_lock: bool) {
        let locker = &mut self.builtin_locker;
        locker.caps_lock = caps_lock;
        if locker.submitted.is_some() {
            return;
        }
        locker.typed_at = Instant::now();
        match sym {
            keysyms::KEY_Escape => locker.clear(),
            keysyms::KEY_BackSpace => {
                if let Some(compose) = &mut locker.compose {
                    if compose.status() == xkb::compose::Status::Composing {
                        compose.reset();
                        return;
                    }
                }
                if locker.len > 0 {
                    let text = std::str::from_utf8(&locker.password[..locker.len]).expect("typed UTF-8");
                    let last = text.char_indices().last().map(|(i, _)| i).unwrap_or(0);
                    locker.password[last..locker.len].zeroize();
                    locker.len = last;
                }
            }
            keysyms::KEY_Return | keysyms::KEY_KP_Enter => {
                if let (Some(worker), Some(user)) = (&locker.worker, &locker.user) {
                    let secret =
                        SecretString::from(std::str::from_utf8(&locker.password[..locker.len]).expect("typed UTF-8"));
                    let result = worker.submit(user.clone(), secret);
                    locker.clear();
                    if result.is_ok() {
                        locker.submitted = Some(locker.epoch);
                        locker.message = "Checking...";
                        locker.error = false;
                    } else {
                        locker.message = "Authentication busy or unavailable";
                        locker.error = true;
                    }
                }
            }
            _ if ctrl => {
                if matches!(sym, keysyms::KEY_u | keysyms::KEY_U) {
                    locker.clear();
                }
            }
            _ => {
                if let Some(encoded) = crate::ui::compose_text(&mut locker.compose, sym) {
                    if locker.len + encoded.len() <= locker.password.len() {
                        locker.password[locker.len..locker.len + encoded.len()].copy_from_slice(encoded.as_bytes());
                        locker.len += encoded.len();
                        locker.message = "Enter to unlock";
                        locker.error = false;
                    }
                }
            }
        }
    }

    fn authentication_ready(&mut self) {
        let result = self.builtin_locker.results.as_ref().and_then(|rx| rx.try_recv().ok());
        let Some(result) = result else { return };
        let epoch = self.builtin_locker.submitted.take();
        if !matches!(self.lock, LockState::BuiltIn) || epoch != Some(self.builtin_locker.epoch) {
            return;
        }
        self.builtin_locker.error = !matches!(result, Ok(Outcome::Success));
        self.builtin_locker.failed_at = Some(Instant::now());
        match result {
            Ok(Outcome::Success) => {
                self.builtin_locker.clear();
                self.builtin_locker.failed_at = None;
                self.lock = LockState::Unlocked;
                self.force_keyboard_focus_sync();
            }
            Ok(Outcome::WrongPassword) => self.builtin_locker.message = "Incorrect password. Try again.",
            Ok(Outcome::AccountProblem { .. }) => {
                self.builtin_locker.message = "Account unavailable. Contact your administrator."
            }
            Ok(Outcome::TryAgainLater { .. }) => {
                self.builtin_locker.message = "Too many attempts or service busy. Try again later."
            }
            Err(_) => self.builtin_locker.message = "Authentication failed. The session remains locked.",
        }
        self.schedule_render();
    }

    pub fn builtin_lock_elements(&self, renderer: &mut GlesRenderer) -> Vec<OutputElement> {
        let t = &self.theme;
        let locker = &self.builtin_locker;
        let screen = self.output_rect();
        let cx = screen.x + screen.w / 2.0;
        let mut p = Painter::new(self, renderer);

        // The wallpaper under a dark veil, or flat graphite.
        match &self.wallpaper {
            Some(wallpaper) => {
                p.image(&wallpaper.buffer, (screen.x, screen.y));
                p.fill(screen, Colour([0.0, 0.0, 0.0, 0.58]), 0.0);
            }
            None => p.fill(screen, t.background, 0.0),
        }

        // The time, large and light, with a soft accent glow behind it.
        let now = LocalTime::now();
        let clock_size = (t.font_size * 6.0).min(screen.h as f32 / 7.0).max(t.font_size * 2.0);
        let clock_style = Style::new(clock_size, t.text).weight(300).align(Align::Centre);
        let clock = now.as_ref().map(LocalTime::clock).unwrap_or_default();
        let clock_h = clock_size as f64 * 1.3;
        let clock_rect = Rect::new(screen.x, screen.y + screen.h * 0.26 - clock_h / 2.0, screen.w, clock_h);
        let (clock_w, _) = p.measure(&clock, screen.w, clock_style);
        if clock_w > 0.0 {
            let halo = Rect::new(cx - clock_w * 0.35, clock_rect.y + clock_h * 0.3, clock_w * 0.7, clock_h * 0.4);
            p.shape(halo, Shape { radius: 40.0, glow_radius: 48.0, glow: t.accent.alpha(0.10), ..Default::default() });
        }
        p.text(&clock, clock_rect, clock_style);
        let date = now.as_ref().map(LocalTime::date).unwrap_or_default();
        p.text(
            &date,
            Rect::new(screen.x, clock_rect.bottom() - 4.0, screen.w, 26.0),
            Style::new(t.font_size * 1.15, t.muted).align(Align::Centre),
        );

        // Who is signed in: an initial in the accent gradient, then the name.
        let user = locker.user.as_deref().unwrap_or("Locked");
        let avatar = Rect::new(cx - 34.0, screen.y + screen.h * 0.52 - 34.0, 68.0, 68.0);
        p.shape(
            avatar,
            Shape {
                radius: 34.0,
                fill: (t.accent, t.accent_end),
                glow_radius: 22.0,
                glow: t.accent.alpha(0.22),
                ..Default::default()
            },
        );
        let initial: String = user.chars().take(1).collect::<String>().to_uppercase();
        p.text(&initial, avatar, Style::new(t.font_size * 2.2, t.on_accent).bold().align(Align::Centre));
        let name = Rect::new(screen.x, avatar.bottom() + 10.0, screen.w, 26.0);
        p.text(user, name, Style::new(t.font_size * 1.2, t.text).bold().align(Align::Centre));

        // The password field, shaking briefly after a wrong password.
        let shake = locker
            .failed_at
            .filter(|_| locker.error)
            .map(|at| at.elapsed().as_secs_f64())
            .filter(|e| *e < SHAKE)
            .map(|e| (e * 46.0).sin() * 10.0 * (1.0 - e / SHAKE))
            .unwrap_or(0.0);
        let w = (screen.w - 48.0).clamp(1.0, 320.0);
        let field = Rect::new(cx - w / 2.0 + shake, name.bottom() + 18.0, w, 48.0);
        let failed = locker.error && locker.failed_at.is_some_and(|at| at.elapsed() < Duration::from_secs(3));
        let edge = if failed { t.danger } else { t.accent };
        p.shape(
            field,
            Shape {
                border_width: 1.0,
                border: edge,
                glow_radius: t.glow,
                glow: edge.alpha(0.27),
                ..Shape::filled(t.popup(), t.radius)
            },
        );
        p.glyph(glyph::LOCK, Rect::new(field.x + 6.0, field.y, 36.0, field.h), 16, t.muted);

        let count = std::str::from_utf8(&locker.password[..locker.len]).expect("typed UTF-8").chars().count();
        let checking = locker.submitted.is_some();
        let button = Rect::new(field.right() - 40.0, field.y + 8.0, 32.0, 32.0);
        let ready = count > 0 && !checking;
        if ready {
            p.shape(button, Shape { radius: 16.0, fill: (t.accent, t.accent_end), ..Default::default() });
        } else {
            p.fill(button, t.surface_raised, 16.0);
        }
        p.glyph(glyph::ARROW_RIGHT, button, 16, if ready { t.on_accent } else { t.muted });

        // One dot per character, never the characters themselves; they pulse while checking.
        let inner = Rect::new(field.x + 42.0, field.y, button.x - field.x - 50.0, field.h);
        let pitch = 15.0;
        let room = ((inner.w - 8.0) / pitch).floor().max(1.0) as usize;
        let shown = if checking { room.min(6) } else { count.min(room) };
        if shown == 0 {
            p.text("Password", inner, Style::new(t.font_size * 1.05, t.muted).align(Align::Centre));
        }
        let start = inner.x + (inner.w - shown as f64 * pitch) / 2.0 + (pitch - 8.0) / 2.0;
        let since = self.start.elapsed().as_secs_f64();
        for i in 0..shown {
            let alpha = if checking { 0.3 + 0.7 * (0.5 + 0.5 * (since * 7.0 - i as f64 * 0.7).sin()) } else { 1.0 };
            let dot = Rect::new(start + i as f64 * pitch, field.y + field.h / 2.0 - 4.0, 8.0, 8.0);
            p.fill(dot, t.text.alpha(alpha as f32), 4.0);
        }
        let blink = locker.typed_at.elapsed().as_millis() % 1000 < 600;
        if !checking && shown > 0 && blink {
            let x = start + shown as f64 * pitch - 2.0;
            p.fill(Rect::new(x, field.y + 14.0, 2.0, field.h - 28.0), t.accent, 1.0);
        }

        // What is going on, and a warning when Caps Lock would spoil the password.
        let status_colour = if locker.error { t.danger } else { t.muted };
        let status = Rect::new(screen.x + 24.0, field.bottom() + 12.0, screen.w - 48.0, 24.0);
        p.text(locker.message, status, Style::new(t.font_size * 0.95, status_colour).align(Align::Centre));
        if locker.caps_lock {
            let style = Style::new(t.font_size * 0.9, t.accent).bold();
            let (w, _) = p.measure("Caps Lock is on", 400.0, style);
            let chip = Rect::new(cx - (w + 46.0) / 2.0, status.bottom() + 10.0, w + 46.0, 28.0);
            p.shape(
                chip,
                Shape { border_width: 1.0, border: t.accent.alpha(0.5), ..Shape::filled(t.accent.alpha(0.12), 14.0) },
            );
            p.glyph(glyph::CAPS, Rect::new(chip.x + 8.0, chip.y, 20.0, chip.h), 14, t.accent);
            p.text("Caps Lock is on", Rect::new(chip.x + 32.0, chip.y, w + 4.0, chip.h), style);
        }

        // Keys along the bottom edge.
        let hints = [("Enter", "Unlock"), ("Esc", "Clear")];
        let width = p.key_hints(&hints, 0.0, 0.0, true);
        p.key_hints(&hints, cx - width / 2.0, screen.bottom() - 32.0, false);
        p.finish()
    }
}

/// How long the password field shakes after a wrong password, in seconds.
const SHAKE: f64 = 0.45;

#[allow(unsafe_code, reason = "the current account is resolved through libc's reentrant passwd API")]
fn current_user() -> Option<String> {
    let mut record = std::mem::MaybeUninit::<libc::passwd>::uninit();
    let mut result = std::ptr::null_mut();
    let mut buffer = vec![0u8; 16384];
    // SAFETY: getpwuid_r receives writable record, result and buffer storage of
    // the specified size. All storage stays live until the account name is copied.
    let status = unsafe {
        libc::getpwuid_r(libc::getuid(), record.as_mut_ptr(), buffer.as_mut_ptr().cast(), buffer.len(), &mut result)
    };
    if status != 0 || result.is_null() {
        return None;
    }
    // SAFETY: A non-null successful result points to record, with pw_name terminated inside buffer.
    unsafe { CStr::from_ptr((*result).pw_name) }.to_str().ok().map(str::to_owned)
}
