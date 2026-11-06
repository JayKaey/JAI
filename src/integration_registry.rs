//! The integration registry: one place that knows every live client — WebUntis, Apple Calendar
//! (CalDAV), Gmail (IMAP), itslearning, WhatsApp, the custom web feeds, the mobile satellite,
//! location, the hivemind nodes and the language engine itself — and can verify them all at once.
//!
//! `diagnose_integrations` (the tool, the hub's "Test & Diagnose All Connections" button and
//! `JARVIS.exe --diagnose`) runs every check concurrently and reports, per service, the exact
//! HTTP status (or protocol result), timing and a preview of the data it read — so a broken
//! integration fails loudly instead of Jarvis quietly answering from imagination.

use crate::pim_config::{CALENDAR, EMAIL, LMS, WEBUNTIS};
use crate::tools::ToolContext;
use chrono::{Duration as ChronoDuration, Local};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tracing::info;

/// One service's verdict.
#[derive(Debug, Clone, PartialEq)]
pub struct ServiceReport {
    pub name: String,
    /// "ok", "error" or "off" (not configured).
    pub state: String,
    /// "HTTP 200", "IMAP OK", "TCP 8443 open", …
    pub status: String,
    pub detail: String,
    pub preview: Vec<String>,
    pub ms: u64,
}

impl ServiceReport {
    fn new(name: &str, state: &str, status: impl Into<String>, detail: impl Into<String>, preview: Vec<String>, started: Instant) -> Self {
        Self { name: name.into(), state: state.into(), status: status.into(), detail: detail.into(), preview, ms: started.elapsed().as_millis() as u64 }
    }

    pub fn ok(&self) -> bool {
        self.state == "ok"
    }

    pub fn line(&self) -> String {
        let mark = match self.state.as_str() {
            "ok" => "OK",
            "off" => "OFF",
            _ => "ERR",
        };
        let mut s = format!("{mark} {} — {} · {} ({} ms)", self.name, self.status, self.detail, self.ms);
        for p in self.preview.iter().take(3) {
            s.push_str(&format!("\n    · {}", p.chars().take(140).collect::<String>()));
        }
        s
    }
}

/// The last verification of every service (the hub shows it).
#[derive(Clone, Default)]
pub struct IntegrationRegistry {
    last: Arc<Mutex<Vec<ServiceReport>>>,
    running: Arc<std::sync::atomic::AtomicBool>,
}

impl IntegrationRegistry {
    pub fn last(&self) -> Vec<ServiceReport> {
        self.last.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn running(&self) -> bool {
        self.running.load(std::sync::atomic::Ordering::Acquire)
    }

    /// Checks everything (concurrently, 30 s per service at most).
    pub async fn diagnose_all(&self, tools: &ToolContext) -> Vec<ServiceReport> {
        self.running.store(true, std::sync::atomic::Ordering::Release);
        tools.stark.pulse(&["brain", "context", "webuntis"], false);
        tools.stark.pulse(&["brain", "context", "calendar"], false);
        let (a, b, c, d, e, f, g, h, i, j) = tokio::join!(
            limited("WebUntis", check_webuntis(tools)),
            limited("Apple Calendar (CalDAV)", check_calendar(tools)),
            limited("Gmail (IMAP)", check_mail(tools)),
            limited("itslearning", check_lms(tools)),
            limited("WhatsApp", check_whatsapp(tools)),
            limited("Custom websites", check_feeds(tools)),
            limited("Mobile satellite", check_satellite(tools)),
            limited("Location / GPS", check_location(tools)),
            limited("Hivemind nodes", check_nodes(tools)),
            limited("Language engine", check_llm(tools)),
        );
        let reports = vec![a, b, c, d, e, f, g, h, i, j];
        for r in &reports {
            info!("[DIAG] {}", r.line().replace('\n', " "));
        }
        *self.last.lock().unwrap_or_else(|e| e.into_inner()) = reports.clone();
        self.running.store(false, std::sync::atomic::Ordering::Release);
        let failed: Vec<&str> = reports.iter().filter(|r| r.state == "error").map(|r| r.name.as_str()).collect();
        tools.stark.pulse(&["context", "brain"], !failed.is_empty());
        reports
    }
}

/// A check with a 30-second limit.
async fn limited(name: &'static str, check: impl std::future::Future<Output = ServiceReport>) -> ServiceReport {
    let started = Instant::now();
    match tokio::time::timeout(Duration::from_secs(30), check).await {
        Ok(r) => r,
        Err(_) => ServiceReport::new(name, "error", "timeout", "no answer within 30 s", Vec::new(), started),
    }
}

/// An HTTP probe of a base URL: the status code (any answer counts as reachable).
async fn http_status(url: &str) -> String {
    match crate::websearch::HTTP.get(url).timeout(Duration::from_secs(10)).send().await {
        Ok(r) => format!("HTTP {}", r.status().as_u16()),
        Err(e) => format!("unreachable ({})", crate::web_connector::err_chain(e)),
    }
}

async fn check_webuntis(tools: &ToolContext) -> ServiceReport {
    let started = Instant::now();
    let name = "WebUntis";
    let pim = &tools.pim;
    if !pim.config().enabled(WEBUNTIS) {
        return ServiceReport::new(name, "off", "not configured", "switch it on in the Integrations hub", Vec::new(), started);
    }
    let cfg = pim.config().read(|s| s.webuntis.clone());
    let status = http_status(&format!("https://{}/WebUntis/", crate::web_connector::host_of(&cfg.server))).await;
    let today = Local::now().date_naive();
    match pim.lessons(today, today + ChronoDuration::days(6)).await {
        Ok(lessons) => {
            let source = pim.untis().last_source.lock().unwrap_or_else(|e| e.into_inner()).clone();
            let preview = lessons.iter().take(3).map(|l| format!("{} {}", l.date.format("%a %d.%m."), l.line())).collect();
            ServiceReport::new(name, "ok", status, format!("{} lessons in the next 7 days (via {source})", lessons.len()), preview, started)
        }
        Err(e) => ServiceReport::new(name, "error", status, e, Vec::new(), started),
    }
}

async fn check_calendar(tools: &ToolContext) -> ServiceReport {
    let started = Instant::now();
    let name = "Apple Calendar (CalDAV)";
    let pim = &tools.pim;
    if !pim.config().enabled(CALENDAR) {
        return ServiceReport::new(name, "off", "not configured", "events stay in the local calendar only", Vec::new(), started);
    }
    let cfg = pim.config().read(|s| s.calendar.clone());
    let status = http_status(&cfg.caldav_url).await;
    match pim.test_service(CALENDAR).await {
        Ok(detail) => {
            let today = Local::now().date_naive();
            let (events, note) = pim.schedule(today, today + ChronoDuration::days(6)).await;
            let preview = events.iter().take(3).map(|e| format!("{} {}", e.start.format("%a %d.%m."), e.line())).collect();
            // The discovery PROPFINDs answered (a bare GET of the server is 403 without a login).
            ServiceReport::new(name, if note.is_some() { "error" } else { "ok" }, "PROPFIND 207", format!("{detail}; {} event(s) in the next 7 days{}", events.len(), note.map(|n| format!(" {n}")).unwrap_or_default()), preview, started)
        }
        Err(e) => ServiceReport::new(name, "error", status, e, Vec::new(), started),
    }
}

async fn check_mail(tools: &ToolContext) -> ServiceReport {
    let started = Instant::now();
    let name = "Gmail (IMAP)";
    let pim = &tools.pim;
    if !pim.config().enabled(EMAIL) {
        return ServiceReport::new(name, "off", "not configured", "add the IMAP account in the Integrations hub", Vec::new(), started);
    }
    match pim.unread_mail(3, None).await {
        Ok((n, list)) => ServiceReport::new(name, "ok", "IMAP OK", format!("{n} unread"), list.iter().map(|h| format!("{} — {}", h.from, h.subject)).collect(), started),
        Err(e) => ServiceReport::new(name, "error", "IMAP failed", e, Vec::new(), started),
    }
}

async fn check_lms(tools: &ToolContext) -> ServiceReport {
    let started = Instant::now();
    let name = "itslearning";
    let pim = &tools.pim;
    if !pim.config().enabled(LMS) {
        return ServiceReport::new(name, "off", "not configured", "add the portal in the Integrations hub", Vec::new(), started);
    }
    let cfg = pim.config().read(|s| s.lms.clone());
    let status = if cfg.base_url.trim().is_empty() { "feed only".to_string() } else { http_status(&crate::web_connector::normalize_url(&cfg.base_url)).await };
    match pim.course_updates(None).await {
        Ok(updates) => ServiceReport::new(name, "ok", status, format!("{} update(s)", updates.len()), crate::lms_client::updates_text(&updates, 3).lines().map(String::from).collect(), started),
        Err(e) => ServiceReport::new(name, "error", status, e, Vec::new(), started),
    }
}

async fn check_whatsapp(tools: &ToolContext) -> ServiceReport {
    let started = Instant::now();
    let name = "WhatsApp";
    let settings = tools.config.read(|c| c.whatsapp.clone());
    let listener = if settings.notification_listener { crate::whatsapp_client::listener_status() } else { Ok("listener off".into()) };
    let mut detail = Vec::new();
    let mut state = "ok";
    match &listener {
        Ok(t) => detail.push(t.clone()),
        Err(e) => {
            detail.push(format!("notification listener: {e}"));
            state = "error";
        }
    }
    let mut status = "desktop URI".to_string();
    if !settings.bridge_url.trim().is_empty() {
        let token = tools.pim.config().vault().secret(crate::whatsapp_client::VAULT_SERVICE, crate::vault::TOKEN);
        match tools.stark.whatsapp.test_bridge(&settings, token).await {
            Ok(t) => {
                status = t.trim_end_matches('.').replace("Bridge answered ", "bridge ");
            }
            Err(e) => {
                status = "bridge failed".into();
                detail.push(e);
                state = "error";
            }
        }
    }
    let preview = tools.stark.whatsapp.recent(None, false, 3).iter().map(|m| m.line()).collect();
    detail.push(format!("{} unread", tools.stark.whatsapp.unread_count()));
    ServiceReport::new(name, state, status, detail.join("; "), preview, started)
}

async fn check_feeds(tools: &ToolContext) -> ServiceReport {
    let started = Instant::now();
    let name = "Custom websites";
    let feeds = tools.pim.config().read(|s| s.feeds.clone());
    if feeds.is_empty() {
        return ServiceReport::new(name, "off", "none registered", "add web feeds in the Integrations hub", Vec::new(), started);
    }
    let mut preview = Vec::new();
    let mut failures = 0;
    for f in &feeds {
        let status = http_status(&crate::web_connector::normalize_url(&f.url)).await;
        match tools.pim.web().snapshot(f).await {
            Ok(lines) => preview.push(format!("{} ({status}): {}", f.name, lines.first().cloned().unwrap_or_else(|| "(no text)".into()))),
            Err(e) => {
                failures += 1;
                preview.push(format!("{} ({status}): {e}", f.name));
            }
        }
    }
    ServiceReport::new(name, if failures == 0 { "ok" } else { "error" }, format!("{} feed(s)", feeds.len()), format!("{failures} failing"), preview, started)
}

async fn check_satellite(tools: &ToolContext) -> ServiceReport {
    let started = Instant::now();
    let name = "Mobile satellite";
    let s = tools.config.read(|c| c.satellite.clone());
    if !s.enabled {
        return ServiceReport::new(name, "off", "disabled", "switch it on in Quick Connections", Vec::new(), started);
    }
    let url = crate::satellite::pairing_url(&tools.config);
    let open = tokio::time::timeout(Duration::from_secs(3), tokio::net::TcpStream::connect(("127.0.0.1", s.port))).await.is_ok_and(|r| r.is_ok());
    let phone = tools.stark.location.phone_fix().map(|f| format!("phone GPS ±{:.0} m", f.accuracy_m)).unwrap_or_else(|| "no phone connected".into());
    ServiceReport::new(
        name,
        if open { "ok" } else { "error" },
        if open { format!("TCP {} open", s.port) } else { format!("TCP {} closed (Jarvis not running?)", s.port) },
        phone,
        vec![url.replace(&s.token, "<key>")],
        started,
    )
}

async fn check_location(tools: &ToolContext) -> ServiceReport {
    let started = Instant::now();
    let name = "Location / GPS";
    match tools.stark.location.current(tools).await {
        Ok(fix) => ServiceReport::new(name, "ok", fix.source.clone(), format!("{:.4}, {:.4} ±{:.0} m", fix.lat, fix.lon, fix.accuracy_m), fix.address.into_iter().collect(), started),
        Err(e) => ServiceReport::new(name, "error", "no position", e, Vec::new(), started),
    }
}

async fn check_nodes(tools: &ToolContext) -> ServiceReport {
    let started = Instant::now();
    let name = "Hivemind nodes";
    let nodes = tools.stark.hivemind.list();
    if nodes.is_empty() {
        return ServiceReport::new(name, "off", "none registered", "\"add my Raspberry Pi at 192.168.1.50\"", Vec::new(), started);
    }
    let mut preview = Vec::new();
    let mut down = 0;
    for n in &nodes {
        match crate::hivemind_mesh::tcp_ping(&n.host, n.ssh_port.max(1)).await {
            Ok(ms) => preview.push(format!("{}: SSH port answers in {ms} ms", n.name)),
            Err(e) => {
                down += 1;
                preview.push(format!("{}: {e}", n.name));
            }
        }
    }
    ServiceReport::new(name, if down == 0 { "ok" } else { "error" }, format!("{} node(s)", nodes.len()), format!("{down} unreachable"), preview, started)
}

async fn check_llm(tools: &ToolContext) -> ServiceReport {
    let started = Instant::now();
    let name = "Language engine";
    let cfg = tools.config.snapshot();
    let base = match tools.pim.llm() {
        Some(llm) => llm.url().to_string(),
        None => format!("http://127.0.0.1:{}", cfg.llm_port),
    };
    let show = crate::websearch::HTTP.post(format!("{base}/api/show")).json(&json!({ "model": cfg.primary_model })).timeout(Duration::from_secs(8)).send().await;
    match show {
        Ok(r) if r.status().is_success() => {
            let v: Value = r.json().await.unwrap_or_default();
            let family = v["details"]["family"].as_str().unwrap_or("?").to_string();
            let size = v["details"]["parameter_size"].as_str().unwrap_or("?").to_string();
            let mismatch = cfg.primary_model.to_lowercase().contains("qwen3") && !family.starts_with("qwen3");
            ServiceReport::new(
                name,
                "ok",
                "HTTP 200",
                format!(
                    "'{}' is a {family} model with {size} parameters{}",
                    cfg.primary_model,
                    if mismatch { " — NOT Qwen3 despite its tag; small models skip tools, so Jarvis routes integrations himself" } else { "" }
                ),
                vec![format!("num_ctx {}", cfg.num_ctx)],
                started,
            )
        }
        Ok(r) => ServiceReport::new(name, "error", format!("HTTP {}", r.status().as_u16()), format!("model '{}' not available", cfg.primary_model), Vec::new(), started),
        Err(_) => ServiceReport::new(name, "off", "not running", format!("the engine at {base} is not running (start JARVIS.exe)"), Vec::new(), started),
    }
}

/// The report as text (tool output, CLI, the diagnostics window).
pub fn report_text(reports: &[ServiceReport]) -> String {
    let ok = reports.iter().filter(|r| r.state == "ok").count();
    let failed = reports.iter().filter(|r| r.state == "error").count();
    format!(
        "Integration diagnostics: {ok} working, {failed} failing, {} not configured.\n{}",
        reports.len() - ok - failed,
        reports.iter().map(ServiceReport::line).collect::<Vec<_>>().join("\n")
    )
}

pub fn is_tool(name: &str) -> bool {
    name == "diagnose_integrations"
}

pub fn tool_definition() -> Value {
    json!({"type": "function", "function": {
        "name": "diagnose_integrations",
        "description": "Test & diagnose every connection at once (WebUntis, Apple Calendar, Gmail, itslearning, WhatsApp, custom websites, the phone satellite, location, hivemind nodes, the language engine): exact HTTP status, timing and a data preview per service, also shown in a window.",
        "parameters": {"type": "object", "properties": {}, "required": []}
    }})
}

pub async fn run_tool(tools: &ToolContext) -> Result<String, String> {
    let reports = tools.stark.integrations.diagnose_all(tools).await;
    let text = report_text(&reports);
    if crate::gui::windows_supported() {
        let table = format!(
            "| Service | Result | Status | Detail |\n|---|---|---|---|\n{}",
            reports
                .iter()
                .map(|r| format!("| {} | {} | {} | {} |", r.name, if r.ok() { "OK" } else if r.state == "off" { "OFF" } else { "ERR" }, r.status, r.detail.replace('|', "/")))
                .collect::<Vec<_>>()
                .join("\n")
        );
        let previews: String = reports
            .iter()
            .filter(|r| !r.preview.is_empty())
            .map(|r| format!("**{}**\n{}", r.name, r.preview.iter().map(|p| format!("- {p}")).collect::<Vec<_>>().join("\n")))
            .collect::<Vec<_>>()
            .join("\n\n");
        let _ = tools.widgets.spawn_window(&json!({ "id": "integration_diagnostics", "title": "Integration Diagnostics", "markdown": format!("{table}\n\n{previews}"), "replace_content": true, "width": 760, "height": 560 }));
        crate::gui::sync_windows();
    }
    Ok(text)
}

/// `JARVIS.exe --diagnose`: the same checks from the command line (read-only; no audio, no HUD).
pub async fn cli(cfg: &crate::config::JarvisConfig) -> std::process::ExitCode {
    let store = crate::config::ConfigStore::new(cfg.clone());
    let memory = match crate::memory::MemoryManager::open(&cfg.memory_db_path) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("cannot open {}: {e}", cfg.memory_db_path);
            return std::process::ExitCode::FAILURE;
        }
    };
    let skills = match crate::skills::SkillManager::open(&cfg.memory_db_path, &cfg.skills_dir) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("cannot open the skills: {e}");
            return std::process::ExitCode::FAILURE;
        }
    };
    let tools = ToolContext {
        memory,
        skills,
        monitor: crate::telemetry::SystemMonitor::start(cfg.hardware_profile),
        voice_volume: Arc::new(std::sync::atomic::AtomicU8::new(100)),
        config: store.clone(),
        modality: crate::modality::ModalityStore::in_memory(),
        widgets: crate::widgets::WidgetEngine::in_memory(),
        reminders: crate::reminders::ReminderStore::in_memory(),
        protocols: crate::protocols::ProtocolStore::in_memory(),
        voice_speed: Arc::new(std::sync::atomic::AtomicU32::new(1.0f32.to_bits())),
        focus: crate::focus::FocusTracker::new(),
        music: crate::music::MusicEngine::in_memory(),
        workout: crate::workout_coach::WorkoutCoach::in_memory(),
        pim: crate::pim::Pim::open(&cfg.memory_db_path, store.clone(), None),
        stark: crate::stark::StarkSystems::open(&cfg.memory_db_path),
    };
    println!("J.A.R.V.I.S. integration diagnostics…\n");
    let reports = tools.stark.integrations.diagnose_all(&tools).await;
    println!("{}", report_text(&reports));
    if reports.iter().any(|r| r.state == "error") {
        std::process::ExitCode::FAILURE
    } else {
        std::process::ExitCode::SUCCESS
    }
}
