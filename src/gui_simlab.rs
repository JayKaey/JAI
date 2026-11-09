//! The Simulation Lab: every simulation is a ROSE block inside the main HUD by default
//! (`gui::hud_block_window`), redrawn at 60 FPS, with the parameter sliders on the left and the
//! live plot / animation on the right; `[ DETACH ]` or "open it in a separate window" moves it
//! into its own egui deferred viewport. The plot is the ROSE look: a `#050A10` canvas in a 1 px
//! `#2B7A9E` frame with white L-corner brackets, white grid axes with their numbers, f(x) in
//! glowing cyan `#00E5FF` and f′(x) in crimson `#FF1E56`. The canvas and the controls get
//! explicit rectangles (a painter allocated inside a horizontal row was only one row tall, which
//! left the old window's plot pitch-black).
//! Child module of `gui` (shares the HUD palette and building blocks); the models live in
//! `simulation_lab.rs`, the per-window animation state (time, pendulum angle, ball positions,
//! the descent's position, the sort's progress) here.

use super::widget_view::WidgetHost;
use super::{bold, button, muted, paint_background, stroke, Palette, Tone};
use crate::simulation_lab::{self, Param, SimKind, Simulation};
use eframe::egui::{self, pos2, vec2, Align2, Color32, FontId, Frame, Margin, Pos2, Rect, RichText, Rounding, Sense, Shape, Stroke, ViewportBuilder, ViewportClass, ViewportCommand, ViewportId};
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Animation state of one window (reset whenever the simulation's revision changes).
struct Runtime {
    revision: u64,
    t: f64,
    last: Instant,
    /// Projectile: precomputed (t, x, y) path.
    path: Vec<(f64, f64, f64)>,
    /// Pendulum: angle (rad), angular velocity, trail of bob positions (world units).
    theta: f64,
    omega: f64,
    trail: Vec<(f64, f64)>,
    /// Collision: positions, velocities, whether they met.
    x1: f64,
    x2: f64,
    v1: f64,
    v2: f64,
    hit: bool,
    /// Gradient descent: position, steps taken, the path, timer.
    gd_x: f64,
    gd_steps: usize,
    gd_path: Vec<f64>,
    gd_timer: f64,
    /// Sorting: states and the progress through them.
    sort: Vec<(Vec<f64>, (usize, usize))>,
    sort_pos: f64,
}

impl Runtime {
    fn new(sim: &Simulation) -> Self {
        let mut r = Runtime {
            revision: sim.revision,
            t: 0.0,
            last: Instant::now(),
            path: Vec::new(),
            theta: sim.value("angle").to_radians(),
            omega: 0.0,
            trail: Vec::new(),
            x1: -6.0,
            x2: 4.0,
            v1: sim.value("velocity1"),
            v2: sim.value("velocity2"),
            hit: false,
            gd_x: sim.value("start"),
            gd_steps: 0,
            gd_path: vec![sim.value("start")],
            gd_timer: 0.0,
            sort: Vec::new(),
            sort_pos: 0.0,
        };
        match sim.kind {
            SimKind::Projectile => r.path = trajectory(sim),
            SimKind::Sorting => {
                let n = sim.value("count").round().clamp(2.0, 400.0) as usize;
                let mut seed = 0x9E37_79B9u32 ^ (sim.revision as u32).wrapping_mul(2_654_435_761);
                let values: Vec<f64> = (0..n)
                    .map(|_| {
                        seed ^= seed << 13;
                        seed ^= seed >> 17;
                        seed ^= seed << 5;
                        (seed % 1000) as f64 / 1000.0 * 0.95 + 0.05
                    })
                    .collect();
                r.sort = simulation_lab::sort_steps(&values, &sim.algorithm);
            }
            _ => {}
        }
        r
    }
}

static RUNTIMES: Mutex<Option<HashMap<String, Runtime>>> = Mutex::new(None);

fn viewport_id(id: &str) -> ViewportId {
    ViewportId::from_hash_of(("jarvis_simlab", id))
}

/// The projectile's flight (with quadratic drag), sampled every 5 ms.
fn trajectory(sim: &Simulation) -> Vec<(f64, f64, f64)> {
    let (v, a, g, h, k) = (sim.value("velocity"), sim.value("angle").to_radians(), sim.value("gravity").max(0.01), sim.value("height"), sim.value("drag"));
    let (mut x, mut y, mut vx, mut vy, mut t) = (0.0, h, v * a.cos(), v * a.sin(), 0.0);
    let dt = 0.005;
    let mut out = vec![(0.0, x, y)];
    while t < 120.0 {
        let speed = (vx * vx + vy * vy).sqrt();
        vx += -k * speed * vx * dt;
        vy += (-g - k * speed * vy) * dt;
        x += vx * dt;
        y += vy * dt;
        t += dt;
        if y < 0.0 {
            out.push((t, x, 0.0));
            break;
        }
        out.push((t, x, y));
    }
    out
}

/// The dock key of a simulation (`hud_dock`).
pub(super) fn dock_key(id: &str) -> String {
    format!("sim:{id}")
}

/// ROSE plot colours.
const PLOT_CANVAS: Color32 = Color32::from_rgb(0x05, 0x0A, 0x10);
fn plot_frame() -> Color32 {
    crate::hud_board::border_colour()
}
/// f(x): the theme's contrast accent (cyan in the ROSE default).
fn plot_cyan() -> Color32 {
    crate::theme_engine::current().secondary()
}
/// f′(x): the theme's primary (crimson in ROSE), else crimson `#FF1E56` when the primary is too
/// close to f(x)'s colour to tell the curves apart (Arc Cyan: two blues).
fn plot_crimson() -> Color32 {
    let (a, b) = (crate::theme_engine::current().primary(), plot_cyan());
    let distance = a.r().abs_diff(b.r()) as u32 + a.g().abs_diff(b.g()) as u32 + a.b().abs_diff(b.b()) as u32;
    if distance < 200 { Color32::from_rgb(0xFF, 0x1E, 0x56) } else { a }
}
/// Segments of every plotted curve.
pub const CURVE_SEGMENTS: usize = 200;

/// Registers every simulation with egui (each HUD frame): a block in the HUD, or its own window
/// once detached.
pub(super) fn show_simulations(ctx: &egui::Context, host: &WidgetHost) {
    static SEEN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let lab = &host.app.tools().stark.simlab;
    let revision = lab.revision();
    let changed = SEEN.swap(revision, std::sync::atomic::Ordering::AcqRel) != revision;
    let sims = lab.list();
    // (A window playing its exit keeps its run until it has gone.)
    let leaving: Vec<String> = SHOWN.lock().unwrap_or_else(|e| e.into_inner()).iter().map(|(sim, _)| sim.id.clone()).collect();
    {
        let mut guard = RUNTIMES.lock().unwrap_or_else(|e| e.into_inner());
        guard.get_or_insert_with(HashMap::new).retain(|id, _| sims.iter().any(|s| s.id == *id) || leaving.contains(id));
    }
    let p = Palette::new(&host.app.config().read(|c| c.interface.clone()));
    let open_ids: Vec<String> = sims.iter().map(|s| s.id.clone()).collect();
    let mut shown_now: Vec<(Simulation, ViewportBuilder)> = Vec::new();
    for sim in sims {
        let key = dock_key(&sim.id);
        if !crate::hud_dock::adopt(&key) {
            let title = format!("{} — Simulation Lab", sim.title);
            let action = super::hud_block_window(ctx, &key, &title, [900.0, 560.0], |ui| draw_body(ui, &p, &sim, host, true));
            if action.close {
                lab.close(&sim.id);
                crate::hud_dock::forget(&key);
            }
            if action.detach {
                crate::hud_dock::detach(&key);
            }
            if !sim.paused {
                ctx.request_repaint_after(Duration::from_millis(16));
            }
            continue;
        }
        if changed {
            ctx.request_repaint_of(viewport_id(&sim.id));
        }
        // Opaque: a transparent GL window showed black wherever nothing was painted.
        let mut builder = ViewportBuilder::default()
            .with_title(format!("{} — J.A.R.V.I.S. Simulation Lab", sim.title))
            .with_app_id("jarvis")
            .with_inner_size([960.0, 620.0])
            .with_min_inner_size([560.0, 380.0])
            .with_transparent(false)
            .with_decorations(false);
        if let Some(icon) = super::window_icon() {
            builder = builder.with_icon(std::sync::Arc::new(icon));
        }
        if let Some((at, size)) = super::detached_placement(ctx, &key) {
            builder = builder.with_position(at).with_inner_size(size);
        }
        let builder = super::for_test_session(builder);
        // (Asked for again while it was leaving: it stays.)
        super::detached_keep(ctx, viewport_id(&sim.id), true);
        shown_now.push((sim.clone(), builder.clone()));
        let (host, id) = (host.clone(), sim.id.clone());
        ctx.show_viewport_deferred(viewport_id(&sim.id), builder, move |ctx, class| {
            crate::crash_guard::guard("Simulation Lab window", || sim_frame(ctx, class, &host, &id));
        });
    }
    // A simulation closed while it had a window of its own plays the window's exit before the
    // window goes; one that went back into the HUD leaves at once.
    let before = std::mem::take(&mut *SHOWN.lock().unwrap_or_else(|e| e.into_inner()));
    for (sim, builder) in before {
        if shown_now.iter().any(|(s, _)| s.id == sim.id) || open_ids.contains(&sim.id) || !super::detached_keep(ctx, viewport_id(&sim.id), false) {
            continue;
        }
        let (host, id) = (host.clone(), sim.id.clone());
        ctx.show_viewport_deferred(viewport_id(&sim.id), builder.clone(), move |ctx, class| {
            crate::crash_guard::guard("Simulation Lab window", || sim_frame(ctx, class, &host, &id));
        });
        shown_now.push((sim, builder));
    }
    *SHOWN.lock().unwrap_or_else(|e| e.into_inner()) = shown_now;
}

/// The simulations in windows of their own shown in the last frame, as they were (with their
/// builders): one that was closed keeps its window while its exit plays, drawn from here.
static SHOWN: Mutex<Vec<(Simulation, ViewportBuilder)>> = Mutex::new(Vec::new());

fn sim_frame(ctx: &egui::Context, class: ViewportClass, host: &WidgetHost, id: &str) {
    let lab = &host.app.tools().stark.simlab;
    // (Closed and playing its exit: drawn as it was.)
    let open = lab.get(id);
    let leaving = open.is_none();
    let shown = || SHOWN.lock().unwrap_or_else(|e| e.into_inner()).iter().find(|(sim, _)| sim.id == id).map(|(sim, _)| sim.clone());
    let Some(sim) = open.or_else(|| shown().filter(|_| super::detached_leaving(ctx))) else {
        ctx.send_viewport_cmd(ViewportCommand::Visible(false));
        ctx.request_repaint_of(ViewportId::ROOT);
        return;
    };
    if !leaving && ctx.input(|i| i.viewport().close_requested()) {
        // Closing the detached window brings the simulation back onto the HUD.
        crate::hud_dock::dock(&dock_key(id));
        ctx.send_viewport_cmd(ViewportCommand::Visible(false));
        ctx.request_repaint_of(ViewportId::ROOT);
        return;
    }
    let p = Palette::new(&host.app.config().read(|c| c.interface.clone()));
    paint_background(ctx, &p);
    let body = |ui: &mut egui::Ui| draw_body(ui, &p, &sim, host, false);
    if class == ViewportClass::Embedded {
        egui::Window::new(&sim.title).id(egui::Id::new(("jarvis_sim_embedded", id))).default_size([900.0, 560.0]).show(ctx, body);
    } else {
        egui::CentralPanel::default().frame(Frame::none().inner_margin(Margin::same(12.0))).show(ctx, |ui| {
            // Closing the borderless window brings the simulation back onto the HUD.
            if super::detached_title_bar(ui, ctx, &sim.title) {
                crate::hud_dock::dock(&dock_key(id));
            }
            body(ui);
        });
        super::paint_window_border(ctx);
    }
    ctx.request_repaint_after(Duration::from_millis(if sim.paused { 250 } else { 16 }));
}

/// The control column's width and the plot canvas inside `avail` (side by side; the controls
/// take at most 40 % of a narrow block).
pub(super) fn split_layout(avail: Rect) -> (Rect, Rect) {
    let controls_w = 250.0f32.min(avail.width() * 0.4).max(150.0);
    let controls = Rect::from_min_size(avail.min, vec2(controls_w, avail.height()));
    let plot = Rect::from_min_max(pos2(avail.left() + controls_w + 12.0, avail.top()), avail.max);
    (controls, plot)
}

fn draw_body(ui: &mut egui::Ui, p: &Palette, sim: &Simulation, host: &WidgetHost, in_app: bool) {
    let lab = &host.app.tools().stark.simlab;
    ui.horizontal_wrapped(|ui| {
        if !in_app {
            ui.label(RichText::new(sim.title.to_uppercase()).font(bold(16.0)).color(p.accent).extra_letter_spacing(1.5));
        }
        super::chip(ui, sim.kind.label(), p.violet);
        if ui.button(if sim.paused { "Resume" } else { "Pause" }).clicked() {
            lab.toggle_pause(&sim.id);
        }
        if ui.button("Restart").clicked() {
            lab.restart(&sim.id);
        }
        if !in_app && button(ui, p, "Close", Tone::Danger).clicked() {
            lab.close(&sim.id);
            ui.ctx().request_repaint_of(ViewportId::ROOT);
        }
    });
    ui.add_space(6.0);
    let avail = ui.available_rect_before_wrap();
    if avail.width() < 200.0 || avail.height() < 120.0 {
        return;
    }
    let (controls_rect, plot_rect) = split_layout(avail);
    ui.allocate_new_ui(egui::UiBuilder::new().max_rect(controls_rect), |ui| {
        ui.set_width(controls_rect.width());
        egui::ScrollArea::vertical().id_salt(("sim_controls", &sim.id)).max_height(controls_rect.height()).show(ui, |ui| controls(ui, p, sim, host));
    });
    let response = ui.allocate_rect(plot_rect, Sense::hover());
    let painter = ui.painter_at(plot_rect.expand(2.0));
    painter.rect_filled(plot_rect, Rounding::ZERO, PLOT_CANVAS);
    painter.rect_stroke(plot_rect, Rounding::ZERO, Stroke::new(1.0f32, plot_frame()));
    crate::hud_board::brackets(&painter, plot_rect, Color32::WHITE);
    let mut guard = RUNTIMES.lock().unwrap_or_else(|e| e.into_inner());
    let runtimes = guard.get_or_insert_with(HashMap::new);
    let rt = runtimes.entry(sim.id.clone()).or_insert_with(|| Runtime::new(sim));
    if rt.revision != sim.revision {
        // Slider changes keep the pendulum swinging from where it is only when its own
        // start angle did not change; everything else restarts cleanly.
        *rt = Runtime::new(sim);
    }
    let now = Instant::now();
    let dt = if sim.paused { 0.0 } else { now.duration_since(rt.last).as_secs_f64().min(0.05) };
    rt.last = now;
    rt.t += dt;
    let inner = plot_rect.shrink(16.0);
    match sim.kind {
        SimKind::Function => draw_function(&painter, inner, p, sim, response.hover_pos()),
        SimKind::Projectile => draw_projectile(&painter, inner, p, sim, rt),
        SimKind::Pendulum => draw_pendulum(&painter, inner, p, sim, rt, dt),
        SimKind::Collision => draw_collision(&painter, inner, p, sim, rt, dt),
        SimKind::Waves => draw_waves(&painter, inner, p, sim, rt),
        SimKind::GradientDescent => draw_descent(&painter, inner, p, sim, rt, dt),
        SimKind::Sorting => draw_sorting(&painter, inner, p, sim, rt, dt),
    }
}

fn controls(ui: &mut egui::Ui, p: &Palette, sim: &Simulation, host: &WidgetHost) {
    let lab = &host.app.tools().stark.simlab;
    ui.label(RichText::new("PARAMETERS").font(bold(10.5)).color(p.muted).extra_letter_spacing(1.0));
    ui.spacing_mut().slider_width = 150.0;
    for prm in &sim.params {
        let Param { name, label, value, min, max, unit } = prm.clone();
        let mut v = value;
        ui.label(RichText::new(format!("{label}{}", if unit.is_empty() { String::new() } else { format!(" ({unit})") })).size(11.5));
        let step = ((max - min) / 400.0).max(1e-4);
        if ui.add(egui::Slider::new(&mut v, min..=max).step_by(if (max - min) > 50.0 { 1.0 } else { step })).changed() {
            lab.set_param(&sim.id, &name, v);
        }
    }
    if sim.kind == SimKind::Function {
        ui.add_space(4.0);
        let (mut d, mut i) = (sim.show_derivative, sim.show_integral);
        // One option per line (they wrap inside the column instead of running off its edge).
        if ui.add(egui::Checkbox::new(&mut d, RichText::new("Show derivative f′(x)").size(11.5))).changed() {
            lab.set_option(&sim.id, Some(d), None);
        }
        if ui.add(egui::Checkbox::new(&mut i, RichText::new("Shade the integral").size(11.5))).changed() {
            lab.set_option(&sim.id, None, Some(i));
        }
        ui.add_space(4.0);
        for (label, src, _) in &sim.functions {
            ui.label(RichText::new(format!("{label}(x) = {src}")).monospace().size(11.5).color(p.accent));
        }
    }
    if sim.kind == SimKind::Sorting {
        muted(ui, p, format!("Algorithm: {}", sim.algorithm));
    }
    ui.add_space(8.0);
    ui.label(RichText::new("READOUT").font(bold(10.5)).color(p.muted).extra_letter_spacing(1.0));
    ui.add(egui::Label::new(RichText::new(sim.readout()).size(11.5)).wrap());
    ui.add_space(8.0);
    muted(ui, p, "Drag the sliders, or tell Jarvis: “set the angle to 60 degrees and double the velocity”.");
}

// ---------------------------------------------------------------------- plotting ----

/// World → screen mapping with a grid and labelled axes.
struct View {
    rect: Rect,
    x0: f64,
    x1: f64,
    y0: f64,
    y1: f64,
}

impl View {
    fn to_screen(&self, x: f64, y: f64) -> Pos2 {
        let fx = ((x - self.x0) / (self.x1 - self.x0)) as f32;
        let fy = ((y - self.y0) / (self.y1 - self.y0)) as f32;
        pos2(self.rect.left() + fx * self.rect.width(), self.rect.bottom() - fy * self.rect.height())
    }

    fn to_world(&self, pos: Pos2) -> (f64, f64) {
        let fx = ((pos.x - self.rect.left()) / self.rect.width()) as f64;
        let fy = ((self.rect.bottom() - pos.y) / self.rect.height()) as f64;
        (self.x0 + fx * (self.x1 - self.x0), self.y0 + fy * (self.y1 - self.y0))
    }

    fn grid(&self, painter: &egui::Painter, _p: &Palette) {
        let (minor, axis, ink) = (Color32::from_white_alpha(22), Color32::from_white_alpha(200), Color32::from_white_alpha(190));
        let step = |span: f64| {
            let raw = span / 8.0;
            let mag = 10f64.powf(raw.abs().log10().floor());
            [1.0, 2.0, 5.0, 10.0].iter().map(|m| m * mag).find(|s| *s >= raw).unwrap_or(raw)
        };
        let font = FontId::proportional(10.0);
        let (sx, sy) = (step(self.x1 - self.x0), step(self.y1 - self.y0));
        let mut x = (self.x0 / sx).ceil() * sx;
        while x <= self.x1 {
            let a = self.to_screen(x, self.y0);
            let b = self.to_screen(x, self.y1);
            painter.line_segment([a, b], Stroke::new(1.0f32, minor));
            painter.text(pos2(a.x, self.rect.bottom() + 2.0), Align2::CENTER_TOP, label(x), font.clone(), ink);
            x += sx;
        }
        let mut y = (self.y0 / sy).ceil() * sy;
        while y <= self.y1 {
            let a = self.to_screen(self.x0, y);
            let b = self.to_screen(self.x1, y);
            painter.line_segment([a, b], Stroke::new(1.0f32, minor));
            painter.text(pos2(self.rect.left() - 3.0, a.y), Align2::RIGHT_CENTER, label(y), font.clone(), ink);
            y += sy;
        }
        if self.x0 < 0.0 && self.x1 > 0.0 {
            painter.line_segment([self.to_screen(0.0, self.y0), self.to_screen(0.0, self.y1)], Stroke::new(1.2f32, axis));
        }
        if self.y0 < 0.0 && self.y1 > 0.0 {
            painter.line_segment([self.to_screen(self.x0, 0.0), self.to_screen(self.x1, 0.0)], Stroke::new(1.2f32, axis));
        }
    }
}

fn label(v: f64) -> String {
    if v.abs() < 1e-9 {
        "0".into()
    } else if v.abs() >= 1000.0 || (v - v.round()).abs() < 1e-9 {
        format!("{v:.0}")
    } else if v.abs() >= 1.0 {
        format!("{v:.1}")
    } else {
        format!("{v:.2}")
    }
}

/// f(x) in glowing cyan, then the HUD's other accents for further functions.
fn curve_colours(p: &Palette) -> [Color32; 6] {
    [plot_cyan(), p.green, p.amber, p.violet, p.sky, p.red]
}

/// The samples of a curve over [a, b] (`CURVE_SEGMENTS` segments).
pub(super) fn sample_xs(a: f64, b: f64) -> Vec<f64> {
    (0..=CURVE_SEGMENTS).map(|i| a + (b - a) * i as f64 / CURVE_SEGMENTS as f64).collect()
}

/// Screen polylines of a sampled curve: one per finite run (asymptotes and NaN gaps split it).
/// Points may leave the view by up to one view height; the caller clips to the plot, so a curve
/// leaving the top runs off the edge instead of flattening along it.
pub(super) fn curve_runs(xs: &[f64], ys: &[f64], to_screen: impl Fn(f64, f64) -> Pos2, y0: f64, y1: f64) -> Vec<Vec<Pos2>> {
    let span = y1 - y0;
    let mut runs = Vec::new();
    let mut run: Vec<Pos2> = Vec::new();
    for (x, y) in xs.iter().zip(ys) {
        if y.is_finite() && *y >= y0 - span && *y <= y1 + span {
            run.push(to_screen(*x, *y));
        } else if !run.is_empty() {
            runs.push(std::mem::take(&mut run));
        }
    }
    if !run.is_empty() {
        runs.push(run);
    }
    runs.retain(|r| r.len() >= 2);
    runs
}

fn draw_function(painter: &egui::Painter, rect: Rect, p: &Palette, sim: &Simulation, hover: Option<Pos2>) {
    let rect = Rect::from_min_max(rect.min + vec2(34.0, 0.0), rect.max - vec2(0.0, 16.0));
    let (a, b) = sim.x_range;
    let n = CURVE_SEGMENTS;
    let xs = sample_xs(a, b);
    let mut series: Vec<(Vec<f64>, Color32, bool)> = Vec::new();
    for (k, (_, _, e)) in sim.functions.iter().enumerate() {
        let colour = curve_colours(p)[k % 6];
        series.push((xs.iter().map(|x| e.eval(*x, &sim.params)).collect(), colour, false));
        if sim.show_derivative {
            // f′(x) in crimson (dashed for the further functions).
            series.push((xs.iter().map(|x| simulation_lab::derivative(e, *x, &sim.params)).collect(), if k == 0 { plot_crimson() } else { colour.gamma_multiply(0.6) }, k > 0));
        }
    }
    // The y range from the 2nd-98th percentile, so asymptotes don't flatten everything.
    let mut ys: Vec<f64> = series.iter().flat_map(|(v, _, _)| v.iter().copied()).filter(|v| v.is_finite()).collect();
    ys.sort_by(|a, b| a.total_cmp(b));
    let (mut y0, mut y1) = if ys.is_empty() { (-1.0, 1.0) } else { (ys[ys.len() * 2 / 100], ys[(ys.len() * 98 / 100).min(ys.len() - 1)]) };
    if (y1 - y0).abs() < 1e-6 {
        y0 -= 1.0;
        y1 += 1.0;
    }
    let pad = (y1 - y0) * 0.1;
    let view = View { rect, x0: a, x1: b, y0: y0 - pad, y1: y1 + pad };
    view.grid(painter, p);
    // Integral of the first function: shaded strips down to the axis.
    if sim.show_integral {
        if let Some((values, colour, _)) = series.first() {
            let base = 0.0f64.clamp(view.y0, view.y1);
            for i in 0..n {
                let (ya, yb) = (values[i], values[i + 1]);
                if !(ya.is_finite() && yb.is_finite()) {
                    continue;
                }
                let clamp = |y: f64| y.clamp(view.y0, view.y1);
                let pts = vec![view.to_screen(xs[i], base), view.to_screen(xs[i], clamp(ya)), view.to_screen(xs[i + 1], clamp(yb)), view.to_screen(xs[i + 1], base)];
                painter.add(Shape::convex_polygon(pts, colour.gamma_multiply(0.18), Stroke::NONE));
            }
        }
    }
    let clipped = painter.with_clip_rect(view.rect.intersect(painter.clip_rect()));
    let painter = &clipped;
    for (values, colour, dashed) in &series {
        for run in curve_runs(&xs, values, |x, y| view.to_screen(x, y), view.y0, view.y1) {
            if *dashed {
                painter.extend(Shape::dashed_line(&run, Stroke::new(1.4f32, *colour), 6.0, 4.0));
            } else {
                // A soft glow under the crisp anti-aliased stroke.
                painter.add(Shape::line(run.clone(), Stroke::new(6.0f32, colour.gamma_multiply(0.18))));
                painter.add(Shape::line(run, Stroke::new(2.2f32, *colour)));
            }
        }
    }
    // Roots.
    for (k, (_, _, e)) in sim.functions.iter().enumerate() {
        for r in simulation_lab::roots(e, a, b, &sim.params) {
            let at = view.to_screen(r, 0.0f64.clamp(view.y0, view.y1));
            painter.circle_stroke(at, 4.5, stroke(1.6, curve_colours(p)[k % 6]));
        }
    }
    // Legend.
    let mut top = 8.0;
    for (k, (label, src, _)) in sim.functions.iter().enumerate() {
        top += readout(painter, rect, top, &format!("{label}(x) = {src}"), curve_colours(p)[k % 6]) + 1.0;
    }
    // Crosshair under the pointer.
    if let Some(h) = hover.filter(|h| rect.contains(*h)) {
        let (x, _) = view.to_world(h);
        painter.line_segment([pos2(h.x, rect.top()), pos2(h.x, rect.bottom())], stroke(1.0, p.muted.gamma_multiply(0.6)));
        let mut lines = vec![format!("x = {x:.3}")];
        for (k, (label, _, e)) in sim.functions.iter().enumerate() {
            let y = e.eval(x, &sim.params);
            if y.is_finite() && y >= view.y0 && y <= view.y1 {
                painter.circle_filled(view.to_screen(x, y), 4.0, curve_colours(p)[k % 6]);
            }
            lines.push(format!("{label}(x) = {y:.4}"));
            if sim.show_derivative {
                lines.push(format!("{label}′(x) = {:.4}", simulation_lab::derivative(e, x, &sim.params)));
            }
        }
        painter.text(h + vec2(10.0, -10.0), Align2::LEFT_BOTTOM, lines.join("\n"), FontId::monospace(11.0), p.text);
    }
}

fn draw_projectile(painter: &egui::Painter, rect: Rect, p: &Palette, sim: &Simulation, rt: &mut Runtime) {
    let rect = Rect::from_min_max(rect.min + vec2(34.0, 0.0), rect.max - vec2(0.0, 16.0));
    let max_x = rt.path.iter().map(|s| s.1).fold(1.0, f64::max);
    let max_y = rt.path.iter().map(|s| s.2).fold(1.0, f64::max);
    let view = View { rect, x0: 0.0, x1: max_x * 1.08, y0: 0.0, y1: max_y * 1.25 };
    view.grid(painter, p);
    let flight = rt.path.last().map(|s| s.0).unwrap_or(1.0);
    // Play the flight in real time (fast ones slower, long ones faster), then hold a moment.
    let speed = (flight / 4.0).clamp(0.25, 6.0);
    let t = (rt.t * speed) % (flight + speed);
    let pts: Vec<Pos2> = rt.path.iter().step_by(4).map(|s| view.to_screen(s.1, s.2)).collect();
    painter.extend(Shape::dashed_line(&pts, stroke(1.2, p.accent.gamma_multiply(0.45)), 6.0, 5.0));
    let i = rt.path.partition_point(|s| s.0 < t.min(flight)).min(rt.path.len().saturating_sub(1));
    let flown: Vec<Pos2> = rt.path[..=i].iter().step_by(2).map(|s| view.to_screen(s.1, s.2)).collect();
    if flown.len() >= 2 {
        painter.add(Shape::line(flown, stroke(2.4, p.accent)));
    }
    if let Some(&(ts, x, y)) = rt.path.get(i) {
        let at = view.to_screen(x, y);
        painter.circle_filled(at, 8.0, p.amber);
        painter.circle_stroke(at, 12.0, stroke(1.0, p.amber.gamma_multiply(0.5)));
        let next = rt.path.get(i + 1).copied().unwrap_or((ts, x, y));
        let dt = (next.0 - ts).max(1e-6);
        let (vx, vy) = ((next.1 - x) / dt, (next.2 - y) / dt);
        let tip = view.to_screen(x + vx * 0.15 * max_x / sim.value("velocity").max(1.0), y + vy * 0.15 * max_x / sim.value("velocity").max(1.0));
        painter.arrow(at, tip - at, stroke(2.0, p.green));
        readout(painter, rect, 8.0, &format!("t = {ts:.2} s   x = {x:.1} m   y = {y:.1} m   v = {:.1} m/s", (vx * vx + vy * vy).sqrt()), p.text);
    }
    painter.line_segment([view.to_screen(0.0, 0.0), view.to_screen(view.x1, 0.0)], stroke(2.0, p.green.gamma_multiply(0.7)));
}

fn draw_pendulum(painter: &egui::Painter, rect: Rect, p: &Palette, sim: &Simulation, rt: &mut Runtime, dt: f64) {
    let (l, g, damping) = (sim.value("length").max(0.05), sim.value("gravity"), sim.value("damping"));
    // RK4 in small steps.
    let steps = ((dt / 0.002).ceil() as usize).max(1);
    let h = dt / steps as f64;
    let f = |theta: f64, omega: f64| (omega, -(g / l) * theta.sin() - damping * omega);
    for _ in 0..steps {
        let (k1t, k1o) = f(rt.theta, rt.omega);
        let (k2t, k2o) = f(rt.theta + 0.5 * h * k1t, rt.omega + 0.5 * h * k1o);
        let (k3t, k3o) = f(rt.theta + 0.5 * h * k2t, rt.omega + 0.5 * h * k2o);
        let (k4t, k4o) = f(rt.theta + h * k3t, rt.omega + h * k3o);
        rt.theta += h / 6.0 * (k1t + 2.0 * k2t + 2.0 * k3t + k4t);
        rt.omega += h / 6.0 * (k1o + 2.0 * k2o + 2.0 * k3o + k4o);
    }
    let pivot = pos2(rect.center().x, rect.top() + rect.height() * 0.12);
    let scale = (rect.height() * 0.78).min(rect.width() * 0.42) / 10.0f32.max(l as f32 * 1.05).min(10.5);
    let len = l as f32 * scale;
    let bob = pivot + vec2(rt.theta.sin() as f32, rt.theta.cos() as f32) * len;
    if dt > 0.0 {
        rt.trail.push((bob.x as f64, bob.y as f64));
        if rt.trail.len() > 90 {
            rt.trail.remove(0);
        }
    }
    for (k, w) in rt.trail.windows(2).enumerate() {
        let alpha = k as f32 / rt.trail.len().max(1) as f32;
        painter.line_segment([pos2(w[0].0 as f32, w[0].1 as f32), pos2(w[1].0 as f32, w[1].1 as f32)], stroke(2.0, p.accent.gamma_multiply(alpha * 0.6)));
    }
    painter.line_segment([pivot - vec2(60.0, 0.0), pivot + vec2(60.0, 0.0)], stroke(3.0, p.muted));
    painter.line_segment([pivot, bob], stroke(2.0, p.text));
    painter.circle_filled(pivot, 4.0, p.muted);
    painter.circle_filled(bob, 14.0, p.amber);
    painter.circle_stroke(bob, 18.0, stroke(1.0, p.amber.gamma_multiply(0.5)));
    let energy = 0.5 * (l * rt.omega).powi(2) + g * l * (1.0 - rt.theta.cos());
    readout(painter, rect, 8.0, &format!("θ = {:+.1}°   ω = {:+.2} rad/s   E/m = {:.2} J/kg   t = {:.1} s", rt.theta.to_degrees(), rt.omega, energy, rt.t), p.text);
}

fn draw_collision(painter: &egui::Painter, rect: Rect, p: &Palette, sim: &Simulation, rt: &mut Runtime, dt: f64) {
    let (m1, m2, e) = (sim.value("mass1"), sim.value("mass2"), sim.value("elasticity"));
    let r = |m: f64| 0.35 + 0.25 * m.cbrt();
    let (r1, r2) = (r(m1), r(m2));
    rt.x1 += rt.v1 * dt;
    rt.x2 += rt.v2 * dt;
    if !rt.hit && rt.x2 - rt.x1 <= r1 + r2 && rt.v1 > rt.v2 {
        let (u1, u2) = simulation_lab::collide(m1, m2, rt.v1, rt.v2, e);
        rt.v1 = u1;
        rt.v2 = u2;
        rt.hit = true;
        // Perfectly inelastic: they move on together.
        if e < 0.02 {
            rt.x2 = rt.x1 + r1 + r2;
        }
    }
    if rt.x1 < -16.0 || rt.x2 > 16.0 || rt.x1 > 16.0 || rt.x2 < -16.0 || rt.t > 20.0 {
        *rt = Runtime::new(sim);
    }
    let view = View { rect, x0: -12.0, x1: 12.0, y0: -4.0, y1: 4.0 };
    let track_y = rect.center().y + 30.0;
    painter.line_segment([pos2(rect.left(), track_y), pos2(rect.right(), track_y)], stroke(2.0, p.muted));
    let px_per_m = rect.width() / 24.0;
    for (x, rad, m, v, colour, name) in [(rt.x1, r1, m1, rt.v1, p.accent, "m₁"), (rt.x2, r2, m2, rt.v2, p.amber, "m₂")] {
        let c = pos2(view.to_screen(x, 0.0).x, track_y - rad as f32 * px_per_m);
        painter.circle_filled(c, rad as f32 * px_per_m, colour.gamma_multiply(0.85));
        painter.text(c, Align2::CENTER_CENTER, format!("{name}\n{m:.1} kg"), FontId::proportional(11.0), Color32::BLACK);
        if v.abs() > 0.01 {
            painter.arrow(c - vec2(0.0, rad as f32 * px_per_m + 14.0), vec2(v as f32 * px_per_m * 0.6, 0.0), stroke(2.0, p.green));
        }
    }
    let p_total = m1 * rt.v1 + m2 * rt.v2;
    let ke = 0.5 * m1 * rt.v1 * rt.v1 + 0.5 * m2 * rt.v2 * rt.v2;
    readout(painter, rect, 8.0, &format!("v₁ = {:+.2} m/s   v₂ = {:+.2} m/s   p = {:.2} kg·m/s   KE = {:.2} J   {}", rt.v1, rt.v2, p_total, ke, if rt.hit { "after the collision" } else { "before the collision" }), p.text);
}

fn draw_waves(painter: &egui::Painter, rect: Rect, p: &Palette, sim: &Simulation, rt: &Runtime) {
    let (f, lambda, sep, phase) = (sim.value("frequency"), sim.value("wavelength").max(0.05), sim.value("separation"), sim.value("phase").to_radians());
    let k = std::f64::consts::TAU / lambda;
    let w = std::f64::consts::TAU * f;
    let span_x = (sep * 1.8).max(12.0);
    let span_y = span_x * rect.height() as f64 / rect.width() as f64;
    let (cols, rows) = (120usize, ((120.0 * rect.height() / rect.width()) as usize).clamp(30, 120));
    let (cw, ch) = (rect.width() / cols as f32, rect.height() / rows as f32);
    let mut mesh = egui::Mesh::default();
    let (s1, s2) = ((-sep / 2.0, 0.0), (sep / 2.0, 0.0));
    for j in 0..rows {
        for i in 0..cols {
            let x = -span_x / 2.0 + (i as f64 + 0.5) / cols as f64 * span_x;
            let y = span_y / 2.0 - (j as f64 + 0.5) / rows as f64 * span_y;
            let r1 = ((x - s1.0).powi(2) + (y - s1.1).powi(2)).sqrt();
            let r2 = ((x - s2.0).powi(2) + (y - s2.1).powi(2)).sqrt();
            let a = (k * r1 - w * rt.t).sin() / (1.0 + 0.15 * r1) + (k * r2 - w * rt.t + phase).sin() / (1.0 + 0.15 * r2);
            let v = (a / 2.0).clamp(-1.0, 1.0) as f32;
            let colour = if v >= 0.0 { p.accent.gamma_multiply(v) } else { p.violet.gamma_multiply(-v * 0.8) };
            let cell = Rect::from_min_size(pos2(rect.left() + i as f32 * cw, rect.top() + j as f32 * ch), vec2(cw + 0.5, ch + 0.5));
            mesh.add_colored_rect(cell, colour);
        }
    }
    painter.add(Shape::mesh(mesh));
    for s in [s1, s2] {
        let at = pos2(rect.left() + ((s.0 + span_x / 2.0) / span_x) as f32 * rect.width(), rect.center().y);
        painter.circle_filled(at, 5.0, p.amber);
    }
    readout(painter, rect, 8.0, &format!("f = {f:.2} Hz   λ = {lambda:.2} m   d = {sep:.1} m   Δφ = {:.0}°", phase.to_degrees()), p.text);
}

fn draw_descent(painter: &egui::Painter, rect: Rect, p: &Palette, sim: &Simulation, rt: &mut Runtime, dt: f64) {
    let rect = Rect::from_min_max(rect.min + vec2(34.0, 0.0), rect.max - vec2(0.0, 16.0));
    let Some((_, src, e)) = sim.functions.first() else { return };
    let lr = sim.value("learning_rate");
    let max_steps = sim.value("steps").max(1.0) as usize;
    rt.gd_timer += dt;
    let diverged = !rt.gd_x.is_finite() || rt.gd_x.abs() > 1e4;
    while rt.gd_timer > 0.12 && rt.gd_steps < max_steps && !diverged {
        rt.gd_timer -= 0.12;
        rt.gd_x -= lr * simulation_lab::derivative(e, rt.gd_x, &sim.params);
        rt.gd_steps += 1;
        rt.gd_path.push(rt.gd_x);
    }
    let (mut a, mut b) = sim.x_range;
    if (a, b) == (-10.0, 10.0) {
        let s = sim.value("start");
        a = (s - 2.0).min(-3.0);
        b = (s + 2.0).max(4.0);
    }
    let n = 400;
    let xs: Vec<f64> = (0..=n).map(|i| a + (b - a) * i as f64 / n as f64).collect();
    let ys: Vec<f64> = xs.iter().map(|x| e.eval(*x, &sim.params)).collect();
    let finite: Vec<f64> = ys.iter().copied().filter(|v| v.is_finite()).collect();
    let (y0, y1) = finite.iter().fold((f64::MAX, f64::MIN), |(lo, hi), v| (lo.min(*v), hi.max(*v)));
    let pad = ((y1 - y0) * 0.1).max(0.5);
    let view = View { rect, x0: a, x1: b, y0: y0 - pad, y1: y1 + pad };
    view.grid(painter, p);
    let pts: Vec<Pos2> = xs.iter().zip(&ys).filter(|(_, y)| y.is_finite()).map(|(x, y)| view.to_screen(*x, *y)).collect();
    painter.add(Shape::line(pts, stroke(2.2, p.accent)));
    let visible = |x: f64| x >= a && x <= b;
    let path: Vec<Pos2> = rt.gd_path.iter().copied().filter(|x| x.is_finite() && visible(*x)).map(|x| view.to_screen(x, e.eval(x, &sim.params).clamp(view.y0, view.y1))).collect();
    for (k, pt) in path.iter().enumerate() {
        painter.circle_filled(*pt, 3.0, p.amber.gamma_multiply(0.3 + 0.7 * k as f32 / path.len().max(1) as f32));
    }
    if path.len() >= 2 {
        painter.add(Shape::line(path.clone(), stroke(1.0, p.amber.gamma_multiply(0.6))));
    }
    if let Some(last) = path.last() {
        painter.circle_filled(*last, 8.0, p.amber);
    }
    let loss = e.eval(rt.gd_x, &sim.params);
    let status = if diverged || !visible(rt.gd_x) && rt.gd_steps > 2 { "DIVERGING — the learning rate is too large" } else if rt.gd_steps >= max_steps { "done" } else { "descending…" };
    readout(painter, rect, 8.0, &format!("loss(x) = {src}\nstep {} / {max_steps}   x = {:.4}   loss = {:.4}   lr = {lr}\n{status}", rt.gd_steps, rt.gd_x, loss), if status.starts_with("DIVERG") { p.red } else { p.text });
}

fn draw_sorting(painter: &egui::Painter, rect: Rect, p: &Palette, sim: &Simulation, rt: &mut Runtime, dt: f64) {
    if rt.sort.is_empty() {
        return;
    }
    rt.sort_pos = (rt.sort_pos + dt * sim.value("speed")).min((rt.sort.len() - 1) as f64);
    let (values, (i, j)) = &rt.sort[rt.sort_pos as usize];
    let done = rt.sort_pos as usize >= rt.sort.len() - 1;
    let n = values.len().max(1);
    let w = rect.width() / n as f32;
    for (k, v) in values.iter().enumerate() {
        let h = *v as f32 * (rect.height() - 30.0);
        let bar = Rect::from_min_max(pos2(rect.left() + k as f32 * w + 1.0, rect.bottom() - h), pos2(rect.left() + (k + 1) as f32 * w - 1.0, rect.bottom()));
        let colour = if done {
            p.green
        } else if k == *i || k == *j {
            p.amber
        } else {
            p.accent.gamma_multiply(0.75)
        };
        painter.rect_filled(bar, Rounding::same(1.5), colour);
    }
    readout(painter, rect, 8.0, &format!("{} sort · step {} of {}{}", sim.algorithm, rt.sort_pos as usize, rt.sort.len() - 1, if done { " · sorted" } else { "" }), p.text);
}

/// A simulation's read-out in the top-left corner of its canvas, `top` points under its edge.
/// Its figures (three spaces apart) share a line while the canvas is wide enough for them and
/// go on to the next line when it is not; a single figure wider than the canvas wraps. Nothing
/// is cut by a narrow block. The height it took.
fn readout(painter: &egui::Painter, rect: Rect, top: f32, text: &str, colour: Color32) -> f32 {
    let font = FontId::monospace(11.5);
    let room = (rect.width() - 20.0).max(60.0);
    let width = |s: &str| painter.layout_no_wrap(s.to_string(), font.clone(), colour).size().x;
    let mut lines: Vec<String> = Vec::new();
    for given in text.lines() {
        let mut line = String::new();
        for figure in given.split("   ") {
            if !line.is_empty() && width(&format!("{line}   {figure}")) > room {
                lines.push(std::mem::take(&mut line));
            }
            if !line.is_empty() {
                line.push_str("   ");
            }
            line.push_str(figure);
        }
        lines.push(line);
    }
    let mut used = 0.0;
    for line in lines {
        let galley = painter.layout(line, font.clone(), colour, room);
        let height = galley.size().y;
        painter.galley(pos2(rect.left() + 10.0, rect.top() + top + used), galley, colour);
        used += height;
    }
    used
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_plot_gets_a_full_height_canvas_beside_the_controls() {
        // The old window allocated its painter inside a one-row horizontal layout: the canvas was
        // a sliver and the plot area stayed black. Now both rects span the whole body.
        let body = Rect::from_min_size(pos2(10.0, 50.0), vec2(860.0, 480.0));
        let (controls, plot) = split_layout(body);
        assert_eq!(controls.height(), 480.0);
        assert_eq!(plot.height(), 480.0);
        assert_eq!(controls.width(), 250.0);
        assert!(plot.width() > 500.0 && plot.left() > controls.right() && plot.right() == body.right());
        // A narrow block keeps the controls to 40 %.
        let (c, p) = split_layout(Rect::from_min_size(pos2(0.0, 0.0), vec2(420.0, 300.0)));
        assert!(c.width() <= 168.0 && p.width() > 200.0);
    }

    #[test]
    fn a_parabola_draws_one_continuous_200_segment_curve() {
        let xs = sample_xs(-10.0, 10.0);
        assert_eq!(xs.len(), CURVE_SEGMENTS + 1);
        assert_eq!(CURVE_SEGMENTS, 200);
        let (a, b, c) = (1.0, -2.0, -3.0);
        let ys: Vec<f64> = xs.iter().map(|x| a * x * x + b * x + c).collect();
        let view = View { rect: Rect::from_min_size(pos2(0.0, 0.0), vec2(600.0, 400.0)), x0: -10.0, x1: 10.0, y0: -5.0, y1: 120.0 };
        let runs = curve_runs(&xs, &ys, |x, y| view.to_screen(x, y), view.y0, view.y1);
        assert_eq!(runs.len(), 1, "continuous");
        assert_eq!(runs[0].len(), 201);
        assert!(runs[0].iter().filter(|p| view.rect.expand(0.5).contains(**p)).count() > 150, "mostly inside the plot");
        // sin(x) too; a pole splits the curve instead of drawing a wall.
        let sin: Vec<f64> = xs.iter().map(|x| x.sin()).collect();
        assert_eq!(curve_runs(&xs, &sin, |x, y| view.to_screen(x, y), -1.2, 1.2).len(), 1);
        let pole: Vec<f64> = xs.iter().map(|x| 1.0 / x).collect();
        assert!(curve_runs(&xs, &pole, |x, y| view.to_screen(x, y), -2.0, 2.0).len() >= 2);
        // ROSE colours: f(x) cyan, f'(x) crimson.
        // In the ROSE default: f(x) in the contrast cyan, f'(x) in the primary rose-crimson.
        let rose = crate::theme_engine::preset("rose_crimson").unwrap();
        assert_eq!(rose.secondary(), Color32::from_rgb(0x00, 0xF5, 0xFF));
        assert_eq!(rose.primary(), Color32::from_rgb(0xFF, 0x00, 0x55));
        assert_eq!(PLOT_CANVAS, Color32::from_rgb(0x05, 0x0A, 0x10));
    }

    #[test]
    fn simulations_open_in_the_hud_and_detach_only_on_request() {
        use crate::simulation_lab::{place_simulation, SimKind, SimulationLab};
        let _dock = crate::hud_dock::test_lock();
        let lab = SimulationLab::default();
        let plot = lab.launch(SimKind::Function, "x^2 - 2x - 3", Some("Function plotter")).unwrap();
        assert!(!place_simulation(&plot.id, &serde_json::json!({})));
        assert!(!crate::hud_dock::adopt(&dock_key(&plot.id)), "the plotter is a HUD block");
        let swing = lab.launch(SimKind::Pendulum, "", None).unwrap();
        assert!(place_simulation(&swing.id, &serde_json::json!({"separate_window": true})));
        assert!(crate::hud_dock::adopt(&dock_key(&swing.id)), "its own window only when asked");
        crate::hud_dock::forget(&dock_key(&plot.id));
        crate::hud_dock::forget(&dock_key(&swing.id));
    }

    #[test]
    fn snapshot_function_plotter_draws_a_bright_curve_not_a_black_canvas() {
        use crate::simulation_lab::{SimKind, SimulationLab};
        let lab = SimulationLab::default();
        let mut sim = lab.launch(SimKind::Function, "x^2 - 2x - 3", Some("Function plotter")).unwrap();
        sim.show_derivative = true;
        let p = Palette::new(&crate::interface::InterfaceSettings::default());
        // The curves take the active theme's colours, which other tests switch: judged by the
        // colours this drawing uses, not by a fixed cyan.
        let _theme = crate::theme_engine::test_lock();
        crate::theme_engine::finish_fade();
        let (f_colour, df_colour) = (plot_cyan(), plot_crimson());
        let size = vec2(900.0, 520.0);
        let plot = Rect::from_min_size(pos2(20.0, 20.0), size - vec2(40.0, 40.0));
        let mut snap = crate::ui_snapshot::Snapshot::new(size);
        let canvas = snap.render(2, |ctx| {
            crate::gui::install_fonts(ctx);
            egui::CentralPanel::default().frame(Frame::none()).show(ctx, |ui| {
                let painter = ui.painter_at(plot.expand(2.0));
                painter.rect_filled(plot, Rounding::ZERO, PLOT_CANVAS);
                painter.rect_stroke(plot, Rounding::ZERO, Stroke::new(1.0f32, plot_frame()));
                crate::hud_board::brackets(&painter, plot, Color32::WHITE);
                draw_function(&painter, plot.shrink(16.0), &p, &sim, None);
            });
        });
        canvas.save("simlab_function_plotter");
        // Pixels of f's colour (cyan in the default theme) and of f''s (crimson) exist.
        let near = |[r, g, b]: [u8; 3], c: Color32| r.abs_diff(c.r()) as u32 + g.abs_diff(c.g()) as u32 + b.abs_diff(c.b()) as u32 <= 90;
        let (mut cyan, mut crimson) = (0, 0);
        for y in 0..canvas.height {
            for x in 0..canvas.width {
                let px = canvas.pixel(x, y);
                if near(px, f_colour) {
                    cyan += 1;
                }
                if near(px, df_colour) {
                    crimson += 1;
                }
            }
        }
        assert!(cyan > 300, "f(x) drawn in {f_colour:?}: {cyan} px");
        assert!(crimson > 200, "f'(x) drawn in {df_colour:?}: {crimson} px");
    }
}
