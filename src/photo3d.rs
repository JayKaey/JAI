//! PHOTO TO 3D: the 3D studio's last resort before composing primitives. When neither the
//! library, the curated repositories nor Sketchfab and its free Objaverse mirror have a real
//! model of a thing, JAI rebuilds it from a photograph:
//! 1. the picture search (`image_viewer`) brings a few photos; BiRefNet lite cuts each subject
//!    out, and the cleanest whole subject wins;
//! 2. it is framed the way TripoSR was trained: cropped to the subject, which fills 0.85 of a
//!    512² square on mid grey;
//! 3. TripoSR's encoder turns the picture into a triplane; its decoder gives the density on a
//!    256³ grid (radius 0.87), and marching cubes cuts the surface at density 25;
//! 4. the decoder's colours paint the vertices, and the mesh is turned upright (TripoSR's Z-up
//!    to the studio's Y-up, the photographed side towards the studio's camera).
//!
//! Offline: the three networks lie in `models/photo3d`, fetched once by hand with Sir's leave,
//! never by JAI (without them the studio composes, as before). They run on half the CPU's
//! threads, never the GPU. Licences: TripoSR (Tripo AI and Stability AI; the ONNX export is
//! QtMeshEditor's) and BiRefNet (Zheng et al.), both MIT.

use crate::mesh_io::{self, Material, Model, Part};
use image::imageops::{self, FilterType};
use image::{GrayImage, Luma, Rgb, Rgb32FImage, RgbImage};
use ort::session::Session;
use ort::value::Tensor;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Instant;
use tracing::info;

pub const CUTOUT: &str = "birefnet_lite.onnx";
pub const ENCODER: &str = "triposr_encoder_int8.onnx";
pub const DECODER: &str = "triposr_decoder.onnx";
/// What `build` answers when the request went away mid-way.
pub const CANCELLED: &str = "cancelled";
/// BiRefNet's square input.
const CUT_SIDE: u32 = 1024;
/// TripoSR's picture: 512², the subject filling 0.85 of it on grey 0.5.
const SIDE: u32 = 512;
const FILL: f32 = 0.85;
const GREY: f32 = 0.5;
/// The density grid (points a side), its half-width and the surface's density.
const GRID: usize = 256;
const RADIUS: f32 = 0.87;
const THRESHOLD: f32 = 25.0;
/// Points per decoder call.
const CHUNK: usize = 1 << 18;
/// Larger photos are shrunk to this first.
const MAX_PHOTO: u32 = 1024;
/// A subject score that needs no other photo.
const CRISP: f32 = 0.9;

/// Where the networks lie: `photo3d` beside Kokoro's model (`models/photo3d`).
pub fn dir(cfg: &crate::config::JaiConfig) -> PathBuf {
    Path::new(&cfg.kokoro_model_path).parent().unwrap_or(Path::new("models")).join("photo3d")
}

pub fn installed(dir: &Path) -> bool {
    [CUTOUT, ENCODER, DECODER].iter().all(|f| dir.join(f).is_file())
}

fn msg(e: impl std::fmt::Display) -> String {
    e.to_string()
}

/// The CPU only: on DirectML the cut-out reset the GPU (DXGI 887A0007, 2026-11-09), which Sir's
/// game and video would feel.
fn session(path: &Path, threads: usize) -> Result<Session, String> {
    let env = crate::config::ort_env().map_err(msg)?;
    Session::builder(env)
        .map_err(msg)?
        .with_optimization_level(ort::session::builder::GraphOptimizationLevel::Level3)
        .unwrap_or_else(|e| e.recover())
        .with_intra_threads(threads.max(1))
        .unwrap_or_else(|e| e.recover())
        .commit_from_file(path)
        .map_err(|e| format!("{}: {e}", path.display()))
}

// ------------------------------------------------------------------------------ the photo ----

/// BiRefNet's matte of `rgb` (0 background … 255 subject), at its size.
fn matte(net: &mut Session, rgb: &RgbImage) -> Result<GrayImage, String> {
    const MEAN: [f32; 3] = [0.485, 0.456, 0.406];
    const STD: [f32; 3] = [0.229, 0.224, 0.225];
    let sq = imageops::resize(rgb, CUT_SIDE, CUT_SIDE, FilterType::Triangle);
    let (side, n) = (CUT_SIDE as usize, (CUT_SIDE * CUT_SIDE) as usize);
    let mut x = vec![0.0f32; 3 * n];
    for (i, p) in sq.pixels().enumerate() {
        for c in 0..3 {
            x[c * n + i] = (p[c] as f32 / 255.0 - MEAN[c]) / STD[c];
        }
    }
    let out = net.run(ort::inputs![Tensor::from_array(([1usize, 3, side, side], x)).map_err(msg)?]).map_err(msg)?;
    let (_, logits) = out[0].try_extract_tensor::<f32>().map_err(msg)?;
    if logits.len() != n {
        return Err("the cut-out network gave an odd matte".into());
    }
    let mask = GrayImage::from_fn(CUT_SIDE, CUT_SIDE, |x, y| Luma([(255.0 / (1.0 + (-logits[(y * CUT_SIDE + x) as usize]).exp())).round() as u8]));
    Ok(imageops::resize(&mask, rgb.width(), rgb.height(), FilterType::Triangle))
}

/// The box around the matte's subject (alpha above one half): x0, y0, x1, y1 inclusive.
fn subject_box(alpha: &GrayImage) -> Option<(u32, u32, u32, u32)> {
    alpha.enumerate_pixels().filter(|(_, _, p)| p[0] > 127).fold(None, |b, (x, y, _)| Some(b.map_or((x, y, x, y), |(x0, y0, x1, y1)| (x0.min(x), y0.min(y), x1.max(x), y1.max(y)))))
}

/// How well a matte shows one whole subject (higher is better): a crisp edge, not cut off by the
/// picture's border. None when there is none or it fills the picture (a landscape, a close-up).
pub fn subject_score(alpha: &GrayImage) -> Option<f32> {
    let (w, h) = alpha.dimensions();
    let (x0, y0, x1, y1) = subject_box(alpha)?;
    let (mut solid, mut soft) = (0u32, 0u32);
    for p in alpha.pixels() {
        match p[0] {
            128.. => solid += 1,
            26..=127 => soft += 1,
            _ => {}
        }
    }
    let cover = solid as f32 / (w * h) as f32;
    if !(0.02..=0.8).contains(&cover) {
        return None;
    }
    let cut = [x0 == 0, y0 == 0, x1 + 1 == w, y1 + 1 == h].iter().filter(|&&c| c).count() as f32;
    Some(1.0 - soft as f32 / solid as f32 - 0.3 * cut + cover.min(0.3))
}

/// TripoSR's picture: the subject cut out by its matte, centred on mid grey so that it fills
/// 0.85 of the square, at 512² (planar RGB, 0..1).
pub fn frame(rgb: &RgbImage, alpha: &GrayImage) -> Option<Vec<f32>> {
    let (x0, y0, x1, y1) = subject_box(alpha)?;
    let (w, h) = (x1 - x0 + 1, y1 - y0 + 1);
    let side = (w.max(h) as f32 / FILL).round() as u32;
    let (ox, oy) = ((side - w) / 2, (side - h) / 2);
    let mut canvas = Rgb32FImage::from_pixel(side, side, Rgb([GREY; 3]));
    for y in 0..h {
        for x in 0..w {
            let a = alpha.get_pixel(x0 + x, y0 + y)[0] as f32 / 255.0;
            let c = rgb.get_pixel(x0 + x, y0 + y).0;
            canvas.put_pixel(ox + x, oy + y, Rgb(c.map(|v| v as f32 / 255.0 * a + GREY * (1.0 - a))));
        }
    }
    let small = imageops::resize(&canvas, SIDE, SIDE, FilterType::Triangle);
    let n = (SIDE * SIDE) as usize;
    let mut chw = vec![0.0f32; 3 * n];
    for (i, p) in small.pixels().enumerate() {
        for c in 0..3 {
            chw[c * n + i] = p[c].clamp(0.0, 1.0);
        }
    }
    Some(chw)
}

// ------------------------------------------------------------------------------ the field ----

/// The triplane: its shape and values.
type Codes = (Vec<i64>, Vec<f32>);

fn triplane(net: &mut Session, picture: Vec<f32>) -> Result<Codes, String> {
    let s = SIDE as usize;
    let out = net.run(ort::inputs![Tensor::from_array(([1usize, 3, s, s], picture)).map_err(msg)?]).map_err(msg)?;
    let (shape, codes) = out[0].try_extract_tensor::<f32>().map_err(msg)?;
    Ok((shape.to_vec(), codes.to_vec()))
}

/// The field at `points` (x, y, z within ±RADIUS, flat): density and colour (0..1) per point.
fn decode(net: &mut Session, codes: &Codes, points: Vec<f32>) -> Result<(Vec<f32>, Vec<f32>), String> {
    let p = points.len() / 3;
    let out = net
        .run(ort::inputs![
            "scene_codes" => Tensor::from_array((codes.0.clone(), codes.1.clone())).map_err(msg)?,
            "points" => Tensor::from_array(([1usize, p, 3], points)).map_err(msg)?
        ])
        .map_err(msg)?;
    let get = |name: &str| -> Result<Vec<f32>, String> { Ok(out.get(name).ok_or_else(|| format!("the decoder has no {name}"))?.try_extract_tensor::<f32>().map_err(msg)?.1.to_vec()) };
    Ok((get("density")?, get("color")?))
}

/// Density minus the threshold on the GRID³ lattice over ±RADIUS (x slowest, z fastest):
/// positive inside.
fn field(net: &mut Session, codes: &Codes, go: &dyn Fn(f32, &str) -> bool) -> Result<Vec<f32>, String> {
    let n = GRID;
    let (total, step) = (n * n * n, 2.0 * RADIUS / (n - 1) as f32);
    let mut out = Vec::with_capacity(total);
    while out.len() < total {
        let at = out.len();
        let points = (at..(at + CHUNK).min(total)).flat_map(|i| [i / (n * n), i / n % n, i % n].map(|g| g as f32 * step - RADIUS)).collect();
        out.extend(decode(net, codes, points)?.0.iter().map(|d| d - THRESHOLD));
        if !go(0.5 + 0.35 * out.len() as f32 / total as f32, "Measuring its shape…") {
            return Err(CANCELLED.into());
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------- the surface ----

/// The classic marching-cubes triangle table (Paul Bourke's): each corner case's triangles as
/// edge numbers in hex.
#[rustfmt::skip]
const TRIANGLES: [&str; 256] = [
    "","083","019","183981","12a","08312a","92a029","2832a8a98","3b2","0b28b0","19023b","1b219b98b","3a1ba3","0a108a8ba","3903b9ba9","98aa8b",
    "478","430734","019847","419471731","12a847","34730412a","92a902847","2a9297273794","8473b2","b47b24204","90184723b","47b94b9b2921","3a13ba784","1ba14b1047b4","47890b9bab03","47b4b99ba",
    "954","954083","054150","854835315","12a954","30812a495","52a542402","2a5325354348","95423b","0b208b495","05401523b","21525828b485","a3ba13954","4950818a18ba","54050b5bab03","54858aa8b",
    "978579","930953573","078017157","153357","978957a12","a12950530573","802825857a52","2a5253357","7957893b2","95797292027b","23b018178157","b21b17715","958857a13a3b","5705097b010aba0","ba0b03a50807570","ba57b5",
    "a65","0835a6","9015a6","1831985a6","165261","165126308","965906026","598582526328","23ba65","b08b20a65","01923b5a6","5a61929b298b","63b653513","08b0b50515b6","3b6036065059","65969bb98",
    "5a6478","43047365a","1905a6847","a65197173794","612651478","125526304347","847905065026","739794329596269","3b2784a65","5a647242027b","01947823b5a6","9219b294b7b45a6","8473b53515b6","51b5b610b7b404b","059065036b63847","65969b4797b9",
    "a4964a","4a649a083","a01a60640","83181686461a","149124264","308129249264","024426","832824426","a49a64b23","08228b49a4a6","3b201606461a","64161a48121b8b1","964936913b63","8b1810b61914641","3b6360064","648b68",
    "7a678a89a","0730a709a67a","a671a7178180","a67a71173","126168189867","269291679093739","780706602","732672","23ba68a89867","20727b09767a9a7","1801781a767a23b","b21b17a61671","896867916b63136","091b67","7807063b0b60","7b6",
    "76b","308b76","019b76","819831b76","a126b7","12a3086b7","2902a96b7","6b72a3a83a98","723627","708760620","276237019","162186198876","a76a17137","a7617a187108","03707a0a96a7","76a7a88a9",
    "684b86","36b306046","86b846901","946963931b36","6846b82a1","12a30b06b046","4b846b0292a9","a93a32943b36463","823842462","042462","190234246438","194142246","8138618466a1","a10a06604","4634386a3039a93","a946a4",
    "49576b","083495b76","50154076b","b76834354315","954a1276b","6b712a083495","76b54a42a402","348354325a52b76","723762549","954086062687","362376150540","628687218485158","954a16176137","16a176107870954","40a4a503a6a737a","76a7a854a48a",
    "6956b9b89","36b063056095","0b805b01556b","6b3635531","12a95b9b8b56","0b306b09656912a","b85b56805a52025","6b36352a3a53","589528562382","956960062","158180568382628","156216","13616a386569896","a10a06950560","03856a","a56",
    "b5a75b","b5ab75830","5b75ab190","a75ab7981831","b12b71751","08312717572b","9759279022b7","75272b592328982","25a235375","820852875a25","9015a35373a2","982921872a25752","135375","087071175","903935537","987597",
    "5845a8ab8","5045b05abb30","01984a8aba45","ab4a45b34941314","2512852b8458","04b0b345b2b151b","0250592b5458b85","9452b3","25a352345384","5a2524420","3a235a385458019","5a2524192942","845853351","045105","845853905035","945",
    "4b749b9ab","0834979b79ab","1ab1b414074b","3143481a474bab4","4b79b492b912","9749b791b2b1083","b74b42240","b74b42834324","29a279237749","9a7974a27870207","37a3a274a1a040a","1a2874","491417713","491417081871","403743","487",
    "9a8ab8","30939bb9a","01a0a88ab","31ab3a","12b1b99b8","30939b1292b9","02b80b","32b","23828aa89","9a2092","23828a0181a8","1a2","138918","091","038","",
];
/// A cube's corners and its edges (corner pairs), numbered as the table numbers them.
const CORNERS: [[usize; 3]; 8] = [[0, 0, 0], [1, 0, 0], [1, 1, 0], [0, 1, 0], [0, 0, 1], [1, 0, 1], [1, 1, 1], [0, 1, 1]];
const EDGES: [[usize; 2]; 12] = [[0, 1], [1, 2], [2, 3], [3, 0], [4, 5], [5, 6], [6, 7], [7, 4], [0, 4], [1, 5], [2, 6], [3, 7]];

/// The surface where `field` (n³, x slowest, positive inside) crosses zero: vertices in grid
/// units, shared between cubes, and triangles counter-clockwise seen from outside.
pub fn marching_cubes(field: &[f32], n: usize) -> (Vec<[f32; 3]>, Vec<u32>) {
    let key = |p: [usize; 3]| (p[0] * n + p[1]) * n + p[2];
    let (mut verts, mut tris) = (Vec::new(), Vec::new());
    let mut shared: HashMap<(usize, usize), u32> = HashMap::new();
    for x in 0..n.saturating_sub(1) {
        for y in 0..n - 1 {
            for z in 0..n - 1 {
                let corner = |i: usize| [x + CORNERS[i][0], y + CORNERS[i][1], z + CORNERS[i][2]];
                let case = (0..8).filter(|&i| field[key(corner(i))] > 0.0).fold(0, |c, i| c | 1 << i);
                for t in TRIANGLES[case].as_bytes().chunks_exact(3) {
                    let mut ids = [0u32; 3];
                    for (id, &e) in ids.iter_mut().zip(t) {
                        let [a, b] = EDGES[(e as char).to_digit(16).unwrap_or(0) as usize].map(corner);
                        let (ka, kb) = (key(a), key(b));
                        *id = *shared.entry((ka.min(kb), ka.max(kb))).or_insert_with(|| {
                            let s = field[ka] / (field[ka] - field[kb]);
                            verts.push([0, 1, 2].map(|c| a[c] as f32 + s * (b[c] as f32 - a[c] as f32)));
                            (verts.len() - 1) as u32
                        });
                    }
                    // (The table winds the other way round for a field that is positive inside.)
                    tris.extend([ids[0], ids[2], ids[1]]);
                }
            }
        }
    }
    (verts, tris)
}

/// TripoSR's frame (Z up, photographed from +X) to the studio's (Y up, the photographed side
/// towards its camera at +Z): −90° about X and +90° about Y, as TripoSR's viewer turns it, then
/// a half turn about Y (measured: without it the studio showed the back first).
pub fn upright(p: [f32; 3]) -> [f32; 3] {
    [p[1], p[2], p[0]]
}

/// The rebuilt thing as a studio model: one part painted by its vertex colours (sRGB, 0..1),
/// turned upright.
pub fn model_of(name: &str, points: &[[f32; 3]], indices: Vec<u32>, colours: &[[f32; 3]]) -> Model {
    let positions: Vec<[f32; 3]> = points.iter().map(|&p| upright(p)).collect();
    let part = Part {
        name: name.to_string(),
        normals: mesh_io::smooth_normals(&positions, &indices),
        colors: colours.iter().map(|c| c.map(|v| mesh_io::srgb_to_linear(v.clamp(0.0, 1.0)))).collect(),
        positions,
        indices,
        ..Part::default()
    };
    let material = Material { name: "photo colours".into(), base_color: [1.0; 4], roughness: 0.8, vertex_colors: true, ..Material::default() };
    Model { name: name.to_string(), parts: vec![part], materials: vec![material], unit_m: 1.0, ..Model::default() }
}

/// Rebuilds the thing in the clearest of `photos` (encoded pictures): the model and which photo
/// it came from. `go` reports progress and says whether to carry on.
pub fn build(dir: &Path, photos: &[Vec<u8>], name: &str, threads: usize, go: &dyn Fn(f32, &str) -> bool) -> Result<(Model, usize), String> {
    let started = Instant::now();
    let mut times = Vec::new();
    go(0.05, "Cutting the subject out…");
    let mut cut = session(&dir.join(CUTOUT), threads)?;
    let mut best: Option<(f32, usize, RgbImage, GrayImage)> = None;
    for (i, bytes) in photos.iter().enumerate() {
        let Some(img) = crate::image_viewer::load_upright(bytes) else { continue };
        let rgb = if img.width().max(img.height()) > MAX_PHOTO { img.resize(MAX_PHOTO, MAX_PHOTO, FilterType::Triangle) } else { img }.to_rgb8();
        let alpha = matte(&mut cut, &rgb)?;
        let score = subject_score(&alpha);
        info!("[PHOTO3D] Photo {} ({}×{}): subject score {score:?}", i + 1, rgb.width(), rgb.height());
        if let Some(s) = score.filter(|s| best.as_ref().is_none_or(|b| *s > b.0)) {
            best = Some((s, i, rgb, alpha));
        }
        if !go(0.05 + 0.15 * (i + 1) as f32 / photos.len() as f32, "Cutting the subject out…") {
            return Err(CANCELLED.into());
        }
        // A crisp whole subject ends the search (each cut-out takes seconds).
        if best.as_ref().is_some_and(|b| b.0 >= CRISP) {
            break;
        }
    }
    drop(cut);
    times.push(started.elapsed());
    let (_, chosen, rgb, alpha) = best.ok_or("no photo shows one whole subject")?;
    let picture = frame(&rgb, &alpha).ok_or("the subject vanished")?;
    go(0.25, "Reading its shape from the photo…");
    let codes = triplane(&mut session(&dir.join(ENCODER), threads)?, picture)?;
    times.push(started.elapsed());
    if !go(0.5, "Measuring its shape…") {
        return Err(CANCELLED.into());
    }
    let mut dec = session(&dir.join(DECODER), threads)?;
    let grid = field(&mut dec, &codes, go)?;
    times.push(started.elapsed());
    go(0.86, "Cutting out the surface…");
    let (verts, indices) = marching_cubes(&grid, GRID);
    drop(grid);
    if indices.is_empty() {
        return Err("the photo gave no solid shape".into());
    }
    go(0.93, "Painting it…");
    let step = 2.0 * RADIUS / (GRID - 1) as f32;
    let points: Vec<[f32; 3]> = verts.iter().map(|v| v.map(|g| g * step - RADIUS)).collect();
    let mut colours = Vec::with_capacity(points.len());
    for chunk in points.chunks(CHUNK) {
        let (_, c) = decode(&mut dec, &codes, chunk.iter().flatten().copied().collect())?;
        colours.extend(c.chunks_exact(3).map(|c| [c[0], c[1], c[2]]));
    }
    let model = model_of(name, &points, indices, &colours);
    let ms: Vec<u128> = times.iter().map(|t| t.as_millis()).collect();
    info!("[PHOTO3D] '{name}' from photo {}: {} triangles in {} ms on {threads} threads (cut-outs done at {} ms, triplane {}, field {})", chosen + 1, model.triangles(), started.elapsed().as_millis(), ms[0], ms[1], ms[2]);
    Ok((model, chosen))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mesh_io::math;

    /// A ball of radius `r` (grid units) in an n³ grid, positive inside.
    fn ball(n: usize, r: f32) -> Vec<f32> {
        let c = (n - 1) as f32 / 2.0;
        (0..n * n * n).map(|i| r - math::length([(i / (n * n)) as f32 - c, (i / n % n) as f32 - c, (i % n) as f32 - c])).collect()
    }

    #[test]
    fn marching_cubes_closes_a_ball_facing_out() {
        let (n, r) = (24, 8.3);
        let (verts, tris) = marching_cubes(&ball(n, r), n);
        assert!(tris.len() > 300, "{} triangles", tris.len() / 3);
        let c = [(n - 1) as f32 / 2.0; 3];
        // On the sphere …
        for v in &verts {
            assert!((math::length(math::sub(*v, c)) - r).abs() < 0.1, "{v:?}");
        }
        // … every triangle facing away from its centre …
        let mut volume = 0.0;
        for t in tris.chunks_exact(3) {
            let [a, b, d] = [0, 1, 2].map(|k| math::sub(verts[t[k] as usize], c));
            let normal = math::cross(math::sub(b, a), math::sub(d, a));
            assert!(math::dot(normal, a) > 0.0, "a triangle faces inwards");
            volume += math::dot(a, math::cross(b, d)) / 6.0;
        }
        let ideal = 4.0 / 3.0 * std::f32::consts::PI * r * r * r;
        assert!((volume / ideal - 1.0).abs() < 0.03, "volume {volume} vs {ideal}");
        // … and closed: every edge between exactly two triangles.
        let mut edges: HashMap<(u32, u32), u32> = HashMap::new();
        for t in tris.chunks_exact(3) {
            for (a, b) in [(t[0], t[1]), (t[1], t[2]), (t[2], t[0])] {
                *edges.entry((a.min(b), a.max(b))).or_default() += 1;
            }
        }
        assert!(edges.values().all(|&k| k == 2), "the surface has holes");
    }

    #[test]
    fn an_empty_or_full_grid_has_no_surface() {
        assert!(marching_cubes(&[-1.0; 64], 4).1.is_empty());
        assert!(marching_cubes(&[1.0; 64], 4).1.is_empty());
        assert!(marching_cubes(&[], 0).1.is_empty());
    }

    #[test]
    fn the_model_stands_upright_and_keeps_its_colours() {
        // TripoSR's up (+Z) is the studio's up (+Y); the turn keeps lengths and handedness.
        assert_eq!(upright([0.0, 0.0, 1.0]), [0.0, 1.0, 0.0]);
        // TripoSR's camera (+X) is the studio's (+Z); its right (+Y) is the studio's right (+X).
        assert_eq!(upright([1.0, 0.0, 0.0]), [0.0, 0.0, 1.0]);
        assert_eq!(upright([0.0, 1.0, 0.0]), [1.0, 0.0, 0.0]);
        let (x, y, z) = (upright([1.0, 0.0, 0.0]), upright([0.0, 1.0, 0.0]), upright([0.0, 0.0, 1.0]));
        assert_eq!(math::cross(x, y), z);

        let (verts, tris) = marching_cubes(&ball(12, 4.0), 12);
        let colours = vec![[1.0, 0.5, 0.0]; verts.len()];
        let m = model_of("Teapot", &verts, tris, &colours);
        assert_eq!(m.parts.len(), 1);
        let p = &m.parts[0];
        assert_eq!((p.colors.len(), p.normals.len()), (p.positions.len(), p.positions.len()));
        assert!((p.colors[0][1] - mesh_io::srgb_to_linear(0.5)).abs() < 1e-6, "stored linear");
        assert!(m.materials[0].vertex_colors);
        // Still facing out after the turn.
        let c = m.bounds().centre();
        let t = &p.indices[..3];
        let [a, b, d] = [0, 1, 2].map(|k| p.positions[t[k] as usize]);
        assert!(math::dot(math::cross(math::sub(b, a), math::sub(d, a)), math::sub(a, c)) > 0.0);
    }

    /// A 200×100 photo: a red block (the subject) at x 120..160, y 30..90 on blue.
    fn photo() -> (RgbImage, GrayImage) {
        let inside = |x: u32, y: u32| (120..160).contains(&x) && (30..90).contains(&y);
        let rgb = RgbImage::from_fn(200, 100, |x, y| if inside(x, y) { Rgb([255, 0, 0]) } else { Rgb([0, 0, 255]) });
        let alpha = GrayImage::from_fn(200, 100, |x, y| Luma([if inside(x, y) { 255 } else { 0 }]));
        (rgb, alpha)
    }

    #[test]
    fn the_subject_is_framed_on_grey_at_085() {
        let (rgb, alpha) = photo();
        let px = frame(&rgb, &alpha).expect("a subject");
        let n = (SIDE * SIDE) as usize;
        assert_eq!(px.len(), 3 * n);
        let at = |x: usize, y: usize| [0, 1, 2].map(|c| px[c * n + y * SIDE as usize + x]);
        assert_eq!(at(256, 256), [1.0, 0.0, 0.0], "the subject in the middle");
        assert_eq!(at(3, 3), [GREY; 3], "mid grey, not the photo's blue");
        // The 60 px tall subject fills 0.85 of the height; its 40 px width is centred.
        let tall = (0..SIDE as usize).filter(|&y| at(256, y)[0] > 0.75).count() as f32;
        assert!((tall / SIDE as f32 - FILL).abs() < 0.01, "{tall}");
        let wide = (0..SIDE as usize).filter(|&x| at(x, 256)[0] > 0.75).collect::<Vec<_>>();
        assert!((wide[0] as f32 + wide[wide.len() - 1] as f32) / 2.0 - 255.5 < 1.5);
        assert!(frame(&rgb, &GrayImage::new(200, 100)).is_none());
    }

    #[test]
    fn a_whole_crisp_subject_beats_a_cut_off_one() {
        let (_, alpha) = photo();
        let whole = subject_score(&alpha).expect("a subject");
        let cut = GrayImage::from_fn(200, 100, |x, y| Luma([if x < 40 && (30..90).contains(&y) { 255 } else { 0 }]));
        assert!(subject_score(&cut).expect("a subject") < whole);
        let soft = GrayImage::from_fn(200, 100, |x, y| Luma([if alpha.get_pixel(x, y)[0] > 0 { 255 } else if (100..180).contains(&x) && (10..95).contains(&y) { 90 } else { 0 }]));
        assert!(subject_score(&soft).expect("a subject") < whole);
        // Nothing, or everything (a landscape), is no subject.
        assert!(subject_score(&GrayImage::new(200, 100)).is_none());
        assert!(subject_score(&GrayImage::from_pixel(200, 100, Luma([255]))).is_none());
    }

    #[test]
    #[ignore = "runs the real networks (PHOTO3D_DIR, else models/photo3d) on the CPU for about a minute"]
    fn the_networks_rebuild_a_drawn_ball() {
        let dir = PathBuf::from(std::env::var("PHOTO3D_DIR").unwrap_or_else(|_| "models/photo3d".into()));
        assert!(installed(&dir), "no networks in {}", dir.display());
        // An orange ball lit from the top left on white, with a navy dot on its upper left.
        let img = RgbImage::from_fn(512, 512, |x, y| {
            let (dx, dy) = ((x as f32 - 256.0) / 150.0, (y as f32 - 256.0) / 150.0);
            let r2 = dx * dx + dy * dy;
            if r2 > 1.0 {
                return Rgb([255, 255, 255]);
            }
            if (dx + 0.4).powi(2) + (dy + 0.35).powi(2) < 0.06 {
                return Rgb([20, 30, 120]);
            }
            let light = (0.25 + 0.75 * (-0.5 * dx - 0.6 * dy + 0.62 * (1.0 - r2).sqrt()).max(0.0)).min(1.0);
            Rgb([235.0 * light, 120.0 * light, 30.0 * light].map(|v| v as u8))
        });
        let mut png = Vec::new();
        img.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png).unwrap();
        let cores = std::thread::available_parallelism().map_or(4, |n| n.get());
        let (m, chosen) = build(&dir, &[png], "Ball", cores / 2, &|p, s| {
            eprintln!("{:3.0}% {s}", p * 100.0);
            true
        })
        .unwrap();
        assert_eq!(chosen, 0);
        let (size, centre) = (m.bounds().size(), m.bounds().centre());
        let p = &m.parts[0];
        let mean = p.colors.iter().fold([0.0f32; 3], |a, c| math::add(a, *c));
        // Where the dot went (navy: more blue than red), from the model's centre.
        let navy: Vec<[f32; 3]> = p.positions.iter().zip(&p.colors).filter(|(_, c)| c[2] > c[0]).map(|(v, _)| math::sub(*v, centre)).collect();
        let dot = math::scale(navy.iter().fold([0.0; 3], |a, v| math::add(a, *v)), 1.0 / navy.len().max(1) as f32);
        eprintln!("{} triangles, size {size:?}, colour sum {mean:?}, {} navy vertices around {dot:?}", m.triangles(), navy.len());
        assert!(m.triangles() > 2000);
        // As wide as tall, with depth: a single view's guess at a ball.
        assert!((size[0] / size[1] - 1.0).abs() < 0.2 && size[2] > 0.25 * size[0], "{size:?}");
        assert!(mean[0] > 2.0 * mean[2], "orange, not {mean:?}");
        // The photographed side faces the studio's camera (+Z), the right way round.
        assert!(dot[2] > 0.1 && dot[0] < -0.05 && dot[1] > 0.05, "the dot is at {dot:?}");
    }
}
