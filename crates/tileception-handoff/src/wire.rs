//! What machines say to each other once a connection is secured.

use serde::{Deserialize, Serialize};

use crate::picture::Area;

/// Bumped whenever a message changes shape; machines on different versions
/// refuse each other instead of misreading.
pub const PROTOCOL: u32 = 5;

/// Clipboard contents travel in pieces of at most this many bytes.
pub const CHUNK: usize = 32 * 1024;

/// The most compressed tile bytes one picture message carries.
pub const PICTURE_BUDGET: usize = 60 * 1024;

/// The most popups a picture lists; any more are drawn but not told apart.
pub const MAX_POPUPS: usize = 16;

/// Which selection a clipboard message is about.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Target {
    /// Copy and paste.
    Clipboard,
    /// Select and middle-click.
    Primary,
}

/// One scroll step, as the pointer reported it.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Default)]
pub struct Scroll {
    /// From a touchpad (true) rather than a wheel or knob.
    pub finger: bool,
    pub horizontal: Option<f64>,
    pub vertical: Option<f64>,
    pub horizontal_v120: Option<f64>,
    pub vertical_v120: Option<f64>,
}

/// A rectangle of a window's picture, its RGBA rows compressed with LZ4
/// (the uncompressed size first).
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct Tile {
    pub left: u32,
    pub top: u32,
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

/// Part of a new picture of a window shown on another machine: only what
/// changed since the last one, spread over messages that fit the wire.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct Picture {
    pub window: u64,
    pub serial: u64,
    /// The whole picture, in pixels.
    pub width: u32,
    pub height: u32,
    /// Pixels per logical pixel on the owner.
    pub scale: f64,
    /// Where the window itself is in the picture. The rest is its menus
    /// and other popups reaching past it, each listed in `popups`.
    pub body: Area,
    pub popups: Vec<Area>,
    pub tiles: Vec<Tile>,
    /// The picture is complete with this part.
    pub last: bool,
}

/// Everything the compositor sends and receives. Pointer positions crossing
/// an edge are fractions along that edge, so screens need not match in size.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub enum Message {
    /// The sender placed this machine at one of its edges.
    Arranged,
    /// The sender dismissed the prompt or ended the arrangement.
    Dismissed,
    /// The sender's input now goes here, entering at this point of the edge.
    Enter {
        along: f64,
    },
    /// The pointer left here towards the sender: its input is its own again.
    Return {
        along: f64,
    },
    /// The sender stopped sending input (its return-home shortcut, a lock):
    /// release whatever it still holds.
    Leave,
    Motion {
        right: f64,
        down: f64,
    },
    Button {
        button: u32,
        pressed: bool,
    },
    Scroll(Scroll),
    /// A key by its evdev code; the receiver's keymap decides what it types.
    Key {
        key: u32,
        pressed: bool,
    },
    /// What the sender offers to paste; no types means nothing.
    Selection {
        target: Target,
        version: u64,
        mime_types: Vec<String>,
    },
    /// Asks for the contents of an offered selection.
    Fetch {
        request: u64,
        target: Target,
        version: u64,
        mime_type: String,
    },
    Data {
        request: u64,
        bytes: Vec<u8>,
    },
    /// The last piece of a fetch, or its failure.
    DataEnd {
        request: u64,
        failed: bool,
    },
    /// The sender locked: sharing pauses until someone resumes it.
    Paused,
    /// Sharing goes on again after a pause.
    Resumed,
    /// One of the sender's windows comes here, dragged by the pointer that
    /// follows: held at `grab` (fractions of its size) with `button` down.
    /// Windows are known by the number their owner gives them.
    WindowShown {
        window: u64,
        title: String,
        app_id: String,
        width: u32,
        height: u32,
        grab: (f64, f64),
        button: u32,
    },
    /// The window's title or app changed.
    WindowInfo {
        window: u64,
        title: String,
        app_id: String,
    },
    WindowPicture(Picture),
    /// The window closed, or its owner took it back.
    WindowGone {
        window: u64,
    },
    /// The window's program asked to be moved (its title bar was dragged):
    /// the pointer holding it there drags it.
    WindowMove {
        window: u64,
    },
    /// A picture arrived; the owner may send the next.
    WindowAck {
        window: u64,
        serial: u64,
    },
    /// The size the window's place has, in logical pixels, and the screen
    /// around it, which its menus should stay on.
    WindowSize {
        window: u64,
        width: u32,
        height: u32,
        room: Room,
    },
    /// What the pointer looks like over the window, as its program set it.
    WindowCursor {
        window: u64,
        image: PointerImage,
    },
    /// The pointer over the window, in logical pixels from its top left.
    WindowPointer {
        window: u64,
        across: f64,
        down: f64,
    },
    WindowButton {
        window: u64,
        button: u32,
        pressed: bool,
    },
    WindowScroll {
        window: u64,
        scroll: Scroll,
    },
    /// The pointer left the window.
    WindowLeave {
        window: u64,
    },
    /// Whether the window has the keyboard where it is shown.
    WindowFocus {
        window: u64,
        focused: bool,
    },
    WindowKey {
        window: u64,
        key: u32,
        pressed: bool,
    },
    /// Someone closed the window where it is shown.
    WindowClose {
        window: u64,
    },
    /// The window is dragged back to its owner, held as in `WindowShown`.
    /// Without a button, it simply goes home.
    WindowBack {
        window: u64,
        grab: (f64, f64),
        button: u32,
    },
    /// Sent whenever the machines link: the sender's windows that were
    /// shown here when the connection was lost and are still meant to be.
    /// They pick up where they were; the sender's other windows still
    /// waiting here are gone.
    WindowsResumed {
        windows: Vec<Resumed>,
    },
    /// A program on the sender is dragging something (drag and drop), and
    /// the drag comes along with the pointer: sent just before `Enter` or
    /// `Return`. The sender's `button` holds it.
    DragStart {
        drag: u64,
        mime_types: Vec<String>,
        button: u32,
        preview: DragPreview,
    },
    /// It was dropped on a program here, which wants it in this type:
    /// answered with `Data` and `DataEnd`, as a paste is.
    DragFetch {
        request: u64,
        drag: u64,
        mime_type: String,
    },
    /// It was dropped as files (`text/uri-list`), which have to be copied
    /// over first: answered with a `DragFile` or `DragLink` per entry, each
    /// file's contents in `Data` after its `DragFile`, then `DataEnd`.
    DragFiles {
        request: u64,
        drag: u64,
    },
    /// A file (its `Data` follows) or folder, by its path within what was
    /// dragged, parts separated by `/`.
    DragFile {
        request: u64,
        path: String,
        directory: bool,
    },
    /// An entry that is not a local file, such as a web address, as it is.
    DragLink {
        request: u64,
        uri: String,
    },
    /// The drag is over where it went (dropped or not), or its program on
    /// the sender gave it up.
    DragEnd {
        drag: u64,
    },
}

/// What a drag carries, for showing by the pointer.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
pub struct DragPreview {
    /// The first item's name, or the start of the text.
    pub label: String,
    /// How many items: files, or 1.
    pub count: u32,
    /// What kind of thing it is.
    pub kind: DragKind,
    /// A small picture of the first item, packed with `pack_rgba`.
    pub thumbnail: Option<Thumbnail>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DragKind {
    Files,
    Folder,
    Text,
    Image,
    Link,
    #[default]
    Other,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct Thumbnail {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

/// A screen's rectangle in logical pixels from a window's top left.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Room {
    pub left: i32,
    pub top: i32,
    pub width: i32,
    pub height: i32,
}

/// What the pointer looks like.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub enum PointerImage {
    /// The usual arrow.
    Default,
    Hidden,
    /// A picture the program drew, RGBA rows compressed with LZ4, with the
    /// point that is the pointer's position, in its pixels.
    Picture {
        width: u32,
        height: u32,
        hotspot: (u32, u32),
        /// Pixels per logical pixel on the owner.
        scale: f64,
        pixels: Vec<u8>,
    },
}

/// A window shown on another machine before a connection was lost.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct Resumed {
    pub window: u64,
    pub title: String,
    pub app_id: String,
}

/// Messages of the connection itself, around the compositor's.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub(crate) enum Frame {
    Hello { protocol: u32, machine: u64, name: String },
    Ping,
    App(Message),
}

pub(crate) fn encode(frame: &Frame) -> Result<Vec<u8>, postcard::Error> {
    postcard::to_stdvec(frame)
}

pub(crate) fn decode(bytes: &[u8]) -> Result<Frame, postcard::Error> {
    postcard::from_bytes(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips() {
        let frames = [
            Frame::Hello { protocol: PROTOCOL, machine: 7, name: "desk".into() },
            Frame::Ping,
            Frame::App(Message::Motion { right: -1.5, down: 2.25 }),
            Frame::App(Message::Selection {
                target: Target::Primary,
                version: 3,
                mime_types: vec!["text/plain;charset=utf-8".into()],
            }),
            Frame::App(Message::Data { request: 1, bytes: vec![0; CHUNK] }),
        ];
        for frame in frames {
            let bytes = encode(&frame).expect("encodes");
            assert_eq!(decode(&bytes).expect("decodes"), frame);
        }
    }

    #[test]
    fn a_full_picture_part_fits_one_noise_message() {
        let tile = |pixels| Tile { left: u32::MAX, top: u32::MAX, width: u32::MAX, height: u32::MAX, pixels };
        let picture = Picture {
            window: u64::MAX,
            serial: u64::MAX,
            width: u32::MAX,
            height: u32::MAX,
            scale: 1.5,
            body: Area { left: u32::MAX, top: u32::MAX, width: u32::MAX, height: u32::MAX },
            popups: vec![Area { left: u32::MAX, top: u32::MAX, width: u32::MAX, height: u32::MAX }; MAX_POPUPS],
            tiles: vec![tile(vec![0xff; PICTURE_BUDGET / 2]), tile(vec![0xff; PICTURE_BUDGET / 2])],
            last: true,
        };
        let bytes = encode(&Frame::App(Message::WindowPicture(picture))).expect("encodes");
        assert!(bytes.len() + crate::secure::TAG <= crate::secure::MAX_MESSAGE);
    }

    #[test]
    fn a_full_chunk_fits_one_noise_message() {
        let frame = Frame::App(Message::Data { request: u64::MAX, bytes: vec![0xff; CHUNK] });
        let bytes = encode(&frame).expect("encodes");
        assert!(bytes.len() + crate::secure::TAG <= crate::secure::MAX_MESSAGE);
    }
}
