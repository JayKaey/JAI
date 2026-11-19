//! J.A.I.'s core, drawn two ways from the same inputs. SIMPLE is an instrument of rings; COMPLEX
//! (`holo_mind`) is the sphere of circuitry. Every ring reads something: the inner ring is his
//! own voice, the band is how hard he works, the scale is the clock (a tick a second, an arc
//! for the minutes, a block for the hour), the orbits turn with thought and the outer ticks are
//! Sir's voice. His voice is the accent, Sir's is white.
//!
//! Both are light: thin white-hot filaments whose glow is a real bloom (`holo::Bloom`), falling
//! off through the accent into the deep tone; bands are glass lit from their edge.

use crate::holo::{self, a, lit, mix, polar, px, Face, Light, NORTH};
use eframe::egui::{self, pos2, vec2, Align2, Color32, Painter, Pos2, Rect, Vec2};
use std::f32::consts::{PI, TAU};

/// How many bands of a voice the cores draw.
pub const BANDS: usize = 32;

/// Which core is shown.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum CoreStyle {
    #[default]
    Simple,
    Complex,
}

impl CoreStyle {
    pub fn name(self) -> &'static str {
        match self {
            CoreStyle::Simple => "simple",
            CoreStyle::Complex => "complex",
        }
    }

    /// "simple", "complex", and what he may call them.
    pub fn of_word(word: &str) -> Option<Self> {
        match word.trim().to_lowercase().as_str() {
            "simple" | "rings" | "ring" | "classic" | "plain" | "einfach" | "einfache" | "einfachen" | "ringe" => Some(CoreStyle::Simple),
            "complex" | "detailed" | "sphere" | "ultron" | "age of ultron" | "mind" | "circuit" | "komplex" | "komplexe" | "komplexen" | "detailliert" | "kugel" => Some(CoreStyle::Complex),
            _ => None,
        }
    }
}

/// What a core is drawn from, each frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CoreLook {
    /// Seconds of animation time.
    pub clock: f32,
    /// His own voice, 0-1.
    pub voice: f32,
    /// Sir's voice, 0-1.
    pub heard: f32,
    /// The spectrum of whichever voice is live (low to high).
    pub bands: [f32; BANDS],
    /// 0 resting … 1 thinking.
    pub busy: f32,
    /// 0 flat … 1 the rings stand as a gyroscope.
    pub tilt: f32,
    /// 0 asleep … 1 awake.
    pub wake: f32,
    /// A fault's flash, 1 fading to 0.
    pub fault: f32,
    pub alpha: f32,
    /// The time of day in seconds (the scale ring is the clock).
    pub day_secs: f32,
    /// What he is doing, under the name ("STANDING BY").
    pub word: &'static str,
    /// How hard each layer of his mind works right now, 0-1: hearing, language, tools, memory
    /// (the complex core's shells, inside out).
    pub layers: [f32; crate::holo_mind::SHELLS],
    /// The lines alone, without the light they throw. For a window whose black Windows sees
    /// through (the corner orb): faint light on black would stand there as a dark veil.
    pub crisp: bool,
}

impl Default for CoreLook {
    fn default() -> Self {
        Self { clock: 0.0, voice: 0.0, heard: 0.0, bands: [0.0; BANDS], busy: 0.0, tilt: 0.0, wake: 1.0, fault: 0.0, alpha: 1.0, day_secs: 0.0, word: "", layers: [0.0; crate::holo_mind::SHELLS], crisp: false }
    }
}

/// Draws the core. `id` names this core on screen (its glow map is kept under it): the HUD's
/// core, the corner orb and a preview each have their own.
pub fn paint(p: &Painter, id: egui::Id, style: CoreStyle, centre: Pos2, radius: f32, look: &CoreLook) {
    match style {
        CoreStyle::Simple => paint_simple(p, id, centre, radius, look),
        CoreStyle::Complex => crate::holo_mind::paint(p, id, centre, radius, look),
    }
}

/// A core's light: with a glow map square about it (a cell every two and a half points or
/// so), or the bare lines when it is drawn `crisp`.
pub(crate) fn glow(p: &Painter, id: egui::Id, centre: Pos2, reach: f32, crisp: bool) -> Light {
    if crisp {
        return Light::new(p);
    }
    Light::glowing(p, id, Rect::from_center_size(centre, vec2(reach * 2.0, reach * 2.0)), ((reach * 2.0 / 2.6) as usize).clamp(32, 230))
}

/// A ring standing in space: turned by `angle` about an axis lying in the screen at `axis`.
pub(crate) struct Tilted {
    pub centre: Pos2,
    kx: f32,
    ky: f32,
    sin: f32,
    cos: f32,
    focal: f32,
}

impl Tilted {
    pub fn new(centre: Pos2, axis: f32, angle: f32, focal: f32) -> Self {
        Self { centre, kx: axis.cos(), ky: axis.sin(), sin: angle.sin(), cos: angle.cos(), focal }
    }

    /// Where the ring's point at `alpha` lands on screen, and how far it stands towards the
    /// viewer (-radius … radius).
    pub fn at(&self, radius: f32, alpha: f32) -> (Pos2, f32) {
        let (x, y) = (radius * alpha.cos(), radius * alpha.sin());
        let along = self.kx * x + self.ky * y;
        let rx = x * self.cos + self.kx * along * (1.0 - self.cos);
        let ry = y * self.cos + self.ky * along * (1.0 - self.cos);
        let z = (self.kx * y - self.ky * x) * self.sin;
        let s = self.focal / (self.focal - z);
        (pos2(self.centre.x + rx * s, self.centre.y + ry * s), z)
    }

    /// An arc of the ring in `n` steps: its points and how brightly each stands (the far side
    /// dims).
    pub fn arc(&self, radius: f32, from: f32, to: f32, n: usize, colour: Color32) -> (Vec<Pos2>, Vec<Color32>) {
        (0..=n)
            .map(|i| {
                let (at, z) = self.at(radius, from + (to - from) * i as f32 / n as f32);
                (at, a(colour, fog(z, radius)))
            })
            .unzip()
    }
}

fn steps(radius: f32, sweep: f32) -> usize {
    (sweep.abs() * radius.max(8.0) / 3.5).ceil().clamp(6.0, 360.0) as usize
}

/// What is behind the core's plane dims to 55 %.
pub(crate) fn fog(z: f32, radius: f32) -> f32 {
    1.0 - 0.45 * (-z / radius.max(1.0)).clamp(0.0, 1.0)
}

fn paint_simple(p: &Painter, id: egui::Id, c: Pos2, r: f32, look: &CoreLook) {
    let t = holo::theme();
    let alpha = look.alpha * (0.30 + 0.70 * look.wake);
    if alpha <= 0.003 || r < 4.0 {
        return;
    }
    let accent = mix(t.accent(), t.alert(), look.fault);
    let deep = mix(t.deep(), t.alert(), look.fault);
    let hot = mix(t.hot(), t.alert(), look.fault * 0.6);
    let ink = t.ink();
    let w = px(p);
    let u = r / 100.0;
    let small = r < 64.0;
    let clock = look.clock;
    // A filament is never thinner than a pixel and grows a little with the core.
    let thin = w.max(1.15 * u.min(1.6));
    let mut light = glow(p, id, c, r * 1.34, look.crisp);
    light.spill = (0.62 + 0.30 * look.voice + 0.16 * look.busy) * alpha;
    let focal = r * 6.0;

    // The outer reference ring with its four bearings; it never tilts.
    light.ring(c, 100.0 * u, w, a(ink, 0.22 * alpha));
    for q in 0..4 {
        let angle = NORTH + q as f32 * TAU / 4.0;
        light.seg(polar(c, 96.0 * u, angle), polar(c, 104.0 * u, angle), w, a(ink, 0.60 * alpha));
    }

    // Sir's voice: white ticks beyond the reference ring, his spectrum mirrored about north.
    if look.heard > 0.004 {
        let n = if small { 60 } else { 120 };
        for i in 0..n {
            let mirrored = if i < n / 2 { i } else { n - 1 - i };
            let band = look.bands[(mirrored * 5) % BANDS];
            let len = (1.5 + 15.0 * band * look.heard.sqrt()) * u;
            let angle = NORTH + TAU * (i as f32 + 0.5) / n as f32;
            light.seg(polar(c, 106.0 * u, angle), polar(c, 106.0 * u + len, angle), thin, a(ink, (0.25 + 0.65 * band) * look.heard.sqrt() * alpha));
        }
    }

    // Two orbits in the deep tone, turning against each other: faster with thought.
    let pace = 1.0 + 5.0 * look.busy;
    for (radius, axis, lean, speed, spans) in [(93.0, 2.8, 1.10, 0.11, &[(0.0, 2.2), (3.4, 1.1)][..]), (86.0, 1.05, 0.75, -0.17, &[(0.6, 1.5), (2.9, 0.5), (4.4, 0.9)][..])] {
        let tilted = Tilted::new(c, axis + clock * 0.05, look.tilt * lean, focal);
        let spin = clock * speed * pace;
        for (start, sweep) in spans {
            let (pts, cols) = tilted.arc(radius * u, spin + start, spin + start + sweep, steps(radius * u, *sweep), mix(deep, hot, 0.25).gamma_multiply(0.95 * alpha));
            light.ribbon(&pts, &cols, false, thin);
            // An orbit ends in a bright head.
            let (head, z) = tilted.at(radius * u, spin + start + sweep);
            light.spark(head, 2.4 * u.max(0.7), a(hot, fog(z, radius * u) * alpha));
        }
    }

    // The scale: the clock. Sixty ticks, twelve of them long.
    let scale = Tilted::new(c, 1.75 - clock * 0.031, look.tilt * 0.9, focal);
    let ticks = if small { 12 } else { 60 };
    for i in 0..ticks {
        let angle = NORTH + TAU * i as f32 / ticks as f32;
        let long = small || i % 5 == 0;
        let (from, z) = scale.at(if long { 70.0 } else { 73.0 } * u, angle);
        let (to, _) = scale.at(77.0 * u, angle);
        light.seg(from, to, w, a(ink, if long { 0.70 } else { 0.32 } * fog(z, r) * alpha));
    }
    let secs = look.day_secs.rem_euclid(86_400.0);
    let minute = (secs / 3600.0).fract() * TAU;
    let hour = (secs / 43_200.0).fract() * TAU;
    // The minutes: a line of light from twelve to the minute hand.
    if minute > 0.01 {
        let (pts, cols) = scale.arc(80.0 * u, NORTH, NORTH + minute, steps(80.0 * u, minute), a(hot, 0.95 * alpha));
        light.ribbon(&pts, &cols, false, thin);
    }
    // The hour: a short block outside the scale.
    let (pts, cols) = scale.arc(83.2 * u, NORTH + hour - 0.07, NORTH + hour + 0.07, 8, a(hot, alpha));
    light.ribbon(&pts, &cols, false, 3.0 * u.max(0.5));
    // The second: one bright tick that steps round.
    let second = NORTH + (secs % 60.0).floor() / 60.0 * TAU;
    let (from, z) = scale.at(66.0 * u, second);
    let (to, _) = scale.at(77.0 * u, second);
    light.seg(from, to, thin * 1.3, a(hot, fog(z, r) * alpha));

    // The band: four segments of glass, lit from the filament on their inner edge. They open
    // while he listens and spin up while he thinks.
    let band = Tilted::new(c, 0.35 + clock * 0.043, look.tilt, focal);
    let spin = clock * (0.16 + 1.25 * look.busy);
    let gap = 0.13 + 0.17 * look.heard.sqrt();
    let lit_glass = 0.30 + 0.14 * look.busy + 0.20 * look.voice;
    for k in 0..4 {
        let from = spin + k as f32 * TAU / 4.0 + gap;
        let to = spin + (k + 1) as f32 * TAU / 4.0 - gap;
        let n = steps(62.0 * u, to - from);
        let (inner, inner_cols) = band.arc(52.6 * u, from, to, n, lit(accent, lit_glass * alpha));
        let (outer, outer_cols) = band.arc(62.5 * u, from, to, n, lit(deep, 0.035 * alpha));
        light.sheet(&inner, &outer, &inner_cols, &outer_cols);
        // The filament: white-hot, a little uneven along its length like any real tube.
        let (pts, mut cols) = band.arc(52.6 * u, from, to, n, a(hot, alpha));
        for (i, col) in cols.iter_mut().enumerate() {
            *col = a(*col, 0.86 + 0.14 * (i as f32 * 0.37 + clock * 0.9 + k as f32 * 2.0).sin());
        }
        light.ribbon(&pts, &cols, false, thin * 1.25);
        if !small {
            // The segment's far edge: a dashed hairline.
            let dashes = 14;
            for d in 0..dashes {
                let x0 = from + (to - from) * d as f32 / dashes as f32;
                let x1 = x0 + (to - from) / dashes as f32 * 0.55;
                let (pts, cols) = band.arc(62.5 * u, x0, x1, 3, a(ink, 0.42 * alpha));
                light.ribbon(&pts, &cols, false, w);
            }
        }
    }

    // His voice: the inner ring. Still, it is a ring of dots; speaking, the dots grow into bars.
    let bars = if small { 36 } else { 72 };
    for i in 0..bars {
        // The spectrum, dealt round the ring and mirrored left to right: loud all the way round.
        let mirrored = if i < bars / 2 { i } else { bars - 1 - i };
        let band = look.bands[(mirrored * 7) % BANDS];
        let level = band * look.voice.sqrt();
        let len = (1.2 + 13.0 * level) * u;
        let angle = NORTH + TAU * (i as f32 + 0.5) / bars as f32;
        let colour = mix(a(accent, (0.60 + 0.40 * level) * alpha), a(hot, alpha), level);
        light.seg(polar(c, 38.0 * u, angle), polar(c, 38.0 * u + len, angle), (1.5 * u).max(w), colour);
    }
    light.ring(c, 35.0 * u, thin, a(mix(accent, hot, 0.3 + 0.5 * look.voice), (0.70 + 0.30 * look.voice) * alpha));
    // The air inside the ring: it fills with light as he speaks.
    light.haze(c, (0.5 + 5.0 * look.voice + 1.2 * look.busy) * alpha * u.max(0.4));

    // Thinking: a sweep of light runs the inner ring.
    if look.busy > 0.02 {
        let head = clock * 3.4;
        for k in 0..18 {
            let from = head - k as f32 * 0.07;
            light.arc(c, 35.0 * u, from - 0.07, from, (2.2 * u).max(w), a(hot, (1.0 - k as f32 / 18.0).powi(2) * look.busy * alpha));
        }
    }
    light.paint(p);

    // The name, and under it what he is doing.
    let size = (11.5 * u).max(7.0);
    holo::text(p, pos2(c.x, c.y - 1.5 * u), Align2::CENTER_CENTER, Face::Label, "J.A.I.", size, a(ink, (0.55 + 0.45 * look.wake) * look.alpha));
    if !small && !look.word.is_empty() {
        holo::text(p, pos2(c.x, c.y + 13.0 * u), Align2::CENTER_CENTER, Face::Label, look.word, (4.6 * u).max(6.5), a(accent, 0.9 * alpha));
    }
    if r >= 150.0 {
        // The bearings, on the reference ring.
        for (q, word) in ["000", "090", "180", "270"].iter().enumerate() {
            let at = polar(c, 111.0 * u, NORTH + q as f32 * PI / 2.0);
            holo::text(p, at, Align2::CENTER_CENTER, Face::Mono, word, (5.0 * u).max(8.0), a(ink, 0.42 * alpha));
        }
    }
}

/// How long the core's ripple runs (seconds).
pub const RIPPLE_SECS: f32 = 0.9;

/// The core's answer when he hears his name (his silent "yes?"): three rings of light leave it
/// and fade, the first white-hot, the next in the accent, the last in the deep tone. `age` is
/// the seconds since he heard it; returns whether it is still running.
pub fn ripple(p: &Painter, centre: Pos2, radius: f32, age: f32) -> bool {
    if !(0.0..RIPPLE_SECS).contains(&age) {
        return false;
    }
    let t = holo::theme();
    let mut light = Light::new(p);
    for (delay, colour, glow) in [(0.0, t.hot(), 1.0), (0.14, t.accent(), 0.7), (0.28, t.deep(), 0.5)] {
        let run = (age - delay) / (RIPPLE_SECS - delay);
        if !(0.0..1.0).contains(&run) {
            continue;
        }
        let fade = (1.0 - run) * (1.0 - run);
        light.neon_arc(centre, radius * (1.04 + 0.85 * crate::holo_motion::ease_out_expo(run)), 0.0, TAU, px(p).max(1.0), a(colour, 0.9 * fade), fade * glow);
    }
    light.paint(p);
    true
}

// ----------------------------------------------------------------------- where he stands ----

/// How far the docked core stands from the screen's corner, and how large it is there.
pub const DOCK_INSET: f32 = 75.0;
pub const DOCKED_DIAMETER: f32 = 130.0;
/// A change of pose: the rings stand up into the gyroscope (and lie down again), the core
/// glides to its corner (and home).
pub const UNFOLD_SECS: f32 = 0.42;
pub const DOCK_SECS: f32 = 0.46;
/// The centred core's radius, as a share of the screen's shorter side.
pub const CENTRE_RADIUS_SHARE: f32 = 0.17;
/// How long a fault's flash takes to die away.
const FAULT_SECS: f32 = 0.7;
/// How far the core opens while he listens, swells with his own voice and with the music.
const LISTEN_OPEN: f32 = 0.055;
const VOICE_PULSE: f32 = 0.03;
const MUSIC_PULSE: f32 = 0.045;

/// The docked core's centre: bottom right, left of the text console while it is out
/// (`console_width`: how far it has slid in), so the console never covers him.
pub fn docked_centre(screen: Rect, console_width: f32) -> Pos2 {
    pos2(screen.right() - console_width.max(0.0) - DOCK_INSET, screen.bottom() - DOCK_INSET)
}

/// How the core stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pose {
    /// In the middle, flat, facing Sir.
    Front,
    /// In the middle, its rings standing as a gyroscope: thinking, researching, asleep.
    Gyro,
    /// Small in the bottom right corner while something covers the middle.
    Docked,
}

/// What he is doing, as the core shows it on top of its pose.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mood {
    #[default]
    Rest,
    Listening,
    Thinking,
    Speaking,
}

/// What the core's pose depends on.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CoreInputs {
    /// A turn is being answered.
    pub thinking: bool,
    /// A research agent is running.
    pub researching: bool,
    /// The board was signed off ("screen down").
    pub signed_off: bool,
    /// Sleep mode or the zero-VRAM standby.
    pub asleep: bool,
    /// The text console is out (the docked core keeps left of it).
    pub console_open: bool,
    /// The core view is on screen (it owns the middle).
    pub core_open: bool,
    /// A block, the spotlight or a tool block covers the middle.
    pub centre_covered: bool,
    /// Nothing has happened for 12 s: the idle morph may run.
    pub idle: bool,
    /// The idle morph shows the gyroscope right now (set by `Core::follow`).
    pub idle_gyro: bool,
}

impl CoreInputs {
    /// Something is crunching: everything turns faster and a docked core stands up.
    pub fn busy(&self) -> bool {
        self.thinking || self.researching
    }
}

/// The pose for a set of inputs: docked only when the middle of the screen is really covered
/// (he never sits on what Sir reads, and never leaves the middle empty), else the gyroscope
/// while he thinks, researches, sleeps or is signed off, else facing Sir.
pub fn resolve(inputs: CoreInputs) -> Pose {
    if inputs.centre_covered || inputs.core_open {
        Pose::Docked
    } else if inputs.thinking || inputs.researching || inputs.signed_off || inputs.asleep || (inputs.idle && inputs.idle_gyro) {
        Pose::Gyro
    } else {
        Pose::Front
    }
}

/// (flat, dock) a pose eases to: flat 0 the gyroscope, 1 facing Sir; dock 0 the middle, 1 the
/// corner. A docked core stands up into a small gyroscope while he is busy.
pub fn pose_targets(pose: Pose, busy: bool) -> (f32, f32) {
    match pose {
        Pose::Gyro => (0.0, 0.0),
        Pose::Front => (1.0, 0.0),
        Pose::Docked => (if busy { 0.0 } else { 1.0 }, 1.0),
    }
}

/// Where the core is and how large, this frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CoreFrame {
    pub centre: Pos2,
    /// The radius of its reference ring.
    pub radius: f32,
    /// That radius at rest: without the swell of listening, of his voice, of the music.
    pub rest: f32,
    /// 0 the gyroscope … 1 facing Sir.
    pub flat: f32,
    /// 0 the middle … 1 the corner.
    pub dock: f32,
}

pub fn centre_radius(screen: Rect) -> f32 {
    (screen.width().min(screen.height()) * CENTRE_RADIUS_SHARE).clamp(70.0, 260.0)
}

/// The undocked core's centre and radius: the middle of the screen, unless the text console
/// (`console_width` slid in) would cut into it. Then he moves left just far enough, and
/// shrinks only when even that is not enough.
pub fn home_pose(screen: Rect, console_width: f32) -> (Pos2, f32) {
    let free_right = screen.right() - console_width.max(0.0) - 16.0;
    let free_width = (free_right - screen.left()).max(0.0);
    let radius = centre_radius(screen).min((free_width / 2.0 - 8.0).max(40.0));
    let x = screen.center().x.min(free_right - radius).max(screen.left() + radius);
    (pos2(x, screen.center().y), radius)
}

/// A value easing from `from` to `to` over `secs`, started at `start` (the core's clock).
#[derive(Debug, Clone, Copy)]
struct Tween {
    from: f32,
    to: f32,
    start: f32,
    secs: f32,
}

impl Tween {
    fn fixed(v: f32) -> Self {
        Self { from: v, to: v, start: 0.0, secs: 0.0 }
    }

    fn value(&self, now: f32) -> f32 {
        self.from + (self.to - self.from) * crate::holo_motion::ease_in_out((now - self.start) / self.secs.max(1e-3))
    }

    fn retarget(&mut self, to: f32, now: f32, secs: f32) {
        if (self.to - to).abs() >= 1e-6 {
            *self = Self { from: self.value(now), to, start: now, secs };
        }
    }

    fn done(&self, now: f32) -> bool {
        now - self.start >= self.secs
    }
}

/// While he idles, the core now and then stands up into the gyroscope and lies down again:
/// after 120-240 s of unbroken idle it holds the other form for 120-240 s. Anything happening
/// ends it at once.
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

/// `value` towards `target`, the same at any frame rate, never past it.
pub fn follow(value: f32, target: f32, rate: f32, dt: f32) -> f32 {
    value + (target - value) * (1.0 - (-rate * dt).exp())
}

/// A frame time safe to animate with: a frame after an idle second never jumps, and at ten
/// frames a second everything still turns with the clock.
pub fn safe_dt(dt: f32) -> f32 {
    if dt.is_finite() { dt.clamp(0.001, 0.1) } else { 0.016 }
}

/// What the core hears of the world, each frame.
#[derive(Debug, Clone, Copy, Default)]
pub struct Feed {
    /// His own voice (the playback level, 0-1).
    pub voice: f32,
    /// Sir's voice (the microphone, 0-1), while he is being listened to.
    pub heard: f32,
    /// The spectrum of what sounds, when it is known; made up from the level otherwise.
    pub bands: Option<[f32; BANDS]>,
    /// A research agent or the language model is working.
    pub busy: bool,
    pub asleep: bool,
    /// How hard each layer of his mind works (hearing, language, tools, memory).
    pub layers: [f32; crate::holo_mind::SHELLS],
    /// The time of day in seconds (his scale ring is the clock).
    pub day_secs: f32,
    /// What he is doing, in a word ("THINKING").
    pub word: &'static str,
}

/// Each core on screen has its own glow map: they are told apart by this.
static NEXT_CORE: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(1);

/// The core as it lives on the HUD: which core, where it stands, how it moves there.
pub struct Core {
    pub style: CoreStyle,
    id: u32,
    day_secs: f32,
    word: &'static str,
    pose: Pose,
    busy_target: bool,
    flat: Tween,
    dock: Tween,
    /// How far the rings stand up (0 flat … 1 the gyroscope) and how fast that changes: a
    /// spring following `1 - flat`.
    tilt: f32,
    tilt_v: f32,
    clock: f32,
    voice: f32,
    heard: f32,
    busy: f32,
    wake: f32,
    bands: [f32; BANDS],
    layers: [f32; crate::holo_mind::SHELLS],
    /// How far the text console has slid in: the docked core keeps left of it.
    console_width: f32,
    idle_morph: IdleMorph,
    /// Where Sir put him (fractions of the canvas, None: home), and where he is now.
    custom_home: Option<Vec2>,
    home_now: Option<Vec2>,
    /// How large Sir made him at home ("make the orb bigger": 0.4-2.5), and how large he is
    /// while he grows there.
    pub size: f32,
    size_now: f32,
    /// Each monitor's part of a canvas that reaches over more than one.
    rooms: Vec<Rect>,
    mood: Mood,
    listen: f32,
    fault: f32,
    music: f32,
}

impl Default for Core {
    fn default() -> Self {
        Self::new(Pose::Gyro)
    }
}

impl Core {
    pub fn new(pose: Pose) -> Self {
        let (flat, dock) = pose_targets(pose, false);
        Self {
            style: CoreStyle::default(),
            id: NEXT_CORE.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            day_secs: 0.0,
            word: "",
            pose,
            busy_target: false,
            flat: Tween::fixed(flat),
            dock: Tween::fixed(dock),
            tilt: 1.0 - flat,
            tilt_v: 0.0,
            clock: 0.0,
            voice: 0.0,
            heard: 0.0,
            busy: 0.0,
            wake: 1.0,
            bands: [0.0; BANDS],
            layers: [0.0; crate::holo_mind::SHELLS],
            console_width: 0.0,
            idle_morph: IdleMorph::new(0x9E37_79B9),
            custom_home: None,
            home_now: None,
            size: 1.0,
            size_now: 1.0,
            rooms: Vec::new(),
            mood: Mood::Rest,
            listen: 0.0,
            fault: 0.0,
            music: 0.0,
        }
    }

    /// Puts the core anywhere on the canvas (`at`: fractions of the screen it is drawn for,
    /// beyond 0-1 on another monitor of a canvas that reaches there; None: back home). He
    /// glides there from wherever he is.
    pub fn set_custom_home(&mut self, at: Option<Vec2>) {
        self.custom_home = at.map(|f| vec2(f.x.clamp(-8.0, 8.0), f.y.clamp(-8.0, 8.0)));
    }

    /// Each monitor's part of the canvas, when it reaches over more than one: he may stand on
    /// any of them.
    pub fn set_rooms(&mut self, rooms: &[Rect]) {
        if self.rooms != rooms {
            self.rooms = rooms.to_vec();
        }
    }

    /// The monitor's part the point `p` of the canvas lies in (else the nearest; `screen` on a
    /// canvas of one screen).
    pub fn room_at(&self, screen: Rect, p: Pos2) -> Rect {
        self.rooms.iter().copied().min_by(|a, b| a.distance_sq_to_pos(p).total_cmp(&b.distance_sq_to_pos(p))).unwrap_or(screen)
    }

    pub fn custom_home(&self) -> Option<Vec2> {
        self.custom_home
    }

    /// Follows the pointer while Sir drags him (no easing lag).
    pub fn drag_to(&mut self, at: Vec2) {
        self.set_custom_home(Some(at));
        self.home_now = self.custom_home;
    }

    /// The undocked centre (as fractions of `screen`) and radius: Sir's spot, kept a radius
    /// inside the monitor it lies on, else the middle (left of the console when it cuts in).
    fn home_target(&self, screen: Rect) -> (Vec2, f32) {
        let (home, radius) = home_pose(screen, self.console_width);
        let to_frac = |p: Pos2| vec2((p.x - screen.left()) / screen.width().max(1.0), (p.y - screen.top()) / screen.height().max(1.0));
        match self.custom_home {
            Some(f) => {
                let p = screen.min + vec2(f.x * screen.width(), f.y * screen.height());
                let room = self.room_at(screen, p);
                let r = centre_radius(room) * self.size_now;
                let p = pos2(p.x.clamp(room.left() + r, (room.right() - r).max(room.left() + r)), p.y.clamp(room.top() + r, (room.bottom() - r).max(room.top() + r)));
                (to_frac(p), r)
            }
            None => (to_frac(home), radius * self.size_now),
        }
    }

    /// Eases his home towards its target (once a frame, with the canvas).
    pub fn glide(&mut self, screen: Rect, dt: f32) {
        let (target, _) = self.home_target(screen);
        let now = self.home_now.unwrap_or(target);
        // (A move takes a few frames however few are drawn: never three-quarters in one.)
        let dt = safe_dt(dt).min(1.0 / 30.0);
        // (He lands on his size: an ease alone never arrives, and the HUD would draw for ever.)
        self.size_now = if (self.size - self.size_now).abs() > 2e-3 { follow(self.size_now, self.size, 9.0, dt) } else { self.size };
        self.home_now = Some(vec2(follow(now.x, target.x, 14.0, dt), follow(now.y, target.y, 14.0, dt)));
    }

    pub fn pose(&self) -> Pose {
        self.pose
    }

    pub fn set_console_width(&mut self, width: f32) {
        self.console_width = width.max(0.0);
    }

    /// Moves towards a pose. Leaving the corner for the gyroscope, he first glides home flat
    /// and stands up there.
    pub fn set_pose(&mut self, pose: Pose, busy: bool) {
        if pose == self.pose && busy == self.busy_target {
            return;
        }
        let (flat, dock) = pose_targets(pose, busy);
        let now = self.clock;
        let leaving_corner = self.pose == Pose::Docked && pose == Pose::Gyro;
        self.dock.retarget(dock, now, DOCK_SECS);
        if leaving_corner && (self.flat.to - flat).abs() > 1e-6 {
            self.flat = Tween { from: self.flat.value(now), to: flat, start: now + DOCK_SECS * 0.8, secs: UNFOLD_SECS * 1.2 };
        } else {
            self.flat.retarget(flat, now, UNFOLD_SECS);
        }
        self.pose = pose;
        self.busy_target = busy;
    }

    /// Picks the pose from `inputs` (with the idle morph's turn) and moves towards it.
    pub fn follow(&mut self, mut inputs: CoreInputs) {
        inputs.idle_gyro = self.idle_morph.step(self.clock, inputs.idle);
        self.set_pose(resolve(inputs), inputs.busy());
    }

    pub fn set_mood(&mut self, mood: Mood) {
        self.mood = mood;
    }

    /// The music's bass right now (0 while nothing plays): he moves with it.
    pub fn set_music(&mut self, bass: f32) {
        self.music = bass.clamp(0.0, 1.0);
    }

    /// An order failed: one flash in the alert colour.
    pub fn flash_fault(&mut self) {
        self.fault = 1.0;
    }

    /// Advances the clock, the envelopes and the spring that stands the rings up.
    pub fn step(&mut self, dt: f32, feed: &Feed) {
        let dt = safe_dt(dt);
        self.clock += dt;
        (self.day_secs, self.word) = (feed.day_secs, feed.word);
        self.listen = follow(self.listen, if self.mood == Mood::Listening { 1.0 } else { 0.0 }, 9.0, dt);
        self.fault = (self.fault - dt / FAULT_SECS).max(0.0);
        // Quick to rise, slower to fall, like a meter's needle.
        let rise = |now: f32, target: f32| follow(now, target.clamp(0.0, 1.0), if target > now { 34.0 } else { 9.0 }, dt);
        self.voice = rise(self.voice, feed.voice);
        self.heard = rise(self.heard, feed.heard);
        self.busy = follow(self.busy, if feed.busy { 1.0 } else { 0.0 }, 3.7, dt);
        self.wake = follow(self.wake, if feed.asleep { 0.0 } else { 1.0 }, 3.0, dt);
        for (now, target) in self.layers.iter_mut().zip(feed.layers) {
            *now = rise(*now, target);
        }
        let level = self.voice.max(self.heard);
        for (i, band) in self.bands.iter_mut().enumerate() {
            let target = match feed.bands {
                Some(bands) => bands[i],
                // No spectrum to go by: a voice's shape (strong low, thinning out), each band
                // flickering at its own pace under the real level.
                None => {
                    let x = i as f32 / BANDS as f32;
                    let flicker = 0.5 + 0.5 * (self.clock * (7.0 + 9.0 * x) + i as f32 * 2.4).sin() * (self.clock * 3.1 + i as f32).cos();
                    (1.0 - 0.72 * x) * (0.35 + 0.65 * flicker) * level.min(1.0).sqrt()
                }
            };
            *band = rise(*band, target);
        }
        let flat = self.flat.value(self.clock).clamp(0.0, 1.0);
        crate::holo_motion::spring(&mut self.tilt, &mut self.tilt_v, 1.0 - flat, 9.0, dt);
        // (Arrived, as far as any eye can tell: it stands, and the HUD may rest.)
        if (self.tilt - (1.0 - flat)).abs() < 5e-4 && self.tilt_v.abs() < 5e-3 {
            (self.tilt, self.tilt_v) = (1.0 - flat, 0.0);
        }
    }

    /// `step` for a stage that has only a voice level to give (the backdrop, the verifier).
    pub fn tick(&mut self, dt: f32, voice: f32, busy: bool) {
        self.step(dt, &Feed { voice, busy, day_secs: self.day_secs, word: self.word, ..Default::default() });
    }

    /// Whether anything is still moving (the HUD keeps drawing meanwhile).
    pub fn is_animating(&self) -> bool {
        let easing = |x: f32| x > 0.003 && x < 0.997;
        if self.fault > 0.0 || self.music > 0.01 || easing(self.listen) || easing(self.wake) || self.voice > 0.004 || self.heard > 0.004 {
            return true;
        }
        let settling = (self.tilt - (1.0 - self.flat.to)).abs() > 1e-3 || self.tilt_v.abs() > 1e-3;
        let gliding = matches!((self.custom_home, self.home_now), (Some(t), Some(n)) if (t - n).length() > 1e-4) || (self.size - self.size_now).abs() > 2e-3;
        !self.flat.done(self.clock) || !self.dock.done(self.clock) || settling || gliding
    }

    /// Where he is on `screen` this frame.
    pub fn frame(&self, screen: Rect) -> CoreFrame {
        let dock = self.dock.value(self.clock).clamp(0.0, 1.0);
        let flat = self.flat.value(self.clock).clamp(0.0, 1.0);
        let docked = docked_centre(screen, self.console_width);
        let (target, home_radius) = self.home_target(screen);
        let f = self.home_now.unwrap_or(target);
        let home = screen.min + vec2(f.x * screen.width(), f.y * screen.height());
        let centre = home.lerp(docked, dock);
        let radius = home_radius + (DOCKED_DIAMETER / 2.0 - home_radius) * dock;
        // At home he opens a little while he listens and swells with his own voice.
        let alive = 1.0 + (1.0 - dock) * (LISTEN_OPEN * self.listen + MUSIC_PULSE * self.music + if self.mood == Mood::Speaking { VOICE_PULSE * self.voice } else { 0.0 });
        CoreFrame { centre, radius: radius * alive, rest: radius, flat, dock }
    }

    /// What he is drawn from this frame.
    pub fn look(&self) -> CoreLook {
        CoreLook {
            clock: self.clock,
            voice: self.voice.max(self.music * 0.5),
            heard: self.heard * self.listen.max(0.25),
            bands: self.bands,
            busy: self.busy,
            tilt: self.tilt.clamp(0.0, 1.1),
            wake: self.wake,
            fault: self.fault,
            alpha: 1.0,
            day_secs: self.day_secs,
            word: self.word,
            layers: self.layers,
            crisp: false,
        }
    }

    /// The radius a style is drawn at for a frame: the sphere's rings reach past its shells,
    /// so it is drawn smaller to take the room the rings would.
    pub fn drawn_radius(&self, frame: &CoreFrame) -> f32 {
        match self.style {
            CoreStyle::Simple => frame.radius,
            CoreStyle::Complex => frame.radius * 0.74,
        }
    }

    /// The name of this core's glow map on the painter's layer.
    fn glow_id(&self, p: &Painter) -> egui::Id {
        egui::Id::new(("holo-core", self.id, p.layer_id()))
    }

    /// Paints him on `screen` (the HUD's canvas).
    pub fn paint(&self, p: &Painter, screen: Rect) {
        let frame = self.frame(screen);
        paint(p, self.glow_id(p), self.style, frame.centre, self.drawn_radius(&frame), &self.look());
    }

    fn radius_in(&self, rect: Rect) -> f32 {
        rect.width().min(rect.height()) * if self.style == CoreStyle::Simple { 0.36 } else { 0.30 }
    }

    /// Paints him filling a small window (pure orb mode), as he is now.
    pub fn paint_in(&self, p: &Painter, rect: Rect) {
        paint(p, self.glow_id(p), self.style, rect.center(), self.radius_in(rect), &self.look());
    }

    /// As `paint_in`, the lines alone (`CoreLook::crisp`): for the corner orb, whose window's
    /// black Windows sees through.
    pub fn paint_in_crisp(&self, p: &Painter, rect: Rect) {
        paint(p, self.glow_id(p), self.style, rect.center(), self.radius_in(rect), &CoreLook { crisp: true, ..self.look() });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn screen() -> Rect {
        Rect::from_min_size(pos2(0.0, 0.0), vec2(1920.0, 1080.0))
    }

    fn settle(core: &mut Core, secs: f32, feed: &Feed) {
        for _ in 0..(secs * 60.0) as usize {
            core.step(1.0 / 60.0, feed);
            core.glide(screen(), 1.0 / 60.0);
        }
    }

    /// Docked only while the middle is covered; thinking stands him up; otherwise he faces Sir.
    #[test]
    fn he_docks_only_when_the_middle_is_covered() {
        let none = CoreInputs::default();
        assert_eq!(resolve(none), Pose::Front);
        assert_eq!(resolve(CoreInputs { thinking: true, ..none }), Pose::Gyro);
        assert_eq!(resolve(CoreInputs { asleep: true, ..none }), Pose::Gyro);
        assert_eq!(resolve(CoreInputs { centre_covered: true, thinking: true, ..none }), Pose::Docked);
        assert_eq!(resolve(CoreInputs { core_open: true, ..none }), Pose::Docked);
        assert_eq!(resolve(CoreInputs { idle: true, ..none }), Pose::Front, "idle alone changes nothing");
        assert_eq!(resolve(CoreInputs { idle: true, idle_gyro: true, ..none }), Pose::Gyro);
        assert_eq!(pose_targets(Pose::Docked, true), (0.0, 1.0), "a busy docked core stands up");
    }

    /// "Make the orb bigger": he grows to the size he was given, without a jump, and lands on it.
    #[test]
    fn he_grows_to_the_size_he_was_given_and_lands_on_it() {
        let mut core = Core::new(Pose::Front);
        let usual = core.frame(screen()).rest;
        core.size = 1.5;
        core.glide(screen(), 1.0 / 60.0);
        let growing = core.frame(screen()).rest;
        assert!(growing > usual && growing < usual * 1.2, "one frame is a step, not the whole way: {usual} to {growing}");
        assert!(core.is_animating(), "the HUD keeps drawing while he grows");
        for _ in 0..180 {
            core.glide(screen(), 1.0 / 60.0);
        }
        assert_eq!(core.frame(screen()).rest, usual * 1.5);
        assert!(!core.is_animating(), "and rests once he is there");
        // Docked in the corner he is the corner's size, whatever his size at home.
        let mut docked = Core::new(Pose::Docked);
        docked.size = 2.0;
        docked.glide(screen(), 1.0 / 60.0);
        assert!((docked.frame(screen()).rest - DOCKED_DIAMETER / 2.0).abs() < 1e-3);
    }

    #[test]
    fn docking_glides_to_the_corner_and_keeps_left_of_the_console() {
        let mut core = Core::new(Pose::Front);
        let home = core.frame(screen());
        assert_eq!(home.centre, screen().center());
        assert_eq!(home.radius, centre_radius(screen()));
        core.follow(CoreInputs { centre_covered: true, ..Default::default() });
        // He never jumps: no frame moves him further than an eased glide's fastest (three
        // times its mean speed).
        let way = (docked_centre(screen(), 0.0) - home.centre).length();
        let fastest = way * 3.2 / (DOCK_SECS * 60.0);
        let mut last = home.centre;
        for _ in 0..(DOCK_SECS * 60.0) as usize + 2 {
            core.step(1.0 / 60.0, &Feed::default());
            let at = core.frame(screen()).centre;
            assert!((at - last).length() <= fastest, "a jump of {} (the fastest step is {fastest})", (at - last).length());
            last = at;
        }
        let docked = core.frame(screen());
        assert_eq!(docked.centre, docked_centre(screen(), 0.0));
        assert!((docked.radius - DOCKED_DIAMETER / 2.0).abs() < 0.5);
        core.set_console_width(440.0);
        assert_eq!(core.frame(screen()).centre.x, 1920.0 - 440.0 - DOCK_INSET);
    }

    #[test]
    fn at_home_he_stays_clear_of_the_console() {
        let narrow = Rect::from_min_size(pos2(0.0, 0.0), vec2(1000.0, 800.0));
        let (centre, radius) = home_pose(narrow, 440.0);
        assert!(centre.x + radius <= 1000.0 - 440.0 - 16.0 + 0.01);
        assert!(centre.x - radius >= 0.0);
        let (wide, _) = home_pose(screen(), 440.0);
        assert_eq!(wide, screen().center(), "on a wide screen the console never reaches him");
    }

    #[test]
    fn thinking_stands_the_rings_up_without_overshoot_and_waking_lays_them_flat() {
        let mut core = Core::new(Pose::Front);
        core.follow(CoreInputs { thinking: true, ..Default::default() });
        let mut peak = 0.0f32;
        for _ in 0..120 {
            core.step(1.0 / 60.0, &Feed { busy: true, ..Default::default() });
            peak = peak.max(core.look().tilt);
        }
        assert!(peak <= 1.001 && core.look().tilt > 0.97, "tilt {} peak {peak}", core.look().tilt);
        core.follow(CoreInputs::default());
        settle(&mut core, 1.5, &Feed::default());
        assert!(core.look().tilt < 0.02);
        assert!(!core.is_animating(), "tilt {} moving at {}", core.tilt, core.tilt_v);
    }

    #[test]
    fn he_moves_with_his_own_voice_and_shows_sirs_only_while_listening() {
        let mut core = Core::new(Pose::Front);
        core.set_mood(Mood::Speaking);
        settle(&mut core, 0.5, &Feed { voice: 0.8, ..Default::default() });
        let look = core.look();
        assert!(look.voice > 0.7 && look.bands.iter().any(|b| *b > 0.2), "his voice lights the ring");
        assert!(core.frame(screen()).radius > core.frame(screen()).rest, "and swells him");
        // Sir talking while he is not listened to barely shows; listened to, it does.
        let mut core = Core::new(Pose::Front);
        settle(&mut core, 0.5, &Feed { heard: 0.9, ..Default::default() });
        let unheard = core.look().heard;
        core.set_mood(Mood::Listening);
        settle(&mut core, 0.5, &Feed { heard: 0.9, ..Default::default() });
        assert!(core.look().heard > unheard * 3.0);
        assert_eq!(core.look().voice, 0.0, "Sir's voice never moves the ring of his own");
    }

    #[test]
    fn he_goes_where_sir_puts_him_and_glides_there() {
        let mut core = Core::new(Pose::Front);
        settle(&mut core, 0.2, &Feed::default());
        core.set_custom_home(Some(vec2(0.25, 0.3)));
        let mut last = core.frame(screen()).centre;
        for _ in 0..90 {
            core.glide(screen(), 1.0 / 60.0);
            let at = core.frame(screen()).centre;
            assert!((at - last).length() < 120.0);
            last = at;
        }
        assert!((last - pos2(480.0, 324.0)).length() < 2.0, "{last:?}");
        // Dragged off the edge he is under the hand; let go, he glides a radius inside.
        core.drag_to(vec2(0.0, 0.0));
        assert_eq!(core.frame(screen()).centre, pos2(0.0, 0.0));
        for _ in 0..90 {
            core.glide(screen(), 1.0 / 60.0);
        }
        let at = core.frame(screen());
        assert!(at.centre.x >= at.rest - 0.5 && at.centre.y >= at.rest - 0.5, "{at:?}");
        // On a canvas over two monitors he may stand on the second.
        let wide = Rect::from_min_size(pos2(0.0, 0.0), vec2(3840.0, 1080.0));
        core.set_rooms(&[screen(), Rect::from_min_size(pos2(1920.0, 0.0), vec2(1920.0, 1080.0))]);
        core.drag_to(vec2(0.75, 0.5));
        assert_eq!(core.frame(wide).centre, pos2(2880.0, 540.0));
    }

    #[test]
    fn the_idle_morph_waits_two_minutes_and_ends_with_any_activity() {
        let mut morph = IdleMorph::new(7);
        assert!(!morph.step(10.0, true));
        let mut shown_at = None;
        for s in 0..600 {
            if morph.step(s as f32, true) {
                shown_at = Some(s as f32);
                break;
            }
        }
        let at = shown_at.expect("it shows within ten minutes of idle");
        assert!(at >= IdleMorph::INTERVAL.0, "{at}");
        assert!(!morph.step(at + 1.0, false), "activity ends it");
        assert!(!morph.step(at + 2.0, true), "and the countdown starts again");
    }

    #[test]
    fn a_flat_ring_is_a_circle_and_a_tilted_one_has_a_near_and_a_far_side() {
        let c = pos2(500.0, 400.0);
        let flat = Tilted::new(c, 0.3, 0.0, 600.0);
        for k in 0..8 {
            let (at, z) = flat.at(100.0, k as f32);
            assert!(((at - c).length() - 100.0).abs() < 1e-3 && z.abs() < 1e-4);
        }
        let tilted = Tilted::new(c, 0.0, 1.0, 600.0);
        let (near, z_near) = tilted.at(100.0, PI / 2.0);
        let (far, z_far) = tilted.at(100.0, -PI / 2.0);
        assert!(z_near > 50.0 && z_far < -50.0, "{z_near} {z_far}");
        // Seen from the front it is an ellipse: the tilted axis is shorter than the radius.
        assert!((near.y - c.y).abs() < 70.0 && (far.y - c.y).abs() < 70.0);
        assert!(fog(z_far, 100.0) < fog(z_near, 100.0));
        assert_eq!(fog(z_near, 100.0), 1.0);
    }

    #[test]
    fn he_knows_his_cores_by_name() {
        for (word, style) in [("simple", CoreStyle::Simple), ("Rings", CoreStyle::Simple), ("complex", CoreStyle::Complex), ("Age of Ultron", CoreStyle::Complex), ("komplexen", CoreStyle::Complex)] {
            assert_eq!(CoreStyle::of_word(word), Some(style), "{word}");
        }
        assert_eq!(CoreStyle::of_word("louder"), None);
        assert_eq!(CoreStyle::default().name(), "simple");
    }
}
