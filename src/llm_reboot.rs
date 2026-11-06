//! `[ ⟳ REBOOT LLM ]` / "Jarvis, reboot your language model": a hard reset of the language engine
//! when it is stuck (a cold start that never finishes, a wedged generation, a poisoned
//! conversation).
//!
//! 1. Abort: the running turn is cancelled (from the button; a spoken request is itself the
//!    running turn), so its `ThinkingStateGuard` drops, and the conversation history is cleared.
//! 2. Flush: every model is unloaded from VRAM (`/api/generate` with `"keep_alive": 0`).
//! 3. Health: `/api/version` must answer within 1.5 s; if it does not (or the flush could not
//!    reach the engine), the built-in engine process is killed and started fresh.
//! 4. Warm reload: a one-token generation (`"prompt": "ok"`, `num_predict` 1) loads the model
//!    again; then the prompt prefix is re-primed in the background.
//!
//! The HUD's header button shows the phase (amber while rebooting, the latency once online) and
//! Jarvis confirms "Language model rebooted and online, Sir." How long the reload takes depends on
//! the model: unloading is instant, loading an 18 GB model back into VRAM takes seconds.

use serde_json::{json, Value};
use std::future::Future;
use std::sync::{Arc, LazyLock, Mutex, OnceLock};
use std::time::{Duration, Instant};
use tracing::{info, warn};

pub const TOOL: &str = "reboot_language_model";
pub const CONFIRMATION: &str = "Language model rebooted and online, Sir.";

/// What started the reboot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trigger {
    /// The HUD button: the running turn is cancelled first.
    Button,
    /// Sir asked: the running turn is the one rebooting, so it carries on.
    Voice,
    /// A turn found the engine unavailable: Jarvis recovers on his own (at most once a minute).
    Auto,
}

static AUTO_AT: Mutex<Option<Instant>> = Mutex::new(None);

/// Starts an automatic reboot in the background unless one ran in the last minute; true when
/// one started. (Live 2026-11-03: a 40 s start timeout left the engine "unavailable" until
/// restart, and "then you reboot it" could not reach the reboot tool without the model.)
pub fn auto_recover() -> bool {
    {
        let mut at = AUTO_AT.lock().unwrap_or_else(|e| e.into_inner());
        if CONTEXT.get().is_none() || at.is_some_and(|t| t.elapsed() < Duration::from_secs(60)) {
            return false;
        }
        *at = Some(Instant::now());
    }
    tokio::spawn(async {
        let _ = reboot(Trigger::Auto).await;
    });
    true
}

/// The button's state.
#[derive(Debug, Clone, PartialEq)]
pub enum Phase {
    Idle,
    Rebooting { since: Instant, step: &'static str },
    Online { latency_ms: u64, restarted: bool },
    Failed(String),
}

static PHASE: LazyLock<Mutex<Phase>> = LazyLock::new(|| Mutex::new(Phase::Idle));

pub fn phase() -> Phase {
    PHASE.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

fn set_phase(p: Phase) {
    *PHASE.lock().unwrap_or_else(|e| e.into_inner()) = p;
    crate::gui::refresh();
}

/// The header button's label for a phase (and whether it glows amber).
pub fn button_label(p: &Phase) -> (String, bool) {
    match p {
        Phase::Idle => ("[ REBOOT LLM ]".into(), false),
        Phase::Rebooting { step, .. } => (format!("[ {step}… ]"), true),
        Phase::Online { latency_ms, .. } => (format!("[ LLM ONLINE · {:.1} S ]", *latency_ms as f32 / 1000.0), false),
        Phase::Failed(_) => ("[ REBOOT FAILED ]".into(), true),
    }
}

/// The unload request for one model.
pub fn unload_payload(model: &str) -> Value {
    json!({ "model": model, "keep_alive": 0 })
}

/// The one-token warm-up that loads the model back.
pub fn warm_payload(model: &str, keep_alive: Value) -> Value {
    json!({ "model": model, "prompt": "ok", "stream": false, "keep_alive": keep_alive, "options": { "num_predict": 1 } })
}

/// What a reboot did.
#[derive(Debug, Clone, PartialEq)]
pub struct CycleReport {
    pub unloaded: Vec<String>,
    pub restarted: bool,
    pub latency_ms: u64,
    pub warm_ms: u64,
    /// The model that was warmed (the configured one, or an installed stand-in).
    pub model: String,
}

/// Steps 2-4 against an engine at `url` (`restart`: kills and relaunches the engine process).
pub async fn cycle_engine<F, Fut>(client: &reqwest::Client, url: &str, models: &[String], keep_alive: Value, restart: F, on_step: impl Fn(&'static str)) -> Result<CycleReport, String>
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<(), String>>,
{
    let started = Instant::now();
    on_step("FLUSHING VRAM");
    let mut unloaded = Vec::new();
    let mut unreachable = false;
    for model in models.iter().filter(|m| !m.trim().is_empty()) {
        match client.post(format!("{url}/api/generate")).json(&unload_payload(model)).timeout(Duration::from_secs(4)).send().await {
            Ok(r) if r.status().is_success() => unloaded.push(model.clone()),
            Ok(r) => warn!("[REBOOT] Unloading '{model}' answered HTTP {}", r.status()),
            Err(e) => {
                warn!("[REBOOT] The engine did not take the unload of '{model}': {e}");
                unreachable = true;
            }
        }
    }
    on_step("HEALTH CHECK");
    let healthy = !unreachable
        && client.get(format!("{url}/api/version")).timeout(Duration::from_millis(1500)).send().await.is_ok_and(|r| r.status().is_success());
    let mut restarted = false;
    if !healthy {
        on_step("RESTARTING ENGINE");
        warn!("[REBOOT] The language engine is unresponsive; restarting it");
        restart().await?;
        restarted = true;
    }
    on_step("WARM RELOAD");
    // The configured model if it is installed, else whatever chat model is (a missing tag would
    // 404 every request).
    let tags: Value = match client.get(format!("{url}/api/tags")).timeout(Duration::from_secs(4)).send().await {
        Ok(r) => r.json().await.unwrap_or_default(),
        Err(_) => Value::Null,
    };
    let installed: Vec<String> = tags["models"].as_array().into_iter().flatten().filter_map(|m| m["name"].as_str().map(String::from)).collect();
    let model = &match crate::runtime::pick_model(&installed, models) {
        Some(m) => m,
        None => models.first().filter(|m| !m.trim().is_empty()).cloned().ok_or("no language model is configured")?,
    };
    if !models.first().is_some_and(|m| m == model) {
        warn!("[REBOOT] '{}' is not installed; ACTIVE LLM // {model}", models.first().map(String::as_str).unwrap_or(""));
    }
    let warm = Instant::now();
    let resp = client
        .post(format!("{url}/api/generate"))
        .json(&warm_payload(model, keep_alive))
        .timeout(Duration::from_secs(180))
        .send()
        .await
        .map_err(|e| format!("the warm-up of '{model}' failed: {e}"))?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body: String = resp.text().await.unwrap_or_default().chars().take(200).collect();
        return Err(format!("the warm-up of '{model}' answered HTTP {status}: {body}"));
    }
    let _ = resp.bytes().await;
    Ok(CycleReport { unloaded, restarted, latency_ms: started.elapsed().as_millis() as u64, warm_ms: warm.elapsed().as_millis() as u64, model: model.clone() })
}

/// What the reboot needs from the running Jarvis (registered at startup).
pub struct RebootContext {
    pub llm: Arc<crate::runtime::LlmRuntime>,
    pub brain: crate::brain::Brain,
    pub config: crate::config::ConfigStore,
    /// Cancels the running turn and silences Jarvis.
    pub interrupt: Box<dyn Fn() + Send + Sync>,
    /// A notice in the HUD.
    pub notice: Box<dyn Fn(String) + Send + Sync>,
    /// Says a line outside of a turn (the button's confirmation).
    pub say: Box<dyn Fn(&str) + Send + Sync>,
}

static CONTEXT: OnceLock<RebootContext> = OnceLock::new();

pub fn install(ctx: RebootContext) {
    let _ = CONTEXT.set(ctx);
}

/// The whole reboot. Errs while another one runs.
pub async fn reboot(trigger: Trigger) -> Result<CycleReport, String> {
    let ctx = CONTEXT.get().ok_or("the language engine is not running in this mode")?;
    if matches!(phase(), Phase::Rebooting { .. }) {
        return Err("A reboot is already running, Sir.".into());
    }
    let since = Instant::now();
    set_phase(Phase::Rebooting { since, step: "ABORTING" });
    crate::core_sphere_3d::CoreActivityBus::chain(&["llm_reboot.rs", "brain.rs", "runtime.rs"]);
    info!("[REBOOT] Language model reboot ({trigger:?})");
    if trigger == Trigger::Button {
        (ctx.interrupt)();
    }
    ctx.brain.clear_history().await;
    crate::vram_guard::touch();
    let (primary, fallback, keep_alive) = ctx.config.read(|c| (c.primary_model.clone(), c.fallback_model.clone(), crate::vram_guard::keep_alive_value(c)));
    let client = reqwest::Client::builder().no_proxy().connect_timeout(Duration::from_secs(2)).build().map_err(|e| e.to_string())?;
    let llm = Arc::clone(&ctx.llm);
    let url = llm.url().to_string();
    let models = vec![primary, fallback];
    let result = cycle_engine(&client, &url, &models, keep_alive, || async move { llm.restart_engine().await }, |step| set_phase(Phase::Rebooting { since, step })).await;
    match &result {
        Ok(report) => {
            info!("[REBOOT] Online in {} ms (warm-up {} ms, unloaded {:?}, engine restarted: {}); ACTIVE LLM // {}", report.latency_ms, report.warm_ms, report.unloaded, report.restarted, report.model);
            set_phase(Phase::Online { latency_ms: report.latency_ms, restarted: report.restarted });
            (ctx.notice)(format!("SYSTEM // LLM REBOOT COMPLETE — LATENCY: {} ms", report.latency_ms));
            ctx.brain.request_prime();
            if trigger == Trigger::Button {
                (ctx.say)(CONFIRMATION);
            }
        }
        Err(e) => {
            warn!("[REBOOT] Failed: {e}");
            set_phase(Phase::Failed(e.clone()));
            (ctx.notice)(format!("SYSTEM // LLM REBOOT FAILED — {e}"));
        }
    }
    result
}

/// Starts a button reboot in the background.
pub fn reboot_from_button(rt: &tokio::runtime::Handle) {
    rt.spawn(async {
        let _ = reboot(Trigger::Button).await;
    });
}

pub fn is_tool(name: &str) -> bool {
    name == TOOL
}

pub fn definitions() -> Vec<Value> {
    vec![json!({
        "type": "function",
        "function": {
            "name": TOOL,
            "description": "Hard-reset YOUR OWN language model when Sir asks to reboot or restart it (or your brain): cancels stuck work, clears the conversation, flushes VRAM, restarts an unresponsive engine and reloads the model.",
            "parameters": { "type": "object", "properties": {} }
        }
    })]
}

/// The tool: the result ends with the "Spoken:" line.
pub async fn run_tool() -> Result<String, String> {
    let report = reboot(Trigger::Voice).await?;
    Ok(format!(
        "Language model rebooted in {} ms (warm-up {} ms{}).\nSpoken: {CONFIRMATION}",
        report.latency_ms,
        report.warm_ms,
        if report.restarted { ", engine process restarted" } else { "" }
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// A fake language engine: answers every request with `{}` and records (path, body). With
    /// `installed` models it lists them on /api/tags and 404s a warm-up of any other, like Ollama.
    async fn fake_engine(healthy: bool, installed: &[&str]) -> (String, Arc<Mutex<Vec<(String, Value)>>>) {
        let installed: Vec<String> = installed.iter().map(|m| m.to_string()).collect();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = Arc::clone(&seen);
        tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else { return };
                let log = Arc::clone(&log);
                let installed = installed.clone();
                tokio::spawn(async move {
                    let mut buf = Vec::new();
                    let mut chunk = [0u8; 4096];
                    loop {
                        let n = sock.read(&mut chunk).await.unwrap_or(0);
                        if n == 0 {
                            return;
                        }
                        buf.extend_from_slice(&chunk[..n]);
                        let text = String::from_utf8_lossy(&buf).to_string();
                        if let Some(head_end) = text.find("\r\n\r\n") {
                            let len = text[..head_end].lines().find_map(|l| l.to_lowercase().strip_prefix("content-length:").map(|v| v.trim().parse::<usize>().unwrap_or(0))).unwrap_or(0);
                            if buf.len() >= head_end + 4 + len {
                                let path = text.split_whitespace().nth(1).unwrap_or("").to_string();
                                let body: Value = serde_json::from_slice(&buf[head_end + 4..head_end + 4 + len]).unwrap_or(Value::Null);
                                let missing = !installed.is_empty() && body["prompt"] == "ok" && !installed.iter().any(|m| body["model"] == m.as_str());
                                let status = if path == "/api/version" && !healthy { "503 Service Unavailable" } else if missing { "404 Not Found" } else { "200 OK" };
                                let payload = if path == "/api/tags" { json!({ "models": installed.iter().map(|m| json!({ "name": m })).collect::<Vec<_>>() }).to_string() } else { "{}".to_string() };
                                log.lock().unwrap_or_else(|e| e.into_inner()).push((path, body));
                                let reply = format!("HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}", payload.len());
                                let _ = sock.write_all(reply.as_bytes()).await;
                                return;
                            }
                        }
                    }
                });
            }
        });
        (url, seen)
    }

    #[test]
    fn payloads_flush_vram_and_warm_with_one_token() {
        assert_eq!(unload_payload("qwen3:30b-a3b"), json!({ "model": "qwen3:30b-a3b", "keep_alive": 0 }));
        let warm = warm_payload("qwen3:30b-a3b", json!("90s"));
        assert_eq!(warm["prompt"], "ok");
        assert_eq!(warm["stream"], false);
        assert_eq!(warm["options"]["num_predict"], 1);
        assert_eq!(warm["keep_alive"], "90s");
    }

    #[test]
    fn button_labels_follow_the_phase() {
        assert_eq!(button_label(&Phase::Idle), ("[ REBOOT LLM ]".into(), false));
        let (label, amber) = button_label(&Phase::Rebooting { since: Instant::now(), step: "WARM RELOAD" });
        assert!(amber && label.contains("WARM RELOAD"));
        assert_eq!(button_label(&Phase::Online { latency_ms: 2400, restarted: false }).0, "[ LLM ONLINE · 2.4 S ]");
        assert!(button_label(&Phase::Failed("x".into())).1);
    }

    #[tokio::test]
    async fn a_healthy_engine_is_flushed_and_warmed_without_a_restart() {
        let (url, seen) = fake_engine(true, &[]).await;
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        let steps = Mutex::new(Vec::new());
        let models = vec!["qwen3:30b-a3b".to_string(), "qwen3:8b".to_string()];
        let report = cycle_engine(&client, &url, &models, json!("90s"), || async { Err::<(), _>("must not restart".to_string()) }, |s| steps.lock().unwrap_or_else(|e| e.into_inner()).push(s)).await.unwrap();
        assert!(!report.restarted);
        assert_eq!(report.unloaded, models);
        let seen = seen.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let unloads: Vec<&Value> = seen.iter().filter(|(p, b)| p == "/api/generate" && b["keep_alive"] == 0).map(|(_, b)| b).collect();
        assert_eq!(unloads.len(), 2, "both models flushed: {seen:?}");
        assert!(seen.iter().any(|(p, _)| p == "/api/version"));
        let warm = seen.iter().find(|(p, b)| p == "/api/generate" && b["prompt"] == "ok").expect("warm-up sent");
        assert_eq!(warm.1["model"], "qwen3:30b-a3b");
        assert_eq!(*steps.lock().unwrap_or_else(|e| e.into_inner()), vec!["FLUSHING VRAM", "HEALTH CHECK", "WARM RELOAD"]);
    }

    #[tokio::test]
    async fn an_unresponsive_engine_is_restarted_before_the_warm_up() {
        let (url, _) = fake_engine(false, &[]).await;
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        let restarted = Arc::new(Mutex::new(false));
        let flag = Arc::clone(&restarted);
        let report = cycle_engine(&client, &url, &["m".to_string()], json!(-1), || async move {
            *flag.lock().unwrap_or_else(|e| e.into_inner()) = true;
            Ok(())
        }, |_| {})
        .await
        .unwrap();
        assert!(report.restarted && *restarted.lock().unwrap_or_else(|e| e.into_inner()));
        // No engine at all and no way to restart it: an honest error.
        let dead = cycle_engine(&client, "http://127.0.0.1:9", &["m".to_string()], json!(0), || async { Err::<(), _>("external Ollama".to_string()) }, |_| {}).await;
        assert_eq!(dead, Err("external Ollama".to_string()));
    }

    #[tokio::test]
    async fn the_warm_up_locks_onto_an_installed_model_when_the_configured_tags_are_missing() {
        let (url, seen) = fake_engine(true, &["llama3.2:3b", "nomic-embed-text"]).await;
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        let models = vec!["qwen3:30b-a3b".to_string(), "qwen2.5:7b-instruct".to_string()];
        let report = cycle_engine(&client, &url, &models, json!("90s"), || async { Ok(()) }, |_| {}).await.expect("an installed model warms up");
        assert_eq!(report.model, "llama3.2:3b");
        let seen = seen.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let warm = seen.iter().find(|(p, b)| p == "/api/generate" && b["prompt"] == "ok").expect("warm-up sent");
        assert_eq!(warm.1["model"], "llama3.2:3b");
        // The configured model wins whenever it is installed (":latest" spelled or not).
        assert_eq!(crate::runtime::pick_model(&["qwen2.5:7b-instruct".into(), "qwen3:30b-a3b".into()], &models).as_deref(), Some("qwen3:30b-a3b"));
        assert_eq!(crate::runtime::pick_model(&["llama3:latest".into()], &["llama3".into()]).as_deref(), Some("llama3:latest"));
        assert_eq!(crate::runtime::pick_model(&["nomic-embed-text".into()], &models), None, "an embedding model cannot chat");
    }

    #[tokio::test]
    async fn without_a_running_jarvis_the_reboot_says_so() {
        if CONTEXT.get().is_none() {
            assert!(reboot(Trigger::Voice).await.is_err());
            assert!(!auto_recover(), "no automatic reboot without a running engine");
        }
        assert!(is_tool(TOOL) && definitions()[0]["function"]["name"] == TOOL);
    }
}
