//! Apple Calendar (iCloud) or any CalDAV server (RFC 4791) plus read-only iCalendar feeds
//! (RFC 5545), mirrored into `calendar_events` (`jai_memory.db`) so the schedule also works
//! offline.
//!
//! * Reading: calendars are discovered (current-user-principal → calendar-home-set → the VEVENT
//!   collections), then one `REPORT calendar-query` per calendar fetches the time range; `.ics`
//!   feeds are fetched whole. Recurring events are expanded (DAILY / WEEKLY / MONTHLY / YEARLY
//!   with INTERVAL, COUNT, UNTIL, BYDAY, BYMONTHDAY, EXDATE and overridden instances).
//! * Writing: a new event is built as a `VEVENT`, saved locally first and `PUT` to the server;
//!   without a connection it stays pending and is pushed on the next sync. Rescheduling edits
//!   the server's own resource in place (alarms and attendees survive); deleting is a `DELETE`.
//! * iCloud discovery is the documented three steps — `PROPFIND` (Depth 0) on
//!   `https://caldav.icloud.com/` for `current-user-principal`, `PROPFIND` on the principal for
//!   `calendar-home-set` (the regional `pXX-caldav.icloud.com` host), `PROPFIND` (Depth 1) on the
//!   home for the collections — and the writable `VEVENT` collections found are cached in
//!   `caldav_discovery` so later starts skip it. A new event is verified with a `GET` after its
//!   `PUT`.
//! * Times: UTC values are converted to local time; `TZID` and floating times are taken as
//!   local wall-clock time (Sir's calendars live in his own time zone). JAI writes UTC.

use crate::pim_config::CalendarSettings;
use crate::web_connector::{err_chain, normalize_url, resolve};
use chrono::{Datelike, Duration as ChronoDuration, Local, NaiveDate, NaiveDateTime, NaiveTime, TimeZone, Utc, Weekday};
use regex::Regex;
use rusqlite::{params, Connection};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::{Arc, LazyLock, Mutex, MutexGuard};
use std::time::{Duration, Instant};
use tracing::{info, warn};

const STAMP: &str = "%Y-%m-%dT%H:%M:%S";
const PRODID: &str = "-//Stark Industries//J.A.I.//EN";

#[derive(Debug, Clone, PartialEq)]
pub struct CalEvent {
    pub uid: String,
    pub title: String,
    pub start: NaiveDateTime,
    pub end: NaiveDateTime,
    pub all_day: bool,
    pub location: String,
    pub description: String,
    /// "caldav", "ics" or "local".
    pub source: String,
    /// The server resource (CalDAV), for edits.
    pub href: String,
    pub etag: String,
    pub recurring: bool,
    /// "" (in sync), "put" or "delete" (waiting for the server).
    pub pending: String,
}

impl CalEvent {
    pub fn new(title: &str, start: NaiveDateTime, end: NaiveDateTime, all_day: bool) -> Self {
        Self {
            uid: format!("{}@jai.stark", uuid::Uuid::new_v4()),
            title: title.trim().to_string(),
            start,
            end,
            all_day,
            location: String::new(),
            description: String::new(),
            source: "local".into(),
            href: String::new(),
            etag: String::new(),
            recurring: false,
            pending: String::new(),
        }
    }

    pub fn overlaps(&self, other: &CalEvent) -> bool {
        !self.all_day && !other.all_day && self.start < other.end && other.start < self.end
    }

    /// "09:00–10:30 Dentist (Main Street)" / "all day: Mum's birthday".
    pub fn line(&self) -> String {
        let place = if self.location.trim().is_empty() { String::new() } else { format!(" ({})", self.location.trim()) };
        if self.all_day {
            format!("all day: {}{place}", self.title)
        } else {
            format!("{}–{} {}{place}", self.start.format("%H:%M"), self.end.format("%H:%M"), self.title)
        }
    }
}

/// A discovered CalDAV calendar.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct DavCalendar {
    href: String,
    name: String,
    /// The account may write to it (`current-user-privilege-set`); true when the server is silent.
    #[serde(default = "yes")]
    writable: bool,
}

fn yes() -> bool {
    true
}

struct DavResponse {
    status: u16,
    etag: String,
    body: String,
    url: String,
}

struct Inner {
    conn: Mutex<Connection>,
    /// No automatic redirects: CalDAV methods and credentials must survive a hop to another host.
    http: reqwest::Client,
    /// (server, user) → calendars.
    calendars: tokio::sync::Mutex<Option<(String, Vec<DavCalendar>)>>,
    last_sync: Mutex<Option<(Instant, NaiveDate, NaiveDate)>>,
    writes: tokio::sync::Mutex<()>,
}

#[derive(Clone)]
pub struct CalendarStore {
    inner: Arc<Inner>,
}

fn stamp(t: NaiveDateTime) -> String {
    t.format(STAMP).to_string()
}

fn unstamp(s: &str) -> NaiveDateTime {
    NaiveDateTime::parse_from_str(s, STAMP).unwrap_or_default()
}

/// CalDAV preserves the verb and credentials only across trusted redirect targets.
fn safe_dav_redirect(from: &str, to: &str) -> bool {
    let (Ok(from), Ok(to)) = (reqwest::Url::parse(from), reqwest::Url::parse(to)) else { return false };
    if !matches!(to.scheme(), "http" | "https") || (from.scheme() == "https" && to.scheme() != "https") {
        return false;
    }
    let (Some(old), Some(new)) = (from.host_str(), to.host_str()) else { return false };
    old.eq_ignore_ascii_case(new)
        || (to.scheme() == "https" && (old == "icloud.com" || old.ends_with(".icloud.com")) && new.ends_with(".icloud.com"))
}

fn writable_properties(xml: &str) -> bool {
    let privileges = xml_first(xml, "current-user-privilege-set").unwrap_or_default();
    privileges.is_empty() || has_tag(&privileges, "write") || has_tag(&privileges, "all") || has_tag(&privileges, "bind")
}

impl CalendarStore {
    pub fn open<P: AsRef<Path>>(db_path: P) -> Result<Self, String> {
        Self::with(Connection::open(db_path.as_ref()).map_err(|e| e.to_string())?)
    }

    pub fn in_memory() -> Self {
        Self::with(Connection::open_in_memory().expect("in-memory SQLite")).expect("in-memory calendar")
    }

    fn with(conn: Connection) -> Result<Self, String> {
        conn.busy_timeout(Duration::from_secs(3)).map_err(|e| e.to_string())?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS calendar_events (
                 uid TEXT NOT NULL,
                 start TEXT NOT NULL,
                 end_time TEXT NOT NULL,
                 all_day INTEGER NOT NULL DEFAULT 0,
                 title TEXT NOT NULL,
                 location TEXT NOT NULL DEFAULT '',
                 description TEXT NOT NULL DEFAULT '',
                 source TEXT NOT NULL,
                 href TEXT NOT NULL DEFAULT '',
                 etag TEXT NOT NULL DEFAULT '',
                 recurring INTEGER NOT NULL DEFAULT 0,
                 pending TEXT NOT NULL DEFAULT '',
                 updated_at TEXT NOT NULL,
                 PRIMARY KEY (uid, start)
             );
             CREATE INDEX IF NOT EXISTS calendar_events_start ON calendar_events(start);
             CREATE TABLE IF NOT EXISTS caldav_discovery (
                 key TEXT PRIMARY KEY,
                 calendars TEXT NOT NULL,
                 updated_at TEXT NOT NULL
             );",
        )
        .map_err(|e| e.to_string())?;
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(25))
            .connect_timeout(Duration::from_secs(8))
            .user_agent("JaiCalDAV/1.0")
            .build()
            .unwrap_or_default();
        Ok(Self {
            inner: Arc::new(Inner { conn: Mutex::new(conn), http, calendars: tokio::sync::Mutex::new(None), last_sync: Mutex::new(None), writes: tokio::sync::Mutex::new(()) }),
        })
    }

    fn conn(&self) -> MutexGuard<'_, Connection> {
        self.inner.conn.lock().unwrap_or_else(|e| e.into_inner())
    }

    // ------------------------------------------------------------------- local store ----

    fn row(r: &rusqlite::Row<'_>) -> rusqlite::Result<CalEvent> {
        Ok(CalEvent {
            uid: r.get(0)?,
            start: unstamp(&r.get::<_, String>(1)?),
            end: unstamp(&r.get::<_, String>(2)?),
            all_day: r.get::<_, i64>(3)? != 0,
            title: r.get(4)?,
            location: r.get(5)?,
            description: r.get(6)?,
            source: r.get(7)?,
            href: r.get(8)?,
            etag: r.get(9)?,
            recurring: r.get::<_, i64>(10)? != 0,
            pending: r.get(11)?,
        })
    }

    const COLUMNS: &'static str = "uid, start, end_time, all_day, title, location, description, source, href, etag, recurring, pending";

    /// Events overlapping [from, to), chronologically (pending deletions left out).
    pub fn events_between(&self, from: NaiveDateTime, to: NaiveDateTime) -> Vec<CalEvent> {
        let conn = self.conn();
        let Ok(mut stmt) = conn.prepare(&format!(
            "SELECT {} FROM calendar_events WHERE start < ?2 AND end_time > ?1 AND pending != 'delete' ORDER BY all_day DESC, start ASC",
            Self::COLUMNS
        )) else {
            return Vec::new();
        };
        let Ok(rows) = stmt.query_map(params![stamp(from), stamp(to)], Self::row) else { return Vec::new() };
        rows.flatten().collect()
    }

    fn upsert(&self, conn: &Connection, ev: &CalEvent) -> rusqlite::Result<usize> {
        conn.execute(
            "INSERT INTO calendar_events (uid, start, end_time, all_day, title, location, description, source, href, etag, recurring, pending, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
             ON CONFLICT(uid, start) DO UPDATE SET end_time = excluded.end_time, all_day = excluded.all_day, title = excluded.title,
                 location = excluded.location, description = excluded.description, source = excluded.source, href = excluded.href,
                 etag = excluded.etag, recurring = excluded.recurring, pending = excluded.pending, updated_at = excluded.updated_at",
            params![
                ev.uid,
                stamp(ev.start),
                stamp(ev.end),
                ev.all_day as i64,
                ev.title,
                ev.location,
                ev.description,
                ev.source,
                ev.href,
                ev.etag,
                ev.recurring as i64,
                ev.pending,
                chrono::Local::now().to_rfc3339()
            ],
        )
    }

    pub fn save_local(&self, ev: &CalEvent) -> Result<(), String> {
        let conn = self.conn();
        self.upsert(&conn, ev).map(|_| ()).map_err(|e| e.to_string())
    }

    fn remove_local(&self, uid: &str) {
        let _ = self.conn().execute("DELETE FROM calendar_events WHERE uid = ?1", params![uid]);
    }

    fn mark_pending(&self, uid: &str, pending: &str) {
        let _ = self.conn().execute("UPDATE calendar_events SET pending = ?2 WHERE uid = ?1", params![uid, pending]);
    }

    /// Replaces one source's events inside the window (pending local edits are kept).
    fn replace_window(&self, source: &str, from: NaiveDateTime, to: NaiveDateTime, events: &[CalEvent]) {
        let mut conn = self.conn();
        let keep: HashSet<String> = {
            let Ok(mut stmt) = conn.prepare("SELECT uid FROM calendar_events WHERE pending != ''") else { return };
            stmt.query_map([], |r| r.get::<_, String>(0)).map(|rows| rows.flatten().collect()).unwrap_or_default()
        };
        let Ok(tx) = conn.transaction() else {
            warn!("[CAL] Cannot begin calendar refresh transaction");
            return;
        };
        if let Err(e) = tx.execute(
            "DELETE FROM calendar_events WHERE source = ?1 AND start < ?3 AND end_time > ?2 AND pending = ''",
            params![source, stamp(from), stamp(to)],
        ) {
            warn!("[CAL] Could not refresh the {source} window: {e}");
            return;
        }
        for ev in events.iter().filter(|e| !keep.contains(&e.uid)) {
            if let Err(e) = self.upsert(&tx, ev) {
                warn!("[CAL] Could not store '{}': {e}", ev.title);
                return;
            }
        }
        if let Err(e) = tx.commit() { warn!("[CAL] Could not commit refreshed calendar: {e}"); }
    }

    /// Whether this exact event already exists (deadline sync).
    pub fn has_uid(&self, uid: &str) -> bool {
        self.conn().query_row("SELECT 1 FROM calendar_events WHERE uid = ?1 LIMIT 1", params![uid], |_| Ok(())).is_ok()
    }

    /// Events matching a uid or title, upcoming first.
    pub fn find(&self, query: &str) -> Vec<CalEvent> {
        let q = query.trim().to_lowercase();
        if q.is_empty() {
            return Vec::new();
        }
        let now = Local::now().naive_local();
        let all = self.events_between(now - ChronoDuration::days(30), now + ChronoDuration::days(400));
        if let Some(exact) = all.iter().find(|e| e.uid.to_lowercase() == q) {
            return vec![exact.clone()];
        }
        let words: Vec<&str> = q.split_whitespace().filter(|w| w.len() > 2).collect();
        let mut hits: Vec<(usize, CalEvent)> = all
            .into_iter()
            .filter_map(|e| {
                let t = e.title.to_lowercase();
                let score = if t == q {
                    100
                } else if t.contains(&q) || q.contains(&t) {
                    50
                } else {
                    words.iter().filter(|w| t.contains(**w)).count() * 10
                };
                (score > 0).then_some((score, e))
            })
            .collect();
        // Best match first; among equals the next upcoming one.
        hits.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| (a.1.end < now).cmp(&(b.1.end < now))).then_with(|| a.1.start.cmp(&b.1.start)));
        hits.into_iter().map(|(_, e)| e).collect()
    }

    // --------------------------------------------------------------------- CalDAV ----

    fn caldav_ready(cfg: &CalendarSettings, token: Option<&str>) -> bool {
        !cfg.caldav_url.trim().is_empty() && !cfg.username.trim().is_empty() && token.is_some_and(|t| !t.is_empty())
    }

    #[allow(clippy::too_many_arguments)]
    async fn dav(&self, method: &str, url: &str, cfg: &CalendarSettings, token: &str, depth: Option<&str>, body: Option<&str>, extra: &[(&str, String)]) -> Result<DavResponse, String> {
        if !safe_dav_redirect(&normalize_url(&cfg.caldav_url), url) {
            return Err("the discovered calendar URL is outside the configured account's trusted hosts".into());
        }
        let mut url = url.to_string();
        let m = reqwest::Method::from_bytes(method.as_bytes()).map_err(|e| e.to_string())?;
        for _ in 0..5 {
            let mut req = self.inner.http.request(m.clone(), &url).basic_auth(cfg.username.trim(), Some(token));
            if let Some(d) = depth {
                req = req.header("Depth", d);
            }
            if let Some(b) = body {
                let ct = if method == "PUT" { "text/calendar; charset=utf-8" } else { "application/xml; charset=utf-8" };
                req = req.header(reqwest::header::CONTENT_TYPE, ct).body(b.to_string());
            }
            for (k, v) in extra {
                req = req.header(*k, v.as_str());
            }
            let resp = req.send().await.map_err(err_chain)?;
            let status = resp.status();
            if status.is_redirection() {
                if let Some(next) = resp.headers().get(reqwest::header::LOCATION).and_then(|v| v.to_str().ok()) {
                    let next = resolve(&url, next);
                    if !safe_dav_redirect(&url, &next) {
                        return Err("the calendar redirected to an untrusted host or downgraded HTTPS; credentials were not forwarded".into());
                    }
                    url = next;
                    continue;
                }
            }
            if status.as_u16() == 401 {
                return Err("the calendar server rejected the username or app-specific password (HTTP 401)".into());
            }
            let etag = resp.headers().get(reqwest::header::ETAG).and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
            let body = resp.text().await.map_err(err_chain)?;
            return Ok(DavResponse { status: status.as_u16(), etag, body, url });
        }
        Err("the calendar server redirected too often".into())
    }

    /// The discovery cached in SQLite (a day old at most).
    fn cached_discovery(&self, key: &str) -> Option<Vec<DavCalendar>> {
        let (json, at): (String, String) = self
            .conn()
            .query_row("SELECT calendars, updated_at FROM caldav_discovery WHERE key = ?1", params![key], |r| Ok((r.get(0)?, r.get(1)?)))
            .ok()?;
        let fresh = chrono::DateTime::parse_from_rfc3339(&at).is_ok_and(|t| Local::now().signed_duration_since(t) < ChronoDuration::hours(24));
        let list: Vec<DavCalendar> = serde_json::from_str(&json).ok()?;
        (fresh && !list.is_empty()).then_some(list)
    }

    fn store_discovery(&self, key: &str, list: &[DavCalendar]) {
        let json = serde_json::to_string(list).unwrap_or_default();
        let _ = self.conn().execute(
            "INSERT INTO caldav_discovery (key, calendars, updated_at) VALUES (?1, ?2, ?3) ON CONFLICT(key) DO UPDATE SET calendars = ?2, updated_at = ?3",
            params![key, json, Local::now().to_rfc3339()],
        );
    }

    fn forget_discovery(&self) {
        let _ = self.conn().execute("DELETE FROM caldav_discovery", []);
    }

    /// The server's VEVENT calendars (cached per server and user, in memory and in SQLite).
    async fn calendars(&self, cfg: &CalendarSettings, token: &str) -> Result<Vec<DavCalendar>, String> {
        let key = format!("{}|{}", cfg.caldav_url.trim(), cfg.username.trim());
        let mut cache = self.inner.calendars.lock().await;
        if let Some((k, list)) = cache.as_ref() {
            if *k == key && !list.is_empty() {
                return Ok(list.clone());
            }
        }
        if let Some(list) = self.cached_discovery(&key) {
            *cache = Some((key, list.clone()));
            return Ok(list);
        }
        let base = normalize_url(&cfg.caldav_url);
        let probe = self
            .dav(
                "PROPFIND",
                &base,
                cfg,
                token,
                Some("0"),
                Some(r#"<?xml version="1.0" encoding="utf-8"?><d:propfind xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav"><d:prop><d:resourcetype/><d:displayname/><d:current-user-principal/><d:current-user-privilege-set/><c:calendar-home-set/></d:prop></d:propfind>"#),
                &[],
            )
            .await?;
        if probe.status >= 400 {
            return Err(format!("the calendar server answered HTTP {} to the discovery request", probe.status));
        }
        // The URL is a calendar itself.
        if xml_first(&probe.body, "resourcetype").is_some_and(|r| has_tag(&r, "calendar")) {
            let name = xml_first(&probe.body, "displayname").unwrap_or_else(|| "Calendar".into());
            let list = vec![DavCalendar { href: probe.url.clone(), name, writable: writable_properties(&probe.body) }];
            self.store_discovery(&key, &list);
            *cache = Some((key, list.clone()));
            return Ok(list);
        }
        let mut home_base = probe.url.clone();
        let mut home = xml_first(&probe.body, "calendar-home-set").and_then(|b| xml_first(&b, "href"));
        if home.is_none() {
            let principal = xml_first(&probe.body, "current-user-principal")
                .and_then(|b| xml_first(&b, "href"))
                .ok_or("the server reported no calendar account (current-user-principal)")?;
            let principal = resolve(&probe.url, &principal);
            let r = self
                .dav(
                    "PROPFIND",
                    &principal,
                    cfg,
                    token,
                    Some("0"),
                    Some(r#"<?xml version="1.0" encoding="utf-8"?><d:propfind xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav"><d:prop><c:calendar-home-set/></d:prop></d:propfind>"#),
                    &[],
                )
                .await?;
            if r.status >= 400 {
                return Err(format!("calendar principal discovery failed (HTTP {})", r.status));
            }
            home = xml_first(&r.body, "calendar-home-set").and_then(|b| xml_first(&b, "href"));
            home_base = r.url;
        }
        let home = resolve(&home_base, &home.ok_or("the server reported no calendar home")?);
        let r = self
            .dav(
                "PROPFIND",
                &home,
                cfg,
                token,
                Some("1"),
                Some(r#"<?xml version="1.0" encoding="utf-8"?><d:propfind xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav"><d:prop><d:resourcetype/><d:displayname/><c:supported-calendar-component-set/><d:current-user-privilege-set/></d:prop></d:propfind>"#),
                &[],
            )
            .await?;
        if r.status >= 400 {
            return Err(format!("calendar collection discovery failed (HTTP {})", r.status));
        }
        let mut list = Vec::new();
        for response in xml_all(&r.body, "response") {
            let Some(href) = xml_first(&response, "href") else { continue };
            let is_calendar = xml_first(&response, "resourcetype").is_some_and(|t| has_tag(&t, "calendar"));
            let comps = xml_first(&response, "supported-calendar-component-set").unwrap_or_default();
            if !is_calendar || (!comps.is_empty() && !comps.to_uppercase().contains("VEVENT")) {
                continue;
            }
            let name = xml_first(&response, "displayname").filter(|n| !n.trim().is_empty()).unwrap_or_else(|| "Calendar".into());
            // Shared read-only calendars list their privileges without write.
            let writable = writable_properties(&response);
            list.push(DavCalendar { href: resolve(&r.url, &href), name, writable });
        }
        if list.is_empty() {
            return Err("the account has no event calendars".into());
        }
        info!(
            "[CAL] CalDAV calendars: {}",
            list.iter().map(|c| if c.writable { c.name.clone() } else { format!("{} (read-only)", c.name) }).collect::<Vec<_>>().join(", ")
        );
        self.store_discovery(&key, &list);
        *cache = Some((key, list.clone()));
        Ok(list)
    }

    async fn report(&self, cal: &DavCalendar, cfg: &CalendarSettings, token: &str, from: NaiveDateTime, to: NaiveDateTime) -> Result<Vec<CalEvent>, String> {
        let body = format!(
            r#"<?xml version="1.0" encoding="utf-8"?><c:calendar-query xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav"><d:prop><d:getetag/><c:calendar-data/></d:prop><c:filter><c:comp-filter name="VCALENDAR"><c:comp-filter name="VEVENT"><c:time-range start="{}" end="{}"/></c:comp-filter></c:comp-filter></c:filter></c:calendar-query>"#,
            ics_utc(from),
            ics_utc(to)
        );
        let r = self.dav("REPORT", &cal.href, cfg, token, Some("1"), Some(&body), &[]).await?;
        if r.status >= 400 {
            return Err(format!("'{}' answered HTTP {}", cal.name, r.status));
        }
        let mut out = Vec::new();
        for response in xml_all(&r.body, "response") {
            let Some(data) = xml_first(&response, "calendar-data") else { continue };
            let href = xml_first(&response, "href").map(|h| resolve(&r.url, &h)).unwrap_or_default();
            let etag = xml_first(&response, "getetag").unwrap_or_default();
            for mut ev in expand(&parse_events(&data), from, to) {
                ev.source = "caldav".into();
                ev.href = href.clone();
                ev.etag = etag.clone();
                out.push(ev);
            }
        }
        Ok(out)
    }

    /// The calendar new events go to: the configured one, else the first writable event
    /// calendar (iCloud's default "Calendar" / "Kalender" / "Home" preferred).
    async fn target(&self, cfg: &CalendarSettings, token: &str) -> Result<DavCalendar, String> {
        let list = self.calendars(cfg, token).await?;
        let wanted = cfg.calendar_name.trim().to_lowercase();
        let writable: Vec<&DavCalendar> = list.iter().filter(|c| c.writable).collect();
        let pool = writable;
        if !wanted.is_empty() && !pool.iter().any(|c| c.name.to_lowercase().contains(&wanted)) {
            return Err(format!("the configured calendar '{}' was not found among writable collections", cfg.calendar_name.trim()));
        }
        let skip = ["reminders", "erinnerungen", "tasks", "aufgaben", "ferien", "holidays", "feiertage", "birthdays", "geburtstage"];
        Ok(pool
            .iter()
            .find(|c| !wanted.is_empty() && c.name.to_lowercase().contains(&wanted))
            .or_else(|| pool.iter().find(|c| ["calendar", "kalender", "home", "privat", "personal"].contains(&c.name.to_lowercase().as_str())))
            .or_else(|| pool.iter().find(|c| !skip.contains(&c.name.to_lowercase().as_str())))
            .or_else(|| pool.first())
            .map(|c| (*c).clone())
            .ok_or("the account has no writable calendar")?)
    }

    async fn put_event(&self, cfg: &CalendarSettings, token: &str, ev: &mut CalEvent) -> Result<(), String> {
        for attempt in 0..2 {
        let cal = self.target(cfg, token).await?;
        let file: String = ev.uid.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '_' }).collect();
        let href = if ev.href.is_empty() { resolve(&format!("{}/", cal.href.trim_end_matches('/')), &format!("{file}.ics")) } else { ev.href.clone() };
        let r = self.dav("PUT", &href, cfg, token, None, Some(&build_vcalendar(ev)), &[("If-None-Match", "*".to_string())]).await?;
        if matches!(r.status, 403 | 404 | 409) {
            // A stale or read-only collection: discover again next time.
            *self.inner.calendars.lock().await = None;
            self.forget_discovery();
            if attempt == 0 && ev.href.is_empty() {
                warn!("[CAL] PUT HTTP {}; rediscovering writable calendar collections", r.status);
                continue;
            }
        }
        if !(200..300).contains(&r.status) && r.status != 412 {
            warn!("[CAL] New event PUT rejected: HTTP {}, calendar {}", r.status, cal.name);
            return Err(format!("'{}' refused the new event (HTTP {})", cal.name, r.status));
        }
        // Read it back: the event really is on the server.
        let check = self.dav("GET", &r.url, cfg, token, None, None, &[]).await;
        let verified = check.as_ref().is_ok_and(|c| (200..300).contains(&c.status) && parse_events(&c.body).iter().any(|e| e.uid == ev.uid));
        if r.status == 412 && !verified {
            return Err("the resource exists but could not be verified (HTTP 412); the local event remains pending".into());
        }
        ev.href = r.url;
        ev.etag = match check {
            Ok(c) if !c.etag.is_empty() => c.etag,
            _ => r.etag,
        };
        ev.source = "caldav".into();
        ev.pending.clear();
        info!("[CAL] Created '{}' in {} (HTTP {}{})", ev.title, cal.name, r.status, if verified { ", verified" } else { ", not read back" });
        return Ok(());
        }
        Err("calendar discovery did not yield a writable collection".into())
    }

    /// Pushes local events created or deleted while offline.
    async fn push_pending(&self, cfg: &CalendarSettings, token: &str) -> usize {
        let _writer = self.inner.writes.lock().await;
        let pending: Vec<CalEvent> = {
            let conn = self.conn();
            let Ok(mut stmt) = conn.prepare(&format!("SELECT {} FROM calendar_events WHERE pending != ''", Self::COLUMNS)) else { return 0 };
            stmt.query_map([], Self::row).map(|rows| rows.flatten().collect()).unwrap_or_default()
        };
        let mut pushed = 0;
        for mut ev in pending {
            match ev.pending.as_str() {
                "put" => match self.put_event(cfg, token, &mut ev).await {
                    Ok(()) => {
                        let _ = self.save_local(&ev);
                        pushed += 1;
                    }
                    Err(e) => warn!("[CAL] '{}' is still waiting for the server: {e}", ev.title),
                },
                "delete" if !ev.href.is_empty() => match self.dav("DELETE", &ev.href, cfg, token, None, None, &[]).await {
                    Ok(r) if r.status < 300 || r.status == 404 => {
                        self.remove_local(&ev.uid);
                        pushed += 1;
                    }
                    Ok(r) => warn!("[CAL] Deleting '{}' failed (HTTP {})", ev.title, r.status),
                    Err(e) => warn!("[CAL] Deleting '{}' failed: {e}", ev.title),
                },
                _ => self.remove_local(&ev.uid),
            }
        }
        pushed
    }

    // ----------------------------------------------------------------------- sync ----

    /// Refreshes the local mirror for [from, to] (inclusive days). Returns what was read.
    pub async fn sync(&self, cfg: &CalendarSettings, token: Option<&str>, from: NaiveDate, to: NaiveDate, force: bool) -> Result<String, String> {
        if !force {
            let last = *self.inner.last_sync.lock().unwrap_or_else(|e| e.into_inner());
            if let Some((at, f, t)) = last {
                if at.elapsed() < Duration::from_secs(90) && f <= from && t >= to {
                    return Ok("Calendar is up to date.".into());
                }
            }
        }
        let start = from.and_time(NaiveTime::MIN);
        let end = (to + ChronoDuration::days(1)).and_time(NaiveTime::MIN);
        let mut parts = Vec::new();
        let mut errors = Vec::new();
        if let Some(token) = token.filter(|_| Self::caldav_ready(cfg, token)) {
            let pushed = self.push_pending(cfg, token).await;
            if pushed > 0 {
                parts.push(format!("pushed {pushed} offline change(s)"));
            }
            match self.calendars(cfg, token).await {
                Ok(list) => {
                    let mut events = Vec::new();
                    let mut complete = true;
                    for cal in &list {
                        match self.report(cal, cfg, token, start, end).await {
                            Ok(mut evs) => events.append(&mut evs),
                            Err(e) => {
                                complete = false;
                                errors.push(e);
                            }
                        }
                    }
                    if complete {
                        self.replace_window("caldav", start, end, &events);
                    } else {
                        // Partial failure must never remove another collection's offline cache.
                        for event in &events {
                            let pending: bool = self.conn().query_row("SELECT EXISTS(SELECT 1 FROM calendar_events WHERE uid = ?1 AND pending != '')", params![event.uid], |r| r.get(0)).unwrap_or(true);
                            if !pending { self.save_local(event)?; }
                        }
                    }
                    parts.push(format!("{} event(s) from {} calendar(s)", events.len(), list.len()));
                }
                Err(e) => {
                    // A wrong account must be discovered again next time.
                    *self.inner.calendars.lock().await = None;
                    self.forget_discovery();
                    errors.push(format!("CalDAV: {e}"));
                }
            }
        }
        if !cfg.ics_url.trim().is_empty() {
            match self.fetch_feed(&cfg.ics_url, start, end).await {
                Ok(events) => {
                    parts.push(format!("{} event(s) from the iCal feed", events.len()));
                    self.replace_window("ics", start, end, &events);
                }
                Err(e) => errors.push(format!("iCal feed: {e}")),
            }
        }
        if errors.is_empty() || !parts.is_empty() {
            *self.inner.last_sync.lock().unwrap_or_else(|e| e.into_inner()) = Some((Instant::now(), from, to));
        }
        match (parts.is_empty(), errors.is_empty()) {
            (_, true) if parts.is_empty() => Ok("Offline calendar only (no CalDAV account or feed configured).".into()),
            (_, true) => Ok(format!("Synced: {}.", parts.join("; "))),
            (true, false) => Err(errors.join("; ")),
            (false, false) => Ok(format!("Synced: {} (but {}).", parts.join("; "), errors.join("; "))),
        }
    }

    async fn fetch_feed(&self, url: &str, from: NaiveDateTime, to: NaiveDateTime) -> Result<Vec<CalEvent>, String> {
        let resp = crate::websearch::HTTP.get(normalize_url(url)).timeout(Duration::from_secs(20)).send().await.map_err(err_chain)?;
        if !resp.status().is_success() {
            return Err(format!("HTTP {}", resp.status().as_u16()));
        }
        let text = resp.text().await.map_err(err_chain)?;
        if !text.contains("BEGIN:VCALENDAR") {
            return Err("the URL is not an iCalendar feed".into());
        }
        Ok(expand(&parse_events(&text), from, to)
            .into_iter()
            .map(|mut e| {
                e.source = "ics".into();
                e
            })
            .collect())
    }

    /// A connection check for the Integrations hub.
    pub async fn test(&self, cfg: &CalendarSettings, token: Option<&str>) -> Result<String, String> {
        *self.inner.calendars.lock().await = None;
        self.forget_discovery();
        let mut out = Vec::new();
        if let Some(token) = token.filter(|_| Self::caldav_ready(cfg, token)) {
            let list = self.calendars(cfg, token).await?;
            let target = self.target(cfg, token).await.map(|c| c.name).unwrap_or_else(|_| "none writable".into());
            out.push(format!("CalDAV OK: {} — new events go to '{target}'", list.iter().map(|c| c.name.as_str()).collect::<Vec<_>>().join(", ")));
        }
        if !cfg.ics_url.trim().is_empty() {
            let now = Local::now().naive_local();
            let n = self.fetch_feed(&cfg.ics_url, now, now + ChronoDuration::days(30)).await?.len();
            out.push(format!("iCal feed OK ({n} events in the next 30 days)"));
        }
        if out.is_empty() {
            return Err("Add a CalDAV server with username and app-specific password, or an .ics feed URL.".into());
        }
        Ok(out.join("; "))
    }

    // ------------------------------------------------------------------- changes ----

    /// Persist before returning; upload independently so network timeouts cannot undo a save.
    pub async fn create(&self, cfg: &CalendarSettings, token: Option<&str>, mut ev: CalEvent) -> Result<String, String> {
        if ev.title.trim().is_empty() || ev.title.len() > 4096 {
            return Err("An event needs a title between 1 and 4096 bytes.".into());
        }
        if ev.end <= ev.start {
            return Err("The event end must be after its start.".into());
        }
        if !ev.all_day && (Local.from_local_datetime(&ev.start).single().is_none() || Local.from_local_datetime(&ev.end).single().is_none()) {
            return Err("That local time is ambiguous or does not exist because of daylight saving time; choose an unambiguous time.".into());
        }
        if ev.all_day {
            ev.start = ev.start.date().and_time(NaiveTime::MIN);
            ev.end = ev.end.date().and_time(NaiveTime::MIN);
            if ev.end <= ev.start { ev.end = ev.start + ChronoDuration::days(1); }
        }
        ev.pending = "put".into();
        self.save_local(&ev)?;
        let when = describe_when(&ev);
        let Some(token) = token.filter(|_| Self::caldav_ready(cfg, token)).map(str::to_owned) else {
            return Ok(format!("Added '{}' {when} to the local calendar. iCloud sync is pending until a CalDAV account and password are configured.", ev.title));
        };
        let store = self.clone();
        let cfg = cfg.clone();
        let title = ev.title.clone();
        tokio::spawn(async move {
            let _writer = store.inner.writes.lock().await;
            let pending: bool = store.conn().query_row("SELECT EXISTS(SELECT 1 FROM calendar_events WHERE uid = ?1 AND pending = 'put')", params![ev.uid], |r| r.get(0)).unwrap_or(false);
            if !pending { return; }
            match store.put_event(&cfg, &token, &mut ev).await {
                Ok(()) => {
                    if let Err(e) = store.save_local(&ev) {
                        warn!("[CAL] Remote event saved, but local sync status could not be recorded: {e}");
                    }
                }
                Err(e) => warn!("[CAL] '{}' is safely stored locally; sync pending: {e}", ev.title),
            }
        });
        Ok(format!("Added '{title}' {when} to your local calendar. iCloud synchronization is running in the background."))
    }

    pub async fn delete(&self, cfg: &CalendarSettings, token: Option<&str>, ev: &CalEvent) -> Result<String, String> {
        let series = if ev.recurring { " (the whole recurring series)" } else { "" };
        if ev.source == "ics" {
            return Err(format!("'{}' comes from a read-only iCal feed; delete it in the calendar app itself.", ev.title));
        }
        if ev.href.is_empty() {
            self.remove_local(&ev.uid);
            return Ok(format!("Deleted '{}'{series} from the calendar.", ev.title));
        }
        let Some(token) = token.filter(|_| Self::caldav_ready(cfg, token)) else {
            self.mark_pending(&ev.uid, "delete");
            return Ok(format!("Removed '{}'{series}; it is deleted on the server at the next sync.", ev.title));
        };
        let mut extra = Vec::new();
        if !ev.etag.is_empty() {
            extra.push(("If-Match", ev.etag.clone()));
        }
        let r = self.dav("DELETE", &ev.href, cfg, token, None, None, &extra).await?;
        if r.status == 412 {
            return Err(format!("'{}' was changed elsewhere in the meantime; ask me again after the next sync.", ev.title));
        }
        if r.status >= 300 && r.status != 404 {
            return Err(format!("The server refused to delete '{}' (HTTP {}).", ev.title, r.status));
        }
        self.remove_local(&ev.uid);
        Ok(format!("Deleted '{}'{series} from Sir's calendar.", ev.title))
    }

    /// Moves an event (keeping everything else of the server's resource) and/or renames it.
    pub async fn reschedule(&self, cfg: &CalendarSettings, token: Option<&str>, ev: &CalEvent, start: NaiveDateTime, end: NaiveDateTime, title: Option<&str>) -> Result<String, String> {
        if ev.source == "ics" {
            return Err(format!("'{}' comes from a read-only iCal feed; change it in the calendar app itself.", ev.title));
        }
        if ev.recurring {
            return Err(format!("'{}' is a recurring series; I can only delete the whole series, not move one instance.", ev.title));
        }
        let mut next = ev.clone();
        next.start = start;
        next.end = end;
        next.all_day = false;
        if let Some(t) = title.filter(|t| !t.trim().is_empty()) {
            next.title = t.trim().to_string();
        }
        let when = describe_when(&next);
        let remote = token.filter(|_| Self::caldav_ready(cfg, token) && !ev.href.is_empty());
        if let Some(token) = remote {
            let current = self.dav("GET", &ev.href, cfg, token, None, None, &[]).await?;
            if current.status >= 300 {
                return Err(format!("The server no longer has '{}' (HTTP {}).", ev.title, current.status));
            }
            let body = rewrite_event(&current.body, &next).ok_or_else(|| format!("'{}' could not be edited (unexpected iCalendar data).", ev.title))?;
            let etag = if current.etag.is_empty() { ev.etag.clone() } else { current.etag.clone() };
            let mut extra = Vec::new();
            if !etag.is_empty() {
                extra.push(("If-Match", etag));
            }
            let r = self.dav("PUT", &ev.href, cfg, token, None, Some(&body), &extra).await?;
            if !(200..300).contains(&r.status) {
                return Err(format!("The server refused the change to '{}' (HTTP {}).", ev.title, r.status));
            }
            next.etag = r.etag;
        } else if !ev.href.is_empty() {
            return Err(format!("'{}' lives on the CalDAV server, which is not reachable without its app-specific password.", ev.title));
        }
        self.remove_local(&ev.uid);
        self.save_local(&next)?;
        Ok(format!("Moved '{}' to {when}.", next.title))
    }
}

/// "on Thursday 22 October, 15:00–16:00".
pub fn describe_when(ev: &CalEvent) -> String {
    let today = Local::now().date_naive();
    let day = crate::pim::spoken_day(ev.start.date(), today);
    if ev.all_day {
        format!("{day} (all day)")
    } else {
        format!("{day}, {}–{}", ev.start.format("%H:%M"), ev.end.format("%H:%M"))
    }
}

/// What JAI says once an event is in: "Done, Sir. I have scheduled 'Test' for tomorrow at
/// 15:00." (24-hour time, as Sir writes it; all-day events: "for Saturday, all day").
pub fn confirmation(ev: &CalEvent) -> String {
    let day = crate::pim::spoken_day(ev.start.date(), Local::now().date_naive());
    if ev.all_day {
        return format!("Done, Sir. I have scheduled '{}' for {day}, all day.", ev.title);
    }
    format!("Done, Sir. I have scheduled '{}' for {day} at {}.", ev.title, ev.start.format("%H:%M"))
}

/// The schedule of a range, grouped by day, with clashes marked.
pub fn schedule_text(events: &[CalEvent], from: NaiveDate, to: NaiveDate) -> String {
    let today = Local::now().date_naive();
    let mut out = Vec::new();
    let mut day = from;
    while day <= to {
        let start = day.and_time(NaiveTime::MIN);
        let end = start + ChronoDuration::days(1);
        let todays: Vec<&CalEvent> = events.iter().filter(|e| e.start < end && e.end > start).collect();
        if !todays.is_empty() {
            out.push(format!("{}:", crate::pim::capitalise(&crate::pim::spoken_day(day, today))));
            for e in &todays {
                let clash: Vec<&str> = todays.iter().filter(|o| o.uid != e.uid && o.overlaps(e)).map(|o| o.title.as_str()).collect();
                let mut line = format!("- {}", e.line());
                if !clash.is_empty() {
                    line.push_str(&format!(" [CONFLICT] with {}", clash.join(", ")));
                }
                out.push(line);
            }
        }
        day += ChronoDuration::days(1);
    }
    out.join("\n")
}

/// Pairs of overlapping timed events.
pub fn conflicts(events: &[CalEvent]) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for (i, a) in events.iter().enumerate() {
        for b in &events[i + 1..] {
            if a.uid != b.uid && a.overlaps(b) {
                out.push((a.title.clone(), b.title.clone()));
            }
        }
    }
    out
}

// ------------------------------------------------------------------------- XML helpers ----

fn tag_regex(local: &str) -> Regex {
    Regex::new(&format!(
        r"<(?:[A-Za-z][\w.-]*:)?{local}(?:\s[^>]*?)?(?:/>|>(?s:(.*?))</(?:[A-Za-z][\w.-]*:)?{local}\s*>)"
    ))
    .expect("valid XML tag regex")
}

/// The decoded content of every `<…:local>` element.
fn xml_all(xml: &str, local: &str) -> Vec<String> {
    tag_regex(local).captures_iter(xml).map(|c| c.get(1).map(|m| m.as_str().to_string()).unwrap_or_default()).collect()
}

/// The decoded text of the first `<…:local>` element (None when missing or empty).
fn xml_first(xml: &str, local: &str) -> Option<String> {
    tag_regex(local).captures(xml).and_then(|c| c.get(1)).map(|m| xml_unescape(m.as_str())).map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

/// An element is present (self-closing or not): `<cal:calendar/>` in a resourcetype.
fn has_tag(xml: &str, local: &str) -> bool {
    tag_regex(local).is_match(xml)
}

fn xml_unescape(s: &str) -> String {
    static CDATA: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?s)<!\[CDATA\[(.*?)\]\]>").expect("valid CDATA regex"));
    static NUMERIC: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"&#(x[0-9A-Fa-f]+|\d+);").expect("valid entity regex"));
    if let Some(c) = CDATA.captures(s) {
        return c[1].to_string();
    }
    let s = NUMERIC.replace_all(s, |c: &regex::Captures<'_>| {
        let raw = &c[1];
        let code = if let Some(hex) = raw.strip_prefix('x') { u32::from_str_radix(hex, 16).ok() } else { raw.parse().ok() };
        code.and_then(char::from_u32).map(String::from).unwrap_or_default()
    });
    s.replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&apos;", "'").replace("&amp;", "&")
}

// ----------------------------------------------------------------------- iCalendar ----

#[derive(Debug, Clone, Copy, PartialEq)]
struct IcsTime {
    at: NaiveDateTime,
    all_day: bool,
}

#[derive(Debug, Clone, Default)]
struct VEvent {
    uid: String,
    summary: String,
    location: String,
    description: String,
    start: Option<IcsTime>,
    end: Option<IcsTime>,
    duration: Option<ChronoDuration>,
    rrule: Option<String>,
    exdates: Vec<NaiveDateTime>,
    recurrence_id: Option<NaiveDateTime>,
    cancelled: bool,
}

/// Content lines with folded continuations joined.
fn unfold(ics: &str) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    for raw in ics.split('\n') {
        let line = raw.strip_suffix('\r').unwrap_or(raw);
        if let Some(rest) = line.strip_prefix(' ').or_else(|| line.strip_prefix('\t')) {
            if let Some(last) = lines.last_mut() {
                last.push_str(rest);
                continue;
            }
        }
        if !line.is_empty() {
            lines.push(line.to_string());
        }
    }
    lines
}

/// "DTSTART;TZID=Europe/Berlin:20261022T090000" → ("DTSTART", [("TZID", …)], "20261022T090000").
fn split_prop(line: &str) -> (String, Vec<(String, String)>, String) {
    let mut in_quotes = false;
    let mut colon = None;
    for (i, c) in line.char_indices() {
        match c {
            '"' => in_quotes = !in_quotes,
            ':' if !in_quotes => {
                colon = Some(i);
                break;
            }
            _ => {}
        }
    }
    let (head, value) = match colon {
        Some(i) => (&line[..i], &line[i + 1..]),
        None => (line, ""),
    };
    let mut parts = head.split(';');
    let name = parts.next().unwrap_or("").trim().to_uppercase();
    let params = parts
        .filter_map(|p| p.split_once('='))
        .map(|(k, v)| (k.trim().to_uppercase(), v.trim().trim_matches('"').to_string()))
        .collect();
    (name, params, value.to_string())
}

fn param<'a>(params: &'a [(String, String)], key: &str) -> Option<&'a str> {
    params.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
}

fn parse_ics_time(value: &str, params: &[(String, String)]) -> Option<IcsTime> {
    let v = value.trim();
    if param(params, "VALUE") == Some("DATE") || (v.len() == 8 && v.chars().all(|c| c.is_ascii_digit())) {
        let d = NaiveDate::parse_from_str(&v[..8.min(v.len())], "%Y%m%d").ok()?;
        return Some(IcsTime { at: d.and_time(NaiveTime::MIN), all_day: true });
    }
    let (body, utc) = match v.strip_suffix('Z') {
        Some(b) => (b, true),
        None => (v, false),
    };
    let t = NaiveDateTime::parse_from_str(body, "%Y%m%dT%H%M%S").or_else(|_| NaiveDateTime::parse_from_str(body, "%Y%m%dT%H%M")).ok()?;
    let at = if utc { Utc.from_utc_datetime(&t).with_timezone(&Local).naive_local() } else { t };
    Some(IcsTime { at, all_day: false })
}

/// "PT1H30M", "P1D", "-PT15M".
fn parse_duration(v: &str) -> Option<ChronoDuration> {
    static DUR: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"^([+-])?P(?:(\d+)W)?(?:(\d+)D)?(?:T(?:(\d+)H)?(?:(\d+)M)?(?:(\d+)S)?)?$").expect("valid duration regex")
    });
    let c = DUR.captures(v.trim())?;
    let n = |i: usize| c.get(i).and_then(|m| m.as_str().parse::<i64>().ok()).unwrap_or(0);
    let secs = n(2) * 604_800 + n(3) * 86_400 + n(4) * 3600 + n(5) * 60 + n(6);
    Some(ChronoDuration::seconds(if c.get(1).is_some_and(|m| m.as_str() == "-") { -secs } else { secs }))
}

fn unescape_text(v: &str) -> String {
    let mut out = String::with_capacity(v.len());
    let mut chars = v.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('n') | Some('N') => out.push('\n'),
                Some(other) => out.push(other),
                None => {}
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn parse_events(ics: &str) -> Vec<VEvent> {
    let mut events = Vec::new();
    let mut current: Option<VEvent> = None;
    // Components nested inside a VEVENT (VALARM) are skipped.
    let mut nested = 0usize;
    for line in unfold(ics) {
        let (name, params, value) = split_prop(&line);
        match (name.as_str(), value.trim().to_uppercase().as_str()) {
            ("BEGIN", "VEVENT") => {
                current = Some(VEvent::default());
                nested = 0;
                continue;
            }
            ("END", "VEVENT") => {
                if let Some(ev) = current.take() {
                    events.push(ev);
                }
                continue;
            }
            ("BEGIN", _) if current.is_some() => {
                nested += 1;
                continue;
            }
            ("END", _) if current.is_some() => {
                nested = nested.saturating_sub(1);
                continue;
            }
            _ => {}
        }
        let Some(ev) = current.as_mut().filter(|_| nested == 0) else { continue };
        match name.as_str() {
            "UID" => ev.uid = value.trim().to_string(),
            "SUMMARY" => ev.summary = unescape_text(&value),
            "LOCATION" => ev.location = unescape_text(&value),
            "DESCRIPTION" => ev.description = unescape_text(&value),
            "DTSTART" => ev.start = parse_ics_time(&value, &params),
            "DTEND" => ev.end = parse_ics_time(&value, &params),
            "DURATION" => ev.duration = parse_duration(&value),
            "RRULE" => ev.rrule = Some(value.trim().to_string()),
            "EXDATE" => ev.exdates.extend(value.split(',').filter_map(|v| parse_ics_time(v, &params)).map(|t| t.at)),
            "RECURRENCE-ID" => ev.recurrence_id = parse_ics_time(&value, &params).map(|t| t.at),
            "STATUS" => ev.cancelled = value.trim().eq_ignore_ascii_case("CANCELLED"),
            _ => {}
        }
    }
    events
}

fn weekday_of(code: &str) -> Option<Weekday> {
    Some(match code {
        "MO" => Weekday::Mon,
        "TU" => Weekday::Tue,
        "WE" => Weekday::Wed,
        "TH" => Weekday::Thu,
        "FR" => Weekday::Fri,
        "SA" => Weekday::Sat,
        "SU" => Weekday::Sun,
        _ => return None,
    })
}

/// The n-th (negative: from the end) weekday of a month.
fn nth_weekday(year: i32, month: u32, weekday: Weekday, n: i32) -> Option<NaiveDate> {
    if n > 0 {
        NaiveDate::from_weekday_of_month_opt(year, month, weekday, n as u8)
    } else {
        let first_next = if month == 12 { NaiveDate::from_ymd_opt(year + 1, 1, 1)? } else { NaiveDate::from_ymd_opt(year, month + 1, 1)? };
        let mut d = first_next - ChronoDuration::days(1);
        while d.weekday() != weekday {
            d -= ChronoDuration::days(1);
        }
        Some(d - ChronoDuration::weeks((-n - 1) as i64))
    }
}

/// Start times of a recurring event inside [from, to).
fn occurrences(start: NaiveDateTime, rrule: &str, from: NaiveDateTime, to: NaiveDateTime, exdates: &[NaiveDateTime]) -> Vec<NaiveDateTime> {
    let parts: HashMap<String, String> = rrule
        .split(';')
        .filter_map(|p| p.split_once('='))
        .map(|(k, v)| (k.trim().to_uppercase(), v.trim().to_uppercase()))
        .collect();
    let freq = parts.get("FREQ").map(String::as_str).unwrap_or("");
    let interval = parts.get("INTERVAL").and_then(|v| v.parse::<i64>().ok()).unwrap_or(1).max(1);
    let count = parts.get("COUNT").and_then(|v| v.parse::<usize>().ok());
    let until = parts.get("UNTIL").and_then(|v| parse_ics_time(v, &[])).map(|t| if t.all_day { t.at + ChronoDuration::days(1) } else { t.at + ChronoDuration::seconds(1) });
    let byday: Vec<(i32, Weekday)> = parts
        .get("BYDAY")
        .map(|v| {
            v.split(',')
                .filter_map(|d| {
                    let code = &d[d.len().saturating_sub(2)..];
                    let n = d[..d.len().saturating_sub(2)].parse::<i32>().unwrap_or(0);
                    weekday_of(code).map(|w| (n, w))
                })
                .collect()
        })
        .unwrap_or_default();
    let bymonthday: Vec<u32> = parts.get("BYMONTHDAY").map(|v| v.split(',').filter_map(|d| d.parse().ok()).collect()).unwrap_or_default();
    let time = start.time();
    let mut out = Vec::new();
    let mut seen = 0usize;
    let mut accept = |t: NaiveDateTime, out: &mut Vec<NaiveDateTime>| -> bool {
        // false = stop.
        if t < start {
            return true;
        }
        if until.is_some_and(|u| t >= u) || t >= to {
            return false;
        }
        seen += 1;
        if count.is_some_and(|c| seen > c) {
            return false;
        }
        let excluded = exdates.iter().any(|x| *x == t || (x.time() == NaiveTime::MIN && x.date() == t.date()));
        if t >= from - ChronoDuration::days(3) && !excluded {
            out.push(t);
        }
        true
    };
    let mut guard = 0;
    match freq {
        "DAILY" => {
            let mut t = start;
            while guard < 4000 && accept(t, &mut out) {
                t += ChronoDuration::days(interval);
                guard += 1;
            }
        }
        "WEEKLY" => {
            let mut days: Vec<Weekday> = byday.iter().map(|(_, w)| *w).collect();
            if days.is_empty() {
                days.push(start.weekday());
            }
            days.sort_by_key(|w| w.num_days_from_monday());
            let monday = start.date() - ChronoDuration::days(start.weekday().num_days_from_monday() as i64);
            'weeks: for week in 0.. {
                if guard > 2000 {
                    break;
                }
                for wd in &days {
                    guard += 1;
                    let date = monday + ChronoDuration::weeks(week * interval) + ChronoDuration::days(wd.num_days_from_monday() as i64);
                    if !accept(date.and_time(time), &mut out) {
                        break 'weeks;
                    }
                }
            }
        }
        "MONTHLY" => {
            let first = NaiveDate::from_ymd_opt(start.year(), start.month(), 1).unwrap_or(start.date());
            'months: for k in 0.. {
                if guard > 1200 {
                    break;
                }
                guard += 1;
                let Some(month) = first.checked_add_months(chrono::Months::new((k * interval) as u32)) else { break };
                let mut dates: Vec<NaiveDate> = if !byday.is_empty() {
                    byday
                        .iter()
                        .flat_map(|(n, w)| {
                            if *n == 0 {
                                (1..=5).filter_map(|i| nth_weekday(month.year(), month.month(), *w, i)).filter(|d| d.month() == month.month()).collect::<Vec<_>>()
                            } else {
                                nth_weekday(month.year(), month.month(), *w, *n).into_iter().collect()
                            }
                        })
                        .collect()
                } else {
                    let days = if bymonthday.is_empty() { vec![start.day()] } else { bymonthday.clone() };
                    days.iter().filter_map(|d| NaiveDate::from_ymd_opt(month.year(), month.month(), *d)).collect()
                };
                dates.sort();
                for d in dates {
                    if !accept(d.and_time(time), &mut out) {
                        break 'months;
                    }
                }
            }
        }
        "YEARLY" => {
            for k in 0..200 {
                let Some(d) = start.date().with_year(start.year() + (k * interval) as i32) else { continue };
                if !accept(d.and_time(time), &mut out) {
                    break;
                }
            }
        }
        _ => {
            accept(start, &mut out);
        }
    }
    out
}

/// Concrete occurrences inside [from, to).
fn expand(events: &[VEvent], from: NaiveDateTime, to: NaiveDateTime) -> Vec<CalEvent> {
    let overridden: HashSet<(String, NaiveDateTime)> = events.iter().filter_map(|e| e.recurrence_id.map(|r| (e.uid.clone(), r))).collect();
    let mut out = Vec::new();
    for ev in events {
        let Some(start) = ev.start else { continue };
        let length = match (ev.end, ev.duration) {
            (Some(end), _) if end.at > start.at => end.at - start.at,
            (_, Some(d)) if d > ChronoDuration::zero() => d,
            _ if start.all_day => ChronoDuration::days(1),
            _ => ChronoDuration::hours(1),
        };
        let starts = match (&ev.rrule, ev.recurrence_id) {
            (Some(rule), None) => occurrences(start.at, rule, from, to, &ev.exdates),
            _ => vec![start.at],
        };
        for s in starts {
            if ev.recurrence_id.is_none() && overridden.contains(&(ev.uid.clone(), s)) {
                continue;
            }
            let e = s + length;
            if ev.cancelled || s >= to || e <= from {
                continue;
            }
            out.push(CalEvent {
                uid: if ev.uid.is_empty() { format!("{}-{}", ev.summary, s) } else { ev.uid.clone() },
                title: if ev.summary.trim().is_empty() { "(untitled event)".into() } else { ev.summary.trim().to_string() },
                start: s,
                end: e,
                all_day: start.all_day,
                location: ev.location.trim().to_string(),
                description: ev.description.trim().chars().take(600).collect(),
                source: String::new(),
                href: String::new(),
                etag: String::new(),
                recurring: ev.rrule.is_some() || ev.recurrence_id.is_some(),
                pending: String::new(),
            });
        }
    }
    out.sort_by_key(|e| e.start);
    out
}

fn escape_text(s: &str) -> String {
    s.replace('\\', "\\\\").replace(';', "\\;").replace(',', "\\,").replace("\r\n", "\n").replace('\r', "\n").replace('\n', "\\n")
}

/// A content line folded at 75 octets (RFC 5545 §3.1).
fn fold(line: &str) -> String {
    let mut out = String::new();
    let mut width = 0;
    for c in line.chars() {
        let len = c.len_utf8();
        if width + len > 75 {
            out.push_str("\r\n ");
            width = 1;
        }
        out.push(c);
        width += len;
    }
    out
}

/// A local time as an iCalendar UTC value ("20261022T130000Z").
fn ics_utc(t: NaiveDateTime) -> String {
    let local = Local.from_local_datetime(&t).earliest().unwrap_or_else(|| Local.from_utc_datetime(&t));
    local.with_timezone(&Utc).format("%Y%m%dT%H%M%SZ").to_string()
}

fn time_lines(ev: &CalEvent) -> [String; 2] {
    if ev.all_day {
        [format!("DTSTART;VALUE=DATE:{}", ev.start.format("%Y%m%d")), format!("DTEND;VALUE=DATE:{}", ev.end.format("%Y%m%d"))]
    } else {
        [format!("DTSTART:{}", ics_utc(ev.start)), format!("DTEND:{}", ics_utc(ev.end))]
    }
}

/// A complete VCALENDAR with one VEVENT.
pub fn build_vcalendar(ev: &CalEvent) -> String {
    let [dtstart, dtend] = time_lines(ev);
    let mut lines = vec![
        "BEGIN:VCALENDAR".to_string(),
        "VERSION:2.0".into(),
        format!("PRODID:{PRODID}"),
        "CALSCALE:GREGORIAN".into(),
        "BEGIN:VEVENT".into(),
        format!("UID:{}", ev.uid),
        format!("DTSTAMP:{}", Utc::now().format("%Y%m%dT%H%M%SZ")),
        dtstart,
        dtend,
        format!("SUMMARY:{}", escape_text(&ev.title)),
    ];
    if !ev.location.trim().is_empty() {
        lines.push(format!("LOCATION:{}", escape_text(ev.location.trim())));
    }
    if !ev.description.trim().is_empty() {
        lines.push(format!("DESCRIPTION:{}", escape_text(ev.description.trim())));
    }
    lines.extend(["END:VEVENT".to_string(), "END:VCALENDAR".to_string()]);
    lines.iter().map(|l| fold(l)).collect::<Vec<_>>().join("\r\n") + "\r\n"
}

/// The server's resource with new times / title in its (single, non-recurring) VEVENT.
fn rewrite_event(ics: &str, ev: &CalEvent) -> Option<String> {
    let lines = unfold(ics);
    let mut out = Vec::with_capacity(lines.len() + 2);
    let mut in_event = false;
    let mut nested = 0usize;
    let mut done = false;
    let [dtstart, dtend] = time_lines(ev);
    for line in lines {
        let (name, _, value) = split_prop(&line);
        let upper = value.trim().to_uppercase();
        match name.as_str() {
            "BEGIN" if upper == "VEVENT" && !done => {
                in_event = true;
                out.push(line);
                continue;
            }
            "END" if upper == "VEVENT" && in_event => {
                in_event = false;
                done = true;
                out.push(line);
                continue;
            }
            "BEGIN" if in_event => nested += 1,
            "END" if in_event => nested = nested.saturating_sub(1),
            _ => {}
        }
        if in_event && nested == 0 {
            match name.as_str() {
                "RRULE" => return None,
                "DTSTART" => {
                    out.push(dtstart.clone());
                    out.push(dtend.clone());
                    continue;
                }
                "DTEND" | "DURATION" => continue,
                "SUMMARY" => {
                    out.push(format!("SUMMARY:{}", escape_text(&ev.title)));
                    continue;
                }
                "DTSTAMP" => {
                    out.push(format!("DTSTAMP:{}", Utc::now().format("%Y%m%dT%H%M%SZ")));
                    continue;
                }
                "SEQUENCE" => {
                    let n = value.trim().parse::<u32>().unwrap_or(0) + 1;
                    out.push(format!("SEQUENCE:{n}"));
                    continue;
                }
                _ => {}
            }
        }
        out.push(line);
    }
    done.then(|| out.iter().map(|l| fold(l)).collect::<Vec<_>>().join("\r\n") + "\r\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dt(s: &str) -> NaiveDateTime {
        NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M").unwrap()
    }

    const ICS: &str = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nUID:a1\r\nSUMMARY:Physics tutoring\\, room 2\r\nDTSTART;TZID=Europe/Berlin:20261019T160000\r\nDTEND;TZID=Europe/Berlin:20261019T170000\r\nRRULE:FREQ=WEEKLY;BYDAY=MO,TH;COUNT=6\r\nEXDATE;TZID=Europe/Berlin:20261022T160000\r\nBEGIN:VALARM\r\nTRIGGER:-PT15M\r\nDESCRIPTION:alarm\r\nEND:VALARM\r\nEND:VEVENT\r\nBEGIN:VEVENT\r\nUID:b2\r\nSUMMARY:Mum's birthday\r\nDTSTART;VALUE=DATE:20261024\r\nDTEND;VALUE=DATE:20261025\r\nEND:VEVENT\r\nBEGIN:VEVENT\r\nUID:c3\r\nSUMMARY:Dentist with a very long title that has to be folded somewhere along the line\r\n because it is long\r\nDTSTART:20261022T140000\r\nDURATION:PT45M\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";

    #[test]
    fn ics_feeds_are_parsed_and_recurrences_expanded() {
        let evs = expand(&parse_events(ICS), dt("2026-10-19 00:00"), dt("2026-11-10 00:00"));
        let tutoring: Vec<_> = evs.iter().filter(|e| e.uid == "a1").map(|e| e.start).collect();
        // Mon 19, (Thu 22 excluded), Mon 26, Thu 29, Mon 2, Thu 5 = COUNT 6 with one EXDATE.
        assert_eq!(tutoring, vec![dt("2026-10-19 16:00"), dt("2026-10-26 16:00"), dt("2026-10-29 16:00"), dt("2026-11-02 16:00"), dt("2026-11-05 16:00")]);
        assert!(evs.iter().all(|e| e.title != "alarm"), "VALARM properties do not leak");
        let birthday = evs.iter().find(|e| e.uid == "b2").unwrap();
        assert!(birthday.all_day);
        let dentist = evs.iter().find(|e| e.uid == "c3").unwrap();
        assert_eq!(dentist.end, dt("2026-10-22 14:45"));
        assert!(dentist.title.ends_with("along the linebecause it is long"));
        assert_eq!(evs.iter().find(|e| e.uid == "a1").unwrap().title, "Physics tutoring, room 2");
    }

    #[test]
    fn monthly_rules_and_conflicts() {
        let starts = occurrences(dt("2026-01-30 10:00"), "FREQ=MONTHLY;BYDAY=-1FR;COUNT=3", dt("2026-01-01 00:00"), dt("2027-01-01 00:00"), &[]);
        assert_eq!(starts, vec![dt("2026-01-30 10:00"), dt("2026-02-27 10:00"), dt("2026-03-27 10:00")]);
        let a = CalEvent::new("A", dt("2026-10-22 09:00"), dt("2026-10-22 10:00"), false);
        let b = CalEvent::new("B", dt("2026-10-22 09:30"), dt("2026-10-22 11:00"), false);
        let c = CalEvent::new("C", dt("2026-10-22 10:00"), dt("2026-10-22 10:30"), false);
        assert_eq!(conflicts(&[a, b, c]), vec![("A".to_string(), "B".to_string()), ("B".to_string(), "C".to_string())]);
    }

    #[test]
    fn built_events_round_trip_and_rewrite_in_place() {
        let mut ev = CalEvent::new("Lab report; draft, v2", dt("2026-10-23 15:00"), dt("2026-10-23 16:30"), false);
        ev.location = "Library".into();
        let ics = build_vcalendar(&ev);
        assert!(ics.contains("SUMMARY:Lab report\\; draft\\, v2"));
        let back = expand(&parse_events(&ics), dt("2026-10-23 00:00"), dt("2026-10-24 00:00"));
        assert_eq!(back.len(), 1);
        assert_eq!((back[0].start, back[0].end, back[0].title.as_str()), (ev.start, ev.end, "Lab report; draft, v2"));
        let server = "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:x\r\nDTSTART:20261023T130000Z\r\nDURATION:PT1H\r\nSUMMARY:Old\r\nSEQUENCE:2\r\nBEGIN:VALARM\r\nTRIGGER:-PT10M\r\nEND:VALARM\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
        let mut moved = ev.clone();
        moved.start = dt("2026-10-24 10:00");
        moved.end = dt("2026-10-24 11:00");
        let rewritten = rewrite_event(server, &moved).unwrap();
        assert!(rewritten.contains("SEQUENCE:3") && rewritten.contains("TRIGGER:-PT10M") && !rewritten.contains("DURATION"));
        let parsed = expand(&parse_events(&rewritten), dt("2026-10-24 00:00"), dt("2026-10-25 00:00"));
        assert_eq!(parsed[0].start, moved.start);
        assert!(rewrite_event("BEGIN:VEVENT\r\nRRULE:FREQ=DAILY\r\nEND:VEVENT\r\n", &moved).is_none(), "series are not rewritten");
    }

    #[test]
    fn caldav_multistatus_is_read_with_any_prefix() {
        let xml = r#"<?xml version="1.0"?><multistatus xmlns="DAV:"><response><href>/123/calendars/home/</href><propstat><prop><resourcetype><collection/><calendar xmlns="urn:ietf:params:xml:ns:caldav"/></resourcetype><displayname>Home</displayname></prop></propstat></response><D:response xmlns:D="DAV:"><D:href>/123/calendars/inbox/</D:href><D:propstat><D:prop><D:resourcetype><D:collection/><C:schedule-inbox xmlns:C="urn:ietf:params:xml:ns:caldav"/></D:resourcetype></D:prop></D:propstat></D:response></multistatus>"#;
        let responses = xml_all(xml, "response");
        assert_eq!(responses.len(), 2);
        assert!(has_tag(&xml_first(&responses[0], "resourcetype").unwrap(), "calendar"));
        assert!(!has_tag(&responses[1], "calendar"));
        assert_eq!(xml_first(&responses[0], "displayname").as_deref(), Some("Home"));
        assert_eq!(xml_unescape("BEGIN:VEVENT&#13;\nSUMMARY:Tom &amp; Jerry"), "BEGIN:VEVENT\r\nSUMMARY:Tom & Jerry");
        assert!(!has_tag("<C:calendar-data>x</C:calendar-data>", "calendar"));
    }

    #[tokio::test]
    async fn event_creation_is_durable_before_remote_sync() {
        let store = CalendarStore::in_memory();
        let event = CalEvent::new("Mathe lernen", dt("2026-10-23 16:00"), dt("2026-10-23 17:00"), false);
        let uid = event.uid.clone();
        let result = store.create(&CalendarSettings::default(), None, event).await.unwrap();
        assert!(result.contains("local calendar"));
        let saved = store.events_between(dt("2026-10-23 00:00"), dt("2026-10-24 00:00"));
        assert_eq!(saved.len(), 1);
        assert_eq!(saved[0].pending, "put", "adding credentials later must sync offline creations");
        store.replace_window("caldav", dt("2026-10-23 00:00"), dt("2026-10-24 00:00"), &[]);
        assert!(store.has_uid(&uid), "remote refresh must preserve the local insertion");
        let invalid = CalEvent::new("Backwards", dt("2026-10-23 17:00"), dt("2026-10-23 16:00"), false);
        assert!(store.create(&CalendarSettings::default(), None, invalid).await.is_err());
    }

    #[test]
    fn caldav_redirects_preserve_only_trusted_credentials() {
        assert!(safe_dav_redirect("https://caldav.icloud.com/", "https://p42-caldav.icloud.com/123/calendars/"));
        assert!(safe_dav_redirect("https://calendar.example/a", "https://calendar.example/b"));
        assert!(!safe_dav_redirect("https://caldav.icloud.com/", "https://icloud.com.attacker.example/"));
        assert!(!safe_dav_redirect("https://caldav.icloud.com/", "http://p42-caldav.icloud.com/"));
        assert!(!writable_properties("<d:current-user-privilege-set><d:privilege><d:read/></d:privilege></d:current-user-privilege-set>"));
        assert!(writable_properties("<d:current-user-privilege-set><d:privilege><d:bind/></d:privilege></d:current-user-privilege-set>"));
    }

    #[test]
    fn calendar_text_cannot_inject_content_lines() {
        let ev = CalEvent::new("Name\rATTENDEE:injected", dt("2026-10-23 16:00"), dt("2026-10-23 17:00"), false);
        let output = build_vcalendar(&ev);
        assert!(output.contains("SUMMARY:Name\\nATTENDEE:injected"));
        assert!(!output.contains("\rATTENDEE"));
        let folded = fold(&format!("SUMMARY:{}", "Ä".repeat(100)));
        assert!(folded.split("\r\n").all(|line| line.len() <= 75));
    }
}
