//! The emblem as a thing that was built (Sir, 2026-11-12: "more realistic, not cartoony, a bit
//! worn down"): under its rings of light a machined housing (a dark plate with the sheen of
//! brushed metal, bevelled bezels, a channel the light ring sits in, six bolts), the light's own
//! spill on the metal next to it, and over everything the glass it sits behind (a highlight, fine
//! scratches, dust). Nothing here moves: the wear is the same on every start. Sizes are in the
//! emblem's reference pixels (its rim at 175), scaled by `unit`.

use eframe::egui::{pos2, vec2, Color32, Mesh, Painter, Pos2, Shape, Stroke};
use std::f32::consts::{PI, TAU};

/// The emblem's rim in reference pixels.
const RIM: f32 = 175.0;
/// Where the light comes from (up and to the left), as a unit vector on screen.
const LIGHT: (f32, f32) = (-0.6, -0.8);

fn rgba(rgb: [f32; 3], alpha: f32) -> Color32 {
    let a = alpha.clamp(0.0, 1.0);
    Color32::from_rgba_premultiplied((rgb[0].clamp(0.0, 255.0) * a) as u8, (rgb[1].clamp(0.0, 255.0) * a) as u8, (rgb[2].clamp(0.0, 255.0) * a) as u8, (255.0 * a) as u8)
}

fn lerp3(a: [f32; 3], b: [f32; 3], t: f32) -> [f32; 3] {
    let t = t.clamp(0.0, 1.0);
    [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t, a[2] + (b[2] - a[2]) * t]
}

/// A number 0 … 1 that belongs to `n` (the same on every start).
pub fn chance(n: u32) -> f32 {
    let mut x = n.wrapping_mul(0x9E37_79B9) ^ 0x85EB_CA6B;
    x ^= x >> 15;
    x = x.wrapping_mul(0x2C1B_3C6D);
    x ^= x >> 12;
    x = x.wrapping_mul(0x297A_2D39);
    x ^= x >> 15;
    (x >> 8) as f32 / (1u32 << 24) as f32
}

/// Smooth noise round a circle: `theta` in radians, `cells` lobes round it, 0 … 1.
pub fn ring_noise(theta: f32, cells: u32, seed: u32) -> f32 {
    let at = theta.rem_euclid(TAU) / TAU * cells as f32;
    let (i, f) = (at.floor() as u32, at.fract());
    let (a, b) = (chance(seed.wrapping_add(i % cells)), chance(seed.wrapping_add((i + 1) % cells)));
    let s = f * f * (3.0 - 2.0 * f);
    a + (b - a) * s
}

/// How brushed metal catches the light at `theta` round a disc: two bright lobes across from
/// each other (0 … 1).
pub fn sheen(theta: f32) -> f32 {
    // (The lobes stand where the light's direction crosses the brush marks.)
    let lobe = (theta - 0.93).cos().abs();
    lobe.powf(18.0) * 0.85 + lobe.powf(3.0) * 0.15
}

/// How much of the light a ring gives at `theta` (0.62 … 1): no ring of light is even, an old
/// one less so, and it flickers a hair (`clock` in seconds).
pub fn unevenness(theta: f32, clock: f32) -> f32 {
    let slow = ring_noise(theta, 9, 11);
    let fine = ring_noise(theta, 46, 97);
    // Two tired spots that stay where they are.
    let tired = [(1.9f32, 0.22f32), (4.6, 0.14)].iter().map(|(at, depth)| depth * (-((theta.rem_euclid(TAU) - at) / 0.16).powi(2)).exp()).sum::<f32>();
    let flicker = 0.015 * (clock * 23.0 + theta * 3.0).sin() + 0.01 * (clock * 7.3).sin();
    (0.8 + 0.14 * slow + 0.06 * fine - tired + flicker).clamp(0.62, 1.0)
}

/// A band between two radii, shaded per corner: `shade(theta, t)` with `t` 0 at the inner
/// edge and 1 at the outer one.
fn band(mesh: &mut Mesh, centre: Pos2, inner: f32, outer: f32, steps: usize, shade: impl Fn(f32, f32) -> Color32) {
    let base = mesh.vertices.len() as u32;
    for k in 0..=steps {
        let theta = TAU * k as f32 / steps as f32;
        let dir = vec2(theta.cos(), theta.sin());
        mesh.colored_vertex(centre + dir * inner, shade(theta, 0.0));
        mesh.colored_vertex(centre + dir * outer, shade(theta, 1.0));
    }
    for k in 0..steps as u32 {
        let i = base + k * 2;
        mesh.add_triangle(i, i + 1, i + 2);
        mesh.add_triangle(i + 1, i + 3, i + 2);
    }
}

/// How lit an edge is whose outward normal points along `theta` (0 in shadow … 1 facing the
/// light).
fn facing(theta: f32) -> f32 {
    0.5 + 0.5 * (theta.cos() * LIGHT.0 + theta.sin() * LIGHT.1)
}

/// The housing under the rings of light. `accent`: the light's colour (it spills on the metal
/// next to the light ring); `glow`: how strongly the light ring burns right now (0 … 1).
pub fn paint_under(p: &Painter, centre: Pos2, unit: f32, alpha: f32, accent: [u8; 3], glow: f32) {
    if alpha <= 0.01 || unit <= 0.0 {
        return;
    }
    let u = unit;
    let steps = if RIM * u < 80.0 { 72 } else { 180 };
    let acc = [accent[0] as f32, accent[1] as f32, accent[2] as f32];
    let mut mesh = Mesh::default();
    // The light the ring throws round itself (on the canvas and on the housing).
    band(&mut mesh, centre, 121.0 * u, 300.0 * u, steps, |_, t| rgba(acc, (if t < 0.5 { 0.075 } else { 0.0 }) * alpha * (0.6 + 0.4 * glow)));
    // The plate: dark brushed metal (kept dark: black stays black round it).
    let plate = |theta: f32, r: f32| -> [f32; 3] {
        let s = sheen(theta) * (0.55 + 0.45 * ring_noise(theta, 60, 7 + (r as u32)));
        lerp3([9.0, 10.0, 12.0], [70.0, 74.0, 82.0], s * 0.55)
    };
    let metal = |mesh: &mut Mesh, inner: f32, outer: f32, lift: f32| {
        band(mesh, centre, inner * u, outer * u, steps, |theta, t| {
            let c = plate(theta, inner + (outer - inner) * t);
            rgba([c[0] + lift, c[1] + lift, c[2] + lift * 1.1], 0.96 * alpha)
        });
    };
    metal(&mut mesh, 0.0, 91.0, 0.0);
    metal(&mut mesh, 91.0, 103.0, 9.0);
    // The channel the light ring sits in: darker than the plate, lit from inside by the ring.
    band(&mut mesh, centre, 103.0 * u, 121.0 * u, steps, |theta, t| {
        let wall = (t - 0.5).abs() * 2.0;
        let lit = lerp3([4.0, 4.0, 5.0], acc, 0.30 * (1.0 - wall) * unevenness(theta, 0.0) * (0.5 + 0.5 * glow));
        rgba(lit, 0.97 * alpha)
    });
    metal(&mut mesh, 121.0, 160.0, 2.0);
    metal(&mut mesh, 160.0, 181.0, 12.0);
    // The light's spill on the metal either side of the channel.
    band(&mut mesh, centre, 91.0 * u, 103.0 * u, steps, |theta, t| rgba(acc, 0.20 * t * t * alpha * unevenness(theta, 0.0) * (0.5 + 0.5 * glow)));
    band(&mut mesh, centre, 121.0 * u, 139.0 * u, steps, |theta, t| rgba(acc, 0.16 * (1.0 - t) * (1.0 - t) * alpha * unevenness(theta, 0.0) * (0.5 + 0.5 * glow)));
    // The bevels: every step in the housing catches the light on the side that faces it and
    // lies in shadow on the other. (radius, whether the step rises outwards, strength)
    for (r, rises, strength) in [(91.0f32, true, 0.7f32), (103.0, false, 0.55), (121.0, true, 0.55), (160.0, true, 0.9), (181.0, false, 1.0)] {
        let w = (1.3 * u).max(0.8);
        band(&mut mesh, centre, r * u - w, r * u + w, steps, |theta, t| {
            let f = if rises { facing(theta + PI) } else { facing(theta) };
            let edge = 1.0 - (t - 0.5).abs() * 2.0;
            let c = lerp3([0.0, 0.0, 0.0], [178.0, 186.0, 200.0], f);
            rgba(c, (0.16 + 0.5 * (f - 0.5).abs()) * strength * (0.4 + 0.6 * edge) * alpha)
        });
    }
    p.add(Shape::mesh(mesh));
    if RIM * u < 80.0 {
        return;
    }
    // Six bolts in the outer bezel.
    for k in 0..6 {
        // (Off the upright: the marks at twelve and the lettering at six keep their places.)
        let theta = k as f32 * PI / 3.0;
        let at = centre + vec2(theta.cos(), theta.sin()) * 170.5 * u;
        let r = 4.6 * u;
        p.circle_filled(at + vec2(0.6, 0.9) * u, r * 1.15, Color32::from_black_alpha((120.0 * alpha) as u8));
        p.circle_filled(at, r, rgba([30.0, 32.0, 36.0], alpha));
        p.circle_stroke(at, r, Stroke::new((0.9 * u).max(0.6), rgba([120.0, 126.0, 138.0], 0.55 * alpha)));
        // Its slot, each bolt turned its own way.
        let turn = chance(40 + k as u32) * PI;
        let d = vec2(turn.cos(), turn.sin()) * r * 0.72;
        p.line_segment([at - d, at + d], Stroke::new((1.2 * u).max(0.7), Color32::from_black_alpha((210.0 * alpha) as u8)));
        p.line_segment([at - d + vec2(0.0, 0.7 * u), at + d + vec2(0.0, 0.7 * u)], Stroke::new((0.6 * u).max(0.5), rgba([150.0, 156.0, 168.0], 0.28 * alpha)));
    }
}

/// The glass the emblem sits behind, and what years have left on it and on the housing: a
/// highlight, scratches (most of them round with the rings, as a cloth leaves them), chips out
/// of the outer bezel, dust.
pub fn paint_over(p: &Painter, centre: Pos2, unit: f32, alpha: f32) {
    if alpha <= 0.01 || RIM * unit < 80.0 {
        return;
    }
    let u = unit;
    let mut mesh = Mesh::default();
    // The highlight: a soft crescent across the upper left of the glass.
    for (inner, outer, strength) in [(118.0f32, 178.0f32, 0.10f32), (20.0, 88.0, 0.06)] {
        band(&mut mesh, centre, inner * u, outer * u, 96, |theta, t| {
            let towards = (facing(theta) - 0.62).max(0.0) / 0.38;
            let across = 1.0 - (t - 0.5).abs() * 2.0;
            Color32::WHITE.gamma_multiply(strength * towards * towards * across * alpha)
        });
    }
    p.add(Shape::mesh(mesh));
    // Scratches round with the rings.
    for k in 0..22u32 {
        let r = (60.0 + chance(k * 3 + 1) * 119.0) * u;
        let from = chance(k * 3 + 2) * TAU;
        let len = 0.08 + chance(k * 3 + 3) * 0.5;
        let strength = 0.04 + chance(k * 7 + 5) * 0.09;
        let points: Vec<Pos2> = (0..=12).map(|s| from + len * s as f32 / 12.0).map(|a| centre + vec2(a.cos(), a.sin()) * r).collect();
        p.add(Shape::line(points, Stroke::new(1.0f32, Color32::WHITE.gamma_multiply(strength * alpha))));
    }
    // A few straight ones across it.
    for k in 0..5u32 {
        let (a, b) = (chance(200 + k) * TAU, chance(210 + k) * TAU);
        let (ra, rb) = ((40.0 + chance(220 + k) * 130.0) * u, (40.0 + chance(230 + k) * 130.0) * u);
        let (from, to) = (centre + vec2(a.cos(), a.sin()) * ra, centre + vec2(b.cos(), b.sin()) * rb);
        let to = from + (to - from) * (0.25 + 0.3 * chance(240 + k));
        p.line_segment([from, to], Stroke::new(1.0f32, Color32::WHITE.gamma_multiply((0.035 + 0.04 * chance(250 + k)) * alpha)));
    }
    // Chips out of the outer bezel's edge.
    for k in 0..9u32 {
        let theta = chance(300 + k) * TAU;
        let width = 0.008 + chance(310 + k) * 0.018;
        let depth = (1.0 + chance(320 + k) * 2.0) * u;
        let points: Vec<Pos2> = [(-width, 0.0), (-width * 0.4, depth), (width * 0.5, depth * 0.7), (width, 0.0)].iter().map(|(da, dr)| centre + vec2((theta + da).cos(), (theta + da).sin()) * (181.5 * u - dr)).collect();
        p.add(Shape::convex_polygon(points, Color32::from_black_alpha((200.0 * alpha) as u8), Stroke::NONE));
    }
    // Dust on the glass.
    for k in 0..46u32 {
        let (theta, r) = (chance(400 + k) * TAU, chance(450 + k).sqrt() * 176.0 * u);
        let at = centre + vec2(theta.cos(), theta.sin()) * r;
        let size = 0.6 + chance(500 + k) * 0.9;
        p.rect_filled(eframe::egui::Rect::from_center_size(at, vec2(size, size)), eframe::egui::Rounding::ZERO, Color32::WHITE.gamma_multiply((0.05 + 0.12 * chance(550 + k)) * alpha));
    }
    let _ = pos2(0.0, 0.0);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_wear_is_the_same_on_every_start_and_the_light_never_even() {
        assert_eq!(chance(7), chance(7));
        assert!((0..200).all(|n| (0.0..1.0).contains(&chance(n))));
        let spread: Vec<f32> = (0..64).map(|k| unevenness(TAU * k as f32 / 64.0, 0.0)).collect();
        let (least, most) = spread.iter().fold((1.0f32, 0.0f32), |(lo, hi), v| (lo.min(*v), hi.max(*v)));
        assert!(least >= 0.62 && most <= 1.0 && most - least > 0.12, "uneven, within bounds: {least} … {most}");
        // The flicker is a hair, not a blink.
        assert!((unevenness(1.0, 0.0) - unevenness(1.0, 0.07)).abs() < 0.05);
        // The noise closes round the circle, and the sheen has its two lobes across from each other.
        assert!((ring_noise(0.0, 9, 3) - ring_noise(TAU, 9, 3)).abs() < 1e-4);
        assert!(sheen(0.93) > 0.95 && sheen(0.93 + PI) > 0.95 && sheen(0.93 + PI / 2.0) < 0.05);
    }
}
