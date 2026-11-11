//! Calendar-aware workout history and estimated strength records. All sets and the active
//! rest deadline live in SQLite, shared by the desktop and authenticated mobile satellite.

use crate::calendar_client::CalendarStore;
use chrono::{Duration as ChronoDuration, Local, NaiveTime, Utc};
use rusqlite::{params, Connection};
use serde::Serialize;
use serde_json::{json, Value};
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

#[derive(Debug, Clone, Serialize)]
pub struct WorkoutLog {
    pub id: i64,
    pub exercise: String,
    pub sets: u32,
    pub reps: u32,
    pub weight_kg: f64,
    pub estimated_1rm: f64,
    pub logged_at: String,
    pub calendar_uid: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PersonalRecord {
    pub exercise: String,
    pub estimated_1rm: f64,
    pub weight_kg: f64,
    pub reps: u32,
    pub date: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct StrengthPoint {
    pub exercise: String,
    pub date: String,
    pub estimated_1rm: f64,
    pub volume_kg: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct ScheduledWorkout {
    pub uid: String,
    pub title: String,
    pub date: String,
    pub all_day: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct RestTimer {
    pub deadline_unix_ms: i64,
    pub remaining_seconds: u64,
    pub finished: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct LogOutcome {
    pub entry: WorkoutLog,
    pub new_personal_record: bool,
    pub previous_1rm: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct WorkoutSnapshot {
    pub scheduled: Vec<ScheduledWorkout>,
    pub recent: Vec<WorkoutLog>,
    pub records: Vec<PersonalRecord>,
    pub progression: Vec<StrengthPoint>,
    pub rest: Option<RestTimer>,
}

#[derive(Clone)]
pub struct WorkoutCoach {
    conn: Arc<Mutex<Connection>>,
}

impl WorkoutCoach {
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self, String> {
        Self::with(Connection::open(path).map_err(|e| e.to_string())?)
    }

    pub fn in_memory() -> Self {
        Self::with(Connection::open_in_memory().expect("in-memory SQLite")).expect("workout schema")
    }

    fn with(conn: Connection) -> Result<Self, String> {
        conn.busy_timeout(Duration::from_secs(3)).map_err(|e| e.to_string())?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS workout_logs (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                exercise TEXT NOT NULL,
                exercise_key TEXT NOT NULL,
                sets INTEGER NOT NULL CHECK(sets BETWEEN 1 AND 100),
                reps INTEGER NOT NULL CHECK(reps BETWEEN 1 AND 1000),
                weight_kg REAL NOT NULL CHECK(weight_kg >= 0 AND weight_kg <= 2000),
                estimated_1rm REAL NOT NULL,
                logged_at TEXT NOT NULL,
                calendar_uid TEXT
             );
             CREATE INDEX IF NOT EXISTS workout_logs_exercise ON workout_logs(exercise_key, logged_at);
             CREATE TABLE IF NOT EXISTS workout_rest_timer (
                id INTEGER PRIMARY KEY CHECK(id=1),
                deadline_unix_ms INTEGER NOT NULL,
                notified INTEGER NOT NULL DEFAULT 0
             );",
        ).map_err(|e| e.to_string())?;
        let has_notification = conn.prepare("PRAGMA table_info(workout_rest_timer)").map_err(|e| e.to_string())?
            .query_map([], |r| r.get::<_, String>(1)).map_err(|e| e.to_string())?
            .filter_map(Result::ok).any(|name| name == "notified");
        if !has_notification {
            conn.execute("ALTER TABLE workout_rest_timer ADD COLUMN notified INTEGER NOT NULL DEFAULT 0", []).map_err(|e| e.to_string())?;
        }
        Ok(Self { conn: Arc::new(Mutex::new(conn)) })
    }

    fn conn(&self) -> MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn log_set(&self, exercise: &str, sets: u32, reps: u32, weight_kg: f64, calendar_uid: Option<&str>) -> Result<LogOutcome, String> {
        let exercise = exercise.split_whitespace().collect::<Vec<_>>().join(" ");
        if exercise.is_empty() || exercise.len() > 160 {
            return Err("Give an exercise name between 1 and 160 bytes.".into());
        }
        if !(1..=100).contains(&sets) || !(1..=1000).contains(&reps) || !weight_kg.is_finite() || !(0.0..=2000.0).contains(&weight_kg) {
            return Err("Use 1–100 sets, 1–1000 repetitions and a finite weight between 0 and 2000 kg.".into());
        }
        if calendar_uid.is_some_and(|uid| uid.len() > 1024) {
            return Err("The calendar event identifier is too long.".into());
        }
        let estimated_1rm = estimated_one_rep_max(weight_kg, reps);
        let logged_at = Local::now().to_rfc3339();
        let key = exercise.to_lowercase();
        let mut conn = self.conn();
        let tx = conn.transaction().map_err(|e| e.to_string())?;
        let previous_1rm: f64 = tx.query_row("SELECT COALESCE(MAX(estimated_1rm), 0) FROM workout_logs WHERE exercise_key = ?1", params![key], |r| r.get(0)).map_err(|e| e.to_string())?;
        tx.execute("INSERT INTO workout_logs(exercise, exercise_key, sets, reps, weight_kg, estimated_1rm, logged_at, calendar_uid) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",
            params![exercise, key, sets, reps, weight_kg, estimated_1rm, logged_at, calendar_uid]).map_err(|e| e.to_string())?;
        let id = tx.last_insert_rowid();
        tx.commit().map_err(|e| e.to_string())?;
        Ok(LogOutcome {
            entry: WorkoutLog { id, exercise, sets, reps, weight_kg, estimated_1rm, logged_at, calendar_uid: calendar_uid.map(str::to_owned) },
            new_personal_record: estimated_1rm > previous_1rm + 0.001,
            previous_1rm,
        })
    }

    pub fn start_rest(&self, seconds: u64) -> Result<RestTimer, String> {
        if !(1..=86_400).contains(&seconds) {
            return Err("Rest timers must last between 1 second and 24 hours.".into());
        }
        let deadline = Utc::now().timestamp_millis() + seconds as i64 * 1000;
        self.conn().execute("INSERT INTO workout_rest_timer(id, deadline_unix_ms, notified) VALUES(1,?1,0) ON CONFLICT(id) DO UPDATE SET deadline_unix_ms = excluded.deadline_unix_ms, notified=0", params![deadline]).map_err(|e| e.to_string())?;
        Ok(rest_at(deadline, Utc::now().timestamp_millis()))
    }

    pub fn cancel_rest(&self) -> Result<(), String> {
        self.conn().execute("DELETE FROM workout_rest_timer WHERE id = 1", []).map(|_| ()).map_err(|e| e.to_string())
    }

    /// Atomically claim a completed timer once, including across restarts and multiple viewers.
    pub fn take_rest_completion(&self) -> Result<bool, String> {
        self.conn().execute("UPDATE workout_rest_timer SET notified=1 WHERE id=1 AND notified=0 AND deadline_unix_ms <= ?1", params![Utc::now().timestamp_millis()])
            .map(|updated| updated != 0).map_err(|e| e.to_string())
    }

    pub fn snapshot(&self, calendar: &CalendarStore) -> Result<WorkoutSnapshot, String> {
        let today = Local::now().date_naive();
        let scheduled = calendar.events_between(today.and_time(NaiveTime::MIN), (today + ChronoDuration::days(15)).and_time(NaiveTime::MIN))
            .into_iter().filter(|event| is_workout_title(&event.title))
            .map(|event| ScheduledWorkout { uid: event.uid, title: event.title, date: event.start.date().to_string(), all_day: event.all_day }).collect();
        let conn = self.conn();
        let mut recent_stmt = conn.prepare("SELECT id, exercise, sets, reps, weight_kg, estimated_1rm, logged_at, calendar_uid FROM workout_logs ORDER BY id DESC LIMIT 100").map_err(|e| e.to_string())?;
        let recent = recent_stmt.query_map([], |r| Ok(WorkoutLog { id: r.get(0)?, exercise: r.get(1)?, sets: r.get(2)?, reps: r.get(3)?, weight_kg: r.get(4)?, estimated_1rm: r.get(5)?, logged_at: r.get(6)?, calendar_uid: r.get(7)? }))
            .map_err(|e| e.to_string())?.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())?;
        let mut records_stmt = conn.prepare("SELECT exercise, estimated_1rm, weight_kg, reps, logged_at FROM (SELECT *, ROW_NUMBER() OVER(PARTITION BY exercise_key ORDER BY estimated_1rm DESC, id DESC) AS rank FROM workout_logs) WHERE rank = 1 ORDER BY exercise_key").map_err(|e| e.to_string())?;
        let records = records_stmt.query_map([], |r| Ok(PersonalRecord { exercise: r.get(0)?, estimated_1rm: r.get(1)?, weight_kg: r.get(2)?, reps: r.get(3)?, date: r.get(4)? }))
            .map_err(|e| e.to_string())?.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())?;
        let mut progression_stmt = conn.prepare("SELECT exercise, substr(logged_at,1,10), MAX(estimated_1rm), SUM(sets * reps * weight_kg) FROM workout_logs WHERE logged_at >= ?1 GROUP BY exercise_key, substr(logged_at,1,10) ORDER BY substr(logged_at,1,10), exercise_key LIMIT 5000").map_err(|e| e.to_string())?;
        let progression = progression_stmt.query_map(params![(today - ChronoDuration::days(730)).to_string()], |r| Ok(StrengthPoint { exercise: r.get(0)?, date: r.get(1)?, estimated_1rm: r.get(2)?, volume_kg: r.get(3)? }))
            .map_err(|e| e.to_string())?.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())?;
        let rest = match conn.query_row("SELECT deadline_unix_ms FROM workout_rest_timer WHERE id=1", [], |r| r.get::<_, i64>(0)) {
            Ok(deadline) => Some(rest_at(deadline, Utc::now().timestamp_millis())),
            Err(rusqlite::Error::QueryReturnedNoRows) => None,
            Err(e) => return Err(e.to_string()),
        };
        Ok(WorkoutSnapshot { scheduled, recent, records, progression, rest })
    }
}

fn rest_at(deadline_unix_ms: i64, now: i64) -> RestTimer {
    let remaining_ms = deadline_unix_ms.saturating_sub(now).max(0) as u64;
    RestTimer { deadline_unix_ms, remaining_seconds: remaining_ms.div_ceil(1000), finished: remaining_ms == 0 }
}

/// Epley estimate, with the measured single used directly for a one-repetition set.
fn estimated_one_rep_max(weight_kg: f64, reps: u32) -> f64 {
    if reps <= 1 { weight_kg } else { weight_kg * (1.0 + reps as f64 / 30.0) }
}

pub fn is_workout_title(title: &str) -> bool {
    title.to_lowercase().split(|c: char| !c.is_alphanumeric()).any(|word| matches!(word,
        "gym" | "workout" | "training" | "fitness" | "krafttraining" | "arme" | "arms" | "legs" | "beine" | "brust" | "chest" | "rücken" | "ruecken" | "biceps" | "bizeps" | "triceps" | "trizeps" | "push" | "pull" | "legday" | "oberkörper" | "unterkörper"))
}

pub fn tool_definition() -> Value {
    json!({"type":"function","function":{
        "name":"manage_workout",
        "description":"Calendar-synced gym log, estimated one-repetition personal records, strength progression and a rest timer shared by the PC and mobile satellite. Calendar entries such as Arme are detected automatically.",
        "parameters":{"type":"object","properties":{
            "action":{"type":"string","enum":["status","log","rest","cancel_rest"]},
            "exercise":{"type":"string"},"sets":{"type":"integer","minimum":1,"maximum":100},
            "reps":{"type":"integer","minimum":1,"maximum":1000},"weight_kg":{"type":"number","minimum":0,"maximum":2000},
            "seconds":{"type":"integer","minimum":1,"maximum":86400},"calendar_uid":{"type":"string"}
        },"required":["action"]}
    }})
}

pub fn run_tool(ctx: &crate::tools::ToolContext, args: &Value) -> Result<String, String> {
    use crate::tools::{int_arg, str_arg};
    match str_arg(args, &["action"]).unwrap_or("status") {
        "log" => {
            let exercise = str_arg(args, &["exercise"]).ok_or("Name the exercise to log.")?;
            let sets = u32::try_from(int_arg(args, "sets").unwrap_or(1)).map_err(|_| "Invalid set count.")?;
            let reps = u32::try_from(int_arg(args, "reps").ok_or("Give the number of repetitions.")?).map_err(|_| "Invalid repetition count.")?;
            let weight = args.get("weight_kg").and_then(|v| v.as_f64().or_else(|| v.as_str().and_then(|s| s.parse().ok()))).ok_or("Give weight_kg (0 for an unweighted exercise).")?;
            let scheduled = ctx.workout.snapshot(ctx.pim.calendar())?;
            let today = Local::now().date_naive().to_string();
            let calendar_uid = str_arg(args, &["calendar_uid"]).or_else(|| scheduled.scheduled.iter().find(|w| w.date == today).map(|w| w.uid.as_str()));
            let outcome = ctx.workout.log_set(exercise, sets, reps, weight, calendar_uid)?;
            Ok(format!("Logged {sets} × {reps} {exercise} at {weight:.1} kg. Estimated 1RM: {:.1} kg.{}", outcome.entry.estimated_1rm,
                if outcome.new_personal_record { format!(" New estimated personal record (previous {:.1} kg).", outcome.previous_1rm) } else { String::new() }))
        }
        "rest" => {
            let seconds = u64::try_from(int_arg(args, "seconds").unwrap_or(90)).map_err(|_| "Rest duration must be positive.")?;
            let rest = ctx.workout.start_rest(seconds)?;
            Ok(format!("Rest timer started for {} seconds on the desktop and phone.", rest.remaining_seconds))
        }
        "cancel_rest" => { ctx.workout.cancel_rest()?; Ok("Rest timer cleared.".into()) }
        "status" => serde_json::to_string_pretty(&ctx.workout.snapshot(ctx.pim.calendar())?).map_err(|e| e.to_string()),
        _ => Err("Workout actions are status, log, rest and cancel_rest.".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_and_volume_are_computed_from_persisted_sets() {
        let coach = WorkoutCoach::in_memory();
        let first = coach.log_set("Bench Press", 3, 10, 60.0, None).unwrap();
        assert!(first.new_personal_record);
        assert!((first.entry.estimated_1rm - 80.0).abs() < 0.001);
        assert!(!coach.log_set("bench press", 1, 1, 70.0, None).unwrap().new_personal_record);
        let snapshot = coach.snapshot(&CalendarStore::in_memory()).unwrap();
        assert_eq!(snapshot.records.len(), 1);
        assert_eq!(snapshot.recent.len(), 2);
        assert!((snapshot.progression[0].volume_kg - 1870.0).abs() < 0.001);
        assert!(coach.log_set("Squat", 0, 5, 80.0, None).is_err());
        assert!(coach.log_set("Squat", 1, 5, f64::NAN, None).is_err());
    }

    #[test]
    fn rest_deadlines_are_shared_and_survive_reopen() {
        let path = std::env::temp_dir().join(format!("jai-workout-{}.db", uuid::Uuid::new_v4()));
        let coach = WorkoutCoach::open(&path).unwrap();
        let timer = coach.start_rest(60).unwrap();
        drop(coach);
        let reopened = WorkoutCoach::open(&path).unwrap();
        assert_eq!(reopened.snapshot(&CalendarStore::in_memory()).unwrap().rest.unwrap().deadline_unix_ms, timer.deadline_unix_ms);
        reopened.cancel_rest().unwrap();
        assert!(reopened.snapshot(&CalendarStore::in_memory()).unwrap().rest.is_none());
        drop(reopened);
        std::fs::remove_file(path).unwrap();
        assert_eq!(rest_at(1001, 1000).remaining_seconds, 1);
        assert!(rest_at(1000, 1001).finished);
    }

    #[test]
    fn workout_calendar_detection_uses_words() {
        assert!(is_workout_title("Arme"));
        assert!(is_workout_title("Gym: push day"));
        assert!(!is_workout_title("Pharmacy appointment"));
        assert!(!is_workout_title("Warmest greetings"));
    }
}
