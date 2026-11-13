//! What a device is and how its data points read: Tuya's standard codes
//! (`switch_led`, `bright_value_v2`, `temp_set`...) mapped to the few
//! things the panel shows, and changes turned back into data points.

use serde_json::{Map, Value};

use crate::store::{DataPoint, DeviceEntry, Readings};

/// What kind of device, as far as the panel cares.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Kind {
    Light,
    Plug,
    /// Air conditioners, thermostats and heaters: a target temperature.
    Climate,
    /// Temperature and humidity sensors: readings only.
    Sensor,
    #[default]
    Other,
}

/// An integer data point and its range.
#[derive(Debug, Clone, PartialEq)]
pub struct Range {
    pub point_id: String,
    pub min: i64,
    pub max: i64,
    /// Values are stored multiplied by ten to this power.
    pub scale: u32,
    pub step: i64,
}

impl Range {
    fn from(point_id: &str, point: &DataPoint) -> Range {
        let number = |name: &str, default: i64| point.values.get(name).and_then(Value::as_i64).unwrap_or(default);
        let min = number("min", 0);
        let max = number("max", 1000).max(min + 1);
        Range {
            point_id: point_id.to_string(),
            min,
            max,
            scale: number("scale", 0).clamp(0, 6) as u32,
            step: number("step", 1).max(1),
        }
    }

    fn divisor(&self) -> f64 {
        10f64.powi(self.scale as i32)
    }

    /// The real value of a raw reading.
    pub fn real(&self, raw: i64) -> f64 {
        raw as f64 / self.divisor()
    }

    /// The raw value nearest to a real one, on a step and within range.
    pub fn raw(&self, real: f64) -> i64 {
        let raw = (real * self.divisor()).round() as i64;
        let stepped = self.min + ((raw - self.min) as f64 / self.step as f64).round() as i64 * self.step;
        stepped.clamp(self.min, self.max)
    }

    /// A raw reading as a share of the range, in percent.
    pub fn percent(&self, raw: i64) -> u32 {
        (((raw - self.min) as f64 / (self.max - self.min) as f64) * 100.0).round().clamp(0.0, 100.0) as u32
    }

    /// The raw value at a share of the range.
    pub fn at_percent(&self, percent: u32) -> i64 {
        let raw = self.min as f64 + (self.max - self.min) as f64 * f64::from(percent.min(100)) / 100.0;
        self.raw(raw / self.divisor())
    }

    /// The smallest change of the real value.
    pub fn real_step(&self) -> f64 {
        self.step as f64 / self.divisor()
    }
}

/// Which data points carry what, for one device.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Model {
    pub kind: Kind,
    pub switch: Option<String>,
    pub brightness: Option<Range>,
    pub colour_temp: Option<Range>,
    /// `work_mode`: brightness and colour temperature apply in "white".
    pub work_mode: Option<String>,
    pub power: Option<Range>,
    pub temperature: Option<Range>,
    pub target: Option<Range>,
    pub humidity: Option<Range>,
    pub mode: Option<(String, Vec<String>)>,
    pub fan: Option<(String, Vec<String>)>,
    /// `colour_data_v2`: hue, saturation and value, the brightness in colour mode.
    pub colour: Option<String>,
}

/// An enum data point and its choices.
fn choices(point_id: String, point: &DataPoint) -> (String, Vec<String>) {
    let values = point.values.get("range").and_then(Value::as_array);
    let choices = values.map(|values| values.iter().filter_map(Value::as_str).map(str::to_string).collect());
    (point_id, choices.unwrap_or_default())
}

impl Model {
    /// The readings it reports, from its data points.
    pub fn reports(&self) -> Reports {
        Reports { temperature: self.temperature.is_some(), humidity: self.humidity.is_some() }
    }

    pub fn for_entry(entry: &DeviceEntry) -> Model {
        let mapping = if entry.mapping.is_empty() { fallback_mapping(&entry.category) } else { entry.mapping.clone() };
        let find = |codes: &[&str]| {
            codes
                .iter()
                .find_map(|code| mapping.iter().find(|(_, point)| point.code == *code))
                .map(|(point_id, point)| (point_id.clone(), point))
        };
        let range = |codes: &[&str]| find(codes).map(|(point_id, point)| Range::from(&point_id, point));
        let mut model = Model {
            switch: find(&["switch_led", "switch", "switch_1"]).map(|(point_id, _)| point_id),
            brightness: range(&["bright_value_v2", "bright_value"]),
            colour_temp: range(&["temp_value_v2", "temp_value"]),
            work_mode: find(&["work_mode"]).map(|(point_id, _)| point_id),
            power: range(&["cur_power"]),
            temperature: range(&["temp_current", "va_temperature", "temp_indoor"]),
            target: range(&["temp_set"]),
            humidity: range(&["va_humidity", "humidity_value", "humidity_indoor"]),
            mode: find(&["mode"]).map(|(point_id, point)| choices(point_id, point)),
            fan: find(&["fan_speed_enum", "fan_speed", "windspeed"]).map(|(point_id, point)| choices(point_id, point)),
            colour: find(&["colour_data_v2"]).map(|(point_id, _)| point_id),
            kind: Kind::Other,
        };
        model.kind = match entry.category.as_str() {
            "dj" | "dd" | "dc" | "fwd" | "xdd" | "gyd" | "tgq" | "tgkg" => Kind::Light,
            "cz" | "pc" | "kg" => Kind::Plug,
            "kt" | "ktkzq" | "wk" | "wkf" | "rs" | "qn" => Kind::Climate,
            "wsdcg" | "wnykq" => Kind::Sensor,
            _ if model.brightness.is_some() => Kind::Light,
            _ if model.target.is_some() => Kind::Climate,
            _ if model.temperature.is_some() || model.humidity.is_some() => Kind::Sensor,
            _ if model.switch.is_some() => Kind::Plug,
            _ => Kind::Other,
        };
        model
    }
}

/// Which of temperature and humidity a device reports, or which of them its
/// history shows.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Reports {
    pub temperature: bool,
    pub humidity: bool,
}

impl Reports {
    pub fn any(self) -> bool {
        self.temperature || self.humidity
    }

    /// What the history shows: the readings chosen, but never one the
    /// device does not report. Without a choice, or with one it cannot
    /// follow (its data points changed), everything it reports.
    pub fn shown(self, chosen: Option<Readings>) -> Reports {
        let wanted = match chosen {
            Some(Readings::Temperature) => Reports { temperature: true, humidity: false },
            Some(Readings::Humidity) => Reports { temperature: false, humidity: true },
            Some(Readings::Both) | None => return self,
        };
        let kept =
            Reports { temperature: self.temperature && wanted.temperature, humidity: self.humidity && wanted.humidity };
        if kept.any() { kept } else { self }
    }

    /// The choices Settings offers: only a device that reports both has any.
    pub fn choices(self) -> &'static [Readings] {
        if self.temperature && self.humidity { &Readings::ALL } else { &[] }
    }
}

/// Lights speak a well-known set of data points; others need the mapping.
fn fallback_mapping(category: &str) -> std::collections::BTreeMap<String, DataPoint> {
    let point = |code: &str, kind: &str, values: Value| DataPoint { code: code.into(), kind: kind.into(), values };
    let mut mapping = std::collections::BTreeMap::new();
    if category == "dj" {
        mapping.insert("20".into(), point("switch_led", "Boolean", Value::Null));
        mapping.insert(
            "21".into(),
            point("work_mode", "Enum", serde_json::json!({"range": ["white", "colour", "scene", "music"]})),
        );
        mapping.insert(
            "22".into(),
            point("bright_value_v2", "Integer", serde_json::json!({"min": 10, "max": 1000, "scale": 0, "step": 1})),
        );
        mapping.insert(
            "23".into(),
            point("temp_value_v2", "Integer", serde_json::json!({"min": 0, "max": 1000, "scale": 0, "step": 1})),
        );
    }
    mapping
}

/// A device's state, read from its data points.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct State {
    pub is_on: Option<bool>,
    /// Percent of the brightness range.
    pub brightness: Option<u32>,
    /// Percent from warm (0) to cool (100).
    pub colour_temp: Option<u32>,
    /// Watts.
    pub power: Option<f64>,
    pub temperature: Option<f64>,
    pub target: Option<f64>,
    /// Percent relative humidity.
    pub humidity: Option<f64>,
    pub mode: Option<String>,
    pub fan: Option<String>,
    /// A light's `work_mode`: "white", "colour", "scene" or "music".
    pub work_mode: Option<String>,
    /// A light's colour as hue (0 to 360), saturation and value (0 to 1000).
    pub colour: Option<(u32, u32, u32)>,
}

impl State {
    /// Whether a light shows a colour, where brightness is the colour's value.
    pub fn in_colour(&self) -> bool {
        self.work_mode.as_deref() == Some("colour") && self.colour.is_some()
    }

    /// Reads `dps`, keeping what a partial update does not mention.
    pub fn update(&mut self, model: &Model, dps: &Map<String, Value>) {
        fn raw<'a>(range: &'a Option<Range>, dps: &Map<String, Value>) -> Option<(&'a Range, i64)> {
            let range = range.as_ref()?;
            Some((range, dps.get(&range.point_id)?.as_i64()?))
        }
        if let Some(value) = model.switch.as_ref().and_then(|point_id| dps.get(point_id)).and_then(Value::as_bool) {
            self.is_on = Some(value);
        }
        if let Some((range, value)) = raw(&model.brightness, dps) {
            self.brightness = Some(range.percent(value).max(1));
        }
        if let Some((range, value)) = raw(&model.colour_temp, dps) {
            self.colour_temp = Some(range.percent(value));
        }
        if let Some((range, value)) = raw(&model.power, dps) {
            self.power = Some(range.real(value));
        }
        if let Some((range, value)) = raw(&model.temperature, dps) {
            self.temperature = Some(range.real(value));
        }
        if let Some((range, value)) = raw(&model.target, dps) {
            self.target = Some(range.real(value));
        }
        if let Some((range, value)) = raw(&model.humidity, dps) {
            self.humidity = Some(range.real(value));
        }
        if let Some(mode) = model.mode.as_ref().and_then(|(point_id, _)| dps.get(point_id)).and_then(Value::as_str) {
            self.mode = Some(mode.to_string());
        }
        if let Some(fan) = model.fan.as_ref().and_then(|(point_id, _)| dps.get(point_id)).and_then(Value::as_str) {
            self.fan = Some(fan.to_string());
        }
        if let Some(work_mode) = model.work_mode.as_ref().and_then(|point_id| dps.get(point_id)).and_then(Value::as_str)
        {
            self.work_mode = Some(work_mode.to_string());
        }
        if let Some(colour) = model.colour.as_ref().and_then(|point_id| dps.get(point_id)).and_then(Value::as_str) {
            self.colour = parse_colour(colour);
        }
    }

    /// The brightness to show: the white brightness, or in colour mode the
    /// colour's value.
    pub fn shown_brightness(&self) -> Option<u32> {
        match self.colour {
            Some((_, _, value)) if self.in_colour() => Some((value / 10).clamp(1, 100)),
            _ => self.brightness,
        }
    }
}

/// A change asked for from the panel.
#[derive(Debug, Clone, PartialEq)]
pub enum Change {
    Switch(bool),
    /// Percent of the brightness range, 1 to 100.
    Brightness(u32),
    /// Percent from warm (0) to cool (100).
    ColourTemp(u32),
    /// Degrees.
    Target(f64),
    Mode(String),
    Fan(String),
}

/// `colour_data_v2` is twelve hex digits (hue, saturation, value, four
/// each); some devices send JSON instead.
fn parse_colour(text: &str) -> Option<(u32, u32, u32)> {
    if text.starts_with('{') {
        let value: Value = serde_json::from_str(text).ok()?;
        let part = |name: &str| value.get(name).and_then(Value::as_u64).map(|found| found as u32);
        return Some((part("h")?, part("s")?, part("v")?));
    }
    if text.len() != 12 || !text.is_ascii() {
        return None;
    }
    let part = |start: usize| u32::from_str_radix(&text[start..start + 4], 16).ok();
    Some((part(0)?, part(4)?, part(8)?))
}

fn format_colour((hue, saturation, value): (u32, u32, u32)) -> String {
    format!("{:04x}{:04x}{:04x}", hue.min(360), saturation.min(1000), value.min(1000))
}

/// The data points to send for a change, or `None` if the device cannot.
pub fn data_points(model: &Model, state: &State, change: &Change) -> Option<Map<String, Value>> {
    let mut dps = Map::new();
    let white = |dps: &mut Map<String, Value>| {
        if let Some(point_id) = &model.work_mode {
            dps.insert(point_id.clone(), Value::from("white"));
        }
    };
    match change {
        Change::Switch(wanted) => {
            dps.insert(model.switch.clone()?, Value::from(*wanted));
        }
        Change::Brightness(percent) if state.in_colour() && model.colour.is_some() => {
            // Dimming a coloured light keeps its colour.
            let (hue, saturation, _) = state.colour?;
            let value = (percent.clamp(&1, &100) * 10).max(10);
            dps.insert(model.colour.clone()?, Value::from(format_colour((hue, saturation, value))));
        }
        Change::Brightness(percent) => {
            let range = model.brightness.as_ref()?;
            white(&mut dps);
            dps.insert(range.point_id.clone(), Value::from(range.at_percent((*percent).max(1))));
        }
        Change::ColourTemp(percent) => {
            let range = model.colour_temp.as_ref()?;
            white(&mut dps);
            dps.insert(range.point_id.clone(), Value::from(range.at_percent(*percent)));
        }
        Change::Target(degrees) => {
            let range = model.target.as_ref()?;
            dps.insert(range.point_id.clone(), Value::from(range.raw(*degrees)));
        }
        Change::Mode(mode) => {
            let (point_id, modes) = model.mode.as_ref()?;
            if !modes.is_empty() && !modes.contains(mode) {
                return None;
            }
            dps.insert(point_id.clone(), Value::from(mode.as_str()));
        }
        Change::Fan(speed) => {
            let (point_id, speeds) = model.fan.as_ref()?;
            if !speeds.is_empty() && !speeds.contains(speed) {
                return None;
            }
            dps.insert(point_id.clone(), Value::from(speed.as_str()));
        }
    }
    Some(dps)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(category: &str, points: &[(&str, &str, &str, Value)]) -> DeviceEntry {
        DeviceEntry {
            category: category.into(),
            mapping: points
                .iter()
                .map(|(point_id, code, kind, values)| {
                    (
                        (*point_id).into(),
                        DataPoint { code: (*code).into(), kind: (*kind).into(), values: values.clone() },
                    )
                })
                .collect(),
            ..Default::default()
        }
    }

    #[test]
    fn lights_without_a_mapping_use_the_standard_points() {
        let model = Model::for_entry(&DeviceEntry { category: "dj".into(), ..Default::default() });
        assert_eq!(model.kind, Kind::Light);
        assert_eq!(model.switch.as_deref(), Some("20"));
        let mut state = State::default();
        let dps = serde_json::json!({"20": true, "22": 505, "23": 0});
        state.update(&model, dps.as_object().expect("object"));
        assert_eq!((state.is_on, state.brightness, state.colour_temp), (Some(true), Some(50), Some(0)));
        let sent = data_points(&model, &State::default(), &Change::Brightness(100)).expect("dimmable");
        assert_eq!(serde_json::Value::Object(sent), serde_json::json!({"21": "white", "22": 1000}));
    }

    #[test]
    fn air_conditioners_read_scaled_temperatures() {
        let model = Model::for_entry(&entry(
            "kt",
            &[
                ("1", "switch", "Boolean", Value::Null),
                ("2", "temp_set", "Integer", serde_json::json!({"min": 160, "max": 300, "scale": 1, "step": 5})),
                ("3", "temp_current", "Integer", serde_json::json!({"min": -200, "max": 600, "scale": 1, "step": 1})),
                ("4", "mode", "Enum", serde_json::json!({"range": ["cold", "hot", "wet", "wind", "auto"]})),
            ],
        ));
        assert_eq!(model.kind, Kind::Climate);
        let mut state = State::default();
        let dps = serde_json::json!({"1": true, "2": 210, "3": 235, "4": "cold"});
        state.update(&model, dps.as_object().expect("object"));
        assert_eq!(state.target, Some(21.0));
        assert_eq!(state.temperature, Some(23.5));
        assert_eq!(state.mode.as_deref(), Some("cold"));
        // Half a degree steps; out of range is clamped.
        let sent = data_points(&model, &State::default(), &Change::Target(21.3)).expect("settable");
        assert_eq!(sent.get("2"), Some(&Value::from(215)));
        let sent = data_points(&model, &State::default(), &Change::Target(40.0)).expect("settable");
        assert_eq!(sent.get("2"), Some(&Value::from(300)));
        assert_eq!(data_points(&model, &State::default(), &Change::Mode("turbo".into())), None);
    }

    #[test]
    fn dimming_a_coloured_light_keeps_its_colour() {
        let model = Model::for_entry(&entry(
            "dj",
            &[
                ("20", "switch_led", "Boolean", Value::Null),
                ("21", "work_mode", "Enum", serde_json::json!({"range": ["white", "colour"]})),
                ("22", "bright_value_v2", "Integer", serde_json::json!({"min": 10, "max": 1000})),
                ("24", "colour_data_v2", "Json", Value::Null),
            ],
        ));
        let mut state = State::default();
        let dps = serde_json::json!({"20": true, "21": "colour", "22": 1000, "24": "00f003e801f4"});
        state.update(&model, dps.as_object().expect("object"));
        assert_eq!(state.colour, Some((240, 1000, 500)));
        assert_eq!(state.shown_brightness(), Some(50));
        let sent = data_points(&model, &state, &Change::Brightness(20)).expect("dimmable");
        assert_eq!(Value::Object(sent), serde_json::json!({"24": "00f003e800c8"}));
    }

    #[test]
    fn sensors_read_temperature_humidity_or_both() {
        let both = Model::for_entry(&entry(
            "wsdcg",
            &[
                ("1", "va_temperature", "Integer", serde_json::json!({"min": -200, "max": 600, "scale": 1})),
                ("2", "va_humidity", "Integer", serde_json::json!({"min": 0, "max": 100, "scale": 0})),
            ],
        ));
        assert_eq!(both.kind, Kind::Sensor);
        assert_eq!(both.reports(), Reports { temperature: true, humidity: true });
        let mut state = State::default();
        state.update(&both, serde_json::json!({"1": 214, "2": 48}).as_object().expect("object"));
        assert_eq!((state.temperature, state.humidity), (Some(21.4), Some(48.0)));
        // A report of one reading keeps the other.
        state.update(&both, serde_json::json!({"2": 51}).as_object().expect("object"));
        assert_eq!((state.temperature, state.humidity), (Some(21.4), Some(51.0)));

        // A humidity sensor of no known category, with a scaled reading.
        let humid = Model::for_entry(&entry(
            "xyz",
            &[("7", "humidity_value", "Integer", serde_json::json!({"min": 0, "max": 1000, "scale": 1}))],
        ));
        assert_eq!(humid.kind, Kind::Sensor);
        assert_eq!(humid.reports(), Reports { temperature: false, humidity: true });
        let mut state = State::default();
        state.update(&humid, serde_json::json!({"7": 455}).as_object().expect("object"));
        assert_eq!((state.temperature, state.humidity), (None, Some(45.5)));

        // A thermometer only.
        let thermometer = Model::for_entry(&entry(
            "wsdcg",
            &[("1", "temp_current", "Integer", serde_json::json!({"min": -400, "max": 1000, "scale": 1}))],
        ));
        assert_eq!(thermometer.reports(), Reports { temperature: true, humidity: false });
        // Lights report neither.
        let light = Model::for_entry(&DeviceEntry { category: "dj".into(), ..Default::default() });
        assert!(!light.reports().any());
    }

    #[test]
    fn the_history_shows_what_was_chosen_of_what_is_reported() {
        let both = Reports { temperature: true, humidity: true };
        let temperature = Reports { temperature: true, humidity: false };
        let humidity = Reports { temperature: false, humidity: true };
        // Without a choice, everything reported.
        assert_eq!(both.shown(None), both);
        assert_eq!(humidity.shown(None), humidity);
        assert_eq!(both.shown(Some(Readings::Temperature)), temperature);
        assert_eq!(both.shown(Some(Readings::Humidity)), humidity);
        assert_eq!(both.shown(Some(Readings::Both)), both);
        // Never a reading the device does not report; a choice it cannot
        // follow shows what it does report.
        assert_eq!(temperature.shown(Some(Readings::Both)), temperature);
        assert_eq!(temperature.shown(Some(Readings::Humidity)), temperature);
        assert_eq!(humidity.shown(Some(Readings::Temperature)), humidity);
        assert!(!Reports::default().shown(Some(Readings::Both)).any());
        // Only a device reporting both offers a choice.
        assert_eq!(both.choices(), Readings::ALL);
        assert!(temperature.choices().is_empty());
        assert!(humidity.choices().is_empty());
    }

    #[test]
    fn partial_updates_keep_the_rest() {
        let model = Model::for_entry(&DeviceEntry { category: "dj".into(), ..Default::default() });
        let mut state = State::default();
        state.update(&model, serde_json::json!({"20": true, "22": 1000}).as_object().expect("object"));
        state.update(&model, serde_json::json!({"20": false}).as_object().expect("object"));
        assert_eq!((state.is_on, state.brightness), (Some(false), Some(100)));
    }
}
