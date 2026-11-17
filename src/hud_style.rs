//! The HUD's shared look (Sir, 2026-11-12: "look and feel better"): one motion table, one shell
//! for every block (the shadow it floats on, its frame line of light, the emblem's light on the
//! edge that faces it), the header every block wears (title, state light, its keys) and what a
//! block shows while it loads, has nothing to show, or failed. The board's report blocks and the
//! tool blocks (`gui::hud_block_window`) both draw with these, so they read as one system.

use eframe::egui::{self, pos2, vec2, Color32, FontFamily, FontId, Painter, Pos2, Rect, Rounding, Sense, Stroke, Ui, Vec2};
use std::collections::HashMap;
use std::sync::Mutex;

/// `Stroke::new` takes `impl Into<f32>`, which leaves float literals untyped.
/// Every line of the HUD: a hairline stays one, anything wider is drawn finer (2 → 1.4,
/// 6 → 3), as Stark's screens draw their light.
pub fn stroke(width: f32, colour: Color32) -> Stroke {
    Stroke::new(thin(width), colour)
}

/// A line's width the Stark way (`stroke`).
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

    /// A reading (a gauge, a meter, a percentage) that glides to `value` in a quarter of a
    /// second instead of jumping; at once with the animations off.
    pub fn glide(ctx: &eframe::egui::Context, id: eframe::egui::Id, value: f32) -> f32 {
        if !crate::gui::motion_enabled() {
            return value;
        }
        ctx.animate_value_with_time(id, value, SLOW.as_secs_f32())
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

/// The film's lettering colours, whatever the theme: a cool white for what is read and a
/// blue-grey for what explains it (the accent is kept for what is live or selected).
pub const TEXT: Color32 = Color32::from_rgb(0xE8, 0xF2, 0xFF);
pub const MUTED: Color32 = Color32::from_rgb(0x8F, 0xA4, 0xB8);

// ----------------------------------------------------------------------------- hairlines ----

/// `v` (points) moved onto the nearest edge between two physical pixels at `ppp` pixels per
/// point: a line one physical pixel wide from there covers whole pixels and never blurs.
pub fn snap(v: f32, ppp: f32) -> f32 {
    (v * ppp).round() / ppp
}

/// One physical pixel, in points.
pub fn px(p: &Painter) -> f32 {
    1.0 / p.ctx().pixels_per_point()
}

/// `r` with its edges on the pixel grid.
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

/// Light round the brightest strokes only: two wider, faint copies of `r` under it, gone a few
/// pixels out (never a halo round a whole block).
pub fn glow(p: &Painter, r: Rect, colour: Color32, strength: f32) {
    if strength <= 0.0 {
        return;
    }
    for (grow, a) in [(1.0, 0.13), (2.5, 0.045)] {
        p.rect_filled(r.expand(grow), Rounding::same(grow), colour.gamma_multiply(a * strength.min(1.0)));
    }
}

/// One point in whole physical pixels: the Stark hairline at any scale.
fn hairline(ppp: f32) -> f32 {
    ppp.round().max(1.0) / ppp
}

/// How deep a Stark panel's cut corners are (top-left and bottom-right, at 45°).
pub const CHAMFER: f32 = 12.0;

/// The cut of a panel at `r`: `CHAMFER`, or none for a panel too small to carry it.
pub fn chamfer(r: Rect) -> f32 {
    if r.width() >= 5.0 * CHAMFER && r.height() >= 3.0 * CHAMFER {
        CHAMFER
    } else {
        0.0
    }
}

/// The corner brackets inside `r`, a hairline thick, on the pixel grid: L-shaped at the top-right
/// and bottom-left, and at the cut corners a line of light along the cut with short arms on
/// from it (`chamfer`). `arm`: how long the arms are; `lit` lays their light round them (the
/// block in use).
pub fn corners(p: &Painter, r: Rect, arm: f32, colour: Color32, lit: f32) {
    if !r.is_positive() || colour.a() == 0 {
        return;
    }
    let ppp = p.ctx().pixels_per_point();
    let t = hairline(ppp);
    let cut = snap(chamfer(r), ppp);
    let r = snapped(r, ppp);
    let arm = snap(arm.min(r.width() * 0.5).min(r.height() * 0.5), ppp);
    let reach = snap((arm * 0.7).max(4.0), ppp);
    let bar = |rect: Rect| {
        glow(p, rect, colour, lit);
        p.rect_filled(rect, Rounding::ZERO, colour);
    };
    for (x, y, dx, dy, cut) in [(r.left(), r.top(), 1.0, 1.0, cut), (r.right(), r.top(), -1.0, 1.0, 0.0), (r.left(), r.bottom(), 1.0, -1.0, 0.0), (r.right(), r.bottom(), -1.0, -1.0, cut)] {
        // (At a cut corner the arms start where the cut ends.)
        bar(Rect::from_two_pos(pos2(x + cut * dx, y), pos2(x + (cut + if cut > 0.0 { reach } else { arm }) * dx, y + t * dy)));
        bar(Rect::from_two_pos(pos2(x, y + cut * dy), pos2(x + t * dx, y + (cut + if cut > 0.0 { reach } else { arm }) * dy)));
        if cut > 0.0 {
            let h = t * 0.5;
            p.line_segment([pos2(x + h * dx, y + cut * dy), pos2(x + cut * dx, y + h * dy)], Stroke::new(t * 1.2, colour));
        }
    }
}

/// A Stark panel's glass and frame: one plain quad of glass (`surface`) with fine scan lines of
/// the theme's light, its top-left and bottom-right corners cut off (shaded away and edged), and
/// a hairline in `edge` (as given) round it. `alpha`: the glass's own opacity.
pub fn panel(p: &Painter, r: Rect, fill: Color32, edge: Color32, alpha: f32) {
    if !r.is_positive() || alpha <= 0.0 {
        return;
    }
    surface(p, r, fill, alpha);
    let ppp = p.ctx().pixels_per_point();
    let t = hairline(ppp);
    let c = snap(chamfer(r), ppp);
    let r = snapped(r, ppp);
    let mut mesh = egui::Mesh::default();
    // The hologram's scan lines, every third pixel, clear of the cut corners.
    let light = crate::theme_engine::current().primary().gamma_multiply(0.035 * alpha);
    let mut y = r.top() + 2.0;
    while y < r.bottom() - 1.0 {
        let left = r.left() + (c - (y - r.top())).max(0.0);
        let right = r.right() - (c - (r.bottom() - y)).max(0.0);
        mesh.add_colored_rect(Rect::from_min_max(pos2(left, y), pos2(right, y + t)), light);
        y += 3.0;
    }
    if c > 0.0 {
        // The cut corners, shaded off the glass down to the canvas.
        let shade = Color32::from_black_alpha((235.0 * alpha.min(1.0)) as u8);
        for (a, b, d) in [(r.left_top(), pos2(r.left() + c, r.top()), pos2(r.left(), r.top() + c)), (r.right_bottom(), pos2(r.right() - c, r.bottom()), pos2(r.right(), r.bottom() - c))] {
            let at = mesh.vertices.len() as u32;
            for v in [a, b, d] {
                mesh.colored_vertex(v, shade);
            }
            mesh.add_triangle(at, at + 1, at + 2);
        }
    }
    p.add(egui::Shape::mesh(mesh));
    if c <= 0.0 {
        border(p, r, 1.0, edge);
        return;
    }
    for side in [
        Rect::from_min_max(pos2(r.left() + c, r.top()), pos2(r.right(), r.top() + t)),
        Rect::from_min_max(pos2(r.left(), r.bottom() - t), pos2(r.right() - c, r.bottom())),
        Rect::from_min_max(pos2(r.left(), r.top() + c), pos2(r.left() + t, r.bottom() - t)),
        Rect::from_min_max(pos2(r.right() - t, r.top() + t), pos2(r.right(), r.bottom() - c)),
    ] {
        p.rect_filled(side, Rounding::ZERO, edge);
    }
    let h = t * 0.5;
    p.line_segment([pos2(r.left() + h, r.top() + c), pos2(r.left() + c, r.top() + h)], Stroke::new(t, edge));
    p.line_segment([pos2(r.right() - c, r.bottom() - h), pos2(r.right() - h, r.bottom() - c)], Stroke::new(t, edge));
}

/// A block's shell, as the films draw a panel: hologram glass with its corners cut, a hairline
/// frame in a faint tint of the theme, and bright corner brackets (the theme's own colour, and
/// lit, on the block in use). `alpha`: the block's own opacity (its entrance, a search that dims
/// it).
pub fn shell(p: &Painter, r: Rect, active: bool, alpha: f32) {
    if !r.is_positive() || alpha <= 0.0 {
        return;
    }
    let edge = if active { crate::hud_board::active_colour().gamma_multiply(0.45) } else { crate::hud_board::border_colour() };
    panel(p, r, if active { crate::hud_board::BLOCK_FILL_LIFTED } else { crate::hud_board::BLOCK_FILL }, edge.gamma_multiply(alpha), alpha);
    let (bracket, lit) = if active { (crate::hud_board::active_colour(), 0.8) } else { (crate::hud_board::bracket_colour().gamma_multiply(0.85), 0.0) };
    corners(p, r, BRACKET_ARM, bracket.gamma_multiply(alpha), lit * alpha);
    marks(p, r, active, alpha);
}

/// How long a corner bracket's arms are (long and fine).
pub const BRACKET_ARM: f32 = 14.0;
/// The Stark panel's tab: a short bar of the theme's colour on the top edge, just past the
/// top-left bracket (as the films mark a panel). Narrow blocks go without it.
pub const TAB_W: f32 = 28.0;

/// Where the tab of a block at `r` lies (a hairline on its top edge), if it has room.
pub fn tab_rect(r: Rect, pixel: f32) -> Option<Rect> {
    let ppp = 1.0 / pixel;
    (r.width() >= 5.0 * TAB_W).then(|| Rect::from_min_size(pos2(snap(r.left() + chamfer(r) + BRACKET_ARM + 4.0, ppp), snap(r.top(), ppp)), vec2(TAB_W, hairline(ppp))))
}

/// The Stark marks on a panel's edges: the red tab with a short dash after it and a fine tick
/// ruler hanging from the top edge, and a hologram-blue mark on the bottom edge by the right
/// bracket. Light on the frame, nothing added round it; narrow panels go without.
pub fn marks(p: &Painter, r: Rect, active: bool, alpha: f32) {
    let pixel = px(p);
    let Some(tab) = tab_rect(r, pixel) else { return };
    let (ppp, line) = (1.0 / pixel, tab.height());
    let theme = crate::theme_engine::current();
    let red = theme.primary().gamma_multiply(if active { 1.0 } else { 0.8 } * alpha);
    glow(p, tab, red, if active { 0.7 } else { 0.25 } * alpha);
    p.rect_filled(tab, Rounding::ZERO, red);
    p.rect_filled(Rect::from_min_size(pos2(tab.right() + 4.0, tab.top()), vec2(6.0, line)), Rounding::ZERO, red.gamma_multiply(0.6));
    let tick = MUTED.gamma_multiply(0.45 * alpha);
    for k in 0..5 {
        let x = snap(tab.right() + 16.0 + 4.0 * k as f32, ppp);
        p.rect_filled(Rect::from_min_size(pos2(x, tab.top()), vec2(line, if k == 2 { 5.0 } else { 3.0 })), Rounding::ZERO, tick);
    }
    let blue = theme.secondary().gamma_multiply(if active { 0.95 } else { 0.6 } * alpha);
    let w = (TAB_W * 0.6).round();
    p.rect_filled(Rect::from_min_size(pos2(snap(r.right() - chamfer(r) - BRACKET_ARM - 4.0 - w, ppp), snap(r.bottom(), ppp) - line), vec2(w, line)), Rounding::ZERO, blue);
}

/// `a` to `b` by `t`, channel by channel (both as they are stored: premultiplied).
pub fn lerp_colour(a: Color32, b: Color32, t: f32) -> Color32 {
    let ch = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t.clamp(0.0, 1.0)).round() as u8;
    Color32::from_rgba_premultiplied(ch(a.r(), b.r()), ch(a.g(), b.g()), ch(a.b(), b.b()), ch(a.a(), b.a()))
}

/// The ink for text on a coloured `fill`: dark or light, whichever reads better (WCAG contrast;
/// one of the two always reaches 4.58:1, on the white and yellow themes too; the HUD's own
/// near-black would leave a mid grey at 4.49).
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

/// A vertical scroll area whose content fades into the block at an edge it goes on past (the
/// top once scrolled, the bottom while more follows), with a slim mark of where it stands:
/// rows are never cut off hard.
pub fn faded<R>(ui: &mut Ui, area: egui::ScrollArea, add: impl FnOnce(&mut Ui) -> R) -> egui::scroll_area::ScrollAreaOutput<R> {
    faded_into(ui, area, crate::hud_board::BLOCK_FILL, add)
}

/// [`faded`] on a surface of another `fill` (a see-through workspace window: the block's fill
/// would lay dark bars over it).
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
    p.rect_filled(Rect::from_min_size(pos2(snap(r.right() - 2.0, ppp), y), vec2(1.0 / ppp, h)), Rounding::ZERO, crate::hud_board::active_colour().gamma_multiply(0.6));
    out
}

/// A block's surface: faint glass, flat (no sheen, no gradient). `alpha`: the block's own
/// opacity (its entrance, a search that dims it).
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
        // (A ring of light, a hairline: thin as every line of the HUD.)
        p.circle_stroke(centre, radius * (1.04 + 0.55 * out), stroke(px(p).max(0.8), colour.gamma_multiply(0.85 * fade)));
    }
    true
}

/// A block's title as it fits `width`: whole when it does; else, of a "FAMILY // NAME" title,
/// the name alone, else the family alone; and only then cut with an ellipsis. (Seen live
/// 2026-11-12: a narrow block read "BRIEFING //…".)
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

/// A title as the HUD letters it: the display face, upper case, the letters spaced apart as the
/// films track them, on one line (cut with an ellipsis where it is too long for `width`).
pub fn title_galley(p: &Painter, text: &str, size: f32, colour: Color32, width: f32) -> std::sync::Arc<egui::Galley> {
    tracked(p, text, title_font(size), 0.14, colour, width)
}

/// A label as the films letter it: small capitals in the label face, tracked wide.
pub fn label_galley(p: &Painter, text: &str, size: f32, colour: Color32) -> std::sync::Arc<egui::Galley> {
    tracked(p, text, label_font(size), 0.16, colour, f32::INFINITY)
}

/// `text` in capitals, its letters `spacing` ems apart, on one line of at most `width`.
fn tracked(p: &Painter, text: &str, font: FontId, spacing: f32, colour: Color32, width: f32) -> std::sync::Arc<egui::Galley> {
    let mut job = egui::text::LayoutJob::default();
    let size = font.size;
    job.append(&text.to_uppercase(), 0.0, egui::TextFormat { font_id: font, color: colour, extra_letter_spacing: size * spacing, ..Default::default() });
    job.wrap.max_width = width.max(10.0);
    job.wrap.max_rows = 1;
    job.wrap.break_anywhere = true;
    job.wrap.overflow_character = Some('…');
    p.layout_job(job)
}

/// A readout as the films set large figures: every digit as wide as the widest, so a changing
/// number (a clock's seconds) never jitters.
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

/// A title decoding itself, `progress` 0 … 1: its letters settle left to right and the rest
/// flicker through other letters (`clock` in seconds); spaces and marks stand from the start.
/// Played backwards as a block leaves.
pub fn decode(text: &str, progress: f32, clock: f64) -> std::borrow::Cow<'_, str> {
    if progress >= 1.0 {
        return text.into();
    }
    const POOL: &[u8] = b"ABCDEFGHJKLMNPRSTUVWXYZ0123456789";
    let settled = (progress.max(0.0) * text.chars().count() as f32) as usize;
    let tick = (clock * 30.0) as usize;
    text.chars().enumerate().map(|(i, c)| if i < settled || !c.is_alphanumeric() { c } else { POOL[(i * 7 + tick * 13 + i * tick) % POOL.len()] as char }).collect::<String>().into()
}

/// How long a changed digit takes to roll in.
const ROLL_SECS: f64 = 0.22;

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
    let e = motion::ease_out(t);
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
            // (Faint: light the emblem throws, not a glow of the block's own.)
            mesh.colored_vertex(q, colour.gamma_multiply(0.3 * a));
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

// (A dragged block used to line itself up with its neighbours and the screen, with guide lines
// while it was held. Gone on 2026-11-13: Sir wants no alignment help. A block goes where the
// hand has it.)

/// A block let go while it moved faster than this (points per second) is thrown.
pub const THROW_FROM: f32 = 420.0;
/// Flung away faster than this, a block closes (it waits dim in the tray for ten minutes).
pub const FLING_AWAY: f32 = 3000.0;

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
/// lie on a screen.
const EDGE_FALLOFF: u8 = 44;
const DUST: usize = 40;

/// The glass the HUD is seen through: the light is not quite even across it, it falls off
/// towards the edges, and a little fine dust lies on it (no scratches: nothing ornamental). All
/// of it is still (it asks for no frame) and faint enough that text under it stays sharp.
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
            let colour = if v < 0.5 { Color32::from_black_alpha((v * 2.0 * 8.0) as u8) } else { white((v - 0.5) * 2.0 * 0.01) };
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
    // Dust: single physical pixels, faint.
    let mut next = chance(0x0D57_C0DE);
    let ppp = p.ctx().pixels_per_point();
    for _ in 0..DUST {
        let at = pos2(snap(rect.left() + rect.width() * next(), ppp), snap(rect.top() + rect.height() * next(), ppp));
        mesh.add_colored_rect(Rect::from_min_size(at, Vec2::splat(1.0 / ppp)), white(0.025 + next() * 0.035));
    }
    p.add(egui::Shape::mesh(mesh));
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

/// The state mark, as the films mark a state (no coloured lamps): a small steady tick for all
/// is well, a square that blinks slowly while busy, a diamond for trouble (amber; red when it
/// failed), nothing at rest. Returns whether it moves.
pub fn paint_light(p: &Painter, centre: Pos2, light: Light, time: f64, alpha: f32) -> bool {
    let ppp = p.ctx().pixels_per_point();
    let c = pos2(snap(centre.x, ppp), snap(centre.y, ppp));
    match light {
        Light::Rest => {}
        Light::Good => {
            let s = stroke(1.2, TEXT.gamma_multiply(0.6 * alpha));
            p.line_segment([c + vec2(-3.0, 0.0), c + vec2(-1.0, 2.5)], s);
            p.line_segment([c + vec2(-1.0, 2.5), c + vec2(3.5, -2.5)], s);
        }
        Light::Busy => {
            // (A slow blink, eased: on, off, on, about once a second and a half.)
            let on = 0.5 + 0.5 * (time * std::f64::consts::TAU / 1.4).sin() as f32;
            p.rect_filled(Rect::from_center_size(c, Vec2::splat(4.0)), Rounding::ZERO, light_colour(light).gamma_multiply((0.2 + 0.8 * on) * alpha));
        }
        Light::Warn | Light::Fault => {
            let diamond = vec![c + vec2(0.0, -4.0), c + vec2(4.0, 0.0), c + vec2(0.0, 4.0), c + vec2(-4.0, 0.0)];
            p.add(egui::Shape::convex_polygon(diamond, light_colour(light).gamma_multiply(alpha), Stroke::NONE));
        }
    }
    light == Light::Busy
}

/// The label face (Rajdhani Medium): small capitals and the lighter large figures.
pub fn label_font(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name("label".into()))
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
    key_lit(p, right, mid, label, if hot { 1.0 } else { 0.0 }, if down { 1.0 } else { 0.0 }, lit, alpha)
}

/// A key made of light: its bracketed word and no box. At rest it is quiet blue-grey; under the
/// pointer (`hot` 0 … 1 as it comes up) its letters brighten and a hairline runs under them; a
/// press flashes it once (`flash` 1 … 0 as the press fades).
#[allow(clippy::too_many_arguments)]
fn key_lit(p: &Painter, right: f32, mid: f32, label: &str, hot: f32, flash: f32, lit: bool, alpha: f32) -> Rect {
    let galley = label_galley(p, label, KEY_FONT, Color32::PLACEHOLDER);
    let r = Rect::from_min_size(pos2(right - galley.size().x - 10.0, mid - KEY_H / 2.0), vec2(galley.size().x + 10.0, KEY_H));
    let accent = crate::hud_board::active_colour();
    let rest = if lit { accent } else { MUTED };
    let ink = lerp_colour(lerp_colour(rest, TEXT, hot), Color32::WHITE, flash);
    let text = Rect::from_min_size(pos2(r.left() + 5.0, mid - galley.size().y * 0.5), galley.size());
    let under = (0.7 * hot).max(if lit { 0.4 } else { 0.0 }).max(flash);
    if under > 0.0 {
        let ppp = p.ctx().pixels_per_point();
        let line = Rect::from_min_size(pos2(snap(text.left(), ppp), snap(text.bottom() + 1.0, ppp)), vec2(snap(text.width(), ppp), 1.0 / ppp));
        glow(p, line, accent, flash * alpha);
        p.rect_filled(line, Rounding::ZERO, accent.gamma_multiply(under * alpha));
    }
    p.galley_with_override_text_color(text.min, galley, ink.gamma_multiply(alpha));
    r
}

/// A header key's width.
pub fn key_width(p: &Painter, label: &str) -> f32 {
    label_galley(p, label, KEY_FONT, Color32::PLACEHOLDER).size().x + 10.0
}

/// A header key's lettering and height (larger since 2026-11-15: readable at a glance).
const KEY_FONT: f32 = 11.5;
pub const KEY_H: f32 = 18.0;

/// The header keys of a block, right to left from `right`: each (label, lit, hint). Returns
/// which was clicked this frame and where the leftmost key begins. They light up under the
/// pointer and give under the press.
/// How many of a header's keys leave a title `title` wide its room in a row `row` wide: all of
/// them when it fits, else the last ones go first (a narrow block keeps its title whole), and
/// the first always stays.
pub fn keys_that_fit(p: &Painter, labels: &[(&str, bool, &str)], row: f32, title: f32) -> usize {
    let mut used = 0.0;
    let mut fit = 0;
    for (k, (label, _, _)) in labels.iter().enumerate() {
        used += key_width(p, label) + 4.0;
        if k > 0 && row - used < title {
            break;
        }
        fit = k + 1;
    }
    fit
}

pub fn keys(ui: &mut Ui, id: egui::Id, right: f32, mid: f32, labels: &[(&str, bool, &str)], alpha: f32) -> (Option<usize>, f32) {
    let mut clicked = None;
    let mut x = right;
    for (k, (label, lit, hint)) in labels.iter().enumerate() {
        let width = key_width(ui.painter(), label);
        let r = Rect::from_min_size(pos2(x - width, mid - KEY_H / 2.0), vec2(width, KEY_H));
        let response = ui.interact(r, id.with(("key", k)), Sense::click());
        let hot = ui.ctx().animate_bool_with_time(id.with(("hot", k)), response.hovered(), motion::FAST.as_secs_f32());
        // The flash: full while pressed, gone a FAST beat after the press ends.
        let (now, pressed) = (ui.input(|i| i.time), id.with(("pressed", k)));
        if response.is_pointer_button_down_on() {
            ui.data_mut(|d| d.insert_temp(pressed, now));
        }
        let since = ui.data(|d| d.get_temp::<f64>(pressed)).map_or(1.0, |t| (now - t) as f32);
        let flash = (1.0 - since / motion::FAST.as_secs_f32()).clamp(0.0, 1.0);
        if flash > 0.0 {
            ui.ctx().request_repaint();
        }
        key_lit(ui.painter(), x, mid, label, hot, flash, *lit, alpha);
        if response.clicked() {
            clicked = Some(k);
        }
        let tip = tip_with_sentence(label, hint);
        if !tip.is_empty() {
            response.on_hover_text(tip);
        }
        x -= width + 4.0;
    }
    (clicked, x + 4.0)
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
    let dim = white(0.035);
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
        // (A picture to come is a hairline frame, not a grey slab.)
        if kind == Outline::Picture && k == 0 {
            border(&p, *s, 1.0, white(0.10));
        } else {
            p.rect_filled(*s, Rounding::ZERO, dim);
        }
    }
    // The scan that tells "still loading" from "empty": a thin line of light that sweeps across
    // the block every 1.4 s, a faint wake behind it.
    let ppp = p.ctx().pixels_per_point();
    let t = ((time / 1.4).fract()) as f32;
    let x = snap(r.left() + r.width() * t, ppp);
    let accent = crate::hud_board::active_colour();
    let mut wake = egui::Mesh::default();
    for (wx, c) in [(x - 48.0, Color32::TRANSPARENT), (x, accent.gamma_multiply(0.07))] {
        wake.colored_vertex(pos2(wx, r.top()), c);
        wake.colored_vertex(pos2(wx, r.bottom()), c);
    }
    wake.add_triangle(0, 1, 2);
    wake.add_triangle(1, 3, 2);
    p.add(egui::Shape::mesh(wake));
    let line = Rect::from_min_max(pos2(x, r.top()), pos2(x + 1.0 / ppp, r.bottom()));
    glow(&p, line, accent, 0.5);
    p.rect_filled(line, Rounding::ZERO, accent.gamma_multiply(0.7));
    p.ctx().request_repaint_after(std::time::Duration::from_millis(16));
}

/// One plain sentence in the middle of a block that has nothing to show.
pub fn empty(ui: &Ui, r: Rect, sentence: &str) {
    empty_on(&ui.painter_at(r), r, sentence);
}

/// `empty` for a painter.
pub fn empty_on(p: &Painter, r: Rect, sentence: &str) {
    let p = p.with_clip_rect(r.intersect(p.clip_rect()));
    let c = r.center();
    // A small empty frame above the words, in hairlines.
    let mark = Rect::from_center_size(c - vec2(0.0, 18.0), vec2(22.0, 16.0));
    border(&p, mark, 1.0, white(0.3));
    p.line_segment([mark.left_bottom(), mark.right_top()], stroke(px(&p), white(0.16)));
    let font = FontId::proportional(12.5);
    let galley = p.layout(sentence.to_string(), font, MUTED, (r.width() - 24.0).max(60.0));
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

/// What went wrong, under the trouble mark: a diamond in hairlines (a painter's: no `[ RETRY ]`).
pub fn failed_on(p: &Painter, r: Rect, what: &str) {
    let p = p.with_clip_rect(r.intersect(p.clip_rect()));
    let c = r.center();
    // Short blocks keep the sentence and drop the mark.
    let tall = r.height() >= 96.0;
    if tall {
        let m = c - vec2(0.0, 30.0);
        let diamond = vec![m + vec2(0.0, -11.0), m + vec2(11.0, 0.0), m + vec2(0.0, 11.0), m + vec2(-11.0, 0.0)];
        p.add(egui::Shape::closed_line(diamond, stroke(1.2, FAULT)));
        p.line_segment([m + vec2(0.0, -5.0), m + vec2(0.0, 1.5)], stroke(1.2, FAULT));
        p.circle_filled(m + vec2(0.0, 4.5), 0.9, FAULT);
    }
    let font = FontId::proportional(12.5);
    let galley = p.layout(what.to_string(), font, TEXT, (r.width() - 24.0).max(60.0));
    let y = if tall { c.y - 12.0 } else { c.y - galley.size().y * 0.5 };
    p.galley(pos2(c.x - galley.size().x * 0.5, y), galley, Color32::PLACEHOLDER);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// Text on a theme's own colour reads on every theme, the white and yellow ones too.
    #[test]
    fn text_on_every_theme_colour_reaches_four_and_a_half_to_one() {
        let mut fills: Vec<Color32> = crate::theme_engine::PRESETS.iter().map(|(_, p)| Color32::from_rgb(p.core_primary[0], p.core_primary[1], p.core_primary[2])).collect();
        fills.extend([Color32::WHITE, Color32::from_rgb(255, 214, 0), Color32::from_rgb(20, 40, 160), Color32::from_rgb(120, 120, 120)]);
        for fill in fills {
            assert!(contrast(fill, ink_on(fill)) >= 4.5, "{fill:?}: {}", contrast(fill, ink_on(fill)));
        }
        assert_eq!(ink_on(Color32::WHITE), Color32::BLACK);
        assert_eq!(ink_on(Color32::from_rgb(20, 40, 160)), Color32::WHITE);
        assert!((contrast(Color32::BLACK, Color32::WHITE) - 21.0).abs() < 0.01);
    }

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
        // (One mesh: the scratches went with the film look, nothing ornamental on the glass.)
        assert_eq!(shapes.len(), 1, "one mesh");
        assert!(shapes[0] <= EDGE_FALLOFF, "{shapes:?}");
        // "White at a share" is what it says: a twentieth lifts a dark surface by a twentieth.
        assert_eq!(white(0.05), Color32::from_rgba_premultiplied(13, 13, 13, 13));
        let wait = out.viewport_output.values().map(|v| v.repaint_delay).min().unwrap_or(Duration::MAX);
        assert!(wait > Duration::from_millis(500), "the glass never drives the frame rate: {wait:?}");
        let mut a = chance(7);
        let mut b = chance(7);
        assert!((0..50).all(|_| a() == b()) && (0..50).all(|_| (0.0..1.0).contains(&a())));
    }

    /// A title decodes left to right: as long as the title, its marks and spaces standing from
    /// the start, the settled letters its own, whole at the end.
    #[test]
    fn a_title_decodes_left_to_right() {
        let start = decode("CHART // 2026", 0.0, 0.5);
        assert_eq!(start.chars().count(), 13);
        assert_eq!(start.chars().nth(5), Some(' '));
        assert_eq!(start.chars().nth(6), Some('/'));
        assert!(decode("CHART // 2026", 0.5, 0.5).starts_with("CHART "));
        assert_eq!(decode("CHART // 2026", 1.0, 0.5), "CHART // 2026");
    }

    /// Every key's sentence does what the key does, without the language model; [ YES ], [ NO ]
    /// and [ NONE ] answer the question their chip asks and are no orders of their own.
    #[test]
    fn a_key_s_sentence_takes_the_key_s_road() {
        let roads = [("[ DETACH ]", "configure_interface"), ("[ POP OUT ]", "configure_interface"), ("[ UNDO ]", "undo_last_action")];
        let wrong: Vec<String> = SAY_INSTEAD
            .iter()
            .filter(|(key, _)| !matches!(*key, "[ YES ]" | "[ NO ]" | "[ NONE ]"))
            .filter_map(|(key, said)| {
                let tool = roads.iter().find(|(k, _)| k == key).map(|(_, t)| *t);
                let got = crate::phrasings::route(said);
                (got.as_ref().map(|(t, _)| t.as_str()) != tool).then(|| format!("{key} \"{said}\": {got:?}"))
            })
            .collect();
        assert!(wrong.is_empty(), "{wrong:#?}");
        // A block's [ PIN ] names the block ("pin it" pins a pop-up): its tip says so itself.
        // (An open block's own name routes the same way: `hud_dock::blocks_named`.)
        let pinned = crate::phrasings::route("pin the chart");
        assert!(pinned.as_ref().is_some_and(|(tool, args)| tool == "hud_board" && args["action"] == "pin"), "{pinned:?}");
        assert_eq!(tip_with_sentence("[ DETACH ]", "Own window."), "Own window. Or say “detach it”.");
        assert_eq!(tip_with_sentence("[ UNDO ]", ""), "Say “undo that”.");
        assert_eq!(tip_with_sentence("[ CLOSE ]", "Close"), "Close", "no sentence of its own: the tip as it was");
    }

    /// "12:40" turning "12:41": the four figures that stay stand still, the changed one draws
    /// the old figure leaving and the new arriving, and a quarter of a second later it is one
    /// readout again.
    #[test]
    fn a_changed_digit_rolls_into_place_and_the_rest_stand() {
        let ctx = egui::Context::default();
        crate::gui::install_fonts(&ctx);
        let shapes = |text: &str, time: f64| {
            let input = egui::RawInput { time: Some(time), ..Default::default() };
            ctx.run(input, |ctx| {
                let p = ctx.layer_painter(egui::LayerId::background());
                rolling_figures(&p, egui::Id::new("roll_test"), pos2(10.0, 10.0), text, FontId::monospace(20.0), Color32::WHITE);
            })
            .shapes
            .len()
        };
        assert_eq!(shapes("12:40", 0.0), 1, "at rest: one readout");
        assert_eq!(shapes("12:41", 1.0), 4 + 2, "four stand, one rolls (old and new)");
        assert_eq!(shapes("12:41", 1.1), 4 + 2, "still rolling");
        assert_eq!(shapes("12:41", 1.5), 1, "settled");
    }

    /// Sharp at any scaling: frames and brackets sit on whole physical pixels at 100,
    /// 125 and 150 %, one pixel thin (the brackets too, but two at 150 %: Stark hairlines).
    #[test]
    fn hairlines_sit_on_whole_pixels_at_any_scaling() {
        for ppp in [1.0f32, 1.25, 1.5] {
            for v in [0.0f32, 0.3, 10.49, 117.77] {
                let at = snap(v, ppp) * ppp;
                assert!((at - at.round()).abs() < 1e-3 && (snap(v, ppp) - v).abs() <= 0.5 / ppp + 1e-4, "{v} at {ppp}");
            }
            let ctx = egui::Context::default();
            ctx.set_pixels_per_point(ppp);
            let out = ctx.run(egui::RawInput::default(), |ctx| {
                let p = ctx.layer_painter(egui::LayerId::background());
                let r = Rect::from_min_max(pos2(10.3, 20.6), pos2(210.45, 140.2));
                border(&p, r, 1.0, Color32::WHITE);
                corners(&p, r, BRACKET_ARM, Color32::WHITE, 0.0);
            });
            let rects: Vec<Rect> = out.shapes.iter().filter_map(|s| if let egui::Shape::Rect(r) = &s.shape { Some(r.rect) } else { None }).collect();
            assert_eq!(rects.len(), 4 + 8, "four sides, eight arms at {ppp}");
            for (k, r) in rects.iter().enumerate() {
                for edge in [r.left(), r.right(), r.top(), r.bottom()] {
                    assert!((edge * ppp - (edge * ppp).round()).abs() < 1e-3, "{r:?} off the pixel grid at {ppp}");
                }
                let thin = (r.width().min(r.height()) * ppp).round();
                // (A bracket is a `hairline`: two pixels at 150 %, where one would be half a point.)
                assert_eq!(thin, if k < 4 { 1.0 } else { ppp.round() }, "{r:?} at {ppp}");
            }
        }
    }

    /// A narrow block keeps its title whole: its keys go, the last first, and the first stays.
    #[test]
    fn a_narrow_header_lets_its_keys_go_before_its_title() {
        let ctx = egui::Context::default();
        // (The keys are lettered in the HUD's own label face.)
        crate::gui::install_fonts(&ctx);
        let labels = [("[ CLOSE ]", false, ""), ("[ DETACH ]", false, ""), ("[ PIN ]", false, "")];
        let mut fits = Vec::new();
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            let p = ctx.layer_painter(egui::LayerId::background());
            let all: f32 = labels.iter().map(|(l, _, _)| key_width(&p, l) + 4.0).sum();
            let close = key_width(&p, "[ CLOSE ]") + 4.0;
            fits = vec![
                keys_that_fit(&p, &labels, 200.0 + all, 200.0),
                keys_that_fit(&p, &labels, 199.0 + all, 200.0),
                keys_that_fit(&p, &labels, 200.0 + close, 200.0),
                keys_that_fit(&p, &labels, 40.0, 200.0),
                keys_that_fit(&p, &labels[..0], 400.0, 200.0),
            ];
        });
        assert_eq!(fits, [3, 2, 1, 1, 0], "all, without the last, the first alone, the first even when the title is cut, none of none");
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
