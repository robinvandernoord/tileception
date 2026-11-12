//! The panel block format: what a plugin says it shows in the notification
//! panel, read from its `tileception://panel` resource. Plugins only
//! describe; Tileception lays the block out and draws it in its own style.
//!
//! A block has a header (icon, title and a summary), an `active` count that
//! unfolds it when something turns on, and either a notice or one or more
//! views of items: folding groups, rows with a switch and level bars, and
//! cards with a stepper, choices and a trend line. Actions are the
//! launcher's action types; a level's action gets the chosen `value` added
//! to its arguments.
//!
//! The bar's chips ([`Bar`]) use the same icons, tints and actions, and the
//! cards under the launcher ([`Launcher`]) are the block's cards.

use serde::{Deserialize, Serialize};

use crate::plugins::ActionKind;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Block {
    pub title: String,
    pub icon: Option<String>,
    /// Beside the title while folded.
    pub summary: String,
    /// Beside the title while unfolded; the folded summary when missing.
    pub summary_open: Option<String>,
    /// How many things are on. The block starts folded while this is 0 and
    /// unfolds when it changes; folding by hand holds until it does.
    pub active: u32,
    /// Instead of items: setup needed, or why there is nothing to show.
    pub notice: Option<Notice>,
    /// Ways to list the items, such as by room and all at once; a switch
    /// in the header chooses between several.
    pub views: Vec<View>,
    pub footer: Option<Footer>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Notice {
    pub text: String,
    /// What clicking the block does.
    pub action: Option<ActionKind>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct View {
    pub title: String,
    pub items: Vec<Item>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Item {
    Group(Group),
    Row(Row),
    Card(Box<Card>),
}

/// A heading over its items, such as a room; clicking it folds them.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Group {
    pub title: String,
    /// At the heading's right, such as the room's temperature.
    pub detail: Option<String>,
    pub items: Vec<Item>,
}

/// One thing: an icon, a name over a detail line, an optional switch, and
/// level bars that show while the row is selected.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Row {
    /// `id`: unique within the block; keeps the selection across updates.
    #[serde(rename = "id")]
    pub key: String,
    pub title: String,
    pub detail: String,
    pub icon: Option<String>,
    pub tint: Tint,
    /// The title is drawn dimmer, as for something switched off.
    pub dim: bool,
    /// The detail is a problem, drawn in the danger colour.
    pub alert: bool,
    /// Whether it can be used now; a row that cannot is drawn faded.
    pub enabled: bool,
    pub switch: Option<Switch>,
    pub levels: Vec<Level>,
    /// A small trend line at the right, for a row without a switch, such as
    /// a sensor's last hours.
    pub trend: Option<Trend>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Switch {
    /// `on`.
    #[serde(rename = "on")]
    pub checked: bool,
    /// A change was sent and not yet confirmed.
    pub pending: bool,
    pub action: Option<ActionKind>,
}

/// A draggable bar from 0 to 100.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Level {
    pub title: String,
    pub value: u32,
    pub look: LevelLook,
    /// After the number, such as `%`.
    pub unit: String,
    /// Words instead of the number, spread evenly over the range, such as
    /// Warm, Neutral and Cool.
    pub steps: Vec<String>,
    /// Sent while dragging and on release, with `value` in its arguments.
    pub action: Option<ActionKind>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LevelLook {
    #[default]
    Plain,
    /// Warm to cool colours under the knob.
    Warmth,
}

impl Level {
    /// What the bar says for a value.
    pub fn label(&self, value: u32) -> String {
        if self.steps.is_empty() {
            return format!("{value}{}", self.unit);
        }
        let index = (value.min(100) as usize * self.steps.len() / 100).min(self.steps.len() - 1);
        self.steps[index].clone()
    }
}

/// A larger tile, such as a thermostat or a timer: a big reading, a title
/// and detail, buttons, a stepper, a switch, rows of choices, a trend line
/// and a progress bar.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Card {
    /// `id`.
    #[serde(rename = "id")]
    pub key: String,
    pub title: String,
    pub detail: Option<String>,
    /// The big reading, such as the room temperature.
    pub value: String,
    pub icon: Option<String>,
    pub tint: Tint,
    /// Whether it is on: the stepper's value and chosen options stand out.
    pub lit: bool,
    pub enabled: bool,
    /// Clicking the title, such as showing the next thermostat.
    pub title_action: Option<ActionKind>,
    /// Beside the title, before the stepper, such as pause and stop.
    pub buttons: Vec<Button>,
    pub stepper: Option<Stepper>,
    pub switch: Option<Switch>,
    pub choices: Vec<Choices>,
    pub trend: Option<Trend>,
    /// A thin bar along the bottom, such as how far a timer is.
    pub progress: Option<Progress>,
}

/// A button on a card: its icon, or its title when it has none.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Button {
    pub title: String,
    pub icon: Option<String>,
    /// The card's main button, filled with the accent.
    pub primary: bool,
    pub enabled: bool,
    pub action: Option<ActionKind>,
}

/// How far something is, from 0 to 100.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Progress {
    pub value: f64,
}

impl Progress {
    /// The share done, from 0 to 1.
    pub fn share(self) -> f64 {
        if self.value.is_finite() { (self.value / 100.0).clamp(0.0, 1.0) } else { 0.0 }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Stepper {
    pub value: String,
    pub minus: Option<ActionKind>,
    pub plus: Option<ActionKind>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Choices {
    pub title: String,
    pub options: Vec<Choice>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Choice {
    pub title: String,
    pub selected: bool,
    pub action: Option<ActionKind>,
}

/// Lines over the last `span` seconds, newest at the right.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Trend {
    pub span: f64,
    pub lines: Vec<Line>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Line {
    /// Thin and dashed, such as a target, instead of a solid reading.
    pub dashed: bool,
    /// A second kind of reading, such as humidity beside temperature: drawn
    /// in a second colour, on its own scale.
    pub secondary: bool,
    /// Points as (seconds ago, value); each value holds until the next.
    pub points: Vec<(f64, f64)>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Footer {
    pub text: String,
    /// Whether the dot before the text lights up.
    pub good: bool,
    pub actions: Vec<FooterAction>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct FooterAction {
    pub title: String,
    pub enabled: bool,
    pub action: Option<ActionKind>,
}

/// What a plugin shows in the bar, read from its `tileception://bar`
/// resource: chips that come and go, such as one while lights are on.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Bar {
    pub chips: Vec<BarChip>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct BarChip {
    pub label: String,
    pub icon: Option<String>,
    pub tint: Tint,
    /// What clicking it does; without one, the panel opens with the
    /// plugin's block unfolded.
    pub action: Option<ActionKind>,
}

/// What a plugin shows under the launcher, read from its
/// `tileception://launcher` resource: cards that come and go, such as one
/// per running timer.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Launcher {
    pub cards: Vec<Card>,
}

/// The most cards one plugin shows under the launcher.
pub const LAUNCHER_CARDS: usize = 4;

/// Colours from the theme for icons and badges.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Tint {
    Accent,
    Cool,
    Warm,
    Danger,
    #[default]
    Muted,
    /// Muted and see-through, for something that does not answer.
    Faded,
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test assertions")]
mod tests {
    use super::*;

    #[test]
    fn level_labels() {
        let plain = Level { unit: "%".into(), ..Level::default() };
        assert_eq!(plain.label(40), "40%");
        let warmth = Level { steps: vec!["Warm".into(), "Neutral".into(), "Cool".into()], ..Level::default() };
        assert_eq!(warmth.label(0), "Warm");
        assert_eq!(warmth.label(33), "Warm");
        assert_eq!(warmth.label(34), "Neutral");
        assert_eq!(warmth.label(66), "Neutral");
        assert_eq!(warmth.label(67), "Cool");
        assert_eq!(warmth.label(100), "Cool");
    }

    #[test]
    fn reads_a_small_block() {
        let block: Block = serde_json::from_str(
            r#"{"title":"Home","active":1,"views":[{"title":"Rooms","items":[
                {"type":"group","title":"Living","items":[
                    {"type":"row","id":"lamp","title":"Lamp","enabled":true,
                     "switch":{"on":true,"action":{"type":"tool","name":"toggle","arguments":{"device":"lamp"}}}}
                ]}
            ]}]}"#,
        )
        .unwrap();
        let Item::Group(group) = &block.views[0].items[0] else { panic!("a group") };
        let Item::Row(row) = &group.items[0] else { panic!("a row") };
        assert!(row.switch.as_ref().unwrap().checked);
        assert_eq!(block.active, 1);
    }

    #[test]
    fn reads_launcher_cards() {
        let launcher: Launcher = serde_json::from_str(
            r#"{"cards":[{"id":"tea","title":"Tea","value":"2:41","enabled":true,
                "buttons":[{"title":"Pause","icon":"glyph:pause","primary":true,"enabled":true,
                            "action":{"type":"tool","name":"pause","arguments":{"timer":"tea"}}},
                           {"title":"Stop"}],
                "progress":{"value":35.5}}]}"#,
        )
        .unwrap();
        let card = &launcher.cards[0];
        assert_eq!(card.buttons.len(), 2);
        assert!(card.buttons[0].primary && card.buttons[0].enabled);
        assert!(!card.buttons[1].enabled && card.buttons[1].action.is_none());
        assert!((card.progress.unwrap().share() - 0.355).abs() < 1e-9);
        assert!((Progress { value: 250.0 }.share() - 1.0).abs() < 1e-9);
        assert!(Progress { value: f64::NAN }.share().abs() < 1e-9);
    }
}
