//! The emblem's light as light behaves (Sir, 2026-11-12: "simpler, more natural and realistic";
//! a housing of metal round it, tried the same evening, he did not want). Nothing is added to
//! the emblem: its rings of light are a little uneven, as every ring of light is, and they
//! flicker by a hair.

use std::f32::consts::TAU;

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

// (The glow the bright ring threw into the dark round it went with the film look: nothing is
// added round the emblem, its lines carry their own faint light.)

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
