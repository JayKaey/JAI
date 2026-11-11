//! The application icon: JAI's HUD emblem on a deep-space glass squircle. It reads at 256 × 256 px
//! and stays unmistakable at 16-24 px on the Windows taskbar.
//!
//! From the outside in: a hairline rim with a 72-tick scale (long ticks every 30°), a bold cyan ring
//! cut into four arcs, a slim ice-white ring of three long arcs, an amber arc-reactor ring of three
//! short arcs and a white-hot core in a cyan / amber halo, all lit over a `#0A1E33` → `#02060C`
//! radial gradient with a cold rim light. The fine parts (ticks, hairline, core ring) only appear
//! from about 64 px up; the bold rings carry the taskbar frames on their own.
//!
//! Pure `std` on purpose: `build.rs` includes this file (`#[path]`) to write `assets/jarvis_icon.ico`
//! (taskbar, Alt-Tab, Explorer and the tray load it from the executable's resources), and the HUD
//! window takes the same pixels at runtime (`generate_rose_gyro_app_icon`). Every pixel is
//! anti-aliased analytically (coverage from signed distances) and 8 × 8 supersampled.

// The `.ico` encoders serve `build.rs` and the tests; the application itself only renders pixels.
#![cfg_attr(not(test), allow(dead_code))]

use std::f32::consts::{PI, TAU};

/// The largest frame (also the window icon).
pub const ICON_SIZE: u32 = 256;
/// The frames of the `.ico` (Windows picks the one closest to what it draws).
pub const ICO_SIZES: [u32; 7] = [16, 24, 32, 48, 64, 128, 256];
/// Samples per pixel along each axis (8 × 8 = 64).
pub const SUPERSAMPLE: usize = 8;

/// One pixel of the 256 px canvas in unit space (the canvas spans −1 … 1).
const P: f32 = 1.0 / 128.0;
const CORNER: f32 = 56.0 * P;
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
/// The bold cyan ring: four 62° arcs, centred on the diagonals.
pub const OUTER: Ring = Ring { radius: 0.68, half: 0.055, arcs: 4, span: deg(62.0), phase: deg(-45.0) };
/// The slim ice ring: three 100° arcs, the first at the bottom.
pub const MIDDLE: Ring = Ring { radius: 0.50, half: 0.030, arcs: 3, span: deg(100.0), phase: deg(90.0) };
/// The amber arc-reactor ring: three 50° arcs, the first at the top.
pub const INNER: Ring = Ring { radius: 0.36, half: 0.050, arcs: 3, span: deg(50.0), phase: deg(-90.0) };
pub const CORE_RADIUS: f32 = 0.15;

const SPACE_EDGE: [f32; 3] = rgb(0x02060C);
const SPACE_MID: [f32; 3] = rgb(0x0A1E33);
const BEVEL: [f32; 3] = rgb(0x1A3550);
const CYAN: [f32; 3] = rgb(0x22D3FF);
const ICE: [f32; 3] = rgb(0xCFF6FF);
const AMBER: [f32; 3] = rgb(0xFFB347);
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

/// Signed distance to the squircle (a square with 56 px corners), < 0 inside.
fn squircle_distance(x: f32, y: f32) -> f32 {
    let inner = 1.0 - 0.5 * P - CORNER;
    let (qx, qy) = (x.abs() - inner, y.abs() - inner);
    qx.max(0.0).hypot(qy.max(0.0)) + qx.max(qy).min(0.0) - CORNER
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

/// Paints one ring (arcs cut flat and radial, with a soft glow dying a little past each cut).
fn ring(c: &mut [f32; 3], ring: &Ring, colour: [f32; 3], r: f32, theta: f32, px: f32, glow: f32) {
    let half = ring.half.max(px * 0.6);
    let inside = arc_inside(theta, r, ring.phase, ring.arcs, ring.span);
    let d = (r - ring.radius).abs();
    let spread = if r > ring.radius { 0.08 } else { 0.05 };
    let ends = (-(inside.min(0.0) / 0.05).powi(2)).exp();
    add(c, colour, (-((d - half).max(0.0) / spread).powi(2)).exp() * 0.28 * glow * ends);
    over(c, colour, cover(d, half, px) * (inside / px + 0.5).clamp(0.0, 1.0));
}

/// Colour and alpha of one sample at unit coordinates (x, y), y down; `px` = one pixel.
fn sample(x: f32, y: f32, px: f32) -> ([f32; 3], f32) {
    let sd = squircle_distance(x, y);
    let alpha = cover(sd, 0.0, px);
    if alpha <= 0.0 {
        return ([0.0; 3], 0.0);
    }
    // Taskbar frames (32 px and less) keep only a hint of the glows: the bold shapes carry them.
    let glow = if px > 0.05 { 0.35 } else { 1.0 };
    let fine = px < 0.03;
    let (r, theta) = (x.hypot(y), y.atan2(x));
    // Deep space, lifted toward the middle so the black reads as depth, not a hole.
    let mut c = SPACE_EDGE;
    over(&mut c, SPACE_MID, (1.0 - r).clamp(0.0, 1.0).powf(1.5));
    // The cold rim light just inside the edge, brighter on the upper left.
    let bevel = P.max(px * 0.5);
    let lit = 0.7 + 0.3 * (-(x + y) * 0.7).clamp(-1.0, 1.0);
    over(&mut c, BEVEL, cover((sd + bevel).abs(), bevel, px) * lit);
    if fine {
        // The hairline rim and its tick scale: 72 ticks, every sixth one longer and brighter.
        over(&mut c, ICE, cover((r - 0.90).abs(), 0.010, px) * 0.45);
        let step = TAU / 72.0;
        let nearest = (theta / step).round();
        let across = (theta - nearest * step).abs() * r;
        let long = (nearest as i32).rem_euclid(6) == 0;
        let (inner, outer) = if long { (0.77, 0.87) } else { (0.82, 0.87) };
        let reach = cover((r - (inner + outer) / 2.0).abs(), (outer - inner) / 2.0, px);
        over(&mut c, ICE, cover(across, if long { 0.007 } else { 0.004 }, px) * reach * if long { 0.8 } else { 0.4 });
    }
    ring(&mut c, &OUTER, CYAN, r, theta, px, glow);
    ring(&mut c, &MIDDLE, ICE, r, theta, px, glow);
    ring(&mut c, &INNER, AMBER, r, theta, px, glow);
    // The white-hot core in its amber / cyan halo, and (large frames) a fine cyan ring around it.
    let off_core = (r - CORE_RADIUS).max(0.0);
    add(&mut c, AMBER, (-(off_core / 0.07).powi(2)).exp() * 0.55 * glow);
    add(&mut c, CYAN, (-(off_core / 0.18).powi(2)).exp() * 0.16 * glow);
    if fine {
        over(&mut c, CYAN, cover((r - 0.235).abs(), 0.008, px) * 0.8);
    }
    over(&mut c, WHITE, cover(r, CORE_RADIUS.max(px * 0.9), px));
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
#[cfg(jarvis_app)]
pub fn generate_rose_gyro_app_icon() -> eframe::egui::IconData {
    eframe::egui::IconData { rgba: render_icon_rgba(ICON_SIZE), width: ICON_SIZE, height: ICON_SIZE }
}


#[cfg(all(test, jarvis_app))]
mod tests {
    use super::*;

    fn pixel(rgba: &[u8], n: usize, x: usize, y: usize) -> [u8; 4] {
        let o = (y * n + x) * 4;
        [rgba[o], rgba[o + 1], rgba[o + 2], rgba[o + 3]]
    }

    /// The 256 px pixel at `radius` canvas px from the centre, `deg` degrees (y down).
    fn polar(rgba: &[u8], radius: f32, deg: f32) -> [u8; 4] {
        let (s, c) = deg.to_radians().sin_cos();
        pixel(rgba, 256, (128.0 + radius * c) as usize, (128.0 + radius * s) as usize)
    }

    fn is_cyan(p: [u8; 4]) -> bool {
        p[0] < 80 && p[1] > 190 && p[2] > 230
    }

    fn is_ice(p: [u8; 4]) -> bool {
        p[0] > 170 && p[1] > 220 && p[2] > 230
    }

    fn is_amber(p: [u8; 4]) -> bool {
        p[0] > 220 && p[1] > 140 && p[1] < 210 && p[2] < 120
    }

    fn is_dark(p: [u8; 4]) -> bool {
        (p[0] as u32 + p[1] as u32 + p[2] as u32) < 110
    }

    /// The ring's pixel radius on the 256 px canvas.
    fn at(ring: &Ring) -> f32 {
        ring.radius * 128.0
    }

    fn arc_centre(ring: &Ring, k: usize) -> f32 {
        ring.phase.to_degrees() + 360.0 * k as f32 / ring.arcs as f32
    }

    #[test]
    fn icon_is_a_glass_squircle_with_cyan_ice_and_amber_rings_and_a_white_core() {
        let icon = generate_rose_gyro_app_icon();
        assert_eq!((icon.width, icon.height), (256, 256));
        let rgba = &icon.rgba;
        let px = |x, y| pixel(rgba, 256, x, y);
        // Transparent in the rounded corner, opaque deep space inside.
        assert_eq!(px(3, 3)[3], 0);
        assert_eq!(px(128, 6)[3], 255);
        assert!(is_dark(px(128, 6)), "{:?}", px(128, 6));
        // The cold rim light just inside the top edge.
        let bevel = px(128, 1);
        assert!(bevel[2] > 40 && bevel[2] < 100 && bevel[0] < 50, "bevel {bevel:?}");
        for (ring, is_ring, name) in [(&OUTER, is_cyan as fn([u8; 4]) -> bool, "cyan"), (&MIDDLE, is_ice, "ice"), (&INNER, is_amber, "amber")] {
            for k in 0..ring.arcs {
                let (arc, gap) = (arc_centre(ring, k), arc_centre(ring, k) + 180.0 / ring.arcs as f32);
                assert!(is_ring(polar(rgba, at(ring), arc)), "{name} arc at {arc}°: {:?}", polar(rgba, at(ring), arc));
                assert!(!is_ring(polar(rgba, at(ring), gap)), "{name} gap at {gap}°: {:?}", polar(rgba, at(ring), gap));
            }
        }
        // Dark air between the rings, then the white-hot core.
        let air = polar(rgba, 56.0, 0.0);
        assert!(is_dark(air) || air[0] < 120, "{air:?}");
        let c = px(128, 128);
        assert!(c[0] > 245 && c[1] > 245 && c[2] > 245, "{c:?}");
        // The fine parts exist at full size: a long tick on the hairline rim at the top.
        let tick = polar(rgba, 0.82 * 128.0, -90.0);
        assert!(tick[0] > 90 && tick[1] > 120, "tick {tick:?}");
    }

    #[test]
    fn each_ring_repeats_by_its_arc_count() {
        let rgba = render_icon_rgba(256);
        // Sampled well away from every arc cut (the cuts sit at the arc centres ± half the span).
        for (ring, degrees) in [(&OUTER, [20.0f32, 30.0, 40.0, 50.0, 60.0]), (&MIDDLE, [5.0, 10.0, 60.0, 80.0, 100.0]), (&INNER, [15.0, 25.0, 35.0, 80.0, 100.0])] {
            let turn = 360.0 / ring.arcs as f32;
            for deg in degrees {
                let a = polar(&rgba, at(ring), deg);
                for k in 1..ring.arcs {
                    let b = polar(&rgba, at(ring), deg + turn * k as f32);
                    let diff = (0..3).map(|i| (a[i] as i32 - b[i] as i32).abs()).max().unwrap();
                    assert!(diff < 40, "r {} at {deg}° vs +{}°: {a:?} {b:?}", at(ring), turn * k as f32);
                }
            }
        }
        assert!(arc_inside(OUTER.phase, 1.0, OUTER.phase, OUTER.arcs, OUTER.span) > 0.0);
        assert!(arc_inside(OUTER.phase + PI / 4.0, 1.0, OUTER.phase, OUTER.arcs, OUTER.span) < 0.0);
    }

    #[test]
    fn snapshot_icon_frames() {
        // The 256 px icon and the 16 / 32 px taskbar frames (nearest-neighbour ×8 / ×4) as PNGs.
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
            assert!(px(0, 0)[3] < 16, "transparent corner at {size} px: {:?}", px(0, 0));
            let all: Vec<[u8; 4]> = (0..size).flat_map(|y| (0..size).map(move |x| (x, y))).map(|(x, y)| px(x, y)).collect();
            // Cyan and amber survive at taskbar size, the core is bright, the dark dominates.
            assert!(all.iter().any(|p| p[2] > 150 && p[1] > 140 && p[0] < 110), "cyan at {size} px");
            assert!(all.iter().any(|p| p[0] > 150 && p[1] > 100 && p[2] < 110), "amber at {size} px");
            let c = px(size / 2, size / 2).iter().take(3).map(|v| *v as u32).sum::<u32>().max(px(size / 2 - 1, size / 2 - 1).iter().take(3).map(|v| *v as u32).sum());
            assert!(c > 450, "bright core at {size} px: {c}");
            let dark = all.iter().filter(|p| p[3] == 255 && is_dark(**p)).count();
            assert!(dark * 4 > size * size, "dark background pixels at {size} px: {dark}");
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
