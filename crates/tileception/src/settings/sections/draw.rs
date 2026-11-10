use super::*;

impl Tileception {
    pub(super) fn section_layout(&self) -> Layout {
        // A screen covers the page area next to the sidebar exactly, so it
        // reads as a page opened from Settings rather than a window over it.
        let frame = self.settings_frame();
        let inline = self.section_is_page();
        let panel_top = if inline { frame.body.y } else { frame.panel.y };
        let panel =
            Rect::new(frame.body.x, panel_top, frame.panel.right() - frame.body.x, frame.panel.bottom() - panel_top);
        let Some(screen) = self.settings.section.as_ref() else {
            return Layout {
                panel,
                list: panel,
                body: panel,
                map: None,
                hits: Vec::new(),
                fields: Vec::new(),
                max_scroll: 0.,
            };
        };
        let compact = panel.w < 900.;
        let list_width = if compact { (panel.w * 0.28).clamp(120., 172.) } else { 248. };
        let map = (screen.kind == Kind::Displays).then(|| {
            if compact {
                Rect::new(panel.x + 24., panel.y + 122., list_width, 76.)
            } else {
                Rect::new(panel.x + 24., panel.y + 132., (panel.w - 76.) * 0.6, (panel.h - 218.).max(160.))
            }
        });
        let top = if map.is_some() { panel.y + if compact { 240. } else { 166. } } else { panel.y + 124. };
        let search_height = if screen.kind == Kind::Shortcuts { 40. } else { 0. };
        let mut list = Rect::new(
            panel.x + 24.,
            top + search_height,
            list_width,
            (panel.bottom() - top - if compact && screen.kind == Kind::Displays { 70. } else { 112. } - search_height)
                .max(1.),
        );
        let mut body = Rect::new(
            list.right() + if compact { 16. } else { 28. },
            top - 34.,
            (panel.right() - list.right() - if compact { 40. } else { 52. }).max(1.),
            (panel.bottom() - top - 52.).max(1.),
        );
        if compact && screen.kind == Kind::Displays {
            body.y = panel.y + 122.;
            body.h = (panel.bottom() - body.y - 74.).max(1.);
        }
        if let Some(map) = map.filter(|_| !compact) {
            let left = map.right() + 28.;
            let width = (panel.right() - left - 24.).max(1.);
            list = Rect::new(left, top, width, (screen.rows.len() as f64 * 52.).clamp(52., 156.));
            body = Rect::new(left, list.bottom() + 24., width, (panel.bottom() - list.bottom() - 98.).max(1.));
        }
        let mut hits = vec![
            (Rect::new(panel.right() - 268., panel.bottom() - 52., 112., 34.), Action::Cancel),
            (Rect::new(panel.right() - 144., panel.bottom() - 52., 120., 34.), Action::Stage),
        ];
        if !inline {
            hits.push((Rect::new(panel.x + 24., panel.y + 18., 80., 30.), Action::Back));
        }
        if screen.kind != Kind::Sharing {
            let button_width = (list.w - 8.) / 2.;
            hits.push((Rect::new(list.x, top - 34., button_width, 28.), Action::Add));
            if screen.selected.is_some() {
                hits.push((Rect::new(list.x + button_width + 8., top - 34., button_width, 28.), Action::Remove));
            }
            if screen.kind == Kind::Shortcuts {
                hits.push((Rect::new(list.x, top, list.w, 30.), Action::Search));
            }
            for (position, index) in screen.visible_rows().into_iter().enumerate() {
                let row = Rect::new(list.x, list.y + position as f64 * 52. - screen.list_scroll, list.w, 48.);
                if let Some(clipped) = intersect(row, list) {
                    hits.push((clipped, Action::Row(index)));
                }
            }
            if matches!(screen.kind, Kind::Startup | Kind::Rules) && screen.selected.is_some() {
                hits.push((Rect::new(list.x, list.bottom() + 8., (list.w - 8.) / 2., 28.), Action::Up));
                hits.push((
                    Rect::new(list.x + (list.w + 8.) / 2., list.bottom() + 8., (list.w - 8.) / 2., 28.),
                    Action::Down,
                ));
            }
        }
        if screen.kind == Kind::Displays {
            let second = if compact { panel.y + 90. } else { panel.y + 60. };
            hits.push((Rect::new(panel.x + 24., panel.y + 60., 236., 28.), Action::Edges));
            hits.push((Rect::new(panel.x + 268., panel.y + 60., 80., 28.), Action::Identify));
            hits.push((Rect::new(panel.x + if compact { 24. } else { 356. }, second, 116., 28.), Action::Automatic));
            hits.push((
                Rect::new(panel.x + if compact { 148. } else { 480. }, second, 100., 28.),
                Action::Touchscreens,
            ));
        }
        let specs = screen.fields(&self.section_outputs(), &self.touch.devices);
        let mut fields = Vec::new();
        let mut offset = 0.;
        for (index, field) in specs.into_iter().enumerate() {
            let rect = Rect::new(body.x, body.y + offset - screen.scroll, body.w, field_height(&field) - 6.);
            offset += field_height(&field);
            if matches!(field.form, Form::Toggle) {
                // The whole row toggles, with the switch at its right like
                // the toggles on the Settings pages.
                if rect.y >= body.y && rect.bottom() <= body.bottom() {
                    hits.push((rect, Action::Field(index)));
                }
                fields.push((rect, field));
                continue;
            }
            let input = Rect::new(rect.x, rect.y + 51., rect.w, 34.);
            let extra = field_extra(&field, screen.kind);
            if input.y >= body.y && input.bottom() <= body.bottom() {
                hits.push((Rect::new(input.x, input.y, input.w - extra, input.h), Action::Field(index)));
                if let Some(options) = segments(&field, input.w - extra) {
                    let width = (input.w - extra) / options.len() as f64;
                    for position in 0..options.len() {
                        hits.push((
                            Rect::new(input.x + position as f64 * width, input.y, width, input.h),
                            Action::Choose(index, position),
                        ));
                    }
                }
                if field.pick {
                    hits.push((
                        Rect::new(
                            input.right() - 86. - if field.removable.is_some() { 40. } else { 0. },
                            input.y,
                            86.,
                            34.,
                        ),
                        Action::Pick(index),
                    ));
                } else if screen.kind == Kind::Shortcuts && field.path == Path::Name && matches!(field.form, Form::Text)
                {
                    hits.push((Rect::new(input.right() - 86., input.y, 86., 34.), Action::Record));
                }
                if let Some(position) = field.removable {
                    hits.push((Rect::new(input.right() - 32., input.y, 32., 34.), Action::RemoveValue(position)));
                } else if matches!(field.form, Form::Secret) {
                    hits.push((Rect::new(input.right() - 152., input.y, 56., 34.), Action::Reveal));
                    hits.push((Rect::new(input.right() - 88., input.y, 88., 34.), Action::Generate));
                }
            }
            fields.push((rect, field));
        }
        let mut height = offset;
        if screen.selected.is_some()
            && matches!(screen.kind, Kind::Arguments | Kind::Shortcuts | Kind::Sharing | Kind::Displays)
        {
            let allowed = (screen.kind != Kind::Sharing || screen.advanced)
                && (screen.kind != Kind::Displays
                    || screen.row().is_some_and(|row| row.value.get("split").is_some_and(toml::Value::is_array)));
            if allowed {
                let rect = Rect::new(body.x, body.y + height - screen.scroll + 4., 160., 30.);
                if rect.y >= body.y && rect.bottom() <= body.bottom() {
                    hits.push((rect, Action::AddValue));
                }
                height += 42.;
            }
        }
        if screen.kind == Kind::Sharing {
            let rect = Rect::new(body.x, body.y + height - screen.scroll + 4., body.w, 30.);
            if rect.y >= body.y && rect.bottom() <= body.bottom() {
                hits.push((rect, Action::Advanced));
            }
            height += 42.;
        }
        Layout { panel, list, body, map, hits, fields, max_scroll: (height - body.h).max(0.) }
    }

    pub(super) fn section_menu_rect(&self, layout: &Layout) -> Rect {
        let panel = layout.panel;
        let width = (panel.w - 64.).min(640.);
        let count = self
            .settings
            .section
            .as_ref()
            .and_then(|screen| screen.menu.as_ref())
            .map_or(1, |menu| menu.filtered().len().clamp(1, 10));
        let height = (panel.h - 100.).min(52. + count as f64 * 32.);
        Rect::new(panel.x + (panel.w - width) / 2., panel.y + (panel.h - height) / 2., width, height)
    }

    pub(in crate::settings) fn section_draw(&self, painter: &mut Painter<'_>) {
        let Some(screen) = &self.settings.section else { return };
        let layout = self.section_layout();
        let theme = &self.theme;
        let inline = self.section_is_page();
        if !inline {
            painter.fill(self.settings_frame().panel, Colour([0., 0., 0., 0.5]), 10.);
        }
        painter.shape(
            layout.panel,
            Shape { border_width: 1., border: theme.border, ..Shape::filled(theme.background, 10.) },
        );
        painter.text(
            FIELDS[screen.field].label,
            Rect::new(
                layout.panel.x + if inline { 24. } else { 122. },
                layout.panel.y + 16.,
                layout.panel.w - 150.,
                36.,
            ),
            Style::new(24., theme.text).weight(500),
        );
        let hint = match screen.kind {
            Kind::Displays
                if self.settings.draft[super::edges()].as_ref().and_then(toml::Value::as_bool).unwrap_or(false) =>
            {
                "Drag a display to a free edge. Equal rectangles show full-edge crossing."
            }
            Kind::Displays => "Drag a display to a free edge of another to arrange them.",
            Kind::Sharing => "Connect computers on the same network to share input and clipboard.",
            Kind::Touch => "Choose which display receives input from each touchscreen.",
            Kind::Shortcuts => {
                "Record keys, choose gestures and assign actions. Built-in shortcuts remain available unless overridden."
            }
            Kind::Startup => "Applications and commands to start when you log in, in the order shown.",
            Kind::Arguments => "Add arguments to applications launched by Tileception. Each row is one argument.",
            Kind::Rules => "Match windows and choose how they open. Later matching rules override earlier rules.",
            Kind::Battery => {
                "Choose the focused border colour at each charge threshold. The lowest matching threshold wins."
            }
        };
        if screen.kind != Kind::Displays || layout.panel.w >= 900. {
            painter.text(
                hint,
                Rect::new(
                    layout.panel.x + 24.,
                    layout.panel.y + if screen.kind == Kind::Displays { 94. } else { 60. },
                    layout.panel.w - 48.,
                    32.,
                ),
                Style::new(12., theme.muted).lines(2),
            );
        }
        if let Some(map) = layout.map {
            self.section_draw_map(painter, map);
        }
        if screen.kind == Kind::Sharing {
            let area = Rect::new(layout.list.x, layout.list.y - 34., layout.list.w, layout.list.h + 34.);
            painter.shape(area, Shape { border_width: 1., border: theme.border, ..Shape::filled(theme.surface, 6.) });
            painter.clipped(area, |painter| self.section_draw_sharing(painter, layout.list));
            draw_scrollbar(
                painter,
                area,
                screen.list_scroll,
                (430. + self.handoff.peers.len() as f64 * 48. - layout.list.h).max(0.),
                theme,
            );
        } else {
            painter.clipped(layout.list, |painter| {
                for (position, index) in screen.visible_rows().into_iter().enumerate() {
                    let row = &screen.rows[index];
                    let rect = Rect::new(
                        layout.list.x,
                        layout.list.y + position as f64 * 52. - screen.list_scroll,
                        layout.list.w,
                        48.,
                    );
                    if index == screen.selected.unwrap_or(usize::MAX) {
                        painter.shape(
                            rect,
                            Shape {
                                border_width: 1.,
                                border: theme.accent.alpha(0.5),
                                ..Shape::filled(theme.accent.alpha(0.08), 5.)
                            },
                        );
                    } else if rect.contains(Point::new(self.settings.pointer.0, self.settings.pointer.1)) {
                        painter.fill(rect, theme.surface, 5.);
                    }
                    if screen.kind == Kind::Shortcuts {
                        // The keys, what they do and, at the right, whether
                        // they are built in.
                        let tag = if row.inherited {
                            "Built-in"
                        } else if row.value.as_str() == Some("none") {
                            "Disabled"
                        } else if row.builtin.as_ref().is_some_and(|builtin| *builtin != row.value) {
                            "Changed"
                        } else {
                            ""
                        };
                        let style = Style::new(10., theme.muted);
                        let (tag_width, _) = painter.measure(tag, 80., style);
                        painter.text(
                            tag,
                            Rect::new(rect.right() - tag_width - 10., rect.y + 4., tag_width + 1., 22.),
                            style,
                        );
                        painter.text(
                            &model::describe_combo(&screen.label(index)),
                            Rect::new(rect.x + 10., rect.y + 3., rect.w - tag_width - 28., 23.),
                            Style::new(12., theme.text).weight(500),
                        );
                        painter.text(
                            &model::describe_action(&row.value),
                            Rect::new(rect.x + 10., rect.y + 25., rect.w - 20., 20.),
                            Style::new(10., theme.muted),
                        );
                        continue;
                    }
                    painter.text(
                        &screen.label(index),
                        Rect::new(rect.x + 10., rect.y + 2., rect.w - 20., 25.),
                        Style::new(12., theme.text),
                    );
                    let status = if screen.kind == Kind::Shortcuts {
                        if row.inherited {
                            "Built-in"
                        } else if row.value.as_str() == Some("none") {
                            "Disabled"
                        } else {
                            "Custom"
                        }
                    } else if screen.kind == Kind::Displays {
                        if screen.monitors.iter().any(|monitor| monitor.name == row.name) {
                            if row.value.get("enabled").and_then(toml::Value::as_bool) == Some(false) {
                                "Disabled"
                            } else {
                                "Connected"
                            }
                        } else if row.name == "*" {
                            "Display defaults"
                        } else {
                            "Disconnected"
                        }
                    } else {
                        ""
                    };
                    painter.text(
                        status,
                        Rect::new(rect.x + 10., rect.y + 25., rect.w - 20., 20.),
                        Style::new(10., theme.muted),
                    );
                }
            });

            draw_scrollbar(
                painter,
                layout.list,
                screen.list_scroll,
                (screen.visible_rows().len() as f64 * 52. - layout.list.h).max(0.),
                theme,
            );
        }
        painter.clipped(layout.body, |painter| {
            for (index, (rect, field)) in layout.fields.iter().enumerate() {
                if rect.bottom() < layout.body.y || rect.y > layout.body.bottom() {
                    continue;
                }
                let toggle = matches!(field.form, Form::Toggle);
                let text_width = if toggle { rect.w - 64. } else { rect.w };
                painter.text(
                    &field.label,
                    Rect::new(rect.x, rect.y, text_width, 22.),
                    Style::new(13., theme.text).weight(500),
                );
                painter.text(
                    &field.hint,
                    Rect::new(rect.x, rect.y + 23., text_width, 26.),
                    Style::new(10., theme.muted).lines(2),
                );
                let extra = field_extra(field, screen.kind);
                let control = Rect::new(rect.x, rect.y + 51., rect.w - extra, 34.);
                let editing = screen
                    .edit
                    .as_ref()
                    .is_some_and(|(active, _)| active.path == field.path && active.label == field.label);
                let focused = screen.focus == Some(Action::Field(index));
                let value = screen.read(&field.path);
                if toggle {
                    let enabled = value == "true";
                    let switch = Rect::new(rect.right() - 40., rect.y + 12., 34., 20.);
                    painter.shape(
                        switch,
                        Shape {
                            radius: 10.,
                            fill: if enabled {
                                theme.accent_fill()
                            } else {
                                (theme.surface_raised, theme.surface_raised)
                            },
                            border_width: if enabled { 0. } else { 1. },
                            border: theme.border,
                            ..Default::default()
                        },
                    );
                    painter.fill(
                        Rect::new(switch.x + if enabled { 17. } else { 3. }, switch.y + 3., 14., 14.),
                        if enabled { theme.on_accent } else { theme.muted },
                        7.,
                    );
                    if focused {
                        painter.shape(
                            Rect::new(switch.x - 4., switch.y - 4., 42., 28.),
                            Shape { radius: 14., border_width: 1., border: theme.accent, ..Default::default() },
                        );
                    }
                    painter.fill(Rect::new(rect.x, rect.bottom() + 2., rect.w, 1.), theme.border.alpha(0.6), 0.);
                    continue;
                }
                if let Some(options) = segments(field, control.w) {
                    painter.shape(
                        control,
                        Shape {
                            border_width: 1.,
                            border: if focused { theme.accent } else { theme.border },
                            ..Shape::filled(theme.surface, 6.)
                        },
                    );
                    let width = control.w / options.len() as f64;
                    let pointer = Point::new(self.settings.pointer.0, self.settings.pointer.1);
                    for (position, (label, option)) in options.iter().enumerate() {
                        let segment = Rect::new(control.x + position as f64 * width, control.y, width, control.h);
                        let chosen = *option == value || value.is_empty() && option == "inherit";
                        if chosen {
                            painter.shape(
                                Rect::new(segment.x + 3., segment.y + 3., segment.w - 6., segment.h - 6.),
                                Shape {
                                    fill: (theme.accent.alpha(0.18), theme.accent.alpha(0.18)),
                                    border_width: 1.,
                                    border: theme.accent.alpha(0.6),
                                    radius: 4.,
                                    ..Default::default()
                                },
                            );
                        } else if segment.contains(pointer) {
                            painter.fill(
                                Rect::new(segment.x + 3., segment.y + 3., segment.w - 6., segment.h - 6.),
                                theme.surface_raised,
                                4.,
                            );
                        }
                        painter.text(
                            label,
                            segment,
                            Style::new(11., if chosen { theme.text } else { theme.muted }).align(Align::Centre),
                        );
                    }
                    continue;
                }
                painter.shape(
                    control,
                    Shape {
                        border_width: 1.,
                        border: if focused || editing { theme.accent } else { theme.border },
                        ..Shape::filled(theme.surface, 5.)
                    },
                );
                let secret = matches!(field.form, Form::Secret) && !screen.reveal;
                if editing {
                    let (_, input) = screen.edit.as_ref().expect("active input");
                    let mask = "*".repeat(input.text.chars().count());
                    // One mask character per character, so the caret and
                    // the selection show where they are.
                    let masked = |offset: usize| input.text[..offset].chars().count();
                    let (text, cursor, anchor) = if secret {
                        (mask.as_str(), masked(input.cursor), input.anchor.map(masked))
                    } else {
                        (input.text.as_str(), input.cursor, input.anchor)
                    };
                    if let Some(image) = self.text.borrow_mut().field(
                        text,
                        &theme.font,
                        12.,
                        theme.text,
                        (control.w - 24.) as f32,
                        crate::text::Field { cursor, anchor, visible: true, accent: theme.accent.straight_rgba8() },
                    ) {
                        painter.image(&image.buffer, (control.x + 12., control.y + (control.h - image.height) / 2.));
                    }
                } else {
                    let label = if secret {
                        "*".repeat(value.chars().count())
                    } else if screen.kind == Kind::Shortcuts && field.label == "Gesture" {
                        match &field.form {
                            Form::Choice(options) => options
                                .iter()
                                .find(|(_, option)| *option == value)
                                .map_or_else(|| "Choose a gesture...".into(), |(label, _)| label.clone()),
                            _ => value.clone(),
                        }
                    } else if field.label == "Theme colour" {
                        match &field.form {
                            Form::Choice(options) => options
                                .iter()
                                .find(|(_, option)| *option == value)
                                .map_or_else(|| "Custom".into(), |(label, _)| label.clone()),
                            _ => value.clone(),
                        }
                    } else if let Form::Choice(options) = &field.form {
                        options
                            .iter()
                            .find(|(_, option)| *option == value || value.is_empty() && option == "inherit")
                            .map_or_else(
                                || {
                                    if value.is_empty() {
                                        "Choose...".into()
                                    } else if screen.kind == Kind::Shortcuts {
                                        model::describe_action(&toml::Value::String(value.clone()))
                                    } else {
                                        value.clone()
                                    }
                                },
                                |(label, _)| label.clone(),
                            )
                    } else {
                        value.clone()
                    };
                    let left = if matches!(field.form, Form::Colour) {
                        let colour = Colour::parse(&value).ok().unwrap_or(match value.as_str() {
                            "warning" => theme.warning,
                            "danger" => theme.danger,
                            "muted" => theme.muted,
                            "text" => theme.text,
                            _ => theme.accent,
                        });
                        painter.fill(Rect::new(control.x + 10., control.y + 9., 16., 16.), colour, 3.);
                        34.
                    } else {
                        12.
                    };
                    painter.text(
                        &label,
                        Rect::new(control.x + left, control.y, control.w - left - 26., control.h),
                        Style::new(12., theme.text),
                    );
                }
                if matches!(field.form, Form::Choice(_)) {
                    painter.glyph(
                        glyph::CHEVRON_DOWN,
                        Rect::new(control.right() - 25., control.y + 8., 16., 18.),
                        14,
                        theme.muted,
                    );
                }
            }
        });
        if screen.selected.is_none() {
            let (title, text) = if screen.rows.is_empty() {
                match screen.kind {
                    Kind::Startup => {
                        ("No autostart commands", "Add an application or command to start it each time you log in.")
                    }
                    Kind::Arguments => {
                        ("No application arguments", "Add an application to pass extra arguments whenever it starts.")
                    }
                    Kind::Rules => {
                        ("No window rules", "Add a rule to open an application floating, fullscreen or at a set size.")
                    }
                    Kind::Touch => ("No touchscreen mappings", "Add a touchscreen to choose the display it controls."),
                    Kind::Battery => {
                        ("No battery colours", "Add a charge level to colour the focused border below it.")
                    }
                    _ => ("Nothing here yet", "Use Add to create the first entry."),
                }
            } else {
                ("Nothing selected", "Choose an entry from the list to edit it.")
            };
            let left = if screen.rows.is_empty() { layout.list.x } else { layout.body.x };
            let block = Rect::new(left, layout.body.y + 12., (layout.panel.right() - left - 24.).max(1.), 80.);
            painter.text(title, Rect::new(block.x, block.y, block.w, 26.), Style::new(15., theme.text).weight(500));
            painter.text(text, Rect::new(block.x, block.y + 28., block.w, 40.), Style::new(12., theme.muted).lines(2));
        }
        draw_scrollbar(painter, layout.body, screen.scroll, layout.max_scroll, theme);
        for (rect, action) in &layout.hits {
            if matches!(action, Action::Field(_) | Action::Row(_) | Action::Choose(..)) {
                continue;
            }
            if *action == Action::Search {
                painter.shape(
                    *rect,
                    Shape {
                        border_width: 1.,
                        border: if screen.searching { theme.accent } else { theme.border },
                        ..Shape::filled(theme.surface, 5.)
                    },
                );
                if screen.searching {
                    if let Some(image) = self.text.borrow_mut().field(
                        &screen.search.text,
                        &theme.font,
                        11.,
                        theme.text,
                        (rect.w - 20.) as f32,
                        crate::text::Field {
                            cursor: screen.search.cursor,
                            anchor: screen.search.anchor,
                            visible: true,
                            accent: theme.accent.straight_rgba8(),
                        },
                    ) {
                        painter.image(&image.buffer, (rect.x + 10., rect.y + (rect.h - image.height) / 2.));
                    }
                } else {
                    painter.text(
                        if screen.search.text.is_empty() { "Search shortcuts..." } else { &screen.search.text },
                        Rect::new(rect.x + 10., rect.y, rect.w - 20., rect.h),
                        Style::new(11., theme.muted),
                    );
                }
                continue;
            }
            if *action == Action::Edges {
                let enabled =
                    self.settings.draft[super::edges()].as_ref().and_then(toml::Value::as_bool).unwrap_or(false);
                painter.shape(
                    *rect,
                    Shape {
                        border_width: if screen.focus == Some(*action) { 1. } else { 0. },
                        border: theme.accent,
                        ..Shape::filled(theme.surface_raised, 5.)
                    },
                );
                painter.text(
                    "Cross anywhere on the edge",
                    Rect::new(rect.x + 12., rect.y, rect.w - 60., rect.h),
                    Style::new(11., theme.text),
                );
                let switch = Rect::new(rect.right() - 42., rect.y + 6., 30., 16.);
                painter.shape(
                    switch,
                    Shape {
                        radius: 8.,
                        fill: if enabled { theme.accent_fill() } else { (theme.surface, theme.surface) },
                        border_width: if enabled { 0. } else { 1. },
                        border: theme.border,
                        ..Default::default()
                    },
                );
                painter.fill(
                    Rect::new(switch.x + if enabled { 16. } else { 2. }, switch.y + 2., 12., 12.),
                    if enabled { theme.on_accent } else { theme.muted },
                    6.,
                );
                continue;
            }
            if *action == Action::Advanced {
                painter.shape(
                    *rect,
                    Shape {
                        border_width: if screen.focus == Some(*action) { 1. } else { 0. },
                        border: theme.accent,
                        ..Shape::filled(theme.surface_raised, 5.)
                    },
                );
                painter.glyph(
                    if screen.advanced { glyph::CHEVRON_DOWN } else { glyph::CHEVRON_RIGHT },
                    Rect::new(rect.x + 8., rect.y + 7., 16., 16.),
                    14,
                    theme.muted,
                );
                painter.text(
                    "Advanced networking",
                    Rect::new(rect.x + 32., rect.y, rect.w - 40., rect.h),
                    Style::new(11., theme.text),
                );
                continue;
            }
            let label = match action {
                Action::Add if screen.kind == Kind::Displays => "Add screen",
                Action::Add => "Add",
                Action::Remove => {
                    if screen.kind == Kind::Displays
                        && screen
                            .row()
                            .is_some_and(|row| screen.monitors.iter().any(|monitor| monitor.name == row.name))
                    {
                        "Reset display"
                    } else if screen.kind == Kind::Shortcuts {
                        if screen.row().is_some_and(|row| row.inherited) {
                            "Disable"
                        } else if screen.row().is_some_and(|row| row.builtin.is_some()) {
                            "Restore default"
                        } else {
                            "Remove"
                        }
                    } else {
                        "Remove"
                    }
                }
                Action::Up => "Move up",
                Action::Down => "Move down",
                Action::Pick(_) => "Choose...",
                Action::Record => {
                    if screen.recording {
                        "Press keys..."
                    } else {
                        "Record"
                    }
                }
                Action::RemoveValue(_) => "",
                Action::Reveal => {
                    if screen.reveal {
                        "Hide"
                    } else {
                        "Show"
                    }
                }
                Action::Generate => "Generate",
                Action::Advanced => {
                    if screen.advanced {
                        "Hide advanced networking"
                    } else {
                        "Advanced networking"
                    }
                }
                Action::Search => "Search",
                Action::AddValue => match screen.kind {
                    Kind::Sharing => "Add manual computer",
                    Kind::Displays => "Add area",
                    Kind::Shortcuts => "Add command",
                    _ => "Add argument",
                },
                Action::Identify => "Identify",
                Action::Touchscreens => "Touchscreens",
                Action::Automatic => "Automatic layout",
                Action::Cancel => "Discard changes",
                Action::Back => "Back",
                Action::Stage => "Apply changes",
                _ => "",
            };
            let primary = *action == Action::Stage;
            let focused = screen.focus == Some(*action);
            let hover = rect.contains(Point::new(self.settings.pointer.0, self.settings.pointer.1));
            let look = if primary { Look::Primary } else { Look::Plain };
            painter.button(*rect, Button::new(label, look).focused(focused), hover);
            if *action == Action::Back {
                painter.glyph(glyph::ARROW_LEFT, Rect::new(rect.x + 6., rect.y + 7., 16., 16.), 14, theme.text);
            }
            if matches!(action, Action::RemoveValue(_)) {
                painter.glyph(glyph::CLOSE, Rect::new(rect.x + 8., rect.y + 9., 16., 16.), 14, theme.text);
            }
        }
        painter.fill(Rect::new(layout.panel.x, layout.panel.bottom() - 68., layout.panel.w, 1.), theme.border, 0.);
        let status = if !screen.error.is_empty() {
            screen.error.as_str()
        } else if screen.kind == Kind::Displays && !self.settings.message.is_empty() {
            self.settings.message.as_str()
        } else if screen.kind == Kind::Displays {
            "Apply tries the changes first: keep them within 15 seconds or they go back."
        } else {
            "Back keeps your changes as a draft. Apply saves them."
        };
        painter.text(
            status,
            Rect::new(layout.panel.x + 24., layout.panel.bottom() - 62., (layout.panel.w - 320.).max(1.), 54.),
            Style::new(
                11.,
                // The settings-wide error colours its own message only, not
                // the hint shown in its place on other screens.
                if !screen.error.is_empty()
                    || screen.kind == Kind::Displays && self.settings.error && !self.settings.message.is_empty()
                {
                    theme.danger
                } else {
                    theme.muted
                },
            )
            .lines(3),
        );
        if let Some(menu) = &screen.menu {
            painter.fill(layout.panel, Colour([0., 0., 0., 0.45]), 6.);
            let rect = self.section_menu_rect(&layout);
            painter.shape(rect, Shape { border_width: 1., border: theme.border, ..Shape::filled(theme.surface, 6.) });
            if menu.picker {
                let search = Rect::new(rect.x + 8., rect.y + 7., rect.w - 16., 30.);
                painter.shape(
                    search,
                    Shape { border_width: 1., border: theme.accent, ..Shape::filled(theme.background, 4.) },
                );
                painter.glyph(glyph::SEARCH, Rect::new(search.x + 8., search.y + 7., 16., 16.), 14, theme.muted);
                if menu.filter.text.is_empty() {
                    painter.text(
                        "Search applications...",
                        Rect::new(search.x + 32., search.y, search.w - 44., search.h),
                        Style::new(12., theme.muted),
                    );
                }
                if let Some(image) = self.text.borrow_mut().field(
                    &menu.filter.text,
                    &theme.font,
                    12.,
                    theme.text,
                    (search.w - 44.) as f32,
                    crate::text::Field {
                        cursor: menu.filter.cursor,
                        anchor: menu.filter.anchor,
                        visible: true,
                        accent: theme.accent.straight_rgba8(),
                    },
                ) {
                    painter.image(&image.buffer, (search.x + 32., search.y + (search.h - image.height) / 2.));
                }
            } else {
                painter.text(
                    "Choose an option",
                    Rect::new(rect.x + 14., rect.y, rect.w - 28., 44.),
                    Style::new(13., theme.text),
                );
            }
            let visible = ((rect.h - 44.) / 32.).floor() as usize;
            let options = menu.filtered();
            for (index, (label, value)) in options.iter().enumerate().skip(menu.first).take(visible) {
                let row = Rect::new(rect.x + 6., rect.y + 44. + (index - menu.first) as f64 * 32., rect.w - 12., 32.);
                if index == menu.selected {
                    painter.fill(row, theme.accent.alpha(0.12), 4.);
                }
                let left = if menu.picker {
                    if let Some(icon) = menu.icons.get(*value).and_then(|app| self.launcher.app_icon(app, 24)) {
                        painter.image(&icon, (row.x + 8., row.y + 4.));
                    } else {
                        painter.glyph(glyph::WINDOW, Rect::new(row.x + 10., row.y + 6., 20., 20.), 18, theme.muted);
                    }
                    42.
                } else {
                    10.
                };
                painter.text(
                    label,
                    Rect::new(row.x + left, row.y, row.w - left - 10., row.h),
                    Style::new(12., theme.text),
                );
            }
            if options.is_empty() {
                painter.text(
                    "No matches",
                    Rect::new(rect.x + 16., rect.y + 48., rect.w - 32., 32.),
                    Style::new(12., theme.muted),
                );
            }
            draw_scrollbar(
                painter,
                Rect::new(rect.x, rect.y + 44., rect.w, rect.h - 44.),
                menu.first as f64 * 32.,
                (options.len().saturating_sub(visible) as f64) * 32.,
                theme,
            );
        }
    }

    fn section_draw_sharing(&self, painter: &mut Painter<'_>, area: Rect) {
        let theme = &self.theme;
        let area = Rect::new(area.x + 14., area.y, area.w - 28., area.h);
        let mut top = area.y - 20. - self.settings.section.as_ref().map_or(0., |screen| screen.list_scroll);
        painter.text("On both computers", Rect::new(area.x, top, area.w, 28.), Style::new(13., theme.text).weight(500));
        top += 36.;
        for (title, text) in [
            ("1. Turn sharing on", "Use the same passphrase on both computers."),
            ("2. Apply changes", "Use discovery, or add the other computer's address under Advanced networking."),
            ("3. Place its screen", "Drag the connection card to the matching screen edge."),
        ] {
            painter.text(title, Rect::new(area.x, top, area.w, 38.), Style::new(12., theme.text).weight(500).lines(2));
            painter.text(text, Rect::new(area.x, top + 40., area.w, 48.), Style::new(11., theme.muted).lines(3));
            top += 96.;
        }
        painter.text(
            "Connected computers",
            Rect::new(area.x, top, area.w, 30.),
            Style::new(12., theme.text).weight(500),
        );
        top += 32.;
        if self.handoff.peers.is_empty() {
            painter.text(
                if self
                    .settings
                    .section
                    .as_ref()
                    .is_some_and(|screen| screen.read(&Path::Key("discovery".into())) == "true")
                {
                    "None yet. Save matching settings on another computer."
                } else {
                    "None connected. Enable discovery or add a manual computer under Advanced networking."
                },
                Rect::new(area.x, top, area.w, 60.),
                Style::new(11., theme.muted).lines(3),
            );
        }
        for peer in self.handoff.peers.values() {
            let status = match &peer.link {
                crate::handoff::Link::Linked { paused: Some(_), .. } => "Paused",
                crate::handoff::Link::Linked { .. } => "Connected",
                crate::handoff::Link::Asking { .. } => "Waiting for placement",
                crate::handoff::Link::Dismissed => "Declined",
            };
            painter.text(&peer.name, Rect::new(area.x, top, area.w, 24.), Style::new(12., theme.text));
            painter.text(status, Rect::new(area.x, top + 24., area.w, 20.), Style::new(10., theme.muted));
            top += 48.;
        }
    }

    fn section_draw_map(&self, painter: &mut Painter<'_>, area: Rect) {
        let Some(screen) = &self.settings.section else { return };
        let theme = &self.theme;
        painter.shape(area, Shape { border_width: 1., border: theme.border, ..Shape::filled(theme.surface, 6.) });
        let equal = self.settings.draft[super::edges()].as_ref().and_then(toml::Value::as_bool).unwrap_or(false);
        let (origin, factor) = screen.display_map(area, equal);
        painter.clipped(area, |painter| {
            for (index, rect) in screen.display_map_rects(equal) {
                let rect = Rect::new(
                    origin.x + rect.x * factor + 2.,
                    origin.y + rect.y * factor + 2.,
                    (rect.w * factor - 4.).max(1.),
                    (rect.h * factor - 4.).max(1.),
                );
                let selected = screen.selected == Some(index);
                painter.shape(
                    rect,
                    Shape {
                        border_width: 1.,
                        border: if selected { theme.accent } else { theme.border },
                        ..Shape::filled(if selected { theme.accent.alpha(0.14) } else { theme.background }, 5.)
                    },
                );
                painter.text(
                    &screen.rows[index].name,
                    Rect::new(rect.x + 4., rect.y, rect.w - 8., rect.h / 2.),
                    Style::new(13., theme.text).align(Align::Centre),
                );
                let number = screen
                    .monitors
                    .iter()
                    .position(|monitor| monitor.name == screen.rows[index].name)
                    .map_or_else(|| "Offline".into(), |number| (number + 1).to_string());
                painter.text(
                    &number,
                    Rect::new(rect.x + 4., rect.y + rect.h / 2., rect.w - 8., rect.h / 2.),
                    Style::new(20., theme.accent).weight(500).align(Align::Centre),
                );
            }
        });
    }
}

pub(in crate::settings) fn draw_scrollbar(
    painter: &mut Painter<'_>,
    area: Rect,
    scroll: f64,
    maximum: f64,
    theme: &crate::theme::Theme,
) {
    if maximum <= 0. {
        return;
    }
    let height = (area.h * area.h / (area.h + maximum)).clamp(20., area.h.max(20.));
    painter.fill(
        Rect::new(area.right() + 6., area.y + (area.h - height) * (scroll / maximum).clamp(0., 1.), 3., height),
        theme.muted.alpha(0.4),
        2.,
    );
}

pub(super) fn field_extra(field: &Field, kind: Kind) -> f64 {
    let picker = field.pick || kind == Kind::Shortcuts && field.path == Path::Name && matches!(field.form, Form::Text);
    (if picker { 94. } else { 0. })
        + (if field.removable.is_some() { 40. } else { 0. })
        + (if matches!(field.form, Form::Secret) { 160. } else { 0. })
}

/// The height a field takes in a screen's form, including the gap below it.
pub(super) fn field_height(field: &Field) -> f64 {
    if matches!(field.form, Form::Toggle) { 64. } else { 104. }
}

/// The options of a short choice, shown as side-by-side buttons rather than
/// a menu.
fn segments(field: &Field, width: f64) -> Option<&Vec<(String, String)>> {
    match &field.form {
        Form::Choice(options)
            if (2..=3).contains(&options.len())
                && field.path != Path::Name
                && options.iter().all(|(label, _)| label.chars().count() <= 24)
                && options
                    .iter()
                    .all(|(label, _)| label.chars().count() as f64 * 6.5 + 20. <= width / options.len() as f64) =>
        {
            Some(options)
        }
        _ => None,
    }
}
