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
    render::{push_image, push_shape, solid, OutputElement},
    shapes::Shape,
    ui::Icons,
    Tileception,
};

/// Serve compositor-owned clipboard text without starting a Wayland client.
pub(crate) fn send_clipboard_text(fd: std::os::fd::OwnedFd, text: String) {
    use std::io::Write;
    std::thread::spawn(move || {
        let _ = std::fs::File::from(fd).write_all(text.as_bytes());
    });
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
                    set_data_device_selection(&self.display_handle, &self.seat, mime_types.clone(), Some(text));
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
        let text = current_data_device_selection_userdata(&self.seat).and_then(|data| data.clone());
        if let Some(text) = text {
            self.launcher_insert_paste(&text);
            return;
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

    fn launcher_panel(&self) -> Rect {
        let screen = self.output_rect();
        let w = (screen.w - 32.0).clamp(1.0, 580.0);
        let h = (screen.h - 32.0).clamp(1.0, 530.0);
        Rect::new(screen.x + (screen.w - w) / 2.0, screen.y + (screen.h - h) / 3.0, w, h)
    }

    pub fn launcher_at(&self, point: Point) -> Option<usize> {
        let panel = self.launcher_panel();
        if point.x < panel.x + 12.0 || point.x > panel.right() - 12.0 || point.y < panel.y + 76.0 {
            return None;
        }
        let i = ((point.y - panel.y - 76.0) / 50.0) as usize;
        (i < self.launcher.matches().len() && point.y < panel.bottom() - 36.0).then_some(i)
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
        let panel = self.launcher_panel();
        if point.x >= panel.x + 16.0
            && point.x < panel.right() - 16.0
            && point.y >= panel.y + 12.0
            && point.y < panel.y + 64.0
        {
            let extend = self.seat.get_keyboard().expect("seat has a keyboard").modifier_state().shift;
            self.launcher_cursor_at(point, extend);
            self.launcher.selecting = true;
            self.launcher.input.anchor.get_or_insert(self.launcher.input.cursor);
            return;
        }
        if let Some(i) = self.launcher_at(point) {
            self.launcher.selected = i;
            self.launch_app();
        } else if !self.launcher_panel().contains(point) {
            self.close_launcher();
        }
    }

    pub fn launcher_release(&mut self) {
        self.launcher.selecting = false;
    }

    fn launcher_cursor_at(&mut self, point: Point, extend: bool) {
        let panel = self.launcher_panel();
        let input = &self.launcher.input;
        let position = self.text.borrow_mut().field_hit(
            &input.text,
            &self.theme.font,
            self.theme.font_size,
            (panel.w - 48.0) as f32,
            input.cursor,
            (point.x - panel.x - 24.0) as f32,
        );
        self.launcher.input.move_to(position, extend);
        self.launcher.edited = Instant::now();
    }

    pub fn launcher_elements(&self, renderer: &mut GlesRenderer) -> Vec<OutputElement> {
        if !self.launcher.open {
            return Vec::new();
        }
        let panel = self.launcher_panel();
        let t = &self.theme;
        let shapes = self.shapes(renderer);
        let mut out = Vec::new();
        let input = &self.launcher.input;
        if input.text.is_empty() {
            self.ui_label(
                &mut out,
                renderer,
                "Search applications...",
                Rect::new(panel.x + 24.0, panel.y + 16.0, panel.w - 48.0, 42.0),
                t.muted,
                false,
            );
        }
        if let Some(image) = self.text.borrow_mut().field(
            &input.text,
            &t.font,
            t.font_size,
            t.text,
            (panel.w - 48.0) as f32,
            crate::text::Field {
                cursor: input.cursor,
                anchor: input.anchor,
                visible: self.launcher.edited.elapsed().as_millis() % 1000 < 600,
                accent: t.accent.straight_rgba8(),
            },
        ) {
            push_image(
                &mut out,
                renderer,
                &image.buffer,
                (panel.x + 24.0, panel.y + 16.0 + (42.0 - image.height as f64) / 2.0),
            );
        }
        let matches = self.launcher.matches();
        if matches.is_empty() {
            self.ui_label(
                &mut out,
                renderer,
                "No matching applications",
                Rect::new(panel.x + 24.0, panel.y + 80.0, panel.w - 48.0, 42.0),
                t.muted,
                false,
            );
        }
        for (i, app) in matches.iter().enumerate() {
            let row = Rect::new(panel.x + 12.0, panel.y + 76.0 + i as f64 * 50.0, panel.w - 24.0, 48.0);
            if row.bottom() > panel.bottom() - 36.0 {
                break;
            }
            self.ui_label(
                &mut out,
                renderer,
                &app.name,
                Rect::new(row.x + 52.0, row.y, row.w - 64.0, row.h),
                t.text,
                i == self.launcher.selected,
            );
            if let Some(image) = self.launcher.app_icon(&app.id, 32) {
                push_image(&mut out, renderer, &image, (row.x + 10.0, row.y + 8.0));
            } else {
                self.ui_label(
                    &mut out,
                    renderer,
                    &app.name.chars().take(1).collect::<String>(),
                    Rect::new(row.x + 18.0, row.y, 28.0, row.h),
                    t.accent,
                    true,
                );
            }
            if i == self.launcher.selected {
                push_shape(&mut out, shapes.as_ref(), &row, Shape::filled(t.accent.alpha(0.14), t.radius));
            }
        }
        self.ui_label(
            &mut out,
            renderer,
            "Arrows to choose, Enter to launch, Esc to close",
            Rect::new(panel.x + 24.0, panel.bottom() - 32.0, panel.w - 48.0, 26.0),
            t.muted,
            false,
        );
        push_shape(
            &mut out,
            shapes.as_ref(),
            &panel,
            Shape {
                border_width: 1.0,
                border: t.border,
                glow_radius: t.glow,
                glow: t.accent.alpha(0.15),
                ..Shape::filled(t.surface, t.popup_radius)
            },
        );
        out.push(solid(&self.output_rect(), [0.0, 0.0, 0.0, 0.45]));
        out
    }
}

fn usage_path() -> PathBuf {
    let root = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".local/state"));
    root.join("tileception/launcher-usage.toml")
}
