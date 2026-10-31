//! The compositor's own locker: the greeter's tiles in lock mode
//! ([`crate::greeter::Mode::Lock`]), checking the password with PAM on a worker
//! thread. Password buffers never enter command logs or text caches.

use std::{ffi::CStr, sync::mpsc};

use smithay::reexports::calloop::ping::make_ping;
use tileception_auth::{Authenticator, Outcome, SecretString};
use tileception_notify::Urgency;
use zeroize::Zeroizing;

use crate::{Tileception, greeter::Greeter, lock::LockState};

pub struct BuiltinLocker {
    user: Option<String>,
    worker: Option<Authenticator>,
    results: Option<mpsc::Receiver<Result<Outcome, tileception_auth::Error>>>,
    epoch: u64,
    submitted: Option<u64>,
    pub suspend_after_lock: bool,
}

impl Default for BuiltinLocker {
    fn default() -> Self {
        Self { user: current_user(), worker: None, results: None, epoch: 0, submitted: None, suspend_after_lock: false }
    }
}

/// Said when the password cannot be checked at all.
const UNAVAILABLE: &str = "Authentication unavailable. Switch to a console for recovery.";

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
                    .map_err(|error| std::io::Error::other(error.to_string()))?;
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
        locker.epoch = locker.epoch.wrapping_add(1);
        // A check still running from an earlier lock no longer counts.
        locker.submitted = None;
        let unavailable = locker.user.is_none() || locker.worker.is_none();
        let login = locker.user.clone().unwrap_or_else(|| "Locked".into());
        let notifications = self.notification_status().map_or(0, |(count, _)| count);
        let mut screen = Greeter::lock(&login, notifications);
        if unavailable {
            screen.notice = Some(UNAVAILABLE.into());
        }
        self.greeter = Some(Box::new(screen));
        self.lock = LockState::BuiltIn;
        self.handoff_lock_changed();
        // Locking can start from a key binding, while the keyboard is busy.
        self.loop_handle.insert_idle(|state| state.greeter_keyboard_layouts());
        self.power_menu = None;
        self.launcher.open = false;
        self.notifications.open = false;
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

    /// Enter on the lock screen: has the worker check `password`.
    pub(crate) fn submit_unlock(&mut self, password: Zeroizing<String>) {
        let locker = &mut self.builtin_locker;
        let submitted = match (&locker.worker, &locker.user) {
            (Some(worker), Some(user)) => worker.submit(user.clone(), SecretString::from(password.as_str())).is_ok(),
            _ => false,
        };
        drop(password);
        if submitted {
            locker.submitted = Some(locker.epoch);
            return;
        }
        if let Some(screen) = self.greeter.as_deref_mut() {
            screen.unlock_unavailable();
        }
        self.notify_locally("Cannot unlock", "Authentication is busy or unavailable.", Urgency::Critical);
    }

    fn authentication_ready(&mut self) {
        let result = self.builtin_locker.results.as_ref().and_then(|receiver| receiver.try_recv().ok());
        let Some(result) = result else { return };
        let epoch = self.builtin_locker.submitted.take();
        if !matches!(self.lock, LockState::BuiltIn) || epoch != Some(self.builtin_locker.epoch) {
            return;
        }
        let Some(screen) = self.greeter.as_deref_mut() else { return };
        match screen.unlock_result(&result) {
            None => {
                tracing::info!("session unlocked");
                self.end_greeter_keymap();
                self.greeter = None;
                self.lock = LockState::Unlocked;
                self.force_keyboard_focus_sync();
            }
            Some((summary, body)) => self.notify_locally(summary, &body, Urgency::Critical),
        }
        self.schedule_render();
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
