//! The emblem's light as light behaves (Sir, 2026-11-12: "simpler, more natural and realistic";
//! a housing of metal round it, tried the same evening, he did not want). Nothing is added to
//! the emblem: its rings of light are a little uneven, as every ring of light is, they flicker
//! by a hair, and they throw a soft glow into the dark round them. Sizes are in the emblem's
//! reference pixels (its rim at 175), scaled by `unit`.

use eframe::egui::{vec2, Color32, Mesh, Painter, Pos2, Shape};
use std::f32::consts::TAU;

fn rgba(rgb: [f32; 3], alpha: f32) -> Color32 {
    let a = alpha.clamp(0.0, 1.0);
    Color32::from_rgba_premultiplied((rgb[0].clamp(0.0, 255.0) * a) as u8, (rgb[1].clamp(0.0, 255.0) * a) as u8, (rgb[2].clamp(0.0, 255.0) * a) as u8, (255.0 * a) as u8)
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

/// How much of its light a ring gives at `theta` (0.84 … 1): no ring of light is perfectly
/// even, and it flickers by a hair (`clock` in seconds). Gentle: a living light, not a
/// broken one.
pub fn unevenness(theta: f32, clock: f32) -> f32 {
    let slow = ring_noise(theta, 7, 11);
    let fine = ring_noise(theta, 31, 97);
    let breath = 0.012 * (clock * 0.9 + theta).sin();
    let flicker = 0.008 * (clock * 19.0 + theta * 3.0).sin();
    (0.88 + 0.09 * slow + 0.03 * fine + breath + flicker).clamp(0.84, 1.0)
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

/// The light the bright ring throws into the dark round it: strongest at the ring, gone a
/// little beyond the rim outside and before the middle inside. `accent`: the light's colour;
/// `glow`: how strongly the ring burns right now (0 … 1).
pub fn paint_glow(p: &Painter, centre: Pos2, unit: f32, alpha: f32, accent: [u8; 3], glow: f32) {
    if alpha <= 0.01 || unit <= 0.0 {
        return;
    }
    let u = unit;
    let acc = [accent[0] as f32, accent[1] as f32, accent[2] as f32];
    let strength = alpha * (0.6 + 0.4 * glow.clamp(0.0, 1.0));
    let steps = 96;
    let mut mesh = Mesh::default();
    // (The bright ring stands at 112: three bands outwards, each fainter, and one inwards.)
    for (inner, outer, from, to) in [(112.0f32, 150.0f32, 0.085f32, 0.045f32), (150.0, 215.0, 0.045, 0.014), (215.0, 300.0, 0.014, 0.0), (64.0, 112.0, 0.0, 0.06)] {
        band(&mut mesh, centre, inner * u, outer * u, steps, |theta, t| rgba(acc, (from + (to - from) * t) * strength * unevenness(theta, 0.0)));
    }
    p.add(Shape::mesh(mesh));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_light_is_gently_uneven_and_the_same_on_every_start() {
        assert_eq!(chance(7), chance(7));
        assert!((0..200).all(|n| (0.0..1.0).contains(&chance(n))));
        let spread: Vec<f32> = (0..64).map(|k| unevenness(TAU * k as f32 / 64.0, 0.0)).collect();
        let (least, most) = spread.iter().fold((1.0f32, 0.0f32), |(lo, hi), v| (lo.min(*v), hi.max(*v)));
        assert!(least >= 0.84 && most <= 1.0 && most - least > 0.03 && most - least < 0.16, "gently uneven: {least} … {most}");
        // The flicker is a hair, not a blink; the noise closes round the circle.
        assert!((unevenness(1.0, 0.0) - unevenness(1.0, 0.07)).abs() < 0.03);
        assert!((ring_noise(0.0, 9, 3) - ring_noise(TAU, 9, 3)).abs() < 1e-4);
    }
}
