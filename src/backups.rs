//! Nightly backups. The memory database holds everything he has learnt (memories, the voice
//! print, the words he learnt to hear, notes, rules, desk layouts); the settings file holds how
//! Sir has set him up. Once a day a consistent copy of both goes into `backups/` beside the
//! database, and the last seven days stay. A scripted session backs nothing up.
//!
//! The copy is SQLite's own (`VACUUM INTO`): whole and readable even while he is writing to the
//! database, checked before it takes its name. To go back to a day: close him, copy that day's
//! file over the database, start him again.

use chrono::{Local, NaiveDate};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;
use tracing::{info, warn};

/// How many days of backups stay.
pub const KEPT_DAYS: usize = 7;
const CHECK_EVERY: Duration = Duration::from_secs(20 * 60);
/// After the start the first look waits this long: the start is busy enough.
const FIRST_LOOK: Duration = Duration::from_secs(4 * 60);

/// (database, settings file) of this session. None: a scripted session or a test.
static HOME: Mutex<Option<(PathBuf, PathBuf)>> = Mutex::new(None);

fn home() -> Option<(PathBuf, PathBuf)> {
    HOME.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

/// Where the backups live: `backups/` beside the database.
pub fn folder(db: &Path) -> PathBuf {
    db.parent().filter(|p| !p.as_os_str().is_empty()).map(|p| p.join("backups")).unwrap_or_else(|| PathBuf::from("backups"))
}

fn db_name(day: NaiveDate) -> String {
    format!("jai_memory-{}.db", day.format("%Y-%m-%d"))
}

fn config_name(day: NaiveDate) -> String {
    format!("jai_config-{}.json", day.format("%Y-%m-%d"))
}

/// The days that have a backup of the database, newest first.
pub fn days(folder: &Path) -> Vec<NaiveDate> {
    let mut days: Vec<NaiveDate> = std::fs::read_dir(folder)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name();
            let day = name.to_str()?.strip_prefix("jai_memory-")?.strip_suffix(".db")?;
            NaiveDate::parse_from_str(day, "%Y-%m-%d").ok()
        })
        .collect();
    days.sort_unstable_by(|a, b| b.cmp(a));
    days
}

/// A consistent copy of the database and of the settings for `day`. The copy is written under
/// another name and checked first, so a half-written file never stands for a backup.
pub fn make(db: &Path, config: &Path, day: NaiveDate) -> Result<PathBuf, String> {
    let folder = folder(db);
    std::fs::create_dir_all(&folder).map_err(|e| format!("could not make {}: {e}", folder.display()))?;
    let target = folder.join(db_name(day));
    let part = folder.join(format!("{}.part", db_name(day)));
    let _ = std::fs::remove_file(&part);
    {
        let source = rusqlite::Connection::open(db).map_err(|e| format!("could not open the database: {e}"))?;
        source.busy_timeout(Duration::from_secs(15)).map_err(|e| e.to_string())?;
        source.execute("VACUUM INTO ?1", [part.to_string_lossy().as_ref()]).map_err(|e| format!("the copy failed: {e}"))?;
    }
    let sound = rusqlite::Connection::open(&part)
        .and_then(|copy| copy.query_row("PRAGMA quick_check", [], |row| row.get::<_, String>(0)))
        .map_err(|e| format!("the copy cannot be read: {e}"))?;
    if sound != "ok" {
        let _ = std::fs::remove_file(&part);
        return Err(format!("the copy is not sound: {sound}"));
    }
    let _ = std::fs::remove_file(&target);
    if let Err(e) = std::fs::rename(&part, &target) {
        // (The day's backup is open in another program: it stays as it is, nothing half-done beside it.)
        let _ = std::fs::remove_file(&part);
        return Err(format!("today's backup is in use and could not be replaced: {e}"));
    }
    if config.is_file() {
        if let Err(e) = std::fs::copy(config, folder.join(config_name(day))) {
            warn!("[BACKUP] The settings were not copied: {e}");
        }
    }
    prune(&folder);
    Ok(target)
}

/// His own copies older than the newest `KEPT_DAYS` leave (nothing else in the folder is touched).
fn prune(folder: &Path) -> usize {
    let mut gone = 0;
    for day in days(folder).into_iter().skip(KEPT_DAYS) {
        for name in [db_name(day), config_name(day)] {
            if std::fs::remove_file(folder.join(name)).is_ok() {
                gone += 1;
            }
        }
    }
    gone
}

/// Once a day, a while after the start and then whenever the date has turned.
pub fn start(db: &str, config: &str, stop: tokio_util::sync::CancellationToken) {
    if cfg!(test) || crate::platform::in_test_session() {
        return;
    }
    *HOME.lock().unwrap_or_else(|e| e.into_inner()) = Some((PathBuf::from(db), PathBuf::from(config)));
    tokio::spawn(async move {
        tokio::select! { _ = stop.cancelled() => return, _ = tokio::time::sleep(FIRST_LOOK) => {} }
        loop {
            if let Some((db, config)) = home() {
                let today = Local::now().date_naive();
                if !days(&folder(&db)).contains(&today) {
                    match tokio::task::spawn_blocking(move || make(&db, &config, today)).await {
                        Ok(Ok(file)) => info!("[BACKUP] Today's copy of the memory and the settings is in {} (the last {KEPT_DAYS} days stay)", file.display()),
                        Ok(Err(why)) => warn!("[BACKUP] No backup today: {why}"),
                        Err(e) => warn!("[BACKUP] The backup task ended early: {e}"),
                    }
                }
            }
            tokio::select! { _ = stop.cancelled() => return, _ = tokio::time::sleep(CHECK_EVERY) => {} }
        }
    });
}

/// "Back up your memory", "when was the last backup": the kit's phrases for it.
pub fn command(s: &str) -> Option<Value> {
    static NOW: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"^(?:back\s*up\s+(?:your\s+memory|yourself|your\s+data|everything\s+you\s+know)(?:\s+now)?|make\s+a\s+backup(?:\s+now)?|sichere?\s+(?:dein\s+ged(?:ä|ae)chtnis|deine\s+daten|dich)|mach\s+(?:ein|eine)\s+(?:backup|sicherung))$").expect("valid regex")
    });
    static STATUS: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"^(?:when\s+was\s+(?:the|your)\s+last\s+backup|(?:show|list)\s+(?:me\s+)?(?:the|your)\s+backups|do\s+you\s+have\s+(?:a\s+)?backups?|is\s+your\s+memory\s+backed\s+up|wann\s+war\s+(?:das|die|dein|deine)\s+letzte\s+(?:backup|sicherung)|zeig\s+(?:mir\s+)?(?:die|deine)\s+(?:backups|sicherungen))$").expect("valid regex")
    });
    let s = s.trim();
    if NOW.is_match(s) {
        Some(json!({"action": "now"}))
    } else if STATUS.is_match(s) {
        Some(json!({"action": "status"}))
    } else {
        None
    }
}

/// What the backups are, in his words: the newest one and how many days stand.
fn in_words(folder: &Path) -> (String, String) {
    let days = days(folder);
    let Some(newest) = days.first() else {
        return ("No backup has been made yet. The first follows a few minutes after a start, then one a day.".to_string(), "There is no backup yet, Sir. The first one follows shortly.".to_string());
    };
    let file = folder.join(db_name(*newest));
    let mb = std::fs::metadata(&file).map(|m| m.len() as f64 / 1_048_576.0).unwrap_or(0.0);
    let today = Local::now().date_naive();
    let when = match (today - *newest).num_days() {
        0 => "today".to_string(),
        1 => "yesterday".to_string(),
        n => format!("{n} days ago"),
    };
    let list = days.iter().map(|d| d.format("%Y-%m-%d").to_string()).collect::<Vec<_>>().join(", ");
    (
        format!("Backups in {}\nNewest: {} ({mb:.1} MB)\nDays kept: {list}\nTo go back to a day: close me, copy that day's file over my memory database, start me again.", folder.display(), newest.format("%Y-%m-%d")),
        format!("The last backup is from {when}, Sir. {} day{} are kept.", days.len(), if days.len() == 1 { "" } else { "s" }),
    )
}

pub async fn run(args: &Value) -> Result<String, String> {
    let Some((db, config)) = home() else {
        return Ok(crate::kit::say("A scripted session backs nothing up.", "This session backs nothing up, Sir."));
    };
    let folder = folder(&db);
    if args["action"].as_str() == Some("now") {
        let today = Local::now().date_naive();
        let file = tokio::task::spawn_blocking(move || make(&db, &config, today)).await.map_err(|e| e.to_string())?.map_err(|why| format!("The backup did not work: {why}"))?;
        info!("[BACKUP] Made on request: {}", file.display());
        let (display, _) = in_words(&folder);
        return Ok(crate::kit::say(display, "Backed up, Sir."));
    }
    let (display, spoken) = in_words(&folder);
    Ok(crate::kit::say(display, spoken))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("jai_backups_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        dir
    }

    #[test]
    fn a_backup_is_a_whole_readable_copy_and_only_seven_days_stay() {
        let dir = temp("copy");
        let db = dir.join("memory.db");
        let config = dir.join("settings.json");
        std::fs::write(&config, "{\"config_version\": 27}").expect("settings");
        {
            let conn = rusqlite::Connection::open(&db).expect("db");
            conn.execute_batch("PRAGMA journal_mode=WAL; CREATE TABLE kept(line TEXT); INSERT INTO kept VALUES ('the first'), ('the second');").expect("rows");
        }
        // Nine days, oldest first: the two oldest leave with the ninth.
        let first = NaiveDate::from_ymd_opt(2026, 11, 1).expect("date");
        let mut last = first;
        for n in 0..9 {
            last = first + chrono::Duration::days(n);
            make(&db, &config, last).expect("backup");
        }
        let kept = days(&folder(&db));
        assert_eq!(kept.len(), KEPT_DAYS);
        assert_eq!(kept.first(), Some(&last));
        assert_eq!(kept.last(), Some(&(first + chrono::Duration::days(2))));
        assert!(!folder(&db).join(config_name(first)).exists() && folder(&db).join(config_name(last)).is_file());
        // The copy holds what the database held.
        let copy = rusqlite::Connection::open(folder(&db).join(db_name(last))).expect("copy");
        let lines: i64 = copy.query_row("SELECT COUNT(*) FROM kept", [], |row| row.get(0)).expect("count");
        assert_eq!(lines, 2);
        // While another program has the day's backup open it is not replaced, and nothing
        // half-written stays beside it.
        if cfg!(windows) {
            assert!(make(&db, &config, last).is_err_and(|why| why.contains("in use")));
            assert!(folder(&db).join(db_name(last)).is_file());
        }
        drop(copy);
        // A second backup on the same day replaces the first.
        make(&db, &config, last).expect("again");
        assert!(std::fs::read_dir(folder(&db)).expect("folder").flatten().all(|e| !e.file_name().to_string_lossy().ends_with(".part")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn he_is_asked_for_a_backup_in_plain_words() {
        for said in ["back up your memory", "backup your memory now", "make a backup", "mach ein backup", "sichere dein gedächtnis"] {
            assert_eq!(command(said), Some(json!({"action": "now"})), "{said}");
        }
        for said in ["when was the last backup", "show me your backups", "wann war das letzte backup"] {
            assert_eq!(command(said), Some(json!({"action": "status"})), "{said}");
        }
        for said in ["back up", "go back", "make a backup of my documents folder", "back up my phone"] {
            assert_eq!(command(said), None, "{said}");
        }
    }
}
