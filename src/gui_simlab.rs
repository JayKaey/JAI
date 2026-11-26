//! The Simulation Lab's windows: each simulation is a window of its own (a block only in an
//! offscreen session), redrawn at 60 FPS, with the sliders on the left and the live plot or
//! animation on the right. Light on black glass in the theme's colours: ink hairlines for
//! structure, the accent only for what is live or measured, filaments with a soft glow. The
//! models live in `simulation_lab.rs` and the physics in `simulation_physics.rs`; the
//! per-window state (time, the n-body scene, the ripple tank, trails) is kept here.

use super::widget_view::WidgetHost;
use super::{muted, paint_background, stroke, Palette};
use crate::holo::Face;
use crate::holo::{a as faint, lit, mix, Light};
use crate::simulation_lab::{self, Param, SimKind, Simulation};
use crate::simulation_physics as phys;
use eframe::egui::{self, pos2, vec2, Align2, Color32, Frame, Margin, Pos2, Rect, RichText, Rounding, Sense, Shape, Stroke, ViewportBuilder, ViewportClass, ViewportCommand, ViewportId};
use std::collections::{HashMap, VecDeque};
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
    /// What the physics-based kinds step (see `advance`).
    world: World,
    /// Sir's hold on a 3D view (a Cell: the painters see the runtime shared).
    cam: std::cell::Cell<Cam>,
}

/// How Sir looks at a 3D simulation (Sir, 2026-11-22: "I want to be able to move around in
/// it"): dragging turns it and tips it, the right button (or Shift) moves it, the wheel brings
/// it nearer, a double click gives it back. Until he takes hold of it, it turns by itself.
#[derive(Clone, Copy)]
struct Cam {
    held: bool,
    /// Turn and rise (radians) while held; the view's own last ones (to take over from).
    aim: (f32, f32),
    own: (f32, f32),
    zoom: f32,
    pan: egui::Vec2,
}

impl Default for Cam {
    fn default() -> Self {
        Cam { held: false, aim: (0.0, 0.0), own: (0.0, 0.0), zoom: 1.0, pan: egui::Vec2::ZERO }
    }
}

impl Cam {
    /// A drag of (dx, dy) points: the view turns about its upright and tips toward or away.
    fn turn(&mut self, d: egui::Vec2) {
        if !self.held {
            (self.held, self.aim) = (true, self.own);
        }
        self.aim.0 -= d.x * 0.008;
        self.aim.1 = (self.aim.1 + d.y * 0.006).clamp(4f32.to_radians(), 89f32.to_radians());
    }

    /// The wheel: `k` times nearer, about the point under the pointer (`at`, from the centre).
    fn zoom_by(&mut self, k: f32, at: egui::Vec2) {
        let zoom = (self.zoom * k).clamp(0.25, 12.0);
        self.pan = at - (at - self.pan) * (zoom / self.zoom);
        self.zoom = zoom;
    }
}

impl Runtime {
    /// The turn and rise a 3D view is seen at: its own (`turn`, `rise`) until Sir holds it.
    fn aim(&self, turn: f32, rise: f32) -> (f32, f32) {
        let mut cam = self.cam.get();
        cam.own = (turn, rise);
        self.cam.set(cam);
        if cam.held { cam.aim } else { cam.own }
    }

    /// (how much nearer, how far moved) for a 3D view's scale and centre.
    fn lens(&self) -> (f32, egui::Vec2) {
        let cam = self.cam.get();
        (cam.zoom, cam.pan)
    }
}

/// The state of the simulations that `simulation_physics` runs.
enum World {
    None,
    /// The scene, scene time per second, the largest reach seen (the view only grows), the start energy.
    Gravity { scene: phys::Bodies, rate: f64, reach: f64, e0: f64 },
    Ripples(phys::Ripples),
    /// The spring, (time, stretch) of the last ten seconds, the largest stretch seen.
    Spring { spring: phys::Spring, trace: VecDeque<(f64, f64)>, reach: f64 },
    Chaos { pendulum: phys::DoublePendulum, e0: f64, lower: VecDeque<[f64; 2]>, upper: VecDeque<[f64; 2]> },
    Balls { balls: phys::Balls, e0: f64 },
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
            world: World::None,
            cam: Default::default(),
        };
        match sim.kind {
            SimKind::Gravity => {
                let scene = sim.bodies();
                // The quickest orbit goes round in about five seconds at speed 1.
                let rate = phys::quickest_period(&scene.bodies, scene.g) / 5.0;
                r.world = World::Gravity { rate, reach: scene.extent() * 1.1, e0: scene.energy(), scene };
            }
            SimKind::Ripples => r.world = World::Ripples(sim.ripples()),
            SimKind::Spring => {
                let spring = sim.spring();
                r.world = World::Spring { reach: spring.x.abs().max(0.3), spring, trace: VecDeque::new() };
            }
            SimKind::DoublePendulum => {
                let pendulum = sim.double_pendulum();
                r.world = World::Chaos { e0: pendulum.energy(), pendulum, lower: VecDeque::new(), upper: VecDeque::new() };
            }
            SimKind::Balls => {
                let balls = sim.balls();
                r.world = World::Balls { e0: balls.energy(), balls };
            }
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
    ViewportId::from_hash_of(("jai_simlab", id))
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

/// A plot's frame: structure, in ink.
fn plot_frame() -> Color32 {
    crate::hud_board::border_colour()
}
/// The first series (f(x)): the accent.
fn first_series() -> Color32 {
    crate::holo_theme::current().accent()
}
/// The second series (f′(x)): the deep tone, else ink when it is too close to the accent to tell
/// the curves apart.
fn second_series() -> Color32 {
    let t = crate::holo_theme::current();
    let (a, b) = (t.accent(), t.deep());
    let distance = a.r().abs_diff(b.r()) as u32 + a.g().abs_diff(b.g()) as u32 + a.b().abs_diff(b.b()) as u32;
    if distance < 200 { t.ink() } else { b }
}
/// Segments of every plotted curve.
pub const CURVE_SEGMENTS: usize = 200;

/// Registers every simulation with egui (each HUD frame): a window of its own (a block on the
/// board in a session without windows).
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
            let action = super::hud_block_window(ctx, &key, &title, [900.0, 560.0], |ui| draw_body(ui, &p, &sim, host));
            if action.close {
                lab.close(&sim.id);
                crate::hud_dock::forget(&key);
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
            .with_title(format!("{} — J.A.I. Simulation Lab", sim.title))
            .with_app_id("jai")
            .with_inner_size([960.0, 620.0])
            .with_min_inner_size([560.0, 380.0])
            .with_transparent(false)
            .with_decorations(false);
        if let Some(icon) = super::window_icon() {
            builder = builder.with_icon(std::sync::Arc::new(icon));
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

/// Sir closed a simulation's window: the simulation is closed. (Left open, its window was
/// asked for again in every frame and stood as its exit's last one: "I tried closing a window
/// JAI opened and it turned black", 2026-11-14.)
fn closed_by_hand(lab: &simulation_lab::SimulationLab, id: &str) {
    lab.close(id);
    crate::hud_dock::forget(&dock_key(id));
}

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
        closed_by_hand(lab, id);
        ctx.send_viewport_cmd(ViewportCommand::Visible(false));
        ctx.request_repaint_of(ViewportId::ROOT);
        return;
    }
    let p = Palette::new(&host.app.config().read(|c| c.interface.clone()));
    paint_background(ctx, &p);
    let body = |ui: &mut egui::Ui| draw_body(ui, &p, &sim, host);
    if class == ViewportClass::Embedded {
        egui::Window::new(&sim.title).id(egui::Id::new(("jai_sim_embedded", id))).default_size([900.0, 560.0]).show(ctx, body);
    } else {
        egui::CentralPanel::default().frame(Frame::none().inner_margin(Margin::same(12.0))).show(ctx, |ui| {
            if super::detached_title_bar(ui, ctx, &dock_key(id), &sim.title) && !leaving {
                closed_by_hand(lab, id);
            }
            body(ui);
        });
        super::paint_window_border(ctx);
    }
    // (Beside a game a running simulation slows to four frames a second: the card is the game's.)
    ctx.request_repaint_after(Duration::from_millis(if sim.paused || crate::hud_feel::game_in_front() { 250 } else { 16 }));
}

/// The control column's width and the plot canvas inside `avail` (side by side; the controls
/// take at most 40 % of a narrow block).
pub(super) fn split_layout(avail: Rect) -> (Rect, Rect) {
    let controls_w = 250.0f32.min(avail.width() * 0.4).max(150.0);
    let controls = Rect::from_min_size(avail.min, vec2(controls_w, avail.height()));
    let plot = Rect::from_min_max(pos2(avail.left() + controls_w + 12.0, avail.top()), avail.max);
    (controls, plot)
}

fn draw_body(ui: &mut egui::Ui, p: &Palette, sim: &Simulation, host: &WidgetHost) {
    let lab = &host.app.tools().stark.simlab;
    // (The window's bar, or the block's frame, already wears the title and CLOSE: once is enough,
    // Sir's screenshot of 2026-11-22 had both twice.)
    ui.horizontal_wrapped(|ui| {
        if !sim.title.eq_ignore_ascii_case(sim.kind.label()) {
            super::chip(ui, sim.kind.label(), p.deep);
        }
        if crate::holo_ui::key(ui, if sim.paused { "Resume" } else { "Pause" }).clicked() {
            lab.toggle_pause(&sim.id);
        }
        if sim.kind.turns_3d() && crate::holo_ui::key_on(ui, "3D", sim.three_d).clicked() {
            lab.set_3d(&sim.id, !sim.three_d);
        }
        if crate::holo_ui::key(ui, "Restart").clicked() {
            lab.restart(&sim.id);
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
        // (Nothing of the column runs under the plot: it ended cut off there.)
        ui.set_clip_rect(controls_rect.intersect(ui.clip_rect()));
        crate::holo_block::faded(ui, egui::ScrollArea::vertical().id_salt(("sim_controls", &sim.id)).max_height(controls_rect.height()), |ui| controls(ui, p, sim, host));
    });
    let deep = sim.three_d && matches!(sim.kind, SimKind::Gravity | SimKind::Ripples | SimKind::Surface);
    let held = deep || sim.kind == SimKind::Function;
    let response = ui.allocate_rect(plot_rect, if held { Sense::click_and_drag() } else { Sense::hover() });
    // (Read before the plot is drawn: the context is never asked inside its own lock.)
    let (wheel, shift, secondary) = if response.hovered() || response.dragged() {
        ui.input(|i| (i.zoom_delta() * (i.smooth_scroll_delta.y / 240.0).exp(), i.modifiers.shift, i.pointer.secondary_down()))
    } else {
        (1.0, false, false)
    };
    let painter = ui.painter_at(plot_rect.expand(2.0));
    painter.rect_filled(plot_rect, Rounding::ZERO, crate::holo::theme().void());
    crate::holo::outline(&painter, &crate::holo::plate(plot_rect, [0.0; 4]), plot_frame());
    crate::hud_board::brackets(&painter, plot_rect, crate::holo::theme().ink());
    let mut guard = RUNTIMES.lock().unwrap_or_else(|e| e.into_inner());
    let runtimes = guard.get_or_insert_with(HashMap::new);
    let rt = runtimes.entry(sim.id.clone()).or_insert_with(|| Runtime::new(sim));
    if rt.revision != sim.revision {
        // Slider changes keep the pendulum swinging from where it is only when its own
        // start angle did not change; everything else restarts cleanly. Sir's view stays his.
        let cam = rt.cam.get();
        *rt = Runtime::new(sim);
        rt.cam.set(cam);
    }
    let now = Instant::now();
    let dt = if sim.paused { 0.0 } else { now.duration_since(rt.last).as_secs_f64().min(0.05) };
    rt.last = now;
    rt.t += dt;
    let inner = plot_rect.shrink(16.0);
    if deep {
        let mut cam = rt.cam.get();
        if response.double_clicked() {
            cam = Cam::default();
        } else if response.dragged() && (shift || secondary) {
            cam.pan += response.drag_delta();
        } else if response.dragged() {
            cam.turn(response.drag_delta());
        }
        if let Some(at) = response.hover_pos().filter(|_| wheel != 1.0) {
            cam.zoom_by(wheel, at - inner.center());
        }
        rt.cam.set(cam);
        hint(&painter, plot_rect, "DRAG TURNS · RIGHT-DRAG MOVES · WHEEL ZOOMS · DOUBLE-CLICK RESETS");
    } else if sim.kind == SimKind::Function {
        let view = View::of(plot_area(inner), sim.plane());
        if response.double_clicked() {
            lab.set_view(&sim.id, None);
        } else if response.dragged() && response.drag_delta() != egui::Vec2::ZERO {
            let (x, y) = view.to_world(view.rect.center() - response.drag_delta());
            let (cx, cy) = view.to_world(view.rect.center());
            lab.set_view(&sim.id, Some([view.x0 + x - cx, view.x1 + x - cx, view.y0 + y - cy, view.y1 + y - cy]));
        } else if let Some(at) = response.hover_pos().filter(|_| wheel != 1.0) {
            lab.set_view(&sim.id, Some(simulation_lab::zoomed([view.x0, view.x1, view.y0, view.y1], wheel as f64, view.to_world(at))));
        }
        hint(&painter, plot_rect, "DRAG MOVES · WHEEL ZOOMS · DOUBLE-CLICK FITS");
    }
    advance(sim, rt, dt);
    match sim.kind {
        SimKind::Gravity | SimKind::Ripples | SimKind::StandingWave | SimKind::Doppler | SimKind::Sound | SimKind::Light | SimKind::Spring | SimKind::DoublePendulum | SimKind::Balls | SimKind::Surface => draw_physics(&painter, inner, sim, rt),
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
    ui.label(RichText::new("PARAMETERS").font(crate::holo::font(Face::Label, 9.0)).color(p.muted).extra_letter_spacing(1.0));
    ui.spacing_mut().slider_width = 150.0;
    for prm in &sim.params {
        let Param { name, label, value, min, max, unit } = prm.clone();
        let mut v = value;
        ui.label(RichText::new(format!("{label}{}", if unit.is_empty() { String::new() } else { format!(" ({unit})") })).font(crate::holo::font(Face::Body, 12.0)).color(p.text));
        let step = ((max - min) / 400.0).max(1e-4);
        if ui.add(crate::holo_ui::Slider::new(&mut v, min..=max).step_by(if (max - min) > 50.0 { 1.0 } else { step })).changed() {
            lab.set_param(&sim.id, &name, v);
        }
    }
    if sim.kind == SimKind::Function {
        ui.add_space(4.0);
        let (mut d, mut i) = (sim.show_derivative, sim.show_integral);
        // One option per line (they wrap inside the column instead of running off its edge).
        if crate::holo_ui::toggle(ui, &mut d, "Show derivative f′(x)").changed() {
            lab.set_option(&sim.id, Some(d), None);
        }
        if crate::holo_ui::toggle(ui, &mut i, "Shade the integral").changed() {
            lab.set_option(&sim.id, None, Some(i));
        }
        plot_editor(ui, p, sim, lab);
    }
    if sim.kind == SimKind::Surface {
        ui.add_space(4.0);
        for (label, src, _) in &sim.functions {
            ui.label(RichText::new(format!("{label}(x, y) = {src}")).font(crate::holo::font(Face::Mono, 11.5)).color(p.accent));
        }
    }
    if sim.kind == SimKind::Sorting {
        muted(ui, format!("Algorithm: {}", sim.algorithm));
    }
    ui.add_space(8.0);
    ui.label(RichText::new("READOUT").font(crate::holo::font(Face::Label, 9.0)).color(p.muted).extra_letter_spacing(1.0));
    ui.add(egui::Label::new(RichText::new(sim.readout()).font(crate::holo::font(Face::Body, 12.0)).color(p.text)).wrap());
    ui.add_space(8.0);
    muted(
        ui,
        match sim.kind {
            SimKind::Function => "Type a curve or a point above, or tell JAI: “plot x cubed and sin(x)”.",
            SimKind::Projectile => "Drag the sliders, or tell JAI: “set the angle to 60 degrees and double the velocity”.",
            _ => "Drag the sliders, or tell JAI what to change.",
        },
    );
}

/// A plot's GeoGebra side: the input line ("g(x) = sin x", "A = (2, 3)"), what stands on the
/// plot (each with a key that takes it off) and the four ends of its axes.
fn plot_editor(ui: &mut egui::Ui, p: &Palette, sim: &Simulation, lab: &simulation_lab::SimulationLab) {
    let id = egui::Id::new(("plot_input", &sim.id));
    let (mut text, mut said) = ui.data_mut(|d| d.get_temp::<(String, String)>(id)).unwrap_or_default();
    ui.add_space(6.0);
    ui.label(RichText::new("INPUT").font(crate::holo::font(Face::Label, 9.0)).color(p.muted).extra_letter_spacing(1.0));
    // (The key first, from the right: the line takes what is left, so the row fits the column.)
    let entered = ui
        .horizontal(|ui| {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let added = crate::holo_ui::key(ui, "ADD").clicked();
                let line = crate::holo_ui::slot(ui, egui::TextEdit::singleline(&mut text).desired_width(ui.available_width()).hint_text("g(x) = sin x   A = (2, 3)").font(crate::holo::font(Face::Mono, 12.0)));
                added || (line.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)))
            })
            .inner
        })
        .inner;
    if entered && !text.trim().is_empty() {
        match lab.plot_input(&sim.id, &text) {
            Ok(_) => (text, said) = (String::new(), String::new()),
            Err(e) => said = e,
        }
    }
    if !said.is_empty() {
        ui.label(RichText::new(&said).font(crate::holo::font(Face::Body, 11.5)).color(crate::holo::theme().alert()));
    }
    ui.data_mut(|d| d.insert_temp(id, (text, said)));
    let row = |ui: &mut egui::Ui, text: String, colour: Color32, what: &str| {
        ui.horizontal(|ui| {
            ui.label(RichText::new(text).font(crate::holo::font(Face::Mono, 11.5)).color(colour));
            if crate::holo_ui::key(ui, "×").on_hover_text("Take it off the plot").clicked() {
                lab.remove_object(&sim.id, what);
            }
        });
    };
    for (k, (label, src, _)) in sim.functions.iter().enumerate() {
        row(ui, format!("{label}(x) = {src}"), curve_colours(p)[k % 6], label);
    }
    for (name, x, y) in &sim.points {
        row(ui, format!("{name} = ({}, {})", label(*x), label(*y)), crate::holo::theme().ink(), name);
    }
    ui.add_space(6.0);
    ui.label(RichText::new("AXES").font(crate::holo::font(Face::Label, 9.0)).color(p.muted).extra_letter_spacing(1.0));
    let mut v = sim.plane();
    let was = v;
    for (axis, at) in [("x", 0), ("y", 2)] {
        let speed = (v[at + 1] - v[at]) / 200.0;
        ui.horizontal(|ui| {
            ui.add(crate::holo_ui::Number::new(&mut v[at]).speed(speed).prefix(format!("{axis} ")));
            ui.add(crate::holo_ui::Number::new(&mut v[at + 1]).speed(speed).prefix("to "));
        });
    }
    if v != was {
        lab.set_view(&sim.id, Some(v));
    }
    if sim.view.is_some() && crate::holo_ui::key(ui, "FIT").on_hover_text("Fit the view to the curves again").clicked() {
        lab.set_view(&sim.id, None);
    }
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
    fn of(rect: Rect, [x0, x1, y0, y1]: [f64; 4]) -> View {
        View { rect, x0, x1, y0, y1 }
    }

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
        let ink_colour = crate::holo::theme().ink();
        let (minor, axis, ink) = (crate::holo::a(ink_colour, 0.09), crate::holo::a(ink_colour, 0.45), crate::holo::theme().muted());
        let step = |span: f64| {
            let raw = span / 8.0;
            let mag = 10f64.powf(raw.abs().log10().floor());
            [1.0, 2.0, 5.0, 10.0].iter().map(|m| m * mag).find(|s| *s >= raw).unwrap_or(raw)
        };
        let font = crate::holo::font(Face::Mono, 10.0);
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

/// f(x) in the accent, then the theme's other tones for further functions.
fn curve_colours(_p: &Palette) -> [Color32; 6] {
    let t = crate::holo::theme();
    [first_series(), second_series(), t.ink(), t.warn(), t.muted(), t.hot()]
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

/// A plot's own controls, said once in its lower right corner.
fn hint(painter: &egui::Painter, rect: Rect, text: &str) {
    painter.text(rect.right_bottom() - vec2(6.0, 3.0), Align2::RIGHT_BOTTOM, text, crate::holo::font(Face::Label, 8.0), faint(crate::holo::theme().ink(), 0.4));
}

/// The plane of a plot inside its canvas (room on the left and below for the axes' figures).
fn plot_area(rect: Rect) -> Rect {
    Rect::from_min_max(rect.min + vec2(34.0, 0.0), rect.max - vec2(0.0, 16.0))
}

fn draw_function(painter: &egui::Painter, rect: Rect, p: &Palette, sim: &Simulation, hover: Option<Pos2>) {
    let rect = plot_area(rect);
    let [a, b, y0, y1] = sim.plane();
    let n = CURVE_SEGMENTS;
    let xs = sample_xs(a, b);
    let mut series: Vec<(Vec<f64>, Color32, bool)> = Vec::new();
    for (k, (_, _, e)) in sim.functions.iter().enumerate() {
        let colour = curve_colours(p)[k % 6];
        series.push((xs.iter().map(|x| e.eval(*x, &sim.params)).collect(), colour, false));
        if sim.show_derivative {
            // f′(x) in the second series' tone (dashed for the further functions).
            series.push((xs.iter().map(|x| simulation_lab::derivative(e, *x, &sim.params)).collect(), if k == 0 { second_series() } else { colour.gamma_multiply(0.6) }, k > 0));
        }
    }
    let view = View::of(rect, [a, b, y0, y1]);
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
    // Roots (rings), maxima and minima (diamonds), where two curves meet (white dots), his points.
    let th = crate::holo::theme();
    for (k, (_, _, e)) in sim.functions.iter().enumerate() {
        let colour = curve_colours(p)[k % 6];
        for r in simulation_lab::roots(e, a, b, &sim.params) {
            painter.circle_stroke(view.to_screen(r, 0.0), 4.5, stroke(1.6, colour));
        }
        for (x, y, _) in simulation_lab::turning_points(e, a, b, &sim.params) {
            let at = view.to_screen(x, y);
            painter.add(Shape::closed_line([vec2(0.0, -5.0), vec2(5.0, 0.0), vec2(0.0, 5.0), vec2(-5.0, 0.0)].map(|d| at + d).to_vec(), stroke(1.4, colour)));
        }
        for (_, _, g) in &sim.functions[k + 1..] {
            for (x, y) in simulation_lab::crossings(e, g, a, b, &sim.params) {
                painter.circle_filled(view.to_screen(x, y), 3.5, th.hot());
            }
        }
    }
    let mut taken = Vec::new();
    for (name, x, y) in &sim.points {
        let at = view.to_screen(*x, *y);
        painter.circle_filled(at, 4.0, th.accent());
        painter.circle_stroke(at, 7.0, stroke(1.0, faint(th.ink(), 0.5)));
        tag_free(painter, &mut taken, &[(at + vec2(8.0, -6.0), Align2::LEFT_BOTTOM), (at + vec2(-8.0, -6.0), Align2::RIGHT_BOTTOM)], &format!("{name} ({}, {})", label(*x), label(*y)), th.ink());
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
        painter.text(h + vec2(10.0, -10.0), Align2::LEFT_BOTTOM, lines.join("\n"), crate::holo::font(Face::Mono, 11.0), p.text);
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
        painter.circle_filled(at, 8.0, p.accent);
        painter.circle_stroke(at, 12.0, stroke(1.0, p.accent.gamma_multiply(0.5)));
        let next = rt.path.get(i + 1).copied().unwrap_or((ts, x, y));
        let dt = (next.0 - ts).max(1e-6);
        let (vx, vy) = ((next.1 - x) / dt, (next.2 - y) / dt);
        let tip = view.to_screen(x + vx * 0.15 * max_x / sim.value("velocity").max(1.0), y + vy * 0.15 * max_x / sim.value("velocity").max(1.0));
        painter.arrow(at, tip - at, stroke(2.0, p.nominal));
        readout(painter, rect, 8.0, &format!("t = {ts:.2} s   x = {x:.1} m   y = {y:.1} m   v = {:.1} m/s", (vx * vx + vy * vy).sqrt()), p.text);
    }
    painter.line_segment([view.to_screen(0.0, 0.0), view.to_screen(view.x1, 0.0)], stroke(2.0, p.nominal.gamma_multiply(0.7)));
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
    painter.circle_filled(bob, 14.0, p.accent);
    painter.circle_stroke(bob, 18.0, stroke(1.0, p.accent.gamma_multiply(0.5)));
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
    for (x, rad, m, v, colour, name) in [(rt.x1, r1, m1, rt.v1, p.accent, "m₁"), (rt.x2, r2, m2, rt.v2, p.deep, "m₂")] {
        let c = pos2(view.to_screen(x, 0.0).x, track_y - rad as f32 * px_per_m);
        // A faint pane with a bright edge (a strong fill of the accent reads olive).
        painter.circle_filled(c, rad as f32 * px_per_m, faint(crate::holo::theme().ink(), 0.09));
        painter.circle_stroke(c, rad as f32 * px_per_m, stroke(1.8, colour));
        painter.text(c, Align2::CENTER_CENTER, format!("{name}\n{m:.1} kg"), crate::holo::font(Face::Mono, 11.0), crate::holo::theme().ink());
        if v.abs() > 0.01 {
            painter.arrow(c - vec2(0.0, rad as f32 * px_per_m + 14.0), vec2(v as f32 * px_per_m * 0.6, 0.0), stroke(2.0, p.nominal));
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
            let colour = field_colour((a / 2.0).clamp(-1.0, 1.0) as f32);
            let cell = Rect::from_min_size(pos2(rect.left() + i as f32 * cw, rect.top() + j as f32 * ch), vec2(cw + 0.5, ch + 0.5));
            mesh.add_colored_rect(cell, colour);
        }
    }
    painter.add(Shape::mesh(mesh));
    let mut light = Light::new(painter);
    for s in [s1, s2] {
        let at = pos2(rect.left() + ((s.0 + span_x / 2.0) / span_x) as f32 * rect.width(), rect.center().y);
        light.spark(at, 9.0, crate::holo::theme().hot());
    }
    light.paint(painter);
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
        painter.circle_filled(*pt, 3.0, p.accent.gamma_multiply(0.3 + 0.7 * k as f32 / path.len().max(1) as f32));
    }
    if path.len() >= 2 {
        painter.add(Shape::line(path.clone(), stroke(1.0, p.accent.gamma_multiply(0.6))));
    }
    if let Some(last) = path.last() {
        painter.circle_filled(*last, 8.0, p.accent);
    }
    let loss = e.eval(rt.gd_x, &sim.params);
    let status = if diverged || !visible(rt.gd_x) && rt.gd_steps > 2 { "DIVERGING — the learning rate is too large" } else if rt.gd_steps >= max_steps { "done" } else { "descending…" };
    readout(painter, rect, 8.0, &format!("loss(x) = {src}\nstep {} / {max_steps}   x = {:.4}   loss = {:.4}   lr = {lr}\n{status}", rt.gd_steps, rt.gd_x, loss), if status.starts_with("DIVERG") { p.alert } else { p.text });
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
        // Faint bars with a bright cap; the two being compared are one solid block.
        let (edge, fill) = if done {
            (p.nominal, faint(p.nominal, 0.1))
        } else if k == *i || k == *j {
            (crate::holo::theme().hot(), crate::holo::theme().hot())
        } else {
            (p.accent, faint(crate::holo::theme().ink(), 0.1))
        };
        painter.rect_filled(bar, Rounding::ZERO, fill);
        painter.line_segment([bar.left_top(), bar.right_top()], stroke(2.0, edge));
    }
    readout(painter, rect, 8.0, &format!("{} sort · step {} of {}{}", sim.algorithm, rt.sort_pos as usize, rt.sort.len() - 1, if done { " · sorted" } else { "" }), p.text);
}

// ------------------------------------------------------- the physics-based simulations ----

/// Steps what `simulation_physics` runs by one frame (`dt` is 0 while paused).
fn advance(sim: &Simulation, rt: &mut Runtime, dt: f64) {
    if dt <= 0.0 {
        return;
    }
    match &mut rt.world {
        World::Gravity { scene, rate, reach, .. } => {
            scene.advance(dt * *rate * sim.value("speed"));
            *reach = reach.max(scene.extent() * 1.05);
        }
        World::Ripples(tank) => {
            for _ in 0..sim.value("speed").round().max(1.0) as usize {
                tank.step();
            }
        }
        World::Spring { spring, trace, reach } => {
            spring.advance(dt);
            trace.push_back((spring.t, spring.x));
            while trace.front().is_some_and(|(s, _)| spring.t - s > 10.0) {
                trace.pop_front();
            }
            *reach = reach.max(spring.x.abs() * 1.05);
        }
        World::Chaos { pendulum, lower, upper, .. } => {
            pendulum.advance(dt);
            let (p1, p2) = pendulum.bobs();
            upper.push_back(p1);
            lower.push_back(p2);
            if upper.len() > 120 {
                upper.pop_front();
            }
            if lower.len() > 500 {
                lower.pop_front();
            }
        }
        World::Balls { balls, .. } => balls.advance(dt),
        World::None => {}
    }
}

/// Paints the kinds that `simulation_physics` runs.
fn draw_physics(painter: &egui::Painter, rect: Rect, sim: &Simulation, rt: &Runtime) {
    match sim.kind {
        SimKind::Gravity => draw_gravity(painter, rect, sim, rt),
        SimKind::Ripples => draw_ripples(painter, rect, sim, rt),
        SimKind::StandingWave => draw_standing(painter, rect, sim, rt),
        SimKind::Doppler => draw_doppler(painter, rect, sim, rt),
        SimKind::Sound => draw_sound(painter, rect, sim, rt),
        SimKind::Light => draw_light(painter, rect, sim),
        SimKind::Spring => draw_spring(painter, rect, rt),
        SimKind::DoublePendulum => draw_chaos(painter, rect, rt),
        SimKind::Balls => draw_balls(painter, rect, rt),
        SimKind::Surface => draw_surface(painter, rect, sim, rt),
        _ => {}
    }
}

/// Light of an intensity 0-1 through the theme: black glass, grey ink, the accent, then white-hot
/// (a dim accent on black reads olive, so the low end goes through ink instead).
fn ramp(t: f32) -> Color32 {
    let th = crate::holo::theme();
    let t = t.clamp(0.0, 1.0);
    let grey = mix(th.void(), th.ink(), 0.45);
    if t < 0.45 {
        mix(th.void(), grey, (t / 0.45).powf(1.6))
    } else if t < 0.85 {
        mix(grey, th.accent(), (t - 0.45) / 0.4)
    } else {
        mix(th.accent(), th.hot(), (t - 0.85) / 0.15)
    }
}

/// A signed wave: crests light up through the ramp, troughs stay a cool grey.
fn field_colour(v: f32) -> Color32 {
    let th = crate::holo::theme();
    if v >= 0.0 { ramp(v) } else { mix(th.void(), th.ink(), 0.22 * -v) }
}

/// `cols` × `rows` cells over `rect`, one colour per vertex so the cells blend into each other.
fn grid_mesh(rect: Rect, cols: usize, rows: usize, colour: impl Fn(usize, usize) -> Color32) -> Shape {
    let mut mesh = egui::Mesh::default();
    for j in 0..=rows {
        for i in 0..=cols {
            let at = pos2(rect.left() + rect.width() * i as f32 / cols as f32, rect.top() + rect.height() * j as f32 / rows as f32);
            mesh.colored_vertex(at, colour(i, j));
        }
    }
    let w = cols as u32 + 1;
    for j in 0..rows as u32 {
        for i in 0..cols as u32 {
            let k = j * w + i;
            mesh.add_triangle(k, k + 1, k + w);
            mesh.add_triangle(k + 1, k + w + 1, k + w);
        }
    }
    Shape::mesh(mesh)
}

/// Short samples of line colours with what each line is.
fn legend(painter: &egui::Painter, at: Pos2, items: &[(Color32, &str)]) {
    let font = crate::holo::font(Face::Mono, 10.0);
    let mut x = at.x;
    for (colour, text) in items {
        painter.line_segment([pos2(x, at.y), pos2(x + 16.0, at.y)], stroke(2.0, *colour));
        let used = painter.text(pos2(x + 20.0, at.y), Align2::LEFT_CENTER, *text, font.clone(), crate::holo::theme().muted());
        x = used.right() + 16.0;
    }
}

fn tag(painter: &egui::Painter, at: Pos2, anchor: Align2, text: &str, colour: Color32) {
    painter.text(at, anchor, text, crate::holo::font(Face::Mono, 10.0), colour);
}

/// A tag at the first of `spots` where no tag in `taken` stands, or none: crowded names give
/// way rather than print over each other.
fn tag_free(painter: &egui::Painter, taken: &mut Vec<Rect>, spots: &[(Pos2, Align2)], text: &str, colour: Color32) {
    let galley = painter.layout_no_wrap(text.to_string(), crate::holo::font(Face::Mono, 10.0), colour);
    if let Some(rect) = spots.iter().map(|&(at, anchor)| anchor.anchor_size(at, galley.size())).find(|rect| !taken.iter().any(|t| t.intersects(*rect))) {
        taken.push(rect);
        painter.galley(rect.min, galley, colour);
    }
}

/// A canvas under its read-out (the read-out takes the top 44 points).
fn below_readout(rect: Rect) -> Rect {
    Rect::from_min_max(rect.min + vec2(0.0, 44.0), rect.max - vec2(0.0, 4.0))
}

/// The points of a circle, for a ribbon.
fn circle_points(centre: Pos2, radius: f32, n: usize) -> Vec<Pos2> {
    (0..n).map(|i| centre + vec2((i as f32 / n as f32 * std::f32::consts::TAU).cos(), (i as f32 / n as f32 * std::f32::consts::TAU).sin()) * radius).collect()
}

fn draw_gravity(painter: &egui::Painter, rect: Rect, sim: &Simulation, rt: &Runtime) {
    let World::Gravity { scene, reach, e0, .. } = &rt.world else { return };
    let th = crate::holo::theme();
    let area = below_readout(rect);
    let deep = sim.three_d;
    // (Leaning back, the plane takes about half its width in height: it may be drawn larger.)
    let (zoom, pan) = if deep { rt.lens() } else { (1.0, egui::Vec2::ZERO) };
    let radius = if deep { ((area.width() - 40.0) / 2.7).min((area.height() - 40.0) / 1.1) } else { area.width().min(area.height()) / 2.0 - 24.0 }.max(40.0) * zoom;
    let centre = area.center() + pan;
    // Distance from the centre by its square root when the planets would otherwise leave the inner ones a dot.
    let flat = |p: [f64; 2]| {
        let r = (p[0].hypot(p[1]) / reach).min(4.0);
        let k = if scene.root_scale { r.sqrt() } else { r } as f32;
        let angle = p[1].atan2(p[0]) as f32;
        egui::Vec2::angled(angle) * (k * radius)
    };
    let heavy = (0..scene.bodies.len()).max_by(|a, b| scene.bodies[*a].mass.total_cmp(&scene.bodies[*b].mass)).unwrap_or(0);
    // 3D: the plane of the orbits seen from 30° above, turning slowly, in perspective, sunk
    // into the well the masses make of it (their potential, softened, its depth capped).
    let top = scene.bodies.get(heavy).map_or(1.0, |b| b.mass.max(1e-300));
    let wells: Vec<(egui::Vec2, f32)> = scene.bodies.iter().map(|b| (flat(b.pos), (b.mass / top) as f32)).collect();
    let soft = radius * 0.15;
    let (turn, rise) = if deep { rt.aim((20.0 + 4.0 * rt.t as f32).to_radians(), 30f32.to_radians()) } else { (0.0, 0.0) };
    // (where on the screen, how large against the plane seen from above, how near: 0-1)
    let view = |at: egui::Vec2| -> (Pos2, f32, f32) {
        if !deep {
            return (centre + vec2(at.x, -at.y), 1.0, 1.0);
        }
        let h = -0.42 * radius * wells.iter().map(|(p, m)| m * soft / ((at - *p).length_sq() + soft * soft).sqrt()).sum::<f32>().min(1.3);
        let (x, y) = (at.x * turn.cos() - at.y * turn.sin(), at.x * turn.sin() + at.y * turn.cos());
        let (up, near) = (y * rise.sin() + h * rise.cos(), -y * rise.cos() + h * rise.sin());
        let scale = 3.5 * radius / (3.5 * radius - near.min(1.5 * radius));
        (centre + vec2(0.0, -0.13 * radius) + vec2(x, -up) * scale, scale, (near / radius * 0.5 + 0.5).clamp(0.0, 1.0))
    };
    // The origin's axes and range circles: the scale the scene is drawn at. In 3D the plane is
    // a sheet of rings and spokes that sinks where the masses are.
    if deep {
        let tau = std::f32::consts::TAU;
        for k in 1..=8 {
            let ring: Vec<Pos2> = (0..=72).map(|i| view(egui::Vec2::angled(i as f32 / 72.0 * tau) * (radius * k as f32 / 8.0)).0).collect();
            crate::holo::path(painter, &ring, faint(th.ink(), if [2, 4, 8].contains(&k) { 0.16 } else { 0.07 }));
        }
        for s in 0..24 {
            let spoke: Vec<Pos2> = (0..=24).map(|i| view(egui::Vec2::angled(s as f32 / 24.0 * tau) * (radius * i as f32 / 24.0)).0).collect();
            crate::holo::path(painter, &spoke, faint(th.ink(), if s % 6 == 0 { 0.16 } else { 0.06 }));
        }
    } else {
        painter.line_segment([pos2(area.left(), centre.y), pos2(area.right(), centre.y)], stroke(1.0, faint(th.ink(), 0.07)));
        painter.line_segment([pos2(centre.x, area.top()), pos2(centre.x, area.bottom())], stroke(1.0, faint(th.ink(), 0.07)));
    }
    // (The scale's figures: under each circle seen from above; in 3D at its left end, clear of
    // the bodies' names, which stand to their right, and of the sun low in its well.)
    let (side, anchor, nudge) = if deep { (vec2(-turn.cos(), turn.sin()), Align2::RIGHT_BOTTOM, vec2(-4.0, -2.0)) } else { (vec2(0.0, -1.0), Align2::LEFT_BOTTOM, vec2(4.0, -2.0)) };
    let mut figures = Vec::new();
    for frac in [0.25f32, 0.5, 1.0] {
        if !deep {
            painter.circle_stroke(centre, radius * frac, stroke(1.0, faint(th.ink(), 0.16)));
        }
        let value = reach * if scene.root_scale { (frac * frac) as f64 } else { frac as f64 };
        figures.push((view(side * (radius * frac)).0 + nudge, format!("{} {}", label(value), scene.units.0)));
    }
    let tones = [th.accent(), th.ink(), th.deep()];
    let colour_of = |i: usize| if i == heavy { th.hot() } else { tones[i % 3] };
    let mut light = Light::new(painter);
    for (i, (trail, body)) in scene.trails.iter().zip(&scene.bodies).enumerate() {
        let seen: Vec<(Pos2, f32, f32)> = trail.iter().copied().chain([body.pos]).map(|p| view(flat(p))).collect();
        let n = seen.len();
        if n >= 2 {
            let points: Vec<Pos2> = seen.iter().map(|s| s.0).collect();
            let colours: Vec<Color32> = seen.iter().enumerate().map(|(k, s)| lit(colour_of(i), (0.12 + 0.8 * (k as f32 / n as f32)) * (0.55 + 0.45 * s.2))).collect();
            light.ribbon(&points, &colours, false, 1.3);
        }
        let (at, scale, _) = view(flat(body.pos));
        light.spark(at, body.size * 1.6 * scale, colour_of(i));
    }
    light.paint(painter);
    // The bodies, the farthest first; then the names, the heaviest's first (at a free corner of
    // its body), the scale's last.
    let mut seen: Vec<(usize, (Pos2, f32, f32))> = scene.bodies.iter().enumerate().map(|(i, b)| (i, view(flat(b.pos)))).collect();
    seen.sort_by(|a, b| a.1 .2.total_cmp(&b.1 .2));
    for &(i, (at, scale, _)) in &seen {
        let size = scene.bodies[i].size * scale;
        painter.circle_filled(at, size * 0.4, th.hot());
        painter.circle_stroke(at, size * 0.75, stroke(1.2, colour_of(i)));
    }
    seen.sort_by(|a, b| scene.bodies[b.0].mass.total_cmp(&scene.bodies[a.0].mass));
    let mut taken = Vec::new();
    for (i, (at, scale, _)) in seen {
        let (x, y) = (scene.bodies[i].size * scale * 0.8 + 3.0, scene.bodies[i].size * scale * 0.8);
        let spots = [(at + vec2(x, -y), Align2::LEFT_BOTTOM), (at + vec2(-x, -y), Align2::RIGHT_BOTTOM), (at + vec2(x, y), Align2::LEFT_TOP), (at + vec2(-x, y), Align2::RIGHT_TOP)];
        tag_free(painter, &mut taken, &spots, &scene.bodies[i].name, th.ink());
    }
    for (at, text) in figures {
        tag_free(painter, &mut taken, &[(at, anchor)], &text, th.muted());
    }
    let drift = (scene.energy() - e0) / e0.abs().max(1e-300);
    let well = if deep { "   3D: the plane sinks into the well of their gravity" } else { "" };
    readout(painter, rect, 8.0, &format!("t = {} {}   energy drift {drift:+.1e}   {} bodies   speed ×{:.1}{well}", trim_t(scene.t), scene.units.1, scene.bodies.len(), sim.value("speed")), th.ink());
}

/// A time with the digits it needs (0.0748 years, 12.4 years).
fn trim_t(t: f64) -> String {
    format!("{:.*}", if t < 1.0 { 3 } else { 1 }, t)
}

fn draw_ripples(painter: &egui::Painter, rect: Rect, sim: &Simulation, rt: &Runtime) {
    let World::Ripples(tank) = &rt.world else { return };
    let th = crate::holo::theme();
    let area = below_readout(rect);
    readout(painter, rect, 8.0, &format!("f = {:.3}   λ = {:.1} cells   slits {}   slit width {} cells   separation {} cells   {} source{}", tank.frequency, tank.wavelength(), sim.value("slits").round() as i64, sim.value("slit_width").round() as i64, sim.value("separation").round() as i64, if tank.plane { "plane" } else { "point" }, if sim.three_d { "   3D: the water's height" } else { "" }), th.ink());
    if sim.three_d {
        return relief(painter, area, tank, rt);
    }
    let aspect = tank.w as f32 / tank.h as f32;
    let (w, h) = if area.width() / area.height() > aspect { (area.height() * aspect, area.height()) } else { (area.width(), area.width() / aspect) };
    let tank_rect = Rect::from_center_size(area.center(), vec2(w, h));
    painter.add(grid_mesh(tank_rect, tank.w - 1, tank.h - 1, |i, j| field_colour((tank.at(i, j) * 1.1).tanh() * 0.85)));
    let (cw, ch) = (tank_rect.width() / (tank.w - 1) as f32, tank_rect.height() / (tank.h - 1) as f32);
    // The wall with its slits, in ink.
    for y in 0..tank.h {
        if tank.wall[y * tank.w + tank.wall_x] {
            let cell = Rect::from_min_max(pos2(tank_rect.left() + (tank.wall_x as f32 - 0.5) * cw, tank_rect.top() + (y as f32 - 0.5) * ch), pos2(tank_rect.left() + (tank.wall_x as f32 + 1.5) * cw, tank_rect.top() + (y as f32 + 0.5) * ch));
            painter.rect_filled(cell.intersect(tank_rect), Rounding::ZERO, faint(th.ink(), 0.85));
        }
    }
    crate::holo::outline(painter, &crate::holo::plate(tank_rect, [0.0; 4]), faint(th.ink(), 0.4));
    // The source.
    let source = pos2(tank_rect.left() + 16.0 * cw, tank_rect.center().y);
    let mut light = Light::new(painter);
    if tank.plane {
        light.seg(pos2(source.x, tank_rect.top() + ch), pos2(source.x, tank_rect.bottom() - ch), 2.0, th.accent());
    } else {
        light.spark(source, 9.0, th.hot());
    }
    light.paint(painter);
    // One wavelength, measured against the picture.
    let lambda = tank.wavelength() as f32 * cw;
    let at = pos2(tank_rect.left() + 12.0, tank_rect.bottom() - 14.0);
    for x in [at.x, at.x + lambda] {
        painter.line_segment([pos2(x, at.y - 4.0), pos2(x, at.y + 4.0)], stroke(1.0, th.ink()));
    }
    painter.line_segment([at, at + vec2(lambda, 0.0)], stroke(1.0, th.ink()));
    tag(painter, at + vec2(lambda + 6.0, 0.0), Align2::LEFT_CENTER, &format!("λ = {:.0} cells", tank.wavelength()), th.ink());
}

/// The ripple tank in 3D: the water's height as a wireframe seen from 38° above, the wall with
/// its slits standing in it, the source a line or a spark of light.
fn relief(painter: &egui::Painter, area: Rect, tank: &phys::Ripples, rt: &Runtime) {
    let th = crate::holo::theme();
    let ((turn, rise), swell) = (rt.aim((-28f32).to_radians(), 38f32.to_radians()), 9.0f32);
    let (zoom, pan) = rt.lens();
    let half = vec2(tank.w as f32, tank.h as f32) / 2.0;
    // (Fitted at its own angle, so turning it by hand does not breathe its size.)
    let (t0, r0) = rt.cam.get().own;
    let wide = half.x * t0.cos().abs() + half.y * t0.sin().abs();
    let tall = (half.x * t0.sin().abs() + half.y * t0.cos().abs()) * r0.sin() + swell * r0.cos();
    let fit = ((area.width() - 30.0) / (2.0 * wide)).min((area.height() - 30.0) / (2.0 * tall)) * zoom;
    let area = area.translate(pan);
    // (screen point, how near: 0-1) of a cell at a height, both in cells.
    let project = |i: f32, j: f32, h: f32| {
        let (x, y) = (i - half.x, half.y - j);
        let (x1, y1) = (x * turn.cos() - y * turn.sin(), x * turn.sin() + y * turn.cos());
        let (up, near) = (y1 * rise.sin() + h * rise.cos(), -y1 * rise.cos() + h * rise.sin());
        (area.center() + vec2(x1, -up) * fit, (near / half.y * 0.5 + 0.5).clamp(0.0, 1.0))
    };
    let height = |i: usize, j: usize| (tank.at(i, j) * 1.1).tanh() * 0.85 * swell;
    let mut light = Light::new(painter);
    for along_x in [true, false] {
        let (lines, run) = if along_x { (tank.h, tank.w) } else { (tank.w, tank.h) };
        for a in (0..lines).step_by(4) {
            let (mut points, mut colours) = (Vec::new(), Vec::new());
            for b in (0..run).step_by(2) {
                let (i, j) = if along_x { (b, a) } else { (a, b) };
                let h = height(i, j);
                let (at, near) = project(i as f32, j as f32, h);
                points.push(at);
                colours.push(lit(mix(th.ink(), th.accent(), (h / swell * 0.5 + 0.5).clamp(0.0, 1.0)), 0.08 + 0.45 * near));
            }
            light.ribbon(&points, &colours, false, 1.0);
        }
    }
    if tank.plane {
        light.seg(project(16.0, 1.0, 0.0).0, project(16.0, tank.h as f32 - 2.0, 0.0).0, 2.0, th.accent());
    } else {
        light.spark(project(16.0, half.y, 0.0).0, 9.0, th.hot());
    }
    light.paint(painter);
    let x = tank.wall_x as f32;
    for j in (0..tank.h).filter(|j| tank.wall[j * tank.w + tank.wall_x]) {
        crate::holo::rule(painter, project(x, j as f32, -0.4 * swell).0, project(x, j as f32, 1.2 * swell).0, faint(th.ink(), 0.5));
    }
}

fn draw_standing(painter: &egui::Painter, rect: Rect, sim: &Simulation, rt: &Runtime) {
    let th = crate::holo::theme();
    let (n, length, speed, amp, slow) = (sim.value("harmonic").round().max(1.0) as u32, sim.value("length").max(0.01), sim.value("speed"), sim.value("amplitude"), sim.value("slowmo").max(1.0));
    let t = rt.t / slow;
    let area = below_readout(rect);
    let (left, right, mid) = (area.left() + 24.0, area.right() - 24.0, area.center().y);
    let reach = area.height() as f64 * 0.38;
    let on = |x: f64, y: f64| pos2(left + (right - left) * (x / length) as f32, mid - (y * reach) as f32);
    let curve = |f: &dyn Fn(f64) -> f64| -> Vec<Pos2> { (0..=240).map(|i| {
        let x = length * i as f64 / 240.0;
        on(x, f(x))
    }).collect() };
    crate::holo::rule(painter, pos2(left, mid), pos2(right, mid), faint(th.ink(), 0.3));
    // The envelope the string swings inside.
    for sign in [1.0, -1.0] {
        painter.extend(Shape::dashed_line(&curve(&|x| sign * amp * (n as f64 * std::f64::consts::PI * x / length).sin()), stroke(1.0, faint(th.ink(), 0.35)), 5.0, 4.0));
    }
    // The two waves that make it: one running right, one left.
    let halves = |right_going: bool| curve(&|x| {
        let (r, l) = phys::travelling(n, length, speed, amp, x, t);
        if right_going { r } else { l }
    });
    let mut light = Light::new(painter);
    light.strip(&halves(true), false, 1.0, lit(th.deep(), 0.55));
    light.strip(&halves(false), false, 1.0, lit(th.ink(), 0.4));
    light.neon(&curve(&|x| phys::standing(n, length, speed, amp, x, t)), false, 2.4, th.accent(), 1.0);
    light.paint(painter);
    // Nodes (rings: the string never moves there), antinodes (ticks at the envelope).
    for x in phys::nodes(n, length) {
        painter.circle_stroke(on(x, 0.0), 5.0, stroke(1.2, th.ink()));
    }
    for k in 0..n {
        let x = (k as f64 + 0.5) * length / n as f64;
        painter.line_segment([on(x, amp) - vec2(0.0, 10.0), on(x, amp) - vec2(0.0, 4.0)], stroke(1.0, th.ink()));
    }
    for x in [left, right] {
        painter.line_segment([pos2(x, mid - 14.0), pos2(x, mid + 14.0)], stroke(3.0, th.ink()));
    }
    legend(painter, pos2(left, area.bottom() - 8.0), &[(th.accent(), "the string"), (th.deep(), "wave running right"), (th.ink(), "wave running left"), (faint(th.ink(), 0.5), "rings: nodes   ticks: antinodes")]);
    readout(painter, rect, 8.0, &format!("harmonic {n}   f = {:.1} Hz   λ = {:.2} m   {} nodes   shown {:.0}× slower", phys::harmonic_frequency(n, length, speed), 2.0 * length / n as f64, n + 1, slow), th.ink());
}

fn draw_doppler(painter: &egui::Painter, rect: Rect, sim: &Simulation, rt: &Runtime) {
    let th = crate::holo::theme();
    let (f0, vs, c) = (sim.value("frequency"), sim.value("speed"), sim.value("wave_speed").max(1.0));
    let mach = vs / c;
    let area = below_readout(rect);
    let half = 6.0f64;
    let scale = area.width() / (2.0 * half) as f32;
    let on = |x: f64, y: f64| pos2(area.center().x + x as f32 * scale, area.center().y - y as f32 * scale);
    // Distances in units of how far a wave goes in a second; the source starts at the left and
    // goes round again once it has crossed the view.
    let cycle = (9.0 / mach.max(0.6)).min(14.0);
    let t = rt.t % cycle;
    let start = -4.8;
    let fronts = phys::wave_fronts(start, mach, 1.0, 1.2, t, 48);
    let source = start + mach * t;
    let clipped = painter.with_clip_rect(area.intersect(painter.clip_rect()));
    crate::holo::rule(&clipped, pos2(area.left(), area.center().y), pos2(area.right(), area.center().y), faint(th.ink(), 0.25));
    let mut light = Light::new(&clipped);
    for (cx, r) in &fronts {
        if *r > 0.02 {
            let points = circle_points(on(*cx, 0.0), *r as f32 * scale, 96);
            // (Ink, not a fading accent: a dim accent reads olive.)
            light.strip(&points, true, 1.2, lit(th.ink(), (1.0 - *r / 9.0).clamp(0.1, 1.0) as f32 * 0.7));
        }
    }
    // The Mach cone: the lines every front touches once the source outruns them.
    if let (Some(mu), Some(&(_, oldest))) = (phys::mach_angle(vs, c), fronts.first()) {
        let tip = on(source, 0.0);
        let along = oldest * (mach * mach - 1.0) / mach;
        let across = oldest * (mach * mach - 1.0).sqrt() / mach;
        for sign in [1.0, -1.0] {
            light.neon(&[tip, on(source - along, sign * across)], false, 2.0, th.accent(), 1.0);
        }
        tag(&clipped, tip + vec2(16.0, -12.0), Align2::LEFT_BOTTOM, &format!("Mach cone  θ = {:.1}°", mu.to_degrees()), th.ink());
    }
    light.spark(on(source, 0.0), 12.0, th.hot());
    light.paint(&clipped);
    painter.circle_stroke(on(source, 0.0), 6.0, stroke(1.5, th.accent()));
    let (ahead, behind) = phys::doppler(f0, vs, c);
    tag(painter, pos2(area.left() + 8.0, area.center().y - 28.0), Align2::LEFT_BOTTOM, &format!("behind\n{behind:.0} Hz"), th.ink());
    tag(painter, pos2(area.right() - 8.0, area.center().y - 28.0), Align2::RIGHT_BOTTOM, &ahead.map_or("ahead\nno tone".to_string(), |a| format!("ahead\n{a:.0} Hz")), th.ink());
    readout(painter, rect, 8.0, &format!("source {f0:.0} Hz   speed {vs:.0} m/s = Mach {mach:.2}   waves {c:.0} m/s"), th.ink());
}

fn draw_sound(painter: &egui::Painter, rect: Rect, sim: &Simulation, rt: &Runtime) {
    let th = crate::holo::theme();
    let (f, c, amp, slow) = (sim.value("frequency").max(1.0), sim.value("speed").max(1.0), sim.value("amplitude"), sim.value("slowmo").max(1.0));
    let (lambda, t) = (c / f, rt.t / slow);
    let length = 4.0 * lambda;
    let s0 = amp * lambda * 0.11;
    let area = below_readout(rect);
    let columns = Rect::from_min_max(area.min, pos2(area.max.x, area.top() + area.height() * 0.5));
    let curve = Rect::from_min_max(pos2(area.left(), columns.bottom() + 16.0), area.max);
    let across = |r: Rect, x: f64| r.left() + 8.0 + (r.width() - 16.0) * (x / length) as f32;
    // Air in rows of particles: where they bunch up the air is pressed together.
    let (lanes, count, tagged) = (5usize, 72usize, 24usize);
    for lane in 0..lanes {
        let y = columns.top() + columns.height() * (lane as f32 + 0.5) / lanes as f32;
        for j in 0..count {
            let rest = length * (j as f64 + 0.5) / count as f64;
            let (s, _) = phys::sound(f, c, s0, rest, t);
            let at = pos2(across(columns, rest + s), y);
            if j == tagged {
                painter.line_segment([pos2(across(columns, rest), y - 7.0), pos2(across(columns, rest), y + 7.0)], stroke(1.0, faint(th.ink(), 0.5)));
                painter.circle_filled(at, 3.2, th.accent());
            } else {
                painter.circle_filled(at, 1.8, faint(th.ink(), 0.7));
            }
        }
    }
    // Pressure (the measured curve) and the displacement it is a quarter wave out of step with.
    let mid = curve.center().y;
    let reach = curve.height() * 0.4;
    crate::holo::rule(painter, pos2(curve.left(), mid), pos2(curve.right(), mid), faint(th.ink(), 0.3));
    let k = std::f64::consts::TAU * f / c;
    let line = |pressure: bool| -> Vec<Pos2> { (0..=240).map(|i| {
        let x = length * i as f64 / 240.0;
        let (s, p) = phys::sound(f, c, s0, x, t);
        let v = if pressure { p / (s0 * k) } else { s / s0 };
        pos2(across(curve, x), mid - v as f32 * reach)
    }).collect() };
    let mut light = Light::new(painter);
    light.strip(&line(false), false, 1.0, lit(th.ink(), 0.45));
    light.neon(&line(true), false, 2.4, th.accent(), 1.0);
    light.paint(painter);
    legend(painter, pos2(curve.left() + 8.0, curve.top() - 4.0), &[(th.accent(), "pressure"), (th.ink(), "displacement")]);
    // One wavelength, measured.
    let (a, b) = (across(curve, 0.0), across(curve, lambda));
    let y = curve.bottom() - 6.0;
    painter.line_segment([pos2(a, y), pos2(b, y)], stroke(1.0, th.ink()));
    for x in [a, b] {
        painter.line_segment([pos2(x, y - 4.0), pos2(x, y + 4.0)], stroke(1.0, th.ink()));
    }
    tag(painter, pos2(b + 6.0, y), Align2::LEFT_CENTER, &format!("λ = {lambda:.2} m"), th.ink());
    readout(painter, rect, 8.0, &format!("{f:.0} Hz   v = {c:.0} m/s   λ = {lambda:.3} m   shown {slow:.0}× slower"), th.ink());
}

fn draw_light(painter: &egui::Painter, rect: Rect, sim: &Simulation) {
    let th = crate::holo::theme();
    let (l, d, a, s) = (sim.value("wavelength"), sim.value("separation"), sim.value("slit_width"), sim.value("screen"));
    // The screen shows two dark places of the single-slit envelope on either side.
    let span = 2.5 * phys::fringe_spacing(l, a, s);
    let area = below_readout(rect);
    let plot = Rect::from_min_max(area.min + vec2(28.0, 8.0), pos2(area.max.x - 8.0, area.top() + area.height() * 0.56));
    let screen = Rect::from_min_max(pos2(plot.left(), plot.bottom() + 14.0), pos2(plot.right(), plot.bottom() + 14.0 + area.height() * 0.24));
    let n = 480usize;
    let y_of = |i: usize| -span + 2.0 * span * i as f64 / n as f64;
    let intensity: Vec<f32> = (0..=n).map(|i| phys::double_slit(l, d, a, s, y_of(i)) as f32).collect();
    painter.add(grid_mesh(screen, n, 1, |i, _| ramp(intensity[i].powf(0.6))));
    crate::holo::outline(painter, &crate::holo::plate(screen, [0.0; 4]), faint(th.ink(), 0.4));
    let x_at = |y: f64| plot.left() + plot.width() * ((y + span) / (2.0 * span)) as f32;
    let up = |v: f32| plot.bottom() - v * plot.height() * 0.9;
    // The envelope (the slit's own diffraction) and the curve it holds.
    let envelope: Vec<Pos2> = (0..=n).map(|i| pos2(plot.left() + plot.width() * i as f32 / n as f32, up(phys::double_slit(l, 0.0, a, s, y_of(i)) as f32))).collect();
    painter.extend(Shape::dashed_line(&envelope, stroke(1.0, faint(th.ink(), 0.45)), 5.0, 4.0));
    let curve: Vec<Pos2> = intensity.iter().enumerate().map(|(i, v)| pos2(plot.left() + plot.width() * i as f32 / n as f32, up(*v))).collect();
    let mut light = Light::new(painter);
    light.neon(&curve, false, 2.0, th.accent(), 1.0);
    light.paint(painter);
    crate::holo::rule(painter, plot.left_bottom(), plot.right_bottom(), faint(th.ink(), 0.4));
    tag(painter, pos2(plot.left() - 4.0, plot.top() + 0.1 * plot.height()), Align2::RIGHT_CENTER, "1", th.muted());
    tag(painter, pos2(plot.left() - 4.0, plot.bottom()), Align2::RIGHT_CENTER, "0", th.muted());
    // Positions on the screen in millimetres.
    let span_mm = span * 1000.0;
    let raw = 2.0 * span_mm / 8.0;
    let mag = 10f64.powf(raw.log10().floor());
    let step = [1.0, 2.0, 5.0, 10.0].iter().map(|m| m * mag).find(|v| *v >= raw).unwrap_or(raw);
    let mut mm = (-span_mm / step).ceil() * step;
    while mm <= span_mm {
        let x = x_at(mm / 1000.0);
        painter.line_segment([pos2(x, screen.bottom()), pos2(x, screen.bottom() + 4.0)], stroke(1.0, th.muted()));
        tag(painter, pos2(x, screen.bottom() + 6.0), Align2::CENTER_TOP, &label(mm), th.muted());
        mm += step;
    }
    tag(painter, pos2(screen.right(), screen.bottom() + 20.0), Align2::RIGHT_TOP, "position on the screen (mm)", th.muted());
    // The fringe spacing, measured against the pattern.
    let fringe = phys::fringe_spacing(l, d, s);
    let (x0, x1) = (x_at(0.0), x_at(fringe));
    let y = plot.top() + 3.0;
    painter.line_segment([pos2(x0, y), pos2(x1, y)], stroke(1.0, th.ink()));
    for x in [x0, x1] {
        painter.line_segment([pos2(x, y - 3.0), pos2(x, y + 3.0)], stroke(1.0, th.ink()));
    }
    tag(painter, pos2(x1 + 6.0, y), Align2::LEFT_CENTER, &format!("{:.2} mm", fringe * 1000.0), th.ink());
    readout(painter, rect, 8.0, &format!("{} light   λ = {l:.0} nm   d = {d:.0} µm   a = {a:.0} µm   L = {s:.1} m   fringes {:.2} mm apart", phys::colour_name(l), fringe * 1000.0), th.ink());
}

fn draw_spring(painter: &egui::Painter, rect: Rect, rt: &Runtime) {
    let World::Spring { spring, trace, reach } = &rt.world else { return };
    let th = crate::holo::theme();
    let area = below_readout(rect);
    let scene = Rect::from_min_max(area.min, pos2(area.max.x, area.top() + area.height() * 0.42));
    let chart = Rect::from_min_max(pos2(area.left() + 8.0, scene.bottom() + 24.0), pos2(area.right() - 8.0, area.max.y - 6.0));
    let wall = scene.left() + 22.0;
    let (track, equilibrium) = (scene.center().y, scene.left() + scene.width() * 0.55);
    let per_metre = scene.width() * 0.3 / reach.max(0.8) as f32;
    let block = Rect::from_center_size(pos2(equilibrium + spring.x as f32 * per_metre, track), vec2(46.0, 46.0));
    // The wall, hatched.
    painter.line_segment([pos2(wall, track - 40.0), pos2(wall, track + 40.0)], stroke(2.0, th.ink()));
    for k in 0..8 {
        let y = track - 36.0 + k as f32 * 10.0;
        painter.line_segment([pos2(wall, y + 8.0), pos2(wall - 8.0, y)], stroke(1.0, faint(th.ink(), 0.5)));
    }
    // The coil: straight ends and 14 turns between.
    let (from, to) = (wall, block.left());
    let mut coil = vec![pos2(from, track)];
    for k in 0..28 {
        let x = from + 12.0 + (to - from - 24.0).max(0.0) * (k as f32 + 0.5) / 28.0;
        coil.push(pos2(x, track + if k % 2 == 0 { -11.0 } else { 11.0 }));
    }
    coil.push(pos2(to, track));
    let mut light = Light::new(painter);
    light.strip(&coil, false, 1.4, lit(th.ink(), 0.7));
    light.paint(painter);
    painter.extend(Shape::dashed_line(&[pos2(equilibrium, track - 40.0), pos2(equilibrium, track + 40.0)], stroke(1.0, faint(th.ink(), 0.4)), 4.0, 3.0));
    tag(painter, pos2(equilibrium, track + 44.0), Align2::CENTER_TOP, "x = 0", th.muted());
    painter.rect_filled(block, Rounding::ZERO, faint(th.ink(), 0.09));
    crate::holo::outline(painter, &crate::holo::plate(block, [6.0, 0.0, 6.0, 0.0]), th.accent());
    if spring.force > 0.0 {
        let push = (spring.force * (spring.drive * spring.t).cos()) as f32 * 5.0;
        painter.arrow(pos2(block.right() + 6.0, track), vec2(push, 0.0), stroke(1.5, th.ink()));
        tag(painter, pos2(block.right() + 10.0, track - 16.0), Align2::LEFT_BOTTOM, "F", th.muted());
    }
    // x(t) over the last ten seconds.
    let mid = chart.center().y;
    crate::holo::rule(painter, pos2(chart.left(), mid), pos2(chart.right(), mid), faint(th.ink(), 0.3));
    let points: Vec<Pos2> = trace.iter().map(|(s, x)| pos2(chart.right() - ((spring.t - s) / 10.0) as f32 * chart.width(), mid - (x / reach.max(0.3)) as f32 * chart.height() * 0.45)).collect();
    if points.len() >= 2 {
        let mut light = Light::new(&painter.with_clip_rect(chart.expand(2.0)));
        light.neon(&points, false, 2.0, th.accent(), 1.0);
        light.paint(&painter.with_clip_rect(chart.expand(2.0)));
    }
    tag(painter, chart.left_top() + vec2(0.0, -12.0), Align2::LEFT_BOTTOM, "x(t), the last 10 s", th.muted());
    readout(painter, rect, 8.0, &format!("x = {:+.3} m   v = {:+.3} m/s   energy {:.3} J   t = {:.1} s", spring.x, spring.v, spring.energy(), spring.t), th.ink());
}

fn draw_chaos(painter: &egui::Painter, rect: Rect, rt: &Runtime) {
    let World::Chaos { pendulum, e0, lower, upper } = &rt.world else { return };
    let th = crate::holo::theme();
    let area = below_readout(rect);
    let centre = area.center();
    let scale = (area.width().min(area.height()) / 2.0 * 0.92) / (pendulum.l1 + pendulum.l2) as f32;
    let on = |p: [f64; 2]| centre + vec2(p[0] as f32, p[1] as f32) * scale;
    let (b1, b2) = pendulum.bobs();
    let (p1, p2) = (on(b1), on(b2));
    let mut light = Light::new(painter);
    for (trail, last, tone, gain) in [(upper, b1, th.deep(), 0.55), (lower, b2, th.accent(), 0.9)] {
        let points: Vec<Pos2> = trail.iter().copied().chain([last]).map(&on).collect();
        let n = points.len();
        if n >= 2 {
            let colours: Vec<Color32> = (0..n).map(|k| lit(tone, gain * (k as f32 / n as f32).powi(2))).collect();
            light.ribbon(&points, &colours, false, 1.6);
        }
    }
    light.paint(painter);
    painter.line_segment([centre, p1], stroke(1.5, th.ink()));
    painter.line_segment([p1, p2], stroke(1.5, th.ink()));
    painter.circle_stroke(centre, 4.0, stroke(1.2, th.ink()));
    for (at, mass) in [(p1, pendulum.m1), (p2, pendulum.m2)] {
        let r = 6.0 + 4.0 * mass.cbrt() as f32;
        painter.circle_filled(at, r, faint(th.ink(), 0.09));
        painter.circle_stroke(at, r, stroke(1.8, th.accent()));
        painter.circle_filled(at, 2.0, th.hot());
    }
    let drift = (pendulum.energy() - e0) / e0.abs().max(1e-12);
    readout(painter, rect, 8.0, &format!("θ₁ = {:+.1}°   θ₂ = {:+.1}°   energy drift {drift:+.1e}   t = {:.1} s", pendulum.a1.to_degrees(), pendulum.a2.to_degrees(), rt.t), th.ink());
}

fn draw_balls(painter: &egui::Painter, rect: Rect, rt: &Runtime) {
    let World::Balls { balls, e0 } = &rt.world else { return };
    let th = crate::holo::theme();
    let area = below_readout(rect);
    let aspect = (balls.width / balls.height) as f32;
    let (w, h) = if area.width() / area.height() > aspect { (area.height() * aspect, area.height()) } else { (area.width(), area.width() / aspect) };
    let frame = Rect::from_center_size(area.center(), vec2(w, h));
    let scale = frame.width() / balls.width as f32;
    crate::holo::outline(painter, &crate::holo::plate(frame, [0.0; 4]), faint(th.ink(), 0.6));
    let mut fastest = 0.0f64;
    for ball in &balls.balls {
        let speed = ball.vel[0].hypot(ball.vel[1]);
        fastest = fastest.max(speed);
        // Hotter with speed: the accent warms to white-hot.
        let edge = mix(th.accent(), th.hot(), (speed / 12.0).min(1.0) as f32);
        let at = pos2(frame.left() + ball.pos[0] as f32 * scale, frame.bottom() - ball.pos[1] as f32 * scale);
        painter.circle_filled(at, ball.r as f32 * scale, faint(th.ink(), 0.09));
        painter.circle_stroke(at, ball.r as f32 * scale, stroke(1.6, edge));
    }
    let drift = (balls.energy() - e0) / e0.abs().max(1e-12) * 100.0;
    readout(painter, rect, 8.0, &format!("{} balls   energy {:.1} J ({drift:+.1} %)   fastest {fastest:.1} m/s   {:.0} % of the speed kept per bounce", balls.balls.len(), balls.energy(), balls.elasticity * 100.0), th.ink());
}

fn draw_surface(painter: &egui::Painter, rect: Rect, sim: &Simulation, rt: &Runtime) {
    let th = crate::holo::theme();
    let r = sim.value("range").max(0.5);
    // (In 2D it is seen from straight above, its height in its colour and brightness.)
    let (turn, tilt) = if sim.three_d {
        let (turn, tilt) = rt.aim((sim.value("turn") + sim.value("spin") * rt.t).to_radians() as f32, sim.value("tilt").to_radians() as f32);
        (turn as f64, tilt as f64)
    } else {
        (0.0, std::f64::consts::FRAC_PI_2)
    };
    let (zoom, pan) = if sim.three_d { rt.lens() } else { (1.0, egui::Vec2::ZERO) };
    let n = 34usize;
    let mut ps = sim.surface_params();
    let step = |k: usize| -r + 2.0 * r * k as f64 / n as f64;
    let z: Vec<Vec<f64>> = (0..=n).map(|j| (0..=n).map(|i| sim.surface_z(&mut ps, step(i), step(j))).collect()).collect();
    // The height range from the 2nd-98th percentile, so a pole does not flatten the rest.
    let mut sorted: Vec<f64> = z.iter().flatten().copied().filter(|v| v.is_finite()).collect();
    sorted.sort_by(|a, b| a.total_cmp(b));
    let (mut lo, mut hi) = if sorted.is_empty() { (-1.0, 1.0) } else { (sorted[sorted.len() * 2 / 100], sorted[(sorted.len() * 98 / 100).min(sorted.len() - 1)]) };
    if hi - lo < 1e-9 {
        lo -= 1.0;
        hi += 1.0;
    }
    let tall = r * 0.9;
    let high = |v: f64| ((v - (lo + hi) / 2.0) / (hi - lo) * 2.0 * tall).clamp(-1.4 * tall, 1.4 * tall);
    let area = below_readout(rect);
    let pixels = area.width().min(area.height()) * 0.5 * 0.8 / (r as f32 * 1.4) * zoom;
    let centre = area.center() + vec2(0.0, 8.0) + pan;
    // (screen point, how near to the viewer, 0-1) of a point of the surface.
    let project = |x: f64, y: f64, h: f64| {
        let (x1, y1) = (x * turn.cos() - y * turn.sin(), x * turn.sin() + y * turn.cos());
        let up = y1 * tilt.sin() + h * tilt.cos();
        let near = -y1 * tilt.cos() + h * tilt.sin();
        (centre + vec2(x1 as f32, -up as f32) * pixels, ((near / (r * 1.6)) * 0.5 + 0.5).clamp(0.0, 1.0) as f32)
    };
    // The floor and the axes they stand on.
    let floor = -tall;
    let corner = |sx: f64, sy: f64| project(sx * r, sy * r, floor).0;
    for (a, b) in [((-1.0, -1.0), (1.0, -1.0)), ((1.0, -1.0), (1.0, 1.0)), ((1.0, 1.0), (-1.0, 1.0)), ((-1.0, 1.0), (-1.0, -1.0))] {
        crate::holo::rule(painter, corner(a.0, a.1), corner(b.0, b.1), faint(th.ink(), 0.3));
    }
    tag(painter, corner(1.0, -1.0) + vec2(6.0, 0.0), Align2::LEFT_CENTER, "x", th.ink());
    if sim.three_d {
        tag(painter, corner(-1.0, 1.0) + vec2(6.0, 0.0), Align2::LEFT_CENTER, "y", th.ink());
        crate::holo::rule(painter, corner(-1.0, -1.0), project(-r, -r, tall).0, faint(th.ink(), 0.6));
        tag(painter, project(-r, -r, tall).0 - vec2(0.0, 6.0), Align2::CENTER_BOTTOM, "z", th.ink());
    } else {
        tag(painter, corner(-1.0, 1.0) - vec2(6.0, 0.0), Align2::RIGHT_CENTER, "y", th.ink());
    }
    // The wireframe: lines of constant y, then of constant x. Peaks take the accent, the near
    // lines are brighter; a point that is not a number cuts the line.
    let mut light = Light::new(painter);
    let mut flush = |points: &mut Vec<Pos2>, colours: &mut Vec<Color32>| {
        if points.len() >= 2 {
            light.ribbon(points, colours, false, 1.1);
        }
        points.clear();
        colours.clear();
    };
    for along_x in [true, false] {
        for j in 0..=n {
            let (mut points, mut colours) = (Vec::new(), Vec::new());
            for i in 0..=n {
                let (xi, yi) = if along_x { (i, j) } else { (j, i) };
                let v = z[yi][xi];
                if !v.is_finite() {
                    flush(&mut points, &mut colours);
                    continue;
                }
                let (at, near) = project(step(xi), step(yi), high(v));
                let height = ((v - lo) / (hi - lo)).clamp(0.0, 1.0) as f32;
                points.push(at);
                colours.push(lit(mix(th.ink(), th.accent(), height), 0.1 + 0.7 * near));
            }
            flush(&mut points, &mut colours);
        }
    }
    light.paint(painter);
    let src = sim.functions.first().map(|(_, s, _)| s.as_str()).unwrap_or("");
    let seen = if sim.three_d { format!("turn {:.0}°   tilt {:.0}°", turn.to_degrees().rem_euclid(360.0), tilt.to_degrees()) } else { "2D: seen from above".to_string() };
    readout(painter, rect, 8.0, &format!("z = {src}   {seen}   z from {lo:.2} to {hi:.2}"), th.ink());
}

/// A simulation's read-out in the top-left corner of its canvas, `top` points under its edge.
/// Its figures (three spaces apart) share a line while the canvas is wide enough for them and
/// go on to the next line when it is not; a single figure wider than the canvas wraps. Nothing
/// is cut by a narrow block. The height it took.
fn readout(painter: &egui::Painter, rect: Rect, top: f32, text: &str, colour: Color32) -> f32 {
    let font = crate::holo::font(Face::Mono, 11.5);
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

    /// Sir's screenshot of 2026-11-22: the input line's ADD key ran under the plot. The editor
    /// keeps to its column, in a wide window and in a narrow one.
    #[test]
    fn the_plot_editor_keeps_to_its_column() {
        let lab = simulation_lab::SimulationLab::default();
        let sim = lab.launch(SimKind::Function, "a*x^2 + b*x + c", None).unwrap();
        let p = Palette::new(&crate::interface::InterfaceSettings::default());
        let _look = crate::holo_theme::look_for_pictures();
        for width in [900.0, 560.0] {
            let size = vec2(width, 560.0);
            let (column, _) = split_layout(Rect::from_min_size(pos2(12.0, 40.0), size - vec2(24.0, 52.0)));
            let mut stage = crate::visual_ui_verifier::Stage::new(size);
            let frame = stage.frames(2, &mut |ctx| {
                crate::holo::install_fonts(ctx);
                egui::CentralPanel::default().frame(Frame::none()).show(ctx, |ui| {
                    ui.allocate_new_ui(egui::UiBuilder::new().max_rect(column), |ui| {
                        ui.set_width(column.width());
                        plot_editor(ui, &p, &sim, &lab);
                    });
                });
            });
            let add = frame.find_exact("ADD");
            assert!(!add.is_empty() && add.iter().all(|r| r.right() <= column.right() + 1.0), "{width}: ADD at {add:?}, the column ends at {}", column.right());
            let hint = frame.find_text("sin x");
            assert!(hint.iter().all(|r| r.right() <= column.right() + 1.0), "{width}: the input line at {hint:?}");
        }
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
        let _theme = crate::holo_theme::test_lock();
        crate::holo_theme::finish_fade();
        let (f_colour, df_colour) = (first_series(), second_series());
        let size = vec2(900.0, 520.0);
        let plot = Rect::from_min_size(pos2(20.0, 20.0), size - vec2(40.0, 40.0));
        let mut snap = crate::ui_snapshot::Snapshot::new(size);
        let canvas = snap.render(2, |ctx| {
            crate::holo::install_fonts(ctx);
            egui::CentralPanel::default().frame(Frame::none()).show(ctx, |ui| {
                let painter = ui.painter_at(plot.expand(2.0));
                painter.rect_filled(plot, Rounding::ZERO, crate::holo::theme().void());
                crate::holo::outline(&painter, &crate::holo::plate(plot, [0.0; 4]), plot_frame());
                crate::hud_board::brackets(&painter, plot, crate::holo::theme().ink());
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

    /// A kind's canvas as the window draws it after `seconds` of its run, saved as a picture
    /// (`target/ui-snapshots/<theme>/<name>.png`).
    fn picture(kind: SimKind, spec: &str, name: &str, seconds: f64) -> crate::ui_snapshot::Canvas {
        picture_of(simulation_lab::SimulationLab::default().launch(kind, spec, None).unwrap(), name, seconds)
    }

    fn picture_of(sim: Simulation, name: &str, seconds: f64) -> crate::ui_snapshot::Canvas {
        let _look = crate::holo_theme::look_for_pictures();
        let mut rt = Runtime::new(&sim);
        for _ in 0..(seconds / 0.04) as usize {
            rt.t += 0.04;
            advance(&sim, &mut rt, 0.04);
        }
        let size = vec2(900.0, 560.0);
        let plot = Rect::from_min_size(pos2(10.0, 10.0), size - vec2(20.0, 20.0));
        let mut stage = crate::visual_ui_verifier::Stage::new(size);
        let frame = stage.frames(2, &mut |ctx| {
            crate::holo::install_fonts(ctx);
            egui::CentralPanel::default().frame(Frame::none()).show(ctx, |ui| {
                let painter = ui.painter_at(plot.expand(2.0));
                painter.rect_filled(plot, Rounding::ZERO, crate::holo::theme().void());
                crate::holo::outline(&painter, &crate::holo::plate(plot, [0.0; 4]), plot_frame());
                draw_physics(&painter, plot.shrink(16.0), &sim, &rt);
            });
        });
        let canvas = stage.raster(&frame);
        canvas.save(name);
        assert_eq!(crate::visual_ui_verifier::text_over_text(&frame), Vec::<String>::new(), "{name}");
        canvas
    }

    /// Pixels brighter than `sum` (red + green + blue), and how many are not black glass.
    fn lit_pixels(canvas: &crate::ui_snapshot::Canvas, sum: u32) -> (usize, usize) {
        let (mut bright, mut drawn) = (0, 0);
        for y in 0..canvas.height {
            for x in 0..canvas.width {
                let [r, g, b] = canvas.pixel(x, y);
                let total = r as u32 + g as u32 + b as u32;
                bright += usize::from(total > sum);
                drawn += usize::from(total > 60);
            }
        }
        (bright, drawn)
    }

    /// Light on black: some of it bright, most of the glass still dark.
    fn assert_light_on_black(canvas: &crate::ui_snapshot::Canvas, what: &str, at_least: usize) {
        let (bright, drawn) = lit_pixels(canvas, 450);
        let all = canvas.width * canvas.height;
        assert!(bright > at_least, "{what}: {bright} bright pixels");
        assert!(drawn < all * 3 / 4, "{what}: {drawn} of {all} pixels are lit, the glass is flooded");
    }

    #[test]
    fn snapshot_gravity_draws_orbits_with_trails_and_names() {
        assert_light_on_black(&picture(SimKind::Gravity, "solar system", "simlab_gravity_solar", 20.0), "the solar system", 300);
        assert_light_on_black(&picture(SimKind::Gravity, "the sun, the earth and a comet", "simlab_gravity_comet", 20.0), "sun, earth and comet", 200);
    }

    /// Width and height of what is drawn below the read-out (pixels not black glass).
    fn drawn_box(canvas: &crate::ui_snapshot::Canvas) -> (usize, usize) {
        let (mut x0, mut y0, mut x1, mut y1) = (usize::MAX, usize::MAX, 0, 0);
        for y in 80..canvas.height - 30 {
            for x in 30..canvas.width - 30 {
                if canvas.pixel(x, y).iter().map(|c| *c as u32).sum::<u32>() > 60 {
                    (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x), y1.max(y));
                }
            }
        }
        (x1.saturating_sub(x0), y1.saturating_sub(y0))
    }

    /// "Make it 3D" (Sir, 2026-11-21): the orbits' plane leans back and sinks into the wells of
    /// the masses, the tank stands up as a relief of the water's height, and a surface made 2D
    /// lies flat, seen from above.
    #[test]
    fn snapshot_simulations_in_3d_and_from_above() {
        let flat = picture(SimKind::Gravity, "solar system", "simlab_gravity_2d", 20.0);
        let deep = picture(SimKind::Gravity, "solar system in 3D", "simlab_gravity_3d", 20.0);
        assert_light_on_black(&deep, "the orbits in 3D", 300);
        let ((w2, h2), (w3, h3)) = (drawn_box(&flat), drawn_box(&deep));
        assert!(h2 * 10 > w2 * 8, "seen from above the orbits are round: {w2}×{h2}");
        assert!(h3 * 10 < w3 * 6, "in 3D the plane leans back: {w3}×{h3}");
        assert_light_on_black(&picture(SimKind::Gravity, "binary star in 3D", "simlab_gravity_binary_3d", 12.0), "two wells", 200);
        let relief = picture(SimKind::Ripples, "double slit plane waves in 3D", "simlab_ripples_3d", 8.0);
        assert_light_on_black(&relief, "the tank as a relief", 600);
        let (w, h) = drawn_box(&relief);
        assert!(w > 600 && h > 200, "the relief fills the canvas: {w}×{h}");
        let lab = simulation_lab::SimulationLab::default();
        let mut sim = lab.launch(SimKind::Surface, "z = x^2 - y^2", None).unwrap();
        sim.three_d = false;
        let above = picture_of(sim, "simlab_surface_2d", 1.0);
        assert_light_on_black(&above, "the saddle from above", 400);
        let (w, h) = drawn_box(&above);
        assert!(w.abs_diff(h) * 5 < w, "a square seen from above (its x and y beside it): {w}×{h}");
    }

    #[test]
    fn snapshot_ripple_tank_shows_waves_through_the_slits() {
        let canvas = picture(SimKind::Ripples, "double slit plane waves", "simlab_ripples", 8.0);
        assert_light_on_black(&canvas, "the ripple tank", 600);
        // The wall stands in ink: a column of near-white pixels a third of the way across.
        let walls = (0..canvas.height).filter(|y| (0..canvas.width).any(|x| canvas.pixel(x, *y).iter().all(|c| *c > 200))).count();
        assert!(walls > 40, "the wall: {walls} rows");
    }

    #[test]
    fn snapshot_standing_wave_marks_its_nodes() {
        assert_light_on_black(&picture(SimKind::StandingWave, "", "simlab_standing_wave", 2.5), "the string", 150);
    }

    #[test]
    fn snapshot_doppler_draws_fronts_and_the_mach_cone() {
        assert_light_on_black(&picture(SimKind::Doppler, "", "simlab_doppler_subsonic", 4.0), "the moving source", 300);
        assert_light_on_black(&picture(SimKind::Doppler, "supersonic", "simlab_doppler_mach_cone", 4.0), "the Mach cone", 300);
    }

    #[test]
    fn snapshot_sound_draws_air_particles_and_the_pressure_curve() {
        assert_light_on_black(&picture(SimKind::Sound, "", "simlab_sound", 2.5), "the sound wave", 200);
    }

    #[test]
    fn snapshot_light_draws_the_double_slit_fringes_on_a_screen() {
        let canvas = picture(SimKind::Light, "", "simlab_light_double_slit", 0.0);
        assert_light_on_black(&canvas, "the double slit", 300);
        // Dark and bright alternate along a scanline of the screen band: count the changes.
        let flips = (0..canvas.height)
            .map(|y| {
                let lit: Vec<bool> = (30..canvas.width - 30).map(|x| canvas.pixel(x, y).iter().map(|c| *c as u32).sum::<u32>() > 150).collect();
                lit.windows(2).filter(|w| w[0] != w[1]).count()
            })
            .max()
            .unwrap_or(0);
        assert!(flips >= 8, "fringes alternate along the screen ({flips} changes)");
    }

    #[test]
    fn snapshot_spring_and_double_pendulum_leave_trails() {
        assert_light_on_black(&picture(SimKind::Spring, "resonance", "simlab_spring", 6.0), "the spring", 150);
        assert_light_on_black(&picture(SimKind::DoublePendulum, "", "simlab_double_pendulum", 6.0), "the double pendulum", 150);
    }

    #[test]
    fn snapshot_balls_in_a_box_are_faint_panes_with_bright_edges() {
        let canvas = picture(SimKind::Balls, "", "simlab_balls", 3.0);
        assert_light_on_black(&canvas, "the balls", 300);
    }

    #[test]
    fn snapshot_surface_is_a_wireframe_seen_from_an_angle() {
        assert_light_on_black(&picture(SimKind::Surface, "", "simlab_surface_ripple", 1.0), "the ripple surface", 400);
        assert_light_on_black(&picture(SimKind::Surface, "z = x^2 - y^2", "simlab_surface_saddle", 1.0), "the saddle", 400);
    }

    #[test]
    fn the_physics_kinds_run_for_a_minute_without_blowing_up() {
        for (kind, spec) in [(SimKind::Gravity, "binary star"), (SimKind::Gravity, "figure eight"), (SimKind::Ripples, ""), (SimKind::Spring, "resonance"), (SimKind::DoublePendulum, ""), (SimKind::Balls, "")] {
            let sim = simulation_lab::SimulationLab::default().launch(kind, spec, None).unwrap();
            let mut rt = Runtime::new(&sim);
            for _ in 0..1500 {
                rt.t += 0.04;
                advance(&sim, &mut rt, 0.04);
            }
            match &rt.world {
                World::Gravity { scene, e0, .. } => assert!(((scene.energy() - e0) / e0.abs()).abs() < 1e-2, "{spec}: energy {} → {}", e0, scene.energy()),
                World::Ripples(tank) => assert!(tank.now.iter().all(|v| v.is_finite() && v.abs() < 10.0)),
                World::Spring { spring, .. } => assert!(spring.x.is_finite() && spring.x.abs() < 20.0),
                World::Chaos { pendulum, e0, .. } => assert!(((pendulum.energy() - e0) / e0.abs()).abs() < 1e-3),
                World::Balls { balls, .. } => assert!(balls.balls.iter().all(|b| b.pos[0].is_finite() && b.pos[1] >= 0.0)),
                World::None => panic!("{kind:?} has no world"),
            }
        }
    }
}
