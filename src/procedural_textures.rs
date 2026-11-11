//! Finishes and planet surfaces for the 3D studio, computed in Rust: "make it carbon fibre",
//! "marble", "wood", "camo", "brushed steel" and the rest, and the Moon, Mars, Jupiter … when no
//! photo of them is at hand.
//!
//! * A finish is a preset material (colour, metallic, roughness) and, for the patterned ones, a
//!   texture set: albedo, a normal map from the pattern's height (Sobel), and a packed
//!   occlusion/roughness/metallic map (glTF's R/G/B). Every pattern is seamless (noise and
//!   lattices wrap at the edges) and depends only on its seed, so the same request always looks
//!   the same. The studio projects them from three sides (triplanar), so any model wears them,
//!   with or without UVs.
//! * Planets are equirectangular maps from 3D noise sampled on the sphere (no seam, no pinched
//!   poles) with craters, bands, polar caps and a normal map where the surface has relief.
//!
//! Plain CPU work spread over the cores; a 512² finish takes a few milliseconds in release.

use crate::mesh_io::{srgb_to_linear, Texture};
use std::collections::HashMap;
use std::f32::consts::{PI, TAU};
use std::sync::{Arc, LazyLock, Mutex};

/// The side of a finish's textures.
pub const FINISH_SIZE: u32 = 512;
/// A planet map's width (its height is half of it).
pub const PLANET_WIDTH: u32 = 1024;

// ----------------------------------------------------------------------------- finishes ----

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Pattern {
    /// No texture: the factors alone (polished metals, paint, glass).
    Plain,
    Brushed,
    Carbon,
    Marble,
    Wood,
    Camo,
    Concrete,
    Leather,
    Rust,
    Lava,
    Hex,
}

/// A material preset Sir can name.
#[derive(Debug, PartialEq)]
pub struct Finish {
    /// How JAI says it ("carbon fibre").
    pub name: &'static str,
    /// The words that ask for it, English and German, longest first where they overlap.
    pub words: &'static [&'static str],
    /// Linear base colour; None keeps the model's own colour ("make it matte").
    pub color: Option<[f32; 3]>,
    pub metallic: f32,
    pub roughness: f32,
    pub pattern: Pattern,
    /// Below 1: see-through (glass).
    pub alpha: f32,
    /// Self-lit strength (lava).
    pub emissive: f32,
}

const fn f(name: &'static str, words: &'static [&'static str], color: Option<[f32; 3]>, metallic: f32, roughness: f32, pattern: Pattern) -> Finish {
    Finish { name, words, color, metallic, roughness, pattern, alpha: 1.0, emissive: 0.0 }
}

/// Every finish (metal colours are linear reflectances of the real metals).
pub static FINISHES: &[Finish] = &[
    f("brushed steel", &["brushed steel", "brushed metal", "brushed aluminium", "brushed aluminum", "gebürsteter stahl", "gebürstetem stahl", "gebürstetes metall", "gebürstetem metall", "gebürstet"], Some([0.66, 0.67, 0.69]), 1.0, 0.38, Pattern::Brushed),
    f("carbon fibre", &["carbon fibre", "carbon fiber", "carbon-fibre", "carbon-fiber", "carbonfaser", "kohlefaser", "kohlenstofffaser", "carbon"], Some([1.0, 1.0, 1.0]), 0.0, 0.3, Pattern::Carbon),
    f("marble", &["marble", "marmor"], Some([1.0, 1.0, 1.0]), 0.0, 0.18, Pattern::Marble),
    f("wood", &["wooden", "wood", "holz", "hölzern"], Some([1.0, 1.0, 1.0]), 0.0, 0.62, Pattern::Wood),
    f("camouflage", &["camouflage", "camo", "tarnmuster", "tarnfarbe", "tarnfarben", "flecktarn"], Some([1.0, 1.0, 1.0]), 0.0, 0.85, Pattern::Camo),
    f("concrete", &["concrete", "stone", "beton", "stein"], Some([1.0, 1.0, 1.0]), 0.0, 0.9, Pattern::Concrete),
    f("leather", &["leather", "leder"], Some([1.0, 1.0, 1.0]), 0.0, 0.6, Pattern::Leather),
    f("rust", &["rusty", "rusted", "rust", "rostig", "verrostet", "rost"], Some([1.0, 1.0, 1.0]), 1.0, 0.7, Pattern::Rust),
    Finish { name: "lava", words: &["molten lava", "lava", "magma"], color: Some([1.0, 1.0, 1.0]), metallic: 0.0, roughness: 0.85, pattern: Pattern::Lava, alpha: 1.0, emissive: 3.0 },
    f("honeycomb", &["honeycomb", "hexagon", "hexagons", "hex panels", "waben", "wabenmuster", "sechsecke"], Some([0.5, 0.52, 0.55]), 1.0, 0.45, Pattern::Hex),
    f("chrome", &["chrome", "chromed", "mirror", "chrom", "verchromt", "spiegel"], Some([0.55, 0.556, 0.554]), 1.0, 0.04, Pattern::Plain),
    f("gold", &["golden", "gold", "goldene", "goldenen", "goldener", "goldfarben"],Some([1.0, 0.766, 0.336]), 1.0, 0.22, Pattern::Plain),
    f("silver", &["silver", "silber", "silberne", "silbernen", "silberner"], Some([0.972, 0.96, 0.915]), 1.0, 0.18, Pattern::Plain),
    f("copper", &["copper", "kupfer"], Some([0.955, 0.638, 0.538]), 1.0, 0.28, Pattern::Plain),
    f("bronze", &["bronze"], Some([0.714, 0.428, 0.18]), 1.0, 0.34, Pattern::Plain),
    f("brass", &["brass", "messing"], Some([0.91, 0.78, 0.42]), 1.0, 0.3, Pattern::Plain),
    f("titanium", &["titanium", "titan"], Some([0.542, 0.497, 0.449]), 1.0, 0.32, Pattern::Plain),
    f("aluminium", &["aluminium", "aluminum", "alu"], Some([0.913, 0.922, 0.924]), 1.0, 0.3, Pattern::Plain),
    f("steel", &["stainless steel", "steel", "iron", "metal", "stahl", "eisen", "metall", "edelstahl"], Some([0.56, 0.57, 0.58]), 1.0, 0.36, Pattern::Plain),
    f("metallic", &["metallic", "metallisch", "metallic paint", "metalliclack"], None, 1.0, 0.3, Pattern::Plain),
    f("vibranium", &["vibranium"], Some([0.16, 0.14, 0.22]), 1.0, 0.25, Pattern::Plain),
    Finish { name: "glass", words: &["glass", "glas", "transparent", "see-through", "durchsichtig"], color: Some([0.9, 0.95, 1.0]), metallic: 0.0, roughness: 0.04, pattern: Pattern::Plain, alpha: 0.22, emissive: 0.0 },
    f("plastic", &["plastic", "plastik", "kunststoff"], None, 0.0, 0.42, Pattern::Plain),
    f("rubber", &["rubber", "gummi"], None, 0.0, 0.92, Pattern::Plain),
    f("matte", &["matte", "matt", "mattiert"], None, 0.0, 0.88, Pattern::Plain),
    f("glossy", &["glossy", "shiny", "polished", "glänzend", "poliert", "hochglanz"], None, -1.0, 0.1, Pattern::Plain),
    f("ceramic", &["ceramic", "porcelain", "keramik", "porzellan"], Some([0.92, 0.92, 0.9]), 0.0, 0.08, Pattern::Plain),
];

/// The finish a phrase names ("in carbon fibre", "aus Marmor"): the longest match on word
/// boundaries.
pub fn finish_in(text: &str) -> Option<&'static Finish> {
    let t = format!(" {} ", text.to_lowercase().replace(['.', ',', '!', '?', '"'], " "));
    let mut best: Option<(&'static Finish, usize)> = None;
    for fin in FINISHES {
        for w in fin.words {
            if t.contains(&format!(" {w} ")) && best.is_none_or(|(_, l)| w.len() > l) {
                best = Some((fin, w.len()));
            }
        }
    }
    best.map(|(f, _)| f)
}

pub fn finish_named(name: &str) -> Option<&'static Finish> {
    let n = name.trim().to_lowercase();
    FINISHES.iter().find(|f| f.name == n).or_else(|| finish_in(&n))
}

/// A finish's textures.
#[derive(Clone, Debug)]
pub struct TextureSet {
    pub albedo: Texture,
    pub normal: Texture,
    /// R occlusion, G roughness, B metallic.
    pub mr: Texture,
    pub emissive: Option<Texture>,
}

static SETS: LazyLock<Mutex<HashMap<(Pattern, u32), Arc<TextureSet>>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

/// The pattern's textures (made once per pattern and seed); None for a plain finish.
pub fn texture_set(pattern: Pattern, seed: u32) -> Option<Arc<TextureSet>> {
    if pattern == Pattern::Plain {
        return None;
    }
    if let Some(set) = SETS.lock().unwrap_or_else(|e| e.into_inner()).get(&(pattern, seed)) {
        return Some(Arc::clone(set));
    }
    let set = Arc::new(generate(pattern, seed, FINISH_SIZE));
    let mut sets = SETS.lock().unwrap_or_else(|e| e.into_inner());
    if sets.len() > 24 {
        sets.clear();
    }
    sets.insert((pattern, seed), Arc::clone(&set));
    Some(set)
}

// -------------------------------------------------------------------------------- noise ----

fn hash3(x: i32, y: i32, z: i32, seed: u32) -> u32 {
    let mut h = (x as u32).wrapping_mul(0x8DA6_B343) ^ (y as u32).wrapping_mul(0xD816_3841) ^ (z as u32).wrapping_mul(0xCB1A_B31F) ^ seed.wrapping_mul(0x9E37_79B9);
    h ^= h >> 15;
    h = h.wrapping_mul(0x2C1B_3C6D);
    h ^= h >> 12;
    h = h.wrapping_mul(0x297A_2D39);
    h ^= h >> 15;
    h
}

fn rnd(x: i32, y: i32, z: i32, seed: u32) -> f32 {
    (hash3(x, y, z, seed) >> 8) as f32 / (1u32 << 24) as f32
}

fn smooth(t: f32) -> f32 {
    t * t * t * (t * (t * 6.0 - 15.0) + 10.0)
}

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

fn smoothstep(e0: f32, e1: f32, x: f32) -> f32 {
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// 2D value noise with `px` × `py` lattice cells before it repeats.
fn vnoise(x: f32, y: f32, px: i32, py: i32, seed: u32) -> f32 {
    let (xf, yf) = (x.floor(), y.floor());
    let (x0, y0) = (xf as i32, yf as i32);
    let (u, v) = (smooth(x - xf), smooth(y - yf));
    let wx = |i: i32| i.rem_euclid(px.max(1));
    let wy = |i: i32| i.rem_euclid(py.max(1));
    let a = rnd(wx(x0), wy(y0), 0, seed);
    let b = rnd(wx(x0 + 1), wy(y0), 0, seed);
    let c = rnd(wx(x0), wy(y0 + 1), 0, seed);
    let d = rnd(wx(x0 + 1), wy(y0 + 1), 0, seed);
    lerp(lerp(a, b, u), lerp(c, d, u), v)
}

/// Seamless fractal noise over the unit square (0 … 1), `cells` lattice cells at the base.
fn fbm(u: f32, v: f32, cells: i32, octaves: u32, seed: u32) -> f32 {
    let (mut sum, mut amp, mut norm, mut c) = (0.0, 0.5, 0.0, cells);
    for o in 0..octaves {
        sum += amp * vnoise(u * c as f32, v * c as f32, c, c, seed.wrapping_add(o * 101));
        norm += amp;
        amp *= 0.5;
        c *= 2;
    }
    sum / norm
}

/// 3D value noise (planets: sampled on the sphere, so no seam).
fn vnoise3(p: [f32; 3], seed: u32) -> f32 {
    let f = [p[0].floor(), p[1].floor(), p[2].floor()];
    let i = [f[0] as i32, f[1] as i32, f[2] as i32];
    let t = [smooth(p[0] - f[0]), smooth(p[1] - f[1]), smooth(p[2] - f[2])];
    let c = |dx: i32, dy: i32, dz: i32| rnd(i[0] + dx, i[1] + dy, i[2] + dz, seed);
    let x00 = lerp(c(0, 0, 0), c(1, 0, 0), t[0]);
    let x10 = lerp(c(0, 1, 0), c(1, 1, 0), t[0]);
    let x01 = lerp(c(0, 0, 1), c(1, 0, 1), t[0]);
    let x11 = lerp(c(0, 1, 1), c(1, 1, 1), t[0]);
    lerp(lerp(x00, x10, t[1]), lerp(x01, x11, t[1]), t[2])
}

fn fbm3(p: [f32; 3], octaves: u32, seed: u32) -> f32 {
    let (mut sum, mut amp, mut norm, mut k) = (0.0, 0.5, 0.0, 1.0);
    for o in 0..octaves {
        sum += amp * vnoise3([p[0] * k + 17.0, p[1] * k + 31.0, p[2] * k + 7.0], seed.wrapping_add(o * 977));
        norm += amp;
        amp *= 0.5;
        k *= 2.03;
    }
    sum / norm
}

/// Seamless cellular noise: the distances to the nearest and second-nearest feature point, in
/// cells.
fn worley(u: f32, v: f32, cells: i32, seed: u32) -> (f32, f32) {
    let (x, y) = (u * cells as f32, v * cells as f32);
    let (cx, cy) = (x.floor() as i32, y.floor() as i32);
    let (mut f1, mut f2) = (9.0f32, 9.0f32);
    for dy in -1..=1 {
        for dx in -1..=1 {
            let (gx, gy) = (cx + dx, cy + dy);
            let (wx, wy) = (gx.rem_euclid(cells), gy.rem_euclid(cells));
            let px = gx as f32 + rnd(wx, wy, 1, seed);
            let py = gy as f32 + rnd(wx, wy, 2, seed);
            let d = ((px - x).powi(2) + (py - y).powi(2)).sqrt();
            if d < f1 {
                f2 = f1;
                f1 = d;
            } else if d < f2 {
                f2 = d;
            }
        }
    }
    (f1, f2)
}

// ------------------------------------------------------------------------------ patterns ----

/// One texel of a pattern: linear albedo, height (0 … 1), roughness, metallic, emissive.
#[derive(Clone, Copy, Default)]
struct Texel {
    albedo: [f32; 3],
    height: f32,
    rough: f32,
    metal: f32,
    glow: f32,
}

fn srgb(c: [u8; 3]) -> [f32; 3] {
    [srgb_to_linear(c[0] as f32 / 255.0), srgb_to_linear(c[1] as f32 / 255.0), srgb_to_linear(c[2] as f32 / 255.0)]
}

fn mix3(a: [f32; 3], b: [f32; 3], t: f32) -> [f32; 3] {
    [lerp(a[0], b[0], t), lerp(a[1], b[1], t), lerp(a[2], b[2], t)]
}

fn scale3(a: [f32; 3], k: f32) -> [f32; 3] {
    [a[0] * k, a[1] * k, a[2] * k]
}

/// The pattern at (u, v) in the unit square; seamless in both directions.
fn sample(pattern: Pattern, u: f32, v: f32, seed: u32) -> Texel {
    match pattern {
        Pattern::Plain => Texel { albedo: [1.0; 3], height: 0.5, rough: 1.0, metal: 1.0, glow: 0.0 },
        Pattern::Brushed => {
            let s = 0.6 * vnoise(u * 3.0, v * 320.0, 3, 320, seed) + 0.4 * vnoise(u * 9.0, v * 760.0, 9, 760, seed ^ 7);
            let blotch = fbm(u, v, 3, 3, seed ^ 11);
            Texel { albedo: [0.82 + 0.18 * s; 3], height: s * 0.35, rough: 0.28 + 0.16 * s + 0.08 * blotch, metal: 1.0, glow: 0.0 }
        }
        Pattern::Carbon => {
            const TOWS: f32 = 16.0;
            let (x, y) = (u * TOWS, v * TOWS);
            let (i, j) = (x.floor() as i32, y.floor() as i32);
            let (fu, fv) = (x - x.floor(), y - y.floor());
            // 2×2 twill: over two, under two, shifted by one each row.
            let along_u = (i - j).rem_euclid(4) < 2;
            let (across, along) = if along_u { (fv, fu) } else { (fu, fv) };
            let bulge = (PI * across).sin().powf(0.6);
            let fibre = vnoise(if along_u { u * 24.0 } else { u * 900.0 }, if along_u { v * 900.0 } else { v * 24.0 }, if along_u { 24 } else { 900 }, if along_u { 900 } else { 24 }, seed);
            let _ = along;
            let sheen = if along_u { 0.055 } else { 0.03 };
            let a = sheen * (0.55 + 0.45 * bulge) * (0.85 + 0.3 * fibre);
            Texel { albedo: [a, a, a * 1.05], height: 0.5 * bulge + 0.04 * fibre, rough: 0.22 + 0.12 * (1.0 - bulge), metal: 0.0, glow: 0.0 }
        }
        Pattern::Marble => {
            let turb = fbm(u, v, 4, 6, seed);
            let t = 4.0 * u + 2.0 * v + 5.0 * turb;
            let vein = 1.0 - (PI * t).sin().abs().powf(0.18);
            let fine = 1.0 - (PI * (6.0 * u - 4.0 * v + 8.0 * fbm(u, v, 8, 5, seed ^ 3))).sin().abs().powf(0.08);
            let cloud = fbm(u, v, 2, 4, seed ^ 5);
            let base = mix3(srgb([238, 236, 230]), srgb([214, 212, 208]), cloud);
            let a = mix3(mix3(base, srgb([96, 98, 104]), (vein * 1.1).min(1.0)), srgb([150, 146, 140]), fine * 0.5);
            Texel { albedo: a, height: 0.5 - 0.15 * vein, rough: 0.12 + 0.12 * vein, metal: 0.0, glow: 0.0 }
        }
        Pattern::Wood => {
            let warp = fbm(u, v, 3, 4, seed);
            let t = v * 9.0 + 2.2 * warp + 0.25 * (TAU * u * 2.0).sin();
            let ring = t - t.floor();
            let late = smoothstep(0.55, 0.95, ring) * (1.0 - smoothstep(0.95, 1.0, ring));
            let grain = vnoise(u * 6.0, v * 420.0, 6, 420, seed ^ 9);
            let pores = (vnoise(u * 60.0, v * 900.0, 60, 900, seed ^ 13) - 0.6).max(0.0) * 2.0;
            let light = srgb([186, 128, 74]);
            let dark = srgb([112, 62, 30]);
            let a = scale3(mix3(light, dark, (late * 0.85 + 0.15 * grain).min(1.0)), 1.0 - 0.25 * pores);
            Texel { albedo: a, height: 0.5 - 0.2 * late - 0.25 * pores, rough: 0.55 + 0.15 * late + 0.15 * pores, metal: 0.0, glow: 0.0 }
        }
        Pattern::Camo => {
            let a = fbm(u, v, 3, 5, seed);
            let b = fbm(u, v, 4, 5, seed ^ 0x55);
            let c = fbm(u, v, 5, 4, seed ^ 0xAA);
            let colour = if c > 0.6 {
                srgb([28, 27, 24])
            } else if a > 0.54 {
                srgb([92, 70, 46])
            } else if b > 0.5 {
                srgb([44, 62, 36])
            } else {
                srgb([104, 110, 66])
            };
            let weave = 0.5 + 0.5 * ((TAU * u * 128.0).sin() * (TAU * v * 128.0).sin());
            Texel { albedo: scale3(colour, 0.92 + 0.08 * weave), height: 0.5 + 0.1 * weave, rough: 0.82 + 0.1 * weave, metal: 0.0, glow: 0.0 }
        }
        Pattern::Concrete => {
            let n = fbm(u, v, 6, 6, seed);
            let (f1, _) = worley(u, v, 40, seed ^ 3);
            let pit = 1.0 - smoothstep(0.0, 0.12, f1);
            let blotch = fbm(u, v, 2, 3, seed ^ 4);
            let g = 0.32 + 0.16 * n + 0.06 * blotch - 0.12 * pit;
            Texel { albedo: [g, g * 0.99, g * 0.96], height: 0.5 + 0.3 * (n - 0.5) - 0.35 * pit, rough: 0.86 + 0.1 * pit, metal: 0.0, glow: 0.0 }
        }
        Pattern::Leather => {
            let (f1, f2) = worley(u, v, 48, seed);
            let crease = 1.0 - smoothstep(0.0, 0.16, f2 - f1);
            let tone = fbm(u, v, 4, 4, seed ^ 6);
            let a = scale3(mix3(srgb([92, 52, 28]), srgb([70, 38, 20]), tone), 1.0 - 0.35 * crease);
            Texel { albedo: a, height: 0.6 - 0.45 * crease + 0.1 * f1, rough: 0.52 + 0.2 * crease, metal: 0.0, glow: 0.0 }
        }
        Pattern::Rust => {
            let m = fbm(u, v, 4, 6, seed);
            let fine = fbm(u, v, 32, 3, seed ^ 2);
            let rust = smoothstep(0.42, 0.56, m + 0.15 * (fine - 0.5));
            let metal = srgb([150, 150, 152]);
            let oxide = mix3(srgb([120, 52, 22]), srgb([70, 30, 14]), fine);
            Texel { albedo: mix3(metal, oxide, rust), height: 0.5 + 0.3 * rust * fine, rough: lerp(0.35, 0.92, rust), metal: 1.0 - rust, glow: 0.0 }
        }
        Pattern::Lava => {
            let (f1, f2) = worley(u, v, 10, seed);
            let crack = 1.0 - smoothstep(0.0, 0.09, f2 - f1);
            let heat = fbm(u, v, 6, 4, seed ^ 8);
            let rock = mix3(srgb([30, 26, 24]), srgb([58, 50, 46]), heat);
            let hot = crack * (0.6 + 0.4 * heat);
            Texel { albedo: mix3(rock, srgb([255, 120, 20]), hot * 0.8), height: 0.6 - 0.5 * crack, rough: 0.85 - 0.3 * crack, metal: 0.0, glow: hot }
        }
        Pattern::Hex => {
            // 26 columns × 15 rows of √3 height: square to within 0.1 %, so seamless.
            const COLS: f32 = 26.0;
            const ROWS: f32 = 15.0;
            let s3 = 3f32.sqrt();
            let (x, y) = (u * COLS, v * ROWS * s3);
            let hexd = |cx: f32, cy: f32| {
                let (dx, dy) = ((x - cx).abs(), (y - cy).abs());
                (dx * 0.5 + dy * s3 * 0.5).max(dx)
            };
            let (ia, ja) = ((x + 0.5).floor(), (y / s3 + 0.5).floor() * s3);
            let (ib, jb) = (x.floor() + 0.5, (y / s3).floor() * s3 + s3 * 0.5);
            let (da, db) = (hexd(ia, ja), hexd(ib, jb));
            let (d, cx, cy) = if da < db { (da, ia, ja) } else { (db, ib, jb) };
            let edge = smoothstep(0.40, 0.47, d);
            let id = rnd(((cx * 2.0).round() as i32).rem_euclid(COLS as i32 * 2), ((cy / s3 * 2.0).round() as i32).rem_euclid(ROWS as i32 * 2), 3, seed);
            let tone = 0.8 + 0.2 * id;
            Texel { albedo: [tone * (1.0 - 0.6 * edge); 3], height: 0.7 - 0.6 * edge, rough: 0.35 + 0.25 * edge + 0.1 * id, metal: 1.0 - 0.7 * edge, glow: 0.0 }
        }
    }
}

/// Runs `row(y, out_row)` for every row on every core.
fn rows_parallel<T: Send + Default + Clone>(width: u32, height: u32, row: impl Fn(u32, &mut [T]) + Sync) -> Vec<T> {
    let mut out = vec![T::default(); (width * height) as usize];
    let workers = std::thread::available_parallelism().map_or(4, |n| n.get()).clamp(1, 12);
    let per = (height as usize).div_ceil(workers).max(1);
    std::thread::scope(|s| {
        for (k, chunk) in out.chunks_mut(per * width as usize).enumerate() {
            let row = &row;
            s.spawn(move || {
                for (r, line) in chunk.chunks_mut(width as usize).enumerate() {
                    row((k * per + r) as u32, line);
                }
            });
        }
    });
    out
}

fn to_byte(c: f32) -> u8 {
    (c.clamp(0.0, 1.0) * 255.0 + 0.5) as u8
}

fn srgb_byte(c: f32) -> u8 {
    to_byte(crate::mesh_io::linear_to_srgb(c))
}

/// A normal map (tangent space, +Z out) from a height field that wraps (`wrap_x`/`wrap_y`) or
/// clamps at its edges.
fn normal_map(height: &[f32], w: u32, h: u32, strength: f32, wrap_y: bool) -> Texture {
    let (wi, hi) = (w as i64, h as i64);
    let at = |x: i64, y: i64| {
        let x = x.rem_euclid(wi);
        let y = if wrap_y { y.rem_euclid(hi) } else { y.clamp(0, hi - 1) };
        height[(y * wi + x) as usize]
    };
    let mut rgba = vec![0u8; (w * h * 4) as usize];
    for y in 0..hi {
        for x in 0..wi {
            // Sobel.
            let dx = (at(x + 1, y - 1) + 2.0 * at(x + 1, y) + at(x + 1, y + 1)) - (at(x - 1, y - 1) + 2.0 * at(x - 1, y) + at(x - 1, y + 1));
            let dy = (at(x - 1, y + 1) + 2.0 * at(x, y + 1) + at(x + 1, y + 1)) - (at(x - 1, y - 1) + 2.0 * at(x, y - 1) + at(x + 1, y - 1));
            let n = [-dx * strength, dy * strength, 1.0];
            let l = (n[0] * n[0] + n[1] * n[1] + 1.0).sqrt();
            let i = ((y * wi + x) * 4) as usize;
            rgba[i] = to_byte(n[0] / l * 0.5 + 0.5);
            rgba[i + 1] = to_byte(n[1] / l * 0.5 + 0.5);
            rgba[i + 2] = to_byte(n[2] / l * 0.5 + 0.5);
            rgba[i + 3] = 255;
        }
    }
    Texture::new(w, h, rgba)
}

fn normal_strength(p: Pattern) -> f32 {
    match p {
        Pattern::Carbon => 2.2,
        Pattern::Brushed => 0.9,
        Pattern::Marble => 0.6,
        Pattern::Wood => 1.4,
        Pattern::Camo => 0.8,
        Pattern::Concrete => 2.6,
        Pattern::Leather => 3.0,
        Pattern::Rust => 2.0,
        Pattern::Lava => 3.2,
        Pattern::Hex => 3.0,
        Pattern::Plain => 0.0,
    }
}

/// A finish's textures, `size` texels a side (seamless, the same for the same seed).
pub fn generate(pattern: Pattern, seed: u32, size: u32) -> TextureSet {
    let size = size.max(4);
    let texels: Vec<Texel> = rows_parallel(size, size, |y, line| {
        let v = y as f32 / size as f32;
        for (x, t) in line.iter_mut().enumerate() {
            *t = sample(pattern, x as f32 / size as f32, v, seed);
        }
    });
    let n = texels.len();
    let (mut albedo, mut mr) = (vec![0u8; n * 4], vec![0u8; n * 4]);
    let mut glow = vec![0u8; n * 4];
    let any_glow = texels.iter().any(|t| t.glow > 0.0);
    for (i, t) in texels.iter().enumerate() {
        albedo[i * 4..i * 4 + 4].copy_from_slice(&[srgb_byte(t.albedo[0]), srgb_byte(t.albedo[1]), srgb_byte(t.albedo[2]), 255]);
        let ao = 0.75 + 0.25 * t.height.clamp(0.0, 1.0);
        mr[i * 4..i * 4 + 4].copy_from_slice(&[to_byte(ao), to_byte(t.rough), to_byte(t.metal), 255]);
        if any_glow {
            let g = t.glow.clamp(0.0, 1.0);
            glow[i * 4..i * 4 + 4].copy_from_slice(&[srgb_byte(g), srgb_byte(g * 0.42), srgb_byte(g * 0.08), 255]);
        }
    }
    let heights: Vec<f32> = texels.iter().map(|t| t.height).collect();
    TextureSet {
        albedo: Texture::new(size, size, albedo),
        normal: normal_map(&heights, size, size, normal_strength(pattern) * size as f32 / 512.0, true),
        mr: Texture::new(size, size, mr),
        emissive: any_glow.then(|| Texture::new(size, size, glow)),
    }
}

// ------------------------------------------------------------------------------- planets ----

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Planet {
    Earth,
    Moon,
    Mercury,
    Venus,
    Mars,
    Jupiter,
    Saturn,
    Uranus,
    Neptune,
    Pluto,
    Sun,
}

/// A planet's maps: albedo, normal (relief), packed occlusion/roughness/metallic, emissive.
#[derive(Clone, Debug)]
pub struct PlanetMaps {
    pub albedo: Texture,
    pub normal: Option<Texture>,
    pub mr: Texture,
    pub emissive: Option<Texture>,
}

/// The direction on the unit sphere of the equirectangular texel (u, v): u = 0.5 is longitude 0
/// (facing +Z), v = 0 the north pole.
pub fn sphere_dir(u: f32, v: f32) -> [f32; 3] {
    let lon = (u - 0.5) * TAU;
    let lat = (0.5 - v) * PI;
    [lat.cos() * lon.sin(), lat.sin(), lat.cos() * lon.cos()]
}

/// Craters added to a height field: `count` of them, the larger ones rarer.
fn craters(height: &mut [f32], w: u32, h: u32, count: u32, max_radius: f32, seed: u32) {
    for k in 0..count {
        let (a, b, c) = (rnd(k as i32, 0, 5, seed), rnd(k as i32, 1, 5, seed), rnd(k as i32, 2, 5, seed));
        let centre = sphere_dir(a, (1.0 - 2.0 * b).acos() / PI);
        let r = max_radius * (0.04 + 0.96 * c.powi(4));
        let lat = centre[1].asin();
        let (u0, v0) = (centre[0].atan2(centre[2]) / TAU + 0.5, 0.5 - lat / PI);
        let reach = r * 1.6;
        let dv = reach / PI;
        let du = if lat.cos() > 0.2 { reach / (TAU * (lat.cos() - reach.min(0.9)).max(0.1)) } else { 0.5 };
        let (y0, y1) = (((v0 - dv) * h as f32).floor().max(0.0) as i64, ((v0 + dv) * h as f32).ceil().min(h as f32 - 1.0) as i64);
        let span = (du * w as f32).ceil() as i64;
        let xc = (u0 * w as f32) as i64;
        let (x0, x1) = if span * 2 >= w as i64 { (0, w as i64 - 1) } else { (xc - span, xc + span) };
        for y in y0..=y1 {
            for x in x0..=x1 {
                let xw = x.rem_euclid(w as i64);
                let d = sphere_dir((xw as f32 + 0.5) / w as f32, (y as f32 + 0.5) / h as f32);
                let dist = (d[0] * centre[0] + d[1] * centre[1] + d[2] * centre[2]).clamp(-1.0, 1.0).acos();
                if dist > reach {
                    continue;
                }
                let t = dist / r;
                let bowl = if t < 1.0 { -(1.0 - t * t) * 0.9 } else { 0.0 };
                let rim = 0.45 * (-((t - 1.0) / 0.22).powi(2)).exp();
                height[(y as u32 * w + xw as u32) as usize] += (bowl + rim) * (0.6 + 0.4 * c) * 0.25;
            }
        }
    }
}

/// A planet's maps at `width` × `width / 2`.
pub fn planet(p: Planet, width: u32) -> PlanetMaps {
    let w = width.max(16);
    let h = w / 2;
    let seed = p as u32 * 7919 + 13;
    // Pass 1: height (relief) and a tone per texel, from 3D noise on the sphere.
    let field: Vec<(f32, [f32; 3], f32, f32)> = rows_parallel(w, h, |y, line| {
        let v = (y as f32 + 0.5) / h as f32;
        let lat = (0.5 - v) * PI;
        for (x, out) in line.iter_mut().enumerate() {
            let d = sphere_dir((x as f32 + 0.5) / w as f32, v);
            *out = planet_texel(p, d, lat, seed);
        }
    });
    let mut height: Vec<f32> = field.iter().map(|t| t.0).collect();
    let (crater_count, crater_size) = match p {
        Planet::Moon => (650, 0.16),
        Planet::Mercury => (900, 0.12),
        Planet::Mars => (220, 0.1),
        Planet::Pluto => (60, 0.08),
        _ => (0, 0.0),
    };
    if crater_count > 0 {
        craters(&mut height, w, h, crater_count, crater_size, seed);
    }
    let mut albedo = vec![0u8; (w * h * 4) as usize];
    let mut mr = vec![0u8; (w * h * 4) as usize];
    let mut glow = if p == Planet::Sun { vec![0u8; (w * h * 4) as usize] } else { Vec::new() };
    for (i, (base_h, colour, rough, emit)) in field.iter().enumerate() {
        // Crater floors darker, rims brighter.
        let shade = if crater_count > 0 { (1.0 + (height[i] - base_h) * 1.2).clamp(0.55, 1.35) } else { 1.0 };
        let c = scale3(*colour, shade);
        albedo[i * 4..i * 4 + 4].copy_from_slice(&[srgb_byte(c[0]), srgb_byte(c[1]), srgb_byte(c[2]), 255]);
        mr[i * 4..i * 4 + 4].copy_from_slice(&[255, to_byte(*rough), 0, 255]);
        if p == Planet::Sun {
            glow[i * 4..i * 4 + 4].copy_from_slice(&[srgb_byte(c[0] * emit), srgb_byte(c[1] * emit), srgb_byte(c[2] * emit), 255]);
        }
    }
    let relief = match p {
        Planet::Moon | Planet::Mercury => Some(9.0),
        Planet::Mars | Planet::Pluto => Some(6.0),
        Planet::Earth => Some(2.0),
        _ => None,
    };
    PlanetMaps {
        albedo: Texture::new(w, h, albedo),
        normal: relief.map(|k| normal_map(&height, w, h, k * w as f32 / 1024.0, false)),
        mr: Texture::new(w, h, mr),
        emissive: (p == Planet::Sun).then(|| Texture::new(w, h, glow)),
    }
}

/// (height, linear colour, roughness, emissive) of one planet texel.
fn planet_texel(p: Planet, d: [f32; 3], lat: f32, seed: u32) -> (f32, [f32; 3], f32, f32) {
    let s = |k: f32| [d[0] * k, d[1] * k, d[2] * k];
    let alat = lat.abs().to_degrees();
    match p {
        Planet::Earth => {
            let land = fbm3(s(1.7), 7, seed);
            let coast = 0.515;
            let cloud = fbm3([d[0] * 3.0, d[1] * 6.0, d[2] * 3.0], 6, seed ^ 99);
            let cloud = smoothstep(0.52, 0.7, cloud) * 0.85;
            let (mut c, rough, hgt) = if land < coast {
                let depth = smoothstep(coast - 0.12, coast, land);
                (mix3(srgb([8, 24, 66]), srgb([22, 84, 132]), depth), 0.32, 0.4)
            } else {
                let dry = smoothstep(0.35, 0.65, fbm3(s(3.0), 4, seed ^ 7)) * (1.0 - smoothstep(20.0, 45.0, alat));
                let green = mix3(srgb([46, 92, 40]), srgb([94, 104, 58]), smoothstep(40.0, 65.0, alat));
                let ground = mix3(green, srgb([196, 164, 112]), dry);
                let mountain = smoothstep(coast + 0.08, coast + 0.2, land);
                (mix3(ground, srgb([120, 108, 96]), mountain * 0.6), 0.88, 0.5 + (land - coast) * 2.0)
            };
            let ice = smoothstep(68.0, 74.0, alat + 6.0 * (fbm3(s(4.0), 3, seed ^ 3) - 0.5));
            c = mix3(c, srgb([236, 242, 248]), ice);
            c = mix3(c, srgb([250, 250, 252]), cloud);
            (hgt, c, lerp(rough, 0.9, cloud.max(ice)), 0.0)
        }
        Planet::Moon | Planet::Mercury => {
            let maria = smoothstep(0.42, 0.52, fbm3(s(1.4), 5, seed));
            let n = fbm3(s(6.0), 5, seed ^ 4);
            let (bright, dark) = if p == Planet::Moon { (srgb([170, 168, 162]), srgb([88, 88, 90])) } else { (srgb([168, 156, 144]), srgb([110, 100, 92])) };
            let c = scale3(mix3(bright, dark, maria * 0.85), 0.85 + 0.3 * n);
            (0.5 + 0.2 * (n - 0.5) - 0.1 * maria, c, 0.95, 0.0)
        }
        Planet::Mars => {
            let n = fbm3(s(2.0), 6, seed);
            let dark = smoothstep(0.48, 0.6, fbm3(s(1.3), 5, seed ^ 2));
            let mut c = scale3(mix3(srgb([196, 104, 58]), srgb([110, 56, 36]), dark * 0.8), 0.85 + 0.3 * n);
            let cap = smoothstep(78.0, 83.0, alat + 4.0 * (n - 0.5));
            c = mix3(c, srgb([240, 236, 230]), cap);
            (0.5 + 0.3 * (n - 0.5), c, 0.92, 0.0)
        }
        Planet::Venus => {
            let swirl = fbm3([d[0] * 2.0, d[1] * 7.0, d[2] * 2.0], 6, seed);
            let c = mix3(srgb([226, 196, 140]), srgb([186, 150, 96]), swirl);
            (0.5, c, 0.9, 0.0)
        }
        Planet::Jupiter | Planet::Saturn => {
            let turb = fbm3([d[0] * 4.0, d[1] * 16.0, d[2] * 4.0], 5, seed);
            let bands = (lat * if p == Planet::Jupiter { 13.0 } else { 9.0 } + 1.6 * turb).sin() * 0.5 + 0.5;
            let fine = fbm3([d[0] * 8.0, d[1] * 40.0, d[2] * 8.0], 4, seed ^ 5);
            let mut c = if p == Planet::Jupiter {
                let a = mix3(srgb([232, 220, 196]), srgb([170, 118, 80]), bands);
                mix3(a, srgb([200, 168, 130]), fine * 0.4)
            } else {
                let a = mix3(srgb([236, 222, 182]), srgb([198, 172, 120]), bands * 0.8);
                mix3(a, srgb([214, 194, 150]), fine * 0.3)
            };
            if p == Planet::Jupiter {
                // The Great Red Spot.
                let lon = d[0].atan2(d[2]);
                let (dl, dt) = ((lon - 0.7) / 0.22, (lat - (-22f32).to_radians()) / 0.09);
                let e = dl * dl + dt * dt;
                if e < 1.6 {
                    let k = 1.0 - smoothstep(0.6, 1.6, e);
                    c = mix3(c, srgb([196, 92, 60]), k * (0.75 + 0.25 * fine));
                }
            }
            (0.5, c, 0.9, 0.0)
        }
        Planet::Uranus => {
            let bands = (lat * 6.0).sin() * 0.5 + 0.5;
            (0.5, mix3(srgb([176, 222, 230]), srgb([150, 204, 216]), bands * 0.4), 0.9, 0.0)
        }
        Planet::Neptune => {
            let turb = fbm3([d[0] * 3.0, d[1] * 12.0, d[2] * 3.0], 5, seed);
            let bands = (lat * 8.0 + 1.2 * turb).sin() * 0.5 + 0.5;
            let mut c = mix3(srgb([66, 108, 210]), srgb([44, 74, 170]), bands);
            let lon = d[0].atan2(d[2]);
            let (dl, dt) = ((lon + 0.5) / 0.25, (lat - (-20f32).to_radians()) / 0.08);
            if dl * dl + dt * dt < 1.0 {
                c = scale3(c, 0.55);
            }
            (0.5, c, 0.9, 0.0)
        }
        Planet::Pluto => {
            let n = fbm3(s(2.5), 6, seed);
            let mut c = mix3(srgb([206, 184, 152]), srgb([118, 84, 62]), smoothstep(0.45, 0.6, fbm3(s(1.2), 5, seed ^ 8)));
            // Tombaugh Regio, the bright "heart".
            let lon = d[0].atan2(d[2]);
            let (dl, dt) = ((lon - 2.9) / 0.5, (lat - 0.25) / 0.4);
            if dl * dl + dt * dt < 1.0 {
                c = mix3(c, srgb([238, 228, 212]), 0.85);
            }
            (0.5 + 0.2 * (n - 0.5), scale3(c, 0.9 + 0.2 * n), 0.9, 0.0)
        }
        Planet::Sun => {
            let gran = fbm3(s(22.0), 4, seed);
            let big = fbm3(s(3.0), 4, seed ^ 3);
            let c = mix3(srgb([255, 150, 30]), srgb([255, 226, 140]), gran * 0.7 + big * 0.3);
            (0.5, c, 1.0, 2.2)
        }
    }
}

/// Saturn's rings as a strip: x runs from the inner edge (0) to the outer (1); alpha is the
/// ring's density (the Cassini division nearly empty).
pub fn saturn_rings(width: u32) -> Texture {
    let w = width.max(16);
    let h = 4u32;
    let mut rgba = vec![0u8; (w * h * 4) as usize];
    for x in 0..w {
        let r = x as f32 / (w - 1) as f32;
        let n = vnoise(r * 180.0, 0.5, 4096, 1, 77) * 0.5 + vnoise(r * 600.0, 0.5, 4096, 1, 78) * 0.5;
        let mut density = 0.35 + 0.6 * n;
        if r < 0.12 {
            density *= 0.25; // C ring
        }
        if (0.52..0.58).contains(&r) {
            density *= 0.06; // Cassini division
        }
        if (0.86..0.875).contains(&r) {
            density *= 0.1; // Encke gap
        }
        density *= smoothstep(0.0, 0.03, r) * (1.0 - smoothstep(0.97, 1.0, r));
        let c = mix3(srgb([200, 180, 150]), srgb([236, 222, 196]), n);
        for y in 0..h {
            let i = ((y * w + x) * 4) as usize;
            rgba[i..i + 4].copy_from_slice(&[srgb_byte(c[0]), srgb_byte(c[1]), srgb_byte(c[2]), to_byte(density)]);
        }
    }
    Texture::new(w, h, rgba)
}

/// The Blue Marble cached by the Earth panel, ready as Earth maps with shiny oceans; None when
/// it was never fetched.
pub fn earth_from_photo(bytes: &[u8]) -> Option<PlanetMaps> {
    let albedo = crate::mesh_io::decode_texture(bytes)?;
    let (w, h) = (albedo.width, albedo.height);
    let mut mr = vec![0u8; (w * h * 4) as usize];
    for (i, px) in albedo.rgba.chunks_exact(4).enumerate() {
        let (r, g, b) = (px[0] as f32, px[1] as f32, px[2] as f32);
        // Water: blue clearly above red and green, and dark.
        let water = b > r * 1.15 && b > g * 0.95 && (r + g + b) < 400.0;
        mr[i * 4..i * 4 + 4].copy_from_slice(&[255, if water { 80 } else { 225 }, 0, 255]);
    }
    Some(PlanetMaps { albedo, normal: None, mr: Texture::new(w, h, mr), emissive: None })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finishes_are_found_by_their_words_in_both_languages() {
        assert_eq!(finish_in("make it carbon fibre").map(|f| f.name), Some("carbon fibre"));
        assert_eq!(finish_in("mach es aus Kohlefaser").map(|f| f.name), Some("carbon fibre"));
        assert_eq!(finish_in("brushed steel please").map(|f| f.name), Some("brushed steel"), "the longer phrase wins over 'steel'");
        assert_eq!(finish_in("in Marmor").map(|f| f.name), Some("marble"));
        assert_eq!(finish_in("give it a camo texture").map(|f| f.name), Some("camouflage"));
        assert_eq!(finish_in("make it golden").map(|f| f.name), Some("gold"));
        assert_eq!(finish_in("make it glass").map(|f| f.name), Some("glass"));
        assert_eq!(finish_in("make it red"), None);
        assert_eq!(finish_in("goldfish"), None, "whole words only");
        for fin in FINISHES {
            assert!(finish_named(fin.name).is_some_and(|f| std::ptr::eq(f, fin)), "{}", fin.name);
        }
    }

    #[test]
    fn patterns_are_deterministic_and_seamless() {
        for p in [Pattern::Brushed, Pattern::Carbon, Pattern::Marble, Pattern::Wood, Pattern::Camo, Pattern::Concrete, Pattern::Leather, Pattern::Rust, Pattern::Lava, Pattern::Hex] {
            let a = generate(p, 7, 64);
            let b = generate(p, 7, 64);
            assert_eq!(a.albedo.rgba, b.albedo.rgba, "{p:?} is the same every time");
            assert_eq!(a.normal.rgba, b.normal.rgba);
            assert_eq!(a.mr.rgba, b.mr.rgba);
            let c = generate(p, 8, 64);
            assert_ne!(a.albedo.rgba, c.albedo.rgba, "{p:?} depends on its seed");
            // The pattern at u = 1 (and v = 1) is the pattern at 0.
            for k in 0..32 {
                let t = k as f32 / 32.0;
                for (s0, s1) in [(sample(p, 0.0, t, 7), sample(p, 1.0, t, 7)), (sample(p, t, 0.0, 7), sample(p, t, 1.0, 7))] {
                    let d = (0..3).map(|i| (s0.albedo[i] - s1.albedo[i]).abs()).fold(0.0f32, f32::max).max((s0.height - s1.height).abs());
                    assert!(d < 0.02, "{p:?} has a seam at {t}: {d}");
                }
            }
            assert_eq!(a.emissive.is_some(), p == Pattern::Lava);
        }
    }

    #[test]
    fn normal_maps_point_out_and_planets_have_their_look() {
        let set = generate(Pattern::Hex, 1, 64);
        let mean_z: f32 = set.normal.rgba.chunks_exact(4).map(|p| p[2] as f32 / 255.0).sum::<f32>() / (64.0 * 64.0);
        assert!(mean_z > 0.85, "mostly facing out: {mean_z}");
        let moon = planet(Planet::Moon, 128);
        assert_eq!((moon.albedo.width, moon.albedo.height), (128, 64));
        assert!(moon.normal.is_some() && moon.emissive.is_none());
        let avg = moon.albedo.average();
        assert!(avg[0].abs_diff(avg[2]) < 20, "the Moon is grey: {avg:?}");
        let mars = planet(Planet::Mars, 128).albedo.average();
        assert!(mars[0] > mars[2] + 30, "Mars is red: {mars:?}");
        let sun = planet(Planet::Sun, 64);
        assert!(sun.emissive.is_some());
        let earth = planet(Planet::Earth, 128).albedo.average();
        assert!(earth[2] > earth[0], "the Earth is mostly ocean: {earth:?}");
        assert_eq!(planet(Planet::Jupiter, 64).albedo.rgba, planet(Planet::Jupiter, 64).albedo.rgba);
        let rings = saturn_rings(256);
        let cassini = rings.rgba[((256.0 * 0.55) as usize) * 4 + 3];
        let b_ring = rings.rgba[((256.0 * 0.4) as usize) * 4 + 3];
        assert!(cassini < b_ring / 3, "the Cassini division is nearly empty: {cassini} vs {b_ring}");
    }
}
