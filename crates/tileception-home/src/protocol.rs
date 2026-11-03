//! The Tuya LAN protocol, versions 3.3 and 3.5: framing, encryption and the
//! 3.5 session key handshake, for both ends of a connection (the simulated
//! device in [`crate::sim`] uses the device's end).
//!
//! 3.3 frames start with `0x000055AA`, carry an AES-128-ECB payload under the
//! device's local key and end in a CRC32. 3.5 frames start with `0x00006699`
//! and are AES-128-GCM under a session key that both ends derive from two
//! nonces at the start of every connection.

use aes::{
    Aes128,
    cipher::{BlockDecrypt, BlockEncrypt, KeyInit, generic_array::GenericArray},
};
use aes_gcm::{AeadInPlace, Aes128Gcm, Nonce, Tag};
use hmac::{Hmac, Mac};
use sha2::Sha256;

/// The commands this crate sends or answers.
pub mod command {
    pub const SESSION_START: u32 = 0x03;
    pub const SESSION_REPLY: u32 = 0x04;
    pub const SESSION_FINISH: u32 = 0x05;
    pub const CONTROL: u32 = 0x07;
    pub const STATUS: u32 = 0x08;
    pub const HEARTBEAT: u32 = 0x09;
    pub const QUERY: u32 = 0x0a;
    /// Control on 3.4 and newer, and the status query of "device22" devices.
    pub const CONTROL_NEW: u32 = 0x0d;
    /// The status query on 3.4 and newer.
    pub const QUERY_NEW: u32 = 0x10;
}

const PREFIX_55AA: u32 = 0x0000_55AA;
const SUFFIX_55AA: u32 = 0x0000_AA55;
const PREFIX_6699: u32 = 0x0000_6699;
const SUFFIX_6699: u32 = 0x0000_9966;
/// Frames claiming to be longer than this are a desynchronised stream.
const MAX_LEN: usize = 64 * 1024;

/// The key Tuya devices encrypt their UDP announcements with:
/// MD5 of `yGAdlopoPVldABfn`.
pub const UDP_KEY: [u8; 16] = [108, 30, 200, 226, 187, 155, 181, 154, 181, 11, 13, 175, 100, 155, 65, 10];

/// The protocol versions spoken.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Version {
    V33,
    V35,
}

impl Version {
    pub fn parse(text: &str) -> Option<Version> {
        match text.trim() {
            "3.3" => Some(Version::V33),
            "3.5" => Some(Version::V35),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Version::V33 => "3.3",
            Version::V35 => "3.5",
        }
    }

    /// The version and twelve zero bytes that precede some payloads.
    fn header(self) -> [u8; 15] {
        let mut header = [0; 15];
        header[..3].copy_from_slice(self.as_str().as_bytes());
        header
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// Not a Tuya frame, or one that does not add up.
    Malformed(&'static str),
    /// The checksum or the authentication tag is wrong: usually a wrong key.
    Integrity,
    /// The handshake reply did not prove the device knows the key.
    Handshake,
}

impl std::fmt::Display for Error {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Malformed(why) => write!(formatter, "malformed frame: {why}"),
            Error::Integrity => formatter.write_str("checksum mismatch (wrong local key?)"),
            Error::Handshake => formatter.write_str("session handshake failed (wrong local key?)"),
        }
    }
}

impl std::error::Error for Error {}

/// One decoded message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub seq: u32,
    pub command: u32,
    /// Messages from a device carry a return code; zero is success.
    pub retcode: Option<u32>,
    /// The decrypted payload without any version header: usually JSON, or
    /// empty for an acknowledgement.
    pub payload: Vec<u8>,
}

/// Which end of the connection a codec is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Client,
    Device,
}

/// Encodes and decodes the frames of one connection.
pub struct Codec {
    version: Version,
    side: Side,
    local_key: [u8; 16],
    /// The local key, or on 3.5 the session key once negotiated.
    key: [u8; 16],
    seq: u32,
}

impl Drop for Codec {
    fn drop(&mut self) {
        use zeroize::Zeroize;
        self.local_key.zeroize();
        self.key.zeroize();
    }
}

impl Codec {
    pub fn new(version: Version, side: Side, local_key: [u8; 16]) -> Codec {
        Codec { version, side, local_key, key: local_key, seq: 1 }
    }

    pub fn version(&self) -> Version {
        self.version
    }

    /// Switches to a negotiated session key.
    pub fn set_session_key(&mut self, key: [u8; 16]) {
        self.key = key;
    }

    /// A message for the other end.
    pub fn encode(&mut self, command: u32, payload: &[u8]) -> Vec<u8> {
        let seq = self.seq;
        self.seq = self.seq.wrapping_add(1);
        self.encode_seq(seq, command, payload)
    }

    /// A reply, which carries the sequence number it answers.
    pub fn encode_seq(&self, seq: u32, command: u32, payload: &[u8]) -> Vec<u8> {
        let retcode = (self.side == Side::Device).then_some(0u32);
        let header = needs_version_header(command, self.version).then(|| self.version.header());
        match self.version {
            Version::V33 => {
                let mut body = Vec::new();
                if let Some(code) = retcode {
                    body.extend_from_slice(&code.to_be_bytes());
                }
                if let Some(header) = header {
                    body.extend_from_slice(&header);
                }
                if !payload.is_empty() {
                    body.extend_from_slice(&ecb_encrypt(&self.key, payload));
                }
                frame_55aa(seq, command, &body)
            }
            Version::V35 => {
                let mut plain = Vec::new();
                if let Some(code) = retcode {
                    plain.extend_from_slice(&code.to_be_bytes());
                }
                if let Some(header) = header {
                    plain.extend_from_slice(&header);
                }
                plain.extend_from_slice(payload);
                frame_6699(&self.key, seq, command, plain)
            }
        }
    }

    /// Decodes the frame at the start of `buffer`: `Ok(None)` while it is
    /// still incomplete, otherwise the frame and how many bytes it took.
    pub fn decode(&self, buffer: &[u8]) -> Result<Option<(Frame, usize)>, Error> {
        decode_frame(buffer, &self.key, self.side, self.version)
    }

    /// The client's first handshake message on 3.5, and the nonce to keep.
    pub fn session_start(&mut self) -> ([u8; 16], Vec<u8>) {
        let mut nonce = [0; 16];
        random(&mut nonce);
        let message = self.encode(command::SESSION_START, &nonce);
        (nonce, message)
    }

    /// The client's answer to the device's handshake reply: checks the
    /// device knows the key and returns the finishing message and the
    /// session key, which takes effect after that message is sent.
    pub fn session_finish(&mut self, local_nonce: &[u8; 16], reply: &[u8]) -> Result<(Vec<u8>, [u8; 16]), Error> {
        if reply.len() < 48 {
            return Err(Error::Handshake);
        }
        let mut remote_nonce = [0; 16];
        remote_nonce.copy_from_slice(&reply[..16]);
        if !hmac_verify(&self.local_key, local_nonce, &reply[16..48]) {
            return Err(Error::Handshake);
        }
        let message = self.encode(command::SESSION_FINISH, &hmac(&self.local_key, &remote_nonce));
        Ok((message, session_key(&self.local_key, local_nonce, &remote_nonce)))
    }

    /// The device's reply to a client's handshake start, and its own nonce.
    pub fn session_reply(&self, seq: u32, local_nonce: &[u8]) -> ([u8; 16], Vec<u8>) {
        let mut remote_nonce = [0; 16];
        random(&mut remote_nonce);
        let mut payload = remote_nonce.to_vec();
        payload.extend_from_slice(&hmac(&self.local_key, local_nonce));
        (remote_nonce, self.encode_seq(seq, command::SESSION_REPLY, &payload))
    }

    /// The device checks the client's finishing message; the session key.
    pub fn session_accept(
        &self,
        local_nonce: &[u8; 16],
        remote_nonce: &[u8; 16],
        finish: &[u8],
    ) -> Result<[u8; 16], Error> {
        if !hmac_verify(&self.local_key, remote_nonce, finish) {
            return Err(Error::Handshake);
        }
        Ok(session_key(&self.local_key, local_nonce, remote_nonce))
    }
}

/// Whether a command's payload starts with the version and twelve zeros.
fn needs_version_header(command: u32, version: Version) -> bool {
    use command::*;
    let bare = [QUERY, QUERY_NEW, HEARTBEAT, SESSION_START, SESSION_REPLY, SESSION_FINISH];
    if bare.contains(&command) {
        return false;
    }
    // Status pushes from a 3.3 device have it; those from 3.5 devices too.
    let _ = version;
    true
}

fn frame_55aa(seq: u32, command: u32, body: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(body.len() + 24);
    out.extend_from_slice(&PREFIX_55AA.to_be_bytes());
    out.extend_from_slice(&seq.to_be_bytes());
    out.extend_from_slice(&command.to_be_bytes());
    out.extend_from_slice(&((body.len() + 8) as u32).to_be_bytes());
    out.extend_from_slice(body);
    let crc = crc32fast::hash(&out);
    out.extend_from_slice(&crc.to_be_bytes());
    out.extend_from_slice(&SUFFIX_55AA.to_be_bytes());
    out
}

fn frame_6699(key: &[u8; 16], seq: u32, command: u32, mut plain: Vec<u8>) -> Vec<u8> {
    let mut out = Vec::with_capacity(plain.len() + 50);
    out.extend_from_slice(&PREFIX_6699.to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes());
    out.extend_from_slice(&seq.to_be_bytes());
    out.extend_from_slice(&command.to_be_bytes());
    out.extend_from_slice(&((plain.len() + 12 + 16) as u32).to_be_bytes());
    let mut nonce = [0; 12];
    random(&mut nonce);
    let cipher = Aes128Gcm::new(GenericArray::from_slice(key));
    let tag = cipher
        .encrypt_in_place_detached(Nonce::from_slice(&nonce), &out[4..18], &mut plain)
        .unwrap_or_else(|_| Tag::default());
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&plain);
    out.extend_from_slice(&tag);
    out.extend_from_slice(&SUFFIX_6699.to_be_bytes());
    out
}

fn be32(bytes: &[u8]) -> u32 {
    let mut word = [0; 4];
    word.copy_from_slice(&bytes[..4]);
    u32::from_be_bytes(word)
}

/// Decodes one frame of either kind. `side` is the decoding end: frames a
/// client receives come from a device and carry a return code.
pub fn decode_frame(
    buffer: &[u8],
    key: &[u8; 16],
    side: Side,
    version: Version,
) -> Result<Option<(Frame, usize)>, Error> {
    if buffer.len() < 4 {
        return Ok(None);
    }
    match be32(buffer) {
        PREFIX_55AA => {
            if buffer.len() < 16 {
                return Ok(None);
            }
            let len = be32(&buffer[12..]) as usize;
            if !(8..=MAX_LEN).contains(&len) {
                return Err(Error::Malformed("length"));
            }
            let total = 16 + len;
            if buffer.len() < total {
                return Ok(None);
            }
            if crc32fast::hash(&buffer[..total - 8]) != be32(&buffer[total - 8..]) {
                return Err(Error::Integrity);
            }
            let mut body = &buffer[16..total - 8];
            let mut retcode = None;
            // A return code comes before any version header; an encrypted
            // payload alone is a whole number of blocks.
            if side == Side::Client && !body.starts_with(b"3.") && body.len() >= 4 {
                let rest = &body[4..];
                if rest.is_empty() || rest.len().is_multiple_of(16) || rest.starts_with(b"3.") {
                    retcode = Some(be32(body));
                    body = rest;
                }
            }
            if body.starts_with(b"3.") && body.len() >= 15 {
                body = &body[15..];
            }
            let payload = if body.is_empty() { Vec::new() } else { ecb_decrypt(key, body)? };
            let frame = Frame { seq: be32(&buffer[4..]), command: be32(&buffer[8..]), retcode, payload };
            Ok(Some((frame, total)))
        }
        PREFIX_6699 => {
            if buffer.len() < 18 {
                return Ok(None);
            }
            let len = be32(&buffer[14..]) as usize;
            if !(28..=MAX_LEN).contains(&len) {
                return Err(Error::Malformed("length"));
            }
            let total = 18 + len + 4;
            if buffer.len() < total {
                return Ok(None);
            }
            let nonce = &buffer[18..30];
            let tag = &buffer[total - 20..total - 4];
            let mut plain = buffer[30..total - 20].to_vec();
            let cipher = Aes128Gcm::new(GenericArray::from_slice(key));
            cipher
                .decrypt_in_place_detached(Nonce::from_slice(nonce), &buffer[4..18], &mut plain, Tag::from_slice(tag))
                .map_err(|_| Error::Integrity)?;
            let mut body = plain.as_slice();
            let mut retcode = None;
            if side == Side::Client && body.len() >= 4 && !body.starts_with(b"{") {
                retcode = Some(be32(body));
                body = &body[4..];
            }
            if body.starts_with(version.as_str().as_bytes()) && body.len() >= 15 {
                body = &body[15..];
            }
            let frame =
                Frame { seq: be32(&buffer[6..]), command: be32(&buffer[10..]), retcode, payload: body.to_vec() };
            Ok(Some((frame, total)))
        }
        _ => Err(Error::Malformed("prefix")),
    }
}

pub fn ecb_encrypt(key: &[u8; 16], plain: &[u8]) -> Vec<u8> {
    let cipher = Aes128::new(GenericArray::from_slice(key));
    let pad = 16 - plain.len() % 16;
    let mut data = plain.to_vec();
    data.extend(std::iter::repeat_n(pad as u8, pad));
    for block in data.as_chunks_mut::<16>().0 {
        cipher.encrypt_block(GenericArray::from_mut_slice(block));
    }
    data
}

pub fn ecb_decrypt(key: &[u8; 16], data: &[u8]) -> Result<Vec<u8>, Error> {
    if data.is_empty() || !data.len().is_multiple_of(16) {
        return Err(Error::Malformed("payload is not whole blocks"));
    }
    let cipher = Aes128::new(GenericArray::from_slice(key));
    let mut plain = data.to_vec();
    for block in plain.as_chunks_mut::<16>().0 {
        cipher.decrypt_block(GenericArray::from_mut_slice(block));
    }
    let pad = plain.last().copied().unwrap_or(0) as usize;
    if pad == 0 || pad > 16 || plain[plain.len() - pad..].iter().any(|&byte| byte as usize != pad) {
        return Err(Error::Integrity);
    }
    plain.truncate(plain.len() - pad);
    Ok(plain)
}

fn hmac(key: &[u8; 16], message: &[u8]) -> [u8; 32] {
    let Ok(mut mac) = <Hmac<Sha256> as Mac>::new_from_slice(key) else { return [0; 32] };
    mac.update(message);
    mac.finalize().into_bytes().into()
}

fn hmac_verify(key: &[u8; 16], message: &[u8], expected: &[u8]) -> bool {
    let Ok(mut mac) = <Hmac<Sha256> as Mac>::new_from_slice(key) else { return false };
    mac.update(message);
    mac.verify_slice(expected).is_ok()
}

/// Both nonces XORed, encrypted with GCM under the local key with the
/// client's nonce as IV: the first block of the ciphertext.
fn session_key(local_key: &[u8; 16], local_nonce: &[u8; 16], remote_nonce: &[u8; 16]) -> [u8; 16] {
    let mut mixed: Vec<u8> = local_nonce.iter().zip(remote_nonce).map(|(left, right)| left ^ right).collect();
    let cipher = Aes128Gcm::new(GenericArray::from_slice(local_key));
    // The tag is not part of the key.
    let _ = cipher.encrypt_in_place_detached(Nonce::from_slice(&local_nonce[..12]), &[], &mut mixed);
    let mut key = [0; 16];
    key.copy_from_slice(&mixed[..16]);
    key
}

fn random(buffer: &mut [u8]) {
    if getrandom::fill(buffer).is_err() {
        // Without system randomness, still never reuse a nonce within a run.
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_nanos();
        for (index, byte) in buffer.iter_mut().enumerate() {
            *byte = (now >> ((index % 16) * 8)) as u8 ^ index as u8;
        }
    }
}

/// A key as stored: 16 characters, used as bytes.
pub fn parse_key(text: &str) -> Option<[u8; 16]> {
    let bytes = text.as_bytes();
    (bytes.len() == 16).then(|| {
        let mut key = [0; 16];
        key.copy_from_slice(bytes);
        key
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: [u8; 16] = *b"0123456789abcdef";

    fn roundtrip(version: Version) {
        let mut client = Codec::new(version, Side::Client, KEY);
        let device = Codec::new(version, Side::Device, KEY);
        let sent = client.encode(command::CONTROL, br#"{"dps":{"1":true}}"#);
        let (frame, used) = device.decode(&sent).expect("valid").expect("complete");
        assert_eq!(used, sent.len());
        assert_eq!(frame.command, command::CONTROL);
        assert_eq!(frame.retcode, None);
        assert_eq!(frame.payload, br#"{"dps":{"1":true}}"#);

        let reply = device.encode_seq(frame.seq, command::STATUS, br#"{"dps":{"1":false}}"#);
        let (frame, _) = client.decode(&reply).expect("valid").expect("complete");
        assert_eq!(frame.retcode, Some(0));
        assert_eq!(frame.payload, br#"{"dps":{"1":false}}"#);

        let ack = device.encode_seq(1, command::CONTROL, b"");
        let (frame, _) = client.decode(&ack).expect("valid").expect("complete");
        assert_eq!(frame.retcode, Some(0));
        assert!(frame.payload.is_empty());
    }

    #[test]
    fn frames_roundtrip_on_3_3() {
        roundtrip(Version::V33);
    }

    #[test]
    fn frames_roundtrip_on_3_5() {
        roundtrip(Version::V35);
    }

    #[test]
    fn incomplete_frames_wait_for_more() {
        let mut client = Codec::new(Version::V33, Side::Client, KEY);
        let sent = client.encode(command::QUERY, b"{}");
        let device = Codec::new(Version::V33, Side::Device, KEY);
        assert_eq!(device.decode(&sent[..sent.len() - 1]), Ok(None));
    }

    #[test]
    fn a_wrong_key_is_noticed() {
        let mut client = Codec::new(Version::V35, Side::Client, KEY);
        let sent = client.encode(command::QUERY_NEW, b"{}");
        let device = Codec::new(Version::V35, Side::Device, *b"fedcba9876543210");
        assert_eq!(device.decode(&sent), Err(Error::Integrity));
    }

    #[test]
    fn the_3_5_handshake_agrees_on_a_key() {
        let mut client = Codec::new(Version::V35, Side::Client, KEY);
        let device = Codec::new(Version::V35, Side::Device, KEY);
        let (local_nonce, start) = client.session_start();
        let (frame, _) = device.decode(&start).expect("valid").expect("complete");
        assert_eq!(frame.payload, local_nonce);
        let (remote_nonce, reply) = device.session_reply(frame.seq, &frame.payload);
        let (frame, _) = client.decode(&reply).expect("valid").expect("complete");
        let (finish, client_key) = client.session_finish(&local_nonce, &frame.payload).expect("handshake");
        let (frame, _) = device.decode(&finish).expect("valid").expect("complete");
        let device_key = device.session_accept(&local_nonce, &remote_nonce, &frame.payload).expect("accepted");
        assert_eq!(client_key, device_key);
        assert_ne!(client_key, KEY);
    }

    #[test]
    fn a_forged_handshake_reply_is_refused() {
        let mut client = Codec::new(Version::V35, Side::Client, KEY);
        let (nonce, _) = client.session_start();
        assert_eq!(client.session_finish(&nonce, &[0; 48]).map(|_| ()), Err(Error::Handshake));
    }

    fn unhex(text: &str) -> Vec<u8> {
        (0..text.len()).step_by(2).map(|start| u8::from_str_radix(&text[start..start + 2], 16).unwrap_or(0)).collect()
    }

    /// Frames built independently, the way tinytuya packs them.
    #[test]
    fn reference_frames_decode() {
        let v33 = unhex(
            "000055aa00000007000000080000004b00000000332e33000000000000000000000000c27ee03f8be481e63320de7dc6eb4296\
             8881c9a3b4324017348d7c721e913f93377222e061a924c591cd9c27ea163ed430625bc80000aa55",
        );
        let client = Codec::new(Version::V33, Side::Client, KEY);
        let (frame, used) = client.decode(&v33).expect("valid").expect("complete");
        assert_eq!(used, v33.len());
        assert_eq!((frame.seq, frame.command, frame.retcode), (7, command::STATUS, Some(0)));
        assert_eq!(frame.payload, br#"{"dps":{"1":true,"2":500},"t":1}"#);

        let v35 = unhex(
            "000066990000000000090000000800000042000102030405060708090a0bfd3fc41d2d5a0e36e826071ac09bb02d00d75781\
             25d73a38555391d1de89910a3f7bec8b8d93742afc5f2120558ccace89e7183845a500009966",
        );
        let client = Codec::new(Version::V35, Side::Client, KEY);
        let (frame, used) = client.decode(&v35).expect("valid").expect("complete");
        assert_eq!(used, v35.len());
        assert_eq!((frame.seq, frame.command, frame.retcode), (9, command::STATUS, Some(0)));
        assert_eq!(frame.payload, br#"{"dps":{"1":false}}"#);
    }

    #[test]
    fn ecb_matches_known_padding() {
        let data = ecb_encrypt(&KEY, b"");
        assert_eq!(data.len(), 16);
        assert_eq!(ecb_decrypt(&KEY, &data).expect("decrypts"), b"");
    }
}
