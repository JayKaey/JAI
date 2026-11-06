//! Supervised outbound HTTPS tunnel to the token-protected satellite listener: Jarvis on Sir's
//! phone from anywhere (4G/5G, any Wi-Fi). The origin is loopback HTTP; TLS terminates at the
//! tunnel provider. No router changes.
//!
//! The address. A Cloudflare quick tunnel needs nothing set up, but its trycloudflare.com name
//! is new on every start, so the phone's home-screen icon goes stale. A **permanent address**
//! never changes: a Cloudflare named tunnel (its token sealed in the vault, its own hostname in
//! the settings), Tailscale Funnel (`<pc>.<tailnet>.ts.net`) or Sir's own server over SSH.
//! `plan` decides which one runs, from the settings alone.
//!
//! The tunnel process starts hidden and without the busy cursor, inside the helper job (it dies
//! with Jarvis — no orphaned `cloudflared`), and `kick` (re)starts it the moment Sir asks how to
//! use Jarvis on his phone.

use crate::config::{ConfigStore, SatelliteSettings};
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock, RwLock};
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::sync::{mpsc, oneshot, Notify};
use tokio_util::sync::CancellationToken;

/// Where the token of a Cloudflare named tunnel is kept: sealed in the vault, never in the
/// settings file.
pub const TOKEN_SERVICE: &str = "cloudflare_tunnel";

#[derive(Clone, Debug, Default, Serialize)]
pub struct TunnelStatus {
    pub state: String,
    pub public_url: Option<String>,
    pub detail: String,
    pub provider: String,
    /// The address is the same after every restart (a named tunnel, Funnel, an own server).
    pub permanent: bool,
}

fn state() -> &'static RwLock<TunnelStatus> {
    static STATE: OnceLock<RwLock<TunnelStatus>> = OnceLock::new();
    STATE.get_or_init(|| RwLock::new(TunnelStatus { state: "starting".into(), detail: "Waiting for the satellite listener".into(), ..Default::default() }))
}

pub fn snapshot() -> TunnelStatus {
    state().read().unwrap_or_else(|e| e.into_inner()).clone()
}

fn kick_signal() -> &'static Notify {
    static KICK: OnceLock<Notify> = OnceLock::new();
    KICK.get_or_init(Notify::new)
}

/// Starts the tunnel now: wakes a disabled supervisor (once worldwide access is enabled) or one
/// waiting out a retry delay. Harmless while the tunnel is online.
pub fn kick() {
    kick_signal().notify_one();
}

/// Resolves on the next `kick`.
pub async fn kicked() {
    kick_signal().notified().await;
}

fn update(provider: &str, phase: &str, detail: impl Into<String>, public_url: Option<String>) {
    *state().write().unwrap_or_else(|e| e.into_inner()) = TunnelStatus { state: phase.into(), public_url, detail: detail.into(), provider: provider.into(), permanent: false };
}

/// The verified public address is up.
fn online(provider: &str, url: &str, permanent: bool) {
    let detail = if permanent { "Permanent HTTPS address verified; it stays the same after every restart" } else { "Worldwide HTTPS endpoint verified; scan the QR code" };
    *state().write().unwrap_or_else(|e| e.into_inner()) = TunnelStatus { state: "online".into(), public_url: Some(url.to_string()), detail: detail.into(), provider: provider.into(), permanent };
}

pub fn pairing_url(config: &ConfigStore) -> Option<String> {
    let base = snapshot().public_url?;
    let token = config.read(|c| c.satellite.token.clone());
    Some(format!("{}/satellite?token={}", base.trim_end_matches('/'), urlencoding::encode(&token)))
}

/// Only accept a clean HTTPS origin. Never include credentials, paths or log fragments.
pub fn https_origin(raw: &str) -> Option<String> {
    let url = reqwest::Url::parse(raw.trim()).ok()?;
    (url.scheme() == "https" && url.host_str().is_some() && url.username().is_empty()
        && url.password().is_none() && url.query().is_none() && url.fragment().is_none()
        && matches!(url.path(), "" | "/"))
        .then(|| url.origin().ascii_serialization())
}

fn public_url_from_log(line: &str, provider: &str) -> Option<String> {
    line.split_whitespace().filter_map(|part| {
        let part = part.trim_matches(|c: char| matches!(c, '|' | '(' | ')' | '"' | '\'' | ','));
        let origin = https_origin(part)?;
        let url = reqwest::Url::parse(&origin).ok()?;
        let host = url.host_str()?;
        let valid = if provider == "tailscale" {
            host.ends_with(".ts.net")
        } else {
            host.ends_with(".trycloudflare.com") && host != "trycloudflare.com"
        };
        valid.then_some(origin)
    }).next()
}

/// The bare tunnel token out of what Sir pasted. Cloudflare shows it inside a whole command
/// (`cloudflared.exe service install eyJ…`), so the last word counts; None when that is no token.
pub fn clean_token(pasted: &str) -> Option<String> {
    let word = pasted.split_whitespace().last()?.trim_matches(|c| matches!(c, '"' | '\''));
    (word.len() >= 40 && word.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'/' | b'=' | b'-' | b'_'))).then(|| word.to_string())
}

/// The address that will not change, as far as the settings tell: a Cloudflare named tunnel's
/// (with its token stored) or an own server's. Tailscale's is permanent too, but only known
/// once Funnel reports it.
pub fn configured_address(settings: &SatelliteSettings, has_token: bool) -> Option<String> {
    match settings.tunnel_provider.to_ascii_lowercase().as_str() {
        "auto" | "cloudflare" | "cloudflared" if has_token => https_origin(&settings.tunnel_public_url),
        "ssh" => https_origin(&settings.tunnel_public_url),
        _ => None,
    }
}

/// What the supervisor starts, decided from the settings alone.
#[derive(Debug, Clone, PartialEq)]
pub struct Plan {
    /// "cloudflare", "tailscale" or "ssh".
    pub provider: &'static str,
    /// The executable: "cloudflared", "tailscale" or "ssh".
    pub program: &'static str,
    pub args: Vec<String>,
    /// Added to the process environment. A tunnel token travels here and never among the
    /// arguments, where every program on the PC could read it.
    pub env: Vec<(String, String)>,
    /// The public address when it is known beforehand; a quick tunnel's is read from its output.
    pub address: Option<String>,
    /// The address is the same after every restart.
    pub permanent: bool,
}

/// The tunnel for these settings, forwarding to the loopback origin on `port`. Cloudflare with a
/// tunnel token and a permanent address: the named tunnel behind that address. Without a token:
/// a quick tunnel, whose address is new every time.
pub fn plan(settings: &SatelliteSettings, token: Option<&str>, port: u16) -> Result<Plan, String> {
    let origin = format!("http://127.0.0.1:{port}");
    match settings.tunnel_provider.to_ascii_lowercase().as_str() {
        "auto" | "cloudflare" | "cloudflared" => match token.and_then(clean_token) {
            Some(token) => {
                let address = https_origin(&settings.tunnel_public_url)
                    .ok_or("A tunnel token is stored, but the permanent address is missing: enter it as https://name.your-domain under Integrations, Quick Connections, Mobile satellite")?;
                // The tunnel's own settings at Cloudflare name the local service, port included.
                if settings.tunnel_origin_port == 0 {
                    return Err("The permanent address needs a fixed local port: set tunnel_origin_port (8444 by default) and name it in the tunnel's service".into());
                }
                if port != settings.tunnel_origin_port {
                    return Err(format!(
                        "The permanent address forwards to http://127.0.0.1:{0}, but local port {0} is taken by another program; free it (or change tunnel_origin_port and the tunnel's service) and restart me",
                        settings.tunnel_origin_port
                    ));
                }
                Ok(Plan { provider: "cloudflare", program: "cloudflared", args: vec!["tunnel".into(), "--no-autoupdate".into(), "run".into()], env: vec![("TUNNEL_TOKEN".into(), token)], address: Some(address), permanent: true })
            }
            None if token.is_some_and(|t| !t.trim().is_empty()) => Err("The stored tunnel token is not a Cloudflare tunnel token; paste it again from the tunnel's page".into()),
            None => Ok(Plan { provider: "cloudflare", program: "cloudflared", args: vec!["tunnel".into(), "--no-autoupdate".into(), "--url".into(), origin], env: Vec::new(), address: None, permanent: false }),
        },
        "tailscale" => Ok(Plan { provider: "tailscale", program: "tailscale", args: vec!["funnel".into(), origin], env: Vec::new(), address: None, permanent: true }),
        "ssh" => {
            let address = https_origin(&settings.tunnel_public_url);
            if settings.tunnel_ssh_target.trim().is_empty() || settings.tunnel_ssh_target.starts_with('-') || settings.tunnel_ssh_target.chars().any(char::is_whitespace) || address.is_none() {
                return Err("SSH tunnelling requires tunnel_ssh_target and an HTTPS tunnel_public_url served by the VPS reverse proxy".into());
            }
            let mut args: Vec<String> = ["-N", "-T", "-o", "BatchMode=yes", "-o", "ExitOnForwardFailure=yes", "-o", "ConnectTimeout=15", "-o", "ServerAliveInterval=15", "-o", "ServerAliveCountMax=3"]
                .iter()
                .map(|a| a.to_string())
                .collect();
            if !settings.tunnel_ssh_key.trim().is_empty() {
                args.extend(["-i".into(), settings.tunnel_ssh_key.clone()]);
            }
            args.extend(["-R".into(), format!("127.0.0.1:{}:127.0.0.1:{port}", settings.tunnel_ssh_remote_port.max(1)), settings.tunnel_ssh_target.clone()]);
            Ok(Plan { provider: "ssh", program: "ssh", args, env: Vec::new(), address, permanent: true })
        }
        _ => Err("Unknown tunnel provider; use auto, cloudflare, tailscale or ssh".into()),
    }
}

fn installed(name: &str) -> Option<PathBuf> {
    let filename = format!("{name}{}", std::env::consts::EXE_SUFFIX);
    let local = Path::new("bin").join(&filename);
    if local.is_file() { return Some(local); }
    std::env::var_os("PATH").and_then(|paths| std::env::split_paths(&paths)
        .map(|p| p.join(&filename)).find(|p| p.is_file()))
}

async fn cloudflared() -> Result<PathBuf, String> {
    if let Some(path) = installed("cloudflared") { return Ok(path); }
    let asset_name = match (std::env::consts::OS, std::env::consts::ARCH) {
        ("windows", "x86_64") => "cloudflared-windows-amd64.exe",
        ("windows", "x86") => "cloudflared-windows-386.exe",
        ("linux", "x86_64") => "cloudflared-linux-amd64",
        ("linux", "aarch64") => "cloudflared-linux-arm64",
        _ => return Err("Install the official cloudflared executable in bin/ on this platform.".into()),
    };
    update("cloudflare", "downloading", "Downloading the official cloudflared release and verifying its SHA-256 digest", None);
    let client = reqwest::Client::builder().user_agent("JarvisWorldwide/1.0")
        .connect_timeout(Duration::from_secs(15)).timeout(Duration::from_secs(180))
        .build().map_err(|e| e.to_string())?;
    let release: serde_json::Value = client.get("https://api.github.com/repos/cloudflare/cloudflared/releases/latest")
        .send().await.map_err(|e| e.to_string())?.error_for_status().map_err(|e| e.to_string())?
        .json().await.map_err(|e| e.to_string())?;
    let asset = release["assets"].as_array().and_then(|a| a.iter().find(|a| a["name"] == asset_name))
        .ok_or("Official release does not include a binary for this platform")?;
    let digest = asset["digest"].as_str().and_then(|s| s.strip_prefix("sha256:"))
        .filter(|s| s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit()))
        .ok_or("The official release did not provide a SHA-256 digest; install cloudflared manually")?;
    let url = asset["browser_download_url"].as_str().ok_or("Missing release download URL")?;
    if !url.starts_with("https://github.com/cloudflare/cloudflared/releases/download/") {
        return Err("Unexpected cloudflared release URL".into());
    }
    tokio::fs::create_dir_all("bin").await.map_err(|e| e.to_string())?;
    let path = Path::new("bin").join(format!("cloudflared{}", std::env::consts::EXE_SUFFIX));
    let partial = path.with_extension("download");
    let download = async {
        let mut response = client.get(url).send().await.map_err(|e| e.to_string())?
            .error_for_status().map_err(|e| e.to_string())?;
        let mut file = tokio::fs::File::create(&partial).await.map_err(|e| e.to_string())?;
        let mut hash = ring::digest::Context::new(&ring::digest::SHA256);
        let mut total = 0usize;
        while let Some(chunk) = response.chunk().await.map_err(|e| e.to_string())? {
            total += chunk.len();
            if total > 150 * 1024 * 1024 { return Err("cloudflared download exceeds 150 MiB".into()); }
            hash.update(&chunk);
            file.write_all(&chunk).await.map_err(|e| e.to_string())?;
        }
        file.sync_all().await.map_err(|e| e.to_string())?;
        drop(file);
        let actual: String = hash.finish().as_ref().iter().map(|b| format!("{b:02x}")).collect();
        if !actual.eq_ignore_ascii_case(digest) { return Err("cloudflared SHA-256 verification failed".into()); }
        #[cfg(unix)] {
            use std::os::unix::fs::PermissionsExt;
            tokio::fs::set_permissions(&partial, std::fs::Permissions::from_mode(0o700)).await.map_err(|e| e.to_string())?;
        }
        tokio::fs::rename(&partial, &path).await.map_err(|e| e.to_string())?;
        Ok(path)
    }.await;
    if download.is_err() { let _ = tokio::fs::remove_file(&partial).await; }
    download
}

/// The running tunnel process: its output, line by line, and how it ended. Dropping it (or
/// `kill`) stops the process.
struct TunnelProcess {
    lines: mpsc::UnboundedReceiver<String>,
    exited: oneshot::Receiver<String>,
    stop: Arc<AtomicBool>,
}

impl TunnelProcess {
    fn kill(&self) {
        self.stop.store(true, Ordering::Release);
    }
}

impl Drop for TunnelProcess {
    fn drop(&mut self) {
        self.kill();
    }
}

/// Windows: the feedback-free helper spawner; one I/O thread forwards whole lines from stdout
/// and stderr, reports the exit and kills the process when asked to stop.
#[cfg(windows)]
fn launch(program: &Path, args: &[String], env: &[(String, String)]) -> Result<TunnelProcess, String> {
    let args: Vec<std::ffi::OsString> = args.iter().map(Into::into).collect();
    let mut child = crate::shell::spawn_helper(program, &args, env, true).map_err(|e| e.to_string())?;
    tracing::info!("[TUNNEL] Started {} (pid {})", program.display(), child.id());
    let (line_tx, lines) = mpsc::unbounded_channel();
    let (exit_tx, exited) = oneshot::channel();
    let stop = Arc::new(AtomicBool::new(false));
    let stopping = Arc::clone(&stop);
    std::thread::Builder::new()
        .name("jarvis-tunnel-io".into())
        .spawn(move || {
            let (mut out, mut err) = (Vec::new(), Vec::new());
            let forward = |buf: &mut Vec<u8>, last: bool| {
                while let Some(end) = buf.iter().position(|b| *b == b'\n') {
                    let line: Vec<u8> = buf.drain(..=end).collect();
                    let _ = line_tx.send(String::from_utf8_lossy(&line).trim_end().to_string());
                }
                if last && !buf.is_empty() {
                    let _ = line_tx.send(String::from_utf8_lossy(buf).trim_end().to_string());
                    buf.clear();
                }
            };
            loop {
                let read = child.read_available(&mut out, &mut err);
                forward(&mut out, false);
                forward(&mut err, false);
                match child.try_wait() {
                    Ok(Some(code)) => {
                        child.read_available(&mut out, &mut err);
                        forward(&mut out, true);
                        forward(&mut err, true);
                        let _ = exit_tx.send(format!("exit code {code}"));
                        return;
                    }
                    Ok(None) => {}
                    Err(e) => {
                        let _ = exit_tx.send(format!("unknown ({e})"));
                        return;
                    }
                }
                if stopping.load(Ordering::Acquire) {
                    child.kill();
                }
                if read == 0 {
                    std::thread::sleep(Duration::from_millis(40));
                }
            }
        })
        .map_err(|e| e.to_string())?;
    Ok(TunnelProcess { lines, exited, stop })
}

#[cfg(not(windows))]
fn launch(program: &Path, args: &[String], env: &[(String, String)]) -> Result<TunnelProcess, String> {
    use std::process::Stdio;
    use tokio::io::{AsyncBufReadExt, BufReader};
    let mut child = tokio::process::Command::new(program)
        .args(args)
        .envs(env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| e.to_string())?;
    let (line_tx, lines) = mpsc::unbounded_channel();
    let (exit_tx, exited) = oneshot::channel();
    let stop = Arc::new(AtomicBool::new(false));
    for pipe in [child.stdout.take().map(|p| Box::new(p) as Box<dyn tokio::io::AsyncRead + Unpin + Send>), child.stderr.take().map(|p| Box::new(p) as Box<dyn tokio::io::AsyncRead + Unpin + Send>)].into_iter().flatten() {
        let tx = line_tx.clone();
        tokio::spawn(async move {
            let mut reader = BufReader::new(pipe).lines();
            while let Ok(Some(line)) = reader.next_line().await {
                let _ = tx.send(line);
            }
        });
    }
    let stopping = Arc::clone(&stop);
    tokio::spawn(async move {
        loop {
            tokio::select! {
                status = child.wait() => {
                    let _ = exit_tx.send(format!("{status:?}"));
                    return;
                }
                _ = tokio::time::sleep(Duration::from_millis(200)) => {
                    if stopping.load(Ordering::Acquire) {
                        let _ = child.kill().await;
                    }
                }
            }
        }
    });
    Ok(TunnelProcess { lines, exited, stop })
}

async fn spawn(plan: &Plan) -> Result<TunnelProcess, String> {
    let program = match plan.program {
        "cloudflared" => cloudflared().await?,
        "tailscale" => installed("tailscale").ok_or("Install and sign in to Tailscale before enabling Funnel")?,
        _ => installed("ssh").ok_or("OpenSSH is not installed")?,
    };
    launch(&program, &plan.args, &plan.env).map_err(|e| format!("Could not start {}: {e}", plan.provider))
}

async fn verify_origin(origin: &str, token: &str) -> bool {
    let Ok(client) = reqwest::Client::builder().redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(3)).timeout(Duration::from_secs(5)).build() else { return false };
    let Ok(response) = client.get(format!("{origin}/api/tunnel-health")).header("X-Jarvis-Key", token).send().await else { return false };
    if !response.status().is_success() { return false; }
    response.json::<serde_json::Value>().await.ok().is_some_and(|v| v["service"] == "jarvis-satellite")
}

/// What the tunnel program said went wrong, short enough for the status line (its own log
/// level and timestamp dropped).
fn complaint(line: &str) -> Option<String> {
    let lower = line.to_ascii_lowercase();
    if !(lower.contains(" err ") || lower.contains("error") || lower.contains("not valid") || lower.contains("unauthorized")) {
        return None;
    }
    let said = line.split(" ERR ").last().unwrap_or(line).trim();
    Some(said.chars().take(160).collect())
}

/// Runs until application shutdown, cleaning up the child and retrying transient failures.
/// While worldwide access is switched off it waits for `kick` (Sir asking for phone access).
/// `secrets` holds the token of a named tunnel.
pub async fn run(config: ConfigStore, secrets: crate::vault::Vault, port: u16, shutdown: CancellationToken) {
    let mut attempt = 0u32;
    loop {
        let settings = config.read(|c| c.satellite.clone());
        if !settings.enabled || !settings.worldwide_enabled {
            update("", "disabled", "Worldwide access is disabled in satellite settings", None);
            tokio::select! {
                _ = shutdown.cancelled() => break,
                _ = kicked() => continue,
            }
        }
        let token = secrets.secret(TOKEN_SERVICE, crate::vault::TOKEN);
        update(&settings.tunnel_provider, "starting", "Opening an outbound HTTPS tunnel", None);
        let started = match plan(&settings, token.as_deref(), port) {
            Ok(plan) => tokio::select! {
                _ = shutdown.cancelled() => break,
                result = spawn(&plan) => result.map(|child| (child, plan)),
            },
            Err(why) => Err(why),
        };
        match started {
            Ok((mut child, plan)) => {
                let provider = plan.provider;
                update(provider, "connecting", if plan.address.is_some() { "Connecting the permanent address" } else { "Waiting for the public HTTPS address" }, None);
                let mut lines_open = true;
                let deadline = tokio::time::sleep(Duration::from_secs(90));
                tokio::pin!(deadline);
                let mut ready = false;
                let mut candidate = plan.address.clone();
                let mut said: Option<String> = None;
                let mut health = tokio::time::interval(Duration::from_secs(5));
                health.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                let mut failures = 0u8;
                loop {
                    tokio::select! {
                        _ = shutdown.cancelled() => {
                            child.kill();
                            update(provider, "stopped", "Jarvis has shut down", None);
                            return;
                        }
                        status = &mut child.exited => {
                            let why = said.take().map(|s| format!(": {s}")).unwrap_or_default();
                            update(provider, "reconnecting", format!("Tunnel exited ({}){why}", status.unwrap_or_else(|_| "unknown".into())), None);
                            break;
                        }
                        line = child.lines.recv(), if lines_open => {
                            match line {
                                Some(line) => {
                                    // A named tunnel's address is the one Sir set; only a quick
                                    // tunnel (or Funnel) announces its own.
                                    if plan.address.is_none() {
                                        if let Some(url) = public_url_from_log(&line, provider) {
                                            candidate = Some(url);
                                        }
                                    }
                                    if let Some(c) = complaint(&line) {
                                        said = Some(c);
                                    }
                                }
                                None => lines_open = false,
                            }
                        }
                        _ = &mut deadline, if !ready => {
                            child.kill();
                            let why = match &plan.address {
                                Some(address) if provider == "cloudflare" => format!("{address} does not reach me yet: in the Cloudflare tunnel its public hostname must point to the service http://127.0.0.1:{port}; retrying"),
                                _ => "No working HTTPS address after 90 seconds; retrying".to_string(),
                            };
                            update(provider, "reconnecting", why, None);
                            break;
                        }
                        _ = health.tick() => {
                            let current = config.read(|c| c.satellite.clone());
                            if !current.enabled || !current.worldwide_enabled {
                                child.kill();
                                update(provider, "disabled", "Worldwide access was disabled", None);
                                break;
                            }
                            if current.tunnel_provider != settings.tunnel_provider || current.tunnel_public_url != settings.tunnel_public_url
                                || current.tunnel_origin_port != settings.tunnel_origin_port
                                || current.tunnel_ssh_target != settings.tunnel_ssh_target || current.tunnel_ssh_key != settings.tunnel_ssh_key
                                || current.tunnel_ssh_remote_port != settings.tunnel_ssh_remote_port
                                || secrets.secret(TOKEN_SERVICE, crate::vault::TOKEN) != token {
                                child.kill();
                                update(provider, "reconnecting", "Tunnel settings changed", None);
                                break;
                            }
                            if let Some(url) = &candidate {
                                let verified = tokio::select! {
                                    _ = shutdown.cancelled() => { child.kill(); return; }
                                    verified = verify_origin(url, &current.token) => verified,
                                };
                                if verified {
                                    if !ready {
                                        tracing::info!("[TUNNEL] Worldwide HTTPS endpoint online and verified ({}): {url}", if plan.permanent { "permanent address" } else { "changes when the tunnel restarts" });
                                    }
                                    ready = true; failures = 0; attempt = 0;
                                    online(provider, url, plan.permanent);
                                } else if ready {
                                    failures = failures.saturating_add(1);
                                    if failures >= 3 {
                                        child.kill();
                                        update(provider, "reconnecting", "Public HTTPS endpoint stopped responding; reconnecting", None);
                                        break;
                                    }
                                }
                            }
                        }
                    }
                }
            }
            Err(error) => {
                tracing::warn!("[TUNNEL] {error}");
                update(&settings.tunnel_provider, "error", error, None);
            }
        }
        attempt = attempt.saturating_add(1);
        let delay = Duration::from_secs((5 * 2u64.saturating_pow(attempt.min(5))).min(120));
        // Sir asking for phone access cuts the wait short.
        tokio::select! { _ = shutdown.cancelled() => break, _ = tokio::time::sleep(delay) => {}, _ = kicked() => {} }
    }
    update("", "stopped", "Jarvis has shut down", None);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shape of a Cloudflare tunnel token (base64 of a small JSON object).
    const TOKEN: &str = "eyJhIjoiMDEyMzQ1Njc4OWFiY2RlZjAxMjM0NTY3ODlhYmNkZWYiLCJ0IjoiMTIzNDU2NzgtOWFiYy1kZWYwLTEyMzQtNTY3ODlhYmNkZWYwIiwicyI6IlpYaGhiWEJzWlE9PSJ9";

    #[test]
    fn accepts_only_provider_https_origins() {
        assert_eq!(public_url_from_log("| https://quiet-forest.trycloudflare.com |", "cloudflare"), Some("https://quiet-forest.trycloudflare.com".into()));
        for bad in ["http://x.trycloudflare.com", "https://x.trycloudflare.com.evil.test", "https://x.trycloudflare.com/path", "https://user@x.trycloudflare.com", "https://x.trycloudflare.com?token=secret"] {
            assert!(public_url_from_log(bad, "cloudflare").is_none(), "{bad}");
        }
        assert_eq!(public_url_from_log("https://pc.tail1.ts.net", "tailscale"), Some("https://pc.tail1.ts.net".into()));
    }

    #[test]
    fn a_tunnel_token_and_an_address_give_one_address_for_good() {
        let settings = |address: &str| SatelliteSettings { tunnel_public_url: address.into(), ..Default::default() };
        // Nothing set up: a quick tunnel to whatever port the origin got; its address is read
        // from its output and is new every time.
        let quick = plan(&settings(""), None, 51_234).unwrap();
        assert_eq!((quick.provider, quick.program, quick.permanent, &quick.address), ("cloudflare", "cloudflared", false, &None));
        assert_eq!(quick.args, ["tunnel", "--no-autoupdate", "--url", "http://127.0.0.1:51234"]);
        assert!(quick.env.is_empty());
        // A token and the address it belongs to: the named tunnel, the address known at once.
        let named = plan(&settings("https://jarvis.example.com"), Some(TOKEN), 8444).unwrap();
        assert_eq!((named.provider, named.permanent, named.address.as_deref()), ("cloudflare", true, Some("https://jarvis.example.com")));
        assert_eq!(named.args, ["tunnel", "--no-autoupdate", "run"]);
        assert_eq!(named.env, [("TUNNEL_TOKEN".to_string(), TOKEN.to_string())]);
        assert!(named.args.iter().all(|a| !a.contains("eyJ")), "the token is never on the command line");
        // Pasted as Cloudflare shows it, inside its whole command: the token alone is used.
        let pasted = format!("cloudflared.exe service install {TOKEN}");
        assert_eq!(plan(&settings("https://jarvis.example.com/"), Some(&pasted), 8444).unwrap().env[0].1, TOKEN);
        assert_eq!(clean_token(&pasted).as_deref(), Some(TOKEN));
        assert_eq!(clean_token("hello"), None);
        assert_eq!(clean_token(""), None);
        // A token without its address, with an address that is no clean HTTPS origin, or with
        // the fixed local port taken: said plainly, nothing is started.
        for bad in ["", "http://jarvis.example.com", "https://jarvis.example.com/satellite", "jarvis.example.com"] {
            let why = plan(&settings(bad), Some(TOKEN), 8444).unwrap_err();
            assert!(why.contains("permanent address"), "{bad}: {why}");
        }
        let taken = plan(&settings("https://jarvis.example.com"), Some(TOKEN), 51_234).unwrap_err();
        assert!(taken.contains("8444") && taken.contains("taken"), "{taken}");
        assert!(plan(&settings("https://jarvis.example.com"), Some("not a token"), 8444).unwrap_err().contains("not a Cloudflare tunnel token"));
        // An address without a token is still a quick tunnel (nothing would answer at that name).
        assert!(!plan(&settings("https://jarvis.example.com"), None, 8444).unwrap().permanent);
        assert_eq!(configured_address(&settings("https://jarvis.example.com"), false), None);
        assert_eq!(configured_address(&settings("https://jarvis.example.com"), true).as_deref(), Some("https://jarvis.example.com"));
    }

    #[test]
    fn funnel_and_an_own_server_are_permanent_too() {
        let funnel = plan(&SatelliteSettings { tunnel_provider: "tailscale".into(), ..Default::default() }, None, 8444).unwrap();
        assert_eq!((funnel.provider, funnel.program, funnel.permanent), ("tailscale", "tailscale", true));
        assert_eq!(funnel.args, ["funnel", "http://127.0.0.1:8444"]);
        let own = SatelliteSettings { tunnel_provider: "ssh".into(), tunnel_ssh_target: "sir@vps.example.com".into(), tunnel_public_url: "https://jarvis.example.com".into(), ..Default::default() };
        let ssh = plan(&own, None, 8444).unwrap();
        assert_eq!((ssh.provider, ssh.permanent, ssh.address.as_deref()), ("ssh", true, Some("https://jarvis.example.com")));
        assert!(ssh.args.ends_with(&["-R".to_string(), "127.0.0.1:18443:127.0.0.1:8444".to_string(), "sir@vps.example.com".to_string()]));
        assert_eq!(configured_address(&own, false).as_deref(), Some("https://jarvis.example.com"));
        assert!(plan(&SatelliteSettings { tunnel_provider: "ssh".into(), ..Default::default() }, None, 8444).is_err());
        assert!(plan(&SatelliteSettings { tunnel_provider: "carrier pigeon".into(), ..Default::default() }, None, 8444).is_err());
        // What the tunnel program complains about reaches the status line, without its prefix.
        assert_eq!(complaint("2026-11-06T07:00:00Z ERR Provided Tunnel token is not valid.").as_deref(), Some("Provided Tunnel token is not valid."));
        assert_eq!(complaint("2026-11-06T07:00:00Z INF Registered tunnel connection connIndex=0"), None);
    }
}
