//! The launcher's instant answers: sums and conversions (`calc`), the time
//! somewhere else, days until a date, unix timestamps, colours, characters,
//! and paths or links to open. All local; nothing here waits on the network.

use std::path::{Path, PathBuf};

use crate::zones::{self, Civil, MONTHS};

/// Something to copy with Enter.
#[derive(Debug, Clone, PartialEq)]
pub struct Answer {
    /// The big line: the result.
    pub title: String,
    /// What it is the answer to, or more about it.
    pub detail: String,
    /// What Enter puts on the clipboard.
    pub copy: String,
    pub look: Look,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Look {
    Sum,
    Clock,
    Calendar,
    /// A colour swatch: straight sRGB.
    Colour([u8; 3]),
    /// The character itself stands in for an icon.
    Character,
}

/// A file, folder or link Enter opens.
#[derive(Debug, Clone, PartialEq)]
pub struct Place {
    pub title: String,
    pub detail: String,
    /// What `xdg-open` gets.
    pub target: String,
    /// What Tab puts in the field, for paths.
    pub completion: Option<String>,
    pub folder: bool,
    pub link: bool,
}

/// The answers for what was typed; `now` is the time in seconds since the epoch.
pub fn answers(text: &str, now: i64) -> Vec<Answer> {
    let text = text.trim();
    if let Some(query) = text.strip_prefix(':') {
        return characters(query, 8);
    }
    let mut found = Vec::new();
    if let Some(answer) = crate::calc::answer(text) {
        found.push(Answer { title: answer.shown, detail: format!("{text} ="), copy: answer.copy, look: Look::Sum });
    }
    found.extend(time_in(text, now));
    found.extend(days_between(text, now));
    found.extend(timestamp(text, now));
    found.extend(colours(text));
    found
}

/// The command after `>`, when that is what was typed.
pub fn command(text: &str) -> Option<&str> {
    text.trim_start().strip_prefix('>').map(str::trim)
}

/// "time in tokyo", "tokyo time", "what time is it in new york".
fn time_in(text: &str, now: i64) -> Option<Answer> {
    let lower = text.to_lowercase();
    let lower = lower.trim_end_matches('?').trim();
    let place = ["what time is it in ", "current time in ", "time in ", "time at "]
        .iter()
        .find_map(|prefix| lower.strip_prefix(prefix))
        .or_else(|| lower.strip_suffix(" time"))?;
    let (name, zone) = zones::find(place)?;
    let local = zone.local(now);
    let there = Civil::at(now + local.offset);
    let ahead = local.offset - zones::local_offset(now);
    let relative = match ahead {
        0 => "same as here".to_string(),
        _ => {
            let (h, m) = (ahead.abs() / 3600, ahead.abs() / 60 % 60);
            let amount = if m == 0 { format!("{h} h") } else { format!("{h}:{m:02} h") };
            format!("{amount} {}", if ahead > 0 { "ahead" } else { "behind" })
        }
    };
    let clock = format!("{:02}:{:02}", there.hour, there.minute);
    Some(Answer {
        detail: format!(
            "{} \u{b7} {} ({}) \u{b7} {name} \u{b7} {relative}",
            there.long_date(),
            local.abbreviation,
            zones::offset_label(local.offset)
        ),
        copy: clock.clone(),
        title: clock,
        look: Look::Clock,
    })
}

/// "days until 2026-12-25", "days since 1 january".
fn days_between(text: &str, now: i64) -> Option<Answer> {
    let lower = text.to_lowercase();
    let (until, rest) = ["days until ", "days till ", "days to "]
        .iter()
        .find_map(|prefix| lower.strip_prefix(prefix).map(|rest| (true, rest)))
        .or_else(|| ["days since ", "days from "].iter().find_map(|p| lower.strip_prefix(p).map(|r| (false, r))))?;
    let today = (now + zones::local_offset(now)).div_euclid(86_400);
    let (year, month, day) = parse_date(rest.trim(), today, until)?;
    let target = zones::days_from_civil(year, month, day);
    let days = if until { target - today } else { today - target };
    let unit = if days.abs() == 1 { "day" } else { "days" };
    let date = Civil::at(target * 86_400);
    Some(Answer {
        title: format!("{days} {unit}"),
        detail: format!("{} {} {year}", if until { "until" } else { "since" }, date.long_date()),
        copy: days.to_string(),
        look: Look::Calendar,
    })
}

/// "2026-12-25", "25-12-2026", "25.12.2026", "25 december [2026]",
/// "december 25 [2026]". Without a year: the next such day when counting
/// down, the last one when counting up.
fn parse_date(text: &str, today: i64, upcoming: bool) -> Option<(i64, u32, u32)> {
    let numbers: Vec<&str> = text.split(['-', '.', '/']).collect();
    if let [a, b, c] = numbers[..]
        && let (Ok(a), Ok(b), Ok(c)) = (a.trim().parse::<i64>(), b.trim().parse::<u32>(), c.trim().parse::<i64>())
    {
        let (year, month, day) = if a > 31 { (a, b, c as u32) } else { (c, b, a as u32) };
        return zones::valid_date(year, month, day).then_some((year, month, day));
    }
    let words: Vec<&str> = text.split_whitespace().collect();
    let month = |word: &str| {
        (word.len() >= 3).then_some(())?;
        MONTHS.iter().position(|m| m.to_lowercase().starts_with(word)).map(|i| i as u32 + 1)
    };
    let (day, month, year) = match words[..] {
        [d, m] | [m, d] if d.parse::<u32>().is_ok() && month(m).is_some() => (d.parse().ok()?, month(m)?, None),
        [d, m, y] | [m, d, y] if d.parse::<u32>().is_ok() && month(m).is_some() => {
            (d.parse().ok()?, month(m)?, Some(y.parse().ok()?))
        }
        _ => return None,
    };
    let year = year.unwrap_or_else(|| {
        let (this_year, _, _) = zones::civil_from_days(today);
        let this = zones::days_from_civil(this_year, month, day);
        match (upcoming, this < today, this > today) {
            (true, true, _) => this_year + 1,
            (false, _, true) => this_year - 1,
            _ => this_year,
        }
    });
    zones::valid_date(year, month, day).then_some((year, month, day))
}

/// A unix timestamp to a date and back; "unix" or "timestamp" for now.
fn timestamp(text: &str, now: i64) -> Option<Answer> {
    let lower = text.to_lowercase();
    if matches!(lower.as_str(), "unix" | "timestamp" | "unix time" | "epoch") {
        return Some(Answer {
            title: now.to_string(),
            detail: "Unix time now".into(),
            copy: now.to_string(),
            look: Look::Clock,
        });
    }
    if text.bytes().all(|b| b.is_ascii_digit()) && matches!(text.len(), 9 | 10 | 12 | 13) {
        let n: i64 = text.parse().ok()?;
        let (seconds, unit) = if text.len() > 10 { (n / 1000, "milliseconds") } else { (n, "seconds") };
        let local = Civil::at(seconds + zones::local_offset(seconds));
        let local_text = local.iso();
        return Some(Answer {
            detail: format!("{} \u{b7} unix time in {unit} \u{b7} UTC {}", local.long_date(), Civil::at(seconds).iso()),
            copy: local_text.clone(),
            title: local_text,
            look: Look::Calendar,
        });
    }
    // "2026-10-16", "2026-10-16 14:30", "2026-10-16T14:30:05"
    let (date, time) = text.split_once([' ', 'T']).unwrap_or((text, ""));
    let parts: Vec<&str> = date.split('-').collect();
    let [y, m, d] = parts[..] else { return None };
    if y.len() != 4 {
        return None;
    }
    let (year, month, day) = (y.parse().ok()?, m.parse().ok()?, d.parse().ok()?);
    if !zones::valid_date(year, month, day) {
        return None;
    }
    let clock: Vec<u32> = if time.is_empty() {
        vec![0, 0, 0]
    } else {
        time.split(':').map(|p| p.parse().ok()).collect::<Option<Vec<_>>>()?
    };
    let (hour, minute, second) = match clock[..] {
        [h, m] => (h, m, 0),
        [h, m, s] => (h, m, s),
        _ => return None,
    };
    if hour > 23 || minute > 59 || second > 59 {
        return None;
    }
    let civil = zones::days_from_civil(year, month, day) * 86_400
        + i64::from(hour) * 3600
        + i64::from(minute) * 60
        + i64::from(second);
    let seconds = zones::from_local(civil);
    Some(Answer {
        title: seconds.to_string(),
        detail: format!("Unix time of {} local", Civil::at(civil).iso()),
        copy: seconds.to_string(),
        look: Look::Clock,
    })
}

/// A colour in the other notations: hex, rgb() and hsl().
fn colours(text: &str) -> Vec<Answer> {
    let Some(rgb) = parse_colour(text) else { return Vec::new() };
    let [r, g, b] = rgb;
    let (h, s, l) = to_hsl(rgb);
    let notations = [
        (format!("#{r:02x}{g:02x}{b:02x}"), "Hex"),
        (format!("rgb({r}, {g}, {b})"), "Red, green, blue"),
        (format!("hsl({h}, {s}%, {l}%)"), "Hue, saturation, lightness"),
    ];
    notations
        .into_iter()
        .map(|(notation, name)| Answer {
            title: notation.clone(),
            detail: name.to_string(),
            copy: notation,
            look: Look::Colour(rgb),
        })
        .collect()
}

pub fn parse_colour(text: &str) -> Option<[u8; 3]> {
    let text = text.trim().to_lowercase();
    if let Some(hex) = text.strip_prefix('#') {
        let digits: Vec<u8> = hex.chars().map(|c| c.to_digit(16).map(|d| d as u8)).collect::<Option<_>>()?;
        return match digits[..] {
            [r, g, b] | [r, g, b, _] => Some([r * 17, g * 17, b * 17]),
            [r1, r2, g1, g2, b1, b2] | [r1, r2, g1, g2, b1, b2, _, _] => {
                Some([r1 * 16 + r2, g1 * 16 + g2, b1 * 16 + b2])
            }
            _ => None,
        };
    }
    let (function, args) = text.strip_suffix(')')?.split_once('(')?;
    let args: Vec<&str> = args.split([',', ' ', '/']).filter(|a| !a.is_empty()).collect();
    if args.len() != 3 && args.len() != 4 {
        return None;
    }
    let number = |a: &str| a.trim_end_matches(['%', '°']).trim_end_matches("deg").parse::<f64>().ok();
    match function.trim() {
        "rgb" | "rgba" => {
            let channel = |a: &str| {
                let n = number(a)?;
                let n = if a.ends_with('%') { n * 2.55 } else { n };
                (0.0..=255.0).contains(&n).then_some(n.round() as u8)
            };
            Some([channel(args[0])?, channel(args[1])?, channel(args[2])?])
        }
        "hsl" | "hsla" => {
            let (h, s, l) = (number(args[0])?, number(args[1])?, number(args[2])?);
            ((0.0..=100.0).contains(&s) && (0.0..=100.0).contains(&l)).then(|| from_hsl(h, s / 100.0, l / 100.0))
        }
        _ => None,
    }
}

fn to_hsl([r, g, b]: [u8; 3]) -> (i32, i32, i32) {
    let (r, g, b) = (f64::from(r) / 255.0, f64::from(g) / 255.0, f64::from(b) / 255.0);
    let (max, min) = (r.max(g).max(b), r.min(g).min(b));
    let l = (max + min) / 2.0;
    let d = max - min;
    if d == 0.0 {
        return (0, 0, (l * 100.0).round() as i32);
    }
    let s = d / (1.0 - (2.0 * l - 1.0).abs());
    let h = if max == r {
        60.0 * ((g - b) / d).rem_euclid(6.0)
    } else if max == g {
        60.0 * ((b - r) / d + 2.0)
    } else {
        60.0 * ((r - g) / d + 4.0)
    };
    (h.round() as i32 % 360, (s * 100.0).round() as i32, (l * 100.0).round() as i32)
}

fn from_hsl(h: f64, s: f64, l: f64) -> [u8; 3] {
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let h = h.rem_euclid(360.0) / 60.0;
    let x = c * (1.0 - (h % 2.0 - 1.0).abs());
    let (r, g, b) = match h as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = l - c / 2.0;
    [r, g, b].map(|v| ((v + m) * 255.0).round().clamp(0.0, 255.0) as u8)
}

/// Emoji and symbols, by words in their names: every word typed must start a
/// word of the name or of its extra keywords ("lol", "+1"). The best come
/// first (see the tiers below), then shorter names.
pub fn characters(query: &str, limit: usize) -> Vec<Answer> {
    const TABLE: &str = include_str!("emoji.txt");
    let words: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
    if words.is_empty() {
        return Vec::new();
    }
    let mut found: Vec<(u8, usize, &str, &str)> = TABLE
        .lines()
        .filter_map(|line| line.split_once(' '))
        .filter_map(|(c, entry)| {
            let (name, keywords) = entry.split_once('|').unwrap_or((entry, ""));
            let all = name.split(' ').chain(keywords.split(' '));
            if !words.iter().all(|w| all.clone().any(|n| n.starts_with(w.as_str()))) {
                return None;
            }
            // A chosen keyword first ("heart" is ❤), then a whole word ("lol",
            // not "lollipop"), then a name that starts with it.
            let tier = if keywords.split(' ').any(|k| k == words[0]) {
                0
            } else if name.split(' ').any(|n| n == words[0]) {
                1
            } else if name.starts_with(words[0].as_str()) {
                2
            } else {
                3
            };
            Some((tier, name.split(' ').count(), c, name))
        })
        .collect();
    found.sort_by_key(|(tier, length, _, _)| (*tier, *length));
    found
        .into_iter()
        .take(limit)
        .map(|(_, _, c, name)| Answer {
            title: c.to_string(),
            detail: name.to_string(),
            copy: c.to_string(),
            look: Look::Character,
        })
        .collect()
}

/// Paths under `~/` or `/` that start with what was typed, folders first,
/// or a web address to open.
pub fn places(text: &str, home: &Path) -> Vec<Place> {
    let text = text.trim();
    if text.contains(char::is_whitespace) && !text.starts_with(['/', '~']) {
        return Vec::new();
    }
    if let Some(link) = link(text) {
        return vec![Place {
            title: text.to_string(),
            detail: "Open in the browser".into(),
            target: link,
            completion: None,
            folder: false,
            link: true,
        }];
    }
    let expanded = match text.strip_prefix('~') {
        Some(rest) if rest.is_empty() || rest.starts_with('/') => PathBuf::from(format!("{}{rest}", home.display())),
        _ if text.starts_with('/') => PathBuf::from(text),
        _ => return Vec::new(),
    };
    // "~/Doc" lists ~/ for names starting with "Doc"; "~/Documents/" lists that folder.
    let typed = expanded.to_string_lossy();
    let (dir, prefix) = match typed.rfind('/') {
        Some(at) => (PathBuf::from(&typed[..=at]), typed[at + 1..].to_string()),
        None => return Vec::new(),
    };
    let shown_dir = &text[..text.rfind('/').map_or(text.len(), |at| at + 1)];
    let Ok(entries) = std::fs::read_dir(&dir) else { return Vec::new() };
    let lower = prefix.to_lowercase();
    let mut found: Vec<(bool, String)> = entries
        .flatten()
        .take(4096)
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            let hidden = name.starts_with('.') && !prefix.starts_with('.');
            let folder = entry.file_type().is_ok_and(|t| t.is_dir())
                || (entry.file_type().is_ok_and(|t| t.is_symlink()) && entry.path().is_dir());
            (!hidden && name.to_lowercase().starts_with(&lower)).then_some((folder, name))
        })
        .collect();
    found.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.to_lowercase().cmp(&b.1.to_lowercase())));
    let mut places = Vec::new();
    // The folder typed so far, to open it as it is.
    if prefix.is_empty() && dir.is_dir() {
        places.push(Place {
            title: shown_dir.to_string(),
            detail: "Open folder".into(),
            target: dir.to_string_lossy().into_owned(),
            completion: None,
            folder: true,
            link: false,
        });
    }
    places.extend(found.into_iter().take(8).map(|(folder, name)| {
        let completion = format!("{shown_dir}{name}{}", if folder { "/" } else { "" });
        Place {
            title: format!("{name}{}", if folder { "/" } else { "" }),
            detail: shown_dir.to_string(),
            target: dir.join(&name).to_string_lossy().into_owned(),
            completion: Some(completion),
            folder,
            link: false,
        }
    }));
    places
}

/// A web address: with http(s), or starting with "www.".
fn link(text: &str) -> Option<String> {
    let lower = text.to_lowercase();
    if (lower.starts_with("http://") || lower.starts_with("https://")) && text.len() > 8 {
        return Some(text.to_string());
    }
    (lower.starts_with("www.") && text[4..].contains('.')).then(|| format!("https://{text}"))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test setup and assertions should fail immediately on unexpected errors")]
mod tests {
    use super::*;

    /// Friday 16 October 2026, 12:00 UTC.
    const NOW: i64 = 1_792_152_000;

    fn first(text: &str) -> Option<Answer> {
        answers(text, NOW).into_iter().next()
    }

    #[test]
    fn sums_and_conversions() {
        let sum = first("12*7+3").unwrap();
        assert_eq!((sum.title.as_str(), sum.copy.as_str(), sum.look), ("87", "87", Look::Sum));
        assert_eq!(sum.detail, "12*7+3 =");
        assert_eq!(first("5 km in mi").unwrap().title, "3.106855961 mi");
        assert!(answers("firefox", NOW).is_empty());
        assert!(answers("text editor", NOW).is_empty());
    }

    #[test]
    fn times_elsewhere() {
        if !Path::new("/usr/share/zoneinfo/Asia/Tokyo").exists() {
            return;
        }
        for text in ["time in tokyo", "Tokyo time", "what time is it in Tokyo?", "time in japan"] {
            let answer = first(text).unwrap_or_else(|| panic!("{text}"));
            assert_eq!(answer.title, "21:00", "{text}");
            assert!(answer.detail.contains("JST (UTC+9)"), "{}", answer.detail);
            assert!(answer.detail.starts_with("Friday 16 October"), "{}", answer.detail);
        }
        assert!(first("time in nowhere-at-all").is_none());
    }

    #[test]
    fn days_until_and_since() {
        // Days count from the local date, which is 16 October in every zone at noon UTC.
        let answer = first("days until 2026-12-25").unwrap();
        assert_eq!((answer.title.as_str(), answer.copy.as_str()), ("70 days", "70"));
        assert_eq!(answer.detail, "until Friday 25 December 2026");
        assert_eq!(first("days until 25-12-2026").unwrap().copy, "70");
        assert_eq!(first("days till 25 december").unwrap().copy, "70");
        assert_eq!(first("days until dec 25").unwrap().copy, "70");
        assert_eq!(first("days until 1 january").unwrap().detail, "until Friday 1 January 2027");
        assert_eq!(first("days since 2026-10-15").unwrap().title, "1 day");
        assert_eq!(first("days since 1 november").unwrap().detail, "since Saturday 1 November 2025");
        assert!(first("days until 2026-02-30").is_none());
        assert!(first("days until someday").is_none());
    }

    #[test]
    fn timestamps() {
        let answer = first("1700000000").unwrap();
        assert!(answer.detail.contains("UTC 2023-11-14 22:13:20"), "{}", answer.detail);
        assert!(first("1700000000000").unwrap().detail.contains("milliseconds"));
        assert_eq!(first("unix").unwrap().copy, NOW.to_string());
        let back = first("2023-11-14 22:13:20").unwrap();
        let expected = 1_700_000_000 - zones::local_offset(1_700_000_000);
        assert_eq!(back.copy, expected.to_string());
        assert!(first("2023-11-14 25:00").is_none());
        assert!(first("12345").is_none(), "short numbers are not timestamps");
    }

    #[test]
    fn colour_notations() {
        let rows = answers("#ff8800", NOW);
        let titles: Vec<&str> = rows.iter().map(|a| a.title.as_str()).collect();
        assert_eq!(titles, ["#ff8800", "rgb(255, 136, 0)", "hsl(32, 100%, 50%)"]);
        assert_eq!(rows[0].look, Look::Colour([255, 136, 0]));
        assert_eq!(parse_colour("#f80"), Some([255, 136, 0]));
        assert_eq!(parse_colour("rgb(255, 136, 0)"), Some([255, 136, 0]));
        assert_eq!(parse_colour("rgba(100%, 0%, 0%, 0.5)"), Some([255, 0, 0]));
        assert_eq!(parse_colour("hsl(32, 100%, 50%)"), Some([255, 136, 0]));
        assert_eq!(parse_colour("hsl(240 100% 50%)"), Some([0, 0, 255]));
        assert_eq!(parse_colour("#zzz"), None);
        assert_eq!(parse_colour("rgb(300, 0, 0)"), None);
    }

    #[test]
    fn characters_by_name() {
        let found = characters("thumbs", 8);
        assert_eq!(found.first().map(|a| a.title.as_str()), Some("👍"));
        assert_eq!(characters("tears joy", 8)[0].title, "😂");
        assert_eq!(characters("lol", 8)[0].title, "😂");
        assert_eq!(characters("arrow right", 3)[0].title, "→");
        assert!(characters("alpha", 8).iter().any(|a| a.title == "α"));
        assert_eq!(answers(":fire", NOW)[0].title, "🔥");
        assert_eq!(characters("heart", 1)[0].title, "❤");
        assert!(characters("", 8).is_empty());
        assert!(characters("qqqqqq", 8).is_empty());
    }

    #[test]
    fn commands_after_the_prompt() {
        assert_eq!(command("> uname -a"), Some("uname -a"));
        assert_eq!(command(">"), Some(""));
        assert_eq!(command("uname"), None);
    }

    #[test]
    fn paths_and_links() {
        let root = std::env::temp_dir().join(format!("tileception-places-{}", std::process::id()));
        std::fs::create_dir_all(root.join("Documents")).unwrap();
        std::fs::create_dir_all(root.join("Downloads")).unwrap();
        std::fs::write(root.join("dotfile"), "").unwrap();
        std::fs::write(root.join(".hidden"), "").unwrap();
        let titles = |text: &str| places(text, &root).into_iter().map(|p| p.title).collect::<Vec<_>>();
        assert_eq!(titles("~/"), ["~/", "Documents/", "Downloads/", "dotfile"]);
        assert_eq!(titles("~/do"), ["Documents/", "Downloads/", "dotfile"]);
        assert_eq!(titles("~/.h"), [".hidden"]);
        let doc = &places("~/Docu", &root)[0];
        assert_eq!(doc.completion.as_deref(), Some("~/Documents/"));
        assert_eq!(doc.target, root.join("Documents").to_string_lossy());
        assert!(doc.folder);
        let absolute = format!("{}/Down", root.display());
        assert_eq!(places(&absolute, &root)[0].title, "Downloads/");
        assert!(places("~nobody", &root).is_empty());
        assert!(places("firefox", &root).is_empty());
        let link = &places("https://example.org/a?b", &root)[0];
        assert!(link.link && link.target == "https://example.org/a?b");
        assert_eq!(places("www.example.org", &root)[0].target, "https://www.example.org");
        let _ = std::fs::remove_dir_all(&root);
    }
}
