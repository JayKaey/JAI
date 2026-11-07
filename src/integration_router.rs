//! Deterministic routing of integration questions — "what classes do I have tomorrow", "any
//! new emails", "what's in my calendar on Friday", "add dentist to my calendar tomorrow at 3",
//! "how can I use you on my phone" — straight to the right tool, typos included ("clases",
//! "webunits"). The language model then only phrases a result that is really there: a small
//! model left to itself announces "Checking your WebUntis timetable…", writes the call as text
//! and invents the lessons.
//!
//! `Mode::Verbatim` answers are read out by Jarvis himself (the exact timetable block in the
//! chat, its spoken summary aloud); `Mode::Phrase` results go through the LLM; `Mode::Act` tools
//! run on the fast path with a canned confirmation.

use chrono::Local;
use serde_json::{json, Value};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Jarvis reads the tool's own text (display block + its "Spoken:" line).
    Verbatim,
    /// The LLM answers from the tool result.
    Phrase,
    /// An action with a canned confirmation.
    Act,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Route {
    pub tool: &'static str,
    pub args: Value,
    pub mode: Mode,
}

fn route(tool: &'static str, args: Value, mode: Mode) -> Option<Route> {
    Some(Route { tool, args, mode })
}

fn words(text: &str) -> Vec<String> {
    text.to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() || c == '.' || c == '-' || c == ':' { c } else { ' ' })
        .collect::<String>()
        .split_whitespace()
        .map(|w| w.trim_matches('.').to_string())
        .filter(|w| !w.is_empty())
        .collect()
}

/// A word that is (a misspelling of) one of `targets`. Jaro-Winkler alone is too generous with
/// a shared first letter ("cleaner" ≈ "calender" sent "make the interface cleaner" to the
/// calendar), so a fuzzy hit must also start like the target and differ by at most two edits.
fn fuzzy_has(ws: &[String], targets: &[&str]) -> bool {
    // Real words one or two letters from a keyword: "Terminal" is no "Termine" (2026-11-07 it
    // read his calendar), "then" no "when".
    const NEVER_FUZZY: &[&str] = &["terminal", "terminals", "terminate", "terminated", "termination", "determine", "then", "than", "thee"];
    ws.iter().any(|w| {
        targets.iter().any(|t| {
            let same_start = w.chars().take(2).eq(t.chars().take(2));
            if w != t && NEVER_FUZZY.contains(&w.as_str()) {
                return false;
            }
            w == t
                || (w.len() >= 5 && t.len() >= 5 && same_start && strsim::jaro_winkler(w, t) >= 0.9 && strsim::damerau_levenshtein(w, t) <= 2)
                || (w.len() >= 4 && t.len() == w.len() && strsim::damerau_levenshtein(w, t) <= 1)
        })
    })
}

fn has_phrase(lower: &str, phrases: &[&str]) -> bool {
    phrases.iter().any(|p| lower.contains(p))
}

/// The day or range a question is about ("tomorrow", "Friday", "next week", "24.10."), as the
/// schedule tools take it.
pub fn day_phrase(text: &str) -> Option<String> {
    let lower = format!(" {} ", text.to_lowercase().replace(['?', '!', ','], " "));
    for (needle, value) in [
        (" day after tomorrow", "day after tomorrow"),
        (" übermorgen", "day after tomorrow"),
        (" uebermorgen", "day after tomorrow"),
        (" next week", "next week"),
        (" nächste woche", "next week"),
        (" naechste woche", "next week"),
        (" this week", "this week"),
        (" diese woche", "this week"),
        (" the week", "this week"),
        (" weekend", "weekend"),
        (" wochenende", "weekend"),
        (" tomorrow", "tomorrow"),
        (" tmrw", "tomorrow"),
        (" tommorow", "tomorrow"),
        (" tomorow", "tomorrow"),
        (" today", "today"),
        (" heute", "today"),
        (" tonight", "today"),
    ] {
        if lower.contains(needle) {
            return Some(value.to_string());
        }
    }
    // "morgen" alone (not "heute morgen" = this morning, not "morgens").
    if lower.contains(" morgen ") && !lower.contains("heute morgen") {
        return Some("tomorrow".into());
    }
    let today = Local::now().date_naive();
    for w in words(text) {
        if crate::pim::parse_day(&w, today).is_some() && w.chars().any(|c| c.is_alphabetic() || c == '.' || c == '-') && w.len() >= 3 {
            // "next Friday": keep the "next".
            let before_next = lower.contains(&format!("next {w}")) || lower.contains(&format!("nächsten {w}"));
            return Some(if before_next { format!("next {w}") } else { w });
        }
    }
    None
}

/// Imperatives that are about apps and files, not questions ("open my school folder").
const ACTION_VERBS: &[&str] = &["open", "close", "start", "launch", "play", "move", "find", "search", "delete", "öffne", "oeffne", "starte", "schließe", "finde", "spiel", "sort", "clean", "organize", "sync"];
const QUESTION: &[&str] = &[
    "what", "which", "whats", "when", "do", "does", "have", "got", "any", "tell", "show", "list", "check", "read", "give", "was", "welche", "welcher",
    "wann", "hab", "habe", "zeig", "sag", "gibt", "how", "is", "are", "my", "meine", "mein",
];

/// A conservative safety boundary shared with the brain's contextual routing. Substrings are
/// intentional here: even "playlists" and "playing" must never reach a schedule fast path.
pub fn has_music_language(text: &str) -> bool {
    let lower = text.to_lowercase();
    ["playlist", "song", "music", "play", "sonora", "spotify", "track", "album", "loud", "quiet",
        "jaysrock", "jay's rock", "jays rock", "highway to hell", "musik", "lied"].iter().any(|word| lower.contains(word))
}

const CREATE_WORDS: &[&str] = &[
    "create", "creating", "add", "adding", "new", "make", "entry", "eintragen", "hinzufügen", "hinzufuegen", "erstelle", "erstellen", "neuen",
    "neuer", "neues", "trag", "trage", "füge", "fuege", "füg", "hinzu", "lege", "leg",
];
const CREATE_PHRASES: &[&str] = &["schedule a", "schedule an", "put ", "termin eintragen", "can you", "kannst du"];

/// Creation, modification and capability language: such a sentence never reads the calendar.
pub fn blocks_calendar_read(text: &str) -> bool {
    let lower = text.to_lowercase();
    words(text).iter().any(|w| CREATE_WORDS.contains(&w.as_str())) || CREATE_PHRASES.iter().any(|p| lower.contains(p))
}

/// "Can you create a new event in my calendar?": a request to add an event, details or not.
pub fn calendar_creation_intent(text: &str) -> bool {
    let ws = words(text);
    let lower = text.to_lowercase();
    let event = fuzzy_has(&ws, &["calendar", "kalender", "calender", "calandar"])
        || ws.iter().any(|w| matches!(w.as_str(), "event" | "events" | "appointment" | "appointments" | "termin" | "termine" | "entry" | "eintrag"));
    let create = ws.iter().any(|w| matches!(w.as_str(), "create" | "add" | "new" | "make" | "schedule" | "put" | "book" | "eintragen" | "hinzufügen" | "hinzufuegen" | "erstelle" | "erstellen" | "neuen" | "trag" | "trage" | "füge" | "fuege" | "füg" | "hinzu" | "lege" | "leg"))
        || has_phrase(&lower, &["termin eintragen", "schedule a"]);
    event && create && !has_music_language(text)
}

/// The title Sir named for a new event ("... called Tharnaz Tamme?").
pub fn calendar_title(text: &str) -> Option<String> {
    static TITLE: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r#"(?i)\b(?:called|named|titled|namens|mit dem titel)\s+["']?([^"'?.!]+)"#).expect("valid calendar title regex")
    });
    let fixed = crate::typos::normalize_user_input_typos(text);
    let title = TITLE.captures(&fixed)?.get(1)?.as_str().trim().to_string();
    // "called Test for 15h tomorrow": the date and time are no part of the title.
    let tokens: Vec<&str> = title.split_whitespace().collect();
    let mut kept: Vec<&str> = match when_span(&tokens, Local::now().naive_local()) {
        Some((a, b, _, _)) => tokens[..a].iter().chain(&tokens[b..]).copied().collect(),
        None => tokens,
    };
    while kept.last().is_some_and(|w| matches!(w.to_lowercase().as_str(), "for" | "on" | "at" | "am" | "um" | "to" | "from" | "in" | "von" | "für")) {
        kept.pop();
    }
    let title = kept.join(" ");
    (!title.is_empty() && title.split_whitespace().count() <= 8).then(|| crate::pim::capitalise(&title))
}

/// The details Sir gives after "Just tell me the title, date, and time": "Maths exam on Friday at
/// 9 AM", or only the time when the title is already known.
pub fn calendar_details(text: &str, title: Option<&str>) -> Option<Route> {
    let clean = text.trim().trim_end_matches(['.', '!', '?']).trim();
    let now = Local::now().naive_local();
    if let Some(title) = title.filter(|t| !t.trim().is_empty()) {
        let when = clean.trim_start_matches("on ").trim_start_matches("at ").trim_start_matches("am ").trim_start_matches("um ");
        if crate::pim::parse_datetime(when, now).is_some() {
            return route("create_calendar_event", json!({ "title": crate::pim::capitalise(title), "start_time": when }), Mode::Act);
        }
    }
    let body = format!("add {clean}");
    calendar_create(&body, true)
}

pub fn explicit_school_request(text: &str) -> bool {
    if has_music_language(text) { return false; }
    let ws = words(text);
    ws.iter().any(|w| matches!(w.as_str(), "timetable" | "webuntis" | "stundenplan" | "classes" | "ausfall"))
        || text.to_lowercase().contains("school schedule")
}

/// Capability questions describe the registry; they must not start music or reuse an old intent.
pub fn capability_topic(text: &str) -> Option<&'static str> {
    let lower = text.to_lowercase();
    let s = lower.trim().trim_end_matches(['?', '.', '!']);
    if ["what can you do", "list all your features", "list your features", "all your capabilities", "what are your features",
        "what are your capabilities", "was kannst du", "zeige alle funktionen"].iter().any(|p| s.contains(p)) {
        return Some("all");
    }
    if ["can you", "are you able", "do you support", "is it possible", "kannst du"].iter().any(|p| s.starts_with(p))
        && has_music_language(s)
        && ["specific playlist", "any playlist", "a playlist", "playlists", "specific song", "specific track", "a song", "music", "eine playlist"].iter().any(|p| s.contains(p)) {
        return Some("music");
    }
    None
}

/// The integration a sentence asks about, if any.
pub fn route_text(text: &str) -> Option<Route> {
    let lower = text.to_lowercase();
    let ws = words(text);
    if ws.is_empty() {
        return None;
    }
    // ---- one address that never changes: "what do I have to do to set it up so that I can
    // use you as a website that has a static domain" (a long sentence, hence before the length
    // rule), "can your link stay the same", "eine feste Domain für dich einrichten" ----
    let fixed_address = has_phrase(&lower, &[
        "static domain", "fixed domain", "permanent domain", "own domain", "my domain", "custom domain", "static address", "permanent address", "fixed address",
        "static url", "permanent url", "fixed url", "permanent link", "static link", "fixed link", "feste domain", "eigene domain", "eigenen domain", "feste adresse", "permanente adresse",
        "statische domain", "statische adresse", "fester link", "festen link",
    ]) || (has_phrase(&lower, &["never changes", "never change", "doesn't change", "doesnt change", "does not change", "not change", "always the same", "stay the same", "stays the same", "immer gleich", "nicht ändert", "nie ändert"])
        && fuzzy_has(&ws, &["domain", "address", "link", "url", "website", "adresse", "webseite"]));
    let about_him = fuzzy_has(&ws, &["you", "your", "jarvis", "website", "webseite", "page", "site", "phone", "satellite", "tunnel", "dich", "dir", "deine", "dein", "it"]);
    if fixed_address && about_him && ws.len() <= 40 && !lower.contains("whatsapp") {
        return route("show_mobile_satellite_instructions", json!({ "topic": "permanent_address" }), Mode::Act);
    }
    if ws.len() > 22 {
        return None;
    }
    let asks = fuzzy_has(&ws, QUESTION) || lower.trim_end().ends_with('?');
    let imperative = ws.first().is_some_and(|w| ACTION_VERBS.contains(&w.as_str())) || (ws.first().is_some_and(|w| w == "jarvis") && ws.get(1).is_some_and(|w| ACTION_VERBS.contains(&w.as_str())));

    // ---- the phone: "how can I use you on my phone", "connect my phone", "mobile link" ----
    let phone = fuzzy_has(&ws, &["phone", "handy", "iphone", "ipad", "smartphone", "mobile", "tablet", "telefon"]);
    // "… without being on the same Wi-Fi", "outside my Wi-Fi", "unterwegs", "over 4G": anywhere.
    let anywhere = has_phrase(&lower, &[
        "same wifi", "same wi-fi", "same wi fi", "same network", "outside my wifi", "outside my wi-fi", "outside of my wifi", "outside the wifi", "outside my network",
        "not at home", "away from home", "from anywhere", "anywhere in the world", "on the go", "mobile data", "außer haus", "ausser haus", "nicht zuhause",
        "nicht zu hause", "mobilen daten", "mobile daten", "anderen wlan", "ohne wlan", "gleichen wlan", "selben wlan",
    ]) || ws.iter().any(|w| matches!(w.as_str(), "4g" | "5g" | "lte" | "cellular" | "unterwegs"));
    let use_jarvis = has_phrase(&lower, &["use you", "use jarvis", "reach you", "talk to you", "access you", "dich nutzen", "dich benutzen", "dich erreichen", "jarvis nutzen", "jarvis benutzen"]);
    if ((phone
        && (has_phrase(&lower, &["use you", "connect", "link", "pair", "qr", "satellite", "set up", "setup", "verbind", "benutz", "nutzen", "talk to you", "access you", "on my phone", "from my phone", "auf meinem handy", "mit meinem handy", "auf dem handy", "am handy"])
            || lower.contains("mobile link")
            || anywhere))
        || (anywhere && use_jarvis))
        && !lower.contains("whatsapp") {
            return route("show_mobile_satellite_instructions", json!({}), Mode::Act);
        }

    // ---- diagnostics ----
    if has_phrase(&lower, &["diagnose", "test all connections", "test my connections", "check my connections", "check all connections", "test my integrations", "check my integrations", "integration status", "connection status", "teste alle verbindungen"]) {
        return route("diagnose_integrations", json!({}), Mode::Phrase);
    }

    // ---- calendar: adding an event ----
    let calendar_word = fuzzy_has(&ws, &["calendar", "kalender", "calender", "calandar"]);
    if let Some(r) = calendar_create(text, calendar_word) {
        return Some(r);
    }

    // ---- school: exams / class tests ----
    // "schule" only exactly ("schedule" is close enough to fool a fuzzy match).
    let school = explicit_school_request(text);
    let exams = has_phrase(&lower, &["class test", "class tests", "exam", "klausur", "klassenarbeit", "test coming", "tests coming", "arbeit schreiben", "upcoming test"]);
    if exams && school && !lower.contains("stress test") {
        return route("check_school_schedule", json!({ "target_date": "next 14 days", "include_exams": true, "only_exams": true }), Mode::Phrase);
    }

    // ---- school: the timetable ----
    let calendar_only = calendar_word && !fuzzy_has(&ws, &["class", "classes", "lesson", "lessons", "timetable", "webuntis", "school", "stundenplan"]);
    if school && !calendar_only && !imperative && (asks || ws.len() <= 4) && !has_phrase(&lower, &["leave for school", "to school", "zur schule"]) {
        let day = day_phrase(text).unwrap_or_else(|| "today".into());
        let only = has_phrase(&lower, &["cancel", "entfall", "ausfall", "free period", "freistunde", "substitut", "vertretung", "room change", "fällt", "faellt"]);
        // "Do I have physics tomorrow?", "when does school start": the LLM answers from the data.
        let specific = has_phrase(&lower, &["when ", "wann ", "what time", "how many", "first", "last", "room", "raum", "erste", "letzte", "start", "end"])
            || ws.iter().any(|w| crate::pim::english_subject(w).is_some() && w.len() > 3);
        let mut args = json!({ "target_date": day });
        if only {
            args["only_cancellations"] = json!(true);
        }
        return route("check_school_schedule", args, if specific { Mode::Phrase } else { Mode::Verbatim });
    }

    // ---- "my schedule" / "my day": the timetable and the calendar together ----
    if school && !calendar_word && !imperative && asks && lower.contains("school schedule") && !lower.contains("leave") {
        let day = day_phrase(text).unwrap_or_else(|| "today".into());
        return route("check_school_schedule", json!({ "target_date": day, "include_calendar": true }), Mode::Verbatim);
    }

    // ---- calendar: reading ----
    // Only explicit calendar words read the calendar: "what's on my screen" is a look at the
    // monitor, never the agenda.
    let explicit_calendar = calendar_word || fuzzy_has(&ws, &["appointments", "appointment", "termine", "termin", "events", "agenda"]);
    let agenda = explicit_calendar || (has_phrase(&lower, &["my plans", "am i free", "busy on"]) && day_phrase(text).is_some());
    if agenda && asks && !imperative && !has_music_language(text) && !blocks_calendar_read(text) && !crate::intents::is_screen_question(text) {
        let day = day_phrase(text).unwrap_or_else(|| "today".into());
        let specific = has_phrase(&lower, &["when ", "wann ", "what time", "am i free", "busy", "conflict"]);
        return route("get_calendar_schedule", json!({ "date_or_range": day }), if specific { Mode::Phrase } else { Mode::Verbatim });
    }

    // ---- messages ----
    let whatsapp = lower.contains("whatsapp") || lower.contains("whats app");
    let sending = fuzzy_has(&ws, &["send", "schick", "schicke", "reply", "antworte", "write", "schreib", "forward", "text"]);
    if whatsapp && !sending && (asks || fuzzy_has(&ws, &["messages", "message", "nachrichten", "nachricht", "unread", "new"])) {
        return route("check_whatsapp_messages", json!({ "unread_only": !lower.contains("recent") && !lower.contains("last") }), Mode::Phrase);
    }
    let mail = fuzzy_has(&ws, &["email", "emails", "e-mail", "e-mails", "mail", "mails", "inbox", "posteingang", "gmail"]);
    if mail && !sending && (asks || fuzzy_has(&ws, &["new", "unread", "neue", "ungelesene"])) && !lower.contains("summar") {
        return route("check_unread_emails", json!({ "limit": 5 }), Mode::Phrase);
    }
    if has_phrase(&lower, &["all my messages", "any messages", "any new messages", "check my messages", "neue nachrichten", "all communications", "everything new"]) && !sending {
        return route("check_all_communications", json!({}), Mode::Phrase);
    }

    // ---- itslearning ----
    let lms = has_phrase(&lower, &["itslearning", "its learning", "it's learning"]) || fuzzy_has(&ws, &["homework", "hausaufgaben", "assignments", "worksheets", "arbeitsblätter"]);
    if lms && (asks || ws.len() <= 4) && !has_phrase(&lower, &["summar", "zusammenfass"]) {
        return route("check_course_updates", json!({}), Mode::Phrase);
    }
    None
}

/// "Add dentist to my calendar tomorrow at 3 pm", "put football practice in my calendar on
/// Friday at 17:00", "schedule a meeting with Max next Monday at 10", "trag Zahnarzt morgen um
/// 15 Uhr in meinen Kalender ein".
fn calendar_create(text: &str, calendar_word: bool) -> Option<Route> {
    calendar_create_sentence(text, calendar_word).or_else(|| {
        let sentences: Vec<&str> = text.split_inclusive(['.', '?', '!']).map(str::trim).filter(|s| !s.is_empty()).collect();
        if sentences.len() < 2 {
            return None;
        }
        sentences.iter().find_map(|s| calendar_create_sentence(s, calendar_word || fuzzy_has(&words(s), &["calendar", "kalender", "calender", "calandar"])))
    })
}

/// A word that can be part of a date or time: digits ("3", "16:00", "25th", "22.10."), day and
/// time words in English and German, weekday and month names, and the little words that join
/// them ("on", "at", "the", "um", "am").
fn when_word(word: &str) -> bool {
    let w = word.trim_matches(|c: char| !c.is_alphanumeric() && c != ':' && c != '.' && c != '\'').trim_end_matches('.').to_lowercase();
    if w.is_empty() {
        return false;
    }
    if w.starts_with(|c: char| c.is_ascii_digit()) {
        return w.len() <= 10;
    }
    const WORDS: &[&str] = &[
        "today", "tomorrow", "tonight", "heute", "morgen", "übermorgen", "uebermorgen", "next", "this", "coming", "nächsten", "nächste", "naechsten",
        "naechste", "kommenden", "kommende", "diesen", "on", "at", "the", "of", "am", "um", "pm", "a.m", "p.m", "uhr", "o'clock", "oclock", "noon",
        "midnight", "morning", "afternoon", "evening", "mittag", "mitternacht", "früh", "frueh", "vormittag", "nachmittag", "abend", "abends", "week",
        "woche", "day", "after", "den", "dem", "from", "von", "to", "until", "till", "bis", "in", "hours", "hour", "minutes", "stunden", "stunde", "minuten",
    ];
    WORDS.contains(&w.as_str())
        || (crate::pim::weekday_named(&w).is_some() && w.len() >= 6)
        || (crate::pim::month_named(&w).is_some() && w.len() >= 4)
        || w == "mai"
}

/// The longest run of date/time words in `tokens` that really reads as a point in time, as
/// (first, end) token indices with the parsed start (and whether it had a time of day).
fn when_span(tokens: &[&str], now: chrono::NaiveDateTime) -> Option<(usize, usize, chrono::NaiveDateTime, bool)> {
    let mut best: Option<(usize, usize, chrono::NaiveDateTime, bool)> = None;
    let mut i = 0;
    while i < tokens.len() {
        if !when_word(tokens[i]) {
            i += 1;
            continue;
        }
        let mut j = i;
        while j < tokens.len() && when_word(tokens[j]) {
            j += 1;
        }
        // Inside the run, the longest sub-span that parses ("Room 7 on Friday at 9": the run
        // "7 on Friday at 9" does not, "on Friday at 9" does).
        'run: for len in (1..=j - i).rev() {
            for a in i..=j - len {
                let b = a + len;
                let phrase: Vec<String> = tokens[a..b].iter().map(|t| t.trim_matches(|c: char| matches!(c, ',' | ';' | '!' | '?' | '"')).to_string()).collect();
                let phrase = phrase.join(" ");
                let words_only = phrase.split_whitespace().all(|w| matches!(w.to_lowercase().as_str(), "on" | "at" | "the" | "of" | "am" | "um" | "den" | "dem" | "in" | "to" | "from" | "von" | "bis" | "until" | "till"));
                if words_only {
                    continue;
                }
                if let Some((start, timed)) = crate::pim::parse_datetime(&phrase, now) {
                    if best.is_none_or(|(x, y, _, _)| len > y - x) {
                        best = Some((a, b, start, timed));
                    }
                    break 'run;
                }
            }
        }
        i = j;
    }
    best
}

fn calendar_create_sentence(text: &str, calendar_word: bool) -> Option<Route> {
    static CREATE: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"(?i)^(?:(?:hey|ok|okay)\s+)?(?:jarvis[,\s]+)?(?:please\s+|bitte\s+|can you\s+|could you\s+|would you\s+|kannst du\s+|könntest du\s+)?(add|put|schedule|create|enter|make|book|set up|plan|note|trag|trage|erstelle|erstell|leg|lege|füge|fuege|setz|setze|notier|notiere|mach|mache)\s+(.+)$")
            .expect("valid calendar create regex")
    });
    // "add a new in my calender called test for 15h tommorw": the words spelt right first, so
    // "tomorrow" is a date and never part of the title.
    let fixed = crate::typos::normalize_user_input_typos(text);
    let clean = fixed.trim().trim_end_matches(['.', '!', '?']).trim();
    let c = CREATE.captures(clean)?;
    let lower = clean.to_lowercase();
    let calendar_word = calendar_word || fuzzy_has(&words(clean), &["calendar", "kalender", "calender", "calandar"]);
    let says_event = calendar_word || has_phrase(&lower, &["appointment", "event", "termin", "meeting", "eintrag"]);
    // "Schedule physics study tomorrow at 4 pm": scheduling something at a time is an event, too
    // (reminders, timers and alarms have their own tools).
    let scheduling = matches!(c[1].to_lowercase().as_str(), "schedule" | "plan" | "book")
        && !has_phrase(&lower, &["remind", "reminder", "timer", "alarm", "wecker", "erinner"]);
    if !says_event && !scheduling {
        return None;
    }
    let mut body = c[2].to_string();
    // "to my (Apple) calendar", "in meinen Kalender ein", "zu meinem Kalender hinzu", wherever.
    static CAL_PART: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"(?i)\s*(?:to|in|into|on|onto|zu|in den|in meinen|in meinem|im|zum)\s+(?:my\s+|the\s+|meinen\s+|meinem\s+|den\s+|dem\s+)?(?:apple\s+|icloud\s+|google\s+)?(?:calendar|kalender|calender)(?:\s+(?:ein|hinzu|an))?\b").expect("valid calendar part regex")
    });
    body = CAL_PART.replace_all(&body, " ").to_string();
    // "a new calendar event", "einen neuen Termin": the kind of entry, not its title.
    static KIND: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"(?i)^(?:an?\s+|ein(?:en|e)?\s+)?(?:new\s+|neue[nrs]?\s+)?(?:calendar\s+|kalender)?(?:event|entry|appointment|termin|eintrag)\s*(?:for\s+|für\s+|fuer\s+)?").expect("valid calendar kind regex")
    });
    let before_kind = body.trim().to_string();
    // The kind word itself names an event Sir gives no title ("füge einen Termin um 15 Uhr
    // hinzu" is a "Termin" at 15:00).
    static KIND_WORD: std::sync::LazyLock<regex::Regex> =
        std::sync::LazyLock::new(|| regex::Regex::new(r"(?i)\b(event|entry|appointment|termin|eintrag)\b").expect("valid calendar kind word regex"));
    let kind_word = KIND.find(&before_kind).filter(|m| !m.as_str().trim().is_empty()).and_then(|m| KIND_WORD.find(m.as_str())).map(|w| crate::pim::capitalise(w.as_str()));
    body = KIND.replace(&before_kind, "").to_string();
    // "Add a new in my calendar called Test": the kind word left out ("a new", "einen neuen").
    if body == before_kind {
        // Only when the title or the time follows at once ("a new called Test", "a new for
        // tomorrow"): "add new year party" keeps its "new".
        static KIND_BARE: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
            regex::Regex::new(r"(?i)^(?:an?\s+|ein(?:en|e)?\s+)?(?:new|neue[nrs]?)\s+(?:one\s+)?((?:called|named|titled|namens|mit dem titel|titel|for|für|fuer|on|at|am|um|tomorrow|today|morgen|heute)\b.*)$")
                .expect("valid bare calendar kind regex")
        });
        body = KIND_BARE.replace(&before_kind, "$1").to_string();
    }
    // "all day" / "ganztägig".
    static ALL_DAY: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"(?i)\b(?:as an all[- ]day event|all[- ]day|the whole day|whole day|ganztägig|ganztaegig|ganztags|den ganzen tag)\b").expect("valid all-day regex")
    });
    let all_day_said = ALL_DAY.is_match(&body);
    body = ALL_DAY.replace_all(&body, " ").to_string();
    // "… called Gym" / "namens Zahnarzt": the title Sir names explicitly (a time may follow it).
    static NAMED: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"(?i)\b(?:called|named|titled|with the title|namens|mit dem titel|titel)\s+(.+)$").expect("valid named-title regex")
    });
    let (rest, named) = match NAMED.captures(&body) {
        Some(c) => (body[..c.get(0).map_or(0, |m| m.start())].to_string(), Some(c[1].trim_matches(|ch: char| ch == '"' || ch == '\'' || ch == ':').to_string())),
        None => (body.clone(), None),
    };
    let now = Local::now().naive_local();
    // The date and time: in the rest, else in the tail of the named title ("called Gym at 5").
    let rest_tokens: Vec<&str> = rest.split_whitespace().collect();
    let named_tokens: Vec<&str> = named.as_deref().map(|n| n.split_whitespace().collect()).unwrap_or_default();
    let (title_tokens, start, timed): (Vec<&str>, chrono::NaiveDateTime, bool) = if let Some((a, b, start, timed)) = when_span(&rest_tokens, now) {
        let title = if named.is_some() { named_tokens.clone() } else { rest_tokens[..a].iter().chain(&rest_tokens[b..]).copied().collect() };
        (title, start, timed)
    } else if let Some((a, b, start, timed)) = when_span(&named_tokens, now) {
        (named_tokens[..a].iter().chain(&named_tokens[b..]).copied().collect(), start, timed)
    } else {
        return None;
    };
    // An end time: "from 3 to 5 pm", "von 15 bis 17 Uhr" (inside the parsed span or after it).
    let end = {
        static UNTIL: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
            regex::Regex::new(r"(?i)\b(?:to|until|till|bis|-)\s+(\d{1,2}(?::\d{2})?\s*(?:am|pm|uhr)?)").expect("valid end-time regex")
        });
        UNTIL.captures(&body).and_then(|c| crate::pim::parse_datetime(&format!("at {}", &c[1]), start)).map(|(t, _)| start.date().and_time(t.time())).filter(|e| *e > start && timed)
    };
    const LEAD: &[&str] = &["a", "an", "the", "my", "ein", "eine", "einen", "der", "die", "das", "den", "mein", "meinen", "meine", "for", "für", "fuer", "to", "zu", "on", "at", "am", "um", "and", "und", ":", "-"];
    const TRAIL: &[&str] = &["on", "at", "for", "to", "am", "um", "zum", "zur", "for", "from", "von", "in", "and", "und", "with", ":", "-", "ein", "hinzu", "an"];
    let mut title: Vec<String> = title_tokens.iter().map(|t| t.trim_matches(|c: char| matches!(c, ',' | ';' | ':' | '"' | '\'')).to_string()).filter(|t| !t.is_empty()).collect();
    while title.first().is_some_and(|w| LEAD.contains(&w.to_lowercase().as_str())) {
        title.remove(0);
    }
    while title.last().is_some_and(|w| TRAIL.contains(&w.to_lowercase().as_str())) {
        title.pop();
    }
    let title = title.join(" ");
    let title = match kind_word {
        Some(kind) if title.is_empty() && timed => kind,
        _ => title,
    };
    if title.is_empty() {
        return None;
    }
    let all_day = all_day_said || !timed;
    let mut args = json!({
        "title": crate::pim::capitalise(&title),
        "start_time": start.format("%Y-%m-%d %H:%M").to_string(),
        "all_day": all_day,
    });
    if let Some(end) = end.filter(|_| !all_day) {
        args["end_time"] = json!(end.format("%Y-%m-%d %H:%M").to_string());
    }
    route("create_calendar_event", args, Mode::Act)
}

/// An offer in Jarvis' last reply ("Shall I check your WebUntis timetable?") that Sir's "yes"
/// accepts.
pub fn route_offer(reply: &str) -> Option<Route> {
    let lower = reply.to_lowercase();
    for sentence in lower.split(['.', '!', '?', '\n']) {
        let s = sentence.trim();
        for opener in ["shall i ", "should i ", "would you like me to ", "do you want me to ", "want me to ", "may i ", "soll ich "] {
            if let Some(i) = s.find(opener) {
                let rest = &s[i + opener.len()..];
                if let Some(r) = route_text(&format!("check {rest}?")) {
                    return Some(r);
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool(text: &str) -> Option<&'static str> {
        route_text(text).map(|r| r.tool)
    }

    #[test]
    fn explicit_school_questions_reach_webuntis() {
        for t in ["tell me what classes i have tomorrow", "tell me everything about my webuntis", "Was ist mein Stundenplan heute", "what's on my timetable on Friday?", "school schedule tomorrow"] {
            assert_eq!(tool(t), Some("check_school_schedule"), "{t}");
        }
        assert_eq!(route_text("what classes do i have tomorrow").unwrap().args["target_date"], "tomorrow");
        assert_eq!(route_text("what classes do i have tomorrow").unwrap().mode, Mode::Verbatim);
        assert_eq!(route_text("when do classes start tomorrow?").unwrap().mode, Mode::Phrase);
        assert_eq!(route_text("are any classes cancelled tomorrow").unwrap().args["only_cancellations"], true);
        assert_eq!(route_text("do i have any exams in my classes coming up?").unwrap().args["only_exams"], true);
        assert_eq!(tool("When do I need to leave for school tomorrow?"), None, "commute, not the timetable");
    }

    #[test]
    fn calendar_mail_messages_and_phone() {
        assert_eq!(tool("what's in my calendar on Friday?"), Some("get_calendar_schedule"));
        let tomorrow = (Local::now().date_naive() + chrono::Duration::days(1)).format("%Y-%m-%d").to_string();
        let add = route_text("add dentist to my calendar tomorrow at 3 pm").unwrap();
        assert_eq!((add.tool, add.args["title"].as_str(), add.args["start_time"].as_str()), ("create_calendar_event", Some("Dentist"), Some(format!("{tomorrow} 15:00").as_str())));
        let add = route_text("Schedule a meeting with Max next Friday at 10 AM").unwrap();
        assert_eq!(add.args["title"], "Meeting with Max");
        assert_eq!(tool("do i have any new emails?"), Some("check_unread_emails"));
        assert_eq!(tool("any new whatsapp messages?"), Some("check_whatsapp_messages"));
        assert_eq!(tool("send that to Max on WhatsApp"), None);
        assert_eq!(tool("how can i use you from my phone"), Some("show_mobile_satellite_instructions"));
        assert_eq!(tool("connect my phone"), Some("show_mobile_satellite_instructions"));
        assert_eq!(route_text("how can i use you from my phone").unwrap().args.get("topic"), None);
        // One address that never changes: the setup guide (Sir's own long sentence among them).
        for text in [
            "What do I have to do to set it up that I can use you as a website that has a static domain?",
            "How do I give your website a permanent address?",
            "Can you get a link that never changes?",
            "how can i use you with my own domain",
            "Jarvis, set up a static domain for your website",
            "Wie richte ich eine feste Domain für dich ein?",
        ] {
            let r = route_text(text).unwrap_or_else(|| panic!("{text}: not routed"));
            assert_eq!((r.tool, r.args["topic"].as_str()), ("show_mobile_satellite_instructions", Some("permanent_address")), "{text}");
        }
        assert_eq!(tool("what is a static domain"), None, "a question about the term, not about him");
        for text in [
            "How can I use you on my phone without being on the same Wi-Fi?",
            "Use Jarvis outside my Wi-Fi",
            "Wie kann ich dich unterwegs auf dem Handy nutzen?",
            "Can I reach you from anywhere over 4G?",
        ] {
            assert_eq!(tool(text), Some("show_mobile_satellite_instructions"), "{text}");
        }
        assert_eq!(tool("open spotify"), None);
        assert_eq!(tool("open my calendar"), None);
        assert_eq!(tool("open my school folder"), None);
        assert_eq!(route_text("what's my schedule tomorrow?"), None);
        assert_eq!(route_offer("Shall I check your WebUntis timetable for tomorrow?").map(|r| r.tool), Some("check_school_schedule"));
    }

    #[test]
    fn music_never_uses_a_schedule_fast_path() {
        for text in ["Can you play a specific playlist?", "list my playlists", "what song is playing", "play my school schedule playlist",
            "Spotify calendar", "what is my timetable while music plays", "Can you play Highway to Hell?", "JaysRock", "keep my classes quiet", "what can you do?"] {
            assert!(!route_text(text).is_some_and(|route| matches!(route.tool, "check_school_schedule" | "get_calendar_schedule")), "{text}");
        }
        assert_eq!(capability_topic("Can you play a specific playlist?"), Some("music"));
        assert_eq!(capability_topic("What can you do?"), Some("all"));
        assert_eq!(capability_topic("Can you play my playlist JaysRock?"), None);
    }

    #[test]
    fn creation_and_interface_requests_never_read_the_calendar() {
        for text in ["Can you create a new event in my calendar?", "Can you create a new event?", "can you make the interface cleaner?",
            "Kannst du einen Termin eintragen?", "add a new entry to my calendar", "Make the interface less full"] {
            assert!(!route_text(text).is_some_and(|r| r.tool == "get_calendar_schedule"), "{text}");
        }
        assert!(calendar_creation_intent("Can you create a new event in my calendar?"));
        assert!(!calendar_creation_intent("What's in my calendar tomorrow?"));
        assert_eq!(calendar_title("Can you create a new event called Maths exam?").as_deref(), Some("Maths exam"));
        let add = route_text("Add Maths exam on Friday at 9 AM to my calendar").unwrap();
        assert_eq!((add.tool, add.args["title"].as_str()), ("create_calendar_event", Some("Maths exam")));
        let details = calendar_details("Maths exam on Friday at 9 AM", None).unwrap();
        assert_eq!(details.tool, "create_calendar_event");
        assert_eq!(calendar_details("Friday at 9 AM", Some("maths exam")).unwrap().args["title"], "Maths exam");
        assert_eq!(tool("what's in my calendar on Friday?"), Some("get_calendar_schedule"), "reading still works");
        assert_eq!(tool("what's in my calender tomorrow"), Some("get_calendar_schedule"), "a misspelling still works");
    }

    #[test]
    fn screen_questions_never_read_the_calendar() {
        for text in ["What's on my screen right now?", "what is on my monitor", "Was ist auf meinem Bildschirm?", "what's on today", "what do you see on my display?"] {
            assert!(!route_text(text).is_some_and(|r| r.tool == "get_calendar_schedule"), "{text}");
        }
        assert_eq!(tool("what events do I have today?"), Some("get_calendar_schedule"));
        assert_eq!(tool("am I free on Friday?"), Some("get_calendar_schedule"));
    }

    #[test]
    fn calendar_creation_reads_every_phrasing() {
        let today = Local::now().date_naive();
        let day = |d: i64| (today + chrono::Duration::days(d)).format("%Y-%m-%d").to_string();
        let add = |text: &str| route_text(text).unwrap_or_else(|| panic!("no route for '{text}'")).args;
        let a = add("Add Dentist to my calendar on Friday at 4 PM");
        assert_eq!(a["title"], "Dentist");
        assert!(a["start_time"].as_str().unwrap().ends_with("16:00") && a["all_day"] == false, "{a}");
        let a = add("Create a calendar event for tomorrow at 5 pm called Gym");
        assert_eq!((a["title"].as_str(), a["start_time"].as_str()), (Some("Gym"), Some(format!("{} 17:00", day(1)).as_str())));
        let a = add("Trag Zahnarzt morgen um 16 Uhr in meinen Kalender ein");
        assert_eq!((a["title"].as_str(), a["start_time"].as_str()), (Some("Zahnarzt"), Some(format!("{} 16:00", day(1)).as_str())));
        let a = add("Trag Mathe Klausur übermorgen in meinen Kalender ein");
        assert_eq!((a["title"].as_str(), a["all_day"].as_bool()), (Some("Mathe Klausur"), Some(true)));
        assert!(a["start_time"].as_str().unwrap().starts_with(&day(2)));
        let a = add("Füge Elternabend am Freitag um 19 Uhr zu meinem Kalender hinzu");
        assert_eq!(a["title"], "Elternabend");
        assert!(a["start_time"].as_str().unwrap().ends_with("19:00"));
        let a = add("Add Gym to my calendar all day on Saturday");
        assert_eq!((a["title"].as_str(), a["all_day"].as_bool()), (Some("Gym"), Some(true)));
        let a = add("Put football practice in my calendar next week");
        assert_eq!(a["title"], "Football practice");
        assert!(a["start_time"].as_str().unwrap().starts_with(&day(7)));
        let a = add("Add a new event: Team call from 3 pm to 5 pm tomorrow");
        assert_eq!(a["title"], "Team call");
        assert!(a["start_time"].as_str().unwrap().ends_with("15:00") && a["end_time"].as_str().unwrap().ends_with("17:00"), "{a}");
        let a = add("Schedule Room 101 review in my calendar on Monday at 9");
        assert_eq!(a["title"], "Room 101 review");
        assert!(a["start_time"].as_str().unwrap().ends_with("09:00"));
        let a = add("Erstelle einen Termin ganztägig am Sonntag namens Familienfest");
        assert_eq!((a["title"].as_str(), a["all_day"].as_bool()), (Some("Familienfest"), Some(true)));
        // Without a date there is nothing to add: Jarvis asks instead.
        assert!(route_text("Add a new event to my calendar").is_none_or(|r| r.tool != "create_calendar_event"));
    }

    #[test]
    fn typos_missing_noun_and_european_time() {
        let tomorrow = (Local::now().date_naive() + chrono::Duration::days(1)).format("%Y-%m-%d").to_string();
        // The screenshot: no "event", "calender", "tommorw" and "15h".
        let a = route_text("add a new in my calender called test for 15h tommorw").unwrap();
        assert_eq!(a.tool, "create_calendar_event");
        assert_eq!((a.args["title"].as_str(), a.args["start_time"].as_str(), a.args["all_day"].as_bool()), (Some("Test"), Some(format!("{tomorrow} 15:00").as_str()), Some(false)));
        let a = route_text("schedule physics study tomorrow at 4pm").unwrap().args;
        assert_eq!((a["title"].as_str(), a["start_time"].as_str()), (Some("Physics study"), Some(format!("{tomorrow} 16:00").as_str())));
        let a = route_text("trag morgen um 15 uhr test in meinen kalender ein").unwrap().args;
        assert_eq!((a["title"].as_str(), a["start_time"].as_str()), (Some("Test"), Some(format!("{tomorrow} 15:00").as_str())));
        let a = route_text("put dentist in my calendar for 15h30 tomorrow").unwrap().args;
        assert_eq!((a["title"].as_str(), a["start_time"].as_str()), (Some("Dentist"), Some(format!("{tomorrow} 15:30").as_str())));
        // "new" stays when it is part of the title.
        let a = route_text("add new year party to my calendar tomorrow at 20:00").unwrap().args;
        assert_eq!(a["title"], "New year party");
        // A reminder is no calendar event.
        assert!(route_text("schedule a reminder tomorrow at 4pm").is_none_or(|r| r.tool != "create_calendar_event"));
        assert_eq!(calendar_title("add a new event called test for 15h tommorw").as_deref(), Some("Test"));
    }

    #[test]
    fn german_date_before_calendar_title() {
        let route = route_text("Trag morgen um 16 Uhr Mathe lernen in meinen Kalender ein").unwrap();
        assert_eq!(route.tool, "create_calendar_event");
        assert_eq!(route.args["title"], "Mathe lernen");
        assert!(route.args["start_time"].as_str().unwrap().ends_with("16:00"));
    }
}
