//! Authenticated satellite telemetry, observed completion notifications and PC control.
use crate::app::Jai;
use crate::tools::ToolContext;
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::{OnceLock, RwLock};
use std::time::{Duration, SystemTime};
use sysinfo::{ProcessesToUpdate, System};
use tokio_util::sync::CancellationToken;

#[derive(Clone, Default, Serialize)]
pub struct ProcessInfo {
    pub pid: u32,
    pub name: String,
    pub cpu_percent: f32,
    pub memory_mb: u64,
}

#[derive(Clone, Default, Serialize)]
pub struct PagerEvent {
    pub id: u64,
    pub timestamp: String,
    pub message: String,
}

#[derive(Clone, Default, Serialize)]
pub struct GuardianSnapshot {
    pub sampled_at: String,
    pub processes: Vec<ProcessInfo>,
    pub notifications: VecDeque<PagerEvent>,
}

fn state() -> &'static RwLock<GuardianSnapshot> {
    static STATE: OnceLock<RwLock<GuardianSnapshot>> = OnceLock::new();
    STATE.get_or_init(|| RwLock::new(GuardianSnapshot::default()))
}

pub fn snapshot() -> GuardianSnapshot {
    state().read().unwrap_or_else(|e| e.into_inner()).clone()
}

fn notify(app: &Jai, message: String) {
    let mut current = state().write().unwrap_or_else(|e| e.into_inner());
    let id = current.notifications.back().map_or(1, |e| e.id + 1);
    current.notifications.push_back(PagerEvent { id, timestamp: chrono::Local::now().to_rfc3339(), message: message.clone() });
    while current.notifications.len() > 100 { current.notifications.pop_front(); }
    drop(current);
    app.tools().stark.push_to_satellite(&message);
}

/// Observe only actual changes after startup. A process exiting does not imply success.
pub fn start(app: Jai, shutdown: CancellationToken) {
    let worker_app = app.clone();
    let worker_shutdown = shutdown.clone();
    std::thread::spawn(move || {
        let mut system = System::new();
        system.refresh_cpu_usage();
        let mut jobs: HashMap<(u32, u64), String> = HashMap::new();
        let downloads = crate::features::user_folder("Downloads");
        let mut files: HashMap<PathBuf, (u64, SystemTime, u8)> = HashMap::new();
        let mut initialized = false;
        while !worker_shutdown.is_cancelled() {
            system.refresh_processes(ProcessesToUpdate::All);
            let mut running = HashMap::new();
            let mut rows = Vec::new();
            for (pid, process) in system.processes() {
                let name = process.name().to_string_lossy().to_string();
                let plain = name.to_lowercase().trim_end_matches(".exe").to_string();
                if matches!(plain.as_str(), "cargo" | "rustc" | "python" | "python3" | "ffmpeg" | "curl" | "wget" | "robocopy" | "node") {
                    running.insert((pid.as_u32(), process.start_time()), name.clone());
                }
                rows.push(ProcessInfo { pid: pid.as_u32(), name, cpu_percent: process.cpu_usage() / system.cpus().len().max(1) as f32, memory_mb: process.memory() / 1024 / 1024 });
            }
            rows.sort_by(|a, b| b.cpu_percent.total_cmp(&a.cpu_percent).then(b.memory_mb.cmp(&a.memory_mb)));
            rows.truncate(30);
            {
                let mut current = state().write().unwrap_or_else(|e| e.into_inner());
                current.sampled_at = chrono::Local::now().to_rfc3339();
                current.processes = rows;
            }
            if initialized {
                for (identity, name) in &jobs {
                    if !running.contains_key(identity) {
                        notify(&worker_app, format!("Process finished: {name} (PID {}). Exit status was not captured.", identity.0));
                    }
                }
            }
            jobs = running;
            if let Ok(entries) = std::fs::read_dir(&downloads) {
                for entry in entries.filter_map(Result::ok).take(10000) {
                    let path = entry.path();
                    let Ok(meta) = entry.metadata() else { continue };
                    if !meta.is_file() { continue; }
                    let modified = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
                    let name = entry.file_name().to_string_lossy().to_string();
                    let partial = [".crdownload", ".part", ".tmp", ".download"].iter().any(|s| name.to_ascii_lowercase().ends_with(s));
                    match files.get_mut(&path) {
                        None => { files.insert(path, (meta.len(), modified, if initialized { 0 } else { 255 })); }
                        Some((size, stamp, stable)) => {
                            if *size != meta.len() || *stamp != modified { *size = meta.len(); *stamp = modified; *stable = 0; }
                            else if *stable < 3 && !partial {
                                *stable += 1;
                                if *stable == 3 { notify(&worker_app, format!("Download folder: {name} is stable ({} bytes).", meta.len())); }
                            }
                        }
                    }
                }
                files.retain(|path, _| path.exists());
            }
            initialized = true;
            for _ in 0..5 {
                if worker_shutdown.is_cancelled() { return; }
                std::thread::sleep(Duration::from_secs(1));
            }
        }
    });
    let mut events = app.subscribe();
    tokio::spawn(async move {
        let mut timer = tokio::time::interval(Duration::from_secs(1));
        timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                _ = shutdown.cancelled() => break,
                _ = timer.tick() => {
                    match app.tools().workout.take_rest_completion() {
                        Ok(true) => app.raise_alert("Rest complete, Sir. Your next set is ready when you are.".into()),
                        Ok(false) => {},
                        Err(error) => tracing::warn!("[WORKOUT] Could not check rest timer: {error}"),
                    }
                }
                event = events.recv() => match event {
                    Ok(crate::hud::HudEvent::ToolLog { tool, success, .. }) => {
                        if matches!(tool.as_str(), "run_shell_command" | "download_or_extract_media" | "run_sandbox_calculation_or_script") {
                            notify(&app, format!("{tool} {}.", if success { "completed successfully" } else { "reported a failure" }));
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    _ => {},
                }
            }
        }
    });
}

pub fn definitions() -> Vec<Value> {
    vec![
        json!({"type":"function","function":{"name":"get_remote_pc_status","description":"Live CPU/GPU/RAM, active processes and observed finished jobs/downloads from the authenticated mobile satellite.","parameters":{"type":"object","properties":{}}}}),
        json!({"type":"function","function":{"name":"control_remote_pc","description":"Lock or sleep this PC, send Wake-on-LAN to the configured LAN target, or run a saved Stark protocol. The satellite cannot receive requests while this PC sleeps; waking this PC requires an independent always-on relay.","parameters":{"type":"object","properties":{"action":{"type":"string","enum":["lock","sleep","wake","protocol"]},"protocol":{"type":"string"}},"required":["action"]}}}),
    ]
}

pub fn is_tool(name: &str) -> bool { matches!(name, "get_remote_pc_status" | "control_remote_pc") }

pub async fn run_tool(ctx: &ToolContext, name: &str, args: &Value) -> Result<String, String> {
    if name == "get_remote_pc_status" {
        return serde_json::to_string_pretty(&json!({"telemetry":ctx.monitor.latest(),"guardian":snapshot(),"tunnel":crate::worldwide_tunnel::snapshot()})).map_err(|e| e.to_string());
    }
    match args["action"].as_str().unwrap_or("") {
        "lock" => { lock_pc()?; Ok("The PC is locked.".into()) }
        "sleep" => {
            // Let the authenticated request receive its acknowledgement before suspending networking.
            let stark = ctx.stark.clone();
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_secs(2)).await;
                match tokio::task::spawn_blocking(sleep_pc).await {
                    Ok(Ok(())) => stark.push_to_satellite("The PC resumed after the sleep request."),
                    Ok(Err(e)) => stark.push_to_satellite(&format!("Sleep failed: {e}")),
                    Err(e) => stark.push_to_satellite(&format!("Sleep task failed: {e}")),
                }
            });
            Ok("Sleep requested in two seconds. The satellite will be offline until this PC wakes; use an independent wake relay if configured.".into())
        }
        "wake" => {
            let settings = ctx.config.read(|c| c.satellite.clone());
            send_wol(&settings.wake_mac, &settings.wake_broadcast).await?;
            Ok("Wake-on-LAN packet sent to the configured LAN target. Waking is not confirmed; its firmware and network adapter must support it.".into())
        }
        "protocol" => {
            static RUNNING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
            struct ProtocolGuard;
            impl Drop for ProtocolGuard {
                fn drop(&mut self) { RUNNING.store(false, std::sync::atomic::Ordering::Release); }
            }
            if RUNNING.compare_exchange(false, true, std::sync::atomic::Ordering::AcqRel, std::sync::atomic::Ordering::Acquire).is_err() {
                return Err("A remote protocol is already running; recursive or concurrent protocol calls are not allowed.".into());
            }
            let _guard = ProtocolGuard;
            let protocol = args["protocol"].as_str().or_else(|| args["name"].as_str()).ok_or("A saved protocol name is required")?;
            if ctx.protocols.get(protocol).is_none() { return Err(format!("No saved protocol named {protocol}")); }
            let outcome = Box::pin(ctx.execute("manage_stark_protocol", &json!({"action":"run","name":protocol}))).await;
            if outcome.success { Ok(outcome.output) } else { Err(outcome.output) }
        }
        _ => Err("Choose lock, sleep, wake or protocol".into()),
    }
}

use crate::system_power::{lock_pc, sleep_pc};

fn magic_packet(mac: &str) -> Result<[u8; 102], String> {
    let hex = mac.replace([':', '-'], "");
    if hex.len() != 12 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) { return Err("Set satellite.wake_mac to the target adapter's six-byte MAC address first".into()); }
    let mut address = [0u8; 6];
    for (i, value) in address.iter_mut().enumerate() { *value = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).map_err(|e| e.to_string())?; }
    if address == [0; 6] || address[0] & 1 != 0 { return Err("A unicast adapter MAC address is required".into()); }
    let mut packet = [255u8; 102];
    for chunk in packet[6..].chunks_exact_mut(6) { chunk.copy_from_slice(&address); }
    Ok(packet)
}

pub(crate) async fn send_wol(mac: &str, broadcast: &str) -> Result<(), String> {
    let packet = magic_packet(mac)?;
    let address: std::net::Ipv4Addr = broadcast.parse().map_err(|_| "wake_broadcast must be an IPv4 broadcast address")?;
    let socket = tokio::net::UdpSocket::bind("0.0.0.0:0").await.map_err(|e| e.to_string())?;
    socket.set_broadcast(true).map_err(|e| e.to_string())?;
    socket.send_to(&packet, (address, 9)).await.map_err(|e| e.to_string())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn wake_packet_has_sync_and_sixteen_copies() {
        let packet = magic_packet("02:11:22:33:44:55").unwrap();
        assert_eq!(&packet[..6], &[255;6]);
        assert!(packet[6..].chunks_exact(6).all(|c| c == [2,17,34,51,68,85]));
        for bad in ["", "00", "xx:11:22:33:44:55", "00:00:00:00:00:00"] { assert!(magic_packet(bad).is_err()); }
    }
}
