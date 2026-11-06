//! The Tool Forge: Jarvis builds himself new tools in pure Rust when Sir asks for something no
//! built-in tool covers ("create a tool that …", "mach dir ein eigenes Tool dafür") or tells him
//! he did it wrong ("don't use search, do it this way instead").
//!
//! * A forged tool is a `ForgedToolSpec`: trigger phrases with `{slots}` ("play {song} from my
//!   playlist {playlist}") and a chain of native `RustStep`s — scan a directory, parse a file or
//!   SQLite database, launch a process (no shells, no window), act on a window, call an HTTP API,
//!   compile and run a small Rust plugin with `rustc -O`, or call one of Jarvis' own tools.
//! * Forging runs the tool straight away with Sir's own arguments; only a run that succeeds is
//!   saved (`forged_tools` in `jarvis_memory.db` + `skills/forged_tools/<name>.json`). A failed
//!   run is reported back so the model can repair the spec (the brain retries once).
//! * Its triggers are matched before the model on every later turn, and a correction replaces
//!   the definition (version + 1, the correction kept) — so the fix is permanent.

use regex::Regex;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};
use tracing::{info, warn};

const MAX_STEPS: usize = 12;
const MAX_OUTPUT: usize = 4000;
const STEP_TIMEOUT: Duration = Duration::from_secs(25);
/// Interpreters and system tools a forged tool may not launch: forged tools stay native Rust.
const FORBIDDEN_BINARIES: &[&str] = &[
    "cmd", "powershell", "pwsh", "bash", "sh", "wsl", "python", "python3", "py", "pythonw", "node", "perl", "ruby", "wscript", "cscript", "mshta", "rundll32",
    "regsvr32", "reg", "diskpart", "format", "shutdown", "bcdedit", "vssadmin", "takeown", "icacls",
];
/// Jarvis tools a forged step may not call (no recursion, no self-modification, no quitting).
const FORBIDDEN_TOOLS: &[&str] = &[
    "forge_tool", "correct_forged_tool", "run_forged_tool", "delete_forged_tool", "shutdown_jarvis", "control_jarvis_lifecycle", "patch_own_source",
    "recompile_and_restart", "restore_source_backup", "system_power", "create_skill", "delete_skill",
];

/// One native step. String fields take `{slot}` (Sir's arguments), `{prev}` (the previous
/// step's output) and `{prev_first}` (its first line); `%APPDATA%`, `%LOCALAPPDATA%`,
/// `%USERPROFILE%` and `~` are expanded in paths.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "step", rename_all = "snake_case")]
pub enum RustStep {
    ScanDirectory {
        root: String,
        #[serde(default)]
        extensions: Vec<String>,
        #[serde(default)]
        filter_regex: Option<String>,
    },
    ParseFileOrSqlite {
        path: String,
        #[serde(default)]
        query_or_json_path: Option<String>,
    },
    LaunchOrInvokeProcess {
        binary: String,
        #[serde(default)]
        args: Vec<String>,
        #[serde(default)]
        working_dir: Option<String>,
    },
    Win32WindowAction { window_title_match: String, action: String },
    HttpRequest {
        #[serde(default = "get")]
        method: String,
        url: String,
        #[serde(default)]
        headers: Vec<(String, String)>,
        #[serde(default)]
        body_template: Option<String>,
    },
    CompileAndRunRustPlugin {
        rust_source_code: String,
        #[serde(default)]
        args: Vec<String>,
    },
    /// One of Jarvis' own tools ("control_music", "window_management", …).
    JarvisTool {
        name: String,
        #[serde(default)]
        args: Value,
    },
}

fn get() -> String {
    "GET".into()
}

impl RustStep {
    /// One line for the "New Tool Forged" card.
    pub fn summary(&self) -> String {
        match self {
            Self::ScanDirectory { root, extensions, .. } => format!("scan {root} for {}", if extensions.is_empty() { "files".into() } else { extensions.join("/") }),
            Self::ParseFileOrSqlite { path, query_or_json_path } => format!("read {path}{}", query_or_json_path.as_ref().map(|q| format!(" ({})", truncate(q, 40))).unwrap_or_default()),
            Self::LaunchOrInvokeProcess { binary, .. } => format!("launch {binary}"),
            Self::Win32WindowAction { window_title_match, action } => format!("{action} window '{window_title_match}'"),
            Self::HttpRequest { method, url, .. } => format!("{method} {}", truncate(url, 60)),
            Self::CompileAndRunRustPlugin { .. } => "compile + run a native Rust plugin".into(),
            Self::JarvisTool { name, .. } => format!("call {name}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ForgedToolSpec {
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// "play {song} from my playlist {playlist}", "spiel {song} aus meiner playlist {playlist}".
    #[serde(default)]
    pub triggers: Vec<String>,
    pub steps: Vec<RustStep>,
    #[serde(default)]
    pub version: u32,
    /// Sir's corrections, oldest first.
    #[serde(default)]
    pub corrections: Vec<String>,
}

impl ForgedToolSpec {
    /// The `{slots}` its triggers capture.
    pub fn slots(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for t in &self.triggers {
            for c in SLOT.captures_iter(t) {
                if !out.contains(&c[1].to_string()) {
                    out.push(c[1].to_string());
                }
            }
        }
        out
    }

    pub fn validate(&self) -> Result<(), String> {
        if !Regex::new(r"^[a-z][a-z0-9_]{1,47}$").expect("name expression").is_match(&self.name) {
            return Err(format!("'{}' is not a snake_case tool name.", self.name));
        }
        if self.steps.is_empty() || self.steps.len() > MAX_STEPS {
            return Err(format!("A forged tool needs 1 to {MAX_STEPS} steps."));
        }
        for step in &self.steps {
            match step {
                RustStep::LaunchOrInvokeProcess { binary, .. } => {
                    let stem = Path::new(binary).file_stem().map(|s| s.to_string_lossy().to_lowercase()).unwrap_or_default();
                    if FORBIDDEN_BINARIES.contains(&stem.as_str()) {
                        return Err(format!("'{binary}' is a shell or system tool; forged tools stay native (use a Rust plugin step instead)."));
                    }
                }
                RustStep::JarvisTool { name, .. } if FORBIDDEN_TOOLS.contains(&name.as_str()) => {
                    return Err(format!("A forged tool may not call '{name}'."));
                }
                RustStep::HttpRequest { method, url, .. } => {
                    if !matches!(method.to_uppercase().as_str(), "GET" | "POST" | "PUT" | "PATCH" | "DELETE") {
                        return Err(format!("Unsupported HTTP method '{method}'."));
                    }
                    if !(url.starts_with("http://") || url.starts_with("https://") || url.starts_with('{')) {
                        return Err(format!("'{url}' is not an http(s) URL."));
                    }
                }
                RustStep::ScanDirectory { filter_regex: Some(r), .. } => {
                    // Checked with its {slots} filled, as it will run.
                    let sample = SLOT.replace_all(r, "x").replace("{prev_first}", "x").replace("{prev}", "x");
                    Regex::new(&sample).map_err(|e| format!("Invalid filter_regex: {e}"))?;
                }
                _ => {}
            }
        }
        Ok(())
    }
}

static SLOT: std::sync::LazyLock<Regex> = std::sync::LazyLock::new(|| Regex::new(r"\{([a-z_][a-z0-9_]*)\}").expect("slot expression"));

/// Lower case, punctuation gone, courtesy stripped ("Jarvis, could you play X please?").
fn normalize(text: &str) -> String {
    crate::intent_engine::command_core(text)
}

/// "play {song} from my playlist {playlist}" → an anchored expression with named groups.
fn trigger_regex(trigger: &str) -> Option<Regex> {
    // Slots become plain-word markers so normalising cannot split them ("song_name").
    let names: Vec<String> = SLOT.captures_iter(trigger).map(|c| c[1].to_string()).collect();
    let mut i = 0;
    let marked = SLOT.replace_all(trigger, |_: &regex::Captures| {
        i += 1;
        format!(" slotmark{}x ", i - 1)
    });
    let norm = normalize(&marked);
    let mut pattern = String::from("^");
    let mut rest = norm.as_str();
    let mark = Regex::new(r"slotmark(\d+)x").expect("mark expression");
    while let Some(m) = mark.captures(rest) {
        let whole = m.get(0)?;
        let name = names.get(m[1].parse::<usize>().ok()?)?;
        pattern.push_str(&regex::escape(&rest[..whole.start()]));
        pattern.push_str(&format!("(?P<{name}>.+?)"));
        rest = &rest[whole.end()..];
    }
    pattern.push_str(&regex::escape(rest));
    pattern.push('$');
    Regex::new(&pattern).ok()
}

/// Sir's words against a tool's triggers: the captured slots, or `None`.
pub fn match_triggers(spec: &ForgedToolSpec, text: &str) -> Option<Value> {
    let said = normalize(text);
    if said.is_empty() {
        return None;
    }
    for t in &spec.triggers {
        let Some(rx) = trigger_regex(t) else { continue };
        if let Some(c) = rx.captures(&said) {
            let mut args = json!({});
            for name in rx.capture_names().flatten() {
                if let Some(v) = c.name(name) {
                    args[name] = json!(v.as_str().trim());
                }
            }
            return Some(args);
        }
        // A slot-free trigger also takes a near miss ("open my dev setup" for "open my dev set up").
        if !SLOT.is_match(t) && strsim::jaro_winkler(&normalize(t), &said) >= 0.95 {
            return Some(json!({}));
        }
    }
    None
}

// ------------------------------------------------------------------------- execution ----

/// What a step needs from the running Jarvis.
#[allow(async_fn_in_trait)]
pub trait StepHost {
    async fn jarvis_tool(&self, name: &str, args: &Value) -> Result<String, String>;
}

impl StepHost for crate::tools::ToolContext {
    async fn jarvis_tool(&self, name: &str, args: &Value) -> Result<String, String> {
        let outcome = Box::pin(self.execute(name, args)).await;
        if outcome.success {
            Ok(outcome.output)
        } else {
            Err(outcome.output.trim_start_matches("Error: ").to_string())
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ForgeRun {
    pub name: String,
    pub version: u32,
    pub output: String,
    pub steps: Vec<String>,
    pub elapsed_ms: u128,
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        s.chars().take(max).collect::<String>() + "…"
    }
}

fn expand_env(s: &str) -> String {
    let mut out = s.to_string();
    for var in ["APPDATA", "LOCALAPPDATA", "USERPROFILE", "PROGRAMFILES", "TEMP"] {
        if let Ok(v) = std::env::var(var) {
            out = out.replace(&format!("%{var}%"), &v).replace(&format!("%{}%", var.to_lowercase()), &v);
        }
    }
    if let Some(rest) = out.strip_prefix("~") {
        if let Some(home) = crate::apps::home_dir() {
            out = format!("{}{rest}", home.display());
        }
    }
    out
}

/// `{slot}`, `{prev}` and `{prev_first}` filled in.
fn fill(template: &str, args: &Value, prev: &str) -> String {
    let first = prev.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("");
    let filled = template.replace("{prev_first}", first).replace("{prev}", prev);
    SLOT.replace_all(&filled, |c: &regex::Captures| match &args[&c[1]] {
        Value::String(s) => s.clone(),
        Value::Null => c[0].to_string(),
        other => other.to_string(),
    })
    .into_owned()
}

fn fill_value(v: &Value, args: &Value, prev: &str) -> Value {
    match v {
        Value::String(s) => Value::String(fill(s, args, prev)),
        Value::Array(a) => Value::Array(a.iter().map(|x| fill_value(x, args, prev)).collect()),
        Value::Object(o) => Value::Object(o.iter().map(|(k, x)| (k.clone(), fill_value(x, args, prev))).collect()),
        other => other.clone(),
    }
}

/// Runs every step in order; the first failing step stops the run.
pub async fn execute<H: StepHost>(spec: &ForgedToolSpec, args: &Value, host: &H, plugin_dir: &Path) -> Result<ForgeRun, String> {
    spec.validate()?;
    let started = Instant::now();
    let mut prev = String::new();
    let mut done = Vec::new();
    for (i, step) in spec.steps.iter().enumerate() {
        let out = run_step(spec, step, args, &prev, host, plugin_dir).await.map_err(|e| format!("Step {} ({}) failed: {e}", i + 1, step.summary()))?;
        done.push(step.summary());
        prev = truncate(&out, MAX_OUTPUT);
    }
    Ok(ForgeRun { name: spec.name.clone(), version: spec.version, output: prev, steps: done, elapsed_ms: started.elapsed().as_millis() })
}

async fn run_step<H: StepHost>(spec: &ForgedToolSpec, step: &RustStep, args: &Value, prev: &str, host: &H, plugin_dir: &Path) -> Result<String, String> {
    let f = |s: &str| fill(s, args, prev);
    match step {
        RustStep::ScanDirectory { root, extensions, filter_regex } => {
            let root = expand_env(&f(root));
            let exts: Vec<String> = extensions.iter().map(|e| e.trim_start_matches('.').to_lowercase()).collect();
            let filter = filter_regex.as_deref().map(|r| Regex::new(&format!("(?i){}", f(r)))).transpose().map_err(|e| e.to_string())?;
            tokio::task::spawn_blocking(move || scan(&root, &exts, filter.as_ref())).await.map_err(|e| e.to_string())?
        }
        RustStep::ParseFileOrSqlite { path, query_or_json_path } => {
            let path = expand_env(&f(path));
            let query = query_or_json_path.as_deref().map(f);
            tokio::task::spawn_blocking(move || parse(&path, query.as_deref())).await.map_err(|e| e.to_string())?
        }
        RustStep::LaunchOrInvokeProcess { binary, args: argv, working_dir } => {
            let binary = expand_env(&f(binary));
            let argv: Vec<String> = argv.iter().map(|a| expand_env(&f(a))).collect();
            launch(&binary, &argv, working_dir.as_deref().map(|d| expand_env(&f(d)))).await
        }
        RustStep::Win32WindowAction { window_title_match, action } => {
            let action = f(action).to_lowercase().replace([' ', '-'], "_");
            match action.as_str() {
                "play_pause" | "media_play_pause" | "next_track" | "media_next" | "prev_track" | "media_previous" | "mute" => {
                    let media = match action.as_str() {
                        "next_track" | "media_next" => "next_track",
                        "prev_track" | "media_previous" => "prev_track",
                        "mute" => "mute",
                        _ => "play_pause",
                    };
                    host.jarvis_tool("media_and_volume_control", &json!({ "action": media })).await
                }
                _ => host.jarvis_tool("window_management", &json!({ "action": action, "target_window": f(window_title_match) })).await,
            }
        }
        RustStep::HttpRequest { method, url, headers, body_template } => {
            let client = reqwest::Client::builder().timeout(Duration::from_secs(20)).build().map_err(|e| e.to_string())?;
            let method = reqwest::Method::from_bytes(method.to_uppercase().as_bytes()).map_err(|e| e.to_string())?;
            let mut req = client.request(method, f(url));
            for (k, v) in headers {
                req = req.header(k.as_str(), f(v));
            }
            if let Some(body) = body_template {
                req = req.body(f(body));
            }
            let resp = req.send().await.map_err(|e| e.to_string())?;
            let status = resp.status();
            let text = resp.text().await.map_err(|e| e.to_string())?;
            if status.is_success() {
                Ok(truncate(&text, MAX_OUTPUT))
            } else {
                Err(format!("HTTP {status}: {}", truncate(&text, 300)))
            }
        }
        RustStep::CompileAndRunRustPlugin { rust_source_code, args: argv } => {
            let exe = compile_plugin(&spec.name, rust_source_code, plugin_dir).await?;
            let argv: Vec<String> = argv.iter().map(|a| f(a)).collect();
            launch(&exe.to_string_lossy(), &argv, None).await
        }
        RustStep::JarvisTool { name, args: targs } => {
            if FORBIDDEN_TOOLS.contains(&name.as_str()) {
                return Err(format!("'{name}' may not be called from a forged tool."));
            }
            host.jarvis_tool(name, &fill_value(targs, args, prev)).await
        }
    }
}

fn scan(root: &str, exts: &[String], filter: Option<&Regex>) -> Result<String, String> {
    let root = Path::new(root);
    if !root.exists() {
        return Err(format!("{} does not exist.", root.display()));
    }
    let mut hits = Vec::new();
    for entry in walkdir::WalkDir::new(root).follow_links(false).max_depth(8).into_iter().filter_map(Result::ok).take(200_000) {
        if !entry.file_type().is_file() {
            continue;
        }
        let p = entry.path();
        let ext = p.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
        if !exts.is_empty() && !exts.contains(&ext) {
            continue;
        }
        let shown = p.display().to_string();
        if filter.is_some_and(|r| !r.is_match(&shown)) {
            continue;
        }
        hits.push(shown);
        if hits.len() >= 2000 {
            break;
        }
    }
    if hits.is_empty() {
        Err(format!("Nothing matched under {}.", root.display()))
    } else {
        Ok(hits.join("\n"))
    }
}

fn parse(path: &str, query: Option<&str>) -> Result<String, String> {
    let p = Path::new(path);
    let ext = p.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
    if matches!(ext.as_str(), "sqlite" | "sqlite3" | "db") {
        let q = query.unwrap_or("SELECT name FROM sqlite_master WHERE type='table'").trim().trim_end_matches(';');
        let head = q.to_lowercase();
        if !(head.starts_with("select") || head.starts_with("with")) || q.contains(';') {
            return Err("Only a single read-only SELECT is allowed.".into());
        }
        let uri = format!("file:{}?mode=ro", path.replace('\\', "/"));
        let conn = Connection::open_with_flags(&uri, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_URI).map_err(|e| e.to_string())?;
        let _ = conn.busy_timeout(Duration::from_millis(500));
        let mut stmt = conn.prepare(q).map_err(|e| e.to_string())?;
        let cols = stmt.column_count();
        let rows = stmt
            .query_map([], |r| {
                Ok((0..cols)
                    .map(|i| match r.get_ref(i) {
                        Ok(rusqlite::types::ValueRef::Text(t)) => String::from_utf8_lossy(t).into_owned(),
                        Ok(rusqlite::types::ValueRef::Integer(n)) => n.to_string(),
                        Ok(rusqlite::types::ValueRef::Real(x)) => x.to_string(),
                        _ => String::new(),
                    })
                    .collect::<Vec<_>>()
                    .join("\t"))
            })
            .map_err(|e| e.to_string())?;
        let lines: Vec<String> = rows.flatten().take(500).collect();
        return Ok(lines.join("\n"));
    }
    let meta = std::fs::metadata(p).map_err(|e| format!("{path}: {e}"))?;
    if meta.len() > 16 * 1024 * 1024 {
        return Err(format!("{path} is larger than 16 MiB."));
    }
    let raw = String::from_utf8_lossy(&std::fs::read(p).map_err(|e| e.to_string())?).into_owned();
    match ext.as_str() {
        "json" => {
            let v: Value = serde_json::from_str(&raw).map_err(|e| e.to_string())?;
            let picked = match query {
                Some(q) if !q.is_empty() => {
                    let pointer = if q.starts_with('/') { q.to_string() } else { format!("/{}", q.replace('.', "/")) };
                    v.pointer(&pointer).cloned().ok_or_else(|| format!("No '{q}' in {path}."))?
                }
                _ => v,
            };
            Ok(match picked {
                Value::String(s) => s,
                Value::Array(a) => a.iter().map(|x| x.as_str().map(String::from).unwrap_or_else(|| x.to_string())).collect::<Vec<_>>().join("\n"),
                other => other.to_string(),
            })
        }
        "m3u" | "m3u8" | "pls" => Ok(raw
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && !l.starts_with('#') && !l.starts_with('['))
            .map(|l| l.split_once('=').filter(|_| l.starts_with("File")).map(|(_, v)| v).unwrap_or(l).to_string())
            .collect::<Vec<_>>()
            .join("\n")),
        _ => Ok(match query {
            // A plain-text query: the matching lines.
            Some(q) if !q.is_empty() => {
                let rx = Regex::new(&format!("(?i){q}")).map_err(|e| e.to_string())?;
                raw.lines().filter(|l| rx.is_match(l)).take(500).collect::<Vec<_>>().join("\n")
            }
            _ => truncate(&raw, MAX_OUTPUT),
        }),
    }
}

async fn launch(binary: &str, argv: &[String], dir: Option<String>) -> Result<String, String> {
    let stem = Path::new(binary).file_stem().map(|s| s.to_string_lossy().to_lowercase()).unwrap_or_default();
    if FORBIDDEN_BINARIES.contains(&stem.as_str()) {
        return Err(format!("'{binary}' is not allowed in a forged tool."));
    }
    let mut cmd = tokio::process::Command::new(binary);
    cmd.args(argv).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped()).kill_on_drop(true);
    if let Some(d) = dir {
        cmd.current_dir(d);
    }
    #[cfg(windows)]
    cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    let child = cmd.spawn().map_err(|e| format!("could not start {binary}: {e}"))?;
    let out = tokio::time::timeout(STEP_TIMEOUT, child.wait_with_output()).await.map_err(|_| format!("{binary} did not finish within {} s", STEP_TIMEOUT.as_secs()))?.map_err(|e| e.to_string())?;
    let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if out.status.success() {
        Ok(if stdout.is_empty() { format!("{binary} finished.") } else { truncate(&stdout, MAX_OUTPUT) })
    } else {
        let stderr = String::from_utf8_lossy(&out.stderr);
        Err(format!("{binary} exited with {}: {}", out.status, truncate(stderr.trim(), 400)))
    }
}

fn rustc() -> PathBuf {
    let cargo = crate::apps::home_dir().map(|h| h.join(".cargo").join("bin").join(if cfg!(windows) { "rustc.exe" } else { "rustc" }));
    cargo.filter(|p| p.is_file()).unwrap_or_else(|| PathBuf::from("rustc"))
}

/// Writes `src_plugins/<tool>.rs` and compiles it to `bin/<tool>.exe` (only when it changed).
async fn compile_plugin(tool: &str, source: &str, plugin_dir: &Path) -> Result<PathBuf, String> {
    use std::hash::{Hash, Hasher};
    let src_dir = plugin_dir.join("src_plugins");
    let bin_dir = plugin_dir.join("bin");
    std::fs::create_dir_all(&src_dir).map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&bin_dir).map_err(|e| e.to_string())?;
    let src = src_dir.join(format!("{tool}.rs"));
    let exe = bin_dir.join(format!("{tool}{}", std::env::consts::EXE_SUFFIX));
    let stamp = bin_dir.join(format!("{tool}.hash"));
    let mut h = std::collections::hash_map::DefaultHasher::new();
    source.hash(&mut h);
    let hash = format!("{:016x}", h.finish());
    if exe.is_file() && std::fs::read_to_string(&stamp).is_ok_and(|s| s.trim() == hash) {
        return Ok(exe);
    }
    std::fs::write(&src, source).map_err(|e| e.to_string())?;
    let mut cmd = tokio::process::Command::new(rustc());
    cmd.args(["-O", "--edition", "2021", "-o"]).arg(&exe).arg(&src).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::piped()).kill_on_drop(true);
    #[cfg(windows)]
    cmd.creation_flags(0x0800_0000);
    let out = tokio::time::timeout(Duration::from_secs(120), cmd.output()).await.map_err(|_| "rustc took longer than 120 s".to_string())?.map_err(|e| format!("rustc is not available: {e}"))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(format!("the Rust plugin did not compile: {}", truncate(err.trim(), 600)));
    }
    let _ = std::fs::write(&stamp, hash);
    info!("[FORGE] Compiled plugin {}", exe.display());
    Ok(exe)
}

// ----------------------------------------------------------------------- persistence ----

pub struct ToolForge {
    conn: Mutex<Connection>,
    dir: PathBuf,
}

impl ToolForge {
    pub fn open(db_path: &Path, dir: &Path) -> Result<Self, String> {
        let conn = Connection::open(db_path).map_err(|e| e.to_string())?;
        let _ = conn.busy_timeout(Duration::from_secs(2));
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS forged_tools (
                name TEXT PRIMARY KEY,
                spec TEXT NOT NULL,
                version INTEGER NOT NULL,
                runs INTEGER NOT NULL DEFAULT 0,
                last_ok INTEGER NOT NULL DEFAULT 1,
                last_output TEXT NOT NULL DEFAULT '',
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                updated_at TEXT NOT NULL DEFAULT (datetime('now'))
            );",
        )
        .map_err(|e| e.to_string())?;
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        Ok(Self { conn: Mutex::new(conn), dir: dir.to_path_buf() })
    }

    fn conn(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn list(&self) -> Vec<ForgedToolSpec> {
        let conn = self.conn();
        let Ok(mut stmt) = conn.prepare("SELECT spec FROM forged_tools ORDER BY name") else { return Vec::new() };
        stmt.query_map([], |r| r.get::<_, String>(0)).map(|rows| rows.flatten().filter_map(|s| serde_json::from_str(&s).ok()).collect()).unwrap_or_default()
    }

    pub fn get(&self, name: &str) -> Option<ForgedToolSpec> {
        let spec: String = self.conn().query_row("SELECT spec FROM forged_tools WHERE name=?1", [name], |r| r.get(0)).ok()?;
        serde_json::from_str(&spec).ok()
    }

    /// Saves (or replaces) a tool: SQLite plus a hand-editable JSON copy.
    pub fn save(&self, spec: &ForgedToolSpec) -> Result<(), String> {
        spec.validate()?;
        let raw = serde_json::to_string_pretty(spec).map_err(|e| e.to_string())?;
        self.conn()
            .execute(
                "INSERT INTO forged_tools(name, spec, version) VALUES(?1, ?2, ?3)
                 ON CONFLICT(name) DO UPDATE SET spec=excluded.spec, version=excluded.version, updated_at=datetime('now')",
                params![spec.name, raw, spec.version],
            )
            .map_err(|e| e.to_string())?;
        std::fs::write(self.dir.join(format!("{}.json", spec.name)), raw).map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn delete(&self, name: &str) -> bool {
        let n = self.conn().execute("DELETE FROM forged_tools WHERE name=?1", [name]).unwrap_or(0);
        let _ = std::fs::remove_file(self.dir.join(format!("{name}.json")));
        n > 0
    }

    fn record_run(&self, name: &str, ok: bool, output: &str) {
        let _ = self.conn().execute(
            "UPDATE forged_tools SET runs=runs+1, last_ok=?2, last_output=?3 WHERE name=?1",
            params![name, ok as i64, truncate(output, 1000)],
        );
    }

    /// The forged tool Sir's words trigger, with its arguments.
    pub fn match_utterance(&self, text: &str) -> Option<(ForgedToolSpec, Value)> {
        self.list().into_iter().find_map(|spec| match_triggers(&spec, text).map(|args| (spec, args)))
    }

    /// Verifies a new or corrected tool by running it with `args`; only a successful run is
    /// saved. `correction`: what Sir said was wrong (kept with the tool).
    pub async fn forge<H: StepHost>(&self, mut spec: ForgedToolSpec, args: &Value, correction: Option<&str>, host: &H) -> Result<ForgeRun, String> {
        spec.name = spec.name.trim().to_lowercase().replace([' ', '-'], "_");
        spec.validate()?;
        let previous = self.get(&spec.name);
        spec.version = previous.as_ref().map_or(1, |p| p.version + 1);
        spec.corrections = previous.map(|p| p.corrections).unwrap_or_default();
        if let Some(c) = correction.filter(|c| !c.trim().is_empty()) {
            spec.corrections.push(c.trim().to_string());
        }
        crate::dynamic_popups::spawn(
            crate::dynamic_popups::PopupPayload::ToolForgeLiveCard { tool_name: spec.name.clone(), steps_summary: spec.steps.iter().map(RustStep::summary).collect(), status: "Testing…".into() },
            None,
        );
        let run = execute(&spec, args, host, &self.dir).await;
        let status = match &run {
            Ok(r) => format!("Verified v{} in {} ms", spec.version, r.elapsed_ms),
            Err(e) => format!("Test failed: {}", truncate(e, 120)),
        };
        crate::dynamic_popups::spawn(
            crate::dynamic_popups::PopupPayload::ToolForgeLiveCard { tool_name: spec.name.clone(), steps_summary: spec.steps.iter().map(RustStep::summary).collect(), status },
            None,
        );
        let run = run?;
        self.save(&spec)?;
        self.record_run(&spec.name, true, &run.output);
        info!("[FORGE] '{}' v{} verified and saved ({} step(s), triggers: {:?})", spec.name, spec.version, spec.steps.len(), spec.triggers);
        Ok(ForgeRun { version: spec.version, ..run })
    }

    /// Runs a saved tool.
    pub async fn run<H: StepHost>(&self, name: &str, args: &Value, host: &H) -> Result<ForgeRun, String> {
        let spec = self.get(name).ok_or_else(|| format!("I have no forged tool called '{name}'."))?;
        let result = execute(&spec, args, host, &self.dir).await;
        match &result {
            Ok(r) => self.record_run(name, true, &r.output),
            Err(e) => {
                warn!("[FORGE] '{name}' failed: {e}");
                self.record_run(name, false, e);
            }
        }
        result
    }

    /// Every saved tool as a function the model can call (`forged_<name>`).
    pub fn tool_definitions(&self) -> Vec<Value> {
        self.list()
            .into_iter()
            .map(|spec| {
                let props: serde_json::Map<String, Value> = spec.slots().into_iter().map(|s| (s, json!({"type": "string"}))).collect();
                json!({"type": "function", "function": {
                    "name": format!("forged_{}", spec.name),
                    "description": format!("[Forged tool v{}] {} Triggers: {}.", spec.version, spec.description, spec.triggers.join(" | ")),
                    "parameters": {"type": "object", "properties": props}
                }})
            })
            .collect()
    }
}

static FORGE: OnceLock<Option<ToolForge>> = OnceLock::new();

/// Opens the forge next to the memory database (called at start-up; later calls are no-ops).
pub fn init(db_path: &Path, skills_dir: &Path) {
    FORGE.get_or_init(|| match ToolForge::open(db_path, &skills_dir.join("forged_tools")) {
        Ok(f) => Some(f),
        Err(e) => {
            warn!("[FORGE] Unavailable: {e}");
            None
        }
    });
}

pub fn forge() -> Option<&'static ToolForge> {
    FORGE.get().and_then(Option::as_ref)
}

// ------------------------------------------------------------------- Sir's requests ----

/// "Create a tool that …", "build yourself a tool for …", "mach dir ein eigenes Tool dafür".
pub fn is_forge_request(text: &str) -> bool {
    let t = text.to_lowercase();
    [
        "create a tool", "create yourself a tool", "build a tool", "build yourself a tool", "make a tool", "make yourself a tool", "forge a tool", "write yourself a tool",
        "new tool for", "create a new tool", "erstelle ein tool", "erstell ein tool", "erstelle dir ein tool", "mach dir ein tool", "mach dir ein eigenes tool",
        "bau dir ein tool", "baue dir ein tool", "eigenes tool dafür", "eigenes tool dafur", "neues tool",
    ]
    .iter()
    .any(|p| t.contains(p))
}

/// Sir saying the last action was done wrong ("That didn't work", "don't use search, do it
/// this way instead", "das war falsch, mach es so …"): the correction itself.
pub fn correction(text: &str) -> Option<String> {
    static RX: std::sync::LazyLock<Regex> = std::sync::LazyLock::new(|| {
        Regex::new(r"(?i)\b(?:that didn'?t work|that did not work|it didn'?t work|you did (?:that|it|this) wrong|you got (?:that|it) wrong|(?:don'?t|do not) (?:do it|do that) like that|(?:don'?t|do not) use (?:the )?search|do it (?:this|that|like this) way(?: instead)?|instead,? (?:do|use|go)|das (?:hat nicht funktioniert|war falsch|klappt nicht)|du hast (?:das|es) falsch gemacht|mach (?:das|es) (?:nicht so|anders|so)|nicht so,? sondern|ohne (?:die )?suche)\b").expect("correction expression")
    });
    RX.is_match(text).then(|| text.trim().to_string())
}

/// The forge's JSON instructions for the model.
pub fn forge_prompt() -> String {
    "You build NATIVE tools for J.A.R.V.I.S. (Windows). Reply with ONLY a JSON object:\n\
     {\"name\": \"snake_case\", \"description\": \"one sentence\", \"triggers\": [\"english phrase with {slot}\", \"german phrase with {slot}\"],\n\
      \"args\": {\"slot\": \"value from Sir's request\"},\n\
      \"steps\": [ ... ]}\n\
     Step objects (\"step\" picks the kind; strings may use {slot}, {prev} = previous step output, {prev_first} = its first line; paths may use %APPDATA%, %LOCALAPPDATA%, ~):\n\
     {\"step\":\"scan_directory\",\"root\":\"path\",\"extensions\":[\"m3u\"],\"filter_regex\":\"optional\"}\n\
     {\"step\":\"parse_file_or_sqlite\",\"path\":\"file or {prev_first}\",\"query_or_json_path\":\"SELECT … | a.b.0 | regex\"}\n\
     {\"step\":\"launch_or_invoke_process\",\"binary\":\"C:/path/app.exe\",\"args\":[\"…\"]}  (never cmd/powershell/python)\n\
     {\"step\":\"win32_window_action\",\"window_title_match\":\"Discord\",\"action\":\"focus|minimize|maximize|close|play_pause|next_track\"}\n\
     {\"step\":\"http_request\",\"method\":\"GET\",\"url\":\"https://…\",\"headers\":[[\"k\",\"v\"]],\"body_template\":null}\n\
     {\"step\":\"compile_and_run_rust_plugin\",\"rust_source_code\":\"fn main(){ … }\",\"args\":[\"{slot}\"]}  (std only; print the result)\n\
     {\"step\":\"jarvis_tool\",\"name\":\"control_music|window_management|open_application|open_website|media_and_volume_control|…\",\"args\":{…}}\n\
     Prefer jarvis_tool when one of Jarvis' tools already does a step. Keep it to the fewest steps that do exactly what Sir asked."
        .to_string()
}

/// The model's JSON → a spec and the arguments for its first run.
pub fn spec_from_model(raw: &str) -> Result<(ForgedToolSpec, Value), String> {
    let start = raw.find('{').ok_or("The forge reply has no JSON.")?;
    let end = raw.rfind('}').ok_or("The forge reply has no JSON.")?;
    let v: Value = serde_json::from_str(&raw[start..=end]).map_err(|e| format!("The forge reply is not valid JSON: {e}"))?;
    let args = v.get("args").cloned().filter(Value::is_object).unwrap_or_else(|| json!({}));
    let spec: ForgedToolSpec = serde_json::from_value(v).map_err(|e| format!("The forge reply is not a tool spec: {e}"))?;
    Ok((spec, args))
}

pub fn is_tool(name: &str) -> bool {
    matches!(name, "forge_tool" | "run_forged_tool" | "list_forged_tools" | "delete_forged_tool") || name.starts_with("forged_")
}

pub fn definitions() -> Vec<Value> {
    let mut defs = vec![
        json!({"type": "function", "function": {
            "name": "forge_tool",
            "description": "Create (or correct) a NEW persistent native tool for yourself when Sir asks for something no tool covers, or tells you how to do it instead. Pass the full spec: name, description, triggers (with {slot}s), args for the first run, steps (scan_directory, parse_file_or_sqlite, launch_or_invoke_process, win32_window_action, http_request, compile_and_run_rust_plugin, jarvis_tool). It is test-run immediately and saved only if it works.",
            "parameters": {"type": "object", "properties": {
                "name": {"type": "string"}, "description": {"type": "string"},
                "triggers": {"type": "array", "items": {"type": "string"}},
                "args": {"type": "object"}, "steps": {"type": "array", "items": {"type": "object"}},
                "correction": {"type": "string", "description": "What Sir said was wrong (for a corrected tool)"}
            }, "required": ["name", "steps"]}
        }}),
        json!({"type": "function", "function": {
            "name": "run_forged_tool",
            "description": "Run one of your forged tools by name with its slot arguments.",
            "parameters": {"type": "object", "properties": {"name": {"type": "string"}, "args": {"type": "object"}}, "required": ["name"]}
        }}),
        json!({"type": "function", "function": {
            "name": "list_forged_tools",
            "description": "List the tools you forged for yourself (name, version, triggers).",
            "parameters": {"type": "object", "properties": {}}
        }}),
        json!({"type": "function", "function": {
            "name": "delete_forged_tool",
            "description": "Delete a forged tool Sir no longer wants.",
            "parameters": {"type": "object", "properties": {"name": {"type": "string"}}, "required": ["name"]}
        }}),
    ];
    if let Some(f) = forge() {
        defs.extend(f.tool_definitions());
    }
    defs
}

fn describe(run: &ForgeRun, verb: &str) -> String {
    format!("{verb} '{}' v{} ({} step(s), {} ms). Result: {}", run.name, run.version, run.steps.len(), run.elapsed_ms, truncate(&run.output, 600))
}

pub async fn run_tool<H: StepHost>(host: &H, name: &str, args: &Value) -> Result<String, String> {
    let forge = forge().ok_or("The tool forge is unavailable (its database could not be opened).")?;
    match name {
        "forge_tool" => {
            let first = args.get("args").cloned().filter(Value::is_object).unwrap_or_else(|| json!({}));
            let correction = args["correction"].as_str().map(String::from);
            let spec: ForgedToolSpec = serde_json::from_value(args.clone()).map_err(|e| format!("Not a tool spec: {e}"))?;
            forge.forge(spec, &first, correction.as_deref(), host).await.map(|r| describe(&r, "Forged, verified and saved"))
        }
        "list_forged_tools" => {
            let tools = forge.list();
            if tools.is_empty() {
                return Ok("No forged tools yet.".into());
            }
            Ok(tools.iter().map(|t| format!("- {} v{}: {} (triggers: {})", t.name, t.version, t.description, t.triggers.join(" | "))).collect::<Vec<_>>().join("\n"))
        }
        "delete_forged_tool" => {
            let n = args["name"].as_str().ok_or("delete_forged_tool needs a name.")?;
            if forge.delete(n) {
                Ok(format!("Deleted forged tool '{n}'."))
            } else {
                Err(format!("No forged tool called '{n}'."))
            }
        }
        _ => {
            let (tool, targs) = match name.strip_prefix("forged_") {
                Some(t) => (t.to_string(), args.clone()),
                None => (args["name"].as_str().ok_or("run_forged_tool needs a name.")?.to_string(), args.get("args").cloned().unwrap_or_else(|| json!({}))),
            };
            forge.run(&tool, &targs, host).await.map(|r| describe(&r, "Ran forged tool"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex as StdMutex;

    /// Records Jarvis-tool calls instead of executing them.
    #[derive(Default)]
    struct Host {
        calls: StdMutex<Vec<(String, Value)>>,
    }

    impl StepHost for Host {
        async fn jarvis_tool(&self, name: &str, args: &Value) -> Result<String, String> {
            self.calls.lock().unwrap_or_else(|e| e.into_inner()).push((name.to_string(), args.clone()));
            if name == "fail_me" {
                return Err("simulated failure".into());
            }
            Ok(format!("{name} ok"))
        }
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("jarvis_forge_test_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn playlist_spec() -> ForgedToolSpec {
        ForgedToolSpec {
            name: "playlist_direct_player".into(),
            description: "Plays a song straight from a named playlist without search.".into(),
            triggers: vec!["play {song} from my playlist {playlist}".into(), "spiel {song} aus meiner playlist {playlist}".into()],
            steps: vec![RustStep::JarvisTool { name: "control_music".into(), args: json!({"action": "play_from_playlist", "song_name": "{song}", "playlist_name": "{playlist}"}) }],
            version: 0,
            corrections: vec![],
        }
    }

    #[test]
    fn triggers_capture_slots_through_courtesy() {
        let spec = playlist_spec();
        let args = match_triggers(&spec, "Jarvis, could you play Highway to Hell from my playlist JaysRock please?").unwrap();
        assert_eq!(args, json!({"song": "highway to hell", "playlist": "jaysrock"}));
        let de = match_triggers(&spec, "Spiel Thunderstruck aus meiner Playlist Jays Rock").unwrap();
        assert_eq!(de["playlist"], "jays rock");
        assert!(match_triggers(&spec, "play some music").is_none());
        assert_eq!(spec.slots(), vec!["song".to_string(), "playlist".to_string()]);
    }

    #[test]
    fn validation_keeps_tools_native() {
        let mut spec = playlist_spec();
        assert!(spec.validate().is_ok());
        spec.steps = vec![RustStep::LaunchOrInvokeProcess { binary: "powershell.exe".into(), args: vec![], working_dir: None }];
        assert!(spec.validate().unwrap_err().contains("native"));
        spec.steps = vec![RustStep::JarvisTool { name: "shutdown_jarvis".into(), args: json!({}) }];
        assert!(spec.validate().is_err());
        spec.name = "Bad Name".into();
        assert!(spec.validate().is_err());
    }

    #[test]
    fn model_json_becomes_a_spec() {
        let raw = r#"Here: {"name":"open_dev_setup","description":"x","triggers":["open my dev setup"],"args":{},"steps":[{"step":"win32_window_action","window_title_match":"Visual Studio Code","action":"maximize"},{"step":"http_request","url":"https://example.com"}]}"#;
        let (spec, args) = spec_from_model(raw).unwrap();
        assert_eq!(spec.steps.len(), 2);
        assert!(matches!(&spec.steps[1], RustStep::HttpRequest { method, .. } if method == "GET"));
        assert_eq!(args, json!({}));
    }

    #[tokio::test]
    async fn forge_verifies_persists_and_executes() {
        let dir = temp_dir("persist");
        let db = dir.join("forge.db");
        let forge = ToolForge::open(&db, &dir.join("forged_tools")).unwrap();
        let host = Host::default();
        let run = forge.forge(playlist_spec(), &json!({"song": "Highway to Hell", "playlist": "JaysRock"}), None, &host).await.unwrap();
        assert_eq!(run.version, 1);
        assert_eq!(run.output, "control_music ok");
        let (name, args) = host.calls.lock().unwrap_or_else(|e| e.into_inner())[0].clone();
        assert_eq!(name, "control_music");
        assert_eq!(args["song_name"], "Highway to Hell");
        assert_eq!(args["action"], "play_from_playlist");
        // Persisted in SQLite and as JSON, survives a reopen, and is matched by its trigger.
        assert!(dir.join("forged_tools").join("playlist_direct_player.json").is_file());
        let reopened = ToolForge::open(&db, &dir.join("forged_tools")).unwrap();
        let (spec, args) = reopened.match_utterance("play Back in Black from my playlist Jays Rock").unwrap();
        assert_eq!(spec.name, "playlist_direct_player");
        let again = reopened.run(&spec.name, &args, &host).await.unwrap();
        assert_eq!(again.output, "control_music ok");
        assert_eq!(reopened.tool_definitions()[0]["function"]["name"], "forged_playlist_direct_player");
    }

    #[tokio::test]
    async fn correction_replaces_the_tool_permanently_and_failures_are_not_saved() {
        let dir = temp_dir("correct");
        let forge = ToolForge::open(&dir.join("forge.db"), &dir.join("forged_tools")).unwrap();
        let host = Host::default();
        forge.forge(playlist_spec(), &json!({"song": "a", "playlist": "b"}), None, &host).await.unwrap();
        let mut fixed = playlist_spec();
        fixed.steps.push(RustStep::Win32WindowAction { window_title_match: "Sonora".into(), action: "minimize".into() });
        let run = forge.forge(fixed, &json!({"song": "a", "playlist": "b"}), Some("Don't use search, do it this way instead"), &host).await.unwrap();
        assert_eq!(run.version, 2);
        let saved = forge.get("playlist_direct_player").unwrap();
        assert_eq!(saved.steps.len(), 2);
        assert_eq!(saved.corrections, vec!["Don't use search, do it this way instead".to_string()]);
        // A spec whose test run fails is reported and never saved.
        let broken = ForgedToolSpec { name: "broken_tool".into(), description: String::new(), triggers: vec!["break it".into()], steps: vec![RustStep::JarvisTool { name: "fail_me".into(), args: json!({}) }], version: 0, corrections: vec![] };
        let err = forge.forge(broken, &json!({}), None, &host).await.unwrap_err();
        assert!(err.contains("simulated failure"), "{err}");
        assert!(forge.get("broken_tool").is_none());
    }

    #[tokio::test]
    async fn native_steps_scan_and_parse_files_and_sqlite() {
        let dir = temp_dir("steps");
        std::fs::write(dir.join("JaysRock.m3u"), "#EXTM3U\n#EXTINF:1,AC/DC - Highway to Hell\nC:/music/hth.mp3\nC:/music/tnt.mp3\n").unwrap();
        std::fs::write(dir.join("notes.json"), r#"{"a":{"b":["x","y"]}}"#).unwrap();
        let db = dir.join("lib.sqlite");
        {
            let c = Connection::open(&db).unwrap();
            c.execute_batch("CREATE TABLE t(name TEXT); INSERT INTO t VALUES('Jays Rock');").unwrap();
        }
        let spec = ForgedToolSpec {
            name: "scan_then_parse".into(),
            description: String::new(),
            triggers: vec![],
            steps: vec![
                RustStep::ScanDirectory { root: dir.display().to_string(), extensions: vec!["m3u".into()], filter_regex: Some("{playlist}".into()) },
                RustStep::ParseFileOrSqlite { path: "{prev_first}".into(), query_or_json_path: None },
            ],
            version: 0,
            corrections: vec![],
        };
        let run = execute(&spec, &json!({"playlist": "jaysrock"}), &Host::default(), &dir).await.unwrap();
        assert_eq!(run.output, "C:/music/hth.mp3\nC:/music/tnt.mp3");
        assert_eq!(parse(&dir.join("notes.json").display().to_string(), Some("a.b")).unwrap(), "x\ny");
        assert_eq!(parse(&db.display().to_string(), Some("SELECT name FROM t")).unwrap(), "Jays Rock");
        assert!(parse(&db.display().to_string(), Some("DELETE FROM t")).is_err());
    }

    #[test]
    fn forge_and_correction_requests() {
        assert!(is_forge_request("Create a tool that renames my screenshots by date"));
        assert!(is_forge_request("Mach dir ein eigenes Tool dafür"));
        assert!(!is_forge_request("open the tool shed website"));
        assert!(correction("You did that wrong. Don't use search, do it this way instead: open the playlist").is_some());
        assert!(correction("Das war falsch, mach es so").is_some());
        assert!(correction("That didn't work").is_some());
        assert!(correction("play the next song").is_none());
    }
}
