//! The people who can log in: ordinary accounts from the passwd database,
//! with AccountsService avatars where one is set.

use std::path::{Path, PathBuf};

use crate::theme::Colour;

#[derive(Debug, Clone, PartialEq)]
pub struct Account {
    pub login: String,
    /// The full name from the GECOS field, or the login.
    pub name: String,
    pub uid: u32,
    pub icon: Option<PathBuf>,
    /// The accent pair their session shared, from their card.
    pub accent: Option<(Colour, Colour)>,
}

/// Where AccountsService keeps avatars, readable by everyone.
const ICONS: &str = "/var/lib/AccountsService/icons";

/// The range of ordinary accounts, from `/etc/login.defs`.
fn uid_range(login_defs: &str) -> (u32, u32) {
    let value = |key: &str| {
        login_defs.lines().find_map(|line| {
            let mut words = line.split_whitespace();
            (words.next() == Some(key)).then(|| words.next()?.parse().ok()).flatten()
        })
    };
    (value("UID_MIN").unwrap_or(1000), value("UID_MAX").unwrap_or(60000))
}

/// Shells that mean "no logins".
fn can_log_in(shell: &str) -> bool {
    let name = shell.rsplit('/').next().unwrap_or(shell);
    !shell.is_empty() && !matches!(name, "nologin" | "false" | "sync" | "shutdown" | "halt")
}

/// Ordinary accounts in `passwd`, in uid order, leaving out `hidden` logins.
pub fn parse(passwd: &str, login_defs: &str, hidden: &[String]) -> Vec<Account> {
    let (min, max) = uid_range(login_defs);
    let mut accounts: Vec<Account> = passwd
        .lines()
        .filter_map(|line| {
            let fields: Vec<&str> = line.split(':').collect();
            let [login, _, uid, _, gecos, _, shell] = fields[..] else { return None };
            let uid: u32 = uid.parse().ok()?;
            if !(min..=max).contains(&uid) || !can_log_in(shell) || hidden.iter().any(|h| h == login) {
                return None;
            }
            let full = gecos.split(',').next().unwrap_or_default().trim();
            let name = if full.is_empty() { login.to_string() } else { full.to_string() };
            Some(Account { login: login.to_string(), name, uid, icon: None, accent: None })
        })
        .collect();
    accounts.sort_by_key(|a| a.uid);
    accounts.dedup_by(|a, b| a.login == b.login);
    accounts
}

/// The accounts on this machine. `TILECEPTION_GREETER_PASSWD` points at
/// another passwd file, for tests.
pub fn list(hidden: &[String]) -> Vec<Account> {
    let passwd_path =
        std::env::var_os("TILECEPTION_GREETER_PASSWD").map(PathBuf::from).unwrap_or_else(|| "/etc/passwd".into());
    let passwd = std::fs::read_to_string(&passwd_path).unwrap_or_else(|e| {
        tracing::error!("greeter: could not read {}: {e}", passwd_path.display());
        String::new()
    });
    let login_defs = std::fs::read_to_string("/etc/login.defs").unwrap_or_default();
    let mut accounts = parse(&passwd, &login_defs, hidden);
    for account in &mut accounts {
        account.icon = icon(Path::new(ICONS), &account.login);
    }
    accounts
}

fn icon(dir: &Path, login: &str) -> Option<PathBuf> {
    let path = dir.join(login);
    std::fs::metadata(&path).is_ok_and(|m| m.is_file() && m.len() > 0).then_some(path)
}

/// Accent pairs for people who have not shared theirs, checked for contrast
/// against the dark tile surface and the dark text drawn on them.
const ACCENTS: [(&str, &str); 8] = [
    ("#2bd4a7", "#5c8ff2"),
    ("#f2b04c", "#f0646a"),
    ("#a78bfa", "#5c8ff2"),
    ("#5cc8f2", "#2bd4a7"),
    ("#f08cc0", "#a78bfa"),
    ("#9ad85c", "#2bd4a7"),
    ("#f2865c", "#f2b04c"),
    ("#7aa2ff", "#c08cf0"),
];

/// A pair that stays the same for a login across boots and usually differs
/// between people.
pub fn accent(login: &str) -> (Colour, Colour) {
    // FNV-1a: stable, unlike the standard library's randomly seeded hashers.
    let hash = login.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |h, b| (h ^ u64::from(b)).wrapping_mul(0x100_0000_01b3));
    let (a, b) = ACCENTS[(hash % ACCENTS.len() as u64) as usize];
    (Colour::parse(a).unwrap_or(Colour::TRANSPARENT), Colour::parse(b).unwrap_or(Colour::TRANSPARENT))
}

/// An avatar `size` pixels across, cut to a circle, premultiplied RGBA.
pub fn avatar_pixels(path: &Path, size: u32) -> Option<Vec<u8>> {
    let reader = image::ImageReader::open(path).ok()?.with_guessed_format().ok()?;
    let image = reader.decode().ok()?.resize_to_fill(size, size, image::imageops::FilterType::Triangle).to_rgba8();
    let r = size as f32 / 2.0;
    let mut pixels = image.into_raw();
    for (i, px) in pixels.as_chunks_mut::<4>().0.iter_mut().enumerate() {
        let (x, y) = ((i as u32 % size) as f32 + 0.5, (i as u32 / size) as f32 + 0.5);
        // A one pixel soft edge.
        let coverage = (r - ((x - r).powi(2) + (y - r).powi(2)).sqrt() + 0.5).clamp(0.0, 1.0);
        let alpha = f32::from(px[3]) / 255.0 * coverage;
        for c in &mut px[..3] {
            *c = (f32::from(*c) * alpha).round() as u8;
        }
        px[3] = (alpha * 255.0).round() as u8;
    }
    Some(pixels)
}

#[cfg(test)]
mod tests {
    use super::*;

    const PASSWD: &str = "\
root:x:0:0:root:/root:/bin/bash
daemon:x:1:1:daemon:/usr/sbin:/usr/sbin/nologin
robin:x:1000:1000:Robin van der Noord,,,:/home/robin:/bin/bash
backup:x:1001:1001::/home/backup:/bin/sh
sam:x:1002:1002:Sam:/home/sam:/usr/bin/zsh
ghost:x:1003:1003:Ghost:/home/ghost:/usr/sbin/nologin
nobody:x:65534:65534:nobody:/nonexistent:/usr/sbin/nologin
broken line
";

    #[test]
    fn lists_ordinary_accounts() {
        let accounts = parse(PASSWD, "", &["backup".into()]);
        let logins: Vec<&str> = accounts.iter().map(|a| a.login.as_str()).collect();
        assert_eq!(logins, ["robin", "sam"]);
        assert_eq!(accounts[0].name, "Robin van der Noord");
    }

    #[test]
    fn follows_login_defs() {
        let defs = "# comment\nUID_MIN\t\t 1002\nUID_MAX 1002\n";
        let logins: Vec<String> = parse(PASSWD, defs, &[]).into_iter().map(|a| a.login).collect();
        assert_eq!(logins, ["sam"]);
    }

    #[test]
    fn falls_back_to_the_login() {
        let accounts = parse("alex:x:1005:1005::/home/alex:/bin/bash\n", "", &[]);
        assert_eq!(accounts[0].name, "alex");
    }

    #[test]
    fn accents_are_stable_and_varied() {
        assert_eq!(accent("robin"), accent("robin"));
        let distinct: std::collections::HashSet<String> =
            ["robin", "sam", "alex", "kim", "jo", "max"].iter().map(|l| format!("{:?}", accent(l))).collect();
        assert!(distinct.len() >= 3, "{distinct:?}");
    }
}
