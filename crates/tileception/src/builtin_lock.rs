//! The compositor's own locker. Password buffers never enter command logs or text caches.

use std::{ffi::CStr, sync::mpsc};

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
    render::{push_image, push_shape, solid, OutputElement},
    shapes::Shape,
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
    pub fn lock_session(&mut self, suspend: bool) {
        if suspend && !self.config.file.general.suspend_enabled {
            tracing::warn!("suspend is disabled by [general] suspend_enabled = false");
            return;
        }
        if self.is_locked() {
            return;
        }
        if self.config.file.general.lock_command.is_some() {
            let lock = self.lock_command();
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
        locker.message = if locker.user.is_none() || locker.worker.is_none() {
            "Authentication unavailable. Switch to a console for recovery."
        } else {
            "Enter your password"
        };
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
                        locker.message = "Checking password...";
                    } else {
                        locker.message = "Authentication busy or unavailable";
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
                        locker.message = "Press Enter to unlock";
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
        match result {
            Ok(Outcome::Success) => {
                self.builtin_locker.clear();
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
        let screen = self.output_rect();
        let w = (screen.w - 40.0).clamp(1.0, 420.0);
        let panel = Rect::new(screen.x + (screen.w - w) / 2.0, screen.y + (screen.h - 220.0) / 2.0, w, 220.0);
        let mut out = Vec::new();
        let mut text = self.text.borrow_mut();
        let user = self.builtin_locker.user.as_deref().unwrap_or("Session locked");
        let count = std::str::from_utf8(&self.builtin_locker.password[..self.builtin_locker.len])
            .expect("typed UTF-8")
            .chars()
            .count();
        let dots = "\u{2022}".repeat(count);
        let entered = if count == 0 { "Password" } else { &dots };
        let status = if self.builtin_locker.caps_lock { "Caps Lock is on" } else { self.builtin_locker.message };
        for (label, y, size, colour) in [
            (user, 24.0, t.font_size * 1.5, t.text),
            (entered, 88.0, t.font_size, t.text),
            (status, 154.0, t.font_size * 0.9, t.muted),
        ] {
            if let Some(img) = text.line(label, &t.font, size, colour, (w - 48.0) as f32, false) {
                push_image(&mut out, renderer, &img.buffer, (panel.x + 24.0, panel.y + y));
            }
        }
        let shapes = self.shapes(renderer);
        push_shape(
            &mut out,
            shapes.as_ref(),
            &Rect::new(panel.x + 20.0, panel.y + 72.0, w - 40.0, 48.0),
            Shape { border_width: 1.0, border: t.accent, ..Shape::filled(t.background, t.radius) },
        );
        push_shape(
            &mut out,
            shapes.as_ref(),
            &panel,
            Shape { border_width: 1.0, border: t.border, ..Shape::filled(t.surface, t.popup_radius) },
        );
        out.push(solid(&screen, [0.0, 0.0, 0.0, 0.45]));
        if let Some(wallpaper) = &self.wallpaper {
            push_image(&mut out, renderer, &wallpaper.buffer, (screen.x, screen.y));
        }
        out.push(solid(&screen, t.background.0));
        out
    }
}

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
