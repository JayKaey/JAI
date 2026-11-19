//! The Earth's globe on the GPU (`gui_earth` adds it to the map every frame the globe shows).
//!
//! One egui paint callback (`egui_glow::CallbackFn`, no new crates) draws the whole map
//! rectangle with a fragment shader that ray-casts the sphere per pixel: a perfectly round,
//! anti-aliased limb, no mesh seams. The surface samples
//!
//! * an equirectangular day texture (Blue Marble, 8192 x 4096 as DXT1 when the driver has S3TC,
//!   otherwise 4096 x 2048 RGBA; a 2048 one first so the globe is never blank), the lights of the
//!   night side (VIIRS Black Marble) and a mask (land and, as thin lines, borders and coasts),
//! * two clipmaps of Web Mercator tiles around the point in view, 8 x 8 tiles each, addressed as
//!   a torus (a tile's slot is its x and y modulo 8, so moving the window by a tile replaces one
//!   row or column): "detail" (the imagery up close, shaded relief composited in) and "overlay"
//!   (labels and borders, rain radar, infrared clouds; premultiplied, drawn over the lit ground),
//!
//! lit by the real sun (day and night, a twilight band, city lights, a glint on the water) under
//! an atmosphere in the theme's tint. The holographic look is drawn from the same data: the coast
//! is the 0.5 line of the land mask and the borders a distance to their raster lines, both
//! measured in screen pixels, so they stay one pixel thin at any zoom (the coast with a soft glow
//! a few pixels either side); the land carries a dot grid
//! whose spacing follows the zoom. Below ~1 600 km the same pass draws the base of the flat
//! Web Mercator map (its tiles are egui images on top).
//!
//! Uploads are budgeted per frame (a few MB of the base textures, six clipmap tiles); everything
//! is released when the Earth closes (`request_release` + `janitor`). The offscreen HUD and the
//! tests never run the callback: `gui_earth` keeps drawing its CPU globe until a callback has
//! actually drawn (`proven`), and for good when the shader would not build (`failed`).

use eframe::egui::{self, Rect};
use eframe::egui_glow;
use eframe::glow::{self, HasContext};
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use tracing::{info, warn};

/// Tiles per side of a clipmap.
pub const CLIP_N: u32 = 8;
pub const TILE_PX: u32 = 256;
/// Base texture bytes uploaded per frame at most.
const UPLOAD_BYTES_PER_FRAME: usize = 6 * 1024 * 1024;
/// Clipmap tiles uploaded per frame at most.
const ATLAS_TILES_PER_FRAME: usize = 6;

// ------------------------------------------------------------------------- the images ----

/// The globe's equirectangular textures.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Slot {
    Day,
    Night,
    Mask,
    /// The shaded relief (the terrain globe).
    Relief,
    /// A day's true colour (historic imagery by date).
    Historic,
}

const SLOTS: [Slot; 5] = [Slot::Day, Slot::Night, Slot::Mask, Slot::Relief, Slot::Historic];

impl Slot {
    fn index(self) -> usize {
        self as usize
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// S3TC DXT1: 4 bits a pixel, RGB.
    Dxt1,
    Rgba8,
    Rg8,
    R8,
}

impl Format {
    /// Bytes of a level of `w` x `h`.
    pub fn level_bytes(self, w: u32, h: u32) -> usize {
        match self {
            Format::Dxt1 => (w.div_ceil(4).max(1) * h.div_ceil(4).max(1) * 8) as usize,
            Format::Rgba8 => (w * h * 4) as usize,
            Format::Rg8 => (w * h * 2) as usize,
            Format::R8 => (w * h) as usize,
        }
    }

    fn channels(self) -> usize {
        match self {
            Format::Dxt1 | Format::Rgba8 => 4,
            Format::Rg8 => 2,
            Format::R8 => 1,
        }
    }
}

/// One mip level.
#[derive(Debug, Clone, PartialEq)]
pub struct Level {
    pub w: u32,
    pub h: u32,
    pub data: Vec<u8>,
}

/// A texture's pixels: its levels, the finest first.
#[derive(Debug, Clone, PartialEq)]
pub struct GpuImage {
    pub format: Format,
    pub levels: Vec<Level>,
}

impl GpuImage {
    pub fn bytes(&self) -> usize {
        self.levels.iter().map(|l| l.data.len()).sum()
    }

    /// An uncompressed image (`channels` bytes a pixel) with its whole mip chain.
    pub fn with_mips(format: Format, w: u32, h: u32, data: Vec<u8>) -> Self {
        let ch = format.channels();
        let mut levels = vec![Level { w, h, data }];
        while let Some(last) = levels.last() {
            if last.w == 1 && last.h == 1 {
                break;
            }
            let (data, w2, h2) = downsample(&last.data, last.w as usize, last.h as usize, ch);
            levels.push(Level { w: w2 as u32, h: h2 as u32, data });
        }
        Self { format, levels }
    }

    /// RGBA pixels as DXT1 with the whole mip chain (encoded on up to `threads` threads).
    pub fn dxt1_with_mips(w: u32, h: u32, rgba: Vec<u8>, threads: usize) -> Self {
        let mut levels = Vec::new();
        let (mut cur, mut cw, mut ch) = (rgba, w as usize, h as usize);
        loop {
            levels.push(Level { w: cw as u32, h: ch as u32, data: dxt1_encode_parallel(&cur, cw, ch, threads) });
            if cw == 1 && ch == 1 {
                break;
            }
            let (next, nw, nh) = downsample(&cur, cw, ch, 4);
            (cur, cw, ch) = (next, nw, nh);
        }
        Self { format: Format::Dxt1, levels }
    }

    /// The image as bytes for the disk cache ("JEGI", format, then each level).
    pub fn to_blob(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.bytes() + 16 + 12 * self.levels.len());
        out.extend_from_slice(b"JEGI");
        out.extend_from_slice(&(self.format as u32).to_le_bytes());
        out.extend_from_slice(&(self.levels.len() as u32).to_le_bytes());
        for l in &self.levels {
            out.extend_from_slice(&l.w.to_le_bytes());
            out.extend_from_slice(&l.h.to_le_bytes());
            out.extend_from_slice(&(l.data.len() as u32).to_le_bytes());
            out.extend_from_slice(&l.data);
        }
        out
    }

    pub fn from_blob(bytes: &[u8]) -> Option<Self> {
        let u32_at = |i: usize| bytes.get(i..i + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]));
        if bytes.get(0..4)? != b"JEGI" {
            return None;
        }
        let format = match u32_at(4)? {
            0 => Format::Dxt1,
            1 => Format::Rgba8,
            2 => Format::Rg8,
            3 => Format::R8,
            _ => return None,
        };
        let count = u32_at(8)? as usize;
        let mut at = 12;
        let mut levels = Vec::with_capacity(count.min(32));
        for _ in 0..count.min(32) {
            let (w, h, len) = (u32_at(at)?, u32_at(at + 4)?, u32_at(at + 8)? as usize);
            at += 12;
            if len != format.level_bytes(w, h) {
                return None;
            }
            levels.push(Level { w, h, data: bytes.get(at..at + len)?.to_vec() });
            at += len;
        }
        (!levels.is_empty()).then_some(Self { format, levels })
    }
}

/// Half the size in each direction (an odd last row or column joins its neighbour); a side
/// already 1 stays 1.
pub fn downsample(src: &[u8], w: usize, h: usize, ch: usize) -> (Vec<u8>, usize, usize) {
    let (w2, h2) = ((w / 2).max(1), (h / 2).max(1));
    let mut out = vec![0u8; w2 * h2 * ch];
    for y in 0..h2 {
        let (y0, y1) = ((y * 2).min(h - 1), (y * 2 + 1).min(h - 1));
        for x in 0..w2 {
            let (x0, x1) = ((x * 2).min(w - 1), (x * 2 + 1).min(w - 1));
            for c in 0..ch {
                let sum = src[(y0 * w + x0) * ch + c] as u32 + src[(y0 * w + x1) * ch + c] as u32 + src[(y1 * w + x0) * ch + c] as u32 + src[(y1 * w + x1) * ch + c] as u32;
                out[(y * w2 + x) * ch + c] = ((sum + 2) / 4) as u8;
            }
        }
    }
    (out, w2, h2)
}

fn rgb565(c: [f32; 3]) -> u16 {
    let r = (c[0].clamp(0.0, 255.0) * 31.0 / 255.0).round() as u16;
    let g = (c[1].clamp(0.0, 255.0) * 63.0 / 255.0).round() as u16;
    let b = (c[2].clamp(0.0, 255.0) * 31.0 / 255.0).round() as u16;
    (r << 11) | (g << 5) | b
}

fn from565(c: u16) -> [f32; 3] {
    [((c >> 11) & 31) as f32 * 255.0 / 31.0, ((c >> 5) & 63) as f32 * 255.0 / 63.0, (c & 31) as f32 * 255.0 / 31.0]
}

/// One 4x4 block of RGB as DXT1: the end colours on the block's principal colour axis (so a
/// coast's blue sea and brown land both keep their colour), inset a little; four colours.
fn dxt1_block(px: &[[u8; 3]; 16]) -> [u8; 8] {
    let mut mean = [0f32; 3];
    for p in px {
        for c in 0..3 {
            mean[c] += p[c] as f32 / 16.0;
        }
    }
    let mut cov = [[0f32; 3]; 3];
    for p in px {
        let d = [p[0] as f32 - mean[0], p[1] as f32 - mean[1], p[2] as f32 - mean[2]];
        for i in 0..3 {
            for j in 0..3 {
                cov[i][j] += d[i] * d[j];
            }
        }
    }
    // The principal axis by power iteration, from the channel that varies most.
    let k = (0..3).max_by(|a, b| cov[*a][*a].total_cmp(&cov[*b][*b])).unwrap_or(1);
    let mut axis = cov[k];
    for _ in 0..8 {
        let next = [0, 1, 2].map(|i| cov[i][0] * axis[0] + cov[i][1] * axis[1] + cov[i][2] * axis[2]);
        let len = next.iter().map(|v| v * v).sum::<f32>().sqrt();
        if len < 1e-6 {
            break;
        }
        axis = next.map(|v| v / len);
    }
    let norm = axis.iter().map(|v| v * v).sum::<f32>().sqrt();
    let (mut lo, mut hi) = (mean, mean);
    if cov[k][k] > 1e-3 && norm > 1e-6 {
        let axis = axis.map(|v| v / norm);
        let (mut t0, mut t1) = (f32::MAX, f32::MIN);
        for p in px {
            let t = (p[0] as f32 - mean[0]) * axis[0] + (p[1] as f32 - mean[1]) * axis[1] + (p[2] as f32 - mean[2]) * axis[2];
            t0 = t0.min(t);
            t1 = t1.max(t);
        }
        let inset = (t1 - t0) / 16.0;
        lo = [0, 1, 2].map(|i| mean[i] + axis[i] * (t0 + inset));
        hi = [0, 1, 2].map(|i| mean[i] + axis[i] * (t1 - inset));
    }
    let (mut c0, mut c1) = (rgb565(hi), rgb565(lo));
    if c0 < c1 {
        std::mem::swap(&mut c0, &mut c1);
    }
    let mut out = [0u8; 8];
    out[0..2].copy_from_slice(&c0.to_le_bytes());
    out[2..4].copy_from_slice(&c1.to_le_bytes());
    if c0 == c1 {
        return out;
    }
    let (a, b) = (from565(c0), from565(c1));
    let axis = [a[0] - b[0], a[1] - b[1], a[2] - b[2]];
    let len2 = axis.iter().map(|v| v * v).sum::<f32>().max(1e-6);
    let mut indices = 0u32;
    for (i, p) in px.iter().enumerate() {
        let t = ((p[0] as f32 - b[0]) * axis[0] + (p[1] as f32 - b[1]) * axis[1] + (p[2] as f32 - b[2]) * axis[2]) / len2;
        // Palette: 0 = c0, 1 = c1, 2 = 2/3 c0 + 1/3 c1, 3 = 1/3 c0 + 2/3 c1.
        let index = if t > 5.0 / 6.0 {
            0
        } else if t > 0.5 {
            2
        } else if t > 1.0 / 6.0 {
            3
        } else {
            1
        };
        indices |= index << (2 * i);
    }
    out[4..8].copy_from_slice(&indices.to_le_bytes());
    out
}

/// Block rows `rows` of an RGBA image as DXT1.
fn dxt1_rows(rgba: &[u8], w: usize, h: usize, rows: std::ops::Range<usize>) -> Vec<u8> {
    let bw = w.div_ceil(4).max(1);
    let mut out = Vec::with_capacity(rows.len() * bw * 8);
    for by in rows {
        for bx in 0..bw {
            let mut px = [[0u8; 3]; 16];
            for (i, p) in px.iter_mut().enumerate() {
                let (x, y) = ((bx * 4 + i % 4).min(w - 1), (by * 4 + i / 4).min(h - 1));
                let o = (y * w + x) * 4;
                *p = [rgba[o], rgba[o + 1], rgba[o + 2]];
            }
            out.extend_from_slice(&dxt1_block(&px));
        }
    }
    out
}

/// An RGBA image as DXT1 blocks, row by row of blocks.
pub fn dxt1_encode(rgba: &[u8], w: usize, h: usize) -> Vec<u8> {
    dxt1_rows(rgba, w, h, 0..h.div_ceil(4).max(1))
}

fn dxt1_encode_parallel(rgba: &[u8], w: usize, h: usize, threads: usize) -> Vec<u8> {
    let bh = h.div_ceil(4).max(1);
    let threads = threads.clamp(1, 16).min(bh);
    if threads == 1 || w * h < 512 * 512 {
        return dxt1_encode(rgba, w, h);
    }
    let per = bh.div_ceil(threads);
    let parts: Vec<Vec<u8>> = std::thread::scope(|s| {
        let handles: Vec<_> = (0..threads).map(|k| s.spawn(move || dxt1_rows(rgba, w, h, (k * per).min(bh)..((k + 1) * per).min(bh)))).collect();
        handles.into_iter().map(|h| h.join().unwrap_or_default()).collect()
    });
    parts.concat()
}

/// DXT1 blocks back to RGBA (the tests check the encoder with it).
#[cfg(test)]
pub fn dxt1_decode(blocks: &[u8], w: usize, h: usize) -> Vec<u8> {
    let bw = w.div_ceil(4).max(1);
    let mut out = vec![255u8; w * h * 4];
    for (k, block) in blocks.chunks_exact(8).enumerate() {
        let (bx, by) = (k % bw, k / bw);
        let c0 = u16::from_le_bytes([block[0], block[1]]);
        let c1 = u16::from_le_bytes([block[2], block[3]]);
        let (a, b) = (from565(c0), from565(c1));
        let palette = if c0 > c1 {
            [a, b, [0, 1, 2].map(|i| (2.0 * a[i] + b[i]) / 3.0), [0, 1, 2].map(|i| (a[i] + 2.0 * b[i]) / 3.0)]
        } else {
            [a, b, [0, 1, 2].map(|i| (a[i] + b[i]) / 2.0), [0.0; 3]]
        };
        let indices = u32::from_le_bytes([block[4], block[5], block[6], block[7]]);
        for i in 0..16 {
            let (x, y) = (bx * 4 + i % 4, by * 4 + i / 4);
            if x < w && y < h {
                let c = palette[((indices >> (2 * i)) & 3) as usize];
                let o = (y * w + x) * 4;
                for ch in 0..3 {
                    out[o + ch] = c[ch].round() as u8;
                }
            }
        }
    }
    out
}

// ------------------------------------------------------------------------ the camera ----

/// The view's axes in world coordinates (x towards 0°N 0°E, y towards 0°N 90°E, z north) as the
/// columns of a 3x3 matrix: east, north and up at the point in view. A point's view coordinates
/// (`GlobeView::view_coords`) times this matrix are its world coordinates.
pub fn view_basis(lat0: f64, lon0: f64) -> [[f32; 3]; 3] {
    let (la, lo) = (lat0.to_radians(), lon0.to_radians());
    let (s0, c0, sl, cl) = (la.sin(), la.cos(), lo.sin(), lo.cos());
    [[-sl as f32, cl as f32, 0.0], [(-s0 * cl) as f32, (-s0 * sl) as f32, c0 as f32], [(c0 * cl) as f32, (c0 * sl) as f32, s0 as f32]]
}

/// A unit vector towards a lat/lon (degrees), world coordinates.
pub fn world_unit(lat: f64, lon: f64) -> [f32; 3] {
    let (la, lo) = (lat.to_radians(), lon.to_radians());
    [(la.cos() * lo.cos()) as f32, (la.cos() * lo.sin()) as f32, la.sin() as f32]
}

/// A clipmap's window: `CLIP_N` x `CLIP_N` tiles of level `z` from (`x0`, `y0`) on (x wraps).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Clip {
    pub z: u8,
    pub x0: u32,
    pub y0: u32,
}

impl Clip {
    /// The window of level `z` (3 or more: the torus needs the world to be at least `CLIP_N`
    /// tiles wide) around a point, its tile in the middle.
    pub fn around(lat: f64, lon: f64, z: u8) -> Clip {
        let z = z.clamp(3, 22);
        let n = 1i64 << z;
        let (tx, ty) = tile_of(lat, lon, z);
        let half = CLIP_N as i64 / 2;
        let x0 = (tx as i64 - half + 1).rem_euclid(n) as u32;
        let y0 = (ty as i64 - half + 1).clamp(0, n - CLIP_N as i64) as u32;
        Clip { z, x0, y0 }
    }

    pub fn world_tiles(&self) -> u32 {
        1u32 << self.z
    }

    /// The window's tiles, the middle ones first.
    pub fn tiles(&self) -> Vec<(u32, u32)> {
        let n = self.world_tiles();
        let mid = (CLIP_N as f32 - 1.0) / 2.0;
        let mut out: Vec<(u32, u32, f32)> = (0..CLIP_N)
            .flat_map(|j| (0..CLIP_N).map(move |i| (i, j)))
            .map(|(i, j)| ((self.x0 + i) % n, self.y0 + j, (i as f32 - mid).powi(2) + (j as f32 - mid).powi(2)))
            .collect();
        out.sort_by(|a, b| a.2.total_cmp(&b.2));
        out.into_iter().map(|(x, y, _)| (x, y)).collect()
    }

    pub fn contains(&self, x: u32, y: u32) -> bool {
        let n = self.world_tiles();
        (x + n - self.x0) % n < CLIP_N && y >= self.y0 && y < self.y0 + CLIP_N
    }

    /// The slot (0 .. N²) a tile lives in.
    pub fn slot(x: u32, y: u32) -> usize {
        ((y % CLIP_N) * CLIP_N + x % CLIP_N) as usize
    }
}

/// The Web Mercator tile under a point.
pub fn tile_of(lat: f64, lon: f64, z: u8) -> (u32, u32) {
    let n = (1u64 << z) as f64;
    let x = (crate::earth::merc_x(crate::earth::wrap_lon(lon)) * n).floor().clamp(0.0, n - 1.0);
    let y = (crate::earth::merc_y(lat) * n).floor().clamp(0.0, n - 1.0);
    (x as u32, y as u32)
}

/// What a clipmap draws this frame.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct ClipUse {
    pub clip: Option<Clip>,
    /// 0..1: how much of it shows (the detail fades in with the zoom).
    pub mix: f32,
}

impl ClipUse {
    fn uniform(&self) -> [f32; 4] {
        match self.clip {
            Some(c) if self.mix > 0.0 => [c.world_tiles() as f32, c.x0 as f32, c.y0 as f32, self.mix],
            _ => [0.0; 4],
        }
    }
}

/// Everything the shader needs for one frame (points, not pixels; colours 0..1, gamma space).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Frame {
    pub centre: [f32; 2],
    pub f: f32,
    pub d: f32,
    pub basis: [[f32; 3]; 3],
    pub flatness: f32,
    /// The flat map: Web Mercator x of the view's longitude, y of its latitude, the world's width.
    pub flat: [f32; 3],
    /// How far the camera leans back over the flat map and its heading (`earth::FlatView`).
    pub tilt: [f32; 2],
    pub sun: [f32; 3],
    /// 0: the photo-real look, 1: the holographic one (cross-fades between).
    pub holo: f32,
    /// 1: day and night (the photo look); 0: lit evenly.
    pub night: f32,
    pub borders: f32,
    pub glint: f32,
    pub rim: [f32; 3],
    /// The holo palette: deep ocean, shallow ocean, dark land, land, dots, coast, border.
    pub palette: [[f32; 3]; 7],
    /// Which texture is the day side.
    pub day: Slot,
    pub detail: ClipUse,
    pub overlay: ClipUse,
    pub time: f32,
}

// --------------------------------------------------------------------- shared state ----

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Status {
    Unknown,
    Ready,
    Failed,
}

/// What the driver offers (known after the first callback).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Caps {
    pub dxt1: bool,
    pub max_side: u32,
}

struct Tex {
    tex: glow::Texture,
    w: u32,
    h: u32,
    bytes: usize,
}

struct Incoming {
    tex: glow::Texture,
    image: Arc<GpuImage>,
    /// The level being uploaded (the coarsest first) and the next row of it.
    level: usize,
    row: u32,
}

struct AtlasTex {
    tex: glow::Texture,
    valid: glow::Texture,
}

struct Gpu {
    program: glow::Program,
    vao: glow::VertexArray,
    locations: HashMap<&'static str, Option<glow::UniformLocation>>,
    slots: [Option<Tex>; 5],
    incoming: [Option<Incoming>; 5],
    atlas: [Option<AtlasTex>; 2],
    black: glow::Texture,
    clear: glow::Texture,
    storage: bool,
    aniso: Option<f32>,
}

struct Shared {
    gpu: Option<Gpu>,
    status: Status,
    caps: Option<Caps>,
    /// The latest image for each slot, waiting for the GL thread.
    queued: [Option<Arc<GpuImage>>; 5],
    /// A slot has a whole texture on the GPU.
    ready: [bool; 5],
    /// Clipmap tiles waiting: (slot, 256x256 premultiplied RGBA).
    tiles: [VecDeque<(usize, Arc<Vec<u8>>)>; 2],
    valid: [Option<Vec<u8>>; 2],
    release: bool,
    drawn: bool,
}

static SHARED: Mutex<Shared> = Mutex::new(Shared {
    gpu: None,
    status: Status::Unknown,
    caps: None,
    queued: [None, None, None, None, None],
    ready: [false; 5],
    tiles: [VecDeque::new(), VecDeque::new()],
    valid: [None, None],
    release: false,
    drawn: false,
});

fn shared() -> std::sync::MutexGuard<'static, Shared> {
    crate::crash_guard::lock(&SHARED)
}

/// A callback has drawn the globe: the CPU globe is no longer needed.
pub fn proven() -> bool {
    let s = shared();
    s.drawn && s.status == Status::Ready
}

/// The shader would not build on this driver: the CPU globe stays.
pub fn failed() -> bool {
    shared().status == Status::Failed
}

pub fn caps() -> Option<Caps> {
    shared().caps
}

/// A texture for `slot` (it replaces the one there once it is all on the GPU).
pub fn submit(slot: Slot, image: GpuImage) {
    if image.levels.is_empty() {
        return;
    }
    let mut s = shared();
    s.queued[slot.index()] = Some(Arc::new(image));
}

/// Whether a slot has a whole texture (or one on its way).
pub fn has(slot: Slot) -> bool {
    let s = shared();
    s.ready[slot.index()] || s.queued[slot.index()].is_some()
}

pub fn ready(slot: Slot) -> bool {
    shared().ready[slot.index()]
}

/// The clipmaps.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Atlas {
    Detail,
    Overlay,
}

/// A tile (256 x 256 premultiplied RGBA) for a clipmap slot.
pub fn submit_tile(atlas: Atlas, slot: usize, rgba: Vec<u8>) {
    if rgba.len() != (TILE_PX * TILE_PX * 4) as usize || slot >= (CLIP_N * CLIP_N) as usize {
        return;
    }
    let mut s = shared();
    let queue = &mut s.tiles[atlas as usize];
    queue.retain(|(k, _)| *k != slot);
    queue.push_back((slot, Arc::new(rgba)));
}

/// Which slots of a clipmap hold the tile its window wants (N² bytes, 255 = yes).
pub fn submit_valid(atlas: Atlas, valid: Vec<u8>) {
    if valid.len() == (CLIP_N * CLIP_N) as usize {
        shared().valid[atlas as usize] = Some(valid);
    }
}

/// The Earth closed: its textures go at the next paint (`janitor`), nothing waits any more.
pub fn request_release() {
    let mut s = shared();
    s.release = true;
    s.queued = [None, None, None, None, None];
    s.tiles = [VecDeque::new(), VecDeque::new()];
    s.valid = [None, None];
    s.ready = [false; 5];
}

/// Something still goes up to the GPU (frames keep coming until it is there).
pub fn busy() -> bool {
    let s = shared();
    s.queued.iter().any(Option::is_some) || s.tiles.iter().any(|q| !q.is_empty()) || s.valid.iter().any(Option::is_some) || s.gpu.as_ref().is_some_and(|g| g.incoming.iter().any(Option::is_some))
}

/// A release waits for the janitor.
pub fn release_pending() -> bool {
    let s = shared();
    s.release && s.gpu.is_some()
}

/// The GPU memory the globe holds (bytes): its textures and its clipmaps (mips included).
fn held_bytes(g: &Gpu) -> usize {
    let atlas = g.atlas.iter().flatten().count() * (CLIP_N * TILE_PX).pow(2) as usize * 4 * 4 / 3;
    g.slots.iter().flatten().map(|t| t.bytes).sum::<usize>() + atlas
}

/// A paint callback that releases the globe's GPU memory when the Earth has closed (added by
/// `gui_earth` to the root painter once, after the Earth's exit).
pub fn janitor(at: egui::Pos2) -> egui::PaintCallback {
    egui::PaintCallback {
        rect: Rect::from_min_size(at, egui::vec2(1.0, 1.0)),
        callback: Arc::new(egui_glow::CallbackFn::new(|_info, painter| {
            let mut s = shared();
            if s.release {
                release(painter.gl(), &mut s);
            }
        })),
    }
}

/// The globe for one frame over `rect` (the map).
pub fn callback(rect: Rect, frame: Frame) -> egui::PaintCallback {
    egui::PaintCallback {
        rect,
        callback: Arc::new(egui_glow::CallbackFn::new(move |info, painter| {
            let mut s = shared();
            draw(painter.gl(), &info, &frame, &mut s);
        })),
    }
}

// ------------------------------------------------------------------------ the GL side ----

fn release(gl: &glow::Context, s: &mut Shared) {
    if let Some(g) = s.gpu.take() {
        // SAFETY: GL objects of this context, on the GL thread inside a paint callback.
        unsafe {
            for t in g.slots.iter().flatten() {
                gl.delete_texture(t.tex);
            }
            for t in g.incoming.iter().flatten() {
                gl.delete_texture(t.tex);
            }
            for a in g.atlas.iter().flatten() {
                gl.delete_texture(a.tex);
                gl.delete_texture(a.valid);
            }
            gl.delete_texture(g.black);
            gl.delete_texture(g.clear);
            gl.delete_vertex_array(g.vao);
            gl.delete_program(g.program);
        }
        info!("[EARTH] Globe textures released from the GPU");
    }
    s.release = false;
    s.ready = [false; 5];
    if s.status == Status::Ready {
        s.status = Status::Unknown;
    }
}

fn draw(gl: &glow::Context, info: &egui::PaintCallbackInfo, frame: &Frame, s: &mut Shared) {
    if s.status == Status::Failed {
        return;
    }
    if s.release {
        release(gl, s);
    }
    if s.gpu.is_none() {
        // SAFETY: building GL objects on the GL thread inside a paint callback.
        match unsafe { build(gl) } {
            Ok(gpu) => {
                s.caps = Some(Caps { dxt1: gpu_has_dxt(gl), max_side: unsafe { gl.get_parameter_i32(glow::MAX_TEXTURE_SIZE) }.max(2048) as u32 });
                s.gpu = Some(gpu);
                s.status = Status::Ready;
                info!("[EARTH] GPU globe ready ({:?})", s.caps);
            }
            Err(e) => {
                warn!("[EARTH] The GPU globe could not be built, drawing it on the CPU: {e}");
                s.status = Status::Failed;
                return;
            }
        }
    }
    // SAFETY: as above; every GL object used here belongs to this context.
    unsafe {
        gl.pixel_store_i32(glow::UNPACK_ALIGNMENT, 1);
        upload_bases(gl, s);
        upload_atlases(gl, s);
        gl.pixel_store_i32(glow::UNPACK_ALIGNMENT, 4);
        if let Some(gpu) = s.gpu.as_mut() {
            paint(gl, gpu, info, frame);
            s.drawn = true;
        }
    }
}

fn gpu_has_dxt(gl: &glow::Context) -> bool {
    let ext = gl.supported_extensions();
    ext.contains("GL_EXT_texture_compression_s3tc") || ext.contains("GL_EXT_texture_compression_dxt1")
}

const VERTEX: &str = r#"
void main() {
    vec2 p = vec2(float((gl_VertexID << 1) & 2), float(gl_VertexID & 2));
    gl_Position = vec4(p * 2.0 - 1.0, 0.0, 1.0);
}
"#;

const FRAGMENT: &str = r#"
uniform vec2 u_centre;
uniform float u_f;
uniform float u_d;
uniform mat3 u_basis;
uniform float u_ppp;
uniform float u_screen_h;
uniform float u_flatness;
uniform vec3 u_flat;
uniform vec2 u_tilt;
uniform vec3 u_sun;
uniform float u_holo;
uniform float u_night;
uniform float u_borders;
uniform float u_glint;
uniform vec3 u_rim;
uniform vec3 u_ocean_deep;
uniform vec3 u_ocean_shallow;
uniform vec3 u_land_dark;
uniform vec3 u_land;
uniform vec3 u_dot;
uniform vec3 u_coast;
uniform vec3 u_border;
uniform vec4 u_detail;
uniform vec4 u_overlay;
uniform vec2 u_mask_size;
uniform float u_time;
uniform sampler2D t_day;
uniform sampler2D t_night;
uniform sampler2D t_mask;
uniform sampler2D t_detail;
uniform sampler2D t_detail_ok;
uniform sampler2D t_overlay;
uniform sampler2D t_overlay_ok;
out vec4 o_colour;

const float PI = 3.14159265358979;
const float N = 8.0;

vec2 wrap_x(vec2 g) {
    g.x -= floor(g.x + 0.5);
    return g;
}

float lum(vec3 c) {
    return dot(c, vec3(0.299, 0.587, 0.114));
}

// A clipmap at Web Mercator `merc` (premultiplied; alpha 0 where it has nothing).
vec4 clip_sample(sampler2D tex, sampler2D ok, vec4 clip, vec2 merc, vec2 dmx, vec2 dmy) {
    if (clip.w <= 0.0) {
        return vec4(0.0);
    }
    vec2 t = merc * clip.x;
    vec2 rel = vec2(mod(t.x - clip.y, clip.x), t.y - clip.z);
    if (rel.x < 0.0 || rel.x >= N || rel.y < 0.0 || rel.y >= N) {
        return vec4(0.0);
    }
    vec2 uv = t / N;
    float valid = textureLod(ok, uv, 0.0).r;
    float edge = min(min(rel.x, N - rel.x), min(rel.y, N - rel.y));
    float fade = smoothstep(0.0, 0.75, edge) * valid * clip.w;
    vec4 c = textureGrad(tex, uv, dmx * clip.x / N, dmy * clip.x / N);
    return c * fade;
}

// The holo look's dots: a grid in degrees whose spacing doubles as the view zooms out.
float dot_grid(vec2 p, float spacing, float ppd) {
    vec2 cell = (fract(p / spacing + 0.5) - 0.5) * spacing * ppd;
    return 1.0 - smoothstep(0.55, 1.15, length(cell));
}

float dots(vec2 ll, vec2 dllx, vec2 dlly) {
    vec2 p = vec2(ll.y * cos(radians(ll.x)), ll.x);
    float ppd = 1.0 / max(max(length(dllx), length(dlly)), 1e-6);
    float lv = log2(7.0 / ppd);
    float k = floor(lv);
    float f = lv - k;
    return mix(dot_grid(p, exp2(k), ppd), dot_grid(p, exp2(k + 1.0), ppd), f);
}

// The ground at a point: lat/lon `ll` (degrees), equirect `uv`, Web Mercator `merc` (each with
// its change per pixel), the world normal `n` and the way to the eye.
vec3 ground(vec2 ll, vec2 dllx, vec2 dlly, vec2 uv, vec2 dux, vec2 duy, vec2 merc, vec2 dmx, vec2 dmy, vec3 n, vec3 to_eye, float globe) {
    vec3 day = textureGrad(t_day, uv, dux, duy).rgb;
    vec4 mask = textureGrad(t_mask, uv, dux, duy);
    // How the mask changes over a pixel (finite differences: no derivatives of samples).
    float mx = textureGrad(t_mask, uv + dux, dux, duy).r;
    float my = textureGrad(t_mask, uv + duy, dux, duy).r;
    float grad = abs(mx - mask.r) + abs(my - mask.r);
    float coast_px = abs(mask.r - 0.5) / max(grad, 1e-4);
    float land_a = clamp(0.5 + (mask.r - 0.5) / max(grad, 1e-4), 0.0, 1.0);
    float tpp = max(length(dux * u_mask_size), length(duy * u_mask_size));
    float line = tpp < 1.0 ? 1.0 - smoothstep(0.3 * tpp, 0.9 * tpp, 1.0 - mask.g) : mask.g;
    line *= u_borders;
    vec4 detail = clip_sample(t_detail, t_detail_ok, u_detail, merc, dmx, dmy);
    vec3 photo_day = day * (1.0 - detail.a) + detail.rgb;

    // The photo look: lit by the real sun.
    float ndl = dot(n, u_sun);
    float daylight = smoothstep(-0.10, 0.20, ndl);
    float sun_lit = 0.07 + 0.93 * pow(clamp(ndl * 1.15 + 0.12, 0.0, 1.0), 0.8);
    vec3 photo = photo_day * mix(1.0, sun_lit, u_night);
    float lights = pow(textureGrad(t_night, uv, dux, duy).r, 1.35);
    photo += vec3(1.0, 0.78, 0.46) * lights * 1.3 * (1.0 - smoothstep(-0.16, 0.06, ndl)) * u_night;
    photo += vec3(0.50, 0.20, 0.06) * exp(-ndl * ndl / 0.004) * 0.22 * u_night;
    vec3 h = normalize(u_sun + to_eye);
    float water = 1.0 - smoothstep(0.35, 0.65, mask.r);
    photo += vec3(1.0, 0.94, 0.82) * pow(max(dot(n, h), 0.0), 160.0) * water * daylight * u_glint * 0.65;
    photo = mix(photo, vec3(1.0, 0.96, 0.86) * mix(1.0, 0.35 + 0.65 * daylight, u_night), line * 0.55);

    // The holographic look: crisp coasts with a soft glow either side, borders, a dot grid on
    // the land.
    float l = lum(day);
    // (The holo look's day is the shaded relief: the shelves are a lighter blue.)
    vec3 ocean = mix(u_ocean_deep, u_ocean_shallow, smoothstep(0.10, 0.42, l) * 0.7);
    vec3 land = mix(u_land_dark, u_land, 0.55 + 0.45 * l);
    if (detail.a > 0.0) {
        land = mix(land, mix(u_land_dark, u_land * 1.15, lum(detail.rgb / max(detail.a, 1e-3))), 0.6 * detail.a);
    }
    vec3 holo = mix(ocean, land, land_a);
    holo = mix(holo, u_dot, dots(ll, dllx, dlly) * land_a * 0.5);
    holo += u_coast * 0.16 * exp(-0.45 * coast_px);
    holo = mix(holo, u_coast, 1.0 - smoothstep(0.5, 1.5, coast_px));
    holo = mix(holo, u_border, line);
    holo *= mix(1.0, 0.62 + 0.38 * pow(max(dot(n, to_eye), 0.0), 0.6), globe);

    vec3 c = mix(photo, holo, u_holo);
    // Labels, radar and clouds over the ground (dimmer on the night side).
    vec4 over = clip_sample(t_overlay, t_overlay_ok, u_overlay, merc, dmx, dmy);
    vec3 over_rgb = mix(over.rgb, u_coast * lum(over.rgb) * 1.2, u_holo * 0.7);
    return c * (1.0 - over.a) + over_rgb * mix(1.0, 0.55 + 0.45 * daylight, u_night * (1.0 - u_holo));
}

vec2 merc_of(vec2 ll) {
    float lat = clamp(radians(ll.x), -1.4844, 1.4844);
    return vec2(ll.y / 360.0 + 0.5, (1.0 - log(tan(lat) + 1.0 / cos(lat)) / PI) * 0.5);
}

vec4 globe_colour(vec2 s) {
    vec2 q = vec2(s.x - u_centre.x, -(s.y - u_centre.y)) / u_f;
    float a = dot(q, q) + 1.0;
    float b = -2.0 * u_d;
    float c = u_d * u_d - 1.0;
    float disc = b * b - 4.0 * a * c;
    float t = (-b - sqrt(max(disc, 0.0))) / (2.0 * a);
    vec3 pv = normalize(vec3(q * t, u_d - t));
    vec3 n = u_basis * pv;
    vec3 ray = normalize(vec3(q, -1.0));
    vec3 to_eye = u_basis * (-ray);
    vec2 ll = vec2(degrees(asin(clamp(n.z, -1.0, 1.0))), degrees(atan(n.y, n.x)));
    vec2 uv = vec2(ll.y / 360.0 + 0.5, 0.5 - ll.x / 180.0);
    vec2 dux = wrap_x(dFdx(uv));
    vec2 duy = wrap_x(dFdy(uv));
    vec2 merc = merc_of(ll);
    vec2 dmx = wrap_x(dFdx(merc));
    vec2 dmy = wrap_x(dFdy(merc));
    vec2 dllx = vec2(dFdx(ll.x), dux.x * 360.0);
    vec2 dlly = vec2(dFdy(ll.x), duy.x * 360.0);
    vec3 col = ground(ll, dllx, dlly, uv, dux, duy, merc, dmx, dmy, n, to_eye, 1.0);

    // The limb, anti-aliased in pixels.
    float r_px = length(s - u_centre) * u_ppp;
    float limb_px = u_f / sqrt(max(u_d * u_d - 1.0, 1e-6)) * u_ppp;
    float cover = clamp(limb_px - r_px + 0.5, 0.0, 1.0);

    // The atmosphere: a fresnel rim inside the limb, a glow outside it (lighter on the day side).
    vec3 sky = mix(vec3(0.36, 0.62, 1.0), u_rim, 0.35 + 0.65 * u_holo);
    float mu = clamp(dot(pv, -ray), 0.0, 1.0);
    float side = mix(0.30 + 0.70 * smoothstep(-0.35, 0.35, dot(n, u_sun)), 1.0, max(u_holo, 1.0 - u_night));
    col += sky * pow(1.0 - mu, 3.0) * 0.55 * side;
    float h = u_d * length(q) / sqrt(dot(q, q) + 1.0);
    float above = max(h - 1.0, 0.0);
    vec3 closest = normalize(vec3(0.0, 0.0, u_d) + ray * (u_d * -ray.z));
    float halo_side = mix(0.25 + 0.75 * smoothstep(-0.4, 0.4, dot(u_basis * closest, u_sun)), 1.0, max(u_holo, 1.0 - u_night));
    vec3 halo = sky * (0.55 * exp(-above / 0.018) + 0.22 * exp(-above / 0.07)) * halo_side;
    return vec4(col * cover + halo * (1.0 - cover), cover);
}

vec4 flat_colour(vec2 s) {
    // The ground under the pixel (px from the middle). Leaned back (3D) the camera is u_f from
    // the middle, u_tilt.x from straight down and turned to u_tilt.y; the far ground dims into
    // the deep (as earth::FlatView::under and earth::fade).
    vec2 g = s - u_centre;
    float near_light = 1.0;
    if (u_tilt.x > 0.0 || u_tilt.y != 0.0) {
        float sp = sin(u_tilt.x);
        float cp = cos(u_tilt.x);
        vec2 d = g / u_f;
        float depth = cp / max(cp + d.y * sp, cp / 16.0);
        vec2 w = vec2(depth * d.x, sp + depth * (d.y * cp - sp)) * u_f;
        float sh = sin(u_tilt.y);
        float ch = cos(u_tilt.y);
        g = vec2(w.x * ch - w.y * sh, w.x * sh + w.y * ch);
        near_light = 1.0 - 0.85 * smoothstep(1.5, 4.0, depth);
    }
    float mx = u_flat.x + g.x / u_flat.z;
    float my = u_flat.y + g.y / u_flat.z;
    float lat = atan(sinh(PI * (1.0 - 2.0 * my)));
    vec2 ll = vec2(degrees(lat), fract(mx) * 360.0 - 180.0);
    vec2 uv = vec2(fract(mx), 0.5 - ll.x / 180.0);
    vec2 merc = vec2(fract(mx), my);
    vec2 dux = wrap_x(dFdx(uv));
    vec2 duy = wrap_x(dFdy(uv));
    vec2 dmx = wrap_x(dFdx(merc));
    vec2 dmy = wrap_x(dFdy(merc));
    vec2 dllx = vec2(dFdx(ll.x), dux.x * 360.0);
    vec2 dlly = vec2(dFdy(ll.x), duy.x * 360.0);
    float cl = cos(radians(ll.x));
    vec3 n = vec3(cl * cos(radians(ll.y)), cl * sin(radians(ll.y)), sin(radians(ll.x)));
    vec3 col = ground(ll, dllx, dlly, uv, dux, duy, merc, dmx, dmy, n, n, 0.0);
    float inside = step(0.0, my) * step(my, 1.0);
    return vec4(mix(vec3(0.008, 0.02, 0.035), col, inside) * near_light, 1.0);
}

void main() {
    vec2 s = vec2(gl_FragCoord.x, u_screen_h - gl_FragCoord.y) / u_ppp;
    vec4 globe = vec4(0.0);
    vec4 flat_map = vec4(0.0);
    if (u_flatness < 1.0) {
        globe = globe_colour(s);
    }
    if (u_flatness > 0.0) {
        flat_map = flat_colour(s);
    }
    o_colour = mix(globe, flat_map, u_flatness);
}
"#;

const UNIFORMS: [&str; 33] = [
    "u_centre", "u_f", "u_d", "u_basis", "u_ppp", "u_screen_h", "u_flatness", "u_flat", "u_sun", "u_holo", "u_night", "u_borders", "u_glint", "u_rim", "u_ocean_deep", "u_ocean_shallow", "u_land_dark", "u_land", "u_dot",
    "u_coast", "u_border", "u_detail", "u_overlay", "u_mask_size", "u_time", "t_day", "t_night", "t_mask", "t_detail", "t_detail_ok", "t_overlay", "t_overlay_ok", "u_tilt",
];

/// The shader program (GLSL 3.30, or 1.40 where 3.30 does not build), its vertex array and the
/// two 1 x 1 stand-in textures.
unsafe fn build(gl: &glow::Context) -> Result<Gpu, String> {
    // SAFETY (whole function): the caller is on the GL thread inside a paint callback.
    unsafe {
        let mut last = String::new();
        let mut program = None;
        for header in ["#version 330 core\n", "#version 140\n"] {
            match link(gl, header) {
                Ok(p) => {
                    program = Some(p);
                    break;
                }
                Err(e) => last = e,
            }
        }
        let program = program.ok_or(last)?;
        let vao = gl.create_vertex_array()?;
        let mut locations = HashMap::new();
        for name in UNIFORMS {
            locations.insert(name, gl.get_uniform_location(program, name));
        }
        gl.use_program(Some(program));
        for (name, unit) in [("t_day", 1), ("t_night", 2), ("t_mask", 3), ("t_detail", 4), ("t_detail_ok", 5), ("t_overlay", 6), ("t_overlay_ok", 7)] {
            gl.uniform_1_i32(locations.get(name).and_then(|l| l.as_ref()), unit);
        }
        gl.use_program(None);
        let pixel = |rgba: [u8; 4]| -> Result<glow::Texture, String> {
            let tex = gl.create_texture()?;
            gl.bind_texture(glow::TEXTURE_2D, Some(tex));
            gl.tex_image_2d(glow::TEXTURE_2D, 0, glow::RGBA8 as i32, 1, 1, 0, glow::RGBA, glow::UNSIGNED_BYTE, Some(&rgba));
            gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MIN_FILTER, glow::NEAREST as i32);
            gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MAG_FILTER, glow::NEAREST as i32);
            Ok(tex)
        };
        let black = pixel([4, 10, 22, 255])?;
        let clear = pixel([0, 0, 0, 0])?;
        let version = gl.version();
        let ext = gl.supported_extensions();
        let storage = (version.major, version.minor) >= (4, 2) || ext.contains("GL_ARB_texture_storage");
        let aniso = (ext.contains("GL_EXT_texture_filter_anisotropic") || ext.contains("GL_ARB_texture_filter_anisotropic")).then(|| gl.get_parameter_f32(0x84FF).clamp(1.0, 8.0));
        Ok(Gpu { program, vao, locations, slots: [None, None, None, None, None], incoming: [None, None, None, None, None], atlas: [None, None], black, clear, storage, aniso })
    }
}

unsafe fn link(gl: &glow::Context, header: &str) -> Result<glow::Program, String> {
    // SAFETY: on the GL thread inside a paint callback.
    unsafe {
        let program = gl.create_program()?;
        let mut shaders = Vec::new();
        for (kind, source) in [(glow::VERTEX_SHADER, VERTEX), (glow::FRAGMENT_SHADER, FRAGMENT)] {
            let shader = gl.create_shader(kind)?;
            gl.shader_source(shader, &format!("{header}{source}"));
            gl.compile_shader(shader);
            if !gl.get_shader_compile_status(shader) {
                let log = gl.get_shader_info_log(shader);
                gl.delete_shader(shader);
                for s in shaders {
                    gl.delete_shader(s);
                }
                gl.delete_program(program);
                return Err(format!("{}: {log}", header.trim()));
            }
            gl.attach_shader(program, shader);
            shaders.push(shader);
        }
        gl.link_program(program);
        let linked = gl.get_program_link_status(program);
        let log = if linked { String::new() } else { gl.get_program_info_log(program) };
        for s in shaders {
            gl.detach_shader(program, s);
            gl.delete_shader(s);
        }
        if !linked {
            gl.delete_program(program);
            return Err(format!("{}: {log}", header.trim()));
        }
        Ok(program)
    }
}

fn gl_format(format: Format) -> (u32, u32) {
    match format {
        Format::Dxt1 => (glow::COMPRESSED_RGB_S3TC_DXT1_EXT, glow::RGB),
        Format::Rgba8 => (glow::RGBA8, glow::RGBA),
        Format::Rg8 => (glow::RG8, glow::RG),
        Format::R8 => (glow::R8, glow::RED),
    }
}

/// The waiting base textures, coarsest level first, a few MB a frame; a finished one takes its
/// slot's place.
unsafe fn upload_bases(gl: &glow::Context, s: &mut Shared) {
    let Some(gpu) = s.gpu.as_mut() else { return };
    let mut budget = UPLOAD_BYTES_PER_FRAME;
    for slot in SLOTS {
        let i = slot.index();
        if budget == 0 {
            break;
        }
        if let Some(image) = s.queued[i].take() {
            // A newer image replaces one still going up.
            if let Some(old) = gpu.incoming[i].take() {
                // SAFETY: our own texture.
                unsafe { gl.delete_texture(old.tex) };
            }
            let dxt = image.format == Format::Dxt1;
            if dxt && !gpu_has_dxt(gl) {
                continue;
            }
            // SAFETY: GL thread.
            let Ok(tex) = (unsafe { gl.create_texture() }) else { continue };
            let (internal, _) = gl_format(image.format);
            let (w, h) = (image.levels[0].w, image.levels[0].h);
            let wrap_t = glow::CLAMP_TO_EDGE as i32;
            // SAFETY: GL thread, our texture.
            unsafe {
                gl.bind_texture(glow::TEXTURE_2D, Some(tex));
                gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_WRAP_S, glow::REPEAT as i32);
                gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_WRAP_T, wrap_t);
                gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MIN_FILTER, glow::LINEAR_MIPMAP_LINEAR as i32);
                gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MAG_FILTER, glow::LINEAR as i32);
                gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MAX_LEVEL, image.levels.len() as i32 - 1);
                if let Some(a) = gpu.aniso.filter(|_| slot == Slot::Day || slot == Slot::Historic || slot == Slot::Relief) {
                    gl.tex_parameter_f32(glow::TEXTURE_2D, glow::TEXTURE_MAX_ANISOTROPY_EXT, a);
                }
                if gpu.storage {
                    gl.tex_storage_2d(glow::TEXTURE_2D, image.levels.len() as i32, internal, w as i32, h as i32);
                }
            }
            let level = image.levels.len() - 1;
            gpu.incoming[i] = Some(Incoming { tex, image, level, row: 0 });
        }
        let Some(inc) = gpu.incoming[i].as_mut() else { continue };
        let format = inc.image.format;
        let (internal, pixel_format) = gl_format(format);
        // SAFETY: GL thread, our texture.
        unsafe { gl.bind_texture(glow::TEXTURE_2D, Some(inc.tex)) };
        let mut done = false;
        while budget > 0 {
            let lvl = &inc.image.levels[inc.level];
            let row_bytes = format.level_bytes(lvl.w, 1.min(lvl.h)).max(1);
            if !gpu.storage {
                // The whole level at once (no immutable storage to fill in strips).
                // SAFETY: GL thread, our texture; the data is exactly the level's size.
                unsafe {
                    if format == Format::Dxt1 {
                        gl.compressed_tex_image_2d(glow::TEXTURE_2D, inc.level as i32, internal as i32, lvl.w as i32, lvl.h as i32, 0, lvl.data.len() as i32, &lvl.data);
                    } else {
                        gl.tex_image_2d(glow::TEXTURE_2D, inc.level as i32, internal as i32, lvl.w as i32, lvl.h as i32, 0, pixel_format, glow::UNSIGNED_BYTE, Some(&lvl.data));
                    }
                }
                budget = budget.saturating_sub(lvl.data.len());
            } else if format == Format::Dxt1 {
                let block_row = format.level_bytes(lvl.w, 4);
                let block_rows = lvl.h.div_ceil(4).max(1);
                let first = inc.row / 4;
                let take = ((budget / block_row.max(1)) as u32).clamp(1, block_rows - first);
                let (y, rows) = (first * 4, (take * 4).min(lvl.h - first * 4).max(1.min(lvl.h)));
                let data = &lvl.data[first as usize * block_row..(first + take) as usize * block_row];
                // SAFETY: GL thread; the strip is whole 4x4 blocks inside the level.
                unsafe { gl.compressed_tex_sub_image_2d(glow::TEXTURE_2D, inc.level as i32, 0, y as i32, lvl.w as i32, rows as i32, internal, glow::CompressedPixelUnpackData::Slice(data)) };
                inc.row = (first + take) * 4;
                budget = budget.saturating_sub(data.len());
                if inc.row < lvl.h {
                    continue;
                }
            } else {
                let rows = ((budget / row_bytes) as u32).clamp(1, lvl.h - inc.row);
                let data = &lvl.data[inc.row as usize * row_bytes..(inc.row + rows) as usize * row_bytes];
                // SAFETY: GL thread; the strip lies inside the level.
                unsafe { gl.tex_sub_image_2d(glow::TEXTURE_2D, inc.level as i32, 0, inc.row as i32, lvl.w as i32, rows as i32, pixel_format, glow::UNSIGNED_BYTE, glow::PixelUnpackData::Slice(data)) };
                inc.row += rows;
                budget = budget.saturating_sub(data.len());
                if inc.row < lvl.h {
                    continue;
                }
            }
            // The level is up.
            inc.row = 0;
            if inc.level == 0 {
                done = true;
                break;
            }
            inc.level -= 1;
        }
        if done {
            let Some(inc) = gpu.incoming[i].take() else { continue };
            if let Some(old) = gpu.slots[i].take() {
                // SAFETY: our own texture.
                unsafe { gl.delete_texture(old.tex) };
            }
            let l0 = &inc.image.levels[0];
            gpu.slots[i] = Some(Tex { tex: inc.tex, w: l0.w, h: l0.h, bytes: inc.image.bytes() });
            s.ready[i] = true;
            info!("[EARTH] {slot:?} texture on the GPU: {}x{} {:?}, {} MB (the globe holds {} MB)", l0.w, l0.h, inc.image.format, inc.image.bytes() / (1024 * 1024), held_bytes(gpu) / (1024 * 1024));
        }
    }
}

/// The clipmaps' waiting tiles (a few a frame) and their slot maps.
unsafe fn upload_atlases(gl: &glow::Context, s: &mut Shared) {
    let Some(gpu) = s.gpu.as_mut() else { return };
    for k in 0..2 {
        if s.tiles[k].is_empty() && s.valid[k].is_none() {
            continue;
        }
        if gpu.atlas[k].is_none() {
            // SAFETY: GL thread; new textures of our own.
            let made = unsafe {
                let side = (CLIP_N * TILE_PX) as i32;
                let tex = gl.create_texture();
                let valid = gl.create_texture();
                match (tex, valid) {
                    (Ok(tex), Ok(valid)) => {
                        gl.bind_texture(glow::TEXTURE_2D, Some(tex));
                        gl.tex_image_2d(glow::TEXTURE_2D, 0, glow::RGBA8 as i32, side, side, 0, glow::RGBA, glow::UNSIGNED_BYTE, None);
                        gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_WRAP_S, glow::REPEAT as i32);
                        gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_WRAP_T, glow::REPEAT as i32);
                        gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MIN_FILTER, glow::LINEAR_MIPMAP_LINEAR as i32);
                        gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MAG_FILTER, glow::LINEAR as i32);
                        gl.generate_mipmap(glow::TEXTURE_2D);
                        gl.bind_texture(glow::TEXTURE_2D, Some(valid));
                        let zero = vec![0u8; (CLIP_N * CLIP_N) as usize];
                        gl.tex_image_2d(glow::TEXTURE_2D, 0, glow::R8 as i32, CLIP_N as i32, CLIP_N as i32, 0, glow::RED, glow::UNSIGNED_BYTE, Some(&zero));
                        gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_WRAP_S, glow::REPEAT as i32);
                        gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_WRAP_T, glow::REPEAT as i32);
                        gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MIN_FILTER, glow::NEAREST as i32);
                        gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MAG_FILTER, glow::NEAREST as i32);
                        Some(AtlasTex { tex, valid })
                    }
                    _ => None,
                }
            };
            let Some(made) = made else { continue };
            gpu.atlas[k] = Some(made);
        }
        let Some(atlas) = gpu.atlas[k].as_ref() else { continue };
        let mut uploaded = 0;
        // SAFETY: GL thread; the tiles are 256 x 256 RGBA inside the atlas.
        unsafe {
            gl.bind_texture(glow::TEXTURE_2D, Some(atlas.tex));
            while uploaded < ATLAS_TILES_PER_FRAME {
                let Some((slot, rgba)) = s.tiles[k].pop_front() else { break };
                let (sx, sy) = (slot as u32 % CLIP_N, slot as u32 / CLIP_N);
                gl.tex_sub_image_2d(glow::TEXTURE_2D, 0, (sx * TILE_PX) as i32, (sy * TILE_PX) as i32, TILE_PX as i32, TILE_PX as i32, glow::RGBA, glow::UNSIGNED_BYTE, glow::PixelUnpackData::Slice(&rgba));
                uploaded += 1;
            }
            if uploaded > 0 {
                gl.generate_mipmap(glow::TEXTURE_2D);
            }
            if let Some(valid) = s.valid[k].take() {
                gl.bind_texture(glow::TEXTURE_2D, Some(atlas.valid));
                gl.tex_sub_image_2d(glow::TEXTURE_2D, 0, 0, 0, CLIP_N as i32, CLIP_N as i32, glow::RED, glow::UNSIGNED_BYTE, glow::PixelUnpackData::Slice(&valid));
            }
        }
    }
}

unsafe fn paint(gl: &glow::Context, gpu: &mut Gpu, info: &egui::PaintCallbackInfo, frame: &Frame) {
    let loc = |name: &str| gpu.locations.get(name).and_then(|l| l.as_ref());
    // SAFETY: GL thread; the program and textures are ours. egui restores its own state after
    // the callback (program, vertex array, blending, active texture unit 0).
    unsafe {
        gl.use_program(Some(gpu.program));
        gl.bind_vertex_array(Some(gpu.vao));
        gl.uniform_2_f32(loc("u_centre"), frame.centre[0], frame.centre[1]);
        gl.uniform_1_f32(loc("u_f"), frame.f);
        gl.uniform_1_f32(loc("u_d"), frame.d);
        let b = frame.basis;
        gl.uniform_matrix_3_f32_slice(loc("u_basis"), false, &[b[0][0], b[0][1], b[0][2], b[1][0], b[1][1], b[1][2], b[2][0], b[2][1], b[2][2]]);
        gl.uniform_1_f32(loc("u_ppp"), info.pixels_per_point);
        gl.uniform_1_f32(loc("u_screen_h"), info.screen_size_px[1] as f32);
        gl.uniform_1_f32(loc("u_flatness"), frame.flatness);
        gl.uniform_3_f32(loc("u_flat"), frame.flat[0], frame.flat[1], frame.flat[2]);
        gl.uniform_2_f32(loc("u_tilt"), frame.tilt[0], frame.tilt[1]);
        gl.uniform_3_f32(loc("u_sun"), frame.sun[0], frame.sun[1], frame.sun[2]);
        gl.uniform_1_f32(loc("u_holo"), frame.holo);
        gl.uniform_1_f32(loc("u_night"), frame.night);
        gl.uniform_1_f32(loc("u_borders"), frame.borders);
        gl.uniform_1_f32(loc("u_glint"), frame.glint);
        gl.uniform_3_f32(loc("u_rim"), frame.rim[0], frame.rim[1], frame.rim[2]);
        for (name, c) in ["u_ocean_deep", "u_ocean_shallow", "u_land_dark", "u_land", "u_dot", "u_coast", "u_border"].into_iter().zip(frame.palette) {
            gl.uniform_3_f32(loc(name), c[0], c[1], c[2]);
        }
        let d = frame.detail.uniform();
        let o = frame.overlay.uniform();
        gl.uniform_4_f32(loc("u_detail"), d[0], d[1], d[2], if gpu.atlas[0].is_some() { d[3] } else { 0.0 });
        gl.uniform_4_f32(loc("u_overlay"), o[0], o[1], o[2], if gpu.atlas[1].is_some() { o[3] } else { 0.0 });
        let mask = gpu.slots[Slot::Mask.index()].as_ref();
        gl.uniform_2_f32(loc("u_mask_size"), mask.map_or(1.0, |m| m.w as f32), mask.map_or(1.0, |m| m.h as f32));
        gl.uniform_1_f32(loc("u_time"), frame.time);
        let day = gpu.slots[frame.day.index()].as_ref().or(gpu.slots[Slot::Day.index()].as_ref()).map_or(gpu.black, |t| t.tex);
        let night = gpu.slots[Slot::Night.index()].as_ref().map_or(gpu.clear, |t| t.tex);
        // No mask yet: all land would be wrong, all sea is quiet (a mid grey is neither).
        let mask = mask.map_or(gpu.clear, |t| t.tex);
        let (detail, detail_ok) = gpu.atlas[0].as_ref().map_or((gpu.clear, gpu.clear), |a| (a.tex, a.valid));
        let (overlay, overlay_ok) = gpu.atlas[1].as_ref().map_or((gpu.clear, gpu.clear), |a| (a.tex, a.valid));
        for (unit, tex) in [(1u32, day), (2, night), (3, mask), (4, detail), (5, detail_ok), (6, overlay), (7, overlay_ok)] {
            gl.active_texture(glow::TEXTURE0 + unit);
            gl.bind_texture(glow::TEXTURE_2D, Some(tex));
        }
        gl.draw_arrays(glow::TRIANGLES, 0, 3);
        for unit in 1..=7u32 {
            gl.active_texture(glow::TEXTURE0 + unit);
            gl.bind_texture(glow::TEXTURE_2D, None);
        }
        gl.active_texture(glow::TEXTURE0);
        gl.bind_vertex_array(None);
        gl.use_program(None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::earth::{GlobeView, HOME_ALT_KM};

    #[test]
    fn the_view_basis_turns_view_coordinates_into_the_world() {
        for (lat0, lon0) in [(0.0, 0.0), (30.0, -20.0), (-45.0, 170.0), (80.0, 10.0)] {
            let g = GlobeView::new(lat0, lon0, HOME_ALT_KM, 700.0, (0.0, 0.0));
            let b = view_basis(lat0, lon0);
            for (lat, lon) in [(lat0, lon0), (lat0 + 10.0, lon0 - 15.0), (0.0, 0.0), (-20.0, 60.0)] {
                let v = g.view_coords(lat, lon);
                let w = [0, 1, 2].map(|i| b[0][i] as f64 * v[0] + b[1][i] as f64 * v[1] + b[2][i] as f64 * v[2]);
                let expect = world_unit(lat, lon);
                for i in 0..3 {
                    assert!((w[i] - expect[i] as f64).abs() < 1e-5, "{lat0},{lon0} -> {lat},{lon}: {w:?} vs {expect:?}");
                }
            }
        }
    }

    #[test]
    fn dxt1_keeps_colours_close() {
        let worst = |a: &[u8], b: &[u8]| a.iter().zip(b).enumerate().filter(|(i, _)| i % 4 != 3).map(|(_, (a, b))| (*a as i32 - *b as i32).abs()).max().unwrap();
        // A smooth gradient of shades survives within a few levels.
        let (w, h) = (16usize, 8usize);
        let mut rgba = Vec::new();
        for y in 0..h {
            for x in 0..w {
                let v = (x * 12 + y * 8) as f32;
                rgba.extend_from_slice(&[v as u8, (v * 0.8) as u8, (60.0 + v * 0.5) as u8, 255]);
            }
        }
        let blocks = dxt1_encode(&rgba, w, h);
        assert_eq!(blocks.len(), Format::Dxt1.level_bytes(w as u32, h as u32));
        let err = worst(&rgba, &dxt1_decode(&blocks, w, h));
        assert!(err <= 16, "worst channel error {err}");
        // A coast: blue sea and brown land in one block (their channels run against each other)
        // both keep their colour.
        let coast: Vec<u8> = (0..16).flat_map(|i| if i % 4 < 2 { [10u8, 30, 90, 255] } else { [120, 100, 60, 255] }).collect();
        let err = worst(&coast, &dxt1_decode(&dxt1_encode(&coast, 4, 4), 4, 4));
        assert!(err <= 14, "coast error {err}");
        let flat = [200u8, 100, 50, 255].repeat(16);
        let back = dxt1_decode(&dxt1_encode(&flat, 4, 4), 4, 4);
        assert!(back.chunks(4).all(|p| (p[0] as i32 - 200).abs() <= 4 && (p[1] as i32 - 100).abs() <= 2 && (p[2] as i32 - 50).abs() <= 4));
        // Parallel and serial encodings agree; odd sizes are padded.
        let big: Vec<u8> = (0..(600 * 520 * 4)).map(|i| (i % 251) as u8).collect();
        assert_eq!(dxt1_encode_parallel(&big, 600, 520, 4), dxt1_encode(&big, 600, 520));
        assert_eq!(dxt1_encode(&[9u8; 4 * 6], 3, 2).len(), 8);
    }

    #[test]
    fn mip_chains_and_blobs() {
        let img = GpuImage::with_mips(Format::Rg8, 8, 4, vec![200u8; 8 * 4 * 2]);
        assert_eq!(img.levels.iter().map(|l| (l.w, l.h)).collect::<Vec<_>>(), vec![(8, 4), (4, 2), (2, 1), (1, 1)]);
        assert!(img.levels.iter().all(|l| l.data.len() == Format::Rg8.level_bytes(l.w, l.h) && l.data.iter().all(|v| *v == 200)));
        let dxt = GpuImage::dxt1_with_mips(16, 8, [10u8, 20, 30, 255].repeat(128), 2);
        assert_eq!(dxt.levels.len(), 5);
        assert!(dxt.levels.iter().all(|l| l.data.len() == Format::Dxt1.level_bytes(l.w, l.h)));
        assert_eq!(GpuImage::from_blob(&dxt.to_blob()), Some(dxt.clone()));
        assert_eq!(GpuImage::from_blob(b"nonsense"), None);
        let mut broken = dxt.to_blob();
        broken.truncate(broken.len() - 3);
        assert_eq!(GpuImage::from_blob(&broken), None);
        let (half, w, h) = downsample(&[0, 100, 200, 50, 10, 10, 10, 10, 90, 90, 90, 90], 3, 2, 2);
        assert_eq!((w, h, half.len()), (1, 1, 2));
    }

    #[test]
    fn clipmap_windows_wrap_around_the_world() {
        let c = Clip::around(40.7, -74.0, 7);
        let (tx, ty) = tile_of(40.7, -74.0, 7);
        assert!(c.contains(tx, ty));
        assert_eq!(c.tiles()[0..4].iter().filter(|t| **t == (tx, ty)).count(), 1, "the tile in view comes among the first");
        assert_eq!(c.tiles().len(), 64);
        // Across the date line the window runs on from the east edge to the west.
        let pacific = Clip::around(0.0, 179.9, 6);
        assert!(pacific.contains(63, 32) && pacific.contains(0, 32), "{pacific:?}");
        let slots: std::collections::HashSet<usize> = pacific.tiles().into_iter().map(|(x, y)| Clip::slot(x, y)).collect();
        assert_eq!(slots.len(), 64, "every tile of a window has a slot of its own");
        // Near the pole the window stays inside the world.
        let north = Clip::around(84.0, 0.0, 5);
        assert_eq!(north.y0, 0);
        // At level 3 the window is the whole world.
        let world = Clip::around(10.0, 10.0, 3);
        assert_eq!((world.x0 + 8) % 8, world.x0 % 8);
        assert!((0..8).all(|x| (0..8).all(|y| world.contains(x, y))));
        assert_eq!(ClipUse { clip: Some(world), mix: 1.0 }.uniform(), [8.0, world.x0 as f32, 0.0, 1.0]);
        assert_eq!(ClipUse::default().uniform(), [0.0; 4]);
    }

    /// Clipmap tiles still waiting to go up.
    fn tiles_waiting(atlas: Atlas) -> usize {
        shared().tiles[atlas as usize].len()
    }

    #[test]
    fn nothing_is_waiting_after_a_release() {
        submit_tile(Atlas::Overlay, 3, vec![0u8; (TILE_PX * TILE_PX * 4) as usize]);
        submit_tile(Atlas::Overlay, 3, vec![1u8; (TILE_PX * TILE_PX * 4) as usize]);
        assert_eq!(tiles_waiting(Atlas::Overlay), 1, "a slot's newer tile replaces the waiting one");
        submit_tile(Atlas::Overlay, 99, vec![0u8; 4]);
        assert_eq!(tiles_waiting(Atlas::Overlay), 1, "wrong sizes are refused");
        request_release();
        assert_eq!(tiles_waiting(Atlas::Overlay), 0);
        assert!(!proven() && !ready(Slot::Day));
    }
}
