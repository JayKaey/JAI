//! His own windows on X11: XWayland under KDE Plasma, where a program may place, raise and pin its
//! windows (Wayland lets no program do that). J.A.I.'s windows run on X11 wherever there is one
//! (`gui::on_x11`), so each of them is an X11 window here, its id the handle Windows' code
//! calls `hwnd`. The names follow `win32` and `win32_desktop`; other programs' windows are
//! KWin's business (`plasma`). Every call is a no-op or None without an X server.

use std::sync::OnceLock;
use x11rb::connection::Connection;
use x11rb::protocol::randr::ConnectionExt as _;
use x11rb::protocol::shape::{self, ConnectionExt as _};
use x11rb::protocol::xproto::{AtomEnum, ClientMessageEvent, ConfigureWindowAux, ConnectionExt as _, EventMask, KeyButMask, MapState, PropMode, StackMode};
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;

x11rb::atom_manager! {
    Atoms: AtomsCookie {
        UTF8_STRING,
        WM_TRANSIENT_FOR,
        _NET_ACTIVE_WINDOW,
        _NET_CLIENT_LIST_STACKING,
        _NET_CLOSE_WINDOW,
        _NET_RESTACK_WINDOW,
        _NET_WM_NAME,
        _NET_WM_PID,
        _NET_WM_STATE,
        _NET_WM_STATE_ABOVE,
        _NET_WM_STATE_BELOW,
        _NET_WM_STATE_FULLSCREEN,
        _NET_WM_STATE_HIDDEN,
        _NET_WM_STATE_MAXIMIZED_HORZ,
        _NET_WM_STATE_MAXIMIZED_VERT,
        _NET_WM_STATE_SKIP_TASKBAR,
        _NET_WM_STATE_STICKY,
        _NET_WM_WINDOW_OPACITY,
        _NET_WORKAREA,
    }
}

struct X {
    conn: RustConnection,
    root: u32,
    atoms: Atoms,
}

fn x() -> Option<&'static X> {
    static X11: OnceLock<Option<X>> = OnceLock::new();
    X11.get_or_init(|| {
        let (conn, screen) = RustConnection::connect(None).ok()?;
        let root = conn.setup().roots.get(screen)?.root;
        let atoms = Atoms::new(&conn).ok()?.reply().ok()?;
        Some(X { conn, root, atoms })
    })
    .as_ref()
}

/// Runs `f` on a thread of its own (the X calls block for a reply).
pub fn run_detached(f: impl FnOnce() + Send + 'static) {
    let _ = std::thread::Builder::new().name("jai-x11".into()).spawn(f);
}

fn window(hwnd: isize) -> u32 {
    hwnd as u32
}

fn property32(x: &X, win: u32, atom: u32) -> Vec<u32> {
    x.conn.get_property(false, win, atom, AtomEnum::ANY, 0, 1024).ok().and_then(|c| c.reply().ok()).and_then(|r| r.value32().map(Iterator::collect)).unwrap_or_default()
}

fn title_of(x: &X, win: u32) -> String {
    let utf8 = x.conn.get_property(false, win, x.atoms._NET_WM_NAME, x.atoms.UTF8_STRING, 0, 1024).ok().and_then(|c| c.reply().ok()).map(|r| r.value).unwrap_or_default();
    let bytes = if utf8.is_empty() { x.conn.get_property(false, win, AtomEnum::WM_NAME, AtomEnum::ANY, 0, 1024).ok().and_then(|c| c.reply().ok()).map(|r| r.value).unwrap_or_default() } else { utf8 };
    String::from_utf8_lossy(&bytes).into_owned()
}

/// left, top, right, bottom on the screen.
fn rect_of(x: &X, win: u32) -> Option<[i32; 4]> {
    let size = x.conn.get_geometry(win).ok()?.reply().ok()?;
    let at = x.conn.translate_coordinates(win, x.root, 0, 0).ok()?.reply().ok()?;
    let (l, t) = (i32::from(at.dst_x), i32::from(at.dst_y));
    Some([l, t, l + i32::from(size.width), t + i32::from(size.height)])
}

/// A message to the window manager about `win` (EWMH: `_NET_WM_STATE`, `_NET_ACTIVE_WINDOW` …).
fn tell_wm(x: &X, win: u32, kind: u32, data: [u32; 5]) {
    let event = ClientMessageEvent::new(32, win, kind, data);
    let _ = x.conn.send_event(false, x.root, EventMask::SUBSTRUCTURE_REDIRECT | EventMask::SUBSTRUCTURE_NOTIFY, event);
    let _ = x.conn.flush();
}

fn wm_state(hwnd: isize, state: u32, on: bool) {
    if let Some(x) = x() {
        // (1 add, 0 remove; 2: the request comes from a pager-like tool, which KWin obeys.)
        tell_wm(x, window(hwnd), x.atoms._NET_WM_STATE, [u32::from(on), state, 0, 2, 0]);
    }
}

// ------------------------------------------------------------------------- his windows ----

/// One of this process's own windows on the screen.
#[derive(Debug, Clone, PartialEq)]
pub struct OwnWindow {
    pub hwnd: isize,
    pub title: String,
    /// left, top, right, bottom.
    pub rect: [i32; 4],
}

/// This process's own shown windows that carry a title, from the top down.
pub fn own_windows() -> Vec<OwnWindow> {
    let Some(x) = x() else { return Vec::new() };
    let me = std::process::id();
    let mut list: Vec<OwnWindow> = property32(x, x.root, x.atoms._NET_CLIENT_LIST_STACKING)
        .into_iter()
        .rev()
        .filter(|w| property32(x, *w, x.atoms._NET_WM_PID).first() == Some(&me))
        .filter(|w| x.conn.get_window_attributes(*w).ok().and_then(|c| c.reply().ok()).is_some_and(|a| a.map_state == MapState::VIEWABLE))
        .filter_map(|w| Some(OwnWindow { hwnd: w as isize, title: title_of(x, w), rect: rect_of(x, w)? }))
        .collect();
    list.retain(|w| !w.title.is_empty());
    list
}

/// This process's window titled `title`.
pub fn own_window(title: &str) -> Option<isize> {
    let x = x()?;
    let me = std::process::id();
    property32(x, x.root, x.atoms._NET_CLIENT_LIST_STACKING).into_iter().find(|w| property32(x, *w, x.atoms._NET_WM_PID).first() == Some(&me) && title_of(x, *w) == title).map(|w| w as isize)
}

/// Makes his window titled `title` a window of `owner` (WM_TRANSIENT_FOR): KWin keeps it above
/// the owner, as Windows keeps an owned window. False while there is no such window.
pub fn own_window_titled(title: &str, owner: isize) -> bool {
    let (Some(x), Some(win)) = (x(), own_window(title)) else { return false };
    if win == owner {
        return false;
    }
    // (Owner 0: a window of no one's again.)
    let _ = if owner == 0 { x.conn.delete_property(window(win), x.atoms.WM_TRANSIENT_FOR).map(drop) } else { x.conn.change_property32(PropMode::REPLACE, window(win), x.atoms.WM_TRANSIENT_FOR, AtomEnum::WINDOW, &[window(owner)]).map(drop) };
    let _ = x.conn.flush();
    true
}

/// His window in front, if one of his is.
pub fn foreground_own() -> Option<isize> {
    let x = x()?;
    let active = *property32(x, x.root, x.atoms._NET_ACTIVE_WINDOW).first()?;
    (active != 0 && property32(x, active, x.atoms._NET_WM_PID).first() == Some(&std::process::id())).then_some(active as isize)
}

/// Moves and sizes a window to (x, y, w, h).
pub fn place(hwnd: isize, [l, t, w, h]: [i32; 4]) {
    if let Some(x) = x() {
        let _ = x.conn.configure_window(window(hwnd), &ConfigureWindowAux::new().x(l).y(t).width(w.max(1) as u32).height(h.max(1) as u32));
        let _ = x.conn.flush();
    }
}

/// Puts a window back at remembered bounds (x, y, w, h), unless they are off every monitor now.
pub fn restore_placement(hwnd: isize, [x, y, w, h]: [i32; 4]) {
    let (cx, cy) = (x + w / 2, y + h / 2);
    if w >= 200 && h >= 150 && displays().iter().any(|d| (d.rect[0]..d.rect[2]).contains(&cx) && (d.rect[1]..d.rect[3]).contains(&cy)) {
        place(hwnd, [x, y, w, h]);
    }
}

/// `place`, as `win32::set_window_rect` (the orb).
pub fn set_window_rect(hwnd: isize, rect: [i32; 4]) -> Result<(), String> {
    x().ok_or("no X11 display")?;
    place(hwnd, rect);
    Ok(())
}

/// Keeps a window above every other (or no longer).
pub fn topmost(hwnd: isize, on: bool) {
    if let Some(x) = x() {
        wm_state(hwnd, x.atoms._NET_WM_STATE_ABOVE, on);
    }
}

/// Keeps a window below every other (the desktop background), or no longer.
pub fn keep_below(hwnd: isize, on: bool) {
    if let Some(x) = x() {
        wm_state(hwnd, x.atoms._NET_WM_STATE_BELOW, on);
    }
}

/// Leaves a window off the task manager (or puts it back).
pub fn skip_taskbar(hwnd: isize, on: bool) {
    if let Some(x) = x() {
        wm_state(hwnd, x.atoms._NET_WM_STATE_SKIP_TASKBAR, on);
    }
}

/// Shows a window on every virtual desktop (or on one again).
pub fn sticky(hwnd: isize, on: bool) {
    if let Some(x) = x() {
        wm_state(hwnd, x.atoms._NET_WM_STATE_STICKY, on);
    }
}

fn restack(hwnd: isize, mode: StackMode) {
    if let Some(x) = x() {
        tell_wm(x, window(hwnd), x.atoms._NET_RESTACK_WINDOW, [2, 0, u32::from(mode), 0, 0]);
    }
}

/// Brings a window above the other programs' windows without the focus.
pub fn raise(hwnd: isize) {
    restack(hwnd, StackMode::ABOVE);
}

/// Puts a window behind every other one.
pub fn sink(hwnd: isize) {
    restack(hwnd, StackMode::BELOW);
}

/// How much of what is under a window shows through it (of 255: 255 nothing), as KWin draws it.
pub fn opacity(hwnd: isize, alpha: u8) {
    if let Some(x) = x() {
        let _ = x.conn.change_property32(PropMode::REPLACE, window(hwnd), x.atoms._NET_WM_WINDOW_OPACITY, AtomEnum::CARDINAL, &[u32::from(alpha) * 0x0101_0101]);
        let _ = x.conn.flush();
    }
}

/// Closes a window as its close key would.
pub fn close(hwnd: isize) {
    if let Some(x) = x() {
        tell_wm(x, window(hwnd), x.atoms._NET_CLOSE_WINDOW, [0, 2, 0, 0, 0]);
    }
}

/// Lets clicks pass through a window to what is under it (or catches them again).
pub fn click_through(hwnd: isize, on: bool) -> Result<(), String> {
    let x = x().ok_or("no X11 display")?;
    let done = if on {
        x.conn.shape_rectangles(shape::SO::SET, shape::SK::INPUT, x11rb::protocol::xproto::ClipOrdering::UNSORTED, window(hwnd), 0, 0, &[]).map(drop)
    } else {
        x.conn.shape_mask(shape::SO::SET, shape::SK::INPUT, window(hwnd), 0, 0, x11rb::NONE).map(drop)
    };
    done.and_then(|_| x.conn.flush()).map_err(|e| e.to_string())
}

/// The window is shown.
pub fn is_visible(hwnd: isize) -> bool {
    x().and_then(|x| x.conn.get_window_attributes(window(hwnd)).ok()?.reply().ok()).is_some_and(|a| a.map_state == MapState::VIEWABLE)
}

/// A window's bounds (left, top, width, height) and whether it is maximised, minimised.
pub fn window_state(hwnd: isize) -> Option<([i32; 4], bool, bool)> {
    let x = x()?;
    let [l, t, r, b] = rect_of(x, window(hwnd))?;
    let states = property32(x, window(hwnd), x.atoms._NET_WM_STATE);
    let maximised = states.contains(&x.atoms._NET_WM_STATE_MAXIMIZED_VERT) && states.contains(&x.atoms._NET_WM_STATE_MAXIMIZED_HORZ);
    Some(([l, t, r - l, b - t], maximised, states.contains(&x.atoms._NET_WM_STATE_HIDDEN)))
}

// ----------------------------------------------------------------------------- monitors ----

/// One monitor as X11 reports it (pixels, left/top/right/bottom), as `win32::DisplayInfo`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DisplayInfo {
    pub rect: [i32; 4],
    pub work: [i32; 4],
    pub primary: bool,
    /// The output's name ("DP-1").
    pub device: String,
}

/// Every monitor (RandR): the primary first, then left to right.
pub fn displays() -> Vec<DisplayInfo> {
    let Some(x) = x() else { return Vec::new() };
    let Some(reply) = x.conn.randr_get_monitors(x.root, true).ok().and_then(|c| c.reply().ok()) else { return Vec::new() };
    // (X11 has one work area for the whole desktop: each monitor's is its share of it.)
    let area = property32(x, x.root, x.atoms._NET_WORKAREA);
    let mut list: Vec<DisplayInfo> = reply
        .monitors
        .iter()
        .map(|m| {
            let rect = [i32::from(m.x), i32::from(m.y), i32::from(m.x) + i32::from(m.width), i32::from(m.y) + i32::from(m.height)];
            let device = x.conn.get_atom_name(m.name).ok().and_then(|c| c.reply().ok()).map(|n| String::from_utf8_lossy(&n.name).into_owned()).unwrap_or_default();
            DisplayInfo { rect, work: work_of(rect, &area), primary: m.primary, device }
        })
        .collect();
    list.sort_by_key(|d| (!d.primary, d.rect[0], d.rect[1]));
    list
}

/// The highest refresh rate (Hz) among the lit monitors (RandR: the mode each CRTC shows).
pub fn fastest_refresh_hz() -> Option<u32> {
    let x = x()?;
    let res = x.conn.randr_get_screen_resources_current(x.root).ok()?.reply().ok()?;
    res.crtcs
        .iter()
        .filter_map(|crtc| x.conn.randr_get_crtc_info(*crtc, res.config_timestamp).ok()?.reply().ok())
        .filter_map(|crtc| res.modes.iter().find(|m| crtc.mode != 0 && m.id == crtc.mode))
        .filter(|m| m.htotal > 0 && m.vtotal > 0)
        .map(|m| (f64::from(m.dot_clock) / (f64::from(m.htotal) * f64::from(m.vtotal))).round() as u32)
        .max()
}

/// A monitor's work area: its rect cut by the desktop's (`_NET_WORKAREA`: x, y, w, h), or the
/// whole of it when they do not meet.
fn work_of(rect: [i32; 4], area: &[u32]) -> [i32; 4] {
    let Some(&[x, y, w, h]) = area.get(..4) else { return rect };
    let (x, y) = (x as i32, y as i32);
    let work = [rect[0].max(x), rect[1].max(y), rect[2].min(x + w as i32), rect[3].min(y + h as i32)];
    if work[0] < work[2] && work[1] < work[3] {
        work
    } else {
        rect
    }
}

/// Work areas (left, top, width, height) of all monitors, the primary first.
pub fn monitor_work_areas() -> Vec<[i32; 4]> {
    displays().into_iter().map(|d| [d.work[0], d.work[1], d.work[2] - d.work[0], d.work[3] - d.work[1]]).collect()
}

/// The primary monitor: (width, height, scale), the scale from Xft.dpi as KDE sets it for X11
/// programs that scale themselves.
pub fn primary_screen() -> (i32, i32, f32) {
    let (w, h) = displays().first().map_or((1920, 1080), |d| (d.rect[2] - d.rect[0], d.rect[3] - d.rect[1]));
    let dpi = x().and_then(|x| x11rb::resource_manager::new_from_default(&x.conn).ok()).and_then(|db| db.get_string("Xft.dpi", "").and_then(|v| v.trim().parse::<f32>().ok()));
    (w, h, (dpi.unwrap_or(96.0) / 96.0).clamp(0.5, 4.0))
}

/// Where the pointer is on the screen and whether its left button is held, as XWayland knows
/// it (true over his own windows, which are X11's).
pub fn pointer() -> Option<(i32, i32, bool)> {
    let x = x()?;
    let p = x.conn.query_pointer(x.root).ok()?.reply().ok()?;
    Some((i32::from(p.root_x), i32::from(p.root_y), u16::from(p.mask) & u16::from(KeyButMask::BUTTON1) != 0))
}

/// The bounds (left, top, right, bottom) of the monitor the middle of a window is on.
pub fn window_display_rect(hwnd: isize) -> Option<[i32; 4]> {
    let [l, t, r, b] = rect_of(x()?, window(hwnd))?;
    let (cx, cy) = ((l + r) / 2, (t + b) / 2);
    let all = displays();
    all.iter().find(|d| (d.rect[0]..d.rect[2]).contains(&cx) && (d.rect[1]..d.rect[3]).contains(&cy)).or(all.first()).map(|d| d.rect)
}

/// The window covers its whole monitor.
pub fn covers_monitor(hwnd: isize) -> bool {
    let (Some(x), Some(m)) = (x(), window_display_rect(hwnd)) else { return false };
    rect_of(x, window(hwnd)).is_some_and(|r| r[0] <= m[0] && r[1] <= m[1] && r[2] >= m[2] && r[3] >= m[3])
}

/// Stretches a window over its whole monitor.
pub fn cover_monitor(hwnd: isize) -> Result<(), String> {
    let [l, t, r, b] = window_display_rect(hwnd).ok_or("could not read the window's monitor")?;
    set_window_rect(hwnd, [l, t, r - l, b - t])
}

/// Where a window goes on its monitor (`win32::place` and `win32::snap`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Spot {
    Left,
    Right,
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
    Center,
    /// Its size times this, round its middle.
    Scale(f32),
}

/// The bounds (x, y, w, h) a window of bounds `now` takes at `spot` on monitor `[l, t, r, b]`: a
/// half of it, a corner 16 px in, the middle, or another size; always inside the monitor.
pub fn spot_in(spot: Spot, [l, t, r, b]: [i32; 4], [x, y, ow, oh]: [i32; 4]) -> [i32; 4] {
    let (mw, mh, margin) = (r - l, b - t, 16);
    let (w, h) = match spot {
        Spot::Scale(f) => (((ow as f32 * f) as i32).clamp(320.min(mw), mw), ((oh as f32 * f) as i32).clamp(240.min(mh), mh)),
        Spot::Left | Spot::Right => (mw / 2, mh),
        _ => (ow.min(mw), oh.min(mh)),
    };
    match spot {
        Spot::Left => [l, t, w, h],
        Spot::Right => [r - w, t, w, h],
        Spot::TopLeft => [l + margin, t + margin, w, h],
        Spot::TopRight => [r - margin - w, t + margin, w, h],
        Spot::BottomLeft => [l + margin, b - margin - h, w, h],
        Spot::BottomRight => [r - margin - w, b - margin - h, w, h],
        Spot::Center => [l + (mw - w) / 2, t + (mh - h) / 2, w, h],
        Spot::Scale(_) => [(x + (ow - w) / 2).clamp(l, r - w), (y + (oh - h) / 2).clamp(t, b - h), w, h],
    }
}

/// Puts a window of his at `spot` on its monitor.
pub fn arrange(hwnd: isize, spot: Spot) -> Result<(), String> {
    let (now, ..) = window_state(hwnd).ok_or("the window is gone")?;
    let monitor = window_display_rect(hwnd).ok_or("could not read the window's monitor")?;
    set_window_rect(hwnd, spot_in(spot, monitor, now))
}

#[cfg(test)]
mod tests {
    use super::{spot_in, work_of, Spot};

    #[test]
    fn a_monitors_work_area_is_its_share_of_the_desktops() {
        // Two monitors side by side, a 44 px panel along the bottom of the desktop.
        let area = [0, 0, 3840, 1036];
        assert_eq!(work_of([0, 0, 1920, 1080], &area), [0, 0, 1920, 1036]);
        assert_eq!(work_of([1920, 0, 3840, 1080], &area), [1920, 0, 3840, 1036]);
        assert_eq!(work_of([0, 0, 1920, 1080], &[]), [0, 0, 1920, 1080], "no work area told: the whole monitor");
        assert_eq!(work_of([4000, 0, 5920, 1080], &area), [4000, 0, 5920, 1080], "off the work area: the whole monitor");
    }

    #[test]
    fn a_window_goes_to_a_half_a_corner_the_middle_or_another_size_inside_its_monitor() {
        let monitor = [1920, 0, 3840, 1080];
        let window = [2000, 100, 800, 600];
        assert_eq!(spot_in(Spot::Left, monitor, window), [1920, 0, 960, 1080]);
        assert_eq!(spot_in(Spot::Right, monitor, window), [2880, 0, 960, 1080]);
        assert_eq!(spot_in(Spot::TopRight, monitor, window), [3840 - 16 - 800, 16, 800, 600]);
        assert_eq!(spot_in(Spot::BottomLeft, monitor, window), [1936, 1080 - 16 - 600, 800, 600]);
        assert_eq!(spot_in(Spot::Center, monitor, window), [1920 + 560, 240, 800, 600]);
        assert_eq!(spot_in(Spot::Scale(1.2), monitor, window), [1920, 40, 960, 720], "grown round its middle");
        let big = spot_in(Spot::Scale(10.0), monitor, window);
        assert_eq!(big[2..], [1920, 1080], "never larger than the monitor");
        let small = spot_in(Spot::Scale(0.01), monitor, window);
        assert_eq!(small[2..], [320, 240], "never smaller than 320 × 240");
        for spot in [Spot::Left, Spot::Right, Spot::TopLeft, Spot::TopRight, Spot::BottomLeft, Spot::BottomRight, Spot::Center, Spot::Scale(1.2)] {
            let [x, y, w, h] = spot_in(spot, monitor, window);
            assert!(x >= 1920 && y >= 0 && x + w <= 3840 && y + h <= 1080, "{spot:?} stays on its monitor");
        }
    }

    #[test]
    fn without_an_x_server_every_call_is_harmless() {
        // (The tests run where there may be no X server: then nothing is found and nothing fails.)
        if std::env::var_os("DISPLAY").is_some() {
            return;
        }
        assert!(super::own_windows().is_empty());
        assert_eq!(super::own_window("J.A.I."), None);
        assert!(super::displays().is_empty());
        assert_eq!(super::primary_screen().2, 1.0);
        assert!(super::click_through(1, true).is_err());
    }
}
