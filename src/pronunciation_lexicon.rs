//! The pronunciation lexicon: every reply is respelled for the British voice before eSpeak and
//! Kokoro see it, so JAI never spells out "J A R V I S", reads underscores, backslashes or
//! "15h" aloud, or butchers app names, acronyms, units and times.
//!
//! German words stay German: the phonemizer gives them German pronunciation in the same voice;
//! `is_german_term` adds the local places and school words it should recognise.

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
    let s = USER_PATH.replace_all(s, "your user folder");
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
        (r"\bFPS\b", "F P S"),
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
    RESOLUTION.replace_all(&s, |c: &Captures| format!("{} P", pair_words(c[1].parse().unwrap_or(0)))).into_owned()
}

static TIME_COLON: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\b([01]?\d|2[0-3]):([0-5]\d)\b").unwrap());
static TIME_H: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\b(\d{1,2})h([0-5]\d)?\b").unwrap());

/// The word before a match.
fn word_before(s: &str, at: usize) -> String {
    s[..at].split_whitespace().last().unwrap_or("").trim_matches(|c: char| !c.is_alphanumeric()).to_lowercase()
}

/// "15:00" / "15h" → "three P M"; "07:50" → "seven fifty A M"; "for 8h" → "8 hours".
fn times(s: &str) -> String {
    let s = TIME_COLON
        .replace_all(s, |c: &Captures| {
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
        (r"\s?°\s?C\b", " degrees Celsius"),
        (r"(\d)\s?%", "$1 percent"),
        (r"\s?%", " percent"),
        (r"(\d+(?:[.,]\d+)?)\s?(?:EUR\b|€)", "$1 Euros"),
        (r"(?:\bEUR|€)\s?(\d+(?:[.,]\d+)?)", "$1 Euros"),
        (r"\bEUR\b|€", "Euros"),
        (r"\s*&\s*", " and "),
        (r"\band/or\b", "and or"),
    ]
    .into_iter()
    .map(|(p, r)| (Regex::new(p).expect("valid unit pattern"), r))
    .collect()
});

static WORD_SLASH: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\b([A-Za-z]{2,})/([A-Za-z]{2,})\b").unwrap());
const UNIT_TOKENS: [&str; 20] = ["km", "kg", "wh", "kwh", "kb", "mb", "gb", "tb", "mi", "ms", "hr", "sec", "min", "mbit", "gbit", "fps", "rpm", "eur", "usd", "ml"];

/// "read/write" → "read or write" ("Wh/kg" stays a unit).
fn slashes(s: &str) -> String {
    WORD_SLASH
        .replace_all(s, |c: &Captures| {
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
        let verb = LIVE_AS_VERB_AFTER.contains(&before.as_str()) || ["in a ", "in an ", "in the "].iter().any(|p| after.starts_with(p));
        out.push_str(&s[last..m.start()]);
        let word = m.as_str();
        out.push_str(if verb {
            word
        } else if word.chars().next().is_some_and(char::is_uppercase) && word.chars().skip(1).all(char::is_lowercase) {
            "Lyve"
        } else {
            "lyve"
        });
        last = m.end();
    }
    out.push_str(&s[last..]);
    READ_PAST.replace_all(&out, "${1}${2} red").into_owned()
}

impl PronunciationLexicon {
    /// The text as the voice should say it.
    pub fn normalize_for_speech(text: &str) -> String {
        let mut s = homographs(&paths(&taught(text)));
        for (re, rep) in NAMES.iter() {
            s = re.replace_all(&s, *rep).into_owned();
        }
        s = hardware(&s);
        s = times(&s);
        s = identifiers(&s);
        for (re, rep) in UNITS.iter() {
            s = re.replace_all(&s, *rep).into_owned();
        }
        s = slashes(&s);
        s.split_whitespace().collect::<Vec<_>>().join(" ")
    }
}

/// Local places and school words the phonemizer must read in German ("Musterstadt", "Beispielheim",
/// "Gymnasium", "Oberstufe").
const GERMAN_PLACES_AND_TERMS: [&str; 34] = [
    "musterstadt", "beispielheim", "musterburg", "freiburg", "karlsruhe", "nebenheim", "musterheim", "musterberg", "ringheim", "musterbach", "mustertal", "musterhafen",
    "musteringen", "stuttgart", "münchen", "muenchen", "köln", "koeln", "schwarzwald", "musterkreis", "württemberg", "gymnasium", "oberstufe", "kursstufe",
    "mündlich", "schriftlich", "lehrerzimmer", "mensa", "aula", "sekretariat", "referat", "notenschnitt", "deutschprobeklausur", "probeklausur",
];

pub fn is_german_term(key: &str) -> bool {
    GERMAN_PLACES_AND_TERMS.contains(&key)
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
        assert_eq!(say("meeting at 9h30"), "meeting at nine thirty A M");
        assert_eq!(say("it took 8h and 1:00:00 total"), "it took 8 hours and 1:00:00 total");
        assert_eq!(say("22°C, 85% load, 30 EUR, 5€ & tips"), "22 degrees Celsius, 85 percent load, 30 Euros, 5 Euros and tips");
        assert_eq!(say("read/write access, 25 Wh/kg"), "read or write access, 25 Wh/kg");
        assert_eq!(clock(0, 5), "twelve oh five A M");
        assert_eq!(pair_words(1905), "nineteen oh five");
    }

    #[test]
    fn paths_and_identifiers_are_never_read_raw() {
        assert_eq!(say("I patched src/brain.rs"), "I patched brain dot R S");
        assert_eq!(say(r"Saved to C:\Users\max\Documents\notes.txt"), "Saved to your user folder");
        assert_eq!(say(r"It is in D:\Code\jai_config.json"), "It is in jai config dot Jason");
        assert_eq!(say("hands_free_mode is on, see camelCase and SpatialWindowKind"), "hands free mode is on, see camel Case and Spatial Window Kind");
        assert_eq!(say("WhatsApp and YouTube stay"), "WhatsApp and YouTube stay");
    }

    #[test]
    fn german_words_keep_german_pronunciation() {
        // The lexicon leaves German words alone …
        let text = "Your Deutschprobeklausur in LK Mathematik is in Raum 204 in Musterstadt, Beispielheim is close.";
        assert_eq!(say(text), text);
        // … and the phonemizer reads them in German with the same voice.
        let runs = language_runs("Your Deutschprobeklausur in LK Mathematik is in Raum 204 near Musterstadt and Beispielheim");
        let german: Vec<&str> = runs.iter().filter(|(l, _)| *l == Lang::De).map(|(_, t)| t.as_str()).collect();
        let joined = german.join(" ");
        for word in ["Deutschprobeklausur", "Mathematik", "Raum", "Musterstadt", "Beispielheim"] {
            assert!(joined.contains(word), "{word} should be German: {runs:?}");
        }
        assert!(!joined.contains("Your"));
    }
}
