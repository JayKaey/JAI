//! Holographic window physics for the spatial workspace: spring-damped launch, re-tile and
//! collapse motion, magnetic edge snapping (12 px) with alignment guides, and the Win32
//! click-through "ghost mode" that lets a window float over a game while every click passes
//! through to it.
//!
//! All positions are physical screen pixels on the virtual desktop.

use eframe::egui::{pos2, vec2, Pos2, Rect, Vec2};

pub const STIFFNESS: f32 = 180.0;
pub const DAMPING: f32 = 22.0;
pub const SNAP_THRESHOLD: f32 = 12.0;
/// Background alpha of a window in ghost mode (45 %).
pub const GHOST_ALPHA: u8 = 115;
/// Integration step (stable for these constants).
const STEP: f32 = 1.0 / 240.0;

/// A 2-D spring-damper: `current` chases `target`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SpringVec2 {
    pub current: Pos2,
    pub target: Pos2,
    pub velocity: Vec2,
}

impl SpringVec2 {
    pub fn at(p: Pos2) -> Self {
        Self { current: p, target: p, velocity: Vec2::ZERO }
    }

    /// Starts at `from` and glides to `to`.
    pub fn launch(from: Pos2, to: Pos2) -> Self {
        Self { current: from, target: to, velocity: Vec2::ZERO }
    }

    pub fn retarget(&mut self, to: Pos2) {
        self.target = to;
    }

    /// Places the spring at `p` with no motion (dragging).
    pub fn hold(&mut self, p: Pos2) {
        *self = Self::at(p);
    }

    pub fn settled(&self) -> bool {
        (self.current - self.target).length() < 0.5 && self.velocity.length() < 2.0
    }

    /// Advances by `dt` seconds (semi-implicit Euler in 1/240 s steps); false once at rest.
    pub fn step(&mut self, dt: f32) -> bool {
        let mut left = dt.clamp(0.0, 0.1);
        while left > 0.0 {
            let h = left.min(STEP);
            let accel = -STIFFNESS * (self.current - self.target) - DAMPING * self.velocity;
            self.velocity += accel * h;
            self.current += self.velocity * h;
            left -= h;
        }
        if self.settled() {
            self.current = self.target;
            self.velocity = Vec2::ZERO;
            return false;
        }
        true
    }
}

/// A scalar spring (opacity, collapse scale).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SpringF32 {
    pub current: f32,
    pub target: f32,
    pub velocity: f32,
}

impl SpringF32 {
    pub fn at(v: f32) -> Self {
        Self { current: v, target: v, velocity: 0.0 }
    }

    pub fn step(&mut self, dt: f32) -> bool {
        let mut left = dt.clamp(0.0, 0.1);
        while left > 0.0 {
            let h = left.min(STEP);
            let accel = -STIFFNESS * (self.current - self.target) - DAMPING * self.velocity;
            self.velocity += accel * h;
            self.current += self.velocity * h;
            left -= h;
        }
        if (self.current - self.target).abs() < 0.002 && self.velocity.abs() < 0.01 {
            self.current = self.target;
            self.velocity = 0.0;
            return false;
        }
        true
    }
}

/// An alignment guide to flash: a vertical (x = `at`) or horizontal (y = `at`) line from
/// `from` to `to` along the other axis.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Guide {
    pub vertical: bool,
    pub at: f32,
    pub from: f32,
    pub to: f32,
}

/// Snaps the dragged `rect` to monitor edges and to the edges of the other windows within
/// `threshold` pixels; returns the snapped rectangle and the guides that fired.
pub fn magnetic_snap(rect: Rect, others: &[Rect], screens: &[Rect], threshold: f32) -> (Rect, Vec<Guide>) {
    // (delta to apply, guide coordinate, span of the partner along the other axis).
    let mut best_x: Option<(f32, f32, f32, f32)> = None;
    let mut best_y: Option<(f32, f32, f32, f32)> = None;
    let consider = |best: &mut Option<(f32, f32, f32, f32)>, delta: f32, at: f32, from: f32, to: f32| {
        if delta.abs() <= threshold && best.is_none_or(|(d, ..)| delta.abs() < d.abs()) {
            *best = Some((delta, at, from, to));
        }
    };
    for s in screens {
        for (edge, target) in [(rect.left(), s.left()), (rect.right(), s.right())] {
            consider(&mut best_x, target - edge, target, s.top(), s.bottom());
        }
        for (edge, target) in [(rect.top(), s.top()), (rect.bottom(), s.bottom())] {
            consider(&mut best_y, target - edge, target, s.left(), s.right());
        }
    }
    for o in others {
        // Partners on the same band only (a window far above does not pull sideways).
        let near_v = rect.top() < o.bottom() + 160.0 && rect.bottom() > o.top() - 160.0;
        let near_h = rect.left() < o.right() + 160.0 && rect.right() > o.left() - 160.0;
        if near_v {
            for (edge, target) in [(rect.left(), o.left()), (rect.right(), o.right()), (rect.left(), o.right()), (rect.right(), o.left())] {
                consider(&mut best_x, target - edge, target, rect.top().min(o.top()), rect.bottom().max(o.bottom()));
            }
        }
        if near_h {
            for (edge, target) in [(rect.top(), o.top()), (rect.bottom(), o.bottom()), (rect.top(), o.bottom()), (rect.bottom(), o.top())] {
                consider(&mut best_y, target - edge, target, rect.left().min(o.left()), rect.right().max(o.right()));
            }
        }
    }
    let mut snapped = rect;
    let mut guides = Vec::new();
    if let Some((dx, at, from, to)) = best_x {
        snapped = snapped.translate(vec2(dx, 0.0));
        guides.push(Guide { vertical: true, at, from, to });
    }
    if let Some((dy, at, from, to)) = best_y {
        snapped = snapped.translate(vec2(0.0, dy));
        guides.push(Guide { vertical: false, at, from, to });
    }
    (snapped, guides)
}

/// Monitor rectangles (physical pixels) of the work areas.
pub fn screen_rects() -> Vec<Rect> {
    #[cfg(windows)]
    {
        crate::win32::monitor_work_areas().into_iter().map(|[x, y, w, h]| Rect::from_min_size(pos2(x as f32, y as f32), vec2(w as f32, h as f32))).collect()
    }
    #[cfg(not(windows))]
    {
        vec![Rect::from_min_size(Pos2::ZERO, vec2(1920.0, 1080.0))]
    }
}

/// The cursor in physical screen pixels.
pub fn cursor_pos() -> Option<Pos2> {
    #[cfg(windows)]
    {
        use windows::Win32::Foundation::POINT;
        use windows::Win32::UI::WindowsAndMessaging::GetCursorPos;
        let mut pt = POINT::default();
        // SAFETY: sized out-parameter.
        unsafe { GetCursorPos(&mut pt) }.ok()?;
        Some(pos2(pt.x as f32, pt.y as f32))
    }
    #[cfg(not(windows))]
    {
        None
    }
}

/// Whether the left mouse button is physically held (ends a drag that left the window).
pub fn left_button_down() -> bool {
    #[cfg(windows)]
    {
        use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_LBUTTON};
        // SAFETY: plain key-state query.
        (unsafe { GetAsyncKeyState(VK_LBUTTON.0 as i32) } as u16 & 0x8000) != 0
    }
    #[cfg(not(windows))]
    {
        false
    }
}

/// The native window with exactly this title (the workspace windows' titles are unique).
pub fn find_window(title: &str) -> Option<isize> {
    #[cfg(windows)]
    {
        use windows::core::PCWSTR;
        use windows::Win32::UI::WindowsAndMessaging::FindWindowW;
        let wide: Vec<u16> = title.encode_utf16().chain(std::iter::once(0)).collect();
        // SAFETY: NUL-terminated buffer alive for the call.
        let hwnd = unsafe { FindWindowW(PCWSTR::null(), PCWSTR(wide.as_ptr())) };
        (hwnd.0 != 0).then_some(hwnd.0)
    }
    #[cfg(not(windows))]
    {
        let _ = title;
        None
    }
}

/// Ghost mode: `WS_EX_LAYERED | WS_EX_TRANSPARENT` (every click passes through) plus
/// topmost, so the window floats over a game or IDE without taking input. Off again restores
/// hit-testing (the window stays layered at full opacity, which changes nothing visible).
/// The extended style of a window entering (or leaving) ghost mode: `WS_EX_LAYERED |
/// WS_EX_TRANSPARENT | WS_EX_TOPMOST` on, only `WS_EX_TRANSPARENT` cleared again (the window
/// stays layered; Windows owns the topmost bit, which `SetWindowPos` changes).
#[cfg(windows)]
pub fn ghost_ex_style(ex: isize, enabled: bool) -> isize {
    use windows::Win32::UI::WindowsAndMessaging::{WS_EX_LAYERED, WS_EX_TOPMOST, WS_EX_TRANSPARENT};
    if enabled {
        ex | (WS_EX_LAYERED.0 | WS_EX_TRANSPARENT.0 | WS_EX_TOPMOST.0) as isize
    } else {
        (ex & !(WS_EX_TRANSPARENT.0 as isize)) | WS_EX_LAYERED.0 as isize
    }
}

#[cfg(windows)]
pub fn set_window_click_through(hwnd: isize, enabled: bool) -> Result<(), String> {
    use windows::Win32::Foundation::{COLORREF, HWND};
    use windows::Win32::UI::WindowsAndMessaging::{
        GetWindowLongPtrW, SetLayeredWindowAttributes, SetWindowLongPtrW, SetWindowPos, GWL_EXSTYLE, HWND_NOTOPMOST, HWND_TOPMOST, LWA_ALPHA,
        SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE,
    };
    let h = HWND(hwnd);
    // SAFETY: style queries/changes on a window of this process; no pointers are kept.
    unsafe {
        let ex = GetWindowLongPtrW(h, GWL_EXSTYLE);
        SetWindowLongPtrW(h, GWL_EXSTYLE, ghost_ex_style(ex, enabled));
        SetLayeredWindowAttributes(h, COLORREF(0), 255, LWA_ALPHA).map_err(|e| e.message().to_string())?;
        let order = if enabled { HWND_TOPMOST } else { HWND_NOTOPMOST };
        SetWindowPos(h, order, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_FRAMECHANGED).map_err(|e| e.message().to_string())
    }
}

#[cfg(not(windows))]
pub fn set_window_click_through(_hwnd: isize, _enabled: bool) -> Result<(), String> {
    Err("click-through windows need Windows".into())
}

/// Always-on-top on or off.
#[cfg(windows)]
pub fn set_topmost(hwnd: isize, on: bool) -> Result<(), String> {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::{SetWindowPos, HWND_NOTOPMOST, HWND_TOPMOST, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE};
    // SAFETY: z-order change of a window of this process.
    unsafe { SetWindowPos(HWND(hwnd), if on { HWND_TOPMOST } else { HWND_NOTOPMOST }, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE) }
        .map_err(|e| e.message().to_string())
}

#[cfg(not(windows))]
pub fn set_topmost(_hwnd: isize, _on: bool) -> Result<(), String> {
    Ok(())
}

/// A balanced, non-overlapping Bento cockpit of `n` windows around `center` inside `area`:
/// left and right columns (the orb keeps the middle), each split into rows.
pub fn fan_out_layout(n: usize, area: Rect, center: Pos2) -> Vec<Rect> {
    if n == 0 {
        return Vec::new();
    }
    let gap = 16.0;
    let clear = (area.width() * 0.22).clamp(260.0, 520.0);
    let col_w = ((area.width() - clear - 4.0 * gap) / 2.0).clamp(320.0, 760.0);
    let cx = center.x.clamp(area.left() + col_w + gap, area.right() - col_w - gap);
    let left_x = (cx - clear / 2.0 - gap - col_w).max(area.left() + gap);
    let right_x = (cx + clear / 2.0 + gap).min(area.right() - gap - col_w);
    let right_n = n.div_ceil(2);
    let left_n = n - right_n;
    let column = |x: f32, count: usize| -> Vec<Rect> {
        if count == 0 {
            return Vec::new();
        }
        let h = (area.height() - gap * (count as f32 + 1.0)) / count as f32;
        (0..count).map(|i| Rect::from_min_size(pos2(x, area.top() + gap + i as f32 * (h + gap)), vec2(col_w, h))).collect()
    };
    // Alternate right/left so the first windows land on both sides.
    let (right, left) = (column(right_x, right_n), column(left_x, left_n));
    let mut out = Vec::with_capacity(n);
    let (mut r, mut l) = (right.into_iter(), left.into_iter());
    for i in 0..n {
        let next = if i % 2 == 0 { r.next().or_else(|| l.next()) } else { l.next().or_else(|| r.next()) };
        if let Some(rect) = next {
            out.push(rect);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spring_glides_and_settles() {
        let mut s = SpringVec2::launch(pos2(0.0, 0.0), pos2(800.0, 300.0));
        let mut t = 0.0;
        while s.step(1.0 / 60.0) {
            t += 1.0 / 60.0;
            assert!(t < 3.0, "spring never settled");
        }
        assert_eq!(s.current, pos2(800.0, 300.0));
        // Fast: most of the way after a quarter second.
        let mut s = SpringVec2::launch(pos2(0.0, 0.0), pos2(1000.0, 0.0));
        for _ in 0..15 {
            s.step(1.0 / 60.0);
        }
        assert!(s.current.x > 600.0, "{}", s.current.x);
    }

    #[test]
    fn snaps_to_screen_and_neighbour_edges() {
        let screen = Rect::from_min_size(pos2(0.0, 0.0), vec2(2560.0, 1400.0));
        let beside = Rect::from_min_size(pos2(400.0, 100.0), vec2(400.0, 300.0));
        // 8 px from the screen's left edge and 5 px below the neighbour's top.
        let r = Rect::from_min_size(pos2(8.0, 105.0), vec2(300.0, 200.0));
        let (s, guides) = magnetic_snap(r, &[beside], &[screen], SNAP_THRESHOLD);
        assert_eq!(s.min, pos2(0.0, 100.0));
        assert_eq!(guides.len(), 2);
        let other = Rect::from_min_size(pos2(1000.0, 100.0), vec2(400.0, 300.0));
        // Right edge 10 px from the neighbour's left edge: they touch.
        let r = Rect::from_min_size(pos2(690.0, 400.0), vec2(300.0, 200.0));
        let (s, _) = magnetic_snap(r, &[other], &[screen], SNAP_THRESHOLD);
        assert_eq!(s.right(), 1000.0);
        // Too far: untouched.
        let r = Rect::from_min_size(pos2(600.0, 600.0), vec2(300.0, 200.0));
        assert_eq!(magnetic_snap(r, &[other], &[screen], SNAP_THRESHOLD).0, r);
    }

    #[cfg(windows)]
    #[test]
    fn ghost_mode_toggles_the_click_through_styles() {
        use windows::core::w;
        use windows::Win32::Foundation::{HINSTANCE, HWND};
        use windows::Win32::UI::WindowsAndMessaging::{
            CreateWindowExW, DestroyWindow, GetWindowLongPtrW, GWL_EXSTYLE, HMENU, WINDOW_EX_STYLE, WS_EX_LAYERED, WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP,
        };
        // SAFETY: a hidden window owned by this test thread, destroyed below.
        let hwnd = unsafe { CreateWindowExW(WINDOW_EX_STYLE(0), w!("STATIC"), w!("jai ghost test"), WS_POPUP, 0, 0, 120, 80, HWND(0), HMENU(0), HINSTANCE(0), None) };
        assert_ne!(hwnd.0, 0, "test window");
        // SAFETY: style query on the live test window.
        let ex = || unsafe { GetWindowLongPtrW(hwnd, GWL_EXSTYLE) } as u32;
        let click_through = WS_EX_LAYERED.0 | WS_EX_TRANSPARENT.0;
        // The style math, topmost included (Windows applies z-order lazily to hidden windows,
        // so the live check below covers the click-through bits).
        let on = ghost_ex_style(0, true) as u32;
        assert_eq!(on, click_through | WS_EX_TOPMOST.0);
        assert_eq!(ghost_ex_style(on as isize, false) as u32 & WS_EX_TRANSPARENT.0, 0);
        assert_ne!(ghost_ex_style(on as isize, false) as u32 & WS_EX_LAYERED.0, 0);
        // The real window.
        let result = std::panic::catch_unwind(|| {
            set_window_click_through(hwnd.0, true).unwrap();
            assert_eq!(ex() & click_through, click_through, "clicks pass through");
            set_window_click_through(hwnd.0, false).unwrap();
            assert_eq!(ex() & WS_EX_TRANSPARENT.0, 0, "clicks reach it again");
            set_topmost(hwnd.0, false).unwrap();
            assert_eq!(find_window("jai ghost test"), Some(hwnd.0));
        });
        // SAFETY: the window created above (destroyed even when an assertion failed).
        unsafe {
            let _ = DestroyWindow(hwnd);
        }
        if let Err(panic) = result {
            std::panic::resume_unwind(panic);
        }
    }

    #[test]
    fn spring_is_critically_close_and_never_overshoots_far() {
        let mut s = SpringVec2::launch(pos2(0.0, 0.0), pos2(1000.0, 0.0));
        let mut peak: f32 = 0.0;
        while s.step(1.0 / 60.0) {
            peak = peak.max(s.current.x);
        }
        // stiffness 180 / damping 22: a cinematic glide with at most a few percent of overshoot.
        assert!(peak < 1040.0, "overshoot {peak}");
        let mut fade = SpringF32 { target: 1.0, ..SpringF32::at(0.0) };
        while fade.step(1.0 / 60.0) {}
        assert_eq!(fade.current, 1.0);
    }

    #[test]
    fn fan_out_never_overlaps() {
        let area = Rect::from_min_size(pos2(0.0, 0.0), vec2(2560.0, 1400.0));
        for n in 1..=6 {
            let rects = fan_out_layout(n, area, area.center());
            assert_eq!(rects.len(), n);
            for (i, a) in rects.iter().enumerate() {
                assert!(area.contains_rect(*a), "{a:?}");
                for b in &rects[i + 1..] {
                    assert!(!a.intersects(*b), "{a:?} overlaps {b:?}");
                }
                assert!(!a.contains(area.center()), "the orb's centre stays clear");
            }
        }
    }
}
