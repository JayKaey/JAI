//! Three small things Sir keeps with J.A.I. (2026-11-13): a notebook ("note: …", "what did I
//! note about …"), a sleep timer ("stop the music in 30 minutes") and his own rules ("whenever
//! Minecraft starts, set the music to 20 percent").
//!
//! Notes and rules live in the kit's store. A rule's order is carried out as if Sir had typed
//! it (`StarkEvent::RunMacro`), so a rule can do whatever an order of his can; it is only
//! kept when its order is one J.A.I. knows. The sleep timer lets the master volume sink for a
//! few seconds, pauses every player and puts the volume back where it stood.

use crate::fleet::PcEvent;
use crate::kit::say;
use chrono::Timelike;
use regex::Regex;
use serde_json::{json, Value};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn re(pattern: &str) -> Regex {
    Regex::new(pattern).expect("a valid phrase")
}

// ---------------------------------------------------------------------------- notebook ----

const NOTE: &str = "note";
pub const NOTEBOOK: &str = "kit_notebook";

static NOTE_ADD: LazyLock<Regex> = LazyLock::new(|| {
    re(r"(?i)^\s*(?:(?:jai|jay)[,.]?\s+)?(?:please\s+)?(?:note(?:\s+down)?(?:\s+that)?|take\s+a\s+note(?:\s+that)?|make\s+a\s+note(?:\s+of|\s+that)?|write\s+down(?:\s+that)?|jot\s+down|new\s+note|notiere?(?:\s+dir)?(?:,?\s+dass)?|notiz|neue\s+notiz|schreib\s+(?:dir\s+)?auf(?:,?\s+dass)?)\s*[:,]?\s+(?P<text>.+?)\s*$")
});
static NOTE_SHOW: LazyLock<Regex> = LazyLock::new(|| re(r"^(?:(?:show|open|read)\s+(?:me\s+)?(?:my\s+|the\s+)?(?:notes|notebook)|(?:my\s+)?notebook|what\s+(?:are|is\s+in)\s+my\s+notes|(?:zeig\s+(?:mir\s+)?)?(?:meine\s+notizen|mein\s+notizbuch))$"));
static NOTE_SEARCH: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^what\s+did\s+i\s+(?:note|write\s+down|jot\s+down)\s+(?:about|on|for)\s+(.+)$|^(?:search|look\s+in|find\s+in)\s+(?:my\s+)?notes\s+(?:for\s+)?(.+)$|^was\s+habe\s+ich\s+(?:zu|über)\s+(.+?)\s+notiert$")
});
static NOTE_DELETE_LAST: LazyLock<Regex> = LazyLock::new(|| re(r"^(?:delete|remove|scratch)\s+(?:the\s+|my\s+)?last\s+note$|^lösche?\s+die\s+letzte\s+notiz$"));
static NOTE_CLEAR: LazyLock<Regex> = LazyLock::new(|| re(r"^(?:delete|clear|remove)\s+all\s+(?:of\s+)?(?:my\s+)?notes$|^lösche?\s+alle\s+(?:meine\s+)?notizen$"));

/// "Note: buy milk", "show my notes", "what did I note about the exam", "delete the last note".
pub fn notebook_command(s: &str, original: &str) -> Option<Value> {
    if NOTE_SHOW.is_match(s) {
        return Some(json!({"action": "show"}));
    }
    if let Some(c) = NOTE_SEARCH.captures(s) {
        return Some(json!({"action": "show", "query": c.get(1).or(c.get(2)).or(c.get(3))?.as_str().trim()}));
    }
    if NOTE_DELETE_LAST.is_match(s) {
        return Some(json!({"action": "delete_last"}));
    }
    if NOTE_CLEAR.is_match(s) {
        return Some(json!({"action": "clear"}));
    }
    // ("Note that my locker code is 4512", "make a note that …": what he is to remember for
    // good is the older order, his long-term memory.)
    if crate::intents::is_memory_command(original) || crate::intents::is_memory_command(s) {
        return None;
    }
    let text = NOTE_ADD.captures(original)?.name("text")?.as_str().trim();
    // ("Note that down" is about something said before, not a note of three words.)
    (text.chars().count() >= 3 && !matches!(crate::ear::words(text).join(" ").as_str(), "that" | "this" | "it" | "down" | "that down" | "this down" | "it down" | "das" | "das auf" | "auf")).then(|| json!({"action": "add", "text": text}))
}

/// The notes as they stand in the block: the newest first, each with its day and time. With
/// `query`, only those that hold all its words.
pub fn notes_text(query: Option<&str>) -> (String, usize) {
    let wanted: Vec<String> = query.map(crate::ear::words).unwrap_or_default();
    let notes: Vec<String> = crate::kit::list(NOTE)
        .into_iter()
        .rev()
        .filter(|(_, v, _)| {
            let text = v["text"].as_str().unwrap_or("").to_lowercase();
            wanted.iter().all(|w| text.contains(w.as_str()))
        })
        .map(|(_, v, at)| format!("{}\n    {}", at.get(..16).unwrap_or(&at), v["text"].as_str().unwrap_or("")))
        .collect();
    let n = notes.len();
    (notes.join("\n\n"), n)
}

pub fn notebook(args: &Value) -> Result<String, String> {
    match args["action"].as_str().unwrap_or("show") {
        "add" => {
            let text = args["text"].as_str().unwrap_or("").trim();
            if text.is_empty() {
                return Err("What shall I note, Sir?".to_string());
            }
            crate::kit::put(NOTE, &crate::kit::stamp(), &json!({"text": text}))?;
            if crate::kit::text_is_up(NOTEBOOK) {
                crate::kit::show_text(NOTEBOOK, "Notebook // your notes", &notes_text(None).0, [460.0, 440.0]);
            }
            Ok(say(format!("Noted: {text}"), "Noted, Sir."))
        }
        "delete_last" => match crate::kit::list(NOTE).pop() {
            Some((key, v, _)) => {
                crate::kit::remove(NOTE, &key);
                if crate::kit::text_is_up(NOTEBOOK) {
                    crate::kit::show_text(NOTEBOOK, "Notebook // your notes", &notes_text(None).0, [460.0, 440.0]);
                }
                Ok(say(format!("Deleted the note: {}", v["text"].as_str().unwrap_or("")), "The last note is gone, Sir."))
            }
            None => Err("There is no note to delete, Sir.".to_string()),
        },
        "clear" => {
            let n = crate::kit::clear(NOTE);
            crate::kit::close_text(NOTEBOOK);
            Ok(say(format!("{n} notes deleted."), if n == 0 { "The notebook was empty, Sir.".to_string() } else { format!("All {n} notes are gone, Sir.") }))
        }
        _ => {
            let query = args["query"].as_str().filter(|q| !q.trim().is_empty());
            let (text, n) = notes_text(query);
            if n == 0 {
                return Ok(match query {
                    Some(q) => say(format!("No note about \"{q}\"."), format!("I find no note about {q}, Sir.")),
                    None => say("The notebook is empty. Say \"note: …\" to keep something.", "Your notebook is empty, Sir."),
                });
            }
            let title = match query {
                Some(q) => format!("Notebook // about {q}"),
                None => "Notebook // your notes".to_string(),
            };
            crate::kit::show_text(NOTEBOOK, &title, &text, [460.0, 440.0]);
            Ok(say(text, format!("{n} note{}, Sir. On the board.", if n == 1 { "" } else { "s" })))
        }
    }
}

// ------------------------------------------------------------------- where things are ----

/// Where Sir said a thing is: the thing (lower case) → {said, at}.
const PLACE: &str = "put_away";
const PREP: &str = r"in|on|under|behind|inside|next\s+to|at|im|in\s+der|in\s+den|auf|auf\s+dem|auf\s+der|unter|unterm|hinter|neben|am";

static PUT: LazyLock<Regex> = LazyLock::new(|| {
    re(&format!(
        r"^i\s+(?:put|left|placed|stored|hid|have\s+put|ve\s+put)\s+(?:my\s+|the\s+)?(?P<a>.+?)\s+(?P<pa>{PREP})\s+(?P<wa>.+)$|^my\s+(?P<b>.+?)\s+(?P<vb>is|are|s)\s+(?P<pb>{PREP})\s+(?P<wb>.+)$|^ich\s+habe?\s+(?:mein(?:e|en)?\s+|die\s+|den\s+|das\s+)?(?P<c>.+?)\s+(?P<pc>{PREP})\s+(?P<wc>.+?)\s+(?:gelegt|getan|gestellt|gepackt|versteckt|gelassen)$|^mein(?:e)?\s+(?P<d>.+?)\s+(?P<vd>ist|sind|liegt|liegen|steht|stehen)\s+(?P<pd>{PREP})\s+(?P<wd>.+)$"
    ))
});
static WHERE: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^where\s+(?:did|have)\s+i\s+(?:put|left|leave|placed|place|stored|store|hid|hide)\s+(?:my\s+|the\s+)?(?P<a>.+)$|^wo\s+habe?\s+ich\s+(?:mein(?:e|en)?\s+|die\s+|den\s+|das\s+)?(?P<b>.+?)\s+(?:hingelegt|gelegt|gelassen|hingetan|hingestellt|versteckt)$")
});
static WHERE_IS: LazyLock<Regex> = LazyLock::new(|| re(r"^(?:where\s+(?:is|are|s)|wheres|wo\s+(?:ist|sind|liegt|liegen))\s+(?:my|mein|meine)\s+(?P<a>.+)$"));

/// The thing as it is kept: without a plural "s" a question may drop or add.
fn thing_key(thing: &str) -> String {
    let thing = thing.trim().to_lowercase();
    thing.strip_suffix('s').filter(|t| t.chars().count() >= 3).unwrap_or(&thing).to_string()
}

fn kept_place(thing: &str) -> Option<(Value, String)> {
    let key = thing_key(thing);
    crate::kit::list(PLACE).into_iter().rev().find(|(k, _, _)| *k == key || k.contains(&key) || key.contains(k.as_str())).map(|(_, v, at)| (v, at))
}

/// "My keys are in the top drawer", "I put the charger behind the TV", "ich habe den Schlüssel
/// in die Schublade gelegt"; "where did I put my keys", "wo ist mein Ladekabel". "Where is my
/// X" is only his when he told J.A.I. where X is: else it is a question for the rest of him.
pub fn place_command(s: &str) -> Option<Value> {
    let s = s.trim_end_matches(['?', '.', '!']);
    let pick = |c: &regex::Captures, names: &[&str]| names.iter().find_map(|n| c.name(n)).map(|m| m.as_str().trim().to_string());
    if let Some(c) = WHERE.captures(s) {
        return Some(json!({"action": "where", "thing": pick(&c, &["a", "b"])?}));
    }
    if let Some(c) = WHERE_IS.captures(s) {
        let thing = pick(&c, &["a"])?;
        return kept_place(&thing).map(|_| json!({"action": "where", "thing": thing}));
    }
    let c = PUT.captures(s)?;
    let thing = pick(&c, &["a", "b", "c", "d"])?;
    let mut place = pick(&c, &["wa", "wb", "wc", "wd"])?;
    let prep = pick(&c, &["pa", "pb", "pc", "pd"])?;
    // A place is "the drawer", "my bag", "im Auto": "my phone is on silent", "my PC is in game
    // mode" and "my name is in the credits" are not a thing put somewhere.
    const ARTICLE: &[&str] = &["the", "my", "a", "his", "her", "our", "der", "die", "das", "dem", "den", "meinem", "meiner", "meine", "meinen", "einer", "einem"];
    let placed = matches!(prep.as_str(), "im" | "am" | "unterm") || place.split_whitespace().next().is_some_and(|w| ARTICLE.contains(&w));
    if !placed || thing.split_whitespace().count() > 4 || matches!(thing.as_str(), "name" | "turn" | "birthday" | "head" | "life" | "mind" | "heart") {
        return None;
    }
    let german = c.name("c").or(c.name("d")).is_some();
    if c.name("c").is_some() {
        // ("… in die Schublade gelegt": where it lies now is "in der Schublade".)
        for (to, now) in [("die ", "der "), ("den ", "dem ")] {
            if let Some(rest) = place.strip_prefix(to) {
                place = format!("{now}{rest}");
            }
        }
    }
    let verb = pick(&c, &["vb", "vd"]).map(|v| if v == "s" { "is".to_string() } else { v }).unwrap_or_else(|| (if german { "liegt" } else if thing.ends_with('s') { "are" } else { "is" }).to_string());
    let said = if german { format!("{thing} {verb} {prep} {place}") } else { format!("your {thing} {verb} {prep} {place}") };
    Some(json!({"action": "put", "thing": thing, "said": said, "german": german}))
}

/// "today at 18:04", "yesterday at 7:30", "on Monday", "on 3 November".
pub(crate) fn when_said(at: &str, now: chrono::NaiveDateTime) -> String {
    let Ok(at) = chrono::NaiveDateTime::parse_from_str(at.get(..19).unwrap_or(at), "%Y-%m-%d %H:%M:%S") else { return "earlier".to_string() };
    match (now.date() - at.date()).num_days() {
        0 => format!("today at {}", at.format("%H:%M")),
        1 => format!("yesterday at {}", at.format("%H:%M")),
        2..=6 => format!("on {}", at.format("%A")),
        _ => format!("on {}", at.format("%-d %B")),
    }
}

pub fn place(args: &Value) -> Result<String, String> {
    let thing = args["thing"].as_str().unwrap_or("").trim();
    if thing.is_empty() {
        return Err("Which thing, Sir?".to_string());
    }
    if args["action"] == "put" {
        let said = args["said"].as_str().unwrap_or(thing);
        crate::kit::put(PLACE, &thing_key(thing), &json!({"said": said}))?;
        return Ok(say(format!("Kept: {said}."), if args["german"] == true { "Gemerkt, Sir." } else { "Noted, Sir." }));
    }
    match kept_place(thing) {
        Some((v, at)) => {
            let said = v["said"].as_str().unwrap_or("").to_string();
            let when = when_said(&at, chrono::Local::now().naive_local());
            Ok(say(format!("{said} (you told me {when})"), format!("You said {said}, Sir. That was {when}.")))
        }
        None => Ok(say(format!("You haven't told me where {thing} is."), format!("You haven't told me where you put {thing}, Sir."))),
    }
}

// --------------------------------------------------------------------------- birthdays ----
// "Max's birthday is on 3 March" / "Max hat am 3. März Geburtstag" keeps the day; "when is
// Max's birthday?" reads it; the butler notes it the evening before and on the morning.

const BIRTHDAY: &str = "birthday";
const WHO: &str = r"(?P<who>\p{L}[\p{L}.' -]{0,40}?)";

/// (pattern, German): the orders that keep a birthday; `who` and `date` are captured.
static BIRTHDAY_SET: LazyLock<Vec<(Regex, bool)>> = LazyLock::new(|| {
    [
        (r"^(?:remember |note )?(?:that )?{WHO}(?:'s|') birthday is (?:on )?(?P<date>.+)$", false),
        (r"^{WHO} has (?:(?:his|her|their) )?birthday on (?P<date>.+)$", false),
        (r"^(?:merk dir,? (?:dass )?)?{WHO} hat am (?P<date>.+?) geburtstag$", true),
        (r"^(?:der )?geburtstag von {WHO} ist am (?P<date>.+)$", true),
        (r"^{WHO}'? geburtstag ist am (?P<date>.+)$", true),
    ]
    .into_iter()
    .map(|(p, german)| (re(&p.replace("{WHO}", WHO)), german))
    .collect()
});
static BIRTHDAY_ASK: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    [r"^when(?:'s| is) {WHO}(?:'s|') birthday$", r"^when does {WHO} have (?:(?:his|her|their) )?birthday$", r"^when(?:'s| is) the birthday of {WHO}$", r"^wann hat {WHO} geburtstag$", r"^wann ist {WHO}'? geburtstag$", r"^wann ist der geburtstag von {WHO}$"]
        .into_iter()
        .map(|p| re(&p.replace("{WHO}", WHO)))
        .collect()
});
static DAY_MONTH: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    let months = crate::weather_history::MONTHS;
    [
        format!(r"^(?:the )?(?P<d>\d{{1,2}})(?:st|nd|rd|th|\.)?(?: of)? (?P<m>{months})\.?(?:,? (?P<y>\d{{4}}))?$"),
        format!(r"^(?P<m>{months})\.? (?:the )?(?P<d>\d{{1,2}})(?:st|nd|rd|th)?(?:,? (?P<y>\d{{4}}))?$"),
        r"^(?P<d>\d{1,2})\.(?P<n>\d{1,2})\.(?P<y>\d{4})?$".to_string(),
    ]
    .iter()
    .map(|p| re(p))
    .collect()
});

/// "3 March", "March 3rd, 2008", "the 3rd of March", "3. März", "03.03.2008" as (month, day, year).
pub fn day_month(text: &str) -> Option<(u32, u32, Option<i32>)> {
    let text = text.trim().trim_end_matches(['.', ',']).trim();
    let text = if text.ends_with(|c: char| c.is_ascii_digit()) && text.matches('.').count() == 1 { format!("{text}.") } else { text.to_string() };
    let c = DAY_MONTH.iter().find_map(|r| r.captures(&text))?;
    let month = match c.name("m") {
        Some(m) => crate::weather_history::month_of(m.as_str())?,
        None => c.name("n")?.as_str().parse().ok()?,
    };
    let day: u32 = c.name("d")?.as_str().parse().ok()?;
    let year = c.name("y").and_then(|y| y.as_str().parse().ok());
    chrono::NaiveDate::from_ymd_opt(year.unwrap_or(2024), month, day).map(|_| (month, day, year))
}

/// The birthday's next day from `today` on (29 February falls on the 28th in other years).
pub fn next_birthday(today: chrono::NaiveDate, month: u32, day: u32) -> chrono::NaiveDate {
    use chrono::Datelike;
    let on = |y: i32| chrono::NaiveDate::from_ymd_opt(y, month, day).or_else(|| chrono::NaiveDate::from_ymd_opt(y, month, day.saturating_sub(1))).unwrap_or(today);
    let this = on(today.year());
    if this >= today { this } else { on(today.year() + 1) }
}

/// The name as he typed it (its own capitals), from the lowercase match.
fn name_as_typed(original: &str, lower: &str, who: regex::Match) -> String {
    match original.get(who.range()).filter(|_| original.len() == lower.len()) {
        Some(typed) => typed.trim().to_string(),
        None => who.as_str().split_whitespace().map(capitalised).collect::<Vec<_>>().join(" "),
    }
}

pub fn birthday_command(original: &str) -> Option<Value> {
    let typed = original.trim().trim_end_matches(['?', '!']).trim().replace('’', "'");
    let lower = typed.to_lowercase();
    let not_a_name = |who: &str| matches!(who.split_whitespace().next().unwrap_or(""), "my" | "your" | "his" | "her" | "their" | "its" | "our" | "the" | "a" | "mein" | "meine" | "dein" | "deine" | "sein" | "seine" | "ihr" | "ihre" | "unser" | "der" | "die" | "das" | "it" | "es");
    for (r, german) in BIRTHDAY_SET.iter() {
        let Some(c) = r.captures(&lower) else { continue };
        let (who, date) = (c.name("who")?, c.name("date")?.as_str());
        if not_a_name(who.as_str()) {
            return None;
        }
        let (month, day, year) = day_month(date)?;
        return Some(json!({"action": "set", "name": name_as_typed(&typed, &lower, who), "month": month, "day": day, "year": year, "german": german}));
    }
    let c = BIRTHDAY_ASK.iter().find_map(|r| r.captures(&lower))?;
    let who = c.name("who")?;
    (!not_a_name(who.as_str())).then(|| json!({"action": "when", "name": name_as_typed(&typed, &lower, who)}))
}

/// Every kept birthday: (name, month, day, year).
pub fn birthdays() -> Vec<(String, u32, u32, Option<i32>)> {
    crate::kit::list(BIRTHDAY)
        .into_iter()
        .filter_map(|(_, v, _)| Some((v["name"].as_str()?.to_string(), v["month"].as_u64()? as u32, v["day"].as_u64()? as u32, v["year"].as_i64().map(|y| y as i32))))
        .collect()
}

/// A kept birthday by name: the same name, or one with a genitive "s" more or less ("Pauls").
fn birthday_of(name: &str) -> Option<(String, u32, u32, Option<i32>)> {
    let want = name.to_lowercase();
    birthdays().into_iter().find(|(n, ..)| {
        let n = n.to_lowercase();
        n == want || n.strip_suffix('s') == Some(want.as_str()) || want.strip_suffix('s') == Some(n.as_str())
    })
}

/// "3 March, Sir: in 107 days. He turns 18." (the answer's middle part).
pub fn birthday_when(today: chrono::NaiveDate, name: &str, month: u32, day: u32, year: Option<i32>) -> String {
    use chrono::Datelike;
    let next = next_birthday(today, month, day);
    let when = match (next - today).num_days() {
        0 => "That's today".to_string(),
        1 => "That's tomorrow".to_string(),
        n => format!("That's in {n} days"),
    };
    let turns = year.filter(|y| *y < next.year()).map(|y| format!(": {name} turns {}", next.year() - y)).unwrap_or_default();
    format!("{name}'s birthday is on {}, Sir. {when}{turns}.", next.format("%-d %B"))
}

pub fn birthday(args: &Value) -> Result<String, String> {
    let name = args["name"].as_str().unwrap_or("").trim();
    if name.is_empty() {
        return Err("Whose birthday, Sir?".to_string());
    }
    let today = chrono::Local::now().date_naive();
    if args["action"] == "set" {
        let (month, day) = (args["month"].as_u64().unwrap_or(0) as u32, args["day"].as_u64().unwrap_or(0) as u32);
        let shown = chrono::NaiveDate::from_ymd_opt(2024, month, day).ok_or("That is no day of the year, Sir.")?.format("%-d %B");
        crate::kit::put(BIRTHDAY, &name.to_lowercase(), &json!({"name": name, "month": month, "day": day, "year": args["year"]}))?;
        let line = format!("Noted, Sir: {name}'s birthday is on {shown}.");
        return Ok(say(line.clone(), if args["german"] == true { "Gemerkt, Sir.".to_string() } else { line }));
    }
    match birthday_of(name) {
        Some((name, month, day, year)) => {
            let line = birthday_when(today, &name, month, day, year);
            Ok(say(line.clone(), line))
        }
        None => Ok(say(format!("You haven't told me {name}'s birthday yet. Say \"{name}'s birthday is on 3 March\"."), format!("You haven't told me {name}'s birthday yet, Sir."))),
    }
}

/// The butler's birthday note when one is due: the evening before (19-23 h) or the morning of
/// the day (7-12 h). `calendar`: all-day events (day, title); those with "birthday" or
/// "Geburtstag" in the title count. (key that marks it told, card title, card body, spoken).
pub fn birthday_note(now: chrono::NaiveDateTime, people: &[(String, u32, u32, Option<i32>)], calendar: &[(chrono::NaiveDate, String)]) -> Option<(String, String, String, String)> {
    use chrono::Datelike;
    let today = now.date();
    let (day, when) = match now.hour() {
        7..=11 => (today, "today"),
        19..=22 => (today + chrono::Duration::days(1), "tomorrow"),
        _ => return None,
    };
    let mut labels: Vec<String> = people
        .iter()
        .filter(|(_, m, d, _)| next_birthday(day, *m, *d) == day)
        .map(|(name, _, _, year)| match year.filter(|y| *y < day.year()) {
            Some(y) => format!("{name}, who turns {}", day.year() - y),
            None => name.clone(),
        })
        .collect();
    for (_, title) in calendar.iter().filter(|(on, _)| *on == day) {
        let low = title.to_lowercase();
        let named = people.iter().any(|(n, ..)| low.contains(&n.to_lowercase()));
        if (low.contains("birthday") || low.contains("geburtstag")) && !named && !labels.contains(title) {
            labels.push(title.clone());
        }
    }
    if labels.is_empty() {
        return None;
    }
    let spoken = format!("{} {when}, Sir: {}.", if labels.len() == 1 { "A birthday" } else { "Birthdays" }, crate::briefing::join_and(&labels));
    Some((format!("{day}|{when}"), format!("BIRTHDAY // {}", when.to_uppercase()), labels.join("\n"), spoken))
}

// ------------------------------------------------------------------------- sleep timer ----

static SLEEP_SET: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:stop|pause|turn\s+off|switch\s+off)\s+(?:the\s+|my\s+)?(?P<what>music|everything|sound|playback|song|video)\s+(?:in|after)\s+(?P<a>.+)$|^(?:set\s+)?(?:a\s+|the\s+)?sleep\s+timer\s+(?:for\s+|of\s+|to\s+|in\s+)?(?P<b>.+)$|^(?P<what2>music|everything)\s+off\s+in\s+(?P<c>.+)$|^(?:mach\s+)?(?:die\s+)?musik\s+in\s+(?P<d>.+?)\s+aus$|^schlaftimer\s+(?:auf\s+|für\s+)?(?P<e>.+)$")
});
static SLEEP_CANCEL: LazyLock<Regex> = LazyLock::new(|| re(r"^(?:cancel|stop|clear|end|forget)\s+(?:the\s+)?sleep\s+timer$|^sleep\s+timer\s+off$|^schlaftimer\s+(?:aus|abbrechen)$"));
static SLEEP_STATUS: LazyLock<Regex> =
    LazyLock::new(|| re(r"^how\s+(?:long|much\s+time)\s+(?:is\s+)?(?:left\s+)?(?:on|until|till)\s+the\s+sleep\s+timer(?:\s+is\s+up)?$|^sleep\s+timer(?:\s+status)?$|^is\s+(?:a|the)\s+sleep\s+timer\s+(?:set|on|running)$|^wie\s+lange\s+läuft\s+der\s+schlaftimer\s+noch$"));

/// "Stop the music in 30 minutes", "sleep timer 20 minutes", "cancel the sleep timer".
pub fn sleep_command(s: &str) -> Option<Value> {
    if SLEEP_CANCEL.is_match(s) {
        return Some(json!({"action": "cancel"}));
    }
    if SLEEP_STATUS.is_match(s) {
        return Some(json!({"action": "status"}));
    }
    let c = SLEEP_SET.captures(s)?;
    let span = ["a", "b", "c", "d", "e"].iter().find_map(|g| c.name(g))?.as_str();
    let minutes = (crate::listening_lock::duration_in(span)?.as_secs() / 60).clamp(1, 720);
    let what = c.name("what").or(c.name("what2")).map_or("music", |m| if m.as_str() == "everything" { "everything" } else { "music" });
    Some(json!({"action": "set", "minutes": minutes, "what": what}))
}

/// The timer that runs: its number (a newer one or a cancel makes an older one void), when it
/// is up, and what it ends.
static SLEEPING: Mutex<Option<(u64, Instant, String)>> = Mutex::new(None);
/// How long the volume sinks before the players are paused.
const FADE: Duration = Duration::from_secs(12);

fn minutes_words(minutes: u64) -> String {
    match minutes {
        1 => "a minute".to_string(),
        60 => "an hour".to_string(),
        m if m % 60 == 0 => format!("{} hours", m / 60),
        m => format!("{m} minutes"),
    }
}

/// How long the sleep timer has left, in a few words (None: none runs).
pub fn sleep_left() -> Option<String> {
    let (_, due, what) = lock(&SLEEPING).clone()?;
    let left = due.saturating_duration_since(Instant::now()).as_secs();
    Some(format!("{} off in {}", if what == "everything" { "everything" } else { "music" }, if left < 90 { format!("{left} s") } else { format!("{} min", left.div_ceil(60)) }))
}

pub fn sleep_timer(args: &Value) -> Result<String, String> {
    match args["action"].as_str().unwrap_or("status") {
        "cancel" => Ok(match lock(&SLEEPING).take() {
            Some(_) => say("Sleep timer cancelled.", "The sleep timer is off, Sir."),
            None => say("No sleep timer was running.", "No sleep timer was set, Sir."),
        }),
        "set" => {
            let minutes = args["minutes"].as_u64().unwrap_or(30).clamp(1, 720);
            let what = if args["what"] == "everything" { "everything" } else { "music" }.to_string();
            let id = crate::kit::stamp().parse::<u64>().unwrap_or(0);
            let due = Instant::now() + Duration::from_secs(minutes * 60);
            *lock(&SLEEPING) = Some((id, due, what.clone()));
            tracing::info!("[SLEEP TIMER] Set: {what} off in {minutes} min");
            let ends = what.clone();
            tokio::spawn(async move {
                tokio::time::sleep_until(tokio::time::Instant::from_std(due.checked_sub(FADE).unwrap_or(due))).await;
                if lock(&SLEEPING).as_ref().is_none_or(|(now, _, _)| *now != id) {
                    return;
                }
                fall_silent(&ends, move || lock(&SLEEPING).as_ref().is_some_and(|(now, _, _)| *now == id)).await;
                let mut running = lock(&SLEEPING);
                if running.as_ref().is_some_and(|(now, _, _)| *now == id) {
                    *running = None;
                }
            });
            let span = minutes_words(minutes);
            Ok(say(format!("Sleep timer: {what} off in {span}."), if what == "everything" { format!("Everything goes quiet in {span}, Sir.") } else { format!("The music stops in {span}, Sir.") }))
        }
        _ => Ok(match sleep_left() {
            Some(left) => say(format!("Sleep timer: {left}."), format!("The sleep timer says {left}, Sir.")),
            None => say("No sleep timer is running.", "No sleep timer is set, Sir."),
        }),
    }
}

/// The master volume (0-1), for the timer's fade.
async fn master_volume() -> Option<f32> {
    #[cfg(windows)]
    return crate::win32::call(crate::win32::get_volume).await.ok().and_then(Result::ok).map(|(level, _)| level);
    #[cfg(not(windows))]
    return crate::audio_mixer::master_state().await.ok().map(|(level, _)| level);
}

async fn set_master_volume(level: f32) {
    #[cfg(windows)]
    let _ = crate::win32::call(move || crate::win32::set_volume(level)).await;
    #[cfg(not(windows))]
    let _ = crate::media::execute(crate::media::MediaAction::SetVolume((level.clamp(0.0, 1.0) * 100.0).round() as u8)).await;
}

/// The timer is up: the volume sinks, every player is paused (with "everything", J.A.I.'s own
/// voice too), and the volume stands again where it stood. A test touches nothing.
async fn fall_silent(what: &str, still_wanted: impl Fn() -> bool) {
    if crate::platform::hands_off() {
        tracing::info!("[SLEEP TIMER] Up: a test session leaves Sir's players and his volume alone");
        return;
    }
    let stood = master_volume().await;
    if let Some(level) = stood.filter(|l| *l > 0.02) {
        let steps = 24;
        for k in 1..=steps {
            // (Cancelled, or set anew, while the volume sinks: it stands again, nothing is paused.)
            if !still_wanted() {
                set_master_volume(level).await;
                tracing::info!("[SLEEP TIMER] Called off while the volume sank: it stands where it stood");
                return;
            }
            set_master_volume(level * (1.0 - 0.9 * k as f32 / steps as f32)).await;
            tokio::time::sleep(FADE / steps).await;
        }
    }
    let stopped = crate::music::stop_all_active_media().await;
    let _ = what;
    if let Some(level) = stood {
        tokio::time::sleep(Duration::from_millis(600)).await;
        set_master_volume(level).await;
    }
    tracing::info!("[SLEEP TIMER] Up: {} player(s) paused, the volume stands where it stood", stopped.sessions.len() + stopped.windows.len());
    crate::hud_notify::record("The sleep timer was up: I paused what was playing.", false);
}

// ----------------------------------------------------------------------- whenever-rules ----

const RULE: &str = "rule";
pub const RULES: &str = "kit_rules";

/// When a rule fires.
#[derive(Debug, Clone, PartialEq)]
pub enum When {
    Starts(String),
    Closes(String),
    /// Every day at this hour and minute.
    At(u32, u32),
    /// What the PC of an account does (its name as said: "max", "maxs"; "i" is this PC), as
    /// Sir's PC hears it (`fleet::events`).
    Pc(String, PcEvent),
}

impl When {
    pub fn words(&self) -> String {
        match self {
            When::Starts(app) => format!("whenever {app} starts"),
            When::Closes(app) => format!("whenever {app} closes"),
            When::At(h, m) => format!("every day at {h:02}:{m:02}"),
            When::Pc(who, what) => {
                let me = who == "i";
                let (who, s) = if me { ("I".to_string(), "") } else { (capitalised(who), "s") };
                match what {
                    PcEvent::Online => format!("whenever {who}'s PC comes online"),
                    PcEvent::Offline => format!("whenever {who}'s PC goes offline"),
                    PcEvent::Plays(game) if game.is_empty() => format!("whenever {who} start{s} a game"),
                    PcEvent::Plays(game) => format!("whenever {who} play{s} {game}"),
                    PcEvent::Stops(game) => format!("whenever {who} stop{s} playing{}", if game.is_empty() { String::new() } else { format!(" {game}") }),
                }
            }
        }
    }

    fn to_json(&self) -> Value {
        match self {
            When::Starts(app) => json!({"kind": "starts", "app": app}),
            When::Closes(app) => json!({"kind": "closes", "app": app}),
            When::At(h, m) => json!({"kind": "at", "hour": h, "minute": m}),
            When::Pc(who, what) => {
                let (event, game) = match what {
                    PcEvent::Online => ("online", ""),
                    PcEvent::Offline => ("offline", ""),
                    PcEvent::Plays(game) => ("plays", game.as_str()),
                    PcEvent::Stops(game) => ("stops", game.as_str()),
                };
                json!({"kind": "pc", "who": who, "event": event, "game": game})
            }
        }
    }

    fn of(v: &Value) -> Option<When> {
        match v["kind"].as_str()? {
            "starts" => Some(When::Starts(v["app"].as_str()?.to_string())),
            "closes" => Some(When::Closes(v["app"].as_str()?.to_string())),
            "at" => Some(When::At(v["hour"].as_u64()? as u32, v["minute"].as_u64()? as u32)),
            "pc" => {
                let game = v["game"].as_str().unwrap_or("").to_string();
                let what = match v["event"].as_str()? {
                    "online" => PcEvent::Online,
                    "offline" => PcEvent::Offline,
                    "plays" => PcEvent::Plays(game),
                    "stops" => PcEvent::Stops(game),
                    _ => return None,
                };
                Some(When::Pc(v["who"].as_str()?.to_string(), what))
            }
            _ => None,
        }
    }

    /// Whether it holds for the programs that have just started and closed, the minute of the
    /// day that has just begun and what the PCs have just done.
    pub fn fires(&self, started: &[String], closed: &[String], minute: Option<(u32, u32)>, pcs: &[(String, PcEvent)]) -> bool {
        let game = |wanted: &str, game: &str| {
            let compact = |s: &str| s.chars().filter(|c| c.is_alphanumeric()).collect::<String>().to_lowercase();
            wanted.is_empty() || compact(game).contains(&compact(wanted))
        };
        match self {
            When::Starts(app) => started.iter().any(|p| is_app(app, p)),
            When::Closes(app) => closed.iter().any(|p| is_app(app, p)),
            When::At(h, m) => minute == Some((*h, *m)),
            When::Pc(who, what) => pcs.iter().any(|(name, done)| {
                crate::fleet::names_in(who).contains(name)
                    && match (what, done) {
                        (PcEvent::Online, PcEvent::Online) | (PcEvent::Offline, PcEvent::Offline) => true,
                        (PcEvent::Plays(wanted), PcEvent::Plays(played)) | (PcEvent::Stops(wanted), PcEvent::Stops(played)) => game(wanted, played),
                        _ => false,
                    }
            }),
        }
    }
}

/// What a PC does, said at the start of `s` in plain words: "max comes online", "maxs pc goes
/// offline", "max starts a game", "max plays minecraft", "max stops playing", "i start a game"
/// (this PC). With `whole` the words are that alone (a protocol's line); else the order follows.
fn pc_event(s: &str, whole: bool) -> Option<(When, String)> {
    let words: Vec<&str> = s.split_whitespace().collect();
    let who = *words.first()?;
    let pc = words.get(1).is_some_and(|w| matches!(*w, "pc" | "computer" | "laptop" | "rechner"));
    let me = matches!(who, "i" | "ich");
    // (A program's own name is the app rule's: "discord starts".)
    if (!pc && !me && known_app(who)) || !who.chars().all(char::is_alphanumeric) {
        return None;
    }
    let rest = &words[1 + pc as usize..];
    // The game named, then the order: in a rule the longest run of words that names a program he
    // knows (one word else); in a protocol's line all of them.
    let named = |from: &[&str], needed: bool| -> Option<(String, String)> {
        if whole {
            return Some((from.join(" "), String::new()));
        }
        let take = (1..=from.len().saturating_sub(1).min(3)).rev().find(|n| known_app(&from[..*n].join(" ")));
        match take.or(needed.then_some(1)) {
            Some(n) if from.len() > n => Some((from[..n].join(" "), from[n..].join(" "))),
            _ if needed => None,
            _ => Some((String::new(), from.join(" "))),
        }
    };
    let (what, order) = match rest {
        [is, "online", after @ ..] if !me && matches!(*is, "comes" | "goes" | "is" | "kommt" | "ist") => (PcEvent::Online, after.join(" ")),
        [is, "offline", after @ ..] if !me && matches!(*is, "goes" | "is" | "geht" | "ist") => (PcEvent::Offline, after.join(" ")),
        [start, "a", "game", after @ ..] if matches!(*start, "starts" | "start") => (PcEvent::Plays(String::new()), after.join(" ")),
        [start, "gaming" | "playing", after @ ..] if matches!(*start, "starts" | "start" | "is" | "am") => {
            let (game, order) = named(after, false)?;
            (PcEvent::Plays(game), order)
        }
        [stop, "gaming" | "playing", after @ ..] if matches!(*stop, "stops" | "stop") => {
            let (game, order) = named(after, false)?;
            (PcEvent::Stops(game), order)
        }
        [play, after @ ..] if !after.is_empty() && matches!(*play, "plays" | "play" | "spielt" | "spiele") => {
            let (game, order) = named(after, true)?;
            (PcEvent::Plays(game), order)
        }
        // ("max starts minecraft": a program he knows, or on that PC.)
        [start, after @ ..] if !me && !after.is_empty() && matches!(*start, "starts" | "launches" | "opens") && (pc || whole || named(after, false).is_some_and(|(game, _)| !game.is_empty())) => {
            let (game, order) = named(after, true)?;
            (PcEvent::Plays(game), order)
        }
        [quit, after @ ..] if !me && !after.is_empty() && matches!(*quit, "quits" | "closes") && (pc || whole) => {
            let (game, order) = named(after, true)?;
            (PcEvent::Stops(game), order)
        }
        _ => return None,
    };
    let order = order.trim_start_matches("then ").trim_start_matches("dann ").trim().to_string();
    (whole == order.is_empty()).then(|| (When::Pc(if me { "i".into() } else { who.to_string() }, what), order))
}

/// A protocol's WHEN line ("Minecraft starts", "every day at 7:30", "Max comes online", "I start
/// a game"): the rules' grammar, without an order.
pub fn when_of(line: &str) -> Option<When> {
    let said = crate::ear::words(line).join(" ");
    let s = RULE_WHEN.captures(&said).and_then(|c| c.get(1)).map_or(said.as_str(), |m| m.as_str());
    if let Some((when, _)) = pc_event(s, true) {
        return Some(when);
    }
    // (The rules' grammar wants an order after it: one is lent.)
    parse_rule(&format!("{s} then x")).or_else(|| parse_rule(&format!("when {s} then x"))).map(|(when, _)| when)
}

static RULE_LIST: LazyLock<Regex> = LazyLock::new(|| re(r"^(?:list|show)\s+(?:me\s+)?(?:my\s+|the\s+|all\s+)?rules$|^what\s+rules\s+(?:do\s+(?:i|you)\s+have|are\s+there)$|^(?:zeig\s+(?:mir\s+)?)?meine\s+regeln$"));
static RULE_REMOVE: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:delete|remove|forget|drop)\s+(?:the\s+|my\s+)?rule\s+(?:number\s+)?(?P<n>\d+)$|^(?:delete|remove|forget|drop)\s+(?:the\s+|my\s+)?rule\s+(?:about|for|with)\s+(?P<about>.+)$|^(?:delete|remove|forget)\s+(?P<all>all)\s+(?:my\s+)?rules$|^lösche?\s+regel\s+(?P<n2>\d+)$")
});
static RULE_WHEN: LazyLock<Regex> = LazyLock::new(|| re(r"^(?:whenever|every\s+time|each\s+time|when|as\s+soon\s+as|immer\s+wenn|jedes\s+mal\s+wenn|sobald|wenn)\s+(.+)$"));
static RULE_SUBJECT: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?P<app>.+?)\s+(?:(?P<starts>starts|opens|launches|is\s+started|is\s+opened|comes\s+up|is\s+running|startet|aufgeht|geöffnet\s+wird|läuft)|(?P<closes>closes|quits|exits|ends|is\s+closed|shuts\s+down|zugeht|geschlossen\s+wird|beendet\s+wird))\s+(?:then\s+|dann\s+)?(?P<order>.+)$")
});
static RULE_I: LazyLock<Regex> = LazyLock::new(|| re(r"^i\s+(?:(?P<starts>start|open|launch|run)|(?P<closes>close|quit|exit|leave))\s+(?P<rest>.+)$"));
static RULE_DAILY: LazyLock<Regex> = LazyLock::new(|| re(r"^(?:every\s+day|daily|each\s+day|jeden\s+tag|täglich)\s+(?:at|um)\s+(?P<h>\d{1,2})(?:\s*[:.]?\s*(?P<m>\d{2}))?\s*(?P<half>am|pm)?(?:\s+uhr)?\s+(?:then\s+|dann\s+)?(?P<order>.+)$"));

/// Whether `name` is exactly what a program J.A.I. knows of is called (a built-in one by its
/// name or one of its other names, or an installed one).
fn known_app(name: &str) -> bool {
    crate::apps::find_spec(name).is_some_and(|spec| spec.id == name || spec.display.eq_ignore_ascii_case(name) || spec.aliases.contains(&name)) || crate::appindex::is_known(name)
}

/// "I start <app> <order>" has no comma once it is normalised: the app is the longest run of
/// words that names a program he knows (one word when none does).
fn split_app(rest: &str) -> Option<(String, String)> {
    let words: Vec<&str> = rest.split_whitespace().filter(|w| !matches!(*w, "then" | "dann")).collect();
    let words: Vec<&str> = if matches!(words.first(), Some(&("the" | "my" | "a"))) { words[1..].to_vec() } else { words };
    if words.len() < 2 {
        return None;
    }
    let take = (1..=words.len().saturating_sub(1).min(3)).rev().find(|n| known_app(&words[..*n].join(" "))).unwrap_or(1);
    Some((words[..take].join(" "), words[take..].join(" ")))
}

/// The rule in a sentence: when it fires and what is then to be done.
pub fn parse_rule(s: &str) -> Option<(When, String)> {
    if let Some(c) = RULE_DAILY.captures(s) {
        let mut h: u32 = c.name("h")?.as_str().parse().ok()?;
        let m: u32 = c.name("m").map_or(Some(0), |m| m.as_str().parse().ok())?;
        match c.name("half").map(|m| m.as_str()) {
            Some("pm") if h < 12 => h += 12,
            Some("am") if h == 12 => h = 0,
            _ => {}
        }
        return (h < 24 && m < 60).then(|| (When::At(h, m), c.name("order").map_or("", |m| m.as_str()).trim().to_string()));
    }
    let rest = RULE_WHEN.captures(s)?.get(1)?.as_str();
    if let Some(found) = pc_event(rest, false) {
        return Some(found);
    }
    if let Some(c) = RULE_I.captures(rest) {
        let (app, order) = split_app(c.name("rest")?.as_str())?;
        return Some((if c.name("starts").is_some() { When::Starts(app) } else { When::Closes(app) }, order));
    }
    let c = RULE_SUBJECT.captures(rest)?;
    let app = c.name("app")?.as_str().trim_start_matches("the ").trim_start_matches("my ").trim().to_string();
    let order = c.name("order")?.as_str().trim().to_string();
    (!app.is_empty() && app.split(' ').count() <= 3).then(|| (if c.name("starts").is_some() { When::Starts(app) } else { When::Closes(app) }, order))
}

/// "Whenever Minecraft starts, set the music to 20 percent", "every day at 7:30 open my
/// calendar", "list my rules", "delete rule 2", "delete the rule about Minecraft".
pub fn rule_command(s: &str) -> Option<Value> {
    if RULE_LIST.is_match(s) {
        return Some(json!({"action": "list"}));
    }
    if let Some(c) = RULE_REMOVE.captures(s) {
        if c.name("all").is_some() {
            return Some(json!({"action": "remove", "all": true}));
        }
        return Some(match c.name("n").or(c.name("n2")) {
            Some(n) => json!({"action": "remove", "number": n.as_str().parse::<u64>().ok()?}),
            None => json!({"action": "remove", "about": c.name("about")?.as_str().trim()}),
        });
    }
    let (when, order) = parse_rule(s)?;
    // (Only what he can carry out is a rule: "when Discord closes I feel lonely" is talk.)
    (!order.is_empty() && crate::ear::is_order(&order)).then(|| json!({"action": "add", "when": when.to_json(), "then": order}))
}

fn rules() -> Vec<(String, When, String)> {
    crate::kit::list(RULE).into_iter().filter_map(|(key, v, _)| Some((key, When::of(&v["when"])?, v["then"].as_str()?.to_string()))).collect()
}

fn rules_text() -> String {
    let all = rules();
    if all.is_empty() {
        return "No rules yet. Say, for example: \"whenever Minecraft starts, set the music to 20 percent\".".to_string();
    }
    let lines: Vec<String> = all.iter().enumerate().map(|(k, (_, when, then))| format!("{}.  {}\n    → {then}", k + 1, capitalised(&when.words()))).collect();
    format!("{}\n\n\"Delete rule 2\" removes one.", lines.join("\n\n"))
}

fn capitalised(text: &str) -> String {
    let mut c = text.chars();
    c.next().map_or(String::new(), |first| first.to_uppercase().chain(c).collect())
}

pub fn rule(args: &Value) -> Result<String, String> {
    let refresh = || {
        if crate::kit::text_is_up(RULES) {
            crate::kit::show_text(RULES, "Rules // whenever … then …", &rules_text(), [500.0, 380.0]);
        }
    };
    match args["action"].as_str().unwrap_or("list") {
        "add" => {
            let when = When::of(&args["when"]).ok_or("I did not catch when the rule should hold, Sir.")?;
            let then = args["then"].as_str().unwrap_or("").trim().to_string();
            if then.is_empty() {
                return Err("And what shall I do then, Sir?".to_string());
            }
            if rules().iter().any(|(_, w, t)| *w == when && *t == then) {
                return Ok(say("That rule is kept already.", "I have that rule already, Sir."));
            }
            crate::kit::put(RULE, &crate::kit::stamp(), &json!({"when": when.to_json(), "then": then}))?;
            refresh();
            tracing::info!("[RULES] Kept: {} → {then}", when.words());
            Ok(say(format!("Rule kept: {} → {then}", when.words()), format!("Understood, Sir. {}, I'll {then}.", capitalised(&when.words()))))
        }
        "remove" => {
            let all = rules();
            let gone: Vec<&(String, When, String)> = if args["all"] == true {
                all.iter().collect()
            } else if args["last"] == true {
                all.last().into_iter().collect()
            } else if let Some(n) = args["number"].as_u64() {
                all.get((n as usize).wrapping_sub(1)).into_iter().collect()
            } else {
                let about = args["about"].as_str().unwrap_or("").to_lowercase();
                all.iter().filter(|(_, when, then)| !about.is_empty() && (when.words().to_lowercase().contains(&about) || then.to_lowercase().contains(&about))).collect()
            };
            if gone.is_empty() {
                return Err("I have no such rule, Sir. \"List my rules\" shows them with their numbers.".to_string());
            }
            for (key, _, _) in &gone {
                crate::kit::remove(RULE, key);
            }
            refresh();
            Ok(say(format!("{} rule(s) removed.", gone.len()), if gone.len() == 1 { "That rule is gone, Sir.".to_string() } else { format!("{} rules are gone, Sir.", gone.len()) }))
        }
        _ => {
            let n = rules().len();
            let text = rules_text();
            crate::kit::show_text(RULES, "Rules // whenever … then …", &text, [500.0, 380.0]);
            Ok(say(text, if n == 0 { "You have no rules yet, Sir.".to_string() } else { format!("{n} rule{}, Sir. On the board.", if n == 1 { "" } else { "s" }) }))
        }
    }
}

/// Whether the running program `process` (lower case, without ".exe") is the app a rule names.
fn is_app(app: &str, process: &str) -> bool {
    let named = app.to_lowercase();
    if let Some(spec) = crate::apps::find_spec(&named) {
        if spec.processes.contains(&process) {
            return true;
        }
    }
    let compact: String = named.chars().filter(|c| c.is_alphanumeric()).collect();
    let running: String = process.chars().filter(|c| c.is_alphanumeric()).collect();
    // (The program itself or a build of it, "discordptb", "codeinsiders": not a helper whose
    // name merely begins the same, "steamwebhelper".)
    const BUILDS: &[&str] = &["ptb", "canary", "beta", "dev", "development", "nightly", "insiders", "preview", "x64", "64", "32", "portable"];
    compact.len() >= 3 && (running == compact || running.strip_prefix(compact.as_str()).is_some_and(|rest| BUILDS.contains(&rest)) || (named == "minecraft" && running == "javaw"))
}

/// The orders of the rules that fire: for the programs that have just started and closed, for
/// the minute of the day when it has just begun (`minute`: Some((hour, minute))) and for what
/// the PCs have just done.
pub fn fired(started: &[String], closed: &[String], minute: Option<(u32, u32)>, pcs: &[(String, PcEvent)]) -> Vec<(String, String)> {
    rules().into_iter().filter(|(_, when, _)| when.fires(started, closed, minute, pcs)).map(|(_, when, then)| (when.words(), then)).collect()
}

/// Watches what starts and closes, the clock and the PCs, and has the rules' orders carried out
/// as if Sir had typed them and the protocols that wait on it engaged. (A test and a scripted
/// session run no rules.)
pub fn start(tools: crate::tools::ToolContext, stop: tokio_util::sync::CancellationToken) {
    if cfg!(test) || crate::platform::in_test_session() {
        return;
    }
    tokio::spawn(async move {
        use sysinfo::{ProcessesToUpdate, System};
        let mut system = System::new();
        let mut running: Option<std::collections::HashSet<String>> = None;
        let mut last_minute = None;
        let mut last_game: Option<Option<String>> = None;
        let mut ticks = 0u64;
        let mut tick = tokio::time::interval(Duration::from_secs(4));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                _ = stop.cancelled() => return,
                _ = tick.tick() => {}
            }
            // (What the dashboard on the upright monitor shows, while it is on; and, once a
            // minute, the names of the desk layouts for the fast path.)
            crate::kit_screen::gather(&tools);
            // The guests' time is up: said on the board, not aloud.
            if crate::speaker_id::guests_just_left() {
                crate::hud_notify::record("Guest mode is over: I answer to you alone again.", false);
            }
            if ticks % 15 == 0 {
                crate::kit_screen::sync_layouts(&tools);
            }
            ticks += 1;
            let all = rules();
            let waiting: Vec<(String, When)> = tools.protocols.list().into_iter().flat_map(|p| p.when.iter().filter_map(|line| when_of(line)).map(|when| (p.name.clone(), when)).collect::<Vec<_>>()).collect();
            // What the other PCs did, and the game on this one ("i": the first look only learns it).
            let mut pcs = crate::fleet::events();
            let game = crate::vram_guard::game_now().map(|(game, _)| game);
            if let Some(before) = last_game.replace(game.clone()).filter(|before| *before != game) {
                pcs.extend(before.map(|g| ("i".to_string(), PcEvent::Stops(g))));
                pcs.extend(game.map(|g| ("i".to_string(), PcEvent::Plays(g))));
            }
            if all.is_empty() && waiting.is_empty() {
                running = None;
                continue;
            }
            let (mut started, mut closed) = (Vec::new(), Vec::new());
            if all.iter().map(|(_, when, _)| when).chain(waiting.iter().map(|(_, when)| when)).any(|when| matches!(when, When::Starts(_) | When::Closes(_))) {
                system.refresh_processes(ProcessesToUpdate::All);
                let now: std::collections::HashSet<String> = system.processes().values().map(|p| p.name().to_string_lossy().to_lowercase().trim_end_matches(".exe").to_string()).collect();
                // (The first look only learns what runs: a rule fires on a change, not on a start of his own.)
                if let Some(before) = &running {
                    started = now.difference(before).cloned().collect();
                    closed = before.difference(&now).cloned().collect();
                }
                running = Some(now);
            }
            let clock = chrono::Local::now();
            let minute = (clock.hour(), clock.minute());
            let new_minute = (last_minute != Some(minute)).then_some(minute);
            last_minute = Some(minute);
            for (when, then) in fired(&started, &closed, new_minute, &pcs) {
                tracing::info!("[RULES] {when}: {then}");
                crate::hud_notify::record(&format!("Rule: {when} → {then}"), false);
                let _ = tools.stark.events.send(crate::stark::StarkEvent::RunMacro { name: format!("rule ({when})"), steps: vec![then] });
            }
            for (name, when) in waiting.into_iter().filter(|(_, when)| when.fires(&started, &closed, new_minute, &pcs)) {
                tracing::info!("[PROTOCOL] {}: the {name} protocol", when.words());
                crate::hud_notify::record(&format!("{} → {}", capitalised(&when.words()), crate::protocols::display_name(&name)), false);
                let tools = tools.clone();
                tokio::spawn(async move {
                    let _ = tools.execute("manage_stark_protocol", &json!({ "action": "execute_protocol", "protocol_name": name })).await;
                });
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain(text: &str) -> String {
        crate::ear::words(text).join(" ")
    }

    #[test]
    fn notes_are_taken_shown_searched_and_deleted() {
        let _one = crate::kit::test_lock();
        let said = "Jay, note: Maths test on Friday, chapters 3 and 4.";
        assert_eq!(notebook_command(&plain(said), said), Some(json!({"action": "add", "text": "Maths test on Friday, chapters 3 and 4."})), "the note keeps its capitals and commas");
        for (said, text) in [("Take a note that the bike needs air", "the bike needs air"), ("write down buy milk", "buy milk"), ("Notiz: Zahnarzt anrufen", "Zahnarzt anrufen")] {
            assert_eq!(notebook_command(&plain(said), said), Some(json!({"action": "add", "text": text})), "{said}");
        }
        // What he is to remember for good is the older order: his long-term memory, not a note.
        for said in ["Notiere dir, dass Oma am Sonntag kommt", "Note that my locker code is 4512"] {
            assert!(crate::intents::is_memory_command(said), "{said}");
            assert_eq!(notebook_command(&plain(said), said), None, "{said}");
        }
        assert_eq!(notebook_command("show my notes", "x"), Some(json!({"action": "show"})));
        assert_eq!(notebook_command("what did i note about the test", "x"), Some(json!({"action": "show", "query": "the test"})));
        assert_eq!(notebook_command("delete the last note", "x"), Some(json!({"action": "delete_last"})));
        for said in ["note that down", "what is a note", "play a high note", "I noted nothing"] {
            assert_eq!(notebook_command(&plain(said), said), None, "{said}");
        }
        // Kept, found by their words, and gone on his word.
        assert!(notebook(&json!({"action": "show"})).unwrap().contains("empty"));
        notebook(&json!({"action": "add", "text": "Maths test on Friday"})).unwrap();
        notebook(&json!({"action": "add", "text": "Buy milk"})).unwrap();
        assert_eq!(notes_text(None).1, 2);
        let (found, n) = notes_text(Some("maths friday"));
        assert!(n == 1 && found.contains("Maths test on Friday"), "{found}");
        assert!(notes_text(None).0.find("Buy milk") < notes_text(None).0.find("Maths test"), "the newest first");
        assert!(notebook(&json!({"action": "delete_last"})).unwrap().contains("Buy milk"));
        assert!(notebook(&json!({"action": "clear"})).unwrap().contains("1 notes deleted"));
        assert!(notebook(&json!({"action": "delete_last"})).is_err());
        crate::kit::close_text(NOTEBOOK);
    }

    #[test]
    fn birthdays_are_kept_by_voice_and_noted_the_evening_before() {
        let set = |m: u32, d: u32, y: Option<i32>, name: &str, german: bool| Some(json!({"action": "set", "name": name, "month": m, "day": d, "year": y, "german": german}));
        for (said, want) in [
            ("Max's birthday is on 3 March", set(3, 3, None, "Max", false)),
            ("Remember that Max Mustermann's birthday is on March 3rd, 2008.", set(3, 3, Some(2008), "Max Mustermann", false)),
            ("Anna has her birthday on the 29th of February", set(2, 29, None, "Anna", false)),
            ("Thomas’ birthday is 12.10.", set(10, 12, None, "Thomas", false)),
            ("Max hat am 3. März Geburtstag", set(3, 3, None, "Max", true)),
            ("Der Geburtstag von Herr Müller ist am 03.03.1970", set(3, 3, Some(1970), "Herr Müller", true)),
            ("Pauls Geburtstag ist am 1. Mai", set(5, 1, None, "Pauls", true)),
        ] {
            assert_eq!(birthday_command(said), want, "{said}");
        }
        for (said, name) in [("When is Max's birthday?", "Max"), ("when does Anna have her birthday", "Anna"), ("Wann hat Paul Geburtstag?", "Paul"), ("wann ist Thomas' Geburtstag", "Thomas")] {
            assert_eq!(birthday_command(said), Some(json!({"action": "when", "name": name})), "{said}");
        }
        for not in ["my birthday is on 3 March", "his birthday is on 3 March", "Max's birthday is on Friday", "Max's birthday is on 31 February", "when is the birthday party", "the birthday cake is in the fridge", "wann hat die Bäckerei geöffnet"] {
            assert_eq!(birthday_command(not), None, "{not}");
        }
        let day = |y: i32, m: u32, d: u32| chrono::NaiveDate::from_ymd_opt(y, m, d).unwrap();
        assert_eq!(next_birthday(day(2026, 11, 16), 3, 3), day(2027, 3, 3), "past this year: next year");
        assert_eq!(next_birthday(day(2026, 3, 3), 3, 3), day(2026, 3, 3), "today is today");
        assert_eq!(next_birthday(day(2026, 11, 16), 2, 29), day(2027, 2, 28), "29 February outside a leap year");
        assert_eq!(next_birthday(day(2027, 11, 16), 2, 29), day(2028, 2, 29));
        assert_eq!(birthday_when(day(2026, 11, 16), "Max", 3, 3, Some(2008)), "Max's birthday is on 3 March, Sir. That's in 107 days: Max turns 19.");
        assert_eq!(birthday_when(day(2026, 3, 2), "Anna", 3, 3, None), "Anna's birthday is on 3 March, Sir. That's tomorrow.");

        let at = |d: u32, h: u32| day(2026, 3, d).and_hms_opt(h, 0, 0).unwrap();
        let people = [("Max".to_string(), 3, 3, Some(2008)), ("Anna".to_string(), 3, 4, None)];
        let calendar = [(day(2026, 3, 3), "Geburtstag Oma".to_string()), (day(2026, 3, 3), "Max's Birthday".to_string()), (day(2026, 3, 3), "Dentist".to_string())];
        let (key, title, body, spoken) = birthday_note(at(2, 20), &people, &calendar).expect("the evening before");
        assert_eq!((key.as_str(), title.as_str()), ("2026-03-03|tomorrow", "BIRTHDAY // TOMORROW"));
        assert_eq!(body, "Max, who turns 18\nGeburtstag Oma", "a calendar birthday of a kept name is not doubled; other events are not birthdays");
        assert_eq!(spoken, "Birthdays tomorrow, Sir: Max, who turns 18 and Geburtstag Oma.");
        assert_eq!(birthday_note(at(3, 8), &people, &[]).map(|n| n.3), Some("A birthday today, Sir: Max, who turns 18.".to_string()));
        assert_eq!(birthday_note(at(3, 20), &people, &[]).map(|n| n.0), Some("2026-03-04|tomorrow".to_string()), "Anna's is the next evening's");
        for quiet in [at(2, 18), at(2, 23), at(3, 6), at(3, 12), at(10, 20)] {
            assert_eq!(birthday_note(quiet, &people, &calendar), None, "{quiet}");
        }
    }

    #[test]
    fn where_he_put_a_thing_is_kept_and_told() {
        let _one = crate::kit::test_lock();
        let heard = |said: &str| place_command(&crate::intents::normalize_phrase(said));
        let said = |said: &str| heard(said).and_then(|a| a["said"].as_str().map(str::to_string));
        assert_eq!(said("My keys are in the top drawer.").as_deref(), Some("your keys are in the top drawer"));
        assert_eq!(said("I put the charger behind the TV").as_deref(), Some("your charger is behind the tv"));
        assert_eq!(said("Ich habe den Schlüssel in die Schublade gelegt").as_deref(), Some("schlüssel liegt in der schublade"));
        assert_eq!(said("Meine Brille liegt auf dem Schreibtisch").as_deref(), Some("brille liegt auf dem schreibtisch"));
        assert_eq!(said("Mein Handy liegt im Auto").as_deref(), Some("handy liegt im auto"));
        // Not a thing put somewhere: a mode, a state, a name, a question.
        for said in ["my phone is on silent", "my pc is in game mode", "my name is in the credits", "my next lesson is in room 3", "what is in my notes", "where is the iss", "the weather is in the news"] {
            assert_eq!(heard(said), None, "{said}");
        }
        // "Where did I put …" is always his; "where is my …" only once he told J.A.I.
        assert_eq!(heard("where is my wallet"), None);
        assert!(place(&json!({"action": "where", "thing": "wallet"})).unwrap().contains("haven't told me"));
        for said in ["My keys are in the top drawer.", "Ich habe meinen Ladekabel in die Tasche gelegt"] {
            place(&heard(said).unwrap()).unwrap();
        }
        for (asked, answer) in [("Where did I put my keys?", "your keys are in the top drawer"), ("where are my key", "your keys are in the top drawer"), ("Wo ist mein Ladekabel?", "ladekabel liegt in der tasche"), ("wo habe ich mein ladekabel hingelegt", "ladekabel liegt in der tasche")] {
            let args = heard(asked).unwrap_or_else(|| panic!("{asked}"));
            let told = place(&args).unwrap();
            assert!(told.contains(answer) && told.contains("today at"), "{asked}: {told}");
        }
        // The newest place wins; the day is said as he would.
        place(&heard("my keys are on the kitchen table").unwrap()).unwrap();
        assert!(place(&json!({"action": "where", "thing": "keys"})).unwrap().contains("kitchen table"));
        let now = chrono::NaiveDate::from_ymd_opt(2026, 11, 16).unwrap().and_hms_opt(9, 0, 0).unwrap();
        assert_eq!(when_said("2026-11-15 18:04:00.000", now), "yesterday at 18:04");
        assert_eq!(when_said("2026-11-12 07:00:00.000", now), "on Thursday");
        assert_eq!(when_said("2026-10-03 07:00:00.000", now), "on 3 October");
    }

    #[test]
    fn the_sleep_timer_is_set_asked_about_and_cancelled() {
        for (said, minutes, what) in [
            ("stop the music in 30 minutes", 30, "music"),
            ("turn off everything in an hour", 60, "everything"),
            ("sleep timer 20 minutes", 20, "music"),
            ("set a sleep timer for half an hour", 30, "music"),
            ("everything off in 10 minutes", 10, "everything"),
            ("mach die musik in 15 minuten aus", 15, "music"),
        ] {
            assert_eq!(sleep_command(said), Some(json!({"action": "set", "minutes": minutes, "what": what})), "{said}");
        }
        assert_eq!(sleep_command("cancel the sleep timer"), Some(json!({"action": "cancel"})));
        assert_eq!(sleep_command("how long is left on the sleep timer"), Some(json!({"action": "status"})));
        for said in ["stop the music", "stop the music in the kitchen", "sleep", "set a timer for 20 minutes"] {
            assert_eq!(sleep_command(said), None, "{said}");
        }
        // Stopping the music at once stays what it was.
        assert!(crate::intents::is_media_stop("stop the music") && !crate::intents::is_media_stop("stop the music in 30 minutes"));
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        rt.block_on(async {
            *lock(&SLEEPING) = None;
            assert!(sleep_timer(&json!({"action": "status"})).unwrap().contains("No sleep timer"));
            assert!(sleep_timer(&json!({"action": "set", "minutes": 30, "what": "music"})).unwrap().contains("in 30 minutes"));
            assert_eq!(sleep_left().as_deref(), Some("music off in 30 min"));
            assert!(sleep_timer(&json!({"action": "cancel"})).unwrap().contains("cancelled"));
            assert_eq!(sleep_left(), None);
        });
        assert_eq!((minutes_words(1), minutes_words(60), minutes_words(120), minutes_words(45)), ("a minute".to_string(), "an hour".to_string(), "2 hours".to_string(), "45 minutes".to_string()));
    }

    /// Sir's PCs and his friends' (2026-11-21: "when a friend of mine on his pc does something"):
    /// what they do in a rule and in a protocol's WHEN line, and this PC's own game.
    #[test]
    fn what_another_pc_does_is_a_rule_and_a_protocols_when() {
        let pc = |who: &str, what: PcEvent| When::Pc(who.into(), what);
        for (line, when) in [
            ("Max comes online", pc("max", PcEvent::Online)),
            ("Whenever Max's PC goes offline", pc("maxs", PcEvent::Offline)),
            ("max starts a game", pc("max", PcEvent::Plays(String::new()))),
            ("Max plays Rocket League", pc("max", PcEvent::Plays("rocket league".into()))),
            ("max starts minecraft", pc("max", PcEvent::Plays("minecraft".into()))),
            ("Max stops playing", pc("max", PcEvent::Stops(String::new()))),
            ("I start a game", pc("i", PcEvent::Plays(String::new()))),
            ("Minecraft starts", When::Starts("minecraft".into())),
            ("when Discord closes", When::Closes("discord".into())),
            ("every day at 7:30", When::At(7, 30)),
        ] {
            assert_eq!(when_of(line), Some(when), "{line}");
        }
        for line in ["", "max", "what a nice day", "i feel like it"] {
            assert_eq!(when_of(line), None, "{line}");
        }
        // In a rule the order follows.
        assert_eq!(parse_rule("whenever max comes online play my playlist"), Some((pc("max", PcEvent::Online), "play my playlist".into())));
        assert_eq!(parse_rule("whenever max starts a game then mute the music"), Some((pc("max", PcEvent::Plays(String::new())), "mute the music".into())));
        assert_eq!(parse_rule("when i start a game open discord"), Some((pc("i", PcEvent::Plays(String::new())), "open discord".into())));
        assert_eq!(parse_rule("when i open spotify set the volume to 30"), Some((When::Starts("spotify".into()), "set the volume to 30".into())), "his own program stays the app rule");
        // Fired by what the PC did: its name as said ("maxs": Max's), any game or the one named.
        let heard = [("max".to_string(), PcEvent::Plays("Minecraft: Java Edition".into()))];
        assert!(pc("maxs", PcEvent::Plays(String::new())).fires(&[], &[], None, &heard));
        assert!(pc("max", PcEvent::Plays("minecraft".into())).fires(&[], &[], None, &heard));
        assert!(!pc("max", PcEvent::Plays("fortnite".into())).fires(&[], &[], None, &heard));
        assert!(!pc("erika", PcEvent::Plays(String::new())).fires(&[], &[], None, &heard));
        assert!(!pc("max", PcEvent::Online).fires(&[], &[], None, &heard));
        // Kept and read back as a rule, and said in words.
        let when = pc("max", PcEvent::Plays("minecraft".into()));
        assert_eq!(When::of(&when.to_json()), Some(when.clone()));
        assert_eq!(when.words(), "whenever Max plays minecraft");
        assert_eq!(pc("i", PcEvent::Plays(String::new())).words(), "whenever I start a game");
    }

    #[test]
    fn a_rule_says_when_and_what_and_fires_on_the_change() {
        let _one = crate::kit::test_lock();
        assert_eq!(parse_rule("whenever discord starts mute the music"), Some((When::Starts("discord".into()), "mute the music".into())));
        assert_eq!(parse_rule("when i open spotify set the volume to 30"), Some((When::Starts("spotify".into()), "set the volume to 30".into())));
        assert_eq!(parse_rule("every time steam closes then open the mixer"), Some((When::Closes("steam".into()), "open the mixer".into())));
        assert_eq!(parse_rule("every day at 7 30 open my calendar"), Some((When::At(7, 30), "open my calendar".into())));
        assert_eq!(parse_rule("every day at 9 pm night mode"), Some((When::At(21, 0), "night mode".into())));
        assert_eq!(parse_rule("when is my next lesson"), None);
        assert_eq!(parse_rule("whenever i feel like it"), None);
        // Only an order he knows makes a rule.
        assert_eq!(rule_command("whenever discord starts open the mixer"), Some(json!({"action": "add", "when": {"kind": "starts", "app": "discord"}, "then": "open the mixer"})));
        assert_eq!(rule_command("whenever discord closes i feel a little lonely"), None);
        assert_eq!(rule_command("list my rules"), Some(json!({"action": "list"})));
        assert_eq!(rule_command("delete rule 2"), Some(json!({"action": "remove", "number": 2})));
        assert_eq!(rule_command("delete the rule about discord"), Some(json!({"action": "remove", "about": "discord"})));
        // Kept, fired by what starts and closes and by the clock, and removed.
        rule(&json!({"action": "add", "when": {"kind": "starts", "app": "discord"}, "then": "open the mixer"})).unwrap();
        rule(&json!({"action": "add", "when": {"kind": "closes", "app": "spotify"}, "then": "night mode"})).unwrap();
        rule(&json!({"action": "add", "when": {"kind": "at", "hour": 7, "minute": 30}, "then": "open my calendar"})).unwrap();
        assert!(rule(&json!({"action": "add", "when": {"kind": "starts", "app": "discord"}, "then": "open the mixer"})).unwrap().contains("already"));
        assert_eq!(fired(&["discord".into()], &[], None, &[]), [("whenever discord starts".to_string(), "open the mixer".to_string())]);
        assert_eq!(fired(&["notepad".into()], &["spotify".into()], None, &[]).len(), 1);
        assert_eq!(fired(&[], &[], Some((7, 30)), &[]), [("every day at 07:30".to_string(), "open my calendar".to_string())]);
        assert!(fired(&[], &[], Some((7, 31)), &[]).is_empty() && fired(&["discordptb".into()], &[], None, &[]).len() == 1);
        assert!(is_app("minecraft", "javaw") && is_app("Discord", "discord") && !is_app("discord", "disk"));
        assert!(rule(&json!({"action": "list"})).unwrap().contains("1.  Whenever discord starts"));
        assert!(rule(&json!({"action": "remove", "number": 1})).is_ok());
        assert!(rule(&json!({"action": "remove", "about": "calendar"})).is_ok());
        assert_eq!(rules().len(), 1);
        assert!(rule(&json!({"action": "remove", "number": 9})).is_err());
        assert!(rule(&json!({"action": "remove", "all": true})).unwrap().contains("1 rule"));
        crate::kit::close_text(RULES);
    }
}
