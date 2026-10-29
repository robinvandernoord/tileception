//! The small card at the bottom of the screen after a volume, brightness or
//! media key: the new level, what is playing, or why nothing happened.

use std::time::Instant;

use smithay::backend::renderer::gles::GlesRenderer;
use tileception_core::Rect;
use tileception_media::Status;

use crate::{
    Tileception,
    controls::{level_bar, value_label, volume_glyph},
    media::OsdKind,
    render::OutputElement,
    ui::{Painter, Style, ease_out, glyph},
};

const FADE_IN: f64 = 0.12;
const FADE_OUT: f64 = 0.25;

impl Tileception {
    /// Whether the card is still (fading) on screen.
    pub fn osd_visible(&self) -> bool {
        self.media.osd.is_some_and(|osd| Instant::now() < osd.until + std::time::Duration::from_secs_f64(FADE_OUT))
    }

    pub fn osd_elements(&self, renderer: &mut GlesRenderer) -> Vec<OutputElement> {
        let Some(osd) = self.media.osd.filter(|_| self.osd_visible()) else { return Vec::new() };
        let theme = &self.theme;
        let snapshot = &self.media.snapshot;
        let now = Instant::now();
        let shown = ease_out(now.duration_since(osd.shown).as_secs_f64() / FADE_IN);
        let leaving = now.saturating_duration_since(osd.until).as_secs_f64() / FADE_OUT;
        let alpha = (shown * (1.0 - leaving).clamp(0.0, 1.0)) as f32;

        let screen = self.output_rect();
        let width: f64 = if osd.kind == OsdKind::Media { 380.0 } else { 320.0 };
        let width = width.min(screen.w - 24.0);
        let height = if osd.kind == OsdKind::Media { 84.0 } else { 68.0 };
        let bottom =
            self.strip_now().map_or(screen.bottom() - 72.0, |strip| (strip.y - 12.0).min(screen.bottom() - 72.0));
        let card = Rect::new(screen.x + (screen.w - width) / 2.0, bottom - height, width, height);
        let mut painter = Painter::new(self, renderer);
        painter.alpha = alpha;
        painter.offset = (0.0, (1.0 - shown) * 12.0);
        painter.popup(card);

        let icon = Rect::new(card.x + 14.0, card.y + (card.h - 40.0) / 2.0, 40.0, 40.0);
        let text_x = icon.right() + 14.0;
        let text_w = card.right() - 16.0 - text_x;
        let title = Rect::new(text_x, card.y + 12.0, text_w - 60.0, 22.0);
        let value = Rect::new(card.right() - 16.0 - 60.0, card.y + 12.0, 60.0, 22.0);
        let bar = Rect::new(text_x, card.y + 38.0, text_w, 16.0);
        let heading = Style::new(theme.font_size, theme.text).bold();

        // Something to say instead of a level.
        let problem = |painter: &mut Painter, message: &str| {
            painter.fill(icon, theme.surface_raised, 20.0);
            painter.glyph(glyph::ALERT, icon, 18, theme.warning);
            painter.text(message, Rect::new(text_x, card.y, text_w, card.h), Style::new(theme.font_size, theme.text));
        };
        let level = |painter: &mut Painter,
                     symbol: &'static str,
                     name: &str,
                     percent: u32,
                     max: u32,
                     muted: bool,
                     boost: bool| {
            let boosted = boost && percent > 100 && !muted;
            painter.fill(icon, if boosted { theme.warning.alpha(0.18) } else { theme.surface_raised }, 20.0);
            painter.glyph(
                symbol,
                icon,
                18,
                if muted {
                    theme.muted
                } else if boosted {
                    theme.warning
                } else {
                    theme.accent
                },
            );
            painter.text(name, title, heading);
            let (label, colour) = value_label(percent, muted, boost, theme);
            let style = Style::new(theme.font_size, colour).bold();
            let (label_width, _) = painter.measure(&label, value.w, style);
            painter.text(&label, Rect::new(value.right() - label_width, value.y, label_width, value.h), style);
            level_bar(painter, bar, percent, max, muted, None);
        };

        match osd.kind {
            OsdKind::Volume if !snapshot.audio.connected => problem(&mut painter, "No sound server"),
            OsdKind::Volume => match &snapshot.audio.output {
                Some(out) => {
                    let max = self.config.file.media.max_volume();
                    let name = if out.percent > 100 && !out.muted { "Volume boosted" } else { "Volume" };
                    level(&mut painter, volume_glyph(out.percent, out.muted), name, out.percent, max, out.muted, true);
                }
                None => problem(&mut painter, "No audio output"),
            },
            OsdKind::Mic => match &snapshot.audio.input {
                Some(mic) => {
                    let symbol = if mic.muted { glyph::MIC_OFF } else { glyph::MIC };
                    level(&mut painter, symbol, "Microphone", mic.percent, 100, mic.muted, false);
                }
                None => problem(&mut painter, "No microphone"),
            },
            OsdKind::Brightness => match &snapshot.backlight {
                Some(backlight) => level(&mut painter, glyph::SUN, "Brightness", backlight.percent, 100, false, false),
                None => problem(&mut painter, "No adjustable backlight"),
            },
            OsdKind::Media => match snapshot.media.active() {
                Some(player) => {
                    let art = Rect::new(card.x + 12.0, card.y + 12.0, 60.0, 60.0);
                    self.draw_art(&mut painter, art, player);
                    let info_x = art.right() + 14.0;
                    let info_width = card.right() - 16.0 - 36.0 - info_x;
                    let title = if player.title.is_empty() { player.identity.as_str() } else { player.title.as_str() };
                    painter.text(title, Rect::new(info_x, card.y + 12.0, info_width, 22.0), heading);
                    if !player.artist.is_empty() {
                        painter.text(
                            &player.artist,
                            Rect::new(info_x, card.y + 34.0, info_width, 20.0),
                            Style::new(theme.font_size * 0.92, theme.muted),
                        );
                    }
                    painter.text(
                        &player.identity,
                        Rect::new(info_x, card.y + 54.0, info_width, 18.0),
                        Style::new(theme.font_size * 0.85, theme.muted),
                    );
                    let status = Rect::new(card.right() - 16.0 - 32.0, card.y + (card.h - 32.0) / 2.0, 32.0, 32.0);
                    let symbol = match player.status {
                        Status::Playing => glyph::PLAY,
                        Status::Paused => glyph::PAUSE,
                        Status::Stopped => glyph::STOP,
                    };
                    painter.fill(status, theme.surface_raised, 16.0);
                    painter.glyph(
                        symbol,
                        status,
                        14,
                        if player.status == Status::Playing { theme.accent } else { theme.muted },
                    );
                }
                None if !snapshot.media.connected => problem(&mut painter, "Media players unavailable"),
                None => problem(&mut painter, "No media player running"),
            },
        }
        painter.finish()
    }
}
