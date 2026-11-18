//! A panel: a plate of black glass in a hairline frame, one corner cut. Its header carries an
//! index tab (every panel is numbered, as on a drawing) and the title; the frame is white ink
//! while it rests and lights in the accent when it is the one he works in.
//!
//! A panel does not appear, it is drawn: a line shoots out, unfolds into the frame, the tab
//! slides in and the title decodes ([`Plate::open`] runs 0-1 through all of it).

use crate::holo::{self, a, lit, mix, px, Face, Light};
use crate::holo_motion::{decode, ease_out_expo, phase};
use eframe::egui::{pos2, vec2, Align2, Color32, Mesh, Painter, Pos2, Rect, Shape};

/// The header's height.
pub const HEADER: f32 = 30.0;
/// How much of the bottom right corner is cut.
pub const CUT: f32 = 14.0;
/// The room between the frame and what it holds.
pub const PAD: f32 = 14.0;
/// A corner tick's arm.
pub const TICK: f32 = 9.0;

/// What a plate is drawn from.
#[derive(Clone, Copy, Debug)]
pub struct Plate<'a> {
    /// Its number on the drawing ("01"); empty for none.
    pub index: &'a str,
    pub title: &'a str,
    /// 0 resting … 1 the panel he works in.
    pub focus: f32,
    /// 0 not there … 1 standing: the entrance (and, run backwards, the exit).
    pub open: f32,
    pub alpha: f32,
    /// Seconds, for the decoding title.
    pub clock: f64,
    /// Room kept free at the right of the header (for keys): the title is cut short of it.
    pub reserve: f32,
}

impl Default for Plate<'_> {
    fn default() -> Self {
        Self { index: "", title: "", focus: 0.0, open: 1.0, alpha: 1.0, clock: 0.0, reserve: 0.0 }
    }
}

/// Where a plate's parts landed.
#[derive(Clone, Copy, Debug)]
pub struct Laid {
    /// The frame as drawn (smaller than asked while it unfolds).
    pub frame: Rect,
    pub header: Rect,
    /// The room for content, inside the padding.
    pub body: Rect,
    /// Where the title starts and ends: keys go right of it.
    pub title_start: f32,
    pub title_end: f32,
    /// How visible the content should be drawn (0 until the frame stands).
    pub content: f32,
}

/// The frame a plate has reached at `open`: first a line along its top edge growing from the
/// left, then the height.
pub fn unfolded(r: Rect, open: f32) -> Rect {
    let w = r.width() * ease_out_expo(phase(open, 0.0, 0.42));
    let h = r.height() * ease_out_expo(phase(open, 0.26, 0.86));
    Rect::from_min_size(r.min, vec2(w, h))
}

/// Glass: a convex shape filled a breath lighter at the top than at the bottom, as if lit
/// from above.
pub fn glass(p: &Painter, points: &[Pos2], alpha: f32) {
    let t = holo::theme();
    let (top, bottom) = points.iter().fold((f32::MAX, f32::MIN), |(lo, hi), q| (lo.min(q.y), hi.max(q.y)));
    let base = t.glass();
    let lift = mix(base, a(t.ink(), 0.92), 0.050);
    let mut mesh = Mesh::default();
    let centre = points.iter().fold(vec2(0.0, 0.0), |s, q| s + q.to_vec2()) / points.len() as f32;
    let shade = |y: f32| a(mix(lift, base, ((y - top) / (bottom - top).max(1.0)).clamp(0.0, 1.0)), alpha);
    mesh.colored_vertex(centre.to_pos2(), shade(centre.y));
    for q in points {
        mesh.colored_vertex(*q, shade(q.y));
    }
    let n = points.len() as u32;
    for i in 0..n {
        mesh.add_triangle(0, 1 + i, 1 + (i + 1) % n);
    }
    p.add(Shape::mesh(mesh));
}

/// Draws the plate and says where its parts are.
pub fn plate(p: &Painter, r: Rect, plate: &Plate) -> Laid {
    let t = holo::theme();
    let frame = unfolded(r, plate.open);
    let alpha = plate.alpha;
    let chrome = phase(plate.open, 0.55, 0.92) * alpha;
    let content = phase(plate.open, 0.66, 1.0) * alpha;
    let header = Rect::from_min_size(frame.min, vec2(frame.width(), HEADER.min(frame.height())));
    let body = Rect::from_min_max(pos2(r.left() + PAD, r.top() + HEADER + PAD * 0.7), pos2(r.right() - PAD, r.bottom() - PAD));
    let laid = Laid { frame, header, body, title_start: frame.left(), title_end: frame.left(), content };
    if alpha <= 0.003 || frame.width() < 1.0 {
        return laid;
    }
    let edge = mix(a(t.ink(), 0.20), t.accent(), plate.focus);
    if frame.height() < 2.0 {
        // Still a line: bright, with a spark at its travelling end.
        let mut light = Light::new(p);
        light.neon(&[frame.left_top(), frame.right_top()], false, px(p), a(t.hot(), alpha), 1.0);
        light.spark(frame.right_top(), 5.0, lit(t.hot(), alpha));
        light.paint(p);
        return laid;
    }
    let cut = CUT.min(frame.height() / 2.0);
    let shape = holo::plate(frame, [0.0, 0.0, cut, 0.0]);
    glass(p, &shape, alpha);
    if plate.focus > 0.01 {
        // The panel he works in: its frame is a line of light.
        let mut light = Light::new(p);
        let mut closed = shape.clone();
        closed.push(shape[0]);
        light.halo(&closed, false, 9.0, lit(t.accent(), 0.30 * plate.focus * alpha));
        light.paint(p);
    }
    holo::outline(p, &shape, a(edge, alpha));
    // The corners: three ticks and the cut.
    let bright = a(mix(t.ink(), t.hot(), plate.focus), 0.85 * alpha);
    let arm = TICK.min(frame.width() / 3.0).min(frame.height() / 3.0);
    for (at, dx, dy) in [(frame.left_top(), 1.0, 1.0), (frame.right_top(), -1.0, 1.0), (frame.left_bottom(), 1.0, -1.0)] {
        holo::path(p, &[pos2(at.x + dx * arm, at.y), at, pos2(at.x, at.y + dy * arm)], bright);
    }
    holo::path(p, &[pos2(frame.right(), frame.bottom() - cut), pos2(frame.right() - cut, frame.bottom())], bright);

    let (mut title_start, mut title_end) = (frame.left(), frame.left());
    if chrome > 0.003 && frame.height() >= HEADER {
        // The header's rule, with the accent under the tab.
        let y = frame.top() + HEADER;
        holo::rule(p, pos2(frame.left(), y), pos2(frame.right(), y), a(t.ink(), 0.15 * chrome));
        let slide = ease_out_expo(phase(plate.open, 0.50, 0.90));
        let mut x = frame.left() + PAD - (1.0 - slide) * 18.0;
        let mid = frame.top() + HEADER / 2.0;
        if !plate.index.is_empty() {
            // The index tab: a solid block of the accent, its number cut out in black.
            let g = holo::galley(p, Face::Label, plate.index, 8.5, Color32::BLACK);
            let tab = Rect::from_min_size(pos2(frame.left(), frame.top()), vec2(g.size().x + 16.0, 17.0));
            let clip = p.with_clip_rect(frame.intersect(p.clip_rect()));
            clip.rect_filled(tab.translate(vec2(-(1.0 - slide) * tab.width(), 0.0)), 0.0, a(mix(t.accent(), t.hot(), 0.25 * plate.focus), chrome));
            clip.galley(pos2(tab.left() + 8.0 - (1.0 - slide) * tab.width(), tab.center().y - g.size().y / 2.0), g, Color32::BLACK);
            holo::rule(p, pos2(frame.left(), y), pos2(tab.right() + 26.0, y), a(t.accent(), 0.9 * chrome));
            x = tab.right() + 12.0 - (1.0 - slide) * 18.0;
        }
        let shown = decode(&plate.title.to_uppercase(), phase(plate.open, 0.55, 1.0), plate.clock);
        let room = (frame.right() - PAD - plate.reserve - x).max(1.0);
        let g = holo::galley_within(p, Face::Label, &shown, 10.5, a(t.ink(), chrome), room);
        (title_start, title_end) = (x, x + g.size().x);
        p.with_clip_rect(frame.intersect(p.clip_rect())).galley(pos2(x, holo::snap(mid - g.size().y / 2.0 + 0.5, holo::ppp(p))), g, t.ink());
    }
    Laid { title_start, title_end, ..laid }
}

/// A plain plate without a header: a tooltip, a menu, a card inside a panel.
pub fn card(p: &Painter, r: Rect, focus: f32, alpha: f32) {
    let t = holo::theme();
    let shape = holo::plate(r, [0.0, 0.0, CUT.min(r.height() / 3.0), 0.0]);
    glass(p, &shape, alpha);
    holo::outline(p, &shape, a(mix(a(t.ink(), 0.20), t.accent(), focus), alpha));
}

/// A plate's frame over a rect that is already filled with glass: for a container whose height
/// is known only once its content is laid out (`holo_ui::plate`). The canvas shows through the
/// cut corner; the hairline, the three ticks and the cut are drawn as on `plate`.
pub fn dress(p: &Painter, r: Rect, focus: f32, alpha: f32) {
    let t = holo::theme();
    let cut = CUT.min(r.height() / 3.0);
    holo::fill(p, &[pos2(r.right() - cut, r.bottom() + 1.0), pos2(r.right() + 1.0, r.bottom() + 1.0), pos2(r.right() + 1.0, r.bottom() - cut)], t.void());
    holo::outline(p, &holo::plate(r, [0.0, 0.0, cut, 0.0]), a(mix(a(t.ink(), 0.20), t.accent(), focus), alpha));
    let bright = a(mix(t.ink(), t.hot(), focus), 0.85 * alpha);
    let arm = TICK.min(r.width() / 3.0).min(r.height() / 3.0);
    for (at, dx, dy) in [(r.left_top(), 1.0, 1.0), (r.right_top(), -1.0, 1.0), (r.left_bottom(), 1.0, -1.0)] {
        holo::path(p, &[pos2(at.x + dx * arm, at.y), at, pos2(at.x, at.y + dy * arm)], bright);
    }
    holo::path(p, &[pos2(r.right(), r.bottom() - cut), pos2(r.right() - cut, r.bottom())], bright);
}

/// The text under a panel's bottom edge, left: what it is and how old ("WX-02 · 14:32:07").
pub fn serial(p: &Painter, frame: Rect, text: &str, alpha: f32) {
    let t = holo::theme();
    holo::text(p, pos2(frame.left() + 1.0, frame.bottom() + 5.0), Align2::LEFT_TOP, Face::Mono, text, 9.5, a(t.faint(), alpha));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plate_is_a_line_before_it_is_a_frame() {
        let r = Rect::from_min_size(pos2(100.0, 100.0), vec2(400.0, 240.0));
        assert_eq!(unfolded(r, 0.0).size(), vec2(0.0, 0.0));
        let line = unfolded(r, 0.24);
        assert!(line.width() > 300.0 && line.height() == 0.0, "{line:?}");
        let half = unfolded(r, 0.5);
        assert!((half.width() - 400.0).abs() < 1.0 && half.height() > 100.0 && half.height() < 240.0, "{half:?}");
        assert_eq!(unfolded(r, 1.0), r);
        // It grows from its top left corner: that corner never moves.
        for k in 0..=10 {
            assert_eq!(unfolded(r, k as f32 / 10.0).min, r.min);
        }
    }
}
