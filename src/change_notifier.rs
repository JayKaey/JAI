//! The background change notifier: lightweight `tokio` timers that refresh WebUntis, itslearning
//! or a registered web feed every few minutes, compare the result with the last snapshot in
//! `watch_state` (`jarvis_memory.db`), light up the service's node in the HUD's core on every
//! check, and tell Sir proactively the moment a class is cancelled or moved, or something new is
//! posted. The first check only records the baseline. Watchers are saved in the PIM settings and
//! restart with JAI.

use crate::pim::{self, Pim};
use crate::pim_config::{self, WatcherSpec, LMS, WEBUNTIS};
use crate::tools::str_arg;
use crate::webuntis_client::LessonStatus;
use chrono::{Duration as ChronoDuration, Local};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;
use tracing::{info, warn};

struct Running {
    minutes: u32,
    task: tokio::task::JoinHandle<()>,
}

struct Inner {
    conn: Mutex<Connection>,
    running: Mutex<HashMap<String, Running>>,
}

#[derive(Clone)]
pub struct Watchers {
    inner: Arc<Inner>,
}

impl Watchers {
    pub fn open<P: AsRef<Path>>(db_path: P) -> Self {
        match Connection::open(db_path.as_ref()) {
            Ok(conn) => Self::with(conn),
            Err(e) => {
                warn!("[WATCH] Snapshots cannot be persisted ({e}).");
                Self::in_memory()
            }
        }
    }

    pub fn in_memory() -> Self {
        Self::with(Connection::open_in_memory().expect("in-memory SQLite"))
    }

    fn with(conn: Connection) -> Self {
        let _ = conn.busy_timeout(Duration::from_secs(3));
        if let Err(e) = conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS watch_state (
                 service TEXT PRIMARY KEY,
                 snapshot TEXT NOT NULL,
                 updated_at TEXT NOT NULL
             );",
        ) {
            warn!("[WATCH] Could not create watch_state: {e}");
        }
        Self { inner: Arc::new(Inner { conn: Mutex::new(conn), running: Mutex::new(HashMap::new()) }) }
    }

    fn conn(&self) -> MutexGuard<'_, Connection> {
        self.inner.conn.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn running(&self) -> MutexGuard<'_, HashMap<String, Running>> {
        self.inner.running.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn snapshot(&self, service: &str) -> Option<Vec<String>> {
        self.conn()
            .query_row("SELECT snapshot FROM watch_state WHERE service = ?1", params![service], |r| r.get::<_, String>(0))
            .optional()
            .ok()
            .flatten()
            .and_then(|s| serde_json::from_str(&s).ok())
    }

    fn store(&self, service: &str, lines: &[String]) {
        let json = serde_json::to_string(lines).unwrap_or_else(|_| "[]".into());
        if let Err(e) = self.conn().execute(
            "INSERT INTO watch_state (service, snapshot, updated_at) VALUES (?1, ?2, ?3)
             ON CONFLICT(service) DO UPDATE SET snapshot = excluded.snapshot, updated_at = excluded.updated_at",
            params![service, json, Local::now().to_rfc3339()],
        ) {
            warn!("[WATCH] Could not store the {service} snapshot: {e}");
        }
    }

    /// Starts the watchers saved in the settings.
    pub fn restore(&self, pim: &Pim) {
        for spec in pim.config().read(|s| s.watchers.clone()) {
            match ready(pim, &spec.service) {
                Ok(()) => self.start(pim, &spec.service, spec.interval_minutes),
                Err(e) => warn!("[WATCH] Not restarting the {} watcher: {e}", spec.service),
            }
        }
    }

    pub fn start(&self, pim: &Pim, service: &str, minutes: u32) {
        let minutes = minutes.clamp(2, 1440);
        self.stop(service);
        let (key, watcher, node) = (service.to_string(), self.clone(), node_for(service));
        let pim = pim.clone();
        let task = tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_secs(u64::from(minutes) * 60));
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                tick.tick().await;
                pim.pulse(&["briefing", node], false);
                match check(&pim, &watcher, &key).await {
                    Ok(changes) if !changes.is_empty() => {
                        pim.pulse(&[node, "briefing", "brain", "voice"], true);
                        let shown: Vec<String> = changes.iter().take(3).cloned().collect();
                        let more = if changes.len() > 3 { format!(" And {} more.", changes.len() - 3) } else { String::new() };
                        pim.alert(format!("Sir, {}{more}", shown.join(" ")));
                    }
                    Ok(_) => {}
                    Err(e) => warn!("[WATCH] {key}: {e}"),
                }
            }
        });
        info!("[WATCH] Watching {service} every {minutes} min");
        self.running().insert(service.to_string(), Running { minutes, task });
    }

    pub fn stop(&self, service: &str) -> bool {
        match self.running().remove(service) {
            Some(r) => {
                r.task.abort();
                info!("[WATCH] Stopped watching {service}");
                true
            }
            None => false,
        }
    }

    /// "webuntis every 15 min, feed:Mensa every 30 min" / "none".
    pub fn describe(&self) -> String {
        let running = self.running();
        if running.is_empty() {
            return "none running".into();
        }
        let mut list: Vec<String> = running.iter().map(|(k, r)| format!("{} every {} min", display_key(k), r.minutes)).collect();
        list.sort();
        list.join(", ")
    }

    /// (service, minutes) of the running watchers (for the HUD).
    pub fn list(&self) -> Vec<(String, u32)> {
        let mut v: Vec<(String, u32)> = self.running().iter().map(|(k, r)| (k.clone(), r.minutes)).collect();
        v.sort();
        v
    }
}

fn node_for(service: &str) -> &'static str {
    if service.starts_with("feed:") {
        "briefing"
    } else {
        pim::node_of(service)
    }
}

fn display_key(key: &str) -> String {
    match key.strip_prefix("feed:") {
        Some(name) => format!("web feed '{name}'"),
        None => pim_config::display(key).to_string(),
    }
}

/// Whether a watcher's service can be checked.
fn ready(pim: &Pim, key: &str) -> Result<(), String> {
    match key.strip_prefix("feed:") {
        Some(name) => pim.config().read(|s| s.feed(name).map(|_| ())).ok_or_else(|| format!("there is no web feed called '{name}'")),
        None => match pim.config().missing(key) {
            Some(why) => Err(why),
            None => Ok(()),
        },
    }
}

/// One refresh: the new lines' messages (empty on the first, baseline run).
async fn check(pim: &Pim, watchers: &Watchers, key: &str) -> Result<Vec<String>, String> {
    let (lines, messages): (Vec<String>, HashMap<String, String>) = match key {
        WEBUNTIS => {
            let today = Local::now().date_naive();
            let lessons = pim.lessons(today, today + ChronoDuration::days(6)).await?;
            let changed: Vec<_> = lessons.into_iter().filter(|l| l.status != LessonStatus::Regular).collect();
            let messages = changed
                .iter()
                .map(|l| {
                    let when = format!("{} at {}", pim::spoken_day(l.date, today), l.start.format("%H:%M"));
                    let what = match l.status {
                        LessonStatus::Cancelled => format!("{} {when} has been cancelled.", l.subject_name()),
                        _ => format!("{} {when} has a substitution or room change{}.", l.subject_name(), if l.rooms.is_empty() { String::new() } else { format!(": room {}", l.rooms.join(", ")) }),
                    };
                    (l.key(), what)
                })
                .collect();
            (changed.iter().map(|l| l.key()).collect(), messages)
        }
        LMS => {
            let updates = pim.course_updates(None).await?;
            let messages = updates
                .iter()
                .map(|u| (u.id.clone(), format!("new on itslearning{}: {}.", if u.course.is_empty() { String::new() } else { format!(" in {}", u.course) }, u.title.trim_end_matches('.'))))
                .collect();
            (updates.iter().map(|u| u.id.clone()).collect(), messages)
        }
        feed_key => {
            let name = feed_key.strip_prefix("feed:").unwrap_or(feed_key);
            let feed = pim.config().read(|s| s.feed(name).cloned()).ok_or_else(|| format!("the web feed '{name}' is no longer registered"))?;
            let lines = pim.web().snapshot(&feed).await?;
            let messages = lines.iter().map(|l| (l.clone(), format!("{} changed: {}.", feed.name, l.chars().take(160).collect::<String>().trim_end_matches('.')))).collect();
            (lines, messages)
        }
    };
    let previous = watchers.snapshot(key);
    watchers.store(key, &lines);
    let Some(previous) = previous else {
        info!("[WATCH] {key}: baseline of {} entries recorded", lines.len());
        return Ok(Vec::new());
    };
    let before: HashSet<&String> = previous.iter().collect();
    Ok(lines.iter().filter(|l| !before.contains(l)).filter_map(|l| messages.get(l).cloned()).collect())
}

/// `manage_schedule_watcher(service_name, interval_minutes)`.
pub fn tool(pim: &Pim, args: &Value) -> Result<String, String> {
    let raw = str_arg(args, &["service_name", "service", "name"]).unwrap_or("list");
    let watchers = pim.watchers();
    if matches!(raw.to_lowercase().as_str(), "list" | "status" | "all" | "show") {
        return Ok(format!("Watchers: {}.", watchers.describe()));
    }
    let key = match pim.config().read(|s| s.feed(raw).map(|f| f.name.clone())) {
        Some(name) => format!("feed:{name}"),
        None => match pim_config::parse_service(raw) {
            Some(WEBUNTIS) => WEBUNTIS.to_string(),
            Some(LMS) => LMS.to_string(),
            _ => return Err(format!("I can watch WebUntis, itslearning or a registered web feed; '{raw}' is none of them.")),
        },
    };
    let minutes = match args.get("interval_minutes").or_else(|| args.get("minutes")) {
        Some(Value::Number(n)) => n.as_u64().map(|m| m as u32),
        Some(Value::String(s)) => s.trim().parse().ok(),
        _ => None,
    };
    let stop = minutes == Some(0) || str_arg(args, &["action"]).is_some_and(|a| matches!(a.to_lowercase().as_str(), "stop" | "off" | "disable" | "remove"));
    if stop {
        let was = watchers.stop(&key);
        pim.config().update(|s| s.watchers.retain(|w| w.service != key))?;
        return Ok(if was { format!("Stopped watching {}.", display_key(&key)) } else { format!("{} was not being watched.", display_key(&key)) });
    }
    ready(pim, &key)?;
    let default = match key.as_str() {
        WEBUNTIS => 15,
        LMS => 10,
        _ => pim.config().read(|s| s.feed(key.trim_start_matches("feed:")).map(|f| f.interval_minutes)).filter(|m| *m > 0).unwrap_or(30),
    };
    let minutes = minutes.unwrap_or(default).clamp(2, 1440);
    watchers.start(pim, &key, minutes);
    pim.config().update(|s| {
        s.watchers.retain(|w| w.service != key);
        s.watchers.push(WatcherSpec { service: key.clone(), interval_minutes: minutes });
    })?;
    Ok(format!(
        "Watching {} every {minutes} minutes; I'll tell you the moment something changes{}.",
        display_key(&key),
        if key == WEBUNTIS { " (cancelled classes, substitutions, room changes)" } else { "" }
    ))
}
