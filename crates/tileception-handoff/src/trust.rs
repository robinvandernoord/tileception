//! Which machines this one trusts, by their static keys, and which keys it
//! refuses.
//!
//! The passphrase lets a machine in for the first time. Both sides then pin
//! each other's key, and from then on the key is enough: a pinned machine
//! connects even after the passphrase changed. Forgetting a machine moves its
//! key to the refused list, which survives restarts, so it cannot connect
//! with that key again, not even with the passphrase. It can still make a
//! new key, so forgetting a machine that knows the passphrase only keeps it
//! out once the passphrase changes too.

use std::{
    fs::{self, OpenOptions},
    io::{self, Write},
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
};

use zeroize::Zeroizing;

use crate::service::PeerId;

/// A machine's public static key (X25519).
pub type Key = [u8; 32];

/// The most refused keys kept and passed on. Each costs 33 bytes and a name
/// on the wire, so a full list still fits in one frame.
const MOST_FORGOTTEN: usize = 512;
/// Names are what the other machine says it is called: kept short and on
/// one line.
const LONGEST_NAME: usize = 64;

/// A machine this one connects to without the passphrase.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Trusted {
    pub key: Key,
    pub machine: PeerId,
    pub name: String,
}

/// A key this machine refuses, and the name it had when it was forgotten.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Forgotten {
    pub key: Key,
    pub name: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Standing {
    /// Pinned: connects with its key alone.
    Known,
    /// Never seen, or seen with another key: needs the passphrase.
    Unknown,
    /// Forgotten: refused even with the passphrase.
    Refused,
}

/// A short, readable form of a key for telling machines apart: the first
/// eight bytes in groups of four hex digits.
pub fn fingerprint(key: &Key) -> String {
    key[..8].chunks(2).map(|pair| format!("{:02x}{:02x}", pair[0], pair[1])).collect::<Vec<_>>().join(" ")
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn unhex(text: &str) -> Option<Key> {
    if text.len() != 64 || !text.is_ascii() {
        return None;
    }
    let mut key = [0; 32];
    for (index, byte) in key.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&text[index * 2..index * 2 + 2], 16).ok()?;
    }
    Some(key)
}

/// One line, printable, not too long.
pub(crate) fn clean_name(name: &str) -> String {
    let name: String = name.chars().map(|char| if char.is_control() { ' ' } else { char }).take(LONGEST_NAME).collect();
    name.trim().to_string()
}

#[derive(Debug, Default)]
pub(crate) struct Trust {
    path: Option<PathBuf>,
    trusted: Vec<Trusted>,
    /// Oldest first.
    forgotten: Vec<Forgotten>,
}

impl Trust {
    /// What `path` says; nothing trusted when it is missing or unreadable.
    /// Without a path nothing is kept between runs.
    pub(crate) fn load(path: Option<PathBuf>) -> Trust {
        let text = path.as_deref().map(fs::read_to_string);
        let mut trust = match text {
            Some(Ok(text)) => Trust::parse(&text),
            Some(Err(error)) if error.kind() != io::ErrorKind::NotFound => {
                tracing::warn!("handoff: cannot read the trusted machines: {error}");
                Trust::default()
            }
            _ => Trust::default(),
        };
        trust.path = path;
        trust
    }

    fn parse(text: &str) -> Trust {
        let mut trust = Trust::default();
        for line in text.lines() {
            let mut fields = line.splitn(4, ' ');
            match (fields.next(), fields.next().and_then(unhex)) {
                (Some("trust"), Some(key)) => {
                    let Some(machine) = fields.next().and_then(|hex| PeerId::from_str_radix(hex, 16).ok()) else {
                        continue;
                    };
                    let name = clean_name(fields.next().unwrap_or_default());
                    trust.pin(Trusted { key, machine, name });
                }
                (Some("forget"), Some(key)) => {
                    let rest = [fields.next(), fields.next()].into_iter().flatten().collect::<Vec<_>>().join(" ");
                    trust.forget(key, &rest);
                }
                _ => {}
            }
        }
        trust
    }

    fn render(&self) -> String {
        let mut text = String::from(
            "# Tileception handoff: machines this one trusts, and keys it refuses.\n\
             # Change with `tileception msg handoff forget` and `handoff new-key`.\n",
        );
        for trusted in &self.trusted {
            text.push_str(&format!("trust {} {:016x} {}\n", hex(&trusted.key), trusted.machine, trusted.name));
        }
        for forgotten in &self.forgotten {
            text.push_str(&format!("forget {} {}\n", hex(&forgotten.key), forgotten.name));
        }
        text
    }

    /// Writes the lists back, when they are kept.
    pub(crate) fn save(&self) {
        let Some(path) = &self.path else { return };
        if let Err(error) = write_private(path, self.render().as_bytes()) {
            tracing::warn!("handoff: cannot keep the trusted machines in {}: {error}", path.display());
        }
    }

    pub(crate) fn standing(&self, key: &Key) -> Standing {
        if self.forgotten.iter().any(|forgotten| forgotten.key == *key) {
            Standing::Refused
        } else if self.trusted.iter().any(|trusted| trusted.key == *key) {
            Standing::Known
        } else {
            Standing::Unknown
        }
    }

    pub(crate) fn trusted(&self) -> &[Trusted] {
        &self.trusted
    }

    pub(crate) fn forgotten(&self) -> &[Forgotten] {
        &self.forgotten
    }

    /// Trusts a machine's key, in place of any key it had before. Whether
    /// anything changed. A refused key is never pinned.
    pub(crate) fn pin(&mut self, trusted: Trusted) -> bool {
        if self.standing(&trusted.key) == Standing::Refused {
            return false;
        }
        let trusted = Trusted { name: clean_name(&trusted.name), ..trusted };
        if self.trusted.contains(&trusted) {
            return false;
        }
        self.trusted.retain(|old| old.key != trusted.key && old.machine != trusted.machine);
        self.trusted.push(trusted);
        true
    }

    /// The machine with this key moved to a new one, which it told over a
    /// connection secured with the old one. The old key is refused from now
    /// on. Whether the new key was taken.
    pub(crate) fn replace(&mut self, old: &Key, new: Key) -> bool {
        if self.standing(&new) != Standing::Unknown {
            return false;
        }
        let Some(trusted) = self.trusted.iter().find(|trusted| trusted.key == *old).cloned() else {
            return false;
        };
        self.forget(*old, &trusted.name);
        self.trusted.push(Trusted { key: new, ..trusted });
        true
    }

    /// Refuses a key from now on. The machine it belonged to when it was
    /// trusted; `None` as well when the key was refused already.
    pub(crate) fn forget(&mut self, key: Key, name: &str) -> Option<Option<Trusted>> {
        if self.standing(&key) == Standing::Refused {
            return None;
        }
        let index = self.trusted.iter().position(|trusted| trusted.key == key);
        let was = index.map(|index| self.trusted.remove(index));
        let name = was.as_ref().map_or_else(|| clean_name(name), |was| was.name.clone());
        self.forgotten.push(Forgotten { key, name });
        if self.forgotten.len() > MOST_FORGOTTEN {
            self.forgotten.remove(0);
        }
        Some(was)
    }
}

/// This machine's static key pair.
#[derive(Clone)]
pub(crate) struct Keys {
    pub(crate) private: Zeroizing<Vec<u8>>,
    pub(crate) public: Key,
}

impl Keys {
    pub(crate) fn generate() -> io::Result<Keys> {
        let builder = snow::Builder::new(crate::secure::PATTERN.parse().map_err(io::Error::other)?);
        let pair = builder.generate_keypair().map_err(io::Error::other)?;
        let public = pair.public.as_slice().try_into().map_err(|_| io::Error::other("unexpected key size"))?;
        Ok(Keys { private: Zeroizing::new(pair.private), public })
    }

    /// The pair kept in `path`, or a new one (kept there when possible).
    pub(crate) fn load_or_create(path: Option<&Path>) -> io::Result<Keys> {
        if let Some(path) = path {
            match fs::read_to_string(path).map(Zeroizing::new) {
                Ok(text) => {
                    if let Some(keys) = Keys::parse(&text) {
                        return Ok(keys);
                    }
                    tracing::warn!("handoff: {} is not a key; making a new one", path.display());
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => tracing::warn!("handoff: cannot read {}: {error}; making a new key", path.display()),
            }
        }
        let keys = Keys::generate()?;
        if let Some(path) = path
            && let Err(error) = keys.save(path)
        {
            tracing::warn!(
                "handoff: cannot keep this machine's key in {}: {error}; other machines will need the \
                 passphrase again after a restart",
                path.display()
            );
        }
        Ok(keys)
    }

    fn parse(text: &str) -> Option<Keys> {
        let mut lines = text.lines().filter(|line| !line.starts_with('#'));
        let private = Zeroizing::new(unhex(lines.next()?.trim())?);
        let public = unhex(lines.next()?.trim())?;
        Some(Keys { private: Zeroizing::new(private.to_vec()), public })
    }

    pub(crate) fn save(&self, path: &Path) -> io::Result<()> {
        let text = Zeroizing::new(format!(
            "# Tileception handoff: this machine's private key, then its public key. Keep it secret.\n{}\n{}\n",
            hex(&self.private),
            hex(&self.public)
        ));
        write_private(path, text.as_bytes())
    }
}

/// Replaces a file readable by its owner only, all at once.
fn write_private(path: &Path, bytes: &[u8]) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut temporary = path.as_os_str().to_owned();
    temporary.push(".new");
    let temporary = PathBuf::from(temporary);
    let mut file = OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&temporary)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    fs::rename(&temporary, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn trusted(byte: u8, machine: PeerId, name: &str) -> Trusted {
        Trusted { key: [byte; 32], machine, name: name.into() }
    }

    #[test]
    fn lists_survive_being_written() {
        let mut trust = Trust::default();
        assert!(trust.pin(trusted(1, 0xa1, "Robin's laptop")));
        assert!(trust.pin(trusted(2, 0xb2, "desk")));
        assert_eq!(trust.forget([2; 32], ""), Some(Some(trusted(2, 0xb2, "desk"))));
        assert_eq!(trust.forget([3; 32], "stranger"), Some(None));
        let again = Trust::parse(&trust.render());
        assert_eq!(again.trusted(), trust.trusted());
        assert_eq!(again.forgotten(), trust.forgotten());
        assert_eq!(again.standing(&[1; 32]), Standing::Known);
        assert_eq!(again.standing(&[2; 32]), Standing::Refused);
        assert_eq!(again.standing(&[4; 32]), Standing::Unknown);
    }

    #[test]
    fn a_machine_has_one_key() {
        let mut trust = Trust::default();
        trust.pin(trusted(1, 0xa1, "laptop"));
        // Paired again with the passphrase after it lost its key.
        assert!(trust.pin(trusted(2, 0xa1, "laptop")));
        assert_eq!(trust.trusted(), [trusted(2, 0xa1, "laptop")]);
        assert_eq!(trust.standing(&[1; 32]), Standing::Unknown);
        assert!(!trust.pin(trusted(2, 0xa1, "laptop")));
    }

    #[test]
    fn forgotten_keys_stay_refused() {
        let mut trust = Trust::default();
        trust.pin(trusted(1, 0xa1, "laptop"));
        assert!(trust.forget([1; 32], "").is_some());
        assert!(trust.forget([1; 32], "").is_none());
        assert!(!trust.pin(trusted(1, 0xa1, "laptop")));
        assert!(trust.trusted().is_empty());
        assert_eq!(trust.forgotten(), [Forgotten { key: [1; 32], name: "laptop".into() }]);
    }

    #[test]
    fn a_new_key_replaces_the_old_one() {
        let mut trust = Trust::default();
        trust.pin(trusted(1, 0xa1, "laptop"));
        trust.pin(trusted(2, 0xb2, "desk"));
        assert!(trust.replace(&[1; 32], [3; 32]));
        assert_eq!(trust.standing(&[1; 32]), Standing::Refused);
        assert!(trust.trusted().contains(&trusted(3, 0xa1, "laptop")));
        // Not to a key that belongs to another machine or was refused.
        assert!(!trust.replace(&[3; 32], [2; 32]));
        assert!(!trust.replace(&[3; 32], [1; 32]));
        // Not for a machine that is not trusted.
        assert!(!trust.replace(&[9; 32], [8; 32]));
    }

    #[test]
    fn names_stay_on_one_line() {
        let mut trust = Trust::default();
        trust.pin(trusted(1, 0xa1, "evil\ntrust 0000 00 x"));
        let again = Trust::parse(&trust.render());
        assert_eq!(again.trusted().len(), 1);
        assert_eq!(again.trusted()[0].name, "evil trust 0000 00 x");
    }

    #[test]
    fn refused_keys_are_bounded() {
        let mut trust = Trust::default();
        for index in 0..=MOST_FORGOTTEN {
            let mut key = [0; 32];
            key[..8].copy_from_slice(&(index as u64).to_le_bytes());
            trust.forget(key, "");
        }
        assert_eq!(trust.forgotten().len(), MOST_FORGOTTEN);
        assert_eq!(trust.standing(&[0; 32]), Standing::Unknown);
    }

    #[test]
    fn keys_are_kept_privately() {
        let directory = std::env::temp_dir().join(format!("tileception-handoff-keys-{:016x}", crate::random_id()));
        let path = directory.join("key");
        let keys = Keys::load_or_create(Some(&path)).expect("a key");
        let again = Keys::load_or_create(Some(&path)).expect("the same key");
        assert_eq!(again.public, keys.public);
        assert_eq!(*again.private, *keys.private);
        let mode = fs::metadata(&path).expect("kept").permissions();
        assert_eq!(std::os::unix::fs::PermissionsExt::mode(&mode) & 0o777, 0o600);
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn fingerprints_are_short() {
        let mut key = [0; 32];
        key[..8].copy_from_slice(&[0xa1, 0xb2, 0xc3, 0xd4, 0xe5, 0xf6, 0x07, 0x18]);
        assert_eq!(fingerprint(&key), "a1b2 c3d4 e5f6 0718");
    }
}
