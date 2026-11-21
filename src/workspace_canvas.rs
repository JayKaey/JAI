//! The cinematic spatial workspace: JAI fans out up to six borderless, rounded glass
//! windows from the orb ("bring up the research workspace", "open all windows", "show me
//! everything on …") — each a real OS window (an egui viewport) that glides into a balanced
//! Bento cockpit on spring physics, snaps magnetically to screen and window edges while dragged,
//! can be pinned on top or turned into a click-through ghost over a game, and collapses back
//! into the orb.
//!
//! Window types: the live crawler stream, the executive dossier, the knowledge-graph pinboard,
//! the code / data / plot inspector, the hardware & VRAM HUD and the schedule & command deck.

use crate::app::Jai;
use crate::deep_research_engine::{self as research, FactRow, MissionSnapshot, TelemetryKind};
use crate::research_graph_view::{contradiction, single, verified, GraphPick, ResearchGraphView};
use crate::tools::ToolContext;
use crate::window_physics::{self as physics, Guide, SpringF32, SpringVec2};
use eframe::egui::{
    self, pos2, vec2, Align, Align2, Color32, Frame, Margin, Pos2, Rect, RichText, Rounding, Sense, Stroke, TextFormat, Ui, UiBuilder, Vec2,
    ViewportBuilder, ViewportClass, ViewportCommand, ViewportId,
};
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, LazyLock, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};
use tokio::runtime::Handle;
use tracing::warn;

use crate::holo::{self, Face};

fn text_c() -> Color32 {
    holo::theme().ink()
}
fn muted_c() -> Color32 {
    holo::theme().muted()
}
fn live_c() -> Color32 {
    holo::theme().accent()
}
/// A caption or readout in the kit's faces (capitals for labels, thin mono for data).
fn mono(s: impl Into<String>, size: f32, colour: Color32) -> RichText {
    RichText::new(s).font(holo::font(Face::Mono, size)).color(colour)
}
fn body(s: impl Into<String>, size: f32, colour: Color32) -> RichText {
    RichText::new(s).font(holo::font(Face::Body, size)).color(colour)
}
fn label(s: &str, colour: Color32) -> RichText {
    RichText::new(s.to_uppercase()).font(holo::font(Face::Label, 9.0)).color(colour)
}
fn head(s: impl Into<String>, size: f32, colour: Color32) -> RichText {
    RichText::new(s).font(holo::font(Face::Head, size)).color(colour)
}
const HEADER_H: f32 = 30.0;
const DEFAULT_ALPHA: u8 = 228;
/// Launch stagger between windows of one fan-out.
const STAGGER: Duration = Duration::from_millis(70);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SpatialWindowKind {
    LiveCrawlerStream,
    ExecutiveDossier,
    KnowledgeGraphPinboard,
    CodeAndSandboxInspector,
    HardwareAndVramHud,
    ScheduleAndCommandDeck,
}

impl SpatialWindowKind {
    pub const ALL: [Self; 6] = [
        Self::LiveCrawlerStream,
        Self::ExecutiveDossier,
        Self::KnowledgeGraphPinboard,
        Self::CodeAndSandboxInspector,
        Self::HardwareAndVramHud,
        Self::ScheduleAndCommandDeck,
    ];
    pub const RESEARCH: [Self; 4] = [Self::LiveCrawlerStream, Self::ExecutiveDossier, Self::KnowledgeGraphPinboard, Self::CodeAndSandboxInspector];

    pub fn title(self) -> &'static str {
        match self {
            Self::LiveCrawlerStream => "LIVE CRAWLER",
            Self::ExecutiveDossier => "EXECUTIVE DOSSIER",
            Self::KnowledgeGraphPinboard => "KNOWLEDGE GRAPH",
            Self::CodeAndSandboxInspector => "CODE & SANDBOX",
            Self::HardwareAndVramHud => "HARDWARE & VRAM",
            Self::ScheduleAndCommandDeck => "SCHEDULE & COMMAND",
        }
    }

    pub fn tag(self) -> &'static str {
        match self {
            Self::LiveCrawlerStream => "LIVE STREAM",
            Self::ExecutiveDossier => "DOSSIER",
            Self::KnowledgeGraphPinboard => "PINBOARD",
            Self::CodeAndSandboxInspector => "INSPECTOR",
            Self::HardwareAndVramHud => "TELEMETRY",
            Self::ScheduleAndCommandDeck => "COMMAND DECK",
        }
    }

    /// The theme's accent for research, its deep tone for telemetry and code, ink for the schedule.
    pub fn accent(self) -> Color32 {
        let t = holo::theme();
        match self {
            Self::LiveCrawlerStream | Self::ExecutiveDossier | Self::KnowledgeGraphPinboard => t.accent(),
            Self::CodeAndSandboxInspector | Self::HardwareAndVramHud => t.deep(),
            Self::ScheduleAndCommandDeck => t.ink(),
        }
    }

    fn default_size(self) -> Vec2 {
        match self {
            Self::HardwareAndVramHud => vec2(520.0, 560.0),
            Self::ScheduleAndCommandDeck => vec2(560.0, 640.0),
            _ => vec2(640.0, 620.0),
        }
    }

    /// "telemetry window", "the graph", "dossier", "crawler" …
    pub fn parse(text: &str) -> Option<Self> {
        let t = text.to_lowercase().replace(['_', '-'], " ");
        let any = |words: &[&str]| words.iter().any(|w| t.contains(w));
        if any(&["crawler", "stream", "feed", "crawl"]) {
            Some(Self::LiveCrawlerStream)
        } else if any(&["dossier", "report", "summary", "executive"]) {
            Some(Self::ExecutiveDossier)
        } else if any(&["graph", "pinboard", "knowledge", "citation", "map"]) {
            Some(Self::KnowledgeGraphPinboard)
        } else if any(&["code", "sandbox", "inspector", "json", "csv", "plot"]) {
            Some(Self::CodeAndSandboxInspector)
        } else if any(&["hardware", "telemetry", "vram", "gpu", "cpu", "system", "hud", "performance"]) {
            Some(Self::HardwareAndVramHud)
        } else if any(&["schedule", "calendar", "timetable", "command", "deck", "untis", "briefing", "protocol"]) {
            Some(Self::ScheduleAndCommandDeck)
        } else {
            None
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InspectFormat {
    Text,
    Code,
    Json,
    Csv,
    Markdown,
    Plot,
}

impl InspectFormat {
    fn parse(s: &str, body: &str) -> Self {
        match s.to_lowercase().as_str() {
            "code" | "rust" | "python" | "js" | "javascript" | "c" | "cpp" | "java" | "shell" => Self::Code,
            "json" => Self::Json,
            "csv" | "table" | "tsv" => Self::Csv,
            "markdown" | "md" => Self::Markdown,
            "plot" | "graph" | "chart" | "function" => Self::Plot,
            "text" | "plain" => Self::Text,
            _ => {
                let b = body.trim_start();
                if (b.starts_with('{') || b.starts_with('[')) && serde_json::from_str::<Value>(b).is_ok() {
                    Self::Json
                } else if b.starts_with('#') {
                    Self::Markdown
                } else {
                    Self::Code
                }
            }
        }
    }
}

/// What the inspector shows.
#[derive(Debug, Clone)]
pub struct Inspector {
    pub title: String,
    pub body: String,
    pub format: InspectFormat,
}

/// A window of a recalled profile.
#[derive(Debug, Clone, PartialEq)]
pub struct ScenePlacement {
    pub kind: SpatialWindowKind,
    /// Physical screen pixels.
    pub rect: Rect,
    pub ghost: bool,
    pub pinned: bool,
    pub alpha: Option<u8>,
}

pub struct SpatialWindow {
    pub id: u64,
    pub kind: SpatialWindowKind,
    pub spring: SpringVec2,
    pub fade: SpringF32,
    /// Physical size (follows Sir's resizing).
    pub size: Vec2,
    /// Creation geometry handed to the viewport builder (kept constant while it is open).
    spawn_at: Pos2,
    spawn_size: Vec2,
    pending_size: Option<Vec2>,
    pub alpha: u8,
    pub ghost: bool,
    pub pinned: bool,
    pub collapsed: bool,
    pub closing: bool,
    /// Collapsed and gone into the orb (the OS window is closed).
    hidden: bool,
    restore_to: Option<Pos2>,
    pub mission_id: Option<i64>,
    /// A title of its own ("OVERVIEW & FACTS"), else the kind's.
    pub label: Option<String>,
    graph: Option<ResearchGraphView>,
    drag: Option<(Pos2, Pos2)>,
    guides: Vec<Guide>,
    guides_until: Option<Instant>,
    last_step: Option<Instant>,
    settled_since: Option<Instant>,
    launch_at: Instant,
    applied_ghost: Option<bool>,
    hwnd: Option<isize>,
    seen_selection: u64,
    focus: bool,
}

impl SpatialWindow {
    fn new(id: u64, kind: SpatialWindowKind, from: Pos2, to: Rect, mission_id: Option<i64>, delay: Duration) -> Self {
        let size = to.size();
        let spawn_at = from - size / 2.0;
        Self {
            id,
            kind,
            spring: SpringVec2::launch(spawn_at, to.min),
            fade: SpringF32 { target: 1.0, ..SpringF32::at(0.0) },
            size,
            spawn_at,
            spawn_size: size,
            pending_size: None,
            alpha: DEFAULT_ALPHA,
            ghost: false,
            pinned: false,
            collapsed: false,
            closing: false,
            hidden: false,
            restore_to: None,
            mission_id,
            label: None,
            graph: None,
            drag: None,
            guides: Vec::new(),
            guides_until: None,
            last_step: None,
            settled_since: None,
            launch_at: Instant::now() + delay,
            applied_ghost: None,
            hwnd: None,
            seen_selection: 0,
            focus: false,
        }
    }

    fn title(&self) -> String {
        format!("{} — J.A.I. {}", self.label.as_deref().unwrap_or(self.kind.title()), self.id)
    }

    fn open(&self) -> bool {
        !self.hidden && !self.closing
    }

    fn rect(&self) -> Rect {
        Rect::from_min_size(self.spring.target, self.size)
    }

    /// Sends the window to `to` (physical rect) on the spring; reopens it from the orb if it
    /// was collapsed.
    fn glide_to(&mut self, to: Rect, orb: Pos2) {
        if self.hidden {
            self.reopen(orb);
        }
        self.collapsed = false;
        self.closing = false;
        self.fade.target = 1.0;
        self.spring.retarget(to.min);
        if (to.size() - self.size).length() > 2.0 {
            self.pending_size = Some(to.size());
            self.size = to.size();
        }
    }

    fn reopen(&mut self, orb: Pos2) {
        self.hidden = false;
        self.spawn_at = orb - self.size / 2.0;
        self.spawn_size = self.size;
        self.spring = SpringVec2::launch(self.spawn_at, self.restore_to.unwrap_or(self.spring.target));
        self.fade = SpringF32 { target: 1.0, ..SpringF32::at(0.0) };
        self.hwnd = None;
        self.applied_ghost = None;
        self.last_step = None;
        self.launch_at = Instant::now();
    }

    /// Pulls the window back into the orb (closing, or collapsing to be restored later).
    fn retract(&mut self, orb: Pos2, close: bool) {
        if close {
            self.closing = true;
        } else {
            self.collapsed = true;
            self.restore_to = Some(self.spring.target);
        }
        self.drag = None;
        self.spring.retarget(orb - self.size / 2.0);
        self.fade.target = 0.0;
    }
}

#[derive(Default)]
pub struct WorkspaceCanvas {
    pub windows: Vec<SpatialWindow>,
    next_id: u64,
    pub selected_fact: Option<i64>,
    pub selected_source: Option<i64>,
    selection_revision: u64,
    pub inspect: Option<Inspector>,
    snapshots: HashMap<i64, (Instant, Arc<MissionSnapshot>)>,
    ctx: Option<egui::Context>,
    seen_research: u64,
    /// Topic workspaces: the overview dossier per mission.
    briefs: HashMap<i64, String>,
}

impl WorkspaceCanvas {
    fn snapshot(&mut self, mission: i64) -> Option<Arc<MissionSnapshot>> {
        let running = research::engine().is_some_and(|e| e.is_running(mission));
        let max_age = if running { Duration::from_millis(1500) } else { Duration::from_secs(10) };
        if let Some((at, snap)) = self.snapshots.get(&mission) {
            if at.elapsed() < max_age {
                return Some(Arc::clone(snap));
            }
        }
        let snap = Arc::new(research::snapshot(mission, 600)?);
        self.snapshots.insert(mission, (Instant::now(), Arc::clone(&snap)));
        Some(snap)
    }

    fn select(&mut self, fact: Option<i64>, source: Option<i64>) {
        self.selected_fact = fact;
        self.selected_source = source;
        self.selection_revision += 1;
    }

    /// Asks every workspace window (and the HUD, which registers them) to repaint.
    fn repaint_all(&self) {
        if let Some(ctx) = &self.ctx {
            ctx.request_repaint_of(ViewportId::ROOT);
            for w in &self.windows {
                ctx.request_repaint_of(viewport_id(w.id));
            }
        }
    }
}

pub static CANVAS: LazyLock<Mutex<WorkspaceCanvas>> = LazyLock::new(|| Mutex::new(WorkspaceCanvas { next_id: 1, ..Default::default() }));

fn canvas() -> MutexGuard<'static, WorkspaceCanvas> {
    CANVAS.lock().unwrap_or_else(|e| e.into_inner())
}

fn viewport_id(id: u64) -> ViewportId {
    ViewportId::from_hash_of(("jai_spatial", id))
}

/// The orb: the centre of the HUD window, else of the primary monitor (physical pixels).
fn orb_center() -> Pos2 {
    #[cfg(windows)]
    use crate::win32 as wm;
    #[cfg(not(windows))]
    use crate::x11 as wm;
    let hwnd = crate::gui::hud_hwnd();
    if hwnd != 0 && wm::is_visible(hwnd) {
        if let Some(([x, y, w, h], _, minimised)) = wm::window_state(hwnd) {
            if !minimised && w > 0 && h > 0 {
                return pos2((x + w / 2) as f32, (y + h / 2) as f32);
            }
        }
    }
    physics::screen_rects().first().map(|r| r.center()).unwrap_or(pos2(960.0, 540.0))
}

/// The window icon, decoded once.
fn icon() -> Option<Arc<egui::IconData>> {
    static ICON: OnceLock<Option<Arc<egui::IconData>>> = OnceLock::new();
    ICON.get_or_init(|| crate::gui::window_icon().map(Arc::new)).clone()
}

fn area_around(p: Pos2) -> Rect {
    let screens = physics::screen_rects();
    screens.iter().copied().find(|r| r.contains(p)).or_else(|| screens.first().copied()).unwrap_or(Rect::from_min_size(Pos2::ZERO, vec2(1920.0, 1080.0)))
}

/// After a change from a tool thread: new windows open (even with the HUD in the tray), open
/// ones repaint.
fn after_change() {
    canvas().repaint_all();
    crate::gui::sync_windows();
}

// --------------------------------------------------------------------- choreography ----

/// Opens (or restores) windows of these kinds and fans every open window out from the orb
/// into a balanced cockpit on the orb's monitor. Returns how many windows are open.
pub fn spawn_windows(kinds: &[SpatialWindowKind], mission_id: Option<i64>) -> usize {
    let specs: Vec<(SpatialWindowKind, Option<String>)> = kinds.iter().map(|k| (*k, None)).collect();
    spawn_with(&specs, mission_id)
}

/// The same with a title per window ("OVERVIEW & FACTS").
pub fn spawn_labeled(specs: &[(SpatialWindowKind, &str)], mission_id: Option<i64>) -> usize {
    let specs: Vec<(SpatialWindowKind, Option<String>)> = specs.iter().map(|(k, l)| (*k, Some(l.to_string()))).collect();
    spawn_with(&specs, mission_id)
}

/// The overview dossier of a topic workspace, shown above the mission's chapters.
pub fn set_brief(mission_id: i64, markdown: &str) {
    canvas().briefs.insert(mission_id, markdown.to_string());
}

fn spawn_with(specs: &[(SpatialWindowKind, Option<String>)], mission_id: Option<i64>) -> usize {
    let orb = orb_center();
    let area = area_around(orb);
    let count = {
        let mut c = canvas();
        let mut fresh = 0u32;
        for (kind, label) in specs {
            let kind = *kind;
            match c.windows.iter_mut().find(|w| w.kind == kind && !w.closing) {
                Some(w) => {
                    if mission_id.is_some() {
                        w.mission_id = mission_id;
                    }
                    if label.is_some() {
                        w.label = label.clone();
                    }
                    if w.collapsed {
                        w.glide_to(Rect::from_min_size(w.restore_to.unwrap_or(w.spring.target), w.size), orb);
                    }
                }
                None => {
                    let id = c.next_id;
                    c.next_id += 1;
                    let start = Rect::from_center_size(orb, kind.default_size());
                    let mut w = SpatialWindow::new(id, kind, orb, start, mission_id, STAGGER * fresh);
                    w.label = label.clone();
                    c.windows.push(w);
                    fresh += 1;
                }
            }
        }
        // The cockpit: every open window of this monitor, in kind order.
        let mut open: Vec<usize> = (0..c.windows.len()).filter(|&i| c.windows[i].open() && !c.windows[i].collapsed).collect();
        open.sort_by_key(|&i| SpatialWindowKind::ALL.iter().position(|k| *k == c.windows[i].kind).unwrap_or(9));
        let slots = physics::fan_out_layout(open.len(), area, orb);
        for (slot, &i) in slots.into_iter().zip(open.iter()) {
            c.windows[i].glide_to(slot, orb);
        }
        c.windows.iter().filter(|w| w.open()).count()
    };
    after_change();
    count
}

/// The research cockpit (crawler, dossier, graph, inspector) for a mission.
pub fn spawn_research_workspace(mission_id: Option<i64>) -> usize {
    spawn_windows(&SpatialWindowKind::RESEARCH, mission_id)
}

/// Recalls a profile: matching windows glide to their places, missing ones fan out of the
/// orb, the rest are pulled back into it.
pub fn apply_scene(placements: Vec<ScenePlacement>, mission_id: Option<i64>) {
    let orb = orb_center();
    {
        let mut c = canvas();
        let mut used: Vec<u64> = Vec::new();
        let mut fresh = 0u32;
        for p in &placements {
            let index = match c.windows.iter().position(|w| w.kind == p.kind && !w.closing && !used.contains(&w.id)) {
                Some(i) => {
                    c.windows[i].glide_to(p.rect, orb);
                    i
                }
                None => {
                    let id = c.next_id;
                    c.next_id += 1;
                    c.windows.push(SpatialWindow::new(id, p.kind, orb, p.rect, mission_id, STAGGER * fresh));
                    fresh += 1;
                    c.windows.len() - 1
                }
            };
            let w = &mut c.windows[index];
            w.ghost = p.ghost;
            w.pinned = p.pinned || p.ghost;
            w.alpha = p.alpha.unwrap_or(DEFAULT_ALPHA);
            if w.mission_id.is_none() {
                w.mission_id = mission_id;
            }
            used.push(w.id);
        }
        for w in c.windows.iter_mut().filter(|w| !used.contains(&w.id) && w.open()) {
            w.retract(orb, true);
        }
    }
    after_change();
}

/// Where the open windows are (for saving a profile).
pub fn window_placements() -> Vec<ScenePlacement> {
    canvas()
        .windows
        .iter()
        .filter(|w| w.open() && !w.collapsed)
        .map(|w| ScenePlacement { kind: w.kind, rect: w.rect(), ghost: w.ghost, pinned: w.pinned, alpha: Some(w.alpha) })
        .collect()
}

fn targets(c: &mut WorkspaceCanvas, kind: Option<SpatialWindowKind>) -> impl Iterator<Item = &mut SpatialWindow> {
    c.windows.iter_mut().filter(move |w| !w.closing && kind.is_none_or(|k| w.kind == k))
}

/// Ghost mode (click-through, topmost, 45 % glass) for one kind or all windows.
pub fn set_ghost(kind: Option<SpatialWindowKind>, on: bool) -> usize {
    let n = {
        let mut c = canvas();
        let mut n = 0;
        for w in targets(&mut c, kind) {
            w.ghost = on;
            if on {
                w.pinned = true;
            }
            n += 1;
        }
        n
    };
    after_change();
    n
}

pub fn unghost_all() -> usize {
    set_ghost(None, false)
}

pub fn ghost_count() -> usize {
    canvas().windows.iter().filter(|w| w.ghost && w.open()).count()
}

pub fn set_pinned(kind: Option<SpatialWindowKind>, on: bool) -> usize {
    let n = {
        let mut c = canvas();
        let mut n = 0;
        for w in targets(&mut c, kind) {
            w.pinned = on || w.ghost;
            n += 1;
        }
        n
    };
    after_change();
    n
}

/// Closes (or collapses) windows into the orb.
pub fn into_orb(kind: Option<SpatialWindowKind>, close: bool) -> usize {
    let orb = orb_center();
    let n = {
        let mut c = canvas();
        let mut n = 0;
        for w in targets(&mut c, kind).filter(|w| w.open() && !w.collapsed) {
            w.retract(orb, close);
            n += 1;
        }
        n
    };
    after_change();
    n
}

/// Closes every workspace window at once (no retract animation): egui closes the OS windows
/// on the next HUD frame because they are no longer registered. Returns how many were open.
pub fn close_all_now() -> usize {
    let n = {
        let mut c = canvas();
        let n = c.windows.iter().filter(|w| !w.hidden && !w.closing).count();
        c.windows.clear();
        c.inspect = None;
        n
    };
    after_change();
    n
}

/// Open (not collapsed, not closing) workspace windows.
#[cfg(test)]
pub fn open_count() -> usize {
    canvas().windows.iter().filter(|w| w.open() && !w.collapsed && !w.closing).count()
}

/// Puts content into the inspector (opening it when needed).
pub fn inspect(content: Inspector) {
    let open = {
        let mut c = canvas();
        c.inspect = Some(content);
        c.windows.iter().any(|w| w.kind == SpatialWindowKind::CodeAndSandboxInspector && w.open() && !w.collapsed)
    };
    if open {
        after_change();
    } else {
        spawn_windows(&[SpatialWindowKind::CodeAndSandboxInspector], None);
    }
}

// --------------------------------------------------------------------------- frames ----

/// Registers every workspace window with egui (each HUD frame); egui opens, patches and closes
/// the OS windows to match.
pub fn show(ctx: &egui::Context, app: &Jai, rt: &Handle) {
    let ppp = ctx.pixels_per_point().max(0.25);
    let specs: Vec<(u64, String, Pos2, Vec2, bool, bool)> = {
        let mut c = canvas();
        if c.ctx.is_none() {
            c.ctx = Some(ctx.clone());
        }
        // Research telemetry arrived: the crawler/dossier/graph windows repaint.
        let rev = research::engine().map(|e| e.revision()).unwrap_or(0);
        if rev != c.seen_research {
            c.seen_research = rev;
            for w in c.windows.iter().filter(|w| matches!(w.kind, SpatialWindowKind::LiveCrawlerStream | SpatialWindowKind::KnowledgeGraphPinboard)) {
                ctx.request_repaint_of(viewport_id(w.id));
            }
        }
        c.windows.iter().filter(|w| !w.hidden).map(|w| (w.id, w.title(), w.spawn_at, w.spawn_size, w.ghost, w.pinned || w.ghost)).collect()
    };
    for (id, title, at, size, ghost, on_top) in specs {
        let mut builder = ViewportBuilder::default()
            .with_title(title)
            .with_app_id("jai")
            .with_decorations(false)
            .with_transparent(true)
            .with_resizable(true)
            .with_active(false)
            .with_inner_size(size / ppp)
            .with_min_inner_size([240.0, 140.0])
            .with_position(at / ppp)
            .with_mouse_passthrough(ghost)
            .with_window_level(if on_top { egui::viewport::WindowLevel::AlwaysOnTop } else { egui::viewport::WindowLevel::Normal });
        if let Some(icon) = icon() {
            builder = builder.with_icon(icon);
        }
        let (app, rt) = (app.clone(), rt.clone());
        ctx.show_viewport_deferred(viewport_id(id), builder, move |ctx, class| {
            crate::crash_guard::guard("workspace window", || window_frame(ctx, class, &app, &rt, id));
        });
    }
}

fn window_frame(ctx: &egui::Context, class: ViewportClass, app: &Jai, rt: &Handle, id: u64) {
    let mut c = canvas();
    let Some(index) = c.windows.iter().position(|w| w.id == id) else {
        ctx.send_viewport_cmd(ViewportCommand::Visible(false));
        ctx.request_repaint_of(ViewportId::ROOT);
        return;
    };
    let mut w = c.windows.remove(index);
    let keep = frame_body(ctx, class, app, rt, &mut w, &mut c);
    // (Sized by hand at its edges and corners, as every window of his is.)
    if keep && class != ViewportClass::Embedded {
        crate::gui::resize_grips(ctx);
    }
    if keep {
        let at = index.min(c.windows.len());
        c.windows.insert(at, w);
    } else {
        ctx.request_repaint_of(ViewportId::ROOT);
    }
}

/// One frame of a workspace window; false once it has gone into the orb for good.
fn frame_body(ctx: &egui::Context, class: ViewportClass, app: &Jai, rt: &Handle, w: &mut SpatialWindow, c: &mut WorkspaceCanvas) -> bool {
    let now = Instant::now();
    let dt = w.last_step.map(|t| now.duration_since(t).as_secs_f32()).unwrap_or(1.0 / 60.0).min(0.1);
    w.last_step = Some(now);
    let ppp = ctx.pixels_per_point().max(0.25);
    let embedded = class == ViewportClass::Embedded;
    let (outer, inner) = ctx.input(|i| (i.viewport().outer_rect, i.viewport().inner_rect));
    if let Some(inner) = inner.filter(|_| !embedded && w.pending_size.is_none()) {
        let actual = inner.size() * ppp;
        if (actual - w.size).length() > 1.5 && w.drag.is_none() {
            w.size = actual;
        }
    }
    if ctx.input(|i| i.viewport().close_requested()) {
        ctx.send_viewport_cmd(ViewportCommand::CancelClose);
        w.retract(orb_center(), true);
    }
    if w.focus {
        w.focus = false;
        // (Never in a test session: Sir may be playing.)
        if !crate::platform::in_test_session() {
            ctx.send_viewport_cmd(ViewportCommand::Focus);
        }
    }
    if let Some(size) = w.pending_size.take() {
        ctx.send_viewport_cmd(ViewportCommand::InnerSize(size / ppp));
    }

    // Motion: Sir's drag with magnetic snapping, else the spring.
    let mut moving = false;
    if let Some((cursor0, window0)) = w.drag {
        match physics::cursor_pos().filter(|_| physics::left_button_down()) {
            Some(cursor) => {
                let rect = Rect::from_min_size(window0 + (cursor - cursor0), w.size);
                let others: Vec<Rect> = c.windows.iter().filter(|o| o.open()).map(SpatialWindow::rect).collect();
                let (snapped, guides) = physics::magnetic_snap(rect, &others, &physics::screen_rects(), physics::SNAP_THRESHOLD);
                if !guides.is_empty() {
                    w.guides = guides;
                    w.guides_until = Some(now + Duration::from_millis(650));
                }
                w.spring.hold(snapped.min);
                ctx.send_viewport_cmd(ViewportCommand::OuterPosition(snapped.min / ppp));
                moving = true;
            }
            None => w.drag = None,
        }
    } else if now >= w.launch_at && !embedded {
        let fading = w.fade.step(dt);
        if w.spring.step(dt) {
            ctx.send_viewport_cmd(ViewportCommand::OuterPosition(w.spring.current / ppp));
            moving = true;
            w.settled_since = None;
        } else {
            let since = *w.settled_since.get_or_insert(now);
            // Moved by the OS (Win+arrows, a display change): adopt where it really is.
            if let Some(outer) = outer.filter(|_| since.elapsed() > Duration::from_millis(600) && !w.closing && !w.collapsed) {
                let actual = outer.min * ppp;
                if (actual - w.spring.current).length() > 3.0 {
                    w.spring.hold(actual);
                }
            }
        }
        moving |= fading;
    }
    if (w.closing || w.collapsed) && w.spring.settled() && w.fade.current < 0.05 {
        ctx.send_viewport_cmd(ViewportCommand::Visible(false));
        if w.closing {
            return false;
        }
        w.hidden = true;
        ctx.request_repaint_of(ViewportId::ROOT);
        return true;
    }

    // Ghost mode on the native window (WS_EX_LAYERED | WS_EX_TRANSPARENT | topmost).
    if !embedded && w.applied_ghost != Some(w.ghost) && now >= w.launch_at {
        if w.hwnd.is_none() {
            w.hwnd = physics::find_window(&w.title());
        }
        if let Some(hwnd) = w.hwnd {
            match physics::set_window_click_through(hwnd, w.ghost) {
                Ok(()) => w.applied_ghost = Some(w.ghost),
                Err(e) => {
                    warn!("[WORKSPACE] Ghost mode: {e}");
                    w.applied_ghost = Some(w.ghost);
                }
            }
            if !w.ghost {
                let _ = physics::set_topmost(hwnd, w.pinned);
            }
        }
    }

    let accent = w.kind.accent();
    let fade = w.fade.current.clamp(0.0, 1.0);
    let base_alpha = if w.ghost { w.alpha.min(physics::GHOST_ALPHA) } else { w.alpha };
    let share = base_alpha as f32 / 255.0 * fade;
    let fill = holo::a(holo::theme().void(), share);
    let mut repaint_after = body_cadence(w);
    let logical = w.size / ppp;
    let (kind_title, window_id) = (w.kind.title(), w.id);
    let mut draw = |ui: &mut Ui| {
        ui.set_opacity(fade);
        let full = ui.max_rect();
        let painter = ui.painter().clone();
        crate::holo_frame::card(&painter, full.shrink(0.6), 0.6, share);
        header(ui, ctx, w, full, accent);
        let body = Rect::from_min_max(pos2(full.left() + 12.0, full.top() + HEADER_H + 8.0), pos2(full.right() - 12.0, full.bottom() - 12.0));
        ui.allocate_new_ui(UiBuilder::new().max_rect(body), |ui| {
            ui.set_clip_rect(body);
            match w.kind {
                SpatialWindowKind::LiveCrawlerStream => crawler_view(ui, w, c, app, rt, &mut repaint_after, fill),
                SpatialWindowKind::ExecutiveDossier => dossier_view(ui, w, c, ctx, fill),
                SpatialWindowKind::KnowledgeGraphPinboard => graph_view(ui, w, c, ctx, accent),
                SpatialWindowKind::CodeAndSandboxInspector => inspector_view(ui, c, accent, &mut repaint_after, fill),
                SpatialWindowKind::HardwareAndVramHud => hardware_view(ui, w, app, &mut repaint_after),
                SpatialWindowKind::ScheduleAndCommandDeck => schedule_view(ui, app, rt, fill),
            }
        });
        // Alignment guides of the last snap, fading out.
        if let Some(until) = w.guides_until {
            let left = until.saturating_duration_since(now).as_secs_f32() / 0.65;
            if left <= 0.0 {
                w.guides.clear();
                w.guides_until = None;
            } else {
                let origin = w.spring.current;
                for g in &w.guides {
                    let ink = Stroke::new(2.0_f32, holo::a(live_c(), 0.9 * left));
                    if g.vertical {
                        let x = ((g.at - origin.x) / ppp).clamp(full.left() + 1.0, full.right() - 1.0);
                        painter.line_segment([pos2(x, full.top() + 10.0), pos2(x, full.bottom() - 10.0)], ink);
                    } else {
                        let y = ((g.at - origin.y) / ppp).clamp(full.top() + 1.0, full.bottom() - 1.0);
                        painter.line_segment([pos2(full.left() + 10.0, y), pos2(full.right() - 10.0, y)], ink);
                    }
                }
                ctx.request_repaint();
            }
        }
        // Resize grip.
        let grip = Rect::from_min_size(full.right_bottom() - vec2(18.0, 18.0), vec2(16.0, 16.0));
        let g = ui.interact(grip, ui.id().with(("grip", w.id)), Sense::drag());
        for k in 0..3 {
            let o = 4.0 + k as f32 * 4.0;
            painter.line_segment([grip.right_bottom() - vec2(o, 2.0), grip.right_bottom() - vec2(2.0, o)], Stroke::new(1.0_f32, holo::a(if g.hovered() { accent } else { holo::theme().ink() }, if g.hovered() { 0.9 } else { 0.4 })));
        }
        if g.drag_started() {
            ctx.send_viewport_cmd(ViewportCommand::BeginResize(egui::ResizeDirection::SouthEast));
        }
    };
    if embedded {
        egui::Window::new(kind_title).id(egui::Id::new(("jai_spatial_embedded", window_id))).default_size(logical).title_bar(false).frame(Frame::none()).show(ctx, |ui| {
            ui.set_min_size(logical);
            draw(ui)
        });
    } else {
        egui::CentralPanel::default().frame(Frame::none()).show(ctx, |ui| draw(ui));
    }
    if moving || w.drag.is_some() || now < w.launch_at {
        ctx.request_repaint();
    } else {
        ctx.request_repaint_after(repaint_after);
    }
    true
}

/// How often a calm window refreshes.
fn body_cadence(w: &SpatialWindow) -> Duration {
    match w.kind {
        SpatialWindowKind::HardwareAndVramHud if w.ghost => Duration::from_millis(700),
        SpatialWindowKind::HardwareAndVramHud => Duration::from_millis(120),
        SpatialWindowKind::LiveCrawlerStream => Duration::from_millis(500),
        SpatialWindowKind::ScheduleAndCommandDeck => Duration::from_secs(5),
        _ => Duration::from_secs(2),
    }
}

#[derive(Clone, Copy)]
enum Icon {
    Ghost,
    Pin,
    Collapse,
    Close,
}

fn icon_button(ui: &mut Ui, rect: Rect, icon: Icon, active: bool, accent: Color32, id: egui::Id) -> egui::Response {
    let r = ui.interact(rect, id, Sense::click());
    let hot = r.hovered();
    let col = match (icon, hot, active) {
        (Icon::Close, true, _) => contradiction(),
        (_, _, true) => accent,
        (_, true, _) => text_c(),
        _ => muted_c(),
    };
    let p = ui.painter();
    if hot || active {
        p.rect_filled(rect.shrink(1.0), Rounding::ZERO, holo::a(col, 0.14));
    }
    let c = rect.center();
    let s = Stroke::new(1.4_f32, col);
    match icon {
        Icon::Ghost => {
            p.circle_filled(c + vec2(0.0, -1.5), 4.6, col);
            p.rect_filled(Rect::from_center_size(c + vec2(0.0, 1.6), vec2(9.2, 5.0)), 0.0, col);
            for dx in [-3.0, 0.0, 3.0] {
                p.circle_filled(c + vec2(dx, 4.2), 1.55, col);
            }
            let eye = holo::theme().void();
            p.circle_filled(c + vec2(-1.7, -1.6), 0.95, eye);
            p.circle_filled(c + vec2(1.7, -1.6), 0.95, eye);
        }
        Icon::Pin => {
            p.circle_filled(c + vec2(0.0, -3.0), 3.0, col);
            p.line_segment([c + vec2(0.0, -0.5), c + vec2(0.0, 5.5)], s);
            p.line_segment([c + vec2(-3.5, 0.2), c + vec2(3.5, 0.2)], s);
        }
        Icon::Collapse => {
            p.line_segment([c + vec2(-4.5, 1.0), c + vec2(4.5, 1.0)], s);
        }
        Icon::Close => {
            p.line_segment([c + vec2(-4.0, -4.0), c + vec2(4.0, 4.0)], s);
            p.line_segment([c + vec2(4.0, -4.0), c + vec2(-4.0, 4.0)], s);
        }
    }
    r
}

/// `[ TITLE // TAG ]` on the left (drags the window), ghost / pin / collapse / close right.
fn header(ui: &mut Ui, ctx: &egui::Context, w: &mut SpatialWindow, full: Rect, accent: Color32) {
    let bar = Rect::from_min_max(full.min + vec2(10.0, 3.0), pos2(full.right() - 10.0, full.top() + HEADER_H));
    let painter = ui.painter().clone();
    let t = ui.input(|i| i.time) as f32;
    let pulse = 0.55 + 0.45 * (t * 2.4).sin();
    painter.circle_filled(pos2(bar.left() + 8.0, bar.center().y), 3.4, accent.gamma_multiply(pulse));
    painter.circle_stroke(pos2(bar.left() + 8.0, bar.center().y), 6.0, Stroke::new(1.0_f32, holo::a(holo::theme().ink(), 0.25)));
    let title = w.label.clone().unwrap_or_else(|| w.kind.title().to_string());
    let title_w = holo::text(&painter, pos2(bar.left() + 20.0, bar.center().y), Align2::LEFT_CENTER, Face::Label, &title, 10.0, accent).width();
    holo::text(&painter, pos2(bar.left() + 28.0 + title_w, bar.center().y), Align2::LEFT_CENTER, Face::Mono, &format!("// {}{}", w.kind.tag(), if w.ghost { " · GHOST" } else { "" }), 10.0, muted_c());
    painter.line_segment([pos2(full.left() + 14.0, full.top() + HEADER_H + 3.0), pos2(full.right() - 14.0, full.top() + HEADER_H + 3.0)], Stroke::new(1.0_f32, holo::a(holo::theme().ink(), 0.2)));

    let size = vec2(24.0, 22.0);
    let mut x = bar.right() - size.x;
    let mut slot = || {
        let r = Rect::from_min_size(pos2(x, bar.center().y - size.y / 2.0), size);
        x -= size.x + 2.0;
        r
    };
    let (close, collapse, pin, ghost) = (slot(), slot(), slot(), slot());
    let base = ui.id();
    if icon_button(ui, close, Icon::Close, false, accent, base.with(("close", w.id))).on_hover_text("Close (back into the orb)").clicked() {
        w.retract(orb_center(), true);
    }
    if icon_button(ui, collapse, Icon::Collapse, false, accent, base.with(("collapse", w.id))).on_hover_text("Collapse into the orb").clicked() {
        w.retract(orb_center(), false);
    }
    if icon_button(ui, pin, Icon::Pin, w.pinned, accent, base.with(("pin", w.id))).on_hover_text("Always on top").clicked() {
        w.pinned = !w.pinned;
        ctx.request_repaint_of(ViewportId::ROOT);
    }
    if icon_button(ui, ghost, Icon::Ghost, w.ghost, accent, base.with(("ghost", w.id)))
        .on_hover_text("Ghost mode: clicks pass through to the game or IDE behind. Undo with \"disable ghost mode\" or UN-GHOST ALL in the HUD.")
        .clicked()
    {
        w.ghost = true;
        w.pinned = true;
        ctx.request_repaint_of(ViewportId::ROOT);
    }
    let drag_zone = Rect::from_min_max(bar.min, pos2(x + size.x - 4.0, bar.max.y));
    let drag = ui.interact(drag_zone, ui.id().with(("drag", w.id)), Sense::click_and_drag());
    if drag.drag_started_by(egui::PointerButton::Primary) {
        match physics::cursor_pos() {
            Some(cursor) => w.drag = Some((cursor, w.spring.current)),
            None => ctx.send_viewport_cmd(ViewportCommand::StartDrag),
        }
    }
    if drag.double_clicked() {
        w.retract(orb_center(), false);
    }
}

// ------------------------------------------------------------------------ the views ----

fn muted(ui: &mut Ui, text: &str) {
    ui.add(egui::Label::new(body(text, 12.0, muted_c())).wrap());
}

fn stat(ui: &mut Ui, caption: &str, value: String, colour: Color32) {
    ui.vertical(|ui| {
        ui.label(label(caption, muted_c()));
        ui.label(mono(value, 12.0, colour));
    });
}

fn bar(ui: &mut Ui, caption: &str, value: &str, frac: f32, colour: Color32) {
    ui.horizontal(|ui| {
        ui.add_sized([62.0, 14.0], egui::Label::new(label(caption, muted_c())));
        let width = (ui.available_width() - 96.0).max(40.0);
        let (rect, _) = ui.allocate_exact_size(vec2(width, 9.0), Sense::hover());
        let p = ui.painter();
        p.rect_filled(rect, Rounding::ZERO, holo::a(holo::theme().ink(), 0.07));
        let filled = Rect::from_min_size(rect.min, vec2(rect.width() * frac.clamp(0.0, 1.0), rect.height()));
        p.rect_filled(filled, Rounding::ZERO, colour);
        ui.label(mono(value, 10.5, text_c()));
    });
}

fn pill(ui: &mut Ui, text: &str, colour: Color32) -> egui::Response {
    Frame::none()
        .fill(holo::a(colour, 0.12))
        .stroke(Stroke::new(1.0_f32, holo::a(colour, 0.55)))
        .rounding(Rounding::ZERO)
        .inner_margin(Margin::symmetric(8.0, 2.0))
        .show(ui, |ui| ui.label(label(text, colour)))
        .response
        .interact(Sense::click())
}

fn mission_for(w: &SpatialWindow) -> Option<i64> {
    w.mission_id.or_else(research::latest_mission_id)
}

fn crawler_view(ui: &mut Ui, w: &mut SpatialWindow, c: &mut WorkspaceCanvas, app: &Jai, rt: &Handle, repaint: &mut Duration, fill: Color32) {
    let Some(engine) = research::engine() else {
        muted(ui, "The research engine is not running.");
        return;
    };
    let Some(id) = mission_for(w) else {
        muted(ui, "No research mission yet — say \"JAI, research solid-state batteries for two hours\".");
        return;
    };
    let snap = c.snapshot(id);
    let live = engine.live(id);
    let running = engine.is_running(id);
    if running {
        *repaint = Duration::from_millis(250);
    }
    if let Some(s) = &snap {
        ui.horizontal(|ui| {
            ui.label(head(&s.mission.topic, 14.0, live_c()));
            pill(ui, &s.mission.status, if running { live_c() } else { muted_c() });
            if let Some(tick) = engine.last_progress(id).filter(|_| running) {
                ui.label(mono(format!("LIVE {}", tick.at), 10.5, live_c()));
            }
        });
    }
    let (elapsed, target) = live.as_ref().map(|l| (l.elapsed_secs, l.target_secs)).or_else(|| snap.as_ref().map(|s| (s.mission.elapsed_secs as u64, s.mission.target_secs as u64))).unwrap_or((0, 1));
    let clock = |s: u64| format!("{:02}:{:02}:{:02}", s / 3600, (s / 60) % 60, s % 60);
    bar(ui, "MISSION", &format!("{} / {}", clock(elapsed), clock(target)), elapsed as f32 / target.max(1) as f32, live_c());
    if let Some(l) = &live {
        ui.horizontal_wrapped(|ui| {
            stat(ui, "PAGES", l.pages.to_string(), text_c());
            stat(ui, "FACTS", l.facts.to_string(), verified());
            stat(ui, "PAGES/MIN", format!("{:.0}", l.pages_per_min()), live_c());
            stat(ui, "FRONTIER", l.frontier.to_string(), text_c());
            stat(ui, "QUERIES", l.queries_left.to_string(), text_c());
            stat(ui, "STAGE", l.stage.to_uppercase(), live_c());
        });
    }
    ui.horizontal(|ui| {
        // On the runtime: a resumed mission spawns its tasks there (this is the UI thread).
        let action = |name: &str| {
            let tools = app.tools().clone();
            let args = json!({ "action": name, "mission_id": id });
            rt.spawn(async move {
                if let Err(e) = research::run_tool(&tools, &args) {
                    warn!("[WORKSPACE] {e}");
                }
            });
        };
        let paused = snap.as_ref().is_some_and(|s| s.mission.status == "paused");
        if running && !paused && crate::holo_ui::key(ui, "Pause").clicked() {
            action("pause");
        }
        if (paused || (!running && snap.as_ref().is_some_and(|s| s.mission.status != "completed"))) && crate::holo_ui::key(ui, "Resume").clicked() {
            action("resume");
        }
        if running && crate::holo_ui::key(ui, "Stop").clicked() {
            action("stop");
        }
    });
    ui.add_space(4.0);
    crate::holo_block::faded_into(ui, egui::ScrollArea::vertical().id_salt(("crawl_feed", w.id)).stick_to_bottom(true).auto_shrink([false; 2]), fill, |ui| {
        for e in engine.recent_events(Some(id), 180) {
            let (tag, colour, text) = match &e.kind {
                TelemetryKind::Fetch { url, status, ms, bytes } => (
                    format!("GET {status}"),
                    if *status < 300 { verified() } else if *status < 400 { single() } else { contradiction() },
                    format!("{ms:>5} ms {:>6.1} KB  {url}", *bytes as f32 / 1024.0),
                ),
                TelemetryKind::Query { provider, query, results } => ("QUERY".into(), single(), format!("{provider} \"{query}\" → {results}")),
                TelemetryKind::Snippet { text, .. } => ("SNIP".into(), muted_c(), text.clone()),
                TelemetryKind::Fact { category, claim, corroborated, contradiction, .. } => (
                    format!("FACT {}", category.to_uppercase()),
                    if *contradiction { crate::research_graph_view::contradiction() } else if *corroborated { verified() } else { single() },
                    claim.clone(),
                ),
                TelemetryKind::Stage { stage, detail } => (stage.to_uppercase(), live_c(), detail.clone()),
                TelemetryKind::Error { url, message } => ("ERR".into(), contradiction(), format!("{message} — {url}")),
                TelemetryKind::Finished { status, summary } => (status.to_uppercase(), live_c(), summary.clone()),
                TelemetryKind::Progress { .. } => continue,
            };
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                ui.label(mono(&e.at, 10.5, muted_c()));
                ui.label(mono(tag, 10.5, colour));
                ui.add(egui::Label::new(mono(text, 10.5, text_c())).wrap());
            });
        }
    });
}

fn fact_colour(f: &FactRow) -> Color32 {
    if f.contradicting_source.is_some() {
        contradiction()
    } else if f.corroborating > 0 {
        verified()
    } else {
        single()
    }
}

/// A clickable fact line; highlighted when selected in the graph.
fn fact_row(ui: &mut Ui, f: &FactRow, selected: bool, related: bool) -> egui::Response {
    let fill = if selected {
        holo::a(live_c(), 0.18)
    } else if related {
        holo::a(holo::theme().ink(), 0.06)
    } else {
        holo::a(holo::theme().void(), 0.0)
    };
    let inner = Frame::none().fill(fill).rounding(Rounding::ZERO).inner_margin(Margin::symmetric(6.0, 4.0)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal_wrapped(|ui| {
            ui.label(mono("-", 11.0, fact_colour(f)));
            ui.add(egui::Label::new(body(&f.claim, 12.0, text_c())).wrap());
            ui.label(mono(format!("[S{}]", f.source_id), 10.5, live_c()));
            if f.corroborating > 0 {
                ui.label(mono(format!("OK {}×", f.corroborating + 1), 10.5, verified()));
            }
            if f.contradicting_source.is_some() {
                ui.label(mono("WARN: disputed", 10.5, contradiction()));
            }
        });
    });
    inner.response.interact(Sense::click())
}

fn dossier_view(ui: &mut Ui, w: &mut SpatialWindow, c: &mut WorkspaceCanvas, ctx: &egui::Context, fill: Color32) {
    let Some(id) = mission_for(w) else {
        muted(ui, "No dossier yet — start a deep research mission and it writes itself here, chapter by chapter.");
        return;
    };
    let Some(snap) = c.snapshot(id) else {
        let room = ui.available_rect_before_wrap();
        crate::holo_block::outline(ui, room.shrink(6.0), crate::holo_block::Outline::Lines);
        return;
    };
    let scroll_to_selection = w.seen_selection != c.selection_revision;
    w.seen_selection = c.selection_revision;
    let mut clicked: Option<(i64, i64)> = None;
    let m = &snap.mission;
    let brief = c.briefs.get(&id).cloned();
    crate::holo_block::faded_into(ui, egui::ScrollArea::vertical().id_salt(("dossier", w.id)).auto_shrink([false; 2]), fill, |ui| {
        // A topic workspace: what JAI knows, before the research chapters.
        if let Some(brief) = &brief {
            crate::holo_ui::fold(ui, (w.id, "brief"), "Overview & facts", true, |ui| {
                markdown_lite(ui, brief, live_c());
            });
            ui.add_space(6.0);
        }
        ui.label(head(format!("Stark Research Dossier — {}", m.topic), 16.0, live_c()));
        ui.label(mono(format!("Mission #{} · started {} · {} · {} of {} min · {} pages · {} facts · {} sources", m.id, m.created_at, m.status, m.elapsed_secs / 60, m.target_secs / 60, m.pages, m.facts, snap.sources.len()), 10.5, muted_c()));
        ui.add_space(6.0);
        ui.label(label("EXECUTIVE SUMMARY", holo::theme().deep()));
        if m.executive_summary.trim().is_empty() {
            let mut top: Vec<&FactRow> = snap.facts.iter().filter(|f| f.contradicting_source.is_none()).collect();
            top.sort_by(|a, b| b.corroborating.cmp(&a.corroborating).then(b.confidence.partial_cmp(&a.confidence).unwrap_or(std::cmp::Ordering::Equal)));
            if top.is_empty() {
                muted(ui, "The crawler is still gathering material.");
            }
            for f in top.into_iter().take(5) {
                if fact_row(ui, f, c.selected_fact == Some(f.id), c.selected_source == Some(f.source_id)).clicked() {
                    clicked = Some((f.id, f.source_id));
                }
            }
        } else {
            ui.add(egui::Label::new(body(&m.executive_summary, 12.5, text_c())).wrap());
        }
        let chapters: [(&str, &[&str]); 6] = [
            ("Chapter 1 — Overview & Definitions", &["definition"]),
            ("Chapter 2 — Architecture & Design", &["architecture"]),
            ("Chapter 3 — Benchmarks & Numbers", &["benchmark", "statistic"]),
            ("Chapter 4 — History & Timeline", &["history"]),
            ("Chapter 5 — Limitations, Risks & Criticism", &["limitation"]),
            ("Chapter 6 — Further Claims", &["claim"]),
        ];
        for (title, cats) in chapters {
            let facts: Vec<&FactRow> = snap.facts.iter().filter(|f| cats.contains(&f.category.as_str())).take(30).collect();
            if facts.is_empty() {
                continue;
            }
            ui.add_space(4.0);
            crate::holo_ui::fold(ui, (w.id, title), &format!("{title}  ({})", facts.len()), true, |ui| {
                for f in facts {
                    let selected = c.selected_fact == Some(f.id);
                    let r = fact_row(ui, f, selected, c.selected_source == Some(f.source_id));
                    if selected && scroll_to_selection {
                        r.scroll_to_me(Some(Align::Center));
                    }
                    if r.clicked() {
                        clicked = Some((f.id, f.source_id));
                    }
                }
            });
        }
        let conflicts: Vec<&FactRow> = snap.facts.iter().filter(|f| f.contradicting_source.is_some()).take(20).collect();
        if !conflicts.is_empty() {
            crate::holo_ui::fold(ui, (w.id, "conflicts"), &format!("Chapter 7 — Contradictions  ({})", conflicts.len()), true, |ui| {
                egui::Grid::new(("conflict_grid", w.id)).striped(true).num_columns(3).show(ui, |ui| {
                    for h in ["CLAIM", "SOURCE", "DISPUTED BY"] {
                        ui.label(label(h, muted_c()));
                    }
                    ui.end_row();
                    for f in conflicts {
                        ui.add(egui::Label::new(body(&f.claim, 11.5, text_c())).wrap());
                        ui.label(mono(snap.source(f.source_id).map(|s| s.domain.clone()).unwrap_or_default(), 10.5, live_c()));
                        ui.label(mono(f.contradicting_source.and_then(|s| snap.source(s)).map(|s| s.domain.clone()).unwrap_or_default(), 10.5, contradiction()));
                        ui.end_row();
                    }
                });
            });
        }
        crate::holo_ui::fold(ui, (w.id, "sources"), &format!("Sources  ({})", snap.sources.len()), false, |ui| {
            egui::Grid::new(("source_grid", w.id)).striped(true).num_columns(3).show(ui, |ui| {
                for s in snap.sources.iter().take(80) {
                    let selected = c.selected_source == Some(s.id);
                    ui.label(mono(format!("S{}", s.id), 10.5, if selected { live_c() } else { muted_c() }));
                    crate::holo_ui::link_to(ui, body(s.title.chars().take(70).collect::<String>(), 11.5, text_c()), &s.url).on_hover_text(format!("{} · crawled {} · link depth {}", s.domain, s.crawled_at, s.depth));
                    let (rect, _) = ui.allocate_exact_size(vec2(60.0, 8.0), Sense::hover());
                    ui.painter().rect_filled(rect, Rounding::ZERO, holo::a(holo::theme().ink(), 0.07));
                    ui.painter().rect_filled(Rect::from_min_size(rect.min, vec2(60.0 * s.reliability, 8.0)), Rounding::ZERO, holo::a(verified(), 0.4 + 0.6 * s.reliability));
                    ui.end_row();
                }
            });
        });
    });
    if let Some((fact, source)) = clicked {
        pick(c, ctx, GraphPick::Fact { fact_id: fact, source_id: source }, &snap);
    }
}

/// A fact or source was picked (graph or dossier): highlight it everywhere and put the raw
/// quote, the source and its stored page into the inspector.
fn pick(c: &mut WorkspaceCanvas, ctx: &egui::Context, p: GraphPick, snap: &MissionSnapshot) {
    match p {
        GraphPick::Fact { fact_id, source_id } => {
            c.select(Some(fact_id), Some(source_id));
            let fact = snap.facts.iter().find(|f| f.id == fact_id);
            let source = snap.source(source_id);
            let page = research::source_markdown(source_id).map(|(_, md)| md).unwrap_or_default();
            c.inspect = Some(Inspector {
                title: format!("Fact #{fact_id}"),
                body: format!(
                    "> {}\n\n**Category:** {} · **Confidence:** {:.0}% · **Corroborated by:** {} more source(s){}\n\n**Source S{source_id}:** {}\n{}\n**Reliability:** {:.2}\n\n---\n\n{}",
                    fact.map(|f| f.claim.as_str()).unwrap_or(""),
                    fact.map(|f| f.category.as_str()).unwrap_or(""),
                    fact.map(|f| f.confidence * 100.0).unwrap_or(0.0),
                    fact.map(|f| f.corroborating).unwrap_or(0),
                    if fact.is_some_and(|f| f.contradicting_source.is_some()) { " · **disputed**" } else { "" },
                    source.map(|s| s.title.as_str()).unwrap_or(""),
                    source.map(|s| s.url.as_str()).unwrap_or(""),
                    source.map(|s| s.reliability).unwrap_or(0.0),
                    page.chars().take(20_000).collect::<String>()
                ),
                format: InspectFormat::Markdown,
            });
        }
        GraphPick::Source(source_id) => {
            c.select(None, Some(source_id));
            if let Some((url, md)) = research::source_markdown(source_id) {
                c.inspect = Some(Inspector { title: format!("S{source_id} · {url}"), body: md.chars().take(40_000).collect(), format: InspectFormat::Markdown });
            }
        }
        GraphPick::Question(_) | GraphPick::Hub => c.select(None, None),
    }
    for w in &c.windows {
        ctx.request_repaint_of(viewport_id(w.id));
    }
}

fn graph_view(ui: &mut Ui, w: &mut SpatialWindow, c: &mut WorkspaceCanvas, ctx: &egui::Context, accent: Color32) {
    let Some(id) = mission_for(w) else {
        muted(ui, "The pinboard fills itself as a deep research mission discovers sources and facts.");
        return;
    };
    let Some(snap) = c.snapshot(id) else {
        let room = ui.available_rect_before_wrap();
        crate::holo_block::outline(ui, room.shrink(6.0), crate::holo_block::Outline::Lines);
        return;
    };
    let graph = w.graph.get_or_insert_with(ResearchGraphView::new);
    graph.sync(&snap);
    if w.seen_selection != c.selection_revision {
        w.seen_selection = c.selection_revision;
        if let Some(f) = c.selected_fact {
            graph.select_fact(f);
        }
    }
    if let Some(p) = graph.show(ui, accent) {
        pick(c, ctx, p, &snap);
        w.seen_selection = c.selection_revision;
    }
}

static IDENT_X: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\bx\b").unwrap());
static IDENT_T: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\bt\b").unwrap());

fn inspector_view(ui: &mut Ui, c: &mut WorkspaceCanvas, accent: Color32, repaint: &mut Duration, fill: Color32) {
    let Some(item) = c.inspect.clone() else {
        muted(ui, "Ask JAI to inspect code, JSON, CSV or a function to plot — or click a fact or source in the knowledge graph.");
        return;
    };
    ui.horizontal(|ui| {
        ui.label(head(&item.title, 12.5, accent));
        pill(ui, &format!("{:?}", item.format), muted_c());
        if crate::holo_ui::key(ui, "Copy").clicked() {
            ui.ctx().copy_text(item.body.clone());
        }
    });
    ui.add_space(4.0);
    match item.format {
        InspectFormat::Plot => plot_view(ui, &item.body, accent, repaint),
        InspectFormat::Csv => csv_view(ui, &item.body, fill),
        _ => {
            crate::holo_block::faded_into(ui, egui::ScrollArea::both().id_salt("inspector_body").auto_shrink([false; 2]), fill, |ui| match item.format {
                InspectFormat::Code => {
                    ui.label(highlighted(&item.body, 11.5));
                }
                InspectFormat::Json => {
                    let pretty = serde_json::from_str::<Value>(&item.body).ok().and_then(|v| serde_json::to_string_pretty(&v).ok()).unwrap_or_else(|| item.body.clone());
                    ui.label(highlighted(&pretty, 11.5));
                }
                InspectFormat::Markdown => markdown_lite(ui, &item.body, accent),
                _ => {
                    ui.add(egui::Label::new(mono(&item.body, 11.0, text_c())).wrap());
                }
            });
        }
    }
}

const KEYWORDS: [&str; 40] = [
    "fn", "let", "mut", "pub", "struct", "enum", "impl", "use", "mod", "if", "else", "for", "while", "loop", "return", "match", "def", "class", "import",
    "from", "const", "var", "function", "async", "await", "true", "false", "None", "null", "self", "Self", "in", "where", "type", "static", "new",
    "try", "except", "break", "continue",
];

/// Lightweight syntax colouring: keywords, strings, numbers and comments.
fn highlighted(code: &str, size: f32) -> egui::text::LayoutJob {
    let mut job = egui::text::LayoutJob::default();
    let t = holo::theme();
    let mut push = |text: &str, colour: Color32| {
        job.append(text, 0.0, TextFormat { font_id: holo::font(Face::Mono, size), color: colour, ..Default::default() });
    };
    for line in code.lines().take(4000) {
        let chars: Vec<char> = line.chars().collect();
        let mut i = 0;
        while i < chars.len() {
            let ch = chars[i];
            let rest: String = chars[i..].iter().collect();
            if rest.starts_with("//") || (ch == '#' && chars[..i].iter().all(|c| c.is_whitespace())) {
                push(&rest, t.faint());
                break;
            }
            if ch == '"' || ch == '\'' {
                let mut j = i + 1;
                while j < chars.len() && chars[j] != ch {
                    if chars[j] == '\\' {
                        j += 1;
                    }
                    j += 1;
                }
                let end = (j + 1).min(chars.len());
                push(&chars[i..end].iter().collect::<String>(), t.deep());
                i = end;
                continue;
            }
            if ch.is_ascii_digit() {
                let mut j = i;
                while j < chars.len() && (chars[j].is_ascii_alphanumeric() || chars[j] == '.' || chars[j] == '_') {
                    j += 1;
                }
                push(&chars[i..j].iter().collect::<String>(), t.accent());
                i = j;
                continue;
            }
            if ch.is_alphanumeric() || ch == '_' {
                let mut j = i;
                while j < chars.len() && (chars[j].is_alphanumeric() || chars[j] == '_') {
                    j += 1;
                }
                let word: String = chars[i..j].iter().collect();
                push(&word, if KEYWORDS.contains(&word.as_str()) { t.hot() } else { t.ink() });
                i = j;
                continue;
            }
            push(&ch.to_string(), t.muted());
            i += 1;
        }
        push("\n", t.ink());
    }
    job
}

fn markdown_lite(ui: &mut Ui, text: &str, accent: Color32) {
    let mut code: Option<String> = None;
    for line in text.lines().take(3000) {
        if line.trim_start().starts_with("```") {
            match code.take() {
                Some(block) => {
                    Frame::none().fill(holo::a(holo::theme().ink(), 0.05)).rounding(Rounding::ZERO).inner_margin(Margin::same(6.0)).show(ui, |ui| {
                        ui.label(highlighted(&block, 11.0));
                    });
                }
                None => code = Some(String::new()),
            }
            continue;
        }
        if let Some(block) = code.as_mut() {
            block.push_str(line);
            block.push('\n');
            continue;
        }
        let t = line.trim();
        if t.is_empty() {
            ui.add_space(4.0);
        } else if let Some(h) = t.strip_prefix("# ") {
            ui.label(head(h, 16.0, accent));
        } else if let Some(h) = t.strip_prefix("## ") {
            ui.label(head(h, 14.0, accent));
        } else if let Some(h) = t.strip_prefix("### ").or_else(|| t.strip_prefix("#### ")) {
            ui.label(head(h, 12.5, holo::theme().deep()));
        } else if let Some(item) = t.strip_prefix("- ").or_else(|| t.strip_prefix("* ")) {
            ui.horizontal_wrapped(|ui| {
                ui.label(body("•", 12.0, accent));
                ui.add(egui::Label::new(body(item.replace("**", ""), 12.0, text_c())).wrap());
            });
        } else if let Some(q) = t.strip_prefix("> ") {
            ui.add(egui::Label::new(body(q, 12.5, muted_c()).italics()).wrap());
        } else if t.starts_with('|') {
            if !t.contains("---") {
                ui.label(mono(t, 11.0, text_c()));
            }
        } else if t == "---" {
            crate::holo_ui::rule(ui);
        } else {
            ui.add(egui::Label::new(body(t.replace("**", ""), 12.0, text_c())).wrap());
        }
    }
    if let Some(block) = code {
        ui.label(highlighted(&block, 11.0));
    }
}

/// Splits one CSV line (quotes respected).
fn csv_fields(line: &str, delim: char) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    let mut chars = line.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '"' if quoted && chars.peek() == Some(&'"') => {
                cur.push('"');
                chars.next();
            }
            '"' => quoted = !quoted,
            c if c == delim && !quoted => out.push(std::mem::take(&mut cur).trim().to_string()),
            c => cur.push(c),
        }
    }
    out.push(cur.trim().to_string());
    out
}

fn csv_delimiter(body: &str) -> char {
    let first = body.lines().next().unwrap_or("");
    [',', ';', '\t', '|'].into_iter().max_by_key(|d| first.matches(*d).count()).unwrap_or(',')
}

fn csv_view(ui: &mut Ui, body: &str, fill: Color32) {
    let delim = csv_delimiter(body);
    let rows: Vec<Vec<String>> = body.lines().filter(|l| !l.trim().is_empty()).take(500).map(|l| csv_fields(l, delim)).collect();
    let width = rows.iter().map(Vec::len).max().unwrap_or(0);
    crate::holo_block::faded_into(ui, egui::ScrollArea::both().id_salt("inspector_csv").auto_shrink([false; 2]), fill, |ui| {
        egui::Grid::new("inspector_csv_grid").striped(true).num_columns(width).show(ui, |ui| {
            for (i, row) in rows.iter().enumerate() {
                for cell in row {
                    ui.label(if i == 0 { label(cell, holo::theme().deep()) } else { mono(cell, 11.0, text_c()) });
                }
                ui.end_row();
            }
        });
    });
}

/// Plots numeric data (one or two columns) or a function of x (and t: animated).
fn plot_view(ui: &mut Ui, body: &str, accent: Color32, repaint: &mut Duration) {
    let numbers: Vec<Vec<f64>> = body
        .lines()
        .map(|l| l.split([',', ';', '\t', ' ']).filter(|s| !s.is_empty()).filter_map(|s| s.trim().parse::<f64>().ok()).collect::<Vec<_>>())
        .filter(|v| !v.is_empty())
        .collect();
    let time = ui.input(|i| i.time);
    let expr = body.trim().trim_start_matches("y=").trim_start_matches("y =").trim().to_string();
    let live = IDENT_T.is_match(&expr);
    let points: Vec<(f64, f64)> = if numbers.len() >= 2 {
        numbers.iter().enumerate().map(|(i, v)| if v.len() >= 2 { (v[0], v[1]) } else { (i as f64, v[0]) }).collect()
    } else {
        let with_t = IDENT_T.replace_all(&expr, format!("({time:.4})").as_str()).to_string();
        (0..=240)
            .filter_map(|i| {
                let x = -10.0 + 20.0 * i as f64 / 240.0;
                let e = IDENT_X.replace_all(&with_t, format!("({x:.6})").as_str()).to_string();
                crate::calc::evaluate(&e).ok().filter(|y| y.is_finite()).map(|y| (x, y))
            })
            .collect()
    };
    if live {
        *repaint = Duration::from_millis(33);
    }
    let rect = ui.available_rect_before_wrap();
    ui.allocate_rect(rect, Sense::hover());
    let p = ui.painter_at(rect);
    if points.len() < 2 {
        holo::text(&p, rect.center(), Align2::CENTER_CENTER, Face::Body, "Nothing to plot (give numbers, or a function of x such as sin(x)*x).", 12.0, muted_c());
        return;
    }
    let (mut x0, mut x1, mut y0, mut y1) = (f64::MAX, f64::MIN, f64::MAX, f64::MIN);
    for &(x, y) in &points {
        x0 = x0.min(x);
        x1 = x1.max(x);
        y0 = y0.min(y);
        y1 = y1.max(y);
    }
    if (y1 - y0).abs() < 1e-9 {
        y0 -= 1.0;
        y1 += 1.0;
    }
    let inner = rect.shrink2(vec2(36.0, 16.0));
    let to = |x: f64, y: f64| pos2(inner.left() + ((x - x0) / (x1 - x0).max(1e-9)) as f32 * inner.width(), inner.bottom() - ((y - y0) / (y1 - y0)) as f32 * inner.height());
    let ink = holo::theme().ink();
    holo::outline(&p, &holo::plate(inner, [0.0; 4]), holo::a(ink, 0.2));
    if y0 < 0.0 && y1 > 0.0 {
        holo::rule(&p, to(x0, 0.0), to(x1, 0.0), holo::a(ink, 0.35));
    }
    if x0 < 0.0 && x1 > 0.0 {
        holo::rule(&p, to(0.0, y0), to(0.0, y1), holo::a(ink, 0.35));
    }
    let line: Vec<Pos2> = points.iter().map(|&(x, y)| to(x, y)).collect();
    p.add(egui::Shape::line(line.clone(), Stroke::new(4.0_f32, holo::a(holo::theme().deep(), 0.15))));
    p.add(egui::Shape::line(line, Stroke::new(1.6_f32, accent)));
    let fmt = |v: f64| crate::calc::format_number(v);
    let m = muted_c();
    holo::text(&p, inner.left_top() + vec2(-4.0, 0.0), Align2::RIGHT_TOP, Face::Mono, &fmt(y1), 10.5, m);
    holo::text(&p, inner.left_bottom() + vec2(-4.0, 0.0), Align2::RIGHT_BOTTOM, Face::Mono, &fmt(y0), 10.5, m);
    holo::text(&p, inner.left_bottom() + vec2(0.0, 3.0), Align2::LEFT_TOP, Face::Mono, &fmt(x0), 10.5, m);
    holo::text(&p, inner.right_bottom() + vec2(0.0, 3.0), Align2::RIGHT_TOP, Face::Mono, &fmt(x1), 10.5, m);
}

/// Ping and the busiest processes, sampled off the UI thread while a hardware HUD is open.
#[derive(Default)]
struct HwData {
    ping: VecDeque<Option<f32>>,
    processes: Vec<(String, f32)>,
    wanted: Option<Instant>,
}

static HW: LazyLock<Mutex<HwData>> = LazyLock::new(|| Mutex::new(HwData::default()));

fn hw() -> MutexGuard<'static, HwData> {
    HW.lock().unwrap_or_else(|e| e.into_inner())
}

fn want_hardware() {
    static SAMPLER: OnceLock<()> = OnceLock::new();
    hw().wanted = Some(Instant::now());
    SAMPLER.get_or_init(|| {
        let spawned = std::thread::Builder::new().name("jai-hud-sampler".into()).spawn(|| {
            use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System};
            let mut sys = System::new();
            let target: std::net::SocketAddr = ([1, 1, 1, 1], 443).into();
            loop {
                std::thread::sleep(Duration::from_secs(2));
                if hw().wanted.is_none_or(|t| t.elapsed() > Duration::from_secs(15)) {
                    continue;
                }
                let t0 = Instant::now();
                let ping = std::net::TcpStream::connect_timeout(&target, Duration::from_millis(1500)).ok().map(|_| t0.elapsed().as_secs_f32() * 1000.0);
                sys.refresh_processes_specifics(ProcessesToUpdate::All, ProcessRefreshKind::new().with_memory());
                let mut procs: Vec<(String, f32)> = sys.processes().values().map(|p| (p.name().to_string_lossy().into_owned(), p.memory() as f32 / 1_048_576.0)).collect();
                procs.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
                procs.truncate(6);
                let mut d = hw();
                d.ping.push_back(ping);
                while d.ping.len() > 40 {
                    d.ping.pop_front();
                }
                d.processes = procs;
            }
        });
        if let Err(e) = spawned {
            warn!("[WORKSPACE] No hardware sampler: {e}");
        }
    });
}

fn hardware_view(ui: &mut Ui, w: &SpatialWindow, app: &Jai, repaint: &mut Duration) {
    want_hardware();
    let t = app.tools().monitor.latest();
    let (badge, detail, dormant) = crate::vram_guard::badge();
    let compact = ui.available_height() < 260.0 || ui.available_width() < 330.0;
    let gpu = t.gpu.clone();
    let (used, total) = gpu.as_ref().map(|g| (g.vram_used_mb.unwrap_or(0), g.vram_total_mb.unwrap_or(0))).unwrap_or((0, 0));
    let ollama_mb = crate::vram_guard::GUARD.lock().map(|g| g.ollama_vram_mb()).unwrap_or(0);
    let gb = |mb: u64| format!("{:.1} GB", mb as f32 / 1024.0);
    let (ping_now, procs) = {
        let d = hw();
        (d.ping.back().copied().flatten(), d.processes.clone())
    };
    if !compact {
        let shown = badge.clone();
        let r = pill(ui, &shown, if dormant { holo::theme().deep() } else { live_c() }).on_hover_text(format!("{detail}\nClick to {}", if dormant { "wake JAI" } else { "clear the GPU now" }));
        if r.clicked() {
            if dormant {
                crate::vram_guard::wake("hardware HUD");
            } else {
                crate::vram_guard::flush_with_reason("cleared from the hardware HUD");
            }
        }
        ui.add_space(4.0);
    }
    bar(ui, "CPU", &format!("{:.0}%", t.cpu_usage_pct), t.cpu_usage_pct / 100.0, live_c());
    bar(ui, "RAM", &format!("{:.1}/{:.0} GB", t.ram_used_gb, t.ram_total_gb), t.ram_usage_pct / 100.0, holo::theme().deep());
    if total > 0 {
        bar(ui, "VRAM", &format!("{}/{}", gb(used), gb(total)), used as f32 / total as f32, if dormant { holo::theme().deep() } else { live_c() });
        if !compact {
            bar(ui, "JAI", &format!("{ollama_mb} MB"), ollama_mb as f32 / total.max(1) as f32, holo::theme().deep());
        }
    }
    if let Some(g) = &gpu {
        ui.label(mono(
            format!(
                "{} · {} · {} · {}",
                g.name.trim(),
                g.utilization_pct.map(|u| format!("{u:.0}% load")).unwrap_or_else(|| "—".into()),
                g.temperature_c.map(|c| format!("{c:.0}°C")).unwrap_or_else(|| "—".into()),
                g.power_w.map(|p| format!("{p:.0} W")).unwrap_or_else(|| "—".into())
            ),
            10.5,
            text_c(),
        ));
    }
    ui.label(mono(format!("NET ↓ {:.0} KB/s  ↑ {:.0} KB/s  · PING {}", t.net_down_kbps, t.net_up_kbps, ping_now.map(|p| format!("{p:.0} ms")).unwrap_or_else(|| "—".into())), 10.5, text_c()));
    if compact {
        *repaint = Duration::from_millis(if w.ghost { 1000 } else { 500 });
        return;
    }
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        // Per-core load.
        let (rect, _) = ui.allocate_exact_size(vec2((ui.available_width() - 130.0).max(80.0), 86.0), Sense::hover());
        let p = ui.painter_at(rect);
        let cores = t.cpu_per_core.len().max(1);
        let bw = rect.width() / cores as f32;
        for (i, load) in t.cpu_per_core.iter().enumerate() {
            let h = rect.height() * (load / 100.0).clamp(0.02, 1.0);
            let r = Rect::from_min_max(pos2(rect.left() + i as f32 * bw + 1.0, rect.bottom() - h), pos2(rect.left() + (i + 1) as f32 * bw - 1.0, rect.bottom()));
            p.rect_filled(r, Rounding::ZERO, holo::a(live_c(), 0.35 + 0.65 * load / 100.0));
        }
        holo::text(&p, rect.left_top(), Align2::LEFT_TOP, Face::Label, "CORES", 8.5, muted_c());
        // Ping radar.
        let (rect, _) = ui.allocate_exact_size(vec2(120.0, 86.0), Sense::hover());
        let p = ui.painter_at(rect);
        let c = rect.center();
        let r = 40.0;
        for k in 1..=3 {
            p.circle_stroke(c, r * k as f32 / 3.0, Stroke::new(1.0_f32, holo::a(verified(), 0.18)));
        }
        let a = (ui.input(|i| i.time) as f32 * 1.6) % std::f32::consts::TAU;
        p.line_segment([c, c + vec2(a.cos(), a.sin()) * r], Stroke::new(1.4_f32, holo::a(live_c(), 0.8)));
        let history: Vec<Option<f32>> = hw().ping.iter().copied().collect();
        let n = history.len().max(1) as f32;
        for (i, ping) in history.iter().enumerate() {
            let angle = i as f32 / n * std::f32::consts::TAU;
            match ping {
                Some(ms) => {
                    p.circle_filled(c + vec2(angle.cos(), angle.sin()) * (ms / 120.0).clamp(0.08, 1.0) * r, 2.0, if *ms < 40.0 { verified() } else if *ms < 90.0 { single() } else { contradiction() });
                }
                None => {
                    p.circle_filled(c + vec2(angle.cos(), angle.sin()) * r, 2.0, contradiction());
                }
            }
        }
        holo::text(&p, rect.left_top(), Align2::LEFT_TOP, Face::Label, "PING", 8.5, muted_c());
    });
    ui.add_space(4.0);
    ui.label(label("TOP PROCESSES (RAM)", muted_c()));
    for (name, mb) in procs {
        ui.horizontal(|ui| {
            ui.label(mono(format!("{mb:>7.0} MB"), 10.5, live_c()));
            ui.label(mono(name, 10.5, text_c()));
        });
    }
}

/// Today's lessons and events, fetched in the background every ten minutes.
#[derive(Default)]
struct ScheduleData {
    fetched_at: Option<Instant>,
    loading: bool,
    lessons: Vec<(String, Color32)>,
    lesson_note: Option<String>,
    events: Vec<String>,
    event_note: Option<String>,
}

static SCHEDULE: LazyLock<Mutex<ScheduleData>> = LazyLock::new(|| Mutex::new(ScheduleData::default()));

fn schedule() -> MutexGuard<'static, ScheduleData> {
    SCHEDULE.lock().unwrap_or_else(|e| e.into_inner())
}

fn refresh_schedule(tools: ToolContext, rt: &Handle) {
    {
        let mut s = schedule();
        if s.loading || s.fetched_at.is_some_and(|t| t.elapsed() < Duration::from_secs(600)) {
            return;
        }
        s.loading = true;
    }
    rt.spawn(async move {
        use crate::webuntis_client::LessonStatus;
        let today = chrono::Local::now().date_naive();
        let lessons = tools.pim.lessons(today, today).await;
        let (events, note) = tools.pim.schedule(today, today + chrono::Duration::days(1)).await;
        let mut s = schedule();
        match lessons {
            Ok(mut list) => {
                list.sort_by_key(|l| l.start);
                s.lessons = list
                    .iter()
                    .map(|l| {
                        let subject = if l.subject_long.trim().is_empty() { l.subject.clone() } else { l.subject_long.clone() };
                        let room = if l.rooms.is_empty() { String::new() } else { format!(" · {}", l.rooms.join("/")) };
                        let (tag, colour) = match l.status {
                            LessonStatus::Cancelled => (" · CANCELLED", contradiction()),
                            LessonStatus::Irregular => (" · CHANGED", single()),
                            LessonStatus::Regular => ("", text_c()),
                        };
                        (format!("{}–{}  {subject}{room}{tag}", l.start.format("%H:%M"), l.end.format("%H:%M")), colour)
                    })
                    .collect();
                s.lesson_note = list.is_empty().then(|| "No lessons today.".to_string());
            }
            Err(e) => {
                s.lessons.clear();
                s.lesson_note = Some(e);
            }
        }
        s.events = events
            .iter()
            .map(|e| {
                let when = if e.all_day { format!("{} all day", e.start.format("%a")) } else { e.start.format("%a %H:%M").to_string() };
                format!("{when}  {}{}", e.title, if e.location.is_empty() { String::new() } else { format!(" @ {}", e.location) })
            })
            .collect();
        s.event_note = note.or_else(|| events.is_empty().then(|| "No events today or tomorrow.".to_string()));
        s.fetched_at = Some(Instant::now());
        s.loading = false;
    });
}

fn schedule_view(ui: &mut Ui, app: &Jai, rt: &Handle, fill: Color32) {
    refresh_schedule(app.tools().clone(), rt);
    let now = chrono::Local::now();
    crate::holo_block::faded_into(ui, egui::ScrollArea::vertical().id_salt("schedule_deck").auto_shrink([false; 2]), fill, |ui| {
        ui.label(head(now.format("%A, %d %B · %H:%M").to_string(), 14.0, text_c()));
        ui.add_space(6.0);
        let s = schedule();
        ui.label(label("TIMETABLE (WEBUNTIS)", muted_c()));
        if s.loading && s.fetched_at.is_none() {
            let room = ui.available_rect_before_wrap();
            crate::holo_block::outline(ui, room.shrink(6.0), crate::holo_block::Outline::Lines);
        }
        for (line, colour) in &s.lessons {
            ui.label(mono(line, 10.5, *colour));
        }
        if let Some(note) = &s.lesson_note {
            muted(ui, note);
        }
        ui.add_space(8.0);
        ui.label(label("CALENDAR", muted_c()));
        for line in &s.events {
            ui.label(mono(line, 10.5, text_c()));
        }
        if let Some(note) = &s.event_note {
            muted(ui, note);
        }
        drop(s);
        ui.add_space(10.0);
        ui.label(label("STARK PROTOCOLS", muted_c()));
        let protocols = app.tools().protocols.list();
        if protocols.is_empty() {
            muted(ui, "No protocols yet — \"JAI, create a gaming protocol that …\"");
        }
        ui.horizontal_wrapped(|ui| {
            for p in protocols {
                if pill(ui, &p.name, live_c()).on_hover_text(&p.description).clicked() {
                    let app = app.clone();
                    let text = format!("engage {} protocol", p.name);
                    rt.spawn(async move {
                        app.handle_command(crate::hud::HudCommand::Chat { text }).await;
                    });
                }
            }
        });
        ui.add_space(8.0);
        ui.label(label("MISSION PROFILES", muted_c()));
        ui.horizontal_wrapped(|ui| {
            for scene in crate::spatial_scenes::builtin_scenes() {
                if pill(ui, &scene.name, holo::theme().deep()).on_hover_text(&scene.description).clicked() {
                    // Off the UI thread: loading re-arranges this very canvas.
                    let tools = app.tools().clone();
                    rt.spawn(async move {
                        if let Err(e) = crate::spatial_scenes::load_spatial_scene(&tools, &scene.name) {
                            warn!("[WORKSPACE] {e}");
                        }
                    });
                }
            }
        });
    });
}

// ---------------------------------------------------------------------------- tools ----

pub fn is_tool(name: &str) -> bool {
    matches!(name, "spawn_multi_window_workspace" | "control_spatial_window")
}

pub fn definitions() -> Vec<Value> {
    let kinds = ["crawler", "dossier", "graph", "inspector", "hardware", "schedule", "all"];
    vec![
        json!({"type": "function", "function": {
            "name": "spawn_multi_window_workspace",
            "description": "Bring up the cinematic multi-window workspace: glass windows fanned out from the orb. For 'bring up the research workspace', 'open all windows', 'show me everything on <topic>' (a topic starts or reuses a deep research mission). Windows: crawler, dossier, graph, inspector (code / JSON / CSV / plot; pass inspect), hardware, schedule.",
            "parameters": {"type": "object", "properties": {
                "windows": {"type": "array", "items": {"type": "string", "enum": kinds}},
                "topic": {"type": "string"},
                "mission_id": {"type": "integer"},
                "duration_minutes": {"type": "integer", "description": "Research time when a topic starts a new mission (default 30)."},
                "inspect": {"type": "object", "properties": {
                    "title": {"type": "string"},
                    "content": {"type": "string", "description": "Code, JSON, CSV, Markdown, numbers or a function of x (t animates) to plot."},
                    "format": {"type": "string", "enum": ["code", "json", "csv", "markdown", "plot", "text"]}
                }}
            }}
        }}),
        json!({"type": "function", "function": {
            "name": "control_spatial_window",
            "description": "Control the floating workspace windows: ghost (click-through overlay over a game, 45 % glass, on top), unghost ('disable ghost mode'), pin / unpin (always on top), collapse (into the orb), restore, close, retile (re-spread them), focus. window: crawler, dossier, graph, inspector, hardware/telemetry, schedule, or all.",
            "parameters": {"type": "object", "properties": {
                "action": {"type": "string", "enum": ["ghost", "unghost", "pin", "unpin", "collapse", "restore", "close", "retile", "focus"]},
                "window": {"type": "string"}
            }, "required": ["action"]}
        }}),
    ]
}

fn kinds_arg(args: &Value) -> Option<Vec<SpatialWindowKind>> {
    let list: Vec<String> = match args.get("windows").or_else(|| args.get("window_types")) {
        Some(Value::Array(a)) => a.iter().filter_map(|v| v.as_str().map(String::from)).collect(),
        Some(Value::String(s)) => s.split(',').map(|s| s.trim().to_string()).collect(),
        _ => return None,
    };
    if list.iter().any(|s| matches!(s.to_lowercase().as_str(), "all" | "everything" | "every")) {
        return Some(SpatialWindowKind::ALL.to_vec());
    }
    let mut kinds: Vec<SpatialWindowKind> = list.iter().filter_map(|s| SpatialWindowKind::parse(s)).collect();
    kinds.dedup();
    (!kinds.is_empty()).then_some(kinds)
}

pub fn run_tool(tools: &ToolContext, name: &str, args: &Value) -> Result<String, String> {
    match name {
        "spawn_multi_window_workspace" => {
            let topic = crate::features::str_arg(args, &["topic", "subject", "query"]).map(str::trim).filter(|t| !t.is_empty()).map(String::from);
            let mut mission = args["mission_id"].as_i64();
            let mut started = None;
            if let (None, Some(t)) = (mission, &topic) {
                let lower = t.to_lowercase();
                mission = research::list_missions(30).into_iter().find(|m| m.topic.to_lowercase().contains(&lower) || lower.contains(&m.topic.to_lowercase())).map(|m| m.id);
                if mission.is_none() {
                    let minutes = args["duration_minutes"].as_u64().unwrap_or(30);
                    research::run_tool(tools, &json!({ "action": "start", "topic": t, "duration_minutes": minutes, "open_workspace": false }))?;
                    mission = research::latest_mission_id();
                    started = Some(minutes);
                }
            }
            if let Some(i) = args.get("inspect").filter(|v| v.is_object()) {
                let body = i["content"].as_str().unwrap_or("").to_string();
                let format = InspectFormat::parse(i["format"].as_str().unwrap_or(""), &body);
                let item = Inspector { title: i["title"].as_str().unwrap_or("Inspector").to_string(), body, format };
                // Only something to look at: the inspector alone.
                if topic.is_none() && kinds_arg(args).is_none() {
                    let title = item.title.clone();
                    inspect(item);
                    return Ok(format!("\"{title}\" is in the code & sandbox inspector ({format:?})."));
                }
                canvas().inspect = Some(item);
            }
            let kinds = kinds_arg(args).unwrap_or_else(|| {
                if topic.is_some() || (mission.is_some() && args.get("inspect").is_none()) {
                    let mut k = SpatialWindowKind::RESEARCH.to_vec();
                    k.push(SpatialWindowKind::HardwareAndVramHud);
                    k
                } else {
                    SpatialWindowKind::ALL.to_vec()
                }
            });
            let open = spawn_windows(&kinds, mission.or_else(research::latest_mission_id));
            let names = kinds.iter().map(|k| k.title().to_lowercase()).collect::<Vec<_>>().join(", ");
            Ok(crate::features::spoken(
                format!(
                    "Workspace up: {names} ({open} windows open){}.",
                    started.map(|m| format!("; deep research on \"{}\" started for {m} minutes", topic.clone().unwrap_or_default())).unwrap_or_default()
                ),
                match started {
                    Some(_) if crate::vram_guard::in_game() => "Spreading out the workspace, Sir — the research starts once your game is over.".to_string(),
                    Some(_) => "Spreading out the workspace, Sir — and the research is under way.".to_string(),
                    None => "Spreading out the workspace, Sir.".to_string(),
                },
            ))
        }
        "control_spatial_window" => {
            let action = crate::features::str_arg(args, &["action"]).unwrap_or("").to_lowercase().replace([' ', '-'], "_");
            let target = crate::features::str_arg(args, &["window", "target", "name"]).unwrap_or("all");
            let kind = if matches!(target.to_lowercase().as_str(), "all" | "every" | "everything" | "windows" | "") { None } else { SpatialWindowKind::parse(target) };
            if kind.is_none() && !matches!(target.to_lowercase().as_str(), "all" | "every" | "everything" | "windows" | "") {
                return Err(format!("I don't know a workspace window called \"{target}\"."));
            }
            let what = kind.map(|k| format!("the {} window", k.title().to_lowercase())).unwrap_or_else(|| "all workspace windows".into());
            let n = match action.as_str() {
                "ghost" | "ghost_mode" | "click_through" => set_ghost(kind, true),
                "unghost" | "disable_ghost" | "disable_ghost_mode" | "solid" => set_ghost(kind, false),
                "pin" | "on_top" => set_pinned(kind, true),
                "unpin" => set_pinned(kind, false),
                "collapse" | "minimize" | "minimise" => into_orb(kind, false),
                "close" | "dismiss" => into_orb(kind, true),
                "focus" | "front" | "raise" => {
                    let mut c = canvas();
                    let mut n = 0;
                    for w in c.windows.iter_mut().filter(|w| w.open() && kind.is_none_or(|k| w.kind == k)) {
                        w.focus = true;
                        n += 1;
                    }
                    c.repaint_all();
                    n
                }
                "restore" | "retile" | "spread" => {
                    let kinds: Vec<SpatialWindowKind> = {
                        let c = canvas();
                        c.windows.iter().filter(|w| !w.closing && kind.is_none_or(|k| w.kind == k)).map(|w| w.kind).collect()
                    };
                    if kinds.is_empty() {
                        0
                    } else {
                        spawn_windows(&kinds, None)
                    }
                }
                other => return Err(format!("Unknown window action '{other}'.")),
            };
            if n == 0 {
                return Err(format!("There is no open workspace window for that ({what})."));
            }
            Ok(match action.as_str() {
                "ghost" | "ghost_mode" | "click_through" => format!("Ghost mode on for {what}: clicks pass straight through. Say \"disable ghost mode\" or press UN-GHOST ALL in the HUD to undo."),
                "unghost" | "disable_ghost" | "disable_ghost_mode" | "solid" => format!("Ghost mode off for {what}."),
                _ => format!("Done: {action} for {what}."),
            })
        }
        other => Err(format!("Unknown workspace tool '{other}'.")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_names_parse() {
        assert_eq!(SpatialWindowKind::parse("the telemetry window"), Some(SpatialWindowKind::HardwareAndVramHud));
        assert_eq!(SpatialWindowKind::parse("knowledge graph"), Some(SpatialWindowKind::KnowledgeGraphPinboard));
        assert_eq!(SpatialWindowKind::parse("crawler"), Some(SpatialWindowKind::LiveCrawlerStream));
        assert_eq!(SpatialWindowKind::parse("timetable"), Some(SpatialWindowKind::ScheduleAndCommandDeck));
        assert_eq!(SpatialWindowKind::parse("banana"), None);
        assert_eq!(kinds_arg(&json!({"windows": ["all"]})).map(|k| k.len()), Some(6));
    }

    #[test]
    fn csv_and_formats() {
        assert_eq!(csv_fields(r#"a,"b, c",d"#, ','), vec!["a", "b, c", "d"]);
        assert_eq!(csv_delimiter("x;y;z\n1;2;3"), ';');
        assert_eq!(InspectFormat::parse("", r#"{"a": 1}"#), InspectFormat::Json);
        assert_eq!(InspectFormat::parse("plot", "sin(x)"), InspectFormat::Plot);
    }

    #[test]
    fn the_window_manager_fans_out_non_overlapping_viewports() {
        // The only test that touches the global canvas.
        assert_eq!(spawn_research_workspace(Some(42)), 4);
        let area = area_around(orb_center());
        {
            let c = canvas();
            let kinds: Vec<SpatialWindowKind> = c.windows.iter().map(|w| w.kind).collect();
            for k in SpatialWindowKind::RESEARCH {
                assert!(kinds.contains(&k), "{k:?}");
            }
            let rects: Vec<Rect> = c.windows.iter().filter(|w| w.open()).map(SpatialWindow::rect).collect();
            for (i, a) in rects.iter().enumerate() {
                assert!(area.expand(1.0).contains_rect(*a), "{a:?} outside {area:?}");
                for b in &rects[i + 1..] {
                    assert!(!a.intersects(*b), "{a:?} overlaps {b:?}");
                }
            }
            for w in &c.windows {
                assert!(w.title().contains("J.A.I."), "never a target of 'close it'");
                assert_eq!(w.mission_id, Some(42));
                assert_ne!(w.spring.current, w.spring.target, "they glide out of the orb");
            }
        }
        // The topic knowledge workspace: three titled windows, re-using the open ones.
        set_brief(42, "# Spotify\n\nA streaming service.");
        spawn_labeled(&crate::intent_engine::topic_workspace_windows(), Some(42));
        {
            let c = canvas();
            let dossier = c.windows.iter().find(|w| w.kind == SpatialWindowKind::ExecutiveDossier).unwrap();
            assert_eq!(dossier.label.as_deref(), Some("OVERVIEW & FACTS"));
            assert!(dossier.title().starts_with("OVERVIEW & FACTS — J.A.I."));
            assert!(c.briefs.contains_key(&42));
            assert_eq!(c.windows.iter().filter(|w| w.open()).count(), 4);
        }
        // Ghost mode (click-through, pinned) and UN-GHOST ALL.
        assert_eq!(set_ghost(Some(SpatialWindowKind::HardwareAndVramHud), true), 0);
        assert_eq!(set_ghost(Some(SpatialWindowKind::LiveCrawlerStream), true), 1);
        assert_eq!(ghost_count(), 1);
        assert!(canvas().windows.iter().find(|w| w.kind == SpatialWindowKind::LiveCrawlerStream).unwrap().pinned);
        assert_eq!(unghost_all(), 4);
        assert_eq!(ghost_count(), 0);
        // A recalled profile: its window glides in, the others are pulled into the orb.
        let place = Rect::from_min_size(pos2(10.0, 10.0), vec2(400.0, 300.0));
        apply_scene(vec![ScenePlacement { kind: SpatialWindowKind::HardwareAndVramHud, rect: place, ghost: true, pinned: true, alpha: Some(115) }], Some(42));
        {
            let c = canvas();
            let open: Vec<&SpatialWindow> = c.windows.iter().filter(|w| w.open()).collect();
            assert_eq!(open.len(), 1);
            assert!(open[0].ghost && open[0].pinned);
            assert_eq!(open[0].spring.target, place.min);
        }
        // (Outside the lock above: the canvas mutex is not re-entrant.)
        assert_eq!(window_placements(), vec![ScenePlacement { kind: SpatialWindowKind::HardwareAndVramHud, rect: place, ghost: true, pinned: true, alpha: Some(115) }]);
        assert_eq!(into_orb(None, true), 1);
        assert!(canvas().windows.iter().all(|w| !w.open()));
        // "No. Just can you close the windows you opened?": every window goes at once (no
        // retract animation), far inside the 15 ms budget.
        spawn_labeled(&crate::intent_engine::topic_workspace_windows(), Some(7));
        assert_eq!(open_count(), 3);
        let started = std::time::Instant::now();
        assert_eq!(close_all_now(), 3);
        assert!(started.elapsed() < Duration::from_millis(15), "{:?}", started.elapsed());
        assert!(canvas().windows.is_empty());
        assert_eq!(open_count(), 0);
    }

    #[test]
    fn windows_fly_out_and_back_into_the_orb() {
        let orb = pos2(1280.0, 700.0);
        let target = Rect::from_min_size(pos2(100.0, 100.0), vec2(600.0, 500.0));
        let mut w = SpatialWindow::new(1, SpatialWindowKind::ExecutiveDossier, orb, target, None, Duration::ZERO);
        assert_eq!(w.spring.current, orb - target.size() / 2.0);
        while w.spring.step(1.0 / 60.0) | w.fade.step(1.0 / 60.0) {}
        assert_eq!(w.spring.current, target.min);
        assert_eq!(w.fade.current, 1.0);
        w.retract(orb, false);
        while w.spring.step(1.0 / 60.0) | w.fade.step(1.0 / 60.0) {}
        assert!(w.collapsed && w.fade.current < 0.05);
        assert_eq!(w.restore_to, Some(target.min));
    }
}
