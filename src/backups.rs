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

// ------------------------------------------------------------------- Minecraft worlds ----
// Once a week, and on "back up my worlds", each instance's `saves` (with its options, server
// list and mod config) goes into `backups/minecraft/<instance>/<day>/`; five copies stay per
// instance. Never while Minecraft runs: a world being written is not copied half-saved.

/// How many copies of an instance's worlds stay.
pub const KEPT_WORLD_COPIES: usize = 5;
const WORLDS_EVERY_DAYS: i64 = 7;
/// The Minecraft folder set in the settings (empty: the launchers' own places).
static MINECRAFT_DIR: Mutex<String> = Mutex::new(String::new());

fn worlds_folder(db: &Path) -> PathBuf {
    folder(db).join("minecraft")
}

/// The instance's name for its folder: its own folder, or for a `.minecraft` the one around it
/// (Prism's "MyPack/.minecraft" is "MyPack"; the vanilla one is "minecraft").
pub fn instance_name(root: &Path) -> String {
    let name = |p: &Path| p.file_name().map(|n| n.to_string_lossy().to_string());
    let own = name(root).unwrap_or_default();
    let base = if own == ".minecraft" { root.parent().and_then(name).filter(|n| !n.eq_ignore_ascii_case("Roaming")).unwrap_or(own) } else { own };
    base.trim_start_matches('.').chars().map(|c| if c.is_alphanumeric() || " -_".contains(c) { c } else { '_' }).collect()
}

fn copy_tree(from: &Path, to: &Path) -> std::io::Result<u64> {
    std::fs::create_dir_all(to)?;
    let mut bytes = 0;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let (src, dst) = (entry.path(), to.join(entry.file_name()));
        if entry.file_type()?.is_dir() {
            bytes += copy_tree(&src, &dst)?;
        } else if entry.file_name() != "session.lock" {
            bytes += std::fs::copy(&src, &dst)?;
        }
    }
    Ok(bytes)
}

/// The days an instance has a copy of, newest first.
pub fn world_days(instance: &Path) -> Vec<NaiveDate> {
    let mut days: Vec<NaiveDate> = std::fs::read_dir(instance).into_iter().flatten().flatten().filter(|e| e.path().is_dir()).filter_map(|e| NaiveDate::parse_from_str(e.file_name().to_str()?, "%Y-%m-%d").ok()).collect();
    days.sort_unstable_by(|a, b| b.cmp(a));
    days
}

/// The newest copy of any instance.
fn newest_worlds(into: &Path) -> Option<NaiveDate> {
    std::fs::read_dir(into).into_iter().flatten().flatten().filter_map(|e| world_days(&e.path()).first().copied()).max()
}

/// Copies the worlds of every instance that has any (each folder once) for `day`, under another
/// name first. (instances copied, bytes, what failed).
pub fn back_up_worlds(roots: &[PathBuf], into: &Path, day: NaiveDate) -> (usize, u64, Vec<String>) {
    let (mut done, mut bytes, mut failed) = (0, 0, Vec::new());
    let mut seen = std::collections::HashSet::new();
    for root in roots.iter().filter(|r| r.join("saves").is_dir()) {
        if !seen.insert(std::fs::canonicalize(root).unwrap_or_else(|_| root.clone())) {
            continue;
        }
        let instance = into.join(instance_name(root));
        let (target, part) = (instance.join(day.format("%Y-%m-%d").to_string()), instance.join(format!("{}.part", day.format("%Y-%m-%d"))));
        let _ = std::fs::remove_dir_all(&part);
        let copied = copy_tree(&root.join("saves"), &part.join("saves")).and_then(|n| {
            for file in ["options.txt", "servers.dat"].map(|f| root.join(f)).iter().filter(|f| f.is_file()) {
                std::fs::copy(file, part.join(file.file_name().unwrap_or_default()))?;
            }
            Ok(n + if root.join("config").is_dir() { copy_tree(&root.join("config"), &part.join("config"))? } else { 0 })
        });
        match copied.and_then(|n| {
            let _ = std::fs::remove_dir_all(&target);
            std::fs::rename(&part, &target).map(|_| n)
        }) {
            Ok(n) => {
                bytes += n;
                done += 1;
                for old in world_days(&instance).into_iter().skip(KEPT_WORLD_COPIES) {
                    let _ = std::fs::remove_dir_all(instance.join(old.format("%Y-%m-%d").to_string()));
                }
            }
            Err(e) => {
                let _ = std::fs::remove_dir_all(&part);
                failed.push(format!("{}: {e}", instance_name(root)));
            }
        }
    }
    (done, bytes, failed)
}

/// "Back up my worlds" (`forced`) or the weekly look: copies them unless Minecraft runs.
async fn worlds_now(forced: bool) -> Result<String, String> {
    let Some((db, _)) = home() else { return Err("A scripted session backs nothing up.".to_string()) };
    let into = worlds_folder(&db);
    let today = Local::now().date_naive();
    if !forced && newest_worlds(&into).is_some_and(|d| (today - d).num_days() < WORLDS_EVERY_DAYS) {
        return Ok(String::new());
    }
    if crate::apps::is_running("minecraft").await {
        return Err("Minecraft is running: worlds are copied once it is closed.".to_string());
    }
    let configured = MINECRAFT_DIR.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let roots = crate::minecraft_doctor::instance_roots_from(&configured);
    let (done, bytes, failed) = tokio::task::spawn_blocking(move || back_up_worlds(&roots, &into, today)).await.map_err(|e| e.to_string())?;
    let mut line = match done {
        0 if failed.is_empty() => "No Minecraft worlds were found to back up.".to_string(),
        0 => String::new(),
        n => format!("The worlds{} are backed up ({:.1} GB); the last {KEPT_WORLD_COPIES} copies stay.", if n == 1 { String::new() } else { format!(" of {n} instances") }, bytes as f64 / 1_073_741_824.0),
    };
    if !failed.is_empty() {
        line = format!("{line} Not copied: {}.", failed.join("; ")).trim().to_string();
    }
    Ok(line)
}

/// Once a day, a while after the start and then whenever the date has turned.
pub fn start(db: &str, config: &str, minecraft_dir: &str, stop: tokio_util::sync::CancellationToken) {
    if cfg!(test) || crate::platform::in_test_session() {
        return;
    }
    *HOME.lock().unwrap_or_else(|e| e.into_inner()) = Some((PathBuf::from(db), PathBuf::from(config)));
    *MINECRAFT_DIR.lock().unwrap_or_else(|e| e.into_inner()) = minecraft_dir.to_string();
    tokio::spawn(async move {
        tokio::select! { _ = stop.cancelled() => return, _ = tokio::time::sleep(FIRST_LOOK) => {} }
        loop {
            if let Some((db, config)) = home() {
                let today = Local::now().date_naive();
                if !days(&folder(&db)).contains(&today) {
                    match tokio::task::spawn_blocking(move || make(&db, &config, today)).await {
                        Ok(Ok(file)) => info!("[BACKUP] Today's copy of the memory and the settings is in {} (the last {KEPT_DAYS} days stay)", file.display()),
                        Ok(Err(why)) => {
                            warn!("[BACKUP] No backup today: {why}");
                            // (Once a day at most: the next look is twenty minutes away, the note is not repeated.)
                            static SAID: Mutex<Option<NaiveDate>> = Mutex::new(None);
                            if SAID.lock().unwrap_or_else(|e| e.into_inner()).replace(today) != Some(today) {
                                crate::hud_notify::record(&format!("Backup: today's copy of my memory could not be made ({why})."), true);
                            }
                        }
                        Err(e) => warn!("[BACKUP] The backup task ended early: {e}"),
                    }
                }
                match worlds_now(false).await {
                    Ok(line) if !line.is_empty() => info!("[BACKUP] {line}"),
                    Ok(_) => {}
                    Err(why) => info!("[BACKUP] Worlds not copied yet: {why}"),
                }
            }
            tokio::select! { _ = stop.cancelled() => return, _ = tokio::time::sleep(CHECK_EVERY) => {} }
        }
    });
}

/// How many days of backups stand and how large they are together, for the privacy board.
pub fn kept() -> (usize, f64) {
    let Some((db, _)) = home() else { return (0, 0.0) };
    let folder = folder(&db);
    let days = days(&folder);
    let bytes: u64 = days.iter().flat_map(|day| [db_name(*day), config_name(*day)]).filter_map(|name| std::fs::metadata(folder.join(name)).ok()).map(|m| m.len()).sum();
    (days.len(), bytes as f64 / 1_048_576.0)
}

/// Every backup leaves (the privacy board's key, on Sir's second click).
pub fn delete_all() -> usize {
    let Some((db, _)) = home() else { return 0 };
    let folder = folder(&db);
    let mut gone = 0;
    for day in days(&folder) {
        for name in [db_name(day), config_name(day)] {
            if std::fs::remove_file(folder.join(name)).is_ok() {
                gone += 1;
            }
        }
    }
    gone
}

/// "Back up your memory", "when was the last backup": the kit's phrases for it.
pub fn command(s: &str) -> Option<Value> {
    static NOW: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"^(?:back\s*up\s+(?:your\s+memory|yourself|your\s+data|everything\s+you\s+know)(?:\s+now)?|make\s+a\s+backup(?:\s+now)?|sichere?\s+(?:dein\s+ged(?:ä|ae)chtnis|deine\s+daten|dich)|mach\s+(?:ein|eine)\s+(?:backup|sicherung))$").expect("valid regex")
    });
    static STATUS: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"^(?:when\s+was\s+(?:the|your)\s+last\s+backup|(?:show|list)\s+(?:me\s+)?(?:the|your)\s+backups|do\s+you\s+have\s+(?:a\s+)?backups?|is\s+your\s+memory\s+backed\s+up|wann\s+war\s+(?:das|die|dein|deine)\s+letzte\s+(?:backup|sicherung)|zeig\s+(?:mir\s+)?(?:die|deine)\s+(?:backups|sicherungen))$").expect("valid regex")
    });
    static WORLDS: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"^(?:back\s*up|save|copy)\s+(?:all\s+)?(?:my|the)\s+(?:minecraft\s+)?(?:worlds|saves)(?:\s+now)?$|^(?:sichere?|speichere?)\s+(?:alle\s+)?(?:meine|die)\s+(?:minecraft[\s-]?)?welten$").expect("valid regex")
    });
    let s = s.trim();
    if WORLDS.is_match(s) {
        Some(json!({"action": "worlds"}))
    } else if NOW.is_match(s) {
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
    if args["action"].as_str() == Some("worlds") {
        let line = worlds_now(true).await?;
        return Ok(crate::kit::say(format!("{line} ({})", worlds_folder(&db).display()), format!("{line} Sir.").replace(". Sir.", ", Sir.")));
    }
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
    fn minecraft_worlds_are_copied_per_instance_and_five_copies_stay() {
        let dir = temp("worlds");
        let vanilla = dir.join("Roaming").join(".minecraft");
        let pack = dir.join("instances").join("My Pack!").join(".minecraft");
        let empty = dir.join("instances").join("Fresh");
        for (root, world) in [(&vanilla, "World One"), (&pack, "Skyblock")] {
            std::fs::create_dir_all(root.join("saves").join(world).join("region")).expect("world");
            std::fs::write(root.join("saves").join(world).join("level.dat"), b"level").expect("level");
            std::fs::write(root.join("saves").join(world).join("region").join("r.0.0.mca"), vec![7u8; 4096]).expect("region");
            std::fs::write(root.join("saves").join(world).join("session.lock"), b"lock").expect("lock");
        }
        std::fs::write(vanilla.join("options.txt"), b"fov:1.0").expect("options");
        std::fs::create_dir_all(pack.join("config")).expect("config");
        std::fs::write(pack.join("config").join("mod.toml"), b"x = 1").expect("mod config");
        std::fs::create_dir_all(&empty).expect("no saves");
        assert_eq!((instance_name(&vanilla), instance_name(&pack), instance_name(&empty)), ("minecraft".to_string(), "My Pack_".to_string(), "Fresh".to_string()));

        let into = dir.join("backups").join("minecraft");
        let roots = [vanilla.clone(), pack.clone(), empty.clone(), pack.clone()];
        let first = NaiveDate::from_ymd_opt(2026, 10, 1).expect("date");
        for week in 0..7 {
            let (done, bytes, failed) = back_up_worlds(&roots, &into, first + chrono::Duration::weeks(week));
            assert_eq!((done, failed), (2, Vec::<String>::new()), "each instance once, the one without worlds left out");
            assert!(bytes >= 2 * (4096 + 5), "{bytes}");
        }
        let kept = world_days(&into.join("minecraft"));
        assert_eq!(kept.len(), KEPT_WORLD_COPIES);
        assert_eq!(kept[0], first + chrono::Duration::weeks(6), "the newest stays");
        let newest = into.join("minecraft").join(kept[0].format("%Y-%m-%d").to_string());
        assert_eq!(std::fs::read(newest.join("saves").join("World One").join("region").join("r.0.0.mca")).expect("copied").len(), 4096);
        assert!(newest.join("options.txt").is_file() && !newest.join("saves").join("World One").join("session.lock").exists(), "options come along, the lock does not");
        assert!(into.join("My Pack_").join(kept[0].format("%Y-%m-%d").to_string()).join("config").join("mod.toml").is_file());
        assert_eq!(newest_worlds(&into), Some(kept[0]));
        assert!(!into.join("minecraft").read_dir().expect("dir").flatten().any(|e| e.file_name().to_string_lossy().ends_with(".part")), "no half copies left");
        assert_eq!(command("back up my minecraft worlds"), Some(json!({"action": "worlds"})));
        assert_eq!(command("sichere meine welten"), Some(json!({"action": "worlds"})));
        assert_eq!(command("back up your memory"), Some(json!({"action": "now"})));
        let _ = std::fs::remove_dir_all(&dir);
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
