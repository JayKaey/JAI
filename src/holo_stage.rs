//! Where a window opens. In the films nothing is filed into a grid: what J.A.R.V.I.S. brings
//! up stands about the thing it belongs to, the first beside it at eye level, the next on the
//! other side, later ones above and below, each hugging the same circle. Here the thing is the
//! core: windows flank it, alternate sides, follow its range circle as they stack, keep a
//! generous gap, and are tied to it by a leader. Only when nothing fits about the core does a
//! window take any free spot.

use eframe::egui::{pos2, Pos2, Rect, Vec2};

/// The room windows keep between each other.
pub const GAP: f32 = 36.0;

fn clear(candidate: Rect, taken: &[Rect], gap: f32) -> bool {
    !taken.iter().any(|r| r.expand(gap * 0.5 - 0.01).intersects(candidate.expand(gap * 0.5 - 0.01)))
}

fn inside(candidate: Rect, bounds: Rect) -> bool {
    candidate.left() >= bounds.left() - 0.5 && candidate.top() >= bounds.top() - 0.5 && candidate.right() <= bounds.right() + 0.5 && candidate.bottom() <= bounds.bottom() + 0.5
}

/// How far from the core's vertical axis a window stands whose rows run from `top` to `bottom`:
/// on the core's range circle of `reach`, so windows above and below the core's level slide
/// in over it and the group reads as an arc.
fn on_circle(core: Pos2, reach: f32, top: f32, bottom: f32) -> f32 {
    let nearest = if core.y < top {
        top - core.y
    } else if core.y > bottom {
        core.y - bottom
    } else {
        0.0
    };
    (reach * reach - nearest * nearest).max(0.0).sqrt()
}

/// The spot for a window of `size` in `bounds`, about the `core` (its rect), off everything in
/// `taken` by `gap`. None when nothing fits.
pub fn slot(bounds: Rect, size: Vec2, core: Rect, taken: &[Rect], gap: f32) -> Option<Pos2> {
    let c = core.center();
    // The circle windows stand on: the core's own with room for its light.
    let reach = core.width().max(core.height()) * 0.5 + gap.max(core.width() * 0.16);
    let wide = bounds.width() >= bounds.height();
    let mut candidates: Vec<Pos2> = Vec::new();
    // The rows a window may take beside the core: level with it, and above or below what
    // already stands on that side.
    let mut rows = vec![c.y - size.y * 0.5];
    for t in taken {
        rows.push(t.bottom() + gap);
        rows.push(t.top() - gap - size.y);
        rows.push(t.top());
    }
    rows.push(bounds.top());
    rows.push(bounds.bottom() - size.y);
    for &y in &rows {
        let out = on_circle(c, reach, y, y + size.y);
        candidates.push(pos2(c.x + out, y));
        candidates.push(pos2(c.x - out - size.x, y));
        // Further out, past what stands there already.
        for t in taken {
            candidates.push(pos2(t.right() + gap, y));
            candidates.push(pos2(t.left() - gap - size.x, y));
        }
    }
    // Over and under the core, centred on it.
    candidates.push(pos2(c.x - size.x * 0.5, c.y - reach - size.y));
    candidates.push(pos2(c.x - size.x * 0.5, c.y + reach));
    let diagonal = bounds.size().length().max(1.0);
    let (left, right) = taken.iter().fold((0, 0), |(l, r), t| if t.center().x < c.x { (l + 1, r) } else { (l, r + 1) });
    candidates
        .into_iter()
        .map(|at| Rect::from_min_size(at, size))
        // (The core keeps a circle clear, not a square: a window may stand in its corner.)
        .filter(|r| inside(*r, bounds) && clear(*r, taken, gap) && r.distance_to_pos(c) >= reach - 0.5)
        .map(|r| {
            let centre = r.center();
            let on_left = centre.x < c.x;
            // Near the core, level with it, on the emptier side (the right one first, above
            // before below). On a wide screen never over or under the core while a flank is
            // free: his own lines stand under him, the strip over him.
            let on_axis = r.left() < c.x && r.right() > c.x;
            let score = (centre - c).length() / diagonal
                + 0.45 * (centre.y - c.y).abs() / bounds.height().max(1.0)
                + 0.07 * if on_left { left as f32 } else { right as f32 }
                + if on_left && left == right { 0.02 } else { 0.0 }
                + if centre.y > c.y + 1.0 { 0.01 } else { 0.0 }
                + if wide == on_axis { 0.35 } else { 0.0 };
            (score, r.min)
        })
        .min_by(|a, b| a.0.total_cmp(&b.0))
        .map(|(_, at)| at)
}

/// How much of `taken` a window at `r` covers: its area over them, a header (the top `head` of
/// each, where its name and keys are) ten times, and the `core` a hundred times.
fn covered(r: Rect, taken: &[Rect], head: f32, core: Option<Rect>) -> f32 {
    let over = |b: Rect| {
        let i = r.intersect(b);
        if i.is_positive() {
            i.area()
        } else {
            0.0
        }
    };
    taken.iter().map(|t| over(*t) + 9.0 * over(Rect::from_min_size(t.min, Vec2::new(t.width(), head)))).sum::<f32>() + core.map_or(0.0, |c| 100.0 * over(c))
}

/// Where a window of `size` goes in `bounds` when no spot is free of `taken`: where it covers
/// them least (`covered`), of such spots the nearest to `want`. (Sir, 2026-11-21: "When a
/// monitor is to full than he should just put in somewhere so that it doesnt overlay that much.")
pub fn least_covered(bounds: Rect, size: Vec2, taken: &[Rect], head: f32, core: Option<Rect>, want: Pos2) -> Pos2 {
    let mut xs = vec![want.x, bounds.left(), bounds.right() - size.x];
    let mut ys = vec![want.y, bounds.top(), bounds.bottom() - size.y];
    for t in taken.iter().chain(core.as_ref()) {
        xs.extend([t.left(), t.right(), t.left() - size.x, t.right() - size.x]);
        ys.extend([t.top() + head, t.bottom(), t.top() - size.y, t.bottom() - size.y]);
    }
    let mut best = (f32::INFINITY, f32::INFINITY, want);
    for &y in &ys {
        for &x in &xs {
            let at = pos2(x.clamp(bounds.left(), (bounds.right() - size.x).max(bounds.left())), y.clamp(bounds.top(), (bounds.bottom() - size.y).max(bounds.top())));
            let (cost, far) = (covered(Rect::from_min_size(at, size), taken, head, core), (at - want).length());
            if cost < best.0 - 0.5 || (cost <= best.0 + 0.5 && far < best.1) {
                best = (cost, far, at);
            }
        }
    }
    best.2
}

/// Where a window wanted at `want` goes in `bounds`: there when it keeps `gap` from all of
/// `taken`, else the nearest spot that does, else where it covers them least.
pub fn settle(bounds: Rect, want: Rect, taken: &[Rect], gap: f32, head: f32) -> Rect {
    let size = want.size().min(bounds.size());
    let mut xs = vec![want.left(), bounds.left(), bounds.right() - size.x];
    let mut ys = vec![want.top(), bounds.top(), bounds.bottom() - size.y];
    for t in taken {
        xs.extend([t.right() + gap, t.left() - gap - size.x]);
        ys.extend([t.bottom() + gap, t.top() - gap - size.y]);
    }
    let far = |r: &Rect| (r.min - want.min).length();
    ys.iter()
        .flat_map(|&y| xs.iter().map(move |&x| Rect::from_min_size(pos2(x, y), size)))
        .filter(|r| inside(*r, bounds) && clear(*r, taken, gap))
        .min_by(|a, b| far(a).total_cmp(&far(b)))
        .unwrap_or_else(|| Rect::from_min_size(least_covered(bounds, size, taken, head, None, want.min), size))
}

/// The leader that ties a window to the core: from the core's range circle towards the window,
/// to the corner of the window's header that faces the core. None for a window that covers
/// the core or stands on it.
pub fn leader(core: Pos2, reach: f32, window: Rect) -> Option<(Pos2, Pos2)> {
    if window.expand(reach * 0.5).contains(core) {
        return None;
    }
    let to = pos2(if window.center().x >= core.x { window.left() } else { window.right() }, (window.top() + 15.0).min(window.bottom()));
    let dir = (to - core).normalized();
    let from = core + dir * reach;
    ((to - core).length() > reach + 24.0).then_some((from, to))
}

#[cfg(test)]
mod tests {
    use super::*;
    use eframe::egui::vec2;

    fn screen() -> Rect {
        Rect::from_min_size(pos2(0.0, 0.0), vec2(1920.0, 1080.0))
    }

    fn core() -> Rect {
        Rect::from_center_size(pos2(960.0, 540.0), vec2(360.0, 360.0))
    }

    /// The first window stands beside the core at its level, the second on the other side;
    /// none touches the core or another, and each keeps the gap.
    #[test]
    fn windows_flank_the_core_and_alternate_sides() {
        let size = vec2(420.0, 300.0);
        let mut taken: Vec<Rect> = Vec::new();
        for k in 0..6 {
            let at = slot(screen(), size, core(), &taken, GAP).unwrap_or_else(|| panic!("window {k} finds no place"));
            let r = Rect::from_min_size(at, size);
            assert!(screen().contains_rect(r), "{k}: {r:?}");
            assert!(!r.intersects(core()), "{k} covers the core: {r:?}");
            for t in &taken {
                assert!(!r.expand(GAP * 0.5 - 0.5).intersects(t.expand(GAP * 0.5 - 0.5)), "{k} stands too close to {t:?}: {r:?}");
            }
            taken.push(r);
        }
        let (first, second) = (taken[0], taken[1]);
        assert!(first.left() > core().right() && (first.center().y - 540.0).abs() < 1.0, "the first: beside the core, level with it: {first:?}");
        assert!(second.right() < core().left() && (second.center().y - 540.0).abs() < 1.0, "the second: on the other side: {second:?}");
        // Three a side in the end, not six in a row.
        assert_eq!(taken.iter().filter(|r| r.center().x > 960.0).count(), 3, "{taken:?}");
    }

    /// Windows above and below the core's level slide in over it: they hug its circle.
    #[test]
    fn stacked_windows_follow_the_cores_circle() {
        let size = vec2(380.0, 240.0);
        let beside = Rect::from_min_size(slot(screen(), size, core(), &[], GAP).unwrap(), size);
        let mut taken = vec![beside, Rect::from_min_size(pos2(core().left() - 400.0 - 380.0, beside.top()), size)];
        let third = Rect::from_min_size(slot(screen(), size, core(), &taken, GAP).unwrap(), size);
        taken.push(third);
        assert!(third.bottom() <= beside.top() - GAP + 0.5 || third.top() >= beside.bottom() + GAP - 0.5, "it stacks: {third:?}");
        let nearer = if third.center().x > 960.0 { third.left() < beside.left() } else { third.right() > taken[1].right() };
        assert!(nearer, "above the level it stands nearer the axis than the one beside the core: {third:?} / {beside:?}");
    }

    #[test]
    fn a_full_stage_has_no_slot_and_an_upright_screen_stacks_over_and_under() {
        assert_eq!(slot(Rect::from_min_size(pos2(0.0, 0.0), vec2(500.0, 400.0)), vec2(420.0, 300.0), Rect::from_center_size(pos2(250.0, 200.0), vec2(200.0, 200.0)), &[], GAP), None);
        let upright = Rect::from_min_size(pos2(0.0, 0.0), vec2(1080.0, 1920.0));
        let core = Rect::from_center_size(upright.center(), vec2(320.0, 320.0));
        let at = slot(upright, vec2(700.0, 400.0), core, &[], GAP).expect("over or under the core");
        let r = Rect::from_min_size(at, vec2(700.0, 400.0));
        assert!(r.bottom() <= core.top() || r.top() >= core.bottom(), "{r:?}");
    }

    /// Sir, 2026-11-21: a window he moves keeps off the others as a new one does; where the
    /// monitor has no room left, it covers them least and never their headers when it can.
    #[test]
    fn a_moved_window_settles_off_the_others_or_covers_them_least() {
        let standing = Rect::from_min_size(pos2(100.0, 100.0), vec2(600.0, 400.0));
        let want = Rect::from_min_size(pos2(150.0, 150.0), vec2(500.0, 300.0));
        let free = settle(screen(), want, &[standing], GAP, 36.0);
        assert!(free == want.translate(free.min - want.min) && screen().contains_rect(free), "its size kept, on the screen: {free:?}");
        assert!(!free.expand(GAP * 0.5 - 0.5).intersects(standing.expand(GAP * 0.5 - 0.5)), "off the other: {free:?}");
        assert!((free.min - want.min).length() < 700.0, "the nearest free spot, not any: {free:?}");
        let clear = Rect::from_min_size(pos2(1200.0, 500.0), vec2(500.0, 300.0));
        assert_eq!(settle(screen(), clear, &[standing], GAP, 36.0), clear, "where it was sent when that is free");
        // No room at all: four windows fill the screen in quarters.
        let quarters: Vec<Rect> = [(0.0, 0.0), (960.0, 0.0), (0.0, 540.0), (960.0, 540.0)].into_iter().map(|(x, y)| Rect::from_min_size(pos2(x, y), vec2(960.0, 540.0))).collect();
        let full = settle(screen(), want, &quarters, GAP, 36.0);
        assert!(screen().expand(0.5).contains_rect(full), "{full:?}");
        let heads = |r: Rect| quarters.iter().filter(|q| r.intersects(Rect::from_min_size(q.min, vec2(q.width(), 36.0)))).count();
        assert_eq!(heads(full), 0, "it covers no header while a place without one is left: {full:?}");
        // The least covered spot beats the middle of everything.
        let middle = Rect::from_center_size(screen().center(), want.size());
        assert!(covered(full, &quarters, 36.0, None) < covered(middle, &quarters, 36.0, None), "{full:?}");
        // And never the core when it can help it.
        let at = least_covered(screen(), vec2(500.0, 300.0), &quarters, 36.0, Some(core()), pos2(710.0, 390.0));
        assert!(!Rect::from_min_size(at, vec2(500.0, 300.0)).intersect(core()).is_positive(), "an edge on the core's is no cover: {at:?}");
    }

    #[test]
    fn a_leader_runs_from_the_cores_circle_to_the_windows_near_corner() {
        let c = pos2(960.0, 540.0);
        let window = Rect::from_min_size(pos2(1300.0, 300.0), vec2(400.0, 300.0));
        let (from, to) = leader(c, 200.0, window).unwrap();
        assert!(((from - c).length() - 200.0).abs() < 0.01);
        assert_eq!(to, pos2(1300.0, 315.0));
        assert!(leader(c, 200.0, Rect::from_center_size(c, vec2(600.0, 400.0))).is_none(), "a window over the core has none");
    }
}
