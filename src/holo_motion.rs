//! How the HUD moves. Things arrive fast and settle softly (an expo ease out), leave quicker
//! than they came, and never all at once: a panel is a line first, then a frame, then its
//! header, then its rows one after the other. Positions glide on a critically damped spring,
//! so a window that is pushed aside never overshoots or snaps.

/// A panel's entrance, start to settled, in seconds; its exit takes [`LEAVE`].
pub const ARRIVE: f32 = 0.42;
pub const LEAVE: f32 = 0.22;
/// The pause between one row of a cascade and the next.
pub const STAGGER: f32 = 0.028;
/// A small thing answering the pointer (a key lighting, a toggle moving).
pub const TOUCH: f32 = 0.12;

pub fn ease_out_expo(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    if t >= 1.0 { 1.0 } else { 1.0 - 2f32.powf(-10.0 * t) }
}

pub fn ease_out_cubic(t: f32) -> f32 {
    1.0 - (1.0 - t.clamp(0.0, 1.0)).powi(3)
}

pub fn ease_in_cubic(t: f32) -> f32 {
    t.clamp(0.0, 1.0).powi(3)
}

pub fn ease_in_out(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    if t < 0.5 { 4.0 * t * t * t } else { 1.0 - (-2.0 * t + 2.0).powi(3) / 2.0 }
}

/// A small overshoot, for a thing that pops into place (a key, a dot).
pub fn ease_out_back(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0) - 1.0;
    1.0 + 2.2 * t * t * t + 1.2 * t * t
}

/// The share of the stretch `from..to` that `t` has covered (0 before it, 1 after).
pub fn phase(t: f32, from: f32, to: f32) -> f32 {
    ((t - from) / (to - from).max(1e-6)).clamp(0.0, 1.0)
}

/// How far row `index` of a cascade is in, `age` seconds after the cascade began: each row
/// starts [`STAGGER`] after the one before and takes `each` seconds.
pub fn cascade(age: f32, index: usize, each: f32) -> f32 {
    ease_out_cubic((age - index as f32 * STAGGER) / each.max(1e-3))
}

/// One step of a critically damped spring: `x` follows `target` with stiffness `omega`
/// (higher is quicker; 14 settles in about a third of a second) and never overshoots.
pub fn spring(x: &mut f32, v: &mut f32, target: f32, omega: f32, dt: f32) {
    let dt = dt.clamp(0.0, 0.05);
    let d = *x - target;
    let k = (-omega * dt).exp();
    let c = *v + omega * d;
    *x = target + (d + c * dt) * k;
    *v = (*v - omega * c * dt) * k;
}

/// `value` towards `target` at `rate` a second, without passing it.
pub fn approach(value: f32, target: f32, rate: f32, dt: f32) -> f32 {
    let step = rate * dt.max(0.0);
    if (target - value).abs() <= step { target } else { value + step * (target - value).signum() }
}

/// A title being decoded: the letters up to `progress` (0-1) are final, the next few are
/// still cycling through glyphs, the rest is not there yet. `clock` drives the cycling.
pub fn decode(text: &str, progress: f32, clock: f64) -> String {
    if progress >= 1.0 {
        return text.to_string();
    }
    const GLYPHS: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ0123456789/<>-+";
    let chars: Vec<char> = text.chars().collect();
    let done = (progress.max(0.0) * chars.len() as f32) as usize;
    let tick = (clock * 30.0) as usize;
    chars
        .iter()
        .enumerate()
        .filter(|(i, _)| *i < done + 3)
        .map(|(i, c)| if i < done || c.is_whitespace() { *c } else { GLYPHS[(i * 7 + tick * 3) % GLYPHS.len()] as char })
        .collect()
}

/// A figure counting up to `value` as `progress` goes 0-1 (the last digits settle last).
pub fn count_up(value: f64, progress: f32) -> f64 {
    value * ease_out_cubic(progress) as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eases_start_at_nought_end_at_one_and_never_go_backwards() {
        for ease in [ease_out_expo as fn(f32) -> f32, ease_out_cubic, ease_in_cubic, ease_in_out] {
            assert!(ease(0.0).abs() < 2e-3 && (ease(1.0) - 1.0).abs() < 1e-6);
            let mut last = -1.0;
            for k in 0..=100 {
                let v = ease(k as f32 / 100.0);
                assert!(v >= last - 1e-6, "an ease runs backwards");
                last = v;
            }
        }
        // Arriving is fast first: half the way in the first sixth of the time.
        assert!(ease_out_expo(1.0 / 6.0) > 0.5);
        // The pop passes its place a little and comes back.
        assert!((0..100).any(|k| ease_out_back(k as f32 / 100.0) > 1.0) && (ease_out_back(1.0) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn a_spring_reaches_its_target_without_overshoot() {
        let (mut x, mut v) = (0.0f32, 0.0f32);
        let mut peak = 0.0f32;
        for _ in 0..90 {
            spring(&mut x, &mut v, 100.0, 14.0, 1.0 / 60.0);
            peak = peak.max(x);
        }
        assert!(peak <= 100.0 + 1e-3, "overshot to {peak}");
        assert!((x - 100.0).abs() < 0.5, "settled at {x} after 1.5 s");
        // A third of a second in, it is most of the way.
        let (mut x, mut v) = (0.0f32, 0.0f32);
        for _ in 0..20 {
            spring(&mut x, &mut v, 100.0, 14.0, 1.0 / 60.0);
        }
        assert!(x > 90.0, "{x}");
    }

    #[test]
    fn rows_cascade_one_after_the_other() {
        assert_eq!(cascade(0.0, 0, 0.2), 0.0);
        assert!(cascade(0.1, 0, 0.2) > cascade(0.1, 2, 0.2));
        assert_eq!(cascade(0.1, 6, 0.2), 0.0, "the seventh row has not started");
        assert_eq!(cascade(1.0, 6, 0.2), 1.0);
    }

    #[test]
    fn a_title_decodes_left_to_right_into_itself() {
        assert_eq!(decode("NETWORK RADAR", 1.0, 0.3), "NETWORK RADAR");
        let half = decode("NETWORK RADAR", 0.5, 0.3);
        assert!(half.starts_with("NETWOR") && half.chars().count() <= 9, "{half}");
        assert_eq!(decode("NETWORK RADAR", 0.0, 0.0).chars().count(), 3);
        assert_ne!(decode("ABCDEF", 0.2, 0.0), decode("ABCDEF", 0.2, 0.1), "the undecided letters cycle");
    }

    #[test]
    fn approach_never_passes_its_target() {
        assert_eq!(approach(0.0, 1.0, 4.0, 0.1), 0.4);
        assert_eq!(approach(0.9, 1.0, 4.0, 0.1), 1.0);
        assert_eq!(approach(1.0, 0.0, 4.0, 1.0), 0.0);
    }
}
