//! The routine learner: what Sir asks for at about the same time on several days becomes a
//! one-click offer ("you usually open your WebUntis schedule about now") and an answer to "what
//! do I usually do now".
//!
//! Only actions are noted — which tool ran with which arguments, when — never what was said
//! around them, and only for the kinds of command a routine is made of (opening things, music,
//! the schedule, protocols, the look). A habit is the same action inside the same ninety-minute
//! window on at least three different days of the last three weeks. An offer is made once per
//! habit and day, never for something already done today.

use crate::features::{spoken, str_arg};
use chrono::{Datelike, NaiveDateTime, Timelike};
use rusqlite::{params, Connection};
use serde_json::{json, Value};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// How many different days an action must have happened on to be a habit.
pub const MIN_DAYS: usize = 3;
/// How far back habits are learned from.
pub const WINDOW_DAYS: i64 = 21;
/// An action counts for "now" when it usually happens within this many minutes of it.
pub const NEAR_MINUTES: i64 = 45;

/// The tools a routine is made of (reads, the look, music, opening things, protocols).
const ROUTINE_TOOLS: &[&str] = &[
    "open_application",
    "open_website",
    "open_jai_view",
    "control_music",
    "check_school_schedule",
    "get_calendar_schedule",
    "check_email",
    "run_protocol",
    "configure_interface",
    "launch_interactive_simulation",
    "manage_flashcards",
    "get_precise_location_weather",
    "check_ride_and_commute_status",
    "daily_briefing",
    "tile_windows",
];

/// One noted action.
#[derive(Debug, Clone, PartialEq)]
pub struct Event {
    pub at: NaiveDateTime,
    pub tool: String,
    pub args: Value,
    /// A few words for the offer ("open the WebUntis schedule").
    pub label: String,
}

/// Something Sir does at about the same time on several days.
#[derive(Debug, Clone, PartialEq)]
pub struct Habit {
    pub tool: String,
    pub args: Value,
    pub label: String,
    /// The different days it happened on.
    pub days: usize,
    /// Its usual time as minutes after midnight.
    pub usual_minute: i64,
}

/// A few words for an action, for the offer card.
pub fn label_for(tool: &str, args: &Value) -> String {
    let arg = |keys: &[&str]| keys.iter().find_map(|k| args[*k].as_str()).unwrap_or("").to_string();
    match tool {
        "open_application" => format!("open {}", arg(&["app_name", "name"])),
        "open_website" => format!("open {}", arg(&["platform_or_url", "url", "site"])),
        "open_jai_view" => format!("open the {} view", arg(&["view"]).replace('_', " ")),
        "control_music" => match arg(&["playlist", "playlist_name", "query", "song"]).as_str() {
            "" => format!("music: {}", arg(&["action"]).replace('_', " ")),
            what => format!("play {what}"),
        },
        "check_school_schedule" => "check the school schedule".to_string(),
        "get_calendar_schedule" => "check the calendar".to_string(),
        "check_email" => "check the mail".to_string(),
        "run_protocol" => format!("run the {} protocol", arg(&["name", "protocol"])),
        "manage_flashcards" => "study the flashcards".to_string(),
        other => other.replace('_', " "),
    }
    .trim()
    .to_string()
}

/// Whether an action is the kind a routine is made of.
pub fn is_routine(tool: &str, args: &Value) -> bool {
    if !ROUTINE_TOOLS.contains(&tool) {
        return false;
    }
    match tool {
        // Starting something, never stopping or skipping it.
        "control_music" => matches!(args["action"].as_str().unwrap_or(""), "play" | "play_playlist" | "play_song" | "resume" | "welcome_home"),
        "manage_flashcards" => matches!(args["action"].as_str().unwrap_or("quiz"), "quiz" | "start" | "study"),
        "configure_interface" => args.get("hud_palette").is_some(),
        _ => true,
    }
}

/// The habits in `events` that belong to the time `now`: the same action within `NEAR_MINUTES`
/// of now on at least `MIN_DAYS` different days of the last `WINDOW_DAYS`, most regular first.
/// An action already done today is no offer.
pub fn habits_now(events: &[Event], now: NaiveDateTime) -> Vec<Habit> {
    let minute_of = |t: &NaiveDateTime| (t.hour() * 60 + t.minute()) as i64;
    let now_minute = minute_of(&now);
    // Minutes between two times of day, around midnight.
    let apart = |a: i64, b: i64| {
        let d = (a - b).abs();
        d.min(1440 - d)
    };
    let mut groups: Vec<(String, Value, Vec<&Event>)> = Vec::new();
    for event in events {
        let age = (now.date() - event.at.date()).num_days();
        if !(0..=WINDOW_DAYS).contains(&age) {
            continue;
        }
        match groups.iter_mut().find(|(tool, args, _)| *tool == event.tool && *args == event.args) {
            Some((_, _, list)) => list.push(event),
            None => groups.push((event.tool.clone(), event.args.clone(), vec![event])),
        }
    }
    let mut habits: Vec<Habit> = Vec::new();
    for (tool, args, list) in groups {
        if list.iter().any(|e| e.at.date() == now.date()) {
            continue;
        }
        let near: Vec<&&Event> = list.iter().filter(|e| apart(minute_of(&e.at), now_minute) <= NEAR_MINUTES).collect();
        let mut days: Vec<i32> = near.iter().map(|e| e.at.date().num_days_from_ce()).collect();
        days.sort_unstable();
        days.dedup();
        if days.len() < MIN_DAYS {
            continue;
        }
        // The usual time: the mean of the offsets from now (so 23:50 and 00:10 average to midnight).
        let offset = |e: &&&Event| {
            let d = minute_of(&e.at) - now_minute;
            if d > 720 { d - 1440 } else if d < -720 { d + 1440 } else { d }
        };
        let mean = near.iter().map(offset).sum::<i64>() / near.len() as i64;
        habits.push(Habit { label: list.last().map(|e| e.label.clone()).unwrap_or_default(), tool, args, days: days.len(), usual_minute: (now_minute + mean).rem_euclid(1440) });
    }
    habits.sort_by(|a, b| b.days.cmp(&a.days).then(apart(a.usual_minute, now_minute).cmp(&apart(b.usual_minute, now_minute))));
    habits
}

/// "07:15".
pub fn clock(minute: i64) -> String {
    format!("{:02}:{:02}", minute / 60, minute % 60)
}

#[derive(Clone)]
pub struct Routines {
    conn: Arc<Mutex<Connection>>,
    /// Habits already offered today, as "day|tool|args".
    offered: Arc<Mutex<Vec<String>>>,
}

impl Routines {
    pub fn open(path: &Path) -> Result<Self, String> {
        let conn = Connection::open(path).map_err(|e| e.to_string())?;
        conn.busy_timeout(Duration::from_secs(3)).map_err(|e| e.to_string())?;
        Self::init(conn)
    }

    pub fn in_memory() -> Self {
        Self::init(Connection::open_in_memory().expect("in-memory SQLite")).expect("routine schema")
    }

    fn init(conn: Connection) -> Result<Self, String> {
        conn.execute_batch("CREATE TABLE IF NOT EXISTS routine_events (id INTEGER PRIMARY KEY, at TEXT NOT NULL, tool TEXT NOT NULL, args TEXT NOT NULL, label TEXT NOT NULL); CREATE INDEX IF NOT EXISTS routine_events_at ON routine_events(at);").map_err(|e| e.to_string())?;
        Ok(Self { conn: Arc::new(Mutex::new(conn)), offered: Arc::new(Mutex::new(Vec::new())) })
    }

    fn conn(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Notes an action Sir asked for (only the kinds a routine is made of). Events older than
    /// the learning window are dropped as it goes.
    pub fn note(&self, tool: &str, args: &Value, at: NaiveDateTime) {
        if !is_routine(tool, args) {
            return;
        }
        let conn = self.conn();
        let _ = conn.execute("INSERT INTO routine_events (at, tool, args, label) VALUES (?1, ?2, ?3, ?4)", params![at.format("%Y-%m-%d %H:%M:%S").to_string(), tool, args.to_string(), label_for(tool, args)]);
        let cutoff = (at - chrono::Duration::days(WINDOW_DAYS + 1)).format("%Y-%m-%d %H:%M:%S").to_string();
        let _ = conn.execute("DELETE FROM routine_events WHERE at < ?1", params![cutoff]);
    }

    pub fn events(&self) -> Vec<Event> {
        let conn = self.conn();
        let Ok(mut stmt) = conn.prepare("SELECT at, tool, args, label FROM routine_events ORDER BY at") else { return Vec::new() };
        stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?, r.get::<_, String>(3)?)))
            .map(|rows| rows.flatten().filter_map(|(at, tool, args, label)| Some(Event { at: NaiveDateTime::parse_from_str(&at, "%Y-%m-%d %H:%M:%S").ok()?, tool, args: serde_json::from_str(&args).ok()?, label })).collect())
            .unwrap_or_default()
    }

    pub fn forget_all(&self) -> usize {
        self.offered.lock().unwrap_or_else(|e| e.into_inner()).clear();
        self.conn().execute("DELETE FROM routine_events", []).unwrap_or(0)
    }

    /// The habit to offer at `now`, if there is one that has not been offered today.
    pub fn offer(&self, now: NaiveDateTime) -> Option<Habit> {
        let habit = habits_now(&self.events(), now).into_iter().next()?;
        let key = format!("{}|{}|{}", now.date(), habit.tool, habit.args);
        let mut offered = self.offered.lock().unwrap_or_else(|e| e.into_inner());
        if offered.contains(&key) {
            return None;
        }
        offered.retain(|k| k.starts_with(&now.date().to_string()));
        offered.push(key);
        Some(habit)
    }

    /// The `suggest_routine` tool (its `run` action is carried out by the hub, which can call
    /// other tools).
    pub fn tool(&self, args: &Value, now: NaiveDateTime) -> Result<String, String> {
        match str_arg(args, &["action"]).unwrap_or("now").to_lowercase().as_str() {
            "forget" | "reset" | "clear" => {
                let n = self.forget_all();
                Ok(spoken(format!("Forgot {n} noted action(s)."), "Forgotten, Sir. I'll start learning your routines afresh."))
            }
            "list" | "all" => {
                let events = self.events();
                let mut lines: Vec<String> = Vec::new();
                // Every habit of the day, asked hour by hour (as of tomorrow, so that what Sir
                // has already done today is listed too).
                for hour in 0..24 {
                    let Some(at) = now.date().and_hms_opt(hour, 30, 0) else { continue };
                    for habit in habits_now(&events, at + chrono::Duration::days(1)) {
                        let line = format!("{}  {} ({} days)", clock(habit.usual_minute), habit.label, habit.days);
                        if !lines.contains(&line) {
                            lines.push(line);
                        }
                    }
                }
                lines.sort();
                if lines.is_empty() {
                    return Ok(spoken(format!("No routines yet ({} actions noted).", events.len()), "I haven't seen a routine yet, Sir. Three days of the same thing at the same time make one."));
                }
                Ok(spoken(format!("Your routines\n{}", lines.join("\n")), format!("I know {} of your routines, Sir. They're on the board.", lines.len())))
            }
            _ => {
                let habits = habits_now(&self.events(), now);
                match habits.first() {
                    None => Ok(spoken("Nothing you usually do at this time.", "Nothing in particular at this hour, Sir.")),
                    Some(first) => {
                        let lines: Vec<String> = habits.iter().map(|h| format!("{}  {} ({} days)", clock(h.usual_minute), h.label, h.days)).collect();
                        Ok(spoken(format!("Around now you usually\n{}", lines.join("\n")), format!("Around now you usually {}, Sir. Shall I?", first.label)))
                    }
                }
            }
        }
    }
}

pub fn tool_definition() -> Value {
    json!({"type": "function", "function": {
        "name": "suggest_routine",
        "description": "Sir's routines, learned from what he asks for at the same time on several days. action now (what he usually does at this hour), list (every routine with its usual time), run (carries out the routine that belongs to now), forget (drops everything learned).",
        "parameters": {"type": "object", "properties": {
            "action": {"type": "string", "enum": ["now", "list", "run", "forget"]}
        }, "required": []}
    }})
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    fn at(day: u32, h: u32, m: u32) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(2026, 11, day).unwrap().and_hms_opt(h, m, 0).unwrap()
    }

    fn schedule(day: u32, h: u32, m: u32) -> Event {
        Event { at: at(day, h, m), tool: "open_jai_view".into(), args: json!({"view": "schedule"}), label: "open the schedule view".into() }
    }

    #[test]
    fn the_same_action_at_the_same_time_on_three_days_is_a_habit() {
        let mut events = vec![schedule(2, 7, 10), schedule(3, 7, 25), schedule(4, 6, 55)];
        let habits = habits_now(&events, at(6, 7, 15));
        assert_eq!(habits.len(), 1);
        assert_eq!((habits[0].label.as_str(), habits[0].days, clock(habits[0].usual_minute)), ("open the schedule view", 3, "07:10".to_string()));
        // Two days are a coincidence; a different hour is another time of day.
        assert!(habits_now(&events[..2], at(6, 7, 15)).is_empty());
        assert!(habits_now(&events, at(6, 12, 0)).is_empty());
        // Twice on one day is still one day.
        assert!(habits_now(&[schedule(2, 7, 10), schedule(2, 7, 12), schedule(3, 7, 25)], at(6, 7, 15)).is_empty());
        // Already done today: no offer.
        events.push(schedule(6, 7, 5));
        assert!(habits_now(&events, at(6, 7, 15)).is_empty());
        // Older than three weeks: forgotten.
        assert!(habits_now(&[schedule(2, 7, 10), schedule(3, 7, 25), schedule(4, 6, 55)], at(28, 7, 15)).is_empty());
        // Other arguments are another action.
        let other = Event { args: json!({"view": "system_stats"}), ..schedule(5, 7, 0) };
        assert_eq!(habits_now(&[schedule(2, 7, 10), schedule(3, 7, 25), other], at(6, 7, 15)).len(), 0);
    }

    #[test]
    fn a_habit_around_midnight_keeps_its_time() {
        let late = |day: u32, h: u32, m: u32| Event { at: at(day, h, m), tool: "control_music".into(), args: json!({"action": "play_playlist", "playlist": "Night"}), label: "play Night".into() };
        let habits = habits_now(&[late(2, 23, 50), late(4, 0, 10), late(5, 23, 55)], at(6, 0, 5));
        assert_eq!(habits.len(), 1, "{habits:?}");
        assert!(["23:58", "23:59", "00:00"].contains(&clock(habits[0].usual_minute).as_str()), "{}", clock(habits[0].usual_minute));
    }

    #[test]
    fn only_routine_actions_are_noted_and_an_offer_is_made_once_a_day() {
        let r = Routines::in_memory();
        for day in 2..=4 {
            r.note("open_jai_view", &json!({"view": "schedule"}), at(day, 7, 10));
            // Not what a routine is made of: stopping music, a search, a shutdown.
            r.note("control_music", &json!({"action": "pause"}), at(day, 7, 10));
            r.note("web_search", &json!({"query": "weather"}), at(day, 7, 10));
            r.note("shutdown_jai", &json!({}), at(day, 7, 10));
        }
        assert_eq!(r.events().len(), 3);
        assert_eq!(r.events()[0].label, "open the schedule view");
        let offer = r.offer(at(6, 7, 0)).expect("an offer");
        assert_eq!(offer.tool, "open_jai_view");
        assert_eq!(r.offer(at(6, 7, 20)), None, "once a day");
        assert!(r.offer(at(7, 7, 5)).is_some(), "again the next day");
        let said = r.tool(&json!({"action": "now"}), at(8, 7, 5)).unwrap();
        assert!(said.contains("07:10  open the schedule view (3 days)") && said.ends_with("Spoken: Around now you usually open the schedule view, Sir. Shall I?"), "{said}");
        assert!(r.tool(&json!({"action": "list"}), at(8, 15, 0)).unwrap().contains("07:10  open the schedule view (3 days)"));
        assert!(r.tool(&json!({"action": "now"}), at(8, 15, 0)).unwrap().contains("Nothing you usually do"));
        assert!(r.tool(&json!({"action": "forget"}), at(8, 15, 0)).unwrap().contains("Forgot 3"));
        assert!(r.events().is_empty());
        assert_eq!((label_for("open_application", &json!({"app_name": "Spotify"})), label_for("control_music", &json!({"action": "play_playlist", "playlist": "JaysRock"}))), ("open Spotify".to_string(), "play JaysRock".to_string()));
    }
}
