//! The window list for other programs (ext-foreign-toplevel-list): taskbars,
//! window switchers and scripts see each window's title and app id.

use std::collections::HashMap;

use smithay::{
    delegate_foreign_toplevel_list,
    wayland::foreign_toplevel_list::{ForeignToplevelHandle, ForeignToplevelListHandler, ForeignToplevelListState},
};
use tileception_core::WindowId;

use crate::{
    Tileception,
    state::{app_id, window_title},
};

pub struct Toplevels {
    pub state: ForeignToplevelListState,
    /// Each listed window's handle, with the title and app id last sent.
    listed: HashMap<WindowId, (ForeignToplevelHandle, String, String)>,
}

impl Toplevels {
    pub fn new(display: &smithay::reexports::wayland_server::DisplayHandle) -> Self {
        Self { state: ForeignToplevelListState::new::<Tileception>(display), listed: HashMap::new() }
    }
}

impl Tileception {
    /// Brings the list in line with the windows, once per frame: new windows
    /// appear, renamed ones update and closed ones go.
    pub fn update_toplevels(&mut self) {
        // Minimized windows are still open, so they stay listed.
        let open: std::collections::HashSet<_> =
            self.windows.keys().copied().filter(|window_id| self.output_for_window(*window_id).is_some()).collect();
        let gone: Vec<WindowId> =
            self.toplevels.listed.keys().filter(|window_id| !open.contains(window_id)).copied().collect();
        for window_id in gone {
            if let Some((handle, ..)) = self.toplevels.listed.remove(&window_id) {
                self.toplevels.state.remove_toplevel(&handle);
            }
        }
        for (window_id, window) in &self.windows {
            // Wayland windows count once they are placed, on their first commit.
            if !open.contains(window_id) {
                continue;
            }
            let (title, app) = (window_title(window), app_id(window));
            match self.toplevels.listed.get_mut(window_id) {
                Some((handle, sent_title, sent_app)) => {
                    if *sent_title == title && *sent_app == app {
                        continue;
                    }
                    if *sent_title != title {
                        handle.send_title(&title);
                    }
                    if *sent_app != app {
                        handle.send_app_id(&app);
                    }
                    handle.send_done();
                    *sent_title = title;
                    *sent_app = app;
                }
                None => {
                    let handle = self.toplevels.state.new_toplevel::<Tileception>(title.clone(), app.clone());
                    self.toplevels.listed.insert(*window_id, (handle, title, app));
                }
            }
        }
        self.toplevels.state.cleanup_closed_handles();
    }
}

impl ForeignToplevelListHandler for Tileception {
    fn foreign_toplevel_list_state(&mut self) -> &mut ForeignToplevelListState {
        &mut self.toplevels.state
    }
}

delegate_foreign_toplevel_list!(Tileception);
