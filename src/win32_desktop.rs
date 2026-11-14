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
            if cloaked != 0 || style & WS_EX_TRANSPARENT.0 as isize != 0 || shell_class(&class_of(hwnd)) {
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

/// The share (0 to 1) of the screen rect `[left, top, right, bottom]` that the one window of
/// another program covering most of it covers. His own windows do not count.
pub fn covered_share(rect: [i32; 4]) -> f32 {
    let area = ((rect[2] - rect[0]).max(1) as i64) * ((rect[3] - rect[1]).max(1) as i64);
    let most = top_level_windows().into_iter().filter(|(_, _, own)| !own).map(|(_, r, _)| overlap(rect, r)).max().unwrap_or(0);
    most as f32 / area as f32
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

/// Gives the corner orb's window its shape: a disc of `orb` pixels for the orb and, where the
/// window is wider, the plate beside it (from 12 pixels right of the disc). The window's own
/// surface may or may not be see-through; cut to this shape it looks the same either way.
pub fn shape_orb(hwnd: isize, orb: i32, width: i32, height: i32) {
    use windows::Win32::Graphics::Gdi::{CombineRgn, CreateEllipticRgn, CreateRectRgn, DeleteObject, SetWindowRgn, RGN_OR};
    // SAFETY: regions made here; the one handed to the window is the system's from then on, the
    // other is deleted.
    unsafe {
        if !IsWindow(HWND(hwnd)).as_bool() {
            return;
        }
        let shape = CreateEllipticRgn(0, 0, orb + 1, orb + 1);
        if width > orb + 12 {
            let plate = CreateRectRgn(orb + 12, 0, width, height);
            CombineRgn(shape, shape, plate, RGN_OR);
            let _ = DeleteObject(plate);
        }
        SetWindowRgn(HWND(hwnd), shape, TRUE);
    }
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
}
