//! The hologram kit: everything the HUD draws is light on black glass, and this is how light is
//! drawn. Hairlines snapped to the pixel grid for structure, soft-edged ribbons that add up
//! where they cross for anything that glows (rings, arcs, traces), cut-corner plates for glass,
//! tracked wide capitals for labels and thin squared figures for readings.
//!
//! The rules it is built on (from the films' interface designers, see CLAUDE.md "The Stark
//! look"): every mark has a function, structure is white and colour is information, thick
//! faint bands sit beside bright hairlines, and what is nearer is brighter.

use eframe::egui::{self, pos2, vec2, Align2, Color32, FontData, FontDefinitions, FontFamily, FontId, Mesh, Painter, Pos2, Rect, Shape, Stroke, Vec2};
use std::f32::consts::TAU;
use std::sync::Arc;

pub use crate::holo_theme::{current as theme, Theme};

// ---------------------------------------------------------------------------------- type ----

/// The HUD's faces.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Face {
    /// Wide capitals: titles, labels, keys (Michroma).
    Label,
    /// Thin squared figures and headlines (Oxanium Light).
    Figure,
    /// The same face with weight: a headline, a selected figure (Oxanium SemiBold).
    Head,
    /// Sentences (Barlow).
    Body,
    /// Sentences with weight (Barlow SemiBold).
    Strong,
    /// Columns of data, the console (Share Tech Mono).
    Mono,
}

const FACES: [(&str, &[u8]); 7] = [
    ("holo-label", include_bytes!("../assets/fonts/Michroma-Regular.ttf")),
    ("holo-figure", include_bytes!("../assets/fonts/Oxanium-Light.ttf")),
    ("holo-head", include_bytes!("../assets/fonts/Oxanium-SemiBold.ttf")),
    ("holo-body", include_bytes!("../assets/fonts/Barlow-Medium.ttf")),
    ("holo-strong", include_bytes!("../assets/fonts/Barlow-SemiBold.ttf")),
    ("holo-mono", include_bytes!("../assets/fonts/ShareTechMono-Regular.ttf")),
    ("holo-plain", include_bytes!("../assets/fonts/Oxanium-Regular.ttf")),
];

/// One of the HUD's own faces as a file, for the phone page ("label", "figure", "body", "mono").
pub fn face_file(name: &str) -> Option<&'static [u8]> {
    FACES.iter().find(|(n, _)| n.strip_prefix("holo-") == Some(name)).map(|(_, bytes)| *bytes)
}

fn family(face: Face) -> FontFamily {
    FontFamily::Name(
        match face {
            Face::Label => "holo-label",
            Face::Figure => "holo-figure",
            Face::Head => "holo-head",
            Face::Body => "holo-body",
            Face::Strong => "holo-strong",
            Face::Mono => "holo-mono",
        }
        .into(),
    )
}

pub fn font(face: Face, size: f32) -> FontId {
    FontId::new(size, family(face))
}

/// Registers the faces. What a face has no glyph for (arrows, box drawing, symbols, other
/// scripts) comes from Windows' own fonts, then from egui's.
pub fn install_fonts(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    for (name, bytes) in FACES {
        fonts.font_data.insert(name.to_string(), FontData::from_static(bytes));
    }
    #[cfg_attr(not(windows), allow(unused_mut))]
    let mut system: Vec<String> = Vec::new();
    #[cfg(windows)]
    {
        let dir = std::path::PathBuf::from(std::env::var_os("WINDIR").unwrap_or_else(|| "C:\\Windows".into())).join("Fonts");
        for (name, file) in [("segoe", "segoeui.ttf"), ("symbols", "seguisym.ttf")] {
            if let Ok(bytes) = std::fs::read(dir.join(file)) {
                fonts.font_data.insert(name.to_string(), FontData::from_owned(bytes));
                system.push(name.to_string());
            }
        }
    }
    let defaults = fonts.families.get(&FontFamily::Proportional).cloned().unwrap_or_default();
    let fallback: Vec<String> = system.into_iter().chain(defaults).collect();
    let chain = |own: &[&str]| own.iter().map(|n| n.to_string()).chain(fallback.iter().cloned()).collect::<Vec<_>>();
    for (face, own) in [
        (Face::Label, &["holo-label", "holo-plain"][..]),
        (Face::Figure, &["holo-figure", "holo-body"][..]),
        (Face::Head, &["holo-head", "holo-strong"][..]),
        (Face::Body, &["holo-body"][..]),
        (Face::Strong, &["holo-strong"][..]),
        (Face::Mono, &["holo-mono", "holo-body"][..]),
    ] {
        fonts.families.insert(family(face), chain(own));
    }
    fonts.families.insert(FontFamily::Proportional, chain(&["holo-body"]));
    fonts.families.insert(FontFamily::Monospace, chain(&["holo-mono", "holo-body"]));
    // The families egui's text styles and `RichText` ask for by name: "display" (headlines
    // and large figures), "label" (a row's name, a button's word: the squared face, which
    // reads in mixed case as well as in capitals) and "bold".
    fonts.families.insert(FontFamily::Name("display".into()), chain(&["holo-head", "holo-strong"]));
    fonts.families.insert(FontFamily::Name("label".into()), chain(&["holo-plain", "holo-body"]));
    fonts.families.insert(FontFamily::Name("bold".into()), chain(&["holo-strong"]));
    ctx.set_fonts(fonts);
}

/// Text laid out in a face: labels are set in capitals and tracked, the rest as written.
pub fn galley(p: &Painter, face: Face, text: &str, size: f32, colour: Color32) -> Arc<egui::Galley> {
    let (text, spacing) = match face {
        Face::Label => (text.to_uppercase(), size * 0.14),
        Face::Head | Face::Figure => (text.to_string(), size * 0.02),
        _ => (text.to_string(), 0.0),
    };
    let mut job = egui::text::LayoutJob::default();
    job.wrap.max_width = f32::INFINITY;
    job.append(&text, 0.0, egui::TextFormat { font_id: font(face, size), extra_letter_spacing: spacing, color: colour, ..Default::default() });
    p.layout_job(job)
}

/// The same, cut with an ellipsis where it would pass `width`.
pub fn galley_within(p: &Painter, face: Face, text: &str, size: f32, colour: Color32, width: f32) -> Arc<egui::Galley> {
    let (text, spacing) = match face {
        Face::Label => (text.to_uppercase(), size * 0.14),
        Face::Head | Face::Figure => (text.to_string(), size * 0.02),
        _ => (text.to_string(), 0.0),
    };
    let mut job = egui::text::LayoutJob::default();
    job.wrap = egui::text::TextWrapping { max_width: width.max(1.0), max_rows: 1, break_anywhere: true, overflow_character: Some('…') };
    job.append(&text, 0.0, egui::TextFormat { font_id: font(face, size), extra_letter_spacing: spacing, color: colour, ..Default::default() });
    p.layout_job(job)
}

/// Text wrapped into `width` from `at` down and cut after `max_rows` with an ellipsis;
/// returns the height it took.
pub fn wrapped(p: &Painter, at: Pos2, width: f32, face: Face, text: &str, size: f32, colour: Color32, max_rows: usize) -> f32 {
    let (text, spacing) = if face == Face::Label { (text.to_uppercase(), size * 0.14) } else { (text.to_string(), 0.0) };
    let mut job = egui::text::LayoutJob::default();
    job.wrap = egui::text::TextWrapping { max_width: width.max(10.0), max_rows: max_rows.max(1), break_anywhere: false, overflow_character: Some('…') };
    job.append(&text, 0.0, egui::TextFormat { font_id: font(face, size), extra_letter_spacing: spacing, color: colour, ..Default::default() });
    let g = p.layout_job(job);
    let height = g.size().y;
    p.galley(pos2(snap(at.x, ppp(p)), snap(at.y, ppp(p))), g, colour);
    height
}

/// Text that runs out of its room fades into the glass instead of being cut: a band of
/// `colour` that hides what is under it at the height `solid` and lets it show at `clear`.
pub fn fade(p: &Painter, left: f32, right: f32, solid: f32, clear: f32, colour: Color32) {
    let mut mesh = egui::Mesh::default();
    for (y, c) in [(solid, colour), (clear, Color32::TRANSPARENT)] {
        mesh.colored_vertex(pos2(left, y), c);
        mesh.colored_vertex(pos2(right, y), c);
    }
    mesh.add_triangle(0, 1, 2);
    mesh.add_triangle(1, 3, 2);
    p.add(Shape::mesh(mesh));
}

/// Draws `text` anchored at `at`; returns where it stands.
pub fn text(p: &Painter, at: Pos2, anchor: Align2, face: Face, text: &str, size: f32, colour: Color32) -> Rect {
    let g = galley(p, face, text, size, colour);
    let rect = anchor.anchor_size(at, g.size());
    // (Text on whole pixels: half a pixel off and every stem turns grey.)
    let min = pos2(snap(rect.min.x, ppp(p)), snap(rect.min.y, ppp(p)));
    p.galley(min, g, colour);
    Rect::from_min_size(min, rect.size())
}

// -------------------------------------------------------------------------------- colour ----

/// `colour` at `alpha` (0-1) of its own strength.
pub fn a(colour: Color32, alpha: f32) -> Color32 {
    colour.gamma_multiply(alpha.clamp(0.0, 1.0))
}

/// `colour` as light that adds to what is under it. Crossing lines brighten each other, and
/// as light piles up it whitens (a tenth of it is white), the way a bright lamp burns out to
/// white in a camera: dense places glow hot instead of turning into a flat patch of colour.
pub fn lit(colour: Color32, strength: f32) -> Color32 {
    let k = strength.clamp(0.0, 1.0);
    let f = |v: u8| ((v as f32 * 0.90 + 25.5) * k) as u8;
    Color32::from_rgba_premultiplied(f(colour.r()), f(colour.g()), f(colour.b()), 0)
}

fn smoothstep(lo: f32, hi: f32, x: f32) -> f32 {
    let t = ((x - lo) / (hi - lo)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// The colour of light at brightness `x` (0-1): dim light is the deep tone, brighter the
/// accent, the brightest nearly white. A dim yellow would read as olive; a glow that falls off
/// through amber reads as light.
pub fn glow_colour(t: &Theme, x: f32) -> [f32; 3] {
    let x = x.clamp(0.0, 1.0);
    let (deep, accent, hot) = (t.deep(), t.accent(), t.hot());
    let warm = smoothstep(0.04, 0.50, x);
    let white = smoothstep(0.55, 1.0, x);
    let ch = |d: u8, a: u8, h: u8| {
        let v = d as f32 + (a as f32 - d as f32) * warm;
        (v + (h as f32 + (255.0 - h as f32) * 0.5 - v) * white) / 255.0 * x
    };
    [ch(deep.r(), accent.r(), hot.r()), ch(deep.g(), accent.g(), hot.g()), ch(deep.b(), accent.b(), hot.b())]
}

fn brightness(c: Color32) -> f32 {
    c.r().max(c.g()).max(c.b()) as f32 / 255.0
}

// --------------------------------------------------------------------------------- bloom ----

/// Light that spills. Lines put their energy into a small map; the map is blurred twice (a
/// tight glow that hugs the line, a wide one that fills the air), coloured by brightness
/// ([`glow_colour`]) and laid under the sharp lines. This is what makes a ring of light look
/// lit rather than drawn.
pub struct Bloom {
    id: egui::Id,
    rect: Rect,
    w: usize,
    h: usize,
    per_point: f32,
    energy: Vec<f32>,
}

impl Bloom {
    /// A glow map over `rect` whose longer side has `cells` cells.
    pub fn new(id: impl std::hash::Hash, rect: Rect, cells: usize) -> Self {
        let per_point = cells as f32 / rect.width().max(rect.height()).max(1.0);
        let w = ((rect.width() * per_point).ceil() as usize).max(2);
        let h = ((rect.height() * per_point).ceil() as usize).max(2);
        Self { id: egui::Id::new(("holo-bloom", id)), rect, w, h, per_point, energy: vec![0.0; w * h] }
    }

    pub fn point(&mut self, at: Pos2, e: f32) {
        let x = (at.x - self.rect.left()) * self.per_point - 0.5;
        let y = (at.y - self.rect.top()) * self.per_point - 0.5;
        if x < -1.0 || y < -1.0 || x >= self.w as f32 || y >= self.h as f32 {
            return;
        }
        let (x0, y0) = (x.floor(), y.floor());
        let (fx, fy) = (x - x0, y - y0);
        for (dx, dy, k) in [(0, 0, (1.0 - fx) * (1.0 - fy)), (1, 0, fx * (1.0 - fy)), (0, 1, (1.0 - fx) * fy), (1, 1, fx * fy)] {
            let (cx, cy) = (x0 as i32 + dx, y0 as i32 + dy);
            if cx >= 0 && cy >= 0 && (cx as usize) < self.w && (cy as usize) < self.h {
                self.energy[cy as usize * self.w + cx as usize] += e * k;
            }
        }
    }

    /// A line of brightness `e` (per point of its length: long or short, it glows the same).
    pub fn line(&mut self, from: Pos2, to: Pos2, e: f32) {
        let cells = (to - from).length() * self.per_point;
        let n = (cells / 0.7).ceil().max(1.0) as usize;
        let each = e * cells.max(0.3) / n as f32;
        for s in 0..n {
            self.point(from + (to - from) * ((s as f32 + 0.5) / n as f32), each);
        }
    }

    /// A box blur of `radius` cells, `passes` times over (three passes are close to a Gaussian).
    fn blurred(&self, radius: usize, passes: usize) -> Vec<f32> {
        let (w, h) = (self.w, self.h);
        let mut a = self.energy.clone();
        let mut b = vec![0.0; w * h];
        let span = (2 * radius + 1) as f32;
        for _ in 0..passes {
            for y in 0..h {
                let row = &a[y * w..(y + 1) * w];
                let mut sum: f32 = row[..(radius + 1).min(w)].iter().sum();
                for x in 0..w {
                    b[y * w + x] = sum / span;
                    if x + radius + 1 < w {
                        sum += row[x + radius + 1];
                    }
                    if x >= radius {
                        sum -= row[x - radius];
                    }
                }
            }
            for x in 0..w {
                let mut sum: f32 = (0..(radius + 1).min(h)).map(|y| b[y * w + x]).sum();
                for y in 0..h {
                    a[y * w + x] = sum / span;
                    if y + radius + 1 < h {
                        sum += b[(y + radius + 1) * w + x];
                    }
                    if y >= radius {
                        sum -= b[(y - radius) * w + x];
                    }
                }
            }
        }
        a
    }

    /// The glow as a picture: premultiplied light with no alpha (it adds to what is under it).
    fn image(&self, strength: f32) -> egui::ColorImage {
        let t = theme();
        let tight = self.blurred(1, 2);
        let wide = self.blurred((self.w.max(self.h) / 26).max(3), 3);
        let pixels = tight
            .iter()
            .zip(&wide)
            .map(|(n, f)| {
                let x = 1.0 - (-(n * 1.15 + f * 2.6) * strength).exp();
                let [r, g, b] = glow_colour(&t, x);
                Color32::from_rgba_premultiplied((r * 255.0) as u8, (g * 255.0) as u8, (b * 255.0) as u8, 0)
            })
            .collect();
        egui::ColorImage { size: [self.w, self.h], pixels }
    }

    pub fn paint(self, p: &Painter, strength: f32) {
        if strength <= 0.0 || self.energy.iter().all(|e| *e <= 0.0) {
            return;
        }
        let image = self.image(strength);
        let ctx = p.ctx();
        // (Read first, set outside: the context's lock is not re-entrant.)
        let kept: Option<egui::TextureHandle> = ctx.data_mut(|d| d.get_temp(self.id));
        let handle = match kept {
            Some(mut handle) => {
                handle.set(image, egui::TextureOptions::LINEAR);
                handle
            }
            None => {
                let handle = ctx.load_texture("holo-bloom", image, egui::TextureOptions::LINEAR);
                ctx.data_mut(|d| d.insert_temp(self.id, handle.clone()));
                handle
            }
        };
        let mut mesh = Mesh::with_texture(handle.id());
        mesh.add_rect_with_uv(self.rect, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
        p.add(Shape::mesh(mesh));
    }
}

pub fn mix(from: Color32, to: Color32, t: f32) -> Color32 {
    let t = t.clamp(0.0, 1.0);
    let f = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgba_premultiplied(f(from.r(), to.r()), f(from.g(), to.g()), f(from.b(), to.b()), f(from.a(), to.a()))
}

// ------------------------------------------------------------------------------ geometry ----

pub fn ppp(p: &Painter) -> f32 {
    p.ctx().pixels_per_point()
}

/// One physical pixel, in points.
pub fn px(p: &Painter) -> f32 {
    1.0 / ppp(p)
}

/// `v` on the pixel grid.
pub fn snap(v: f32, ppp: f32) -> f32 {
    (v * ppp).round() / ppp
}

/// A point on a circle (radians, 0 = east, clockwise on screen).
pub fn polar(centre: Pos2, radius: f32, angle: f32) -> Pos2 {
    pos2(centre.x + radius * angle.cos(), centre.y + radius * angle.sin())
}

/// Twelve o'clock.
pub const NORTH: f32 = -std::f32::consts::FRAC_PI_2;

fn arc_steps(radius: f32, sweep: f32) -> usize {
    ((sweep.abs() * radius.max(6.0) / 3.5).ceil() as usize).clamp(4, 480)
}

pub fn arc_points(centre: Pos2, radius: f32, from: f32, to: f32) -> Vec<Pos2> {
    let n = arc_steps(radius, to - from);
    (0..=n).map(|i| polar(centre, radius, from + (to - from) * i as f32 / n as f32)).collect()
}

// --------------------------------------------------------------------------------- light ----

/// A batch of light: ribbons with soft edges, all in one mesh (a ring of a thousand ticks is
/// one draw call). Colours made with [`lit`] add up where ribbons cross.
pub struct Light {
    mesh: Mesh,
    feather: f32,
    /// Where this light spills, if it does ([`Light::glowing`]).
    bloom: Option<Bloom>,
    /// How strongly the bloom shows.
    pub spill: f32,
    warm: Color32,
}

impl Light {
    pub fn new(p: &Painter) -> Self {
        Self { mesh: Mesh::default(), feather: px(p), bloom: None, spill: 1.0, warm: theme().deep() }
    }

    /// Light with a bloom over `rect`: every ribbon and spark drawn also glows.
    pub fn glowing(p: &Painter, id: impl std::hash::Hash, rect: Rect, cells: usize) -> Self {
        Self { bloom: Some(Bloom::new(id, rect, cells)), ..Self::new(p) }
    }

    /// A band of glass between two lines, lit from one of them: a quad strip from `inner` to
    /// `outer` with a colour for each edge (the same number of points on both).
    pub fn sheet(&mut self, inner: &[Pos2], outer: &[Pos2], inner_colours: &[Color32], outer_colours: &[Color32]) {
        let n = inner.len().min(outer.len());
        let mut prev: Option<(u32, u32)> = None;
        for i in 0..n {
            let row = (self.vertex(inner[i], inner_colours[i.min(inner_colours.len() - 1)]), self.vertex(outer[i], outer_colours[i.min(outer_colours.len() - 1)]));
            if let Some(pr) = prev {
                self.quad(pr.0, pr.1, row.0, row.1);
            }
            prev = Some(row);
        }
    }

    fn vertex(&mut self, at: Pos2, colour: Color32) -> u32 {
        self.mesh.colored_vertex(at, colour);
        self.mesh.vertices.len() as u32 - 1
    }

    fn quad(&mut self, a: u32, b: u32, c: u32, d: u32) {
        self.mesh.add_triangle(a, b, c);
        self.mesh.add_triangle(b, d, c);
    }

    /// A ribbon through `points`, `width` wide, each point in its own colour.
    pub fn ribbon(&mut self, points: &[Pos2], colours: &[Color32], closed: bool, width: f32) {
        let n = points.len();
        if n < 2 {
            return;
        }
        let f = self.feather;
        // (Thinner than a pixel: a pixel wide and fainter, or it would flicker as it moves.)
        let (half, dim) = if width < f { (f / 2.0, (width / f).max(0.0)) } else { (width / 2.0, 1.0) };
        let dir = |i: usize, j: usize| {
            let d = points[j] - points[i];
            let len = d.length();
            if len > 1e-5 { d / len } else { Vec2::ZERO }
        };
        let mut first = 0;
        let mut last_row = None;
        let count = if closed { n + 1 } else { n };
        for k in 0..count {
            let i = k % n;
            let before = if i == 0 { if closed { dir(n - 1, 0) } else { dir(0, 1) } } else { dir(i - 1, i) };
            let after = if i + 1 == n { if closed { dir(n - 1, 0) } else { dir(n - 2, n - 1) } } else { dir(i, i + 1) };
            let tangent = (before + after).normalized();
            let tangent = if tangent == Vec2::ZERO { after } else { tangent };
            // A mitre keeps the width through a bend (capped: a hairpin would shoot out).
            let mitre = (1.0 / tangent.dot(after).abs().max(0.5)).min(2.0);
            let normal = vec2(-tangent.y, tangent.x) * mitre;
            let c = if dim < 1.0 { a(colours[i.min(colours.len() - 1)], dim) } else { colours[i.min(colours.len() - 1)] };
            let at = points[i];
            let row = [
                self.vertex(at - normal * (half + f), Color32::TRANSPARENT),
                self.vertex(at - normal * half, c),
                self.vertex(at + normal * half, c),
                self.vertex(at + normal * (half + f), Color32::TRANSPARENT),
            ];
            if k == 0 {
                first = row[0];
            }
            if let Some(prev) = last_row {
                let prev: [u32; 4] = prev;
                for s in 0..3 {
                    self.quad(prev[s], prev[s + 1], row[s], row[s + 1]);
                }
            }
            last_row = Some(row);
        }
        let _ = first;
        if let Some(bloom) = &mut self.bloom {
            // A wider line gives more light, a hairline its share.
            let gain = (width / f).clamp(0.5, 5.0);
            let colour = |i: usize| brightness(colours[i.min(colours.len() - 1)]);
            for i in 0..n - 1 {
                bloom.line(points[i], points[i + 1], (colour(i) + colour(i + 1)) / 2.0 * gain);
            }
            if closed {
                bloom.line(points[n - 1], points[0], (colour(n - 1) + colour(0)) / 2.0 * gain);
            }
        }
    }

    /// A ribbon of one colour.
    pub fn strip(&mut self, points: &[Pos2], closed: bool, width: f32, colour: Color32) {
        self.ribbon(points, &[colour], closed, width);
    }

    pub fn seg(&mut self, from: Pos2, to: Pos2, width: f32, colour: Color32) {
        self.strip(&[from, to], false, width, colour);
    }

    /// The glow under a line, for light that has no bloom map (a panel's rule, a key): brightest
    /// on the line, falling off through the deep tone, gone `reach` to either side.
    pub fn halo(&mut self, points: &[Pos2], closed: bool, reach: f32, colour: Color32) {
        let n = points.len();
        if n < 2 {
            return;
        }
        let k = brightness(colour);
        let mid = lit(mix(colour, self.warm, 0.7), 1.0).gamma_multiply(k * 0.30);
        let dir = |i: usize, j: usize| (points[j] - points[i]).normalized();
        let mut last_row: Option<[u32; 5]> = None;
        let count = if closed { n + 1 } else { n };
        for s in 0..count {
            let i = s % n;
            let before = if i == 0 { if closed { dir(n - 1, 0) } else { dir(0, 1) } } else { dir(i - 1, i) };
            let after = if i + 1 == n { if closed { dir(n - 1, 0) } else { dir(n - 2, n - 1) } } else { dir(i, i + 1) };
            let tangent = (before + after).normalized();
            let tangent = if tangent == Vec2::ZERO { after } else { tangent };
            let normal = vec2(-tangent.y, tangent.x);
            let at = points[i];
            let row = [
                self.vertex(at - normal * reach, Color32::TRANSPARENT),
                self.vertex(at - normal * reach * 0.36, mid),
                self.vertex(at, colour),
                self.vertex(at + normal * reach * 0.36, mid),
                self.vertex(at + normal * reach, Color32::TRANSPARENT),
            ];
            if let Some(prev) = last_row {
                for q in 0..4 {
                    self.quad(prev[q], prev[q + 1], row[q], row[q + 1]);
                }
            }
            last_row = Some(row);
        }
    }

    /// A line of light: a bright filament with a glow under it (`glow` 0-1 scales the glow).
    /// With a bloom map the map is the glow.
    pub fn neon(&mut self, points: &[Pos2], closed: bool, width: f32, colour: Color32, glow: f32) {
        if glow > 0.0 && self.bloom.is_none() {
            self.halo(points, closed, width * 2.0 + 7.0, lit(colour, 0.42 * glow));
        }
        self.strip(points, closed, width, colour);
    }

    pub fn arc(&mut self, centre: Pos2, radius: f32, from: f32, to: f32, width: f32, colour: Color32) {
        let full = (to - from).abs() >= TAU - 1e-4;
        let mut points = arc_points(centre, radius, from, to);
        if full {
            points.pop();
        }
        self.strip(&points, full, width, colour);
    }

    pub fn neon_arc(&mut self, centre: Pos2, radius: f32, from: f32, to: f32, width: f32, colour: Color32, glow: f32) {
        let full = (to - from).abs() >= TAU - 1e-4;
        let mut points = arc_points(centre, radius, from, to);
        if full {
            points.pop();
        }
        self.neon(&points, full, width, colour, glow);
    }

    pub fn ring(&mut self, centre: Pos2, radius: f32, width: f32, colour: Color32) {
        self.arc(centre, radius, 0.0, TAU, width, colour);
    }

    /// A filled piece of a ring between two radii (a band, a segment key).
    pub fn band(&mut self, centre: Pos2, inner: f32, outer: f32, from: f32, to: f32, colour: Color32) {
        self.arc(centre, (inner + outer) / 2.0, from, to, (outer - inner).abs(), colour);
    }

    /// `count` ticks on a circle from `from` to `to`, each from radius `inner` to `outer`.
    pub fn ticks(&mut self, centre: Pos2, inner: f32, outer: f32, from: f32, to: f32, count: usize, width: f32, colour: Color32) {
        let full = (to - from).abs() >= TAU - 1e-4;
        let div = if full { count } else { count.saturating_sub(1).max(1) };
        for i in 0..count {
            let angle = from + (to - from) * i as f32 / div as f32;
            self.seg(polar(centre, inner, angle), polar(centre, outer, angle), width, colour);
        }
    }

    /// An arc in dashes: `dash` and `gap` in radians.
    pub fn dashes(&mut self, centre: Pos2, radius: f32, from: f32, to: f32, dash: f32, gap: f32, width: f32, colour: Color32) {
        let step = (dash + gap).max(1e-3);
        let dir = (to - from).signum();
        let mut at = 0.0;
        let sweep = (to - from).abs();
        while at < sweep {
            let end = (at + dash).min(sweep);
            self.arc(centre, radius, from + dir * at, from + dir * end, width, colour);
            at += step;
        }
    }

    /// A soft point of light.
    pub fn spark(&mut self, centre: Pos2, radius: f32, colour: Color32) {
        let mid = self.vertex(centre, colour);
        let n = 10;
        let rim: Vec<u32> = (0..n).map(|i| self.vertex(polar(centre, radius, TAU * i as f32 / n as f32), Color32::TRANSPARENT)).collect();
        for i in 0..n {
            self.mesh.add_triangle(mid, rim[i], rim[(i + 1) % n]);
        }
        if let Some(bloom) = &mut self.bloom {
            bloom.point(centre, brightness(colour) * (radius * bloom.per_point).powi(2).clamp(0.3, 6.0));
        }
    }

    /// Light for the bloom alone: the air glows here, nothing sharp is drawn.
    pub fn haze(&mut self, centre: Pos2, energy: f32) {
        if let Some(bloom) = &mut self.bloom {
            bloom.point(centre, energy);
        }
    }

    pub fn paint(self, p: &Painter) {
        if let Some(bloom) = self.bloom {
            bloom.paint(p, self.spill);
        }
        if !self.mesh.indices.is_empty() {
            p.add(Shape::mesh(self.mesh));
        }
    }
}

// ----------------------------------------------------------------------------- structure ----

/// A hairline between two points: one physical pixel, on the grid when it runs straight.
pub fn rule(p: &Painter, from: Pos2, to: Pos2, colour: Color32) {
    let (k, w) = (ppp(p), px(p));
    let centre = |v: f32| ((v * k).floor() + 0.5) / k;
    let (from, to) = if (from.y - to.y).abs() < 1e-3 {
        (pos2(snap(from.x, k), centre(from.y)), pos2(snap(to.x, k), centre(to.y)))
    } else if (from.x - to.x).abs() < 1e-3 {
        (pos2(centre(from.x), snap(from.y, k)), pos2(centre(to.x), snap(to.y, k)))
    } else {
        (from, to)
    };
    p.line_segment([from, to], Stroke::new(w, colour));
}

/// A rectangle with corners cut at 45°: `cut` is [top-left, top-right, bottom-right,
/// bottom-left] in points. Fill and outline share these points, so a cut corner is cut in both.
pub fn plate(r: Rect, cut: [f32; 4]) -> Vec<Pos2> {
    let m = r.width().min(r.height()) / 2.0;
    let [tl, tr, br, bl] = cut.map(|c| c.clamp(0.0, m));
    let mut pts = Vec::with_capacity(8);
    let mut corner = |c: f32, a: Pos2, b: Pos2, at: Pos2| {
        if c > 0.0 {
            pts.push(a);
            pts.push(b);
        } else {
            pts.push(at);
        }
    };
    corner(tl, pos2(r.left(), r.top() + tl), pos2(r.left() + tl, r.top()), r.left_top());
    corner(tr, pos2(r.right() - tr, r.top()), pos2(r.right(), r.top() + tr), r.right_top());
    corner(br, pos2(r.right(), r.bottom() - br), pos2(r.right() - br, r.bottom()), r.right_bottom());
    corner(bl, pos2(r.left() + bl, r.bottom()), pos2(r.left(), r.bottom() - bl), r.left_bottom());
    pts
}

/// `points` on the pixel grid: each on the centre of a pixel, and none past the line's own
/// right or bottom bound, so a frame flush with the screen's edge keeps all four sides.
fn on_grid(p: &Painter, points: &[Pos2]) -> Vec<Pos2> {
    let k = ppp(p);
    let (right, bottom) = points.iter().fold((f32::MIN, f32::MIN), |(x, y), q| (x.max(q.x), y.max(q.y)));
    let centre = |v: f32, most: f32| ((v * k).floor() + 0.5).min((most * k).round() - 0.5) / k;
    points.iter().map(|q| pos2(centre(q.x, right), centre(q.y, bottom))).collect()
}

pub fn fill(p: &Painter, points: &[Pos2], colour: Color32) {
    p.add(Shape::convex_polygon(points.to_vec(), colour, Stroke::NONE));
}

/// A closed hairline through `points`.
pub fn outline(p: &Painter, points: &[Pos2], colour: Color32) {
    p.add(Shape::closed_line(on_grid(p, points), Stroke::new(px(p), colour)));
}

/// An open hairline through `points`.
pub fn path(p: &Painter, points: &[Pos2], colour: Color32) {
    p.add(Shape::line(on_grid(p, points), Stroke::new(px(p), colour)));
}

/// Corner ticks: short bright strokes that mark a frame's corners, `arm` long.
pub fn corner_ticks(p: &Painter, r: Rect, arm: f32, colour: Color32) {
    for (at, dx, dy) in [(r.left_top(), 1.0, 1.0), (r.right_top(), -1.0, 1.0), (r.right_bottom(), -1.0, -1.0), (r.left_bottom(), 1.0, -1.0)] {
        path(p, &[pos2(at.x + dx * arm, at.y), at, pos2(at.x, at.y + dy * arm)], colour);
    }
}

/// A field of dots (the drafting grid, a texture behind a reading).
pub fn dot_field(p: &Painter, r: Rect, pitch: f32, origin: Pos2, colour: Color32) {
    let (k, w) = (ppp(p), px(p));
    let mut mesh = Mesh::default();
    let start = |lo: f32, o: f32| o + ((lo - o) / pitch).ceil() * pitch;
    let mut y = start(r.top(), origin.y);
    while y <= r.bottom() {
        let mut x = start(r.left(), origin.x);
        while x <= r.right() {
            let at = pos2(snap(x, k), snap(y, k));
            mesh.add_colored_rect(Rect::from_min_size(at, vec2(w, w)), colour);
            x += pitch;
        }
        y += pitch;
    }
    p.add(Shape::mesh(mesh));
}

/// An empty slot: a box with a cross through it (a picture not loaded, a cell with no data).
pub fn slot(p: &Painter, r: Rect, colour: Color32) {
    outline(p, &plate(r, [0.0; 4]), colour);
    p.line_segment([r.left_top(), r.right_bottom()], Stroke::new(px(p), a(colour, 0.6)));
    p.line_segment([r.right_top(), r.left_bottom()], Stroke::new(px(p), a(colour, 0.6)));
}

/// A leader: from the thing (`from`, marked with a dot) at 45° and then level to its label.
pub fn leader(p: &Painter, from: Pos2, to: Pos2, colour: Color32) {
    let dy = to.y - from.y;
    let dx = (to.x - from.x).signum() * dy.abs().min((to.x - from.x).abs());
    let knee = pos2(from.x + dx, to.y);
    p.add(Shape::line(vec![from, knee, to], Stroke::new(px(p), colour)));
    p.circle_filled(from, 1.6, colour);
}

/// Every line of the HUD: a hairline stays one, anything wider is drawn finer (2 → 1.4,
/// 6 → 3). (`Stroke::new` takes `impl Into<f32>`, which leaves float literals untyped.)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plate_cuts_only_the_corners_asked_for() {
        let r = Rect::from_min_size(pos2(10.0, 20.0), vec2(200.0, 100.0));
        assert_eq!(plate(r, [0.0; 4]), vec![r.left_top(), r.right_top(), r.right_bottom(), r.left_bottom()]);
        let cut = plate(r, [0.0, 0.0, 14.0, 0.0]);
        assert_eq!(cut.len(), 5);
        assert!(cut.contains(&pos2(r.right(), r.bottom() - 14.0)) && cut.contains(&pos2(r.right() - 14.0, r.bottom())));
        // A cut never passes the middle of the shorter side.
        let all = plate(r, [500.0; 4]);
        assert!(all.iter().all(|q| r.expand(0.01).contains(*q)));
    }

    /// Light that adds has no alpha, and a tenth of it is white: a pure yellow has a little
    /// blue in it, so where lines pile up they burn out to white instead of to flat yellow.
    #[test]
    fn light_that_adds_carries_no_alpha_and_whitens_as_it_piles_up() {
        let c = lit(Color32::from_rgb(255, 212, 0), 0.5);
        assert_eq!((c.r(), c.g(), c.b(), c.a()), (127, 108, 12, 0));
        assert_eq!(lit(Color32::from_rgb(255, 212, 0), 0.0), Color32::from_rgba_premultiplied(0, 0, 0, 0));
        // Dim light is the deep tone, bright light the accent, the brightest nearly white.
        let t = crate::holo_theme::preset("stark_gold").unwrap();
        let (dim, mid, bright) = (glow_colour(&t, 0.12), glow_colour(&t, 0.5), glow_colour(&t, 1.0));
        assert!(dim[1] / dim[0] < mid[1] / mid[0], "a faint glow is more orange than a strong one: {dim:?} {mid:?}");
        assert!(bright[2] > 0.6 && mid[2] < 0.1, "only the brightest light is white: {mid:?} {bright:?}");
    }

    #[test]
    fn a_ribbon_has_soft_edges_and_a_hairline_keeps_a_pixel() {
        let ctx = egui::Context::default();
        let mut out = None;
        let _ = ctx.run(Default::default(), |ctx| {
            let p = ctx.layer_painter(egui::LayerId::background());
            let mut light = Light::new(&p);
            light.seg(pos2(0.0, 0.0), pos2(100.0, 0.0), 0.25, Color32::WHITE);
            out = Some(light.mesh);
        });
        let mesh = out.unwrap();
        // Two rows of four: transparent, lit, lit, transparent.
        assert_eq!(mesh.vertices.len(), 8);
        assert_eq!(mesh.vertices[0].color, Color32::TRANSPARENT);
        assert!(mesh.vertices[1].color.a() < 255 && mesh.vertices[1].color.a() > 0, "a thinner line is a fainter pixel");
        assert!((mesh.vertices[1].pos.y - mesh.vertices[2].pos.y).abs() >= 0.99, "never thinner than a pixel");
    }
}
