//! The Stark Investigative Pinboard: a force-directed citation and concept graph of a Deep
//! Research mission, painted with `egui::Painter`.
//!
//! The topic is the hub, sub-questions branch from it, the crawled sources hang off the question
//! that found them, and the extracted facts are the leaves. Edges are coloured by evidence:
//! emerald-cyan for facts two or more sites agree on, amber for single-source claims and a
//! pulsing crimson for contradictions between sources. Drag to pan, scroll to zoom, drag a node
//! to move it, click a fact or source to inspect it (and highlight it in the dossier).

use crate::deep_research_engine::{FactRow, MissionSnapshot};
use eframe::egui::{self, pos2, vec2, Align2, Color32, Pos2, Rect, Sense, Stroke, Ui, Vec2};
use std::collections::HashMap;

use crate::holo::{self, Face};

/// Verified by several sources: nominal, so plain ink.
pub fn verified() -> Color32 {
    holo::theme().ink()
}
/// A claim only one source makes.
pub fn single() -> Color32 {
    holo::theme().warn()
}
/// Sources contradict each other.
pub fn contradiction() -> Color32 {
    holo::theme().alert()
}
fn hub() -> Color32 {
    holo::theme().accent()
}
fn question() -> Color32 {
    holo::theme().accent()
}
fn source() -> Color32 {
    holo::theme().deep()
}

const MAX_SOURCES: usize = 45;
const MAX_FACTS: usize = 140;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NodeKind {
    Hub,
    Question,
    Source,
    Fact,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EdgeKind {
    Structure,
    Verified,
    Single,
    Contradiction,
}

impl EdgeKind {
    fn of(fact: &FactRow) -> Self {
        if fact.contradicting_source.is_some() {
            EdgeKind::Contradiction
        } else if fact.corroborating > 0 {
            EdgeKind::Verified
        } else {
            EdgeKind::Single
        }
    }

    fn colour(self) -> Color32 {
        match self {
            EdgeKind::Structure => holo::a(holo::theme().ink(), 0.45),
            EdgeKind::Verified => verified(),
            EdgeKind::Single => single(),
            EdgeKind::Contradiction => contradiction(),
        }
    }

    fn rest_length(self) -> f32 {
        match self {
            EdgeKind::Structure => 120.0,
            _ => 55.0,
        }
    }
}

#[derive(Debug, Clone)]
pub struct GraphNode {
    pub kind: NodeKind,
    /// Fact, source or query id (0 for the hub).
    pub ref_id: i64,
    pub label: String,
    /// For facts: the source; for sources: the URL.
    pub detail: String,
    pub pos: Vec2,
    vel: Vec2,
    pinned: bool,
    pub colour: Color32,
    pub radius: f32,
}

#[derive(Debug, Clone, Copy)]
pub struct GraphEdge {
    pub a: usize,
    pub b: usize,
    pub kind: EdgeKind,
}

/// What Sir clicked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraphPick {
    Hub,
    Question(i64),
    Source(i64),
    Fact { fact_id: i64, source_id: i64 },
}

pub struct ResearchGraphView {
    pub nodes: Vec<GraphNode>,
    pub edges: Vec<GraphEdge>,
    index: HashMap<(NodeKind, i64), usize>,
    pan: Vec2,
    zoom: f32,
    dragging: Option<usize>,
    pub selected: Option<usize>,
    /// (facts, sources, queries) of the last sync.
    synced: (usize, usize, usize, i64),
    /// Layout energy of the last step; the view stops animating once it is calm.
    energy: f32,
}

impl Default for ResearchGraphView {
    fn default() -> Self {
        Self::new()
    }
}

/// A stable pseudo-random offset per key (new nodes appear near their parent, never stacked).
fn jitter(kind: NodeKind, id: i64) -> Vec2 {
    let mut h = (id as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ (kind as u64).wrapping_mul(0xC2B2_AE3D_27D4_EB4F);
    h ^= h >> 29;
    let a = (h & 0xFFFF) as f32 / 65535.0 * std::f32::consts::TAU;
    let r = 20.0 + ((h >> 16) & 0xFF) as f32 / 255.0 * 30.0;
    vec2(a.cos() * r, a.sin() * r)
}

/// The node under `pos` (screen), nearest first.
fn hit_test(nodes: &[GraphNode], origin: Pos2, zoom: f32, pos: Pos2) -> Option<usize> {
    let dist = |n: &GraphNode| (origin + n.pos * zoom - pos).length();
    nodes
        .iter()
        .enumerate()
        .filter(|(_, n)| dist(n) <= (n.radius * zoom).max(6.0) + 3.0)
        .min_by(|a, b| dist(a.1).partial_cmp(&dist(b.1)).unwrap_or(std::cmp::Ordering::Equal))
        .map(|(i, _)| i)
}

fn short(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        format!("{}…", s.chars().take(n).collect::<String>())
    }
}

impl ResearchGraphView {
    pub fn new() -> Self {
        Self {
            nodes: Vec::new(),
            edges: Vec::new(),
            index: HashMap::new(),
            pan: Vec2::ZERO,
            zoom: 0.9,
            dragging: None,
            selected: None,
            synced: (usize::MAX, 0, 0, 0),
            energy: f32::MAX,
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn node(&mut self, kind: NodeKind, ref_id: i64, near: Vec2, label: String, detail: String, colour: Color32, radius: f32) -> usize {
        if let Some(&i) = self.index.get(&(kind, ref_id)) {
            let n = &mut self.nodes[i];
            n.label = label;
            n.detail = detail;
            n.colour = colour;
            n.radius = radius;
            return i;
        }
        let i = self.nodes.len();
        self.nodes.push(GraphNode { kind, ref_id, label, detail, pos: if kind == NodeKind::Hub { near } else { near + jitter(kind, ref_id) }, vel: Vec2::ZERO, pinned: kind == NodeKind::Hub, colour, radius });
        self.index.insert((kind, ref_id), i);
        i
    }

    /// Rebuilds the node/edge sets from the mission (positions of known nodes are kept).
    pub fn sync(&mut self, snap: &MissionSnapshot) {
        let key = (snap.facts.len(), snap.sources.len(), snap.queries.len(), snap.mission.id);
        if key == self.synced {
            return;
        }
        if self.synced.3 != snap.mission.id {
            *self = Self::new();
        }
        self.synced = key;
        let old: HashMap<(NodeKind, i64), Vec2> = self.nodes.iter().map(|n| ((n.kind, n.ref_id), n.pos)).collect();
        let selected = self.selected.map(|i| (self.nodes[i].kind, self.nodes[i].ref_id));
        self.nodes.clear();
        self.edges.clear();
        self.index.clear();
        let hub = self.node(NodeKind::Hub, 0, Vec2::ZERO, snap.mission.topic.clone(), snap.mission.status.clone(), hub(), 16.0);
        let mut qpos: HashMap<i64, usize> = HashMap::new();
        let nq = snap.queries.len().max(1) as f32;
        for (i, q) in snap.queries.iter().enumerate().take(60) {
            let angle = i as f32 / nq * std::f32::consts::TAU;
            let near = vec2(angle.cos(), angle.sin()) * 170.0;
            let colour = if q.status == "done" { question() } else { holo::theme().deep() };
            let n = self.node(NodeKind::Question, q.id, near, q.sub_question.clone(), q.search_query.clone(), colour, 7.0);
            self.edges.push(GraphEdge { a: hub, b: n, kind: EdgeKind::Structure });
            qpos.insert(q.id, n);
        }
        // The best sources that produced facts first.
        let mut with_facts: HashMap<i64, usize> = HashMap::new();
        for f in &snap.facts {
            *with_facts.entry(f.source_id).or_default() += 1;
        }
        let mut sources: Vec<_> = snap.sources.iter().collect();
        sources.sort_by(|a, b| {
            let fa = with_facts.get(&a.id).copied().unwrap_or(0);
            let fb = with_facts.get(&b.id).copied().unwrap_or(0);
            fb.cmp(&fa).then(b.reliability.partial_cmp(&a.reliability).unwrap_or(std::cmp::Ordering::Equal))
        });
        let mut spos: HashMap<i64, usize> = HashMap::new();
        for s in sources.into_iter().take(MAX_SOURCES) {
            let parent = s.query_id.and_then(|q| qpos.get(&q).copied()).unwrap_or(hub);
            let near = self.nodes[parent].pos * 1.5;
            let colour = holo::mix(source(), verified(), (s.reliability - 0.5).max(0.0) * 2.0);
            let n = self.node(NodeKind::Source, s.id, near, s.domain.clone(), format!("{}\n{}\nreliability {:.2}", s.title, s.url, s.reliability), colour, 5.0 + s.reliability * 4.0);
            self.edges.push(GraphEdge { a: parent, b: n, kind: EdgeKind::Structure });
            spos.insert(s.id, n);
        }
        for f in snap.facts.iter().filter(|f| spos.contains_key(&f.source_id)).take(MAX_FACTS) {
            let parent = spos[&f.source_id];
            let kind = EdgeKind::of(f);
            let near = self.nodes[parent].pos * 1.15;
            let source = snap.source(f.source_id).map(|s| format!("{} (reliability {:.2})\n{}", s.domain, s.reliability, s.url)).unwrap_or_default();
            let n = self.node(NodeKind::Fact, f.id, near, f.claim.clone(), source, kind.colour(), 3.0 + f.confidence * 2.5);
            self.edges.push(GraphEdge { a: parent, b: n, kind });
            if let Some(other) = f.contradicting_source.and_then(|s| spos.get(&s).copied()) {
                self.edges.push(GraphEdge { a: n, b: other, kind: EdgeKind::Contradiction });
            }
        }
        for n in &mut self.nodes {
            if let Some(p) = old.get(&(n.kind, n.ref_id)) {
                n.pos = *p;
            }
        }
        self.selected = selected.and_then(|k| self.index.get(&k).copied());
        self.energy = f32::MAX;
    }

    /// One step of the force layout (repulsion, edge springs, a weak pull to the centre).
    pub fn step(&mut self, dt: f32) {
        let n = self.nodes.len();
        if n == 0 {
            return;
        }
        let dt = dt.clamp(0.0, 1.0 / 30.0);
        let mut force = vec![Vec2::ZERO; n];
        for i in 0..n {
            for j in (i + 1)..n {
                let d = self.nodes[i].pos - self.nodes[j].pos;
                let dist2 = d.length_sq().max(25.0);
                let push = d / dist2.sqrt() * (9000.0 / dist2);
                force[i] += push;
                force[j] -= push;
            }
        }
        for e in &self.edges {
            let d = self.nodes[e.b].pos - self.nodes[e.a].pos;
            let len = d.length().max(0.01);
            let pull = d / len * (len - e.kind.rest_length()) * 2.2;
            force[e.a] += pull;
            force[e.b] -= pull;
        }
        let mut energy = 0.0;
        for (i, node) in self.nodes.iter_mut().enumerate() {
            if node.pinned || Some(i) == self.dragging {
                node.vel = Vec2::ZERO;
                continue;
            }
            let f = force[i] - node.pos * 0.35;
            node.vel = (node.vel + f * dt * 80.0) * 0.7;
            let speed = node.vel.length();
            if speed > 600.0 {
                node.vel *= 600.0 / speed;
            }
            node.pos += node.vel * dt;
            energy += node.vel.length_sq();
        }
        self.energy = energy / n as f32;
    }

    /// Whether the layout still moves (the window keeps repainting at 60 FPS).
    pub fn animating(&self) -> bool {
        self.energy > 4.0 || self.dragging.is_some()
    }

    pub fn select_fact(&mut self, fact_id: i64) {
        self.selected = self.index.get(&(NodeKind::Fact, fact_id)).copied();
    }

    /// Paints the pinboard into `ui` and handles pan, zoom, node dragging and picks.
    pub fn show(&mut self, ui: &mut Ui, accent: Color32) -> Option<GraphPick> {
        let rect = ui.available_rect_before_wrap();
        let response = ui.allocate_rect(rect, Sense::click_and_drag());
        let painter = ui.painter_at(rect);
        let t = ui.input(|i| i.time) as f32;
        let dt = ui.input(|i| i.stable_dt);
        let hover = response.hover_pos();

        // Zoom around the cursor.
        if let Some(cursor) = hover {
            let scroll = ui.input(|i| i.smooth_scroll_delta.y);
            if scroll != 0.0 {
                let world = (cursor - (rect.center() + self.pan)) / self.zoom;
                self.zoom = (self.zoom * (scroll * 0.0018).exp()).clamp(0.15, 4.0);
                self.pan = cursor - rect.center() - world * self.zoom;
            }
        }
        // Drag a node, or pan.
        if response.drag_started() {
            self.dragging = response.interact_pointer_pos().and_then(|p| hit_test(&self.nodes, rect.center() + self.pan, self.zoom, p));
        }
        if response.dragged() {
            let delta = response.drag_delta();
            match self.dragging {
                Some(i) => self.nodes[i].pos += delta / self.zoom,
                None => self.pan += delta,
            }
        }
        if response.drag_stopped() {
            self.dragging = None;
        }
        let mut pick = None;
        if response.clicked() {
            if let Some(i) = response.interact_pointer_pos().and_then(|p| hit_test(&self.nodes, rect.center() + self.pan, self.zoom, p)) {
                self.selected = Some(i);
                let n = &self.nodes[i];
                pick = Some(match n.kind {
                    NodeKind::Hub => GraphPick::Hub,
                    NodeKind::Question => GraphPick::Question(n.ref_id),
                    NodeKind::Source => GraphPick::Source(n.ref_id),
                    NodeKind::Fact => {
                        let source_id = self.edges.iter().find(|e| e.b == i && self.nodes[e.a].kind == NodeKind::Source).map(|e| self.nodes[e.a].ref_id).unwrap_or(0);
                        GraphPick::Fact { fact_id: n.ref_id, source_id }
                    }
                });
            } else {
                self.selected = None;
            }
        }
        self.step(dt);
        let origin = rect.center() + self.pan;
        let zoom = self.zoom;
        let to_screen = |p: Vec2| origin + p * zoom;

        // Faint grid: the pinboard.
        let grid = 48.0 * zoom;
        if grid > 12.0 {
            let ink = holo::a(holo::theme().ink(), 0.05);
            let mut x = rect.left() + (origin.x - rect.left()).rem_euclid(grid);
            while x < rect.right() {
                painter.line_segment([pos2(x, rect.top()), pos2(x, rect.bottom())], Stroke::new(1.0_f32, ink));
                x += grid;
            }
            let mut y = rect.top() + (origin.y - rect.top()).rem_euclid(grid);
            while y < rect.bottom() {
                painter.line_segment([pos2(rect.left(), y), pos2(rect.right(), y)], Stroke::new(1.0_f32, ink));
                y += grid;
            }
        }
        let pulse = 0.55 + 0.45 * (t * 4.0).sin();
        let highlighted = self.selected;
        for e in &self.edges {
            let (a, b) = (to_screen(self.nodes[e.a].pos), to_screen(self.nodes[e.b].pos));
            let near_selection = highlighted.is_some_and(|s| s == e.a || s == e.b);
            let base = e.kind.colour();
            let alpha = match e.kind {
                EdgeKind::Structure => 0.35,
                EdgeKind::Contradiction => pulse,
                _ => 0.8,
            } * if near_selection { 1.0 } else if highlighted.is_some() { 0.45 } else { 1.0 };
            let width = (if e.kind == EdgeKind::Structure { 1.0 } else { 1.4 }) * zoom.clamp(0.6, 1.6);
            if e.kind != EdgeKind::Structure {
                // Glow underneath.
                painter.line_segment([a, b], Stroke::new(width * 3.5, base.gamma_multiply(alpha * 0.18)));
            }
            painter.line_segment([a, b], Stroke::new(width, base.gamma_multiply(alpha)));
        }
        let hovered = hover.and_then(|p| hit_test(&self.nodes, origin, zoom, p));
        for (i, n) in self.nodes.iter().enumerate() {
            let c = to_screen(n.pos);
            if !rect.expand(40.0).contains(c) {
                continue;
            }
            let r = (n.radius * zoom).max(2.0);
            if Some(i) == highlighted {
                painter.circle_stroke(c, r + 6.0, Stroke::new(1.6_f32, accent.gamma_multiply(0.6 + 0.4 * pulse)));
            }
            painter.circle_filled(c, r * 1.9, n.colour.gamma_multiply(0.12));
            painter.circle_filled(c, r, n.colour);
            if n.kind == NodeKind::Hub {
                painter.circle_stroke(c, r + 4.0 + 2.0 * pulse, Stroke::new(1.2_f32, holo::a(hub(), 0.5)));
            }
            let show_label = match n.kind {
                NodeKind::Hub => true,
                NodeKind::Question => zoom > 0.55,
                NodeKind::Source => zoom > 0.75,
                NodeKind::Fact => zoom > 2.2 || Some(i) == hovered,
            };
            if show_label {
                let (text, face, size) = match n.kind {
                    NodeKind::Hub => (short(&n.label, 40), Face::Head, 14.0),
                    NodeKind::Question => (short(&n.label, 34), Face::Mono, 10.5),
                    NodeKind::Source => (short(&n.label, 26), Face::Mono, 10.0),
                    NodeKind::Fact => (short(&n.label, 60), Face::Mono, 10.0),
                };
                let colour = if n.kind == NodeKind::Hub { hub() } else { holo::theme().ink() };
                holo::text(&painter, c + vec2(r + 4.0, 0.0), Align2::LEFT_CENTER, face, &text, size, colour);
            }
        }
        if let Some(i) = hovered {
            let n = &self.nodes[i];
            response.clone().on_hover_ui_at_pointer(|ui| {
                ui.set_max_width(360.0);
                ui.label(egui::RichText::new(&n.label).font(holo::font(Face::Body, 13.0)).color(holo::theme().ink()));
                if !n.detail.is_empty() {
                    ui.label(egui::RichText::new(&n.detail).font(holo::font(Face::Mono, 10.5)).color(holo::theme().muted()));
                }
            });
        }
        // Legend.
        let mut y = rect.top() + 10.0;
        for (label, colour) in [("Verified (2+ sources)", verified()), ("Single source", single()), ("Contradiction", contradiction())] {
            painter.circle_filled(pos2(rect.left() + 14.0, y + 6.0), 4.0, colour);
            holo::text(&painter, pos2(rect.left() + 24.0, y + 6.0), Align2::LEFT_CENTER, Face::Label, label, 8.5, holo::theme().muted());
            y += 16.0;
        }
        holo::text(&painter, rect.right_bottom() + vec2(-8.0, -6.0), Align2::RIGHT_BOTTOM, Face::Mono, &format!("{} nodes · zoom {:.0}%", self.nodes.len(), self.zoom * 100.0), 10.0, holo::theme().faint());
        // The selected node's card.
        if let Some(n) = highlighted.and_then(|i| self.nodes.get(i)) {
            let card = Rect::from_min_size(pos2(rect.left() + 10.0, rect.bottom() - 96.0), vec2((rect.width() - 20.0).min(520.0), 86.0));
            crate::holo_frame::card(&painter, card, 1.0, 1.0);
            let kind = match n.kind {
                NodeKind::Hub => "TOPIC",
                NodeKind::Question => "SUB-QUESTION",
                NodeKind::Source => "SOURCE",
                NodeKind::Fact => "FACT",
            };
            let t = holo::theme();
            holo::text(&painter, card.min + vec2(10.0, 8.0), Align2::LEFT_TOP, Face::Label, kind, 8.5, n.colour);
            holo::wrapped(&painter, card.min + vec2(10.0, 24.0), card.width() - 20.0, Face::Body, &short(&n.label, 220), 12.0, t.ink(), 2);
            holo::text(&painter, card.left_bottom() + vec2(10.0, -8.0), Align2::LEFT_BOTTOM, Face::Mono, &short(n.detail.lines().next().unwrap_or(""), 90), 9.5, t.muted());
        }
        if self.animating() {
            ui.ctx().request_repaint();
        }
        pick
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::deep_research_engine::{MissionRow, QueryRow, SourceRow};

    fn snapshot() -> MissionSnapshot {
        let mission = MissionRow {
            id: 1,
            topic: "solid-state batteries".into(),
            status: "running".into(),
            created_at: String::new(),
            target_secs: 3600,
            elapsed_secs: 60,
            pages: 2,
            facts: 3,
            executive_summary: String::new(),
            focus_areas: String::new(),
        };
        let queries = vec![QueryRow { id: 1, sub_question: "What is it?".into(), search_query: "ssb overview".into(), status: "done".into(), priority: 100 }];
        let src = |id: i64, domain: &str| SourceRow { id, url: format!("https://{domain}/x"), domain: domain.into(), title: domain.into(), reliability: 0.8, query_id: Some(1), crawled_at: String::new(), depth: 1 };
        let fact = |id: i64, source_id: i64, corroborating: i64, contradicting_source: Option<i64>| FactRow {
            id,
            source_id,
            category: "statistic".into(),
            claim: format!("claim {id}"),
            confidence: 0.7,
            corroborating,
            contradicting_source,
        };
        MissionSnapshot { mission, queries, sources: vec![src(10, "a.org"), src(11, "b.org")], facts: vec![fact(1, 10, 1, None), fact(2, 11, 0, None), fact(3, 11, 0, Some(10))] }
    }

    #[test]
    fn builds_hub_questions_sources_facts_and_coloured_edges() {
        let mut g = ResearchGraphView::new();
        g.sync(&snapshot());
        assert_eq!(g.nodes.len(), 1 + 1 + 2 + 3);
        let kinds: Vec<EdgeKind> = g.edges.iter().map(|e| e.kind).collect();
        assert!(kinds.contains(&EdgeKind::Verified));
        assert!(kinds.contains(&EdgeKind::Single));
        assert_eq!(kinds.iter().filter(|k| **k == EdgeKind::Contradiction).count(), 2);
        // Layout settles and keeps nodes apart.
        for _ in 0..600 {
            g.step(1.0 / 60.0);
        }
        assert!(!g.animating());
        let a = g.nodes[2].pos;
        let b = g.nodes[3].pos;
        assert!((a - b).length() > 10.0);
        // A re-sync with the same data keeps positions.
        let before = g.nodes[4].pos;
        g.sync(&snapshot());
        assert_eq!(g.nodes[4].pos, before);
    }
}
