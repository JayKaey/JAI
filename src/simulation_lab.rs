//! The Stark Simulation Lab: physics problems, functions and algorithms as live, interactive
//! simulations, each in its own native window (drawn by `gui_simlab.rs` at 60 FPS).
//!
//! * Math: several functions at once (`f(x)=x^2-4; g(x)=sin(2x)`), with their derivatives,
//!   integrals (area), roots and extrema.
//! * Physics: projectile motion (with drag), the nonlinear pendulum (with damping), 1-D
//!   collisions (any elasticity) and two-source wave interference.
//! * Algorithms / ML: gradient descent on a loss curve (learning rate!) and sorting algorithms.
//!
//! Every simulation has parameter sliders (gravity, angle, velocity, frequency, learning rate …)
//! that Sir drags with the mouse — or changes by voice: "set the angle to 60 degrees and double
//! the velocity" (`adjust_simulation`, also on the instant fast path).

use crate::simulation_physics as phys;
use crate::tools::str_arg;
use serde_json::{json, Value};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};

// ------------------------------------------------------------------ expressions ----

#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    Num(f64),
    X,
    Param(String),
    Neg(Box<Expr>),
    Bin(char, Box<Expr>, Box<Expr>),
    Call(String, Box<Expr>),
}

impl Expr {
    /// Evaluates at `x`; unknown names are parameters (looked up in `params`, default 1).
    pub fn eval(&self, x: f64, params: &[Param]) -> f64 {
        match self {
            Expr::Num(v) => *v,
            Expr::X => x,
            Expr::Param(n) => params.iter().find(|p| p.name == *n).map(|p| p.value).unwrap_or(1.0),
            Expr::Neg(e) => -e.eval(x, params),
            Expr::Bin(op, a, b) => {
                let (a, b) = (a.eval(x, params), b.eval(x, params));
                match op {
                    '+' => a + b,
                    '-' => a - b,
                    '*' => a * b,
                    '/' => a / b,
                    '^' => a.powf(b),
                    _ => f64::NAN,
                }
            }
            Expr::Call(f, a) => {
                let v = a.eval(x, params);
                match f.as_str() {
                    "sin" => v.sin(),
                    "cos" => v.cos(),
                    "tan" => v.tan(),
                    "asin" | "arcsin" => v.asin(),
                    "acos" | "arccos" => v.acos(),
                    "atan" | "arctan" => v.atan(),
                    "sinh" => v.sinh(),
                    "cosh" => v.cosh(),
                    "tanh" => v.tanh(),
                    "exp" => v.exp(),
                    "ln" => v.ln(),
                    "log" | "lg" => v.log10(),
                    "log2" => v.log2(),
                    "sqrt" => v.sqrt(),
                    "abs" => v.abs(),
                    "floor" => v.floor(),
                    "ceil" => v.ceil(),
                    "sign" | "sgn" => v.signum(),
                    "sigmoid" => 1.0 / (1.0 + (-v).exp()),
                    "relu" => v.max(0.0),
                    _ => f64::NAN,
                }
            }
        }
    }

    /// Parameter names used by the expression.
    pub fn params(&self, out: &mut Vec<String>) {
        match self {
            Expr::Param(n) if !out.contains(n) => out.push(n.clone()),
            Expr::Neg(e) | Expr::Call(_, e) => e.params(out),
            Expr::Bin(_, a, b) => {
                a.params(out);
                b.params(out);
            }
            _ => {}
        }
    }
}

const FUNCTIONS: &[&str] = &[
    "sin", "cos", "tan", "asin", "acos", "atan", "arcsin", "arccos", "arctan", "sinh", "cosh", "tanh", "exp", "ln", "log", "lg", "log2", "sqrt",
    "abs", "floor", "ceil", "sign", "sgn", "sigmoid", "relu",
];

struct Parser {
    tokens: Vec<Token>,
    pos: usize,
}

#[derive(Debug, Clone, PartialEq)]
enum Token {
    Num(f64),
    Ident(String),
    Op(char),
    Open,
    Close,
}

fn tokenize(s: &str) -> Result<Vec<Token>, String> {
    let chars: Vec<char> = s.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() {
            i += 1;
        } else if c.is_ascii_digit() || (c == '.' && chars.get(i + 1).is_some_and(|d| d.is_ascii_digit())) {
            let start = i;
            while i < chars.len() && (chars[i].is_ascii_digit() || chars[i] == '.') {
                i += 1;
            }
            if i < chars.len() && (chars[i] == 'e' || chars[i] == 'E') && chars.get(i + 1).is_some_and(|d| d.is_ascii_digit() || *d == '-') {
                i += 2;
                while i < chars.len() && chars[i].is_ascii_digit() {
                    i += 1;
                }
            }
            let text: String = chars[start..i].iter().collect();
            out.push(Token::Num(text.parse().map_err(|_| format!("bad number '{text}'"))?));
        } else if c.is_alphabetic() || c == '_' || c == 'π' {
            let start = i;
            // Letters and ASCII digits only: "x²" is x squared, not a name.
            while i < chars.len() && (chars[i].is_alphabetic() || chars[i].is_ascii_digit() || chars[i] == '_' || chars[i] == 'π') {
                i += 1;
            }
            let word: String = chars[start..i].iter().collect::<String>().to_lowercase();
            // "2xsin" style runs: split known function names and single variables.
            out.extend(split_word(&word).into_iter().map(Token::Ident));
        } else if matches!(c, '+' | '-' | '*' | '/' | '^' | '·' | '×' | '÷') {
            out.push(Token::Op(match c {
                '·' | '×' => '*',
                '÷' => '/',
                o => o,
            }));
            i += 1;
        } else if c == '(' || c == '[' {
            out.push(Token::Open);
            i += 1;
        } else if c == ')' || c == ']' {
            out.push(Token::Close);
            i += 1;
        } else if c == '²' {
            out.extend([Token::Op('^'), Token::Num(2.0)]);
            i += 1;
        } else if c == '³' {
            out.extend([Token::Op('^'), Token::Num(3.0)]);
            i += 1;
        } else {
            return Err(format!("unexpected '{c}'"));
        }
    }
    Ok(out)
}

fn split_word(word: &str) -> Vec<String> {
    if FUNCTIONS.contains(&word) || word.len() <= 1 || matches!(word, "pi" | "π" | "e" | "tau") {
        return vec![word.to_string()];
    }
    // "xsin" → x, sin; "ab" stays a parameter name only if it is not made of known pieces.
    let mut out = Vec::new();
    let mut rest = word;
    'outer: while !rest.is_empty() {
        for f in FUNCTIONS.iter().copied().chain(["pi"]) {
            if let Some(r) = rest.strip_prefix(f) {
                out.push(f.to_string());
                rest = r;
                continue 'outer;
            }
        }
        // ("xy" is x times y, the surface's two variables.)
        if let Some(c) = rest.chars().next().filter(|c| matches!(c, 'x' | 'y')) {
            out.push(c.to_string());
            rest = &rest[1..];
            continue;
        }
        return vec![word.to_string()];
    }
    out
}

impl Parser {
    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.pos)
    }

    fn next(&mut self) -> Option<Token> {
        let t = self.tokens.get(self.pos).cloned();
        self.pos += 1;
        t
    }

    fn expr(&mut self) -> Result<Expr, String> {
        let mut left = self.term()?;
        while let Some(Token::Op(op)) = self.peek().cloned() {
            if op != '+' && op != '-' {
                break;
            }
            self.pos += 1;
            let right = self.term()?;
            left = Expr::Bin(op, Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn term(&mut self) -> Result<Expr, String> {
        let mut left = self.unary()?;
        loop {
            match self.peek().cloned() {
                Some(Token::Op(op)) if op == '*' || op == '/' => {
                    self.pos += 1;
                    let right = self.unary()?;
                    left = Expr::Bin(op, Box::new(left), Box::new(right));
                }
                // Implicit multiplication: "2x", "3sin(x)", "(x+1)(x-1)".
                Some(Token::Num(_)) | Some(Token::Ident(_)) | Some(Token::Open) => {
                    let right = self.power()?;
                    left = Expr::Bin('*', Box::new(left), Box::new(right));
                }
                _ => return Ok(left),
            }
        }
    }

    fn unary(&mut self) -> Result<Expr, String> {
        match self.peek() {
            Some(Token::Op('-')) => {
                self.pos += 1;
                Ok(Expr::Neg(Box::new(self.unary()?)))
            }
            Some(Token::Op('+')) => {
                self.pos += 1;
                self.unary()
            }
            _ => self.power(),
        }
    }

    fn power(&mut self) -> Result<Expr, String> {
        let base = self.atom()?;
        if let Some(Token::Op('^')) = self.peek() {
            self.pos += 1;
            let exponent = self.unary()?;
            return Ok(Expr::Bin('^', Box::new(base), Box::new(exponent)));
        }
        Ok(base)
    }

    fn atom(&mut self) -> Result<Expr, String> {
        match self.next() {
            Some(Token::Num(v)) => Ok(Expr::Num(v)),
            Some(Token::Open) => {
                let e = self.expr()?;
                match self.next() {
                    Some(Token::Close) => Ok(e),
                    _ => Err("missing ')'".into()),
                }
            }
            Some(Token::Ident(name)) => match name.as_str() {
                "x" => Ok(Expr::X),
                "pi" | "π" => Ok(Expr::Num(std::f64::consts::PI)),
                "tau" => Ok(Expr::Num(std::f64::consts::TAU)),
                "e" => Ok(Expr::Num(std::f64::consts::E)),
                f if FUNCTIONS.contains(&f) => {
                    let arg = if let Some(Token::Open) = self.peek() { self.atom()? } else { self.power()? };
                    Ok(Expr::Call(f.to_string(), Box::new(arg)))
                }
                other => Ok(Expr::Param(other.to_string())),
            },
            Some(t) => Err(format!("unexpected {t:?}")),
            None => Err("the formula ended too early".into()),
        }
    }
}

/// Parses "x^2 - 4", "f(x) = 3sin(2x)", "y=e^-x²".
pub fn parse_expr(text: &str) -> Result<Expr, String> {
    let body = match text.split_once('=') {
        Some((lhs, rhs)) if lhs.trim().len() <= 6 => rhs,
        _ => text,
    };
    let tokens = tokenize(body.trim())?;
    if tokens.is_empty() {
        return Err("empty formula".into());
    }
    let mut p = Parser { tokens, pos: 0 };
    let e = p.expr()?;
    if p.pos < p.tokens.len() {
        return Err(format!("could not read the formula after token {}", p.pos));
    }
    Ok(e)
}

/// Central-difference derivative.
pub fn derivative(e: &Expr, x: f64, params: &[Param]) -> f64 {
    let h = 1e-4 * (1.0 + x.abs());
    (e.eval(x + h, params) - e.eval(x - h, params)) / (2.0 * h)
}

/// Simpson's rule over [a, b].
pub fn integral(e: &Expr, a: f64, b: f64, params: &[Param]) -> f64 {
    let n = 400;
    let h = (b - a) / n as f64;
    let mut sum = e.eval(a, params) + e.eval(b, params);
    for i in 1..n {
        let x = a + i as f64 * h;
        let v = e.eval(x, params);
        if v.is_finite() {
            sum += v * if i % 2 == 1 { 4.0 } else { 2.0 };
        }
    }
    sum * h / 3.0
}

/// Roots in [a, b] (sign changes refined by bisection).
pub fn roots(e: &Expr, a: f64, b: f64, params: &[Param]) -> Vec<f64> {
    zeros(|x| e.eval(x, params), a, b)
}

/// Where f′ changes sign in [a, b]: the turning points (x, f(x), whether a maximum).
pub fn turning_points(e: &Expr, a: f64, b: f64, params: &[Param]) -> Vec<(f64, f64, bool)> {
    zeros(|x| derivative(e, x, params), a, b)
        .into_iter()
        .filter_map(|x| {
            let (y, h) = (e.eval(x, params), (b - a) * 1e-3);
            let (l, r) = (e.eval(x - h, params), e.eval(x + h, params));
            // (A pole's sign change is none: both sides are finite and on one side of it.)
            (y.is_finite() && l.is_finite() && r.is_finite() && (l - y).signum() == (r - y).signum() && y.abs() < 1e9).then_some((x, y, l < y))
        })
        .collect()
}

/// Where two curves meet in [a, b]: (x, y).
pub fn crossings(f: &Expr, g: &Expr, a: f64, b: f64, params: &[Param]) -> Vec<(f64, f64)> {
    zeros(|x| f.eval(x, params) - g.eval(x, params), a, b).into_iter().map(|x| (x, f.eval(x, params))).filter(|(_, y)| y.is_finite()).collect()
}

/// The zeros of `f` in [a, b] (sign changes refined by bisection).
fn zeros(f: impl Fn(f64) -> f64, a: f64, b: f64) -> Vec<f64> {
    let n = 800;
    let step = (b - a) / n as f64;
    let mut out: Vec<f64> = Vec::new();
    let mut x0 = a;
    let mut y0 = f(x0);
    for i in 1..=n {
        let x1 = a + i as f64 * step;
        let y1 = f(x1);
        if y0.is_finite() && y1.is_finite() {
            if y0 == 0.0 {
                out.push(x0);
            } else if y0.signum() != y1.signum() && (y1 - y0).abs() < 1e6 {
                let (mut lo, mut hi) = (x0, x1);
                for _ in 0..60 {
                    let mid = 0.5 * (lo + hi);
                    if f(mid).signum() == f(lo).signum() {
                        lo = mid;
                    } else {
                        hi = mid;
                    }
                }
                out.push(0.5 * (lo + hi));
            }
        }
        x0 = x1;
        y0 = y1;
    }
    out.dedup_by(|a, b| (*a - *b).abs() < step * 2.0);
    out
}

// ------------------------------------------------------------------- simulations ----

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SimKind {
    Function,
    Projectile,
    Pendulum,
    Collision,
    Waves,
    GradientDescent,
    Sorting,
    /// Bodies pulling on each other (the solar system, a binary star, a scene in words).
    Gravity,
    /// A ripple tank, with slits.
    Ripples,
    StandingWave,
    Doppler,
    Sound,
    /// Light through a double slit.
    Light,
    Spring,
    DoublePendulum,
    Balls,
    /// z = f(x, y) as a 3-D mesh.
    Surface,
}

impl SimKind {
    /// The kinds with a 3D view: the orbits, the ripple tank and surfaces.
    pub fn turns_3d(self) -> bool {
        matches!(self, Self::Gravity | Self::Ripples | Self::Surface)
    }

    /// The kind a sentence or a `sim_type` names (English and German). The specific wave kinds
    /// come before plain "wave interference".
    pub fn parse(s: &str) -> Option<Self> {
        let s = s.to_lowercase();
        let has = |w: &[&str]| w.iter().any(|x| s.contains(x));
        let words: Vec<&str> = s.split(|c: char| !c.is_alphanumeric()).collect();
        let word = |w: &[&str]| words.iter().any(|x| w.contains(x));
        Some(if has(&["projectile", "trajectory", "throw", "ballistic", "cannon", "wurf", "flugbahn"]) {
            Self::Projectile
        } else if has(&["double pendulum", "doppelpendel", "chaotic pendulum", "chaos pendulum", "double_pendulum"]) {
            Self::DoublePendulum
        } else if has(&["pendulum", "pendel", "swing"]) && !has(&["federpendel", "spring pendulum"]) {
            Self::Pendulum
        } else if has(&["bouncing ball", "balls in a box", "balls bouncing", "billiard", "gas in a box", "springende bälle", "bälle in", "balls"]) && !has(&["collision", "collide"]) {
            Self::Balls
        } else if has(&["collision", "collide", "stoß", "stoss", "impact", "momentum"]) {
            Self::Collision
        } else if has(&["spring", "oscillator", "resonance", "federpendel", "schwingung"]) || word(&["feder", "federn"]) {
            Self::Spring
        } else if has(&["gravity", "orbit", "solar system", "planet", "comet", "n-body", "nbody", "kepler", "satellite", "binary star", "three body", "three-body", "figure eight", "schwerkraft", "umlaufbahn", "sonnensystem", "gravitation", "komet", "doppelstern"])
            || word(&["sun", "sonne", "moon", "mond", "earth", "erde"])
        {
            Self::Gravity
        } else if has(&["doppler", "mach cone", "mach number", "machkegel", "sonic boom", "supersonic", "überschall", "schallmauer", "ambulance", "siren"]) {
            Self::Doppler
        } else if has(&["standing wave", "standing_wave", "stationary wave", "stehende welle", "vibrating string", "guitar string", "overtone", "oberton", "harmonics", "harmonic"]) || word(&["saite"]) {
            Self::StandingWave
        } else if has(&["ripple", "wellenwanne", "water wave", "wasserwelle", "single slit", "single-slit", "einzelspalt"]) {
            Self::Ripples
        } else if has(&["double slit", "double-slit", "doppelspalt", "young's", "light wave", "lichtwelle", "diffraction", "beugung", "slit"]) || word(&["light", "licht", "laser"]) {
            Self::Light
        } else if has(&["sound", "schallwelle", "acoustic", "akustik", "longitudinal", "pressure wave"]) || word(&["schall"]) {
            Self::Sound
        } else if has(&["wave", "interference", "welle"]) {
            Self::Waves
        } else if has(&["gradient", "descent", "learning", "ml", "optimi", "loss"]) {
            Self::GradientDescent
        } else if has(&["sort", "algorithm", "bubble", "quick", "insertion", "merge", "sortier"]) {
            Self::Sorting
        } else if has(&["surface", "3d", "3-d", "paraboloid", "saddle", "sattel", "oberfläche", "fläche", "f(x,y)", "f(x, y)", "z=", "z ="]) {
            Self::Surface
        } else if has(&["function", "plot", "graph", "math", "derivative", "integral", "funktion", "f(x)", "curve", "parabola"]) {
            Self::Function
        } else {
            return None;
        })
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Function => "Function plotter",
            Self::Projectile => "Projectile motion",
            Self::Pendulum => "Pendulum",
            Self::Collision => "1-D collision",
            Self::Waves => "Wave interference",
            Self::GradientDescent => "Gradient descent",
            Self::Sorting => "Sorting algorithm",
            Self::Gravity => "Gravity and orbits",
            Self::Ripples => "Ripple tank",
            Self::StandingWave => "Standing wave",
            Self::Doppler => "Doppler effect",
            Self::Sound => "Sound wave",
            Self::Light => "Light through a double slit",
            Self::Spring => "Mass on a spring",
            Self::DoublePendulum => "Double pendulum",
            Self::Balls => "Bouncing balls",
            Self::Surface => "3-D surface",
        }
    }

    /// The `sim_type` the tool takes for it.
    pub fn id(self) -> &'static str {
        match self {
            Self::Function => "function",
            Self::Projectile => "projectile",
            Self::Pendulum => "pendulum",
            Self::Collision => "collision",
            Self::Waves => "waves",
            Self::GradientDescent => "gradient_descent",
            Self::Sorting => "sorting",
            Self::Gravity => "gravity",
            Self::Ripples => "ripples",
            Self::StandingWave => "standing_wave",
            Self::Doppler => "doppler",
            Self::Sound => "sound",
            Self::Light => "light",
            Self::Spring => "spring",
            Self::DoublePendulum => "double_pendulum",
            Self::Balls => "balls",
            Self::Surface => "surface",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Param {
    pub name: String,
    pub label: String,
    pub value: f64,
    pub min: f64,
    pub max: f64,
    pub unit: String,
}

fn param(name: &str, label: &str, value: f64, min: f64, max: f64, unit: &str) -> Param {
    Param { name: name.into(), label: label.into(), value, min, max, unit: unit.into() }
}

#[derive(Debug, Clone)]
pub struct Simulation {
    pub id: String,
    pub title: String,
    pub kind: SimKind,
    pub params: Vec<Param>,
    /// (label, source, parsed) of the plotted functions (function plots, the loss curve).
    pub functions: Vec<(String, String, Expr)>,
    pub show_derivative: bool,
    pub show_integral: bool,
    pub x_range: (f64, f64),
    /// Sorting: bubble, insertion, selection or quick.
    pub algorithm: String,
    /// Gravity: the scene as named (a preset's words, the bodies named, or a JSON list).
    pub scene: String,
    /// Bumped on every change: the window restarts its animation.
    pub revision: u64,
    pub paused: bool,
    /// Seen in 3D (`SimKind::turns_3d`): the orbits' plane leaning back, the tank as a relief, a
    /// surface from the side; in 2D from straight above.
    pub three_d: bool,
    /// A plot: the part of the plane on screen (x from, x to, y from, y to) as Sir dragged and
    /// zoomed it or JAI set it; None: the curves' own (`plane`).
    pub view: Option<[f64; 4]>,
    /// A plot: named points, as in a geometry program ("A", 2, 3).
    pub points: Vec<(String, f64, f64)>,
}

/// The names plots give their curves, in turn.
const CURVE_NAMES: [&str; 6] = ["f", "g", "h", "p", "q", "r"];

impl Simulation {
    /// The part of the plane a plot shows: Sir's or JAI's, else x over `x_range` and y over the
    /// curves' 2nd-98th percentile with a tenth to spare (an asymptote flattens nothing).
    pub fn plane(&self) -> [f64; 4] {
        if let Some(view) = self.view {
            return view;
        }
        let (a, b) = self.x_range;
        let xs = (0..=200).map(|i| a + (b - a) * i as f64 / 200.0);
        let mut ys: Vec<f64> = self
            .functions
            .iter()
            .flat_map(|(_, _, e)| xs.clone().flat_map(move |x| [Some(e.eval(x, &self.params)), self.show_derivative.then(|| derivative(e, x, &self.params))]))
            .flatten()
            .chain(self.points.iter().map(|p| p.2))
            .filter(|y| y.is_finite())
            .collect();
        ys.sort_by(f64::total_cmp);
        let (mut y0, mut y1) = if ys.is_empty() { (-1.0, 1.0) } else { (ys[ys.len() * 2 / 100], ys[(ys.len() * 98 / 100).min(ys.len() - 1)]) };
        if (y1 - y0).abs() < 1e-6 {
            y0 -= 1.0;
            y1 += 1.0;
        }
        let pad = (y1 - y0) * 0.1;
        [a, b, y0 - pad, y1 + pad]
    }

    /// The next free name for a curve ("g" after "f"), or the one its formula gives itself
    /// ("h(x) = …").
    fn curve_name(&self, src: &str) -> String {
        let own = src.split_once('=').and_then(|(lhs, _)| lhs.trim().strip_suffix("(x)")).map(str::trim).filter(|n| (1..=3).contains(&n.len()) && n.chars().all(char::is_alphabetic));
        let free = || CURVE_NAMES.iter().find(|n| !self.functions.iter().any(|(l, _, _)| l == *n)).map_or("y", |n| *n);
        own.unwrap_or_else(free).to_string()
    }

    pub fn value(&self, name: &str) -> f64 {
        self.params.iter().find(|p| p.name == name).map(|p| p.value).unwrap_or(0.0)
    }

    /// The gravity scene (the solar system when the scene is not understood).
    pub fn bodies(&self) -> phys::Bodies {
        gravity_scene(&self.scene).or_else(|| phys::Bodies::preset("solar system")).expect("the solar system preset exists")
    }

    pub fn ripples(&self) -> phys::Ripples {
        phys::Ripples::new(160, 96, self.value("frequency"), self.value("slits").round() as usize, self.value("slit_width").round() as usize, self.value("separation").round() as usize, self.value("plane") >= 0.5)
    }

    pub fn spring(&self) -> phys::Spring {
        phys::Spring { mass: self.value("mass"), k: self.value("k"), c: self.value("damping"), force: self.value("force"), drive: self.value("drive"), x: self.value("start"), v: 0.0, t: 0.0 }
    }

    pub fn double_pendulum(&self) -> phys::DoublePendulum {
        phys::DoublePendulum {
            l1: self.value("length1"),
            l2: self.value("length2"),
            m1: self.value("mass1"),
            m2: self.value("mass2"),
            g: self.value("gravity"),
            a1: self.value("angle1").to_radians(),
            a2: self.value("angle2").to_radians(),
            w1: 0.0,
            w2: 0.0,
        }
    }

    /// The box (10 m × 6 m) and its balls, always placed the same way.
    pub fn balls(&self) -> phys::Balls {
        phys::Balls::new(self.value("count").round() as usize, 10.0, 6.0, self.value("gravity"), self.value("elasticity"), self.value("radius"), 7)
    }

    /// The sliders plus a trailing `y` (the surface's second variable, set by `surface_z`).
    pub fn surface_params(&self) -> Vec<Param> {
        let mut ps = self.params.clone();
        ps.push(param("y", "y", 0.0, f64::MIN, f64::MAX, ""));
        ps
    }

    /// z = f(x, y) of a surface; `ps` comes from `surface_params`.
    pub fn surface_z(&self, ps: &mut [Param], x: f64, y: f64) -> f64 {
        if let Some(p) = ps.last_mut() {
            p.value = y;
        }
        self.functions.first().map_or(f64::NAN, |(_, _, e)| e.eval(x, ps))
    }

    /// Numbers worth reading out (range, period, roots …).
    pub fn readout(&self) -> String {
        match self.kind {
            SimKind::Projectile => {
                let (v, a, g, h) = (self.value("velocity"), self.value("angle").to_radians(), self.value("gravity").max(0.01), self.value("height"));
                let (vx, vy) = (v * a.cos(), v * a.sin());
                let t = (vy + (vy * vy + 2.0 * g * h).sqrt()) / g;
                format!("flight time {:.2} s, range {:.1} m, peak height {:.1} m (without drag)", t, vx * t, h + vy * vy / (2.0 * g))
            }
            SimKind::Pendulum => {
                let (l, g) = (self.value("length").max(0.01), self.value("gravity").max(0.01));
                format!("small-angle period {:.2} s (2π√(L/g))", std::f64::consts::TAU * (l / g).sqrt())
            }
            SimKind::Collision => {
                let (m1, m2, v1, v2, e) = (self.value("mass1"), self.value("mass2"), self.value("velocity1"), self.value("velocity2"), self.value("elasticity"));
                let (u1, u2) = collide(m1, m2, v1, v2, e);
                format!(
                    "after: v1 = {u1:.2} m/s, v2 = {u2:.2} m/s; momentum {:.2} → {:.2} kg·m/s; kinetic energy {:.2} → {:.2} J",
                    m1 * v1 + m2 * v2,
                    m1 * u1 + m2 * u2,
                    0.5 * m1 * v1 * v1 + 0.5 * m2 * v2 * v2,
                    0.5 * m1 * u1 * u1 + 0.5 * m2 * u2 * u2
                )
            }
            SimKind::Waves => {
                let (f, lambda) = (self.value("frequency"), self.value("wavelength"));
                format!("wave speed {:.2} m/s (f·λ); maxima where the path difference is a whole number of wavelengths", f * lambda)
            }
            SimKind::GradientDescent => match self.functions.first() {
                Some((_, src, e)) => {
                    let (mut x, lr) = (self.value("start"), self.value("learning_rate"));
                    for _ in 0..self.value("steps").max(1.0) as usize {
                        x -= lr * derivative(e, x, &self.params);
                        if !x.is_finite() || x.abs() > 1e6 {
                            return format!("loss {src}: diverges with learning rate {lr} (too large)");
                        }
                    }
                    format!("loss {src}: after {} steps x = {x:.4}, loss = {:.4}", self.value("steps") as i64, e.eval(x, &self.params))
                }
                None => String::new(),
            },
            SimKind::Sorting => format!("{} sort of {} values", self.algorithm, self.value("count") as i64),
            SimKind::Gravity => {
                let s = self.bodies();
                let Some(hi) = (0..s.bodies.len()).max_by(|a, b| s.bodies[*a].mass.total_cmp(&s.bodies[*b].mass)) else { return String::new() };
                let heavy = &s.bodies[hi];
                let mut orbits: Vec<(f64, &str)> = s
                    .bodies
                    .iter()
                    .enumerate()
                    .filter(|(i, _)| *i != hi)
                    .map(|(_, b)| {
                        // The semi-major axis from the energy (vis-viva): 1/a = 2/r − v²/GM.
                        let (mu, r) = (s.g * (heavy.mass + b.mass), ((b.pos[0] - heavy.pos[0]).powi(2) + (b.pos[1] - heavy.pos[1]).powi(2)).sqrt());
                        let v2 = (b.vel[0] - heavy.vel[0]).powi(2) + (b.vel[1] - heavy.vel[1]).powi(2);
                        let axis = 1.0 / (2.0 / r.max(1e-9) - v2 / mu.max(1e-12));
                        (if axis > 0.0 { phys::kepler_period(s.g, heavy.mass + b.mass, axis) } else { f64::INFINITY }, b.name.as_str())
                    })
                    .collect();
                orbits.sort_by(|a, b| a.0.total_cmp(&b.0));
                let orbit = |(t, name): &(f64, &str)| format!("{name} orbits {} in {} {}", heavy.name, format!("{:.*}", if *t < 1.0 { 3 } else { 1 }, t), s.units.1);
                let shown = match (orbits.first(), orbits.last()) {
                    (Some(a), Some(b)) if orbits.len() > 1 => format!("{}, {}", orbit(a), orbit(b)),
                    (Some(a), _) => orbit(a),
                    _ => String::new(),
                };
                format!("{} bodies; {shown} (Kepler: T = 2π√(a³/GM)); energy {:.3e}, kept by the integrator", s.bodies.len(), s.energy())
            }
            SimKind::Ripples => {
                let (f, w, d) = (self.value("frequency"), self.value("slit_width"), self.value("separation"));
                let lambda = 1.0 / f.max(1e-6);
                let mut out = format!("wavelength {lambda:.1} cells (speed 1 cell per time unit ÷ frequency {f:.3})");
                if self.value("slits").round() >= 2.0 && lambda < d {
                    out += &format!("; bright bands where d·sin θ = m·λ, the first at {:.0}°", (lambda / d).asin().to_degrees());
                }
                if self.value("slits").round() >= 1.0 {
                    out += if w < lambda { "; the slit is narrower than a wavelength, so the waves spread out in a circle" } else { "; a slit wider than a wavelength lets a beam through" };
                }
                out
            }
            SimKind::StandingWave => {
                let (n, l, v) = (self.value("harmonic").round().max(1.0) as u32, self.value("length").max(0.01), self.value("speed"));
                format!(
                    "harmonic {n}: f = n·v/2L = {:.1} Hz, wavelength 2L/n = {:.2} m, {} nodes (the ends included) and {n} antinodes; the fundamental is {:.1} Hz",
                    phys::harmonic_frequency(n, l, v),
                    2.0 * l / n as f64,
                    n + 1,
                    phys::harmonic_frequency(1, l, v)
                )
            }
            SimKind::Doppler => {
                let (f, v, c) = (self.value("frequency"), self.value("speed"), self.value("wave_speed").max(1.0));
                let (ahead, behind) = phys::doppler(f, v, c);
                let head = match (ahead, phys::mach_angle(v, c)) {
                    (Some(a), _) => format!("heard {a:.0} Hz in front"),
                    (None, Some(angle)) => format!("faster than its own waves, so a Mach cone of half-angle {:.1}° (sin θ = c/v) and no tone in front", angle.to_degrees()),
                    (None, None) => "at the speed of its waves they pile up in front (the sound barrier)".to_string(),
                };
                format!("source {f:.0} Hz at Mach {:.2}: {head}; {behind:.0} Hz behind", v / c)
            }
            SimKind::Sound => {
                let (f, c) = (self.value("frequency").max(1.0), self.value("speed").max(1.0));
                format!("{f:.0} Hz in a medium at {c:.0} m/s: wavelength v/f = {:.3} m, period {:.2} ms; the air is pressed together a quarter wavelength ahead of its greatest displacement", c / f, 1000.0 / f)
            }
            SimKind::Light => {
                let (l, d, a, s) = (self.value("wavelength"), self.value("separation"), self.value("slit_width"), self.value("screen"));
                format!(
                    "{} light, {l:.0} nm: bright fringes {:.2} mm apart (λL/d); the single-slit envelope first goes dark at ±{:.1} mm (λL/a)",
                    phys::colour_name(l),
                    phys::fringe_spacing(l, d, s) * 1000.0,
                    phys::fringe_spacing(l, a, s) * 1000.0
                )
            }
            SimKind::Spring => {
                let sp = self.spring();
                let z = sp.damping_ratio();
                let regime = if z < 0.999 { "underdamped: it rings" } else if z < 1.001 { "critically damped: back fastest, no overshoot" } else { "overdamped: it creeps back" };
                let w0 = sp.natural();
                let mut out = format!("ω₀ = √(k/m) = {w0:.2} rad/s (f₀ {:.2} Hz, period {:.2} s); damping ratio ζ = {z:.2}, {regime}", w0 / std::f64::consts::TAU, std::f64::consts::TAU / w0.max(1e-9));
                if sp.force > 0.0 {
                    let amp = sp.force / ((sp.k - sp.mass * sp.drive.powi(2)).powi(2) + (sp.c * sp.drive).powi(2)).sqrt().max(1e-9);
                    out += &format!("; driven at {:.2} rad/s the steady swing is {amp:.3} m (largest near ω₀)", sp.drive);
                }
                out
            }
            SimKind::DoublePendulum => format!(
                "total energy {:.2} J, kept by the integrator; chaotic: a start angle changed by a hundredth of a degree gives another path within seconds",
                self.double_pendulum().energy()
            ),
            SimKind::Balls => {
                let b = self.balls();
                format!("{} balls; each bounce keeps {:.0} % of the speed; energy {:.1} J ({})", b.balls.len(), b.elasticity * 100.0, b.energy(), if b.elasticity > 0.999 { "kept" } else { "drains away" })
            }
            SimKind::Surface => {
                let Some((_, src, _)) = self.functions.first() else { return String::new() };
                let r = self.value("range");
                let mut ps = self.surface_params();
                let zs: Vec<f64> = (0..=30).flat_map(|j| (0..=30).map(move |i| (i, j))).map(|(i, j)| self.surface_z(&mut ps, -r + 2.0 * r * i as f64 / 30.0, -r + 2.0 * r * j as f64 / 30.0)).filter(|z| z.is_finite()).collect();
                let (lo, hi) = zs.iter().fold((f64::MAX, f64::MIN), |(lo, hi), z| (lo.min(*z), hi.max(*z)));
                format!("z = {src}; z(0, 0) = {:.3}; between {lo:.2} and {hi:.2} for x and y within ±{r}", self.surface_z(&mut ps, 0.0, 0.0))
            }
            SimKind::Function => {
                let [a, b, ..] = self.plane();
                let (a, b) = (fmt(a).parse().unwrap_or(a), fmt(b).parse().unwrap_or(b));
                let mut parts: Vec<String> = self
                    .functions
                    .iter()
                    .map(|(label, src, e)| {
                        let r = roots(e, a, b, &self.params);
                        let shown: Vec<String> = r.iter().take(6).map(|x| format!("{x:.3}")).collect();
                        let turns: Vec<String> = turning_points(e, a, b, &self.params).iter().take(4).map(|(x, y, max)| format!("{} ({x:.3}, {y:.3})", if *max { "max" } else { "min" })).collect();
                        format!(
                            "{label}(x) = {src}: roots {}{}; ∫ from {a} to {b} = {:.4}",
                            if shown.is_empty() { "none in range".to_string() } else { shown.join(", ") },
                            if turns.is_empty() { String::new() } else { format!("; {}", turns.join(", ")) },
                            integral(e, a, b, &self.params)
                        )
                    })
                    .collect();
                for (i, (f, _, e)) in self.functions.iter().enumerate() {
                    for (g, _, d) in &self.functions[i + 1..] {
                        let met: Vec<String> = crossings(e, d, a, b, &self.params).iter().take(4).map(|(x, y)| format!("({x:.3}, {y:.3})")).collect();
                        if !met.is_empty() {
                            parts.push(format!("{f} and {g} meet at {}", met.join(", ")));
                        }
                    }
                }
                parts.extend(self.points.iter().map(|(n, x, y)| format!("{n} = ({}, {})", fmt(*x), fmt(*y))));
                parts.join(" | ")
            }
        }
    }
}

/// 1-D collision with coefficient of restitution `e` (1 = elastic, 0 = perfectly inelastic).
pub fn collide(m1: f64, m2: f64, v1: f64, v2: f64, e: f64) -> (f64, f64) {
    let total = (m1 + m2).max(1e-9);
    let u1 = (m1 * v1 + m2 * v2 + m2 * e * (v2 - v1)) / total;
    let u2 = (m1 * v1 + m2 * v2 + m1 * e * (v1 - v2)) / total;
    (u1, u2)
}

fn defaults(kind: SimKind) -> Vec<Param> {
    match kind {
        SimKind::Projectile => vec![
            param("velocity", "Velocity", 20.0, 1.0, 120.0, "m/s"),
            param("angle", "Angle", 45.0, 0.0, 90.0, "°"),
            param("gravity", "Gravity", 9.81, 0.5, 30.0, "m/s²"),
            param("height", "Launch height", 0.0, 0.0, 100.0, "m"),
            param("drag", "Air drag", 0.0, 0.0, 0.5, "1/m"),
        ],
        SimKind::Pendulum => vec![
            param("length", "Length", 2.0, 0.1, 10.0, "m"),
            param("gravity", "Gravity", 9.81, 0.5, 30.0, "m/s²"),
            param("angle", "Start angle", 30.0, 1.0, 175.0, "°"),
            param("damping", "Damping", 0.05, 0.0, 1.5, "1/s"),
        ],
        SimKind::Collision => vec![
            param("mass1", "Mass 1", 2.0, 0.1, 20.0, "kg"),
            param("mass2", "Mass 2", 1.0, 0.1, 20.0, "kg"),
            param("velocity1", "Velocity 1", 3.0, -10.0, 10.0, "m/s"),
            param("velocity2", "Velocity 2", -1.0, -10.0, 10.0, "m/s"),
            param("elasticity", "Elasticity", 1.0, 0.0, 1.0, ""),
        ],
        SimKind::Waves => vec![
            param("frequency", "Frequency", 1.0, 0.1, 5.0, "Hz"),
            param("wavelength", "Wavelength", 1.5, 0.3, 6.0, "m"),
            param("separation", "Source separation", 4.0, 0.5, 12.0, "m"),
            param("phase", "Phase difference", 0.0, 0.0, 360.0, "°"),
        ],
        SimKind::GradientDescent => vec![
            param("learning_rate", "Learning rate", 0.1, 0.001, 1.5, ""),
            param("start", "Start x", -2.5, -6.0, 6.0, ""),
            param("steps", "Steps", 40.0, 1.0, 200.0, ""),
        ],
        SimKind::Sorting => vec![param("count", "Values", 40.0, 5.0, 150.0, ""), param("speed", "Speed", 30.0, 1.0, 400.0, "steps/s")],
        SimKind::Function => vec![param("a", "a", 1.0, -10.0, 10.0, ""), param("b", "b", 0.0, -10.0, 10.0, ""), param("c", "c", 0.0, -10.0, 10.0, "")],
        SimKind::Gravity => vec![param("speed", "Playback speed", 1.0, 0.1, 10.0, "×")],
        SimKind::Ripples => vec![
            param("frequency", "Frequency", 0.05, 0.02, 0.12, "cycles/step"),
            param("slits", "Slits in the wall", 2.0, 0.0, 2.0, ""),
            param("slit_width", "Slit width", 6.0, 1.0, 20.0, "cells"),
            param("separation", "Slit separation", 24.0, 8.0, 60.0, "cells"),
            param("plane", "Source (0 point, 1 plane)", 1.0, 0.0, 1.0, ""),
            param("speed", "Playback speed", 3.0, 1.0, 8.0, "steps/frame"),
        ],
        SimKind::StandingWave => vec![
            param("harmonic", "Harmonic", 3.0, 1.0, 8.0, ""),
            param("length", "String length", 2.0, 0.5, 5.0, "m"),
            param("speed", "Wave speed", 40.0, 5.0, 300.0, "m/s"),
            param("amplitude", "Amplitude", 0.8, 0.1, 1.0, ""),
            param("slowmo", "Slow motion", 40.0, 1.0, 400.0, "×"),
        ],
        SimKind::Doppler => vec![
            param("frequency", "Source frequency", 440.0, 50.0, 2000.0, "Hz"),
            param("speed", "Source speed", 120.0, 0.0, 700.0, "m/s"),
            param("wave_speed", "Wave speed", 343.0, 100.0, 1500.0, "m/s"),
        ],
        SimKind::Sound => vec![
            param("frequency", "Frequency", 440.0, 100.0, 2000.0, "Hz"),
            param("speed", "Speed of sound", 343.0, 200.0, 1500.0, "m/s"),
            param("amplitude", "Loudness", 0.6, 0.1, 1.0, ""),
            param("slowmo", "Slow motion", 400.0, 20.0, 2000.0, "×"),
        ],
        SimKind::Light => vec![
            param("wavelength", "Wavelength", 532.0, 380.0, 750.0, "nm"),
            param("separation", "Slit separation", 100.0, 20.0, 500.0, "µm"),
            param("slit_width", "Slit width", 30.0, 5.0, 100.0, "µm"),
            param("screen", "Screen distance", 2.0, 0.5, 5.0, "m"),
        ],
        SimKind::Spring => vec![
            param("mass", "Mass", 1.0, 0.1, 10.0, "kg"),
            param("k", "Spring constant", 20.0, 1.0, 100.0, "N/m"),
            param("damping", "Damping", 0.4, 0.0, 12.0, "N·s/m"),
            param("force", "Driving force", 0.0, 0.0, 10.0, "N"),
            param("drive", "Drive frequency", 4.0, 0.2, 20.0, "rad/s"),
            param("start", "Start stretch", 0.5, -1.0, 1.0, "m"),
        ],
        SimKind::DoublePendulum => vec![
            param("length1", "Upper rod", 1.0, 0.2, 2.0, "m"),
            param("length2", "Lower rod", 1.0, 0.2, 2.0, "m"),
            param("mass1", "Upper mass", 1.0, 0.1, 5.0, "kg"),
            param("mass2", "Lower mass", 1.0, 0.1, 5.0, "kg"),
            param("angle1", "Upper start angle", 120.0, -180.0, 180.0, "°"),
            param("angle2", "Lower start angle", 120.0, -180.0, 180.0, "°"),
            param("gravity", "Gravity", 9.81, 0.5, 30.0, "m/s²"),
        ],
        SimKind::Balls => vec![
            param("count", "Balls", 20.0, 1.0, 60.0, ""),
            param("radius", "Ball size", 0.3, 0.1, 0.6, "m"),
            param("gravity", "Gravity", 9.81, 0.0, 20.0, "m/s²"),
            param("elasticity", "Elasticity", 0.9, 0.0, 1.0, ""),
        ],
        SimKind::Surface => vec![
            param("range", "Range of x and y", 5.0, 1.0, 10.0, "±"),
            param("turn", "Turn", 40.0, 0.0, 360.0, "°"),
            param("tilt", "Tilt", 55.0, 5.0, 85.0, "°"),
            param("spin", "Spin", 8.0, 0.0, 60.0, "°/s"),
        ],
    }
}

/// A gravity scene from Sir's words: a JSON list of bodies, bodies named ("the sun, the earth
/// and a comet") or a preset ("solar system", "binary star", "figure eight").
pub fn gravity_scene(spec: &str) -> Option<phys::Bodies> {
    phys::Bodies::from_json(spec).or_else(|| phys::Bodies::from_words(spec)).or_else(|| phys::Bodies::preset(spec))
}

/// The slider a spoken or written name means: its alias, or the name itself when a slider is
/// called that ("speed" is the Doppler source's, though it also stands for a velocity).
fn slider_name(params: &[Param], raw: &str) -> String {
    let aliased = alias(raw);
    let plain = raw.trim().to_lowercase().replace([' ', '-'], "_");
    if params.iter().any(|p| p.name == aliased) || !params.iter().any(|p| p.name == plain) {
        aliased
    } else {
        plain
    }
}

/// Parameter names Sir may say, mapped to the slider they move.
fn alias(name: &str) -> String {
    let n = name.trim().to_lowercase().replace([' ', '-'], "_");
    match n.as_str() {
        "speed" | "v" | "initial_velocity" | "launch_speed" | "geschwindigkeit" | "v0" => "velocity".into(),
        "theta" | "launch_angle" | "winkel" | "degrees" => "angle".into(),
        "g" | "gravitation" | "schwerkraft" | "erdbeschleunigung" => "gravity".into(),
        "lr" | "learning" | "alpha" | "lernrate" | "step_size" => "learning_rate".into(),
        "freq" | "f" | "frequenz" => "frequency".into(),
        "lambda" | "wellenlänge" | "wellenlaenge" => "wavelength".into(),
        "l" | "länge" | "laenge" | "string_length" => "length".into(),
        "friction" | "reibung" | "dämpfung" => "damping".into(),
        "restitution" | "bounciness" | "e" | "coefficient_of_restitution" => "elasticity".into(),
        "drag_coefficient" | "air_resistance" | "luftwiderstand" => "drag".into(),
        "n" | "size" | "elements" | "anzahl" => "count".into(),
        "m1" | "mass_1" | "first_mass" => "mass1".into(),
        "m2" | "mass_2" | "second_mass" => "mass2".into(),
        "v1" | "velocity_1" => "velocity1".into(),
        "v2" | "velocity_2" => "velocity2".into(),
        "distance" | "gap" | "abstand" | "slit_distance" | "slit_spacing" | "d" => "separation".into(),
        "wavelength_nm" | "nm" => "wavelength".into(),
        "width" | "slit" | "a" | "spaltbreite" => "slit_width".into(),
        "screen_distance" | "bildschirm" => "screen".into(),
        "stiffness" | "spring_constant" | "federkonstante" => "k".into(),
        "harmonics" | "overtone" | "mode" | "oberton" => "harmonic".into(),
        "rotation" | "rotate" | "drehung" => "turn".into(),
        "extent" | "bereich" => "range".into(),
        "slow_motion" | "slowmotion" | "slow" => "slowmo".into(),
        "loudness" | "lautstärke" => "amplitude".into(),
        "driving_force" | "drive_force" => "force".into(),
        "drive_frequency" | "driving_frequency" => "drive".into(),
        "source_speed" => "speed".into(),
        "source_frequency" => "frequency".into(),
        "balls" => "count".into(),
        "angle_1" | "upper_angle" | "first_angle" => "angle1".into(),
        "angle_2" | "lower_angle" | "second_angle" => "angle2".into(),
        "length_1" | "upper_rod" => "length1".into(),
        "length_2" | "lower_rod" => "length2".into(),
        other => other.to_string(),
    }
}

/// A change: "60", "*2", "x2", "double", "half", "+10", "-5", "/2".
fn apply_change(current: f64, raw: &Value) -> Option<f64> {
    match raw {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => {
            let t = s.trim().to_lowercase().replace(['°', '%'], "");
            let t = t.trim_end_matches("degrees").trim_end_matches("grad").trim_end_matches("m/s").trim();
            match t {
                "double" | "twice" | "verdoppeln" | "doppelt" => return Some(current * 2.0),
                "triple" | "verdreifachen" => return Some(current * 3.0),
                "half" | "halve" | "halbieren" | "halb" => return Some(current / 2.0),
                "more" | "increase" | "higher" | "up" | "mehr" => return Some(current * 1.25),
                "less" | "decrease" | "lower" | "down" | "weniger" => return Some(current * 0.8),
                _ => {}
            }
            let num = |s: &str| s.trim().replace(',', ".").parse::<f64>().ok();
            if let Some(r) = t.strip_prefix('*').or_else(|| t.strip_prefix('x')).or_else(|| t.strip_prefix("times")) {
                return num(r).map(|f| current * f);
            }
            if let Some(r) = t.strip_prefix('/') {
                return num(r).filter(|f| *f != 0.0).map(|f| current / f);
            }
            if let Some(r) = t.strip_prefix("+=").or_else(|| t.strip_prefix('+')) {
                return num(r).map(|f| current + f);
            }
            if let Some(r) = t.strip_prefix("-=") {
                return num(r).map(|f| current - f);
            }
            num(t)
        }
        _ => None,
    }
}

#[derive(Clone, Default)]
pub struct SimulationLab {
    sims: Arc<RwLock<Vec<Simulation>>>,
    seq: Arc<AtomicU64>,
    revision: Arc<AtomicU64>,
}

impl SimulationLab {
    pub fn list(&self) -> Vec<Simulation> {
        self.sims.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn get(&self, id: &str) -> Option<Simulation> {
        self.list().into_iter().find(|s| s.id == id)
    }

    /// Changes anything (sliders, voice, buttons): the window redraws.
    pub fn revision(&self) -> u64 {
        self.revision.load(Ordering::Acquire)
    }

    fn edit<R>(&self, id: &str, f: impl FnOnce(&mut Simulation) -> R) -> Option<R> {
        let out = {
            let mut sims = self.sims.write().unwrap_or_else(|e| e.into_inner());
            let sim = sims.iter_mut().find(|s| s.id == id)?;
            let r = f(sim);
            sim.revision += 1;
            r
        };
        self.revision.fetch_add(1, Ordering::AcqRel);
        Some(out)
    }

    /// A simulation by id, title or kind words ("the pendulum"); "" / "it" = the newest.
    pub fn find(&self, reference: &str) -> Option<Simulation> {
        let r = reference.trim().to_lowercase();
        let list = self.list();
        if r.is_empty() || crate::focus::is_pronoun(&r) || r == "last" {
            return list.last().cloned();
        }
        list.iter()
            .rev()
            .find(|s| s.id == r || s.title.to_lowercase().contains(&r) || r.contains(&s.title.to_lowercase()))
            .or_else(|| SimKind::parse(&r).and_then(|k| list.iter().rev().find(|s| s.kind == k)))
            .cloned()
    }

    pub fn set_param(&self, id: &str, name: &str, value: f64) {
        self.edit(id, |s| {
            if let Some(p) = s.params.iter_mut().find(|p| p.name == name) {
                p.value = value.clamp(p.min, p.max);
            }
        });
    }

    /// How it is seen (paused, in 3D): the window keeps its run where it is (an `edit` restarts it).
    fn look(&self, id: &str, f: impl FnOnce(&mut Simulation)) {
        if let Some(sim) = self.sims.write().unwrap_or_else(|e| e.into_inner()).iter_mut().find(|s| s.id == id) {
            f(sim);
        }
        self.revision.fetch_add(1, Ordering::AcqRel);
    }

    pub fn toggle_pause(&self, id: &str) {
        self.look(id, |s| s.paused = !s.paused);
    }

    pub fn set_3d(&self, id: &str, on: bool) {
        self.look(id, |s| s.three_d = on);
    }

    pub fn restart(&self, id: &str) {
        self.edit(id, |_| ());
    }

    pub fn close(&self, id: &str) -> bool {
        let removed = {
            let mut sims = self.sims.write().unwrap_or_else(|e| e.into_inner());
            let before = sims.len();
            sims.retain(|s| s.id != id);
            before != sims.len()
        };
        if removed {
            self.revision.fetch_add(1, Ordering::AcqRel);
        }
        removed
    }

    pub fn set_option(&self, id: &str, derivative: Option<bool>, integral: Option<bool>) {
        self.edit(id, |s| {
            if let Some(d) = derivative {
                s.show_derivative = d;
            }
            if let Some(i) = integral {
                s.show_integral = i;
            }
        });
    }

    /// Where a plot looks (None: the curves' own view again); its run goes on. A view that is
    /// no rectangle is let be.
    pub fn set_view(&self, id: &str, view: Option<[f64; 4]>) {
        if view.is_none_or(|v| v.iter().all(|x| x.is_finite()) && v[1] > v[0] && v[3] > v[2]) {
            self.look(id, |s| s.view = view);
        }
    }

    /// A point of a plot, named ("A") or given the next free capital; the same name moves it.
    pub fn put_point(&self, id: &str, name: Option<&str>, x: f64, y: f64) -> Option<String> {
        self.edit(id, |s| {
            let name = name.map(str::to_string).unwrap_or_else(|| ('A'..='Z').map(String::from).find(|n| !s.points.iter().any(|(p, _, _)| p == n)).unwrap_or_else(|| "P".into()));
            s.points.retain(|(p, _, _)| *p != name);
            s.points.push((name.clone(), x, y));
            name
        })
    }

    /// What Sir types into a plot's input line, as in GeoGebra: "A = (2, 3)" or "(1, 4)" is a
    /// point, anything else a curve ("g(x) = sin x", "x^3").
    pub fn plot_input(&self, id: &str, text: &str) -> Result<Vec<String>, String> {
        let text = text.trim();
        let key = if !text.contains('x') && point_of(&json!(text)).is_some() { "point" } else { "function" };
        self.adjust(id, &json!({ key: text }))
    }

    /// Takes a curve (by its name or formula) or a point (by its name) off a plot: what went.
    pub fn remove_object(&self, id: &str, what: &str) -> Option<String> {
        let w = what.trim().trim_end_matches("(x)").trim().to_lowercase();
        self.edit(id, |s| {
            if let Some(i) = s.functions.iter().position(|(l, src, _)| l.to_lowercase() == w || src.to_lowercase().replace(' ', "") == w.replace(' ', "")) {
                let (label, src, _) = s.functions.remove(i);
                return Some(format!("{label}(x) = {src}"));
            }
            let i = s.points.iter().position(|(n, _, _)| n.to_lowercase() == w.trim_start_matches("point ").trim())?;
            Some(format!("the point {}", s.points.remove(i).0))
        })
        .flatten()
    }

    /// One change of a plot's own (its view, a point, a curve taken off): what it did, None
    /// when `key` is no such change.
    fn plot_change(&self, sim: &Simulation, key: &str, value: &Value, plane: &mut Option<Option<[f64; 4]>>) -> Result<Option<String>, String> {
        let key = key.trim().to_lowercase().replace([' ', '-'], "_");
        let mut v = plane.flatten().unwrap_or_else(|| sim.plane());
        let number = || value.as_f64().or_else(|| value.as_str().and_then(|s| s.trim().replace('−', "-").parse().ok())).ok_or_else(|| format!("'{value}' is no number"));
        match key.as_str() {
            "view" | "reset_view" | "fit" => {
                *plane = Some(None);
                return Ok(Some("the view fits the curves again".into()));
            }
            "point" | "add_point" => {
                let (name, x, y) = point_of(value).ok_or_else(|| format!("I couldn't read the point '{value}' (\"A = (2, 3)\")."))?;
                let name = self.put_point(&sim.id, name.as_deref(), x, y).unwrap_or_default();
                return Ok(Some(format!("{name} = ({}, {})", fmt(x), fmt(y))));
            }
            "remove" | "delete" | "remove_function" | "remove_point" => {
                let what = value.as_str().unwrap_or_default();
                return self.remove_object(&sim.id, what).map(|gone| Some(format!("removed {gone}"))).ok_or_else(|| format!("This plot has no curve or point called '{what}'."));
            }
            "zoom" => {
                let k = match value.as_str().map(str::trim) {
                    Some("in" | "rein" | "hinein") => 2.0,
                    Some("out" | "raus" | "heraus") => 0.5,
                    _ => number()?,
                };
                v = zoomed(v, k, ((v[0] + v[1]) / 2.0, (v[2] + v[3]) / 2.0));
            }
            "x_range" | "x_axis" | "x" | "y_range" | "y_axis" | "y" => {
                let (a, b) = two_numbers(value).ok_or_else(|| format!("I couldn't read the range '{value}' (\"-5 to 5\")."))?;
                let at = if key.starts_with('x') { 0 } else { 2 };
                (v[at], v[at + 1]) = (a.min(b), a.max(b));
            }
            "x_min" => v[0] = number()?,
            "x_max" => v[1] = number()?,
            "y_min" => v[2] = number()?,
            "y_max" => v[3] = number()?,
            _ => return Ok(None),
        }
        if !(v[1] > v[0] && v[3] > v[2]) {
            return Err("The axes must run from a smaller number to a larger one.".into());
        }
        *plane = Some(Some(v));
        Ok(Some(format!("x from {} to {}, y from {} to {}", fmt(v[0]), fmt(v[1]), fmt(v[2]), fmt(v[3]))))
    }

    /// Opens a new simulation; returns its id.
    pub fn launch(&self, kind: SimKind, spec: &str, title: Option<&str>) -> Result<Simulation, String> {
        if self.list().len() >= 6 {
            return Err("Six simulations are already open; close one first.".into());
        }
        let n = self.seq.fetch_add(1, Ordering::AcqRel) + 1;
        let mut sim = Simulation {
            id: format!("sim_{n}"),
            title: title.filter(|t| !t.trim().is_empty()).map(str::to_string).unwrap_or_else(|| kind.label().to_string()),
            kind,
            params: defaults(kind),
            functions: Vec::new(),
            show_derivative: false,
            show_integral: false,
            x_range: (-10.0, 10.0),
            algorithm: "bubble".into(),
            scene: "solar system".into(),
            revision: 0,
            paused: false,
            three_d: kind == SimKind::Surface || (kind.turns_3d() && crate::intents::says_3d(spec)),
            view: None,
            points: Vec::new(),
        };
        configure(&mut sim, spec)?;
        self.sims.write().unwrap_or_else(|e| e.into_inner()).push(sim.clone());
        self.revision.fetch_add(1, Ordering::AcqRel);
        Ok(sim)
    }

    /// Voice / tool changes: {"angle": 60, "velocity": "*2"}; returns what changed.
    pub fn adjust(&self, id: &str, changes: &Value) -> Result<Vec<String>, String> {
        let sim = self.get(id).ok_or("That simulation is closed.")?;
        let mut applied = Vec::new();
        let mut updates: Vec<(String, f64)> = Vec::new();
        let map: Vec<(String, Value)> = match changes {
            Value::Object(o) => o.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
            Value::String(s) => parse_pairs(s).into_iter().map(|(k, v)| (k, json!(v))).collect(),
            _ => Vec::new(),
        };
        // A plot's view (`plane`): Some(None) puts it back to the curves' own.
        let mut plane: Option<Option<[f64; 4]>> = None;
        for (raw_name, raw_value) in map {
            if sim.kind == SimKind::Function {
                if let Some(said) = self.plot_change(&sim, &raw_name, &raw_value, &mut plane)? {
                    applied.push(said);
                    continue;
                }
            }
            let name = slider_name(&sim.params, &raw_name);
            match name.as_str() {
                "derivative" | "integral" => {
                    let on = raw_value.as_bool().unwrap_or(!matches!(raw_value.as_str(), Some("off" | "false" | "hide" | "no")));
                    self.set_option(id, (name == "derivative").then_some(on), (name == "integral").then_some(on));
                    applied.push(format!("{name} {}", if on { "shown" } else { "hidden" }));
                    continue;
                }
                "function" | "formula" | "add_function" => {
                    if let Some(src) = raw_value.as_str() {
                        let e = parse_expr(src)?;
                        // "g(x) = cos x" keeps its name and is shown as "cos x".
                        let body = src.split_once('=').filter(|(lhs, _)| lhs.trim().len() <= 6).map_or(src, |(_, rhs)| rhs).trim();
                        self.edit(id, |s| {
                            let label = s.curve_name(src);
                            s.functions.retain(|(l, _, _)| *l != label);
                            s.functions.push((label, body.to_string(), e));
                        });
                        applied.push(format!("added {src}"));
                    }
                    continue;
                }
                _ => {}
            }
            let Some(p) = sim.params.iter().find(|p| p.name == name) else {
                return Err(format!("'{raw_name}' is not a slider here (sliders: {}).", sim.params.iter().map(|p| p.name.as_str()).collect::<Vec<_>>().join(", ")));
            };
            let current = updates.iter().find(|(n, _)| *n == name).map(|(_, v)| *v).unwrap_or(p.value);
            let next = apply_change(current, &raw_value).ok_or_else(|| format!("I couldn't read the value '{raw_value}' for {}.", p.label))?.clamp(p.min, p.max);
            updates.push((name.clone(), next));
            applied.push(format!("{} {} → {}{}", p.label.to_lowercase(), fmt(p.value), fmt(next), if p.unit.is_empty() { String::new() } else { format!(" {}", p.unit) }));
        }
        for (name, value) in updates {
            self.set_param(id, &name, value);
        }
        if let Some(view) = plane {
            self.set_view(id, view);
        }
        if applied.is_empty() {
            return Err("Tell me which slider to change and to what.".into());
        }
        Ok(applied)
    }
}

fn fmt(v: f64) -> String {
    if (v - v.round()).abs() < 1e-9 {
        format!("{}", v.round() as i64)
    } else {
        format!("{v:.3}").trim_end_matches('0').trim_end_matches('.').to_string()
    }
}

/// The plane `view` seen `k` times nearer (0.5: farther) about the point `at`; as it was when
/// that would make it absurdly small or large.
pub fn zoomed(view: [f64; 4], k: f64, at: (f64, f64)) -> [f64; 4] {
    let v = [at.0 - (at.0 - view[0]) / k, at.0 + (view[1] - at.0) / k, at.1 - (at.1 - view[2]) / k, at.1 + (view[3] - at.1) / k];
    let fine = |a: f64, b: f64| (1e-9..1e9).contains(&(b - a));
    if k.is_finite() && k > 0.0 && fine(v[0], v[1]) && fine(v[2], v[3]) { v } else { view }
}

/// Two numbers in order: "-5 to 5", "-5..5", "[-5, 5]", "(2, 3)", [2, 3].
fn two_numbers(v: &Value) -> Option<(f64, f64)> {
    if let Some([a, b]) = v.as_array().map(Vec::as_slice) {
        return Some((a.as_f64()?, b.as_f64()?));
    }
    static NUMBER: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| regex::Regex::new(r"-?\d+(?:\.\d+)?").expect("valid number regex"));
    let text = v.as_str()?.replace('−', "-");
    let mut n = NUMBER.find_iter(&text).filter_map(|m| m.as_str().parse::<f64>().ok());
    Some((n.next()?, n.next()?))
}

/// A point as said or written: "A = (2, 3)", "B(1|-4)", "(2, 3)", "point C at 1, 2", [2, 3].
fn point_of(v: &Value) -> Option<(Option<String>, f64, f64)> {
    let (x, y) = two_numbers(&match v.as_str() {
        Some(s) => json!(s.replace('|', ",")),
        None => v.clone(),
    })?;
    let name = v.as_str().and_then(|s| {
        let s = s.trim();
        let s = s.strip_prefix("point ").or_else(|| s.strip_prefix("Point ")).or_else(|| s.strip_prefix("punkt ")).unwrap_or(s).trim();
        let name: String = s.chars().take_while(|c| c.is_alphabetic()).collect();
        (name.chars().count() == 1).then(|| name.to_uppercase())
    });
    Some((name, x, y))
}

/// "angle=60, velocity 30 m/s, gravity: 1.62" → pairs.
fn parse_pairs(text: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for part in text.split([',', ';', '&']).flat_map(|p| p.split(" and ")) {
        let p = part.trim();
        let (k, v) = match p.split_once(['=', ':']) {
            Some((k, v)) => (k.trim().to_string(), v.trim().to_string()),
            None => {
                let mut words = p.split_whitespace().collect::<Vec<_>>();
                let Some(pos) = words.iter().position(|w| w.trim_end_matches(['°', '%']).replace(',', ".").parse::<f64>().is_ok()) else { continue };
                let value = words.remove(pos).to_string();
                let key = words.into_iter().filter(|w| !matches!(*w, "to" | "of" | "at" | "set" | "the" | "auf" | "mit" | "degrees" | "m/s" | "kg" | "hz")).collect::<Vec<_>>().join("_");
                (key, value)
            }
        };
        if !k.is_empty() && !v.is_empty() {
            out.push((k, v));
        }
    }
    out
}

/// Applies the creation spec: formulas and options for plots, key=value pairs for the rest.
fn configure(sim: &mut Simulation, spec: &str) -> Result<(), String> {
    let spec = spec.trim();
    if sim.kind == SimKind::Gravity {
        if gravity_scene(spec).is_some() {
            sim.scene = spec.to_string();
        }
        // (A list of bodies holds no sliders.)
        if spec.starts_with(['{', '[']) {
            return Ok(());
        }
    }
    // A JSON object of parameters.
    if let Ok(Value::Object(o)) = serde_json::from_str::<Value>(spec) {
        for (k, v) in o {
            let name = slider_name(&sim.params, &k);
            if let Some(p) = sim.params.iter_mut().find(|p| p.name == name) {
                if let Some(next) = apply_change(p.value, &v) {
                    p.value = next.clamp(p.min, p.max);
                }
            } else if name == "function" || name == "formula" {
                if let Some(src) = v.as_str() {
                    let e = parse_expr(src)?;
                    sim.functions.push(("f".into(), src.into(), e));
                }
            }
        }
        if sim.kind == SimKind::GradientDescent && sim.functions.is_empty() {
            sim.functions.push(("loss".into(), "x^4 - 3x^3 + 2".into(), parse_expr("x^4 - 3x^3 + 2")?));
        }
        if sim.kind == SimKind::Surface {
            let given = sim.functions.pop().map(|(_, src, _)| src).unwrap_or_default();
            return set_surface(sim, &given);
        }
        return Ok(());
    }
    let lower = spec.to_lowercase();
    let word = |w: &[&str]| w.iter().any(|x| lower.contains(x));
    // Words that name a setting rather than give a number (explicit numbers below win).
    let mut presets: Vec<(&str, f64)> = Vec::new();
    match sim.kind {
        SimKind::Ripples => {
            let slits = if word(&["no slit", "ohne spalt"]) { Some(0.0) } else if word(&["single", "einzel"]) { Some(1.0) } else if word(&["double", "doppel", "two slit"]) { Some(2.0) } else { None };
            presets.extend(slits.map(|s| ("slits", s)));
            if word(&["plane", "eben", "straight"]) {
                presets.push(("plane", 1.0));
            } else if word(&["point", "punkt", "circular"]) {
                presets.push(("plane", 0.0));
            }
        }
        SimKind::Light => {
            let colours = [("red", 650.0), ("rot", 650.0), ("orange", 600.0), ("yellow", 580.0), ("gelb", 580.0), ("green", 532.0), ("grün", 532.0), ("blue", 470.0), ("blau", 470.0), ("violet", 410.0), ("purple", 410.0)];
            presets.extend(colours.iter().find(|(c, _)| lower.contains(c)).map(|(_, nm)| ("wavelength", *nm)));
        }
        SimKind::Doppler if word(&["supersonic", "sonic boom", "mach cone", "überschall"]) => presets.push(("speed", 500.0)),
        SimKind::Spring if word(&["resonan"]) => {
            // Driven at its own frequency, lightly damped.
            let w0 = (sim.value("k") / sim.value("mass").max(1e-9)).sqrt();
            presets.extend([("force", 2.0), ("drive", w0), ("damping", 0.3), ("start", 0.0)]);
        }
        _ => {}
    }
    for (name, v) in presets {
        if let Some(p) = sim.params.iter_mut().find(|p| p.name == name) {
            p.value = v.clamp(p.min, p.max);
        }
    }
    match sim.kind {
        SimKind::Surface => return set_surface(sim, spec),
        SimKind::Function | SimKind::GradientDescent => {
            sim.show_derivative = lower.contains("derivative") || lower.contains("ableitung") || lower.contains("f'");
            sim.show_integral = lower.contains("integral") || lower.contains("area") || lower.contains("fläche");
            // "x from -5 to 5", "range -5..5", "[-5, 5]".
            static RANGE: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
                regex::Regex::new(r"(?:from|range|between|x\s*∈|von)\s*\[?\s*(-?\d+(?:\.\d+)?)\s*(?:to|\.\.|,|and|bis)\s*(-?\d+(?:\.\d+)?)").expect("valid range regex")
            });
            if let Some(c) = RANGE.captures(&lower) {
                if let (Ok(a), Ok(b)) = (c[1].parse::<f64>(), c[2].parse::<f64>()) {
                    if b > a {
                        sim.x_range = (a, b);
                    }
                }
            }
            let cleaned = RANGE.replace_all(&lower, "").to_string();
            let mut labels = ["f", "g", "h", "p", "q", "r"].iter();
            for piece in cleaned.split([';', '\n']).flat_map(|p| p.split(" and ")).flat_map(|p| p.split(", ")) {
                let p = piece.trim().trim_start_matches("plot").trim_start_matches("graph").trim();
                let p = ["with derivative", "with the derivative", "and derivative", "derivative", "with integral", "integral", "show", "area"]
                    .iter()
                    .fold(p.to_string(), |acc, w| acc.replace(w, ""));
                let p = p.trim().trim_matches(',').trim();
                // A formula has an operator, a call or a number ("x" alone is the axis, "show" a word).
                if p.is_empty() || !p.chars().any(|c| "+-*/^(=²³".contains(c) || c.is_ascii_digit()) {
                    continue;
                }
                if let Some(k) = p.split_once('=').map(|(k, _)| k.trim()).filter(|k| sim.params.iter().any(|q| q.name == *k)) {
                    // "a = 2" sets a slider.
                    let v = p.split_once('=').map(|(_, v)| v.trim()).unwrap_or("");
                    if let (Some(q), Ok(val)) = (sim.params.iter_mut().find(|q| q.name == k), v.parse::<f64>()) {
                        q.value = val.clamp(q.min, q.max);
                    }
                    continue;
                }
                match parse_expr(p) {
                    Ok(e) => {
                        let label = if sim.kind == SimKind::GradientDescent { "loss".to_string() } else { labels.next().copied().unwrap_or("y").to_string() };
                        sim.functions.push((label, p.split_once('=').map(|(_, r)| r.trim()).filter(|_| p.split_once('=').is_some_and(|(l, _)| l.trim().len() <= 6)).unwrap_or(p).to_string(), e));
                    }
                    Err(e) if sim.functions.is_empty() && cleaned.split([';', '\n']).count() == 1 => return Err(format!("I couldn't read the formula '{p}': {e}.")),
                    Err(_) => {}
                }
            }
            if sim.functions.is_empty() {
                let default = if sim.kind == SimKind::GradientDescent { "x^4 - 3x^3 + 2" } else { "a*x^2 + b*x + c" };
                sim.functions.push((if sim.kind == SimKind::GradientDescent { "loss" } else { "f" }.into(), default.into(), parse_expr(default)?));
            }
            // Sliders for the parameters the formulas use.
            let mut used = Vec::new();
            for (_, _, e) in &sim.functions {
                e.params(&mut used);
            }
            if sim.kind == SimKind::Function {
                sim.params.retain(|p| used.contains(&p.name));
                let missing: Vec<String> = used.iter().filter(|n| !sim.params.iter().any(|p| p.name == **n)).cloned().collect();
                for name in missing {
                    sim.params.push(param(&name, &name, 1.0, -10.0, 10.0, ""));
                }
            }
            for (k, v) in parse_pairs(&lower) {
                let k = slider_name(&sim.params, &k);
                if let (Some(q), Some(val)) = (sim.params.iter_mut().find(|q| q.name == k), v.replace(',', ".").parse::<f64>().ok()) {
                    q.value = val.clamp(q.min, q.max);
                }
            }
        }
        SimKind::Sorting => {
            for algo in ["bubble", "insertion", "selection", "quick"] {
                if lower.contains(algo) {
                    sim.algorithm = algo.into();
                }
            }
            for (k, v) in parse_pairs(&lower) {
                let k = slider_name(&sim.params, &k);
                if let (Some(q), Ok(val)) = (sim.params.iter_mut().find(|q| q.name == k), v.parse::<f64>()) {
                    q.value = val.clamp(q.min, q.max);
                }
            }
        }
        _ => {
            for (k, v) in parse_pairs(spec) {
                let k = slider_name(&sim.params, &k);
                if let Some(q) = sim.params.iter_mut().find(|q| q.name == k) {
                    if let Some(val) = apply_change(q.value, &json!(v)) {
                        q.value = val.clamp(q.min, q.max);
                    }
                }
            }
        }
    }
    Ok(())
}

/// Puts a surface's formula in: "z = x^2 - y^2" or "plot sin(x)*cos(y)"; with none named (or
/// only words), the ripple sin(√(x²+y²)). Unknown names in the formula become sliders.
fn set_surface(sim: &mut Simulation, spec: &str) -> Result<(), String> {
    static Z: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| regex::Regex::new(r"(?:\bz|f\s*\(\s*x\s*,\s*y\s*\))\s*=\s*([^,;?!\n]+)").expect("valid surface regex"));
    const LEAD: &[&str] = &[
        "plot", "a", "an", "the", "3d", "3-d", "surface", "of", "show", "me", "simulate", "graph", "draw", "function", "as", "zeig", "zeige", "mir", "eine", "die", "fläche", "oberfläche", "von", "please", "bitte", "visualize", "display", "open", "for", "in", "and", "with", "z", "paraboloid", "saddle",
    ];
    let lower = spec.to_lowercase();
    let named = Z.captures(&lower).map(|c| c[1].trim().trim_end_matches('.').trim_end_matches(" in 3d").trim_end_matches(" in 3-d").trim().to_string());
    let rest = lower.split_whitespace().filter(|w| !LEAD.contains(&w.trim_matches(|c: char| !c.is_alphanumeric() && c != '-'))).collect::<Vec<_>>().join(" ");
    let parsed = match (&named, rest.is_empty()) {
        (Some(src), _) => Some(parse_expr(src).map(|e| (src.clone(), e)).map_err(|e| format!("I couldn't read the surface '{src}': {e}."))?),
        // Words alone ("a nice hill") are no formula: only one-letter names count.
        (None, false) => parse_expr(&rest).ok().filter(|e| {
            let mut used = Vec::new();
            e.params(&mut used);
            used.iter().all(|n| n.chars().count() == 1 || n == "y")
        }).map(|e| (rest.clone(), e)),
        _ => None,
    };
    let (src, e) = match parsed {
        Some(p) => p,
        None => ("sin(sqrt(x^2 + y^2))".to_string(), parse_expr("sin(sqrt(x^2 + y^2))")?),
    };
    let mut used = Vec::new();
    e.params(&mut used);
    let missing: Vec<String> = used.into_iter().filter(|n| n != "y" && !sim.params.iter().any(|p| p.name == *n)).collect();
    for name in missing {
        sim.params.push(param(&name, &name, 1.0, -10.0, 10.0, ""));
    }
    sim.functions = vec![("z".into(), src, e)];
    Ok(())
}

/// The sequence of array states a sorting algorithm goes through (for the animation).
pub fn sort_steps(values: &[f64], algorithm: &str) -> Vec<(Vec<f64>, (usize, usize))> {
    let mut v = values.to_vec();
    let mut steps = vec![(v.clone(), (usize::MAX, usize::MAX))];
    let n = v.len();
    match algorithm {
        "insertion" => {
            for i in 1..n {
                let mut j = i;
                while j > 0 && v[j - 1] > v[j] {
                    v.swap(j - 1, j);
                    steps.push((v.clone(), (j - 1, j)));
                    j -= 1;
                }
            }
        }
        "selection" => {
            for i in 0..n {
                let min = (i..n).min_by(|a, b| v[*a].total_cmp(&v[*b])).unwrap_or(i);
                if min != i {
                    v.swap(i, min);
                    steps.push((v.clone(), (i, min)));
                }
            }
        }
        "quick" => {
            fn quick(v: &mut Vec<f64>, lo: isize, hi: isize, steps: &mut Vec<(Vec<f64>, (usize, usize))>) {
                if lo >= hi {
                    return;
                }
                let pivot = v[hi as usize];
                let mut i = lo - 1;
                for j in lo..hi {
                    if v[j as usize] <= pivot {
                        i += 1;
                        v.swap(i as usize, j as usize);
                        steps.push((v.clone(), (i as usize, j as usize)));
                    }
                }
                v.swap((i + 1) as usize, hi as usize);
                steps.push((v.clone(), ((i + 1) as usize, hi as usize)));
                quick(v, lo, i, steps);
                quick(v, i + 2, hi, steps);
            }
            let hi = n as isize - 1;
            quick(&mut v, 0, hi, &mut steps);
        }
        _ => {
            for i in 0..n {
                for j in 0..n.saturating_sub(i + 1) {
                    if v[j] > v[j + 1] {
                        v.swap(j, j + 1);
                        steps.push((v.clone(), (j, j + 1)));
                    }
                }
            }
        }
    }
    steps.push((v, (usize::MAX, usize::MAX)));
    steps
}

// ---------------------------------------------------------------------------- tools ----

pub fn is_tool(name: &str) -> bool {
    matches!(name, "launch_interactive_simulation" | "adjust_simulation")
}

pub fn tool_definitions() -> Vec<Value> {
    vec![
        json!({"type": "function", "function": {
            "name": "launch_interactive_simulation",
            "description": "Simulation Lab: a live simulation with sliders in a window of its own. sim_type function (f(x); several separated by ';'; 'derivative' / 'integral'; 'x from -5 to 5'), projectile, pendulum, collision, waves, gradient_descent, sorting (bubble / insertion / selection / quick), gravity (formula_or_params: 'the sun, the earth and a comet', 'binary star', or JSON bodies), surface (z = f(x, y)), ripples, standing_wave, doppler, sound, light (double slit), spring, double_pendulum, balls. formula_or_params: the formulas, or 'angle=60, velocity=30, gravity=9.81'.",
            "parameters": {"type": "object", "properties": {
                "sim_type": {"type": "string", "enum": ["function", "projectile", "pendulum", "collision", "waves", "gradient_descent", "sorting", "gravity", "surface", "ripples", "standing_wave", "doppler", "sound", "light", "spring", "double_pendulum", "balls"]},
                "formula_or_params": {"type": "string"},
                "title": {"type": "string"}
            }, "required": ["sim_type"]}
        }}),
        json!({"type": "function", "function": {
            "name": "adjust_simulation",
            "description": "Change an open simulation by voice: changes maps slider names to values or operations ({\"angle\": 60, \"velocity\": \"*2\"}; also \"half\", \"+10\", or \"function\": \"g(x) = sin(x)\" to add a curve, \"derivative\": true; a plot also takes \"zoom\": \"in\"/\"out\"/2, \"x_range\"/\"y_range\": \"-5 to 5\", \"view\": \"reset\", \"point\": \"A = (2, 3)\", \"remove\": \"g\"); action restart, pause, resume, close, or 3d / 2d for its view. sim omitted = the newest simulation.",
            "parameters": {"type": "object", "properties": {
                "sim": {"type": "string", "description": "id, title or kind (\"the pendulum\")"},
                "changes": {"type": "object"},
                "action": {"type": "string", "enum": ["set", "restart", "pause", "resume", "close", "3d", "2d"]}
            }, "required": []}
        }}),
    ]
}

pub fn run_tool(lab: &SimulationLab, name: &str, args: &Value) -> Result<String, String> {
    match name {
        "launch_interactive_simulation" => {
            let raw = str_arg(args, &["sim_type", "type", "kind"]).unwrap_or("function");
            let spec = str_arg(args, &["formula_or_params", "formula", "params", "parameters"]).unwrap_or("");
            let kind = SimKind::parse(raw).or_else(|| SimKind::parse(spec)).unwrap_or(SimKind::Function);
            if !crate::gui::windows_supported() {
                return Err("JAI is running without a display, so the Simulation Lab cannot open a window.".into());
            }
            let sim = lab.launch(kind, spec, str_arg(args, &["title", "name"]))?;
            crate::gui::sync_windows();
            Ok(format!(
                "Opened the {} simulation '{}' ({}) in its own window with sliders for {}. {}",
                kind.label().to_lowercase(),
                sim.title,
                sim.id,
                sim.params.iter().map(|p| p.label.to_lowercase()).collect::<Vec<_>>().join(", "),
                sim.readout()
            ))
        }
        "adjust_simulation" => {
            let reference = str_arg(args, &["sim", "id", "simulation", "sim_id"]).unwrap_or("");
            let sim = lab.find(reference).ok_or("No simulation is open, Sir.")?;
            let action = str_arg(args, &["action"]).unwrap_or("set").to_lowercase();
            let out = match action.as_str() {
                "close" | "stop" => {
                    lab.close(&sim.id);
                    format!("Closed the {} simulation.", sim.title)
                }
                "restart" | "reset" | "replay" => {
                    lab.restart(&sim.id);
                    format!("Restarted the {} simulation.", sim.title)
                }
                "pause" | "resume" | "play" => {
                    let wanted = action == "pause";
                    if sim.paused != wanted {
                        lab.toggle_pause(&sim.id);
                    }
                    format!("{} the {} simulation.", if wanted { "Paused" } else { "Resumed" }, sim.title)
                }
                "3d" | "2d" => {
                    let (deep, view) = (action == "3d", action.to_uppercase());
                    if !sim.kind.turns_3d() {
                        format!("The {} simulation stays flat, Sir: the orbits, the ripple tank and surfaces turn {view}.", sim.title)
                    } else if sim.three_d == deep {
                        format!("The {} simulation is in {view} already, Sir.", sim.title)
                    } else {
                        lab.set_3d(&sim.id, deep);
                        format!("The {} simulation in {view}, Sir.", sim.title)
                    }
                }
                _ => {
                    let changes = args.get("changes").cloned().or_else(|| str_arg(args, &["text", "change"]).map(|s| json!(s))).unwrap_or(Value::Null);
                    let applied = lab.adjust(&sim.id, &changes)?;
                    let now = lab.get(&sim.id).map(|s| s.readout()).unwrap_or_default();
                    format!("Updated the {} simulation: {}. {now}", sim.title, applied.join(", "))
                }
            };
            crate::gui::sync_windows();
            Ok(out)
        }
        other => Err(format!("Unknown simulation tool '{other}'.")),
    }
}

/// "Can you show me the simulation?", "simulate that", "plot it", "zeig mir die Flugbahn": the
/// `launch_interactive_simulation` arguments, with the kind and the numbers taken from Sir's
/// words or, failing that, from the conversation so far (`context`, oldest first). A ball
/// "thrown 5 m up" becomes a vertical launch reaching 5 m (v = √(2gh)); "from 5 m" is the launch
/// height. None when the sentence is not a request to see a simulation.
pub fn simulation_request(text: &str, context: &[String]) -> Option<Value> {
    use regex::Regex;
    use std::sync::LazyLock;
    static ASKS: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?i)\b(?:simulat\w*|simulier\w*|visuali[sz]\w*|animat\w*|plot\w*|zeichne|darstell\w*)\b|\b(?:show|zeig|zeige|display|open|launch|start|starte|run)\b.*\b(?:simulation|simulator|trajectory|flugbahn|graph|plot|animation|pendulum|pendel|surface|orbits?|solar system|double slit|doppler|ripples?|standing wave|spring|balls|(?:bubble|quick|insertion|selection)\s*sort|sorting(?:\s+algorithm)?|sortieralgorithmus)\b|\bz\s*=\s*\S").expect("valid simulation request regex")
    });
    // A sentence that points back at the conversation ("simulate that", "show me the simulation",
    // "plot it at 45 degrees"); "visualise the volcano" names a thing of its own.
    static BACK: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?i)\b(?:it|that|this|again|es|nochmal|(?:the|die|den)\s+(?:simulation|simulator|animation|graph|plot|trajectory|curve|flugbahn|kurve))\b|\bdas\s*$|\d").expect("valid reference regex")
    });
    let lower = text.to_lowercase();
    if !ASKS.is_match(&lower) || lower.contains("calendar") || lower.contains("kalender") {
        return None;
    }
    // The kind: named now, or the subject of the last messages (newest first).
    let kind_of = |s: &str| {
        let l = s.to_lowercase();
        // Physics words first: "ml" or "quick" inside ordinary words must not pick a kind.
        let words: Vec<&str> = l.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).collect();
        let physics = ["ball", "throw", "thrown", "throws", "wirft", "geworfen", "cannon", "projectile", "trajectory", "flugbahn", "wurf", "launched", "falls", "dropped"];
        if words.iter().any(|w| physics.contains(w)) {
            return Some(SimKind::Projectile);
        }
        SimKind::parse(s).filter(|k| *k != SimKind::GradientDescent || l.contains("gradient") || l.contains("descent"))
            .filter(|k| *k != SimKind::Sorting || l.contains("sort"))
    };
    // "plot it at 45 degrees": the verb alone names no function, the thing is the conversation's
    // (a formula, "x" or a function's name keeps the plotter).
    let formula = lower.contains(['=', '^']) || lower.split(|c: char| !c.is_alphanumeric()).any(|w| ["x", "sin", "cos", "tan", "log", "ln", "sqrt", "exp"].contains(&w));
    let verb_only = !formula && SimKind::parse(&lower.replace("plot", "").replace("graph", "")).is_none();
    let earlier = || context.iter().rev().take(6).find_map(|c| kind_of(c).map(|k| (k, c.as_str())));
    let (kind, kind_text) = match kind_of(text) {
        Some(SimKind::Function) if verb_only && BACK.is_match(&lower) => earlier().unwrap_or((SimKind::Function, text)),
        Some(k) => (k, text),
        None if BACK.is_match(&lower) => earlier()?,
        None => return None,
    };
    // A sun, moon or earth alone is simulated only when a simulation is asked for: "Zeichne den
    // Mond" wants a picture, "simulate the moon" its orbit.
    let gravity_words = ["gravit", "orbit", "solar", "planet", "comet", "body", "kepler", "satellit", "binary", "figure eight", "schwerkraft", "umlaufbahn", "sonnensystem", "komet", "doppelstern"];
    if kind == SimKind::Gravity && kind_text == text && !gravity_words.iter().chain(&["simul", "animat"]).any(|w| lower.contains(w)) {
        return None;
    }
    // Numbers: Sir's sentence first, then the conversation, newest first.
    let mut sources: Vec<String> = vec![lower.clone()];
    sources.extend(context.iter().rev().take(6).map(|c| c.to_lowercase()));
    let num = |re: &Regex| -> Option<f64> { sources.iter().find_map(|s| re.captures(s).and_then(|c| c.get(1)).and_then(|m| m.as_str().replace(',', ".").parse::<f64>().ok())) };
    let id = kind.id();
    let mut spec: Vec<String> = Vec::new();
    let mut title = kind.label().to_string();
    match kind {
        SimKind::Projectile => {
            static GRAVITY: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\b(?:gravity|g|schwerkraft|erdbeschleunigung)\b\s*(?:=|of|is|von|:)?\s*(\d+(?:[.,]\d+)?)").expect("gravity regex"));
            static APEX: LazyLock<Regex> = LazyLock::new(|| {
                Regex::new(r"(\d+(?:[.,]\d+)?)\s*(?:m|meters?|metres?|meter)\s+(?:straight\s+|vertically\s+|senkrecht\s+)?(?:up|high|upwards|into the air|in die höhe|hoch|nach oben)|(?:up to|reaches|reach|height of|max(?:imum)? height of|to a height of|bis zu|höhe von)\s*(\d+(?:[.,]\d+)?)\s*(?:m|meters?|metres?|meter)\b").expect("apex regex")
            });
            static FROM: LazyLock<Regex> = LazyLock::new(|| {
                Regex::new(r"(?:from|off|von|aus)\s+(?:a\s+|an?\s+|einer\s+)?(?:height\s+of\s+|höhe\s+von\s+)?(\d+(?:[.,]\d+)?)\s*(?:m|meters?|metres?|meter)\b").expect("launch height regex")
            });
            // "9.81 m/s²" is gravity, never a launch speed (group 2 marks the square).
            static SPEED: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(\d+(?:[.,]\d+)?)\s*(?:m/s|meters per second|metres per second|meter pro sekunde|mps)(²|\^2|2\b| squared| per second)?").expect("speed regex"));
            static ANGLE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(\d+(?:[.,]\d+)?)\s*(?:°|degrees?|deg|grad)").expect("angle regex"));
            let gravity = if sources.iter().any(|s| s.contains("moon") || s.contains("mond")) {
                1.62
            } else if sources.iter().any(|s| s.contains("mars")) {
                3.71
            } else {
                num(&GRAVITY).filter(|g| (0.5..=30.0).contains(g)).unwrap_or(9.81)
            };
            let apex = sources.iter().find_map(|s| APEX.captures(s).and_then(|c| c.get(1).or(c.get(2))).and_then(|m| m.as_str().replace(',', ".").parse::<f64>().ok()));
            let launch_height = num(&FROM);
            let speed = sources.iter().find_map(|s| {
                SPEED.captures_iter(s).find(|c| c.get(2).is_none()).and_then(|c| c[1].replace(',', ".").parse::<f64>().ok())
            });
            let angle = num(&ANGLE);
            let vertical = apex.is_some() || sources.iter().take(3).any(|s| s.contains("straight up") || s.contains("vertical") || s.contains("senkrecht") || s.contains("thrown up") || s.contains("throws it up") || s.contains("nach oben"));
            // A height reached fixes the launch speed (v = √(2gh)); a model's own number may be off.
            match (apex, speed) {
                (Some(h), _) => {
                    let v = (2.0 * gravity * h).sqrt();
                    spec.push(format!("angle={}", angle.unwrap_or(90.0)));
                    spec.push(format!("velocity={:.2}", v.clamp(1.0, 120.0)));
                    title = format!("Ball thrown {} m up", trim_number(h));
                }
                (None, Some(v)) => {
                    spec.push(format!("angle={}", angle.unwrap_or(if vertical { 90.0 } else { 45.0 })));
                    spec.push(format!("velocity={}", trim_number(v.clamp(1.0, 120.0))));
                    title = format!("Projectile at {} m/s", trim_number(v));
                }
                (None, None) => {
                    if let Some(a) = angle.or(vertical.then_some(90.0)) {
                        spec.push(format!("angle={a}"));
                    }
                }
            }
            spec.push(format!("gravity={gravity}"));
            if let Some(h) = launch_height.filter(|h| apex.is_none_or(|a| (a - *h).abs() > f64::EPSILON)) {
                spec.push(format!("height={}", trim_number(h.clamp(0.0, 100.0))));
            }
        }
        SimKind::Pendulum => {
            static LENGTH: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(\d+(?:[.,]\d+)?)\s*(cm|m|meters?|metres?)\s*(?:long|length|lang|pendulum|pendel|string|faden)?").expect("length regex"));
            if let Some(c) = sources.iter().find_map(|s| LENGTH.captures(s)) {
                if let Ok(x) = c[1].replace(',', ".").parse::<f64>() {
                    let metres = if &c[2] == "cm" { x / 100.0 } else { x };
                    spec.push(format!("length={}", trim_number(metres)));
                }
            }
            static ANGLE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(\d+(?:[.,]\d+)?)\s*(?:°|degrees?|grad)").expect("angle regex"));
            if let Some(a) = num(&ANGLE) {
                spec.push(format!("angle={a}"));
            }
        }
        SimKind::Function => {
            static FORMULA: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?:f\(x\)|y)\s*=\s*([^,;?!\n]+)").expect("formula regex"));
            if let Some(f) = sources.iter().find_map(|s| FORMULA.captures(s).map(|c| c[1].trim().trim_end_matches('.').to_string())) {
                title = format!("f(x) = {f}");
                spec.push(f);
            }
        }
        // The scene, the formula, the colour: `configure` reads them from the words themselves.
        SimKind::Gravity | SimKind::Surface | SimKind::Ripples | SimKind::StandingWave | SimKind::Doppler | SimKind::Sound | SimKind::Light | SimKind::Spring | SimKind::DoublePendulum | SimKind::Balls | SimKind::Sorting => spec.push(kind_text.trim().to_string()),
        _ => {}
    }
    Some(json!({ "sim_type": id, "formula_or_params": spec.join(", "), "title": title }))
}

/// The classic kinematics questions answered exactly, without model arithmetic: "A ball is
/// thrown 5 m straight up. How fast was it thrown?" (v = √(2gh)), how long it is in the air, and
/// a drop from a height (t = √(2h/g), impact speed g·t). None for anything else.
pub fn quick_physics_answer(text: &str) -> Option<String> {
    use regex::Regex;
    use std::sync::LazyLock;
    let l = text.to_lowercase();
    let asks_speed = ["how fast", "what speed", "which speed", "initial speed", "initial velocity", "launch speed", "at what speed", "with what speed", "wie schnell", "welche geschwindigkeit", "mit welcher geschwindigkeit"]
        .iter()
        .any(|p| l.contains(p));
    let asks_time = ["how long", "how much time", "time in the air", "flight time", "wie lange"].iter().any(|p| l.contains(p));
    if !asks_speed && !asks_time {
        return None;
    }
    static UP: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(\d+(?:[.,]\d+)?)\s*(?:m|meters?|metres?|meter)\s+(?:straight\s+|vertically\s+|senkrecht\s+)?(?:up|high|upwards|into the air|in die höhe|hoch|nach oben)\b|(?:reaches|reach|reached|to a (?:maximum )?height of|maximum height of|up to|bis zu|höhe von)\s*(\d+(?:[.,]\d+)?)\s*(?:m|meters?|metres?|meter)\b").expect("apex regex")
    });
    static DROP: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?:dropped|drops|falls|fall|released|let go|fallen gelassen|fällt|fallen)\b.*?(?:from|off|von|aus)\s+(?:a\s+|an?\s+|einer\s+)?(?:height\s+of\s+|höhe\s+von\s+)?(\d+(?:[.,]\d+)?)\s*(?:m|meters?|metres?|meter)\b").expect("drop regex")
    });
    let number = |c: &regex::Captures| c.iter().skip(1).flatten().next().and_then(|m| m.as_str().replace(',', ".").parse::<f64>().ok());
    let g = if l.contains("moon") || l.contains("mond") {
        1.62
    } else if l.contains("mars") {
        3.71
    } else {
        9.81
    };
    let n = |x: f64| trim_number(if x >= 10.0 { (x * 10.0).round() / 10.0 } else { (x * 100.0).round() / 100.0 });
    if let Some(h) = UP.captures(&l).and_then(|c| number(&c)).filter(|h| *h > 0.0) {
        let v = (2.0 * g * h).sqrt();
        let rise = v / g;
        return Some(if asks_speed {
            format!("About {} metres per second, Sir: v equals the square root of 2 g h, with g at {} and h at {} metres.", n(v), trim_number(g), trim_number(h))
        } else {
            format!("It rises for {} seconds and lands after {} seconds in total, Sir.", n(rise), n(2.0 * rise))
        });
    }
    if let Some(h) = DROP.captures(&l).and_then(|c| number(&c)).filter(|h| *h > 0.0) {
        let t = (2.0 * h / g).sqrt();
        return Some(if asks_speed {
            format!("It hits the ground at about {} metres per second, Sir, after {} seconds.", n(g * t), n(t))
        } else {
            format!("About {} seconds, Sir; it lands at {} metres per second.", n(t), n(g * t))
        });
    }
    None
}

/// 5.0 → "5", 9.9 → "9.9".
fn trim_number(x: f64) -> String {
    let s = format!("{x:.2}");
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

/// "Can you make it 3D?", "show the orbits in 3D", "back to 2D", "mach es 3D" while a
/// simulation is open: `adjust_simulation` with the action "3d" or "2d". A bare order is the
/// Earth's while it is up, and one that names a map, the Earth or a place is never the lab's.
pub fn view_order(lab: &SimulationLab, text: &str) -> Option<Value> {
    use regex::Regex;
    use std::sync::LazyLock;
    static OWN: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"\b(?:the|die|den|das)\s+(?:simulations?|sim|simulator|orbits?|planets?|solar\s+system|ripples?|waves?|tank|surface|plot|graph|bahnen|wellen|fläche)\b").expect("valid simulation noun regex")
    });
    if let Some(args) = plot_order(lab, text) {
        return Some(args);
    }
    let sim = lab.find("")?;
    let lower = text.to_lowercase();
    let named = OWN.is_match(&lower);
    let deep = crate::intents::view_3d(&crate::intents::normalize_phrase(&OWN.replace_all(&lower, "it")))?;
    let other = ["map", "earth", "globe", "camera", "city", "town", "street", "streets", "karte", "erde", "globus", "kamera", "stadt"];
    if !named && (crate::earth::is_open() || lower.split(|c: char| !c.is_alphanumeric()).any(|w| other.contains(&w))) {
        return None;
    }
    Some(json!({ "sim": sim.id, "action": if deep { "3d" } else { "2d" } }))
}

/// What Sir says to the plot that is open (Sir, 2026-11-22: "JAI and myself should be able to
/// edit the size of the coordinate system and zoom in and out"): "zoom in on the graph", "set
/// the x axis from -5 to 5", "reset the view", "add the point A at 2, 3", "remove g", "g(x) =
/// cos x", "also plot x^3". The `adjust_simulation` arguments.
pub fn plot_order(lab: &SimulationLab, text: &str) -> Option<Value> {
    use regex::Regex;
    use std::sync::LazyLock;
    static ZOOM: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"^(?:jai,?\s+)?(?:zoom|zoome)\s+(in|out|rein|raus|hinein|heraus)(?:\s+(?:on|of|in|into|at|auf)\s+(?:the\s+|das\s+|den\s+)?(graph|plot|function|curve|coordinate\s+system|grid|graphen|funktion|koordinatensystem))?$").expect("zoom regex")
    });
    static AXIS: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"^(?:jai,?\s+)?(?:(?:set|make|change|put|let|show|stell|setz)\s+)?(?:the\s+|die\s+)?([xy])(?:\s*-?\s*(?:axis|achse|range|bereich))?(?:\s+(?:go|run|goes|runs|reach))?\s+(?:from|von)?\s*(-?\d+(?:\.\d+)?)\s+(?:to|bis|until|through)\s+(-?\d+(?:\.\d+)?)$").expect("axis regex")
    });
    static RESET: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^(?:jai,?\s+)?(?:reset|fit)\s+(?:the\s+)?(?:view|zoom|graph|plot|axes|coordinate\s+system)$").expect("reset regex"));
    static POINT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^(?:jai,?\s+)?(?:add|plot|mark|draw|put|set|zeichne|setze?)\s+(?:the\s+|a\s+|den\s+|einen\s+)?(?:point|punkt)\s+(.+)$").expect("point regex"));
    static REMOVE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"^(?:jai,?\s+)?(?:remove|delete|take\s+away|entferne|lösche)\s+(?:the\s+|die\s+|den\s+)?(?:function\s+|curve\s+|graph\s+|point\s+|funktion\s+|punkt\s+)?([a-z])(?:\s*\(x\))?$").expect("remove regex")
    });
    static DEFINE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^(?:jai,?\s+)?(?:(?:also|now)\s+)?(?:(?:add|plot|draw|graph)\s+)?(?:also\s+)?([a-z]\(x\)\s*=\s*.+)$|^(?:jai,?\s+)?(?:add|also\s+plot|also\s+draw|add\s+the\s+function)\s+(.+)$").expect("define regex"));
    let sim = lab.find("").filter(|s| s.kind == SimKind::Function)?;
    let s = text.trim().trim_end_matches(['.', '!', '?']).to_lowercase().replace("minus ", "-").replace('−', "-");
    let change = |key: &str, value: Value| Some(json!({ "sim": sim.id, "changes": { key: value } }));
    if let Some(c) = ZOOM.captures(&s) {
        // (Without a word for the plot, the Earth's zoom is the Earth's.)
        if c.get(2).is_none() && crate::earth::is_open() {
            return None;
        }
        return change("zoom", json!(if matches!(&c[1], "in" | "rein" | "hinein") { "in" } else { "out" }));
    }
    if let Some(c) = AXIS.captures(&s) {
        return change(if &c[1] == "x" { "x_range" } else { "y_range" }, json!(format!("{} to {}", &c[2], &c[3])));
    }
    if RESET.is_match(&s) {
        return change("view", json!("reset"));
    }
    if let Some(c) = POINT.captures(&s) {
        return point_of(&json!(c[1].replace(" at ", " "))).is_some().then(|| change("point", json!(c[1].replace(" at ", " "))))?;
    }
    if let Some(c) = REMOVE.captures(&s) {
        let name = &c[1];
        return (sim.functions.iter().any(|(l, _, _)| l == name) || sim.points.iter().any(|(p, _, _)| p.eq_ignore_ascii_case(name))).then(|| change("remove", json!(name)))?;
    }
    if let Some(c) = DEFINE.captures(&s) {
        let src = c.get(1).or(c.get(2))?.as_str().trim();
        return (src.contains('x') && parse_expr(src).is_ok()).then(|| change("function", json!(src)))?;
    }
    None
}

/// "Set the angle to 60 degrees and double the velocity" while a simulation is open: the
/// `adjust_simulation` arguments (the instant fast path).
pub fn voice_adjust(lab: &SimulationLab, text: &str) -> Option<Value> {
    let sim = lab.find("")?;
    let lower = text.to_lowercase().replace(['?', '!'], "");
    let names: Vec<&str> = sim.params.iter().map(|p| p.name.as_str()).collect();
    let mut changes = serde_json::Map::new();
    for clause in lower.split([',', ';']).flat_map(|c| c.split(" and ")).flat_map(|c| c.split(" und ")) {
        let words: Vec<&str> = clause.split_whitespace().filter(|w| !matches!(*w, "the" | "die" | "den" | "das" | "der" | "please" | "bitte" | "jai")).collect();
        if words.is_empty() {
            continue;
        }
        // A one-letter slider ("a", "b") only counts as "a to 5" / "a = 5" ("set a timer" is not one).
        let find_param = |ws: &[&str]| -> Option<String> {
            ws.iter().enumerate().find_map(|(i, w)| {
                let n = slider_name(&sim.params, w.trim_matches(|c: char| !c.is_alphanumeric() && c != '_'));
                let next = ws.get(i + 1).copied().unwrap_or("");
                let explicit = n.chars().count() > 1 || matches!(next, "to" | "=" | "auf" | "zu") || next.parse::<f64>().is_ok();
                (names.contains(&n.as_str()) && explicit).then_some(n)
            })
        };
        let Some(name) = find_param(&words) else { continue };
        let op = match words[0] {
            "double" | "verdopple" | "verdoppel" => Some(json!("double")),
            "halve" | "half" | "halbiere" => Some(json!("half")),
            "triple" => Some(json!("triple")),
            "increase" | "raise" | "erhöhe" if !words.iter().any(|w| w.parse::<f64>().is_ok()) => Some(json!("more")),
            "decrease" | "lower" | "reduce" | "verringere" if !words.iter().any(|w| w.parse::<f64>().is_ok()) => Some(json!("less")),
            _ => None,
        };
        let value = op.or_else(|| {
            let num = words.iter().rev().find_map(|w| w.trim_end_matches(['°', '%']).replace(',', ".").parse::<f64>().ok())?;
            let by = words.iter().any(|w| *w == "by" || *w == "um");
            Some(match words[0] {
                "increase" | "raise" | "erhöhe" if by => json!(format!("+{num}")),
                "decrease" | "lower" | "reduce" | "verringere" if by => json!(format!("-={num}")),
                _ => json!(num),
            })
        });
        if let Some(v) = value {
            changes.insert(name, v);
        }
    }
    if changes.is_empty() {
        return None;
    }
    // Only commands that are clearly about the simulation ("set/make/change/double …").
    let verb = ["set", "make", "change", "double", "halve", "half", "triple", "increase", "decrease", "raise", "lower", "reduce", "put", "stell", "setz", "verdopp", "halbier", "erhöh", "verringer", "ändere"];
    if !verb.iter().any(|v| lower.trim_start_matches("jai").trim_start_matches([',', ' ']).starts_with(v) || lower.contains(&format!(" {v}"))) {
        return None;
    }
    Some(json!({ "sim": sim.id, "changes": Value::Object(changes) }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinematics_questions_are_answered_exactly() {
        let a = quick_physics_answer("A ball is thrown 5 m straight up. How fast was it thrown?").unwrap();
        assert!(a.starts_with("About 9.9 metres per second"), "{a}");
        let a = quick_physics_answer("A ball is thrown 5 m up. How long is it in the air?").unwrap();
        assert!(a.contains("2.02 seconds in total"), "{a}");
        let a = quick_physics_answer("A stone is dropped from a height of 20 m. How fast does it hit the ground?").unwrap();
        assert!(a.contains("19.8 metres per second") && a.contains("2.02 seconds"), "{a}");
        assert!(quick_physics_answer("How fast is a cheetah?").is_none());
        assert!(quick_physics_answer("Show me the simulation").is_none());
    }

    /// "Can you make it 3D?" with the orbits open (Sir's screenshot, 2026-11-21) turned the view
    /// of nothing: no route knew the words. Now the newest simulation turns, its run goes on (a
    /// pause rewound it too), a flat kind says so, and a map or the Earth keeps its own orders.
    #[test]
    fn make_it_3d_turns_the_open_simulation() {
        let lab = SimulationLab::default();
        assert!(view_order(&lab, "Can you make it 3D?").is_none(), "nothing is open");
        let sim = lab.launch(SimKind::Gravity, "solar system", None).unwrap();
        assert!(!sim.three_d);
        for (text, action) in [
            ("Can you make it 3D?", "3d"),
            ("Show the orbits in 3D", "3d"),
            ("Make the simulation three-dimensional", "3d"),
            ("3D view, please", "3d"),
            ("Mach es 3D", "3d"),
            ("Back to 2D", "2d"),
            ("Make it flat again", "2d"),
            ("Mach es wieder 2D", "2d"),
        ] {
            let args = view_order(&lab, text).unwrap_or_else(|| panic!("{text}"));
            assert_eq!((args["sim"].as_str(), args["action"].as_str()), (Some(sim.id.as_str()), Some(action)), "{text}");
        }
        for text in ["Tilt the map", "Show me the earth in 3D", "Make it louder", "Render me an Iron Man suit in 3D", "What is 3D printing?"] {
            assert!(view_order(&lab, text).is_none(), "{text}");
        }
        let said = run_tool(&lab, "adjust_simulation", &json!({ "action": "3d" })).unwrap();
        let now = lab.get(&sim.id).unwrap();
        assert!(now.three_d && now.revision == sim.revision, "the run goes on: {said}");
        assert!(run_tool(&lab, "adjust_simulation", &json!({ "action": "3d" })).unwrap().contains("already"));
        lab.toggle_pause(&sim.id);
        assert_eq!(lab.get(&sim.id).unwrap().revision, sim.revision, "a pause holds the run where it is");
        run_tool(&lab, "adjust_simulation", &json!({ "action": "2d" })).unwrap();
        assert!(!lab.get(&sim.id).unwrap().three_d);
        let pendulum = lab.launch(SimKind::Pendulum, "", None).unwrap();
        assert!(run_tool(&lab, "adjust_simulation", &json!({ "sim": pendulum.id, "action": "3d" })).unwrap().contains("flat"));
        assert!(lab.launch(SimKind::Gravity, "simulate the solar system in 3D", None).unwrap().three_d);
        assert!(lab.launch(SimKind::Surface, "z = x*y", None).unwrap().three_d);
        assert!(!lab.launch(SimKind::Gravity, "the sun and the earth for 3 days", None).unwrap().three_d);
    }

    /// A plot is a GeoGebra of his (Sir, 2026-11-22): he and JAI zoom it, set its axes, put
    /// points and more curves on it and take them off, by voice and by the tool, and it reads
    /// out its extrema and where its curves meet.
    #[test]
    fn a_plot_is_zoomed_and_edited_like_geogebra() {
        assert_eq!(zoomed([-10.0, 10.0, -5.0, 5.0], 2.0, (0.0, 0.0)), [-5.0, 5.0, -2.5, 2.5]);
        assert_eq!(zoomed([0.0, 4.0, 0.0, 4.0], 0.5, (0.0, 0.0)), [0.0, 8.0, 0.0, 8.0], "about the cursor's point");
        assert_eq!(zoomed([0.0, 1.0, 0.0, 1.0], 1e12, (0.5, 0.5)), [0.0, 1.0, 0.0, 1.0], "no absurd zoom");
        assert_eq!(two_numbers(&json!("−5 to 5")), Some((-5.0, 5.0)));
        assert_eq!(point_of(&json!("A = (2, 3)")), Some((Some("A".into()), 2.0, 3.0)));
        assert_eq!(point_of(&json!("b(1|-4)")), Some((Some("B".into()), 1.0, -4.0)));
        assert_eq!(point_of(&json!("(2.5, 3)")), Some((None, 2.5, 3.0)));
        assert_eq!(point_of(&json!("nothing")), None);

        let lab = SimulationLab::default();
        assert!(plot_order(&lab, "zoom in on the graph").is_none(), "no plot is open");
        let sim = lab.launch(SimKind::Function, "x^2 - 4", None).unwrap();
        let fit = lab.get(&sim.id).unwrap().plane();
        assert!(fit[0] == -10.0 && fit[1] == 10.0 && fit[2] < -4.0 && fit[3] > 90.0, "{fit:?}");
        let orders = |text: &str| plot_order(&lab, text).map(|a| a["changes"].clone());
        assert_eq!(orders("Zoom in on the graph"), Some(json!({ "zoom": "in" })));
        assert_eq!(orders("zoom out"), Some(json!({ "zoom": "out" })));
        assert_eq!(orders("Set the x axis from minus 5 to 5."), Some(json!({ "x_range": "-5 to 5" })));
        assert_eq!(orders("y from 0 to 20"), Some(json!({ "y_range": "0 to 20" })));
        assert_eq!(orders("reset the view"), Some(json!({ "view": "reset" })));
        assert_eq!(orders("add the point A at 2, 3"), Some(json!({ "point": "a 2, 3" })));
        assert_eq!(orders("g(x) = cos(x)"), Some(json!({ "function": "g(x) = cos(x)" })));
        assert_eq!(orders("also plot x^3"), Some(json!({ "function": "x^3" })));
        for text in ["zoom in on the map", "add a reminder", "remove the timer", "set the volume to 5", "remove g"] {
            assert_eq!(orders(text), None, "{text}");
        }

        let said = lab.adjust(&sim.id, &json!({ "x_range": "-5 to 5", "y_range": [-10, 30] })).unwrap();
        assert_eq!(lab.get(&sim.id).unwrap().plane(), [-5.0, 5.0, -10.0, 30.0], "{said:?}");
        lab.adjust(&sim.id, &json!({ "zoom": "in" })).unwrap();
        assert_eq!(lab.get(&sim.id).unwrap().plane(), [-2.5, 2.5, 0.0, 20.0]);
        assert!(lab.adjust(&sim.id, &json!({ "x_min": 9 })).is_err(), "an axis that runs backwards");
        lab.adjust(&sim.id, &json!({ "view": "reset" })).unwrap();
        assert_eq!(lab.get(&sim.id).unwrap().plane(), fit);

        lab.adjust(&sim.id, &json!({ "point": "A = (2, 3)", "function": "g(x) = 2x - 1" })).unwrap();
        lab.adjust(&sim.id, &json!({ "point": "(0, 0)" })).unwrap();
        let now = lab.get(&sim.id).unwrap();
        assert_eq!(now.points.iter().map(|(n, ..)| n.as_str()).collect::<Vec<_>>(), ["A", "B"]);
        assert_eq!(now.functions.iter().map(|(l, ..)| l.as_str()).collect::<Vec<_>>(), ["f", "g"]);
        assert_eq!(orders("remove g"), Some(json!({ "remove": "g" })));
        let readout = now.readout();
        assert!(readout.contains("meet") && readout.contains("A = (2, 3)"), "{readout}");
        assert_eq!(turning_points(&now.functions[0].2, -5.0, 5.0, &now.params).len(), 1);
        let meet = crossings(&now.functions[0].2, &now.functions[1].2, -10.0, 10.0, &now.params);
        assert_eq!(meet.len(), 2, "x² - 4 = 2x - 1 at x = -1 and 3: {meet:?}");
        assert!((meet[0].0 + 1.0).abs() < 1e-6 && (meet[1].0 - 3.0).abs() < 1e-6, "{meet:?}");
        lab.adjust(&sim.id, &json!({ "remove": "g", "delete": "A" })).unwrap();
        let now = lab.get(&sim.id).unwrap();
        assert_eq!((now.functions.len(), now.points.len()), (1, 1));
        assert!(lab.adjust(&sim.id, &json!({ "remove": "z" })).is_err());
        // The input line: points and curves as GeoGebra takes them.
        lab.plot_input(&sim.id, "C = (1, -2)").unwrap();
        lab.plot_input(&sim.id, "h(x) = sin(x)").unwrap();
        lab.plot_input(&sim.id, "x^3").unwrap();
        assert!(lab.plot_input(&sim.id, "x +* 2").is_err());
        let now = lab.get(&sim.id).unwrap();
        assert!(now.points.iter().any(|(n, x, y)| n == "C" && *x == 1.0 && *y == -2.0));
        assert_eq!(now.functions.iter().map(|(l, s, _)| format!("{l}: {s}")).collect::<Vec<_>>(), ["f: x^2 - 4", "h: sin(x)", "g: x^3"]);
    }

    #[test]
    fn simulation_requests_take_their_numbers_from_the_conversation() {
        // JAI's reply first, Sir's words last (they win): what the brain passes.
        let context = vec![
            "It needs about 9.9 metres per second, Sir, with gravity at 9.81 m/s².".to_string(),
            "A ball is thrown 5 m straight up. How fast was it thrown?".to_string(),
        ];
        let args = simulation_request("Can you show me the simulation?", &context).expect("a launch");
        assert_eq!(args["sim_type"], "projectile");
        let spec = args["formula_or_params"].as_str().unwrap();
        assert!(spec.contains("angle=90") && spec.contains("gravity=9.81"), "{spec}");
        assert!(spec.contains("velocity=9.9"), "{spec}");
        let args = simulation_request("Simulate a cannon ball at 30 m/s and 60 degrees from 10 m", &[]).unwrap();
        let spec = args["formula_or_params"].as_str().unwrap();
        assert!(spec.contains("velocity=30") && spec.contains("angle=60") && spec.contains("height=10"), "{spec}");
        let args = simulation_request("plot f(x) = x^2 - 3", &[]).unwrap();
        assert_eq!((args["sim_type"].as_str(), args["formula_or_params"].as_str()), (Some("function"), Some("x^2 - 3")));
        assert!(simulation_request("show me my calendar", &[]).is_none());
        assert!(simulation_request("what's the weather", &context).is_none());
        assert!(simulation_request("show me the simulation", &[]).is_none(), "nothing to simulate");
    }

    #[test]
    fn expressions_parse_and_evaluate() {
        let p: Vec<Param> = vec![param("a", "a", 2.0, -10.0, 10.0, "")];
        let e = parse_expr("f(x) = 3x^2 - 2x + 1").unwrap();
        assert_eq!(e.eval(2.0, &p), 9.0);
        assert!((parse_expr("sin(pi/2)").unwrap().eval(0.0, &p) - 1.0).abs() < 1e-12);
        assert_eq!(parse_expr("a*x").unwrap().eval(3.0, &p), 6.0);
        assert!((parse_expr("2sin(x)").unwrap().eval(std::f64::consts::FRAC_PI_2, &p) - 2.0).abs() < 1e-12);
        assert_eq!(parse_expr("-x^2").unwrap().eval(3.0, &p), -9.0);
        assert_eq!(parse_expr("(x+1)(x-1)").unwrap().eval(3.0, &p), 8.0);
        assert_eq!(parse_expr("x²").unwrap().eval(4.0, &p), 16.0);
        assert!(parse_expr("x +* ").is_err());
        let r = roots(&parse_expr("x^2 - 4").unwrap(), -10.0, 10.0, &p);
        assert_eq!(r.len(), 2);
        assert!((r[0] + 2.0).abs() < 1e-6 && (r[1] - 2.0).abs() < 1e-6);
        assert!((integral(&parse_expr("x^2").unwrap(), 0.0, 3.0, &p) - 9.0).abs() < 1e-6);
        assert!((derivative(&parse_expr("x^3").unwrap(), 2.0, &p) - 12.0).abs() < 1e-4);
    }

    #[test]
    fn simulations_launch_and_adjust_by_voice() {
        let lab = SimulationLab::default();
        let sim = lab.launch(SimKind::Projectile, "angle=60, velocity=30", None).unwrap();
        assert_eq!(sim.value("angle"), 60.0);
        assert_eq!(sim.value("velocity"), 30.0);
        let args = voice_adjust(&lab, "Set the angle to 45 degrees and double the velocity").unwrap();
        let applied = lab.adjust(args["sim"].as_str().unwrap(), &args["changes"]).unwrap();
        assert_eq!(applied.len(), 2);
        let now = lab.get(&sim.id).unwrap();
        assert_eq!(now.value("angle"), 45.0);
        assert_eq!(now.value("velocity"), 60.0);
        assert!(voice_adjust(&lab, "open Spotify").is_none());
        let quad = lab.launch(SimKind::Function, "a*x^2 + b", None).unwrap();
        assert!(voice_adjust(&lab, "set a timer for 5 minutes").is_none(), "not the slider a");
        assert_eq!(voice_adjust(&lab, "set a to 3").unwrap()["changes"]["a"], 3.0);
        assert!(lab.close(&quad.id));
        let plot = lab.launch(SimKind::Function, "f(x)=x^2-4; g(x)=sin(x) with derivative, x from -5 to 5", None).unwrap();
        assert_eq!(plot.functions.len(), 2);
        assert!(plot.show_derivative);
        assert_eq!(plot.x_range, (-5.0, 5.0));
        assert!(plot.readout().contains("roots"));
        let (u1, u2) = collide(1.0, 1.0, 2.0, 0.0, 1.0);
        assert!((u1 - 0.0).abs() < 1e-12 && (u2 - 2.0).abs() < 1e-12);
        let steps = sort_steps(&[3.0, 1.0, 2.0], "quick");
        assert_eq!(steps.last().unwrap().0, vec![1.0, 2.0, 3.0]);
        assert!(lab.close(&sim.id));
        assert_eq!(apply_change(10.0, &json!("x3")), Some(30.0));
    }

    #[test]
    fn the_new_kinds_are_found_by_their_words_in_english_and_german() {
        use SimKind::*;
        let cases = [
            ("simulate the solar system", Gravity),
            ("the sun, the earth and a comet", Gravity),
            ("zeig mir die Umlaufbahn des Mondes", Gravity),
            ("binary star", Gravity),
            ("figure eight three body", Gravity),
            ("ripple tank", Ripples),
            ("Wellenwanne", Ripples),
            ("single slit", Ripples),
            ("standing wave on a string", StandingWave),
            ("stehende Welle", StandingWave),
            ("third harmonic", StandingWave),
            ("doppler effect", Doppler),
            ("sonic boom", Doppler),
            ("Überschall", Doppler),
            ("sound wave", Sound),
            ("Schallwelle", Sound),
            ("longitudinal wave", Sound),
            ("double slit", Light),
            ("Doppelspalt", Light),
            ("a red laser", Light),
            ("light interference", Light),
            ("mass on a spring", Spring),
            ("Federpendel", Spring),
            ("resonance", Spring),
            ("double pendulum", DoublePendulum),
            ("Doppelpendel", DoublePendulum),
            ("bouncing balls in a box", Balls),
            ("billiard", Balls),
            ("3d surface", Surface),
            ("plot z = x^2 - y^2", Surface),
            ("Oberfläche", Surface),
            // The old kinds keep their words.
            ("wave interference", Waves),
            ("Welle", Waves),
            ("pendulum", Pendulum),
            ("Pendel", Pendulum),
            ("a cannon ball", Projectile),
            ("collision", Collision),
            ("gradient descent", GradientDescent),
            ("bubble sort", Sorting),
            ("plot f(x) = x^2", Function),
            ("function", Function),
        ];
        for (words, kind) in cases {
            assert_eq!(SimKind::parse(words), Some(kind), "{words}");
        }
        for kind in [Function, Projectile, Pendulum, Collision, Waves, GradientDescent, Sorting, Gravity, Ripples, StandingWave, Doppler, Sound, Light, Spring, DoublePendulum, Balls, Surface] {
            assert_eq!(SimKind::parse(kind.id()), Some(kind), "the tool's own name for {kind:?}");
        }
    }

    #[test]
    fn the_new_simulations_read_out_their_physics() {
        let lab = SimulationLab::default();
        let read = |kind: SimKind, spec: &str| {
            let sim = lab.launch(kind, spec, None).unwrap();
            let out = sim.readout();
            lab.close(&sim.id);
            out
        };
        let solar = read(SimKind::Gravity, "solar system");
        assert!(solar.contains("Mercury orbits Sun in 0.241 years") && solar.contains("Neptune orbits Sun in 164."), "{solar}");
        assert!(read(SimKind::Gravity, "binary star").contains("3 bodies"));
        let scene = r#"{"g": 1, "bodies": [{"name": "Sun", "mass": 10, "x": 0, "y": 0}, {"name": "Rock", "mass": 0.01, "x": 2, "y": 0, "vx": 0, "vy": 2.2}]}"#;
        let sim = lab.launch(SimKind::Gravity, scene, None).unwrap();
        assert_eq!((sim.scene.as_str(), sim.bodies().bodies.len()), (scene, 2));
        let words = lab.launch(SimKind::Gravity, "the sun, the earth and a comet", None).unwrap();
        assert_eq!(words.bodies().bodies.len(), 3);
        assert!(words.readout().contains("Comet orbits Sun") || words.readout().contains("Earth orbits Sun"));
        lab.close(&sim.id);
        lab.close(&words.id);
        let tank = lab.launch(SimKind::Ripples, "single slit, point source", None).unwrap();
        assert_eq!((tank.value("slits"), tank.value("plane")), (1.0, 0.0));
        assert!(tank.readout().contains("wavelength 20.0 cells") && tank.readout().contains("narrower than a wavelength"), "{}", tank.readout());
        lab.close(&tank.id);
        let two = read(SimKind::Ripples, "");
        assert!(two.contains("the first at 56°"), "{two}");
        let string = read(SimKind::StandingWave, "");
        assert!(string.contains("30.0 Hz") && string.contains("4 nodes") && string.contains("1.33 m"), "{string}");
        let doppler = read(SimKind::Doppler, "");
        assert!(doppler.contains("heard 677 Hz in front") && doppler.contains("326 Hz behind"), "{doppler}");
        let boom = read(SimKind::Doppler, "supersonic");
        assert!(boom.contains("Mach cone of half-angle 43.3°") && boom.contains("Mach 1.46"), "{boom}");
        assert!(read(SimKind::Sound, "").contains("wavelength v/f = 0.780 m"));
        let light = read(SimKind::Light, "");
        assert!(light.contains("green light") && light.contains("10.64 mm") && light.contains("±35.5 mm"), "{light}");
        assert!(read(SimKind::Light, "a red laser").contains("red light, 650 nm"));
        let spring = read(SimKind::Spring, "");
        assert!(spring.contains("ω₀ = √(k/m) = 4.47 rad/s") && spring.contains("underdamped"), "{spring}");
        assert!(read(SimKind::Spring, "resonance").contains("steady swing is 1.491 m"));
        assert!(read(SimKind::DoublePendulum, "").contains("total energy 14.7"));
        let balls = read(SimKind::Balls, "");
        assert!(balls.contains("20 balls") && balls.contains("90 % of the speed"), "{balls}");
    }

    #[test]
    fn a_surface_takes_x_and_y_and_its_own_sliders() {
        let lab = SimulationLab::default();
        let saddle = lab.launch(SimKind::Surface, "z = x^2 - y^2", None).unwrap();
        assert_eq!(saddle.functions[0].1, "x^2 - y^2");
        let out = saddle.readout();
        assert!(out.contains("z(0, 0) = 0.000") && out.contains("between -25.00 and 25.00"), "{out}");
        let both = lab.launch(SimKind::Surface, "z = a*x*y", None).unwrap();
        assert!(both.params.iter().any(|p| p.name == "a") && !both.params.iter().any(|p| p.name == "y"));
        let mut ps = both.surface_params();
        assert_eq!(both.surface_z(&mut ps, 3.0, 4.0), 12.0);
        // "xy" is x times y, and a formula in words is found among the filler.
        let joined = lab.launch(SimKind::Surface, "z = 2xy", None).unwrap();
        let mut ps = joined.surface_params();
        assert_eq!(joined.surface_z(&mut ps, 3.0, 4.0), 24.0);
        let said = lab.launch(SimKind::Surface, "plot sin(x)*cos(y) as a surface", None).unwrap();
        assert_eq!(said.functions[0].1, "sin(x)*cos(y)");
        lab.close(&saddle.id);
        lab.close(&both.id);
        lab.close(&joined.id);
        lab.close(&said.id);
        let default = lab.launch(SimKind::Surface, "simulate a 3d surface", None).unwrap();
        assert_eq!(default.functions[0].1, "sin(sqrt(x^2 + y^2))");
        assert!(lab.launch(SimKind::Surface, "z = x +* ", None).is_err());
    }

    /// Slips found while merging the new kinds: a request naming its own thing took the last
    /// simulation's kind, "Zeichne den Mond" became an orbit, a formula in 3D went to the studio.
    #[test]
    fn a_request_keeps_its_own_subject() {
        let context = vec!["Simulate a pendulum with a 2 m string".to_string()];
        assert_eq!(simulation_request("simulate it again", &context).unwrap()["sim_type"], "pendulum");
        assert_eq!(simulation_request("plot it with 3 m", &context).unwrap()["sim_type"], "pendulum");
        assert!(simulation_request("Visualise the volcano", &context).is_none());
        assert!(simulation_request("Zeichne den Mond", &[]).is_none());
        assert_eq!(simulation_request("simulate the moon", &[]).unwrap()["sim_type"], "gravity");
        assert_eq!(simulation_request("Zeichne die Umlaufbahn der Erde", &[]).unwrap()["sim_type"], "gravity");
        let said = "Show me z = x^2 + y^2 in 3D";
        assert!(crate::model3d::intent(said).is_none());
        let args = simulation_request(said, &[]).unwrap();
        assert_eq!(args["sim_type"], "surface");
        let lab = SimulationLab::default();
        let surface = lab.launch(SimKind::Surface, args["formula_or_params"].as_str().unwrap(), None).unwrap();
        assert_eq!(surface.functions[0].1, "x^2 + y^2");
        lab.close(&surface.id);
    }

    /// jai.log 2026-11-20: "open bubblesort" looked for a program of that name.
    #[test]
    fn a_sorting_algorithm_is_opened_by_its_name() {
        let lab = SimulationLab::default();
        for (said, algorithm) in [("open bubblesort", "bubble"), ("show me quick sort", "quick"), ("zeig mir Insertion Sort", "insertion"), ("start the sorting algorithm", "bubble")] {
            let args = simulation_request(said, &[]).unwrap_or_else(|| panic!("{said}"));
            assert_eq!(args["sim_type"], "sorting", "{said}");
            let sim = lab.launch(SimKind::Sorting, args["formula_or_params"].as_str().unwrap(), None).unwrap();
            assert_eq!(sim.algorithm, algorithm, "{said}");
            lab.close(&sim.id);
        }
        for said in ["sort my downloads", "open the downloads sorted by date", "show me all sorts of things"] {
            assert!(simulation_request(said, &[]).is_none(), "{said}");
        }
    }

    #[test]
    fn the_new_simulations_are_reached_by_voice_and_their_sliders_by_name() {
        let args = |s: &str| simulation_request(s, &[]).unwrap_or(Value::Null);
        for (said, kind) in [
            ("simulate a double slit with a red laser", "light"),
            ("simulate the sun, the earth and a comet", "gravity"),
            ("show me ripples with a single slit", "ripples"),
            ("simulate standing waves", "standing_wave"),
            ("plot z = sin(x) * cos(y)", "surface"),
            ("simulate the doppler effect", "doppler"),
            ("simulate the double pendulum", "double_pendulum"),
            ("simulate sound waves", "sound"),
            ("simulate bouncing balls", "balls"),
            ("simulate a mass on a spring", "spring"),
            ("simuliere den Doppelspalt", "light"),
        ] {
            assert_eq!(args(said)["sim_type"], kind, "{said}");
            // (No fixed route claims them: the language model's tool or `simulation_request` does.)
            assert!(crate::phrasings::route(said).is_none_or(|(tool, _)| tool == "launch_interactive_simulation"), "{said}");
        }
        assert!(args("simulate the sun, the earth and a comet")["formula_or_params"].as_str().unwrap().contains("comet"));
        let lab = SimulationLab::default();
        let sim = lab.launch(SimKind::Doppler, "", None).unwrap();
        let said = voice_adjust(&lab, "set the speed to 200").unwrap();
        assert_eq!(said["changes"]["speed"], 200.0);
        lab.adjust(&sim.id, &said["changes"]).unwrap();
        assert_eq!(lab.get(&sim.id).unwrap().value("speed"), 200.0);
        lab.adjust(&sim.id, &json!({"source speed": "half"})).unwrap();
        assert_eq!(lab.get(&sim.id).unwrap().value("speed"), 100.0);
        assert!(tool_definitions()[0].to_string().contains("double_pendulum"));
    }
}
