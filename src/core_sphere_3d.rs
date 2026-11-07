//! "Jarvis, show me your core": every real source file of Jarvis as a node of a rotatable,
//! zoomable 3D sphere, wired to the files it uses, lighting up while it runs.
//!
//! * The graph comes from `build.rs` (`CORE_FILES`, `CORE_EDGES`): each `src/*.rs` file, its
//!   non-empty lines and the files it references (`crate::x`, `use crate::{…}`, `super::` in the
//!   GUI's path modules, `mod x;` in main.rs). The orchestrators (main, app, brain, gui, tools) sit
//!   on an inner shell, every other module on an outer Fibonacci-lattice shell; dependencies are
//!   great-circle splines that bulge outwards.
//! * `CoreActivityBus::pulse("brain.rs")` marks a module as running. A node glows for three seconds
//!   after its pulse, and consecutive pulses (within 2.5 s) send energy packets along the path
//!   between them — speaking lights ears → stt → intents → brain, answering brain → lexicon →
//!   reference transfer → tts.
//! * The view: drag to orbit (with inertia), the wheel or the `CORE SCALE` slider to zoom
//!   (50-300 %), hover or click a node for its readout (file, lines, connected modules, last
//!   execution). The centre pulses with Jarvis' voice.

use eframe::egui::{self, pos2, vec2, Align2, Color32, FontId, Pos2, Rect, Rounding, Sense, Shape, Stroke, Ui, Vec2};
use std::collections::{HashMap, VecDeque};
use std::f32::consts::{PI, TAU};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

include!(concat!(env!("OUT_DIR"), "/core_graph.rs"));

/// The orchestrators on the inner shell.
pub const INNER_FILES: [&str; 5] = ["main.rs", "app.rs", "brain.rs", "gui.rs", "tools.rs"];
const INNER_RADIUS: f32 = 0.42;
pub const MIN_ZOOM: f32 = 0.5;
pub const MAX_ZOOM: f32 = 3.0;
/// Radians per pixel of drag.
const DRAG_RATE: f32 = 0.008;
/// A node glows this long after its pulse.
pub const GLOW_SECS: f32 = 3.0;
/// Pulses this close together are one chain (packets travel between them).
pub const CHAIN_WINDOW: Duration = Duration::from_millis(2500);
/// A packet's flight time along a spline, and the stagger between packets of one chain.
pub const PACKET_SECS: f32 = 0.7;
const PACKET_STAGGER: Duration = Duration::from_millis(220);
const MAX_PACKETS: usize = 64;
const ARC_STEPS: usize = 14;

/// What a module is about (its colour on the sphere).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Category {
    Core,
    Voice,
    Interface,
    Integration,
    System,
}

impl Category {
    pub fn colour(self) -> Color32 {
        match self {
            Self::Core => crate::theme_engine::current().primary(),
            Self::Voice => crate::theme_engine::current().secondary(),
            Self::Interface => Color32::from_rgb(0xDD, 0xF8, 0xFF),
            Self::Integration => Color32::from_rgb(0xFF, 0xC2, 0x4A),
            Self::System => Color32::from_rgb(0x7F, 0xA6, 0xB6),
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Core => "CORE",
            Self::Voice => "VOICE",
            Self::Interface => "INTERFACE",
            Self::Integration => "INTEGRATION",
            Self::System => "SYSTEM",
        }
    }
}

/// A file's category from its name.
pub fn category(name: &str) -> Category {
    let stem = name.trim_end_matches(".rs");
    let has = |words: &[&str]| words.iter().any(|w| stem.contains(w));
    if INNER_FILES.contains(&name) || has(&["intent", "context", "memory", "config", "runtime", "llm_reboot", "thinking_guard", "persona", "anti_repeat", "skills", "tool_forge", "lifecycle"]) {
        Category::Core
    } else if has(&["ears", "stt", "tts", "vad", "wake", "phonem", "pronunc", "voice", "audio", "dsp", "acoustic", "modality", "fish"]) {
        Category::Voice
    } else if has(&["gui", "hud", "emblem", "core_sphere", "widget", "canvas", "interface", "oled", "popup", "icon", "views", "graph_view", "scenes", "physics", "tray"]) {
        Category::Interface
    } else if has(&["webuntis", "calendar", "lms", "itslearning", "email", "whatsapp", "music", "pim", "web", "satellite", "contacts", "media", "location", "commute", "briefing", "research", "finance", "workout", "deal", "omni", "sites", "integration", "study", "exam", "airdrop", "teleport", "tunnel"]) {
        Category::Integration
    } else {
        Category::System
    }
}

/// `n` points evenly spread over a sphere of `radius` (golden-angle Fibonacci lattice).
pub fn fibonacci_sphere(n: usize, radius: f32) -> Vec<[f32; 3]> {
    let golden = PI * (3.0 - 5f32.sqrt());
    (0..n)
        .map(|i| {
            let y = if n == 1 { 0.0 } else { 1.0 - 2.0 * (i as f32 + 0.5) / n as f32 };
            let r = (1.0 - y * y).max(0.0).sqrt();
            let a = golden * i as f32;
            [a.cos() * r * radius, y * radius, a.sin() * r * radius]
        })
        .collect()
}

fn length(v: [f32; 3]) -> f32 {
    (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt()
}

fn normalize(v: [f32; 3]) -> [f32; 3] {
    let l = length(v).max(1e-6);
    [v[0] / l, v[1] / l, v[2] / l]
}

/// A great-circle spline from `a` to `b` (radius interpolated, bulging 12 % outwards in the
/// middle), `steps + 1` points.
pub fn arc(a: [f32; 3], b: [f32; 3], steps: usize) -> Vec<[f32; 3]> {
    let (ra, rb) = (length(a), length(b));
    let (ua, mut ub) = (normalize(a), normalize(b));
    let mut dot = (ua[0] * ub[0] + ua[1] * ub[1] + ua[2] * ub[2]).clamp(-1.0, 1.0);
    if dot < -0.999 {
        // Antipodal: bend through a perpendicular.
        ub = normalize([ub[0] + 0.05, ub[1] + 0.07, ub[2] - 0.03]);
        dot = (ua[0] * ub[0] + ua[1] * ub[1] + ua[2] * ub[2]).clamp(-1.0, 1.0);
    }
    let omega = dot.acos();
    (0..=steps)
        .map(|k| {
            let t = k as f32 / steps.max(1) as f32;
            let (wa, wb) = if omega < 1e-4 { (1.0 - t, t) } else { (((1.0 - t) * omega).sin() / omega.sin(), (t * omega).sin() / omega.sin()) };
            let u = normalize([ua[0] * wa + ub[0] * wb, ua[1] * wa + ub[1] * wb, ua[2] * wa + ub[2] * wb]);
            let r = (ra + (rb - ra) * t) * (1.0 + 0.12 * (PI * t).sin());
            [u[0] * r, u[1] * r, u[2] * r]
        })
        .collect()
}

/// A source file on the sphere.
#[derive(Debug, Clone)]
pub struct CoreNode {
    pub name: &'static str,
    pub loc: u32,
    pub pos: [f32; 3],
    pub category: Category,
}

/// The whole sphere: nodes and (from, to, references) edges.
#[derive(Debug, Clone)]
pub struct CoreGraph {
    pub nodes: Vec<CoreNode>,
    pub edges: Vec<(usize, usize, u16)>,
}

impl CoreGraph {
    /// The compiled-in module graph, laid out: inner shell for the orchestrators, the rest on the
    /// outer lattice (bigger files nearer the equator, so the poles stay readable).
    pub fn build() -> Self {
        Self::from_parts(CORE_FILES, CORE_EDGES)
    }

    pub fn from_parts(files: &[(&'static str, u32)], edges: &[(u16, u16, u16)]) -> Self {
        let inner: Vec<usize> = (0..files.len()).filter(|i| INNER_FILES.contains(&files[*i].0)).collect();
        let mut outer: Vec<usize> = (0..files.len()).filter(|i| !inner.contains(i)).collect();
        // Alternate big and small files along the lattice so neither pole crowds.
        outer.sort_by_key(|i| std::cmp::Reverse(files[*i].1));
        let mut ordered = Vec::with_capacity(outer.len());
        let (mut lo, mut hi) = (0usize, outer.len());
        while lo < hi {
            ordered.push(outer[lo]);
            lo += 1;
            if lo < hi {
                hi -= 1;
                ordered.push(outer[hi]);
            }
        }
        let mut pos = vec![[0.0f32; 3]; files.len()];
        for (k, p) in fibonacci_sphere(inner.len(), INNER_RADIUS).into_iter().enumerate() {
            pos[inner[k]] = p;
        }
        for (k, p) in fibonacci_sphere(ordered.len(), 1.0).into_iter().enumerate() {
            pos[ordered[k]] = p;
        }
        let nodes = files.iter().zip(pos).map(|((name, loc), pos)| CoreNode { name, loc: *loc, pos, category: category(name) }).collect();
        let edges = edges.iter().map(|(a, b, n)| (*a as usize, *b as usize, *n)).filter(|(a, b, _)| *a < files.len() && *b < files.len() && a != b).collect();
        Self { nodes, edges }
    }

    pub fn index(&self, name: &str) -> Option<usize> {
        self.nodes.iter().position(|n| n.name == name)
    }

    /// The files node `i` uses or is used by, most-referenced first.
    pub fn neighbours(&self, i: usize) -> Vec<usize> {
        let mut out: Vec<(usize, u16)> = self.edges.iter().filter_map(|(a, b, n)| if *a == i { Some((*b, *n)) } else if *b == i { Some((*a, *n)) } else { None }).collect();
        out.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
        let mut seen = std::collections::HashSet::new();
        out.into_iter().filter(|(j, _)| seen.insert(*j)).map(|(j, _)| j).collect()
    }
}

// --------------------------------------------------------------------------- activity bus ----

#[derive(Debug, Clone, Copy)]
struct Packet {
    from: &'static str,
    to: &'static str,
    at: Instant,
}

#[derive(Default)]
struct BusState {
    last: HashMap<&'static str, Instant>,
    packets: VecDeque<Packet>,
    previous: Option<(&'static str, Instant)>,
    next_packet: Option<Instant>,
}

impl BusState {
    fn pulse_at(&mut self, file: &'static str, now: Instant) {
        self.last.insert(file, now);
        if let Some((prev, at)) = self.previous {
            if prev != file && now.saturating_duration_since(at) <= CHAIN_WINDOW {
                // Packets of one chain leave one after another, so the chain reads in order.
                let start = self.next_packet.filter(|t| *t > now).unwrap_or(now);
                self.packets.push_back(Packet { from: prev, to: file, at: start });
                self.next_packet = Some(start + PACKET_STAGGER);
                while self.packets.len() > MAX_PACKETS {
                    self.packets.pop_front();
                }
            }
        }
        self.previous = Some((file, now));
    }

    fn packets(&mut self, now: Instant) -> Vec<(&'static str, &'static str, f32)> {
        self.packets.retain(|p| now.saturating_duration_since(p.at).as_secs_f32() <= PACKET_SECS);
        self.packets
            .iter()
            .filter(|p| p.at <= now)
            .map(|p| (p.from, p.to, (now.duration_since(p.at).as_secs_f32() / PACKET_SECS).clamp(0.0, 1.0)))
            .collect()
    }
}

static BUS: LazyLock<Mutex<BusState>> = LazyLock::new(|| Mutex::new(BusState::default()));

/// Which modules are running: `CoreActivityBus::pulse("brain.rs")` from anywhere.
pub struct CoreActivityBus;

impl CoreActivityBus {
    /// Marks `file` as running now; a pulse soon after another one sends a packet between them.
    pub fn pulse(file: &'static str) {
        BUS.lock().unwrap_or_else(|e| e.into_inner()).pulse_at(file, Instant::now());
        crate::gui::refresh();
    }

    /// A sequence of modules handing work to each other ("ears.rs", "stt.rs", "brain.rs").
    pub fn chain(files: &[&'static str]) {
        for f in files {
            Self::pulse(f);
        }
    }

    /// When `file` last ran.
    pub fn last_active(file: &str) -> Option<Instant> {
        BUS.lock().unwrap_or_else(|e| e.into_inner()).last.get(file).copied()
    }

    /// Packets in flight now: (from, to, progress 0-1).
    pub fn packets(now: Instant) -> Vec<(&'static str, &'static str, f32)> {
        BUS.lock().unwrap_or_else(|e| e.into_inner()).packets(now)
    }

    /// The module a tool runs in (so a tool call lights the right file).
    pub fn module_for_tool(tool: &str) -> Option<&'static str> {
        let t = tool.to_lowercase();
        let has = |w: &[&str]| w.iter().any(|x| t.contains(x));
        Some(if has(&["music", "playlist", "track", "sonora", "spotify"]) {
            "music.rs"
        } else if has(&["volume", "mixer", "mute"]) {
            "audio_mixer.rs"
        } else if has(&["hud_board", "board"]) {
            "hud_board.rs"
        } else if has(&["school", "webuntis", "timetable"]) {
            "webuntis_client.rs"
        } else if has(&["course", "itslearning", "homework"]) {
            "itslearning_bw_sso.rs"
        } else if has(&["calendar", "event"]) {
            "calendar_client.rs"
        } else if has(&["email", "mail"]) {
            "email_client.rs"
        } else if has(&["whatsapp", "message"]) {
            "whatsapp_client.rs"
        } else if has(&["web_search", "search_web"]) {
            "websearch.rs"
        } else if has(&["vram", "gpu"]) {
            "vram_guard.rs"
        } else if has(&["forge"]) {
            "tool_forge.rs"
        } else if has(&["research"]) {
            "deep_research_engine.rs"
        } else if has(&["reboot", "language_model"]) {
            "llm_reboot.rs"
        } else if has(&["interface", "configure", "window_style", "gui"]) {
            "interface.rs"
        } else if has(&["window", "focus"]) {
            "winctl.rs"
        } else if has(&["file", "folder"]) {
            "file_ops.rs"
        } else if has(&["memory", "remember"]) {
            "memory.rs"
        } else if has(&["reminder", "timer", "alarm"]) {
            "reminders.rs"
        } else if has(&["replay", "clip"]) {
            "instant_replay.rs"
        } else if has(&["widget"]) {
            "widgets.rs"
        } else {
            return None;
        })
    }
}

/// Shorthand for `CoreActivityBus::pulse`.
pub fn pulse(file: &'static str) {
    CoreActivityBus::pulse(file);
}

// ---------------------------------------------------------------------------------- view ----

/// The voice / button commands for the view.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CoreCommand {
    Open(bool),
    /// Absolute zoom (1 = 100 %).
    Zoom(f32),
    /// Relative zoom (× factor).
    ZoomBy(f32),
    /// Auto-rotation speed × factor (0 stops it).
    Spin(f32),
}

/// The interactive sphere.
pub struct CoreSphereView {
    graph: CoreGraph,
    yaw: f32,
    pitch: f32,
    velocity: Vec2,
    zoom: f32,
    zoom_target: f32,
    /// Auto-rotation (rad/s).
    spin: f32,
    hovered: Option<usize>,
    selected: Option<usize>,
    last_step: Option<Instant>,
    /// The pointer is over the sphere: auto-rotation pauses.
    hover_hold: bool,
}

impl Default for CoreSphereView {
    fn default() -> Self {
        Self { graph: CoreGraph::build(), yaw: 0.6, pitch: -0.35, velocity: Vec2::ZERO, zoom: 1.0, zoom_target: 1.0, spin: 0.12, hovered: None, selected: None, last_step: None, hover_hold: false }
    }
}

impl CoreSphereView {
    #[cfg(test)]
    pub fn zoom_target(&self) -> f32 {
        self.zoom_target
    }

    pub fn set_zoom(&mut self, zoom: f32) {
        self.zoom_target = zoom.clamp(MIN_ZOOM, MAX_ZOOM);
    }

    /// Where the sphere faces: (yaw, pitch) in radians.
    pub fn orientation(&self) -> (f32, f32) {
        (self.yaw, self.pitch)
    }

    pub fn apply(&mut self, cmd: CoreCommand) {
        match cmd {
            CoreCommand::Zoom(z) => self.set_zoom(z),
            CoreCommand::ZoomBy(f) => self.set_zoom(self.zoom_target * f),
            CoreCommand::Spin(f) => self.spin = if f <= 0.0 { 0.0 } else if self.spin.abs() < 1e-3 { 0.12 * f } else { (self.spin * f).clamp(-3.0, 3.0) },
            CoreCommand::Open(_) => {}
        }
    }

    /// Rotation by yaw (about y) then pitch (about x), and the perspective projection onto a
    /// circle of `radius` px around `centre`: (screen point, depth; > 0 is behind).
    pub fn project(&self, p: [f32; 3], centre: Pos2, radius: f32) -> (Pos2, f32) {
        let (sy, cy) = self.yaw.sin_cos();
        let (x1, z1) = (p[0] * cy + p[2] * sy, -p[0] * sy + p[2] * cy);
        let (sp, cp) = self.pitch.sin_cos();
        let (y2, z2) = (p[1] * cp - z1 * sp, p[1] * sp + z1 * cp);
        let focal = 3.2;
        let scale = focal / (focal + z2);
        (centre + vec2(x1, -y2) * radius * scale, z2)
    }

    /// Inertia, auto-rotation and the zoom easing (frame-rate independent).
    pub fn step(&mut self, dt: f32) {
        let dt = dt.clamp(0.0, 0.1);
        let spin = if self.hover_hold { 0.0 } else { self.spin };
        self.yaw = (self.yaw + (self.velocity.x + spin) * dt).rem_euclid(TAU);
        self.pitch = (self.pitch + self.velocity.y * dt).clamp(-1.4, 1.4);
        self.velocity *= (-3.0 * dt).exp();
        self.zoom += (self.zoom_target - self.zoom) * (1.0 - (-10.0 * dt).exp());
    }

    /// Grab-and-turn: the front face follows the pointer (drag right turns it right, drag up
    /// tilts it up), and the flick keeps spinning with inertia.
    pub fn drag(&mut self, d: Vec2, dt: f32) {
        self.yaw = (self.yaw - d.x * DRAG_RATE).rem_euclid(TAU);
        self.pitch = (self.pitch - d.y * DRAG_RATE).clamp(-1.4, 1.4);
        if dt > 0.0 {
            self.velocity = vec2(-d.x * DRAG_RATE, -d.y * DRAG_RATE) / dt.max(1.0 / 240.0);
        }
    }

    /// Node radius on screen (px) for its lines of code.
    fn node_px(loc: u32, scale: f32) -> f32 {
        ((loc.max(20) as f32 / 40.0).ln().max(0.0) * 1.6 + 2.2) * scale
    }

    /// Draws and drives the sphere inside `rect` (`voice`: 0-1 for the pulsing centre).
    pub fn show(&mut self, ui: &mut Ui, rect: Rect, voice: f32) {
        let now = Instant::now();
        let dt = self.last_step.map(|t| now.duration_since(t).as_secs_f32()).unwrap_or(0.0);
        self.last_step = Some(now);
        let response = ui.interact(rect, ui.id().with("core_sphere"), Sense::click_and_drag());
        if response.dragged() {
            self.drag(response.drag_delta(), dt);
        }
        // The pointer over the sphere holds its auto-rotation, so a node stays under the cursor.
        let centre = rect.center();
        let radius = rect.width().min(rect.height()) * 0.36 * self.zoom;
        self.hover_hold = response.hover_pos().is_some_and(|p| p.distance(centre) <= radius * 1.08);
        if response.hovered() {
            let wheel = ui.input(|i| i.smooth_scroll_delta.y);
            if wheel.abs() > 0.1 {
                self.set_zoom(self.zoom_target * (1.0 + wheel * 0.0015));
            }
        }
        self.step(dt);

        let painter = ui.painter_at(rect);
        let screen: Vec<(Pos2, f32)> = self.graph.nodes.iter().map(|n| self.project(n.pos, centre, radius)).collect();
        let scale = (radius / 300.0).clamp(0.6, 2.2);

        // Hover: the nearest node under the pointer, front-most first.
        self.hovered = response.hover_pos().and_then(|p| {
            let mut best: Option<(usize, f32)> = None;
            for (i, (s, d)) in screen.iter().enumerate() {
                let r = Self::node_px(self.graph.nodes[i].loc, scale) + 5.0;
                if (*s - p).length() <= r && best.is_none_or(|(_, bd)| *d < bd) {
                    best = Some((i, *d));
                }
            }
            best.map(|(i, _)| i)
        });
        if response.clicked() {
            self.selected = if self.hovered == self.selected { None } else { self.hovered };
        }
        let focus = self.hovered.or(self.selected);
        let neighbours: Vec<usize> = focus.map(|i| self.graph.neighbours(i)).unwrap_or_default();
        let glow = |name: &str| CoreActivityBus::last_active(name).map(|t| (1.0 - now.duration_since(t).as_secs_f32() / GLOW_SECS).max(0.0)).unwrap_or(0.0);
        let fog = |d: f32| (1.0 - 0.6 * ((d + 1.0) / 2.0).clamp(0.0, 1.0)).clamp(0.25, 1.0);

        // The voice-driven core at the centre.
        let core_r = radius * (0.03 + 0.03 * voice);
        for (k, a) in [(2.6, 0.035), (1.6, 0.08), (1.0, 0.4)] {
            painter.circle_filled(centre, core_r * k, crate::theme_engine::hdr_neon_color(crate::theme_engine::current().core_primary, (255.0 * a) as u8));
        }
        painter.circle_stroke(centre, radius * (0.5 + 0.02 * voice), Stroke::new(1.0f32, Color32::from_rgba_premultiplied(0, 40, 48, 0)));

        // Splines, back to front by their midpoint.
        let mut edges: Vec<(f32, usize, usize)> = self
            .graph
            .edges
            .iter()
            .map(|(a, b, _)| {
                let mid = arc(self.graph.nodes[*a].pos, self.graph.nodes[*b].pos, 2)[1];
                (self.project(mid, centre, radius).1, *a, *b)
            })
            .collect();
        edges.sort_by(|x, y| y.0.total_cmp(&x.0));
        for (depth, a, b) in edges {
            let lit = focus.is_some_and(|f| f == a || f == b);
            let hot = glow(self.graph.nodes[a].name).min(glow(self.graph.nodes[b].name));
            let base = if lit { 0.75 } else if focus.is_some() { 0.04 } else { 0.11 };
            let alpha = (base + 0.6 * hot) * fog(depth);
            if alpha < 0.02 {
                continue;
            }
            let colour = if lit { Category::Voice.colour() } else if hot > 0.0 { self.graph.nodes[a].category.colour() } else { crate::hud_board::border_colour() };
            let points: Vec<Pos2> = arc(self.graph.nodes[a].pos, self.graph.nodes[b].pos, ARC_STEPS).into_iter().map(|p| self.project(p, centre, radius).0).collect();
            painter.add(Shape::line(points, Stroke::new(if lit { 1.4f32 } else { 0.8 }, additive(colour, alpha))));
        }

        // Packets of energy travelling between modules.
        for (from, to, t) in CoreActivityBus::packets(now) {
            let (Some(a), Some(b)) = (self.graph.index(from), self.graph.index(to)) else { continue };
            let path = arc(self.graph.nodes[a].pos, self.graph.nodes[b].pos, ARC_STEPS * 2);
            let at = |u: f32| {
                let x = u.clamp(0.0, 1.0) * (path.len() - 1) as f32;
                let (i, f) = (x.floor() as usize, x.fract());
                let (p, q) = (path[i], path[(i + 1).min(path.len() - 1)]);
                [p[0] + (q[0] - p[0]) * f, p[1] + (q[1] - p[1]) * f, p[2] + (q[2] - p[2]) * f]
            };
            let colour = self.graph.nodes[b].category.colour();
            for k in 0..6 {
                let u = t - k as f32 * 0.03;
                if u < 0.0 {
                    continue;
                }
                let (p, d) = self.project(at(u), centre, radius);
                let fade = (1.0 - k as f32 / 6.0) * fog(d);
                painter.circle_filled(p, (3.2 - k as f32 * 0.4) * scale.min(1.4), additive(colour, 0.9 * fade));
            }
        }

        // Nodes, back to front.
        let mut order: Vec<usize> = (0..self.graph.nodes.len()).collect();
        order.sort_by(|a, b| screen[*b].1.total_cmp(&screen[*a].1));
        let label_font = FontId::monospace((9.5 * scale.sqrt()).clamp(8.5, 12.0));
        for i in order {
            let node = &self.graph.nodes[i];
            let (p, d) = screen[i];
            let r = Self::node_px(node.loc, scale);
            let hot = glow(node.name);
            let focused = focus == Some(i);
            let near = neighbours.contains(&i);
            let dim = if focus.is_some() && !focused && !near { 0.3 } else { 1.0 };
            let colour = node.category.colour();
            if hot > 0.0 || focused {
                let g = hot.max(if focused { 0.7 } else { 0.0 });
                painter.circle_filled(p, r * 3.2, additive(colour, 0.18 * g));
                painter.circle_filled(p, r * 1.8, additive(colour, 0.35 * g));
            }
            painter.circle_filled(p, r, colour.gamma_multiply(0.85 * fog(d) * dim));
            painter.circle_stroke(p, r + 1.5, Stroke::new(if focused || near { 1.2f32 } else { 0.6 }, Color32::WHITE.gamma_multiply((if focused || near { 0.9 } else { 0.25 }) * fog(d) * dim)));
            let labelled = focused || near || hot > 0.05 || (d < -0.2 && (node.loc > 700 || self.zoom > 1.7));
            if labelled {
                painter.text(p + vec2(r + 4.0, 0.0), Align2::LEFT_CENTER, node.name, label_font.clone(), Color32::WHITE.gamma_multiply((0.55 + 0.45 * hot.max(if focused { 1.0 } else { 0.0 })) * fog(d).max(0.5) * dim.max(0.6)));
            }
        }

        if let Some(i) = focus {
            self.readout(&painter, rect, i, now);
        }
        self.scale_slider(ui, rect);
        painter.text(rect.left_top() + vec2(4.0, 2.0), Align2::LEFT_TOP, format!("CORE // {} MODULES · {} LINKS · DRAG TO ORBIT · WHEEL TO ZOOM", self.graph.nodes.len(), self.graph.edges.len()), FontId::monospace(10.0), Color32::from_rgb(0x8F, 0xB8, 0xC8));
        if hot_anything(&self.graph, now) || self.velocity.length() > 0.01 || (self.zoom - self.zoom_target).abs() > 1e-3 || self.spin.abs() > 0.0 {
            ui.ctx().request_repaint_after(Duration::from_millis(16));
        }
    }

    /// The technical readout of the focused file.
    fn readout(&self, painter: &egui::Painter, rect: Rect, i: usize, now: Instant) {
        let node = &self.graph.nodes[i];
        let names: Vec<&str> = self.graph.neighbours(i).iter().map(|j| self.graph.nodes[*j].name).collect();
        let last = match CoreActivityBus::last_active(node.name) {
            Some(t) => format!("{:.1} S AGO", now.duration_since(t).as_secs_f32()),
            None => "NOT THIS SESSION".into(),
        };
        let shown: Vec<&str> = names.iter().take(8).copied().collect();
        let more = names.len().saturating_sub(shown.len());
        let rows = [
            ("FILE //", format!("src/{}", node.name)),
            ("LINES OF CODE", node.loc.to_string()),
            ("CATEGORY", node.category.label().to_string()),
            ("CONNECTED MODULES", names.len().to_string()),
            ("", format!("{}{}", shown.join(", "), if more > 0 { format!(" +{more}") } else { String::new() })),
            ("LAST EXECUTION", last),
        ];
        let block = Rect::from_min_size(rect.left_top() + vec2(4.0, 22.0), vec2(330.0, 132.0));
        painter.rect_filled(block, Rounding::ZERO, crate::hud_board::BLOCK_FILL);
        painter.rect_stroke(block, Rounding::ZERO, Stroke::new(1.0f32, crate::hud_board::active_colour()));
        crate::hud_board::brackets(painter, block, crate::hud_board::bracket_colour());
        let clip = painter.with_clip_rect(block.shrink(4.0));
        let mut y = block.top() + 10.0;
        for (label, value) in rows {
            if label.is_empty() {
                let galley = clip.layout(value, FontId::proportional(11.0), Color32::from_rgb(0xDD, 0xF8, 0xFF), block.width() - 24.0);
                let h = galley.size().y.min(30.0);
                clip.galley(pos2(block.left() + 12.0, y), galley, Color32::WHITE);
                y += h + 4.0;
                continue;
            }
            clip.text(pos2(block.left() + 12.0, y), Align2::LEFT_TOP, label, FontId::monospace(10.0), Color32::from_rgb(0x8F, 0xB8, 0xC8));
            clip.text(pos2(block.right() - 12.0, y), Align2::RIGHT_TOP, value, FontId::monospace(10.5), Color32::WHITE);
            y += 16.0;
        }
    }

    /// `[ CORE SCALE: 150 % ]`: a slider in the bottom-right of the view (clear of the docked
    /// emblem).
    fn scale_slider(&mut self, ui: &mut Ui, rect: Rect) {
        let track = Rect::from_min_size(pos2(rect.right() - 230.0, rect.bottom() - 30.0), vec2(150.0, 14.0));
        let painter = ui.painter_at(rect);
        let label = format!("CORE SCALE: {:.0} %", self.zoom_target * 100.0);
        painter.text(pos2(track.left(), track.top() - 4.0), Align2::LEFT_BOTTOM, label, FontId::monospace(10.0), Color32::from_rgb(0xD4, 0xEC, 0xF5));
        painter.rect_stroke(track, Rounding::ZERO, Stroke::new(1.0f32, crate::hud_board::border_colour()));
        let t = (self.zoom_target - MIN_ZOOM) / (MAX_ZOOM - MIN_ZOOM);
        painter.rect_filled(Rect::from_min_size(track.min, vec2(track.width() * t, track.height())), Rounding::ZERO, crate::hud_board::border_colour().gamma_multiply(0.5));
        let knob = Rect::from_center_size(pos2(track.left() + track.width() * t, track.center().y), vec2(6.0, 18.0));
        painter.rect_filled(knob, Rounding::ZERO, Color32::WHITE);
        let response = ui.interact(track.expand(4.0), ui.id().with("core_scale"), Sense::click_and_drag());
        if let Some(p) = response.interact_pointer_pos().filter(|_| response.dragged() || response.clicked()) {
            let t = ((p.x - track.left()) / track.width()).clamp(0.0, 1.0);
            self.set_zoom(MIN_ZOOM + t * (MAX_ZOOM - MIN_ZOOM));
        }
    }
}

/// A premultiplied colour with zero alpha (adds light).
fn additive(c: Color32, amount: f32) -> Color32 {
    let a = amount.clamp(0.0, 1.0);
    Color32::from_rgba_premultiplied((c.r() as f32 * a) as u8, (c.g() as f32 * a) as u8, (c.b() as f32 * a) as u8, 0)
}

/// Whether any module is still glowing or a packet is in flight.
fn hot_anything(graph: &CoreGraph, now: Instant) -> bool {
    !CoreActivityBus::packets(now).is_empty() || graph.nodes.iter().any(|n| CoreActivityBus::last_active(n.name).is_some_and(|t| now.duration_since(t).as_secs_f32() < GLOW_SECS))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_graph_holds_every_source_file_with_real_dependencies() {
        let g = CoreGraph::build();
        let on_disk = std::fs::read_dir("src").unwrap().flatten().filter(|e| e.path().extension().is_some_and(|x| x == "rs")).count();
        assert_eq!(g.nodes.len(), on_disk, "one node per src/*.rs file");
        for name in ["main.rs", "brain.rs", "tts.rs", "fish_zero_shot_tts.rs", "core_sphere_3d.rs", "hud_board.rs", "rose_emblem_renderer.rs", "webuntis_client.rs", "music.rs"] {
            assert!(g.index(name).is_some(), "{name}");
        }
        let brain = g.index("brain.rs").unwrap();
        assert!(g.nodes[brain].loc > 1000);
        let uses = |a: &str, b: &str| {
            let (a, b) = (g.index(a).unwrap(), g.index(b).unwrap());
            g.edges.iter().any(|(x, y, _)| *x == a && *y == b)
        };
        assert!(uses("brain.rs", "tools.rs"), "brain uses tools");
        assert!(uses("jarvis_voice_dsp.rs", "fish_zero_shot_tts.rs"), "the chain calls the transfer");
        assert!(uses("main.rs", "gui.rs"), "main declares every module");
        assert!(uses("gui_widgets.rs", "gui.rs"), "the GUI's path modules reach gui.rs through super::");
        assert!(g.neighbours(brain).len() > 10);
    }

    #[test]
    fn layout_puts_orchestrators_inside_and_spreads_the_rest_evenly() {
        let g = CoreGraph::build();
        for n in &g.nodes {
            let r = length(n.pos);
            if INNER_FILES.contains(&n.name) {
                assert!((r - INNER_RADIUS).abs() < 1e-4, "{} on the inner shell", n.name);
            } else {
                assert!((r - 1.0).abs() < 1e-4, "{} on the outer shell", n.name);
            }
        }
        // Fibonacci lattice: no two outer points closer than a fraction of the mean spacing.
        let pts = fibonacci_sphere(120, 1.0);
        let mut min = f32::MAX;
        for i in 0..pts.len() {
            for j in i + 1..pts.len() {
                let d = [pts[i][0] - pts[j][0], pts[i][1] - pts[j][1], pts[i][2] - pts[j][2]];
                min = min.min(length(d));
            }
        }
        let mean_spacing = (4.0 * PI / 120.0).sqrt();
        assert!(min > 0.6 * mean_spacing, "{min} vs {mean_spacing}");
    }

    #[test]
    fn splines_run_from_node_to_node_and_bulge_outwards() {
        let (a, b) = ([1.0, 0.0, 0.0], [0.0, 1.0, 0.0]);
        let s = arc(a, b, 10);
        assert_eq!(s.len(), 11);
        assert!(length([s[0][0] - 1.0, s[0][1], s[0][2]]) < 1e-5 && length([s[10][0], s[10][1] - 1.0, s[10][2]]) < 1e-5);
        assert!(length(s[5]) > 1.1, "bulges outwards");
        // Antipodal points still give a finite path.
        assert!(arc([1.0, 0.0, 0.0], [-1.0, 0.0, 0.0], 8).iter().all(|p| p.iter().all(|v| v.is_finite())));
    }

    #[test]
    fn rotation_zoom_and_projection() {
        let mut v = CoreSphereView::default();
        let c = pos2(500.0, 400.0);
        v.yaw = 0.0;
        v.pitch = 0.0;
        let (front, d_front) = v.project([0.0, 0.0, -1.0], c, 200.0);
        let (back, d_back) = v.project([0.0, 0.0, 1.0], c, 200.0);
        assert!(d_front < 0.0 && d_back > 0.0 && (front - c).length() < 1e-3 && (back - c).length() < 1e-3);
        let (right, _) = v.project([1.0, 0.0, 0.0], c, 200.0);
        assert!((right.x - 700.0).abs() < 1e-3);
        // A quarter turn of yaw brings +x round to the front, in the middle of the view.
        v.yaw = std::f32::consts::FRAC_PI_2;
        let (p, d) = v.project([1.0, 0.0, 0.0], c, 200.0);
        assert!(d < -0.99 && (p.x - c.x).abs() < 1e-3, "{p:?} {d}");
        // Zoom: clamped to 50-300 % and eased in.
        v.set_zoom(10.0);
        assert_eq!(v.zoom_target(), MAX_ZOOM);
        v.apply(CoreCommand::Zoom(0.1));
        assert_eq!(v.zoom_target(), MIN_ZOOM);
        v.apply(CoreCommand::Zoom(2.0));
        for _ in 0..60 {
            v.step(1.0 / 60.0);
        }
        assert!((v.zoom - 2.0).abs() < 0.01);
        v.apply(CoreCommand::ZoomBy(1.2));
        assert!((v.zoom_target() - 2.4).abs() < 1e-4);
        // Inertia dies away; spin can stop.
        v.velocity = vec2(2.0, 0.0);
        v.apply(CoreCommand::Spin(0.0));
        let start = v.yaw;
        for _ in 0..240 {
            v.step(1.0 / 60.0);
        }
        assert!(v.velocity.length() < 0.01 && v.yaw != start);
    }

    #[test]
    fn pulses_light_modules_and_chain_packets_in_order() {
        let t0 = Instant::now();
        let mut bus = BusState::default();
        bus.pulse_at("ears.rs", t0);
        bus.pulse_at("stt.rs", t0 + Duration::from_millis(100));
        bus.pulse_at("intents.rs", t0 + Duration::from_millis(150));
        bus.pulse_at("brain.rs", t0 + Duration::from_millis(200));
        assert!(bus.last.contains_key("brain.rs"));
        let flying = bus.packets(t0 + Duration::from_millis(320));
        assert!(flying.iter().any(|(a, b, _)| *a == "ears.rs" && *b == "stt.rs"), "{flying:?}");
        assert!(!flying.iter().any(|(a, b, _)| *a == "intents.rs" && *b == "brain.rs"), "the last packet waits its turn");
        // The later packets of the chain leave after the earlier ones.
        let later = bus.packets(t0 + Duration::from_millis(700));
        assert!(later.iter().any(|(a, b, _)| *a == "intents.rs" && *b == "brain.rs"), "{later:?}");
        // A pulse long after the chain starts a new one (no packet).
        let mut quiet = BusState::default();
        quiet.pulse_at("music.rs", t0);
        quiet.pulse_at("brain.rs", t0 + Duration::from_secs(5));
        assert!(quiet.packets.is_empty());
        CoreActivityBus::pulse("tools.rs");
        assert!(CoreActivityBus::last_active("tools.rs").is_some());
        assert_eq!(CoreActivityBus::module_for_tool("control_music"), Some("music.rs"));
        assert_eq!(CoreActivityBus::module_for_tool("check_school_schedule"), Some("webuntis_client.rs"));
        assert_eq!(CoreActivityBus::module_for_tool("reboot_language_model"), Some("llm_reboot.rs"));
        assert_eq!(category("brain.rs"), Category::Core);
        assert_eq!(category("fish_zero_shot_tts.rs"), Category::Voice);
        assert_eq!(category("hud_board.rs"), Category::Interface);
        assert_eq!(category("webuntis_client.rs"), Category::Integration);
    }

    #[test]
    fn dragging_turns_the_front_face_with_the_pointer_and_hover_holds_the_spin() {
        let c = pos2(500.0, 400.0);
        let front = |v: &CoreSphereView| v.project([0.0, 0.0, -1.0], c, 200.0).0;
        let mut v = CoreSphereView { yaw: 0.0, pitch: 0.0, ..CoreSphereView::default() };
        // The old sign: dragging right moved the front face left.
        let mut old = CoreSphereView { yaw: 0.0, pitch: 0.0, ..CoreSphereView::default() };
        old.yaw += 40.0 * 0.006;
        assert!(front(&old).x < c.x, "reproduces the reversed drag");
        v.drag(vec2(40.0, 0.0), 1.0 / 60.0);
        assert!(front(&v).x > c.x + 5.0, "drag right: the face turns right");
        let mut v = CoreSphereView { yaw: 0.0, pitch: 0.0, ..CoreSphereView::default() };
        v.drag(vec2(0.0, -40.0), 1.0 / 60.0);
        assert!(front(&v).y < c.y - 5.0, "drag up: it tilts up");
        // Hovering holds the auto-rotation (inertia aside).
        let mut v = CoreSphereView { velocity: Vec2::ZERO, hover_hold: true, ..CoreSphereView::default() };
        let yaw = v.yaw;
        for _ in 0..60 {
            v.step(1.0 / 60.0);
        }
        assert_eq!(v.yaw, yaw, "a node stays under the cursor");
        v.hover_hold = false;
        v.step(1.0 / 60.0);
        assert_ne!(v.yaw, yaw);
    }
}
