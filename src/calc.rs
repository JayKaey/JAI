//! Instant arithmetic and unit conversion, the way the real J.A.R.V.I.S. does sums: no language
//! model round-trip (small LLMs are unreliable at maths anyway). English and German phrasing:
//! "what's 15 percent of 240", "wie viel ist 12 mal 17", "square root of 2", "convert 5 miles to
//! km", "100 Fahrenheit in Celsius", "how many grams in 3 pounds".

use regex::Regex;
use std::sync::LazyLock;

fn re(p: &str) -> Regex {
    Regex::new(p).expect("valid calc regex")
}

// ------------------------------------------------------------------------- evaluator ----

struct Parser<'a> {
    chars: Vec<char>,
    pos: usize,
    _src: &'a str,
}

impl<'a> Parser<'a> {
    fn new(src: &'a str) -> Self {
        Self { chars: src.chars().filter(|c| !c.is_whitespace()).collect(), pos: 0, _src: src }
    }

    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }

    fn eat(&mut self, c: char) -> bool {
        if self.peek() == Some(c) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn expr(&mut self) -> Result<f64, String> {
        let mut v = self.term()?;
        loop {
            if self.eat('+') {
                v += self.term()?;
            } else if self.eat('-') {
                v -= self.term()?;
            } else {
                return Ok(v);
            }
        }
    }

    fn term(&mut self) -> Result<f64, String> {
        let mut v = self.unary()?;
        loop {
            if self.eat('*') {
                v *= self.unary()?;
            } else if self.eat('/') {
                let d = self.unary()?;
                if d == 0.0 {
                    return Err("division by zero".into());
                }
                v /= d;
            } else if self.eat('%') {
                // "15%" as a postfix percentage, otherwise modulo.
                if matches!(self.peek(), None | Some(')' | '+' | '-' | '*' | '/')) {
                    v /= 100.0;
                } else {
                    let d = self.unary()?;
                    v %= d;
                }
            } else {
                return Ok(v);
            }
        }
    }

    /// Sign binds looser than powers: -3^2 = -(3^2).
    fn unary(&mut self) -> Result<f64, String> {
        if self.eat('-') {
            return Ok(-self.unary()?);
        }
        if self.eat('+') {
            return self.unary();
        }
        self.power()
    }

    fn power(&mut self) -> Result<f64, String> {
        let base = self.primary()?;
        if self.eat('^') {
            let exp = self.unary()?; // right-associative, allows 2^-1
            return Ok(base.powf(exp));
        }
        Ok(base)
    }

    fn primary(&mut self) -> Result<f64, String> {
        if self.eat('(') {
            let v = self.expr()?;
            if !self.eat(')') {
                return Err("missing ')'".into());
            }
            return Ok(v);
        }
        let start = self.pos;
        if self.peek().is_some_and(|c| c.is_ascii_digit() || c == '.') {
            while self.peek().is_some_and(|c| c.is_ascii_digit() || c == '.') {
                self.pos += 1;
            }
            let s: String = self.chars[start..self.pos].iter().collect();
            return s.parse::<f64>().map_err(|_| format!("bad number '{s}'"));
        }
        while self.peek().is_some_and(|c| c.is_ascii_alphabetic()) {
            self.pos += 1;
        }
        let name: String = self.chars[start..self.pos].iter().collect();
        match name.as_str() {
            "pi" => Ok(std::f64::consts::PI),
            "e" => Ok(std::f64::consts::E),
            "" => Err("expected a number".into()),
            f => {
                let arg = if self.peek() == Some('(') { self.primary()? } else { self.unary()? };
                Ok(match f {
                    "sqrt" => {
                        if arg < 0.0 {
                            return Err("square root of a negative number".into());
                        }
                        arg.sqrt()
                    }
                    "cbrt" => arg.cbrt(),
                    "abs" => arg.abs(),
                    "ln" => arg.ln(),
                    "log" => arg.log10(),
                    "sin" => arg.to_radians().sin(),
                    "cos" => arg.to_radians().cos(),
                    "tan" => arg.to_radians().tan(),
                    "round" => arg.round(),
                    "floor" => arg.floor(),
                    "ceil" => arg.ceil(),
                    other => return Err(format!("unknown function '{other}'")),
                })
            }
        }
    }
}

/// Evaluates `+ - * / ^ %`, parentheses, `pi`, `e` and sqrt/cbrt/abs/ln/log/sin/cos/tan/round.
pub fn evaluate(expr: &str) -> Result<f64, String> {
    let mut p = Parser::new(expr);
    let v = p.expr()?;
    if p.pos != p.chars.len() {
        return Err(format!("unexpected '{}'", p.chars[p.pos..].iter().collect::<String>()));
    }
    if !v.is_finite() {
        return Err("the result is not a finite number".into());
    }
    Ok(v)
}

/// 36, 0.333333, 1.5e-7 → speakable, without float noise.
pub fn format_number(v: f64) -> String {
    if v != 0.0 && (v.abs() >= 1e15 || v.abs() < 1e-6) {
        return format!("{v:.4e}");
    }
    let rounded = (v * 1e6).round() / 1e6;
    let s = format!("{rounded:.6}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s == "-0" { "0".into() } else { s.to_string() }
}

// --------------------------------------------------------------------- natural text ----

const NUMBER_WORDS: &[(&str, &str)] = &[
    ("zero", "0"), ("one", "1"), ("two", "2"), ("three", "3"), ("four", "4"), ("five", "5"), ("six", "6"), ("seven", "7"),
    ("eight", "8"), ("nine", "9"), ("ten", "10"), ("eleven", "11"), ("twelve", "12"), ("twenty", "20"), ("hundred", "100"),
    ("thousand", "1000"), ("million", "1000000"), ("null", "0"), ("eins", "1"), ("zwei", "2"), ("drei", "3"), ("vier", "4"),
    ("fünf", "5"), ("sechs", "6"), ("sieben", "7"), ("acht", "8"), ("neun", "9"), ("zehn", "10"), ("elf", "11"), ("zwölf", "12"),
    ("zwanzig", "20"), ("hundert", "100"), ("tausend", "1000"),
];

/// "3,5" (German decimal comma) vs "1,000" (thousands separator).
fn fix_commas(s: &str) -> String {
    static THOUSANDS: LazyLock<Regex> = LazyLock::new(|| re(r"(\d),(\d{3})\b"));
    static DECIMAL: LazyLock<Regex> = LazyLock::new(|| re(r"(\d),(\d)"));
    let s = THOUSANDS.replace_all(s, "$1$2");
    DECIMAL.replace_all(&s, "$1.$2").into_owned()
}

/// Spoken maths → an expression for [`evaluate`].
fn to_expression(text: &str) -> String {
    let mut s = format!(" {} ", fix_commas(&text.to_lowercase()));
    for (from, to) in [
        ("multiplied by", "*"), ("times", "*"), ("mal", "*"), (" x ", " * "), ("×", "*"), ("divided by", "/"), ("geteilt durch", "/"),
        ("durch", "/"), ("over", "/"), ("÷", "/"), ("plus", "+"), ("und", "+"), ("minus", "-"), ("weniger", "-"),
        ("to the power of", "^"), ("hoch", "^"), ("squared", "^2"), ("cubed", "^3"), ("quadrat", "^2"),
        ("the square root of", "sqrt "), ("square root of", "sqrt "), ("die wurzel aus", "sqrt "), ("wurzel aus", "sqrt "),
        ("wurzel von", "sqrt "),
        ("cube root of", "cbrt "), ("percent", "%"), ("prozent", "%"), ("modulo", "%"), ("mod", "%"),
    ] {
        s = s.replace(from, to);
    }
    let mut out: Vec<String> = Vec::new();
    for w in s.split_whitespace() {
        let w = w.trim_matches(|c: char| matches!(c, '?' | '!' | ','));
        match NUMBER_WORDS.iter().find(|(word, _)| *word == w) {
            Some((_, n)) => out.push(n.to_string()),
            None => out.push(w.to_string()),
        }
    }
    out.join(" ")
}

static ASK_MATH: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:jarvis\s*,?\s*)?(?:what(?:'s| is)|whats|calculate|compute|how much is|work out|tell me|wie ?viel (?:ist|sind|macht|ergibt)|was (?:ist|ergibt|macht)|rechne|berechne|was sind)\s+(.+?)[?.!]*$")
});
static PERCENT_OF: LazyLock<Regex> =
    LazyLock::new(|| re(r"^([\d.]+)\s*(?:%|percent|prozent)\s*(?:of|von)\s+([\d.]+)$"));
static ALLOWED: LazyLock<Regex> = LazyLock::new(|| re(r"^[\d\s.+\-*/^%()a-z]+$"));
static FUNCS: LazyLock<Regex> = LazyLock::new(|| re(r"\b(sqrt|cbrt|abs|ln|log|sin|cos|tan|round|floor|ceil|pi)\b"));

/// The value of a spoken calculation, with how to say it ("15 percent of 240").
pub fn answer_math(text: &str) -> Option<(String, f64)> {
    let lower = text.trim().to_lowercase();
    let body = ASK_MATH.captures(&lower).and_then(|c| c.get(1)).map(|m| m.as_str().to_string()).unwrap_or_else(|| lower.clone());
    let expr = to_expression(&body).trim().trim_end_matches(['?', '.', '!', '=']).trim().to_string();
    if let Some(c) = PERCENT_OF.captures(&expr) {
        let (p, of): (f64, f64) = (c[1].parse().ok()?, c[2].parse().ok()?);
        return Some((format!("{} percent of {}", format_number(p), format_number(of)), p / 100.0 * of));
    }
    // Only real calculations: digits, at least one operator or function, nothing else left over.
    let letters_ok = FUNCS.replace_all(&expr, "").chars().all(|c| !c.is_ascii_alphabetic());
    let has_op = expr.contains(['+', '*', '/', '^', '%']) || expr.trim_start_matches('-').contains('-') || FUNCS.is_match(&expr);
    if !ALLOWED.is_match(&expr) || !letters_ok || !has_op || !expr.chars().any(|c| c.is_ascii_digit()) {
        return None;
    }
    let value = evaluate(&expr).ok()?;
    let spoken = expr
        .replace('*', " times ")
        .replace('/', " divided by ")
        .replace('^', " to the power of ")
        .replace('+', " plus ")
        .replace(" - ", " minus ")
        .replace("sqrt", "the square root of");
    Some((spoken.split_whitespace().collect::<Vec<_>>().join(" "), value))
}

// ---------------------------------------------------------------------------- units ----

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Dim {
    Length,
    Mass,
    Volume,
    Time,
    Speed,
    Data,
    Area,
    Energy,
    Temperature,
}

struct Unit {
    names: &'static [&'static str],
    display: &'static str,
    dim: Dim,
    /// Size in the dimension's base unit (metre, kilogram, litre, second, m/s, byte, m², joule).
    factor: f64,
}

const UNITS: &[Unit] = &[
    Unit { names: &["mm", "millimeter", "millimeters", "millimetre", "millimetres", "millimeter"], display: "millimetres", dim: Dim::Length, factor: 0.001 },
    Unit { names: &["cm", "centimeter", "centimeters", "centimetre", "centimetres", "zentimeter"], display: "centimetres", dim: Dim::Length, factor: 0.01 },
    Unit { names: &["m", "meter", "meters", "metre", "metres"], display: "metres", dim: Dim::Length, factor: 1.0 },
    Unit { names: &["km", "kilometer", "kilometers", "kilometre", "kilometres", "kilometern"], display: "kilometres", dim: Dim::Length, factor: 1000.0 },
    Unit { names: &["in", "inch", "inches", "zoll"], display: "inches", dim: Dim::Length, factor: 0.0254 },
    Unit { names: &["ft", "foot", "feet", "fuß", "fuss"], display: "feet", dim: Dim::Length, factor: 0.3048 },
    Unit { names: &["yd", "yard", "yards"], display: "yards", dim: Dim::Length, factor: 0.9144 },
    Unit { names: &["mi", "mile", "miles", "meile", "meilen"], display: "miles", dim: Dim::Length, factor: 1609.344 },
    Unit { names: &["nmi", "nautical mile", "nautical miles", "seemeile", "seemeilen"], display: "nautical miles", dim: Dim::Length, factor: 1852.0 },
    Unit { names: &["mg", "milligram", "milligrams", "milligramm"], display: "milligrams", dim: Dim::Mass, factor: 1e-6 },
    Unit { names: &["g", "gram", "grams", "gramm", "gramme"], display: "grams", dim: Dim::Mass, factor: 0.001 },
    Unit { names: &["kg", "kilo", "kilos", "kilogram", "kilograms", "kilogramm"], display: "kilograms", dim: Dim::Mass, factor: 1.0 },
    Unit { names: &["t", "tonne", "tonnes", "ton", "tons", "tonnen"], display: "tonnes", dim: Dim::Mass, factor: 1000.0 },
    Unit { names: &["oz", "ounce", "ounces", "unze", "unzen"], display: "ounces", dim: Dim::Mass, factor: 0.028_349_523_125 },
    Unit { names: &["lb", "lbs", "pound", "pounds", "pfund"], display: "pounds", dim: Dim::Mass, factor: 0.453_592_37 },
    Unit { names: &["st", "stone", "stones"], display: "stone", dim: Dim::Mass, factor: 6.350_293_18 },
    Unit { names: &["ml", "milliliter", "milliliters", "millilitre", "millilitres"], display: "millilitres", dim: Dim::Volume, factor: 0.001 },
    Unit { names: &["l", "liter", "liters", "litre", "litres"], display: "litres", dim: Dim::Volume, factor: 1.0 },
    Unit { names: &["gal", "gallon", "gallons", "us gallon", "us gallons"], display: "US gallons", dim: Dim::Volume, factor: 3.785_411_784 },
    Unit { names: &["pint", "pints", "uk pint", "uk pints"], display: "pints", dim: Dim::Volume, factor: 0.568_261_25 },
    Unit { names: &["cup", "cups", "tasse", "tassen"], display: "cups", dim: Dim::Volume, factor: 0.236_588 },
    Unit { names: &["fl oz", "fluid ounce", "fluid ounces"], display: "fluid ounces", dim: Dim::Volume, factor: 0.029_573_53 },
    Unit { names: &["s", "sec", "second", "seconds", "sekunde", "sekunden"], display: "seconds", dim: Dim::Time, factor: 1.0 },
    Unit { names: &["min", "minute", "minutes", "minuten"], display: "minutes", dim: Dim::Time, factor: 60.0 },
    Unit { names: &["h", "hr", "hour", "hours", "stunde", "stunden"], display: "hours", dim: Dim::Time, factor: 3600.0 },
    Unit { names: &["day", "days", "tag", "tage", "tagen"], display: "days", dim: Dim::Time, factor: 86_400.0 },
    Unit { names: &["week", "weeks", "woche", "wochen"], display: "weeks", dim: Dim::Time, factor: 604_800.0 },
    Unit { names: &["year", "years", "jahr", "jahre", "jahren"], display: "years", dim: Dim::Time, factor: 31_557_600.0 },
    Unit { names: &["km/h", "kmh", "kph", "kilometers per hour", "kilometres per hour", "stundenkilometer"], display: "kilometres per hour", dim: Dim::Speed, factor: 1.0 / 3.6 },
    Unit { names: &["mph", "miles per hour"], display: "miles per hour", dim: Dim::Speed, factor: 0.447_04 },
    Unit { names: &["m/s", "meters per second", "metres per second"], display: "metres per second", dim: Dim::Speed, factor: 1.0 },
    Unit { names: &["knot", "knots", "knoten", "kn"], display: "knots", dim: Dim::Speed, factor: 0.514_444 },
    Unit { names: &["b", "byte", "bytes"], display: "bytes", dim: Dim::Data, factor: 1.0 },
    Unit { names: &["kb", "kilobyte", "kilobytes"], display: "kilobytes", dim: Dim::Data, factor: 1e3 },
    Unit { names: &["mb", "megabyte", "megabytes"], display: "megabytes", dim: Dim::Data, factor: 1e6 },
    Unit { names: &["gb", "gigabyte", "gigabytes"], display: "gigabytes", dim: Dim::Data, factor: 1e9 },
    Unit { names: &["tb", "terabyte", "terabytes"], display: "terabytes", dim: Dim::Data, factor: 1e12 },
    Unit { names: &["kib", "kibibyte", "kibibytes"], display: "kibibytes", dim: Dim::Data, factor: 1024.0 },
    Unit { names: &["mib", "mebibyte", "mebibytes"], display: "mebibytes", dim: Dim::Data, factor: 1_048_576.0 },
    Unit { names: &["gib", "gibibyte", "gibibytes"], display: "gibibytes", dim: Dim::Data, factor: 1_073_741_824.0 },
    Unit { names: &["m2", "m²", "square meter", "square meters", "square metre", "square metres", "quadratmeter"], display: "square metres", dim: Dim::Area, factor: 1.0 },
    Unit { names: &["km2", "km²", "square kilometer", "square kilometers", "square kilometre", "square kilometres", "quadratkilometer"], display: "square kilometres", dim: Dim::Area, factor: 1e6 },
    Unit { names: &["sq ft", "ft2", "square foot", "square feet"], display: "square feet", dim: Dim::Area, factor: 0.092_903_04 },
    Unit { names: &["acre", "acres"], display: "acres", dim: Dim::Area, factor: 4_046.856_422_4 },
    Unit { names: &["ha", "hectare", "hectares", "hektar"], display: "hectares", dim: Dim::Area, factor: 10_000.0 },
    Unit { names: &["j", "joule", "joules"], display: "joules", dim: Dim::Energy, factor: 1.0 },
    Unit { names: &["kj", "kilojoule", "kilojoules"], display: "kilojoules", dim: Dim::Energy, factor: 1000.0 },
    Unit { names: &["cal", "calorie", "calories", "kalorie", "kalorien"], display: "calories", dim: Dim::Energy, factor: 4.184 },
    Unit { names: &["kcal", "kilocalorie", "kilocalories", "kilokalorien"], display: "kilocalories", dim: Dim::Energy, factor: 4184.0 },
    Unit { names: &["kwh", "kilowatt hour", "kilowatt hours", "kilowattstunde", "kilowattstunden"], display: "kilowatt hours", dim: Dim::Energy, factor: 3.6e6 },
    Unit { names: &["c", "°c", "celsius", "degrees celsius", "grad celsius", "grad"], display: "degrees Celsius", dim: Dim::Temperature, factor: 1.0 },
    Unit { names: &["f", "°f", "fahrenheit", "degrees fahrenheit", "grad fahrenheit"], display: "degrees Fahrenheit", dim: Dim::Temperature, factor: 1.0 },
    Unit { names: &["k", "kelvin"], display: "kelvin", dim: Dim::Temperature, factor: 1.0 },
];

fn unit(name: &str) -> Option<&'static Unit> {
    let n = name.trim().trim_end_matches(['.', '?', '!']).to_lowercase();
    let n = n.strip_prefix("the ").unwrap_or(&n).trim().to_string();
    UNITS.iter().find(|u| u.names.contains(&n.as_str()))
}

fn to_celsius(v: f64, display: &str) -> f64 {
    match display {
        "degrees Fahrenheit" => (v - 32.0) * 5.0 / 9.0,
        "kelvin" => v - 273.15,
        _ => v,
    }
}

fn from_celsius(c: f64, display: &str) -> f64 {
    match display {
        "degrees Fahrenheit" => c * 9.0 / 5.0 + 32.0,
        "kelvin" => c + 273.15,
        _ => c,
    }
}

/// Converts `value` between two named units of the same dimension.
pub fn convert(value: f64, from: &str, to: &str) -> Result<(f64, &'static str, &'static str), String> {
    let f = unit(from).ok_or_else(|| format!("I don't know the unit '{from}'"))?;
    let t = unit(to).ok_or_else(|| format!("I don't know the unit '{to}'"))?;
    if f.dim != t.dim {
        return Err(format!("{} and {} measure different things", f.display, t.display));
    }
    let out = if f.dim == Dim::Temperature {
        from_celsius(to_celsius(value, f.display), t.display)
    } else {
        value * f.factor / t.factor
    };
    Ok((out, f.display, t.display))
}

static CONVERT: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:jarvis\s*,?\s*)?(?:(?:please\s+)?(?:convert|change|turn|rechne|umrechnen|wandle)\s+|what(?:'s| is)\s+|whats\s+|how much is\s+|wie ?viel (?:sind|ist)\s+|was sind\s+)?(-?[\d.,]+)\s*([a-zäöüß°²/ ]{1,24}?)\s+(?:to|in|into|as|nach|zu|in)\s+([a-zäöüß°²/ ]{1,24}?)(?:\s+(?:um|umrechnen|please|bitte))?[?.!]*$")
});
static HOW_MANY: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:jarvis\s*,?\s*)?(?:how many|wie ?viele?)\s+([a-zäöüß°²/ ]{1,24}?)\s+(?:are\s+)?(?:in|sind in|hat|haben)\s+(?:a\s+|an\s+|one\s+|ein\s+|einer\s+|einem\s+)?(-?[\d.,]*)\s*([a-zäöüß°²/ ]{1,24}?)[?.!]*$")
});

/// "convert 5 miles to km", "100 F in C", "how many grams in 3 pounds".
pub fn answer_conversion(text: &str) -> Option<String> {
    let lower = fix_commas(&text.trim().to_lowercase());
    let (value, from, to) = if let Some(c) = CONVERT.captures(&lower) {
        (c[1].parse::<f64>().ok()?, c[2].trim().to_string(), c[3].trim().to_string())
    } else {
        let c = HOW_MANY.captures(&lower)?;
        let v = if c[2].is_empty() { 1.0 } else { c[2].parse::<f64>().ok()? };
        (v, c[3].trim().to_string(), c[1].trim().to_string())
    };
    let (out, from_name, to_name) = convert(value, &from, &to).ok()?;
    let plural = |n: f64, name: &str| if (n - 1.0).abs() < 1e-9 { name.trim_end_matches('s').to_string() } else { name.to_string() };
    Some(format!(
        "{} {} is {} {}, Sir.",
        format_number(value),
        if from_name.starts_with("degrees") || from_name == "kelvin" { from_name.to_string() } else { plural(value, from_name) },
        format_number((out * 1000.0).round() / 1000.0),
        if to_name.starts_with("degrees") || to_name == "kelvin" { to_name.to_string() } else { plural(out, to_name) },
    ))
}

/// Calculation or conversion, answered in character; None when the text is neither.
pub fn try_answer(text: &str) -> Option<String> {
    if let Some(line) = answer_conversion(text) {
        return Some(line);
    }
    let (spoken, value) = answer_math(text)?;
    Some(format!("{spoken} is {}, Sir.", format_number(value)))
}

/// The `calculate` tool: an expression, or a conversion "5 miles to km".
pub fn tool(expression: &str) -> Result<String, String> {
    if let Some(line) = answer_conversion(expression) {
        return Ok(line.trim_end_matches(", Sir.").to_string());
    }
    let expr = to_expression(expression);
    let value = evaluate(expr.trim()).or_else(|e| answer_math(expression).map(|(_, v)| v).ok_or(e))?;
    Ok(format!("{} = {}", expression.trim(), format_number(value)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn evaluates_expressions() {
        assert_eq!(evaluate("2+3*4").unwrap(), 14.0);
        assert_eq!(evaluate("(2+3)*4").unwrap(), 20.0);
        assert_eq!(evaluate("2^3^2").unwrap(), 512.0);
        assert_eq!(evaluate("-3^2").unwrap(), -9.0);
        assert_eq!(evaluate("2^-1").unwrap(), 0.5);
        assert_eq!(evaluate("sqrt(16)+abs(-2)").unwrap(), 6.0);
        assert_eq!(evaluate("50%").unwrap(), 0.5);
        assert_eq!(evaluate("10%3").unwrap(), 1.0);
        assert!(evaluate("1/0").is_err());
        assert!(evaluate("2+").is_err());
        assert_eq!(format_number(1.0 / 3.0), "0.333333");
        assert_eq!(format_number(36.0), "36");
        assert_eq!(format_number(0.1 + 0.2), "0.3");
    }

    #[test]
    fn answers_spoken_maths_in_english_and_german() {
        assert_eq!(try_answer("What's 15 percent of 240?").unwrap(), "15 percent of 240 is 36, Sir.");
        assert_eq!(try_answer("Wie viel ist 12 mal 17?").unwrap(), "12 times 17 is 204, Sir.");
        assert_eq!(try_answer("calculate 3,5 plus 1,25").unwrap(), "3.5 plus 1.25 is 4.75, Sir.");
        assert_eq!(try_answer("what is the square root of 144").unwrap(), "the square root of 144 is 12, Sir.");
        assert_eq!(try_answer("what is 2 to the power of 10").unwrap(), "2 to the power of 10 is 1024, Sir.");
        assert_eq!(try_answer("1,000 divided by 8").unwrap(), "1000 divided by 8 is 125, Sir.");
        assert!(try_answer("What is the meaning of life?").is_none());
        assert!(try_answer("What's the weather in Berlin").is_none());
        assert!(try_answer("Open Spotify").is_none());
        assert!(try_answer("What time is it").is_none());
    }

    #[test]
    fn converts_units() {
        assert_eq!(try_answer("Convert 5 miles to km").unwrap(), "5 miles is 8.047 kilometres, Sir.");
        assert_eq!(try_answer("100 fahrenheit in celsius").unwrap(), "100 degrees Fahrenheit is 37.778 degrees Celsius, Sir.");
        assert_eq!(try_answer("Wie viel sind 3 Pfund in Kilo?").unwrap(), "3 pounds is 1.361 kilograms, Sir.");
        assert_eq!(try_answer("how many grams in a pound").unwrap(), "1 pound is 453.592 grams, Sir.");
        assert_eq!(try_answer("what is 1 gb in mb").unwrap(), "1 gigabyte is 1000 megabytes, Sir.");
        assert!(convert(1.0, "kg", "metres").is_err());
        assert_eq!(tool("2*(3+4)").unwrap(), "2*(3+4) = 14");
    }
}
