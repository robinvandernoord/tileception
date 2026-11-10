//! Optional persistence for the regular clipboard, managed with the config,
//! and lending the clipboard: Tileception puts a picture on it for one
//! paste, then gives back what was copied before.
//!
//! Lending first reads every type the clipboard offers (bounded in size and
//! time), since the program that copied it lets go once something else is
//! copied. The previous contents come back shortly after Tileception wrote
//! out a paste of the picture in full, or after [`LEND_FOR`], whichever is
//! first, unless something else was copied in between: the newer copy wins.
//! Clipboard tools (history, persistence) read new contents at once, so
//! reads within [`SETTLE`] are not taken for the paste. With persistence on,
//! its helper takes the picture over at once and serves the paste itself,
//! so only the timeout gives the clipboard back. An empty clipboard and
//! another machine's selection stay replaced. The primary selection is never
//! touched.

use std::{
    io::Read,
    os::{fd::OwnedFd, unix::net::UnixStream},
    process::Child,
    sync::Arc,
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};
use smithay::{
    reexports::calloop::{
        channel,
        timer::{TimeoutAction, Timer},
    },
    wayland::selection::{
        SelectionTarget,
        data_device::{current_data_device_selection_userdata, set_data_device_selection},
    },
};

use crate::{Tileception, launcher::Clip};

/// How long a lent clipboard waits for its paste. Long enough to open the
/// browser, sign in and paste; short enough that the previous contents are
/// back before they are likely to be wanted again.
pub(crate) const LEND_FOR: Duration = Duration::from_secs(120);
/// Clipboard tools (persistence, history) read new contents within this;
/// a paste by hand comes later.
const SETTLE: Duration = Duration::from_secs(2);
/// After the paste, time for the program to ask for the picture again.
const AFTER_PASTE: Duration = Duration::from_secs(1);
/// The most of the previous contents kept, over all its types; the types
/// that do not fit are left out.
const SAVE_LIMIT: usize = 32 * 1024 * 1024;
/// The most types kept.
const SAVE_TYPES: usize = 32;
/// Reading the previous contents gives up after this.
const SAVE_TIME: Duration = Duration::from_secs(3);

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct ClipboardConfig {
    pub persist: bool,
}

#[derive(Default)]
pub struct Clipboard {
    child: Option<Child>,
    lent: Option<Loan>,
    loans: u64,
    /// Told the loan whose picture was pasted in full.
    pasted: Option<channel::Sender<u64>>,
}

enum Loan {
    /// Reading the previous contents; the picture goes on once they are read.
    Saving { loan: u64, png: Arc<[u8]>, version: u64 },
    Lent {
        loan: u64,
        png: Arc<[u8]>,
        /// The types and contents to give back.
        previous: (Vec<String>, Clip),
        since: Instant,
        pasted: bool,
        /// Persistence's helper took the picture over.
        kept: bool,
    },
}

/// Whether a new clipboard selection, while the picture is lent, is
/// persistence's helper taking the picture over rather than a new copy. The
/// picture itself never counts: it goes on before the loan starts, and
/// Smithay reports a new selection before it replaces the current one.
fn kept_by_helper(persisting: bool, kept: bool, since: Duration, mime_types: &[String]) -> bool {
    persisting && !kept && since < SETTLE && mime_types == ["image/png"]
}

/// Reads each type in turn, leaving out what fails, is empty or does not
/// fit in `limit` (and every type after one that does not), until `deadline`.
fn read_all(readers: Vec<(String, UnixStream)>, limit: usize, deadline: Instant) -> Vec<(String, Vec<u8>)> {
    let mut saved = Vec::new();
    let mut left = limit;
    'types: for (mime_type, mut reader) in readers {
        let mut bytes = Vec::new();
        let mut chunk = vec![0; 64 * 1024];
        loop {
            let Some(time) = deadline.checked_duration_since(Instant::now()).filter(|time| !time.is_zero()) else {
                break 'types;
            };
            if reader.set_read_timeout(Some(time)).is_err() {
                continue 'types;
            }
            match reader.read(&mut chunk) {
                Ok(0) => break,
                Ok(length) if bytes.len() + length > left => break 'types,
                Ok(length) => bytes.extend_from_slice(&chunk[..length]),
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                Err(_) => continue 'types,
            }
        }
        if !bytes.is_empty() {
            left -= bytes.len();
            saved.push((mime_type, bytes));
        }
    }
    saved
}

impl Clipboard {
    pub fn stop(&mut self) {
        if let Some(mut child) = self.child.take() {
            // Kill only our helper, then reap it before another can start.
            if let Err(err) = child.kill() {
                tracing::warn!("could not stop clipboard persistence: {err}");
            }
            if let Err(err) = child.wait() {
                tracing::warn!("could not reap clipboard persistence: {err}");
            }
        }
    }

    fn check(&mut self) {
        let Some(child) = self.child.as_mut() else { return };
        match child.try_wait() {
            Ok(Some(status)) => {
                self.child = None;
                tracing::error!("clipboard persistence exited ({status}); reload the config to retry");
            }
            Ok(None) => {}
            Err(err) => tracing::error!("could not check clipboard persistence: {err}"),
        }
    }
}

impl Drop for Clipboard {
    fn drop(&mut self) {
        self.stop();
    }
}

impl Tileception {
    pub fn init_clipboard(&mut self) {
        self.apply_clipboard();
        let (sender, receiver) = channel::channel::<u64>();
        self.loop_handle
            .insert_source(receiver, |event, _, state| {
                if let channel::Event::Msg(loan) = event {
                    state.clipboard_pasted(loan);
                }
            })
            .expect("clipboard paste watcher");
        self.clipboard.pasted = Some(sender);
        self.loop_handle
            .insert_source(Timer::from_duration(Duration::from_secs(1)), |_, _, state| {
                state.clipboard.check();
                TimeoutAction::ToDuration(Duration::from_secs(1))
            })
            .expect("clipboard helper watcher");
    }

    pub fn apply_clipboard(&mut self) {
        self.clipboard.check();
        if !self.config.file.clipboard.persist {
            self.clipboard.stop();
            return;
        }
        // The greeter never retains clipboard contents.
        if self.greeter.is_some() || self.clipboard.child.is_some() {
            return;
        }
        // Installed next to the compositor, with a private name so installation
        // never replaces a separately managed wl-clip-persist. Source-only
        // development builds can still use an upstream helper from PATH.
        let helper = std::env::current_exe()
            .ok()
            .and_then(|exe| exe.parent().map(|directory| directory.join("tileception-clip-persist")))
            .filter(|path| path.is_file());
        let program = helper.as_deref().map_or_else(|| "wl-clip-persist".into(), |path| path.to_string_lossy());
        match self.command(&program, &["--clipboard".into(), "regular".into()]).spawn() {
            Ok(child) => {
                self.clipboard.child = Some(child);
                tracing::info!("clipboard persistence started");
            }
            Err(err) => tracing::error!(
                "could not start clipboard persistence ({program}): {err}; rebuild with make build or reinstall Tileception, then reload the config"
            ),
        }
    }

    /// Puts `png` on the clipboard for one paste, then gives back what was
    /// there before (see the module documentation).
    pub(crate) fn lend_clipboard_png(&mut self, png: Arc<[u8]>) {
        self.clipboard.loans += 1;
        let loan = self.clipboard.loans;
        match self.clipboard.lent.take() {
            // Lent before and not given back yet: what came before that
            // comes back, after this picture instead.
            Some(Loan::Lent { previous, .. }) => return self.lent(loan, png, Some(previous)),
            Some(Loan::Saving { loan, version, .. }) => {
                self.clipboard.lent = Some(Loan::Saving { loan, png, version });
                return;
            }
            None => {}
        }
        // Another machine's selection is that machine's to keep.
        let Some((version, mime_types)) = self.clipboard_offered_here() else { return self.lent(loan, png, None) };
        let owned = current_data_device_selection_userdata(&self.seat).and_then(|data| data.clone());
        match owned {
            Some(Clip::Remote(_)) => return self.lent(loan, png, None),
            // Tileception's own: kept as it is.
            Some(clip) => return self.lent(loan, png, Some((mime_types, clip))),
            // Nothing was copied: the picture stays.
            None if mime_types.is_empty() => return self.lent(loan, png, None),
            None => {}
        }
        let readers: Vec<_> = mime_types
            .into_iter()
            .take(SAVE_TYPES)
            .filter_map(|mime_type| {
                let reader = self.read_selection(SelectionTarget::Clipboard, mime_type.clone())?;
                Some((mime_type, reader))
            })
            .collect();
        let (sender, receiver) = channel::channel::<Vec<(String, Vec<u8>)>>();
        let registered = self.loop_handle.insert_source(receiver, move |event, _, state| {
            let saved = match event {
                channel::Event::Msg(saved) => saved,
                channel::Event::Closed => Vec::new(),
            };
            state.clipboard_saved(loan, saved);
        });
        let spawned = registered.is_ok()
            && std::thread::Builder::new()
                .name("tileception-clipboard-save".into())
                .spawn(move || {
                    let _ = sender.send(read_all(readers, SAVE_LIMIT, Instant::now() + SAVE_TIME));
                })
                .is_ok();
        if spawned {
            self.clipboard.lent = Some(Loan::Saving { loan, png, version });
        } else {
            tracing::warn!("clipboard: could not save its contents; the picture replaces them");
            self.lent(loan, png, None);
        }
    }

    fn clipboard_saved(&mut self, loan: u64, saved: Vec<(String, Vec<u8>)>) {
        let Some(Loan::Saving { png, version, .. }) =
            self.clipboard.lent.take_if(|lent| matches!(lent, Loan::Saving { loan: saving, .. } if *saving == loan))
        else {
            return;
        };
        // Copied again while it was read: that copy is not what was saved.
        let unchanged = self.clipboard_offered_here().is_some_and(|(now, _)| now == version);
        let previous = (unchanged && !saved.is_empty()).then(|| {
            let mime_types = saved.iter().map(|(mime_type, _)| mime_type.clone()).collect();
            (mime_types, Clip::Saved(saved.into()))
        });
        if previous.is_none() {
            tracing::info!("clipboard: nothing saved to give back after the picture");
        }
        self.lent(loan, png, previous);
    }

    fn lent(&mut self, loan: u64, png: Arc<[u8]>, previous: Option<(Vec<String>, Clip)>) {
        self.copy_png(png.clone());
        let Some(previous) = previous else { return };
        self.clipboard.lent =
            Some(Loan::Lent { loan, png, previous, since: Instant::now(), pasted: false, kept: false });
        self.give_clipboard_back_after(loan, LEND_FOR);
    }

    fn give_clipboard_back_after(&mut self, loan: u64, delay: Duration) {
        let timer = self.loop_handle.insert_source(Timer::from_duration(delay), move |_, _, state| {
            state.give_clipboard_back(loan);
            TimeoutAction::Drop
        });
        if let Err(error) = timer {
            tracing::warn!("clipboard: could not wait to give it back: {error}");
            self.give_clipboard_back(loan);
        }
    }

    fn give_clipboard_back(&mut self, loan: u64) {
        let Some(Loan::Lent { previous: (mime_types, clip), .. }) =
            self.clipboard.lent.take_if(|lent| matches!(lent, Loan::Lent { loan: lent, .. } if *lent == loan))
        else {
            return;
        };
        set_data_device_selection(&self.display_handle, &self.seat, mime_types.clone(), Some(clip));
        self.selection_offered(SelectionTarget::Clipboard, Some(mime_types.clone()));
        if let Some(xwm) = self.xwm.as_mut()
            && let Err(error) = xwm.new_selection(SelectionTarget::Clipboard, Some(mime_types))
        {
            tracing::warn!("could not offer the clipboard back to X11: {error:?}");
        }
        tracing::info!("clipboard: gave back what was copied before the picture");
    }

    /// Serves `clip`, which Tileception owns, to a program pasting it as
    /// `mime_type`. Once a lent picture was written out in full, the
    /// clipboard is given back.
    pub(crate) fn serve_clip(&mut self, clip: &Clip, mime_type: &str, pipe: OwnedFd) {
        let paste = match (&self.clipboard.lent, clip, &self.clipboard.pasted) {
            (Some(Loan::Lent { loan, png, since, pasted: false, .. }), Clip::Png(served), Some(sender))
                if mime_type == "image/png" && Arc::ptr_eq(served, png) && since.elapsed() >= SETTLE =>
            {
                Some((*loan, sender.clone()))
            }
            _ => None,
        };
        match paste {
            Some((loan, sender)) => clip.send_then(mime_type, pipe, move || {
                let _ = sender.send(loan);
            }),
            None => clip.send(mime_type, pipe),
        }
    }

    fn clipboard_pasted(&mut self, loan: u64) {
        let Some(Loan::Lent { loan: lent, pasted, .. }) = &mut self.clipboard.lent else { return };
        if *lent != loan || *pasted {
            return;
        }
        *pasted = true;
        self.give_clipboard_back_after(loan, AFTER_PASTE);
    }

    /// The clipboard now offers `mime_types`, from whoever copied last.
    pub(crate) fn clipboard_replaced(&mut self, mime_types: &[String]) {
        let persisting = self.clipboard.child.is_some();
        let Some(Loan::Lent { since, kept, .. }) = &mut self.clipboard.lent else { return };
        if kept_by_helper(persisting, *kept, since.elapsed(), mime_types) {
            *kept = true;
        } else {
            self.clipboard.lent = None;
            tracing::info!("clipboard: copied again, so the earlier contents stay replaced");
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

    #[test]
    fn tells_the_picture_from_a_new_copy() {
        let png = vec!["image/png".to_string()];
        let text = vec!["text/plain".to_string()];
        let soon = Duration::from_millis(50);
        // Persistence takes the picture over at once, and only once.
        assert!(kept_by_helper(true, false, soon, &png));
        assert!(!kept_by_helper(true, true, soon, &png));
        assert!(!kept_by_helper(true, false, SETTLE, &png));
        assert!(!kept_by_helper(true, false, soon, &text));
        assert!(!kept_by_helper(false, false, soon, &png));
    }

    fn offered(bytes: &'static [u8]) -> UnixStream {
        let (reader, mut writer) = UnixStream::pair().expect("a pair");
        std::thread::spawn(move || {
            let _ = writer.write_all(bytes);
        });
        reader
    }

    #[test]
    fn saves_every_type_that_fits() {
        let readers = vec![
            ("text/plain".to_string(), offered(b"hello")),
            ("text/html".to_string(), offered(b"")),
            ("image/png".to_string(), offered(b"0123456789")),
            ("image/bmp".to_string(), offered(b"bmp")),
        ];
        // The empty type is left out; the picture does not fit, so neither
        // does anything after it.
        let saved = read_all(readers, 12, Instant::now() + Duration::from_secs(3));
        assert_eq!(saved, vec![("text/plain".to_string(), b"hello".to_vec())]);
        let readers = vec![("text/plain".to_string(), offered(b"hello")), ("image/png".to_string(), offered(b"png"))];
        let saved = read_all(readers, 12, Instant::now() + Duration::from_secs(3));
        assert_eq!(saved.len(), 2);
    }

    #[test]
    fn gives_up_when_a_type_stalls() {
        let (stalled, _writer) = UnixStream::pair().expect("a pair");
        let started = Instant::now();
        let saved = read_all(
            vec![("text/plain".to_string(), stalled), ("text/html".to_string(), offered(b"late"))],
            1024,
            Instant::now() + Duration::from_millis(200),
        );
        assert!(saved.is_empty());
        assert!(started.elapsed() < Duration::from_secs(2));
    }
}
