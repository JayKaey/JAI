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
use serde_json::json;
use std::time::Duration;
use tracing::warn;

/// Default news feed (configurable as `briefing_news_feed`).
pub const DEFAULT_FEED: &str = "https://feeds.bbci.co.uk/news/world/rss.xml";

pub fn greeting(hour: u32) -> &'static str {
    match hour {
        5..=11 => "Good morning, Sir.",
        12..=17 => "Good afternoon, Sir.",
        18..=22 => "Good evening, Sir.",
        _ => "Burning the midnight oil, Sir?",
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
            LessonStatus::Cancelled => format!("{} at {} is cancelled, a free period", l.subject_name(), spoken_clock(l.start)),
            _ => format!(
                "{} at {} has a substitution{}",
                l.subject_name(),
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
            first.subject_name()
        )
    } else if changes.is_empty() {
        format!(
            "Your timetable {day_word} runs as planned: {} lesson{}, starting at {} with {}.",
            lessons.len(),
            if lessons.len() == 1 { "" } else { "s" },
            spoken_clock(first.start),
            first.subject_name()
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
            first.title.chars().take(70).collect::<String>(),
            if first.course.is_empty() { String::new() } else { format!(" in {}", first.course) }
        ));
    }
    for u in due.iter().take(2) {
        if let Some(d) = u.due {
            parts.push(format!("'{}' is due {} at {}", u.title.chars().take(60).collect::<String>(), crate::pim::spoken_day(d.date(), now.date()), d.format("%H:%M")));
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
        details.push(note.to_string());
    }
    let details = details.join(" · ");
    Note {
        headline: format!("{} – {} · {}", l.start.format("%H:%M"), l.end.format("%H:%M"), l.subject_name()),
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
    let (school_on, lms_on, mail_on) = (cfg.missing(WEBUNTIS).is_none(), cfg.missing(LMS).is_none(), cfg.missing(EMAIL).is_none());
    pim.pulse(&["brain", "briefing"], false);
    for (on, node) in [(school_on, "webuntis"), (lms_on, "itslearning"), (true, "calendar"), (mail_on, "email")] {
        if on {
            pim.pulse(&["brain", "briefing", node], false);
        }
    }
    let feed = tools.config.read(|c| c.briefing_news_feed.clone());
    crate::websearch::set_home_location(crate::websearch::place_from_memories(&tools.memory.list(200).unwrap_or_default()));
    let place = crate::websearch::home_location().unwrap_or_default();
    let quick = Duration::from_secs(6);
    let (school, lms, (events, cal_note), mail, weather, news) = tokio::join!(
        // (Three days on from today: when today's lessons are over the next school day is what
        // is told, and a weekend may lie in between.)
        async { if school_on { Some(pim.lessons(day, if day == now.date_naive() { day + ChronoDuration::days(3) } else { day }).await) } else { None } },
        async { if lms_on { Some(pim.course_updates(None).await) } else { None } },
        pim.schedule(day, day),
        async { if mail_on { Some(pim.unread_mail(3, None).await) } else { None } },
        async { tokio::time::timeout(quick, crate::websearch::weather_json(&place)).await.ok().flatten() },
        async {
            if feed.trim().is_empty() || when == Period::Evening {
                Vec::new()
            } else {
                tokio::time::timeout(quick, crate::websearch::headlines(&feed, 2)).await.unwrap_or_default()
            }
        }
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

    match school {
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
            spoken.push("I couldn't reach WebUntis for your timetable.".into());
            md.extend(["### School".to_string(), format!("- WebUntis unavailable: {e}")]);
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
            md.extend(["### itslearning".to_string(), format!("- Unavailable: {e}")]);
        }
        None => {}
    }

    md.push(format!("### Calendar ({day_word})"));
    let timed: Vec<_> = events.iter().filter(|e| !e.all_day).collect();
    if events.is_empty() && appointments > 0 {
        spoken.push(format!("Nothing more on your calendar {day_word}."));
        md.push("- Nothing more scheduled.".into());
    } else if events.is_empty() {
        spoken.push(format!("Your calendar is clear {day_word}."));
        md.push("- Nothing scheduled.".into());
    } else {
        let names: Vec<String> = events
            .iter()
            .take(4)
            .map(|e| if e.all_day { e.title.clone() } else { format!("{} at {}", e.title, spoken_clock(e.start.time())) })
            .collect();
        spoken.push(format!(
            "Your calendar has {} appointment{} {day_word}: {}.",
            events.len(),
            if events.len() == 1 { "" } else { "s" },
            join_and(&names)
        ));
        let clashes = crate::calendar_client::conflicts(&events);
        if let Some((a, b)) = clashes.first() {
            spoken.push(format!("Note that {a} and {b} overlap."));
        }
        md.extend(events.iter().map(|e| {
            let clash = timed.iter().any(|o| o.uid != e.uid && o.overlaps(e));
            format!("- {}{}", e.line(), if clash { " [CONFLICT]" } else { "" })
        }));
    }
    if let Some(note) = cal_note {
        md.push(format!("- {note}"));
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
            md.extend(["### Email".to_string(), format!("- Unavailable: {e}")]);
        }
        None => {}
    }

    if let Some(line) = weather.as_ref().and_then(crate::websearch::spoken_weather) {
        md.extend(["### Weather".to_string(), format!("- {line}")]);
        spoken.push(line);
    }
    if let Some(line) = reminders_on(&tools.reminders.list(), day, now) {
        md.extend(["### Reminders".to_string(), format!("- {line}")]);
        spoken.push(line);
    }
    let system = system_line(tools);
    md.extend(["### System".to_string(), format!("- {system}")]);
    spoken.push(system);
    if !news.is_empty() {
        let headlines = news.iter().map(|h| h.trim_end_matches('.').to_string()).collect::<Vec<_>>();
        md.push("### Headlines".into());
        md.extend(headlines.iter().map(|h| format!("- {h}")));
        spoken.push(format!("In the headlines: {}.", headlines.join(". ")));
    }

    if !board.blocks.is_empty() {
        crate::hud_board::present(board);
    }
    let card = json!({ "id": "daily_briefing", "type": "MarkdownNoteCard", "title": "Daily Briefing Summary", "markdown": md.join("\n") });
    match tools.widgets.create_or_update(&card) {
        Ok(_) => crate::gui::refresh(),
        Err(e) => warn!("[BRIEF] The summary card failed: {e}"),
    }
    spoken.join(" ")
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
        .map(|e| Note {
            headline: if e.all_day { e.title.clone() } else { format!("{} · {}", e.start.format("%H:%M"), e.title) },
            body: if e.location.is_empty() { "Calendar".into() } else { e.location.clone() },
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
    use chrono::NaiveTime;

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
        assert_eq!(line, "On WebUntis tomorrow: Physik at 8 is cancelled, a free period and Mathe at 9:50 has a substitution in room 101.");
        assert_eq!(details.len(), 3);
        let (calm, _) = school_lines(&[lesson(t(8, 0), "Physik", LessonStatus::Regular)], "today", false);
        assert_eq!(calm, "Your timetable today runs as planned: 1 lesson, starting at 8 with Physik.");
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
        assert_eq!(school_lines(&midday.lessons, "today", true).0, "Still ahead today: 2 lessons, the next at 9:25 with Deutsch.");
        // At 14:49 the day is over: Monday's lessons, not the morning's.
        let after = lessons_ahead(&all, friday, friday.and_time(t(14, 49)));
        assert_eq!((after.day, names(&after), after.over), (monday, vec!["Mathe".to_string()], 0));
        assert_eq!(day_words(after.day, friday), ("on Monday".to_string(), "Monday".to_string()));
        assert_eq!(school_lines(&after.lessons, "on Monday", false).0, "Your timetable on Monday runs as planned: 1 lesson, starting at 7:40 with Mathe.");
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
