//! School things Sir tells J.A.I. himself (2026-11-16): his points ("I got 13 points in
//! physics", "ich habe 13 Punkte in Physik"), kept in the kit's store, with the average per
//! subject and overall and what the next test needs to lift a subject to the next whole point.
//!
//! Points are the Oberstufe's 0-15; they are his own words, never read from WebUntis.
//!
//! From WebUntis (and itslearning's assignments) it also builds:
//! - tomorrow in one line ("what do I need tomorrow?"): the lessons, what to pack for them (a
//!   list he teaches: "for PE I need my sports kit") and the exams with their lesson number,
//! - the homework block, ticked off by voice ("maths homework done"), ticks kept by id,
//! - a study plan for the exams of the next three weeks: short sessions on the days before
//!   each; into the calendar only after his yes.

use crate::kit::say;
use chrono::{Datelike, Duration as Days, NaiveDate, NaiveTime};
use regex::Regex;
use serde_json::{json, Value};
use std::sync::LazyLock;

/// What he packs for a subject: key = subject, value = the items as he said them.
pub const PACK_ITEMS: &str = "pack_items";
/// Homework he ticked off: key = the homework's id.
pub const HOMEWORK_DONE: &str = "homework_done";

/// His points: key = when he said them, value = {subject, points}.
pub const GRADES: &str = "grades";

fn re(pattern: &str) -> Regex {
    Regex::new(pattern).expect("a valid school phrase")
}

const SUBJECT: &str = r"(?P<s>[\p{L}][\p{L} ]{1,30}?)";

static GOT: LazyLock<Regex> = LazyLock::new(|| {
    re(&format!(
        r"^(?:i (?:got|scored|have got|had|get)|i've got|ich (?:habe|hab|hatte|bekam|kriegte)|ich hab)\s+(?:a |an |eine |ne )?(?P<n>\d{{1,2}})\s+(?:points?|punkte?)\s+(?:in|on|im|bei)\s+(?:the |my |der |dem |meiner |meinem )?{SUBJECT}(?:\s+(?:test|exam|klausur|arbeit|ex))?(?:\s+(?:geschrieben|bekommen|gekriegt|gehabt|zurück|zurückbekommen))?$"
    ))
});
static GOT_DE: LazyLock<Regex> = LazyLock::new(|| {
    re(&format!(r"^ich (?:habe|hab|hatte) in (?:der |dem )?{SUBJECT} (?:eine |ne )?(?P<n>\d{{1,2}}) punkte?(?: (?:geschrieben|bekommen|gekriegt|gehabt|zurückbekommen))?$"))
});
static SHOW: LazyLock<Regex> = LazyLock::new(|| {
    re(&format!(
        r"^(?:what(?:'s|s| s| is) my (?:average|grade average|points average)|how am i doing at school|show (?:me )?my (?:grades|points|marks)|wie ist mein (?:schnitt|durchschnitt|notenschnitt|punkteschnitt)|was ist mein (?:schnitt|durchschnitt)|zeig(?:e)? (?:mir )?meine (?:noten|punkte))(?:\s+(?:in|im|bei)\s+(?:der |dem )?{SUBJECT})?$"
    ))
});
static FORGET_LAST: LazyLock<Regex> =
    LazyLock::new(|| re(r"^(?:delete|remove|forget) (?:my |the )?last (?:grade|points|mark)|^(?:lösch|vergiss) (?:meine |die )?letzte (?:note|punktzahl)"));
static PACK: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:what do i (?:need|have to pack|pack|have) (?:for )?tomorrow|what(?:'s|s| s| is) (?:on )?tomorrow at school|what do i need for school tomorrow|was (?:muss|brauche) ich (?:für )?morgen (?:einpacken|mitnehmen)|was habe ich morgen(?: in der schule)?|was brauche ich morgen)$")
});
static PACK_TEACH: LazyLock<Regex> = LazyLock::new(|| {
    re(&format!(r"^for {SUBJECT} i (?:need|pack|bring|take) (?P<i>.+)$|^für (?P<s2>[\p{{L}}][\p{{L}} ]{{1,30}}?) (?:brauche|packe|nehme) ich (?P<i2>.+?)(?: mit| ein)?$"))
});
static HOMEWORK: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:what|which) homework (?:is left|do i have|have i got|is open)|^(?:show|list) (?:me )?my homework$|^welche hausaufgaben (?:habe ich|hab ich|sind offen|muss ich machen)|^zeig(?:e)? (?:mir )?meine hausaufgaben$")
});
static HOMEWORK_TICK: LazyLock<Regex> = LazyLock::new(|| {
    re(&format!(
        r"^(?:my |the )?{SUBJECT} homework (?:is )?(?:done|finished)$|^i (?:did|finished|have done|'ve done) (?:my |the )?(?P<s2>[\p{{L}}][\p{{L}} ]{{1,30}}?) homework$|^(?P<s3>[\p{{L}}][\p{{L}}]{{1,30}})[- ]?hausaufgaben (?:sind )?(?:erledigt|fertig|gemacht)$|^ich habe (?:die |meine )?(?P<s4>[\p{{L}}][\p{{L}}]{{1,30}})[- ]?hausaufgaben (?:gemacht|erledigt)$"
    ))
});
static STUDY: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:make|plan|build|give) (?:me )?a (?:study|revision|learning) plan$|^(?:when|how) should i (?:study|revise|learn) for my (?:exams|tests)$|^mach(?:e)? (?:mir )?einen lernplan$|^(?:mein )?lernplan$")
});
static STUDY_TO_CALENDAR: LazyLock<Regex> = LazyLock::new(|| re(r"^add (?:my |the )?study plan to (?:my |the )?calendar$"));
static WEEK: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:what(?:'s|s| s| is) my (?:week|next week) (?:like|looking like|ahead)|what does my (?:week|next week) look like|how does my (?:week|next week) look|(?:show (?:me )?)?my week(?: ahead)?|the week ahead|wie sieht meine (?:woche|nächste woche) aus|(?:zeig(?:e)? (?:mir )?)?meine woche)$")
});

/// An order about school days (packing, homework, the study plan): the tool's arguments.
pub fn school_command(s: &str) -> Option<Value> {
    let s = s.trim().trim_end_matches(['?', '.', '!']).trim();
    let pick = |c: &regex::Captures, names: &[&str]| names.iter().find_map(|n| c.name(n)).map(|m| m.as_str().trim().to_string());
    if PACK.is_match(s) {
        return Some(json!({"action": "tomorrow"}));
    }
    if let Some(c) = PACK_TEACH.captures(s) {
        let subject = subject(&pick(&c, &["s", "s2"])?)?;
        let items = pick(&c, &["i", "i2"])?;
        return Some(json!({"action": "teach_pack", "subject": subject, "items": items}));
    }
    if HOMEWORK.is_match(s) {
        return Some(json!({"action": "homework"}));
    }
    if let Some(c) = HOMEWORK_TICK.captures(s) {
        return Some(json!({"action": "homework_done", "subject": subject(&pick(&c, &["s", "s2", "s3", "s4"])?)?}));
    }
    if STUDY_TO_CALENDAR.is_match(s) {
        return Some(json!({"action": "plan_to_calendar"}));
    }
    if WEEK.is_match(s) {
        return Some(json!({"action": "week", "next": s.contains("next week") || s.contains("nächste woche")}));
    }
    STUDY.is_match(s).then(|| json!({"action": "study_plan"}))
}

pub fn school_only_looks(args: &Value) -> bool {
    matches!(args["action"].as_str().unwrap_or(""), "tomorrow" | "homework" | "study_plan" | "week")
}

/// An order about his points: the tool's arguments.
pub fn grades_command(s: &str) -> Option<Value> {
    let s = s.trim().trim_end_matches(['?', '.', '!']).trim();
    if let Some(c) = GOT.captures(s).or_else(|| GOT_DE.captures(s)) {
        let points: u8 = c.name("n")?.as_str().parse().ok()?;
        let subject = subject(c.name("s")?.as_str())?;
        return (points <= 15).then(|| json!({"action": "add", "subject": subject, "points": points}));
    }
    if let Some(c) = SHOW.captures(s) {
        return Some(match c.name("s").and_then(|m| subject(m.as_str())) {
            Some(subject) => json!({"action": "show", "subject": subject}),
            None => json!({"action": "show"}),
        });
    }
    FORGET_LAST.is_match(s).then(|| json!({"action": "delete_last"}))
}

/// The subject's one name (English, as the timetable's aliases give it), or None for a word
/// that is no subject ("I got 3 points in a row").
fn subject(said: &str) -> Option<String> {
    crate::pim::subject_aliases(said.trim()).into_iter().next()
}

/// (points, times written) per subject, in the order first written.
fn by_subject(all: &[(String, u8)]) -> Vec<(String, Vec<u8>)> {
    let mut out: Vec<(String, Vec<u8>)> = Vec::new();
    for (subject, points) in all {
        match out.iter_mut().find(|(s, _)| s == subject) {
            Some((_, list)) => list.push(*points),
            None => out.push((subject.clone(), vec![*points])),
        }
    }
    out
}

fn average(points: &[u8]) -> f32 {
    points.iter().map(|&p| p as f32).sum::<f32>() / points.len().max(1) as f32
}

/// The points the next test needs to lift the average to the next whole point (None: none
/// can, more than 15 would be needed, or it is at 15 already).
pub fn needed_for_next(points: &[u8]) -> Option<u8> {
    let avg = average(points);
    let target = avg.floor() + 1.0;
    if points.is_empty() || target > 15.0 {
        return None;
    }
    let sum: f32 = points.iter().map(|&p| p as f32).sum();
    let need = (target * (points.len() as f32 + 1.0) - sum).max(0.0).ceil();
    (need <= 15.0).then_some(need as u8)
}

fn all_grades() -> Vec<(String, u8)> {
    let mut rows = crate::kit::list(GRADES);
    rows.sort_by(|a, b| a.0.cmp(&b.0));
    rows.into_iter().filter_map(|(_, v, _)| Some((v["subject"].as_str()?.to_string(), v["points"].as_u64()? as u8))).collect()
}

fn title_case(s: &str) -> String {
    let mut c = s.chars();
    c.next().map(|f| f.to_uppercase().chain(c).collect()).unwrap_or_default()
}

/// The block: one row per subject (average, how often, what the next test needs), then the
/// overall average.
pub fn grades_text(all: &[(String, u8)]) -> String {
    let subjects = by_subject(all);
    let mut out = String::new();
    for (subject, points) in &subjects {
        let next = needed_for_next(points).map(|n| format!("  ·  next: {n} for {}", average(points).floor() as u8 + 1)).unwrap_or_default();
        out.push_str(&format!("{}  {:.1}  ({}×){next}\n", title_case(subject), average(points), points.len()));
    }
    if subjects.len() > 1 {
        let overall = subjects.iter().map(|(_, p)| average(p)).sum::<f32>() / subjects.len() as f32;
        out.push_str(&format!("\nOverall  {overall:.1}"));
    }
    out.trim_end().to_string()
}

pub fn grades(args: &Value) -> Result<String, String> {
    match args["action"].as_str().unwrap_or("show") {
        "add" => {
            let subject = args["subject"].as_str().ok_or("Which subject, Sir?")?;
            let points = args["points"].as_u64().filter(|&p| p <= 15).ok_or("Points go from 0 to 15, Sir.")? as u8;
            crate::kit::put(GRADES, &crate::kit::stamp(), &json!({"subject": subject, "points": points}))?;
            let all = all_grades();
            let mine: Vec<u8> = all.iter().filter(|(s, _)| s == subject).map(|(_, p)| *p).collect();
            crate::kit::show_text(GRADES, "GRADES", &grades_text(&all), [340.0, 220.0]);
            let next = match needed_for_next(&mine) {
                Some(n) => format!(" {n} next time makes it {}.", average(&mine).floor() as u8 + 1),
                None => String::new(),
            };
            let line = format!("Noted, Sir: {points} points in {subject}. {} now averages {:.1}.{next}", title_case(subject), average(&mine));
            Ok(say(&line, &line))
        }
        "delete_last" => {
            let mut rows = crate::kit::list(GRADES);
            rows.sort_by(|a, b| a.0.cmp(&b.0));
            let (key, value, _) = rows.pop().ok_or("There are no points to take back, Sir.")?;
            crate::kit::remove(GRADES, &key);
            let line = format!("Taken back, Sir: {} points in {}.", value["points"], value["subject"].as_str().unwrap_or("?"));
            Ok(say(&line, &line))
        }
        _ => {
            let all = all_grades();
            if all.is_empty() {
                return Ok(say("No points yet, Sir. Tell me \"I got 13 points in physics\" and I keep them.", "No points yet, Sir."));
            }
            crate::kit::show_text(GRADES, "GRADES", &grades_text(&all), [340.0, 220.0]);
            let spoken = match args["subject"].as_str() {
                Some(subject) => {
                    let mine: Vec<u8> = all.iter().filter(|(s, _)| s == subject).map(|(_, p)| *p).collect();
                    if mine.is_empty() {
                        format!("No points in {subject} yet, Sir.")
                    } else {
                        format!("{} averages {:.1}, Sir, over {} {}.", title_case(subject), average(&mine), mine.len(), if mine.len() == 1 { "test" } else { "tests" })
                    }
                }
                None => {
                    let subjects = by_subject(&all);
                    let overall = subjects.iter().map(|(_, p)| average(p)).sum::<f32>() / subjects.len() as f32;
                    format!("Your average is {overall:.1} points, Sir, over {} {}.", subjects.len(), if subjects.len() == 1 { "subject" } else { "subjects" })
                }
            };
            Ok(say(&spoken, &spoken))
        }
    }
}

// ------------------------------------------------------------------------- school days ----

/// A lesson as this module needs it: its day, start, subject (the long name) and whether it
/// takes place.
#[derive(Debug, Clone, PartialEq)]
pub struct Period {
    pub date: NaiveDate,
    pub start: NaiveTime,
    pub subject: String,
    pub cancelled: bool,
}

/// An exam or a piece of homework (WebUntis or itslearning).
#[derive(Debug, Clone, PartialEq)]
pub struct Task {
    pub id: String,
    pub exam: bool,
    pub date: NaiveDate,
    pub start: Option<NaiveTime>,
    pub subject: String,
    pub text: String,
}

/// What he packs for a subject: his own list, else his PE kit for PE.
fn items_for(subject: &str, taught: &[(String, String)]) -> Option<String> {
    let key = crate::kit_school::subject(subject).unwrap_or_else(|| subject.to_lowercase());
    taught.iter().find(|(s, _)| *s == key).map(|(_, i)| i.clone()).or_else(|| (key == "sports").then(|| "your sports kit".to_string()))
}

/// The number of the lesson that starts at `start` on its day (1 = the day's first start).
fn lesson_number(day: &[Period], start: NaiveTime) -> Option<usize> {
    let mut starts: Vec<NaiveTime> = day.iter().map(|p| p.start).collect();
    starts.sort();
    starts.dedup();
    starts.iter().position(|s| *s == start).map(|i| i + 1)
}

/// Tomorrow in one line: the lessons in order (what to pack after each), then the exams with
/// their lesson number.
pub fn tomorrow_line(periods: &[Period], exams: &[Task], taught: &[(String, String)]) -> String {
    let mut subjects: Vec<String> = Vec::new();
    let mut sorted: Vec<&Period> = periods.iter().filter(|p| !p.cancelled).collect();
    sorted.sort_by_key(|p| p.start);
    for p in sorted {
        let said = match items_for(&p.subject, taught) {
            Some(items) => format!("{} (pack {items})", p.subject),
            None => p.subject.clone(),
        };
        if !subjects.iter().any(|s| s.starts_with(&p.subject)) {
            subjects.push(said);
        }
    }
    if subjects.is_empty() {
        return "No lessons tomorrow, Sir.".to_string();
    }
    let mut line = format!("Tomorrow, Sir: {}.", subjects.join(", "));
    for e in exams {
        let number = e.start.and_then(|s| lesson_number(periods, s)).map(|n| format!(" in lesson {n}")).unwrap_or_default();
        line.push_str(&format!(" {} exam{number}.", e.subject));
    }
    line
}

/// The open homework, soonest due first.
fn open_homework<'a>(tasks: &'a [Task], done: &[String]) -> Vec<&'a Task> {
    let mut open: Vec<&Task> = tasks.iter().filter(|t| !t.exam && !done.contains(&t.id)).collect();
    open.sort_by_key(|t| t.date);
    open
}

fn homework_text(open: &[&Task]) -> String {
    open.iter().map(|t| format!("{}  {}  {}", t.date.format("%a %d.%m."), t.subject, t.text.chars().take(70).collect::<String>())).collect::<Vec<_>>().join("\n")
}

/// The soonest open homework of `subject`.
fn homework_of<'a>(open: &[&'a Task], subject: &str) -> Option<&'a Task> {
    open.iter().copied().find(|t| crate::kit_school::subject(&t.subject).as_deref() == Some(subject) || t.subject.eq_ignore_ascii_case(subject))
}

/// Study sessions before each exam of the next three weeks: (day, subject, minutes). Up to four
/// days before an exam, never on the exam's day or in the past; the day before is the longest.
pub fn study_sessions(exams: &[Task], today: NaiveDate) -> Vec<(NaiveDate, String, u32)> {
    let mut out: Vec<(NaiveDate, String, u32)> = Vec::new();
    for e in exams.iter().filter(|e| e.exam && e.date > today && e.date <= today + Days::days(21)) {
        let days = (e.date - today).num_days().min(4);
        for back in 1..=days {
            let day = e.date - Days::days(back);
            if day >= today {
                out.push((day, e.subject.clone(), if back == 1 { 45 } else { 30 }));
            }
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));
    out
}

fn plan_text(sessions: &[(NaiveDate, String, u32)]) -> String {
    let mut lines: Vec<String> = Vec::new();
    for (day, subject, minutes) in sessions {
        let head = day.format("%a %d.%m.").to_string();
        match lines.last_mut().filter(|l| l.starts_with(&head)) {
            Some(line) => line.push_str(&format!(" · {subject} {minutes} min")),
            None => lines.push(format!("{head}  {subject} {minutes} min")),
        }
    }
    lines.join("\n")
}

/// The study sessions go into the calendar at this time of day (after school).
const STUDY_AT: (u32, u32) = (16, 30);

/// The week he asks about: this one from Monday to Sunday, or the next once it is the weekend.
pub fn week_of(today: NaiveDate, next: bool) -> NaiveDate {
    let monday = today - Days::days(today.weekday().num_days_from_monday() as i64);
    if next || matches!(today.weekday(), chrono::Weekday::Sat | chrono::Weekday::Sun) { monday + Days::days(7) } else { monday }
}

/// Monday to Sunday in one block: each day's exams, homework due, appointments, and the school
/// days without lessons. (the block, one line to say). `events`: (start, title, all day).
pub fn week_text(monday: NaiveDate, upcoming: bool, periods: &[Period], tasks: &[Task], done: &[String], events: &[(chrono::NaiveDateTime, String, bool)]) -> (String, String) {
    let (mut exams, mut due, mut dates, mut free_days) = (0, 0, 0, Vec::new());
    let mut lines = Vec::new();
    for offset in 0..7 {
        let day = monday + Days::days(offset);
        let mut rows: Vec<String> = Vec::new();
        let school_day = offset < 5;
        let lessons: Vec<Period> = periods.iter().filter(|p| p.date == day).cloned().collect();
        if school_day && !lessons.is_empty() && lessons.iter().all(|p| p.cancelled) {
            rows.push("no lessons".to_string());
            free_days.push(day.format("%A").to_string());
        }
        for t in tasks.iter().filter(|t| t.date == day && !done.contains(&t.id)) {
            if t.exam {
                exams += 1;
                let number = t.start.and_then(|s| lesson_number(&lessons, s)).map(|n| format!(" (lesson {n})")).unwrap_or_default();
                rows.push(format!("EXAM  {}{number}", t.subject));
            } else {
                due += 1;
                rows.push(format!("DUE   {}: {}", t.subject, t.text.chars().take(50).collect::<String>()));
            }
        }
        let mut todays: Vec<&(chrono::NaiveDateTime, String, bool)> = events.iter().filter(|(at, _, _)| at.date() == day).collect();
        todays.sort_by_key(|(at, _, _)| *at);
        for (at, title, all_day) in todays {
            dates += 1;
            rows.push(if *all_day { format!("all day  {title}") } else { format!("{}  {title}", at.format("%H:%M")) });
        }
        lines.push(format!("{}{}", day.format("%a %d.%m."), if rows.is_empty() { String::new() } else { format!("\n    {}", rows.join("\n    ")) }));
    }
    let count = |n: usize, one: &str, many: &str| (n > 0).then(|| format!("{} {}", crate::earth::count_word(n).to_lowercase(), if n == 1 { one } else { many }));
    let parts: Vec<String> = [count(exams, "exam", "exams"), count(due, "piece of homework due", "pieces of homework due"), count(dates, "appointment", "appointments")].into_iter().flatten().collect();
    let mut spoken = match parts.as_slice() {
        [] => "A quiet week, Sir: no exams, no homework due and no appointments.".to_string(),
        _ => format!("{}, Sir: {}.", if upcoming { "Next week" } else { "This week" }, crate::briefing::join_and(&parts)),
    };
    if !free_days.is_empty() {
        spoken.push_str(&format!(" No lessons on {}.", crate::briefing::join_and(&free_days)));
    }
    (lines.join("\n"), spoken)
}

async fn periods(pim: &crate::pim::Pim, from: NaiveDate, to: NaiveDate) -> Result<Vec<Period>, String> {
    use crate::webuntis_client::LessonStatus;
    Ok(pim
        .lessons(from, to)
        .await?
        .into_iter()
        .map(|l| Period { date: l.date, start: l.start, subject: if l.subject_long.is_empty() { l.subject } else { l.subject_long }, cancelled: l.status == LessonStatus::Cancelled })
        .collect())
}

/// WebUntis's exams and homework, and itslearning's assignments, in a range.
async fn tasks(pim: &crate::pim::Pim, from: NaiveDate, to: NaiveDate) -> Result<Vec<Task>, String> {
    let mut out: Vec<Task> = pim
        .school_tasks(from, to)
        .await?
        .into_iter()
        .map(|t| Task { id: format!("untis:{}", t.id), exam: t.kind == "exam", date: t.date, start: t.start, subject: t.subject, text: t.text })
        .collect();
    if let Ok(updates) = pim.course_updates(None).await {
        out.extend(updates.into_iter().filter(|u| u.kind == "assignment").filter_map(|u| {
            let due = u.due?.date();
            (due >= from && due <= to).then(|| Task { id: format!("lms:{}", u.id), exam: false, date: due, start: None, subject: u.course, text: u.title })
        }));
    }
    Ok(out)
}

pub async fn school(ctx: &crate::tools::ToolContext, args: &Value) -> Result<String, String> {
    let today = chrono::Local::now().date_naive();
    match args["action"].as_str().unwrap_or("") {
        "teach_pack" => {
            let (subject, items) = (args["subject"].as_str().ok_or("Which subject, Sir?")?, args["items"].as_str().ok_or("What do you pack, Sir?")?);
            crate::kit::put(PACK_ITEMS, subject, &json!(items))?;
            let line = format!("Noted, Sir: for {subject} you pack {items}.");
            Ok(say(&line, &line))
        }
        "tomorrow" => {
            let mut day = today + Days::days(1);
            // (Friday evening: "tomorrow" at school is Monday.)
            while matches!(day.weekday(), chrono::Weekday::Sat | chrono::Weekday::Sun) {
                day += Days::days(1);
            }
            let periods = periods(&ctx.pim, day, day).await?;
            let exams: Vec<Task> = tasks(&ctx.pim, day, day).await.unwrap_or_default().into_iter().filter(|t| t.exam).collect();
            let taught: Vec<(String, String)> = crate::kit::list(PACK_ITEMS).into_iter().filter_map(|(k, v, _)| Some((k, v.as_str()?.to_string()))).collect();
            let line = tomorrow_line(&periods, &exams, &taught);
            Ok(say(&line, &line))
        }
        "homework" | "homework_done" => {
            let all = tasks(&ctx.pim, today, today + Days::days(14)).await?;
            let done: Vec<String> = crate::kit::list(HOMEWORK_DONE).into_iter().map(|(k, _, _)| k).collect();
            let mut open = open_homework(&all, &done);
            let mut spoken = String::new();
            if args["action"] == "homework_done" {
                let subject = args["subject"].as_str().ok_or("Which subject, Sir?")?;
                let task = homework_of(&open, subject).ok_or_else(|| format!("I see no open {subject} homework, Sir."))?;
                crate::kit::put(HOMEWORK_DONE, &task.id, &json!(true))?;
                spoken = format!("Ticked off, Sir: {} homework. ", task.subject);
                let id = task.id.clone();
                open.retain(|t| t.id != id);
            }
            spoken.push_str(&match open.len() {
                0 => "No homework left, Sir.".to_string(),
                1 => format!("One left: {} by {}.", open[0].subject, open[0].date.format("%A")),
                n => format!("{n} left; the next is {} by {}.", open[0].subject, open[0].date.format("%A")),
            });
            let text = if open.is_empty() { "All done.".to_string() } else { homework_text(&open) };
            crate::kit::show_text("homework", "HOMEWORK", &text, [420.0, 220.0]);
            Ok(say(&spoken, &spoken))
        }
        "week" => {
            let monday = week_of(today, args["next"].as_bool().unwrap_or(false));
            let sunday = monday + Days::days(6);
            let periods = periods(&ctx.pim, monday, sunday).await.unwrap_or_default();
            let tasks = tasks(&ctx.pim, monday, sunday).await.unwrap_or_default();
            let done: Vec<String> = crate::kit::list(HOMEWORK_DONE).into_iter().map(|(k, _, _)| k).collect();
            let _ = ctx.pim.schedule(monday, sunday).await;
            let (from, to) = (monday.and_hms_opt(0, 0, 0).ok_or("bad day")?, (sunday + Days::days(1)).and_hms_opt(0, 0, 0).ok_or("bad day")?);
            let events: Vec<(chrono::NaiveDateTime, String, bool)> = ctx.pim.calendar().events_between(from, to).into_iter().map(|e| (e.start, e.title, e.all_day)).collect();
            let (text, spoken) = week_text(monday, monday > today, &periods, &tasks, &done, &events);
            crate::kit::show_text("week_ahead", &format!("WEEK // {} – {}", monday.format("%d.%m."), sunday.format("%d.%m.")), &text, [440.0, 520.0]);
            Ok(say(&spoken, &spoken))
        }
        "study_plan" | "plan_to_calendar" => {
            let exams: Vec<Task> = tasks(&ctx.pim, today, today + Days::days(21)).await?.into_iter().filter(|t| t.exam).collect();
            let sessions = study_sessions(&exams, today);
            if sessions.is_empty() {
                return Ok(say("No exams in the next three weeks, Sir: nothing to plan.", "No exams in the next three weeks, Sir."));
            }
            if args["action"] == "plan_to_calendar" {
                let mut added = 0;
                for (day, subject, minutes) in &sessions {
                    let start = day.and_hms_opt(STUDY_AT.0, STUDY_AT.1, 0).ok_or("bad time")?;
                    let end = start + Days::minutes(*minutes as i64);
                    let call = json!({"title": format!("Study: {subject}"), "start_time": start.format("%Y-%m-%d %H:%M").to_string(), "end_time": end.format("%Y-%m-%d %H:%M").to_string()});
                    if crate::pim::run_tool(ctx, "create_calendar_event", &call).await.is_ok() {
                        added += 1;
                    }
                }
                let line = format!("{added} study sessions are in your calendar, Sir, at {}:{:02}.", STUDY_AT.0, STUDY_AT.1);
                return Ok(say(&line, &line));
            }
            crate::kit::show_text("study_plan", "STUDY PLAN", &plan_text(&sessions), [420.0, 240.0]);
            crate::hud_lines::offer("add my study plan to the calendar");
            let line = format!("{} sessions before {} exams, Sir. Shall I put them in your calendar?", sessions.len(), exams.iter().filter(|e| e.date > today).count());
            Ok(say(&line, &line))
        }
        other => Err(format!("The school kit has no action '{other}'.")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 11, day).unwrap()
    }
    fn t(h: u32, m: u32) -> NaiveTime {
        NaiveTime::from_hms_opt(h, m, 0).unwrap()
    }
    fn period(start: NaiveTime, subject: &str) -> Period {
        Period { date: d(17), start, subject: subject.into(), cancelled: false }
    }
    fn task(id: &str, exam: bool, day: u32, subject: &str) -> Task {
        Task { id: id.into(), exam, date: d(day), start: None, subject: subject.into(), text: "p. 42".into() }
    }

    #[test]
    fn school_days_are_heard_in_english_and_german() {
        for (said, want) in [
            ("what do i need tomorrow", json!({"action": "tomorrow"})),
            ("was muss ich morgen einpacken", json!({"action": "tomorrow"})),
            ("for pe i need my sports kit", json!({"action": "teach_pack", "subject": "sports", "items": "my sports kit"})),
            ("für chemie brauche ich meinen kittel", json!({"action": "teach_pack", "subject": "chemistry", "items": "meinen kittel"})),
            ("what homework is left", json!({"action": "homework"})),
            ("maths homework done", json!({"action": "homework_done", "subject": "mathematics"})),
            ("mathe hausaufgaben erledigt", json!({"action": "homework_done", "subject": "mathematics"})),
            ("ich habe die physik hausaufgaben gemacht", json!({"action": "homework_done", "subject": "physics"})),
            ("make me a study plan", json!({"action": "study_plan"})),
            ("mach mir einen lernplan", json!({"action": "study_plan"})),
            ("add my study plan to the calendar", json!({"action": "plan_to_calendar"})),
            ("what's my week like", json!({"action": "week", "next": false})),
            ("what does my next week look like", json!({"action": "week", "next": true})),
            ("show me my week ahead", json!({"action": "week", "next": false})),
            ("wie sieht meine nächste Woche aus", json!({"action": "week", "next": true})),
            ("zeig mir meine Woche", json!({"action": "week", "next": false})),
        ] {
            assert_eq!(school_command(&crate::intents::normalize_phrase(said)), Some(want.clone()), "{said}");
        }
        for not in ["what do i need", "the homework of the dog", "for you i need nothing", "my week was long", "what a week"] {
            assert_eq!(school_command(&crate::intents::normalize_phrase(not)), None, "{not}");
        }
    }

    #[test]
    fn the_week_ahead_lists_each_day_and_says_what_matters() {
        assert_eq!(week_of(d(18), false), d(16), "a Wednesday is this week");
        assert_eq!(week_of(d(16), false), d(16), "Monday itself");
        assert_eq!(week_of(d(21), false), d(23), "Saturday looks at the next week");
        assert_eq!(week_of(d(22), true), d(23), "so does Sunday, next week or not");
        assert_eq!(week_of(d(18), true), d(23), "next week from a Wednesday");
        let at = |day: u32, p: Period| Period { date: d(day), ..p };
        let periods = [
            at(17, period(t(8, 0), "Mathematics")),
            at(17, period(t(9, 40), "Physics")),
            at(18, Period { cancelled: true, ..period(t(8, 0), "Art") }),
            at(18, Period { cancelled: true, ..period(t(8, 45), "Music") }),
            at(19, Period { cancelled: true, ..period(t(8, 0), "Art") }),
            at(19, period(t(8, 45), "English")),
        ];
        let tasks = [Task { start: Some(t(9, 40)), ..task("e", true, 17, "Physics") }, task("h", false, 19, "Mathematics"), task("done", false, 20, "English"), task("later", false, 24, "English")];
        let events = [
            (d(20).and_hms_opt(15, 30, 0).unwrap(), "Dentist".to_string(), false),
            (d(20).and_hms_opt(9, 0, 0).unwrap(), "Call".to_string(), false),
            (d(21).and_hms_opt(0, 0, 0).unwrap(), "Trip".to_string(), true),
        ];
        let (text, spoken) = week_text(d(16), false, &periods, &tasks, &["done".to_string()], &events);
        assert_eq!(
            text,
            "Mon 16.11.\nTue 17.11.\n    EXAM  Physics (lesson 2)\nWed 18.11.\n    no lessons\nThu 19.11.\n    DUE   Mathematics: p. 42\nFri 20.11.\n    09:00  Call\n    15:30  Dentist\nSat 21.11.\n    all day  Trip\nSun 22.11."
        );
        assert_eq!(spoken, "This week, Sir: one exam, one piece of homework due and three appointments. No lessons on Wednesday.");
        assert_eq!(text.lines().count(), 13, "seven days, one row each item; it fits the 520 px block");
        let (quiet, said) = week_text(d(23), true, &periods, &tasks, &[], &[]);
        assert!(quiet.lines().all(|l| !l.starts_with(' ') || l.contains("DUE")), "{quiet}");
        assert_eq!(said, "Next week, Sir: one piece of homework due.");
        assert_eq!(week_text(d(30), false, &[], &[], &[], &[]).1, "A quiet week, Sir: no exams, no homework due and no appointments.");
    }

    #[test]
    fn tomorrow_is_one_line_with_the_pack_and_the_exam_lesson() {
        let periods = [period(t(9, 40), "Physics"), period(t(8, 0), "Mathematics"), period(t(8, 45), "Sports"), period(t(9, 40), "Physics"), Period { cancelled: true, ..period(t(11, 30), "Art") }];
        let exam = Task { start: Some(t(9, 40)), ..task("e", true, 17, "Physics") };
        let taught = [("mathematics".to_string(), "your compass".to_string())];
        assert_eq!(
            tomorrow_line(&periods, &[exam], &taught),
            "Tomorrow, Sir: Mathematics (pack your compass), Sports (pack your sports kit), Physics. Physics exam in lesson 3."
        );
        assert_eq!(tomorrow_line(&[], &[], &[]), "No lessons tomorrow, Sir.");
    }

    #[test]
    fn homework_is_ticked_off_by_subject_soonest_first() {
        let all = [task("a", false, 20, "Mathematics"), task("b", false, 18, "Mathematics"), task("c", false, 19, "Physics"), task("x", true, 18, "Physics")];
        let open = open_homework(&all, &["c".to_string()]);
        assert_eq!(open.iter().map(|t| t.id.as_str()).collect::<Vec<_>>(), ["b", "a"], "exams and ticked homework are out");
        assert_eq!(homework_of(&open, "mathematics").map(|t| t.id.as_str()), Some("b"), "the soonest");
        assert_eq!(homework_of(&open, "physics"), None);
    }

    #[test]
    fn study_sessions_come_before_each_exam_and_never_in_the_past() {
        let exams = [task("p", true, 20, "Physics"), task("m", true, 17, "Maths"), task("old", true, 15, "Art"), task("far", true, 30, "German")];
        let s = study_sessions(&exams, d(16));
        assert_eq!(s.iter().filter(|x| x.1 == "Maths").map(|x| (x.0, x.2)).collect::<Vec<_>>(), [(d(16), 45)], "only today is left before tomorrow's exam");
        assert_eq!(s.iter().filter(|x| x.1 == "Physics").map(|x| (x.0, x.2)).collect::<Vec<_>>(), [(d(16), 30), (d(17), 30), (d(18), 30), (d(19), 45)]);
        assert!(!s.iter().any(|x| x.1 == "Art"), "an exam in the past");
        assert!(s.iter().any(|x| x.1 == "German"), "within three weeks");
        assert!(plan_text(&s).starts_with("Mon 16.11.  Maths 45 min · Physics 30 min"), "{}", plan_text(&s));
    }

    #[test]
    fn his_points_are_heard_in_english_and_german() {
        for (said, subject, points) in [
            ("i got 13 points in physics", "physics", 13),
            ("I got 9 points in the maths test.", "mathematics", 9),
            ("ich habe 13 punkte in physik", "physics", 13),
            ("ich hab 7 punkte in der deutsch klausur", "german", 7),
            ("ich habe in mathe 11 punkte geschrieben", "mathematics", 11),
            ("i've got 15 points in english", "english", 15),
        ] {
            assert_eq!(grades_command(&said.to_lowercase()), Some(json!({"action": "add", "subject": subject, "points": points})), "{said}");
        }
        for not in ["i got 3 points in a row", "i got 20 points in physics", "i got points in physics", "ich habe 13 punkte"] {
            assert_eq!(grades_command(not), None, "{not}");
        }
        assert_eq!(grades_command("what's my average in physics"), Some(json!({"action": "show", "subject": "physics"})));
        assert_eq!(grades_command("wie ist mein schnitt"), Some(json!({"action": "show"})));
        assert_eq!(grades_command("delete my last grade"), Some(json!({"action": "delete_last"})));
    }

    #[test]
    fn the_next_test_lifts_the_average_to_the_next_whole_point() {
        assert_eq!(needed_for_next(&[12, 13]), Some(14), "12.5 → 13 needs 14");
        assert_eq!(needed_for_next(&[10]), Some(12), "10 → 11 needs 12");
        assert_eq!(needed_for_next(&[3, 3, 3]), Some(7), "3 → 4 needs 7");
        assert_eq!(needed_for_next(&[15]), None, "15 is the top");
        assert_eq!(needed_for_next(&[2]), Some(4));
        assert_eq!(needed_for_next(&[14]), None, "14 → 15 needs 16: no test can");
        assert_eq!(needed_for_next(&[]), None);
    }

    #[test]
    fn points_are_kept_averaged_and_taken_back() {
        let _one = crate::kit::test_lock();
        grades(&json!({"action": "add", "subject": "physics", "points": 12})).unwrap();
        let said = grades(&json!({"action": "add", "subject": "physics", "points": 13})).unwrap();
        assert!(said.contains("averages 12.5") && said.contains("14 next time makes it 13"), "{said}");
        grades(&json!({"action": "add", "subject": "german", "points": 9})).unwrap();
        let text = grades_text(&all_grades());
        assert!(text.contains("Physics  12.5  (2×)") && text.contains("German  9.0") && text.contains("Overall  10.8"), "{text}");
        assert!(grades(&json!({"action": "delete_last"})).unwrap().contains("9 points in german"));
        assert!(grades(&json!({"action": "show", "subject": "german"})).unwrap().contains("No points in german"));
        crate::kit::close_text(GRADES);
    }
}
