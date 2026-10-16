//! The small card at the bottom of the screen after a volume, brightness or
//! media key: the new level, what is playing, or why nothing happened.

use std::time::Instant;

use smithay::backend::renderer::gles::GlesRenderer;
use tileception_core::Rect;
use tileception_media::Status;

use crate::{
    controls::{level_bar, value_label, volume_glyph},
    media::OsdKind,
    render::OutputElement,
    ui::{ease_out, glyph, Painter, Style},
    Tileception,
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
        let t = &self.theme;
        let s = &self.media.snapshot;
        let now = Instant::now();
        let shown = ease_out(now.duration_since(osd.shown).as_secs_f64() / FADE_IN);
        let leaving = now.saturating_duration_since(osd.until).as_secs_f64() / FADE_OUT;
        let alpha = (shown * (1.0 - leaving).clamp(0.0, 1.0)) as f32;

        let screen = self.output_rect();
        let w: f64 = if osd.kind == OsdKind::Media { 380.0 } else { 320.0 };
        let w = w.min(screen.w - 24.0);
        let h = if osd.kind == OsdKind::Media { 84.0 } else { 68.0 };
        let card = Rect::new(screen.x + (screen.w - w) / 2.0, screen.bottom() - h - 72.0, w, h);
        let mut p = Painter::new(self, renderer);
        p.alpha = alpha;
        p.offset = (0.0, (1.0 - shown) * 12.0);
        p.popup(card);

        let icon = Rect::new(card.x + 14.0, card.y + (card.h - 40.0) / 2.0, 40.0, 40.0);
        let text_x = icon.right() + 14.0;
        let text_w = card.right() - 16.0 - text_x;
        let title = Rect::new(text_x, card.y + 12.0, text_w - 60.0, 22.0);
        let value = Rect::new(card.right() - 16.0 - 60.0, card.y + 12.0, 60.0, 22.0);
        let bar = Rect::new(text_x, card.y + 38.0, text_w, 16.0);
        let heading = Style::new(t.font_size, t.text).bold();

        // Something to say instead of a level.
        let problem = |p: &mut Painter, message: &str| {
            p.fill(icon, t.surface_raised, 20.0);
            p.glyph(glyph::ALERT, icon, 18, t.warning);
            p.text(message, Rect::new(text_x, card.y, text_w, card.h), Style::new(t.font_size, t.text));
        };
        let level = |p: &mut Painter, g: &'static str, name: &str, v: u32, max: u32, muted: bool, boost: bool| {
            let boosted = boost && v > 100 && !muted;
            p.fill(icon, if boosted { t.warning.alpha(0.18) } else { t.surface_raised }, 20.0);
            p.glyph(
                g,
                icon,
                18,
                if muted {
                    t.muted
                } else if boosted {
                    t.warning
                } else {
                    t.accent
                },
            );
            p.text(name, title, heading);
            let (label, colour) = value_label(v, muted, boost, t);
            let style = Style::new(t.font_size, colour).bold();
            let (lw, _) = p.measure(&label, value.w, style);
            p.text(&label, Rect::new(value.right() - lw, value.y, lw, value.h), style);
            level_bar(p, bar, v, max, muted, None);
        };

        match osd.kind {
            OsdKind::Volume if !s.audio.connected => problem(&mut p, "No sound server"),
            OsdKind::Volume => match &s.audio.output {
                Some(out) => {
                    let max = self.config.file.media.max_volume();
                    let name = if out.percent > 100 && !out.muted { "Volume boosted" } else { "Volume" };
                    level(&mut p, volume_glyph(out.percent, out.muted), name, out.percent, max, out.muted, true);
                }
                None => problem(&mut p, "No audio output"),
            },
            OsdKind::Mic => match &s.audio.input {
                Some(mic) => {
                    let g = if mic.muted { glyph::MIC_OFF } else { glyph::MIC };
                    level(&mut p, g, "Microphone", mic.percent, 100, mic.muted, false);
                }
                None => problem(&mut p, "No microphone"),
            },
            OsdKind::Brightness => match &s.backlight {
                Some(b) => level(&mut p, glyph::SUN, "Brightness", b.percent, 100, false, false),
                None => problem(&mut p, "No adjustable backlight"),
            },
            OsdKind::Media => match s.media.active() {
                Some(player) => {
                    let art = Rect::new(card.x + 12.0, card.y + 12.0, 60.0, 60.0);
                    self.draw_art(&mut p, art, player);
                    let x = art.right() + 14.0;
                    let w = card.right() - 16.0 - 36.0 - x;
                    let title = if player.title.is_empty() { player.identity.as_str() } else { player.title.as_str() };
                    p.text(title, Rect::new(x, card.y + 12.0, w, 22.0), heading);
                    if !player.artist.is_empty() {
                        p.text(
                            &player.artist,
                            Rect::new(x, card.y + 34.0, w, 20.0),
                            Style::new(t.font_size * 0.92, t.muted),
                        );
                    }
                    p.text(
                        &player.identity,
                        Rect::new(x, card.y + 54.0, w, 18.0),
                        Style::new(t.font_size * 0.8, t.muted.alpha(0.8)),
                    );
                    let status = Rect::new(card.right() - 16.0 - 32.0, card.y + (card.h - 32.0) / 2.0, 32.0, 32.0);
                    let g = match player.status {
                        Status::Playing => glyph::PLAY,
                        Status::Paused => glyph::PAUSE,
                        Status::Stopped => glyph::STOP,
                    };
                    p.fill(status, t.surface_raised, 16.0);
                    p.glyph(g, status, 14, if player.status == Status::Playing { t.accent } else { t.muted });
                }
                None if !s.media.connected => problem(&mut p, "Media players unavailable"),
                None => problem(&mut p, "No media player running"),
            },
        }
        p.finish()
    }
}
