//! The Stark Executive Briefing ("Good morning, JAI", "brief me", "update me on my day", "was
//! steht heute an?"): greeting, time and date, then — gathered concurrently — cancelled classes
//! and room changes (WebUntis), new uploads and due dates (itslearning), the day's appointments
//! with clashes (Apple Calendar), unread e-mail, the weather at Sir's home, today's reminders,
//! system health and the top headlines. Delivered as one short spoken report, with the details
//! in a "Daily Briefing Summary" card in the HUD's widgets panel. Services that are not
//! configured are simply left out.

use crate::lms_client::CourseUpdate;
use crate::pim_config::{EMAIL, LMS, WEBUNTIS};
use crate::reminders::Reminder;
use crate::tools::ToolContext;
use crate::webuntis_client::{Lesson, LessonStatus};
use chrono::{Datelike, Duration as ChronoDuration, Local, NaiveDate, Timelike};
use regex::Regex;
use serde_json::{json, Value};
use std::sync::LazyLock;
use std::time::Duration;
use tracing::warn;

/// Default news feed (configurable as `briefing_news_feed`).
pub const DEFAULT_FEED: &str = "https://feeds.bbci.co.uk/news/world/rss.xml";

/// The parts of the briefing Sir can leave out and put back, with the words he names them by.
pub const PARTS: &[(&str, &[&str])] = &[
    ("weather", &["weather", "forecast", "sun", "wetter"]),
    ("school", &["school", "timetable", "lessons", "classes", "webuntis", "schule", "stundenplan", "unterricht"]),
    ("itslearning", &["itslearning", "homework", "assignments", "hausaufgaben", "aufgaben"]),
    ("calendar", &["calendar", "appointments", "events", "kalender", "termine"]),
    ("email", &["email", "emails", "e mail", "e mails", "mail", "mails", "inbox"]),
    ("news", &["news", "headlines", "nachrichten", "schlagzeilen"]),
    ("games", &["free games", "games", "game deals", "wishlist", "sales", "spiele"]),
    ("reminders", &["reminders", "erinnerungen"]),
    ("system", &["system", "system status", "pc", "computer"]),
];

static PARTS_OUT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(?:(?:leave|take|keep|drop|skip|remove|cut)\s+(?P<a>.+?)\s+(?:out of|out from|from|off)\s+(?:my|the)\s+(?:daily\s+|morning\s+)?briefing|(?:don'?t|do not|stop)\s+(?:tell|telling|mention|mentioning|read|reading)(?:\s+me)?(?:\s+about)?\s+(?P<b>.+?)\s+in\s+(?:my|the)\s+briefing|no\s+(?:more\s+)?(?P<c>.+?)\s+in\s+(?:my|the)\s+briefing|lass\s+(?P<d>.+?)\s+(?:im|aus dem|aus meinem)\s+briefing\s+weg)$").expect("briefing parts out")
});
static PARTS_IN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(?:(?:put|bring|add|take)\s+(?P<a>.+?)\s+back\s+(?:in|into|to)(?:\s+(?:my|the)\s+briefing)?|(?:add|include)\s+(?P<b>.+?)\s+(?:in|into|to)\s+(?:my|the)\s+briefing|nimm\s+(?P<c>.+?)\s+wieder\s+(?:ins|in das|in mein)\s+briefing(?:\s+auf)?)$").expect("briefing parts in")
});

/// "Leave the news and the weather out of my briefing" → {"leave_out": ["news", "weather"]};
/// "put the free games back in" → {"put_back": ["games"]}; "lass das Wetter im Briefing weg".
/// None when a named part is not one of `PARTS` (`s`: normalised, lower case).
pub fn part_order(s: &str) -> Option<Value> {
    if matches!(s, "what is in my briefing" | "whats in my briefing" | "what does my briefing tell me" | "what does my briefing cover") {
        return Some(json!({ "parts": "list" }));
    }
    let (key, caps) = match (PARTS_OUT.captures(s), PARTS_IN.captures(s)) {
        (Some(c), _) => ("leave_out", c),
        (None, Some(c)) => ("put_back", c),
        _ => return None,
    };
    let what = ["a", "b", "c", "d"].iter().find_map(|n| caps.name(n))?.as_str();
    let named: Option<Vec<&str>> = what
        .split(|c| c == ',')
        .flat_map(|p| p.split(" and ").flat_map(|p| p.split(" und ")))
        .map(|p| {
            let p = p.split_whitespace().filter(|w| !matches!(*w, "the" | "my" | "your" | "all" | "die" | "das" | "den" | "meine" | "meinen")).collect::<Vec<_>>().join(" ");
            PARTS.iter().find(|(_, words)| words.contains(&p.as_str())).map(|(part, _)| *part)
        })
        .collect();
    let named = named.filter(|n| !n.is_empty())?;
    Some(json!({ key: named }))
}

/// Leaves parts out of the briefing or puts them back (`part_order`'s arguments). What it says.
pub fn set_parts(tools: &ToolContext, args: &Value) -> String {
    let names = |key: &str| -> Vec<String> { args[key].as_array().map(|a| a.iter().filter_map(|v| v.as_str()).filter(|p| PARTS.iter().any(|(k, _)| k == p)).map(str::to_string).collect()).unwrap_or_default() };
    let (out, back) = (names("leave_out"), names("put_back"));
    tools.config.update(|c| {
        c.briefing_skip.retain(|p| !back.contains(p));
        for p in &out {
            if !c.briefing_skip.contains(p) {
                c.briefing_skip.push(p.clone());
            }
        }
    });
    let skip = tools.config.read(|c| c.briefing_skip.clone());
    let told = |list: &[String]| join_and(&list.iter().map(|p| part_name(p).to_string()).collect::<Vec<_>>());
    match (out.is_empty(), back.is_empty()) {
        (false, _) => format!("Very good, Sir: the briefing leaves out {} from now on.", told(&out)),
        (true, false) => format!("Very good, Sir: {} {} back in the briefing.", told(&back), if back.len() == 1 && !back[0].ends_with('s') { "is" } else { "are" }),
        _ if skip.is_empty() => "Your briefing tells everything, Sir.".to_string(),
        _ => format!("Your briefing leaves out {}, Sir.", told(&skip)),
    }
}

fn part_name(part: &str) -> &str {
    match part {
        "school" => "the timetable",
        "itslearning" => "itslearning",
        "calendar" => "the calendar",
        "email" => "e-mail",
        "news" => "the headlines",
        "games" => "the game deals",
        "system" => "the system status",
        "weather" => "the weather",
        "reminders" => "the reminders",
        other => other,
    }
}

pub fn greeting(hour: u32) -> &'static str {
    match hour {
        5..=11 => "Good morning, Sir.",
        12..=17 => "Good afternoon, Sir.",
        18..=22 => "Good evening, Sir.",
        _ => "Burning the midnight oil, Sir?",
    }
}

/// The one thing wrong at start-up, said in place of "all systems are online": a model file
/// that is missing, else a drive with under 5 GB free. None when all is well.
pub fn startup_problem(missing_models: &[&str], disks: &[crate::telemetry::DiskInfo]) -> Option<String> {
    if let Some(label) = missing_models.first() {
        return Some(format!("One thing first: the {label} is missing, so part of me will not work until it is back."));
    }
    let tight = disks.iter().filter(|d| d.total_gb > 0.0 && d.free_gb < 5.0).min_by(|a, b| a.free_gb.total_cmp(&b.free_gb))?;
    Some(format!("One thing first: drive {} has only {:.1} gigabytes free.", tight.mount.trim_end_matches(['\\', '/', ':']), tight.free_gb))
}

/// The start-up line with `problem` in place of the systems report, when there is one.
pub fn startup_line_with(hour: u32, seed: usize, problem: Option<&str>) -> String {
    let line = startup_line(hour, seed);
    match problem {
        // (The greeting is the line up to its first full stop or question mark.)
        Some(problem) => {
            let hello = line.find(['.', '?']).map_or(line.as_str(), |end| &line[..=end]);
            format!("{hello} {problem}")
        }
        None => line,
    }
}

/// The start-up line, different each time: a time-of-day greeting and a systems report, both
/// picked by `seed`.
pub fn startup_line(hour: u32, seed: usize) -> String {
    let hello: &[&str] = match hour {
        5..=11 => &["Good morning, Sir.", "Morning, Sir.", "Good morning, Sir. I trust you slept well."],
        12..=17 => &["Good afternoon, Sir.", "Afternoon, Sir.", "Welcome back, Sir."],
        18..=22 => &["Good evening, Sir.", "Evening, Sir.", "Welcome back, Sir."],
        _ => &["Burning the midnight oil, Sir?", "Working late again, Sir?", "Still up, Sir?"],
    };
    const STATUS: [&str; 6] = [
        "All systems are online.",
        "All systems are up and running.",
        "Every system is online and standing by.",
        "Systems are online. Ready when you are.",
        "Everything is up and running.",
        "I'm online and at your service.",
    ];
    format!("{} {}", hello[seed % hello.len()], STATUS[seed / hello.len() % STATUS.len()])
}

fn ordinal(day: u32) -> String {
    let suffix = match (day % 10, day % 100) {
        (1, 11) | (2, 12) | (3, 13) => "th",
        (1, _) => "st",
        (2, _) => "nd",
        (3, _) => "rd",
        _ => "th",
    };
    format!("{day}{suffix}")
}

fn reminders_on(items: &[Reminder], day: NaiveDate, now: chrono::DateTime<Local>) -> Option<String> {
    let todays: Vec<&Reminder> = items.iter().filter(|r| r.due().date_naive() == day && r.due() > now).collect();
    if todays.is_empty() {
        return None;
    }
    let list = todays.iter().take(4).map(|r| format!("{} at {}", r.text.trim_end_matches('.'), r.due().format("%H:%M"))).collect::<Vec<_>>();
    Some(format!("You have {} reminder{}: {}.", todays.len(), if todays.len() == 1 { "" } else { "s" }, join_and(&list)))
}

pub(crate) fn join_and(items: &[String]) -> String {
    match items {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

fn system_line(tools: &ToolContext) -> String {
    let t = tools.monitor.latest();
    let mut parts = vec![format!("the CPU is at {:.0} percent", t.cpu_usage_pct), format!("memory at {:.0} percent", t.ram_usage_pct)];
    if let Some(temp) = t.gpu.as_ref().and_then(|g| g.temperature_c) {
        parts.push(format!("the graphics card at {temp:.0} degrees"));
    }
    let tight: Vec<String> = t.disks.iter().filter(|d| d.used_pct >= 90.0).map(|d| format!("drive {} is {:.0} percent full", d.mount.trim_end_matches(['\\', '/']), d.used_pct)).collect();
    let health = if t.cpu_usage_pct > 90.0 || t.ram_usage_pct > 90.0 || !tight.is_empty() { "Systems need a little attention" } else { "All systems nominal" };
    let mut line = format!("{health}: {}.", join_and(&parts));
    if !tight.is_empty() {
        line.push_str(&format!(" Note that {}.", join_and(&tight)));
    }
    line
}

#[derive(Clone, Copy, PartialEq)]
enum Period {
    Morning,
    Afternoon,
    Evening,
}

fn period(time_of_day: Option<&str>, hour: u32) -> Period {
    let t = time_of_day.unwrap_or("").to_lowercase();
    if t.contains("even") || t.contains("night") || t.contains("abend") {
        Period::Evening
    } else if t.contains("after") || t.contains("nachmittag") {
        Period::Afternoon
    } else if t.contains("morn") || t.contains("früh") {
        Period::Morning
    } else {
        match hour {
            4..=11 => Period::Morning,
            12..=16 => Period::Afternoon,
            _ => Period::Evening,
        }
    }
}

/// "08:00" → "8", "09:50" → "9:50" (as said aloud).
fn spoken_clock(t: chrono::NaiveTime) -> String {
    if t.minute() == 0 {
        format!("{}", t.hour())
    } else {
        format!("{}:{:02}", t.hour(), t.minute())
    }
}

/// The briefing's weather at his home: "In Musterstadt it's 9 degrees and light rain; today 5 to
/// 13 degrees, rain likely from about 15:00, after your last lesson. The sun sets at 16:52."
/// (`leave` / `back`: his way to school and home on a school day; `sun`: sunrise and sunset.)
#[allow(clippy::too_many_arguments)]
fn weather_line(
    town: Option<&str>,
    w: &crate::location_engine::CurrentWeather,
    hourly: &[crate::location_engine::HourWeather],
    now: chrono::NaiveDateTime,
    day: NaiveDate,
    day_word: &str,
    leave: Option<chrono::NaiveDateTime>,
    back: Option<chrono::NaiveDateTime>,
    sun: Option<(chrono::NaiveTime, chrono::NaiveTime)>,
) -> String {
    let degrees = |t: f64| format!("{:.0}", t.round() + 0.0);
    let sky = crate::location_engine::weather_code_text(w.code);
    let mut line = match town {
        Some(t) => format!("In {t} it's {} degrees and {sky}", degrees(w.temperature)),
        None => format!("It's {} degrees and {sky}", degrees(w.temperature)),
    };
    let hours: Vec<&crate::location_engine::HourWeather> = hourly.iter().filter(|h| h.time.date() == day).collect();
    if !hours.is_empty() {
        let (lo, hi) = hours.iter().fold((f64::MAX, f64::MIN), |(lo, hi), h| (lo.min(h.temperature), hi.max(h.temperature)));
        line.push_str(&format!("; {day_word} {} to {} degrees", degrees(lo), degrees(hi)));
        if let Some(rain) = rain_against_day(&hours, now, leave, back) {
            line.push_str(&format!(", {rain}"));
        }
    }
    line.push('.');
    // (After today's sunset the sun is no news until tomorrow's briefing.)
    match sun {
        Some((rise, _)) if day != now.date() => line.push_str(&format!(" {} the sun rises at {}.", crate::pim::capitalise(day_word), spoken_clock(rise))),
        Some((rise, _)) if now.time() < rise => line.push_str(&format!(" The sun rises at {}.", spoken_clock(rise))),
        Some((_, set)) if now.time() < set => line.push_str(&format!(" The sun sets at {}.", spoken_clock(set))),
        _ => {}
    }
    line
}

/// When rain is likely (50 % and more) in the hours ahead of `day` up to ten at night, set
/// against his way to school (`leave`), his last lesson's end (`back`) and the hours between.
/// None when it stays dry.
fn rain_against_day(hours: &[&crate::location_engine::HourWeather], now: chrono::NaiveDateTime, leave: Option<chrono::NaiveDateTime>, back: Option<chrono::NaiveDateTime>) -> Option<String> {
    let hour = ChronoDuration::hours(1);
    let wet = |at: chrono::NaiveDateTime| hours.iter().any(|h| h.time <= at && at < h.time + hour && h.precip_probability >= 50.0);
    let first = hours.iter().find(|h| h.time + hour > now && h.time.hour() <= 22 && h.precip_probability >= 50.0)?;
    let from = first.time.max(now);
    let clock = |t: chrono::NaiveDateTime| spoken_clock(t.time());
    Some(match (leave, back) {
        (Some(l), _) if l >= now && wet(l) => format!("rain likely around {}, when you leave for school: take an umbrella", clock(l)),
        (Some(l), _) if from < l => format!("rain likely early, around {}, before you leave", clock(from)),
        (_, Some(b)) if b >= now && wet(b) => format!("rain likely around {}, on your way home from school", clock(b)),
        (_, Some(b)) if from < b => format!("rain likely from about {}, while you're at school; dry for the way home", clock(from)),
        (_, Some(_)) => format!("rain likely from about {}, after your last lesson", clock(from)),
        _ => format!("rain likely from about {}", clock(from)),
    })
}

/// The lessons a briefing tells of: the ones still ahead.
#[derive(Debug, Clone, PartialEq)]
struct Ahead {
    /// The day they are on: the briefing's own, or the next one that has lessons.
    day: NaiveDate,
    lessons: Vec<Lesson>,
    /// How many of that day's are over already (and so left out).
    over: usize,
}

/// What is still ahead of `now` among `all` (the briefing's `day` and the days after it): that
/// day's lessons that have not ended; and when none of today's is left, the next day's that has
/// any. (Sir, 2026-11-13, at 14:49 before a list of the morning's lessons: "only tell me about
/// classes that are in the future, not about classes that I already had".)
fn lessons_ahead(all: &[Lesson], day: NaiveDate, now: chrono::NaiveDateTime) -> Ahead {
    let mut sorted: Vec<&Lesson> = all.iter().filter(|l| l.date >= day).collect();
    sorted.sort_by_key(|l| (l.date, l.start));
    let on_day: Vec<&Lesson> = sorted.iter().copied().filter(|l| l.date == day).collect();
    let left: Vec<Lesson> = on_day.iter().copied().filter(|l| l.date > now.date() || l.end > now.time()).cloned().collect();
    if !left.is_empty() || day != now.date() {
        return Ahead { day, over: on_day.len() - left.len(), lessons: left };
    }
    match sorted.iter().copied().find(|l| l.date > day) {
        Some(next) => Ahead { day: next.date, lessons: sorted.iter().copied().filter(|l| l.date == next.date).cloned().collect(), over: 0 },
        None => Ahead { day, lessons: Vec::new(), over: on_day.len() },
    }
}

/// A day as a briefing says it and as a card heads it: ("today", "today"), ("on Monday",
/// "Monday").
fn day_words(day: NaiveDate, today: NaiveDate) -> (String, String) {
    let word = crate::pim::spoken_day(day, today);
    if day == today || day == today + ChronoDuration::days(1) {
        (word.clone(), word)
    } else {
        (format!("on {word}"), word)
    }
}

/// (`some_over`: part of the day's lessons are behind him and left out.)
fn school_lines(lessons: &[Lesson], day_word: &str, some_over: bool) -> (String, Vec<String>) {
    if lessons.is_empty() {
        return if some_over { (format!("No more lessons {day_word}."), vec![format!("No more lessons {day_word}.")]) } else { (format!("No lessons {day_word} on WebUntis."), vec!["No lessons.".into()]) };
    }
    let changes: Vec<String> = lessons
        .iter()
        .filter(|l| l.status != LessonStatus::Regular)
        .map(|l| match l.status {
            LessonStatus::Cancelled => format!("{} at {} is cancelled, a free period", l.display_subject(), spoken_clock(l.start)),
            _ => format!(
                "{} at {} has a substitution{}",
                l.display_subject(),
                spoken_clock(l.start),
                if l.rooms.is_empty() { String::new() } else { format!(" in room {}", l.rooms.join(", ")) }
            ),
        })
        .collect();
    let first = &lessons[0];
    let spoken = if changes.is_empty() && some_over {
        format!(
            "Still ahead {day_word}: {} lesson{}, the next at {} with {}.",
            lessons.len(),
            if lessons.len() == 1 { "" } else { "s" },
            spoken_clock(first.start),
            first.display_subject()
        )
    } else if changes.is_empty() {
        format!(
            "Your timetable {day_word} runs as planned: {} lesson{}, starting at {} with {}.",
            lessons.len(),
            if lessons.len() == 1 { "" } else { "s" },
            spoken_clock(first.start),
            first.display_subject()
        )
    } else {
        format!("On WebUntis {day_word}: {}.", join_and(&changes))
    };
    (spoken, lessons.iter().map(|l| l.line()).collect())
}

fn lms_lines(updates: &[CourseUpdate], day: NaiveDate) -> (Option<String>, Vec<String>) {
    let now = Local::now().naive_local();
    let fresh: Vec<&CourseUpdate> = updates.iter().filter(|u| u.kind != "assignment" && u.when.is_some_and(|w| now - w < ChronoDuration::hours(36))).collect();
    let due: Vec<&CourseUpdate> = updates
        .iter()
        .filter(|u| u.due.is_some_and(|d| d > now && d.date() <= day + ChronoDuration::days(3)))
        .collect();
    let mut details: Vec<String> = fresh.iter().take(6).map(|u| u.line()).collect();
    details.extend(due.iter().map(|u| u.line()));
    let mut parts = Vec::new();
    if let Some(first) = fresh.first() {
        parts.push(format!(
            "{} new item{} on itslearning, including '{}'{}",
            fresh.len(),
            if fresh.len() == 1 { "" } else { "s" },
            crate::english::in_english(&first.title.chars().take(70).collect::<String>()),
            if first.course.is_empty() { String::new() } else { format!(" in {}", crate::english::in_english(&first.course)) }
        ));
    }
    for u in due.iter().take(2) {
        if let Some(d) = u.due {
            parts.push(format!("'{}' is due {} at {}", crate::english::in_english(&u.title.chars().take(60).collect::<String>()), crate::pim::spoken_day(d.date(), now.date()), d.format("%H:%M")));
        }
    }
    let spoken = (!parts.is_empty()).then(|| format!("{}.", crate::pim::capitalise(&join_and(&parts))));
    (spoken, details)
}

/// A lesson as a card of the timetable stack: when and what, then where and with whom. What
/// WebUntis leaves empty is left out (seen live 2026-11-12: a substitution without subject,
/// room or teacher read "09:25 ·" and "Changed: room · , , ,").
fn lesson_note(l: &Lesson) -> crate::hud_board::Note {
    use crate::hud_board::Note;
    let list = |items: &[String]| {
        let mut named: Vec<&str> = items.iter().map(|s| s.trim()).filter(|s| !s.is_empty()).collect();
        named.dedup();
        named.join(", ")
    };
    let (rooms, teachers, note) = (list(&l.rooms), list(&l.teachers), l.note.trim());
    let mut details: Vec<String> = Vec::new();
    if !rooms.is_empty() {
        details.push(format!("Room {rooms}"));
    }
    if !teachers.is_empty() {
        details.push(teachers);
    }
    if !note.is_empty() {
        details.push(crate::english::in_english(note));
    }
    let details = details.join(" · ");
    Note {
        headline: format!("{} – {} · {}", l.start.format("%H:%M"), l.end.format("%H:%M"), l.display_subject()),
        body: match l.status {
            LessonStatus::Cancelled => "Cancelled — free period.".into(),
            LessonStatus::Irregular if details.is_empty() => "Changed (a substitution or another room).".into(),
            LessonStatus::Irregular => format!("Changed: {details}"),
            LessonStatus::Regular if details.is_empty() => "As planned.".into(),
            LessonStatus::Regular => details,
        },
    }
}

/// The next briefing follows a greeting (the catch-up at the start): it does not greet again.
static GREETED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// The next briefing comes right after a greeting: it opens with the time, not with another
/// "Good evening, Sir".
pub fn after_a_greeting() {
    GREETED.store(true, std::sync::atomic::Ordering::Release);
}

/// The lesson that runs now or comes next, within three days: its subject, when it starts and
/// its room. None without a timetable or when none is in reach. (For the glance on the idle
/// screen and on the background; a school server that does not answer is no news there. A
/// scripted session never asks Sir's school.)
pub async fn next_lesson(tools: &ToolContext) -> Option<(String, chrono::NaiveDateTime, String)> {
    let pim = &tools.pim;
    if pim.config().missing(WEBUNTIS).is_some() || crate::platform::in_test_session() {
        return None;
    }
    let now = Local::now().naive_local();
    let all = pim.lessons(now.date(), now.date() + ChronoDuration::days(3)).await.ok()?;
    let ahead = lessons_ahead(&all, now.date(), now);
    let next = ahead.lessons.iter().find(|l| l.status != LessonStatus::Cancelled)?;
    Some((next.display_subject(), next.date.and_time(next.start), next.rooms.first().cloned().unwrap_or_default()))
}

/// The briefing as speakable prose; the details go to the HUD card.
pub async fn executive(tools: &ToolContext, time_of_day: Option<&str>) -> String {
    let now = Local::now();
    let greeted = GREETED.swap(false, std::sync::atomic::Ordering::AcqRel);
    let pim = &tools.pim;
    let when = period(time_of_day, now.hour());
    // The evening briefing looks ahead to tomorrow.
    let day = if when == Period::Evening && now.hour() >= 16 { now.date_naive() + ChronoDuration::days(1) } else { now.date_naive() };
    let day_word = if day == now.date_naive() { "today" } else { "tomorrow" };
    let cfg = pim.config();
    // The parts Sir left out are neither fetched nor told ("leave the news out of my briefing").
    let skip = tools.config.read(|c| c.briefing_skip.clone());
    let on = |part: &str| !skip.iter().any(|p| p == part);
    let (school_on, lms_on, mail_on, cal_on) = (cfg.missing(WEBUNTIS).is_none() && on("school"), cfg.missing(LMS).is_none() && on("itslearning"), cfg.missing(EMAIL).is_none() && on("email"), on("calendar"));
    pim.pulse(&["brain", "briefing"], false);
    for (on, node) in [(school_on, "webuntis"), (lms_on, "itslearning"), (cal_on, "calendar"), (mail_on, "email")] {
        if on {
            pim.pulse(&["brain", "briefing", node], false);
        }
    }
    let feed = if on("news") { tools.config.read(|c| c.briefing_news_feed.clone()) } else { String::new() };
    crate::websearch::set_home_location(crate::websearch::place_from_memories(&tools.memory.list(200).unwrap_or_default()));
    let quick = Duration::from_secs(6);
    let (school, lms, (events, cal_note), mail, weather, news, free) = tokio::join!(
        // (Three days on from today: when today's lessons are over the next school day is what
        // is told, and a weekend may lie in between.)
        async { if school_on { Some(pim.lessons(day, if day == now.date_naive() { day + ChronoDuration::days(3) } else { day }).await) } else { None } },
        async { if lms_on { Some(pim.course_updates(None).await) } else { None } },
        async { if cal_on { pim.schedule(day, day).await } else { (Vec::new(), None) } },
        async { if mail_on { Some(pim.unread_mail(3, None).await) } else { None } },
        // (Where he lives may take the Windows location service a few seconds to say.)
        async {
            if !on("weather") {
                return None;
            }
            match tokio::time::timeout(Duration::from_secs(12), crate::location_engine::home_weather(tools)).await {
                Ok(Ok(w)) => Some(w),
                Ok(Err(e)) => {
                    warn!("[BRIEF] Weather: {e}");
                    None
                }
                Err(_) => None,
            }
        },
        async {
            if feed.trim().is_empty() || when == Period::Evening {
                Vec::new()
            } else {
                tokio::time::timeout(quick, crate::websearch::headlines(&feed, 2)).await.unwrap_or_default()
            }
        },
        async { if on("games") { crate::kit_games::briefing_free_games().await } else { (Vec::new(), None) } }
    );
    // Only what is still ahead: the lessons that have not ended (or the next school day's), the
    // appointments that are not over.
    let ahead = school.as_ref().and_then(|r| r.as_ref().ok()).map(|all| lessons_ahead(all, day, now.naive_local()));
    let appointments = events.len();
    let events: Vec<_> = events.into_iter().filter(|e| e.all_day || day != now.date_naive() || e.end > now.naive_local()).collect();
    // The same picture on the HUD board: timetable, itslearning deadlines and appointments.
    let mut board = board_report(day, day_word, None, lms.as_ref().and_then(|r| r.as_ref().ok()).map(|v| v.as_slice()), &events, now.naive_local());
    if let Some(ahead) = ahead.as_ref().filter(|a| !a.lessons.is_empty() || a.over == 0) {
        let school_board = board_report(ahead.day, &day_words(ahead.day, now.date_naive()).0, Some(&ahead.lessons), None, &[], now.naive_local());
        board.blocks.splice(0..0, school_board.blocks);
    }

    let mut spoken = vec![format!("It's {} on {}, the {} of {}.", crate::brain::spoken_time(now), now.format("%A"), ordinal(now.day()), now.format("%B"))];
    if !greeted {
        spoken.insert(0, greeting(now.hour()).to_string());
    }
    let mut md = vec![format!("## {} briefing — {}", match when { Period::Morning => "Morning", Period::Afternoon => "Afternoon", Period::Evening => "Evening" }, now.format("%A %-d %B, %H:%M"))];

    // A service that failed: told plainly, its fix one click away (`integration_trouble`).
    let mut troubles: Vec<crate::proactive_butler::Card> = Vec::new();
    // In the holidays the school part is one line: until when.
    let holiday =if school_on { crate::webuntis_client::holiday_on(&pim.holidays().await, day).cloned() } else { None };
    match school {
        _ if holiday.is_some() => {
            let line = holiday.as_ref().map(crate::webuntis_client::holiday_line).unwrap_or_default();
            spoken.push(line.clone());
            md.extend(["### School".to_string(), format!("- {line}")]);
        }
        Some(Ok(_)) => {
            let ahead = ahead.clone().unwrap_or(Ahead { day, lessons: Vec::new(), over: 0 });
            let (said, headed) = day_words(ahead.day, now.date_naive());
            let (line, details) = school_lines(&ahead.lessons, &said, ahead.over > 0);
            spoken.push(line);
            md.push(format!("### School ({headed})"));
            md.extend(details.into_iter().map(|d| format!("- {d}")));
        }
        Some(Err(e)) => {
            warn!("[BRIEF] WebUntis: {e}");
            let card = crate::proactive_butler::integration_trouble("WebUntis", &e, None);
            spoken.push(card.body.clone());
            md.extend(["### School".to_string(), format!("- {}", card.body)]);
            troubles.push(card);
        }
        None => {}
    }
    match lms {
        Some(Ok(updates)) => {
            let (line, details) = lms_lines(&updates, day);
            if let Some(line) = line {
                spoken.push(line);
            }
            md.push("### itslearning".into());
            if details.is_empty() {
                md.push("- Nothing new.".into());
            }
            md.extend(details.into_iter().map(|d| format!("- {d}")));
        }
        Some(Err(e)) => {
            warn!("[BRIEF] itslearning: {e}");
            let site = cfg.read(|s| s.lms.base_url.trim().to_string());
            let card = crate::proactive_butler::integration_trouble("itslearning", &e, Some(&site));
            spoken.push(card.body.clone());
            md.extend(["### itslearning".to_string(), format!("- {}", card.body)]);
            troubles.push(card);
        }
        None => {}
    }

    let timed: Vec<_> = events.iter().filter(|e| !e.all_day).collect();
    let calendar = match (events.is_empty(), appointments > 0) {
        _ if !cal_on => None,
        (true, true) => Some((format!("Nothing more on your calendar {day_word}."), vec!["Nothing more scheduled.".to_string()])),
        (true, false) => Some((format!("Your calendar is clear {day_word}."), vec!["Nothing scheduled.".to_string()])),
        (false, _) => {
            // (Its titles in English: `english`.)
            let names: Vec<String> = events
                .iter()
                .take(4)
                .map(|e| crate::english::in_english(&e.title))
                .zip(events.iter())
                .map(|(title, e)| if e.all_day { title } else { format!("{title} at {}", spoken_clock(e.start.time())) })
                .collect();
            let mut said = format!("Your calendar has {} appointment{} {day_word}: {}.", events.len(), if events.len() == 1 { "" } else { "s" }, join_and(&names));
            if let Some((a, b)) = crate::calendar_client::conflicts(&events).first() {
                said.push_str(&format!(" Note that {} and {} overlap.", crate::english::in_english(a), crate::english::in_english(b)));
            }
            let lines = events
                .iter()
                .map(|e| {
                    let clash = timed.iter().any(|o| o.uid != e.uid && o.overlaps(e));
                    format!("{}{}", e.line(), if clash { " [CONFLICT]" } else { "" })
                })
                .collect();
            Some((said, lines))
        }
    };
    if let Some((said, lines)) = calendar {
        spoken.push(said);
        md.push(format!("### Calendar ({day_word})"));
        md.extend(lines.into_iter().map(|l: String| format!("- {l}")));
        md.extend(cal_note.map(|note| format!("- {note}")));
    }

    match mail {
        Some(Ok((count, list))) => {
            md.push("### Email".into());
            if count == 0 {
                spoken.push("No unread e-mail.".into());
                md.push("- Inbox zero.".into());
            } else {
                let latest = list.first().map(|h| format!(", the latest from {} about '{}'", h.from, h.subject.chars().take(60).collect::<String>())).unwrap_or_default();
                spoken.push(format!("You have {count} unread e-mail{}{latest}.", if count == 1 { "" } else { "s" }));
                md.push(format!("- {count} unread"));
                md.extend(list.iter().map(|h| format!("- {} — {}", h.from, h.subject)));
            }
        }
        Some(Err(e)) => {
            warn!("[BRIEF] E-mail: {e}");
            let card = crate::proactive_butler::integration_trouble("Email", &e, None);
            spoken.push(card.body.clone());
            md.extend(["### Email".to_string(), format!("- {}", card.body)]);
            troubles.push(card);
        }
        None => {}
    }

    if let Some((home, current, hourly)) = &weather {
        // His way to school and back, on a school day: the first lesson that takes place (when
        // he has not left yet) and the last one's end.
        let (leave, back) = ahead.as_ref().filter(|a| a.day == day && holiday.is_none()).map_or((None, None), |a| {
            let real: Vec<&Lesson> = a.lessons.iter().filter(|l| l.status != LessonStatus::Cancelled).collect();
            // shortcut: half an hour's way to school; commute_advisor times the real route.
            let leave = real.first().filter(|_| a.over == 0).map(|l| a.day.and_time(l.start) - ChronoDuration::minutes(30));
            (leave, real.last().map(|l| a.day.and_time(l.end)))
        });
        let line = weather_line(home.town.as_deref(), current, hourly, now.naive_local(), day, day_word, leave, back, crate::location_engine::home_sun(day));
        md.extend(["### Weather".to_string(), format!("- {line}")]);
        spoken.push(line);
    }
    // His countdowns a week or less away ("5 days until your exam").
    if let Some(line) = crate::countdowns::briefing_line(day) {
        md.extend(["### Countdowns".to_string(), format!("- {line}")]);
        spoken.push(line);
    }
    // The town he found Sir in himself is asked about once (his yes keeps it as the home).
    let asked_home = weather.as_ref().and_then(|(home, ..)| {
        let asked = crate::kit::get("home_asked", "town").and_then(|v| v.as_str().map(str::to_string));
        let question = home_question(home, asked.as_deref())?;
        let town = home.town.clone().unwrap_or_default();
        let _ = crate::kit::put("home_asked", "town", &json!(town));
        tools.focus.note_offer(crate::focus::PendingOffer {
            tool: "manage_location_settings".into(),
            args: json!({ "action": "set_home", "address": town, "lat": home.lat, "lon": home.lon }),
            terminate: None,
        });
        Some(question)
    });
    if let Some(line) = reminders_on(&tools.reminders.list(), day, now).filter(|_| on("reminders")) {
        md.extend(["### Reminders".to_string(), format!("- {line}")]);
        spoken.push(line);
    }
    if on("system") {
        let system = system_line(tools);
        md.extend(["### System".to_string(), format!("- {system}")]);
        spoken.push(system);
    }
    if !news.is_empty() {
        let headlines = news.iter().map(|h| h.trim_end_matches('.').to_string()).collect::<Vec<_>>();
        md.push("### Headlines".into());
        md.extend(headlines.iter().map(|h| format!("- {h}")));
        spoken.push(format!("In the headlines: {}.", headlines.join(". ")));
    }
    let (free, told) = free;
    if !free.is_empty() {
        md.push("### Free on Epic".into());
        md.extend(free.iter().map(|t| format!("- {t}")));
    }
    spoken.extend(told);

    if !board.blocks.is_empty() {
        crate::hud_board::present(board);
    }
    let card = json!({ "id": "daily_briefing", "type": "MarkdownNoteCard", "title": "Daily Briefing Summary", "markdown": md.join("\n") });
    match tools.widgets.create_or_update(&card) {
        Ok(_) => crate::gui::refresh(),
        Err(e) => warn!("[BRIEF] The summary card failed: {e}"),
    }
    for card in troubles.iter().filter(|c| !c.fixes.is_empty()) {
        crate::proactive_butler::show(card);
    }
    // (His question last: Sir's answer is the next thing said.)
    spoken.extend(asked_home);
    spoken.join(" ")
}

/// "By the way, Sir: you live in Musterstadt, don't you?" for a home JAI worked out himself
/// (the Windows location service, the connection's address), once per town (`asked`: the town
/// asked about last). None for a home Sir set or named.
fn home_question(home: &crate::location_engine::Home, asked: Option<&str>) -> Option<String> {
    let town = home.town.as_deref().filter(|t| !home.confirmed && Some(*t) != asked)?;
    Some(format!("By the way, Sir: you live in {town}, don't you?"))
}

/// The unified academic briefing board: the day's lessons (WebUntis), the open itslearning
/// deadlines and the calendar's appointments, as one set of HUD blocks.
pub fn board_report(
    day: NaiveDate,
    day_word: &str,
    lessons: Option<&[Lesson]>,
    updates: Option<&[CourseUpdate]>,
    events: &[crate::calendar_client::CalEvent],
    now: chrono::NaiveDateTime,
) -> crate::hud_board::Report {
    use crate::hud_board::{Block, BlockKind, Note};
    let mut blocks = Vec::new();
    if let Some(lessons) = lessons {
        let held: Vec<&Lesson> = lessons.iter().filter(|l| l.status != LessonStatus::Cancelled).collect();
        let cancelled = lessons.len() - held.len();
        let span = match (held.first(), held.last()) {
            (Some(a), Some(b)) => format!("{} – {}", a.start.format("%H:%M"), b.end.format("%H:%M")),
            _ => "No lessons.".into(),
        };
        blocks.push(Block::new(
            format!("BRIEFING // SCHOOL {}", day_word.to_uppercase()),
            "WEBUNTIS",
            "SOURCE: WEBUNTIS",
            BlockKind::HeroFigure {
                header: format!("Lessons {day_word}"),
                figure: if held.is_empty() { "FREE".into() } else { held.len().to_string() },
                note: if cancelled > 0 { format!("{span} · {cancelled} cancelled") } else { span },
            },
        ));
        let notes: Vec<Note> = lessons
            .iter()
            .map(lesson_note)
            .collect();
        if !notes.is_empty() {
            blocks.push(Block::new("WEBUNTIS // TIMETABLE", day.format("%a %d %b").to_string(), "SOURCE: WEBUNTIS", BlockKind::NoteStack { notes }));
        }
    }
    if let Some(updates) = updates {
        // The open tasks figure and the deadlines stack from the itslearning board.
        let it = crate::itslearning_bw_sso::board_report(updates, now);
        blocks.extend(it.blocks.into_iter().filter(|b| b.label.contains("OPEN TASKS") || b.label.contains("DEADLINES")));
    }
    let appointments: Vec<Note> = events
        .iter()
        .take(8)
        .map(|e| {
            let title = crate::english::in_english(&e.title);
            Note {
                headline: if e.all_day { title } else { format!("{} · {title}", e.start.format("%H:%M")) },
                body: if e.location.is_empty() { "Calendar".into() } else { crate::english::in_english(&e.location) },
            }
        })
        .collect();
    if !appointments.is_empty() {
        blocks.push(Block::new("CALENDAR // APPOINTMENTS", day_word.to_uppercase(), "SOURCE: CALENDAR", BlockKind::NoteStack { notes: appointments }));
    }
    crate::hud_board::Report { topic: format!("briefing {day_word}"), created_at: now.format("%Y-%m-%d %H:%M:%S").to_string(), blocks, sources: Vec::new(), id: None }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::location_engine::{CurrentWeather, HourWeather};
    use chrono::NaiveTime;

    fn hours(day: NaiveDate, wet: &[u32]) -> Vec<HourWeather> {
        (0..24)
            .map(|h| HourWeather {
                time: day.and_hms_opt(h, 0, 0).unwrap(),
                temperature: 5.0 + h as f64 / 3.0,
                precip_probability: if wet.contains(&h) { 70.0 } else { 10.0 },
                ..Default::default()
            })
            .collect()
    }

    /// 2026-11-16: "In <a district of his town> it's 9 degrees and light rain shower". His
    /// town by name, Open-Meteo's words, the day's range, the rain against his school day and
    /// the sun.
    #[test]
    fn the_weather_names_his_town_and_sets_the_rain_against_his_day() {
        let day = NaiveDate::from_ymd_opt(2026, 11, 17).unwrap();
        let at = |h: u32, m: u32| day.and_hms_opt(h, m, 0).unwrap();
        let w = CurrentWeather { temperature: 8.6, code: 61, ..Default::default() };
        let sun = Some((NaiveTime::from_hms_opt(7, 41, 0).unwrap(), NaiveTime::from_hms_opt(16, 52, 0).unwrap()));
        let (leave, back) = (Some(at(7, 20)), Some(at(13, 5)));
        let line = weather_line(Some("Musterstadt"), &w, &hours(day, &[15, 16]), at(6, 30), day, "today", leave, back, sun);
        assert_eq!(line, "In Musterstadt it's 9 degrees and light rain; today 5 to 13 degrees, rain likely from about 15, after your last lesson. The sun rises at 7:41.");
        let afternoon = weather_line(Some("Musterstadt"), &w, &hours(day, &[15, 16]), at(14, 0), day, "today", leave, back, sun);
        assert!(afternoon.ends_with("after your last lesson. The sun sets at 16:52."), "{afternoon}");
        let day_hours = hours(day, &[7]);
        let wet: Vec<&HourWeather> = day_hours.iter().collect();
        assert_eq!(rain_against_day(&wet, at(6, 30), leave, back).as_deref(), Some("rain likely around 7:20, when you leave for school: take an umbrella"));
        let day_hours = hours(day, &[13]);
        let wet: Vec<&HourWeather> = day_hours.iter().collect();
        assert_eq!(rain_against_day(&wet, at(6, 30), leave, back).as_deref(), Some("rain likely around 13:05, on your way home from school"));
        let day_hours = hours(day, &[10, 11]);
        let wet: Vec<&HourWeather> = day_hours.iter().collect();
        assert_eq!(rain_against_day(&wet, at(6, 30), leave, back).as_deref(), Some("rain likely from about 10, while you're at school; dry for the way home"));
        let day_hours = hours(day, &[]);
        let dry: Vec<&HourWeather> = day_hours.iter().collect();
        assert_eq!(rain_against_day(&dry, at(6, 30), leave, back), None);
        // After sunset no sun; no school day: just when. The evening's briefing tells tomorrow's.
        let late = weather_line(None, &w, &hours(day, &[20]), at(18, 0), day, "today", None, None, sun);
        assert_eq!(late, "It's 9 degrees and light rain; today 5 to 13 degrees, rain likely from about 20.");
        let tomorrow = day + ChronoDuration::days(1);
        let evening = weather_line(None, &w, &hours(tomorrow, &[]), at(19, 0), tomorrow, "tomorrow", None, None, sun);
        assert_eq!(evening, "It's 9 degrees and light rain; tomorrow 5 to 13 degrees. Tomorrow the sun rises at 7:41.");
    }

    /// He asks once about a town he found himself, never about one Sir set or named.
    #[test]
    fn the_home_he_found_is_asked_about_once() {
        let found = crate::location_engine::Home { lat: 52.5, lon: 13.4, accuracy_m: 25_000.0, town: Some("Musterstadt".into()), source: "IP geolocation", confirmed: false };
        assert_eq!(home_question(&found, None).as_deref(), Some("By the way, Sir: you live in Musterstadt, don't you?"));
        assert_eq!(home_question(&found, Some("Musterstadt")), None, "asked already");
        assert!(home_question(&found, Some("Anderstadt")).is_some(), "a new town is asked about");
        let set = crate::location_engine::Home { confirmed: true, ..found };
        assert_eq!(home_question(&set, None), None);
    }

    /// "Leave the news out of my briefing" and the like, said and routed.
    #[test]
    fn briefing_parts_are_left_out_and_put_back_by_voice() {
        for (said, args) in [
            ("leave the news out of my briefing", json!({"leave_out": ["news"]})),
            ("leave the news and the weather out of my briefing", json!({"leave_out": ["news", "weather"]})),
            ("dont tell me about the free games in my briefing", json!({"leave_out": ["games"]})),
            ("put the free games back in my briefing", json!({"put_back": ["games"]})),
            ("add the headlines to my briefing", json!({"put_back": ["news"]})),
            ("put the weather back in", json!({"put_back": ["weather"]})),
            ("lass das wetter im briefing weg", json!({"leave_out": ["weather"]})),
            ("whats in my briefing", json!({"parts": "list"})),
        ] {
            assert_eq!(part_order(said), Some(args), "{said}");
        }
        assert_eq!(part_order("leave the dog out of my briefing"), None, "not a part");
        assert_eq!(part_order("give me my briefing"), None);
        let routed = crate::phrasings::route("Leave the news out of my briefing.");
        assert_eq!(routed, Some(("run_executive_briefing".to_string(), json!({"leave_out": ["news"]}))));
    }

    #[test]
    fn the_briefing_board_combines_timetable_deadlines_and_appointments() {
        use crate::hud_board::BlockKind;
        let day = NaiveDate::from_ymd_opt(2026, 11, 3).unwrap();
        let t = |h: u32| NaiveTime::from_hms_opt(h, 0, 0).unwrap();
        let lesson = |h: u32, subject: &str, status: LessonStatus| Lesson {
            date: day,
            start: t(h),
            end: t(h + 1),
            subject: subject.into(),
            subject_long: String::new(),
            teachers: vec!["Mr Smith".into()],
            rooms: vec!["204".into()],
            original_rooms: vec![],
            original_teachers: vec![],
            status,
            note: String::new(),
        };
        let lessons = vec![lesson(8, "M", LessonStatus::Regular), lesson(9, "PH", LessonStatus::Cancelled), lesson(10, "D", LessonStatus::Irregular)];
        let now = day.and_hms_opt(7, 0, 0).unwrap();
        let task = CourseUpdate { id: "t".into(), kind: "assignment", course: "Physik".into(), title: "Protokoll".into(), when: None, due: day.and_hms_opt(20, 0, 0), author: String::new(), url: String::new() };
        let r = board_report(day, "today", Some(&lessons), Some(&[task]), &[], now);
        let BlockKind::HeroFigure { figure, note, .. } = &r.blocks[0].kind else { panic!() };
        assert_eq!(figure, "2");
        assert_eq!(note, "08:00 – 11:00 · 1 cancelled");
        assert!(r.blocks.iter().any(|b| b.label == "WEBUNTIS // TIMETABLE"));
        assert!(r.blocks.iter().any(|b| b.label == "ITSLEARNING // OPEN TASKS"));
        assert!(!r.blocks.iter().any(|b| b.label.contains("CALENDAR")), "no appointments, no block");
        // A free day reads as FREE.
        let free = board_report(day, "today", Some(&[]), None, &[], now);
        assert!(matches!(&free.blocks[0].kind, BlockKind::HeroFigure { figure, .. } if figure == "FREE"));
    }

    #[test]
    fn greetings_and_lists() {
        assert_eq!(greeting(8), "Good morning, Sir.");
        assert_eq!(greeting(15), "Good afternoon, Sir.");
        assert_eq!(greeting(20), "Good evening, Sir.");
        assert_eq!(greeting(2), "Burning the midnight oil, Sir?");
        for hour in [8, 15, 20, 2] {
            let lines: std::collections::HashSet<String> = (0..18).map(|seed| startup_line(hour, seed)).collect();
            assert!(lines.len() == 18 && lines.iter().all(|l| l.contains("Sir")), "{lines:?}");
        }
        assert!(startup_line(8, 0).starts_with("Good morning") && startup_line(2, 0).starts_with("Burning"));
        // One problem replaces the systems report; nothing is added when all is well.
        let disk = |mount: &str, free_gb: f32| crate::telemetry::DiskInfo { mount: mount.into(), total_gb: 500.0, free_gb, ..Default::default() };
        assert_eq!(startup_problem(&[], &[disk("C:\\", 120.0), disk("D:\\", 40.0)]), None);
        assert_eq!(startup_problem(&[], &[disk("C:\\", 120.0), disk("D:\\", 3.24)]).as_deref(), Some("One thing first: drive D has only 3.2 gigabytes free."));
        assert!(startup_problem(&["Whisper speech model"], &[disk("C:\\", 1.0)]).unwrap().contains("the Whisper speech model is missing"));
        assert_eq!(startup_line_with(8, 2, Some("One thing first: x.")), "Good morning, Sir. One thing first: x.");
        assert_eq!(startup_line_with(2, 0, Some("One thing first: x.")), "Burning the midnight oil, Sir? One thing first: x.");
        assert_eq!(startup_line_with(20, 4, None), startup_line(20, 4));
        assert_eq!(join_and(&["a".into(), "b".into(), "c".into()]), "a, b and c");
        assert_eq!(ordinal(21), "21st");
        assert!(period(Some("evening"), 8) == Period::Evening && period(None, 9) == Period::Morning);
    }

    #[test]
    fn cancelled_classes_lead_the_school_line() {
        let day = NaiveDate::from_ymd_opt(2026, 10, 22).unwrap();
        let t = |h, m| NaiveTime::from_hms_opt(h, m, 0).unwrap();
        let lesson = |start, subject: &str, status| Lesson {
            date: day,
            start,
            end: start + ChronoDuration::minutes(45),
            subject: String::new(),
            subject_long: subject.into(),
            teachers: vec![],
            rooms: vec!["101".into()],
            original_rooms: vec![],
            original_teachers: vec![],
            status,
            note: String::new(),
        };
        let (line, details) = school_lines(&[lesson(t(8, 0), "Physik", LessonStatus::Cancelled), lesson(t(9, 50), "Mathe", LessonStatus::Irregular), lesson(t(11, 0), "Englisch", LessonStatus::Regular)], "tomorrow", false);
        assert_eq!(line, "On WebUntis tomorrow: Physics at 8 is cancelled, a free period and Mathematics at 9:50 has a substitution in room 101.");
        assert_eq!(details.len(), 3);
        let (calm, _) = school_lines(&[lesson(t(8, 0), "Physik", LessonStatus::Regular)], "today", false);
        assert_eq!(calm, "Your timetable today runs as planned: 1 lesson, starting at 8 with Physics.");
    }

    /// Sir at 14:49 before a list of the morning's lessons: only what is still ahead is told.
    #[test]
    fn a_briefing_tells_only_of_the_lessons_still_ahead() {
        let friday = NaiveDate::from_ymd_opt(2026, 11, 13).unwrap();
        let monday = friday + ChronoDuration::days(3);
        let t = |h, m| NaiveTime::from_hms_opt(h, m, 0).unwrap();
        let lesson = |date, start: NaiveTime, end, subject: &str| Lesson {
            date,
            start,
            end,
            subject: String::new(),
            subject_long: subject.into(),
            teachers: vec![],
            rooms: vec![],
            original_rooms: vec![],
            original_teachers: vec![],
            status: LessonStatus::Regular,
            note: String::new(),
        };
        // (As WebUntis hands them over: in no particular order.)
        let all = vec![
            lesson(monday, t(7, 40), t(9, 15), "Mathe"),
            lesson(friday, t(12, 10), t(12, 55), "Kunst"),
            lesson(friday, t(7, 40), t(9, 15), "Physik"),
            lesson(friday, t(9, 25), t(11, 0), "Deutsch"),
        ];
        let names = |a: &Ahead| a.lessons.iter().map(|l| l.subject_long.clone()).collect::<Vec<_>>();
        // Before school: the whole day, in its order.
        let early = lessons_ahead(&all, friday, friday.and_time(t(6, 30)));
        assert_eq!((early.day, names(&early), early.over), (friday, vec!["Physik".to_string(), "Deutsch".into(), "Kunst".into()], 0));
        // In the second lesson: that one (it has not ended) and what follows.
        let midday = lessons_ahead(&all, friday, friday.and_time(t(10, 0)));
        assert_eq!((midday.day, names(&midday), midday.over), (friday, vec!["Deutsch".to_string(), "Kunst".into()], 1));
        assert_eq!(school_lines(&midday.lessons, "today", true).0, "Still ahead today: 2 lessons, the next at 9:25 with German.");
        // At 14:49 the day is over: Monday's lessons, not the morning's.
        let after = lessons_ahead(&all, friday, friday.and_time(t(14, 49)));
        assert_eq!((after.day, names(&after), after.over), (monday, vec!["Mathe".to_string()], 0));
        assert_eq!(day_words(after.day, friday), ("on Monday".to_string(), "Monday".to_string()));
        assert_eq!(school_lines(&after.lessons, "on Monday", false).0, "Your timetable on Monday runs as planned: 1 lesson, starting at 7:40 with Mathematics.");
        // Nothing in reach after today's: said in one line, with no list of what is behind him.
        let last = lessons_ahead(&all[1..], friday, friday.and_time(t(14, 49)));
        assert_eq!((last.day, last.lessons.len(), last.over), (friday, 0, 3));
        assert_eq!(school_lines(&last.lessons, "today", true), ("No more lessons today.".to_string(), vec!["No more lessons today.".to_string()]));
        // The evening's briefing looks at tomorrow as a whole.
        let evening = lessons_ahead(&all, monday, friday.and_time(t(20, 0)));
        assert_eq!((evening.day, names(&evening), evening.over), (monday, vec!["Mathe".to_string()], 0));
        assert_eq!(day_words(friday + ChronoDuration::days(1), friday).0, "tomorrow");
    }
}
