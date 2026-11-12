//! The handshake and the encrypted channel after it.
//!
//! Every machine has a static X25519 key of its own, kept in its state
//! directory. A connection starts with a Noise XX handshake: both sides prove
//! they hold their static key and learn the other's, and the connection gets
//! fresh keys of its own. What happens next depends on whether each side
//! already trusts the other's key (see `trust`):
//!
//! - Both pinned the other: that is enough. The passphrase is not used, so
//!   it may have changed since they paired.
//! - Either one did not: the passphrase decides. A passphrase is short
//!   enough to be guessed, so it never goes into a key directly: SPAKE2 (a
//!   password authenticated key exchange) turns it into a strong shared key,
//!   and someone recording the exchange learns nothing to guess against
//!   offline. Each guess costs a live connection, which the listener
//!   rate-limits. SPAKE2 runs inside the Noise channel and both its identity
//!   and the confirmation that both sides got the same key cover the Noise
//!   handshake hash, so a machine in the middle, which has a different
//!   handshake with each side, still has to know the passphrase. Afterwards
//!   both sides pin each other's key.
//! - Either side forgot the other's key: refused, passphrase or not.
//!
//! What this protects against: strangers on the network (they know neither
//! a trusted key nor the passphrase), someone recording or changing traffic,
//! and machines that were forgotten. What it does not: a machine that knows
//! the passphrase can always pair, even after it was forgotten, by making a
//! new key; keeping it out takes a new passphrase as well. A trusted machine
//! is trusted with everything handoff carries. Anyone who can connect learns
//! the public keys, which are not secret. Someone who copies a machine's
//! private key can pass for it until its peers learn of a new key.

use std::io::{self, Read, Write};

use hmac::{Hmac, Mac};
use sha2::Sha256;
use snow::{HandshakeState, TransportState};
use spake2::{Ed25519Group, Identity, Password, Spake2};
use zeroize::Zeroizing;

use crate::{
    trust::{Key, Keys, Standing},
    wire::{self, Frame},
};

/// The largest Noise message, and so the largest frame on the wire.
pub(crate) const MAX_MESSAGE: usize = 65535;
/// What encryption adds to every message.
pub(crate) const TAG: usize = 16;

pub(crate) const PATTERN: &str = "Noise_XX_25519_ChaChaPoly_BLAKE2s";
const MAGIC: &[u8] = b"tileception-handoff/2";
/// What versions before static keys start with.
const OLD_MAGIC: &[u8] = b"tileception-handoff/1";

#[derive(Debug)]
pub enum Error {
    Socket(io::Error),
    /// The other side is not Tileception or knows a different passphrase.
    /// Deliberately not told apart.
    Refused,
    /// The other side knows the passphrase but runs a version that speaks
    /// another protocol. Only someone who knows the passphrase learns this,
    /// except that versions from before static keys are recognised at once.
    Incompatible,
    /// The other side's key was forgotten here.
    Forgotten,
    /// The passphrase was needed, but too many guesses failed lately.
    Limited,
}

impl std::fmt::Display for Error {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Socket(error) => write!(formatter, "{error}"),
            Error::Refused => formatter.write_str("handshake refused (different passphrase)"),
            Error::Incompatible => formatter.write_str("a different Tileception version"),
            Error::Forgotten => formatter.write_str("refused: this machine was forgotten here"),
            Error::Limited => formatter.write_str("refused: too many wrong passphrases lately"),
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

/// A finished handshake.
pub(crate) struct Secured {
    pub(crate) transport: TransportState,
    /// The other machine's static key.
    pub(crate) key: Key,
    /// Whether the passphrase let it in, rather than its key.
    pub(crate) paired: bool,
}

/// What the handshake needs to know about this machine.
pub(crate) struct Credentials<'cred, Judge: Fn(&Key) -> Standing> {
    pub(crate) keys: &'cred Keys,
    pub(crate) passphrase: &'cred str,
    /// Whether this machine trusts a key.
    pub(crate) standing: Judge,
    /// False when the passphrase may not be tried now (too many failures).
    pub(crate) may_pair: bool,
}

/// Runs the whole handshake. The machine that dialled is the initiator.
pub(crate) fn handshake(
    stream: &mut (impl Read + Write),
    credentials: &Credentials<'_, impl Fn(&Key) -> Standing>,
    initiator: bool,
) -> Result<Secured, Error> {
    let builder = snow::Builder::new(PATTERN.parse()?).prologue(MAGIC)?.local_private_key(&credentials.keys.private)?;
    let mut noise = if initiator { builder.build_initiator()? } else { builder.build_responder()? };
    if initiator {
        send_handshake(stream, &mut noise, MAGIC)?;
        receive_handshake(stream, &mut noise, &[])?;
        send_handshake(stream, &mut noise, &[])?;
    } else {
        receive_handshake(stream, &mut noise, MAGIC)?;
        send_handshake(stream, &mut noise, &[])?;
        receive_handshake(stream, &mut noise, &[])?;
    }
    let key: Key = noise.get_remote_static().and_then(|key| key.try_into().ok()).ok_or(Error::Refused)?;
    let hash = noise.get_handshake_hash().to_vec();
    let mut transport = noise.into_transport_mode()?;

    let known = match (credentials.standing)(&key) {
        Standing::Refused => return Err(Error::Forgotten),
        Standing::Known => true,
        Standing::Unknown => false,
    };
    send(stream, &mut transport, &Frame::Trust { known })?;
    let Frame::Trust { known: theirs } = receive(stream, &mut transport)? else { return Err(Error::Refused) };
    if known && theirs {
        return Ok(Secured { transport, key, paired: false });
    }
    if !credentials.may_pair {
        return Err(Error::Limited);
    }
    pair(stream, &mut transport, credentials.passphrase, &hash, initiator)?;
    Ok(Secured { transport, key, paired: true })
}

/// Proves both sides know the same passphrase, bound to this handshake.
fn pair(
    stream: &mut (impl Read + Write),
    transport: &mut TransportState,
    passphrase: &str,
    hash: &[u8],
    initiator: bool,
) -> Result<(), Error> {
    let mut identity = MAGIC.to_vec();
    identity.extend_from_slice(hash);
    let (spake, ours) =
        Spake2::<Ed25519Group>::start_symmetric(&Password::new(passphrase.as_bytes()), &Identity::new(&identity));
    send(stream, transport, &Frame::Pake(ours))?;
    let Frame::Pake(theirs) = receive(stream, transport)? else { return Err(Error::Refused) };
    let key = Zeroizing::new(spake.finish(&theirs).map_err(|_| Error::Refused)?);
    send(stream, transport, &Frame::Confirm(confirmation(&key, hash, initiator)?.finalize().into_bytes().to_vec()))?;
    let Frame::Confirm(theirs) = receive(stream, transport)? else { return Err(Error::Refused) };
    confirmation(&key, hash, !initiator)?.verify_slice(&theirs).map_err(|_| Error::Refused)
}

/// What a side that got `key` says to show it, by its role.
fn confirmation(key: &[u8], hash: &[u8], initiator: bool) -> Result<Hmac<Sha256>, Error> {
    let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(key).map_err(|_| Error::Refused)?;
    mac.update(MAGIC);
    mac.update(if initiator { b"initiator" } else { b"responder" });
    mac.update(hash);
    Ok(mac)
}

fn send(stream: &mut impl Write, transport: &mut TransportState, frame: &Frame) -> Result<(), Error> {
    write_frame(stream, &seal(transport, frame)?)?;
    Ok(())
}

fn receive(stream: &mut impl Read, transport: &mut TransportState) -> Result<Frame, Error> {
    open(transport, &read_frame(stream)?).map_err(|_| Error::Refused)
}

fn send_handshake(stream: &mut impl Write, noise: &mut HandshakeState, prefix: &[u8]) -> Result<(), Error> {
    let mut message = vec![0; MAX_MESSAGE];
    let length = noise.write_message(&[], &mut message)?;
    let mut frame = prefix.to_vec();
    frame.extend_from_slice(&message[..length]);
    write_frame(stream, &frame)?;
    Ok(())
}

fn receive_handshake(stream: &mut impl Read, noise: &mut HandshakeState, prefix: &[u8]) -> Result<(), Error> {
    let message = read_frame(stream)?;
    if !prefix.is_empty() && message.starts_with(OLD_MAGIC) {
        return Err(Error::Incompatible);
    }
    let message = message.strip_prefix(prefix).ok_or(Error::Refused)?;
    let mut payload = vec![0; MAX_MESSAGE];
    noise.read_message(message, &mut payload)?;
    Ok(())
}

pub(crate) fn seal(transport: &mut TransportState, frame: &Frame) -> io::Result<Vec<u8>> {
    let plain = wire::encode(frame).map_err(io::Error::other)?;
    seal_bytes(transport, &plain)
}

/// Seals a frame already encoded before entering the bounded outbox.
pub(crate) fn seal_bytes(transport: &mut TransportState, plain: &[u8]) -> io::Result<Vec<u8>> {
    if plain.len() + TAG > MAX_MESSAGE {
        return Err(io::Error::other("message too large"));
    }
    let mut sealed = vec![0; plain.len() + TAG];
    let length = transport.write_message(plain, &mut sealed).map_err(io::Error::other)?;
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

    /// One side of a handshake: its keys, its passphrase and the keys it
    /// trusts and refuses.
    struct Side {
        keys: Keys,
        passphrase: &'static str,
        known: Vec<Key>,
        refused: Vec<Key>,
    }

    impl Side {
        fn new(passphrase: &'static str) -> Side {
            Side { keys: Keys::generate().expect("keys"), passphrase, known: Vec::new(), refused: Vec::new() }
        }

        fn shake(&self, stream: &mut UnixStream, initiator: bool) -> Result<Secured, Error> {
            let credentials = Credentials {
                keys: &self.keys,
                passphrase: self.passphrase,
                standing: |key: &Key| {
                    if self.refused.contains(key) {
                        Standing::Refused
                    } else if self.known.contains(key) {
                        Standing::Known
                    } else {
                        Standing::Unknown
                    }
                },
                may_pair: true,
            };
            handshake(stream, &credentials, initiator)
        }
    }

    fn connect(dialler: &Side, listener: Side) -> (Result<Secured, Error>, Result<Secured, Error>) {
        let (mut left, mut right) = UnixStream::pair().expect("socket pair");
        let other = std::thread::spawn(move || {
            let result = listener.shake(&mut right, false);
            drop(right);
            result
        });
        let ours = dialler.shake(&mut left, true);
        drop(left);
        (ours, other.join().expect("listener thread"))
    }

    #[test]
    fn same_passphrase_pairs_and_talks() {
        let (dialler, listener) = (Side::new("correct horse"), Side::new("correct horse"));
        let (dialler_key, listener_key) = (dialler.keys.public, listener.keys.public);
        let (ours, theirs) = connect(&dialler, listener);
        let (mut ours, mut theirs) = (ours.expect("dialler"), theirs.expect("listener"));
        assert!(ours.paired && theirs.paired);
        assert_eq!((ours.key, theirs.key), (listener_key, dialler_key));
        let frame = Frame::App(Message::Key { key: 30, pressed: true });
        let sealed = seal(&mut ours.transport, &frame).expect("seals");
        assert_eq!(open(&mut theirs.transport, &sealed).expect("opens"), frame);
        let back = Frame::Ping;
        let sealed = seal(&mut theirs.transport, &back).expect("seals");
        assert_eq!(open(&mut ours.transport, &sealed).expect("opens"), back);
    }

    #[test]
    fn different_passphrases_are_refused() {
        let (ours, theirs) = connect(&Side::new("correct horse"), Side::new("battery staple"));
        assert!(matches!(ours, Err(Error::Refused)));
        assert!(matches!(theirs, Err(Error::Refused)));
    }

    #[test]
    fn trusted_keys_need_no_passphrase() {
        let mut dialler = Side::new("old words");
        let mut listener = Side::new("new words");
        dialler.known.push(listener.keys.public);
        listener.known.push(dialler.keys.public);
        let (ours, theirs) = connect(&dialler, listener);
        assert!(!ours.expect("dialler").paired);
        assert!(!theirs.expect("listener").paired);
    }

    #[test]
    fn trust_on_one_side_only_needs_the_passphrase() {
        let mut dialler = Side::new("old words");
        let listener = Side::new("new words");
        dialler.known.push(listener.keys.public);
        let (ours, theirs) = connect(&dialler, listener);
        assert!(matches!(ours, Err(Error::Refused)));
        assert!(matches!(theirs, Err(Error::Refused)));

        let mut dialler = Side::new("same");
        let listener = Side::new("same");
        dialler.known.push(listener.keys.public);
        let (ours, theirs) = connect(&dialler, listener);
        assert!(ours.expect("dialler").paired && theirs.expect("listener").paired);
    }

    #[test]
    fn forgotten_keys_are_refused_with_the_passphrase() {
        let dialler = Side::new("same");
        let mut listener = Side::new("same");
        listener.refused.push(dialler.keys.public);
        let (ours, theirs) = connect(&dialler, listener);
        assert!(ours.is_err());
        assert!(matches!(theirs, Err(Error::Forgotten)));
    }

    #[test]
    fn no_guessing_while_limited() {
        let (mut left, mut right) = UnixStream::pair().expect("socket pair");
        let listener = Side::new("same");
        let other = std::thread::spawn(move || {
            let credentials = Credentials {
                keys: &listener.keys,
                passphrase: listener.passphrase,
                standing: |_: &Key| Standing::Unknown,
                may_pair: false,
            };
            let result = handshake(&mut right, &credentials, false);
            drop(right);
            result
        });
        let ours = Side::new("same").shake(&mut left, true);
        drop(left);
        assert!(ours.is_err());
        assert!(matches!(other.join().expect("thread"), Err(Error::Limited)));
    }

    #[test]
    fn tampering_is_detected() {
        let (ours, theirs) = connect(&Side::new("same"), Side::new("same"));
        let (mut ours, mut theirs) = (ours.expect("dialler"), theirs.expect("listener"));
        let mut sealed = seal(&mut ours.transport, &Frame::Ping).expect("seals");
        sealed[0] ^= 1;
        assert!(open(&mut theirs.transport, &sealed).is_err());
    }

    #[test]
    fn something_else_is_refused() {
        let (mut left, mut right) = UnixStream::pair().expect("socket pair");
        let other = std::thread::spawn(move || Side::new("same").shake(&mut right, false));
        write_frame(&mut left, b"GET / HTTP/1.1").expect("writes");
        assert!(matches!(other.join().expect("thread"), Err(Error::Refused)));
        drop(left);
    }

    #[test]
    fn older_versions_are_recognised() {
        let (mut left, mut right) = UnixStream::pair().expect("socket pair");
        let other = std::thread::spawn(move || Side::new("same").shake(&mut right, false));
        let mut hello = OLD_MAGIC.to_vec();
        hello.extend_from_slice(&[0; 33]);
        write_frame(&mut left, &hello).expect("writes");
        assert!(matches!(other.join().expect("thread"), Err(Error::Incompatible)));
        drop(left);
    }
}
