//! Screen time and the break coach: which programs Sir was in today and for how long ("what
//! did I use today", "how long have I been at the screen"), and a quiet card after fifty
//! minutes without a break.
//!
//! Every fifteen seconds the foreground program is noted, unless keyboard and mouse have been
//! still for two minutes (then nobody is at the screen). Five minutes away count as a break and
//! start the stretch over. The coach never speaks: it puts up one card, at most every fifty
//! minutes, and never over a full-screen game or film. Totals are kept per day in SQLite next
//! to the memory database; nothing leaves the PC.

use crate::features::{bar, spoken, str_arg};
use rusqlite::{params, Connection};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Seconds between two samples.
pub const SAMPLE_SECS: u64 = 15;
/// A stretch this long without a break earns the coach's card.
pub const BREAK_AFTER_SECS: u64 = 50 * 60;
/// This long away from keyboard and mouse is a break.
pub const BREAK_IS_SECS: u64 = 5 * 60;
/// Idle this long: nobody is at the screen, the time is not counted.
pub const AWAY_AFTER_SECS: u64 = 120;

#[derive(Default)]
struct State {
    /// The day the totals belong to ("2026-11-06").
    day: String,
    /// Seconds per program today.
    apps: HashMap<String, u64>,
    /// The stretch at the screen since the last break.
    stretch_secs: u64,
    /// How long Sir has been away within this stretch.
    away_secs: u64,
    /// The stretch length at which the coach last spoke up.
    nudged_at: u64,
    /// Samples since the totals were last written.
    unsaved: u32,
}

#[derive(Clone)]
pub struct Tracker {
    state: Arc<Mutex<State>>,
    conn: Arc<Mutex<Connection>>,
    coach: Arc<std::sync::atomic::AtomicBool>,
}

/// A program's name as Sir knows it ("chrome.exe" → "Chrome", "Code.exe" → "VS Code").
pub fn app_name(process: &str) -> String {
    let stem = process.trim().trim_end_matches(".exe").trim_end_matches(".EXE");
    match stem.to_lowercase().as_str() {
        "" => "Desktop".to_string(),
        "code" => "VS Code".to_string(),
        "msedge" => "Edge".to_string(),
        "javaw" | "java" => "Minecraft / Java".to_string(),
        "winword" => "Word".to_string(),
        "powerpnt" => "PowerPoint".to_string(),
        "explorer" => "File Explorer".to_string(),
        "applicationframehost" => "Windows app".to_string(),
        "jai" => "J.A.I.".to_string(),
        "windowsterminal" => "Terminal".to_string(),
        lower => {
            let mut c = lower.chars();
            c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
        }
    }
}

/// "1 h 05 min", "12 min", "under a minute".
pub fn span(secs: u64) -> String {
    match (secs / 3600, (secs % 3600) / 60) {
        (0, 0) => "under a minute".to_string(),
        (0, m) => format!("{m} min"),
        (h, m) => format!("{h} h {m:02} min"),
    }
}

/// The same for speech: "an hour and five minutes", "twelve minutes".
fn spoken_span(secs: u64) -> String {
    let (h, m) = (secs / 3600, (secs % 3600) / 60);
    let hours = match h {
        0 => String::new(),
        1 => "an hour".to_string(),
        n => format!("{n} hours"),
    };
    let minutes = match m {
        0 => String::new(),
        1 => "one minute".to_string(),
        n => format!("{n} minutes"),
    };
    match (hours.is_empty(), minutes.is_empty()) {
        (true, true) => "less than a minute".to_string(),
        (false, true) => hours,
        (true, false) => minutes,
        (false, false) => format!("{hours} and {minutes}"),
    }
}

impl Tracker {
    pub fn open(path: &Path) -> Result<Self, String> {
        let conn = Connection::open(path).map_err(|e| e.to_string())?;
        conn.busy_timeout(Duration::from_secs(3)).map_err(|e| e.to_string())?;
        Self::init(conn)
    }

    pub fn in_memory() -> Self {
        Self::init(Connection::open_in_memory().expect("in-memory SQLite")).expect("screen-time schema")
    }

    fn init(conn: Connection) -> Result<Self, String> {
        conn.execute_batch("CREATE TABLE IF NOT EXISTS screen_time (day TEXT NOT NULL, app TEXT NOT NULL, seconds INTEGER NOT NULL, PRIMARY KEY (day, app));").map_err(|e| e.to_string())?;
        Ok(Self { state: Arc::new(Mutex::new(State::default())), conn: Arc::new(Mutex::new(conn)), coach: Arc::new(std::sync::atomic::AtomicBool::new(true)) })
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn conn(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn set_coach(&self, on: bool) {
        self.coach.store(on, std::sync::atomic::Ordering::Release);
    }

    pub fn coach(&self) -> bool {
        self.coach.load(std::sync::atomic::Ordering::Acquire)
    }

    /// Today's totals as they are in the database (a restart continues the day).
    fn load(&self, day: &str) -> HashMap<String, u64> {
        let conn = self.conn();
        let Ok(mut stmt) = conn.prepare("SELECT app, seconds FROM screen_time WHERE day = ?1") else { return HashMap::new() };
        stmt.query_map(params![day], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?.max(0) as u64))).map(|rows| rows.flatten().collect()).unwrap_or_default()
    }

    fn save(&self, day: &str, apps: &HashMap<String, u64>) {
        let conn = self.conn();
        for (app, secs) in apps {
            let _ = conn.execute("INSERT INTO screen_time (day, app, seconds) VALUES (?1, ?2, ?3) ON CONFLICT(day, app) DO UPDATE SET seconds = excluded.seconds", params![day, app, *secs as i64]);
        }
    }

    /// One sample: `process` is in front, keyboard and mouse have been still for `idle_secs`,
    /// `fullscreen` says a game or a film covers the screen. The length of the stretch in
    /// seconds when it is time for the coach's card.
    pub fn sample(&self, day: &str, process: &str, idle_secs: u64, fullscreen: bool) -> Option<u64> {
        let mut s = self.state();
        if s.day != day {
            // A new day (or the first sample): yesterday is written, today starts from what
            // the database already has.
            if !s.day.is_empty() {
                let (old_day, old) = (s.day.clone(), std::mem::take(&mut s.apps));
                drop(s);
                self.save(&old_day, &old);
                s = self.state();
            }
            drop(s);
            let known = self.load(day);
            s = self.state();
            s.day = day.to_string();
            s.apps = known;
            s.unsaved = 0;
        }
        if idle_secs >= AWAY_AFTER_SECS {
            // Away: the time is nobody's. Long enough away is a break.
            s.away_secs += SAMPLE_SECS;
            if s.away_secs.max(idle_secs) >= BREAK_IS_SECS {
                s.stretch_secs = 0;
                s.nudged_at = 0;
            }
            return None;
        }
        s.away_secs = 0;
        *s.apps.entry(app_name(process)).or_default() += SAMPLE_SECS;
        s.stretch_secs += SAMPLE_SECS;
        s.unsaved += 1;
        if s.unsaved >= 4 {
            s.unsaved = 0;
            let (day, apps) = (s.day.clone(), s.apps.clone());
            drop(s);
            self.save(&day, &apps);
            s = self.state();
        }
        let due = self.coach() && !fullscreen && s.stretch_secs >= BREAK_AFTER_SECS && s.stretch_secs - s.nudged_at >= BREAK_AFTER_SECS;
        if due {
            s.nudged_at = s.stretch_secs;
            return Some(s.stretch_secs);
        }
        None
    }

    /// Today's programs, longest first, and the current stretch.
    pub fn today(&self) -> (Vec<(String, u64)>, u64) {
        let s = self.state();
        let mut apps: Vec<(String, u64)> = s.apps.iter().map(|(a, secs)| (a.clone(), *secs)).collect();
        apps.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        (apps, s.stretch_secs)
    }

    /// The last seven days' totals as (day, seconds), oldest first.
    pub fn week(&self) -> Vec<(String, u64)> {
        let (day, apps) = {
            let s = self.state();
            (s.day.clone(), s.apps.clone())
        };
        if !day.is_empty() {
            self.save(&day, &apps);
        }
        let conn = self.conn();
        let Ok(mut stmt) = conn.prepare("SELECT day, SUM(seconds) FROM screen_time GROUP BY day ORDER BY day DESC LIMIT 7") else { return Vec::new() };
        let mut days: Vec<(String, u64)> = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?.max(0) as u64))).map(|rows| rows.flatten().collect()).unwrap_or_default();
        days.reverse();
        days
    }

    /// The `screen_time_report` tool.
    pub fn tool(&self, args: &Value) -> Result<String, String> {
        let action = str_arg(args, &["action"]).unwrap_or("today").to_lowercase();
        match action.as_str() {
            "coach_on" | "coach_off" => {
                let on = action == "coach_on";
                self.set_coach(on);
                Ok(spoken(format!("Break coach {}.", if on { "on" } else { "off" }), if on { "The break coach is on, Sir. A card after fifty minutes, never over a game." } else { "The break coach is off, Sir." }))
            }
            "week" => {
                let days = self.week();
                if days.is_empty() {
                    return Ok(spoken("No screen time recorded yet.", "I have no screen time on record yet, Sir."));
                }
                let most = days.iter().map(|d| d.1).max().unwrap_or(1).max(1);
                let lines: Vec<String> = days.iter().map(|(day, secs)| format!("{day}  {}  {}", bar(*secs as f64 / most as f64, 20), span(*secs))).collect();
                let total: u64 = days.iter().map(|d| d.1).sum();
                Ok(spoken(format!("Screen time, last {} days\n{}", days.len(), lines.join("\n")), format!("{} at the screen over the last {} days, Sir. That is {} a day.", spoken_span(total), days.len(), spoken_span(total / days.len() as u64))))
            }
            "break" | "stretch" | "session" => {
                let (_, stretch) = self.today();
                let left = BREAK_AFTER_SECS.saturating_sub(stretch);
                Ok(spoken(
                    format!("At the screen without a break: {}", span(stretch)),
                    if stretch < 60 { "You have only just sat down, Sir.".to_string() } else if left == 0 { format!("{} without a break, Sir. Five minutes away would do you good.", spoken_span(stretch)) } else { format!("{} without a break, Sir.", spoken_span(stretch)) },
                ))
            }
            _ => {
                let (apps, stretch) = self.today();
                let total: u64 = apps.iter().map(|a| a.1).sum();
                if total == 0 {
                    return Ok(spoken("No screen time recorded today yet.", "Nothing on record for today yet, Sir."));
                }
                let lines: Vec<String> = apps.iter().take(10).map(|(app, secs)| format!("{}  {}  {app}", bar(*secs as f64 / total as f64, 20), span(*secs))).collect();
                let top = &apps[0];
                Ok(spoken(
                    format!("Screen time today: {} (current stretch {})\n{}", span(total), span(stretch), lines.join("\n")),
                    format!("{} at the screen today, Sir, most of it in {}: {}.", spoken_span(total), top.0, spoken_span(top.1)),
                ))
            }
        }
    }
}

pub fn tool_definition() -> Value {
    json!({"type": "function", "function": {
        "name": "screen_time_report",
        "description": "Screen time and the break coach. action today (programs used today and for how long), week (the last seven days), break (how long Sir has been at the screen without a break), coach_on / coach_off (the quiet card after fifty minutes).",
        "parameters": {"type": "object", "properties": {
            "action": {"type": "string", "enum": ["today", "week", "break", "coach_on", "coach_off"]}
        }, "required": []}
    }})
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY: &str = "2026-11-06";

    fn sit(tracker: &Tracker, process: &str, secs: u64) -> Vec<u64> {
        (0..secs / SAMPLE_SECS).filter_map(|_| tracker.sample(DAY, process, 3, false)).collect()
    }

    #[test]
    fn programs_are_counted_only_while_sir_is_at_the_screen() {
        let t = Tracker::in_memory();
        sit(&t, "Code.exe", 600);
        sit(&t, "chrome.exe", 300);
        // Idle for two minutes and more: nobody's time.
        for _ in 0..8 {
            assert_eq!(t.sample(DAY, "chrome.exe", 150, false), None);
        }
        let (apps, stretch) = t.today();
        assert_eq!(apps, vec![("VS Code".to_string(), 600), ("Chrome".to_string(), 300)]);
        assert_eq!(stretch, 900, "two minutes away is not a break yet");
        // The totals survive a restart on the same day and start over on the next.
        let said = t.tool(&json!({})).unwrap();
        assert!(said.contains("Screen time today: 15 min") && said.contains("VS Code") && said.contains("most of it in VS Code: 10 minutes"), "{said}");
        assert_eq!(t.week(), vec![(DAY.to_string(), 900)]);
        t.sample("2026-11-07", "Code.exe", 3, false);
        assert_eq!(t.today().0, vec![("VS Code".to_string(), SAMPLE_SECS)]);
        assert_eq!(t.week(), vec![(DAY.to_string(), 900), ("2026-11-07".to_string(), SAMPLE_SECS)]);
        assert_eq!((app_name("javaw.exe"), app_name("Discord.exe"), app_name("")), ("Minecraft / Java".to_string(), "Discord".to_string(), "Desktop".to_string()));
    }

    #[test]
    fn the_coach_speaks_up_after_fifty_minutes_and_a_break_starts_the_stretch_over() {
        let t = Tracker::in_memory();
        assert_eq!(sit(&t, "Code.exe", BREAK_AFTER_SECS - SAMPLE_SECS), Vec::<u64>::new(), "not a second early");
        assert_eq!(t.sample(DAY, "Code.exe", 3, false), Some(BREAK_AFTER_SECS));
        // Not again until another fifty minutes have passed.
        assert_eq!(sit(&t, "Code.exe", BREAK_AFTER_SECS - SAMPLE_SECS), Vec::<u64>::new());
        assert_eq!(t.sample(DAY, "Code.exe", 3, false), Some(2 * BREAK_AFTER_SECS));
        // Five minutes away: the stretch starts over.
        for _ in 0..(BREAK_IS_SECS / SAMPLE_SECS) {
            t.sample(DAY, "Code.exe", AWAY_AFTER_SECS + 5, false);
        }
        assert_eq!(t.today().1, 0);
        // Never over a full-screen game, and never with the coach off.
        assert_eq!((0..BREAK_AFTER_SECS / SAMPLE_SECS + 4).filter_map(|_| t.sample(DAY, "javaw.exe", 3, true)).count(), 0);
        let t = Tracker::in_memory();
        t.tool(&json!({"action": "coach_off"})).unwrap();
        assert!(sit(&t, "Code.exe", BREAK_AFTER_SECS + 300).is_empty());
        let said = t.tool(&json!({"action": "break"})).unwrap();
        assert!(said.contains("55 min") && said.contains("Five minutes away"), "{said}");
        // A long idle reported by Windows is a break even on the first away sample.
        let t = Tracker::in_memory();
        sit(&t, "Code.exe", 1200);
        t.sample(DAY, "Code.exe", BREAK_IS_SECS + 10, false);
        assert_eq!(t.today().1, 0);
    }

    #[test]
    fn spans_read_naturally() {
        assert_eq!((span(40), span(720), span(3900)), ("under a minute".to_string(), "12 min".to_string(), "1 h 05 min".to_string()));
        assert_eq!((spoken_span(30), spoken_span(60), spoken_span(3600), spoken_span(7500)), ("less than a minute".to_string(), "one minute".to_string(), "an hour".to_string(), "2 hours and 5 minutes".to_string()));
    }
}
