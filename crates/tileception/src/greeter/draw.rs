//! How the greeter looks, in the desktop's Tide style. One [`Scene`] says
//! where everything goes, for drawing and for clicks alike.

use smithay::backend::renderer::{element::memory::MemoryRenderBuffer, gles::GlesRenderer};
use tileception_core::Rect;

use super::{Action, Auth, Greeter, settings::Panel, settings::WallpaperFallback};
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
const ROW_H: f64 = 38.0;
const AVATAR: f64 = 92.0;
const FIELD_H: f64 = 48.0;
const PALETTE_ROWS: usize = 8;
/// How long the field shakes after a wrong password, in seconds.
const SHAKE: f64 = 0.45;
/// How long a change of person takes to crossfade, in seconds.
const CROSSFADE: f64 = 0.3;

/// What a click on a rectangle does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    Person(usize),
    Session(usize),
    Submit,
    Run(Action),
    Launcher,
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
    panels: Vec<(Panel, Rect)>,
    tabs: Vec<(usize, Rect)>,
    login: Option<LoginLayout>,
    /// Session rows, by session.
    rows: Vec<(usize, Rect)>,
    power: Vec<(Action, Rect)>,
    /// Locked: the one tab, saying whose session it is.
    locked_tab: Option<Rect>,
    /// Locked: what happened while away, and play or pause.
    away: Option<Rect>,
    media: Option<Rect>,
    palette: Option<PaletteLayout>,
}

fn panel_rect(panels: &[(Panel, Rect)], panel: Panel) -> Option<Rect> {
    panels.iter().find(|(p, _)| *p == panel).map(|(_, r)| *r)
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
        let mut scene = Scene {
            screen,
            bar,
            launcher_button,
            panels: panels.clone(),
            tabs: Vec::new(),
            login: None,
            rows: Vec::new(),
            power: Vec::new(),
            locked_tab: None,
            away: None,
            media: None,
            palette: None,
        };
        let Some(g) = self.greeter.as_deref() else { return scene };

        if let Some(login) = panel_rect(&panels, Panel::Login) {
            // One person needs no tabs; someone not listed is in the launcher then.
            let mut top = login.y + 14.0;
            if g.locked() {
                scene.locked_tab = Some(Rect::new(login.x + 6.0, login.y + 6.0, (login.w - 12.0).min(260.0), TAB_H));
                top = login.y + 6.0 + TAB_H + 10.0;
            } else if g.accounts.len() > 1 {
                let strip = Rect::new(login.x + 6.0, login.y + 6.0, login.w - 12.0, TAB_H);
                let plus = 34.0;
                let n = g.accounts.len() as f64;
                let gap = 3.0;
                let w = ((strip.w - plus - gap * n) / n).clamp(1.0, 240.0);
                for i in 0..g.accounts.len() {
                    scene.tabs.push((i, Rect::new(strip.x + i as f64 * (w + gap), strip.y, w, strip.h)));
                }
                scene.tabs.push((g.accounts.len(), Rect::new(strip.right() - plus, strip.y, plus, strip.h)));
                top = strip.bottom() + 10.0;
            }
            let column_h = AVATAR + 14.0 + 30.0 + 22.0 + 22.0 + FIELD_H + 12.0 + 24.0;
            let room = login.bottom() - top;
            let y = top + ((room - column_h) / 2.0).max(0.0);
            let cx = login.x + login.w / 2.0;
            let avatar = Rect::new(cx - AVATAR / 2.0, y, AVATAR, AVATAR);
            let name = Rect::new(login.x + 16.0, avatar.bottom() + 14.0, login.w - 32.0, 30.0);
            let sub = Rect::new(name.x, name.bottom(), name.w, 22.0);
            let w = (login.w - 48.0).clamp(1.0, 360.0);
            let field = Rect::new(cx - w / 2.0, sub.bottom() + 22.0, w, FIELD_H);
            let submit = Rect::new(field.right() - 40.0, field.y + 8.0, 32.0, 32.0);
            let hint = Rect::new(login.x + 16.0, field.bottom() + 12.0, login.w - 32.0, 24.0);
            scene.login = Some(LoginLayout { avatar, name, sub, field, submit, hint });
        }

        if let Some(session) = panel_rect(&panels, Panel::Session)
            && g.locked()
        {
            let inner = body(session);
            scene.away = Some(Rect::new(inner.x, inner.y, inner.w, ROW_H + 8.0));
            if self.media.snapshot.media.active().is_some() {
                let player = Rect::new(inner.x, inner.y + ROW_H + 20.0, inner.w, 64.0);
                scene.media = Some(Rect::new(player.right() - 48.0, player.y + 14.0, 36.0, 36.0));
            }
        } else if let Some(session) = panel_rect(&panels, Panel::Session) {
            let inner = body(session);
            let power_y = inner.bottom() - 36.0;
            for (i, action) in [Action::Suspend, Action::Restart, Action::PowerOff].into_iter().enumerate() {
                scene.power.push((action, Rect::new(inner.x + i as f64 * 44.0, power_y, 36.0, 36.0)));
            }
            let fits = ((power_y - 14.0 - inner.y) / (ROW_H + 4.0)).floor().max(0.0) as usize;
            // The chosen session stays in view when not all fit.
            let chosen = g.session_cursor.max(g.session_of_selected());
            let first = (chosen + 1).saturating_sub(fits);
            for (row, i) in (first..g.sessions.len()).take(fits).enumerate() {
                scene.rows.push((i, Rect::new(inner.x, inner.y + row as f64 * (ROW_H + 4.0), inner.w, ROW_H)));
            }
        }

        if g.palette.is_some() {
            let actions = self.greeter_actions().len();
            let w = (screen.w - 48.0).clamp(1.0, 560.0);
            let shown = actions.clamp(1, PALETTE_ROWS);
            let h = 12.0 + 44.0 + 10.0 + shown as f64 * (ROW_H + 2.0) + 10.0;
            let panel = Rect::new(screen.x + (screen.w - w) / 2.0, screen.y + (screen.h * 0.2).max(BAR_H + 16.0), w, h);
            let field = Rect::new(panel.x + 12.0, panel.y + 12.0, panel.w - 24.0, 44.0);
            let rows = (0..actions.min(PALETTE_ROWS))
                .map(|i| Rect::new(field.x, field.bottom() + 10.0 + i as f64 * (ROW_H + 2.0), field.w, ROW_H))
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
            out.extend(palette.rows.iter().enumerate().map(|(i, r)| (*r, Target::Action(first + i))));
            out.push((palette.panel, Target::Palette));
            return out;
        }
        out.push((scene.launcher_button, Target::Launcher));
        out.extend(scene.tabs.iter().map(|(i, r)| (*r, Target::Person(*i))));
        if let Some(login) = &scene.login {
            out.push((login.submit, Target::Submit));
        }
        out.extend(scene.rows.iter().map(|(i, r)| (*r, Target::Session(*i))));
        out.extend(scene.power.iter().map(|(a, r)| (*r, Target::Run(*a))));
        out.extend(scene.media.map(|r| (r, Target::Run(Action::Media))));
        out.extend(scene.panels.iter().map(|(p, r)| (*r, Target::Panel(*p))));
        out
    }

    pub fn greeter_elements(&self, renderer: &mut GlesRenderer) -> Vec<OutputElement> {
        let Some(g) = self.greeter.as_deref() else { return Vec::new() };
        let mut out = Vec::new();
        // Leaving for the session: everything fades to black, which the
        // session's first frames fade in from.
        if let Some(at) = g.leaving {
            let gone = ease_out(at.elapsed().as_secs_f64() / super::FADE_OUT.as_secs_f64());
            out.push(crate::render::solid(&self.output_rect(), [0.0, 0.0, 0.0, gone as f32], self.scale));
        }
        // Notifications above everything (locked, only Tileception's own).
        out.extend(self.notification_elements(renderer));
        let scene = self.greeter_scene();
        let t = &self.theme;
        let fade = ease_out(g.switched_at.elapsed().as_secs_f64() / CROSSFADE) as f32;
        let accent = (g.previous_accent.0.mix(t.accent, fade), g.previous_accent.1.mix(t.accent_end, fade));
        let mut p = Painter::new(self, renderer);

        self.greeter_draw_backdrop(&mut p, g, scene.screen, accent);
        self.greeter_draw_bar(&mut p, g, &scene);
        for (panel, rect) in &scene.panels {
            let focused = g.focus == *panel && g.palette.is_none();
            draw_frame(&mut p, *rect, focused, accent);
            match panel {
                Panel::Login => self.greeter_draw_login(&mut p, g, &scene, accent),
                Panel::Clock => self.greeter_draw_clock(&mut p, g, *rect),
                Panel::Session if g.locked() => self.greeter_draw_away(&mut p, g, &scene, *rect, accent),
                Panel::Session => self.greeter_draw_sessions(&mut p, g, &scene, *rect, focused, accent),
            }
        }
        if let Some(palette) = &scene.palette {
            self.greeter_draw_palette(&mut p, g, palette, accent);
        }
        out.extend(p.finish());
        out
    }

    /// Between the tiles: a backdrop from the person's accent, or with
    /// per-user looks off, the `[theme]` wallpaper.
    fn greeter_draw_backdrop(&self, p: &mut Painter, g: &Greeter, screen: Rect, accent: (Colour, Colour)) {
        let t = &self.theme;
        let fallback = match g.file.per_user.wallpaper_fallback {
            WallpaperFallback::Generated if !g.file.per_user.wallpaper => WallpaperFallback::System,
            other => other,
        };
        p.fill(screen, t.background, 0.0);
        match fallback {
            WallpaperFallback::Generated => {
                // A soft wash of the person's accent, brightest in two corners.
                p.shape(screen, Shape { fill: (accent.0.alpha(0.10), accent.1.alpha(0.07)), ..Default::default() });
                let spot = (screen.w.min(screen.h) * 0.35).max(1.0);
                let corner = |x: f64, y: f64| Rect::new(x - spot / 2.0, y - spot / 2.0, spot, spot);
                let glow = |colour: Colour| Shape {
                    radius: (spot / 2.0) as f32,
                    fill: (colour.alpha(0.10), colour.alpha(0.10)),
                    glow_radius: (spot * 0.8) as f32,
                    glow: colour.alpha(0.16),
                    ..Default::default()
                };
                p.shape(corner(screen.x + screen.w * 0.12, screen.bottom() - screen.h * 0.1), glow(accent.0));
                p.shape(corner(screen.right() - screen.w * 0.1, screen.y + screen.h * 0.18), glow(accent.1));
            }
            WallpaperFallback::System => {
                if let Some(wallpaper) = &self.wallpaper {
                    p.image(&wallpaper.buffer, (screen.x, screen.y));
                    p.fill(screen, Colour([0.0, 0.0, 0.0, 0.35]), 0.0);
                }
            }
            WallpaperFallback::Flat => {}
        }
        // Shared wallpapers over that: the newly selected person's fades in,
        // over the one before when the person changed.
        if g.file.per_user.wallpaper {
            let fade = ease_out(g.switched_at.elapsed().as_secs_f64() / CROSSFADE) as f32;
            let mut layer = |person: usize, alpha: f32| {
                if let Some(wallpaper) = g.wallpapers.get(&person).filter(|_| alpha > 0.0) {
                    p.alpha = alpha;
                    p.image(wallpaper, (screen.x, screen.y));
                    p.fill(screen, Colour([0.0, 0.0, 0.0, 0.3]), 0.0);
                }
            };
            if g.previous_user != g.user {
                layer(g.previous_user, 1.0 - fade);
            }
            layer(g.user, fade);
            p.alpha = 1.0;
        }
    }

    fn greeter_draw_bar(&self, p: &mut Painter, g: &Greeter, scene: &Scene) {
        let t = &self.theme;
        let bar = scene.bar;
        p.fill(bar, t.surface, 0.0);
        p.fill(Rect::new(bar.x, bar.bottom() - 1.0, bar.w, 1.0), t.border, 0.0);
        // The Tide mark: a tall tile beside two small ones.
        let (x, y, s) = (bar.x + 14.0, bar.y + (BAR_H - 14.0) / 2.0, 6.0);
        p.fill(Rect::new(x, y, s, 14.0), t.accent, 1.5);
        p.fill(Rect::new(x + s + 2.0, y, s, s), t.accent, 1.5);
        p.fill(Rect::new(x + s + 2.0, y + s + 2.0, s, s), t.accent_end, 1.5);
        p.text(&g.hostname, Rect::new(x + 22.0, bar.y, 240.0, BAR_H), Style::new(t.font_size * 0.95, t.muted));
        let now = format!("{} \u{00b7} {}", local_time("%A %-d %B"), local_time(&g.file.clock_format));
        p.text(&now, bar, Style::new(t.font_size, t.text).bold().align(Align::Centre));
        let battery = self.battery.reading.as_ref().map(|r| format!("{}%", r.percent)).unwrap_or_default();
        if !battery.is_empty() {
            let style = Style::new(t.font_size * 0.9, t.muted);
            let (w, _) = p.measure(&battery, 300.0, style);
            p.text(&battery, Rect::new(scene.launcher_button.x - w - 12.0, bar.y, w + 4.0, BAR_H), style);
        }
        let open = g.palette.is_some();
        if open {
            p.fill(scene.launcher_button, t.accent.alpha(0.16), t.radius);
        }
        p.glyph(glyph::POWER, scene.launcher_button, 16, if open { t.accent } else { t.text });
    }

    fn greeter_draw_login(&self, p: &mut Painter, g: &Greeter, scene: &Scene, accent: (Colour, Colour)) {
        let t = &self.theme;
        let strip = scene
            .tabs
            .iter()
            .map(|(_, r)| *r)
            .reduce(|a, b| Rect::new(a.x.min(b.x), a.y, b.right().max(a.right()) - a.x.min(b.x), a.h));
        if let Some(strip) = strip {
            let around = Rect::new(strip.x - 3.0, strip.y - 3.0, strip.w + 6.0, strip.h + 6.0);
            p.fill(around, t.background.alpha(0.5), t.radius);
        }
        for (i, rect) in &scene.tabs {
            let active = *i == g.user;
            let (a, b) = g.accent_of(t, *i);
            if active {
                let tint =
                    Shape { fill: (accent.0.alpha(0.16), accent.0.alpha(0.05)), radius: 4.0, ..Default::default() };
                p.shape(*rect, tint);
                let underline = Rect::new(rect.x + 2.0, rect.bottom() - 2.0, rect.w - 4.0, 2.0);
                p.shape(underline, Shape { radius: 1.0, fill: accent, ..Default::default() });
            }
            let colour = if active { t.text } else { t.muted };
            match g.accounts.get(*i) {
                Some(account) => {
                    let dot = Rect::new(rect.x + 8.0, rect.y + (rect.h - 18.0) / 2.0, 18.0, 18.0);
                    self.greeter_draw_avatar(p, g, *i, dot, (a, b), 0.62);
                    let label = Rect::new(dot.right() + 7.0, rect.y, rect.w - 40.0, rect.h);
                    let style = Style::new(t.font_size * 0.95, colour);
                    p.text(&account.name, label, if active { style.bold() } else { style });
                }
                None => p.glyph(PLUS, *rect, 14, colour),
            }
        }
        if let (Some(tab), Some(account)) = (scene.locked_tab, g.selected()) {
            p.fill(Rect::new(tab.x - 3.0, tab.y - 3.0, tab.w + 6.0, tab.h + 6.0), t.background.alpha(0.5), t.radius);
            p.shape(
                tab,
                Shape { fill: (accent.0.alpha(0.16), accent.0.alpha(0.05)), radius: 4.0, ..Default::default() },
            );
            let underline = Rect::new(tab.x + 2.0, tab.bottom() - 2.0, tab.w - 4.0, 2.0);
            p.shape(underline, Shape { radius: 1.0, fill: accent, ..Default::default() });
            p.glyph(glyph::LOCK, Rect::new(tab.x + 6.0, tab.y, 22.0, tab.h), 14, accent.0);
            let label = format!("{} \u{00b7} locked", account.name);
            p.text(
                &label,
                Rect::new(tab.x + 32.0, tab.y, tab.w - 40.0, tab.h),
                Style::new(t.font_size * 0.95, t.text).bold(),
            );
        }

        let Some(l) = &scene.login else { return };
        let account = g.selected();
        p.shape(
            Rect::new(l.avatar.x - 5.0, l.avatar.y - 5.0, l.avatar.w + 10.0, l.avatar.h + 10.0),
            Shape {
                radius: (l.avatar.w / 2.0 + 5.0) as f32,
                border_width: 1.5,
                border: accent.0.alpha(0.45),
                glow_radius: 18.0,
                glow: accent.0.alpha(0.18),
                ..Default::default()
            },
        );
        self.greeter_draw_avatar(p, g, g.user, l.avatar, accent, 0.42);

        let typed_name = g.name.as_str();
        let name = match account {
            Some(account) => account.name.clone(),
            None if g.named => typed_name.trim().to_string(),
            None => "Someone not listed".to_string(),
        };
        p.text(&name, l.name, Style::new(t.font_size * 1.7, t.text).bold().align(Align::Centre));
        let session = g.sessions.get(g.session_of_selected()).map(|s| s.name.as_str()).unwrap_or("no session");
        let sub = match (&g.auth, account) {
            _ if g.notice.is_some() => g.notice.clone().unwrap_or_default(),
            (Auth::Answering, _) if g.locked() => "Unlocking...".to_string(),
            _ if g.locked() => match &g.mode {
                super::Mode::Lock { locked_at, .. } => format!("Locked at {locked_at}"),
                super::Mode::Login => String::new(),
            },
            (Auth::Asking { text, .. }, _) if !text.is_empty() => text.clone(),
            (Auth::Creating | Auth::Answering, _) => "Logging in...".to_string(),
            (Auth::Starting, _) => format!("Starting {session}..."),
            (_, Some(account)) if account.login != account.name => format!("{} \u{00b7} {session}", account.login),
            (_, Some(_)) => session.to_string(),
            (_, None) if g.named => session.to_string(),
            (_, None) => "Log in with an account that is not listed".to_string(),
        };
        let sub_colour = if g.notice.is_some() { t.danger } else { t.muted };
        p.text(&sub, l.sub, Style::new(t.font_size * 0.95, sub_colour).align(Align::Centre));

        // The field, shaking briefly after a wrong answer.
        let failed_for = g.failed_at.map(|at| at.elapsed().as_secs_f64());
        let shake =
            failed_for.filter(|e| *e < SHAKE).map(|e| (e * 46.0).sin() * 10.0 * (1.0 - e / SHAKE)).unwrap_or(0.0);
        let failed = failed_for.is_some_and(|e| e < 3.0);
        let field = Rect::new(l.field.x + shake, l.field.y, l.field.w, l.field.h);
        let focused = g.focus == Panel::Login && g.palette.is_none();
        let edge = if failed {
            t.danger
        } else if focused {
            accent.0
        } else {
            t.border
        };
        p.shape(
            field,
            Shape {
                border_width: 1.0,
                border: edge,
                glow_radius: if focused || failed { t.glow * 0.8 } else { 0.0 },
                glow: edge.alpha(0.2),
                ..Shape::filled(t.background.alpha(0.9), t.radius)
            },
        );
        let naming = g.other() && !g.named;
        let busy = g.busy();
        let submit = Rect::new(l.submit.x + shake, l.submit.y, l.submit.w, l.submit.h);
        let ready = !busy && (if naming { !g.name.is_empty() } else { !g.field.is_empty() });
        if ready {
            p.shape(
                submit,
                Shape { radius: t.radius, fill: (accent.0.alpha(0.9), accent.1.alpha(0.9)), ..Default::default() },
            );
        } else {
            p.fill(submit, t.surface_raised, t.radius);
        }
        p.glyph(glyph::ARROW_RIGHT, submit, 16, if ready { t.on_accent } else { t.muted });

        let inner = Rect::new(field.x + 16.0, field.y, submit.x - field.x - 24.0, field.h);
        let caret_on = focused && !busy && g.typed_at.elapsed().as_millis() % 1000 < 600;
        let since = self.start.elapsed().as_secs_f64();
        if naming {
            let style = Style::new(t.font_size * 1.1, t.text);
            if typed_name.is_empty() {
                p.text("Username", inner, Style { colour: t.muted, ..style });
            }
            let (w, _) = p.text(typed_name, inner, style);
            if caret_on {
                p.fill(Rect::new(inner.x + w + 1.0, field.y + 14.0, 2.0, field.h - 28.0), accent.0, 1.0);
            }
        } else {
            let pitch = 15.0;
            let room = ((inner.w - 8.0) / pitch).floor().max(1.0) as usize;
            let count = g.field.chars().min(room);
            let shown = if busy { room.min(6) } else { count };
            if shown == 0 {
                let placeholder = match &g.auth {
                    Auth::Asking { text, .. } if !text.is_empty() => text.trim_end_matches(':').to_string(),
                    _ => "Password".to_string(),
                };
                p.text(&placeholder, inner, Style::new(t.font_size * 1.1, t.muted));
            }
            let visible = matches!(g.auth, Auth::Asking { prompt: super::greetd::Prompt::Visible, .. });
            if visible && !busy {
                // A question whose answer may be shown, such as a one-time code.
                let (w, _) = p.text(g.field.as_str(), inner, Style::new(t.font_size * 1.1, t.text));
                if caret_on {
                    p.fill(Rect::new(inner.x + w + 1.0, field.y + 14.0, 2.0, field.h - 28.0), accent.0, 1.0);
                }
            } else {
                for i in 0..shown {
                    let alpha = if busy { 0.3 + 0.7 * (0.5 + 0.5 * (since * 7.0 - i as f64 * 0.7).sin()) } else { 1.0 };
                    let dot = Rect::new(inner.x + i as f64 * pitch + 3.5, field.y + field.h / 2.0 - 4.0, 8.0, 8.0);
                    p.fill(dot, t.text.alpha(alpha as f32), 4.0);
                }
                if caret_on {
                    let x = inner.x + shown as f64 * pitch + 2.0;
                    p.fill(Rect::new(x, field.y + 14.0, 2.0, field.h - 28.0), accent.0, 1.0);
                }
            }
        }

        // Caps Lock first: it spoils passwords.
        if g.caps_lock && !naming {
            let style = Style::new(t.font_size * 0.9, t.warning).bold();
            let (w, _) = p.measure("Caps Lock is on", 400.0, style);
            let chip = Rect::new(l.hint.x + (l.hint.w - w - 46.0) / 2.0, l.hint.y - 2.0, w + 46.0, 28.0);
            p.shape(
                chip,
                Shape { border_width: 1.0, border: t.warning.alpha(0.5), ..Shape::filled(t.warning.alpha(0.12), 14.0) },
            );
            p.glyph(glyph::CAPS, Rect::new(chip.x + 8.0, chip.y, 20.0, chip.h), 14, t.warning);
            p.text("Caps Lock is on", Rect::new(chip.x + 32.0, chip.y, w + 4.0, chip.h), style);
            return;
        }
        let super_key = if self.nested { "Alt" } else { "Super" };
        let tab = format!("{super_key}+Tab");
        let hints: Vec<(&str, &str)> = if busy {
            Vec::new()
        } else if g.locked() {
            vec![("Enter", "unlock"), (super_key, "actions")]
        } else if naming {
            vec![("Enter", "continue"), (super_key, "actions")]
        } else if g.other() {
            vec![("Enter", "log in"), ("Esc", "change username")]
        } else if g.accounts.len() > 1 {
            vec![(tab.as_str(), "next user"), (super_key, "actions")]
        } else {
            vec![("Enter", "log in"), (super_key, "actions")]
        };
        let width = p.key_hints(&hints, 0.0, 0.0, true);
        p.key_hints(&hints, l.hint.x + (l.hint.w - width) / 2.0, l.hint.y + l.hint.h / 2.0, false);
    }

    /// An avatar picture, or the initial on the accent gradient.
    fn greeter_draw_avatar(
        &self,
        p: &mut Painter,
        g: &Greeter,
        index: usize,
        rect: Rect,
        accent: (Colour, Colour),
        text: f32,
    ) {
        let t = &self.theme;
        let account = g.accounts.get(index);
        if let Some(picture) = account.and_then(|a| self.greeter_avatar_buffer(g, a, rect.w)) {
            p.image(&picture, (rect.x, rect.y));
            return;
        }
        p.shape(rect, Shape { radius: (rect.w / 2.0) as f32, fill: accent, ..Default::default() });
        match account {
            Some(account) => {
                let initial: String = account.name.chars().take(1).collect::<String>().to_uppercase();
                let style = Style::new((rect.h as f32 * text).max(6.0), t.on_accent).bold().align(Align::Centre);
                p.text(&initial, rect, style);
            }
            None => p.glyph(USER, rect, (rect.w * 0.5).round().max(8.0) as u32, t.on_accent),
        }
    }

    fn greeter_avatar_buffer(&self, g: &Greeter, account: &super::Account, size: f64) -> Option<MemoryRenderBuffer> {
        let path = account.icon.as_ref()?;
        let pixels = (size * self.scale).round().max(1.0) as u32;
        let key = (account.login.clone(), pixels);
        g.avatars
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

    fn greeter_draw_clock(&self, p: &mut Painter, g: &Greeter, rect: Rect) {
        let t = &self.theme;
        draw_title(p, t, rect, "Clock");
        let inner = body(rect);
        let size = (t.font_size * 5.6).min((inner.h * 0.42) as f32).max(t.font_size * 1.6);
        let time = Rect::new(inner.x, inner.y, inner.w, f64::from(size) * 1.25);
        p.text(&local_time(&g.file.clock_format), time, Style::new(size, t.text).weight(300));
        let date = Rect::new(inner.x, time.bottom(), inner.w, 24.0);
        p.text(&local_time("%A %-d %B"), date, Style::new(t.font_size * 1.05, t.muted));
        let battery = self.battery.reading.as_ref().map(|r| r.describe()).unwrap_or_default();
        if !battery.is_empty() && date.bottom() + 34.0 < rect.bottom() {
            p.text(
                &battery,
                Rect::new(inner.x, date.bottom() + 10.0, inner.w, 22.0),
                Style::new(t.font_size * 0.9, t.muted),
            );
        }
    }

    /// Locked: counts of what came in, and the player without its title.
    fn greeter_draw_away(&self, p: &mut Painter, g: &Greeter, scene: &Scene, rect: Rect, accent: (Colour, Colour)) {
        let t = &self.theme;
        draw_title(p, t, rect, "While you were away");
        if let (Some(row), Some((total, _))) = (scene.away, self.notification_status()) {
            let before = match &g.mode {
                super::Mode::Lock { notifications, .. } => *notifications,
                super::Mode::Login => total,
            };
            let count = total.saturating_sub(before);
            p.fill(row, t.surface_raised.alpha(0.6), t.radius);
            p.glyph(
                glyph::BELL,
                Rect::new(row.x + 6.0, row.y, 28.0, row.h),
                16,
                if count > 0 { accent.0 } else { t.muted },
            );
            let label = match count {
                0 => "No new notifications".to_string(),
                1 => "1 notification".to_string(),
                n => format!("{n} notifications"),
            };
            p.text(&label, Rect::new(row.x + 42.0, row.y, row.w - 180.0, row.h), Style::new(t.font_size, t.text));
            if count > 0 {
                let style = Style::new(t.font_size * 0.8, t.muted);
                let (w, _) = p.measure("hidden until unlock", 300.0, style);
                p.text("hidden until unlock", Rect::new(row.right() - w - 12.0, row.y, w + 2.0, row.h), style);
            }
        }
        let (Some(button), Some(player)) = (scene.media, self.media.snapshot.media.active()) else { return };
        let panel = Rect::new(rect.x + 14.0, button.y - 14.0, rect.w - 28.0, 64.0);
        p.fill(panel, t.surface_raised.alpha(0.6), t.radius);
        let art = Rect::new(panel.x + 12.0, panel.y + 12.0, 40.0, 40.0);
        p.shape(
            art,
            Shape { radius: t.radius, fill: (accent.0.alpha(0.5), accent.1.alpha(0.5)), ..Default::default() },
        );
        p.glyph(glyph::MUSIC, art, 18, t.on_accent);
        let playing = player.status == tileception_media::Status::Playing;
        let text = Rect::new(art.right() + 12.0, panel.y + 12.0, button.x - art.right() - 24.0, 20.0);
        p.text(if playing { "Playing" } else { "Paused" }, text, Style::new(t.font_size, t.text).bold());
        let below = Rect::new(text.x, text.bottom(), text.w, 20.0);
        p.text("Title hidden until unlock", below, Style::new(t.font_size * 0.85, t.muted));
        p.shape(button, Shape { border_width: 1.0, border: t.border, ..Shape::filled(t.surface_raised, t.radius) });
        p.glyph(if playing { glyph::PAUSE } else { glyph::PLAY }, button, 16, t.text);
    }

    fn greeter_draw_sessions(
        &self,
        p: &mut Painter,
        g: &Greeter,
        scene: &Scene,
        rect: Rect,
        focused: bool,
        accent: (Colour, Colour),
    ) {
        let t = &self.theme;
        draw_title(p, t, rect, "Session");
        let current = g.session_of_selected();
        let last = g.selected().and_then(|a| g.last_session(&a.login));
        if g.sessions.is_empty() {
            p.text("No sessions installed", body(rect), Style::new(t.font_size, t.muted).align(Align::Centre));
        }
        for (i, row) in &scene.rows {
            let Some(session) = g.sessions.get(*i) else { continue };
            let on = *i == current;
            if on {
                p.shape(
                    *row,
                    Shape {
                        fill: (accent.0.alpha(0.14), accent.0.alpha(0.04)),
                        radius: t.radius,
                        ..Default::default()
                    },
                );
            }
            if focused && *i == g.session_cursor {
                p.shape(
                    *row,
                    Shape { border_width: 1.0, border: accent.0.alpha(0.7), radius: t.radius, ..Default::default() },
                );
            }
            let mark = Rect::new(row.x + 10.0, row.y + (row.h - 20.0) / 2.0, 20.0, 20.0);
            if session.id.starts_with("tileception") {
                p.shape(mark, Shape { radius: 4.0, fill: accent, ..Default::default() });
                p.glyph(glyph::OVERVIEW, mark, 14, t.on_accent);
            } else {
                p.fill(mark, t.surface_raised, 4.0);
            }
            let colour = if on { t.text } else { t.muted };
            let label = Rect::new(mark.right() + 10.0, row.y, row.w - 130.0, row.h);
            p.text(&session.name, label, Style::new(t.font_size, colour));
            if Some(*i) == last {
                let style = Style::new(t.font_size * 0.8, t.muted);
                let (w, _) = p.measure("last used", 200.0, style);
                p.text("last used", Rect::new(row.right() - w - 12.0, row.y, w + 2.0, row.h), style);
            }
        }
        if let Some((_, first)) = scene.power.first() {
            p.fill(Rect::new(first.x, first.y - 10.0, body(rect).w, 1.0), t.text.alpha(0.06), 0.0);
        }
        for (action, button) in &scene.power {
            p.shape(
                *button,
                Shape { border_width: 1.0, border: t.border, ..Shape::filled(t.surface_raised, t.radius) },
            );
            let icon = match action {
                Action::Suspend => glyph::MOON,
                Action::Restart => glyph::RESTART,
                _ => glyph::POWER,
            };
            p.glyph(icon, *button, 16, t.muted);
        }
        if let Some((_, last)) = scene.power.last() {
            let style = Style::new(t.font_size * 0.8, t.muted);
            let text = "more in the launcher";
            let (w, _) = p.measure(text, 300.0, style);
            let right = body(rect).right();
            if right - w > last.right() + 12.0 {
                p.text(text, Rect::new(right - w - 2.0, last.y, w + 4.0, last.h), style);
            }
        }
    }

    fn greeter_draw_palette(&self, p: &mut Painter, g: &Greeter, palette: &PaletteLayout, accent: (Colour, Colour)) {
        let t = &self.theme;
        let Some(state) = &g.palette else { return };
        let opened = state.opened.map_or(1.0, |at| ease_out(at.elapsed().as_secs_f64() / 0.16));
        p.alpha = opened as f32;
        p.fill(self.output_rect(), Colour([0.0, 0.0, 0.0, 0.32]), 0.0);
        p.offset = (0.0, (opened - 1.0) * 12.0);
        p.popup(palette.panel);
        p.shape(
            palette.field,
            Shape {
                border_width: 1.0,
                border: accent.0.alpha(0.45),
                glow_radius: t.glow * 0.6,
                glow: accent.0.alpha(0.12),
                ..Shape::filled(t.background, t.radius)
            },
        );
        let typing = !state.query.is_empty();
        p.glyph(
            glyph::SEARCH,
            Rect::new(palette.field.x + 8.0, palette.field.y, 28.0, palette.field.h),
            18,
            if typing { accent.0 } else { t.muted },
        );
        let text = Rect::new(palette.field.x + 42.0, palette.field.y, palette.field.w - 54.0, palette.field.h);
        let style = Style::new(t.font_size * 1.15, t.text);
        let (w, _) = if typing {
            p.text(&state.query, text, style)
        } else {
            p.text(
                if g.locked() { "Power and media" } else { "Power, sessions, people" },
                text,
                Style { colour: t.muted, ..style },
            );
            (0.0, 0.0)
        };
        if self.start.elapsed().as_millis() % 1000 < 600 {
            p.fill(Rect::new(text.x + w + 1.0, text.y + 12.0, 2.0, text.h - 24.0), accent.0, 1.0);
        }
        let actions = self.greeter_actions();
        if actions.is_empty() {
            let row = Rect::new(palette.field.x, palette.field.bottom() + 10.0, palette.field.w, ROW_H);
            p.text("No matches", row, Style::new(t.font_size, t.muted).align(Align::Centre));
        }
        let first = palette_first(g);
        for (i, row) in palette.rows.iter().enumerate() {
            let Some((action, label)) = actions.get(first + i) else { break };
            let selected = first + i == state.selected;
            if selected {
                p.shape(
                    *row,
                    Shape {
                        fill: (accent.0.alpha(0.16), accent.0.alpha(0.05)),
                        radius: t.radius,
                        ..Default::default()
                    },
                );
            }
            let icon = match action {
                Action::Media => glyph::MUSIC,
                Action::Suspend => glyph::MOON,
                Action::Restart | Action::Firmware => glyph::RESTART,
                Action::PowerOff => glyph::POWER,
                Action::Session(_) => glyph::OVERVIEW,
                Action::Person(_) => USER,
            };
            p.glyph(icon, Rect::new(row.x + 6.0, row.y, 28.0, row.h), 16, if selected { accent.0 } else { t.muted });
            p.text(
                label,
                Rect::new(row.x + 42.0, row.y, row.w - 110.0, row.h),
                Style::new(t.font_size, if selected { t.text } else { t.muted }),
            );
            if selected {
                let style = Style::new(t.font_size * 0.8, t.muted);
                let (w, _) = p.measure("Enter", 100.0, style);
                p.text("Enter", Rect::new(row.right() - w - 12.0, row.y, w + 2.0, row.h), style);
            }
        }
        p.alpha = 1.0;
        p.offset = (0.0, 0.0);
    }
}

/// The first launcher row shown, so the selected one is in view.
fn palette_first(g: &Greeter) -> usize {
    let selected = g.palette.as_ref().map_or(0, |p| p.selected);
    (selected + 1).saturating_sub(PALETTE_ROWS)
}

/// A tile, as windows are framed on the desktop.
fn draw_frame(p: &mut Painter, rect: Rect, focused: bool, accent: (Colour, Colour)) {
    let t = p.theme();
    p.shape(
        Rect::new(rect.x, rect.y + 4.0, rect.w, rect.h),
        Shape { radius: t.radius, glow_radius: 14.0, glow: Colour([0.0, 0.0, 0.0, 0.2]), ..Default::default() },
    );
    p.shape(
        rect,
        Shape {
            border_width: 1.0,
            border: if focused { accent.0.mix(t.border, 0.35) } else { t.border },
            glow_radius: if focused { t.glow * 0.7 } else { 0.0 },
            glow: accent.0.alpha(0.12),
            ..Shape::filled(t.surface.alpha(0.985), t.radius)
        },
    );
}

/// A panel's title, with a small dot before it.
fn draw_title(p: &mut Painter, t: &crate::theme::Theme, rect: Rect, title: &str) {
    p.fill(Rect::new(rect.x + 14.0, rect.y + 17.0, 6.0, 6.0), t.muted.alpha(0.8), 3.0);
    p.text(
        title,
        Rect::new(rect.x + 26.0, rect.y + 8.0, rect.w - 40.0, 24.0),
        Style::new(t.font_size * 0.95, t.muted).bold(),
    );
}

const USER: &str = r#"<circle cx="12" cy="8" r="4"/><path d="M4 21a8 8 0 0 1 16 0"/>"#;
const PLUS: &str = r#"<path d="M12 5v14M5 12h14"/>"#;

/// Now, as `format` (strftime) gives it in the local time zone.
#[allow(unsafe_code, reason = "the clock is formatted by libc's strftime in the local time zone")]
pub fn local_time(format: &str) -> String {
    let Ok(format) = std::ffi::CString::new(format) else { return String::new() };
    // SAFETY: time accepts a null pointer and only returns the current time.
    let now = unsafe { libc::time(std::ptr::null_mut()) };
    let mut tm = std::mem::MaybeUninit::<libc::tm>::uninit();
    // SAFETY: localtime_r reads `now` and fills the caller-owned `tm`, which lives past the call.
    if unsafe { libc::localtime_r(&now, tm.as_mut_ptr()) }.is_null() {
        return String::new();
    }
    let mut buffer = [0u8; 128];
    // SAFETY: localtime_r succeeded, so `tm` is initialised. strftime writes at
    // most buffer.len() bytes and returns how many it wrote before the NUL.
    let written = unsafe { libc::strftime(buffer.as_mut_ptr().cast(), buffer.len(), format.as_ptr(), tm.as_ptr()) };
    String::from_utf8_lossy(&buffer[..written.min(buffer.len())]).into_owned()
}
