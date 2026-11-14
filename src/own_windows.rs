//! His own windows on the desktop, as windows. Lined up on the monitor Sir is using when he
//! asks ("tidy the windows"; nothing lines up by itself), and moved by gliding there, never by
//! a jump. (Sir, 2026-11-13: "he shouldn't teleport windows and his orb and so around, he should
//! move them with smooth animations".)

use std::sync::Mutex;
use std::time::{Duration, Instant};

/// How long a window takes to glide where it was sent.
pub const GLIDE: Duration = Duration::from_millis(280);
/// The gap between two windows that are lined up, and round them.
const GAP: i32 = 14;

/// A glide: the window, from where (x, y, w, h), to where, since when.
type Glide = (isize, [i32; 4], [i32; 4], Instant);
static GLIDES: Mutex<Vec<Glide>> = Mutex::new(Vec::new());

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Where a glide from `from` to `to` (x, y, w, h) stands after the share `t` of its time: fast
/// at first, slow at the end.
pub fn between(from: [i32; 4], to: [i32; 4], t: f32) -> [i32; 4] {
    let t = 1.0 - (1.0 - t.clamp(0.0, 1.0)).powi(3);
    std::array::from_fn(|i| (from[i] as f32 + (to[i] - from[i]) as f32 * t).round() as i32)
}

/// Window `hwnd` glides from `from` to `to` (x, y, w, h). A window already on its way goes on
/// from where it is.
pub fn glide(hwnd: isize, from: [i32; 4], to: [i32; 4]) {
    let mut glides = lock(&GLIDES);
    let now = Instant::now();
    let from = match glides.iter().position(|g| g.0 == hwnd) {
        Some(k) => {
            let (_, a, b, since) = glides.remove(k);
            between(a, b, since.elapsed().as_secs_f32() / GLIDE.as_secs_f32())
        }
        None => from,
    };
    if from != to {
        glides.push((hwnd, from, to, now));
        crate::gui::refresh();
    }
}

/// One step of every glide, from the board's frame (its thread owns his windows). Whether any
/// window is still on its way (the board then draws on).
#[cfg(windows)]
pub fn step() -> bool {
    let mut glides = lock(&GLIDES);
    glides.retain(|(hwnd, from, to, since)| {
        // (With the interface's animations off it is simply there.)
        let t = if crate::gui::motion_enabled() { since.elapsed().as_secs_f32() / GLIDE.as_secs_f32() } else { 1.0 };
        crate::win32_desktop::place(*hwnd, between(*from, *to, t));
        t < 1.0
    });
    !glides.is_empty()
}

#[cfg(not(windows))]
pub fn step() -> bool {
    false
}

/// The cells (x, y, w, h) of a grid for `count` windows in the work area `work` (left, top,
/// right, bottom): as many columns as rows, or one more; a gap between them and round them.
pub fn cells(work: [i32; 4], count: usize) -> Vec<[i32; 4]> {
    if count == 0 {
        return Vec::new();
    }
    let (w, h) = (work[2] - work[0], work[3] - work[1]);
    // (An upright monitor stacks them: more rows than columns.)
    let cols = if h > w { ((count as f32 * w as f32 / h as f32).sqrt().floor() as usize).max(1) } else { (count as f32).sqrt().ceil() as usize };
    let rows = count.div_ceil(cols);
    let (cw, ch) = ((w - GAP * (cols as i32 + 1)) / cols as i32, (h - GAP * (rows as i32 + 1)) / rows as i32);
    (0..count).map(|k| [work[0] + GAP + (k % cols) as i32 * (cw + GAP), work[1] + GAP + (k / cols) as i32 * (ch + GAP), cw, ch]).collect()
}

/// Where each of `windows` (its handle and rect: left, top, right, bottom) goes when they are
/// lined up in `work`: in reading order by where they stand now, each in its cell at its own
/// size, made smaller only where it is larger than the cell. (handle, from, to), each x, y, w, h.
pub fn lined_up(work: [i32; 4], windows: &[(isize, [i32; 4])]) -> Vec<(isize, [i32; 4], [i32; 4])> {
    let mut order: Vec<&(isize, [i32; 4])> = windows.iter().collect();
    order.sort_by_key(|(_, r)| (r[1].div_euclid(240), r[0]));
    order
        .into_iter()
        .zip(cells(work, windows.len()))
        .map(|((hwnd, [l, t, r, b]), [x, y, cw, ch])| {
            let from = [*l, *t, r - l, b - t];
            (*hwnd, from, [x, y, from[2].min(cw), from[3].min(ch)])
        })
        .collect()
}

/// His windows as windows: all but the board itself and the corner orb.
#[cfg(windows)]
pub fn list() -> Vec<crate::win32_desktop::OwnWindow> {
    let board = crate::gui::hud_hwnd();
    crate::win32_desktop::own_windows().into_iter().filter(|w| w.hwnd != board && w.title != "J.A.I. // orb").collect()
}

/// "Tidy the windows": his windows glide into one grid on the monitor Sir is using. How many.
#[cfg(windows)]
pub fn tidy_now() -> usize {
    let own = list();
    let monitors = crate::monitors::recent();
    let Some(on) = crate::wallpaper::monitor_in_use(&monitors) else { return 0 };
    let moves = lined_up(on.work, &own.iter().map(|w| (w.hwnd, w.rect)).collect::<Vec<_>>());
    let n = moves.len();
    for (hwnd, from, to) in moves {
        glide(hwnd, from, to);
    }
    if n > 0 {
        tracing::info!("[HUD] Tidied: {n} of his windows lined up on the monitor at {},{}", on.rect[0], on.rect[1]);
    }
    n
}

#[cfg(not(windows))]
pub fn tidy_now() -> usize {
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_glide_starts_fast_ends_slow_and_arrives_exactly() {
        let (from, to) = ([100, 100, 520, 460], [1400, 300, 520, 460]);
        assert_eq!(between(from, to, 0.0), from);
        assert_eq!(between(from, to, 1.0), to);
        assert_eq!(between(from, to, 2.0), to, "never past it");
        let (quarter, half) = (between(from, to, 0.25), between(from, to, 0.5));
        assert!(quarter[0] - from[0] > to[0] - half[0], "the first quarter covers more than the second half");
        // No step of sixty a second is a jump.
        let mut last = from;
        for k in 1..=17 {
            let now = between(from, to, k as f32 / 17.0);
            assert!((now[0] - last[0]).abs() < 260, "{} px in one frame", now[0] - last[0]);
            assert!(now[0] >= last[0]);
            last = now;
        }
    }

    #[test]
    fn windows_line_up_in_a_grid_that_fills_the_monitor() {
        let work = [0, 0, 2560, 1392];
        assert!(cells(work, 0).is_empty());
        assert_eq!(cells(work, 1), vec![[14, 14, 2532, 1364]]);
        let four = cells(work, 4);
        assert_eq!(four.len(), 4);
        assert_eq!((four[0][0], four[0][1]), (14, 14));
        assert_eq!(four[1][0], four[0][0] + four[0][2] + 14, "a gap between two in a row");
        assert_eq!(four[2][1], four[0][1] + four[0][3] + 14, "and between two rows");
        // Five: three columns and two rows, none outside the monitor, none on another.
        let five = cells(work, 5);
        for (k, c) in five.iter().enumerate() {
            assert!(c[0] >= 0 && c[1] >= 0 && c[0] + c[2] <= 2560 && c[1] + c[3] <= 1392, "{c:?}");
            for o in &five[k + 1..] {
                assert!(c[0] + c[2] <= o[0] || o[0] + o[2] <= c[0] || c[1] + c[3] <= o[1] || o[1] + o[3] <= c[1], "{c:?} lies on {o:?}");
            }
        }
        // The upright monitor stacks them.
        let tall = cells([-1080, -290, 0, 1582], 3);
        assert!(tall.iter().all(|c| c[0] == -1066), "one column: {tall:?}");
    }

    #[test]
    fn lined_up_they_keep_their_order_and_their_size_where_it_fits() {
        let work = [0, 0, 2560, 1392];
        // (As they lie about: the second one stands top left.)
        let windows = [(1, [1500, 700, 2020, 1160]), (2, [60, 40, 700, 560]), (3, [900, 60, 2500, 1500])];
        let moves = lined_up(work, &windows);
        assert_eq!(moves.iter().map(|m| m.0).collect::<Vec<_>>(), vec![2, 3, 1], "in reading order");
        let (_, from, to) = moves[0];
        assert_eq!(from, [60, 40, 640, 520]);
        assert_eq!((to[0], to[1], to[2], to[3]), (14, 14, 640, 520), "its own size, in the first cell");
        // The one larger than a cell is made as small as its cell.
        let (_, _, big) = moves[1];
        let cell = cells(work, 3)[1];
        assert_eq!((big[2], big[3]), (cell[2], cell[3]));
    }
}
