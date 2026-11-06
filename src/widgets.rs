//! Dynamic native widgets and extra native windows, created and changed by Jarvis at runtime.
//!
//! * [`DynamicWidgetSpec`]: a serde-described widget (gauge, chart, timer, table, note, checklist,
//!   buttons, live command output …). Specs are persisted in `jarvis_memory.db` (`widgets`
//!   table), so custom widgets survive restarts; live values (telemetry samples, command output)
//!   are refreshed in memory by the runtime task.
//! * [`NativeWindowSpec`]: an additional real OS window (egui deferred viewport, drawn by
//!   `gui::widget_view`) showing any mix of widgets, text, markdown, code and live telemetry.
//!
//! The LLM tools (`create_or_update_widget`, `remove_widget`, `spawn_native_window`,
//! `update_native_window`, `close_native_window`) and the HUD both go through [`WidgetEngine`].

use crate::telemetry::{SystemMonitor, SystemTelemetry};
use crate::tools::{bool_arg, str_arg};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::collections::HashSet;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;
use tokio::sync::broadcast;
use tracing::{info, warn};

const MAX_WIDGETS: usize = 48;
const MAX_WINDOWS: usize = 12;
const MAX_TEXT: usize = 20_000;

fn hundred() -> f64 {
    100.0
}
fn sixty() -> usize {
    60
}
fn five() -> u64 {
    5
}
fn yes() -> bool {
    true
}

pub fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum TimerMode {
    #[default]
    #[serde(alias = "timer", alias = "alarm")]
    Countdown,
    Stopwatch,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct KeyValue {
    #[serde(alias = "k", alias = "label", alias = "name")]
    pub key: String,
    #[serde(alias = "v", default)]
    pub value: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ChecklistItem {
    #[serde(alias = "label", alias = "task", alias = "title")]
    pub text: String,
    #[serde(default, alias = "checked", alias = "completed")]
    pub done: bool,
}

/// `action`: `jarvis: <command>` (runs as if Sir typed it), `shell: <command>`, `skill: <name>`,
/// `app: <name>` or `url: <address>`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct QuickAction {
    #[serde(alias = "text", alias = "name")]
    pub label: String,
    #[serde(default, alias = "command")]
    pub action: String,
}

/// The widget types Jarvis can build (`"type"` in the JSON spec).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type")]
pub enum WidgetKind {
    #[serde(alias = "gauge", alias = "metric", alias = "metric_gauge")]
    MetricGauge {
        /// Live value: cpu, ram, gpu, gpu_temp, vram, disk:<drive>; none = set by Jarvis.
        #[serde(default)]
        source: Option<String>,
        #[serde(default)]
        value: f64,
        #[serde(default)]
        min: f64,
        #[serde(default = "hundred")]
        max: f64,
        #[serde(default)]
        unit: String,
        #[serde(default)]
        warn_at: Option<f64>,
    },
    #[serde(alias = "progress", alias = "progress_bar", alias = "bar")]
    ProgressBar {
        #[serde(default)]
        source: Option<String>,
        #[serde(default)]
        value: f64,
        #[serde(default = "hundred")]
        max: f64,
        #[serde(default)]
        unit: String,
    },
    #[serde(alias = "chart", alias = "line_chart", alias = "linechart", alias = "graph")]
    LiveLineChart {
        #[serde(default)]
        source: Option<String>,
        #[serde(default)]
        series: Vec<f64>,
        #[serde(default)]
        unit: String,
        #[serde(default = "sixty")]
        max_points: usize,
        #[serde(default)]
        min: Option<f64>,
        #[serde(default)]
        max: Option<f64>,
    },
    #[serde(alias = "sparkline", alias = "spark")]
    Sparkline {
        #[serde(default)]
        source: Option<String>,
        #[serde(default)]
        series: Vec<f64>,
        #[serde(default)]
        unit: String,
        #[serde(default = "sixty")]
        max_points: usize,
    },
    #[serde(alias = "timer", alias = "countdown", alias = "stopwatch", alias = "countdown_or_stopwatch")]
    CountdownOrStopwatch {
        #[serde(default)]
        mode: TimerMode,
        #[serde(default)]
        duration_secs: f64,
        /// Start of the current run (Unix ms) while running.
        #[serde(default)]
        started_at_ms: Option<i64>,
        /// Time accumulated by earlier runs.
        #[serde(default)]
        elapsed_ms: i64,
        #[serde(default = "yes")]
        running: bool,
        #[serde(default)]
        alerted: bool,
    },
    #[serde(alias = "key_value", alias = "key_value_list", alias = "kv", alias = "list", alias = "info")]
    KeyValueList {
        #[serde(default)]
        items: Vec<KeyValue>,
    },
    #[serde(alias = "table", alias = "data_table")]
    DataTable {
        #[serde(default)]
        columns: Vec<String>,
        #[serde(default)]
        rows: Vec<Vec<String>>,
    },
    #[serde(alias = "note", alias = "markdown", alias = "note_card", alias = "markdown_note_card", alias = "text")]
    MarkdownNoteCard {
        #[serde(default, alias = "text", alias = "content", alias = "body")]
        markdown: String,
    },
    #[serde(alias = "checklist", alias = "todo", alias = "todo_list", alias = "tasks")]
    Checklist {
        #[serde(default)]
        items: Vec<ChecklistItem>,
    },
    #[serde(alias = "buttons", alias = "quick_actions", alias = "actions", alias = "quick_action_buttons")]
    QuickActionButtons {
        #[serde(default)]
        buttons: Vec<QuickAction>,
    },
    #[serde(alias = "command_output", alias = "live_command", alias = "live_command_output", alias = "terminal")]
    LiveCommandOutput {
        #[serde(default)]
        command: String,
        #[serde(default = "five")]
        interval_secs: u64,
        #[serde(default)]
        output: String,
        #[serde(default)]
        last_run_ms: i64,
        #[serde(default = "yes")]
        ok: bool,
    },
}

impl WidgetKind {
    pub fn type_name(&self) -> &'static str {
        match self {
            Self::MetricGauge { .. } => "MetricGauge",
            Self::ProgressBar { .. } => "ProgressBar",
            Self::LiveLineChart { .. } => "LiveLineChart",
            Self::Sparkline { .. } => "Sparkline",
            Self::CountdownOrStopwatch { .. } => "CountdownOrStopwatch",
            Self::KeyValueList { .. } => "KeyValueList",
            Self::DataTable { .. } => "DataTable",
            Self::MarkdownNoteCard { .. } => "MarkdownNoteCard",
            Self::Checklist { .. } => "Checklist",
            Self::QuickActionButtons { .. } => "QuickActionButtons",
            Self::LiveCommandOutput { .. } => "LiveCommandOutput",
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum Dock {
    /// The widgets panel of the main HUD.
    #[default]
    Panel,
    /// Only inside native windows that list it.
    Window,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DynamicWidgetSpec {
    pub id: String,
    #[serde(default)]
    pub title: String,
    #[serde(flatten)]
    pub kind: WidgetKind,
    /// Accent colour (#RRGGBB) overriding the HUD's.
    #[serde(default)]
    pub accent: Option<String>,
    #[serde(default)]
    pub dock: Dock,
    #[serde(default)]
    pub updated_at: String,
}

impl DynamicWidgetSpec {
    fn sanitize(&mut self) {
        self.title = self.title.trim().chars().take(60).collect();
        if self.title.is_empty() {
            self.title = self.kind.type_name().to_string();
        }
        if let Some(a) = &self.accent {
            self.accent = crate::interface::parse_color(a).map(crate::interface::hex);
        }
        match &mut self.kind {
            WidgetKind::MetricGauge { min, max, source, .. } => {
                if *max <= *min {
                    *max = *min + 100.0;
                }
                *source = source.take().map(|s| s.trim().to_lowercase()).filter(|s| !s.is_empty());
            }
            WidgetKind::ProgressBar { max, source, .. } => {
                if *max <= 0.0 {
                    *max = 100.0;
                }
                *source = source.take().map(|s| s.trim().to_lowercase()).filter(|s| !s.is_empty());
            }
            WidgetKind::LiveLineChart { series, max_points, source, .. } | WidgetKind::Sparkline { series, max_points, source, .. } => {
                *max_points = (*max_points).clamp(2, 600);
                series.retain(|v| v.is_finite());
                if series.len() > *max_points {
                    series.drain(..series.len() - *max_points);
                }
                *source = source.take().map(|s| s.trim().to_lowercase()).filter(|s| !s.is_empty());
            }
            WidgetKind::CountdownOrStopwatch { duration_secs, running, started_at_ms, mode, .. } => {
                *duration_secs = duration_secs.clamp(0.0, 7.0 * 86_400.0);
                if *mode == TimerMode::Countdown && *duration_secs <= 0.0 {
                    *duration_secs = 60.0;
                }
                if *running && started_at_ms.is_none() {
                    *started_at_ms = Some(now_ms());
                }
                if !*running {
                    *started_at_ms = None;
                }
            }
            WidgetKind::DataTable { rows, .. } => rows.truncate(200),
            WidgetKind::Checklist { items } => {
                items.retain(|i| !i.text.trim().is_empty());
                items.truncate(100);
            }
            WidgetKind::KeyValueList { items } => items.truncate(100),
            WidgetKind::MarkdownNoteCard { markdown } => {
                if markdown.chars().count() > MAX_TEXT {
                    *markdown = markdown.chars().take(MAX_TEXT).collect();
                }
            }
            WidgetKind::QuickActionButtons { buttons } => {
                buttons.retain(|b| !b.label.trim().is_empty());
                for b in buttons.iter_mut().filter(|b| b.action.trim().is_empty()) {
                    b.action = format!("jarvis: {}", b.label);
                }
                buttons.truncate(24);
            }
            WidgetKind::LiveCommandOutput { interval_secs, .. } => *interval_secs = (*interval_secs).clamp(2, 3600),
        }
    }
}

/// Timer state at `now` (ms): (elapsed ms, remaining ms for countdowns).
pub fn timer_state(kind: &WidgetKind, now: i64) -> Option<(i64, Option<i64>)> {
    let WidgetKind::CountdownOrStopwatch { mode, duration_secs, started_at_ms, elapsed_ms, running, .. } = kind else {
        return None;
    };
    let elapsed = elapsed_ms + if *running { started_at_ms.map(|s| (now - s).max(0)).unwrap_or(0) } else { 0 };
    let remaining = (*mode == TimerMode::Countdown).then(|| ((duration_secs * 1000.0) as i64 - elapsed).max(0));
    Some((elapsed, remaining))
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BlockKind {
    Text,
    Markdown,
    Code,
    /// Compact live CPU/RAM/GPU/disk summary.
    Telemetry,
    /// Live PC stats: CPU overall + per core, RAM + swap, GPU load / temperature / VRAM.
    PcStats,
    /// Every mounted drive: label, file system, total / used / free space with capacity bars.
    DriveSpace,
    /// The programs using the most CPU and memory right now.
    Processes,
    /// Download / upload throughput with history.
    Network,
}

impl BlockKind {
    /// The live data views (refreshed every second), as Sir names them.
    pub fn live_from_name(raw: &str) -> Option<Self> {
        let n = raw.trim().to_lowercase().replace(['-', ' '], "_");
        Some(match n.as_str() {
            "pc_stats" | "pc_stat" | "pcstats" | "stats" | "system" | "system_stats" | "system_status" | "pc" | "performance" | "hardware"
            | "cpu" | "gpu" | "ram" | "memory" | "pc_statistics" | "computer_stats" | "telemetry_full" | "live_stats" => Self::PcStats,
            "drive_space" | "drives" | "drive" | "disks" | "disk" | "disk_space" | "storage" | "space" | "free_space" | "remaining_space"
            | "drive_usage" | "disk_usage" | "speicherplatz" | "festplatten" => Self::DriveSpace,
            "processes" | "process" | "top_processes" | "process_monitor" | "tasks" | "task_manager" | "programs" | "apps" => Self::Processes,
            "network" | "net" | "internet" | "bandwidth" | "network_traffic" | "traffic" | "netzwerk" => Self::Network,
            "telemetry" | "summary" | "system_summary" => Self::Telemetry,
            _ => return None,
        })
    }

    /// Default id, title and size of a window showing only this view.
    pub fn window_defaults(self) -> Option<(&'static str, &'static str, f32, f32)> {
        Some(match self {
            Self::PcStats => ("pc_stats", "PC Stats", 600.0, 660.0),
            Self::DriveSpace => ("drive_space", "Drive Space", 640.0, 460.0),
            Self::Processes => ("process_monitor", "Top Processes", 600.0, 520.0),
            Self::Network => ("network", "Network", 480.0, 340.0),
            Self::Telemetry => ("system_status", "System status", 460.0, 420.0),
            _ => return None,
        })
    }

    pub fn is_live(self) -> bool {
        !matches!(self, Self::Text | Self::Markdown | Self::Code)
    }

    pub fn id(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Markdown => "markdown",
            Self::Code => "code",
            Self::Telemetry => "telemetry",
            Self::PcStats => "pc_stats",
            Self::DriveSpace => "drive_space",
            Self::Processes => "processes",
            Self::Network => "network",
        }
    }
}

/// Live content kinds from tool arguments (`live`, `content`, `shows`, `add_content`): a string
/// ("pc_stats", "drives and processes") or a list.
fn live_kinds(v: Option<&Value>) -> Vec<BlockKind> {
    let mut out = Vec::new();
    let mut push = |s: &str| {
        for part in s.split([',', '+', '&']).flat_map(|p| p.split(" and ")) {
            if let Some(k) = BlockKind::live_from_name(part) {
                if !out.contains(&k) {
                    out.push(k);
                }
            }
        }
    };
    match v {
        Some(Value::String(s)) => push(s),
        Some(Value::Array(a)) => a.iter().filter_map(Value::as_str).for_each(&mut push),
        _ => {}
    }
    out
}

/// Single live values that become a gauge in a window ("add CPU temperature to that window").
pub(crate) fn gauge_source(raw: &str) -> Option<(&'static str, &'static str, &'static str)> {
    let n = raw.trim().to_lowercase().replace(['-', ' '], "_");
    Some(match n.as_str() {
        "cpu_temp" | "cpu_temperature" | "processor_temperature" | "cpu_temps" => ("cpu_temp", "CPU temperature", "°C"),
        "gpu_temp" | "gpu_temperature" | "graphics_temperature" => ("gpu_temp", "GPU temperature", "°C"),
        "cpu_load" | "cpu_usage" => ("cpu", "CPU load", "%"),
        "ram_usage" | "memory_usage" => ("ram", "RAM usage", "%"),
        "gpu_load" | "gpu_usage" => ("gpu", "GPU load", "%"),
        "vram" | "vram_usage" | "gpu_memory" => ("vram", "VRAM usage", "%"),
        "gpu_power" | "power" => ("gpu_power", "GPU power", "W"),
        _ => return None,
    })
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WindowBlock {
    pub kind: BlockKind,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub body: String,
    #[serde(default)]
    pub language: String,
}

/// How one of Jarvis' extra windows looks. Sir changes it from the window's hover toolbar or by
/// voice (`customize_hud_and_windows`, `update_native_window`); it is saved per window id in
/// `window_styles`, so a window opened again under that id keeps its look.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct WindowStyle {
    /// Background opacity 0.2-1.0 (text and controls stay opaque).
    pub opacity: f32,
    /// false = borderless (no title bar; drag the header to move).
    pub decorations: bool,
    /// Theme preset (see `interface::THEMES`); `None` follows the HUD.
    pub theme: Option<String>,
    /// #RRGGBB overrides on top of the theme.
    pub accent: Option<String>,
    pub background: Option<String>,
    /// Text size 0.8-1.6.
    pub font_scale: f32,
}

impl Default for WindowStyle {
    fn default() -> Self {
        Self { opacity: 1.0, decorations: true, theme: None, accent: None, background: None, font_scale: 1.0 }
    }
}

impl WindowStyle {
    pub const MIN_OPACITY: f32 = 0.2;
    pub const FONT_SCALE: (f32, f32) = (0.8, 1.6);

    fn sanitize(&mut self) {
        self.opacity = if self.opacity.is_finite() { self.opacity.clamp(Self::MIN_OPACITY, 1.0) } else { 1.0 };
        self.font_scale = if self.font_scale.is_finite() { self.font_scale.clamp(Self::FONT_SCALE.0, Self::FONT_SCALE.1) } else { 1.0 };
        self.theme = self.theme.take().and_then(|t| crate::interface::theme(&t).map(|t| t.name.to_string()));
        self.accent = self.accent.take().and_then(|c| crate::interface::parse_color(&c).map(crate::interface::hex));
        self.background = self.background.take().and_then(|c| crate::interface::parse_color(&c).map(crate::interface::hex));
    }

    /// Applies style fields of tool / toolbar arguments; returns what changed.
    pub fn apply(&mut self, args: &Value) -> Vec<String> {
        let mut done = Vec::new();
        let num = |k: &str| args.get(k).and_then(|v| v.as_f64().or_else(|| v.as_str().and_then(|s| s.trim().trim_end_matches('%').trim().parse().ok())));
        if let Some(o) = num("opacity").or_else(|| num("transparency").map(|t| if t <= 1.0 { 1.0 - t } else { 100.0 - t })) {
            self.opacity = (if o > 1.0 { o / 100.0 } else { o }) as f32;
            done.push(format!("opacity {:.0} %", self.opacity.clamp(Self::MIN_OPACITY, 1.0) * 100.0));
        }
        let borderless = bool_arg(args, "borderless")
            .or_else(|| bool_arg(args, "frameless"))
            .or_else(|| bool_arg(args, "decorations").map(|d| !d))
            .or_else(|| bool_arg(args, "title_bar").map(|t| !t));
        if let Some(b) = borderless {
            self.decorations = !b;
            done.push(if b { "borderless".into() } else { "title bar".into() });
        }
        if let Some(t) = str_arg(args, &["theme", "preset"]) {
            if matches!(t.to_lowercase().as_str(), "hud" | "follow" | "follow hud" | "default" | "same as hud" | "none") {
                self.theme = None;
                self.accent = None;
                self.background = None;
                done.push("theme of the HUD".into());
            } else if let Some(theme) = crate::interface::theme(t) {
                self.theme = Some(theme.name.to_string());
                self.accent = None;
                self.background = None;
                done.push(format!("theme {}", theme.display));
            } else if crate::interface::parse_color(t).is_some() {
                self.accent = Some(t.to_string());
                done.push(format!("accent {t}"));
            }
        }
        for (keys, field, label) in [
            (&["accent_color", "accent", "color", "colour"][..], &mut self.accent, "accent"),
            (&["background_color", "background"][..], &mut self.background, "background"),
        ] {
            if let Some(c) = str_arg(args, keys).filter(|c| crate::interface::parse_color(c).is_some()) {
                *field = Some(c.to_string());
                done.push(format!("{label} {c}"));
            }
        }
        let scale = args.get("font_scale").or_else(|| args.get("scale")).or_else(|| args.get("text_size"));
        if let Some(v) = scale {
            let next = match v.as_str().map(|s| s.trim().to_lowercase()).as_deref() {
                Some("bigger" | "larger" | "größer") => Some(self.font_scale * 1.15),
                Some("smaller" | "kleiner") => Some(self.font_scale / 1.15),
                Some("normal" | "default") => Some(1.0),
                _ => v.as_f64().or_else(|| v.as_str().and_then(|s| s.trim().trim_end_matches('%').parse().ok())).map(|x| if x > 3.0 { x / 100.0 } else { x } as f32),
            };
            if let Some(n) = next {
                self.font_scale = n;
                done.push(format!("text {:.0} %", n.clamp(Self::FONT_SCALE.0, Self::FONT_SCALE.1) * 100.0));
            }
        }
        self.sanitize();
        done
    }
}

/// An additional native window.
#[derive(Debug, Clone, PartialEq)]
pub struct NativeWindowSpec {
    pub id: String,
    pub title: String,
    pub width: f32,
    pub height: f32,
    /// Position in logical points (relative to the chosen monitor's work area when `monitor` is set).
    pub x: Option<f32>,
    pub y: Option<f32>,
    /// 1 = primary monitor, 2 = the next one, …
    pub monitor: Option<usize>,
    pub always_on_top: bool,
    /// Sir may drag its edges (content reflows either way).
    pub resizable: bool,
    pub min_size: [f32; 2],
    pub style: WindowStyle,
    pub widgets: Vec<String>,
    pub blocks: Vec<WindowBlock>,
    /// Bumped on every change (the GUI re-applies size/position/level when it changes).
    pub revision: u64,
    /// Where the window really is (logical x, y, width, height), reported by the GUI; relative
    /// changes ("make it smaller") start from here.
    pub actual: Option<[f32; 4]>,
    /// Bumped when Sir asks for the open window again: only then does it come to the front
    /// (never over a game for a mere resize or a live update).
    pub raise: u64,
}

impl NativeWindowSpec {
    /// The live views it shows ("pc_stats, drive_space").
    pub fn live_kinds(&self) -> Vec<&'static str> {
        self.blocks.iter().filter(|b| b.kind.is_live()).map(|b| b.kind.id()).collect()
    }
}

/// A widget finished something Sir must hear about (a countdown reached zero).
#[derive(Debug, Clone)]
pub struct WidgetAlert {
    pub widget_id: String,
    pub message: String,
}

struct Inner {
    widgets: RwLock<Vec<DynamicWidgetSpec>>,
    windows: RwLock<Vec<NativeWindowSpec>>,
    conn: Option<Mutex<Connection>>,
    revision: AtomicU64,
    window_seq: AtomicU64,
    alerts: broadcast::Sender<WidgetAlert>,
    running_commands: Mutex<HashSet<String>>,
    /// Drives in the latest sample (sizes the drive-space window).
    drive_count: std::sync::atomic::AtomicUsize,
    /// Top processes, sampled only while a window shows them.
    processes: RwLock<Vec<crate::diagnostics::ProcessUsage>>,
}

#[derive(Clone)]
pub struct WidgetEngine {
    inner: Arc<Inner>,
}

fn snake(raw: &str) -> String {
    crate::memory::MemoryManager::normalize_key(raw).chars().take(40).collect()
}

fn to_string_cell(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// Lenient argument shapes from the LLM → the canonical spec JSON.
fn normalize_payload(obj: &mut Map<String, Value>) {
    if let Some(items) = obj.get_mut("items") {
        match items.take() {
            Value::Object(map) => *items = Value::Array(map.into_iter().map(|(k, v)| json!({ "key": k, "value": to_string_cell(&v) })).collect()),
            Value::Array(list) => {
                *items = Value::Array(
                    list.into_iter()
                        .map(|item| match item {
                            Value::String(s) => json!({ "text": s, "key": s, "value": "" }),
                            Value::Array(pair) if pair.len() >= 2 => json!({ "key": to_string_cell(&pair[0]), "value": to_string_cell(&pair[1]) }),
                            Value::Object(mut o) => {
                                if let Some(v) = o.get("value").filter(|v| !v.is_string()).map(to_string_cell) {
                                    o.insert("value".into(), Value::String(v));
                                }
                                Value::Object(o)
                            }
                            other => json!({ "text": to_string_cell(&other), "key": to_string_cell(&other), "value": "" }),
                        })
                        .collect(),
                )
            }
            other => *items = other,
        }
    }
    if let Some(Value::Array(rows)) = obj.get_mut("rows") {
        for row in rows.iter_mut() {
            *row = match row.take() {
                Value::Array(cells) => Value::Array(cells.iter().map(|c| Value::String(to_string_cell(c))).collect()),
                Value::Object(o) => Value::Array(o.values().map(|c| Value::String(to_string_cell(c))).collect()),
                other => Value::Array(vec![Value::String(to_string_cell(&other))]),
            };
        }
    }
    if let Some(Value::Array(buttons)) = obj.get_mut("buttons") {
        for b in buttons.iter_mut() {
            if let Value::String(s) = b {
                *b = json!({ "label": s.clone(), "action": format!("jarvis: {s}") });
            }
        }
    }
    if let Some(Value::Array(series)) = obj.get_mut("series") {
        series.retain(|v| v.is_number());
    }
    for key in ["value", "min", "max", "warn_at", "duration_secs"] {
        if let Some(Value::String(s)) = obj.get(key) {
            if let Ok(n) = s.trim().trim_end_matches('%').parse::<f64>() {
                obj.insert(key.into(), json!(n));
            }
        }
    }
    // "duration_minutes": 5 / "minutes": 5 for timers.
    for (key, factor) in [("duration_minutes", 60.0), ("minutes", 60.0), ("seconds", 1.0), ("hours", 3600.0)] {
        if let Some(n) = obj.get(key).and_then(Value::as_f64) {
            obj.insert("duration_secs".into(), json!(n * factor));
        }
    }
}

/// Keys of the tool call that are instructions, not spec fields.
const CONTROL_KEYS: &[&str] = &[
    "push_value", "add_items", "check_item", "uncheck_item", "remove_item", "start", "pause", "reset", "widget_id", "from_last_reply",
    "duration_minutes", "minutes", "seconds", "hours", "window",
];

impl WidgetEngine {
    /// Opens the `widgets` table in Jarvis' memory database and loads the saved widgets.
    pub fn open<P: AsRef<Path>>(db_path: P) -> Result<Self, rusqlite::Error> {
        let conn = Connection::open(db_path.as_ref())?;
        conn.busy_timeout(Duration::from_secs(3))?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS widgets (
                 id TEXT PRIMARY KEY,
                 position INTEGER NOT NULL DEFAULT 0,
                 spec_json TEXT NOT NULL,
                 updated_at TEXT NOT NULL DEFAULT (datetime('now'))
             );
             CREATE TABLE IF NOT EXISTS window_styles (
                 id TEXT PRIMARY KEY,
                 style_json TEXT NOT NULL,
                 always_on_top INTEGER NOT NULL DEFAULT 0,
                 updated_at TEXT NOT NULL DEFAULT (datetime('now'))
             );",
        )?;
        let mut widgets = Vec::new();
        {
            let mut stmt = conn.prepare("SELECT id, spec_json FROM widgets ORDER BY position ASC, updated_at ASC")?;
            let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
            for (id, raw) in rows.flatten() {
                match serde_json::from_str::<DynamicWidgetSpec>(&raw) {
                    Ok(spec) => widgets.push(spec),
                    Err(e) => warn!("[WIDGET] Ignoring unreadable widget '{id}': {e}"),
                }
            }
        }
        info!("[WIDGET] {} widget(s) restored", widgets.len());
        Ok(Self::with(widgets, Some(conn)))
    }

    /// Not persisted (tests, self-test).
    pub fn in_memory() -> Self {
        Self::with(Vec::new(), None)
    }

    fn with(widgets: Vec<DynamicWidgetSpec>, conn: Option<Connection>) -> Self {
        let (alerts, _) = broadcast::channel(32);
        Self {
            inner: Arc::new(Inner {
                widgets: RwLock::new(widgets),
                windows: RwLock::new(Vec::new()),
                conn: conn.map(Mutex::new),
                revision: AtomicU64::new(1),
                window_seq: AtomicU64::new(0),
                alerts,
                running_commands: Mutex::new(HashSet::new()),
                drive_count: std::sync::atomic::AtomicUsize::new(0),
                processes: RwLock::new(Vec::new()),
            }),
        }
    }

    /// The latest process sample (for `Processes` blocks).
    pub fn processes(&self) -> Vec<crate::diagnostics::ProcessUsage> {
        self.inner.processes.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// The GUI reports where a window really is (logical x, y, width, height). Not a content
    /// change: no revision bump, no repaint.
    pub fn note_geometry(&self, id: &str, rect: [f32; 4]) {
        let mut list = self.inner.windows.write().unwrap_or_else(|e| e.into_inner());
        if let Some(w) = list.iter_mut().find(|w| w.id == id) {
            let changed = w.actual.is_none_or(|a| a.iter().zip(rect.iter()).any(|(x, y)| (x - y).abs() > 1.0));
            if changed {
                w.actual = Some(rect);
            }
        }
    }

    /// The window a phrase names: an id, a title ("the drive space window"), a live view it shows
    /// ("pc stats"), "last" or "that".
    pub fn find_window_named(&self, raw: &str) -> Option<NativeWindowSpec> {
        find_window_in(&self.inner.windows.read().unwrap_or_else(|e| e.into_inner()), raw)
    }

    pub fn revision(&self) -> u64 {
        self.inner.revision.load(Ordering::Acquire)
    }

    fn bump(&self) {
        self.inner.revision.fetch_add(1, Ordering::AcqRel);
    }

    pub fn subscribe_alerts(&self) -> broadcast::Receiver<WidgetAlert> {
        self.inner.alerts.subscribe()
    }

    pub fn widgets(&self) -> Vec<DynamicWidgetSpec> {
        self.inner.widgets.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn widget(&self, id: &str) -> Option<DynamicWidgetSpec> {
        self.inner.widgets.read().unwrap_or_else(|e| e.into_inner()).iter().find(|w| w.id == id).cloned()
    }

    pub fn windows(&self) -> Vec<NativeWindowSpec> {
        self.inner.windows.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    fn persist(&self, spec: &DynamicWidgetSpec) {
        let Some(conn) = &self.inner.conn else { return };
        let position = self.inner.widgets.read().unwrap_or_else(|e| e.into_inner()).iter().position(|w| w.id == spec.id).unwrap_or(0) as i64;
        let raw = serde_json::to_string(spec).unwrap_or_default();
        if let Err(e) = conn.lock().unwrap_or_else(|e| e.into_inner()).execute(
            "INSERT INTO widgets (id, position, spec_json, updated_at) VALUES (?1, ?2, ?3, datetime('now'))
             ON CONFLICT(id) DO UPDATE SET position = excluded.position, spec_json = excluded.spec_json, updated_at = excluded.updated_at",
            params![spec.id, position, raw],
        ) {
            warn!("[WIDGET] Could not save widget '{}': {e}", spec.id);
        }
    }

    /// The saved look of window `id` (style, always on top).
    fn load_style(&self, id: &str) -> Option<(WindowStyle, bool)> {
        let conn = self.inner.conn.as_ref()?.lock().unwrap_or_else(|e| e.into_inner());
        let (raw, top): (String, bool) = conn
            .query_row("SELECT style_json, always_on_top FROM window_styles WHERE id = ?1", params![id], |r| Ok((r.get(0)?, r.get(1)?)))
            .ok()?;
        let mut style: WindowStyle = serde_json::from_str(&raw).ok()?;
        style.sanitize();
        Some((style, top))
    }

    fn save_style(&self, spec: &NativeWindowSpec) {
        let Some(conn) = &self.inner.conn else { return };
        let raw = serde_json::to_string(&spec.style).unwrap_or_default();
        if let Err(e) = conn.lock().unwrap_or_else(|e| e.into_inner()).execute(
            "INSERT INTO window_styles (id, style_json, always_on_top, updated_at) VALUES (?1, ?2, ?3, datetime('now'))
             ON CONFLICT(id) DO UPDATE SET style_json = excluded.style_json, always_on_top = excluded.always_on_top, updated_at = excluded.updated_at",
            params![spec.id, raw, spec.always_on_top],
        ) {
            warn!("[WINDOW] Could not save the look of '{}': {e}", spec.id);
        }
    }

    fn unpersist(&self, id: &str) {
        if let Some(conn) = &self.inner.conn {
            let _ = conn.lock().unwrap_or_else(|e| e.into_inner()).execute("DELETE FROM widgets WHERE id = ?1", params![id]);
        }
    }

    /// Inserts or replaces a widget (and saves it).
    pub fn upsert(&self, mut spec: DynamicWidgetSpec) -> Result<DynamicWidgetSpec, String> {
        spec.sanitize();
        spec.updated_at = chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
        {
            let mut list = self.inner.widgets.write().unwrap_or_else(|e| e.into_inner());
            match list.iter_mut().find(|w| w.id == spec.id) {
                Some(existing) => *existing = spec.clone(),
                None => {
                    if list.len() >= MAX_WIDGETS {
                        return Err(format!("There are already {MAX_WIDGETS} widgets; remove one first."));
                    }
                    list.push(spec.clone());
                }
            }
        }
        self.persist(&spec);
        self.bump();
        Ok(spec)
    }

    /// Changes a widget in place; `save` persists it (structural changes, not live samples).
    pub fn modify(&self, id: &str, save: bool, f: impl FnOnce(&mut DynamicWidgetSpec)) -> Option<DynamicWidgetSpec> {
        let updated = {
            let mut list = self.inner.widgets.write().unwrap_or_else(|e| e.into_inner());
            let spec = list.iter_mut().find(|w| w.id == id)?;
            f(spec);
            spec.clone()
        };
        if save {
            self.persist(&updated);
        }
        self.bump();
        Some(updated)
    }

    pub fn remove(&self, id: &str) -> bool {
        let removed = {
            let mut list = self.inner.widgets.write().unwrap_or_else(|e| e.into_inner());
            let before = list.len();
            list.retain(|w| w.id != id);
            list.len() != before
        };
        if removed {
            self.unpersist(id);
            for w in self.inner.windows.write().unwrap_or_else(|e| e.into_inner()).iter_mut() {
                if w.widgets.iter().any(|x| x == id) {
                    w.widgets.retain(|x| x != id);
                    w.revision += 1;
                }
            }
            self.bump();
        }
        removed
    }

    fn resolve_id(&self, raw: &str) -> Option<String> {
        let list = self.inner.widgets.read().unwrap_or_else(|e| e.into_inner());
        if raw == "last" {
            return list.last().map(|w| w.id.clone());
        }
        let key = snake(raw);
        list.iter()
            .find(|w| w.id == raw || w.id == key)
            .or_else(|| list.iter().find(|w| snake(&w.title) == key))
            .map(|w| w.id.clone())
    }

    // ------------------------------------------------------------------- tool calls ----

    /// `create_or_update_widget`: creates a widget, or merges the given fields into an existing
    /// one (same `id`, or same title). Extra operations: `push_value`, `add_items`,
    /// `check_item` / `uncheck_item` / `remove_item` (index or text), `start` / `pause` / `reset`.
    pub fn create_or_update(&self, args: &Value) -> Result<String, String> {
        let mut patch = args.as_object().cloned().ok_or("create_or_update_widget needs a JSON object.")?;
        normalize_payload(&mut patch);
        let raw_id = str_arg(args, &["id", "widget_id"]).map(String::from);
        let existing = raw_id
            .as_deref()
            .and_then(|id| self.resolve_id(id))
            .or_else(|| str_arg(args, &["title"]).and_then(|t| self.resolve_id(t)))
            .and_then(|id| self.widget(&id));
        let id = existing
            .as_ref()
            .map(|w| w.id.clone())
            .or_else(|| raw_id.as_deref().map(snake))
            .filter(|id| !id.is_empty())
            .or_else(|| str_arg(args, &["title"]).map(snake).filter(|id| !id.is_empty()))
            .unwrap_or_else(|| format!("widget_{}", self.widgets().len() + 1));

        let type_changed = |w: &DynamicWidgetSpec| {
            patch.get("type").and_then(Value::as_str).is_some_and(|t| {
                serde_json::from_value::<WidgetKind>(json!({ "type": t })).map(|k| k.type_name() != w.kind.type_name()).unwrap_or(true)
            })
        };
        let mut base = match &existing {
            Some(w) if !type_changed(w) => serde_json::to_value(w).map_err(|e| e.to_string())?,
            Some(w) => json!({ "title": w.title, "accent": w.accent, "dock": w.dock }),
            None => json!({}),
        };
        let obj = base.as_object_mut().ok_or("internal widget error")?;
        for (k, v) in &patch {
            if !CONTROL_KEYS.contains(&k.as_str()) && k != "id" {
                obj.insert(k.clone(), v.clone());
            }
        }
        obj.insert("id".into(), Value::String(id.clone()));
        if !obj.contains_key("type") {
            return Err(
                "A new widget needs a 'type': MetricGauge, ProgressBar, LiveLineChart, Sparkline, CountdownOrStopwatch, KeyValueList, DataTable, MarkdownNoteCard, Checklist, QuickActionButtons or LiveCommandOutput."
                    .into(),
            );
        }
        let mut spec: DynamicWidgetSpec = serde_json::from_value(base).map_err(|e| format!("Invalid widget spec: {e}"))?;
        apply_operations(&mut spec, &patch);
        let created = existing.is_none();
        let spec = self.upsert(spec)?;
        info!("[WIDGET] {} '{}' ({})", if created { "Created" } else { "Updated" }, spec.id, spec.kind.type_name());
        Ok(format!(
            "{} widget '{}' ({}, \"{}\"){}.",
            if created { "Created" } else { "Updated" },
            spec.id,
            spec.kind.type_name(),
            spec.title,
            if spec.dock == Dock::Panel { " in the widgets panel of the HUD" } else { "" }
        ))
    }

    /// `remove_widget`: by id, title, "last" or "all".
    pub fn remove_tool(&self, args: &Value) -> Result<String, String> {
        let target = str_arg(args, &["id", "widget_id", "title", "name"]).ok_or("remove_widget needs 'id'.")?;
        if target == "all" {
            let ids: Vec<String> = self.widgets().into_iter().map(|w| w.id).collect();
            for id in &ids {
                self.remove(id);
            }
            return Ok(format!("Removed all {} widgets.", ids.len()));
        }
        let id = self.resolve_id(target).ok_or_else(|| format!("No widget matches '{target}'."))?;
        self.remove(&id);
        Ok(format!("Removed widget '{id}'."))
    }

    /// `spawn_native_window`.
    pub fn spawn_window(&self, args: &Value) -> Result<String, String> {
        // "A window with my PC stats": the live view decides the id, title and size, so asking
        // again brings the same window back instead of stacking copies.
        let live = live_kinds(args.get("live").or_else(|| args.get("content")).or_else(|| args.get("shows")));
        let defaults = (live.len() == 1).then(|| live[0].window_defaults()).flatten();
        let seq = self.inner.window_seq.load(Ordering::Acquire) + 1;
        let id = str_arg(args, &["id", "window_id"])
            .map(snake)
            .filter(|s| !s.is_empty())
            .or_else(|| defaults.map(|d| d.0.to_string()))
            .unwrap_or_else(|| format!("window_{seq}"));
        // In the HUD by default; its own OS window only when Sir said so (or named a monitor).
        let separate = args.get("separate_window").and_then(Value::as_bool) == Some(true) || args.get("monitor").is_some_and(|m| !m.is_null());
        if separate {
            crate::hud_dock::detach(&id);
        }
        if self.windows().iter().any(|w| w.id == id) {
            let mut args = args.clone();
            if let Some(o) = args.as_object_mut() {
                o.insert("id".into(), json!(id));
            }
            let out = self.update_window(&args)?;
            if let Some(w) = self.inner.windows.write().unwrap_or_else(|e| e.into_inner()).iter_mut().find(|w| w.id == id) {
                w.raise += 1;
            }
            self.bump();
            return Ok(format!("{out} (it was already open)"));
        }
        if self.windows().len() >= MAX_WINDOWS {
            return Err(format!("{MAX_WINDOWS} Jarvis windows are already open; close one first."));
        }
        self.inner.window_seq.fetch_add(1, Ordering::AcqRel);
        // A window opened again under a known id keeps the look Sir gave it.
        let (style, always_on_top) = self.load_style(&id).unwrap_or_default();
        let (default_title, width, height) = defaults.map(|d| (d.1, d.2, d.3)).unwrap_or(("J.A.R.V.I.S.", 560.0, 420.0));
        let mut spec = NativeWindowSpec {
            id: id.clone(),
            title: str_arg(args, &["title", "name"]).unwrap_or(default_title).chars().take(80).collect(),
            width,
            height,
            x: None,
            y: None,
            monitor: None,
            always_on_top,
            resizable: true,
            min_size: [220.0, 160.0],
            style,
            widgets: Vec::new(),
            blocks: live.iter().map(|k| WindowBlock { kind: *k, title: String::new(), body: String::new(), language: String::new() }).collect(),
            revision: 1,
            actual: None,
            raise: 0,
        };
        if spec.blocks.iter().any(|b| b.kind == BlockKind::DriveSpace) && defaults.is_some() {
            // Tall enough for every drive without scrolling.
            let drives = self.inner.drive_count.load(Ordering::Relaxed).max(2) as f32;
            spec.height = (150.0 + drives * 78.0).clamp(280.0, 900.0);
        }
        let before = (spec.style.clone(), spec.always_on_top);
        let created = self.apply_window_args(&mut spec, args, true)?;
        if (spec.style.clone(), spec.always_on_top) != before {
            self.save_style(&spec);
        }
        if spec.widgets.is_empty() && spec.blocks.is_empty() {
            spec.blocks.push(WindowBlock { kind: BlockKind::Telemetry, title: "System".into(), body: String::new(), language: String::new() });
        }
        let live = spec.live_kinds();
        let summary = format!(
            "Opened window '{id}' {} (\"{}\", {:.0}×{:.0}{}{}) with {}{} widget(s) and {} content block(s){}.",
            if separate { "as its own OS window" } else { "as a block inside the HUD (\"detach it\" gives it its own window)" },
            spec.title,
            spec.width,
            spec.height,
            spec.monitor.map(|m| format!(", on Monitor {m}")).unwrap_or_default(),
            if spec.always_on_top { ", always on top" } else { "" },
            if live.is_empty() { String::new() } else { format!("live {} (refreshed every second), ", live.join(" + ")) },
            spec.widgets.len(),
            spec.blocks.len(),
            if created.is_empty() { String::new() } else { format!("; created widgets {}", created.join(", ")) }
        );
        self.inner.windows.write().unwrap_or_else(|e| e.into_inner()).push(spec);
        self.bump();
        info!("[WINDOW] {summary}");
        Ok(summary)
    }

    /// `update_native_window`.
    pub fn update_window(&self, args: &Value) -> Result<String, String> {
        let raw = str_arg(args, &["id", "window_id"]).unwrap_or("last");
        let mut spec = self.find_window(raw).ok_or_else(|| format!("No Jarvis window matches '{raw}'."))?;
        let before = (spec.style.clone(), spec.always_on_top);
        self.apply_window_args(&mut spec, args, false)?;
        if (spec.style.clone(), spec.always_on_top) != before {
            self.save_style(&spec);
        }
        spec.revision += 1;
        let live = spec.live_kinds();
        let summary = format!(
            "Updated window '{}' (\"{}\", {:.0}×{:.0}{}{}{}).",
            spec.id,
            spec.title,
            spec.width,
            spec.height,
            spec.monitor.map(|m| format!(", Monitor {m}")).unwrap_or_default(),
            if spec.always_on_top { ", always on top" } else { "" },
            if live.is_empty() { String::new() } else { format!(", live {}", live.join(" + ")) }
        );
        {
            let mut list = self.inner.windows.write().unwrap_or_else(|e| e.into_inner());
            if let Some(w) = list.iter_mut().find(|w| w.id == spec.id) {
                *w = spec;
            }
        }
        self.bump();
        Ok(summary)
    }

    /// `close_native_window`: by id, title, "last" or "all".
    pub fn close_window(&self, raw: &str) -> Result<String, String> {
        let mut list = self.inner.windows.write().unwrap_or_else(|e| e.into_inner());
        if list.is_empty() {
            return Err("No Jarvis windows are open.".into());
        }
        let closed: Vec<String> = if raw == "all" {
            list.drain(..).map(|w| w.id).collect()
        } else {
            let id = find_window_in(&list, raw).map(|w| w.id).ok_or_else(|| format!("No Jarvis window matches '{raw}'."))?;
            list.retain(|w| w.id != id);
            vec![id]
        };
        drop(list);
        // Widgets that only lived in the closed windows go with them.
        let referenced: HashSet<String> = self.windows().into_iter().flat_map(|w| w.widgets).collect();
        for w in self.widgets().into_iter().filter(|w| w.dock == Dock::Window && !referenced.contains(&w.id)) {
            self.remove(&w.id);
        }
        self.bump();
        Ok(format!("Closed {} window(s): {}.", closed.len(), closed.join(", ")))
    }

    /// The GUI's close button on a window.
    pub fn window_closed_by_user(&self, id: &str) {
        let _ = self.close_window(id);
    }

    /// Restyles Jarvis windows (`target`: an id, a title, "last" or "all") from voice or the
    /// window's toolbar; the look is saved per window.
    pub fn style_window(&self, target: &str, args: &Value) -> Result<String, String> {
        let ids: Vec<String> = {
            let list = self.inner.windows.read().unwrap_or_else(|e| e.into_inner());
            if list.is_empty() {
                return Err("No Jarvis windows are open.".into());
            }
            if target == "all" {
                list.iter().map(|w| w.id.clone()).collect()
            } else {
                vec![find_window_in(&list, target).map(|w| w.id).ok_or_else(|| format!("No Jarvis window matches '{target}'."))?]
            }
        };
        let mut changes = Vec::new();
        let mut changed_specs = Vec::new();
        {
            let mut list = self.inner.windows.write().unwrap_or_else(|e| e.into_inner());
            for w in list.iter_mut().filter(|w| ids.contains(&w.id)) {
                let mut done = w.style.apply(args);
                if let Some(top) = bool_arg(args, "always_on_top").or_else(|| bool_arg(args, "pinned")) {
                    w.always_on_top = top;
                    done.push(if top { "always on top".into() } else { "not on top".into() });
                }
                if !done.is_empty() {
                    w.revision += 1;
                    changes = done;
                    changed_specs.push(w.clone());
                }
            }
        }
        if changed_specs.is_empty() {
            return Err("Nothing to change: pass opacity, borderless, theme, accent_color, background_color, font_scale or always_on_top.".into());
        }
        for spec in &changed_specs {
            self.save_style(spec);
        }
        self.bump();
        Ok(format!("Window{} {} restyled: {}.", if ids.len() > 1 { "s" } else { "" }, ids.join(", "), changes.join(", ")))
    }

    fn find_window(&self, raw: &str) -> Option<NativeWindowSpec> {
        find_window_in(&self.inner.windows.read().unwrap_or_else(|e| e.into_inner()), raw)
    }

    /// Size, place, level and content from tool arguments. Returns ids of widgets created inline.
    fn apply_window_args(&self, spec: &mut NativeWindowSpec, args: &Value, new: bool) -> Result<Vec<String>, String> {
        let num = |k: &str| args.get(k).and_then(|v| v.as_f64().or_else(|| v.as_str().and_then(|s| s.trim().trim_end_matches("px").parse().ok())));
        if !new {
            if let Some(t) = str_arg(args, &["title"]) {
                spec.title = t.chars().take(80).collect();
            }
        }
        // "Make it smaller / bigger / twice as big": relative to where it really is now.
        let (cur_w, cur_h) = spec.actual.map(|a| (a[2], a[3])).unwrap_or((spec.width, spec.height));
        let factor = match args.get("size").or_else(|| args.get("resize")).or_else(|| args.get("scale_window")) {
            Some(Value::String(s)) => match s.trim().to_lowercase().as_str() {
                "smaller" | "small" | "kleiner" | "shrink" => Some(0.8),
                "much smaller" | "tiny" => Some(0.6),
                "bigger" | "larger" | "big" | "größer" | "groesser" | "grow" => Some(1.25),
                "much bigger" | "much larger" | "huge" => Some(1.6),
                "half" => Some(0.5),
                "double" | "twice" => Some(2.0),
                other => other.trim_end_matches('%').trim().parse::<f32>().ok().map(|p| if p > 5.0 { p / 100.0 } else { p }),
            },
            Some(v) => v.as_f64().map(|f| f as f32),
            None => None,
        };
        if let Some(f) = factor.filter(|f| f.is_finite() && *f > 0.0) {
            spec.width = (cur_w * f).clamp(220.0, 3840.0);
            spec.height = (cur_h * f).clamp(160.0, 2160.0);
        }
        if let Some(w) = num("width") {
            spec.width = (w as f32).clamp(220.0, 3840.0);
        }
        if let Some(h) = num("height") {
            spec.height = (h as f32).clamp(160.0, 2160.0);
        }
        if let Some(x) = num("x") {
            spec.x = Some(x as f32);
        }
        if let Some(y) = num("y") {
            spec.y = Some(y as f32);
        }
        // Monitor 2, "left", "right", "primary", "next".
        if let Some(target) = args.get("monitor").or_else(|| args.get("monitor_index")).or_else(|| args.get("target_monitor")) {
            let monitors = crate::monitors::list();
            let current = spec.actual.and_then(|a| {
                // `actual` is logical; the monitor list is physical. Good enough to find "next".
                crate::monitors::monitor_at(&monitors, (a[0] + a[2] / 2.0) as i32, (a[1] + a[3] / 2.0) as i32)
            });
            match crate::monitors::resolve(target, current.or(spec.monitor), &monitors) {
                Some(m) => {
                    spec.monitor = Some(m);
                    // A new monitor without explicit coordinates: centred on it.
                    if num("x").is_none() && num("y").is_none() {
                        spec.x = None;
                        spec.y = None;
                    }
                }
                None if monitors.is_empty() => spec.monitor = num("monitor").map(|m| (m as usize).max(1)),
                None => return Err(format!("There is no monitor {target}; {} monitor(s) are connected.", monitors.len())),
            }
        }
        if let Some(top) = bool_arg(args, "always_on_top").or_else(|| bool_arg(args, "pinned")) {
            spec.always_on_top = top;
        }
        if let Some(r) = bool_arg(args, "resizable") {
            spec.resizable = r;
        }
        if let Some(Value::Array(min)) = args.get("min_size") {
            let v: Vec<f32> = min.iter().filter_map(|x| x.as_f64().map(|f| f as f32)).collect();
            if v.len() == 2 {
                spec.min_size = [v[0].clamp(120.0, 2000.0), v[1].clamp(100.0, 2000.0)];
            }
        }
        if let (Some(w), Some(h)) = (num("min_width"), num("min_height")) {
            spec.min_size = [(w as f32).clamp(120.0, 2000.0), (h as f32).clamp(100.0, 2000.0)];
        }
        spec.style.apply(args);
        if bool_arg(args, "replace_content").unwrap_or(false) {
            spec.blocks.clear();
            spec.widgets.clear();
        }
        let mut created = Vec::new();
        // Live views and single live values added to (or removed from) an open window.
        let add = |key: &str| args.get(key);
        let mut wanted: Vec<BlockKind> = live_kinds(add("live").or_else(|| add("content")).or_else(|| add("shows")));
        let mut gauges: Vec<(&str, &str, &str)> = Vec::new();
        for key in ["add_content", "add_live", "add"] {
            match add(key) {
                Some(Value::String(s)) => s.split([',', '+']).flat_map(|p| p.split(" and ")).for_each(|p| match gauge_source(p) {
                    Some(g) => gauges.push(g),
                    None => wanted.extend(BlockKind::live_from_name(p)),
                }),
                Some(Value::Array(a)) => a.iter().filter_map(Value::as_str).for_each(|p| match gauge_source(p) {
                    Some(g) => gauges.push(g),
                    None => wanted.extend(BlockKind::live_from_name(p)),
                }),
                _ => {}
            }
        }
        // An open window grows by what is added to it (unless Sir gave a height), so the new
        // content is not hidden below the fold.
        let mut grow = 0.0f32;
        for kind in wanted {
            if !spec.blocks.iter().any(|b| b.kind == kind) {
                spec.blocks.push(WindowBlock { kind, title: String::new(), body: String::new(), language: String::new() });
                grow += match kind {
                    BlockKind::PcStats => 560.0,
                    BlockKind::Processes => 400.0,
                    BlockKind::Network => 300.0,
                    BlockKind::DriveSpace => 260.0,
                    _ => 160.0,
                };
            }
        }
        for (source, title, unit) in gauges {
            if source == "cpu_temp" {
                crate::telemetry::want_cpu_temperature();
            }
            let id = format!("{}_{source}", spec.id);
            self.create_or_update(&json!({ "id": id, "type": "MetricGauge", "title": title, "source": source, "unit": unit, "dock": "window" }))?;
            if !spec.widgets.contains(&id) {
                spec.widgets.push(id.clone());
                created.push(id);
                grow += 200.0;
            }
        }
        if !new && grow > 0.0 && num("height").is_none() {
            let current = spec.actual.map(|a| a[3]).unwrap_or(spec.height);
            spec.height = (current + grow).clamp(160.0, 1100.0);
        }
        for key in ["remove_content", "remove_live"] {
            let names: Vec<String> = match add(key) {
                Some(Value::String(s)) => vec![s.clone()],
                Some(Value::Array(a)) => a.iter().filter_map(|v| v.as_str().map(String::from)).collect(),
                _ => Vec::new(),
            };
            for name in names {
                if let Some(kind) = BlockKind::live_from_name(&name) {
                    spec.blocks.retain(|b| b.kind != kind);
                }
                if let Some((source, ..)) = gauge_source(&name) {
                    let id = format!("{}_{source}", spec.id);
                    spec.widgets.retain(|w| *w != id);
                }
            }
        }
        if let Some(list) = args.get("widgets").and_then(Value::as_array) {
            for w in list {
                let mut w = w.clone();
                if let Some(o) = w.as_object_mut() {
                    o.entry("dock").or_insert(json!("window"));
                }
                self.create_or_update(&w)?;
                let id = str_arg(&w, &["id", "widget_id"]).map(snake).or_else(|| str_arg(&w, &["title"]).map(snake));
                if let Some(id) = id.and_then(|id| self.resolve_id(&id)).or_else(|| self.widgets().last().map(|w| w.id.clone())) {
                    if !spec.widgets.contains(&id) {
                        spec.widgets.push(id.clone());
                        created.push(id);
                    }
                }
            }
        }
        let ids = |key: &str| -> Vec<String> {
            args.get(key).and_then(Value::as_array).map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect()).unwrap_or_default()
        };
        for raw in ids("widget_ids").into_iter().chain(ids("add_widget_ids")) {
            let id = self.resolve_id(&raw).ok_or_else(|| format!("No widget matches '{raw}'."))?;
            if !spec.widgets.contains(&id) {
                spec.widgets.push(id);
            }
        }
        for raw in ids("remove_widget_ids") {
            if let Some(id) = self.resolve_id(&raw) {
                spec.widgets.retain(|w| *w != id);
            }
        }
        let block = |kind: BlockKind, body: &str| WindowBlock {
            kind,
            title: str_arg(args, &["content_title", "heading"]).unwrap_or("").to_string(),
            body: body.chars().take(MAX_TEXT).collect(),
            language: str_arg(args, &["language", "lang"]).unwrap_or("").to_string(),
        };
        if let Some(t) = str_arg(args, &["markdown"]) {
            spec.blocks.push(block(BlockKind::Markdown, t));
        }
        if let Some(t) = str_arg(args, &["text", "content", "body"]) {
            spec.blocks.push(block(BlockKind::Text, t));
        }
        if let Some(t) = str_arg(args, &["code"]) {
            spec.blocks.push(block(BlockKind::Code, t));
        }
        if bool_arg(args, "telemetry").unwrap_or(false) && !spec.blocks.iter().any(|b| b.kind == BlockKind::Telemetry) {
            spec.blocks.push(block(BlockKind::Telemetry, ""));
        }
        if let Some(list) = args.get("blocks").and_then(Value::as_array) {
            for b in list {
                if let Ok(b) = serde_json::from_value::<WindowBlock>(b.clone()) {
                    spec.blocks.push(b);
                }
            }
        }
        Ok(created)
    }

    // ------------------------------------------------------------------- HUD actions ----

    pub fn toggle_item(&self, id: &str, index: usize) {
        self.modify(id, true, |w| {
            if let WidgetKind::Checklist { items } = &mut w.kind {
                if let Some(item) = items.get_mut(index) {
                    item.done = !item.done;
                }
            }
        });
    }

    pub fn timer(&self, id: &str, op: &str) {
        self.modify(id, true, |w| timer_op(&mut w.kind, op));
    }

    /// Compact description for the LLM's live context.
    pub fn describe(&self) -> (String, String) {
        let widgets = self.widgets();
        let windows = self.windows();
        let w = if widgets.is_empty() {
            "none".to_string()
        } else {
            widgets.iter().map(|w| format!("{} ({} \"{}\")", w.id, w.kind.type_name(), w.title)).collect::<Vec<_>>().join(", ")
        };
        let win = if windows.is_empty() {
            "none".to_string()
        } else {
            windows
                .iter()
                .map(|x| {
                    let live = x.live_kinds();
                    format!(
                        "{} (\"{}\"{}, {:.0}×{:.0}{}{}, {} widget(s))",
                        x.id,
                        x.title,
                        if live.is_empty() { String::new() } else { format!(", live {}", live.join("+")) },
                        x.actual.map(|a| a[2]).unwrap_or(x.width),
                        x.actual.map(|a| a[3]).unwrap_or(x.height),
                        x.monitor.map(|m| format!(", Monitor {m}")).unwrap_or_default(),
                        if x.always_on_top { ", on top" } else { "" },
                        x.widgets.len()
                    )
                })
                .collect::<Vec<_>>()
                .join(", ")
        };
        (win, w)
    }

    // ---------------------------------------------------------------------- runtime ----

    /// Live telemetry for sourced widgets, scheduled command output, and timer alerts.
    pub fn start_runtime(&self, monitor: SystemMonitor) {
        let engine = self.clone();
        let mut telemetry = monitor.subscribe();
        tokio::spawn(async move {
            while telemetry.changed().await.is_ok() {
                let t = telemetry.borrow_and_update().clone();
                engine.sample(&t);
            }
        });
        let engine = self.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_millis(500));
            loop {
                tick.tick().await;
                engine.check_timers();
                engine.run_due_commands();
            }
        });
        // Process lists cost a full process scan: only while a window shows them.
        let engine = self.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_secs(2));
            loop {
                tick.tick().await;
                let wanted = engine.windows().iter().any(|w| w.blocks.iter().any(|b| b.kind == BlockKind::Processes));
                if !wanted {
                    continue;
                }
                if let Ok(list) = tokio::task::spawn_blocking(crate::diagnostics::top_processes).await {
                    *engine.inner.processes.write().unwrap_or_else(|e| e.into_inner()) = list;
                    engine.bump();
                }
            }
        });
    }

    fn sample(&self, t: &SystemTelemetry) {
        self.inner.drive_count.store(t.disks.len(), Ordering::Relaxed);
        let mut changed = false;
        {
            let mut list = self.inner.widgets.write().unwrap_or_else(|e| e.into_inner());
            for w in list.iter_mut() {
                match &mut w.kind {
                    WidgetKind::MetricGauge { source: Some(s), value, .. } | WidgetKind::ProgressBar { source: Some(s), value, .. } => {
                        if let Some(v) = telemetry_value(t, s) {
                            *value = v;
                            changed = true;
                        }
                    }
                    WidgetKind::LiveLineChart { source: Some(s), series, max_points, .. } | WidgetKind::Sparkline { source: Some(s), series, max_points, .. } => {
                        if let Some(v) = telemetry_value(t, s) {
                            series.push(v);
                            if series.len() > *max_points {
                                series.drain(..series.len() - *max_points);
                            }
                            changed = true;
                        }
                    }
                    _ => {}
                }
            }
        }
        if changed {
            self.bump();
        }
    }

    fn check_timers(&self) {
        let now = now_ms();
        let mut finished = Vec::new();
        {
            let mut list = self.inner.widgets.write().unwrap_or_else(|e| e.into_inner());
            for w in list.iter_mut() {
                let Some((_, Some(remaining))) = timer_state(&w.kind, now) else { continue };
                if let WidgetKind::CountdownOrStopwatch { running, alerted, elapsed_ms, started_at_ms, duration_secs, .. } = &mut w.kind {
                    if *running && remaining == 0 && !*alerted {
                        *alerted = true;
                        *running = false;
                        *elapsed_ms = (*duration_secs * 1000.0) as i64;
                        *started_at_ms = None;
                        finished.push(w.clone());
                    }
                }
            }
        }
        for w in finished {
            self.persist(&w);
            self.bump();
            let message = format!("Sir, your {} timer has finished.", w.title.trim_end_matches(" timer").trim_end_matches(" Timer"));
            info!("[WIDGET] Timer '{}' finished", w.id);
            let _ = self.inner.alerts.send(WidgetAlert { widget_id: w.id.clone(), message });
        }
    }

    fn run_due_commands(&self) {
        let now = now_ms();
        let due: Vec<(String, String)> = self
            .inner
            .widgets
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .filter_map(|w| match &w.kind {
                WidgetKind::LiveCommandOutput { command, interval_secs, last_run_ms, .. }
                    if !command.trim().is_empty() && now - last_run_ms >= *interval_secs as i64 * 1000 =>
                {
                    Some((w.id.clone(), command.clone()))
                }
                _ => None,
            })
            .collect();
        for (id, command) in due {
            if !self.inner.running_commands.lock().unwrap_or_else(|e| e.into_inner()).insert(id.clone()) {
                continue;
            }
            let engine = self.clone();
            tokio::spawn(async move {
                let result = crate::shell::run(&command).await;
                engine.modify(&id, false, |w| {
                    if let WidgetKind::LiveCommandOutput { output, last_run_ms, ok, .. } = &mut w.kind {
                        *last_run_ms = now_ms();
                        match result {
                            Ok(r) => {
                                *ok = r.exit_code == Some(0);
                                *output = r.output.chars().take(4000).collect();
                            }
                            Err(e) => {
                                *ok = false;
                                *output = e;
                            }
                        }
                    }
                });
                engine.inner.running_commands.lock().unwrap_or_else(|e| e.into_inner()).remove(&id);
            });
        }
    }

    /// Runs a `LiveCommandOutput` widget's command right away (HUD refresh button).
    pub fn refresh_now(&self, id: &str) {
        self.modify(id, false, |w| {
            if let WidgetKind::LiveCommandOutput { last_run_ms, .. } = &mut w.kind {
                *last_run_ms = 0;
            }
        });
    }
}

/// By id, title ("the drive space window"), the live view it shows ("pc stats") or "last".
fn find_window_in(list: &[NativeWindowSpec], raw: &str) -> Option<NativeWindowSpec> {
    if raw == "last" || raw.is_empty() {
        return list.last().cloned();
    }
    let lower = raw.trim().to_lowercase();
    let cleaned = lower.trim_start_matches("the ").trim_start_matches("my ").trim_start_matches("your ");
    let cleaned = cleaned.trim_end_matches(" window").trim_end_matches(" fenster").trim();
    let exact = |name: &str| {
        let key = snake(name);
        list.iter().find(|w| w.id == name || w.id == key).or_else(|| list.iter().find(|w| snake(&w.title) == key))
    };
    exact(raw)
        .or_else(|| exact(cleaned))
        .or_else(|| BlockKind::live_from_name(cleaned).and_then(|kind| list.iter().rev().find(|w| w.blocks.iter().any(|b| b.kind == kind))))
        .cloned()
}

pub fn timer_op(kind: &mut WidgetKind, op: &str) {
    let now = now_ms();
    if let WidgetKind::CountdownOrStopwatch { running, started_at_ms, elapsed_ms, alerted, .. } = kind {
        match op {
            "start" | "resume" if !*running => {
                *running = true;
                *started_at_ms = Some(now);
            }
            "pause" | "stop" if *running => {
                *elapsed_ms += started_at_ms.map(|s| now - s).unwrap_or(0);
                *running = false;
                *started_at_ms = None;
            }
            "reset" => {
                *elapsed_ms = 0;
                *alerted = false;
                *started_at_ms = if *running { Some(now) } else { None };
            }
            _ => {}
        }
    }
}

fn apply_operations(spec: &mut DynamicWidgetSpec, patch: &Map<String, Value>) {
    if let Some(v) = patch.get("push_value").and_then(|v| v.as_f64().or_else(|| v.as_str().and_then(|s| s.parse().ok()))) {
        match &mut spec.kind {
            WidgetKind::MetricGauge { value, .. } | WidgetKind::ProgressBar { value, .. } => *value = v,
            WidgetKind::LiveLineChart { series, .. } | WidgetKind::Sparkline { series, .. } => series.push(v),
            _ => {}
        }
    }
    if let Some(Value::Array(add)) = patch.get("add_items") {
        let mut wrapper = Map::new();
        wrapper.insert("items".into(), Value::Array(add.clone()));
        normalize_payload(&mut wrapper);
        let items = wrapper.remove("items").and_then(|v| v.as_array().cloned()).unwrap_or_default();
        match &mut spec.kind {
            WidgetKind::Checklist { items: list } => list.extend(items.into_iter().filter_map(|i| serde_json::from_value(i).ok())),
            WidgetKind::KeyValueList { items: list } => list.extend(items.into_iter().filter_map(|i| serde_json::from_value(i).ok())),
            WidgetKind::DataTable { rows, .. } => {
                for row in add {
                    if let Value::Array(cells) = row {
                        rows.push(cells.iter().map(to_string_cell).collect());
                    }
                }
            }
            _ => {}
        }
    }
    if let WidgetKind::Checklist { items } = &mut spec.kind {
        let pick = |v: &Value, items: &Vec<ChecklistItem>| -> Option<usize> {
            match v {
                Value::Number(n) => n.as_u64().map(|i| i as usize).filter(|i| *i < items.len()),
                Value::String(s) => {
                    let s = s.to_lowercase();
                    items.iter().position(|i| i.text.to_lowercase() == s).or_else(|| items.iter().position(|i| i.text.to_lowercase().contains(&s)))
                }
                _ => None,
            }
        };
        for (key, done) in [("check_item", true), ("uncheck_item", false)] {
            if let Some(i) = patch.get(key).and_then(|v| pick(v, items)) {
                items[i].done = done;
            }
        }
        if let Some(i) = patch.get("remove_item").and_then(|v| pick(v, items)) {
            items.remove(i);
        }
    }
    for op in ["start", "pause", "reset"] {
        if patch.get(op).and_then(Value::as_bool) == Some(true) {
            timer_op(&mut spec.kind, op);
        }
    }
}

/// Live sources: cpu, ram, gpu (utilisation), gpu_temp, vram, disk[:<mount>], ram_gb,
/// net_down / net_up (KB/s).
pub fn telemetry_value(t: &SystemTelemetry, source: &str) -> Option<f64> {
    let s = source.trim().to_lowercase();
    let gpu = t.gpu.as_ref();
    Some(match s.as_str() {
        "cpu" | "cpu_usage" | "cpu_load" | "processor" => t.cpu_usage_pct as f64,
        "ram" | "memory" | "ram_usage" | "mem" => t.ram_usage_pct as f64,
        "ram_gb" | "ram_used" => t.ram_used_gb as f64,
        "gpu" | "gpu_usage" | "gpu_load" | "gpu_util" => gpu?.utilization_pct? as f64,
        "gpu_temp" | "gpu_temperature" | "temperature" | "temp" => gpu?.temperature_c? as f64,
        "vram" | "gpu_memory" | "vram_usage" => {
            let g = gpu?;
            g.vram_used_mb? as f64 / g.vram_total_mb.filter(|t| *t > 0)? as f64 * 100.0
        }
        "gpu_power" | "power" => gpu?.power_w? as f64,
        "cpu_temp" | "cpu_temperature" => t.cpu_temp_c? as f64,
        "swap" | "swap_usage" => (t.swap_used_gb / t.swap_total_gb.max(0.01) * 100.0) as f64,
        "net_down" | "network" | "download" | "net_rx" => t.net_down_kbps as f64,
        "net_up" | "upload" | "net_tx" => t.net_up_kbps as f64,
        other if other.starts_with("disk") => {
            let mount = other.trim_start_matches("disk").trim_start_matches([':', '_', ' ']).trim_end_matches(['\\', '/', ':']);
            let d = if mount.is_empty() {
                t.disks.first()?
            } else {
                t.disks.iter().find(|d| d.mount.to_lowercase().trim_end_matches(['\\', '/', ':']) == mount)?
            };
            d.used_pct as f64
        }
        _ => return None,
    })
}

pub fn tool_definitions() -> Vec<Value> {
    vec![
        json!({"type": "function", "function": {
            "name": "create_or_update_widget",
            "description": "Create or change a live widget in your HUD's widgets panel (it persists). Types: MetricGauge/ProgressBar (value, min, max, unit; or source cpu|ram|gpu|gpu_temp|vram|disk:C for live data), LiveLineChart/Sparkline (series or a live source), CountdownOrStopwatch (mode countdown|stopwatch, duration_secs; alerts Sir when done), KeyValueList (items [{key,value}]), DataTable (columns, rows), MarkdownNoteCard (markdown), Checklist (items [{text,done}]), QuickActionButtons (buttons [{label, action: 'jarvis: <command>' | 'shell: <cmd>' | 'app: <name>' | 'url: <url>'}]), LiveCommandOutput (command, interval_secs). To update, pass the same id with only the fields to change, or push_value, add_items, check_item, start/pause/reset.",
            "parameters": {"type": "object", "properties": {
                "id": {"type": "string", "description": "snake_case id; reuse it to update"},
                "type": {"type": "string", "enum": ["MetricGauge", "ProgressBar", "LiveLineChart", "Sparkline", "CountdownOrStopwatch", "KeyValueList", "DataTable", "MarkdownNoteCard", "Checklist", "QuickActionButtons", "LiveCommandOutput"]},
                "title": {"type": "string"},
                "source": {"type": "string", "description": "Live data: cpu, ram, gpu, gpu_temp, vram, disk:C, net_down, net_up"},
                "value": {"type": "number"}, "min": {"type": "number"}, "max": {"type": "number"}, "unit": {"type": "string"},
                "series": {"type": "array", "items": {"type": "number"}},
                "mode": {"type": "string", "enum": ["countdown", "stopwatch"]},
                "duration_secs": {"type": "number"},
                "items": {"type": "array", "items": {"type": "object"}},
                "columns": {"type": "array", "items": {"type": "string"}},
                "rows": {"type": "array", "items": {"type": "array", "items": {"type": "string"}}},
                "markdown": {"type": "string"},
                "buttons": {"type": "array", "items": {"type": "object"}},
                "command": {"type": "string"}, "interval_secs": {"type": "integer"},
                "push_value": {"type": "number"}, "add_items": {"type": "array", "items": {"type": "object"}},
                "check_item": {"type": "string"}, "accent": {"type": "string"}
            }, "required": ["title"]}
        }}),
        json!({"type": "function", "function": {
            "name": "remove_widget",
            "description": "Remove a widget by id or title ('last' or 'all' allowed).",
            "parameters": {"type": "object", "properties": {"id": {"type": "string"}}, "required": ["id"]}
        }}),
        json!({"type": "function", "function": {
            "name": "spawn_native_window",
            "description": "Open a real native J.A.R.V.I.S. window. ALWAYS call it when Sir asks for a (new/second/separate) window — never just say you did. live = data refreshed every second: pc_stats (CPU per core, RAM, swap, GPU load/temperature/VRAM), drive_space (every drive's free and used space), processes (top CPU/memory programs), network (traffic). Or text, markdown, code and/or widgets. Optional size, monitor (1, 2, left, right, primary), always-on-top.",
            "parameters": {"type": "object", "properties": {
                "live": {"type": "string", "enum": ["pc_stats", "drive_space", "processes", "network"], "description": "Live data view"},
                "id": {"type": "string"}, "title": {"type": "string"},
                "width": {"type": "number"}, "height": {"type": "number"},
                "x": {"type": "number"}, "y": {"type": "number"},
                "monitor": {"type": "string", "description": "1 = primary, 2 = second …, left, right, primary, next"},
                "always_on_top": {"type": "boolean"},
                "resizable": {"type": "boolean"},
                "opacity": {"type": "number", "description": "Background opacity 0.2-1.0"},
                "borderless": {"type": "boolean"},
                "theme": {"type": "string", "description": "Theme preset for this window (stark, mark42, ultron, stealth, emerald …) or 'hud'"},
                "font_scale": {"type": "number", "description": "Text size 0.8-1.6"},
                "markdown": {"type": "string"}, "text": {"type": "string"},
                "code": {"type": "string"}, "language": {"type": "string"},
                "telemetry": {"type": "boolean", "description": "Show live CPU/RAM/GPU/disk"},
                "widget_ids": {"type": "array", "items": {"type": "string"}},
                "widgets": {"type": "array", "items": {"type": "object"}, "description": "New widget specs (same fields as create_or_update_widget)"},
                "from_last_reply": {"type": "boolean", "description": "Show your previous answer"}
            }, "required": ["title"]}
        }}),
        json!({"type": "function", "function": {
            "name": "update_native_window",
            "description": "Change one of your windows ('last' = newest, or its id/title/live view like drive_space): size (width/height in px, or size smaller|bigger), monitor, always-on-top, look (opacity, borderless, theme, font_scale), add_content (pc_stats, drive_space, processes, network, cpu_temp, gpu_temp, vram …) or remove_content, widgets, text.",
            "parameters": {"type": "object", "properties": {
                "id": {"type": "string"}, "title": {"type": "string"},
                "width": {"type": "number"}, "height": {"type": "number"}, "x": {"type": "number"}, "y": {"type": "number"},
                "size": {"type": "string", "description": "smaller, bigger, half, double or a factor"},
                "monitor": {"type": "string"}, "always_on_top": {"type": "boolean"}, "resizable": {"type": "boolean"},
                "add_content": {"type": "array", "items": {"type": "string"}},
                "remove_content": {"type": "array", "items": {"type": "string"}},
                "opacity": {"type": "number"}, "borderless": {"type": "boolean"}, "theme": {"type": "string"}, "font_scale": {"type": "number"},
                "markdown": {"type": "string"}, "text": {"type": "string"}, "code": {"type": "string"},
                "replace_content": {"type": "boolean"},
                "add_widget_ids": {"type": "array", "items": {"type": "string"}},
                "remove_widget_ids": {"type": "array", "items": {"type": "string"}},
                "telemetry": {"type": "boolean"}
            }, "required": ["id"]}
        }}),
        json!({"type": "function", "function": {
            "name": "close_native_window",
            "description": "Close a Jarvis window by id or title ('last' = newest, 'all').",
            "parameters": {"type": "object", "properties": {"id": {"type": "string"}}, "required": ["id"]}
        }}),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creates_merges_and_operates_on_widgets() {
        let e = WidgetEngine::in_memory();
        e.create_or_update(&json!({"type": "gauge", "title": "CPU load", "source": "CPU", "unit": "%"})).unwrap();
        let w = e.widget("cpu_load").unwrap();
        assert!(matches!(&w.kind, WidgetKind::MetricGauge { source: Some(s), max, .. } if s == "cpu" && *max == 100.0));

        e.create_or_update(&json!({"type": "Checklist", "title": "Groceries", "items": ["Milk", "Eggs"]})).unwrap();
        e.create_or_update(&json!({"id": "groceries", "add_items": ["Bread"], "check_item": "eggs"})).unwrap();
        match e.widget("groceries").unwrap().kind {
            WidgetKind::Checklist { items } => {
                assert_eq!(items.len(), 3);
                assert!(items[1].done && !items[0].done);
            }
            other => panic!("{other:?}"),
        }
        e.create_or_update(&json!({"type": "KeyValueList", "title": "Specs", "items": {"CPU": "Ryzen", "Cores": 16}})).unwrap();
        assert!(matches!(e.widget("specs").unwrap().kind, WidgetKind::KeyValueList { items } if items.len() == 2 && items[1].value == "16"));
        e.create_or_update(&json!({"type": "table", "title": "Results", "columns": ["A", "B"], "rows": [[1, "x"]]})).unwrap();
        assert!(matches!(e.widget("results").unwrap().kind, WidgetKind::DataTable { rows, .. } if rows[0][0] == "1"));
        assert!(e.create_or_update(&json!({"title": "No type"})).is_err());
        assert!(e.remove_tool(&json!({"id": "Specs"})).is_ok());
        assert!(e.widget("specs").is_none());
    }

    #[test]
    fn timers_count_down_and_alert_once() {
        let e = WidgetEngine::in_memory();
        let mut alerts = e.subscribe_alerts();
        e.create_or_update(&json!({"type": "timer", "title": "Tea", "minutes": 3})).unwrap();
        let w = e.widget("tea").unwrap();
        let (_, remaining) = timer_state(&w.kind, now_ms()).unwrap();
        assert!(remaining.unwrap() > 170_000);
        // Pretend it started four minutes ago.
        e.modify("tea", false, |w| {
            if let WidgetKind::CountdownOrStopwatch { started_at_ms, .. } = &mut w.kind {
                *started_at_ms = Some(now_ms() - 240_000);
            }
        });
        e.check_timers();
        e.check_timers();
        assert_eq!(alerts.try_recv().unwrap().message, "Sir, your Tea timer has finished.");
        assert!(alerts.try_recv().is_err(), "alerts only once");
    }

    #[test]
    fn windows_open_update_and_close_with_their_widgets() {
        let e = WidgetEngine::in_memory();
        e.spawn_window(&json!({
            "title": "Monitor", "width": 700, "always_on_top": true,
            "widgets": [{"type": "Sparkline", "title": "GPU temp", "source": "gpu_temp"}],
            "markdown": "# Hello"
        }))
        .unwrap();
        let w = &e.windows()[0];
        assert_eq!((w.width, w.always_on_top, w.widgets.len(), w.blocks.len()), (700.0, true, 1, 1));
        assert_eq!(e.widget("gpu_temp").unwrap().dock, Dock::Window);
        e.update_window(&json!({"id": "last", "title": "Renamed", "x": 10})).unwrap();
        assert_eq!(e.windows()[0].title, "Renamed");
        e.close_window("renamed").unwrap();
        assert!(e.windows().is_empty());
        assert!(e.widget("gpu_temp").is_none(), "window-only widgets close with their window");
        assert!(e.close_window("last").is_err());
    }

    #[test]
    fn telemetry_sources() {
        let t = SystemTelemetry {
            cpu_usage_pct: 42.0,
            ram_usage_pct: 60.0,
            disks: vec![crate::telemetry::DiskInfo { mount: "C:\\".into(), total_gb: 100.0, free_gb: 25.0, used_pct: 75.0, ..Default::default() }],
            ..Default::default()
        };
        assert_eq!(telemetry_value(&t, "cpu"), Some(42.0));
        assert_eq!(telemetry_value(&t, "disk:C"), Some(75.0));
        assert_eq!(telemetry_value(&t, "gpu"), None);
        assert_eq!(telemetry_value(&t, "banana"), None);
    }

    #[test]
    fn windows_open_inside_the_hud_unless_a_separate_window_is_asked_for() {
        let _dock = crate::hud_dock::test_lock();
        let e = WidgetEngine::in_memory();
        e.spawn_window(&json!({"id": "dock_test_inside", "title": "Inside", "live": "pc_stats"})).unwrap();
        assert!(!crate::hud_dock::is_detached("dock_test_inside"), "a HUD block by default");
        let out = e.spawn_window(&json!({"id": "dock_test_outside", "title": "Outside", "live": "pc_stats", "separate_window": true})).unwrap();
        assert!(crate::hud_dock::is_detached("dock_test_outside"));
        assert!(out.contains("own OS window"), "{out}");
        crate::hud_dock::forget("dock_test_inside");
        crate::hud_dock::forget("dock_test_outside");
    }
}
