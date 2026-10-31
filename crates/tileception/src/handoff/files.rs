//! Files dragged or copied to another machine.
//!
//! A list of files names them by their paths on the machine they are on,
//! which mean nothing elsewhere. So when a program wants another machine's
//! dragged or copied files, they are copied over first, into a folder of
//! dropped files here (`~/.cache/tileception/handoff-drops`, cleared of drops
//! older than a week), and the program gets the list of those copies.

use std::{
    collections::HashMap,
    io::{Read, Write},
    os::{fd::OwnedFd, unix::net::UnixStream},
    path::{Path, PathBuf},
    sync::mpsc,
    time::{Duration, SystemTime},
};

use tileception_handoff::{CHUNK, Message, Messenger, PeerId};

use super::clipboard::STALL;
use crate::{
    Tileception,
    drag::{file_path, file_uri, parse_uri_list},
};

const URI_LIST: &str = "text/uri-list";
/// The files of a copy or cut, as GNOME-style file managers paste them.
const GNOME_COPIED: &str = "x-special/gnome-copied-files";
/// Types that only make sense on the machine they come from: keys to the
/// file transfer portal there.
const LOCAL_ONLY: [&str; 2] = ["application/vnd.portal.filetransfer", "application/vnd.portal.files"];
/// Other lists of the files, by their paths on the machine they come from.
/// Programs prefer these to `text/uri-list`, but nothing copies the files
/// for them, so they are left out when that is there.
const FILE_LISTS: [&str; 3] = ["x-special/gnome-icon-list", "application/x-kde4-urilist", "application/x-kde-urilist"];
/// Dropped files older than this are cleared away.
const KEEP_DROPS: Duration = Duration::from_secs(7 * 24 * 60 * 60);
/// The most entries one drop of files copies.
const MOST_ENTRIES: usize = 100_000;

/// The types another machine's drag or selection is offered in here: none
/// that only work there, and files only in the types that copy them over.
pub(crate) fn offered_types(mime_types: Vec<String>) -> Vec<String> {
    let files = mime_types.iter().any(|mime| mime == URI_LIST);
    mime_types
        .into_iter()
        .filter(|mime| !LOCAL_ONLY.contains(&mime.as_str()) && !(files && FILE_LISTS.contains(&mime.as_str())))
        .collect()
}

/// How a program here wants the copied files listed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FileList {
    Uris,
    GnomeCopied,
}

impl FileList {
    /// Whether a program asking for `mime_type`, of something offered in
    /// `offered`, wants files, which are then copied over.
    pub(crate) fn wanted(mime_type: &str, offered: &[String]) -> Option<Self> {
        if !offered.iter().any(|mime| mime == URI_LIST) {
            return None;
        }
        match mime_type {
            URI_LIST => Some(Self::Uris),
            GNOME_COPIED => Some(Self::GnomeCopied),
            _ => None,
        }
    }

    fn write(self, uris: &[String]) -> String {
        match self {
            Self::Uris => uris.iter().map(|uri| format!("{uri}\r\n")).collect(),
            // The copies are new files here, so a cut there pastes as a copy.
            Self::GnomeCopied => {
                std::iter::once("copy").chain(uris.iter().map(String::as_str)).collect::<Vec<_>>().join("\n")
            }
        }
    }
}

/// Copies of files on their way here, by request.
#[derive(Default)]
pub(crate) struct Files {
    incoming: HashMap<u64, (PeerId, mpsc::Sender<Piece>)>,
}

/// Part of a copy of files arriving.
enum Piece {
    Entry { path: String, directory: bool },
    Link(String),
    Bytes(Vec<u8>),
}

impl Files {
    /// Whether `request` is a copy of files arriving.
    pub(crate) fn receives(&self, request: u64) -> bool {
        self.incoming.contains_key(&request)
    }

    pub(crate) fn entry(&mut self, request: u64, path: String, directory: bool) {
        self.piece(request, Piece::Entry { path, directory });
    }

    pub(crate) fn link(&mut self, request: u64, uri: String) {
        self.piece(request, Piece::Link(uri));
    }

    pub(crate) fn data(&mut self, request: u64, bytes: Vec<u8>) {
        self.piece(request, Piece::Bytes(bytes));
    }

    /// The last of a copy of files; dropping the sender finishes it.
    pub(crate) fn end(&mut self, request: u64, failed: bool) {
        if failed {
            tracing::warn!("handoff: copying files failed on the other machine");
        }
        self.incoming.remove(&request);
    }

    /// The copies coming from a machine that went away stop.
    pub(crate) fn forget(&mut self, peer: PeerId) {
        self.incoming.retain(|_, (from, _)| *from != peer);
    }

    fn piece(&mut self, request: u64, piece: Piece) {
        let delivered = self.incoming.get(&request).is_some_and(|(_, sender)| sender.send(piece).is_ok());
        if !delivered {
            self.incoming.remove(&request);
        }
    }
}

impl Tileception {
    /// Sends another machine the files `list` (a `text/uri-list` read from
    /// a program here) names, as the answer to its `request`.
    pub(crate) fn serve_files(&mut self, peer: PeerId, request: u64, list: Option<UnixStream>) {
        let messenger = self.handoff.messenger();
        let (Some(list), Some(messenger)) = (list, messenger) else {
            self.handoff.send(peer, Message::DataEnd { request, failed: true });
            return;
        };
        let started = std::thread::Builder::new().name("tileception-handoff-files".into()).spawn(move || {
            let failed = send_files(&messenger, peer, request, list).is_err();
            messenger.send_bulk(peer, Message::DataEnd { request, failed });
        });
        if started.is_err() {
            self.handoff.send(peer, Message::DataEnd { request, failed: true });
        }
    }

    /// Copies another machine's files here, asking for them with `message`,
    /// then gives the program that wants them the copies as `list`.
    pub(crate) fn receive_files_into(
        &mut self,
        peer: PeerId,
        pipe: OwnedFd,
        list: FileList,
        message: impl FnOnce(u64) -> Message,
    ) {
        let request = self.handoff.clipboard.next_request();
        let (sender, receiver) = mpsc::channel::<Piece>();
        let started = std::thread::Builder::new().name("tileception-handoff-drop".into()).spawn(move || {
            match receive_files(&drops_folder(), request, &receiver) {
                Ok(uris) => {
                    let _ = std::fs::File::from(pipe).write_all(list.write(&uris).as_bytes());
                }
                Err(error) => tracing::warn!("handoff: copied files: {error}"),
            }
        });
        if started.is_err() {
            return;
        }
        self.handoff.files.incoming.insert(request, (peer, sender));
        if !self.handoff.send(peer, message(request)) {
            self.handoff.files.incoming.remove(&request);
        }
    }
}

/// Sends the files a `text/uri-list` names, folders with everything in
/// them; other entries go as they are.
fn send_files(messenger: &Messenger, peer: PeerId, request: u64, mut list: UnixStream) -> Result<(), String> {
    let mut text = String::new();
    list.read_to_string(&mut text).map_err(|error| format!("the list of files: {error}"))?;
    let mut entries = 0;
    for uri in parse_uri_list(&text) {
        let Some(path) = file_path(&uri) else {
            send(messenger, peer, Message::DragLink { request, uri })?;
            continue;
        };
        let name = path.file_name().map(|name| name.to_string_lossy().into_owned()).ok_or("a file without a name")?;
        send_entry(messenger, peer, request, &path, &name, &mut entries)?;
    }
    Ok(())
}

fn send(messenger: &Messenger, peer: PeerId, message: Message) -> Result<(), String> {
    if messenger.send_bulk(peer, message) { Ok(()) } else { Err("the other machine went away".into()) }
}

/// One file, or a folder and what is in it. Links inside folders are left
/// out, so nothing outside what was dragged goes along.
fn send_entry(
    messenger: &Messenger,
    peer: PeerId,
    request: u64,
    path: &Path,
    relative: &str,
    entries: &mut usize,
) -> Result<(), String> {
    *entries += 1;
    if *entries > MOST_ENTRIES {
        return Err("too many files".into());
    }
    let metadata = std::fs::metadata(path).map_err(|error| format!("{}: {error}", path.display()))?;
    if metadata.is_dir() {
        send(messenger, peer, Message::DragFile { request, path: relative.to_string(), directory: true })?;
        let read = std::fs::read_dir(path).map_err(|error| format!("{}: {error}", path.display()))?;
        for entry in read.flatten() {
            let Ok(kind) = entry.file_type() else { continue };
            if kind.is_symlink() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            send_entry(messenger, peer, request, &entry.path(), &format!("{relative}/{name}"), entries)?;
        }
        return Ok(());
    }
    if !metadata.is_file() {
        return Ok(());
    }
    let mut file = std::fs::File::open(path).map_err(|error| format!("{}: {error}", path.display()))?;
    send(messenger, peer, Message::DragFile { request, path: relative.to_string(), directory: false })?;
    loop {
        let mut chunk = vec![0; CHUNK];
        let length = file.read(&mut chunk).map_err(|error| format!("{}: {error}", path.display()))?;
        if length == 0 {
            return Ok(());
        }
        chunk.truncate(length);
        send(messenger, peer, Message::Data { request, bytes: chunk })?;
    }
}

/// Where files from other machines go.
fn drops_folder() -> PathBuf {
    std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".cache"))
        .join("tileception/handoff-drops")
}

/// Writes the pieces of a copy of files into a folder of its own, returning
/// the list for the program that wants them.
fn receive_files(drops: &Path, request: u64, pieces: &mpsc::Receiver<Piece>) -> Result<Vec<String>, String> {
    clear_old_drops(drops);
    let stamp = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap_or_default().as_secs();
    let root = drops.join(format!("{stamp}-{request}"));
    std::fs::create_dir_all(&root).map_err(|error| format!("{}: {error}", root.display()))?;
    let mut uris = Vec::new();
    let mut file: Option<std::fs::File> = None;
    loop {
        let piece = match pieces.recv_timeout(STALL) {
            Ok(piece) => piece,
            Err(mpsc::RecvTimeoutError::Disconnected) => return Ok(uris),
            Err(mpsc::RecvTimeoutError::Timeout) => return Err("the other machine stopped sending".into()),
        };
        match piece {
            Piece::Entry { path, directory } => {
                let relative = safe_relative(&path).ok_or_else(|| format!("refused a file called {path:?}"))?;
                let target = root.join(&relative);
                if relative.components().count() == 1 {
                    uris.push(file_uri(&target));
                }
                file = None;
                if directory {
                    std::fs::create_dir_all(&target).map_err(|error| format!("{}: {error}", target.display()))?;
                } else {
                    if let Some(parent) = target.parent() {
                        std::fs::create_dir_all(parent).map_err(|error| format!("{}: {error}", parent.display()))?;
                    }
                    let created =
                        std::fs::File::create(&target).map_err(|error| format!("{}: {error}", target.display()))?;
                    file = Some(created);
                }
            }
            Piece::Link(uri) => {
                file = None;
                uris.push(uri);
            }
            Piece::Bytes(bytes) => {
                if let Some(file) = file.as_mut() {
                    file.write_all(&bytes).map_err(|error| error.to_string())?;
                }
            }
        }
    }
}

/// A path within a drop, when it stays within it: plain names only.
fn safe_relative(path: &str) -> Option<PathBuf> {
    let parts: Vec<&str> = path.split('/').collect();
    let plain = |part: &&str| !part.is_empty() && *part != "." && *part != ".." && !part.contains('\0');
    (parts.len() <= 64 && parts.iter().all(plain)).then(|| parts.iter().collect())
}

/// Removes drops older than a week.
fn clear_old_drops(drops: &Path) {
    let Ok(entries) = std::fs::read_dir(drops) else { return };
    for entry in entries.flatten() {
        let old = entry
            .metadata()
            .and_then(|metadata| metadata.modified())
            .ok()
            .and_then(|modified| modified.elapsed().ok())
            .is_some_and(|age| age > KEEP_DROPS);
        if old {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn types(list: &[&str]) -> Vec<String> {
        list.iter().map(|mime| (*mime).to_string()).collect()
    }

    #[test]
    fn files_are_only_offered_in_types_that_copy_them() {
        let nemo = types(&["x-special/gnome-icon-list", "x-special/gnome-copied-files", "text/uri-list", "text/plain"]);
        assert_eq!(offered_types(nemo), types(&["x-special/gnome-copied-files", "text/uri-list", "text/plain"]));
        let dolphin = types(&["application/x-kde4-urilist", "text/uri-list", "application/vnd.portal.filetransfer"]);
        assert_eq!(offered_types(dolphin), types(&["text/uri-list"]));
        // Without a uri-list to copy them by, nothing better is on offer.
        let odd = types(&["x-special/gnome-icon-list"]);
        assert_eq!(offered_types(odd.clone()), odd);
    }

    #[test]
    fn copied_files_are_listed_as_asked() {
        let offered = types(&["x-special/gnome-copied-files", "text/uri-list"]);
        assert_eq!(FileList::wanted("text/uri-list", &offered), Some(FileList::Uris));
        assert_eq!(FileList::wanted("x-special/gnome-copied-files", &offered), Some(FileList::GnomeCopied));
        assert_eq!(FileList::wanted("text/plain", &offered), None);
        assert_eq!(FileList::wanted("x-special/gnome-copied-files", &offered[..1]), None);
        let uris = types(&["file:///a", "file:///b"]);
        assert_eq!(FileList::Uris.write(&uris), "file:///a\r\nfile:///b\r\n");
        assert_eq!(FileList::GnomeCopied.write(&uris), "copy\nfile:///a\nfile:///b");
    }

    #[test]
    fn dropped_paths_stay_inside_the_drop() {
        assert_eq!(safe_relative("photos/2026/a.png"), Some(PathBuf::from("photos/2026/a.png")));
        assert_eq!(safe_relative("../etc/passwd"), None);
        assert_eq!(safe_relative("photos/../../x"), None);
        assert_eq!(safe_relative("/etc/passwd"), None);
        assert_eq!(safe_relative("a//b"), None);
        assert_eq!(safe_relative("./a"), None);
        assert_eq!(safe_relative(""), None);
    }

    #[test]
    fn a_drop_of_files_is_written_out_in_order() {
        let cache = std::env::temp_dir().join(format!("tileception-drop-test-{}", std::process::id()));
        let (sender, receiver) = mpsc::channel();
        for piece in [
            Piece::Entry { path: "notes".into(), directory: true },
            Piece::Entry { path: "notes/a.txt".into(), directory: false },
            Piece::Bytes(b"hello ".to_vec()),
            Piece::Bytes(b"there".to_vec()),
            Piece::Link("https://example.org/".into()),
            Piece::Entry { path: "b c.txt".into(), directory: false },
        ] {
            sender.send(piece).expect("receiver is there");
        }
        drop(sender);
        let uris = receive_files(&cache, 7, &receiver).expect("written");
        assert_eq!(uris.len(), 3);
        assert_eq!(uris[1], "https://example.org/");
        let notes = file_path(&uris[0]).expect("a local file");
        assert_eq!(std::fs::read_to_string(notes.join("a.txt")).expect("copied"), "hello there");
        assert!(file_path(&uris[2]).expect("a local file").ends_with("b c.txt"));
        let _ = std::fs::remove_dir_all(&cache);
    }
}
