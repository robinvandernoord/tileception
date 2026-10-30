//! Drag and drop between programs, made visible.
//!
//! A program that starts a drag may hand over an icon for it, drawn by the
//! pointer as it asks. Without one, a small card says what is dragged: a
//! thumbnail of a picture, or the first file's name or the start of the
//! text, with how many items there are. To know that, the list of files or
//! the text is read from the program once, in the background, as the drag
//! starts. A drag that came from another machine shows the same card (see
//! `handoff/drag.rs`).

use std::{
    io::Read,
    os::unix::net::UnixStream,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};

use smithay::{
    backend::{
        allocator::Fourcc,
        renderer::{
            element::{Kind, memory::MemoryRenderBuffer, surface::render_elements_from_surface_tree},
            gles::GlesRenderer,
        },
    },
    reexports::wayland_server::protocol::{wl_data_source::WlDataSource, wl_surface::WlSurface},
    utils::{Logical, Point, Transform},
    wayland::{
        compositor::{SurfaceAttributes, with_states},
        selection::data_device::with_source_metadata,
    },
};
use tileception_core::Rect;
use tileception_handoff::{DragKind, DragPreview, pack_thumbnail, unpack_rgba};

use crate::{
    Tileception,
    render::{OutputElement, enlarged, physical_point},
    shapes::Shape,
    theme::Colour,
    ui::{Align, Painter, Style, glyph},
};

/// The most read from a program to describe what it drags.
const DESCRIBE_LIMIT: u64 = 1024 * 1024;
/// How long a program gets to say what it drags.
const DESCRIBE_WAIT: Duration = Duration::from_secs(2);
/// Pictures larger than this are not opened for a thumbnail.
const THUMBNAIL_SOURCE_LIMIT: u64 = 32 * 1024 * 1024;
/// A thumbnail's longest side, in pixels.
const THUMBNAIL: u32 = 80;
/// Where the card goes from the pointer's tip.
const CARD_OFFSET: (f64, f64) = (18.0, 20.0);

/// A program's drag here.
pub struct LocalDrag {
    /// None when the program keeps the drag to its own windows.
    pub source: Option<WlDataSource>,
    pub mime_types: Vec<String>,
    icon: Option<WlSurface>,
    /// Where the icon goes from the pointer, as the program moved it.
    icon_offset: Point<i32, Logical>,
    /// Filled in from the background once the program said what it drags.
    described: Arc<Mutex<Option<DragPreview>>>,
    pub card: Option<Card>,
}

impl LocalDrag {
    /// What is dragged, once known.
    pub fn preview(&mut self) -> Option<&DragPreview> {
        if self.card.is_none()
            && let Some(preview) = self.described.lock().ok().and_then(|mut described| described.take())
        {
            self.card = Some(Card::new(preview));
        }
        self.card.as_ref().map(|card| &card.preview)
    }
}

/// What a drag shows when its program gave no icon.
pub struct Card {
    pub preview: DragPreview,
    thumbnail: Option<MemoryRenderBuffer>,
}

impl Card {
    pub fn new(preview: DragPreview) -> Card {
        let thumbnail = preview.thumbnail.as_ref().and_then(|thumbnail| {
            let pixels = unpack_rgba(thumbnail.width, thumbnail.height, &thumbnail.pixels)?;
            let size = (thumbnail.width as i32, thumbnail.height as i32);
            Some(MemoryRenderBuffer::from_slice(&pixels, Fourcc::Abgr8888, size, 1, Transform::Normal, None))
        });
        Card { preview, thumbnail }
    }
}

impl Tileception {
    /// A program here started dragging.
    pub fn drag_started(&mut self, source: Option<WlDataSource>, icon: Option<WlSurface>) {
        let mime_types =
            source.as_ref().and_then(|source| with_source_metadata(source, |meta| meta.mime_types.clone()).ok());
        let mime_types = mime_types.unwrap_or_default();
        let described = Arc::new(Mutex::new(None));
        if let Some(source) = &source {
            describe(source, &mime_types, described.clone());
        }
        self.drag = Some(LocalDrag { source, mime_types, icon, icon_offset: Point::default(), described, card: None });
        self.schedule_render();
    }

    /// A program's drag here ended, dropped or not.
    pub fn drag_ended(&mut self) {
        self.drag = None;
        self.handoff_drag_ended();
        self.schedule_render();
    }

    /// A drag icon moved itself by attaching with an offset.
    pub fn drag_icon_commit(&mut self, surface: &WlSurface) -> bool {
        let Some(drag) = self.drag.as_mut().filter(|drag| drag.icon.as_ref() == Some(surface)) else { return false };
        let delta =
            with_states(surface, |states| states.cached_state.get::<SurfaceAttributes>().current().buffer_delta.take());
        if let Some(delta) = delta {
            drag.icon_offset += delta;
        }
        true
    }

    /// What a drag shows by the pointer: its program's icon, or a card.
    pub fn drag_elements(&mut self, renderer: &mut GlesRenderer) -> Vec<OutputElement> {
        if self.draws_lock_screen() {
            return Vec::new();
        }
        let pointer = self.pointer;
        let scale = self.scale;
        if let Some(drag) = self.drag.as_mut().filter(|_| !self.handoff.drags.sent_away()) {
            if let Some(icon) = &drag.icon {
                let at = (pointer.x + f64::from(drag.icon_offset.x), pointer.y + f64::from(drag.icon_offset.y));
                let origin = physical_point(at, scale);
                let surfaces = render_elements_from_surface_tree(renderer, icon, origin, 1.0, 1.0, Kind::Unspecified);
                return enlarged(surfaces, origin, scale).collect();
            }
            drag.preview();
        }
        let card = match &self.drag {
            Some(drag) if drag.icon.is_none() && !self.handoff.drags.sent_away() => drag.card.as_ref(),
            _ => self.handoff.drags.incoming_card(),
        };
        let Some(card) = card else { return Vec::new() };
        let mut painter = Painter::new(self, renderer);
        draw_card(&mut painter, card, (pointer.x + CARD_OFFSET.0, pointer.y + CARD_OFFSET.1));
        painter.finish()
    }
}

/// The card: a thumbnail with the name under it, or a line icon with the
/// name beside it; a badge with the count when there is more than one.
fn draw_card(painter: &mut Painter, card: &Card, (left, top): (f64, f64)) {
    let theme = painter.theme();
    let preview = &card.preview;
    let radius = theme.popup_radius;
    let label_style = Style::new(theme.font_size * 0.9, theme.text);
    let label_width = painter.measure(&preview.label, 220.0, label_style).0;
    let frame = Shape {
        border_width: 1.0,
        border: theme.border,
        glow_radius: 18.0,
        glow: Colour([0.0, 0.0, 0.0, 0.35]),
        ..Shape::filled(theme.surface_raised.alpha(0.94), radius)
    };
    let rect = if let Some(thumbnail) = &card.thumbnail {
        let size = thumbnail_size(preview);
        let width = (size.0 + 16.0).max(label_width.min(220.0) + 16.0);
        let rect = Rect::new(left, top, width, size.1 + 16.0 + 24.0);
        painter.shape(rect, frame);
        let position = (rect.x + (rect.w - size.0) / 2.0, rect.y + 8.0);
        painter.image_sized(thumbnail, position, Some((size.0 as i32, size.1 as i32)));
        let label = Rect::new(rect.x + 8.0, rect.bottom() - 28.0, rect.w - 16.0, 24.0);
        painter.text(&preview.label, label, label_style.align(Align::Centre));
        rect
    } else {
        let width = 40.0 + label_width.min(220.0) + 14.0;
        let rect = Rect::new(left, top, width, 36.0);
        painter.shape(rect, frame);
        let icon = match preview.kind {
            DragKind::Folder => glyph::FOLDER,
            DragKind::Link => glyph::GLOBE,
            DragKind::Text => glyph::COPY,
            DragKind::Image => glyph::CAMERA,
            DragKind::Files | DragKind::Other => glyph::FILE,
        };
        painter.glyph(icon, Rect::new(rect.x + 8.0, rect.y + 6.0, 24.0, 24.0), 18, theme.accent);
        let label = Rect::new(rect.x + 40.0, rect.y, rect.w - 48.0, rect.h);
        painter.text(&preview.label, label, label_style);
        rect
    };
    if preview.count > 1 {
        let count = preview.count.to_string();
        let style = Style::new(theme.font_size * 0.8, theme.on_accent).bold();
        let width = (painter.measure(&count, 60.0, style).0 + 12.0).max(22.0);
        let badge = Rect::new(rect.right() - width + 6.0, rect.y - 8.0, width, 22.0);
        painter.shape(badge, Shape { radius: 11.0, fill: (theme.accent, theme.accent_end), ..Default::default() });
        painter.text(&count, badge, style.align(Align::Centre));
    }
}

/// A thumbnail's size in logical pixels, one per pixel of it.
fn thumbnail_size(preview: &DragPreview) -> (f64, f64) {
    preview.thumbnail.as_ref().map_or((0.0, 0.0), |thumbnail| (f64::from(thumbnail.width), f64::from(thumbnail.height)))
}

/// Asks the dragging program for its list of files or its text, and works
/// out the preview from it in the background.
fn describe(source: &WlDataSource, mime_types: &[String], into: Arc<Mutex<Option<DragPreview>>>) {
    let has = |wanted: &str| mime_types.iter().any(|mime| mime == wanted);
    let wanted = if has("text/uri-list") {
        Some("text/uri-list")
    } else {
        ["text/plain;charset=utf-8", "UTF8_STRING", "text/plain"].into_iter().find(|mime| has(mime))
    };
    let Some(wanted) = wanted else {
        let image = mime_types.iter().any(|mime| mime.starts_with("image/"));
        let preview = DragPreview {
            label: if image { "Image".into() } else { "Data".into() },
            count: 1,
            kind: if image { DragKind::Image } else { DragKind::Other },
            thumbnail: None,
        };
        if let Ok(mut described) = into.lock() {
            *described = Some(preview);
        }
        return;
    };
    let Ok((reader, writer)) = UnixStream::pair() else { return };
    source.send(wanted.to_string(), std::os::fd::AsFd::as_fd(&writer));
    drop(writer);
    let files = wanted == "text/uri-list";
    let spawned = std::thread::Builder::new().name("tileception-drag-describe".into()).spawn(move || {
        let _ = reader.set_read_timeout(Some(DESCRIBE_WAIT));
        let mut bytes = Vec::new();
        let _ = reader.take(DESCRIBE_LIMIT).read_to_end(&mut bytes);
        let text = String::from_utf8_lossy(&bytes);
        let preview = if files { preview_of_uris(&parse_uri_list(&text)) } else { preview_of_text(&text) };
        if let Ok(mut described) = into.lock() {
            *described = Some(preview);
        }
    });
    if let Err(error) = spawned {
        tracing::warn!("drag: cannot describe what is dragged: {error}");
    }
}

/// The entries of a `text/uri-list`.
pub fn parse_uri_list(text: &str) -> Vec<String> {
    text.lines().map(str::trim).filter(|line| !line.is_empty() && !line.starts_with('#')).map(String::from).collect()
}

/// A local file's path from a `file://` URI.
pub fn file_path(uri: &str) -> Option<PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    // Only this machine's files: no host, or localhost.
    let path = rest.strip_prefix("localhost").unwrap_or(rest);
    if !path.starts_with('/') {
        return None;
    }
    let bytes = percent_decode(path)?;
    Some(PathBuf::from(<std::ffi::OsString as std::os::unix::ffi::OsStringExt>::from_vec(bytes)))
}

/// A `file://` URI for a local path.
pub fn file_uri(path: &Path) -> String {
    use std::os::unix::ffi::OsStrExt;
    let mut uri = String::from("file://");
    for byte in path.as_os_str().as_bytes() {
        let keep = byte.is_ascii_alphanumeric() || b"/-_.~".contains(byte);
        if keep {
            uri.push(char::from(*byte));
        } else {
            uri.push_str(&format!("%{byte:02X}"));
        }
    }
    uri
}

fn percent_decode(text: &str) -> Option<Vec<u8>> {
    let bytes = text.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let hex = std::str::from_utf8(bytes.get(index + 1..index + 3)?).ok()?;
            decoded.push(u8::from_str_radix(hex, 16).ok()?);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    Some(decoded)
}

fn preview_of_text(text: &str) -> DragPreview {
    let line = text.lines().map(str::trim).find(|line| !line.is_empty()).unwrap_or("Text");
    let mut label: String = line.chars().take(60).collect();
    if line.chars().count() > 60 {
        label.push('\u{2026}');
    }
    DragPreview { label, count: 1, kind: DragKind::Text, thumbnail: None }
}

fn preview_of_uris(uris: &[String]) -> DragPreview {
    let count = u32::try_from(uris.len()).unwrap_or(u32::MAX).max(1);
    let Some(first) = uris.first() else {
        return DragPreview { label: "Nothing".into(), count: 1, kind: DragKind::Other, thumbnail: None };
    };
    let Some(path) = file_path(first) else {
        return DragPreview { label: first.clone(), count, kind: DragKind::Link, thumbnail: None };
    };
    let label = path.file_name().map_or_else(|| path.display().to_string(), |name| name.to_string_lossy().into());
    let kind = if path.is_dir() {
        DragKind::Folder
    } else if is_picture(&path) {
        DragKind::Image
    } else {
        DragKind::Files
    };
    let thumbnail = (kind == DragKind::Image).then(|| thumbnail_of(&path)).flatten();
    DragPreview { label, count, kind, thumbnail }
}

fn is_picture(path: &Path) -> bool {
    let extension = path.extension().map(|extension| extension.to_string_lossy().to_ascii_lowercase());
    matches!(extension.as_deref(), Some("png" | "jpg" | "jpeg" | "webp"))
}

/// A small picture of an image file, at most `THUMBNAIL` pixels on a side.
fn thumbnail_of(path: &Path) -> Option<tileception_handoff::Thumbnail> {
    if std::fs::metadata(path).ok()?.len() > THUMBNAIL_SOURCE_LIMIT {
        return None;
    }
    let image = image::open(path).ok()?;
    let small = image.thumbnail(THUMBNAIL, THUMBNAIL).to_rgba8();
    pack_thumbnail(small.width(), small.height(), small.as_raw())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uri_lists_skip_comments_and_blank_lines() {
        let list = "# from a file manager\r\nfile:///home/robin/a%20b.txt\r\n\r\nhttps://example.org/\r\n";
        assert_eq!(parse_uri_list(list), vec!["file:///home/robin/a%20b.txt", "https://example.org/"]);
    }

    #[test]
    fn file_uris_round_trip() {
        let path = Path::new("/tmp/drops/photo #1 (é).png");
        let uri = file_uri(path);
        assert!(!uri.contains(' ') && !uri.contains('#'));
        assert_eq!(file_path(&uri).as_deref(), Some(path));
        assert_eq!(file_path("file://localhost/etc/hosts").as_deref(), Some(Path::new("/etc/hosts")));
        assert_eq!(file_path("file://elsewhere/etc/hosts"), None);
        assert_eq!(file_path("https://example.org/"), None);
    }

    #[test]
    fn previews_say_what_is_dragged() {
        let text = preview_of_text("\n  first line\nsecond");
        assert_eq!((text.label.as_str(), text.kind), ("first line", DragKind::Text));
        let links = preview_of_uris(&["https://example.org/".into(), "https://example.com/".into()]);
        assert_eq!((links.kind, links.count), (DragKind::Link, 2));
        let files = preview_of_uris(&["file:///nonexistent/report.pdf".into()]);
        assert_eq!((files.label.as_str(), files.kind), ("report.pdf", DragKind::Files));
    }
}
