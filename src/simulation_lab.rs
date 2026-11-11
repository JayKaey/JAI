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
        if let Some(r) = rest.strip_prefix('x') {
            out.push("x".into());
            rest = r;
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
    let n = 800;
    let step = (b - a) / n as f64;
    let mut out: Vec<f64> = Vec::new();
    let mut x0 = a;
    let mut y0 = e.eval(x0, params);
    for i in 1..=n {
        let x1 = a + i as f64 * step;
        let y1 = e.eval(x1, params);
        if y0.is_finite() && y1.is_finite() {
            if y0 == 0.0 {
                out.push(x0);
            } else if y0.signum() != y1.signum() && (y1 - y0).abs() < 1e6 {
                let (mut lo, mut hi) = (x0, x1);
                for _ in 0..60 {
                    let mid = 0.5 * (lo + hi);
                    if e.eval(mid, params).signum() == e.eval(lo, params).signum() {
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
}

impl SimKind {
    pub fn parse(s: &str) -> Option<Self> {
        let s = s.to_lowercase();
        let has = |w: &[&str]| w.iter().any(|x| s.contains(x));
        Some(if has(&["projectile", "trajectory", "throw", "ballistic", "cannon", "wurf", "flugbahn"]) {
            Self::Projectile
        } else if has(&["pendulum", "pendel", "swing"]) {
            Self::Pendulum
        } else if has(&["collision", "collide", "stoß", "stoss", "impact", "momentum"]) {
            Self::Collision
        } else if has(&["wave", "interference", "welle", "ripple", "double slit"]) {
            Self::Waves
        } else if has(&["gradient", "descent", "learning", "ml", "optimi", "loss"]) {
            Self::GradientDescent
        } else if has(&["sort", "algorithm", "bubble", "quick", "insertion", "merge", "sortier"]) {
            Self::Sorting
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
    /// Bumped on every change: the window restarts its animation.
    pub revision: u64,
    pub paused: bool,
}

impl Simulation {
    pub fn value(&self, name: &str) -> f64 {
        self.params.iter().find(|p| p.name == name).map(|p| p.value).unwrap_or(0.0)
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
            SimKind::Function => {
                let (a, b) = self.x_range;
                self.functions
                    .iter()
                    .map(|(label, src, e)| {
                        let r = roots(e, a, b, &self.params);
                        let shown: Vec<String> = r.iter().take(6).map(|x| format!("{x:.3}")).collect();
                        format!(
                            "{label}(x) = {src}: roots {}; ∫ from {a} to {b} = {:.4}",
                            if shown.is_empty() { "none in range".to_string() } else { shown.join(", ") },
                            integral(e, a, b, &self.params)
                        )
                    })
                    .collect::<Vec<_>>()
                    .join(" | ")
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
        "distance" | "gap" | "abstand" => "separation".into(),
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

    pub fn toggle_pause(&self, id: &str) {
        self.edit(id, |s| s.paused = !s.paused);
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
            revision: 0,
            paused: false,
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
        for (raw_name, raw_value) in map {
            let name = alias(&raw_name);
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
                        self.edit(id, |s| {
                            let label = ["f", "g", "h", "p", "q", "r"].get(s.functions.len()).copied().unwrap_or("y").to_string();
                            s.functions.push((label, src.to_string(), e));
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
    // A JSON object of parameters.
    if let Ok(Value::Object(o)) = serde_json::from_str::<Value>(spec) {
        for (k, v) in o {
            let name = alias(&k);
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
        return Ok(());
    }
    let lower = spec.to_lowercase();
    match sim.kind {
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
                let k = alias(&k);
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
                let k = alias(&k);
                if let (Some(q), Ok(val)) = (sim.params.iter_mut().find(|q| q.name == k), v.parse::<f64>()) {
                    q.value = val.clamp(q.min, q.max);
                }
            }
        }
        _ => {
            for (k, v) in parse_pairs(spec) {
                let k = alias(&k);
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
            "description": "Simulation Lab: a live simulation with sliders as a HUD block (separate_window true only when Sir asks for a window). sim_type function (f(x); several separated by ';'; 'derivative' / 'integral'; 'x from -5 to 5'), projectile, pendulum, collision, waves, gradient_descent, sorting (bubble / insertion / selection / quick). formula_or_params: the formulas, or 'angle=60, velocity=30, gravity=9.81'. For any physics problem, function or algorithm he could watch.",
            "parameters": {"type": "object", "properties": {
                "sim_type": {"type": "string", "enum": ["function", "projectile", "pendulum", "collision", "waves", "gradient_descent", "sorting"]},
                "separate_window": {"type": "boolean", "description": "true only when Sir asked for a new / separate window."},
                "formula_or_params": {"type": "string"},
                "title": {"type": "string"}
            }, "required": ["sim_type"]}
        }}),
        json!({"type": "function", "function": {
            "name": "adjust_simulation",
            "description": "Change an open simulation by voice: changes maps slider names to values or operations ({\"angle\": 60, \"velocity\": \"*2\"}; also \"half\", \"+10\", or \"function\": \"sin(x)\" to add a curve, \"derivative\": true); action restart, pause, resume or close. sim omitted = the newest simulation.",
            "parameters": {"type": "object", "properties": {
                "sim": {"type": "string", "description": "id, title or kind (\"the pendulum\")"},
                "changes": {"type": "object"},
                "action": {"type": "string", "enum": ["set", "restart", "pause", "resume", "close"]}
            }, "required": []}
        }}),
    ]
}

/// Where a new simulation opens: a block inside the HUD, or its own OS window when Sir asked for
/// one (`separate_window`). Returns whether it is detached.
pub fn place_simulation(id: &str, args: &Value) -> bool {
    let separate = args.get("separate_window").and_then(Value::as_bool) == Some(true);
    if separate {
        crate::hud_dock::detach(&format!("sim:{id}"));
    }
    separate
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
            let separate = place_simulation(&sim.id, args);
            crate::gui::sync_windows();
            Ok(format!(
                "Opened the {} simulation '{}' ({}) {} with sliders for {}. {}",
                kind.label().to_lowercase(),
                sim.title,
                sim.id,
                if separate { "in its own window" } else { "on the HUD" },
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
        Regex::new(r"(?i)\b(?:simulat\w*|simulier\w*|visuali[sz]\w*|animat\w*|plot\w*|zeichne|darstell\w*)\b|\b(?:show|zeig|zeige|display|open|launch|start|starte|run)\b.*\b(?:simulation|simulator|trajectory|flugbahn|graph|plot|animation|pendulum|pendel)\b").expect("valid simulation request regex")
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
    let kind = kind_of(text).or_else(|| context.iter().rev().take(6).find_map(|c| kind_of(c)))?;
    // Numbers: Sir's sentence first, then the conversation, newest first.
    let mut sources: Vec<String> = vec![lower.clone()];
    sources.extend(context.iter().rev().take(6).map(|c| c.to_lowercase()));
    let num = |re: &Regex| -> Option<f64> { sources.iter().find_map(|s| re.captures(s).and_then(|c| c.get(1)).and_then(|m| m.as_str().replace(',', ".").parse::<f64>().ok())) };
    let id = match kind {
        SimKind::Function => "function",
        SimKind::Projectile => "projectile",
        SimKind::Pendulum => "pendulum",
        SimKind::Collision => "collision",
        SimKind::Waves => "waves",
        SimKind::GradientDescent => "gradient_descent",
        SimKind::Sorting => "sorting",
    };
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
                let n = alias(w.trim_matches(|c: char| !c.is_alphanumeric() && c != '_'));
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
}
