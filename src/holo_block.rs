//! What a block is beyond its plate: its lamp (resting, busy, a caution, a fault), what it
//! shows while it loads, has nothing to show or failed, its title fitted into its header, a
//! list that fades where it scrolls on, the light the core throws on the edge that faces it,
//! and the throw of a block Sir flings.

use crate::holo::{self, Face};
use eframe::egui::{self, pos2, vec2, Color32, Painter, Pos2, Rect, Rounding, Ui};
use std::collections::HashMap;
use std::sync::Mutex;

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

// ------------------------------------------------------------------------------- scroll ----

/// A vertical scroll area whose content fades into the block at an edge it goes on past, with
/// a slim mark of where it stands: rows are never cut off hard.
pub fn faded<R>(ui: &mut Ui, area: egui::ScrollArea, add: impl FnOnce(&mut Ui) -> R) -> egui::scroll_area::ScrollAreaOutput<R> {
    faded_into(ui, area, holo::theme().glass(), add)
}

/// [`faded`] on a surface of another `fill`.
pub fn faded_into<R>(ui: &mut Ui, area: egui::ScrollArea, fill: Color32, add: impl FnOnce(&mut Ui) -> R) -> egui::scroll_area::ScrollAreaOutput<R> {
    let out = area.show(ui, add);
    let (inner, offset, full) = (out.inner_rect, out.state.offset.y, out.content_size.y);
    if full <= inner.height() + 1.0 || inner.height() < 30.0 {
        return out;
    }
    // Where rows can be seen: egui lets them paint a margin past the area, a block's frame may cut it short.
    let r = inner.expand(ui.visuals().clip_rect_margin).intersect(ui.clip_rect());
    let p = ui.painter_at(r);
    let band = (r.height() / 5.0).min(22.0);
    let fade = |from: f32, to: f32| holo::fade(&p, r.left(), r.right(), from, to, fill);
    if offset > 1.0 {
        fade(r.top(), r.top() + band);
    }
    if offset + inner.height() < full - 1.0 || r.bottom() < inner.bottom() - 1.0 {
        fade(r.bottom(), r.bottom() - band);
    }
    let h = (r.height() * r.height() / full).max(14.0);
    let y = r.top() + (r.height() - h) * (offset / (full - inner.height()).max(1.0)).clamp(0.0, 1.0);
    let ppp = ui.ctx().pixels_per_point();
    let t = holo::theme();
    p.rect_filled(Rect::from_min_size(pos2(holo::snap(r.right() - 2.0, ppp), r.top()), vec2(1.0 / ppp, r.height())), Rounding::ZERO, holo::a(t.ink(), 0.10));
    p.rect_filled(Rect::from_min_size(pos2(holo::snap(r.right() - 3.0, ppp), y), vec2(3.0 / ppp, h)), Rounding::ZERO, holo::a(t.accent(), 0.85));
    out
}

// -------------------------------------------------------------------------------- title ----

/// A block's title as it fits `width`: whole when it does; else, of a "FAMILY // NAME" title,
/// the name alone, else the family alone; and only then cut with an ellipsis.
pub fn fitted_title(p: &Painter, text: &str, size: f32, colour: Color32, width: f32) -> std::sync::Arc<egui::Galley> {
    let whole = title_galley(p, text, size, colour, f32::INFINITY);
    if whole.size().x <= width {
        return whole;
    }
    if let Some((family, name)) = text.split_once("//") {
        for part in [name.trim(), family.trim()] {
            if !part.is_empty() {
                let short = title_galley(p, part, size, colour, f32::INFINITY);
                if short.size().x <= width {
                    return short;
                }
            }
        }
    }
    title_galley(p, text, size, colour, width)
}

/// A title: wide capitals, tracked, on one line (cut with an ellipsis where it is too long for
/// `width`). (`size` is the panels' old measure; the wide face is set smaller to take the
/// same room.)
pub fn title_galley(p: &Painter, text: &str, size: f32, colour: Color32, width: f32) -> std::sync::Arc<egui::Galley> {
    holo::galley_within(p, Face::Label, text, size * 0.76, colour, width.max(10.0))
}


// --------------------------------------------------------------------------- core light ----

/// How strongly the core (`orb`, its rect) lights a block at `r`: 1 right beside it, 0 from
/// one and a half cores away, and nothing for a block that covers it.
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

/// The core's light on a block: the edges that face it are lit, brightest at the point nearest
/// to it, fading along the edge and a few points into the glass.
pub fn core_light(p: &Painter, r: Rect, orb: Rect, colour: Color32, alpha: f32) {
    let strength = light_strength(r, orb) * alpha;
    if strength <= 0.01 {
        return;
    }
    let c = orb.center();
    let half = orb.width() * 0.8;
    let depth = 22.0;
    let mut mesh = egui::Mesh::default();
    // (a point on the edge nearest the core, the edge's direction, the way into the block)
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
            // (Faint: light the core throws, not a glow of the block's own.)
            mesh.colored_vertex(q, holo::lit(colour, 0.26 * a));
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

// ------------------------------------------------------------------------- drag and throw ----

/// A block let go while it moved faster than this (points per second) is thrown.
pub const THROW_FROM: f32 = 420.0;
/// Flung away faster than this, a block closes (it waits dim in the tray for ten minutes).
pub const FLING_AWAY: f32 = 3000.0;

/// One frame of a thrown block: it glides on from `rect` at `velocity`, slowing down, and stops
/// at the edge of `bounds`. Returns where it is now and how fast; None once it rests.
pub fn glide_on(rect: Rect, velocity: egui::Vec2, bounds: Rect, dt: f32) -> Option<(Rect, egui::Vec2)> {
    let dt = dt.clamp(0.0, crate::holo_motion::LONGEST_STEP);
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

// --------------------------------------------------------------------------------- lamp ----

/// What a block's state mark says.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Lamp {
    /// Nothing to report: no mark.
    #[default]
    Rest,
    /// Loading or working: it blinks in the accent.
    Busy,
    /// There and up to date.
    Good,
    /// Something to look at.
    Warn,
    /// It failed.
    Fault,
}

/// The marks the blocks have set, by block id.
static LAMPS: Mutex<Option<HashMap<String, Lamp>>> = Mutex::new(None);

/// Block `id` reports its state (its header mark shows it).
pub fn set_lamp(id: &str, lamp: Lamp) {
    let mut lamps = lock(&LAMPS);
    let map = lamps.get_or_insert_with(HashMap::new);
    if lamp == Lamp::Rest {
        map.remove(id);
    } else {
        map.insert(id.to_string(), lamp);
    }
}

/// What block `id` last reported.
pub fn lamp(id: &str) -> Lamp {
    lock(&LAMPS).as_ref().and_then(|m| m.get(id).copied()).unwrap_or_default()
}


/// The colour a lamp shows in: ink for all is well, the accent while it works, a caution, the alert.
pub fn lamp_colour(lamp: Lamp) -> Color32 {
    match lamp {
        Lamp::Rest => holo::theme().faint(),
        Lamp::Busy => holo::theme().accent(),
        Lamp::Good => holo::theme().ink(),
        Lamp::Warn => holo::theme().warn(),
        Lamp::Fault => holo::theme().alert(),
    }
}

/// The state mark: a small steady tick for all is well, a square that blinks slowly while busy,
/// a diamond for trouble (the alert colour when it failed), nothing at rest. Returns whether
/// it moves.
pub fn paint_lamp(p: &Painter, centre: Pos2, lamp: Lamp, time: f64, alpha: f32) -> bool {
    let ppp = p.ctx().pixels_per_point();
    let c = pos2(holo::snap(centre.x, ppp), holo::snap(centre.y, ppp));
    match lamp {
        Lamp::Rest => {}
        Lamp::Good => {
            let s = holo::stroke(1.2, holo::a(holo::theme().ink(), 0.6 * alpha));
            p.line_segment([c + vec2(-3.0, 0.0), c + vec2(-1.0, 2.5)], s);
            p.line_segment([c + vec2(-1.0, 2.5), c + vec2(3.5, -2.5)], s);
        }
        Lamp::Busy => {
            // (A slow blink, eased: on, off, on, about once a second and a half.)
            let on = 0.5 + 0.5 * (time * std::f64::consts::TAU / 1.4).sin() as f32;
            let square = Rect::from_center_size(c, vec2(5.0, 5.0));
            let mut light = holo::Light::new(p);
            light.halo(&[square.left_center(), square.right_center()], false, 7.0, holo::lit(lamp_colour(lamp), 0.34 * on * alpha));
            light.paint(p);
            p.rect_filled(square, Rounding::ZERO, lamp_colour(lamp).gamma_multiply((0.2 + 0.8 * on) * alpha));
        }
        Lamp::Warn | Lamp::Fault => {
            let diamond = vec![c + vec2(0.0, -4.0), c + vec2(4.0, 0.0), c + vec2(0.0, 4.0), c + vec2(-4.0, 0.0)];
            p.add(egui::Shape::convex_polygon(diamond, lamp_colour(lamp).gamma_multiply(alpha), egui::Stroke::NONE));
        }
    }
    lamp == Lamp::Busy
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

/// The outline of what is coming: dim shapes where the content will stand, and a line of light
/// that passes over them. Asks for the next frame itself.
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
    let t = holo::theme();
    let dim = holo::a(t.ink(), 0.045);
    let mut shapes: Vec<Rect> = Vec::new();
    match kind {
        Outline::Lines => {
            let mut y = r.top() + 4.0;
            let widths = [0.92, 0.78, 0.86, 0.55, 0.81, 0.67];
            for k in 0..((r.height() - 8.0) / 18.0).floor().clamp(1.0, 12.0) as usize {
                shapes.push(Rect::from_min_size(pos2(r.left(), y), vec2(r.width() * widths[k % widths.len()], 6.0)));
                y += 18.0;
            }
        }
        Outline::Picture => {
            shapes.push(Rect::from_min_max(r.min, pos2(r.right(), r.bottom() - 22.0)));
            shapes.push(Rect::from_min_size(pos2(r.left(), r.bottom() - 12.0), vec2(r.width() * 0.45, 6.0)));
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
                shapes.push(Rect::from_min_size(pos2(r.left() + 18.0, y + 2.0), vec2((r.width() - 18.0) * widths[k % widths.len()], 6.0)));
                y += 24.0;
            }
        }
    }
    for (k, s) in shapes.iter().enumerate() {
        // (A picture to come is an empty slot, not a grey slab.)
        if kind == Outline::Picture && k == 0 {
            holo::slot(&p, *s, holo::a(t.ink(), 0.14));
        } else {
            p.rect_filled(*s, Rounding::ZERO, dim);
        }
    }
    // The scan that tells "still loading" from "empty": a line of light that sweeps across the
    // block every 1.4 s, a faint wake behind it.
    let x = holo::snap(r.left() + r.width() * (time / 1.4).fract() as f32, p.ctx().pixels_per_point());
    let mut wake = egui::Mesh::default();
    for (wx, c) in [(x - 60.0, Color32::TRANSPARENT), (x, holo::lit(t.accent(), 0.10))] {
        wake.colored_vertex(pos2(wx, r.top()), c);
        wake.colored_vertex(pos2(wx, r.bottom()), c);
    }
    wake.add_triangle(0, 1, 2);
    wake.add_triangle(1, 3, 2);
    p.add(egui::Shape::mesh(wake));
    let mut light = holo::Light::new(&p);
    light.neon(&[pos2(x, r.top()), pos2(x, r.bottom())], false, holo::px(&p), holo::a(t.hot(), 0.8), 0.7);
    light.paint(&p);
    p.ctx().request_repaint_after(std::time::Duration::from_millis(16));
}

/// One plain sentence in the middle of a block that has nothing to show.
pub fn empty(ui: &Ui, r: Rect, sentence: &str) {
    empty_on(&ui.painter_at(r), r, sentence);
}

/// `empty` for a painter.
pub fn empty_on(p: &Painter, r: Rect, sentence: &str) {
    crate::holo_kit::empty(&p.with_clip_rect(r.intersect(p.clip_rect())), r, sentence, 1.0);
}

/// A block that failed: what went wrong, in a sentence, and a RETRY key. Returns whether the
/// key was clicked.
pub fn failed(ui: &mut Ui, r: Rect, what: &str) -> bool {
    let c = r.center();
    failed_on(&ui.painter_at(r), r, what);
    let bottom = (c.y + 38.0).min(r.bottom() - 12.0);
    let width = crate::holo_kit::key_width(ui.painter(), "RETRY");
    crate::holo_ui::keys_at(ui, egui::Id::new(("retry", r.min.x as i32, r.min.y as i32)), c.x + width * 0.5, bottom, &[("RETRY", true, "Try again")], 1.0).0.is_some()
}

/// What went wrong, under the trouble mark: a diamond in hairlines (a painter's: no RETRY).
pub fn failed_on(p: &Painter, r: Rect, what: &str) {
    let p = p.with_clip_rect(r.intersect(p.clip_rect()));
    let c = r.center();
    let t = holo::theme();
    // Short blocks keep the sentence and drop the mark.
    let tall = r.height() >= 96.0;
    if tall {
        let m = c - vec2(0.0, 30.0);
        let diamond = vec![m + vec2(0.0, -11.0), m + vec2(11.0, 0.0), m + vec2(0.0, 11.0), m + vec2(-11.0, 0.0)];
        p.add(egui::Shape::closed_line(diamond, holo::stroke(1.2, t.alert())));
        p.line_segment([m + vec2(0.0, -5.0), m + vec2(0.0, 1.5)], holo::stroke(1.2, t.alert()));
        p.circle_filled(m + vec2(0.0, 4.5), 0.9, t.alert());
    }
    let galley = p.layout(what.to_string(), holo::font(Face::Body, 13.5), t.ink(), (r.width() - 24.0).max(60.0));
    let y = if tall { c.y - 12.0 } else { c.y - galley.size().y * 0.5 };
    p.galley(pos2(c.x - galley.size().x * 0.5, y), galley, Color32::PLACEHOLDER);
}

#[cfg(test)]
mod tests {
    use super::*;
    use eframe::egui::Vec2;

    #[test]
    fn the_core_lights_only_blocks_near_it() {
        let orb = Rect::from_center_size(pos2(500.0, 500.0), Vec2::splat(200.0));
        let beside = Rect::from_min_size(pos2(610.0, 420.0), vec2(200.0, 160.0));
        let far = Rect::from_min_size(pos2(1400.0, 420.0), vec2(200.0, 160.0));
        let over = Rect::from_center_size(orb.center(), vec2(300.0, 300.0));
        assert!(light_strength(beside, orb) > 0.8);
        assert_eq!(light_strength(far, orb), 0.0);
        assert_eq!(light_strength(over, orb), 0.0, "a block that covers it is not lit by it");
    }

    #[test]
    fn a_thrown_block_glides_on_and_comes_to_rest() {
        let bounds = Rect::from_min_size(pos2(0.0, 0.0), vec2(1000.0, 800.0));
        let mut rect = Rect::from_min_size(pos2(100.0, 100.0), vec2(200.0, 150.0));
        let mut v = vec2(900.0, 0.0);
        let mut frames = 0;
        while let Some((r, nv)) = glide_on(rect, v, bounds, 1.0 / 60.0) {
            assert!(r.left() >= rect.left(), "it glides the way it was thrown");
            (rect, v) = (r, nv);
            frames += 1;
            assert!(frames < 120, "it never comes to rest");
        }
        assert!(rect.left() > 150.0 && bounds.contains_rect(rect), "{rect:?}");
        // Thrown at the edge, it stops there.
        let (stopped, v) = glide_on(Rect::from_min_size(pos2(790.0, 100.0), vec2(200.0, 150.0)), vec2(3000.0, 100.0), bounds, 1.0 / 60.0).unwrap();
        assert_eq!((stopped.right(), v.x), (1000.0, 0.0));
    }


    #[test]
    fn a_block_reports_its_state_and_rest_clears_it() {
        set_lamp("test-block", Lamp::Busy);
        assert_eq!(lamp("test-block"), Lamp::Busy);
        set_lamp("test-block", Lamp::Rest);
        assert_eq!(lamp("test-block"), Lamp::Rest);
        assert_ne!(lamp_colour(Lamp::Warn), lamp_colour(Lamp::Fault));
    }
}
