//! App search, icon lookup and a small persistent usage history.

use std::{
    collections::HashMap,
    path::PathBuf,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};
use smithay::{
    backend::renderer::gles::GlesRenderer,
    input::keyboard::{keysyms, xkb},
};
use tileception_apps::{AppIndex, IconLookup, Usage};
use tileception_core::{Point, Rect};

use smithay::wayland::selection::{
    data_device::{
        current_data_device_selection_userdata, request_data_device_client_selection, set_data_device_selection,
        SelectionRequestError,
    },
    SelectionTarget,
};

use crate::{
    render::OutputElement,
    shapes::Shape,
    theme::Colour,
    ui::{ease_out, glyph, Align, Icons, Painter, Style},
    Tileception,
};

/// Clipboard contents the compositor owns itself: launcher text, or a
/// screenshot from `tileception msg copy-image`.
#[derive(Debug, Clone)]
pub enum Clip {
    Text(String),
    Png(std::sync::Arc<[u8]>),
}

impl Clip {
    pub fn text(&self) -> Option<&str> {
        match self {
            Clip::Text(text) => Some(text),
            Clip::Png(_) => None,
        }
    }

    /// Serves the contents without starting a Wayland client.
    pub fn send(&self, fd: std::os::fd::OwnedFd) {
        use std::io::Write;
        let clip = self.clone();
        std::thread::spawn(move || {
            let bytes = match &clip {
                Clip::Text(text) => text.as_bytes(),
                Clip::Png(png) => png,
            };
            let _ = std::fs::File::from(fd).write_all(bytes);
        });
    }
}

#[derive(Default, Serialize, Deserialize)]
struct StoredUsage {
    count: u32,
    last_used: u64,
}

pub struct Launcher {
    pub open: bool,
    input: crate::text_input::TextInput,
    edited: Instant,
    opened: Instant,
    selecting: bool,
    clipboard: Option<smithay::reexports::calloop::channel::Sender<(u64, String)>>,
    epoch: u64,
    selected: usize,
    index: AppIndex,
    lookup: IconLookup,
    pub icons: Icons,
    usage: HashMap<String, Usage>,
    compose: Option<xkb::compose::State>,
}

impl Default for Launcher {
    fn default() -> Self {
        let usage: HashMap<String, StoredUsage> =
            std::fs::read_to_string(usage_path()).ok().and_then(|s| toml::from_str(&s).ok()).unwrap_or_default();
        Self {
            open: false,
            input: Default::default(),
            edited: Instant::now(),
            opened: Instant::now(),
            selecting: false,
            clipboard: None,
            epoch: 0,
            selected: 0,
            index: AppIndex::scan_default(),
            lookup: IconLookup::new("hicolor", &IconLookup::default_dirs()),
            icons: Icons::default(),
            compose: crate::ui::compose_state(),
            usage: usage
                .into_iter()
                .map(|(id, value)| {
                    (id, Usage { count: value.count, last_used: UNIX_EPOCH + Duration::from_secs(value.last_used) })
                })
                .collect(),
        }
    }
}

impl Launcher {
    fn matches(&self) -> Vec<&tileception_apps::App> {
        self.index.search(&self.input.text, &self.usage, 8)
    }

    pub fn app_icon(
        &self,
        app_id: &str,
        size: u32,
    ) -> Option<smithay::backend::renderer::element::memory::MemoryRenderBuffer> {
        let desktop_id = if app_id.ends_with(".desktop") { app_id.to_owned() } else { format!("{app_id}.desktop") };
        let icon = self.index.get(&desktop_id).and_then(|app| app.icon.as_deref()).unwrap_or(app_id);
        let path = self.lookup.find(icon, size, 1)?;
        self.icons.image(&path, size)
    }
}

impl Tileception {
    /// Puts a PNG file on the clipboard, owned by the compositor, so copying a
    /// screenshot needs no helper window.
    pub fn copy_image(&mut self, path: &std::path::Path) -> Result<(), String> {
        use std::io::Read;
        let mut png = Vec::new();
        std::fs::File::open(path)
            .and_then(|f| f.take(MAX_IMAGE + 1).read_to_end(&mut png))
            .map_err(|e| format!("{}: {e}", path.display()))?;
        if png.len() as u64 > MAX_IMAGE || !png.starts_with(b"\x89PNG") {
            return Err(format!("{} is not a PNG of at most 64 MB", path.display()));
        }
        let mime_types = vec!["image/png".to_string()];
        set_data_device_selection(&self.display_handle, &self.seat, mime_types.clone(), Some(Clip::Png(png.into())));
        if let Some(xwm) = self.xwm.as_mut() {
            if let Err(error) = xwm.new_selection(SelectionTarget::Clipboard, Some(mime_types)) {
                tracing::warn!("could not offer the image to X11: {error:?}");
            }
        }
        Ok(())
    }

    pub fn init_launcher(&mut self) {
        use smithay::reexports::calloop::channel;
        let (sender, receiver) = channel::channel::<(u64, String)>();
        self.launcher.clipboard = Some(sender);
        self.loop_handle
            .insert_source(receiver, |event, _, state| {
                if let channel::Event::Msg((epoch, text)) = event {
                    if state.launcher.open && state.launcher.epoch == epoch {
                        state.launcher_insert_paste(&text);
                        state.schedule_render();
                    }
                }
            })
            .expect("clipboard event source");
    }

    pub fn toggle_launcher(&mut self) {
        if self.is_locked() {
            return;
        }
        if self.launcher.open {
            self.close_launcher();
            return;
        }
        if self.launcher.index.changed_since_scan() {
            self.launcher.index.refresh();
        }
        self.power_menu = None;
        self.launcher.open = true;
        self.launcher.selecting = false;
        self.launcher.epoch = self.launcher.epoch.wrapping_add(1);
        self.launcher.input.clear();
        self.launcher.edited = Instant::now();
        self.launcher.opened = Instant::now();
        if let Some(compose) = &mut self.launcher.compose {
            compose.reset();
        }
        self.launcher.selected = 0;
        self.force_keyboard_focus_sync();
        let pointer = self.seat.get_pointer().expect("seat has a pointer");
        let serial = smithay::utils::SERIAL_COUNTER.next_serial();
        pointer.motion(self, None, &smithay::input::pointer::MotionEvent { location: self.pointer, serial, time: 0 });
        pointer.frame(self);
    }

    pub fn close_launcher(&mut self) {
        self.launcher.open = false;
        self.launcher.selecting = false;
        self.force_keyboard_focus_sync();
    }

    pub fn launcher_key(&mut self, sym: u32, mods: crate::config::Mods) {
        self.launcher.edited = Instant::now();
        let word = mods.ctrl || mods.alt;
        match sym {
            keysyms::KEY_Escape => self.close_launcher(),
            keysyms::KEY_Return | keysyms::KEY_KP_Enter => self.launch_app(),
            keysyms::KEY_Up | keysyms::KEY_ISO_Left_Tab => self.launcher_step(-1),
            keysyms::KEY_Down | keysyms::KEY_Tab => self.launcher_step(1),
            keysyms::KEY_Left => self.launcher.input.move_horizontal(false, word, mods.shift),
            keysyms::KEY_Right => self.launcher.input.move_horizontal(true, word, mods.shift),
            keysyms::KEY_Home => self.launcher.input.move_to(0, mods.shift),
            keysyms::KEY_End => self.launcher.input.move_to(self.launcher.input.text.len(), mods.shift),
            keysyms::KEY_BackSpace => {
                self.launcher.input.delete(false, word);
                self.launcher.selected = 0;
            }
            keysyms::KEY_Delete => {
                self.launcher.input.delete(true, word);
                self.launcher.selected = 0;
            }
            _ if mods.ctrl && matches!(sym, keysyms::KEY_a | keysyms::KEY_A) => self.launcher.input.select_all(),
            _ if mods.ctrl && matches!(sym, keysyms::KEY_c | keysyms::KEY_C | keysyms::KEY_x | keysyms::KEY_X) => {
                let range = self.launcher.input.selection();
                if !range.is_empty() {
                    let text = self.launcher.input.text[range].to_owned();
                    let mime_types = vec!["text/plain;charset=utf-8".into(), "text/plain".into()];
                    set_data_device_selection(
                        &self.display_handle,
                        &self.seat,
                        mime_types.clone(),
                        Some(Clip::Text(text)),
                    );
                    if let Some(xwm) = self.xwm.as_mut() {
                        if let Err(error) = xwm.new_selection(SelectionTarget::Clipboard, Some(mime_types)) {
                            tracing::warn!("could not offer launcher text to X11: {error:?}");
                        }
                    }
                    if matches!(sym, keysyms::KEY_x | keysyms::KEY_X) {
                        self.launcher.input.insert("");
                        self.launcher.selected = 0;
                    }
                }
            }
            _ if mods.ctrl && matches!(sym, keysyms::KEY_v | keysyms::KEY_V) => self.launcher_paste(),
            _ if mods.ctrl && matches!(sym, keysyms::KEY_u | keysyms::KEY_U) => {
                self.launcher.input.clear();
                self.launcher.selected = 0;
            }
            _ if !mods.ctrl && !mods.logo && !mods.alt => {
                if let Some(text) = crate::ui::compose_text(&mut self.launcher.compose, sym) {
                    self.launcher.input.insert(&text);
                    self.launcher.selected = 0;
                }
            }
            _ => {}
        }
    }

    fn launcher_paste(&mut self) {
        use std::os::unix::net::UnixStream;
        match current_data_device_selection_userdata(&self.seat).and_then(|data| data.clone()) {
            Some(Clip::Text(text)) => {
                self.launcher_insert_paste(&text);
                return;
            }
            Some(Clip::Png(_)) => return,
            None => {}
        }
        let Some(sender) = self.launcher.clipboard.clone() else { return };
        let epoch = self.launcher.epoch;
        let Ok((reader, writer)) = UnixStream::pair() else { return };
        // Bound stalled or oversized transfers without blocking the compositor.
        if reader.set_read_timeout(Some(Duration::from_secs(3))).is_err() {
            return;
        }
        let mut requested = false;
        for mime_type in ["text/plain;charset=utf-8", "text/plain", "UTF8_STRING", "STRING"] {
            let Ok(fd) = writer.try_clone() else { return };
            match request_data_device_client_selection(&self.seat, mime_type.into(), fd.into()) {
                Ok(()) => {
                    requested = true;
                    break;
                }
                Err(SelectionRequestError::InvalidMimetype) => continue,
                Err(SelectionRequestError::ServerSideSelection) => {
                    if let Some(xwm) = self.xwm.as_mut() {
                        requested = xwm
                            .send_selection(
                                SelectionTarget::Clipboard,
                                mime_type.into(),
                                writer.into(),
                                self.loop_handle.clone(),
                            )
                            .is_ok();
                    }
                    // This writer is transferred to Xwayland or dropped here.
                    if requested {
                        self.launcher_read_paste(reader, sender, epoch);
                    }
                    return;
                }
                Err(SelectionRequestError::NoSelection) => return,
            }
        }
        drop(writer);
        if requested {
            self.launcher_read_paste(reader, sender, epoch);
        }
    }

    fn launcher_read_paste(
        &self,
        reader: std::os::unix::net::UnixStream,
        sender: smithay::reexports::calloop::channel::Sender<(u64, String)>,
        epoch: u64,
    ) {
        use std::io::Read;
        std::thread::spawn(move || {
            let mut text = String::new();
            if reader.take(4097).read_to_string(&mut text).is_ok() && text.len() <= 4096 {
                let _ = sender.send((epoch, text));
            }
        });
    }

    fn launcher_insert_paste(&mut self, text: &str) {
        let text: String = text.chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
        self.launcher.input.insert(&text);
        self.launcher.edited = Instant::now();
        self.launcher.selected = 0;
    }

    pub fn launcher_step(&mut self, delta: isize) {
        let len = self.launcher.matches().len();
        if len > 0 {
            self.launcher.selected = (self.launcher.selected as isize + delta).rem_euclid(len as isize) as usize;
        }
    }

    fn launch_app(&mut self) {
        let Some((id, command)) = self
            .launcher
            .matches()
            .get(self.launcher.selected)
            .map(|app| (app.id.clone(), tileception_apps::command_line(app, &format!("{} -e", self.terminal))))
        else {
            return;
        };
        self.close_launcher();
        self.spawn("sh", &["-c".into(), command]);
        let entry = self.launcher.usage.entry(id).or_insert(Usage { count: 0, last_used: UNIX_EPOCH });
        entry.count = entry.count.saturating_add(1);
        entry.last_used = SystemTime::now();
        let stored: HashMap<_, _> = self
            .launcher
            .usage
            .iter()
            .map(|(id, u)| {
                (
                    id.clone(),
                    StoredUsage {
                        count: u.count,
                        last_used: u.last_used.duration_since(UNIX_EPOCH).unwrap_or_default().as_secs(),
                    },
                )
            })
            .collect();
        if let Ok(data) = toml::to_string(&stored) {
            let path = usage_path();
            if let Some(parent) = path.parent() {
                if let Err(error) = std::fs::create_dir_all(parent).and_then(|_| std::fs::write(&path, data)) {
                    tracing::warn!("could not save launcher usage: {error}");
                }
            }
        }
    }

    /// Where everything in the launcher goes, for drawing and for the pointer.
    fn launcher_layout(&self, results: usize) -> LauncherLayout {
        let screen = self.output_rect();
        let w = (screen.w - 32.0).clamp(1.0, PANEL_W);
        let list_h = if results == 0 { 2.0 * ROW_H } else { results as f64 * ROW_H };
        let h = PAD + FIELD_H + CAPTION_H + list_h + PAD + FOOTER_H;
        // A fixed top edge, so the field stays put while results come and go.
        let top = (screen.y + screen.h * 0.18).min(screen.bottom() - 16.0 - h).max(screen.y + 16.0);
        let panel = Rect::new(screen.x + (screen.w - w) / 2.0, top, w, h);
        let field = Rect::new(panel.x + PAD, panel.y + PAD, panel.w - 2.0 * PAD, FIELD_H);
        let caption = Rect::new(panel.x + PAD + 6.0, field.bottom(), panel.w - 2.0 * PAD - 12.0, CAPTION_H);
        let rows = (0..results)
            .map(|i| Rect::new(panel.x + PAD, caption.bottom() + i as f64 * ROW_H, panel.w - 2.0 * PAD, ROW_H - 2.0))
            .collect();
        let footer = Rect::new(panel.x, panel.bottom() - FOOTER_H, panel.w, FOOTER_H);
        let text = Rect::new(field.x + 44.0, field.y, field.w - 58.0, field.h);
        LauncherLayout { panel, field, text, caption, rows, footer }
    }

    pub fn launcher_at(&self, point: Point) -> Option<usize> {
        let layout = self.launcher_layout(self.launcher.matches().len());
        layout.rows.iter().position(|row| row.contains(point))
    }

    pub fn launcher_motion(&mut self, point: Point) {
        if self.launcher.selecting {
            self.launcher_cursor_at(point, true);
            return;
        }
        if let Some(i) = self.launcher_at(point) {
            self.launcher.selected = i;
        }
    }

    pub fn launcher_click(&mut self, point: Point) {
        let layout = self.launcher_layout(self.launcher.matches().len());
        if layout.field.contains(point) {
            let extend = self.seat.get_keyboard().expect("seat has a keyboard").modifier_state().shift;
            self.launcher_cursor_at(point, extend);
            self.launcher.selecting = true;
            self.launcher.input.anchor.get_or_insert(self.launcher.input.cursor);
            return;
        }
        if let Some(i) = self.launcher_at(point) {
            self.launcher.selected = i;
            self.launch_app();
        } else if !layout.panel.contains(point) {
            self.close_launcher();
        }
    }

    pub fn launcher_release(&mut self) {
        self.launcher.selecting = false;
    }

    fn launcher_cursor_at(&mut self, point: Point, extend: bool) {
        let text = self.launcher_layout(self.launcher.matches().len()).text;
        let input = &self.launcher.input;
        let position = self.text.borrow_mut().field_hit(
            &input.text,
            &self.theme.font,
            self.theme.font_size * SEARCH_SCALE,
            text.w as f32,
            input.cursor,
            (point.x - text.x) as f32,
        );
        self.launcher.input.move_to(position, extend);
        self.launcher.edited = Instant::now();
    }

    pub fn launcher_elements(&self, renderer: &mut GlesRenderer) -> Vec<OutputElement> {
        if !self.launcher.open {
            return Vec::new();
        }
        let t = &self.theme;
        let matches = self.launcher.matches();
        let layout = self.launcher_layout(matches.len());
        let input = &self.launcher.input;
        let mut p = Painter::new(self, renderer);

        // Fades in while dropping into place.
        let shown = ease_out(self.launcher.opened.elapsed().as_secs_f64() / 0.16);
        p.alpha = shown as f32;
        p.fill(self.output_rect(), Colour([0.0, 0.0, 0.0, 0.32]), 0.0);
        p.offset = (0.0, (shown - 1.0) * 12.0);
        p.popup(layout.panel);

        // The search field, glowing softly as the place where typing goes.
        p.shape(
            layout.field,
            Shape {
                border_width: 1.0,
                border: t.accent.alpha(0.45),
                glow_radius: t.glow * 0.6,
                glow: t.accent.alpha(0.12),
                ..Shape::filled(t.background, t.radius)
            },
        );
        let typing = !input.text.is_empty();
        p.glyph(
            glyph::SEARCH,
            Rect::new(layout.field.x + 8.0, layout.field.y, 28.0, layout.field.h),
            18,
            if typing { t.accent } else { t.muted },
        );
        let size = t.font_size * SEARCH_SCALE;
        if !typing {
            let hint = Rect::new(layout.text.x + 4.0, layout.text.y, layout.text.w - 4.0, layout.text.h);
            p.text("Search applications", hint, Style::new(size, t.muted));
        }
        let field = self.text.borrow_mut().field(
            &input.text,
            &t.font,
            size,
            t.text,
            layout.text.w as f32,
            crate::text::Field {
                cursor: input.cursor,
                anchor: input.anchor,
                visible: self.launcher.edited.elapsed().as_millis() % 1000 < 600,
                accent: t.accent.straight_rgba8(),
            },
        );
        if let Some(image) = field {
            p.image(&image.buffer, (layout.text.x, layout.text.y + (layout.text.h - image.height as f64) / 2.0));
        }
        // The rest of the chosen name, greyed out after what was typed.
        let chosen = matches.get(self.launcher.selected);
        if let Some(rest) = chosen.and_then(|app| completion(&input.text, &app.name)) {
            if input.cursor == input.text.len() && input.anchor.is_none() {
                let typed = p.measure(&input.text, 4096.0, Style::new(size, t.text)).0;
                let room = layout.text.w - typed - 4.0;
                if room > 24.0 {
                    let at = Rect::new(layout.text.x + typed + 1.0, layout.text.y, room, layout.text.h);
                    p.text(rest, at, Style::new(size, t.muted.alpha(0.8)));
                }
            }
        }

        let caption = if typing {
            "RESULTS"
        } else if self.launcher.usage.is_empty() {
            "APPLICATIONS"
        } else {
            "FREQUENTLY USED"
        };
        p.text(caption, layout.caption, Style::new(t.font_size * 0.75, t.muted).bold());

        if matches.is_empty() {
            let area = Rect::new(layout.panel.x, layout.caption.bottom(), layout.panel.w, 2.0 * ROW_H);
            p.glyph(glyph::SEARCH, Rect::new(area.x, area.y + 8.0, area.w, 40.0), 28, t.muted.alpha(0.7));
            let message = format!("Nothing matches \u{201c}{}\u{201d}", input.text.trim());
            p.text(
                &message,
                Rect::new(area.x + 24.0, area.y + 52.0, area.w - 48.0, 28.0),
                Style::new(t.font_size, t.muted).align(Align::Centre),
            );
        }
        for (i, (app, row)) in matches.iter().zip(&layout.rows).enumerate() {
            let selected = i == self.launcher.selected;
            if selected {
                p.shape(
                    *row,
                    Shape {
                        radius: t.radius,
                        fill: (t.accent.alpha(0.20), t.accent_end.alpha(0.06)),
                        ..Default::default()
                    },
                );
                p.shape(Rect::new(row.x, row.y + 12.0, 3.0, row.h - 24.0), Shape::filled(t.accent, 1.5));
            }
            let icon = Rect::new(row.x + 14.0, row.y + (row.h - 32.0) / 2.0, 32.0, 32.0);
            if let Some(image) = self.launcher.app_icon(&app.id, 32) {
                p.image(&image, (icon.x, icon.y));
            } else {
                p.shape(icon, Shape { radius: t.radius, fill: (t.accent, t.accent_end), ..Default::default() });
                let initial: String = app.name.chars().take(1).collect::<String>().to_uppercase();
                p.text(&initial, icon, Style::new(t.font_size * 1.2, t.on_accent).bold().align(Align::Centre));
            }
            let enter_w = if selected { 64.0 } else { 0.0 };
            let text_x = icon.right() + 14.0;
            let text_w = row.right() - text_x - 12.0 - enter_w;
            let name_style = Style::new(t.font_size * 1.05, if selected { t.text } else { t.text.alpha(0.88) });
            let name_style = if selected { name_style.bold() } else { name_style };
            match subtitle(app) {
                Some(sub) => {
                    p.text(&app.name, Rect::new(text_x, row.y + 7.0, text_w, 22.0), name_style);
                    p.text(sub, Rect::new(text_x, row.y + 27.0, text_w, 18.0), Style::new(t.font_size * 0.85, t.muted));
                }
                None => {
                    p.text(&app.name, Rect::new(text_x, row.y, text_w, row.h), name_style);
                }
            }
            if selected {
                let w = p.keycap_width("Enter");
                p.keycap("Enter", row.right() - 12.0 - w, row.y + row.h / 2.0);
            }
        }

        // Keys along the bottom, under a hairline.
        let footer = layout.footer;
        p.fill(Rect::new(footer.x + 1.0, footer.y, footer.w - 2.0, 1.0), t.border, 0.0);
        let hints = [("\u{2191}\u{2193}", "Choose"), ("Enter", "Launch"), ("Ctrl+U", "Clear"), ("Esc", "Close")];
        p.key_hints(&hints, footer.x + 16.0, footer.y + footer.h / 2.0, false);
        p.finish()
    }
}

/// The part of `name` still to come after `typed`, when `name` starts with it.
fn completion<'a>(typed: &str, name: &'a str) -> Option<&'a str> {
    if typed.is_empty() {
        return None;
    }
    let mut rest = name.char_indices();
    for typed in typed.chars() {
        let (_, c) = rest.next()?;
        if !c.to_lowercase().eq(typed.to_lowercase()) {
            return None;
        }
    }
    let at = rest.next().map(|(i, _)| i)?;
    Some(&name[at..])
}

/// A short description under the name: the generic name, or else the comment.
fn subtitle(app: &tileception_apps::App) -> Option<&str> {
    app.generic_name
        .as_deref()
        .filter(|g| !g.is_empty() && !g.eq_ignore_ascii_case(&app.name))
        .or(app.comment.as_deref())
        .filter(|s| !s.is_empty())
}

/// The largest image `copy-image` takes.
const MAX_IMAGE: u64 = 64 * 1024 * 1024;
const PANEL_W: f64 = 620.0;
const PAD: f64 = 10.0;
const FIELD_H: f64 = 52.0;
const CAPTION_H: f64 = 32.0;
const ROW_H: f64 = 54.0;
const FOOTER_H: f64 = 44.0;
/// The search text is a little larger than the rest of the interface.
const SEARCH_SCALE: f32 = 1.2;

struct LauncherLayout {
    panel: Rect,
    field: Rect,
    /// Where the typed text goes inside the field.
    text: Rect,
    caption: Rect,
    rows: Vec<Rect>,
    footer: Rect,
}

fn usage_path() -> PathBuf {
    let root = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".local/state"));
    root.join("tileception/launcher-usage.toml")
}

#[cfg(test)]
mod tests {
    use super::completion;

    #[test]
    fn completes_names_that_start_with_the_query() {
        assert_eq!(completion("viv", "Vivaldi"), Some("aldi"));
        assert_eq!(completion("VIV", "Vivaldi"), Some("aldi"));
        assert_eq!(completion("ée", "Éether"), Some("ther"));
        assert_eq!(completion("ald", "Vivaldi"), None);
        assert_eq!(completion("vivaldi", "Vivaldi"), None);
        assert_eq!(completion("", "Vivaldi"), None);
    }
}
