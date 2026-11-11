//! "Tell me when …": JAI keeps an eye on something and speaks up once it happens.
//!
//! * download: the browser download running in the Downloads folder finishes (Chrome and Edge
//!   `.crdownload`, Firefox `.part`, Safari `.download`, Opera `.opdownload`, `.partial`).
//! * closes / starts: an application exits ("tell me when Blender is done": a render, an export,
//!   a game's update) or starts.
//! * online: a website answers again ("tell me when GitHub is back up").
//!
//! A watch speaks once and ends (eight at a time, twelve hours at most). While Sir is away its
//! line also reaches his phone (`away_mode` relays every alert). Watching only reads: the folder,
//! the process list, one request to the site every 30 seconds.

use crate::crash_guard::lock;
use crate::tools::{str_arg, ToolContext};
use serde_json::{json, Value};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime};
use sysinfo::{ProcessesToUpdate, System};
use tokio_util::sync::CancellationToken;
use tracing::info;

pub const TOOL: &str = "watch_and_tell";
const MAX_WATCHES: usize = 8;
const MAX_AGE: Duration = Duration::from_secs(12 * 3600);
const PARTIAL: &[&str] = &[".crdownload", ".part", ".partial", ".download", ".opdownload"];
const ONLINE_EVERY: Duration = Duration::from_secs(30);
/// A partial file untouched this long is not downloading (stalled, or a leftover).
const STALE: Duration = Duration::from_secs(10 * 60);

/// What people call an application → its process ("OBS" → obs64.exe).
const ALIASES: &[(&str, &str)] = &[
    ("obs", "obs64"),
    ("obsstudio", "obs64"),
    ("word", "winword"),
    ("powerpoint", "powerpnt"),
    ("minecraft", "javaw"),
    ("vscode", "code"),
    ("visualstudiocode", "code"),
    ("visualstudio", "devenv"),
    ("edge", "msedge"),
    ("premiere", "adobepremierepro"),
    ("premierepro", "adobepremierepro"),
    ("aftereffects", "afterfx"),
    ("davinci", "resolve"),
    ("davinciresolve", "resolve"),
    ("epic", "epicgameslauncher"),
    ("epicgames", "epicgameslauncher"),
];

#[derive(Debug, Clone, PartialEq)]
enum Kind {
    /// The partial files seen when the watch began.
    Download { dir: PathBuf, waiting: HashSet<PathBuf>, since: SystemTime },
    Closes { process: String },
    Starts { process: String },
    Online { url: String },
}

#[derive(Debug, Clone)]
struct Watch {
    id: u64,
    kind: Kind,
    /// "your download", "Blender", "github.com".
    label: String,
    since: Instant,
    last_try: Option<Instant>,
}

static WATCHES: Mutex<Vec<Watch>> = Mutex::new(Vec::new());
static NEXT: AtomicU64 = AtomicU64::new(1);

fn spoken(display: impl AsRef<str>, line: impl AsRef<str>) -> String {
    format!("{}\nSpoken: {}", display.as_ref(), line.as_ref())
}

fn norm(name: &str) -> String {
    let lower = name.trim().to_lowercase();
    let stem = lower.strip_suffix(".exe").unwrap_or(&lower);
    stem.chars().filter(|c| c.is_alphanumeric()).collect()
}

fn strip_article(name: &str) -> &str {
    let mut n = name.trim();
    for lead in ["the ", "my ", "mein ", "meine ", "der ", "die ", "das "] {
        n = n.strip_prefix(lead).unwrap_or(n);
    }
    n.trim()
}

/// The application Sir named as a process stem.
fn wanted(name: &str) -> String {
    let n = norm(strip_article(name));
    ALIASES.iter().find(|(a, _)| *a == n).map(|(_, p)| p.to_string()).unwrap_or(n)
}

/// The process is the application Sir named: the same name, the name with more after it
/// ("handbrake" → HandBrake.Worker.exe), or his name with a version after it ("Blender 4.2").
fn matches_process(process: &str, wanted: &str) -> bool {
    let p = norm(process);
    let versioned = p.len() >= 4 && wanted.strip_prefix(p.as_str()).is_some_and(|rest| rest.chars().all(|c| c.is_ascii_digit()));
    !wanted.is_empty() && (p == wanted || (wanted.len() >= 4 && p.starts_with(wanted)) || versioned)
}

fn running(system: &System, wanted: &str) -> bool {
    system.processes().values().any(|p| matches_process(&p.name().to_string_lossy(), wanted))
}

fn process_list() -> System {
    let mut system = System::new();
    system.refresh_processes(ProcessesToUpdate::All);
    system
}

/// "blender" → "Blender", "obs" → "OBS".
fn pretty(name: &str) -> String {
    let n = strip_article(name);
    if n.chars().count() <= 3 {
        return n.to_uppercase();
    }
    let mut c = n.chars();
    c.next().map(|f| f.to_uppercase().chain(c).collect()).unwrap_or_default()
}

fn is_partial(path: &Path) -> bool {
    let name = path.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
    PARTIAL.iter().any(|s| name.ends_with(s))
}

fn partials(dir: &Path) -> HashSet<PathBuf> {
    std::fs::read_dir(dir).into_iter().flatten().filter_map(Result::ok).map(|e| e.path()).filter(|p| is_partial(p)).collect()
}

/// A partial file written to within the last ten minutes (a growing file's write time moves with
/// every write): a download in progress, not last week's leftover or one that stalled.
fn fresh(path: &Path) -> bool {
    match std::fs::metadata(path).and_then(|m| m.modified()) {
        Ok(at) => at.elapsed().map_or(true, |age| age < STALE),
        Err(_) => false,
    }
}

/// The downloads in progress.
fn active(dir: &Path) -> HashSet<PathBuf> {
    partials(dir).into_iter().filter(|p| fresh(p)).collect()
}

/// The newest finished (non-empty) file written since the watch began.
fn newest_since(dir: &Path, since: SystemTime) -> Option<String> {
    std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .filter(|e| !is_partial(&e.path()))
        .filter_map(|e| {
            let meta = e.metadata().ok()?;
            let modified = meta.modified().ok()?;
            (meta.is_file() && meta.len() > 0 && modified >= since).then(|| (modified, e.file_name().to_string_lossy().into_owned()))
        })
        .max_by_key(|(t, _)| *t)
        .map(|(_, name)| name)
}

fn download_done(dir: &Path, waiting: &HashSet<PathBuf>, since: SystemTime) -> Option<String> {
    // Still going: a file it waits for is still being written, or a partial file appeared since
    // (Chrome renames "Unconfirmed 123.crdownload" once it knows the name; a second download).
    if waiting.iter().any(|p| fresh(p)) || active(dir).iter().any(|p| !waiting.contains(p)) {
        return None;
    }
    Some(match newest_since(dir, since) {
        Some(file) => format!("Your download is finished, Sir: {file}."),
        None if waiting.iter().any(|p| p.exists()) => "Your download seems stuck, Sir: nothing has arrived for ten minutes.".into(),
        None => "Your download has stopped, Sir, but there's no finished file; it may have been cancelled.".into(),
    })
}

/// A website Sir named: (address, how to say it). "github" → github.com.
fn url_of(target: &str) -> Option<(String, String)> {
    let t = strip_article(target).trim_end_matches('/').to_lowercase();
    if t.is_empty() {
        return None;
    }
    if let Some(rest) = t.strip_prefix("https://").or_else(|| t.strip_prefix("http://")) {
        return Some((t.clone(), rest.split('/').next().unwrap_or(rest).to_string()));
    }
    if t.contains(' ') && !t.contains('.') {
        return None;
    }
    let host = if t.contains('.') { t.replace(' ', "") } else { format!("{t}.com") };
    host.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-')).then(|| (format!("https://{host}"), host))
}

async fn reachable(url: &str) -> bool {
    let Ok(client) = reqwest::Client::builder().timeout(Duration::from_secs(10)).build() else { return false };
    client.get(url).send().await.is_ok_and(|r| r.status().as_u16() < 500)
}

fn add(kind: Kind, label: &str) -> Result<u64, String> {
    let mut watches = lock(&WATCHES);
    watches.retain(|w| w.since.elapsed() < MAX_AGE);
    // One download watch covers every download (asked twice, it would speak twice).
    if let Some(existing) = watches.iter().find(|w| w.kind == kind || matches!((&w.kind, &kind), (Kind::Download { .. }, Kind::Download { .. }))) {
        return Ok(existing.id);
    }
    if watches.len() >= MAX_WATCHES {
        return Err("I'm already keeping an eye on eight things for you, Sir. Shall I drop one?".into());
    }
    let id = NEXT.fetch_add(1, Ordering::Relaxed);
    info!("[WATCH] {label}");
    watches.push(Watch { id, kind, label: label.to_string(), since: Instant::now(), last_try: None });
    Ok(id)
}

fn remove(id: u64) {
    lock(&WATCHES).retain(|w| w.id != id);
}

fn describe(w: &Watch) -> String {
    match &w.kind {
        Kind::Download { .. } => "your download to finish".into(),
        Kind::Closes { .. } => format!("{} to close", w.label),
        Kind::Starts { .. } => format!("{} to start", w.label),
        Kind::Online { .. } => format!("{} to come back up", w.label),
    }
}

/// The model's live context while something is watched.
pub fn live_context() -> String {
    let watches = lock(&WATCHES);
    if watches.is_empty() {
        return String::new();
    }
    format!("Watching for Sir (watch_and_tell): {}.\n", watches.iter().map(describe).collect::<Vec<_>>().join("; "))
}

/// One look at every watch; the lines of those that happened.
async fn check(system: &mut System) -> Vec<String> {
    let watches = lock(&WATCHES).clone();
    if watches.is_empty() {
        return Vec::new();
    }
    if watches.iter().any(|w| matches!(w.kind, Kind::Closes { .. } | Kind::Starts { .. })) {
        system.refresh_processes(ProcessesToUpdate::All);
    }
    let mut said = Vec::new();
    for w in watches {
        if w.since.elapsed() > MAX_AGE {
            remove(w.id);
            continue;
        }
        let done = match &w.kind {
            Kind::Download { dir, waiting, since } => download_done(dir, waiting, *since),
            Kind::Closes { process } => (!running(system, process)).then(|| format!("{} has closed, Sir.", w.label)),
            Kind::Starts { process } => running(system, process).then(|| format!("{} is running now, Sir.", w.label)),
            Kind::Online { url } => {
                if w.last_try.is_some_and(|t| t.elapsed() < ONLINE_EVERY) {
                    None
                } else {
                    if let Some(x) = lock(&WATCHES).iter_mut().find(|x| x.id == w.id) {
                        x.last_try = Some(Instant::now());
                    }
                    reachable(url).await.then(|| format!("{} is back up, Sir.", w.label))
                }
            }
        };
        if let Some(line) = done {
            remove(w.id);
            info!("[WATCH] Done: {}", describe(&w));
            said.push(line);
        }
    }
    said
}

/// Looks every three seconds while anything is watched.
pub fn start(stop: CancellationToken) {
    if cfg!(test) {
        return;
    }
    tokio::spawn(async move {
        let mut system = System::new();
        let mut tick = tokio::time::interval(Duration::from_secs(3));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                _ = stop.cancelled() => return,
                _ = tick.tick() => {}
            }
            for line in check(&mut system).await {
                crate::announcements::say(line);
            }
        }
    });
}

// ---------------------------------------------------------------------- tool ----

pub fn is_tool(name: &str) -> bool {
    name == TOOL
}

pub fn definitions() -> Vec<Value> {
    vec![json!({"type": "function", "function": {
        "name": TOOL,
        "description": "Keeps an eye on something and tells Sir once it happens. download: the browser download in his Downloads folder finishes. closes: the application `target` exits (a render, an export, a game's update). starts: `target` starts. online: the website `target` answers again. list: what is being watched. cancel: stop watching (`target`, or everything).",
        "parameters": {"type": "object", "properties": {
            "action": {"type": "string", "enum": ["download", "closes", "starts", "online", "list", "cancel"]},
            "target": {"type": "string", "description": "the application or website"}
        }, "required": ["action"]}
    }})]
}

/// "Tell me when my download is done", "tell me when Blender is finished" (only while it runs),
/// "sag mir wenn GitHub wieder online ist".
pub fn intent(s: &str) -> Option<Value> {
    const LEADS: &[&str] = &["tell me when ", "let me know when ", "notify me when ", "ping me when ", "alert me when ", "sag mir wenn ", "sag mir bescheid wenn ", "gib mir bescheid wenn ", "sag bescheid wenn ", "gib bescheid wenn ", "melde dich wenn "];
    let rest = LEADS.iter().find_map(|l| s.find(l).map(|i| s[i + l.len()..].trim()))?;
    let rest = rest.trim_end_matches(" jai").trim();
    if ["download", "herunterladen", "heruntergeladen", "runtergeladen"].iter().any(|w| rest.contains(w)) {
        return Some(json!({ "action": "download" }));
    }
    const ONLINE: &[&str] = &[" is back up", " is back online", " is up again", " is online again", " is reachable again", " works again", " is working again", " wieder online ist", " wieder erreichbar ist", " wieder geht", " wieder funktioniert"];
    for tail in ONLINE {
        if let Some(name) = rest.strip_suffix(tail) {
            let name = strip_article(name);
            if url_of(name).is_some() {
                return Some(json!({ "action": "online", "target": name }));
            }
        }
    }
    const CLOSES: &[&str] = &[" is done", " is finished", " finishes", " is closed", " closes", " exits", " quits", " has closed", " has finished", " is done rendering", " finished rendering", " is done exporting", " fertig ist", " zu ist", " beendet ist", " geschlossen ist", " sich schließt"];
    for tail in CLOSES {
        if let Some(name) = rest.strip_suffix(tail) {
            let name = strip_article(name);
            // Only an application that is running: "tell me when the pizza is done" is a timer.
            if !name.is_empty() && name.split_whitespace().count() <= 3 && running(&process_list(), &wanted(name)) {
                return Some(json!({ "action": "closes", "target": name }));
            }
        }
    }
    None
}

pub async fn run_tool(_tools: &ToolContext, args: &Value) -> Result<String, String> {
    let action = args["action"].as_str().unwrap_or("list").to_lowercase();
    let target = str_arg(args, &["target", "app", "application", "name", "site", "url"]).unwrap_or("").trim().to_string();
    match action.as_str() {
        "download" => {
            let dir = crate::features::user_folder("Downloads");
            let waiting = active(&dir);
            if waiting.is_empty() {
                return Ok(spoken("No download in progress in Downloads", "I don't see a download in progress in your Downloads folder, Sir."));
            }
            let n = waiting.len();
            add(Kind::Download { dir, waiting, since: SystemTime::now() - Duration::from_secs(5) }, "your download")?;
            Ok(spoken(format!("Watching {n} download(s) in Downloads"), "I'll tell you the moment it's done, Sir."))
        }
        "closes" | "close" | "exits" | "starts" | "start" => {
            if target.is_empty() {
                return Ok(spoken("Which application?", "Which application, Sir?"));
            }
            let closes = action.starts_with("clos") || action == "exits";
            let process = wanted(&target);
            let label = pretty(&target);
            let is_running = running(&process_list(), &process);
            if closes && !is_running {
                return Ok(spoken(format!("{label} isn't running"), format!("{label} isn't running, Sir.")));
            }
            if !closes && is_running {
                return Ok(spoken(format!("{label} is running"), format!("{label} is already running, Sir.")));
            }
            let kind = if closes { Kind::Closes { process } } else { Kind::Starts { process } };
            add(kind, &label)?;
            Ok(spoken(format!("Watching for {label} to {}", if closes { "close" } else { "start" }), format!("I'll tell you when {label} {}, Sir.", if closes { "closes" } else { "starts" })))
        }
        "online" | "back_up" => {
            let Some((url, host)) = url_of(&target) else { return Ok(spoken("Which website?", "Which website, Sir?")) };
            if reachable(&url).await {
                return Ok(spoken(format!("{host} answers"), format!("{host} is up right now, Sir.")));
            }
            add(Kind::Online { url }, &host)?;
            Ok(spoken(format!("Watching {host} (every 30 s)"), format!("{host} isn't answering. I'll tell you when it's back, Sir.")))
        }
        "list" | "status" => {
            let watches = lock(&WATCHES).clone();
            if watches.is_empty() {
                return Ok(spoken("Nothing watched", "I'm not watching anything for you right now, Sir."));
            }
            let what: Vec<String> = watches.iter().map(describe).collect();
            Ok(spoken(format!("Watching: {}", what.join("; ")), format!("I'm waiting for {}, Sir.", crate::discord_bot::join_names(&what))))
        }
        "cancel" | "stop" => {
            let mut watches = lock(&WATCHES);
            let before = watches.len();
            if target.is_empty() {
                watches.clear();
            } else {
                let t = norm(strip_article(&target));
                watches.retain(|w| !norm(&w.label).contains(&t) && !(t.contains("download") && matches!(w.kind, Kind::Download { .. })));
            }
            let dropped = before - watches.len();
            Ok(spoken(format!("Stopped {dropped} watch(es)"), if dropped == 0 { "I wasn't watching that, Sir." } else { "Very well, Sir. I've stopped watching." }))
        }
        other => Err(format!("Unknown watch action '{other}'.")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn applications_and_sites_are_recognised() {
        assert_eq!(wanted("OBS"), "obs64");
        assert_eq!(wanted("the Blender"), "blender");
        assert!(matches_process("blender.exe", "blender"));
        assert!(matches_process("HandBrake.Worker.exe", "handbrake"));
        assert!(matches_process("obs64.exe", &wanted("obs studio")));
        assert!(!matches_process("code.exe", "codex"));
        assert!(matches_process("blender.exe", &wanted("Blender 4.2")));
        assert!(!matches_process("steam.exe", ""));
        assert_eq!(pretty("obs"), "OBS");
        assert_eq!(pretty("my blender"), "Blender");
        assert_eq!(url_of("github"), Some(("https://github.com".into(), "github.com".into())));
        assert_eq!(url_of("the status.example.org"), Some(("https://status.example.org".into(), "status.example.org".into())));
        assert_eq!(url_of("https://example.com/x/"), Some(("https://example.com/x".into(), "example.com".into())));
        assert_eq!(url_of("the big server"), None);
        assert_eq!(url_of(""), None);
    }

    #[test]
    fn tell_me_when_sentences() {
        let parsed = |s: &str| intent(s).map(|v| (v["action"].as_str().unwrap_or("").to_string(), v["target"].as_str().unwrap_or("").to_string()));
        assert_eq!(parsed("tell me when my download is done"), Some(("download".into(), String::new())));
        assert_eq!(parsed("sag mir bescheid wenn der download fertig ist"), Some(("download".into(), String::new())));
        assert_eq!(parsed("let me know when github is back up"), Some(("online".into(), "github".into())));
        assert_eq!(parsed("sag mir wenn youtube wieder online ist"), Some(("online".into(), "youtube".into())));
        // Nothing of that name runs: left to the brain (a timer, a reminder).
        assert_eq!(parsed("tell me when the pizza is done"), None);
        assert_eq!(parsed("what time is it"), None);
    }

    #[tokio::test]
    async fn a_download_watch_ends_when_the_partial_file_is_gone() {
        let dir = std::env::temp_dir().join(format!("jai-watch-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let partial = dir.join("setup.exe.crdownload");
        std::fs::write(&partial, b"half").unwrap();
        let waiting = partials(&dir);
        assert_eq!(waiting.len(), 1);
        let since = SystemTime::now() - Duration::from_secs(5);
        assert_eq!(download_done(&dir, &waiting, since), None, "still downloading");
        std::fs::rename(&partial, dir.join("setup.exe")).unwrap();
        std::fs::write(dir.join("setup.exe"), b"the whole file").unwrap();
        assert_eq!(download_done(&dir, &waiting, since).as_deref(), Some("Your download is finished, Sir: setup.exe."));
        // Cancelled: the partial file is deleted and nothing finished appears.
        let other = dir.join("big.iso.part");
        std::fs::write(&other, b"x").unwrap();
        let waiting = partials(&dir);
        std::fs::remove_file(&other).unwrap();
        assert!(download_done(&dir, &waiting, SystemTime::now() + Duration::from_secs(60)).unwrap().contains("cancelled"));
        // The global list: one watch per thing, described for the model.
        let id = add(Kind::Download { dir: dir.clone(), waiting: waiting.clone(), since }, "your download").unwrap();
        assert_eq!(add(Kind::Download { dir: dir.clone(), waiting, since }, "your download").unwrap(), id);
        assert!(live_context().contains("your download to finish"));
        let mut system = System::new();
        assert_eq!(check(&mut system).await.len(), 1, "the waited-for file is gone");
        assert!(live_context().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn leftovers_and_stalled_downloads_do_not_keep_a_watch_waiting() {
        let dir = std::env::temp_dir().join(format!("jai-watch-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let age = |path: &Path, secs: u64| std::fs::File::options().write(true).open(path).unwrap().set_modified(SystemTime::now() - Duration::from_secs(secs)).unwrap();
        // Last week's broken download is not "a download in progress".
        let leftover = dir.join("old.iso.part");
        std::fs::write(&leftover, b"x").unwrap();
        age(&leftover, 7 * 24 * 3600);
        assert!(active(&dir).is_empty());
        let running = dir.join("game.zip.crdownload");
        std::fs::write(&running, b"half").unwrap();
        let waiting = active(&dir);
        assert_eq!(waiting, HashSet::from([running.clone()]));
        let since = SystemTime::now() - Duration::from_secs(5);
        assert_eq!(download_done(&dir, &waiting, since), None);
        // Chrome names the file once it knows the name: still the same download.
        let named = dir.join("game (1).zip.crdownload");
        std::fs::rename(&running, &named).unwrap();
        assert_eq!(download_done(&dir, &waiting, since), None, "the renamed partial is still downloading");
        std::fs::rename(&named, dir.join("game.zip")).unwrap();
        assert_eq!(download_done(&dir, &waiting, since).as_deref(), Some("Your download is finished, Sir: game.zip."), "the leftover does not hold it back");
        // A download that stops growing for ten minutes is reported, not waited for all day.
        let stuck = dir.join("movie.mkv.part");
        std::fs::write(&stuck, b"x").unwrap();
        let waiting = active(&dir);
        age(&stuck, 11 * 60);
        assert_eq!(download_done(&dir, &waiting, SystemTime::now()).as_deref(), Some("Your download seems stuck, Sir: nothing has arrived for ten minutes."));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
