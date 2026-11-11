//! The Guardian: JAI watches over the machine on his own, like in the films. Every 10 s he
//! checks the latest telemetry (no extra sampling) for
//!
//! * the GPU above 83 °C or its VRAM above 95 % (Sir's 8 GB RTX 3060 Ti),
//! * the processor above 90 °C (where the board reports a sensor),
//! * system RAM above 90 %,
//! * the CPU pegged above 95 % for over a minute,
//! * any drive below 5 GB free,
//!
//! and, once a condition has lasted long enough, speaks up (through `raise_alert`, which honours
//! Sir's reply mode) naming the process responsible: "Pardon the interruption, Sir, but your GPU
//! VRAM is at 96 % capacity due to Chrome. Shall I open a live telemetry window or terminate
//! background tasks?". The offer is remembered, so "yes" opens the window and "terminate it"
//! closes the culprit — and both are buttons on the card the butler shows with the alert
//! (`proactive_butler::hardware`). Each kind of warning has a cooldown, so it never nags.

use crate::diagnostics::ProcessUsage;
use crate::telemetry::SystemTelemetry;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::time::{Duration, Instant};

pub const CHECK_INTERVAL: Duration = Duration::from_secs(10);
const COOLDOWN: Duration = Duration::from_secs(20 * 60);

pub const GPU_TEMP_LIMIT: f32 = 83.0;
pub const CPU_TEMP_LIMIT: f32 = 90.0;
pub const VRAM_LIMIT_PCT: f32 = 95.0;
pub const RAM_LIMIT_PCT: f32 = 90.0;
pub const CPU_LIMIT_PCT: f32 = 95.0;
pub const DISK_FREE_LIMIT_GB: f32 = 5.0;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Kind {
    GpuHot,
    CpuHot,
    VramFull,
    RamFull,
    CpuPegged,
    DiskLow(String),
}

impl Kind {
    /// How long the condition must hold before JAI speaks up.
    fn sustain(&self) -> Duration {
        match self {
            Self::GpuHot => Duration::from_secs(10),
            Self::CpuHot => Duration::from_secs(20),
            Self::VramFull | Self::RamFull => Duration::from_secs(30),
            Self::CpuPegged => Duration::from_secs(60),
            Self::DiskLow(_) => Duration::ZERO,
        }
    }

    /// Which resource names the culprit process.
    fn needs_culprit(&self) -> bool {
        !matches!(self, Self::DiskLow(_))
    }
}

/// A condition that just became worth an alert.
#[derive(Debug, Clone, PartialEq)]
pub struct Anomaly {
    pub kind: Kind,
    /// The measured value (°C, %, GB free).
    pub value: f32,
}

#[derive(Default)]
pub struct Guardian {
    since: HashMap<Kind, Instant>,
    last_alert: HashMap<Kind, Instant>,
}

/// What a confirmation after the alert does.
#[derive(Debug, Clone, PartialEq)]
pub struct Offer {
    /// "Yes" → this tool call (a live window).
    pub primary: (&'static str, Value),
    /// "Terminate it" → close this app.
    pub terminate: Option<String>,
}

impl Guardian {
    pub fn new() -> Self {
        Self::default()
    }

    fn conditions(t: &SystemTelemetry) -> Vec<Anomaly> {
        let mut out = Vec::new();
        if let Some(g) = &t.gpu {
            if let Some(temp) = g.temperature_c.filter(|c| *c > GPU_TEMP_LIMIT) {
                out.push(Anomaly { kind: Kind::GpuHot, value: temp });
            }
            if let (Some(used), Some(total)) = (g.vram_used_mb, g.vram_total_mb.filter(|t| *t > 0)) {
                let pct = used as f32 / total as f32 * 100.0;
                if pct > VRAM_LIMIT_PCT {
                    out.push(Anomaly { kind: Kind::VramFull, value: pct });
                }
            }
        }
        if let Some(temp) = t.cpu_temp_c.filter(|c| *c > CPU_TEMP_LIMIT) {
            out.push(Anomaly { kind: Kind::CpuHot, value: temp });
        }
        if t.ram_usage_pct > RAM_LIMIT_PCT {
            out.push(Anomaly { kind: Kind::RamFull, value: t.ram_usage_pct });
        }
        if t.cpu_usage_pct > CPU_LIMIT_PCT {
            out.push(Anomaly { kind: Kind::CpuPegged, value: t.cpu_usage_pct });
        }
        for d in t.disks.iter().filter(|d| !d.removable && d.total_gb > 16.0 && d.free_gb < DISK_FREE_LIMIT_GB) {
            out.push(Anomaly { kind: Kind::DiskLow(d.letter()), value: d.free_gb });
        }
        out
    }

    /// Conditions that have lasted long enough and are out of their cooldown.
    pub fn check(&mut self, t: &SystemTelemetry, now: Instant) -> Vec<Anomaly> {
        let current = Self::conditions(t);
        self.since.retain(|k, _| current.iter().any(|a| a.kind == *k));
        let mut due = Vec::new();
        for a in current {
            let since = *self.since.entry(a.kind.clone()).or_insert(now);
            let cooled = self.last_alert.get(&a.kind).is_none_or(|last| now.duration_since(*last) >= COOLDOWN);
            if now.duration_since(since) >= a.kind.sustain() && cooled {
                self.last_alert.insert(a.kind.clone(), now);
                due.push(a);
            }
        }
        due
    }
}

/// Processes JAI never offers to terminate.
fn untouchable(name: &str) -> bool {
    let n = name.to_lowercase();
    matches!(
        n.as_str(),
        "system" | "registry" | "memory compression" | "dwm" | "csrss" | "svchost" | "explorer" | "jai" | "ollama" | "ollama_llama_server"
            | "msmpeng" | "wininit" | "services" | "lsass" | "searchindexer" | "audiodg"
    ) || n.starts_with("ollama")
}

/// The process behind an anomaly: top CPU for the CPU, top memory for RAM; for the GPU the
/// language engine when it is loaded, otherwise the biggest GPU user Windows reports.
pub fn culprit(kind: &Kind, processes: &[ProcessUsage], gpu_pids: &[String]) -> Option<String> {
    let pick = |by_cpu: bool, only: Option<&[String]>| {
        processes
            .iter()
            .filter(|p| only.is_none_or(|o| o.iter().any(|n| n.eq_ignore_ascii_case(&p.name))))
            .max_by(|a, b| if by_cpu { a.cpu_pct.total_cmp(&b.cpu_pct) } else { a.memory_mb.total_cmp(&b.memory_mb) })
            .map(|p| p.name.clone())
    };
    match kind {
        Kind::CpuPegged | Kind::CpuHot => pick(true, None),
        Kind::RamFull => pick(false, None),
        Kind::VramFull | Kind::GpuHot => {
            if processes.iter().any(|p| p.name.to_lowercase().starts_with("ollama")) && gpu_pids.iter().any(|n| n.to_lowercase().starts_with("ollama")) {
                return Some("ollama".into());
            }
            let gpu_users: Vec<String> = gpu_pids.iter().filter(|n| !untouchable(n)).cloned().collect();
            if gpu_users.is_empty() {
                return pick(kind == &Kind::GpuHot, None);
            }
            pick(kind == &Kind::GpuHot, Some(&gpu_users)).or_else(|| gpu_users.first().cloned())
        }
        Kind::DiskLow(_) => None,
    }
}

/// The spoken alert and what "yes" / "terminate it" will do.
pub fn compose(a: &Anomaly, culprit: Option<&str>, t: &SystemTelemetry) -> (String, Offer) {
    let who = culprit.map(|c| if c.eq_ignore_ascii_case("ollama") { "my own language model".to_string() } else { crate::apps::display_name(c) });
    let due = who.as_ref().map(|w| format!(" due to {w}")).unwrap_or_default();
    let terminate = culprit.filter(|c| !untouchable(c)).map(String::from);
    let ask_terminate = if terminate.is_some() { " or terminate background tasks" } else { "" };
    let telemetry = ("spawn_native_window", json!({ "live": "pc_stats", "title": "PC Stats" }));
    let opener = "Pardon the interruption, Sir, but";
    match &a.kind {
        Kind::VramFull => (
            format!("{opener} your GPU VRAM is at {:.0}% capacity{due}. Shall I open a live telemetry window{ask_terminate}?", a.value),
            Offer { primary: telemetry, terminate },
        ),
        Kind::GpuHot => (
            format!("{opener} your graphics card is running hot at {:.0} degrees{due}. Shall I open a live telemetry window{ask_terminate}?", a.value),
            Offer { primary: telemetry, terminate },
        ),
        Kind::CpuHot => (
            format!("{opener} the processor is running hot at {:.0} degrees{due}. Shall I open a live telemetry window{ask_terminate}?", a.value),
            Offer { primary: telemetry, terminate },
        ),
        Kind::RamFull => (
            format!("{opener} system memory is at {:.0}% — {:.1} of {:.0} gigabytes{due}. Shall I open a live telemetry window{ask_terminate}?", a.value, t.ram_used_gb, t.ram_total_gb),
            Offer { primary: telemetry, terminate },
        ),
        Kind::CpuPegged => (
            format!("{opener} the processor has been above {CPU_LIMIT_PCT:.0}% for over a minute{due}. Shall I open the process monitor{ask_terminate}?"),
            Offer { primary: ("spawn_native_window", json!({ "live": "processes", "title": "Top Processes" })), terminate },
        ),
        Kind::DiskLow(drive) => (
            format!("{opener} drive {drive} is down to {:.1} gigabytes of free space. Shall I open the drive space window?", a.value),
            Offer { primary: ("spawn_native_window", json!({ "live": "drive_space", "title": "Drive Space" })), terminate: None },
        ),
    }
}

/// VRAM is full because JAI's own language model fills it (the engine loads as much of the
/// model as fits): without the model the card would be far from full, so it is no anomaly.
pub fn vram_is_ours(t: &SystemTelemetry, llm_vram_mb: Option<u64>) -> bool {
    let (Some(g), Some(llm)) = (t.gpu.as_ref(), llm_vram_mb) else { return false };
    let (Some(used), Some(total)) = (g.vram_used_mb, g.vram_total_mb.filter(|t| *t > 0)) else { return false };
    used.saturating_sub(llm) * 100 / total < 70
}

/// Whether the culprit needs the process list (sampling it costs half a second).
pub fn wants_processes(a: &[Anomaly]) -> bool {
    a.iter().any(|x| x.kind.needs_culprit())
}

/// Names of the processes using the GPU (NVML; empty without it).
pub fn gpu_process_names() -> Vec<String> {
    #[cfg(feature = "desktop-gpu")]
    {
        use std::sync::OnceLock;
        static NVML: OnceLock<Option<nvml_wrapper::Nvml>> = OnceLock::new();
        let Some(nvml) = NVML.get_or_init(|| nvml_wrapper::Nvml::init().ok()).as_ref() else { return Vec::new() };
        let Ok(dev) = nvml.device_by_index(0) else { return Vec::new() };
        let mut procs = dev.running_compute_processes().unwrap_or_default();
        procs.extend(dev.running_graphics_processes().unwrap_or_default());
        // Biggest VRAM users first where the driver reports it (it does not under WDDM).
        procs.sort_by_key(|p| std::cmp::Reverse(match p.used_gpu_memory {
            nvml_wrapper::enums::device::UsedGpuMemory::Used(b) => b,
            _ => 0,
        }));
        let mut sys = sysinfo::System::new();
        sys.refresh_processes(sysinfo::ProcessesToUpdate::All);
        let mut names: Vec<String> = Vec::new();
        for p in procs {
            if let Some(proc_) = sys.process(sysinfo::Pid::from_u32(p.pid)) {
                let name = proc_.name().to_string_lossy().trim_end_matches(".exe").to_string();
                if !names.contains(&name) {
                    names.push(name);
                }
            }
        }
        names
    }
    #[cfg(not(feature = "desktop-gpu"))]
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::telemetry::{DiskInfo, GpuInfo};

    fn hot_vram() -> SystemTelemetry {
        SystemTelemetry {
            gpu: Some(GpuInfo { name: "RTX 3060 Ti".into(), temperature_c: Some(70.0), vram_used_mb: Some(7900), vram_total_mb: Some(8192), ..Default::default() }),
            cpu_usage_pct: 10.0,
            ram_usage_pct: 50.0,
            ..Default::default()
        }
    }

    #[test]
    fn alerts_need_persistence_and_respect_cooldown() {
        let mut g = Guardian::new();
        let t0 = Instant::now();
        assert!(g.check(&hot_vram(), t0).is_empty(), "a spike is not an alert");
        assert!(g.check(&hot_vram(), t0 + Duration::from_secs(20)).is_empty());
        let due = g.check(&hot_vram(), t0 + Duration::from_secs(30));
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].kind, Kind::VramFull);
        assert!(g.check(&hot_vram(), t0 + Duration::from_secs(40)).is_empty(), "cooldown");
        assert!(g.check(&SystemTelemetry::default(), t0 + Duration::from_secs(50)).is_empty());
    }

    #[test]
    fn low_drives_alert_at_once_and_name_the_drive() {
        let mut g = Guardian::new();
        let t = SystemTelemetry {
            disks: vec![
                DiskInfo { mount: "C:\\".into(), total_gb: 930.0, free_gb: 3.2, used_pct: 99.6, ..Default::default() },
                DiskInfo { mount: "D:\\".into(), total_gb: 1800.0, free_gb: 600.0, used_pct: 66.0, ..Default::default() },
            ],
            ..Default::default()
        };
        let due = g.check(&t, Instant::now());
        assert_eq!(due, vec![Anomaly { kind: Kind::DiskLow("C:".into()), value: 3.2 }]);
        let (msg, offer) = compose(&due[0], None, &t);
        assert!(msg.contains("drive C: is down to 3.2 gigabytes"), "{msg}");
        assert_eq!(offer.primary.1["live"], "drive_space");
    }

    #[test]
    fn messages_name_the_culprit_and_offer_help() {
        let procs = vec![
            ProcessUsage { name: "chrome".into(), cpu_pct: 12.0, memory_mb: 5200.0, count: 20 },
            ProcessUsage { name: "Cyberpunk2077".into(), cpu_pct: 40.0, memory_mb: 9000.0, count: 1 },
        ];
        let a = Anomaly { kind: Kind::VramFull, value: 96.0 };
        let who = culprit(&a.kind, &procs, &["Cyberpunk2077".into(), "explorer".into()]);
        assert_eq!(who.as_deref(), Some("Cyberpunk2077"));
        let (msg, offer) = compose(&a, who.as_deref(), &hot_vram());
        assert_eq!(msg, "Pardon the interruption, Sir, but your GPU VRAM is at 96% capacity due to Cyberpunk2077. Shall I open a live telemetry window or terminate background tasks?");
        assert_eq!(offer.terminate.as_deref(), Some("Cyberpunk2077"));
        // The language model itself is never offered for termination.
        let (msg, offer) = compose(&a, Some("ollama"), &hot_vram());
        assert!(msg.contains("due to my own language model") && !msg.contains("terminate"), "{msg}");
        assert!(offer.terminate.is_none());
        assert_eq!(culprit(&Kind::RamFull, &procs, &[]).as_deref(), Some("Cyberpunk2077"));
    }

    #[test]
    fn jai_own_model_filling_vram_is_no_anomaly() {
        // 7.9 of 8 GB used, 5.6 GB of it JAI's model: the rest is a third of the card.
        assert!(vram_is_ours(&hot_vram(), Some(5600)));
        // A game holding 6.5 GB next to a small model is worth an alert.
        assert!(!vram_is_ours(&hot_vram(), Some(1400)));
        assert!(!vram_is_ours(&hot_vram(), None), "no model loaded");
    }

    #[test]
    fn a_hot_processor_is_an_alert_once_it_has_lasted() {
        let hot = SystemTelemetry { cpu_temp_c: Some(94.0), cpu_usage_pct: 60.0, ram_usage_pct: 40.0, ..Default::default() };
        let mut g = Guardian::new();
        let t0 = Instant::now();
        assert!(g.check(&hot, t0).is_empty(), "a spike is not an alert");
        let due = g.check(&hot, t0 + Duration::from_secs(20));
        assert_eq!(due, vec![Anomaly { kind: Kind::CpuHot, value: 94.0 }]);
        let procs = vec![ProcessUsage { name: "javaw".into(), cpu_pct: 71.0, memory_mb: 6100.0, count: 1 }, ProcessUsage { name: "chrome".into(), cpu_pct: 9.0, memory_mb: 3000.0, count: 14 }];
        let who = culprit(&due[0].kind, &procs, &[]);
        assert_eq!(who.as_deref(), Some("javaw"), "the process working the processor hardest");
        let (msg, offer) = compose(&due[0], who.as_deref(), &hot);
        assert!(msg.contains("the processor is running hot at 94 degrees"), "{msg}");
        assert_eq!(offer.primary.1["live"], "pc_stats");
        // No sensor, no alert; a warm processor is none either.
        assert!(g.check(&SystemTelemetry { cpu_temp_c: None, ..Default::default() }, t0 + Duration::from_secs(60)).is_empty());
        assert!(Guardian::new().check(&SystemTelemetry { cpu_temp_c: Some(78.0), ..Default::default() }, t0).is_empty());
    }
}
