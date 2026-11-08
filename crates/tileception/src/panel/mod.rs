//! Plugin blocks in the notification panel, between the sound controls and
//! Do not disturb: one per plugin that offers a `tileception://panel`
//! resource ([`block`]), in plugin priority order. Tileception lays them out
//! and draws them in its own style; folding, the view, the selected row and
//! a held level bar are kept here, so they survive the plugin's updates.

pub mod block;

use std::{
    collections::{HashMap, HashSet},
    time::Instant,
};

use block::{Block, Card, Item, Level, LevelLook, Row, Tint};
use serde_json::Value;
use tileception_core::{Point, Rect};

use crate::{
    Tileception,
    controls::level_bar,
    plugins::{ActionKind, Place},
    shapes::Shape,
    theme::Colour,
    ui::{Align, Painter, Style, glyph},
};

const ROW: f64 = 40.0;
const SLIDER: f64 = 28.0;
const HEADING: f64 = 24.0;
const HEADER: f64 = 32.0;
const CARD: f64 = 64.0;
const CHOICES: f64 = 28.0;
const FOOTER: f64 = 28.0;
const TREND: f64 = 30.0;
/// Least time between values sent while a level bar is dragged.
const SEND_INTERVAL: f64 = 1.0 / 8.0;
/// Below the blocks, before Do not disturb.
const GAP: f64 = 10.0;

/// What the panel keeps about each block between updates.
#[derive(Default)]
pub struct Panel {
    blocks: HashMap<String, BlockState>,
    pub drag: Option<Drag>,
}

#[derive(Default)]
struct BlockState {
    /// Folded or unfolded by hand, and the block's `active` count then.
    fold: Option<(bool, u32)>,
    view: usize,
    selected: Option<String>,
    /// Groups folded by hand, by title.
    groups: HashSet<String>,
}

/// A level bar held: whose, when its value was last sent and where it is.
#[derive(Clone)]
pub struct Drag {
    plugin: String,
    row: String,
    level: usize,
    sent: Instant,
    value: u32,
}

/// The blocks' layout, worked out afresh each frame.
#[derive(Default)]
pub struct PanelLayout {
    /// Zero height without blocks.
    pub rect: Rect,
    blocks: Vec<BlockLayout>,
}

struct BlockLayout {
    plugin: String,
    rect: Rect,
    header: Rect,
    folded: bool,
    /// The view switch, one tab per view, when there are several.
    tabs: Vec<Rect>,
    notice: Option<Rect>,
    cards: Vec<(Card, CardLayout)>,
    headings: Vec<(Rect, String, Option<String>, bool)>,
    rows: Vec<(Row, RowLayout)>,
    levels: Vec<LevelLayout>,
    /// The footer and a rectangle per action.
    footer: Option<(Rect, Vec<Rect>)>,
}

struct RowLayout {
    rect: Rect,
    switch: Option<Rect>,
    selected: bool,
}

struct LevelLayout {
    row: String,
    index: usize,
    level: Level,
    track: Rect,
    label: Rect,
    value: u32,
}

struct CardLayout {
    rect: Rect,
    title: Rect,
    minus: Rect,
    plus: Rect,
    switch: Rect,
    choices: Vec<Vec<Rect>>,
    trend: Option<Rect>,
}

#[derive(Clone, PartialEq)]
enum PointerAction {
    Fold(String),
    View(String, usize),
    Group(String, String),
    Select(String, String),
    Act(String, ActionKind),
    Level { plugin: String, row: String, level: usize, track: Rect },
}

impl crate::ui::PointerAction for PointerAction {
    fn activate(&self, state: &mut Tileception, _rect: Rect, point: Point) {
        match self {
            Self::Fold(plugin) => {
                let Some(block) = state.panel_block(plugin) else { return };
                let (folded, active) = (state.panel_folded(plugin, block), block.active);
                state.panel.blocks.entry(plugin.clone()).or_default().fold = Some((!folded, active));
            }
            Self::View(plugin, view) => state.panel.blocks.entry(plugin.clone()).or_default().view = *view,
            Self::Group(plugin, title) => {
                let groups = &mut state.panel.blocks.entry(plugin.clone()).or_default().groups;
                if !groups.remove(title) {
                    groups.insert(title.clone());
                }
            }
            Self::Select(plugin, row) => {
                let selected = &mut state.panel.blocks.entry(plugin.clone()).or_default().selected;
                *selected = (selected.as_ref() != Some(row)).then(|| row.clone());
            }
            Self::Act(plugin, action) => state.panel_action(plugin, action.clone()),
            Self::Level { plugin, row, level, track } => {
                let value = (((point.x - track.x) / track.w).clamp(0., 1.) * 100.).round() as u32;
                state.panel.drag =
                    Some(Drag { plugin: plugin.clone(), row: row.clone(), level: *level, sent: Instant::now(), value });
                state.panel_send_level(plugin, row, *level, value);
            }
        }
        state.schedule_render();
    }
}

/// A tint's badge fill and its icon colour; the bar's chips use these too.
pub(crate) fn tint(theme: &crate::theme::Theme, tint: Tint) -> (Colour, Colour) {
    let warm = Colour([0.95, 0.71, 0.36, 1.0]);
    match tint {
        Tint::Accent => (theme.accent.alpha(0.14), theme.accent),
        Tint::Cool => (theme.accent_end.alpha(0.14), theme.accent_end),
        Tint::Warm => (warm.alpha(0.14), warm),
        Tint::Danger => (theme.danger.alpha(0.14), theme.danger),
        Tint::Muted => (theme.surface_raised.alpha(0.6), theme.muted),
        Tint::Faded => (theme.surface_raised.alpha(0.4), theme.muted.alpha(0.6)),
    }
}

fn icon(name: Option<&str>) -> &'static str {
    name.and_then(glyph::named).unwrap_or(glyph::PLUG)
}

/// A rectangle grown by `x` and `y` on each side.
fn grown(rect: Rect, across: f64, down: f64) -> Rect {
    Rect::new(rect.x - across, rect.y - down, rect.w + across * 2.0, rect.h + down * 2.0)
}

impl Tileception {
    /// The blocks plugins offer, highest priority first.
    fn panel_blocks(&self) -> Vec<(&str, &Block)> {
        self.plugins.as_ref().map(crate::plugins::Host::panels).unwrap_or_default()
    }

    fn panel_block(&self, plugin: &str) -> Option<&Block> {
        self.panel_blocks().into_iter().find(|(plugin_id, _)| *plugin_id == plugin).map(|(_, block)| block)
    }

    fn panel_folded(&self, plugin: &str, block: &Block) -> bool {
        match self.panel.blocks.get(plugin).and_then(|state| state.fold) {
            Some((folded, active)) if active == block.active => folded,
            _ => block.active == 0,
        }
    }

    /// A plugin's block changed: forget a selection that is gone.
    pub(crate) fn panel_changed(&mut self) {
        let present: Vec<String> = self.panel_blocks().iter().map(|(plugin, _)| (*plugin).to_string()).collect();
        self.panel.blocks.retain(|plugin, _| present.contains(plugin));
        if self.panel.drag.as_ref().is_some_and(|drag| !present.contains(&drag.plugin)) {
            self.panel.drag = None;
        }
        if self.notifications.open {
            self.schedule_render();
        }
    }

    /// Folds or unfolds a plugin's block, as `tileception msg home fold`.
    pub(crate) fn panel_fold(&mut self, plugin: &str, folded: bool) -> Result<(), String> {
        let active = self.panel_block(plugin).ok_or_else(|| format!("plugin {plugin} has no panel block"))?.active;
        self.panel.blocks.entry(plugin.into()).or_default().fold = Some((folded, active));
        self.schedule_render();
        Ok(())
    }

    /// Chooses a block's view by title, case aside.
    pub(crate) fn panel_view(&mut self, plugin: &str, title: &str) -> Result<(), String> {
        let block = self.panel_block(plugin).ok_or_else(|| format!("plugin {plugin} has no panel block"))?;
        let view = block
            .views
            .iter()
            .position(|view| view.title.eq_ignore_ascii_case(title))
            .ok_or_else(|| format!("no view called {title:?}"))?;
        self.panel.blocks.entry(plugin.into()).or_default().view = view;
        self.schedule_render();
        Ok(())
    }

    /// Selects a row by id or title, showing its level bars.
    pub(crate) fn panel_select(&mut self, plugin: &str, name: &str) -> Result<(), String> {
        let block = self.panel_block(plugin).ok_or_else(|| format!("plugin {plugin} has no panel block"))?;
        let mut rows = Vec::new();
        for view in &block.views {
            collect_rows(&view.items, &mut rows);
        }
        let row = rows
            .iter()
            .find(|row| row.key == name || row.title.eq_ignore_ascii_case(name.trim()))
            .ok_or_else(|| format!("no row called {name:?}"))?;
        let key = row.key.clone();
        self.panel.blocks.entry(plugin.into()).or_default().selected = Some(key);
        self.schedule_render();
        Ok(())
    }

    /// Does what a bar chip asks, as the panel does.
    pub(crate) fn panel_chip_action(&mut self, plugin: &str, action: ActionKind) {
        self.panel_action(plugin, action);
    }

    /// Does what a block asks: tools go to its plugin.
    fn panel_action(&mut self, plugin: &str, action: ActionKind) {
        match action {
            ActionKind::Tool { name, arguments } => {
                if let Some(plugins) = &self.plugins {
                    plugins.tool(Place::Panel, plugin.into(), name, arguments);
                }
            }
            ActionKind::Settings { page } => self.open_settings_named(&page),
            ActionKind::Copy { text } => self.copy_text(text),
            ActionKind::Open { target } => self.spawn("xdg-open", &[target]),
            ActionKind::Run { command, args } => self.spawn(&command, &args),
            ActionKind::Terminal { directory } => {
                let script = format!("cd '{}' && exec {}", directory.replace('\'', "'\\''"), self.terminal);
                self.spawn("sh", &["-c".into(), script]);
            }
        }
    }

    fn panel_send_level(&mut self, plugin: &str, row: &str, level: usize, value: u32) {
        let Some(action) = self.panel_level(plugin, row, level).and_then(|level| level.action.clone()) else { return };
        let action = match action {
            ActionKind::Tool { name, mut arguments } => {
                if let Value::Object(map) = &mut arguments {
                    map.insert("value".into(), Value::from(value));
                }
                ActionKind::Tool { name, arguments }
            }
            other => other,
        };
        self.panel_action(plugin, action);
    }

    fn panel_level(&self, plugin: &str, row: &str, level: usize) -> Option<Level> {
        let block = self.panel_block(plugin)?;
        let mut rows = Vec::new();
        for view in &block.views {
            collect_rows(&view.items, &mut rows);
        }
        rows.into_iter().find(|candidate| candidate.key == row)?.levels.get(level).cloned()
    }

    /// The pointer moved while a level bar is held.
    pub fn panel_drag(&mut self, pointer_x: f64) {
        let Some(drag) = self.panel.drag.clone() else { return };
        let layout = self.panel_controls().plugins;
        let Some(level) = layout
            .blocks
            .iter()
            .filter(|block| block.plugin == drag.plugin)
            .flat_map(|block| &block.levels)
            .find(|level| level.row == drag.row && level.index == drag.level)
        else {
            return;
        };
        let value = (((pointer_x - level.track.x) / level.track.w).clamp(0.0, 1.0) * 100.0).round() as u32;
        let mut held = drag.clone();
        held.value = value;
        if drag.sent.elapsed().as_secs_f64() >= SEND_INTERVAL && value != drag.value {
            held.sent = Instant::now();
            self.panel_send_level(&drag.plugin, &drag.row, drag.level, value);
        }
        self.panel.drag = Some(held);
        self.schedule_render();
    }

    /// Lets go of a level bar, sending where it ended.
    pub fn panel_release(&mut self) -> bool {
        let Some(drag) = self.panel.drag.take() else { return false };
        self.panel_send_level(&drag.plugin, &drag.row, drag.level, drag.value);
        true
    }

    /// The blocks with their top left at (left, top).
    pub fn panel_layout(&self, left: f64, top: f64, width: f64) -> PanelLayout {
        let mut layout = PanelLayout { rect: Rect::new(left, top, width, 0.0), blocks: Vec::new() };
        let mut cursor = top;
        for (plugin, block) in self.panel_blocks() {
            let laid = self.panel_block_layout(plugin, block, left, cursor, width);
            cursor = laid.rect.bottom() + GAP;
            layout.blocks.push(laid);
        }
        if !layout.blocks.is_empty() {
            layout.rect.h = cursor - GAP - top;
        }
        layout
    }

    fn panel_block_layout(&self, plugin: &str, block: &Block, left: f64, top: f64, width: f64) -> BlockLayout {
        let state = self.panel.blocks.get(plugin);
        let inner = Rect::new(left + 10.0, top, width - 20.0, 0.0);
        let mut cursor = top + 4.0;
        let header = Rect::new(left, cursor, width, HEADER);
        cursor += HEADER;
        let mut laid = BlockLayout {
            plugin: plugin.into(),
            rect: Rect::new(left, top, width, 0.0),
            header,
            folded: true,
            tabs: Vec::new(),
            notice: None,
            cards: Vec::new(),
            headings: Vec::new(),
            rows: Vec::new(),
            levels: Vec::new(),
            footer: None,
        };
        if block.notice.is_some() {
            laid.notice = Some(Rect::new(inner.x, cursor, inner.w, 24.0));
            cursor += 30.0;
            laid.rect.h = cursor - top;
            return laid;
        }
        laid.folded = self.panel_folded(plugin, block);
        if laid.folded {
            laid.rect.h = cursor + 4.0 - top;
            return laid;
        }
        if block.views.len() > 1 {
            let tab_w = 52.0;
            let tabs_w = tab_w * block.views.len() as f64;
            let start = header.right() - 40.0 - tabs_w;
            laid.tabs = (0..block.views.len())
                .map(|index| Rect::new(start + tab_w * index as f64, header.y + 4.0, tab_w, HEADER - 8.0))
                .collect();
        }
        let view = state.map_or(0, |state| state.view).min(block.views.len().saturating_sub(1));
        if let Some(view) = block.views.get(view) {
            for item in &view.items {
                self.panel_item_layout(plugin, item, inner, &mut cursor, &mut laid);
            }
        }
        if let Some(footer) = &block.footer {
            let rect = Rect::new(inner.x, cursor, inner.w, FOOTER);
            let mut right = rect.right();
            let mut actions: Vec<Rect> = footer
                .actions
                .iter()
                .map(|_| {
                    right -= 72.0;
                    Rect::new(right, rect.y + 2.0, 72.0, rect.h - 4.0)
                })
                .collect();
            actions.reverse();
            laid.footer = Some((rect, actions));
            cursor = rect.bottom() + 6.0;
        }
        laid.rect.h = cursor - top;
        laid
    }

    fn panel_item_layout(&self, plugin: &str, item: &Item, inner: Rect, cursor: &mut f64, laid: &mut BlockLayout) {
        let state = self.panel.blocks.get(plugin);
        match item {
            Item::Group(group) => {
                let folded = state.is_some_and(|state| state.groups.contains(&group.title));
                laid.headings.push((
                    Rect::new(inner.x + 2.0, *cursor, inner.w - 4.0, HEADING),
                    group.title.clone(),
                    group.detail.clone(),
                    folded,
                ));
                *cursor += HEADING;
                if !folded {
                    for item in &group.items {
                        self.panel_item_layout(plugin, item, inner, cursor, laid);
                    }
                }
                *cursor += 4.0;
            }
            Item::Row(row) => {
                let rect = Rect::new(inner.x, *cursor, inner.w, ROW);
                let switch = row
                    .switch
                    .is_some()
                    .then(|| Rect::new(rect.right() - 46.0, rect.y + (ROW - 22.0) / 2.0, 38.0, 22.0));
                let selected = row.enabled
                    && !row.levels.is_empty()
                    && state.and_then(|state| state.selected.as_ref()) == Some(&row.key);
                laid.rows.push((row.clone(), RowLayout { rect, switch, selected }));
                *cursor += ROW;
                if selected {
                    for (index, level) in row.levels.iter().enumerate() {
                        let value = match &self.panel.drag {
                            Some(drag) if drag.plugin == plugin && drag.row == row.key && drag.level == index => {
                                drag.value
                            }
                            _ => level.value,
                        };
                        laid.levels.push(LevelLayout {
                            row: row.key.clone(),
                            index,
                            level: level.clone(),
                            track: Rect::new(inner.x + 118.0, *cursor, inner.w - 118.0 - 58.0, SLIDER),
                            label: Rect::new(inner.right() - 54.0, *cursor, 48.0, SLIDER),
                            value,
                        });
                        *cursor += SLIDER;
                    }
                    *cursor += 4.0;
                }
            }
            Item::Card(card) => {
                let rect_top = *cursor + 2.0;
                let mut inside = rect_top + 8.0;
                let main = Rect::new(inner.x, inside, inner.w, CARD - 16.0);
                let switch = Rect::new(main.right() - 46.0, main.y + (main.h - 22.0) / 2.0, 38.0, 22.0);
                let plus = Rect::new(switch.x - 40.0, main.y + (main.h - 28.0) / 2.0, 28.0, 28.0);
                let minus = Rect::new(plus.x - 78.0, plus.y, 28.0, 28.0);
                let right = if card.stepper.is_some() {
                    minus.x
                } else if card.switch.is_some() {
                    switch.x
                } else {
                    main.right()
                };
                let title = Rect::new(main.x + 124.0, main.y + 4.0, (right - main.x - 128.0).max(20.0), 38.0);
                inside = main.bottom() + 4.0;
                let mut choices = Vec::new();
                for row in &card.choices {
                    if row.options.is_empty() {
                        choices.push(Vec::new());
                        continue;
                    }
                    let strip = Rect::new(inner.x + 8.0, inside, inner.w - 16.0, CHOICES - 4.0);
                    let label_w = 44.0;
                    let each = (strip.w - label_w) / row.options.len() as f64;
                    choices.push(
                        (0..row.options.len())
                            .map(|index| Rect::new(strip.x + label_w + each * index as f64, strip.y, each, strip.h))
                            .collect(),
                    );
                    inside += CHOICES;
                }
                let trend =
                    card.trend.as_ref().filter(|trend| trend.lines.iter().any(|line| line.points.len() >= 2)).map(
                        |_| {
                            let rect = Rect::new(inner.x + 8.0, inside, inner.w - 16.0, TREND);
                            inside += TREND + 2.0;
                            rect
                        },
                    );
                let rect = Rect::new(inner.x, rect_top, inner.w, inside + 4.0 - rect_top);
                *cursor = rect.bottom() + 6.0;
                laid.cards.push(((**card).clone(), CardLayout { rect, title, minus, plus, switch, choices, trend }));
            }
        }
    }

    pub(crate) fn panel_pointer_target(&self, layout: &PanelLayout, point: Point) -> Option<crate::ui::PointerTarget> {
        use crate::ui::PointerTarget;
        let laid = layout.blocks.iter().find(|block| block.rect.contains(point))?;
        let block = self.panel_block(&laid.plugin)?;
        let plugin = laid.plugin.clone();
        let act = |rect: Rect, action: &Option<ActionKind>| {
            action.clone().map(|action| PointerTarget::button(rect, PointerAction::Act(plugin.clone(), action)))
        };
        if let Some(notice) = &block.notice {
            let area = Rect::new(laid.rect.x, laid.header.y, laid.rect.w, laid.rect.bottom() - laid.header.y);
            return act(area, &notice.action);
        }
        for (index, tab) in laid.tabs.iter().enumerate() {
            if tab.contains(point) {
                return Some(PointerTarget::button(*tab, PointerAction::View(plugin, index)));
            }
        }
        if laid.header.contains(point) {
            return Some(PointerTarget::button(laid.header, PointerAction::Fold(plugin)));
        }
        if let (Some((_, rects)), Some(footer)) = (&laid.footer, &block.footer) {
            for (rect, action) in rects.iter().zip(&footer.actions) {
                if rect.contains(point) {
                    return if action.enabled { act(*rect, &action.action) } else { None };
                }
            }
        }
        for (card, card_layout) in &laid.cards {
            if card_layout.rect.contains(point) {
                return self.panel_card_pointer_target(&plugin, card, card_layout, point);
            }
        }
        for (rect, title, _, _) in &laid.headings {
            if rect.contains(point) {
                return Some(PointerTarget::button(*rect, PointerAction::Group(plugin, title.clone())));
            }
        }
        for level in &laid.levels {
            let rect = grown(level.track, 8.0, 0.0);
            if rect.contains(point) {
                return Some(PointerTarget::new(
                    rect,
                    PointerAction::Level { plugin, row: level.row.clone(), level: level.index, track: level.track },
                    smithay::input::pointer::CursorIcon::EwResize,
                ));
            }
        }
        for (row, row_layout) in &laid.rows {
            if !row.enabled || !row_layout.rect.contains(point) {
                continue;
            }
            if let (Some(switch), Some(action)) =
                (row_layout.switch, row.switch.as_ref().and_then(|switch| switch.action.as_ref()))
            {
                let area = Rect::new(switch.x - 8., row_layout.rect.y, switch.w + 16., row_layout.rect.h);
                if area.contains(point) {
                    return Some(PointerTarget::button(area, PointerAction::Act(plugin, action.clone())));
                }
            }
            if !row.levels.is_empty() {
                return Some(PointerTarget::button(row_layout.rect, PointerAction::Select(plugin, row.key.clone())));
            }
        }
        None
    }

    fn panel_card_pointer_target(
        &self,
        plugin: &str,
        card: &Card,
        laid: &CardLayout,
        point: Point,
    ) -> Option<crate::ui::PointerTarget> {
        let target = |rect: Rect, action: Option<&ActionKind>| {
            action
                .map(|action| crate::ui::PointerTarget::button(rect, PointerAction::Act(plugin.into(), action.clone())))
        };
        if laid.title.contains(point) {
            return target(laid.title, card.title_action.as_ref());
        }
        if !card.enabled {
            return None;
        }
        if let Some(switch) = &card.switch {
            let area = grown(laid.switch, 6.0, 8.0);
            if area.contains(point) {
                return target(area, switch.action.as_ref());
            }
        }
        if let Some(stepper) = &card.stepper {
            for (rect, action) in [(laid.minus, &stepper.minus), (laid.plus, &stepper.plus)] {
                if rect.contains(point) {
                    return target(rect, action.as_ref());
                }
            }
        }
        for (row, rects) in card.choices.iter().zip(&laid.choices) {
            for (choice, rect) in row.options.iter().zip(rects) {
                if rect.contains(point) {
                    return target(*rect, choice.action.as_ref());
                }
            }
        }
        None
    }

    pub fn draw_panel_blocks(&self, painter: &mut Painter, layout: &PanelLayout, pointer: Point) {
        for laid in &layout.blocks {
            if let Some(block) = self.panel_block(&laid.plugin) {
                self.panel_draw_block(painter, block, laid, pointer);
            }
        }
    }

    fn panel_draw_block(&self, painter: &mut Painter, block: &Block, laid: &BlockLayout, pointer: Point) {
        let theme = &self.theme;
        painter.shape(
            laid.rect,
            Shape { border_width: 1.0, border: theme.border, ..Shape::filled(theme.surface, theme.radius) },
        );
        let header = laid.header;
        if block.notice.is_none() && header.contains(pointer) && !laid.tabs.iter().any(|tab| tab.contains(pointer)) {
            painter.fill(header, theme.surface_raised, theme.radius);
        }
        painter.glyph(
            icon(block.icon.as_deref()),
            Rect::new(header.x + 8.0, header.y, 24.0, header.h),
            16,
            theme.accent,
        );
        let title = Style::new(theme.font_size, theme.text).bold();
        let (title_w, _) = painter.text(&block.title, Rect::new(header.x + 38.0, header.y, 160.0, header.h), title);
        let summary = if laid.folded { &block.summary } else { block.summary_open.as_ref().unwrap_or(&block.summary) };
        let tabs_w = laid.tabs.len() as f64 * 52.0;
        let summary_w = header.w - title_w - 80.0 - tabs_w;
        painter.text(
            summary,
            Rect::new(header.x + 46.0 + title_w, header.y, summary_w.max(10.0), header.h),
            Style::new(theme.font_size * 0.88, theme.muted),
        );
        if block.notice.is_none() {
            let chevron = if laid.folded { glyph::CHEVRON_RIGHT } else { glyph::CHEVRON_DOWN };
            painter.glyph(chevron, Rect::new(header.right() - 34.0, header.y, 28.0, header.h), 13, theme.muted);
        }
        if let (Some(first), Some(last)) = (laid.tabs.first(), laid.tabs.last()) {
            painter.shape(
                Rect::new(first.x, first.y, last.right() - first.x, first.h),
                Shape { border_width: 1.0, border: theme.border, ..Shape::filled(theme.background.alpha(0.4), 6.0) },
            );
            let chosen = self.panel.blocks.get(&laid.plugin).map_or(0, |state| state.view).min(laid.tabs.len() - 1);
            for (index, (rect, view)) in laid.tabs.iter().zip(&block.views).enumerate() {
                if index == chosen {
                    painter.fill(
                        Rect::new(rect.x + 2.0, rect.y + 2.0, rect.w - 4.0, rect.h - 4.0),
                        theme.surface_raised,
                        4.0,
                    );
                    painter.fill(Rect::new(rect.x + 8.0, rect.bottom() - 4.0, rect.w - 16.0, 2.0), theme.accent, 1.0);
                }
                let colour = if index == chosen { theme.text } else { theme.muted };
                painter.text(&view.title, *rect, Style::new(theme.font_size * 0.82, colour).align(Align::Centre));
            }
        }
        if let (Some(rect), Some(notice)) = (laid.notice, &block.notice) {
            painter.text(&notice.text, rect, Style::new(theme.font_size * 0.9, theme.muted));
            return;
        }
        for (card, card_layout) in &laid.cards {
            self.panel_draw_card(painter, card, card_layout, pointer);
        }
        for (rect, title, detail, folded) in &laid.headings {
            let style = Style::new(theme.font_size * 0.78, theme.muted).bold();
            let (width, _) = painter.text(&title.to_uppercase(), *rect, style);
            if *folded {
                painter.glyph(
                    glyph::CHEVRON_RIGHT,
                    Rect::new(rect.x + width + 4.0, rect.y, 14.0, rect.h),
                    10,
                    theme.muted,
                );
            }
            if let Some(detail) = detail {
                let style = Style::new(theme.font_size * 0.78, theme.muted);
                let (width, _) = painter.measure(detail, 120.0, style);
                painter.text(detail, Rect::new(rect.right() - width - 4.0, rect.y, width + 4.0, rect.h), style);
            }
        }
        for (row, row_layout) in &laid.rows {
            self.panel_draw_row(painter, row, row_layout, pointer);
        }
        for level in &laid.levels {
            self.panel_draw_level(painter, &laid.plugin, level, pointer);
        }
        if let (Some((rect, actions)), Some(footer)) = (&laid.footer, &block.footer) {
            let dot = Rect::new(rect.x + 6.0, rect.y + rect.h / 2.0 - 3.0, 6.0, 6.0);
            painter.fill(dot, if footer.good { theme.accent } else { theme.muted }, 3.0);
            let text_w = rect.w - 24.0 - actions.len() as f64 * 72.0;
            painter.text(
                &footer.text,
                Rect::new(rect.x + 18.0, rect.y, text_w.max(10.0), rect.h),
                Style::new(theme.font_size * 0.8, theme.muted),
            );
            for (button, action) in actions.iter().zip(&footer.actions) {
                let hover = action.enabled && button.contains(pointer);
                if hover {
                    painter.fill(*button, theme.surface_raised, theme.radius);
                }
                let colour = match () {
                    _ if !action.enabled => theme.muted.alpha(0.5),
                    _ if hover => theme.text,
                    _ => theme.muted,
                };
                painter.text(&action.title, *button, Style::new(theme.font_size * 0.82, colour).align(Align::Centre));
            }
        }
    }

    fn panel_draw_row(&self, painter: &mut Painter, row: &Row, laid: &RowLayout, pointer: Point) {
        let theme = &self.theme;
        if laid.selected {
            painter.fill(laid.rect, theme.accent.alpha(0.12), theme.radius);
        } else if row.enabled && laid.rect.contains(pointer) && (row.switch.is_some() || !row.levels.is_empty()) {
            painter.fill(laid.rect, theme.surface_raised, theme.radius);
        }
        let badge = Rect::new(laid.rect.x + 6.0, laid.rect.y + 6.0, 28.0, 28.0);
        let (fill, colour) = tint(theme, row.tint);
        painter.fill(badge, fill, 6.0);
        painter.glyph(icon(row.icon.as_deref()), badge, 15, colour);
        let text_x = badge.right() + 10.0;
        let text_w = laid.switch.map_or(laid.rect.right(), |switch| switch.x) - text_x - 8.0;
        let name_colour = if row.dim { theme.text.alpha(0.72) } else { theme.text };
        painter.text(
            &row.title,
            Rect::new(text_x, laid.rect.y + 3.0, text_w, 18.0),
            Style::new(theme.font_size * 0.92, name_colour),
        );
        let detail_colour = if row.alert { theme.danger.alpha(0.85) } else { theme.muted };
        painter.text(
            &row.detail,
            Rect::new(text_x, laid.rect.y + 20.0, text_w, 16.0),
            Style::new(theme.font_size * 0.78, detail_colour),
        );
        if let (Some(rect), Some(switch)) = (laid.switch, &row.switch) {
            draw_switch(painter, rect, switch.checked, switch.pending, row.enabled);
        }
    }

    fn panel_draw_level(&self, painter: &mut Painter, plugin: &str, level: &LevelLayout, pointer: Point) {
        let theme = &self.theme;
        let track = level.track;
        painter.text(
            &level.level.title,
            Rect::new(track.x - 74.0, track.y, 72.0, track.h),
            Style::new(theme.font_size * 0.82, theme.muted),
        );
        let held = self
            .panel
            .drag
            .as_ref()
            .is_some_and(|drag| drag.plugin == plugin && drag.row == level.row && drag.level == level.index);
        let active = held || grown(track, 8.0, 0.0).contains(pointer);
        match level.level.look {
            LevelLook::Warmth => {
                let bar = Rect::new(track.x, track.y + track.h / 2.0 - 2.0, track.w, 4.0);
                painter.shape(
                    bar,
                    Shape {
                        radius: 2.0,
                        fill: (Colour([1.0, 0.70, 0.42, 1.0]), Colour([0.81, 0.89, 1.0, 1.0])),
                        ..Default::default()
                    },
                );
                let centre = bar.x + bar.w * f64::from(level.value.min(100)) / 100.0;
                let radius = if active { 8.0 } else { 6.0 };
                painter.fill(
                    Rect::new(centre - radius, bar.y + 2.0 - radius, radius * 2.0, radius * 2.0),
                    theme.text,
                    radius as f32,
                );
            }
            LevelLook::Plain => level_bar(painter, track, level.value, 100, false, Some(active)),
        }
        painter.text(
            &level.level.label(level.value),
            level.label,
            Style::new(theme.font_size * 0.82, theme.text).align(Align::Centre),
        );
    }

    fn panel_draw_card(&self, painter: &mut Painter, card: &Card, laid: &CardLayout, pointer: Point) {
        let theme = &self.theme;
        painter.shape(
            laid.rect,
            Shape {
                border_width: 1.0,
                border: theme.border,
                ..Shape::filled(theme.surface_raised.alpha(0.5), theme.radius)
            },
        );
        let main = Rect::new(laid.rect.x, laid.title.y - 4.0, laid.rect.w, CARD - 16.0);
        let (_, colour) = tint(theme, card.tint);
        painter.glyph(icon(card.icon.as_deref()), Rect::new(main.x + 6.0, main.y, 28.0, main.h), 20, colour);
        painter.text(
            &card.value,
            Rect::new(main.x + 38.0, main.y + 2.0, 84.0, 30.0),
            Style::new(theme.font_size * 1.8, theme.text).weight(300),
        );
        let hover_title = card.title_action.is_some() && laid.title.contains(pointer);
        let title_colour = if hover_title { theme.text } else { theme.text.alpha(0.85) };
        painter.text(
            &card.title,
            Rect::new(laid.title.x, main.y + 4.0, laid.title.w, 18.0),
            Style::new(theme.font_size * 0.88, title_colour),
        );
        if let Some(detail) = &card.detail {
            painter.text(
                detail,
                Rect::new(laid.title.x, main.y + 22.0, laid.title.w, 20.0),
                Style::new(theme.font_size * 0.8, theme.muted),
            );
        }
        if let Some(stepper) = &card.stepper {
            for (rect, sign, action) in
                [(laid.minus, glyph::MINUS, &stepper.minus), (laid.plus, glyph::PLUS, &stepper.plus)]
            {
                let usable = card.enabled && action.is_some();
                let hover = usable && rect.contains(pointer);
                painter.shape(
                    rect,
                    Shape {
                        border_width: 1.0,
                        border: theme.border,
                        ..Shape::filled(if hover { theme.surface_raised } else { theme.surface }, 6.0)
                    },
                );
                painter.glyph(sign, rect, 13, if usable { theme.text } else { theme.muted });
            }
            let between = Rect::new(laid.minus.right(), laid.minus.y, laid.plus.x - laid.minus.right(), laid.minus.h);
            painter.text(
                &stepper.value,
                between,
                Style::new(theme.font_size, if card.lit { theme.accent } else { theme.muted })
                    .bold()
                    .align(Align::Centre),
            );
        }
        if let Some(switch) = &card.switch {
            draw_switch(painter, laid.switch, switch.checked, switch.pending, card.enabled);
        }
        for (row, rects) in card.choices.iter().zip(&laid.choices) {
            let (Some(first), Some(last)) = (rects.first(), rects.last()) else { continue };
            painter.text(
                &row.title,
                Rect::new(first.x - 44.0, first.y, 42.0, first.h),
                Style::new(theme.font_size * 0.78, theme.muted),
            );
            painter.shape(
                Rect::new(first.x, first.y, last.right() - first.x, first.h),
                Shape { border_width: 1.0, border: theme.border, ..Shape::filled(theme.background.alpha(0.4), 6.0) },
            );
            for (choice, rect) in row.options.iter().zip(rects) {
                let inset = Rect::new(rect.x + 2.0, rect.y + 2.0, rect.w - 4.0, rect.h - 4.0);
                if choice.selected {
                    painter.fill(inset, theme.surface_raised, 4.0);
                    painter.fill(Rect::new(rect.x + 6.0, rect.bottom() - 4.0, rect.w - 12.0, 2.0), theme.accent, 1.0);
                } else if card.enabled && rect.contains(pointer) {
                    painter.fill(inset, theme.surface_raised.alpha(0.5), 4.0);
                }
                let colour = if choice.selected && card.lit { theme.text } else { theme.muted };
                painter.text(&choice.title, *rect, Style::new(theme.font_size * 0.76, colour).align(Align::Centre));
            }
        }
        if let (Some(rect), Some(trend)) = (laid.trend, &card.trend) {
            draw_trend(painter, rect, trend);
        }
    }
}

/// Every row in some items, groups opened.
fn collect_rows<'a>(items: &'a [Item], rows: &mut Vec<&'a Row>) {
    for item in items {
        match item {
            Item::Group(group) => collect_rows(&group.items, rows),
            Item::Row(row) => rows.push(row),
            Item::Card(_) => {}
        }
    }
}

/// Lines over the trend's span, newest at the right: dashed ones thin and
/// muted, solid ones in the accent. Each value holds until the next point.
fn draw_trend(painter: &mut Painter, rect: Rect, trend: &block::Trend) {
    let theme = painter.theme().clone();
    let values = || trend.lines.iter().flat_map(|line| line.points.iter().map(|(_, value)| *value));
    let mut min = values().fold(f64::INFINITY, f64::min);
    let mut max = values().fold(f64::NEG_INFINITY, f64::max);
    if !min.is_finite() || !max.is_finite() {
        return;
    }
    if max - min < 1.0 {
        let middle = (min + max) / 2.0;
        min = middle - 0.5;
        max = middle + 0.5;
    }
    let span = trend.span.max(1.0);
    let place = |(age, value): (f64, f64)| {
        let pos_x = rect.right() - age.clamp(0.0, span) / span * rect.w;
        let pos_y = rect.bottom() - 2.0 - (value - min) / (max - min) * (rect.h - 4.0);
        (pos_x.clamp(rect.x, rect.right()), pos_y.clamp(rect.y, rect.bottom()))
    };
    for line in &trend.lines {
        let points: Vec<(f64, f64)> = line.points.iter().copied().map(place).collect();
        let (colour, thickness) = if line.dashed { (theme.muted.alpha(0.9), 1.0) } else { (theme.accent, 2.0) };
        for (index, &(left, value)) in points.iter().enumerate() {
            let next = points.get(index + 1).copied();
            let right = next.map_or(rect.right(), |(next_x, _)| next_x);
            if line.dashed {
                // Anchor the dash phase to the chart, independent of samples.
                let mut dash = rect.x + ((left - rect.x) / 8.0).floor() * 8.0;
                while dash < right {
                    let start = dash.max(left);
                    let end = (dash + 4.0).min(right);
                    if end > start {
                        painter.fill(Rect::new(start, value - thickness / 2.0, end - start, thickness), colour, 0.0);
                    }
                    dash += 8.0;
                }
            } else if right > left {
                painter.fill(Rect::new(left, value - thickness / 2.0, right - left, thickness), colour, 0.0);
            }
            if let Some((next_x, next_value)) = next {
                painter.fill(
                    Rect::new(
                        next_x - thickness / 2.0,
                        value.min(next_value) - thickness / 2.0,
                        thickness,
                        (next_value - value).abs() + thickness,
                    ),
                    colour,
                    0.0,
                );
            }
        }
    }
}

pub(crate) fn draw_switch(painter: &mut Painter, track: Rect, lit: bool, pending: bool, usable: bool) {
    let theme = painter.theme();
    let radius = (track.h / 2.0) as f32;
    let alpha = if usable { 1.0 } else { 0.4 };
    if lit {
        painter.shape(
            track,
            Shape {
                radius,
                fill: (theme.accent.alpha(alpha), theme.accent_end.alpha(alpha)),
                glow_radius: if pending || !usable { 0.0 } else { 8.0 },
                glow: theme.accent.alpha(0.25),
                ..Default::default()
            },
        );
    } else {
        painter.shape(
            track,
            Shape {
                border_width: 1.0,
                border: theme.border.alpha(alpha),
                ..Shape::filled(theme.background.alpha(alpha), radius)
            },
        );
    }
    let knob = track.h - 6.0;
    let knob_x = if lit { track.right() - knob - 3.0 } else { track.x + 3.0 };
    let colour = if pending || !lit { theme.muted } else { theme.on_accent };
    painter.fill(Rect::new(knob_x, track.y + 3.0, knob, knob), colour.alpha(alpha), (knob / 2.0) as f32);
}
