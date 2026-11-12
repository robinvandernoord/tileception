//! Plugins' readings on the lock screen, such as the smart home's indoor
//! temperature: read-only pills in the clock tile, only while a session is
//! locked and only when the lock screen shows anything of the session.

use super::{Greeter, settings::Lock};
use crate::{Tileception, panel::block::Reading, ui::glyph};

/// A reading as the clock tile draws it: its symbol and label.
pub(super) type Pill = (&'static str, String);

impl Tileception {
    /// The readings of plugins with a `[lock]` block, for the clock tile.
    pub(super) fn lock_readings(&self, greeter: &Greeter) -> Vec<Pill> {
        let Some(plugins) = &self.plugins else { return Vec::new() };
        shown(greeter.locked(), &greeter.file.lock, plugins.lock_readings())
    }
}

/// What the lock screen shows of `readings`: nothing on the login screen,
/// and nothing while the lock screen keeps the session to itself.
fn shown(locked: bool, privacy: &Lock, readings: Vec<&Reading>) -> Vec<Pill> {
    if !locked || !privacy.shows_session() {
        return Vec::new();
    }
    readings
        .into_iter()
        .map(|reading| (reading.icon.as_deref().and_then(glyph::named).unwrap_or(glyph::PLUG), reading.label()))
        .filter(|(_, label)| !label.is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn readings_follow_the_lock_screens_privacy() {
        let indoors =
            Reading { title: "Indoors".into(), value: "21\u{b0}".into(), icon: Some("glyph:thermometer".into()) };
        let blank = Reading::default();
        let readings = || vec![&indoors, &blank];
        let open = Lock { show_media: false, show_notification_count: true };
        assert_eq!(shown(true, &open, readings()), [(glyph::THERMOMETER, "Indoors 21\u{b0}".to_string())]);
        let media = Lock { show_media: true, show_notification_count: false };
        assert_eq!(shown(true, &media, readings()).len(), 1);
        // A lock screen that shows nothing of the session shows no readings.
        let closed = Lock { show_media: false, show_notification_count: false };
        assert!(shown(true, &closed, readings()).is_empty());
        // Never on the login screen.
        assert!(shown(false, &open, readings()).is_empty());
    }
}
