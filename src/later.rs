//! Orders with a delay (Sir, 2026-11-13: "Play music in one minute" was taken for a song called
//! "music in one minute", the library was searched and "One" played).
//!
//! "Play music in one minute", "close the board at 18:30", "in ten minutes show the briefing",
//! "spiel in einer Minute Musik", "um 18:30 zeig mir das Briefing": an order he knows and when
//! it is to be carried out. The sentence is read for its time clause (`clause`: first or last
//! in English, also after the verb in German; a sober grammar, nothing that could mean two
//! times), and it is a delayed order only when
//!
//! * without the clause it is an order the fast path reads (`brain::fast_as_said`,
//!   `brain::another_way`, or several of them in one sentence), and
//! * as it stands it is not understood, or only by a rule that took the clause's words for
//!   part of a name (a song called "music in one minute") or read the sentence as if the
//!   clause were not there.
//!
//! What is understood as a whole stays what it was: timers, reminders, alarms, the sleep timer
//! ("stop the music in 20 minutes"), the PC's power dialogue, calendar entries, a note that
//! holds a time, and every question.
//!
//! The orders wait in the kit's store, so they survive a restart; one that fell due while he
//! was off is not run late: it is dropped, and he says so once at the next start (`load`).
//! When one falls due the app carries it out as if Sir had just said it (`fall_due`,
//! `Jai::run_put_off`). "What is scheduled", "cancel that" right after, "cancel the second
//! one", "cancel the scheduled music", "cancel everything scheduled" (`command`,
//! `cancel_just_made`); `planned` is for the HUD.
//!
//! Only orders the fast path reads can be put off: one that needs the language model would be
//! guessed at again, minutes later and with nobody there to be asked.

use crate::brain::Fast;
use crate::intents::Intent;
use crate::kit::say;
use chrono::{DateTime, Duration as Span, Local, NaiveTime, TimeZone};
use regex::Regex;
use serde_json::{json, Value};
use std::cell::Cell;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn re(pattern: &str) -> Regex {
    Regex::new(pattern).expect("a valid phrase")
}

/// The kit's tool that keeps, lists and drops them.
pub const TOOL: &str = "scheduled_order";
/// Their kind in the kit's store, and their block on the board.
const KIND: &str = "later";
pub const BLOCK: &str = "kit_later";
/// How many may wait at once, and how far ahead one may lie.
const MOST: usize = 20;
const FARTHEST: i64 = 7 * 86_400;

// ---------------------------------------------------------------------- the time clause ----

/// When an order is to be carried out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum When {
    /// So many seconds after it is given.
    In(i64),
    /// The next time the clock shows this. `either`: said without its half of the day ("at
    /// half past six"), so the morning's or the evening's, whichever comes first. `tomorrow`:
    /// said for tomorrow.
    At { hour: u32, minute: u32, either: bool, tomorrow: bool },
}

/// Whose words a clause is in, where they say ("in einer Minute", "at 7 pm"); digits and
/// "minute" alone are anybody's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tongue {
    English,
    German,
    Either,
}

/// A sentence with a time clause: when, and the sentence without the clause as it was said.
#[derive(Debug, Clone, PartialEq)]
pub struct Clause {
    pub when: When,
    pub rest: String,
    /// The clause's own words, as words are compared (small letters, no punctuation).
    words: Vec<String>,
    /// It stood first ("in ten minutes show the briefing"): everything after it is put off.
    leads: bool,
}

const NUMBER: &str = r"\d{1,3}|a|an|one|two|three|four|five|six|seven|eight|nine|ten|eleven|twelve|fifteen|twenty five|twenty|thirty|forty five|forty|fifty|sixty|ninety|ein|eine|einer|einem|einen|zwei|drei|vier|fünf|sechs|sieben|acht|neun|zehn|elf|zwölf|fünfzehn|fünfundzwanzig|zwanzig|dreißig|fünfundvierzig|vierzig|fünfzig|sechzig|neunzig";
const UNIT: &str = r"seconds?|secs?|minutes?|mins?|hours?|hrs?|sekunden?|minuten?|stunden?";
const HOURS_EN: &str = "one|two|three|four|five|six|seven|eight|nine|ten|eleven|twelve";
const HOURS_DE: &str = "eins|ein|zwei|drei|vier|fünf|sechs|sieben|acht|neun|zehn|elf|zwölf";

/// "in one minute", "in 90 seconds", "in an hour and a half", "in two and a half hours", "in 2
/// hours and 10 minutes", "in half an hour", "in einer halben Stunde", "in anderthalb Stunden".
static SPAN: LazyLock<Regex> = LazyLock::new(|| {
    re(&format!(
        r"^in (?:(?P<half>half an hour|a half hour|einer halben stunde)|(?P<quarter>a quarter of an hour|a quarter hour|einer viertelstunde|einer viertel stunde)|(?P<three>three quarters of an hour|einer dreiviertelstunde)|(?P<onehalf>anderthalb|eineinhalb) (?P<u0>stunden|minuten)|(?P<nh>{NUMBER}) and a half (?P<uh>minutes|hours)|(?P<ne>zwei|drei|vier|fünf)einhalb (?P<ue>stunden|minuten)|(?P<n>{NUMBER}) (?P<u>{UNIT})(?: (?:and|und) (?:(?P<ahalf>a half)|(?P<n2>{NUMBER}) (?P<u2>seconds?|secs?|minutes?|mins?|sekunden?|minuten?)))?)$"
    ))
});
/// "at 18:30", "at 7 pm", "at half past six", "at quarter to seven", "at six thirty", "at
/// noon", "tonight at 8", "at 8 tonight", "tomorrow at 7".
static AT_EN: LazyLock<Regex> = LazyLock::new(|| {
    const DAY: &str = "tonight|this evening|this afternoon|this morning|today|tomorrow morning|tomorrow evening|tomorrow afternoon|tomorrow";
    re(&format!(
        r"^(?:(?P<day>{DAY}) )?at (?:(?P<named>noon|midday|midnight)|(?P<frac>half past|a quarter past|quarter past|a quarter to|quarter to|five past|ten past|twenty past|twenty five past|five to|ten to|twenty to|twenty five to) (?P<fh>\d{{1,2}}|{HOURS_EN})|(?P<h>\d{{1,2}})(?:[:.](?P<m>\d{{2}})| (?P<m2>\d{{2}}))?|(?P<hw>{HOURS_EN})(?: (?P<mw>thirty|fifteen|forty five|ten|twenty|fifty|oh five))?)(?: ?(?P<ampm>am|pm|oclock))?(?: (?P<day2>{DAY}|in the morning|in the evening|in the afternoon))?$"
    ))
});
/// "um 18:30", "um 18 Uhr 30", "um halb sieben", "um viertel nach sechs", "heute Abend um 8",
/// "um 8 heute Abend", "morgen früh um 7".
static AT_DE: LazyLock<Regex> = LazyLock::new(|| {
    const DAY: &str = "heute abend|heute nachmittag|heute morgen|heute früh|heute mittag|heute|morgen früh|morgen abend|morgen nachmittag|morgen mittag|morgen";
    re(&format!(
        r"^(?:(?P<day>{DAY}) )?um (?:(?P<named>mitternacht)|(?P<frac>halb|viertel nach|viertel vor|fünf nach|zehn nach|zwanzig nach|fünf vor|zehn vor|zwanzig vor) (?P<fh>\d{{1,2}}|{HOURS_DE})|(?P<h>\d{{1,2}})(?:[:.](?P<m>\d{{2}}))?(?: uhr(?: (?P<m2>\d{{1,2}}))?)?|(?P<hw>{HOURS_DE})(?: uhr(?: (?P<mw>dreißig|fünfzehn|fünfundvierzig|zehn|zwanzig|fünf))?)?)(?: (?P<day2>{DAY}|abends|nachmittags|morgens|früh|mittags))?$"
    ))
});

fn number(word: &str) -> Option<i64> {
    if let Ok(n) = word.parse::<i64>() {
        return Some(n);
    }
    Some(match word {
        "a" | "an" | "one" | "ein" | "eine" | "einer" | "einem" | "einen" | "eins" => 1,
        "two" | "zwei" => 2,
        "three" | "drei" => 3,
        "four" | "vier" => 4,
        "five" | "fünf" | "oh five" => 5,
        "six" | "sechs" => 6,
        "seven" | "sieben" => 7,
        "eight" | "acht" => 8,
        "nine" | "neun" => 9,
        "ten" | "zehn" => 10,
        "eleven" | "elf" => 11,
        "twelve" | "zwölf" => 12,
        "fifteen" | "fünfzehn" => 15,
        "twenty" | "zwanzig" => 20,
        "twenty five" | "fünfundzwanzig" => 25,
        "thirty" | "dreißig" => 30,
        "forty" | "vierzig" => 40,
        "forty five" | "fünfundvierzig" => 45,
        "fifty" | "fünfzig" => 50,
        "sixty" | "sechzig" => 60,
        "ninety" | "neunzig" => 90,
        _ => return None,
    })
}

fn unit_secs(unit: &str) -> i64 {
    if unit.starts_with("sec") || unit.starts_with("sek") {
        1
    } else if unit.starts_with('h') || unit.starts_with("stunde") {
        3600
    } else {
        60
    }
}

/// The time a clause's words say, when they are a clause and nothing else.
fn when_of(words: &str) -> Option<(When, Tongue)> {
    let s = words.replace("a.m", "am").replace("p.m", "pm");
    let s = s.as_str();
    if let Some(c) = SPAN.captures(s) {
        let of = |name: &str| c.name(name).map(|m| m.as_str());
        let german_word = |w: &str| matches!(w, "ein" | "eine" | "einer" | "einem" | "einen" | "zwei" | "drei" | "vier" | "fünf" | "sechs" | "sieben" | "acht" | "neun" | "zehn" | "elf" | "zwölf" | "fünfzehn" | "fünfundzwanzig" | "zwanzig" | "dreißig" | "fünfundvierzig" | "vierzig" | "fünfzig" | "sechzig" | "neunzig");
        let german_unit = |u: &str| matches!(u, "sekunde" | "sekunden" | "minuten" | "stunde" | "stunden");
        let english_unit = |u: &str| !german_unit(u) && u != "minute";
        let (secs, tongue) = if let Some(half) = of("half") {
            (1800, if half.starts_with("einer") { Tongue::German } else { Tongue::English })
        } else if let Some(quarter) = of("quarter") {
            (900, if quarter.starts_with("einer") { Tongue::German } else { Tongue::English })
        } else if let Some(three) = of("three") {
            (2700, if three.starts_with("einer") { Tongue::German } else { Tongue::English })
        } else if of("onehalf").is_some() {
            (unit_secs(of("u0")?) * 3 / 2, Tongue::German)
        } else if let Some(n) = of("nh") {
            (number(n)? * unit_secs(of("uh")?) + unit_secs(of("uh")?) / 2, Tongue::English)
        } else if let Some(n) = of("ne") {
            (number(n)? * unit_secs(of("ue")?) + unit_secs(of("ue")?) / 2, Tongue::German)
        } else {
            let (n, unit) = (of("n")?, of("u")?);
            let mut secs = number(n)? * unit_secs(unit);
            if of("ahalf").is_some() {
                secs += unit_secs(unit) / 2;
            }
            if let (Some(n2), Some(u2)) = (of("n2"), of("u2")) {
                // ("In 10 minutes and 2 hours" is no way of saying a time.)
                if unit_secs(u2) >= unit_secs(unit) {
                    return None;
                }
                secs += number(n2)? * unit_secs(u2);
            }
            let tongue = if german_word(n) || german_unit(unit) || s.contains(" und ") {
                Tongue::German
            } else if english_unit(unit) || n.chars().any(char::is_alphabetic) {
                Tongue::English
            } else {
                Tongue::Either
            };
            (secs, tongue)
        };
        return (1..=FARTHEST).contains(&secs).then_some((When::In(secs), tongue));
    }
    let (c, german) = match AT_EN.captures(s) {
        Some(c) => (c, false),
        None => (AT_DE.captures(s)?, true),
    };
    let of = |name: &str| c.name(name).map(|m| m.as_str());
    // The day and its half, said before the time or after it ("heute morgen" is this morning,
    // "morgen" and "morgen früh" are tomorrow).
    let days = [of("day").unwrap_or(""), of("day2").unwrap_or("")];
    let says = |words: &[&str]| days.iter().any(|day| day.split(' ').any(|d| words.contains(&d)));
    let tomorrow = days.iter().any(|day| day.starts_with("tomorrow") || *day == "morgen" || day.starts_with("morgen "));
    let evening = says(&["tonight", "evening", "afternoon", "abend", "nachmittag", "abends", "nachmittags"]) || of("ampm") == Some("pm");
    let morning = says(&["morning", "früh", "morgens"]) || days.contains(&"heute morgen") || of("ampm") == Some("am");
    let midday = says(&["mittag", "mittags"]);
    // The hour and the minute as they were said, on whichever clock.
    let mut written_24 = false;
    let (mut hour, minute): (i64, i64) = if let Some(named) = of("named") {
        written_24 = true;
        (if matches!(named, "noon" | "midday") { 12 } else { 0 }, 0)
    } else if let Some(frac) = of("frac") {
        let h = number(of("fh")?)?;
        match frac {
            "half past" => (h, 30),
            "halb" => (h - 1, 30),
            "a quarter past" | "quarter past" | "viertel nach" => (h, 15),
            "a quarter to" | "quarter to" | "viertel vor" => (h - 1, 45),
            other => {
                let (n, past) = other.rsplit_once(' ').map(|(n, word)| (number(n), matches!(word, "past" | "nach")))?;
                if past { (h, n?) } else { (h - 1, 60 - n?) }
            }
        }
    } else if let Some(h) = of("h") {
        // ("07:30" is the morning's, as it is written.)
        written_24 = h.len() == 2 && h.starts_with('0');
        (h.parse().ok()?, of("m").or(of("m2")).map_or(Some(0), |m| m.parse().ok())?)
    } else {
        (number(of("hw")?)?, of("mw").map_or(Some(0), number)?)
    };
    if !(0..=59).contains(&minute) || !(-1..=23).contains(&hour) {
        return None;
    }
    // ("Quarter to one" on the twelve-hour face is a quarter to after twelve.)
    if hour <= 0 && of("frac").is_some() {
        hour += 12;
    }
    let face = (1..=12).contains(&hour) && !written_24;
    if face && evening && hour < 12 {
        hour += 12;
    } else if face && morning && hour == 12 {
        hour = 0;
    } else if face && midday && hour <= 5 {
        hour += 12;
    } else if face && says(&["tonight"]) && hour == 12 {
        hour = 0;
    }
    // Said without its half of the day: the next time the clock shows it. For tomorrow that
    // would be anybody's guess; it is taken as it is written, the morning's.
    let either = face && !evening && !morning && !midday && !tomorrow;
    let when = When::At { hour: hour.clamp(0, 23) as u32, minute: minute as u32, either, tomorrow };
    Some((when, if german { Tongue::German } else { Tongue::English }))
}

impl When {
    /// When that is, from `now`.
    pub fn due(self, now: DateTime<Local>) -> Option<DateTime<Local>> {
        match self {
            When::In(secs) => Some(now + Span::seconds(secs.clamp(1, FARTHEST))),
            When::At { hour, minute, either, tomorrow } => {
                let hours = if either { vec![hour % 12, hour % 12 + 12] } else { vec![hour] };
                let first = now.date_naive() + Span::days(i64::from(tomorrow));
                hours
                    .into_iter()
                    .filter_map(|h| {
                        let time = NaiveTime::from_hms_opt(h, minute, 0)?;
                        let on = |day: chrono::NaiveDate| Local.from_local_datetime(&day.and_time(time)).earliest();
                        on(first).filter(|due| *due > now).or_else(|| on(first + Span::days(1)))
                    })
                    .min()
            }
        }
    }

    fn args(self, order: &str) -> Value {
        match self {
            When::In(secs) => json!({"action": "set", "order": order, "in_s": secs}),
            When::At { hour, minute, either, tomorrow } => json!({"action": "set", "order": order, "at": format!("{hour:02}:{minute:02}"), "either": either, "tomorrow": tomorrow}),
        }
    }

    fn of_args(args: &Value) -> Option<When> {
        if let Some(secs) = args["in_s"].as_i64() {
            return (1..=FARTHEST).contains(&secs).then_some(When::In(secs));
        }
        let (hour, minute) = args["at"].as_str()?.split_once(':')?;
        let (hour, minute): (u32, u32) = (hour.parse().ok()?, minute.parse().ok()?);
        (hour < 24 && minute < 60).then_some(When::At { hour, minute, either: args["either"] == true, tomorrow: args["tomorrow"] == true })
    }
}

/// One word of a sentence: as it was said, and as it is compared.
struct Tok<'a> {
    raw: &'a str,
    plain: String,
}

fn toks(text: &str) -> Vec<Tok<'_>> {
    text.split_whitespace()
        .map(|raw| Tok { raw, plain: raw.to_lowercase().replace(['\'', '’'], "").trim_matches(|c: char| !c.is_alphanumeric()).replace('-', " ") })
        .filter(|t| !t.plain.is_empty())
        .collect()
}

/// Words a clause may begin with.
const BEGINS: &[&str] = &["in", "at", "um", "tonight", "today", "tomorrow", "this", "heute", "morgen"];
/// What may stand before the order and is no part of it, and what may follow it.
const ADDRESS: &[&str] = &["jai", "jay", "hey", "ok", "okay", "please", "bitte", "and", "und", "then", "dann", "so", "also", "now"];
const COURTESY: &[&str] = &["please", "bitte", "sir", "jai", "jay", "thanks", "danke"];
/// What a German sentence may put between its verb and the clause ("zeig mir um 18:30 …").
const AFTER_THE_VERB: &[&str] = &["mir", "mal", "uns", "dir", "bitte", "doch"];
/// The part of a German verb that stands last ("mach das Board in zehn Minuten zu").
const LAST_PART: &[&str] = &["aus", "an", "auf", "zu", "ab", "ein", "weg", "leiser", "lauter", "hoch", "runter", "stumm", "her"];
/// The longest a clause is, in words ("tomorrow morning at a quarter past six").
const LONGEST: usize = 8;

/// Whether a sentence holds a word a time clause begins with: asked of every sentence the fast
/// path reads, so it costs next to nothing (no word is copied).
fn may_hold_a_clause(text: &str) -> bool {
    text.split_whitespace().any(|word| {
        let word = word.trim_matches(|c: char| !c.is_alphanumeric());
        BEGINS.iter().any(|begin| word.eq_ignore_ascii_case(begin))
    })
}

/// The time clause of a sentence and what is left without it. None: it holds none, or more
/// than one, or the clause is all there is.
pub fn clause(text: &str) -> Option<Clause> {
    if !may_hold_a_clause(text) {
        return None;
    }
    let all = toks(text);
    if !(3..=28).contains(&all.len()) {
        return None;
    }
    let joined = |from: usize, to: usize| all[from..to].iter().map(|t| t.plain.as_str()).collect::<Vec<_>>().join(" ");
    let begins = |at: usize| BEGINS.contains(&all[at].plain.split(' ').next().unwrap_or(""));
    let first = all.iter().take_while(|t| ADDRESS.contains(&t.plain.as_str())).count();
    let last = all.len() - all.iter().rev().take_while(|t| COURTESY.contains(&t.plain.as_str())).count();
    if first + 3 > last {
        return None;
    }
    // (from, to, when, it leads, a last part of the verb behind it)
    let mut found: Vec<(usize, usize, When, bool)> = Vec::new();
    let mut take = |from: usize, to: usize, when: When, leads: bool| {
        if !found.iter().any(|(f, t, _, _)| *f == from && *t == to) {
            found.push((from, to, when, leads));
        }
    };
    // It stands first: "in ten minutes show the briefing" (the longest run of words that is one).
    if begins(first) {
        if let Some((to, when)) = (first + 1..=(first + LONGEST).min(last - 1)).rev().find_map(|to| when_of(&joined(first, to)).map(|(when, _)| (to, when))) {
            take(first, to, when, true);
        }
    }
    // It stands last: "play Back in Black in ten minutes" (again the longest).
    if let Some((from, when)) = (last.saturating_sub(LONGEST).max(first + 1)..last).filter(|from| begins(*from)).find_map(|from| when_of(&joined(from, last)).map(|(when, _)| (from, when))) {
        take(from, last, when, false);
    }
    // German puts it after the verb ("spiel in einer Minute Musik", "zeig mir um 18:30 das
    // Briefing") or before the part of the verb that stands last ("mach es in zehn Minuten zu").
    let after_verb = first + 1 + usize::from(all.get(first + 1).is_some_and(|t| AFTER_THE_VERB.contains(&t.plain.as_str())));
    if after_verb < last && begins(after_verb) {
        if let Some((to, when)) = (after_verb + 1..=(after_verb + LONGEST).min(last - 1)).rev().find_map(|to| when_of(&joined(after_verb, to)).filter(|(_, tongue)| *tongue != Tongue::English).map(|(when, _)| (to, when))) {
            take(after_verb, to, when, false);
        }
    }
    if LAST_PART.contains(&all[last - 1].plain.as_str()) {
        let end = last - 1;
        if let Some((from, when)) = (end.saturating_sub(LONGEST).max(first + 1)..end).filter(|from| begins(*from)).find_map(|from| when_of(&joined(from, end)).filter(|(_, tongue)| *tongue != Tongue::English).map(|(when, _)| (from, when))) {
            take(from, end, when, false);
        }
    }
    // One clause, or none: two times in one sentence mean nothing certain.
    let [(from, to, when, leads)] = found.as_slice() else { return None };
    let (from, to, when, leads) = (*from, *to, *when, *leads);
    // The order as it was said: without the clause, the address in front and the courtesies
    // behind it ("JAI, play music in one minute, please" is "play music").
    let kept: Vec<&str> = all.iter().enumerate().filter(|(at, _)| (*at < from || *at >= to) && *at >= first && *at < last).map(|(_, t)| t.raw).collect();
    let rest = kept.join(" ");
    let rest = rest.trim_matches(|c: char| c.is_whitespace() || matches!(c, ',' | ';' | ':' | '.' | '!' | '?' | '\u{2014}' | '-')).to_string();
    if rest.is_empty() || clause_in(&rest) {
        return None;
    }
    Some(Clause { when, rest, words: crate::ear::words(&joined(from, to)), leads })
}

/// Whether what is left of a sentence holds a time clause of its own.
fn clause_in(rest: &str) -> bool {
    let all = toks(rest);
    let joined = |from: usize, to: usize| all[from..to].iter().map(|t| t.plain.as_str()).collect::<Vec<_>>().join(" ");
    (0..all.len()).filter(|from| BEGINS.contains(&all[*from].plain.as_str())).any(|from| (from + 2..=(from + LONGEST).min(all.len())).any(|to| when_of(&joined(from, to)).is_some()))
}

// ----------------------------------------------------------------- is it a delayed order ----

thread_local! {
    /// Set while this thread asks the fast path what a sentence is without this module.
    static ASKING: Cell<bool> = const { Cell::new(false) };
}

/// The sentences of the last moments and what each was taken for: one turn asks several times
/// (who it is for, whether it is an order, the fast path itself).
static DECIDED: Mutex<Vec<(String, Option<Value>, Instant)>> = Mutex::new(Vec::new());

/// What the fast path makes of a sentence without this module.
#[derive(Debug, PartialEq)]
enum Reading {
    /// Nothing: it is the language model's.
    Nothing,
    /// It is understood, and not by a tool with arguments: an answer, a cancel, his ear, the
    /// privacy lock, a request about the selection.
    Whole,
    /// An order or a read: the tool and its arguments.
    Call(String, Value),
}

fn reading(text: &str) -> Reading {
    let of = |found: &Fast| match found {
        Fast::Intent(Intent::Tool { name, args }) => Reading::Call(name.to_string(), args.clone()),
        Fast::Intent(Intent::Silent(action)) => Reading::Call(action.tool.to_string(), action.args.clone()),
        Fast::Intent(Intent::Stop) => Reading::Call("(stop)".to_string(), json!({})),
        Fast::Route(route) => Reading::Call(route.tool.to_string(), route.args.clone()),
        Fast::Intent(Intent::Answer(_) | Intent::Time | Intent::Date) | Fast::Selection(_) | Fast::Other => Reading::Whole,
    };
    // (As the brain asks: said another way first, then as it stands.)
    match crate::brain::another_way(text) {
        Some((_, found)) => of(&found),
        None => crate::brain::fast_as_said(text).as_ref().map_or(Reading::Nothing, of),
    }
}

/// Tools that read a time as part of what they are asked: a sentence one of them reads is
/// understood as a whole, whatever its words are.
const TIME_IS_THEIRS: &[&str] = &[
    "reminders", "create_calendar_event", "modify_or_delete_calendar_event", "set_smart_morning_alarm", "sleep_timer", "whenever_rule", "focus_mode", "create_or_update_widget", "set_auto_responder",
    "manage_deep_research", "listening_lock", TOOL,
];

/// Arguments that hold a name: where a rule takes whatever words follow for the name of a
/// song, an app or a window, a time clause among them was swallowed, not meant. (A note, a
/// message or a search holds whatever Sir said, a time too: those are understood as they stand.)
const NAMES: &[(&str, &[&str])] = &[
    ("control_music", &["song_name", "playlist_name", "artist_name", "album_name"]),
    ("open_application", &["app_name"]),
    ("close_application", &["app_name"]),
    ("is_app_running", &["app_name"]),
    ("window_management", &["target_window"]),
    ("move_window_to_monitor", &["app_or_window_name"]),
    ("place_application_window", &["app_name"]),
    ("control_audio_mixer", &["app", "apps", "name"]),
    ("hud_board", &["target"]),
    ("arrange_jai_window", &["target", "other"]),
    ("remove_widget", &["id"]),
    ("close_native_window", &["id", "title"]),
    ("open_jai_view", &["view", "name"]),
    ("find_files", &["query"]),
    ("open_website", &["platform_or_url", "target_or_query"]),
    ("desk_layout", &["name"]),
    ("type_snippet", &["name"]),
    ("manage_stark_protocol", &["protocol_name"]),
    ("manage_voice_macro", &["name"]),
    ("configure_interface", &["theme", "load_theme", "hud_palette", "accent_color"]),
];

/// Whether a name among `args` holds the clause's words ("music in one minute").
fn swallowed(tool: &str, args: &Value, clause: &[String]) -> bool {
    let Some((_, keys)) = NAMES.iter().find(|(name, _)| *name == tool) else { return false };
    if clause.is_empty() {
        return false;
    }
    let holds = |value: &str| {
        let words = crate::ear::words(value);
        // (A song of his that is really called that is what he asked for.)
        words.windows(clause.len()).any(|window| window == clause) && !(tool == "control_music" && crate::music::quick_lookup(value).is_some())
    };
    keys.iter().any(|key| match &args[*key] {
        Value::String(value) => holds(value),
        Value::Array(values) => values.iter().filter_map(Value::as_str).any(holds),
        _ => false,
    })
}

/// Whether a sentence is understood as it stands, time and all. Not when nothing reads it,
/// when the only rule that does took the clause's words for part of a name, or when it is
/// read exactly as it would be without the clause (the clause went unheard).
fn understood_whole(text: &str, rest: &str, clause: &[String]) -> bool {
    match reading(text) {
        Reading::Nothing => false,
        Reading::Whole => true,
        Reading::Call(tool, args) => TIME_IS_THEIRS.contains(&tool.as_str()) || !(swallowed(&tool, &args, clause) || reading(rest) == Reading::Call(tool, args)),
    }
}

/// Whether this thread is in the middle of asking what a sentence is without this module
/// (`brain::another_way` keeps no memory of what it answers then).
pub(crate) fn asking() -> bool {
    ASKING.with(Cell::get)
}

/// The order a sentence puts off, as the arguments of `scheduled_order`: "play music in one
/// minute" is {"action": "set", "order": "play music", "in_s": 60}. None for every sentence
/// that is no order with a delay (see the module's head).
pub fn order(text: &str) -> Option<Value> {
    // (The routers this asks ask it in their turn; and what is being read another way is
    // read without it: `brain::another_way_knowing` asks it first, for the sentence itself.)
    if asking() || crate::brain::rewording() || !may_hold_a_clause(text) {
        return None;
    }
    let found = clause(text)?;
    if !cfg!(test) {
        if let Some((_, decided, _)) = lock(&DECIDED).iter().find(|(said, _, at)| said == text && at.elapsed() < Duration::from_secs(2)) {
            return decided.clone();
        }
    }
    ASKING.with(|asking| asking.set(true));
    let decided = decide(text, &found);
    ASKING.with(|asking| asking.set(false));
    if !cfg!(test) {
        let mut known = lock(&DECIDED);
        known.retain(|(said, _, at)| said != text && at.elapsed() < Duration::from_secs(2));
        known.push((text.to_string(), decided.clone(), Instant::now()));
        if known.len() > 6 {
            known.remove(0);
        }
    }
    decided
}

fn decide(text: &str, found: &Clause) -> Option<Value> {
    let rest = found.rest.as_str();
    // A plain question never changes anything, and the PC's power has its own dialogue, with
    // its double check ("shut down my PC in ten minutes" is asked about now, as it always was).
    if crate::intents::only_asks(text) || crate::system_power::power_request(text).is_some() || crate::system_power::power_request(rest).is_some() {
        return None;
    }
    let ctx = crate::intent_engine::ReflexContext::live();
    let clauses = crate::compound_planner::clauses(rest);
    if clauses.len() >= 2 {
        // Several orders. With the time first, all of them are put off ("in ten minutes close
        // the board and show the briefing"). With the time last it is said of the last one
        // alone ("open the mixer and play music in one minute"): the planner reads the
        // sentence clause by clause, and that clause comes back here on its own.
        if !found.leads {
            return None;
        }
        let all_orders = crate::compound_planner::plan(rest, &ctx).is_some_and(|plan| plan.privacy.is_none() && plan.calls.len() >= 2 && plan.calls.iter().all(|call| call.tool != TOOL));
        // (Its first order with the time in front may be something understood as a whole:
        // "in 20 minutes stop the music and close the board" sets the sleep timer.)
        let first_with_time = crate::compound_planner::clauses(text).into_iter().next().map(|(clause, _)| clause).unwrap_or_default();
        if !all_orders || understood_whole(&first_with_time, &clauses[0].0, &found.words) {
            return None;
        }
        return Some(found.when.args(rest));
    }
    // Without the clause it must be an order the fast path reads …
    if !matches!(reading(rest), Reading::Call(ref tool, _) if tool != TOOL) {
        return None;
    }
    // … and with it, nothing that is understood as it stands.
    if understood_whole(text, rest, &found.words) {
        return None;
    }
    Some(found.when.args(rest))
}

/// What a sentence asks of the orders that wait, as the arguments of `scheduled_order`: one
/// more to be put off, the one just put off dropped again, or their list and the orders that
/// drop them in so many words. The brain asks this before every other router (`run_turn`,
/// `fast_before_the_parser`): "what is scheduled" would otherwise be the calendar's, "cancel
/// that" the conversation's.
pub fn route(text: &str) -> Option<Value> {
    order(text).or_else(|| cancel_just_made(text)).or_else(|| command(&crate::intents::normalize_phrase(text)))
}

/// "Cancel that", "never mind", "vergiss es" right after an order was put off: that order is
/// what is dropped (the arguments that do it). None once a minute has passed, or when it has
/// been carried out or dropped already.
pub fn cancel_just_made(text: &str) -> Option<Value> {
    let still_waits = waiting(|w| w.last_set.as_ref().is_some_and(|(key, at)| at.elapsed() < JUST_NOW && w.planned.iter().any(|p| p.key == *key)));
    (still_waits && crate::intent_engine::is_cancel(text)).then(|| json!({"action": "cancel", "which": "last"}))
}

// ------------------------------------------------------------------------- the phrases ----

static LIST: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:what(?:s| is| are)? (?:scheduled|planned)|what (?:have you got|have you|do you have|have i got|do i have) (?:scheduled|planned)|what (?:have|did) i (?:scheduled|schedule|planned|plan)|(?:is|are) (?:there )?(?:anything|something|any orders?) (?:scheduled|planned)|(?:show|list|tell|read)(?: me)? (?:the |my |all |all the |all my )?(?:scheduled|planned|delayed) (?:orders|commands|things|tasks)|(?:show|tell)(?: me)? what(?:s| is) (?:scheduled|planned)|(?:scheduled|planned) (?:orders|commands)|was ist geplant|was hast du geplant|was habe ich geplant|ist (?:etwas|was|irgendwas) geplant|(?:zeig|zeige|liste|nenn|nenne)(?: mir)? (?:die |meine |alle )?geplanten (?:befehle|aufträge|sachen|aufgaben)|(?:zeig|zeige|sag)(?: mir)? was geplant ist|geplante (?:befehle|aufträge))$")
});
static CANCEL_ALL: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:cancel|drop|delete|remove|forget|clear|scrap|call off) (?:everything|all|all of it|anything)(?: that is| thats| you have| i have)? (?:scheduled|planned)$|^(?:cancel|drop|delete|remove|forget|clear|scrap|call off) (?:all |all the |all my |all of the |all of my |every )(?:scheduled |planned |delayed )(?:orders?|commands?|things|tasks)$|^(?:streich|streiche|lösch|lösche|vergiss|storniere) alles geplante$|^(?:streich|streiche|lösch|lösche|vergiss|storniere) alle geplanten (?:befehle|aufträge|sachen|aufgaben)$")
});
static CANCEL_NTH: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:cancel|drop|delete|remove|forget|scrap|call off) (?:the )?(?P<kind>scheduled |planned )?(?:order |command )?(?:number |nummer )?(?P<n>\d|one|two|three|four|five|first|second|third|fourth|fifth|1st|2nd|3rd|4th|5th|last)(?P<kind1> scheduled| planned)?(?: one| order| command)?$|^(?:streich|streiche|lösch|lösche|storniere) (?:den |die |das )?(?P<n2>ersten?|zweiten?|dritten?|vierten?|fünften?|letzten?|\d)(?P<kind2> geplanten?)?(?: befehl| auftrag)?$")
});
static CANCEL_ABOUT: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:cancel|drop|delete|remove|forget|scrap|call off) (?:the |my |that )?(?:scheduled|planned|delayed) (?P<what>.+)$|^(?:streich|streiche|lösch|lösche|vergiss|storniere) (?:den |die |das |meinen |meine |mein )?geplante[nrs]? (?P<was>.+)$")
});

/// "What is scheduled", "cancel everything scheduled", "cancel the second one", "cancel the
/// scheduled music". `s`: the fast path's normalised phrase.
pub fn command(s: &str) -> Option<Value> {
    if LIST.is_match(s) {
        return Some(json!({"action": "list"}));
    }
    if CANCEL_ALL.is_match(s) {
        return Some(json!({"action": "cancel", "which": "all"}));
    }
    if let Some(c) = CANCEL_NTH.captures(s) {
        // "The second one" is one of these only while their list is fresh in his mind (it
        // was just read out, or one was just put off); "the second scheduled order" always.
        let named = c.name("kind").or(c.name("kind1")).or(c.name("kind2")).is_some();
        if named || recently() {
            let n = c.name("n").or(c.name("n2"))?.as_str();
            return Some(match n {
                "last" | "letzte" | "letzten" => json!({"action": "cancel", "which": "last"}),
                "1" | "one" | "first" | "1st" | "erste" | "ersten" => json!({"action": "cancel", "number": 1}),
                "2" | "two" | "second" | "2nd" | "zweite" | "zweiten" => json!({"action": "cancel", "number": 2}),
                "3" | "three" | "third" | "3rd" | "dritte" | "dritten" => json!({"action": "cancel", "number": 3}),
                "4" | "four" | "fourth" | "4th" | "vierte" | "vierten" => json!({"action": "cancel", "number": 4}),
                "5" | "five" | "fifth" | "5th" | "fünfte" | "fünften" => json!({"action": "cancel", "number": 5}),
                other => json!({"action": "cancel", "number": other.parse::<u64>().ok()?}),
            });
        }
        return None;
    }
    let c = CANCEL_ABOUT.captures(s)?;
    let about = c.name("what").or(c.name("was"))?.as_str().trim();
    (!matches!(about, "order" | "orders" | "command" | "commands" | "one" | "befehl" | "auftrag")).then(|| json!({"action": "cancel", "about": about}))
}

// --------------------------------------------------------------------------- the store ----

#[derive(Debug, Clone, PartialEq)]
struct Planned {
    /// Its key in the kit's store: the moment it was put off, so the latest sorts last.
    key: String,
    due: DateTime<Local>,
    order: String,
}

/// What waits, as he holds it while he runs (the kit's store holds it across a restart).
struct Waiting {
    /// The orders that wait, the next one first.
    planned: Vec<Planned>,
    /// The one put off last (its key), and when: "cancel that" means it for a minute.
    last_set: Option<(String, Instant)>,
    /// When the list was last read out: "cancel the second one" means one of these for a while.
    listed: Option<Instant>,
}

#[cfg(not(test))]
static WAITING: Mutex<Waiting> = Mutex::new(Waiting { planned: Vec::new(), last_set: None, listed: None });
#[cfg(test)]
thread_local! {
    /// (A test has what waits to itself: other tests ask the fast path about "cancel that"
    /// at the same time.)
    static WAITING: std::cell::RefCell<Waiting> = const { std::cell::RefCell::new(Waiting { planned: Vec::new(), last_set: None, listed: None }) };
}

fn waiting<R>(f: impl FnOnce(&mut Waiting) -> R) -> R {
    #[cfg(not(test))]
    {
        f(&mut lock(&WAITING))
    }
    #[cfg(test)]
    {
        WAITING.with(|waiting| f(&mut waiting.borrow_mut()))
    }
}

const JUST_NOW: Duration = Duration::from_secs(60);

fn recently() -> bool {
    waiting(|w| w.listed.is_some_and(|at| at.elapsed() < Duration::from_secs(90)) || w.last_set.as_ref().is_some_and(|(_, at)| at.elapsed() < JUST_NOW))
}

/// What is planned, the next one first: when it is due, and the order as he said it (for the
/// HUD: the lines beside the emblem, the dashboard).
pub fn planned() -> Vec<(DateTime<Local>, String)> {
    waiting(|w| w.planned.iter().map(|p| (p.due, p.order.clone())).collect())
}

/// Reads what was put off before he was last closed (called once the kit knows its database;
/// a scripted session has none and starts with nothing planned). What fell due while he was
/// off is not carried out late: it is dropped, and he says so once, when the greeting is over.
pub fn load() {
    let missed = load_at(Local::now());
    if let Some(line) = missed_line(&missed) {
        tracing::info!("[LATER] {line}");
        let _ = std::thread::Builder::new().name("jai-later-missed".into()).spawn(move || {
            std::thread::sleep(Duration::from_secs(25));
            crate::hud_notify::record(&line, false);
            crate::announcements::say(line);
        });
    }
}

/// What is still ahead is kept; what fell due meanwhile is taken out of the store and returned.
fn load_at(now: DateTime<Local>) -> Vec<(DateTime<Local>, String)> {
    let mut ahead: Vec<Planned> = Vec::new();
    let mut missed: Vec<(DateTime<Local>, String)> = Vec::new();
    for (key, value, _) in crate::kit::list(KIND) {
        let entry = value["order"].as_str().zip(value["due_ms"].as_i64().and_then(|ms| Local.timestamp_millis_opt(ms).single()));
        match entry {
            Some((order, due)) if due > now => ahead.push(Planned { key, due, order: order.to_string() }),
            Some((order, due)) => {
                crate::kit::remove(KIND, &key);
                missed.push((due, order.to_string()));
            }
            None => {
                crate::kit::remove(KIND, &key);
            }
        }
    }
    ahead.sort_by_key(|p| p.due);
    missed.sort_by_key(|(due, _)| *due);
    waiting(|w| w.planned = ahead);
    missed
}

/// What he says of the orders that fell due while he was off.
fn missed_line(missed: &[(DateTime<Local>, String)]) -> Option<String> {
    let told = |(due, order): &(DateTime<Local>, String)| format!("\u{201C}{}\u{201D} at {}", order, due.format("%H:%M"));
    match missed {
        [] => None,
        [one] => Some(format!("While I was off, Sir, an order of yours fell due and was not carried out: {}.", told(one))),
        many => {
            let (last, rest) = many.split_last()?;
            // (More than three are counted, not read out.)
            if many.len() > 3 {
                return Some(format!("While I was off, Sir, {} orders of yours fell due and were not carried out; the first was {}.", many.len(), told(&many[0])));
            }
            Some(format!("While I was off, Sir, {} orders of yours fell due and were not carried out: {}, and {}.", many.len(), rest.iter().map(told).collect::<Vec<_>>().join(", "), told(last)))
        }
    }
}

/// The orders that are due now, in the order they fall due: taken out of the store, to be
/// carried out as if Sir had just said them (`Jai::run_put_off`).
pub fn fall_due(now: DateTime<Local>) -> Vec<String> {
    let due: Vec<Planned> = waiting(|w| {
        if w.planned.first().is_none_or(|next| next.due > now) {
            return Vec::new();
        }
        let (due, ahead): (Vec<Planned>, Vec<Planned>) = w.planned.drain(..).partition(|p| p.due <= now);
        w.planned = ahead;
        due
    });
    if due.is_empty() {
        return Vec::new();
    }
    for p in &due {
        crate::kit::remove(KIND, &p.key);
    }
    show_again();
    due.into_iter().map(|p| p.order).collect()
}

/// Whether an order that falls due may be carried out in this session. In a scripted session
/// only a silent one runs it, where every tool that reaches outside J.A.I. is noted and left
/// undone (`tools::stays_inside`); any other test session leaves it alone, as it leaves the
/// rules alone.
pub fn may_run_here() -> bool {
    !crate::platform::in_test_session() || crate::tools::silent_session()
}

/// Whether an order is carried out without a word (a badge is all its answer: "pause",
/// "close the board"): when one of these falls due he says a short word first, so that what
/// happens is no surprise.
pub fn runs_without_a_word(order: &str) -> bool {
    matches!(crate::brain::fast_as_said(order), Some(Fast::Intent(Intent::Silent(_) | Intent::Stop)))
}

// ---------------------------------------------------------------------------- the tool ----

/// "one minute", "10 minutes", "90 seconds", "an hour and a half", "2 hours and 10 minutes".
fn span_words(secs: i64) -> String {
    let unit = |n: i64, one: &str| if n == 1 { format!("one {one}") } else { format!("{n} {one}s") };
    match (secs / 3600, secs % 3600 / 60, secs % 60) {
        (0, 0, s) => unit(s, "second"),
        (0, m, 0) => unit(m, "minute"),
        (0, 1, _) => format!("{secs} seconds"),
        (0, m, s) => format!("{} and {}", unit(m, "minute"), unit(s, "second")),
        (h, 0, _) => unit(h, "hour"),
        (1, 30, _) => "an hour and a half".to_string(),
        (h, 30, _) => format!("{h} and a half hours"),
        (h, m, _) => format!("{} and {}", unit(h, "hour"), unit(m, "minute")),
    }
}

/// "18:30", "18:30 tomorrow", "07:00 on Friday": a time of day as it is told from `now`.
fn clock(due: DateTime<Local>, now: DateTime<Local>) -> String {
    match (due.date_naive() - now.date_naive()).num_days() {
        0 => due.format("%H:%M").to_string(),
        1 => format!("{} tomorrow", due.format("%H:%M")),
        _ => format!("{} on {}", due.format("%H:%M"), due.format("%A")),
    }
}

fn list_text(planned: &[Planned], now: DateTime<Local>) -> String {
    let lines: Vec<String> = planned.iter().enumerate().map(|(k, p)| format!("{}.  {}  \u{00B7}  {}", k + 1, clock(p.due, now), p.order)).collect();
    format!("{}\n\n\"Cancel the second one\" drops one of them, \"cancel everything scheduled\" all.", lines.join("\n"))
}

/// The block on the board follows what waits, while it is up.
fn show_again() {
    if !crate::kit::text_is_up(BLOCK) {
        return;
    }
    let planned = waiting(|w| w.planned.clone());
    if planned.is_empty() {
        crate::kit::close_text(BLOCK);
    } else {
        crate::kit::show_text(BLOCK, "Planned // orders put off", &list_text(&planned, Local::now()), [480.0, 300.0]);
    }
}

pub fn run(args: &Value) -> Result<String, String> {
    run_at(args, Local::now())
}

fn run_at(args: &Value, now: DateTime<Local>) -> Result<String, String> {
    match args["action"].as_str().unwrap_or("list") {
        "set" => {
            let order = args["order"].as_str().unwrap_or("").trim();
            if order.is_empty() {
                return Err("And what shall I do then, Sir?".to_string());
            }
            let when = When::of_args(args).ok_or("I did not catch when, Sir.")?;
            let due = when.due(now).ok_or("I did not catch when, Sir.")?;
            if waiting(|w| w.planned.len()) >= MOST {
                return Err(format!("{MOST} orders are waiting already, Sir, and that is as many as I keep. \"Cancel everything scheduled\" makes room."));
            }
            let key = crate::kit::stamp();
            crate::kit::put(KIND, &key, &json!({"order": order, "due_ms": due.timestamp_millis()}))?;
            waiting(|w| {
                w.planned.push(Planned { key: key.clone(), due, order: order.to_string() });
                w.planned.sort_by_key(|p| p.due);
                w.last_set = Some((key, Instant::now()));
            });
            tracing::info!("[LATER] '{order}' is put off until {}", due.format("%Y-%m-%d %H:%M:%S"));
            show_again();
            let spoken = match when {
                When::In(secs) => format!("In {}, Sir.", span_words(secs)),
                When::At { .. } => format!("At {}, Sir.", clock(due, now)),
            };
            Ok(say(format!("Planned for {}: {order}", clock(due, now)), spoken))
        }
        "cancel" => {
            let (nothing_waits, gone): (bool, Vec<Planned>) = waiting(|w| {
                let planned = &w.planned;
                let gone: Vec<Planned> = if args["which"] == "all" {
                    planned.clone()
                } else if args["which"] == "last" {
                    // (The one put off last, not the one that is due last.)
                    planned.iter().max_by(|a, b| a.key.cmp(&b.key)).cloned().into_iter().collect()
                } else if let Some(n) = args["number"].as_u64() {
                    planned.get((n as usize).wrapping_sub(1)).cloned().into_iter().collect()
                } else {
                    const NO_WORDS: &[&str] = &["the", "my", "a", "an", "that", "this", "order", "orders", "one", "den", "die", "das", "der", "befehl", "auftrag"];
                    let about: Vec<String> = crate::ear::words(args["about"].as_str().unwrap_or("")).into_iter().filter(|w| !NO_WORDS.contains(&w.as_str())).collect();
                    planned.iter().filter(|p| !about.is_empty() && { let words = crate::ear::words(&p.order); about.iter().all(|a| words.contains(a)) }).cloned().collect()
                };
                let nothing_waits = planned.is_empty();
                w.planned.retain(|p| !gone.iter().any(|g| g.key == p.key));
                (nothing_waits, gone)
            });
            if nothing_waits {
                return Err("Nothing is scheduled, Sir.".to_string());
            }
            if gone.is_empty() {
                return Err("I have no such order waiting, Sir. \"What is scheduled\" tells you the ones I have.".to_string());
            }
            for p in &gone {
                crate::kit::remove(KIND, &p.key);
            }
            show_again();
            tracing::info!("[LATER] Dropped: {}", gone.iter().map(|p| format!("'{}'", p.order)).collect::<Vec<_>>().join(", "));
            let told = |p: &Planned| format!("{} at {}", p.order, clock(p.due, now));
            Ok(match gone.as_slice() {
                [one] => say(format!("No longer planned: {}.", told(one)), format!("Cancelled, Sir: {}.", told(one))),
                many => say(format!("No longer planned: {}.", many.iter().map(told).collect::<Vec<_>>().join("; ")), if many.len() == 2 { "Both are cancelled, Sir.".to_string() } else { format!("All {} are cancelled, Sir.", many.len()) }),
            })
        }
        _ => {
            let planned = waiting(|w| w.planned.clone());
            if planned.is_empty() {
                return Ok(say("Nothing is scheduled. Say, for example: \"close the board at 18:30\".", "Nothing is scheduled, Sir."));
            }
            waiting(|w| w.listed = Some(Instant::now()));
            let text = list_text(&planned, now);
            let told = |p: &Planned| format!("{} at {}", p.order, clock(p.due, now));
            let spoken = match planned.as_slice() {
                [one] => format!("One order, Sir: {}.", told(one)),
                [first, second] => format!("Two, Sir: {}, and {}.", told(first), told(second)),
                [first, second, third] => format!("Three, Sir: {}, {}, and {}.", told(first), told(second), told(third)),
                many => {
                    crate::kit::show_text(BLOCK, "Planned // orders put off", &text, [480.0, 300.0]);
                    format!("{} orders are waiting, Sir; the next is {}. They are on the board.", many.len(), told(&many[0]))
                }
            };
            Ok(say(text, spoken))
        }
    }
}

#[cfg(test)]
pub(crate) fn forget_all() {
    waiting(|w| {
        w.planned.clear();
        w.last_set = None;
        w.listed = None;
    });
    crate::kit::clear(KIND);
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Timelike;

    fn at(hour: u32, minute: u32) -> DateTime<Local> {
        Local.with_ymd_and_hms(2026, 11, 13, hour, minute, 0).single().expect("a time of that day")
    }

    fn put_off(said: &str) -> Option<(String, When)> {
        clause(said).map(|c| (c.rest, c.when))
    }

    #[test]
    fn a_time_clause_is_read_first_last_and_in_german_after_the_verb() {
        let face = |hour, minute| When::At { hour, minute, either: true, tomorrow: false };
        let sure = |hour, minute| When::At { hour, minute, either: false, tomorrow: false };
        for (said, order, when) in [
            ("Play music in one minute.", "Play music", When::In(60)),
            ("close the board at 18:30", "close the board", sure(18, 30)),
            ("In ten minutes show the briefing", "show the briefing", When::In(600)),
            ("In ten minutes, show the briefing.", "show the briefing", When::In(600)),
            ("JAI, play music in one minute, please", "play music", When::In(60)),
            ("open the mixer in half an hour", "open the mixer", When::In(1800)),
            ("open the mixer in 90 seconds", "open the mixer", When::In(90)),
            ("open the mixer in an hour and a half", "open the mixer", When::In(5400)),
            ("open the mixer in two and a half hours", "open the mixer", When::In(9000)),
            ("open the mixer in 2 hours and 10 minutes", "open the mixer", When::In(7800)),
            ("open the mixer in a quarter of an hour", "open the mixer", When::In(900)),
            ("close the board at half past six", "close the board", face(6, 30)),
            ("close the board at quarter to seven", "close the board", face(6, 45)),
            ("close the board at ten past six", "close the board", face(6, 10)),
            ("close the board at 7 pm", "close the board", sure(19, 0)),
            ("close the board at 7 p.m.", "close the board", sure(19, 0)),
            ("close the board at 7:15 am", "close the board", sure(7, 15)),
            ("close the board at six thirty", "close the board", face(6, 30)),
            ("close the board at 6 o'clock", "close the board", face(6, 0)),
            ("close the board at noon", "close the board", sure(12, 0)),
            ("close the board at midnight", "close the board", sure(0, 0)),
            ("Tonight at 8 play music", "play music", sure(20, 0)),
            ("play music at 8 tonight", "play music", sure(20, 0)),
            ("play music at 07:30", "play music", sure(7, 30)),
            ("play music tomorrow at 7", "play music", When::At { hour: 7, minute: 0, either: false, tomorrow: true }),
            ("play Back in Black in 10 minutes", "play Back in Black", When::In(600)),
            // German: first, last, after the verb, before the part of the verb that stands last.
            ("Spiel in einer Minute Musik.", "Spiel Musik", When::In(60)),
            ("Um 18:30 zeig mir das Briefing", "zeig mir das Briefing", sure(18, 30)),
            ("Zeig mir um 18:30 das Briefing", "Zeig mir das Briefing", sure(18, 30)),
            ("spiel Musik in zehn Minuten", "spiel Musik", When::In(600)),
            ("schließ das Board in einer halben Stunde", "schließ das Board", When::In(1800)),
            ("mach den Mixer in 10 Minuten zu", "mach den Mixer zu", When::In(600)),
            ("öffne den Mixer in anderthalb Stunden", "öffne den Mixer", When::In(5400)),
            ("schließ das Board um halb sieben", "schließ das Board", face(6, 30)),
            ("schließ das Board um viertel nach sechs", "schließ das Board", face(6, 15)),
            ("schließ das Board um 18 Uhr 30", "schließ das Board", sure(18, 30)),
            ("spiel heute Abend um 8 Musik", "spiel Musik", sure(20, 0)),
            ("spiel Musik um 8 heute Abend", "spiel Musik", sure(20, 0)),
            ("spiel morgen früh um 7 Musik", "spiel Musik", When::At { hour: 7, minute: 0, either: false, tomorrow: true }),
        ] {
            assert_eq!(put_off(said), Some((order.to_string(), when)), "{said}");
        }
        // No clause: no time at all, a span that is no delay, two times, a time and nothing else.
        for said in [
            "play music",
            "play In the End",
            "set a timer for 5 minutes",
            "play music for ten minutes",
            "open the mixer in a new window",
            "what is the weather in Berlin",
            "in ten minutes",
            "at 18:30",
            "in ten minutes play music in one minute",
            "play music at 25:00",
            "set the brightness at 40",
            "play music in one",
            "play music in 10 minutes and 2 hours",
            "play in one minute some music",
            "I will be back in ten minutes, wait for me then",
        ] {
            assert_eq!(put_off(said), None, "{said}");
        }
        // The clause's own words, as they are looked for in what a rule took for a name.
        assert_eq!(clause("close the board at 18:30").map(|c| c.words), Some(vec!["at".to_string(), "18".to_string(), "30".to_string()]));
        assert!(clause("In ten minutes show the briefing").is_some_and(|c| c.leads) && clause("show the briefing in ten minutes").is_some_and(|c| !c.leads));
    }

    #[test]
    fn a_time_without_its_half_of_the_day_is_the_next_time_the_clock_shows_it() {
        let due = |when: When, now| when.due(now).map(|d| (d.date_naive() == at(0, 0).date_naive(), d.hour(), d.minute()));
        let half_past_six = When::At { hour: 6, minute: 30, either: true, tomorrow: false };
        assert_eq!(due(half_past_six, at(18, 2)), Some((true, 18, 30)), "said at two minutes past six in the evening");
        assert_eq!(due(half_past_six, at(5, 0)), Some((true, 6, 30)));
        assert_eq!(due(half_past_six, at(19, 0)), Some((false, 6, 30)), "both have passed: tomorrow morning");
        let seven_pm = When::At { hour: 19, minute: 0, either: false, tomorrow: false };
        assert_eq!(due(seven_pm, at(18, 2)), Some((true, 19, 0)));
        assert_eq!(due(seven_pm, at(19, 30)), Some((false, 19, 0)), "seven has passed: tomorrow's");
        assert_eq!(due(When::At { hour: 12, minute: 0, either: true, tomorrow: false }, at(13, 0)), Some((false, 0, 0)), "\"at twelve\" after noon is midnight");
        assert_eq!(due(When::At { hour: 7, minute: 0, either: false, tomorrow: true }, at(6, 0)), Some((false, 7, 0)));
        assert_eq!(When::In(60).due(at(18, 2)), Some(at(18, 3)));
        assert_eq!((span_words(60), span_words(600), span_words(90), span_words(5400), span_words(3600), span_words(9000), span_words(7800), span_words(45)), ("one minute".into(), "10 minutes".into(), "90 seconds".into(), "an hour and a half".into(), "one hour".into(), "2 and a half hours".into(), "2 hours and 10 minutes".into(), "45 seconds".into()));
        assert_eq!((clock(at(18, 30), at(18, 2)), clock(at(18, 30) + Span::days(1), at(18, 2)), clock(at(7, 0) + Span::days(3), at(18, 2))), ("18:30".to_string(), "18:30 tomorrow".to_string(), "07:00 on Monday".to_string()));
    }

    #[test]
    fn an_order_is_put_off_when_it_is_one_without_its_time_and_none_with_it() {
        let _dock = crate::hud_dock::test_lock();
        let _kit = crate::kit::test_lock();
        forget_all();
        let secs = |order: &str, secs: i64| Some(json!({"action": "set", "order": order, "in_s": secs}));
        let clock = |order: &str, at: &str, either: bool| Some(json!({"action": "set", "order": order, "at": at, "either": either, "tomorrow": false}));
        for (said, want) in [
            // What Sir said on 2026-11-13: the music rule had taken the clause for the song's name.
            ("Play music in one minute.", secs("Play music", 60)),
            ("close the board at 18:30", clock("close the board", "18:30", false)),
            ("In ten minutes show the briefing", secs("show the briefing", 600)),
            ("Spiel in einer Minute Musik", secs("Spiel Musik", 60)),
            ("Um 18:30 zeig mir das Briefing", clock("zeig mir das Briefing", "18:30", false)),
            ("open the mixer in half an hour", secs("open the mixer", 1800)),
            ("open the mixer in 90 seconds", secs("open the mixer", 90)),
            ("open the mixer in an hour and a half", secs("open the mixer", 5400)),
            ("open the mixer at half past six", clock("open the mixer", "06:30", true)),
            ("open the mixer at 7 pm", clock("open the mixer", "19:00", false)),
            ("Tonight at 8 open the mixer", clock("open the mixer", "20:00", false)),
            ("Can you play music in one minute?", secs("Can you play music", 60)),
            ("mute discord in ten minutes", secs("mute discord", 600)),
            ("pause in 5 minutes", secs("pause", 300)),
            ("night mode at 22:00", clock("night mode", "22:00", false)),
            ("could you kill that sleep timer in ten minutes", secs("could you kill that sleep timer", 600)),
            // Several orders with the time in front: all of them wait.
            ("In ten minutes close the board and open the mixer", secs("close the board and open the mixer", 600)),
        ] {
            assert_eq!(order(said), want, "{said}");
            assert!(matches!(crate::brain::fast_as_said(said), Some(Fast::Intent(Intent::Tool { name: TOOL, .. }))), "{said}: the fast path's own answer");
            assert!(matches!(crate::intents::parse(said), Some(Intent::Tool { name: TOOL, .. })), "{said}: the parser's");
            assert!(crate::brain::another_way(said).is_none(), "{said}: understood as it stands");
        }
    }

    #[test]
    fn what_is_understood_as_a_whole_is_left_as_it_was() {
        let _dock = crate::hud_dock::test_lock();
        let _kit = crate::kit::test_lock();
        forget_all();
        for said in [
            // Timers, reminders, the sleep timer, the PC's power, the calendar, rules, notes.
            "set a timer for 5 minutes",
            "start a timer in 5 minutes",
            "remind me in 10 minutes to check the oven",
            "remind me at 6 pm to call the dentist",
            "stop the music in 20 minutes",
            "turn the music off in 20 minutes",
            "mach die Musik in 15 Minuten aus",
            "sleep timer in 30 minutes",
            "shut down my PC in 10 minutes",
            "restart the computer at 8 pm",
            "add dentist to my calendar tomorrow at 3 pm",
            "every day at 7:30 open my calendar",
            "note: call the dentist at 6 pm",
            // Questions never change anything.
            "what is scheduled at 18:30?",
            "what time is it in ten minutes?",
            "is the board still open at 6 pm?",
            "should I close the board in ten minutes?",
            // No order without the clause: the language model's, as before.
            "tell me a joke in ten minutes",
            "in ten minutes I have to leave",
            "at 7 pm there is a show on TV",
            "the train leaves in ten minutes",
            "I will be back in half an hour",
            "we could go for a walk in an hour",
            // A setting with a number is no time.
            "set the volume at 5",
            "brightness at 40",
        ] {
            assert_eq!(order(said), None, "{said}");
            assert!(!matches!(crate::brain::fast_as_said(said), Some(Fast::Intent(Intent::Tool { name: TOOL, .. }))), "{said}");
        }
        // The sleep timer's own phrase is still the sleep timer's.
        assert!(matches!(crate::intents::parse("stop the music in 20 minutes"), Some(Intent::Tool { name: "sleep_timer", .. })));
        assert!(matches!(crate::intents::parse("turn the music off in 20 minutes"), Some(Intent::Tool { name: "sleep_timer", .. })));
        // With several orders and the time last, it is said of the last one: the planner
        // reads the sentence clause by clause.
        assert_eq!(order("open the mixer and play music in one minute"), None);
        let plan = crate::compound_planner::plan("open the mixer and play music in one minute", &crate::intent_engine::ReflexContext::live()).expect("two orders");
        assert_eq!((plan.calls.len(), plan.calls[0].clause.as_str(), plan.calls[1].tool), (2, "open the mixer", TOOL), "the mixer opens now");
        assert_ne!(plan.calls[0].tool, TOOL);
        assert_eq!(plan.calls[1].args, json!({"action": "set", "order": "play music", "in_s": 60}));
        // "In 20 minutes stop the music and close the board": its first order is the sleep timer's.
        assert_eq!(order("in 20 minutes stop the music and close the board"), None);
    }

    #[test]
    fn orders_wait_are_listed_dropped_and_fall_due() {
        let _dock = crate::hud_dock::test_lock();
        let _kit = crate::kit::test_lock();
        forget_all();
        let now = at(18, 2);
        assert!(run_at(&json!({"action": "list"}), now).unwrap().contains("Nothing is scheduled, Sir."));
        assert!(run_at(&json!({"action": "cancel", "which": "all"}), now).is_err());
        // His answer is short and names the time.
        let said = run_at(&json!({"action": "set", "order": "Play music", "in_s": 60}), now).unwrap();
        assert!(said.ends_with("Spoken: In one minute, Sir.") && said.starts_with("Planned for 18:03: Play music"), "{said}");
        let said = run_at(&json!({"action": "set", "order": "close the board", "at": "18:30", "either": false, "tomorrow": false}), now).unwrap();
        assert!(said.ends_with("Spoken: At 18:30, Sir."), "{said}");
        let said = run_at(&json!({"action": "set", "order": "open the mixer", "at": "06:30", "either": true, "tomorrow": false}), now).unwrap();
        assert!(said.ends_with("Spoken: At 18:30, Sir."), "half past six, said at two past six in the evening: {said}");
        let said = run_at(&json!({"action": "set", "order": "night mode", "at": "07:00", "either": false, "tomorrow": false}), now).unwrap();
        assert!(said.ends_with("Spoken: At 07:00 tomorrow, Sir."), "{said}");
        assert_eq!(planned().iter().map(|(due, order)| (due.format("%d %H:%M").to_string(), order.clone())).collect::<Vec<_>>(), [("13 18:03".to_string(), "Play music".to_string()), ("13 18:30".into(), "close the board".into()), ("13 18:30".into(), "open the mixer".into()), ("14 07:00".into(), "night mode".into())]);
        // They are kept where they survive a restart.
        assert_eq!(crate::kit::list(KIND).len(), 4);
        // Listed with their times; a question lists and never drops.
        let listed = run_at(&json!({"action": "list"}), now).unwrap();
        assert!(listed.contains("1.  18:03  \u{00B7}  Play music") && listed.contains("4.  07:00 tomorrow  \u{00B7}  night mode") && listed.contains("4 orders are waiting, Sir; the next is Play music at 18:03."), "{listed}");
        assert!(crate::tools::only_looks(TOOL, &json!({"action": "list"})) && !crate::tools::only_looks(TOOL, &json!({"action": "cancel", "which": "all"})) && !crate::tools::only_looks(TOOL, &json!({"action": "set"})));
        assert!(crate::intents::beyond_the_question("is the music still scheduled, or did you cancel everything scheduled?", TOOL, &json!({"action": "cancel", "which": "all"})));
        // Dropped by its number, by what it is about, the one put off last, all of them.
        assert!(run_at(&json!({"action": "cancel", "number": 2}), now).unwrap().ends_with("Spoken: Cancelled, Sir: close the board at 18:30."));
        assert!(run_at(&json!({"action": "cancel", "about": "the music"}), now).unwrap().contains("Cancelled, Sir: Play music at 18:03."));
        assert!(run_at(&json!({"action": "cancel", "about": "the lights"}), now).is_err() && run_at(&json!({"action": "cancel", "number": 9}), now).is_err());
        assert!(run_at(&json!({"action": "cancel", "which": "last"}), now).unwrap().contains("night mode at 07:00 tomorrow"), "the one put off last, not the one due last");
        assert_eq!(planned().len(), 1);
        run_at(&json!({"action": "set", "order": "pause", "in_s": 300}), now).unwrap();
        assert!(run_at(&json!({"action": "cancel", "which": "all"}), now).unwrap().ends_with("Spoken: Both are cancelled, Sir."));
        assert!(planned().is_empty() && crate::kit::list(KIND).is_empty());
        // Falling due: in their order, once, and gone from the store.
        run_at(&json!({"action": "set", "order": "open the mixer", "in_s": 120}), now).unwrap();
        run_at(&json!({"action": "set", "order": "Play music", "in_s": 60}), now).unwrap();
        run_at(&json!({"action": "set", "order": "close the board", "in_s": 3600}), now).unwrap();
        assert!(fall_due(now + Span::seconds(59)).is_empty());
        assert_eq!(fall_due(now + Span::seconds(121)), ["Play music", "open the mixer"]);
        assert!(fall_due(now + Span::seconds(122)).is_empty(), "each falls due once");
        assert_eq!((planned().len(), crate::kit::list(KIND).len()), (1, 1));
        // What makes no sense is refused in his words.
        assert!(run_at(&json!({"action": "set", "order": "", "in_s": 60}), now).is_err() && run_at(&json!({"action": "set", "order": "pause"}), now).is_err());
        forget_all();
        crate::kit::close_text(BLOCK);
    }

    #[test]
    fn what_fell_due_while_he_was_off_is_dropped_and_told_once() {
        let _dock = crate::hud_dock::test_lock();
        let _kit = crate::kit::test_lock();
        forget_all();
        let now = at(18, 2);
        // As the store holds them after a restart: two that fell due meanwhile, one still ahead.
        for (key, order, due) in [("1", "Play music", at(17, 30)), ("2", "close the board", at(18, 0)), ("3", "open the mixer", at(19, 0))] {
            crate::kit::put(KIND, key, &json!({"order": order, "due_ms": due.timestamp_millis()})).unwrap();
        }
        let missed = load_at(now);
        assert_eq!(missed, [(at(17, 30), "Play music".to_string()), (at(18, 0), "close the board".to_string())]);
        assert_eq!(planned(), [(at(19, 0), "open the mixer".to_string())], "what is still ahead waits on");
        assert_eq!(crate::kit::list(KIND).len(), 1, "what was missed is out of the store: it is told once");
        assert!(fall_due(now).is_empty(), "nothing that was missed runs late");
        assert_eq!(missed_line(&missed).as_deref(), Some("While I was off, Sir, 2 orders of yours fell due and were not carried out: \u{201C}Play music\u{201D} at 17:30, and \u{201C}close the board\u{201D} at 18:00."));
        assert_eq!(missed_line(&missed[..1]).as_deref(), Some("While I was off, Sir, an order of yours fell due and was not carried out: \u{201C}Play music\u{201D} at 17:30."));
        assert!(load_at(now).is_empty() && missed_line(&[]).is_none(), "a second start has nothing to tell");
        forget_all();
    }

    #[test]
    fn the_planned_orders_are_asked_about_and_cancelled_in_his_words() {
        let _dock = crate::hud_dock::test_lock();
        let _kit = crate::kit::test_lock();
        forget_all();
        let phrase = |said: &str| command(&crate::intents::normalize_phrase(said));
        for said in ["What is scheduled?", "what's scheduled", "What have you got planned?", "what is planned", "show me the scheduled orders", "is anything scheduled?", "Was ist geplant?", "Was hast du geplant?", "zeig mir die geplanten Befehle"] {
            assert_eq!(phrase(said), Some(json!({"action": "list"})), "{said}");
            assert!(matches!(crate::intents::parse(said), Some(Intent::Tool { name: TOOL, .. })), "{said}");
        }
        for said in ["cancel everything scheduled", "Cancel everything that is scheduled.", "cancel all scheduled orders", "drop everything planned", "Streich alles Geplante", "lösche alle geplanten Befehle"] {
            assert_eq!(phrase(said), Some(json!({"action": "cancel", "which": "all"})), "{said}");
        }
        assert_eq!(phrase("cancel the scheduled music"), Some(json!({"action": "cancel", "about": "music"})));
        assert_eq!(phrase("streich die geplante Musik"), Some(json!({"action": "cancel", "about": "musik"})));
        assert_eq!(phrase("cancel the second scheduled order"), Some(json!({"action": "cancel", "number": 2})));
        // "The second one" means one of these only while their list is fresh.
        assert_eq!(phrase("cancel the second one"), None);
        assert_eq!(cancel_just_made("cancel that"), None, "nothing was put off");
        run_at(&json!({"action": "set", "order": "Play music", "in_s": 60}), at(18, 2)).unwrap();
        assert_eq!(phrase("cancel the second one"), Some(json!({"action": "cancel", "number": 2})));
        assert_eq!(phrase("cancel the last one"), Some(json!({"action": "cancel", "which": "last"})));
        assert_eq!(phrase("streich den zweiten"), Some(json!({"action": "cancel", "number": 2})));
        // "Cancel that" right after putting one off drops it; anything else is no cancel.
        for said in ["Cancel that.", "never mind", "Vergiss es"] {
            assert_eq!(cancel_just_made(said), Some(json!({"action": "cancel", "which": "last"})), "{said}");
        }
        assert_eq!(cancel_just_made("cancel the timer"), None);
        assert!(matches!(crate::brain::fast_as_said("cancel that"), Some(Fast::Intent(Intent::Tool { name: TOOL, .. }))));
        run_at(&json!({"action": "cancel", "which": "last"}), at(18, 2)).unwrap();
        assert_eq!(cancel_just_made("cancel that"), None, "it is gone already");
        assert!(matches!(crate::brain::fast_as_said("cancel that"), Some(Fast::Other)), "the conversation's cancel again");
        // Talk about plans and the timetable is nobody's list.
        for said in ["what is planned for tomorrow", "what's on the schedule", "cancel the plan", "my schedule", "I planned a trip"] {
            assert_eq!(phrase(said), None, "{said}");
        }
        forget_all();
    }

    #[test]
    fn a_silent_order_gets_a_word_first_and_a_test_session_leaves_the_pc_alone() {
        let _dock = crate::hud_dock::test_lock();
        let _kit = crate::kit::test_lock();
        assert!(runs_without_a_word("pause") && runs_without_a_word("mute"));
        assert!(!runs_without_a_word("show the briefing") && !runs_without_a_word("open the mixer"));
        // (Under `cargo test` no app runs and nothing falls due; a scripted session carries
        // one out only where its tools stay inside J.A.I.)
        assert!(may_run_here() || crate::platform::in_test_session());
        assert!(crate::kit::stays_inside(TOOL, &json!({"action": "set"})) && crate::kit::is_tool(TOOL));
    }
}
