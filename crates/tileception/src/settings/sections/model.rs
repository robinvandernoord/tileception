use super::*;
use toml::Value;
use toml_edit::{DocumentMut, Item};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Kind {
    Displays,
    Sharing,
    Touch,
    Shortcuts,
    Startup,
    Arguments,
    Rules,
    Battery,
}

#[derive(Clone, PartialEq, Eq)]
pub(super) enum Path {
    Name,
    Key(String),
    Element(String, usize),
    Position,
    Split,
}

#[derive(Clone)]
pub(super) enum Form {
    Text,
    Secret,
    Number,
    OptionalNumber,
    Scale,
    Colour,
    Toggle,
    Choice(Vec<(String, String)>),
}

#[derive(Clone)]
pub(super) struct Field {
    pub path: Path,
    pub label: String,
    pub hint: String,
    pub form: Form,
    pub pick: bool,
    pub removable: Option<usize>,
}

pub(super) struct Row {
    pub name: String,
    pub value: Value,
    pub original_name: String,
    pub original: Value,
    pub source: Option<Item>,
    pub inherited: bool,
    pub builtin: Option<Value>,
}

pub(in crate::settings) struct Screen {
    pub field: usize,
    pub(super) kind: Kind,
    pub original: String,
    pub(super) rows: Vec<Row>,
    pub selected: Option<usize>,
    pub scroll: f64,
    pub list_scroll: f64,
    pub search: TextInput,
    pub searching: bool,
    pub error: String,
    pub(super) edit: Option<(Field, TextInput)>,
    pub(super) focus: Option<Action>,
    pub(super) menu: Option<Menu>,
    pub recording: bool,
    pub record_modifier: Option<Mods>,
    pub removed: bool,
    pub initial: Vec<(String, Value)>,
    pub reveal: bool,
    pub advanced: bool,
    pub(super) monitors: Vec<displays::Monitor>,
    pub(super) map_drag: Option<displays::Drag>,
}

pub(super) struct Spec;
impl Spec {
    fn field(path: Path, label: &str, hint: &str, form: Form) -> Field {
        Field { path, label: label.into(), hint: hint.into(), form, pick: false, removable: None }
    }
    fn key(key: &str, label: &str, hint: &str, form: Form) -> Field {
        Self::field(Path::Key(key.into()), label, hint, form)
    }
    fn choice(options: &[(&str, &str)]) -> Form {
        Form::Choice(options.iter().map(|(label, value)| ((*label).into(), (*value).into())).collect())
    }
}

fn empty() -> Value {
    Value::Table(toml::Table::new())
}
fn string(text: &str) -> Value {
    Value::String(text.into())
}

impl Row {
    fn item(&self) -> Result<Item, String> {
        if self.value == self.original
            && let Some(source) = &self.source
        {
            return Ok(source.clone());
        }
        sync(self.source.as_ref(), &self.value)
    }
    fn new(name: String, value: Value, source: Option<Item>, inherited: bool) -> Row {
        Row { original_name: name.clone(), original: value.clone(), name, value, source, inherited, builtin: None }
    }
    pub fn changed(&self) -> bool {
        self.value != self.original || self.name != self.original_name || self.source.is_none() && !self.inherited
    }
}

impl Screen {
    pub fn new(field: usize, source: &str) -> Result<Self, String> {
        let key = FIELDS[field].key;
        let kind = match key {
            "outputs" => Kind::Displays,
            "handoff" => Kind::Sharing,
            "touchscreens" => Kind::Touch,
            "binds" => Kind::Shortcuts,
            "general.autostart" => Kind::Startup,
            "launch_args" => Kind::Arguments,
            "rules" => Kind::Rules,
            "battery.levels" => Kind::Battery,
            _ => return Err("Unsupported settings screen".into()),
        };
        let original = crate::config_edit::section_text(source, key)?;
        let raw: Value = source.parse::<toml::Table>().map(Value::Table).map_err(|error| error.to_string())?;
        let doc: DocumentMut = source.parse().map_err(|error| format!("{error}"))?;
        let target = key.split('.').try_fold(&raw, |value, name| value.get(name));
        let item = key.split('.').try_fold(doc.as_item(), |item, name| item.get(name));
        let mut rows = Vec::new();
        match kind {
            Kind::Startup | Kind::Rules | Kind::Battery => {
                if let Some(values) = target.and_then(Value::as_array) {
                    for (position, value) in values.iter().enumerate() {
                        let source = item.and_then(|item| match item {
                            Item::ArrayOfTables(tables) => tables.get(position).cloned().map(Item::Table),
                            Item::Value(toml_edit::Value::Array(array)) => {
                                array.get(position).cloned().map(Item::Value)
                            }
                            _ => None,
                        });
                        rows.push(Row::new(String::new(), value.clone(), source, false));
                    }
                }
            }
            Kind::Sharing => rows.push(Row::new(
                "This computer".into(),
                target.cloned().unwrap_or_else(empty),
                item.cloned(),
                item.is_none(),
            )),
            _ => {
                if let Some(table) = target.and_then(Value::as_table) {
                    for (name, value) in table {
                        rows.push(Row::new(
                            name.clone(),
                            value.clone(),
                            item.and_then(|item| item.get(name)).cloned(),
                            false,
                        ));
                    }
                }
            }
        }
        if kind == Kind::Shortcuts {
            let config = crate::config::Config::parse(source)?;
            if config.file.general.default_binds || config.file.binds.is_none() {
                let defaults: toml::Table =
                    crate::config::DEFAULT.parse::<toml::Table>().map_err(|error| error.to_string())?;
                if let Some(defaults) = defaults.get("binds").and_then(Value::as_table) {
                    for (name, value) in defaults {
                        if let Some(row) = rows.iter_mut().find(|row| {
                            crate::config::parse_combo(&row.name).ok() == crate::config::parse_combo(name).ok()
                        }) {
                            row.builtin = Some(value.clone());
                        } else {
                            let mut row = Row::new(name.clone(), value.clone(), None, true);
                            row.builtin = Some(value.clone());
                            rows.push(row);
                        }
                    }
                }
            }
        }
        if kind == Kind::Battery && target.is_none() {
            for level in crate::config::Config::parse(source)?.file.battery.levels {
                rows.push(Row::new(
                    String::new(),
                    Value::Table(toml::Table::from_iter([
                        ("at".into(), Value::Integer(level.threshold as i64)),
                        ("colour".into(), string(&level.colour)),
                    ])),
                    None,
                    true,
                ));
            }
        }
        let selected = (!rows.is_empty()).then_some(0);
        let initial = rows.iter().map(|row| (row.name.clone(), row.value.clone())).collect();
        Ok(Self {
            field,
            kind,
            original,
            rows,
            selected,
            scroll: 0.,
            list_scroll: 0.,
            search: TextInput::default(),
            searching: false,
            error: String::new(),
            edit: None,
            focus: None,
            menu: None,
            recording: false,
            record_modifier: None,
            removed: false,
            initial,
            reveal: false,
            advanced: false,
            monitors: Vec::new(),
            map_drag: None,
        })
    }

    pub fn changed(&self) -> bool {
        (self.removed
            && (self.rows.len() != self.initial.len()
                || self
                    .rows
                    .iter()
                    .zip(&self.initial)
                    .any(|(row, (name, value))| row.name != *name || row.value != *value)))
            || self.edit.as_ref().is_some_and(|(field, input)| input.text != self.read(&field.path))
            || self.rows.iter().any(Row::changed)
    }
    pub(super) fn row(&self) -> Option<&Row> {
        self.selected.and_then(|index| self.rows.get(index))
    }
    pub fn label(&self, index: usize) -> String {
        let row = &self.rows[index];
        match self.kind {
            Kind::Startup => row.value.as_str().unwrap_or("New command").to_string(),
            Kind::Rules => row
                .value
                .get("app_id")
                .or_else(|| row.value.get("title"))
                .and_then(Value::as_str)
                .unwrap_or("New rule")
                .into(),
            Kind::Battery => format!("At or below {}%", row.value.get("at").map_or("?".into(), ToString::to_string)),
            _ => {
                if row.name.is_empty() {
                    "New entry".into()
                } else {
                    row.name.clone()
                }
            }
        }
    }

    pub(super) fn visible_rows(&self) -> Vec<usize> {
        let query = self.search.text.to_lowercase();
        (0..self.rows.len())
            .filter(|index| !self.shadowed(*index))
            .filter(|index| {
                self.label(*index).to_lowercase().contains(&query)
                    || self.rows[*index].value.to_string().to_lowercase().contains(&query)
            })
            .collect()
    }

    fn shadowed(&self, index: usize) -> bool {
        let row = &self.rows[index];
        self.kind == Kind::Shortcuts
            && row.inherited
            && !row.changed()
            && self.rows.iter().any(|other| {
                (!other.inherited || other.changed())
                    && crate::config::parse_combo(&other.name).ok() == crate::config::parse_combo(&row.name).ok()
            })
    }

    pub(super) fn filter_changed(&mut self) {
        let visible = self.visible_rows();
        if self.selected.is_none_or(|index| !visible.contains(&index)) {
            self.selected = visible.first().copied();
            self.scroll = 0.;
        }
        self.list_scroll = 0.;
    }

    pub(super) fn read(&self, path: &Path) -> String {
        let Some(row) = self.row() else { return String::new() };
        if *path == Path::Name {
            return row.name.clone();
        }
        if *path == Path::Position {
            return if row.value.get("position").is_some() { "manual" } else { "auto" }.into();
        }
        if *path == Path::Split {
            return match row.value.get("split") {
                Some(Value::Array(_)) => "custom",
                Some(Value::Integer(count)) if *count > 1 => "equal",
                _ => "off",
            }
            .into();
        }
        let value = match path {
            Path::Key(key) => {
                if key.is_empty() {
                    Some(&row.value)
                } else {
                    row.value.get(key)
                }
            }
            Path::Element(key, index) => {
                let array = if key.is_empty() { Some(&row.value) } else { row.value.get(key) };
                match array {
                    Some(Value::String(_)) if *index == 0 => array,
                    _ => array.and_then(|value| value.get(*index)),
                }
            }
            _ => None,
        };
        value.map_or_else(
            || {
                if self.kind == Kind::Sharing {
                    let Path::Key(key) = path else { return String::new() };
                    match key.as_str() {
                        "enabled" => "false",
                        "discovery" => "true",
                        "port" => return tileception_handoff::DEFAULT_PORT.to_string(),
                        _ => "",
                    }
                    .into()
                } else if *path == Path::Key("scale".into()) {
                    "auto".into()
                } else if self.kind == Kind::Displays {
                    match path {
                        Path::Key(key) if key == "enabled" => "true".into(),
                        Path::Key(key) if key == "mode" => "preferred".into(),
                        Path::Key(key) if key == "transform" => "normal".into(),
                        _ => String::new(),
                    }
                } else {
                    String::new()
                }
            },
            |value| match value {
                Value::String(text) => text.clone(),
                _ => value.to_string(),
            },
        )
    }

    pub(super) fn write(&mut self, path: &Path, value: Value) {
        let Some(row) = self.selected.and_then(|index| self.rows.get_mut(index)) else { return };
        match path {
            Path::Name => row.name = value.as_str().unwrap_or_default().into(),
            Path::Key(key) if key.is_empty() => row.value = value,
            Path::Key(key) => {
                if let Some(table) = row.value.as_table_mut() {
                    table.insert(key.clone(), value);
                }
            }
            Path::Element(key, index) => {
                if key.is_empty() && row.value.is_str() && *index == 0 {
                    row.value = value;
                    return;
                }
                let array = if key.is_empty() {
                    &mut row.value
                } else {
                    let Some(table) = row.value.as_table_mut() else { return };
                    table.entry(key).or_insert_with(|| Value::Array(Vec::new()))
                };
                if let Some(array) = array.as_array_mut() {
                    while array.len() <= *index {
                        array.push(Value::Integer(0));
                    }
                    array[*index] = value;
                }
            }
            Path::Position => {
                if let Some(table) = row.value.as_table_mut() {
                    if value.as_str() == Some("auto") {
                        table.remove("position");
                    } else if !table.contains_key("position") {
                        table.insert("position".into(), Value::Array(vec![Value::Integer(0), Value::Integer(0)]));
                    }
                }
            }
            Path::Split => {
                if let Some(table) = row.value.as_table_mut() {
                    match value.as_str() {
                        Some("equal") => {
                            table.insert("split".into(), Value::Integer(2));
                        }
                        Some("custom") => {
                            table.insert("split".into(), Value::Array(vec![Value::Float(1.), Value::Float(1.)]));
                        }
                        _ => {
                            table.remove("split");
                        }
                    }
                }
            }
        }
    }

    pub(super) fn set_text(&mut self, field: &Field, text: &str) -> Result<(), String> {
        let text = if matches!(field.form, Form::Text | Form::Secret) && field.path != Path::Name {
            text
        } else {
            text.trim()
        };
        if text.is_empty()
            && matches!(field.form, Form::OptionalNumber | Form::Text | Form::Secret)
            && let Path::Key(key) = &field.path
            && !key.is_empty()
        {
            if let Some(table) =
                self.selected.and_then(|index| self.rows.get_mut(index)).and_then(|row| row.value.as_table_mut())
            {
                table.remove(key);
            }
            return Ok(());
        }
        let value = match &field.form {
            Form::Number | Form::OptionalNumber => {
                if text.is_empty()
                    && matches!(field.form, Form::OptionalNumber)
                    && let Path::Element(key, _) = &field.path
                {
                    if let Some(table) = self
                        .selected
                        .and_then(|index| self.rows.get_mut(index))
                        .and_then(|row| row.value.as_table_mut())
                    {
                        table.remove(key);
                    }
                    return Ok(());
                }
                if matches!(&field.path, Path::Element(key, _) if key == "split") {
                    Value::Float(text.parse::<f64>().map_err(|_| format!("{} needs a number.", field.label))?)
                } else {
                    Value::Integer(text.parse::<i64>().map_err(|_| format!("{} needs a whole number.", field.label))?)
                }
            }
            Form::Scale if text != "auto" => Value::Float(
                text.parse::<f64>().map_err(|_| "Scale must be auto or a number from 0.5 to 4.".to_string())?,
            ),
            Form::Choice(_) if text == "inherit" => {
                if let Path::Key(key) = &field.path
                    && let Some(table) = self
                        .selected
                        .and_then(|index| self.rows.get_mut(index))
                        .and_then(|row| row.value.as_table_mut())
                {
                    table.remove(key);
                }
                return Ok(());
            }
            Form::Choice(_) if text == "true" || text == "false" => Value::Boolean(text == "true"),
            _ => string(text),
        };
        self.write(&field.path, value);
        Ok(())
    }

    pub fn finish_input(&mut self) -> Result<(), String> {
        if let Some((field, input)) = self.edit.take() {
            if input.text == self.read(&field.path) {
                return Ok(());
            }
            if let Err(error) = self.set_text(&field, &input.text) {
                self.edit = Some((field, input));
                return Err(error);
            }
        }
        Ok(())
    }

    pub(super) fn fields(&self, outputs: &[String], devices: &[String]) -> Vec<Field> {
        let Some(row) = self.row() else { return Vec::new() };
        let mut fields = Vec::new();
        let mut name = |label, hint| fields.push(Spec::field(Path::Name, label, hint, Form::Text));
        match self.kind {
            Kind::Startup => fields.push(Spec::key(
                "",
                "Command",
                "Run once when the session starts. Shell syntax is supported.",
                Form::Text,
            )),
            Kind::Arguments => {
                name("Application", "Desktop file name, for example firefox.desktop.");
                array_fields(&mut fields, &row.value, "", "Argument", Form::Text);
            }
            Kind::Shortcuts => {
                name("Shortcut or gesture", "Record a key combination, or choose a gesture below.");
                let gestures = [
                    "Swipe3Left",
                    "Swipe3Right",
                    "Swipe3Up",
                    "Swipe3Down",
                    "Swipe4Left",
                    "Swipe4Right",
                    "Swipe4Up",
                    "Swipe4Down",
                    "Pinch2In",
                    "Pinch2Out",
                    "Pinch3In",
                    "Pinch3Out",
                    "TouchEdgeTop",
                    "TouchEdgeBottom",
                    "TouchEdgeLeft",
                    "TouchEdgeRight",
                ];
                fields.push(Spec::field(
                    Path::Name,
                    "Gesture",
                    "Selecting a gesture replaces the shortcut.",
                    Form::Choice(gestures.iter().map(|text| (describe_combo(text), (*text).into())).collect()),
                ));
                let actions = [
                    ("Custom command", "spawn "),
                    ("Open launcher", "launcher"),
                    ("Open Settings", "settings"),
                    ("Close window", "close"),
                    ("Toggle fullscreen", "fullscreen"),
                    ("Float or tile", "float"),
                    ("Overview", "overview"),
                    ("Lock", "lock"),
                    ("Power menu", "power-menu"),
                    ("Disable binding", "none"),
                ];
                fields.push(Spec::field(
                    Path::Element("".into(), 0),
                    "Action",
                    "Choose an action or edit the commands below. Multiple commands run in order.",
                    Spec::choice(&actions),
                ));
                if let Some(array) = row.value.as_array() {
                    array_fields(&mut fields, &Value::Array(array.clone()), "", "Command", Form::Text);
                } else {
                    fields.push(Spec::field(
                        Path::Element("".into(), 0),
                        "Command",
                        "For a program use spawn, followed by a shell command.",
                        Form::Text,
                    ));
                }
            }
            Kind::Rules => {
                fields.push(Spec::key(
                    "app_id",
                    "Application",
                    "Match the app ID or X11 class. * matches any text; ? matches one character.",
                    Form::Text,
                ));
                fields.push(Spec::key(
                    "title",
                    "Window title",
                    "Optional pattern. When both patterns are set, both must match.",
                    Form::Text,
                ));
                for (key, label) in [("float", "Floating"), ("fullscreen", "Fullscreen"), ("minimized", "Minimized")] {
                    fields.push(Spec::key(
                        key,
                        label,
                        "Keep the application's preference, or override it.",
                        Spec::choice(&[("Application default", "inherit"), ("On", "true"), ("Off", "false")]),
                    ));
                }
                for (index, label) in ["Floating width", "Floating height"].iter().enumerate() {
                    fields.push(Spec::field(
                        Path::Element("size".into(), index),
                        label,
                        "Pixels. Clear either dimension to remove the size override.",
                        Form::OptionalNumber,
                    ));
                }
            }
            Kind::Battery => {
                fields.push(Spec::key(
                    "at",
                    "At or below charge",
                    "Percentage from 0 to 100. The lowest matching threshold is used.",
                    Form::Number,
                ));
                fields.push(Spec::key(
                    "colour",
                    "Border colour",
                    "Hex colour or theme name, such as warning, danger or accent.",
                    Form::Colour,
                ));
                fields.push(Spec::key(
                    "colour",
                    "Theme colour",
                    "Or enter a custom colour above.",
                    Spec::choice(&[
                        ("Accent", "accent"),
                        ("Warning", "warning"),
                        ("Danger", "danger"),
                        ("Muted", "muted"),
                        ("Text", "text"),
                    ]),
                ));
            }
            Kind::Touch => {
                name("Touchscreen name", "Use the exact device name, or select a detected touchscreen.");
                if !devices.is_empty() {
                    fields.push(Spec::field(
                        Path::Name,
                        "Detected touchscreens",
                        "Touch a screen to detect it if it is not listed yet.",
                        Form::Choice(devices.iter().map(|text| (text.clone(), text.clone())).collect()),
                    ));
                }
                fields.push(Spec::key(
                    "output",
                    "Display",
                    "Connector name. A disconnected mapping stays saved.",
                    Form::Choice(outputs.iter().map(|text| (text.clone(), text.clone())).collect()),
                ));
                fields.push(Spec::key(
                    "output",
                    "Display connector",
                    "Manual entry also supports currently disconnected displays.",
                    Form::Text,
                ));
            }
            Kind::Sharing => {
                fields.push(Spec::key(
                    "enabled",
                    "Share mouse, keyboard and clipboard",
                    "Enable on both computers and use the same passphrase.",
                    Form::Toggle,
                ));
                fields.push(Spec::key("name", "Computer name", "Empty uses this computer's host name.", Form::Text));
                fields.push(Spec::key(
                    "passphrase",
                    "Shared passphrase",
                    "Enter the same secret on both computers. Sharing does not listen without one.",
                    Form::Secret,
                ));
                fields.push(Spec::key(
                    "discovery",
                    "Find computers automatically",
                    "Look for other Tileception computers on the local network.",
                    Form::Toggle,
                ));
                if self.advanced {
                    fields.push(Spec::key(
                        "port",
                        "Network port",
                        "Normally leave this unchanged. Manual peers may use host:port.",
                        Form::Number,
                    ));
                }
                if self.advanced
                    && let Some(peers) = row.value.get("peers")
                {
                    array_fields(&mut fields, peers, "peers", "Manual computer", Form::Text);
                }
            }
            Kind::Displays => {
                if !self.monitors.iter().any(|monitor| monitor.name == row.name) {
                    name(
                        "Connector",
                        "Connected screens appear automatically; saved disconnected screens remain available.",
                    );
                }
                fields.push(Spec::key(
                    "enabled",
                    "Screen enabled",
                    "Disabled screens stay in this list so you can enable them again.",
                    Form::Toggle,
                ));
                let mut modes = vec![("Preferred resolution and refresh".into(), "preferred".into())];
                if let Some(monitor) = self.monitors.iter().find(|monitor| monitor.name == row.name) {
                    for mode in &monitor.modes {
                        let value = displays::mode_string(*mode);
                        modes.push((
                            format!("{} × {} at {} Hz", mode.size.w, mode.size.h, f64::from(mode.refresh) / 1000.),
                            value,
                        ));
                    }
                }
                if let Some(value) = row.value.get("mode").and_then(Value::as_str)
                    && !modes.iter().any(|(_, candidate)| candidate == value)
                {
                    modes.push((format!("{value} (saved mode)"), value.into()));
                }
                fields.push(Spec::key(
                    "mode",
                    "Resolution and refresh",
                    "Choose an advertised mode. Apply previews it for 15 seconds.",
                    Form::Choice(modes),
                ));
                fields.push(Spec::key("transform", "Rotation and flip", "Rotations follow Wayland's counterclockwise convention. Flip mirrors horizontally before rotation.", Spec::choice(&[
                    ("Normal", "normal"),
                    ("90° counterclockwise", "90"),
                    ("180°", "180"),
                    ("90° clockwise", "270"),
                    ("Mirrored", "flipped"),
                    ("Mirrored, 90° counterclockwise", "flipped-90"),
                    ("Mirrored, 180°", "flipped-180"),
                    ("Mirrored, 90° clockwise", "flipped-270"),
                ])));
                fields.push(Spec::key(
                    "scale",
                    "Scale",
                    "Automatic, or a number from 0.5 to 4 such as 1.25.",
                    Form::Scale,
                ));
                fields.push(Spec::field(
                    Path::Position,
                    "Position",
                    "Drag screens above to arrange them, or enter logical coordinates.",
                    Spec::choice(&[("Automatic horizontal row", "auto"), ("Manual position", "manual")]),
                ));
                if row.value.get("position").is_some() {
                    for (index, label) in ["Horizontal position", "Vertical position"].iter().enumerate() {
                        fields.push(Spec::field(
                            Path::Element("position".into(), index),
                            label,
                            "Logical pixels, after scaling. Negative positions are supported.",
                            Form::Number,
                        ));
                    }
                }
                fields.push(Spec::field(
                    Path::Split,
                    "Screen areas",
                    "Separate workspace areas side by side on this display.",
                    Spec::choice(&[
                        ("One workspace", "off"),
                        ("Equal areas", "equal"),
                        ("Custom proportions", "custom"),
                    ]),
                ));
                match row.value.get("split") {
                    Some(Value::Integer(_)) => {
                        fields.push(Spec::key("split", "Number of areas", "Between 1 and 8.", Form::Number))
                    }
                    Some(Value::Array(parts)) => {
                        array_fields(&mut fields, &Value::Array(parts.clone()), "split", "Area width", Form::Number)
                    }
                    _ => {}
                }
            }
        }
        for field in &mut fields {
            field.pick = match self.kind {
                Kind::Startup => true,
                Kind::Arguments | Kind::Rules => field.path == Path::Name || field.path == Path::Key("app_id".into()),
                Kind::Shortcuts => {
                    matches!(&field.path, Path::Element(key, 0) if key.is_empty()) && matches!(field.form, Form::Text)
                }
                _ => false,
            };
        }
        fields
    }

    pub fn add(&mut self) {
        let (name, value) = match self.kind {
            Kind::Startup => (String::new(), string("")),
            Kind::Arguments => (String::new(), Value::Array(vec![string("")])),
            Kind::Shortcuts => (String::new(), string("launcher")),
            Kind::Battery => (
                String::new(),
                Value::Table(toml::Table::from_iter([
                    ("at".into(), Value::Integer(20)),
                    ("colour".into(), string("warning")),
                ])),
            ),
            _ => (String::new(), empty()),
        };
        self.rows.push(Row::new(name, value, None, false));
        self.selected = Some(self.rows.len() - 1);
        self.scroll = 0.;
    }
    pub fn remove(&mut self) {
        if let Some(index) = self.selected {
            if self.kind == Kind::Displays && self.monitors.iter().any(|monitor| monitor.name == self.rows[index].name)
            {
                let scale = self
                    .rows
                    .iter()
                    .find(|row| row.name == "*")
                    .and_then(|row| row.value.get("scale"))
                    .cloned()
                    .unwrap_or_else(|| string("auto"));
                let value = Value::Table(toml::Table::from_iter([("scale".into(), scale)]));
                let row = &mut self.rows[index];
                row.value = value.clone();
                row.original = value;
                row.source = None;
                row.inherited = true;
                self.removed = true;
            } else if self.kind == Kind::Shortcuts && self.rows[index].builtin.is_some() {
                self.removed = true;
                let row = &mut self.rows[index];
                if row.inherited {
                    row.value = string("none");
                    row.inherited = false;
                } else {
                    row.value = row.builtin.clone().unwrap_or_else(|| string("none"));
                    row.inherited = true;
                    row.source = None;
                    row.original = row.value.clone();
                }
            } else {
                self.rows.remove(index);
                self.removed = true;
                self.selected = (!self.rows.is_empty()).then_some(index.min(self.rows.len().saturating_sub(1)));
            }
            self.scroll = 0.;
        }
    }
    pub fn move_row(&mut self, down: bool) {
        if let Some(index) = self.selected {
            let next = if down { (index + 1).min(self.rows.len() - 1) } else { index.saturating_sub(1) };
            if index != next {
                self.rows.swap(index, next);
                self.selected = Some(next);
                self.removed = true;
            }
        }
    }
    pub fn add_value(&mut self) {
        let kind = self.kind;
        if let Some(row) = self.selected.and_then(|index| self.rows.get_mut(index)) {
            let target = match kind {
                Kind::Sharing => row
                    .value
                    .as_table_mut()
                    .map(|table| table.entry("peers").or_insert_with(|| Value::Array(Vec::new()))),
                Kind::Displays => row.value.get_mut("split"),
                _ => Some(&mut row.value),
            };
            if let Some(target) = target {
                if let Value::String(command) = target {
                    *target = Value::Array(vec![string(command)]);
                }
                if let Value::Array(array) = target {
                    array.push(if kind == Kind::Displays { Value::Float(1.) } else { string("") });
                }
            }
        }
    }
    pub fn remove_value(&mut self, index: usize) {
        let kind = self.kind;
        if let Some(row) = self.selected.and_then(|position| self.rows.get_mut(position)) {
            let value = match kind {
                Kind::Sharing => row.value.get_mut("peers"),
                Kind::Displays => row.value.get_mut("split"),
                _ => Some(&mut row.value),
            };
            if let Some(Value::Array(array)) = value
                && index < array.len()
            {
                array.remove(index);
            }
        }
    }

    pub fn source(&self) -> Result<String, String> {
        self.validate_displays()?;
        let mut excerpt: DocumentMut = self.original.parse().map_err(|error| format!("{error}"))?;
        let key = FIELDS[self.field].key;
        let mut names = std::collections::BTreeSet::new();
        for (index, row) in self.rows.iter().enumerate() {
            if self.shadowed(index) {
                continue;
            }
            if matches!(self.kind, Kind::Displays | Kind::Touch | Kind::Arguments | Kind::Shortcuts) {
                if row.name.trim().is_empty() {
                    return Err("Each entry needs a name.".into());
                }
                let name = if self.kind == Kind::Shortcuts {
                    format!("{:?}", crate::config::parse_combo(&row.name)?)
                } else {
                    row.name.clone()
                };
                if !names.insert(name) {
                    return Err(format!("Duplicate entry: {}. Change or remove the other entry first.", row.name));
                }
            }
            if self.kind == Kind::Startup && row.value.as_str().is_none_or(|command| command.trim().is_empty()) {
                return Err("Autostart commands cannot be empty.".into());
            }
            if self.kind == Kind::Shortcuts {
                for command in row.value.as_array().map_or_else(|| vec![&row.value], |array| array.iter().collect()) {
                    let command = command.as_str().ok_or("A shortcut action must be text")?;
                    if command != "none" {
                        crate::commands::parse(command)?;
                    }
                }
            }
            if self.kind == Kind::Battery
                && row.value.get("at").and_then(Value::as_integer).is_none_or(|value| !(0..=100).contains(&value))
            {
                return Err("Battery thresholds must be between 0 and 100.".into());
            }
        }
        let item = match self.kind {
            Kind::Sharing => self.rows[0].item()?,
            Kind::Rules => {
                let mut tables = toml_edit::ArrayOfTables::new();
                for row in &self.rows {
                    tables.push(row.item()?.into_table().map_err(|_| "Expected a rule table")?);
                }
                Item::ArrayOfTables(tables)
            }
            Kind::Startup | Kind::Battery => {
                if self.kind == Kind::Battery
                    && excerpt.get("battery").and_then(|item| item.get("levels")).is_some_and(Item::is_array_of_tables)
                {
                    let mut tables = toml_edit::ArrayOfTables::new();
                    for row in &self.rows {
                        tables.push(row.item()?.into_table().map_err(|_| "Expected a threshold table")?);
                    }
                    excerpt["battery"]["levels"] = Item::ArrayOfTables(tables);
                    return Ok(excerpt.to_string());
                }
                let mut array = toml_edit::Array::new();
                for row in &self.rows {
                    array.push_formatted(row.item()?.into_value().map_err(|_| "Expected an array value")?);
                }
                if let Some(old) =
                    key.split('.').try_fold(excerpt.as_item(), |item, key| item.get(key)).and_then(Item::as_array)
                {
                    *array.decor_mut() = old.decor().clone();
                    array.set_trailing(old.trailing().clone());
                    array.set_trailing_comma(old.trailing_comma());
                }
                Item::Value(toml_edit::Value::Array(array))
            }
            _ => {
                let previous = excerpt.get(key).and_then(Item::as_table).cloned().unwrap_or_default();
                let mut table = previous.clone();
                table.clear();
                for row in &self.rows {
                    if row.inherited && !row.changed() {
                        continue;
                    }
                    if self.kind == Kind::Shortcuts && row.builtin.is_some() && row.name != row.original_name {
                        table[&row.original_name] = toml_edit::value("none");
                    }
                    let old = previous.get_key_value(&row.original_name).map(|(key, _)| key.clone());
                    let formatted = match old {
                        Some(key) if row.name == row.original_name => key,
                        Some(key) => toml_edit::Key::new(&row.name)
                            .with_leaf_decor(key.leaf_decor().clone())
                            .with_dotted_decor(key.dotted_decor().clone()),
                        None => toml_edit::Key::new(&row.name),
                    };
                    table.insert_formatted(&formatted, row.item()?);
                }
                Item::Table(table)
            }
        };
        if let Some((section, name)) = key.split_once('.') {
            if excerpt.get(section).is_none() {
                excerpt[section] = Item::Table(toml_edit::Table::new());
            }
            let table = excerpt[section].as_table_mut().ok_or("Expected a section table")?;
            let formatted =
                table.get_key_value(name).map(|(key, _)| key.clone()).unwrap_or_else(|| toml_edit::Key::new(name));
            table.insert_formatted(&formatted, item);
        } else {
            excerpt[key] = item;
        }
        Ok(excerpt.to_string())
    }
}

fn array_fields(fields: &mut Vec<Field>, value: &Value, key: &str, label: &str, form: Form) {
    if let Some(array) = value.as_array() {
        for index in 0..array.len() {
            let mut field = Spec::field(
                Path::Element(key.into(), index),
                &format!("{label} {}", index + 1),
                if key == "peers" {
                    "Host name, IP address, or host:port. Only needed when discovery is unavailable."
                } else if label == "Argument" {
                    "One argument per row. Spaces stay inside this argument; no shell quoting needed."
                } else {
                    "Entries are used in the displayed order."
                },
                form.clone(),
            );
            field.removable = Some(index);
            fields.push(field);
        }
    }
}

/// Keep table/key/value decorations for untouched children of a changed row.
fn sync(old: Option<&Item>, value: &Value) -> Result<Item, String> {
    if let Some(old) = old.and_then(Item::as_value) {
        let parsed = format!("item = {old}").parse::<toml::Table>().ok();
        if parsed.as_ref().and_then(|table| table.get("item")) == Some(value) {
            return Ok(Item::Value(old.clone()));
        }
        if let (Some(before), Value::Array(after)) = (old.as_array(), value) {
            let mut array = before.clone();
            array.clear();
            for (index, value) in after.iter().enumerate() {
                let original = before.get(index).cloned().map(Item::Value);
                array.push_formatted(sync(original.as_ref(), value)?.into_value().map_err(|_| "Expected array item")?);
            }
            return Ok(Item::Value(toml_edit::Value::Array(array)));
        }
    }
    if let Value::Table(values) = value {
        if old.is_some_and(Item::is_inline_table) {
            let mut table = old.and_then(Item::as_inline_table).cloned().unwrap_or_default();
            table.retain(|key, _| values.contains_key(key));
            for (key, value) in values {
                let previous = table.get(key).cloned().map(Item::Value);
                table.insert(key, sync(previous.as_ref(), value)?.into_value().map_err(|_| "Expected inline value")?);
            }
            return Ok(Item::Value(toml_edit::Value::InlineTable(table)));
        }
        let mut table = old.and_then(Item::as_table).cloned().unwrap_or_default();
        table.retain(|key, _| values.contains_key(key));
        for (key, value) in values {
            table[key] = sync(table.get(key), value)?;
        }
        return Ok(Item::Table(table));
    }
    let mut new: toml_edit::Value = value.to_string().parse().map_err(|error| format!("{error}"))?;
    if let Some(old) = old.and_then(Item::as_value) {
        *new.decor_mut() = old.decor().clone();
    }
    Ok(Item::Value(new))
}

/// A key combination or gesture as people say it: "Super+/" rather than
/// `Super+slash`, "3-finger swipe left" rather than `Swipe3Left`.
pub(super) fn describe_combo(combo: &str) -> String {
    combo.split('+').map(describe_key).collect::<Vec<_>>().join("+")
}

fn describe_key(key: &str) -> String {
    let named = match key {
        "slash" => "/",
        "comma" => ",",
        "period" => ".",
        "equal" => "=",
        "minus" => "-",
        "bracketleft" => "[",
        "bracketright" => "]",
        "semicolon" => ";",
        "apostrophe" => "'",
        "grave" => "`",
        "backslash" => "\\",
        "KP_Add" => "Keypad +",
        "KP_Subtract" => "Keypad -",
        "Escape" => "Esc",
        "space" => "Space",
        "Page_Up" => "Page Up",
        "Page_Down" => "Page Down",
        "Print" => "Print Screen",
        "WheelUp" => "Scroll up",
        "WheelDown" => "Scroll down",
        "XF86AudioRaiseVolume" => "Volume up key",
        "XF86AudioLowerVolume" => "Volume down key",
        "XF86AudioMute" => "Mute key",
        "XF86AudioMicMute" => "Microphone mute key",
        "XF86AudioPlay" => "Play key",
        "XF86AudioPause" => "Pause key",
        "XF86AudioStop" => "Stop key",
        "XF86AudioNext" => "Next track key",
        "XF86AudioPrev" => "Previous track key",
        "XF86MonBrightnessUp" => "Brightness up key",
        "XF86MonBrightnessDown" => "Brightness down key",
        _ => "",
    };
    if !named.is_empty() {
        return named.into();
    }
    let (touch, gesture) = key.strip_prefix("Touch").map_or((false, key), |rest| (true, rest));
    let prefix = if touch { "Touch: " } else { "" };
    if let Some(edge) = gesture.strip_prefix("Edge") {
        return format!("Swipe in from the {} edge", edge.to_lowercase());
    }
    for (kind, verb) in [("Swipe", "swipe"), ("Pinch", "pinch")] {
        if let Some(rest) = gesture.strip_prefix(kind)
            && let Some(fingers) = rest.chars().next().filter(char::is_ascii_digit)
        {
            let direction = rest[1..].to_lowercase();
            return if fingers == '2' && kind == "Pinch" {
                format!("{prefix}Pinch {direction}")
            } else {
                format!("{prefix}{fingers}-finger {verb} {direction}")
            };
        }
    }
    key.into()
}

/// What a binding's command or commands do, in a few words.
pub(super) fn describe_action(value: &Value) -> String {
    match value {
        Value::String(command) => describe_command(command),
        Value::Array(commands) => {
            let parts: Vec<_> = commands.iter().filter_map(Value::as_str).map(describe_command).collect();
            if parts.is_empty() { "No action".into() } else { parts.join(", then ") }
        }
        _ => "No action".into(),
    }
}

fn describe_command(command: &str) -> String {
    let command = command.trim();
    let (name, rest) =
        command.split_once(char::is_whitespace).map_or((command, ""), |(name, rest)| (name, rest.trim()));
    let first = rest.split_whitespace().next().unwrap_or("");
    let text = match (name, first) {
        ("", _) => "No action",
        ("none", _) => "Disabled",
        ("spawn" | "exec", "") => "Run a command",
        ("spawn" | "exec", _) => {
            let program = first.trim_matches(['"', '\'']);
            let program = program.rsplit('/').next().unwrap_or(program);
            return match program {
                "tileception-screenshot" if rest.contains("region") => "Screenshot of an area".into(),
                "tileception-screenshot" => "Screenshot".into(),
                "gtk-launch" if rest.contains("default-web-browser") => "Open the web browser".into(),
                "xdg-open" if rest.contains("$HOME") => "Open your home folder".into(),
                "notify-send" => "Show a notification".into(),
                _ => format!("Run {program}"),
            };
        }
        ("terminal", _) => "Open a terminal",
        ("close", _) => "Close the window",
        ("focus", _) | ("left" | "right" | "up" | "down", "") => {
            return format!("Focus {}", if first.is_empty() { name } else { first });
        }
        ("move", _) => return format!("Move the window {first}"),
        ("resize", _) => return format!("Resize toward the {first}"),
        ("toggle-split", _) => "Switch the split direction",
        ("cycle", "prev" | "previous") => "Previous window",
        ("cycle", _) => "Next window",
        ("float", _) => "Float or tile the window",
        ("fullscreen", _) => "Fullscreen",
        ("minimize", _) => "Minimize",
        ("restore", _) => "Restore a minimized window",
        ("show-minimized", _) => "Show minimized windows",
        ("select", _) => "Select windows",
        ("group", _) => "Group the selection into tabs",
        ("next-tab", _) => "Next tab",
        ("prev-tab", _) => "Previous tab",
        ("drop", _) => "Drop the window",
        ("overview", _) => "Overview",
        ("step-in", _) => "Zoom in a level",
        ("step-out", _) => "Zoom out a level",
        ("maximize", _) => "Maximize",
        ("back", _) => "Previous view",
        ("forward", _) => "Next view",
        ("save", _) => return format!("Save view {first}"),
        ("go", _) => return format!("Go to view {first}"),
        ("desktop", _) => "Show or hide the Desktop",
        ("minimap", _) => "Minimap",
        ("launcher", _) => "Open the launcher",
        ("settings", _) => "Open Settings",
        ("notifications", _) => "Notifications",
        ("do-not-disturb", _) => "Do not disturb",
        ("lock", _) => "Lock the screen",
        ("power-menu", _) => "Power menu",
        ("screenshot", _) => "Screenshot",
        ("doctor", _) => "Doctor",
        ("duck", _) => "Rubber duck",
        ("handoff", "home") => "Bring input back",
        ("handoff", _) => "Sharing",
        ("reload", _) => "Reload the configuration",
        ("vt", _) => return format!("Switch to console {first}"),
        ("quit", _) => "Log out",
        ("restart", _) => "Restart Tileception",
        ("volume", "up") => "Volume up",
        ("volume", "down") => "Volume down",
        ("volume", "mute") => "Mute",
        ("volume", _) => "Volume",
        ("mic", "mute") => "Mute the microphone",
        ("mic", _) => "Microphone volume",
        ("brightness", "up") => "Brightness up",
        ("brightness", "down") => "Brightness down",
        ("brightness", _) => "Brightness",
        ("media", "play-pause") => "Play or pause",
        ("media", "play") => "Play",
        ("media", "pause") => "Pause",
        ("media", "next") => "Next track",
        ("media", "previous") => "Previous track",
        ("media", "stop") => "Stop playback",
        ("home", _) => "Smart home",
        ("colour-scheme" | "color-scheme", _) => "Colour scheme",
        _ => {
            let mut text = command.replace('-', " ");
            if let Some(first) = text.get_mut(0..1) {
                first.make_ascii_uppercase();
            }
            return text;
        }
    };
    text.into()
}
