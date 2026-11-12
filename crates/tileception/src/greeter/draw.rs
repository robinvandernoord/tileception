//! How the greeter looks, in the desktop's Tide style. One [`Scene`] says
//! where everything goes, for drawing and for clicks alike.

use smithay::backend::renderer::{element::memory::MemoryRenderBuffer, gles::GlesRenderer};
use tileception_core::{Point, Rect};

use super::{Action, Auth, Greeter, access::Aid, settings::Panel, settings::WallpaperFallback};
use crate::{
    Tileception,
    render::OutputElement,
    shapes::Shape,
    theme::Colour,
    ui::{Align, Painter, Style, ease_out, glyph},
};

/// The bar along the top.
pub const BAR_H: f64 = 34.0;
const TAB_H: f64 = 30.0;
/// The narrowest a person's tab gets before the strip scrolls.
const TAB_MIN: f64 = 120.0;
const TAB_ARROW: f64 = 24.0;
const ROW_H: f64 = 38.0;
const PILL_H: f64 = 26.0;
const AVATAR: f64 = 92.0;
const FIELD_H: f64 = 48.0;
const PALETTE_ROWS: usize = 8;
/// With people as tiles, how much wider the selected person's tile is than
/// the others, and the narrowest each may get before fewer people are shown.
const CARD_GROW: f64 = 2.8;
const CARD_MIN: f64 = 96.0;
const CARD_SELECTED_MIN: f64 = 300.0;
/// How long the field shakes after a wrong password, in seconds.
const SHAKE: f64 = 0.45;
/// How long a change of person takes to crossfade, in seconds.
const CROSSFADE: f64 = 0.3;
/// Tileception's logo, made by `scripts/session-badge.sh`; shown for its
/// sessions when no other login screen's badge is installed.
const TILECEPTION_BADGE: &[u8] = include_bytes!("../../../../resources/session-badge.svg");

/// What a click on a rectangle does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    Person(usize),
    Session(usize),
    Submit,
    Run(Action),
    /// A switch of the accessibility panel, by its row.
    Aid(usize),
    Launcher,
    /// The keyboard layout in the bar, when there are several.
    Layout,
    Panel(Panel),
    /// A launcher row, by position.
    Action(usize),
    /// The launcher itself, around its rows.
    Palette,
}

struct LoginLayout {
    avatar: Rect,
    name: Rect,
    sub: Rect,
    field: Rect,
    submit: Rect,
    hint: Rect,
}

struct PaletteLayout {
    panel: Rect,
    field: Rect,
    rows: Vec<Rect>,
}

struct Scene {
    screen: Rect,
    bar: Rect,
    launcher_button: Rect,
    /// The bar's battery and keyboard layout, each with its label.
    battery: Option<(Rect, String)>,
    keyboard: Option<(Rect, String)>,
    panels: Vec<(Panel, Rect)>,
    tabs: Vec<(usize, Rect)>,
    /// Arrows at the ends of a tab strip too long to show at once, each
    /// with the person it moves to (none at the end) and whether it points left.
    tab_arrows: Vec<(Rect, Option<usize>, bool)>,
    /// With people as tiles: each shown person's tile, by person.
    cards: Vec<(usize, Rect)>,
    login: Option<LoginLayout>,
    /// Session rows, by session.
    rows: Vec<(usize, Rect)>,
    power: Vec<(Action, Rect)>,
    /// The accessibility panel's switches, by row.
    aids: Vec<(usize, Rect)>,
    /// Locked: the one tab, saying whose session it is.
    locked_tab: Option<Rect>,
    /// Locked: what happened while away, and play or pause.
    away: Option<Rect>,
    media: Option<Rect>,
    palette: Option<PaletteLayout>,
}

fn panel_rect(panels: &[(Panel, Rect)], panel: Panel) -> Option<Rect> {
    panels.iter().find(|(candidate, _)| *candidate == panel).map(|(_, rect)| *rect)
}

/// Where a panel's content goes, under its title.
fn body(rect: Rect) -> Rect {
    Rect::new(rect.x + 14.0, rect.y + 40.0, (rect.w - 28.0).max(0.0), (rect.h - 54.0).max(0.0))
}

impl Tileception {
    fn greeter_scene(&self) -> Scene {
        let screen = self.output_rect();
        let bar = Rect::new(screen.x, screen.y, screen.w, BAR_H);
        let launcher_button = Rect::new(bar.right() - 40.0, bar.y + 3.0, 32.0, BAR_H - 6.0);
        let panels = self.greeter_panels();
        // Right to left from the launcher: battery, then keyboard layout.
        let style = Style::new(self.theme.font_size * 0.9, self.theme.muted);
        let mut right = launcher_button.x - 8.0;
        let battery = self.battery.reading.as_ref().map(|reading| {
            let label = format!("{}%", reading.percent);
            let width = self.measure_text(&label, style, 300.0).0 + 8.0;
            right -= width + 4.0;
            (Rect::new(right, bar.y + 3.0, width, BAR_H - 6.0), label)
        });
        let keyboard = self.greeter.as_deref().and_then(|greeter| greeter.active_layout()).map(|(layout, variant)| {
            let label = super::keymap::short(layout, variant);
            let width = 22.0 + self.measure_text(&label, style, 300.0).0 + 10.0;
            right -= width + 4.0;
            (Rect::new(right, bar.y + 3.0, width, BAR_H - 6.0), label)
        });
        let mut scene = Scene {
            screen,
            bar,
            launcher_button,
            battery,
            keyboard,
            panels: panels.clone(),
            tabs: Vec::new(),
            tab_arrows: Vec::new(),
            cards: Vec::new(),
            login: None,
            rows: Vec::new(),
            power: Vec::new(),
            aids: Vec::new(),
            locked_tab: None,
            away: None,
            media: None,
            palette: None,
        };
        let Some(greeter) = self.greeter.as_deref() else { return scene };

        if let Some(login) = panel_rect(&panels, Panel::Login) {
            // One person needs no tabs; someone not listed is in the launcher then.
            let mut top = login.y + 14.0;
            // Where the name and field go: the panel, or the selected person's tile.
            let mut area = login;
            if greeter.locked() {
                scene.locked_tab = Some(Rect::new(login.x + 6.0, login.y + 6.0, (login.w - 12.0).min(260.0), TAB_H));
                top = login.y + 6.0 + TAB_H + 10.0;
            } else if greeter.cards() {
                let gap = self.config.file.general.gap.clamp(4.0, 32.0);
                for (person, left, width) in card_row(login.w, gap, greeter.people(), greeter.user) {
                    let card = Rect::new(login.x + left, login.y, width, login.h);
                    if person == greeter.user {
                        area = card;
                    }
                    scene.cards.push((person, card));
                }
            } else if greeter.accounts.len() > 1 {
                let strip = Rect::new(login.x + 6.0, login.y + 6.0, login.w - 12.0, TAB_H);
                let plus = 34.0;
                let account_count = greeter.accounts.len();
                let gap = 3.0;
                let room = strip.w - plus - gap;
                let fits = ((room + gap) / (TAB_MIN + gap)).floor().max(1.0) as usize;
                let (mut tab_left, mut room) = (strip.x, room);
                let shown = if account_count > fits {
                    // Arrows at both ends, and as many tabs as fit between them.
                    room -= 2.0 * (TAB_ARROW + gap);
                    let fits = ((room + gap) / (TAB_MIN + gap)).floor().max(1.0) as usize;
                    let shown = tab_window(account_count, greeter.user, fits);
                    let left = Rect::new(tab_left, strip.y, TAB_ARROW, strip.h);
                    let right = Rect::new(strip.right() - plus - gap - TAB_ARROW, strip.y, TAB_ARROW, strip.h);
                    scene.tab_arrows.push((left, shown.start.checked_sub(1), true));
                    scene.tab_arrows.push((right, Some(shown.end).filter(|&index| index < account_count), false));
                    tab_left += TAB_ARROW + gap;
                    shown
                } else {
                    0..account_count
                };
                let count = shown.len() as f64;
                let tab_width = ((room - gap * (count - 1.0)) / count).clamp(1.0, 240.0);
                for (column, index) in shown.enumerate() {
                    scene.tabs.push((
                        index,
                        Rect::new(tab_left + column as f64 * (tab_width + gap), strip.y, tab_width, strip.h),
                    ));
                }
                scene.tabs.push((account_count, Rect::new(strip.right() - plus, strip.y, plus, strip.h)));
                top = strip.bottom() + 10.0;
            }
            // Larger text needs taller lines.
            let font_size = f64::from(self.theme.font_size);
            let (name_h, sub_h) = ((font_size * 1.7 * 1.35).max(30.0), (font_size * 1.7).max(22.0));
            let column_h = AVATAR + 14.0 + name_h + sub_h + 22.0 + FIELD_H + 12.0 + 24.0;
            let room = area.bottom() - top;
            let avatar_top = top + ((room - column_h) / 2.0).max(0.0);
            let center_x = area.x + area.w / 2.0;
            let avatar = Rect::new(center_x - AVATAR / 2.0, avatar_top, AVATAR, AVATAR);
            let name = Rect::new(area.x + 16.0, avatar.bottom() + 14.0, area.w - 32.0, name_h);
            let sub = Rect::new(name.x, name.bottom(), name.w, sub_h);
            let field_width = (area.w - 48.0).clamp(1.0, 360.0);
            let field = Rect::new(center_x - field_width / 2.0, sub.bottom() + 22.0, field_width, FIELD_H);
            let submit = Rect::new(field.right() - 40.0, field.y + 8.0, 32.0, 32.0);
            let hint = Rect::new(area.x + 16.0, field.bottom() + 12.0, area.w - 32.0, 24.0);
            scene.login = Some(LoginLayout { avatar, name, sub, field, submit, hint });
        }

        if let Some(session) = panel_rect(&panels, Panel::Session)
            && greeter.locked()
        {
            let inner = body(session);
            let mut top = inner.y;
            if greeter.file.lock.show_notification_count {
                scene.away = Some(Rect::new(inner.x, inner.y, inner.w, ROW_H + 8.0));
                top += ROW_H + 20.0;
            }
            if greeter.file.lock.show_media && self.media.snapshot.media.active().is_some() {
                let player = Rect::new(inner.x, top, inner.w, 64.0);
                scene.media = Some(Rect::new(player.right() - 48.0, player.y + 14.0, 36.0, 36.0));
            }
        } else if let Some(session) = panel_rect(&panels, Panel::Session) {
            let inner = body(session);
            let power_y = inner.bottom() - 36.0;
            for (index, action) in [Action::Suspend, Action::Restart, Action::PowerOff].into_iter().enumerate() {
                scene.power.push((action, Rect::new(inner.x + index as f64 * 44.0, power_y, 36.0, 36.0)));
            }
            let fits = ((power_y - 14.0 - inner.y) / (ROW_H + 4.0)).floor().max(0.0) as usize;
            // Keep keyboard focus visible while browsing, otherwise the chosen session.
            let chosen =
                if greeter.focus == Panel::Session { greeter.session_cursor } else { greeter.session_of_selected() };
            let first = (chosen + 1).saturating_sub(fits);
            for (row, index) in (first..greeter.sessions.len()).take(fits).enumerate() {
                scene.rows.push((index, Rect::new(inner.x, inner.y + row as f64 * (ROW_H + 4.0), inner.w, ROW_H)));
            }
        }

        if let Some(panel) = panel_rect(&panels, Panel::Accessibility) {
            let inner = body(panel);
            let fits = ((inner.h + 4.0) / (ROW_H + 4.0)).floor().max(0.0) as usize;
            for row in 0..Aid::ALL.len().min(fits) {
                scene.aids.push((row, Rect::new(inner.x, inner.y + row as f64 * (ROW_H + 4.0), inner.w, ROW_H)));
            }
        }

        if greeter.palette.is_some() {
            let actions = self.greeter_actions().len();
            let width = (screen.w - 48.0).clamp(1.0, 560.0);
            let shown = actions.clamp(1, PALETTE_ROWS);
            let height = 12.0 + 44.0 + 10.0 + shown as f64 * (ROW_H + 2.0) + 10.0;
            let panel = Rect::new(
                screen.x + (screen.w - width) / 2.0,
                screen.y + (screen.h * 0.2).max(BAR_H + 16.0),
                width,
                height,
            );
            let field = Rect::new(panel.x + 12.0, panel.y + 12.0, panel.w - 24.0, 44.0);
            let rows = (0..actions.min(PALETTE_ROWS))
                .map(|index| Rect::new(field.x, field.bottom() + 10.0 + index as f64 * (ROW_H + 2.0), field.w, ROW_H))
                .collect();
            scene.palette = Some(PaletteLayout { panel, field, rows });
        }
        scene
    }

    /// What can be clicked, the first match winning.
    pub(super) fn greeter_targets(&self) -> Vec<(Rect, Target)> {
        let scene = self.greeter_scene();
        let mut out = Vec::new();
        if let Some(palette) = &scene.palette {
            let first = self.greeter.as_deref().map_or(0, palette_first);
            out.extend(palette.rows.iter().enumerate().map(|(index, rect)| (*rect, Target::Action(first + index))));
            out.push((palette.panel, Target::Palette));
            return out;
        }
        out.push((scene.launcher_button, Target::Launcher));
        if let Some((rect, _)) = &scene.keyboard {
            out.push((*rect, Target::Layout));
        }
        out.extend(scene.tabs.iter().map(|(person, rect)| (*rect, Target::Person(*person))));
        out.extend(
            scene.tab_arrows.iter().filter_map(|(rect, next, _)| next.map(|person| (*rect, Target::Person(person)))),
        );
        if let Some(login) = &scene.login {
            out.push((login.submit, Target::Submit));
        }
        out.extend(scene.cards.iter().map(|(person, rect)| (*rect, Target::Person(*person))));
        out.extend(scene.aids.iter().map(|(row, rect)| (*rect, Target::Aid(*row))));
        out.extend(scene.rows.iter().map(|(session, rect)| (*rect, Target::Session(*session))));
        out.extend(scene.power.iter().map(|(action, rect)| (*rect, Target::Run(*action))));
        out.extend(scene.media.map(|rect| (rect, Target::Run(Action::Media))));
        out.extend(scene.panels.iter().map(|(panel, rect)| (*rect, Target::Panel(*panel))));
        out
    }

    /// The greeter, topmost first: the bar, tiles and palette, then the
    /// backdrop between them, kept apart so frost can blur it.
    pub fn greeter_elements(&self, renderer: &mut GlesRenderer) -> (Vec<OutputElement>, Vec<OutputElement>) {
        let Some(greeter) = self.greeter.as_deref() else { return Default::default() };
        let mut out = Vec::new();
        // Leaving for the session: everything fades to black, which the
        // session's first frames fade in from.
        if let Some(leaving_since) = greeter.leaving {
            let gone = ease_out(leaving_since.elapsed().as_secs_f64() / super::FADE_OUT.as_secs_f64());
            out.push(crate::render::solid(&self.output_rect(), [0.0, 0.0, 0.0, gone as f32], self.scale));
        }
        // Notifications above everything (locked, only Tileception's own).
        out.extend(self.notification_elements(renderer));
        let scene = self.greeter_scene();
        let theme = &self.theme;
        let fade = ease_out(greeter.switched_at.elapsed().as_secs_f64() / CROSSFADE) as f32;
        let accent =
            (greeter.previous_accent.0.mix(theme.accent, fade), greeter.previous_accent.1.mix(theme.accent_end, fade));
        let mut painter = Painter::new(self, renderer);
        self.greeter_draw_backdrop(&mut painter, greeter, scene.screen, accent);
        let backdrop = painter.finish();

        let mut painter = Painter::new(self, renderer);
        self.greeter_draw_bar(&mut painter, greeter, &scene);
        for (panel, rect) in &scene.panels {
            let focused = greeter.focus == *panel && greeter.palette.is_none();
            // People as tiles stand on their own, without the panel around them.
            if *panel != Panel::Login || scene.cards.is_empty() {
                draw_frame(&mut painter, *rect, focused, accent);
            }
            match panel {
                Panel::Login => self.greeter_draw_login(&mut painter, greeter, &scene, accent),
                Panel::Clock => self.greeter_draw_clock(&mut painter, greeter, *rect, focused, accent),
                Panel::Motd => {
                    greeter_draw_motd(&mut painter, &self.theme, greeter, *rect, focused.then_some(accent.0))
                }
                Panel::Session if greeter.locked() => {
                    self.greeter_draw_away(&mut painter, greeter, &scene, *rect, focused, accent)
                }
                Panel::Session => self.greeter_draw_sessions(&mut painter, greeter, &scene, *rect, focused, accent),
                Panel::Accessibility => {
                    self.greeter_draw_accessibility(&mut painter, greeter, &scene, *rect, focused, accent)
                }
            }
        }
        if let Some(palette) = &scene.palette {
            self.greeter_draw_palette(&mut painter, greeter, palette, accent);
        }
        out.extend(painter.finish());
        (out, backdrop)
    }

    /// Between the tiles: a backdrop from the person's accent, or with
    /// per-user looks off, the `[theme]` wallpaper.
    fn greeter_draw_backdrop(&self, painter: &mut Painter, greeter: &Greeter, screen: Rect, accent: (Colour, Colour)) {
        let theme = &self.theme;
        let fallback = match greeter.file.per_user.wallpaper_fallback {
            WallpaperFallback::Generated if !greeter.file.per_user.wallpaper => WallpaperFallback::System,
            other => other,
        };
        painter.fill(screen, theme.background, 0.0);
        if greeter.aid_on(Aid::HighContrast) {
            return;
        }
        match fallback {
            WallpaperFallback::Generated => {
                // A soft wash of the person's accent, brightest in two corners.
                painter
                    .shape(screen, Shape { fill: (accent.0.alpha(0.10), accent.1.alpha(0.07)), ..Default::default() });
                let spot = (screen.w.min(screen.h) * 0.35).max(1.0);
                let corner =
                    |centre_x: f64, centre_y: f64| Rect::new(centre_x - spot / 2.0, centre_y - spot / 2.0, spot, spot);
                let glow = |colour: Colour| Shape {
                    radius: (spot / 2.0) as f32,
                    fill: (colour.alpha(0.10), colour.alpha(0.10)),
                    glow_radius: (spot * 0.8) as f32,
                    glow: colour.alpha(0.16),
                    ..Default::default()
                };
                painter.shape(corner(screen.x + screen.w * 0.12, screen.bottom() - screen.h * 0.1), glow(accent.0));
                painter.shape(corner(screen.right() - screen.w * 0.1, screen.y + screen.h * 0.18), glow(accent.1));
            }
            WallpaperFallback::System => {
                if let Some(wallpaper) = &self.wallpaper {
                    painter.image(&wallpaper.buffer, (screen.x, screen.y));
                    painter.fill(screen, Colour([0.0, 0.0, 0.0, 0.35]), 0.0);
                }
            }
            WallpaperFallback::Flat => {}
        }
        // Shared wallpapers over that: the newly selected person's fades in,
        // over the one before when the person changed. People shown as
        // tiles carry their own.
        if greeter.file.per_user.wallpaper && !greeter.cards() {
            let fade = ease_out(greeter.switched_at.elapsed().as_secs_f64() / CROSSFADE) as f32;
            let mut layer = |person: usize, alpha: f32| {
                if let Some(wallpaper) = greeter.wallpapers.get(&person).filter(|_| alpha > 0.0) {
                    painter.alpha = alpha;
                    painter.image(wallpaper, (screen.x, screen.y));
                    painter.fill(screen, Colour([0.0, 0.0, 0.0, 0.3]), 0.0);
                }
            };
            if greeter.previous_user != greeter.user {
                layer(greeter.previous_user, 1.0 - fade);
            }
            layer(greeter.user, fade);
            painter.alpha = 1.0;
        }
    }

    fn greeter_draw_bar(&self, painter: &mut Painter, greeter: &Greeter, scene: &Scene) {
        let theme = &self.theme;
        let bar = scene.bar;
        painter.frosted(bar, Shape::filled(theme.surface, 0.0));
        painter.fill(Rect::new(bar.x, bar.bottom() - 1.0, bar.w, 1.0), theme.border, 0.0);
        // The Tide mark: a tall tile beside two small ones.
        let (left, top, size) = (bar.x + 14.0, bar.y + (BAR_H - 14.0) / 2.0, 6.0);
        painter.fill(Rect::new(left, top, size, 14.0), theme.accent, 1.5);
        painter.fill(Rect::new(left + size + 2.0, top, size, size), theme.accent, 1.5);
        painter.fill(Rect::new(left + size + 2.0, top + size + 2.0, size, size), theme.accent_end, 1.5);
        painter.text(
            &greeter.hostname,
            Rect::new(left + 22.0, bar.y, 240.0, BAR_H),
            Style::new(theme.font_size * 0.95, theme.muted),
        );
        let now = format!("{} \u{00b7} {}", local_time("%A %-d %B"), local_time(&greeter.file.clock_format));
        painter.text(&now, bar, Style::new(theme.font_size, theme.text).bold().align(Align::Centre));
        let style = Style::new(theme.font_size * 0.9, theme.muted);
        if let Some((rect, label)) = &scene.battery {
            painter.text(label, *rect, style.align(Align::Centre));
        }
        if let Some((rect, label)) = &scene.keyboard {
            let pointer = Point::new(self.pointer.x, self.pointer.y);
            if rect.contains(pointer) {
                painter.fill(*rect, theme.surface_raised, theme.radius);
            }
            painter.glyph(glyph::KEYBOARD, Rect::new(rect.x + 4.0, rect.y, 18.0, rect.h), 14, theme.muted);
            painter.text(label, Rect::new(rect.x + 26.0, rect.y, rect.w - 28.0, rect.h), style);
        }
        let open = greeter.palette.is_some();
        if open {
            painter.fill(scene.launcher_button, theme.accent.alpha(0.16), theme.radius);
        }
        painter.glyph(glyph::POWER, scene.launcher_button, 16, if open { theme.accent } else { theme.text });
    }

    fn greeter_draw_login(&self, painter: &mut Painter, greeter: &Greeter, scene: &Scene, accent: (Colour, Colour)) {
        let theme = &self.theme;
        self.greeter_draw_cards(painter, greeter, scene, accent);
        let strip =
            scene.tabs.iter().map(|(_, rect)| *rect).chain(scene.tab_arrows.iter().map(|(rect, _, _)| *rect)).reduce(
                |merged, tab| {
                    Rect::new(
                        merged.x.min(tab.x),
                        merged.y,
                        tab.right().max(merged.right()) - merged.x.min(tab.x),
                        merged.h,
                    )
                },
            );
        if let Some(strip) = strip {
            let around = Rect::new(strip.x - 3.0, strip.y - 3.0, strip.w + 6.0, strip.h + 6.0);
            painter.fill(around, theme.background.alpha(0.5), theme.radius);
        }
        for (person, rect) in &scene.tabs {
            let active = *person == greeter.user;
            let (accent_start, accent_end) = greeter.accent_of(theme, *person);
            if active {
                let tint =
                    Shape { fill: (accent.0.alpha(0.16), accent.0.alpha(0.05)), radius: 4.0, ..Default::default() };
                painter.shape(*rect, tint);
                let underline = Rect::new(rect.x + 2.0, rect.bottom() - 2.0, rect.w - 4.0, 2.0);
                painter.shape(underline, Shape { radius: 1.0, fill: accent, ..Default::default() });
            }
            let colour = if active { theme.text } else { theme.muted };
            match greeter.accounts.get(*person) {
                Some(account) => {
                    let dot = Rect::new(rect.x + 8.0, rect.y + (rect.h - 18.0) / 2.0, 18.0, 18.0);
                    self.greeter_draw_avatar(painter, greeter, *person, dot, (accent_start, accent_end), 0.62);
                    let label = Rect::new(dot.right() + 7.0, rect.y, rect.w - 40.0, rect.h);
                    let style = Style::new(theme.font_size * 0.95, colour);
                    painter.text(&account.name, label, if active { style.bold() } else { style });
                }
                None => painter.glyph(PLUS, *rect, 14, colour),
            }
        }
        for (rect, target, left) in &scene.tab_arrows {
            let colour = if target.is_some() { theme.muted } else { theme.muted.alpha(0.3) };
            painter.glyph(if *left { CHEVRON_LEFT } else { CHEVRON_RIGHT }, *rect, 14, colour);
        }
        if let (Some(tab), Some(account)) = (scene.locked_tab, greeter.selected()) {
            painter.fill(
                Rect::new(tab.x - 3.0, tab.y - 3.0, tab.w + 6.0, tab.h + 6.0),
                theme.background.alpha(0.5),
                theme.radius,
            );
            painter.shape(
                tab,
                Shape { fill: (accent.0.alpha(0.16), accent.0.alpha(0.05)), radius: 4.0, ..Default::default() },
            );
            let underline = Rect::new(tab.x + 2.0, tab.bottom() - 2.0, tab.w - 4.0, 2.0);
            painter.shape(underline, Shape { radius: 1.0, fill: accent, ..Default::default() });
            painter.glyph(glyph::LOCK, Rect::new(tab.x + 6.0, tab.y, 22.0, tab.h), 14, accent.0);
            let label = format!("{} \u{00b7} locked", account.name);
            painter.text(
                &label,
                Rect::new(tab.x + 32.0, tab.y, tab.w - 40.0, tab.h),
                Style::new(theme.font_size * 0.95, theme.text).bold(),
            );
        }

        let Some(layout) = &scene.login else { return };
        let account = greeter.selected();
        painter.shape(
            Rect::new(layout.avatar.x - 5.0, layout.avatar.y - 5.0, layout.avatar.w + 10.0, layout.avatar.h + 10.0),
            Shape {
                radius: (layout.avatar.w / 2.0 + 5.0) as f32,
                border_width: 1.5,
                border: accent.0.alpha(0.45),
                glow_radius: 18.0,
                glow: accent.0.alpha(0.18),
                ..Default::default()
            },
        );
        self.greeter_draw_avatar(painter, greeter, greeter.user, layout.avatar, accent, 0.42);

        let typed_name = greeter.name.as_str();
        let name = match account {
            Some(account) => account.name.clone(),
            None if greeter.named => typed_name.trim().to_string(),
            None => "Someone not listed".to_string(),
        };
        painter.text(&name, layout.name, Style::new(theme.font_size * 1.7, theme.text).bold().align(Align::Centre));
        let session = greeter
            .sessions
            .get(greeter.session_of_selected())
            .map(|entry| entry.name.as_str())
            .unwrap_or("no session");
        let sub = match (&greeter.auth, account) {
            _ if greeter.notice.is_some() => greeter.notice.clone().unwrap_or_default(),
            (Auth::Answering, _) if greeter.locked() => "Unlocking...".to_string(),
            _ if greeter.locked() => match &greeter.mode {
                super::Mode::Lock { locked_at, .. } => format!("Locked at {locked_at}"),
                super::Mode::Login => String::new(),
            },
            (Auth::Asking { text, .. }, _) if !text.is_empty() => text.clone(),
            (Auth::Creating | Auth::Answering, _) => "Logging in...".to_string(),
            (Auth::Starting, _) => format!("Starting {session}..."),
            (_, Some(account)) => {
                let last = greeter
                    .last_login(&account.login)
                    .and_then(|timestamp| last_login(timestamp, &greeter.file.clock_format));
                match (last, account.login != account.name) {
                    (Some(last), true) => format!("{} \u{00b7} last login {last}", account.login),
                    (Some(last), false) => format!("Last login {last}"),
                    (None, true) => format!("{} \u{00b7} {session}", account.login),
                    (None, false) => session.to_string(),
                }
            }
            (_, None) if greeter.named => session.to_string(),
            (_, None) => "Log in with an account that is not listed".to_string(),
        };
        let sub_colour = if greeter.notice.is_some() { theme.danger } else { theme.muted };
        painter.text(&sub, layout.sub, Style::new(theme.font_size * 0.95, sub_colour).align(Align::Centre));

        // The field, shaking briefly after a wrong answer.
        let failed_for = greeter.failed_at.map(|failed_at| failed_at.elapsed().as_secs_f64());
        let shake = failed_for
            .filter(|elapsed| *elapsed < SHAKE)
            .map(|elapsed| (elapsed * 46.0).sin() * 10.0 * (1.0 - elapsed / SHAKE))
            .unwrap_or(0.0);
        let failed = failed_for.is_some_and(|elapsed| elapsed < 3.0);
        let field = Rect::new(layout.field.x + shake, layout.field.y, layout.field.w, layout.field.h);
        let focused = greeter.focus == Panel::Login && greeter.palette.is_none();
        let edge = if failed {
            theme.danger
        } else if focused {
            accent.0
        } else {
            theme.border
        };
        painter.shape(
            field,
            Shape {
                border_width: 1.0,
                border: edge,
                glow_radius: if focused || failed { theme.glow * 0.8 } else { 0.0 },
                glow: edge.alpha(0.2),
                ..Shape::filled(theme.background.alpha(0.9), theme.radius)
            },
        );
        let naming = greeter.other() && !greeter.named;
        let busy = greeter.busy();
        let submit = Rect::new(layout.submit.x + shake, layout.submit.y, layout.submit.w, layout.submit.h);
        let ready = !busy && (if naming { !greeter.name.is_empty() } else { !greeter.field.is_empty() });
        if ready {
            painter.shape(
                submit,
                Shape { radius: theme.radius, fill: (accent.0.alpha(0.9), accent.1.alpha(0.9)), ..Default::default() },
            );
        } else {
            painter.fill(submit, theme.surface_raised, theme.radius);
        }
        painter.glyph(glyph::ARROW_RIGHT, submit, 16, if ready { theme.on_accent } else { theme.muted });

        let inner = Rect::new(field.x + 16.0, field.y, submit.x - field.x - 24.0, field.h);
        let caret_on = focused && !busy && greeter.typed_at.elapsed().as_millis() % 1000 < 600;
        let selection = accent.0.alpha(0.35);
        let since = self.start.elapsed().as_secs_f64();
        if naming {
            let style = Style::new(theme.font_size * 1.1, theme.text);
            if typed_name.is_empty() {
                painter.text("Username", inner, Style { colour: theme.muted, ..style });
            }
            if greeter.name.is_selected() {
                let (width, _) = painter.measure(typed_name, inner.w, style);
                painter.fill(Rect::new(inner.x - 2.0, field.y + 12.0, width + 4.0, field.h - 24.0), selection, 4.0);
            }
            let (width, _) = painter.text(typed_name, inner, style);
            if caret_on {
                painter.fill(Rect::new(inner.x + width + 1.0, field.y + 14.0, 2.0, field.h - 28.0), accent.0, 1.0);
            }
        } else {
            let pitch = 15.0;
            let room = ((inner.w - 8.0) / pitch).floor().max(1.0) as usize;
            let count = greeter.field.chars().min(room);
            let shown = if busy { room.min(6) } else { count };
            if shown == 0 {
                let placeholder = match &greeter.auth {
                    Auth::Asking { text, .. } if !text.is_empty() => text.trim_end_matches(':').to_string(),
                    _ => "Password".to_string(),
                };
                painter.text(&placeholder, inner, Style::new(theme.font_size * 1.1, theme.muted));
            }
            let visible = matches!(greeter.auth, Auth::Asking { prompt: super::greetd::Prompt::Visible, .. });
            let selected = greeter.field.is_selected() && !busy;
            if visible && !busy {
                // A question whose answer may be shown, such as a one-time code.
                let style = Style::new(theme.font_size * 1.1, theme.text);
                if selected {
                    let (width, _) = painter.measure(greeter.field.as_str(), inner.w, style);
                    painter.fill(Rect::new(inner.x - 2.0, field.y + 12.0, width + 4.0, field.h - 24.0), selection, 4.0);
                }
                let (width, _) = painter.text(greeter.field.as_str(), inner, style);
                if caret_on {
                    painter.fill(Rect::new(inner.x + width + 1.0, field.y + 14.0, 2.0, field.h - 28.0), accent.0, 1.0);
                }
            } else {
                if selected {
                    let width = shown as f64 * pitch + 4.0;
                    painter.fill(Rect::new(inner.x, field.y + 12.0, width, field.h - 24.0), selection, 4.0);
                }
                for index in 0..shown {
                    let alpha =
                        if busy { 0.3 + 0.7 * (0.5 + 0.5 * (since * 7.0 - index as f64 * 0.7).sin()) } else { 1.0 };
                    let dot = Rect::new(inner.x + index as f64 * pitch + 3.5, field.y + field.h / 2.0 - 4.0, 8.0, 8.0);
                    painter.fill(dot, theme.text.alpha(alpha as f32), 4.0);
                }
                if caret_on {
                    let caret_x = inner.x + shown as f64 * pitch + 2.0;
                    painter.fill(Rect::new(caret_x, field.y + 14.0, 2.0, field.h - 28.0), accent.0, 1.0);
                }
            }
        }

        // Caps Lock first: it spoils passwords.
        if greeter.caps_lock && !naming {
            let style = Style::new(theme.font_size * 0.9, theme.warning).bold();
            let (width, _) = painter.measure("Caps Lock is on", 400.0, style);
            let chip = Rect::new(
                layout.hint.x + (layout.hint.w - width - 46.0) / 2.0,
                layout.hint.y - 2.0,
                width + 46.0,
                28.0,
            );
            painter.shape(
                chip,
                Shape {
                    border_width: 1.0,
                    border: theme.warning.alpha(0.5),
                    ..Shape::filled(theme.warning.alpha(0.12), 14.0)
                },
            );
            painter.glyph(glyph::CAPS, Rect::new(chip.x + 8.0, chip.y, 20.0, chip.h), 14, theme.warning);
            painter.text("Caps Lock is on", Rect::new(chip.x + 32.0, chip.y, width + 4.0, chip.h), style);
            return;
        }
        let super_key = if self.nested { "Alt" } else { "Super" };
        let tab = format!("{super_key}+Tab");
        let hints: Vec<(&str, &str)> = if busy {
            Vec::new()
        } else if greeter.locked() {
            vec![("Enter", "unlock"), (super_key, "actions")]
        } else if naming {
            vec![("Enter", "continue"), (super_key, "actions")]
        } else if greeter.other() {
            vec![("Enter", "log in"), ("Esc", "change username")]
        } else if greeter.accounts.len() > 1 {
            vec![(tab.as_str(), "next user"), (super_key, "actions")]
        } else {
            vec![("Enter", "log in"), (super_key, "actions")]
        };
        let width = painter.key_hints(&hints, 0.0, 0.0, true);
        painter.key_hints(
            &hints,
            layout.hint.x + (layout.hint.w - width) / 2.0,
            layout.hint.y + layout.hint.h / 2.0,
            false,
        );
    }

    /// People as tiles: each over their own wallpaper (or their accent),
    /// darkened so text reads on it. The selected one, widest, gets the
    /// login form from [`Self::greeter_draw_login`]; the others their
    /// picture and name.
    fn greeter_draw_cards(&self, painter: &mut Painter, greeter: &Greeter, scene: &Scene, accent: (Colour, Colour)) {
        let theme = &self.theme;
        let focused = greeter.focus == Panel::Login && greeter.palette.is_none();
        let contrast = greeter.aid_on(Aid::HighContrast);
        for (person, rect) in &scene.cards {
            let active = *person == greeter.user;
            let own = greeter.accent_of(theme, *person);
            let drop = if theme.frost { 0.0 } else { 4.0 };
            painter.shape(
                Rect::new(rect.x, rect.y + drop, rect.w, rect.h),
                Shape {
                    radius: theme.radius,
                    glow_radius: 14.0,
                    glow: Colour([0.0, 0.0, 0.0, 0.2]),
                    ..Default::default()
                },
            );
            painter.fill(*rect, theme.surface, theme.radius);
            if !contrast {
                let wallpaper = greeter.wallpapers.get(person).filter(|_| greeter.file.per_user.wallpaper);
                match wallpaper {
                    Some(wallpaper) => {
                        painter.clipped(*rect, |clipped| clipped.image(wallpaper, (scene.screen.x, scene.screen.y)));
                    }
                    None => painter.shape(
                        *rect,
                        Shape {
                            fill: (own.0.alpha(0.22), own.1.alpha(0.10)),
                            radius: theme.radius,
                            ..Default::default()
                        },
                    ),
                }
                let shade = if active { 0.45 } else { 0.68 };
                painter.fill(*rect, Colour([0.0, 0.0, 0.0, shade]), theme.radius);
            }
            let lit = active && focused;
            painter.shape(
                *rect,
                Shape {
                    border_width: if lit { 1.5 } else { 1.0 },
                    border: if lit { accent.0 } else { theme.border },
                    glow_radius: if lit { theme.glow * 0.7 } else { 0.0 },
                    glow: accent.0.alpha(0.2),
                    radius: theme.radius,
                    ..Default::default()
                },
            );
            if active {
                continue;
            }
            let font_size = f64::from(theme.font_size);
            let size = (rect.w * 0.5).clamp(32.0, 64.0);
            let name_h = (font_size * 1.6).max(22.0);
            let top = rect.y + ((rect.h - size - 10.0 - name_h) / 2.0).max(0.0);
            let picture = Rect::new(rect.x + (rect.w - size) / 2.0, top, size, size);
            self.greeter_draw_avatar(painter, greeter, *person, picture, own, 0.42);
            let name = greeter.accounts.get(*person).map_or("Not listed", |account| account.name.as_str());
            painter.text(
                name,
                Rect::new(rect.x + 8.0, picture.bottom() + 10.0, rect.w - 16.0, name_h),
                Style::new(theme.font_size, theme.text).align(Align::Centre),
            );
        }
    }

    /// Switches for larger text and high contrast.
    fn greeter_draw_accessibility(
        &self,
        painter: &mut Painter,
        greeter: &Greeter,
        scene: &Scene,
        rect: Rect,
        focused: bool,
        accent: (Colour, Colour),
    ) {
        let theme = &self.theme;
        draw_title(painter, theme, rect, "Accessibility", focused.then_some(accent.0));
        for (row, line) in &scene.aids {
            let Some(aid) = Aid::ALL.get(*row).copied() else { continue };
            let enabled = greeter.aid_on(aid);
            if focused && *row == greeter.aid_cursor {
                painter.shape(
                    *line,
                    Shape {
                        border_width: 1.0,
                        border: accent.0.alpha(0.7),
                        radius: theme.radius,
                        ..Default::default()
                    },
                );
            }
            painter.glyph(
                aid.glyph(),
                Rect::new(line.x + 6.0, line.y, 28.0, line.h),
                16,
                if enabled { accent.0 } else { theme.muted },
            );
            painter.text(
                aid.label(),
                Rect::new(line.x + 42.0, line.y, (line.w - 42.0 - 60.0).max(0.0), line.h),
                Style::new(theme.font_size, theme.text),
            );
            let track = Rect::new(line.right() - 48.0, line.y + (line.h - 22.0) / 2.0, 38.0, 22.0);
            crate::panel::draw_switch(painter, track, enabled, false, true);
        }
    }

    /// An avatar picture, or the initial on the accent gradient.
    fn greeter_draw_avatar(
        &self,
        painter: &mut Painter,
        greeter: &Greeter,
        index: usize,
        rect: Rect,
        accent: (Colour, Colour),
        text: f32,
    ) {
        let theme = &self.theme;
        let account = greeter.accounts.get(index);
        if let Some(picture) = account.and_then(|entry| self.greeter_avatar_buffer(greeter, entry, rect.w)) {
            painter.image(&picture, (rect.x, rect.y));
            return;
        }
        painter.shape(rect, Shape { radius: (rect.w / 2.0) as f32, fill: accent, ..Default::default() });
        match account {
            Some(account) => {
                let initial: String = account.name.chars().take(1).collect::<String>().to_uppercase();
                let style = Style::new((rect.h as f32 * text).max(6.0), theme.on_accent).bold().align(Align::Centre);
                painter.text(&initial, rect, style);
            }
            None => painter.glyph(USER, rect, (rect.w * 0.5).round().max(8.0) as u32, theme.on_accent),
        }
    }

    fn greeter_avatar_buffer(
        &self,
        greeter: &Greeter,
        account: &super::Account,
        size: f64,
    ) -> Option<MemoryRenderBuffer> {
        let path = account.icon.as_ref()?;
        let pixels = (size * self.scale).round().max(1.0) as u32;
        let key = (account.login.clone(), pixels);
        greeter
            .avatars
            .borrow_mut()
            .entry(key)
            .or_insert_with(|| {
                let rgba = super::accounts::avatar_pixels(path, pixels)?;
                Some(MemoryRenderBuffer::from_slice(
                    &rgba,
                    smithay::backend::allocator::Fourcc::Abgr8888,
                    (pixels as i32, pixels as i32),
                    1,
                    smithay::utils::Transform::Normal,
                    None,
                ))
            })
            .clone()
    }

    /// The time and date centred, the current week under them and the
    /// battery and machine as pills along the bottom, each when there is room.
    fn greeter_draw_clock(
        &self,
        painter: &mut Painter,
        greeter: &Greeter,
        rect: Rect,
        focused: bool,
        accent: (Colour, Colour),
    ) {
        let theme = &self.theme;
        draw_title(painter, theme, rect, "Clock", focused.then_some(accent.0));
        let inner = body(rect);
        let font_size = f64::from(theme.font_size);
        let min_time = font_size * 1.6 * 1.25 + 24.0;
        let pill_style = Style::new(theme.font_size * 0.85, theme.muted);
        let mut pills = self.battery.reading.as_ref().map(|reading| reading.describe()).into_iter().collect::<Vec<_>>();
        pills.extend(machine(&greeter.hostname, &greeter.kernel));
        let mut room = inner.h;
        if !pills.is_empty() && room >= PILL_H + 12.0 + min_time {
            room -= PILL_H + 12.0;
            let mut left = inner.x;
            for label in &pills {
                let width = painter.measure(label, 400.0, pill_style).0 + 20.0;
                if left + width > inner.right() {
                    break;
                }
                let pill = Rect::new(left, inner.bottom() - PILL_H, width, PILL_H);
                painter.fill(pill, theme.surface_raised, (PILL_H / 2.0) as f32);
                painter.text(label, pill, pill_style.align(Align::Centre));
                left += width + 6.0;
            }
        }
        let now = local_tm();
        let strip_h = (font_size * 4.2).round();
        let strip_w = inner.w.min(7.0 * 84.0 + 6.0 * 6.0);
        let strip = now.filter(|_| strip_w >= 7.0 * 40.0 && room >= font_size * 3.5 * 1.25 + 24.0 + 20.0 + strip_h);
        let below = 24.0 + if strip.is_some() { 20.0 + strip_h } else { 0.0 };
        // Wider tiles get a bigger time, up to about one and a half times.
        let grow = (rect.w / 600.0).clamp(1.0, 1.6) as f32;
        let size = (theme.font_size * 5.6 * grow).min(((room - below) / 1.25) as f32).max(theme.font_size * 1.6);
        let block = f64::from(size) * 1.25 + below;
        let top = inner.y + ((room - block) / 2.0).max(0.0);
        let time = Rect::new(inner.x, top, inner.w, f64::from(size) * 1.25);
        painter.text(
            &local_time(&greeter.file.clock_format),
            time,
            Style::new(size, theme.text).weight(300).align(Align::Centre),
        );
        let date = Rect::new(inner.x, time.bottom(), inner.w, 24.0);
        painter.text(
            &local_time("%A %-d %B"),
            date,
            Style::new(theme.font_size * 1.05, theme.muted).align(Align::Centre),
        );
        if let Some(now) = strip {
            let week = Rect::new(inner.x + (inner.w - strip_w) / 2.0, date.bottom() + 20.0, strip_w, strip_h);
            draw_week(painter, theme, week, &now, accent);
        }
    }

    /// Locked: counts of what came in, and the player without its title.
    fn greeter_draw_away(
        &self,
        painter: &mut Painter,
        greeter: &Greeter,
        scene: &Scene,
        rect: Rect,
        focused: bool,
        accent: (Colour, Colour),
    ) {
        let theme = &self.theme;
        draw_title(painter, theme, rect, "While you were away", focused.then_some(accent.0));
        if !greeter.file.lock.show_notification_count && !greeter.file.lock.show_media {
            painter.text(
                "Shown after unlocking",
                body(rect),
                Style::new(theme.font_size, theme.muted).align(Align::Centre),
            );
            return;
        }
        if let (Some(row), Some((total, _))) = (scene.away, self.notification_status()) {
            let before = match &greeter.mode {
                super::Mode::Lock { notifications, .. } => *notifications,
                super::Mode::Login => total,
            };
            let count = total.saturating_sub(before);
            painter.fill(row, theme.surface_raised.alpha(0.6), theme.radius);
            painter.glyph(
                glyph::BELL,
                Rect::new(row.x + 6.0, row.y, 28.0, row.h),
                16,
                if count > 0 { accent.0 } else { theme.muted },
            );
            let label = match count {
                0 => "No new notifications".to_string(),
                1 => "1 notification".to_string(),
                number => format!("{number} notifications"),
            };
            painter.text(
                &label,
                Rect::new(row.x + 42.0, row.y, row.w - 180.0, row.h),
                Style::new(theme.font_size, theme.text),
            );
            if count > 0 {
                let style = Style::new(theme.font_size * 0.8, theme.muted);
                let (width, _) = painter.measure("hidden until unlock", 300.0, style);
                painter.text(
                    "hidden until unlock",
                    Rect::new(row.right() - width - 12.0, row.y, width + 2.0, row.h),
                    style,
                );
            }
        }
        let (Some(button), Some(player)) = (scene.media, self.media.snapshot.media.active()) else { return };
        let panel = Rect::new(rect.x + 14.0, button.y - 14.0, rect.w - 28.0, 64.0);
        painter.fill(panel, theme.surface_raised.alpha(0.6), theme.radius);
        let art = Rect::new(panel.x + 12.0, panel.y + 12.0, 40.0, 40.0);
        painter.shape(
            art,
            Shape { radius: theme.radius, fill: (accent.0.alpha(0.5), accent.1.alpha(0.5)), ..Default::default() },
        );
        painter.glyph(glyph::MUSIC, art, 18, theme.on_accent);
        let playing = player.status == tileception_media::Status::Playing;
        let text = Rect::new(art.right() + 12.0, panel.y + 12.0, button.x - art.right() - 24.0, 20.0);
        painter.text(if playing { "Playing" } else { "Paused" }, text, Style::new(theme.font_size, theme.text).bold());
        let below = Rect::new(text.x, text.bottom(), text.w, 20.0);
        painter.text("Title hidden until unlock", below, Style::new(theme.font_size * 0.85, theme.muted));
        painter.shape(
            button,
            Shape { border_width: 1.0, border: theme.border, ..Shape::filled(theme.surface_raised, theme.radius) },
        );
        painter.glyph(if playing { glyph::PAUSE } else { glyph::PLAY }, button, 16, theme.text);
    }

    fn greeter_draw_sessions(
        &self,
        painter: &mut Painter,
        greeter: &Greeter,
        scene: &Scene,
        rect: Rect,
        focused: bool,
        accent: (Colour, Colour),
    ) {
        let theme = &self.theme;
        draw_title(painter, theme, rect, "Session", focused.then_some(accent.0));
        let current = greeter.session_of_selected();
        let last = greeter.selected().and_then(|account| greeter.last_session(&account.login));
        if greeter.sessions.is_empty() {
            painter.text(
                "No sessions installed",
                body(rect),
                Style::new(theme.font_size, theme.muted).align(Align::Centre),
            );
        }
        for (index, row) in &scene.rows {
            let Some(session) = greeter.sessions.get(*index) else { continue };
            let is_current = *index == current;
            if is_current {
                painter.shape(
                    *row,
                    Shape {
                        fill: (accent.0.alpha(0.14), accent.0.alpha(0.04)),
                        radius: theme.radius,
                        ..Default::default()
                    },
                );
            }
            if focused && *index == greeter.session_cursor {
                painter.shape(
                    *row,
                    Shape {
                        border_width: 1.0,
                        border: accent.0.alpha(0.7),
                        radius: theme.radius,
                        ..Default::default()
                    },
                );
            }
            let mark = Rect::new(row.x + 10.0, row.y + (row.h - 20.0) / 2.0, 20.0, 20.0);
            let icons = &self.launcher.icons;
            let logo = session.logo.as_deref().and_then(|path| icons.image(path, mark.w as u32)).or_else(|| {
                session
                    .session_id
                    .starts_with("tileception")
                    .then(|| icons.embedded("tileception-badge", TILECEPTION_BADGE, mark.w as u32))
                    .flatten()
            });
            if let Some(logo) = logo {
                painter.image(&logo, (mark.x, mark.y));
            } else {
                // No logo installed: the name's initial.
                painter.fill(mark, theme.surface_raised, 4.0);
                let initial = session.name.chars().take(1).collect::<String>().to_uppercase();
                painter.text(
                    &initial,
                    mark,
                    Style::new(theme.font_size * 0.8, theme.muted).bold().align(Align::Centre),
                );
            }
            let colour = if is_current { theme.text } else { theme.muted };
            let label = Rect::new(mark.right() + 10.0, row.y, row.w - 130.0, row.h);
            painter.text(&session.name, label, Style::new(theme.font_size, colour));
            if Some(*index) == last {
                let style = Style::new(theme.font_size * 0.8, theme.muted);
                let (width, _) = painter.measure("last used", 200.0, style);
                painter.text("last used", Rect::new(row.right() - width - 12.0, row.y, width + 2.0, row.h), style);
            }
        }
        if let Some((_, first)) = scene.power.first() {
            painter.fill(Rect::new(first.x, first.y - 10.0, body(rect).w, 1.0), theme.text.alpha(0.06), 0.0);
        }
        for (action, button) in &scene.power {
            painter.shape(
                *button,
                Shape { border_width: 1.0, border: theme.border, ..Shape::filled(theme.surface_raised, theme.radius) },
            );
            let icon = match action {
                Action::Suspend => glyph::MOON,
                Action::Restart => glyph::RESTART,
                _ => glyph::POWER,
            };
            painter.glyph(icon, *button, 16, theme.muted);
        }
        if let Some((_, last)) = scene.power.last() {
            let hints = [(if self.nested { "Alt" } else { "Super" }, "more actions")];
            let width = painter.key_hints(&hints, 0.0, 0.0, true);
            let right = body(rect).right();
            if right - width > last.right() + 12.0 {
                painter.key_hints(&hints, right - width, last.y + last.h / 2.0, false);
            }
        }
    }

    fn greeter_draw_palette(
        &self,
        painter: &mut Painter,
        greeter: &Greeter,
        palette: &PaletteLayout,
        accent: (Colour, Colour),
    ) {
        let theme = &self.theme;
        let Some(state) = &greeter.palette else { return };
        let opened = state.opened.map_or(1.0, |opened_at| ease_out(opened_at.elapsed().as_secs_f64() / 0.16));
        painter.alpha = opened as f32;
        painter.fill(self.output_rect(), Colour([0.0, 0.0, 0.0, 0.32]), 0.0);
        painter.offset = (0.0, (opened - 1.0) * 12.0);
        painter.popup(palette.panel);
        painter.shape(
            palette.field,
            Shape {
                border_width: 1.0,
                border: accent.0.alpha(0.45),
                glow_radius: theme.glow * 0.6,
                glow: accent.0.alpha(0.12),
                ..Shape::filled(theme.background, theme.radius)
            },
        );
        let typing = !state.query.is_empty();
        painter.glyph(
            glyph::SEARCH,
            Rect::new(palette.field.x + 8.0, palette.field.y, 28.0, palette.field.h),
            18,
            if typing { accent.0 } else { theme.muted },
        );
        let text = Rect::new(palette.field.x + 42.0, palette.field.y, palette.field.w - 54.0, palette.field.h);
        let style = Style::new(theme.font_size * 1.15, theme.text);
        let (width, _) = if typing {
            painter.text(&state.query, text, style)
        } else {
            painter.text(
                match state.page {
                    super::PalettePage::More => "More…",
                    super::PalettePage::ConfirmFirmware => "Restart into UEFI settings?",
                    super::PalettePage::Main if greeter.locked() => "Power and media",
                    super::PalettePage::Main => "Power, sessions, people",
                },
                text,
                Style { colour: theme.muted, ..style },
            );
            (0.0, 0.0)
        };
        if self.start.elapsed().as_millis() % 1000 < 600 {
            painter.fill(Rect::new(text.x + width + 1.0, text.y + 12.0, 2.0, text.h - 24.0), accent.0, 1.0);
        }
        let actions = self.greeter_actions();
        if actions.is_empty() {
            let row = Rect::new(palette.field.x, palette.field.bottom() + 10.0, palette.field.w, ROW_H);
            painter.text("No matches", row, Style::new(theme.font_size, theme.muted).align(Align::Centre));
        }
        let first = palette_first(greeter);
        for (index, row) in palette.rows.iter().enumerate() {
            let Some((action, label)) = actions.get(first + index) else { break };
            let selected = first + index == state.selected;
            if selected {
                painter.shape(
                    *row,
                    Shape {
                        fill: (accent.0.alpha(0.16), accent.0.alpha(0.05)),
                        radius: theme.radius,
                        ..Default::default()
                    },
                );
            }
            let icon = match action {
                Action::Media => glyph::MUSIC,
                Action::Suspend => glyph::MOON,
                Action::Restart | Action::Firmware | Action::ConfirmFirmware => glyph::RESTART,
                Action::More => glyph::CHEVRON_RIGHT,
                Action::Back => glyph::ARROW_LEFT,
                Action::PowerOff => glyph::POWER,
                Action::Session(_) => glyph::OVERVIEW,
                Action::Person(_) => USER,
                Action::Layout(_) | Action::Keymap(_) => glyph::KEYBOARD,
                Action::Aid(aid) => aid.glyph(),
            };
            painter.glyph(
                icon,
                Rect::new(row.x + 6.0, row.y, 28.0, row.h),
                16,
                if selected { accent.0 } else { theme.muted },
            );
            painter.text(
                label,
                Rect::new(row.x + 42.0, row.y, row.w - 110.0, row.h),
                Style::new(theme.font_size, if selected { theme.text } else { theme.muted }),
            );
            if selected {
                let style = Style::new(theme.font_size * 0.8, theme.muted);
                let (width, _) = painter.measure("Enter", 100.0, style);
                painter.text("Enter", Rect::new(row.right() - width - 12.0, row.y, width + 2.0, row.h), style);
            }
        }
        painter.alpha = 1.0;
        painter.offset = (0.0, 0.0);
    }
}

/// The first launcher row shown, so the selected one is in view.
fn palette_first(greeter: &Greeter) -> usize {
    let selected = greeter.palette.as_ref().map_or(0, |palette| palette.selected);
    (selected + 1).saturating_sub(PALETTE_ROWS)
}

/// A tile, as windows are framed on the desktop.
fn draw_frame(painter: &mut Painter, rect: Rect, focused: bool, accent: (Colour, Colour)) {
    let theme = painter.theme();
    // Frosted, the shadow cannot be offset: it would show through.
    let drop = if theme.frost { 0.0 } else { 4.0 };
    painter.shape(
        Rect::new(rect.x, rect.y + drop, rect.w, rect.h),
        Shape { radius: theme.radius, glow_radius: 14.0, glow: Colour([0.0, 0.0, 0.0, 0.2]), ..Default::default() },
    );
    painter.frosted(
        rect,
        Shape {
            border_width: 1.0,
            border: if focused { accent.0.mix(theme.border, 0.35) } else { theme.border },
            glow_radius: if focused { theme.glow * 0.7 } else { 0.0 },
            glow: accent.0.alpha(0.12),
            ..Shape::filled(theme.surface.alpha(0.985), theme.radius)
        },
    );
}

/// A panel's title, with a small dot before it.
/// The administrator's message, paragraph by paragraph, as far as it fits.
fn greeter_draw_motd(
    painter: &mut Painter,
    theme: &crate::theme::Theme,
    greeter: &Greeter,
    rect: Rect,
    lit: Option<Colour>,
) {
    draw_title(painter, theme, rect, &greeter.file.motd.title, lit);
    let inner = body(rect);
    let style = Style::new(theme.font_size, theme.text.alpha(0.86)).lines(12);
    let mut top = inner.y;
    for paragraph in greeter.motd.split("\n\n").map(|raw| raw.split_whitespace().collect::<Vec<_>>().join(" ")) {
        let (_, height) = painter.measure(&paragraph, inner.w, style);
        if paragraph.is_empty() || top + height > inner.bottom() {
            continue;
        }
        painter.text(&paragraph, Rect::new(inner.x, top, inner.w, height), style);
        top += height + 10.0;
    }
}

/// A tile's title after a status dot, lit in `lit` while the tile has focus.
fn draw_title(painter: &mut Painter, theme: &crate::theme::Theme, rect: Rect, title: &str, lit: Option<Colour>) {
    painter.fill(Rect::new(rect.x + 14.0, rect.y + 17.0, 6.0, 6.0), lit.unwrap_or(theme.muted.alpha(0.8)), 3.0);
    painter.text(
        title,
        Rect::new(rect.x + 26.0, rect.y + 8.0, rect.w - 40.0, 24.0),
        Style::new(theme.font_size * 0.95, theme.muted).bold(),
    );
}

const USER: &str = r#"<circle cx="12" cy="8" r="4"/><path d="M4 21a8 8 0 0 1 16 0"/>"#;
const PLUS: &str = r#"<path d="M12 5v14M5 12h14"/>"#;
const CHEVRON_LEFT: &str = r#"<path d="M15 6l-6 6 6 6"/>"#;
const CHEVRON_RIGHT: &str = r#"<path d="M9 6l6 6-6 6"/>"#;

/// People as tiles across `width`, `gap` apart: which of `count` people
/// (someone not listed last, always shown) get one, where each starts and
/// how wide it is. The selected one is [`CARD_GROW`] times as wide as the
/// others; when they would be too narrow, fewer people are shown, around
/// the selected one.
fn card_row(width: f64, gap: f64, count: usize, selected: usize) -> Vec<(usize, f64, f64)> {
    let listed = count.saturating_sub(1);
    let fits = |shown: usize| {
        let others = shown as f64;
        let unit = (width - gap * others) / (others + CARD_GROW);
        unit >= CARD_MIN && unit * CARD_GROW >= CARD_SELECTED_MIN
    };
    let shown = (1..=listed).rev().find(|shown| fits(*shown)).unwrap_or(1.min(listed));
    let people: Vec<usize> = tab_window(listed, selected, shown).chain(std::iter::once(listed)).collect();
    let others = (people.len() - 1) as f64;
    let unit = ((width - gap * others) / (others + CARD_GROW)).max(1.0);
    let mut left = 0.0;
    people
        .into_iter()
        .map(|person| {
            let card = if person == selected { unit * CARD_GROW } else { unit };
            let placed = (person, left, card);
            left += card + gap;
            placed
        })
        .collect()
}

/// Which of `count` people's tabs show when only `fits` do: a run around the
/// selected one, or the last ones while someone not listed is selected.
fn tab_window(count: usize, selected: usize, fits: usize) -> std::ops::Range<usize> {
    if fits >= count {
        return 0..count;
    }
    let first = selected.min(count - 1).saturating_sub(fits / 2).min(count - fits);
    first..first + fits
}

/// Now, broken down in the local time zone.
#[allow(unsafe_code, reason = "the current time comes from libc's time")]
fn local_tm() -> Option<libc::tm> {
    // SAFETY: time accepts a null pointer and only returns the current time.
    local_tm_at(unsafe { libc::time(std::ptr::null_mut()) })
}

/// `timestamp` (seconds since 1970), broken down in the local time zone.
#[allow(unsafe_code, reason = "local time comes from libc's localtime_r")]
fn local_tm_at(timestamp: libc::time_t) -> Option<libc::tm> {
    let mut datetime = std::mem::MaybeUninit::<libc::tm>::uninit();
    // SAFETY: localtime_r reads `timestamp` and fills the caller-owned `datetime`, which lives past the call.
    if unsafe { libc::localtime_r(&timestamp, datetime.as_mut_ptr()) }.is_null() {
        return None;
    }
    // SAFETY: localtime_r succeeded, so `datetime` is initialised.
    Some(unsafe { datetime.assume_init() })
}

/// Now, as `format` (strftime) gives it in the local time zone.
#[allow(unsafe_code, reason = "the clock is formatted by libc's strftime")]
pub fn local_time(format: &str) -> String {
    local_tm().map(|datetime| format_tm(format, &datetime)).unwrap_or_default()
}

/// `datetime` as `format` (strftime) gives it.
#[allow(unsafe_code, reason = "times are formatted by libc's strftime")]
fn format_tm(format: &str, datetime: &libc::tm) -> String {
    let Ok(format) = std::ffi::CString::new(format) else { return String::new() };
    let mut buffer = [0u8; 128];
    // SAFETY: strftime reads the initialised `datetime`, writes at most buffer.len()
    // bytes and returns how many it wrote before the NUL.
    let written = unsafe { libc::strftime(buffer.as_mut_ptr().cast(), buffer.len(), format.as_ptr(), datetime) };
    String::from_utf8_lossy(&buffer[..written.min(buffer.len())]).into_owned()
}

/// When someone last logged in, `timestamp` seconds since 1970, said the way the
/// rest of the sentence "Last login ..." needs: "today at 09:12",
/// "yesterday at 18:02", "3 days ago", "last week" or "on 12 October".
fn last_login(timestamp: i64, clock_format: &str) -> Option<String> {
    let then = local_tm_at(timestamp as libc::time_t)?;
    let days = days_between(&then, &local_tm()?)?;
    let time = format_tm(clock_format, &then);
    describe_login(days, &time, &format_tm("%-d %B", &then), &format_tm("%-d %B %Y", &then))
}

/// Calendar days from `then` to `now`, each taken at local noon so daylight
/// saving changes do not shift a day.
#[allow(unsafe_code, reason = "dates are turned back into times by libc's mktime")]
fn days_between(then: &libc::tm, now: &libc::tm) -> Option<i64> {
    let noon = |datetime: &libc::tm| {
        let mut day = *datetime;
        (day.tm_hour, day.tm_min, day.tm_sec, day.tm_isdst) = (12, 0, 0, -1);
        // SAFETY: mktime reads and normalises the caller-owned copy.
        let timestamp = unsafe { libc::mktime(&mut day) };
        (timestamp != -1).then_some(timestamp)
    };
    Some(((noon(now)? - noon(then)?) as f64 / 86_400.0).round() as i64)
}

fn describe_login(days: i64, time: &str, date: &str, dated: &str) -> Option<String> {
    Some(match days {
        ..0 => return None,
        0 => format!("today at {time}"),
        1 => format!("yesterday at {time}"),
        2..=6 => format!("{days} days ago"),
        7..=13 => "last week".to_string(),
        14..=300 => format!("on {date}"),
        _ => format!("on {dated}"),
    })
}

/// How long since the machine started, suspend included.
#[allow(unsafe_code, reason = "boot time comes from libc's clock_gettime")]
fn uptime() -> Option<std::time::Duration> {
    let mut now = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    // SAFETY: clock_gettime only writes the caller-owned `now`.
    if unsafe { libc::clock_gettime(libc::CLOCK_BOOTTIME, &mut now) } != 0 {
        return None;
    }
    Some(std::time::Duration::from_secs(u64::try_from(now.tv_sec).ok()?))
}

/// The host, how long it has been up and its kernel, such as
/// "workstation", "up 3 d 4 h" and "Linux 6.17.0".
fn machine(hostname: &str, kernel: &str) -> Vec<String> {
    let uptime_text = uptime().map(|elapsed| {
        let minutes = elapsed.as_secs() / 60;
        let (days, hours, minutes) = (minutes / (24 * 60), minutes / 60 % 24, minutes % 60);
        match (days, hours) {
            (0, 0) => format!("up {minutes} min"),
            (0, _) => format!("up {hours} h {minutes} min"),
            _ => format!("up {days} d {hours} h"),
        }
    });
    let kernel = (!kernel.is_empty()).then(|| format!("Linux {kernel}"));
    [Some(hostname.to_string()).filter(|name| !name.is_empty()), uptime_text, kernel].into_iter().flatten().collect()
}

/// The days of `year`'s `month` (0 is January).
fn days_in_month(year: i32, month: i32) -> i32 {
    match month {
        1 if (year % 4 == 0 && year % 100 != 0) || year % 400 == 0 => 29,
        1 => 28,
        3 | 5 | 8 | 10 => 30,
        _ => 31,
    }
}

/// The days of `now`'s week, Monday to Sunday, running into the months
/// either side.
fn week_days(now: &libc::tm) -> [i32; 7] {
    let year = now.tm_year + 1900;
    let length = days_in_month(year, now.tm_mon);
    let before = if now.tm_mon == 0 { 31 } else { days_in_month(year, now.tm_mon - 1) };
    // Columns from Monday; tm_wday counts from Sunday.
    let monday = now.tm_mday - (now.tm_wday + 6) % 7;
    std::array::from_fn(|offset| match monday + offset as i32 {
        day if day < 1 => before + day,
        day if day > length => day - length,
        day => day,
    })
}

/// This week as seven chips, weekday over day, today lit.
fn draw_week(painter: &mut Painter, theme: &crate::theme::Theme, rect: Rect, now: &libc::tm, accent: (Colour, Colour)) {
    let gap = 6.0;
    let width = (rect.w - gap * 6.0) / 7.0;
    let today = ((now.tm_wday + 6) % 7) as usize;
    for (weekday, (day, name)) in week_days(now).iter().zip(["Mo", "Tu", "We", "Th", "Fr", "Sa", "Su"]).enumerate() {
        let chip = Rect::new(rect.x + weekday as f64 * (width + gap), rect.y, width, rect.h);
        if weekday == today {
            painter.shape(
                chip,
                Shape {
                    radius: theme.radius,
                    fill: (accent.0.alpha(0.16), accent.1.alpha(0.06)),
                    ..Default::default()
                },
            );
            painter.fill(Rect::new(chip.x + 4.0, chip.bottom() - 2.0, chip.w - 8.0, 2.0), accent.0, 1.0);
        } else {
            painter.fill(chip, theme.surface_raised.alpha(0.7), theme.radius);
        }
        let colour = if weekday == today { theme.text } else { theme.muted };
        let half = chip.h * 0.42;
        painter.text(
            name,
            Rect::new(chip.x, chip.y + 4.0, chip.w, half),
            Style::new(theme.font_size * 0.8, colour).align(Align::Centre),
        );
        let number = Style::new(
            theme.font_size * 1.35,
            if weekday == today || weekday < 5 { theme.text } else { theme.text.alpha(0.6) },
        );
        painter.text(
            &day.to_string(),
            Rect::new(chip.x, chip.y + half, chip.w, chip.h - half - 4.0),
            if weekday == today { number.bold() } else { number }.align(Align::Centre),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn date_tm(year: i32, month: i32, mday: i32, wday: i32) -> libc::tm {
        // SAFETY: libc::tm is plain data, and all zeroes is a valid value.
        #[allow(unsafe_code, reason = "a zeroed libc::tm for tests")]
        let mut datetime: libc::tm = unsafe { std::mem::zeroed() };
        (datetime.tm_year, datetime.tm_mon, datetime.tm_mday, datetime.tm_wday) = (year - 1900, month, mday, wday);
        datetime
    }

    #[test]
    fn the_built_in_badge_is_the_generated_one() {
        let script = concat!(env!("CARGO_MANIFEST_DIR"), "/../../scripts/session-badge.sh");
        let output = std::process::Command::new("sh").arg(script).output().expect("runs the badge script");
        assert_eq!(output.stdout, TILECEPTION_BADGE, "regenerate resources/session-badge.svg with the script");
    }

    #[test]
    fn the_week_runs_into_the_months_around_it() {
        // Thursday 29 October 2026.
        assert_eq!(week_days(&date_tm(2026, 9, 29, 4)), [26, 27, 28, 29, 30, 31, 1]);
        // Sunday 1 March 2026: the week began in February.
        assert_eq!(week_days(&date_tm(2026, 2, 1, 0)), [23, 24, 25, 26, 27, 28, 1]);
        // Friday 1 January 2027: back into December.
        assert_eq!(week_days(&date_tm(2027, 0, 1, 5)), [28, 29, 30, 31, 1, 2, 3]);
    }

    #[test]
    fn the_tab_strip_keeps_the_selected_person_in_view() {
        assert_eq!(tab_window(3, 0, 5), 0..3);
        assert_eq!(tab_window(10, 0, 4), 0..4);
        assert_eq!(tab_window(10, 5, 4), 3..7);
        assert_eq!(tab_window(10, 9, 4), 6..10);
        // Someone not listed: the last tabs, beside the + tab.
        assert_eq!(tab_window(10, 10, 4), 6..10);
    }

    #[test]
    fn people_as_tiles_fill_the_row_around_the_selected_one() {
        // Three listed and someone not listed, with room for all.
        let row = card_row(1000.0, 10.0, 4, 1);
        assert_eq!(row.iter().map(|(person, _, _)| *person).collect::<Vec<_>>(), [0, 1, 2, 3]);
        let (_, left, width) = row[1];
        assert!((width / row[0].2 - CARD_GROW).abs() < 1e-9);
        assert!((left - row[0].2 - 10.0).abs() < 1e-9);
        let (_, last_left, last_width) = row[3];
        assert!((last_left + last_width - 1000.0).abs() < 1e-9);
        // Too many for the width: fewer, around the selected one, and
        // someone not listed still last.
        let row = card_row(700.0, 10.0, 11, 6);
        let people: Vec<usize> = row.iter().map(|(person, _, _)| *person).collect();
        assert!(people.contains(&6) && people.last() == Some(&10) && people.len() < 11);
        assert!(row.iter().all(|(person, _, width)| *person == 6 || *width >= CARD_MIN));
        // Someone not listed selected: the last listed people beside them.
        let row = card_row(700.0, 10.0, 11, 10);
        assert_eq!(row.last().map(|(person, _, width)| (*person, *width >= CARD_SELECTED_MIN)), Some((10, true)));
    }

    #[test]
    fn logins_are_said_relative_to_today() {
        let say = |days| describe_login(days, "18:02", "12 October", "12 October 2025");
        assert_eq!(say(0).as_deref(), Some("today at 18:02"));
        assert_eq!(say(1).as_deref(), Some("yesterday at 18:02"));
        assert_eq!(say(3).as_deref(), Some("3 days ago"));
        assert_eq!(say(9).as_deref(), Some("last week"));
        assert_eq!(say(20).as_deref(), Some("on 12 October"));
        assert_eq!(say(400).as_deref(), Some("on 12 October 2025"));
        // A clock set back since.
        assert_eq!(say(-1), None);
    }

    #[test]
    fn february_follows_leap_years() {
        assert_eq!(days_in_month(2024, 1), 29);
        assert_eq!(days_in_month(2026, 1), 28);
        assert_eq!(days_in_month(1900, 1), 28);
        assert_eq!(days_in_month(2000, 1), 29);
    }
}
