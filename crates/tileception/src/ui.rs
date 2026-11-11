//! Shared text and icon drawing for the compositor's interface.

use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    path::{Path, PathBuf},
    rc::Rc,
};

use smithay::input::keyboard::xkb;
use smithay::{
    backend::{
        allocator::Fourcc,
        renderer::{element::memory::MemoryRenderBuffer, gles::GlesRenderer},
    },
    utils::Transform,
};
use tileception_core::Rect;
use zeroize::Zeroizing;

use crate::{
    Tileception,
    render::{OutputElement, push_image},
    shapes::{Shape, Shapes},
    theme::{ButtonStyle, Colour},
};

/// A control's stable identity, bounds and pointer affordance. Buttons arm on
/// press and only activate after release over the same target.
#[derive(Clone)]
pub(crate) struct PointerTarget {
    pub rect: Rect,
    pub cursor: smithay::input::pointer::CursorIcon,
    on_press: bool,
    action: Rc<dyn ErasedPointerAction>,
}

/// Components supply their own action type and activation handler.
pub(crate) trait PointerAction: PartialEq + 'static {
    fn activate(&self, state: &mut Tileception, rect: Rect, point: tileception_core::Point);

    /// A middle click on this. A text field takes it: it places its caret
    /// there and pastes the primary selection (what was last selected
    /// anywhere), with `Tileception::paste_primary`. Returns whether it did.
    fn middle_click(&self, _state: &mut Tileception, _rect: Rect, _point: tileception_core::Point) -> bool {
        false
    }
}

// Erasure lets the input stack hold a button from any component. Equality is
// still checked using the concrete action type, never a string or numeric tag.
trait ErasedPointerAction {
    fn as_any(&self) -> &dyn std::any::Any;
    fn same(&self, other: &dyn ErasedPointerAction) -> bool;
    fn activate(&self, state: &mut Tileception, rect: Rect, point: tileception_core::Point);
    fn middle_click(&self, state: &mut Tileception, rect: Rect, point: tileception_core::Point) -> bool;
}

impl<Action: PointerAction> ErasedPointerAction for Action {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
    fn same(&self, other: &dyn ErasedPointerAction) -> bool {
        other.as_any().downcast_ref::<Action>() == Some(self)
    }
    fn activate(&self, state: &mut Tileception, rect: Rect, point: tileception_core::Point) {
        PointerAction::activate(self, state, rect, point);
    }
    fn middle_click(&self, state: &mut Tileception, rect: Rect, point: tileception_core::Point) -> bool {
        PointerAction::middle_click(self, state, rect, point)
    }
}

impl PartialEq for PointerTarget {
    fn eq(&self, other: &Self) -> bool {
        self.rect == other.rect
            && self.cursor == other.cursor
            && self.on_press == other.on_press
            && self.action.same(other.action.as_ref())
    }
}

pub(crate) enum PointerEvent {
    Press(PointerTarget),
    Release(PointerTarget),
    Captured,
    Unrelated,
}

impl PointerTarget {
    pub fn new(rect: Rect, action: impl PointerAction, cursor: smithay::input::pointer::CursorIcon) -> Self {
        Self { rect, action: Rc::new(action), cursor, on_press: true }
    }

    pub fn button(rect: Rect, action: impl PointerAction) -> Self {
        Self { rect, action: Rc::new(action), cursor: smithay::input::pointer::CursorIcon::Pointer, on_press: false }
    }

    pub fn on_press(&self) -> bool {
        self.on_press
    }

    pub fn activate(&self, state: &mut Tileception, point: tileception_core::Point) {
        self.action.activate(state, self.rect, point);
    }

    pub fn middle_click(&self, state: &mut Tileception, point: tileception_core::Point) -> bool {
        self.action.middle_click(state, self.rect, point)
    }

    pub fn button_event(armed: &mut Option<Self>, target: Option<Self>, pressed: bool) -> PointerEvent {
        if pressed {
            *armed = None;
            match target {
                Some(target) if target.on_press => PointerEvent::Press(target),
                Some(target) => {
                    *armed = Some(target);
                    PointerEvent::Captured
                }
                None => PointerEvent::Unrelated,
            }
        } else {
            match armed.take() {
                Some(pressed) if target.as_ref() == Some(&pressed) => PointerEvent::Release(pressed),
                Some(_) => PointerEvent::Captured,
                None => PointerEvent::Unrelated,
            }
        }
    }
}

impl Tileception {
    pub(crate) fn shell_pointer_target(&self, point: tileception_core::Point) -> Option<PointerTarget> {
        if !self.is_locked() && self.feedback.is_some() {
            return self.feedback_pointer_target(point);
        }
        if self.is_locked() || self.overlay_under(point) {
            return None;
        }
        if self.password_prompt.is_some() {
            return self.password_prompt_pointer_target(point);
        }
        if self.power_menu.is_some() {
            return self.power_pointer_target(point);
        }
        if let Some(target) = self.handoff_pointer_target(point) {
            return Some(target);
        }
        if self.tray.open {
            return self.tray_pointer_target(point);
        }
        if self.launcher.open && !self.launcher.home && self.launcher_takes(point) {
            return self.launcher_pointer_target(point);
        }
        if self.doctor_contains(point) {
            return self.doctor_pointer_target(point);
        }
        if self.notifications_contain(point) {
            return self.notification_pointer_target(point);
        }
        // The network card opens over the welcome screen.
        if self.network.zoom.open {
            return self.bar_pointer_target(point);
        }
        if self.launcher.open && self.launcher_takes(point) {
            return self.launcher_pointer_target(point);
        }
        if let Some(target) = self.duck_pointer_target(point) {
            return Some(target);
        }
        if self.bar_takes(point) {
            return self.bar_pointer_target(point);
        }
        if self.strip_takes(point) {
            return self.strip_pointer_target(point);
        }
        None
    }

    pub(crate) fn shell_pointer_button(&mut self, pressed: bool) -> bool {
        let point = tileception_core::Point::new(self.pointer.x, self.pointer.y);
        let target = self.shell_pointer_target(point);
        match PointerTarget::button_event(&mut self.ui_pressed, target, pressed) {
            PointerEvent::Release(target) => target.activate(self, point),
            PointerEvent::Captured => {}
            PointerEvent::Press(_) | PointerEvent::Unrelated => return false,
        }
        self.schedule_render();
        true
    }

    /// A middle button over the compositor's own controls: pressed on a
    /// text field, it pastes the primary selection there. Returns whether
    /// the button was taken.
    pub(crate) fn shell_middle_button(&mut self, pressed: bool) -> bool {
        if !pressed {
            return std::mem::take(&mut self.middle_pasted);
        }
        let point = tileception_core::Point::new(self.pointer.x, self.pointer.y);
        self.middle_pasted = self.shell_pointer_target(point).is_some_and(|target| target.middle_click(self, point));
        if self.middle_pasted {
            self.schedule_render();
        }
        self.middle_pasted
    }

    pub(crate) fn shell_pointer_cursor(&self) -> Option<smithay::input::pointer::CursorIcon> {
        let point = tileception_core::Point::new(self.pointer.x, self.pointer.y);
        if self.is_locked() || (self.feedback.is_none() && self.overlay_under(point)) {
            return None;
        }
        if self.panel_reordering() {
            return Some(smithay::input::pointer::CursorIcon::Grabbing);
        }
        if let Some(target) = self.shell_pointer_target(point) {
            return Some(target.cursor);
        }
        (self.feedback.is_some()
            || self.password_prompt.is_some()
            || self.power_menu.is_some()
            || (self.launcher.open && self.launcher_takes(point))
            || self.doctor_contains(point)
            || self.notifications_contain(point)
            || self.bar_takes(point))
        .then_some(smithay::input::pointer::CursorIcon::Default)
    }

    pub(crate) fn shell_pointer_feedback(&self, renderer: &mut GlesRenderer) -> Vec<OutputElement> {
        let point = tileception_core::Point::new(self.pointer.x, self.pointer.y);
        let mut painter = Painter::new(self, renderer);
        if let Some(target) = self.shell_pointer_target(point)
            && !target.on_press()
        {
            painter.fill(
                target.rect,
                self.theme.text.alpha(if self.ui_pressed.as_ref() == Some(&target) { 0.12 } else { 0.04 }),
                5.,
            );
        }
        painter.finish()
    }
}

/// Stroked line icons on a 24 by 24 grid (the Tide set from `docs/design.html`).
pub mod glyph {
    pub const SETTINGS: &str = r#"<path d="M10.1 5.1L10.2 2.7L13.8 2.7L13.9 5.1L15.5 5.7L17.3 4.1L19.9 6.7L18.3 8.5L18.9 10.1L21.3 10.2L21.3 13.8L18.9 13.9L18.3 15.5L19.9 17.3L17.3 19.9L15.5 18.3L13.9 18.9L13.8 21.3L10.2 21.3L10.1 18.9L8.5 18.3L6.7 19.9L4.1 17.3L5.7 15.5L5.1 13.9L2.7 13.8L2.7 10.2L5.1 10.1L5.7 8.5L4.1 6.7L6.7 4.1L8.5 5.7z"/><circle cx="12" cy="12" r="3"/>"#;
    pub const HOME: &str = r#"<path d="M3 11l9-7 9 7"/><path d="M5 10v10h14V10"/><path d="M10 20v-5h4v5"/>"#;
    pub const BULB: &str = r#"<path d="M9 18h6M10 21h4"/><path d="M12 3a6 6 0 0 0-3.5 10.9c.6.5 1 1.2 1 2V16h5v-.1c0-.8.4-1.5 1-2A6 6 0 0 0 12 3z"/>"#;
    pub const PLUG: &str = r#"<path d="M9 3v5M15 3v5"/><path d="M6 8h12v3a6 6 0 0 1-12 0z"/><path d="M12 17v4"/>"#;
    pub const FLAME: &str = r#"<path d="M12 3s5 4.5 5 9.5A5 5 0 0 1 7 12.5C7 10 9 9 9 9s0 2.5 2 3c0-3.5 1-6.5 1-9z"/>"#;
    pub const SNOWFLAKE: &str = r#"<path d="M12 2v20M4 7l16 10M20 7L4 17"/><path d="M9 4l3 2 3-2M9 20l3-2 3 2"/>"#;
    pub const DROP: &str = r#"<path d="M12 3s6 6.5 6 11a6 6 0 0 1-12 0c0-4.5 6-11 6-11z"/>"#;
    pub const THERMOMETER: &str = r#"<path d="M14 14.8V5a2 2 0 0 0-4 0v9.8a4 4 0 1 0 4 0z"/><path d="M12 11v6"/>"#;
    pub const FAN: &str = r#"<circle cx="12" cy="12" r="2"/><path d="M12 10c0-4 1-7 4-7 2 0 3 2 2 4-1 2-4 3-6 3zM14 12c4 0 7 1 7 4 0 2-2 3-4 2-2-1-3-4-3-6zM12 14c0 4-1 7-4 7-2 0-3-2-2-4 1-2 4-3 6-3zM10 12c-4 0-7-1-7-4 0-2 2-3 4-2 2 1 3 4 3 6z"/>"#;
    pub const WIFI_OFF: &str = r#"<path d="M2 9a15 15 0 0 1 6-3.4M22 9a15 15 0 0 0-9-3.9M5 12.5a10 10 0 0 1 4-2.3M8.5 16a5 5 0 0 1 7 0M3 3l18 18"/>"#;
    /// Wi-Fi at full strength: a narrow fan that keeps to the text's height.
    /// The others light fewer arcs.
    pub const WIFI: &str = r#"<path d="M3.4 9.3a12.5 12.5 0 0 1 17.2 0M6.2 12.4a8.5 8.5 0 0 1 11.6 0M9 15.5a4.5 4.5 0 0 1 6 0M12 19h.01"/>"#;
    pub const WIFI_2: &str = r#"<path d="M6.2 12.4a8.5 8.5 0 0 1 11.6 0M9 15.5a4.5 4.5 0 0 1 6 0M12 19h.01"/>"#;
    pub const WIFI_1: &str = r#"<path d="M9 15.5a4.5 4.5 0 0 1 6 0M12 19h.01"/>"#;
    pub const WIFI_0: &str = r#"<path d="M12 19h.01"/>"#;
    /// Wi-Fi switched off or offline: the fan faded and struck through.
    pub const WIFI_DISABLED: &str = r#"<path d="M3.4 9.3a12.5 12.5 0 0 1 17.2 0M6.2 12.4a8.5 8.5 0 0 1 11.6 0M9 15.5a4.5 4.5 0 0 1 6 0" stroke-opacity=".35"/><path d="M4 4l16 16"/>"#;
    /// A wired network: three linked boxes.
    pub const ETHERNET: &str = r#"<rect x="9" y="2" width="6" height="6" rx="1"/><rect x="3" y="16" width="6" height="6" rx="1"/><rect x="15" y="16" width="6" height="6" rx="1"/><path d="M12 8v4M6 16v-4h12v4"/>"#;
    pub const BLUETOOTH: &str = r#"<path d="M7 7l10 10-5 5V2l5 5L7 17"/>"#;
    pub const MINUS: &str = r#"<path d="M5 12h14"/>"#;
    pub const PLUS: &str = r#"<path d="M12 5v14M5 12h14"/>"#;
    pub const LOCK: &str = r#"<rect x="5" y="11" width="14" height="10" rx="2"/><path d="M8 11V7a4 4 0 0 1 8 0v4"/>"#;
    pub const LOG_OUT: &str = r#"<path d="M16 17l5-5-5-5M21 12H9M12 21H5V3h7"/>"#;
    pub const KEYBOARD: &str = r#"<rect x="2" y="6" width="20" height="12" rx="2"/><path d="M6 10h.01M10 10h.01M14 10h.01M18 10h.01M7 14h10"/>"#;
    pub const MOON: &str = r#"<path d="M21 12.8A9 9 0 1 1 11.2 3a7 7 0 0 0 9.8 9.8z"/>"#;
    pub const RESTART: &str = r#"<path d="M3 12a9 9 0 1 0 3-6.7L3 8M3 3v5h5"/>"#;
    pub const POWER: &str = r#"<path d="M12 2v10M18.4 6.6a9 9 0 1 1-12.8 0"/>"#;
    pub const SEARCH: &str = r#"<circle cx="11" cy="11" r="7"/><path d="M20 20l-4-4"/>"#;
    pub const BELL: &str = r#"<path d="M6 8a6 6 0 0 1 12 0c0 7 3 9 3 9H3s3-2 3-9M10.3 21a1.9 1.9 0 0 0 3.4 0"/>"#;
    pub const CLOSE: &str = r#"<path d="M18 6L6 18M6 6l12 12"/>"#;
    pub const COPY: &str = r#"<rect x="9" y="9" width="12" height="12" rx="2"/><path d="M5 15H4.5A1.5 1.5 0 0 1 3 13.5v-9A1.5 1.5 0 0 1 4.5 3h9A1.5 1.5 0 0 1 15 4.5V5"/>"#;
    pub const CHECK: &str = r#"<path d="M5 12.5l4.5 4.5L19 7.5"/>"#;
    pub const ARROW_RIGHT: &str = r#"<path d="M5 12h14M13 6l6 6-6 6"/>"#;
    pub const CAPS: &str = r#"<path d="M12 4l8 8h-4v5H8v-5H4z"/><path d="M8 21h8"/>"#;
    pub const CLEAR: &str = r#"<path d="M4 7h16M10 11v6M14 11v6M6 7l1 13h10l1-13M9 7V4h6v3"/>"#;
    pub const VOLUME_OFF: &str = r#"<path d="M11 5L6 9H2v6h4l5 4z"/><path d="M22 9l-6 6M16 9l6 6"/>"#;
    pub const VOLUME_LOW: &str = r#"<path d="M11 5L6 9H2v6h4l5 4z"/><path d="M15.5 8.5a5 5 0 0 1 0 7"/>"#;
    pub const VOLUME_HIGH: &str =
        r#"<path d="M11 5L6 9H2v6h4l5 4z"/><path d="M15.5 8.5a5 5 0 0 1 0 7M18.5 5.5a9.5 9.5 0 0 1 0 13"/>"#;
    pub const VOLUME_NONE: &str = r#"<path d="M11 5L6 9H2v6h4l5 4z"/>"#;
    pub const MIC: &str =
        r#"<rect x="9" y="2" width="6" height="12" rx="3"/><path d="M5 10v1a7 7 0 0 0 14 0v-1M12 18v4M8 22h8"/>"#;
    pub const MIC_OFF: &str = r#"<rect x="9" y="2" width="6" height="12" rx="3"/><path d="M5 10v1a7 7 0 0 0 14 0v-1M12 18v4M8 22h8M3 3l18 18"/>"#;
    pub const SUN: &str = r#"<circle cx="12" cy="12" r="4"/><path d="M12 2v2M12 20v2M4.9 4.9l1.4 1.4M17.7 17.7l1.4 1.4M2 12h2M20 12h2M4.9 19.1l1.4-1.4M17.7 6.3l1.4-1.4"/>"#;
    pub const PLAY: &str = r#"<path d="M7 4.5l12 7.5-12 7.5z"/>"#;
    pub const PAUSE: &str = r#"<path d="M8 5v14M16 5v14"/>"#;
    pub const NEXT: &str = r#"<path d="M5 5l10 7-10 7zM19 5v14"/>"#;
    pub const PREVIOUS: &str = r#"<path d="M19 19L9 12l10-7zM5 19V5"/>"#;
    pub const STOP: &str = r#"<rect x="6" y="6" width="12" height="12" rx="1.5"/>"#;
    pub const MUSIC: &str =
        r#"<path d="M9 18V5l12-2v13"/><circle cx="6" cy="18" r="3"/><circle cx="18" cy="16" r="3"/>"#;
    pub const CHEVRON_DOWN: &str = r#"<path d="M6 9l6 6 6-6"/>"#;
    pub const CHEVRON_UP: &str = r#"<path d="M6 15l6-6 6 6"/>"#;
    pub const CHEVRON_RIGHT: &str = r#"<path d="M9 6l6 6-6 6"/>"#;
    pub const CHAT: &str = r#"<path d="M21 12a8 8 0 0 1-11.6 7.1L4 20l1.1-4.4A8 8 0 1 1 21 12z"/><path d="M8.5 12h.01M12 12h.01M15.5 12h.01"/>"#;
    pub const ALERT: &str = r#"<path d="M12 3.5l9.5 16.5h-19z"/><path d="M12 10v4.5M12 17.5v.01"/>"#;
    pub const CALCULATOR: &str = r#"<rect x="5" y="3" width="14" height="18" rx="2"/><path d="M8 7h8M8 12h.01M12 12h.01M16 12h.01M8 16h.01M12 16h.01M16 16h.01"/>"#;
    pub const CLOCK: &str = r#"<circle cx="12" cy="12" r="9"/><path d="M12 7v5l3 2"/>"#;
    pub const CALENDAR: &str = r#"<rect x="3" y="5" width="18" height="16" rx="2"/><path d="M3 10h18M8 3v4M16 3v4"/>"#;
    pub const TERMINAL: &str = r#"<path d="M5 8l4 4-4 4M12 17h7"/><rect x="2" y="3" width="20" height="18" rx="2"/>"#;
    pub const FOLDER: &str = r#"<path d="M3 6.5A1.5 1.5 0 0 1 4.5 5H9l2 2.5h8.5A1.5 1.5 0 0 1 21 9v9.5a1.5 1.5 0 0 1-1.5 1.5h-15A1.5 1.5 0 0 1 3 18.5z"/>"#;
    pub const FILE: &str = r#"<path d="M14 3H6.5A1.5 1.5 0 0 0 5 4.5v15A1.5 1.5 0 0 0 6.5 21h11a1.5 1.5 0 0 0 1.5-1.5V8z"/><path d="M14 3v5h5"/>"#;
    pub const GLOBE: &str =
        r#"<circle cx="12" cy="12" r="9"/><path d="M3 12h18M12 3a14 14 0 0 1 0 18M12 3a14 14 0 0 0 0 18"/>"#;
    pub const OVERVIEW: &str = r#"<rect x="3" y="3" width="7" height="7" rx="1"/><rect x="14" y="3" width="7" height="7" rx="1"/><rect x="3" y="14" width="7" height="7" rx="1"/><rect x="14" y="14" width="7" height="7" rx="1"/>"#;
    /// The minimap: a split layout with the camera's frame on one part.
    pub const MINIMAP: &str = r#"<rect x="3" y="4" width="18" height="16" rx="2"/><path d="M10 4v16M10 12h11"/><rect x="12.5" y="6.5" width="6" height="3" rx="0.5"/>"#;
    pub const CAMERA: &str = r#"<path d="M4 8h3l2-3h6l2 3h3a1 1 0 0 1 1 1v10a1 1 0 0 1-1 1H4a1 1 0 0 1-1-1V9a1 1 0 0 1 1-1z"/><circle cx="12" cy="13.5" r="3.5"/>"#;
    pub const CONTRAST: &str = r#"<circle cx="12" cy="12" r="9"/><path d="M12 3v18M12 7.5h5M12 12h6M12 16.5h5"/>"#;
    pub const MONITOR: &str = r#"<rect x="3" y="4" width="18" height="12" rx="1.5"/><path d="M8 20h8M12 16v4"/>"#;
    pub const WINDOW: &str = r#"<rect x="3" y="4" width="18" height="16" rx="2"/><path d="M3 9h18"/>"#;
    pub const ZOOM_IN: &str = r#"<circle cx="11" cy="11" r="7"/><path d="M20 20l-4-4M8 11h6M11 8v6"/>"#;
    pub const ZOOM_OUT: &str = r#"<circle cx="11" cy="11" r="7"/><path d="M20 20l-4-4M8 11h6"/>"#;
    pub const ARROW_LEFT: &str = r#"<path d="M19 12H5M11 6l-6 6 6 6"/>"#;
    pub const DUCK: &str = r#"<circle cx="10" cy="8" r="3.5"/><path d="M13.5 8.5l3 .5-3 1.2M7 11.5C4.5 11.5 3 13 3 14.8 3 18 6.5 20 11 20s8.5-2.5 9-7c-2 1-4.5 1-6.5-.6"/>"#;

    /// The symbol for a plugin icon named `glyph:<name>`; the plug for an
    /// unknown name, none for a name without the prefix.
    pub fn named(icon: &str) -> Option<&'static str> {
        Some(match icon.strip_prefix("glyph:")? {
            "folder" => FOLDER,
            "file" => FILE,
            "globe" => GLOBE,
            "terminal" => TERMINAL,
            "copy" => COPY,
            "home" => HOME,
            "search" => SEARCH,
            "bulb" => BULB,
            "drop" => DROP,
            "thermometer" => THERMOMETER,
            "snowflake" => SNOWFLAKE,
            "flame" => FLAME,
            "fan" => FAN,
            "wifi_off" => WIFI_OFF,
            "bluetooth" => BLUETOOTH,
            "bell" => BELL,
            "lock" => LOCK,
            "play" => PLAY,
            "pause" => PAUSE,
            "stop" => STOP,
            "next" => NEXT,
            "previous" => PREVIOUS,
            "clock" => CLOCK,
            "plus" => PLUS,
            "minus" => MINUS,
            "check" => CHECK,
            "close" => CLOSE,
            _ => PLUG,
        })
    }
}

/// App and file icons, rasterised at the output's scale and cached by path
/// and size in physical pixels.
pub struct Icons {
    cache: RefCell<HashMap<(PathBuf, u32), Option<MemoryRenderBuffer>>>,
    scale: Cell<f64>,
}

impl Default for Icons {
    fn default() -> Self {
        Icons { cache: Default::default(), scale: Cell::new(1.0) }
    }
}

pub fn compose_state() -> Option<xkb::compose::State> {
    let locale = ["LC_ALL", "LC_CTYPE", "LANG"]
        .iter()
        .find_map(|key| std::env::var_os(key).filter(|value| !value.is_empty()))
        .unwrap_or_else(|| "C.UTF-8".into());
    let context = xkb::Context::new(xkb::CONTEXT_NO_FLAGS);
    let table = xkb::compose::Table::new_from_locale(&context, &locale, xkb::compose::COMPILE_NO_FLAGS).ok()?;
    Some(xkb::compose::State::new(&table, xkb::compose::STATE_NO_FLAGS))
}

pub fn compose_text(compose: &mut Option<xkb::compose::State>, sym: u32) -> Option<Zeroizing<String>> {
    if let Some(state) = compose {
        state.feed(sym.into());
        match state.status() {
            xkb::compose::Status::Composing => return None,
            xkb::compose::Status::Cancelled => {
                state.reset();
                return None;
            }
            xkb::compose::Status::Composed => {
                let text = state.utf8().map(Zeroizing::new);
                state.reset();
                return text;
            }
            xkb::compose::Status::Nothing => {}
        }
    }
    char::from_u32(xkb::keysym_to_utf32(sym.into()))
        .filter(|character| !character.is_control())
        .map(|character| Zeroizing::new(character.to_string()))
}

/// Entries and texture bytes of a cache of square images, by their size in
/// physical pixels; missing images take nothing.
pub fn square_stats<'a>(images: impl Iterator<Item = (u32, Option<&'a MemoryRenderBuffer>)>) -> (usize, usize) {
    images.fold((0, 0), |(count, bytes), (size, image)| {
        (count + 1, bytes + image.map_or(0, |_| size as usize * size as usize * 4))
    })
}

impl Icons {
    /// Entries and texture bytes, for `tileception msg caches`.
    pub fn stats(&self) -> (usize, usize) {
        square_stats(self.cache.borrow().iter().map(|(key, image)| (key.1, image.as_ref())))
    }

    pub fn set_scale(&self, scale: f64) {
        self.scale.set(scale);
    }

    /// Physical pixels for `size` logical ones.
    pub fn pixels(&self, size: u32) -> u32 {
        (f64::from(size) * self.scale.get()).round().max(1.0) as u32
    }

    /// The icon at `path`, `size` logical pixels square.
    pub fn image(&self, path: &Path, size: u32) -> Option<MemoryRenderBuffer> {
        let size = self.pixels(size);
        let key = (path.to_path_buf(), size);
        if let Some(image) = self.cache.borrow().get(&key) {
            return image.clone();
        }
        let image = load_icon(path, size);
        let mut cache = self.cache.borrow_mut();
        if cache.len() >= 512 {
            cache.clear();
        }
        cache.insert(key, image.clone());
        image
    }

    /// An SVG built into the program, `size` logical pixels square; `name`
    /// keys it in the cache.
    pub fn embedded(&self, name: &str, svg: &[u8], size: u32) -> Option<MemoryRenderBuffer> {
        let size = self.pixels(size);
        let key = (PathBuf::from(format!("embedded:{name}")), size);
        if let Some(image) = self.cache.borrow().get(&key) {
            return image.clone();
        }
        let image = render_svg(svg, None, size).map(|pixels| square_buffer(&pixels, size));
        let mut cache = self.cache.borrow_mut();
        if cache.len() >= 512 {
            cache.clear();
        }
        cache.insert(key, image.clone());
        image
    }
}

/// Line icons rasterised once per size and colour, at the output's scale.
pub struct Glyphs {
    cache: RefCell<HashMap<GlyphKey, Option<MemoryRenderBuffer>>>,
    scale: Cell<f64>,
}

impl Default for Glyphs {
    fn default() -> Self {
        Glyphs { cache: Default::default(), scale: Cell::new(1.0) }
    }
}

/// A glyph's path data, size in physical pixels and straight RGBA colour.
type GlyphKey = (&'static str, u32, [u8; 4]);

impl Glyphs {
    /// Entries and texture bytes, for `tileception msg caches`.
    pub fn stats(&self) -> (usize, usize) {
        square_stats(self.cache.borrow().iter().map(|(key, image)| (key.1, image.as_ref())))
    }

    pub fn set_scale(&self, scale: f64) {
        self.scale.set(scale);
    }

    /// The glyph `size` logical pixels square.
    pub fn image(&self, glyph: &'static str, size: u32, colour: Colour) -> Option<MemoryRenderBuffer> {
        let size = (f64::from(size) * self.scale.get()).round().max(1.0) as u32;
        let key = (glyph, size, colour.straight_rgba8());
        if let Some(image) = self.cache.borrow().get(&key) {
            return image.clone();
        }
        let image = render_glyph(glyph, size, colour);
        let mut cache = self.cache.borrow_mut();
        if cache.len() >= 256 {
            cache.clear();
        }
        cache.insert(key, image.clone());
        image
    }
}

fn render_glyph(glyph: &str, size: u32, colour: Colour) -> Option<MemoryRenderBuffer> {
    let [red, green, blue, alpha] = colour.straight_rgba8();
    let svg = format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="rgb({red},{green},{blue})" stroke-opacity="{}" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">{glyph}</svg>"#,
        alpha as f32 / 255.0,
    );
    let tree = resvg::usvg::Tree::from_str(&svg, &resvg::usvg::Options::default()).ok()?;
    let mut pixmap = resvg::tiny_skia::Pixmap::new(size, size)?;
    let scale = size as f32 / 24.0;
    resvg::render(&tree, resvg::tiny_skia::Transform::from_scale(scale, scale), &mut pixmap.as_mut());
    // tiny-skia pixels are premultiplied RGBA, which is what the renderer wants.
    Some(MemoryRenderBuffer::from_slice(
        pixmap.data(),
        Fourcc::Abgr8888,
        (size as i32, size as i32),
        1,
        Transform::Normal,
        None,
    ))
}

/// Eases a 0 to 1 progress so movement settles softly.
pub fn ease_out(progress: f64) -> f64 {
    let progress = progress.clamp(0.0, 1.0);
    1.0 - (1.0 - progress).powi(3)
}

/// The wall clock in the local time zone.
pub struct LocalTime {
    pub year: i32,
    pub hour: i32,
    pub minute: i32,
    pub second: i32,
    pub weekday: usize,
    pub day: i32,
    pub month: usize,
}

impl LocalTime {
    #[allow(unsafe_code, reason = "the local time zone is applied through libc's reentrant localtime_r")]
    pub fn now() -> Option<LocalTime> {
        // SAFETY: time accepts a null pointer and only returns the current time.
        let now = unsafe { libc::time(std::ptr::null_mut()) };
        Self::at(now)
    }

    /// The local time at a Unix time in seconds.
    #[allow(unsafe_code, reason = "the local time zone is applied through libc's reentrant localtime_r")]
    pub fn at(now: libc::time_t) -> Option<LocalTime> {
        let mut calendar = std::mem::MaybeUninit::<libc::tm>::uninit();
        // SAFETY: localtime_r reads `now` and fills the caller-owned `calendar`, which lives past the call.
        let filled = unsafe { libc::localtime_r(&now, calendar.as_mut_ptr()) };
        if filled.is_null() {
            return None;
        }
        // SAFETY: a non-null result means localtime_r initialised `calendar`.
        let calendar = unsafe { calendar.assume_init() };
        Some(LocalTime {
            year: calendar.tm_year + 1900,
            hour: calendar.tm_hour,
            minute: calendar.tm_min,
            // 60 is a leap second.
            second: calendar.tm_sec.clamp(0, 59),
            weekday: calendar.tm_wday.clamp(0, 6) as usize,
            day: calendar.tm_mday,
            month: calendar.tm_mon.clamp(0, 11) as usize,
        })
    }

    pub fn clock(&self) -> String {
        format!("{:02}:{:02}", self.hour, self.minute)
    }

    /// The clock with seconds: "20:42:07".
    pub fn clock_seconds(&self) -> String {
        format!("{:02}:{:02}:{:02}", self.hour, self.minute, self.second)
    }

    /// "Thursday 15 October"
    pub fn date(&self) -> String {
        const DAYS: [&str; 7] = ["Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday"];
        const MONTHS: [&str; 12] = [
            "January",
            "February",
            "March",
            "April",
            "May",
            "June",
            "July",
            "August",
            "September",
            "October",
            "November",
            "December",
        ];
        format!("{} {} {}", DAYS[self.weekday], self.day, MONTHS[self.month])
    }
}

/// "now", "4 min", "2 h", "3 d": how long ago something happened.
pub fn ago(when: std::time::SystemTime) -> String {
    let secs = when.elapsed().map(|duration| duration.as_secs()).unwrap_or(0);
    match secs {
        0..=59 => "now".into(),
        60..=3599 => format!("{} min", secs / 60),
        3600..=86_399 => format!("{} h", secs / 3600),
        _ => format!("{} d", secs / 86_400),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Align {
    Left,
    Centre,
}

/// How a piece of interface text looks.
#[derive(Debug, Clone, Copy)]
pub struct Style {
    pub size: f32,
    pub colour: Colour,
    pub weight: u16,
    /// 0 for one line, otherwise wrapped to at most this many lines.
    pub lines: u8,
    pub align: Align,
}

impl Style {
    pub fn new(size: f32, colour: Colour) -> Style {
        Style { size, colour, weight: 400, lines: 0, align: Align::Left }
    }
    pub fn weight(self, weight: u16) -> Style {
        Style { weight, ..self }
    }
    pub fn colour(self, colour: Colour) -> Style {
        Style { colour, ..self }
    }
    pub fn bold(self) -> Style {
        self.weight(600)
    }
    pub fn lines(self, lines: u8) -> Style {
        Style { lines, ..self }
    }
    pub fn align(self, align: Align) -> Style {
        Style { align, ..self }
    }
}

/// How much a button asks for attention.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Look {
    /// The one that carries the accent: what the surface is for.
    Primary,
    /// An outlined button beside it.
    Plain,
    /// [`Look::Plain`], for something to be careful with.
    Warning,
    /// Bare until the pointer is over it, for buttons inside a row or card.
    Quiet,
}

/// A button: what it shows and the state it is in. [`Painter::button`] turns
/// that into its shape, colours and text, so every button looks alike and a
/// change of style is made in one place.
#[derive(Debug, Clone, Copy)]
pub struct Button<'a> {
    label: &'a str,
    glyph: Option<&'static str>,
    look: Look,
    enabled: bool,
    focused: bool,
    round: bool,
}

impl<'a> Button<'a> {
    pub fn new(label: &'a str, look: Look) -> Button<'a> {
        Button { label, glyph: None, look, enabled: true, focused: false, round: false }
    }
    pub fn primary(label: &'a str) -> Button<'a> {
        Button::new(label, Look::Primary)
    }
    pub fn plain(label: &'a str) -> Button<'a> {
        Button::new(label, Look::Plain)
    }
    pub fn quiet(label: &'a str) -> Button<'a> {
        Button::new(label, Look::Quiet)
    }
    /// A button that shows a line icon instead of a label.
    pub fn glyph(glyph: &'static str, look: Look) -> Button<'static> {
        Button { glyph: Some(glyph), ..Button::new("", look) }
    }
    /// One that cannot be pressed now is drawn faint and ignores the pointer.
    pub fn enabled(self, enabled: bool) -> Button<'a> {
        Button { enabled, ..self }
    }
    /// Whether the keyboard is on it, which draws a ring.
    pub fn focused(self, focused: bool) -> Button<'a> {
        Button { focused, ..self }
    }
    /// Fully rounded ends, for icon buttons.
    pub fn round(self) -> Button<'a> {
        Button { round: true, ..self }
    }
}

/// The shape of a button that carries the accent and the colour of what is
/// written on it, in the theme's [`ButtonStyle`]:
///
/// - halo: a [`Shape::halo`] on the popup surface, with more of the accent in
///   it and a wider glow under the pointer;
/// - gradient: filled from the accent to its end colour;
/// - flat: filled with the accent alone, under a line of light.
///
/// The filled ones turn lighter and glow under the pointer.
fn accent_button(theme: &crate::theme::Theme, radius: f32, hover: bool) -> (Shape, Colour) {
    let lit = |colour: Colour| if hover { colour.mix(Colour([1.0; 4]), 0.14) } else { colour };
    let filled = |fill: (Colour, Colour), highlight: f32| Shape {
        fill,
        highlight,
        glow_radius: if hover { theme.glow * 0.75 } else { 0.0 },
        glow: theme.accent.alpha(0.33),
        radius,
        ..Default::default()
    };
    match theme.buttons {
        ButtonStyle::Halo => {
            let halo = Shape::halo(theme, theme.popup(), radius);
            let shape = if hover {
                let fill = theme.popup().mix(theme.accent, 0.34);
                Shape { fill: (fill, fill), highlight: 0.14, glow: theme.accent.alpha(0.42), ..halo }
            } else {
                Shape { glow_radius: theme.glow * 0.78, glow: theme.accent.alpha(0.26), ..halo }
            };
            (shape, theme.text)
        }
        ButtonStyle::Gradient => (filled((lit(theme.accent), lit(theme.accent_end)), 0.0), theme.on_accent),
        ButtonStyle::Flat => (filled((lit(theme.accent), lit(theme.accent)), 0.28), theme.on_accent),
    }
}

/// Draws the compositor's own interface back to front (in painting order),
/// optionally faded and shifted as a whole for animations.
pub struct Painter<'a> {
    state: &'a Tileception,
    renderer: &'a mut GlesRenderer,
    shapes: Option<Shapes>,
    layers: Vec<OutputElement>,
    pub alpha: f32,
    pub offset: (f64, f64),
}

impl<'a> Painter<'a> {
    pub fn new(state: &'a Tileception, renderer: &'a mut GlesRenderer) -> Painter<'a> {
        let shapes = state.shapes(renderer);
        Painter { state, renderer, shapes, layers: Vec::new(), alpha: 1.0, offset: (0.0, 0.0) }
    }

    pub fn theme(&self) -> &'a crate::theme::Theme {
        &self.state.theme
    }

    /// The elements, topmost first as the renderer wants them.
    pub fn finish(mut self) -> Vec<OutputElement> {
        self.layers.reverse();
        self.layers
    }

    /// Clip a scrolling block, including its text and icons, to a viewport.
    pub fn clipped(&mut self, rect: Rect, draw: impl FnOnce(&mut Painter<'_>)) {
        let start = self.layers.len();
        draw(self);
        let elements = self.layers.split_off(start);
        let clipped = crate::render::clip_elements(elements, &self.shifted(rect), self.state.scale);
        self.layers.extend(clipped);
    }

    fn shifted(&self, rect: Rect) -> Rect {
        Rect::new(rect.x + self.offset.0, rect.y + self.offset.1, rect.w, rect.h)
    }

    pub fn shape(&mut self, rect: Rect, shape: Shape) {
        let rect = self.shifted(rect);
        if self.alpha <= 0.0 {
            return;
        }
        if let Some(element) = self.shapes.as_ref().and_then(|shapes| shapes.element_faded(&rect, shape, self.alpha)) {
            self.layers.push(OutputElement::Shape(element));
        }
    }

    pub fn fill(&mut self, rect: Rect, colour: Colour, radius: f32) {
        self.shape(rect, Shape::filled(colour, radius));
    }

    /// A panel background: with frost on, translucent over a blur of what
    /// is behind it.
    pub fn frosted(&mut self, rect: Rect, shape: Shape) {
        self.frosted_over(rect, shape, false);
    }

    /// [`Painter::frosted`]; `over_panels` also blurs the frosted panels
    /// it covers, for popups opened over them.
    fn frosted_over(&mut self, rect: Rect, shape: Shape, over_panels: bool) {
        let theme = self.theme();
        let shape = Shape { fill: (theme.panel(shape.fill.0), theme.panel(shape.fill.1)), ..shape };
        let before = self.layers.len();
        self.shape(rect, shape);
        if theme.frost
            && self.layers.len() > before
            && let Some(element) = self.layers.last()
        {
            use smithay::backend::renderer::element::Element;
            self.state.frost.borrow_mut().region(crate::frost::Region {
                panel: element.id().clone(),
                rect: self.shifted(rect),
                radius: shape.radius,
                alpha: self.alpha,
                over_panels,
            });
        }
    }

    /// An image rasterised at the output's scale, at its own size; or a
    /// picture of `size` pixels drawn `size` logical pixels large, so it is
    /// enlarged with the scale.
    pub fn image_sized(&mut self, buffer: &MemoryRenderBuffer, position: (f64, f64), size: Option<(i32, i32)>) {
        if self.alpha <= 0.0 {
            return;
        }
        let scale = self.state.scale;
        let position = (((position.0 + self.offset.0) * scale).round(), ((position.1 + self.offset.1) * scale).round());
        let alpha = (self.alpha < 1.0).then_some(self.alpha);
        let (src, size) = match size {
            Some((width, height)) => {
                let (width, height) = (width.max(1), height.max(1));
                let shown = ((f64::from(width) * scale).round() as i32, (f64::from(height) * scale).round() as i32);
                (
                    Some(smithay::utils::Rectangle::from_size((f64::from(width), f64::from(height)).into())),
                    Some(shown.into()),
                )
            }
            None => (None, None),
        };
        if let Ok(element) = smithay::backend::renderer::element::memory::MemoryRenderBufferRenderElement::from_buffer(
            self.renderer,
            position,
            buffer,
            alpha,
            src,
            size,
            smithay::backend::renderer::element::Kind::Unspecified,
        ) {
            self.layers.push(OutputElement::Memory(element));
        }
    }

    /// A window's picture fitted into `rect`.
    pub fn window(&mut self, window_id: tileception_core::WindowId, rect: Rect) {
        if self.alpha <= 0.0 {
            return;
        }
        let rect = self.shifted(rect);
        let elements = self.state.picture_faded(self.renderer, window_id, &rect, self.alpha);
        // Pictures come topmost first; layers are kept in painting order.
        self.layers.extend(elements.into_iter().rev());
    }

    pub fn image(&mut self, buffer: &MemoryRenderBuffer, position: (f64, f64)) {
        self.image_sized(buffer, position, None);
    }

    /// Text inside `rect`, centred vertically and aligned horizontally.
    /// Returns its size, also when nothing could be drawn.
    pub fn text(&mut self, label: &str, rect: Rect, style: Style) -> (f64, f64) {
        self.text_font(label, rect, style, &self.state.theme.font)
    }

    pub fn text_font(&mut self, label: &str, rect: Rect, style: Style, font: &str) -> (f64, f64) {
        let image = self.state.text.borrow_mut().styled(
            label,
            font,
            style.size,
            style.colour,
            rect.w.max(1.0) as f32,
            style.weight,
            style.lines,
        );
        let Some(image) = image else { return (0.0, 0.0) };
        let (width, height) = (image.width, image.height);
        let left = match style.align {
            Align::Left => rect.x,
            Align::Centre => rect.x + (rect.w - width) / 2.0,
        };
        self.image(&image.buffer, (left, rect.y + (rect.h - height) / 2.0));
        (width, height)
    }

    /// The size text would take, without drawing it.
    pub fn measure(&self, label: &str, max_width: f64, style: Style) -> (f64, f64) {
        self.measure_font(label, max_width, style, &self.state.theme.font)
    }

    pub fn measure_font(&self, label: &str, max_width: f64, style: Style, font: &str) -> (f64, f64) {
        self.state
            .text
            .borrow_mut()
            .styled(label, font, style.size, style.colour, max_width.max(1.0) as f32, style.weight, style.lines)
            .map(|rendered| (rendered.width, rendered.height))
            .unwrap_or((0.0, 0.0))
    }

    /// A line icon centred in `rect`.
    pub fn glyph(&mut self, glyph: &'static str, rect: Rect, size: u32, colour: Colour) {
        if let Some(image) = self.state.glyphs.image(glyph, size, colour) {
            let position = (rect.x + (rect.w - size as f64) / 2.0, rect.y + (rect.h - size as f64) / 2.0);
            self.image(&image, position);
        }
    }

    /// A button filling `rect`; `hover` is whether the pointer is over it.
    /// Every button is drawn here: see the Buttons section of AGENTS.md.
    pub fn button(&mut self, rect: Rect, button: Button<'_>, hover: bool) {
        let theme = self.theme();
        let Button { label, glyph, look, enabled, focused, round } = button;
        let hover = hover && enabled;
        let radius = if round { rect.h as f32 / 2.0 } else { theme.radius };
        let outlined = |fill: Colour| Shape { border_width: 1.0, border: theme.border, ..Shape::filled(fill, radius) };
        let (shape, colour) = match (look, enabled) {
            (Look::Quiet, false) => (None, theme.muted.alpha(0.5)),
            (_, false) => (Some(outlined(Colour::TRANSPARENT)), theme.muted),
            (Look::Primary, true) => {
                let (shape, colour) = accent_button(theme, radius, hover);
                (Some(shape), colour)
            }
            (Look::Quiet, true) => (hover.then(|| Shape::filled(theme.surface_raised, radius)), theme.text),
            (Look::Plain | Look::Warning, true) => (
                Some(outlined(if hover { theme.border } else { theme.surface_raised })),
                if look == Look::Warning { theme.warning } else { theme.text },
            ),
        };
        // Beside the accent of a primary button, whether its border or its
        // fill, the focus ring is in the text colour.
        let ring = if look == Look::Primary && enabled { theme.text } else { theme.accent };
        let shape = match (shape, focused) {
            (shape, true) => Some(Shape {
                border_width: 2.0,
                border: ring,
                border_end: None,
                ..shape.unwrap_or_else(|| Shape::filled(Colour::TRANSPARENT, radius))
            }),
            (shape, false) => shape,
        };
        if let Some(shape) = shape {
            self.shape(rect, shape);
        }
        let primary = look == Look::Primary;
        match glyph {
            Some(glyph) => self.glyph(glyph, rect, if primary { 14 } else { 15 }, colour),
            None => {
                let style = Style::new(theme.font_size * 0.88, colour).weight(if primary { 600 } else { 500 });
                self.text(label, rect, style.align(Align::Centre));
            }
        }
    }

    fn keycap_style(&self) -> Style {
        let theme = self.theme();
        Style::new(theme.font_size * 0.8, theme.text).bold()
    }

    pub fn keycap_width(&self, label: &str) -> f64 {
        (self.measure(label, 200.0, self.keycap_style()).0 + 12.0).max(20.0)
    }

    /// A key on the keyboard, drawn as a small cap with its label. Returns its width.
    pub fn keycap(&mut self, label: &str, left: f64, centre_y: f64) -> f64 {
        let theme = self.theme();
        let style = self.keycap_style();
        let cap = Rect::new(left, centre_y - 10.0, self.keycap_width(label), 20.0);
        self.shape(Rect::new(cap.x, cap.y + 1.0, cap.w, cap.h), Shape::filled(Colour([0.0, 0.0, 0.0, 0.35]), 4.0));
        self.shape(cap, Shape { border_width: 1.0, border: theme.border, ..Shape::filled(theme.surface_raised, 4.0) });
        self.text(label, cap, style.align(Align::Centre));
        cap.w
    }

    /// Keycaps followed by what they do, in a row; returns the row's width.
    pub fn key_hints(&mut self, hints: &[(&str, &str)], left: f64, centre_y: f64, measure_only: bool) -> f64 {
        let theme = self.theme();
        let style = Style::new(theme.font_size * 0.85, theme.muted);
        let mut cursor = left;
        for (index, (key, what)) in hints.iter().enumerate() {
            if index > 0 {
                cursor += 16.0;
            }
            let cap_w = if measure_only { self.keycap_width(key) } else { self.keycap(key, cursor, centre_y) };
            cursor += cap_w + 6.0;
            let (text_width, _) = if measure_only {
                self.measure(what, 300.0, style)
            } else {
                self.text(what, Rect::new(cursor, centre_y - 12.0, 300.0, 24.0), style)
            };
            cursor += text_width;
        }
        cursor - left
    }

    /// A popup surface: rounded, with a hairline border and a soft shadow.
    pub fn popup(&mut self, rect: Rect) {
        let theme = self.theme();
        // Frosted, the shadow cannot be offset: it would show through.
        let drop = if theme.frost { 0.0 } else { 6.0 };
        self.shape(
            Rect::new(rect.x, rect.y + drop, rect.w, rect.h),
            Shape {
                radius: theme.popup_radius,
                glow_radius: 28.0,
                glow: Colour([0.0, 0.0, 0.0, 0.42]),
                ..Default::default()
            },
        );
        self.frosted_over(
            rect,
            Shape { border_width: 1.0, border: theme.border, ..Shape::filled(theme.popup(), theme.popup_radius) },
            true,
        );
    }

    /// A panel against a screen edge, full height: square, with a hairline
    /// and a soft shadow only on the side that faces the desktop.
    pub fn edge_panel(&mut self, rect: Rect, left: bool) {
        let theme = self.theme();
        self.shape(rect, Shape { glow_radius: 28.0, glow: Colour([0.0, 0.0, 0.0, 0.42]), ..Default::default() });
        self.frosted(rect, Shape::filled(theme.popup(), 0.0));
        let edge = if left { rect.right() - 1.0 } else { rect.x };
        self.fill(Rect::new(edge, rect.y, 1.0, rect.h), theme.border, 0.0);
    }
}

/// An SVG fitted and centred in a square of `size` pixels, premultiplied RGBA.
fn render_svg(data: &[u8], resources_dir: Option<PathBuf>, size: u32) -> Option<Vec<u8>> {
    let options = resvg::usvg::Options { resources_dir, ..Default::default() };
    let tree = resvg::usvg::Tree::from_data(data, &options).ok()?;
    let mut pixels = resvg::tiny_skia::Pixmap::new(size, size)?;
    let scale = (size as f32 / tree.size().width()).min(size as f32 / tree.size().height());
    let transform = resvg::tiny_skia::Transform::from_translate(
        (size as f32 - tree.size().width() * scale) / 2.0,
        (size as f32 - tree.size().height() * scale) / 2.0,
    )
    .pre_scale(scale, scale);
    resvg::render(&tree, transform, &mut pixels.as_mut());
    Some(pixels.take())
}

fn square_buffer(pixels: &[u8], size: u32) -> MemoryRenderBuffer {
    MemoryRenderBuffer::from_slice(pixels, Fourcc::Abgr8888, (size as i32, size as i32), 1, Transform::Normal, None)
}

fn load_icon(path: &Path, size: u32) -> Option<MemoryRenderBuffer> {
    let pixels = if path.extension().is_some_and(|extension| extension == "svg") {
        render_svg(&std::fs::read(path).ok()?, path.parent().map(Path::to_path_buf), size)?
    } else {
        let reader = image::ImageReader::open(path).ok()?.with_guessed_format().ok()?;
        let image = reader.decode().ok()?.resize(size, size, image::imageops::FilterType::Triangle).to_rgba8();
        let mut canvas = image::RgbaImage::new(size, size);
        image::imageops::overlay(
            &mut canvas,
            &image,
            ((size - image.width()) / 2) as i64,
            ((size - image.height()) / 2) as i64,
        );
        for pixel in canvas.pixels_mut() {
            let alpha = pixel.0[3] as u32;
            for channel in &mut pixel.0[..3] {
                *channel = (*channel as u32 * alpha / 255) as u8;
            }
        }
        canvas.into_raw()
    };
    Some(square_buffer(&pixels, size))
}

impl Tileception {
    pub fn ui_label(
        &self,
        out: &mut Vec<OutputElement>,
        renderer: &mut GlesRenderer,
        label: &str,
        rect: Rect,
        colour: Colour,
        bold: bool,
    ) {
        if let Some(image) = self.text.borrow_mut().line(
            label,
            &self.theme.font,
            self.theme.font_size,
            colour,
            rect.w.max(1.0) as f32,
            bold,
        ) {
            push_image(out, renderer, &image.buffer, (rect.x, rect.y + (rect.h - image.height) / 2.0), self.scale);
        }
    }
}

impl crate::Tileception {
    /// `tileception msg caches`: how many images each cache holds and how
    /// much texture memory they take once drawn.
    pub fn cache_status(&self) -> String {
        let (text, rich) = self.text.borrow().stats();
        let art = square_stats(self.media.art.borrow().iter().map(|(key, image)| (key.1, image.as_ref())));
        let rows = [
            ("text", text),
            ("rich text", rich),
            ("app icons", self.launcher.icons.stats()),
            ("line icons", self.glyphs.stats()),
            ("cover art", art),
        ];
        let mib = |bytes: usize| bytes as f64 / (1024.0 * 1024.0);
        let mut out = String::new();
        for (name, (count, bytes)) in rows {
            out.push_str(&format!("{name:<11} {count:>4} images {:>8.1} MiB\n", mib(bytes)));
        }
        let total: usize = rows.iter().map(|(_, (_, bytes))| bytes).sum();
        out.push_str(&format!("{:<11} {:>4}        {:>8.1} MiB", "total", "", mib(total)));
        out
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeMap, fs, path::Path};

    /// The accent gradient written out by hand is how a button ends up
    /// looking unlike the others. These are the marks that keep it; anything
    /// new takes `Painter::button`, or a named fill of `Theme`.
    #[test]
    fn the_accent_gradient_is_not_drawn_by_hand() {
        fn count(folder: &Path, needle: &str, found: &mut BTreeMap<String, usize>) {
            for entry in fs::read_dir(folder).expect("the source folder can be read").flatten() {
                let path = entry.path();
                if path.is_dir() {
                    count(&path, needle, found);
                } else if path.extension().is_some_and(|extension| extension == "rs") {
                    let uses = fs::read_to_string(&path).expect("a source file is text").matches(needle).count();
                    if uses > 0 {
                        found.insert(path.display().to_string(), uses);
                    }
                }
            }
        }
        let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let needle = ["(theme.accent, ", "theme.accent_end)"].concat();
        let mut found = BTreeMap::new();
        count(&source, &needle, &mut found);
        let allowed = [
            ("bar.rs", 1),
            ("greeter/mod.rs", 2),
            ("launcher.rs", 2),
            ("panel/mod.rs", 1),
            ("updates/popover.rs", 1),
            ("updates/window.rs", 2),
        ];
        let allowed: BTreeMap<String, usize> =
            allowed.into_iter().map(|(file, uses)| (source.join(file).display().to_string(), uses)).collect();
        assert_eq!(found, allowed);
    }
}
