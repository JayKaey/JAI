//! The complex core: J.A.I. as a sphere of circuitry, after the hologram of the lab scene in
//! "Age of Ultron" (Animal Logic: an aura "full of angular shapes reminiscent of computer
//! circuitry", a man-made construction, with rings of data round it that answer the voice
//! like turning platters). Four shells of circuit board stand round a bright nucleus, each
//! turning at its own pace: stepped traces ending in pads, buses of parallel lines, chips,
//! fields of data. Pulses run along the traces, faster while he thinks; three bright bands
//! swing round the nucleus like a gyroscope; the rings outside are his voice and his tape.
//! What is near is the accent, what is far falls back into the deep tone.

use crate::holo::{self, a, lit, mix, px, Face, Light};
use crate::holo_core::{CoreLook, BANDS};
use eframe::egui::{self, pos2, vec2, Align2, Color32, Painter, Pos2, Rect, Vec2};
use std::f32::consts::{PI, TAU};
use std::sync::LazyLock;

type V3 = [f32; 3];
type M3 = [[f32; 3]; 3];

fn mul(a: &M3, b: &M3) -> M3 {
    let mut m = [[0.0; 3]; 3];
    for (i, row) in m.iter_mut().enumerate() {
        for (j, cell) in row.iter_mut().enumerate() {
            *cell = a[i][0] * b[0][j] + a[i][1] * b[1][j] + a[i][2] * b[2][j];
        }
    }
    m
}

fn apply(m: &M3, v: V3) -> V3 {
    [m[0][0] * v[0] + m[0][1] * v[1] + m[0][2] * v[2], m[1][0] * v[0] + m[1][1] * v[1] + m[1][2] * v[2], m[2][0] * v[0] + m[2][1] * v[1] + m[2][2] * v[2]]
}

fn rot_x(t: f32) -> M3 {
    let (s, c) = t.sin_cos();
    [[1.0, 0.0, 0.0], [0.0, c, -s], [0.0, s, c]]
}

fn rot_y(t: f32) -> M3 {
    let (s, c) = t.sin_cos();
    [[c, 0.0, s], [0.0, 1.0, 0.0], [-s, 0.0, c]]
}

fn rot_z(t: f32) -> M3 {
    let (s, c) = t.sin_cos();
    [[c, -s, 0.0], [s, c, 0.0], [0.0, 0.0, 1.0]]
}

/// A point of the unit sphere (latitude, longitude; y is up).
fn unit(lat: f32, lon: f32) -> V3 {
    [lat.cos() * lon.cos(), lat.sin(), lat.cos() * lon.sin()]
}

/// One shell: its radius (of the core's), its grid, how many traces it carries, how fast it
/// turns (radians a second), how its axis leans and how brightly it is drawn (the outer
/// shells are fainter, or nobody would see in).
struct Shell {
    radius: f32,
    rows: usize,
    cols: usize,
    traces: usize,
    spin: f32,
    lean: (f32, f32),
    gain: f32,
}

pub const SHELLS: usize = 4;

const SHELL: [Shell; SHELLS] = [
    Shell { radius: 0.42, rows: 10, cols: 30, traces: 36, spin: 0.26, lean: (0.35, -0.2), gain: 0.84 },
    Shell { radius: 0.60, rows: 14, cols: 44, traces: 80, spin: -0.15, lean: (-0.22, 0.3), gain: 0.80 },
    Shell { radius: 0.79, rows: 18, cols: 60, traces: 136, spin: 0.09, lean: (0.12, 0.16), gain: 0.78 },
    Shell { radius: 0.97, rows: 22, cols: 78, traces: 176, spin: -0.055, lean: (-0.08, -0.1), gain: 0.70 },
];

/// A line on a shell: its points on the unit sphere, where its pulse starts, whether it ends
/// in a pad, and how heavy it is drawn (1 a trace, more a bar).
struct Wire {
    shell: usize,
    points: Vec<V3>,
    phase: f32,
    pad: bool,
    weight: f32,
}

/// A pin standing out of a shell (the bristling rim).
struct Pin {
    shell: usize,
    at: V3,
    len: f32,
    phase: f32,
}

struct Mind {
    wires: Vec<Wire>,
    pins: Vec<Pin>,
}

struct Rng(u32);

impl Rng {
    fn next(&mut self) -> f32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 17;
        self.0 ^= self.0 << 5;
        (self.0 >> 8) as f32 / (1u32 << 24) as f32
    }

    fn pick(&mut self, n: usize) -> usize {
        ((self.next() * n as f32) as usize).min(n.saturating_sub(1))
    }

    /// -1 … 1, mostly near 0.
    fn about(&mut self) -> f32 {
        self.next() - self.next()
    }
}

/// The same mind every time: it is him, not noise.
static MIND: LazyLock<Mind> = LazyLock::new(|| build(0x4A41_4921));

/// A shell's grid: rows are latitudes (the poles stay bare), columns longitudes. Positions
/// are fractions of a cell, so a bus can run a third of a row beside its neighbour.
struct Grid {
    rows: f32,
    cols: f32,
}

impl Grid {
    fn at(&self, row: f32, col: f32) -> V3 {
        unit((-66.0 + 132.0 * (row / (self.rows - 1.0)).clamp(0.0, 1.0)).to_radians(), TAU * col / self.cols)
    }

    /// Appends the way from `from` to `to` (one of the two must stay the same: a trace runs
    /// along a latitude or up a meridian), in steps small enough to follow the sphere.
    fn run(&self, points: &mut Vec<V3>, from: (f32, f32), to: (f32, f32)) {
        let n = ((to.0 - from.0).abs().max((to.1 - from.1).abs()) * 2.0).ceil().max(1.0) as usize;
        if points.is_empty() {
            points.push(self.at(from.0, from.1));
        }
        for s in 1..=n {
            let k = s as f32 / n as f32;
            points.push(self.at(from.0 + (to.0 - from.0) * k, from.1 + (to.1 - from.1) * k));
        }
    }
}

fn build(seed: u32) -> Mind {
    let mut rng = Rng(seed);
    let mut wires = Vec::new();
    let mut pins = Vec::new();
    for (si, shell) in SHELL.iter().enumerate() {
        let grid = Grid { rows: shell.rows as f32, cols: shell.cols as f32 };
        let (rows, cols) = (grid.rows, grid.cols);
        // The board is populated in districts with bare glass between them: seen through the
        // gaps, the inner shells give the depth.
        let districts: Vec<(f32, f32)> = (0..4 + si * 2).map(|_| (1.0 + rng.next() * (rows - 2.0), rng.next() * cols)).collect();
        let site = |rng: &mut Rng| {
            let (r, c) = districts[rng.pick(districts.len())];
            ((r + rng.about() * rows * 0.22).clamp(0.0, rows - 1.0).round(), (c + rng.about() * cols * 0.13).rem_euclid(cols).round())
        };
        let wire = |points: Vec<V3>, rng: &mut Rng, pad: bool, weight: f32| Wire { shell: si, points, phase: rng.next(), pad, weight };

        // Traces: along a latitude, a step up a meridian, on again; a pad at the end.
        for _ in 0..shell.traces {
            let mut at = site(&mut rng);
            let mut points = Vec::new();
            let mut along = rng.next() < 0.75;
            for _ in 0..2 + rng.pick(3) {
                let to = if along {
                    (at.0, at.1 + (1.0 + rng.pick(4) as f32) * if rng.next() < 0.5 { -1.0 } else { 1.0 })
                } else {
                    ((at.0 + (1.0 + rng.pick(2) as f32) * if rng.next() < 0.5 { -1.0 } else { 1.0 }).clamp(0.0, rows - 1.0), at.1)
                };
                grid.run(&mut points, at, to);
                at = to;
                along = !along;
            }
            let pad = rng.next() < 0.7;
            wires.push(wire(points, &mut rng, pad, 1.0));
        }
        // Buses: three to five lines side by side, turning one corner together.
        for _ in 0..shell.traces / 7 {
            let at = site(&mut rng);
            let (len, rise) = (2.0 + rng.pick(5) as f32, (1.0 + rng.pick(3) as f32) * if rng.next() < 0.5 { -1.0 } else { 1.0 });
            let dir = if rng.next() < 0.5 { -1.0 } else { 1.0 };
            for k in 0..3 + rng.pick(3) {
                let d = k as f32 * 0.32;
                let mut points = Vec::new();
                let corner = (at.0 + d * rise.signum(), at.1 + dir * (len - d));
                grid.run(&mut points, (corner.0, at.1), corner);
                grid.run(&mut points, corner, ((at.0 + rise).clamp(0.0, rows - 1.0), corner.1));
                wires.push(wire(points, &mut rng, k == 0, 1.0));
            }
        }
        // Chips: a small rectangle with legs on its long sides.
        for _ in 0..2 + si * 2 {
            let at = site(&mut rng);
            let (h, w) = (0.8 + rng.next() * 0.8, 1.5 + rng.pick(3) as f32);
            let mut points = Vec::new();
            for (from, to) in [((at.0, at.1), (at.0, at.1 + w)), ((at.0, at.1 + w), (at.0 + h, at.1 + w)), ((at.0 + h, at.1 + w), (at.0 + h, at.1)), ((at.0 + h, at.1), (at.0, at.1))] {
                grid.run(&mut points, from, to);
            }
            wires.push(wire(points, &mut rng, false, 1.0));
            let legs = (w * 2.0) as usize;
            for l in 0..=legs {
                let c = at.1 + w * l as f32 / legs as f32;
                for (from, to) in [(at.0, at.0 - 0.45), (at.0 + h, at.0 + h + 0.45)] {
                    let mut points = Vec::new();
                    grid.run(&mut points, (from, c), (to.clamp(0.0, rows - 1.0), c));
                    wires.push(wire(points, &mut rng, false, 1.0));
                }
            }
        }
        // Fields of data: close parallel lines.
        for _ in 0..2 + si * 2 {
            let at = site(&mut rng);
            let (h, w) = (1.0 + rng.pick(2) as f32, 2.0 + rng.pick(5) as f32);
            let lines = 4 + rng.pick(5);
            for k in 0..=lines {
                let row = (at.0 + h * k as f32 / lines as f32).clamp(0.0, rows - 1.0);
                let mut points = Vec::new();
                grid.run(&mut points, (row, at.1), (row, at.1 + w * (0.4 + 0.6 * rng.next())));
                wires.push(wire(points, &mut rng, false, 1.0));
            }
        }
        // Broken rings: bars along a latitude, drawn heavy.
        for _ in 0..2 + si {
            let row = (rng.next() * (rows - 1.0)).round();
            let mut col = rng.next() * cols;
            let end = col + cols * (0.2 + 0.5 * rng.next());
            while col < end {
                let run = 0.6 + rng.pick(4) as f32;
                let mut points = Vec::new();
                grid.run(&mut points, (row, col), (row, col + run));
                wires.push(wire(points, &mut rng, false, 2.4));
                col += run + 0.5 + rng.pick(3) as f32;
            }
        }
        // Pins: thick on the outer shells, round the equator most of all.
        for _ in 0..si * 60 {
            let lat = rng.about() * 1.15;
            pins.push(Pin { shell: si, at: unit(lat, rng.next() * TAU), len: 0.012 + 0.085 * rng.next() * rng.next(), phase: rng.next() });
        }
    }
    Mind { wires, pins }
}

/// How the whole sphere is turned towards the viewer: seen a little from above.
fn stance(clock: f32) -> M3 {
    mul(&rot_x(-0.36 + 0.03 * (clock * 0.21).sin()), &rot_z(0.14))
}

struct View {
    centre: Pos2,
    radius: f32,
    focal: f32,
}

impl View {
    /// A point in space (in radii) on the screen, and how far it stands to the front (-1 … 1).
    fn project(&self, v: V3) -> (Pos2, f32) {
        let s = self.focal / (self.focal - v[2]);
        (pos2(self.centre.x + v[0] * s * self.radius, self.centre.y - v[1] * s * self.radius), v[2])
    }
}

/// 0 at the back of the sphere … 1 at the front.
fn nearness(z: f32) -> f32 {
    let t = (z * 0.5 + 0.5).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

pub fn paint(p: &Painter, id: egui::Id, c: Pos2, r: f32, look: &CoreLook) {
    paint_turned(p, id, c, r, look, 0.0, 0.0);
}

/// The sphere turned by hand: `yaw` about its axis, `pitch` towards the viewer (the core view).
pub fn paint_turned(p: &Painter, id: egui::Id, c: Pos2, r: f32, look: &CoreLook, yaw: f32, pitch: f32) {
    let t = holo::theme();
    let alpha = look.alpha * (0.25 + 0.75 * look.wake);
    if alpha <= 0.003 || r < 4.0 {
        return;
    }
    let accent = mix(t.accent(), t.alert(), look.fault);
    let deep = mix(t.deep(), t.alert(), look.fault);
    let hot = mix(t.hot(), t.alert(), look.fault * 0.5);
    let w = px(p);
    let clock = look.clock;
    let mind = &*MIND;
    let view = View { centre: c, radius: r, focal: 3.4 };
    let stance = mul(&rot_x(pitch), &mul(&stance(clock), &rot_y(yaw)));
    let pace = 1.0 + 4.0 * look.busy;
    // Small, he is drawn with fewer wires: a corner orb is no place for six hundred of them.
    let detail = (r / 150.0).clamp(0.22, 1.0);
    let energy = ((0.70 + 0.30 * look.voice + 0.20 * look.busy) * alpha).min(1.0);
    let line = w.max(r / 300.0);
    // Every wire glows into the bloom: where the board is dense the air itself is lit.
    let mut light: Light = crate::holo_core::glow(p, id, c, r * 1.66, look.crisp);
    light.spill = (0.20 + 0.10 * look.voice + 0.05 * look.busy) * alpha;

    // The shells.
    let turns: Vec<M3> = SHELL.iter().map(|s| mul(&stance, &mul(&mul(&rot_x(s.lean.0), &rot_z(s.lean.1)), &rot_y(clock * s.spin * pace)))).collect();
    let mut screen: Vec<Pos2> = Vec::with_capacity(32);
    let mut colours: Vec<Color32> = Vec::with_capacity(32);
    for (k, wire) in mind.wires.iter().enumerate() {
        if (k as f32 * 0.618_034).fract() > detail {
            continue;
        }
        let shell = &SHELL[wire.shell];
        // The shells breathe with his voice, the inner ones most.
        let swell = 1.0 + 0.05 * look.voice * (1.0 - wire.shell as f32 * 0.18) * (0.6 + 0.4 * (clock * 5.0 + wire.shell as f32 * 1.7).sin());
        let radius = shell.radius * swell;
        let drive = look.layers[wire.shell];
        screen.clear();
        colours.clear();
        let mut front = 0.0f32;
        for q in &wire.points {
            let v = apply(&turns[wire.shell], [q[0] * radius, q[1] * radius, q[2] * radius]);
            let (at, z) = view.project(v);
            let near = nearness(z / shell.radius.max(0.01));
            front = front.max(near);
            let strength = (0.13 + 0.50 * near * near) * shell.gain * (1.0 + 0.3 * drive) * energy;
            colours.push(lit(mix(deep, accent, near * near), strength.min(1.0)));
            screen.push(at);
        }
        light.ribbon(&screen, &colours, false, line * wire.weight);
        // A pulse runs the wire: one stretch at a time, bright.
        let n = screen.len();
        let speed = (0.20 + 0.55 * look.busy + 0.9 * drive) * (0.6 + wire.phase * 0.8);
        let at = (clock * speed + wire.phase * 7.0).fract();
        if at < 0.5 && n >= 2 && wire.weight < 2.0 {
            let head = ((at / 0.5) * (n - 1) as f32) as usize;
            let seg = [screen[head], screen[(head + 1).min(n - 1)]];
            light.strip(&seg, false, line * 1.6, lit(hot, (0.30 + 0.70 * front) * energy));
        }
        if wire.pad && detail > 0.4 {
            let twinkle = 0.5 + 0.5 * (clock * (1.3 + wire.phase * 2.2) + wire.phase * 40.0).sin();
            light.spark(screen[n - 1], (1.3 + 1.7 * front) * (r / 200.0).max(0.6), lit(hot, (0.18 + 0.82 * front) * (0.35 + 0.65 * twinkle) * energy));
        }
    }
    if detail > 0.5 {
        for pin in &mind.pins {
            let shell = &SHELL[pin.shell];
            let grow = 1.0 + 1.8 * look.voice * (0.5 + 0.5 * (clock * 9.0 + pin.phase * 30.0).sin());
            let (from, z) = view.project(apply(&turns[pin.shell], pin.at.map(|x| x * shell.radius)));
            let (to, _) = view.project(apply(&turns[pin.shell], pin.at.map(|x| x * (shell.radius + pin.len * grow))));
            let near = nearness(z / shell.radius);
            light.seg(from, to, line, lit(mix(deep, hot, near), (0.14 + 0.56 * near) * energy));
        }
    }

    // The rings outside, each in its own plane like the gimbals of a gyroscope.
    let ring_in = |plane: &M3, radius: f32, angle: f32| view.project(apply(plane, [radius * angle.cos(), radius * angle.sin(), 0.0]));
    // His voice: a platter of ticks in the equator's plane, long where the spectrum is loud.
    let plane = mul(&stance, &rot_x(PI / 2.0));
    let ticks = (220.0 * detail) as usize;
    let spin = clock * 0.20 * pace;
    for i in 0..ticks {
        let angle = spin + TAU * i as f32 / ticks as f32;
        let fold = ((i as f32 / ticks as f32 * 4.0).fract() - 0.5).abs() * 2.0;
        let band = look.bands[((fold * (BANDS - 1) as f32) as usize).min(BANDS - 1)];
        let level = band * look.voice.sqrt();
        let (from, z) = ring_in(&plane, 1.10, angle);
        let (to, _) = ring_in(&plane, 1.10 + 0.016 + 0.17 * level, angle);
        let near = nearness(z / 1.1);
        light.seg(from, to, line, lit(mix(deep, hot, 0.2 + 0.8 * level), (0.16 + 0.44 * near + 0.4 * level) * energy));
    }
    // His tape: blocks of data on a ring leaning one way.
    let plane = mul(&stance, &mul(&rot_z(0.62), &rot_x(PI / 2.0 - 0.42)));
    let mut rng = Rng(0x7A91_3C55);
    let mut at = 0.0;
    while at < TAU {
        let run = 0.03 + 0.24 * rng.next() * rng.next();
        let bright = rng.next();
        let from = at - clock * 0.11 * pace;
        let n = ((run * 60.0) as usize).max(2);
        screen.clear();
        colours.clear();
        for s in 0..=n {
            let (q, z) = ring_in(&plane, 1.22, from + run * s as f32 / n as f32);
            screen.push(q);
            colours.push(lit(mix(deep, accent, bright), (0.10 + 0.55 * nearness(z / 1.22)) * (0.45 + 0.55 * bright) * energy));
        }
        light.ribbon(&screen, &colours, false, (r / 75.0 * (0.4 + bright)).max(w));
        at += run + 0.014 + 0.06 * rng.next();
    }
    // His index: a hairline with marks, leaning the other way and turning against the tape.
    let plane = mul(&stance, &mul(&rot_z(-0.5), &rot_x(PI / 2.0 + 0.5)));
    screen.clear();
    colours.clear();
    for s in 0..=180 {
        let (q, z) = ring_in(&plane, 1.34, TAU * s as f32 / 180.0);
        screen.push(q);
        colours.push(lit(mix(deep, accent, 0.4), (0.10 + 0.40 * nearness(z / 1.34)) * energy));
    }
    light.ribbon(&screen, &colours, false, line);
    for i in 0..36 {
        let angle = clock * 0.07 + TAU * i as f32 / 36.0;
        let (from, z) = ring_in(&plane, 1.34, angle);
        let (to, _) = ring_in(&plane, if i % 9 == 0 { 1.42 } else { 1.37 }, angle);
        light.seg(from, to, line, lit(accent, (0.16 + 0.6 * nearness(z / 1.34)) * energy));
    }

    // The gyroscope: three bright bands swinging round the nucleus.
    for (k, (radius, sweep)) in [(0.19, 5.2), (0.27, 4.1), (0.345, 2.9)].into_iter().enumerate() {
        let k = k as f32;
        let turn = mul(&stance, &mul(&rot_y(clock * (0.5 + 0.23 * k) * pace * 0.6 + k * 2.1), &rot_x(1.1 + k * 0.9 + (clock * 0.3 + k).sin() * 0.3)));
        let start = clock * (0.9 - 0.25 * k) * pace * 0.5;
        let n = 72;
        screen.clear();
        colours.clear();
        for s in 0..=n {
            let angle = start + sweep * s as f32 / n as f32;
            let (q, z) = view.project(apply(&turn, [radius * angle.cos(), radius * angle.sin(), 0.0]));
            // Each band fades towards its tail.
            let along = s as f32 / n as f32;
            screen.push(q);
            colours.push(lit(mix(accent, hot, along), (0.30 + 0.70 * nearness(z / radius)) * (0.10 + 0.90 * along * along) * energy));
        }
        light.ribbon(&screen, &colours, false, (r / 55.0).max(w * 1.5));
    }

    // The spindle: his axis, through the nucleus.
    let (top, _) = view.project(apply(&stance, [0.0, 1.20, 0.0]));
    let (bottom, _) = view.project(apply(&stance, [0.0, -1.20, 0.0]));
    light.seg(top, bottom, w, lit(accent, 0.22 * energy));

    // The nucleus.
    let pulse = 1.0 + 0.10 * look.voice + 0.03 * (clock * 2.4).sin();
    light.haze(c, (3.0 + 5.0 * look.voice + 1.5 * look.busy) * alpha * (r / 150.0).max(0.4));
    light.spark(c, r * 0.10 * pulse, lit(hot, 0.95 * alpha));
    light.arc(c, r * 0.085 * pulse, 0.0, TAU, (r / 60.0).max(w * 1.5), lit(hot, 0.95 * alpha));
    light.arc(c, r * 0.122, clock * 0.8, clock * 0.8 + 4.4, line, lit(accent, 0.8 * energy));
    light.dashes(c, r * 0.150, -clock * 0.5, -clock * 0.5 + TAU, 0.16, 0.10, line, lit(accent, 0.55 * energy));
    light.paint(p);

    if r >= 120.0 && !look.word.is_empty() {
        holo::text(p, pos2(c.x, c.y + r * 1.56), Align2::CENTER_CENTER, Face::Label, &format!("J.A.I. // {}", look.word), (r * 0.048).clamp(8.0, 13.0), a(t.ink(), 0.85 * look.alpha));
    }
}

/// How many wires and pins the mind is made of (the core view's readout).
pub fn census() -> (usize, usize) {
    (MIND.wires.len(), MIND.pins.len())
}

// ----------------------------------------------------------------------------- the view ----

pub const MIN_ZOOM: f32 = 0.5;
pub const MAX_ZOOM: f32 = 3.0;

/// What Sir may tell the core view, by voice or key.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CoreCommand {
    Open(bool),
    /// Absolute zoom (1 = 100 %).
    Zoom(f32),
    /// Relative zoom (× factor).
    ZoomBy(f32),
    /// How fast it turns by itself, × factor (0 holds it still).
    Spin(f32),
}

/// The core, large: turned by hand, zoomed, with what each layer does written beside it.
pub struct MindView {
    yaw: f32,
    pitch: f32,
    /// How fast the hand left it turning (radians a second), dying away.
    drift: Vec2,
    zoom: f32,
    zoom_target: f32,
    spin: f32,
}

impl Default for MindView {
    fn default() -> Self {
        Self { yaw: 0.0, pitch: 0.0, drift: Vec2::ZERO, zoom: 1.0, zoom_target: 1.0, spin: 1.0 }
    }
}

impl MindView {
    pub fn set_zoom(&mut self, zoom: f32) {
        self.zoom_target = zoom.clamp(MIN_ZOOM, MAX_ZOOM);
    }

    /// (yaw, pitch) it is turned by.
    pub fn orientation(&self) -> (f32, f32) {
        (self.yaw, self.pitch)
    }

    pub fn apply(&mut self, cmd: CoreCommand) {
        match cmd {
            CoreCommand::Open(_) => {}
            CoreCommand::Zoom(z) => self.set_zoom(z),
            CoreCommand::ZoomBy(f) => self.set_zoom(self.zoom_target * f),
            CoreCommand::Spin(f) => self.spin = (if self.spin <= 0.0 { 1.0 } else { self.spin } * f).clamp(0.0, 6.0),
        }
    }

    /// The hand moved by `d` points over `dt` seconds.
    pub fn drag(&mut self, d: Vec2, dt: f32) {
        let turn = vec2(d.x * 0.008, d.y * 0.008);
        self.yaw += turn.x;
        self.pitch = (self.pitch + turn.y).clamp(-1.2, 1.2);
        self.drift = turn / dt.max(1e-3);
    }

    /// Where a point of the sphere (in radii; +z towards the viewer) lands for a view at
    /// `centre` of `radius`, turned as it is now.
    pub fn project(&self, p: [f32; 3], centre: Pos2, radius: f32) -> (Pos2, f32) {
        let turn = mul(&rot_x(self.pitch), &mul(&stance(0.0), &rot_y(self.yaw)));
        View { centre, radius, focal: 3.4 }.project(apply(&turn, p))
    }

    /// One frame. `held`: the pointer is over it, and it stands still to be read.
    pub fn step(&mut self, dt: f32, held: bool) {
        self.zoom = crate::holo_core::follow(self.zoom, self.zoom_target, 9.0, dt);
        self.yaw += (self.drift.x + if held { 0.0 } else { 0.10 * self.spin }) * dt;
        self.pitch = (self.pitch + self.drift.y * dt).clamp(-1.2, 1.2);
        // (Let go, it turns on a little and comes to rest: a sixth of a turn at most.)
        self.drift = (self.drift * (-5.0 * dt).exp()).clamp(vec2(-5.0, -5.0), vec2(5.0, 5.0));
    }

    /// Whether it is still settling (the HUD keeps drawing meanwhile; while it spins it always is).
    pub fn moving(&self) -> bool {
        self.spin > 0.0 || self.drift.length() > 0.01 || (self.zoom - self.zoom_target).abs() > 0.002
    }

    /// Draws the view in `rect`. `active`: the parts working now, freshest first, as (name,
    /// layer, 1 just now … 0 fading); `layer_names`: the four layers, inside out.
    pub fn show(&mut self, ui: &mut egui::Ui, rect: Rect, look: &CoreLook, layer_names: &[&str; SHELLS], active: &[(&str, usize, f32)]) {
        let t = holo::theme();
        let dt = crate::holo_motion::dt(ui.ctx());
        let response = ui.interact(rect, ui.id().with("holo_mind_view"), egui::Sense::click_and_drag());
        if response.dragged() {
            self.drag(response.drag_delta(), dt);
        }
        if response.hovered() {
            let wheel = ui.input(|i| i.smooth_scroll_delta.y);
            if wheel != 0.0 {
                self.set_zoom(self.zoom_target * (wheel * 0.0015).exp());
            }
        }
        if response.double_clicked() {
            *self = Self { spin: self.spin, ..Self::default() };
        }
        self.step(dt, response.hovered() && !response.dragged());
        let p = ui.painter_at(rect);
        let centre = rect.center();
        let radius = rect.width().min(rect.height()) * 0.27 * self.zoom;
        paint_turned(&p, ui.id().with("holo_mind_view"), centre, radius, &CoreLook { word: "", ..*look }, self.yaw, self.pitch);

        // What it is, top left.
        let (wires, pins) = census();
        let at = rect.left_top() + vec2(4.0, 6.0);
        holo::text(&p, at, Align2::LEFT_TOP, Face::Label, "Core // mind", 12.0, a(t.ink(), look.alpha));
        holo::text(&p, at + vec2(0.0, 20.0), Align2::LEFT_TOP, Face::Mono, &format!("{wires} TRACES // {pins} PINS // {SHELLS} LAYERS"), 10.5, a(t.muted(), look.alpha));
        holo::text(&p, at + vec2(0.0, 36.0), Align2::LEFT_TOP, Face::Mono, "DRAG TO TURN // WHEEL TO ZOOM // DOUBLE CLICK TO RESET", 10.5, a(t.faint(), look.alpha));

        // The layers, inside out, each with how hard it works: bottom left.
        let mut y = rect.bottom() - 6.0 - SHELLS as f32 * 22.0;
        for (i, name) in layer_names.iter().enumerate() {
            let row = Rect::from_min_size(pos2(rect.left() + 4.0, y), vec2(260.0, 18.0));
            holo::text(&p, row.left_center(), Align2::LEFT_CENTER, Face::Mono, &format!("L{}", i + 1), 10.5, a(t.accent(), look.alpha));
            holo::text(&p, row.left_center() + vec2(26.0, 0.0), Align2::LEFT_CENTER, Face::Label, name, 8.0, a(t.ink(), 0.9 * look.alpha));
            crate::holo_kit::cells(&p, Rect::from_min_size(pos2(row.left() + 116.0, row.center().y - 4.0), vec2(140.0, 8.0)), look.layers[i], 20, t.accent(), look.alpha);
            y += 22.0;
        }

        // The parts at work, each on a leader from its layer: right.
        for (k, (name, layer, fresh)) in active.iter().take(9).enumerate() {
            let shell = SHELL[(*layer).min(SHELLS - 1)].radius * radius;
            let angle = -0.9 + k as f32 * 0.23;
            let from = pos2(centre.x + shell * angle.cos(), centre.y + shell * angle.sin());
            let to = pos2((centre.x + radius * 1.75).min(rect.right() - 150.0), centre.y - radius * 0.95 + k as f32 * 24.0);
            let label = name.trim_end_matches(".rs").replace('_', " ");
            crate::holo_kit::callout(&p, from, to, &label, (0.25 + 0.75 * fresh) * look.alpha);
        }

        // The zoom, bottom right: a ruler from 50 to 300 %.
        let ruler = Rect::from_min_size(pos2(rect.right() - 164.0, rect.bottom() - 20.0), vec2(160.0, 14.0));
        crate::holo_kit::slider(&p, ruler, (self.zoom - MIN_ZOOM) / (MAX_ZOOM - MIN_ZOOM), 0.0, look.alpha);
        holo::text(&p, ruler.left_top() + vec2(0.0, -6.0), Align2::LEFT_BOTTOM, Face::Mono, &format!("ZOOM {:.0} %", self.zoom_target * 100.0), 10.5, a(t.muted(), look.alpha));
        if self.moving() {
            ui.ctx().request_repaint();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_mind_is_the_same_every_time_and_every_wire_lies_on_its_shell() {
        let (first, again) = (build(0x4A41_4921), build(0x4A41_4921));
        assert_eq!(first.wires.len(), again.wires.len());
        assert!(first.wires.len() > 450, "dense enough to read as circuitry: {}", first.wires.len());
        for (x, y) in first.wires.iter().zip(&again.wires) {
            assert_eq!(x.points, y.points);
            assert!(x.points.len() >= 2 && x.shell < SHELLS);
            for q in &x.points {
                let len = (q[0] * q[0] + q[1] * q[1] + q[2] * q[2]).sqrt();
                assert!((len - 1.0).abs() < 1e-4, "{len}");
            }
        }
    }

    /// A wire runs along a latitude or up a meridian, never across: that is what makes it
    /// circuitry.
    #[test]
    fn wires_step_along_the_grid() {
        let mind = build(7);
        let mut stepped = 0;
        for wire in &mind.wires {
            for pair in wire.points.windows(2) {
                let same_lat = (pair[0][1] - pair[1][1]).abs() < 1e-4;
                let lon = |q: V3| q[2].atan2(q[0]);
                let turn = (lon(pair[0]) - lon(pair[1])).abs();
                let same_lon = turn < 1e-3 || (turn - TAU).abs() < 1e-3;
                assert!(same_lat || same_lon, "a diagonal step: {pair:?}");
                stepped += 1;
            }
        }
        assert!(stepped > 1000);
    }

    #[test]
    fn the_front_is_bright_and_the_back_falls_away() {
        assert!(nearness(1.0) > 0.99 && nearness(-1.0) < 0.01);
        assert!(nearness(0.3) > nearness(-0.3));
        let view = View { centre: pos2(400.0, 300.0), radius: 100.0, focal: 3.4 };
        let (front, _) = view.project([1.0, 0.0, 1.0]);
        let (back, _) = view.project([1.0, 0.0, -1.0]);
        assert!(front.x > back.x, "what is nearer is drawn larger");
    }
}
