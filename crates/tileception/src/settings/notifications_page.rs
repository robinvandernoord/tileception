//! The Notifications page: the sounds of the Sound mode, the times the mode
//! menu offers and sounds for single apps, as rows, then a card for each
//! family of sounds with buttons to hear them. A preview plays at the
//! volume on the page, whatever the mode, and a second click stops it.

use std::time::{Duration, Instant};

use smithay::reexports::calloop::timer::{TimeoutAction, Timer};

use super::*;
use crate::sound::{Clip, Family};

/// The page's title, as it appears in [`CATEGORIES`].
pub(super) const TITLE: &str = "Notifications";

/// The page's place in [`CATEGORIES`], which its fields name.
pub(super) const PAGE: usize = 12;

const CLIPS: [(Clip, &str); 4] =
    [(Clip::Low, "Low"), (Clip::Normal, "Normal"), (Clip::Critical, "Critical"), (Clip::Alarm, "Alarm")];

const HEADING_H: f64 = 26.;
const CARD_H: f64 = 86.;
const CARD_GAP: f64 = 10.;
const BUTTON_H: f64 = 32.;

/// The family cards below the page's rows.
pub(super) struct SoundsFrame {
    pub heading: Rect,
    pub cards: Vec<(Family, Rect)>,
    pub buttons: Vec<(Rect, Hit)>,
    pub height: f64,
}

impl SoundsFrame {
    pub(super) fn shift(&mut self, amount: f64) {
        self.heading.y += amount;
        for (_, rect) in &mut self.cards {
            rect.y += amount;
        }
        for (rect, _) in &mut self.buttons {
            rect.y += amount;
        }
    }
}

impl Tileception {
    pub(super) fn settings_sounds_layout(&self, left: f64, top: f64, width: f64) -> SoundsFrame {
        let heading = Rect::new(left, top, width, HEADING_H);
        let mut cursor = heading.bottom();
        let mut cards = Vec::new();
        let mut buttons = Vec::new();
        for family in Family::ALL {
            let card = Rect::new(left, cursor, width, CARD_H);
            let gap = 8.;
            let each = (width - 32. - gap * (CLIPS.len() as f64 - 1.)) / CLIPS.len() as f64;
            for (index, (clip, _)) in CLIPS.iter().enumerate() {
                let rect =
                    Rect::new(left + 16. + index as f64 * (each + gap), card.bottom() - 16. - BUTTON_H, each, BUTTON_H);
                buttons.push((rect, Hit::SoundPreview(family, *clip)));
            }
            cards.push((family, card));
            cursor = card.bottom() + CARD_GAP;
        }
        SoundsFrame { heading, cards, buttons, height: cursor - CARD_GAP - top }
    }

    /// The volume on the page, from 0 to 1, as the draft has it.
    fn settings_sound_volume(&self) -> f32 {
        let index = FIELDS.iter().position(|field| field.key == "notifications.volume");
        let value = index
            .and_then(|index| self.settings.draft[index].clone().or_else(|| self.settings.defaults[index].clone()));
        let percent = match value {
            Some(toml::Value::Integer(number)) => number as f64,
            Some(toml::Value::Float(number)) => number,
            _ => f64::from(self.config.file.notifications.volume),
        };
        (percent.clamp(0., 100.) / 100.) as f32
    }

    /// The family the draft has chosen.
    fn settings_sound_family(&self) -> Family {
        let index = FIELDS.iter().position(|field| field.key == "notifications.sounds");
        match index.and_then(|index| self.settings.draft[index].clone()) {
            Some(toml::Value::String(name)) => {
                Family::ALL.into_iter().find(|family| family.name().eq_ignore_ascii_case(&name)).unwrap_or_default()
            }
            _ => self.config.file.notifications.sounds,
        }
    }

    /// Whether this sound's preview is playing.
    pub(super) fn settings_previewing(&self, family: Family, clip: Clip) -> bool {
        self.settings.sound_preview.is_some_and(|(playing_family, playing_clip, started)| {
            (playing_family, playing_clip) == (family, clip) && started.elapsed() < crate::sound::length(family, clip)
        })
    }

    /// Plays the sound, or stops it when it is playing.
    pub(super) fn settings_preview_sound(&mut self, family: Family, clip: Clip) {
        if self.settings_previewing(family, clip) {
            self.notifications.sounds.stop_preview();
            self.settings.sound_preview = None;
            return;
        }
        let volume = self.settings_sound_volume();
        self.notifications.sounds.preview(family, clip, volume);
        self.settings.sound_preview = Some((family, clip, Instant::now()));
        // The button turns back from stop to play when the sound ends.
        let length = crate::sound::length(family, clip) + Duration::from_millis(50);
        let timer = self.loop_handle.insert_source(Timer::from_duration(length), |_, _, state| {
            state.schedule_render();
            TimeoutAction::Drop
        });
        if let Err(error) = timer {
            tracing::warn!("could not wait for the preview's end: {error}");
        }
    }

    /// The heading and the cards; their buttons are drawn with the page's hits.
    pub(super) fn settings_draw_sounds(&self, painter: &mut Painter<'_>, frame: &SoundsFrame) {
        let theme = &self.theme;
        // As the page's group headings are drawn.
        let heading = Rect::new(frame.heading.x + 2., frame.heading.bottom() - 24., frame.heading.w, 20.);
        painter.text("SOUND PREVIEWS", heading, Style::new(10., theme.muted).weight(600));
        let chosen = self.settings_sound_family();
        for (family, card) in &frame.cards {
            painter.shape(*card, Shape { border_width: 1., border: theme.border, ..Shape::filled(theme.surface, 8.) });
            let (name_w, _) = painter.text(
                family.name(),
                Rect::new(card.x + 16., card.y + 12., card.w - 32., 22.),
                Style::new(13., theme.text).weight(600),
            );
            if *family == chosen {
                let tag = Rect::new(card.x + 16. + name_w + 10., card.y + 15., 56., 18.);
                painter.shape(tag, Shape::filled(theme.accent.alpha(0.18), 9.));
                painter.text("In Use", tag, Style::new(10.5, theme.accent).align(Align::Centre));
            }
        }
    }
}

/// A preview button's label.
pub(super) fn clip_label(clip: Clip) -> &'static str {
    CLIPS.iter().find(|(each, _)| *each == clip).map_or("", |(_, label)| label)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_page_is_where_its_fields_say() {
        assert_eq!(CATEGORIES[PAGE].title, TITLE);
    }
}
