//! Pastes what an X11 program copied, in any type.
//!
//! smithay 0.7's `X11Wm::send_selection` looks the X atom for every type but
//! plain text up in a property it never sets, so a picture, HTML or a file
//! list copied in an X11 program (a browser or an Electron app under
//! Xwayland) never reached a Wayland one: only text did. This asks the X
//! server for the selection itself, on a thread with a connection of its own,
//! so a slow or large transfer does not hold up the compositor.

use std::{
    io::Write as _,
    os::fd::OwnedFd,
    time::{Duration, Instant},
};

use smithay::wayland::selection::SelectionTarget;
use x11rb::{
    connection::Connection as _,
    protocol::{
        Event,
        xproto::{AtomEnum, ConnectionExt as _, CreateWindowAux, EventMask, Property, WindowClass},
    },
    rust_connection::RustConnection,
};

use crate::Tileception;

/// How long the program that owns the selection may take to answer, for the
/// first answer and for each part of a large transfer.
const ANSWER_TIMEOUT: Duration = Duration::from_secs(5);

/// Where the owner puts what we asked for, on our own window.
const PROPERTY: &[u8] = b"_TILECEPTION_PASTE";

impl Tileception {
    /// Writes the X11 `selection`, as `mime_type`, into `pipe`. False when
    /// there is no Xwayland to ask, or no thread to ask it on.
    pub(crate) fn send_x11_selection(&self, selection: SelectionTarget, mime_type: String, pipe: OwnedFd) -> bool {
        let Some(display) = self.xdisplay else { return false };
        std::thread::Builder::new()
            .name("x11-paste".into())
            .spawn(move || {
                if let Err(error) = transfer(display, selection, &mime_type, pipe) {
                    tracing::warn!("could not paste {mime_type} from X11: {error}");
                }
            })
            .map_err(|error| tracing::warn!("could not start pasting from X11: {error}"))
            .is_ok()
    }
}

/// The X target for a MIME type: X names plain text its own way, and takes
/// every other MIME type as the target's name.
fn target_name(mime_type: &str) -> &str {
    match mime_type {
        "text/plain;charset=utf-8" => "UTF8_STRING",
        "text/plain" => "TEXT",
        other => other,
    }
}

fn transfer(display: u32, selection: SelectionTarget, mime_type: &str, pipe: OwnedFd) -> Result<(), String> {
    let (connection, screen) =
        RustConnection::connect(Some(&format!(":{display}"))).map_err(|error| format!("connecting: {error}"))?;
    let root = connection.setup().roots.get(screen).ok_or("no X screen")?.root;
    let intern = |name: &[u8]| -> Result<u32, String> {
        Ok(connection
            .intern_atom(false, name)
            .map_err(|error| error.to_string())?
            .reply()
            .map_err(|error| error.to_string())?
            .atom)
    };
    let selection_atom = intern(match selection {
        SelectionTarget::Clipboard => b"CLIPBOARD",
        SelectionTarget::Primary => b"PRIMARY",
    })?;
    let target = intern(target_name(mime_type).as_bytes())?;
    let property = intern(PROPERTY)?;
    let incr = intern(b"INCR")?;

    let window = connection.generate_id().map_err(|error| error.to_string())?;
    connection
        .create_window(
            x11rb::COPY_DEPTH_FROM_PARENT,
            window,
            root,
            0,
            0,
            1,
            1,
            0,
            WindowClass::INPUT_ONLY,
            x11rb::COPY_FROM_PARENT,
            &CreateWindowAux::new().event_mask(EventMask::PROPERTY_CHANGE),
        )
        .map_err(|error| error.to_string())?;
    connection
        .convert_selection(window, selection_atom, target, property, x11rb::CURRENT_TIME)
        .map_err(|error| error.to_string())?;
    connection.flush().map_err(|error| error.to_string())?;

    let mut out = crate::launcher::paste_pipe(pipe);
    let answered = wait_for(&connection, |event| match event {
        Event::SelectionNotify(notify) if notify.requestor == window => Some(notify.property),
        _ => None,
    })?;
    if answered == u32::from(AtomEnum::NONE) {
        return Err("the program that copied it does not offer it".into());
    }
    let read = |connection: &RustConnection| {
        // Deleting the property tells the owner of a large selection to put
        // the next part there.
        connection
            .get_property(true, window, property, AtomEnum::ANY, 0, 0x1fff_ffff)
            .map_err(|error| error.to_string())?
            .reply()
            .map_err(|error| error.to_string())
    };
    let first = read(&connection)?;
    if first.type_ != incr {
        return out.write_all(&first.value).map_err(|error| error.to_string());
    }
    // Too large for one request: the owner sends it in parts, each a new
    // value of the property, and ends with an empty one.
    connection.flush().map_err(|error| error.to_string())?;
    loop {
        wait_for(&connection, |event| match event {
            Event::PropertyNotify(notify)
                if notify.window == window && notify.atom == property && notify.state == Property::NEW_VALUE =>
            {
                Some(())
            }
            _ => None,
        })?;
        let part = read(&connection)?;
        connection.flush().map_err(|error| error.to_string())?;
        if part.value.is_empty() {
            return Ok(());
        }
        out.write_all(&part.value).map_err(|error| error.to_string())?;
    }
}

/// The first event `matches` accepts, ignoring others, until the owner has
/// taken too long.
fn wait_for<T>(connection: &RustConnection, mut matches: impl FnMut(&Event) -> Option<T>) -> Result<T, String> {
    let deadline = Instant::now() + ANSWER_TIMEOUT;
    loop {
        match connection.poll_for_event().map_err(|error| error.to_string())? {
            Some(event) => {
                if let Some(found) = matches(&event) {
                    return Ok(found);
                }
            }
            None if Instant::now() >= deadline => return Err("the program that copied it did not answer".into()),
            None => std::thread::sleep(Duration::from_millis(2)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::target_name;

    #[test]
    fn plain_text_has_x_names_and_other_types_keep_theirs() {
        assert_eq!(target_name("text/plain;charset=utf-8"), "UTF8_STRING");
        assert_eq!(target_name("text/plain"), "TEXT");
        assert_eq!(target_name("image/png"), "image/png");
        assert_eq!(target_name("text/html"), "text/html");
    }
}
