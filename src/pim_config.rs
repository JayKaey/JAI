//! Settings of the Personal Information Manager: e-mail (IMAP), calendar (CalDAV / `.ics`),
//! school timetable (WebUntis), LMS (itslearning), custom web feeds, background watchers and the
//! study folder.
//!
//! Every service is one row of `service_configs` (`jarvis_memory.db`): its settings as JSON plus
//! the last connection status shown in the Integrations hub. App tokens and passwords never go
//! there; they are sealed in the vault (`vault.rs`).

use crate::vault::{self, Vault};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard, RwLock};
use std::time::Duration;
use tracing::{info, warn};

pub const EMAIL: &str = "email";
pub const CALENDAR: &str = "calendar";
pub const WEBUNTIS: &str = "webuntis";
pub const LMS: &str = "itslearning";
pub const FEEDS: &str = "feeds";
pub const WATCHERS: &str = "watchers";
pub const STUDY: &str = "study";

/// The services with a connection of their own, in hub order.
pub const SERVICES: [&str; 5] = [EMAIL, CALENDAR, WEBUNTIS, LMS, FEEDS];

pub fn display(service: &str) -> &'static str {
    match service {
        EMAIL => "Email (IMAP)",
        CALENDAR => "Calendar (CalDAV / iCal)",
        WEBUNTIS => "WebUntis timetable",
        LMS => "itslearning",
        FEEDS => "Custom web feeds",
        WATCHERS => "Change watchers",
        STUDY => "Study folder",
        _ => "Unknown service",
    }
}

/// "my gmail", "Apple Kalender", "Stundenplan", "its learning" → the service id.
pub fn parse_service(text: &str) -> Option<&'static str> {
    let t = text.trim().to_lowercase();
    let has = |words: &[&str]| words.iter().any(|w| t.contains(w));
    if has(&["webuntis", "untis", "stundenplan", "timetable", "school schedule", "vertretung", "school"]) {
        Some(WEBUNTIS)
    } else if has(&["itslearning", "its learning", "it's learning", "itslearn", "lms", "course", "kurs"]) {
        Some(LMS)
    } else if has(&["gmail", "mail", "imap", "inbox", "posteingang"]) {
        Some(EMAIL)
    } else if has(&["calendar", "kalender", "icloud", "apple", "caldav", "ical", ".ics", "webcal"]) {
        Some(CALENDAR)
    } else if has(&["watch", "notifier", "beobacht"]) {
        Some(WATCHERS)
    } else if has(&["feed", "website", "web site", "webseite", "connector", "rest", "dashboard", "site"]) {
        Some(FEEDS)
    } else if has(&["study", "lern", "worksheet", "folder"]) {
        Some(STUDY)
    } else {
        None
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct EmailSettings {
    pub enabled: bool,
    pub imap_host: String,
    pub imap_port: u16,
    pub address: String,
    pub mailbox: String,
}

impl Default for EmailSettings {
    fn default() -> Self {
        Self { enabled: false, imap_host: "imap.gmail.com".into(), imap_port: 993, address: String::new(), mailbox: "INBOX".into() }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CalendarSettings {
    pub enabled: bool,
    /// CalDAV server or calendar collection ("https://caldav.icloud.com").
    pub caldav_url: String,
    /// A read-only `.ics` / `webcal://` feed (optional).
    pub ics_url: String,
    /// Apple ID e-mail for iCloud.
    pub username: String,
    /// Calendar new events go to (display name; empty = the first one).
    pub calendar_name: String,
}

impl Default for CalendarSettings {
    fn default() -> Self {
        Self { enabled: false, caldav_url: "https://caldav.icloud.com".into(), ics_url: String::new(), username: String::new(), calendar_name: String::new() }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct WebUntisSettings {
    pub enabled: bool,
    /// "hebe.webuntis.com".
    pub server: String,
    /// The school's login name (as in the WebUntis URL `?school=`).
    pub school: String,
    pub username: String,
    /// The class to read when the account is not a student account ("10b").
    pub class_name: String,
    /// A student / person element id, when known.
    pub student_id: String,
    /// JSON-RPC client name sent with `authenticate`.
    pub client_name: String,
}

impl Default for WebUntisSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            server: "hebe.webuntis.com".into(),
            school: String::new(),
            username: String::new(),
            class_name: String::new(),
            student_id: String::new(),
            client_name: "JARVIS".into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LmsSettings {
    pub enabled: bool,
    /// "https://myschool.itslearning.com".
    pub base_url: String,
    /// RSS / notification feed URL (optional).
    pub feed_url: String,
    /// A REST endpoint (path or URL) to read instead of the standard ones (optional).
    pub rest_endpoint: String,
    pub username: String,
    /// The session is refreshed when it has been idle this long (itslearning times out at ~10).
    pub keepalive_minutes: u32,
    /// "schule_bw": the federated "Login über Schule@BW" single sign-on (`itslearning_bw_sso`);
    /// empty: the REST password login (bws.itslearning.com always uses Schule@BW).
    pub auth: String,
}

impl Default for LmsSettings {
    fn default() -> Self {
        Self { enabled: false, base_url: String::new(), feed_url: String::new(), rest_endpoint: String::new(), username: String::new(), keepalive_minutes: 8, auth: String::new() }
    }
}

/// A custom website / REST connector.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct WebFeed {
    pub name: String,
    pub url: String,
    /// CSS selector (HTML) or JSON path (`$.data.items[0].status`); empty = the readable page.
    pub selector: String,
    pub interval_minutes: u32,
}

/// A background change watcher.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct WatcherSpec {
    /// "webuntis", "itslearning" or "feed:<name>".
    pub service: String,
    pub interval_minutes: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct StudySettings {
    /// Root of the study folders (empty = Documents/School).
    pub school_dir: String,
    /// Also sort matching files out of Downloads.
    pub sort_downloads: bool,
}

/// Everything the Integrations hub edits.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct PimSettings {
    pub email: EmailSettings,
    pub calendar: CalendarSettings,
    pub webuntis: WebUntisSettings,
    pub lms: LmsSettings,
    pub feeds: Vec<WebFeed>,
    pub watchers: Vec<WatcherSpec>,
    pub study: StudySettings,
}

impl PimSettings {
    pub fn feed(&self, name_or_url: &str) -> Option<&WebFeed> {
        let q = name_or_url.trim().to_lowercase();
        if q.is_empty() {
            return None;
        }
        self.feeds
            .iter()
            .find(|f| f.name.to_lowercase() == q || f.url.to_lowercase() == q)
            .or_else(|| self.feeds.iter().find(|f| !f.name.is_empty() && (q.contains(&f.name.to_lowercase()) || f.name.to_lowercase().contains(&q))))
    }
}

/// The last connection check of a service.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ServiceStatus {
    /// "connected", "error", "untested" or "offline".
    pub state: String,
    pub detail: String,
    pub last_checked: String,
}

struct Inner {
    conn: Mutex<Connection>,
    settings: RwLock<PimSettings>,
    /// Last connection checks (the HUD reads them every frame).
    statuses: RwLock<HashMap<String, ServiceStatus>>,
    vault: Vault,
}

#[derive(Clone)]
pub struct PimConfig {
    inner: Arc<Inner>,
}

impl PimConfig {
    pub fn open<P: AsRef<Path>>(db_path: P) -> Result<Self, String> {
        let conn = Connection::open(db_path.as_ref()).map_err(|e| e.to_string())?;
        let vault = Vault::open(db_path.as_ref()).unwrap_or_else(|e| {
            warn!("[PIM] The vault cannot be opened ({e}); tokens last for this session only.");
            Vault::in_memory()
        });
        Self::with(conn, vault)
    }

    /// Settings for this session only (the database could not be opened).
    pub fn in_memory() -> Self {
        Self::with(Connection::open_in_memory().expect("in-memory SQLite"), Vault::in_memory()).expect("in-memory PIM settings")
    }

    fn with(conn: Connection, vault: Vault) -> Result<Self, String> {
        conn.busy_timeout(Duration::from_secs(3)).map_err(|e| e.to_string())?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS service_configs (
                 service TEXT PRIMARY KEY,
                 settings TEXT NOT NULL DEFAULT '{}',
                 status TEXT NOT NULL DEFAULT 'untested',
                 detail TEXT NOT NULL DEFAULT '',
                 last_checked TEXT NOT NULL DEFAULT '',
                 updated_at TEXT NOT NULL
             );",
        )
        .map_err(|e| e.to_string())?;
        let settings = load(&conn);
        let statuses = load_statuses(&conn);
        let on: Vec<&str> = SERVICES.iter().filter(|s| enabled(&settings, s)).map(|s| display(s)).collect();
        info!("[PIM] Services configured: {}", if on.is_empty() { "none".to_string() } else { on.join(", ") });
        Ok(Self { inner: Arc::new(Inner { conn: Mutex::new(conn), settings: RwLock::new(settings), statuses: RwLock::new(statuses), vault }) })
    }

    fn conn(&self) -> MutexGuard<'_, Connection> {
        self.inner.conn.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn vault(&self) -> &Vault {
        &self.inner.vault
    }

    pub fn settings(&self) -> PimSettings {
        self.inner.settings.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn read<R>(&self, f: impl FnOnce(&PimSettings) -> R) -> R {
        f(&self.inner.settings.read().unwrap_or_else(|e| e.into_inner()))
    }

    /// Saves every service whose settings changed.
    pub fn save(&self, next: &PimSettings) -> Result<(), String> {
        let previous = self.settings();
        let rows: [(&str, Value, Value); 7] = [
            (EMAIL, to_json(&previous.email), to_json(&next.email)),
            (CALENDAR, to_json(&previous.calendar), to_json(&next.calendar)),
            (WEBUNTIS, to_json(&previous.webuntis), to_json(&next.webuntis)),
            (LMS, to_json(&previous.lms), to_json(&next.lms)),
            (FEEDS, to_json(&previous.feeds), to_json(&next.feeds)),
            (WATCHERS, to_json(&previous.watchers), to_json(&next.watchers)),
            (STUDY, to_json(&previous.study), to_json(&next.study)),
        ];
        let now = chrono::Local::now().to_rfc3339();
        {
            let conn = self.conn();
            for (service, before, after) in rows {
                if before == after {
                    continue;
                }
                conn.execute(
                    "INSERT INTO service_configs (service, settings, updated_at) VALUES (?1, ?2, ?3)
                     ON CONFLICT(service) DO UPDATE SET settings = excluded.settings, status = 'untested', detail = '', updated_at = excluded.updated_at",
                    params![service, after.to_string(), now],
                )
                .map_err(|e| e.to_string())?;
                self.inner.statuses.write().unwrap_or_else(|e| e.into_inner()).remove(service);
                info!("[PIM] Saved the {} settings", display(service));
            }
        }
        *self.inner.settings.write().unwrap_or_else(|e| e.into_inner()) = next.clone();
        Ok(())
    }

    pub fn update(&self, f: impl FnOnce(&mut PimSettings)) -> Result<PimSettings, String> {
        let mut next = self.settings();
        f(&mut next);
        self.save(&next)?;
        Ok(next)
    }

    pub fn status(&self, service: &str) -> ServiceStatus {
        self.inner
            .statuses
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(service)
            .cloned()
            .unwrap_or_else(|| ServiceStatus { state: "untested".into(), ..Default::default() })
    }

    pub fn set_status(&self, service: &str, ok: bool, detail: &str) {
        let now = chrono::Local::now().format("%Y-%m-%d %H:%M").to_string();
        let detail: String = detail.chars().take(300).collect();
        let state = if ok { "connected" } else { "error" };
        if let Err(e) = self.conn().execute(
            "INSERT INTO service_configs (service, status, detail, last_checked, updated_at) VALUES (?1, ?2, ?3, ?4, ?4)
             ON CONFLICT(service) DO UPDATE SET status = excluded.status, detail = excluded.detail, last_checked = excluded.last_checked",
            params![service, state, detail, now],
        ) {
            warn!("[PIM] Could not record the status of {service}: {e}");
        }
        self.inner
            .statuses
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(service.to_string(), ServiceStatus { state: state.into(), detail, last_checked: now });
    }

    pub fn token(&self, service: &str) -> Option<String> {
        self.inner.vault.token(service)
    }

    pub fn has_token(&self, service: &str) -> bool {
        self.inner.vault.has_secret(service, vault::TOKEN)
    }

    /// Stores (or with an empty value, removes) a service's app token / password.
    pub fn set_token(&self, service: &str, token: &str) -> Result<(), String> {
        if token.trim().is_empty() {
            self.inner.vault.clear_secret(service, vault::TOKEN);
            return Ok(());
        }
        self.inner.vault.set_secret(service, vault::TOKEN, token.trim())
    }

    pub fn session(&self, service: &str) -> Option<String> {
        self.inner.vault.secret(service, vault::SESSION).filter(|s| !s.trim().is_empty())
    }

    pub fn set_session(&self, service: &str, session: &str) -> Result<(), String> {
        if session.trim().is_empty() {
            self.inner.vault.clear_secret(service, vault::SESSION);
            return Ok(());
        }
        self.inner.vault.set_secret(service, vault::SESSION, session.trim())
    }

    pub fn enabled(&self, service: &str) -> bool {
        self.read(|s| enabled(s, service))
    }

    /// Why a service cannot be used yet, if it cannot.
    pub fn missing(&self, service: &str) -> Option<String> {
        let s = self.settings();
        let hint = "open the Integrations hub (\"open my productivity settings\")";
        let need = |what: &str| Some(format!("{} needs {what}; {hint}.", display(service)));
        match service {
            EMAIL if !s.email.enabled => need("to be switched on"),
            EMAIL if s.email.imap_host.trim().is_empty() || s.email.address.trim().is_empty() => need("an IMAP host and e-mail address"),
            EMAIL if !self.has_token(EMAIL) => need("an app token (for Gmail: an app password)"),
            // Without a server the calendar still works offline (local SQLite).
            CALENDAR if !s.calendar.enabled => need("to be switched on"),
            WEBUNTIS if !s.webuntis.enabled => need("to be switched on"),
            // A school's own host (myschool.webuntis.com) needs no school name; WebUntis says so
            // at login when a shared server does.
            WEBUNTIS if s.webuntis.server.trim().is_empty() => need("the school server"),
            WEBUNTIS if !self.has_token(WEBUNTIS) && self.session(WEBUNTIS).is_none() => need("a password or a session id"),
            LMS if !s.lms.enabled => need("to be switched on"),
            LMS if s.lms.base_url.trim().is_empty() && s.lms.feed_url.trim().is_empty() => need("the portal URL or a feed URL"),
            FEEDS if s.feeds.is_empty() => need("at least one web feed"),
            _ => None,
        }
    }
}

fn enabled(s: &PimSettings, service: &str) -> bool {
    match service {
        EMAIL => s.email.enabled,
        CALENDAR => s.calendar.enabled,
        WEBUNTIS => s.webuntis.enabled,
        LMS => s.lms.enabled,
        FEEDS => !s.feeds.is_empty(),
        WATCHERS => !s.watchers.is_empty(),
        _ => false,
    }
}

fn load_statuses(conn: &Connection) -> HashMap<String, ServiceStatus> {
    let Ok(mut stmt) = conn.prepare("SELECT service, status, detail, last_checked FROM service_configs") else { return HashMap::new() };
    let Ok(rows) = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, ServiceStatus { state: r.get(1)?, detail: r.get(2)?, last_checked: r.get(3)? }))) else {
        return HashMap::new();
    };
    rows.flatten().collect()
}

fn to_json<T: Serialize>(v: &T) -> Value {
    serde_json::to_value(v).unwrap_or(Value::Null)
}

fn load(conn: &Connection) -> PimSettings {
    let mut s = PimSettings::default();
    let Ok(mut stmt) = conn.prepare("SELECT service, settings FROM service_configs") else { return s };
    let Ok(rows) = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))) else { return s };
    for (service, json) in rows.flatten() {
        let Ok(v) = serde_json::from_str::<Value>(&json) else { continue };
        match service.as_str() {
            EMAIL => s.email = serde_json::from_value(v).unwrap_or_default(),
            CALENDAR => s.calendar = serde_json::from_value(v).unwrap_or_default(),
            WEBUNTIS => s.webuntis = serde_json::from_value(v).unwrap_or_default(),
            LMS => s.lms = serde_json::from_value(v).unwrap_or_default(),
            FEEDS => s.feeds = serde_json::from_value(v).unwrap_or_default(),
            WATCHERS => s.watchers = serde_json::from_value(v).unwrap_or_default(),
            STUDY => s.study = serde_json::from_value(v).unwrap_or_default(),
            _ => {}
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_persist_per_service_and_tokens_stay_in_the_vault() {
        let cfg = PimConfig::in_memory();
        cfg.update(|s| {
            s.email.enabled = true;
            s.email.address = "sir@gmail.com".into();
            s.feeds.push(WebFeed { name: "Mensa".into(), url: "https://mensa.example.org".into(), selector: ".menu".into(), interval_minutes: 30 });
        })
        .unwrap();
        assert!(cfg.missing(EMAIL).unwrap().contains("app token"));
        cfg.set_token(EMAIL, "abcd efgh").unwrap();
        assert_eq!(cfg.missing(EMAIL), None);
        assert_eq!(cfg.token(EMAIL).as_deref(), Some("abcd efgh"));
        let stored: String = cfg.conn().query_row("SELECT settings FROM service_configs WHERE service = 'email'", [], |r| r.get(0)).unwrap();
        assert!(!stored.contains("abcd"), "tokens never reach service_configs");
        assert_eq!(load(&cfg.conn()).feeds[0].selector, ".menu");
        assert_eq!(cfg.read(|s| s.feed("my mensa").map(|f| f.url.clone())).as_deref(), Some("https://mensa.example.org"));
        cfg.set_status(EMAIL, true, "3 unread");
        assert_eq!(cfg.status(EMAIL).state, "connected");
    }

    #[test]
    fn services_are_recognised_in_speech() {
        assert_eq!(parse_service("my Gmail"), Some(EMAIL));
        assert_eq!(parse_service("Apple Kalender"), Some(CALENDAR));
        assert_eq!(parse_service("Stundenplan"), Some(WEBUNTIS));
        assert_eq!(parse_service("its learning"), Some(LMS));
        assert_eq!(parse_service("the canteen website"), Some(FEEDS));
    }
}
