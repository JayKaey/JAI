//! Instant diagnostics Sir can ask for by voice:
//! * network: online?, local and public IP, latency to Cloudflare/Google, DNS time, traffic;
//! * processes: what is using the CPU and memory right now ("what's slowing my PC down?").

use crate::telemetry::SystemTelemetry;
use std::collections::HashMap;
use std::net::{SocketAddr, UdpSocket};
use std::time::{Duration, Instant};
use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System};

/// The address the OS would use to reach the internet (no packet is sent).
fn local_ip() -> Option<String> {
    let socket = UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("1.1.1.1:80").ok()?;
    socket.local_addr().ok().map(|a| a.ip().to_string())
}

/// Best of three TCP handshakes, in ms.
async fn latency(addr: &str) -> Option<u128> {
    let addr: SocketAddr = addr.parse().ok()?;
    let mut best: Option<u128> = None;
    for _ in 0..3 {
        let started = Instant::now();
        if let Ok(Ok(_)) = tokio::time::timeout(Duration::from_secs(2), tokio::net::TcpStream::connect(addr)).await {
            let ms = started.elapsed().as_millis();
            best = Some(best.map_or(ms, |b| b.min(ms)));
        }
    }
    best
}

async fn public_ip() -> Option<String> {
    let client = reqwest::Client::builder().timeout(Duration::from_secs(3)).build().ok()?;
    let ip = client.get("https://api.ipify.org").send().await.ok()?.text().await.ok()?;
    let ip = ip.trim().to_string();
    (!ip.is_empty() && ip.len() <= 45).then_some(ip)
}

/// The `network_status` tool.
pub async fn network_status(telemetry: &SystemTelemetry) -> String {
    let dns_started = Instant::now();
    let dns = tokio::time::timeout(Duration::from_secs(3), tokio::net::lookup_host("example.com:443")).await;
    let dns_ms = matches!(dns, Ok(Ok(_))).then(|| dns_started.elapsed().as_millis());
    let (cloudflare, google, public) = tokio::join!(latency("1.1.1.1:443"), latency("8.8.8.8:443"), public_ip());
    let online = cloudflare.is_some() || google.is_some();
    let mut out = vec![if online { "Online.".to_string() } else { "OFFLINE: no internet connection (Cloudflare and Google unreachable).".to_string() }];
    match local_ip() {
        Some(ip) => out.push(format!("Local IP {ip}.")),
        None => out.push("No network interface with a route to the internet.".into()),
    }
    if let Some(ip) = public {
        out.push(format!("Public IP {ip}."));
    }
    let lat: Vec<String> = [("Cloudflare", cloudflare), ("Google", google)]
        .iter()
        .filter_map(|(name, ms)| ms.map(|ms| format!("{name} {ms} ms")))
        .collect();
    if !lat.is_empty() {
        out.push(format!("Latency: {}.", lat.join(", ")));
    }
    match dns_ms {
        Some(ms) => out.push(format!("DNS lookup {ms} ms.")),
        None if online => out.push("DNS lookups are failing.".into()),
        None => {}
    }
    out.push(format!("Traffic now: {:.0} KB/s down, {:.0} KB/s up.", telemetry.net_down_kbps, telemetry.net_up_kbps));
    out.join(" ")
}

/// One spoken sentence from `network_status` output.
pub fn spoken_network(report: &str) -> String {
    if report.starts_with("OFFLINE") {
        return "I'm afraid we're offline, Sir. Neither Cloudflare nor Google is reachable.".into();
    }
    let latency = report.split("Cloudflare ").nth(1).and_then(|r| r.split(" ms").next()).map(|ms| format!(", with a latency of {ms} milliseconds"));
    format!("We're online, Sir{}. The details are in the chat.", latency.unwrap_or_default())
}

#[derive(Debug, Clone)]
pub struct ProcessUsage {
    pub name: String,
    pub cpu_pct: f32,
    pub memory_mb: f64,
    pub count: usize,
}

/// Processes grouped by name (Chrome's dozens of processes count as one), sampled twice ~0.5 s
/// apart so CPU percentages are real. CPU is a share of the whole machine.
pub fn top_processes() -> Vec<ProcessUsage> {
    let mut sys = System::new();
    let kind = ProcessRefreshKind::new().with_cpu().with_memory();
    sys.refresh_processes_specifics(ProcessesToUpdate::All, kind);
    std::thread::sleep(Duration::from_millis(500));
    sys.refresh_processes_specifics(ProcessesToUpdate::All, kind);
    let cores = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1) as f32;
    let mut groups: HashMap<String, ProcessUsage> = HashMap::new();
    for p in sys.processes().values() {
        let name = p.name().to_string_lossy().trim_end_matches(".exe").to_string();
        if name.is_empty() || matches!(name.to_lowercase().as_str(), "system idle process" | "idle" | "system" | "registry" | "memory compression") {
            continue;
        }
        let g = groups.entry(name.to_lowercase()).or_insert(ProcessUsage { name, cpu_pct: 0.0, memory_mb: 0.0, count: 0 });
        g.cpu_pct += p.cpu_usage() / cores;
        g.memory_mb += p.memory() as f64 / 1_048_576.0;
        g.count += 1;
    }
    groups.into_values().collect()
}

/// The `top_processes` tool.
pub fn processes_report(list: &[ProcessUsage], limit: usize) -> String {
    let mut by_cpu = list.to_vec();
    by_cpu.sort_by(|a, b| b.cpu_pct.total_cmp(&a.cpu_pct));
    let mut by_mem = list.to_vec();
    by_mem.sort_by(|a, b| b.memory_mb.total_cmp(&a.memory_mb));
    let fmt = |p: &ProcessUsage| {
        format!(
            "{} ({:.1}% CPU, {:.0} MB{})",
            p.name,
            p.cpu_pct,
            p.memory_mb,
            if p.count > 1 { format!(", {} processes", p.count) } else { String::new() }
        )
    };
    format!(
        "Top CPU: {}.\nTop memory: {}.",
        by_cpu.iter().take(limit).map(fmt).collect::<Vec<_>>().join("; "),
        by_mem.iter().take(limit).map(fmt).collect::<Vec<_>>().join("; ")
    )
}

/// "Chrome is using the most CPU at 23 percent, and the most memory at 3.1 gigabytes."
pub fn spoken_processes(list: &[ProcessUsage]) -> String {
    let list: Vec<ProcessUsage> = list.iter().map(|p| ProcessUsage { name: crate::apps::display_name(&p.name), ..p.clone() }).collect();
    let top_cpu = list.iter().max_by(|a, b| a.cpu_pct.total_cmp(&b.cpu_pct));
    let top_mem = list.iter().max_by(|a, b| a.memory_mb.total_cmp(&b.memory_mb));
    match (top_cpu, top_mem) {
        (Some(c), Some(m)) if c.cpu_pct < 5.0 => format!(
            "Nothing is straining the processor, Sir. {} holds the most memory, at {:.1} gigabytes.",
            m.name,
            m.memory_mb / 1024.0
        ),
        (Some(c), Some(m)) if c.name == m.name => format!(
            "{} is the culprit, Sir: {:.0} percent of the processor and {:.1} gigabytes of memory.",
            c.name,
            c.cpu_pct,
            m.memory_mb / 1024.0
        ),
        (Some(c), Some(m)) => format!(
            "{} is using the most processor time at {:.0} percent, and {} the most memory at {:.1} gigabytes, Sir.",
            c.name,
            c.cpu_pct,
            m.name,
            m.memory_mb / 1024.0
        ),
        _ => "I couldn't read the process list, Sir.".into(),
    }
}

/// The spoken summary of a `processes_report` ("Top CPU: chrome (23.0% CPU, 3200 MB, …)").
pub fn spoken_from_report(report: &str) -> String {
    let parse = |line: &str| -> Option<ProcessUsage> {
        let first = line.split_once(": ")?.1.split("; ").next()?;
        let (name, rest) = first.split_once(" (")?;
        let cpu: f32 = rest.split('%').next()?.trim().parse().ok()?;
        let mb: f64 = rest.split(", ").nth(1)?.trim_end_matches(|c: char| !c.is_ascii_digit()).trim().parse().ok()?;
        Some(ProcessUsage { name: name.to_string(), cpu_pct: cpu, memory_mb: mb, count: 1 })
    };
    let mut lines = report.lines();
    match (lines.next().and_then(parse), lines.next().and_then(parse)) {
        (Some(cpu), Some(mem)) => spoken_processes(&[cpu, mem]),
        _ => "The process list is in the chat, Sir.".into(),
    }
}

pub fn tool_definitions() -> Vec<serde_json::Value> {
    vec![
        serde_json::json!({"type": "function", "function": {
            "name": "network_status",
            "description": "Internet diagnostics: online or not, local and public IP, latency, DNS and current traffic.",
            "parameters": {"type": "object", "properties": {}, "required": []}
        }}),
        serde_json::json!({"type": "function", "function": {
            "name": "top_processes",
            "description": "Which programs use the most CPU and memory right now (for 'why is my PC slow').",
            "parameters": {"type": "object", "properties": {"limit": {"type": "integer"}}, "required": []}
        }}),
    ]
}

// ------------------------------------------------------------------ offline, and back ----
// One cheap handshake every 30 s; two misses in a row mean the internet is gone, told once, and
// its return is told once. Meanwhile the online read tools answer from their last answer.

static OFFLINE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
const PROBE_EVERY: Duration = Duration::from_secs(30);
const MISSES_FOR_OFFLINE: u32 = 2;
/// The online read tools whose last answer is kept for a time without internet.
const KEPT_ANSWERS: &[&str] = &["web_search", "get_precise_location_weather", "check_ride_and_commute_status"];
const KEPT: &str = "last_answer";

pub fn is_offline() -> bool {
    OFFLINE.load(std::sync::atomic::Ordering::Acquire)
}

/// One probe's step: (offline, misses so far) after it, and Some(new state) when it turned.
pub fn link_step(offline: bool, misses: u32, reached: bool) -> (bool, u32, Option<bool>) {
    match (reached, offline) {
        (true, true) => (false, 0, Some(false)),
        (true, false) => (false, 0, None),
        (false, false) if misses + 1 >= MISSES_FOR_OFFLINE => (true, misses + 1, Some(true)),
        (false, _) => (offline, misses + 1, None),
    }
}

async fn reachable() -> bool {
    for addr in ["1.1.1.1:443", "9.9.9.9:443"] {
        let Ok(addr) = addr.parse::<SocketAddr>() else { continue };
        if matches!(tokio::time::timeout(Duration::from_secs(3), tokio::net::TcpStream::connect(addr)).await, Ok(Ok(_))) {
            return true;
        }
    }
    false
}

pub fn start_link_watch(stop: tokio_util::sync::CancellationToken) {
    if cfg!(test) || crate::platform::in_test_session() {
        return;
    }
    tokio::spawn(async move {
        let (mut offline, mut misses) = (false, 0);
        loop {
            tokio::select! { _ = stop.cancelled() => return, _ = tokio::time::sleep(PROBE_EVERY) => {} }
            let step = link_step(offline, misses, reachable().await);
            (offline, misses) = (step.0, step.1);
            OFFLINE.store(offline, std::sync::atomic::Ordering::Release);
            match step.2 {
                Some(true) => {
                    tracing::warn!("[NET] Offline: two handshakes in a row failed");
                    crate::announcements::say("The internet has dropped, Sir. Until it is back I answer from what I last looked up.");
                }
                Some(false) => {
                    tracing::info!("[NET] Back online");
                    crate::announcements::say("We're back online, Sir.");
                }
                None => {}
            }
        }
    });
}

/// Whether a tool's error reads like an unreachable server.
fn unreachable(err: &str) -> bool {
    let e = err.to_lowercase();
    ["error sending request", "dns", "connect", "timed out", "timeout", "network", "unreachable", "no internet", "resolve"].iter().any(|w| e.contains(w))
}

/// The tool call's result, kept when it is an online read that worked; when it failed without
/// internet, the last kept answer with its time instead.
pub fn kept_answer(tool: &str, args: &serde_json::Value, result: Result<String, String>) -> Result<String, String> {
    if !KEPT_ANSWERS.contains(&tool) {
        return result;
    }
    let key = format!("{tool} {args}");
    match result {
        Ok(output) => {
            let _ = crate::kit::put(KEPT, &key, &serde_json::json!({"output": output, "at": chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string()}));
            Ok(output)
        }
        Err(err) if is_offline() || unreachable(&err) => match crate::kit::get(KEPT, &key) {
            Some(kept) => Ok(offline_copy(kept["output"].as_str().unwrap_or(""), kept["at"].as_str().unwrap_or(""), chrono::Local::now().naive_local())),
            None => Err(err),
        },
        Err(err) => Err(err),
    }
}

/// The kept answer, marked as such with its time.
pub fn offline_copy(output: &str, at: &str, now: chrono::NaiveDateTime) -> String {
    format!("{output}\n\n(No internet right now: this is what I found {}.)", crate::kit_notes::when_said(at, now))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_internet_is_gone_after_two_misses_and_told_once_each_way() {
        let (mut offline, mut misses, mut told) = (false, 0, Vec::new());
        for reached in [true, false, true, false, false, false, false, true, true, false] {
            let step = link_step(offline, misses, reached);
            (offline, misses) = (step.0, step.1);
            told.extend(step.2);
        }
        assert_eq!(told, [true, false], "one miss is a blip; the drop and the return are told once each");
        assert!(!offline && misses == 1);
        assert!(unreachable("error sending request for url (https://api.example.com/)") && unreachable("DNS lookup failed") && !unreachable("No results for that query"));
        let now = chrono::NaiveDate::from_ymd_opt(2026, 11, 16).unwrap().and_hms_opt(18, 0, 0).unwrap();
        assert_eq!(offline_copy("Rain at 17:00.", "2026-11-16 14:05:09", now), "Rain at 17:00.\n\n(No internet right now: this is what I found today at 14:05.)");
        assert_eq!(kept_answer("calculate", &serde_json::json!({}), Err("timed out".into())), Err("timed out".to_string()), "only the online reads are kept");
    }

    #[test]
    fn process_reports() {
        let list = vec![
            ProcessUsage { name: "chrome".into(), cpu_pct: 23.0, memory_mb: 3200.0, count: 18 },
            ProcessUsage { name: "code".into(), cpu_pct: 4.0, memory_mb: 900.0, count: 6 },
        ];
        assert_eq!(spoken_processes(&list), "Google Chrome is the culprit, Sir: 23 percent of the processor and 3.1 gigabytes of memory.");
        assert!(processes_report(&list, 3).starts_with("Top CPU: chrome (23.0% CPU, 3200 MB, 18 processes)"));
        assert_eq!(spoken_from_report(&processes_report(&list, 3)), spoken_processes(&list));
        assert!(!top_processes().is_empty(), "this machine runs processes");
        assert!(spoken_network("OFFLINE: x").contains("offline"));
        assert_eq!(spoken_network("Online. Latency: Cloudflare 14 ms, Google 20 ms."), "We're online, Sir, with a latency of 14 milliseconds. The details are in the chat.");
    }
}
