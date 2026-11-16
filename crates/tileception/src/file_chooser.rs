//! Choosing a file or folder through the desktop portal's file chooser
//! (`org.freedesktop.portal.FileChooser`), which `tileception-portals.conf`
//! hands to xdg-desktop-portal-gtk. The dialog is a window of its own; the
//! call waits for it on a thread, so the event loop goes on meanwhile.

use std::{collections::HashMap, path::PathBuf};

use smithay::reexports::calloop::channel;
use zbus::zvariant::{OwnedValue, Value};

use crate::Tileception;

/// What may be chosen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pick {
    /// A PNG, JPEG or WebP picture.
    Picture,
    /// A folder.
    Folder,
    /// Any file.
    File,
}

/// What the dialog gave: a path, nothing when it was cancelled, or why it
/// could not be shown.
pub type Chosen = Result<Option<PathBuf>, String>;

impl Tileception {
    /// Shows the file chooser, starting in `folder`, and calls `done` with
    /// what was chosen.
    pub fn choose_file(
        &mut self,
        title: &str,
        pick: Pick,
        folder: Option<PathBuf>,
        done: impl FnOnce(&mut Tileception, Chosen) + 'static,
    ) {
        let (sender, receiver) = channel::channel::<Chosen>();
        let mut done = Some(done);
        let registered = self.loop_handle.insert_source(receiver, move |event, _, state| {
            let chosen = match event {
                channel::Event::Msg(chosen) => chosen,
                channel::Event::Closed => Err("the file chooser ended without an answer".into()),
            };
            if let Some(done) = done.take() {
                done(state, chosen);
            }
        });
        if let Err(error) = registered {
            return tracing::error!("file chooser: {error}");
        }
        let title = title.to_string();
        let started = std::thread::Builder::new().name("tileception-file-chooser".into()).spawn(move || {
            let chosen = open_file(&title, pick, folder.as_deref()).map_err(|error| match error {
                zbus::Error::MethodError(name, _, _) if name.as_str().ends_with("ServiceUnknown") => {
                    "no desktop portal is running (xdg-desktop-portal with its GTK backend)".to_string()
                }
                other => other.to_string(),
            });
            let _ = sender.send(chosen);
        });
        if let Err(error) = started {
            tracing::error!("file chooser: {error}");
        }
    }
}

fn open_file(title: &str, pick: Pick, folder: Option<&std::path::Path>) -> zbus::Result<Option<PathBuf>> {
    let bus = zbus::blocking::Connection::session()?;
    // The reply names the request object; it is known in advance from the
    // token, so its answer is listened for before asking and cannot be missed.
    let token = format!("tileception{}", std::process::id());
    let sender = bus.unique_name().map(|name| name.trim_start_matches(':').replace('.', "_")).unwrap_or_default();
    let request_path = format!("/org/freedesktop/portal/desktop/request/{sender}/{token}");
    let request = zbus::blocking::proxy::Builder::<zbus::blocking::Proxy>::new(&bus)
        .destination("org.freedesktop.portal.Desktop")?
        .path(request_path)?
        .interface("org.freedesktop.portal.Request")?
        .cache_properties(zbus::proxy::CacheProperties::No)
        .build()?;
    let mut responses = request.receive_signal("Response")?;

    let mut options: HashMap<&str, Value<'_>> = HashMap::new();
    options.insert("handle_token", Value::from(token.as_str()));
    options.insert("modal", Value::from(true));
    match pick {
        Pick::Folder => {
            options.insert("directory", Value::from(true));
        }
        Pick::Picture => {
            let types = ["image/png", "image/jpeg", "image/webp"].map(|mime| (1u32, mime.to_string())).to_vec();
            options.insert("filters", Value::from(vec![("Pictures".to_string(), types)]));
        }
        Pick::File => {}
    }
    if let Some(folder) = folder {
        use std::os::unix::ffi::OsStrExt;
        let mut bytes = folder.as_os_str().as_bytes().to_vec();
        bytes.push(0);
        options.insert("current_folder", Value::from(bytes));
    }
    let chooser = zbus::blocking::Proxy::new(
        &bus,
        "org.freedesktop.portal.Desktop",
        "/org/freedesktop/portal/desktop",
        "org.freedesktop.portal.FileChooser",
    )?;
    let _handle: zbus::zvariant::OwnedObjectPath = chooser.call("OpenFile", &("", title, options))?;

    let Some(message) = responses.next() else { return Ok(None) };
    let (response, results): (u32, HashMap<String, OwnedValue>) = message.body().deserialize()?;
    // 0 is chosen, 1 cancelled, 2 ended some other way.
    if response != 0 {
        return Ok(None);
    }
    let uris: Vec<String> = match results.get("uris") {
        Some(value) => value.try_clone()?.try_into()?,
        None => Vec::new(),
    };
    Ok(uris.first().and_then(|uri| crate::drag::file_path(uri)))
}

/// `path` with the home folder written as `~/`, as the config file takes it.
pub fn tilde(path: &std::path::Path) -> String {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    match home.as_deref().and_then(|home| path.strip_prefix(home).ok()) {
        Some(rest) if !rest.as_os_str().is_empty() => format!("~/{}", rest.display()),
        _ => path.display().to_string(),
    }
}
