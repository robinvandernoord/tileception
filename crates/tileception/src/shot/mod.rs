//! The area screenshot (Shift+Print, `tileception msg snip`).
//!
//! The screen freezes under a picker: hover a window and click to take it,
//! or drag an area. The area then gets handles to resize it, a column of
//! drawing tools beside it and a row of actions under it: copy, save, ask
//! the Doctor, record the area with or without sound, or present it over
//! the whole screen (see `zoom`). Freezing can be
//! switched off to pick over the live screen; the picture is then taken
//! when it is copied or saved.
//!
//! The feedback dialog opens it in a mode of its own, which only attaches
//! the picture to the feedback (or cancels back to it); the dialog waits
//! inside the picker meanwhile, out of the picture.

mod export;
mod marks;
pub mod record;

use std::{
    cell::RefCell,
    path::{Path, PathBuf},
    rc::Rc,
    sync::{Arc, Mutex},
    time::Instant,
};

use smithay::{
    backend::{allocator::Fourcc, renderer::element::memory::MemoryRenderBuffer, renderer::gles::GlesRenderer},
    input::{keyboard::keysyms, keyboard::xkb, pointer::CursorIcon},
    output::Output,
    utils::Transform,
};
use tileception_core::{Point, Rect};
use tileception_notify::{Image, Urgency};

use crate::{
    Tileception,
    capture::{CaptureTarget, PendingCapture},
    config::Mods,
    launcher::{Clip, Paste},
    render::OutputElement,
    shapes::Shape,
    text_input::TextInput,
    theme::Colour,
    ui::{Align, Button, Look, Painter, Split, Style, glyph},
};
pub use export::CaptureConfig;
use export::{Format, Size};
use marks::{Mark, PALETTE, Tool, WIDTHS};

/// A button's side, in points.
const BUTTON: f64 = 32.0;
/// Room between a toolbar's edge and its buttons.
const PAD: f64 = 4.0;
/// Room between buttons, and between a toolbar and the selection.
const GAP: f64 = 2.0;
const MARGIN: f64 = 8.0;
/// A separator between groups of buttons.
const SEPARATOR: f64 = 9.0;
/// How close to an edge of the selection a press grabs it.
const GRAB: f64 = 7.0;
/// How far the pointer moves before a press becomes a drag.
const DRAG: f64 = 3.0;
/// The arrow joined to Copy and Save.
const ARROW: f64 = 24.0;
/// The button that attaches the picture to feedback.
const ATTACH: f64 = 176.0;
/// The menu of formats and sizes beside Copy and Save.
const MENU_W: f64 = 272.0;
const MENU_TITLE: f64 = 26.0;
/// The switch between sizes.
const MENU_SIZES: f64 = 30.0;
const MENU_ROW: f64 = 34.0;
/// The JPG row, whose quality buttons show their file sizes under them.
const MENU_JPG: f64 = 44.0;
const QUALITY_W: f64 = 52.0;
/// The magnifier while picking: pixels across, and their size in points.
const ZOOM_PIXELS: i32 = 11;
const ZOOM_CELL: f64 = 9.0;

/// An area screenshot being picked.
pub struct Shot {
    output: Option<Output>,
    still: Option<Rc<Still>>,
    /// Over the live screen instead of a still of it.
    live: bool,
    /// While a still is taken, the picker leaves itself out of it.
    pub hidden: bool,
    /// What to do once the still that is being taken arrives.
    waiting: Option<Finish>,
    selection: Option<Rect>,
    drag: Option<Drag>,
    tool: Tool,
    colour: usize,
    width: usize,
    marks: Vec<Mark>,
    /// What undo took away, for redo; a new mark empties it.
    undone: Vec<Mark>,
    typing: Option<Typing>,
    compose: Option<xkb::compose::State>,
    /// The colours and widths are showing beside the colour button.
    palette: bool,
    /// The button pressed, which acts when let go over it.
    pressed: Option<Control>,
    /// The menu of formats beside Copy or Save, while it is open.
    menu: Option<Out>,
    /// The menu's choice the keyboard is on: PNG, WebP, then each JPG quality.
    menu_row: usize,
    /// The size chosen in the menu, as an index into the config's list.
    scale: usize,
    /// The menu's files, made in the background to show their sizes.
    files: Option<Files>,
    /// The marks drawn for the screen, until they change.
    overlay: RefCell<Option<Overlay>>,
    /// Counts changes to the marks and the selection.
    generation: u64,
    /// The sounds Record takes along.
    sources: record::Sources,
    /// The feedback dialog this picks a picture for, waiting meanwhile.
    feedback: Option<Box<crate::feedback::Feedback>>,
}

/// A picture attached to feedback, kept so Edit can reopen it: the frozen
/// screen, the area and the marks on it.
#[derive(Clone)]
pub struct Draft {
    output: Option<Output>,
    still: Rc<Still>,
    selection: Rect,
    marks: Vec<Mark>,
    tool: Tool,
    colour: usize,
    width: usize,
}

struct Still {
    rgba: Vec<u8>,
    width: i32,
    height: i32,
    /// Pixels per point when it was taken.
    scale: f64,
    buffer: MemoryRenderBuffer,
    includes_wizard: bool,
}

impl Still {
    fn source(&self) -> marks::Source<'_> {
        marks::Source { rgba: &self.rgba, width: self.width as u32, height: self.height as u32 }
    }

    /// The colour at a pixel, or none outside the screen.
    fn pixel(&self, column: i32, row: i32) -> Option<[u8; 3]> {
        if column < 0 || row < 0 || column >= self.width || row >= self.height {
            return None;
        }
        let start = (row as usize * self.width as usize + column as usize) * 4;
        let pixel = self.rgba.get(start..start + 3)?;
        Some([pixel[0], pixel[1], pixel[2]])
    }

    /// The selection in pixels, cut to the screen: left, top, width, height.
    fn pixels(&self, selection: Rect) -> Option<(u32, u32, u32, u32)> {
        let rect = crate::render::physical(&selection, self.scale);
        let left = rect.loc.x.clamp(0, self.width);
        let top = rect.loc.y.clamp(0, self.height);
        let right = (rect.loc.x + rect.size.w).clamp(0, self.width);
        let bottom = (rect.loc.y + rect.size.h).clamp(0, self.height);
        (right > left && bottom > top).then(|| (left as u32, top as u32, (right - left) as u32, (bottom - top) as u32))
    }
}

/// A picture file's bytes.
type File = Arc<[u8]>;

/// A file being made: none yet, then the file or why it failed.
type Made = Option<Result<File, String>>;

/// The menu's files (PNG, WebP and JPG at each quality) at the chosen
/// size, each filled in once it is made.
struct Files {
    generation: u64,
    size: Size,
    formats: Vec<Format>,
    made: Arc<Mutex<Vec<Made>>>,
}

impl Files {
    /// The file already made for `export`, if there is one.
    fn made(&self, generation: u64, export: Export) -> Option<Arc<[u8]>> {
        if self.generation != generation || self.size != export.size {
            return None;
        }
        let index = self.formats.iter().position(|format| *format == export.format)?;
        let made = self.made.lock().ok()?;
        made[index].as_ref()?.as_ref().ok().cloned()
    }
}

/// Copy or Save, whose menu is open.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Out {
    Copy,
    Save,
}

/// What to copy or save: a format at a size.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Export {
    format: Format,
    size: Size,
}

impl Export {
    /// What Enter, Copy and Save give: PNG at full size.
    const DEFAULT: Export = Export { format: Format::Png, size: Size::FULL };
}

/// Where the parts of the menu are: its title, the size switch, and a
/// rect for each choice (PNG and WebP rows, then a button per JPG quality).
struct MenuParts {
    title: Rect,
    track: Rect,
    size_label: Rect,
    sizes: Vec<Rect>,
    jpg_row: Rect,
    choices: Vec<Rect>,
}

impl MenuParts {
    fn new(menu: Rect, qualities: usize, sizes: usize) -> MenuParts {
        let inner = Rect::new(menu.x + PAD, menu.y + PAD, menu.w - PAD * 2.0, menu.h - PAD * 2.0);
        let title = Rect::new(inner.x + 8.0, inner.y, inner.w - 16.0, MENU_TITLE);
        let track = Rect::new(inner.x + 2.0, title.bottom(), inner.w - 4.0, MENU_SIZES);
        let size_label = Rect::new(track.x + 10.0, track.y, 36.0, track.h);
        let room = track.right() - 2.0 - size_label.right();
        let size_w = room / sizes.max(1) as f64;
        let sizes = (0..sizes)
            .map(|index| Rect::new(size_label.right() + index as f64 * size_w, track.y + 2.0, size_w, track.h - 4.0))
            .collect();
        let png = Rect::new(inner.x, track.bottom() + 6.0, inner.w, MENU_ROW);
        let webp = Rect::new(inner.x, png.bottom(), inner.w, MENU_ROW);
        let jpg_row = Rect::new(inner.x, webp.bottom(), inner.w, MENU_JPG);
        let first = jpg_row.right() - 4.0 - qualities as f64 * (QUALITY_W + GAP * 2.0) + GAP * 2.0;
        let mut choices = vec![png, webp];
        choices.extend((0..qualities).map(|index| {
            Rect::new(first + index as f64 * (QUALITY_W + GAP * 2.0), jpg_row.y + 4.0, QUALITY_W, MENU_JPG - 8.0)
        }));
        MenuParts { title, track, size_label, sizes, jpg_row, choices }
    }

    fn height() -> f64 {
        PAD * 2.0 + MENU_TITLE + MENU_SIZES + 6.0 + MENU_ROW * 2.0 + MENU_JPG
    }
}

struct Overlay {
    generation: u64,
    buffer: Option<MemoryRenderBuffer>,
    position: (f64, f64),
}

/// Text being typed onto the picture.
struct Typing {
    corner: Point,
    input: TextInput,
    edited: Instant,
}

#[derive(Clone, Copy)]
enum Drag {
    /// A new selection from `anchor`; `moved` once it went far enough.
    New {
        anchor: Point,
        moved: bool,
    },
    Move {
        grab: Point,
        start: Rect,
    },
    Resize {
        edges: Edges,
        grab: Point,
        start: Rect,
    },
    /// The newest mark follows the pointer.
    Draw,
    /// Selecting in the text being typed.
    Select,
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
struct Edges {
    left: bool,
    right: bool,
    top: bool,
    bottom: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Control {
    Tool(Tool),
    Colour,
    Swatch(usize),
    Width(usize),
    Undo,
    Redo,
    Live,
    Record,
    /// Whether Record takes along what the speakers play.
    SystemSound,
    /// Whether Record takes along the microphone.
    Microphone,
    /// Presents the area over the whole screen, live.
    Zoom,
    Doctor,
    Cancel,
    Save,
    Copy,
    /// Hands the picture to the feedback dialog.
    Attach,
    /// The arrow beside Copy or Save, which opens its menu.
    Menu(Out),
    /// A choice in the open menu: PNG, WebP, then each JPG quality.
    Choice(usize),
    /// A size in the open menu.
    Scale(usize),
}

/// What finishing does with the picture.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Finish {
    Copy(Export),
    Save(Export),
    Doctor,
    /// Attaches the picture to the waiting feedback dialog.
    Feedback,
}

/// Where the toolbars go around a selection.
struct Layout {
    column: Rect,
    row: Rect,
    row_separator: Option<Rect>,
    palette: Option<Rect>,
    menu: Option<Rect>,
    /// The column is to the right of the selection (or inside it, at its right).
    column_right: bool,
    row_below: bool,
    buttons: Vec<(Rect, Control)>,
}

impl Shot {
    fn new(output: Option<Output>) -> Shot {
        Shot {
            output,
            still: None,
            live: false,
            hidden: false,
            waiting: None,
            selection: None,
            drag: None,
            tool: Tool::Select,
            colour: 0,
            width: 1,
            marks: Vec::new(),
            undone: Vec::new(),
            typing: None,
            compose: crate::ui::compose_state(),
            palette: false,
            pressed: None,
            menu: None,
            menu_row: 0,
            scale: 0,
            files: None,
            overlay: RefCell::new(None),
            generation: 0,
            sources: record::Sources::load(),
            feedback: None,
        }
    }

    /// The picker again on an attached picture, as it was left.
    fn from_draft(draft: Draft) -> Shot {
        let mut shot = Shot::new(draft.output);
        shot.still = Some(draft.still);
        shot.selection = Some(draft.selection);
        shot.marks = draft.marks;
        shot.tool = draft.tool;
        shot.colour = draft.colour;
        shot.width = draft.width;
        shot
    }

    /// What Edit needs to reopen this picture.
    fn draft(&self) -> Option<Draft> {
        Some(Draft {
            output: self.output.clone(),
            still: self.still.clone()?,
            selection: self.selection?,
            marks: self.marks.clone(),
            tool: self.tool,
            colour: self.colour,
            width: self.width,
        })
    }

    /// The feedback dialog waiting for this picture, if any.
    pub(crate) fn feedback(&self) -> Option<&crate::feedback::Feedback> {
        self.feedback.as_deref()
    }

    fn colour(&self) -> [u8; 3] {
        PALETTE[self.colour]
    }

    fn line_width(&self) -> f64 {
        WIDTHS[self.width]
    }

    fn changed(&mut self) {
        self.generation += 1;
    }

    fn undo(&mut self) {
        if let Some(mark) = self.marks.pop() {
            self.undone.push(mark);
            self.changed();
        }
    }

    fn redo(&mut self) {
        if let Some(mark) = self.undone.pop() {
            self.marks.push(mark);
            self.changed();
        }
    }

    /// Adds a mark that the pointer goes on to drag.
    fn begin_mark(&mut self, mark: Mark) {
        self.marks.push(mark);
        self.undone.clear();
        self.changed();
    }

    /// Puts the typed text on the picture, if there is any.
    fn commit_typing(&mut self) {
        let Some(typing) = self.typing.take() else { return };
        let mark = Mark::Text {
            corner: typing.corner,
            text: typing.input.text,
            colour: self.colour(),
            width: self.line_width(),
        };
        if !mark.is_empty() {
            self.begin_mark(mark);
        }
    }

    /// Switches what the speakers play (`system`) or the microphone on or
    /// off for Record, and remembers it for next time.
    fn toggle_sound(&mut self, system: bool) {
        if system {
            self.sources.system = !self.sources.system;
        } else {
            self.sources.microphone = !self.sources.microphone;
        }
        self.sources.save();
    }

    fn next_step(&self) -> u32 {
        let steps = self.marks.iter().filter(|mark| matches!(mark, Mark::Step { .. })).count();
        steps as u32 + 1
    }
}

impl Control {
    fn label(self, shot: &Shot) -> &'static str {
        match self {
            Control::Tool(tool) => tool.label(),
            Control::Colour => "Colour and width  1-6  [ ]",
            Control::Swatch(_) => "Colour  1-6",
            Control::Width(0) => "Thin  [",
            Control::Width(1) => "Medium",
            Control::Width(_) => "Thick  ]",
            Control::Undo => "Undo  Ctrl+Z",
            Control::Redo => "Redo  Ctrl+Y",
            Control::Live if shot.live => "Freeze the screen  F",
            Control::Live => "Show the live screen  F",
            Control::Record => "Record the area  Ctrl+R",
            Control::SystemSound if shot.sources.system => "Record with system sound: on  O",
            Control::SystemSound => "Record with system sound: off  O",
            Control::Microphone if shot.sources.microphone => "Record with the microphone: on  M",
            Control::Microphone => "Record with the microphone: off  M",
            Control::Zoom => "Present the area full-screen  Z",
            Control::Attach => "Attach to feedback  Enter",
            Control::Doctor => "Ask the Wizard  D",
            Control::Cancel if shot.feedback.is_some() => "Back to feedback  Esc",
            Control::Cancel => "Cancel  Esc",
            Control::Save => "Save  Ctrl+S",
            Control::Copy => "Copy  Enter",
            Control::Menu(Out::Copy) => "Copy as  Shift+Enter",
            Control::Menu(Out::Save) => "Save as  Ctrl+Shift+S",
            Control::Choice(_) | Control::Scale(_) => "",
        }
    }
}

fn tool_glyph(tool: Tool) -> &'static str {
    match tool {
        Tool::Select => glyph::MOVE,
        Tool::Pen => glyph::PEN,
        Tool::Arrow => glyph::ARROW_UP_RIGHT,
        Tool::Rectangle => glyph::SQUARE,
        Tool::Highlighter => glyph::HIGHLIGHTER,
        Tool::Text => glyph::TYPE,
        Tool::Step => glyph::STEP,
        Tool::Redact => glyph::REDACT,
    }
}

fn opaque([red, green, blue]: [u8; 3]) -> Colour {
    Colour([f32::from(red) / 255.0, f32::from(green) / 255.0, f32::from(blue) / 255.0, 1.0])
}

/// The selection's edges near `point`, if any.
fn edges_at(selection: Rect, point: Point) -> Option<Edges> {
    let within_x = point.x >= selection.x - GRAB && point.x <= selection.right() + GRAB;
    let within_y = point.y >= selection.y - GRAB && point.y <= selection.bottom() + GRAB;
    if !within_x || !within_y {
        return None;
    }
    let edges = Edges {
        left: (point.x - selection.x).abs() <= GRAB,
        right: (point.x - selection.right()).abs() <= GRAB,
        top: (point.y - selection.y).abs() <= GRAB,
        bottom: (point.y - selection.bottom()).abs() <= GRAB,
    };
    // A small selection has its edges close together: the nearer one wins.
    let edges = Edges {
        left: edges.left && (!edges.right || point.x < selection.x + selection.w / 2.0),
        right: edges.right && (!edges.left || point.x >= selection.x + selection.w / 2.0),
        top: edges.top && (!edges.bottom || point.y < selection.y + selection.h / 2.0),
        bottom: edges.bottom && (!edges.top || point.y >= selection.y + selection.h / 2.0),
    };
    (edges != Edges::default()).then_some(edges)
}

fn edge_cursor(edges: Edges) -> CursorIcon {
    match (edges.left, edges.right, edges.top, edges.bottom) {
        (true, _, true, _) => CursorIcon::NwResize,
        (_, true, true, _) => CursorIcon::NeResize,
        (true, _, _, true) => CursorIcon::SwResize,
        (_, true, _, true) => CursorIcon::SeResize,
        (true, _, _, _) => CursorIcon::WResize,
        (_, true, _, _) => CursorIcon::EResize,
        (_, _, true, _) => CursorIcon::NResize,
        _ => CursorIcon::SResize,
    }
}

/// `start` with the grabbed edges moved by (`right`, `down`), at least a
/// point wide and high, the right way round when dragged past each other.
fn resized(start: Rect, edges: Edges, right: f64, down: f64) -> Rect {
    let mut left = start.x;
    let mut top = start.y;
    let mut far_x = start.right();
    let mut far_y = start.bottom();
    if edges.left {
        left += right;
    }
    if edges.right {
        far_x += right;
    }
    if edges.top {
        top += down;
    }
    if edges.bottom {
        far_y += down;
    }
    Rect::new(left.min(far_x), top.min(far_y), (far_x - left).abs().max(1.0), (far_y - top).abs().max(1.0))
}

/// `rect` cut to `screen`.
fn within(rect: Rect, screen: Rect) -> Rect {
    let left = rect.x.max(screen.x);
    let top = rect.y.max(screen.y);
    let right = rect.right().min(screen.right());
    let bottom = rect.bottom().min(screen.bottom());
    Rect::new(left, top, (right - left).max(1.0), (bottom - top).max(1.0))
}

/// A folder from `~/.config/user-dirs.dirs` (`XDG_PICTURES_DIR`), or
/// `fallback` in the home folder.
fn user_dir(key: &str, fallback: &str) -> PathBuf {
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/tmp"));
    let config = std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).unwrap_or_else(|| home.join(".config"));
    let named = std::fs::read_to_string(config.join("user-dirs.dirs")).ok().and_then(|dirs| {
        dirs.lines().find_map(|line| {
            let value = line.strip_prefix(key)?.trim_start().strip_prefix('=')?.trim().trim_matches('"');
            let value = value.replace("$HOME", &home.to_string_lossy());
            (!value.is_empty()).then(|| PathBuf::from(value))
        })
    });
    named.unwrap_or_else(|| home.join(fallback))
}

pub(crate) fn screenshots_dir() -> PathBuf {
    user_dir("XDG_PICTURES_DIR", "Pictures").join("Screenshots")
}

pub(crate) fn videos_dir() -> PathBuf {
    user_dir("XDG_VIDEOS_DIR", "Videos").join("Screencasts")
}

pub(crate) fn cache_dir() -> PathBuf {
    std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache")))
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("tileception")
}

/// A new file in `dir` named after the time, such as
/// `screenshot-20261116-142301.png`; made with the folder if it is missing.
pub(crate) fn stamped(dir: &Path, name: &str, extension: &str) -> Option<PathBuf> {
    std::fs::create_dir_all(dir).ok()?;
    let time = crate::ui::LocalTime::now()?;
    let stamp = format!(
        "{}{:02}{:02}-{:02}{:02}{:02}",
        time.year,
        time.month + 1,
        time.day,
        time.hour,
        time.minute,
        time.second
    );
    (0..100)
        .map(|count| {
            let suffix = if count == 0 { String::new() } else { format!("-{count}") };
            dir.join(format!("{name}-{stamp}{suffix}.{extension}"))
        })
        .find(|path| !path.exists())
}

/// A `file://` address for a path, with what an address cannot hold escaped.
pub(crate) fn file_uri(path: &Path) -> String {
    let mut uri = String::from("file://");
    for byte in path.to_string_lossy().bytes() {
        if byte.is_ascii_alphanumeric() || b"/-_.~".contains(&byte) {
            uri.push(byte as char);
        } else {
            uri.push_str(&format!("%{byte:02X}"));
        }
    }
    uri
}

/// A word for `sh -c`, quoted.
fn shell_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', r"'\''"))
}

/// The file for `export`, and for a format other than PNG a PNG beside it
/// for the clipboard. Files the menu already made are used as they are.
fn shot_files(
    shot: &Shot,
    export: Export,
    rgba: &[u8],
    width: u32,
    height: u32,
) -> Result<(File, Option<File>), String> {
    let made = |format: Format| {
        shot.files.as_ref().and_then(|files| files.made(shot.generation, Export { format, size: export.size }))
    };
    let mut resized = None;
    let mut make = |format: Format| -> Result<Arc<[u8]>, String> {
        if let Some(file) = made(format) {
            return Ok(file);
        }
        if resized.is_none() {
            resized = Some(export::resize(rgba, width, height, export.size).ok_or("the picture could not be resized")?);
        }
        let (pixels, new_width, new_height) = resized.as_ref().ok_or("the picture could not be resized")?;
        export::encode(pixels, *new_width, *new_height, format).map(Arc::from)
    };
    let file = make(export.format)?;
    let png = if export.format == Format::Png { None } else { make(Format::Png).ok() };
    Ok((file, png))
}

/// A small copy of a picture for a notification.
fn thumbnail(rgba: &[u8], width: u32, height: u32) -> Option<Image> {
    let picture = image::RgbaImage::from_raw(width, height, rgba.to_vec())?;
    let ratio = (256.0 / f64::from(width.max(height))).min(1.0);
    let (small_w, small_h) =
        (((f64::from(width) * ratio).round() as u32).max(1), ((f64::from(height) * ratio).round() as u32).max(1));
    let small = image::imageops::thumbnail(&picture, small_w, small_h);
    Some(Image::Rgba { width: small_w, height: small_h, data: small.into_raw() })
}

impl Tileception {
    /// Starts picking an area to take; while recording, it stops instead.
    pub fn capture(&mut self) {
        if self.recording.is_some() {
            self.stop_recording();
            return;
        }
        if self.is_locked() || self.shot.is_some() {
            return;
        }
        self.cancel_colour_pick();
        self.shot = Some(Shot::new(self.output.clone()));
        self.take_shot_still();
        self.force_keyboard_focus_sync();
    }

    /// Picks a picture for `feedback`, which waits in the picker until it
    /// is attached or the picker is cancelled. With a `draft`, the picker
    /// reopens on that picture, as long as its screen is still there.
    pub(crate) fn capture_for_feedback(&mut self, feedback: crate::feedback::Feedback, draft: Option<Draft>) {
        if self.is_locked() || self.shot.is_some() {
            self.feedback = Some(feedback);
            return;
        }
        self.cancel_colour_pick();
        let outputs = self.all_outputs();
        let draft = draft.filter(|draft| draft.output.as_ref().is_some_and(|output| outputs.contains(output)));
        let fresh = draft.is_none();
        let mut shot = match draft {
            Some(draft) => Shot::from_draft(draft),
            None => Shot::new(self.output.clone()),
        };
        shot.feedback = Some(Box::new(feedback));
        self.shot = Some(shot);
        if fresh {
            self.take_shot_still();
        }
        self.force_keyboard_focus_sync();
        self.schedule_render();
    }

    /// Queues a still of the screen under the picker, which leaves itself out.
    fn take_shot_still(&mut self) {
        let region = smithay::utils::Rectangle::from_size(self.output_pixels().into());
        self.pending_captures.push(PendingCapture { region, target: CaptureTarget::Shot });
        self.schedule_render();
    }

    /// The still from [`Self::process_captures`], in the captured screen's
    /// context.
    pub fn shot_still_taken(&mut self, rgba: Vec<u8>, (width, height): (i32, i32), includes_wizard: bool) {
        let scale = self.scale;
        let output = self.output.clone();
        let Some(shot) = self.shot.as_mut().filter(|shot| shot.output == output) else { return };
        let buffer =
            MemoryRenderBuffer::from_slice(&rgba, Fourcc::Abgr8888, (width, height), 1, Transform::Normal, None);
        shot.still = Some(Rc::new(Still { rgba, width, height, scale, buffer, includes_wizard }));
        shot.changed();
        if let Some(finish) = shot.waiting.take() {
            self.finish_shot_now(finish);
        }
        self.schedule_render();
    }

    /// Leaves the picker out of what is drawn, while a still is taken.
    pub fn shot_hidden(&mut self, hidden: bool) {
        if let Some(shot) = self.shot.as_mut() {
            shot.hidden = hidden;
        }
    }

    /// Closes the picker; a feedback dialog waiting in it comes back as it was.
    pub fn cancel_shot(&mut self) {
        if let Some(shot) = self.shot.take() {
            if let Some(feedback) = shot.feedback {
                self.feedback = Some(*feedback);
            }
            self.force_keyboard_focus_sync();
            self.schedule_render();
        }
    }

    /// The pointer is going to another machine: picking ends here.
    pub fn hand_over_shot(&mut self) {
        self.cancel_shot();
    }

    /// Whether the picker is on the screen whose context this is.
    fn shot_here(&self) -> bool {
        self.shot.as_ref().is_some_and(|shot| shot.output == self.output)
    }

    fn pointer_point(&self) -> Point {
        let screen = self.output_rect();
        Point::new(self.pointer.x.clamp(screen.x, screen.right()), self.pointer.y.clamp(screen.y, screen.bottom()))
    }

    /// The topmost window under `point`, cut to the screen.
    fn window_rect_at(&self, point: Point) -> Option<Rect> {
        let screen = self.output_rect();
        self.drawn_windows(self.now())
            .into_iter()
            .find(|drawn| drawn.content.contains(point))
            .map(|drawn| within(drawn.content, screen))
    }

    fn shot_selection_changed(&mut self) {
        if let Some(shot) = self.shot.as_mut() {
            shot.changed();
        }
        self.schedule_render();
    }

    /// The pointer moved while picking.
    pub fn shot_motion(&mut self) {
        let Some(shot) = self.shot.as_ref() else { return };
        if shot.output != self.output {
            // Before anything is picked, the picker follows the pointer to another screen.
            if shot.selection.is_none() && shot.drag.is_none() {
                let output = self.output.clone();
                if let Some(shot) = self.shot.as_mut() {
                    shot.output = output;
                    shot.still = None;
                }
                self.take_shot_still();
            }
            self.schedule_render();
            return;
        }
        let point = self.pointer_point();
        let screen = self.output_rect();
        let mut typing_press = None;
        let Some(shot) = self.shot.as_mut() else { return };
        match shot.drag {
            Some(Drag::New { anchor, moved }) => {
                if moved || (point.x - anchor.x).abs() >= DRAG || (point.y - anchor.y).abs() >= DRAG {
                    shot.drag = Some(Drag::New { anchor, moved: true });
                    shot.selection = Some(within(marks::corners(anchor, point), screen));
                    shot.changed();
                }
            }
            Some(Drag::Move { grab, start }) => {
                let left = (start.x + point.x - grab.x).clamp(screen.x, screen.right() - start.w);
                let top = (start.y + point.y - grab.y).clamp(screen.y, screen.bottom() - start.h);
                shot.selection = Some(Rect::new(left, top, start.w, start.h));
                shot.changed();
            }
            Some(Drag::Resize { edges, grab, start }) => {
                shot.selection = Some(within(resized(start, edges, point.x - grab.x, point.y - grab.y), screen));
                shot.changed();
            }
            Some(Drag::Draw) => {
                if let Some(mark) = shot.marks.last_mut() {
                    mark.drag_to(point);
                }
                shot.changed();
            }
            Some(Drag::Select) => typing_press = Some(point),
            None => {}
        }
        if let Some(point) = typing_press {
            self.shot_field_press(point, true);
        }
        self.schedule_render();
    }

    /// A button pressed or let go while picking.
    pub fn shot_button(&mut self, button: u32, pressed: bool) {
        const BTN_LEFT: u32 = 0x110;
        const BTN_RIGHT: u32 = 0x111;
        const BTN_MIDDLE: u32 = 0x112;
        if !self.shot_here() {
            return;
        }
        match (button, pressed) {
            (BTN_LEFT, true) => self.shot_press(),
            (BTN_LEFT, false) => self.shot_release(),
            (BTN_RIGHT, true) => {
                let point = self.pointer_point();
                let control = self.shot_layout().and_then(|layout| {
                    layout.buttons.iter().find(|(rect, _)| rect.contains(point)).map(|(_, control)| *control)
                });
                // A right click on Copy or Save opens its menu.
                let out = match control {
                    Some(Control::Copy | Control::Menu(Out::Copy)) => Some(Out::Copy),
                    Some(Control::Save | Control::Menu(Out::Save)) => Some(Out::Save),
                    _ => None,
                };
                if let Some(out) = out {
                    self.toggle_shot_menu(out);
                    self.schedule_render();
                    return;
                }
                let Some(shot) = self.shot.as_mut() else { return };
                if shot.menu.is_some() {
                    shot.menu = None;
                } else if shot.typing.is_some() {
                    shot.typing = None;
                } else if shot.palette {
                    shot.palette = false;
                } else if shot.selection.is_none() {
                    self.cancel_shot();
                }
            }
            (BTN_MIDDLE, true) => {
                let point = self.pointer_point();
                if self.shot_field_rect().is_some_and(|field| field.contains(point)) {
                    self.shot_field_press(point, false);
                    self.paste_primary(Paste::Shot);
                }
            }
            _ => {}
        }
        self.schedule_render();
    }

    fn shot_press(&mut self) {
        let point = self.pointer_point();
        let control = self.shot_layout().and_then(|layout| {
            layout.buttons.iter().find(|(rect, _)| rect.contains(point)).map(|(_, control)| *control)
        });
        let field = self.shot_field_rect();
        let Some(shot) = self.shot.as_mut() else { return };
        shot.pressed = control;
        if control.is_some() {
            return;
        }
        shot.palette = false;
        shot.menu = None;
        if shot.typing.is_some() {
            if field.is_some_and(|field| field.contains(point)) {
                shot.drag = Some(Drag::Select);
                // With Shift, the press selects on from the caret.
                let extend = self.seat.get_keyboard().is_some_and(|keyboard| keyboard.modifier_state().shift);
                self.shot_field_press(point, extend);
                return;
            }
            shot.commit_typing();
        }
        let Some(selection) = shot.selection else {
            shot.drag = Some(Drag::New { anchor: point, moved: false });
            return;
        };
        if let Some(edges) = edges_at(selection, point) {
            shot.drag = Some(Drag::Resize { edges, grab: point, start: selection });
            return;
        }
        if !selection.contains(point) {
            if shot.tool == Tool::Select {
                shot.drag = Some(Drag::New { anchor: point, moved: false });
            }
            return;
        }
        let (colour, width) = (shot.colour(), shot.line_width());
        match shot.tool {
            Tool::Select => shot.drag = Some(Drag::Move { grab: point, start: selection }),
            Tool::Text => {
                shot.typing = Some(Typing { corner: point, input: TextInput::new(""), edited: Instant::now() });
            }
            Tool::Step => {
                let number = shot.next_step();
                shot.begin_mark(Mark::Step { centre: point, number, colour, width });
            }
            Tool::Pen => {
                shot.begin_mark(Mark::Pen { points: vec![point], colour, width });
                shot.drag = Some(Drag::Draw);
            }
            Tool::Highlighter => {
                shot.begin_mark(Mark::Highlighter { points: vec![point], colour, width });
                shot.drag = Some(Drag::Draw);
            }
            Tool::Arrow => {
                shot.begin_mark(Mark::Arrow { start: point, end: point, colour, width });
                shot.drag = Some(Drag::Draw);
            }
            Tool::Rectangle => {
                shot.begin_mark(Mark::Rectangle { start: point, end: point, colour, width });
                shot.drag = Some(Drag::Draw);
            }
            Tool::Redact => {
                shot.begin_mark(Mark::Redact { start: point, end: point });
                shot.drag = Some(Drag::Draw);
            }
        }
    }

    fn shot_release(&mut self) {
        let point = self.pointer_point();
        let control = self.shot_layout().and_then(|layout| {
            layout.buttons.iter().find(|(rect, _)| rect.contains(point)).map(|(_, control)| *control)
        });
        let snapped = self.window_rect_at(point).unwrap_or_else(|| self.output_rect());
        let Some(shot) = self.shot.as_mut() else { return };
        if let Some(pressed) = shot.pressed.take() {
            if control == Some(pressed) {
                self.shot_control(pressed);
            }
            return;
        }
        match shot.drag.take() {
            // A click without a drag takes the window under it, or the whole screen.
            Some(Drag::New { moved: false, .. }) if shot.selection.is_none() => {
                shot.selection = Some(snapped);
                shot.changed();
            }
            Some(Drag::Draw) => {
                if shot.marks.last().is_some_and(Mark::is_empty) {
                    shot.marks.pop();
                }
                shot.changed();
            }
            Some(Drag::Select) => {
                let selected = shot.typing.as_ref().and_then(|typing| typing.input.selected_text());
                self.offer_primary(selected);
            }
            _ => {}
        }
    }

    fn shot_control(&mut self, control: Control) {
        let doctor = self.shell().doctor();
        let Some(shot) = self.shot.as_mut() else { return };
        match control {
            Control::Tool(tool) => {
                shot.commit_typing();
                shot.tool = tool;
            }
            Control::Colour => shot.palette = !shot.palette,
            Control::Swatch(index) => {
                shot.colour = index;
                shot.palette = false;
            }
            Control::Width(index) => {
                shot.width = index;
                shot.palette = false;
            }
            Control::Undo => shot.undo(),
            Control::Redo => shot.redo(),
            Control::Live => self.toggle_shot_live(),
            Control::Record => self.record_shot(),
            Control::SystemSound => shot.toggle_sound(true),
            Control::Microphone => shot.toggle_sound(false),
            Control::Zoom => self.present_shot(),
            Control::Attach => self.finish_shot(Finish::Feedback),
            Control::Doctor if doctor => self.finish_shot(Finish::Doctor),
            Control::Doctor => {}
            Control::Cancel => self.cancel_shot(),
            Control::Save => self.finish_shot(Finish::Save(Export::DEFAULT)),
            Control::Copy => self.finish_shot(Finish::Copy(Export::DEFAULT)),
            Control::Menu(out) => self.toggle_shot_menu(out),
            Control::Choice(row) => self.shot_choose(row),
            Control::Scale(index) => {
                shot.scale = index;
                self.shot_make_files();
            }
        }
        self.schedule_render();
    }

    /// Opens the menu of formats beside Copy or Save, or closes it.
    fn toggle_shot_menu(&mut self, out: Out) {
        let Some(shot) = self.shot.as_mut() else { return };
        shot.commit_typing();
        shot.palette = false;
        shot.menu = if shot.menu == Some(out) { None } else { Some(out) };
        shot.menu_row = 0;
        self.shot_make_files();
    }

    /// The menu's choices: PNG, WebP, and JPG at each quality in the config.
    fn shot_formats(&self) -> Vec<Format> {
        let mut formats = vec![Format::Png, Format::Webp];
        formats.extend(self.config.file.capture.qualities().into_iter().map(Format::Jpg));
        formats
    }

    /// The size chosen in the menu.
    fn shot_size(&self) -> Size {
        let sizes = self.config.file.capture.sizes();
        sizes[self.shot.as_ref().map_or(0, |shot| shot.scale).min(sizes.len() - 1)]
    }

    /// Copies or saves in the format of a row of the open menu.
    fn shot_choose(&mut self, row: usize) {
        let Some(out) = self.shot.as_ref().and_then(|shot| shot.menu) else { return };
        let Some(format) = self.shot_formats().get(row).copied() else { return };
        let export = Export { format, size: self.shot_size() };
        self.finish_shot(match out {
            Out::Copy => Finish::Copy(export),
            Out::Save => Finish::Save(export),
        });
    }

    /// Makes the menu's files in the background, to show how big each is;
    /// the one chosen is then ready at once.
    fn shot_make_files(&mut self) {
        let formats = self.shot_formats();
        let size = self.shot_size();
        let Some(shot) = self.shot.as_ref().filter(|shot| shot.menu.is_some()) else { return };
        let fresh = shot
            .files
            .as_ref()
            .is_some_and(|files| files.generation == shot.generation && files.size == size && files.formats == formats);
        if fresh {
            return;
        }
        let generation = shot.generation;
        let Ok((rgba, width, height)) = self.shot_pixels(shot) else { return };
        let made: Arc<Mutex<Vec<Made>>> = Arc::new(Mutex::new(vec![None; formats.len()]));
        let filling = made.clone();
        let making = formats.clone();
        let spawned = std::thread::Builder::new().name("capture formats".into()).spawn(move || {
            let resized = export::resize(&rgba, width, height, size);
            // The quick ones first: PNG and the JPGs, then WebP.
            let order = (0..making.len()).filter(|index| *index != 1).chain([1]);
            for index in order {
                let file = match resized.as_ref() {
                    Some((pixels, width, height)) => {
                        export::encode(pixels, *width, *height, making[index]).map(Arc::<[u8]>::from)
                    }
                    None => Err("the picture could not be resized".into()),
                };
                if let Ok(mut made) = filling.lock() {
                    made[index] = Some(file);
                }
            }
        });
        if let Err(error) = spawned {
            tracing::warn!("could not make the screenshot's formats: {error}");
            return;
        }
        let watched = made.clone();
        if let Some(shot) = self.shot.as_mut() {
            shot.files = Some(Files { generation, size, formats, made });
        }
        // Redrawn as each size arrives, until all three are there.
        let timer = smithay::reexports::calloop::timer::Timer::from_duration(std::time::Duration::from_millis(80));
        let watching = self.loop_handle.insert_source(timer, move |_, _, state| {
            use smithay::reexports::calloop::timer::TimeoutAction;
            let current = state
                .shot
                .as_ref()
                .and_then(|shot| shot.files.as_ref())
                .is_some_and(|files| Arc::ptr_eq(&files.made, &watched));
            if !current {
                return TimeoutAction::Drop;
            }
            state.schedule_render();
            let done = watched.lock().map(|made| made.iter().all(Option::is_some)).unwrap_or(true);
            if done { TimeoutAction::Drop } else { TimeoutAction::ToDuration(std::time::Duration::from_millis(80)) }
        });
        if let Err(error) = watching {
            tracing::warn!("could not watch the screenshot's formats: {error}");
        }
    }

    /// Freezes the screen again (with a new still) or shows it live.
    fn toggle_shot_live(&mut self) {
        let Some(shot) = self.shot.as_mut() else { return };
        shot.live = !shot.live;
        let live = shot.live;
        shot.changed();
        if !live {
            self.take_shot_still();
        }
    }

    fn record_shot(&mut self) {
        let Some(shot) = self.shot.as_ref().filter(|shot| shot.feedback.is_none()) else { return };
        let Some(selection) = shot.selection else { return };
        let sources = shot.sources;
        self.cancel_shot();
        self.start_recording(selection, sources);
    }

    /// Presents the picked area over the whole screen, leaving the picker.
    fn present_shot(&mut self) {
        let Some(shot) = self.shot.as_ref().filter(|shot| shot.feedback.is_none()) else { return };
        let Some(selection) = shot.selection else { return };
        self.cancel_shot();
        self.start_zoom(selection);
    }

    /// Keys while picking.
    pub fn shot_key(&mut self, sym: u32, mods: Mods) {
        let Some(output) = self.shot.as_ref().and_then(|shot| shot.output.clone()) else { return };
        if self.output.as_ref() != Some(&output) {
            self.with_output(&output, |state| state.shot_key_here(sym, mods));
        } else {
            self.shot_key_here(sym, mods);
        }
        self.schedule_render();
    }

    fn shot_key_here(&mut self, sym: u32, mods: Mods) {
        let sym = crate::text_input::plain(sym);
        let doctor = self.shell().doctor();
        let choices = self.shot_formats().len();
        let sizes = self.config.file.capture.sizes().len();
        let Some(shot) = self.shot.as_mut() else { return };
        // For feedback, the picture can only be attached: copying, saving,
        // recording, the Doctor and the live screen are not offered.
        let feedback = shot.feedback.is_some();
        let finish = if feedback { Finish::Feedback } else { Finish::Copy(Export::DEFAULT) };
        if let Some(typing) = shot.typing.as_mut() {
            match sym {
                keysyms::KEY_Escape => shot.typing = None,
                keysyms::KEY_Return | keysyms::KEY_KP_Enter => shot.commit_typing(),
                _ => {
                    let key = typing.input.key(&mut shot.compose, sym, mods);
                    typing.edited = Instant::now();
                    self.field_clipboard(key.clipboard, Paste::Shot);
                }
            }
            return;
        }
        let letter = (0x20..0x7f).contains(&sym).then(|| char::from(sym as u8).to_ascii_lowercase());
        // The open menu takes the arrows (Up and Down move between its choices,
        // Left and Right change the size), Enter and Esc.
        if shot.menu.is_some() {
            match sym {
                keysyms::KEY_Escape => shot.menu = None,
                keysyms::KEY_Up => shot.menu_row = (shot.menu_row + choices - 1) % choices,
                keysyms::KEY_Down => shot.menu_row = (shot.menu_row + 1) % choices,
                keysyms::KEY_Left | keysyms::KEY_Right => {
                    let last = sizes - 1;
                    shot.scale = if sym == keysyms::KEY_Left {
                        shot.scale.min(last).saturating_sub(1)
                    } else {
                        (shot.scale + 1).min(last)
                    };
                    self.shot_make_files();
                }
                keysyms::KEY_Return | keysyms::KEY_KP_Enter => {
                    let row = shot.menu_row;
                    self.shot_choose(row);
                }
                _ => {}
            }
            return;
        }
        match (sym, letter) {
            (keysyms::KEY_Escape, _) if shot.palette => shot.palette = false,
            (keysyms::KEY_Return | keysyms::KEY_KP_Enter, _) if mods.shift && shot.selection.is_some() && !feedback => {
                self.toggle_shot_menu(Out::Copy);
            }
            (_, Some('s')) if mods.ctrl && mods.shift && shot.selection.is_some() && !feedback => {
                self.toggle_shot_menu(Out::Save)
            }
            (keysyms::KEY_Escape, _) => self.cancel_shot(),
            (_, Some('c')) if mods.ctrl && feedback => {}
            (keysyms::KEY_Return | keysyms::KEY_KP_Enter, _) | (_, Some('c')) if mods.ctrl || letter.is_none() => {
                if shot.selection.is_some() {
                    self.finish_shot(finish);
                } else {
                    let point = self.pointer_point();
                    let snapped = self.window_rect_at(point).unwrap_or_else(|| self.output_rect());
                    self.shot_select(snapped);
                }
            }
            (_, Some('s')) if mods.ctrl && !feedback => self.finish_shot(Finish::Save(Export::DEFAULT)),
            (_, Some('r')) if mods.ctrl => self.record_shot(),
            (_, Some('z')) if mods.ctrl && mods.shift => shot.redo(),
            (_, Some('z')) if mods.ctrl => shot.undo(),
            (_, Some('y')) if mods.ctrl => shot.redo(),
            (keysyms::KEY_Left | keysyms::KEY_Right | keysyms::KEY_Up | keysyms::KEY_Down, _) => {
                self.shot_arrow(sym, mods)
            }
            _ if mods.ctrl || mods.alt || mods.logo => {}
            (_, Some('f')) if !feedback => self.toggle_shot_live(),
            (_, Some('d')) if doctor && !feedback => self.finish_shot(Finish::Doctor),
            (_, Some('o')) if !feedback => shot.toggle_sound(true),
            (_, Some('m')) if !feedback => shot.toggle_sound(false),
            (_, Some('z')) if !feedback && shot.selection.is_some() => self.present_shot(),
            (_, Some('[')) => shot.width = shot.width.saturating_sub(1),
            (_, Some(']')) => shot.width = (shot.width + 1).min(WIDTHS.len() - 1),
            (_, Some(digit @ '1'..='6')) => shot.colour = usize::from(digit as u8 - b'1'),
            (_, Some(letter)) => {
                if let Some(tool) = Tool::for_key(letter) {
                    shot.tool = tool;
                }
            }
            _ => {}
        }
    }

    fn shot_select(&mut self, rect: Rect) {
        if let Some(shot) = self.shot.as_mut() {
            shot.selection = Some(rect);
            shot.changed();
        }
    }

    /// Arrows nudge the pointer a pixel before anything is picked (ten
    /// with Ctrl); then they move the selection, and with Shift resize it
    /// by its right and bottom edges.
    fn shot_arrow(&mut self, sym: u32, mods: Mods) {
        let step = if mods.ctrl { 10.0 } else { 1.0 } / self.scale.max(0.1);
        let (right, down) = match sym {
            keysyms::KEY_Left => (-step, 0.0),
            keysyms::KEY_Right => (step, 0.0),
            keysyms::KEY_Up => (0.0, -step),
            _ => (0.0, step),
        };
        let screen = self.output_rect();
        let Some(selection) = self.shot.as_ref().and_then(|shot| shot.selection) else {
            let left = (self.pointer.x + right).clamp(screen.x, screen.right() - 0.01);
            let top = (self.pointer.y + down).clamp(screen.y, screen.bottom() - 0.01);
            let time = (self.now() * 1000.0) as u32;
            self.on_motion((left, top).into(), time);
            return;
        };
        let moved = if mods.shift {
            let grown =
                Rect::new(selection.x, selection.y, (selection.w + right).max(1.0), (selection.h + down).max(1.0));
            within(grown, screen)
        } else {
            let left = (selection.x + right).clamp(screen.x, screen.right() - selection.w);
            let top = (selection.y + down).clamp(screen.y, screen.bottom() - selection.h);
            Rect::new(left, top, selection.w, selection.h)
        };
        self.shot_select(moved);
        self.shot_selection_changed();
    }

    /// Pasted text for the text being typed.
    pub fn shot_insert_paste(&mut self, text: &str) {
        if let Some(typing) = self.shot.as_mut().and_then(|shot| shot.typing.as_mut()) {
            typing.input.paste(text);
            typing.edited = Instant::now();
        }
    }

    /// Whether text is being typed onto the picture, for a paste to arrive.
    pub fn shot_typing(&self) -> bool {
        self.shot.as_ref().is_some_and(|shot| shot.typing.is_some())
    }

    /// The size of the text being typed, in points.
    fn shot_text_size(&self) -> f32 {
        self.shot.as_ref().map_or(16.0, |shot| marks::text_size(shot.line_width()) as f32)
    }

    /// Where the text being typed is, as wide as the room to the right of it.
    fn shot_field_rect(&self) -> Option<Rect> {
        let typing = self.shot.as_ref()?.typing.as_ref()?;
        let screen = self.output_rect();
        let size = f64::from(self.shot_text_size());
        let width = (screen.right() - typing.corner.x).clamp(40.0, 900.0);
        Some(Rect::new(typing.corner.x, typing.corner.y, width, (size * 1.3).ceil()))
    }

    /// A press in the text being typed: places the caret, or with `extend`
    /// selects up to there.
    fn shot_field_press(&mut self, point: Point, extend: bool) {
        let Some(field) = self.shot_field_rect() else { return };
        let size = self.shot_text_size();
        let font = self.theme.font.clone();
        let mut text = self.text.borrow_mut();
        if let Some(typing) = self.shot.as_mut().and_then(|shot| shot.typing.as_mut()) {
            typing.input.click(&mut text, &font, size, field.w as f32, (point.x - field.x) as f32, extend);
            typing.edited = Instant::now();
        }
    }

    /// Copies, saves or hands the picture to the Doctor, and closes the
    /// picker. Over the live screen, a still is taken first.
    fn finish_shot(&mut self, finish: Finish) {
        let Some(shot) = self.shot.as_mut() else { return };
        shot.commit_typing();
        if shot.selection.is_none() {
            return;
        }
        if shot.live || shot.still.is_none() {
            shot.waiting = Some(finish);
            self.take_shot_still();
            return;
        }
        self.finish_shot_now(finish);
    }

    /// Finishes with the still there is.
    fn finish_shot_now(&mut self, finish: Finish) {
        let Some(mut shot) = self.shot.take() else { return };
        self.force_keyboard_focus_sync();
        self.schedule_render();
        let (rgba, width, height) = match self.shot_pixels(&shot) {
            Ok(pixels) => pixels,
            Err(error) => {
                self.notify_locally("Screenshot failed", &error, Urgency::Normal);
                if let Some(feedback) = shot.feedback.take() {
                    self.feedback = Some(*feedback);
                }
                return;
            }
        };
        if let Some(feedback) = shot.feedback.take() {
            // Kept for Edit, which opens the same still, area and marks again.
            let draft = shot.draft();
            let includes_wizard = shot.still.as_ref().is_some_and(|still| still.includes_wizard);
            let file = export::encode(&rgba, width, height, Format::Png);
            let picture =
                file.map(|png| crate::feedback::Picture { png, rgba, size: (width, height), draft, includes_wizard });
            self.feedback_attach(*feedback, picture);
            return;
        }
        let preview = thumbnail(&rgba, width, height);
        let export = match finish {
            Finish::Copy(export) | Finish::Save(export) => export,
            Finish::Doctor | Finish::Feedback => Export::DEFAULT,
        };
        let (file, png) = match shot_files(&shot, export, &rgba, width, height) {
            Ok(files) => files,
            Err(error) => {
                self.notify_locally("Screenshot failed", &error, Urgency::Normal);
                return;
            }
        };
        let (out_w, out_h) = export.size.fit(width, height);
        let about = format!("{}, {} x {}, {}", export.format.label(), out_w, out_h, export::file_size(file.len()));
        match finish {
            Finish::Copy(export) => {
                if export.format == Format::Png {
                    self.copy_png(file);
                } else {
                    // Programs that only paste PNG still get a picture.
                    let mut types: Vec<(String, Vec<u8>)> = vec![(export.format.mime().into(), file.to_vec())];
                    if let Some(png) = png {
                        types.push(("image/png".into(), png.to_vec()));
                    }
                    let mime_types = types.iter().map(|(mime_type, _)| mime_type.clone()).collect();
                    self.offer_clipboard(mime_types, Clip::Saved(Arc::from(types)));
                }
                let notification_id = self.notify_locally_with("Screenshot copied", &about, Urgency::Low, vec![]);
                if let Some(preview) = preview {
                    self.set_local_notification_image(notification_id, preview);
                }
                self.close_notification_later(notification_id);
            }
            Finish::Save(export) => {
                let Some(path) = stamped(&screenshots_dir(), "screenshot", export.format.extension()) else {
                    self.notify_locally(
                        "Screenshot not saved",
                        "Could not create the Screenshots folder",
                        Urgency::Normal,
                    );
                    return;
                };
                match std::fs::write(&path, &file) {
                    Ok(()) => self.notify_saved("Screenshot saved", &path, preview),
                    Err(error) => self.notify_locally("Screenshot not saved", &error.to_string(), Urgency::Normal),
                }
            }
            Finish::Doctor => {
                self.open_doctor();
                if self.doctor.open {
                    self.doctor_attach(file);
                }
            }
            // Attached above, while the dialog was at hand.
            Finish::Feedback => {}
        }
    }

    /// The selection with its marks, as opaque RGBA pixels, width and height.
    fn shot_pixels(&self, shot: &Shot) -> Result<(Vec<u8>, u32, u32), String> {
        let still = shot.still.as_ref().ok_or("the screen was not captured")?;
        let selection = shot.selection.ok_or("nothing was selected")?;
        let area = still.pixels(selection).ok_or("the area is off the screen")?;
        let source = still.source();
        let mut pixmap = marks::crop(&source, area).ok_or("the area is off the screen")?;
        let font = self.theme.font.clone();
        let mut text = self.text.borrow_mut();
        marks::draw(
            &mut pixmap,
            (area.0 as i32, area.1 as i32),
            still.scale,
            &shot.marks,
            Some(&source),
            &mut |words, size, colour| text.pixels(words, &font, size, colour, true),
        );
        let (width, height) = (pixmap.width(), pixmap.height());
        Ok((pixmap.take(), width, height))
    }

    /// Says where a file was saved, with buttons to show it in its folder
    /// or open it.
    pub(crate) fn notify_saved(&mut self, summary: &str, path: &Path, preview: Option<Image>) {
        let uri = file_uri(path);
        let folder = path.parent().map(|folder| folder.to_string_lossy().into_owned()).unwrap_or_default();
        // The file manager opens on the folder with the file selected; one
        // without the FileManager1 interface just opens the folder.
        let show = format!(
            "gdbus call --session --dest org.freedesktop.FileManager1 --object-path /org/freedesktop/FileManager1 \
             --method org.freedesktop.FileManager1.ShowItems {} '' >/dev/null 2>&1 || xdg-open {}",
            shell_quote(&format!("['{uri}']")),
            shell_quote(&folder),
        );
        let open = format!("xdg-open {}", shell_quote(&path.to_string_lossy()));
        let buttons = vec![
            ("Show in folder", Some(crate::commands::Command::Spawn(show))),
            ("Open", Some(crate::commands::Command::Spawn(open))),
        ];
        let body = path.to_string_lossy().into_owned();
        let notification_id = self.notify_locally_with(summary, &body, Urgency::Low, buttons);
        if let Some(preview) = preview {
            self.set_local_notification_image(notification_id, preview);
        }
        self.close_notification_later(notification_id);
    }

    /// Closes one of Tileception's own notifications after a while.
    fn close_notification_later(&mut self, notification_id: u32) {
        let timer = smithay::reexports::calloop::timer::Timer::from_duration(std::time::Duration::from_secs(20));
        let closing = self.loop_handle.insert_source(timer, move |_, _, state| {
            state.close_local_notification(notification_id);
            smithay::reexports::calloop::timer::TimeoutAction::Drop
        });
        if let Err(error) = closing {
            tracing::warn!("could not time the screenshot notification: {error}");
        }
    }

    /// The pointer over the picker.
    pub(crate) fn shot_cursor(&self) -> Option<CursorIcon> {
        let shot = self.shot.as_ref()?;
        if !self.shot_here() {
            return Some(CursorIcon::Default);
        }
        let point = self.pointer_point();
        if let Some(layout) = self.shot_layout()
            && layout.buttons.iter().any(|(rect, _)| rect.contains(point))
        {
            return Some(CursorIcon::Pointer);
        }
        if self.shot_field_rect().is_some_and(|field| field.contains(point)) {
            return Some(CursorIcon::Text);
        }
        match shot.drag {
            Some(Drag::Move { .. }) => return Some(CursorIcon::Grabbing),
            Some(Drag::Resize { edges, .. }) => return Some(edge_cursor(edges)),
            Some(_) => return Some(CursorIcon::Crosshair),
            None => {}
        }
        let Some(selection) = shot.selection else { return Some(CursorIcon::Crosshair) };
        if let Some(edges) = edges_at(selection, point) {
            return Some(edge_cursor(edges));
        }
        Some(match (selection.contains(point), shot.tool) {
            (true, Tool::Select) => CursorIcon::Move,
            (true, Tool::Text) => CursorIcon::Text,
            (true, _) | (false, Tool::Select) => CursorIcon::Crosshair,
            (false, _) => CursorIcon::Default,
        })
    }

    /// Where the column of tools, the row of actions and the palette go.
    fn shot_layout(&self) -> Option<Layout> {
        let shot = self.shot.as_ref()?;
        let selection = shot.selection?;
        if shot.drag.is_some_and(|drag| matches!(drag, Drag::New { .. })) {
            return None;
        }
        let screen = self.output_rect();
        let step = BUTTON + GAP;
        let mut column_items: Vec<Option<Control>> = Tool::ALL.iter().map(|tool| Some(Control::Tool(*tool))).collect();
        column_items.extend([None, Some(Control::Colour), Some(Control::Undo), Some(Control::Redo)]);
        let column_h = PAD * 2.0
            + column_items.iter().map(|item| if item.is_some() { step } else { SEPARATOR }).sum::<f64>()
            - GAP;
        let column_w = BUTTON + PAD * 2.0;
        let (column_x, column_right) = if selection.right() + MARGIN + column_w <= screen.right() - 4.0 {
            (selection.right() + MARGIN, true)
        } else if selection.x - MARGIN - column_w >= screen.x + 4.0 {
            (selection.x - MARGIN - column_w, false)
        } else {
            (selection.right() - MARGIN - column_w, true)
        };
        let column_y = selection.y.clamp(screen.y + 4.0, (screen.bottom() - column_h - 4.0).max(screen.y + 4.0));
        let row_items = if shot.feedback.is_some() {
            vec![(Control::Cancel, BUTTON, None), (Control::Attach, ATTACH, None)]
        } else {
            let mut items = vec![
                (Control::Live, BUTTON, None),
                (Control::Record, BUTTON, None),
                (Control::SystemSound, BUTTON, None),
                (Control::Microphone, BUTTON, None),
                (Control::Zoom, BUTTON, None),
            ];
            if self.shell().doctor() {
                items.push((Control::Doctor, BUTTON, None));
            }
            items.extend([
                (Control::Cancel, BUTTON, None),
                (Control::Save, 80.0, Some(Out::Save)),
                (Control::Copy, 82.0, Some(Out::Copy)),
            ]);
            items
        };
        let inset = 4.0_f64.min(screen.w / 4.0);
        let row_pad = PAD.min(screen.w / 8.0);
        let preferred = row_items
            .iter()
            .map(|(control, width, menu)| {
                width
                    + GAP
                    + if menu.is_some() { ARROW } else { 0.0 }
                    + if *control == Control::Cancel && shot.feedback.is_none() { SEPARATOR } else { 0.0 }
            })
            .sum::<f64>()
            - GAP;
        let room = screen.w - 2.0 * (inset + row_pad);
        // A wrapped toolbar can reach into the tool column. Give the two
        // their own horizontal space on narrow screens.
        let reserve = if preferred > room && room >= column_w + MARGIN + BUTTON { column_w + MARGIN } else { 0.0 };
        let column_x = if reserve == 0.0 {
            column_x
        } else if column_right {
            screen.right() - inset - column_w
        } else {
            screen.x + inset
        };
        let column = Rect::new(column_x, column_y, column_w, column_h);
        let available = room - reserve;
        let mut row_buttons = Vec::new();
        let mut row_separator = None;
        let (mut row_x, mut row_y, mut used) = (0.0_f64, 0.0, 0.0_f64);
        for (control, width, menu) in row_items {
            let total = (width + if menu.is_some() { ARROW } else { 0.0 }).min(available);
            let separator = control == Control::Cancel && shot.feedback.is_none();
            let gap = if separator { GAP + SEPARATOR } else { GAP };
            if row_x > 0.0 && row_x + gap + total > available {
                row_x = 0.0;
                row_y += step;
            }
            if row_x > 0.0 {
                if separator {
                    row_separator = Some(Rect::new(row_x + gap / 2.0, row_y + 4.0, 1.0, BUTTON - 8.0));
                }
                row_x += gap;
            }
            // Keep the format arrow attached to its button when wrapping.
            let arrow = if menu.is_some() { ARROW.min(total / 3.0) } else { 0.0 };
            row_buttons.push((Rect::new(row_x, row_y, total - arrow, BUTTON), control));
            if let Some(out) = menu {
                row_buttons.push((Rect::new(row_x + total - arrow, row_y, arrow, BUTTON), Control::Menu(out)));
            }
            row_x += total;
            used = used.max(row_x);
        }
        let row_w = used + 2.0 * row_pad;
        let row_h = row_y + BUTTON + 2.0 * PAD;
        let (row_y, row_below) = if selection.bottom() + MARGIN + row_h <= screen.bottom() - 4.0 {
            (selection.bottom() + MARGIN, true)
        } else if selection.y - MARGIN - row_h >= screen.y + 4.0 {
            (selection.y - MARGIN - row_h, false)
        } else {
            ((selection.bottom() - MARGIN - row_h).max(screen.y + 4.0), true)
        };
        // Its right end lines up with the selection's, away from the column.
        let left = screen.x + inset + if column_right { 0.0 } else { reserve };
        let right = (screen.right() - row_w - inset - if column_right { reserve } else { 0.0 }).max(left);
        let row_x = (selection.right() - row_w).clamp(left, right);
        let row = Rect::new(row_x, row_y, row_w, row_h);
        let place = |rect: Rect| Rect::new(row.x + row_pad + rect.x, row.y + PAD + rect.y, rect.w, rect.h);
        let row_separator = row_separator.map(place);

        let mut buttons = Vec::new();
        let mut cursor = column.y + PAD;
        let mut colour_button = None;
        for item in column_items {
            match item {
                Some(control) => {
                    let rect = Rect::new(column.x + PAD, cursor, BUTTON, BUTTON);
                    if control == Control::Colour {
                        colour_button = Some(rect);
                    }
                    buttons.push((rect, control));
                    cursor += step;
                }
                None => cursor += SEPARATOR,
            }
        }
        buttons.extend(row_buttons.into_iter().map(|(rect, control)| (place(rect), control)));

        let palette = match (shot.palette, colour_button) {
            (true, Some(anchor)) => {
                let count = PALETTE.len() + WIDTHS.len();
                let width = PAD * 2.0 + count as f64 * step - GAP + SEPARATOR;
                let height = BUTTON + PAD * 2.0;
                let left = if column_right && column.right() + MARGIN + width <= screen.right() {
                    column.right() + MARGIN
                } else if column.x - MARGIN - width >= screen.x {
                    column.x - MARGIN - width
                } else {
                    column.right() + MARGIN
                };
                let rect = Rect::new(left, anchor.y - PAD, width, height);
                let mut cursor = rect.x + PAD;
                for index in 0..PALETTE.len() {
                    buttons.push((Rect::new(cursor, rect.y + PAD, BUTTON, BUTTON), Control::Swatch(index)));
                    cursor += step;
                }
                cursor += SEPARATOR - GAP;
                for index in 0..WIDTHS.len() {
                    buttons.push((Rect::new(cursor, rect.y + PAD, BUTTON, BUTTON), Control::Width(index)));
                    cursor += step;
                }
                Some(rect)
            }
            _ => None,
        };
        let menu = shot.menu.and_then(|out| {
            let (anchor, _) = buttons.iter().find(|(_, control)| *control == Control::Menu(out))?;
            let height = MenuParts::height();
            let width = MENU_W.min(screen.w - 2.0 * inset);
            let min_x = screen.x + inset;
            let left = (anchor.right() - width).clamp(min_x, (screen.right() - width - inset).max(min_x));
            let below = row.bottom() + 6.0;
            let above = row.y - 6.0 - height;
            let fits_below = below + height <= screen.bottom() - 4.0;
            let fits_above = above >= screen.y + 4.0;
            let top = match (row_below, fits_below, fits_above) {
                (true, true, _) | (false, true, false) => below,
                (_, _, true) => above,
                _ => (screen.bottom() - height - 4.0).max(screen.y + 4.0),
            };
            let rect = Rect::new(left, top, width, height);
            let parts = MenuParts::new(
                rect,
                self.config.file.capture.qualities().len(),
                self.config.file.capture.sizes().len(),
            );
            for (index, rect) in parts.choices.iter().enumerate() {
                buttons.push((*rect, Control::Choice(index)));
            }
            for (index, rect) in parts.sizes.iter().enumerate() {
                buttons.push((*rect, Control::Scale(index)));
            }
            Some(rect)
        });
        // The palette and the menu are on top, so they are looked at first.
        buttons.reverse();
        Some(Layout { column, row, row_separator, palette, menu, column_right, row_below, buttons })
    }

    /// The marks drawn for the screen, redrawn when they change.
    fn shot_overlay(&self, shot: &Shot) -> Option<(MemoryRenderBuffer, (f64, f64))> {
        if let Some(overlay) = shot.overlay.borrow().as_ref()
            && overlay.generation == shot.generation
        {
            return overlay.buffer.clone().map(|buffer| (buffer, overlay.position));
        }
        let selection = shot.selection?;
        let scale = self.scale;
        let drawn = if shot.marks.is_empty() {
            None
        } else {
            let area = crate::render::physical(&selection, scale);
            let mut pixmap = resvg::tiny_skia::Pixmap::new(area.size.w.max(1) as u32, area.size.h.max(1) as u32)?;
            let font = self.theme.font.clone();
            let mut text = self.text.borrow_mut();
            // Live, there is nothing to redact from yet: grey blocks show where.
            let source = shot.still.as_ref().filter(|_| !shot.live).map(|still| still.source());
            marks::draw(
                &mut pixmap,
                (area.loc.x, area.loc.y),
                scale,
                &shot.marks,
                source.as_ref(),
                &mut |words, size, colour| text.pixels(words, &font, size, colour, true),
            );
            let buffer = MemoryRenderBuffer::from_slice(
                pixmap.data(),
                Fourcc::Abgr8888,
                (pixmap.width() as i32, pixmap.height() as i32),
                1,
                Transform::Normal,
                None,
            );
            Some(buffer)
        };
        let position = (
            f64::from(crate::render::physical(&selection, scale).loc.x) / scale,
            f64::from(crate::render::physical(&selection, scale).loc.y) / scale,
        );
        *shot.overlay.borrow_mut() = Some(Overlay { generation: shot.generation, buffer: drawn.clone(), position });
        drawn.map(|buffer| (buffer, position))
    }

    /// The picker: the still, the marks, the dimmed surroundings, the
    /// selection's outline and handles, and the toolbars.
    pub fn shot_elements(&self, renderer: &mut GlesRenderer) -> Vec<OutputElement> {
        let Some(shot) = self.shot.as_ref().filter(|shot| !shot.hidden) else { return Vec::new() };
        if !self.shot_here() || (!shot.live && shot.still.is_none()) {
            return Vec::new();
        }
        let theme = &self.theme;
        let screen = self.output_rect();
        let point = self.pointer_point();
        let overlay = self.shot_overlay(shot);
        let layout = self.shot_layout();
        let mut painter = Painter::new(self, renderer);
        if let Some(still) = shot.still.as_ref().filter(|_| !shot.live) {
            painter.image(&still.buffer, (0.0, 0.0));
        }
        if let Some((buffer, position)) = overlay {
            painter.image(&buffer, position);
        }
        let dim = Colour([0.0, 0.0, 0.0, 0.5]);
        match shot.selection {
            Some(selection) => {
                painter.fill(Rect::new(screen.x, screen.y, screen.w, selection.y - screen.y), dim, 0.0);
                painter.fill(
                    Rect::new(screen.x, selection.bottom(), screen.w, screen.bottom() - selection.bottom()),
                    dim,
                    0.0,
                );
                painter.fill(Rect::new(screen.x, selection.y, selection.x - screen.x, selection.h), dim, 0.0);
                painter.fill(
                    Rect::new(selection.right(), selection.y, screen.right() - selection.right(), selection.h),
                    dim,
                    0.0,
                );
                let outline =
                    Shape { border_width: 1.0, border: theme.accent, ..Shape::filled(Colour::TRANSPARENT, 0.0) };
                painter.shape(
                    Rect::new(selection.x - 1.0, selection.y - 1.0, selection.w + 2.0, selection.h + 2.0),
                    outline,
                );
                self.shot_handles(&mut painter, selection);
                self.shot_size_label(&mut painter, shot, selection);
            }
            None => {
                painter.fill(screen, dim.alpha(0.35), 0.0);
                if shot.drag.is_none()
                    && let Some(window) = self.window_rect_at(point)
                {
                    let glow = Shape {
                        border_width: 2.0,
                        border: theme.accent,
                        glow_radius: theme.glow,
                        glow: theme.accent.alpha(0.3),
                        ..Shape::filled(theme.accent.alpha(0.08), theme.radius)
                    };
                    painter.shape(window, glow);
                }
                self.shot_hint(&mut painter, screen);
            }
        }
        if let Some(field) = self.shot_field_rect()
            && let Some(typing) = shot.typing.as_ref()
        {
            let colour = opaque(shot.colour());
            let size = self.shot_text_size();
            let image = self.text.borrow_mut().field(
                &typing.input.text,
                &theme.font,
                size,
                colour,
                field.w as f32,
                crate::text::Field {
                    cursor: typing.input.cursor,
                    anchor: typing.input.anchor,
                    visible: typing.edited.elapsed().as_millis() % 1000 < 600,
                    accent: theme.accent.straight_rgba8(),
                },
            );
            let width = image.as_ref().and_then(|image| image.ink.map(|ink| ink.right)).unwrap_or(0.0) + 12.0;
            let outline =
                Shape { border_width: 1.0, border: theme.accent.alpha(0.6), ..Shape::filled(Colour::TRANSPARENT, 3.0) };
            painter.shape(Rect::new(field.x - 4.0, field.y - 2.0, width.max(24.0) + 4.0, field.h + 4.0), outline);
            if let Some(image) = image {
                painter.image(&image.buffer, (field.x, field.y));
            }
        }
        if let Some(layout) = layout {
            self.shot_toolbars(&mut painter, shot, &layout, point);
        }
        if shot.selection.is_none()
            || shot.drag.is_some_and(|drag| matches!(drag, Drag::New { .. } | Drag::Resize { .. }))
        {
            self.shot_magnifier(&mut painter, shot, point);
        }
        painter.finish()
    }

    fn shot_handles(&self, painter: &mut Painter<'_>, selection: Rect) {
        let theme = &self.theme;
        let side = 8.0;
        let (middle_x, middle_y) = (selection.x + selection.w / 2.0, selection.y + selection.h / 2.0);
        for (x_pos, y_pos) in [
            (selection.x, selection.y),
            (middle_x, selection.y),
            (selection.right(), selection.y),
            (selection.x, middle_y),
            (selection.right(), middle_y),
            (selection.x, selection.bottom()),
            (middle_x, selection.bottom()),
            (selection.right(), selection.bottom()),
        ] {
            let handle = Rect::new(x_pos - side / 2.0, y_pos - side / 2.0, side, side);
            painter.shape(
                handle,
                Shape { border_width: 2.0, border: theme.on_accent, ..Shape::filled(theme.accent, 2.0) },
            );
        }
    }

    /// The selection's size in pixels, above its top left corner.
    fn shot_size_label(&self, painter: &mut Painter<'_>, shot: &Shot, selection: Rect) {
        let theme = &self.theme;
        let physical = crate::render::physical(&selection, shot.still.as_ref().map_or(self.scale, |still| still.scale));
        let label = format!("{} x {}", physical.size.w, physical.size.h);
        let style = Style::new(theme.font_size * 0.85, theme.text).align(Align::Centre);
        let (text_w, _) = painter.measure(&label, 300.0, style);
        let (width, height) = (text_w + 14.0, 22.0);
        let top = if selection.y - height - 6.0 >= self.output_rect().y {
            selection.y - height - 6.0
        } else {
            selection.y + 6.0
        };
        let left = if selection.y - height - 6.0 >= self.output_rect().y { selection.x } else { selection.x + 6.0 };
        let rect = Rect::new(left, top, width, height);
        painter.shape(
            rect,
            Shape { border_width: 1.0, border: theme.border, ..Shape::filled(theme.surface.alpha(0.92), 5.0) },
        );
        painter.text(&label, rect, style);
    }

    /// What to do, at the top of the screen, before anything is picked.
    fn shot_hint(&self, painter: &mut Painter<'_>, screen: Rect) {
        let feedback = self.shot.as_ref().is_some_and(|shot| shot.feedback.is_some());
        let hints: &[(&str, &str)] = if feedback {
            &[("Drag", "an area"), ("Click", "a window"), ("Esc", "back to feedback")]
        } else {
            &[("Drag", "an area"), ("Click", "a window"), ("F", "live screen"), ("Esc", "cancel")]
        };
        let width = painter.key_hints(hints, 0.0, 0.0, true) + 32.0;
        let rect = Rect::new(screen.x + (screen.w - width) / 2.0, screen.y + 48.0, width, 40.0);
        painter.popup(rect);
        painter.key_hints(hints, rect.x + 16.0, rect.y + rect.h / 2.0, false);
    }

    fn shot_toolbars(&self, painter: &mut Painter<'_>, shot: &Shot, layout: &Layout, point: Point) {
        let theme = &self.theme;
        painter.popup(layout.column);
        painter.popup(layout.row);
        if let Some(palette) = layout.palette {
            painter.popup(palette);
        }
        if let Some(menu) = layout.menu {
            self.shot_menu(painter, shot, menu, point);
        }
        // Separators, where the buttons leave a gap.
        let column_gap = layout
            .buttons
            .iter()
            .find(|(_, control)| *control == Control::Colour)
            .map(|(rect, _)| rect.y - SEPARATOR / 2.0 - GAP / 2.0);
        if let Some(y_pos) = column_gap {
            painter.fill(Rect::new(layout.column.x + 8.0, y_pos, layout.column.w - 16.0, 1.0), theme.border, 0.0);
        }
        if let Some(separator) = layout.row_separator {
            painter.fill(separator, theme.border, 0.0);
        }
        let mut hovered = None;
        for (rect, control) in &layout.buttons {
            // The menu draws its own.
            if matches!(control, Control::Choice(_) | Control::Scale(_)) {
                continue;
            }
            let hover = rect.contains(point) && shot.drag.is_none();
            if hover {
                hovered = Some((*rect, *control));
            }
            let pressed = shot.pressed == Some(*control) && hover;
            let button = match control {
                Control::Tool(tool) => {
                    Button::glyph(tool_glyph(*tool), if shot.tool == *tool { Look::Primary } else { Look::Quiet })
                }
                Control::Colour => Button::swatch(opaque(shot.colour()), Look::Quiet),
                Control::Swatch(index) => {
                    Button::swatch(opaque(PALETTE[*index]), Look::Quiet).focused(shot.colour == *index)
                }
                Control::Width(index) => {
                    let glyph = [glyph::WIDTH_THIN, glyph::WIDTH_MEDIUM, glyph::WIDTH_THICK][*index];
                    Button::glyph(glyph, if shot.width == *index { Look::Primary } else { Look::Quiet })
                }
                Control::Undo => Button::glyph(glyph::UNDO, Look::Quiet).enabled(!shot.marks.is_empty()),
                Control::Redo => Button::glyph(glyph::REDO, Look::Quiet).enabled(!shot.undone.is_empty()),
                Control::Live => Button::glyph(if shot.live { glyph::EYE } else { glyph::SNOWFLAKE }, Look::Quiet),
                Control::Record => Button::glyph(glyph::RECORD, Look::Quiet),
                Control::SystemSound => {
                    let (sign, look) = if shot.sources.system {
                        (glyph::VOLUME_HIGH, Look::Plain)
                    } else {
                        (glyph::VOLUME_OFF, Look::Quiet)
                    };
                    Button::glyph(sign, look).focused(shot.sources.system)
                }
                Control::Microphone => {
                    let (sign, look) =
                        if shot.sources.microphone { (glyph::MIC, Look::Plain) } else { (glyph::MIC_OFF, Look::Quiet) };
                    Button::glyph(sign, look).focused(shot.sources.microphone)
                }
                Control::Zoom => Button::glyph(glyph::ZOOM_IN, Look::Quiet),
                Control::Attach => Button::primary("Attach to feedback").icon(glyph::CHECK),
                Control::Doctor => Button::glyph(glyph::CHAT, Look::Quiet),
                Control::Cancel => Button::glyph(glyph::CLOSE, Look::Quiet),
                Control::Save | Control::Copy => {
                    // One split button, with the arrow that opens its menu.
                    let out = if *control == Control::Copy { Out::Copy } else { Out::Save };
                    let Some((arrow, _)) = layout.buttons.iter().find(|(_, item)| *item == Control::Menu(out)) else {
                        continue;
                    };
                    let arrow_pressed = shot.pressed == Some(Control::Menu(out));
                    let split = Split {
                        width: arrow.w,
                        hover: arrow.contains(point) && shot.drag.is_none() || arrow_pressed,
                        open: shot.menu == Some(out),
                        marked: false,
                    };
                    let whole = Rect::new(rect.x, rect.y, arrow.right() - rect.x, rect.h);
                    let button = if out == Out::Copy {
                        Button::primary("Copy").icon(glyph::COPY)
                    } else {
                        Button::plain("Save").icon(glyph::DOWNLOAD)
                    };
                    painter.button(whole, button.split(split), hover || pressed);
                    continue;
                }
                // Drawn as part of Copy or Save.
                Control::Menu(_) => continue,
                Control::Choice(_) | Control::Scale(_) => continue,
            };
            painter.button(*rect, button, hover || pressed);
        }
        let Some((rect, control)) = hovered else { return };
        let label = control.label(shot);
        if label.is_empty() {
            return;
        }
        let style = Style::new(theme.font_size * 0.85, theme.text).align(Align::Centre);
        let (text_w, _) = painter.measure(label, 400.0, style);
        let screen = self.output_rect();
        let (width, height) = ((text_w + 16.0).min((screen.w - 4.0).max(1.0)), 26.0);
        let in_column = layout.column.contains(Point::new(rect.x + 1.0, rect.y + 1.0));
        let in_palette = layout.palette.is_some_and(|palette| palette.contains(Point::new(rect.x + 1.0, rect.y + 1.0)));
        let tip = if in_column {
            let left = if layout.column_right && layout.column.right() + 6.0 + width <= screen.right() {
                layout.column.right() + 6.0
            } else {
                layout.column.x - 6.0 - width
            };
            Rect::new(left, rect.y + (rect.h - height) / 2.0, width, height)
        } else {
            let below = if in_palette { true } else { layout.row_below };
            let top = if below { rect.bottom() + PAD + 6.0 } else { rect.y - PAD - 6.0 - height };
            let left = (rect.x + rect.w / 2.0 - width / 2.0)
                .clamp(screen.x + 2.0, (screen.right() - width - 2.0).max(screen.x + 2.0));
            Rect::new(left, top, width, height)
        };
        painter.shape(tip, Shape { border_width: 1.0, border: theme.border, ..Shape::filled(theme.background, 5.0) });
        painter.text(label, tip, style);
    }

    /// The menu beside Copy or Save: the size switch on top, then PNG and
    /// WebP, and JPG with a button for each quality, each with how big its
    /// file is. Only the size is a setting; every other choice acts.
    fn shot_menu(&self, painter: &mut Painter<'_>, shot: &Shot, menu: Rect, point: Point) {
        let theme = &self.theme;
        let formats = self.shot_formats();
        let sizes = self.config.file.capture.sizes();
        let parts = MenuParts::new(menu, formats.len() - 2, sizes.len());
        painter.popup(menu);
        let muted = Style::new(theme.font_size * 0.85, theme.muted);
        let title = if shot.menu == Some(Out::Save) { "Save as" } else { "Copy as" };
        painter.text(title, parts.title, muted);
        // The size switch: a recessed track with the chosen size raised in it.
        painter.shape(
            parts.track,
            Shape { border_width: 1.0, border: theme.border, ..Shape::filled(theme.background, theme.radius) },
        );
        painter.text("Size", parts.size_label, muted);
        let scale = shot.scale.min(sizes.len() - 1);
        // One thumbnail size needs no number to tell it apart.
        let thumbnails = sizes.iter().filter(|size| matches!(size, Size::Longest(_))).count();
        for (index, rect) in parts.sizes.iter().enumerate() {
            let look = if index == scale { Look::Plain } else { Look::Quiet };
            let label = match sizes[index] {
                Size::Longest(_) if thumbnails == 1 => "Thumb".to_string(),
                size => size.label(),
            };
            painter.button(*rect, Button::new(&label, look), rect.contains(point));
        }
        let made = shot.files.as_ref().and_then(|files| files.made.lock().ok().map(|made| made.clone()));
        let weight = |index: usize| match made.as_ref().and_then(|made| made.get(index).cloned().flatten()) {
            Some(Ok(file)) => export::file_size(file.len()),
            Some(Err(_)) => "failed".to_string(),
            None => "...".to_string(),
        };
        // The keyboard's choice shows while the pointer is elsewhere.
        let lit = |index: usize, rect: &Rect| rect.contains(point) || (shot.menu_row == index && !menu.contains(point));
        let bold = Style::new(theme.font_size, theme.text).bold();
        for (index, name, note) in [(0, "PNG", ""), (1, "WebP", "lossless")] {
            let rect = parts.choices[index];
            if lit(index, &rect) {
                painter.shape(
                    rect,
                    Shape {
                        border_width: 1.0,
                        border: theme.accent.alpha(0.35),
                        ..Shape::filled(theme.accent.alpha(0.14), theme.radius)
                    },
                );
            }
            let inside = Rect::new(rect.x + 10.0, rect.y, rect.w - 20.0, rect.h);
            let (name_w, _) = painter.text(name, inside, bold);
            if !note.is_empty() {
                painter.text(note, Rect::new(inside.x + name_w + 10.0, inside.y, 80.0, inside.h), muted);
            }
            let size = weight(index);
            let (size_w, _) = painter.measure(&size, inside.w, muted);
            painter.text(&size, Rect::new(inside.right() - size_w, inside.y, size_w + 1.0, inside.h), muted);
        }
        let jpg = Rect::new(parts.jpg_row.x + 10.0, parts.jpg_row.y, 60.0, parts.jpg_row.h);
        painter.text("JPG", jpg, bold);
        for (index, rect) in parts.choices.iter().enumerate().skip(2) {
            let Format::Jpg(quality) = formats[index] else { continue };
            let label = quality.to_string();
            let size = weight(index);
            let look = if lit(index, rect) { Look::Primary } else { Look::Plain };
            painter.button(*rect, Button::new(&label, look).detail(&size), rect.contains(point));
        }
    }

    /// A loupe by the pointer, with its position and the colour under it.
    fn shot_magnifier(&self, painter: &mut Painter<'_>, shot: &Shot, point: Point) {
        let Some(still) = shot.still.as_ref().filter(|_| !shot.live) else { return };
        let theme = &self.theme;
        let screen = self.output_rect();
        let side = f64::from(ZOOM_PIXELS) * ZOOM_CELL;
        let label_h = 40.0;
        let gap = 22.0;
        let (width, height) = (side + 12.0, side + 12.0 + label_h);
        let mut left = point.x + gap;
        if left + width > screen.right() {
            left = point.x - gap - width;
        }
        let mut top = point.y + gap;
        if top + height > screen.bottom() {
            top = point.y - gap - height;
        }
        let card = Rect::new(left, top, width, height);
        painter.popup(card);
        let (centre_x, centre_y) = ((point.x * still.scale).floor() as i32, (point.y * still.scale).floor() as i32);
        let half = ZOOM_PIXELS / 2;
        for row in 0..ZOOM_PIXELS {
            for column in 0..ZOOM_PIXELS {
                let colour = still
                    .pixel(centre_x + column - half, centre_y + row - half)
                    .map_or(Colour([0.0, 0.0, 0.0, 1.0]), opaque);
                let cell = Rect::new(
                    card.x + 6.0 + f64::from(column) * ZOOM_CELL,
                    card.y + 6.0 + f64::from(row) * ZOOM_CELL,
                    ZOOM_CELL,
                    ZOOM_CELL,
                );
                painter.fill(cell, colour, 0.0);
            }
        }
        // Crosshairs through the pixel under the pointer.
        let middle_x = card.x + 6.0 + f64::from(half) * ZOOM_CELL;
        let middle_y = card.y + 6.0 + f64::from(half) * ZOOM_CELL;
        let line = theme.accent.alpha(0.55);
        painter.fill(Rect::new(card.x + 6.0, middle_y + ZOOM_CELL / 2.0 - 0.5, side, 1.0), line, 0.0);
        painter.fill(Rect::new(middle_x + ZOOM_CELL / 2.0 - 0.5, card.y + 6.0, 1.0, side), line, 0.0);
        painter.shape(
            Rect::new(middle_x - 1.0, middle_y - 1.0, ZOOM_CELL + 2.0, ZOOM_CELL + 2.0),
            Shape { border_width: 1.0, border: Colour([1.0; 4]), ..Shape::filled(Colour::TRANSPARENT, 0.0) },
        );
        let style = Style::new(theme.font_size * 0.85, theme.text).align(Align::Centre);
        let position = format!("{}, {}", centre_x, centre_y);
        painter.text(&position, Rect::new(card.x, card.y + 6.0 + side + 2.0, card.w, 18.0), style);
        if let Some(rgb) = still.pixel(centre_x, centre_y) {
            let [hex, ..] = crate::answers::colour_notations(rgb);
            painter.text(&hex, Rect::new(card.x, card.y + 6.0 + side + 19.0, card.w, 18.0), style.colour(theme.muted));
        }
    }
}
