//! How the HUD moves. Things arrive fast and settle softly (an expo ease out), leave quicker
//! than they came, and never all at once: a panel is a line first, then a frame, then its
//! header, then its rows one after the other. Positions glide on a critically damped spring,
//! so a window that is pushed aside never overshoots or snaps.

/// A panel's entrance, start to settled, in seconds; its exit takes [`LEAVE`].
pub const ARRIVE: f32 = 0.42;
pub const LEAVE: f32 = 0.22;
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

/// The share of the stretch `from..to` that `t` has covered (0 before it, 1 after).
pub fn phase(t: f32, from: f32, to: f32) -> f32 {
    ((t - from) / (to - from).max(1e-6)).clamp(0.0, 1.0)
}

/// The longest step a motion takes in one frame: down to 10 frames a second everything moves
/// in real time; only a slower frame than that slows it.
pub const LONGEST_STEP: f32 = 0.1;

/// The real time since this window's last frame, for a motion that steps (a spring, a glide, a
/// turn). egui's `stable_dt` is a guess of one 60 Hz refresh whenever the frame before only asked
/// to be drawn later (a window taking its turn, one at rest), and the steps were cut at 50 ms:
/// on a busy PC his glides and his core ran at a third of their speed. Between two frames that
/// both moved this is the time that really passed; the first frame of a motion steps one
/// refresh (a window that slept did not move meanwhile). The same in every call of one frame.
pub fn dt(ctx: &eframe::egui::Context) -> f32 {
    let (id, pass) = (eframe::egui::Id::new(("holo_motion_dt", ctx.viewport_id())), ctx.cumulative_pass_nr());
    let (time, refresh) = ctx.input(|i| (i.time, i.predicted_dt));
    ctx.data_mut(|d| {
        let dt = match d.get_temp::<(u64, f64, f32)>(id) {
            Some((seen, _, dt)) if seen == pass => return dt,
            Some((seen, at, _)) if seen + 1 == pass => ((time - at) as f32).clamp(0.0, LONGEST_STEP),
            _ => refresh,
        };
        d.insert_temp(id, (pass, time, dt));
        dt
    })
}

/// One step of a critically damped spring: `x` follows `target` with stiffness `omega`
/// (higher is quicker; 14 settles in about a third of a second) and never overshoots. (The
/// step is exact for any `dt`: a slow frame moves it as far as the time that passed.)
pub fn spring(x: &mut f32, v: &mut f32, target: f32, omega: f32, dt: f32) {
    let dt = dt.clamp(0.0, LONGEST_STEP);
    let d = *x - target;
    let k = (-omega * dt).exp();
    let c = *v + omega * d;
    *x = target + (d + c * dt) * k;
    *v = (*v - omega * c * dt) * k;
}

/// One frame of `from` closing in on `to` so that it is there (within 1 %) after `secs`.
pub fn approach(from: f32, to: f32, dt: f32, secs: f32) -> f32 {
    let rate = 4.6 / secs.max(0.001);
    let next = to + (from - to) * (-rate * dt.clamp(0.0, LONGEST_STEP)).exp();
    if (next - to).abs() < 0.002 { to } else { next }
}

/// A reading (a gauge, a meter, a percentage) that glides to `value` instead of jumping; at
/// once with the animations off.
pub fn glide(ctx: &eframe::egui::Context, id: eframe::egui::Id, value: f32) -> f32 {
    if !crate::gui::motion_enabled() {
        return value;
    }
    ctx.animate_value_with_time(id, value, LEAVE)
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

    /// Sir, 2026-11-22: "when my PC is lagging that should [not] make his animations lag". A
    /// window drawn every 100 ms (a busy PC, or one taking its turn: it asks for its next frame
    /// later, not at once) moves in real time: a glide of a third of a second has landed half a
    /// second on, and the core turns as far as it would at full speed.
    #[test]
    fn a_busy_pc_slows_no_motion() {
        use eframe::egui::{Context, RawInput};
        let glide = |clock: &dyn Fn(&Context) -> f32| {
            let ctx = Context::default();
            let (mut x, mut v, mut turned) = (0.0f32, 0.0f32, 0.0f32);
            for k in 0..=5 {
                let _ = ctx.run(RawInput { time: Some(k as f64 * 0.1), ..Default::default() }, |ctx| {
                    let dt = clock(ctx);
                    spring(&mut x, &mut v, 100.0, 14.0, dt);
                    turned += dt;
                    ctx.request_repaint_after(std::time::Duration::from_millis(50));
                });
            }
            (x, turned)
        };
        // egui's own clock: one 60 Hz refresh a frame, whatever passed (the fault).
        let (x, turned) = glide(&|ctx| ctx.input(|i| i.stable_dt));
        assert!(x < 80.0 && turned < 0.2, "egui's clock kept time after all: {x}, {turned}");
        // The real one: half a second passed, half a second moved (the first frame one refresh).
        let (x, turned) = glide(&dt);
        assert!(x > 98.0, "the glide is still under way: {x}");
        assert!((turned - (0.5 + 1.0 / 60.0)).abs() < 1e-3, "{turned}");
        // Several calls in one frame step it once.
        let ctx = Context::default();
        let _ = ctx.run(RawInput { time: Some(0.0), ..Default::default() }, |_| {});
        let _ = ctx.run(RawInput { time: Some(0.04), ..Default::default() }, |ctx| {
            let _ = dt(ctx);
        });
        let _ = ctx.run(RawInput { time: Some(0.07), ..Default::default() }, |ctx| {
            assert!((dt(ctx) - 0.03).abs() < 1e-4 && (dt(ctx) - 0.03).abs() < 1e-4);
        });
        // A frame after a long sleep is no leap.
        let _ = ctx.run(RawInput { time: Some(9.0), ..Default::default() }, |ctx| assert!(dt(ctx) <= LONGEST_STEP));
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
    fn approach_is_there_after_its_time_and_never_passes_its_target() {
        let mut v = 0.0;
        for _ in 0..(LEAVE * 60.0).ceil() as usize {
            let next = approach(v, 1.0, 1.0 / 60.0, LEAVE);
            assert!(next >= v && next <= 1.0);
            v = next;
        }
        assert!(v > 0.98, "{v}");
        assert_eq!(approach(0.999, 1.0, 1.0 / 60.0, LEAVE), 1.0, "it lands, it does not creep");
    }
}
