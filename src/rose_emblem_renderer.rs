//! The HUD's emblem, painted with `egui::Painter` meshes, in three states (`RoseEmblemState`):
//!
//! * `ThinkingGyroscope` — JAI is thinking, transcribing a turn into an answer, running a
//!   research agent, asleep or signed off: the **thinking stack** of Sir's reference frame
//!   (`image_1f7704.png`). Six circular ring planes stand in depth on one straight spindle axis
//!   that runs at −30° on screen from the bottom-left (front) to the top-right (back). Every ring
//!   is perpendicular to the spindle, so it projects as an ellipse whose major axis is tilted
//!   +60° with a 0.46 aspect. Front to back: a segmented ice-white ring (r 92), a cyan-white
//!   wireframe ring with three white arcs and orbital dots (r 102), the twin magenta-rose core
//!   rings in a wide pink-violet haze (r 96 / 88, a crisp cyan ring of r 82 inside them), the
//!   precision tachometer dial with 10° ticks, three red-pink tick clusters and degree numbers
//!   (r 138) and the dotted rear halo with its red-white `TR 2003 // 3500` telemetry arc (r 152).
//!   A crimson-pink laser threads all six centres and fades out at both ends; ten pinpoint sparks
//!   float around. Every vertex is projected (with a mild perspective) and depth-fogged, and all
//!   parts are drawn back to front. The axial spacing breathes out of the flat emblem on a
//!   critically damped spring, each ring spins on its own harmonic, and the core rings' bloom
//!   pulses while he thinks and speaks.
//! * `AwakeFrontEmblem` — awake with the centre of the screen free: a compact crimson-and-white
//!   dial (no cyan) around the white `J.A.I.` wordmark on a dark core — from the outside in:
//!   a fine dotted rim with four double-bar markers on the diagonals and a fixed red arc under it,
//!   a pink and white tick ring with crosshair marks at top and bottom, three white technical
//!   arcs, the blazing magenta torus, a thinner rose ring, and the inner ring with its vertical
//!   crosshair ticks. The ticks and the torus pulse with his voice. The stack folds into it (the spring pulls every ring plane back onto the centre
//!   while the front rings fade in).
//! * `CornerDocked` — only when something actually covers the 360 × 360 px centre safe zone
//!   (`hud_dock::centre_covered`: a report grid, the spotlight, the core sphere, a big tool
//!   block): the front emblem shrinks to 105 px and glides into the bottom-right corner — left
//!   of the text console when it is out, so the console never covers it — and tilts into a
//!   small thinking stack there while he thinks.
//!
//! Glow is additive (premultiplied colours with zero alpha add light without darkening), so the
//! bloom passes stack like light on the obsidian canvas. The geometry (`StackFrame`, the ring
//! points, the spindle, the state machine, the spring) is pure and unit-tested; painting only
//! turns it into meshes.

use eframe::egui::{pos2, vec2, Align2, Color32, FontFamily, FontId, Mesh, Painter, Pos2, Rect, Shape, Vec2};
use std::f32::consts::{PI, TAU};

/// The geometry below is specified for an outer ring of this radius (px).
pub const REFERENCE_RADIUS: f32 = 175.0;
pub const RING_COUNT: usize = 7;
/// Radius of each front ring (reference px): the core's edge, the magenta torus, the dotted rim,
/// the thinner rose ring, the tick ring, the white arcs and the inner crosshair ring. All
/// different, so the front state is a set of concentric dials.
pub const RING_RADII_PX: [f32; RING_COUNT] = [55.0, 112.0, 175.0, 98.0, 152.0, 130.0, 70.0];
/// Spin of each front ring (rad/s; alternating directions).
const RING_SPIN: [f32; RING_COUNT] = [0.42, -0.24, 0.09, 0.31, -0.07, 0.18, -0.36];

/// The thinking stack's spindle direction on screen: −30° (up and to the right).
pub const STACK_AXIS_DEG: f32 = -30.0;
/// Minor / major axis of every projected ring plane (the cosine of the spindle's tilt out of the
/// screen).
pub const STACK_ASPECT: f32 = 0.46;
pub const STACK_RINGS: usize = 6;
/// Each ring's position along the spindle on screen (reference px; − = front / bottom-left).
pub const STACK_AXIAL: [f32; STACK_RINGS] = [-115.0, -70.0, -28.0, -8.0, 48.0, 95.0];
/// Each ring's radius (reference px).
pub const STACK_RADII: [f32; STACK_RINGS] = [92.0, 102.0, 96.0, 88.0, 138.0, 152.0];
/// The crisp cyan ring hugging the inside of the second core ring.
pub const STACK_INNER_CYAN: f32 = 82.0;
/// Spin of each ring about the spindle (rad/s, distinct harmonics; ring 1 turns clockwise).
pub const STACK_SPIN: [f32; STACK_RINGS] = [0.6, -0.45, 0.9, -0.62, -0.25, 0.18];
/// The laser runs from just in front of ring 1 to just behind ring 6 (reference px on screen).
pub const SPINDLE_FROM: f32 = -232.0;
pub const SPINDLE_TO: f32 = 196.0;
pub const SPINDLE_SEGMENTS: usize = 36;
/// Pinpoint sparks floating around the stack.
pub const SPARK_COUNT: usize = 10;
/// Pinhole focal length of the stack's mild perspective (reference px).
pub const STACK_FOCAL: f32 = 1400.0;
/// Luminance left at the far rear of the depth fog.
pub const REAR_FOG: f32 = 0.35;
/// The spring that breathes the stack out of the flat emblem (rad/s; critically damped).
pub const SPREAD_OMEGA: f32 = 9.0;
/// Seconds the stack needs to fold flat (and to unfold again).
pub const UNFOLD_SECS: f32 = 0.45;
/// Seconds of the glide between the centre and the corner.
pub const DOCK_SECS: f32 = 0.6;
pub const DOCKED_DIAMETER: f32 = 130.0;
/// The centred front emblem's radius as a share of the screen's shorter side (compact, like the
/// reference board's emblem).
pub const CENTRE_RADIUS_SHARE: f32 = 0.17;

// Every colour comes from the active theme (`theme_engine::current()`, re-read each frame), so a
// colour change re-themes the rings, the stack and the laser as it cross-fades.
fn pal() -> crate::theme_engine::ThemePalette {
    crate::theme_engine::current()
}
/// `t` of the way from `c` to white.
fn whiten(c: [u8; 3], t: f32) -> [u8; 3] {
    mix(c, WHITE_RGB, t)
}
fn torus_deep() -> [u8; 3] {
    pal().core_primary
}
fn torus_hot() -> [u8; 3] {
    whiten(pal().core_primary, 0.38)
}
fn crimson_rgb() -> [u8; 3] {
    pal().core_primary
}
fn cyan_rgb() -> [u8; 3] {
    pal().core_secondary
}
fn ice_rgb() -> [u8; 3] {
    whiten(pal().core_secondary, 0.86)
}
const WHITE_RGB: [u8; 3] = [0xFF, 0xFF, 0xFF];
/// The stack's own palette (`image_1f7704.png`).
fn stack_ice() -> [u8; 3] {
    pal().hud_bracket_color
}
fn stack_wire() -> [u8; 3] {
    whiten(pal().core_secondary, 0.6)
}
fn core_pink() -> [u8; 3] {
    pal().core_primary
}
fn core_hot() -> [u8; 3] {
    whiten(pal().core_primary, 0.45)
}
fn laser_core() -> [u8; 3] {
    whiten(pal().core_primary, 0.25)
}
fn laser_glow() -> [u8; 3] {
    pal().core_primary
}
/// Near-white text on the emblem.
fn ice() -> Color32 {
    let c = ice_rgb();
    Color32::from_rgb(c[0], c[1], c[2])
}

/// What the emblem shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoseEmblemState {
    /// Flat, concentric, facing the camera, the `J.A.I.` wordmark in the centre.
    AwakeFrontEmblem,
    /// The six-ring thinking stack in the centre.
    ThinkingGyroscope,
    /// Small (105 px) in the bottom-right corner while something covers the centre.
    CornerDocked,
}

/// What the emblem's state depends on.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EmblemInputs {
    /// A turn is being answered (`thinking_guard::is_thinking`).
    pub thinking: bool,
    /// A research agent is running.
    pub researching: bool,
    /// The board was signed off ("screen down").
    pub signed_off: bool,
    /// Sleep mode or the zero-VRAM standby.
    pub asleep: bool,
    /// The slide-out text console is open (the docked emblem moves left of it).
    pub console_open: bool,
    /// The "show me your core" sphere is on screen (it owns the centre).
    pub core_open: bool,
    /// A board block, the spotlight or a tool block covers the centre safe zone.
    pub centre_covered: bool,
    /// Nothing has happened for 12 s (no turn, no speech, no click): the idle morph may run.
    pub idle: bool,
    /// The idle morph shows the gyroscope right now (`IdleMorph`, set by `RoseEmblem::follow`).
    pub idle_gyro: bool,
}

impl EmblemInputs {
    /// Something is crunching: the rings spin faster and a docked emblem tilts.
    pub fn busy(&self) -> bool {
        self.thinking || self.researching
    }
}

/// The state for a set of inputs: docked only when the centre of the screen is actually covered
/// (so the emblem never sits on what Sir is reading, and never leaves an empty centre), else the
/// thinking stack while it thinks, researches, sleeps or is signed off, else the front emblem.
pub fn resolve_state(inputs: EmblemInputs) -> RoseEmblemState {
    if inputs.centre_covered || inputs.core_open {
        RoseEmblemState::CornerDocked
    } else if inputs.thinking || inputs.researching || inputs.signed_off || inputs.asleep || (inputs.idle && inputs.idle_gyro) {
        RoseEmblemState::ThinkingGyroscope
    } else {
        RoseEmblemState::AwakeFrontEmblem
    }
}

/// (flat, dock) a state eases to: flat 0 = thinking stack, 1 = front emblem; dock 0 = centre,
/// 1 = corner. A docked emblem tilts into a small stack while JAI is busy.
pub fn pose_targets(state: RoseEmblemState, busy: bool) -> (f32, f32) {
    match state {
        RoseEmblemState::ThinkingGyroscope => (0.0, 0.0),
        RoseEmblemState::AwakeFrontEmblem => (1.0, 0.0),
        RoseEmblemState::CornerDocked => (if busy { 0.0 } else { 1.0 }, 1.0),
    }
}

pub fn ease_out_cubic(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    1.0 - (1.0 - t).powi(3)
}

fn ease_in_out_cubic(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    if t < 0.5 { 4.0 * t * t * t } else { 1.0 - (-2.0 * t + 2.0).powi(3) / 2.0 }
}

fn smoothstep(edge0: f32, edge1: f32, x: f32) -> f32 {
    let t = ((x - edge0) / (edge1 - edge0).max(1e-6)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// A value easing from `from` to `to` over `secs`, started at `start` (emblem clock).
#[derive(Debug, Clone, Copy)]
struct Tween {
    from: f32,
    to: f32,
    start: f32,
    secs: f32,
    in_out: bool,
}

impl Tween {
    fn fixed(v: f32) -> Self {
        Self { from: v, to: v, start: 0.0, secs: 0.0, in_out: false }
    }

    fn value(&self, now: f32) -> f32 {
        let t = (now - self.start) / self.secs.max(1e-3);
        let e = if self.in_out { ease_in_out_cubic(t) } else { ease_out_cubic(t) };
        self.from + (self.to - self.from) * e
    }

    fn retarget(&mut self, to: f32, now: f32, secs: f32, in_out: bool) {
        if (self.to - to).abs() < 1e-6 {
            return;
        }
        *self = Self { from: self.value(now), to, start: now, secs, in_out };
    }

    fn done(&self, now: f32) -> bool {
        now - self.start >= self.secs
    }
}

/// One step of a critically damped spring (`x` towards `target`, velocity `v`, stiffness
/// `omega`): no overshoot, settles in about 5 / `omega` seconds. Semi-implicit Euler in 4 ms
/// sub-steps, stable for any frame time.
pub fn spring_step(x: &mut f32, v: &mut f32, target: f32, omega: f32, dt: f32) {
    let steps = (dt / 0.004).ceil().max(1.0) as usize;
    let h = dt / steps as f32;
    for _ in 0..steps {
        let a = omega * omega * (target - *x) - 2.0 * omega * *v;
        *v += a * h;
        *x += *v * h;
    }
}

/// How much light a vertex at depth `z` keeps: full in front, down to `REAR_FOG` at the back.
pub fn depth_fog(z: f32, radius: f32) -> f32 {
    if z <= 0.0 {
        1.0
    } else {
        1.0 - (1.0 - REAR_FOG) * smoothstep(0.0, radius * 0.75, z)
    }
}

/// Where the emblem is and how big, this frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EmblemFrame {
    pub centre: Pos2,
    /// Radius of the outermost front ring.
    pub radius: f32,
    /// 0 = thinking stack, 1 = front emblem.
    pub flat: f32,
    /// 0 = centre, 1 = corner.
    pub dock: f32,
}

impl EmblemFrame {
    /// Screen px per reference px.
    fn unit(&self) -> f32 {
        self.radius / REFERENCE_RADIUS
    }
}

/// The centred emblem's radius on a screen.
pub fn centre_radius(screen: Rect) -> f32 {
    (screen.width().min(screen.height()) * CENTRE_RADIUS_SHARE).clamp(70.0, 260.0)
}

/// The undocked emblem's centre and radius: the middle of the screen, unless the text console
/// (`console_width` slid in) would cut into its rings — then it moves left just far enough (and
/// shrinks only when even that is not enough). On a wide screen the console never reaches it.
pub fn home_pose(screen: Rect, console_width: f32) -> (Pos2, f32) {
    let free_right = screen.right() - console_width.max(0.0) - 16.0;
    let free_width = (free_right - screen.left()).max(0.0);
    let radius = centre_radius(screen).min((free_width / 2.0 - 8.0).max(40.0));
    let x = screen.center().x.min(free_right - radius).max(screen.left() + radius);
    (Pos2::new(x, screen.center().y), radius)
}

/// Screen points of front ring `i` (flat and concentric) at its spin.
pub fn ring_points(frame: &EmblemFrame, i: usize, spin: f32, steps: usize) -> Vec<Pos2> {
    ring_points_at(frame, i, spin, steps, 1.0)
}

/// Front ring `i` at `fraction` of its radius (tick ends).
fn ring_points_at(frame: &EmblemFrame, i: usize, spin: f32, steps: usize, fraction: f32) -> Vec<Pos2> {
    let r = RING_RADII_PX[i] * frame.unit() * fraction;
    (0..steps)
        .map(|k| {
            let a = spin + TAU * k as f32 / steps as f32;
            frame.centre + vec2(r * a.cos(), r * a.sin())
        })
        .collect()
}

// ------------------------------------------------------------------------- thinking stack ----

/// The thinking stack's projection for one frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StackFrame {
    pub centre: Pos2,
    /// Screen px per reference px.
    pub unit: f32,
    /// 0 = every ring plane flat on the centre (the front emblem), 1 = the full stack.
    pub spread: f32,
}

impl StackFrame {
    /// The spindle's direction on screen (unit vector, −30°: up and to the right).
    pub fn axis_dir() -> Vec2 {
        let a = STACK_AXIS_DEG.to_radians();
        vec2(a.cos(), a.sin())
    }

    /// (cos, sin) of the spindle's tilt out of the screen: face-on at spread 0, the 0.46 aspect
    /// at full spread.
    fn tilt(&self) -> (f32, f32) {
        let c = 1.0 + (STACK_ASPECT - 1.0) * self.spread.clamp(0.0, 1.2);
        let c = c.clamp(0.2, 1.0);
        (c, (1.0 - c * c).max(0.0).sqrt())
    }

    /// A point of the stack: `axial` along the spindle (reference px on screen at full spread),
    /// at `radius` and `angle` in the ring plane. (screen position, depth; depth > 0 is behind
    /// the centre).
    pub fn point(&self, axial: f32, radius: f32, angle: f32) -> (Pos2, f32) {
        let a2 = Self::axis_dir();
        let (c, s) = self.tilt();
        let full_s = (1.0 - STACK_ASPECT * STACK_ASPECT).sqrt();
        // The ring centre on the 3D spindle u = (a2·sin, cos): its screen offset is `axial`
        // at full spread and shrinks to 0 as the planes fold flat.
        let along = axial * self.spread / full_s;
        let centre = [a2.x * s * along, a2.y * s * along, c * along];
        // In-plane basis: e1 = the major axis (+60° on screen), e2 = the foreshortened minor one.
        let e1 = [-a2.y, a2.x, 0.0];
        let e2 = [a2.x * c, a2.y * c, -s];
        let (ca, sa) = (angle.cos(), angle.sin());
        let p: [f32; 3] = std::array::from_fn(|k| centre[k] + radius * (ca * e1[k] + sa * e2[k]));
        let scale = STACK_FOCAL / (STACK_FOCAL + p[2]).max(STACK_FOCAL * 0.3);
        (self.centre + vec2(p[0], p[1]) * self.unit * scale, p[2] * self.unit)
    }

    /// Ring `i`'s centre on screen (and its depth).
    pub fn ring_centre(&self, i: usize) -> (Pos2, f32) {
        self.point(STACK_AXIAL[i], 0.0, 0.0)
    }

    /// `steps` points around a ring at `axial` / `radius`, starting at `spin`.
    pub fn ring(&self, axial: f32, radius: f32, spin: f32, steps: usize) -> Vec<(Pos2, f32)> {
        (0..steps).map(|k| self.point(axial, radius, spin + TAU * k as f32 / steps as f32)).collect()
    }

    /// The laser: `SPINDLE_SEGMENTS + 1` points from the front tip to the rear tip with their
    /// depth and glow (sin² taper: nothing at the tips).
    pub fn spindle(&self) -> Vec<(Pos2, f32, f32)> {
        (0..=SPINDLE_SEGMENTS)
            .map(|k| {
                let t = k as f32 / SPINDLE_SEGMENTS as f32;
                let (p, d) = self.point(SPINDLE_FROM + (SPINDLE_TO - SPINDLE_FROM) * t, 0.0, 0.0);
                (p, d, (t * PI).sin().powi(2))
            })
            .collect()
    }

    /// Rings farthest first (painter's algorithm).
    pub fn draw_order(&self) -> Vec<usize> {
        let mut order: Vec<usize> = (0..STACK_RINGS).collect();
        order.sort_by(|a, b| self.ring_centre(*b).1.total_cmp(&self.ring_centre(*a).1));
        order
    }
}

/// A tiny deterministic hash (0-1) for the sparks.
fn hash01(i: usize, salt: u32) -> f32 {
    let mut x = (i as u32).wrapping_mul(0x9E37_79B9) ^ salt.wrapping_mul(0x85EB_CA6B);
    x ^= x >> 15;
    x = x.wrapping_mul(0x2C1B_3C6D);
    x ^= x >> 12;
    x = x.wrapping_mul(0x297A_2D39);
    x ^= x >> 15;
    (x & 0x00FF_FFFF) as f32 / 0x00FF_FFFF as f32
}

/// Spark `i` at time `clock`: (axial, radius, angle, brightness 0-1, pink?).
pub fn spark(i: usize, clock: f32) -> (f32, f32, f32, f32, bool) {
    let axial = -125.0 + 230.0 * hash01(i, 1);
    let radius = 70.0 + 110.0 * hash01(i, 2);
    let drift = (0.05 + 0.12 * hash01(i, 3)) * if hash01(i, 4) > 0.5 { 1.0 } else { -1.0 };
    let angle = TAU * hash01(i, 5) + drift * clock;
    let twinkle = 0.45 + 0.55 * (clock * (1.3 + hash01(i, 6)) + TAU * hash01(i, 7)).sin().max(0.0).powf(1.5);
    (axial, radius, angle, twinkle.min(1.0), hash01(i, 8) < 0.4)
}

/// Ring 6's rose arcs at `clock`: (main start, main span, tracer start, tracer span) in radians.
/// The main arc orbits the halo with a breathing span; a shorter tracer counter-rotates.
pub fn halo_arcs(clock: f32) -> (f32, f32, f32, f32) {
    let span = 0.52 + 0.16 * (clock * 1.8).sin();
    (clock * 0.75, span, -clock * 1.15 + 2.1, 0.4 * span)
}

/// Silent length of ring 5's rose ticks (reference px).
pub const TACH_BASE_LEN: f32 = 7.0;
/// How far ring 5's ticks surge at full voice (px).
pub const TACH_SURGE_PX: f32 = 36.0;
/// How far the double inner rings swell at full voice (px).
pub const VOICE_SWELL_PX: f32 = 14.0;
/// Ring 5's three rose clusters, 120° apart (0°, 120°, 240° in ring space): on the white 10°
/// grid, so the equaliser is symmetric around the whole ring.
pub const TACH_CLUSTERS: [f32; 3] = [0.0, TAU / 3.0, 2.0 * TAU / 3.0];
/// Angle between two rose ticks: exactly 2° (five steps per white tick).
pub const TACH_STEP: f32 = TAU / 180.0;
/// Ticks either side of a cluster's centre (2K + 1 = 11 per cluster, ±10°).
pub const TACH_HALF: i32 = 5;

/// Ring 5's voice equaliser, one cluster at `voice` (0-1): each tick's (offset from the cluster
/// centre in whole tick steps, length px). The count never changes, so no tick ever jumps; the
/// lengths follow a symmetric cos² window, longest in the middle, the outer ticks at rest.
pub fn tach_ticks(voice: f32) -> Vec<(f32, f32)> {
    let v = if voice < 0.02 { 0.0 } else { voice.min(1.0) };
    (-TACH_HALF..=TACH_HALF)
        .map(|k| {
            let w = ((k as f32 / TACH_HALF as f32) * std::f32::consts::FRAC_PI_2).cos().powi(2);
            (k as f32, TACH_BASE_LEN + v * TACH_SURGE_PX * w)
        })
        .collect()
}

// ------------------------------------------------------------------------------ meshes ----

/// A premultiplied colour with zero alpha: blends additively (light on the canvas).
fn glow(rgb: [u8; 3], amount: f32) -> Color32 {
    let a = amount.clamp(0.0, 1.0);
    Color32::from_rgba_premultiplied((rgb[0] as f32 * a) as u8, (rgb[1] as f32 * a) as u8, (rgb[2] as f32 * a) as u8, 0)
}

fn mix(a: [u8; 3], b: [u8; 3], t: f32) -> [u8; 3] {
    std::array::from_fn(|i| (a[i] as f32 + (b[i] as f32 - a[i] as f32) * t.clamp(0.0, 1.0)) as u8)
}

/// A ribbon along `points` with a colour per vertex. `soft`: a glow at full light across its
/// middle 40 % that falls off to both edges; otherwise a crisp stroke with a 0.8 px
/// anti-aliasing feather.
fn ribbon(mesh: &mut Mesh, points: &[Pos2], colours: &[Color32], width: f32, closed: bool, soft: bool) {
    let n = points.len();
    if n < 2 {
        return;
    }
    let half = (width * 0.5).max(0.3);
    let feather = 0.8;
    let profile: &[(f32, bool)] = if soft { &[(-half, false), (-0.2 * half, true), (0.2 * half, true), (half, false)] } else { &[(-(half + feather), false), (-half, true), (half, true), (half + feather, false)] };
    let base = mesh.vertices.len() as u32;
    for i in 0..n {
        let prev = if i > 0 { points[i - 1] } else if closed { points[n - 1] } else { points[i] };
        let next = if i + 1 < n { points[i + 1] } else if closed { points[0] } else { points[i] };
        let t = next - prev;
        let len = t.length();
        let normal = if len > 1e-5 { vec2(-t.y / len, t.x / len) } else { vec2(0.0, 1.0) };
        for (offset, lit) in profile {
            mesh.colored_vertex(points[i] + normal * *offset, if *lit { colours[i] } else { Color32::TRANSPARENT });
        }
    }
    let cols = profile.len() as u32;
    let segments = if closed { n } else { n - 1 };
    for s in 0..segments {
        let (a, b) = (s as u32, ((s + 1) % n) as u32);
        for k in 0..cols - 1 {
            let (a0, a1) = (base + a * cols + k, base + a * cols + k + 1);
            let (b0, b1) = (base + b * cols + k, base + b * cols + k + 1);
            mesh.add_triangle(a0, b0, b1);
            mesh.add_triangle(a0, b1, a1);
        }
    }
}

/// A soft round mote (a fan: bright centre, transparent rim).
fn mote(mesh: &mut Mesh, at: Pos2, radius: f32, colour: Color32) {
    let base = mesh.vertices.len() as u32;
    mesh.colored_vertex(at, colour);
    let n = 10;
    for k in 0..n {
        let a = TAU * k as f32 / n as f32;
        mesh.colored_vertex(at + vec2(a.cos(), a.sin()) * radius, Color32::TRANSPARENT);
    }
    for k in 0..n {
        mesh.add_triangle(base, base + 1 + k, base + 1 + (k + 1) % n);
    }
}

/// The soft passes are lit this much brighter than their nominal alpha: added light on obsidian
/// needs it to read as bloom rather than a faint smudge.
pub const SOFT_BLOOM_GAIN: f32 = 2.5;

/// The four HDR bloom passes along a line: the aura, the halo and the body in `base` at full
/// saturation (`theme_engine::hdr_neon_color`), then the 1.5 px filament in its white-hot centre
/// (90 % white + 10 % base). `light[i]`: per-vertex light (fog, taper) times `gain`. (`_hot`: the
/// old inner-glow colour, kept for the call sites.)
#[allow(clippy::too_many_arguments)]
fn neon(mesh: &mut Mesh, points: &[Pos2], light: &[f32], base: [u8; 3], _hot: [u8; 3], gain: f32, scale: f32, closed: bool) {
    use crate::theme_engine::{hdr_neon_color, NeonLayer, NEON_PASSES};
    let hot_centre = whiten(base, 0.9);
    for (k, (width, alpha, layer)) in NEON_PASSES.iter().enumerate() {
        let rgb = if *layer == NeonLayer::HotCenter { hot_centre } else { base };
        let colours: Vec<Color32> = light.iter().map(|l| {
            let a = (*alpha as f32 * gain * l).clamp(0.0, 255.0) as u8;
            if *layer == NeonLayer::HotCenter { glow(rgb, a as f32 / 255.0) } else { hdr_neon_color(rgb, a) }
        }).collect();
        ribbon(mesh, points, &colours, width * scale, closed, k < 2);
    }
}

/// A crisp line with per-vertex light.
fn line(mesh: &mut Mesh, points: &[Pos2], light: &[f32], rgb: [u8; 3], alpha: f32, width: f32, closed: bool) {
    let colours: Vec<Color32> = light.iter().map(|l| glow(rgb, alpha * l)).collect();
    ribbon(mesh, points, &colours, width, closed, false);
}

/// A soft band (haze) with per-vertex light.
fn haze(mesh: &mut Mesh, points: &[Pos2], light: &[f32], rgb: [u8; 3], alpha: f32, width: f32, closed: bool) {
    let colours: Vec<Color32> = light.iter().map(|l| glow(rgb, alpha * l)).collect();
    ribbon(mesh, points, &colours, width, closed, true);
}

/// The slice of a closed ring from `start` for `length` (fractions of a turn).
fn arc_of<T: Copy>(points: &[T], start: f32, length: f32) -> Vec<T> {
    let n = points.len();
    if n < 2 {
        return Vec::new();
    }
    let from = (start.rem_euclid(1.0) * n as f32) as usize;
    let count = ((length * n as f32).ceil() as usize).max(2);
    (0..=count).map(|k| points[(from + k) % n]).collect()
}

// ---------------------------------------------------------------------------- idle morph ----

/// While JAI idles, the front emblem now and then unfolds into the six-ring gyroscope and
/// folds back, calmly: after 120-240 s of uninterrupted idle (random) it holds the other form
/// for 120-240 s. Any activity ends it at once.
#[derive(Debug, Clone, Copy)]
pub struct IdleMorph {
    next_at: f32,
    until: f32,
    seed: u32,
}

impl IdleMorph {
    pub const INTERVAL: (f32, f32) = (120.0, 240.0);
    pub const DURATION: (f32, f32) = (120.0, 240.0);

    pub fn new(seed: u32) -> Self {
        let mut m = Self { next_at: 0.0, until: 0.0, seed: seed.max(1) };
        m.next_at = m.uniform(Self::INTERVAL);
        m
    }

    /// xorshift32: a value in `range`.
    fn uniform(&mut self, (lo, hi): (f32, f32)) -> f32 {
        self.seed ^= self.seed << 13;
        self.seed ^= self.seed >> 17;
        self.seed ^= self.seed << 5;
        lo + (hi - lo) * (self.seed % 10_000) as f32 / 10_000.0
    }

    /// Whether the gyroscope shows at `clock` (s); `idle` false resets the countdown.
    pub fn step(&mut self, clock: f32, idle: bool) -> bool {
        if !idle {
            self.until = 0.0;
            self.next_at = clock + self.uniform(Self::INTERVAL);
            return false;
        }
        if clock < self.until {
            return true;
        }
        if clock >= self.next_at {
            self.until = clock + self.uniform(Self::DURATION);
            self.next_at = self.until + self.uniform(Self::INTERVAL);
            return true;
        }
        false
    }
}

/// Exponential smoothing towards `target` (frame-rate independent, never overshoots, always
/// from the current mid-flight value).
pub fn approach(value: f32, target: f32, rate: f32, dt: f32) -> f32 {
    value + (target - value) * (1.0 - (-rate * dt).exp())
}

/// A frame time safe to animate with: a wake-up frame after an idle second never jumps.
pub fn safe_dt(dt: f32) -> f32 {
    if dt.is_finite() { dt.clamp(0.001, 0.033) } else { 0.016 }
}

// ------------------------------------------------------------------------------ emblem ----

/// The animated emblem.
pub struct RoseEmblem {
    state: RoseEmblemState,
    busy_target: bool,
    flat: Tween,
    dock: Tween,
    spin: [f32; RING_COUNT],
    stack_spin: [f32; STACK_RINGS],
    /// The stack's axial spread (0 flat … 1 full) and its velocity: a critically damped spring
    /// following `1 - flat`.
    spread: f32,
    spread_v: f32,
    clock: f32,
    /// Smoothed voice amplitude (0-1).
    voice: f32,
    /// Extra spin while something crunches (research, thinking).
    busy: f32,
    /// How far the text console has slid in (px): the docked emblem keeps left of it.
    console_width: f32,
    idle_morph: IdleMorph,
    /// Where Sir put the emblem (fractions of the canvas, None = home), and where it is now
    /// (eased towards it; None until the first frame).
    custom_home: Option<Vec2>,
    home_now: Option<Vec2>,
}

impl Default for RoseEmblem {
    fn default() -> Self {
        Self::new(RoseEmblemState::ThinkingGyroscope)
    }
}

impl RoseEmblem {
    pub fn new(state: RoseEmblemState) -> Self {
        let (flat, dock) = pose_targets(state, false);
        Self {
            state,
            busy_target: false,
            flat: Tween::fixed(flat),
            dock: Tween::fixed(dock),
            spin: [0.0, 1.1, 2.3, 0.4, 3.0, 4.4, 5.1],
            stack_spin: [0.3, 1.7, 0.9, 2.6, 4.1, 5.5],
            spread: 1.0 - flat,
            spread_v: 0.0,
            clock: 0.0,
            voice: 0.0,
            busy: 0.0,
            console_width: 0.0,
            idle_morph: IdleMorph::new(0x9E37_79B9),
            custom_home: None,
            home_now: None,
        }
    }

    /// Puts the emblem anywhere on the canvas (`at`: fractions 0-1, None = back home); it glides
    /// there from wherever it is.
    pub fn set_custom_home(&mut self, at: Option<Vec2>) {
        self.custom_home = at.map(|f| vec2(f.x.clamp(0.0, 1.0), f.y.clamp(0.0, 1.0)));
    }

    pub fn custom_home(&self) -> Option<Vec2> {
        self.custom_home
    }

    /// Follows the pointer while Sir drags the emblem (no easing lag).
    pub fn drag_to(&mut self, at: Vec2) {
        self.set_custom_home(Some(at));
        self.home_now = self.custom_home;
    }

    /// The undocked centre and radius on `screen`: Sir's spot (kept a radius inside the canvas),
    /// else the middle (left of the text console when it would cut in).
    fn home_target(&self, screen: Rect) -> (Vec2, f32) {
        let (home, radius) = home_pose(screen, self.console_width);
        let to_frac = |p: Pos2| vec2((p.x - screen.left()) / screen.width().max(1.0), (p.y - screen.top()) / screen.height().max(1.0));
        match self.custom_home {
            Some(f) => {
                let r = centre_radius(screen);
                let p = screen.min + vec2(f.x * screen.width(), f.y * screen.height());
                let p = pos2(p.x.clamp(screen.left() + r, (screen.right() - r).max(screen.left() + r)), p.y.clamp(screen.top() + r, (screen.bottom() - r).max(screen.top() + r)));
                (to_frac(p), r)
            }
            None => (to_frac(home), radius),
        }
    }

    /// Eases the emblem's home towards its target (call once per frame with the canvas).
    pub fn glide(&mut self, screen: Rect, dt: f32) {
        let (target, _) = self.home_target(screen);
        let now = self.home_now.unwrap_or(target);
        let dt = safe_dt(dt);
        self.home_now = Some(vec2(approach(now.x, target.x, 14.0, dt), approach(now.y, target.y, 14.0, dt)));
    }

    pub fn state(&self) -> RoseEmblemState {
        self.state
    }

    /// The text console's slid-in width this frame (0 when closed).
    pub fn set_console_width(&mut self, width: f32) {
        self.console_width = width.max(0.0);
    }

    /// Animates towards a state: the unfold is a 0.45 s cubic ease-out; leaving the corner for the
    /// stack, the emblem first glides home flat and then unfolds into the stack.
    pub fn set_state(&mut self, state: RoseEmblemState, busy: bool) {
        if state == self.state && busy == self.busy_target {
            return;
        }
        let (flat, dock) = pose_targets(state, busy);
        let now = self.clock;
        let leaving_corner = self.state == RoseEmblemState::CornerDocked && state == RoseEmblemState::ThinkingGyroscope;
        self.dock.retarget(dock, now, DOCK_SECS, true);
        if leaving_corner && (self.flat.to - flat).abs() > 1e-6 {
            // Glide home flat, then unfold into the stack.
            self.flat = Tween { from: self.flat.value(now), to: flat, start: now + DOCK_SECS * 0.8, secs: UNFOLD_SECS * 1.4, in_out: false };
        } else {
            self.flat.retarget(flat, now, UNFOLD_SECS, false);
        }
        self.state = state;
        self.busy_target = busy;
    }

    /// Picks the state from `inputs` (with the idle morph's turn) and animates towards it.
    pub fn follow(&mut self, mut inputs: EmblemInputs) {
        inputs.idle_gyro = self.idle_morph.step(self.clock, inputs.idle);
        self.set_state(resolve_state(inputs), inputs.busy());
    }

    /// Advances the clock, the ring spins, the stack's spring and the voice envelope (`voice`
    /// 0-1, `busy`: a research agent or the language model is working).
    pub fn step(&mut self, dt: f32, voice: f32, busy: bool) {
        let dt = safe_dt(dt);
        self.clock += dt;
        let rate: f32 = if voice > self.voice { 0.45 } else { 0.86 };
        self.voice += (voice.clamp(0.0, 1.0) - self.voice) * (1.0 - rate.powf(dt * 60.0));
        let target = if busy { 1.0 } else { 0.0 };
        self.busy += (target - self.busy) * (1.0 - 0.94f32.powf(dt * 60.0));
        let flat = self.flat.value(self.clock).clamp(0.0, 1.0);
        let boost = 1.0 + 2.6 * self.busy + 2.4 * self.voice;
        for (angle, speed) in self.spin.iter_mut().zip(RING_SPIN) {
            *angle = (*angle + speed * boost * dt).rem_euclid(TAU);
        }
        let stack_boost = 1.0 + 0.9 * self.busy + 0.6 * self.voice;
        for (angle, speed) in self.stack_spin.iter_mut().zip(STACK_SPIN) {
            *angle = (*angle + speed * stack_boost * dt).rem_euclid(TAU);
        }
        spring_step(&mut self.spread, &mut self.spread_v, 1.0 - flat, SPREAD_OMEGA, dt);
    }

    /// Whether a transition is still running (the HUD repaints at 60 FPS meanwhile).
    pub fn is_animating(&self) -> bool {
        let settling = (self.spread - (1.0 - self.flat.to)).abs() > 1e-3 || self.spread_v.abs() > 1e-3;
        let gliding = matches!((self.custom_home, self.home_now), (Some(t), Some(n)) if (t - n).length() > 1e-4);
        !self.flat.done(self.clock) || !self.dock.done(self.clock) || settling || gliding
    }

    /// This frame's placement on `screen`.
    pub fn frame(&self, screen: Rect) -> EmblemFrame {
        let dock = self.dock.value(self.clock).clamp(0.0, 1.0);
        let flat = self.flat.value(self.clock).clamp(0.0, 1.0);
        let docked = crate::hud_dock::docked_centre(screen, self.console_width);
        let (target, home_radius) = self.home_target(screen);
        let f = self.home_now.unwrap_or(target);
        let home = screen.min + vec2(f.x * screen.width(), f.y * screen.height());
        let centre = home.lerp(docked, dock);
        let radius = home_radius + (DOCKED_DIAMETER / 2.0 - home_radius) * dock;
        EmblemFrame { centre, radius, flat, dock }
    }

    /// The thinking stack for a frame (its breathing spread).
    pub fn stack_frame(&self, frame: &EmblemFrame) -> StackFrame {
        // While he thinks the stack breathes a few percent along its spindle.
        let breath = 1.0 + 0.035 * self.busy * (self.clock * 1.6).sin();
        StackFrame { centre: frame.centre, unit: frame.unit(), spread: (self.spread * breath).clamp(0.0, 1.2) }
    }

    /// Paints the emblem on `screen` (the HUD canvas).
    pub fn paint(&self, painter: &Painter, screen: Rect) {
        let frame = self.frame(screen);
        self.paint_frame(painter, &frame);
    }

    /// Paints the emblem filling a small window (pure orb mode), at its current pose.
    pub fn paint_in(&self, painter: &Painter, rect: Rect) {
        let mut frame = self.frame(rect);
        frame.centre = rect.center();
        frame.radius = rect.width().min(rect.height()) * 0.36;
        frame.dock = 0.0;
        self.paint_frame(painter, &frame);
    }

    fn paint_frame(&self, painter: &Painter, frame: &EmblemFrame) {
        // The front rings fade in as the stack folds onto them, and out as it unfolds.
        let front = smoothstep(0.15, 0.9, frame.flat);
        let stack = 1.0 - front;
        if stack > 0.01 {
            self.paint_stack(painter, &self.stack_frame(frame), stack);
        }
        if front > 0.01 {
            self.paint_front(painter, frame, front);
        }
    }

    /// The seven concentric front rings, their fixed markers and the wordmark.
    fn paint_front(&self, painter: &Painter, frame: &EmblemFrame, alpha: f32) {
        let scale = frame.unit().clamp(0.35, 1.8);
        for i in 0..RING_COUNT {
            let mut mesh = Mesh::default();
            self.ring_mesh(&mut mesh, frame, i, scale, alpha);
            if !mesh.indices.is_empty() {
                painter.add(Shape::mesh(mesh));
            }
        }
        let mut marks = Mesh::default();
        let unit = frame.unit();
        let outer = RING_RADII_PX[2] * unit;
        // Four double-bar markers just outside the rim, on the diagonals.
        for k in 0..4 {
            let a = PI / 4.0 + k as f32 * PI / 2.0;
            let (radial, tangent) = (vec2(a.cos(), a.sin()), vec2(-a.sin(), a.cos()));
            for off in [-2.2f32, 2.2] {
                let c = frame.centre + radial * (outer * 1.09 + off * scale);
                line(&mut marks, &[c - tangent * 4.5 * scale, c + tangent * 4.5 * scale], &[alpha, alpha], WHITE_RGB, 0.85, 1.3 * scale.max(0.6), false);
            }
        }
        // A fixed red arc under the rim.
        let arc: Vec<Pos2> = (0..=24).map(|k| {
            let a = PI * 0.5 - 0.42 + 0.84 * k as f32 / 24.0;
            frame.centre + vec2(a.cos(), a.sin()) * outer * 1.04
        }).collect();
        neon(&mut marks, &arc, &vec![alpha; arc.len()], crimson_rgb(), torus_hot(), 0.9, scale * 0.7, false);
        painter.add(Shape::mesh(marks));
        if frame.radius > 90.0 && frame.dock < 0.5 {
            let font = FontId::monospace((frame.radius * 0.05).clamp(6.5, 9.0));
            painter.text(frame.centre + vec2(0.0, outer * 0.93), Align2::CENTER_CENTER, "TR 2003 // 3500", font, glow(WHITE_RGB, 0.7 * alpha));
        }
        // The wordmark on its dark core disc.
        let a = alpha;
        let disc = 0.52 * frame.radius;
        painter.circle_filled(frame.centre, disc, Color32::from_rgba_unmultiplied(4, 2, 6, (235.0 * a) as u8));
        let size = (frame.radius * 0.17).clamp(8.0, 46.0);
        let font = FontId::new(size, FontFamily::Name("bold".into()));
        let halo = glow(crimson_rgb(), 0.5 * a * (0.55 + 0.45 * self.voice));
        let wordmark = "J.A.I.";
        painter.text(frame.centre + vec2(0.0, 1.5), Align2::CENTER_CENTER, wordmark, font.clone(), halo);
        painter.text(frame.centre, Align2::CENTER_CENTER, wordmark, font, Color32::WHITE.gamma_multiply(a));
    }

    fn ring_mesh(&self, mesh: &mut Mesh, frame: &EmblemFrame, i: usize, scale: f32, alpha: f32) {
        let spin = self.spin[i];
        let steps = if frame.radius < 80.0 { 96 } else { 168 };
        // While he speaks the double inner rings swell with every syllable (+14 px at full voice).
        let swell = if matches!(i, 1 | 3) { 1.0 + self.voice * VOICE_SWELL_PX / (RING_RADII_PX[i] * frame.unit()).max(1.0) } else { 1.0 };
        let points = ring_points_at(frame, i, spin, steps, swell);
        let fog = vec![alpha; points.len()];
        let voice = self.voice;
        let pulse = 0.5 + 0.5 * (self.clock * 2.2).sin();
        let lit = |seg: &[Pos2]| -> (Vec<Pos2>, Vec<f32>) { (seg.to_vec(), vec![alpha; seg.len()]) };
        // A fixed radial tick through ring `i` at angle `a` (crosshair marks; they do not spin).
        let cross = |mesh: &mut Mesh, a: f32, from: f32, to: f32, rgb: [u8; 3]| {
            let r = RING_RADII_PX[i] * frame.unit();
            let dir = vec2(a.cos(), a.sin());
            line(mesh, &[frame.centre + dir * r * from, frame.centre + dir * r * to], &[alpha, alpha], rgb, 0.9, 1.2 * scale.max(0.6), false);
        };
        match i {
            // The core's edge: a faint crimson hairline.
            0 => line(mesh, &points, &fog, crimson_rgb(), 0.45, 0.9 * scale.max(0.7), true),
            // The blazing magenta torus, breathing with his voice.
            1 => {
                let gain = 0.85 + 0.3 * voice + 0.15 * self.busy;
                let soft: Vec<f32> = fog.iter().map(|f| f * 0.6).collect();
                haze(mesh, &points, &soft, torus_deep(), 0.16 * SOFT_BLOOM_GAIN, 22.0 * scale, true);
                neon(mesh, &points, &fog, torus_deep(), torus_hot(), gain, scale * 1.25, true);
            }
            // The fine dotted rim.
            2 => {
                for k in 0..120 {
                    let p = points[(k * steps) / 120];
                    mote(mesh, p, 1.1 * scale.max(0.6), glow(mix(crimson_rgb(), WHITE_RGB, if k % 10 == 0 { 0.7 } else { 0.0 }), 0.55 * alpha));
                }
            }
            // A thinner rose ring inside the torus, with two white glints.
            3 => {
                neon(mesh, &points, &fog, crimson_rgb(), torus_hot(), 0.55, scale * 0.6, true);
                for start in [0.2, 0.7] {
                    let (p, l) = lit(&arc_of(&points, start, 0.06));
                    line(mesh, &p, &l, WHITE_RGB, 0.8, 1.3 * scale, false);
                }
            }
            // The tick ring: pink ticks, every sixth a long white one, pulsing with the voice;
            // crosshair marks at top and bottom.
            4 => {
                line(mesh, &points, &fog, crimson_rgb(), 0.35, 0.8 * scale.max(0.7), true);
                let ticks = 72;
                let outer = ring_points(frame, i, spin, ticks);
                let short = ring_points_at(frame, i, spin, ticks, 1.0 - (0.05 + 0.04 * voice) * (1.0 + 0.3 * pulse));
                let long = ring_points_at(frame, i, spin, ticks, 1.0 - 0.11 - 0.05 * voice);
                for k in 0..ticks {
                    let major = k % 6 == 0;
                    let (from, rgb, a, w) = if major { (long[k], WHITE_RGB, 0.95, 1.4) } else { (short[k], crimson_rgb(), 0.6 + 0.3 * voice, 1.0) };
                    line(mesh, &[from, outer[k]], &[alpha, alpha], rgb, a, w * scale.max(0.7), false);
                }
                cross(mesh, -PI / 2.0, 0.84, 1.08, WHITE_RGB);
                // The bottom mark stops at the ring so it never runs through the "TR 2003" label below.
                cross(mesh, PI / 2.0, 0.84, 1.0, WHITE_RGB);
            }
            // Three white technical arcs on a pink hairline.
            5 => {
                line(mesh, &points, &fog, crimson_rgb(), 0.3, 0.7 * scale.max(0.7), true);
                for (k, (start, length)) in [(0.05f32, 0.14f32), (0.36, 0.1), (0.62, 0.18)].into_iter().enumerate() {
                    // The segmented sweeps breathe out and back with his voice.
                    let length = length * (1.0 + 1.2 * voice * (self.clock * 7.0 + k as f32 * 2.1).sin().abs());
                    let (p, l) = lit(&arc_of(&points, start, length));
                    line(mesh, &p, &l, WHITE_RGB, 0.92, 2.2 * scale.max(0.6), false);
                }
            }
            // The inner ring with its vertical crosshair ticks.
            _ => {
                neon(mesh, &points, &fog, crimson_rgb(), torus_hot(), 0.45, scale * 0.5, true);
                cross(mesh, -PI / 2.0, 0.78, 1.18, mix(crimson_rgb(), WHITE_RGB, 0.5));
                cross(mesh, PI / 2.0, 0.78, 1.18, mix(crimson_rgb(), WHITE_RGB, 0.5));
            }
        }
    }

    /// The six-ring thinking stack, the laser and the sparks, back to front, at `alpha`.
    fn paint_stack(&self, painter: &Painter, sf: &StackFrame, alpha: f32) {
        let unit = sf.unit;
        let scale = unit.clamp(0.3, 1.8);
        let fog_radius = 160.0 * unit;
        let mut items: Vec<(f32, Mesh)> = Vec::with_capacity(STACK_RINGS + SPINDLE_SEGMENTS + SPARK_COUNT + 1);
        let light_of = |pts: &[(Pos2, f32)], base: f32| -> Vec<f32> { pts.iter().map(|(_, d)| base * alpha * depth_fog(*d, fog_radius)).collect() };
        let pos = |pts: &[(Pos2, f32)]| -> Vec<Pos2> { pts.iter().map(|(p, _)| *p).collect() };
        let steps = if unit < 0.5 { 96 } else { 180 };
        // The twin core rings breathe with his voice and pulse while he thinks.
        let pulse = 0.8 + 0.2 * (self.clock * 3.0).sin() * (0.4 + 0.6 * self.busy) + 0.35 * self.voice;
        for i in sf.draw_order() {
            let (axial, r, spin) = (STACK_AXIAL[i], STACK_RADII[i], self.stack_spin[i]);
            let ring = sf.ring(axial, r, spin, steps);
            let mut mesh = Mesh::default();
            match i {
                // Ring 1: five ice-white technical arc segments with clean gaps.
                0 => {
                    for s in 0..5 {
                        let seg = arc_of(&ring, s as f32 / 5.0, 0.2 - 0.04);
                        let l = light_of(&seg, 1.0);
                        haze(&mut mesh, &pos(&seg), &l, stack_ice(), 0.22, 7.0 * scale, false);
                        line(&mut mesh, &pos(&seg), &l, stack_ice(), 0.95, 2.2 * scale.max(0.6), false);
                    }
                }
                // Ring 2: a cyan-white wireframe hairline, three bright white arcs, orbital dots.
                1 => {
                    let l = light_of(&ring, 1.0);
                    line(&mut mesh, &pos(&ring), &l, stack_wire(), 0.55, 0.9 * scale.max(0.7), true);
                    for start in [0.05, 0.38, 0.71] {
                        let seg = arc_of(&ring, start, 0.09);
                        line(&mut mesh, &pos(&seg), &light_of(&seg, 1.0), WHITE_RGB, 0.95, 3.0 * scale.max(0.5), false);
                    }
                    for (k, (p, d)) in sf.ring(axial, r * 1.07, spin, 8).into_iter().enumerate() {
                        let a = alpha * depth_fog(d, fog_radius) * if k % 2 == 0 { 0.9 } else { 0.55 };
                        mote(&mut mesh, p, 2.0 * scale.max(0.6), glow(stack_wire(), a));
                        mote(&mut mesh, p, 0.8 * scale.max(0.6), glow(WHITE_RGB, a));
                    }
                }
                // Rings 3 and 4: the twin magenta-rose core rings in their pink-violet haze; a
                // crisp cyan-white ring hugs the inside of ring 4.
                2 | 3 => {
                    let l = light_of(&ring, 1.0);
                    haze(&mut mesh, &pos(&ring), &l, core_pink(), 0.14 * SOFT_BLOOM_GAIN * pulse, 28.0 * scale, true);
                    neon(&mut mesh, &pos(&ring), &l, core_pink(), core_hot(), 0.95 * pulse, scale, true);
                    if i == 3 {
                        let inner = sf.ring(axial, STACK_INNER_CYAN, spin * 0.5, steps);
                        let li = light_of(&inner, 1.0);
                        line(&mut mesh, &pos(&inner), &li, mix(cyan_rgb(), WHITE_RGB, 0.6), 0.9, 1.2 * scale.max(0.6), true);
                    }
                }
                // Ring 5: the precision tachometer: a hairline, white ticks every 10° and three
                // glowing red-pink tick clusters 120° apart on the same grid.
                4 => {
                    let l = light_of(&ring, 1.0);
                    line(&mut mesh, &pos(&ring), &l, ice_rgb(), 0.7, 1.1 * scale.max(0.7), true);
                    let outer = sf.ring(axial, r, spin, 36);
                    let inner = sf.ring(axial, r * 0.92, spin, 36);
                    for k in 0..36 {
                        let f = alpha * depth_fog(outer[k].1, fog_radius);
                        line(&mut mesh, &[inner[k].0, outer[k].0], &[f, f], WHITE_RGB, 0.85, 1.1 * scale.max(0.6), false);
                    }
                    // The rose clusters are a voice equaliser: louder speech (Sir's or his) spikes
                    // the ticks outward from the rim, brighter, the middle ones furthest.
                    let r_in = r * 1.01 - TACH_BASE_LEN;
                    for centre in TACH_CLUSTERS {
                        for (offset, len) in tach_ticks(self.voice) {
                            let a = spin + centre + offset * TACH_STEP;
                            let (o, d) = sf.point(axial, r_in + len, a);
                            let (n, _) = sf.point(axial, r_in, a);
                            let f = alpha * depth_fog(d, fog_radius);
                            let gain = 0.8 + 0.6 * ((len - TACH_BASE_LEN) / TACH_SURGE_PX).min(1.0);
                            neon(&mut mesh, &[n, o], &[f, f], crimson_rgb(), core_hot(), gain, scale * 0.6, false);
                        }
                    }
                }
                // Ring 6: the dotted rear halo (depth-fogged to 0.35).
                _ => {
                    for (p, d) in sf.ring(axial, r, spin, 120) {
                        let a = 0.35 * alpha * depth_fog(d, fog_radius).max(0.6);
                        mote(&mut mesh, p, 1.1 * scale.max(0.6), glow(stack_wire(), a));
                    }
                    let fine_o = sf.ring(axial, r * 0.97, spin, 72);
                    let fine_i = sf.ring(axial, r * 0.94, spin, 72);
                    for k in 0..72 {
                        let f = 0.35 * alpha;
                        line(&mut mesh, &[fine_i[k].0, fine_o[k].0], &[f, f], ice_rgb(), 0.6, 0.7 * scale.max(0.6), false);
                    }
                    // The red-white telemetry arc orbiting the halo, and its counter-rotating tracer.
                    let (main, span, tracer, tracer_span) = halo_arcs(self.clock);
                    for (start, len) in [(main, span), (tracer, tracer_span)] {
                        let arc: Vec<(Pos2, f32)> = (0..=40).map(|k| sf.point(axial, r * 1.03, start + len * k as f32 / 40.0)).collect();
                        let l = light_of(&arc, 1.0);
                        neon(&mut mesh, &pos(&arc), &l, crimson_rgb(), mix(core_hot(), WHITE_RGB, 0.5), 0.9, scale * 0.8, false);
                    }
                }
            }
            items.push((sf.ring_centre(i).1, mesh));
        }
        // The laser, one depth-sorted piece per segment so it threads through the rings.
        let laser = sf.spindle();
        let beam = 0.85 + 0.15 * (self.clock * 3.1).sin() + 0.2 * self.busy;
        for w in laser.windows(2) {
            let (a, b) = (w[0], w[1]);
            let light = [a.2 * alpha * beam * depth_fog(a.1, fog_radius), b.2 * alpha * beam * depth_fog(b.1, fog_radius)];
            let mut mesh = Mesh::default();
            haze(&mut mesh, &[a.0, b.0], &light, laser_glow(), 0.55, 8.0 * scale, false);
            line(&mut mesh, &[a.0, b.0], &light, laser_core(), 1.0, 1.4 * scale.max(0.6), false);
            items.push(((a.1 + b.1) * 0.5, mesh));
        }
        // Pinpoint sparks.
        for i in 0..SPARK_COUNT {
            let (axial, r, angle, bright, pink) = spark(i, self.clock);
            let (p, d) = sf.point(axial, r, angle);
            let a = bright * alpha * depth_fog(d, fog_radius);
            let mut mesh = Mesh::default();
            mote(&mut mesh, p, 2.2 * scale.max(0.6), glow(if pink { core_hot() } else { ice_rgb() }, 0.5 * a));
            mote(&mut mesh, p, 0.75 * scale.max(0.6), glow(WHITE_RGB, a));
            items.push((d, mesh));
        }
        // Back to front.
        items.sort_by(|a, b| b.0.total_cmp(&a.0));
        for (_, mesh) in items {
            if !mesh.indices.is_empty() {
                painter.add(Shape::mesh(mesh));
            }
        }
        // The dial's degree numbers and the halo's telemetry label (big enough to read only).
        if unit > 0.7 && sf.spread > 0.6 {
            let font = FontId::monospace((5.0 * unit).clamp(6.0, 9.0));
            // The label glides with the orbiting arc's midpoint, on its outer side.
            let (main, span, _, _) = halo_arcs(self.clock);
            let (p, _) = sf.point(STACK_AXIAL[5], STACK_RADII[5] * 1.13, main + span * 0.5);
            let align = if p.x < sf.ring_centre(5).0.x { Align2::RIGHT_CENTER } else { Align2::LEFT_CENTER };
            let colour = Color32::from_rgb(0xFF, 0x9A, 0xB4).gamma_multiply(0.8 * alpha);
            let telemetry = painter.layout_no_wrap("TR 2003 // 3500".to_string(), font.clone(), colour);
            let telemetry_at = align.anchor_size(p, telemetry.size());
            // The dial's numbers turn under it: one that would cross the label is left out until
            // it has passed (2026-11-07 verifier: "180" drawn over "TR 2003 // 3500").
            let spin = self.stack_spin[4];
            for (k, label) in ["180", "200", "220"].iter().enumerate() {
                let (q, d) = sf.point(STACK_AXIAL[4], STACK_RADII[4] * 0.8, spin + 0.12 + k as f32 * 0.14);
                let ink = ice().gamma_multiply(0.55 * alpha * depth_fog(d, fog_radius));
                let number = painter.layout_no_wrap(label.to_string(), font.clone(), ink);
                let at = Align2::CENTER_CENTER.anchor_size(q, number.size());
                if !at.expand(2.0).intersects(telemetry_at) {
                    painter.galley(at.min, number, ink);
                }
            }
            painter.galley(telemetry_at.min, telemetry, colour);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;


    fn screen() -> Rect {
        Rect::from_min_size(pos2(0.0, 0.0), vec2(1920.0, 1080.0))
    }

    fn full_stack() -> StackFrame {
        StackFrame { centre: pos2(0.0, 0.0), unit: 1.0, spread: 1.0 }
    }

    #[test]
    fn cubic_ease_out_is_monotonic_and_fast_first() {
        assert_eq!(ease_out_cubic(0.0), 0.0);
        assert_eq!(ease_out_cubic(1.0), 1.0);
        assert!(ease_out_cubic(0.5) > 0.85);
        let mut last = 0.0;
        for k in 1..=20 {
            let v = ease_out_cubic(k as f32 / 20.0);
            assert!(v >= last);
            last = v;
        }
    }

    #[test]
    fn front_state_is_seven_flat_concentric_rings() {
        let f = EmblemFrame { centre: pos2(100.0, 100.0), radius: 50.0, flat: 1.0, dock: 0.0 };
        for (i, radius_px) in RING_RADII_PX.iter().enumerate() {
            for p in ring_points(&f, i, 0.3, 32) {
                let r = (p - f.centre).length();
                assert!((r - radius_px / REFERENCE_RADIUS * 50.0).abs() < 1e-3, "ring {i} radius {r}");
            }
        }
        let mut radii = RING_RADII_PX.to_vec();
        radii.sort_by(f32::total_cmp);
        radii.dedup();
        assert_eq!(radii.len(), RING_COUNT, "every ring has its own radius");
    }

    #[test]
    fn six_rings_stand_on_one_minus_thirty_degree_spindle() {
        let sf = full_stack();
        assert_eq!(STACK_RINGS, 6);
        assert_eq!(STACK_AXIAL, [-115.0, -70.0, -28.0, -8.0, 48.0, 95.0]);
        assert_eq!(STACK_RADII, [92.0, 102.0, 96.0, 88.0, 138.0, 152.0]);
        let dir = StackFrame::axis_dir();
        assert!((dir.y.atan2(dir.x).to_degrees() + 30.0).abs() < 1e-3, "−30° on screen");
        for (i, axial) in STACK_AXIAL.iter().enumerate() {
            let (c, _) = sf.ring_centre(i);
            // On the axis line through the centre (perpendicular distance ~0) …
            let cross = c.x * dir.y - c.y * dir.x;
            assert!(cross.abs() < 1e-2, "ring {i} off the axis by {cross}");
            // … at its axial position (within the mild perspective).
            let along = c.x * dir.x + c.y * dir.y;
            assert!((along - axial).abs() < 0.06 * axial.abs() + 0.5, "ring {i}: {along}");
        }
        // Ring 1 is front-most and bottom-left, ring 6 rear-most and top-right.
        let (front, d_front) = sf.ring_centre(0);
        let (back, d_back) = sf.ring_centre(5);
        assert!(front.x < 0.0 && front.y > 0.0 && back.x > 0.0 && back.y < 0.0);
        assert!(d_front < 0.0 && d_back > 0.0);
        let order = sf.draw_order();
        assert_eq!((order[0], order[STACK_RINGS - 1]), (5, 0), "drawn back to front");
    }

    #[test]
    fn each_ring_is_an_ellipse_tilted_plus_sixty_with_aspect_046() {
        let sf = full_stack();
        let pts = sf.ring(0.0, 100.0, 0.0, 720);
        let c = pts.iter().fold(vec2(0.0, 0.0), |acc, (p, _)| acc + p.to_vec2()) / pts.len() as f32;
        let (far, _) = pts.iter().map(|(p, _)| (*p - c.to_pos2(), 0)).max_by(|a, b| a.0.length().total_cmp(&b.0.length())).unwrap();
        let (near, _) = pts.iter().map(|(p, _)| (*p - c.to_pos2(), 0)).min_by(|a, b| a.0.length().total_cmp(&b.0.length())).unwrap();
        let major = far.y.atan2(far.x).to_degrees().rem_euclid(180.0);
        assert!((major - 60.0).abs() < 2.0, "major axis at {major}°");
        let aspect = near.length() / far.length();
        assert!((aspect - STACK_ASPECT).abs() < 0.03, "aspect {aspect}");
        // Folded flat (spread 0) the rings are concentric circles on the centre.
        let flat = StackFrame { spread: 0.0, ..sf };
        for i in 0..STACK_RINGS {
            let (c, d) = flat.ring_centre(i);
            assert!(c.to_vec2().length() < 1e-3 && d.abs() < 1e-3);
            for (p, _) in flat.ring(STACK_AXIAL[i], STACK_RADII[i], 0.4, 16) {
                assert!((p.to_vec2().length() - STACK_RADII[i]).abs() < 1e-2);
            }
        }
    }

    #[test]
    fn the_laser_threads_every_centre_and_fades_at_both_ends() {
        let sf = full_stack();
        let s = sf.spindle();
        assert_eq!(s.len(), SPINDLE_SEGMENTS + 1);
        assert!(s[0].2.abs() < 1e-6 && s[SPINDLE_SEGMENTS].2.abs() < 1e-6, "no endpoint dots");
        assert!((s[SPINDLE_SEGMENTS / 2].2 - 1.0).abs() < 1e-4);
        // It extends past the front ring (bottom-left) and the rear ring (top-right).
        let dir = StackFrame::axis_dir();
        let along = |p: Pos2| p.x * dir.x + p.y * dir.y;
        assert!(along(s[0].0) < along(sf.ring_centre(0).0) && along(s[SPINDLE_SEGMENTS].0) > along(sf.ring_centre(5).0));
        assert!(s[0].1 < 0.0 && s[SPINDLE_SEGMENTS].1 > 0.0, "front tip nearer, rear tip farther");
    }

    #[test]
    fn the_spring_breathes_the_stack_out_without_overshoot() {
        let (mut x, mut v) = (0.0f32, 0.0f32);
        let mut peak = 0.0f32;
        for _ in 0..60 {
            spring_step(&mut x, &mut v, 1.0, SPREAD_OMEGA, 1.0 / 60.0);
            peak = peak.max(x);
        }
        assert!(x > 0.97 && peak <= 1.0 + 1e-4, "critically damped: {x}, peak {peak}");
        // A long frame does not blow it up.
        spring_step(&mut x, &mut v, 0.0, SPREAD_OMEGA, 0.1);
        assert!(x.is_finite() && x <= 1.0);
        // Thinking: the emblem's spread follows the unfold.
        let mut e = RoseEmblem::new(RoseEmblemState::AwakeFrontEmblem);
        assert!(e.spread < 1e-6);
        e.follow(EmblemInputs { thinking: true, ..Default::default() });
        for _ in 0..60 {
            e.step(1.0 / 60.0, 0.0, true);
        }
        assert!(e.spread > 0.9, "unfolded: {}", e.spread);
    }

    #[test]
    fn rings_spin_at_distinct_harmonics() {
        let mut speeds = STACK_SPIN.iter().map(|s| s.abs()).collect::<Vec<_>>();
        speeds.sort_by(f32::total_cmp);
        speeds.dedup();
        assert_eq!(speeds.len(), STACK_RINGS);
        const { assert!(STACK_SPIN[0] > 0.0 && STACK_SPIN[1] < 0.0, "counter-rotating neighbours") };
        let mut e = RoseEmblem::new(RoseEmblemState::ThinkingGyroscope);
        let start = e.stack_spin;
        e.step(0.05, 0.0, false);
        assert!(e.stack_spin.iter().zip(start).all(|(a, b)| (a - b).abs() > 1e-4));
    }

    #[test]
    fn sparks_float_near_the_rings() {
        for i in 0..SPARK_COUNT {
            for t in [0.0, 3.3, 77.0] {
                let (axial, r, angle, bright, _) = spark(i, t);
                assert!((-130.0..=110.0).contains(&axial) && (60.0..=185.0).contains(&r) && angle.is_finite());
                assert!((0.0..=1.0).contains(&bright));
            }
        }
        assert!((8..=12).contains(&SPARK_COUNT));
    }

    #[test]
    fn the_stack_paints_back_to_front_meshes() {
        let ctx = eframe::egui::Context::default();
        let mut e = RoseEmblem::new(RoseEmblemState::ThinkingGyroscope);
        e.step(0.016, 0.3, true);
        let _ = ctx.run(Default::default(), |ctx| {
            let painter = ctx.layer_painter(eframe::egui::LayerId::background());
            e.paint(&painter, screen());
        });
    }

    #[test]
    fn depth_fog_dims_the_rear_to_35_percent() {
        assert_eq!(depth_fog(-50.0, 175.0), 1.0);
        assert_eq!(depth_fog(0.0, 175.0), 1.0);
        assert!((depth_fog(500.0, 175.0) - REAR_FOG).abs() < 1e-4);
        assert!(depth_fog(40.0, 175.0) < 1.0 && depth_fog(40.0, 175.0) > REAR_FOG);
    }

    #[test]
    fn bloom_has_four_additive_passes_ending_in_a_white_core() {
        assert_eq!(crate::theme_engine::NEON_PASSES.map(|p| (p.0, p.1)), [(22.0, 28), (10.0, 75), (4.2, 220), (1.5, 255)]);
        let c = glow(WHITE_RGB, 245.0 / 255.0);
        assert_eq!(c.a(), 0, "additive: zero alpha");
        assert_eq!(c.r(), 245);
        let mut mesh = Mesh::default();
        let pts = [pos2(0.0, 0.0), pos2(10.0, 0.0), pos2(20.0, 0.0)];
        neon(&mut mesh, &pts, &[1.0, 1.0, 1.0], torus_deep(), torus_hot(), 1.0, 1.0, false);
        assert_eq!(mesh.vertices.len(), 3 * (4 + 4 + 4 + 4));
        let widest = mesh.vertices.iter().map(|v| v.pos.y.abs()).fold(0.0, f32::max);
        assert!((widest - 11.0).abs() < 1e-3, "the aura is 22 px wide: {widest}");
        assert!(mesh.vertices.iter().all(|v| v.color.a() == 0));
    }

    #[test]
    fn state_machine_docks_only_when_the_centre_is_covered() {
        use RoseEmblemState::*;
        let base = EmblemInputs::default();
        assert_eq!(resolve_state(base), AwakeFrontEmblem);
        assert_eq!(resolve_state(EmblemInputs { thinking: true, ..base }), ThinkingGyroscope);
        assert_eq!(resolve_state(EmblemInputs { researching: true, ..base }), ThinkingGyroscope);
        assert_eq!(resolve_state(EmblemInputs { signed_off: true, ..base }), ThinkingGyroscope);
        assert_eq!(resolve_state(EmblemInputs { asleep: true, ..base }), ThinkingGyroscope);
        // Sir's screenshot: only the text console is open, the centre is empty → stay centred.
        assert_eq!(resolve_state(EmblemInputs { console_open: true, ..base }), AwakeFrontEmblem);
        assert_eq!(resolve_state(EmblemInputs { console_open: true, thinking: true, ..base }), ThinkingGyroscope);
        assert_eq!(resolve_state(EmblemInputs { centre_covered: true, ..base }), CornerDocked);
        assert_eq!(resolve_state(EmblemInputs { core_open: true, ..base }), CornerDocked);
        // Docked and thinking: it stays in the corner but tilts into a small stack.
        assert_eq!(resolve_state(EmblemInputs { centre_covered: true, thinking: true, ..base }), CornerDocked);
        assert_eq!(pose_targets(CornerDocked, true), (0.0, 1.0));
        assert_eq!(pose_targets(CornerDocked, false), (1.0, 1.0));
        let mut e = RoseEmblem::new(AwakeFrontEmblem);
        e.follow(EmblemInputs { thinking: true, ..base });
        assert_eq!(e.state(), ThinkingGyroscope);
        for _ in 0..30 {
            e.step(1.0 / 60.0, 0.0, true);
        }
        assert!(e.frame(screen()).flat < 1e-3, "fully unfolded after 0.5 s");
    }

    #[test]
    fn waking_locks_flat_in_under_half_a_second() {
        let mut e = RoseEmblem::new(RoseEmblemState::ThinkingGyroscope);
        e.set_state(RoseEmblemState::AwakeFrontEmblem, false);
        for _ in 0..12 {
            e.step(1.0 / 60.0, 0.0, false);
        }
        let mid = e.frame(screen()).flat;
        assert!(mid > 0.5 && mid < 1.0, "{mid}");
        for _ in 0..16 {
            e.step(1.0 / 60.0, 0.0, false);
        }
        assert!((e.frame(screen()).flat - 1.0).abs() < 1e-3, "locked flat after 0.47 s");
    }

    #[test]
    fn the_centred_emblem_stays_clear_of_the_console() {
        // A wide screen: the 440 px console never reaches the centred emblem.
        let wide = Rect::from_min_size(pos2(0.0, 0.0), vec2(2560.0, 1440.0));
        assert_eq!(home_pose(wide, 440.0), (wide.center(), centre_radius(wide)));
        // Sir's portrait monitor (1080 px wide): it slides left just enough, same size.
        let tall = Rect::from_min_size(pos2(0.0, 0.0), vec2(1080.0, 1920.0));
        let (c, r) = home_pose(tall, 440.0);
        assert_eq!(r, centre_radius(tall));
        assert!(c.x + r <= 1080.0 - 440.0 - 15.9 && c.x - r >= 0.0, "{c:?} r {r}");
        assert_eq!(c.y, tall.center().y);
        assert_eq!(home_pose(tall, 0.0).0, tall.center(), "centred again once the console is closed");
        // Too narrow even for that: it shrinks instead of going under the console.
        let tiny = Rect::from_min_size(pos2(0.0, 0.0), vec2(700.0, 900.0));
        let (c, r) = home_pose(tiny, 440.0);
        assert!(c.x + r <= 700.0 - 440.0 - 15.9 && r < centre_radius(tiny));
    }

    #[test]
    fn docking_glides_to_the_corner_and_keeps_left_of_the_console() {
        let mut e = RoseEmblem::new(RoseEmblemState::AwakeFrontEmblem);
        e.set_state(RoseEmblemState::CornerDocked, false);
        for _ in 0..60 {
            e.step(1.0 / 60.0, 0.0, false);
        }
        let f = e.frame(screen());
        assert_eq!(f.centre, pos2(1920.0 - 75.0, 1080.0 - 75.0));
        assert!((f.radius * 2.0 - DOCKED_DIAMETER).abs() < 0.01);
        // The text console slides out: the docked emblem sits left of it, never under it.
        e.set_console_width(440.0);
        let f = e.frame(screen());
        assert_eq!(f.centre, pos2(1920.0 - 440.0 - 75.0, 1080.0 - 75.0));
        assert!(f.centre.x + f.radius < 1920.0 - 440.0);
        // Signing off: home first, then the stack.
        e.set_console_width(0.0);
        e.set_state(RoseEmblemState::ThinkingGyroscope, false);
        e.step(0.3, 0.0, false);
        assert!(e.frame(screen()).flat > 0.95, "still flat while gliding home");
        for _ in 0..120 {
            e.step(1.0 / 60.0, 0.0, false);
        }
        let f = e.frame(screen());
        assert_eq!(f.centre, screen().center());
        assert!(f.flat < 1e-3);
    }

    #[test]
    fn ring_six_arcs_orbit_and_breathe_with_time() {
        let (m0, s0, t0, ts0) = halo_arcs(0.0);
        let (m1, s1, t1, ts1) = halo_arcs(0.5);
        assert!(m1 > m0 && t1 < t0, "main arc orbits forward, the tracer backward");
        assert!((s1 - s0).abs() > 0.05 && ts0 < s0 && ts1 < s1, "the spans breathe, the tracer stays shorter");
        // Smooth: a 60 FPS frame moves the arc by well under a degree.
        assert!((halo_arcs(10.0 + 1.0 / 60.0).0 - halo_arcs(10.0).0).abs() < 0.02);
        for t in [0.0, 3.0, 100.0] {
            let (_, s, _, ts) = halo_arcs(t);
            assert!((0.36..=0.68).contains(&s) && ts > 0.1);
        }
    }

    /// Repro 2026-11-03: the clusters sat at 270°/0°/90° (gaps 90°/90°/180°), 2.005° apart, on
    /// half-steps whenever an even number of ticks was lit.
    #[test]
    fn ring_five_clusters_are_120_degrees_apart_on_the_2_degree_grid() {
        let deg: Vec<f32> = TACH_CLUSTERS.iter().map(|c| c.to_degrees().rem_euclid(360.0)).collect();
        let mut sorted = deg.clone();
        sorted.sort_by(f32::total_cmp);
        for k in 0..3 {
            let gap = (sorted[(k + 1) % 3] - sorted[k]).rem_euclid(360.0);
            assert!((gap - 120.0).abs() < 1e-3, "cluster gaps {sorted:?}");
        }
        assert!((TACH_STEP - TAU / 180.0).abs() < 1e-6, "tick step {}°", TACH_STEP.to_degrees());
        for voice in [0.0, 0.33, 0.5, 0.77, 1.0] {
            for (offset, _) in tach_ticks(voice) {
                assert!((offset - offset.round()).abs() < 1e-6, "tick off the grid at voice {voice}: {offset}");
            }
            for c in &deg {
                assert!((c / 10.0 - (c / 10.0).round()).abs() < 1e-4, "cluster centre {c}° between white ticks");
            }
        }
    }

    #[test]
    fn ring_five_ticks_grow_symmetrically_with_the_voice() {
        let n = (2 * TACH_HALF + 1) as usize;
        let silent = tach_ticks(0.0);
        assert_eq!(silent.len(), n);
        assert!(silent.iter().all(|(_, len)| (*len - TACH_BASE_LEN).abs() < 1e-6));
        assert!(tach_ticks(0.01).iter().all(|(_, len)| (*len - TACH_BASE_LEN).abs() < 1e-6), "noise under 0.02 stays silent");
        let loud = tach_ticks(1.0);
        let mid = tach_ticks(0.5);
        assert!(loud.len() == n && mid.len() == n, "the tick count never changes");
        let longest = |ticks: &[(f32, f32)]| ticks.iter().map(|t| t.1).fold(0.0, f32::max);
        assert!(longest(&loud) > TACH_BASE_LEN + 15.0);
        assert!(longest(&mid) > TACH_BASE_LEN && longest(&mid) < longest(&loud));
        // Mirror-symmetric about the centre, longest in the middle, the outer ticks at rest.
        for k in 0..n / 2 {
            assert!((loud[k].0 + loud[n - 1 - k].0).abs() < 1e-6 && (loud[k].1 - loud[n - 1 - k].1).abs() < 1e-4, "tick {k}");
        }
        assert!(loud[n / 2].1 == longest(&loud) && (loud[0].1 - TACH_BASE_LEN).abs() < 1e-4);
    }

    #[test]
    fn idle_jai_morphs_between_the_front_emblem_and_the_gyroscope() {
        let mut m = IdleMorph::new(7);
        // Not idle: never the gyroscope.
        assert!(!(0..200).any(|k| m.step(k as f32 * 0.1, false)));
        // Idle: never before 120 s, within 240 s the gyroscope shows, stays 120-240 s, then the
        // emblem returns (no flip-flopping every few seconds).
        let start = 20.0;
        let mut on = None;
        let mut off = None;
        let mut t = start;
        while t < start + 600.0 {
            let gyro = m.step(t, true);
            match (gyro, on, off) {
                (true, None, _) => on = Some(t),
                (false, Some(_), None) => off = Some(t),
                _ => {}
            }
            t += 0.05;
        }
        let (on, off) = (on.unwrap(), off.unwrap());
        assert!(on - start <= IdleMorph::INTERVAL.1 + 0.1, "first morph after {} s", on - start);
        assert!(on - start >= IdleMorph::INTERVAL.0 - 0.1, "not before 120 s: {} s", on - start);
        let held = off - on;
        assert!((IdleMorph::DURATION.0 - 0.1..=IdleMorph::DURATION.1 + 0.1).contains(&held), "held {held} s");
        // Activity ends it on the spot.
        let mut m = IdleMorph::new(3);
        let mut t = 0.0;
        while !m.step(t, true) {
            t += 0.1;
        }
        assert!(!m.step(t + 0.1, false), "the instant Sir speaks");
        // The state machine: the idle morph only stands in for the front emblem.
        let base = EmblemInputs { idle: true, idle_gyro: true, ..Default::default() };
        assert_eq!(resolve_state(base), RoseEmblemState::ThinkingGyroscope);
        assert_eq!(resolve_state(EmblemInputs { idle: false, ..base }), RoseEmblemState::AwakeFrontEmblem);
        assert_eq!(resolve_state(EmblemInputs { centre_covered: true, ..base }), RoseEmblemState::CornerDocked);
        assert!(!base.busy(), "idling never spins the rings up");
    }

    #[test]
    fn the_emblem_moves_anywhere_and_glides_without_jumps() {
        let s = screen();
        let mut e = RoseEmblem::new(RoseEmblemState::AwakeFrontEmblem);
        e.glide(s, 1.0 / 60.0);
        assert!((e.frame(s).centre - s.center()).length() < 1.0, "home is the centre");
        // "Move the orb to the top left": it glides there, every frame a small step.
        e.set_custom_home(Some(vec2(0.0, 0.0)));
        let mut last = e.frame(s).centre;
        let mut biggest = 0.0f32;
        for _ in 0..120 {
            // A huge frame time (a wake-up after an idle second) is clamped.
            e.glide(s, 1.0);
            let now = e.frame(s).centre;
            biggest = biggest.max((now - last).length());
            last = now;
        }
        let r = centre_radius(s);
        assert!((last - pos2(r, r)).length() < 2.0, "kept a radius inside the canvas: {last:?}");
        assert!(biggest < (s.center() - pos2(r, r)).length() * 0.4, "no jump: largest step {biggest}");
        // Dragging follows the pointer at once.
        e.drag_to(vec2(0.75, 0.6));
        assert!((e.frame(s).centre - pos2(0.75 * 1920.0, 0.6 * 1080.0)).length() < 1.0);
        assert_eq!(safe_dt(5.0), 0.033);
        assert_eq!(safe_dt(f32::NAN), 0.016);
        assert!((approach(0.3, 1.0, 14.0, 0.016) - 0.3).abs() > 0.0, "eases from the mid-flight value");
    }

    #[test]
    fn the_front_emblem_moves_only_with_its_own_voice() {
        let s = screen();
        let mut quiet = RoseEmblem::new(RoseEmblemState::AwakeFrontEmblem);
        let mut talking = RoseEmblem::new(RoseEmblemState::AwakeFrontEmblem);
        for _ in 0..30 {
            quiet.step(1.0 / 60.0, 0.0, false);
            talking.step(1.0 / 60.0, 0.8, false);
        }
        let f = quiet.frame(s);
        let ring = |e: &RoseEmblem| {
            let swell = 1.0 + e.voice * VOICE_SWELL_PX / (RING_RADII_PX[1] * f.unit());
            (ring_points_at(&f, 1, 0.0, 8, swell)[0] - f.centre).length()
        };
        assert_eq!(quiet.voice, 0.0, "silent JAI: calm ticks");
        assert!(ring(&talking) > ring(&quiet) + 4.0, "speaking swells the inner rings");
        assert!(tach_ticks(talking.voice).iter().any(|(_, l)| *l > TACH_BASE_LEN + 5.0));
    }
}
