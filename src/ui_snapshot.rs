//! Offscreen snapshots of the HUD for tests: real egui frames (the same painters the window uses)
//! tessellated by epaint and rasterised on the CPU into PNGs under `target/ui-snapshots/`, so the
//! layout, clipping and the emblem / sphere / block rendering can be checked without opening a
//! window. Triangles are filled at pixel centres with interpolated colours and bilinear texture
//! samples (the font atlas), blended premultiplied in gamma space like egui's own renderer.

use eframe::egui::{self, epaint, pos2, vec2, Color32, Rect, TextureId};
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
}

impl Snapshot {
    /// A context the size of `size` points (1 px per point) with the HUD's fonts.
    pub fn new(size: egui::Vec2) -> Self {
        let ctx = egui::Context::default();
        crate::gui::install_fonts(&ctx);
        // Measured as it rests: no window entrance or exit moves what is checked.
        ctx.data_mut(|d| d.insert_temp(egui::Id::new(crate::gui::STILL_WINDOWS), true));
        Self { ctx, textures: HashMap::new(), size }
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
    pub fn render(&mut self, frames: usize, mut ui: impl FnMut(&egui::Context)) -> Canvas {
        let mut last = None;
        for k in 0..frames.max(1) {
            let input = egui::RawInput { screen_rect: Some(self.screen()), time: Some(k as f64 / 60.0), predicted_dt: 1.0 / 60.0, ..Default::default() };
            last = Some(self.frame(input, &mut ui));
        }
        let output = last.expect("one frame");
        self.rasterise(output.shapes, output.pixels_per_point)
    }

    /// Tessellates a frame's shapes and fills the triangles into a canvas of this size.
    pub fn rasterise(&self, shapes: Vec<epaint::ClippedShape>, pixels_per_point: f32) -> Canvas {
        let primitives = self.ctx.tessellate(shapes, pixels_per_point);
        let (w, h) = (self.size.x as usize, self.size.y as usize);
        let mut canvas = Canvas { width: w, height: h, pixels: vec![[0.0, 0.0, 0.0, 1.0]; w * h] };
        for p in primitives {
            if let epaint::Primitive::Mesh(mesh) = &p.primitive {
                canvas.fill(mesh, p.clip_rect, self.textures.get(&mesh.texture_id));
            }
        }
        canvas
    }
}

impl Canvas {
    fn fill(&mut self, mesh: &epaint::Mesh, clip: Rect, texture: Option<&Texture>) {
        let clip = clip.intersect(Rect::from_min_size(pos2(0.0, 0.0), vec2(self.width as f32, self.height as f32)));
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
            for y in min_y.max(0)..max_y.min(self.height as i32) {
                for x in min_x.max(0)..max_x.min(self.width as i32) {
                    let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
                    if px < clip.left() || px > clip.right() || py < clip.top() || py > clip.bottom() {
                        continue;
                    }
                    let w0 = ((v[1].pos.x - px) * (v[2].pos.y - py) - (v[2].pos.x - px) * (v[1].pos.y - py)) / area;
                    let w1 = ((v[2].pos.x - px) * (v[0].pos.y - py) - (v[0].pos.x - px) * (v[2].pos.y - py)) / area;
                    let w2 = 1.0 - w0 - w1;
                    if w0 < -1e-4 || w1 < -1e-4 || w2 < -1e-4 {
                        continue;
                    }
                    let mut src = [0.0f32; 4];
                    for (c, s) in src.iter_mut().enumerate() {
                        *s = colours[0][c] * w0 + colours[1][c] * w1 + colours[2][c] * w2;
                    }
                    if let Some(t) = texture {
                        let u = v[0].uv.x * w0 + v[1].uv.x * w1 + v[2].uv.x * w2;
                        let vv = v[0].uv.y * w0 + v[1].uv.y * w1 + v[2].uv.y * w2;
                        let tex = t.sample(u, vv);
                        for c in 0..4 {
                            src[c] *= tex[c];
                        }
                    }
                    let dst = &mut self.pixels[y as usize * self.width + x as usize];
                    for c in 0..4 {
                        dst[c] = (src[c] + dst[c] * (1.0 - src[3])).clamp(0.0, 1.0);
                    }
                }
            }
        }
    }

    pub fn pixel(&self, x: usize, y: usize) -> [u8; 3] {
        let p = self.pixels[y * self.width + x];
        [(p[0] * 255.0) as u8, (p[1] * 255.0) as u8, (p[2] * 255.0) as u8]
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

    #[cfg(test)]
    pub fn save(&self, name: &str) -> PathBuf {
        let dir = PathBuf::from("target").join("ui-snapshots");
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
    use crate::hud_board::{self, BlockKind};
    use crate::rose_emblem_renderer::{RoseEmblem, RoseEmblemState};
    use std::time::Instant;

    const W: f32 = 1280.0;
    const H: f32 = 760.0;

    fn screen() -> Rect {
        Rect::from_min_size(pos2(0.0, 0.0), vec2(W, H))
    }

    /// The emblem in each state, on the obsidian canvas.
    #[test]
    fn snapshot_emblem_states() {
        for (state, name, steps) in [(RoseEmblemState::AwakeFrontEmblem, "emblem_front", 1), (RoseEmblemState::ThinkingGyroscope, "emblem_gyroscope", 40), (RoseEmblemState::CornerDocked, "emblem_docked", 1)] {
            let mut emblem = RoseEmblem::new(state);
            for _ in 0..steps {
                emblem.step(1.0 / 60.0, 0.4, state == RoseEmblemState::ThinkingGyroscope);
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
        let emblem = RoseEmblem::new(RoseEmblemState::AwakeFrontEmblem);
        let paint = |finish: bool| {
            let mut snap = Snapshot::new(vec2(W, H));
            snap.render(3, |ctx| {
                crate::gui::paint_canvas(ctx);
                egui::CentralPanel::default().frame(egui::Frame::none()).show(ctx, |ui| emblem.paint(&ui.painter_at(screen()), screen()));
                if finish {
                    crate::gui::paint_finish(ctx, false);
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
                Block::new("VIDEO // STEAM FRAME HANDS-ON", "SOURCE: YOUTUBE", "", BlockKind::MediaCard { images: vec!["https://invalid.example/a.jpg".into()], caption: "Hands-on with the new headset at the event".into() }),
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
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        hud_board::present(sample_report());
        // The cascade runs on the wall clock.
        std::thread::sleep(std::time::Duration::from_millis(700));
        hud_board::set_research(Some(hud_board::ResearchStatus { topic: "state of VR — a very long research topic title".into(), phase: "Reading sources".into(), started: Instant::now(), blocks: 12, sources: 42, progress: 0.45 }));
        let mut view = hud_board::BoardView::default();
        let area = Rect::from_min_max(pos2(24.0, 42.0), pos2(W - 24.0, H - 150.0));
        let comms = hud_board::dialogue_block("what does the chart say about the quest three and how does it compare to the index", "The Quest 2 still leads at 38.2 percent, Sir, with the Quest 3 on 24.1 percent and the Index at 11 percent — and this answer is long enough that it must wrap onto a second line and then stop.", true, "18:00:00").unwrap();
        let mut snap = Snapshot::new(vec2(W, H));
        let handle = rt.handle().clone();
        let canvas = snap.render(30, |ctx| {
            crate::gui::paint_canvas(ctx);
            egui::CentralPanel::default().frame(egui::Frame::none()).show(ctx, |ui| {
                view.paint(ui, area, &handle);
                hud_board::paint_dialogue(&ui.painter_at(screen()), Rect::from_min_size(pos2(W / 2.0 - 380.0, H - 140.0), vec2(760.0, 116.0)), &comms);
            });
        });
        canvas.save("board_blocks");
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

        crate::core_sphere_3d::CoreActivityBus::chain(&["ears.rs", "stt.rs", "intents.rs", "brain.rs"]);
        let mut sphere = crate::core_sphere_3d::CoreSphereView::default();
        let mut snap = Snapshot::new(vec2(W, H));
        let canvas = snap.render(12, |ctx| {
            crate::gui::paint_canvas(ctx);
            egui::CentralPanel::default().frame(egui::Frame::none()).show(ctx, |ui| sphere.show(ui, area, 0.5));
        });
        canvas.save("core_sphere");
        let middle = Rect::from_center_size(area.center(), vec2(300.0, 300.0));
        assert!(canvas.brightness(middle) > 8.0, "the sphere is drawn: {}", canvas.brightness(middle));
    }
}
