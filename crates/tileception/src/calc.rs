//! The launcher's calculator: sums (`12*7+3`, `2^10`, `15% of 80`), number
//! bases (`0xff`, `255 in hex`) and offline unit conversions (`5 km in mi`,
//! `30 c in f`, `2 GiB in MB`, `5 ft 10 in in cm`, `180 cm in ft and in`,
//! `how many ml in a cup`). Everything is local and instant.

/// What a sum or conversion comes to.
#[derive(Debug, Clone, PartialEq)]
pub struct Answer {
    /// What Enter copies: the bare result, such as `87` or `3.10686`.
    pub copy: String,
    /// What the row shows: the result with its unit or base, such as `3.10686 mi`.
    pub shown: String,
}

/// The answer to `input`, when it is a sum, a base conversion or a unit
/// conversion. A plain number on its own is not a sum.
pub fn answer(input: &str) -> Option<Answer> {
    let input = plain(input);
    if input.is_empty() {
        return None;
    }
    if let Some(answer) = convert_units(input) {
        return Some(answer);
    }
    if let Some((expr, base)) = split_conversion(input)
        && let Some(base) = Base::named(base)
    {
        let value = Parser::new(expr).parse()?;
        return base.format(value.value);
    }
    let value = Parser::new(input).parse()?;
    // "12" or "3.5" is just what was typed; "0xff" or "1e3" is worth showing.
    if !value.worked {
        return None;
    }
    let shown = number(value.value)?;
    Some(Answer { copy: shown.clone(), shown })
}

/// What was asked, without the words around it: `convert 5 ft to cm`,
/// `what is 2+2?`.
fn plain(input: &str) -> &str {
    let input = input.trim().trim_end_matches('?').trim_end();
    ["convert ", "what is ", "what's "]
        .iter()
        .find_map(|prefix| strip_prefix_ignoring_case(input, prefix))
        .map_or(input, str::trim_start)
}

fn strip_prefix_ignoring_case<'text>(text: &'text str, prefix: &str) -> Option<&'text str> {
    text.get(..prefix.len()).filter(|head| head.eq_ignore_ascii_case(prefix)).map(|_| &text[prefix.len()..])
}

/// `<left> in <right>` or `<left> to <right>`, at the last such word
/// following a complete quantity or expression, so target inches stay units.
fn split_conversion(input: &str) -> Option<(&str, &str)> {
    let lower = input.to_ascii_lowercase();
    lower
        .char_indices()
        .rev()
        .filter_map(|(position, _)| {
            [" in ", " to ", " as ", " into "]
                .iter()
                .find(|word| lower[position..].starts_with(**word))
                .map(|word| (position, word.len()))
        })
        .map(|(position, len)| (input[..position].trim(), input[position + len..].trim()))
        .find(|(left, right)| !right.is_empty() && (quantity(left).is_some() || Parser::new(left).parse().is_some()))
}

#[derive(Debug, Clone, Copy)]
enum Base {
    Hex,
    Binary,
    Octal,
    Decimal,
}

impl Base {
    fn named(name: &str) -> Option<Base> {
        match name.to_ascii_lowercase().as_str() {
            "hex" | "hexadecimal" => Some(Base::Hex),
            "bin" | "binary" => Some(Base::Binary),
            "oct" | "octal" => Some(Base::Octal),
            "dec" | "decimal" => Some(Base::Decimal),
            _ => None,
        }
    }

    fn format(self, value: f64) -> Option<Answer> {
        if value.fract() != 0.0 || value.abs() >= 2f64.powi(63) {
            return None;
        }
        let integer = value as i64;
        let (sign, magnitude) = (if integer < 0 { "-" } else { "" }, integer.unsigned_abs());
        let text = match self {
            Base::Hex => format!("{sign}0x{magnitude:x}"),
            Base::Binary => format!("{sign}0b{magnitude:b}"),
            Base::Octal => format!("{sign}0o{magnitude:o}"),
            Base::Decimal => format!("{sign}{magnitude}"),
        };
        Some(Answer { copy: text.clone(), shown: text })
    }
}

/// A result shown with up to ten significant digits, without trailing zeros,
/// and in scientific notation only when very large or very small.
pub fn number(value: f64) -> Option<String> {
    if !value.is_finite() {
        return None;
    }
    if value == 0.0 {
        return Some("0".into());
    }
    let magnitude = value.abs();
    if !(1e-6..1e15).contains(&magnitude) {
        let text = format!("{value:.9e}");
        let (mantissa, exponent) = text.split_once('e')?;
        let mantissa = mantissa.trim_end_matches('0').trim_end_matches('.');
        return Some(format!("{mantissa}e{exponent}"));
    }
    let decimals = (9 - magnitude.log10().floor() as i32).clamp(0, 15) as usize;
    let text = format!("{value:.decimals$}");
    let text = if text.contains('.') { text.trim_end_matches('0').trim_end_matches('.') } else { &text };
    Some(if text == "-0" { "0".into() } else { text.to_string() })
}

struct Value {
    value: f64,
    /// Whether anything happened beyond reading a decimal number: an
    /// operator, a function, or a hex, binary or exponent literal.
    worked: bool,
}

/// A recursive-descent parser over the expression grammar:
///
/// ```text
/// sum     = product (("+" | "-") product)*
/// product = power (("*" | "/" | "mod") power)*
/// power   = unary ("^" power)?
/// unary   = "-" unary | "+" unary | postfix
/// postfix = atom "%"* ("of" power)?
/// atom    = number | constant | function "(" sum ")" | "(" sum ")"
/// ```
struct Parser<'a> {
    text: &'a str,
    cursor: usize,
    worked: bool,
}

impl<'a> Parser<'a> {
    fn new(text: &'a str) -> Parser<'a> {
        Parser { text, cursor: 0, worked: false }
    }

    fn parse(mut self) -> Option<Value> {
        let value = self.sum()?;
        self.skip_space();
        (self.cursor == self.text.len() && value.is_finite()).then_some(Value { value, worked: self.worked })
    }

    fn rest(&self) -> &'a str {
        &self.text[self.cursor..]
    }

    fn skip_space(&mut self) {
        let rest = self.rest();
        self.cursor += rest.len() - rest.trim_start().len();
    }

    /// Takes `token` (case-insensitive) when it comes next.
    fn eat(&mut self, token: &str) -> bool {
        self.skip_space();
        let rest = self.rest();
        if rest.get(..token.len()).is_some_and(|head| head.eq_ignore_ascii_case(token)) {
            // A word must end there: "mod" is not the start of "modern".
            let word = token.chars().all(|character| character.is_ascii_alphabetic());
            if word && rest[token.len()..].starts_with(|character: char| character.is_ascii_alphanumeric()) {
                return false;
            }
            self.cursor += token.len();
            return true;
        }
        false
    }

    fn sum(&mut self) -> Option<f64> {
        let mut value = self.product()?;
        loop {
            if self.eat("+") {
                value += self.product()?;
            } else if self.eat("-") {
                value -= self.product()?;
            } else {
                return Some(value);
            }
            self.worked = true;
        }
    }

    fn product(&mut self) -> Option<f64> {
        let mut value = self.power()?;
        loop {
            if self.eat("*") || self.eat("×") {
                value *= self.power()?;
            } else if self.eat("/") || self.eat("÷") {
                value /= self.power()?;
            } else if self.eat("mod") {
                value = value.rem_euclid(self.power()?);
            } else {
                return Some(value);
            }
            self.worked = true;
        }
    }

    fn power(&mut self) -> Option<f64> {
        let base = self.unary()?;
        if self.eat("^") || self.eat("**") {
            self.worked = true;
            return Some(base.powf(self.power()?));
        }
        Some(base)
    }

    fn unary(&mut self) -> Option<f64> {
        if self.eat("-") {
            return Some(-self.unary()?);
        }
        if self.eat("+") {
            return self.unary();
        }
        self.postfix()
    }

    fn postfix(&mut self) -> Option<f64> {
        let mut value = self.atom()?;
        while self.eat("%") {
            value /= 100.0;
            self.worked = true;
        }
        // "15% of 80"; "of" also reads as times without the percent sign.
        // What follows is a whole power, so "50% of 2^3" is half of eight.
        if self.eat("of") {
            self.worked = true;
            value *= self.power()?;
        }
        Some(value)
    }

    fn atom(&mut self) -> Option<f64> {
        self.skip_space();
        if self.eat("(") {
            let value = self.sum()?;
            return self.eat(")").then_some(value);
        }
        for (name, function) in FUNCTIONS {
            let start = self.cursor;
            if self.eat(name) {
                if self.eat("(") {
                    let value = self.sum()?;
                    if self.eat(")") {
                        self.worked = true;
                        return Some(function(value));
                    }
                    return None;
                }
                self.cursor = start;
            }
        }
        // A constant on its own ("e", "pi") is not a sum: typing "e" searches.
        for (name, value) in [("pi", std::f64::consts::PI), ("π", std::f64::consts::PI), ("e", std::f64::consts::E)] {
            if self.eat(name) {
                return Some(value);
            }
        }
        self.literal()
    }

    fn literal(&mut self) -> Option<f64> {
        let rest = self.rest();
        let radix = [("0x", 16), ("0b", 2), ("0o", 8)]
            .into_iter()
            .find(|(prefix, _)| rest.len() > 2 && rest.get(..2).is_some_and(|head| head.eq_ignore_ascii_case(prefix)));
        if let Some((_, radix)) = radix {
            let digits: String = rest[2..]
                .chars()
                .take_while(|character| character.is_ascii_alphanumeric() || *character == '_')
                .collect();
            let value = u64::from_str_radix(&digits.replace('_', ""), radix).ok()?;
            self.cursor += 2 + digits.len();
            self.worked = true;
            return Some(value as f64);
        }
        let mut end = 0;
        let bytes = rest.as_bytes();
        while end < bytes.len() && (bytes[end].is_ascii_digit() || bytes[end] == b'.' || bytes[end] == b'_') {
            end += 1;
        }
        // An exponent: "1e3", "2.5E-4".
        if end > 0 && end < bytes.len() && matches!(bytes[end], b'e' | b'E') {
            let mut exp = end + 1;
            if exp < bytes.len() && matches!(bytes[exp], b'+' | b'-') {
                exp += 1;
            }
            let digits = exp;
            while exp < bytes.len() && bytes[exp].is_ascii_digit() {
                exp += 1;
            }
            if exp > digits {
                end = exp;
                self.worked = true;
            }
        }
        if end == 0 {
            return None;
        }
        let value = rest[..end].replace('_', "").parse::<f64>().ok()?;
        self.cursor += end;
        Some(value)
    }
}

type Function = fn(f64) -> f64;

/// Longer names first, so "sqrt" is not read as something shorter.
/// Trigonometry works in degrees.
const FUNCTIONS: [(&str, Function); 12] = [
    ("sqrt", f64::sqrt),
    ("cbrt", f64::cbrt),
    ("round", f64::round),
    ("floor", f64::floor),
    ("ceil", f64::ceil),
    ("abs", f64::abs),
    ("sin", |degrees| degrees.to_radians().sin()),
    ("cos", |degrees| degrees.to_radians().cos()),
    ("tan", |degrees| degrees.to_radians().tan()),
    ("log", f64::log10),
    ("ln", f64::ln),
    ("exp", f64::exp),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Dimension {
    Length,
    Mass,
    Temperature,
    Data,
    Time,
    Speed,
    Volume,
    Area,
    Energy,
    Power,
    Pressure,
    Angle,
    Frequency,
}

/// A unit: what it measures, and its size in that dimension's base unit
/// (metres, kilograms, kelvin, bytes, seconds, metres per second, litres,
/// square metres, joules, watts, pascals, degrees, hertz).
/// Temperatures also have an offset: kelvin = value * factor + offset.
struct Unit {
    names: &'static [&'static str],
    /// The name results are shown with.
    symbol: &'static str,
    dimension: Dimension,
    factor: f64,
    offset: f64,
    /// Short names are matched with exact case only, where case changes the
    /// meaning (MB is bytes, Mb is bits; MW is mega, mW is milli).
    exact_case: bool,
}

/// Names up to this long are symbols, which an `exact_case` unit matches by
/// case; longer ones are words ("Megawatts"), which mean the same either way.
const LONGEST_SYMBOL: usize = 3;

const fn unit(names: &'static [&'static str], symbol: &'static str, dimension: Dimension, factor: f64) -> Unit {
    Unit { names, symbol, dimension, factor, offset: 0.0, exact_case: false }
}

/// A unit whose symbol means something else in another case.
const fn cased(names: &'static [&'static str], symbol: &'static str, dimension: Dimension, factor: f64) -> Unit {
    Unit { names, symbol, dimension, factor, offset: 0.0, exact_case: true }
}

const fn data(names: &'static [&'static str], symbol: &'static str, factor: f64) -> Unit {
    Unit { names, symbol, dimension: Dimension::Data, factor, offset: 0.0, exact_case: true }
}

const KIB: f64 = 1024.0;
const DAY: f64 = 86_400.0;

const UNITS: &[Unit] = &[
    unit(&["nm", "nanometre", "nanometres", "nanometer", "nanometers"], "nm", Dimension::Length, 1e-9),
    unit(&["µm", "um", "micrometre", "micrometres", "micron", "microns"], "µm", Dimension::Length, 1e-6),
    unit(&["mm", "millimetre", "millimetres", "millimeter", "millimeters"], "mm", Dimension::Length, 1e-3),
    unit(&["cm", "centimetre", "centimetres", "centimeter", "centimeters"], "cm", Dimension::Length, 1e-2),
    unit(&["dm", "decimetre", "decimetres", "decimeter", "decimeters"], "dm", Dimension::Length, 1e-1),
    unit(&["m", "metre", "metres", "meter", "meters"], "m", Dimension::Length, 1.0),
    unit(&["km", "kilometre", "kilometres", "kilometer", "kilometers"], "km", Dimension::Length, 1e3),
    unit(&["in", "inch", "inches", "\""], "in", Dimension::Length, 0.0254),
    unit(&["ft", "foot", "feet", "'"], "ft", Dimension::Length, 0.3048),
    unit(&["yd", "yard", "yards"], "yd", Dimension::Length, 0.9144),
    unit(&["mi", "mile", "miles"], "mi", Dimension::Length, 1609.344),
    unit(&["nmi", "nautical mile", "nautical miles"], "nmi", Dimension::Length, 1852.0),
    unit(
        &["ly", "light year", "light years", "lightyear", "lightyears"],
        "ly",
        Dimension::Length,
        9_460_730_472_580_800.0,
    ),
    unit(&["µg", "ug", "mcg", "microgram", "micrograms"], "µg", Dimension::Mass, 1e-9),
    unit(&["mg", "milligram", "milligrams"], "mg", Dimension::Mass, 1e-6),
    unit(&["g", "gram", "grams"], "g", Dimension::Mass, 1e-3),
    unit(&["kg", "kilo", "kilos", "kilogram", "kilograms"], "kg", Dimension::Mass, 1.0),
    unit(&["t", "tonne", "tonnes", "ton", "tons"], "t", Dimension::Mass, 1e3),
    unit(&["oz", "ounce", "ounces"], "oz", Dimension::Mass, 0.028_349_523_125),
    unit(&["lb", "lbs", "pound", "pounds"], "lb", Dimension::Mass, 0.453_592_37),
    unit(&["st", "stone", "stones"], "st", Dimension::Mass, 6.350_293_18),
    Unit {
        names: &["c", "°c", "celsius", "degc", "deg c", "degree c", "degrees c", "degree celsius", "degrees celsius"],
        symbol: "°C",
        dimension: Dimension::Temperature,
        factor: 1.0,
        offset: 273.15,
        exact_case: false,
    },
    Unit {
        names: &[
            "f",
            "°f",
            "fahrenheit",
            "degf",
            "deg f",
            "degree f",
            "degrees f",
            "degree fahrenheit",
            "degrees fahrenheit",
        ],
        symbol: "°F",
        dimension: Dimension::Temperature,
        factor: 5.0 / 9.0,
        offset: 273.15 - 32.0 * 5.0 / 9.0,
        exact_case: false,
    },
    unit(&["k", "kelvin", "kelvins"], "K", Dimension::Temperature, 1.0),
    data(&["b", "bit", "bits"], "bit", 1.0 / 8.0),
    data(&["B", "byte", "bytes"], "B", 1.0),
    data(&["kb", "Kb", "kbit", "kilobit", "kilobits"], "kbit", 1e3 / 8.0),
    data(&["kB", "KB", "kilobyte", "kilobytes"], "kB", 1e3),
    data(&["Mb", "mbit", "Mbit", "megabit", "megabits"], "Mbit", 1e6 / 8.0),
    data(&["MB", "megabyte", "megabytes"], "MB", 1e6),
    data(&["Gb", "gbit", "Gbit", "gigabit", "gigabits"], "Gbit", 1e9 / 8.0),
    data(&["GB", "gigabyte", "gigabytes"], "GB", 1e9),
    data(&["TB", "terabyte", "terabytes"], "TB", 1e12),
    data(&["PB", "petabyte", "petabytes"], "PB", 1e15),
    data(&["KiB", "kib", "kibibyte", "kibibytes"], "KiB", KIB),
    data(&["MiB", "mib", "mebibyte", "mebibytes"], "MiB", KIB * KIB),
    data(&["GiB", "gib", "gibibyte", "gibibytes"], "GiB", KIB * KIB * KIB),
    data(&["TiB", "tib", "tebibyte", "tebibytes"], "TiB", KIB * KIB * KIB * KIB),
    unit(&["ns", "nanosecond", "nanoseconds"], "ns", Dimension::Time, 1e-9),
    unit(&["µs", "us", "microsecond", "microseconds"], "µs", Dimension::Time, 1e-6),
    unit(&["ms", "millisecond", "milliseconds"], "ms", Dimension::Time, 1e-3),
    unit(&["s", "sec", "secs", "second", "seconds"], "s", Dimension::Time, 1.0),
    unit(&["min", "mins", "minute", "minutes"], "min", Dimension::Time, 60.0),
    unit(&["h", "hr", "hrs", "hour", "hours"], "h", Dimension::Time, 3600.0),
    unit(&["d", "day", "days"], "days", Dimension::Time, DAY),
    unit(&["wk", "week", "weeks"], "weeks", Dimension::Time, 7.0 * DAY),
    unit(&["month", "months"], "months", Dimension::Time, 30.436_875 * DAY),
    unit(&["yr", "yrs", "year", "years"], "years", Dimension::Time, 365.2425 * DAY),
    unit(&["decade", "decades"], "decades", Dimension::Time, 3652.425 * DAY),
    unit(&["century", "centuries"], "centuries", Dimension::Time, 36_524.25 * DAY),
    unit(&["m/s", "mps", "metres per second", "meters per second"], "m/s", Dimension::Speed, 1.0),
    unit(
        &["km/h", "kmh", "kph", "kmph", "kilometres per hour", "kilometers per hour"],
        "km/h",
        Dimension::Speed,
        1.0 / 3.6,
    ),
    unit(&["km/s", "kilometres per second", "kilometers per second"], "km/s", Dimension::Speed, 1e3),
    unit(&["mph", "mi/h", "miles per hour"], "mph", Dimension::Speed, 0.44704),
    unit(&["ft/s", "fps", "feet per second"], "ft/s", Dimension::Speed, 0.3048),
    unit(&["kn", "kt", "knot", "knots"], "kn", Dimension::Speed, 1852.0 / 3600.0),
    unit(&["ml", "mL", "millilitre", "millilitres", "milliliter", "milliliters"], "ml", Dimension::Volume, 1e-3),
    unit(&["cl", "cL", "centilitre", "centilitres", "centiliter", "centiliters"], "cl", Dimension::Volume, 1e-2),
    unit(&["dl", "dL", "decilitre", "decilitres", "deciliter", "deciliters"], "dl", Dimension::Volume, 1e-1),
    unit(&["l", "L", "litre", "litres", "liter", "liters"], "l", Dimension::Volume, 1.0),
    unit(&["m3", "m³", "cubic metre", "cubic metres", "cubic meter", "cubic meters"], "m³", Dimension::Volume, 1e3),
    unit(
        &["cm3", "cm³", "cc", "cubic centimetre", "cubic centimetres", "cubic centimeter", "cubic centimeters"],
        "cm³",
        Dimension::Volume,
        1e-3,
    ),
    unit(&["ft3", "ft³", "cu ft", "cubic foot", "cubic feet"], "ft³", Dimension::Volume, 28.316_846_592),
    unit(&["in3", "in³", "cu in", "cubic inch", "cubic inches"], "in³", Dimension::Volume, 0.016_387_064),
    unit(&["gal", "gallon", "gallons"], "gal", Dimension::Volume, 3.785_411_784),
    unit(&["qt", "quart", "quarts"], "qt", Dimension::Volume, 0.946_352_946),
    unit(&["pt", "pint", "pints"], "pt", Dimension::Volume, 0.473_176_473),
    unit(
        &["imp gal", "uk gal", "imperial gallon", "imperial gallons", "uk gallon", "uk gallons"],
        "imp gal",
        Dimension::Volume,
        4.546_09,
    ),
    unit(
        &["imp pt", "uk pt", "imperial pint", "imperial pints", "uk pint", "uk pints"],
        "imp pt",
        Dimension::Volume,
        0.568_261_25,
    ),
    unit(&["cup", "cups"], "cups", Dimension::Volume, 0.236_588_236_5),
    unit(&["floz", "fl oz", "fluid ounce", "fluid ounces"], "fl oz", Dimension::Volume, 0.029_573_529_562_5),
    unit(&["tbsp", "tablespoon", "tablespoons"], "tbsp", Dimension::Volume, 0.014_786_764_781_25),
    unit(&["tsp", "teaspoon", "teaspoons"], "tsp", Dimension::Volume, 0.004_928_921_593_75),
    unit(
        &["mm2", "mm²", "sq mm", "square millimetre", "square millimetres", "square millimeter", "square millimeters"],
        "mm²",
        Dimension::Area,
        1e-6,
    ),
    unit(
        &["cm2", "cm²", "sq cm", "square centimetre", "square centimetres", "square centimeter", "square centimeters"],
        "cm²",
        Dimension::Area,
        1e-4,
    ),
    unit(
        &["m2", "m²", "sq m", "square metre", "square metres", "square meter", "square meters"],
        "m²",
        Dimension::Area,
        1.0,
    ),
    unit(&["ha", "hectare", "hectares"], "ha", Dimension::Area, 1e4),
    unit(
        &["km2", "km²", "sq km", "square kilometre", "square kilometres", "square kilometer", "square kilometers"],
        "km²",
        Dimension::Area,
        1e6,
    ),
    unit(&["in2", "in²", "sq in", "square inch", "square inches"], "in²", Dimension::Area, 0.000_645_16),
    unit(&["ft2", "ft²", "sq ft", "sqft", "square foot", "square feet"], "ft²", Dimension::Area, 0.092_903_04),
    unit(&["yd2", "yd²", "sq yd", "square yard", "square yards"], "yd²", Dimension::Area, 0.836_127_36),
    unit(&["ac", "acre", "acres"], "acres", Dimension::Area, 4_046.856_422_4),
    unit(&["mi2", "mi²", "sq mi", "square mile", "square miles"], "mi²", Dimension::Area, 2_589_988.110_336),
    unit(&["j", "joule", "joules"], "J", Dimension::Energy, 1.0),
    unit(&["kj", "kilojoule", "kilojoules"], "kJ", Dimension::Energy, 1e3),
    cased(&["mJ", "millijoule", "millijoules"], "mJ", Dimension::Energy, 1e-3),
    cased(&["MJ", "megajoule", "megajoules"], "MJ", Dimension::Energy, 1e6),
    unit(&["cal", "calorie", "calories"], "cal", Dimension::Energy, 4.184),
    unit(&["kcal", "kilocalorie", "kilocalories"], "kcal", Dimension::Energy, 4184.0),
    unit(&["wh", "watt hour", "watt hours"], "Wh", Dimension::Energy, 3600.0),
    unit(&["kwh", "kilowatt hour", "kilowatt hours"], "kWh", Dimension::Energy, 3.6e6),
    unit(&["btu"], "BTU", Dimension::Energy, 1_055.055_852_62),
    unit(&["ev", "electronvolt", "electronvolts"], "eV", Dimension::Energy, 1.602_176_634e-19),
    unit(&["w", "watt", "watts"], "W", Dimension::Power, 1.0),
    unit(&["kw", "kilowatt", "kilowatts"], "kW", Dimension::Power, 1e3),
    cased(&["mW", "milliwatt", "milliwatts"], "mW", Dimension::Power, 1e-3),
    cased(&["MW", "megawatt", "megawatts"], "MW", Dimension::Power, 1e6),
    unit(&["hp", "horsepower"], "hp", Dimension::Power, 745.699_871_582_270_2),
    unit(&["pa", "pascal", "pascals"], "Pa", Dimension::Pressure, 1.0),
    unit(&["hpa", "hectopascal", "hectopascals"], "hPa", Dimension::Pressure, 1e2),
    unit(&["kpa", "kilopascal", "kilopascals"], "kPa", Dimension::Pressure, 1e3),
    cased(&["MPa", "megapascal", "megapascals"], "MPa", Dimension::Pressure, 1e6),
    unit(&["mbar", "millibar", "millibars"], "mbar", Dimension::Pressure, 1e2),
    unit(&["bar", "bars"], "bar", Dimension::Pressure, 1e5),
    unit(&["atm", "atmosphere", "atmospheres"], "atm", Dimension::Pressure, 101_325.0),
    unit(&["psi"], "psi", Dimension::Pressure, 6_894.757_293_168),
    unit(&["mmhg", "torr"], "mmHg", Dimension::Pressure, 133.322_387_415),
    unit(&["deg", "°", "degree", "degrees"], "°", Dimension::Angle, 1.0),
    unit(&["rad", "radian", "radians"], "rad", Dimension::Angle, 180.0 / std::f64::consts::PI),
    unit(&["grad", "gon", "gradian", "gradians"], "grad", Dimension::Angle, 0.9),
    unit(&["turn", "turns", "rev", "revolution", "revolutions"], "turns", Dimension::Angle, 360.0),
    unit(&["hz", "hertz"], "Hz", Dimension::Frequency, 1.0),
    unit(&["khz", "kilohertz"], "kHz", Dimension::Frequency, 1e3),
    cased(&["MHz", "megahertz"], "MHz", Dimension::Frequency, 1e6),
    unit(&["ghz", "gigahertz"], "GHz", Dimension::Frequency, 1e9),
    unit(&["rpm"], "rpm", Dimension::Frequency, 1.0 / 60.0),
];

fn find_unit(name: &str) -> Option<&'static Unit> {
    let name = name.trim();
    // Exact spelling first, so "Mb" (bits) and "MB" (bytes) stay apart.
    UNITS.iter().find(|candidate| candidate.names.contains(&name)).or_else(|| {
        let lower = name.to_lowercase();
        UNITS.iter().find(|candidate| {
            candidate.names.iter().any(|alias| {
                (!candidate.exact_case || alias.chars().count() > LONGEST_SYMBOL) && alias.to_lowercase() == lower
            })
        })
    })
}

/// `<amount> <unit> in|to <unit>`, where the amount may be a sum (`2*3 km`)
/// or parts that add up (`5 ft 10 in`), and the target may be parts too
/// (`ft and in`). `how many <unit> in <amount> <unit>` asks the same.
fn convert_units(input: &str) -> Option<Answer> {
    let (left, target) = how_many(input).or_else(|| split_conversion(input))?;
    let (base, dimension) = quantity(left)?;
    if let Some(target) = find_unit(target) {
        if target.dimension != dimension {
            return None;
        }
        let value = (base - target.offset) / target.factor;
        let copy = number(value)?;
        let shown = format!("{copy} {}", symbol(target, &copy));
        return Some(Answer { copy, shown });
    }
    let targets = units_in(target)?;
    let fits = targets.len() > 1
        && dimension != Dimension::Temperature
        && targets.iter().all(|unit| unit.dimension == dimension)
        && targets.windows(2).all(|pair| pair[0].factor > pair[1].factor);
    if !fits {
        return None;
    }
    let shown = in_parts(base, &targets)?;
    Some(Answer { copy: shown.clone(), shown })
}

/// `how many <unit> in <amount> <unit>`, as the amount and the target.
fn how_many(input: &str) -> Option<(&str, &str)> {
    let rest = strip_prefix_ignoring_case(input, "how many ")?;
    let lower = rest.to_ascii_lowercase();
    let (position, len) = [" are in ", " is in ", " in "]
        .iter()
        .find_map(|word| lower.find(word).map(|position| (position, word.len())))?;
    Some((rest[position + len..].trim(), rest[..position].trim()))
}

/// An amount and its unit, as a value in its dimension's base unit: `5 km`,
/// `2*3 m`, `a foot`, or parts that add up: `5 ft 10 in`, `1 h 30 min`,
/// `5 feet and 10 inches`, `5'10"`.
fn quantity(text: &str) -> Option<(f64, Dimension)> {
    if let Some((base, unit)) = one_quantity(text) {
        return Some((base, unit.dimension));
    }
    // The first part ends where the next amount starts.
    text.char_indices()
        .filter(|&(split, character)| {
            split > 0
                && (character.is_ascii_digit()
                    || (character == '.' && text[split + 1..].starts_with(|next: char| next.is_ascii_digit())))
                && !text[..split].ends_with(|before: char| before.is_ascii_digit() || before == '.' || before == '_')
        })
        .find_map(|(split, _)| {
            let first = text[..split].trim_end();
            let first = first.strip_suffix(',').or_else(|| first.strip_suffix(" and")).unwrap_or(first);
            let (base, unit) = one_quantity(first)?;
            let (rest, dimension) = quantity(&text[split..])?;
            // A minus in front is for the whole amount: -5 ft 3 in is -(5 ft 3 in).
            let negative = base < 0.0 || (base == 0.0 && first.trim_start().starts_with('-'));
            let total = if negative { base - rest } else { base + rest };
            (unit.dimension == dimension && dimension != Dimension::Temperature).then_some((total, dimension))
        })
}

/// One amount and its unit: whatever follows the amount, the longest tail
/// that names a unit.
fn one_quantity(text: &str) -> Option<(f64, &'static Unit)> {
    text.char_indices().filter(|(split, _)| *split > 0).find_map(|(split, _)| {
        let unit = find_unit(&text[split..])?;
        let amount = text[..split].trim();
        let amount = if ["a", "an", "one"].iter().any(|word| amount.eq_ignore_ascii_case(word)) {
            // A word of its own: "a m", not the start of "am".
            if !text[split..].starts_with(char::is_whitespace) {
                return None;
            }
            1.0
        } else {
            Parser::new(amount).parse()?.value
        };
        Some((amount * unit.factor + unit.offset, unit))
    })
}

/// The units named one after another: `ft in`, `feet and inches`, `h, min`.
fn units_in(text: &str) -> Option<Vec<&'static Unit>> {
    let words: Vec<&str> = text
        .split(|character: char| character.is_whitespace() || character == ',')
        .filter(|word| !word.is_empty() && !word.eq_ignore_ascii_case("and"))
        .collect();
    let mut units = Vec::new();
    let mut start = 0;
    while start < words.len() {
        // The most words that still name a unit, for "fl oz" or "sq ft".
        let (end, unit) =
            (start + 1..=words.len()).rev().find_map(|end| Some((end, find_unit(&words[start..end].join(" "))?)))?;
        units.push(unit);
        start = end;
    }
    (!units.is_empty()).then_some(units)
}

/// `base` in whole larger units and what is left in the smallest, to two
/// decimals: `5 ft 10.87 in`. Larger units that come to nothing are left out.
fn in_parts(base: f64, units: &[&'static Unit]) -> Option<String> {
    let (smallest, larger) = units.split_last()?;
    let hundredths = |value: f64| (value * 100.0).round() / 100.0;
    // Round once, in the smallest unit, so 72 in is 6 ft 0 in, not 5 ft 12 in.
    let total = hundredths(base / smallest.factor);
    let sign = if total < 0.0 { "-" } else { "" };
    let mut rest = total.abs();
    let mut parts = Vec::new();
    for unit in larger {
        let size = unit.factor / smallest.factor;
        let count = (rest / size + 1e-9).floor();
        rest = hundredths((rest - count * size).max(0.0));
        if count > 0.0 || !parts.is_empty() {
            let count = number(count)?;
            parts.push(format!("{count} {}", symbol(unit, &count)));
        }
    }
    let rest = number(rest)?;
    parts.push(format!("{rest} {}", symbol(smallest, &rest)));
    Some(format!("{sign}{}", parts.join(" ")))
}

/// The name a result shown as `amount` goes with: 1 day, 2 days.
fn symbol(unit: &Unit, amount: &str) -> &'static str {
    if amount != "1" {
        return unit.symbol;
    }
    let singular = |name: &str| match unit.symbol.strip_suffix("ies") {
        Some(stem) => name.strip_suffix('y') == Some(stem),
        None => unit.symbol.strip_suffix('s') == Some(name),
    };
    unit.names.iter().copied().find(|name| singular(name)).unwrap_or(unit.symbol)
}

/// Whether `input` reads as a unit or base conversion, even an unfinished
/// one, so it is not mistaken for a question.
pub fn looks_like_conversion(input: &str) -> bool {
    split_conversion(plain(input)).is_some_and(|(left, right)| {
        left.starts_with(|character: char| {
            character.is_ascii_digit() || character == '-' || character == '.' || character == '('
        }) && (find_unit(right).is_some() || units_in(right).is_some() || Base::named(right).is_some())
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test setup and assertions should fail immediately on unexpected errors")]
mod tests {
    use super::*;

    fn shown(input: &str) -> Option<String> {
        answer(input).map(|result| result.shown)
    }

    fn close(input: &str, expected: f64) {
        let got: f64 = answer(input).unwrap_or_else(|| panic!("{input} has no answer")).copy.parse().unwrap();
        assert!((got - expected).abs() <= expected.abs() * 1e-6 + 1e-9, "{input}: {got} is not {expected}");
    }

    #[test]
    fn sums() {
        assert_eq!(shown("12*7+3"), Some("87".into()));
        assert_eq!(shown("2^10"), Some("1024".into()));
        assert_eq!(shown("2 ^ 3 ^ 2"), Some("512".into()), "powers group to the right");
        assert_eq!(shown("(1 + 2) * 3"), Some("9".into()));
        assert_eq!(shown("-3 + 5"), Some("2".into()));
        assert_eq!(shown("7 / 2"), Some("3.5".into()));
        assert_eq!(shown("10 mod 3"), Some("1".into()));
        assert_eq!(shown("1/3"), Some("0.3333333333".into()));
        assert_eq!(shown("0.1 + 0.2"), Some("0.3".into()));
        assert_eq!(shown("2 × 3 ÷ 4"), Some("1.5".into()));
        assert_eq!(shown("1_000 * 3"), Some("3000".into()));
        close("sqrt(16) + abs(-2)", 6.0);
        close("sin(30)", 0.5);
        close("2 * pi", std::f64::consts::TAU);
        close("1e3 + 1", 1001.0);
    }

    #[test]
    fn percentages() {
        assert_eq!(shown("15% of 80"), Some("12".into()));
        assert_eq!(shown("50% of 2^3"), Some("4".into()));
        assert_eq!(shown("10% of 2^10"), Some("102.4".into()));
        assert_eq!(shown("50% of -8"), Some("-4".into()));
        assert_eq!(shown("50%"), Some("0.5".into()));
        assert_eq!(shown("200 * 10%"), Some("20".into()));
    }

    #[test]
    fn not_sums() {
        for input in ["", "12", "3.5", "e", "pi", "firefox", "1 +", "(2", "2 ** ", "modern", "e mail", "1/0", "sqrt"] {
            assert_eq!(answer(input), None, "{input:?}");
        }
    }

    #[test]
    fn bases() {
        assert_eq!(shown("0xff"), Some("255".into()));
        assert_eq!(shown("0b101"), Some("5".into()));
        assert_eq!(shown("0o17"), Some("15".into()));
        assert_eq!(shown("255 in hex"), Some("0xff".into()));
        assert_eq!(shown("5 in binary"), Some("0b101".into()));
        assert_eq!(shown("0xff in bin"), Some("0b11111111".into()));
        assert_eq!(shown("0x10 to dec"), Some("16".into()));
        assert_eq!(shown("-10 in hex"), Some("-0xa".into()));
        assert_eq!(shown("1.5 in hex"), None);
        assert_eq!(shown("0xff + 1"), Some("256".into()));
    }

    #[test]
    fn conversions() {
        close("5 km in mi", 3.106_855_96);
        close("1 mi in km", 1.609_344);
        close("30 c in f", 86.0);
        close("100 °F to C", 37.777_777_8);
        close("0 k in c", -273.15);
        close("2 GiB in MB", 2_147.483_648);
        close("1 GB in GiB", 0.931_322_574_6);
        close("100 Mb in MB", 12.5);
        close("1 MB in Mb", 8.0);
        close("90 min in h", 1.5);
        close("1 year in days", 365.2425);
        close("100 km/h in mph", 62.137_119_2);
        close("1 gal in l", 3.785_411_784);
        close("2 cups in ml", 473.176_473);
        close("1 lb to kg", 0.453_592_37);
        close("12 inches in cm", 30.48);
        close("2*3 km in m", 6000.0);
        close("5km in m", 5000.0);
        assert_eq!(shown("5 km in mi"), Some("3.106855961 mi".into()));
        assert_eq!(shown("30 c in f"), Some("86 °F".into()));
        assert_eq!(answer("5 km in kg"), None, "different dimensions");
        assert_eq!(answer("5 parsecs in m"), None);
        assert_eq!(answer("songs in spotify"), None);
    }

    #[test]
    fn everyday_conversions() {
        close("5 ft in cm", 152.4);
        close("100 f to c", 37.777_777_8);
        close("3 cups in ml", 709.764_709_5);
        close("10 kilometers to miles", 6.213_711_92);
        close("2 tablespoons in tsp", 6.0);
        close("1 kilo in lbs", 2.204_622_62);
        close("98.6 degrees fahrenheit in celsius", 37.0);
        close("20 deg c to f", 68.0);
        close("1 cubic foot in l", 28.316_846_592);
        close("1 imperial gallon in l", 4.546_09);
        close("500 mcg in mg", 0.5);
        close("1 decade in years", 10.0);
        close("1500 ms in s", 1.5);
        close("50 miles per hour in km/h", 80.467_2);
        close("5 ft into cm", 152.4);
        assert_eq!(shown("1 week in days"), Some("7 days".into()));
        assert_eq!(shown("24 h in days"), Some("1 day".into()), "one, not ones");
        assert_eq!(shown("16 tbsp in cups"), Some("1 cup".into()));
        assert_eq!(shown("1 min in s"), Some("60 s".into()));
    }

    #[test]
    fn more_kinds_of_unit() {
        close("1 acre in m2", 4_046.856_422_4);
        close("100 sq ft in m²", 9.290_304);
        close("1 square mile in km2", 2.589_988_110_336);
        close("2 ha in acres", 4.942_108_16);
        close("1 kwh in kj", 3600.0);
        close("100 kcal in kj", 418.4);
        close("1 btu in J", 1_055.055_852_62);
        close("100 hp in kw", 74.569_987_16);
        close("1 atm in psi", 14.695_948_8);
        close("1013.25 hPa in atm", 1.0);
        close("2 bar in kpa", 200.0);
        close("180 degrees in rad", std::f64::consts::PI);
        close("1 turn in deg", 360.0);
        close("2.4 GHz in MHz", 2400.0);
        close("3000 rpm in hz", 50.0);
        assert_eq!(shown("1 acre in ha"), Some("0.4046856422 ha".into()));
        assert_eq!(answer("1 kwh in kw"), None, "energy is not power");
        assert_eq!(answer("30 degrees in f"), None, "an angle is not a temperature");
    }

    #[test]
    fn milli_is_not_mega() {
        close("5 mW in W", 0.005);
        close("500 mJ in J", 0.5);
        close("1 MW in kW", 1000.0);
        close("2 MJ in kJ", 2000.0);
        close("1 MPa in bar", 10.0);
        close("3 Megawatts in kW", 3000.0);
        close("250 milliwatts in W", 0.25);
        assert_eq!(shown("2 kW in MW"), Some("0.002 MW".into()));
        assert_eq!(shown("2 W in mW"), Some("2000 mW".into()));
        for input in ["5 mw in W", "500 mj in J", "1 mpa in bar", "100 mhz in Hz", "5 Mw in W"] {
            assert_eq!(answer(input), None, "{input:?} could be milli or mega");
        }
    }

    #[test]
    fn a_unit_after_a_word_stands_apart() {
        for input in ["am in cm", "and in h", "apt to ml", "onem in cm", "how many cm in am"] {
            assert_eq!(answer(input), None, "{input:?}");
        }
        close("a m in cm", 100.0);
        close("an  hour in min", 60.0);
        close("One mile in km", 1.609_344);
    }

    #[test]
    fn one_of_a_plural() {
        assert_eq!(shown("100 years in centuries"), Some("1 century".into()));
        assert_eq!(shown("200 years in centuries"), Some("2 centuries".into()));
        assert_eq!(shown("10 years in decades"), Some("1 decade".into()));
        assert_eq!(shown("150 years in centuries and years"), Some("1 century 50 years".into()));
        assert_eq!(shown("1000 ms in s"), Some("1 s".into()));
    }

    #[test]
    fn a_minus_is_for_every_part() {
        close("-5 ft 3 in in cm", -160.02);
        close("-1 h 30 min 30 s in s", -5430.0);
        close("-0 ft 3 in in cm", -7.62);
        close("-5 ft in cm", -152.4);
        assert_eq!(shown("-5 ft 3 in in ft in"), Some("-5 ft 3 in".into()));
    }

    #[test]
    fn amounts_in_parts() {
        close("5 ft 10 in in cm", 177.8);
        close("5'10\" in cm", 177.8);
        close("5 feet and 10 inches to cm", 177.8);
        close("1 h 30 min in min", 90.0);
        close("1h30min in s", 5400.0);
        close("6 lb 4 oz in kg", 2.834_951_562_5);
        close("1 hour, 15 minutes in h", 1.25);
        assert_eq!(answer("5 ft 10 kg in cm"), None, "parts measure the same thing");
        assert_eq!(answer("20 c 5 f in k"), None, "temperatures do not add up");
    }

    #[test]
    fn results_in_parts() {
        assert_eq!(shown("180 cm in ft and in"), Some("5 ft 10.87 in".into()));
        assert_eq!(shown("182.88 cm in feet and inches"), Some("6 ft 0 in".into()), "not 5 ft 12 in");
        assert_eq!(shown("100 min in h min"), Some("1 h 40 min".into()));
        assert_eq!(shown("30 cm in ft in"), Some("11.81 in".into()), "no 0 ft");
        assert_eq!(shown("80 kg in st lb"), Some("12 st 8.37 lb".into()));
        assert_eq!(shown("1 ft in ft in"), Some("1 ft 0 in".into()));
        assert_eq!(answer("180 cm in in ft"), None, "largest first");
        assert_eq!(answer("10 c in f k"), None);
        assert_eq!(answer("5 km in mi kg"), None);
        let answer = answer("100 min in h min").unwrap();
        assert_eq!(answer.copy, answer.shown);
    }

    #[test]
    fn conversions_in_words() {
        close("convert 5 ft to cm", 152.4);
        close("what is 5 km in mi?", 3.106_855_96);
        close("how many cm in a foot", 30.48);
        close("how many ml are in 3 cups", 709.764_709_5);
        close("How many seconds in a day?", 86_400.0);
        close("how many feet in one mile", 5280.0);
        close("a mile in km", 1.609_344);
        assert_eq!(shown("convert 255 to hex"), Some("0xff".into()));
        assert_eq!(shown("what is 2+2?"), Some("4".into()));
        assert_eq!(answer("how many apples in a basket"), None);
        assert_eq!(answer("what is love"), None);
    }

    #[test]
    fn conversions_are_not_questions() {
        assert!(looks_like_conversion("5 km in mi"));
        assert!(looks_like_conversion("255 in hex"));
        assert!(looks_like_conversion("convert 5 km in mi"));
        assert!(looks_like_conversion("180 cm in ft and in"));
        assert!(!looks_like_conversion("what is in my home folder"));
        assert!(!looks_like_conversion("set gaps to 4"));
    }

    #[test]
    fn numbers_are_short() {
        assert_eq!(number(1234.5).as_deref(), Some("1234.5"));
        assert_eq!(number(2.0f64.powi(60)).as_deref(), Some("1.152921505e18"));
        assert_eq!(number(0.000_000_123).as_deref(), Some("1.23e-7"));
        assert_eq!(number(-0.000_000_000_1 * 0.0).as_deref(), Some("0"));
        assert_eq!(number(f64::NAN), None);
    }
}
