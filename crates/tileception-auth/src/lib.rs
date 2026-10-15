//! Linux-PAM authentication. Use [`Authenticator`] on an event-loop thread.

use secrecy::ExposeSecret;
pub use secrecy::SecretString;
use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::ptr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use zeroize::Zeroize;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Success,
    WrongPassword,
    AccountProblem {
        code: i32,
        message: String,
    },
    /// PAM supplies no reliable retry duration. Do not retry automatically.
    TryAgainLater {
        code: i32,
        message: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    InvalidUser,
    InvalidPassword,
    ServiceUnavailable,
    Pam { operation: &'static str, code: i32 },
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for Error {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubmitError {
    Busy,
    Stopped,
}

struct Attempt {
    user: String,
    password: SecretString,
}

/// Owns a dedicated PAM worker. Dropping it never blocks on PAM's failure delay.
pub struct Authenticator {
    requests: mpsc::SyncSender<Attempt>,
    busy: Arc<AtomicBool>,
}

impl Authenticator {
    pub fn start(wake: impl Fn() + Send + 'static) -> std::io::Result<(Self, mpsc::Receiver<Result<Outcome, Error>>)> {
        Self::start_with(wake, check_password)
    }

    fn start_with(
        wake: impl Fn() + Send + 'static,
        check: impl Fn(&str, SecretString) -> Result<Outcome, Error> + Send + 'static,
    ) -> std::io::Result<(Self, mpsc::Receiver<Result<Outcome, Error>>)> {
        let (requests, attempts) = mpsc::sync_channel::<Attempt>(1);
        let (results, receiver) = mpsc::channel();
        let busy = Arc::new(AtomicBool::new(false));
        let worker_busy = Arc::clone(&busy);
        std::thread::Builder::new().name("tileception-pam".into()).spawn(move || {
            while let Ok(attempt) = attempts.recv() {
                let result = check(&attempt.user, attempt.password);
                if results.send(result).is_err() {
                    break;
                }
                worker_busy.store(false, Ordering::Release);
                wake();
            }
        })?;
        Ok((Self { requests, busy }, receiver))
    }

    /// Rejected attempts are dropped immediately, zeroizing their passwords.
    pub fn submit(&self, user: String, password: SecretString) -> Result<(), SubmitError> {
        if self.busy.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire).is_err() {
            return Err(SubmitError::Busy);
        }
        match self.requests.try_send(Attempt { user, password }) {
            Ok(()) => Ok(()),
            Err(error) => {
                self.busy.store(false, Ordering::Release);
                Err(match error {
                    mpsc::TrySendError::Full(_) => SubmitError::Busy,
                    mpsc::TrySendError::Disconnected(_) => SubmitError::Stopped,
                })
            }
        }
    }
}

/// Blocking primitive. Never invoke directly on the compositor event loop.
pub fn check_password(user: &str, password: SecretString) -> Result<Outcome, Error> {
    let service = match std::fs::metadata("/etc/pam.d/tileception") {
        Ok(_) => c"tileception",
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => c"login",
        Err(_) => return Err(Error::ServiceUnavailable),
    };
    run(user, password, service, None)
}

struct Conversation {
    user: CString,
    password: Option<SecretString>,
}

fn run(user: &str, password: SecretString, service: &CStr, confdir: Option<&CStr>) -> Result<Outcome, Error> {
    let user = CString::new(user).map_err(|_| Error::InvalidUser)?;
    if user.as_bytes().is_empty() {
        return Err(Error::InvalidUser);
    }
    if password.expose_secret().as_bytes().contains(&0) {
        return Err(Error::InvalidPassword);
    }
    let mut data = Conversation { user, password: Some(password) };
    let conversation = PamConv { conv: converse, appdata_ptr: ptr::from_mut(&mut data).cast() };
    let mut handle = ptr::null_mut();
    // All strings and callback data remain alive until pam_end completes.
    let status = unsafe {
        match confdir {
            None => pam_start(service.as_ptr(), data.user.as_ptr(), &conversation, &mut handle),
            Some(dir) => start_confdir(service, &data.user, &conversation, dir, &mut handle),
        }
    };
    if status != SUCCESS {
        return Err(Error::Pam { operation: "start", code: status });
    }
    let mut transaction = Transaction { handle, status: SUCCESS };
    // A successful pam_start created this live, exclusively owned transaction.
    let status = unsafe { pam_authenticate(transaction.handle, 0) };
    // All authentication prompts have completed; PAM owns any response copies.
    data.password.take();
    transaction.status = status;
    if status != SUCCESS {
        return classify(status, false, transaction.handle);
    }
    // The authenticated handle remains live; no credentials or session are changed.
    let status = unsafe { pam_acct_mgmt(transaction.handle, 0) };
    transaction.status = status;
    classify(status, true, transaction.handle)
}

struct Transaction {
    handle: *mut c_void,
    status: c_int,
}

impl Drop for Transaction {
    fn drop(&mut self) {
        // The handle was returned by successful pam_start and is ended exactly once.
        unsafe { pam_end(self.handle, self.status) };
    }
}

fn classify(code: c_int, account: bool, handle: *mut c_void) -> Result<Outcome, Error> {
    match code {
        SUCCESS => Ok(Outcome::Success),
        9 | 11 | 24 | 30 | 31 => Ok(Outcome::TryAgainLater { code, message: pam_message(handle, code) }),
        6 | 7 | 10 if !account => Ok(Outcome::WrongPassword),
        6 | 7 | 10 | 12 | 13 => Ok(Outcome::AccountProblem { code, message: pam_message(handle, code) }),
        _ => Err(Error::Pam { operation: if account { "account" } else { "authenticate" }, code }),
    }
}

fn pam_message(handle: *mut c_void, code: c_int) -> String {
    // pam_strerror accepts a live or null PAM handle and returns library-owned text.
    unsafe {
        let message = pam_strerror(handle, code);
        if message.is_null() {
            return "PAM rejected the attempt".into();
        }
        CStr::from_ptr(message).to_string_lossy().into_owned()
    }
}

const SUCCESS: c_int = 0;
const BUF_ERR: c_int = 5;
const CONV_ERR: c_int = 19;

#[repr(C)]
struct PamMessage {
    style: c_int,
    text: *const c_char,
}

#[repr(C)]
struct PamResponse {
    text: *mut c_char,
    code: c_int,
}

#[repr(C)]
struct PamConv {
    conv: unsafe extern "C" fn(c_int, *mut *const PamMessage, *mut *mut PamResponse, *mut c_void) -> c_int,
    appdata_ptr: *mut c_void,
}

unsafe extern "C" fn converse(
    count: c_int,
    messages: *mut *const PamMessage,
    output: *mut *mut PamResponse,
    data: *mut c_void,
) -> c_int {
    if !(1..=32).contains(&count) || messages.is_null() || output.is_null() || data.is_null() {
        return CONV_ERR;
    }
    // Linux-PAM supplies count readable message pointers and our live Conversation.
    // Successful calloc/malloc buffers are C-owned on success, wiped/freed on failure.
    unsafe {
        let data = &*data.cast::<Conversation>();
        let responses = libc::calloc(count as usize, size_of::<PamResponse>()).cast::<PamResponse>();
        if responses.is_null() {
            return BUF_ERR;
        }
        for index in 0..count as usize {
            let message = *messages.add(index);
            let bytes = if message.is_null() {
                None
            } else {
                match (*message).style {
                    1 => data.password.as_ref().map(|secret| secret.expose_secret().as_bytes()),
                    2 => Some(data.user.as_bytes()),
                    // Do not retain module text, which could contain sensitive data.
                    3 | 4 => continue,
                    _ => None,
                }
            };
            let Some(bytes) = bytes else {
                clear_responses(responses, count as usize);
                return CONV_ERR;
            };
            let text = libc::malloc(bytes.len() + 1).cast::<u8>();
            if text.is_null() {
                clear_responses(responses, count as usize);
                return BUF_ERR;
            }
            ptr::copy_nonoverlapping(bytes.as_ptr(), text, bytes.len());
            *text.add(bytes.len()) = 0;
            (*responses.add(index)).text = text.cast();
        }
        *output = responses;
    }
    SUCCESS
}

unsafe fn clear_responses(responses: *mut PamResponse, count: usize) {
    // The callback allocated count zero-initialized responses, each text is null or malloc-owned.
    unsafe {
        for index in 0..count {
            let text = (*responses.add(index)).text;
            if !text.is_null() {
                let length = CStr::from_ptr(text).to_bytes_with_nul().len();
                std::slice::from_raw_parts_mut(text.cast::<u8>(), length).zeroize();
                libc::free(text.cast());
            }
        }
        libc::free(responses.cast());
    }
}

unsafe fn start_confdir(
    service: &CStr,
    user: &CStr,
    conversation: &PamConv,
    dir: &CStr,
    handle: *mut *mut c_void,
) -> c_int {
    // Resolve the optional Linux-PAM >= 1.4 symbol without raising the runtime minimum.
    unsafe {
        let symbol = libc::dlsym(libc::RTLD_DEFAULT, c"pam_start_confdir".as_ptr());
        if symbol.is_null() {
            return 3;
        }
        let start: unsafe extern "C" fn(
            *const c_char,
            *const c_char,
            *const PamConv,
            *const c_char,
            *mut *mut c_void,
        ) -> c_int = std::mem::transmute(symbol);
        start(service.as_ptr(), user.as_ptr(), conversation, dir.as_ptr(), handle)
    }
}

#[link(name = "pam")]
unsafe extern "C" {
    fn pam_start(
        service: *const c_char,
        user: *const c_char,
        conversation: *const PamConv,
        handle: *mut *mut c_void,
    ) -> c_int;
    fn pam_authenticate(handle: *mut c_void, flags: c_int) -> c_int;
    fn pam_acct_mgmt(handle: *mut c_void, flags: c_int) -> c_int;
    fn pam_end(handle: *mut c_void, status: c_int) -> c_int;
    fn pam_strerror(handle: *mut c_void, status: c_int) -> *const c_char;
}

#[cfg(test)]
mod tests;
