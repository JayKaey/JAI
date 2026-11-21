//! The physics behind the Simulation Lab's moving scenes, kept apart from their painting
//! (`gui_simlab.rs`) so every law can be checked by a test: gravity between bodies (the solar
//! system with the planets' real orbits, the Earth and the Moon, a binary star, the three-body
//! figure eight, two planets, or a scene of bodies described in words), a ripple tank (the
//! two-dimensional wave equation with a wall of slits), a string's standing wave, the Doppler
//! effect and its Mach cone, a sound wave's compressions, light through a double slit, a
//! spring, the double pendulum and balls bouncing in a box.

use std::collections::VecDeque;
use std::f64::consts::{PI, TAU};

// ------------------------------------------------------------------------- gravity ----

/// One body of a gravity scene.
#[derive(Debug, Clone)]
pub struct Body {
    pub name: String,
    pub mass: f64,
    pub pos: [f64; 2],
    pub vel: [f64; 2],
    /// How large it is drawn (points), not its real size.
    pub size: f32,
}

/// A gravity scene: bodies, the gravitational constant in its units and the trails they leave.
#[derive(Debug, Clone)]
pub struct Bodies {
    pub bodies: Vec<Body>,
    /// The gravitational constant in the scene's units (4π² in AU, years and Sun masses).
    pub g: f64,
    /// The longest step one integration step may take (a fraction of the fastest orbit).
    pub max_step: f64,
    pub t: f64,
    /// Distance at which the pull stops growing, so two bodies passing through each other
    /// never fling each other out of the scene.
    pub softening: f64,
    /// Units of the scene's distance, time and mass ("AU", "years", "Sun masses").
    pub units: (&'static str, &'static str, &'static str),
    /// Distances are drawn by their square root (the outer planets would otherwise leave the
    /// inner ones a dot at the Sun).
    pub root_scale: bool,
    pub trails: Vec<VecDeque<[f64; 2]>>,
    trail_clock: f64,
}

/// Mean orbits of the planets (J2000): semi-major axis (AU), eccentricity, longitude of the
/// perihelion (°) and mass (Sun masses).
const PLANETS: &[(&str, f64, f64, f64, f64)] = &[
    ("Mercury", 0.387_098, 0.205_630, 77.46, 1.660e-7),
    ("Venus", 0.723_332, 0.006_772, 131.53, 2.448e-6),
    ("Earth", 1.000_001, 0.016_709, 102.94, 3.003e-6),
    ("Mars", 1.523_679, 0.093_400, 336.04, 3.227e-7),
    ("Jupiter", 5.204_4, 0.048_9, 14.75, 9.546e-4),
    ("Saturn", 9.582_6, 0.056_5, 92.43, 2.858e-4),
    ("Uranus", 19.218_4, 0.045_7, 170.96, 4.366e-5),
    ("Neptune", 30.07, 0.011_3, 44.97, 5.151e-5),
];

/// G in AU³ / (Sun mass · year²).
pub const G_SOLAR: f64 = 4.0 * PI * PI;

impl Bodies {
    fn new(bodies: Vec<Body>, g: f64, max_step: f64, units: (&'static str, &'static str, &'static str), root_scale: bool) -> Self {
        let n = bodies.len();
        let mut scene = Bodies { bodies, g, max_step, t: 0.0, softening: 0.0, units, root_scale, trails: vec![VecDeque::new(); n], trail_clock: 0.0 };
        scene.centre();
        scene
    }

    /// The scenes Sir can name: "solar system", "inner planets", "earth and moon", "binary
    /// star", "figure eight" (three bodies), "two planets".
    pub fn preset(name: &str) -> Option<Self> {
        let n = name.to_lowercase();
        let has = |w: &[&str]| w.iter().any(|x| n.contains(x));
        if has(&["inner planet", "inner solar", "terrestrial", "innere planeten"]) {
            return Some(Self::solar(4, false));
        }
        // (Before the solar system, whose "planets" they also say.)
        if has(&["two planets", "2 planets", "zwei planeten", "two bodies", "two-body", "2 bodies", "zwei körper", "zweikörper"]) {
            return Some(Self::two_planets());
        }
        if has(&["three body", "three-body", "3 body", "3-body", "three bodies", "figure eight", "figure-eight", "figure 8", "dreikörper", "drei körper"]) {
            // Chenciner and Montgomery's figure eight: three equal masses chasing each other
            // along one curve (G = 1, period 6.3259).
            let p = [0.970_004_36, -0.243_087_53];
            let v = [0.466_203_685, 0.432_365_73];
            let bodies = vec![
                Body { name: "A".into(), mass: 1.0, pos: p, vel: v, size: 8.0 },
                Body { name: "B".into(), mass: 1.0, pos: [-p[0], -p[1]], vel: v, size: 8.0 },
                Body { name: "C".into(), mass: 1.0, pos: [0.0, 0.0], vel: [-2.0 * v[0], -2.0 * v[1]], size: 8.0 },
            ];
            return Some(Self::new(bodies, 1.0, 0.001, ("units", "time units", "masses"), false));
        }
        if has(&["solar", "planets", "planet orbit", "sonnensystem", "planeten"]) {
            return Some(Self::solar(8, true));
        }
        if has(&["moon", "mond"]) {
            // Earth and Moon in AU, years and Sun masses: the Moon 384,400 km out, once round
            // in 27.3 days.
            let (me, mm, r) = (3.003e-6, 3.694e-8, 0.002_569_6);
            let v = (G_SOLAR * (me + mm) / r).sqrt();
            let bodies = vec![
                Body { name: "Earth".into(), mass: me, pos: [0.0, 0.0], vel: [0.0, 0.0], size: 11.0 },
                Body { name: "Moon".into(), mass: mm, pos: [r, 0.0], vel: [0.0, v], size: 5.0 },
            ];
            return Some(Self::new(bodies, G_SOLAR, 0.000_2, ("AU", "years", "Sun masses"), false));
        }
        if has(&["binary", "double star", "doppelstern"]) {
            // Two stars (1 and 0.6 Sun masses, 2 AU apart) and a planet round both at 7 AU.
            let (m1, m2, d) = (1.0, 0.6, 2.0);
            let v_rel = (G_SOLAR * (m1 + m2) / d).sqrt();
            let mp = 3.0e-6;
            let rp = 7.0;
            let vp = (G_SOLAR * (m1 + m2) / rp).sqrt();
            let bodies = vec![
                Body { name: "Star A".into(), mass: m1, pos: [-d * m2 / (m1 + m2), 0.0], vel: [0.0, -v_rel * m2 / (m1 + m2)], size: 12.0 },
                Body { name: "Star B".into(), mass: m2, pos: [d * m1 / (m1 + m2), 0.0], vel: [0.0, v_rel * m1 / (m1 + m2)], size: 10.0 },
                Body { name: "Planet".into(), mass: mp, pos: [0.0, rp], vel: [-vp, 0.0], size: 5.0 },
            ];
            return Some(Self::new(bodies, G_SOLAR, 0.002, ("AU", "years", "Sun masses"), false));
        }
        if has(&["gravity", "gravitation", "schwerkraft", "orbit", "umlaufbahn"]) {
            return Some(Self::two_planets());
        }
        None
    }

    /// A planet and a smaller one on an eccentric mutual orbit (G = 1): 85 % of the circular
    /// speed, so the pull is seen to change along the ellipse.
    fn two_planets() -> Self {
        let (m1, m2, r) = (1.0f64, 0.25f64, 2.0f64);
        let v_rel = 0.85 * ((m1 + m2) / r).sqrt();
        let bodies = vec![
            Body { name: "Planet 1".into(), mass: m1, pos: [-r * m2 / (m1 + m2), 0.0], vel: [0.0, -v_rel * m2 / (m1 + m2)], size: 12.0 },
            Body { name: "Planet 2".into(), mass: m2, pos: [r * m1 / (m1 + m2), 0.0], vel: [0.0, v_rel * m1 / (m1 + m2)], size: 8.0 },
        ];
        Self::new(bodies, 1.0, 0.002, ("units", "time units", "masses"), false)
    }

    /// The Sun and its first `planets` planets, each starting at its perihelion (vis-viva
    /// speed there), turned by its longitude of the perihelion.
    fn solar(planets: usize, root_scale: bool) -> Self {
        let mut bodies = vec![Body { name: "Sun".into(), mass: 1.0, pos: [0.0, 0.0], vel: [0.0, 0.0], size: 13.0 }];
        for &(name, a, e, perihelion, mass) in PLANETS.iter().take(planets) {
            let r = a * (1.0 - e);
            let v = (G_SOLAR * (1.0 + mass) * (1.0 + e) / (a * (1.0 - e))).sqrt();
            let angle = perihelion.to_radians();
            let (s, c) = angle.sin_cos();
            let size = match name {
                "Jupiter" => 8.0,
                "Saturn" => 7.5,
                "Uranus" | "Neptune" => 6.0,
                "Earth" | "Venus" => 5.5,
                _ => 4.5,
            };
            bodies.push(Body { name: name.into(), mass, pos: [r * c, r * s], vel: [-v * s, v * c], size });
        }
        // Mercury's 88 days in 250 steps.
        Self::new(bodies, G_SOLAR, 0.24 / 250.0, ("AU", "years", "Sun masses"), root_scale)
    }

    /// A scene described in words by the language model: `[{"name":"Sun","mass":1,"x":0,
    /// "y":0,"vx":0,"vy":0}, …]` with `g` (default 1). None when it is not such a list.
    pub fn from_json(spec: &str) -> Option<Self> {
        let value: serde_json::Value = serde_json::from_str(spec.trim()).ok()?;
        let (list, g) = match &value {
            serde_json::Value::Array(a) => (a.clone(), 1.0),
            serde_json::Value::Object(o) => (o.get("bodies")?.as_array()?.clone(), o.get("g").and_then(|g| g.as_f64()).unwrap_or(1.0)),
            _ => return None,
        };
        let num = |b: &serde_json::Value, k: &str| b.get(k).and_then(|v| v.as_f64()).filter(|v| v.is_finite());
        let bodies: Vec<Body> = list
            .iter()
            .take(24)
            .enumerate()
            .filter_map(|(i, b)| {
                let mass = num(b, "mass")?.max(0.0);
                Some(Body {
                    name: b.get("name").and_then(|n| n.as_str()).map(str::to_string).unwrap_or_else(|| format!("Body {}", i + 1)),
                    mass,
                    pos: [num(b, "x").unwrap_or(0.0), num(b, "y").unwrap_or(0.0)],
                    vel: [num(b, "vx").unwrap_or(0.0), num(b, "vy").unwrap_or(0.0)],
                    size: (4.0 + 3.0 * (mass.max(1e-12)).log10().clamp(-3.0, 1.0) as f32 + 9.0).clamp(4.0, 14.0),
                })
            })
            .collect();
        if bodies.len() < 2 || !g.is_finite() || g <= 0.0 {
            return None;
        }
        Some(Self::described(bodies, g, ("units", "time units", "masses"), 1e-3))
    }

    /// A scene of bodies put together from words or a list: the step is a small part of the
    /// quickest pair's orbit.
    fn described(bodies: Vec<Body>, g: f64, units: (&'static str, &'static str, &'static str), softening: f64) -> Self {
        let quickest = quickest_period(&bodies, g);
        let mut scene = Self::new(bodies, g, (quickest / 300.0).clamp(1e-6, 1.0), units, false);
        scene.softening = softening;
        scene
    }

    /// A scene named by its bodies: "the sun, the earth and a comet", "sun with jupiter and
    /// saturn" (the Sun and its planets from their real orbits, a Moon about the Earth, a comet
    /// on a long ellipse). None unless the Sun or a comet is named, or two planets are.
    pub fn from_words(text: &str) -> Option<Self> {
        let l = text.to_lowercase();
        let words: Vec<&str> = l.split(|c: char| !c.is_alphanumeric()).collect();
        let named = |names: &[&str]| words.iter().any(|w| names.contains(w));
        let sun = named(&["sun", "sonne"]);
        let comet = named(&["comet", "komet", "comets"]);
        let moon = named(&["moon", "mond"]);
        let german = |n: &str| match n {
            "Mercury" => "merkur",
            "Earth" => "erde",
            "Mars" => "mars",
            "Jupiter" => "jupiter",
            "Saturn" => "saturn",
            "Uranus" => "uranus",
            "Neptune" => "neptun",
            _ => "venus",
        };
        let planets: Vec<_> = PLANETS.iter().filter(|(n, ..)| named(&[&n.to_lowercase(), german(n)])).collect();
        if !(sun || comet || planets.len() >= 2) || planets.len() + usize::from(sun) + usize::from(comet) + usize::from(moon) < 2 {
            return None;
        }
        let mut bodies = vec![Body { name: "Sun".into(), mass: 1.0, pos: [0.0, 0.0], vel: [0.0, 0.0], size: 13.0 }];
        let orbit = |a: f64, e: f64, angle: f64, mass: f64| {
            let r = a * (1.0 - e);
            let v = (G_SOLAR * (1.0 + mass) * (1.0 + e) / r).sqrt();
            let (s, c) = angle.sin_cos();
            ([r * c, r * s], [-v * s, v * c])
        };
        for &&(name, a, e, perihelion, mass) in &planets {
            let (pos, vel) = orbit(a, e, perihelion.to_radians(), mass);
            let size = if matches!(name, "Jupiter" | "Saturn") { 8.0 } else { 5.5 };
            bodies.push(Body { name: name.into(), mass, pos, vel, size });
        }
        // A Moon only about a named Earth (384,400 km out, once round in 27.3 days).
        if let Some(earth) = bodies.iter().find(|b| b.name == "Earth").cloned().filter(|_| moon) {
            let mm = 3.694e-8;
            let r = 0.002_569_6;
            let v = (G_SOLAR * (earth.mass + mm) / r).sqrt();
            bodies.push(Body { name: "Moon".into(), mass: mm, pos: [earth.pos[0] + r, earth.pos[1]], vel: [earth.vel[0], earth.vel[1] + v], size: 4.0 });
        }
        if comet {
            // A long ellipse (e 0.9, a 3 AU, perihelion 0.3 AU), thrown the other way round.
            let (pos, vel) = orbit(3.0, 0.9, 200f64.to_radians(), 1e-12);
            bodies.push(Body { name: "Comet".into(), mass: 1e-12, pos, vel: [-vel[0], -vel[1]], size: 4.0 });
        }
        Some(Self::described(bodies, G_SOLAR, ("AU", "years", "Sun masses"), 1e-5))
    }

    /// The scene's centre of mass at rest at the origin (a lone heavy body stays put).
    fn centre(&mut self) {
        let total: f64 = self.bodies.iter().map(|b| b.mass).sum();
        if total <= 0.0 {
            return;
        }
        let mut c = [0.0; 2];
        let mut p = [0.0; 2];
        for b in &self.bodies {
            for k in 0..2 {
                c[k] += b.mass * b.pos[k];
                p[k] += b.mass * b.vel[k];
            }
        }
        for b in &mut self.bodies {
            for k in 0..2 {
                b.pos[k] -= c[k] / total;
                b.vel[k] -= p[k] / total;
            }
        }
    }

    fn accelerations(&self) -> Vec<[f64; 2]> {
        let n = self.bodies.len();
        let mut acc = vec![[0.0; 2]; n];
        let eps2 = self.softening * self.softening;
        for i in 0..n {
            for j in i + 1..n {
                let (a, b) = (&self.bodies[i], &self.bodies[j]);
                let d = [b.pos[0] - a.pos[0], b.pos[1] - a.pos[1]];
                let r2 = d[0] * d[0] + d[1] * d[1] + eps2;
                if r2 <= 0.0 {
                    continue;
                }
                let inv = self.g / (r2 * r2.sqrt());
                for k in 0..2 {
                    acc[i][k] += inv * b.mass * d[k];
                    acc[j][k] -= inv * a.mass * d[k];
                }
            }
        }
        acc
    }

    /// Moves the scene on by `dt` in steps no longer than `max_step` (leapfrog: kick, drift,
    /// kick, which keeps an orbit's energy for good), and drops a trail point now and then.
    pub fn advance(&mut self, dt: f64) {
        if !(dt > 0.0) {
            return;
        }
        let steps = ((dt / self.max_step).ceil() as usize).clamp(1, 20_000);
        let h = dt / steps as f64;
        let mut acc = self.accelerations();
        for _ in 0..steps {
            for (b, a) in self.bodies.iter_mut().zip(&acc) {
                b.vel[0] += 0.5 * h * a[0];
                b.vel[1] += 0.5 * h * a[1];
                b.pos[0] += h * b.vel[0];
                b.pos[1] += h * b.vel[1];
            }
            acc = self.accelerations();
            for (b, a) in self.bodies.iter_mut().zip(&acc) {
                b.vel[0] += 0.5 * h * a[0];
                b.vel[1] += 0.5 * h * a[1];
            }
        }
        self.t += dt;
        // About 240 points round the slowest orbit shown, whatever the speed.
        self.trail_clock += dt;
        let every = self.max_step * 4.0;
        if self.trail_clock >= every {
            self.trail_clock = 0.0;
            for (trail, b) in self.trails.iter_mut().zip(&self.bodies) {
                trail.push_back(b.pos);
                if trail.len() > 900 {
                    trail.pop_front();
                }
            }
        }
    }

    /// Kinetic plus potential energy (constant while nothing is lost).
    pub fn energy(&self) -> f64 {
        let kinetic: f64 = self.bodies.iter().map(|b| 0.5 * b.mass * (b.vel[0].powi(2) + b.vel[1].powi(2))).sum();
        let mut potential = 0.0;
        for (i, a) in self.bodies.iter().enumerate() {
            for b in &self.bodies[i + 1..] {
                potential -= self.g * a.mass * b.mass / (dist(a.pos, b.pos).powi(2) + self.softening.powi(2)).sqrt();
            }
        }
        kinetic + potential
    }

    /// The largest distance of a body from the centre (what the view must hold).
    pub fn extent(&self) -> f64 {
        self.bodies.iter().map(|b| dist(b.pos, [0.0, 0.0])).fold(0.0, f64::max)
    }
}

fn dist(a: [f64; 2], b: [f64; 2]) -> f64 {
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2)).sqrt()
}

/// The shortest orbit of any pair of bodies, as they stand (Kepler's third law with their
/// summed mass): the clock a scene's step and its playback speed are set by.
pub fn quickest_period(bodies: &[Body], g: f64) -> f64 {
    bodies
        .iter()
        .enumerate()
        .flat_map(|(i, a)| bodies.iter().skip(i + 1).map(move |b| (a, b)))
        .map(|(a, b)| kepler_period(g, (a.mass + b.mass).max(1e-12), dist(a.pos, b.pos).max(1e-6)))
        .fold(f64::MAX, f64::min)
}

/// Kepler's third law: a body's period (in the scene's time) at semi-major axis `a` round
/// mass `m`.
pub fn kepler_period(g: f64, m: f64, a: f64) -> f64 {
    TAU * (a.powi(3) / (g * m)).sqrt()
}

// -------------------------------------------------------------------- ripple tank ----

/// A ripple tank: the two-dimensional wave equation on a grid (leapfrog in time, the
/// five-point Laplacian in space), its edges soaking the waves up, a source on the left (one
/// point, or a straight wave front), and a wall with 0, 1 or 2 slits a third of the way across.
pub struct Ripples {
    pub w: usize,
    pub h: usize,
    pub now: Vec<f32>,
    before: Vec<f32>,
    /// Cells of the wall (held at rest).
    pub wall: Vec<bool>,
    /// Damping per step (1 inside, less towards the edges).
    damp: Vec<f32>,
    pub t: f64,
    /// Cycles per time unit of the source; waves travel one cell per time unit.
    pub frequency: f64,
    pub plane: bool,
    pub wall_x: usize,
}

/// Courant number of the ripple tank (stable below 1/√2 in two dimensions).
pub const RIPPLE_COURANT: f32 = 0.5;
/// Time per step at that Courant number (cells and time units: speed 1).
pub const RIPPLE_DT: f64 = 0.5;

impl Ripples {
    /// `slits`: 0 (no wall), 1 or 2; widths and separation in cells.
    pub fn new(w: usize, h: usize, frequency: f64, slits: usize, slit_width: usize, separation: usize, plane: bool) -> Self {
        let (w, h) = (w.max(16), h.max(16));
        let wall_x = w / 3;
        let mut wall = vec![false; w * h];
        if slits > 0 {
            let centre = h / 2;
            let gaps: Vec<usize> = if slits == 1 { vec![centre] } else { vec![centre.saturating_sub(separation / 2), (centre + separation / 2).min(h - 1)] };
            // Each slit is `width` cells: from g − width/2 up to (not with) g + (width+1)/2.
            let width = slit_width.max(1);
            let open = |y: usize| gaps.iter().any(|&g| y + width / 2 >= g && y < g + (width + 1) / 2);
            for y in 0..h {
                if !open(y) {
                    for x in wall_x..(wall_x + 2).min(w) {
                        wall[y * w + x] = true;
                    }
                }
            }
        }
        // A sponge 14 cells deep along every edge: the waves leave instead of coming back.
        let sponge = 14.0f32;
        let mut damp = vec![1.0f32; w * h];
        for y in 0..h {
            for x in 0..w {
                let edge = (x.min(w - 1 - x).min(y).min(h - 1 - y)) as f32;
                if edge < sponge {
                    let k = (sponge - edge) / sponge;
                    damp[y * w + x] = 1.0 - 0.12 * k * k;
                }
            }
        }
        Ripples { w, h, now: vec![0.0; w * h], before: vec![0.0; w * h], wall, damp, t: 0.0, frequency, plane, wall_x }
    }

    /// One step of the wave equation.
    pub fn step(&mut self) {
        let (w, h) = (self.w, self.h);
        let c2 = RIPPLE_COURANT * RIPPLE_COURANT;
        let mut next = vec![0.0f32; w * h];
        for y in 1..h - 1 {
            for x in 1..w - 1 {
                let i = y * w + x;
                if self.wall[i] {
                    continue;
                }
                let lap = self.now[i - 1] + self.now[i + 1] + self.now[i - w] + self.now[i + w] - 4.0 * self.now[i];
                next[i] = (2.0 * self.now[i] - self.before[i] + c2 * lap) * self.damp[i];
            }
        }
        self.t += RIPPLE_DT;
        // The source drives its cells.
        let drive = (TAU * self.frequency * self.t).sin() as f32;
        if self.plane {
            let x = 16.min(w - 2);
            for y in 1..h - 1 {
                next[y * w + x] = drive;
            }
        } else {
            let (x, y) = (16.min(w - 2), h / 2);
            next[y * w + x] = 2.0 * drive;
        }
        self.before = std::mem::replace(&mut self.now, next);
    }

    pub fn at(&self, x: usize, y: usize) -> f32 {
        self.now[y * self.w + x]
    }

    /// The wavelength in cells (speed 1 / frequency).
    pub fn wavelength(&self) -> f64 {
        1.0 / self.frequency.max(1e-6)
    }
}

// ------------------------------------------------------------------ standing wave ----

/// A string fixed at both ends in its `n`th harmonic: y(x, t) = A sin(nπx/L) cos(ωt), the sum
/// of two waves of half the amplitude running against each other.
pub fn standing(n: u32, length: f64, speed: f64, amplitude: f64, x: f64, t: f64) -> f64 {
    let k = n as f64 * PI / length;
    amplitude * (k * x).sin() * (k * speed * t).cos()
}

/// The two travelling halves of that standing wave at `x` (to the right, to the left).
pub fn travelling(n: u32, length: f64, speed: f64, amplitude: f64, x: f64, t: f64) -> (f64, f64) {
    let k = n as f64 * PI / length;
    let w = k * speed;
    (0.5 * amplitude * (k * x - w * t).sin(), 0.5 * amplitude * (k * x + w * t).sin())
}

/// The `n`th harmonic's frequency f = n v / (2L).
pub fn harmonic_frequency(n: u32, length: f64, speed: f64) -> f64 {
    n as f64 * speed / (2.0 * length)
}

/// Where the nodes of the `n`th harmonic are (x = kL/n).
pub fn nodes(n: u32, length: f64) -> Vec<f64> {
    (0..=n).map(|k| k as f64 * length / n.max(1) as f64).collect()
}

// ------------------------------------------------------------------------ Doppler ----

/// The frequencies heard ahead of a source and behind it, moving at `v` through a medium in
/// which the waves travel at `c` (None ahead once it outruns its own waves).
pub fn doppler(f: f64, v: f64, c: f64) -> (Option<f64>, f64) {
    let ahead = (v < c).then(|| f * c / (c - v));
    (ahead, f * c / (c + v))
}

/// The Mach cone's half angle (radians) of a source faster than its waves.
pub fn mach_angle(v: f64, c: f64) -> Option<f64> {
    (v > c && c > 0.0).then(|| (c / v).asin())
}

/// The wave fronts a source left behind until `t`: (centre x, radius) of each, newest last.
/// The source starts at x0 and moves at `v`; one front every 1/f.
pub fn wave_fronts(x0: f64, v: f64, c: f64, f: f64, t: f64, keep: usize) -> Vec<(f64, f64)> {
    let period = 1.0 / f.max(1e-6);
    let emitted = (t / period).floor() as i64;
    let first = (emitted - keep as i64 + 1).max(0);
    (first..=emitted).map(|k| {
        let te = k as f64 * period;
        (x0 + v * te, c * (t - te))
    }).collect()
}

// ------------------------------------------------------------------------- sound ----

/// A sound wave's particle displacement s(x, t) = s0 sin(kx − ωt) and the pressure it makes,
/// −dS/dx (positive where the air is pressed together).
pub fn sound(frequency: f64, speed: f64, s0: f64, x: f64, t: f64) -> (f64, f64) {
    let k = TAU * frequency / speed;
    let w = TAU * frequency;
    let phase = k * x - w * t;
    (s0 * phase.sin(), -s0 * k * phase.cos())
}

// ------------------------------------------------------------------------- light ----

/// The light a double slit throws on a screen at height `y` (m): cos²(π d y / λL) ·
/// sinc²(π a y / λL), 1 in the middle. λ in nm, slit separation `d` and width `a` in µm,
/// screen distance `l` in m.
pub fn double_slit(lambda_nm: f64, d_um: f64, a_um: f64, l: f64, y: f64) -> f64 {
    let lambda = lambda_nm * 1e-9;
    let (d, a) = (d_um * 1e-6, a_um * 1e-6);
    let s = y / (y * y + l * l).sqrt();
    let beta = PI * a * s / lambda;
    let alpha = PI * d * s / lambda;
    let sinc = if beta.abs() < 1e-9 { 1.0 } else { beta.sin() / beta };
    alpha.cos().powi(2) * sinc * sinc
}

/// The distance between bright fringes, λL/d (m).
pub fn fringe_spacing(lambda_nm: f64, d_um: f64, l: f64) -> f64 {
    lambda_nm * 1e-9 * l / (d_um * 1e-6)
}

/// What colour light of this wavelength is (named, not drawn: the HUD shows light in its own).
pub fn colour_name(lambda_nm: f64) -> &'static str {
    match lambda_nm {
        l if l < 380.0 => "ultraviolet",
        l if l < 450.0 => "violet",
        l if l < 495.0 => "blue",
        l if l < 570.0 => "green",
        l if l < 590.0 => "yellow",
        l if l < 620.0 => "orange",
        l if l <= 750.0 => "red",
        _ => "infrared",
    }
}

// ------------------------------------------------------------------------ spring ----

/// A mass on a spring: m x″ = −k x − c x′ + F cos(ω_d t). State (x, v).
#[derive(Debug, Clone, Copy)]
pub struct Spring {
    pub mass: f64,
    pub k: f64,
    pub c: f64,
    pub force: f64,
    pub drive: f64,
    pub x: f64,
    pub v: f64,
    pub t: f64,
}

impl Spring {
    fn accel(&self, t: f64, x: f64, v: f64) -> f64 {
        (-self.k * x - self.c * v + self.force * (self.drive * t).cos()) / self.mass.max(1e-9)
    }

    /// RK4 in steps of at most a millisecond.
    pub fn advance(&mut self, dt: f64) {
        let steps = ((dt / 0.001).ceil() as usize).clamp(1, 10_000);
        let h = dt / steps as f64;
        for _ in 0..steps {
            let (t, x, v) = (self.t, self.x, self.v);
            let (k1x, k1v) = (v, self.accel(t, x, v));
            let (k2x, k2v) = (v + 0.5 * h * k1v, self.accel(t + 0.5 * h, x + 0.5 * h * k1x, v + 0.5 * h * k1v));
            let (k3x, k3v) = (v + 0.5 * h * k2v, self.accel(t + 0.5 * h, x + 0.5 * h * k2x, v + 0.5 * h * k2v));
            let (k4x, k4v) = (v + h * k3v, self.accel(t + h, x + h * k3x, v + h * k3v));
            self.x += h / 6.0 * (k1x + 2.0 * k2x + 2.0 * k3x + k4x);
            self.v += h / 6.0 * (k1v + 2.0 * k2v + 2.0 * k3v + k4v);
            self.t += h;
        }
    }

    /// ω₀ = √(k/m).
    pub fn natural(&self) -> f64 {
        (self.k / self.mass.max(1e-9)).sqrt()
    }

    /// ζ = c / (2√(km)): under 1 it swings, 1 is critical, over 1 it creeps back.
    pub fn damping_ratio(&self) -> f64 {
        self.c / (2.0 * (self.k * self.mass).max(1e-12).sqrt())
    }

    pub fn energy(&self) -> f64 {
        0.5 * self.mass * self.v * self.v + 0.5 * self.k * self.x * self.x
    }
}

// -------------------------------------------------------------- double pendulum ----

/// Two pendulums hung one from the other (angles from straight down, radians).
#[derive(Debug, Clone, Copy)]
pub struct DoublePendulum {
    pub l1: f64,
    pub l2: f64,
    pub m1: f64,
    pub m2: f64,
    pub g: f64,
    pub a1: f64,
    pub a2: f64,
    pub w1: f64,
    pub w2: f64,
}

impl DoublePendulum {
    /// The angular accelerations (the equations of motion from its Lagrangian).
    fn accel(&self, a1: f64, a2: f64, w1: f64, w2: f64) -> (f64, f64) {
        let (l1, l2, m1, m2, g) = (self.l1, self.l2, self.m1, self.m2, self.g);
        let d = a1 - a2;
        let den = 2.0 * m1 + m2 - m2 * (2.0 * d).cos();
        let n1 = -g * (2.0 * m1 + m2) * a1.sin() - m2 * g * (a1 - 2.0 * a2).sin() - 2.0 * d.sin() * m2 * (w2 * w2 * l2 + w1 * w1 * l1 * d.cos());
        let n2 = 2.0 * d.sin() * (w1 * w1 * l1 * (m1 + m2) + g * (m1 + m2) * a1.cos() + w2 * w2 * l2 * m2 * d.cos());
        (n1 / (l1 * den), n2 / (l2 * den))
    }

    /// RK4 in steps of at most half a millisecond (it is chaotic: small steps keep its energy).
    pub fn advance(&mut self, dt: f64) {
        let steps = ((dt / 0.0005).ceil() as usize).clamp(1, 20_000);
        let h = dt / steps as f64;
        for _ in 0..steps {
            let s = [self.a1, self.a2, self.w1, self.w2];
            let f = |s: [f64; 4]| {
                let (b1, b2) = self.accel(s[0], s[1], s[2], s[3]);
                [s[2], s[3], b1, b2]
            };
            let add = |s: [f64; 4], k: [f64; 4], h: f64| [s[0] + h * k[0], s[1] + h * k[1], s[2] + h * k[2], s[3] + h * k[3]];
            let k1 = f(s);
            let k2 = f(add(s, k1, 0.5 * h));
            let k3 = f(add(s, k2, 0.5 * h));
            let k4 = f(add(s, k3, h));
            for i in 0..4 {
                let v = s[i] + h / 6.0 * (k1[i] + 2.0 * k2[i] + 2.0 * k3[i] + k4[i]);
                match i {
                    0 => self.a1 = v,
                    1 => self.a2 = v,
                    2 => self.w1 = v,
                    _ => self.w2 = v,
                }
            }
        }
    }

    /// The bobs' positions, y down from the pivot.
    pub fn bobs(&self) -> ([f64; 2], [f64; 2]) {
        let p1 = [self.l1 * self.a1.sin(), self.l1 * self.a1.cos()];
        (p1, [p1[0] + self.l2 * self.a2.sin(), p1[1] + self.l2 * self.a2.cos()])
    }

    pub fn energy(&self) -> f64 {
        let (l1, l2, m1, m2, g) = (self.l1, self.l2, self.m1, self.m2, self.g);
        let v1 = (l1 * self.w1).powi(2);
        let v2 = v1 + (l2 * self.w2).powi(2) + 2.0 * l1 * l2 * self.w1 * self.w2 * (self.a1 - self.a2).cos();
        let kinetic = 0.5 * m1 * v1 + 0.5 * m2 * v2;
        let (p1, p2) = self.bobs();
        // (y is down: the potential is −m g y.)
        kinetic - m1 * g * p1[1] - m2 * g * p2[1]
    }
}

// ------------------------------------------------------------------------- balls ----

/// A ball in the box.
#[derive(Debug, Clone, Copy)]
pub struct Ball {
    pub pos: [f64; 2],
    pub vel: [f64; 2],
    pub r: f64,
}

/// Balls bouncing in a box (`width` × `height`, y up from the floor) under gravity, off the
/// walls and off each other, keeping `elasticity` of their speed along each impact.
pub struct Balls {
    pub balls: Vec<Ball>,
    pub width: f64,
    pub height: f64,
    pub g: f64,
    pub elasticity: f64,
}

impl Balls {
    pub fn new(count: usize, width: f64, height: f64, g: f64, elasticity: f64, radius: f64, seed: u32) -> Self {
        let mut s = seed | 1;
        let mut rnd = || {
            s ^= s << 13;
            s ^= s >> 17;
            s ^= s << 5;
            (s % 10_000) as f64 / 10_000.0
        };
        let mut balls: Vec<Ball> = Vec::new();
        for _ in 0..count.clamp(1, 60) {
            let r = radius * (0.7 + 0.6 * rnd());
            // A place that overlaps no other ball (tried a few times).
            let mut pos = [0.0; 2];
            for _ in 0..40 {
                pos = [r + rnd() * (width - 2.0 * r), height * 0.3 + rnd() * (height * 0.7 - r)];
                if balls.iter().all(|b| dist(b.pos, pos) > b.r + r) {
                    break;
                }
            }
            balls.push(Ball { pos, vel: [(rnd() - 0.5) * 6.0, (rnd() - 0.5) * 4.0], r });
        }
        Balls { balls, width, height, g, elasticity: elasticity.clamp(0.0, 1.0) }
    }

    /// Moves on by `dt` in small steps.
    pub fn advance(&mut self, dt: f64) {
        let steps = ((dt / 0.002).ceil() as usize).clamp(1, 5_000);
        let h = dt / steps as f64;
        for _ in 0..steps {
            for b in &mut self.balls {
                b.vel[1] -= self.g * h;
                b.pos[0] += b.vel[0] * h;
                b.pos[1] += b.vel[1] * h;
                // Off a wall as off a mirror: the way it went past the wall it goes back (a
                // ball only set back onto the floor would gain the height it sank each time).
                let e = self.elasticity;
                for (k, high) in [(0, self.width), (1, self.height)] {
                    if b.pos[k] < b.r {
                        b.pos[k] = (b.r + (b.r - b.pos[k]) * e).min(high - b.r);
                        b.vel[k] = b.vel[k].abs() * e;
                    } else if b.pos[k] > high - b.r {
                        b.pos[k] = (high - b.r - (b.pos[k] - (high - b.r)) * e).max(b.r);
                        b.vel[k] = -b.vel[k].abs() * e;
                    }
                }
            }
            let n = self.balls.len();
            for i in 0..n {
                for j in i + 1..n {
                    let (a, b) = (self.balls[i], self.balls[j]);
                    let d = [b.pos[0] - a.pos[0], b.pos[1] - a.pos[1]];
                    let r = (d[0] * d[0] + d[1] * d[1]).sqrt();
                    if r >= a.r + b.r || r < 1e-9 {
                        continue;
                    }
                    let nrm = [d[0] / r, d[1] / r];
                    let (ma, mb) = (a.r * a.r, b.r * b.r);
                    let closing = (a.vel[0] - b.vel[0]) * nrm[0] + (a.vel[1] - b.vel[1]) * nrm[1];
                    // Pushed apart where they overlap (by their masses' share).
                    let overlap = a.r + b.r - r;
                    let (sa, sb) = (mb / (ma + mb), ma / (ma + mb));
                    for k in 0..2 {
                        self.balls[i].pos[k] -= nrm[k] * overlap * sa;
                        self.balls[j].pos[k] += nrm[k] * overlap * sb;
                    }
                    if closing > 0.0 {
                        let impulse = (1.0 + self.elasticity) * closing / (1.0 / ma + 1.0 / mb);
                        for k in 0..2 {
                            self.balls[i].vel[k] -= impulse / ma * nrm[k];
                            self.balls[j].vel[k] += impulse / mb * nrm[k];
                        }
                    }
                }
            }
        }
    }

    /// Kinetic plus potential energy (each ball's mass taken as r²).
    pub fn energy(&self) -> f64 {
        self.balls.iter().map(|b| b.r * b.r * (0.5 * (b.vel[0].powi(2) + b.vel[1].powi(2)) + self.g * b.pos[1])).sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The Earth goes round the Sun once a year, Jupiter in 11.86 years, and the solar
    /// system keeps its energy to a part in a million over a Jupiter year.
    #[test]
    fn the_planets_keep_their_real_years() {
        let scene = Bodies::preset("solar system").unwrap();
        assert_eq!(scene.bodies.len(), 9);
        assert!(scene.root_scale);
        let earth = &scene.bodies[3];
        assert_eq!(earth.name, "Earth");
        assert!((kepler_period(G_SOLAR, 1.0, 1.0) - 1.0).abs() < 1e-12);
        // One year on, the Earth is back where it started (within a thousandth of an AU).
        let mut s = Bodies::preset("inner planets").unwrap();
        let start = s.bodies[3].pos;
        s.advance(1.0);
        assert!(dist(s.bodies[3].pos, start) < 0.01, "after a year the Earth is {} AU from its start", dist(s.bodies[3].pos, start));
        // Half a year on, it is on the far side of the Sun.
        let mut s = Bodies::preset("inner planets").unwrap();
        s.advance(0.5);
        assert!(dist(s.bodies[3].pos, start) > 1.9);
        // Energy over a Jupiter year.
        let mut s = Bodies::preset("solar system").unwrap();
        let e0 = s.energy();
        for _ in 0..119 {
            s.advance(0.1);
        }
        assert!(((s.energy() - e0) / e0).abs() < 1e-5, "energy drifted by {}", (s.energy() - e0) / e0);
        // Every planet is still bound to the Sun, at about its own distance.
        for (body, &(name, a, e, ..)) in s.bodies[1..].iter().zip(PLANETS) {
            let r = dist(body.pos, s.bodies[0].pos);
            assert!(r > a * (1.0 - e) * 0.97 && r < a * (1.0 + e) * 1.03, "{name} at {r} AU");
        }
    }

    /// The figure eight comes back to its start after its period.
    #[test]
    fn the_figure_eight_closes() {
        let mut s = Bodies::preset("three body problem").unwrap();
        let start: Vec<[f64; 2]> = s.bodies.iter().map(|b| b.pos).collect();
        s.advance(6.325_9);
        for (b, p) in s.bodies.iter().zip(&start) {
            assert!(dist(b.pos, *p) < 0.01, "{} is {} from its start", b.name, dist(b.pos, *p));
        }
    }

    #[test]
    fn the_moon_takes_its_month_and_scenes_come_in_words() {
        let mut s = Bodies::preset("the earth and the moon").unwrap();
        let start = s.bodies[1].pos;
        s.advance(27.32 / 365.25);
        assert!(dist(s.bodies[1].pos, start) < 0.000_1);
        for name in ["binary star", "two planets", "gravity between two planets"] {
            let mut s = Bodies::preset(name).unwrap();
            let e0 = s.energy();
            s.advance(5.0);
            assert!(((s.energy() - e0) / e0).abs() < 1e-3, "{name}");
        }
        let scene = Bodies::from_json(r#"{"g": 1, "bodies": [{"name": "Sun", "mass": 10, "x": 0, "y": 0}, {"name": "Rock", "mass": 0.01, "x": 2, "y": 0, "vx": 0, "vy": 2.2}]}"#).unwrap();
        assert_eq!(scene.bodies.len(), 2);
        assert_eq!(scene.bodies[1].name, "Rock");
        assert!(Bodies::from_json("not json").is_none());
        assert!(Bodies::from_json(r#"[{"mass": 1}]"#).is_none(), "one body is no scene");
        assert!(Bodies::preset("a cat").is_none());
    }

    #[test]
    fn a_scene_named_by_its_bodies_is_built_from_their_orbits() {
        let mut s = Bodies::from_words("simulate the sun, the earth and a comet").unwrap();
        let names: Vec<&str> = s.bodies.iter().map(|b| b.name.as_str()).collect();
        assert_eq!(names, ["Sun", "Earth", "Comet"]);
        let e0 = s.energy();
        s.advance(1.0);
        assert!(((s.energy() - e0) / e0).abs() < 1e-3, "drift {}", (s.energy() - e0) / e0);
        let s = Bodies::from_words("die Sonne, die Erde und der Mond").unwrap();
        assert_eq!(s.bodies.len(), 3);
        assert!(Bodies::from_words("the earth and the moon").is_none(), "that is the preset");
        assert!(Bodies::from_words("a cat and a dog").is_none());
    }

    /// Waves leave the source at one cell per time unit, pass the slits and do not come back
    /// from the edges.
    #[test]
    fn ripples_travel_at_their_speed_and_stay_calm() {
        let mut r = Ripples::new(160, 100, 0.05, 2, 6, 24, false);
        assert!((r.wavelength() - 20.0).abs() < 1e-9);
        for _ in 0..80 {
            r.step();
        }
        // 40 time units: the front is about 40 cells from the source; beyond it the water is
        // still (a twentieth of the waves behind it at most).
        let (sx, sy) = (16, 50);
        let band = |lo: usize, hi: usize| (lo..hi).map(|x| r.at(sx + x, sy).abs()).fold(0.0f32, f32::max);
        let (near, far) = (band(20, 35), band(48, 60));
        assert!(near > 0.01, "the wave has come 20 to 35 cells ({near})");
        assert!(far < near * 0.05, "but not 48 ({far} against {near})");
        // Two slits of six cells: six open cells each in the wall's column.
        let open = (0..r.h).filter(|&y| !r.wall[y * r.w + r.wall_x]).count();
        assert_eq!(open, 12);
        for _ in 0..2000 {
            r.step();
        }
        let peak = r.now.iter().fold(0.0f32, |m, v| m.max(v.abs()));
        assert!(peak.is_finite() && peak < 10.0, "the tank stays stable ({peak})");
        // Beyond the wall the two slits' waves arrive.
        assert!((r.wall_x + 5..r.w - 20).any(|x| r.at(x, 50).abs() > 0.01));
    }

    #[test]
    fn a_standing_wave_rests_at_its_nodes() {
        let (n, l, v, a) = (3, 2.0, 4.0, 1.0);
        for x in nodes(n, l) {
            for t in [0.0, 0.1, 0.37] {
                assert!(standing(n, l, v, a, x, t).abs() < 1e-9);
            }
        }
        assert!((harmonic_frequency(n, l, v) - 3.0).abs() < 1e-12);
        // The two travelling halves add up to it.
        let (r, left) = travelling(n, l, v, a, 0.7, 0.21);
        assert!((r + left - standing(n, l, v, a, 0.7, 0.21)).abs() < 1e-12);
    }

    #[test]
    fn the_doppler_shift_and_the_mach_cone() {
        let (ahead, behind) = doppler(440.0, 34.3, 343.0);
        assert!((ahead.unwrap() - 488.888).abs() < 0.01);
        assert!((behind - 400.0).abs() < 0.01);
        assert_eq!(doppler(440.0, 400.0, 343.0).0, None);
        assert!((mach_angle(686.0, 343.0).unwrap() - PI / 6.0).abs() < 1e-9);
        assert_eq!(mach_angle(100.0, 343.0), None);
        let fronts = wave_fronts(0.0, 1.0, 2.0, 1.0, 3.5, 10);
        assert_eq!(fronts.len(), 4);
        assert_eq!(fronts[3], (3.0, 1.0));
    }

    #[test]
    fn sound_and_light_follow_their_laws() {
        // Pressure is highest where the displacement crosses zero going down.
        let (s, p) = sound(343.0, 343.0, 1.0, 0.0, 0.0);
        assert!(s.abs() < 1e-12 && p < 0.0);
        assert!((double_slit(500.0, 50.0, 10.0, 1.0, 0.0) - 1.0).abs() < 1e-12);
        let dy = fringe_spacing(500.0, 50.0, 1.0);
        assert!((dy - 0.01).abs() < 1e-12);
        // Dark half-way between bright fringes, bright again at the next.
        assert!(double_slit(500.0, 50.0, 10.0, 1.0, dy / 2.0) < 0.01);
        assert!(double_slit(500.0, 50.0, 10.0, 1.0, dy) > 0.8);
        assert_eq!(colour_name(532.0), "green");
        assert_eq!(colour_name(650.0), "red");
    }

    #[test]
    fn a_spring_swings_at_its_own_frequency() {
        let mut s = Spring { mass: 2.0, k: 8.0, c: 0.0, force: 0.0, drive: 0.0, x: 1.0, v: 0.0, t: 0.0 };
        assert!((s.natural() - 2.0).abs() < 1e-12);
        let e0 = s.energy();
        s.advance(PI); // one period: 2π/ω₀
        assert!((s.x - 1.0).abs() < 1e-6 && s.v.abs() < 1e-5);
        assert!((s.energy() - e0).abs() < 1e-6);
        let damped = Spring { c: 8.0, ..s };
        assert!((damped.damping_ratio() - 1.0).abs() < 1e-12, "critical damping");
    }

    #[test]
    fn the_double_pendulum_keeps_its_energy() {
        let mut p = DoublePendulum { l1: 1.0, l2: 1.0, m1: 1.0, m2: 1.0, g: 9.81, a1: 2.0, a2: 2.5, w1: 0.0, w2: 0.0 };
        let e0 = p.energy();
        for _ in 0..100 {
            p.advance(0.1);
        }
        assert!(((p.energy() - e0) / e0.abs()).abs() < 1e-4, "energy drifted to {}", p.energy());
        // Hanging straight down it stays there.
        let mut rest = DoublePendulum { a1: 0.0, a2: 0.0, ..p };
        rest.w1 = 0.0;
        rest.w2 = 0.0;
        rest.advance(1.0);
        assert!(rest.a1.abs() < 1e-12 && rest.a2.abs() < 1e-12);
    }

    #[test]
    fn balls_stay_in_the_box_and_lose_energy_only_when_told() {
        let mut b = Balls::new(12, 10.0, 6.0, 9.81, 1.0, 0.35, 7);
        let e0 = b.energy();
        b.advance(5.0);
        for ball in &b.balls {
            assert!(ball.pos[0] >= ball.r - 1e-9 && ball.pos[0] <= 10.0 - ball.r + 1e-9);
            assert!(ball.pos[1] >= ball.r - 1e-9 && ball.pos[1] <= 6.0 - ball.r + 1e-9);
        }
        assert!(((b.energy() - e0) / e0).abs() < 0.05, "elastic: energy kept ({} → {})", e0, b.energy());
        let mut soft = Balls::new(12, 10.0, 6.0, 9.81, 0.5, 0.35, 7);
        let e0 = soft.energy();
        soft.advance(5.0);
        assert!(soft.energy() < e0 * 0.6, "inelastic bounces lose energy");
    }
}
