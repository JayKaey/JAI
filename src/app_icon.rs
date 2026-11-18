//! The application icon: J.A.I.'s simple core as light on a black disc. It reads at 256 × 256 px
//! and stays unmistakable at 16-24 px on the Windows taskbar.
//!
//! From the outside in: a hairline rim of ink with a 60-tick scale (the core's clock ring), two
//! orbit arcs in the deep tone, the band of four glass segments with a white-hot filament on
//! their inner edge, the fine voice ring and a white-hot nucleus in its glow. The fine parts
//! (ticks, rim, voice ring) only appear from about 64 px up; at taskbar size the band is four
//! solid arcs of his yellow round a bright dot.
//!
//! Pure `std` on purpose: `build.rs` includes this file (`#[path]`) to write `assets/jai_icon.ico`
//! (taskbar, Alt-Tab, Explorer and the tray load it from the executable's resources), and the HUD
//! window takes the same pixels at runtime (`window_icon`). Every pixel is anti-aliased
//! analytically (coverage from signed distances) and 8 × 8 supersampled.

// The `.ico` encoders serve `build.rs` and the tests; the application itself only renders pixels.
#![cfg_attr(not(test), allow(dead_code))]

use std::f32::consts::{PI, TAU};

/// The largest frame (also the window icon).
pub const ICON_SIZE: u32 = 256;
/// The frames of the `.ico` (Windows picks the one closest to what it draws).
pub const ICO_SIZES: [u32; 7] = [16, 24, 32, 48, 64, 128, 256];
/// Samples per pixel along each axis (8 × 8 = 64).
pub const SUPERSAMPLE: usize = 8;

/// The disc's radius in unit space (the canvas spans −1 … 1).
pub const DISC: f32 = 0.97;
/// A ring: (centre-line radius, half stroke width, arc count, arc span in radians, phase of the
/// first arc's centre in radians, y down).
pub struct Ring {
    pub radius: f32,
    pub half: f32,
    pub arcs: usize,
    pub span: f32,
    pub phase: f32,
}
const fn deg(d: f32) -> f32 {
    d * PI / 180.0
}
/// The band: four segments of 62°, centred on the diagonals (three yellow arcs on black would
/// read as a radiation sign at taskbar size).
pub const BAND: Ring = Ring { radius: 0.56, half: 0.075, arcs: 4, span: deg(62.0), phase: deg(-45.0) };
/// The orbit: two arcs in the deep tone, opposite each other.
pub const ORBIT: Ring = Ring { radius: 0.80, half: 0.022, arcs: 2, span: deg(104.0), phase: deg(20.0) };
/// The fine voice ring round the nucleus.
pub const VOICE: Ring = Ring { radius: 0.34, half: 0.012, arcs: 1, span: TAU, phase: 0.0 };
pub const NUCLEUS: f32 = 0.13;

const VOID: [f32; 3] = rgb(0x020304);
const YELLOW: [f32; 3] = rgb(0xFFD400);
const DEEP: [f32; 3] = rgb(0xFF8A00);
const HOT: [f32; 3] = rgb(0xFFF4BF);
const INK: [f32; 3] = rgb(0xEEF3F7);
const WHITE: [f32; 3] = [1.0; 3];

const fn rgb(hex: u32) -> [f32; 3] {
    [((hex >> 16) & 0xFF) as f32 / 255.0, ((hex >> 8) & 0xFF) as f32 / 255.0, (hex & 0xFF) as f32 / 255.0]
}

/// Coverage of a stroke of half-width `half` at distance `d` (`px` = one pixel in unit space).
fn cover(d: f32, half: f32, px: f32) -> f32 {
    ((half - d) / px + 0.5).clamp(0.0, 1.0)
}

fn over(dst: &mut [f32; 3], colour: [f32; 3], alpha: f32) {
    for i in 0..3 {
        dst[i] += (colour[i] - dst[i]) * alpha.clamp(0.0, 1.0);
    }
}

fn add(dst: &mut [f32; 3], colour: [f32; 3], amount: f32) {
    for i in 0..3 {
        dst[i] += colour[i] * amount.max(0.0);
    }
}

/// How far the angle `theta` lies inside the nearest of `count` arcs of angular width `span`
/// spaced evenly from `phase`, as a length along the circle of radius `r` (negative in a gap).
pub fn arc_inside(theta: f32, r: f32, phase: f32, count: usize, span: f32) -> f32 {
    (0..count)
        .map(|k| {
            let d = (theta - phase - k as f32 * TAU / count as f32 + PI).rem_euclid(TAU) - PI;
            (span / 2.0 - d.abs()) * r
        })
        .fold(f32::MIN, f32::max)
}

/// How much of a ring's stroke covers the point (r, theta): its arcs cut flat and radial.
fn on_ring(ring: &Ring, r: f32, theta: f32, px: f32) -> f32 {
    let inside = if ring.span >= TAU { 1.0 } else { (arc_inside(theta, r, ring.phase, ring.arcs, ring.span) / px + 0.5).clamp(0.0, 1.0) };
    cover((r - ring.radius).abs(), ring.half.max(px * 0.6), px) * inside
}

/// A glow round a ring's centre line: light that falls off through the deep tone (never a dim
/// yellow, which reads as olive).
fn glow(c: &mut [f32; 3], ring: &Ring, r: f32, theta: f32, reach: f32, strength: f32) {
    let ends = if ring.span >= TAU { 1.0 } else { (-(arc_inside(theta, r, ring.phase, ring.arcs, ring.span).min(0.0) / 0.06).powi(2)).exp() };
    let d = (r - ring.radius).abs() / reach;
    add(c, YELLOW, (-(d * 2.4).powi(2)).exp() * 0.30 * strength * ends);
    add(c, DEEP, (-d.powi(2)).exp() * 0.16 * strength * ends);
}

/// Colour and alpha of one sample at unit coordinates (x, y), y down; `px` = one pixel.
fn sample(x: f32, y: f32, px: f32) -> ([f32; 3], f32) {
    let (r, theta) = (x.hypot(y), y.atan2(x));
    let alpha = cover(r - DISC, 0.0, px);
    if alpha <= 0.0 {
        return ([0.0; 3], 0.0);
    }
    // Taskbar frames (32 px and less) are drawn in bold shapes: no fine lines, little glow.
    let fine = px < 0.03;
    let strength = if fine { 1.0 } else { 0.4 };
    let mut c = VOID;
    if fine {
        // The rim and its scale: 60 ticks, every fifth one longer and brighter.
        over(&mut c, INK, cover((r - 0.93).abs(), 0.006, px) * 0.35);
        let step = TAU / 60.0;
        let nearest = (theta / step).round();
        let across = (theta - nearest * step).abs() * r;
        let long = (nearest as i32).rem_euclid(5) == 0;
        let (inner, outer) = if long { (0.865, 0.925) } else { (0.895, 0.925) };
        let reach = cover((r - (inner + outer) / 2.0).abs(), (outer - inner) / 2.0, px);
        over(&mut c, INK, cover(across, if long { 0.006 } else { 0.004 }, px) * reach * if long { 0.85 } else { 0.4 });
        // The orbit arcs in the deep tone.
        glow(&mut c, &ORBIT, r, theta, 0.07, 0.6);
        over(&mut c, DEEP, on_ring(&ORBIT, r, theta, px));
    }
    // The band: glass lit from its inner edge, and the filament there.
    glow(&mut c, &BAND, r, theta, 0.16, strength);
    if fine {
        let across = ((r - (BAND.radius - BAND.half)) / (2.0 * BAND.half)).clamp(0.0, 1.0);
        over(&mut c, YELLOW, on_ring(&BAND, r, theta, px) * (0.42 - 0.34 * across));
        let filament = Ring { radius: BAND.radius - BAND.half, half: 0.014, ..BAND };
        over(&mut c, HOT, on_ring(&filament, r, theta, px));
        over(&mut c, YELLOW, on_ring(&VOICE, r, theta, px) * 0.9);
    } else {
        over(&mut c, YELLOW, on_ring(&BAND, r, theta, px));
    }
    // The nucleus: white-hot in its glow.
    let off = (r - NUCLEUS).max(0.0);
    add(&mut c, YELLOW, (-(off / 0.09).powi(2)).exp() * 0.55 * strength);
    add(&mut c, DEEP, (-(off / 0.22).powi(2)).exp() * 0.20 * strength);
    over(&mut c, HOT, cover(r, NUCLEUS.max(px * 1.1), px));
    over(&mut c, WHITE, cover(r, (NUCLEUS * 0.62).max(px * 0.7), px));
    (c, alpha)
}

/// The icon as straight (non-premultiplied) RGBA, `size` × `size`, 8 × 8 supersampled.
pub fn render_icon_rgba(size: u32) -> Vec<u8> {
    let n = size.max(1) as usize;
    let px = 2.0 / n as f32;
    let k = SUPERSAMPLE;
    let mut out = vec![0u8; n * n * 4];
    for j in 0..n {
        for i in 0..n {
            let (mut c, mut a) = ([0.0f32; 3], 0.0f32);
            for sy in 0..k {
                for sx in 0..k {
                    let x = (i as f32 + (sx as f32 + 0.5) / k as f32) * px - 1.0;
                    let y = (j as f32 + (sy as f32 + 0.5) / k as f32) * px - 1.0;
                    let (sc, sa) = sample(x, y, px);
                    for q in 0..3 {
                        c[q] += sc[q] * sa;
                    }
                    a += sa;
                }
            }
            let o = (j * n + i) * 4;
            if a > 0.0 {
                for q in 0..3 {
                    out[o + q] = ((c[q] / a).clamp(0.0, 1.0) * 255.0).round() as u8;
                }
            }
            out[o + 3] = ((a / (k * k) as f32).clamp(0.0, 1.0) * 255.0).round() as u8;
        }
    }
    out
}

// ---------------------------------------------------------------------------- encoders ----

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &b in bytes {
        crc ^= b as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0xEDB8_8320 } else { crc >> 1 };
        }
    }
    !crc
}

fn adler32(bytes: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
    for &x in bytes {
        a = (a + x as u32) % 65_521;
        b = (b + a) % 65_521;
    }
    (b << 16) | a
}

fn png_chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    let start = out.len();
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    let crc = crc32(&out[start..]);
    out.extend_from_slice(&crc.to_be_bytes());
}

/// A PNG of straight RGBA pixels (zlib with stored deflate blocks: no compressor needed).
pub fn encode_png(width: u32, height: u32, rgba: &[u8]) -> Vec<u8> {
    let mut raw = Vec::with_capacity((width as usize * 4 + 1) * height as usize);
    for row in rgba.chunks_exact(width as usize * 4) {
        raw.push(0);
        raw.extend_from_slice(row);
    }
    let mut zlib = vec![0x78, 0x01];
    let blocks: Vec<&[u8]> = raw.chunks(65_535).collect();
    for (k, block) in blocks.iter().enumerate() {
        zlib.push(u8::from(k + 1 == blocks.len()));
        let len = block.len() as u16;
        zlib.extend_from_slice(&len.to_le_bytes());
        zlib.extend_from_slice(&(!len).to_le_bytes());
        zlib.extend_from_slice(block);
    }
    zlib.extend_from_slice(&adler32(&raw).to_be_bytes());
    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&width.to_be_bytes());
    ihdr.extend_from_slice(&height.to_be_bytes());
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]);
    let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
    png_chunk(&mut png, b"IHDR", &ihdr);
    png_chunk(&mut png, b"IDAT", &zlib);
    png_chunk(&mut png, b"IEND", &[]);
    png
}

/// A 32-bit DIB for an `.ico` frame: BITMAPINFOHEADER (double height), bottom-up BGRA rows and an
/// all-zero AND mask (the alpha channel decides).
fn encode_dib(size: u32, rgba: &[u8]) -> Vec<u8> {
    let n = size as usize;
    let mask_row = n.div_ceil(32) * 4;
    let mut out = Vec::with_capacity(40 + n * n * 4 + mask_row * n);
    for v in [40u32, size, size * 2] {
        out.extend_from_slice(&v.to_le_bytes());
    }
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&32u16.to_le_bytes());
    for v in [0u32, (n * n * 4 + mask_row * n) as u32, 0, 0, 0, 0] {
        out.extend_from_slice(&v.to_le_bytes());
    }
    for row in (0..n).rev() {
        for px in rgba[row * n * 4..(row + 1) * n * 4].chunks_exact(4) {
            out.extend_from_slice(&[px[2], px[1], px[0], px[3]]);
        }
    }
    out.resize(out.len() + mask_row * n, 0);
    out
}

/// An `.ico` with one frame per (size, RGBA): PNG for 256 px, DIB for the small sizes.
pub fn encode_ico(frames: &[(u32, Vec<u8>)]) -> Vec<u8> {
    let images: Vec<Vec<u8>> = frames.iter().map(|(s, px)| if *s >= 256 { encode_png(*s, *s, px) } else { encode_dib(*s, px) }).collect();
    let mut out = Vec::new();
    out.extend_from_slice(&[0, 0, 1, 0]);
    out.extend_from_slice(&(frames.len() as u16).to_le_bytes());
    let mut offset = 6 + 16 * frames.len();
    for ((size, _), image) in frames.iter().zip(&images) {
        let dim = if *size >= 256 { 0 } else { *size as u8 };
        out.extend_from_slice(&[dim, dim, 0, 0]);
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&32u16.to_le_bytes());
        out.extend_from_slice(&(image.len() as u32).to_le_bytes());
        out.extend_from_slice(&(offset as u32).to_le_bytes());
        offset += image.len();
    }
    for image in images {
        out.extend_from_slice(&image);
    }
    out
}

/// The full icon file (all `ICO_SIZES`).
pub fn icon_ico() -> Vec<u8> {
    let frames: Vec<(u32, Vec<u8>)> = ICO_SIZES.iter().map(|s| (*s, render_icon_rgba(*s))).collect();
    encode_ico(&frames)
}

/// The window / taskbar icon of the HUD (256 × 256 RGBA).
#[cfg(jai_app)]
pub fn window_icon() -> eframe::egui::IconData {
    eframe::egui::IconData { rgba: render_icon_rgba(ICON_SIZE), width: ICON_SIZE, height: ICON_SIZE }
}

#[cfg(all(test, jai_app))]
mod tests {
    use super::*;

    fn pixel(rgba: &[u8], n: usize, x: usize, y: usize) -> [u8; 4] {
        let o = (y * n + x) * 4;
        [rgba[o], rgba[o + 1], rgba[o + 2], rgba[o + 3]]
    }

    /// The 256 px pixel at `radius` (of the canvas's half) from the centre, `deg` degrees (y down).
    fn polar(rgba: &[u8], radius: f32, deg: f32) -> [u8; 4] {
        let (s, c) = deg.to_radians().sin_cos();
        pixel(rgba, 256, (128.0 + radius * 128.0 * c) as usize, (128.0 + radius * 128.0 * s) as usize)
    }

    /// His yellow, lit: red and green high, blue low.
    fn is_yellow(p: [u8; 4]) -> bool {
        p[0] > 200 && p[1] > 150 && p[2] < 150
    }

    fn is_deep(p: [u8; 4]) -> bool {
        p[0] > 200 && p[1] > 90 && p[1] < 190 && p[2] < 90
    }

    fn is_dark(p: [u8; 4]) -> bool {
        (p[0] as u32 + p[1] as u32 + p[2] as u32) < 130
    }

    #[test]
    fn the_icon_is_the_simple_core_on_a_black_disc() {
        let icon = window_icon();
        assert_eq!((icon.width, icon.height), (256, 256));
        let rgba = &icon.rgba;
        // A disc: clear in the corners, opaque black inside.
        assert_eq!(pixel(rgba, 256, 3, 3)[3], 0);
        let edge = polar(rgba, 0.955, 45.0);
        assert!(edge[3] == 255 && is_dark(edge), "{edge:?}");
        // The band's four segments, with gaps between them; the filament on their inner edge.
        for k in 0..4 {
            let arc = -45.0 + 90.0 * k as f32;
            let filament = polar(rgba, BAND.radius - BAND.half, arc);
            assert!(filament[0] > 230 && filament[1] > 200, "the filament is white-hot at {arc}°: {filament:?}");
            let glass = polar(rgba, BAND.radius, arc);
            assert!(glass[0] > 110 && glass[1] > 80 && glass[2] < 60, "the glass is lit warm at {arc}°: {glass:?}");
            let gap = polar(rgba, BAND.radius, arc + 45.0);
            assert!(!is_yellow(gap), "gap at {}°: {gap:?}", arc + 45.0);
        }
        // The glass of a segment is lit from its inner edge: brighter there than at its outer.
        let lum = |p: [u8; 4]| p[0] as u32 + p[1] as u32;
        assert!(lum(polar(rgba, BAND.radius - BAND.half * 0.5, -45.0)) > lum(polar(rgba, BAND.radius + BAND.half * 0.8, -45.0)) + 30);
        // The orbit in the deep tone, the nucleus white-hot, dark air between.
        assert!(is_deep(polar(rgba, ORBIT.radius, 20.0)), "{:?}", polar(rgba, ORBIT.radius, 20.0));
        assert!(!is_deep(polar(rgba, ORBIT.radius, 110.0)));
        let c = pixel(rgba, 256, 128, 128);
        assert!(c[0] > 245 && c[1] > 245 && c[2] > 235, "{c:?}");
        assert!(is_dark(polar(rgba, 0.45, 150.0)), "{:?}", polar(rgba, 0.45, 150.0));
        // The fine parts exist at full size: a long tick of the scale at the top.
        let tick = polar(rgba, 0.895, -90.0);
        assert!(tick[0] > 120 && tick[2] > 120, "tick {tick:?}");
    }

    #[test]
    fn arcs_repeat_by_their_count() {
        assert!(arc_inside(BAND.phase, 1.0, BAND.phase, BAND.arcs, BAND.span) > 0.0);
        assert!(arc_inside(BAND.phase + PI / 4.0, 1.0, BAND.phase, BAND.arcs, BAND.span) < 0.0);
        assert!(arc_inside(BAND.phase + TAU / 4.0, 1.0, BAND.phase, BAND.arcs, BAND.span) > 0.0);
    }

    #[test]
    fn snapshot_icon_frames() {
        // The 256 px icon and the 16 / 24 / 32 px taskbar frames (nearest-neighbour, enlarged) as PNGs.
        let dir = std::path::Path::new("target/ui-snapshots");
        let _ = std::fs::create_dir_all(dir);
        let _ = std::fs::write(dir.join("app_icon_256.png"), encode_png(256, 256, &render_icon_rgba(256)));
        for (size, zoom) in [(16u32, 8u32), (24, 6), (32, 4)] {
            let small = render_icon_rgba(size);
            let big = size * zoom;
            let mut up = vec![0u8; (big * big * 4) as usize];
            for y in 0..big {
                for x in 0..big {
                    let s = (((y / zoom) * size + x / zoom) * 4) as usize;
                    let d = ((y * big + x) * 4) as usize;
                    up[d..d + 4].copy_from_slice(&small[s..s + 4]);
                }
            }
            let _ = std::fs::write(dir.join(format!("app_icon_{size}_x{zoom}.png")), encode_png(big, big, &up));
        }
    }

    #[test]
    fn the_16_and_24_px_frames_still_read() {
        for size in [16usize, 24] {
            let rgba = render_icon_rgba(size as u32);
            let px = |x: usize, y: usize| pixel(&rgba, size, x, y);
            assert!(px(0, 0)[3] < 16, "clear corner at {size} px: {:?}", px(0, 0));
            let all: Vec<[u8; 4]> = (0..size).flat_map(|y| (0..size).map(move |x| (x, y))).map(|(x, y)| px(x, y)).collect();
            // His yellow survives at taskbar size, the nucleus is bright, the dark dominates.
            assert!(all.iter().filter(|p| p[0] > 170 && p[1] > 130 && p[2] < 120).count() >= 6, "yellow at {size} px");
            let c = px(size / 2, size / 2).iter().take(3).map(|v| *v as u32).sum::<u32>().max(px(size / 2 - 1, size / 2 - 1).iter().take(3).map(|v| *v as u32).sum());
            assert!(c > 450, "a bright nucleus at {size} px: {c}");
            let dark = all.iter().filter(|p| p[3] == 255 && is_dark(**p)).count();
            assert!(dark * 5 > size * size, "dark pixels at {size} px: {dark}");
        }
    }

    #[test]
    fn ico_holds_every_size_with_a_valid_png_frame() {
        let ico = icon_ico();
        assert_eq!(&ico[0..4], &[0, 0, 1, 0]);
        let count = u16::from_le_bytes([ico[4], ico[5]]) as usize;
        assert_eq!(count, ICO_SIZES.len());
        for k in 0..count {
            let e = &ico[6 + 16 * k..22 + 16 * k];
            let size = u32::from_le_bytes(e[8..12].try_into().unwrap()) as usize;
            let offset = u32::from_le_bytes(e[12..16].try_into().unwrap()) as usize;
            assert!(offset + size <= ico.len());
            let data = &ico[offset..offset + size];
            if e[0] == 0 {
                assert!(data.starts_with(b"\x89PNG\r\n\x1a\n"));
                // The PNG decodes back to the same pixels.
                let decoded = image::load_from_memory(data).expect("valid PNG").to_rgba8();
                assert_eq!(decoded.dimensions(), (256, 256));
                assert_eq!(decoded.as_raw(), &render_icon_rgba(256));
            } else {
                assert_eq!(u32::from_le_bytes(data[0..4].try_into().unwrap()), 40, "BITMAPINFOHEADER");
                assert_eq!(u32::from_le_bytes(data[4..8].try_into().unwrap()), e[0] as u32);
            }
        }
    }
}
