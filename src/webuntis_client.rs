//! WebUntis school timetable over its JSON-RPC 2.0 API (`/WebUntis/jsonrpc.do?school=…`):
//! `authenticate`, `getTimetable` (with substitution texts, original rooms and teachers) and
//! `getKlassen`; exams and homework come from the web client's REST endpoints
//! (`/WebUntis/api/exams`, `/WebUntis/api/homeworks/lessons`) with the same session — JSON-RPC
//! has no homework method.
//!
//! Periods with `code == "cancelled"` are free periods (Entfall); `code == "irregular"` (or an
//! original room / teacher) is a substitution or room change (Vertretung). WebUntis drops a
//! session after ~10 idle minutes: one older than 8 minutes is renewed first, and a "not
//! authenticated" answer (-8520) logs in again once. Schools behind SSO can paste a browser
//! session id (`JSESSIONID`) instead of a password.
//!
//! Dates go over JSON-RPC as `YYYYMMDD` integers. The timetable element is the logged-in person
//! (`personId` / `personType` from `authenticate`), the configured class or student. Lessons that
//! come back without names are resolved with `getSubjects` / `getRooms` / `getTeachers`; when the
//! school blocks those or the timetable itself for students (-8509), or JSON-RPC returns nothing,
//! the web client's weekly timetable (`/api/public/timetable/weekly/data`, same session cookies)
//! is read instead — it always carries plain names and `cellState` CANCEL / SUBSTITUTION.

use crate::pim_config::WebUntisSettings;

/// (element type, id) → (short name, long name).
type NameTable = std::collections::HashMap<(i64, i64), (String, String)>;
use crate::web_connector::{err_chain, host_of};
use base64::Engine;
use chrono::{Datelike, Duration as ChronoDuration, Local, NaiveDate, NaiveTime};
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tracing::{info, warn};

/// Renew the session before it gets this old.
const SESSION_MAX_IDLE: Duration = Duration::from_secs(8 * 60);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LessonStatus {
    Regular,
    /// `code == "cancelled"`: Entfall / free period.
    Cancelled,
    /// `code == "irregular"`: substitution or room change (Vertretung).
    Irregular,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Lesson {
    pub date: NaiveDate,
    pub start: NaiveTime,
    pub end: NaiveTime,
    pub subject: String,
    pub subject_long: String,
    pub teachers: Vec<String>,
    pub rooms: Vec<String>,
    /// What the room / teacher would have been (room changes and substitutions).
    pub original_rooms: Vec<String>,
    pub original_teachers: Vec<String>,
    pub status: LessonStatus,
    pub note: String,
}

impl Lesson {
    /// "Physik (PH)" or just the short name.
    pub fn subject_name(&self) -> String {
        match (self.subject_long.trim(), self.subject.trim()) {
            ("", "") => "Lesson".into(),
            (long, "") => long.to_string(),
            ("", short) => short.to_string(),
            (long, short) if long.eq_ignore_ascii_case(short) => long.to_string(),
            (long, short) => format!("{long} ({short})"),
        }
    }

    /// The subject in English when it is a known school subject ("Physik" → "Physics").
    pub fn display_subject(&self) -> String {
        let long = if self.subject_long.trim().is_empty() { self.subject.trim() } else { self.subject_long.trim() };
        crate::pim::english_subject(long).or_else(|| crate::pim::english_subject(self.subject.trim())).unwrap_or_else(|| crate::english::in_english(&self.subject_name()))
    }

    /// "07:50 - 08:35: Physics (Room 204) with MÜL [CANCELLED]".
    pub fn line(&self) -> String {
        let mut s = format!("{} - {}: {}", self.start.format("%H:%M"), self.end.format("%H:%M"), self.display_subject());
        // The weekly web API leaves names blank that the school hides from students.
        let named = |v: &[String]| v.iter().map(|x| x.trim()).filter(|x| !x.is_empty()).collect::<Vec<_>>().join(", ");
        let (rooms, teachers) = (named(&self.rooms), named(&self.teachers));
        if !rooms.is_empty() {
            s.push_str(&format!(" (Room {rooms})"));
        }
        if !teachers.is_empty() {
            s.push_str(&format!(" with {teachers}"));
        }
        match self.status {
            LessonStatus::Cancelled => s.push_str(" [CANCELLED]"),
            LessonStatus::Irregular => {
                let mut detail = Vec::new();
                if !self.original_rooms.is_empty() {
                    detail.push(format!("instead of room {}", self.original_rooms.join(", ")));
                }
                if !self.original_teachers.is_empty() {
                    detail.push(format!("instead of {}", self.original_teachers.join(", ")));
                }
                s.push_str(&format!(" [SUBSTITUTION / ROOM CHANGE{}]", if detail.is_empty() { String::new() } else { format!(": {}", detail.join(", ")) }));
            }
            LessonStatus::Regular => {}
        }
        if !self.note.is_empty() {
            s.push_str(&format!(" — note: {}", self.note));
        }
        s
    }

    /// A stable key for change detection.
    pub fn key(&self) -> String {
        format!("{} {} {} {:?} {} {} {}", self.date, self.start.format("%H:%M"), self.subject, self.status, self.rooms.join("/"), self.teachers.join("/"), self.note)
    }
}

/// An exam or a homework due date.
#[derive(Debug, Clone, PartialEq)]
pub struct SchoolTask {
    pub id: String,
    /// "exam" or "homework".
    pub kind: &'static str,
    pub date: NaiveDate,
    pub start: Option<NaiveTime>,
    pub end: Option<NaiveTime>,
    pub subject: String,
    pub text: String,
}

/// How to log in: a password (app token) or a session id pasted from the browser.
#[derive(Clone, Copy)]
pub struct Auth<'a> {
    pub password: Option<&'a str>,
    pub session: Option<&'a str>,
}

struct Session {
    key: String,
    id: String,
    person_id: i64,
    person_type: i64,
    klasse_id: i64,
    used: Instant,
    /// (element id, element type) of the timetable to read.
    element: Option<(i64, i64)>,
    bearer: Option<String>,
}

#[derive(Clone)]
pub struct WebUntis {
    http: reqwest::Client,
    session: Arc<tokio::sync::Mutex<Option<Session>>>,
    /// (type, id) → (short name, long name) from getSubjects / getRooms / getTeachers.
    names: Arc<std::sync::Mutex<NameTable>>,
    /// The last way the timetable was read ("JSON-RPC" / "weekly web API"), for diagnostics.
    pub last_source: Arc<std::sync::Mutex<String>>,
}

impl Default for WebUntis {
    fn default() -> Self {
        Self {
            // No cookie jar: the session cookie is set explicitly so a stale jar never overrides it.
            http: reqwest::Client::builder()
                .user_agent(crate::web_connector::USER_AGENT)
                .timeout(Duration::from_secs(20))
                .connect_timeout(Duration::from_secs(8))
                .build()
                .unwrap_or_default(),
            session: Arc::new(tokio::sync::Mutex::new(None)),
            names: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
            last_source: Arc::new(std::sync::Mutex::new(String::new())),
        }
    }
}

fn base(cfg: &WebUntisSettings) -> String {
    format!("https://{}/WebUntis", host_of(&cfg.server))
}

fn rpc_url(cfg: &WebUntisSettings) -> String {
    format!("{}/jsonrpc.do?school={}", base(cfg), urlencoding::encode(cfg.school.trim()))
}

fn cookie(cfg: &WebUntisSettings, session: &str) -> String {
    // School-specific servers (myschool.webuntis.com) work without the school's login name.
    if cfg.school.trim().is_empty() {
        return format!("JSESSIONID={session}");
    }
    let school = base64::engine::general_purpose::STANDARD.encode(cfg.school.trim());
    format!("JSESSIONID={session}; schoolname=\"_{school}\"")
}

fn untis_date(d: NaiveDate) -> i64 {
    d.format("%Y%m%d").to_string().parse().unwrap_or(0)
}

fn parse_date(v: &Value) -> Option<NaiveDate> {
    let n = v.as_i64().or_else(|| v.as_str()?.parse().ok())?;
    NaiveDate::parse_from_str(&n.to_string(), "%Y%m%d").ok()
}

/// A holiday of the school: its long name and its first and last day.
#[derive(Debug, Clone, PartialEq)]
pub struct Holiday {
    pub name: String,
    pub from: NaiveDate,
    pub to: NaiveDate,
}

pub fn parse_holidays(v: &Value) -> Vec<Holiday> {
    let mut out: Vec<Holiday> = v
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|h| {
            let name = h["longName"].as_str().filter(|n| !n.trim().is_empty()).or_else(|| h["name"].as_str())?.trim().to_string();
            let (from, to) = (parse_date(&h["startDate"])?, parse_date(&h["endDate"])?);
            (from <= to).then_some(Holiday { name, from, to })
        })
        .collect();
    out.sort_by_key(|h| h.from);
    out
}

/// The holiday `day` falls in, if any.
pub fn holiday_on(holidays: &[Holiday], day: NaiveDate) -> Option<&Holiday> {
    holidays.iter().find(|h| h.from <= day && day <= h.to)
}

/// The holidays last read (`Pim::holidays` keeps them), for those who cannot wait for WebUntis.
pub static HOLIDAYS: std::sync::Mutex<Option<(std::time::Instant, Vec<Holiday>)>> = std::sync::Mutex::new(None);

/// The holiday `day` falls in, from the last read only (no call to WebUntis).
pub fn known_holiday(day: NaiveDate) -> Option<Holiday> {
    HOLIDAYS.lock().unwrap_or_else(|e| e.into_inner()).as_ref().and_then(|(_, all)| holiday_on(all, day).cloned())
}

/// "The Christmas holidays: no school until Monday 11 January." — the day after a holiday's last
/// day that is not a weekend is when school starts again.
pub fn holiday_line(h: &Holiday) -> String {
    use chrono::Datelike;
    let mut back = h.to + chrono::Duration::days(1);
    while matches!(back.weekday(), chrono::Weekday::Sat | chrono::Weekday::Sun) {
        back += chrono::Duration::days(1);
    }
    format!("{}: no school until {}.", h.name, back.format("%A %-d %B"))
}

fn parse_time(v: &Value) -> Option<NaiveTime> {
    let n = v.as_i64().or_else(|| v.as_str()?.parse().ok())?;
    NaiveTime::from_hms_opt((n / 100) as u32, (n % 100) as u32, 0)
}

/// Human messages for WebUntis' JSON-RPC error codes.
fn rpc_error(e: &Value) -> String {
    let code = e["code"].as_i64().unwrap_or(0);
    let message = e["message"].as_str().unwrap_or("unknown error");
    match code {
        -8504 => "WebUntis rejected the username or password".into(),
        -8520 => "the WebUntis session expired".into(),
        -8509 => "this WebUntis account has no right to read that timetable (set your class or student id)".into(),
        -8998 => "WebUntis blocked the login for a while after too many attempts".into(),
        _ => format!("WebUntis error {code}: {message}"),
    }
}

impl WebUntis {
    async fn post(&self, cfg: &WebUntisSettings, session: Option<&str>, method: &str, params: Value) -> Result<Value, String> {
        let mut req = self.http.post(rpc_url(cfg)).json(&json!({ "id": "jai", "method": method, "params": params, "jsonrpc": "2.0" }));
        if let Some(s) = session {
            req = req.header(reqwest::header::COOKIE, cookie(cfg, s));
        }
        let resp = req.send().await.map_err(err_chain)?;
        let status = resp.status();
        let body: Value = resp.json().await.map_err(|e| format!("WebUntis sent no JSON (HTTP {}; check the server and school name): {e}", status.as_u16()))?;
        if let Some(err) = body.get("error").filter(|e| !e.is_null()) {
            return Err(rpc_error(err));
        }
        Ok(body["result"].clone())
    }

    async fn login(&self, cfg: &WebUntisSettings, auth: Auth<'_>) -> Result<Session, String> {
        let key = format!("{}|{}|{}", host_of(&cfg.server), cfg.school.trim(), cfg.username.trim());
        if let Some(password) = auth.password.filter(|p| !p.is_empty()) {
            if cfg.username.trim().is_empty() {
                return Err("WebUntis needs a username for the password login".into());
            }
            let client = if cfg.client_name.trim().is_empty() { "JAI" } else { cfg.client_name.trim() };
            let r = self.post(cfg, None, "authenticate", json!({ "user": cfg.username.trim(), "password": password, "client": client })).await?;
            let id = r["sessionId"].as_str().ok_or("WebUntis returned no session")?.to_string();
            info!(
                "[UNTIS] Logged in to {}{} (personId {}, personType {}, klasseId {})",
                host_of(&cfg.server),
                if cfg.school.trim().is_empty() { String::new() } else { format!(" / {}", cfg.school.trim()) },
                r["personId"].as_i64().unwrap_or(0),
                r["personType"].as_i64().unwrap_or(0),
                r["klasseId"].as_i64().unwrap_or(0)
            );
            return Ok(Session {
                key,
                id,
                person_id: r["personId"].as_i64().unwrap_or(0),
                person_type: r["personType"].as_i64().unwrap_or(0),
                klasse_id: r["klasseId"].as_i64().unwrap_or(0),
                used: Instant::now(),
                element: None,
                bearer: None,
            });
        }
        let id = auth.session.filter(|s| !s.is_empty()).ok_or("WebUntis needs a password or a session id")?;
        Ok(Session { key, id: id.trim().to_string(), person_id: 0, person_type: 0, klasse_id: 0, used: Instant::now(), element: None, bearer: None })
    }

    /// A live session (renewed when idle too long or for another account).
    async fn ensure<'a>(&self, guard: &'a mut Option<Session>, cfg: &WebUntisSettings, auth: Auth<'_>, force: bool) -> Result<&'a mut Session, String> {
        let key = format!("{}|{}|{}", host_of(&cfg.server), cfg.school.trim(), cfg.username.trim());
        let stale = guard.as_ref().is_none_or(|s| s.key != key || s.used.elapsed() > SESSION_MAX_IDLE);
        if force || stale {
            if let Some(old) = guard.take().filter(|s| s.key == key && auth.password.is_some()) {
                let _ = self.post(cfg, Some(&old.id), "logout", json!({})).await;
            }
            *guard = Some(self.login(cfg, auth).await?);
        }
        let s = guard.as_mut().ok_or("no WebUntis session")?;
        s.used = Instant::now();
        Ok(s)
    }

    /// A JSON-RPC call with one silent re-login when the session expired.
    async fn rpc(&self, cfg: &WebUntisSettings, auth: Auth<'_>, method: &str, params: Value) -> Result<Value, String> {
        let mut guard = self.session.lock().await;
        let id = self.ensure(&mut guard, cfg, auth, false).await?.id.clone();
        match self.post(cfg, Some(&id), method, params.clone()).await {
            Err(e) if e.contains("expired") && auth.password.is_some() => {
                info!("[UNTIS] Session expired; logging in again");
                let id = self.ensure(&mut guard, cfg, auth, true).await?.id.clone();
                self.post(cfg, Some(&id), method, params).await
            }
            Err(e) if e.contains("expired") => Err("the pasted WebUntis session id has expired; paste a fresh one in the Integrations hub".into()),
            other => other,
        }
    }

    /// A REST call of the WebUntis web client (bearer token fetched when required).
    async fn rest(&self, cfg: &WebUntisSettings, auth: Auth<'_>, path: &str) -> Result<Value, String> {
        let mut guard = self.session.lock().await;
        let session = self.ensure(&mut guard, cfg, auth, false).await?;
        let url = format!("{}/{}", base(cfg), path.trim_start_matches('/'));
        for attempt in 0..2 {
            let mut req = self.http.get(&url).header(reqwest::header::COOKIE, cookie(cfg, &session.id)).header(reqwest::header::ACCEPT, "application/json");
            if let Some(b) = &session.bearer {
                req = req.bearer_auth(b);
            }
            let resp = req.send().await.map_err(err_chain)?;
            let status = resp.status().as_u16();
            if (status == 401 || status == 403) && attempt == 0 && session.bearer.is_none() {
                let token = self
                    .http
                    .get(format!("{}/api/token/new", base(cfg)))
                    .header(reqwest::header::COOKIE, cookie(cfg, &session.id))
                    .send()
                    .await
                    .map_err(err_chain)?
                    .text()
                    .await
                    .map_err(err_chain)?;
                session.bearer = Some(token.trim().trim_matches('"').to_string());
                continue;
            }
            if status >= 400 {
                return Err(format!("WebUntis answered HTTP {status} for {path}"));
            }
            return resp.json().await.map_err(|e| format!("WebUntis sent no JSON for {path}: {e}"));
        }
        Err(format!("WebUntis refused {path}"))
    }

    /// The (id, type) of the timetable to read: the configured class, the configured student,
    /// or the logged-in person.
    async fn element(&self, cfg: &WebUntisSettings, auth: Auth<'_>) -> Result<(i64, i64), String> {
        {
            let mut guard = self.session.lock().await;
            let s = self.ensure(&mut guard, cfg, auth, false).await?;
            if let Some(e) = s.element {
                return Ok(e);
            }
        }
        let class = cfg.class_name.trim();
        let element = if !class.is_empty() {
            let classes = self.rpc(cfg, auth, "getKlassen", json!({})).await?;
            let found = classes.as_array().and_then(|list| {
                list.iter().find(|k| {
                    ["name", "longName", "longname"].iter().filter_map(|f| k[*f].as_str()).any(|n| n.trim().eq_ignore_ascii_case(class))
                })
            });
            let id = found.and_then(|k| k["id"].as_i64()).ok_or_else(|| format!("WebUntis has no class called '{class}'"))?;
            (id, 1)
        } else if let Ok(id) = cfg.student_id.trim().parse::<i64>() {
            (id, 5)
        } else {
            let guard = self.session.lock().await;
            let s = guard.as_ref().ok_or("no WebUntis session")?;
            if s.person_id > 0 {
                (s.person_id, if s.person_type > 0 { s.person_type } else { 5 })
            } else if s.klasse_id > 0 {
                (s.klasse_id, 1)
            } else {
                return Err("WebUntis did not say whose timetable to read; enter your class (e.g. 10b) or student id in the Integrations hub".into());
            }
        };
        if let Some(s) = self.session.lock().await.as_mut() {
            s.element = Some(element);
        }
        Ok(element)
    }

    /// Lessons in [from, to], sorted, double periods merged: JSON-RPC first (names resolved
    /// when missing), the web client's weekly timetable when JSON-RPC is blocked or empty.
    pub async fn lessons(&self, cfg: &WebUntisSettings, auth: Auth<'_>, from: NaiveDate, to: NaiveDate) -> Result<Vec<Lesson>, String> {
        let rpc = match self.element(cfg, auth).await {
            Ok(element) => self.rpc_lessons(cfg, auth, element, from, to).await,
            Err(e) => Err(e),
        };
        let note = |source: &str| *self.last_source.lock().unwrap_or_else(|e| e.into_inner()) = source.to_string();
        match rpc {
            Ok(lessons) if !lessons.is_empty() && lessons.iter().all(|l| !l.subject.trim().is_empty() || !l.subject_long.trim().is_empty()) => {
                note("JSON-RPC getTimetable");
                Ok(lessons)
            }
            Ok(lessons) => match self.weekly_lessons(cfg, auth, from, to).await {
                Ok(weekly) if !weekly.is_empty() => {
                    note("weekly web API (JSON-RPC returned no or nameless lessons)");
                    Ok(weekly)
                }
                Ok(_) | Err(_) => {
                    note("JSON-RPC getTimetable");
                    Ok(lessons)
                }
            },
            Err(e) => {
                warn!("[UNTIS] JSON-RPC timetable failed ({e}); trying the weekly web API");
                match self.weekly_lessons(cfg, auth, from, to).await {
                    Ok(weekly) => {
                        note("weekly web API (JSON-RPC refused)");
                        Ok(weekly)
                    }
                    Err(e2) => Err(format!("{e} (weekly web API: {e2})")),
                }
            }
        }
    }

    async fn rpc_lessons(&self, cfg: &WebUntisSettings, auth: Auth<'_>, (id, kind): (i64, i64), from: NaiveDate, to: NaiveDate) -> Result<Vec<Lesson>, String> {
        let fields = json!(["id", "name", "longname"]);
        let result = self
            .rpc(
                cfg,
                auth,
                "getTimetable",
                json!({ "options": {
                    "element": { "id": id, "type": kind },
                    "startDate": untis_date(from), "endDate": untis_date(to),
                    "showInfo": true, "showSubstText": true, "showLsText": true, "showLsNumber": true, "showStudentgroup": true, "showBooking": true,
                    "klasseFields": fields, "roomFields": fields, "subjectFields": fields, "teacherFields": fields
                }}),
            )
            .await?;
        let raw: Vec<Value> = result.as_array().cloned().unwrap_or_default();
        // Entries with ids but no names: resolve them through the master data (best effort).
        let nameless = raw.iter().any(|v| ["su", "ro", "te"].iter().any(|k| v[*k].as_array().is_some_and(|a| a.iter().any(|x| x["name"].as_str().is_none() && x["id"].is_i64()))));
        if nameless {
            self.load_names(cfg, auth).await;
        }
        let names = self.names.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let mut lessons: Vec<Lesson> = raw.iter().filter_map(|v| parse_lesson_with(v, &names)).collect();
        lessons.sort_by_key(|l| (l.date, l.start));
        Ok(merge_double_periods(lessons))
    }

    /// Subject, room and teacher names (each list may be forbidden for students: -8509).
    async fn load_names(&self, cfg: &WebUntisSettings, auth: Auth<'_>) {
        for (method, kind) in [("getSubjects", 3i64), ("getRooms", 4), ("getTeachers", 2)] {
            match self.rpc(cfg, auth, method, json!({})).await {
                Ok(list) => {
                    let mut names = self.names.lock().unwrap_or_else(|e| e.into_inner());
                    for x in list.as_array().into_iter().flatten() {
                        if let Some(id) = x["id"].as_i64() {
                            let short = x["name"].as_str().unwrap_or("").to_string();
                            let long = x["longName"].as_str().or(x["longname"].as_str()).unwrap_or("").to_string();
                            names.insert((kind, id), (short, long));
                        }
                    }
                }
                Err(e) => info!("[UNTIS] {method} unavailable ({e})"),
            }
        }
    }

    /// The web client's weekly timetable, one request per week in [from, to].
    pub async fn weekly_lessons(&self, cfg: &WebUntisSettings, auth: Auth<'_>, from: NaiveDate, to: NaiveDate) -> Result<Vec<Lesson>, String> {
        let (id, kind) = match self.element(cfg, auth).await {
            Ok(e) => e,
            Err(_) => self.page_element(cfg, auth).await?,
        };
        // The weekly endpoint knows classes (1) and students (5).
        let kind = if kind == 1 { 1 } else { 5 };
        let mut monday = from - ChronoDuration::days(from.weekday().num_days_from_monday() as i64);
        let mut out = Vec::new();
        while monday <= to {
            let path = format!("api/public/timetable/weekly/data?elementType={kind}&elementId={id}&date={}&formatId=1", monday.format("%Y-%m-%d"));
            let v = self.rest(cfg, auth, &path).await?;
            out.extend(parse_weekly(&v, id).into_iter().filter(|l| l.date >= from && l.date <= to));
            monday += ChronoDuration::days(7);
        }
        out.sort_by_key(|l| (l.date, l.start));
        Ok(merge_double_periods(out))
    }

    /// The student (or class) the web client shows by default, when login gave no person id.
    async fn page_element(&self, cfg: &WebUntisSettings, auth: Auth<'_>) -> Result<(i64, i64), String> {
        for kind in [5i64, 1] {
            if let Ok(v) = self.rest(cfg, auth, &format!("api/public/timetable/weekly/pageconfig?type={kind}")).await {
                let data = if v["data"].is_object() { &v["data"] } else { &v };
                if let Some(id) = data["selectedElementId"].as_i64().filter(|i| *i > 0) {
                    return Ok((id, kind));
                }
                if let Some(id) = data["elements"].as_array().and_then(|a| a.first()).and_then(|e| e["id"].as_i64()) {
                    return Ok((id, kind));
                }
            }
        }
        Err("WebUntis did not say whose timetable to read; enter your class (e.g. 10b) or student id in the Integrations hub".into())
    }

    /// The school's holidays (JSON-RPC `getHolidays`).
    pub async fn holidays(&self, cfg: &WebUntisSettings, auth: Auth<'_>) -> Result<Vec<Holiday>, String> {
        self.rpc(cfg, auth, "getHolidays", json!({})).await.map(|v| parse_holidays(&v))
    }

    /// Exams and homework due in [from, to] (each best effort: schools switch them off).
    pub async fn tasks(&self, cfg: &WebUntisSettings, auth: Auth<'_>, from: NaiveDate, to: NaiveDate) -> Result<Vec<SchoolTask>, String> {
        let range = format!("startDate={}&endDate={}", untis_date(from), untis_date(to));
        let mut out = Vec::new();
        let mut errors = Vec::new();
        match self.rest(cfg, auth, &format!("api/exams?{range}&klasseId=-1&withGrades=false")).await {
            Ok(v) => out.extend(parse_exams(&v)),
            Err(e) => errors.push(e),
        }
        match self.rest(cfg, auth, &format!("api/homeworks/lessons?{range}")).await {
            Ok(v) => out.extend(parse_homework(&v)),
            Err(e) => errors.push(e),
        }
        if out.is_empty() && errors.len() == 2 {
            warn!("[UNTIS] Exams/homework unavailable: {}", errors.join("; "));
            return Err(errors.join("; "));
        }
        out.sort_by_key(|t| (t.date, t.start));
        Ok(out)
    }

    pub async fn test(&self, cfg: &WebUntisSettings, auth: Auth<'_>) -> Result<String, String> {
        *self.session.lock().await = None;
        let today = Local::now().date_naive();
        let lessons = self.lessons(cfg, auth, today, today + ChronoDuration::days(6)).await?;
        Ok(format!("Connected to {} ({}): {} lessons in the next 7 days.", host_of(&cfg.server), cfg.school.trim(), lessons.len()))
    }
}

fn names(v: &Value, key: &str, field: &str) -> Vec<String> {
    v[key]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|x| x[field].as_str().map(str::trim).filter(|s| !s.is_empty()).map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
fn parse_lesson(v: &Value) -> Option<Lesson> {
    parse_lesson_with(v, &std::collections::HashMap::new())
}

/// Names of one element list, falling back to the master data by id.
fn names_or_ids(v: &Value, key: &str, field: &str, kind: i64, master: &std::collections::HashMap<(i64, i64), (String, String)>) -> Vec<String> {
    v[key]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|x| {
                    x[field].as_str().map(str::trim).filter(|s| !s.is_empty()).map(String::from).or_else(|| {
                        let id = x["id"].as_i64()?;
                        master.get(&(kind, id)).map(|(short, long)| if field == "longname" && !long.is_empty() { long.clone() } else { short.clone() }).filter(|s| !s.is_empty())
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

fn parse_lesson_with(v: &Value, master: &std::collections::HashMap<(i64, i64), (String, String)>) -> Option<Lesson> {
    let date = parse_date(&v["date"])?;
    let start = parse_time(&v["startTime"])?;
    let end = parse_time(&v["endTime"]).unwrap_or(start);
    let original_rooms = names(v, "ro", "orgname");
    let original_teachers = names(v, "te", "orgname");
    let status = match v["code"].as_str().unwrap_or("") {
        "cancelled" => LessonStatus::Cancelled,
        "irregular" => LessonStatus::Irregular,
        _ if !original_rooms.is_empty() || !original_teachers.is_empty() => LessonStatus::Irregular,
        _ => LessonStatus::Regular,
    };
    let note = ["substText", "info", "lstext"]
        .iter()
        .filter_map(|k| v[*k].as_str().map(str::trim).filter(|s| !s.is_empty()))
        .collect::<Vec<_>>()
        .join(" / ");
    Some(Lesson {
        date,
        start,
        end,
        subject: names_or_ids(v, "su", "name", 3, master).join(", "),
        subject_long: names_or_ids(v, "su", "longname", 3, master).join(", "),
        teachers: names_or_ids(v, "te", "name", 2, master),
        rooms: names_or_ids(v, "ro", "name", 4, master),
        original_rooms,
        original_teachers,
        status,
        note,
    })
}

/// The weekly web API: `data.result.data.elementPeriods[<id>]` periods whose `elements` point
/// into `data.result.data.elements` (type 2 teacher, 3 subject, 4 room).
fn parse_weekly(v: &Value, element_id: i64) -> Vec<Lesson> {
    let data = [&v["data"]["result"]["data"], &v["data"]["result"], &v["data"], v].into_iter().find(|d| d["elementPeriods"].is_object()).cloned().unwrap_or(Value::Null);
    let elements: std::collections::HashMap<(i64, i64), (String, String)> = data["elements"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|e| {
                    let key = (e["type"].as_i64()?, e["id"].as_i64()?);
                    let short = e["name"].as_str().unwrap_or("").to_string();
                    let long = e["longName"].as_str().or(e["displayname"].as_str()).unwrap_or("").to_string();
                    Some((key, (short, long)))
                })
                .collect()
        })
        .unwrap_or_default();
    let periods = data["elementPeriods"][element_id.to_string()].as_array().cloned().or_else(|| data["elementPeriods"].as_object().and_then(|o| o.values().next()).and_then(Value::as_array).cloned()).unwrap_or_default();
    periods
        .iter()
        .filter_map(|p| {
            let date = parse_date(&p["date"])?;
            let start = parse_time(&p["startTime"])?;
            let end = parse_time(&p["endTime"]).unwrap_or(start);
            let mut subject = (String::new(), String::new());
            let (mut rooms, mut teachers, mut org_rooms, mut org_teachers) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
            for el in p["elements"].as_array().into_iter().flatten() {
                let kind = el["type"].as_i64().unwrap_or(0);
                let name_of = |id: i64| elements.get(&(kind, id)).cloned();
                let current = el["id"].as_i64().and_then(name_of);
                let original = el["orgId"].as_i64().filter(|o| *o > 0).and_then(name_of);
                match kind {
                    3 => subject = current.unwrap_or_default(),
                    4 => {
                        rooms.extend(current.map(|c| c.0));
                        org_rooms.extend(original.map(|c| c.0));
                    }
                    2 => {
                        teachers.extend(current.map(|c| c.0));
                        org_teachers.extend(original.map(|c| c.0));
                    }
                    _ => {}
                }
            }
            let state = p["cellState"].as_str().unwrap_or("STANDARD").to_uppercase();
            let status = match state.as_str() {
                "CANCEL" | "CANCELLED" | "FREE" => LessonStatus::Cancelled,
                "SUBSTITUTION" | "ROOMSUBSTITUTION" | "ADDITIONAL" | "SHIFT" | "IRREGULAR" => LessonStatus::Irregular,
                _ if !org_rooms.is_empty() || !org_teachers.is_empty() => LessonStatus::Irregular,
                _ => LessonStatus::Regular,
            };
            let mut note: Vec<String> = ["substText", "lessonText", "periodText", "periodInfo"]
                .iter()
                .filter_map(|k| p[*k].as_str().map(str::trim).filter(|s| !s.is_empty()).map(String::from))
                .collect();
            if state == "EXAM" {
                note.insert(0, "exam".into());
            }
            Some(Lesson {
                date,
                start,
                end,
                subject: subject.0,
                subject_long: subject.1,
                teachers,
                rooms,
                original_rooms: org_rooms,
                original_teachers: org_teachers,
                status,
                note: note.join(" / "),
            })
        })
        .collect()
}

/// The timetable of one day the way JAI says it ("Tomorrow you have five lessons, from 7:50
/// to 13:00: Physics in room 204 — cancelled, …").
pub fn spoken_day(lessons: &[Lesson], day: NaiveDate) -> String {
    let today = Local::now().date_naive();
    let label = crate::pim::spoken_day(day, today);
    let todays: Vec<&Lesson> = lessons.iter().filter(|l| l.date == day).collect();
    if todays.is_empty() {
        return format!("You have no lessons {}{}, Sir.", if label == "today" || label == "tomorrow" { "" } else { "on " }, label);
    }
    let held: Vec<&&Lesson> = todays.iter().filter(|l| l.status != LessonStatus::Cancelled).collect();
    let t = |n: NaiveTime| n.format("%-H:%M").to_string();
    let first = held.first().map(|l| t(l.start)).unwrap_or_else(|| t(todays[0].start));
    let last = held.last().map(|l| t(l.end)).unwrap_or_else(|| t(todays[todays.len() - 1].end));
    let items: Vec<String> = todays
        .iter()
        .take(9)
        .map(|l| {
            let mut s = format!("{} at {}", l.display_subject(), t(l.start));
            if let Some(room) = l.rooms.first() {
                s.push_str(&format!(" in room {room}"));
            }
            match l.status {
                LessonStatus::Cancelled => s.push_str(", cancelled"),
                LessonStatus::Irregular => s.push_str(", with a substitution or room change"),
                LessonStatus::Regular => {}
            }
            s
        })
        .collect();
    let cancelled = todays.len() - held.len();
    format!(
        "{} you have {} lesson{}{}, from {first} to {last}: {}.",
        crate::pim::capitalise(&label),
        held.len(),
        if held.len() == 1 { "" } else { "s" },
        if cancelled > 0 { format!(" ({cancelled} cancelled)") } else { String::new() },
        items.join("; ")
    )
}

/// Back-to-back periods of the same lesson become one ("08:00–09:30 Maths").
fn merge_double_periods(lessons: Vec<Lesson>) -> Vec<Lesson> {
    let mut out: Vec<Lesson> = Vec::with_capacity(lessons.len());
    for l in lessons {
        if let Some(prev) = out.last_mut() {
            let gap = l.start.signed_duration_since(prev.end);
            let same = prev.date == l.date
                && prev.subject == l.subject
                && prev.status == l.status
                && prev.rooms == l.rooms
                && prev.teachers == l.teachers
                && prev.note == l.note;
            if same && gap >= ChronoDuration::zero() && gap <= ChronoDuration::minutes(15) {
                prev.end = l.end;
                continue;
            }
            // The same period listed twice (one entry per student group).
            if prev.date == l.date && prev.start == l.start && prev.subject == l.subject && prev.status == l.status {
                continue;
            }
        }
        out.push(l);
    }
    out
}

fn str_field(v: &Value, keys: &[&str]) -> String {
    keys.iter()
        .find_map(|k| match &v[*k] {
            Value::String(s) if !s.trim().is_empty() => Some(s.trim().to_string()),
            Value::Array(a) if !a.is_empty() => Some(a.iter().filter_map(|x| x.as_str().or_else(|| x["name"].as_str())).collect::<Vec<_>>().join(", ")),
            Value::Object(o) => o.get("name").or_else(|| o.get("longName")).and_then(Value::as_str).map(String::from),
            _ => None,
        })
        .unwrap_or_default()
}

fn parse_exams(v: &Value) -> Vec<SchoolTask> {
    let list = v["data"]["exams"].as_array().or_else(|| v["exams"].as_array()).cloned().unwrap_or_default();
    list.iter()
        .filter_map(|e| {
            let date = parse_date(&e["examDate"]).or_else(|| parse_date(&e["date"]))?;
            let subject = str_field(e, &["subject", "subjectName"]);
            let name = str_field(e, &["name", "examType"]);
            let text = [name, str_field(e, &["text"])].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" — ");
            Some(SchoolTask {
                id: format!("exam-{}", e["id"].as_i64().map(|i| i.to_string()).unwrap_or_else(|| format!("{date}-{subject}"))),
                kind: "exam",
                date,
                start: parse_time(&e["startTime"]),
                end: parse_time(&e["endTime"]),
                subject,
                text,
            })
        })
        .collect()
}

fn parse_homework(v: &Value) -> Vec<SchoolTask> {
    let data = if v["data"].is_object() { &v["data"] } else { v };
    let lessons: std::collections::HashMap<i64, String> = data["lessons"]
        .as_array()
        .map(|a| a.iter().filter_map(|l| Some((l["id"].as_i64()?, str_field(l, &["subject", "subjectName"])))).collect())
        .unwrap_or_default();
    data["homeworks"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter(|h| h["completed"].as_bool() != Some(true))
                .filter_map(|h| {
                    let date = parse_date(&h["dueDate"]).or_else(|| parse_date(&h["date"]))?;
                    let subject = h["lessonId"].as_i64().and_then(|id| lessons.get(&id).cloned()).unwrap_or_default();
                    Some(SchoolTask {
                        id: format!("homework-{}", h["id"].as_i64().map(|i| i.to_string()).unwrap_or_else(|| format!("{date}-{subject}"))),
                        kind: "homework",
                        date,
                        start: None,
                        end: None,
                        subject,
                        text: str_field(h, &["text", "remark"]),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The timetable of a range for Sir, day by day.
pub fn schedule_text(lessons: &[Lesson], from: NaiveDate, to: NaiveDate, only_changes: bool) -> String {
    let today = Local::now().date_naive();
    let mut out = Vec::new();
    let mut day = from;
    while day <= to {
        let todays: Vec<&Lesson> = lessons.iter().filter(|l| l.date == day && (!only_changes || l.status != LessonStatus::Regular)).collect();
        let all_today = lessons.iter().filter(|l| l.date == day).count();
        let label = crate::pim::capitalise(&crate::pim::spoken_day(day, today));
        if todays.is_empty() {
            if only_changes && all_today > 0 {
                out.push(format!("{label}: no cancellations or changes ({all_today} lessons as planned)."));
            } else if from == to {
                out.push(format!("{label}: no lessons."));
            }
        } else {
            let cancelled = todays.iter().filter(|l| l.status == LessonStatus::Cancelled).count();
            let changed = todays.iter().filter(|l| l.status == LessonStatus::Irregular).count();
            let mut head = format!("{label}:");
            if cancelled + changed > 0 {
                head.push_str(&format!(" {cancelled} cancelled, {changed} substitution(s)/room change(s)."));
            }
            out.push(head);
            out.extend(todays.iter().map(|l| format!("- {}", l.line())));
        }
        day += ChronoDuration::days(1);
    }
    if out.is_empty() {
        return if only_changes { "No cancellations or changes in that period.".into() } else { "No lessons in that period.".into() };
    }
    out.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn holidays_are_read_and_say_when_school_starts_again() {
        let v = json!([
            {"id": 2, "name": "Weihn", "longName": "Weihnachtsferien", "startDate": 20261221, "endDate": 20270108},
            {"id": 1, "name": "Herbst", "longName": "", "startDate": "20261026", "endDate": 20261030},
            {"id": 3, "name": "Broken", "longName": "Broken", "startDate": 20261201, "endDate": 20261130},
            {"id": 4, "name": "NoDate", "longName": "No date"}
        ]);
        let all = parse_holidays(&v);
        assert_eq!(all.iter().map(|h| h.name.as_str()).collect::<Vec<_>>(), ["Herbst", "Weihnachtsferien"], "sorted, an empty long name falls back to the short one, a reversed or missing range is left out");
        let d = |y: i32, m: u32, day: u32| NaiveDate::from_ymd_opt(y, m, day).unwrap();
        assert_eq!(holiday_on(&all, d(2026, 12, 21)).map(|h| h.name.as_str()), Some("Weihnachtsferien"), "the first day");
        assert_eq!(holiday_on(&all, d(2027, 1, 8)).map(|h| h.name.as_str()), Some("Weihnachtsferien"), "the last day");
        assert_eq!(holiday_on(&all, d(2027, 1, 11)), None);
        assert_eq!(holiday_on(&all, d(2026, 11, 16)), None);
        assert_eq!(holiday_line(&all[1]), "Weihnachtsferien: no school until Monday 11 January.", "a Friday's end: school on the Monday");
        assert_eq!(holiday_line(&all[0]), "Herbst: no school until Monday 2 November.");
        assert!(parse_holidays(&json!({"error": "no"})).is_empty());
    }

    #[test]
    fn periods_are_classified_and_merged() {
        let raw = json!([
            {"date": 20261022, "startTime": 800, "endTime": 845, "su": [{"name": "PH", "longname": "Physik"}], "te": [{"name": "MÜL"}], "ro": [{"name": "204"}], "code": "cancelled"},
            {"date": 20261022, "startTime": 845, "endTime": 930, "su": [{"name": "PH", "longname": "Physik"}], "te": [{"name": "MÜL"}], "ro": [{"name": "204"}], "code": "cancelled"},
            {"date": 20261022, "startTime": 950, "endTime": 1035, "su": [{"name": "M", "longname": "Mathematik"}], "te": [{"name": "SCH", "orgname": "BEC"}], "ro": [{"name": "101", "orgname": "105"}], "substText": "Vertretung"},
            {"date": 20261022, "startTime": 1055, "endTime": 1140, "su": [{"name": "E"}], "te": [{"name": "KLE"}], "ro": [{"name": "12"}]}
        ]);
        let mut lessons: Vec<Lesson> = raw.as_array().unwrap().iter().filter_map(parse_lesson).collect();
        lessons.sort_by_key(|l| (l.date, l.start));
        let lessons = merge_double_periods(lessons);
        assert_eq!(lessons.len(), 3);
        assert_eq!(lessons[0].status, LessonStatus::Cancelled);
        assert_eq!(lessons[0].end, NaiveTime::from_hms_opt(9, 30, 0).unwrap());
        assert_eq!(lessons[0].line(), "08:00 - 09:30: Physics (Room 204) with MÜL [CANCELLED]");
        assert_eq!(lessons[1].status, LessonStatus::Irregular);
        let line = lessons[1].line();
        assert!(line.contains("SUBSTITUTION / ROOM CHANGE") && line.contains("instead of room 105") && line.contains("instead of BEC"), "{line}");
        assert!(line.starts_with("09:50 - 10:35: Mathematics (Room 101)"), "{line}");
        assert!(spoken_day(&lessons, NaiveDate::from_ymd_opt(2026, 10, 22).unwrap()).contains("Physics at 8:00 in room 204, cancelled"));
        assert_eq!(lessons[2].status, LessonStatus::Regular);
        let day = NaiveDate::from_ymd_opt(2026, 10, 22).unwrap();
        let text = schedule_text(&lessons, day, day, true);
        assert!(text.contains("1 cancelled, 1 substitution") && !text.contains("KLE"), "{text}");
    }

    #[test]
    fn weekly_web_api_periods_are_read() {
        let v = json!({"data": {"result": {"data": {
            "elementPeriods": {"4711": [
                {"date": 20261023, "startTime": 750, "endTime": 835, "cellState": "CANCEL", "elements": [{"type": 3, "id": 12}, {"type": 4, "id": 204}, {"type": 2, "id": 9}]},
                {"date": 20261023, "startTime": 835, "endTime": 920, "cellState": "SUBSTITUTION", "substText": "Vertretung", "elements": [{"type": 3, "id": 13}, {"type": 4, "id": 101, "orgId": 105}, {"type": 2, "id": 8, "orgId": 9}]}
            ]},
            "elements": [
                {"type": 3, "id": 12, "name": "PH", "longName": "Physik"}, {"type": 3, "id": 13, "name": "M", "longName": "Mathematik"},
                {"type": 4, "id": 204, "name": "204"}, {"type": 4, "id": 101, "name": "101"}, {"type": 4, "id": 105, "name": "105"},
                {"type": 2, "id": 9, "name": "MÜL"}, {"type": 2, "id": 8, "name": "SCH"}
            ]
        }}}});
        let lessons = parse_weekly(&v, 4711);
        assert_eq!(lessons.len(), 2);
        assert_eq!(lessons[0].line(), "07:50 - 08:35: Physics (Room 204) with MÜL [CANCELLED]");
        assert_eq!(lessons[1].status, LessonStatus::Irregular);
        assert!(lessons[1].line().contains("instead of room 105") && lessons[1].line().contains("instead of MÜL"), "{}", lessons[1].line());
        // JSON-RPC entries with ids only get their names from the master data.
        let mut master = std::collections::HashMap::new();
        master.insert((3, 12), ("PH".to_string(), "Physik".to_string()));
        let l = parse_lesson_with(&json!({"date": 20261023, "startTime": 750, "endTime": 835, "su": [{"id": 12}], "ro": [], "te": []}), &master).unwrap();
        assert_eq!(l.display_subject(), "Physics");
    }

    #[test]
    fn exams_and_homework_are_read() {
        let exams = json!({"data": {"exams": [{"id": 7, "examDate": 20261105, "startTime": 800, "endTime": 930, "subject": "M", "name": "Klassenarbeit", "text": "Analysis"}]}});
        let t = parse_exams(&exams);
        assert_eq!((t[0].kind, t[0].subject.as_str(), t[0].text.as_str()), ("exam", "M", "Klassenarbeit — Analysis"));
        let hw = json!({"data": {"lessons": [{"id": 3, "subject": "Physik"}], "homeworks": [{"id": 9, "lessonId": 3, "dueDate": 20261023, "text": "Worksheet 4", "completed": false}, {"id": 10, "lessonId": 3, "dueDate": 20261023, "text": "done", "completed": true}]}});
        let h = parse_homework(&hw);
        assert_eq!(h.len(), 1);
        assert_eq!((h[0].id.as_str(), h[0].subject.as_str(), h[0].text.as_str()), ("homework-9", "Physik", "Worksheet 4"));
        assert_eq!(cookie(&WebUntisSettings { school: "gym-x".into(), ..Default::default() }, "ABC"), "JSESSIONID=ABC; schoolname=\"_Z3ltLXg=\"");
    }
}
