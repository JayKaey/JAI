//! itslearning for Baden-Württemberg schools (`bws.itslearning.com`) through the federated
//! **"Login über Schule@BW"** single sign-on, in pure Rust.
//!
//! A direct POST to itslearning's own login form fails for these accounts: the school's users live
//! in the Schule@BW identity provider. The flow (`login`) walks the whole federation chain on one
//! cookie session (`reqwest` with a shared cookie jar, up to 20 redirects per hop):
//!
//! 1. `GET https://bws.itslearning.com/index.aspx`;
//! 2. find the "Login über Schule@BW" trigger — a link, a submit button or an ASP.NET postback
//!    (`__VIEWSTATE`, `__EVENTVALIDATION`, `__EVENTTARGET`) — and follow it to the IdP;
//! 3. parse the IdP's login form (Keycloak `kc-form-login`, Shibboleth `j_username`, ADFS …) with
//!    its hidden CSRF / execution fields and POST the username and password;
//! 4. POST the returned `SAMLResponse` + `RelayState` (or an OIDC `code` / `id_token` form) to
//!    itslearning's assertion consumer service, confirming an attribute-release page on the way;
//! 5. stop on itslearning with its session cookies.
//!
//! If the IdP serves a page that only works with JavaScript, `edge_login` drives a headless
//! Microsoft Edge over the DevTools protocol (a minimal WebSocket client below): it clicks the
//! Schule@BW link, fills the form, submits, and hands the resulting itslearning cookies to the
//! same cookie jar.
//!
//! With the session, `fetch_updates` obtains an API token the way the itslearning app does for
//! single-sign-on schools (the OAuth authorize endpoint answers the logged-in session with a code
//! for the app's redirect URI), then reads the personal tasks and notifications; when that is
//! refused it tries the REST endpoints with the session cookies alone.
//!
//! The username lives in the itslearning settings; the password only in JAI's encrypted vault
//! (`pim_config` token "itslearning"). Nothing here stores either.

use crate::hud_board::{Bar, Block, BlockKind, Note, Report};
use crate::lms_client::CourseUpdate;
use crate::pim_config::LmsSettings;
use reqwest::cookie::Jar;
use reqwest::Url;
use scraper::{ElementRef, Html, Selector};
use serde_json::{json, Value};
use std::sync::{Arc, LazyLock};
use std::time::{Duration, Instant};
use tracing::{info, warn};

/// The Baden-Württemberg itslearning portal.
pub const BWS_BASE: &str = "https://bws.itslearning.com";
#[cfg(test)]
pub const BWS_PORTAL: &str = "https://bws.itslearning.com/index.aspx";
/// The settings value that selects this flow (`LmsSettings::auth`).
pub const AUTH_SCHULE_BW: &str = "schule_bw";
/// The itslearning app's public OAuth client and its redirect URI.
const CLIENT_ID: &str = "10ae9d30-1853-48ff-81cb-47b58a325685";
const APP_REDIRECT: &str = "itsl-itslearning://login";
/// A session is reused this long before logging in again.
const SESSION_TTL: Duration = Duration::from_secs(8 * 60);
const MAX_HOPS: usize = 14;
const JS_NEEDED: &str = "the Schule@BW page needs JavaScript";

/// Whether these settings use the Schule@BW single sign-on.
pub fn uses_schule_bw(cfg: &LmsSettings) -> bool {
    cfg.auth.trim().eq_ignore_ascii_case(AUTH_SCHULE_BW) || crate::web_connector::host_of(&cfg.base_url) == "bws.itslearning.com"
}

// ------------------------------------------------------------------------------- parsing ----

fn sel(s: &str) -> Selector {
    Selector::parse(s).expect("valid selector")
}

fn resolve(page: &str, href: &str) -> String {
    crate::web_connector::resolve(page, &href.replace("&amp;", "&"))
}

fn host(url: &str) -> String {
    Url::parse(url).ok().and_then(|u| u.host_str().map(str::to_lowercase)).unwrap_or_default()
}

/// A form ready to submit: absolute action URL and its fields in document order.
#[derive(Debug, Clone, PartialEq)]
pub struct FormPost {
    pub action: String,
    pub fields: Vec<(String, String)>,
}

/// How to reach the identity provider from itslearning's login page.
#[derive(Debug, Clone, PartialEq)]
pub enum Trigger {
    Link(String),
    Post(FormPost),
}

/// The IdP's username / password form.
#[derive(Debug, Clone, PartialEq)]
pub struct LoginForm {
    pub post: FormPost,
    pub user_field: String,
    pub pass_field: String,
}

/// What a page of the chain is.
#[derive(Debug, Clone, PartialEq)]
pub enum Page {
    /// Logged in on itslearning.
    Landed,
    /// "Login über Schule@BW" found.
    Federation(Trigger),
    Login(LoginForm),
    /// A form that posts itself (SAMLResponse / SAMLRequest / OIDC form_post / consent).
    AutoPost(FormPost),
    /// The IdP refused the credentials.
    Failed(String),
    /// Only works in a browser with JavaScript.
    JavaScript,
    Unknown,
}

/// The hidden / text / checked inputs of a form (and a named submit button, if given).
fn form_fields(form: ElementRef, submit: Option<ElementRef>) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for input in form.select(&sel("input")) {
        let v = input.value();
        let Some(name) = v.attr("name") else { continue };
        let kind = v.attr("type").unwrap_or("text").to_lowercase();
        match kind.as_str() {
            "submit" | "button" | "image" | "reset" | "file" => continue,
            "checkbox" | "radio" if v.attr("checked").is_none() => continue,
            _ => out.push((name.to_string(), v.attr("value").unwrap_or("").to_string())),
        }
    }
    if let Some(button) = submit {
        if let Some(name) = button.value().attr("name") {
            out.push((name.to_string(), button.value().attr("value").unwrap_or("").to_string()));
        }
    }
    out
}

fn form_action(form: ElementRef, page_url: &str) -> String {
    match form.value().attr("action").map(str::trim).filter(|a| !a.is_empty()) {
        Some(a) => resolve(page_url, a),
        None => page_url.to_string(),
    }
}

fn text_of(e: ElementRef) -> String {
    let mut t = e.text().collect::<String>();
    for attr in ["value", "title", "aria-label", "alt"] {
        if let Some(v) = e.value().attr(attr) {
            t.push(' ');
            t.push_str(v);
        }
    }
    t.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase()
}

fn names_schule_bw(text: &str) -> bool {
    let t = text.to_lowercase();
    t.contains("schule@bw") || t.contains("schule @ bw") || t.contains("schule-bw") || t.contains("schulebw") || t.contains("schule bw")
}

/// `__doPostBack('ctl00$…$federatedLoginButton','')` → the event target.
fn postback_target(js: &str) -> Option<String> {
    let start = js.find("__doPostBack(")? + "__doPostBack(".len();
    let rest = &js[start..];
    let quote = rest.chars().next().filter(|c| *c == '\'' || *c == '"')?;
    let end = rest[1..].find(quote)?;
    Some(rest[1..1 + end].replace("\\$", "$"))
}

/// The "Login über Schule@BW" trigger on itslearning's login page.
pub fn find_federation_trigger(html: &str, page_url: &str) -> Option<Trigger> {
    let doc = Html::parse_document(html);
    // Links: by their text, or by an href / onclick that points at the federation.
    for a in doc.select(&sel("a")) {
        let text = text_of(a);
        let href = a.value().attr("href").unwrap_or("");
        let onclick = a.value().attr("onclick").unwrap_or("");
        if !(names_schule_bw(&text) || names_schule_bw(href)) {
            continue;
        }
        let script = if href.starts_with("javascript:") { href } else { onclick };
        if let Some(target) = postback_target(script) {
            let form = doc.select(&sel("form")).next()?;
            let mut fields = form_fields(form, None);
            fields.retain(|(k, _)| k != "__EVENTTARGET" && k != "__EVENTARGUMENT");
            fields.push(("__EVENTTARGET".into(), target));
            fields.push(("__EVENTARGUMENT".into(), String::new()));
            return Some(Trigger::Post(FormPost { action: form_action(form, page_url), fields }));
        }
        if !href.is_empty() && !href.starts_with('#') && !href.starts_with("javascript:") {
            return Some(Trigger::Link(resolve(page_url, href)));
        }
    }
    // Submit buttons / inputs inside a form.
    for form in doc.select(&sel("form")) {
        for b in form.select(&sel("button, input[type=submit], input[type=image], input[type=button]")) {
            if !names_schule_bw(&text_of(b)) {
                continue;
            }
            let onclick = b.value().attr("onclick").unwrap_or("");
            let mut fields = form_fields(form, Some(b));
            if let Some(target) = postback_target(onclick) {
                fields.retain(|(k, _)| k != "__EVENTTARGET" && k != "__EVENTARGUMENT");
                fields.push(("__EVENTTARGET".into(), target));
                fields.push(("__EVENTARGUMENT".into(), String::new()));
            }
            return Some(Trigger::Post(FormPost { action: form_action(form, page_url), fields }));
        }
    }
    // A <select> of federations (pick the Schule@BW option) with its form.
    for form in doc.select(&sel("form")) {
        for select in form.select(&sel("select")) {
            let Some(name) = select.value().attr("name") else { continue };
            if let Some(option) = select.select(&sel("option")).find(|o| names_schule_bw(&text_of(*o))) {
                let mut fields = form_fields(form, None);
                fields.push((name.to_string(), option.value().attr("value").unwrap_or("").to_string()));
                return Some(Trigger::Post(FormPost { action: form_action(form, page_url), fields }));
            }
        }
    }
    None
}

/// The IdP's login form: the form holding a password input.
pub fn parse_login_form(html: &str, page_url: &str) -> Option<LoginForm> {
    let doc = Html::parse_document(html);
    for form in doc.select(&sel("form")) {
        let Some(pass) = form.select(&sel("input[type=password]")).next() else { continue };
        let pass_field = pass.value().attr("name")?.to_string();
        let user = form
            .select(&sel("input"))
            .find(|i| {
                let name = i.value().attr("name").unwrap_or("").to_lowercase();
                let kind = i.value().attr("type").unwrap_or("text").to_lowercase();
                matches!(kind.as_str(), "text" | "email") || ["username", "j_username", "user", "login", "email", "userid"].iter().any(|n| name.ends_with(n))
            })
            .filter(|i| i.value().attr("type").unwrap_or("text") != "password")?;
        let user_field = user.value().attr("name")?.to_string();
        let submit = form.select(&sel("button[type=submit][name], input[type=submit][name]")).next();
        let mut fields = form_fields(form, submit);
        fields.retain(|(k, _)| k != &user_field && k != &pass_field);
        return Some(LoginForm { post: FormPost { action: form_action(form, page_url), fields }, user_field, pass_field });
    }
    None
}

/// A self-submitting form: SAML (`SAMLResponse` / `SAMLRequest` + `RelayState`), OIDC form_post
/// (`code`, `id_token`, `state`) or a Shibboleth attribute-release page (`_eventId_proceed`).
pub fn parse_auto_post(html: &str, page_url: &str) -> Option<FormPost> {
    let doc = Html::parse_document(html);
    for form in doc.select(&sel("form")) {
        let names: Vec<String> = form.select(&sel("input, button")).filter_map(|i| i.value().attr("name").map(str::to_string)).collect();
        let saml = names.iter().any(|n| n == "SAMLResponse" || n == "SAMLRequest");
        let oidc = names.iter().any(|n| n == "code" || n == "id_token") && names.iter().any(|n| n == "state");
        let consent = names.iter().any(|n| n == "_eventId_proceed" || n.starts_with("_shib_idp_consent"));
        if saml || oidc || consent {
            let proceed = form.select(&sel("[name=_eventId_proceed]")).next();
            return Some(FormPost { action: form_action(form, page_url), fields: form_fields(form, proceed) });
        }
    }
    None
}

/// The IdP's "wrong username or password" message, if the page shows one.
pub fn login_error(html: &str) -> Option<String> {
    let doc = Html::parse_document(html);
    for s in ["#input-error", ".kc-feedback-text", "#kc-error-message", ".alert-error", ".alert-danger", ".form-error", ".output--error", "#errorText", ".error-message"] {
        if let Some(e) = doc.select(&sel(s)).next() {
            let t = e.text().collect::<String>().split_whitespace().collect::<Vec<_>>().join(" ");
            if !t.is_empty() {
                return Some(t);
            }
        }
    }
    let lower = html.to_lowercase();
    ["ungültiger benutzername oder passwort", "invalid username or password", "anmeldung fehlgeschlagen", "benutzername oder passwort ist falsch", "the password you entered was incorrect"]
        .iter()
        .find(|m| lower.contains(*m))
        .map(|m| m.to_string())
}

fn has_password_input(html: &str) -> bool {
    Html::parse_document(html).select(&sel("input[type=password]")).next().is_some()
}

/// A page that cannot be walked without running its scripts.
pub fn needs_javascript(html: &str) -> bool {
    let doc = Html::parse_document(html);
    let lower = html.to_lowercase();
    doc.select(&sel("form")).next().is_none()
        && (lower.contains("enable javascript") || lower.contains("javascript aktivieren") || lower.contains("javascript ist deaktiviert") || (doc.select(&sel("noscript")).next().is_some() && doc.select(&sel("script")).count() > 0 && doc.select(&sel("a[href]")).count() < 3))
}

/// Logged in: on the itslearning host, no password field, and the signed-in shell around.
pub fn is_landed(html: &str, url: &str, base_host: &str) -> bool {
    if host(url) != base_host || has_password_input(html) {
        return false;
    }
    let lower = html.to_lowercase();
    ["logout", "log out", "abmelden", "dashboardmenu", "personal/dashboard", "main.aspx", "itsl-personal", "data-person-id"].iter().any(|m| lower.contains(m))
}

/// What a page of the chain is (`submitted`: the credentials went out already).
pub fn classify(html: &str, url: &str, base_host: &str, submitted: bool) -> Page {
    if is_landed(html, url, base_host) {
        return Page::Landed;
    }
    if let Some(post) = parse_auto_post(html, url) {
        return Page::AutoPost(post);
    }
    if submitted {
        if let Some(e) = login_error(html) {
            return Page::Failed(e);
        }
    }
    // On itslearning's own page the federation comes first (its local form is not for us).
    if host(url) == base_host {
        if let Some(t) = find_federation_trigger(html, url) {
            return Page::Federation(t);
        }
    }
    if let Some(form) = parse_login_form(html, url) {
        return Page::Login(form);
    }
    if let Some(t) = find_federation_trigger(html, url) {
        return Page::Federation(t);
    }
    if needs_javascript(html) {
        return Page::JavaScript;
    }
    Page::Unknown
}

// --------------------------------------------------------------------------------- login ----

/// A logged-in itslearning session.
#[derive(Clone)]
pub struct BwSession {
    pub client: reqwest::Client,
    pub jar: Arc<Jar>,
    pub base: String,
    pub since: Instant,
    /// "http" or "edge".
    pub via: &'static str,
}

fn client_for(jar: &Arc<Jar>, follow: bool) -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .cookie_provider(Arc::clone(jar))
        .user_agent(crate::web_connector::USER_AGENT)
        .redirect(if follow { reqwest::redirect::Policy::limited(20) } else { reqwest::redirect::Policy::none() })
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|e| e.to_string())
}

async fn read(resp: reqwest::Response) -> Result<(String, String), String> {
    let url = resp.url().to_string();
    let html = resp.text().await.map_err(crate::web_connector::err_chain)?;
    Ok((url, html))
}

async fn submit(client: &reqwest::Client, post: &FormPost) -> Result<(String, String), String> {
    read(client.post(&post.action).form(&post.fields).send().await.map_err(crate::web_connector::err_chain)?).await
}

/// The plain-HTTP federation chain (steps 1-5).
pub async fn login(base: &str, username: &str, password: &str) -> Result<BwSession, String> {
    let jar = Arc::new(Jar::default());
    let client = client_for(&jar, true)?;
    let base = base.trim_end_matches('/').to_string();
    let base_host = host(&base);
    let (mut url, mut html) = read(client.get(format!("{base}/index.aspx")).send().await.map_err(crate::web_connector::err_chain)?).await?;
    let mut submitted = false;
    for hop in 0..MAX_HOPS {
        let page = classify(&html, &url, &base_host, submitted);
        info!("[SCHULE@BW] Step {hop}: {} at {}", page_name(&page), host(&url));
        (url, html) = match page {
            Page::Landed => return Ok(BwSession { client, jar, base, since: Instant::now(), via: "http" }),
            Page::Failed(e) => return Err(format!("Schule@BW refused the login: {e}")),
            Page::JavaScript => return Err(JS_NEEDED.into()),
            Page::Unknown => return Err(format!("the sign-in stopped at an unexpected page on {}", host(&url))),
            Page::Federation(Trigger::Link(link)) => read(client.get(&link).send().await.map_err(crate::web_connector::err_chain)?).await?,
            Page::Federation(Trigger::Post(post)) | Page::AutoPost(post) => submit(&client, &post).await?,
            Page::Login(form) => {
                if submitted {
                    return Err("Schule@BW asked for the password again (wrong username or password?)".into());
                }
                submitted = true;
                let mut post = form.post.clone();
                post.fields.push((form.user_field.clone(), username.to_string()));
                post.fields.push((form.pass_field.clone(), password.to_string()));
                submit(&client, &post).await?
            }
        };
    }
    Err("the Schule@BW sign-in did not finish".into())
}

fn page_name(p: &Page) -> &'static str {
    match p {
        Page::Landed => "signed in",
        Page::Federation(_) => "Schule@BW trigger",
        Page::Login(_) => "IdP login form",
        Page::AutoPost(_) => "SAML / OIDC hand-off",
        Page::Failed(_) => "login refused",
        Page::JavaScript => "JavaScript page",
        Page::Unknown => "unknown page",
    }
}

/// Logs in, trying the username as written, then in lower case if the IdP is case-sensitive, then
/// the headless Edge fallback when a page needs JavaScript.
pub async fn sign_in(base: &str, username: &str, password: &str) -> Result<BwSession, String> {
    let mut names = vec![username.trim().to_string()];
    let lower = username.trim().to_lowercase();
    if lower != names[0] {
        names.push(lower);
    }
    let mut last = String::new();
    for name in &names {
        match login(base, name, password).await {
            Ok(s) => return Ok(s),
            Err(e) if e.starts_with("Schule@BW refused") || e.contains("asked for the password again") => last = e,
            Err(e) => {
                warn!("[SCHULE@BW] Plain sign-in failed ({e}); trying headless Edge");
                return edge_login(base, name, password).await.map_err(|edge| format!("{e}; the Edge fallback failed too: {edge}"));
            }
        }
    }
    Err(last)
}

static SESSION: LazyLock<tokio::sync::Mutex<Option<BwSession>>> = LazyLock::new(|| tokio::sync::Mutex::new(None));

/// The cached session (re-signed-in after `SESSION_TTL`).
pub async fn session(cfg: &LmsSettings, password: &str) -> Result<BwSession, String> {
    let mut slot = SESSION.lock().await;
    if let Some(s) = slot.as_ref().filter(|s| s.since.elapsed() < SESSION_TTL) {
        return Ok(s.clone());
    }
    let base = if cfg.base_url.trim().is_empty() { BWS_BASE.to_string() } else { crate::web_connector::normalize_url(&cfg.base_url).trim_end_matches('/').to_string() };
    if cfg.username.trim().is_empty() {
        return Err("itslearning needs your Schule@BW username in its settings".into());
    }
    let started = Instant::now();
    let s = sign_in(&base, &cfg.username, password).await?;
    info!("[SCHULE@BW] Signed in to {} via {} in {} ms", host(&base), s.via, started.elapsed().as_millis());
    *slot = Some(s.clone());
    Ok(s)
}

// ------------------------------------------------------------------------------ the data ----

/// `itsl-itslearning://login?code=…` → the code.
pub fn code_from_redirect(location: &str) -> Option<String> {
    let query = location.split_once('?')?.1;
    query.split('&').find_map(|kv| kv.strip_prefix("code=")).map(|c| urlencoding::decode(c).map(|s| s.into_owned()).unwrap_or_else(|_| c.to_string()))
}

/// The "allow the app access" form of the OAuth authorize page, with its allow button.
fn allow_form(html: &str, page: &str) -> Option<FormPost> {
    let doc = Html::parse_document(html);
    let form = doc.select(&sel("form")).next()?;
    let allow = form.select(&sel("input[type=submit], button")).find(|b| {
        let t = text_of(*b);
        t.contains("allow") || t.contains("erlauben") || t.contains("zulassen") || t.contains("accept")
    });
    Some(FormPost { action: form_action(form, page), fields: form_fields(form, allow) })
}

/// The app's OAuth code for the signed-in session, exchanged for an access token.
async fn app_token(s: &BwSession) -> Result<String, String> {
    let client = client_for(&s.jar, false)?;
    let mut url = format!("{}/oauth2/authorize.aspx?client_id={CLIENT_ID}&state=jarvis&response_type=code&redirect_uri={}", s.base, urlencoding::encode(APP_REDIRECT));
    let mut code = None;
    for _ in 0..8 {
        let resp = client.get(&url).send().await.map_err(crate::web_connector::err_chain)?;
        if resp.status().is_redirection() {
            let location = resp.headers().get("location").and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
            if location.starts_with("itsl-itslearning:") {
                code = code_from_redirect(&location);
                break;
            }
            url = resolve(&url, &location);
            continue;
        }
        // An "allow access" page: confirm it once.
        let (page, html) = read(resp).await?;
        let Some(post) = allow_form(&html, &page) else { break };
        let resp = client.post(&post.action).form(&post.fields).send().await.map_err(crate::web_connector::err_chain)?;
        match resp.headers().get("location").and_then(|v| v.to_str().ok()) {
            Some(l) if l.starts_with("itsl-itslearning:") => {
                code = code_from_redirect(l);
                break;
            }
            Some(l) => url = resolve(&post.action, l),
            None => break,
        }
    }
    let code = code.ok_or("itslearning did not hand out an app code for the session")?;
    let resp = s
        .client
        .post(format!("{}/restapi/oauth2/token", s.base))
        .form(&[("client_id", CLIENT_ID), ("grant_type", "authorization_code"), ("code", code.as_str())])
        .send()
        .await
        .map_err(crate::web_connector::err_chain)?;
    let body: Value = resp.json().await.map_err(|e| e.to_string())?;
    body["access_token"].as_str().map(str::to_string).ok_or_else(|| format!("no access token ({})", body["error"].as_str().unwrap_or("unknown error")))
}

async fn get_json(s: &BwSession, token: Option<&str>, path: &str) -> Result<Value, String> {
    let mut req = s.client.get(format!("{}{path}", s.base)).header("Accept", "application/json");
    if let Some(t) = token {
        req = req.bearer_auth(t);
    }
    let resp = req.send().await.map_err(crate::web_connector::err_chain)?;
    let status = resp.status();
    if !status.is_success() {
        return Err(format!("{path}: HTTP {status}"));
    }
    resp.json().await.map_err(|_| format!("{path}: not JSON (the session is not accepted there)"))
}

/// Tasks and notifications for the signed-in session.
pub async fn fetch_updates(s: &BwSession) -> Result<Vec<CourseUpdate>, String> {
    let token = match app_token(s).await {
        Ok(t) => Some(t),
        Err(e) => {
            warn!("[SCHULE@BW] No app token ({e}); reading with the session cookies");
            None
        }
    };
    let mut out = Vec::new();
    let mut errors = Vec::new();
    for (path, tasks) in [("/restapi/personal/tasks/v1?PageIndex=0&PageSize=40", true), ("/restapi/personal/notifications/v1?PageIndex=0&PageSize=40", false)] {
        match get_json(s, token.as_deref(), path).await {
            Ok(v) => out.extend(if tasks { crate::lms_client::parse_tasks(&v) } else { crate::lms_client::parse_notifications(&v) }),
            Err(e) => errors.push(e),
        }
    }
    if out.is_empty() && !errors.is_empty() {
        return Err(format!("signed in through Schule@BW, but itslearning's course data refused the session: {}", errors.join("; ")));
    }
    Ok(out)
}

/// Everything for `Lms::updates` when the settings use Schule@BW.
pub async fn updates(cfg: &LmsSettings, password: Option<&str>) -> Result<Vec<CourseUpdate>, String> {
    let password = password.filter(|p| !p.is_empty()).ok_or("itslearning needs your Schule@BW password — enter it once in the Integrations Hub (it is kept encrypted)")?;
    crate::core_sphere_3d::pulse("itslearning_bw_sso.rs");
    let s = session(cfg, password).await?;
    match fetch_updates(&s).await {
        Ok(u) => Ok(u),
        Err(e) => {
            // A stale session: sign in once more.
            SESSION.lock().await.take();
            let s = session(cfg, password).await.map_err(|again| format!("{e}; {again}"))?;
            fetch_updates(&s).await
        }
    }
}

// ----------------------------------------------------------------------------- the board ----

/// The `ITSLEARNING // TASKS & COURSES` board: open tasks as a big figure, the tasks and the
/// announcements as note stacks, and the items per course as a bar chart.
pub fn board_report(updates: &[CourseUpdate], now: chrono::NaiveDateTime) -> Report {
    let mut tasks: Vec<&CourseUpdate> = updates.iter().filter(|u| u.kind == "assignment").collect();
    tasks.sort_by_key(|u| u.due.unwrap_or(chrono::NaiveDateTime::MAX));
    let open: Vec<&CourseUpdate> = tasks.iter().copied().filter(|u| u.due.is_none_or(|d| d >= now)).collect();
    let mut blocks = Vec::new();
    let next = open.iter().find(|u| u.due.is_some());
    blocks.push(Block::new(
        "ITSLEARNING // OPEN TASKS",
        "SCHULE@BW",
        "SOURCE: BWS.ITSLEARNING.COM",
        BlockKind::HeroFigure {
            header: "Open tasks and deadlines".into(),
            figure: open.len().to_string(),
            note: next.map(|u| format!("Next: {} — due {}", u.title, u.due.map(|d| d.format("%a %d %b %H:%M").to_string()).unwrap_or_default())).unwrap_or_else(|| "Nothing due.".into()),
        },
    ));
    let task_notes: Vec<Note> = open
        .iter()
        .take(8)
        .map(|u| Note {
            headline: u.title.clone(),
            body: format!("{}{}", if u.course.is_empty() { String::new() } else { format!("{} · ", u.course) }, u.due.map(|d| format!("due {}", d.format("%A %d %B, %H:%M"))).unwrap_or_else(|| "no deadline".into())),
        })
        .collect();
    if !task_notes.is_empty() {
        blocks.push(Block::new("ITSLEARNING // TASKS & DEADLINES", "FRISTEN", "SOURCE: BWS.ITSLEARNING.COM", BlockKind::NoteStack { notes: task_notes }));
    }
    let news: Vec<Note> = updates
        .iter()
        .filter(|u| u.kind != "assignment")
        .take(8)
        .map(|u| Note { headline: u.title.clone(), body: format!("{}{}", u.course, u.when.map(|w| format!(" · {}", w.format("%a %d %b %H:%M"))).unwrap_or_default()) })
        .collect();
    if !news.is_empty() {
        blocks.push(Block::new("ITSLEARNING // MITTEILUNGEN", "COURSES", "SOURCE: BWS.ITSLEARNING.COM", BlockKind::NoteStack { notes: news }));
    }
    let mut per_course: Vec<(String, usize)> = Vec::new();
    for u in updates.iter().filter(|u| !u.course.is_empty()) {
        match per_course.iter_mut().find(|(c, _)| *c == u.course) {
            Some((_, n)) => *n += 1,
            None => per_course.push((u.course.clone(), 1)),
        }
    }
    per_course.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
    if per_course.len() > 1 {
        blocks.push(Block::new(
            "ITSLEARNING // ITEMS PER COURSE",
            "KURSE",
            "SOURCE: BWS.ITSLEARNING.COM",
            BlockKind::BarChart { unit: String::new(), bars: per_course.iter().take(8).map(|(c, n)| Bar { label: c.chars().take(14).collect(), value: *n as f32 }).collect(), caption: "Tasks, materials and announcements per course.".into() },
        ));
    }
    let sources: Vec<String> = updates.iter().map(|u| u.url.clone()).filter(|u| !u.is_empty()).collect();
    Report { topic: "itslearning".into(), created_at: now.format("%Y-%m-%d %H:%M:%S").to_string(), blocks, sources, id: None }
}

// ------------------------------------------------------------------- headless Edge (CDP) ----

/// A WebSocket frame (client frames are masked).
pub fn encode_frame(payload: &[u8], opcode: u8, mask: [u8; 4]) -> Vec<u8> {
    let mut out = vec![0x80 | opcode];
    match payload.len() {
        n if n < 126 => out.push(0x80 | n as u8),
        n if n <= u16::MAX as usize => {
            out.push(0x80 | 126);
            out.extend_from_slice(&(n as u16).to_be_bytes());
        }
        n => {
            out.push(0x80 | 127);
            out.extend_from_slice(&(n as u64).to_be_bytes());
        }
    }
    out.extend_from_slice(&mask);
    out.extend(payload.iter().enumerate().map(|(i, b)| b ^ mask[i % 4]));
    out
}

/// One frame from the front of `buf`: (fin, opcode, payload, bytes used); None until complete.
pub fn decode_frame(buf: &[u8]) -> Option<(bool, u8, Vec<u8>, usize)> {
    if buf.len() < 2 {
        return None;
    }
    let (fin, opcode, masked) = (buf[0] & 0x80 != 0, buf[0] & 0x0F, buf[1] & 0x80 != 0);
    let (len, mut at) = match buf[1] & 0x7F {
        126 => (u16::from_be_bytes(buf.get(2..4)?.try_into().ok()?) as usize, 4),
        127 => (u64::from_be_bytes(buf.get(2..10)?.try_into().ok()?) as usize, 10),
        n => (n as usize, 2),
    };
    let mask = if masked {
        let m: [u8; 4] = buf.get(at..at + 4)?.try_into().ok()?;
        at += 4;
        Some(m)
    } else {
        None
    };
    let data = buf.get(at..at + len)?;
    let payload = match mask {
        Some(m) => data.iter().enumerate().map(|(i, b)| b ^ m[i % 4]).collect(),
        None => data.to_vec(),
    };
    Some((fin, opcode, payload, at + len))
}

/// A minimal WebSocket client for Edge's local DevTools endpoint.
struct DevTools {
    stream: tokio::net::TcpStream,
    buf: Vec<u8>,
    next_id: u64,
}

impl DevTools {
    async fn connect(ws_url: &str) -> Result<Self, String> {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let rest = ws_url.strip_prefix("ws://").ok_or("not a ws:// URL")?;
        let (addr, path) = rest.split_once('/').map(|(a, p)| (a, format!("/{p}"))).unwrap_or((rest, "/".into()));
        let mut stream = tokio::net::TcpStream::connect(addr).await.map_err(|e| e.to_string())?;
        let key = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, uuid::Uuid::new_v4().as_bytes());
        let request = format!("GET {path} HTTP/1.1\r\nHost: {addr}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\n\r\n");
        stream.write_all(request.as_bytes()).await.map_err(|e| e.to_string())?;
        let mut buf = Vec::new();
        let mut chunk = [0u8; 4096];
        let head_end = loop {
            let n = stream.read(&mut chunk).await.map_err(|e| e.to_string())?;
            if n == 0 {
                return Err("DevTools closed the connection".into());
            }
            buf.extend_from_slice(&chunk[..n]);
            if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                break i + 4;
            }
        };
        if !String::from_utf8_lossy(&buf[..head_end]).starts_with("HTTP/1.1 101") {
            return Err("DevTools refused the WebSocket upgrade".into());
        }
        Ok(Self { stream, buf: buf[head_end..].to_vec(), next_id: 0 })
    }

    async fn recv(&mut self) -> Result<String, String> {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let mut message = Vec::new();
        loop {
            if let Some((fin, opcode, payload, used)) = decode_frame(&self.buf) {
                self.buf.drain(..used);
                match opcode {
                    0x9 => {
                        let pong = encode_frame(&payload, 0xA, [7, 1, 3, 9]);
                        self.stream.write_all(&pong).await.map_err(|e| e.to_string())?;
                    }
                    0x8 => return Err("DevTools closed the session".into()),
                    _ => {
                        message.extend_from_slice(&payload);
                        if fin {
                            return Ok(String::from_utf8_lossy(&message).into_owned());
                        }
                    }
                }
                continue;
            }
            let mut chunk = [0u8; 65_536];
            let n = self.stream.read(&mut chunk).await.map_err(|e| e.to_string())?;
            if n == 0 {
                return Err("DevTools closed the connection".into());
            }
            self.buf.extend_from_slice(&chunk[..n]);
        }
    }

    /// A DevTools command and its result (events in between are skipped).
    async fn call(&mut self, method: &str, params: Value) -> Result<Value, String> {
        use tokio::io::AsyncWriteExt;
        self.next_id += 1;
        let id = self.next_id;
        let text = json!({ "id": id, "method": method, "params": params }).to_string();
        let mask = *uuid::Uuid::new_v4().as_bytes().first_chunk::<4>().unwrap_or(&[1, 2, 3, 4]);
        self.stream.write_all(&encode_frame(text.as_bytes(), 0x1, mask)).await.map_err(|e| e.to_string())?;
        let deadline = Instant::now() + Duration::from_secs(20);
        while Instant::now() < deadline {
            let raw = tokio::time::timeout(Duration::from_secs(20), self.recv()).await.map_err(|_| format!("{method}: no answer"))??;
            let v: Value = serde_json::from_str(&raw).unwrap_or(Value::Null);
            if v["id"].as_u64() == Some(id) {
                if let Some(e) = v.get("error") {
                    return Err(format!("{method}: {e}"));
                }
                return Ok(v["result"].clone());
            }
        }
        Err(format!("{method}: no answer"))
    }
}

/// The page script one polling step runs: Schule@BW link → fill the IdP form → confirm → done.
pub fn step_script(username: &str, password: &str) -> String {
    let (u, p) = (serde_json::to_string(username).unwrap_or_default(), serde_json::to_string(password).unwrap_or_default());
    format!(
        r#"(() => {{
  const user = {u}, pass = {p};
  const onPortal = location.host.includes('itslearning');
  const pw = document.querySelector('input[type=password]');
  const label = e => ((e.innerText || '') + ' ' + (e.value || '') + ' ' + (e.title || '')).toLowerCase();
  if (pw && !onPortal) {{
    const form = pw.form || document;
    const u = form.querySelector('input[type=email],input[name=username],input[name=j_username],input[type=text]');
    const set = (el, v) => {{ el.focus(); el.value = v; el.dispatchEvent(new Event('input', {{bubbles: true}})); el.dispatchEvent(new Event('change', {{bubbles: true}})); }};
    if (u) set(u, user);
    set(pw, pass);
    const btn = form.querySelector('[type=submit],button');
    if (btn) btn.click(); else if (pw.form) pw.form.submit();
    return 'submitted';
  }}
  const proceed = document.querySelector('[name=_eventId_proceed]');
  if (proceed) {{ proceed.click(); return 'consent'; }}
  if (onPortal) {{
    const fed = [...document.querySelectorAll('a,button,input[type=submit]')].find(e => label(e).includes('schule@bw'));
    if (fed && pw) {{ fed.click(); return 'federation'; }}
    if (!pw) return 'landed';
  }}
  return 'waiting';
}})()"#
    )
}

fn edge_binary() -> Option<std::path::PathBuf> {
    ["C:\\Program Files (x86)\\Microsoft\\Edge\\Application\\msedge.exe", "C:\\Program Files\\Microsoft\\Edge\\Application\\msedge.exe"].iter().map(std::path::PathBuf::from).find(|p| p.is_file())
}

/// Step 5: the same sign-in in a headless Edge; its itslearning cookies join a fresh jar.
pub async fn edge_login(base: &str, username: &str, password: &str) -> Result<BwSession, String> {
    let edge = edge_binary().ok_or("Microsoft Edge is not installed")?;
    let profile = std::env::temp_dir().join(format!("jarvis-edge-sso-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&profile);
    std::fs::create_dir_all(&profile).map_err(|e| e.to_string())?;
    let mut cmd = std::process::Command::new(edge);
    cmd.args(["--headless=new", "--disable-gpu", "--no-first-run", "--no-default-browser-check", "--remote-debugging-port=0"])
        .arg(format!("--user-data-dir={}", profile.display()))
        .arg("about:blank")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    crate::shell::hide_console_std(&mut cmd);
    let mut child = cmd.spawn().map_err(|e| format!("Edge did not start: {e}"))?;
    let result = drive_edge(&profile, base, username, password).await;
    let _ = child.kill();
    let _ = child.wait();
    let _ = std::fs::remove_dir_all(&profile);
    result
}

async fn drive_edge(profile: &std::path::Path, base: &str, username: &str, password: &str) -> Result<BwSession, String> {
    // Edge writes its DevTools port once it listens.
    let port_file = profile.join("DevToolsActivePort");
    let deadline = Instant::now() + Duration::from_secs(15);
    let port = loop {
        if let Some(p) = std::fs::read_to_string(&port_file).ok().and_then(|t| t.lines().next().and_then(|l| l.trim().parse::<u16>().ok())) {
            break p;
        }
        if Instant::now() > deadline {
            return Err("Edge did not open its DevTools port".into());
        }
        tokio::time::sleep(Duration::from_millis(150)).await;
    };
    let http = reqwest::Client::builder().no_proxy().build().map_err(|e| e.to_string())?;
    let targets: Value = http.get(format!("http://127.0.0.1:{port}/json/list")).send().await.map_err(|e| e.to_string())?.json().await.map_err(|e| e.to_string())?;
    let ws = targets.as_array().and_then(|t| t.iter().find(|x| x["type"] == "page")).and_then(|t| t["webSocketDebuggerUrl"].as_str()).ok_or("Edge has no page to drive")?.to_string();
    let mut dt = DevTools::connect(&ws).await?;
    dt.call("Network.enable", json!({})).await?;
    dt.call("Page.navigate", json!({ "url": format!("{}/index.aspx", base.trim_end_matches('/')) })).await?;
    let script = step_script(username, password);
    let mut submitted = 0;
    let mut steps = Vec::new();
    let finish = Instant::now() + Duration::from_secs(75);
    let landed = loop {
        tokio::time::sleep(Duration::from_millis(1200)).await;
        if Instant::now() > finish {
            break false;
        }
        let r = dt.call("Runtime.evaluate", json!({ "expression": script, "returnByValue": true })).await.unwrap_or(Value::Null);
        let step = r["result"]["value"].as_str().unwrap_or("waiting").to_string();
        if steps.last() != Some(&step) {
            info!("[SCHULE@BW] Edge: {step}");
            steps.push(step.clone());
        }
        match step.as_str() {
            "landed" => break true,
            "submitted" => {
                submitted += 1;
                if submitted > 2 {
                    return Err("Schule@BW kept asking for the password (wrong username or password?)".into());
                }
            }
            _ => {}
        }
    };
    if !landed {
        return Err(format!("the headless sign-in did not finish ({})", steps.join(" → ")));
    }
    let cookies = dt.call("Network.getAllCookies", json!({})).await?;
    let jar = Arc::new(Jar::default());
    let base_url = Url::parse(base).map_err(|e| e.to_string())?;
    let mut count = 0;
    for c in cookies["cookies"].as_array().into_iter().flatten() {
        let domain = c["domain"].as_str().unwrap_or("");
        if !domain.contains("itslearning") {
            continue;
        }
        let cookie = format!("{}={}; Domain={}; Path={}", c["name"].as_str().unwrap_or(""), c["value"].as_str().unwrap_or(""), domain.trim_start_matches('.'), c["path"].as_str().unwrap_or("/"));
        jar.add_cookie_str(&cookie, &base_url);
        count += 1;
    }
    if count == 0 {
        return Err("Edge signed in but holds no itslearning cookies".into());
    }
    info!("[SCHULE@BW] Edge signed in; {count} itslearning cookies taken over");
    Ok(BwSession { client: client_for(&jar, true)?, jar, base: base.trim_end_matches('/').to_string(), since: Instant::now(), via: "edge" })
}

#[cfg(test)]
mod tests {
    use super::*;

    const PORTAL: &str = BWS_PORTAL;
    const IDP: &str = "https://login.schule-bw.de/realms/SchuleBW/protocol/saml";

    const LOGIN_PAGE: &str = r#"<html><body><form method="post" action="./index.aspx" id="aspnetForm">
        <input type="hidden" name="__VIEWSTATE" value="VS123"><input type="hidden" name="__VIEWSTATEGENERATOR" value="GEN"><input type="hidden" name="__EVENTVALIDATION" value="EV9">
        <input type="text" name="ctl00$ContentPlaceHolder1$Username$input"><input type="password" name="ctl00$ContentPlaceHolder1$Password$input">
        <input type="submit" name="ctl00$ContentPlaceHolder1$nativeLoginButton" value="Anmelden">
        <a id="ctl00_ContentPlaceHolder1_federatedLoginButtons_ctl00_federatedLoginLink" href="javascript:__doPostBack('ctl00$ContentPlaceHolder1$federatedLoginButtons$ctl00$federatedLoginLink','')">Login über Schule@BW</a>
        </form></body></html>"#;

    const KEYCLOAK: &str = r#"<html><body><div id="kc-form"><form id="kc-form-login" action="/realms/SchuleBW/login-actions/authenticate?session_code=abc&amp;execution=e1&amp;client_id=itsl&amp;tab_id=t9" method="post">
        <input id="username" name="username" type="text"><input id="password" name="password" type="password">
        <input type="hidden" id="id-hidden-input" name="credentialId" value="">
        <input type="submit" name="login" id="kc-login" value="Anmelden"></form></div></body></html>"#;

    const SAML_POST: &str = r#"<html><body onload="document.forms[0].submit()"><form method="post" action="https://bws.itslearning.com/Saml/AssertionConsumerService.aspx">
        <input type="hidden" name="SAMLResponse" value="PHNhbWxwOlJlc3BvbnNlPg=="><input type="hidden" name="RelayState" value="rs-42">
        <noscript><input type="submit" value="Continue"></noscript></form></body></html>"#;

    #[test]
    fn the_portal_page_leads_to_schule_bw_by_asp_net_postback() {
        let Some(Trigger::Post(post)) = find_federation_trigger(LOGIN_PAGE, PORTAL) else { panic!("no trigger") };
        assert_eq!(post.action, "https://bws.itslearning.com/index.aspx");
        let get = |k: &str| post.fields.iter().find(|(n, _)| n == k).map(|(_, v)| v.as_str());
        assert_eq!(get("__VIEWSTATE"), Some("VS123"));
        assert_eq!(get("__EVENTVALIDATION"), Some("EV9"));
        assert_eq!(get("__EVENTTARGET"), Some("ctl00$ContentPlaceHolder1$federatedLoginButtons$ctl00$federatedLoginLink"));
        assert!(get("ctl00$ContentPlaceHolder1$nativeLoginButton").is_none(), "the native login button is not pressed");
        // On the portal the federation wins over its own password form.
        assert!(matches!(classify(LOGIN_PAGE, PORTAL, "bws.itslearning.com", false), Page::Federation(_)));
        // A plain link works too.
        let link = r#"<a class="federated" href="/Saml/Login.aspx?IdP=schule-bw">Login über Schule@BW</a>"#;
        assert_eq!(find_federation_trigger(link, PORTAL), Some(Trigger::Link("https://bws.itslearning.com/Saml/Login.aspx?IdP=schule-bw".into())));
    }

    #[test]
    fn the_idp_form_is_parsed_with_its_hidden_execution_fields() {
        let form = parse_login_form(KEYCLOAK, IDP).unwrap();
        assert_eq!(form.post.action, "https://login.schule-bw.de/realms/SchuleBW/login-actions/authenticate?session_code=abc&execution=e1&client_id=itsl&tab_id=t9");
        assert_eq!((form.user_field.as_str(), form.pass_field.as_str()), ("username", "password"));
        assert!(form.post.fields.iter().any(|(k, _)| k == "credentialId"));
        assert!(form.post.fields.iter().any(|(k, v)| k == "login" && v == "Anmelden"));
        assert!(matches!(classify(KEYCLOAK, IDP, "bws.itslearning.com", false), Page::Login(_)));
        // Shibboleth names.
        let shib = r#"<form action="/idp/profile/SAML2/Redirect/SSO?execution=e1s1" method="post"><input name="j_username" type="text"><input name="j_password" type="password"><button type="submit" name="_eventId_proceed">Login</button></form>"#;
        let f = parse_login_form(shib, "https://idp.schule-bw.de/x").unwrap();
        assert_eq!((f.user_field.as_str(), f.pass_field.as_str()), ("j_username", "j_password"));
    }

    #[test]
    fn the_saml_response_is_posted_back_to_itslearning() {
        let post = parse_auto_post(SAML_POST, IDP).unwrap();
        assert_eq!(post.action, "https://bws.itslearning.com/Saml/AssertionConsumerService.aspx");
        assert_eq!(post.fields, vec![("SAMLResponse".into(), "PHNhbWxwOlJlc3BvbnNlPg==".into()), ("RelayState".into(), "rs-42".into())]);
        assert!(matches!(classify(SAML_POST, IDP, "bws.itslearning.com", true), Page::AutoPost(_)));
        // OIDC form_post and a consent page are hand-offs too.
        let oidc = r#"<form action="https://bws.itslearning.com/oidc/callback" method="post"><input type="hidden" name="code" value="c1"><input type="hidden" name="state" value="s1"></form>"#;
        assert!(parse_auto_post(oidc, IDP).is_some());
        let consent = r#"<form action="/idp/consent" method="post"><input type="hidden" name="_shib_idp_consentIds" value="mail"><input type="submit" name="_eventId_proceed" value="Akzeptieren"></form>"#;
        assert!(parse_auto_post(consent, IDP).unwrap().fields.iter().any(|(k, _)| k == "_eventId_proceed"));
    }

    #[test]
    fn refusals_javascript_pages_and_the_signed_in_portal_are_recognised() {
        let refused = KEYCLOAK.replace("<div id=\"kc-form\">", "<span id=\"input-error\">Ungültiger Benutzername oder Passwort.</span><div id=\"kc-form\">");
        assert_eq!(classify(&refused, IDP, "bws.itslearning.com", true), Page::Failed("Ungültiger Benutzername oder Passwort.".into()));
        let js = "<html><head><script src=\"/app.js\"></script></head><body><noscript>Bitte JavaScript aktivieren</noscript></body></html>";
        assert_eq!(classify(js, IDP, "bws.itslearning.com", false), Page::JavaScript);
        let home = r#"<html><body><a href="/Logout.aspx">Abmelden</a><div class="itsl-personal-menu"></div></body></html>"#;
        assert_eq!(classify(home, "https://bws.itslearning.com/DashboardMenu.aspx", "bws.itslearning.com", true), Page::Landed);
        assert_ne!(classify(home, "https://evil.example.com/", "bws.itslearning.com", true), Page::Landed);
        assert!(uses_schule_bw(&LmsSettings { base_url: "https://bws.itslearning.com".into(), ..Default::default() }));
        assert!(!uses_schule_bw(&LmsSettings { base_url: "https://myschool.itslearning.com".into(), ..Default::default() }));
    }

    #[test]
    fn the_app_code_is_read_from_the_custom_scheme_redirect() {
        assert_eq!(code_from_redirect("itsl-itslearning://login?state=jarvis&code=AbC%2B9"), Some("AbC+9".into()));
        assert_eq!(code_from_redirect("itsl-itslearning://login?error=denied"), None);
    }

    #[test]
    fn websocket_frames_round_trip_and_the_step_script_escapes_credentials() {
        for len in [5usize, 300, 70_000] {
            let payload: Vec<u8> = (0..len).map(|i| (i % 251) as u8).collect();
            let frame = encode_frame(&payload, 0x1, [9, 8, 7, 6]);
            let (fin, op, back, used) = decode_frame(&frame).unwrap();
            assert!(fin && op == 1 && back == payload && used == frame.len());
            assert!(decode_frame(&frame[..frame.len() - 1]).is_none(), "incomplete frames wait");
        }
        let script = step_script("max.mustermann", "pa\"ss'\\word");
        assert!(script.contains(r#"const user = "max.mustermann", pass = "pa\"ss'\\word";"#), "{script}");
        assert!(script.contains("schule@bw"));
    }

    #[test]
    fn tasks_and_courses_become_board_blocks() {
        let now = chrono::NaiveDate::from_ymd_opt(2026, 11, 2).unwrap().and_hms_opt(18, 0, 0).unwrap();
        let at = |d: u32, h: u32| chrono::NaiveDate::from_ymd_opt(2026, 11, d).unwrap().and_hms_opt(h, 0, 0);
        let u = |id: &str, kind: &'static str, course: &str, title: &str, due: Option<chrono::NaiveDateTime>| CourseUpdate { id: id.into(), kind, course: course.into(), title: title.into(), when: None, due, author: String::new(), url: format!("https://bws.itslearning.com/{id}") };
        let updates = vec![
            u("t1", "assignment", "Mathematik", "Arbeitsblatt 4", at(5, 8)),
            u("t2", "assignment", "Physik", "Protokoll Pendel", at(3, 23)),
            u("t3", "assignment", "Deutsch", "Erörterung (old)", at(1, 8)),
            u("n1", "announcement", "Mathematik", "Klausur verschoben", None),
        ];
        let r = board_report(&updates, now);
        assert_eq!(r.topic, "itslearning");
        let BlockKind::HeroFigure { figure, note, .. } = &r.blocks[0].kind else { panic!() };
        assert_eq!(figure, "2", "the past deadline is not open");
        assert!(note.starts_with("Next: Protokoll Pendel"), "{note}");
        let BlockKind::NoteStack { notes } = &r.blocks[1].kind else { panic!() };
        assert_eq!(notes[0].headline, "Protokoll Pendel");
        assert!(r.blocks.iter().any(|b| b.label == "ITSLEARNING // MITTEILUNGEN"));
        assert!(r.blocks.iter().any(|b| matches!(&b.kind, BlockKind::BarChart { bars, .. } if bars[0].label == "Mathematik" && bars[0].value == 2.0)));
        assert_eq!(r.sources.len(), 4);
    }
}
