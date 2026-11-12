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
    let s = egui::epaint::Shadow { offset: vec2(0.0, 6.0), blur: 18.0, spread: 0.0, color: Color32::from_black_alpha((140.0 * alpha.min(1.0)) as u8) };
    p.add(s.as_shape(r, Rounding::ZERO));
}

/// White at `share` of its strength as the eye sees it: laid over a dark surface it lifts it by
/// that share. (`Color32::from_white_alpha` counts in linear light: its 15 of 255 is a line a
/// quarter as bright as white, not a sixteenth.)
pub fn white(share: f32) -> Color32 {
    Color32::WHITE.gamma_multiply(share.clamp(0.0, 1.0))
}

/// `a` to `b` by `t`, channel by channel (both as they are stored: premultiplied).
fn lerp_colour(a: Color32, b: Color32, t: f32) -> Color32 {
    let ch = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t.clamp(0.0, 1.0)).round() as u8;
    Color32::from_rgba_premultiplied(ch(a.r(), b.r()), ch(a.g(), b.g()), ch(a.b(), b.b()), ch(a.a(), b.a()))
}

/// A block's surface: glass lit from above. The fill is a shade lighter at the top than at the
/// bottom, a hairline of light runs along its top edge, and a faint sheen lies across its upper
/// left corner. `alpha`: the block's own opacity (its entrance, a search that dims it).
pub fn surface(p: &Painter, r: Rect, fill: Color32, alpha: f32) {
    if !r.is_positive() || alpha <= 0.0 || fill.a() == 0 {
        return;
    }
    let a = fill.a();
    let top = lerp_colour(fill, Color32::from_rgba_premultiplied(20, 30, 42, a), 0.5).gamma_multiply(alpha);
    let bottom = lerp_colour(fill, Color32::from_rgba_premultiplied(1, 3, 6, a), 0.6).gamma_multiply(alpha);
    let mut mesh = egui::Mesh::default();
    mesh.colored_vertex(r.left_top(), top);
    mesh.colored_vertex(r.right_top(), top);
    mesh.colored_vertex(r.right_bottom(), bottom);
    mesh.colored_vertex(r.left_bottom(), bottom);
    mesh.add_triangle(0, 1, 2);
    mesh.add_triangle(0, 2, 3);
    // The sheen: light caught in the upper left corner of the glass.
    let sheen = white(0.03 * alpha);
    mesh.colored_vertex(r.left_top(), sheen);
    mesh.colored_vertex(r.left_top() + vec2((r.width() * 0.55).min(340.0), 0.0), Color32::TRANSPARENT);
    mesh.colored_vertex(r.left_top() + vec2(0.0, (r.height() * 0.8).min(240.0)), Color32::TRANSPARENT);
    mesh.add_triangle(4, 5, 6);
    p.add(egui::Shape::mesh(mesh));
    // The hairline of light along the top edge (inside the border).
    if r.height() > 6.0 && r.width() > 6.0 {
        p.rect_filled(Rect::from_min_max(pos2(r.left() + 1.0, r.top() + 1.0), pos2(r.right() - 1.0, r.top() + 2.0)), Rounding::ZERO, white(0.055 * alpha));
    }
}

/// The dark band a line of text stands on over the board: solid under the text, running out to
/// nothing at both ends (no box round it).
pub fn caption_band(p: &Painter, r: Rect, alpha: f32) {
    let dark = Color32::from_black_alpha((165.0 * alpha.clamp(0.0, 1.0)) as u8);
    let fade = (r.height() * 1.6).clamp(18.0, 60.0);
    let mut mesh = egui::Mesh::default();
    for (x, colour) in [(r.left() - fade, Color32::TRANSPARENT), (r.left() + 4.0, dark), (r.right() - 4.0, dark), (r.right() + fade, Color32::TRANSPARENT)] {
        mesh.colored_vertex(pos2(x, r.top()), colour);
        mesh.colored_vertex(pos2(x, r.bottom()), colour);
    }
    for k in 0..3u32 {
        let i = 2 * k;
        mesh.add_triangle(i, i + 2, i + 3);
        mesh.add_triangle(i, i + 3, i + 1);
    }
    p.add(egui::Shape::mesh(mesh));
}

/// How long the emblem's ripple runs (seconds).
pub const RIPPLE_SECS: f32 = 0.65;

/// The emblem's ripple when he hears his name: two rings that leave it and fade, `age` seconds
/// after it (his silent "yes?"). Returns whether it is still running.
pub fn ripple(p: &Painter, centre: Pos2, radius: f32, age: f32, colour: Color32) -> bool {
    if !(0.0..RIPPLE_SECS).contains(&age) {
        return false;
    }
    for delay in [0.0, 0.16] {
        let t = (age - delay) / (RIPPLE_SECS - delay);
        if !(0.0..1.0).contains(&t) {
            continue;
        }
        let out = motion::ease_out(t);
        let fade = (1.0 - t) * (1.0 - t);
        p.circle_stroke(centre, radius * (1.04 + 0.55 * out), stroke(0.8 + 2.2 * (1.0 - t), colour.gamma_multiply(0.75 * fade)));
    }
    true
}

/// A title as the HUD letters it: the display face, upper case, the letters a little apart, on
/// one line (cut with an ellipsis where it is too long for `width`).
pub fn title_galley(p: &Painter, text: &str, size: f32, colour: Color32, width: f32) -> std::sync::Arc<egui::Galley> {
    let mut job = egui::text::LayoutJob::default();
    job.append(&text.to_uppercase(), 0.0, egui::TextFormat { font_id: title_font(size), color: colour, extra_letter_spacing: size * 0.085, ..Default::default() });
    job.wrap.max_width = width.max(10.0);
    job.wrap.max_rows = 1;
    job.wrap.break_anywhere = true;
    job.wrap.overflow_character = Some('…');
    p.layout_job(job)
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
        // Where the border is worn thin (not cut: a gap reads as a fault of the drawing), and
        // the scuff the knock left just inside it.
        p.line_segment([from, to], stroke(2.4, fill.gamma_multiply(0.6 * alpha)));
        p.line_segment([from + inward * 3.0, to + inward * 3.0 + (to - from) * 0.6], stroke(1.0, edge.gamma_multiply(0.10 * alpha)));
    }
}

// ------------------------------------------------------------------------- drag and throw ----

/// How near (points) a dragged block's edge must come to a line to snap to it, and the gap two
/// blocks keep when one snaps beside the other.
pub const SNAP_REACH: f32 = 9.0;
pub const SNAP_GAP: f32 = 10.0;

/// A dragged block at `free` snapped to what is around it: the edges and middles of the other
/// blocks (and beside them, a gap apart) and of the screen. Returns where it lands and the
/// guide lines to draw (each from-to), at most one per axis.
pub fn snap(free: Rect, others: &[Rect], bounds: Rect, reach: f32) -> (Rect, Vec<[Pos2; 2]>) {
    // (the line's place, the nearer end of what it belongs to, the farther end)
    let mut xs: Vec<(f32, f32, f32)> = vec![(bounds.left(), bounds.top(), bounds.bottom()), (bounds.center().x, bounds.top(), bounds.bottom()), (bounds.right(), bounds.top(), bounds.bottom())];
    let mut ys: Vec<(f32, f32, f32)> = vec![(bounds.top(), bounds.left(), bounds.right()), (bounds.center().y, bounds.left(), bounds.right()), (bounds.bottom(), bounds.left(), bounds.right())];
    for o in others {
        for x in [o.left(), o.center().x, o.right(), o.left() - SNAP_GAP, o.right() + SNAP_GAP] {
            xs.push((x, o.top(), o.bottom()));
        }
        for y in [o.top(), o.center().y, o.bottom(), o.top() - SNAP_GAP, o.bottom() + SNAP_GAP] {
            ys.push((y, o.left(), o.right()));
        }
    }
    // The nearest line within reach of one of the block's own three (two edges and the middle).
    let nearest = |mine: [f32; 3], lines: &[(f32, f32, f32)]| -> Option<(f32, (f32, f32, f32))> {
        let mut best: Option<(f32, (f32, f32, f32))> = None;
        for line in lines {
            for m in mine {
                let d = line.0 - m;
                if d.abs() <= reach && best.is_none_or(|(b, _)| d.abs() < b.abs()) {
                    best = Some((d, *line));
                }
            }
        }
        best
    };
    let dx = nearest([free.left(), free.center().x, free.right()], &xs);
    let dy = nearest([free.top(), free.center().y, free.bottom()], &ys);
    let landed = free.translate(vec2(dx.map_or(0.0, |d| d.0), dy.map_or(0.0, |d| d.0)));
    let mut guides = Vec::new();
    if let Some((_, (x, a, b))) = dx {
        guides.push([pos2(x, a.min(landed.top())), pos2(x, b.max(landed.bottom()))]);
    }
    if let Some((_, (y, a, b))) = dy {
        guides.push([pos2(a.min(landed.left()), y), pos2(b.max(landed.right()), y)]);
    }
    (landed, guides)
}

/// The guide lines of a snap, drawn over everything while the block is held.
pub fn paint_guides(ctx: &egui::Context, guides: &[[Pos2; 2]]) {
    if guides.is_empty() {
        return;
    }
    let p = ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::new("hud_snap_guides")));
    let colour = crate::hud_board::active_colour();
    for [from, to] in guides {
        p.line_segment([*from, *to], stroke(1.0, colour.gamma_multiply(0.75)));
        for end in [from, to] {
            p.circle_filled(*end, 2.0, colour);
        }
    }
}

/// A block let go while it moved faster than this (points per second) is thrown.
pub const THROW_FROM: f32 = 420.0;

/// One frame of a thrown block: it glides on from `rect` at `velocity`, slowing down (it has
/// lost its way after about a third of a second), and stops at the edge of `bounds`. Returns
/// where it is now and how fast; None once it rests.
pub fn glide_on(rect: Rect, velocity: egui::Vec2, bounds: Rect, dt: f32) -> Option<(Rect, egui::Vec2)> {
    let dt = dt.clamp(0.0, 0.05);
    let velocity = velocity * (-9.0 * dt).exp();
    if velocity.length() < 24.0 {
        return None;
    }
    let mut next = rect.translate(velocity * dt);
    let mut v = velocity;
    // The edge of the screen stops it (on that axis).
    if next.left() < bounds.left() || next.right() > bounds.right() {
        next = next.translate(vec2((bounds.left() - next.left()).max(0.0) + (bounds.right() - next.right()).min(0.0), 0.0));
        v.x = 0.0;
    }
    if next.top() < bounds.top() || next.bottom() > bounds.bottom() {
        next = next.translate(vec2(0.0, (bounds.top() - next.top()).max(0.0) + (bounds.bottom() - next.bottom()).min(0.0)));
        v.y = 0.0;
    }
    (v.length() >= 24.0).then_some((next, v))
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
            let colour = if v < 0.5 { Color32::from_black_alpha((v * 2.0 * 16.0) as u8) } else { white((v - 0.5) * 2.0 * 0.02) };
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
        mesh.add_colored_rect(Rect::from_min_size(pos2(at.x.round(), at.y.round()), vec2(size, size)), white(0.04 + next() * 0.085));
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
        let strength = 0.03 + next() * 0.045;
        p.add(egui::Shape::line(vec![from, from + (to - from) * 0.5 + bend, to], stroke(1.0, white(strength))));
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
        (accent.gamma_multiply(0.10), accent.gamma_multiply(0.9), accent)
    } else {
        // At rest a key is quiet: a hairline box and grey letters. It lights up under the
        // pointer, and the whole row comes up when the pointer is over its block.
        (Color32::from_rgba_unmultiplied(3, 7, 12, 150), crate::hud_board::border_colour().gamma_multiply(0.7), Color32::from_rgb(0xA9, 0xC0, 0xCE))
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
    outline_on(&ui.painter_at(r), r, kind);
}

/// `outline` for a painter (the board's blocks).
pub fn outline_on(p: &Painter, r: Rect, kind: Outline) {
    if r.width() < 40.0 || r.height() < 24.0 {
        return;
    }
    let p = p.with_clip_rect(r.intersect(p.clip_rect()));
    let time = p.ctx().input(|i| i.time);
    let dim = white(0.045);
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
    for (k, s) in shapes.iter().enumerate() {
        // (A picture to come is a frame, not a grey slab.)
        if kind == Outline::Picture && k == 0 {
            p.rect_filled(*s, Rounding::ZERO, white(0.018));
            p.rect_stroke(s.shrink(0.5), Rounding::ZERO, stroke(1.0, white(0.12)));
        } else {
            p.rect_filled(*s, Rounding::ZERO, dim);
        }
    }
    // The passing light: a soft band that crosses the block every 1.4 s.
    let t = ((time / 1.4).fract()) as f32;
    let x = r.left() - 80.0 + (r.width() + 160.0) * t;
    let accent = crate::hud_board::active_colour();
    for s in &shapes {
        let band = Rect::from_min_max(pos2(x - 40.0, s.top()), pos2(x + 40.0, s.bottom())).intersect(*s);
        if band.is_positive() {
            p.rect_filled(band, Rounding::ZERO, accent.gamma_multiply(0.08));
        }
    }
    p.ctx().request_repaint_after(std::time::Duration::from_millis(33));
}

/// One plain sentence in the middle of a block that has nothing to show.
pub fn empty(ui: &Ui, r: Rect, sentence: &str) {
    empty_on(&ui.painter_at(r), r, sentence);
}

/// `empty` for a painter.
pub fn empty_on(p: &Painter, r: Rect, sentence: &str) {
    let p = p.with_clip_rect(r.intersect(p.clip_rect()));
    let c = r.center();
    // A small empty frame above the words.
    let mark = Rect::from_center_size(c - vec2(0.0, 18.0), vec2(22.0, 16.0));
    p.rect_stroke(mark, Rounding::ZERO, stroke(1.0, white(0.3)));
    p.line_segment([mark.left_bottom(), mark.right_top()], stroke(1.0, white(0.16)));
    let font = FontId::proportional(12.5);
    let galley = p.layout(sentence.to_string(), font, Color32::from_rgb(0x9F, 0xB4, 0xC4), (r.width() - 24.0).max(60.0));
    p.galley(pos2(c.x - galley.size().x * 0.5, c.y), galley, Color32::PLACEHOLDER);
}

/// A block that failed: what went wrong, in a sentence, and a `[ RETRY ]` key. Returns whether
/// the key was clicked.
pub fn failed(ui: &mut Ui, r: Rect, what: &str) -> bool {
    let c = r.center();
    failed_on(&ui.painter_at(r), r, what);
    let bottom = (c.y + 34.0).min(r.bottom() - 10.0);
    let width = key_width(ui.painter(), "[ RETRY ]");
    keys(ui, egui::Id::new(("retry", r.min.x as i32, r.min.y as i32)), c.x + width * 0.5, bottom, &[("[ RETRY ]", true, "Try again")], 1.0).0.is_some()
}

/// What went wrong, under a warning triangle (a painter's: no `[ RETRY ]`).
pub fn failed_on(p: &Painter, r: Rect, what: &str) {
    let p = p.with_clip_rect(r.intersect(p.clip_rect()));
    let c = r.center();
    // Short blocks keep the sentence and drop the triangle.
    let tall = r.height() >= 96.0;
    if tall {
        let top = c - vec2(0.0, 40.0);
        let tri = [top, top + vec2(11.0, 19.0), top + vec2(-11.0, 19.0)];
        p.add(egui::Shape::closed_line(tri.to_vec(), stroke(1.5, FAULT)));
        p.line_segment([top + vec2(0.0, 7.0), top + vec2(0.0, 13.0)], stroke(1.5, FAULT));
        p.circle_filled(top + vec2(0.0, 16.0), 0.9, FAULT);
    }
    let font = FontId::proportional(12.5);
    let galley = p.layout(what.to_string(), font, Color32::from_rgb(0xE6, 0xEE, 0xF6), (r.width() - 24.0).max(60.0));
    let y = if tall { c.y - 12.0 } else { c.y - galley.size().y * 0.5 };
    p.galley(pos2(c.x - galley.size().x * 0.5, y), galley, Color32::PLACEHOLDER);
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

    /// A dragged block lines up with its neighbours and the screen, keeps a gap beside a
    /// neighbour, and far from every line it stays where the hand has it.
    #[test]
    fn a_dragged_block_snaps_to_what_is_near() {
        let bounds = Rect::from_min_size(pos2(0.0, 0.0), vec2(1600.0, 900.0));
        let other = Rect::from_min_size(pos2(200.0, 100.0), vec2(300.0, 200.0));
        // Its top five points under the neighbour's top, its left edge near the gap beside it.
        let free = Rect::from_min_size(pos2(516.0, 105.0), vec2(270.0, 160.0));
        let (landed, guides) = snap(free, &[other], bounds, SNAP_REACH);
        assert_eq!((landed.left(), landed.top()), (other.right() + SNAP_GAP, other.top()));
        assert_eq!(landed.size(), free.size(), "a snap moves, it never resizes");
        assert_eq!(guides.len(), 2);
        // The middle of the screen.
        let centred = Rect::from_center_size(pos2(803.0, 600.0), vec2(200.0, 100.0));
        assert_eq!(snap(centred, &[], bounds, SNAP_REACH).0.center().x, 800.0);
        // Nothing near: it stays, and no line is drawn.
        let alone = Rect::from_min_size(pos2(1000.0, 500.0), vec2(200.0, 90.0));
        let (same, none) = snap(alone, &[other], bounds, SNAP_REACH);
        assert_eq!(same, alone);
        assert!(none.is_empty());
    }

    /// A thrown block glides on, slows down, rests within half a second and never leaves the
    /// screen.
    #[test]
    fn a_thrown_block_glides_on_and_comes_to_rest() {
        let bounds = Rect::from_min_size(pos2(0.0, 0.0), vec2(1600.0, 900.0));
        let start = Rect::from_min_size(pos2(400.0, 300.0), vec2(300.0, 200.0));
        let (mut rect, mut v, mut frames) = (start, vec2(1500.0, 0.0), 0);
        while let Some((r, next)) = glide_on(rect, v, bounds, 1.0 / 60.0) {
            assert!(next.length() < v.length(), "it only ever slows down");
            (rect, v, frames) = (r, next, frames + 1);
            assert!(frames < 60, "still gliding after a second");
        }
        let way = rect.left() - start.left();
        assert!((80.0..260.0).contains(&way), "glided {way} points in {frames} frames");
        assert!(frames <= 30, "at rest within half a second: {frames} frames");
        // Thrown at the edge, it stops there.
        let mut rect = Rect::from_min_size(pos2(1250.0, 300.0), vec2(300.0, 200.0));
        let mut v = vec2(4000.0, 0.0);
        while let Some((r, next)) = glide_on(rect, v, bounds, 1.0 / 60.0) {
            (rect, v) = (r, next);
        }
        assert!(rect.right() <= bounds.right() + 0.01, "{rect:?}");
        assert!(glide_on(start, vec2(10.0, 0.0), bounds, 1.0 / 60.0).is_none(), "a slow release is no throw");
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
        assert!(shapes[0] <= EDGE_FALLOFF && shapes[1..].iter().all(|a| *a > 0 && *a <= 20), "{shapes:?}");
        // "White at a share" is what it says: a twentieth lifts a dark surface by a twentieth.
        assert_eq!(white(0.05), Color32::from_rgba_premultiplied(13, 13, 13, 13));
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
