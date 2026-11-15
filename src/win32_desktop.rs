//! The Windows side of J.A.I. as the desktop's background (`wallpaper`): taking the board's window
//! back out of the desktop, telling the bare desktop from a window on top of it, how much of a
//! spot other windows cover, the mouse hook that passes the pointer on to a window the desktop
//! lets no click through to, his two global keys, and his own windows as Windows lists them.

use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU32, Ordering};
use std::sync::Mutex;
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{BOOL, HWND, LPARAM, LRESULT, POINT, RECT, TRUE, WPARAM};
use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, EnumWindows, FindWindowExW, FindWindowW, GetClassNameW, GetCursorPos, GetForegroundWindow, GetMessageW, GetWindowLongPtrW, GetWindowRect, GetWindowThreadProcessId, InternalGetWindowText,
    IsIconic, IsWindow, IsWindowVisible, PostThreadMessageW, SetParent, SetWindowLongPtrW, SetWindowPos, SetWindowsHookExW, UnhookWindowsHookEx, WindowFromPoint, GWL_EXSTYLE, HHOOK, HWND_NOTOPMOST, HWND_TOPMOST, MSG,
    MSLLHOOKSTRUCT, SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER, SWP_SHOWWINDOW, WH_MOUSE_LL, WM_APP, WM_HOTKEY, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MBUTTONDOWN, WM_MBUTTONUP, WM_MOUSEMOVE,
    WM_MOUSEWHEEL, WM_RBUTTONDOWN, WM_RBUTTONUP, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TRANSPARENT,
};

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn class_of(hwnd: HWND) -> String {
    let mut buffer = [0u16; 64];
    // SAFETY: a plain query into a buffer of the stated length.
    let n = unsafe { GetClassNameW(hwnd, &mut buffer) }.max(0) as usize;
    String::from_utf16_lossy(&buffer[..n.min(buffer.len())])
}

fn title_of(hwnd: HWND) -> String {
    let mut buffer = [0u16; 256];
    // SAFETY: a plain query into a buffer of the stated length (it never sends a message).
    let n = unsafe { InternalGetWindowText(hwnd, &mut buffer) }.max(0) as usize;
    String::from_utf16_lossy(&buffer[..n.min(buffer.len())])
}

fn rect_of(hwnd: HWND) -> Option<[i32; 4]> {
    let mut r = RECT::default();
    // SAFETY: a plain query on a window handle.
    unsafe { GetWindowRect(hwnd, &mut r) }.ok()?;
    Some([r.left, r.top, r.right, r.bottom])
}

// ------------------------------------------------------------------- into and out of it ----

/// The window's extended style before the desktop took it (`before_attach`), to give it back.
static STYLE_BEFORE: AtomicIsize = AtomicIsize::new(0);

/// Remembers how window `hwnd` is styled, right before `win32::attach_behind_desktop_icons`
/// makes it a layered, click-through child of the desktop.
pub fn before_attach(hwnd: isize) {
    // SAFETY: a plain query on our own window.
    STYLE_BEFORE.store(unsafe { GetWindowLongPtrW(HWND(hwnd), GWL_EXSTYLE) }, Ordering::Release);
}

/// Takes window `hwnd` back out of the desktop: a top-level window of its own again at
/// `[x, y, w, h]`, styled as it was before, shown without the focus. Synchronous: on the Win32
/// worker.
pub fn detach(hwnd: isize, [x, y, w, h]: [i32; 4]) -> Result<(), String> {
    // SAFETY: re-parenting and re-styling our own window; every failure is reported.
    unsafe {
        if !IsWindow(HWND(hwnd)).as_bool() {
            return Err("the window is gone".to_string());
        }
        let _ = SetParent(HWND(hwnd), HWND(0));
        let now = GetWindowLongPtrW(HWND(hwnd), GWL_EXSTYLE);
        let ours = (WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_NOACTIVATE).0 as isize;
        let before = STYLE_BEFORE.load(Ordering::Acquire);
        SetWindowLongPtrW(HWND(hwnd), GWL_EXSTYLE, (now & !ours) | (before & ours));
        SetWindowPos(HWND(hwnd), HWND_NOTOPMOST, x, y, w, h, SWP_NOACTIVATE | SWP_SHOWWINDOW | SWP_FRAMECHANGED).map_err(|e| e.message().to_string())
    }
}

/// Whether window `hwnd` still hangs in the desktop, whether it is still the layered,
/// click-through window the desktop needs (the window library writes a window's styles anew
/// whenever it changes one of them), and where it stands on the screen (left, top, right,
/// bottom). None: the window is gone (the desktop took it down with it).
pub fn standing(hwnd: isize) -> Option<(bool, bool, [i32; 4])> {
    use windows::Win32::UI::WindowsAndMessaging::{GetAncestor, GA_PARENT};
    // SAFETY: plain queries on handles.
    unsafe {
        if !IsWindow(HWND(hwnd)).as_bool() {
            return None;
        }
        let parent = GetAncestor(HWND(hwnd), GA_PARENT);
        let class = class_of(parent);
        let needed = (WS_EX_LAYERED | WS_EX_TRANSPARENT).0 as isize;
        let styled = GetWindowLongPtrW(HWND(hwnd), GWL_EXSTYLE) & needed == needed && IsWindowVisible(HWND(hwnd)).as_bool();
        Some((class == "Progman" || class == "WorkerW", styled, rect_of(HWND(hwnd))?))
    }
}

/// Whether the desktop's own window exists right now (it is gone for a moment while Explorer
/// starts again).
pub fn desktop_exists() -> bool {
    // SAFETY: a lookup by class name.
    unsafe { FindWindowW(w!("Progman"), PCWSTR::null()).0 != 0 }
}

// ------------------------------------------------------------------- what is the desktop ----

/// The desktop's windows a pointer can be over: Progman, the icons' holder, the icons' list
/// and the window that holds them when it is a WorkerW. Looked up by `learn_desktop`.
static DESKTOP: [AtomicIsize; 4] = [AtomicIsize::new(0), AtomicIsize::new(0), AtomicIsize::new(0), AtomicIsize::new(0)];

/// Looks the desktop's windows up afresh (on the Win32 worker, every few seconds: Explorer may
/// have made new ones).
pub fn learn_desktop() {
    unsafe extern "system" fn holder(top: HWND, lparam: LPARAM) -> BOOL {
        // SAFETY: lparam is the &mut (HWND, HWND) passed below and outlives EnumWindows.
        unsafe {
            let view = FindWindowExW(top, HWND(0), w!("SHELLDLL_DefView"), PCWSTR::null());
            if view.0 != 0 {
                *(lparam.0 as *mut (HWND, HWND)) = (top, view);
                return BOOL(0);
            }
        }
        TRUE
    }
    // SAFETY: lookups by class name; the callback only touches `found` through its pointer.
    unsafe {
        let progman = FindWindowW(w!("Progman"), PCWSTR::null());
        let mut found = (HWND(0), HWND(0));
        let _ = EnumWindows(Some(holder), LPARAM(&mut found as *mut (HWND, HWND) as isize));
        let list = if found.1 .0 != 0 { FindWindowExW(found.1, HWND(0), w!("SysListView32"), PCWSTR::null()) } else { HWND(0) };
        for (slot, hwnd) in DESKTOP.iter().zip([progman, found.0, found.1, list]) {
            slot.store(hwnd.0, Ordering::Release);
        }
    }
}

/// Whether the window under the screen point is the desktop itself: its icons or its bare
/// background, not a window that lies on it. Cheap: called from the mouse hook.
pub fn desktop_at(x: i32, y: i32) -> bool {
    // SAFETY: a plain query.
    let hwnd = unsafe { WindowFromPoint(POINT { x, y }) }.0;
    hwnd != 0 && DESKTOP.iter().any(|d| d.load(Ordering::Acquire) == hwnd)
}

/// Where the pointer is on the screen.
pub fn cursor() -> Option<(i32, i32)> {
    let mut p = POINT::default();
    // SAFETY: a plain query into a struct of ours.
    unsafe { GetCursorPos(&mut p) }.ok()?;
    Some((p.x, p.y))
}

/// The classes of the shell's own windows: the desktop and the taskbars. They cover nothing.
fn shell_class(class: &str) -> bool {
    matches!(class, "Progman" | "WorkerW" | "Shell_TrayWnd" | "Shell_SecondaryTrayWnd")
}

/// A window that lies there and covers nothing, by its extended style: one that lets clicks
/// through, or a layered one that never takes the focus (a graphics driver's or a game
/// launcher's overlay over a whole monitor, see-through until it has something to show).
fn overlay(ex: isize) -> bool {
    let has = |bit: u32| ex & bit as isize != 0;
    has(WS_EX_TRANSPARENT.0) || (has(WS_EX_LAYERED.0) && has(WS_EX_NOACTIVATE.0))
}

/// Whether the left mouse button is held down right now.
pub fn left_button_down() -> bool {
    use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_LBUTTON};
    // SAFETY: a plain query of a key's state.
    unsafe { GetAsyncKeyState(VK_LBUTTON.0 as i32) as u16 & 0x8000 != 0 }
}

struct Seen {
    own: u32,
    windows: Vec<(HWND, [i32; 4], bool)>,
}

/// The visible, uncloaked, unminimised top-level windows from the top down, each with its
/// rect and whether it is one of this process's own. The shell's and those that let clicks
/// through (overlays) are left out.
fn top_level_windows() -> Vec<(HWND, [i32; 4], bool)> {
    unsafe extern "system" fn each(hwnd: HWND, lparam: LPARAM) -> BOOL {
        // SAFETY: lparam is the &mut Seen passed below and outlives EnumWindows; plain queries.
        unsafe {
            let seen = &mut *(lparam.0 as *mut Seen);
            if !IsWindowVisible(hwnd).as_bool() || IsIconic(hwnd).as_bool() {
                return TRUE;
            }
            let mut cloaked = 0u32;
            let _ = DwmGetWindowAttribute(hwnd, DWMWA_CLOAKED, &mut cloaked as *mut u32 as *mut _, 4);
            let style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
            if cloaked != 0 || overlay(style) || shell_class(&class_of(hwnd)) {
                return TRUE;
            }
            let Some(rect) = rect_of(hwnd) else { return TRUE };
            if rect[2] - rect[0] < 8 || rect[3] - rect[1] < 8 {
                return TRUE;
            }
            let mut pid = 0u32;
            GetWindowThreadProcessId(hwnd, Some(&mut pid));
            seen.windows.push((hwnd, rect, pid == seen.own));
        }
        TRUE
    }
    let mut seen = Seen { own: std::process::id(), windows: Vec::new() };
    // SAFETY: the callback only touches `seen` through the provided pointer.
    unsafe {
        let _ = EnumWindows(Some(each), LPARAM(&mut seen as *mut Seen as isize));
    }
    seen.windows
}

fn overlap([l, t, r, b]: [i32; 4], [ol, ot, or, ob]: [i32; 4]) -> i64 {
    let (w, h) = ((r.min(or) - l.max(ol)).max(0) as i64, (b.min(ob) - t.max(ot)).max(0) as i64);
    w * h
}

/// The share (0 to 1) of the screen rect `[left, top, right, bottom]` that the one window
/// covering most of it covers: another program's, or one of his own (`covers`).
pub fn covered_share(rect: [i32; 4]) -> f32 {
    let area = ((rect[2] - rect[0]).max(1) as i64) * ((rect[3] - rect[1]).max(1) as i64);
    let most = top_level_windows().into_iter().filter(|(hwnd, _, own)| covers(*own, &title_of(*hwnd))).map(|(_, r, _)| overlap(rect, r)).max().unwrap_or(0);
    most as f32 / area as f32
}

/// Whether a window counts as lying on the emblem: every other program's, and of his own
/// those that carry a name (a block, a card, the console), never the corner orb itself. (Sir,
/// 2026-11-14, a window of his over the emblem: "something is blocking the orb and the orb is
/// still in the middle". Until then his own windows covered nothing.)
fn covers(own: bool, title: &str) -> bool {
    !own || !(title.is_empty() || title == ORB_TITLE)
}

/// The corner orb's window, by its title.
pub const ORB_TITLE: &str = "J.A.I. // orb";

/// The window in front, when it is one of his own.
pub fn foreground_own() -> Option<isize> {
    // SAFETY: plain queries on the foreground window's handle.
    unsafe {
        let hwnd = GetForegroundWindow();
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        (hwnd.0 != 0 && pid == std::process::id()).then_some(hwnd.0)
    }
}

/// Window `hwnd` stays above other programs' windows from now on, or no longer: where it
/// stands, without the focus. Nothing is touched when it stands that way already.
pub fn topmost(hwnd: isize, on: bool) {
    use windows::Win32::UI::WindowsAndMessaging::WS_EX_TOPMOST;
    // SAFETY: a query and a z-order change on a window handle; a stale one just fails.
    unsafe {
        if (GetWindowLongPtrW(HWND(hwnd), GWL_EXSTYLE) & WS_EX_TOPMOST.0 as isize != 0) != on {
            let _ = SetWindowPos(HWND(hwnd), if on { HWND_TOPMOST } else { HWND_NOTOPMOST }, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
        }
    }
}

/// Tells window `hwnd` to paint now, by a message sent to it: that waits for no turn among
/// its thread's windows, as a paint asked for in the usual way does. From any thread.
pub fn paint_now(hwnd: isize) {
    use windows::Win32::Graphics::Gdi::{RedrawWindow, RDW_INTERNALPAINT, RDW_UPDATENOW};
    // SAFETY: a paint request for a window handle; a stale one just fails.
    unsafe {
        let _ = RedrawWindow(HWND(hwnd), None, None, RDW_INTERNALPAINT | RDW_UPDATENOW);
    }
}

/// The window in front, when it is another program's: its rect. None for the desktop, the
/// taskbar and his own windows (then the pointer says where Sir is).
pub fn foreground_rect() -> Option<[i32; 4]> {
    // SAFETY: plain queries on the foreground window's handle.
    unsafe {
        let hwnd = GetForegroundWindow();
        if hwnd.0 == 0 || shell_class(&class_of(hwnd)) {
            return None;
        }
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if pid == std::process::id() {
            return None;
        }
        rect_of(hwnd)
    }
}

// ------------------------------------------------------------------------ his own windows ----

/// One of this process's own windows on the screen.
#[derive(Debug, Clone, PartialEq)]
pub struct OwnWindow {
    pub hwnd: isize,
    pub title: String,
    /// left, top, right, bottom.
    pub rect: [i32; 4],
}

/// This process's own visible windows that carry a title, from the top down (the board, his
/// views in windows of their own). Tool windows (the tray's, the subtitles' bar) are left out.
pub fn own_windows() -> Vec<OwnWindow> {
    top_level_windows()
        .into_iter()
        .filter(|(_, _, own)| *own)
        // SAFETY: a plain query on a handle from the enumeration.
        .filter(|(hwnd, _, _)| unsafe { GetWindowLongPtrW(*hwnd, GWL_EXSTYLE) } & WS_EX_TOOLWINDOW.0 as isize == 0)
        .map(|(hwnd, rect, _)| OwnWindow { hwnd: hwnd.0, title: title_of(hwnd), rect })
        .filter(|w| !w.title.is_empty())
        .collect()
}

/// Puts window `hwnd` behind every other window (in front of the desktop only), where it
/// stands and without the focus.
pub fn sink(hwnd: isize) {
    use windows::Win32::UI::WindowsAndMessaging::HWND_BOTTOM;
    // SAFETY: a z-order change on a window handle; a stale one just fails.
    unsafe {
        let _ = SetWindowPos(HWND(hwnd), HWND_BOTTOM, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
    }
}

/// Brings window `hwnd` above every other program's window without giving it the focus.
pub fn raise(hwnd: isize) {
    // SAFETY: z-order changes on a window handle; a stale one just fails.
    unsafe {
        let flags = SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE;
        let _ = SetWindowPos(HWND(hwnd), HWND_TOPMOST, 0, 0, 0, 0, flags);
        let _ = SetWindowPos(HWND(hwnd), HWND_NOTOPMOST, 0, 0, 0, 0, flags);
    }
}

/// What the window animations on Sir's PC (the Windhawk mod "Windows Animations": it hooks
/// `DestroyWindow` and `ShowWindow` in every program and plays a snapshot of a closing or
/// hidden window) mark a window with whose close they must not play: they play none for it.
/// A snapshot of a window of his is a flat pane, dark or pale (his windows draw with OpenGL):
/// seen from outside 2026-11-15, a "Static" window from a thread started in the mod's DLL over
/// every window of his that closed, the orb's landing too. His "it turned black" of 2026-11-14.
const OWN_EXIT: PCWSTR = w!("windows-animations.CloseBypass");

/// A window of his closes and hides with his own exit only, never with another program's
/// animation of a snapshot of it (`OWN_EXIT`).
pub fn own_exit(hwnd: isize) {
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::UI::WindowsAndMessaging::SetPropW;
    // SAFETY: a property of a window handle; a stale one just fails.
    unsafe {
        let _ = SetPropW(HWND(hwnd), OWN_EXIT, HANDLE(1));
    }
}

/// Looks after the corner orb's window, past the windowing library (which writes a window's
/// whole style anew, its layer with it, whenever one of its own flags changes; so it is told
/// of nothing here): the window's pure black is see-through and lets clicks through (`pass`:
/// all of it does, beside a game), once it has `drawn` it is shown, without the focus, and it
/// goes with no other program's animation of it (`own_exit`).
/// (The see-through surface the library asks for is not shown on Sir's machine. Measured
/// 2026-11-14 from outside: an empty window of the orb's stood solid black, with either
/// surface format, with a window region and without. A black that is keyed out is seen
/// through, and the emblem goes on being drawn in it.)
pub fn tend_orb(hwnd: isize, pass: bool, drawn: bool) {
    use windows::Win32::Foundation::COLORREF;
    use windows::Win32::UI::WindowsAndMessaging::{GetLayeredWindowAttributes, SetLayeredWindowAttributes, LAYERED_WINDOW_ATTRIBUTES_FLAGS, LWA_COLORKEY};
    // SAFETY: the style and the layer attributes of a window handle; a stale one just fails.
    unsafe {
        if !IsWindow(HWND(hwnd)).as_bool() {
            return;
        }
        own_exit(hwnd);
        let ex = GetWindowLongPtrW(HWND(hwnd), GWL_EXSTYLE);
        let wanted = orb_style(ex, pass);
        if wanted != ex {
            SetWindowLongPtrW(HWND(hwnd), GWL_EXSTYLE, wanted);
        }
        let mut flags = LAYERED_WINDOW_ATTRIBUTES_FLAGS(0);
        if GetLayeredWindowAttributes(HWND(hwnd), None, None, Some(&mut flags)).is_err() || flags.0 & LWA_COLORKEY.0 == 0 {
            let _ = SetLayeredWindowAttributes(HWND(hwnd), COLORREF(0), 0, LWA_COLORKEY);
        }
        let on_show = IsWindowVisible(HWND(hwnd)).as_bool();
        ORB_ON_SHOW.store(on_show, Ordering::Release);
        if drawn && !on_show {
            crate::win32::show_no_activate(hwnd);
        }
    }
}

/// Whether the corner orb's window was on show when it was last looked after.
static ORB_ON_SHOW: AtomicBool = AtomicBool::new(false);

/// Whether the corner orb's window is on show (as `tend_orb` last saw it).
pub fn orb_on_show() -> bool {
    ORB_ON_SHOW.load(Ordering::Acquire)
}

/// The corner orb sets out anew: its window of before is gone.
pub fn orb_sets_out() {
    ORB_ON_SHOW.store(false, Ordering::Release);
}

/// The corner orb's extended style out of the one it has: layered always, click-through as a
/// whole only while `pass`.
fn orb_style(ex: isize, pass: bool) -> isize {
    let through = WS_EX_TRANSPARENT.0 as isize;
    (ex & !through) | WS_EX_LAYERED.0 as isize | if pass { through } else { 0 }
}

/// Asks window `hwnd` to close, as its own close key would (it plays its exit and goes).
pub fn close(hwnd: isize) {
    use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_CLOSE};
    // SAFETY: posting a message to a window handle; a stale one just fails.
    unsafe {
        let _ = PostMessageW(HWND(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
    }
}

/// Puts window `hwnd` at `[x, y, w, h]` on the screen, as it is in the order of the windows and
/// without the focus (one step of a glide).
pub fn place(hwnd: isize, [x, y, w, h]: [i32; 4]) {
    // SAFETY: a move of a window handle; a stale one just fails.
    unsafe {
        let _ = SetWindowPos(HWND(hwnd), HWND(0), x, y, w.max(1), h.max(1), SWP_NOZORDER | SWP_NOACTIVATE);
    }
}

// ------------------------------------------------------------- the mouse hook and his keys ----

use crate::wallpaper::{Mouse, MouseKind};

/// The thread with the message loop that the hook and the keys live on (0: not started).
static THREAD: AtomicU32 = AtomicU32::new(0);
static HOOKED: AtomicBool = AtomicBool::new(false);
/// What is done with the mouse (true: the event is his alone and goes no further) and with
/// one of his keys (its number).
static HANDLERS: Mutex<Option<(fn(Mouse) -> bool, fn(usize))>> = Mutex::new(None);
const HOOK_ON: u32 = WM_APP + 71;
const HOOK_OFF: u32 = WM_APP + 72;

unsafe extern "system" fn mouse_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code >= 0 {
        // SAFETY: for WH_MOUSE_LL with a code of 0 or more, lparam points at an MSLLHOOKSTRUCT.
        let info = unsafe { &*(lparam.0 as *const MSLLHOOKSTRUCT) };
        let kind = match wparam.0 as u32 {
            WM_MOUSEMOVE => Some(MouseKind::Move),
            WM_LBUTTONDOWN => Some(MouseKind::Down(0)),
            WM_LBUTTONUP => Some(MouseKind::Up(0)),
            WM_RBUTTONDOWN => Some(MouseKind::Down(1)),
            WM_RBUTTONUP => Some(MouseKind::Up(1)),
            WM_MBUTTONDOWN => Some(MouseKind::Down(2)),
            WM_MBUTTONUP => Some(MouseKind::Up(2)),
            WM_MOUSEWHEEL => Some(MouseKind::Wheel(((info.mouseData >> 16) as i16) as f32 / 120.0)),
            _ => None,
        };
        // (Never waited for: a hook that is slow is taken away by Windows.)
        let handler = HANDLERS.try_lock().ok().and_then(|h| h.map(|(mouse, _)| mouse));
        if let (Some(kind), Some(handler)) = (kind, handler) {
            if handler(Mouse { kind, x: info.pt.x, y: info.pt.y }) {
                return LRESULT(1);
            }
        }
    }
    // SAFETY: passing the event on, as every hook must.
    unsafe { CallNextHookEx(HHOOK(0), code, wparam, lparam) }
}

/// Starts the thread his global keys and the mouse hook live on (once). `keys`: (modifiers,
/// virtual key) of each key; `on_key` is told its number when Sir presses it; `on_mouse` gets
/// every move and button of the mouse while the hook is on (`mouse_hook`) and says whether the
/// event is his alone.
pub fn start(keys: &'static [(u32, u32)], on_mouse: fn(Mouse) -> bool, on_key: fn(usize)) {
    use windows::Win32::System::Threading::GetCurrentThreadId;
    use windows::Win32::UI::Input::KeyboardAndMouse::{RegisterHotKey, HOT_KEY_MODIFIERS, MOD_NOREPEAT};
    if THREAD.load(Ordering::Acquire) != 0 {
        return;
    }
    *lock(&HANDLERS) = Some((on_mouse, on_key));
    let _ = std::thread::Builder::new().name("jai-desk".into()).spawn(move || {
        // SAFETY: a message loop on this thread: its hotkeys, and a hook installed and taken
        // away on this same thread.
        unsafe {
            THREAD.store(GetCurrentThreadId(), Ordering::Release);
            for (i, (mods, vk)) in keys.iter().enumerate() {
                if let Err(e) = RegisterHotKey(HWND(0), 0x4B00 + i as i32, HOT_KEY_MODIFIERS(*mods) | MOD_NOREPEAT, *vk) {
                    tracing::warn!("[DESKTOP] One of my keys is taken by another program ({})", e.message());
                }
            }
            let mut hook: Option<HHOOK> = None;
            let mut msg = MSG::default();
            while GetMessageW(&mut msg, HWND(0), 0, 0).0 > 0 {
                match msg.message {
                    WM_HOTKEY => {
                        let handler = lock(&HANDLERS).map(|(_, key)| key);
                        if let Some(handler) = handler {
                            handler((msg.wParam.0 as i32 - 0x4B00).max(0) as usize);
                        }
                    }
                    HOOK_ON if hook.is_none() => match SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_proc), None, 0) {
                        Ok(h) => {
                            hook = Some(h);
                            HOOKED.store(true, Ordering::Release);
                        }
                        Err(e) => tracing::warn!("[DESKTOP] The mouse could not be followed over the desktop: {}", e.message()),
                    },
                    HOOK_OFF => {
                        if let Some(h) = hook.take() {
                            let _ = UnhookWindowsHookEx(h);
                        }
                        HOOKED.store(false, Ordering::Release);
                    }
                    _ => {}
                }
            }
            if let Some(h) = hook.take() {
                let _ = UnhookWindowsHookEx(h);
            }
        }
    });
}

/// Follows the mouse (`true`) or lets it be (`false`). The hook costs every mouse event of the
/// whole desktop a visit here: it is on only while he is the background and no game runs.
pub fn mouse_hook(on: bool) {
    let thread = THREAD.load(Ordering::Acquire);
    if thread == 0 || HOOKED.load(Ordering::Acquire) == on {
        return;
    }
    // SAFETY: posting a message to our own thread's queue.
    unsafe {
        let _ = PostThreadMessageW(thread, if on { HOOK_ON } else { HOOK_OFF }, WPARAM(0), LPARAM(0));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_window_covers_the_share_of_a_spot_it_lies_on() {
        let spot = [100, 100, 300, 300];
        assert_eq!(overlap(spot, [0, 0, 200, 200]), 100 * 100);
        assert_eq!(overlap(spot, [300, 100, 500, 300]), 0, "beside it");
        assert_eq!(overlap(spot, [0, 0, 1000, 1000]), 200 * 200, "all of it");
    }

    #[test]
    fn the_shell_covers_nothing() {
        assert!(shell_class("Progman") && shell_class("Shell_TrayWnd") && shell_class("WorkerW"));
        assert!(!shell_class("Chrome_WidgetWin_1"));
    }

    /// 2026-11-14, Sir's main display with nothing open on it: the graphics driver's overlay lay
    /// over all of it (layered, never active, a tool window, extended style 0x08080080), counted
    /// as a window covering the emblem, and the orb came out to the corner whenever he spoke.
    #[test]
    fn an_overlay_nobody_sees_covers_nothing() {
        assert!(overlay(0x0808_0080), "layered and never active: an overlay");
        assert!(overlay(WS_EX_TRANSPARENT.0 as isize), "lets clicks through");
        // (A browser, two Electron programs, a player: as they stood beside it that day.)
        for ex in [0x0000_0000, 0x0028_0100, 0x0020_0100, 0x0024_0100] {
            assert!(!overlay(ex), "{ex:#010x} is a window that covers what is under it");
        }
        // (Layered alone is a window like any other; never active alone is a palette.)
        assert!(!overlay(WS_EX_LAYERED.0 as isize) && !overlay(WS_EX_NOACTIVATE.0 as isize));
    }

    /// Any window over the emblem hides it, his own too; only the corner orb itself and his
    /// windows without a name (the tray's, a tool tip) hide nothing.
    #[test]
    fn his_own_windows_hide_the_emblem_as_any_other_does() {
        assert!(covers(false, "") && covers(false, "A film"), "another program's, named or not");
        assert!(covers(true, "Changes \u{2014} J.A.I.") && covers(true, "Projectile \u{2014} J.A.I. Simulation Lab"));
        assert!(!covers(true, ORB_TITLE), "the orb does not hide what it left");
        assert!(!covers(true, ""));
    }

    /// The corner orb's window is a layered one whatever else it is, and lets every click
    /// through only beside a game.
    #[test]
    fn the_orbs_window_is_layered_and_passes_clicks_only_beside_a_game() {
        let (layered, through, topmost) = (WS_EX_LAYERED.0 as isize, WS_EX_TRANSPARENT.0 as isize, 0x8);
        // (As the windowing library makes it: on top, no layer.)
        let made = 0x0004_0118;
        assert_eq!(orb_style(made, false), made | layered);
        assert_eq!(orb_style(made, true), made | layered | through);
        assert_eq!(orb_style(made | layered | through, false), made | layered, "the game is gone: it takes clicks again");
        assert_eq!(orb_style(orb_style(made, false), false), orb_style(made, false), "tended again, nothing changes");
        assert!(orb_style(made, true) & topmost != 0, "what else it is stays");
    }

    /// A window of his carries the mark the window animations on Sir's PC leave alone: it goes
    /// with its own exit, not as a snapshot of it played by another program.
    #[test]
    fn a_window_of_his_goes_with_its_own_exit() {
        use windows::Win32::Foundation::HINSTANCE;
        use windows::Win32::UI::WindowsAndMessaging::{CreateWindowExW, DestroyWindow, GetPropW, HMENU, WINDOW_EX_STYLE, WS_POPUP};
        // SAFETY: a hidden window owned by this test thread, destroyed below; plain queries on it.
        unsafe {
            let hwnd = CreateWindowExW(WINDOW_EX_STYLE(0), w!("STATIC"), w!("jai exit test"), WS_POPUP, 0, 0, 120, 80, HWND(0), HMENU(0), HINSTANCE(0), None);
            assert_ne!(hwnd.0, 0, "test window");
            assert_eq!(GetPropW(hwnd, OWN_EXIT).0, 0, "unmarked as it comes");
            own_exit(hwnd.0);
            assert_eq!(GetPropW(hwnd, OWN_EXIT).0, 1, "the mark the animations look for");
            let _ = DestroyWindow(hwnd);
        }
    }

    /// His windows as Windows has them, watched from outside for a while (`JAI_WATCH=seconds
    /// cargo test --bin jai his_windows_from_outside -- --ignored --nocapture`): every window
    /// of a process called "jai", top-level or inside the desktop, each time it changes (its
    /// title, rect, whether it is shown, its extended style, its owner); a window that moves
    /// more than three times in a second is counted, not listed. With
    /// `JAI_WATCH_THROUGH=left,top,right,bottom` (several: `;` between them) it also says,
    /// whenever that changes, how much of each square spot of the screen still shows what it
    /// showed when the watch began, and how bright its middle and its corners are: a
    /// see-through window that came to stand there leaves it as it was. (The pixels are
    /// compared and dropped; no picture of Sir's screen is kept.)
    #[test]
    #[ignore]
    fn his_windows_from_outside() {
        use std::collections::HashMap;
        use std::time::{Duration, Instant};
        use windows::Win32::UI::WindowsAndMessaging::{EnumChildWindows, GetWindow, GW_OWNER};
        type Row = (u32, String, String, [i32; 4], bool, isize, isize);
        struct Found {
            jai: HashMap<u32, bool>,
            rows: Vec<(isize, Row)>,
        }
        unsafe extern "system" fn each(hwnd: HWND, lparam: LPARAM) -> BOOL {
            // SAFETY: lparam is the &mut Found passed below and outlives the enumeration; plain queries.
            unsafe {
                let found = &mut *(lparam.0 as *mut Found);
                let mut pid = 0u32;
                GetWindowThreadProcessId(hwnd, Some(&mut pid));
                let jai = *found.jai.entry(pid).or_insert_with(|| pid != std::process::id() && crate::win32::process_name(pid).is_some_and(|n| n.eq_ignore_ascii_case("jai")));
                let class = class_of(hwnd);
                if jai && !matches!(class.as_str(), "IME" | "MSCTFIME UI" | "NVOpenGLPbuffer" | "JaiTrayWindow") && !class.starts_with("Winit Thread") {
                    if let Some(rect) = rect_of(hwnd) {
                        found.rows.push((hwnd.0, (pid, class, title_of(hwnd), rect, IsWindowVisible(hwnd).as_bool(), GetWindowLongPtrW(hwnd, GWL_EXSTYLE), GetWindow(hwnd, GW_OWNER).0)));
                    }
                }
            }
            TRUE
        }
        /// Who made a window seen for the first time: its thread (its name, and the module its
        /// start address lies in), and the module of its instance (kernel32's `K32…` exports,
        /// ntdll's thread query).
        fn made_by(hwnd: isize, pid: u32) -> String {
            use windows::Win32::System::Threading::{OpenProcess, OpenThread, PROCESS_QUERY_INFORMATION, PROCESS_VM_READ, THREAD_QUERY_INFORMATION, THREAD_QUERY_LIMITED_INFORMATION};
            use windows::Win32::UI::WindowsAndMessaging::GWLP_HINSTANCE;
            #[repr(C)]
            struct ModuleInfo {
                base: usize,
                size: u32,
                entry: usize,
            }
            #[link(name = "kernel32")]
            extern "system" {
                fn K32EnumProcessModules(process: isize, modules: *mut isize, cb: u32, needed: *mut u32) -> i32;
                fn K32GetModuleFileNameExW(process: isize, module: isize, name: *mut u16, size: u32) -> u32;
                fn K32GetModuleInformation(process: isize, module: isize, info: *mut ModuleInfo, cb: u32) -> i32;
                fn GetThreadDescription(thread: isize, description: *mut *mut u16) -> i32;
                fn LocalFree(memory: isize) -> isize;
            }
            #[link(name = "ntdll")]
            extern "system" {
                fn NtQueryInformationThread(thread: isize, class: u32, info: *mut usize, length: u32, returned: *mut u32) -> i32;
            }
            // SAFETY: plain queries; buffers go with their lengths; every handle is closed and the
            // thread's description freed.
            unsafe {
                let tid = GetWindowThreadProcessId(HWND(hwnd), None);
                let instance = GetWindowLongPtrW(HWND(hwnd), GWLP_HINSTANCE) as usize;
                let (mut name, mut start) = (String::new(), 0usize);
                if let Ok(thread) = OpenThread(THREAD_QUERY_INFORMATION | THREAD_QUERY_LIMITED_INFORMATION, false, tid) {
                    let mut text: *mut u16 = std::ptr::null_mut();
                    if GetThreadDescription(thread.0, &mut text) >= 0 && !text.is_null() {
                        let length = (0..).take_while(|&i| *text.add(i) != 0).count();
                        name = String::from_utf16_lossy(std::slice::from_raw_parts(text, length));
                        LocalFree(text as isize);
                    }
                    // (Class 9: the thread's start address as Win32 knows it.)
                    let _ = NtQueryInformationThread(thread.0, 9, &mut start, std::mem::size_of::<usize>() as u32, std::ptr::null_mut());
                    let _ = windows::Win32::Foundation::CloseHandle(thread);
                }
                let Ok(process) = OpenProcess(PROCESS_QUERY_INFORMATION | PROCESS_VM_READ, false, pid) else { return format!("  thread {tid} '{name}'") };
                let (mut modules, mut needed) = (vec![0isize; 2048], 0u32);
                let listed = K32EnumProcessModules(process.0, modules.as_mut_ptr(), (modules.len() * 8) as u32, &mut needed) != 0;
                let module_of = |address: usize| -> String {
                    for &module in &modules[..(needed as usize / 8).min(modules.len())] {
                        let mut info = ModuleInfo { base: 0, size: 0, entry: 0 };
                        if K32GetModuleInformation(process.0, module, &mut info, std::mem::size_of::<ModuleInfo>() as u32) != 0 && address >= info.base && address < info.base + info.size as usize {
                            let mut buf = [0u16; 512];
                            let n = K32GetModuleFileNameExW(process.0, module, buf.as_mut_ptr(), buf.len() as u32) as usize;
                            return String::from_utf16_lossy(&buf[..n.min(buf.len())]).rsplit('\\').next().unwrap_or_default().to_string();
                        }
                    }
                    format!("{address:#x}")
                };
                let line = if listed { format!("  thread {tid} '{name}' started in {} instance of {}", module_of(start), module_of(instance)) } else { format!("  thread {tid} '{name}' start {start:#x}") };
                let _ = windows::Win32::Foundation::CloseHandle(process);
                line
            }
        }
        let seconds: u64 = std::env::var("JAI_WATCH").ok().and_then(|s| s.parse().ok()).unwrap_or(20);
        // (Each spot with what it showed when the watch began.)
        let spots: Vec<([i32; 4], Vec<u8>)> = std::env::var("JAI_WATCH_THROUGH")
            .unwrap_or_default()
            .split(';')
            .filter_map(|spot| <[i32; 4]>::try_from(spot.split(',').filter_map(|n| n.trim().parse().ok()).collect::<Vec<i32>>()).ok())
            .filter_map(|spot| Some((spot, crate::instant_replay::screen_pixels(spot)?)))
            .collect();
        let (mut looked, mut said) = (0, Vec::<String>::new());
        let key_out = std::env::var("JAI_WATCH_KEY_OUT").ok();
        // (`JAI_WATCH_HIT=x,y;x,y`: whose window a click at each of these points would go to.)
        let hits: Vec<[i32; 2]> = std::env::var("JAI_WATCH_HIT").unwrap_or_default().split(';').filter_map(|at| <[i32; 2]>::try_from(at.split(',').filter_map(|n| n.trim().parse().ok()).collect::<Vec<i32>>()).ok()).collect();
        let mut hit_said = vec![String::new(); hits.len()];
        // (`JAI_WATCH_CLICK=part of a title|x,y|second;…`: at that second a click is put into
        // that window of his, at x,y of its client area, counted from its right or bottom edge
        // where negative. Sir's own pointer is not moved.)
        let mut clicks: Vec<(String, [i32; 2], u64)> = std::env::var("JAI_WATCH_CLICK")
            .unwrap_or_default()
            .split(';')
            .filter_map(|click| {
                let mut parts = click.split('|');
                let (title, at, when) = (parts.next()?, parts.next()?, parts.next()?);
                // (`close` in place of x,y: Windows' own close, as Alt+F4 or the taskbar ask it.)
                let at = if at.trim() == "close" { [i32::MIN, 0] } else { <[i32; 2]>::try_from(at.split(',').filter_map(|n| n.trim().parse().ok()).collect::<Vec<i32>>()).ok()? };
                Some((title.to_string(), at, when.trim().parse().ok()?))
            })
            .collect();
        // (`JAI_WATCH_PAINT=second`: from that second on the board is told to paint with every
        // look, by a message sent to it and not by its turn in the queue: whether a board
        // that had stopped drawing draws again that way.)
        let paint_from: Option<u64> = std::env::var("JAI_WATCH_PAINT").ok().and_then(|s| s.parse().ok());
        let fast = std::env::var("JAI_WATCH_FAST").is_ok();
        // (How often it looks, in milliseconds: 40, or `JAI_WATCH_MS` to count a motion's frames.)
        let every = Duration::from_millis(std::env::var("JAI_WATCH_MS").ok().and_then(|s| s.parse().ok()).unwrap_or(40));
        let (start, mut found) = (Instant::now(), Found { jai: HashMap::new(), rows: Vec::new() });
        let mut last: HashMap<isize, Row> = HashMap::new();
        let mut moves: HashMap<isize, (u64, u32)> = HashMap::new();
        while start.elapsed() < Duration::from_secs(seconds) {
            found.rows.clear();
            // SAFETY: the callback only touches `found` through the provided pointer.
            unsafe {
                let _ = EnumWindows(Some(each), LPARAM(&mut found as *mut Found as isize));
                let _ = EnumChildWindows(FindWindowW(w!("Progman"), PCWSTR::null()), Some(each), LPARAM(&mut found as *mut Found as isize));
            }
            let (ms, second) = (start.elapsed().as_millis(), start.elapsed().as_secs());
            if let Some((hwnd, _)) = found.rows.iter().find(|(_, row)| paint_from.is_some_and(|from| second >= from) && row.2 == "J.A.I. HUD") {
                use windows::Win32::Graphics::Gdi::{RedrawWindow, RDW_INTERNALPAINT, RDW_UPDATENOW};
                // SAFETY: a paint request for a window handle from the enumeration.
                unsafe {
                    let _ = RedrawWindow(HWND(*hwnd), None, None, RDW_INTERNALPAINT | RDW_UPDATENOW);
                }
            }
            for (hwnd, row) in &found.rows {
                if last.get(hwnd) == Some(row) {
                    continue;
                }
                let (at, count) = moves.entry(*hwnd).or_insert((second, 0));
                if *at != second {
                    if *count > 3 {
                        println!("{ms:>6} ms  {hwnd:#x} '{}' moved {count} times in second {at}", row.2);
                    }
                    (*at, *count) = (second, 0);
                }
                *count += 1;
                if *count <= 3 {
                    let (pid, class, title, rect, shown, ex, owner) = row;
                    let made = if last.contains_key(hwnd) { String::new() } else { made_by(*hwnd, *pid) };
                    println!("{ms:>6} ms  {hwnd:#x} pid {pid} {class:<24} '{title}' {rect:?} {} ex {ex:#010x} owner {owner:#x}{made}", if *shown { "shown" } else { "HIDDEN" });
                }
                // An experiment (`JAI_WATCH_KEY_OUT=<title>`): that window of his made a layered
                // one whose pure black Windows sees through, from here.
                if key_out.as_deref() == Some(row.2.as_str()) && row.4 && row.5 & WS_EX_LAYERED.0 as isize == 0 {
                    use windows::Win32::Foundation::COLORREF;
                    use windows::Win32::UI::WindowsAndMessaging::{SetLayeredWindowAttributes, LWA_COLORKEY};
                    // SAFETY: style and layer attributes of a window handle from the enumeration.
                    let done = unsafe {
                        SetWindowLongPtrW(HWND(*hwnd), GWL_EXSTYLE, row.5 | WS_EX_LAYERED.0 as isize);
                        SetLayeredWindowAttributes(HWND(*hwnd), COLORREF(0), 0, LWA_COLORKEY)
                    };
                    println!("{ms:>6} ms  {hwnd:#x} '{}': its black keyed out ({done:?})", row.2);
                }
                last.insert(*hwnd, row.clone());
            }
            let gone: Vec<isize> = last.keys().filter(|h| !found.rows.iter().any(|(hwnd, _)| hwnd == *h)).copied().collect();
            for hwnd in gone {
                println!("{ms:>6} ms  {hwnd:#x} '{}' is gone", last.remove(&hwnd).map(|r| r.2).unwrap_or_default());
            }
            clicks.retain(|(title, at, when)| {
                if second < *when {
                    return true;
                }
                let Some((hwnd, row)) = found.rows.iter().find(|(_, row)| row.4 && row.2.contains(title.as_str())) else { return second < *when + 5 };
                if at[0] == i32::MIN {
                    // SAFETY: a close request posted to a window handle from the enumeration.
                    unsafe {
                        let _ = windows::Win32::UI::WindowsAndMessaging::PostMessageW(HWND(*hwnd), windows::Win32::UI::WindowsAndMessaging::WM_CLOSE, WPARAM(0), LPARAM(0));
                    }
                    println!("{ms:>6} ms  '{}' asked to close, as by Windows", row.2);
                    return false;
                }
                let (w, h) = (row.3[2] - row.3[0], row.3[3] - row.3[1]);
                let (x, y) = (if at[0] < 0 { w + at[0] } else { at[0] }, if at[1] < 0 { h + at[1] } else { at[1] });
                let place = LPARAM(((y as isize) << 16) | (x as isize & 0xFFFF));
                // SAFETY: mouse messages posted to a window handle from the enumeration.
                unsafe {
                    use windows::Win32::UI::WindowsAndMessaging::PostMessageW;
                    for (message, keys) in [(WM_MOUSEMOVE, 0), (WM_LBUTTONDOWN, 1), (WM_LBUTTONUP, 0)] {
                        let _ = PostMessageW(HWND(*hwnd), message, WPARAM(keys), place);
                    }
                }
                println!("{ms:>6} ms  a click put into '{}' at {x},{y}", row.2);
                false
            });
            // (Once a second; with `JAI_WATCH_FAST` with every look, for what shows for a moment.)
            if second > looked || fast {
                looked = second;
                for (k, at) in hits.iter().enumerate() {
                    // SAFETY: plain queries on the window under a point.
                    let line = unsafe {
                        let hwnd = WindowFromPoint(POINT { x: at[0], y: at[1] });
                        let mut pid = 0u32;
                        GetWindowThreadProcessId(hwnd, Some(&mut pid));
                        let program = crate::win32::process_name(pid).unwrap_or_default();
                        let own = if program.eq_ignore_ascii_case("jai") { format!(" '{}'", title_of(hwnd)) } else { String::new() };
                        format!("point {} ({},{}): a click would go to {program} [{}]{own}", k + 1, at[0], at[1], class_of(hwnd))
                    };
                    if hit_said[k] != line {
                        println!("{ms:>6} ms  {line}");
                        hit_said[k] = line;
                    }
                }
                for (k, (spot, before)) in spots.iter().enumerate() {
                    let Some(now) = crate::instant_replay::screen_pixels(*spot) else { continue };
                    let pixels = (before.len() / 3).max(1);
                    let same = before.chunks_exact(3).zip(now.chunks_exact(3)).filter(|(a, b)| a.iter().zip(*b).all(|(x, y)| x.abs_diff(*y) <= 6)).count();
                    // (How bright its middle and its four corners are, of 255: now, and before.)
                    let side = (pixels as f64).sqrt() as usize;
                    let light = |bgr: &[u8], middle: bool| {
                        let (mut sum, mut n) = (0u64, 0u64);
                        for (i, p) in bgr.chunks_exact(3).enumerate() {
                            let (x, y) = ((i % side) as f64 / side as f64 - 0.5, (i / side) as f64 / side as f64 - 0.5);
                            let r = (x * x + y * y).sqrt();
                            if (middle && r < 0.3) || (!middle && r > 0.56) {
                                sum += p.iter().map(|c| *c as u64).sum::<u64>();
                                n += 3;
                            }
                        }
                        sum as f64 / n.max(1) as f64
                    };
                    let line = format!("spot {}: {} % as it was; middle {:.1} (was {:.1}), corners {:.1} (were {:.1})", k + 1, 100 * same / pixels, light(&now, true), light(before, true), light(&now, false), light(before, false));
                    // (Said when it changes, not every second.)
                    if said.get(k) != Some(&line) {
                        println!("{ms:>6} ms  {line}");
                        said.resize(spots.len(), String::new());
                        said[k] = line;
                    }
                }
            }
            std::thread::sleep(every);
        }
        for (hwnd, (at, count)) in moves {
            if count > 3 {
                println!("   end  {hwnd:#x} moved {count} times in second {at}");
            }
        }
    }

    /// A look at the live desktop, for when the orb says the emblem is covered and Sir sees
    /// nothing on it (`cargo test --bin jai what_counts_as_lying_on_the_screen -- --ignored
    /// --nocapture`): every window that counts, from the top down, with its program, its class
    /// and its extended style. (No titles: they are Sir's.)
    #[test]
    #[ignore]
    fn what_counts_as_lying_on_the_screen() {
        for (hwnd, rect, _) in top_level_windows() {
            let mut pid = 0u32;
            // SAFETY: plain queries on a handle from the enumeration.
            let ex = unsafe {
                GetWindowThreadProcessId(hwnd, Some(&mut pid));
                GetWindowLongPtrW(hwnd, GWL_EXSTYLE)
            };
            println!("{:<30} {:<36} {rect:?} ex {ex:#010x}", crate::win32::process_name(pid).unwrap_or_default(), class_of(hwnd));
        }
    }
}
