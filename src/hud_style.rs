//! The bridge from the panels to the hologram kit (`holo`, `holo_frame`, `holo_kit`): every
//! panel that still asks here for a shell, a title, a key or a state mark gets the new look.
//! A panel that has been laid out anew draws with the kit directly; this file shrinks as they
//! move over and goes when the last one has.
//!
//! What lives here for good reasons of its own: the motion table the panels share, the throw
//! of a dragged block, the state light a block reports, and what a block shows while it
//! loads, has nothing to show, or failed.

use crate::holo::{self, Face};
use eframe::egui::{self, pos2, vec2, Color32, FontId, Painter, Pos2, Rect, Rounding, Sense, Stroke, Ui, Vec2};
use std::collections::HashMap;
use std::sync::Mutex;

/// Every line of the HUD: a hairline stays one, anything wider is drawn finer (2 → 1.4,
/// 6 → 3).
pub fn stroke(width: f32, colour: Color32) -> Stroke {
    Stroke::new(thin(width), colour)
}

pub fn thin(width: f32) -> f32 {
    if width > 1.0 {
        1.0 + (width - 1.0) * 0.4
    } else {
        width
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

// ------------------------------------------------------------------------------- motion ----

/// How long things take: arriving fast and settling softly (`holo_motion`).
pub mod motion {
    use std::time::Duration;

    /// A hover, a press, a light changing.
    pub const FAST: Duration = Duration::from_millis(120);
    /// A block gliding to its place, a panel sliding.
    pub const NORMAL: Duration = Duration::from_millis(220);
    /// A whole window coming up; the core crossing the screen.
    pub const SLOW: Duration = Duration::from_millis(380);

    /// A smooth stop.
    pub fn ease_out(t: f32) -> f32 {
        crate::holo_motion::ease_out_cubic(t)
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

    /// A reading (a gauge, a meter, a percentage) that glides to `value` instead of jumping;
    /// at once with the animations off.
    pub fn glide(ctx: &eframe::egui::Context, id: eframe::egui::Id, value: f32) -> f32 {
        if !crate::gui::motion_enabled() {
            return value;
        }
        ctx.animate_value_with_time(id, value, NORMAL.as_secs_f32())
    }
}

// -------------------------------------------------------------------------------- shell ----

/// The dark a plate of glass throws on what is under it (so a panel over the board reads).
pub fn shadow(p: &Painter, r: Rect, alpha: f32) {
    if alpha <= 0.0 || !r.is_positive() {
        return;
    }
    let s = egui::epaint::Shadow { offset: vec2(0.0, 4.0), blur: 22.0, spread: 0.0, color: Color32::from_black_alpha((150.0 * alpha.min(1.0)) as u8) };
    p.add(s.as_shape(r, Rounding::ZERO));
}

/// White at `share` of its strength as the eye sees it.
pub fn white(share: f32) -> Color32 {
    Color32::WHITE.gamma_multiply(share.clamp(0.0, 1.0))
}

/// The lettering: the ink for what is read and a grey for what explains it (the accent is kept
/// for what is live or selected).
pub const TEXT: Color32 = Color32::from_rgb(0xEE, 0xF3, 0xF7);
pub const MUTED: Color32 = Color32::from_rgb(0x8C, 0x97, 0xA2);

pub fn snap(v: f32, ppp: f32) -> f32 {
    holo::snap(v, ppp)
}

/// One physical pixel, in points.
pub fn px(p: &Painter) -> f32 {
    holo::px(p)
}

fn snapped(r: Rect, ppp: f32) -> Rect {
    Rect::from_min_max(pos2(snap(r.left(), ppp), snap(r.top(), ppp)), pos2(snap(r.right(), ppp), snap(r.bottom(), ppp)))
}

/// A border `pixels` whole physical pixels wide, inside `r` and on the pixel grid: nothing of
/// it lies outside the rect, so a frame flush with the screen's edge keeps all four sides.
pub fn border(p: &Painter, r: Rect, pixels: f32, colour: Color32) {
    if colour.a() == 0 || !r.is_positive() {
        return;
    }
    let ppp = p.ctx().pixels_per_point();
    let t = pixels.round().max(1.0) / ppp;
    let r = snapped(r, ppp);
    if r.width() <= 2.0 * t || r.height() <= 2.0 * t {
        p.rect_filled(r, Rounding::ZERO, colour);
        return;
    }
    for side in [
        Rect::from_min_max(r.min, pos2(r.max.x, r.min.y + t)),
        Rect::from_min_max(pos2(r.min.x, r.max.y - t), r.max),
        Rect::from_min_max(pos2(r.min.x, r.min.y + t), pos2(r.min.x + t, r.max.y - t)),
        Rect::from_min_max(pos2(r.max.x - t, r.min.y + t), r.max - vec2(0.0, t)),
    ] {
        p.rect_filled(side, Rounding::ZERO, colour);
    }
}

/// Light round a small bright bar (a tab, a key's line): a glow that falls off through the
/// deep tone.
pub fn glow(p: &Painter, r: Rect, colour: Color32, strength: f32) {
    if strength <= 0.0 || !r.is_positive() {
        return;
    }
    let mut light = holo::Light::new(p);
    let line = if r.width() >= r.height() { [r.left_center(), r.right_center()] } else { [r.center_top(), r.center_bottom()] };
    light.halo(&line, false, 7.0, holo::lit(colour, 0.34 * strength.min(1.0)));
    light.paint(p);
}

/// How deep a panel's cut corner is (the bottom right one, at 45°).
pub const CHAMFER: f32 = crate::holo_frame::CUT;

/// The cut of a panel at `r`, or none for a panel too small to carry it.
pub fn chamfer(r: Rect) -> f32 {
    if r.width() >= 5.0 * CHAMFER && r.height() >= 3.0 * CHAMFER {
        CHAMFER
    } else {
        0.0
    }
}

fn outline_of(r: Rect) -> Vec<Pos2> {
    holo::plate(r, [0.0, 0.0, chamfer(r), 0.0])
}

/// A panel's corner marks: a short tick at three corners and the cut's own line at the
/// fourth. `lit` lays light round them (the panel in use).
pub fn corners(p: &Painter, r: Rect, arm: f32, colour: Color32, lit: f32) {
    if !r.is_positive() || colour.a() == 0 {
        return;
    }
    let arm = arm.min(r.width() / 3.0).min(r.height() / 3.0);
    let cut = chamfer(r);
    let mut ticks: Vec<[Pos2; 3]> = [(r.left_top(), 1.0, 1.0), (r.right_top(), -1.0, 1.0), (r.left_bottom(), 1.0, -1.0)].iter().map(|(at, dx, dy)| [pos2(at.x + dx * arm, at.y), *at, pos2(at.x, at.y + dy * arm)]).collect();
    if cut <= 0.0 {
        let at = r.right_bottom();
        ticks.push([pos2(at.x - arm, at.y), at, pos2(at.x, at.y - arm)]);
    }
    if lit > 0.0 {
        let mut light = holo::Light::new(p);
        for tick in &ticks {
            light.halo(tick, false, 6.0, holo::lit(colour, 0.30 * lit));
        }
        light.paint(p);
    }
    for tick in &ticks {
        holo::path(p, tick, colour);
    }
    if cut > 0.0 {
        holo::path(p, &[pos2(r.right(), r.bottom() - cut), pos2(r.right() - cut, r.bottom())], colour);
    }
}

/// A panel's glass and frame: the plate (its bottom right corner cut) with a hairline in
/// `edge`. `fill` only says how see-through it is; the glass is the theme's.
pub fn panel(p: &Painter, r: Rect, fill: Color32, edge: Color32, alpha: f32) {
    if !r.is_positive() || alpha <= 0.0 {
        return;
    }
    let shape = outline_of(r);
    // (A fill that is mostly clear asks for a see-through panel.)
    crate::holo_frame::glass(p, &shape, alpha * (fill.a() as f32 / 205.0).clamp(0.0, 1.0));
    holo::outline(p, &shape, edge);
}

/// A block's shell: glass, a white hairline frame, corner marks and the tab. The block in use
/// is framed in the accent, lit.
pub fn shell(p: &Painter, r: Rect, active: bool, alpha: f32) {
    if !r.is_positive() || alpha <= 0.0 {
        return;
    }
    let t = holo::theme();
    let shape = outline_of(r);
    crate::holo_frame::glass(p, &shape, alpha);
    if active {
        let mut light = holo::Light::new(p);
        let mut closed = shape.clone();
        closed.push(shape[0]);
        light.halo(&closed, false, 9.0, holo::lit(t.accent(), 0.28 * alpha));
        light.paint(p);
    }
    holo::outline(p, &shape, if active { holo::a(t.accent(), alpha) } else { holo::a(t.ink(), 0.20 * alpha) });
    corners(p, r, BRACKET_ARM, holo::a(if active { t.hot() } else { t.ink() }, 0.85 * alpha), 0.0);
    marks(p, r, active, alpha);
}

/// How long a corner tick's arms are.
pub const BRACKET_ARM: f32 = crate::holo_frame::TICK;
/// The panel's tab: a short bar of the accent on its top edge, at the left corner.
pub const TAB_W: f32 = 26.0;

/// Where the tab of a block at `r` lies, if it has room.
pub fn tab_rect(r: Rect, pixel: f32) -> Option<Rect> {
    let ppp = 1.0 / pixel;
    (r.width() >= 5.0 * TAB_W).then(|| Rect::from_min_size(pos2(snap(r.left(), ppp), snap(r.top(), ppp)), vec2(TAB_W, 3.0)))
}

/// The mark on a panel's top edge: a solid tab of the accent at its left corner and a short
/// ruler after it. Narrow panels go without.
pub fn marks(p: &Painter, r: Rect, active: bool, alpha: f32) {
    let Some(tab) = tab_rect(r, px(p)) else { return };
    let t = holo::theme();
    let colour = holo::a(if active { t.hot() } else { t.accent() }, alpha);
    if active {
        glow(p, tab, t.accent(), 0.8 * alpha);
    }
    p.rect_filled(tab, Rounding::ZERO, colour);
    holo::ruler(p, pos2(tab.right() + 6.0, tab.top()), pos2(tab.right() + 26.0, tab.top()), 4.0, 5, 5.0, vec2(0.0, 1.0), holo::a(t.ink(), 0.36 * alpha));
}

/// `a` to `b` by `t`, channel by channel (both as they are stored: premultiplied).
pub fn lerp_colour(a: Color32, b: Color32, t: f32) -> Color32 {
    holo::mix(a, b, t)
}

/// The ink for text on a coloured `fill`: dark or light, whichever reads better.
pub fn ink_on(fill: Color32) -> Color32 {
    if contrast(fill, Color32::BLACK) >= contrast(fill, Color32::WHITE) { Color32::BLACK } else { Color32::WHITE }
}

/// The WCAG contrast ratio of two opaque colours (1 to 21).
pub fn contrast(a: Color32, b: Color32) -> f32 {
    let lum = |c: Color32| {
        let ch = |v: u8| {
            let v = v as f32 / 255.0;
            if v <= 0.04045 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
        };
        0.2126 * ch(c.r()) + 0.7152 * ch(c.g()) + 0.0722 * ch(c.b())
    };
    let (x, y) = (lum(a), lum(b));
    (x.max(y) + 0.05) / (x.min(y) + 0.05)
}

/// The surface's `fill` at `from`, fading to nothing at `to`, from `left` to `right`: text under
/// it fades into the block.
pub fn fade_band(p: &Painter, fill: Color32, left: f32, right: f32, from: f32, to: f32) {
    let mut mesh = egui::Mesh::default();
    let (a, b) = (fill, Color32::TRANSPARENT);
    mesh.colored_vertex(pos2(left, from), a);
    mesh.colored_vertex(pos2(right, from), a);
    mesh.colored_vertex(pos2(left, to), b);
    mesh.colored_vertex(pos2(right, to), b);
    mesh.add_triangle(0, 1, 2);
    mesh.add_triangle(1, 3, 2);
    p.add(egui::Shape::mesh(mesh));
}

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
    let fade = |from: f32, to: f32| fade_band(&p, fill, r.left(), r.right(), from, to);
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
    p.rect_filled(Rect::from_min_size(pos2(snap(r.right() - 2.0, ppp), r.top()), vec2(1.0 / ppp, r.height())), Rounding::ZERO, holo::a(t.ink(), 0.10));
    p.rect_filled(Rect::from_min_size(pos2(snap(r.right() - 3.0, ppp), y), vec2(3.0 / ppp, h)), Rounding::ZERO, holo::a(t.accent(), 0.85));
    out
}

/// A block's surface: one plain quad. `alpha`: the block's own opacity.
pub fn surface(p: &Painter, r: Rect, fill: Color32, alpha: f32) {
    for shape in surface_shapes(r, fill, alpha) {
        p.add(shape);
    }
}

/// The same as shapes, for a frame that is painted behind content laid out before its size
/// is known.
pub fn surface_shapes(r: Rect, fill: Color32, alpha: f32) -> Vec<egui::Shape> {
    if !r.is_positive() || alpha <= 0.0 || fill.a() == 0 {
        return Vec::new();
    }
    // (The surface is a mesh of its own, four corners and nothing else: the verifier reads a
    // plain quad as the fill it is, and measures a block's unfolding by it.)
    let mut mesh = egui::Mesh::default();
    mesh.add_colored_rect(r, fill.gamma_multiply(alpha));
    vec![egui::Shape::mesh(mesh)]
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

/// How long the core's ripple runs (seconds).
pub const RIPPLE_SECS: f32 = 0.9;

/// The core's ripple when he hears his name: three rings of light that leave it and fade, `age`
/// seconds after it (his silent "yes?"). Returns whether it is still running.
pub fn ripple(p: &Painter, centre: Pos2, radius: f32, age: f32, colour: Color32) -> bool {
    if !(0.0..RIPPLE_SECS).contains(&age) {
        return false;
    }
    let mut light = holo::Light::new(p);
    for delay in [0.0, 0.14, 0.28] {
        let t = (age - delay) / (RIPPLE_SECS - delay);
        if !(0.0..1.0).contains(&t) {
            continue;
        }
        let out = crate::holo_motion::ease_out_expo(t);
        let fade = (1.0 - t) * (1.0 - t);
        light.neon_arc(centre, radius * (1.04 + 0.85 * out), 0.0, std::f32::consts::TAU, px(p).max(1.0), holo::a(colour, 0.9 * fade), fade);
    }
    light.paint(p);
    true
}

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

/// A label: small wide capitals, tracked.
pub fn label_galley(p: &Painter, text: &str, size: f32, colour: Color32) -> std::sync::Arc<egui::Galley> {
    holo::galley(p, Face::Label, text, size * 0.74, colour)
}

/// A readout: every digit as wide as the widest, so a changing number (a clock's seconds) never
/// jitters.
pub fn figure_galley(fonts: &egui::text::Fonts, text: &str, font: FontId, colour: Color32) -> std::sync::Arc<egui::Galley> {
    let widest = ('0'..='9').map(|d| fonts.glyph_width(&font, d)).fold(0.0f32, f32::max);
    let mut job = egui::text::LayoutJob::default();
    let format = egui::TextFormat { font_id: font.clone(), color: colour, ..Default::default() };
    // (Each digit stands in the middle of a cell the widest digit's width: the room it lacks
    // goes half before it and half after it.)
    let mut owed = 0.0;
    let mut buf = [0u8; 4];
    for c in text.chars() {
        let pad = if c.is_ascii_digit() { (widest - fonts.glyph_width(&font, c)) * 0.5 } else { 0.0 };
        job.append(c.encode_utf8(&mut buf), owed + pad, format.clone());
        owed = pad;
    }
    fonts.layout_job(job)
}

/// A title decoding itself, `progress` 0 … 1 (`holo_motion::decode`).
pub fn decode(text: &str, progress: f32, clock: f64) -> std::borrow::Cow<'_, str> {
    if progress >= 1.0 {
        return text.into();
    }
    crate::holo_motion::decode(text, progress, clock).into()
}

/// How long a changed digit takes to roll in.
const ROLL_SECS: f64 = 0.26;

/// Paints `text` as even-width figures with its top-left at `at`; a figure that changed since
/// the last frame (per `id`) rolls up into place, the old one leaving above it. Returns the size.
pub fn rolling_figures(p: &Painter, id: egui::Id, at: Pos2, text: &str, font: FontId, colour: Color32) -> Vec2 {
    let ctx = p.ctx();
    let now = ctx.input(|i| i.time);
    let (old, since) = ctx.data_mut(|d| {
        let slot = d.get_temp_mut_or_insert_with(id, || (text.to_string(), String::new(), f64::MIN));
        if slot.0 != text {
            let old = std::mem::replace(&mut slot.0, text.to_string());
            (slot.1, slot.2) = (old, now);
        }
        (slot.1.clone(), slot.2)
    });
    let new = p.fonts(|f| figure_galley(f, text, font.clone(), colour));
    let size = new.size();
    let t = ((now - since) / ROLL_SECS) as f32;
    if t >= 1.0 || !crate::gui::motion_enabled() || old.chars().count() != text.chars().count() {
        p.galley(at, new, colour);
        return size;
    }
    let old = p.fonts(|f| figure_galley(f, &old, font, colour));
    let e = crate::holo_motion::ease_out_expo(t);
    let glyphs = &new.rows[0].glyphs;
    let old_glyphs = &old.rows[0].glyphs;
    for (i, g) in glyphs.iter().enumerate() {
        // Each figure's column runs halfway to its neighbours.
        let left = if i == 0 { 0.0 } else { (glyphs[i - 1].pos.x + glyphs[i - 1].advance_width + g.pos.x) * 0.5 };
        let right = glyphs.get(i + 1).map_or(size.x, |next| (g.pos.x + g.advance_width + next.pos.x) * 0.5);
        let column = Rect::from_min_max(pos2(at.x + left, at.y), pos2(at.x + right, at.y + size.y));
        let clipped = p.with_clip_rect(column.intersect(p.clip_rect()));
        if old_glyphs.get(i).is_some_and(|o| o.chr == g.chr) {
            clipped.galley(at, new.clone(), colour);
        } else {
            clipped.galley_with_override_text_color(at - vec2(0.0, size.y * e), old.clone(), colour.gamma_multiply(1.0 - e));
            clipped.galley_with_override_text_color(at + vec2(0.0, size.y * (1.0 - e)), new.clone(), colour.gamma_multiply(e));
        }
    }
    ctx.request_repaint();
    size
}

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
pub fn emblem_light(p: &Painter, r: Rect, orb: Rect, colour: Color32, alpha: f32) {
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

/// How dark the display gets towards its corners (alpha of 255).
const EDGE_FALLOFF: u8 = 52;

/// The display's finish: the light falls off towards the edges and darkest in the corners, as
/// on any real screen. Still (it asks for no frame) and faint enough that text stays sharp.
pub fn glass(p: &Painter, rect: Rect) {
    if rect.width() < 200.0 || rect.height() < 120.0 {
        return;
    }
    let mut mesh = egui::Mesh::default();
    let inner = rect.shrink2(vec2(rect.width() * 0.16, rect.height() * 0.2));
    let ring = |r: Rect| [r.left_top(), r.center_top(), r.right_top(), r.right_center(), r.right_bottom(), r.center_bottom(), r.left_bottom(), r.left_center()];
    for (k, at) in ring(rect).into_iter().enumerate() {
        mesh.colored_vertex(at, Color32::from_black_alpha(if k % 2 == 0 { EDGE_FALLOFF } else { EDGE_FALLOFF / 2 }));
    }
    for at in ring(inner) {
        mesh.colored_vertex(at, Color32::TRANSPARENT);
    }
    for k in 0..8u32 {
        let n = (k + 1) % 8;
        mesh.add_triangle(k, n, 8 + n);
        mesh.add_triangle(k, 8 + n, 8 + k);
    }
    p.add(egui::Shape::mesh(mesh));
}

// ------------------------------------------------------------------------------- header ----

/// What a block's state mark says.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Light {
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
static LIGHTS: Mutex<Option<HashMap<String, Light>>> = Mutex::new(None);

/// Block `id` reports its state (its header mark shows it).
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

/// A caution short of a fault (never one of the theme's own colours).
pub fn warn_colour() -> Color32 {
    holo::theme().warn()
}

pub const FAULT: Color32 = Color32::from_rgb(255, 59, 48);
pub const GOOD: Color32 = Color32::from_rgb(120, 232, 176);

pub fn light_colour(light: Light) -> Color32 {
    match light {
        Light::Rest => Color32::from_rgb(0x4A, 0x52, 0x5A),
        Light::Busy => holo::theme().accent(),
        Light::Good => GOOD,
        Light::Warn => warn_colour(),
        Light::Fault => holo::theme().alert(),
    }
}

/// The state mark: a small steady tick for all is well, a square that blinks slowly while busy,
/// a diamond for trouble (the alert colour when it failed), nothing at rest. Returns whether
/// it moves.
pub fn paint_light(p: &Painter, centre: Pos2, light: Light, time: f64, alpha: f32) -> bool {
    let ppp = p.ctx().pixels_per_point();
    let c = pos2(snap(centre.x, ppp), snap(centre.y, ppp));
    match light {
        Light::Rest => {}
        Light::Good => {
            let s = stroke(1.2, holo::a(holo::theme().ink(), 0.6 * alpha));
            p.line_segment([c + vec2(-3.0, 0.0), c + vec2(-1.0, 2.5)], s);
            p.line_segment([c + vec2(-1.0, 2.5), c + vec2(3.5, -2.5)], s);
        }
        Light::Busy => {
            // (A slow blink, eased: on, off, on, about once a second and a half.)
            let on = 0.5 + 0.5 * (time * std::f64::consts::TAU / 1.4).sin() as f32;
            let square = Rect::from_center_size(c, Vec2::splat(5.0));
            glow(p, square, light_colour(light), on * alpha);
            p.rect_filled(square, Rounding::ZERO, light_colour(light).gamma_multiply((0.2 + 0.8 * on) * alpha));
        }
        Light::Warn | Light::Fault => {
            let diamond = vec![c + vec2(0.0, -4.0), c + vec2(4.0, 0.0), c + vec2(0.0, 4.0), c + vec2(-4.0, 0.0)];
            p.add(egui::Shape::convex_polygon(diamond, light_colour(light).gamma_multiply(alpha), Stroke::NONE));
        }
    }
    light == Light::Busy
}

/// The label face: small wide capitals. (`size` is the panels' old measure.)
pub fn label_font(size: f32) -> FontId {
    holo::font(Face::Label, size * 0.74)
}

/// Large figures and headlines.
pub fn title_font(size: f32) -> FontId {
    holo::font(Face::Head, size)
}

/// The height of every block's header row.
pub const HEADER_H: f32 = 24.0;

/// A key as it is lettered: the panels still write "[ CLOSE ]", the kit letters it CLOSE.
fn bare(label: &str) -> &str {
    label.trim().trim_start_matches('[').trim_end_matches(']').trim()
}

/// A header key drawn with its right edge at `right`, centred on `mid`; `lit` keeps it in the
/// accent (a pinned block's PIN). Returns its rect; the caller asks whether it was clicked.
pub fn key(p: &Painter, right: f32, mid: f32, label: &str, hot: bool, down: bool, lit: bool, alpha: f32) -> Rect {
    let r = Rect::from_min_size(pos2(right - key_width(p, label), mid - KEY_H / 2.0), vec2(key_width(p, label), KEY_H));
    crate::holo_kit::key(p, r, bare(label), if hot { 1.0 } else { 0.0 }, down, lit, alpha);
    r
}

/// A header key's width.
pub fn key_width(p: &Painter, label: &str) -> f32 {
    crate::holo_kit::key_width(p, bare(label))
}

pub const KEY_H: f32 = 18.0;

/// How many of a header's keys leave a title `title` wide its room in a row `row` wide: all of
/// them when it fits, else the last ones go first (a narrow block keeps its title whole), and
/// the first always stays.
pub fn keys_that_fit(p: &Painter, labels: &[(&str, bool, &str)], row: f32, title: f32) -> usize {
    let mut used = 0.0;
    let mut fit = 0;
    for (k, (label, _, _)) in labels.iter().enumerate() {
        used += key_width(p, label) + 2.0;
        if k > 0 && row - used < title {
            break;
        }
        fit = k + 1;
    }
    fit
}

/// The header keys of a block, right to left from `right`: each (label, lit, hint). Returns
/// which was clicked this frame and where the leftmost key begins. They light up under the
/// pointer and flash under the press.
pub fn keys(ui: &mut Ui, id: egui::Id, right: f32, mid: f32, labels: &[(&str, bool, &str)], alpha: f32) -> (Option<usize>, f32) {
    let mut clicked = None;
    let mut x = right;
    for (k, (label, lit, hint)) in labels.iter().enumerate() {
        let width = key_width(ui.painter(), label);
        let r = Rect::from_min_size(pos2(x - width, mid - KEY_H / 2.0), vec2(width, KEY_H));
        let response = ui.interact(r, id.with(("key", k)), Sense::click());
        let hot = ui.ctx().animate_bool_with_time(id.with(("hot", k)), response.hovered(), motion::FAST.as_secs_f32());
        crate::holo_kit::key(ui.painter(), r, bare(label), hot, response.is_pointer_button_down_on(), *lit, alpha);
        if response.clicked() {
            clicked = Some(k);
        }
        let tip = tip_with_sentence(label, hint);
        if !tip.is_empty() {
            response.on_hover_text(tip);
        }
        x -= width + 2.0;
    }
    (clicked, x + 2.0)
}

/// What Sir can say instead of pressing a key: its tip names it (`tip_with_sentence`).
pub const SAY_INSTEAD: &[(&str, &str)] = &[
    ("[ DETACH ]", "detach it"),
    ("[ POP OUT ]", "detach it"),
    ("[ UNDO ]", "undo that"),
    ("[ YES ]", "yes"),
    ("[ NO ]", "no"),
    ("[ NONE ]", "none of them"),
];

/// A key's tip with the sentence that does the same, unless the tip names it already.
pub fn tip_with_sentence(label: &str, hint: &str) -> String {
    match SAY_INSTEAD.iter().find(|(key, said)| *key == label && !hint.contains(said)) {
        Some((_, said)) if hint.is_empty() => format!("Say “{said}”."),
        Some((_, said)) => format!("{}. Or say “{said}”.", hint.trim_end_matches('.')),
        None => hint.to_string(),
    }
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
    let x = snap(r.left() + r.width() * (time / 1.4).fract() as f32, p.ctx().pixels_per_point());
    let mut wake = egui::Mesh::default();
    for (wx, c) in [(x - 60.0, Color32::TRANSPARENT), (x, holo::lit(t.accent(), 0.10))] {
        wake.colored_vertex(pos2(wx, r.top()), c);
        wake.colored_vertex(pos2(wx, r.bottom()), c);
    }
    wake.add_triangle(0, 1, 2);
    wake.add_triangle(1, 3, 2);
    p.add(egui::Shape::mesh(wake));
    let mut light = holo::Light::new(&p);
    light.neon(&[pos2(x, r.top()), pos2(x, r.bottom())], false, px(&p), holo::a(t.hot(), 0.8), 0.7);
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
    let width = key_width(ui.painter(), "[ RETRY ]");
    keys(ui, egui::Id::new(("retry", r.min.x as i32, r.min.y as i32)), c.x + width * 0.5, bottom, &[("[ RETRY ]", true, "Try again")], 1.0).0.is_some()
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
        p.add(egui::Shape::closed_line(diamond, stroke(1.2, t.alert())));
        p.line_segment([m + vec2(0.0, -5.0), m + vec2(0.0, 1.5)], stroke(1.2, t.alert()));
        p.circle_filled(m + vec2(0.0, 4.5), 0.9, t.alert());
    }
    let galley = p.layout(what.to_string(), holo::font(Face::Body, 13.5), t.ink(), (r.width() - 24.0).max(60.0));
    let y = if tall { c.y - 12.0 } else { c.y - galley.size().y * 0.5 };
    p.galley(pos2(c.x - galley.size().x * 0.5, y), galley, Color32::PLACEHOLDER);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_on_any_fill_reaches_four_and_a_half_to_one() {
        let mut fills: Vec<Color32> = crate::holo_theme::PRESETS.iter().flat_map(|(_, _, t)| [t.accent(), t.deep(), t.alert()]).collect();
        fills.extend((0..=255).step_by(5).map(|v| Color32::from_gray(v as u8)));
        for fill in fills {
            assert!(contrast(fill, ink_on(fill)) >= 4.5, "{fill:?}: {:.2}", contrast(fill, ink_on(fill)));
        }
        assert!(contrast(TEXT, Color32::BLACK) >= 12.0 && contrast(MUTED, Color32::BLACK) >= 4.5);
    }

    #[test]
    fn motion_is_quick_and_settles_just_past_its_mark() {
        assert!(motion::SLOW <= std::time::Duration::from_millis(420), "nothing of the HUD's own takes longer than its panels' entrance");
        assert!(motion::FAST < motion::NORMAL && motion::NORMAL < motion::SLOW);
        let peak = (0..=100).map(|k| motion::settle(k as f32 / 100.0)).fold(0.0f32, f32::max);
        assert!(peak > 1.0 && peak < 1.02, "{peak}");
        assert_eq!(motion::settle(1.0), 1.0);
        let mut v = 0.0;
        for _ in 0..(motion::NORMAL.as_secs_f32() * 60.0).ceil() as usize {
            v = motion::approach(v, 1.0, 1.0 / 60.0, motion::NORMAL);
        }
        assert!(v > 0.98, "{v}");
    }

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
    fn a_key_is_lettered_without_its_brackets_and_keeps_its_sentence() {
        assert_eq!(bare("[ CLOSE ]"), "CLOSE");
        assert_eq!(bare("PIN"), "PIN");
        assert_eq!(bare("[ < PREV ]"), "< PREV");
        assert_eq!(tip_with_sentence("[ DETACH ]", ""), "Say “detach it”.");
        assert_eq!(tip_with_sentence("[ DETACH ]", "A window of its own"), "A window of its own. Or say “detach it”.");
        assert_eq!(tip_with_sentence("[ CLOSE ]", "Close it"), "Close it");
    }

    #[test]
    fn a_panel_cuts_one_corner_and_a_small_one_none() {
        let r = Rect::from_min_size(pos2(0.0, 0.0), vec2(400.0, 200.0));
        assert_eq!(chamfer(r), CHAMFER);
        assert_eq!(outline_of(r).len(), 5, "four corners, one of them cut");
        let small = Rect::from_min_size(pos2(0.0, 0.0), vec2(50.0, 30.0));
        assert_eq!((chamfer(small), outline_of(small).len()), (0.0, 4));
        assert!(tab_rect(small, 1.0).is_none() && tab_rect(r, 1.0).is_some_and(|tab| tab.left() == r.left() && tab.top() == r.top()));
    }

    #[test]
    fn a_block_reports_its_state_and_rest_clears_it() {
        set_light("test-block", Light::Busy);
        assert_eq!(light("test-block"), Light::Busy);
        set_light("test-block", Light::Rest);
        assert_eq!(light("test-block"), Light::Rest);
        assert_ne!(light_colour(Light::Warn), light_colour(Light::Fault));
    }

    #[test]
    fn a_title_decodes_left_to_right() {
        assert_eq!(decode("SYSTEM", 1.0, 0.0), "SYSTEM");
        let half = decode("SYSTEM TELEMETRY", 0.5, 0.2);
        assert!(half.starts_with("SYSTEM T"), "{half}");
    }
}
