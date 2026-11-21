//! The pronunciation lexicon: every reply is respelled for the British voice before eSpeak and
//! Kokoro see it, so JAI never spells out "J A R V I S", reads underscores, backslashes or
//! "15h" aloud, or butchers app names, acronyms, units and times.
//!
//! German words stay German: the phonemizer gives them German pronunciation in the same voice;
//! `is_german_term` adds the places and school words it should recognise.

use regex::{Captures, Regex};
use std::sync::LazyLock;

pub struct PronunciationLexicon;

const ONES: [&str; 20] = [
    "zero", "one", "two", "three", "four", "five", "six", "seven", "eight", "nine", "ten", "eleven", "twelve", "thirteen", "fourteen", "fifteen", "sixteen",
    "seventeen", "eighteen", "nineteen",
];
const TENS: [&str; 10] = ["", "", "twenty", "thirty", "forty", "fifty", "sixty", "seventy", "eighty", "ninety"];

/// 0-99 in words ("forty-two").
pub fn two_digits(n: u32) -> String {
    match n {
        0..=19 => ONES[n as usize].to_string(),
        _ if n.is_multiple_of(10) => TENS[(n / 10) as usize % 10].to_string(),
        _ => format!("{}-{}", TENS[(n / 10) as usize % 10], ONES[(n % 10) as usize]),
    }
}

/// A model or resolution number the way people say it: 3060 → "thirty sixty", 1440 →
/// "fourteen forty", 720 → "seven twenty", 1500 → "fifteen hundred", 1905 → "nineteen oh five".
pub fn pair_words(n: u32) -> String {
    let (head, tail) = (n / 100, n % 100);
    if head == 0 {
        return two_digits(tail);
    }
    let head = two_digits(head);
    match tail {
        0 => format!("{head} hundred"),
        1..=9 => format!("{head} oh {}", ONES[tail as usize]),
        _ => format!("{head} {}", two_digits(tail)),
    }
}

/// A 24-hour time spoken on the 12-hour clock: 15:00 → "three P M", 07:50 → "seven fifty A M".
pub fn clock(h: u32, m: u32) -> String {
    match (h, m) {
        (0, 0) => return "midnight".into(),
        (12, 0) => return "noon".into(),
        _ => {}
    }
    let (h12, suffix) = match h {
        0 => (12, "A M"),
        1..=11 => (h, "A M"),
        12 => (12, "P M"),
        _ => (h - 12, "P M"),
    };
    let hour = two_digits(h12);
    match m {
        0 => format!("{hour} {suffix}"),
        // (As "what time is it" says them: `brain::spoken_time`.)
        15 => format!("a quarter past {hour} {suffix}"),
        30 => format!("half past {hour} {suffix}"),
        45 => format!("a quarter to {}", clock((h + 1) % 24, 0)),
        1..=9 => format!("{hour} oh {} {suffix}", ONES[m as usize]),
        _ => format!("{hour} {} {suffix}", two_digits(m)),
    }
}

fn spaced_letters(s: &str) -> String {
    s.chars().filter(|c| c.is_alphanumeric()).map(|c| c.to_ascii_uppercase().to_string()).collect::<Vec<_>>().join(" ")
}

/// A file extension aloud.
fn extension_word(ext: &str) -> String {
    match ext.to_lowercase().as_str() {
        "json" => "Jason".into(),
        "toml" => "tommel".into(),
        "txt" => "text".into(),
        "yaml" | "yml" => "yammel".into(),
        "jpg" | "jpeg" => "J peg".into(),
        "log" => "log".into(),
        "zip" => "zip".into(),
        "bat" => "bat".into(),
        "go" => "go".into(),
        "docx" => "doc X".into(),
        "ps1" => "P S one".into(),
        other => spaced_letters(other),
    }
}

const CODE_EXTENSIONS: &str = "rs|py|js|ts|tsx|jsx|json|toml|md|txt|exe|dll|cpp|hpp|h|c|cs|java|go|html|css|bat|ps1|yaml|yml|log|db|png|jpg|jpeg|pdf|docx|xlsx|csv|zip|wav|mp3|mp4|ini|cfg|sh|lock";

static USER_PATH: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"(?i)\b[a-z]:\\users\\[^\s"'<>|]*"#).unwrap());
static WINDOWS_PATH: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"\b[A-Za-z]:\\[^\s"'<>|]+"#).unwrap());
static RELATIVE_PATH: LazyLock<Regex> = LazyLock::new(|| Regex::new(&format!(r"(?:[\w.-]+[/\\])+([\w-]+)\.({CODE_EXTENSIONS})\b")).unwrap());
static FILE_NAME: LazyLock<Regex> = LazyLock::new(|| Regex::new(&format!(r"\b([\w-]+)\.({CODE_EXTENSIONS})\b")).unwrap());

/// "src/brain.rs" → "brain dot R S", "C:\Users\…" → "your user folder".
fn paths(s: &str) -> String {
    // (The folder under his own: "your Documents folder"; never his user name.)
    let s = USER_PATH.replace_all(s, |c: &Captures| match c[0].split('\\').nth(3).filter(|f| !f.is_empty() && !f.contains('.')) {
        Some(folder) => format!("your {folder} folder"),
        None => "your user folder".to_string(),
    });
    let s = WINDOWS_PATH.replace_all(&s, |c: &Captures| {
        let last = c[0].trim_end_matches('\\').rsplit('\\').next().unwrap_or("").to_string();
        match last.rsplit_once('.') {
            Some((name, ext)) if !name.is_empty() && ext.len() <= 5 => format!("{name} dot {}", extension_word(ext)),
            _ if !last.is_empty() => format!("the {last} folder"),
            _ => "that folder".into(),
        }
    });
    let s = RELATIVE_PATH.replace_all(&s, |c: &Captures| format!("{} dot {}", &c[1], extension_word(&c[2])));
    FILE_NAME.replace_all(&s, |c: &Captures| format!("{} dot {}", &c[1], extension_word(&c[2]))).into_owned()
}

static NAMES: LazyLock<Vec<(Regex, &'static str)>> = LazyLock::new(|| {
    [
        // His name is said "Jay", however it is written; a sentence it ends keeps its stop.
        (r"\bJ\.\s?A\.\s?I\.(\s*)$", "Jay.$1"),
        (r"\bJ\.\s?A\.\s?I\b\.?", "Jay"),
        (r"\bJAI\b", "Jay"),
        (r"\bJai\b", "Jay"),
        (r"(?i)\bsonora\b", "Suh-nora"),
        (r"(?i)\bspotify\b", "Spot-ify"),
        (r"(?i)\bweb ?untis\b", "Web-Oontis"),
        (r"(?i)\bitslearning\b", "its learning"),
        (r"(?i)\bjaysrock\b|\bjay'?s rock\b", "Jays Rock"),
        (r"\begui\b", "ee-gooey"),
        (r"(?i)\bsqlite\b", "S-Q-Lite"),
        (r"(?i)\bwi-?fi\b", "Why-Fy"),
        (r"\bVRAM\b", "V-Ram"),
        (r"\bRAM\b", "Ram"),
        (r"\bCPUs\b", "C P Us"),
        (r"\bCPU\b", "C P U"),
        (r"\bGPUs\b", "G P Us"),
        (r"\bGPU\b", "G P U"),
        (r"\bGUI\b", "Gooey"),
        (r"\bAPIs\b", "A P Is"),
        (r"\bAPI\b", "A P I"),
        (r"\bUI\b", "U I"),
        (r"(?i)\bfps\b", "F P S"),
        (r"\bOLED\b", "O L E D"),
        (r"\bLED\b", "L E D"),
        (r"\bCLI\b", "C L I"),
        (r"\bIDE\b", "I D E"),
        (r"\bPh\.?D\.?", "P H D"),
        (r"\bLOC\b", "lines of code"),
        (r"\bSSD\b", "S S D"),
        (r"\bUSB\b", "U S B"),
        (r"\bHDMI\b", "H D M I"),
        (r"\bLLM\b", "L L M"),
        (r"\bURL\b", "U R L"),
        (r"\bPC\b", "P C"),
    ]
    .into_iter()
    .map(|(p, r)| (Regex::new(p).expect("valid name pattern"), r))
    .collect()
});

/// The words Sir taught him ("say Arkham like ark-um"), said his way, before every other rule.
fn taught(s: &str) -> String {
    crate::kit::list(crate::kit_voice::SAY_LIKE).into_iter().fold(s.to_string(), |s, (word, way, _)| {
        match (Regex::new(&format!(r"(?i)\b{}\b", regex::escape(&word))), way.as_str()) {
            (Ok(re), Some(way)) => re.replace_all(&s, regex::NoExpand(way)).into_owned(),
            _ => s,
        }
    })
}

static GPU_MODEL: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\b(RTX|GTX|RX)\s?(\d{4})(\s?(?:Ti|XT|Super))?\b").unwrap());
static RESOLUTION: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\b(360|480|540|720|900|1080|1200|1440|1600|2160|4320)p\b").unwrap());

/// "RTX 3060 Ti" → "R T X thirty sixty T I", "1440p" → "fourteen forty P".
fn hardware(s: &str) -> String {
    let s = GPU_MODEL.replace_all(s, |c: &Captures| {
        let n: u32 = c[2].parse().unwrap_or(0);
        let suffix = match c.get(3).map(|m| m.as_str().trim().to_lowercase()) {
            Some(x) if x == "ti" => " T I".to_string(),
            Some(x) if x == "xt" => " X T".to_string(),
            Some(_) => " Super".to_string(),
            None => String::new(),
        };
        format!("{} {}{suffix}", spaced_letters(&c[1]), pair_words(n))
    });
    let s = RESOLUTION.replace_all(&s, |c: &Captures| format!("{} P", pair_words(c[1].parse().unwrap_or(0))));
    SCREEN_SIZE.replace_all(&s, |c: &Captures| format!("{} by {}", pair_words(c[1].parse().unwrap_or(0)), pair_words(c[2].parse().unwrap_or(0)))).into_owned()
}

/// "1920x1080" → "nineteen twenty by ten eighty".
static SCREEN_SIZE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\b(\d{3,4})\s?[x×]\s?(\d{3,4})\b").unwrap());

static TIME_COLON: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\b([01]?\d|2[0-3]):([0-5]\d)\b").unwrap());
static TIME_MERIDIEM: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)\b(1[0-2]|0?[1-9])(?::([0-5]\d))?\s?([ap])(?:m\b|\.m\b\.?)").unwrap());
static TIME_H: LazyLock<Regex>= LazyLock::new(|| Regex::new(r"\b(\d{1,2})h([0-5]\d)?\b").unwrap());

/// The word before a match.
fn word_before(s: &str, at: usize) -> String {
    s[..at].split_whitespace().last().unwrap_or("").trim_matches(|c: char| !c.is_alphanumeric()).to_lowercase()
}

/// "15:00" / "15h" → "three P M"; "07:50" → "seven fifty A M"; "for 8h" → "8 hours".
fn times(s: &str) -> String {
    // "3 pm", "7:05 a.m.": the 12-hour clock as it is written (a sentence-ending stop stays).
    let s = TIME_MERIDIEM.replace_all(s, |c: &Captures| {
        let whole = c.get(0).expect("whole match");
        let (h, m): (u32, u32) = (c[1].parse().unwrap_or(0), c.get(2).and_then(|m| m.as_str().parse().ok()).unwrap_or(0));
        let h24 = match (c[3].to_lowercase().as_str(), h) {
            ("a", 12) => 0,
            ("p", 1..=11) => h + 12,
            _ => h,
        };
        let rest = s[whole.end()..].trim_start();
        let stop = whole.as_str().ends_with('.') && (rest.is_empty() || rest.starts_with(char::is_uppercase));
        format!("{}{}", clock(h24, m), if stop { "." } else { "" })
    }).into_owned();
    let s = TIME_COLON
        .replace_all(&s, |c: &Captures| {
            let m = c.get(0).expect("whole match");
            // "1:00:00" and "10:30:15" are durations, "3:1" never matched.
            let before = s[..m.start()].chars().last();
            let after = s[m.end()..].chars().next();
            if before == Some(':') || after == Some(':') {
                return m.as_str().to_string();
            }
            clock(c[1].parse().unwrap_or(0), c[2].parse().unwrap_or(0))
        })
        .into_owned();
    TIME_H
        .replace_all(&s, |c: &Captures| {
            let whole = c.get(0).expect("whole match");
            let h: u32 = c[1].parse().unwrap_or(99);
            let m: u32 = c.get(2).and_then(|m| m.as_str().parse().ok()).unwrap_or(0);
            if h > 23 {
                return whole.as_str().to_string();
            }
            let before = word_before(&s, whole.start());
            let at_time = matches!(before.as_str(), "at" | "um" | "by" | "until" | "till" | "from" | "bis" | "ab" | "gegen" | "before");
            if at_time || (h >= 13 && !matches!(before.as_str(), "in" | "takes" | "about" | "around" | "over" | "under")) {
                clock(h, m)
            } else {
                format!("{h} hours")
            }
        })
        .into_owned()
}

static SNAKE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\b[A-Za-z0-9]+(?:_[A-Za-z0-9]+)+\b").unwrap());
static CAMEL: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\b[a-z]{2,}(?:[A-Z][a-z0-9]*)+\b").unwrap());
static PASCAL: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\b(?:[A-Z][a-z0-9]+){3,}\b").unwrap());

fn split_humps(word: &str) -> String {
    let mut out = String::new();
    for (i, ch) in word.chars().enumerate() {
        if i > 0 && ch.is_uppercase() {
            out.push(' ');
        }
        out.push(ch);
    }
    out
}

/// "hands_free_mode" → "hands free mode", "camelCase" → "camel Case",
/// "SpatialWindowKind" → "Spatial Window Kind" ("WhatsApp", "YouTube" stay).
fn identifiers(s: &str) -> String {
    let s = SNAKE.replace_all(s, |c: &Captures| c[0].replace('_', " "));
    let s = CAMEL.replace_all(&s, |c: &Captures| split_humps(&c[0]));
    PASCAL.replace_all(&s, |c: &Captures| split_humps(&c[0])).into_owned()
}

static UNITS: LazyLock<Vec<(Regex, &'static str)>> = LazyLock::new(|| {
    [
        (r"(\d)\s?km/h\b", "$1 kilometres per hour"),
        (r"(\d)\s?mph\b", "$1 miles per hour"),
        (r"(\d)\s?MB/s\b", "$1 megabytes per second"),
        (r"(\d)\s?GB/s\b", "$1 gigabytes per second"),
        (r"(\d)\s?M(?:bps|bit/s)\b", "$1 megabits per second"),
        (r"(\d)\s?G(?:bps|bit/s)\b", "$1 gigabits per second"),
        (r"(\d)\s?kHz\b", "$1 kilohertz"),
        (r"\bkHz\b", "kilohertz"),
        (r"(\d)\s?MHz\b", "$1 megahertz"),
        (r"(\d)\s?GHz\b", "$1 gigahertz"),
        (r"(\d)\s?Hz\b", "$1 Hertz"),
        (r"\bHz\b", "Hertz"),
        (r"\b1\s?ms\b", "1 millisecond"),
        (r"(\d)\s?ms\b", "$1 milliseconds"),
        (r"\b1\s?TB\b", "1 terabyte"),
        (r"\b1\s?GB\b", "1 gigabyte"),
        (r"\b1\s?MB\b", "1 megabyte"),
        (r"\b1\s?KB\b", "1 kilobyte"),
        (r"(\d)\s?TB\b", "$1 terabytes"),
        (r"(\d)\s?GB\b", "$1 gigabytes"),
        (r"(\d)\s?MB\b", "$1 megabytes"),
        (r"(\d)\s?KB\b", "$1 kilobytes"),
        (r"\bTB\b", "terabytes"),
        (r"\bGB\b", "gigabytes"),
        (r"\bMB\b", "megabytes"),
        (r"\bKB\b", "kilobytes"),
        (r"(\d)\s?km\b", "$1 kilometres"),
        (r"(\d)\s?kg\b", "$1 kilograms"),
        (r"(\d)\s?cm\b", "$1 centimetres"),
        (r"(\d)\s?mm\b", "$1 millimetres"),
        (r"(\d)\s?kWh\b", "$1 kilowatt hours"),
        (r"(\d)\s?kW\b", "$1 kilowatts"),
        (r"(\d)\s?mAh\b", "$1 milliamp hours"),
        (r"(\d)\s?min\b", "$1 minutes"),
        (r"(\d)\s?secs?\b", "$1 seconds"),
        (r"(\d)\s?hrs?\b", "$1 hours"),
        (r"\s?°\s?C\b", " degrees Celsius"),
        (r"(\d)\s?%", "$1 percent"),
        (r"\s?%", " percent"),
        (r"\s*&\s*", " and "),
        (r"\band/or\b", "and or"),
    ]
    .into_iter()
    .map(|(p, r)| (Regex::new(p).expect("valid unit pattern"), r))
    .collect()
});

static WORD_SLASH: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\b([A-Za-z]{2,})/([A-Za-z]{2,})\b").unwrap());
const UNIT_TOKENS: [&str; 20] = ["km", "kg", "wh", "kwh", "kb", "mb", "gb", "tb", "mi", "ms", "hr", "sec", "min", "mbit", "gbit", "fps", "rpm", "eur", "usd", "ml"];

/// "a/b/c" and "a::b::c" are paths: "a slash b slash c", "a b c".
static PATH_CHAIN: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\b\w+(?:/\w+){2,}\b|\b\w+(?:::\w+)+\b").unwrap());

/// "read/write" → "read or write" ("Wh/kg" stays a unit); a path of three parts or more is
/// read part by part.
fn slashes(s: &str) -> String {
    // (A date, "12/05/2026", is no path.)
    let s = PATH_CHAIN.replace_all(s, |c: &Captures| if c[0].contains(char::is_alphabetic) { c[0].replace("::", " ").replace('/', " slash ") } else { c[0].to_string() });
    WORD_SLASH
        .replace_all(&s, |c: &Captures| {
            if UNIT_TOKENS.contains(&c[1].to_lowercase().as_str()) || UNIT_TOKENS.contains(&c[2].to_lowercase().as_str()) {
                c[0].to_string()
            } else {
                format!("{} or {}", &c[1], &c[2])
            }
        })
        .into_owned()
}

static LIVE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)\blive\b").unwrap());
/// Words after which "live" is the verb /lɪv/: "I live", "to live", "where do you live",
/// "they'd rather not live", "long live the king".
const LIVE_AS_VERB_AFTER: [&str; 27] = [
    "to", "i", "you", "we", "they", "will", "would", "can", "could", "should", "do", "don't", "didn't", "doesn't", "does", "did", "must",
    "might", "may", "shall", "cannot", "won't", "wouldn't", "couldn't", "shouldn't", "long", "who",
];
/// The perfect and the passive: "have read", "has already read", "was read" → "red".
static READ_PAST: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(\b(?:have|has|had|having|been|was|were)|'ve|’ve)(\s+(?:already|just|not|never|also|finally|recently|now|all))?\s+read\b").unwrap()
});

/// Homographs eSpeak and Kokoro get wrong in JAI's lines.
///
/// * "live" as an adjective or adverb ("live telemetry", "live data", "currently live", "the
///   interface is live") is /laɪv/ like "five": spelt "lyve", which both read that way. The verb
///   stays "live": after a subject pronoun, "to" or a modal ("I live in Germany", "where do you
///   live") and before "in a" / "in the" ("people who live in the city").
/// * "read" after have / has / had / been / was / were is the past tense: "red".
fn homographs(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 8);
    let mut last = 0;
    for m in LIVE.find_iter(s) {
        let before = word_before(s, m.start()).replace('’', "'");
        let after = s[m.end()..].trim_start().to_lowercase();
        // ("Jürgen Schröder live on Hauptstraße": a name before it is a subject.)
        let prev: Vec<&str> = s[..m.start()].split_whitespace().rev().take(2).collect();
        let named = prev.len() == 2 && prev[0].starts_with(char::is_uppercase) && !prev[1].ends_with(['.', '!', '?']);
        let verb = LIVE_AS_VERB_AFTER.contains(&before.as_str()) || ["in a ", "in an ", "in the "].iter().any(|p| after.starts_with(p));
        // eSpeak reads a "live" with no pronoun before it as the adjective: these it must be told ("liv").
        let said_liv = !verb && (["near ", "with ", "here", "there", "together", "alone", "abroad", "nearby"].iter().any(|p| after.starts_with(p)) || named && after.starts_with("on "));
        out.push_str(&s[last..m.start()]);
        let word = m.as_str();
        out.push_str(if verb {
            word
        } else if said_liv {
            "liv"
        } else if word.chars().next().is_some_and(char::is_uppercase) && word.chars().skip(1).all(char::is_lowercase) {
            "Lyve"
        } else {
            "lyve"
        });
        last = m.end();
    }
    out.push_str(&s[last..]);
    let out = READ_PAST.replace_all(&out, "${1}${2} red").into_owned();
    // "I read it yesterday": a time in the past puts "read" in the past ("Did you read it yesterday?" stays).
    let out = READ_THEN
        .replace_all(&out, |c: &Captures| {
            let whole = c.get(0).expect("whole match");
            let from = out[..whole.start()].rfind(['.', '!', '?']).map_or(0, |i| i + 1);
            let to = out[whole.end()..].find(['.', '!', '?']).map_or(out.len(), |i| whole.end() + i);
            let past = READ_TIMES.iter().any(|t| out[from..to].to_lowercase().contains(t));
            if past && !READ_AUX.contains(&word_before(&out, whole.start()).as_str()) { format!("{} red", &c[1]) } else { whole.as_str().to_string() }
        })
        .into_owned();
    // "Close the door", "Please record it": the verb, which eSpeak reads as the adjective / the noun at a sentence's start.
    let out = IMPERATIVE
        .replace_all(&out, |c: &Captures| {
            let verb = match c[2].to_lowercase().as_str() {
                "close" => "cloze",
                "wind" => "wined",
                _ => "rekord",
            };
            let cap = c[2].starts_with(char::is_uppercase);
            format!("{}{}{}", &c[1], if cap { verb[..1].to_uppercase() + &verb[1..] } else { verb.to_string() }, &c[3])
        })
        .into_owned();
    // The metal: "lead pipes" (the noun "lead" of "take the lead" is /liːd/).
    LEAD_METAL.replace_all(&out, |c: &Captures| format!("{}{}", if c[1].starts_with(char::is_uppercase) { "Led" } else { "led" }, &c[2])).into_owned()
}

static READ_THEN: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)\b(I|we|you|he|she|they|who)\s+read\b").unwrap());
const READ_TIMES: [&str; 7] = ["yesterday", "last night", "last week", "last month", "last year", " ago", "the other day"];
const READ_AUX: [&str; 12] = ["did", "didn't", "do", "don't", "does", "doesn't", "will", "would", "can", "could", "should", "to"];
static IMPERATIVE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)((?:^|[.!?]\s+|\b(?:please|and|then|just)[,]?\s+))(close|record|wind)(\s+(?:the|it|all|this|that|your|my|every|a|an|these|those|them|any|both|me|us|everything|another)\b)").unwrap()
});
static LEAD_METAL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\b(lead)(\s+(?:pipes?|paint|poisoning|shot|weights?|acid|batter(?:y|ies)|free|crystal|glass|sheets?|plates?|apron|shielding)\b)").unwrap());

/// A sentence each, with what ends it and the space after.
static SENTENCE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?s).+?(?:[.!?]+(?:\s+|$)|$)").unwrap());
static DE_TIME: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\b([01]?\d|2[0-3]):([0-5]\d)(\s?Uhr\b)?").unwrap());
static DE_UNITS: LazyLock<Vec<(Regex, &'static str)>> = LazyLock::new(|| {
    [
        (r"\s?°\s?C\b", " Grad"),
        (r"(\d)\s?%", "$1 Prozent"),
        (r"\s?%", " Prozent"),
        (r"(\d+(?:[.,]\d+)?)\s?(?:EUR\b|€)", "$1 Euro"),
        (r"(?:\bEUR|€)\s?(\d+(?:[.,]\d+)?)", "$1 Euro"),
        (r"\s*&\s*", " und "),
    ]
    .into_iter()
    .map(|(p, r)| (Regex::new(p).expect("valid unit pattern"), r))
    .collect()
});

/// A time as a German sentence says it, in digits (eSpeak reads them in German inside a German
/// sentence): 10:30 → "halb 11", 9:15 → "Viertel nach 9", 14:20 → "14 Uhr 20".
fn de_clock(h: u32, m: u32) -> String {
    let h12 = |h: u32| match h % 12 {
        0 => 12,
        x => x,
    };
    match (h, m) {
        (0, 0) => "Mitternacht".into(),
        (_, 0) => format!("{h} Uhr"),
        (_, 15) => format!("Viertel nach {}", h12(h)),
        (_, 30) => format!("halb {}", h12(h + 1)),
        (_, 45) => format!("Viertel vor {}", h12(h + 1)),
        _ => format!("{h} Uhr {m}"),
    }
}

/// A German sentence's times and units in German, before the English rules see them.
fn german(s: &str) -> String {
    let mut s = DE_TIME
        .replace_all(s, |c: &Captures| {
            let m = c.get(0).expect("whole match");
            // ("1:00:00" is a duration.)
            if s[..m.start()].ends_with(':') || s[m.end()..].starts_with(':') {
                return m.as_str().to_string();
            }
            de_clock(c[1].parse().unwrap_or(0), c[2].parse().unwrap_or(0))
        })
        .into_owned();
    for (re, rep) in DE_UNITS.iter() {
        s = re.replace_all(&s, *rep).into_owned();
    }
    s
}

/// His apps' and games' titles a reader would stumble over, as they are said ("Civilization
/// VI" → "Civilization six"). Learnt whenever the apps are indexed (`learn_titles`).
static TITLES: std::sync::RwLock<Vec<(String, String)>> = std::sync::RwLock::new(Vec::new());

/// A title's spoken form when it ends on a Roman numeral; None when it reads as it stands.
fn title_spoken(title: &str) -> Option<String> {
    const ROMAN: [&str; 14] = ["II", "III", "IV", "V", "VI", "VII", "VIII", "IX", "X", "XI", "XII", "XIII", "XIV", "XV"];
    let (head, last) = title.trim().rsplit_once(' ')?;
    let n = ROMAN.iter().position(|r| *r == last)? as u32 + 2;
    Some(format!("{head} {}", two_digits(n)))
}

/// Learns the titles of his apps and games that need another spelling to be said right.
pub fn learn_titles(titles: impl IntoIterator<Item = String>) {
    *TITLES.write().unwrap_or_else(|e| e.into_inner()) = learnable(titles);
}

/// The titles to learn, with how each is said; the longest first ("Final Fantasy XIV" before a
/// "Final Fantasy X").
fn learnable(titles: impl IntoIterator<Item = String>) -> Vec<(String, String)> {
    let mut learnt: Vec<(String, String)> = titles.into_iter().filter_map(|t| title_spoken(&t).map(|said| (t, said))).collect();
    learnt.sort_by_key(|(t, _)| std::cmp::Reverse(t.len()));
    learnt.dedup();
    learnt
}

/// The titles he knows, said as they are said.
fn titles(s: &str) -> String {
    titles_with(&TITLES.read().unwrap_or_else(|e| e.into_inner()), s)
}

fn titles_with(learnt: &[(String, String)], s: &str) -> String {
    let mut s = s.to_string();
    for (title, said) in learnt.iter() {
        let mut from = 0;
        while let Some(at) = s[from..].find(title.as_str()).map(|i| i + from) {
            let end = at + title.len();
            // (Whole words only: "GTA V" is not in "GTA Vice City".)
            if s[..at].ends_with(char::is_alphanumeric) || s[end..].starts_with(char::is_alphanumeric) {
                from = end;
            } else {
                s.replace_range(at..end, said);
                from = at + said.len();
            }
        }
    }
    s
}

const MONTHS: [&str; 12] = ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"];

/// 21 → "21st" (eSpeak reads it "twenty first").
fn ordinal(n: u32) -> String {
    let suffix = match (n % 100, n % 10) {
        (11..=13, _) => "th",
        (_, 1) => "st",
        (_, 2) => "nd",
        (_, 3) => "rd",
        _ => "th",
    };
    format!("{n}{suffix}")
}

/// A year the way it is said: 1999 → "nineteen ninety nine", 2026 → "twenty twenty six", 2005 → "two thousand and five".
fn year_words(y: u32) -> String {
    match y {
        2000 => "two thousand".into(),
        2001..=2009 => format!("two thousand and {}", ONES[(y - 2000) as usize]),
        _ => pair_words(y),
    }
}

/// The month a three-letter (or "Sept") abbreviation names.
fn month_of(abbr: &str) -> Option<&'static str> {
    let key = abbr.to_lowercase();
    MONTHS.iter().find(|m| m.to_lowercase().starts_with(&key[..3.min(key.len())])).copied()
}

const MONTH_ABBR: &str = "Jan|Feb|Mar|Apr|Jun|Jul|Aug|Sept?|Oct|Nov|Dec";
static DATE_DOTTED: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\b(\d{1,2})\.(\d{1,2})\.(\d{4})\b").unwrap());
static DATE_ISO: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\b(\d{4})-(\d{2})-(\d{2})\b").unwrap());
static MONTH_DAY: LazyLock<Regex> = LazyLock::new(|| Regex::new(&format!(r"\b({MONTH_ABBR})\.?\s+(\d{{1,2}})(?:st|nd|rd|th)?\b")).unwrap());
static DAY_MONTH: LazyLock<Regex> = LazyLock::new(|| Regex::new(&format!(r"\b(\d{{1,2}})(?:st|nd|rd|th)?\s+({MONTH_ABBR})\b\.?")).unwrap());
static NUMBER_RANGE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\b(\d{1,4})\s?[-–]\s?(\d{1,4})\b").unwrap());
static YEAR: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\b(1[1-9]\d\d|20\d\d)\b").unwrap());
/// Words that put a four-digit number in the calendar ("in 1999", "since 2026", "from 1990 to 2000").
const YEAR_AFTER: [&str; 12] = ["in", "since", "year", "until", "till", "during", "circa", "from", "to", "by", "before", "after"];
/// What follows a four-digit number that is a quantity, not a year ("in 1500 ms").
const NOT_A_YEAR_BEFORE: [&str; 20] = [
    "ms", "mb", "gb", "kb", "tb", "hz", "khz", "mhz", "ghz", "fps", "p", "w", "kg", "km", "users", "items", "files", "times", "people", "words",
];

fn full_date(day: &str, month: &str, year: &str) -> Option<String> {
    let (d, m): (u32, usize) = (day.parse().ok()?, month.parse().ok()?);
    ((1..=31).contains(&d) && (1..=12).contains(&m)).then(|| format!("the {} of {} {year}", ordinal(d), MONTHS[m - 1]))
}

/// "21.11.2026" → "the 21st of November 2026", "Nov 21" → "November 21st", "in 1999" → "in nineteen ninety nine",
/// "3-5" → "3 to 5". English sentences only (a German sentence keeps its digits for the German voice).
fn dates(s: &str) -> String {
    let s = DATE_DOTTED.replace_all(s, |c: &Captures| full_date(&c[1], &c[2], &c[3]).unwrap_or_else(|| c[0].to_string()));
    let s = DATE_ISO.replace_all(&s, |c: &Captures| full_date(&c[3], &c[2], &c[1]).unwrap_or_else(|| c[0].to_string()));
    let s = MONTH_DAY.replace_all(&s, |c: &Captures| match (month_of(&c[1]), c[2].parse::<u32>()) {
        (Some(month), Ok(d)) if (1..=31).contains(&d) => format!("{month} {}", ordinal(d)),
        _ => c[0].to_string(),
    });
    let s = DAY_MONTH
        .replace_all(&s, |c: &Captures| match (month_of(&c[2]), c[1].parse::<u32>()) {
            (Some(month), Ok(d)) if (1..=31).contains(&d) => format!("{} {month}", ordinal(d)),
            _ => c[0].to_string(),
        })
        .into_owned();
    let s = NUMBER_RANGE.replace_all(&s, "$1 to $2").into_owned();
    YEAR.replace_all(&s, |c: &Captures| {
        let at = c.get(0).expect("whole match");
        let before = word_before(&s, at.start());
        let after = s[at.end()..].split_whitespace().next().unwrap_or("").trim_matches(|c: char| !c.is_alphanumeric()).to_lowercase();
        // ("in 1999 and 2026": the second year follows the first.)
        let chained = matches!(before.as_str(), "and" | "or") && s[..at.start()].split_whitespace().rev().nth(1).and_then(|w| w.trim_matches(|c: char| !c.is_ascii_digit()).parse::<u32>().ok()).is_some_and(|y| (1100..=2099).contains(&y));
        let dated = chained
            || YEAR_AFTER.contains(&before.as_str())
            || MONTHS.iter().any(|m| m.to_lowercase() == before)
            || before.starts_with(|c: char| c.is_ascii_digit()) && ["st", "nd", "rd", "th"].iter().any(|x| before.ends_with(x));
        if dated && !NOT_A_YEAR_BEFORE.contains(&after.as_str()) { year_words(c[1].parse().unwrap_or(0)) } else { c[0].to_string() }
    })
    .into_owned()
}

const AMOUNT: &str = r"\d+(?:,\d{3})*(?:[.,]\d+)?";
const CURRENCY: &str = r"[$£€]|\b(?:EUR|USD|GBP)\b";
static MONEY: LazyLock<Regex> = LazyLock::new(|| Regex::new(&format!(r"(?:({CURRENCY})\s?({AMOUNT}))|(?:({AMOUNT})\s?({CURRENCY}))")).unwrap());
static LONE_CURRENCY: LazyLock<Regex> = LazyLock::new(|| Regex::new(CURRENCY).unwrap());

/// A sum as it is said ("$3.50" → "3 dollars and 50 cents"); without an amount just the currency's name.
/// (The euro is respelled: eSpeak reads "euro" as "yaw-roh".)
fn spoken_money(symbol: &str, amount: &str) -> String {
    let (one, many, cents) = match symbol {
        "$" | "USD" => ("dollar", "dollars", "cents"),
        "£" | "GBP" => ("pound", "pounds", "pence"),
        _ => ("ewro", "ewros", "cents"),
    };
    if amount.is_empty() {
        return many.to_string();
    }
    let (major, minor) = match amount.rsplit_once(['.', ',']) {
        Some((m, f)) if f.len() <= 2 => (m, f.parse::<u32>().unwrap_or(0) * if f.len() == 1 { 10 } else { 1 }),
        _ => (amount, 0),
    };
    match (major, minor) {
        ("0", c) if c > 0 => format!("{c} {cents}"),
        (m, 0) => format!("{m} {}", if m == "1" { one } else { many }),
        (m, c) => format!("{m} {} and {c} {cents}", if m == "1" { one } else { many }),
    }
}

/// "$3.50", "5€", "30 EUR", "£2" in words.
fn money(s: &str) -> String {
    let s = MONEY.replace_all(s, |c: &Captures| match (c.get(1), c.get(2)) {
        (Some(symbol), Some(amount)) => spoken_money(symbol.as_str(), amount.as_str()),
        _ => spoken_money(&c[4], &c[3]),
    });
    LONE_CURRENCY.replace_all(&s, |c: &Captures| spoken_money(&c[0], "")).into_owned()
}

impl PronunciationLexicon {
    /// The text as the voice should say it.
    pub fn normalize_for_speech(text: &str) -> String {
        // Times and units follow the language of their sentence.
        let text: String = SENTENCE.find_iter(text).map(|m| if crate::phonemizer::sentence_is_german(m.as_str()) { german(m.as_str()) } else { dates(m.as_str()) }).collect();
        let mut s = homographs(&paths(&taught(&titles(&text))));
        for (re, rep) in NAMES.iter() {
            s = re.replace_all(&s, *rep).into_owned();
        }
        s = money(&hardware(&s));
        s = times(&s);
        s = identifiers(&s);
        for (re, rep) in UNITS.iter() {
            s = re.replace_all(&s, *rep).into_owned();
        }
        s = slashes(&s);
        s.split_whitespace().collect::<Vec<_>>().join(" ")
    }
}

/// Places and school words the phonemizer must read in German ("Freiburg", "Gymnasium",
/// "Oberstufe"). Sir's own town is not written here (no private data in the code): it comes
/// from his home (`home_reads_german`), the towns round it from their German endings
/// (`phonemizer::is_german_word`). Surnames and first names without an umlaut are here too: an
/// English reading of "Meyer" or "Wolfgang" is wrong in a German name.
const GERMAN_PLACES_AND_TERMS: &[&str] = &[
    "freiburg", "karlsruhe", "stuttgart", "münchen", "muenchen", "köln", "koeln", "schwarzwald", "württemberg", "gymnasium", "oberstufe", "kursstufe",
    "mündlich", "schriftlich", "lehrerzimmer", "mensa", "aula", "sekretariat", "referat", "notenschnitt", "deutschprobeklausur", "probeklausur",
    "berlin", "hamburg", "frankfurt", "dresden", "leipzig", "heidelberg", "hannover", "bremen", "dortmund", "essen", "bonn", "mannheim", "augsburg",
    "wiesbaden", "mainz", "kassel", "ulm", "konstanz", "potsdam", "rostock", "kiel", "erfurt", "salzburg", "wien", "bern", "basel", "innsbruck", "graz",
    "linz", "nuernberg", "wuerzburg", "dusseldorf", "duesseldorf", "zuerich", "luebeck", "deutschland", "bayern", "sachsen", "hessen", "schweiz",
    "brandenburg", "niedersachsen", "westfalen", "bundesliga", "autobahn", "oktoberfest",
    "meyer", "meier", "maier", "mayer", "becker", "weber", "wagner", "fischer", "richter", "bauer", "werner", "schulz", "schulze", "schubert", "schuster",
    "klaus", "wolfgang", "dieter", "helmut", "heinz", "hans", "gerhard", "ulrich", "friedrich", "heinrich", "matthias",
];

pub fn is_german_term(key: &str) -> bool {
    GERMAN_PLACES_AND_TERMS.contains(&key) || crate::location_engine::known_home().is_some_and(|home| home_reads_german(&home, key))
}

/// His home town is read in German where home lies in the German-speaking countries.
fn home_reads_german(home: &crate::location_engine::Home, key: &str) -> bool {
    // shortcut: a box round Germany, Austria and Switzerland (it takes in a few border towns of
    // the neighbours); the geocoder's country code when that matters.
    (45.8..=55.1).contains(&home.lat) && (5.8..=17.2).contains(&home.lon) && home.town.as_deref().is_some_and(|t| t.to_lowercase() == key)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::phonemizer::{language_runs, Lang};

    fn say(s: &str) -> String {
        PronunciationLexicon::normalize_for_speech(s)
    }

    #[test]
    fn the_screenshot_sentence_is_speakable() {
        let out = say("J.A.I. uses 0 MB VRAM in SQLite on WebUntis at 15h");
        assert_eq!(out, "Jay uses 0 megabytes V-Ram in S-Q-Lite on Web-Oontis at three P M");
    }

    #[test]
    fn live_is_lyve_unless_it_is_the_verb() {
        assert_eq!(say("Live telemetry is up and the interface is live."), "Lyve telemetry is up and the interface is lyve.");
        assert_eq!(say("live stream, live data, currently live, interface live"), "lyve stream, lyve data, currently lyve, interface lyve");
        assert_eq!(say("We're LIVE now."), "We're lyve now.");
        // The verb keeps /lɪv/.
        assert_eq!(say("I live in Germany."), "I live in Germany.");
        assert_eq!(say("Where do you live, and where would they live?"), "Where do you live, and where would they live?");
        assert_eq!(say("You don't live far; people live in the city."), "You don't live far; people live in the city.");
        assert_eq!(say("I want to live"), "I want to live");
        // Livestream and lived are other words.
        assert_eq!(say("the livestream lived on"), "the livestream lived on");
    }

    #[test]
    fn past_tense_read_is_red() {
        assert_eq!(say("I have read the report."), "I have red the report.");
        assert_eq!(say("She has already read it, and it was read aloud; I've read it too."), "She has already red it, and it was red aloud; I've red it too.");
        assert_eq!(say("I will read it and you read it daily."), "I will read it and you read it daily.");
    }

    #[test]
    fn names_acronyms_and_hardware() {
        assert_eq!(say("JAI opened Spotify and Sonora"), "Jay opened Spot-ify and Suh-nora");
        // His name is "Jay" to the ear, whichever way a line writes it.
        for written in ["J.A.I. is online", "JAI is online", "Jai is online", "J. A. I. is online"] {
            assert_eq!(say(written), "Jay is online", "{written}");
        }
        assert_eq!(say("I am J.A.I."), "I am Jay.");
        assert_eq!(say("They call me JAI, Sir."), "They call me Jay, Sir.");
        assert_eq!(say("itslearning and JaysRock over Wi-Fi"), "its learning and Jays Rock over Why-Fy");
        assert_eq!(say("the egui GUI shows CPU, GPU and RAM"), "the ee-gooey Gooey shows C P U, G P U and Ram");
        assert_eq!(say("the UI calls the API"), "the U I calls the A P I");
        assert_eq!(say("your RTX 3060 Ti runs 1440p at 144 FPS and 1080p at 240 Hz"), "your R T X thirty sixty T I runs fourteen forty P at 144 F P S and ten eighty P at 240 Hertz");
        assert_eq!(say("48 kHz with 12 ms latency, 2,000 LOC"), "48 kilohertz with 12 milliseconds latency, 2,000 lines of code");
        assert_eq!(say("1 GB free of 512 GB, 4 KB pages"), "1 gigabyte free of 512 gigabytes, 4 kilobytes pages");
    }

    #[test]
    fn times_numbers_and_symbols() {
        assert_eq!(say("Lesson at 15:00, bus at 07:50, lunch 12:00"), "Lesson at three P M, bus at seven fifty A M, lunch noon");
        assert_eq!(say("meeting at 9h30"), "meeting at half past nine A M");
        assert_eq!(title_spoken("Civilization VI").as_deref(), Some("Civilization six"));
        assert_eq!(title_spoken("Final Fantasy XIV").as_deref(), Some("Final Fantasy fourteen"));
        assert_eq!((title_spoken("Battlefield 2042"), title_spoken("Portal")), (None, None));
        let learnt = learnable(["GTA V".to_string(), "Civilization VI".to_string(), "Portal".to_string()]);
        assert_eq!(learnt.len(), 2, "only what needs another spelling");
        assert_eq!(titles_with(&learnt, "Civilization VI and GTA V, not GTA Vice City."), "Civilization six and GTA five, not GTA Vice City.");
        assert_eq!(say("at 10:15, 22:45 and 11:45"), "at a quarter past ten A M, a quarter to eleven P M and a quarter to noon");
        // A German sentence keeps its times and units German.
        assert_eq!(say("Der Bus fährt um 10:30 Uhr, es sind 22°C und 85% Akku."), "Der Bus fährt um halb 11, es sind 22 Grad und 85 Prozent Akku.");
        assert_eq!(say("Die Stunde beginnt um 9:15 und kostet 5€ & mehr."), "Die Stunde beginnt um Viertel nach 9 und kostet 5 Euro und mehr.");
        assert_eq!(say("Lunch at 12:30. Der Termin ist um 14:20."), "Lunch at half past twelve P M. Der Termin ist um 14 Uhr 20.");
        assert_eq!(say("it took 8h and 1:00:00 total"), "it took 8 hours and 1:00:00 total");
        assert_eq!(say("22°C, 85% load, 30 EUR, 5€ & tips"), "22 degrees Celsius, 85 percent load, 30 ewros, 5 ewros and tips");
        assert_eq!(say("read/write access, 25 Wh/kg"), "read or write access, 25 Wh/kg");
        assert_eq!(clock(0, 5), "twelve oh five A M");
        assert_eq!(pair_words(1905), "nineteen oh five");
    }

    #[test]
    fn paths_and_identifiers_are_never_read_raw() {
        assert_eq!(say("I patched src/brain.rs"), "I patched brain dot R S");
        assert_eq!(say(r"Saved to C:\Users\max\Documents\notes.txt"), "Saved to your Documents folder");
        assert_eq!(say(r"Saved to C:\Users\max\notes.txt"), "Saved to your user folder");
        assert_eq!(say(r"Saved to C:\Users\max"), "Saved to your user folder");
        // A path of three parts is read part by part, a Rust path word by word; a date stays.
        assert_eq!(say("look in assets/fonts/display"), "look in assets slash fonts slash display");
        assert_eq!(say("call kit::say first"), "call kit say first");
        assert!(!say("on 12/05/2026").contains("slash"));
        assert_eq!(say(r"It is in D:\Code\jai_config.json"), "It is in jai config dot Jason");
        assert_eq!(say("hands_free_mode is on, see camelCase and SpatialWindowKind"), "hands free mode is on, see camel Case and Spatial Window Kind");
        assert_eq!(say("WhatsApp and YouTube stay"), "WhatsApp and YouTube stay");
    }

    #[test]
    fn german_words_keep_german_pronunciation() {
        // The lexicon leaves German words alone …
        let text = "Your Deutschprobeklausur in LK Mathematik is in Raum 204 in Freiburg, Musterheim is close.";
        assert_eq!(say(text), text);
        // … and the phonemizer reads them in German with the same voice (a town by its ending).
        let runs = language_runs("Your Deutschprobeklausur in LK Mathematik is in Raum 204 near Freiburg and Musterheim");
        let german: Vec<&str> = runs.iter().filter(|(l, _)| *l == Lang::De).map(|(_, t)| t.as_str()).collect();
        let joined = german.join(" ");
        for word in ["Deutschprobeklausur", "Mathematik", "Raum", "Freiburg", "Musterheim"] {
            assert!(joined.contains(word), "{word} should be German: {runs:?}");
        }
        assert!(!joined.contains("Your"));
    }

    /// His own town is German by his home, not by a list in the code.
    #[test]
    fn his_home_town_is_read_in_german_where_home_is_german_speaking() {
        let home = crate::location_engine::Home { lat: 48.3, lon: 7.9, accuracy_m: 25.0, town: Some("Musterstadt".into()), source: "home", confirmed: true };
        assert!(home_reads_german(&home, "musterstadt"));
        assert!(!home_reads_german(&home, "london"));
        let abroad = crate::location_engine::Home { lat: 51.5, lon: -0.1, town: Some("Musterstadt".into()), ..home };
        assert!(!home_reads_german(&abroad, "musterstadt"));
    }
}
