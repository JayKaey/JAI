//! Notification-area (tray) icon: shows that Jarvis is running in the background and offers
//! Open HUD, a microphone toggle and Quit. Runs its own Win32 message loop on a dedicated thread,
//! which also receives the "show your HUD" request of a second `JARVIS.exe`.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayAction {
    OpenHud,
    ToggleMic,
    /// Leave the zero-VRAM gaming standby (models load again).
    WakeJarvis,
    Quit,
}

#[cfg(windows)]
pub use win::{remove, show_running_instance, spawn};

#[cfg(not(windows))]
pub fn spawn(_actions: tokio::sync::mpsc::UnboundedSender<TrayAction>, _mic_on: impl Fn() -> bool + Send + Sync + 'static) {}

#[cfg(not(windows))]
pub fn remove() {}

/// Asks an already running Jarvis to show its HUD. False when none could be reached.
#[cfg(not(windows))]
pub fn show_running_instance() -> bool {
    false
}

#[cfg(windows)]
mod win {
    use super::TrayAction;
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
        TranslateMessage, HICON, HMENU, IDI_APPLICATION, IMAGE_ICON, LR_DEFAULTCOLOR, MF_CHECKED, MF_SEPARATOR, MF_STRING,
        MF_UNCHECKED, MSG, SM_CXSMICON, SM_CYSMICON, TPM_BOTTOMALIGN, TPM_RETURNCMD, TPM_RIGHTBUTTON, WINDOW_EX_STYLE,
        WM_APP, WM_CONTEXTMENU, WM_DESTROY, WM_ENDSESSION, WM_LBUTTONUP, WM_NULL, WM_QUERYENDSESSION, WM_RBUTTONUP,
        WNDCLASSW, WS_OVERLAPPED,
    };

    const WM_TRAY: u32 = WM_APP + 1;
    const CLASS: PCWSTR = w!("JarvisTrayWindow");
    /// Posted by a second `JARVIS.exe` so the running instance shows its HUD.
    const SHOW_HUD_MESSAGE: PCWSTR = w!("JARVIS_SHOW_HUD");
    const ICON_ID: u32 = 1;
    const CMD_OPEN: usize = 1;
    const CMD_MIC: usize = 2;
    const CMD_QUIT: usize = 3;
    const CMD_WAKE: usize = 4;

    struct Tray {
        actions: UnboundedSender<TrayAction>,
        mic_on: Box<dyn Fn() -> bool + Send + Sync>,
    }

    static TRAY: OnceLock<Tray> = OnceLock::new();
    static WINDOW: AtomicIsize = AtomicIsize::new(0);
    static TASKBAR_CREATED: AtomicU32 = AtomicU32::new(0);
    static SHOW_HUD: AtomicU32 = AtomicU32::new(0);

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

    pub fn spawn(actions: UnboundedSender<TrayAction>, mic_on: impl Fn() -> bool + Send + Sync + 'static) {
        if TRAY.set(Tray { actions, mic_on: Box::new(mic_on) }).is_err() {
            return;
        }
        if let Err(e) = std::thread::Builder::new().name("jarvis-tray".into()).spawn(run) {
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
                w!("J.A.R.V.I.S."),
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
        let tip: Vec<u16> = "J.A.R.V.I.S. — online".encode_utf16().take(data.szTip.len() - 1).collect();
        data.szTip[..tip.len()].copy_from_slice(&tip);
        Shell_NotifyIconW(NIM_ADD, &data).as_bool()
    }

    /// Icon resource 1 (assets/jarvis_icon.ico, embedded by build.rs) at the small-icon size.
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
        let mic_on = TRAY.get().map(|t| (t.mic_on)()).unwrap_or(true);
        let _ = AppendMenuW(menu, MF_STRING, CMD_OPEN, w!("Open HUD"));
        let _ = AppendMenuW(menu, MF_STRING | if mic_on { MF_CHECKED } else { MF_UNCHECKED }, CMD_MIC, w!("Microphone on"));
        let _ = AppendMenuW(menu, MF_STRING, CMD_WAKE, w!("Wake Jarvis (load models)"));
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
        let _ = AppendMenuW(menu, MF_STRING, CMD_QUIT, w!("Quit J.A.R.V.I.S."));
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
            CMD_WAKE => send(TrayAction::WakeJarvis),
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
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }
}
