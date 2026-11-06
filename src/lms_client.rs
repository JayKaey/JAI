//! itslearning course updates: teacher announcements, newly uploaded materials and assignment
//! deadlines, from the portal's REST API (`/restapi/oauth2/token` with the mobile app's public
//! client id, then `/restapi/personal/...`), an RSS / notification feed, or a custom endpoint.
//!
//! Academic portals drop a session after ~10 idle minutes, so every request goes through the
//! `SessionKeepAliveManager`: one persistent cookie-jar `reqwest::Client`, the OAuth tokens, and
//! the time of the last request. When the session has been idle longer than the keep-alive
//! interval (8 minutes by default) it is refreshed first (refresh token, else a new login); a
//! background heartbeat keeps it warm while Jarvis runs, and a 401 triggers one re-login.

use crate::pim_config::LmsSettings;
use crate::web_connector::{cookie_client, err_chain, host_of, normalize_url, resolve};
use chrono::{DateTime, Local, NaiveDateTime, TimeZone, Utc};
use regex::Regex;
use serde_json::Value;
use std::sync::{Arc, LazyLock};
use std::time::{Duration, Instant};
use tracing::{info, warn};

/// The itslearning mobile app's public OAuth client.
const CLIENT_ID: &str = "10ae9d30-1853-48ff-81cb-47b58a325685";

#[derive(Debug, Clone, PartialEq)]
pub struct CourseUpdate {
    pub id: String,
    /// "announcement", "material", "assignment" or "update".
    pub kind: &'static str,
    pub course: String,
    pub title: String,
    pub when: Option<NaiveDateTime>,
    /// Due date (assignments).
    pub due: Option<NaiveDateTime>,
    pub author: String,
    pub url: String,
}

impl CourseUpdate {
    pub fn line(&self) -> String {
        let mut s = String::new();
        if !self.course.is_empty() {
            s.push_str(&format!("[{}] ", self.course));
        }
        s.push_str(&self.title);
        if let Some(due) = self.due {
            s.push_str(&format!(" — due {}", due.format("%a %d %b %H:%M")));
        }
        if !self.author.is_empty() {
            s.push_str(&format!(" (by {})", self.author));
        }
        if let Some(t) = self.when {
            s.push_str(&format!(" · {}", t.format("%a %d %b %H:%M")));
        }
        s
    }
}

#[derive(Debug, Clone)]
struct Tokens {
    access: String,
    refresh: String,
    expires: Instant,
}

struct KeepAliveState {
    key: String,
    tokens: Option<Tokens>,
    last_request: Option<Instant>,
}

/// One persistent, self-renewing portal session.
#[derive(Clone)]
pub struct SessionKeepAliveManager {
    client: reqwest::Client,
    state: Arc<tokio::sync::Mutex<KeepAliveState>>,
}

impl Default for SessionKeepAliveManager {
    fn default() -> Self {
        Self { client: cookie_client(Duration::from_secs(25)), state: Arc::new(tokio::sync::Mutex::new(KeepAliveState { key: String::new(), tokens: None, last_request: None })) }
    }
}

fn base(cfg: &LmsSettings) -> String {
    normalize_url(&cfg.base_url).trim_end_matches('/').to_string()
}

fn interval(cfg: &LmsSettings) -> Duration {
    Duration::from_secs(u64::from(if cfg.keepalive_minutes == 0 { 8 } else { cfg.keepalive_minutes.clamp(1, 60) }) * 60)
}

impl SessionKeepAliveManager {
    async fn token_request(&self, cfg: &LmsSettings, form: &[(&str, &str)]) -> Result<Tokens, String> {
        let resp = self.client.post(format!("{}/restapi/oauth2/token", base(cfg))).form(form).send().await.map_err(err_chain)?;
        let status = resp.status().as_u16();
        let body: Value = resp.json().await.map_err(|_| format!("{} has no itslearning REST login (HTTP {status})", host_of(&cfg.base_url)))?;
        let access = body["access_token"].as_str().filter(|t| !t.is_empty()).ok_or_else(|| {
            let why = body["error_description"].as_str().or(body["error"].as_str()).unwrap_or("no access token");
            format!("itslearning refused the login ({why}); schools with Feide / Microsoft single sign-on need the RSS feed URL instead")
        })?;
        let expires = body["expires_in"].as_u64().unwrap_or(3600);
        Ok(Tokens {
            access: access.to_string(),
            refresh: body["refresh_token"].as_str().unwrap_or("").to_string(),
            expires: Instant::now() + Duration::from_secs(expires.saturating_sub(60)),
        })
    }

    async fn login(&self, cfg: &LmsSettings, password: &str) -> Result<Tokens, String> {
        if cfg.username.trim().is_empty() {
            return Err("itslearning needs a username for the REST login".into());
        }
        info!("[LMS] Logging in to {}", host_of(&cfg.base_url));
        self.token_request(cfg, &[("client_id", CLIENT_ID), ("grant_type", "password"), ("username", cfg.username.trim()), ("password", password)]).await
    }

    /// A valid access token: re-used while fresh, refreshed after the keep-alive interval of
    /// idleness (or when expired), logged in again when the refresh fails.
    pub async fn ensure_session(&self, cfg: &LmsSettings, password: Option<&str>, force: bool) -> Result<String, String> {
        let key = format!("{}|{}", base(cfg), cfg.username.trim());
        let mut st = self.state.lock().await;
        if st.key != key {
            st.key = key;
            st.tokens = None;
        }
        let idle = st.last_request.is_none_or(|t| t.elapsed() > interval(cfg));
        let expired = st.tokens.as_ref().is_none_or(|t| Instant::now() >= t.expires);
        if !force && !idle && !expired {
            st.last_request = Some(Instant::now());
            return Ok(st.tokens.as_ref().map(|t| t.access.clone()).unwrap_or_default());
        }
        let refreshed = match st.tokens.as_ref().filter(|t| !t.refresh.is_empty()) {
            Some(t) => {
                let refresh = t.refresh.clone();
                match self.token_request(cfg, &[("client_id", CLIENT_ID), ("grant_type", "refresh_token"), ("refresh_token", &refresh)]).await {
                    Ok(t) => Some(t),
                    Err(e) => {
                        warn!("[LMS] Token refresh failed ({e}); logging in again");
                        None
                    }
                }
            }
            None => None,
        };
        let tokens = match refreshed {
            Some(t) => t,
            None => self.login(cfg, password.ok_or("itslearning needs a password for the REST login")?).await?,
        };
        let access = tokens.access.clone();
        st.tokens = Some(tokens);
        st.last_request = Some(Instant::now());
        Ok(access)
    }

    /// GET with the session (access token added for REST paths); one re-login after a 401.
    pub async fn get(&self, cfg: &LmsSettings, password: Option<&str>, url: &str) -> Result<reqwest::Response, String> {
        let rest = url.contains("/restapi/");
        for attempt in 0..2 {
            let mut req = self.client.get(url);
            if rest && password.is_some() {
                let token = self.ensure_session(cfg, password, attempt > 0).await?;
                req = req.bearer_auth(&token).query(&[("access_token", token.as_str())]);
            }
            let resp = req.send().await.map_err(err_chain)?;
            self.state.lock().await.last_request = Some(Instant::now());
            if resp.status().as_u16() == 401 && attempt == 0 && rest {
                info!("[LMS] Session rejected; renewing it");
                continue;
            }
            if !resp.status().is_success() {
                return Err(format!("{} answered HTTP {}", host_of(url), resp.status().as_u16()));
            }
            return Ok(resp);
        }
        Err("itslearning keeps rejecting the session".into())
    }

    /// The heartbeat: refreshes an idle session (cheap `person` call) so the next real request
    /// never meets an expired one. No-op without a REST session.
    pub async fn heartbeat(&self, cfg: &LmsSettings, password: Option<&str>) {
        let due = {
            let st = self.state.lock().await;
            st.tokens.is_some() && st.last_request.is_some_and(|t| t.elapsed() > interval(cfg))
        };
        if !due {
            return;
        }
        let url = format!("{}/restapi/personal/person/v1", base(cfg));
        match self.get(cfg, password, &url).await {
            Ok(_) => info!("[LMS] Heartbeat: session kept alive"),
            Err(e) => warn!("[LMS] Heartbeat failed: {e}"),
        }
    }
}

// ----------------------------------------------------------------------- the client ----

#[derive(Clone, Default)]
pub struct Lms {
    pub keepalive: SessionKeepAliveManager,
}

fn field<'a>(v: &'a Value, keys: &[&str]) -> Option<&'a Value> {
    keys.iter().find_map(|k| v.get(*k).filter(|x| !x.is_null()))
}

fn text(v: &Value, keys: &[&str]) -> String {
    match field(v, keys) {
        Some(Value::String(s)) => crate::pim::plain_text(s),
        Some(Value::Number(n)) => n.to_string(),
        Some(Value::Object(o)) => o.get("FullName").or_else(|| o.get("Title")).and_then(Value::as_str).unwrap_or("").to_string(),
        _ => String::new(),
    }
}

/// RFC 3339, or a bare "2026-10-21T08:15:00(.000)(Z)".
fn date(v: &Value, keys: &[&str]) -> Option<NaiveDateTime> {
    let s = field(v, keys)?.as_str()?.trim();
    if let Ok(d) = DateTime::parse_from_rfc3339(s) {
        return Some(d.with_timezone(&Local).naive_local());
    }
    let body = s.trim_end_matches('Z');
    let body = body.split('.').next().unwrap_or(body);
    let t = NaiveDateTime::parse_from_str(body, "%Y-%m-%dT%H:%M:%S").ok()?;
    Some(if s.ends_with('Z') { Utc.from_utc_datetime(&t).with_timezone(&Local).naive_local() } else { t })
}

fn entities(v: &Value) -> Vec<Value> {
    v.get("EntityArray").or_else(|| v.get("Items")).and_then(Value::as_array).cloned().or_else(|| v.as_array().cloned()).unwrap_or_default()
}

fn kind_of(raw: &str) -> &'static str {
    let t = raw.to_lowercase();
    if t.contains("assignment") || t.contains("task") || t.contains("essay") || t.contains("aufgabe") {
        "assignment"
    } else if t.contains("bulletin") || t.contains("announcement") || t.contains("news") || t.contains("message") {
        "announcement"
    } else if t.contains("file") || t.contains("resource") || t.contains("document") || t.contains("folder") || t.contains("link") || t.contains("page") || t.contains("note") {
        "material"
    } else {
        "update"
    }
}

pub(crate) fn parse_notifications(v: &Value) -> Vec<CourseUpdate> {
    entities(v)
        .iter()
        .map(|n| {
            let title = text(n, &["Text", "Title", "Name"]);
            let kind = kind_of(&format!("{} {}", text(n, &["Type", "ElementType", "ContentType"]), title));
            CourseUpdate {
                id: format!("n-{}", text(n, &["NotificationId", "Id", "ElementId"])),
                kind,
                course: text(n, &["LocationTitle", "CourseTitle", "Course"]),
                title,
                when: date(n, &["PublishedDate", "PublishedDateUtc", "CreatedDate", "Date"]),
                due: None,
                author: text(n, &["PublishedBy", "Author"]),
                url: text(n, &["Url", "ContentUrl"]),
            }
        })
        .filter(|u| !u.title.is_empty())
        .collect()
}

pub(crate) fn parse_tasks(v: &Value) -> Vec<CourseUpdate> {
    entities(v)
        .iter()
        .map(|t| CourseUpdate {
            id: format!("t-{}", text(t, &["TaskId", "Id", "ElementId"])),
            kind: "assignment",
            course: text(t, &["LocationTitle", "CourseTitle", "Course"]),
            title: text(t, &["Title", "Name", "Text"]),
            when: None,
            due: date(t, &["Deadline", "DeadlineUtc", "DueDate"]),
            author: String::new(),
            url: text(t, &["Url", "ContentUrl"]),
        })
        .filter(|u| !u.title.is_empty())
        .collect()
}

static ITEM: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?s)<(item|entry)\b[^>]*>(.*?)</(?:item|entry)>").expect("valid feed regex"));

fn feed_tag(block: &str, name: &str) -> String {
    let re = Regex::new(&format!(r#"(?s)<{name}\b[^>]*?(?:href="([^"]*)"[^>]*)?(?:/>|>(.*?)</{name}>)"#)).expect("valid feed tag regex");
    re.captures(block)
        .map(|c| c.get(2).or(c.get(1)).map(|m| m.as_str()).unwrap_or(""))
        .map(|s| s.trim().trim_start_matches("<![CDATA[").trim_end_matches("]]>").trim())
        .map(|s| crate::pim::plain_text(&s.replace("&amp;", "&").replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&#39;", "'")))
        .unwrap_or_default()
}

/// RSS 2.0 / Atom items.
pub fn parse_feed(xml: &str, base_url: &str) -> Vec<CourseUpdate> {
    ITEM.captures_iter(xml)
        .map(|c| {
            let block = &c[2];
            let title = feed_tag(block, "title");
            let link = feed_tag(block, "link");
            let guid = [feed_tag(block, "guid"), feed_tag(block, "id")].into_iter().find(|g| !g.is_empty()).unwrap_or_else(|| link.clone());
            let published = [feed_tag(block, "pubDate"), feed_tag(block, "published"), feed_tag(block, "updated")].into_iter().find(|d| !d.is_empty()).unwrap_or_default();
            let when = DateTime::parse_from_rfc2822(&published).or_else(|_| DateTime::parse_from_rfc3339(&published)).ok().map(|d| d.with_timezone(&Local).naive_local());
            let category = feed_tag(block, "category");
            let description = feed_tag(block, "description");
            CourseUpdate {
                id: format!("f-{}", if guid.is_empty() { title.clone() } else { guid }),
                kind: kind_of(&format!("{category} {title} {description}")),
                course: category,
                title,
                when,
                due: None,
                author: [feed_tag(block, "author"), feed_tag(block, "dc:creator")].into_iter().find(|a| !a.is_empty()).unwrap_or_default(),
                url: if link.is_empty() { String::new() } else { resolve(base_url, &link) },
            }
        })
        .filter(|u| !u.title.is_empty())
        .collect()
}

impl Lms {
    /// Recent announcements, materials and assignments, newest first, optionally for one course.
    pub async fn updates(&self, cfg: &LmsSettings, password: Option<&str>, course_filter: Option<&str>) -> Result<Vec<CourseUpdate>, String> {
        let mut out: Vec<CourseUpdate> = Vec::new();
        let mut errors = Vec::new();
        // Baden-Württemberg schools sign in through Schule@BW, never with the REST password login.
        let sso = crate::itslearning_bw_sso::uses_schule_bw(cfg);
        if sso {
            match crate::itslearning_bw_sso::updates(cfg, password).await {
                Ok(u) => out.extend(u),
                Err(e) => errors.push(e),
            }
        }
        let rest_login = !sso && password.is_some() && !cfg.base_url.trim().is_empty() && !cfg.username.trim().is_empty();
        if rest_login {
            let base = base(cfg);
            let custom = cfg.rest_endpoint.trim();
            if custom.is_empty() {
                let notifications = format!("{base}/restapi/personal/notifications/v1?PageIndex=0&PageSize=40");
                match self.json(cfg, password, &notifications).await {
                    Ok(v) => out.extend(parse_notifications(&v)),
                    Err(e) => errors.push(e),
                }
                let tasks = format!("{base}/restapi/personal/tasks/v1?PageIndex=0&PageSize=40");
                match self.json(cfg, password, &tasks).await {
                    Ok(v) => out.extend(parse_tasks(&v)),
                    Err(e) => errors.push(e),
                }
            } else {
                let url = if custom.starts_with("http") { custom.to_string() } else { format!("{base}/{}", custom.trim_start_matches('/')) };
                match self.json(cfg, password, &url).await {
                    Ok(v) => {
                        let mut items = parse_notifications(&v);
                        items.extend(parse_tasks(&v).into_iter().filter(|t| t.due.is_some()));
                        out.extend(items);
                    }
                    Err(e) => errors.push(e),
                }
            }
        }
        if !cfg.feed_url.trim().is_empty() {
            let url = normalize_url(&cfg.feed_url);
            match self.keepalive.get(cfg, password, &url).await {
                Ok(resp) => match resp.text().await {
                    Ok(xml) => out.extend(parse_feed(&xml, &url)),
                    Err(e) => errors.push(err_chain(e)),
                },
                Err(e) => errors.push(e),
            }
        }
        if out.is_empty() && !errors.is_empty() {
            return Err(errors.join("; "));
        }
        if !sso && !rest_login && cfg.feed_url.trim().is_empty() {
            return Err("itslearning needs a username and password (REST login) or a feed URL".into());
        }
        if let Some(f) = course_filter.map(str::trim).filter(|f| !f.is_empty()) {
            let f = f.to_lowercase();
            let aliases = crate::pim::subject_aliases(&f);
            out.retain(|u| {
                let hay = format!("{} {}", u.course, u.title).to_lowercase();
                hay.contains(&f) || aliases.iter().any(|a| hay.contains(a.as_str()))
            });
        }
        let mut seen = std::collections::HashSet::new();
        out.retain(|u| seen.insert(u.id.clone()));
        out.sort_by_key(|a| std::cmp::Reverse(a.when.or(a.due)));
        Ok(out)
    }

    async fn json(&self, cfg: &LmsSettings, password: Option<&str>, url: &str) -> Result<Value, String> {
        let resp = self.keepalive.get(cfg, password, url).await?;
        resp.json().await.map_err(|e| format!("{} sent no JSON: {e}", host_of(url)))
    }

    pub async fn test(&self, cfg: &LmsSettings, password: Option<&str>) -> Result<String, String> {
        if password.is_some() && !cfg.base_url.trim().is_empty() && !crate::itslearning_bw_sso::uses_schule_bw(cfg) {
            self.keepalive.ensure_session(cfg, password, true).await?;
        }
        let n = self.updates(cfg, password, None).await?.len();
        Ok(format!("Connected to {}: {n} recent course updates; the session is kept alive every {} minutes.", host_of(if cfg.base_url.trim().is_empty() { &cfg.feed_url } else { &cfg.base_url }), interval(cfg).as_secs() / 60))
    }
}

/// Updates grouped for Sir: announcements, new materials, assignments with deadlines.
pub fn updates_text(updates: &[CourseUpdate], limit: usize) -> String {
    if updates.is_empty() {
        return "No recent course updates.".into();
    }
    let mut out = Vec::new();
    for (kind, heading) in [("announcement", "Announcements"), ("material", "New materials"), ("assignment", "Assignments & deadlines"), ("update", "Other updates")] {
        let items: Vec<String> = updates.iter().filter(|u| u.kind == kind).take(limit).map(|u| format!("- {}", u.line())).collect();
        if !items.is_empty() {
            out.push(format!("{heading}:"));
            out.extend(items);
        }
    }
    out.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn rest_notifications_and_tasks_are_read() {
        let n = json!({"EntityArray": [
            {"NotificationId": 11, "Text": "New file: Arbeitsblatt Optik.pdf", "PublishedDate": "2026-10-21T08:15:00Z", "PublishedBy": {"FullName": "Frau Müller"}, "Type": "File", "LocationTitle": "Physik 10b"},
            {"NotificationId": 12, "Text": "<p>Exkursion am Freitag</p>", "PublishedDate": "2026-10-20T10:00:00Z", "Type": "Bulletin", "LocationTitle": "Biologie"}
        ]});
        let items = parse_notifications(&n);
        assert_eq!(items.len(), 2);
        assert_eq!((items[0].kind, items[0].course.as_str(), items[0].author.as_str()), ("material", "Physik 10b", "Frau Müller"));
        assert_eq!((items[1].kind, items[1].title.as_str()), ("announcement", "Exkursion am Freitag"));
        let t = parse_tasks(&json!({"EntityArray": [{"TaskId": 5, "Title": "Lab report", "Deadline": "2026-10-23T22:00:00Z", "LocationTitle": "Chemie"}]}));
        assert_eq!((t[0].kind, t[0].title.as_str()), ("assignment", "Lab report"));
        assert!(t[0].due.is_some());
        assert!(updates_text(&items, 5).contains("New materials:\n- [Physik 10b] New file: Arbeitsblatt Optik.pdf"));
    }

    #[test]
    fn rss_and_atom_feeds_are_read() {
        let rss = r#"<rss><channel><item><title><![CDATA[Neue Aufgabe: Referat]]></title><link>/main.aspx?id=3</link><guid>abc</guid><pubDate>Wed, 21 Oct 2026 09:00:00 +0200</pubDate><category>Deutsch</category></item></channel></rss>"#;
        let items = parse_feed(rss, "https://school.itslearning.com/feed");
        assert_eq!((items[0].id.as_str(), items[0].kind, items[0].course.as_str()), ("f-abc", "assignment", "Deutsch"));
        assert_eq!(items[0].url, "https://school.itslearning.com/main.aspx?id=3");
        let atom = r#"<feed><entry><title>Course news</title><link href="https://x.org/n/1"/><id>n1</id><updated>2026-10-21T07:00:00Z</updated></entry></feed>"#;
        let items = parse_feed(atom, "https://x.org");
        assert_eq!((items[0].title.as_str(), items[0].url.as_str(), items[0].kind), ("Course news", "https://x.org/n/1", "announcement"));
    }
}
