//! The HUD's shared look (Sir, 2026-11-12: "look and feel better"): one motion table, one shell
//! for every block (the shadow it floats on, the wear on its frame, the emblem's light on the
//! edge that faces it), the header every block wears (title, state light, its keys) and what a
//! block shows while it loads, has nothing to show, or failed. The board's report blocks and the
//! tool blocks (`gui::hud_block_window`) both draw with these, so they read as one system.

use eframe::egui::{self, pos2, vec2, Color32, FontFamily, FontId, Painter, Pos2, Rect, Rounding, Sense, Stroke, Ui};
use std::collections::HashMap;
use std::sync::Mutex;

/// `Stroke::new` takes `impl Into<f32>`, which leaves float literals untyped.
fn stroke(width: f32, colour: Color32) -> Stroke {
    Stroke::new(width, colour)
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

// ------------------------------------------------------------------------------- motion ----

/// Quick and crisp: nothing of the HUD's own moves for longer than a quarter of a second, it
/// stops smoothly, and what glides to a place runs a hair past it and settles.
pub mod motion {
    use std::time::Duration;

    /// A hover, a press, a light changing.
    pub const FAST: Duration = Duration::from_millis(120);
    /// A block gliding to its place, a panel sliding.
    pub const NORMAL: Duration = Duration::from_millis(180);
    /// A whole window coming up; the emblem crossing the screen.
    pub const SLOW: Duration = Duration::from_millis(250);

    /// A smooth stop.
    pub fn ease_out(t: f32) -> f32 {
        1.0 - (1.0 - t.clamp(0.0, 1.0)).powi(3)
    }

    /// A smooth stop that runs just under 2 % past its mark and comes back to it.
    pub fn settle(t: f32) -> f32 {
        if t <= 0.0 {
            return 0.0;
        }
        let (c, x) = (0.7, t.min(1.0) - 1.0);
        1.0 + (c + 1.0) * x.powi(3) + c * x.powi(2)
    }

    /// One frame of `from` closing in on `to` so that it is there (within 1 %) after `time`.
    pub fn approach(from: f32, to: f32, dt: f32, time: Duration) -> f32 {
        let rate = 4.6 / time.as_secs_f32().max(0.001);
        let next = to + (from - to) * (-rate * dt.clamp(0.0, 0.1)).exp();
        if (next - to).abs() < 0.002 { to } else { next }
    }
}

// -------------------------------------------------------------------------------- shell ----

/// The soft shadow a block floats on (under its frame, a little below it).
pub fn shadow(p: &Painter, r: Rect, alpha: f32) {
    if alpha <= 0.0 || !r.is_positive() {
        return;
    }
    let s = egui::epaint::Shadow { offset: vec2(0.0, 7.0), blur: 22.0, spread: 1.0, color: Color32::from_black_alpha((150.0 * alpha.min(1.0)) as u8) };
    p.add(s.as_shape(r, Rounding::ZERO));
}

/// How strongly the emblem (`orb`, its rect) lights a block at `r`: 1 right beside it, 0 from
/// one and a half emblems away, and nothing for a block that covers it.
pub fn light_strength(r: Rect, orb: Rect) -> f32 {
    let c = orb.center();
    if r.contains(c) || !orb.is_positive() {
        return 0.0;
    }
    let nearest = pos2(c.x.clamp(r.left(), r.right()), c.y.clamp(r.top(), r.bottom()));
    let gap = (nearest.distance(c) - orb.width() * 0.5).max(0.0);
    let reach = orb.width() * 1.5;
    (1.0 - gap / reach).clamp(0.0, 1.0).powi(2)
}

/// The emblem's light on a block: the edges that face it are lit, brightest at the point
/// nearest to it, fading along the edge and a few points into the block.
pub fn emblem_light(p: &Painter, r: Rect, orb: Rect, colour: Color32, alpha: f32) {
    let strength = light_strength(r, orb) * alpha;
    if strength <= 0.01 {
        return;
    }
    let c = orb.center();
    let half = orb.width() * 0.8;
    let depth = 16.0;
    let mut mesh = egui::Mesh::default();
    // (a point on the edge nearest the emblem, the edge's direction, the way into the block)
    let mut edges: Vec<(Pos2, egui::Vec2, egui::Vec2, f32, f32)> = Vec::new();
    if c.x < r.left() {
        edges.push((pos2(r.left(), c.y.clamp(r.top(), r.bottom())), vec2(0.0, 1.0), vec2(1.0, 0.0), r.top(), r.bottom()));
    }
    if c.x > r.right() {
        edges.push((pos2(r.right(), c.y.clamp(r.top(), r.bottom())), vec2(0.0, 1.0), vec2(-1.0, 0.0), r.top(), r.bottom()));
    }
    if c.y < r.top() {
        edges.push((pos2(c.x.clamp(r.left(), r.right()), r.top()), vec2(1.0, 0.0), vec2(0.0, 1.0), r.left(), r.right()));
    }
    if c.y > r.bottom() {
        edges.push((pos2(c.x.clamp(r.left(), r.right()), r.bottom()), vec2(1.0, 0.0), vec2(0.0, -1.0), r.left(), r.right()));
    }
    const STEPS: usize = 10;
    for (at, along, into, low, high) in edges {
        let coord = |q: Pos2| if along.x != 0.0 { q.x } else { q.y };
        let base = mesh.vertices.len() as u32;
        for k in 0..=STEPS {
            let t = k as f32 / STEPS as f32 * 2.0 - 1.0;
            let mut q = at + along * (t * half);
            // The lit stretch ends where the edge does.
            let over = coord(q).clamp(low, high) - coord(q);
            q += along * over;
            let a = strength * (1.0 - t * t).powi(2);
            mesh.colored_vertex(q, colour.gamma_multiply(0.55 * a));
            mesh.colored_vertex(q + into * depth, Color32::TRANSPARENT);
        }
        for k in 0..STEPS as u32 {
            let i = base + 2 * k;
            mesh.add_triangle(i, i + 1, i + 3);
            mesh.add_triangle(i, i + 3, i + 2);
        }
    }
    p.add(egui::Shape::mesh(mesh));
}

/// Places along a frame that depend on `seed` alone (a block's name), so they hold still from
/// frame to frame: each a corner (0-3), which of its two sides, how far from the corner and how
/// long.
fn nicks(seed: u64) -> [(usize, bool, f32, f32); 5] {
    let mut s = seed | 1;
    let mut next = || {
        s ^= s << 13;
        s ^= s >> 7;
        s ^= s << 17;
        (s >> 16) as u32
    };
    std::array::from_fn(|_| ((next() % 4) as usize, next() % 2 == 0, 10.0 + (next() % 34) as f32, 2.0 + (next() % 4) as f32))
}

/// A name's seed for `wear`.
pub fn seed_of(name: &str) -> u64 {
    name.bytes().fold(0xcbf2_9ce4_8422_2325u64, |h, b| (h ^ b as u64).wrapping_mul(0x0100_0000_01b3))
}

/// Wear on a frame: a few nicks in its border near the corners, where a real panel is handled,
/// and a faint scuff inside each. `fill` is what lies under the border.
pub fn wear(p: &Painter, r: Rect, fill: Color32, edge: Color32, seed: u64, alpha: f32) {
    if r.width() < 120.0 || r.height() < 90.0 || alpha <= 0.0 {
        return;
    }
    let corners = [(r.left_top(), 1.0, 1.0), (r.right_top(), -1.0, 1.0), (r.left_bottom(), 1.0, -1.0), (r.right_bottom(), -1.0, -1.0)];
    for (corner, horizontal, off, len) in nicks(seed) {
        let (at, dx, dy) = corners[corner];
        let (from, to, inward) = if horizontal {
            (at + vec2(dx * off, 0.0), at + vec2(dx * (off + len), 0.0), vec2(0.0, dy))
        } else {
            (at + vec2(0.0, dy * off), at + vec2(0.0, dy * (off + len)), vec2(dx, 0.0))
        };
        // The gap in the border, and the scuff the knock left just inside it.
        p.line_segment([from, to], stroke(2.4, fill.gamma_multiply(alpha)));
        p.line_segment([from + inward * 3.0, to + inward * 3.0 + (to - from) * 0.6], stroke(1.0, edge.gamma_multiply(0.10 * alpha)));
    }
}

// -------------------------------------------------------------------------------- glass ----

/// A fixed sequence of chance from `seed` (the same glass on every start).
fn chance(seed: u32) -> impl FnMut() -> f32 {
    let mut s = seed | 1;
    move || {
        s ^= s << 13;
        s ^= s >> 17;
        s ^= s << 5;
        (s >> 8) as f32 / (1u32 << 24) as f32
    }
}

/// How dark the display gets towards its corners (alpha of 255), and how many specks of dust
/// and hairline scratches lie on a screen.
const EDGE_FALLOFF: u8 = 58;
const DUST: usize = 90;
const SCRATCHES: usize = 16;

/// The glass the HUD is seen through ("medium" roughness: Sir, 2026-11-12): the light is not
/// quite even across it, it falls off towards the edges, and dust and a few hairline scratches
/// lie on it. All of it is still (it asks for no frame) and faint enough that text under it
/// stays sharp.
pub fn glass(p: &Painter, rect: Rect) {
    if rect.width() < 200.0 || rect.height() < 120.0 {
        return;
    }
    let mut mesh = egui::Mesh::default();
    // Uneven light: a coarse grid whose corners are a shade lighter or darker by chance.
    let (cols, rows) = (6usize, 4usize);
    let mut next = chance(0x51A9_E3B7);
    for j in 0..=rows {
        for i in 0..=cols {
            let at = pos2(rect.left() + rect.width() * i as f32 / cols as f32, rect.top() + rect.height() * j as f32 / rows as f32);
            let v = next();
            let colour = if v < 0.5 { Color32::from_black_alpha((v * 2.0 * 16.0) as u8) } else { Color32::from_white_alpha(((v - 0.5) * 2.0 * 5.0) as u8) };
            mesh.colored_vertex(at, colour);
        }
    }
    for j in 0..rows as u32 {
        for i in 0..cols as u32 {
            let a = j * (cols as u32 + 1) + i;
            let b = a + cols as u32 + 1;
            mesh.add_triangle(a, a + 1, b + 1);
            mesh.add_triangle(a, b + 1, b);
        }
    }
    // The falloff: clear in the middle, darker towards the edges, darkest in the corners.
    let inner = rect.shrink2(vec2(rect.width() * 0.16, rect.height() * 0.2));
    let ring = |r: Rect| [r.left_top(), r.center_top(), r.right_top(), r.right_center(), r.right_bottom(), r.center_bottom(), r.left_bottom(), r.left_center()];
    let base = mesh.vertices.len() as u32;
    for (k, at) in ring(rect).into_iter().enumerate() {
        mesh.colored_vertex(at, Color32::from_black_alpha(if k % 2 == 0 { EDGE_FALLOFF } else { EDGE_FALLOFF / 2 }));
    }
    for at in ring(inner) {
        mesh.colored_vertex(at, Color32::TRANSPARENT);
    }
    for k in 0..8u32 {
        let n = (k + 1) % 8;
        mesh.add_triangle(base + k, base + n, base + 8 + n);
        mesh.add_triangle(base + k, base + 8 + n, base + 8 + k);
    }
    // Dust: single pixels, a few of them two.
    let mut next = chance(0x0D57_C0DE);
    for _ in 0..DUST {
        let at = pos2(rect.left() + rect.width() * next(), rect.top() + rect.height() * next());
        let size = if next() < 0.15 { 2.0 } else { 1.0 };
        mesh.add_colored_rect(Rect::from_min_size(pos2(at.x.round(), at.y.round()), vec2(size, size)), Color32::from_white_alpha(10 + (next() * 22.0) as u8));
    }
    p.add(egui::Shape::mesh(mesh));
    // Hairline scratches: short, nearly straight, most of them leaning the same way (a cloth
    // wiped across the glass).
    let mut next = chance(0x5C2A_7C4B);
    for _ in 0..SCRATCHES {
        let from = pos2(rect.left() + rect.width() * next(), rect.top() + rect.height() * next());
        let lean = if next() < 0.75 { -0.5 + 0.35 * next() } else { 1.2 + 1.2 * next() };
        let len = 26.0 + 130.0 * next() * next();
        let to = from + vec2(lean.cos(), lean.sin()) * len;
        let bend = vec2(-lean.sin(), lean.cos()) * (len * 0.03 * (next() - 0.5));
        let alpha = 7 + (next() * 9.0) as u8;
        p.add(egui::Shape::line(vec![from, from + (to - from) * 0.5 + bend, to], stroke(1.0, Color32::from_white_alpha(alpha))));
    }
}

// ------------------------------------------------------------------------------- header ----

/// What a block's state light says.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Light {
    /// Nothing to report: the light is off.
    #[default]
    Rest,
    /// Loading or working: it pulses in the theme's colour.
    Busy,
    /// There and up to date.
    Good,
    /// Something to look at.
    Warn,
    /// It failed.
    Fault,
}

/// The lights the blocks have set, by block id.
static LIGHTS: Mutex<Option<HashMap<String, Light>>> = Mutex::new(None);

/// Block `id` reports its state (its header light shows it).
pub fn set_light(id: &str, light: Light) {
    let mut lights = lock(&LIGHTS);
    let map = lights.get_or_insert_with(HashMap::new);
    if light == Light::Rest {
        map.remove(id);
    } else {
        map.insert(id.to_string(), light);
    }
}

pub fn light(id: &str) -> Light {
    lock(&LIGHTS).as_ref().and_then(|m| m.get(id).copied()).unwrap_or_default()
}

/// Warnings in the HUD: amber, moved towards orange where the theme itself is yellow (so a
/// warning never looks like one of his own things).
pub fn warn_colour() -> Color32 {
    let (h, s, _) = crate::theme_engine::to_hsv(crate::theme_engine::current().core_primary);
    if s > 0.3 && (25.0..75.0).contains(&h) { Color32::from_rgb(255, 122, 26) } else { Color32::from_rgb(255, 176, 32) }
}

pub const FAULT: Color32 = Color32::from_rgb(255, 59, 92);
pub const GOOD: Color32 = Color32::from_rgb(120, 232, 176);

pub fn light_colour(light: Light) -> Color32 {
    match light {
        Light::Rest => Color32::from_rgb(0x4A, 0x5A, 0x66),
        Light::Busy => crate::hud_board::active_colour(),
        Light::Good => GOOD,
        Light::Warn => warn_colour(),
        Light::Fault => FAULT,
    }
}

/// The state light: a small lamp with a halo (pulsing while busy). Returns whether it moves.
pub fn paint_light(p: &Painter, centre: Pos2, light: Light, time: f64, alpha: f32) -> bool {
    let colour = light_colour(light);
    let pulse = if light == Light::Busy { 0.55 + 0.45 * ((time * 5.0).sin() as f32 * 0.5 + 0.5) } else { 1.0 };
    if light != Light::Rest {
        p.circle_filled(centre, 5.5, colour.gamma_multiply(0.16 * pulse * alpha));
    }
    p.circle_filled(centre, 2.6, colour.gamma_multiply(pulse * alpha));
    light == Light::Busy
}

/// Titles and labels: the display face.
pub fn title_font(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name("display".into()))
}

/// The height of every block's header row.
pub const HEADER_H: f32 = 24.0;

/// A header key (`[ PIN ]`, `[ CLOSE ]`) drawn with its right edge at `right`, centred on `mid`;
/// `lit` keeps it in the theme's colour (a pinned block's `[ PIN ]`). Returns its rect; the
/// caller asks whether it was clicked.
pub fn key(p: &Painter, right: f32, mid: f32, label: &str, hot: bool, down: bool, lit: bool, alpha: f32) -> Rect {
    let font = FontId::monospace(10.0);
    let galley = p.layout_no_wrap(label.to_string(), font, Color32::PLACEHOLDER);
    let r = Rect::from_min_size(pos2(right - galley.size().x - 10.0, mid - 8.0), vec2(galley.size().x + 10.0, 16.0));
    let accent = crate::hud_board::active_colour();
    let (fill, edge, ink) = if down {
        (accent.gamma_multiply(0.30), accent, Color32::WHITE)
    } else if hot {
        (accent.gamma_multiply(0.14), accent, Color32::WHITE)
    } else if lit {
        (accent.gamma_multiply(0.08), accent.gamma_multiply(0.9), accent)
    } else {
        (Color32::from_rgba_unmultiplied(3, 7, 12, 230), crate::hud_board::border_colour(), Color32::from_rgb(0xD4, 0xEC, 0xF5))
    };
    p.rect_filled(r, Rounding::ZERO, fill.gamma_multiply(alpha));
    p.rect_stroke(r.shrink(0.5), Rounding::ZERO, stroke(1.0, edge.gamma_multiply(alpha)));
    p.galley_with_override_text_color(pos2(r.left() + 5.0, mid - galley.size().y * 0.5 + if down { 0.5 } else { 0.0 }), galley, ink.gamma_multiply(alpha));
    r
}

/// A header key's width.
pub fn key_width(p: &Painter, label: &str) -> f32 {
    p.layout_no_wrap(label.to_string(), FontId::monospace(10.0), Color32::PLACEHOLDER).size().x + 10.0
}

/// The header keys of a block, right to left from `right`: each (label, lit, hint). Returns
/// which was clicked this frame and where the leftmost key begins. They light up under the
/// pointer and give under the press.
pub fn keys(ui: &mut Ui, id: egui::Id, right: f32, mid: f32, labels: &[(&str, bool, &str)], alpha: f32) -> (Option<usize>, f32) {
    let mut clicked = None;
    let mut x = right;
    for (k, (label, lit, hint)) in labels.iter().enumerate() {
        let width = key_width(ui.painter(), label);
        let r = Rect::from_min_size(pos2(x - width, mid - 8.0), vec2(width, 16.0));
        let response = ui.interact(r, id.with(("key", k)), Sense::click());
        key(ui.painter(), x, mid, label, response.hovered(), response.is_pointer_button_down_on(), *lit, alpha);
        if response.clicked() {
            clicked = Some(k);
        }
        if !hint.is_empty() {
            response.on_hover_text(*hint);
        }
        x -= width + 4.0;
    }
    (clicked, x + 4.0)
}

// ------------------------------------------------------------- loading, empty and failed ----

/// What kind of content is on its way (the outline drawn while it loads).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outline {
    /// Lines of text.
    Lines,
    /// A picture or a 3D view with a caption under it.
    Picture,
    /// A chart: an axis and bars.
    Chart,
    /// Rows with a mark in front.
    List,
}

/// The outline of what is coming: dim shapes where the content will stand, and a light that
/// passes over them. Asks for the next frame itself.
pub fn outline(ui: &Ui, r: Rect, kind: Outline) {
    if r.width() < 40.0 || r.height() < 24.0 {
        return;
    }
    let p = ui.painter_at(r);
    let time = ui.input(|i| i.time);
    let dim = Color32::from_white_alpha(13);
    let mut shapes: Vec<Rect> = Vec::new();
    match kind {
        Outline::Lines => {
            let mut y = r.top() + 4.0;
            let widths = [0.92, 0.78, 0.86, 0.55, 0.81, 0.67];
            for k in 0..((r.height() - 8.0) / 18.0).floor().clamp(1.0, 12.0) as usize {
                shapes.push(Rect::from_min_size(pos2(r.left(), y), vec2(r.width() * widths[k % widths.len()], 8.0)));
                y += 18.0;
            }
        }
        Outline::Picture => {
            let picture = Rect::from_min_max(r.min, pos2(r.right(), r.bottom() - 22.0));
            shapes.push(picture);
            shapes.push(Rect::from_min_size(pos2(r.left(), r.bottom() - 12.0), vec2(r.width() * 0.45, 8.0)));
        }
        Outline::Chart => {
            let n = ((r.width() / 26.0).floor() as usize).clamp(3, 24);
            let heights = [0.35, 0.6, 0.45, 0.8, 0.55, 0.7, 0.4, 0.9];
            let step = r.width() / n as f32;
            for k in 0..n {
                let h = (r.height() - 6.0) * heights[k % heights.len()];
                shapes.push(Rect::from_min_max(pos2(r.left() + step * k as f32 + 3.0, r.bottom() - 2.0 - h), pos2(r.left() + step * (k + 1) as f32 - 3.0, r.bottom() - 2.0)));
            }
            shapes.push(Rect::from_min_size(pos2(r.left(), r.bottom() - 1.0), vec2(r.width(), 1.0)));
        }
        Outline::List => {
            let mut y = r.top() + 4.0;
            let widths = [0.7, 0.5, 0.62, 0.44];
            for k in 0..((r.height() - 8.0) / 24.0).floor().clamp(1.0, 10.0) as usize {
                shapes.push(Rect::from_min_size(pos2(r.left(), y), vec2(10.0, 10.0)));
                shapes.push(Rect::from_min_size(pos2(r.left() + 18.0, y + 1.0), vec2((r.width() - 18.0) * widths[k % widths.len()], 8.0)));
                y += 24.0;
            }
        }
    }
    for s in &shapes {
        p.rect_filled(*s, Rounding::ZERO, dim);
    }
    // The passing light: a soft band that crosses the block every 1.4 s.
    let t = ((time / 1.4).fract()) as f32;
    let x = r.left() - 80.0 + (r.width() + 160.0) * t;
    let accent = crate::hud_board::active_colour();
    for s in &shapes {
        let band = Rect::from_min_max(pos2(x - 40.0, s.top()), pos2(x + 40.0, s.bottom())).intersect(*s);
        if band.is_positive() {
            p.rect_filled(band, Rounding::ZERO, accent.gamma_multiply(0.10));
        }
    }
    ui.ctx().request_repaint_after(std::time::Duration::from_millis(33));
}

/// One plain sentence in the middle of a block that has nothing to show.
pub fn empty(ui: &Ui, r: Rect, sentence: &str) {
    let p = ui.painter_at(r);
    let c = r.center();
    // A small empty frame above the words.
    let mark = Rect::from_center_size(c - vec2(0.0, 18.0), vec2(22.0, 16.0));
    p.rect_stroke(mark, Rounding::ZERO, stroke(1.0, Color32::from_white_alpha(50)));
    p.line_segment([mark.left_bottom(), mark.right_top()], stroke(1.0, Color32::from_white_alpha(30)));
    let font = FontId::proportional(12.5);
    let galley = p.layout(sentence.to_string(), font, Color32::from_rgb(0x9F, 0xB4, 0xC4), (r.width() - 24.0).max(60.0));
    p.galley(pos2(c.x - galley.size().x * 0.5, c.y), galley, Color32::PLACEHOLDER);
}

/// A block that failed: what went wrong, in a sentence, and a `[ RETRY ]` key. Returns whether
/// the key was clicked.
pub fn failed(ui: &mut Ui, r: Rect, what: &str) -> bool {
    let c = r.center();
    {
        let p = ui.painter_at(r);
        // A warning triangle in the fault colour.
        let top = c - vec2(0.0, 40.0);
        let tri = [top, top + vec2(11.0, 19.0), top + vec2(-11.0, 19.0)];
        p.add(egui::Shape::closed_line(tri.to_vec(), stroke(1.5, FAULT)));
        p.line_segment([top + vec2(0.0, 7.0), top + vec2(0.0, 13.0)], stroke(1.5, FAULT));
        p.circle_filled(top + vec2(0.0, 16.0), 0.9, FAULT);
        let font = FontId::proportional(12.5);
        let galley = p.layout(what.to_string(), font, Color32::from_rgb(0xE6, 0xEE, 0xF6), (r.width() - 24.0).max(60.0));
        p.galley(pos2(c.x - galley.size().x * 0.5, c.y - 12.0), galley, Color32::PLACEHOLDER);
    }
    let bottom = (c.y + 34.0).min(r.bottom() - 10.0);
    let width = key_width(ui.painter(), "[ RETRY ]");
    keys(ui, egui::Id::new(("retry", r.min.x as i32, r.min.y as i32)), c.x + width * 0.5, bottom, &[("[ RETRY ]", true, "Try again")], 1.0).0.is_some()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn motion_is_quick_and_settles_just_past_its_mark() {
        assert!(motion::SLOW <= Duration::from_millis(250) && motion::FAST >= Duration::from_millis(100));
        assert_eq!((motion::settle(0.0), motion::settle(1.0)), (0.0, 1.0));
        let peak = (0..=100).map(|k| motion::settle(k as f32 / 100.0)).fold(0.0f32, f32::max);
        assert!(peak > 1.005 && peak < 1.025, "a tiny overshoot, got {peak}");
        assert!((0..100).all(|k| motion::ease_out(k as f32 / 100.0) <= motion::ease_out((k + 1) as f32 / 100.0)));
        // Within 1 % of the mark after its time, at 60 frames a second.
        let mut x = 0.0;
        for _ in 0..(0.18f32 * 60.0).ceil() as usize {
            x = motion::approach(x, 1.0, 1.0 / 60.0, motion::NORMAL);
        }
        assert!(x > 0.985, "there after its time, got {x}");
    }

    #[test]
    fn the_emblem_lights_only_blocks_near_it() {
        let orb = Rect::from_center_size(pos2(500.0, 400.0), vec2(200.0, 200.0));
        let beside = Rect::from_min_size(pos2(610.0, 300.0), vec2(300.0, 200.0));
        let far = Rect::from_min_size(pos2(1200.0, 300.0), vec2(300.0, 200.0));
        let over = Rect::from_min_size(pos2(400.0, 300.0), vec2(300.0, 200.0));
        assert!(light_strength(beside, orb) > 0.8);
        assert_eq!(light_strength(far, orb), 0.0);
        assert_eq!(light_strength(over, orb), 0.0, "a block that covers the emblem is not lit by it");
        let middle = Rect::from_min_size(pos2(750.0, 300.0), vec2(300.0, 200.0));
        assert!(light_strength(middle, orb) < light_strength(beside, orb) && light_strength(middle, orb) > 0.0);
    }

    #[test]
    fn wear_holds_still_and_differs_between_blocks() {
        assert_eq!(nicks(seed_of("mixer")), nicks(seed_of("mixer")));
        assert_ne!(nicks(seed_of("mixer")), nicks(seed_of("telemetry")));
        for (corner, _, off, len) in nicks(seed_of("earth")) {
            assert!(corner < 4 && (10.0..44.0).contains(&off) && (2.0..6.0).contains(&len));
        }
    }

    /// The glass is faint (text under it stays sharp), still (it asks for no frame) and the
    /// same on every start.
    #[test]
    fn the_glass_is_faint_and_still() {
        let ctx = egui::Context::default();
        let run = || {
            ctx.run(egui::RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(1280.0, 720.0))), ..Default::default() }, |ctx| {
                let painter = ctx.layer_painter(egui::LayerId::background());
                glass(&painter, ctx.screen_rect());
            })
        };
        let _ = (run(), run());
        let out = run();
        let strongest = |shape: &egui::Shape| -> u8 {
            match shape {
                egui::Shape::Mesh(m) => m.vertices.iter().map(|v| v.color.a()).max().unwrap_or(0),
                egui::Shape::Path(path) => match path.stroke.color {
                    egui::epaint::ColorMode::Solid(c) => c.a(),
                    _ => 255,
                },
                _ => 0,
            }
        };
        let shapes: Vec<_> = out.shapes.iter().map(|s| strongest(&s.shape)).collect();
        assert_eq!(shapes.len(), 1 + SCRATCHES, "one mesh and the scratches");
        assert!(shapes[0] <= EDGE_FALLOFF && shapes[1..].iter().all(|a| *a > 0 && *a <= 16), "{shapes:?}");
        let wait = out.viewport_output.values().map(|v| v.repaint_delay).min().unwrap_or(Duration::MAX);
        assert!(wait > Duration::from_millis(500), "the glass never drives the frame rate: {wait:?}");
        let mut a = chance(7);
        let mut b = chance(7);
        assert!((0..50).all(|_| a() == b()) && (0..50).all(|_| (0.0..1.0).contains(&a())));
    }

    #[test]
    fn a_block_reports_its_state_and_rest_clears_it() {
        set_light("style-test-block", Light::Busy);
        assert_eq!(light("style-test-block"), Light::Busy);
        set_light("style-test-block", Light::Fault);
        assert_eq!(light("style-test-block"), Light::Fault);
        set_light("style-test-block", Light::Rest);
        assert_eq!(light("style-test-block"), Light::Rest);
        assert_eq!(light("never-set"), Light::Rest);
        assert_ne!(light_colour(Light::Fault), light_colour(Light::Warn));
    }
}
