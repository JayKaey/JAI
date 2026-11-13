//! Notification-area (tray) icon: shows that JAI is running in the background and offers
//! Open HUD, a microphone toggle and Quit. Runs its own Win32 message loop on a dedicated thread,
//! which also receives the "show your HUD" request of a second `JAI.exe`.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayAction {
    OpenHud,
    ToggleMic,
    /// Leave the zero-VRAM gaming standby (models load again).
    WakeJai,
    /// Enter it: the models leave the graphics memory.
    Standby,
    /// Obey only Sir's voice, on or off.
    VoiceLock(bool),
    /// Guests may speak for half an hour, or no longer.
    Guests(bool),
    /// Night mode now, or day.
    Night(bool),
    Quit,
}

/// What the tray menu ticks, read when the menu opens.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TrayState {
    pub mic_on: bool,
    pub voice_lock: bool,
    pub guests: bool,
    pub night: bool,
    pub dormant: bool,
}

#[cfg(windows)]
pub use win::{ask_test_session_to_give_way, remove, show_running_instance, spawn};

#[cfg(not(windows))]
pub fn spawn(_actions: tokio::sync::mpsc::UnboundedSender<TrayAction>, _state: impl Fn() -> TrayState + Send + Sync + 'static) {}

#[cfg(not(windows))]
pub fn remove() {}

/// Asks an already running JAI to show its HUD. False when none could be reached.
#[cfg(not(windows))]
pub fn show_running_instance() -> bool {
    false
}

/// Asks a running test session to end, for the JAI Sir just started. False when none could
/// be reached.
#[cfg(not(windows))]
pub fn ask_test_session_to_give_way() -> bool {
    false
}

#[cfg(windows)]
mod win {
    use super::{TrayAction, TrayState};
    use std::sync::atomic::{AtomicIsize, AtomicU32, Ordering};
    use std::sync::OnceLock;
    use tokio::sync::mpsc::UnboundedSender;
    use tracing::{info, warn};
    use windows::core::{w, PCWSTR};
    use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, POINT, WPARAM};
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::UI::Shell::{Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NOTIFYICONDATAW};
    use windows::Win32::UI::WindowsAndMessaging::{
        AllowSetForegroundWindow, AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu,
        DispatchMessageW, FindWindowW, GetCursorPos, GetMessageW, GetSystemMetrics, LoadIconW, LoadImageW, PostMessageW,
        PostQuitMessage, RegisterClassW, RegisterWindowMessageW, SetForegroundWindow, SetMenuDefaultItem, TrackPopupMenu,
        TranslateMessage, HICON, HMENU, IDI_APPLICATION, IMAGE_ICON, LR_DEFAULTCOLOR, MF_CHECKED, MF_GRAYED, MF_SEPARATOR, MF_STRING,
        MF_UNCHECKED, MSG, SM_CXSMICON, SM_CYSMICON, TPM_BOTTOMALIGN, TPM_RETURNCMD, TPM_RIGHTBUTTON, WINDOW_EX_STYLE,
        WM_APP, WM_CONTEXTMENU, WM_DESTROY, WM_ENDSESSION, WM_LBUTTONUP, WM_NULL, WM_QUERYENDSESSION, WM_RBUTTONUP,
        WNDCLASSW, WS_OVERLAPPED,
    };

    const WM_TRAY: u32 = WM_APP + 1;
    const CLASS: PCWSTR = w!("JaiTrayWindow");
    /// Posted by a second `JAI.exe` so the running instance shows its HUD.
    const SHOW_HUD_MESSAGE: PCWSTR = w!("JAI_SHOW_HUD");
    /// Posted by the `JAI.exe` Sir started while a test session holds the place: it ends.
    const GIVE_WAY_MESSAGE: PCWSTR = w!("JAI_GIVE_WAY");
    const ICON_ID: u32 = 1;
    const CMD_OPEN: usize = 1;
    const CMD_MIC: usize = 2;
    const CMD_QUIT: usize = 3;
    const CMD_WAKE: usize = 4;
    const CMD_LOCK: usize = 5;
    const CMD_GUESTS: usize = 6;
    const CMD_NIGHT: usize = 7;
    const CMD_STANDBY: usize = 8;

    struct Tray {
        actions: UnboundedSender<TrayAction>,
        state: Box<dyn Fn() -> TrayState + Send + Sync>,
    }

    static TRAY: OnceLock<Tray> = OnceLock::new();
    static WINDOW: AtomicIsize = AtomicIsize::new(0);
    static TASKBAR_CREATED: AtomicU32 = AtomicU32::new(0);
    static SHOW_HUD: AtomicU32 = AtomicU32::new(0);
    static GIVE_WAY: AtomicU32 = AtomicU32::new(0);

    pub fn ask_test_session_to_give_way() -> bool {
        // SAFETY: window lookup by class name and a posted message; no pointers are kept.
        unsafe {
            let hwnd = FindWindowW(CLASS, PCWSTR::null());
            hwnd.0 != 0 && PostMessageW(hwnd, RegisterWindowMessageW(GIVE_WAY_MESSAGE), WPARAM(0), LPARAM(0)).is_ok()
        }
    }

    pub fn show_running_instance() -> bool {
        // SAFETY: window lookup by class name and a posted message; no pointers are kept.
        unsafe {
            let hwnd = FindWindowW(CLASS, PCWSTR::null());
            if hwnd.0 == 0 {
                return false;
            }
            // We were just started by Sir, so we may hand the foreground to the running instance.
            let _ = AllowSetForegroundWindow(u32::MAX); // ASFW_ANY
            PostMessageW(hwnd, RegisterWindowMessageW(SHOW_HUD_MESSAGE), WPARAM(0), LPARAM(0)).is_ok()
        }
    }

    pub fn spawn(actions: UnboundedSender<TrayAction>, state: impl Fn() -> TrayState + Send + Sync + 'static) {
        if TRAY.set(Tray { actions, state: Box::new(state) }).is_err() {
            return;
        }
        if let Err(e) = std::thread::Builder::new().name("jai-tray".into()).spawn(run) {
            warn!("[TRAY] Could not start the tray icon: {e}");
        }
    }

    /// Removes the icon; without this a ghost icon stays in the tray until hovered.
    pub fn remove() {
        let hwnd = WINDOW.swap(0, Ordering::AcqRel);
        if hwnd != 0 {
            let data = NOTIFYICONDATAW {
                cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
                hWnd: HWND(hwnd),
                uID: ICON_ID,
                ..Default::default()
            };
            // SAFETY: identifies our own icon by window handle + id.
            unsafe {
                let _ = Shell_NotifyIconW(NIM_DELETE, &data);
            }
        }
    }

    fn send(action: TrayAction) {
        if let Some(tray) = TRAY.get() {
            let _ = tray.actions.send(action);
        }
    }

    fn run() {
        // SAFETY: window-class registration, a hidden window and its message loop, all on this thread.
        unsafe {
            let Ok(module) = GetModuleHandleW(PCWSTR::null()) else { return };
            let instance = HINSTANCE(module.0);
            let class = CLASS;
            let wc = WNDCLASSW {
                lpfnWndProc: Some(wndproc),
                hInstance: instance,
                lpszClassName: class,
                ..Default::default()
            };
            if RegisterClassW(&wc) == 0 {
                warn!("[TRAY] Could not register the tray window class");
                return;
            }
            // A hidden top-level window (not message-only) so it receives "TaskbarCreated".
            let hwnd = CreateWindowExW(
                WINDOW_EX_STYLE(0),
                class,
                w!("J.A.I."),
                WS_OVERLAPPED,
                0,
                0,
                0,
                0,
                HWND(0),
                HMENU(0),
                instance,
                None,
            );
            if hwnd.0 == 0 {
                warn!("[TRAY] Could not create the tray window");
                return;
            }
            WINDOW.store(hwnd.0, Ordering::Release);
            TASKBAR_CREATED.store(RegisterWindowMessageW(w!("TaskbarCreated")), Ordering::Release);
            SHOW_HUD.store(RegisterWindowMessageW(SHOW_HUD_MESSAGE), Ordering::Release);
            GIVE_WAY.store(RegisterWindowMessageW(GIVE_WAY_MESSAGE), Ordering::Release);
            if add_icon(hwnd) {
                info!("[TRAY] Running in the notification area (click: HUD, right-click: menu)");
            } else {
                warn!("[TRAY] Could not add the tray icon");
            }
            let mut msg = MSG::default();
            while GetMessageW(&mut msg, HWND(0), 0, 0).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
    }

    unsafe fn add_icon(hwnd: HWND) -> bool {
        let mut data = NOTIFYICONDATAW {
            cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: hwnd,
            uID: ICON_ID,
            uFlags: NIF_ICON | NIF_MESSAGE | NIF_TIP,
            uCallbackMessage: WM_TRAY,
            hIcon: load_icon(),
            ..Default::default()
        };
        let tip = if crate::platform::in_test_session() { "J.A.I. — test run (starting him ends it)" } else { "J.A.I. — online" };
        let tip: Vec<u16> = tip.encode_utf16().take(data.szTip.len() - 1).collect();
        data.szTip[..tip.len()].copy_from_slice(&tip);
        Shell_NotifyIconW(NIM_ADD, &data).as_bool()
    }

    /// Icon resource 1 (assets/jai_icon.ico, embedded by build.rs) at the small-icon size.
    unsafe fn load_icon() -> HICON {
        if let Ok(module) = GetModuleHandleW(PCWSTR::null()) {
            let (cx, cy) = (GetSystemMetrics(SM_CXSMICON), GetSystemMetrics(SM_CYSMICON));
            // MAKEINTRESOURCE(1): an integer id in a pointer, never dereferenced.
            let resource = PCWSTR(std::ptr::without_provenance(1));
            if let Ok(handle) = LoadImageW(HINSTANCE(module.0), resource, IMAGE_ICON, cx, cy, LR_DEFAULTCOLOR) {
                if !handle.is_invalid() {
                    return HICON(handle.0);
                }
            }
        }
        LoadIconW(HINSTANCE(0), IDI_APPLICATION).unwrap_or_default()
    }

    unsafe fn show_menu(hwnd: HWND) {
        let Ok(menu) = CreatePopupMenu() else { return };
        let now = TRAY.get().map(|t| (t.state)()).unwrap_or(TrayState { mic_on: true, ..TrayState::default() });
        let tick = |on: bool| MF_STRING | if on { MF_CHECKED } else { MF_UNCHECKED };
        // The first line says who is running and since when (greyed: it is no command).
        static SINCE: OnceLock<String> = OnceLock::new();
        let since = SINCE.get_or_init(|| chrono::Local::now().format("%a %H:%M").to_string());
        let head: Vec<u16> = format!("J.A.I. {} \u{2014} running since {since}", env!("CARGO_PKG_VERSION")).encode_utf16().chain(std::iter::once(0)).collect();
        let _ = AppendMenuW(menu, MF_STRING | MF_GRAYED, 0, PCWSTR(head.as_ptr()));
        let _ = AppendMenuW(menu, MF_STRING, CMD_OPEN, w!("Open HUD"));
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
        let _ = AppendMenuW(menu, tick(now.mic_on), CMD_MIC, w!("Microphone on"));
        let _ = AppendMenuW(menu, tick(now.voice_lock), CMD_LOCK, w!("Obey only my voice"));
        let _ = AppendMenuW(menu, tick(now.guests), CMD_GUESTS, w!("Guests may speak (30 minutes)"));
        let _ = AppendMenuW(menu, tick(now.night), CMD_NIGHT, w!("Night mode"));
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
        // One line for the standby, saying what a click does from where he stands.
        if now.dormant {
            let _ = AppendMenuW(menu, MF_STRING, CMD_WAKE, w!("Wake JAI (load models)"));
        } else {
            let _ = AppendMenuW(menu, MF_STRING, CMD_STANDBY, w!("Standby (free the graphics memory)"));
        }
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
        let _ = AppendMenuW(menu, MF_STRING, CMD_QUIT, w!("Quit J.A.I."));
        let _ = SetMenuDefaultItem(menu, CMD_OPEN as u32, 0);
        let mut pt = POINT::default();
        let _ = GetCursorPos(&mut pt);
        // Required so the menu closes when the user clicks elsewhere.
        let _ = SetForegroundWindow(hwnd);
        let picked = TrackPopupMenu(menu, TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_BOTTOMALIGN, pt.x, pt.y, 0, hwnd, None);
        let _ = PostMessageW(hwnd, WM_NULL, WPARAM(0), LPARAM(0));
        let _ = DestroyMenu(menu);
        match picked.0 as usize {
            CMD_OPEN => send(TrayAction::OpenHud),
            CMD_MIC => send(TrayAction::ToggleMic),
            CMD_WAKE => send(TrayAction::WakeJai),
            CMD_STANDBY => send(TrayAction::Standby),
            CMD_LOCK => send(TrayAction::VoiceLock(!now.voice_lock)),
            CMD_GUESTS => send(TrayAction::Guests(!now.guests)),
            CMD_NIGHT => send(TrayAction::Night(!now.night)),
            CMD_QUIT => send(TrayAction::Quit),
            _ => {}
        }
    }

    unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        match msg {
            WM_TRAY => {
                match lparam.0 as u32 {
                    WM_LBUTTONUP => send(TrayAction::OpenHud),
                    WM_RBUTTONUP | WM_CONTEXTMENU => show_menu(hwnd),
                    _ => {}
                }
                LRESULT(0)
            }
            WM_QUERYENDSESSION => LRESULT(1),
            WM_ENDSESSION => {
                if wparam.0 != 0 {
                    // Windows is logging off / shutting down: save state before the process ends.
                    send(TrayAction::Quit);
                    std::thread::sleep(std::time::Duration::from_millis(800));
                }
                LRESULT(0)
            }
            WM_DESTROY => {
                remove();
                PostQuitMessage(0);
                LRESULT(0)
            }
            _ if msg != 0 && msg == TASKBAR_CREATED.load(Ordering::Acquire) => {
                // Explorer restarted: the icon has to be added again.
                add_icon(hwnd);
                LRESULT(0)
            }
            _ if msg != 0 && msg == SHOW_HUD.load(Ordering::Acquire) => {
                send(TrayAction::OpenHud);
                LRESULT(0)
            }
            _ if msg != 0 && msg == GIVE_WAY.load(Ordering::Acquire) => {
                info!("[TRAY] Sir started JAI: this test session gives way");
                send(TrayAction::Quit);
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }
}
