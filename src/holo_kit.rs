//! Instruments and controls, painted from the hologram kit. A control is a slanted key (the
//! films' tabs lean), selected is a solid block of the accent with its label cut out in black,
//! a reading is a thin large figure with a small wide label, a level is a row of cells or an
//! arc with a filament. Nothing is rounded, nothing is boxed twice.
//!
//! These paint; `holo_ui` wraps them into egui widgets that answer the pointer.

use crate::holo::{self, a, lit, mix, polar, px, Face, Light, NORTH};
use eframe::egui::{pos2, vec2, Align2, Color32, Mesh, Painter, Pos2, Rect, Shape, Stroke};
use std::f32::consts::TAU;

/// A key's height and how far its sides lean.
pub const KEY_H: f32 = 20.0;
pub const LEAN: f32 = 5.0;
const KEY_TEXT: f32 = 8.5;

/// A rectangle whose left and right sides lean: the shape of every key and tab.
pub fn leaning(r: Rect) -> [Pos2; 4] {
    let s = LEAN.min(r.width() / 3.0);
    [pos2(r.left() + s, r.top()), pos2(r.right(), r.top()), pos2(r.right() - s, r.bottom()), pos2(r.left(), r.bottom())]
}

pub fn key_width(p: &Painter, label: &str) -> f32 {
    holo::galley(p, Face::Label, label, KEY_TEXT, Color32::WHITE).size().x + 22.0
}

/// A key. `hot` is how far the pointer has lit it (0-1), `on` whether it is the chosen one.
pub fn key(p: &Painter, r: Rect, label: &str, hot: f32, down: bool, on: bool, alpha: f32) {
    key_tinted(p, r, label, hot, down, on, alpha, None);
}

/// The same in a colour of its own (`tint`): the alert for a key that ends or deletes
/// something. Such a key's frame and lettering wear the colour even at rest.
#[allow(clippy::too_many_arguments)]
pub fn key_tinted(p: &Painter, r: Rect, label: &str, hot: f32, down: bool, on: bool, alpha: f32, tint: Option<Color32>) {
    let t = holo::theme();
    let colour = tint.unwrap_or(t.accent());
    let bright = mix(colour, Color32::WHITE, 0.62);
    let shape = leaning(r);
    let (fill, edge, ink) = if down {
        (a(bright, alpha), a(bright, alpha), Color32::BLACK)
    } else if on {
        (a(mix(colour, bright, 0.35 * hot), alpha), a(colour, alpha), Color32::BLACK)
    } else if tint.is_some() {
        (a(colour, 0.16 * hot * alpha), a(colour, (0.55 + 0.45 * hot) * alpha), a(mix(colour, bright, hot), alpha))
    } else {
        (a(t.ink(), 0.10 * hot * alpha), a(mix(a(t.ink(), 0.34), colour, hot), alpha), a(mix(t.muted(), t.ink(), 0.4 + 0.6 * hot), alpha))
    };
    if on || down || hot > 0.3 {
        let mut light = Light::new(p);
        let mut closed = shape.to_vec();
        closed.push(shape[0]);
        light.halo(&closed, false, 7.0, lit(colour, (if on || down { 0.26 } else { 0.16 * hot }) * alpha));
        light.paint(p);
    }
    holo::fill(p, &shape, fill);
    holo::outline(p, &shape, edge);
    holo::text(p, r.center(), Align2::CENTER_CENTER, Face::Label, label, KEY_TEXT, ink);
}

/// A switch: a small frame with a block that sits left (off) or right (on). `on` runs 0-1 as
/// it moves.
pub fn toggle(p: &Painter, r: Rect, on: f32, hot: f32, alpha: f32) {
    let t = holo::theme();
    let frame = Rect::from_center_size(r.center(), vec2(34.0, 14.0));
    holo::outline(p, &leaning(frame), a(mix(a(t.ink(), 0.36), t.accent(), on.max(hot * 0.6)), alpha));
    let block = Rect::from_min_size(pos2(frame.left() + 3.0 + on * 14.0, frame.top() + 3.0), vec2(14.0, 8.0));
    if on > 0.5 {
        let mut light = Light::new(p);
        light.halo(&[block.left_center(), block.right_center()], false, 8.0, lit(t.accent(), 0.34 * alpha));
        light.paint(p);
    }
    holo::fill(p, &leaning(block), a(mix(a(t.ink(), 0.45), t.accent(), on), alpha));
}

/// A slider: a hairline with a ruler under it, light up to the value, a caret on it.
pub fn slider(p: &Painter, r: Rect, value: f32, hot: f32, alpha: f32) {
    let t = holo::theme();
    let y = r.center().y;
    let x = r.left() + r.width() * value.clamp(0.0, 1.0);
    holo::rule(p, pos2(r.left(), y), pos2(r.right(), y), a(t.ink(), 0.26 * alpha));
    for i in 0..=10 {
        let tx = r.left() + r.width() * i as f32 / 10.0;
        holo::rule(p, pos2(tx, y + 3.0), pos2(tx, y + if i % 5 == 0 { 8.0 } else { 5.5 }), a(t.ink(), if i % 5 == 0 { 0.45 } else { 0.24 } * alpha));
    }
    let mut light = Light::new(p);
    light.neon(&[pos2(r.left(), y), pos2(x, y)], false, 1.6, a(mix(t.accent(), t.hot(), 0.4), alpha), 0.9);
    light.paint(p);
    let caret = Rect::from_center_size(pos2(x, y), vec2(3.0 + 1.5 * hot, 13.0));
    p.rect_filled(caret, 0.0, a(t.hot(), alpha));
}

/// A level as a row of cells: lit up to `value` (0-1). `colour` lights them (the accent, or
/// the alert for a level that is too high).
pub fn cells(p: &Painter, r: Rect, value: f32, count: usize, colour: Color32, alpha: f32) {
    let t = holo::theme();
    let count = count.max(1);
    let gap = 2.0;
    let w = ((r.width() + gap) / count as f32 - gap).max(1.0);
    let lit_to = value.clamp(0.0, 1.0) * count as f32;
    let mut mesh = Mesh::default();
    for i in 0..count {
        let cell = Rect::from_min_size(pos2(holo::snap(r.left() + i as f32 * (w + gap), holo::ppp(p)), r.top()), vec2(w, r.height()));
        let share = (lit_to - i as f32).clamp(0.0, 1.0);
        mesh.add_colored_rect(cell, mix(a(t.ink(), 0.11 * alpha), a(colour, alpha), share));
    }
    p.add(Shape::mesh(mesh));
}

/// A reading round a ring: a faint band, an arc of light in `colour` (the accent, or a caution
/// for a reading that is too high) from twelve o'clock to the value,
/// the figure in the middle and its label under it.
#[allow(clippy::too_many_arguments)]
pub fn gauge(p: &Painter, centre: Pos2, radius: f32, value: f32, figure: &str, caption: &str, colour: Color32, alpha: f32) {
    let t = holo::theme();
    let value = value.clamp(0.0, 1.0);
    let mut light = Light::new(p);
    let w = px(p);
    light.band(centre, radius * 0.84, radius, 0.0, TAU, a(t.ink(), 0.07 * alpha));
    light.ticks(centre, radius * 1.05, radius * 1.11, 0.0, TAU, 24, w, a(t.ink(), 0.30 * alpha));
    if value > 0.002 {
        let to = NORTH + value * TAU;
        // Glass lit from the filament, as on the core's band.
        let n = ((value * TAU * radius / 3.5).ceil() as usize).max(4);
        let ring = |r: f32| (0..=n).map(|i| polar(centre, r, NORTH + (to - NORTH) * i as f32 / n as f32)).collect::<Vec<_>>();
        light.sheet(&ring(radius * 0.84), &ring(radius), &[lit(colour, 0.20 * alpha)], &[lit(colour, 0.02 * alpha)]);
        light.neon_arc(centre, radius * 0.84, NORTH, to, (radius * 0.035).max(1.4), a(mix(colour, Color32::WHITE, 0.62), alpha), 1.0);
        light.spark(polar(centre, radius * 0.84, to), radius * 0.09, lit(mix(colour, Color32::WHITE, 0.62), alpha));
    }
    light.paint(p);
    holo::text(p, pos2(centre.x, centre.y - radius * 0.06), Align2::CENTER_CENTER, Face::Figure, figure, radius * 0.60, a(t.ink(), alpha));
    if !caption.is_empty() {
        holo::text(p, pos2(centre.x, centre.y + radius * 0.36), Align2::CENTER_CENTER, Face::Label, caption, (radius * 0.15).clamp(6.5, 9.0), a(t.muted(), alpha));
    }
}

/// A series as a line of light over its own glow: the last value marked, three faint levels
/// behind it. `values` are scaled between `lo` and `hi`.
pub fn area(p: &Painter, r: Rect, values: &[f32], lo: f32, hi: f32, colour: Color32, alpha: f32) {
    let t = holo::theme();
    for k in 0..=3 {
        let y = r.bottom() - r.height() * k as f32 / 3.0;
        holo::rule(p, pos2(r.left(), y), pos2(r.right(), y), a(t.ink(), if k == 0 { 0.22 } else { 0.07 } * alpha));
    }
    if values.len() < 2 {
        return;
    }
    let at = |i: usize| pos2(r.left() + r.width() * i as f32 / (values.len() - 1) as f32, r.bottom() - r.height() * ((values[i] - lo) / (hi - lo).max(1e-6)).clamp(0.0, 1.0));
    let points: Vec<Pos2> = (0..values.len()).map(at).collect();
    let mut mesh = Mesh::default();
    for q in &points {
        mesh.colored_vertex(*q, lit(colour, 0.15 * alpha));
        mesh.colored_vertex(pos2(q.x, r.bottom()), Color32::TRANSPARENT);
    }
    for i in 0..points.len() as u32 - 1 {
        mesh.add_triangle(i * 2, i * 2 + 1, i * 2 + 2);
        mesh.add_triangle(i * 2 + 1, i * 2 + 3, i * 2 + 2);
    }
    p.add(Shape::mesh(mesh));
    let mut light = Light::new(p);
    light.neon(&points, false, 1.5, a(mix(colour, t.hot(), 0.35), alpha), 0.9);
    let last = *points.last().unwrap();
    light.spark(last, 5.0, lit(t.hot(), alpha));
    light.paint(p);
    p.circle_filled(last, 2.2, a(t.hot(), alpha));
    p.line_segment([last, pos2(last.x, r.bottom())], Stroke::new(px(p), a(t.ink(), 0.30 * alpha)));
}

/// A row of a list: its number, its title, what belongs at the right. The chosen row has a
/// bar of the accent on its left and stands on lighter glass.
pub fn row(p: &Painter, r: Rect, index: &str, title: &str, meta: &str, chosen: f32, hot: f32, alpha: f32) {
    let t = holo::theme();
    let lift = (chosen * 0.9 + hot * 0.5).min(1.0);
    if lift > 0.01 {
        p.rect_filled(r, 0.0, a(t.ink(), 0.055 * lift * alpha));
    }
    if chosen > 0.01 {
        p.rect_filled(Rect::from_min_size(r.min, vec2(2.0, r.height())), 0.0, a(t.accent(), chosen * alpha));
    }
    holo::rule(p, r.left_bottom(), r.right_bottom(), a(t.ink(), 0.07 * alpha));
    let mid = r.center().y;
    let mut x = r.left() + 10.0;
    if !index.is_empty() {
        let drawn = holo::text(p, pos2(x, mid), Align2::LEFT_CENTER, Face::Mono, index, 10.5, a(mix(t.faint(), t.accent(), chosen), alpha));
        x = (x + 26.0).max(drawn.right() + 12.0);
    }
    let meta_w = if meta.is_empty() { 0.0 } else { holo::text(p, pos2(r.right() - 10.0, mid), Align2::RIGHT_CENTER, Face::Mono, meta, 10.5, a(t.muted(), alpha)).width() + 14.0 };
    let g = holo::galley_within(p, Face::Body, title, 13.5, a(mix(t.muted(), t.ink(), 0.55 + 0.45 * lift), alpha), (r.right() - 10.0 - meta_w - x).max(1.0));
    p.galley(pos2(x, holo::snap(mid - g.size().y / 2.0, holo::ppp(p))), g, t.ink());
}

/// A heading inside a panel: its number, a slash, its name, and a rule to the right edge.
pub fn section(p: &Painter, at: Pos2, width: f32, index: &str, title: &str, alpha: f32) {
    let t = holo::theme();
    let n = holo::text(p, at, Align2::LEFT_TOP, Face::Mono, index, 10.5, a(t.accent(), alpha));
    let slash = holo::text(p, pos2(n.right() + 5.0, at.y), Align2::LEFT_TOP, Face::Mono, "/", 10.5, a(t.faint(), alpha));
    let name = holo::text(p, pos2(slash.right() + 6.0, at.y + 1.0), Align2::LEFT_TOP, Face::Label, title, 8.5, a(t.ink(), 0.9 * alpha));
    holo::rule(p, pos2(name.right() + 10.0, name.center().y), pos2(at.x + width, name.center().y), a(t.ink(), 0.13 * alpha));
}

/// A tag: a small block, filled when it marks something live.
pub fn chip(p: &Painter, at: Pos2, text: &str, colour: Color32, filled: bool, alpha: f32) -> Rect {
    let g = holo::galley(p, Face::Label, text, 8.0, Color32::BLACK);
    let r = Rect::from_min_size(at, vec2(g.size().x + 12.0, 14.0));
    if filled {
        p.rect_filled(r, 0.0, a(colour, alpha));
        p.galley(pos2(r.left() + 6.0, r.center().y - g.size().y / 2.0), g, Color32::BLACK);
    } else {
        holo::outline(p, &holo::plate(r, [0.0; 4]), a(colour, 0.7 * alpha));
        holo::text(p, r.center(), Align2::CENTER_CENTER, Face::Label, text, 8.0, a(colour, alpha));
    }
    r
}

/// A label and its value on one line, a dotted line between them.
pub fn pair(p: &Painter, r: Rect, label: &str, value: &str, alpha: f32) {
    let t = holo::theme();
    let mid = r.center().y;
    let l = holo::text(p, pos2(r.left(), mid), Align2::LEFT_CENTER, Face::Label, label, 8.0, a(t.muted(), alpha));
    // (A value too long for the line is cut, never drawn over its label.)
    let g = holo::galley_within(p, Face::Mono, value, 11.5, a(t.ink(), alpha), (r.right() - l.right() - 16.0).max(20.0));
    let v = Rect::from_min_size(pos2(holo::snap(r.right() - g.size().x, holo::ppp(p)), holo::snap(mid - g.size().y / 2.0, holo::ppp(p))), g.size());
    p.galley(v.min, g, t.ink());
    let mut x = l.right() + 8.0;
    let mut mesh = Mesh::default();
    while x < v.left() - 8.0 {
        mesh.add_colored_rect(Rect::from_min_size(pos2(holo::snap(x, holo::ppp(p)), holo::snap(mid + 3.0, holo::ppp(p))), vec2(px(p), px(p))), a(t.ink(), 0.30 * alpha));
        x += 4.0;
    }
    p.add(Shape::mesh(mesh));
}

/// A label on a leader: a dot on the thing, a line bent once, the words at its end.
pub fn callout(p: &Painter, from: Pos2, to: Pos2, text: &str, alpha: f32) {
    let t = holo::theme();
    holo::leader(p, from, to, a(t.ink(), 0.55 * alpha));
    let right = to.x >= from.x;
    holo::text(p, pos2(to.x + if right { 6.0 } else { -6.0 }, to.y), if right { Align2::LEFT_CENTER } else { Align2::RIGHT_CENTER }, Face::Label, text, 8.0, a(t.ink(), 0.9 * alpha));
}

/// Nothing to show: a crossed slot and one plain sentence.
pub fn empty(p: &Painter, r: Rect, sentence: &str, alpha: f32) {
    let t = holo::theme();
    let slot = Rect::from_center_size(pos2(r.center().x, r.center().y - 16.0), vec2(26.0, 18.0));
    holo::slot(p, slot, a(t.ink(), 0.40 * alpha));
    let g = {
        let mut job = eframe::egui::text::LayoutJob::simple(sentence.to_string(), holo::font(Face::Body, 13.5), a(t.muted(), alpha), (r.width() - 24.0).max(40.0));
        job.halign = eframe::egui::Align::Center;
        p.layout_job(job)
    };
    p.galley(pos2(r.center().x, slot.bottom() + 10.0), g, t.muted());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_key_leans_and_stays_inside_its_rect() {
        let r = Rect::from_min_size(pos2(10.0, 10.0), vec2(80.0, KEY_H));
        let shape = leaning(r);
        assert!(shape.iter().all(|q| r.expand(0.01).contains(*q)));
        assert_eq!(shape[0].x - r.left(), LEAN);
        assert_eq!(r.right() - shape[2].x, LEAN);
        // A narrow one leans less, or it would be a sliver.
        let narrow = leaning(Rect::from_min_size(pos2(0.0, 0.0), vec2(9.0, KEY_H)));
        assert!(narrow[0].x <= 3.0);
    }
}
