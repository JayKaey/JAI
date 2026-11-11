//! Real hardware telemetry: CPU load, RAM, disks (sysinfo) and GPU (NVML, falling back to
//! `nvidia-smi`, or the SoC thermal zone on a Raspberry Pi). Sampled on a dedicated thread.

use crate::config::HardwareProfile;
use serde::Serialize;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};
use sysinfo::{Components, Disks, Networks, System};
use tokio::sync::watch;
use tracing::info;

/// Live windows refresh every second.
const SAMPLE_INTERVAL: Duration = Duration::from_millis(1000);
/// Free space changes slowly; the drive list itself (new USB sticks …) even more so.
const DISK_SPACE_INTERVAL: Duration = Duration::from_secs(5);
const DISK_LIST_INTERVAL: Duration = Duration::from_secs(30);

/// CPU temperature sensors are only read once something shows them (on Windows the sensor
/// query goes through WMI and is not free).
static WANT_CPU_TEMP: AtomicBool = AtomicBool::new(false);

pub fn want_cpu_temperature() {
    WANT_CPU_TEMP.store(true, Ordering::Relaxed);
}

/// The Guardian watches the processor's temperature even while nothing shows it: the sensors
/// are then read every 20 s instead of every 5.
static WATCH_CPU_TEMP: AtomicBool = AtomicBool::new(false);

pub fn watch_cpu_temperature() {
    WATCH_CPU_TEMP.store(true, Ordering::Relaxed);
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct GpuInfo {
    pub name: String,
    pub temperature_c: Option<f32>,
    pub utilization_pct: Option<f32>,
    pub vram_used_mb: Option<u64>,
    pub vram_total_mb: Option<u64>,
    pub power_w: Option<f32>,
    pub source: &'static str,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct DiskInfo {
    pub mount: String,
    pub total_gb: f32,
    pub free_gb: f32,
    pub used_pct: f32,
    /// Volume label ("Windows", "Games"), empty when it has none.
    pub label: String,
    /// NTFS, exFAT, ext4 …
    pub file_system: String,
    /// SSD, HDD or unknown.
    pub kind: String,
    pub removable: bool,
}

impl DiskInfo {
    pub fn used_gb(&self) -> f32 {
        (self.total_gb - self.free_gb).max(0.0)
    }

    pub fn free_pct(&self) -> f32 {
        (100.0 - self.used_pct).clamp(0.0, 100.0)
    }

    /// "C:" on Windows, the mount point elsewhere.
    pub fn letter(&self) -> String {
        self.mount.trim_end_matches(['\\', '/']).to_string()
    }
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct SystemTelemetry {
    pub cpu_name: String,
    pub cpu_cores: usize,
    pub cpu_usage_pct: f32,
    /// Load of every logical core (%).
    pub cpu_per_core: Vec<f32>,
    pub cpu_freq_mhz: u64,
    /// Hottest CPU sensor, when the platform exposes one and something asked for it.
    pub cpu_temp_c: Option<f32>,
    pub ram_used_gb: f32,
    pub ram_total_gb: f32,
    pub ram_usage_pct: f32,
    pub swap_used_gb: f32,
    pub swap_total_gb: f32,
    pub disks: Vec<DiskInfo>,
    pub gpu: Option<GpuInfo>,
    pub uptime_secs: u64,
    pub hardware_profile: String,
    /// Network throughput over all physical interfaces (KB/s).
    pub net_down_kbps: f32,
    pub net_up_kbps: f32,
}

impl SystemTelemetry {
    /// Compact, speakable/LLM-friendly summary.
    pub fn summary(&self) -> String {
        let mut s = format!(
            "CPU {} at {:.0}% load across {} threads. RAM {:.1} of {:.1} GB used ({:.0}%).",
            self.cpu_name.trim(),
            self.cpu_usage_pct,
            self.cpu_cores,
            self.ram_used_gb,
            self.ram_total_gb,
            self.ram_usage_pct
        );
        match &self.gpu {
            Some(g) => {
                s.push_str(&format!(" GPU {}", g.name));
                if let Some(t) = g.temperature_c {
                    s.push_str(&format!(" at {t:.0}°C"));
                }
                if let Some(u) = g.utilization_pct {
                    s.push_str(&format!(", {u:.0}% utilisation"));
                }
                if let (Some(used), Some(total)) = (g.vram_used_mb, g.vram_total_mb) {
                    s.push_str(&format!(", VRAM {:.1} of {:.1} GB", used as f32 / 1024.0, total as f32 / 1024.0));
                }
                s.push('.');
            }
            None => s.push_str(" No GPU telemetry available."),
        }
        if self.net_down_kbps > 0.0 || self.net_up_kbps > 0.0 {
            s.push_str(&format!(" Network: {:.0} KB/s down, {:.0} KB/s up.", self.net_down_kbps, self.net_up_kbps));
        }
        for d in &self.disks {
            s.push_str(&format!(" Drive {}: {:.0} GB free of {:.0} GB.", d.mount, d.free_gb, d.total_gb));
        }
        s
    }
}

#[derive(Clone)]
pub struct SystemMonitor {
    latest: Arc<RwLock<SystemTelemetry>>,
    updates: watch::Receiver<SystemTelemetry>,
}

impl SystemMonitor {
    /// Starts the sampler thread.
    pub fn start(profile: HardwareProfile) -> Self {
        let (tx, rx) = watch::channel(SystemTelemetry::default());
        let latest = Arc::new(RwLock::new(SystemTelemetry::default()));
        let latest_thread = Arc::clone(&latest);

        let spawned = std::thread::Builder::new()
            .name("jai-telemetry".into())
            .spawn(move || {
                let mut sampler = Sampler::new(profile);
                loop {
                    let snapshot = sampler.sample();
                    if let Ok(mut guard) = latest_thread.write() {
                        *guard = snapshot.clone();
                    }
                    if tx.send(snapshot).is_err() {
                        break;
                    }
                    std::thread::sleep(SAMPLE_INTERVAL);
                }
            });
        if let Err(e) = spawned {
            tracing::error!("[SYS] Could not start telemetry thread: {e}");
        }
        Self { latest, updates: rx }
    }

    pub fn latest(&self) -> SystemTelemetry {
        self.latest.read().map(|g| g.clone()).unwrap_or_default()
    }

    pub fn subscribe(&self) -> watch::Receiver<SystemTelemetry> {
        self.updates.clone()
    }
}

struct Sampler {
    sys: System,
    disks: Disks,
    disks_refreshed: Instant,
    disk_list_refreshed: Instant,
    networks: Networks,
    networks_refreshed: Instant,
    components: Option<Components>,
    components_refreshed: Instant,
    gpu: GpuProbe,
    profile: HardwareProfile,
}

impl Sampler {
    fn new(profile: HardwareProfile) -> Self {
        let mut sys = System::new();
        sys.refresh_cpu_all();
        sys.refresh_memory();
        let gpu = GpuProbe::detect();
        Self {
            sys,
            disks: Disks::new_with_refreshed_list(),
            disks_refreshed: Instant::now(),
            disk_list_refreshed: Instant::now(),
            networks: Networks::new_with_refreshed_list(),
            networks_refreshed: Instant::now(),
            components: None,
            components_refreshed: Instant::now(),
            gpu,
            profile,
        }
    }

    /// The hottest CPU-like sensor ("Package", "Tctl", "Core", "CPU"), refreshed every 5 s.
    fn cpu_temperature(&mut self) -> Option<f32> {
        let shown = WANT_CPU_TEMP.load(Ordering::Relaxed);
        if !shown && !WATCH_CPU_TEMP.load(Ordering::Relaxed) {
            return None;
        }
        if self.components.is_none() {
            self.components = Some(Components::new_with_refreshed_list());
            self.components_refreshed = Instant::now();
        }
        let refresh = self.components_refreshed.elapsed() > Duration::from_secs(if shown { 5 } else { 20 });
        let components = self.components.as_mut()?;
        if refresh {
            components.refresh();
            self.components_refreshed = Instant::now();
        }
        components
            .list()
            .iter()
            .filter(|c| {
                let l = c.label().to_lowercase();
                ["cpu", "package", "tctl", "tdie", "core", "k10temp", "coretemp", "acpitz", "thermal zone"].iter().any(|k| l.contains(k))
            })
            .map(|c| c.temperature())
            .filter(|t| t.is_finite() && *t > 5.0 && *t < 125.0)
            .max_by(|a, b| a.total_cmp(b))
    }

    fn sample(&mut self) -> SystemTelemetry {
        self.sys.refresh_cpu_all();
        self.sys.refresh_memory();
        if self.disk_list_refreshed.elapsed() > DISK_LIST_INTERVAL {
            self.disks.refresh_list();
            self.disk_list_refreshed = Instant::now();
            self.disks_refreshed = Instant::now();
        } else if self.disks_refreshed.elapsed() > DISK_SPACE_INTERVAL {
            self.disks.refresh();
            self.disks_refreshed = Instant::now();
        }

        let gib = 1024.0 * 1024.0 * 1024.0;
        let total = self.sys.total_memory() as f64;
        let used = self.sys.used_memory() as f64;
        let swap_total = self.sys.total_swap() as f64;
        let swap_used = self.sys.used_swap() as f64;
        let cpu_temp_c = self.cpu_temperature();
        let cpus = self.sys.cpus();
        // Every mounted volume Sir can see (C:\, D:\, USB drives …), not tiny system partitions.
        let mut disks: Vec<DiskInfo> = self
            .disks
            .list()
            .iter()
            .filter(|d| d.total_space() > 1024 * 1024 * 1024)
            .map(|d| {
                let total = d.total_space() as f64;
                let free = d.available_space() as f64;
                DiskInfo {
                    mount: d.mount_point().display().to_string(),
                    total_gb: (total / gib) as f32,
                    free_gb: (free / gib) as f32,
                    used_pct: if total > 0.0 { ((total - free) / total * 100.0) as f32 } else { 0.0 },
                    label: d.name().to_string_lossy().trim().to_string(),
                    file_system: d.file_system().to_string_lossy().trim().to_string(),
                    kind: match d.kind() {
                        sysinfo::DiskKind::SSD => "SSD".into(),
                        sysinfo::DiskKind::HDD => "HDD".into(),
                        _ => String::new(),
                    },
                    removable: d.is_removable(),
                }
            })
            .collect();
        disks.sort_by(|a, b| a.mount.cmp(&b.mount));
        disks.dedup_by(|a, b| a.mount == b.mount);
        disks.truncate(16);

        // Bytes since the previous refresh, over physical interfaces only.
        self.networks.refresh();
        let secs = self.networks_refreshed.elapsed().as_secs_f64().max(0.1);
        self.networks_refreshed = Instant::now();
        let (mut rx, mut tx) = (0u64, 0u64);
        for (name, data) in &self.networks {
            let n = name.to_lowercase();
            if n.contains("loopback") || n == "lo" || n.contains("vethernet") || n.contains("virtual") || n.starts_with("docker") || n.starts_with("veth") {
                continue;
            }
            rx += data.received();
            tx += data.transmitted();
        }

        SystemTelemetry {
            cpu_name: cpus.first().map(|c| c.brand().trim().to_string()).unwrap_or_default(),
            cpu_cores: cpus.len(),
            cpu_usage_pct: round1(self.sys.global_cpu_usage()),
            cpu_per_core: cpus.iter().map(|c| round1(c.cpu_usage())).collect(),
            cpu_freq_mhz: cpus.iter().map(|c| c.frequency()).max().unwrap_or(0),
            cpu_temp_c,
            ram_used_gb: round2((used / gib) as f32),
            ram_total_gb: round2((total / gib) as f32),
            ram_usage_pct: if total > 0.0 { round1((used / total * 100.0) as f32) } else { 0.0 },
            swap_used_gb: round2((swap_used / gib) as f32),
            swap_total_gb: round2((swap_total / gib) as f32),
            disks,
            gpu: self.gpu.sample(),
            uptime_secs: System::uptime(),
            hardware_profile: self.profile.display_name().to_string(),
            net_down_kbps: round1((rx as f64 / 1024.0 / secs) as f32),
            net_up_kbps: round1((tx as f64 / 1024.0 / secs) as f32),
        }
    }
}

fn round1(v: f32) -> f32 {
    (v * 10.0).round() / 10.0
}

fn round2(v: f32) -> f32 {
    (v * 100.0).round() / 100.0
}

enum GpuProbe {
    #[cfg(feature = "desktop-gpu")]
    Nvml(Box<nvml_wrapper::Nvml>),
    NvidiaSmi { last: Option<GpuInfo>, tick: u32 },
    ThermalZone,
    None,
}

impl GpuProbe {
    fn detect() -> Self {
        #[cfg(feature = "desktop-gpu")]
        match nvml_wrapper::Nvml::init() {
            Ok(nvml) => {
                if let Ok(name) = nvml.device_by_index(0).and_then(|d| d.name()) {
                    info!("[SYS] GPU telemetry via NVML: {name}");
                    return Self::Nvml(Box::new(nvml));
                }
            }
            Err(e) => tracing::debug!("[SYS] NVML unavailable: {e}"),
        }
        if nvidia_smi_query().is_some() {
            info!("[SYS] GPU telemetry via nvidia-smi");
            return Self::NvidiaSmi { last: None, tick: 0 };
        }
        if std::path::Path::new("/sys/class/thermal/thermal_zone0/temp").exists() {
            return Self::ThermalZone;
        }
        info!("[SYS] No GPU telemetry source found.");
        Self::None
    }

    fn sample(&mut self) -> Option<GpuInfo> {
        match self {
            #[cfg(feature = "desktop-gpu")]
            Self::Nvml(nvml) => {
                use nvml_wrapper::enum_wrappers::device::TemperatureSensor;
                let dev = nvml.device_by_index(0).ok()?;
                let mem = dev.memory_info().ok();
                Some(GpuInfo {
                    name: dev.name().unwrap_or_else(|_| "NVIDIA GPU".into()),
                    temperature_c: dev.temperature(TemperatureSensor::Gpu).ok().map(|t| t as f32),
                    utilization_pct: dev.utilization_rates().ok().map(|u| u.gpu as f32),
                    vram_used_mb: mem.as_ref().map(|m| m.used / (1024 * 1024)),
                    vram_total_mb: mem.as_ref().map(|m| m.total / (1024 * 1024)),
                    power_w: dev.power_usage().ok().map(|mw| mw as f32 / 1000.0),
                    source: "nvml",
                })
            }
            Self::NvidiaSmi { last, tick } => {
                // Spawning nvidia-smi costs ~50 ms; refresh every third sample.
                if tick.is_multiple_of(3) || last.is_none() {
                    *last = nvidia_smi_query();
                }
                *tick = tick.wrapping_add(1);
                last.clone()
            }
            Self::ThermalZone => {
                let raw = std::fs::read_to_string("/sys/class/thermal/thermal_zone0/temp").ok()?;
                let milli: f32 = raw.trim().parse().ok()?;
                Some(GpuInfo {
                    name: "SoC".into(),
                    temperature_c: Some(milli / 1000.0),
                    source: "thermal_zone",
                    ..Default::default()
                })
            }
            Self::None => None,
        }
    }
}

fn nvidia_smi_query() -> Option<GpuInfo> {
    let out = crate::shell::run_helper_blocking(
        std::path::Path::new("nvidia-smi"),
        ["--query-gpu=name,temperature.gpu,utilization.gpu,memory.used,memory.total,power.draw", "--format=csv,noheader,nounits"],
        std::time::Duration::from_secs(5),
    )
    .ok()?;
    if !out.success() {
        return None;
    }
    let line = String::from_utf8_lossy(&out.stdout).lines().next()?.to_string();
    let f: Vec<&str> = line.split(',').map(str::trim).collect();
    if f.len() < 5 {
        return None;
    }
    Some(GpuInfo {
        name: f[0].to_string(),
        temperature_c: f[1].parse().ok(),
        utilization_pct: f[2].parse().ok(),
        vram_used_mb: f[3].parse().ok(),
        vram_total_mb: f[4].parse().ok(),
        power_w: f.get(5).and_then(|p| p.parse().ok()),
        source: "nvidia-smi",
    })
}
