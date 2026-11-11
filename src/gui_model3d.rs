//! The 3D STUDIO block (`crate::model3d`): on the board, or in its own window once detached.
//!
//! The model is drawn by the GPU (`crate::gpu3d`, an egui paint callback) between the block's
//! own egui chrome: the chips on top (look, wireframe, spin, explode, view, export), the model in
//! the middle (drag to turn, right-drag to move, scroll to zoom, double-click for the home view),
//! the status, a Sketchfab preview card or the scanning lattice while a model is on its way, and
//! the credit line at the bottom. Model files dropped on the studio open in it. Without a GPU
//! path the model's edges are drawn with egui.

use super::{stroke, STRIP_INK, STRIP_MUTED};
use crate::gpu3d;
use crate::mesh_io::math;
use crate::model3d::{self as studio, CameraCmd, Look, Phase, Snapshot};
use eframe::egui::{self, pos2, vec2, Align2, Color32, ColorImage, FontId, Frame, Margin, Painter, Pos2, Rect, RichText, Rounding, Sense, TextureHandle, TextureOptions, Ui, ViewportBuilder, ViewportClass, ViewportCommand, ViewportId};
use std::f32::consts::{PI, TAU};
use std::sync::{Arc, Mutex, MutexGuard};
use tokio::runtime::Handle;

/// The block's id on the board.
pub(super) const ID: &str = "model3d";
const TITLE: &str = "3D model";
const VIEWPORT: &str = "jarvis_model3d";
/// The camera's field of view and its home: a three-quarter view from a little above.
const FOV: f32 = 35.0 * PI / 180.0;
const HOME_YAW: f32 = 0.45;
const HOME_PITCH: f32 = 0.22;
const HOME_DIST: f32 = 3.4;
const CARD: Color32 = Color32::from_rgba_premultiplied(3, 7, 12, 226);

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn mono(size: f32) -> FontId {
    FontId::monospace(super::ts(size))
}

/// The orbit camera (the panel's own; voice moves it through `model3d::take_camera`).
#[derive(Clone, Copy, Debug)]
struct Camera {
    yaw: f32,
    pitch: f32,
    dist: f32,
    target: [f32; 3],
    goal_yaw: f32,
    goal_pitch: f32,
    goal_dist: f32,
    goal_target: [f32; 3],
    /// The turntable's angle, and where it eases to after "show it from the front".
    spin: f32,
    spin_home: Option<f32>,
    last: f64,
    /// The turntable waits after Sir let go.
    idle_until: f64,
}

impl Camera {
    const fn new() -> Self {
        Camera { yaw: HOME_YAW, pitch: HOME_PITCH, dist: HOME_DIST, target: [0.0; 3], goal_yaw: HOME_YAW, goal_pitch: HOME_PITCH, goal_dist: HOME_DIST, goal_target: [0.0; 3], spin: 0.0, spin_home: None, last: 0.0, idle_until: 0.0 }
    }

    fn home(&mut self) {
        self.face(HOME_YAW, HOME_PITCH);
        self.goal_dist = HOME_DIST;
        self.goal_target = [0.0; 3];
    }

    /// Looks from `yaw`/`pitch` (the shorter way round) with the model turned to its front.
    fn face(&mut self, yaw: f32, pitch: f32) {
        let turns = ((self.yaw - yaw) / TAU).round();
        self.goal_yaw = yaw + turns * TAU;
        self.goal_pitch = pitch;
        self.spin_home = Some((self.spin / TAU).round() * TAU);
    }

    fn command(&mut self, cmd: CameraCmd) {
        match cmd {
            CameraCmd::Reset => self.home(),
            CameraCmd::Front => self.face(0.0, 0.06),
            CameraCmd::Back => self.face(PI, 0.06),
            CameraCmd::Left => self.face(-PI / 2.0, 0.06),
            CameraCmd::Right => self.face(PI / 2.0, 0.06),
            CameraCmd::Top => self.face(0.0, 1.45),
            CameraCmd::Bottom => self.face(0.0, -1.45),
            CameraCmd::Zoom(k) => self.goal_dist = (self.goal_dist * k).clamp(0.7, 14.0),
        }
    }

    /// One frame on: eases to the goals, turns the turntable. True while anything moves.
    fn step(&mut self, now: f64, spin_speed: f32, holding: bool) -> bool {
        let dt = ((now - self.last) as f32).clamp(0.0, 0.1);
        self.last = now;
        let k = 1.0 - (-dt * 9.0).exp();
        let ease = |a: &mut f32, b: f32| *a += (b - *a) * k;
        ease(&mut self.yaw, self.goal_yaw);
        ease(&mut self.pitch, self.goal_pitch);
        ease(&mut self.dist, self.goal_dist);
        for i in 0..3 {
            ease(&mut self.target[i], self.goal_target[i]);
        }
        let mut moving = (self.yaw - self.goal_yaw).abs() > 1e-3 || (self.pitch - self.goal_pitch).abs() > 1e-3 || (self.dist - self.goal_dist).abs() > 1e-3 || (0..3).any(|i| (self.target[i] - self.goal_target[i]).abs() > 1e-4);
        if !holding && now >= self.idle_until {
            if spin_speed > 0.0 {
                self.spin_home = None;
                self.spin = (self.spin + spin_speed * dt) % TAU;
                moving = true;
            } else if let Some(h) = self.spin_home {
                self.spin += (h - self.spin) * k;
                if (h - self.spin).abs() < 1e-3 {
                    self.spin = 0.0;
                    self.spin_home = None;
                }
                moving = true;
            }
        }
        moving
    }

    fn eye(&self) -> [f32; 3] {
        let (sy, cy) = self.yaw.sin_cos();
        let (sp, cp) = self.pitch.sin_cos();
        math::add(self.target, math::scale([cp * sy, sp, cp * cy], self.dist))
    }
}

static CAMERA: Mutex<Camera> = Mutex::new(Camera::new());
/// The Sketchfab card's thumbnail on the GPU (by the preview's id).
static PREVIEW: Mutex<Option<(u64, TextureHandle)>> = Mutex::new(None);
/// What the dock was last told about the block.
static DESCRIBED: Mutex<String> = Mutex::new(String::new());
/// The Integrations hub's Sketchfab field and its last answer.
static TOKEN_FIELD: Mutex<String> = Mutex::new(String::new());
static TOKEN_NOTE: Mutex<Option<(bool, String)>> = Mutex::new(None);

fn rgb(c: Color32) -> [f32; 3] {
    [c.r() as f32 / 255.0, c.g() as f32 / 255.0, c.b() as f32 / 255.0]
}

fn chip(ui: &mut Ui, text: &str, on: bool, hint: &str) -> bool {
    let accent = crate::hud_board::active_colour();
    let ink = if on { accent } else { STRIP_INK };
    let edge = if on { accent } else { crate::hud_board::border_colour() };
    ui.add(egui::Button::new(RichText::new(text).font(mono(10.5)).color(ink)).fill(CARD).stroke(stroke(1.0, edge)).rounding(Rounding::ZERO)).on_hover_text(hint).clicked()
}

/// The chips over the model.
fn toolbar(ui: &mut Ui, snap: &Snapshot, rt: &Handle) {
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = vec2(5.0, 4.0);
        let holo = snap.view.look == Look::Hologram;
        if chip(ui, "REAL", !holo, "Realistic look (\"realistic mode\")") {
            studio::set_view(|v| v.look = Look::Realistic);
        }
        if chip(ui, "HOLO", holo, "Hologram look (\"hologram mode\")") {
            studio::set_view(|v| v.look = Look::Hologram);
        }
        if chip(ui, "WIRE", snap.view.wireframe, "Show the mesh's edges (\"wireframe\")") {
            studio::set_view(|v| v.wireframe = !v.wireframe);
        }
        let spinning = snap.view.spin > 0.0;
        if chip(ui, "SPIN", spinning, "Turntable (\"spin faster\", \"stop spinning\")") {
            studio::set_view(|v| v.spin = if v.spin > 0.0 { 0.0 } else { studio::DEFAULT_SPIN });
        }
        if chip(ui, "EXPLODE", snap.view.explode > 0.0, "Pull the parts apart (\"explode it\")") {
            studio::set_view(|v| v.explode = if v.explode > 0.0 { 0.0 } else { 1.0 });
        }
        if chip(ui, "HOME", false, "The home view (or double-click the model)") {
            lock(&CAMERA).home();
        }
        if snap.scene.is_some() {
            ui.add_space(6.0);
            for (label, format) in [("STL", "stl"), ("OBJ", "obj"), ("GLB", "glb")] {
                if chip(ui, &format!("⤓ {label}"), false, &format!("Save it as {label} in Documents\\JARVIS Models (\"save it as {label}\")")) {
                    studio::export_in_background(rt, format, None);
                }
            }
            if chip(ui, "⤓ SAVE AS…", false, "Choose the folder and the name (\"save it to my desktop\", \"save it to D:\\Prints\", \"let me choose where\")") {
                studio::export_in_background(rt, "glb", Some("ask"));
            }
        }
    });
}

/// Clip space of the normalised frame, onto the view's rectangle.
fn project(m: &math::M4, p: [f32; 3], rect: Rect) -> Option<Pos2> {
    let x = m[0] * p[0] + m[4] * p[1] + m[8] * p[2] + m[12];
    let y = m[1] * p[0] + m[5] * p[1] + m[9] * p[2] + m[13];
    let w = m[3] * p[0] + m[7] * p[1] + m[11] * p[2] + m[15];
    if w <= 0.02 {
        return None;
    }
    Some(pos2(rect.center().x + x / w * rect.width() * 0.5, rect.center().y - y / w * rect.height() * 0.5))
}

/// Edges in the normalised frame, drawn with egui (the placeholder, and the model without a GPU
/// path).
fn draw_edges(painter: &Painter, rect: Rect, mvp: &math::M4, edges: &[[[f32; 3]; 2]], colour: Color32, width: f32) {
    for [a, b] in edges {
        if let (Some(pa), Some(pb)) = (project(mvp, *a, rect), project(mvp, *b, rect)) {
            painter.line_segment([pa, pb], egui::Stroke::new(width, colour));
        }
    }
}

/// The view's matrices for this frame.
struct View {
    view: math::M4,
    proj: math::M4,
    eye: [f32; 3],
    yaw: f32,
    spin: f32,
}

fn view_of(cam: &Camera, rect: Rect) -> View {
    let eye = cam.eye();
    View { view: math::look_at(eye, cam.target, [0.0, 1.0, 0.0]), proj: math::perspective(FOV, rect.width() / rect.height().max(1.0), 0.02, 80.0), eye, yaw: cam.yaw, spin: cam.spin }
}

/// The model's turn in the normalised frame: the turntable, a planet's tilted axis, the size.
fn turn(snap: &Snapshot, v: &View) -> math::M4 {
    let spin = math::rot_y(v.spin);
    let tilted = match &snap.scene {
        Some(s) if s.planet => math::mul(&math::rot_z(-s.tilt_deg.to_radians()), &spin),
        _ => spin,
    };
    math::mul(&math::scaling([snap.view.scale; 3]), &tilted)
}

fn view_area(ui: &mut Ui, snap: &Snapshot, rect: Rect, rt: &Handle) {
    let accent = crate::hud_board::active_colour();
    let response = ui.allocate_rect(rect, Sense::click_and_drag());
    let painter = ui.painter_at(rect);
    let now = ui.input(|i| i.time);
    // A soft studio backdrop: light from above, the theme's colour low on the floor.
    let c = rect.center();
    for k in 0..6 {
        let t = k as f32 / 6.0;
        let r = rect.width().max(rect.height()) * (0.62 - t * 0.45);
        painter.circle_filled(pos2(c.x, c.y - rect.height() * 0.08), r, Color32::from_rgba_premultiplied(12, 18, 26, 10));
    }
    crate::hud_board::brackets(&painter, rect.shrink(4.0), accent.gamma_multiply(0.55));
    // Sir's hands.
    let (drag, secondary, scroll, double, shift) = ui.input(|i| (i.pointer.delta(), i.pointer.secondary_down(), i.smooth_scroll_delta.y, i.pointer.button_double_clicked(egui::PointerButton::Primary), i.modifiers.shift));
    let holding = response.dragged();
    let moving = {
        let mut cam = lock(&CAMERA);
        for cmd in studio::take_camera() {
            cam.command(cmd);
        }
        if holding {
            if secondary || shift || response.dragged_by(egui::PointerButton::Middle) {
                let k = cam.dist * 0.0016;
                let (sy, cy) = cam.yaw.sin_cos();
                let right = [cy, 0.0, -sy];
                let up = [0.0, 1.0, 0.0];
                let t = math::add(cam.goal_target, math::add(math::scale(right, -drag.x * k), math::scale(up, drag.y * k)));
                cam.goal_target = [t[0].clamp(-2.0, 2.0), t[1].clamp(-2.0, 2.0), t[2].clamp(-2.0, 2.0)];
            } else {
                cam.goal_yaw -= drag.x * 0.008;
                cam.goal_pitch = (cam.goal_pitch + drag.y * 0.006).clamp(-1.5, 1.5);
            }
            cam.spin_home = None;
            cam.idle_until = now + 1.5;
        }
        if response.hovered() && scroll.abs() > 0.1 {
            cam.goal_dist = (cam.goal_dist * (-scroll * 0.0018).exp()).clamp(0.7, 14.0);
        }
        if double && response.hovered() {
            cam.home();
        }
        cam.step(now, snap.view.spin, holding)
    };
    let cam = *lock(&CAMERA);
    let v = view_of(&cam, rect);
    let viewproj = math::mul(&v.proj, &v.view);
    let animated = moving || snap.phase == Phase::Working || snap.view.look == Look::Hologram || snap.scene.as_ref().is_some_and(|s| s.planet);
    if animated {
        super::next_animation_frame(ui.ctx());
    }
    // The model.
    if let Some(scene) = &snap.scene {
        let turned = turn(snap, &v);
        if gpu3d::status() != gpu3d::Status::Unsupported {
            let size = scene.bounds.size();
            let footprint = (size[0].hypot(size[2]) * 0.5 / scene.bounds.radius().max(1e-9)) * snap.view.scale * 0.8;
            let frame = gpu3d::Frame {
                scene: Arc::clone(scene),
                model: math::mul(&turned, &scene.normalise()),
                view: v.view,
                proj: v.proj,
                eye: v.eye,
                cam_yaw: v.yaw,
                look: snap.view.look,
                wireframe: snap.view.wireframe,
                explode: snap.view.explode,
                accent: rgb(accent),
                time: (now % 10_000.0) as f32,
                ground_y: scene.ground_y() * snap.view.scale,
                footprint,
            };
            painter.add(gpu3d::callback(rect, frame));
        } else {
            draw_edges(&painter, rect, &math::mul(&viewproj, &turned), &scene.wire, accent.gamma_multiply(0.75), 1.0);
        }
    }
    // On its way: the scanning lattice (or the closest built-in shape), turning.
    if snap.phase == Phase::Working {
        if let Some(edges) = &snap.placeholder {
            let pulse = 0.35 + 0.25 * (now * 3.0).sin() as f32;
            let m = math::mul(&viewproj, &math::rot_y((now * 0.7) as f32));
            draw_edges(&painter, rect, &m, edges, accent.gamma_multiply(if snap.scene.is_some() { pulse * 0.4 } else { pulse }), 1.0);
        }
        let line = if snap.status.is_empty() { "WORKING…".to_string() } else { snap.status.to_uppercase() };
        let secs = snap.started.map(|s| s.elapsed().as_secs()).unwrap_or(0);
        painter.text(pos2(rect.left() + 12.0, rect.bottom() - 22.0), Align2::LEFT_BOTTOM, format!("{line}  {secs}s"), mono(11.0), accent);
        let bar = Rect::from_min_size(pos2(rect.left() + 12.0, rect.bottom() - 16.0), vec2(rect.width() - 24.0, 2.0));
        painter.rect_filled(bar, Rounding::ZERO, accent.gamma_multiply(0.18));
        match snap.progress {
            Some(p) => {
                painter.rect_filled(Rect::from_min_size(bar.min, vec2(bar.width() * p, 2.0)), Rounding::ZERO, accent);
            }
            None => {
                let x = ((now * 0.6).fract() as f32) * (bar.width() - 60.0);
                painter.rect_filled(Rect::from_min_size(pos2(bar.left() + x, bar.top()), vec2(60.0, 2.0)), Rounding::ZERO, accent);
            }
        }
    } else if snap.phase == Phase::Failed {
        painter.text(c, Align2::CENTER_CENTER, wrap_line(&snap.status, 60), mono(12.0), super::STRIP_MUTED);
    } else if !snap.status.is_empty() {
        painter.text(pos2(rect.left() + 12.0, rect.bottom() - 14.0), Align2::LEFT_BOTTOM, &snap.status, mono(10.5), STRIP_MUTED);
    }
    // The Sketchfab match (without Sir's token, or while it downloads).
    if let Some(preview) = &snap.preview {
        preview_card(ui, &painter, rect, preview, snap.note.as_deref());
    } else if let Some(note) = &snap.note {
        painter.text(pos2(rect.left() + 12.0, rect.top() + 12.0), Align2::LEFT_TOP, wrap_line(note, 70), mono(10.5), STRIP_MUTED);
    }
    if gpu3d::status() == gpu3d::Status::Unsupported && snap.scene.is_some() {
        let why = gpu3d::unsupported_reason().unwrap_or_default();
        painter.text(pos2(rect.right() - 10.0, rect.top() + 10.0), Align2::RIGHT_TOP, format!("EDGES ONLY: {why}"), mono(9.5), STRIP_MUTED);
    }
    // Files dropped on the studio open in it.
    let (dropped, hovering, pointer) = ui.input(|i| (i.raw.dropped_files.iter().filter_map(|f| f.path.clone()).collect::<Vec<_>>(), !i.raw.hovered_files.is_empty(), i.pointer.latest_pos()));
    let over = pointer.is_none_or(|p| rect.contains(p));
    if hovering && over {
        painter.rect_stroke(rect.shrink(2.0), Rounding::ZERO, stroke(1.5, accent));
        painter.text(c, Align2::CENTER_CENTER, "+ DROP A MODEL (GLB · GLTF · OBJ · STL · ZIP)", mono(13.0), accent);
    }
    if over {
        if let Some(path) = dropped.into_iter().find(|p| crate::mesh_io::Format::is_model_file(p) || crate::mesh_io::Format::of_path(p) == Some(crate::mesh_io::Format::Zip)) {
            studio::open_dropped(path, rt);
        }
    }
}

/// `text` broken into lines of about `width` characters.
fn wrap_line(text: &str, width: usize) -> String {
    let mut out = String::new();
    let mut line = 0;
    for word in text.split_whitespace() {
        if line > 0 && line + word.len() > width {
            out.push('\n');
            line = 0;
        } else if line > 0 {
            out.push(' ');
            line += 1;
        }
        out.push_str(word);
        line += word.len();
    }
    out
}

fn preview_card(ui: &Ui, painter: &Painter, rect: Rect, p: &studio::Preview, note: Option<&str>) {
    let accent = crate::hud_board::active_colour();
    let tex = {
        let mut slot = lock(&PREVIEW);
        if slot.as_ref().is_none_or(|(id, _)| *id != p.id) {
            *slot = p.thumbnail.as_ref().map(|t| {
                let image = ColorImage::from_rgba_unmultiplied([t.width as usize, t.height as usize], &t.rgba[..(t.width * t.height * 4) as usize]);
                (p.id, ui.ctx().load_texture("model3d_preview", image, TextureOptions::LINEAR))
            });
        }
        slot.as_ref().map(|(_, t)| t.clone())
    };
    let w = (rect.width() * 0.32).clamp(160.0, 300.0);
    let thumb_h = tex.as_ref().map_or(0.0, |t| w * t.size()[1] as f32 / t.size()[0].max(1) as f32);
    let text_h = 54.0 + if note.is_some() { 30.0 } else { 0.0 };
    let card = Rect::from_min_size(pos2(rect.left() + 10.0, rect.top() + 10.0), vec2(w + 12.0, thumb_h + text_h + 12.0));
    // A click opens the model's page on Sketchfab.
    let click = ui.interact(card, egui::Id::new("model3d_preview_card"), Sense::click()).on_hover_text("Open it on Sketchfab");
    if click.clicked() && p.url.starts_with("https://") {
        ui.ctx().open_url(egui::OpenUrl::new_tab(&p.url));
    }
    painter.rect_filled(card, Rounding::ZERO, CARD);
    painter.rect_stroke(card, Rounding::ZERO, stroke(1.0, crate::hud_board::border_colour()));
    let mut y = card.top() + 6.0;
    if let Some(t) = &tex {
        let r = Rect::from_min_size(pos2(card.left() + 6.0, y), vec2(w, thumb_h));
        painter.image(t.id(), r, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
        y += thumb_h + 4.0;
    }
    painter.text(pos2(card.left() + 6.0, y), Align2::LEFT_TOP, "SKETCHFAB // BEST MATCH", mono(9.0), accent);
    painter.text(pos2(card.left() + 6.0, y + 13.0), Align2::LEFT_TOP, truncate(&p.title, 34), mono(11.0), STRIP_INK);
    painter.text(pos2(card.left() + 6.0, y + 28.0), Align2::LEFT_TOP, truncate(&format!("by {} · {}", p.author, p.license), 44), mono(9.5), STRIP_MUTED);
    if let Some(n) = note {
        painter.text(pos2(card.left() + 6.0, y + 44.0), Align2::LEFT_TOP, wrap_line(n, ((w / 6.2) as usize).max(20)), mono(9.0), STRIP_MUTED);
    }
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        format!("{}…", s.chars().take(n.saturating_sub(1)).collect::<String>())
    }
}

/// What the footer says of the model: where it is from, its size.
fn facts_of(snap: &Snapshot) -> String {
    let mut facts: Vec<String> = Vec::new();
    if snap.demo {
        facts.push("TEST RUN".into());
    }
    if !snap.source.is_empty() {
        facts.push(snap.source.to_uppercase());
    }
    if snap.triangles > 0 {
        facts.push(format!("{} TRIS", grouped(snap.triangles)));
    }
    if let Some(s) = snap.size_m {
        let planet = snap.scene.as_ref().is_some_and(|x| x.planet);
        facts.push(if planet { format!("Ø {}", studio::length(s[1])) } else { format!("{} TALL", studio::length(s[1])) });
    }
    facts.join("  ·  ")
}

/// The footer's texts for a block `width` wide: side by side where both fit, else one under the
/// other (`true`), each cut to the width. (2026-11-09: in a block 476 px wide the credit, right
/// aligned, was painted over the facts.)
fn footer_lines(facts: &str, credit: Option<&str>, width: f32, char_w: f32) -> (String, Option<String>, bool) {
    let fits = ((width / char_w.max(1.0)).floor() as usize).max(8);
    let Some(credit) = credit else { return (truncate(facts, fits), None, false) };
    if facts.chars().count() + credit.chars().count() + 3 <= fits {
        return (facts.to_string(), Some(credit.to_string()), false);
    }
    (truncate(facts, fits), Some(truncate(credit, fits)), true)
}

/// The footer laid out for the width at hand: (facts, credit, stacked).
fn footer_for(ui: &Ui, snap: &Snapshot) -> (String, Option<String>, bool) {
    let char_w = ui.fonts(|f| f.glyph_width(&mono(9.5), 'M'));
    let credit = snap.credit.as_ref().map(|c| truncate(&c.line(), 90));
    footer_lines(&facts_of(snap), credit.as_deref(), ui.available_width() - 2.0, char_w)
}

/// The lines under the model: where it is from and its size, then its credit.
fn footer(ui: &mut Ui, snap: &Snapshot) {
    let (facts, credit, stacked) = footer_for(ui, snap);
    let credit_label = |ui: &mut Ui, text: String| {
        let text = RichText::new(text).font(mono(9.5)).color(STRIP_MUTED);
        match &snap.credit {
            Some(c) if c.url.starts_with("https://") => {
                ui.hyperlink_to(text, &c.url);
            }
            _ => {
                ui.label(text);
            }
        }
    };
    ui.horizontal(|ui| {
        ui.label(RichText::new(facts).font(mono(9.5)).color(STRIP_MUTED));
        if let Some(c) = credit.clone().filter(|_| !stacked) {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| credit_label(ui, c));
        }
    });
    if let Some(c) = credit.filter(|_| stacked) {
        ui.horizontal(|ui| credit_label(ui, c));
    }
}

fn grouped(n: usize) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, ch) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

fn summary(snap: &Snapshot) -> String {
    if snap.title.is_empty() {
        return "3D studio: empty".into();
    }
    let mut s = format!("3D model: {} ({})", snap.title, if snap.source.is_empty() { "…" } else { &snap.source });
    match snap.phase {
        Phase::Working => s.push_str(", on its way"),
        Phase::Failed => s.push_str(&format!(", failed: {}", snap.status)),
        _ => {}
    }
    if let Some(sz) = snap.size_m {
        s.push_str(&format!(", {} tall", studio::length(sz[1])));
    }
    s.push_str(match snap.view.look {
        Look::Realistic => ", realistic",
        Look::Hologram => ", hologram",
    });
    if snap.view.spin > 0.0 {
        s.push_str(", spinning");
    }
    if let Some(scene) = &snap.scene {
        if scene.part_names.len() > 1 {
            let names: Vec<&str> = scene.part_names.iter().map(String::as_str).take(10).collect();
            s.push_str(&format!(". Parts: {}", names.join(", ")));
        }
    }
    if let Some(c) = &snap.credit {
        s.push_str(&format!(". {}", c.line()));
    }
    s
}

fn body(ui: &mut Ui, rt: &Handle) {
    let snap = studio::snapshot();
    if snap.open {
        let line = summary(&snap);
        let changed = {
            let mut d = lock(&DESCRIBED);
            (*d != line).then(|| {
                d.clone_from(&line);
                line
            })
        };
        if let Some(line) = changed {
            crate::hud_dock::describe_block(ID, &line);
        }
    }
    if !snap.title.is_empty() {
        ui.label(RichText::new(snap.title.to_uppercase()).font(mono(12.5)).color(STRIP_INK));
    }
    toolbar(ui, &snap, rt);
    ui.add_space(4.0);
    let avail = ui.available_rect_before_wrap();
    // The footer's rows and the gap above each (a fixed 20 px left it 10 px short).
    let rows = if footer_for(ui, &snap).2 { 2.0 } else { 1.0 };
    let footer_h = rows * (ui.spacing().interact_size.y + ui.spacing().item_spacing.y);

    let rect = Rect::from_min_max(avail.min, pos2(avail.max.x, (avail.max.y - footer_h).max(avail.min.y + 80.0)));
    view_area(ui, &snap, rect, rt);
    footer(ui, &snap);
}

/// Registers the studio while it is open (a block on the board, or its own window once
/// detached); once closed, the GPU memory goes on the next frame.
pub(super) fn show(ctx: &egui::Context, rt: &Handle) {
    let viewport = ViewportId::from_hash_of(VIEWPORT);
    if !super::detached_keep(ctx, viewport, studio::is_open()) {
        crate::hud_dock::forget(ID);
        lock(&DESCRIBED).clear();
        if gpu3d::holds_gpu_memory() {
            let painter = ctx.layer_painter(egui::LayerId::background());
            painter.add(gpu3d::release_callback(Rect::from_min_size(Pos2::ZERO, vec2(1.0, 1.0))));
            ctx.request_repaint();
        }
        if lock(&PREVIEW).take().is_some() {
            // (The thumbnail's texture goes with its handle.)
        }
        return;
    }
    let title = {
        let snap = studio::snapshot();
        if snap.title.is_empty() {
            TITLE.to_string()
        } else {
            format!("{TITLE} // {}", snap.title)
        }
    };
    if !crate::hud_dock::adopt(ID) {
        let action = super::hud_block_window(ctx, ID, &title, [900.0, 640.0], |ui| body(ui, rt));
        if action.close {
            studio::close();
        }
        if action.detach {
            crate::hud_dock::detach(ID);
        }
        return;
    }
    let mut builder = ViewportBuilder::default().with_title("3D model — J.A.R.V.I.S.").with_app_id("jarvis").with_inner_size([960.0, 700.0]).with_min_inner_size([520.0, 380.0]).with_decorations(false);
    if let Some(icon) = super::window_icon() {
        builder = builder.with_icon(Arc::new(icon));
    }
    if let Some((at, size)) = super::detached_placement(ctx, ID) {
        builder = builder.with_position(at).with_inner_size(size);
    }
    let builder = super::for_test_session(builder);
    let rt = rt.clone();
    ctx.show_viewport_deferred(viewport, builder, move |ctx, class| {
        crate::crash_guard::guard("model3d", || {
            let asked = ctx.input(|i| i.viewport().close_requested());
            if asked || (!studio::is_open() && !super::detached_leaving(ctx)) {
                tracing::info!("[3D] The studio's own window closes ({})", if asked { "Windows asked it to" } else { "the studio was closed" });
                studio::close();
                ctx.send_viewport_cmd(ViewportCommand::Visible(false));
                ctx.request_repaint_of(ViewportId::ROOT);
                return;
            }
            if class == ViewportClass::Embedded {
                egui::Window::new(TITLE).show(ctx, |ui| body(ui, &rt));
            } else {
                egui::CentralPanel::default().frame(Frame::none().fill(super::BLOCK_SLATE).inner_margin(Margin::same(14.0))).show(ctx, |ui| {
                    if super::detached_title_bar(ui, ctx, TITLE) {
                        studio::close();
                    }
                    body(ui, &rt);
                });
                super::paint_window_border(ctx);
            }
        });
    });
}

/// Quick Connections → Sketchfab: Sir's own API token (sealed in the vault, never shown, never
/// logged). With it the studio downloads full Sketchfab models; without it, it shows the best
/// match as a card and composes a model meanwhile.
pub(super) fn sketchfab_section(ui: &mut Ui, p: &super::Palette, vault: &crate::vault::Vault, rt: &Handle) {
    ui.add_space(6.0);
    ui.label(RichText::new("Sketchfab (full models in the 3D studio)").font(super::bold(12.5)).color(p.accent));
    let stored = vault.has_secret(studio::SKETCHFAB_SERVICE, crate::vault::TOKEN);
    let mut field = lock(&TOKEN_FIELD);
    ui.horizontal(|ui| {
        ui.label("API token");
        let hint = if stored { "•••••• stored encrypted (type to replace)" } else { "sketchfab.com → Settings → Password & API" };
        ui.add(egui::TextEdit::singleline(&mut *field).password(true).hint_text(hint).desired_width(300.0));
        let token = field.trim().trim_start_matches("Token ").trim().to_string();
        if ui.add_enabled(!token.is_empty(), egui::Button::new("Save")).clicked() {
            *lock(&TOKEN_NOTE) = Some(match vault.set_secret(studio::SKETCHFAB_SERVICE, crate::vault::TOKEN, &token) {
                Ok(()) => (true, "Stored encrypted. \"Render me … in 3D\" now downloads full Sketchfab models.".into()),
                Err(e) => (false, format!("Not stored: {e}")),
            });
            field.clear();
        }
        if stored && ui.button("Test").clicked() {
            if let Some(token) = vault.secret(studio::SKETCHFAB_SERVICE, crate::vault::TOKEN) {
                *lock(&TOKEN_NOTE) = Some((true, "Asking Sketchfab…".into()));
                let ctx = ui.ctx().clone();
                rt.spawn(async move {
                    let note = studio::check_sketchfab_token(&token).await;
                    *lock(&TOKEN_NOTE) = Some(note);
                    ctx.request_repaint();
                });
            }
        }
        if stored && ui.button("Forget").clicked() {
            vault.clear_secret(studio::SKETCHFAB_SERVICE, crate::vault::TOKEN);
            *lock(&TOKEN_NOTE) = Some((true, "Forgotten: the studio shows Sketchfab matches as cards again.".into()));
        }
    });
    drop(field);
    if let Some((ok, text)) = lock(&TOKEN_NOTE).clone() {
        ui.label(RichText::new(text).size(11.0).color(if ok { p.green } else { p.red }));
    }
    super::muted(ui, p, "Searching needs no token. Every downloaded model shows its author and licence under it.");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_camera_eases_home_and_faces_the_front() {
        let mut cam = Camera::new();
        cam.spin = 2.0;
        cam.command(CameraCmd::Front);
        let mut t = 0.0;
        for _ in 0..200 {
            t += 1.0 / 60.0;
            cam.step(t, 0.0, false);
        }
        assert!(cam.yaw.abs() < 0.01 && (cam.pitch - 0.06).abs() < 0.01, "{cam:?}");
        assert!(cam.spin.abs() < 0.01, "the model turns its front to the camera: {}", cam.spin);
        let eye = cam.eye();
        assert!(eye[2] > 3.0 && eye[0].abs() < 0.05, "{eye:?}");
        cam.command(CameraCmd::Zoom(0.5));
        for _ in 0..200 {
            t += 1.0 / 60.0;
            cam.step(t, 0.35, false);
        }
        assert!((cam.dist - HOME_DIST * 0.5).abs() < 0.02);
        assert!(cam.spin > 0.0, "the turntable turns again");
    }

    /// 2026-11-09 screenshot: "SKETCHFAB · 97,409 TRIS · …" and the credit, right aligned, were
    /// painted over each other in a block 476 px wide.
    #[test]
    fn the_footer_never_paints_the_credit_over_the_facts() {
        let facts = "SKETCHFAB  ·  97,409 TRIS  ·  24.8 CM TALL";
        let credit = "\"Apple iPad Pro\" by Sketch78 · CC Attribution-NonCommercial · Sketchfab";
        let char_w = 5.7;
        for width in [300.0f32, 452.0, 476.0, 640.0, 900.0, 1400.0] {
            let (f, c, stacked) = footer_lines(facts, Some(credit), width, char_w);
            let c = c.unwrap();
            let (fw, cw) = (f.chars().count() as f32 * char_w, c.chars().count() as f32 * char_w);
            if stacked {
                assert!(fw <= width && cw <= width, "{width}: each line fits its own row ({fw}, {cw})");
            } else {
                assert!(fw + cw + 3.0 * char_w <= width, "{width}: side by side only with room between ({fw} + {cw})");
                assert_eq!((f.as_str(), c.as_str()), (facts, credit), "{width}: nothing cut where both fit");
            }
        }
        assert!(footer_lines(facts, Some(credit), 476.0, char_w).2, "the screenshot's width stacks");
        assert!(!footer_lines(facts, Some(credit), 1400.0, char_w).2);
        assert_eq!(footer_lines(facts, None, 476.0, char_w), (facts.to_string(), None, false));
    }

    #[test]
    fn the_projection_puts_the_centre_in_the_middle() {
        let cam = Camera::new();
        let rect = Rect::from_min_size(Pos2::ZERO, vec2(400.0, 300.0));
        let v = view_of(&cam, rect);
        let m = math::mul(&v.proj, &v.view);
        let p = project(&m, [0.0, 0.0, 0.0], rect).unwrap();
        assert!((p.x - 200.0).abs() < 0.5 && (p.y - 150.0).abs() < 0.5, "{p:?}");
        // Up is up on screen.
        let up = project(&m, [0.0, 0.5, 0.0], rect).unwrap();
        assert!(up.y < p.y);
        assert_eq!(wrap_line("a bb ccc dddd", 6), "a bb\nccc\ndddd");
        assert_eq!(grouped(1234567), "1,234,567");
    }
}
