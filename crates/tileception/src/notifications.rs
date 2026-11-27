//! Notification popups and history. The service runs off the compositor thread.

use std::{
    cell::RefCell,
    collections::{BTreeMap, HashMap, HashSet},
    sync::{Arc, mpsc},
    time::Instant,
};

use smithay::{
    backend::{
        allocator::Fourcc,
        renderer::{element::memory::MemoryRenderBuffer, gles::GlesRenderer},
    },
    output::Output,
    reexports::calloop::ping::make_ping,
    utils::Transform,
};
use tileception_core::{Point, Rect};
use tileception_notify::{Event, Image, Notification, NotifyService, Request, Urgency};

use crate::{
    Tileception,
    lock::{LockState, Shown},
    render::OutputElement,
    shapes::Shape,
    ui::{Align, Button, Look, Painter, Style, ago, ease_out, glyph},
};

#[derive(Default)]
pub struct Notifications {
    pub(crate) service: Option<NotifyService>,
    events: Option<mpsc::Receiver<Event>>,
    source: Option<smithay::reexports::calloop::RegistrationToken>,
    active: BTreeMap<u32, Arc<Notification>>,
    history: Vec<Arc<Notification>>,
    pub open: bool,
    /// Whether the open panel has the keyboard: from opening it or a click
    /// in it until a click elsewhere.
    pub focused: bool,
    /// What Tab chose in the panel: a button above the history or a
    /// notification.
    selected: Option<Chosen>,
    /// The screen the panel or the popups showing appeared on; the active
    /// one when unset.
    pub home: Option<Output>,
    offset: usize,
    thumbnails: RefCell<HashMap<u32, Option<Preview>>>,
    /// Shown by [`Tileception::notify_locally`] so far, for their ids.
    local: u32,
    /// What the buttons of Tileception's own notifications do: a command,
    /// or only closing it.
    local_actions: HashMap<u32, Vec<Option<crate::commands::Command>>>,
    /// Ids of [`Tileception::ask_locally`]'s questions still waiting for an answer.
    questions: HashSet<u32>,
    /// The popup whose buttons have the keyboard (Super+Enter), and the
    /// highlighted button. Only ever on request: popups never take it.
    pub answering: Option<(u32, usize)>,
    /// What came in since Tileception's own lock screen showed, oldest first.
    while_locked: Vec<Arc<Notification>>,
    /// Sound, Silent or Do not disturb, and until when.
    pub(crate) mode: crate::notification_mode::ModeState,
    /// Checks for the mode's end.
    pub(crate) mode_timer: Option<smithay::reexports::calloop::RegistrationToken>,
    pub(crate) sounds: crate::sound::Sounds,
    /// The menu under a mode's arrow, while open.
    pub(crate) mode_menu: Option<crate::notification_switch::ModeMenu>,
    /// The menu of a card's bell, while open.
    pub(crate) sound_menu: Option<crate::notification_switch::SoundMenu>,
    /// Whether a press in the mode menu's time field is selecting.
    pub(crate) field_drag: bool,
}

/// What Tab and Shift+Tab move between in the panel: the buttons above the
/// history first, top to bottom, then the notifications.
#[derive(Debug, Clone, PartialEq)]
enum Chosen {
    Button(crate::panel::PanelButton),
    /// A notification in the history, by id.
    Notification(u32),
}

/// The most notifications kept for the lock screen.
const WHILE_LOCKED: usize = 100;

/// The category of [`Tileception::notify_locally`]'s notifications, which a
/// lock screen always shows; everyone else's follow `[lock]`.
const LOCAL_CATEGORY: &str = "x-tileception.local";

/// The category of questions waiting for an answer, which stay off a lock screen.
const PROMPT_CATEGORY: &str = "x-tileception.prompt";

/// The category of the built-in alarms (Timers), which ring until dismissed.
pub const ALARM_CATEGORY: &str = "x-tileception.alarm";

/// How many notifications Page Up and Page Down scroll the panel by.
const PAGE: isize = 5;

/// How long a notification of Tileception's own stays up.
const LOCAL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

#[derive(Clone, Copy, PartialEq)]
enum NotificationKind {
    Local,
    Question,
    Service,
}

#[derive(Clone, PartialEq)]
enum PointerAction {
    Clear,
    Close {
        identifier: u32,
        kind: NotificationKind,
        history: bool,
    },
    Invoke {
        identifier: u32,
        kind: NotificationKind,
        key: String,
    },
    /// "Show on lock screen" for the app's desktop entry and name.
    LockScreen {
        desktop_entry: Option<String>,
        app_name: String,
    },
}

impl crate::ui::PointerAction for PointerAction {
    fn activate(&self, state: &mut Tileception, _rect: Rect, _point: Point) {
        match self {
            Self::Clear => state.clear_notifications(),
            Self::LockScreen { desktop_entry, app_name } => {
                state.toggle_lock_screen_app(desktop_entry.as_deref(), app_name)
            }
            Self::Close { identifier, kind: NotificationKind::Local, .. } => {
                state.close_local_notification(*identifier)
            }
            Self::Invoke { identifier, kind: NotificationKind::Local, key } => state.local_action(*identifier, key),
            Self::Close { identifier, kind: NotificationKind::Question, .. } => {
                state.withdraw_question(*identifier);
                state.answer_typing(*identifier, None);
                state.answer_shortcut(*identifier);
            }
            Self::Invoke { identifier, kind: NotificationKind::Question, key } => {
                state.withdraw_question(*identifier);
                state.answer_typing(*identifier, Some(key));
                state.answer_shortcut(*identifier);
            }
            Self::Close { identifier, history, .. } => {
                if *history {
                    state.notification_request(Request::RemoveFromHistory(*identifier));
                }
                state.notification_request(Request::Dismiss(*identifier));
            }
            Self::Invoke { identifier, key, .. } => {
                state.notification_request(Request::Invoke { id: *identifier, action: key.clone(), token: None })
            }
        }
    }
}

impl Tileception {
    pub fn start_notifications(&mut self, address: Option<String>) {
        let result = (|| {
            let (ping, source) = make_ping().map_err(|err| err.to_string())?;
            let (service, events) = NotifyService::start(Default::default(), address, move || ping.ping())
                .map_err(|err| err.to_string())?;
            let token = self
                .loop_handle
                .insert_source(source, |_, _, state| state.notification_events())
                .map_err(|err| err.to_string())?;
            Ok::<_, String>((service, events, token))
        })();
        match result {
            Ok((service, events, token)) => {
                self.notifications.service = Some(service);
                self.notifications.events = Some(events);
                self.notifications.source = Some(token);
                self.start_notification_mode();
            }
            Err(error) => tracing::error!("could not start notifications: {error}"),
        }
    }

    /// Stops the service, releasing its bus name for another daemon.
    pub fn stop_notifications(&mut self) {
        if let Some(token) = self.notifications.source.take() {
            self.loop_handle.remove(token);
        }
        self.notifications.service = None;
        self.notifications.events = None;
        self.stop_notification_mode();
        self.notifications.sounds.stop_all();
        self.close_switch_menus();
        let questions = &self.notifications.questions;
        self.notifications.active.retain(|notification_id, _| questions.contains(notification_id));
        self.notifications.history.clear();
        self.schedule_render();
    }

    /// Whether Tileception's own notifications are in use (rather than
    /// switched off or replaced in `[shell]`).
    pub fn builtin_notifications(&self) -> bool {
        self.shell().notifications().is_builtin()
    }

    fn notification_events(&mut self) {
        let events: Vec<_> =
            self.notifications.events.as_ref().map(|receiver| receiver.try_iter().collect()).unwrap_or_default();
        for event in events {
            match event {
                Event::Shown(notification) | Event::Replaced(notification) => {
                    if !self.notifications.active.contains_key(&notification.id) {
                        self.notification_sound(&notification);
                    }
                    self.notifications_appear();
                    self.notifications.thumbnails.borrow_mut().remove(&notification.id);
                    let notification = Arc::new(notification);
                    self.came_in_while_locked(&notification);
                    self.notifications.active.insert(notification.id, notification);
                }
                Event::Closed { id: notification_id, .. } => {
                    self.notifications.active.remove(&notification_id);
                    self.notifications.sounds.stop(notification_id);
                }
            }
        }
        // The dashboard shows how many there are, also with the panel shut.
        self.refresh_notification_history();
        self.check_answering();
        self.schedule_render();
    }

    pub fn toggle_notifications(&mut self) {
        if self.is_locked() {
            return;
        }
        // Open on another screen, it comes over to this one.
        self.notifications.open = !self.notifications.open || !self.notifications_here();
        self.notifications.focused = self.notifications.open;
        self.notifications.home = self.outputs.active.clone();
        self.notifications.offset = 0;
        self.notifications.selected = None;
        if !self.notifications.open {
            self.close_switch_menus();
        }
        if self.notifications.open {
            self.doctor.focused = false;
            self.refresh_notification_history();
        }
        self.force_keyboard_focus_sync();
    }

    pub fn close_notifications(&mut self) {
        self.close_switch_menus();
        let focused = self.notifications.focused;
        self.notifications.open = false;
        self.notifications.focused = false;
        if focused {
            self.force_keyboard_focus_sync();
        }
    }

    /// Whether keys go to the open panel.
    pub fn notifications_keyboard(&self) -> bool {
        self.notifications.open && self.notifications.focused
    }

    /// A left press anywhere while the panel is open, before anything takes
    /// it: inside, the panel takes the keyboard; outside, the desktop gets it
    /// back. The press itself is handled as usual. A modal dialog, the power
    /// menu or the tray's menu keeps the keyboard where it is.
    pub fn notifications_press(&mut self, point: Point) {
        if !self.notifications.open
            || self.is_locked()
            || self.feedback.is_some()
            || self.password_prompt.is_some()
            || self.power_menu.is_some()
            || self.colour_pick.is_some()
            || self.shot.is_some()
            || self.tray.open
            || self.bar_card_at(point)
        {
            return;
        }
        let area = self.mode_switch_area();
        self.switch_press(area, point);
        // What shell_pointer_target puts above the panel takes the press.
        let covered = self.overlay_under(point)
            || self.handoff_pointer_target(point).is_some()
            || (self.launcher.open && !self.launcher.home && self.launcher_takes(point))
            || self.doctor_contains(point);
        let inside = !covered && self.notifications_here() && self.notification_panel().contains(point);
        if inside != self.notifications.focused {
            self.notifications.focused = inside;
            if inside {
                self.doctor.focused = false;
            }
            self.force_keyboard_focus_sync();
        }
    }

    /// Keys while the panel has the keyboard. Delete or C clears the history;
    /// the arrows, the page keys, Home and End scroll it. Tab and Shift+Tab
    /// choose a button on the media or plugin cards, or a notification;
    /// Enter or Space presses the button or opens the notification, and
    /// Backspace dismisses it. D switches to the next mode, T opens how long
    /// the mode lasts, M mutes the sound and Esc closes the panel.
    pub fn notification_panel_key(&mut self, sym: u32, mods: crate::config::Mods) {
        use smithay::input::keyboard::keysyms;
        // An open menu has the keys; its time field takes Ctrl ones too.
        if self.notifications.mode_menu.as_ref().is_some_and(|menu| menu.field.is_some()) && !mods.logo {
            return self.mode_menu_key(sym, mods);
        }
        if mods.ctrl || mods.alt || mods.logo {
            return;
        }
        if self.notifications.mode_menu.is_some() {
            return self.mode_menu_key(sym, mods);
        }
        if self.notifications.sound_menu.is_some() && crate::text_input::plain(sym) == keysyms::KEY_Escape {
            self.notifications.sound_menu = None;
            return self.schedule_render();
        }
        let selected = self.selected_notification();
        let buttons = self.panel_buttons();
        let chosen_button = self.chosen_panel_button().cloned();
        // One place per stop: the buttons, then the history.
        let count = buttons.len() as isize;
        let last = self.notifications.history.len() as isize - 1;
        let place = match &chosen_button {
            Some(chosen) => buttons.iter().position(|button| button == chosen).map(|index| index as isize),
            None => selected.as_ref().map(|(index, _)| *index as isize + count),
        };
        let top = self.notifications.offset as isize;
        // From nothing chosen, Tab starts at the first button, or else at
        // the top card in view.
        let first = if count > 0 { 0 } else { top };
        match crate::text_input::plain(sym) {
            keysyms::KEY_Escape => self.close_notifications(),
            keysyms::KEY_Delete | keysyms::KEY_c | keysyms::KEY_C => self.clear_notifications(),
            keysyms::KEY_d | keysyms::KEY_D if self.mode_switch_area().is_some() => self.next_notification_mode(),
            keysyms::KEY_t | keysyms::KEY_T if self.mode_switch_area().is_some() => {
                let mode = self.notifications.mode.mode;
                self.toggle_mode_menu(mode);
            }
            keysyms::KEY_m | keysyms::KEY_M => {
                use crate::media::{Adjust, Control, Level};
                let mute = Control::Level(Level::Volume, Adjust::Mute(tileception_media::Toggle::Flip));
                if let Err(error) = self.media_command(mute) {
                    tracing::info!("{error}");
                }
            }
            keysyms::KEY_Tab if mods.shift => {
                self.choose_panel_stop(&buttons, place.map_or(count + last, |place| place - 1))
            }
            keysyms::KEY_ISO_Left_Tab => {
                self.choose_panel_stop(&buttons, place.map_or(count + last, |place| place - 1))
            }
            keysyms::KEY_Tab => self.choose_panel_stop(&buttons, place.map_or(first, |place| place + 1)),
            keysyms::KEY_Up => self.notification_scroll(-1),
            keysyms::KEY_Down => self.notification_scroll(1),
            keysyms::KEY_Page_Up => self.notification_scroll(-PAGE),
            keysyms::KEY_Page_Down => self.notification_scroll(PAGE),
            keysyms::KEY_Home => self.notifications.offset = 0,
            keysyms::KEY_End => self.notification_scroll(last.max(0)),
            keysyms::KEY_Return | keysyms::KEY_KP_Enter | keysyms::KEY_space => {
                if let Some(button) = &chosen_button {
                    self.press_panel_button(button);
                } else if let Some((_, notification)) = &selected {
                    self.open_notification(notification);
                }
            }
            keysyms::KEY_BackSpace => {
                if let Some((index, notification)) = &selected {
                    let index = *index;
                    // The next one down takes its place, or the one above at the end.
                    let next = self
                        .history_newest_first()
                        .nth(index + 1)
                        .or_else(|| index.checked_sub(1).and_then(|above| self.history_newest_first().nth(above)))
                        .map(|next| next.id);
                    self.dismiss_from_history(notification);
                    self.notifications.selected = next.map(Chosen::Notification);
                    self.reveal_selected();
                }
            }
            _ => return,
        }
        self.schedule_render();
    }

    /// The history as the panel lists it.
    fn history_newest_first(&self) -> impl Iterator<Item = &Arc<Notification>> {
        self.notifications.history.iter().rev()
    }

    /// The chosen notification and its place in the list, while it is there.
    fn selected_notification(&self) -> Option<(usize, Arc<Notification>)> {
        let Some(Chosen::Notification(selected)) = self.notifications.selected else { return None };
        self.history_newest_first()
            .enumerate()
            .find(|(_, notification)| notification.id == selected)
            .map(|(index, notification)| (index, notification.clone()))
    }

    /// Chooses the notification at `index` in the list, kept within it, and
    /// scrolls it into view.
    fn select_notification(&mut self, index: isize) {
        let len = self.notifications.history.len();
        if len == 0 {
            return;
        }
        let index = index.clamp(0, len as isize - 1) as usize;
        let selected = self.history_newest_first().nth(index).map(|notification| notification.id);
        self.notifications.selected = selected.map(Chosen::Notification);
        self.reveal_selected();
    }

    /// The buttons above the history Tab reaches, in order: the media
    /// card's that work now, then the plugin cards'.
    fn panel_buttons(&self) -> Vec<crate::panel::PanelButton> {
        use crate::panel::PanelButton;
        use tileception_media::PlayerCommand;
        let controls = self.panel_controls();
        let mut buttons = Vec::new();
        if controls.controls.media.is_some()
            && let Some(player) = self.media.snapshot.media.active()
        {
            for command in [PlayerCommand::Previous, PlayerCommand::PlayPause, PlayerCommand::Next] {
                if crate::controls::media_can(player, command) {
                    buttons.push(PanelButton::Media(command));
                }
            }
        }
        buttons.extend(self.panel_card_buttons(&controls.plugins));
        buttons
    }

    /// The button chosen with Tab, while the panel has the keyboard.
    pub(crate) fn chosen_panel_button(&self) -> Option<&crate::panel::PanelButton> {
        match &self.notifications.selected {
            Some(Chosen::Button(button)) if self.notifications_keyboard() => Some(button),
            _ => None,
        }
    }

    /// Chooses the stop at `index`, kept within them: a button, or past
    /// the buttons, a notification.
    fn choose_panel_stop(&mut self, buttons: &[crate::panel::PanelButton], index: isize) {
        let total = buttons.len() + self.notifications.history.len();
        if total == 0 {
            return;
        }
        let index = index.clamp(0, total as isize - 1) as usize;
        match buttons.get(index) {
            Some(button) => self.notifications.selected = Some(Chosen::Button(button.clone())),
            None => self.select_notification((index - buttons.len()) as isize),
        }
    }

    /// What a click on the button does.
    fn press_panel_button(&mut self, button: &crate::panel::PanelButton) {
        use crate::panel::PanelButton;
        match button {
            PanelButton::Media(command) => self.media_request(tileception_media::Request::Player(*command)),
            PanelButton::Card { plugin, card, button } => {
                let layout = self.panel_controls().plugins;
                self.panel_press_card_button(&layout, plugin, card, *button);
            }
        }
    }

    /// Scrolls the panel so the chosen notification shows in full.
    fn reveal_selected(&mut self) {
        let Some((index, notification)) = self.selected_notification() else { return };
        if index < self.notifications.offset {
            self.notifications.offset = index;
            return;
        }
        while self.notifications.offset < index
            && !self.notification_cards().iter().any(|card| card.notification.id == notification.id)
        {
            self.notifications.offset += 1;
        }
    }

    /// What a click on the card does: its default action, if it has one.
    fn open_notification(&mut self, notification: &Notification) {
        if notification.default_action {
            self.press_notification_button(notification, "default");
        } else if let Some((key, _)) = notification.actions.first() {
            self.press_notification_button(notification, &key.clone());
        }
    }

    /// What the card's close button does in the panel.
    fn dismiss_from_history(&mut self, notification: &Notification) {
        let kind = self.notification_kind(notification);
        let action = PointerAction::Close { identifier: notification.id, kind, history: true };
        crate::ui::PointerAction::activate(&action, self, Rect::default(), Point::new(0.0, 0.0));
    }

    /// Whether the notifications show on the output being drawn or handled.
    pub fn notifications_here(&self) -> bool {
        match &self.notifications.home {
            Some(home) => self.output.as_ref() == Some(home),
            None => self.is_active_output(),
        }
    }

    /// A new popup shows on the active screen, unless others are showing
    /// somewhere already: they stay together.
    fn notifications_appear(&mut self) {
        if !self.notifications.open && self.notifications.active.is_empty() {
            self.notifications.home = self.outputs.active.clone();
        }
    }

    fn refresh_notification_history(&mut self) {
        if let Some(service) = &self.notifications.service {
            self.notifications.history = service.history().into_iter().map(Arc::new).collect();
        }
    }

    pub fn notification_request(&mut self, request: Request) {
        match &request {
            Request::ClearHistory => self.notifications.history.clear(),
            Request::RemoveFromHistory(notification_id) => {
                self.notifications.history.retain(|entry| entry.id != *notification_id)
            }
            _ => {}
        }
        if let Some(service) = &self.notifications.service {
            service.send(request);
        }
    }

    /// Shows a notification of Tileception's own without the service, such as
    /// the greeter's feedback. It goes away by itself and stays out of the history.
    pub fn notify_locally(&mut self, summary: &str, body: &str, urgency: Urgency) {
        let mut notification = self.local_notification(summary, body);
        notification.urgency = urgency;
        notification.category = Some(LOCAL_CATEGORY.into());
        notification.expires = Some(Instant::now() + LOCAL_TIMEOUT);
        let notification_id = notification.id;
        self.notifications_appear();
        self.notifications.active.insert(notification_id, Arc::new(notification));
        let timer = smithay::reexports::calloop::timer::Timer::from_duration(LOCAL_TIMEOUT);
        let removed = self.loop_handle.insert_source(timer, move |_, _, state| {
            state.notifications.active.remove(&notification_id);
            state.schedule_render();
            smithay::reexports::calloop::timer::TimeoutAction::Drop
        });
        if let Err(error) = removed {
            tracing::warn!("could not time a notification: {error}");
        }
        self.schedule_render();
    }

    /// A notification of Tileception's own with buttons, each running a
    /// command (or only closing it). It stays until a button is pressed, it
    /// is closed, or [`Tileception::close_local_notification`]. Returns its id.
    pub fn notify_locally_with(
        &mut self,
        summary: &str,
        body: &str,
        urgency: Urgency,
        buttons: Vec<(&str, Option<crate::commands::Command>)>,
    ) -> u32 {
        let labels = buttons.iter().enumerate().map(|(index, (label, _))| (index.to_string(), (*label).to_string()));
        let mut notification = self.local_notification(summary, body);
        notification.urgency = urgency;
        notification.category = Some(LOCAL_CATEGORY.into());
        notification.actions = labels.collect();
        let notification_id = notification.id;
        let commands = buttons.into_iter().map(|(_, command)| command).collect();
        self.notifications.local_actions.insert(notification_id, commands);
        self.notifications_appear();
        self.notifications.active.insert(notification_id, Arc::new(notification));
        self.schedule_render();
        notification_id
    }

    /// Gives one of Tileception's own notifications a picture.
    pub fn set_local_notification_image(&mut self, notification_id: u32, image: Image) {
        if let Some(notification) = self.notifications.active.get_mut(&notification_id) {
            Arc::make_mut(notification).image = Some(image);
            self.notifications.thumbnails.borrow_mut().remove(&notification_id);
            self.schedule_render();
        }
    }

    pub fn close_local_notification(&mut self, notification_id: u32) {
        self.notifications.local_actions.remove(&notification_id);
        if self.notifications.active.remove(&notification_id).is_some() {
            self.check_answering();
            self.schedule_render();
        }
    }

    /// A button of one of Tileception's own notifications was pressed.
    fn local_action(&mut self, notification_id: u32, key: &str) {
        let command = self
            .notifications
            .local_actions
            .get_mut(&notification_id)
            .and_then(|commands| key.parse::<usize>().ok().and_then(|index| commands.get_mut(index)))
            .and_then(Option::take);
        self.close_local_notification(notification_id);
        if let Some(command) = command
            && let Err(error) = self.execute(command)
        {
            self.notify_locally("That did not work", &error, Urgency::Normal);
        }
    }

    /// Asks something in a notification of Tileception's own, with a button
    /// per `(key, label)` in `actions`. It stays up until a button or its close
    /// button is clicked, which goes to the question's owner, or
    /// until [`Tileception::withdraw_question`]. Returns its id.
    pub fn ask_locally(
        &mut self,
        summary: &str,
        body: &str,
        desktop_entry: Option<String>,
        actions: Vec<(String, String)>,
    ) -> u32 {
        let mut notification = self.local_notification(summary, body);
        notification.desktop_entry = desktop_entry;
        notification.actions = actions;
        notification.category = Some(PROMPT_CATEGORY.into());
        let notification_id = notification.id;
        self.notifications_appear();
        self.notifications.active.insert(notification_id, Arc::new(notification));
        self.notifications.questions.insert(notification_id);
        self.schedule_render();
        notification_id
    }

    /// Takes away a question that no longer needs an answer.
    pub fn withdraw_question(&mut self, notification_id: u32) {
        if self.notifications.questions.remove(&notification_id) {
            self.notifications.active.remove(&notification_id);
            self.check_answering();
            self.schedule_render();
        }
    }

    /// A notification from Tileception itself, with the next local id.
    fn local_notification(&mut self, summary: &str, body: &str) -> Notification {
        // Counting down from the top keeps clear of the service's ids.
        self.notifications.local = self.notifications.local.wrapping_add(1);
        Notification {
            id: u32::MAX - self.notifications.local,
            app_name: "Tileception".into(),
            app_icon: None,
            summary: summary.into(),
            body: html_escape::encode_text(body).into_owned(),
            actions: Vec::new(),
            default_action: false,
            urgency: Urgency::Normal,
            image: None,
            desktop_entry: None,
            category: None,
            resident: false,
            transient: true,
            suppress_sound: true,
            x: None,
            y: None,
            sound_file: None,
            created: std::time::SystemTime::now(),
            expires: None,
            show_popup: true,
        }
    }

    /// Tileception's own lock screen has just come up: what it shows of
    /// notifications starts from nothing.
    pub fn notifications_locked(&mut self) {
        self.notifications.while_locked.clear();
    }

    /// Keeps what comes in while Tileception's own lock screen shows, as the
    /// history would (not transient ones), a replacement in its place.
    fn came_in_while_locked(&mut self, notification: &Arc<Notification>) {
        if !matches!(self.lock, LockState::BuiltIn) || notification.transient {
            return;
        }
        let kept = &mut self.notifications.while_locked;
        match kept.iter_mut().find(|entry| entry.id == notification.id) {
            Some(entry) => *entry = notification.clone(),
            None => kept.push(notification.clone()),
        }
        if kept.len() > WHILE_LOCKED {
            kept.remove(0);
        }
    }

    /// How much of a notification the lock screen shows, while locked:
    /// `None` when it is not restricted (unlocked, or Tileception's own).
    fn lock_shows(&self, notification: &Notification) -> Option<Shown> {
        if !self.is_locked() || notification.category.as_deref() == Some(LOCAL_CATEGORY) {
            return None;
        }
        // Questions wait for an answer after unlocking.
        if notification.category.as_deref() == Some(PROMPT_CATEGORY)
            || self.notifications.questions.contains(&notification.id)
            || !matches!(self.lock, LockState::BuiltIn)
        {
            return Some(Shown::None);
        }
        let allowed = self.greeter.as_deref().is_some_and(crate::greeter::Greeter::shows_notifications);
        Some(self.config.file.lock.on_lock_screen(&lock_apps(notification), allowed, self.do_not_disturb()))
    }

    /// For the lock screen: how many notifications came in since it showed
    /// that it may count, and those it may show more of, newest first.
    pub fn lock_screen_notifications(&self) -> (usize, Vec<(Arc<Notification>, Shown)>) {
        let mut count = 0;
        let mut shown = Vec::new();
        for notification in self.notifications.while_locked.iter().rev() {
            let level = self.lock_shows(notification).unwrap_or(Shown::Full);
            if level >= Shown::Count {
                count += 1;
            }
            if level >= Shown::Apps {
                shown.push((notification.clone(), level));
            }
        }
        (count, shown)
    }

    /// The notification panel's "Show on lock screen" for an app: writes
    /// its exception in `[lock.notification_apps]`.
    fn toggle_lock_screen_app(&mut self, desktop_entry: Option<&str>, app_name: &str) {
        let apps = [desktop_entry.unwrap_or_default(), app_name];
        let lock = &self.config.file.lock;
        let shown = lock.toggled(&apps);
        let keys: Vec<String> = lock.app_keys(&apps).into_iter().map(str::to_string).collect();
        let Some(first) = keys.first().cloned() else { return };
        let written = crate::config_edit::set_lock_notification_app(
            &crate::config::path(),
            &keys.iter().map(String::as_str).collect::<Vec<_>>(),
            shown,
        );
        match written {
            Ok(()) => {
                // At once, before the config file is read again.
                let exceptions = &mut self.config.file.lock.notification_apps;
                exceptions.retain(|key, _| !keys.contains(key));
                if let Some(shown) = shown {
                    exceptions.insert(first, shown);
                }
                self.schedule_render();
            }
            Err(error) => self.notify_locally("Could not change the lock screen", &error, Urgency::Normal),
        }
    }

    pub fn do_not_disturb(&self) -> bool {
        self.notifications.service.as_ref().is_some_and(|service| service.do_not_disturb())
    }

    /// The history's length and whether Do not disturb is on, while the
    /// built-in service runs.
    pub fn notification_status(&self) -> Option<(usize, bool)> {
        let service = self.notifications.service.as_ref()?;
        Some((service.history_len(), service.do_not_disturb()))
    }

    /// Plays what a new notification sounds like in the Sound mode: by its
    /// urgency, in its app's family. The built-in alarms ring until they are
    /// dismissed, also in Silent when `alarms_in_silent` says so. Nothing
    /// sounds in Do not disturb.
    fn notification_sound(&mut self, notification: &Notification) {
        use crate::notification_mode::Mode;
        let config = &self.config.file.notifications;
        let alarm = notification.category.as_deref() == Some(ALARM_CATEGORY);
        let allowed = match self.notifications.mode.mode {
            Mode::Sound => true,
            Mode::Silent => alarm && config.alarms_in_silent,
            Mode::DoNotDisturb => false,
        };
        if !allowed || notification.suppress_sound {
            return;
        }
        let Some(family) = config.app_sound(&lock_apps(notification)).family() else { return };
        let volume = f32::from(config.volume.min(100)) / 100.0;
        if alarm {
            self.notifications.sounds.ring(notification.id, family, volume);
        } else {
            self.notifications.sounds.play(family, crate::sound::Clip::for_urgency(notification.urgency), volume);
        }
    }

    /// Empties the history and dismisses every popup: the panel's Clear all.
    pub fn clear_notifications(&mut self) {
        self.notification_request(Request::ClearHistory);
        self.notification_request(Request::DismissAll);
    }

    /// How many notifications the history holds.
    pub fn notification_count(&self) -> usize {
        self.notifications.history.len()
    }

    /// The newest notification's summary.
    pub fn latest_notification(&self) -> Option<String> {
        self.notifications.history.last().map(|notification| notification.summary.clone())
    }

    pub fn notification_scroll(&mut self, delta: isize) {
        let len = self.notifications.history.len();
        self.notifications.offset = self.notifications.offset.saturating_add_signed(delta).min(len.saturating_sub(1));
    }

    /// The history panel, against the right edge.
    pub(crate) fn notification_panel(&self) -> Rect {
        let screen = self.beside_sliding_bar(self.usable_rect());
        let width = screen.w.clamp(1.0, PANEL_W);
        Rect::new(screen.right() - width, screen.y, width, screen.h.max(1.0))
    }

    /// The history panel's own controls.
    pub fn panel_controls(&self) -> PanelControls {
        let panel = self.notification_panel();
        let controls = self.controls_layout(panel.x + 12.0, panel.y + 58.0, panel.w - 24.0);
        let below = if controls.rect.h > 0.0 { controls.rect.bottom() + 10.0 } else { panel.y + 58.0 };
        let plugins = self.panel_layout(panel.x + 12.0, below, panel.w - 24.0);
        let below = if plugins.rect.h > 0.0 { plugins.rect.bottom() + 10.0 } else { below };
        // Without the built-in service there is no mode to switch.
        let dnd_h = if self.builtin_notifications() { crate::notification_switch::SWITCH_H } else { 0.0 };
        let dnd = Rect::new(panel.x + 12.0, below, panel.w - 24.0, dnd_h);
        PanelControls {
            clear: Rect::new(panel.right() - 16.0 - 96.0, panel.y + 14.0, 96.0, 32.0),
            controls,
            plugins,
            dnd,
            cards_top: dnd.bottom() + 18.0,
        }
    }

    /// The cards on screen: popups in the bottom right corner, newest
    /// lowest, or the history in the panel, newest first.
    fn notification_cards(&self) -> Vec<Card> {
        let screen = self.usable_rect();
        if self.notifications.open {
            let panel = self.notification_panel();
            let mut next_top = self.panel_controls().cards_top;
            let width = panel.w - 24.0;
            let mut cards = Vec::new();
            for notification in self.notifications.history.iter().rev().skip(self.notifications.offset) {
                let card = self.card_layout(notification.clone(), panel.x + 12.0, next_top, width, false);
                if card.rect.bottom() > panel.bottom() - 52.0 {
                    break;
                }
                next_top = card.rect.bottom() + 8.0;
                cards.push(card);
            }
            return cards;
        }
        let width = (screen.w - 24.0).clamp(1.0, POPUP_W);
        let mut items: Vec<_> = self
            .notifications
            .active
            .values()
            .filter(|notification| {
                notification.show_popup && self.lock_shows(notification).is_none_or(|shown| shown >= Shown::Apps)
            })
            .cloned()
            .collect();
        // Questions waiting for an answer (no timeout, with buttons) stay in the
        // corner, so newer popups cannot push them off screen.
        items.sort_by_key(|notification| {
            (notification.expires.is_some() || notification.actions.is_empty(), std::cmp::Reverse(notification.created))
        });
        items.truncate(3);
        let mut bottom =
            self.strip_now().map_or(screen.bottom() - 14.0, |strip| (strip.y - 12.0).min(screen.bottom() - 14.0));
        let mut cards = Vec::new();
        for notification in items {
            let mut card = self.card_layout(notification, screen.right() - width - 14.0, 0.0, width, true);
            let height = card.rect.h;
            if bottom - height < screen.y + 14.0 {
                break;
            }
            card.shift(bottom - height);
            bottom -= height + 10.0;
            cards.push(card);
        }
        cards
    }

    /// Lays out one card with its top left at (x, y).
    fn card_layout(&self, notification: Arc<Notification>, left: f64, card_top: f64, width: f64, popup: bool) -> Card {
        let theme = &self.theme;
        // On the lock screen: no buttons, and the content only in full.
        let locked = self.lock_shows(&notification);
        let content = locked.is_none_or(|shown| shown >= Shown::Full);
        let mut cursor = card_top + 14.0;
        let mut header = Rect::new(left + 16.0, cursor, width - 56.0, 20.0);
        let close = Rect::new(left + width - 36.0, card_top + 8.0, 28.0, 28.0);
        // In the panel, apps' own notifications say whether they show on the lock screen.
        let lock_toggle = (!popup
            && self.builtin_lock_screen()
            && notification.category.as_deref() != Some(LOCAL_CATEGORY)
            && notification.category.as_deref() != Some(PROMPT_CATEGORY)
            && lock_apps(&notification).iter().any(|app| !app.is_empty()))
        .then(|| Rect::new(close.x - 32.0, close.y, 28.0, 28.0));
        if lock_toggle.is_some() {
            header.w -= 32.0;
        }
        // And the sound they make, while the built-in service runs.
        let bell = (!popup
            && self.notifications.service.is_some()
            && notification.category.as_deref() != Some(LOCAL_CATEGORY)
            && notification.category.as_deref() != Some(PROMPT_CATEGORY)
            && lock_apps(&notification).iter().any(|app| !app.is_empty()))
        .then(|| Rect::new(lock_toggle.unwrap_or(close).x - 32.0, close.y, 28.0, 28.0));
        if bell.is_some() {
            header.w -= 32.0;
        }
        cursor = header.bottom() + 10.0;
        let picture = if content { self.notification_preview(&notification) } else { None };
        let preview = picture.map(|(_, image_width, image_height)| {
            let placed = Rect::new(
                left + 16.0 + ((width - 32.0 - image_width as f64) / 2.0).max(0.0),
                cursor,
                image_width as f64,
                image_height as f64,
            );
            cursor = placed.bottom() + 12.0;
            placed
        });
        let icon = (preview.is_none() && self.notification_icon(&notification, ICON).is_some())
            .then(|| Rect::new(left + 16.0, cursor + 2.0, ICON as f64, ICON as f64));
        let text_x = icon.map_or(left + 16.0, |icon_rect| icon_rect.right() + 12.0);
        let text_w = left + width - 16.0 - text_x;
        let summary = Rect::new(text_x, cursor, text_w, 22.0);
        let summary_text = if content { notification.summary.clone() } else { "New notification".into() };
        let body_text = if content { plain_body(&notification.body) } else { "Shown after unlocking".into() };
        let body_h = if body_text.trim().is_empty() {
            0.0
        } else {
            self.text
                .borrow_mut()
                .styled(
                    body_text.trim(),
                    &theme.font,
                    theme.font_size * 0.95,
                    theme.muted,
                    text_w as f32,
                    400,
                    BODY_LINES,
                )
                .map_or(0.0, |rendered| rendered.height)
        };
        let body = Rect::new(text_x, summary.bottom() + 2.0, text_w, body_h);
        cursor = body.bottom().max(icon.map_or(0.0, |icon_rect| icon_rect.bottom()));
        let count = if locked.is_some() { 0 } else { notification.actions.len() };
        let actions = if count > 0 {
            let top = cursor + 12.0;
            let gap = 8.0;
            let each = (width - 32.0 - gap * (count as f64 - 1.0)) / count as f64;
            cursor = top + 32.0;
            (0..count).map(|index| Rect::new(left + 16.0 + index as f64 * (each + gap), top, each, 32.0)).collect()
        } else {
            Vec::new()
        };
        let rect = Rect::new(left, card_top, width, cursor + 16.0 - card_top);
        Card {
            notification,
            rect,
            header,
            close: if locked.is_some() { None } else { Some(close) },
            lock_toggle,
            bell,
            preview,
            icon,
            summary,
            summary_text,
            body,
            body_text,
            actions,
            popup,
        }
    }

    /// The bell of the card with this notification, while it is in view.
    pub(crate) fn card_bell(&self, notification_id: u32) -> Option<Rect> {
        if !self.notifications.open {
            return None;
        }
        self.notification_cards().into_iter().find(|card| card.notification.id == notification_id)?.bell
    }

    /// Where the mode switch is, while the panel shows it.
    pub(crate) fn mode_switch_area(&self) -> Option<Rect> {
        (self.notifications.open && self.builtin_notifications() && self.notifications.service.is_some())
            .then(|| self.panel_controls().dnd)
    }

    pub fn notifications_contain(&self, point: Point) -> bool {
        if !self.notifications_here() || self.overlay_under(point) {
            return false;
        }
        if self.notifications.open && self.notification_panel().contains(point) {
            return true;
        }
        self.notification_cards().iter().any(|card| card.rect.contains(point))
    }

    pub fn notification_click(&mut self, point: Point) {
        if let Some(target) = self.notification_pointer_target(point) {
            target.activate(self, point);
        }
    }

    pub(crate) fn notification_pointer_target(&self, point: Point) -> Option<crate::ui::PointerTarget> {
        use crate::ui::PointerTarget;
        if self.notifications.open {
            // The menus go over everything else in the panel.
            if let Some(target) = self.sound_menu_pointer_target(point) {
                return target;
            }
            let controls = self.panel_controls();
            if let Some(area) = self.mode_switch_area()
                && let Some(target) = self.switch_pointer_target(area, point)
            {
                return target;
            }
            if controls.clear.contains(point) {
                return Some(PointerTarget::button(controls.clear, PointerAction::Clear));
            }
            if let Some(target) = self.controls_pointer_target(&controls.controls, point) {
                return Some(target);
            }
            if let Some(target) = self.panel_pointer_target(&controls.plugins, point) {
                return Some(target);
            }
        }
        for card in self.notification_cards() {
            let notification = &card.notification;
            let kind = self.notification_kind(notification);
            if let Some(close) = card.close.filter(|close| close.contains(point)) {
                return Some(PointerTarget::button(
                    close,
                    PointerAction::Close { identifier: notification.id, kind, history: self.notifications.open },
                ));
            }
            if let Some(bell) = card.bell.filter(|bell| bell.contains(point)) {
                let apps = lock_apps(notification).map(str::to_string);
                return Some(self.bell_target(bell, notification.id, apps));
            }
            if let Some(toggle) = card.lock_toggle.filter(|toggle| toggle.contains(point)) {
                return Some(PointerTarget::button(
                    toggle,
                    PointerAction::LockScreen {
                        desktop_entry: notification.desktop_entry.clone(),
                        app_name: notification.app_name.clone(),
                    },
                ));
            }
            for (rect, (key, _)) in card.actions.iter().zip(&notification.actions) {
                if rect.contains(point) {
                    return Some(PointerTarget::button(
                        *rect,
                        PointerAction::Invoke { identifier: notification.id, kind, key: key.clone() },
                    ));
                }
            }
            if notification.default_action
                && card.rect.contains(point)
                && kind == NotificationKind::Service
                && card.close.is_some()
            {
                return Some(PointerTarget::button(
                    card.rect,
                    PointerAction::Invoke { identifier: notification.id, kind, key: "default".into() },
                ));
            }
        }
        None
    }

    fn notification_kind(&self, notification: &Notification) -> NotificationKind {
        if notification.category.as_deref() == Some(LOCAL_CATEGORY) {
            NotificationKind::Local
        } else if self.notifications.questions.contains(&notification.id) {
            NotificationKind::Question
        } else {
            NotificationKind::Service
        }
    }

    /// Presses one of a notification's buttons, by its action key: clicked,
    /// or chosen from the keyboard.
    pub fn press_notification_button(&mut self, notification: &Notification, key: &str) {
        if notification.category.as_deref() == Some(LOCAL_CATEGORY) {
            // Tileception's own: the service knows nothing of it.
            self.local_action(notification.id, key);
        } else if self.notifications.questions.contains(&notification.id) {
            self.withdraw_question(notification.id);
            self.answer_typing(notification.id, Some(key));
            self.answer_shortcut(notification.id);
        } else {
            self.notification_request(Request::Invoke { id: notification.id, action: key.to_string(), token: None });
        }
    }

    /// The newest popup with buttons: the one Super+Enter answers.
    fn popup_to_answer(&self) -> Option<Arc<Notification>> {
        let locked = self.is_locked();
        self.notifications
            .active
            .values()
            .filter(|notification| notification.show_popup && !notification.actions.is_empty())
            .filter(|notification| !locked || notification.category.as_deref() == Some(LOCAL_CATEGORY))
            .max_by_key(|notification| notification.created)
            .cloned()
    }

    /// Gives the newest popup with buttons the keyboard, or gives it back.
    pub fn answer_notification(&mut self) {
        if self.notifications.answering.take().is_none()
            && !self.notifications.open
            && let Some(notification) = self.popup_to_answer()
        {
            self.notifications.answering = Some((notification.id, 0));
        }
        self.force_keyboard_focus_sync();
        self.schedule_render();
    }

    /// Keys while a popup has the keyboard: arrows and Tab move between its
    /// buttons, a number or Enter presses one, Esc gives the keyboard back.
    pub fn notification_key(&mut self, sym: u32, mods: crate::config::Mods) {
        use smithay::input::keyboard::keysyms;
        let Some((notification_id, selected)) = self.notifications.answering else { return };
        let Some(notification) = self.notifications.active.get(&notification_id).cloned() else {
            return self.check_answering();
        };
        let count = notification.actions.len().max(1);
        let pressed = match sym {
            keysyms::KEY_Escape => None,
            keysyms::KEY_Left | keysyms::KEY_Up | keysyms::KEY_ISO_Left_Tab => {
                self.notifications.answering = Some((notification_id, (selected + count - 1) % count));
                return self.schedule_render();
            }
            keysyms::KEY_Tab if mods.shift => {
                self.notifications.answering = Some((notification_id, (selected + count - 1) % count));
                return self.schedule_render();
            }
            keysyms::KEY_Right | keysyms::KEY_Down | keysyms::KEY_Tab => {
                self.notifications.answering = Some((notification_id, (selected + 1) % count));
                return self.schedule_render();
            }
            keysyms::KEY_Return | keysyms::KEY_KP_Enter | keysyms::KEY_space if !mods.logo => Some(selected),
            keysyms::KEY_1..=keysyms::KEY_9 => Some((sym - keysyms::KEY_1) as usize),
            keysyms::KEY_KP_1..=keysyms::KEY_KP_9 => Some((sym - keysyms::KEY_KP_1) as usize),
            // Super+Enter again gives the keyboard back too.
            keysyms::KEY_Return | keysyms::KEY_KP_Enter => None,
            _ => return,
        };
        self.notifications.answering = None;
        self.force_keyboard_focus_sync();
        self.schedule_render();
        if let Some((key, _)) = pressed.and_then(|index| notification.actions.get(index)) {
            self.press_notification_button(&notification, key);
        }
    }

    /// Gives the keyboard back once the popup being answered has gone.
    fn check_answering(&mut self) {
        let Some((notification_id, _)) = self.notifications.answering else { return };
        let showing =
            self.notifications.active.get(&notification_id).is_some_and(|notification| notification.show_popup);
        if !showing || self.notifications.open {
            self.notifications.answering = None;
            self.force_keyboard_focus_sync();
        }
    }

    /// The key that gives popups the keyboard, such as "Super+Enter", if bound.
    fn answer_shortcut_label(&self) -> Option<String> {
        let bind = self
            .config
            .binds
            .iter()
            .find(|bind| bind.commands.first() == Some(&crate::commands::Command::AnswerNotification))?;
        let label = crate::launcher::combo_label(bind.mods, bind.key);
        Some(if self.nested { label.replacen("Super", "Alt", 1) } else { label })
    }

    pub fn notification_elements(&self, renderer: &mut GlesRenderer) -> Vec<OutputElement> {
        if !self.notifications_here() {
            return Vec::new();
        }
        let theme = &self.theme;
        let pointer = Point::new(self.pointer.x, self.pointer.y);
        let mut painter = Painter::new(self, renderer);
        if self.notifications.open {
            let panel = self.notification_panel();
            let controls = self.panel_controls();
            painter.edge_panel(panel, false);
            let title = Style::new(theme.font_size * 1.3, theme.text).bold();
            let (title_w, _) = painter.text(
                if self.builtin_notifications() { "Notifications" } else { "Controls" },
                Rect::new(panel.x + 18.0, panel.y + 14.0, panel.w - 150.0, 32.0),
                title,
            );
            let count = self.notifications.history.len();
            if count > 0 {
                let label = count.to_string();
                let style = Style::new(theme.font_size * 0.8, theme.on_accent).bold().align(Align::Centre);
                let width = (painter.measure(&label, 100.0, style).0 + 14.0).max(22.0);
                let chip = Rect::new(panel.x + 18.0 + title_w + 10.0, panel.y + 20.0, width, 20.0);
                painter.shape(chip, Shape { radius: 10.0, fill: theme.accent_fill(), ..Default::default() });
                painter.text(&label, chip, style);
            }
            if count > 0 {
                let hover = controls.clear.contains(pointer);
                if hover {
                    painter.fill(controls.clear, theme.surface_raised, theme.radius);
                }
                let colour = if hover { theme.text } else { theme.muted };
                painter.glyph(
                    glyph::CLEAR,
                    Rect::new(controls.clear.x + 6.0, controls.clear.y, 24.0, 32.0),
                    15,
                    colour,
                );
                painter.text(
                    "Clear all",
                    Rect::new(controls.clear.x + 32.0, controls.clear.y, 60.0, 32.0),
                    Style::new(theme.font_size * 0.95, colour),
                );
            }

            self.draw_controls(&mut painter, &controls.controls, pointer);
            self.draw_panel_blocks(&mut painter, &controls.plugins, pointer);

            if let Some(area) = self.mode_switch_area() {
                self.draw_mode_switch(&mut painter, area, pointer);
            }
            painter.fill(Rect::new(panel.x + 12.0, controls.cards_top - 10.0, panel.w - 24.0, 1.0), theme.border, 0.0);

            // The empty state needs room; the smart home block can take it.
            if self.notifications.history.is_empty() && panel.bottom() - controls.cards_top > 240.0 {
                let mid = (controls.cards_top + panel.bottom()) / 2.0 - 40.0;
                painter.fill(
                    Rect::new(panel.x + panel.w / 2.0 - 32.0, mid - 32.0, 64.0, 64.0),
                    theme.surface_raised,
                    32.0,
                );
                painter.glyph(glyph::BELL, Rect::new(panel.x, mid - 32.0, panel.w, 64.0), 28, theme.muted);
                let (headline, detail) = if !self.builtin_notifications() {
                    ("Built-in notifications are off", "See Notifications in Settings")
                } else if self.notifications.service.is_some() {
                    ("No notifications", "You're all caught up")
                } else {
                    ("Notifications unavailable", "The notification service did not start")
                };
                painter.text(
                    headline,
                    Rect::new(panel.x, mid + 44.0, panel.w, 24.0),
                    Style::new(theme.font_size * 1.05, theme.text).bold().align(Align::Centre),
                );
                painter.text(
                    detail,
                    Rect::new(panel.x, mid + 68.0, panel.w, 22.0),
                    Style::new(theme.font_size * 0.9, theme.muted).align(Align::Centre),
                );
            }
            let builtin = self.builtin_notifications();
            let typing = self.notifications.mode_menu.as_ref().is_some_and(|menu| menu.field.is_some());
            let mut hints: Vec<(&str, &str)> = if !self.notifications_keyboard() {
                if builtin { vec![("Esc", "Close"), ("Scroll", "Older")] } else { vec![("Esc", "Close")] }
            } else if typing {
                vec![("Enter", "Set"), ("Esc", "Back")]
            } else if self.notifications.mode_menu.is_some() {
                vec![("Enter", "Choose"), ("Esc", "Close menu")]
            } else if self.chosen_panel_button().is_some() {
                vec![("Enter", "Press"), ("Tab", "Next"), ("Esc", "Close")]
            } else if self.selected_notification().is_some() {
                vec![("Enter", "Open"), ("Backspace", "Dismiss"), ("Del", "Clear all"), ("Esc", "Close")]
            } else if builtin && count > 0 {
                vec![("Del", "Clear all"), ("Tab", "Choose"), ("D", "Mode"), ("T", "How long"), ("M", "Mute")]
            } else if !self.panel_buttons().is_empty() {
                let mut hints = vec![("Tab", "Choose")];
                if builtin {
                    hints.extend([("D", "Mode"), ("T", "How long"), ("M", "Mute")]);
                }
                hints.push(("Esc", "Close"));
                hints
            } else if builtin {
                vec![("D", "Mode"), ("T", "How long"), ("M", "Mute"), ("Esc", "Close")]
            } else {
                vec![("M", "Mute"), ("Esc", "Close")]
            };
            // As many as fit.
            while hints.len() > 1 && painter.key_hints(&hints, 0.0, 0.0, true) > panel.w - 36.0 {
                hints.pop();
            }
            painter.key_hints(&hints, panel.x + 18.0, panel.bottom() - 26.0, false);
        }

        for card in self.notification_cards() {
            if card.popup {
                // Slides in from the right edge.
                let age = card.notification.created.elapsed().map(|elapsed| elapsed.as_secs_f64()).unwrap_or(1.0);
                let shown = ease_out(age / 0.24);
                painter.offset = ((1.0 - shown) * (card.rect.w + 24.0), 0.0);
                painter.alpha = shown as f32;
            }
            self.draw_card(&mut painter, &card, pointer);
            painter.offset = (0.0, 0.0);
            painter.alpha = 1.0;
        }
        if let Some(area) = self.mode_switch_area() {
            self.draw_mode_menu(&mut painter, area, pointer);
            self.draw_sound_menu(&mut painter, pointer);
        }
        painter.finish()
    }

    fn draw_card(&self, painter: &mut Painter, card: &Card, pointer: Point) {
        let theme = &self.theme;
        let notification = &card.notification;
        let rect = card.rect;
        let critical =
            notification.urgency == Urgency::Critical && notification.category.as_deref() != Some(PROMPT_CATEGORY);
        if card.popup {
            painter.popup(rect);
        } else {
            // The card chosen with the keyboard has the focus ring.
            let chosen = self.notifications_keyboard()
                && self.notifications.selected == Some(Chosen::Notification(notification.id));
            let (border_width, border) = if chosen { (2.0, theme.accent) } else { (1.0, theme.border) };
            painter.shape(rect, Shape { border_width, border, ..Shape::filled(theme.surface, theme.popup_radius) });
        }
        // The accent stripe on the left edge; red for critical notifications.
        let stripe = Rect::new(rect.x + 5.0, rect.y + 14.0, 3.0, rect.h - 28.0);
        let (stripe_start, stripe_end) = if critical { (theme.danger, theme.danger) } else { theme.accent_fill() };
        painter.shape(stripe, Shape { radius: 1.5, fill: (stripe_start, stripe_end), ..Default::default() });

        // Header: app icon, app name and how long ago.
        let mut cursor_x = card.header.x;
        if let Some(icon) = self.notification_icon(notification, 16) {
            painter.image(&icon, (cursor_x, card.header.y + 2.0));
            cursor_x += 22.0;
        }
        // The popup Super+Enter would answer says so; while it has the
        // keyboard, it says how to give it back.
        let mut header_right = card.header.right();
        let answering =
            card.popup && self.notifications.answering.is_some_and(|(answered, _)| answered == notification.id);
        let shortcut = if answering {
            Some("Esc".to_string())
        } else if card.popup && self.popup_to_answer().is_some_and(|newest| newest.id == notification.id) {
            self.answer_shortcut_label()
        } else {
            None
        };
        if let Some(key) = shortcut.as_deref() {
            let hint = [(key, if answering { "Back" } else { "Choose" })];
            let width = painter.key_hints(&hint, 0.0, 0.0, true);
            header_right -= width + 8.0;
            painter.key_hints(&hint, header_right + 8.0, card.header.y + card.header.h / 2.0, false);
        }
        let app = if notification.app_name.is_empty() { "Notification" } else { &notification.app_name };
        // Hovering "Show on lock screen" says in the header what a click does.
        let on_lock_screen = self.config.file.lock.shows_in_full(&lock_apps(notification));
        let sound = self.config.file.notifications.app_sound(&lock_apps(notification)).family();
        let header = match (card.lock_toggle, card.bell) {
            (Some(toggle), _) if toggle.contains(pointer) && on_lock_screen => format!("Hide {app} on the lock screen"),
            (Some(toggle), _) if toggle.contains(pointer) => format!("Show {app} on the lock screen"),
            (_, Some(bell)) if bell.contains(pointer) => match sound {
                Some(family) => format!("{app} sounds: {}", family.name()),
                None => format!("{app} sounds: Off"),
            },
            _ => format!("{app}  \u{00b7}  {}", ago(notification.created)),
        };
        painter.text(
            &header,
            Rect::new(cursor_x, card.header.y, (header_right - cursor_x).max(0.0), card.header.h),
            Style::new(theme.font_size * 0.9, theme.muted).lines(1),
        );
        if let Some(bell) = card.bell {
            let open = self.notifications.sound_menu.as_ref().is_some_and(|menu| menu.notification == notification.id);
            let sign = if sound.is_some() { glyph::BELL } else { glyph::BELL_OFF };
            painter.button(bell, Button::glyph(sign, Look::Quiet).round(), bell.contains(pointer) || open);
        }
        if let Some(toggle) = card.lock_toggle {
            // The eye says which way it is; the button stays quiet either way.
            let eye = if on_lock_screen { glyph::EYE } else { glyph::EYE_OFF };
            painter.button(toggle, Button::glyph(eye, Look::Quiet).round(), toggle.contains(pointer));
        }
        if let Some(close) = card.close {
            let hover_close = close.contains(pointer);
            if hover_close {
                painter.fill(close, theme.surface_raised, close.w as f32 / 2.0);
            }
            painter.glyph(glyph::CLOSE, close, 14, if hover_close { theme.text } else { theme.muted });
        }

        if let (Some(preview_rect), Some((image, width, height))) =
            (card.preview, self.notification_preview(notification))
        {
            painter.image_sized(&image, (preview_rect.x, preview_rect.y), Some((width, height)));
            painter.shape(
                preview_rect,
                Shape { radius: theme.radius, border_width: 1.0, border: theme.border, ..Default::default() },
            );
        }
        if let (Some(icon_rect), Some(icon)) = (card.icon, self.notification_icon(notification, ICON)) {
            painter.image(&icon, (icon_rect.x, icon_rect.y));
        }
        painter.text(&card.summary_text, card.summary, Style::new(theme.font_size * 1.02, theme.text).bold());
        if card.body.h > 0.0 {
            painter.text(
                card.body_text.trim(),
                card.body,
                Style::new(theme.font_size * 0.95, theme.muted).lines(BODY_LINES),
            );
        }
        for (index, (action, (_, label))) in card.actions.iter().zip(&notification.actions).enumerate() {
            let hover = action.contains(pointer);
            let primary = index == 0;
            if primary {
                let alpha = if hover { 0.32 } else { 0.2 };
                painter.shape(
                    *action,
                    Shape {
                        radius: theme.radius,
                        fill: (theme.accent.alpha(alpha), theme.accent.alpha(alpha * 0.6)),
                        border_width: 1.0,
                        border: theme.accent.alpha(0.5),
                        ..Default::default()
                    },
                );
            } else {
                let fill = if hover { theme.surface_raised.mix(theme.border, 0.5) } else { theme.surface_raised };
                painter.shape(
                    *action,
                    Shape { border_width: 1.0, border: theme.border, ..Shape::filled(fill, theme.radius) },
                );
            }
            let colour = if primary {
                theme.accent
            } else if hover {
                theme.text
            } else {
                theme.text.alpha(0.85)
            };
            // With the keyboard: a ring on the chosen button, and numbers.
            let label = if answering {
                if self.notifications.answering.is_some_and(|(_, chosen)| chosen == index) {
                    let ring = Rect::new(action.x - 3.0, action.y - 3.0, action.w + 6.0, action.h + 6.0);
                    painter.shape(
                        ring,
                        Shape {
                            border_width: 2.0,
                            border: theme.accent,
                            ..Shape::filled(crate::theme::Colour::TRANSPARENT, theme.radius + 3.0)
                        },
                    );
                }
                if index < 9 { format!("{}  {label}", index + 1) } else { label.clone() }
            } else {
                label.clone()
            };
            painter.text(
                &label,
                action.inset(6.0),
                Style::new(theme.font_size * 0.92, colour).bold().align(Align::Centre),
            );
        }

        // How long the popup has left, as a thin bar along the bottom.
        if let (true, Some(expires)) = (card.popup, notification.expires) {
            let left = expires.saturating_duration_since(Instant::now()).as_secs_f64();
            let total = left + notification.created.elapsed().map(|elapsed| elapsed.as_secs_f64()).unwrap_or(0.0);
            if total > 0.0 {
                let track = Rect::new(rect.x + 16.0, rect.bottom() - 6.0, rect.w - 32.0, 2.0);
                painter.fill(track, theme.border.alpha(0.6), 1.0);
                let bar = Rect::new(track.x, track.y, track.w * (left / total).clamp(0.0, 1.0), track.h);
                painter.shape(bar, Shape { radius: 1.0, fill: (stripe_start, stripe_end), ..Default::default() });
            }
        }
    }

    /// An app icon for the notification, from its hints or the sending app.
    pub(crate) fn notification_icon(&self, notification: &Notification, size: u32) -> Option<MemoryRenderBuffer> {
        let named = |name: &str| {
            let path = name.strip_prefix("file://").unwrap_or(name);
            if path.starts_with('/') {
                self.launcher.icons.image(std::path::Path::new(path), size)
            } else {
                self.launcher.app_icon(path, size)
            }
        };
        if let Some(Image::Path(path)) = &notification.image
            && !is_file(path)
            && let Some(icon) = named(path)
        {
            return Some(icon);
        }
        // A picture file given as the icon is shown as the preview instead.
        notification
            .app_icon
            .as_deref()
            .filter(|icon| !icon.is_empty() && (!is_file(icon) || icon.ends_with(".svg")))
            .and_then(named)
            .or_else(|| notification.desktop_entry.as_deref().and_then(|entry| self.launcher.app_icon(entry, size)))
    }

    /// A picture the notification carries (a screenshot, an album cover), scaled to fit a card.
    fn notification_preview(&self, notification: &Notification) -> Option<Preview> {
        if let Some(cached) = self.notifications.thumbnails.borrow().get(&notification.id) {
            return cached.clone();
        }
        let preview = load_preview(notification);
        let mut cache = self.notifications.thumbnails.borrow_mut();
        if cache.len() >= 128 {
            cache.clear();
        }
        cache.insert(notification.id, preview.clone());
        preview
    }
}

/// A picture scaled for a card, with its width and height.
type Preview = (MemoryRenderBuffer, i32, i32);

const POPUP_W: f64 = 380.0;
const PANEL_W: f64 = 420.0;
/// App icons beside the text when there is no picture.
const ICON: u32 = 40;
const BODY_LINES: u8 = 3;
const PREVIEW_W: u32 = 348;
const PREVIEW_H: u32 = 190;

pub struct PanelControls {
    clear: Rect,
    /// Sound, brightness and playback, under the title.
    pub controls: crate::controls::Controls,
    /// Plugin blocks such as the smart home, under the controls.
    pub plugins: crate::panel::PanelLayout,
    dnd: Rect,
    cards_top: f64,
}

/// Where the parts of one notification card go.
struct Card {
    notification: Arc<Notification>,
    rect: Rect,
    header: Rect,
    /// None on the lock screen, which has no buttons for apps.
    close: Option<Rect>,
    /// "Show on lock screen", in the panel.
    lock_toggle: Option<Rect>,
    /// The app's sound, in the panel.
    bell: Option<Rect>,
    preview: Option<Rect>,
    icon: Option<Rect>,
    summary: Rect,
    summary_text: String,
    body: Rect,
    body_text: String,
    actions: Vec<Rect>,
    popup: bool,
}

impl Card {
    /// Moves the card down to start at `top`.
    fn shift(&mut self, top: f64) {
        let delta = top - self.rect.y;
        let down = |rect: &mut Rect| rect.y += delta;
        down(&mut self.rect);
        down(&mut self.header);
        self.close.iter_mut().for_each(down);
        self.lock_toggle.iter_mut().for_each(down);
        self.bell.iter_mut().for_each(down);
        down(&mut self.summary);
        down(&mut self.body);
        self.preview.iter_mut().for_each(down);
        self.icon.iter_mut().for_each(down);
        self.actions.iter_mut().for_each(down);
    }
}

/// The names an app's lock screen exception may be under: its desktop entry
/// (empty when it gave none), then its name.
fn lock_apps(notification: &Notification) -> [&str; 2] {
    [notification.desktop_entry.as_deref().unwrap_or_default(), notification.app_name.as_str()]
}

fn read_picture(path: &str) -> Option<image::RgbaImage> {
    let path = path.strip_prefix("file://").unwrap_or(path);
    let reader = image::ImageReader::open(path).ok()?.with_guessed_format().ok()?;
    Some(reader.decode().ok()?.to_rgba8())
}

fn is_file(name: &str) -> bool {
    name.starts_with('/') || name.starts_with("file://")
}

/// Pixel data or an image file, scaled to fit a card and premultiplied.
fn load_preview(notification: &Notification) -> Option<Preview> {
    let image = match &notification.image {
        Some(Image::Rgba { width, height, data }) => image::RgbaImage::from_raw(*width, *height, data.clone())?,
        Some(Image::Path(path)) if is_file(path) => read_picture(path)?,
        Some(_) => return None,
        None => {
            read_picture(notification.app_icon.as_deref().filter(|icon| is_file(icon) && !icon.ends_with(".svg"))?)?
        }
    };
    // Small pictures (avatars, covers) stay small; large ones fill the card's width.
    let (width, height) = (image.width().max(1), image.height().max(1));
    let scale = (PREVIEW_W as f64 / width as f64).min(PREVIEW_H as f64 / height as f64).min(1.0);
    let (thumb_width, thumb_height) =
        (((width as f64 * scale).round() as u32).max(1), ((height as f64 * scale).round() as u32).max(1));
    let mut scaled = image::imageops::thumbnail(&image, thumb_width, thumb_height);
    for pixel in scaled.pixels_mut() {
        let alpha = pixel.0[3] as u32;
        for channel in &mut pixel.0[..3] {
            *channel = (*channel as u32 * alpha / 255) as u8;
        }
    }
    let buffer = MemoryRenderBuffer::from_slice(
        scaled.as_raw(),
        Fourcc::Abgr8888,
        (scaled.width() as i32, scaled.height() as i32),
        1,
        Transform::Normal,
        None,
    );
    Some((buffer, scaled.width() as i32, scaled.height() as i32))
}

pub(crate) fn plain_body(markup: &str) -> String {
    let mut out = String::new();
    let mut tag = false;
    let mut quote = None;
    for character in markup.chars() {
        if tag {
            match character {
                '\'' | '"' if quote == Some(character) => quote = None,
                '\'' | '"' if quote.is_none() => quote = Some(character),
                '>' if quote.is_none() => tag = false,
                _ => {}
            }
        } else if character == '<' {
            tag = true;
        } else {
            out.push(character);
        }
    }
    html_escape::decode_html_entities(&out).into_owned()
}
