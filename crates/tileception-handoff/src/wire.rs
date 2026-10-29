//! What machines say to each other once a connection is secured.

use serde::{Deserialize, Serialize};

/// Bumped whenever a message changes shape; machines on different versions
/// refuse each other instead of misreading.
pub const PROTOCOL: u32 = 1;

/// Clipboard contents travel in pieces of at most this many bytes.
pub const CHUNK: usize = 32 * 1024;

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
    fn a_full_chunk_fits_one_noise_message() {
        let frame = Frame::App(Message::Data { request: u64::MAX, bytes: vec![0xff; CHUNK] });
        let bytes = encode(&frame).expect("encodes");
        assert!(bytes.len() + crate::secure::TAG <= crate::secure::MAX_MESSAGE);
    }
}
