//! Built-in LLM engine.
//!
//! JAI ships its own copy of the Ollama engine in `runtime/ollama` and runs it as a hidden
//! child process on a private localhost port with a private model store (`models/llm`), so
//! nothing has to be installed or started separately.
//!
//! * Windows: the engine runs inside a job object, so it dies with JAI — even after a crash.
//! * A missing model is imported from a local Ollama store when one exists (a file copy, no
//!   download) and pulled from the Ollama registry otherwise.
//! * A crashed engine is restarted automatically.
//! * Without a bundled engine JAI falls back to an external Ollama at `ollama_url`.

use crate::config::JaiConfig;
use futures_util::StreamExt;
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::watch;
use tracing::{debug, info, warn};

const MAX_RESTARTS: u32 = 5;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Starting,
    Installing,
    Ready,
    Unavailable,
}

#[derive(Debug, Clone)]
pub struct EngineStatus {
    pub phase: Phase,
    /// Shown in the HUD, e.g. "built-in engine · 127.0.0.1:11435".
    pub detail: String,
}

pub struct LlmRuntime {
    url: String,
    /// Engine executable when the bundled engine is used.
    engine: Option<PathBuf>,
    models_dir: PathBuf,
    external_url: String,
    log_path: PathBuf,
    status: watch::Sender<EngineStatus>,
    child: Mutex<Option<Child>>,
    #[cfg(windows)]
    job: Option<job::Job>,
    install_lock: tokio::sync::Mutex<()>,
    stopping: AtomicBool,
    /// A deliberate restart (`restart_engine`) is running: the watchdog stays out of it.
    restarting: AtomicBool,
    client: reqwest::Client,
}

impl LlmRuntime {
    /// Starts the engine in the background; `wait_ready`/`settled` tell callers when it is usable.
    pub fn start(cfg: &JaiConfig) -> Arc<Self> {
        let engine = engine_binary(Path::new(&cfg.llm_runtime_dir)).map(|p| absolute(&p));
        let url = match &engine {
            Some(_) => format!("http://127.0.0.1:{}", free_port(cfg.llm_port)),
            None => cfg.ollama_url.trim_end_matches('/').to_string(),
        };
        let (status, _) = watch::channel(EngineStatus {
            phase: Phase::Starting,
            detail: "starting the language engine…".into(),
        });
        let runtime = Arc::new(Self {
            url,
            engine,
            models_dir: absolute(Path::new(&cfg.llm_models_dir)),
            external_url: cfg.ollama_url.trim_end_matches('/').to_string(),
            log_path: Path::new(&cfg.log_file).with_file_name("jarvis-llm.log"),
            status,
            child: Mutex::new(None),
            #[cfg(windows)]
            job: job::Job::kill_on_close(),
            install_lock: tokio::sync::Mutex::new(()),
            stopping: AtomicBool::new(false),
            restarting: AtomicBool::new(false),
            client: reqwest::Client::builder()
                .connect_timeout(Duration::from_secs(3))
                .read_timeout(Duration::from_secs(300))
                .no_proxy()
                .build()
                .unwrap_or_else(|_| reqwest::Client::new()),
        });
        let models = [cfg.primary_model.clone(), cfg.fallback_model.clone()];
        let supervisor = Arc::clone(&runtime);
        tokio::spawn(async move { supervisor.supervise(models).await });
        runtime
    }

    pub fn url(&self) -> &str {
        &self.url
    }

    pub fn status(&self) -> EngineStatus {
        self.status.borrow().clone()
    }

    pub fn subscribe(&self) -> watch::Receiver<EngineStatus> {
        self.status.subscribe()
    }

    fn set_status(&self, phase: Phase, detail: impl Into<String>) {
        self.status.send_replace(EngineStatus { phase, detail: detail.into() });
    }

    fn ready_detail(&self) -> String {
        let addr = self.url.trim_start_matches("http://");
        match self.engine {
            Some(_) => format!("built-in engine · {addr}"),
            None => format!("external Ollama · {addr}"),
        }
    }

    /// Ok once the engine can serve requests. Waits while it is starting; fails fast while a
    /// model is being installed or when the engine is unavailable.
    pub async fn wait_ready(&self, timeout: Duration) -> Result<(), EngineStatus> {
        let mut rx = self.status.subscribe();
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let status = rx.borrow_and_update().clone();
            match status.phase {
                Phase::Ready => return Ok(()),
                Phase::Installing | Phase::Unavailable => return Err(status),
                Phase::Starting => {}
            }
            if !matches!(tokio::time::timeout_at(deadline, rx.changed()).await, Ok(Ok(()))) {
                return Err(status);
            }
        }
    }

    /// Waits until start-up has settled (ready or failed). True when the engine is ready.
    pub async fn settled(&self) -> bool {
        let mut rx = self.status.subscribe();
        loop {
            match rx.borrow_and_update().phase {
                Phase::Ready => return true,
                Phase::Unavailable => return false,
                Phase::Starting | Phase::Installing => {}
            }
            if rx.changed().await.is_err() {
                return false;
            }
        }
    }

    async fn supervise(self: Arc<Self>, models: [String; 2]) {
        let Some(engine) = self.engine.clone() else {
            self.connect_external().await;
            return;
        };
        let [primary, fallback] = models;
        // VRAM held by a separately installed Ollama would push our model onto the CPU.
        self.release_external_vram(&[primary.as_str(), fallback.as_str()]).await;

        // Import before the engine starts so it sees the models right away. The fallback is
        // only copied when that is cheap (it usually shares the primary's weights).
        for (name, budget) in [(&primary, None), (&fallback, Some(64 << 20))] {
            if !name.is_empty() && !has_local_manifest(&self.models_dir, name) {
                self.import(name, budget).await;
            }
        }

        let started = Instant::now();
        self.set_status(Phase::Starting, "starting the built-in engine…");
        if let Err(e) = self.launch(&engine).await {
            // A start slowed by a game hogging the disk or GPU is not final (live: 40 s and more
            // after a cold boot): while the process lives, keep waiting for it.
            let late = self.exit_status().is_none() && self.wait_healthy(Duration::from_secs(120)).await.is_ok();
            if !late {
                warn!("[LLM] Built-in engine failed: {e}");
                self.set_status(Phase::Unavailable, format!("built-in engine failed: {e}"));
                return;
            }
            warn!("[LLM] Built-in engine answered late ({} ms)", started.elapsed().as_millis());
        }
        info!("[LLM] Built-in engine running at {} ({} ms)", self.url, started.elapsed().as_millis());

        if !primary.is_empty() && !self.has_model(&primary).await {
            if let Err(e) = self.pull(&primary).await {
                warn!("[LLM] Could not download '{primary}': {e}");
                if fallback.is_empty() || !self.has_model(&fallback).await {
                    self.set_status(Phase::Unavailable, format!("could not install '{primary}': {e}"));
                    return;
                }
            }
        }
        self.set_status(Phase::Ready, self.ready_detail());
        self.watchdog(&engine).await;
    }

    /// Spawns the engine and waits until it answers.
    async fn launch(&self, engine: &Path) -> Result<(), String> {
        self.spawn_engine(engine)?;
        self.wait_healthy(Duration::from_secs(40)).await
    }

    /// Restarts the engine if it stops unexpectedly (driver reset, out-of-memory kill, …).
    async fn watchdog(&self, engine: &Path) {
        let mut restarts = 0;
        loop {
            tokio::time::sleep(Duration::from_secs(2)).await;
            if self.stopping.load(Ordering::Acquire) {
                return;
            }
            if self.restarting.load(Ordering::Acquire) {
                continue;
            }
            let Some(exit) = self.exit_status() else { continue };
            restarts += 1;
            if restarts > MAX_RESTARTS {
                warn!("[LLM] The built-in engine keeps stopping ({exit}); giving up. See {}", self.log_path.display());
                self.set_status(Phase::Unavailable, format!("the engine keeps stopping ({exit})"));
                return;
            }
            warn!("[LLM] The built-in engine stopped unexpectedly ({exit}); restarting it ({restarts}/{MAX_RESTARTS}).");
            self.set_status(Phase::Starting, "restarting the built-in engine…");
            match self.launch(engine).await {
                Ok(()) => self.set_status(Phase::Ready, self.ready_detail()),
                Err(e) => warn!("[LLM] Restart failed: {e}"),
            }
        }
    }

    fn spawn_engine(&self, engine: &Path) -> Result<(), String> {
        std::fs::create_dir_all(&self.models_dir).map_err(|e| format!("{}: {e}", self.models_dir.display()))?;
        let log = std::fs::File::create(&self.log_path).map_err(|e| format!("{}: {e}", self.log_path.display()))?;
        let log_err = log.try_clone().map_err(|e| e.to_string())?;
        let mut cmd = Command::new(engine);
        cmd.arg("serve")
            .current_dir(engine.parent().unwrap_or(Path::new(".")))
            .stdin(Stdio::null())
            .stdout(Stdio::from(log))
            .stderr(Stdio::from(log_err));
        // Settings of a separately installed Ollama must not leak into the private engine.
        for (key, _) in std::env::vars_os() {
            if key.to_string_lossy().to_ascii_uppercase().starts_with("OLLAMA_") {
                cmd.env_remove(&key);
            }
        }
        cmd.env("OLLAMA_HOST", self.url.trim_start_matches("http://"))
            .env("OLLAMA_MODELS", &self.models_dir)
            // Fully local: no cloud models / remote inference.
            .env("OLLAMA_NO_CLOUD", "1")
            // Imported blobs must never be garbage-collected by the engine.
            .env("OLLAMA_NOPRUNE", "1")
            .env("OLLAMA_FLASH_ATTENTION", "1")
            // An 8-bit KV cache: the full 32k window in the VRAM the 20k one took at f16.
            .env("OLLAMA_KV_CACHE_TYPE", "q8_0")
            .env("OLLAMA_NUM_PARALLEL", "1")
            .env("OLLAMA_MAX_LOADED_MODELS", "1")
            .env("OLLAMA_KEEP_ALIVE", "-1");
        crate::shell::hide_console_std(&mut cmd);
        let child = cmd.spawn().map_err(|e| format!("{}: {e}", engine.display()))?;
        self.adopt(child);
        Ok(())
    }

    /// Keeps the process handle and ties the process to JAI's lifetime.
    fn adopt(&self, child: Child) {
        #[cfg(windows)]
        if !self.job.as_ref().is_some_and(|job| job.assign(&child)) {
            warn!("[LLM] Could not tie the engine to JAI's lifetime (job object).");
        }
        *self.child.lock().unwrap_or_else(|e| e.into_inner()) = Some(child);
    }

    fn exit_status(&self) -> Option<String> {
        let mut guard = self.child.lock().unwrap_or_else(|e| e.into_inner());
        match guard.as_mut()?.try_wait() {
            Ok(Some(status)) => Some(status.to_string()),
            _ => None,
        }
    }

    async fn wait_healthy(&self, timeout: Duration) -> Result<(), String> {
        let started = Instant::now();
        loop {
            let probe = self
                .client
                .get(format!("{}/api/version", self.url))
                .timeout(Duration::from_millis(800))
                .send()
                .await;
            if probe.is_ok_and(|r| r.status().is_success()) {
                return Ok(());
            }
            if let Some(exit) = self.exit_status() {
                return Err(format!("the engine stopped ({exit}): {}", log_tail(&self.log_path, 3)));
            }
            if started.elapsed() > timeout {
                return Err(format!("the engine did not answer within {} s", timeout.as_secs()));
            }
            tokio::time::sleep(Duration::from_millis(150)).await;
        }
    }

    /// Names of the installed models.
    pub async fn list_models(&self) -> Vec<String> {
        let resp = self
            .client
            .get(format!("{}/api/tags", self.url))
            .timeout(Duration::from_secs(3))
            .send()
            .await;
        match resp {
            Ok(r) => r
                .json::<Value>()
                .await
                .ok()
                .and_then(|v| v["models"].as_array().map(|a| a.iter().filter_map(|m| m["name"].as_str().map(String::from)).collect()))
                .unwrap_or_default(),
            Err(e) => {
                debug!("[LLM] Could not list models: {e}");
                Vec::new()
            }
        }
    }

    async fn has_model(&self, name: &str) -> bool {
        self.list_models().await.iter().any(|m| same_model(m, name))
    }

    /// Copies `name` from a local Ollama store into the private store (blocking work off the runtime).
    async fn import(self: &Arc<Self>, name: &str, budget: Option<u64>) -> bool {
        let runtime = Arc::clone(self);
        let label = name.to_string();
        let result = tokio::task::spawn_blocking(move || {
            import_model(&label, &runtime.models_dir, budget, &|done, total| {
                let pct = (done * 100).checked_div(total).unwrap_or(100);
                runtime.set_status(Phase::Installing, format!("installing {label} — {pct} % (local copy, no download)"));
            })
        })
        .await;
        match result {
            Ok(Ok(Some(source))) => {
                info!("[LLM] Imported '{name}' from {}", source.display());
                true
            }
            Ok(Ok(None)) => false,
            Ok(Err(e)) => {
                warn!("[LLM] Could not import '{name}': {e}");
                false
            }
            Err(e) => {
                warn!("[LLM] Import of '{name}' failed: {e}");
                false
            }
        }
    }

    /// Downloads `name` through the engine, reporting progress in the HUD.
    async fn pull(&self, name: &str) -> Result<(), String> {
        info!("[LLM] Downloading '{name}' into the built-in engine…");
        self.set_status(Phase::Installing, format!("downloading {name}…"));
        let resp = self
            .client
            .post(format!("{}/api/pull", self.url))
            .json(&json!({ "model": name, "stream": true }))
            .send()
            .await
            .map_err(|e| e.to_string())?;
        if !resp.status().is_success() {
            let status = resp.status();
            return Err(format!("HTTP {status}: {}", resp.text().await.unwrap_or_default()));
        }
        let mut stream = resp.bytes_stream();
        let mut buf: Vec<u8> = Vec::new();
        let mut last_report = Instant::now();
        while let Some(chunk) = stream.next().await {
            buf.extend_from_slice(&chunk.map_err(|e| e.to_string())?);
            while let Some(pos) = buf.iter().position(|b| *b == b'\n') {
                let line: Vec<u8> = buf.drain(..=pos).collect();
                let Ok(v) = serde_json::from_slice::<Value>(&line) else { continue };
                if let Some(e) = v["error"].as_str() {
                    return Err(e.to_string());
                }
                if v["status"] == "success" {
                    info!("[LLM] '{name}' downloaded");
                    return Ok(());
                }
                if let (Some(total), Some(done)) = (v["total"].as_u64(), v["completed"].as_u64()) {
                    if total > 0 && last_report.elapsed() > Duration::from_secs(2) {
                        last_report = Instant::now();
                        let pct = done * 100 / total;
                        self.set_status(Phase::Installing, format!("downloading {name} — {pct} % of {:.1} GB", total as f64 / 1e9));
                        info!("[LLM]   {name}: {pct} % ({:.0}/{:.0} MB)", done as f64 / 1e6, total as f64 / 1e6);
                    }
                }
            }
        }
        Err("the download ended unexpectedly".into())
    }

    /// Makes `name` available (local import, else download). Used when Sir picks another model.
    pub async fn ensure_model(self: &Arc<Self>, name: &str) -> Result<(), String> {
        let _guard = self.install_lock.lock().await;
        if !self.settled().await {
            return Err(self.status().detail);
        }
        if self.has_model(name).await {
            return Ok(());
        }
        if self.engine.is_none() {
            return Err(format!("'{name}' is not installed in the external Ollama"));
        }
        let result = if self.import(name, None).await && self.has_model(name).await {
            Ok(())
        } else {
            self.pull(name).await
        };
        self.set_status(Phase::Ready, self.ready_detail());
        result
    }

    /// Legacy mode without a bundled engine: use (or start) a separately installed Ollama.
    async fn connect_external(&self) {
        let url = self.url.clone();
        let result = tokio::task::spawn_blocking(move || start_external(&url))
            .await
            .unwrap_or_else(|e| Err(e.to_string()));
        match result {
            Ok(child) => {
                if let Some(child) = child {
                    self.adopt(child);
                }
                info!("[LLM] Using the external Ollama at {}", self.url);
                self.set_status(Phase::Ready, self.ready_detail());
            }
            Err(e) => {
                warn!("[LLM] {e}");
                self.set_status(Phase::Unavailable, e);
            }
        }
    }

    /// Unloads JAI's models from a separately installed Ollama so their VRAM is free for the
    /// built-in engine; other models are only reported.
    async fn release_external_vram(&self, ours: &[&str]) {
        let Some(addr) = socket_addr(&self.external_url) else { return };
        if !addr.ip().is_loopback() || !reachable(&addr, Duration::from_millis(250)) {
            return;
        }
        let ps = match self
            .client
            .get(format!("{}/api/ps", self.external_url))
            .timeout(Duration::from_secs(2))
            .send()
            .await
        {
            Ok(r) => r.json::<Value>().await.unwrap_or_default(),
            Err(_) => return,
        };
        for m in ps["models"].as_array().into_iter().flatten() {
            let name = m["name"].as_str().unwrap_or("");
            let vram_gb = m["size_vram"].as_u64().unwrap_or(0) as f64 / 1e9;
            if ours.iter().any(|o| !o.is_empty() && same_model(o, name)) {
                let unloaded = self
                    .client
                    .post(format!("{}/api/generate", self.external_url))
                    .json(&json!({ "model": name, "keep_alive": 0 }))
                    .timeout(Duration::from_secs(15))
                    .send()
                    .await
                    .is_ok_and(|r| r.status().is_success());
                if unloaded {
                    info!("[LLM] Freed {vram_gb:.1} GB of VRAM held by the separately installed Ollama ('{name}').");
                }
            } else if vram_gb > 0.5 {
                warn!(
                    "[LLM] A separately installed Ollama holds '{name}' in VRAM ({vram_gb:.1} GB); JAI's model may not fit on the GPU."
                );
            }
        }
    }

    /// Kills the built-in engine and starts it fresh (`llm_reboot`); an external Ollama is not
    /// JAI's to restart.
    pub async fn restart_engine(&self) -> Result<(), String> {
        let Some(engine) = self.engine.clone() else {
            return Err(format!("the language engine at {} is an external Ollama, which JAI cannot restart", self.url));
        };
        if self.restarting.swap(true, Ordering::AcqRel) {
            return Err("the engine is already restarting".into());
        }
        if let Some(mut child) = self.child.lock().unwrap_or_else(|e| e.into_inner()).take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        self.set_status(Phase::Starting, "rebooting the built-in engine…");
        let result = self.launch(&engine).await;
        match &result {
            Ok(()) => {
                info!("[LLM] Built-in engine restarted at {}", self.url);
                self.set_status(Phase::Ready, self.ready_detail());
            }
            Err(e) => {
                warn!("[LLM] The engine did not come back: {e}");
                self.set_status(Phase::Unavailable, format!("restart failed: {e}"));
            }
        }
        self.restarting.store(false, Ordering::Release);
        result
    }

    /// Stops the engine (also happens automatically when JAI exits).
    pub fn shutdown(&self) {
        self.stopping.store(true, Ordering::Release);
        if let Some(mut child) = self.child.lock().unwrap_or_else(|e| e.into_inner()).take() {
            let _ = child.kill();
            let _ = child.wait();
            info!("[LLM] Language engine stopped.");
        }
    }
}

impl Drop for LlmRuntime {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// The engine executable inside `dir` (`runtime/ollama`).
pub fn engine_binary(dir: &Path) -> Option<PathBuf> {
    let names: &[&str] = if cfg!(windows) { &["ollama.exe"] } else { &["ollama", "bin/ollama"] };
    names.iter().map(|n| dir.join(n)).find(|p| p.is_file())
}

/// True when the private store already holds a manifest for `name`.
pub fn has_local_manifest(store: &Path, name: &str) -> bool {
    parse_model_name(name).is_some_and(|r| r.manifest_paths().iter().any(|p| store.join(p).is_file()))
}

fn absolute(p: &Path) -> PathBuf {
    std::path::absolute(p).unwrap_or_else(|_| p.to_path_buf())
}

/// `qwen3` and `qwen3:latest` are the same model; names are case-insensitive.
fn canonical(name: &str) -> String {
    let n = name.trim().to_lowercase();
    if n.rsplit('/').next().unwrap_or(&n).contains(':') {
        n
    } else {
        format!("{n}:latest")
    }
}

fn same_model(a: &str, b: &str) -> bool {
    canonical(a) == canonical(b)
}

/// The model to talk to: the first of `preferred` that is installed, else the first installed
/// chat model (a missing tag never leaves JAI without a language model).
pub fn pick_model(installed: &[String], preferred: &[String]) -> Option<String> {
    let found = preferred.iter().filter(|p| !p.trim().is_empty()).find_map(|p| installed.iter().find(|i| same_model(p, i)));
    found.or_else(|| installed.iter().find(|i| !i.to_lowercase().contains("embed"))).cloned()
}

/// First free localhost port at or after `preferred`.
fn free_port(preferred: u16) -> u16 {
    let free = |port: u16| {
        let addr = SocketAddr::from(([127, 0, 0, 1], port));
        TcpStream::connect_timeout(&addr, Duration::from_millis(80)).is_err() && TcpListener::bind(addr).is_ok()
    };
    (preferred.max(1)..preferred.saturating_add(16))
        .find(|p| free(*p))
        .or_else(|| TcpListener::bind("127.0.0.1:0").ok().and_then(|l| l.local_addr().ok()).map(|a| a.port()))
        .unwrap_or(preferred)
}

fn socket_addr(url: &str) -> Option<SocketAddr> {
    let hostport = url.split("://").nth(1).unwrap_or(url).split('/').next()?.to_string();
    let hostport = if hostport.contains(':') { hostport } else { format!("{hostport}:11434") };
    hostport.to_socket_addrs().ok()?.next()
}

fn reachable(addr: &SocketAddr, timeout: Duration) -> bool {
    TcpStream::connect_timeout(addr, timeout).is_ok()
}

fn log_tail(path: &Path, lines: usize) -> String {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let tail: Vec<&str> = text.lines().rev().take(lines).collect();
    tail.into_iter().rev().collect::<Vec<_>>().join(" | ")
}

// ---------------------------------------------------------------------------------------------
// Local import from an Ollama model store
// ---------------------------------------------------------------------------------------------

/// `[host/][namespace/]model[:tag]`, as Ollama parses model names.
struct ModelRef {
    host: Option<String>,
    namespace: String,
    model: String,
    tag: String,
}

fn parse_model_name(name: &str) -> Option<ModelRef> {
    let name = name.trim();
    let (path, tag) = match name.rfind(':') {
        Some(i) if !name[i + 1..].contains('/') => (&name[..i], &name[i + 1..]),
        _ => (name, "latest"),
    };
    let parts: Vec<&str> = path.split('/').filter(|p| !p.is_empty()).collect();
    let (host, namespace, model) = match parts.as_slice() {
        [m] => (None, "library", *m),
        [ns, m] => (None, *ns, *m),
        [h, ns, m] => (Some(h.to_string()), *ns, *m),
        _ => return None,
    };
    let valid = |s: &str| !s.is_empty() && s != "." && s != ".." && !s.contains(['\\', ':']);
    if !valid(namespace) || !valid(model) || !valid(tag) || host.as_deref().is_some_and(|h| h.contains(['\\', '/'])) {
        return None;
    }
    Some(ModelRef { host, namespace: namespace.into(), model: model.into(), tag: tag.into() })
}

impl ModelRef {
    /// Manifest locations relative to a store root, newest layout first.
    fn manifest_paths(&self) -> Vec<PathBuf> {
        let tail = |host: &str| PathBuf::from(host).join(&self.namespace).join(&self.model).join(&self.tag);
        match &self.host {
            Some(h) => vec![Path::new("manifests-v2").join(tail(h)), Path::new("manifests").join(tail(h))],
            None => vec![
                Path::new("manifests-v2").join(tail("ollama.com")),
                Path::new("manifests").join(tail("registry.ollama.ai")),
            ],
        }
    }
}

/// Model stores of separately installed Ollama setups (sources for a local import).
fn foreign_stores(own: &Path) -> Vec<PathBuf> {
    let mut stores: Vec<PathBuf> = Vec::new();
    if let Some(p) = std::env::var_os("OLLAMA_MODELS") {
        stores.push(PathBuf::from(p));
    }
    if let Some(home) = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }) {
        stores.push(PathBuf::from(home).join(".ollama").join("models"));
    }
    if cfg!(unix) {
        stores.push(PathBuf::from("/usr/share/ollama/.ollama/models"));
        stores.push(PathBuf::from("/var/lib/ollama/.ollama/models"));
    }
    let own = absolute(own);
    stores.retain(|s| s.join("blobs").is_dir() && absolute(s) != own);
    stores.dedup();
    stores
}

/// `sha256:<64 hex>` → blob file name; anything else is rejected (no path tricks).
fn blob_file(digest: &str) -> Option<String> {
    let hex = digest.strip_prefix("sha256:")?;
    (hex.len() == 64 && hex.bytes().all(|b| b.is_ascii_hexdigit())).then(|| format!("sha256-{hex}"))
}

/// Copies model `name` (blobs, then manifest) from a local Ollama store into `dest`. `budget`
/// caps the bytes that may be copied. Returns the source store, or None when there is no
/// local copy (or it would exceed the budget).
fn import_model(name: &str, dest: &Path, budget: Option<u64>, progress: &dyn Fn(u64, u64)) -> Result<Option<PathBuf>, String> {
    import_from(&foreign_stores(dest), name, dest, budget, progress)
}

fn import_from(
    stores: &[PathBuf],
    name: &str,
    dest: &Path,
    budget: Option<u64>,
    progress: &dyn Fn(u64, u64),
) -> Result<Option<PathBuf>, String> {
    let model = parse_model_name(name).ok_or_else(|| format!("invalid model name '{name}'"))?;
    for store in stores {
        for rel in model.manifest_paths() {
            let Ok(raw) = std::fs::read(store.join(&rel)) else { continue };
            let manifest: Value = serde_json::from_slice(&raw).map_err(|e| format!("{}: {e}", rel.display()))?;
            let mut blobs = Vec::new();
            for layer in std::iter::once(&manifest["config"]).chain(manifest["layers"].as_array().into_iter().flatten()) {
                let digest = layer["digest"].as_str().unwrap_or_default();
                let file = blob_file(digest).ok_or_else(|| format!("unexpected digest '{digest}' in {}", rel.display()))?;
                let src = store.join("blobs").join(&file);
                let size = std::fs::metadata(&src)
                    .map_err(|_| format!("{file} is missing in {}", store.display()))?
                    .len();
                let present = std::fs::metadata(dest.join("blobs").join(&file)).is_ok_and(|m| m.len() == size);
                blobs.push((file, src, size, present));
            }
            let total: u64 = blobs.iter().filter(|b| !b.3).map(|b| b.2).sum();
            if budget.is_some_and(|max| total > max) {
                return Ok(None);
            }
            if total > 0 {
                info!(
                    "[LLM] Installing '{name}' ({:.1} GB) from {} — one-time local copy, no download",
                    total as f64 / 1e9,
                    store.display()
                );
            }
            let staging = dest.join("import-tmp");
            for dir in [dest.join("blobs"), staging.clone()] {
                std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
            }
            let mut done = 0u64;
            for (file, src, _, present) in &blobs {
                if !present {
                    let tmp = staging.join(file);
                    copy_with_progress(src, &tmp, &mut done, total, progress).map_err(|e| format!("copying {file}: {e}"))?;
                    std::fs::rename(&tmp, dest.join("blobs").join(file)).map_err(|e| format!("{file}: {e}"))?;
                }
                // Cached model metadata, if the source has it (speeds up the first `show`).
                let meta = store.join("metadata").join(format!("{file}.json"));
                let meta_dest = dest.join("metadata").join(format!("{file}.json"));
                if meta.is_file() && !meta_dest.exists() && std::fs::create_dir_all(dest.join("metadata")).is_ok() {
                    let _ = std::fs::copy(&meta, &meta_dest);
                }
            }
            let target = dest.join(&rel);
            if let Some(dir) = target.parent() {
                std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
            }
            let tmp_manifest = staging.join("manifest.tmp");
            std::fs::write(&tmp_manifest, &raw).map_err(|e| e.to_string())?;
            std::fs::rename(&tmp_manifest, &target).map_err(|e| format!("{}: {e}", target.display()))?;
            let _ = std::fs::remove_dir(&staging);
            return Ok(Some(store.clone()));
        }
    }
    Ok(None)
}

fn copy_with_progress(src: &Path, dst: &Path, done: &mut u64, total: u64, progress: &dyn Fn(u64, u64)) -> std::io::Result<()> {
    let mut input = std::fs::File::open(src)?;
    let mut output = std::fs::File::create(dst)?;
    let mut buf = vec![0u8; 8 << 20];
    let mut last = Instant::now();
    loop {
        let n = input.read(&mut buf)?;
        if n == 0 {
            break;
        }
        output.write_all(&buf[..n])?;
        *done += n as u64;
        if last.elapsed() >= Duration::from_millis(750) {
            last = Instant::now();
            progress(*done, total);
        }
    }
    output.sync_all()?;
    progress(*done, total);
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Legacy: separately installed Ollama
// ---------------------------------------------------------------------------------------------

fn installed_ollama() -> Vec<PathBuf> {
    let mut list = Vec::new();
    if let Some(p) = crate::apps::which("ollama") {
        list.push(p);
    }
    if cfg!(windows) {
        if let Ok(local) = std::env::var("LOCALAPPDATA") {
            list.push(PathBuf::from(local).join("Programs").join("Ollama").join("ollama.exe"));
        }
        if let Ok(pf) = std::env::var("ProgramFiles") {
            list.push(PathBuf::from(pf).join("Ollama").join("ollama.exe"));
        }
    } else {
        list.push(PathBuf::from("/usr/local/bin/ollama"));
        list.push(PathBuf::from("/usr/bin/ollama"));
    }
    list.into_iter().filter(|p| p.exists()).collect()
}

/// Blocking: connects to the Ollama at `url`, starting an installed `ollama serve` if needed.
fn start_external(url: &str) -> Result<Option<Child>, String> {
    let addr = socket_addr(url).ok_or_else(|| format!("invalid Ollama URL '{url}'"))?;
    if reachable(&addr, Duration::from_millis(300)) {
        return Ok(None);
    }
    if !addr.ip().is_loopback() {
        return Err(format!("the Ollama server at {addr} is not reachable"));
    }
    for bin in installed_ollama() {
        let mut cmd = Command::new(&bin);
        cmd.arg("serve")
            .env("OLLAMA_HOST", addr.to_string())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        for (key, value) in [("OLLAMA_FLASH_ATTENTION", "1"), ("OLLAMA_NUM_PARALLEL", "1")] {
            if std::env::var_os(key).is_none() {
                cmd.env(key, value);
            }
        }
        crate::shell::hide_console_std(&mut cmd);
        match cmd.spawn() {
            Ok(child) => {
                let started = Instant::now();
                while started.elapsed() < Duration::from_secs(10) && !reachable(&addr, Duration::from_millis(150)) {
                    std::thread::sleep(Duration::from_millis(150));
                }
                info!("[LLM] Started {} in {} ms", bin.display(), started.elapsed().as_millis());
                return Ok(Some(child));
            }
            Err(e) => warn!("[LLM] Could not start {}: {e}", bin.display()),
        }
    }
    Err("no language engine: runtime/ollama is missing and Ollama is not installed".into())
}

#[cfg(windows)]
mod job {
    use std::ffi::c_void;
    use std::os::windows::io::AsRawHandle;
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{CloseHandle, HANDLE};
    use windows::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation, SetInformationJobObject,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };

    /// Processes in this job are killed when its last handle closes — at the latest when
    /// JAI exits or crashes.
    pub struct Job(HANDLE);

    // SAFETY: a job object handle may be used from any thread.
    unsafe impl Send for Job {}
    unsafe impl Sync for Job {}

    impl Job {
        pub fn kill_on_close() -> Option<Self> {
            // SAFETY: plain object creation; `info` outlives the call.
            unsafe {
                let handle = CreateJobObjectW(None, PCWSTR::null()).ok()?;
                let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
                info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
                let set = SetInformationJobObject(
                    handle,
                    JobObjectExtendedLimitInformation,
                    &info as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION as *const c_void,
                    std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                );
                if set.is_err() {
                    let _ = CloseHandle(handle);
                    return None;
                }
                Some(Self(handle))
            }
        }

        pub fn assign(&self, child: &std::process::Child) -> bool {
            // SAFETY: both handles are valid for the duration of the call.
            unsafe { AssignProcessToJobObject(self.0, HANDLE(child.as_raw_handle() as isize)).is_ok() }
        }
    }

    impl Drop for Job {
        fn drop(&mut self) {
            // SAFETY: the handle is owned by this struct.
            unsafe {
                let _ = CloseHandle(self.0);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_names_map_to_manifest_paths() {
        let r = parse_model_name("qwen3:30b-a3b").unwrap();
        assert_eq!(
            r.manifest_paths(),
            vec![
                PathBuf::from("manifests-v2/ollama.com/library/qwen3/30b-a3b"),
                PathBuf::from("manifests/registry.ollama.ai/library/qwen3/30b-a3b"),
            ]
        );
        let r = parse_model_name("hf.co/bartowski/Llama-3.2-3B-GGUF:Q4_K_M").unwrap();
        assert_eq!(r.host.as_deref(), Some("hf.co"));
        assert_eq!((r.namespace.as_str(), r.model.as_str(), r.tag.as_str()), ("bartowski", "Llama-3.2-3B-GGUF", "Q4_K_M"));
        assert_eq!(parse_model_name("granite4.1").unwrap().tag, "latest");
        assert!(parse_model_name("../evil:x").is_none());
        assert!(parse_model_name("").is_none());
    }

    #[test]
    fn model_name_comparison_ignores_latest_and_case() {
        assert!(same_model("Qwen3", "qwen3:latest"));
        assert!(same_model("qwen2.5:7b-instruct", "QWEN2.5:7B-INSTRUCT"));
        assert!(!same_model("qwen2.5:7b", "qwen2.5:7b-instruct"));
    }

    #[test]
    fn blob_digests_are_validated() {
        let hex = "2bada8a7450677000f678be90653b85d364de7db25eb5ea54136ada5f3933730";
        assert_eq!(blob_file(&format!("sha256:{hex}")), Some(format!("sha256-{hex}")));
        assert_eq!(blob_file("sha256:../../etc/passwd"), None);
        assert_eq!(blob_file("md5:abc"), None);
    }

    #[test]
    fn import_copies_blobs_then_manifest_and_respects_the_budget() {
        let root = std::env::temp_dir().join(format!("jarvis_import_{}", uuid::Uuid::new_v4()));
        let (src, dest) = (root.join("src"), root.join("dest"));
        let digest = |c: char| format!("sha256:{}", c.to_string().repeat(64));
        let manifest = json!({
            "config": { "digest": digest('a'), "size": 3 },
            "layers": [{ "digest": digest('b'), "size": 5 }]
        });
        let rel = Path::new("manifests/registry.ollama.ai/library/tiny/1b");
        std::fs::create_dir_all(src.join(rel).parent().unwrap()).unwrap();
        std::fs::create_dir_all(src.join("blobs")).unwrap();
        std::fs::write(src.join(rel), manifest.to_string()).unwrap();
        std::fs::write(src.join("blobs").join(blob_file(&digest('a')).unwrap()), b"cfg").unwrap();
        std::fs::write(src.join("blobs").join(blob_file(&digest('b')).unwrap()), b"model").unwrap();
        let stores = vec![src.clone()];

        // Over budget: nothing is copied.
        assert_eq!(import_from(&stores, "tiny:1b", &dest, Some(4), &|_, _| {}).unwrap(), None);
        assert!(!has_local_manifest(&dest, "tiny:1b"));

        let copied = std::sync::atomic::AtomicU64::new(0);
        let source = import_from(&stores, "tiny:1b", &dest, None, &|done, _| copied.store(done, Ordering::Relaxed)).unwrap();
        assert_eq!(source, Some(src.clone()));
        assert_eq!(copied.load(Ordering::Relaxed), 8);
        assert!(has_local_manifest(&dest, "tiny:1b"));
        assert_eq!(std::fs::read(dest.join("blobs").join(blob_file(&digest('b')).unwrap())).unwrap(), b"model");
        assert!(!dest.join("import-tmp").exists(), "staging folder is cleaned up");

        // Already present: a second import copies nothing, even with a zero budget.
        assert_eq!(import_from(&stores, "tiny:1b", &dest, Some(0), &|_, _| {}).unwrap(), Some(src));
        assert_eq!(import_from(&stores, "missing:1b", &dest, None, &|_, _| {}).unwrap(), None);
        let _ = std::fs::remove_dir_all(root);
    }
}
