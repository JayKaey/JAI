//! 3D model files for the 3D studio (`model3d`): glTF 2.0 (`.gltf` with its `.bin` and images,
//! `.glb`, embedded data URIs), OBJ (with its MTL) and STL (ASCII and binary) are read into one
//! `Model`; STL, OBJ and GLB are written back out at real-world size. ZIP archives (Sketchfab's
//! glTF download, a model Sir unpacked nowhere) are opened with a small inflater of our own.
//!
//! No new crates: serde_json reads the glTF JSON, `image` decodes the textures, `base64` the
//! data URIs. Everything here is plain CPU work on a worker thread: no GPU, no network.

use serde_json::{json, Value};
use std::borrow::Cow;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

/// Textures larger than this on a side are scaled down as they load (VRAM and load time).
pub const MAX_TEXTURE_SIDE: u32 = 2048;
/// The most triangles a model may have (the studio promises about two million).
pub const MAX_TRIANGLES: usize = 4_000_000;
/// Above this many parts, parts that share a material are merged (one draw call each).
const MAX_PARTS: usize = 1500;

/// How far a load is (0 … 1), for the studio's progress bar.
pub type Progress<'a> = &'a (dyn Fn(f32) + Sync);

// ------------------------------------------------------------------------------ the model ----

/// An RGBA8 image, top row first.
#[derive(Clone, Debug)]
pub struct Texture {
    pub width: u32,
    pub height: u32,
    pub rgba: Arc<Vec<u8>>,
}

impl Texture {
    pub fn new(width: u32, height: u32, rgba: Vec<u8>) -> Self {
        Texture { width, height, rgba: Arc::new(rgba) }
    }

    /// The image's average colour (sRGB bytes), for a format that cannot carry the texture.
    pub fn average(&self) -> [u8; 3] {
        let px = self.rgba.len() / 4;
        if px == 0 {
            return [204, 204, 204];
        }
        let step = (px / 4096).max(1);
        let (mut sum, mut n) = ([0u64; 3], 0u64);
        for i in (0..px).step_by(step) {
            for c in 0..3 {
                sum[c] += self.rgba[i * 4 + c] as u64;
            }
            n += 1;
        }
        [(sum[0] / n) as u8, (sum[1] / n) as u8, (sum[2] / n) as u8]
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum AlphaMode {
    Opaque,
    /// Cut out below this alpha.
    Mask(f32),
    Blend,
}

/// A physically based material (glTF's metallic-roughness model). Colours are linear.
#[derive(Clone, Debug, PartialEq)]
pub struct Material {
    pub name: String,
    pub base_color: [f32; 4],
    pub metallic: f32,
    pub roughness: f32,
    pub emissive: [f32; 3],
    pub base_color_tex: Option<usize>,
    /// glTF packing: G = roughness, B = metallic.
    pub metallic_roughness_tex: Option<usize>,
    pub normal_tex: Option<usize>,
    pub normal_scale: f32,
    pub emissive_tex: Option<usize>,
    pub occlusion_tex: Option<usize>,
    pub alpha: AlphaMode,
    pub double_sided: bool,
    /// The textures are projected from three sides (procedural finishes on any model, with or
    /// without UVs) instead of read through the model's UVs.
    pub triplanar: bool,
    /// How many times a procedural texture repeats across the model's largest side.
    pub tile: f32,
    /// The parts' vertex colours tint the base colour (a model rebuilt from a photo); paint
    /// covers them.
    pub vertex_colors: bool,
}

impl Default for Material {
    fn default() -> Self {
        Material {
            name: "default".into(),
            base_color: [0.8, 0.8, 0.8, 1.0],
            metallic: 0.0,
            roughness: 0.5,
            emissive: [0.0; 3],
            base_color_tex: None,
            metallic_roughness_tex: None,
            normal_tex: None,
            normal_scale: 1.0,
            emissive_tex: None,
            occlusion_tex: None,
            alpha: AlphaMode::Opaque,
            double_sided: false,
            triplanar: false,
            tile: 1.0,
            vertex_colors: false,
        }
    }
}

/// One named piece of a model with one material ("helmet", "left arm").
#[derive(Clone, Debug, Default)]
pub struct Part {
    pub name: String,
    pub positions: Vec<[f32; 3]>,
    pub normals: Vec<[f32; 3]>,
    /// Empty when the part has none.
    pub uvs: Vec<[f32; 2]>,
    /// xyz + handedness; empty when the part has none.
    pub tangents: Vec<[f32; 4]>,
    /// Linear RGB per vertex (used where the material says `vertex_colors`); empty when none.
    pub colors: Vec<[f32; 3]>,
    pub indices: Vec<u32>,
    pub material: usize,
}

impl Part {
    pub fn triangles(&self) -> usize {
        self.indices.len() / 3
    }

    pub fn bounds(&self) -> Bounds {
        let mut b = Bounds::empty();
        for p in &self.positions {
            b.add(*p);
        }
        b
    }

    /// The part's surface area (model units²).
    pub fn area(&self) -> f32 {
        self.indices
            .chunks_exact(3)
            .map(|t| {
                let (a, b, c) = (self.positions[t[0] as usize], self.positions[t[1] as usize], self.positions[t[2] as usize]);
                0.5 * math::length(math::cross(math::sub(b, a), math::sub(c, a)))
            })
            .sum()
    }
}

/// Where a downloaded model came from, for its credit line.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Credit {
    pub title: String,
    pub author: String,
    pub license: String,
    /// "Sketchfab", "NASA 3D Resources", "Khronos glTF Sample Assets".
    pub source: String,
    pub url: String,
}

impl Credit {
    /// "Iron Man Mark 7" by Someone · CC Attribution · Sketchfab
    pub fn line(&self) -> String {
        let mut parts = vec![format!("\"{}\"", self.title)];
        if !self.author.is_empty() {
            parts[0] = format!("\"{}\" by {}", self.title, self.author);
        }
        if !self.license.is_empty() {
            parts.push(self.license.clone());
        }
        if !self.source.is_empty() {
            parts.push(self.source.clone());
        }
        parts.join(" · ")
    }
}

#[derive(Clone, Debug, Default)]
pub struct Model {
    pub name: String,
    pub parts: Vec<Part>,
    pub materials: Vec<Material>,
    pub textures: Vec<Texture>,
    /// Metres per model unit (glTF: 1, STL: 0.001).
    pub unit_m: f32,
    pub credit: Option<Credit>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Bounds {
    pub min: [f32; 3],
    pub max: [f32; 3],
}

impl Bounds {
    pub fn empty() -> Self {
        Bounds { min: [f32::MAX; 3], max: [f32::MIN; 3] }
    }

    pub fn add(&mut self, p: [f32; 3]) {
        for i in 0..3 {
            if p[i].is_finite() {
                self.min[i] = self.min[i].min(p[i]);
                self.max[i] = self.max[i].max(p[i]);
            }
        }
    }

    pub fn union(&mut self, other: &Bounds) {
        if other.is_valid() {
            self.add(other.min);
            self.add(other.max);
        }
    }

    pub fn is_valid(&self) -> bool {
        (0..3).all(|i| self.min[i] <= self.max[i])
    }

    pub fn size(&self) -> [f32; 3] {
        if !self.is_valid() {
            return [0.0; 3];
        }
        [self.max[0] - self.min[0], self.max[1] - self.min[1], self.max[2] - self.min[2]]
    }

    pub fn centre(&self) -> [f32; 3] {
        if !self.is_valid() {
            return [0.0; 3];
        }
        [(self.min[0] + self.max[0]) * 0.5, (self.min[1] + self.max[1]) * 0.5, (self.min[2] + self.max[2]) * 0.5]
    }

    /// Half the diagonal.
    pub fn radius(&self) -> f32 {
        0.5 * math::length(self.size())
    }
}

impl Model {
    pub fn triangles(&self) -> usize {
        self.parts.iter().map(Part::triangles).sum()
    }

    pub fn vertices(&self) -> usize {
        self.parts.iter().map(|p| p.positions.len()).sum()
    }

    pub fn bounds(&self) -> Bounds {
        let mut b = Bounds::empty();
        for part in &self.parts {
            b.union(&part.bounds());
        }
        b
    }

    /// Width (x), height (y) and depth (z) in metres.
    pub fn size_m(&self) -> [f32; 3] {
        let s = self.bounds().size();
        [s[0] * self.unit_m, s[1] * self.unit_m, s[2] * self.unit_m]
    }

    /// Makes the model safe to draw: a material for every part, normals everywhere, no index
    /// past its vertices, no empty part, a sane unit, not too many parts.
    pub fn finish(&mut self) {
        if self.materials.is_empty() {
            self.materials.push(Material::default());
        }
        let materials = self.materials.len();
        for part in &mut self.parts {
            if part.material >= materials {
                part.material = 0;
            }
            for p in &mut part.positions {
                for c in p.iter_mut() {
                    if !c.is_finite() {
                        *c = 0.0;
                    }
                }
            }
            let n = part.positions.len();
            if part.indices.is_empty() && n >= 3 {
                part.indices = (0..(n - n % 3) as u32).collect();
            }
            part.indices.truncate(part.indices.len() / 3 * 3);
            if part.indices.iter().any(|&i| i as usize >= n) {
                part.indices = part.indices.chunks_exact(3).filter(|t| t.iter().all(|&i| (i as usize) < n)).flatten().copied().collect();
            }
            if part.normals.len() != n {
                part.normals = smooth_normals(&part.positions, &part.indices);
            }
            if !part.uvs.is_empty() && part.uvs.len() != n {
                part.uvs.clear();
            }
            if !part.tangents.is_empty() && part.tangents.len() != n {
                part.tangents.clear();
            }
            if !part.colors.is_empty() && part.colors.len() != n {
                part.colors.clear();
            }
        }
        self.parts.retain(|p| p.indices.len() >= 3);
        if self.parts.len() > MAX_PARTS {
            self.merge_by_material();
        }
        if !(self.unit_m.is_finite() && self.unit_m > 0.0) {
            self.unit_m = 1.0;
        }
        for t in &mut self.textures {
            if t.rgba.len() != (t.width * t.height * 4) as usize || t.width == 0 || t.height == 0 {
                *t = Texture::new(1, 1, vec![255, 255, 255, 255]);
            }
        }
        let textures = self.textures.len();
        for m in &mut self.materials {
            for slot in [&mut m.base_color_tex, &mut m.metallic_roughness_tex, &mut m.normal_tex, &mut m.emissive_tex, &mut m.occlusion_tex] {
                if slot.is_some_and(|t| t >= textures) {
                    *slot = None;
                }
            }
        }
    }

    /// One part per material (a model of thousands of tiny pieces draws in a few calls).
    fn merge_by_material(&mut self) {
        let mut merged: Vec<Part> = Vec::new();
        let mut slot: HashMap<usize, usize> = HashMap::new();
        let all_uv = self.parts.iter().all(|p| !p.uvs.is_empty());
        let all_tangent = self.parts.iter().all(|p| !p.tangents.is_empty());
        let all_colour = self.parts.iter().all(|p| !p.colors.is_empty());
        for part in std::mem::take(&mut self.parts) {
            let at = *slot.entry(part.material).or_insert_with(|| {
                merged.push(Part { name: self.materials.get(part.material).map_or_else(|| "part".to_string(), |m| m.name.clone()), material: part.material, ..Part::default() });
                merged.len() - 1
            });
            let into = &mut merged[at];
            let base = into.positions.len() as u32;
            into.positions.extend_from_slice(&part.positions);
            into.normals.extend_from_slice(&part.normals);
            if all_uv {
                into.uvs.extend_from_slice(&part.uvs);
            }
            if all_tangent {
                into.tangents.extend_from_slice(&part.tangents);
            }
            if all_colour {
                into.colors.extend_from_slice(&part.colors);
            }
            into.indices.extend(part.indices.iter().map(|i| i + base));
        }
        self.parts = merged;
    }

    /// The parts whose name (or material name) holds `name` ("helmet", "left arm").
    pub fn parts_named(&self, name: &str) -> Vec<usize> {
        let n = name.trim().to_lowercase();
        if n.is_empty() {
            return Vec::new();
        }
        let by_part: Vec<usize> = self.parts.iter().enumerate().filter(|(_, p)| p.name.to_lowercase().contains(&n)).map(|(i, _)| i).collect();
        if !by_part.is_empty() {
            return by_part;
        }
        // "helmet" also finds "Helm", "arms" also finds "arm".
        let stem = n.trim_end_matches('s');
        self.parts
            .iter()
            .enumerate()
            .filter(|(_, p)| {
                let pn = p.name.to_lowercase();
                let mn = self.materials.get(p.material).map(|m| m.name.to_lowercase()).unwrap_or_default();
                (stem.len() >= 3 && (pn.contains(stem) || mn.contains(stem))) || mn.contains(&n)
            })
            .map(|(i, _)| i)
            .collect()
    }
}

/// Area-weighted vertex normals over the triangles that share each vertex.
pub fn smooth_normals(positions: &[[f32; 3]], indices: &[u32]) -> Vec<[f32; 3]> {
    let mut n = vec![[0.0f32; 3]; positions.len()];
    for t in indices.chunks_exact(3) {
        let (ia, ib, ic) = (t[0] as usize, t[1] as usize, t[2] as usize);
        if ia >= positions.len() || ib >= positions.len() || ic >= positions.len() {
            continue;
        }
        let f = math::cross(math::sub(positions[ib], positions[ia]), math::sub(positions[ic], positions[ia]));
        for i in [ia, ib, ic] {
            n[i] = math::add(n[i], f);
        }
    }
    for v in &mut n {
        let l = math::length(*v);
        *v = if l > 1e-20 { math::scale(*v, 1.0 / l) } else { [0.0, 1.0, 0.0] };
    }
    n
}

/// Flat shading: every triangle its own three vertices with the face's normal (glTF's rule for
/// a primitive without normals).
pub fn flat_normals(part: &mut Part) {
    let mut out = Part { name: std::mem::take(&mut part.name), material: part.material, ..Part::default() };
    let has_uv = !part.uvs.is_empty();
    let has_colour = part.colors.len() == part.positions.len();
    for t in part.indices.chunks_exact(3) {
        let idx = [t[0] as usize, t[1] as usize, t[2] as usize];
        if idx.iter().any(|&i| i >= part.positions.len()) {
            continue;
        }
        let (a, b, c) = (part.positions[idx[0]], part.positions[idx[1]], part.positions[idx[2]]);
        let f = math::normalize(math::cross(math::sub(b, a), math::sub(c, a)));
        for &i in &idx {
            out.indices.push(out.positions.len() as u32);
            out.positions.push(part.positions[i]);
            out.normals.push(f);
            if has_uv {
                out.uvs.push(part.uvs.get(i).copied().unwrap_or([0.0, 0.0]));
            }
            if has_colour {
                out.colors.push(part.colors[i]);
            }
        }
    }
    *part = out;
}

pub fn srgb_to_linear(c: f32) -> f32 {
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

pub fn linear_to_srgb(c: f32) -> f32 {
    let c = c.clamp(0.0, 1.0);
    if c <= 0.0031308 {
        c * 12.92
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    }
}

// ------------------------------------------------------------------------------- the maths ----

/// Small column-major matrix and vector helpers (shared with the renderer).
pub mod math {
    pub type M4 = [f32; 16];

    pub fn identity() -> M4 {
        [1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0]
    }

    /// `a * b` (b applied first).
    pub fn mul(a: &M4, b: &M4) -> M4 {
        let mut r = [0.0f32; 16];
        for col in 0..4 {
            for row in 0..4 {
                r[col * 4 + row] = (0..4).map(|k| a[k * 4 + row] * b[col * 4 + k]).sum();
            }
        }
        r
    }

    pub fn translate(t: [f32; 3]) -> M4 {
        let mut m = identity();
        m[12] = t[0];
        m[13] = t[1];
        m[14] = t[2];
        m
    }

    pub fn scaling(s: [f32; 3]) -> M4 {
        let mut m = identity();
        m[0] = s[0];
        m[5] = s[1];
        m[10] = s[2];
        m
    }

    /// A unit quaternion (x, y, z, w) as a rotation.
    pub fn quat(q: [f32; 4]) -> M4 {
        let l = (q[0] * q[0] + q[1] * q[1] + q[2] * q[2] + q[3] * q[3]).sqrt();
        let [x, y, z, w] = if l > 1e-12 { [q[0] / l, q[1] / l, q[2] / l, q[3] / l] } else { [0.0, 0.0, 0.0, 1.0] };
        [
            1.0 - 2.0 * (y * y + z * z),
            2.0 * (x * y + w * z),
            2.0 * (x * z - w * y),
            0.0,
            2.0 * (x * y - w * z),
            1.0 - 2.0 * (x * x + z * z),
            2.0 * (y * z + w * x),
            0.0,
            2.0 * (x * z + w * y),
            2.0 * (y * z - w * x),
            1.0 - 2.0 * (x * x + y * y),
            0.0,
            0.0,
            0.0,
            0.0,
            1.0,
        ]
    }

    pub fn rot_x(a: f32) -> M4 {
        let (s, c) = a.sin_cos();
        [1.0, 0.0, 0.0, 0.0, 0.0, c, s, 0.0, 0.0, -s, c, 0.0, 0.0, 0.0, 0.0, 1.0]
    }

    pub fn rot_y(a: f32) -> M4 {
        let (s, c) = a.sin_cos();
        [c, 0.0, -s, 0.0, 0.0, 1.0, 0.0, 0.0, s, 0.0, c, 0.0, 0.0, 0.0, 0.0, 1.0]
    }

    pub fn rot_z(a: f32) -> M4 {
        let (s, c) = a.sin_cos();
        [c, s, 0.0, 0.0, -s, c, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0]
    }

    /// Euler angles in degrees, applied X first, then Y, then Z.
    pub fn euler_deg(r: [f32; 3]) -> M4 {
        let k = std::f32::consts::PI / 180.0;
        mul(&rot_z(r[2] * k), &mul(&rot_y(r[1] * k), &rot_x(r[0] * k)))
    }

    pub fn point(m: &M4, p: [f32; 3]) -> [f32; 3] {
        [
            m[0] * p[0] + m[4] * p[1] + m[8] * p[2] + m[12],
            m[1] * p[0] + m[5] * p[1] + m[9] * p[2] + m[13],
            m[2] * p[0] + m[6] * p[1] + m[10] * p[2] + m[14],
        ]
    }

    pub fn dir(m: &M4, d: [f32; 3]) -> [f32; 3] {
        [m[0] * d[0] + m[4] * d[1] + m[8] * d[2], m[1] * d[0] + m[5] * d[1] + m[9] * d[2], m[2] * d[0] + m[6] * d[1] + m[10] * d[2]]
    }

    /// Determinant of the upper 3×3 (negative: the transform mirrors, the winding flips).
    pub fn det3(m: &M4) -> f32 {
        m[0] * (m[5] * m[10] - m[9] * m[6]) - m[4] * (m[1] * m[10] - m[9] * m[2]) + m[8] * (m[1] * m[6] - m[5] * m[2])
    }

    /// The inverse transpose of the upper 3×3, as a 4×4 (normals under a non-uniform scale).
    pub fn normal_matrix(m: &M4) -> M4 {
        let d = det3(m);
        if d.abs() < 1e-20 {
            return identity();
        }
        let inv = 1.0 / d;
        // Cofactor matrix (= inverse transpose × det).
        let c = |r0: usize, c0: usize, r1: usize, c1: usize| m[c0 * 4 + r0] * m[c1 * 4 + r1] - m[c1 * 4 + r0] * m[c0 * 4 + r1];
        let mut r = identity();
        r[0] = c(1, 1, 2, 2) * inv;
        r[1] = -c(0, 1, 2, 2) * inv;
        r[2] = c(0, 1, 1, 2) * inv;
        r[4] = -c(1, 0, 2, 2) * inv;
        r[5] = c(0, 0, 2, 2) * inv;
        r[6] = -c(0, 0, 1, 2) * inv;
        r[8] = c(1, 0, 2, 1) * inv;
        r[9] = -c(0, 0, 2, 1) * inv;
        r[10] = c(0, 0, 1, 1) * inv;
        // That is the cofactor matrix laid out as the inverse transpose.
        r
    }

    pub fn perspective(fovy: f32, aspect: f32, near: f32, far: f32) -> M4 {
        let f = 1.0 / (fovy * 0.5).tan();
        let mut m = [0.0f32; 16];
        m[0] = f / aspect.max(1e-6);
        m[5] = f;
        m[10] = (far + near) / (near - far);
        m[11] = -1.0;
        m[14] = 2.0 * far * near / (near - far);
        m
    }

    pub fn look_at(eye: [f32; 3], target: [f32; 3], up: [f32; 3]) -> M4 {
        let f = normalize(sub(target, eye));
        let s = normalize(cross(f, up));
        let u = cross(s, f);
        [s[0], u[0], -f[0], 0.0, s[1], u[1], -f[1], 0.0, s[2], u[2], -f[2], 0.0, -dot(s, eye), -dot(u, eye), dot(f, eye), 1.0]
    }

    pub fn sub(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
        [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
    }

    pub fn add(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
        [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
    }

    pub fn scale(a: [f32; 3], k: f32) -> [f32; 3] {
        [a[0] * k, a[1] * k, a[2] * k]
    }

    pub fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
        a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
    }

    pub fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
        [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
    }

    pub fn length(a: [f32; 3]) -> f32 {
        dot(a, a).sqrt()
    }

    pub fn normalize(a: [f32; 3]) -> [f32; 3] {
        let l = length(a);
        if l > 1e-20 {
            scale(a, 1.0 / l)
        } else {
            [0.0, 1.0, 0.0]
        }
    }
}

// --------------------------------------------------------------------------- loading ----

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Glb,
    Gltf,
    Obj,
    Stl,
    Zip,
}

impl Format {
    pub fn of_path(path: &Path) -> Option<Format> {
        Some(match path.extension()?.to_str()?.to_ascii_lowercase().as_str() {
            "glb" => Format::Glb,
            "gltf" => Format::Gltf,
            "obj" => Format::Obj,
            "stl" => Format::Stl,
            "zip" => Format::Zip,
            _ => return None,
        })
    }

    /// The format by the file's first bytes (a download without a telling name).
    pub fn sniff(bytes: &[u8]) -> Option<Format> {
        if bytes.starts_with(b"glTF") {
            return Some(Format::Glb);
        }
        if bytes.starts_with(b"PK\x03\x04") {
            return Some(Format::Zip);
        }
        if bytes.len() >= 84 {
            let n = u32::from_le_bytes([bytes[80], bytes[81], bytes[82], bytes[83]]) as usize;
            if 84 + n.saturating_mul(50) == bytes.len() {
                return Some(Format::Stl);
            }
        }
        let head = String::from_utf8_lossy(&bytes[..bytes.len().min(2048)]).to_string();
        let t = head.trim_start_matches('\u{feff}').trim_start();
        if t.starts_with('{') && head.contains("\"asset\"") {
            return Some(Format::Gltf);
        }
        if t.starts_with("solid") && head.contains("facet") {
            return Some(Format::Stl);
        }
        if head.lines().any(|l| l.starts_with("v ") || l.starts_with("f ") || l.starts_with("mtllib")) {
            return Some(Format::Obj);
        }
        None
    }

    pub fn is_model_file(path: &Path) -> bool {
        matches!(Format::of_path(path), Some(Format::Glb | Format::Gltf | Format::Obj | Format::Stl))
    }
}

/// Reads a model file (a ZIP is unpacked next to it first).
pub fn load_file(path: &Path, progress: Progress) -> Result<Model, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("I couldn't read {}: {e}", path.display()))?;
    progress(0.05);
    let format = Format::of_path(path).or_else(|| Format::sniff(&bytes)).ok_or_else(|| format!("{} is no model file I can read (glTF, GLB, OBJ, STL or a ZIP of them).", path.display()))?;
    let name = path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "model".into());
    let mut model = match format {
        Format::Zip => {
            let into = path.with_extension("unzipped");
            let file = unpack_zip(&bytes, &into)?;
            return load_file(&file, progress);
        }
        _ => load_bytes(&bytes, format, path.parent(), progress)?,
    };
    if model.name.is_empty() || model.name == "model" {
        model.name = name;
    }
    Ok(model)
}

/// Reads a model from its bytes; `base` is where its external files are (a .gltf's .bin and
/// images, an .obj's .mtl).
pub fn load_bytes(bytes: &[u8], format: Format, base: Option<&Path>, progress: Progress) -> Result<Model, String> {
    let mut model = match format {
        Format::Glb => load_glb(bytes, base, progress)?,
        Format::Gltf => load_gltf(bytes, base, progress)?,
        Format::Obj => load_obj(bytes, base, progress)?,
        Format::Stl => load_stl(bytes)?,
        Format::Zip => return Err("A ZIP must be unpacked first.".into()),
    };
    model.finish();
    if model.triangles() == 0 {
        return Err("The file holds no triangles I could draw.".into());
    }
    if model.triangles() > MAX_TRIANGLES {
        return Err(format!("That model has {} million triangles; I draw up to {} million.", model.triangles() / 1_000_000, MAX_TRIANGLES / 1_000_000));
    }
    progress(1.0);
    Ok(model)
}

/// Maps `f` over `items` on every core (texture decoding).
fn parallel_map<T: Sync, R: Send>(items: &[T], f: impl Fn(&T) -> R + Sync) -> Vec<R> {
    let workers = std::thread::available_parallelism().map_or(4, |n| n.get()).clamp(1, 8).min(items.len().max(1));
    let next = AtomicUsize::new(0);
    let mut out: Vec<Option<R>> = (0..items.len()).map(|_| None).collect();
    let results = std::sync::Mutex::new(&mut out);
    std::thread::scope(|s| {
        for _ in 0..workers {
            s.spawn(|| loop {
                let i = next.fetch_add(1, Ordering::Relaxed);
                if i >= items.len() {
                    break;
                }
                let r = f(&items[i]);
                results.lock().unwrap_or_else(|e| e.into_inner())[i] = Some(r);
            });
        }
    });
    out.into_iter().map(|r| r.expect("every item mapped")).collect()
}

/// An image file's bytes as a texture, scaled down past `MAX_TEXTURE_SIDE`.
pub fn decode_texture(bytes: &[u8]) -> Option<Texture> {
    let img = image::load_from_memory(bytes).ok()?.to_rgba8();
    let (w, h) = img.dimensions();
    if w == 0 || h == 0 {
        return None;
    }
    let img = if w.max(h) > MAX_TEXTURE_SIDE {
        let k = MAX_TEXTURE_SIDE as f32 / w.max(h) as f32;
        image::imageops::resize(&img, ((w as f32 * k).round() as u32).max(1), ((h as f32 * k).round() as u32).max(1), image::imageops::FilterType::Triangle)
    } else {
        img
    };
    let (w, h) = img.dimensions();
    Some(Texture::new(w, h, img.into_raw()))
}

/// A relative file next to the model (never above it, never absolute).
fn sibling(base: Option<&Path>, uri: &str) -> Option<PathBuf> {
    let decoded = urlencoding::decode(uri).map(|c| c.into_owned()).unwrap_or_else(|_| uri.to_string());
    let rel = Path::new(&decoded);
    if rel.is_absolute() || rel.components().any(|c| matches!(c, std::path::Component::ParentDir | std::path::Component::Prefix(_) | std::path::Component::RootDir)) {
        return None;
    }
    Some(base.unwrap_or(Path::new(".")).join(rel))
}

/// `data:…;base64,…` URIs; None for anything else.
fn data_uri(uri: &str) -> Option<Result<Vec<u8>, String>> {
    use base64::Engine;
    let rest = uri.strip_prefix("data:")?;
    let Some((meta, data)) = rest.split_once(',') else { return Some(Err("a broken data URI".into())) };
    Some(if meta.ends_with(";base64") {
        let clean: String = data.chars().filter(|c| !c.is_whitespace()).collect();
        base64::engine::general_purpose::STANDARD.decode(clean.as_bytes()).map_err(|e| format!("a data URI that is no base64: {e}"))
    } else {
        Ok(urlencoding::decode_binary(data.as_bytes()).into_owned())
    })
}

fn uri_bytes(uri: &str, base: Option<&Path>) -> Result<Vec<u8>, String> {
    if let Some(data) = data_uri(uri) {
        return data;
    }
    let path = sibling(base, uri).ok_or_else(|| format!("the file '{uri}' lies outside the model's folder"))?;
    std::fs::read(&path).map_err(|e| format!("{} is missing: {e}", path.display()))
}

// ---------------------------------------------------------------------------- glTF ----

#[derive(Clone, Copy)]
struct View {
    buffer: usize,
    offset: usize,
    length: usize,
    stride: usize,
}

#[derive(Clone)]
struct Accessor {
    view: Option<usize>,
    offset: usize,
    ctype: u32,
    normalized: bool,
    count: usize,
    comps: usize,
    sparse: Option<Sparse>,
}

#[derive(Clone, Copy)]
struct Sparse {
    count: usize,
    idx_view: usize,
    idx_offset: usize,
    idx_ctype: u32,
    val_view: usize,
    val_offset: usize,
}

struct Gltf<'a> {
    json: &'a Value,
    buffers: Vec<Cow<'a, [u8]>>,
    views: Vec<View>,
    accessors: Vec<Accessor>,
}

fn usize_of(v: &Value) -> Option<usize> {
    v.as_u64().map(|n| n as usize)
}

fn comp_size(ctype: u32) -> Result<usize, String> {
    Ok(match ctype {
        5120 | 5121 => 1,
        5122 | 5123 => 2,
        5125 | 5126 => 4,
        other => return Err(format!("an accessor of component type {other}")),
    })
}

fn component(buf: &[u8], o: usize, ctype: u32, normalized: bool) -> f32 {
    match ctype {
        5126 => f32::from_le_bytes([buf[o], buf[o + 1], buf[o + 2], buf[o + 3]]),
        5121 => {
            let v = buf[o] as f32;
            if normalized {
                v / 255.0
            } else {
                v
            }
        }
        5120 => {
            let v = buf[o] as i8 as f32;
            if normalized {
                (v / 127.0).max(-1.0)
            } else {
                v
            }
        }
        5123 => {
            let v = u16::from_le_bytes([buf[o], buf[o + 1]]) as f32;
            if normalized {
                v / 65535.0
            } else {
                v
            }
        }
        5122 => {
            let v = i16::from_le_bytes([buf[o], buf[o + 1]]) as f32;
            if normalized {
                (v / 32767.0).max(-1.0)
            } else {
                v
            }
        }
        5125 => u32::from_le_bytes([buf[o], buf[o + 1], buf[o + 2], buf[o + 3]]) as f32,
        _ => 0.0,
    }
}

fn index_at(buf: &[u8], o: usize, ctype: u32) -> u32 {
    match ctype {
        5121 => buf[o] as u32,
        5123 => u16::from_le_bytes([buf[o], buf[o + 1]]) as u32,
        5125 => u32::from_le_bytes([buf[o], buf[o + 1], buf[o + 2], buf[o + 3]]),
        _ => 0,
    }
}

impl<'a> Gltf<'a> {
    fn new(json: &'a Value, bin: Option<&'a [u8]>, base: Option<&Path>) -> Result<Self, String> {
        let required: Vec<&str> = json["extensionsRequired"].as_array().map(|a| a.iter().filter_map(Value::as_str).collect()).unwrap_or_default();
        for ext in &required {
            if matches!(*ext, "KHR_draco_mesh_compression" | "EXT_meshopt_compression" | "KHR_meshopt_compression") {
                return Err(format!("this model is compressed with {ext}, which I can't unpack without an extra library"));
            }
        }
        if json["asset"]["version"].as_str().is_some_and(|v| v.starts_with('1')) {
            return Err("that is a glTF 1.0 file; I read glTF 2.0".into());
        }
        let mut buffers = Vec::new();
        for (i, b) in json["buffers"].as_array().map(Vec::as_slice).unwrap_or(&[]).iter().enumerate() {
            let bytes: Cow<'a, [u8]> = match b["uri"].as_str() {
                None if i == 0 => Cow::Borrowed(bin.ok_or("the binary chunk is missing")?),
                None => Cow::Owned(Vec::new()),
                Some(uri) => Cow::Owned(uri_bytes(uri, base)?),
            };
            buffers.push(bytes);
        }
        let views = json["bufferViews"]
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or(&[])
            .iter()
            .map(|v| View { buffer: usize_of(&v["buffer"]).unwrap_or(0), offset: usize_of(&v["byteOffset"]).unwrap_or(0), length: usize_of(&v["byteLength"]).unwrap_or(0), stride: usize_of(&v["byteStride"]).unwrap_or(0) })
            .collect();
        let mut accessors = Vec::new();
        for a in json["accessors"].as_array().map(Vec::as_slice).unwrap_or(&[]) {
            let comps = match a["type"].as_str().unwrap_or("SCALAR") {
                "SCALAR" => 1,
                "VEC2" => 2,
                "VEC3" => 3,
                "VEC4" | "MAT2" => 4,
                "MAT3" => 9,
                "MAT4" => 16,
                _ => 1,
            };
            let sparse = a.get("sparse").and_then(|s| {
                Some(Sparse {
                    count: usize_of(&s["count"])?,
                    idx_view: usize_of(&s["indices"]["bufferView"])?,
                    idx_offset: usize_of(&s["indices"]["byteOffset"]).unwrap_or(0),
                    idx_ctype: s["indices"]["componentType"].as_u64()? as u32,
                    val_view: usize_of(&s["values"]["bufferView"])?,
                    val_offset: usize_of(&s["values"]["byteOffset"]).unwrap_or(0),
                })
            });
            accessors.push(Accessor {
                view: usize_of(&a["bufferView"]),
                offset: usize_of(&a["byteOffset"]).unwrap_or(0),
                ctype: a["componentType"].as_u64().unwrap_or(5126) as u32,
                normalized: a["normalized"].as_bool().unwrap_or(false),
                count: usize_of(&a["count"]).unwrap_or(0),
                comps,
                sparse,
            });
        }
        Ok(Gltf { json, buffers, views, accessors })
    }

    /// The bytes of view `v` and where element 0 starts in them.
    fn view_bytes(&self, v: usize) -> Result<(&[u8], View), String> {
        let view = *self.views.get(v).ok_or_else(|| format!("bufferView {v} is missing"))?;
        let buf = self.buffers.get(view.buffer).ok_or_else(|| format!("buffer {} is missing", view.buffer))?;
        let end = view.offset.checked_add(view.length).filter(|e| *e <= buf.len()).ok_or_else(|| format!("bufferView {v} runs past its buffer"))?;
        Ok((&buf[..end], view))
    }

    /// Accessor `i` as floats, `comps` per element.
    fn floats(&self, i: usize) -> Result<(Vec<f32>, usize), String> {
        let a = self.accessors.get(i).ok_or_else(|| format!("accessor {i} is missing"))?;
        let mut out = vec![0f32; a.count * a.comps];
        if let Some(v) = a.view {
            let (buf, view) = self.view_bytes(v)?;
            let size = comp_size(a.ctype)?;
            let elem = size * a.comps;
            let stride = if view.stride > 0 { view.stride } else { elem };
            let start = view.offset + a.offset;
            if a.count > 0 && start + stride * (a.count - 1) + elem > buf.len() {
                return Err(format!("accessor {i} runs past its bufferView"));
            }
            for e in 0..a.count {
                let at = start + e * stride;
                for c in 0..a.comps {
                    out[e * a.comps + c] = component(buf, at + c * size, a.ctype, a.normalized);
                }
            }
        }
        if let Some(sp) = a.sparse {
            let (ib, iv) = self.view_bytes(sp.idx_view)?;
            let (vb, vv) = self.view_bytes(sp.val_view)?;
            let (isz, vsz) = (comp_size(sp.idx_ctype)?, comp_size(a.ctype)?);
            for k in 0..sp.count {
                let io = iv.offset + sp.idx_offset + k * isz;
                let vo = vv.offset + sp.val_offset + k * vsz * a.comps;
                if io + isz > ib.len() || vo + vsz * a.comps > vb.len() {
                    break;
                }
                let target = index_at(ib, io, sp.idx_ctype) as usize;
                if target < a.count {
                    for c in 0..a.comps {
                        out[target * a.comps + c] = component(vb, vo + c * vsz, a.ctype, a.normalized);
                    }
                }
            }
        }
        Ok((out, a.comps))
    }

    fn indices(&self, i: usize) -> Result<Vec<u32>, String> {
        let a = self.accessors.get(i).ok_or_else(|| format!("accessor {i} is missing"))?;
        let Some(v) = a.view else { return Ok((0..a.count as u32).collect()) };
        let (buf, view) = self.view_bytes(v)?;
        let size = comp_size(a.ctype)?;
        let stride = if view.stride > 0 { view.stride } else { size };
        let start = view.offset + a.offset;
        if a.count > 0 && start + stride * (a.count - 1) + size > buf.len() {
            return Err(format!("index accessor {i} runs past its bufferView"));
        }
        Ok((0..a.count).map(|e| index_at(buf, start + e * stride, a.ctype)).collect())
    }

    fn vec3s(&self, i: usize) -> Result<Vec<[f32; 3]>, String> {
        let (f, c) = self.floats(i)?;
        Ok(f.chunks_exact(c.max(1)).filter(|e| e.len() >= 3).map(|e| [e[0], e[1], e[2]]).collect())
    }

    /// Image `i`'s file bytes.
    fn image_bytes(&self, i: usize, base: Option<&Path>) -> Result<Vec<u8>, String> {
        let img = &self.json["images"][i];
        if let Some(uri) = img["uri"].as_str() {
            return uri_bytes(uri, base);
        }
        let v = usize_of(&img["bufferView"]).ok_or("an image without data")?;
        let (buf, view) = self.view_bytes(v)?;
        Ok(buf[view.offset..view.offset + view.length].to_vec())
    }

    /// The image a texture index shows (webp sources through EXT_texture_webp).
    fn texture_image(&self, tex: usize) -> Option<usize> {
        let t = &self.json["textures"][tex];
        usize_of(&t["source"]).or_else(|| usize_of(&t["extensions"]["EXT_texture_webp"]["source"]))
    }
}

pub fn load_glb(bytes: &[u8], base: Option<&Path>, progress: Progress) -> Result<Model, String> {
    if bytes.len() < 20 || &bytes[0..4] != b"glTF" {
        return Err("that is no GLB file".into());
    }
    let u32_at = |o: usize| u32::from_le_bytes([bytes[o], bytes[o + 1], bytes[o + 2], bytes[o + 3]]) as usize;
    if u32_at(4) != 2 {
        return Err("that is a glTF 1.0 binary; I read glTF 2.0".into());
    }
    let length = u32_at(8).min(bytes.len());
    let (mut json, mut bin) = (None, None);
    let mut off = 12;
    while off + 8 <= length {
        let (len, kind) = (u32_at(off), u32_at(off + 4));
        let start = off + 8;
        let Some(end) = start.checked_add(len).filter(|e| *e <= length) else { break };
        match kind {
            0x4E4F_534A if json.is_none() => json = Some(&bytes[start..end]),
            0x004E_4942 if bin.is_none() => bin = Some(&bytes[start..end]),
            _ => {}
        }
        off = end;
    }
    let json: Value = serde_json::from_slice(json.ok_or("the GLB has no JSON chunk")?).map_err(|e| format!("the GLB's JSON is broken: {e}"))?;
    progress(0.1);
    build_gltf(&json, bin, base, progress)
}

pub fn load_gltf(bytes: &[u8], base: Option<&Path>, progress: Progress) -> Result<Model, String> {
    let json: Value = serde_json::from_slice(bytes).map_err(|e| format!("the glTF JSON is broken: {e}"))?;
    progress(0.1);
    build_gltf(&json, None, base, progress)
}

fn local_matrix(node: &Value) -> math::M4 {
    if let Some(m) = node["matrix"].as_array().filter(|m| m.len() == 16) {
        let mut out = [0.0f32; 16];
        for (i, v) in m.iter().enumerate() {
            out[i] = v.as_f64().unwrap_or(0.0) as f32;
        }
        return out;
    }
    let arr = |v: &Value, n: usize, d: f32| -> Vec<f32> { v.as_array().filter(|a| a.len() == n).map(|a| a.iter().map(|x| x.as_f64().unwrap_or(d as f64) as f32).collect()).unwrap_or_else(|| vec![d; n]) };
    let t = arr(&node["translation"], 3, 0.0);
    let s = arr(&node["scale"], 3, 1.0);
    let r = node["rotation"].as_array().filter(|a| a.len() == 4).map(|a| a.iter().map(|x| x.as_f64().unwrap_or(0.0) as f32).collect::<Vec<_>>()).unwrap_or_else(|| vec![0.0, 0.0, 0.0, 1.0]);
    let m = math::mul(&math::translate([t[0], t[1], t[2]]), &math::mul(&math::quat([r[0], r[1], r[2], r[3]]), &math::scaling([s[0], s[1], s[2]])));
    m
}

fn build_gltf(json: &Value, bin: Option<&[u8]>, base: Option<&Path>, progress: Progress) -> Result<Model, String> {
    let doc = Gltf::new(json, bin, base)?;
    let mut model = Model { name: String::new(), unit_m: 1.0, ..Model::default() };

    // ---- Materials and the images they use.
    let mut image_slot: HashMap<usize, usize> = HashMap::new();
    let mut wanted: Vec<usize> = Vec::new();
    let mut tex_of = |info: &Value| -> Option<usize> {
        let image = doc.texture_image(usize_of(&info["index"])?)?;
        Some(*image_slot.entry(image).or_insert_with(|| {
            wanted.push(image);
            wanted.len() - 1
        }))
    };
    for m in json["materials"].as_array().map(Vec::as_slice).unwrap_or(&[]) {
        let pbr = &m["pbrMetallicRoughness"];
        let f = |v: &Value, d: f32| v.as_f64().map_or(d, |x| x as f32);
        let mut mat = Material { name: m["name"].as_str().unwrap_or("material").to_string(), ..Material::default() };
        if let Some(c) = pbr["baseColorFactor"].as_array().filter(|c| c.len() == 4) {
            mat.base_color = [f(&c[0], 1.0), f(&c[1], 1.0), f(&c[2], 1.0), f(&c[3], 1.0)];
        } else {
            mat.base_color = [1.0; 4];
        }
        mat.metallic = f(&pbr["metallicFactor"], 1.0);
        mat.roughness = f(&pbr["roughnessFactor"], 1.0);
        mat.base_color_tex = tex_of(&pbr["baseColorTexture"]);
        mat.metallic_roughness_tex = tex_of(&pbr["metallicRoughnessTexture"]);
        // The older specular-glossiness model (many Sketchfab exports).
        let sg = &m["extensions"]["KHR_materials_pbrSpecularGlossiness"];
        if sg.is_object() {
            if let Some(c) = sg["diffuseFactor"].as_array().filter(|c| c.len() == 4) {
                mat.base_color = [f(&c[0], 1.0), f(&c[1], 1.0), f(&c[2], 1.0), f(&c[3], 1.0)];
            }
            if mat.base_color_tex.is_none() {
                mat.base_color_tex = tex_of(&sg["diffuseTexture"]);
            }
            mat.metallic = 0.0;
            mat.roughness = 1.0 - f(&sg["glossinessFactor"], 1.0);
        }
        mat.normal_tex = tex_of(&m["normalTexture"]);
        mat.normal_scale = f(&m["normalTexture"]["scale"], 1.0);
        mat.occlusion_tex = tex_of(&m["occlusionTexture"]);
        mat.emissive_tex = tex_of(&m["emissiveTexture"]);
        if let Some(e) = m["emissiveFactor"].as_array().filter(|e| e.len() == 3) {
            let k = f(&m["extensions"]["KHR_materials_emissive_strength"]["emissiveStrength"], 1.0);
            mat.emissive = [f(&e[0], 0.0) * k, f(&e[1], 0.0) * k, f(&e[2], 0.0) * k];
        }
        mat.alpha = match m["alphaMode"].as_str() {
            Some("MASK") => AlphaMode::Mask(f(&m["alphaCutoff"], 0.5)),
            Some("BLEND") => AlphaMode::Blend,
            _ => AlphaMode::Opaque,
        };
        // Glass and other see-through materials.
        let transmission = f(&m["extensions"]["KHR_materials_transmission"]["transmissionFactor"], 0.0);
        if transmission > 0.05 {
            mat.alpha = AlphaMode::Blend;
            mat.base_color[3] = mat.base_color[3].min(1.0 - transmission * 0.75);
        }
        mat.double_sided = m["doubleSided"].as_bool().unwrap_or(false);
        model.materials.push(mat);
    }
    let fallback_material = model.materials.len();
    model.materials.push(Material::default());

    // ---- The images, decoded on every core.
    let decoded = parallel_map(&wanted, |&image| doc.image_bytes(image, base).ok().and_then(|b| decode_texture(&b)));
    model.textures = decoded.into_iter().map(|t| t.unwrap_or_else(|| Texture::new(1, 1, vec![255, 255, 255, 255]))).collect();
    progress(0.55);

    // ---- The scene's nodes, with their transforms.
    let nodes = json["nodes"].as_array().map(Vec::as_slice).unwrap_or(&[]);
    let roots: Vec<usize> = {
        let scene = usize_of(&json["scene"]).unwrap_or(0);
        let listed: Vec<usize> = json["scenes"][scene]["nodes"].as_array().map(|a| a.iter().filter_map(usize_of).collect()).unwrap_or_default();
        if !listed.is_empty() {
            listed
        } else {
            // No scene: every node nobody points at.
            let mut child = vec![false; nodes.len()];
            for n in nodes {
                for c in n["children"].as_array().map(Vec::as_slice).unwrap_or(&[]).iter().filter_map(usize_of) {
                    if c < child.len() {
                        child[c] = true;
                    }
                }
            }
            (0..nodes.len()).filter(|i| !child[*i]).collect()
        }
    };
    let mut stack: Vec<(usize, math::M4, usize)> = roots.into_iter().rev().map(|r| (r, math::identity(), 0)).collect();
    let mut visited = 0usize;
    let meshes = json["meshes"].as_array().map(Vec::as_slice).unwrap_or(&[]);
    let total_meshes = nodes.iter().filter(|n| n.get("mesh").is_some()).count().max(1);
    let mut done_meshes = 0usize;
    let mut triangles = 0usize;
    while let Some((n, parent, depth)) = stack.pop() {
        visited += 1;
        if n >= nodes.len() || depth > 64 || visited > 200_000 {
            continue;
        }
        let node = &nodes[n];
        let world = math::mul(&parent, &local_matrix(node));
        for c in node["children"].as_array().map(Vec::as_slice).unwrap_or(&[]).iter().rev().filter_map(usize_of) {
            stack.push((c, world, depth + 1));
        }
        let Some(mesh) = usize_of(&node["mesh"]).and_then(|m| meshes.get(m)) else { continue };
        done_meshes += 1;
        progress(0.55 + 0.45 * (done_meshes as f32 / total_meshes as f32).min(1.0));
        let name = node["name"].as_str().filter(|s| !s.trim().is_empty()).or_else(|| mesh["name"].as_str()).map(str::to_string).unwrap_or_else(|| format!("part {}", model.parts.len() + 1));
        let normal_m = math::normal_matrix(&world);
        let mirrored = math::det3(&world) < 0.0;
        for prim in mesh["primitives"].as_array().map(Vec::as_slice).unwrap_or(&[]) {
            let mode = prim["mode"].as_u64().unwrap_or(4);
            if !matches!(mode, 4..=6) {
                continue;
            }
            let attrs = &prim["attributes"];
            let Some(pos) = usize_of(&attrs["POSITION"]) else { continue };
            let positions = doc.vec3s(pos)?;
            let count = positions.len();
            let mut indices = match usize_of(&prim["indices"]) {
                Some(i) => doc.indices(i)?,
                None => (0..count as u32).collect(),
            };
            indices = match mode {
                5 => (0..indices.len().saturating_sub(2)).flat_map(|i| if i % 2 == 0 { [indices[i], indices[i + 1], indices[i + 2]] } else { [indices[i + 1], indices[i], indices[i + 2]] }).collect(),
                6 => (1..indices.len().saturating_sub(1)).flat_map(|i| [indices[0], indices[i], indices[i + 1]]).collect(),
                _ => indices,
            };
            triangles += indices.len() / 3;
            if triangles > MAX_TRIANGLES {
                return Err(format!("that model has more than {} million triangles; I draw up to that many", MAX_TRIANGLES / 1_000_000));
            }
            let normals = usize_of(&attrs["NORMAL"]).map(|i| doc.vec3s(i)).transpose()?.filter(|n| n.len() == count);
            let uvs = usize_of(&attrs["TEXCOORD_0"])
                .map(|i| doc.floats(i))
                .transpose()?
                .map(|(f, c)| f.chunks_exact(c.max(1)).filter(|e| e.len() >= 2).map(|e| [e[0], e[1]]).collect::<Vec<_>>())
                .filter(|u| u.len() == count)
                .unwrap_or_default();
            let tangents = usize_of(&attrs["TANGENT"])
                .map(|i| doc.floats(i))
                .transpose()?
                .map(|(f, c)| f.chunks_exact(c.max(1)).filter(|e| e.len() >= 4).map(|e| [e[0], e[1], e[2], e[3]]).collect::<Vec<_>>())
                .filter(|t| t.len() == count)
                .unwrap_or_default();
            let mut part = Part {
                name: name.clone(),
                positions: positions.iter().map(|p| math::point(&world, *p)).collect(),
                normals: Vec::new(),
                uvs,
                tangents: tangents.iter().map(|t| {
                    let d = math::normalize(math::dir(&world, [t[0], t[1], t[2]]));
                    [d[0], d[1], d[2], if mirrored { -t[3] } else { t[3] }]
                }).collect(),
                colors: Vec::new(),
                indices,
                material: usize_of(&prim["material"]).filter(|m| *m < fallback_material).unwrap_or(fallback_material),
            };
            if mirrored {
                for t in part.indices.chunks_exact_mut(3) {
                    t.swap(1, 2);
                }
            }
            match normals {
                Some(n) => part.normals = n.iter().map(|v| math::normalize(math::dir(&normal_m, *v))).collect(),
                None => {
                    part.indices.retain(|&i| (i as usize) < count);
                    part.indices.truncate(part.indices.len() / 3 * 3);
                    part.tangents.clear();
                    flat_normals(&mut part);
                }
            }
            model.parts.push(part);
        }
    }
    model.name = json["asset"]["extras"]["title"].as_str().or_else(|| json["scenes"][0]["name"].as_str()).unwrap_or("").to_string();
    if let Some(author) = json["asset"]["extras"]["author"].as_str() {
        model.credit = Some(Credit { title: model.name.clone(), author: author.to_string(), license: json["asset"]["extras"]["license"].as_str().unwrap_or("").to_string(), ..Credit::default() });
    }
    Ok(model)
}

// ----------------------------------------------------------------------------- OBJ ----

#[derive(Default)]
struct ObjPart {
    name: String,
    material: usize,
    positions: Vec<[f32; 3]>,
    normals: Vec<[f32; 3]>,
    uvs: Vec<[f32; 2]>,
    indices: Vec<u32>,
    corners: HashMap<(i64, i64, i64), u32>,
    has_normals: bool,
    has_uvs: bool,
}

fn floats<'a>(it: impl Iterator<Item = &'a str>) -> Vec<f32> {
    it.filter_map(|t| t.parse::<f32>().ok()).collect()
}

/// An OBJ index (1-based, or negative from the end) as a 0-based one.
fn obj_index(raw: &str, len: usize) -> Option<i64> {
    let i: i64 = raw.parse().ok()?;
    let at = if i < 0 { len as i64 + i } else { i - 1 };
    (at >= 0 && (at as usize) < len).then_some(at)
}

pub fn load_obj(bytes: &[u8], base: Option<&Path>, progress: Progress) -> Result<Model, String> {
    let text = String::from_utf8_lossy(bytes);
    let mut model = Model { unit_m: 1.0, ..Model::default() };
    let mut material_of: HashMap<String, usize> = HashMap::new();
    let (mut v, mut vt, mut vn): (Vec<[f32; 3]>, Vec<[f32; 2]>, Vec<[f32; 3]>) = (Vec::new(), Vec::new(), Vec::new());
    let mut parts: Vec<ObjPart> = Vec::new();
    let mut part_of: HashMap<(String, usize), usize> = HashMap::new();
    let (mut group, mut material) = ("model".to_string(), usize::MAX);
    let mut current: Option<usize> = None;
    let total = text.len().max(1);
    let mut read = 0usize;
    let mut triangles = 0usize;
    for (n, raw) in text.lines().enumerate() {
        read += raw.len() + 1;
        if n % 65536 == 0 {
            progress(0.05 + 0.9 * read as f32 / total as f32);
        }
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut it = line.split_whitespace();
        let key = it.next().unwrap_or("");
        match key {
            "v" => {
                let f = floats(it.take(3));
                if f.len() == 3 {
                    v.push([f[0], f[1], f[2]]);
                }
            }
            "vt" => {
                let f = floats(it.take(2));
                vt.push([f.first().copied().unwrap_or(0.0), 1.0 - f.get(1).copied().unwrap_or(0.0)]);
            }
            "vn" => {
                let f = floats(it.take(3));
                if f.len() == 3 {
                    vn.push([f[0], f[1], f[2]]);
                }
            }
            "o" | "g" => {
                let name = line[key.len()..].trim();
                group = if name.is_empty() { "model".to_string() } else { name.to_string() };
                current = None;
            }
            "usemtl" => {
                let name = line[key.len()..].trim().to_string();
                material = *material_of.entry(name.clone()).or_insert_with(|| {
                    model.materials.push(Material { name, ..Material::default() });
                    model.materials.len() - 1
                });
                current = None;
            }
            "mtllib" => {
                let file = line[key.len()..].trim();
                if let Some(path) = sibling(base, file) {
                    if let Ok(mtl) = std::fs::read(&path) {
                        load_mtl(&String::from_utf8_lossy(&mtl), path.parent(), &mut model, &mut material_of);
                    }
                }
            }
            "f" => {
                let p = *current.get_or_insert_with(|| {
                    let key = (group.clone(), material);
                    *part_of.entry(key).or_insert_with(|| {
                        parts.push(ObjPart { name: group.clone(), material, ..ObjPart::default() });
                        parts.len() - 1
                    })
                });
                let part = &mut parts[p];
                let mut corners: Vec<u32> = Vec::with_capacity(4);
                for token in it {
                    let mut f = token.split('/');
                    let Some(pi) = f.next().and_then(|s| obj_index(s, v.len())) else { continue };
                    let ti = f.next().filter(|s| !s.is_empty()).and_then(|s| obj_index(s, vt.len())).unwrap_or(-1);
                    let ni = f.next().filter(|s| !s.is_empty()).and_then(|s| obj_index(s, vn.len())).unwrap_or(-1);
                    let next = part.positions.len() as u32;
                    let idx = *part.corners.entry((pi, ti, ni)).or_insert(next);
                    if idx == next {
                        part.positions.push(v[pi as usize]);
                        part.uvs.push(if ti >= 0 { vt[ti as usize] } else { [0.0, 0.0] });
                        part.normals.push(if ni >= 0 { vn[ni as usize] } else { [0.0, 0.0, 0.0] });
                        part.has_uvs |= ti >= 0;
                        part.has_normals |= ni >= 0;
                    }
                    corners.push(idx);
                }
                for k in 1..corners.len().saturating_sub(1) {
                    part.indices.extend_from_slice(&[corners[0], corners[k], corners[k + 1]]);
                    triangles += 1;
                }
                if triangles > MAX_TRIANGLES {
                    return Err(format!("that model has more than {} million triangles; I draw up to that many", MAX_TRIANGLES / 1_000_000));
                }
            }
            _ => {}
        }
    }
    let default_material = model.materials.len();
    model.materials.push(Material::default());
    for p in parts {
        let mut part = Part {
            name: p.name,
            material: if p.material == usize::MAX { default_material } else { p.material },
            uvs: if p.has_uvs { p.uvs } else { Vec::new() },
            normals: if p.has_normals && p.normals.iter().all(|n| math::length(*n) > 1e-6) { p.normals.iter().map(|n| math::normalize(*n)).collect() } else { Vec::new() },
            positions: p.positions,
            indices: p.indices,
            tangents: Vec::new(),
            colors: Vec::new(),
        };
        if part.normals.is_empty() {
            part.normals = smooth_normals(&part.positions, &part.indices);
        }
        model.parts.push(part);
    }
    // OBJ has no unit: small models are metres, large ones millimetres (CAD and printing).
    let extent = model.bounds().size().iter().cloned().fold(0.0f32, f32::max);
    model.unit_m = if extent > 20.0 { 0.001 } else { 1.0 };
    Ok(model)
}

fn load_mtl(text: &str, dir: Option<&Path>, model: &mut Model, material_of: &mut HashMap<String, usize>) {
    let mut current: Option<usize> = None;
    let mut textures: HashMap<String, Option<usize>> = HashMap::new();
    let mut texture = |model: &mut Model, rest: &str| -> Option<usize> {
        // Options ("-s 1 1 1", "-bm 0.5") come before the file name.
        let tokens: Vec<&str> = rest.split_whitespace().collect();
        let mut i = 0;
        while i < tokens.len() && tokens[i].starts_with('-') {
            let skip = match tokens[i] {
                "-s" | "-o" | "-t" => 3,
                "-mm" => 2,
                "-blendu" | "-blendv" | "-cc" | "-clamp" | "-bm" | "-boost" | "-texres" | "-imfchan" | "-type" => 1,
                _ => 0,
            };
            i += 1 + skip;
        }
        let file = tokens.get(i..).map(|t| t.join(" ")).unwrap_or_default();
        if file.is_empty() {
            return None;
        }
        if let Some(known) = textures.get(&file) {
            return *known;
        }
        let tex = sibling(dir, &file.replace('\\', "/")).and_then(|p| std::fs::read(p).ok()).and_then(|b| decode_texture(&b)).map(|t| {
            model.textures.push(t);
            model.textures.len() - 1
        });
        textures.insert(file, tex);
        tex
    };
    for raw in text.lines() {
        let line = raw.trim();
        let mut it = line.split_whitespace();
        let Some(key) = it.next() else { continue };
        let rest = line[key.len()..].trim();
        if key == "newmtl" {
            let at = *material_of.entry(rest.to_string()).or_insert_with(|| {
                model.materials.push(Material { name: rest.to_string(), ..Material::default() });
                model.materials.len() - 1
            });
            current = Some(at);
            continue;
        }
        let Some(m) = current else { continue };
        let f = floats(rest.split_whitespace());
        match key {
            "Kd" if f.len() >= 3 => {
                let a = model.materials[m].base_color[3];
                model.materials[m].base_color = [srgb_to_linear(f[0]), srgb_to_linear(f[1]), srgb_to_linear(f[2]), a];
            }
            "Ke" if f.len() >= 3 => model.materials[m].emissive = [f[0], f[1], f[2]],
            "d" if !f.is_empty() => model.materials[m].base_color[3] = f[0].clamp(0.0, 1.0),
            "Tr" if !f.is_empty() => model.materials[m].base_color[3] = (1.0 - f[0]).clamp(0.0, 1.0),
            "Ns" if !f.is_empty() => model.materials[m].roughness = (2.0 / (f[0].max(0.0) + 2.0)).sqrt().clamp(0.04, 1.0),
            "Pr" if !f.is_empty() => model.materials[m].roughness = f[0].clamp(0.0, 1.0),
            "Pm" if !f.is_empty() => model.materials[m].metallic = f[0].clamp(0.0, 1.0),
            "map_Kd" => {
                model.materials[m].base_color_tex = texture(model, rest);
                if model.materials[m].base_color_tex.is_some() {
                    let a = model.materials[m].base_color[3];
                    model.materials[m].base_color = [1.0, 1.0, 1.0, a];
                }
            }
            "map_Ke" => {
                model.materials[m].emissive_tex = texture(model, rest);
                if model.materials[m].emissive_tex.is_some() && model.materials[m].emissive == [0.0; 3] {
                    model.materials[m].emissive = [1.0; 3];
                }
            }
            "norm" | "map_Bump" | "bump" | "map_bump" => {
                // A height map read as a normal map looks wrong: only files that are normal maps.
                let lower = rest.to_lowercase();
                if key == "norm" || lower.contains("normal") || lower.contains("nrm") || lower.contains("_n.") {
                    model.materials[m].normal_tex = texture(model, rest);
                }
            }
            _ => {}
        }
        if model.materials[m].base_color[3] < 0.999 {
            model.materials[m].alpha = AlphaMode::Blend;
        }
    }
}

// ----------------------------------------------------------------------------- STL ----

/// STL is Z-up; the studio is Y-up.
fn z_up(p: [f32; 3]) -> [f32; 3] {
    [p[0], p[2], -p[1]]
}

pub fn load_stl(bytes: &[u8]) -> Result<Model, String> {
    let head = String::from_utf8_lossy(&bytes[..bytes.len().min(512)]).to_string();
    let exact_binary = bytes.len() >= 84 && {
        let n = u32::from_le_bytes([bytes[80], bytes[81], bytes[82], bytes[83]]) as usize;
        84 + n.saturating_mul(50) == bytes.len()
    };
    let ascii = !exact_binary && head.trim_start().starts_with("solid");
    let mut part = Part { name: "model".into(), ..Part::default() };
    let mut push = |tri: [[f32; 3]; 3]| {
        let t = [z_up(tri[0]), z_up(tri[1]), z_up(tri[2])];
        let n = math::normalize(math::cross(math::sub(t[1], t[0]), math::sub(t[2], t[0])));
        for p in t {
            part.indices.push(part.positions.len() as u32);
            part.positions.push(p);
            part.normals.push(n);
        }
    };
    if ascii {
        let text = String::from_utf8_lossy(bytes);
        let mut corner: Vec<[f32; 3]> = Vec::with_capacity(3);
        for line in text.lines() {
            let line = line.trim();
            if let Some(rest) = line.strip_prefix("vertex") {
                let f = floats(rest.split_whitespace());
                if f.len() == 3 {
                    corner.push([f[0], f[1], f[2]]);
                }
            } else if line.starts_with("endfacet") {
                if corner.len() == 3 {
                    push([corner[0], corner[1], corner[2]]);
                }
                corner.clear();
            }
        }
    } else {
        if bytes.len() < 84 {
            return Err("that STL file is cut short".into());
        }
        let n = (u32::from_le_bytes([bytes[80], bytes[81], bytes[82], bytes[83]]) as usize).min((bytes.len() - 84) / 50);
        if n > MAX_TRIANGLES {
            return Err(format!("that model has more than {} million triangles; I draw up to that many", MAX_TRIANGLES / 1_000_000));
        }
        let f = |o: usize| f32::from_le_bytes([bytes[o], bytes[o + 1], bytes[o + 2], bytes[o + 3]]);
        for i in 0..n {
            let o = 84 + i * 50 + 12;
            let v = |k: usize| [f(o + k * 12), f(o + k * 12 + 4), f(o + k * 12 + 8)];
            push([v(0), v(1), v(2)]);
        }
    }
    let name = head.trim_start().strip_prefix("solid").map(|s| s.lines().next().unwrap_or("").trim().to_string()).filter(|s| !s.is_empty() && ascii).unwrap_or_default();
    Ok(Model { name, parts: vec![part], materials: vec![Material { name: "print".into(), base_color: [0.62, 0.64, 0.68, 1.0], metallic: 0.0, roughness: 0.55, ..Material::default() }], unit_m: 0.001, ..Model::default() })
}

// ---------------------------------------------------------------------- ZIP + inflate ----

/// The files of a ZIP archive (stored or deflated), as (name, bytes).
pub fn unzip(bytes: &[u8]) -> Result<Vec<(String, Vec<u8>)>, String> {
    let u16_at = |o: usize| -> Option<usize> { bytes.get(o..o + 2).map(|b| u16::from_le_bytes([b[0], b[1]]) as usize) };
    let u32_at = |o: usize| -> Option<usize> { bytes.get(o..o + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as usize) };
    let floor = bytes.len().saturating_sub(65_557);
    let eocd = (floor..bytes.len().saturating_sub(21)).rev().find(|&o| u32_at(o) == Some(0x0605_4b50)).ok_or("that ZIP has no directory")?;
    let entries = u16_at(eocd + 10).ok_or("a broken ZIP")?;
    let mut at = u32_at(eocd + 16).ok_or("a broken ZIP")?;
    let mut out = Vec::new();
    let mut total = 0usize;
    for _ in 0..entries.min(20_000) {
        if u32_at(at) != Some(0x0201_4b50) {
            return Err("a broken ZIP directory".into());
        }
        let method = u16_at(at + 10).ok_or("a broken ZIP")?;
        let csize = u32_at(at + 20).ok_or("a broken ZIP")?;
        let usize_ = u32_at(at + 24).ok_or("a broken ZIP")?;
        let (nlen, elen, clen) = (u16_at(at + 28).ok_or("a broken ZIP")?, u16_at(at + 30).ok_or("a broken ZIP")?, u16_at(at + 32).ok_or("a broken ZIP")?);
        let local = u32_at(at + 42).ok_or("a broken ZIP")?;
        let name = String::from_utf8_lossy(bytes.get(at + 46..at + 46 + nlen).ok_or("a broken ZIP")?).to_string();
        at += 46 + nlen + elen + clen;
        if name.ends_with('/') {
            continue;
        }
        if csize == 0xFFFF_FFFF || usize_ == 0xFFFF_FFFF {
            return Err("that ZIP is a ZIP64 archive, which I can't open".into());
        }
        total += usize_;
        if total > 2_000_000_000 {
            return Err("that ZIP unpacks to more than 2 GB".into());
        }
        if u32_at(local) != Some(0x0403_4b50) {
            return Err("a broken ZIP entry".into());
        }
        let start = local + 30 + u16_at(local + 26).ok_or("a broken ZIP")? + u16_at(local + 28).ok_or("a broken ZIP")?;
        let data = bytes.get(start..start + csize).ok_or("a ZIP entry runs past the file")?;
        let content = match method {
            0 => data.to_vec(),
            8 => inflate(data, usize_)?,
            other => return Err(format!("'{name}' is packed with method {other}, which I can't unpack")),
        };
        out.push((name, content));
    }
    Ok(out)
}

/// Unpacks a ZIP into `dir` (names that would climb out of it are skipped) and returns the
/// model file inside: a `.gltf`/`.glb` first (`scene.gltf` before others), then OBJ, then STL.
pub fn unpack_zip(bytes: &[u8], dir: &Path) -> Result<PathBuf, String> {
    let files = unzip(bytes)?;
    std::fs::create_dir_all(dir).map_err(|e| format!("I couldn't unpack the archive: {e}"))?;
    let mut models: Vec<PathBuf> = Vec::new();
    for (name, content) in files {
        let rel = Path::new(&name);
        if rel.is_absolute() || rel.components().any(|c| !matches!(c, std::path::Component::Normal(_))) {
            continue;
        }
        let path = dir.join(rel);
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        std::fs::write(&path, content).map_err(|e| format!("I couldn't unpack {name}: {e}"))?;
        if Format::is_model_file(&path) {
            models.push(path);
        }
    }
    let rank = |p: &PathBuf| match Format::of_path(p) {
        Some(Format::Gltf) if p.file_name().is_some_and(|n| n == "scene.gltf") => 0,
        Some(Format::Gltf | Format::Glb) => 1,
        Some(Format::Obj) => 2,
        _ => 3,
    };
    models.sort_by_key(|p| (rank(p), p.components().count()));
    models.into_iter().next().ok_or_else(|| "the archive holds no model file".to_string())
}

struct Bits<'a> {
    data: &'a [u8],
    pos: usize,
    buf: u64,
    count: u32,
}

impl Bits<'_> {
    fn fill(&mut self, n: u32) -> Result<(), String> {
        while self.count < n {
            let byte = match self.data.get(self.pos) {
                Some(b) => *b,
                None if self.pos < self.data.len() + 8 => 0,
                None => return Err("the compressed data ends early".into()),
            };
            self.pos += 1;
            self.buf |= (byte as u64) << self.count;
            self.count += 8;
        }
        Ok(())
    }

    fn bits(&mut self, n: u32) -> Result<u32, String> {
        if n == 0 {
            return Ok(0);
        }
        self.fill(n)?;
        let v = (self.buf & ((1u64 << n) - 1)) as u32;
        self.buf >>= n;
        self.count -= n;
        Ok(v)
    }

    fn align(&mut self) {
        let drop = self.count % 8;
        self.buf >>= drop;
        self.count -= drop;
    }
}

/// A canonical Huffman code as one lookup table: entry = symbol << 4 | length (0: no code).
struct Huffman {
    table: Vec<u16>,
    bits: u32,
}

impl Huffman {
    fn new(lengths: &[u8]) -> Result<Huffman, String> {
        let bits = lengths.iter().copied().max().unwrap_or(0) as u32;
        if bits == 0 {
            return Ok(Huffman { table: vec![0; 2], bits: 1 });
        }
        if bits > 15 {
            return Err("a Huffman code longer than 15 bits".into());
        }
        let mut count = [0u32; 16];
        for &l in lengths {
            count[l as usize] += 1;
        }
        count[0] = 0;
        let mut next = [0u32; 16];
        let mut code = 0u32;
        for len in 1..16 {
            code = (code + count[len - 1]) << 1;
            next[len] = code;
        }
        let mut table = vec![0u16; 1 << bits];
        for (sym, &len) in lengths.iter().enumerate() {
            if len == 0 {
                continue;
            }
            let len = len as u32;
            let c = next[len as usize];
            next[len as usize] += 1;
            if c >= (1 << len) {
                return Err("an over-full Huffman code".into());
            }
            let rev = c.reverse_bits() >> (32 - len);
            let mut k = rev;
            while k < (1 << bits) {
                table[k as usize] = ((sym as u16) << 4) | len as u16;
                k += 1 << len;
            }
        }
        Ok(Huffman { table, bits })
    }

    fn decode(&self, b: &mut Bits) -> Result<u16, String> {
        b.fill(self.bits)?;
        let e = self.table[(b.buf & ((1u64 << self.bits) - 1)) as usize];
        if e == 0 {
            return Err("an invalid Huffman code".into());
        }
        let len = (e & 15) as u32;
        b.buf >>= len;
        b.count -= len;
        Ok(e >> 4)
    }
}

const LEN_BASE: [u16; 29] = [3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131, 163, 195, 227, 258];
const LEN_EXTRA: [u8; 29] = [0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0];
const DIST_BASE: [u16; 30] = [1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537, 2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577];
const DIST_EXTRA: [u8; 30] = [0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13, 13];

/// Raw DEFLATE (RFC 1951), as in ZIP entries.
pub fn inflate(data: &[u8], size_hint: usize) -> Result<Vec<u8>, String> {
    let mut out: Vec<u8> = Vec::with_capacity(size_hint.min(1 << 30));
    let mut b = Bits { data, pos: 0, buf: 0, count: 0 };
    loop {
        let last = b.bits(1)?;
        match b.bits(2)? {
            0 => {
                b.align();
                let len = b.bits(16)? as usize;
                let nlen = b.bits(16)? as usize;
                if len != (!nlen & 0xFFFF) {
                    return Err("a stored block with a broken length".into());
                }
                for _ in 0..len {
                    out.push(b.bits(8)? as u8);
                }
            }
            1 => {
                let mut lit = [0u8; 288];
                lit[..144].fill(8);
                lit[144..256].fill(9);
                lit[256..280].fill(7);
                lit[280..].fill(8);
                let (lit, dist) = (Huffman::new(&lit)?, Huffman::new(&[5u8; 30])?);
                inflate_block(&mut b, &mut out, &lit, &dist)?;
            }
            2 => {
                let hlit = b.bits(5)? as usize + 257;
                let hdist = b.bits(5)? as usize + 1;
                let hclen = b.bits(4)? as usize + 4;
                const ORDER: [usize; 19] = [16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15];
                let mut cl = [0u8; 19];
                for &o in ORDER.iter().take(hclen) {
                    cl[o] = b.bits(3)? as u8;
                }
                let cl = Huffman::new(&cl)?;
                let mut lengths: Vec<u8> = Vec::with_capacity(hlit + hdist);
                while lengths.len() < hlit + hdist {
                    match cl.decode(&mut b)? {
                        s @ 0..=15 => lengths.push(s as u8),
                        16 => {
                            let prev = *lengths.last().ok_or("a repeat with nothing to repeat")?;
                            for _ in 0..3 + b.bits(2)? {
                                lengths.push(prev);
                            }
                        }
                        17 => lengths.extend(std::iter::repeat(0).take(3 + b.bits(3)? as usize)),
                        18 => lengths.extend(std::iter::repeat(0).take(11 + b.bits(7)? as usize)),
                        _ => return Err("a broken code-length code".into()),
                    }
                }
                if lengths.len() > hlit + hdist {
                    return Err("code lengths run past their count".into());
                }
                let lit = Huffman::new(&lengths[..hlit])?;
                let dist = Huffman::new(&lengths[hlit..])?;
                inflate_block(&mut b, &mut out, &lit, &dist)?;
            }
            _ => return Err("an invalid block type".into()),
        }
        if last == 1 {
            break;
        }
    }
    Ok(out)
}

fn inflate_block(b: &mut Bits, out: &mut Vec<u8>, lit: &Huffman, dist: &Huffman) -> Result<(), String> {
    loop {
        let sym = lit.decode(b)?;
        match sym {
            0..=255 => out.push(sym as u8),
            256 => return Ok(()),
            257..=285 => {
                let i = (sym - 257) as usize;
                let len = LEN_BASE[i] as usize + b.bits(LEN_EXTRA[i] as u32)? as usize;
                let d = dist.decode(b)? as usize;
                if d >= 30 {
                    return Err("an invalid distance code".into());
                }
                let back = DIST_BASE[d] as usize + b.bits(DIST_EXTRA[d] as u32)? as usize;
                if back > out.len() {
                    return Err("a distance past the start".into());
                }
                let start = out.len() - back;
                if back >= len {
                    out.extend_from_within(start..start + len);
                } else {
                    for k in 0..len {
                        let byte = out[start + k];
                        out.push(byte);
                    }
                }
            }
            _ => return Err("an invalid literal/length code".into()),
        }
    }
}

// ---------------------------------------------------------------------------- export ----

/// What an export wrote.
#[derive(Debug, Clone, PartialEq)]
pub struct Exported {
    pub path: PathBuf,
    pub triangles: usize,
    /// Width, height, depth in metres, as written.
    pub size_m: [f32; 3],
}

/// The model in metres, centred on x/z and standing on y = 0, scaled by `scale` (1 = 1:1).
fn placed(model: &Model, scale: f32) -> (math::M4, [f32; 3]) {
    let b = model.bounds();
    let c = b.centre();
    let k = model.unit_m * scale;
    let m = math::mul(&math::scaling([k, k, k]), &math::translate([-c[0], -b.min[1], -c[2]]));
    let s = b.size();
    (m, [s[0] * k, s[1] * k, s[2] * k])
}

fn file_name_of(path: &Path) -> String {
    path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "model".into())
}

/// Binary STL in millimetres, Z up (what slicers expect).
pub fn export_stl(model: &Model, path: &Path, scale: f32) -> Result<Exported, String> {
    let (m, size) = placed(model, scale);
    let tris = model.triangles();
    let mut out: Vec<u8> = Vec::with_capacity(84 + tris * 50);
    let mut header = format!("J.A.R.V.I.S. 3D studio: {}", model.name).into_bytes();
    header.resize(80, b' ');
    out.extend_from_slice(&header);
    out.extend_from_slice(&(tris as u32).to_le_bytes());
    // Y-up metres → Z-up millimetres.
    let mm = |p: [f32; 3]| {
        let q = math::point(&m, p);
        [q[0] * 1000.0, -q[2] * 1000.0, q[1] * 1000.0]
    };
    for part in &model.parts {
        for t in part.indices.chunks_exact(3) {
            let v = [mm(part.positions[t[0] as usize]), mm(part.positions[t[1] as usize]), mm(part.positions[t[2] as usize])];
            let n = math::normalize(math::cross(math::sub(v[1], v[0]), math::sub(v[2], v[0])));
            for f in n.iter().chain(v.iter().flatten()) {
                out.extend_from_slice(&f.to_le_bytes());
            }
            out.extend_from_slice(&[0, 0]);
        }
    }
    std::fs::write(path, out).map_err(|e| format!("I couldn't write {}: {e}", path.display()))?;
    Ok(Exported { path: path.to_path_buf(), triangles: tris, size_m: size })
}

fn png_bytes(t: &Texture) -> Option<Vec<u8>> {
    let img = image::RgbaImage::from_raw(t.width, t.height, t.rgba.as_ref().clone())?;
    let mut buf = std::io::Cursor::new(Vec::new());
    img.write_to(&mut buf, image::ImageFormat::Png).ok()?;
    Some(buf.into_inner())
}

/// OBJ + MTL (+ PNG textures) in metres, Y up.
pub fn export_obj(model: &Model, path: &Path, scale: f32) -> Result<Exported, String> {
    use std::fmt::Write as _;
    let (m, size) = placed(model, scale);
    let stem = file_name_of(path);
    let dir = path.parent().unwrap_or(Path::new("."));
    let mut obj = format!("# J.A.R.V.I.S. 3D studio: {}\nmtllib {stem}.mtl\n", model.name);
    let mut mtl = String::from("# J.A.R.V.I.S. 3D studio\n");
    let mut written: HashMap<usize, String> = HashMap::new();
    for (i, mat) in model.materials.iter().enumerate() {
        let s = |c: f32| linear_to_srgb(c);
        let _ = writeln!(mtl, "newmtl m{i}_{}", mat.name.replace(char::is_whitespace, "_"));
        let base = match mat.base_color_tex.filter(|_| mat.triplanar).and_then(|t| model.textures.get(t)) {
            // A projected finish has no UVs to carry it: its average colour.
            Some(t) => {
                let a = t.average();
                [srgb_to_linear(a[0] as f32 / 255.0) * mat.base_color[0], srgb_to_linear(a[1] as f32 / 255.0) * mat.base_color[1], srgb_to_linear(a[2] as f32 / 255.0) * mat.base_color[2]]
            }
            None => [mat.base_color[0], mat.base_color[1], mat.base_color[2]],
        };
        let _ = writeln!(mtl, "Kd {:.4} {:.4} {:.4}", s(base[0]), s(base[1]), s(base[2]));
        let _ = writeln!(mtl, "Ks {0:.3} {0:.3} {0:.3}\nNs {1:.1}\nd {2:.3}\nPr {3:.3}\nPm {4:.3}", 0.04 + 0.5 * mat.metallic, (2.0 / mat.roughness.max(0.05).powi(2) - 2.0).clamp(1.0, 1000.0), mat.base_color[3], mat.roughness, mat.metallic);
        if mat.emissive.iter().any(|c| *c > 0.0) {
            let _ = writeln!(mtl, "Ke {:.3} {:.3} {:.3}", mat.emissive[0], mat.emissive[1], mat.emissive[2]);
        }
        if let Some(t) = mat.base_color_tex.filter(|_| !mat.triplanar) {
            let name = written.entry(t).or_insert_with(|| format!("{stem}_tex{t}.png")).clone();
            let _ = writeln!(mtl, "map_Kd {name}");
        }
    }
    for (t, name) in &written {
        if let Some(png) = model.textures.get(*t).and_then(png_bytes) {
            std::fs::write(dir.join(name), png).map_err(|e| format!("I couldn't write {name}: {e}"))?;
        }
    }
    let mut base = 1usize;
    for part in &model.parts {
        let _ = writeln!(obj, "o {}", part.name.replace(char::is_whitespace, "_"));
        let mat = &model.materials[part.material.min(model.materials.len() - 1)];
        let _ = writeln!(obj, "usemtl m{}_{}", part.material, mat.name.replace(char::is_whitespace, "_"));
        let nm = math::normal_matrix(&m);
        for p in &part.positions {
            let q = math::point(&m, *p);
            let _ = writeln!(obj, "v {:.6} {:.6} {:.6}", q[0], q[1], q[2]);
        }
        for n in &part.normals {
            let q = math::normalize(math::dir(&nm, *n));
            let _ = writeln!(obj, "vn {:.4} {:.4} {:.4}", q[0], q[1], q[2]);
        }
        let uv = !part.uvs.is_empty();
        for t in &part.uvs {
            let _ = writeln!(obj, "vt {:.5} {:.5}", t[0], 1.0 - t[1]);
        }
        for t in part.indices.chunks_exact(3) {
            let c = |i: u32| {
                let k = base + i as usize;
                if uv {
                    format!("{k}/{k}/{k}")
                } else {
                    format!("{k}//{k}")
                }
            };
            let _ = writeln!(obj, "f {} {} {}", c(t[0]), c(t[1]), c(t[2]));
        }
        base += part.positions.len();
    }
    std::fs::write(dir.join(format!("{stem}.mtl")), mtl).map_err(|e| format!("I couldn't write the MTL: {e}"))?;
    std::fs::write(path, obj).map_err(|e| format!("I couldn't write {}: {e}", path.display()))?;
    Ok(Exported { path: path.to_path_buf(), triangles: model.triangles(), size_m: size })
}

/// GLB (binary glTF 2.0) in metres, Y up, with its materials and UV-mapped textures.
pub fn export_glb(model: &Model, path: &Path, scale: f32) -> Result<Exported, String> {
    let bytes = glb_bytes(model, scale)?;
    std::fs::write(path, bytes).map_err(|e| format!("I couldn't write {}: {e}", path.display()))?;
    Ok(Exported { path: path.to_path_buf(), triangles: model.triangles(), size_m: placed(model, scale).1 })
}

pub fn glb_bytes(model: &Model, scale: f32) -> Result<Vec<u8>, String> {
    let (m, _) = placed(model, scale);
    let nm = math::normal_matrix(&m);
    let mut bin: Vec<u8> = Vec::new();
    let mut views: Vec<Value> = Vec::new();
    let mut accessors: Vec<Value> = Vec::new();
    let mut push_view = |bin: &mut Vec<u8>, data: &[u8], target: Option<u32>| -> usize {
        while bin.len() % 4 != 0 {
            bin.push(0);
        }
        let mut v = json!({ "buffer": 0, "byteOffset": bin.len(), "byteLength": data.len() });
        if let Some(t) = target {
            v["target"] = json!(t);
        }
        bin.extend_from_slice(data);
        views.push(v);
        views.len() - 1
    };
    let f32_bytes = |v: &[f32]| -> Vec<u8> { v.iter().flat_map(|f| f.to_le_bytes()).collect() };
    // Textures that travel with UVs; projected ones become their average colour.
    let mut images: Vec<Value> = Vec::new();
    let mut textures: Vec<Value> = Vec::new();
    let mut tex_index: HashMap<usize, usize> = HashMap::new();
    let mut materials: Vec<Value> = Vec::new();
    for mat in &model.materials {
        let mut pbr = json!({ "metallicFactor": mat.metallic, "roughnessFactor": mat.roughness });
        let mut colour = mat.base_color;
        let mut entry = json!({ "name": mat.name, "doubleSided": mat.double_sided });
        let mut tex = |slot: Option<usize>, bin: &mut Vec<u8>| -> Option<usize> {
            let t = slot.filter(|_| !mat.triplanar)?;
            if let Some(known) = tex_index.get(&t) {
                return Some(*known);
            }
            let png = png_bytes(model.textures.get(t)?)?;
            let view = push_view(bin, &png, None);
            images.push(json!({ "bufferView": view, "mimeType": "image/png" }));
            textures.push(json!({ "source": images.len() - 1, "sampler": 0 }));
            tex_index.insert(t, textures.len() - 1);
            Some(textures.len() - 1)
        };
        if let Some(t) = tex(mat.base_color_tex, &mut bin) {
            pbr["baseColorTexture"] = json!({ "index": t });
        } else if let Some(t) = mat.base_color_tex.and_then(|t| model.textures.get(t)) {
            let a = t.average();
            for c in 0..3 {
                colour[c] *= srgb_to_linear(a[c] as f32 / 255.0);
            }
        }
        if let Some(t) = tex(mat.metallic_roughness_tex, &mut bin) {
            pbr["metallicRoughnessTexture"] = json!({ "index": t });
        }
        if let Some(t) = tex(mat.normal_tex, &mut bin) {
            entry["normalTexture"] = json!({ "index": t, "scale": mat.normal_scale });
        }
        if let Some(t) = tex(mat.emissive_tex, &mut bin) {
            entry["emissiveTexture"] = json!({ "index": t });
        }
        pbr["baseColorFactor"] = json!(colour);
        entry["pbrMetallicRoughness"] = pbr;
        let peak = mat.emissive.iter().cloned().fold(0.0f32, f32::max);
        if peak > 0.0 {
            if peak > 1.0 {
                entry["emissiveFactor"] = json!([mat.emissive[0] / peak, mat.emissive[1] / peak, mat.emissive[2] / peak]);
                entry["extensions"] = json!({ "KHR_materials_emissive_strength": { "emissiveStrength": peak } });
            } else {
                entry["emissiveFactor"] = json!(mat.emissive);
            }
        }
        match mat.alpha {
            AlphaMode::Blend => entry["alphaMode"] = json!("BLEND"),
            AlphaMode::Mask(c) => {
                entry["alphaMode"] = json!("MASK");
                entry["alphaCutoff"] = json!(c);
            }
            AlphaMode::Opaque => {}
        }
        materials.push(entry);
    }
    let mut meshes: Vec<Value> = Vec::new();
    let mut nodes: Vec<Value> = Vec::new();
    for part in &model.parts {
        let pos: Vec<[f32; 3]> = part.positions.iter().map(|p| math::point(&m, *p)).collect();
        let mut b = Bounds::empty();
        for p in &pos {
            b.add(*p);
        }
        let flat: Vec<f32> = pos.iter().flatten().copied().collect();
        let v = push_view(&mut bin, &f32_bytes(&flat), Some(34962));
        accessors.push(json!({ "bufferView": v, "componentType": 5126, "count": pos.len(), "type": "VEC3", "min": b.min, "max": b.max }));
        let mut attributes = json!({ "POSITION": accessors.len() - 1 });
        let normals: Vec<f32> = part.normals.iter().flat_map(|n| math::normalize(math::dir(&nm, *n))).collect();
        let v = push_view(&mut bin, &f32_bytes(&normals), Some(34962));
        accessors.push(json!({ "bufferView": v, "componentType": 5126, "count": part.normals.len(), "type": "VEC3" }));
        attributes["NORMAL"] = json!(accessors.len() - 1);
        if !part.uvs.is_empty() {
            let uv: Vec<f32> = part.uvs.iter().flatten().copied().collect();
            let v = push_view(&mut bin, &f32_bytes(&uv), Some(34962));
            accessors.push(json!({ "bufferView": v, "componentType": 5126, "count": part.uvs.len(), "type": "VEC2" }));
            attributes["TEXCOORD_0"] = json!(accessors.len() - 1);
        }
        if !part.colors.is_empty() && model.materials.get(part.material).is_some_and(|m| m.vertex_colors) {
            let rgb: Vec<f32> = part.colors.iter().flatten().copied().collect();
            let v = push_view(&mut bin, &f32_bytes(&rgb), Some(34962));
            accessors.push(json!({ "bufferView": v, "componentType": 5126, "count": part.colors.len(), "type": "VEC3" }));
            attributes["COLOR_0"] = json!(accessors.len() - 1);
        }
        let idx: Vec<u8> = part.indices.iter().flat_map(|i| i.to_le_bytes()).collect();
        let v = push_view(&mut bin, &idx, Some(34963));
        accessors.push(json!({ "bufferView": v, "componentType": 5125, "count": part.indices.len(), "type": "SCALAR" }));
        meshes.push(json!({ "name": part.name, "primitives": [{ "attributes": attributes, "indices": accessors.len() - 1, "material": part.material.min(model.materials.len().saturating_sub(1)), "mode": 4 }] }));
        nodes.push(json!({ "name": part.name, "mesh": meshes.len() - 1 }));
    }
    while bin.len() % 4 != 0 {
        bin.push(0);
    }
    let mut doc = json!({
        "asset": { "version": "2.0", "generator": "J.A.R.V.I.S. 3D studio" },
        "scene": 0,
        "scenes": [{ "name": model.name, "nodes": (0..nodes.len()).collect::<Vec<_>>() }],
        "nodes": nodes,
        "meshes": meshes,
        "materials": materials,
        "accessors": accessors,
        "bufferViews": views,
        "buffers": [{ "byteLength": bin.len() }],
    });
    if let Some(c) = &model.credit {
        doc["asset"]["extras"] = json!({ "title": c.title, "author": c.author, "license": c.license, "source": c.source });
    }
    if !textures.is_empty() {
        doc["images"] = json!(images);
        doc["textures"] = json!(textures);
        doc["samplers"] = json!([{ "wrapS": 10497, "wrapT": 10497, "magFilter": 9729, "minFilter": 9987 }]);
    }
    if model.materials.iter().any(|m| m.emissive.iter().any(|c| *c > 1.0)) {
        doc["extensionsUsed"] = json!(["KHR_materials_emissive_strength"]);
    }
    let mut js = serde_json::to_vec(&doc).map_err(|e| e.to_string())?;
    while js.len() % 4 != 0 {
        js.push(b' ');
    }
    let total = 12 + 8 + js.len() + 8 + bin.len();
    let mut out = Vec::with_capacity(total);
    out.extend_from_slice(b"glTF");
    out.extend_from_slice(&2u32.to_le_bytes());
    out.extend_from_slice(&(total as u32).to_le_bytes());
    out.extend_from_slice(&(js.len() as u32).to_le_bytes());
    out.extend_from_slice(&0x4E4F_534Au32.to_le_bytes());
    out.extend_from_slice(&js);
    out.extend_from_slice(&(bin.len() as u32).to_le_bytes());
    out.extend_from_slice(&0x004E_4942u32.to_le_bytes());
    out.extend_from_slice(&bin);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn quiet(_: f32) {}

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("jarvis_mesh_io_{tag}_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A unit cube as one part (24 vertices, 12 triangles), 2 m tall in metres.
    fn cube(name: &str) -> Model {
        let mut part = Part { name: name.into(), ..Part::default() };
        let faces: [([f32; 3], [f32; 3], [f32; 3]); 6] = [
            ([1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]),
            ([-1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, -1.0]),
            ([0.0, 1.0, 0.0], [0.0, 0.0, 1.0], [1.0, 0.0, 0.0]),
            ([0.0, -1.0, 0.0], [0.0, 0.0, -1.0], [1.0, 0.0, 0.0]),
            ([0.0, 0.0, 1.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]),
            ([0.0, 0.0, -1.0], [-1.0, 0.0, 0.0], [0.0, 1.0, 0.0]),
        ];
        for (n, u, v) in faces {
            let base = part.positions.len() as u32;
            for (a, b) in [(-1.0, -1.0), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)] {
                part.positions.push(math::add(n, math::add(math::scale(u, a), math::scale(v, b))));
                part.normals.push(n);
                part.uvs.push([(a + 1.0) * 0.5, (b + 1.0) * 0.5]);
            }
            // Counter-clockwise seen from outside: u × v = n.
            let (u_cross_v, flip) = (math::cross(u, v), false);
            let outward = math::dot(u_cross_v, n) > 0.0;
            if outward != flip {
                part.indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
            } else {
                part.indices.extend_from_slice(&[base, base + 2, base + 1, base, base + 3, base + 2]);
            }
        }
        let mut m = Model { name: name.into(), parts: vec![part], materials: vec![Material { name: "red paint".into(), base_color: [0.8, 0.05, 0.05, 1.0], metallic: 0.2, roughness: 0.4, ..Material::default() }], unit_m: 1.0, ..Model::default() };
        m.finish();
        m
    }

    #[test]
    fn an_embedded_gltf_with_a_node_transform_loads() {
        use base64::Engine;
        // One triangle, positions (0,0,0) (1,0,0) (0,1,0), u16 indices, no normals; the node moves
        // it up by 2 and scales it by 3.
        let mut buf: Vec<u8> = Vec::new();
        for f in [0.0f32, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0] {
            buf.extend_from_slice(&f.to_le_bytes());
        }
        for i in [0u16, 1, 2, 0] {
            buf.extend_from_slice(&i.to_le_bytes());
        }
        let uri = format!("data:application/octet-stream;base64,{}", base64::engine::general_purpose::STANDARD.encode(&buf));
        let doc = json!({
            "asset": {"version": "2.0"},
            "scene": 0, "scenes": [{"nodes": [0]}],
            "nodes": [{"name": "Helmet", "mesh": 0, "translation": [0.0, 2.0, 0.0], "scale": [3.0, 3.0, 3.0]}],
            "meshes": [{"primitives": [{"attributes": {"POSITION": 0}, "indices": 1, "material": 0}]}],
            "materials": [{"name": "Gold", "pbrMetallicRoughness": {"baseColorFactor": [1.0, 0.8, 0.2, 1.0], "metallicFactor": 1.0, "roughnessFactor": 0.3}, "emissiveFactor": [0.1, 0.0, 0.0]}],
            "buffers": [{"byteLength": buf.len(), "uri": uri}],
            "bufferViews": [{"buffer": 0, "byteOffset": 0, "byteLength": 36}, {"buffer": 0, "byteOffset": 36, "byteLength": 6}],
            "accessors": [
                {"bufferView": 0, "componentType": 5126, "count": 3, "type": "VEC3", "min": [0,0,0], "max": [1,1,0]},
                {"bufferView": 1, "componentType": 5123, "count": 3, "type": "SCALAR"}
            ]
        });
        let model = load_bytes(&serde_json::to_vec(&doc).unwrap(), Format::Gltf, None, &quiet).unwrap();
        assert_eq!(model.triangles(), 1);
        assert_eq!(model.parts[0].name, "Helmet");
        let b = model.bounds();
        assert!((b.min[1] - 2.0).abs() < 1e-5 && (b.max[1] - 5.0).abs() < 1e-5 && (b.max[0] - 3.0).abs() < 1e-5, "{b:?}");
        // Flat normals were generated: +Z for this winding.
        assert!(model.parts[0].normals.iter().all(|n| (n[2] - 1.0).abs() < 1e-5), "{:?}", model.parts[0].normals);
        let mat = &model.materials[model.parts[0].material];
        assert_eq!(mat.name, "Gold");
        assert!((mat.roughness - 0.3).abs() < 1e-6 && mat.metallic == 1.0 && mat.emissive[0] > 0.0);
        assert_eq!(model.parts_named("helmet"), vec![0]);
    }

    #[test]
    fn glb_export_round_trips_with_materials_and_real_size() {
        let mut model = cube("Cube");
        model.parts[0].name = "Body".into();
        model.textures.push(Texture::new(2, 2, vec![255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 255, 255]));
        model.materials[0].base_color_tex = Some(0);
        model.materials[0].emissive = [2.0, 1.0, 0.0];
        let bytes = glb_bytes(&model, 0.5).unwrap();
        assert_eq!(&bytes[..4], b"glTF");
        let back = load_bytes(&bytes, Format::Glb, None, &quiet).unwrap();
        assert_eq!(back.triangles(), 12);
        assert_eq!(back.parts[0].name, "Body");
        let s = back.size_m();
        assert!(s.iter().all(|v| (v - 1.0).abs() < 1e-4), "2 m cube at 1:2 is 1 m: {s:?}");
        assert!(back.bounds().min[1].abs() < 1e-5, "stands on the ground");
        let mat = &back.materials[back.parts[0].material];
        assert_eq!(mat.name, "red paint");
        assert!(mat.base_color_tex.is_some() && back.textures[mat.base_color_tex.unwrap()].width == 2);
        assert!((mat.emissive[0] - 2.0).abs() < 1e-4, "emissive strength kept: {:?}", mat.emissive);
        assert!(!back.parts[0].uvs.is_empty());
    }

    #[test]
    fn stl_ascii_binary_and_export_round_trip() {
        let ascii = "solid tri\nfacet normal 0 0 1\nouter loop\nvertex 0 0 0\nvertex 10 0 0\nvertex 0 10 0\nendloop\nendfacet\nendsolid tri\n";
        let m = load_bytes(ascii.as_bytes(), Format::sniff(ascii.as_bytes()).unwrap(), None, &quiet).unwrap();
        assert_eq!((m.triangles(), m.unit_m), (1, 0.001));
        assert_eq!(m.name, "tri");
        // Z-up 10 mm square triangle: 10 mm wide, 10 mm deep, flat (Y-up).
        let s = m.size_m();
        assert!((s[0] - 0.01).abs() < 1e-6 && s[1].abs() < 1e-6 && (s[2] - 0.01).abs() < 1e-6, "{s:?}");
        let dir = temp_dir("stl");
        let model = cube("Cube");
        let out = export_stl(&model, &dir.join("cube.stl"), 0.05).unwrap();
        assert_eq!(out.triangles, 12);
        assert!((out.size_m[1] - 0.1).abs() < 1e-5, "2 m × 0.05 = 10 cm: {:?}", out.size_m);
        let bytes = std::fs::read(&out.path).unwrap();
        assert_eq!(bytes.len(), 84 + 12 * 50);
        assert_eq!(Format::sniff(&bytes), Some(Format::Stl));
        let back = load_file(&out.path, &quiet).unwrap();
        assert_eq!(back.triangles(), 12);
        let s = back.size_m();
        assert!(s.iter().all(|v| (v - 0.1).abs() < 1e-4), "{s:?}");
        // The binary STL's normals point outwards: the top face's normal is +Y after the swap.
        let top = back.parts[0].normals.iter().filter(|n| n[1] > 0.99).count();
        assert_eq!(top, 6, "two triangles × three corners face up");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn obj_with_mtl_and_export_round_trip() {
        let dir = temp_dir("obj");
        std::fs::write(dir.join("box.mtl"), "newmtl Shiny\nKd 1 0 0\nNs 900\nd 0.5\nPm 1\n").unwrap();
        let obj = "mtllib box.mtl\nv 0 0 0\nv 1 0 0\nv 1 1 0\nv 0 1 0\nvt 0 0\nvt 1 0\nvt 1 1\nvt 0 1\ng Visor\nusemtl Shiny\nf 1/1 2/2 3/3 4/4\ng Body\nf -4 -2 -1\n";
        std::fs::write(dir.join("box.obj"), obj).unwrap();
        let m = load_file(&dir.join("box.obj"), &quiet).unwrap();
        assert_eq!(m.triangles(), 3);
        assert_eq!(m.parts.len(), 2);
        let visor = m.parts_named("visor");
        assert_eq!(visor.len(), 1);
        let mat = &m.materials[m.parts[visor[0]].material];
        assert_eq!(mat.name, "Shiny");
        assert!((mat.base_color[0] - 1.0).abs() < 1e-6 && mat.base_color[1] == 0.0 && mat.alpha == AlphaMode::Blend && mat.metallic == 1.0 && mat.roughness < 0.1);
        // Quad → two triangles; UV v flipped to the top-left origin.
        assert_eq!(m.parts[visor[0]].uvs[0], [0.0, 1.0]);
        let out = export_obj(&cube("Cube"), &dir.join("cube.obj"), 1.0).unwrap();
        assert!(dir.join("cube.mtl").exists());
        let back = load_file(&out.path, &quiet).unwrap();
        assert_eq!(back.triangles(), 12);
        assert!(back.size_m().iter().all(|v| (v - 2.0).abs() < 1e-4), "{:?}", back.size_m());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn inflate_and_unzip_read_what_zlib_wrote() {
        use base64::Engine;
        // A ZIP made with Python's zipfile (ZIP_DEFLATED, level 9): `scene.gltf` (deflated,
        // dynamic Huffman) and `notes.txt` (stored).
        let zip = base64::engine::general_purpose::STANDARD.decode(ZIP_FIXTURE).unwrap();
        let files = unzip(&zip).unwrap();
        let names: Vec<&str> = files.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, vec!["scene.gltf", "notes.txt"]);
        let gltf = &files[0].1;
        let doc: Value = serde_json::from_slice(gltf).unwrap();
        assert_eq!(doc["asset"]["version"], "2.0");
        assert_eq!(files[1].1, b"stored, not deflated".to_vec());
        // The archive unpacks into a folder and the model inside it loads.
        let dir = temp_dir("zip");
        let file = unpack_zip(&zip, &dir.join("x")).unwrap();
        assert!(file.ends_with("scene.gltf"));
        let model = load_file(&file, &quiet).unwrap();
        assert_eq!(model.triangles(), 1);
        let _ = std::fs::remove_dir_all(dir);
        // Fixed-Huffman and stored blocks (zlib level 1 on a short text).
        let fixed = base64::engine::general_purpose::STANDARD.decode(FIXED_FIXTURE).unwrap();
        assert_eq!(inflate(&fixed, 0).unwrap(), b"Jarvis, render me an Iron Man suit. Jarvis, render me an Iron Man suit.".to_vec());
        assert!(inflate(&[0xFF, 0xFF, 0xFF], 0).is_err(), "garbage is an error, never a panic");
    }

    #[test]
    fn zip_entries_cannot_climb_out_of_their_folder() {
        assert_eq!(sibling(Some(Path::new("C:/models")), "../secret.bin"), None);
        assert_eq!(sibling(Some(Path::new("C:/models")), "C:/Windows/x.png"), None);
        assert_eq!(sibling(Some(Path::new("/m")), "tex%20one.png"), Some(Path::new("/m").join("tex one.png")));
    }

    #[test]
    fn sniffing_and_parts_merging() {
        assert_eq!(Format::sniff(b"glTF\x02\x00\x00\x00"), Some(Format::Glb));
        assert_eq!(Format::sniff(b"# made by hand\nv 0 0 0\nf 1 1 1\n"), Some(Format::Obj));
        assert_eq!(Format::sniff(b"hello"), None);
        let mut m = Model { unit_m: 1.0, ..Model::default() };
        m.materials = vec![Material::default(), Material { name: "b".into(), ..Material::default() }];
        for i in 0..MAX_PARTS + 10 {
            m.parts.push(Part { name: format!("p{i}"), positions: vec![[0.0; 3], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]], indices: vec![0, 1, 2], material: i % 2, ..Part::default() });
        }
        m.finish();
        assert_eq!(m.parts.len(), 2, "thousands of parts merge by material");
        assert_eq!(m.triangles(), MAX_PARTS + 10);
    }

    /// Optional: real files from a folder (`JARVIS_MODEL_FIXTURES`), e.g. the Khronos samples.
    #[test]
    #[ignore = "reads real model files from JARVIS_MODEL_FIXTURES"]
    fn real_files_load() {
        let dir = std::env::var("JARVIS_MODEL_FIXTURES").expect("JARVIS_MODEL_FIXTURES");
        for entry in std::fs::read_dir(dir).unwrap().flatten() {
            let path = entry.path();
            if !Format::is_model_file(&path) && Format::of_path(&path) != Some(Format::Zip) {
                continue;
            }
            let started = std::time::Instant::now();
            let m = load_file(&path, &quiet).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
            println!("{:<40} {:>8} tris {:>3} parts {:>2} textures {:?} m in {} ms", path.file_name().unwrap().to_string_lossy(), m.triangles(), m.parts.len(), m.textures.len(), m.size_m(), started.elapsed().as_millis());
        }
    }

    const ZIP_FIXTURE: &str = "UEsDBBQAAAAIAAAAIQAwIrukewEAADkEAAAKAAAAc2NlbmUuZ2x0Zo1TTW/CMAy98ytQzt1GKasmph06tgMSGmhDXCYOaUm7SDSpEpcPof73JSmEthREDo39Xmw/J+6h0+0iLCUBNOwelKPcDRGScqYA1H/sIadEE8KIwMCFxmO6g1wQpKhC80hGilZM7+xJ5f6a2DKvwhlfVWC9ekdrafZCfZcmQ/3kOQFOdRU0FxSzZE2O4hSTEvmn6zfyaLgtUSZoSoFuGnIO1tLXAiBomIM5U2UUN5v+jOfj6ZctWa7CqSagbEUjE+1auLjScJjHsbr3S6XhHsiEsAR0e4OBbTgXVN/ECgMe4ixb0wiDerUnHgGBBwmC4PQ1xJL4Aye4WKNgtr1Ek4+Xk/keBGO9v6FWoQtKtm1iDXmaA6t/GsflhNVh25bndyq3d3c2z29P5zck40i9guTiqmDdTa1MxNOMM8Jgvs/0wD27fb9C5syUtwiUp9Dic+RVRpKy+qw7reZpIM5xeFeLc502s/HnODf6cm/15d3R188omATflUnoFP9QSwMEFAAAAAAAAAAhAN5q+nIUAAAAFAAAAAkAAABub3Rlcy50eHRzdG9yZWQsIG5vdCBkZWZsYXRlZFBLAQIUABQAAAAIAAAAIQAwIrukewEAADkEAAAKAAAAAAAAAAAAAACAAQAAAABzY2VuZS5nbHRmUEsBAhQAFAAAAAAAAAAhAN5q+nIUAAAAFAAAAAkAAAAAAAAAAAAAAIABowEAAG5vdGVzLnR4dFBLBQYAAAAAAgACAG8AAADeAQAAAAA=";
    const FIXED_FIXTURE: &str = "80osKsss1lEoSs1LSS1SyE1VSMxT8CzKz1PwTcxTKC7NLNFTIEINAA==";
}
