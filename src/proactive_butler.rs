//! The proactive butler: JAI notices things on his own and puts a card on the HUD with the
//! fix one click away — the films' "Sir, I think you should see this".
//!
//! * Hardware (what the Guardian finds: a hot GPU or CPU, memory or VRAM nearly full, a pegged
//!   processor, a drive running out of space): the telemetry window, or closing the culprit.
//! * What starts within the quarter of an hour: a lesson (WebUntis) or a calendar event.
//! * A new Minecraft crash report: the cause and the fix the Doctor recognises, and the full
//!   diagnosis.
//! * A failed Gradle build: its first errors, and the file at the line. IntelliJ, VS Code and
//!   the terminal all build through the Gradle daemon, and its log is the one place a build's
//!   errors can be read from outside the IDE — other build systems leave nothing to watch.
//!
//! A card's buttons are tool calls, not words: `click` hands the HUD the call behind a label,
//! so a fix never depends on how a sentence is understood. Nothing here acts without that
//! click; every card has its own close button and fades after `CARD_SECS`.

use crate::dynamic_popups::PopupPayload;
use crate::guardian::{Anomaly, Kind, Offer};
use chrono::{Local, NaiveDate, NaiveDateTime, Timelike};
use regex::Regex;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant, SystemTime};
use tracing::{debug, info};

/// How long a card stays (hovering or pinning keeps it).
pub const CARD_SECS: f32 = 90.0;
/// "In a quarter of an hour."
pub const LEAD_MINUTES: i64 = 15;
/// How often the files are looked at (two directory listings).
const POLL: Duration = Duration::from_secs(10);
/// The timetable is read from WebUntis this often, during the school day.
const LESSON_REFRESH: Duration = Duration::from_secs(60 * 60);
/// The birthday notes already given ("2026-03-03|today").
const BIRTHDAY_TOLD: &str = "birthday_told";
/// A card's buttons work this long after it appeared.
const FIX_LIFE: Duration = Duration::from_secs(30 * 60);
/// At most this much of a build log is read at once.
const MAX_CHUNK: u64 = 512 * 1024;

/// A one-click fix: the button's label and the tool call behind it.
#[derive(Debug, Clone, PartialEq)]
pub struct Fix {
    pub label: String,
    pub tool: &'static str,
    pub args: Value,
}

/// What the butler puts on the HUD.
#[derive(Debug, Clone, PartialEq)]
pub struct Card {
    pub title: String,
    pub body: String,
    pub fixes: Vec<Fix>,
    /// What JAI says with it (None: the card and a notice are enough).
    pub spoken: Option<String>,
}

struct Pending {
    fix: Fix,
    card: String,
    at: Instant,
}

/// The buttons of the cards that are (or were just) showing, by label.
static FIXES: LazyLock<Mutex<HashMap<String, Pending>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

/// Shows a card and arms its buttons.
pub fn show(card: &Card) {
    {
        let mut fixes = FIXES.lock().unwrap_or_else(|e| e.into_inner());
        fixes.retain(|_, p| p.at.elapsed() < FIX_LIFE);
        for fix in &card.fixes {
            fixes.insert(fix.label.clone(), Pending { fix: fix.clone(), card: card.title.clone(), at: Instant::now() });
        }
    }
    crate::dynamic_popups::spawn(
        PopupPayload::VisualDataCard { title: card.title.clone(), markdown_or_code: card.body.clone(), chart_points: Vec::new(), action_buttons: card.fixes.iter().map(|f| f.label.clone()).collect() },
        Some(CARD_SECS),
    );
    info!("[BUTLER] Card '{}' with {} one-click fix(es)", card.title, card.fixes.len());
}

/// The tool call behind a clicked card button; None when the button is not one of the butler's
/// (its label then goes to JAI as a request, as before). The card goes with the click.
pub fn click(label: &str) -> Option<Fix> {
    let mut fixes = FIXES.lock().unwrap_or_else(|e| e.into_inner());
    let pending = fixes.remove(label)?;
    fixes.retain(|_, p| p.card != pending.card);
    drop(fixes);
    crate::dynamic_popups::dismiss_key(&PopupPayload::VisualDataCard { title: pending.card, markdown_or_code: String::new(), chart_points: Vec::new(), action_buttons: Vec::new() }.key());
    (pending.at.elapsed() < FIX_LIFE).then_some(pending.fix)
}

// ------------------------------------------------------------------------- hardware ----

/// The facts of a Guardian alert, without the address and the question: "Your GPU VRAM is at
/// 96% capacity due to Chrome."
fn facts(message: &str) -> String {
    let said = message.strip_prefix("Pardon the interruption, Sir, but ").unwrap_or(message);
    let said = said.split(" Shall I ").next().unwrap_or(said).trim();
    let mut chars = said.chars();
    chars.next().map(|first| first.to_uppercase().collect::<String>() + chars.as_str()).unwrap_or_default()
}

/// The card for a Guardian alert: what "yes" would do, and closing the culprit, as buttons.
pub fn hardware(anomaly: &Anomaly, message: &str, offer: &Offer) -> Card {
    let (title, primary) = match &anomaly.kind {
        Kind::GpuHot => (format!("GPU AT {:.0} °C", anomaly.value), "OPEN TELEMETRY"),
        Kind::CpuHot => (format!("CPU AT {:.0} °C", anomaly.value), "OPEN TELEMETRY"),
        Kind::VramFull => (format!("VRAM AT {:.0} %", anomaly.value), "OPEN TELEMETRY"),
        Kind::RamFull => (format!("MEMORY AT {:.0} %", anomaly.value), "OPEN TELEMETRY"),
        Kind::CpuPegged => ("PROCESSOR PEGGED".to_string(), "PROCESS MONITOR"),
        Kind::DiskLow(drive) => (format!("DRIVE {drive} NEARLY FULL"), "DRIVE SPACE"),
    };
    let mut fixes = vec![Fix { label: primary.to_string(), tool: offer.primary.0, args: offer.primary.1.clone() }];
    if let Some(app) = &offer.terminate {
        fixes.push(Fix { label: format!("CLOSE {}", crate::apps::display_name(app).to_uppercase()), tool: "close_application", args: json!({ "app_name": app, "force": true }) });
    }
    Card { title, body: facts(message), fixes, spoken: Some(message.to_string()) }
}

// ---------------------------------------------------------------------- integrations ----

/// A service the briefing could not read (`service`: "itslearning", "WebUntis", "Email";
/// `site`: its web page): what is wrong in one plain sentence (the card's body, also what he
/// says) and the one fix as a button. The raw error stays in the log. (Sir, 2026-11-16: the
/// briefing card showed itslearning's REST paths and HTTP codes.)
pub fn integration_trouble(service: &str, error: &str, site: Option<&str>) -> Card {
    let e = error.to_lowercase();
    let has = |words: &[&str]| words.iter().any(|w| e.contains(w));
    let hub = Fix { label: "OPEN THE INTEGRATIONS HUB".into(), tool: "configure_productivity_service", args: json!({ "service_name": service, "action": "open" }) };
    let browser = site.filter(|s| s.starts_with("https://")).map(|url| Fix { label: format!("OPEN {} IN THE BROWSER", service.to_uppercase()), tool: "open_website", args: json!({ "url": url }) });
    let (body, fixes) = if has(&["refused the session", "app code"]) {
        // (2026-11-16: itslearning's own server fails at the app's consent step, HTTP 500.)
        (format!("{service} signs you in, but its server won't hand your course data to me; it's all there in the browser."), browser.into_iter().collect())
    } else if has(&["needs your", "enter it once", "no password"]) {
        (format!("{service} needs your password once."), vec![hub])
    } else if has(&["401", "403", "password", "credentials", "authentication", "sign-in failed", "login failed"]) {
        (format!("{service} turned the sign-in down: the password may have changed."), vec![hub])
    } else if has(&["timed out", "timeout", "not reachable", "dns", "connect", "network"]) {
        (format!("{service} didn't answer: the connection or its server is down. I'll try again later."), Vec::new())
    } else if has(&["http 500", "http 502", "http 503", "http 504"]) {
        (format!("{service}'s server is having trouble right now."), Vec::new())
    } else {
        (format!("{service} didn't work this time."), vec![hub])
    };
    Card { title: format!("{} // NEEDS A HAND", service.to_uppercase()), body: body.clone(), fixes, spoken: Some(body) }
}

// --------------------------------------------------------------------------- agenda ----

/// A lesson or an event with a start time.
#[derive(Debug, Clone, PartialEq)]
pub struct Upcoming {
    pub title: String,
    pub start: NaiveDateTime,
    /// The room (a lesson) or the location (an event); may be empty.
    pub place: String,
    pub lesson: bool,
}

/// What starts within the next quarter of an hour and has not started yet, soonest first.
pub fn due(now: NaiveDateTime, items: &[Upcoming]) -> Vec<&Upcoming> {
    let mut due: Vec<&Upcoming> = items
        .iter()
        .filter(|u| {
            let secs = (u.start - now).num_seconds();
            secs > 0 && secs <= LEAD_MINUTES * 60
        })
        .collect();
    due.sort_by_key(|u| u.start);
    due
}

/// The card for what starts next (everything starting at the same minute shares one).
pub fn upcoming_card(now: NaiveDateTime, starting: &[&Upcoming]) -> Option<Card> {
    let first = starting.first()?;
    let minutes = (((first.start - now).num_seconds() as f64) / 60.0).ceil().max(1.0) as i64;
    let unit = if minutes == 1 { "minute" } else { "minutes" };
    let describe = |u: &Upcoming| {
        let place = u.place.trim();
        match (place.is_empty(), u.lesson) {
            (true, _) => u.title.clone(),
            (false, true) => format!("{}, room {place}", u.title),
            (false, false) => format!("{}, {place}", u.title),
        }
    };
    let names: Vec<String> = starting.iter().map(|u| describe(u)).collect();
    Some(Card {
        title: format!("NEXT UP // {}", first.start.format("%H:%M")),
        body: format!("{}\nStarts in {minutes} {unit}.", names.join("\n")),
        fixes: vec![
            Fix { label: "OPEN SCHEDULE".into(), tool: "open_jai_view", args: json!({ "view": "schedule" }) },
            Fix { label: "REMIND ME AT START".into(), tool: "reminders", args: json!({ "action": "set", "text": format!("{} starts now", first.title), "in_minutes": minutes }) },
        ],
        spoken: Some(format!("Sir, {} starts in {minutes} {unit}.", describe(first))),
    })
}

/// What is ahead today: the calendar (read from its local store) and the timetable (read from
/// WebUntis once an hour during the school day).
#[derive(Default)]
struct Agenda {
    lessons: Option<(Instant, NaiveDate, Vec<Upcoming>)>,
    announced: HashSet<String>,
}

impl Agenda {
    async fn cards(&mut self, app: &crate::app::Jai) -> Vec<Card> {
        let now = Local::now().naive_local();
        let pim = &app.tools().pim;
        let mut items: Vec<Upcoming> = pim
            .calendar()
            .events_between(now, now + chrono::Duration::minutes(LEAD_MINUTES + 1))
            .into_iter()
            .filter(|e| !e.all_day)
            .map(|e| Upcoming { title: e.title, start: e.start, place: e.location, lesson: false })
            .collect();
        let today = now.date();
        let stale = self.lessons.as_ref().is_none_or(|(at, day, _)| *day != today || at.elapsed() > LESSON_REFRESH);
        if stale && (6..18).contains(&now.hour()) {
            let lessons = match pim.lessons(today, today).await {
                Ok(lessons) => lessons
                    .into_iter()
                    .filter(|l| l.status != crate::webuntis_client::LessonStatus::Cancelled)
                    .map(|l| Upcoming { title: l.display_subject(), start: l.date.and_time(l.start), place: l.rooms.join(", "), lesson: true })
                    .collect(),
                Err(why) => {
                    debug!("[BUTLER] No timetable to watch: {why}");
                    Vec::new()
                }
            };
            self.lessons = Some((Instant::now(), today, lessons));
        }
        if let Some((_, _, lessons)) = &self.lessons {
            items.extend(lessons.iter().cloned());
        }
        if self.announced.len() > 256 {
            self.announced.clear();
        }
        let fresh: Vec<&Upcoming> = due(now, &items).into_iter().filter(|u| self.announced.insert(format!("{}@{}", u.title, u.start))).collect();
        let mut starts: Vec<NaiveDateTime> = fresh.iter().map(|u| u.start).collect();
        starts.dedup();
        let mut cards: Vec<Card> = starts.into_iter().filter_map(|at| upcoming_card(now, &fresh.iter().copied().filter(|u| u.start == at).collect::<Vec<_>>())).collect();
        if matches!(now.hour(), 7..=11 | 19..=22) {
            let (from, to) = (today.and_hms_opt(0, 0, 0).unwrap_or(now), (today + chrono::Duration::days(2)).and_hms_opt(0, 0, 0).unwrap_or(now));
            let all_day: Vec<(NaiveDate, String)> = pim.calendar().events_between(from, to).into_iter().filter(|e| e.all_day).map(|e| (e.start.date(), e.title)).collect();
            // Told once per day and time of day, kept in his store so a restart does not repeat it.
            if let Some((key, title, body, spoken)) = crate::kit_notes::birthday_note(now, &crate::kit_notes::birthdays(), &all_day) {
                if crate::kit::get(BIRTHDAY_TOLD, &key).is_none() && crate::kit::put(BIRTHDAY_TOLD, &key, &json!(true)).is_ok() {
                    cards.push(Card { title, body, fixes: Vec::new(), spoken: Some(spoken) });
                }
            }
        }
        cards
    }
}

// ------------------------------------------------------------------------ Minecraft ----

/// The card for a new crash report: the cause and fix the Doctor recognises.
pub fn minecraft_crash(path: &Path, report: &str) -> Card {
    let findings = crate::minecraft_doctor::analyse(report);
    let (body, spoken) = match findings.first() {
        Some(f) => (format!("{}\nFix: {}", f.problem, f.fix), format!("Sir, Minecraft has crashed. {} The fix is on the card.", f.problem)),
        None => (
            crate::minecraft_doctor::description(report).unwrap_or_else(|| "A new crash report was written.".to_string()),
            "Sir, Minecraft has crashed. The crash report is one click away.".to_string(),
        ),
    };
    let file = path.to_string_lossy().to_string();
    Card {
        title: "MINECRAFT CRASHED".into(),
        body,
        fixes: vec![
            Fix { label: "FULL DIAGNOSIS".into(), tool: "diagnose_minecraft_or_server", args: json!({ "action": "diagnose", "log_path_or_command": file }) },
            Fix { label: "OPEN CRASH REPORT".into(), tool: "open_application", args: json!({ "app_name": "notepad", "args": [file] }) },
        ],
        spoken: Some(spoken),
    }
}

// ---------------------------------------------------------------------------- builds ----

/// One compiler error of a failed build.
#[derive(Debug, Clone, PartialEq)]
pub struct BuildError {
    pub file: String,
    pub line: u32,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct BuildFailure {
    pub project: String,
    /// The first errors (at most `SHOWN_ERRORS`).
    pub errors: Vec<BuildError>,
    pub total: usize,
    /// Gradle's own "What went wrong" line.
    pub why: String,
}

const SHOWN_ERRORS: usize = 4;
const FAILED: &str = "BUILD FAILED";
const SUCCESSFUL: &str = "BUILD SUCCESSFUL";

/// `D:\mod\src\main\java\demo\Handler.java:10: error: cannot find symbol` (javac, Groovy, Scala).
static JAVAC_ERROR: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?m)^\s*((?:[A-Za-z]:)?[^:\r\n]+\.(?:java|groovy|scala)):(\d+): error: (.+?)\s*$").expect("javac error line"));
/// `e: file:///D:/mod/src/main/kotlin/demo/Handler.kt:12:5 Unresolved reference: foo` (Kotlin).
static KOTLIN_ERROR: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?m)^e: (?:file:///)?((?:[A-Za-z]:)?[^:\r\n]+\.kts?):(\d+):\d+:? (.+?)\s*$").expect("kotlin error line"));

/// The folder a source file's project lives in: the one above `src`.
fn project_of(file: &str) -> String {
    let path = file.replace('\\', "/");
    path.split("/src/").next().filter(|root| root.len() < path.len()).and_then(|root| root.rsplit('/').next()).filter(|p| !p.is_empty()).unwrap_or("the project").to_string()
}

/// Where the last finished build of a log chunk ends (the byte after its verdict).
fn last_verdict(chunk: &str) -> Option<usize> {
    let failed = chunk.rfind(FAILED).map(|i| i + FAILED.len());
    let passed = chunk.rfind(SUCCESSFUL).map(|i| i + SUCCESSFUL.len());
    failed.max(passed)
}

/// The failed build at the end of a Gradle daemon log chunk. None when the last build that
/// finished in it succeeded, or none finished.
pub fn build_failure(chunk: &str) -> Option<BuildFailure> {
    let failed = chunk.rfind(FAILED)?;
    if chunk.rfind(SUCCESSFUL).is_some_and(|ok| ok > failed) {
        return None;
    }
    // This build only: what follows the verdict of the one before it.
    let from = last_verdict(&chunk[..failed]).unwrap_or(0);
    let build = &chunk[from..failed];
    let mut errors: Vec<BuildError> = JAVAC_ERROR
        .captures_iter(build)
        .chain(KOTLIN_ERROR.captures_iter(build))
        .map(|c| BuildError { file: c[1].trim().replace('/', "\\"), line: c[2].parse().unwrap_or(1), message: c[3].trim().to_string() })
        .collect();
    let total = errors.len();
    errors.truncate(SHOWN_ERRORS);
    let why = build
        .split("* What went wrong:")
        .nth(1)
        .and_then(|rest| rest.lines().map(str::trim).find(|l| !l.is_empty()))
        .unwrap_or("")
        .to_string();
    let project = errors.first().map(|e| project_of(&e.file)).unwrap_or_else(|| "the project".to_string());
    Some(BuildFailure { project, errors, total, why })
}

/// The card for a failed build: its first errors, and the file at the line in the editor.
pub fn build_card(f: &BuildFailure) -> Card {
    let mut lines: Vec<String> = Vec::new();
    match f.total {
        0 => lines.push(if f.why.is_empty() { "The build failed without a compiler error.".to_string() } else { f.why.clone() }),
        n => {
            lines.push(format!("{n} error{}{}", if n == 1 { "" } else { "s" }, if n > f.errors.len() { format!("; the first {}:", f.errors.len()) } else { ":".to_string() }));
            lines.extend(f.errors.iter().map(|e| format!("{}:{}  {}", e.file.rsplit('\\').next().unwrap_or(&e.file), e.line, e.message)));
        }
    }
    let fixes = f
        .errors
        .first()
        .map(|e| Fix { label: "OPEN FIRST ERROR".into(), tool: "open_application", args: json!({ "app_name": "vscode", "args": ["-g", format!("{}:{}", e.file, e.line)] }) })
        .into_iter()
        .collect();
    Card { title: format!("BUILD FAILED // {}", f.project.to_uppercase()), body: lines.join("\n"), fixes, spoken: None }
}

/// The Gradle daemon logs: `~/.gradle/daemon/<version>/daemon-<pid>.out.log`, with their sizes.
fn gradle_logs(root: &Path) -> Vec<(PathBuf, u64)> {
    let Ok(versions) = std::fs::read_dir(root) else { return Vec::new() };
    versions
        .flatten()
        .filter_map(|v| std::fs::read_dir(v.path()).ok())
        .flatten()
        .flatten()
        .filter(|e| {
            let name = e.file_name().to_string_lossy().to_lowercase();
            name.starts_with("daemon-") && name.ends_with(".out.log")
        })
        .filter_map(|e| Some((e.path(), e.metadata().ok()?.len())))
        .collect()
}

fn read_range(path: &Path, from: u64, to: u64) -> Option<String> {
    let mut file = std::fs::File::open(path).ok()?;
    file.seek(SeekFrom::Start(from)).ok()?;
    let mut bytes = vec![0u8; (to.saturating_sub(from)) as usize];
    file.read_exact(&mut bytes).ok()?;
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

// ---------------------------------------------------------------------------- watch ----

/// The files the butler watches, and how far it has read them.
struct Watch {
    /// The newest Minecraft crash report already handled.
    crash_seen: SystemTime,
    /// Gradle daemon logs: the bytes already read. None until the first look (what is there
    /// when JAI starts is history, not news).
    gradle: Option<HashMap<PathBuf, u64>>,
}

impl Watch {
    fn new() -> Self {
        Self { crash_seen: SystemTime::now(), gradle: None }
    }

    /// A crash report newer than the last one handled.
    fn crashed(&mut self, newest: Option<(PathBuf, SystemTime)>) -> Option<PathBuf> {
        let (path, written) = newest?;
        if written <= self.crash_seen {
            return None;
        }
        self.crash_seen = written;
        Some(path)
    }

    /// A build that failed since the last look. A build still running is left unread until
    /// its verdict is in the log, so its errors and its end are read together.
    fn build_failed(&mut self, root: &Path) -> Option<BuildFailure> {
        let logs = gradle_logs(root);
        let Some(read) = self.gradle.as_mut() else {
            self.gradle = Some(logs.into_iter().collect());
            return None;
        };
        let mut found = None;
        for (path, len) in logs {
            let seen = read.get(&path).copied().unwrap_or(0).min(len);
            if len == seen {
                read.insert(path, seen);
                continue;
            }
            let from = seen.max(len.saturating_sub(MAX_CHUNK));
            let Some(chunk) = read_range(&path, from, len) else { continue };
            match last_verdict(&chunk) {
                Some(end) => {
                    if let Some(failure) = build_failure(&chunk) {
                        found = Some(failure);
                    }
                    read.insert(path, from + end as u64);
                }
                None => {
                    read.insert(path, from);
                }
            }
        }
        found
    }

    /// One look at the files (blocking: off the async threads).
    fn poll(&mut self, tools: &crate::tools::ToolContext) -> Vec<Card> {
        let mut cards = Vec::new();
        if let Some(path) = self.crashed(crate::minecraft_doctor::newest_crash_report(tools)) {
            match std::fs::read(&path) {
                Ok(bytes) => cards.push(minecraft_crash(&path, &String::from_utf8_lossy(&bytes[..bytes.len().min(MAX_CHUNK as usize)]))),
                Err(e) => debug!("[BUTLER] {} is not readable: {e}", path.display()),
            }
        }
        if let Some(root) = crate::apps::home_dir().map(|h| h.join(".gradle").join("daemon")) {
            if let Some(failure) = self.build_failed(&root) {
                cards.push(build_card(&failure));
            }
        }
        cards
    }
}

/// Shows a card; says its line (as an alert, with the alert's two pips: when JAI is free, in
/// Sir's reply mode) or, for a silent one, leaves a notice.
fn present(app: &crate::app::Jai, card: Card) {
    show(&card);
    match &card.spoken {
        Some(line) => {
            crate::tts::play_cue(crate::jai_voice_dsp::Earcon::Alert);
            app.raise_alert(line.clone());
        }
        None => app.notice(format!("{} // {}", card.title, card.body.lines().next().unwrap_or(""))),
    }
}

/// Starts the butler: the agenda every 30 s, the files every 10 s. Off while proactive alerts
/// are off and while the live self-test runs.
pub fn start(app: crate::app::Jai) {
    crate::telemetry::watch_cpu_temperature();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(20)).await;
        let mut agenda = Agenda::default();
        let mut watch = Some(Watch::new());
        let mut tick = tokio::time::interval(POLL);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut round = 0u64;
        loop {
            tick.tick().await;
            round += 1;
            let tools = app.tools().clone();
            if !tools.config.read(|c| c.proactive_alerts) || crate::live_self_tester::running() {
                continue;
            }
            if round % 3 == 1 {
                for card in agenda.cards(&app).await {
                    present(&app, card);
                }
            }
            let mut watching = watch.take().unwrap_or_else(Watch::new);
            let looked = tokio::task::spawn_blocking(move || {
                let cards = watching.poll(&tools);
                (watching, cards)
            })
            .await;
            if let Ok((watching, cards)) = looked {
                watch = Some(watching);
                for card in cards {
                    present(&app, card);
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 2026-11-16: the briefing card read "Unavailable: … /restapi/personal/tasks/v1?… HTTP 401
    /// Unauthorized; …". Now one plain sentence, and the fix that fits as a button.
    #[test]
    fn a_failing_service_is_told_plainly_with_its_fix() {
        let site = Some("https://myschool.itslearning.com");
        let refused = integration_trouble("itslearning", "signed in through Schule@BW, but itslearning's course data refused the session: /restapi/personal/tasks/v1?PageIndex=0&PageSize=40: HTTP 401 Unauthorized", site);
        assert!(!refused.body.contains("restapi") && !refused.body.contains("401"), "{}", refused.body);
        assert_eq!(refused.fixes.iter().map(|f| (f.tool, f.args["url"].as_str())).collect::<Vec<_>>(), [("open_website", site)]);
        let password = integration_trouble("itslearning", "itslearning needs your Schule@BW password — enter it once in the Integrations Hub (it is kept encrypted)", site);
        assert_eq!((password.body.as_str(), password.fixes[0].tool), ("itslearning needs your password once.", "configure_productivity_service"));
        let down = integration_trouble("Email", "imap.example.com: connection timed out", None);
        assert!(down.fixes.is_empty() && down.body.contains("didn't answer"), "{}", down.body);
        assert!(integration_trouble("WebUntis", "HTTP 401 Unauthorized", None).body.contains("password may have changed"));
    }

    fn at(h: u32, m: u32) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(2026, 11, 5).unwrap().and_hms_opt(h, m, 0).unwrap()
    }

    #[test]
    fn a_cards_button_is_a_tool_call_not_a_sentence() {
        let card = Card {
            title: "TEST CARD".into(),
            body: "Something to see.".into(),
            fixes: vec![
                Fix { label: "BUTLER TEST FIX".into(), tool: "spawn_native_window", args: json!({ "live": "pc_stats" }) },
                Fix { label: "BUTLER TEST OTHER".into(), tool: "close_application", args: json!({ "app_name": "notepad" }) },
            ],
            spoken: None,
        };
        show(&card);
        assert_eq!(click("Play Clip"), None, "a model's own card button stays a request to JAI");
        let fix = click("BUTLER TEST FIX").expect("the card's fix");
        assert_eq!((fix.tool, &fix.args["live"]), ("spawn_native_window", &json!("pc_stats")));
        assert_eq!(click("BUTLER TEST FIX"), None, "one click, one fix");
        assert_eq!(click("BUTLER TEST OTHER"), None, "the card went with the click, and its other button with it");
    }

    #[test]
    fn hardware_cards_offer_the_window_and_the_culprit() {
        let a = Anomaly { kind: Kind::VramFull, value: 96.0 };
        let t = crate::telemetry::SystemTelemetry::default();
        let (message, offer) = crate::guardian::compose(&a, Some("chrome"), &t);
        let card = hardware(&a, &message, &offer);
        assert_eq!(card.title, "VRAM AT 96 %");
        assert!(card.body.starts_with("Your GPU VRAM is at 96% capacity") && !card.body.contains("Shall I"), "{}", card.body);
        assert_eq!(card.fixes[0], Fix { label: "OPEN TELEMETRY".into(), tool: "spawn_native_window", args: json!({ "live": "pc_stats", "title": "PC Stats" }) });
        assert_eq!(card.fixes[1].tool, "close_application");
        assert!(card.fixes[1].label.starts_with("CLOSE ") && card.fixes[1].args["app_name"] == "chrome", "{:?}", card.fixes[1]);
        assert_eq!(card.spoken.as_deref(), Some(message.as_str()), "he still says it");
        // A hot processor: no culprit to close, the telemetry window only.
        let hot = Anomaly { kind: Kind::CpuHot, value: 93.4 };
        let (message, offer) = crate::guardian::compose(&hot, None, &t);
        let card = hardware(&hot, &message, &offer);
        assert_eq!(card.title, "CPU AT 93 °C");
        assert_eq!(card.fixes.len(), 1);
        // A full drive names the drive.
        let low = Anomaly { kind: Kind::DiskLow("C:".into()), value: 3.2 };
        let (message, offer) = crate::guardian::compose(&low, None, &t);
        let card = hardware(&low, &message, &offer);
        assert_eq!((card.title.as_str(), card.fixes[0].label.as_str(), &card.fixes[0].args["live"]), ("DRIVE C: NEARLY FULL", "DRIVE SPACE", &json!("drive_space")));
    }

    #[test]
    fn what_starts_within_a_quarter_of_an_hour_gets_a_card() {
        let items = vec![
            Upcoming { title: "Dentist".into(), start: at(14, 30), place: "Main Street 4".into(), lesson: false },
            Upcoming { title: "Physics".into(), start: at(8, 35), place: "204".into(), lesson: true },
            Upcoming { title: "Maths".into(), start: at(9, 25), place: String::new(), lesson: true },
            Upcoming { title: "English".into(), start: at(7, 50), place: "110".into(), lesson: true },
        ];
        let now = at(8, 21);
        let soon = due(now, &items);
        assert_eq!(soon.iter().map(|u| u.title.as_str()).collect::<Vec<_>>(), ["Physics"], "not what has begun, not what is an hour away");
        let card = upcoming_card(now, &soon).unwrap();
        assert_eq!(card.title, "NEXT UP // 08:35");
        assert_eq!(card.body, "Physics, room 204\nStarts in 14 minutes.");
        assert_eq!(card.spoken.as_deref(), Some("Sir, Physics, room 204 starts in 14 minutes."));
        assert_eq!(card.fixes[0], Fix { label: "OPEN SCHEDULE".into(), tool: "open_jai_view", args: json!({ "view": "schedule" }) });
        assert_eq!(card.fixes[1].args, json!({ "action": "set", "text": "Physics starts now", "in_minutes": 14 }));
        // Exactly a quarter of an hour ahead counts; sixteen minutes do not, nor the minute it starts.
        assert_eq!(due(at(8, 20), &items).len(), 1);
        assert!(due(at(8, 19), &items).is_empty() && due(at(8, 35), &items).is_empty());
        // An event: its location, in one minute.
        let card = upcoming_card(at(14, 29), &due(at(14, 29), &items)).unwrap();
        assert_eq!(card.body, "Dentist, Main Street 4\nStarts in 1 minute.");
        assert!(upcoming_card(now, &[]).is_none());
    }

    #[test]
    fn a_minecraft_crash_names_the_cause_and_the_fix() {
        let report = "---- Minecraft Crash Report ----\nDescription: Rendering overlay\n\njava.lang.OutOfMemoryError: Java heap space\n\tat net.minecraft.client.Minecraft.run";
        let path = Path::new(r"C:\Users\sir\AppData\Roaming\.minecraft\crash-reports\crash-2026-11-05_21.40.01-client.txt");
        let card = minecraft_crash(path, report);
        assert_eq!(card.title, "MINECRAFT CRASHED");
        assert!(card.body.contains("ran out of memory") && card.body.contains("Fix: Give it more RAM"), "{}", card.body);
        assert!(card.spoken.as_deref().unwrap().starts_with("Sir, Minecraft has crashed. The game ran out of memory"));
        assert_eq!(card.fixes[0].tool, "diagnose_minecraft_or_server");
        assert_eq!(card.fixes[0].args["log_path_or_command"], path.to_string_lossy().as_ref());
        assert_eq!(card.fixes[1].args["args"][0], path.to_string_lossy().as_ref());
        // A crash the Doctor has no name for: the report's own description.
        let card = minecraft_crash(path, "---- Minecraft Crash Report ----\nDescription: Ticking entity\n");
        assert_eq!(card.body, "Ticking entity");
    }

    /// A Gradle daemon log as it reads on Sir's machine (paths shortened).
    const FAILED_BUILD: &str = "> Task :compileJava FAILED\r\nD:\\Code\\Mods\\Demo-1.21.1\\src\\main\\java\\demo\\TravelHandler.java:10: error: package net.neoforged.neoforge.event.TickEvent does not exist\r\nimport net.neoforged.neoforge.event.TickEvent.PlayerTickEvent;\r\n                                              ^\r\nD:\\Code\\Mods\\Demo-1.21.1\\src\\main\\java\\demo\\TravelHandler.java:30: error: cannot find symbol\r\n    public static void onTick(PlayerTickEvent event) {\r\n  symbol:   class PlayerTickEvent\r\nD:\\Code\\Mods\\Demo-1.21.1\\src\\main\\java\\demo\\config\\ConfigManager.java:3: error: package net.minecraftforge.common does not exist\r\n3 errors\r\n\r\nFAILURE: Build failed with an exception.\r\n\r\n* What went wrong:\r\nExecution failed for task ':compileJava'.\r\n> Compilation failed; see the compiler error output for details.\r\n\r\nBUILD FAILED in 4s\r\n";

    #[test]
    fn a_failed_gradle_build_shows_its_first_errors_and_opens_the_file() {
        let f = build_failure(FAILED_BUILD).expect("the build failed");
        assert_eq!((f.project.as_str(), f.total, f.errors.len()), ("Demo-1.21.1", 3, 3));
        assert_eq!(f.errors[0], BuildError { file: r"D:\Code\Mods\Demo-1.21.1\src\main\java\demo\TravelHandler.java".into(), line: 10, message: "package net.neoforged.neoforge.event.TickEvent does not exist".into() });
        assert_eq!(f.why, "Execution failed for task ':compileJava'.");
        let card = build_card(&f);
        assert_eq!(card.title, "BUILD FAILED // DEMO-1.21.1");
        assert_eq!(card.body.lines().collect::<Vec<_>>()[..2], ["3 errors:", "TravelHandler.java:10  package net.neoforged.neoforge.event.TickEvent does not exist"]);
        assert_eq!(card.fixes[0].args, json!({ "app_name": "vscode", "args": ["-g", r"D:\Code\Mods\Demo-1.21.1\src\main\java\demo\TravelHandler.java:10"] }));
        assert_eq!(card.spoken, None, "a build error is shown, not announced");
        // Kotlin's error lines, and more errors than fit the card.
        let kotlin: String = (1..=6).map(|i| format!("e: file:///D:/Code/Mods/Kt/src/main/kotlin/demo/Handler.kt:{i}:5 Unresolved reference: thing{i}\n")).collect::<String>() + "BUILD FAILED in 2s\n";
        let f = build_failure(&kotlin).unwrap();
        assert_eq!((f.project.as_str(), f.total, f.errors.len(), f.errors[0].line), ("Kt", 6, SHOWN_ERRORS, 1));
        assert!(build_card(&f).body.starts_with("6 errors; the first 4:"));
        // The last build counts: fixed and rebuilt is no failure, and an old failure's errors
        // are not this build's.
        assert_eq!(build_failure(&format!("{FAILED_BUILD}> Task :build\nBUILD SUCCESSFUL in 9s\n")), None);
        let again = build_failure(&format!("{FAILED_BUILD}\n* What went wrong:\nA problem occurred configuring root project 'Demo'.\nBUILD FAILED in 1s\n")).unwrap();
        assert_eq!((again.total, again.why.as_str()), (0, "A problem occurred configuring root project 'Demo'."));
        assert_eq!(build_card(&again).fixes.len(), 0);
        assert_eq!(build_failure("> Task :compileJava\n"), None, "still building");
    }

    #[test]
    fn the_watch_reports_news_only_and_waits_for_a_builds_verdict() {
        let root = std::env::temp_dir().join(format!("jai-butler-test-{}", std::process::id()));
        let dir = root.join("9.2.1");
        std::fs::create_dir_all(&dir).unwrap();
        let log = dir.join("daemon-4242.out.log");
        std::fs::write(&log, FAILED_BUILD).unwrap();
        let mut watch = Watch::new();
        assert_eq!(watch.build_failed(&root), None, "what failed before JAI started is history");
        assert_eq!(watch.build_failed(&root), None);
        // A new build: its errors arrive first, its verdict a moment later.
        let (errors, verdict) = FAILED_BUILD.split_at(FAILED_BUILD.find("FAILURE:").unwrap());
        let append = |text: &str| {
            use std::io::Write;
            std::fs::OpenOptions::new().append(true).open(&log).unwrap().write_all(text.as_bytes()).unwrap();
        };
        append(errors);
        assert_eq!(watch.build_failed(&root), None, "the build is still running");
        append(verdict);
        let failure = watch.build_failed(&root).expect("the finished build");
        assert_eq!(failure.total, 3, "errors and verdict are read together");
        assert_eq!(watch.build_failed(&root), None, "reported once");
        append("> Task :build\nBUILD SUCCESSFUL in 3s\n");
        assert_eq!(watch.build_failed(&root), None);
        std::fs::remove_dir_all(&root).ok();
        // Crash reports: only one written after the last one seen.
        let mut watch = Watch::new();
        let before = watch.crash_seen - Duration::from_secs(60);
        assert_eq!(watch.crashed(Some((PathBuf::from("old.txt"), before))), None);
        let after = watch.crash_seen + Duration::from_secs(5);
        assert_eq!(watch.crashed(Some((PathBuf::from("new.txt"), after))), Some(PathBuf::from("new.txt")));
        assert_eq!(watch.crashed(Some((PathBuf::from("new.txt"), after))), None);
        assert_eq!(watch.crashed(None), None);
    }
}
