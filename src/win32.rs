//! Native Win32 helpers (no PowerShell round-trips): ShellExecuteEx launching, window
//! enumeration/control, media keys via SendInput and Core Audio master volume.
//! Every call runs on one dedicated thread that initialises COM (STA) exactly once.

use std::ffi::c_void;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{mpsc, OnceLock};
use windows::core::{w, PCWSTR, PWSTR};
use windows::Win32::Foundation::{CloseHandle, BOOL, HWND, LPARAM, LRESULT, RECT, TRUE, WPARAM};
use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS};
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MonitorFromRect, MonitorFromWindow, MONITORINFO, MONITOR_DEFAULTTONEAREST, MONITOR_DEFAULTTONULL,
};
use windows::Win32::Media::Audio::Endpoints::IAudioEndpointVolume;
use windows::Win32::Media::Audio::{eConsole, eRender, IMMDeviceEnumerator, MMDeviceEnumerator};
use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CLSCTX_ALL, COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE};
use windows::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYBD_EVENT_FLAGS, KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP,
    VIRTUAL_KEY, VK_MENU,
};
pub use windows::Win32::UI::Input::KeyboardAndMouse::{VK_CONTROL, VK_F, VK_F11, VK_RETURN, VK_SHIFT, VK_W};
use windows::Win32::UI::Shell::{
    DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass, ShellExecuteExW, SEE_MASK_FLAG_NO_UI, SEE_MASK_NOASYNC,
    SHELLEXECUTEINFOW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    BringWindowToTop, DestroyWindow, EnumWindows, FindWindowW, GetAncestor, GetClassNameW, GetForegroundWindow, GetWindow,
    GetWindowLongPtrW, GetWindowPlacement, GetWindowRect, GetWindowTextLengthW, GetWindowThreadProcessId, InternalGetWindowText,
    IsIconic, IsWindow, IsWindowVisible, IsZoomed, PostMessageW, RegisterWindowMessageW, SendMessageW, SetForegroundWindow,
    SetWindowPlacement, SetWindowPos, ShowWindow, ShowWindowAsync, GA_ROOTOWNER, GWL_EXSTYLE, GW_OWNER, SHOW_WINDOW_CMD,
    SWP_NOACTIVATE, SWP_NOZORDER, SW_HIDE, SW_MAXIMIZE, SW_MINIMIZE, SW_RESTORE, SW_SHOW, SW_SHOWNORMAL, WINDOWPLACEMENT,
    WM_APPCOMMAND, WM_CLOSE, WM_COMMAND, WM_NCDESTROY, WS_EX_TOOLWINDOW,
};

pub use windows::Win32::UI::Input::KeyboardAndMouse::{
    VK_MEDIA_NEXT_TRACK, VK_MEDIA_PLAY_PAUSE, VK_MEDIA_PREV_TRACK, VK_MEDIA_STOP, VK_VOLUME_DOWN, VK_VOLUME_MUTE, VK_VOLUME_UP,
};

type Job = Box<dyn FnOnce() + Send>;

fn worker() -> Option<&'static mpsc::Sender<Job>> {
    static TX: OnceLock<Option<mpsc::Sender<Job>>> = OnceLock::new();
    TX.get_or_init(|| {
        let (tx, rx) = mpsc::channel::<Job>();
        std::thread::Builder::new()
            .name("jarvis-win32".into())
            .spawn(move || {
                // SAFETY: called once at the start of this thread, before any COM use.
                let _ = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) };
                for job in rx {
                    job();
                }
            })
            .ok()
            .map(|_| tx)
    })
    .as_ref()
}

/// Runs `f` on the Win32/COM worker thread without waiting for it.
pub fn run_detached(f: impl FnOnce() + Send + 'static) {
    if let Some(tx) = worker() {
        let _ = tx.send(Box::new(f));
    }
}

/// Runs `f` on the Win32/COM worker thread and awaits its result.
pub async fn call<R: Send + 'static>(f: impl FnOnce() -> R + Send + 'static) -> Result<R, String> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    let job: Job = Box::new(move || {
        let _ = tx.send(f());
    });
    worker()
        .ok_or_else(|| "Win32 worker thread unavailable".to_string())?
        .send(job)
        .map_err(|_| "Win32 worker thread stopped".to_string())?;
    rx.await.map_err(|_| "Win32 worker dropped the request".to_string())
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// ShellExecuteEx("open") without error dialogs. Handles exe names via App Paths/PATH,
/// absolute paths, URLs, URI schemes (`spotify:`) and `shell:AppsFolder\<AUMID>`.
pub fn shell_execute(file: &str, params: Option<&str>, dir: Option<&str>) -> Result<(), String> {
    let verb = wide("open");
    let file_w = wide(file);
    let params_w = params.filter(|p| !p.is_empty()).map(wide);
    let dir_w = dir.filter(|d| !d.is_empty()).map(wide);
    let mut info = SHELLEXECUTEINFOW {
        cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOASYNC | SEE_MASK_FLAG_NO_UI,
        lpVerb: PCWSTR(verb.as_ptr()),
        lpFile: PCWSTR(file_w.as_ptr()),
        lpParameters: params_w.as_ref().map_or(PCWSTR::null(), |p| PCWSTR(p.as_ptr())),
        lpDirectory: dir_w.as_ref().map_or(PCWSTR::null(), |d| PCWSTR(d.as_ptr())),
        nShow: SW_SHOWNORMAL.0,
        ..Default::default()
    };
    // SAFETY: all string pointers outlive the call; `info` is a valid, sized struct.
    unsafe { ShellExecuteExW(&mut info) }.map_err(|e| e.message().to_string())
}

#[derive(Debug, Clone)]
pub struct WindowInfo {
    pub hwnd: isize,
    pub title: String,
    pub class: String,
    pub pid: u32,
    pub process: String,
}

/// What `hwnd` is, when it is a visible, uncloaked, unowned top-level application window with a
/// title; None for everything else (tool windows, cloaked store apps, owned dialogs).
///
/// # Safety
/// `hwnd` comes from a window enumeration (the queries made here tolerate a window that has
/// closed in the meantime).
unsafe fn app_window(hwnd: HWND) -> Option<WindowInfo> {
    // SAFETY: plain queries on a window handle, into sized local buffers.
    unsafe {
        if !IsWindowVisible(hwnd).as_bool() || GetWindow(hwnd, GW_OWNER).0 != 0 {
            return None;
        }
        if (GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32) & WS_EX_TOOLWINDOW.0 != 0 {
            return None;
        }
        let mut cloaked: u32 = 0;
        if DwmGetWindowAttribute(hwnd, DWMWA_CLOAKED, &mut cloaked as *mut u32 as *mut c_void, std::mem::size_of::<u32>() as u32).is_ok() && cloaked != 0 {
            return None;
        }
        let title = window_text(hwnd);
        if title.is_empty() {
            return None;
        }
        let mut cls = [0u16; 256];
        let cn = GetClassNameW(hwnd, &mut cls).max(0) as usize;
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        Some(WindowInfo { hwnd: hwnd.0, title, class: String::from_utf16_lossy(&cls[..cn]), pid, process: process_name(pid).unwrap_or_default() })
    }
}

/// A window's title as Windows keeps it, read without asking the window. For a window of this
/// process GetWindowText sends it WM_GETTEXT, which the HUD's thread answers only between two
/// frames (it waits on the display): a turn routed while the HUD was the top window paid one
/// or two frames (13 or 27 ms) for the HUD's title.
fn window_text(hwnd: HWND) -> String {
    let mut buf = [0u16; 512];
    // SAFETY: a plain query into a sized local buffer; a window that has closed reads as "".
    let n = unsafe { InternalGetWindowText(hwnd, &mut buf) }.clamp(0, buf.len() as i32) as usize;
    String::from_utf16_lossy(&buf[..n])
}

/// Visible, uncloaked, unowned top-level application windows with a title.
pub fn app_windows() -> Vec<WindowInfo> {
    unsafe extern "system" fn collect(hwnd: HWND, lparam: LPARAM) -> BOOL {
        // SAFETY: lparam is the &mut Vec passed below and outlives EnumWindows.
        let list = unsafe { &mut *(lparam.0 as *mut Vec<WindowInfo>) };
        // SAFETY: a handle from the enumeration.
        if let Some(window) = unsafe { app_window(hwnd) } {
            list.push(window);
        }
        TRUE
    }
    let mut list: Vec<WindowInfo> = Vec::new();
    // SAFETY: the callback only touches `list` through the provided pointer.
    unsafe {
        let _ = EnumWindows(Some(collect), LPARAM(&mut list as *mut Vec<WindowInfo> as isize));
    }
    list
}

/// Posts `WM_APPCOMMAND` (`command`, e.g. 47 = APPCOMMAND_MEDIA_PAUSE) to the titled, unowned
/// top-level windows of process `pid` — visible or not (a player minimised to the tray still
/// gets it). Returns how many windows received it.
pub fn post_app_command(pid: u32, command: u32) -> usize {
    struct Search {
        pid: u32,
        found: Vec<HWND>,
    }
    unsafe extern "system" fn collect(hwnd: HWND, lparam: LPARAM) -> BOOL {
        // SAFETY: lparam is the &mut Search passed below and outlives EnumWindows.
        let search = unsafe { &mut *(lparam.0 as *mut Search) };
        unsafe {
            let mut owner = 0u32;
            GetWindowThreadProcessId(hwnd, Some(&mut owner));
            if owner == search.pid && GetWindow(hwnd, GW_OWNER).0 == 0 && GetWindowTextLengthW(hwnd) > 0 {
                search.found.push(hwnd);
            }
        }
        TRUE
    }
    let mut search = Search { pid, found: Vec::new() };
    // SAFETY: the callback only touches `search` through the provided pointer.
    unsafe {
        let _ = EnumWindows(Some(collect), LPARAM(&mut search as *mut Search as isize));
    }
    // GET_APPCOMMAND_LPARAM reads the command from the high word; the device bits stay 0 (key).
    let lparam = LPARAM(((command & 0x0FFF) << 16) as isize);
    search
        .found
        .iter()
        // SAFETY: posting a message to a window handle is harmless even if it closed meanwhile.
        .filter(|hwnd| unsafe { PostMessageW(**hwnd, WM_APPCOMMAND, WPARAM(hwnd.0 as usize), lparam) }.is_ok())
        .count()
}

/// Makes this process's top-level window titled `title` an owned window of `owner`: Windows keeps
/// an owned window above its owner whatever raises the owner. False while there is no such window.
pub fn own_window_titled(title: &str, owner: isize) -> bool {
    use windows::Win32::UI::WindowsAndMessaging::{SetWindowLongPtrW, GWLP_HWNDPARENT};
    struct Search<'a> {
        title: &'a str,
        found: Option<HWND>,
    }
    unsafe extern "system" fn find(hwnd: HWND, lparam: LPARAM) -> BOOL {
        // SAFETY: lparam is the &mut Search passed below and outlives EnumWindows.
        let search = unsafe { &mut *(lparam.0 as *mut Search) };
        let mut pid = 0u32;
        // SAFETY: a plain query on a handle from the enumeration.
        unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
        if pid == std::process::id() && window_text(hwnd) == search.title {
            search.found = Some(hwnd);
            return BOOL(0);
        }
        TRUE
    }
    let mut search = Search { title, found: None };
    // SAFETY: the callback only touches `search` through the provided pointer.
    unsafe {
        let _ = EnumWindows(Some(find), LPARAM(&mut search as *mut Search as isize));
    }
    let Some(hwnd) = search.found.filter(|h| h.0 != owner) else { return false };
    // SAFETY: setting the owner of our own window to our own HUD window.
    unsafe { SetWindowLongPtrW(hwnd, GWLP_HWNDPARENT, owner) };
    true
}

/// One of JAI's own windows (the HUD, his extra windows, a dialog of his process).
pub fn is_jarvis_window(w: &WindowInfo) -> bool {
    static OWN: OnceLock<u32> = OnceLock::new();
    w.pid == *OWN.get_or_init(std::process::id) || w.process == "jai" || w.title.contains("J.A.I.")
}

/// The window Sir is working in, never one of JAI's own: the top of the Z-order among the
/// visible, non-minimised application windows (when he typed into the HUD, the one beneath it).
/// The enumeration runs from the top and stops at the first one that qualifies, so a turn does
/// not pay for looking up the process of every open window (measured: ~10 ms a turn before).
pub fn user_window() -> Option<WindowInfo> {
    unsafe extern "system" fn first(hwnd: HWND, lparam: LPARAM) -> BOOL {
        // SAFETY: lparam is the &mut Option passed below and outlives EnumWindows.
        let found = unsafe { &mut *(lparam.0 as *mut Option<WindowInfo>) };
        // SAFETY: a handle from the enumeration; plain queries on it.
        let his = unsafe { app_window(hwnd) }.filter(|w| !is_jarvis_window(w) && !matches!(w.class.as_str(), "Progman" | "WorkerW" | "Shell_TrayWnd") && !unsafe { IsIconic(hwnd) }.as_bool());
        match his {
            Some(window) => {
                *found = Some(window);
                BOOL(0)
            }
            None => TRUE,
        }
    }
    let mut found: Option<WindowInfo> = None;
    // SAFETY: the callback only touches `found` through the provided pointer. (EnumWindows
    // reports an error when its callback stops it early: here that is the normal case.)
    unsafe {
        let _ = EnumWindows(Some(first), LPARAM(&mut found as *mut Option<WindowInfo> as isize));
    }
    found
}

/// The handle still names a live window.
pub fn is_window(hwnd: isize) -> bool {
    // SAFETY: IsWindow accepts any value.
    hwnd != 0 && unsafe { IsWindow(HWND(hwnd)) }.as_bool()
}

/// Whether `hwnd` (or a window it owns, like a browser's popup) is in front.
pub fn is_foreground(hwnd: isize) -> bool {
    // SAFETY: plain queries of window handles.
    unsafe {
        let fg = GetForegroundWindow();
        fg.0 == hwnd || GetAncestor(fg, GA_ROOTOWNER).0 == hwnd
    }
}

/// The window covers its whole monitor (true full screen, not just maximised).
pub fn covers_monitor(hwnd: isize) -> bool {
    let h = HWND(hwnd);
    // SAFETY: queries on a window handle; structs are properly sized out-parameters.
    unsafe {
        let mut rect = RECT::default();
        if GetWindowRect(h, &mut rect).is_err() {
            return false;
        }
        let mut info = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        if !GetMonitorInfoW(MonitorFromWindow(h, MONITOR_DEFAULTTONEAREST), &mut info).as_bool() {
            return false;
        }
        let m = info.rcMonitor;
        rect.left <= m.left && rect.top <= m.top && rect.right >= m.right && rect.bottom >= m.bottom
    }
}

/// The primary monitor: (width px, height px, DPI scale) from `GetSystemMetrics(SM_CXSCREEN /
/// SM_CYSCREEN)` and the system DPI (the process is per-monitor DPI aware, so these are physical
/// pixels).
pub fn primary_screen() -> (i32, i32, f32) {
    use windows::Win32::Graphics::Gdi::{GetDC, GetDeviceCaps, ReleaseDC, LOGPIXELSX};
    use windows::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_CXSCREEN, SM_CYSCREEN};
    // SAFETY: plain metric queries; the screen DC is released right after use.
    unsafe {
        let (w, h) = (GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN));
        let dc = GetDC(HWND(0));
        let dpi = if dc.0 != 0 { GetDeviceCaps(dc, LOGPIXELSX) } else { 96 };
        if dc.0 != 0 {
            ReleaseDC(HWND(0), dc);
        }
        (w, h, (dpi.max(48) as f32 / 96.0).clamp(0.5, 4.0))
    }
}

/// Stretches a borderless window over its whole monitor (the fullscreen board's safety net when
/// it ends up smaller than the monitor). Synchronous: run it on the Win32 worker thread.
pub fn cover_monitor(hwnd: isize) -> Result<(), String> {
    use windows::Win32::UI::WindowsAndMessaging::{HWND_TOP, SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_SHOWWINDOW};
    let [l, t, r, b] = window_display_rect(hwnd).ok_or("could not read the window's monitor")?;
    // SAFETY: a position change on a window handle; failures are reported.
    unsafe {
        SetWindowPos(HWND(hwnd), HWND_TOP, l, t, r - l, b - t, SWP_SHOWWINDOW | SWP_NOACTIVATE | SWP_FRAMECHANGED).map_err(|e| e.message().to_string())
    }
}

/// A window that covers its monitor but is still maximised underneath (winit keeps a maximised
/// window "maximised" when it goes fullscreen, F11 from windowed mode, so Windows and JAI's
/// own checks take the board for a maximised window): made a plain window of exactly the same
/// rect, so nothing moves on screen. Returns whether it was maximised. Synchronous: run it on
/// the Win32 worker thread.
pub fn unmaximise_in_place(hwnd: isize) -> Result<bool, String> {
    use windows::Win32::Foundation::POINT;
    use windows::Win32::Graphics::Gdi::{MonitorFromPoint, MONITOR_DEFAULTTOPRIMARY};
    use windows::Win32::UI::WindowsAndMessaging::{SW_SHOWNOACTIVATE, WINDOWPLACEMENT_FLAGS};
    let h = HWND(hwnd);
    // SAFETY: queries and one placement change on a window handle; the structs are sized;
    // failures are reported.
    unsafe {
        if !IsZoomed(h).as_bool() {
            return Ok(false);
        }
        let mut rect = RECT::default();
        GetWindowRect(h, &mut rect).map_err(|e| e.message().to_string())?;
        let mut wp = WINDOWPLACEMENT { length: std::mem::size_of::<WINDOWPLACEMENT>() as u32, ..Default::default() };
        GetWindowPlacement(h, &mut wp).map_err(|e| e.message().to_string())?;
        // The normal position is in workspace coordinates, which start at the corner of the
        // primary monitor's work area (the screen's corner unless the taskbar sits top or left).
        let mut primary = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        let (dx, dy) = if GetMonitorInfoW(MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY), &mut primary).as_bool() {
            (primary.rcWork.left, primary.rcWork.top)
        } else {
            (0, 0)
        };
        wp.rcNormalPosition = RECT { left: rect.left - dx, top: rect.top - dy, right: rect.right - dx, bottom: rect.bottom - dy };
        wp.showCmd = SW_SHOWNOACTIVATE.0 as u32;
        wp.flags = WINDOWPLACEMENT_FLAGS(0);
        SetWindowPlacement(h, &wp).map_err(|e| e.message().to_string())?;
        Ok(true)
    }
}

/// Presses `modifiers` + `key` and releases them in reverse order (F11, Ctrl+W, Ctrl+Shift+F),
/// delivered to whichever window has the keyboard focus.
pub fn send_chord(modifiers: &[VIRTUAL_KEY], key: VIRTUAL_KEY) -> Result<(), String> {
    let mut inputs: Vec<INPUT> = modifiers.iter().map(|m| key_input(*m, KEYBD_EVENT_FLAGS(0))).collect();
    inputs.push(key_input(key, KEYBD_EVENT_FLAGS(0)));
    inputs.push(key_input(key, KEYEVENTF_KEYUP));
    inputs.extend(modifiers.iter().rev().map(|m| key_input(*m, KEYEVENTF_KEYUP)));
    // SAFETY: valid INPUT array with matching element size.
    let sent = unsafe { SendInput(&inputs, std::mem::size_of::<INPUT>() as i32) };
    if sent == inputs.len() as u32 {
        Ok(())
    } else {
        Err("SendInput was blocked (is a higher-integrity window focused?)".into())
    }
}

/// The window in front right now: its process name and whether it belongs to JAI himself
/// (the addressee detector: his HUD in front means Sir is talking to him).
pub fn foreground_info() -> Option<(String, bool)> {
    // SAFETY: plain queries of the foreground window handle.
    unsafe {
        let hwnd = GetForegroundWindow();
        if hwnd.0 == 0 {
            return None;
        }
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        Some((process_name(pid).unwrap_or_default(), pid == std::process::id()))
    }
}

#[link(name = "kernel32")]
extern "system" {
    /// Milliseconds since Windows started (wraps after 49.7 days, like `LASTINPUTINFO::dwTime`).
    fn GetTickCount() -> u32;
}

/// Seconds since Sir last touched the keyboard or the mouse (None if Windows will not say).
pub fn idle_seconds() -> Option<u64> {
    use windows::Win32::UI::Input::KeyboardAndMouse::{GetLastInputInfo, LASTINPUTINFO};
    let mut last = LASTINPUTINFO { cbSize: std::mem::size_of::<LASTINPUTINFO>() as u32, dwTime: 0 };
    // SAFETY: the struct is sized as the call requires; both calls only read system counters.
    unsafe {
        if !GetLastInputInfo(&mut last).as_bool() {
            return None;
        }
        Some((GetTickCount().wrapping_sub(last.dwTime) / 1000) as u64)
    }
}

/// The clipboard's change counter: it moves whenever anything is copied.
pub fn clipboard_sequence() -> u32 {
    // SAFETY: reads a counter.
    unsafe { windows::Win32::System::DataExchange::GetClipboardSequenceNumber() }
}

/// A game, video or presentation covers the whole monitor of the foreground window.
pub fn foreground_is_fullscreen() -> bool {
    // SAFETY: queries of the foreground window and its monitor; structs are properly sized.
    unsafe {
        let hwnd = GetForegroundWindow();
        if hwnd.0 == 0 {
            return false;
        }
        let mut cls = [0u16; 64];
        let cn = GetClassNameW(hwnd, &mut cls).max(0) as usize;
        let class = String::from_utf16_lossy(&cls[..cn]);
        if matches!(class.as_str(), "Progman" | "WorkerW" | "Shell_TrayWnd") {
            return false;
        }
        let mut rect = RECT::default();
        if GetWindowRect(hwnd, &mut rect).is_err() {
            return false;
        }
        let monitor = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
        let mut info = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        if !GetMonitorInfoW(monitor, &mut info).as_bool() {
            return false;
        }
        let m = info.rcMonitor;
        rect.left <= m.left && rect.top <= m.top && rect.right >= m.right && rect.bottom >= m.bottom
    }
}

/// The window Sir is looking at, skipping JAI's own windows (when he types into the HUD, the
/// HUD is in front): its title and visible bounds (left, top, width, height in physical pixels).
pub fn top_app_window() -> Option<(String, [i32; 4])> {
    app_windows().into_iter().find_map(|w| {
        if is_jarvis_window(&w) {
            return None;
        }
        let hwnd = HWND(w.hwnd);
        // SAFETY: plain queries on a window handle; the rect is a properly sized out-parameter.
        unsafe {
            if IsIconic(hwnd).as_bool() {
                return None;
            }
            let mut r = RECT::default();
            let ok = DwmGetWindowAttribute(hwnd, DWMWA_EXTENDED_FRAME_BOUNDS, &mut r as *mut RECT as *mut c_void, std::mem::size_of::<RECT>() as u32).is_ok()
                || GetWindowRect(hwnd, &mut r).is_ok();
            (ok && r.right - r.left > 40 && r.bottom - r.top > 40).then(|| (w.title.clone(), [r.left, r.top, r.right - r.left, r.bottom - r.top]))
        }
    })
}

/// One display as Windows reports it (physical pixels, left/top/right/bottom).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DisplayInfo {
    pub rect: [i32; 4],
    pub work: [i32; 4],
    pub primary: bool,
    /// `\\.\DISPLAY1` …
    pub device: String,
}

/// Every connected monitor (`EnumDisplayMonitors` + `GetMonitorInfoW`): the primary first, the
/// rest by Windows' own display number (`\\.\DISPLAY2` …, the numbers Settings → Display →
/// Identify shows), then left to right.
pub fn displays() -> Vec<DisplayInfo> {
    use windows::Win32::Graphics::Gdi::{EnumDisplayMonitors, HDC, HMONITOR, MONITORINFOEXW};
    unsafe extern "system" fn collect(monitor: HMONITOR, _: HDC, _: *mut RECT, lparam: LPARAM) -> BOOL {
        // SAFETY: lparam is the &mut Vec passed below and outlives EnumDisplayMonitors.
        let list = unsafe { &mut *(lparam.0 as *mut Vec<DisplayInfo>) };
        let mut info = MONITORINFOEXW::default();
        info.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
        // SAFETY: valid monitor handle from the enumeration; MONITORINFOEXW starts with MONITORINFO
        // and cbSize tells Windows which one it got.
        if unsafe { GetMonitorInfoW(monitor, &mut info as *mut MONITORINFOEXW as *mut MONITORINFO) }.as_bool() {
            let (r, w) = (info.monitorInfo.rcMonitor, info.monitorInfo.rcWork);
            let len = info.szDevice.iter().position(|c| *c == 0).unwrap_or(info.szDevice.len());
            list.push(DisplayInfo {
                rect: [r.left, r.top, r.right, r.bottom],
                work: [w.left, w.top, w.right, w.bottom],
                // MONITORINFOF_PRIMARY = 1
                primary: info.monitorInfo.dwFlags & 1 != 0,
                device: String::from_utf16_lossy(&info.szDevice[..len]),
            });
        }
        TRUE
    }
    let mut list: Vec<DisplayInfo> = Vec::new();
    // SAFETY: the callback only touches `list` through the provided pointer.
    unsafe {
        let _ = EnumDisplayMonitors(HDC(0), None, Some(collect), LPARAM(&mut list as *mut Vec<DisplayInfo> as isize));
    }
    let number = |d: &DisplayInfo| d.device.trim_start_matches("\\\\.\\DISPLAY").parse::<u32>().unwrap_or(u32::MAX);
    list.sort_by_key(|d| (!d.primary, number(d), d.rect[0], d.rect[1]));
    list
}

/// The highest refresh rate (Hz) among the connected monitors, as set in Windows right now
/// (`EnumDisplaySettingsW`); None when no monitor reports one.
pub fn fastest_refresh_hz() -> Option<u32> {
    use windows::Win32::Graphics::Gdi::{EnumDisplaySettingsW, DEVMODEW, ENUM_CURRENT_SETTINGS};
    displays()
        .iter()
        .filter_map(|d| {
            let device: Vec<u16> = d.device.encode_utf16().chain(std::iter::once(0)).collect();
            let mut mode = DEVMODEW { dmSize: std::mem::size_of::<DEVMODEW>() as u16, ..Default::default() };
            // SAFETY: a NUL-terminated device name and a sized out-structure.
            unsafe { EnumDisplaySettingsW(PCWSTR(device.as_ptr()), ENUM_CURRENT_SETTINGS, &mut mode) }.as_bool().then_some(mode.dmDisplayFrequency)
        })
        // 0 and 1 stand for "the hardware's default rate".
        .filter(|hz| *hz > 1)
        .max()
}

/// Work areas (left, top, width, height in physical pixels) of all monitors, primary first.
pub fn monitor_work_areas() -> Vec<[i32; 4]> {
    displays().into_iter().map(|d| [d.work[0], d.work[1], d.work[2] - d.work[0], d.work[3] - d.work[1]]).collect()
}

/// The full bounds (left, top, right, bottom) of the monitor a window is on.
pub fn window_display_rect(hwnd: isize) -> Option<[i32; 4]> {
    // SAFETY: plain queries on a window handle; sized out-parameter.
    unsafe {
        let mut info = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        if !GetMonitorInfoW(MonitorFromWindow(HWND(hwnd), MONITOR_DEFAULTTONEAREST), &mut info).as_bool() {
            return None;
        }
        let r = info.rcMonitor;
        Some([r.left, r.top, r.right, r.bottom])
    }
}

/// Visible bounds (left, top, width, height) and state of a window: (rect, maximised, minimised).
pub fn window_state(hwnd: isize) -> Option<([i32; 4], bool, bool)> {
    let h = HWND(hwnd);
    // SAFETY: plain queries on a window handle; sized out-parameters.
    unsafe {
        let mut r = RECT::default();
        let ok = DwmGetWindowAttribute(h, DWMWA_EXTENDED_FRAME_BOUNDS, &mut r as *mut RECT as *mut c_void, std::mem::size_of::<RECT>() as u32).is_ok()
            || GetWindowRect(h, &mut r).is_ok();
        ok.then(|| ([r.left, r.top, r.right - r.left, r.bottom - r.top], IsZoomed(h).as_bool(), IsIconic(h).as_bool()))
    }
}

/// Moves a window to another monitor's work area, keeping its relative place and size. A
/// maximised window is restored, moved and maximised again there; a borderless full-screen
/// window is stretched over the new monitor. Synchronous: run it on the Win32 worker thread.
pub fn move_to_display(hwnd: isize, target: &DisplayInfo) -> Result<(), String> {
    use windows::Win32::UI::WindowsAndMessaging::{HWND_TOP, SWP_SHOWWINDOW};
    let h = HWND(hwnd);
    // A test session moves its windows without activating them.
    let flags = if crate::platform::quiet_session() { SWP_SHOWWINDOW | SWP_NOACTIVATE } else { SWP_SHOWWINDOW };
    // SAFETY: window state queries and changes on a handle; failures are reported.
    unsafe {
        let maximised = IsZoomed(h).as_bool();
        let fullscreen = !maximised && covers_monitor(hwnd);
        let source = window_display_rect(hwnd).ok_or("could not read the window's monitor")?;
        if fullscreen {
            let [l, t, r, b] = target.rect;
            SetWindowPos(h, HWND_TOP, l, t, r - l, b - t, flags).map_err(|e| e.message().to_string())?;
            return Ok(());
        }
        // Restores maximised / minimised windows so they have a normal frame to move.
        let (src_work, visible, borders) = frame_geometry(h)?;
        let src = if src_work.right > src_work.left { [src_work.left, src_work.top, src_work.right, src_work.bottom] } else { source };
        let [tl, tt, tr, tb] = target.work;
        let (tw, th) = (tr - tl, tb - tt);
        let (sw, sh) = ((src[2] - src[0]).max(1), (src[3] - src[1]).max(1));
        let w = (visible.right - visible.left).min(tw);
        let hgt = (visible.bottom - visible.top).min(th);
        // Same relative position on the new monitor, kept fully inside its work area.
        let rx = (visible.left - src[0]) as f32 / sw as f32;
        let ry = (visible.top - src[1]) as f32 / sh as f32;
        let x = (tl + (rx * tw as f32) as i32).clamp(tl, (tr - w).max(tl));
        let y = (tt + (ry * th as f32) as i32).clamp(tt, (tb - hgt).max(tt));
        let [bl, bt, br, bb] = borders;
        SetWindowPos(h, HWND_TOP, x - bl, y - bt, w + bl + br, hgt + bt + bb, flags).map_err(|e| e.message().to_string())?;
        if maximised {
            let _ = ShowWindow(h, SW_MAXIMIZE);
        }
    }
    focus(hwnd);
    Ok(())
}

/// Places a window so its visible bounds are (x, y, w, h) in physical pixels, then maximises it
/// when asked (protocols restoring a saved workspace).
pub fn place_window(hwnd: isize, [x, y, w, hgt]: [i32; 4], maximise: bool) -> Result<(), String> {
    let h = HWND(hwnd);
    let (_, _, borders) = frame_geometry(h)?;
    set_visible_bounds(h, borders, x, y, w.max(200), hgt.max(150))?;
    if maximise {
        // SAFETY: plain window state change on a handle.
        unsafe {
            let _ = ShowWindow(h, SW_MAXIMIZE);
        }
    }
    Ok(())
}

/// Moves and sizes a window to exactly (x, y, w, h) physical pixels (no minimum size, no frame
/// compensation: for the borderless floating orb). Synchronous: run it on the Win32 worker.
pub fn set_window_rect(hwnd: isize, [x, y, w, hgt]: [i32; 4]) -> Result<(), String> {
    use windows::Win32::UI::WindowsAndMessaging::{SWP_NOACTIVATE, SWP_NOZORDER, SWP_SHOWWINDOW};
    // SAFETY: plain window position change on a handle; failures are reported.
    unsafe {
        SetWindowPos(HWND(hwnd), HWND(0), x, y, w.max(40), hgt.max(40), SWP_NOACTIVATE | SWP_NOZORDER | SWP_SHOWWINDOW).map_err(|e| e.message().to_string())
    }
}

/// Full path of a process' executable (protocols relaunch apps from it).
pub fn process_path(pid: u32) -> Option<String> {
    // SAFETY: handle is checked and closed; buffer length is passed in/out correctly.
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = [0u16; 1024];
        let mut size = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(handle, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut size).is_ok();
        let _ = CloseHandle(handle);
        ok.then(|| String::from_utf16_lossy(&buf[..size as usize]))
    }
}

/// Copies Sir's selection out of a window after bringing it to the front: Ctrl+Insert (the
/// copy chord that never interrupts a terminal), or Ctrl+C with `ctrl_c`. Returns false when the
/// window would not come to the front, so the keys never land elsewhere.
pub fn copy_selection_from(hwnd: isize, ctrl_c: bool) -> bool {
    use windows::Win32::UI::Input::KeyboardAndMouse::{VK_C, VK_INSERT};
    for wait in [120, 250] {
        focus(hwnd);
        std::thread::sleep(std::time::Duration::from_millis(wait));
        if is_foreground(hwnd) {
            return send_chord(&[VK_CONTROL], if ctrl_c { VK_C } else { VK_INSERT }).is_ok();
        }
    }
    false
}

/// Consoles and terminals, where Ctrl+C without a selection would stop the running program.
pub fn is_terminal_window(hwnd: isize) -> bool {
    let mut cls = [0u16; 128];
    // SAFETY: plain query on a window handle into a sized buffer.
    let n = unsafe { GetClassNameW(HWND(hwnd), &mut cls) }.max(0) as usize;
    let class = String::from_utf16_lossy(&cls[..n]);
    let mut pid = 0u32;
    // SAFETY: plain query on a window handle.
    unsafe { GetWindowThreadProcessId(HWND(hwnd), Some(&mut pid)) };
    let process = process_name(pid).unwrap_or_default();
    matches!(class.as_str(), "ConsoleWindowClass" | "CASCADIA_HOSTING_WINDOW_CLASS" | "mintty" | "VirtualConsoleClass" | "PuTTY")
        || matches!(process.as_str(), "windowsterminal" | "conhost" | "cmd" | "powershell" | "pwsh" | "wezterm-gui" | "alacritty" | "mintty" | "putty")
}

/// Executable stem (lower-case, without `.exe`) of a process id.
pub fn process_name(pid: u32) -> Option<String> {
    // SAFETY: handle is checked and closed; buffer length is passed in/out correctly.
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = [0u16; 1024];
        let mut size = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(handle, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut size).is_ok();
        let _ = CloseHandle(handle);
        if !ok {
            return None;
        }
        let path = String::from_utf16_lossy(&buf[..size as usize]);
        let file = path.rsplit(['\\', '/']).next().unwrap_or(&path).to_lowercase();
        Some(file.trim_end_matches(".exe").to_string())
    }
}

fn key_input(vk: VIRTUAL_KEY, flags: KEYBD_EVENT_FLAGS) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: vk,
                wScan: 0,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

/// Presses and releases a (media) key.
pub fn tap_key(vk: VIRTUAL_KEY) -> Result<(), String> {
    let inputs = [
        key_input(vk, KEYEVENTF_EXTENDEDKEY),
        key_input(vk, KEYEVENTF_EXTENDEDKEY | KEYEVENTF_KEYUP),
    ];
    // SAFETY: valid INPUT array with matching element size.
    let sent = unsafe { SendInput(&inputs, std::mem::size_of::<INPUT>() as i32) };
    if sent == inputs.len() as u32 {
        Ok(())
    } else {
        Err("SendInput was blocked (is a higher-integrity window focused?)".into())
    }
}

pub fn show_window(hwnd: isize, cmd: WindowCmd) {
    use windows::Win32::UI::WindowsAndMessaging::{SW_SHOWMINNOACTIVE, SW_SHOWNOACTIVATE};
    // A test session restores and minimises without activating a window (maximising has no
    // such form; a test session's own windows are never maximised).
    let quiet = crate::platform::quiet_session();
    let cmd: SHOW_WINDOW_CMD = match cmd {
        WindowCmd::Minimize if quiet => SW_SHOWMINNOACTIVE,
        WindowCmd::Minimize => SW_MINIMIZE,
        WindowCmd::Maximize => SW_MAXIMIZE,
        WindowCmd::Restore if quiet => SW_SHOWNOACTIVATE,
        WindowCmd::Restore => SW_RESTORE,
    };
    // SAFETY: ShowWindow tolerates stale handles.
    unsafe {
        let _ = ShowWindow(HWND(hwnd), cmd);
    }
}

#[derive(Debug, Clone, Copy)]
pub enum WindowCmd {
    Minimize,
    Maximize,
    Restore,
}

/// Brings a window to the front despite the foreground lock (Alt-tap trick). Never in a test
/// session (`platform::quiet_session`): the Alt tap would reach Sir's game. Every caller that
/// sends keys afterwards checks `is_foreground` first, so they send none then.
pub fn focus(hwnd: isize) {
    if crate::platform::quiet_session() {
        return;
    }
    let h = HWND(hwnd);
    // SAFETY: plain Win32 calls on a window handle; failures are harmless.
    unsafe {
        if IsIconic(h).as_bool() {
            let _ = ShowWindow(h, SW_RESTORE);
        }
        let alt = [
            key_input(VK_MENU, KEYBD_EVENT_FLAGS(0)),
            key_input(VK_MENU, KEYEVENTF_KEYUP),
        ];
        SendInput(&alt[..1], std::mem::size_of::<INPUT>() as i32);
        let _ = SetForegroundWindow(h);
        SendInput(&alt[1..], std::mem::size_of::<INPUT>() as i32);
        let _ = BringWindowToTop(h);
    }
}

pub fn close_window(hwnd: isize) -> bool {
    unsafe { PostMessageW(HWND(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0)).is_ok() }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Snap {
    Left,
    Right,
    Top,
    Bottom,
}

/// A window's monitor work area, its visible bounds and DWM's invisible border widths
/// (left, top, right, bottom). Restores maximised/minimised windows first.
fn frame_geometry(h: HWND) -> Result<(RECT, RECT, [i32; 4]), String> {
    // SAFETY: all out-pointers reference properly sized local structs.
    unsafe {
        if IsZoomed(h).as_bool() || IsIconic(h).as_bool() {
            let _ = ShowWindow(h, SW_RESTORE);
        }
        let monitor = MonitorFromWindow(h, MONITOR_DEFAULTTONEAREST);
        let mut info = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        if !GetMonitorInfoW(monitor, &mut info).as_bool() {
            return Err("could not read the monitor work area".into());
        }
        let mut outer = RECT::default();
        GetWindowRect(h, &mut outer).map_err(|e| e.message().to_string())?;
        let mut visible = outer;
        let _ = DwmGetWindowAttribute(
            h,
            DWMWA_EXTENDED_FRAME_BOUNDS,
            &mut visible as *mut RECT as *mut c_void,
            std::mem::size_of::<RECT>() as u32,
        );
        let borders = [
            visible.left - outer.left,
            visible.top - outer.top,
            outer.right - visible.right,
            outer.bottom - visible.bottom,
        ];
        Ok((info.rcWork, visible, borders))
    }
}

/// Positions a window so its *visible* bounds are (x, y, w, h).
fn set_visible_bounds(h: HWND, [bl, bt, br, bb]: [i32; 4], x: i32, y: i32, w: i32, hgt: i32) -> Result<(), String> {
    // SAFETY: plain window positioning on a handle; failures are reported.
    unsafe { SetWindowPos(h, HWND(0), x - bl, y - bt, w + bl + br, hgt + bt + bb, SWP_NOZORDER | SWP_NOACTIVATE) }
        .map_err(|e| e.message().to_string())
}

/// Snaps a window to half of its monitor's work area, compensating for DWM's invisible borders.
pub fn snap(hwnd: isize, side: Snap) -> Result<(), String> {
    let h = HWND(hwnd);
    let (work, _, borders) = frame_geometry(h)?;
    let (w, hgt) = (work.right - work.left, work.bottom - work.top);
    let (x, y, cw, ch) = match side {
        Snap::Left => (work.left, work.top, w / 2, hgt),
        Snap::Right => (work.left + w / 2, work.top, w - w / 2, hgt),
        Snap::Top => (work.left, work.top, w, hgt / 2),
        Snap::Bottom => (work.left, work.top + hgt / 2, w, hgt - hgt / 2),
    };
    set_visible_bounds(h, borders, x, y, cw, ch)?;
    focus(hwnd);
    Ok(())
}

/// Where to put a window inside its monitor's work area, keeping its size (`Scale` resizes it
/// around its centre instead).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Placement {
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
    Center,
    Scale(f32),
}

pub fn place(hwnd: isize, placement: Placement) -> Result<(), String> {
    let h = HWND(hwnd);
    let (work, visible, borders) = frame_geometry(h)?;
    let (ww, wh) = (work.right - work.left, work.bottom - work.top);
    let (vw, vh) = (visible.right - visible.left, visible.bottom - visible.top);
    let margin = 16;
    let (w, hgt) = match placement {
        Placement::Scale(f) => (((vw as f32 * f) as i32).clamp(320, ww), ((vh as f32 * f) as i32).clamp(240, wh)),
        _ => (vw.min(ww), vh.min(wh)),
    };
    let (x, y) = match placement {
        Placement::TopLeft => (work.left + margin, work.top + margin),
        Placement::TopRight => (work.right - margin - w, work.top + margin),
        Placement::BottomLeft => (work.left + margin, work.bottom - margin - hgt),
        Placement::BottomRight => (work.right - margin - w, work.bottom - margin - hgt),
        Placement::Center => (work.left + (ww - w) / 2, work.top + (wh - hgt) / 2),
        Placement::Scale(_) => (
            (visible.left + (vw - w) / 2).clamp(work.left, work.right - w),
            (visible.top + (vh - hgt) / 2).clamp(work.top, work.bottom - hgt),
        ),
    };
    set_visible_bounds(h, borders, x, y, w, hgt)
}

// ---------------------------------------------------------------------- the HUD window ----

/// Registered message that makes the HUD window destroy itself, posted from any thread when
/// JAI quits: eframe only reacts to close requests while it paints, and a window hidden in
/// the tray never paints.
static HUD_QUIT_MESSAGE: AtomicU32 = AtomicU32::new(0);
const HUD_SUBCLASS_ID: usize = 0x4A52_5653;

pub fn install_hud_quit_hook(hwnd: isize) {
    // SAFETY: subclasses our own window on its own thread (called from eframe's app creator).
    unsafe {
        HUD_QUIT_MESSAGE.store(RegisterWindowMessageW(w!("JARVIS_HUD_QUIT")), Ordering::Release);
        let _ = SetWindowSubclass(HWND(hwnd), Some(hud_subclass), HUD_SUBCLASS_ID, 0);
    }
}

unsafe extern "system" fn hud_subclass(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _id: usize,
    _data: usize,
) -> LRESULT {
    let quit = HUD_QUIT_MESSAGE.load(Ordering::Acquire);
    if quit != 0 && msg == quit {
        // On the window's own thread: winit reports `Destroyed` and eframe leaves its event loop.
        let _ = DestroyWindow(hwnd);
        return LRESULT(0);
    }
    if msg == WM_NCDESTROY {
        let _ = RemoveWindowSubclass(hwnd, Some(hud_subclass), HUD_SUBCLASS_ID);
    }
    DefSubclassProc(hwnd, msg, wparam, lparam)
}

pub fn post_hud_quit(hwnd: isize) -> bool {
    let msg = HUD_QUIT_MESSAGE.load(Ordering::Acquire);
    // SAFETY: posting a message to a window handle; stale handles just fail.
    msg != 0 && unsafe { PostMessageW(HWND(hwnd), msg, WPARAM(0), LPARAM(0)) }.is_ok()
}

/// Shows the HUD (restoring it when minimised) and brings it to the front. Synchronous: run it
/// on the Win32 worker thread (`run_detached`), never on the window's own thread. A test session
/// shows it without taking the focus (Sir may be playing on another monitor); the window
/// drill's still does, it drives the window's own keys.
pub fn show_hud(hwnd: isize) {
    use windows::Win32::UI::WindowsAndMessaging::{SW_SHOWNA, SW_SHOWNOACTIVATE};
    let h = HWND(hwnd);
    let quiet = crate::platform::quiet_session();
    // SAFETY: plain window state changes on a handle.
    unsafe {
        let iconic = IsIconic(h).as_bool();
        let _ = ShowWindow(h, match (iconic, quiet) {
            (true, false) => SW_RESTORE,
            (false, false) => SW_SHOW,
            (true, true) => SW_SHOWNOACTIVATE,
            (false, true) => SW_SHOWNA,
        });
    }
    if !quiet {
        focus(hwnd);
    }
}

pub fn is_visible(hwnd: isize) -> bool {
    // SAFETY: plain query on a window handle.
    unsafe { IsWindowVisible(HWND(hwnd)).as_bool() }
}

/// Puts a hidden window on the taskbar, minimised and without taking focus, so its frame loop
/// runs (Windows does not paint hidden windows). Asynchronous, safe from any thread.
pub fn show_minimized_no_activate(hwnd: isize) {
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWMINNOACTIVE;
    // SAFETY: plain window state change on a handle.
    unsafe {
        let _ = ShowWindowAsync(HWND(hwnd), SW_SHOWMINNOACTIVE);
    }
}

/// Hides the HUD to the tray (asynchronous, safe from any thread).
pub fn hide_hud(hwnd: isize) {
    // SAFETY: plain window state change on a handle.
    unsafe {
        let _ = ShowWindowAsync(HWND(hwnd), SW_HIDE);
    }
}

/// Restored (non-maximised) bounds of a window and whether it is maximised.
pub fn window_placement(hwnd: isize) -> Option<([i32; 4], bool)> {
    let mut wp = WINDOWPLACEMENT { length: std::mem::size_of::<WINDOWPLACEMENT>() as u32, ..Default::default() };
    // SAFETY: `wp` is a properly sized, initialised struct.
    unsafe {
        GetWindowPlacement(HWND(hwnd), &mut wp).ok()?;
        let r = wp.rcNormalPosition;
        Some(([r.left, r.top, r.right - r.left, r.bottom - r.top], IsZoomed(HWND(hwnd)).as_bool()))
    }
}

/// Moves a not-yet-shown window to remembered bounds, unless they are off every monitor now.
/// It stays hidden: eframe shows it after the first frame.
pub fn restore_placement(hwnd: isize, bounds: [i32; 4]) {
    set_normal_placement(hwnd, bounds, SW_HIDE);
}

/// Puts a shown window back at the bounds `window_placement` read earlier (leaving the floating
/// orb), unless they are off every monitor now. It stays shown and is not activated (the
/// startup form, `restore_placement`, hid the HUD when the orb was left).
pub fn put_back_placement(hwnd: isize, bounds: [i32; 4]) {
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNOACTIVATE;
    set_normal_placement(hwnd, bounds, SW_SHOWNOACTIVATE);
}

fn set_normal_placement(hwnd: isize, [x, y, w, h]: [i32; 4], show: SHOW_WINDOW_CMD) {
    if w < 200 || h < 150 {
        return;
    }
    let rect = RECT { left: x, top: y, right: x + w, bottom: y + h };
    let wp = WINDOWPLACEMENT { length: std::mem::size_of::<WINDOWPLACEMENT>() as u32, showCmd: show.0 as u32, rcNormalPosition: rect, ..Default::default() };
    // SAFETY: valid struct pointers for the duration of the calls.
    unsafe {
        if MonitorFromRect(&rect, MONITOR_DEFAULTTONULL).0 != 0 {
            let _ = SetWindowPlacement(HWND(hwnd), &wp);
        }
    }
}

/// Explorer taskbar commands: 419 = minimise all, 407 = toggle desktop.
pub fn taskbar_command(id: usize) -> Result<(), String> {
    let class = wide("Shell_TrayWnd");
    // SAFETY: class string outlives the call.
    unsafe {
        let tray = FindWindowW(PCWSTR(class.as_ptr()), PCWSTR::null());
        if tray.0 == 0 {
            return Err("taskbar window not found".into());
        }
        SendMessageW(tray, WM_COMMAND, WPARAM(id), LPARAM(0));
    }
    Ok(())
}

fn endpoint_volume() -> windows::core::Result<IAudioEndpointVolume> {
    // SAFETY: COM is initialised on the worker thread; a caller on another thread gets it here
    // (S_FALSE / RPC_E_CHANGED_MODE when it already was), so CO_E_NOTINITIALIZED cannot happen.
    unsafe {
        let _ = CoInitializeEx(None, windows::Win32::System::Com::COINIT_MULTITHREADED);
        let enumerator: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
        // The console endpoint, else the multimedia one (some drivers only expose that role).
        let device = enumerator
            .GetDefaultAudioEndpoint(eRender, eConsole)
            .or_else(|_| enumerator.GetDefaultAudioEndpoint(eRender, windows::Win32::Media::Audio::eMultimedia))?;
        device.Activate::<IAudioEndpointVolume>(CLSCTX_ALL, None)
    }
}

/// Master volume of the default output device: (0.0..=1.0, muted).
pub fn get_volume() -> Result<(f32, bool), String> {
    let ep = endpoint_volume().map_err(|e| e.message().to_string())?;
    unsafe {
        let level = ep.GetMasterVolumeLevelScalar().map_err(|e| e.message().to_string())?;
        let muted = ep.GetMute().map_err(|e| e.message().to_string())?.as_bool();
        Ok((level, muted))
    }
}

pub fn set_volume(level: f32) -> Result<(), String> {
    let ep = endpoint_volume().map_err(|e| e.message().to_string())?;
    unsafe {
        ep.SetMasterVolumeLevelScalar(level.clamp(0.0, 1.0), std::ptr::null())
            .map_err(|e| e.message().to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::window_text;
    use windows::core::w;
    use windows::Win32::Foundation::{HINSTANCE, HWND};
    use windows::Win32::UI::WindowsAndMessaging::{CreateWindowExW, DestroyWindow, HMENU, WINDOW_EX_STYLE, WS_POPUP};

    /// A title is read from another thread without waiting for the window's own one: this thread
    /// looks at no message until the read is back (GetWindowText would wait for it to answer).
    #[test]
    fn a_title_is_read_without_asking_the_window() {
        // SAFETY: a hidden window owned by this test thread, destroyed below.
        let hwnd = unsafe { CreateWindowExW(WINDOW_EX_STYLE(0), w!("STATIC"), w!("jai title test"), WS_POPUP, 0, 0, 120, 80, HWND(0), HMENU(0), HINSTANCE(0), None) };
        assert_ne!(hwnd.0, 0, "test window");
        let raw = hwnd.0;
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(window_text(HWND(raw)));
        });
        let title = rx.recv_timeout(std::time::Duration::from_secs(2));
        // SAFETY: the window created above.
        unsafe {
            let _ = DestroyWindow(hwnd);
        }
        assert_eq!(title, Ok("jai title test".to_string()));
    }
}
