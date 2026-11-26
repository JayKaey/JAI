//! The Smart Alarm: "wake me at 6:30 with JaysRock." Ninety minutes before the alarm JAI checks
//! WebUntis: when the first lessons are cancelled he lets Sir sleep until the first real lesson
//! needs him (the same commute buffer). At wake time the playlist starts quietly and fades in, then
//! the music dips and JAI reads the morning briefing.

use crate::features::{spoken, str_arg};
use crate::stark::StarkEvent;
use crate::tools::ToolContext;
use crate::webuntis_client::{Lesson, LessonStatus};
use chrono::{Datelike, Duration as ChronoDuration, Local, NaiveDate, NaiveDateTime, NaiveTime, Timelike, Weekday};
use rusqlite::{params, Connection};
use serde_json::{json, Value};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tracing::{info, warn};

/// How long before the alarm WebUntis is checked.
const DAWN_CHECK_MIN: i64 = 90;
/// The latest a fully free morning is slept in.
const LATEST_WAKE: (u32, u32) = (10, 0);

#[derive(Debug, Clone, PartialEq)]
pub struct Alarm {
    pub id: i64,
    pub wake: NaiveTime,
    pub repeat: String,
    pub adjust: bool,
    pub playlist: Option<String>,
    pub date: NaiveDate,
    pub shifted: Option<NaiveTime>,
    pub reason: String,
    pub fired: Option<NaiveDate>,
    /// What it is for ("call mum"): an alarm with one is no wake-up.
    pub label: String,
}

/// An alarm is ringing: "stop", "I'm up" or "snooze" end it (`hush`).
static RINGING: AtomicBool = AtomicBool::new(false);
/// The ringing one was snoozed: it says nothing now, it rings again soon.
static SNOOZED: AtomicBool = AtomicBool::new(false);
/// How long an alarm rings when nobody stops it.
const RINGS_FOR: Duration = Duration::from_secs(150);
/// "Snooze": how long until it rings again.
const SNOOZE_MIN: i64 = 9;

/// Whether an alarm is ringing now.
pub fn ringing() -> bool {
    RINGING.load(Ordering::Acquire)
}

/// Stops the alarm that rings; whether one did.
pub fn hush() -> bool {
    RINGING.swap(false, Ordering::AcqRel)
}

/// The minute `minutes` from now ("in 20 minutes", a snooze), rounded up: never today's minute
/// that `next_date` would take for tomorrow's.
fn minutes_from_now(minutes: f64) -> NaiveTime {
    let at = (Local::now() + ChronoDuration::seconds((minutes * 60.0) as i64)).time();
    let whole = NaiveTime::from_hms_opt(at.hour(), at.minute(), 0).unwrap_or(at);
    if at == whole { whole } else { whole + ChronoDuration::minutes(1) }
}

/// A wake-up: before eleven, for nothing in particular (the greeting and the morning briefing).
fn wakes_him(a: &Alarm) -> bool {
    a.label.is_empty() && a.effective().time() < NaiveTime::from_hms_opt(11, 0, 0).unwrap_or_default()
}

impl Alarm {
    pub fn effective(&self) -> NaiveDateTime {
        self.date.and_time(self.shifted.unwrap_or(self.wake))
    }
}

#[derive(Clone)]
pub struct AlarmClock {
    conn: Arc<Mutex<Connection>>,
}

/// The next day the alarm applies to (today when the time is still ahead).
pub fn next_date(wake: NaiveTime, repeat: &str, now: NaiveDateTime) -> NaiveDate {
    let mut d = now.date();
    if d.and_time(wake) <= now + ChronoDuration::seconds(30) {
        d += ChronoDuration::days(1);
    }
    if repeat == "weekdays" {
        while matches!(d.weekday(), Weekday::Sat | Weekday::Sun) {
            d += ChronoDuration::days(1);
        }
    }
    d
}

/// The shifted wake time when the morning's first lessons are cancelled (None = keep it).
/// The commute buffer — planned first lesson minus the alarm — stays the same.
pub fn shifted_wake(wake: NaiveTime, lessons: &[Lesson]) -> Option<(NaiveTime, String)> {
    let mut day: Vec<&Lesson> = lessons.iter().collect();
    if day.is_empty() {
        return None;
    }
    day.sort_by_key(|l| l.start);
    let planned = day[0].start;
    let real: Vec<&&Lesson> = day.iter().filter(|l| l.status != LessonStatus::Cancelled).collect();
    let latest = NaiveTime::from_hms_opt(LATEST_WAKE.0, LATEST_WAKE.1, 0)?;
    match real.first() {
        None => {
            let to = latest.max(wake);
            (to > wake).then(|| (to, "every lesson is cancelled".to_string()))
        }
        Some(first) if first.start > planned => {
            let buffer = planned.signed_duration_since(wake);
            let to = (first.start - buffer).min(latest.max(wake));
            let dropped: Vec<String> = day.iter().take_while(|l| l.start < first.start).map(|l| l.display_subject()).collect();
            (to > wake).then(|| (to, format!("{} cancelled; first real lesson {} at {}", dropped.join(", "), first.display_subject(), first.start.format("%H:%M"))))
        }
        _ => None,
    }
}

fn parse_time(raw: &str) -> Option<NaiveTime> {
    let r = raw.trim().to_lowercase().replace('.', ":");
    let pm = r.contains("pm");
    let digits: String = r.chars().filter(|c| c.is_ascii_digit() || *c == ':').collect();
    let (h, m) = match digits.split_once(':') {
        Some((h, m)) => (h.parse::<u32>().ok()?, m.parse::<u32>().ok()?),
        None if digits.len() >= 3 => (digits[..digits.len() - 2].parse().ok()?, digits[digits.len() - 2..].parse().ok()?),
        None => (digits.parse().ok()?, 0),
    };
    NaiveTime::from_hms_opt(if pm && h < 12 { h + 12 } else { h }, m, 0)
}

impl AlarmClock {
    pub fn open(path: &Path) -> Result<Self, String> {
        let conn = Connection::open(path).map_err(|e| e.to_string())?;
        conn.busy_timeout(Duration::from_secs(3)).map_err(|e| e.to_string())?;
        Self::init(conn)
    }

    pub fn in_memory() -> Self {
        Self::init(Connection::open_in_memory().expect("in-memory SQLite")).expect("alarm schema")
    }

    fn init(conn: Connection) -> Result<Self, String> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS smart_alarms (id INTEGER PRIMARY KEY, wake TEXT NOT NULL, repeat TEXT NOT NULL DEFAULT 'once', adjust INTEGER NOT NULL DEFAULT 1, playlist TEXT, active INTEGER NOT NULL DEFAULT 1, date TEXT NOT NULL, shifted TEXT, reason TEXT NOT NULL DEFAULT '', fired TEXT);",
        )
        .map_err(|e| e.to_string())?;
        // (Older tables have no label yet; the error of one that has is the answer.)
        let _ = conn.execute("ALTER TABLE smart_alarms ADD COLUMN label TEXT NOT NULL DEFAULT ''", []);
        Ok(Self { conn: Arc::new(Mutex::new(conn)) })
    }

    fn db(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn list(&self) -> Vec<Alarm> {
        let db = self.db();
        let Ok(mut st) = db.prepare("SELECT id, wake, repeat, adjust, playlist, date, shifted, reason, fired, label FROM smart_alarms WHERE active = 1 ORDER BY date, wake") else { return Vec::new() };
        let time = |s: String| NaiveTime::parse_from_str(&s, "%H:%M").ok();
        let date = |s: String| NaiveDate::parse_from_str(&s, "%Y-%m-%d").ok();
        st.query_map([], |r| {
            Ok((
                (r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?, r.get::<_, bool>(3)?, r.get::<_, Option<String>>(4)?),
                (r.get::<_, String>(5)?, r.get::<_, Option<String>>(6)?, r.get::<_, String>(7)?, r.get::<_, Option<String>>(8)?, r.get::<_, String>(9)?),
            ))
        })
        .map(|rows| {
            rows.flatten()
                .filter_map(|((id, wake, repeat, adjust, playlist), (d, shifted, reason, fired, label))| {
                    Some(Alarm { id, wake: time(wake)?, repeat, adjust, playlist, date: date(d)?, shifted: shifted.and_then(time), reason, fired: fired.and_then(date), label })
                })
                .collect()
        })
        .unwrap_or_default()
    }

    pub fn describe(&self) -> Option<String> {
        let a = self.list().into_iter().next()?;
        Some(format!(
            "Smart alarm: {} {}{}",
            a.date.format("%a %d.%m."),
            a.shifted.unwrap_or(a.wake).format("%H:%M"),
            if a.shifted.is_some_and(|s| s != a.wake) { format!(" (moved from {}: {})", a.wake.format("%H:%M"), a.reason) } else { String::new() }
        ))
    }

    fn add(&self, wake: NaiveTime, repeat: &str, adjust: bool, playlist: Option<&str>, label: &str) -> Result<Alarm, String> {
        let date = next_date(wake, repeat, Local::now().naive_local());
        let db = self.db();
        db.execute(
            "INSERT INTO smart_alarms (wake, repeat, adjust, playlist, date, label) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![wake.format("%H:%M").to_string(), repeat, adjust, playlist, date.format("%Y-%m-%d").to_string(), label],
        )
        .map_err(|e| e.to_string())?;
        Ok(Alarm { id: db.last_insert_rowid(), wake, repeat: repeat.into(), adjust, playlist: playlist.map(String::from), date, shifted: None, reason: String::new(), fired: None, label: label.into() })
    }

    fn cancel_all(&self) -> usize {
        self.db().execute("UPDATE smart_alarms SET active = 0 WHERE active = 1", []).unwrap_or(0)
    }

    /// "Cancel the 7 pm alarm": only the alarms set for that time.
    fn cancel_at(&self, wake: NaiveTime) -> usize {
        self.db().execute("UPDATE smart_alarms SET active = 0 WHERE active = 1 AND wake = ?1", params![wake.format("%H:%M").to_string()]).unwrap_or(0)
    }

    fn set_shift(&self, id: i64, to: NaiveTime, reason: &str) {
        let _ = self.db().execute("UPDATE smart_alarms SET shifted = ?1, reason = ?2 WHERE id = ?3", params![to.format("%H:%M").to_string(), reason, id]);
    }

    /// After ringing (or a missed alarm): the next occurrence, or off for a one-off.
    fn advance(&self, a: &Alarm) {
        let db = self.db();
        if a.repeat == "once" {
            let _ = db.execute("UPDATE smart_alarms SET active = 0, fired = ?1 WHERE id = ?2", params![a.date.format("%Y-%m-%d").to_string(), a.id]);
        } else {
            let after = a.date.and_time(a.wake) + ChronoDuration::minutes(1);
            let next = next_date(a.wake, &a.repeat, after.max(Local::now().naive_local()));
            let _ = db.execute(
                "UPDATE smart_alarms SET date = ?1, shifted = NULL, reason = '', fired = ?2 WHERE id = ?3",
                params![next.format("%Y-%m-%d").to_string(), a.date.format("%Y-%m-%d").to_string(), a.id],
            );
        }
    }

    pub fn start(&self, tools: ToolContext) {
        // (A scripted session reads Sir's alarms but never rings them: his music, his volume.)
        if crate::platform::in_test_session() {
            return;
        }
        let me = self.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_secs(20));
            loop {
                tick.tick().await;
                let now = Local::now().naive_local();
                for a in me.list() {
                    let planned = a.date.and_time(a.wake);
                    // Dawn check: are the first lessons cancelled?
                    if a.adjust && a.shifted.is_none() && now >= planned - ChronoDuration::minutes(DAWN_CHECK_MIN) && now < planned {
                        // No school at all that day (holidays, every lesson cancelled): a repeating
                        // school alarm keeps quiet. (One set for that very day still rings.)
                        let free = if a.repeat == "once" { None } else { tools.pim.no_school(a.date).await };
                        if let Some(why) = free {
                            info!("[ALARM] Not ringing on {}: {why}", a.date);
                            me.advance(&a);
                            let _ = tools.stark.events.send(StarkEvent::Notice(format!("School alarm off today: {why}")));
                            continue;
                        }
                        match tools.pim.lessons(a.date, a.date).await {
                            Ok(lessons) => match shifted_wake(a.wake, &lessons) {
                                Some((to, reason)) => {
                                    info!("[ALARM] Moved to {to} ({reason})");
                                    me.set_shift(a.id, to, &reason);
                                    tools.stark.pulse(&["alarm", "webuntis"], false);
                                    let _ = tools.stark.events.send(StarkEvent::Notice(format!("Alarm moved to {}: {reason}", to.format("%H:%M"))));
                                }
                                None => me.set_shift(a.id, a.wake, "timetable unchanged"),
                            },
                            Err(e) => {
                                warn!("[ALARM] WebUntis check failed: {e}");
                                me.set_shift(a.id, a.wake, "WebUntis unreachable");
                            }
                        }
                        continue;
                    }
                    let due = a.effective();
                    if now >= due {
                        if now - due < ChronoDuration::minutes(30) {
                            me.advance(&a);
                            ring(tools.clone(), a.clone()).await;
                        } else {
                            info!("[ALARM] Missed the {} alarm (JAI was not running)", due.format("%d.%m. %H:%M"));
                            me.advance(&a);
                        }
                    }
                }
            }
        });
    }

    /// `set_smart_morning_alarm`.
    pub fn tool(&self, tools: &ToolContext, args: &Value) -> Result<String, String> {
        let action = str_arg(args, &["action"]).unwrap_or(if str_arg(args, &["wake_time", "time"]).is_some() || args.get("in_minutes").is_some() { "set" } else { "list" }).to_lowercase();
        match action.as_str() {
            // (While one rings, "turn the alarm off" is that one, today: the rest stay set.)
            "cancel" | "delete" | "off" | "remove" | "stop" if hush() => Ok(spoken("Stopped the ringing alarm.", "Alarm off, Sir.")),
            "stop" => Ok(spoken("No alarm was ringing.", "No alarm is ringing, Sir.")),
            "cancel" | "delete" | "off" | "remove" => {
                let at = str_arg(args, &["wake_time", "time"]).and_then(parse_time);
                let n = at.map_or_else(|| self.cancel_all(), |at| self.cancel_at(at));
                let which = at.map_or_else(|| "Alarm cancelled".to_string(), |at| format!("The {} alarm is cancelled", at.format("%H:%M")));
                Ok(spoken(format!("Cancelled {n} alarm(s)."), if n == 0 { "There was no such alarm set, Sir.".to_string() } else { format!("{which}, Sir.") }))
            }
            "snooze" => {
                SNOOZED.store(hush(), Ordering::Release);
                let wake = minutes_from_now(SNOOZE_MIN as f64);
                self.add(wake, "once", false, None, "")?;
                Ok(spoken(format!("Snoozed until {}.", wake.format("%H:%M")), format!("{SNOOZE_MIN} more minutes, Sir.")))
            }
            "set" | "add" | "create" => {
                // "In 20 minutes": from now, to the minute.
                let wake = match args.get("in_minutes").and_then(Value::as_f64).filter(|m| *m >= 1.0) {
                    Some(minutes) => minutes_from_now(minutes),
                    None => parse_time(str_arg(args, &["wake_time", "time"]).ok_or("For what time, Sir?")?).ok_or("I didn't catch the time, Sir.")?,
                };
                let label = str_arg(args, &["label", "reason", "for"]).unwrap_or("").trim();
                let repeat = match str_arg(args, &["repeat"]).unwrap_or("once").to_lowercase().as_str() {
                    r if r.contains("week") || r.contains("school") => "weekdays",
                    r if r.contains("daily") || r.contains("every") => "daily",
                    _ => "once",
                };
                // (Only a wake-up waits for the timetable: an alarm at 7 pm or for something is kept.)
                let morning = label.is_empty() && wake < NaiveTime::from_hms_opt(11, 0, 0).unwrap_or_default() && args.get("in_minutes").is_none();
                let adjust = crate::tools::bool_arg(args, "adjust_for_cancelled_classes").unwrap_or(true) && morning;
                let playlist = str_arg(args, &["playlist_name", "playlist"]);
                let a = self.add(wake, repeat, adjust, playlist, label)?;
                let when = if a.date == Local::now().date_naive() { "today".to_string() } else if a.date == Local::now().date_naive() + ChronoDuration::days(1) { "tomorrow".to_string() } else { a.date.format("%A").to_string() };
                tools.stark.pulse(&["brain", "alarm", "webuntis"], false);
                Ok(spoken(
                    format!(
                        "Alarm: {when} ({}) at {}{}{}{}{}",
                        a.date.format("%d.%m."),
                        wake.format("%H:%M"),
                        if repeat == "once" { String::new() } else { format!(", {repeat}") },
                        if label.is_empty() { String::new() } else { format!(", for \"{label}\"") },
                        playlist.map(|p| format!(", fading in \"{p}\"")).unwrap_or_default(),
                        if adjust { format!(", WebUntis checked {DAWN_CHECK_MIN} min before — cancelled first lessons move it later") } else { String::new() }
                    ),
                    format!(
                        "Alarm set for {} {when}, Sir{}{}{}.",
                        wake.format("%H:%M"),
                        if label.is_empty() { String::new() } else { format!(": {label}") },
                        playlist.map(|p| format!(", with {p}")).unwrap_or_default(),
                        if adjust { ". If your first lessons are cancelled, I'll let you sleep in" } else { "" }
                    ),
                ))
            }
            _ => {
                let all = self.list();
                if all.is_empty() {
                    return Ok(spoken("No alarms are set.", "No alarm is set, Sir."));
                }
                let lines: Vec<String> = all
                    .iter()
                    .map(|a| {
                        let label = if a.label.is_empty() { String::new() } else { format!(" \"{}\"", a.label) };
                        format!("{} {}{label} ({}{}){}", a.date.format("%a %d.%m."), a.shifted.unwrap_or(a.wake).format("%H:%M"), a.repeat, a.playlist.as_ref().map(|p| format!(", {p}")).unwrap_or_default(), if a.reason.is_empty() { String::new() } else { format!(" — {}", a.reason) })
                    })
                    .collect();
                let first = &all[0];
                Ok(spoken(lines.join("\n"), format!("Your next alarm is {} at {}, Sir.", first.date.format("%A"), first.shifted.unwrap_or(first.wake).format("%H:%M"))))
            }
        }
    }
}

/// The system volume, set quietly (no volume bar, nothing for "more" to go on from): Core
/// Audio, on Linux wpctl / pactl.
async fn quiet_volume(level: f32) {
    #[cfg(windows)]
    let _ = crate::win32::set_volume(level);
    #[cfg(not(windows))]
    let _ = crate::media::execute(crate::media::MediaAction::SetVolume((level * 100.0).round() as u8)).await;
}

/// The alarm tone again and again, until he stops it (`hush`) or it has rung `RINGS_FOR`;
/// whether he snoozed it.
async fn sound_until_stopped() -> bool {
    SNOOZED.store(false, Ordering::Release);
    RINGING.store(true, Ordering::Release);
    let started = std::time::Instant::now();
    while ringing() && started.elapsed() < RINGS_FOR {
        crate::tts::play_earcon(crate::jai_voice_dsp::Earcon::Alarm);
        // (The ear is shut while the tone plays: the gap is room for his "stop".)
        tokio::time::sleep(Duration::from_millis(2000)).await;
    }
    RINGING.store(false, Ordering::Release);
    SNOOZED.swap(false, Ordering::AcqRel)
}

/// An alarm for something ("call mum", or at 7 pm): the tone until he stops it, then what it is
/// for. A wake-up: music fading in (or the tone), then the greeting and the morning briefing.
async fn ring(tools: ToolContext, a: Alarm) {
    if !wakes_him(&a) {
        info!("[ALARM] Ringing ({}: {})", a.effective().format("%H:%M"), a.label);
        tools.stark.pulse(&["alarm", "voice"], false);
        let _ = tools.stark.events.send(StarkEvent::Notice(format!("Alarm {}{}", a.effective().format("%H:%M"), if a.label.is_empty() { String::new() } else { format!(": {}", a.label) })));
        if sound_until_stopped().await {
            return;
        }
        let at = a.effective().format("%H:%M");
        let line = if a.label.is_empty() { format!("It's {at}, Sir. That was your alarm.") } else { format!("It's {at}, Sir: {}.", a.label) };
        let _ = tools.stark.events.send(StarkEvent::Alert(line));
        return;
    }
    wake_up(tools, a).await;
}

/// Music fading in (or the alarm tone), then the greeting and the morning briefing.
async fn wake_up(tools: ToolContext, a: Alarm) {
    let settings = tools.config.read(|c| c.features.clone());
    info!("[ALARM] Ringing ({})", a.effective().format("%H:%M"));
    tools.stark.pulse(&["alarm", "music", "voice"], false);
    let target = (settings.alarm_volume.clamp(5, 100) as f32) / 100.0;
    let mut music = false;
    if let Some(p) = &a.playlist {
        quiet_volume(0.05).await;
        let out = Box::pin(tools.execute("control_music", &json!({ "action": "play", "playlist_name": p }))).await;
        music = out.success;
        if music {
            let steps = (settings.alarm_fade_secs.clamp(10, 900) / 5).max(1);
            for i in 1..=steps {
                tokio::time::sleep(Duration::from_secs(5)).await;
                quiet_volume(0.05 + (target - 0.05) * i as f32 / steps as f32).await;
            }
        } else {
            quiet_volume(target).await;
        }
    }
    // (No music: the tone wakes him; snoozed, the greeting waits for the next ring.)
    if !music && sound_until_stopped().await {
        return;
    }
    let shift = if a.shifted.is_some_and(|s| s != a.wake) { format!(" I let you sleep in a little: {}.", a.reason) } else { String::new() };
    let greeting = format!("Good morning, Sir. It's {}.{shift}", a.effective().format("%H:%M"));
    if music {
        let _ = Box::pin(tools.execute("control_music", &json!({ "action": "pause" }))).await;
    }
    let briefing = crate::briefing::executive(&tools, Some("morning")).await;
    let briefing = briefing.rsplit("\nSpoken: ").next().unwrap_or(&briefing).chars().take(900).collect::<String>();
    let _ = tools.stark.events.send(StarkEvent::Alert(format!("{greeting} {briefing}")));
    if music {
        // Back to the music once the briefing has been read.
        tokio::time::sleep(Duration::from_secs(40)).await;
        let _ = Box::pin(tools.execute("control_music", &json!({ "action": "resume" }))).await;
    }
}

pub fn tool_definition() -> Value {
    json!({"type": "function", "function": {
        "name": "set_smart_morning_alarm",
        "description": "Alarms: ring at wake_time (\"06:30\") or in_minutes from now, with a label for what it is for. A morning wake-up checks WebUntis 90 min before (adjust_for_cancelled_classes, default true) and moves later when the first lessons are cancelled; playlist_name fades in (e.g. JaysRock), then the morning briefing is read. Without music the alarm tone rings until Sir says stop. repeat once/weekdays/daily. action list, cancel (wake_time: only that one; while ringing: stops it), snooze.",
        "parameters": {"type": "object", "properties": {
            "wake_time": {"type": "string"},
            "in_minutes": {"type": "number"},
            "label": {"type": "string"},
            "adjust_for_cancelled_classes": {"type": "boolean"},
            "playlist_name": {"type": "string"},
            "repeat": {"type": "string", "enum": ["once", "weekdays", "daily"]},
            "action": {"type": "string", "enum": ["set", "list", "cancel", "snooze"]}
        }}
    }})
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lesson(h: u32, m: u32, subject: &str, cancelled: bool) -> Lesson {
        Lesson {
            date: NaiveDate::from_ymd_opt(2026, 10, 23).unwrap(),
            start: NaiveTime::from_hms_opt(h, m, 0).unwrap(),
            end: NaiveTime::from_hms_opt(h, m, 0).unwrap() + ChronoDuration::minutes(45),
            subject: subject.into(),
            subject_long: String::new(),
            teachers: vec![],
            rooms: vec![],
            original_rooms: vec![],
            original_teachers: vec![],
            status: if cancelled { LessonStatus::Cancelled } else { LessonStatus::Regular },
            note: String::new(),
        }
    }

    #[test]
    fn cancelled_first_lessons_move_the_alarm() {
        let wake = NaiveTime::from_hms_opt(6, 30, 0).unwrap();
        let day = [lesson(7, 50, "M", true), lesson(8, 40, "D", true), lesson(9, 45, "PH", false)];
        let (to, why) = shifted_wake(wake, &day).unwrap();
        // Same 80-minute buffer before the first real lesson.
        assert_eq!(to, NaiveTime::from_hms_opt(8, 25, 0).unwrap());
        assert!(why.contains("M, D cancelled"), "{why}");
        assert_eq!(shifted_wake(wake, &[lesson(7, 50, "M", false)]), None);
        assert_eq!(shifted_wake(wake, &[lesson(7, 50, "M", true)]).unwrap().0, NaiveTime::from_hms_opt(10, 0, 0).unwrap());
        assert_eq!(parse_time("6:30"), NaiveTime::from_hms_opt(6, 30, 0));
        assert_eq!(parse_time("7 pm"), NaiveTime::from_hms_opt(19, 0, 0));
        assert_eq!(parse_time("0645"), NaiveTime::from_hms_opt(6, 45, 0));
        let friday_night = NaiveDate::from_ymd_opt(2026, 10, 23).unwrap().and_hms_opt(22, 0, 0).unwrap();
        assert_eq!(next_date(wake, "weekdays", friday_night).weekday(), Weekday::Mon);
        let clock = AlarmClock::in_memory();
        clock.add(wake, "once", true, Some("JaysRock"), "").unwrap();
        assert_eq!(clock.list().len(), 1);
        assert_eq!(clock.cancel_all(), 1);
    }

    /// Sir, 2026-11-26: "JAI should be able to set alarms". One for 7 pm is no wake-up (no "good
    /// morning", no timetable); one for something says what for; one time cancels only its own.
    #[test]
    fn alarms_for_any_time_say_what_for_and_cancel_one_by_one() {
        let clock = AlarmClock::in_memory();
        let at = |h, m| NaiveTime::from_hms_opt(h, m, 0).unwrap();
        let morning = clock.add(at(6, 30), "weekdays", true, None, "").unwrap();
        let evening = clock.add(at(19, 0), "once", false, None, "").unwrap();
        let called = clock.add(at(7, 0), "once", false, None, "call mum").unwrap();
        assert!(wakes_him(&morning));
        assert!(!wakes_him(&evening), "7 pm is no wake-up");
        assert!(!wakes_him(&called), "an alarm for something is no wake-up, even at 7");
        assert_eq!(clock.list().iter().find(|a| a.id == called.id).map(|a| a.label.as_str()), Some("call mum"), "the label is kept");
        assert_eq!(clock.cancel_at(at(19, 0)), 1);
        assert_eq!(clock.list().len(), 2, "only the 7 pm one is gone");
        // Ringing: "stop" ends it once; there is nothing to stop after.
        RINGING.store(true, Ordering::Release);
        assert!(ringing() && hush() && !ringing() && !hush());
        // An older table without labels gets the column, and what it held stays.
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE smart_alarms (id INTEGER PRIMARY KEY, wake TEXT NOT NULL, repeat TEXT NOT NULL DEFAULT 'once', adjust INTEGER NOT NULL DEFAULT 1, playlist TEXT, active INTEGER NOT NULL DEFAULT 1, date TEXT NOT NULL, shifted TEXT, reason TEXT NOT NULL DEFAULT '', fired TEXT); INSERT INTO smart_alarms (wake, date) VALUES ('06:30', '2030-01-07');").unwrap();
        let old = AlarmClock::init(conn).unwrap();
        assert_eq!(old.list().iter().map(|a| (a.wake, a.label.as_str())).collect::<Vec<_>>(), [(at(6, 30), "")]);
    }
}
