//! The launcher's calculator: sums (`12*7+3`, `2^10`, `15% of 80`), number
//! bases (`0xff`, `255 in hex`) and offline unit conversions (`5 km in mi`,
//! `30 c in f`, `2 GiB in MB`). Everything is local and instant.

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
    let input = input.trim();
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

/// `<left> in <right>` or `<left> to <right>`, at the last such word.
fn split_conversion(input: &str) -> Option<(&str, &str)> {
    let lower = input.to_ascii_lowercase();
    [" in ", " to ", " as "]
        .iter()
        .filter_map(|word| lower.rfind(word).map(|at| (at, word.len())))
        .max_by_key(|(at, _)| *at)
        .map(|(at, len)| (input[..at].trim(), input[at + len..].trim()))
        .filter(|(left, right)| !left.is_empty() && !right.is_empty())
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
        let n = value as i64;
        let (sign, n) = (if n < 0 { "-" } else { "" }, n.unsigned_abs());
        let text = match self {
            Base::Hex => format!("{sign}0x{n:x}"),
            Base::Binary => format!("{sign}0b{n:b}"),
            Base::Octal => format!("{sign}0o{n:o}"),
            Base::Decimal => format!("{sign}{n}"),
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
/// postfix = atom "%"* ("of" unary)?
/// atom    = number | constant | function "(" sum ")" | "(" sum ")"
/// ```
struct Parser<'a> {
    text: &'a str,
    at: usize,
    worked: bool,
}

impl<'a> Parser<'a> {
    fn new(text: &'a str) -> Parser<'a> {
        Parser { text, at: 0, worked: false }
    }

    fn parse(mut self) -> Option<Value> {
        let value = self.sum()?;
        self.skip_space();
        (self.at == self.text.len() && value.is_finite()).then_some(Value { value, worked: self.worked })
    }

    fn rest(&self) -> &'a str {
        &self.text[self.at..]
    }

    fn skip_space(&mut self) {
        let rest = self.rest();
        self.at += rest.len() - rest.trim_start().len();
    }

    /// Takes `token` (case-insensitive) when it comes next.
    fn eat(&mut self, token: &str) -> bool {
        self.skip_space();
        let rest = self.rest();
        if rest.get(..token.len()).is_some_and(|head| head.eq_ignore_ascii_case(token)) {
            // A word must end there: "mod" is not the start of "modern".
            let word = token.chars().all(|c| c.is_ascii_alphabetic());
            if word && rest[token.len()..].starts_with(|c: char| c.is_ascii_alphanumeric()) {
                return false;
            }
            self.at += token.len();
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
        if self.eat("of") {
            self.worked = true;
            value *= self.unary()?;
        }
        Some(value)
    }

    fn atom(&mut self) -> Option<f64> {
        self.skip_space();
        if self.eat("(") {
            let value = self.sum()?;
            return self.eat(")").then_some(value);
        }
        for (name, f) in FUNCTIONS {
            let at = self.at;
            if self.eat(name) {
                if self.eat("(") {
                    let value = self.sum()?;
                    if self.eat(")") {
                        self.worked = true;
                        return Some(f(value));
                    }
                    return None;
                }
                self.at = at;
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
            let digits: String = rest[2..].chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '_').collect();
            let value = u64::from_str_radix(&digits.replace('_', ""), radix).ok()?;
            self.at += 2 + digits.len();
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
        self.at += end;
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
    ("sin", |x| x.to_radians().sin()),
    ("cos", |x| x.to_radians().cos()),
    ("tan", |x| x.to_radians().tan()),
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
}

/// A unit: what it measures, and its size in that dimension's base unit
/// (metres, kilograms, kelvin, bytes, seconds, metres per second, litres).
/// Temperatures also have an offset: kelvin = value * factor + offset.
struct Unit {
    names: &'static [&'static str],
    /// The name results are shown with.
    symbol: &'static str,
    dimension: Dimension,
    factor: f64,
    offset: f64,
    /// Matched with exact case only, where case changes the meaning
    /// (MB is bytes, Mb is bits).
    exact_case: bool,
}

const fn unit(names: &'static [&'static str], symbol: &'static str, dimension: Dimension, factor: f64) -> Unit {
    Unit { names, symbol, dimension, factor, offset: 0.0, exact_case: false }
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
    unit(&["m", "metre", "metres", "meter", "meters"], "m", Dimension::Length, 1.0),
    unit(&["km", "kilometre", "kilometres", "kilometer", "kilometers"], "km", Dimension::Length, 1e3),
    unit(&["in", "inch", "inches", "\""], "in", Dimension::Length, 0.0254),
    unit(&["ft", "foot", "feet", "'"], "ft", Dimension::Length, 0.3048),
    unit(&["yd", "yard", "yards"], "yd", Dimension::Length, 0.9144),
    unit(&["mi", "mile", "miles"], "mi", Dimension::Length, 1609.344),
    unit(&["nmi", "nautical mile", "nautical miles"], "nmi", Dimension::Length, 1852.0),
    unit(&["mg", "milligram", "milligrams"], "mg", Dimension::Mass, 1e-6),
    unit(&["g", "gram", "grams"], "g", Dimension::Mass, 1e-3),
    unit(&["kg", "kilo", "kilos", "kilogram", "kilograms"], "kg", Dimension::Mass, 1.0),
    unit(&["t", "tonne", "tonnes", "ton", "tons"], "t", Dimension::Mass, 1e3),
    unit(&["oz", "ounce", "ounces"], "oz", Dimension::Mass, 0.028_349_523_125),
    unit(&["lb", "lbs", "pound", "pounds"], "lb", Dimension::Mass, 0.453_592_37),
    unit(&["st", "stone", "stones"], "st", Dimension::Mass, 6.350_293_18),
    Unit {
        names: &["c", "°c", "celsius", "degc"],
        symbol: "°C",
        dimension: Dimension::Temperature,
        factor: 1.0,
        offset: 273.15,
        exact_case: false,
    },
    Unit {
        names: &["f", "°f", "fahrenheit", "degf"],
        symbol: "°F",
        dimension: Dimension::Temperature,
        factor: 5.0 / 9.0,
        offset: 273.15 - 32.0 * 5.0 / 9.0,
        exact_case: false,
    },
    unit(&["k", "kelvin"], "K", Dimension::Temperature, 1.0),
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
    unit(&["ms", "millisecond", "milliseconds"], "ms", Dimension::Time, 1e-3),
    unit(&["s", "sec", "secs", "second", "seconds"], "s", Dimension::Time, 1.0),
    unit(&["min", "mins", "minute", "minutes"], "min", Dimension::Time, 60.0),
    unit(&["h", "hr", "hrs", "hour", "hours"], "h", Dimension::Time, 3600.0),
    unit(&["d", "day", "days"], "days", Dimension::Time, DAY),
    unit(&["wk", "week", "weeks"], "weeks", Dimension::Time, 7.0 * DAY),
    unit(&["month", "months"], "months", Dimension::Time, 30.436_875 * DAY),
    unit(&["yr", "year", "years"], "years", Dimension::Time, 365.2425 * DAY),
    unit(&["m/s", "mps"], "m/s", Dimension::Speed, 1.0),
    unit(&["km/h", "kmh", "kph", "kmph"], "km/h", Dimension::Speed, 1.0 / 3.6),
    unit(&["mph", "mi/h"], "mph", Dimension::Speed, 0.44704),
    unit(&["ft/s", "fps"], "ft/s", Dimension::Speed, 0.3048),
    unit(&["kn", "kt", "knot", "knots"], "kn", Dimension::Speed, 1852.0 / 3600.0),
    unit(&["ml", "mL", "millilitre", "millilitres", "milliliter", "milliliters"], "ml", Dimension::Volume, 1e-3),
    unit(&["cl", "cL", "centilitre", "centilitres"], "cl", Dimension::Volume, 1e-2),
    unit(&["dl", "dL", "decilitre", "decilitres"], "dl", Dimension::Volume, 1e-1),
    unit(&["l", "L", "litre", "litres", "liter", "liters"], "l", Dimension::Volume, 1.0),
    unit(&["m3", "m³", "cubic metre", "cubic metres"], "m³", Dimension::Volume, 1e3),
    unit(&["cm3", "cm³", "cc"], "cm³", Dimension::Volume, 1e-3),
    unit(&["gal", "gallon", "gallons"], "gal", Dimension::Volume, 3.785_411_784),
    unit(&["qt", "quart", "quarts"], "qt", Dimension::Volume, 0.946_352_946),
    unit(&["pt", "pint", "pints"], "pt", Dimension::Volume, 0.473_176_473),
    unit(&["cup", "cups"], "cups", Dimension::Volume, 0.236_588_236_5),
    unit(&["floz", "fl oz", "fluid ounce", "fluid ounces"], "fl oz", Dimension::Volume, 0.029_573_529_562_5),
    unit(&["tbsp", "tablespoon", "tablespoons"], "tbsp", Dimension::Volume, 0.014_786_764_781_25),
    unit(&["tsp", "teaspoon", "teaspoons"], "tsp", Dimension::Volume, 0.004_928_921_593_75),
];

fn find_unit(name: &str) -> Option<&'static Unit> {
    let name = name.trim();
    // Exact spelling first, so "Mb" (bits) and "MB" (bytes) stay apart.
    UNITS.iter().find(|u| u.names.contains(&name)).or_else(|| {
        let lower = name.to_lowercase();
        UNITS.iter().find(|u| !u.exact_case && u.names.iter().any(|n| n.to_lowercase() == lower))
    })
}

/// `<amount> <unit> in|to <unit>`, where the amount may be a sum.
fn convert_units(input: &str) -> Option<Answer> {
    let (left, target) = split_conversion(input)?;
    let target = find_unit(target)?;
    // The unit is whatever follows the amount: try the longest tail that names one.
    let (amount, from) = left
        .char_indices()
        .filter(|(i, _)| *i > 0)
        .map(|(i, _)| (&left[..i], &left[i..]))
        .find_map(|(amount, unit)| Some((amount, find_unit(unit)?)))?;
    if from.dimension != target.dimension {
        return None;
    }
    let amount = Parser::new(amount).parse()?.value;
    let base = amount * from.factor + from.offset;
    let value = (base - target.offset) / target.factor;
    let copy = number(value)?;
    let shown = format!("{copy} {}", target.symbol);
    Some(Answer { copy, shown })
}

/// Whether `input` reads as a unit or base conversion, even an unfinished
/// one, so it is not mistaken for a question.
pub fn looks_like_conversion(input: &str) -> bool {
    split_conversion(input).is_some_and(|(left, right)| {
        left.starts_with(|c: char| c.is_ascii_digit() || c == '-' || c == '.' || c == '(')
            && (find_unit(right).is_some() || Base::named(right).is_some())
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test setup and assertions should fail immediately on unexpected errors")]
mod tests {
    use super::*;

    fn shown(input: &str) -> Option<String> {
        answer(input).map(|a| a.shown)
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
    fn conversions_are_not_questions() {
        assert!(looks_like_conversion("5 km in mi"));
        assert!(looks_like_conversion("255 in hex"));
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
