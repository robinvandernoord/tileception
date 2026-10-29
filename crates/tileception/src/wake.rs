//! Suspending until a set time: the hardware clock (RTC) wakes the computer.
//! Setting its alarm needs root, so the system unit `tileception-wake@.service`
//! does it, which a polkit rule lets the person at the computer start. Times
//! are read loosely: "7:30", "0730", "7.30", "7h30", "7am", "23", "+8h",
//! "in 90 min".

use smithay::reexports::calloop::channel;

use crate::Tileception;

/// The alarm file the kernel offers for the first hardware clock.
const ALARM: &str = "/sys/class/rtc/rtc0/wakealarm";
const UNIT: &str = "tileception-wake@.service";
const UNIT_DIRS: [&str; 3] = ["/etc/systemd/system", "/usr/lib/systemd/system", "/lib/systemd/system"];

/// The furthest ahead a wake-up may be. Many hardware clocks can only wake
/// the computer within a day.
pub const MAX_AHEAD: i64 = 24 * 60 * 60;

/// Whether suspending until a time can work here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Support {
    Ready,
    NoClock,
    NotInstalled,
}

pub fn support() -> Support {
    if !std::path::Path::new(ALARM).exists() {
        Support::NoClock
    } else if !UNIT_DIRS.iter().any(|dir| std::path::Path::new(dir).join(UNIT).exists()) {
        Support::NotInstalled
    } else {
        Support::Ready
    }
}

/// A wake-up time as typed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wake {
    /// The next time the clock shows this.
    Clock { hour: u32, minute: u32 },
    /// This many minutes from now.
    After(u32),
}

/// Reads a typed time. Without `+` or "in" it is a time of day, with them a
/// duration; a number without a unit there counts hours up to 24 and minutes
/// above that.
pub fn parse(text: &str) -> Option<Wake> {
    let text = text.trim().to_ascii_lowercase();
    let text = text.strip_prefix("tomorrow").unwrap_or(&text).trim_start();
    let text = text.strip_prefix("at ").unwrap_or(text).trim();
    if let Some(rest) = text.strip_prefix('+').or_else(|| text.strip_prefix("in ")) {
        return duration(rest.trim()).map(Wake::After);
    }
    if text.contains(|character: char| character.is_ascii_alphabetic())
        && ["min", "hour", "hr"].iter().any(|unit| text.contains(unit))
    {
        return duration(text).map(Wake::After);
    }
    clock(text).map(|(hour, minute)| Wake::Clock { hour, minute })
}

/// "7:30", "0730", "7.30", "7h30", "7 30", "7u30", "7am", "7:30 p.m.", "24:00".
fn clock(text: &str) -> Option<(u32, u32)> {
    let compact: String = text.chars().filter(|character| !matches!(character, '.' | ' ')).collect();
    let (text, half) = if let Some(rest) = compact.strip_suffix("am").or_else(|| compact.strip_suffix('a')) {
        (rest.to_string(), Some(false))
    } else if let Some(rest) = compact.strip_suffix("pm").or_else(|| compact.strip_suffix('p')) {
        (rest.to_string(), Some(true))
    } else {
        // The separators, put back where a dot or space parted the numbers.
        (text.replace(['.', ' ', 'h', 'u', ','], ":"), None)
    };
    let text = text.trim_end_matches(['h', 'u', ':']);
    let parts: Vec<&str> = text.split([':', 'h', 'u']).filter(|part| !part.is_empty()).collect();
    if parts.iter().any(|part| !part.bytes().all(|byte| byte.is_ascii_digit())) {
        return None;
    }
    let (hour, minute) = match parts.as_slice() {
        [both] if both.len() <= 2 => (both.parse().ok()?, 0),
        [both] if both.len() <= 4 => {
            let split = both.len() - 2;
            (both[..split].parse().ok()?, both[split..].parse().ok()?)
        }
        [hour, minute] if hour.len() <= 2 && minute.len() == 2 => (hour.parse().ok()?, minute.parse().ok()?),
        _ => return None,
    };
    if minute > 59 {
        return None;
    }
    let hour = match half {
        Some(_) if !(1..=12).contains(&hour) => return None,
        Some(after_noon) => hour % 12 + if after_noon { 12 } else { 0 },
        None if hour == 24 && minute == 0 => 0,
        None if hour > 23 => return None,
        None => hour,
    };
    Some((hour, minute))
}

/// "8h", "8h30", "8h30m", "8 hours 30 minutes", "1.5h", "1:30", "90m", "8".
fn duration(text: &str) -> Option<u32> {
    if let Some((hours, minutes)) = text.split_once(':') {
        let (hours, minutes): (u32, u32) = (hours.trim().parse().ok()?, minutes.trim().parse().ok()?);
        return (minutes < 60).then_some(hours * 60 + minutes).filter(|total| *total > 0);
    }
    let mut total = 0.0;
    let mut rest = text.trim();
    let mut after_hours = false;
    let mut any = false;
    while !rest.is_empty() {
        let end = rest.find(|character: char| !(character.is_ascii_digit() || character == '.')).unwrap_or(rest.len());
        let number: f64 = rest[..end].parse().ok()?;
        rest = rest[end..].trim_start();
        let unit_end = rest.find(|character: char| !character.is_ascii_alphabetic()).unwrap_or(rest.len());
        let unit = &rest[..unit_end];
        rest = rest[unit_end..].trim_start_matches([' ', ',']).trim_start_matches("and ").trim_start();
        let minutes = match unit {
            "h" | "hr" | "hrs" | "hour" | "hours" | "u" | "uur" => number * 60.0,
            "m" | "min" | "mins" | "minute" | "minutes" => number,
            // "8h30": the number after the hours is minutes.
            "" if after_hours => number,
            "" if number <= 24.0 && rest.is_empty() && !any => number * 60.0,
            "" if rest.is_empty() && !any => number,
            _ => return None,
        };
        after_hours = unit.starts_with(['h', 'u']);
        total += minutes;
        any = true;
    }
    let total = total.round();
    (any && total >= 1.0 && total <= u32::MAX as f64).then_some(total as u32)
}

/// When `wake` comes, in seconds since 1970, from `now`. Daylight saving
/// time is the local clock's.
pub fn resolve(wake: Wake, now: i64) -> Option<i64> {
    let when = match wake {
        Wake::After(minutes) => now + i64::from(minutes) * 60,
        Wake::Clock { hour, minute } => {
            let today = at_clock(now, hour, minute, 0)?;
            if today > now + 30 { today } else { at_clock(now, hour, minute, 1)? }
        }
    };
    (when - now <= MAX_AHEAD).then_some(when)
}

/// `hour`:`minute` on the local day `days` after `now`'s.
#[allow(unsafe_code, reason = "local time comes from libc's localtime_r and mktime")]
#[allow(clippy::useless_conversion, reason = "time_t is narrower than i64 on some targets")]
fn at_clock(now: i64, hour: u32, minute: u32, days: i32) -> Option<i64> {
    let mut datetime = local(now)?;
    datetime.tm_mday += days;
    datetime.tm_hour = hour as i32;
    datetime.tm_min = minute as i32;
    datetime.tm_sec = 0;
    datetime.tm_isdst = -1;
    // SAFETY: mktime reads and normalises the initialised `datetime` it is lent.
    let when = unsafe { libc::mktime(&mut datetime) };
    (when != -1).then_some(i64::from(when))
}

#[allow(unsafe_code, reason = "local time comes from libc's localtime_r")]
fn local(timestamp: i64) -> Option<libc::tm> {
    let mut datetime = std::mem::MaybeUninit::<libc::tm>::uninit();
    // SAFETY: localtime_r reads `timestamp` and fills the caller-owned `datetime`.
    if unsafe { libc::localtime_r(&(timestamp as libc::time_t), datetime.as_mut_ptr()) }.is_null() {
        return None;
    }
    // SAFETY: localtime_r succeeded, so `datetime` is initialised.
    Some(unsafe { datetime.assume_init() })
}

pub fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |since| since.as_secs() as i64)
}

/// The local hour and minute at `timestamp`.
pub fn clock_at(timestamp: i64) -> Option<(u32, u32)> {
    local(timestamp).map(|datetime| (datetime.tm_hour as u32, datetime.tm_min as u32))
}

/// "wakes tomorrow at 07:30, in 8 h 15 min".
pub fn describe(when: i64, now: i64) -> String {
    let (Some(then), Some(today)) = (local(when), local(now)) else { return String::new() };
    let day = if (then.tm_year, then.tm_yday) == (today.tm_year, today.tm_yday) { "today" } else { "tomorrow" };
    let minutes = (when - now + 59) / 60;
    let left = match (minutes / 60, minutes % 60) {
        (0, minutes) => format!("{minutes} min"),
        (hours, 0) => format!("{hours} h"),
        (hours, minutes) => format!("{hours} h {minutes} min"),
    };
    format!("wakes {day} at {:02}:{:02}, in {left}", then.tm_hour, then.tm_min)
}

/// What typing in the field has made: ASCII only, so the menu stays `Copy`.
#[derive(Debug, Clone, Copy)]
pub struct Typed {
    bytes: [u8; 24],
    len: u8,
}

impl Typed {
    pub fn new(text: &str) -> Self {
        let mut typed = Typed { bytes: [0; 24], len: 0 };
        text.chars().for_each(|character| typed.push(character));
        typed
    }

    pub fn as_str(&self) -> &str {
        std::str::from_utf8(&self.bytes[..usize::from(self.len)]).unwrap_or_default()
    }

    pub fn push(&mut self, character: char) {
        if (character.is_ascii_graphic() || character == ' ')
            && let Some(slot) = self.bytes.get_mut(usize::from(self.len))
        {
            *slot = character as u8;
            self.len += 1;
        }
    }

    pub fn pop(&mut self) {
        self.len = self.len.saturating_sub(1);
    }
}

impl Tileception {
    /// Sets the alarm through the system unit, off the event loop, then
    /// suspends, or reports the failure through `done`.
    pub fn arm_wake(&mut self, when: i64, done: impl FnOnce(&mut Tileception, Result<(), String>) + 'static) {
        let (sender, receiver) = channel::channel::<Result<(), String>>();
        let mut done = Some(done);
        let registered = self.loop_handle.insert_source(receiver, move |event, _, state| {
            let result = match event {
                channel::Event::Msg(result) => result,
                // The thread ended without a word.
                channel::Event::Closed => Err("the alarm was not set".into()),
            };
            if let Some(done) = done.take() {
                done(state, result);
            }
        });
        if let Err(error) = registered {
            return tracing::error!("could not wait for the wake-up alarm: {error}");
        }
        let unit = format!("tileception-wake@{when}.service");
        let started = std::thread::Builder::new().name("tileception-wake".into()).spawn(move || {
            let result = std::process::Command::new("systemctl")
                .args(["start", "--no-ask-password", &unit])
                .stdin(std::process::Stdio::null())
                .output()
                .map_err(|error| error.to_string())
                .and_then(|output| {
                    if output.status.success() {
                        Ok(())
                    } else {
                        Err(String::from_utf8_lossy(&output.stderr).trim().to_string())
                    }
                });
            let _ = sender.send(result);
        });
        if let Err(error) = started {
            tracing::error!("could not set the wake-up alarm: {error}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clock_time(hour: u32, minute: u32) -> Option<Wake> {
        Some(Wake::Clock { hour, minute })
    }

    #[test]
    fn reads_times_of_day() {
        for text in ["23:00", "2300", "23", "23.00", "23h", "23h00", "23u", "11pm", "11 PM", "11 p.m.", "at 23:00"] {
            assert_eq!(parse(text), clock_time(23, 0), "{text}");
        }
        for text in ["7:30", "730", "0730", "7.30", "7h30", "7 30", "7:30am", "7.30 a.m.", "tomorrow 7:30"] {
            assert_eq!(parse(text), clock_time(7, 30), "{text}");
        }
        assert_eq!(parse("12am"), clock_time(0, 0));
        assert_eq!(parse("12pm"), clock_time(12, 0));
        assert_eq!(parse("24:00"), clock_time(0, 0));
        assert_eq!(parse("0"), clock_time(0, 0));
    }

    #[test]
    fn reads_durations() {
        assert_eq!(parse("+8h"), Some(Wake::After(480)));
        assert_eq!(parse("+8"), Some(Wake::After(480)));
        assert_eq!(parse("+90"), Some(Wake::After(90)));
        assert_eq!(parse("+1:30"), Some(Wake::After(90)));
        assert_eq!(parse("+1.5h"), Some(Wake::After(90)));
        assert_eq!(parse("+8h30"), Some(Wake::After(510)));
        assert_eq!(parse("in 8h30m"), Some(Wake::After(510)));
        assert_eq!(parse("in 90 min"), Some(Wake::After(90)));
        assert_eq!(parse("8 hours and 30 minutes"), Some(Wake::After(510)));
        assert_eq!(parse("45 minutes"), Some(Wake::After(45)));
    }

    #[test]
    fn rejects_nonsense() {
        for text in ["", "25:00", "7:60", "13pm", "0am", "12345", "soon", "+", "+0", "+8x", "7:3"] {
            assert_eq!(parse(text), None, "{text}");
        }
    }

    #[test]
    fn resolves_within_a_day() {
        let now = 1_800_000_000;
        let (hour, minute) = clock_at(now).expect("local time");
        let same = resolve(Wake::Clock { hour, minute }, now).expect("tomorrow");
        assert!(same > now + 23 * 3600 && same <= now + MAX_AHEAD, "{same}");
        assert_eq!(resolve(Wake::After(90), now), Some(now + 5400));
        assert_eq!(resolve(Wake::After(25 * 60), now), None);
    }

    #[test]
    fn keeps_only_ascii() {
        let mut typed = Typed::new("7:30");
        typed.push('\u{e9}');
        typed.push('p');
        assert_eq!(typed.as_str(), "7:30p");
        typed.pop();
        assert_eq!(typed.as_str(), "7:30");
    }
}
