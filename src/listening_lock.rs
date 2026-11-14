//! "Stop listening" — Sir's privacy lock. "JAI, stop listening for a minute" / "for 5 minutes"
//! / "Hör zehn Minuten nicht zu" / "mic off for 20 minutes" pauses him for that long, "don't
//! listen until eight" / "hör bis acht nicht zu" until the clock next shows that time; "stop
//! listening", "mute my microphone", "don't listen", "Hör auf zuzuhören" until Sir lets him
//! listen again. While locked every word is ignored — commands, questions, even "JAI" — except
//! the resume phrases ("you can listen again", "resume listening", "unmute microphone", "Du
//! kannst wieder zuhören", "JAI hör wieder zu"), the one question about the lock itself ("how
//! long until you listen again?") and the header's `[ MIC: MUTED ]` button. A lock with an end
//! lifts itself, and he says so (`take_lapsed`).
//!
//! Nothing of it is kept anywhere: the state lives in this process, so a restart ends a lock,
//! timed or not.
//!
//! While he is not listening, what he hears is not put on screen either (Sir, 2026-11-13: "when I
//! tell JAI not to listen then the subtitles should [not] show except if I explicitly tell it
//! to"): `may_show_heard` is asked by everything that writes heard words on the HUD.

use regex::Regex;
use std::ops::Range;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListeningLockState {
    Listening,
    PausedUntil(Instant),
    LockedUntilWakePhrase,
}

/// What a phrase asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LockCommand {
    /// Pause for this long.
    Pause(Duration),
    /// Pause until the clock next shows this time of day (hour 0-23, minute).
    Until(u32, u32),
    /// Until Sir says he may listen again.
    Lock,
    Resume,
}

static STATE: Mutex<ListeningLockState> = Mutex::new(ListeningLockState::Listening);
/// A lock with an end ran out by itself and Sir has not been told yet (`take_lapsed`).
static LAPSED: AtomicBool = AtomicBool::new(false);
/// Sir asked, while the lock is on, to see what is heard after all (`show_asked`).
static SHOW_ASKED: AtomicBool = AtomicBool::new(false);

/// The state now (a timed pause that ran out reads as listening).
pub fn state() -> ListeningLockState {
    let mut s = STATE.lock().unwrap_or_else(|e| e.into_inner());
    if let ListeningLockState::PausedUntil(until) = *s {
        if Instant::now() >= until {
            *s = ListeningLockState::Listening;
            // (Whoever looks first finds it over: the app is told once, by `take_lapsed`.)
            LAPSED.store(true, Ordering::Release);
            SHOW_ASKED.store(false, Ordering::Release);
        }
    }
    *s
}

pub fn is_locked() -> bool {
    state() != ListeningLockState::Listening
}

pub fn apply(cmd: LockCommand) {
    let next = match cmd {
        LockCommand::Pause(d) => ListeningLockState::PausedUntil(Instant::now() + d),
        LockCommand::Until(hour, minute) => ListeningLockState::PausedUntil(Instant::now() + time_until(seconds_of_day_now(), hour, minute)),
        LockCommand::Lock => ListeningLockState::LockedUntilWakePhrase,
        LockCommand::Resume => ListeningLockState::Listening,
    };
    *STATE.lock().unwrap_or_else(|e| e.into_inner()) = next;
    // Sir's own word, whichever it was: nothing is left to announce, and what he asked to see
    // under an earlier lock he asks anew under this one.
    LAPSED.store(false, Ordering::Release);
    SHOW_ASKED.store(false, Ordering::Release);
    crate::gui::refresh();
}

/// How long he still does not listen, for the lock's indicator: None while he listens, and for
/// a lock without an end ("until you tell me").
pub fn remaining() -> Option<Duration> {
    match state() {
        ListeningLockState::PausedUntil(until) => Some(until.saturating_duration_since(Instant::now())),
        _ => None,
    }
}

/// True once after a lock with an end has run out by itself (never after Sir lifted it): the
/// app then says that he listens again.
pub fn take_lapsed() -> bool {
    state();
    LAPSED.swap(false, Ordering::AcqRel)
}

/// Sir asked for subtitles or for his words on screen while the lock is on (typed, or by the
/// settings' switch): what is heard may be shown until the lock ends. Nothing while he listens.
pub fn show_asked() {
    if is_locked() && !SHOW_ASKED.swap(true, Ordering::AcqRel) {
        tracing::info!("[PRIVACY] Sir asked to see what is heard while I am not listening: shown until the lock ends");
        crate::gui::refresh();
    }
}

/// Whether what the microphone hears may be put on screen (the words under the emblem, the
/// subtitles of a mix that carries the microphone): always while he listens; under the lock
/// only once Sir has asked for it (`show_asked`).
pub fn may_show_heard() -> bool {
    !is_locked() || SHOW_ASKED.load(Ordering::Acquire)
}

/// The header's status: `LISTENING PAUSED (0:48) — SAY "YOU CAN LISTEN AGAIN" TO RESUME`.
pub fn status_line() -> Option<String> {
    match state() {
        ListeningLockState::Listening => None,
        ListeningLockState::PausedUntil(until) => {
            let left = until.saturating_duration_since(Instant::now()).as_secs();
            let clock = if left >= 3600 { format!("{}:{:02}:{:02}", left / 3600, left % 3600 / 60, left % 60) } else { format!("{}:{:02}", left / 60, left % 60) };
            Some(format!("LISTENING PAUSED ({clock}) — SAY \"YOU CAN LISTEN AGAIN\" TO RESUME"))
        }
        ListeningLockState::LockedUntilWakePhrase => Some("LISTENING PAUSED — SAY \"YOU CAN LISTEN AGAIN\" TO RESUME".into()),
    }
}

/// A stretch of time in his words: "40 seconds", "a minute", "7 minutes", "an hour", "1 hour
/// and 48 minutes" (minutes are rounded up: "7 minutes" is what is left of six and a bit).
fn span_words(secs: u64) -> String {
    let plural = |n: u64, one: &str, unit: &str| if n == 1 { one.to_string() } else { format!("{n} {unit}s") };
    if secs < 60 {
        return plural(secs, "a second", "second");
    }
    let minutes = secs.div_ceil(60);
    match (minutes / 60, minutes % 60) {
        (0, m) => plural(m, "a minute", "minute"),
        (h, 0) => plural(h, "an hour", "hour"),
        (h, m) => format!("{} and {}", plural(h, "1 hour", "hour"), plural(m, "1 minute", "minute")),
    }
}

/// "Is my mic muted?", "are you listening?" in his words.
pub fn spoken_state() -> String {
    match state() {
        ListeningLockState::Listening => "I'm listening, Sir.".to_string(),
        ListeningLockState::PausedUntil(until) => {
            let left = until.saturating_duration_since(Instant::now()).as_secs();
            let span = if left >= 3600 {
                format!("another {}", span_words(left))
            } else if left >= 120 {
                format!("{} more minutes", left.div_ceil(60))
            } else if left >= 60 {
                "another minute".to_string()
            } else {
                format!("{left} more seconds")
            };
            format!("I've stopped listening for {span}, Sir.")
        }
        ListeningLockState::LockedUntilWakePhrase => "I'm not listening, Sir, until you tell me I can listen again.".to_string(),
    }
}

/// "How long until you listen again?" in his words.
pub fn how_long() -> String {
    match state() {
        ListeningLockState::Listening => "I'm listening now, Sir.".to_string(),
        ListeningLockState::PausedUntil(until) => {
            let left = until.saturating_duration_since(Instant::now()).as_secs();
            let span = span_words(left);
            // ("Another a minute" is no English.)
            match span.strip_prefix("a ").or_else(|| span.strip_prefix("an ")) {
                Some(unit) => format!("Another {unit}, Sir."),
                None => format!("Another {span}, Sir."),
            }
        }
        ListeningLockState::LockedUntilWakePhrase => "Until you tell me I can listen again, Sir.".to_string(),
    }
}

/// "How long until you listen again?", "when will you listen again?", "wie lange hörst du noch
/// nicht zu?", "wann hörst du wieder zu?": the one question the lock itself answers, by voice
/// too. (Asked first wherever a sentence is read: it holds the very words that lift the lock,
/// "listen again", and a question lifts nothing.)
pub fn asks_how_long(text: &str) -> bool {
    static ASKED: LazyLock<Regex> = LazyLock::new(|| {
        const LEAD: &str = r"^(?:(?:hey|ok|okay|so|and|und|sag mal|jai|jay|sir) )*";
        const MIC: &str = r"(?:the |my |your )?(?:mic|microphone)";
        const HEARS: &str = r"(?:listen(?:ing)?|hear(?:ing)?)(?: to)?(?: me)?";
        Regex::new(&format!(
            concat!(
                // "How long until you (can) listen again", "how much longer until the mic is back on".
                r"{LEAD}(?:how (?:much )?long(?:er)? (?:is it |will it be |does it take )?(?:until|till|til|before) (?:(?:you|u) (?:can |will |may |are |start |are allowed to )?{HEARS}(?: again)?|{MIC} (?:is |comes |goes |turns )?(?:back )?on(?: again)?)",
                // "How long will you not listen", "how much longer are you not listening", "how long is the mic off".
                r"|how (?:much )?long(?:er)? (?:will|do|are|is|does) (?:you|u|{MIC}) (?:still |be |stay |keep )*(?:not {HEARS}|deaf|muted|off|paused|locked|silent)(?: for)?",
                // "When will you listen again", "when does the mic come back on".
                r"|when (?:will|do|can|are|may) (?:you|u) (?:be )?{HEARS} again",
                r"|when (?:does|will|is) {MIC} (?:come |go |be |turn |turned )?(?:back )?on(?: again)?",
                // "Wie lange (noch) bis du wieder zuhörst", "wie lange hörst du noch nicht zu", "wann hörst du wieder zu".
                r"|wie lange (?:noch |dauert es (?:noch )?)?bis du (?:mir )?wieder (?:zuhörst|zuhoerst|hörst|hoerst)",
                r"|wie lange (?:hörst|hoerst) du (?:mir )?(?:noch |jetzt )*nicht(?: mehr)? zu",
                r"|wie lange (?:ist|bleibt) (?:das |dein |mein )?(?:mikro|mikrofon) (?:noch )?(?:aus|stumm)",
                r"|wann (?:hörst|hoerst) du (?:mir )?wieder zu",
                r"|wann (?:ist|geht) (?:das |dein |mein )?(?:mikro|mikrofon) wieder an)$"
            ),
            LEAD = LEAD,
            MIC = MIC,
            HEARS = HEARS
        ))
        .expect("the question about the lock")
    });
    ASKED.is_match(&words(text))
}

fn words(text: &str) -> String {
    let s: String = text.to_lowercase().chars().map(|c| if c.is_alphanumeric() || c == ' ' { c } else { ' ' }).collect();
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

const RESUME: &[&str] = &[
    "you can listen again",
    "you can now listen again",
    "you may listen again",
    "you can listen now",
    "you may listen now",
    "start listening",
    "begin listening",
    "continue listening",
    "keep listening",
    "resume listening",
    "listen again",
    "unmute microphone",
    "unmute my microphone",
    "unmute the microphone",
    "unmute your microphone",
    "unmute mic",
    "unmute my mic",
    "unmute the mic",
    "unmute your mic",
    "turn on the mic",
    "turn on my mic",
    "turn on your mic",
    "turn the mic on",
    "turn my mic on",
    "turn the mic back on",
    "turn my mic back on",
    "turn on the microphone",
    "turn on my microphone",
    "turn the microphone on",
    "turn my microphone on",
    "turn the microphone back on",
    "turn my microphone back on",
    "mikro an",
    "mikrofon an",
    "mikro wieder an",
    "mikrofon wieder an",
    "du kannst wieder zuhören",
    "du kannst wieder zuhoeren",
    "hör wieder zu",
    "hoer wieder zu",
    "du darfst wieder zuhören",
    "fang an zuzuhören",
    "fang wieder an zuzuhören",
];

/// Whether `text` lets him listen again (the only words that get through the lock).
pub fn is_resume_phrase(text: &str) -> bool {
    let s = words(text);
    RESUME.iter().any(|p| s.contains(p))
}

/// What happens to something Sir said.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Not locked: it is heard.
    Heard,
    /// Locked, and these are the words that lift the lock.
    Resumes,
    /// Locked, and he is asked how long for (`how_long` is the answer; the lock stays).
    Asks,
    /// Locked, or the microphone is switched off: dropped unheard, the wake word included.
    Ignored,
}

/// The one decision the microphone path makes for every transcript. `mic_on`: the microphone
/// is switched on, or the words were said with the push-to-talk key held. Under the lock only
/// the words that lift it get through, and the question how long it still holds; with the
/// microphone switched off nothing does (words recorded a moment before the `[ MIC ]` chip was
/// clicked are not acted on after it).
pub fn verdict(text: &str, mic_on: bool) -> Verdict {
    if is_locked() {
        // (The question first: "how long until you listen again" holds the lifting words.
        // And a plain question lifts nothing, here as on every other path.)
        if asks_how_long(text) {
            Verdict::Asks
        } else if is_resume_phrase(text) && !crate::intents::only_asks(text) {
            Verdict::Resumes
        } else {
            Verdict::Ignored
        }
    } else if mic_on {
        Verdict::Heard
    } else {
        Verdict::Ignored
    }
}

// ------------------------------------------------------------------- spans and times ----

/// A number below ten thousand as Sir says it or the recogniser writes it: "7", "seven",
/// "sieben", "twenty", "fünfundvierzig". ("Forty five" is two words: `number_ending_at`.)
fn number_word(word: &str) -> Option<u64> {
    if word.chars().all(|c| c.is_ascii_digit()) {
        return word.parse().ok().filter(|n| *n < 10_000);
    }
    let small = |word: &str| -> Option<u64> {
        Some(match word {
            "zero" | "null" => 0,
            "a" | "an" | "one" | "ein" | "eine" | "einer" | "einen" | "einem" | "eins" => 1,
            "two" | "zwei" | "zwo" | "couple" => 2,
            "three" | "drei" => 3,
            "four" | "vier" => 4,
            "five" | "fünf" | "fuenf" => 5,
            "six" | "sechs" => 6,
            "seven" | "sieben" => 7,
            "eight" | "acht" => 8,
            "nine" | "neun" => 9,
            "ten" | "zehn" => 10,
            "eleven" | "elf" => 11,
            "twelve" | "zwölf" | "zwoelf" => 12,
            "thirteen" | "dreizehn" => 13,
            "fourteen" | "vierzehn" => 14,
            "fifteen" | "fünfzehn" | "fuenfzehn" => 15,
            "sixteen" | "sechzehn" => 16,
            "seventeen" | "siebzehn" => 17,
            "eighteen" | "achtzehn" => 18,
            "nineteen" | "neunzehn" => 19,
            "twenty" | "zwanzig" => 20,
            "thirty" | "dreißig" | "dreissig" => 30,
            "forty" | "fourty" | "vierzig" => 40,
            "fifty" | "fünfzig" | "fuenfzig" => 50,
            "sixty" | "sechzig" => 60,
            "seventy" | "siebzig" => 70,
            "eighty" | "achtzig" => 80,
            "ninety" | "neunzig" => 90,
            _ => return None,
        })
    };
    small(word).or_else(|| {
        // "Fünfundvierzig", "einundzwanzig": the one before the ten.
        let (one, ten) = word.split_once("und")?;
        let (one, ten) = (small(one).filter(|n| (1..=9).contains(n))?, small(ten).filter(|n| *n >= 20 && n % 10 == 0)?);
        Some(ten + one)
    })
}

/// The number whose last word is `w[end - 1]`, and where it begins ("forty five" is two words).
fn number_ending_at(w: &[&str], end: usize) -> Option<(u64, usize)> {
    let last = number_word(w.get(end.checked_sub(1)?)?)?;
    if (1..=9).contains(&last) && end >= 2 && !matches!(w[end - 1], "a" | "an" | "couple") {
        if let Some(ten) = number_word(w[end - 2]).filter(|n| (20..=90).contains(n) && n % 10 == 0 && !w[end - 2].chars().all(|c| c.is_ascii_digit())) {
            return Some((ten + last, end - 2));
        }
    }
    Some((last, end - 1))
}

fn unit_secs(word: &str) -> Option<u64> {
    Some(match word {
        "second" | "seconds" | "sec" | "secs" | "sekunde" | "sekunden" => 1,
        "minute" | "minutes" | "min" | "mins" | "minuten" => 60,
        "hour" | "hours" | "hr" | "hrs" | "stunde" | "stunden" => 3600,
        _ => return None,
    })
}

/// The span in `w` and the words it stands in: "a minute", "5 minutes", "forty five minutes",
/// "half an hour", "eine halbe Stunde", "an hour and a half", "two and a half hours", "1 hour
/// and 30 minutes", "anderthalb Stunden", "a quarter of an hour". Between five seconds and a
/// day.
fn span_in(w: &[&str]) -> Option<(Duration, Range<usize>)> {
    let found = |secs: u64, words: Range<usize>| Some((Duration::from_secs(secs.clamp(5, 24 * 3600)), words));
    for (i, word) in w.iter().enumerate() {
        if *word == "viertelstunde" {
            let from = if i > 0 && matches!(w[i - 1], "eine" | "einer" | "ne") { i - 1 } else { i };
            return found(900, from..i + 1);
        }
        let Some(unit) = unit_secs(word) else { continue };
        let back = |k: usize| i.checked_sub(k).map(|j| w[j]);
        // What stands before the unit: (seconds, the first word of it).
        let before: Option<(u64, usize)> = if matches!(back(1), Some("halbe" | "halben")) {
            // "Eine halbe Stunde".
            Some((unit / 2, if matches!(back(2), Some("eine" | "einer" | "ne")) { i - 2 } else { i - 1 }))
        } else if matches!(back(1), Some("a" | "an")) && back(2) == Some("half") {
            // "Half an hour" has the "an" right before the hour.
            Some((unit / 2, i - 2))
        } else if matches!(back(1), Some("a" | "an")) && back(2) == Some("of") && back(3) == Some("quarter") {
            Some((unit / 4, if back(4) == Some("a") { i - 4 } else { i - 3 }))
        } else if back(1) == Some("half") && back(2) == Some("a") && back(3) == Some("and") {
            // "Two and a half hours".
            number_ending_at(w, i - 3).map(|(n, from)| (n * unit + unit / 2, from))
        } else if matches!(back(1), Some("anderthalb" | "eineinhalb")) {
            Some((unit + unit / 2, i - 1))
        } else if let Some(n) = back(1).and_then(|x| x.strip_suffix("einhalb")).and_then(number_word) {
            // "Zweieinhalb Stunden".
            Some((n * unit + unit / 2, i - 1))
        } else if back(1) == Some("of") && back(2) == Some("couple") {
            Some((2 * unit, if back(3) == Some("a") { i - 3 } else { i - 2 }))
        } else if i == 0 {
            // ("Minute" on its own: one of them.)
            Some((unit, i))
        } else {
            number_ending_at(w, i).map(|(n, from)| (n * unit, from))
        };
        // (A unit after a word that is no number is no span: "the last hour". The next may be.)
        let Some((mut secs, from)) = before else { continue };
        let mut to = i + 1;
        // "An hour and a half", "an hour and twenty minutes", "1 hour 30 minutes".
        let mut next = to;
        if matches!(w.get(next), Some(&("and" | "und"))) {
            next += 1;
        }
        if w.get(next) == Some(&"a") && w.get(next + 1) == Some(&"half") && next > to {
            secs += unit / 2;
            to = next + 2;
        } else if let Some(smaller) = w.get(next + 1).and_then(|x| unit_secs(x)).filter(|smaller| *smaller < unit) {
            if let Some(n) = w.get(next).and_then(|x| number_word(x)) {
                secs += n * smaller;
                to = next + 2;
            }
        }
        return found(secs, from..to);
    }
    None
}

/// "for a minute", "for 5 minutes", "für 2 Minuten", "for 30 seconds", "for an hour", "for half
/// an hour", "für eine halbe Stunde", "for forty five minutes", "for an hour and a half".
pub(crate) fn duration_in(s: &str) -> Option<Duration> {
    let w: Vec<&str> = s.split_whitespace().collect();
    span_in(&w).map(|(span, _)| span)
}

/// Seconds since midnight by the clock on the wall.
fn seconds_of_day_now() -> u32 {
    use chrono::Timelike;
    chrono::Local::now().num_seconds_from_midnight()
}

/// How long from `now` (seconds since midnight) until the clock next shows `hour`:`minute`:
/// later today, or tomorrow (a whole day when it shows exactly that right now).
fn time_until(now: u32, hour: u32, minute: u32) -> Duration {
    const DAY: u32 = 24 * 3600;
    let target = (hour % 24) * 3600 + (minute % 60) * 60;
    let ahead = (target + DAY - now % DAY) % DAY;
    Duration::from_secs(if ahead == 0 { DAY } else { ahead } as u64)
}

/// A time of day as it was said, before it is known which half of the day is meant.
struct Said {
    hour: u32,
    minute: u32,
    /// Some(true): the afternoon half ("pm", "abends"); Some(false): the morning half.
    late: Option<bool>,
    /// Said on the 24-hour clock ("20 Uhr", "noon", "midnight"): no half to choose.
    exact: bool,
}

/// An hour as the recogniser writes it at `w[i]`: "8", "eight", "acht", "20", "8pm". The hour,
/// the half of the day when it is glued to it, and the word after it.
fn hour_at(w: &[&str], i: usize) -> Option<(u32, Option<bool>, usize)> {
    let word = *w.get(i)?;
    if matches!(word, "a" | "an" | "eine" | "einer" | "einen" | "einem" | "couple") {
        return None;
    }
    let (digits, late) = match word.strip_suffix("pm").map(|d| (d, Some(true))).or_else(|| word.strip_suffix("am").map(|d| (d, Some(false)))) {
        Some((digits, late)) if !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit()) => (digits, late),
        _ => (word, None),
    };
    let hour = number_word(digits).filter(|h| *h <= 24)? as u32;
    Some((hour, late, i + 1))
}

/// The minutes said after an hour, at `w[at]`: "30" and "05" (as the recogniser writes "8:30"
/// and "8:05"), "thirty", "forty five", "fünfundvierzig", "oh five". Their value and how many
/// words they are. Never the number of a span that follows ("… twenty minutes").
fn minutes_after(w: &[&str], at: usize) -> Option<(u32, usize)> {
    let word = *w.get(at)?;
    let next = w.get(at + 1).copied().unwrap_or("");
    let one = |word: &str| number_word(word).filter(|n| (1..=9).contains(n) && !matches!(word, "a" | "an") && !word.chars().all(|c| c.is_ascii_digit()));
    if matches!(word, "oh" | "o") {
        return one(next).map(|m| (m as u32, 2));
    }
    let first = number_word(word)?;
    let (minutes, words) = if word.chars().all(|c| c.is_ascii_digit()) {
        if word.len() != 2 {
            return None;
        }
        (first, 1)
    } else if first >= 20 && first % 10 == 0 && one(next).is_some() {
        (first + one(next)?, 2)
    } else if first >= 10 {
        (first, 1)
    } else {
        return None;
    };
    let after = w.get(at + words).copied().unwrap_or("");
    (minutes < 60 && unit_secs(after).is_none()).then_some((minutes as u32, words))
}

/// The time of day said from `w[i]` on ("eight", "8 30", "half past eight", "quarter to nine",
/// "ten past eight", "halb neun", "viertel vor neun", "20 Uhr 15", "8 pm", "eight in the
/// evening", "noon", "Mitternacht"), and the word after it.
fn time_said(w: &[&str], i: usize) -> Option<(Said, usize)> {
    let at = |k: usize| w.get(k).copied().unwrap_or("");
    let named = |hour: u32, to: usize| Some((Said { hour, minute: 0, late: None, exact: true }, to));
    match at(i) {
        "noon" | "midday" | "mittag" | "mittags" => return named(12, i + 1),
        "midnight" | "mitternacht" => return named(0, i + 1),
        // "830", "2015": the hour and the minutes written as one number.
        glued if (3..=4).contains(&glued.len()) && glued.chars().all(|c| c.is_ascii_digit()) => {
            let n: u32 = glued.parse().ok()?;
            let (hour, minute) = (n / 100, n % 100);
            return (hour <= 23 && minute < 60).then_some((Said { hour, minute, late: None, exact: hour == 0 || hour > 12 }, i + 1));
        }
        _ => {}
    }
    // The minutes that stand before the hour, and whether they count back from it.
    let (minute, back, hour_from): (u32, bool, usize) = match (at(i), at(i + 1), at(i + 2)) {
        ("half", "past", _) => (30, false, i + 2),
        // ("Half eight" is half past eight to an Englishman, "halb acht" half past seven.)
        ("half", ..) => (30, false, i + 1),
        ("halb", ..) => (30, true, i + 1),
        // ("Dreiviertel neun" is a quarter to nine.)
        ("dreiviertel", ..) => (15, true, i + 1),
        ("a", "quarter", "past" | "after") => (15, false, i + 3),
        ("a", "quarter", "to" | "before") => (15, true, i + 3),
        ("quarter" | "viertel", "past" | "after" | "nach", _) => (15, false, i + 2),
        ("quarter" | "viertel", "to" | "before" | "vor", _) => (15, true, i + 2),
        (m, "past" | "after" | "nach", _) if number_word(m).is_some_and(|m| (1..60).contains(&m)) => (number_word(m)? as u32, false, i + 2),
        (m, "to" | "before" | "vor", _) if number_word(m).is_some_and(|m| (1..60).contains(&m)) => (number_word(m)? as u32, true, i + 2),
        (m, "minutes" | "minuten", "past" | "after" | "nach") if number_word(m).is_some_and(|m| (1..60).contains(&m)) => (number_word(m)? as u32, false, i + 3),
        (m, "minutes" | "minuten", "to" | "before" | "vor") if number_word(m).is_some_and(|m| (1..60).contains(&m)) => (number_word(m)? as u32, true, i + 3),
        _ => (0, false, i),
    };
    let (mut hour, mut late, mut to) = hour_at(w, hour_from)?;
    let mut minute = minute;
    let mut exact = hour == 0 || hour > 12;
    if back {
        // "Quarter to nine", "halb neun": the hour before the one that is named.
        hour = if hour == 0 { 23 } else { hour - 1 };
        minute = 60 - minute;
        if hour == 0 && !exact {
            hour = 12;
        }
    }
    // "8 o'clock", "8 Uhr", then the minutes when they were not said before the hour: "8 30",
    // "eight thirty", "20 Uhr 15", "eight oh five".
    if at(to) == "o" && at(to + 1) == "clock" {
        to += 2;
    } else if matches!(at(to), "oclock" | "uhr" | "h") {
        to += 1;
    }
    if hour_from == i {
        if let Some((minutes, words)) = minutes_after(w, to) {
            minute = minutes;
            to += words;
        }
    }
    // The half of the day: "pm", "p.m.", "in the evening", "abends", "am Morgen".
    let (half, words): (Option<bool>, usize) = match (at(to), at(to + 1), at(to + 2)) {
        ("am", "morgen" | "vormittag", _) => (Some(false), 2),
        ("am", "abend" | "nachmittag", _) => (Some(true), 2),
        ("in", "the", "morning") | ("in", "der", "früh" | "frueh") => (Some(false), 3),
        ("in", "the", "evening" | "afternoon") => (Some(true), 3),
        ("at", "night", _) => (Some(hour >= 6 && hour != 12), 2),
        ("a", "m", _) => (Some(false), 2),
        ("p", "m", _) => (Some(true), 2),
        ("this", "morning", _) | ("tomorrow", "morning", _) | ("morgen", "früh" | "frueh", _) => (Some(false), 2),
        ("this", "evening" | "afternoon", _) | ("heute", "abend" | "nachmittag", _) => (Some(true), 2),
        ("am" | "morgens" | "früh" | "frueh" | "vormittags", ..) => (Some(false), 1),
        ("pm" | "tonight" | "abends" | "nachmittags", ..) => (Some(true), 1),
        ("nachts", ..) => (Some(hour >= 6 && hour != 12), 1),
        _ => (None, 0),
    };
    if half.is_some() {
        late = half;
        to += words;
    }
    if hour > 23 && !(hour == 24 && minute == 0) {
        return None;
    }
    exact |= hour == 24;
    Some((Said { hour: hour % 24, minute, late, exact }, to))
}

/// The end of a lock that was given as a time of day ("until eight", "till 8:30", "bis halb
/// neun", "until 8 pm", "bis 20 Uhr"): the hour (0-23) and minute, and the words it stands in.
/// `now`: seconds since midnight. An hour on the twelve-hour dial with no half of the day to it
/// means the next time the clock shows it: "until eight" at 18:12 is 20:00, at 21:00 it is
/// 8:00 tomorrow.
fn until_in(w: &[&str], now: u32) -> Option<((u32, u32), Range<usize>)> {
    let at = w.iter().position(|x| matches!(*x, "until" | "till" | "til" | "bis"))?;
    let mut from = at + 1;
    let mut hint: Option<bool> = None;
    loop {
        match w.get(from).copied() {
            Some("um" | "at" | "about" | "around" | "gegen" | "ca" | "circa" | "zirka" | "exactly" | "genau" | "today" | "heute") => {}
            Some("tomorrow" | "morgen" | "früh" | "frueh") => hint = Some(false),
            Some("tonight" | "abend") => hint = Some(true),
            _ => break,
        }
        from += 1;
    }
    let (said, to) = time_said(w, from)?;
    let hour = match (said.exact, said.late.or(hint.filter(|_| !said.exact))) {
        (true, _) => said.hour,
        (false, Some(true)) => said.hour % 12 + 12,
        (false, Some(false)) => said.hour % 12,
        // Whichever of the two the clock shows next.
        (false, None) => {
            let early = said.hour % 12;
            if time_until(now, early, said.minute) <= time_until(now, early + 12, said.minute) { early } else { early + 12 }
        }
    };
    Some(((hour, said.minute), at..to))
}

/// "Hör zehn Minuten nicht zu", "hör mal bis acht nicht zu", "hör mir bitte kurz nicht zu",
/// "hör jetzt auf zuzuhören": "hör … nicht (mehr) zu" or "hör … auf zuzuhören" with nothing in
/// between but the lock's end (`span`: the words of a span or a time of day that was found)
/// and words that say nothing. ("Hör dem Lehrer nicht zu" is advice to someone else, "ich höre
/// nicht auf zuzuhören" a promise.)
fn german_pause(w: &[&str], span: &[Range<usize>]) -> bool {
    const SAYS_NOTHING: &[&str] = &["mir", "uns", "mal", "bitte", "jetzt", "kurz", "für", "fuer", "die", "den", "nächsten", "naechsten", "nächste", "naechste", "lang", "erstmal", "einmal", "eben", "einfach", "dann", "hier", "ab", "sofort", "noch"];
    const ENDS: [&[&str]; 5] = [&["nicht", "mehr", "zu"], &["nicht", "zu"], &["auf", "zuzuhören"], &["auf", "zuzuhoeren"], &["auf", "zu", "zuhören"]];
    let Some(verb) = w.iter().position(|x| matches!(*x, "hör" | "hoer" | "höre" | "hoere")) else { return false };
    let Some(end) = ENDS.iter().find(|tail| w.ends_with(tail)).map(|tail| w.len() - tail.len()) else { return false };
    end > verb && (verb + 1..end).all(|i| SAYS_NOTHING.contains(&w[i]) || span.iter().any(|words| words.contains(&i)))
}

/// The lock command in `text`, if any. Never one about the Discord call ("stop listening to the
/// call" ends the call bridge instead).
pub fn parse(text: &str) -> Option<LockCommand> {
    parse_at(text, seconds_of_day_now())
}

/// `parse` at a time of day (`now`: seconds since midnight; "until eight" depends on it).
pub(crate) fn parse_at(text: &str, now: u32) -> Option<LockCommand> {
    // (The question holds the words that lift the lock, "listen again": it lifts nothing.)
    if asks_how_long(text) {
        return None;
    }
    let s = words(text);
    if is_resume_phrase(&s) {
        return Some(LockCommand::Resume);
    }
    if ["call", "discord", "anruf", "the others", "my friends"].iter().any(|w| s.contains(w)) {
        return None;
    }
    let stop = [
        "stop listening",
        "don t listen",
        "dont listen",
        "do not listen",
        "mute my microphone",
        "mute the microphone",
        "mute your microphone",
        "mute microphone",
        "mute my mic",
        "mute the mic",
        "mute your mic",
        "mute mic",
        "turn off the mic",
        "turn off my mic",
        "turn off your mic",
        "turn the mic off",
        "turn my mic off",
        "turn off the microphone",
        "turn off my microphone",
        "turn the microphone off",
        "turn my microphone off",
        "mikro aus",
        "mikrofon aus",
        "mikro stumm",
        "mikrofon stumm",
        "hör auf zuzuhören",
        "hoer auf zuzuhoeren",
        "hör auf zu zuhören",
        "hör nicht zu",
        "hoer nicht zu",
        "hör nicht mehr zu",
        "hoer nicht mehr zu",
        "hör mir nicht mehr zu",
        "hör mir nicht zu",
        "nicht mehr zuhören",
    ];
    // (Whole words only: "mic off" is no part of "mic offline".)
    let spaced = format!(" {s} ");
    let off = [" mic off ", " microphone off "].iter().any(|p| spaced.contains(p));
    let w: Vec<&str> = s.split(' ').filter(|x| !x.is_empty()).collect();
    // The end Sir gave it: a span ("for ten minutes") or a time of day ("until eight").
    let span = span_in(&w);
    let until = until_in(&w, now);
    let given: Vec<Range<usize>> = span.iter().map(|(_, words)| words.clone()).chain(until.iter().map(|(_, words)| words.clone())).collect();
    // "Hör für 2 Minuten nicht zu", "hör zehn Minuten nicht zu", "hör bis acht nicht zu": the
    // German pause wraps its end.
    let wrapped = (s.starts_with("hör für") || s.contains(" hör für") || s.starts_with("hoer für")) && s.ends_with("nicht zu");
    if !stop.iter().any(|p| s.contains(p)) && !off && !wrapped && !german_pause(&w, &given) {
        return None;
    }
    Some(match (span, until) {
        (Some((length, _)), _) => LockCommand::Pause(length),
        (None, Some(((hour, minute), _))) => LockCommand::Until(hour, minute),
        (None, None) => LockCommand::Lock,
    })
}

/// What he says as he goes quiet / comes back.
pub fn reply(cmd: LockCommand) -> String {
    match cmd {
        // (Whole hours and the long spans in the words Sir would use; "for 5 minutes" as ever.)
        LockCommand::Pause(d) if d.as_secs() >= 3600 => format!("Understood, Sir. I'll stop listening for {}.", span_words(d.as_secs())),
        LockCommand::Pause(d) if d.as_secs() >= 120 => format!("Understood, Sir. I'll stop listening for {} minutes.", d.as_secs() / 60),
        LockCommand::Pause(d) if d.as_secs() >= 60 => "Understood, Sir. I'll stop listening for a minute.".into(),
        LockCommand::Pause(d) => format!("Understood, Sir. I'll stop listening for {} seconds.", d.as_secs()),
        // (Written as the clock shows it; his voice says "eight P M".)
        LockCommand::Until(hour, minute) => format!("Understood, Sir. I'll stop listening until {hour}:{minute:02}."),
        LockCommand::Lock => "Understood, Sir. I'm not listening until you tell me I can listen again.".into(),
        LockCommand::Resume => "I'm listening again, Sir.".into(),
    }
}

#[cfg(test)]
pub(crate) fn test_lock() -> std::sync::MutexGuard<'static, ()> {
    static L: Mutex<()> = Mutex::new(());
    L.lock().unwrap_or_else(|e| e.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phrases_pause_lock_and_resume_in_english_and_german() {
        assert_eq!(parse("JAI, stop listening for a minute"), Some(LockCommand::Pause(Duration::from_secs(60))));
        assert_eq!(parse("Stop listening for 5 minutes"), Some(LockCommand::Pause(Duration::from_secs(300))));
        assert_eq!(parse("Hör für 2 Minuten nicht zu"), Some(LockCommand::Pause(Duration::from_secs(120))));
        assert_eq!(parse("stop listening for thirty seconds"), Some(LockCommand::Pause(Duration::from_secs(30))));
        for lock in ["JAI, stop listening", "Mute my microphone", "Don't listen", "Hör auf zuzuhören"] {
            assert_eq!(parse(lock), Some(LockCommand::Lock), "{lock}");
        }
        // ("Start listening" once went on to the app launcher, with him still paused.)
        for resume in ["You can listen again", "JAI, you can now listen again", "Start listening again", "Start listening", "Keep listening", "Resume listening", "Unmute microphone", "Du kannst wieder zuhören", "Fang wieder an zuzuhören", "JAI hör wieder zu"] {
            assert_eq!(parse(resume), Some(LockCommand::Resume), "{resume}");
            assert!(is_resume_phrase(resume));
        }
        // The Discord bridge keeps its own phrase.
        assert_eq!(parse("stop listening to the call"), None);
        assert_eq!(parse("what's the weather"), None);
    }

    /// Repro (route reader, 2026-11-07): "unmute my mic" holds "mute my mic", so it locked him.
    #[test]
    fn the_microphone_goes_on_and_off_as_said() {
        for off in ["Mute my mic", "Mute the mic", "Mute the microphone", "Turn off the mic", "Turn the microphone off", "Mikrofon aus", "Mikro stumm"] {
            assert_eq!(parse(off), Some(LockCommand::Lock), "{off}");
        }
        for on in ["Unmute my mic", "Unmute the mic", "Unmute your microphone", "Turn on the mic", "Turn the microphone back on", "Mikrofon an", "Mikro wieder an"] {
            assert_eq!(parse(on), Some(LockCommand::Resume), "{on}");
            assert!(is_resume_phrase(on), "{on} must get through the lock");
        }
    }

    #[test]
    fn half_an_hour_is_half_an_hour() {
        assert_eq!(parse("Stop listening for half an hour"), Some(LockCommand::Pause(Duration::from_secs(1800))));
        assert_eq!(parse("Hör für eine halbe Stunde nicht zu"), Some(LockCommand::Pause(Duration::from_secs(1800))));
        assert_eq!(parse("Stop listening for an hour"), Some(LockCommand::Pause(Duration::from_secs(3600))));
        assert_eq!(parse("Stop listening for twenty minutes"), Some(LockCommand::Pause(Duration::from_secs(1200))));
    }

    #[test]
    fn while_locked_only_the_resume_phrase_gets_through() {
        let _g = test_lock();
        apply(LockCommand::Lock);
        assert!(is_locked());
        assert!(status_line().unwrap().contains("YOU CAN LISTEN AGAIN"));
        for ignored in ["JAI", "JAI, what time is it?", "JAI, stop listening"] {
            assert!(!is_resume_phrase(ignored), "{ignored} must be ignored");
        }
        assert!(is_resume_phrase("JAI, you can listen again"));
        // The microphone path's one decision: under the lock only the lifting words pass,
        // whatever the microphone's switch says.
        for mic_on in [true, false] {
            assert_eq!(verdict("JAI, what time is it?", mic_on), Verdict::Ignored);
            assert_eq!(verdict("JAI, you can listen again", mic_on), Verdict::Resumes);
        }
        apply(LockCommand::Resume);
        assert!(!is_locked() && status_line().is_none());
        // Unlocked: heard with the microphone on (or the push-to-talk key held); words that
        // were recorded just before it was switched off are dropped.
        assert_eq!(verdict("JAI", true), Verdict::Heard);
        assert_eq!(verdict("JAI", false), Verdict::Ignored);
        assert_eq!(verdict("JAI, you can listen again", false), Verdict::Ignored);
        // A timed pause runs out on its own; the status counts down.
        apply(LockCommand::Pause(Duration::from_millis(60)));
        assert!(matches!(state(), ListeningLockState::PausedUntil(_)));
        assert!(status_line().unwrap().starts_with("LISTENING PAUSED (0:00)"));
        std::thread::sleep(Duration::from_millis(80));
        assert_eq!(state(), ListeningLockState::Listening);
        assert_eq!(reply(LockCommand::Pause(Duration::from_secs(300))), "Understood, Sir. I'll stop listening for 5 minutes.");
    }

    fn pause(secs: u64) -> Option<LockCommand> {
        Some(LockCommand::Pause(Duration::from_secs(secs)))
    }

    /// Sir, 2026-11-13: a "don't listen" with an end, in the words he would use.
    #[test]
    fn a_lock_is_given_an_end_in_many_words() {
        for (said, want) in [
            ("Don't listen for ten minutes", pause(600)),
            ("Stop listening for an hour", pause(3600)),
            ("Hör zehn Minuten nicht zu", pause(600)),
            ("Hör 10 Minuten nicht zu.", pause(600)),
            ("Mic off for 20 minutes", pause(1200)),
            ("JAI, mic off for twenty minutes please", pause(1200)),
            ("Hör mir bitte für die nächsten 15 Minuten nicht mehr zu", pause(900)),
            ("Stop listening for forty five minutes", pause(2700)),
            ("Stop listening for six minutes", pause(360)),
            ("Stop listening for an hour and a half", pause(5400)),
            ("Stop listening for two and a half hours", pause(9000)),
            ("Don't listen for 1 hour and 30 minutes", pause(5400)),
            ("Hör anderthalb Stunden nicht zu", pause(5400)),
            ("Hör fünfundvierzig Minuten nicht zu", pause(2700)),
            ("Stop listening for a quarter of an hour", pause(900)),
            ("Stop listening for a couple of minutes", pause(120)),
            ("Mute my mic for the next five minutes", pause(300)),
            // No end that he can count: until Sir lets him listen again.
            ("Stop listening for a while", Some(LockCommand::Lock)),
            ("Hör mal kurz nicht zu", Some(LockCommand::Lock)),
            ("Okay, hör jetzt auf zuzuhören", Some(LockCommand::Lock)),
            ("Hör für zehn Minuten auf zuzuhören", pause(600)),
            ("Mic off", Some(LockCommand::Lock)),
            ("Could you turn your mic off for now", Some(LockCommand::Lock)),
            ("Don't listen until I say so", Some(LockCommand::Lock)),
            // Advice to someone else, the opposite order, a promise, and a mic that is merely
            // spoken of.
            ("Hör dem Lehrer nicht zu", None),
            ("Hör nicht auf zuzuhören", None),
            ("Ich höre nicht auf zuzuhören", None),
            ("Hör zehn Minuten Musik", None),
            ("My mic was off the whole time", None),
            ("The mic offline thing again", None),
        ] {
            assert_eq!(parse(said), want, "{said}");
        }
        // The spans other orders borrow from here ("mute Discord for …", the sleep timer, guests).
        for (span, secs) in [("a minute", 60), ("30 seconds", 30), ("half an hour", 1800), ("eine halbe stunde", 1800), ("twenty five minutes", 1500), ("2 hours", 7200), ("einer viertelstunde", 900), ("zweieinhalb stunden", 9000), ("an hour and twenty minutes", 4800)] {
            assert_eq!(duration_in(span), Some(Duration::from_secs(secs)), "{span}");
        }
        assert_eq!(duration_in("the last hour"), None);
        assert_eq!(duration_in("soon"), None);
        assert_eq!(duration_in("3 seconds"), Some(Duration::from_secs(5)), "never shorter than five seconds");
        assert_eq!(duration_in("3 days of 30 hours"), Some(Duration::from_secs(24 * 3600)), "never longer than a day");
    }

    #[test]
    fn until_a_time_of_day_is_the_next_time_the_clock_shows_it() {
        let at = |hour: u32, minute: u32| hour * 3600 + minute * 60;
        let until = |hour: u32, minute: u32| Some(LockCommand::Until(hour, minute));
        // A quarter past six in the evening: "eight" is this evening's.
        for (said, want) in [
            ("Don't listen until eight", until(20, 0)),
            ("Hör bis acht nicht zu", until(20, 0)),
            ("Hör bis um acht Uhr nicht mehr zu", until(20, 0)),
            ("Stop listening until 8:30", until(20, 30)),
            ("Stop listening till half past eight", until(20, 30)),
            ("Hör bis halb neun nicht zu", until(20, 30)),
            ("Don't listen until quarter to nine", until(20, 45)),
            ("Hör bis Viertel vor neun nicht zu", until(20, 45)),
            ("Don't listen until ten past eight", until(20, 10)),
            ("Don't listen until eight thirty", until(20, 30)),
            ("Don't listen until eight o'clock", until(20, 0)),
            ("Mic off until 8 p.m.", until(20, 0)),
            ("Mic off until 8pm", until(20, 0)),
            ("Don't listen until eight in the morning", until(8, 0)),
            ("Don't listen until 7 am", until(7, 0)),
            ("Hör bis 20 Uhr nicht zu", until(20, 0)),
            ("Hör bis 20 Uhr 15 nicht zu", until(20, 15)),
            ("Hör bis morgen früh um sieben nicht zu", until(7, 0)),
            ("Stop listening until noon", until(12, 0)),
            ("Stop listening until midnight", until(0, 0)),
            ("Don't listen until seven", until(19, 0)),
            ("Don't listen until six", until(6, 0)),
            // No time of day: until Sir says so.
            ("Don't listen until tomorrow", Some(LockCommand::Lock)),
            ("Don't listen until I am back", Some(LockCommand::Lock)),
        ] {
            assert_eq!(parse_at(said, at(18, 15)), want, "{said}");
        }
        // Late in the evening it is tomorrow morning's, and just before eight this morning's.
        assert_eq!(parse_at("Don't listen until eight", at(21, 0)), until(8, 0));
        assert_eq!(parse_at("Don't listen until eight", at(7, 50)), until(8, 0));
        assert_eq!(parse_at("Don't listen until eight", at(8, 0)), until(20, 0), "at eight sharp it is the next eight");
        // A span wins over a time when both are said.
        assert_eq!(parse_at("Don't listen until eight, or for ten minutes", at(18, 15)), pause(600));
        // How long that is from now.
        assert_eq!(time_until(at(18, 15), 20, 0), Duration::from_secs(105 * 60));
        assert_eq!(time_until(at(21, 0), 8, 0), Duration::from_secs(11 * 3600));
        assert_eq!(time_until(at(8, 0), 8, 0), Duration::from_secs(24 * 3600));
        assert_eq!(time_until(at(23, 59) + 30, 0, 0), Duration::from_secs(30));
        assert_eq!(reply(LockCommand::Until(20, 0)), "Understood, Sir. I'll stop listening until 20:00.");
        assert_eq!(reply(LockCommand::Until(8, 5)), "Understood, Sir. I'll stop listening until 8:05.");
    }

    #[test]
    fn a_lock_with_an_end_lifts_itself_and_says_so_once() {
        let _g = test_lock();
        apply(LockCommand::Pause(Duration::from_millis(60)));
        assert!(remaining().is_some_and(|left| left <= Duration::from_millis(60)));
        assert!(!take_lapsed(), "it still holds");
        std::thread::sleep(Duration::from_millis(80));
        // (Whoever asks first finds it over; the app is still told.)
        assert!(!is_locked());
        assert_eq!(remaining(), None);
        assert!(take_lapsed(), "it ran out by itself");
        assert!(!take_lapsed(), "said once");
        // Lifted by Sir before its time: nothing is left to say.
        apply(LockCommand::Pause(Duration::from_millis(60)));
        apply(LockCommand::Resume);
        std::thread::sleep(Duration::from_millis(80));
        assert!(!take_lapsed());
        // A lock without an end has no time left to show and never lapses.
        apply(LockCommand::Lock);
        assert_eq!(remaining(), None);
        assert!(!take_lapsed() && is_locked());
        // "Until" a time of day: two minutes from now by the clock on the wall.
        let soon = (seconds_of_day_now() / 60 + 2) % (24 * 60);
        apply(LockCommand::Until(soon / 60, soon % 60));
        let left = remaining().expect("a lock with an end");
        assert!(left > Duration::from_secs(55) && left <= Duration::from_secs(120), "{left:?}");
        assert!(status_line().is_some_and(|line| line.starts_with("LISTENING PAUSED (1:") || line.starts_with("LISTENING PAUSED (0:5") || line.starts_with("LISTENING PAUSED (2:00)")));
        apply(LockCommand::Resume);
        assert!(!is_locked() && remaining().is_none());
    }

    #[test]
    fn how_long_is_answered_and_the_question_lifts_nothing() {
        let _g = test_lock();
        let asked = [
            "How long until you listen again?",
            "JAI, how long until you can listen again?",
            "How much longer until you listen again",
            "How long till you start listening again?",
            "How long will you not listen?",
            "How much longer are you not listening?",
            "How long is the mic off?",
            "When will you listen again?",
            "When are you listening again?",
            "When does the mic come back on?",
            "Wie lange hörst du noch nicht zu?",
            "Wie lange noch, bis du wieder zuhörst?",
            "Wann hörst du wieder zu?",
            "Wie lange ist das Mikro noch aus?",
        ];
        for said in asked {
            assert!(asks_how_long(said), "{said}");
            // (It holds "listen again", the words that lift the lock: it is no order.)
            assert_eq!(parse(said), None, "{said}");
        }
        for other in ["You can listen again", "Listen again", "How long is the film?", "How long until the timer rings?", "When will you be ready?", "JAI, are you there?", "JAI, what time is it?", "How long did I sleep?"] {
            assert!(!asks_how_long(other), "{other}");
        }
        // Under the lock the question is answered and the lock stays.
        apply(LockCommand::Lock);
        for said in asked {
            assert_eq!(verdict(said, true), Verdict::Asks, "{said}");
        }
        assert!(is_locked());
        assert_eq!(how_long(), "Until you tell me I can listen again, Sir.");
        // A plain question lifts nothing, even with the lifting words in it.
        assert_eq!(verdict("Are you able to listen again?", true), Verdict::Ignored);
        assert_eq!(verdict("JAI, are you there?", true), Verdict::Ignored);
        assert_eq!(verdict("You can listen again.", true), Verdict::Resumes);
        apply(LockCommand::Pause(Duration::from_secs(300)));
        assert!(matches!(how_long().as_str(), "Another 5 minutes, Sir."), "{}", how_long());
        assert_eq!(spoken_state(), "I've stopped listening for 5 more minutes, Sir.");
        apply(LockCommand::Pause(Duration::from_secs(2 * 3600)));
        assert_eq!(how_long(), "Another 2 hours, Sir.");
        assert_eq!(spoken_state(), "I've stopped listening for another 2 hours, Sir.");
        assert!(status_line().is_some_and(|line| line.starts_with("LISTENING PAUSED (1:59:5") || line.starts_with("LISTENING PAUSED (2:00:00)")), "{:?}", status_line());
        apply(LockCommand::Pause(Duration::from_secs(40)));
        assert!(how_long().starts_with("Another 40 seconds") || how_long().starts_with("Another 39 seconds"), "{}", how_long());
        apply(LockCommand::Resume);
        // While he listens it is an ordinary question with an ordinary answer.
        assert_eq!(verdict("How long until you listen again?", true), Verdict::Heard);
        assert_eq!(how_long(), "I'm listening now, Sir.");
        // What is left, in words.
        for (secs, words) in [(1, "a second"), (59, "59 seconds"), (60, "a minute"), (61, "2 minutes"), (3600, "an hour"), (6460, "1 hour and 48 minutes"), (3660, "1 hour and 1 minute"), (7200, "2 hours")] {
            assert_eq!(span_words(secs), words, "{secs}");
        }
        assert_eq!(reply(LockCommand::Pause(Duration::from_secs(3600))), "Understood, Sir. I'll stop listening for an hour.");
        assert_eq!(reply(LockCommand::Pause(Duration::from_secs(5400))), "Understood, Sir. I'll stop listening for 1 hour and 30 minutes.");
        assert_eq!(reply(LockCommand::Pause(Duration::from_secs(1200))), "Understood, Sir. I'll stop listening for 20 minutes.");
    }

    /// Sir, 2026-11-13: "when I tell JAI not to listen then the subtitles should [not] show
    /// except if I explicitly tell it to".
    #[test]
    fn what_he_hears_is_not_shown_under_the_lock_unless_sir_asks() {
        let _g = test_lock();
        apply(LockCommand::Resume);
        assert!(may_show_heard());
        // Asked while he listens: that is the settings' affair, and no word about a lock to come.
        show_asked();
        apply(LockCommand::Lock);
        assert!(!may_show_heard(), "nothing heard is shown while he is not listening");
        show_asked();
        assert!(may_show_heard(), "Sir asked for it under the lock");
        // The lock ends: everything is as his settings say, and the next lock hides again.
        apply(LockCommand::Resume);
        assert!(may_show_heard());
        apply(LockCommand::Pause(Duration::from_millis(60)));
        assert!(!may_show_heard());
        show_asked();
        std::thread::sleep(Duration::from_millis(80));
        assert!(may_show_heard(), "the lock ran out");
        apply(LockCommand::Lock);
        assert!(!may_show_heard(), "what was asked under one lock is asked anew under the next");
        apply(LockCommand::Resume);
    }
}
