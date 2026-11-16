//! Instant arithmetic and unit conversion, the way the real J.A.I. does sums: no language
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
    re(r"^(?:jai\s*,?\s*)?(?:what(?:'s| is)|whats|calculate|compute|how much is|work out|tell me|wie ?viel (?:ist|sind|macht|ergibt)|was (?:ist|ergibt|macht)|rechne|berechne|was sind)\s+(.+?)[?.!]*$")
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
    re(r"^(?:jai\s*,?\s*)?(?:(?:please\s+)?(?:convert|change|turn|rechne|umrechnen|wandle)\s+|what(?:'s| is)\s+|whats\s+|how much is\s+|wie ?viel (?:sind|ist)\s+|was sind\s+)?(-?[\d.,]+)\s*([a-zäöüß°²/ €£$]{1,24}?)\s+(?:to|in|into|as|nach|zu|in)\s+([a-zäöüß°²/ €£$]{1,24}?)(?:\s+(?:um|umrechnen|please|bitte))?[?.!]*$")
});
static HOW_MANY: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:jai\s*,?\s*)?(?:how many|wie ?viele?)\s+([a-zäöüß°²/ €£$]{1,24}?)\s+(?:are\s+in|are|in|sind in|sind|hat|haben)\s+(?:a\s+|an\s+|one\s+|ein\s+|einer\s+|einem\s+)?(-?[\d.,]*)\s*([a-zäöüß°²/ €£$]{1,24}?)[?.!]*$")
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
    if let Some(line) = money(value, &from, &to, chrono::Local::now().date_naive()) {
        return Some(line);
    }
    let (out, from_name, to_name) = convert(value, &from, &to).ok()?;
    let plural = |n: f64, name: &str| if (n - 1.0).abs() < 1e-9 { name.trim_end_matches('s').to_string() } else { name.to_string() };
    // (A temperature to a tenth of a degree: "22.222 degrees" is nothing anyone says.)
    let digits = if to_name.starts_with("degrees") || to_name == "kelvin" { 10.0 } else { 1000.0 };
    Some(format!(
        "{} {} is {} {}, Sir.",
        format_number(value),
        if from_name.starts_with("degrees") || from_name == "kelvin" { from_name.to_string() } else { plural(value, from_name) },
        format_number((out * digits).round() / digits),
        if to_name.starts_with("degrees") || to_name == "kelvin" { to_name.to_string() } else { plural(out, to_name) },
    ))
}

// ----------------------------------------------------------------------- currencies ----

/// (ECB code, the names he says, the name said back.) "Pounds" is money only beside another currency.
const CURRENCIES: &[(&str, &[&str], &str)] = &[
    ("EUR", &["eur", "euro", "euros", "€"], "euros"),
    ("USD", &["usd", "dollar", "dollars", "us dollar", "us dollars", "$", "us-dollar"], "US dollars"),
    ("GBP", &["gbp", "pound", "pounds", "british pounds", "pound sterling", "pfund", "britische pfund", "£"], "pounds"),
    ("CHF", &["chf", "swiss franc", "swiss francs", "franken", "schweizer franken"], "Swiss francs"),
    ("JPY", &["jpy", "yen"], "yen"),
    ("CNY", &["cny", "yuan", "renminbi"], "yuan"),
    ("PLN", &["pln", "zloty", "złoty"], "zloty"),
    ("CZK", &["czk", "czech koruna", "czech crowns", "tschechische kronen"], "Czech crowns"),
    ("SEK", &["sek", "swedish krona", "swedish kronor", "schwedische kronen"], "Swedish kronor"),
    ("NOK", &["nok", "norwegian krone", "norwegian kroner", "norwegische kronen"], "Norwegian kroner"),
    ("DKK", &["dkk", "danish krone", "danish kroner", "dänische kronen"], "Danish kroner"),
    ("CAD", &["cad", "canadian dollar", "canadian dollars", "kanadische dollar"], "Canadian dollars"),
    ("AUD", &["aud", "australian dollar", "australian dollars", "australische dollar"], "Australian dollars"),
    ("TRY", &["lira", "turkish lira", "türkische lira"], "Turkish lira"),
    ("HUF", &["huf", "forint"], "forint"),
    ("INR", &["inr", "rupee", "rupees", "rupien"], "rupees"),
    ("KRW", &["krw", "won"], "won"),
];

/// The European Central Bank's reference rates: the day they are for, and units per euro.
static RATES: std::sync::RwLock<Option<(chrono::NaiveDate, Vec<(String, f64)>)>> = std::sync::RwLock::new(None);

fn currency(name: &str) -> Option<(&'static str, &'static str)> {
    let n = name.trim().trim_end_matches(['.', '?', '!']).to_lowercase();
    let n = n.strip_prefix("the ").unwrap_or(&n).trim();
    CURRENCIES.iter().find(|(_, names, _)| names.contains(&n)).map(|(code, _, said)| (*code, *said))
}

/// The day and rates of `eurofxref-daily.xml`.
pub fn parse_ecb(xml: &str) -> Option<(chrono::NaiveDate, Vec<(String, f64)>)> {
    static DAY: LazyLock<Regex> = LazyLock::new(|| re(r"time='(\d{4}-\d{2}-\d{2})'"));
    static RATE: LazyLock<Regex> = LazyLock::new(|| re(r"currency='([A-Z]{3})'\s+rate='([\d.]+)'"));
    let day = chrono::NaiveDate::parse_from_str(&DAY.captures(xml)?[1], "%Y-%m-%d").ok()?;
    let rates: Vec<(String, f64)> = RATE.captures_iter(xml).filter_map(|c| Some((c[1].to_string(), c[2].parse().ok().filter(|r: &f64| *r > 0.0)?))).collect();
    (!rates.is_empty()).then_some((day, rates))
}

/// Keeps the rates for the conversions (and in the kit store, for the next start offline).
pub fn set_rates(day: chrono::NaiveDate, rates: Vec<(String, f64)>, keep: bool) {
    if keep {
        let _ = crate::kit::put("fx_rates", "ecb", &serde_json::json!({ "day": day.to_string(), "rates": rates }));
    }
    *RATES.write().unwrap_or_else(|e| e.into_inner()) = Some((day, rates));
}

/// The day of the rates at hand (loaded from the kit store the first time).
pub fn rates_day() -> Option<chrono::NaiveDate> {
    if RATES.read().unwrap_or_else(|e| e.into_inner()).is_none() {
        let kept = crate::kit::get("fx_rates", "ecb")?;
        let day = chrono::NaiveDate::parse_from_str(kept["day"].as_str()?, "%Y-%m-%d").ok()?;
        let rates = serde_json::from_value(kept["rates"].clone()).ok()?;
        set_rates(day, rates, false);
    }
    RATES.read().unwrap_or_else(|e| e.into_inner()).as_ref().map(|(day, _)| *day)
}

/// "50 dollars in euros" at the ECB's rate; the rate's day is said once it is days old.
fn money(value: f64, from: &str, to: &str, today: chrono::NaiveDate) -> Option<String> {
    let ((from_code, from_said), (to_code, to_said)) = (currency(from)?, currency(to)?);
    let day = rates_day()?;
    let per_euro = |code: &str| -> Option<f64> { if code == "EUR" { Some(1.0) } else { RATES.read().ok()?.as_ref()?.1.iter().find(|(c, _)| c == code).map(|(_, r)| *r) } };
    let out = value / per_euro(from_code)? * per_euro(to_code)?;
    // (Fridays' rates serve the weekend; older than that is worth saying.)
    let when = if (today - day).num_days() > 3 { format!(", at the rate of {}", day.format("%-d %B")) } else { String::new() };
    let one = if (value - 1.0).abs() < 1e-9 { from_said.trim_end_matches('s') } else { from_said };
    Some(format!("{} {one} is {out:.2} {to_said}{when}, Sir.", format_number(value)))
}

// ---------------------------------------------------------------------------- dates ----

const MONTHS: [(&str, u32); 26] = [
    ("january", 1), ("february", 2), ("march", 3), ("april", 4), ("may", 5), ("june", 6), ("july", 7), ("august", 8), ("september", 9), ("october", 10),
    ("november", 11), ("december", 12), ("januar", 1), ("februar", 2), ("märz", 3), ("maerz", 3), ("mai", 5), ("juni", 6), ("juli", 7), ("oktober", 10),
    ("dezember", 12), ("jan", 1), ("feb", 2), ("aug", 8), ("sept", 9), ("dec", 12),
];
/// Days everybody calls by name: (name, month, day).
const NAMED_DAYS: [(&str, u32, u32); 12] = [
    ("christmas eve", 12, 24), ("christmas day", 12, 25), ("christmas", 12, 25), ("new years eve", 12, 31), ("new years day", 1, 1), ("new year", 1, 1), ("halloween", 10, 31),
    ("valentines day", 2, 14), ("heiligabend", 12, 24), ("weihnachten", 12, 25), ("silvester", 12, 31), ("neujahr", 1, 1),
];

/// "the 3rd of March", "the 10th of April 2027".
fn spoken_date(date: chrono::NaiveDate, today: chrono::NaiveDate) -> String {
    use chrono::Datelike;
    const NAMES: [&str; 12] = ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"];
    let day = date.day();
    let suffix = match (day % 10, day % 100) {
        (1, n) if n != 11 => "st",
        (2, n) if n != 12 => "nd",
        (3, n) if n != 13 => "rd",
        _ => "th",
    };
    let year = if date.year() == today.year() { String::new() } else { format!(" {}", date.year()) };
    format!("the {day}{suffix} of {}{year}", NAMES[date.month0() as usize])
}

/// A date as Sir says it: "March 3rd", "the 10th of April 2027", "3. März", "10.04.",
/// "2026-12-24", "tomorrow", "Christmas". Without a year: this year's (`after`: the next one on
/// or after that day). (the date, whether a year was given)
fn parse_date(phrase: &str, today: chrono::NaiveDate, after: Option<chrono::NaiveDate>) -> Option<(chrono::NaiveDate, bool)> {
    use chrono::{Datelike, NaiveDate};
    let s: String = phrase.to_lowercase().replace(['’', '\''], "").chars().map(|c| if c.is_alphanumeric() || c == '.' || c == '-' { c } else { ' ' }).collect();
    let s = s.split_whitespace().filter(|w| !matches!(*w, "the" | "of" | "dem" | "den" | "der" | "am" | "on" | "zum")).collect::<Vec<_>>().join(" ");
    let s = s.trim_end_matches('.').trim();
    match s {
        "today" | "heute" | "now" => return Some((today, true)),
        "tomorrow" | "morgen" => return today.succ_opt().map(|d| (d, true)),
        "yesterday" | "gestern" => return today.pred_opt().map(|d| (d, true)),
        _ => {}
    }
    // The year to take when none is said: this one, or the first that puts the day on or
    // after `after`.
    let place = |month: u32, day: u32, year: Option<i32>| -> Option<(NaiveDate, bool)> {
        match year {
            Some(y) => NaiveDate::from_ymd_opt(y, month, day).map(|d| (d, true)),
            None => {
                let this = NaiveDate::from_ymd_opt(today.year(), month, day)?;
                let date = match after {
                    Some(floor) if this < floor => (floor.year()..=floor.year() + 1).filter_map(|y| NaiveDate::from_ymd_opt(y, month, day)).find(|d| *d >= floor)?,
                    _ => this,
                };
                Some((date, false))
            }
        }
    };
    if let Some((_, month, day)) = NAMED_DAYS.iter().find(|(name, _, _)| s == *name) {
        return place(*month, *day, None);
    }
    static ISO: LazyLock<Regex> = LazyLock::new(|| re(r"^(\d{4})-(\d{1,2})-(\d{1,2})$"));
    static DOTTED: LazyLock<Regex> = LazyLock::new(|| re(r"^(\d{1,2})\.\s?(\d{1,2})\.?\s?(\d{4})?$"));
    static WORDED: LazyLock<Regex> = LazyLock::new(|| re(r"^(?:(\d{1,2})(?:st|nd|rd|th|\.)?\s+([a-zä]+)|([a-zä]+)\s+(\d{1,2})(?:st|nd|rd|th|\.)?)(?:\s+(\d{4}))?$"));
    if let Some(c) = ISO.captures(s) {
        return place(c[2].parse().ok()?, c[3].parse().ok()?, c[1].parse().ok());
    }
    if let Some(c) = DOTTED.captures(s) {
        return place(c[2].parse().ok()?, c[1].parse().ok()?, c.get(3).and_then(|y| y.as_str().parse().ok()));
    }
    let spelled = ordinal_digits(s);
    let c = WORDED.captures(&spelled)?;
    let (day, month) = match (c.get(1), c.get(2), c.get(3), c.get(4)) {
        (Some(d), Some(m), _, _) => (d.as_str(), m.as_str()),
        (_, _, Some(m), Some(d)) => (d.as_str(), m.as_str()),
        _ => return None,
    };
    let month = MONTHS.iter().find(|(name, _)| *name == month)?.1;
    place(month, day.parse().ok()?, c.get(5).and_then(|y| y.as_str().parse().ok()))
}

/// "The first of January", "der zwölfte Mai": a day said in words, as its number.
fn ordinal_digits(phrase: &str) -> String {
    const ENGLISH: [&str; 31] = [
        "first", "second", "third", "fourth", "fifth", "sixth", "seventh", "eighth", "ninth", "tenth", "eleventh", "twelfth", "thirteenth", "fourteenth", "fifteenth", "sixteenth", "seventeenth",
        "eighteenth", "nineteenth", "twentieth", "twenty first", "twenty second", "twenty third", "twenty fourth", "twenty fifth", "twenty sixth", "twenty seventh", "twenty eighth", "twenty ninth",
        "thirtieth", "thirty first",
    ];
    const GERMAN: [&str; 12] = ["erste", "zweite", "dritte", "vierte", "fünfte", "sechste", "siebte", "achte", "neunte", "zehnte", "elfte", "zwölfte"];
    let mut spoken = format!(" {} ", phrase.to_lowercase().replace('-', " "));
    // (The compounds first: "twenty first" is no "first".)
    for (at, word) in ENGLISH.iter().enumerate().rev() {
        spoken = spoken.replace(&format!(" {word} "), &format!(" {} ", at + 1));
    }
    for (at, word) in GERMAN.iter().enumerate() {
        for ending in ["", "n", "r"] {
            spoken = spoken.replace(&format!(" {word}{ending} "), &format!(" {} ", at + 1));
        }
    }
    spoken.trim().to_string()
}

/// "How many days are there between March 3rd and April 10th?", "how many days until
/// Christmas?", "wie viele Tage bis zum 24.12.?", "what day of the week is the 24th of
/// December?": counted on the calendar (a language model guesses these, and guessed 28 for 38).
pub fn answer_dates(text: &str, today: chrono::NaiveDate) -> Option<String> {
    use chrono::Datelike;
    static BETWEEN: LazyLock<Regex> = LazyLock::new(|| {
        re(r"^(?:jai\s*,?\s*)?(?:how many|wie ?viele?)\s+(days|weeks|tage|wochen)\s+(?:are\s+there\s+|are\s+|is\s+it\s+|sind\s+es\s+|liegen\s+)?(?:between|from|zwischen|von)\s+(.+?)\s+(?:and|to|until|till|und|bis)\s+(.+?)[?.!]*$")
    });
    static UNTIL: LazyLock<Regex> = LazyLock::new(|| {
        re(r"^(?:jai\s*,?\s*)?(?:how many|wie ?viele?)\s+(days|weeks|tage|wochen)\s+(?:are\s+there\s+|are\s+|is\s+it\s+|sind\s+es\s+)?(?:left\s+|noch\s+)?(?:until|till|to|before|bis zum|bis zu|bis)\s+(.+?)[?.!]*$|^(?:jai\s*,?\s*)?how long\s+(?:is it\s+)?(?:until|till)\s+(.+?)[?.!]*$")
    });
    static WEEKDAY: LazyLock<Regex> = LazyLock::new(|| {
        re(r"^(?:jai\s*,?\s*)?(?:what|which)\s+(?:day of the week|weekday|day)\s+(?:is|was|will be|will)\s+(?:it\s+on\s+|on\s+)?(.+?)(?:\s+be)?[?.!]*$|^(?:jai\s*,?\s*)?(?:welcher|was für ein)\s+(?:wochentag|tag)\s+(?:ist|war)\s+(?:der\s+|am\s+)?(.+?)[?.!]*$")
    });
    let lower = text.trim().to_lowercase();
    let count = |days: i64, unit: &str| -> String {
        if unit.starts_with('w') {
            let (weeks, rest) = (days / 7, days % 7);
            match (weeks, rest) {
                (w, 0) => format!("{w} week{}", if w == 1 { "" } else { "s" }),
                (0, d) => format!("{d} day{}", if d == 1 { "" } else { "s" }),
                (w, d) => format!("{w} week{} and {d} day{}", if w == 1 { "" } else { "s" }, if d == 1 { "" } else { "s" }),
            }
        } else {
            format!("{days} day{}", if days == 1 { "" } else { "s" })
        }
    };
    if let Some(c) = BETWEEN.captures(&lower) {
        let (from, _) = parse_date(&c[2], today, None)?;
        let (to, _) = parse_date(&c[3], today, Some(from))?;
        let days = (to - from).num_days().abs();
        return Some(format!("There {} {} between {} and {}, Sir.", if days == 1 { "is" } else { "are" }, count(days, &c[1]), spoken_date(from, today), spoken_date(to, today)));
    }
    if let Some(c) = UNTIL.captures(&lower) {
        let unit = c.get(1).map_or("days", |m| m.as_str());
        let (date, dated) = parse_date(c.get(2).or(c.get(3))?.as_str(), today, Some(today))?;
        let days = (date - today).num_days();
        return Some(match days {
            0 => "That is today, Sir.".to_string(),
            d if d < 0 && dated => format!("{} was {} ago, Sir.", capitalise(&spoken_date(date, today)), count(-d, unit)),
            d => format!("{} until {}, Sir.", capitalise(&count(d.abs(), unit)), spoken_date(date, today)),
        });
    }
    let c = WEEKDAY.captures(&lower)?;
    let phrase = c.get(1).or(c.get(2))?.as_str();
    // ("What day is it today" is the date's own question.)
    if !ordinal_digits(phrase).chars().any(|ch| ch.is_ascii_digit()) && !NAMED_DAYS.iter().any(|(name, _, _)| phrase.replace('\'', "").contains(name)) && !matches!(phrase.trim(), "tomorrow" | "morgen" | "yesterday" | "gestern") {
        return None;
    }
    let (date, _) = parse_date(phrase, today, None)?;
    let weekday = ["Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday", "Sunday"][date.weekday().num_days_from_monday() as usize];
    Some(format!("{} {} a {weekday}, Sir.", capitalise(&spoken_date(date, today)), if date < today { "was" } else { "is" }))
}

fn capitalise(s: &str) -> String {
    let mut c = s.chars();
    c.next().map(|f| f.to_uppercase().chain(c).collect()).unwrap_or_default()
}

/// "… to five decimal places", "… auf 3 Nachkommastellen": (what is left to calculate, how
/// many places).
fn decimal_places(text: &str) -> Option<(String, usize)> {
    static PLACES: LazyLock<Regex> = LazyLock::new(|| {
        re(r"^(.+?)\s*,?\s+(?:rounded\s+)?(?:to|with|auf|mit)\s+(\d{1,2}|one|two|three|four|five|six|seven|eight|nine|ten|eine|zwei|drei|vier|fünf|sechs)\s+(?:decimal places?|decimals|decimal digits|places after the (?:decimal )?point|nachkommastellen|stellen nach dem komma)[?.!]*$")
    });
    let lower = text.trim().to_lowercase();
    let c = PLACES.captures(&lower)?;
    let places = c[2].parse::<usize>().ok().or_else(|| ["one", "two", "three", "four", "five", "six", "seven", "eight", "nine", "ten"].iter().position(|w| *w == &c[2]).map(|i| i + 1)).or_else(|| ["eine", "zwei", "drei", "vier", "fünf", "sechs"].iter().position(|w| *w == &c[2]).map(|i| i + 1))?;
    Some((c[1].to_string(), places.min(12)))
}

/// Calculation, conversion or a count of days, answered in character; None when the text is
/// none of them.
pub fn try_answer(text: &str) -> Option<String> {
    if let Some(line) = answer_dates(text, chrono::Local::now().date_naive()) {
        return Some(line);
    }
    if let Some(line) = answer_conversion(text) {
        return Some(line);
    }
    // "… to five decimal places": exactly that many, however the rounding falls.
    if let Some((rest, places)) = decimal_places(text) {
        let (spoken, value) = answer_math(&rest)?;
        return Some(sentence(&spoken, &format!("{value:.places$}")));
    }
    let (spoken, value) = answer_math(text)?;
    Some(sentence(&spoken, &format_number(value)))
}

/// The answer as a sentence: "The square root of 144 is 12, Sir."
fn sentence(spoken: &str, value: &str) -> String {
    let mut letters = spoken.chars();
    let head: String = letters.next().map(|first| first.to_uppercase().chain(letters).collect()).unwrap_or_default();
    format!("{head} is {value}, Sir.")
}

// ------------------------------------------------------------------- linear equations ----

static EQUATION: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:jai\s*,?\s*)?(?:(?:is|ist)\s+x\s*=\s*(?P<v>-?[\d.]+)\s+(?:right|correct|a\s+solution|the\s+solution|richtig|korrekt|eine\s+lösung|die\s+lösung)\s+(?:for|of|to|in|für|von|bei)\s+|(?:check|prüfe?|teste?)\s+x\s*=\s*(?P<w>-?[\d.]+)\s+(?:for|in|against|für|bei)\s+|(?:solve|löse|loese|(?:what\s+is|whats)\s+x\s+(?:if|when|in|for)|was\s+ist\s+x\s+(?:wenn|bei|in|für))\s+(?:the\s+equation\s+|die\s+gleichung\s+)?)(?P<eq>[^=]+=[^=]+?)[?.!]*$")
});

/// "3x + 5 = 20" as the parser reads it: words to signs, "3x" → "3*x", "2(x+1)" → "2*(x+1)".
fn equation_signs(text: &str) -> String {
    static TIMES_X: LazyLock<Regex> = LazyLock::new(|| re(r"(\d|\))\s*(x|\()"));
    let mut s = format!(" {} ", fix_commas(&text.to_lowercase()));
    for (from, to) in [
        (" is equal to ", " = "), (" equal to ", " = "), (" equals ", " = "), (" ist gleich ", " = "), (" gleich ", " = "), (" plus ", " + "), (" minus ", " - "),
        (" times ", " * "), (" mal ", " * "), (" divided by ", " / "), (" geteilt durch ", " / "), (" durch ", " / "), ("×", "*"), ("·", "*"), ("−", "-"), ("÷", "/"),
    ] {
        s = s.replace(from, to);
    }
    TIMES_X.replace_all(s.trim(), "$1*$2").into_owned()
}

/// "Is x = 5 right for 3x + 5 = 20", "solve 2(x + 1) = x + 7", "ist x = 4 richtig für 3x + 5 = 20":
/// the equation, and the value to check. Only equations in x, of numbers and + - * / ^ ( ).
pub fn equation_command(original: &str) -> Option<serde_json::Value> {
    let lower = original.to_lowercase();
    if !lower.contains('=') && !lower.contains("equal") && !lower.contains("gleich") {
        return None;
    }
    let signs = equation_signs(original.trim());
    let c = EQUATION.captures(&signs)?;
    let eq = c.name("eq")?.as_str().trim().to_string();
    let fits = eq.contains('x') && eq.chars().all(|ch| ch.is_ascii_digit() || " .+-*/^()x=".contains(ch));
    let check = c.name("v").or(c.name("w")).and_then(|v| v.as_str().parse::<f64>().ok());
    let german = ["ist ", "prüf", "test", "lös", "loes", "was "].iter().any(|w| signs.starts_with(w));
    fits.then(|| serde_json::json!({"equation": eq, "check": check, "german": german}))
}

/// One side `a·x + b` of an equation: (a, b), or None when it is not of the first degree.
fn linear(side: &str) -> Option<(f64, f64)> {
    let at = |x: f64| evaluate(&side.replace('x', &format!("({x})"))).ok();
    let (b, one) = (at(0.0)?, at(1.0)?);
    let a = one - b;
    let near = |p: f64, q: f64| (p - q).abs() <= 1e-9 * (1.0 + p.abs().max(q.abs()));
    (near(at(2.0)?, 2.0 * a + b) && near(at(-3.0)?, b - 3.0 * a)).then_some((a, b))
}

/// "3x", "-x", "x", "0.5x".
fn times_x(a: f64) -> String {
    match format_number(a).as_str() {
        "1" => "x".to_string(),
        "-1" => "-x".to_string(),
        n => format!("{n}x"),
    }
}

/// Solves or checks a linear equation in x: (the steps, one line to say).
pub fn solve_equation(eq: &str, check: Option<f64>, german: bool) -> Result<(String, String), String> {
    let (left, right) = eq.split_once('=').ok_or("That is no equation, Sir.")?;
    let ((al, bl), (ar, br)) = linear(left).zip(linear(right)).ok_or("I solve equations of the first degree only, Sir: x may not be squared or divided by.")?;
    let (a, rest) = (al - ar, br - bl);
    let mut steps = vec![eq.replace("*x", "x").replace("*(", "(").replace('*', "·")];
    if bl != 0.0 || ar != 0.0 {
        steps.push(format!("{} = {}", times_x(a), format_number(rest)));
    }
    if a == 0.0 {
        let line = match (rest == 0.0, german) {
            (true, false) => "Every x solves it, Sir: both sides are always equal.",
            (true, true) => "Jedes x ist eine Lösung, Sir.",
            (false, false) => "No x solves it, Sir: the x cancels out and the numbers disagree.",
            (false, true) => "Kein x löst sie, Sir.",
        };
        return Ok((steps.join("\n"), line.to_string()));
    }
    let x = rest / a;
    if a != 1.0 {
        steps.push(format!("x = {}   (÷ {})", format_number(x), format_number(a)));
    }
    let Some(v) = check else {
        let line = if german { format!("x ist {}, Sir.", format_number(x)) } else { format!("x is {}, Sir.", format_number(x)) };
        return Ok((steps.join("\n"), line));
    };
    let (l, r) = (al * v + bl, ar * v + br);
    let (lv, rv, xv, vv) = (format_number(l), format_number(r), format_number(x), format_number(v));
    let right_one = (l - r).abs() <= 1e-9 * (1.0 + l.abs().max(r.abs()));
    steps.insert(0, format!("x = {vv}:  left {lv}, right {rv}  ({})", if right_one { "equal" } else { "not equal" }));
    let line = match (right_one, german) {
        (true, false) => format!("Yes, Sir: {vv} is right. Both sides come to {lv}."),
        (true, true) => format!("Ja, Sir: {vv} stimmt. Beide Seiten ergeben {lv}."),
        (false, false) => format!("No, Sir: with {vv} the left side is {lv} and the right {rv}. x is {xv}."),
        (false, true) => format!("Nein, Sir: mit {vv} ist links {lv} und rechts {rv}. x ist {xv}."),
    };
    Ok((steps.join("\n"), line))
}

/// The `equation` kit tool: the steps on the board, one line said.
pub fn equation(args: &serde_json::Value) -> Result<String, String> {
    let eq = args["equation"].as_str().unwrap_or("");
    let (steps, line) = solve_equation(eq, args["check"].as_f64(), args["german"] == true)?;
    crate::kit::show_text("equation", "EQUATION // STEPS", &steps, [380.0, 150.0]);
    Ok(crate::kit::say(format!("{steps}\n{line}"), line))
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
    fn a_linear_equation_is_checked_and_solved_with_its_steps() {
        let asked = |said: &str| equation_command(said);
        let eq = |said: &str| asked(said).and_then(|a| a["equation"].as_str().map(str::to_string)).unwrap_or_else(|| panic!("{said}"));
        assert_eq!(asked("Is x = 5 right for 3x + 5 = 20?"), Some(serde_json::json!({"equation": "3*x + 5 = 20", "check": 5.0, "german": false})));
        assert_eq!(asked("Ist x gleich 4 richtig für 3x plus 5 gleich 20"), Some(serde_json::json!({"equation": "3*x + 5 = 20", "check": 4.0, "german": true})));
        assert_eq!(eq("solve 2(x + 1) = x + 7"), "2*(x + 1) = x + 7");
        assert_eq!(eq("What is x if 4x minus 3 equals 2x plus 9?"), "4*x - 3 = 2*x + 9");
        assert_eq!(eq("Löse 0,5x + 1 = 3"), "0.5*x + 1 = 3");
        for said in ["what is 3 + 5", "solve my problems", "is it right for me", "x = 5", "solve a = 5", "check the weather = good", "is x = 5 right for y + 2 = 7"] {
            assert_eq!(asked(said), None, "{said}");
        }
        let solved = |eq: &str, check: Option<f64>| solve_equation(eq, check, false).unwrap();
        assert_eq!(solved("3*x + 5 = 20", None), ("3x + 5 = 20\n3x = 15\nx = 5   (÷ 3)".to_string(), "x is 5, Sir.".to_string()));
        assert_eq!(solved("2*(x + 1) = x + 7", None).1, "x is 5, Sir.");
        assert_eq!(solved("4*x - 3 = 2*x + 9", None).0, "4x - 3 = 2x + 9\n2x = 12\nx = 6   (÷ 2)");
        assert_eq!(solved("x = 7", None), ("x = 7".to_string(), "x is 7, Sir.".to_string()), "nothing to move, nothing to divide");
        assert_eq!(solved("0.5*x + 1 = 3", None).1, "x is 4, Sir.");
        assert_eq!(solved("-x = 4", None).1, "x is -4, Sir.");
        assert_eq!(solved("3*x + 5 = 20", Some(5.0)).1, "Yes, Sir: 5 is right. Both sides come to 20.");
        assert_eq!(solved("3*x + 5 = 20", Some(4.0)).1, "No, Sir: with 4 the left side is 17 and the right 20. x is 5.");
        assert!(solved("3*x + 5 = 20", Some(4.0)).0.starts_with("x = 4:  left 17, right 20  (not equal)"));
        assert_eq!(solve_equation("3*x + 5 = 20", Some(5.0), true).unwrap().1, "Ja, Sir: 5 stimmt. Beide Seiten ergeben 20.");
        assert!(solved("x + 1 = x + 1", None).1.starts_with("Every x"));
        assert!(solved("x + 1 = x + 2", None).1.starts_with("No x"));
        for not_linear in ["x^2 = 4", "1/x = 2", "x*x + 1 = 5"] {
            assert!(solve_equation(not_linear, None, false).is_err(), "{not_linear}");
        }
    }

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
        assert_eq!(try_answer("what is the square root of 144").unwrap(), "The square root of 144 is 12, Sir.");
        assert_eq!(try_answer("what is 2 to the power of 10").unwrap(), "2 to the power of 10 is 1024, Sir.");
        assert_eq!(try_answer("1,000 divided by 8").unwrap(), "1000 divided by 8 is 125, Sir.");
        assert!(try_answer("What is the meaning of life?").is_none());
        assert!(try_answer("What's the weather in Berlin").is_none());
        assert!(try_answer("Open Spotify").is_none());
        assert!(try_answer("What time is it").is_none());
    }

    /// The silent session of 2026-11-06: the language model answered "28 days" for the 38
    /// between March 3rd and April 10th, and called the calculator with debris for a square
    /// root "to five decimal places".
    #[test]
    fn days_are_counted_on_the_calendar_and_decimals_are_as_many_as_asked() {
        let today = chrono::NaiveDate::from_ymd_opt(2026, 11, 6).unwrap();
        let ask = |text: &str| answer_dates(text, today);
        assert_eq!(ask("How many days are there between March 3rd and April 10th?").unwrap(), "There are 38 days between the 3rd of March and the 10th of April, Sir.");
        assert_eq!(ask("how many days between the 20th of December and January 5th").unwrap(), "There are 16 days between the 20th of December and the 5th of January 2027, Sir.");
        assert_eq!(ask("How many weeks between 2026-01-01 and 2026-03-01?").unwrap(), "There are 8 weeks and 3 days between the 1st of January and the 1st of March, Sir.");
        assert_eq!(ask("Wie viele Tage zwischen dem 3. März und dem 10.04.?").unwrap(), "There are 38 days between the 3rd of March and the 10th of April, Sir.");
        assert_eq!(ask("How many days until Christmas?").unwrap(), "49 days until the 25th of December, Sir.");
        assert_eq!(ask("how many days until new year's eve").unwrap(), "55 days until the 31st of December, Sir.");
        assert_eq!(ask("How many days until March 1st?").unwrap(), "115 days until the 1st of March 2027, Sir.", "the next one");
        assert_eq!(ask("how many days until tomorrow").unwrap(), "1 day until the 7th of November, Sir.");
        assert_eq!(ask("How many days until November 6th?").unwrap(), "That is today, Sir.");
        assert_eq!(ask("how many days until 2026-01-01").unwrap(), "The 1st of January was 309 days ago, Sir.");
        assert_eq!(ask("Wie viele Tage bis zum 24.12.?").unwrap(), "48 days until the 24th of December, Sir.");
        assert_eq!(ask("What day of the week is the 24th of December?").unwrap(), "The 24th of December is a Thursday, Sir.");
        assert_eq!(ask("Which weekday was 2026-01-01?").unwrap(), "The 1st of January was a Thursday, Sir.");
        // No dates: none of its business.
        for text in ["how many days are in a week", "how many weeks to learn Rust", "what day is it", "what day is it today", "how many grams in a pound", "how long until dinner"] {
            assert_eq!(ask(text), None, "{text}");
        }
        assert_eq!(try_answer("What is the square root of 2 to five decimal places?").unwrap(), "The square root of 2 is 1.41421, Sir.");
        assert_eq!(try_answer("calculate 10 divided by 3 to 2 decimal places").unwrap(), "10 divided by 3 is 3.33, Sir.");
        assert_eq!(try_answer("Was ist die Wurzel aus 2 auf drei Nachkommastellen?").unwrap(), "The square root of 2 is 1.414, Sir.");
    }

    #[test]
    fn converts_units() {
        assert_eq!(try_answer("Convert 5 miles to km").unwrap(), "5 miles is 8.047 kilometres, Sir.");
        assert_eq!(try_answer("100 fahrenheit in celsius").unwrap(), "100 degrees Fahrenheit is 37.8 degrees Celsius, Sir.");
        assert_eq!(try_answer("Convert 72 degrees Fahrenheit to Celsius.").unwrap(), "72 degrees Fahrenheit is 22.2 degrees Celsius, Sir.");
        assert_eq!(try_answer("Wie viel sind 3 Pfund in Kilo?").unwrap(), "3 pounds is 1.361 kilograms, Sir.");
        assert_eq!(try_answer("how many grams in a pound").unwrap(), "1 pound is 453.592 grams, Sir.");
        assert_eq!(try_answer("what is 1 gb in mb").unwrap(), "1 gigabyte is 1000 megabytes, Sir.");
        assert!(convert(1.0, "kg", "metres").is_err());
        assert_eq!(tool("2*(3+4)").unwrap(), "2*(3+4) = 14");
    }

    #[test]
    fn days_said_in_words_and_counts_asked_with_are() {
        assert!(try_answer("What day of the week is the first of January 2030?").unwrap().ends_with("a Tuesday, Sir."));
        assert!(try_answer("What day is the twenty first of March 2030?").unwrap().ends_with("a Thursday, Sir."));
        assert!(try_answer("Welcher Wochentag ist der erste Mai 2030?").unwrap().ends_with("a Wednesday, Sir."));
        assert_eq!(ordinal_digits("the second of May"), "the 2 of may");
        assert_eq!(ordinal_digits("twenty-first March"), "21 march");
        assert_eq!(ordinal_digits("zwölften Juni"), "12 juni");
        assert_eq!(try_answer("How many kilometres are 26 miles?").unwrap(), "26 miles is 41.843 kilometres, Sir.");
        assert!(try_answer("How many days are there until the 10th of April 2099?").unwrap().contains("until"));
        // A count of something else is not arithmetic.
        for question in ["How many people are in the room?", "How many planets are there?", "What is the first thing to do?", "Which day is the best one?"] {
            assert_eq!(try_answer(question), None, "{question}");
        }
    }

    /// Money at the ECB's rate (a copy of its daily file); weight stays weight.
    #[test]
    fn money_is_converted_at_the_central_bank_s_rate() {
        let xml = "<Cube><Cube time='2026-11-13'>\n<Cube currency='USD' rate='1.1000'/>\n<Cube currency='GBP' rate='0.8000'/>\n<Cube currency='JPY' rate='175.00'/></Cube></Cube>";
        let (day, rates) = parse_ecb(xml).unwrap();
        assert_eq!((day.to_string(), rates.len()), ("2026-11-13".to_string(), 3));
        set_rates(day, rates, false);
        let friday = chrono::NaiveDate::from_ymd_opt(2026, 11, 13).unwrap();
        assert_eq!(money(50.0, "dollars", "euros", friday).as_deref(), Some("50 US dollars is 45.45 euros, Sir."));
        assert_eq!(money(1.0, "€", "pounds", friday + chrono::Duration::days(3)).as_deref(), Some("1 euro is 0.80 pounds, Sir."), "Monday: Friday's rate");
        assert_eq!(money(100.0, "pfund", "dollar", friday + chrono::Duration::days(6)).as_deref(), Some("100 pounds is 137.50 US dollars, at the rate of 13 November, Sir."));
        assert!(answer_conversion("how much is 50 dollars in euros").is_some_and(|l| l.starts_with("50 US dollars is ")));
        assert_eq!(crate::phrasings::route("How much is 50 dollars in euros?").map(|r| r.0).as_deref(), Some("(answer)"), "answered at once, not searched");
        assert_eq!(answer_conversion("convert 3 pounds to kilograms").as_deref(), Some("3 pounds is 1.361 kilograms, Sir."));
        assert_eq!(money(5.0, "dollars", "rubles", friday), None, "not on the ECB's list");
        assert_eq!(parse_ecb("<html>down</html>"), None);
    }
}
