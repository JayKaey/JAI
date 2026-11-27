//! Offscreen snapshots of the HUD for tests: real egui frames (the same painters the window uses)
//! tessellated by epaint and rasterised on the CPU into PNGs under `target/ui-snapshots/`, so the
//! layout, clipping and the emblem / sphere / block rendering can be checked without opening a
//! window. Triangles are filled at pixel centres with interpolated colours and bilinear texture
//! samples (the font atlas), blended premultiplied in gamma space like egui's own renderer.

use eframe::egui::{self, epaint, pos2, Color32, Rect, TextureId};
use std::collections::HashMap;
#[cfg(test)]
use std::path::PathBuf;

/// An RGBA canvas (premultiplied, 0-1).
pub struct Canvas {
    pub width: usize,
    pub height: usize,
    pixels: Vec<[f32; 4]>,
}

struct Texture {
    width: usize,
    height: usize,
    texels: Vec<[f32; 4]>,
}

impl Texture {
    fn sample(&self, u: f32, v: f32) -> [f32; 4] {
        // A coordinate past the texture repeats it (the grain's tile); the font atlas and
        // pictures never leave 0-1.
        let wrap = |t: f32| if (0.0..=1.0).contains(&t) { t } else { t.rem_euclid(1.0) };
        let (u, v) = (wrap(u), wrap(v));
        let x = (u * self.width as f32 - 0.5).clamp(0.0, (self.width - 1) as f32);
        let y = (v * self.height as f32 - 0.5).clamp(0.0, (self.height - 1) as f32);
        let (x0, y0) = (x.floor() as usize, y.floor() as usize);
        let (x1, y1) = ((x0 + 1).min(self.width - 1), (y0 + 1).min(self.height - 1));
        let (fx, fy) = (x - x0 as f32, y - y0 as f32);
        let at = |x: usize, y: usize| self.texels[y * self.width + x];
        let mut out = [0.0; 4];
        for (c, o) in out.iter_mut().enumerate() {
            let top = at(x0, y0)[c] * (1.0 - fx) + at(x1, y0)[c] * fx;
            let bottom = at(x0, y1)[c] * (1.0 - fx) + at(x1, y1)[c] * fx;
            *o = top * (1.0 - fy) + bottom * fy;
        }
        out
    }
}

fn unit(c: Color32) -> [f32; 4] {
    [c.r() as f32 / 255.0, c.g() as f32 / 255.0, c.b() as f32 / 255.0, c.a() as f32 / 255.0]
}

/// Renders egui frames offscreen.
pub struct Snapshot {
    pub ctx: egui::Context,
    textures: HashMap<TextureId, Texture>,
    size: egui::Vec2,
    /// What the canvas is before anything is drawn (premultiplied): black, or nothing at all
    /// for a surface the desktop shows through (J.A.I.OS's shell).
    pub clear: [f32; 4],
}

impl Snapshot {
    /// A context the size of `size` points (1 px per point) with the HUD's fonts.
    pub fn new(size: egui::Vec2) -> Self {
        let ctx = egui::Context::default();
        crate::holo::install_fonts(&ctx);
        // Measured as it rests: no window entrance or exit moves what is checked.
        ctx.data_mut(|d| d.insert_temp(egui::Id::new(crate::gui::STILL_WINDOWS), true));
        Self { ctx, textures: HashMap::new(), size, clear: [0.0, 0.0, 0.0, 1.0] }
    }

    fn apply(&mut self, delta: &egui::TexturesDelta) {
        for (id, d) in &delta.set {
            let (w, h, texels): (usize, usize, Vec<[f32; 4]>) = match &d.image {
                epaint::ImageData::Color(img) => (img.size[0], img.size[1], img.pixels.iter().map(|c| unit(*c)).collect()),
                epaint::ImageData::Font(img) => (img.size[0], img.size[1], img.srgba_pixels(None).map(unit).collect()),
            };
            match d.pos {
                None => {
                    self.textures.insert(*id, Texture { width: w, height: h, texels });
                }
                Some([px, py]) => {
                    if let Some(t) = self.textures.get_mut(id) {
                        for y in 0..h {
                            for x in 0..w {
                                if px + x < t.width && py + y < t.height {
                                    t.texels[(py + y) * t.width + px + x] = texels[y * w + x];
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    /// The screen this context draws.
    pub fn screen(&self) -> Rect {
        Rect::from_min_size(pos2(0.0, 0.0), self.size)
    }

    /// One frame of `ui` with this input; the font and image textures it loaded are kept for
    /// rasterising.
    pub fn frame(&mut self, input: egui::RawInput, ui: impl FnMut(&egui::Context)) -> egui::FullOutput {
        let output = self.ctx.run(input, ui);
        self.apply(&output.textures_delta);
        output
    }

    /// Runs `frames` frames of `ui` (fonts and animations settle) and rasterises the last one.
    #[cfg(test)]
    pub fn render(&mut self, frames: usize, ui: impl FnMut(&egui::Context)) -> Canvas {
        self.render_scaled(frames, 1.0, ui)
    }

    /// `render` at `ppp` physical pixels per point (Windows' 125 % is 1.25): the picture is
    /// that many times larger.
    #[cfg(test)]
    pub fn render_scaled(&mut self, frames: usize, ppp: f32, mut ui: impl FnMut(&egui::Context)) -> Canvas {
        self.ctx.set_pixels_per_point(ppp);
        let mut last = None;
        for k in 0..frames.max(1) {
            let input = egui::RawInput { screen_rect: Some(self.screen()), time: Some(k as f64 / 60.0), predicted_dt: 1.0 / 60.0, ..Default::default() };
            last = Some(self.frame(input, &mut ui));
        }
        let output = last.expect("one frame");
        self.rasterise(output.shapes, output.pixels_per_point)
    }

    /// Tessellates a frame's shapes and fills the triangles into a canvas of this size (in
    /// physical pixels: points times `pixels_per_point`), on all of the processor's threads.
    pub fn rasterise(&self, shapes: Vec<epaint::ClippedShape>, pixels_per_point: f32) -> Canvas {
        self.rasterise_on(shapes, pixels_per_point, std::thread::available_parallelism().map_or(1, |n| n.get()))
    }

    /// `rasterise` on up to `threads` threads, each filling a band of the rows with every mesh in
    /// turn: a pixel is blended just as on one thread. (J.A.I.OS's shell draws all it shows this
    /// way, on the thread that also takes Sir's clicks.)
    fn rasterise_on(&self, shapes: Vec<epaint::ClippedShape>, ppp: f32, threads: usize) -> Canvas {
        let primitives = self.ctx.tessellate(shapes, ppp);
        let (w, h) = ((self.size.x * ppp).round() as usize, (self.size.y * ppp).round() as usize);
        // (epaint hands points; a GPU backend scales them to pixels, so does this.)
        let meshes: Vec<(std::borrow::Cow<epaint::Mesh>, Rect)> = primitives
            .iter()
            .filter_map(|p| match &p.primitive {
                epaint::Primitive::Mesh(mesh) if ppp == 1.0 => Some((std::borrow::Cow::Borrowed(mesh), p.clip_rect)),
                epaint::Primitive::Mesh(mesh) => {
                    let mut mesh = mesh.clone();
                    mesh.vertices.iter_mut().for_each(|v| v.pos = pos2(v.pos.x * ppp, v.pos.y * ppp));
                    Some((std::borrow::Cow::Owned(mesh), Rect::from_min_max(pos2(p.clip_rect.min.x * ppp, p.clip_rect.min.y * ppp), pos2(p.clip_rect.max.x * ppp, p.clip_rect.max.y * ppp))))
                }
                _ => None,
            })
            .collect();
        let mut canvas = Canvas { width: w, height: h, pixels: vec![self.clear; w * h] };
        // (A band of at least 32 rows: a thread costs more than a few rows.)
        let rows = h.div_ceil(threads.clamp(1, (h / 32).max(1))).max(1);
        std::thread::scope(|s| {
            for (i, band) in canvas.pixels.chunks_mut(rows * w.max(1)).enumerate() {
                let (meshes, textures) = (&meshes, &self.textures);
                s.spawn(move || meshes.iter().for_each(|(mesh, clip)| fill(band, w, i * rows, mesh, *clip, textures.get(&mesh.texture_id))));
            }
        });
        canvas
    }
}

/// Fills a mesh's triangles into `band`: the canvas's rows from `top` on, `width` pixels each.
fn fill(band: &mut [[f32; 4]], width: usize, top: usize, mesh: &epaint::Mesh, clip: Rect, texture: Option<&Texture>) {
    let bottom = top + band.len() / width.max(1);
    let clip = clip.intersect(Rect::from_min_max(pos2(0.0, top as f32), pos2(width as f32, bottom as f32)));
    for tri in mesh.indices.chunks_exact(3) {
        let v = [&mesh.vertices[tri[0] as usize], &mesh.vertices[tri[1] as usize], &mesh.vertices[tri[2] as usize]];
        let area = (v[1].pos.x - v[0].pos.x) * (v[2].pos.y - v[0].pos.y) - (v[2].pos.x - v[0].pos.x) * (v[1].pos.y - v[0].pos.y);
        if area.abs() < 1e-6 {
            continue;
        }
        let min_x = v.iter().map(|p| p.pos.x).fold(f32::MAX, f32::min).max(clip.left()).floor() as i32;
        let max_x = v.iter().map(|p| p.pos.x).fold(f32::MIN, f32::max).min(clip.right()).ceil() as i32;
        let min_y = v.iter().map(|p| p.pos.y).fold(f32::MAX, f32::min).max(clip.top()).floor() as i32;
        let max_y = v.iter().map(|p| p.pos.y).fold(f32::MIN, f32::max).min(clip.bottom()).ceil() as i32;
        let colours = [unit(v[0].color), unit(v[1].color), unit(v[2].color)];
        // (A shape of one colour takes the atlas's white texel at all three corners: it is
        // sampled once, not at every pixel.)
        let flat = texture.filter(|_| v[0].uv == v[1].uv && v[1].uv == v[2].uv).map(|t| t.sample(v[0].uv.x, v[0].uv.y));
        for y in min_y.max(top as i32)..max_y.min(bottom as i32) {
            for x in Canvas::span(&v, area, y as f32 + 0.5007, min_x.max(0), max_x.min(width as i32)) {
                // (Sampled a hair off the pixel's centre and without slack: a pixel on the edge
                // two triangles share is then covered once. Covered twice, light that adds
                // up shows every such edge as a bright seam.)
                let (px, py) = (x as f32 + 0.5003, y as f32 + 0.5007);
                if px < clip.left() || px > clip.right() || py < clip.top() || py > clip.bottom() {
                    continue;
                }
                let [w0, w1, w2] = Canvas::weights(&v, area, px, py);
                if w0 < 0.0 || w1 < 0.0 || w2 < 0.0 {
                    continue;
                }
                let mut src = [0.0f32; 4];
                for (c, s) in src.iter_mut().enumerate() {
                    *s = colours[0][c] * w0 + colours[1][c] * w1 + colours[2][c] * w2;
                }
                if let Some(tex) = flat.or_else(|| texture.map(|t| t.sample(v[0].uv.x * w0 + v[1].uv.x * w1 + v[2].uv.x * w2, v[0].uv.y * w0 + v[1].uv.y * w1 + v[2].uv.y * w2))) {
                    for c in 0..4 {
                        src[c] *= tex[c];
                    }
                }
                let dst = &mut band[(y as usize - top) * width + x as usize];
                for c in 0..4 {
                    dst[c] = (src[c] + dst[c] * (1.0 - src[3])).clamp(0.0, 1.0);
                }
            }
        }
    }
}

impl Canvas {
    /// The weights of the triangle's three corners at a sample point (all ≥ 0 inside it).
    fn weights(v: &[&epaint::Vertex; 3], area: f32, px: f32, py: f32) -> [f32; 3] {
        let w0 = ((v[1].pos.x - px) * (v[2].pos.y - py) - (v[2].pos.x - px) * (v[1].pos.y - py)) / area;
        let w1 = ((v[2].pos.x - px) * (v[0].pos.y - py) - (v[0].pos.x - px) * (v[2].pos.y - py)) / area;
        [w0, w1, 1.0 - w0 - w1]
    }

    /// The pixels of the row sampled at `py` that may lie in the triangle: `from..to` cut to where
    /// each of its weights can be positive, two pixels to spare for rounding (the test of each
    /// pixel still decides). A thin diagonal triangle's box is mostly empty, and J.A.I.OS's shell
    /// draws every picture this way.
    fn span(v: &[&epaint::Vertex; 3], area: f32, py: f32, from: i32, to: i32) -> std::ops::Range<i32> {
        let (mut lo, mut hi) = (f64::from(from), f64::from(to));
        let (sign, py) = (f64::from(area.signum()), f64::from(py));
        for (a, b) in [(1, 2), (2, 0), (0, 1)] {
            let ([ax, ay], [bx, by]) = ([v[a].pos.x, v[a].pos.y].map(f64::from), [v[b].pos.x, v[b].pos.y].map(f64::from));
            // (The weight opposite the edge a→b, times the area, is k·px + c along the row.)
            let (k, c) = ((ay - by) * sign, (ax * (by - py) - bx * (ay - py)) * sign);
            // (An edge less than a pixel high bounds no stretch that its rounding couldn't move.)
            if k >= 1.0 {
                lo = lo.max(-c / k - 0.5003);
            } else if k <= -1.0 {
                hi = hi.min(-c / k - 0.5003 + 1.0);
            }
        }
        (lo.floor() as i32 - 2).max(from)..(hi.ceil() as i32 + 2).min(to)
    }

    pub fn pixel(&self, x: usize, y: usize) -> [u8; 3] {
        let p = self.pixels[y * self.width + x];
        [(p[0] * 255.0) as u8, (p[1] * 255.0) as u8, (p[2] * 255.0) as u8]
    }

    /// The picture as a Wayland buffer takes it (ARGB8888: blue, green, red, alpha in memory,
    /// premultiplied). Light added where nothing covers shows as light: its alpha is raised to
    /// its brightest channel, so no channel exceeds it.
    #[cfg(target_os = "linux")]
    pub fn write_argb(&self, out: &mut [u8]) {
        for (p, o) in self.pixels.iter().zip(out.chunks_exact_mut(4)) {
            let alpha = p[3].max(p[0]).max(p[1]).max(p[2]);
            o.copy_from_slice(&[(p[2] * 255.0) as u8, (p[1] * 255.0) as u8, (p[0] * 255.0) as u8, (alpha * 255.0) as u8]);
        }
    }

    /// Mean brightness (0-255) of a region.
    #[cfg(test)]
    pub fn brightness(&self, r: Rect) -> f32 {
        let (x0, y0) = (r.left().max(0.0) as usize, r.top().max(0.0) as usize);
        let (x1, y1) = ((r.right() as usize).min(self.width), (r.bottom() as usize).min(self.height));
        let mut sum = 0.0;
        let mut n = 0;
        for y in y0..y1 {
            for x in x0..x1 {
                let p = self.pixel(x, y);
                sum += (p[0] as f32 + p[1] as f32 + p[2] as f32) / 3.0;
                n += 1;
            }
        }
        if n == 0 { 0.0 } else { sum / n as f32 }
    }

    /// Writes the picture to `target/ui-snapshots/` (in the folder of its colour set when it was
    /// taken in the look's colours: `holo_theme::look_for_pictures`).
    #[cfg(test)]
    pub fn save(&self, name: &str) -> PathBuf {
        let dir = PathBuf::from("target").join("ui-snapshots").join(crate::holo_theme::picture_set().unwrap_or(""));
        let _ = std::fs::create_dir_all(&dir);
        let mut img = image::RgbaImage::new(self.width as u32, self.height as u32);
        for (i, p) in img.pixels_mut().enumerate() {
            let [r, g, b] = self.pixel(i % self.width, i / self.width);
            *p = image::Rgba([r, g, b, 255]);
        }
        let path = dir.join(format!("{name}.png"));
        img.save(&path).expect("writing the snapshot");
        path
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use eframe::egui::vec2;
    use crate::hud_board::{self, BlockKind};
    use crate::holo_core::{Core, Pose};
    use std::time::Instant;

    const W: f32 = 1280.0;
    const H: f32 = 760.0;

    fn screen() -> Rect {
        Rect::from_min_size(pos2(0.0, 0.0), vec2(W, H))
    }

    /// Drawn on several threads, a picture is the one a single thread draws, pixel for pixel
    /// (bands of uneven heights; the core, glow and lettering in it).
    #[test]
    fn a_picture_drawn_on_many_threads_is_the_one_drawn_on_one() {
        let mut emblem = Core::new(Pose::Gyro);
        for _ in 0..20 {
            emblem.tick(1.0 / 60.0, 0.4, true);
        }
        let mut snap = Snapshot::new(vec2(W, H));
        let mut last = None;
        for k in 0..2 {
            let input = egui::RawInput { screen_rect: Some(snap.screen()), time: Some(k as f64 / 60.0), ..Default::default() };
            last = Some(snap.frame(input, |ctx| {
                crate::gui::paint_canvas(ctx);
                egui::CentralPanel::default().frame(egui::Frame::none()).show(ctx, |ui| {
                    emblem.paint(&ui.painter_at(screen()), screen());
                    ui.label("J.A.I. 01 // SYSTEMS NOMINAL");
                });
            }));
        }
        let shapes = last.expect("a frame").shapes;
        let (one, many) = (snap.rasterise_on(shapes.clone(), 1.0, 1), snap.rasterise_on(shapes, 1.0, 7));
        assert_eq!((one.width, one.height), (many.width, many.height));
        assert!(one.brightness(screen()) > 1.0, "something is drawn");
        assert!(one.pixels == many.pixels, "the bands' picture differs from one thread's");
    }

    /// A row's span holds every pixel the triangle covers (big, small and long thin triangles,
    /// both windings, up to a 5K screen), and a thin diagonal one is visited in a sliver of its box.
    #[test]
    fn a_rows_span_holds_every_pixel_the_triangle_covers() {
        let mut seed = 12345u64;
        let mut next = || {
            seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
            (seed >> 40) as f32 / (1u64 << 24) as f32
        };
        let corner = |x: f32, y: f32| epaint::Vertex { pos: pos2(x, y), uv: pos2(0.0, 0.0), color: Color32::WHITE };
        let mut triangles: Vec<[epaint::Vertex; 3]> = vec![[corner(10.0, 10.0), corner(1810.0, 1210.0), corner(1811.5, 1209.0)], [corner(0.0, 0.0), corner(5120.0, 2880.0), corner(0.0, 2880.0)], [corner(3.0, 500.0), corner(5000.0, 503.5), corner(2400.0, 502.0)]];
        for n in 0..1500 {
            let reach = if n % 100 == 0 { 3000.0 } else { [3.0, 80.0, 600.0][n % 3] };
            let (x, y) = (next() * 5120.0, next() * 2880.0);
            triangles.push([(); 3].map(|_| corner(x + (next() - 0.5) * reach, y + (next() - 0.5) * reach)));
        }
        let (mut visited, mut boxed) = (0usize, 0usize);
        for (n, t) in triangles.iter().enumerate() {
            let v = [&t[0], &t[1], &t[2]];
            let area = (v[1].pos.x - v[0].pos.x) * (v[2].pos.y - v[0].pos.y) - (v[2].pos.x - v[0].pos.x) * (v[1].pos.y - v[0].pos.y);
            if area.abs() < 1e-6 {
                continue;
            }
            let (min_x, max_x) = (v.iter().map(|p| p.pos.x).fold(f32::MAX, f32::min).floor() as i32, v.iter().map(|p| p.pos.x).fold(f32::MIN, f32::max).ceil() as i32);
            let (min_y, max_y) = (v.iter().map(|p| p.pos.y).fold(f32::MAX, f32::min).floor() as i32, v.iter().map(|p| p.pos.y).fold(f32::MIN, f32::max).ceil() as i32);
            for y in min_y..max_y {
                let py = y as f32 + 0.5007;
                let span = Canvas::span(&v, area, py, min_x, max_x);
                for x in min_x..max_x {
                    if Canvas::weights(&v, area, x as f32 + 0.5003, py).iter().all(|w| *w >= 0.0) {
                        assert!(span.contains(&x), "triangle {n} {:?}: pixel {x},{y} covered, outside {span:?}", [t[0].pos, t[1].pos, t[2].pos]);
                    }
                }
                if n == 0 {
                    (visited, boxed) = (visited + span.len(), boxed + (max_x - min_x) as usize);
                }
            }
        }
        assert!(visited * 20 < boxed, "the thin diagonal: {visited} of its box's {boxed} pixels visited");
    }

    /// The emblem in each state, on the obsidian canvas.
    #[test]
    fn snapshot_emblem_states() {
        let _look = crate::holo_theme::look_for_pictures();
        for (state, name, steps) in [(Pose::Front, "emblem_front", 1), (Pose::Gyro, "emblem_gyroscope", 40), (Pose::Docked, "emblem_docked", 1)] {
            let mut emblem = Core::new(state);
            for _ in 0..steps {
                emblem.tick(1.0 / 60.0, 0.4, state == Pose::Gyro);
            }
            let mut snap = Snapshot::new(vec2(W, H));
            let canvas = snap.render(2, |ctx| {
                crate::gui::paint_canvas(ctx);
                egui::CentralPanel::default().frame(egui::Frame::none()).show(ctx, |ui| emblem.paint(&ui.painter_at(screen()), screen()));
            });
            canvas.save(name);
            let f = emblem.frame(screen());
            let around = Rect::from_center_size(f.centre, vec2(f.radius * 2.0, f.radius * 2.0));
            let corner = Rect::from_min_size(pos2(10.0, 10.0), vec2(120.0, 120.0));
            assert!(canvas.brightness(around) > canvas.brightness(corner) + 4.0, "{name}: the emblem is drawn");
        }
    }

    /// The display's finish over the emblem: scan lines, grain and the glass (uneven light,
    /// darker edges, dust, hairline scratches). It darkens the corners and leaves the middle.
    #[test]
    fn snapshot_display_finish() {
        let _look = crate::holo_theme::look_for_pictures();
        let emblem = Core::new(Pose::Front);
        let paint = |finish: bool| {
            let mut snap = Snapshot::new(vec2(W, H));
            snap.render(3, |ctx| {
                crate::gui::paint_canvas(ctx);
                egui::CentralPanel::default().frame(egui::Frame::none()).show(ctx, |ui| emblem.paint(&ui.painter_at(screen()), screen()));
                if finish {
                    crate::gui::paint_finish(ctx);
                }
            })
        };
        let (bare, finished) = (paint(false), paint(true));
        finished.save("display_finish");
        let corner = Rect::from_min_size(pos2(0.0, 0.0), vec2(90.0, 60.0));
        let beside = Rect::from_center_size(screen().center() - vec2(260.0, 0.0), vec2(90.0, 60.0));
        let middle = Rect::from_center_size(screen().center(), vec2(200.0, 200.0));
        assert!(finished.brightness(corner) < finished.brightness(beside), "the corners fall off");
        assert!(finished.brightness(middle) > bare.brightness(middle) * 0.8, "the middle keeps its light");
    }

    fn sample_report() -> hud_board::Report {
        use hud_board::{Bar, Block, Note};
        let note = |h: &str, b: &str| Note { headline: h.into(), body: b.into() };
        hud_board::Report {
            topic: "state of VR".into(),
            created_at: "2026-11-02 18:00:00".into(),
            blocks: vec![
                Block::new("CHART // HEADSET SHARE ON PC — STEAM HARDWARE SURVEY, OCTOBER 2026", "SOURCE: STEAM", "SOURCE: STEAM HARDWARE SURVEY", BlockKind::BarChart {
                    unit: "%".into(),
                    bars: vec![Bar { label: "Quest 2".into(), value: 38.2 }, Bar { label: "Quest 3".into(), value: 24.1 }, Bar { label: "Valve Index".into(), value: 11.0 }, Bar { label: "Pico 4 Ultra Enterprise".into(), value: 4.4 }],
                    caption: "Share of VR headsets used on Steam; the long label checks the axis never overflows.".into(),
                }),
                Block::new("NOTE // 01 — HARDWARE & PRICING", "FACT", "SOURCE: META", BlockKind::NoteStack { notes: vec![note("Quest 3S costs 299 dollars and ships with the Snapdragon XR2 Gen 2 and pancake-free fresnel lenses", "A deliberately long body text that has to wrap inside the block and be cut off with an ellipsis instead of running out of the frame. It keeps going for a while so the clipping is visible in the snapshot."), note("Index is discontinued", "Valve stopped production.")] }),
                Block::new("FIGURE // 2026", "FACT", "SOURCE: IDC", BlockKind::HeroFigure { header: "Headsets shipped worldwide in 2026 (estimate)".into(), figure: "≈20m".into(), note: "IDC estimate, consumer and enterprise combined, all form factors.".into() }),
                Block::new("VIDEO // STEAM FRAME HANDS-ON", "SOURCE: YOUTUBE", "", BlockKind::MediaCard { images: hud_board::video_frames("dQw4w9WgXcQ"), caption: "Hands-on with the new headset at the event · 12:34".into() }),
                Block::new("MAP // TULLEYS FARM, CRAWLEY", "", "SOURCE: OSM", BlockKind::DarkVectorMap { place: "Tulleys Farm, Crawley".into(), lat: 51.1, lon: -0.17, span: 0.02, roads: vec![hud_board::Road { class: 1, points: vec![[51.095, -0.18], [51.105, -0.16]] }], water: vec![], pending: false }),
                Block::new("NOTE // 02 — SOFTWARE", "FACT", "SOURCE: VALVE", BlockKind::NoteStack { notes: vec![note("SteamVR 3", "New compositor.")] }),
            ],
            sources: vec!["https://store.steampowered.com".into(); 42],
            id: None,
        }
    }

    /// The board with every block kind, the research block and the COMMS block.
    #[test]
    fn snapshot_board_blocks_stay_inside_their_frames() {
        let _one = hud_board::test_lock();
        let _look = crate::holo_theme::look_for_pictures();
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        // The video's stills without the network: two pictures, two YouTube does not have.
        for (k, url) in hud_board::video_frames("dQw4w9WgXcQ").iter().enumerate() {
            let still = egui::ColorImage::from_rgb([64, 36], &(0..64 * 36).flat_map(|i| [(i % 64 * 3) as u8, 60 + k as u8 * 40, (i / 64 * 6) as u8]).collect::<Vec<u8>>());
            hud_board::put_media(url, if k < 2 { hud_board::MediaSlot::Ready(std::sync::Arc::new(still)) } else { hud_board::MediaSlot::Failed });
        }
        hud_board::present(sample_report());
        // The cascade runs on the wall clock.
        std::thread::sleep(std::time::Duration::from_millis(700));
        hud_board::set_research(Some(hud_board::ResearchStatus { topic: "state of VR — a very long research topic title".into(), phase: "Reading sources".into(), started: Instant::now(), blocks: 12, sources: 42, progress: 0.45 }));
        let mut view = hud_board::BoardView::default();
        let area = Rect::from_min_max(pos2(24.0, 42.0), pos2(W - 24.0, H - 150.0));
        let comms = hud_board::dialogue_block("what does the chart say about the quest three and how does it compare to the index", "The Quest 2 still leads at 38.2 percent, Sir, with the Quest 3 on 24.1 percent and the Index at 11 percent — and this answer is long enough that it must wrap onto a second line and then stop.", true, "18:00:00").unwrap();
        let handle = rt.handle().clone();
        let mut paint = |ctx: &egui::Context| {
            crate::gui::paint_canvas(ctx);
            egui::CentralPanel::default().frame(egui::Frame::none()).show(ctx, |ui| {
                view.paint(ui, area, &handle);
                let _ = hud_board::paint_dialogue(&ui.painter_at(screen()), Rect::from_min_size(pos2(W / 2.0 - 380.0, H - 140.0), vec2(760.0, 116.0)), &comms);
            });
        };
        let canvas = Snapshot::new(vec2(W, H)).render(30, &mut paint);
        canvas.save("board_blocks");
        // Sharp at 125 and 150 % as well (Windows' display scaling).
        for (ppp, name) in [(1.25, "board_blocks_125"), (1.5, "board_blocks_150")] {
            Snapshot::new(vec2(W, H)).render_scaled(30, ppp, &mut paint).save(name);
        }
        hud_board::set_research(None);
        hud_board::sign_off();
        // Nothing is painted in the gutters between the grid and the comms block.
        let gutter = Rect::from_min_max(pos2(24.0, H - 150.0 + 2.0), pos2(W / 2.0 - 390.0, H - 142.0));
        assert!(canvas.brightness(gutter) < 30.0, "a block bled into the gutter: {}", canvas.brightness(gutter));
        // The blocks are drawn.
        assert!(canvas.brightness(area) > canvas.brightness(Rect::from_min_size(pos2(0.0, 0.0), vec2(20.0, 20.0))));
    }

    /// The itslearning board and the core sphere.
    #[test]
    fn snapshot_itslearning_board_and_core_sphere() {
        let _one = hud_board::test_lock();
        let _look = crate::holo_theme::look_for_pictures();
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let now = chrono::NaiveDate::from_ymd_opt(2026, 11, 2).unwrap().and_hms_opt(18, 0, 0).unwrap();
        let at = |d: u32| chrono::NaiveDate::from_ymd_opt(2026, 11, d).unwrap().and_hms_opt(8, 0, 0);
        let u = |id: &str, kind: &'static str, course: &str, title: &str, due| crate::lms_client::CourseUpdate { id: id.into(), kind, course: course.into(), title: title.into(), when: None, due, author: String::new(), url: String::new() };
        let updates = vec![
            u("1", "assignment", "Mathematik", "Arbeitsblatt 4: Ableitungen und Kurvendiskussion", at(5)),
            u("2", "assignment", "Physik", "Protokoll Pendelversuch", at(3)),
            u("3", "announcement", "Deutsch", "Klausur verschoben auf Donnerstag", None),
            u("4", "material", "Physik", "Neue Folien zur Mechanik", None),
        ];
        hud_board::present(crate::itslearning_bw_sso::board_report(&updates, now));
        std::thread::sleep(std::time::Duration::from_millis(700));
        let mut view = hud_board::BoardView::default();
        let area = Rect::from_min_max(pos2(24.0, 42.0), pos2(W - 24.0, H - 150.0));
        let mut snap = Snapshot::new(vec2(W, H));
        let handle = rt.handle().clone();
        let canvas = snap.render(30, |ctx| {
            crate::gui::paint_canvas(ctx);
            egui::CentralPanel::default().frame(egui::Frame::none()).show(ctx, |ui| view.paint(ui, area, &handle));
        });
        canvas.save("board_itslearning");
        hud_board::sign_off();

        crate::mind_activity::chain(&["ears.rs", "stt.rs", "intents.rs", "brain.rs"]);
        let mut sphere = crate::holo_mind::MindView::default();
        let mut snap = Snapshot::new(vec2(W, H));
        let canvas = snap.render(12, |ctx| {
            crate::gui::paint_canvas(ctx);
            egui::CentralPanel::default().frame(egui::Frame::none()).show(ctx, |ui| sphere.show(ui, area, &crate::holo_core::CoreLook { voice: 0.5, clock: 6.0, layers: [0.9, 0.7, 0.2, 0.1], ..Default::default() }, &crate::mind_activity::LAYERS, &crate::mind_activity::recent(std::time::Instant::now())));
        });
        canvas.save("core_sphere");
        let middle = Rect::from_center_size(area.center(), vec2(300.0, 300.0));
        assert!(canvas.brightness(middle) > 8.0, "the sphere is drawn: {}", canvas.brightness(middle));
        // Its other versions: his files (turned well over the top, as a hand may leave it) and his rings.
        for kind in [crate::holo_mind::CoreKind::Files, crate::holo_mind::CoreKind::Rings] {
            sphere.apply(crate::holo_mind::CoreCommand::Kind(kind));
            sphere.drag(vec2(40.0, -260.0), 1.0 / 60.0);
            let mut snap = Snapshot::new(vec2(W, H));
            let canvas = snap.render(12, |ctx| {
                crate::gui::paint_canvas(ctx);
                egui::CentralPanel::default().frame(egui::Frame::none()).show(ctx, |ui| sphere.show(ui, area, &crate::holo_core::CoreLook { voice: 0.5, clock: 6.0, layers: [0.9, 0.7, 0.2, 0.1], ..Default::default() }, &crate::mind_activity::LAYERS, &crate::mind_activity::recent(std::time::Instant::now())));
            });
            canvas.save(&format!("core_{}", kind.name()));
            assert!(canvas.brightness(middle) > 4.0, "the core's {} are drawn: {}", kind.name(), canvas.brightness(middle));
        }
    }
}
