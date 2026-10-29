//! The handshake and the encrypted channel after it.
//!
//! Both machines know the same passphrase. A passphrase is short enough to be
//! guessed, so it never goes into a key directly: SPAKE2 (a password
//! authenticated key exchange) turns it into a strong shared key, and someone
//! recording the exchange learns nothing to guess against offline. Each guess
//! costs a live connection, which the listener rate-limits. That key is the
//! pre-shared key of a Noise NNpsk0 handshake, which confirms that both sides
//! got the same key and gives the connection fresh keys of its own. Both
//! SPAKE2 messages go into the Noise prologue, binding the two steps.

use std::io::{self, Read, Write};

use snow::{HandshakeState, TransportState};
use spake2::{Ed25519Group, Identity, Password, Spake2};
use zeroize::Zeroizing;

use crate::wire::{self, Frame};

/// The largest Noise message, and so the largest frame on the wire.
pub(crate) const MAX_MESSAGE: usize = 65535;
/// What encryption adds to every message.
pub(crate) const TAG: usize = 16;

const PATTERN: &str = "Noise_NNpsk0_25519_ChaChaPoly_BLAKE2s";
const MAGIC: &[u8] = b"tileception-handoff/1";

#[derive(Debug)]
pub enum Error {
    Socket(io::Error),
    /// The other side is not Tileception, speaks another version, or knows a
    /// different passphrase. Deliberately not told apart.
    Refused,
}

impl std::fmt::Display for Error {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Socket(error) => write!(formatter, "{error}"),
            Error::Refused => formatter.write_str("handshake refused (different passphrase or version)"),
        }
    }
}

impl From<io::Error> for Error {
    fn from(error: io::Error) -> Self {
        if error.kind() == io::ErrorKind::UnexpectedEof { Error::Refused } else { Error::Socket(error) }
    }
}

impl From<snow::Error> for Error {
    fn from(_: snow::Error) -> Self {
        Error::Refused
    }
}

pub(crate) fn write_frame(stream: &mut impl Write, bytes: &[u8]) -> io::Result<()> {
    let length = u16::try_from(bytes.len()).map_err(|_| io::Error::other("frame too large"))?;
    let mut frame = Vec::with_capacity(bytes.len() + 2);
    frame.extend_from_slice(&length.to_be_bytes());
    frame.extend_from_slice(bytes);
    stream.write_all(&frame)?;
    stream.flush()
}

pub(crate) fn read_frame(stream: &mut impl Read) -> io::Result<Vec<u8>> {
    let mut length = [0; 2];
    stream.read_exact(&mut length)?;
    let mut bytes = vec![0; usize::from(u16::from_be_bytes(length))];
    stream.read_exact(&mut bytes)?;
    Ok(bytes)
}

/// Runs the whole handshake. The machine that dialled is the initiator.
pub(crate) fn handshake(
    stream: &mut (impl Read + Write),
    passphrase: &str,
    initiator: bool,
) -> Result<TransportState, Error> {
    let (spake, ours) =
        Spake2::<Ed25519Group>::start_symmetric(&Password::new(passphrase.as_bytes()), &Identity::new(MAGIC));
    let mut hello = MAGIC.to_vec();
    hello.extend_from_slice(&ours);
    write_frame(stream, &hello)?;
    let theirs = read_frame(stream)?;
    let Some(theirs) = theirs.strip_prefix(MAGIC) else { return Err(Error::Refused) };
    let key = Zeroizing::new(spake.finish(theirs).map_err(|_| Error::Refused)?);
    let psk: &[u8; 32] = key.as_slice().try_into().map_err(|_| Error::Refused)?;

    let mut prologue = MAGIC.to_vec();
    let (first, second) = if initiator { (&ours[..], theirs) } else { (theirs, &ours[..]) };
    prologue.extend_from_slice(first);
    prologue.extend_from_slice(second);
    let builder = snow::Builder::new(PATTERN.parse()?).prologue(&prologue)?.psk(0, psk)?;
    let mut noise = if initiator { builder.build_initiator()? } else { builder.build_responder()? };
    if initiator {
        send_handshake(stream, &mut noise)?;
        receive_handshake(stream, &mut noise)?;
    } else {
        receive_handshake(stream, &mut noise)?;
        send_handshake(stream, &mut noise)?;
    }
    Ok(noise.into_transport_mode()?)
}

fn send_handshake(stream: &mut impl Write, noise: &mut HandshakeState) -> Result<(), Error> {
    let mut message = vec![0; MAX_MESSAGE];
    let length = noise.write_message(&[], &mut message)?;
    write_frame(stream, &message[..length])?;
    Ok(())
}

fn receive_handshake(stream: &mut impl Read, noise: &mut HandshakeState) -> Result<(), Error> {
    let message = read_frame(stream)?;
    let mut payload = vec![0; MAX_MESSAGE];
    noise.read_message(&message, &mut payload)?;
    Ok(())
}

pub(crate) fn seal(transport: &mut TransportState, frame: &Frame) -> io::Result<Vec<u8>> {
    let plain = wire::encode(frame).map_err(io::Error::other)?;
    if plain.len() + TAG > MAX_MESSAGE {
        return Err(io::Error::other("message too large"));
    }
    let mut sealed = vec![0; plain.len() + TAG];
    let length = transport.write_message(&plain, &mut sealed).map_err(io::Error::other)?;
    sealed.truncate(length);
    Ok(sealed)
}

pub(crate) fn open(transport: &mut TransportState, sealed: &[u8]) -> io::Result<Frame> {
    let mut plain = vec![0; sealed.len()];
    let length = transport
        .read_message(sealed, &mut plain)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "message failed authentication"))?;
    wire::decode(&plain[..length]).map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

#[cfg(test)]
mod tests {
    use std::os::unix::net::UnixStream;

    use super::*;
    use crate::wire::Message;

    fn pair(
        dialler: &'static str,
        listener: &'static str,
    ) -> (Result<TransportState, Error>, Result<TransportState, Error>) {
        let (mut left, mut right) = UnixStream::pair().expect("socket pair");
        let other = std::thread::spawn(move || {
            let result = handshake(&mut right, listener, false);
            drop(right);
            result
        });
        let ours = handshake(&mut left, dialler, true);
        drop(left);
        (ours, other.join().expect("listener thread"))
    }

    #[test]
    fn same_passphrase_talks() {
        let (ours, theirs) = pair("correct horse", "correct horse");
        let (mut ours, mut theirs) = (ours.expect("dialler"), theirs.expect("listener"));
        let frame = Frame::App(Message::Key { key: 30, pressed: true });
        let sealed = seal(&mut ours, &frame).expect("seals");
        assert_eq!(open(&mut theirs, &sealed).expect("opens"), frame);
        let back = Frame::Ping;
        let sealed = seal(&mut theirs, &back).expect("seals");
        assert_eq!(open(&mut ours, &sealed).expect("opens"), back);
    }

    #[test]
    fn different_passphrases_are_refused() {
        let (ours, theirs) = pair("correct horse", "battery staple");
        assert!(matches!(ours, Err(Error::Refused)));
        assert!(matches!(theirs, Err(Error::Refused)));
    }

    #[test]
    fn tampering_is_detected() {
        let (ours, theirs) = pair("same", "same");
        let (mut ours, mut theirs) = (ours.expect("dialler"), theirs.expect("listener"));
        let mut sealed = seal(&mut ours, &Frame::Ping).expect("seals");
        sealed[0] ^= 1;
        assert!(open(&mut theirs, &sealed).is_err());
    }

    #[test]
    fn something_else_is_refused() {
        let (mut left, mut right) = UnixStream::pair().expect("socket pair");
        let other = std::thread::spawn(move || handshake(&mut right, "same", false));
        write_frame(&mut left, b"GET / HTTP/1.1").expect("writes");
        assert!(matches!(other.join().expect("thread"), Err(Error::Refused)));
        drop(left);
    }
}
